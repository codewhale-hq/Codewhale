use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::{Path, State};
use codewhale_config::ConfigStore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use codewhale_config::ProviderKind;

use super::{ApiError, ProviderCredentialState, RuntimeApiState};

/// Largest accepted credential payload. Provider keys are single-line
/// tokens; anything larger is a mistake, not a longer secret.
const MAX_KEY_BYTES: usize = 4 * 1024;

/// Request body cap for the key route — the key plus JSON framing.
pub(super) const PROVIDER_KEY_BODY_LIMIT_BYTES: usize = MAX_KEY_BYTES + 3072;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SetProviderKeyRequest {
    key: String,
    #[serde(default)]
    expected_base_url: Option<String>,
    #[serde(default)]
    scope: ProviderKeyScope,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ProviderKeyScope {
    #[default]
    Durable,
    Session,
}

pub(super) fn session_keys_required() -> Result<bool, ApiError> {
    Ok(runtime_key_scope()? == ProviderKeyScope::Session)
}

fn runtime_key_scope() -> Result<ProviderKeyScope, ApiError> {
    match std::env::var("CODEWHALE_RUNTIME_KEY_SCOPE") {
        Err(std::env::VarError::NotPresent) => Ok(ProviderKeyScope::Durable),
        Ok(value) if value.is_empty() => Ok(ProviderKeyScope::Durable),
        Ok(value) if value == "session" => Ok(ProviderKeyScope::Session),
        _ => Err(ApiError::conflict(
            "CODEWHALE_RUNTIME_KEY_SCOPE must be unset, empty, or 'session'",
        )),
    }
}

/// Only the first-party account endpoint can receive an account device key.
/// This endpoint supplies a process-only reversible auth transform; it never
/// mutates a user's provider slot or durable config. Running turns retain their
/// materialized client: server-side device/session revocation is authoritative.
fn account_model_access_receipt(config: &codewhale_runtime::config::Config) -> Value {
    let identity = config
        .builtin_provider_identity(ProviderKind::Codewhale)
        .ok();
    let api_base = identity
        .as_ref()
        .map(|identity| config.base_url_for_route(identity))
        .unwrap_or_default();
    let supported = api_base.trim_end_matches('/')
        == codewhale_config::descriptors::defaults::DEFAULT_CODEWHALE_BASE_URL
        && crate::profile_constitution::runtime_account_api_base()
            == codewhale_secrets::account::DEFAULT_ACCOUNT_API_BASE;
    let access = config.account_model_access.read().clone();
    let live = access
        .as_ref()
        .filter(|a| a.expires_at > chrono::Utc::now().timestamp());
    json!({
        "apiBase": if supported { api_base.as_str() } else { "" },
        "supported": supported,
        "configured": identity.as_ref().and_then(|identity| config.account_model_api_key(identity)).is_some(),
        "catalogRefreshNeeded": false,
        "sessionId": live.map(|a| a.session_id.as_str()),
        "expiresAt": live.map(|a| a.expires_at),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct SetAccountModelAccessRequest {
    api_base: String,
    session_id: String,
    expected_session_id: Option<String>,
    key: String,
    expires_at: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ClearAccountModelAccessRequest {
    session_id: String,
}

pub(super) async fn get_account_model_access(
    State(state): State<RuntimeApiState>,
) -> Result<Json<Value>, ApiError> {
    tokio::task::spawn_blocking(move || {
        Ok(Json(account_model_access_receipt(&state.config.read())))
    })
    .await
    .map_err(|_| ApiError::internal("account access read failed"))?
}

pub(super) async fn set_account_model_access(
    State(state): State<RuntimeApiState>,
    Json(request): Json<SetAccountModelAccessRequest>,
) -> Result<Json<Value>, ApiError> {
    tokio::task::spawn_blocking(move || {
        let _credential_change = state.provider_switches.blocking_lock();
        let config = state.config.write();
        let refresh_needed =
            install_account_model_access(&config, state.config_profile.as_deref(), request)?;
        let mut receipt = account_model_access_receipt(&config);
        receipt["catalogRefreshNeeded"] = json!(refresh_needed);
        Ok(Json(receipt))
    })
    .await
    .map_err(|_| ApiError::internal("account access update failed"))?
}

fn install_account_model_access(
    config: &codewhale_runtime::config::Config,
    profile: Option<&str>,
    request: SetAccountModelAccessRequest,
) -> Result<bool, ApiError> {
    let identity = config
        .builtin_provider_identity(ProviderKind::Codewhale)
        .map_err(ApiError::conflict)?;
    let expected_base = codewhale_config::descriptors::defaults::DEFAULT_CODEWHALE_BASE_URL;
    if request.api_base.trim_end_matches('/') != expected_base
        || config.base_url_for_route(&identity).trim_end_matches('/') != expected_base
        || crate::profile_constitution::runtime_account_api_base()
            != codewhale_secrets::account::DEFAULT_ACCOUNT_API_BASE
    {
        return Err(ApiError::conflict(
            "Account model access requires the first-party Codewhale endpoint.",
        ));
    }
    if !request.key.starts_with("cwc_")
        || request.key.len() < 32
        || request.key.len() > MAX_KEY_BYTES
        || request
            .key
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(ApiError::bad_request("Invalid account device credential."));
    }
    let now = chrono::Utc::now();
    let secrets = codewhale_secrets::account::secure_account_session_secrets()
        .map_err(|_| ApiError::internal("Account session storage is unavailable."))?;
    let account = codewhale_secrets::account::AccountSessionStore::new(
        secrets,
        profile,
        codewhale_secrets::account::DEFAULT_ACCOUNT_API_BASE,
    )
    .runtime_info_at(now)
    .map_err(|_| ApiError::conflict("Sign in again before connecting account models."))?;
    let session_expiry = account
        .expires_at
        .as_deref()
        .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
        .map(|date| date.timestamp());
    if account.state != codewhale_secrets::account::AccountSessionState::Authenticated
        || account.session_id.as_deref() != Some(request.session_id.as_str())
        || request.expires_at <= now.timestamp()
        || session_expiry.is_none_or(|expiry| request.expires_at > expiry)
    {
        return Err(ApiError::conflict(
            "Account session changed or expired; sign in again.",
        ));
    }
    // Probe the existing resolver without the account layer. Scope to Codewhale
    // so even an inactive unmarked durable slot is checked. Never replace it.
    let mut existing = config.clone();
    existing.account_model_access = Default::default();
    existing
        .scope_to_provider_identity(&identity)
        .map_err(ApiError::conflict)?;
    let stored_key_present = match codewhale_runtime::config::credential_secret_store() {
        Some(secrets) => secrets
            .get("codewhale")
            .map_err(|_| {
                ApiError::conflict("The existing Codewhale credential could not be checked.")
            })?
            .is_some(),
        None => false,
    };
    if stored_key_present
        || existing
            .provider_config_for(&identity)
            .is_some_and(|entry| entry.auth.is_some() || entry.api_key_env.is_some())
        || !credential_writeability(&existing, &identity).writable
        || crate::provider_readiness::has_api_key_for(&existing, &identity)
    {
        return Err(ApiError::conflict(
            "This Codewhale route already has its own credential.",
        ));
    }
    let mut access = config.account_model_access.write();
    let owner = access
        .as_ref()
        .filter(|a| a.expires_at > now.timestamp())
        .map(|a| a.session_id.as_str());
    if owner != request.expected_session_id.as_deref() {
        return Err(ApiError::conflict(
            "Account model access changed; refresh before retrying.",
        ));
    }
    let changed = access.as_ref().is_none_or(|current| {
        current.session_id != request.session_id
            || current.profile.as_deref() != profile
            || current.credential.expose_secret() != request.key
    });
    if changed {
        invalidate_account_catalog(config);
    }
    *access = Some(codewhale_runtime::config::AccountModelAccess {
        session_id: request.session_id,
        credential: codewhale_runtime::credentials::Credential::ApiKey { key: request.key },
        expires_at: request.expires_at,
        profile: profile.map(str::to_string),
    });
    Ok(changed)
}

pub(super) async fn clear_account_model_access(
    State(state): State<RuntimeApiState>,
    Json(request): Json<ClearAccountModelAccessRequest>,
) -> Result<Json<Value>, ApiError> {
    tokio::task::spawn_blocking(move || {
        let _credential_change = state.provider_switches.blocking_lock();
        let config = state.config.write();
        remove_account_model_access(&config, &request.session_id)?;
        Ok(Json(account_model_access_receipt(&config)))
    })
    .await
    .map_err(|_| ApiError::internal("account access removal failed"))?
}

fn remove_account_model_access(
    config: &codewhale_runtime::config::Config,
    session_id: &str,
) -> Result<(), ApiError> {
    let mut access = config.account_model_access.write();
    if access.as_ref().is_some_and(|a| a.session_id != session_id) {
        return Err(ApiError::conflict(
            "Account model access belongs to a different session.",
        ));
    }
    if access.is_some() {
        invalidate_account_catalog(config);
    }
    *access = None;
    Ok(())
}

// Beginning a generation already invalidates this account-only memory roster
// and all older in-flight tickets. No network request or second cache is needed.
fn invalidate_account_catalog(config: &codewhale_runtime::config::Config) {
    let Ok(identity) = config.builtin_provider_identity(ProviderKind::Codewhale) else {
        return;
    };
    crate::provider_catalog_live::begin_refresh_for_identity(
        ProviderKind::Codewhale,
        "codewhale",
        &config.base_url_for_route(&identity),
    );
}

pub(super) fn invalidate_stale_account_catalog(config: &codewhale_runtime::config::Config) {
    let identity = config
        .builtin_provider_identity(ProviderKind::Codewhale)
        .ok();
    let bound = config.account_model_access.read().is_some();
    if bound
        && identity
            .as_ref()
            .and_then(|identity| config.account_model_api_key(identity))
            .is_none()
    {
        invalidate_account_catalog(config);
    }
}

/// Write-only credential entry for native clients (APPS-48).
///
/// `PUT /v1/providers/{id}/key` accepts a key with durable or session scope.
/// Durable writes use the transactional `codewhale auth set` path; session
/// writes update only the process memory overlay. Both return redacted metadata and
/// answers with the redacted receipt: which backend holds the secret and the
/// post-write `credential_state` readback. The key itself — and even its
/// length — never appears in the response, in errors, or in logs.
///
/// There is deliberately no GET: a route that can return a secret can leak
/// one. Clients needing assurance re-read `credential_state` here or on
/// `GET /v1/providers`.
pub(super) async fn set_provider_key(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Json(request): Json<SetProviderKeyRequest>,
) -> Result<Json<Value>, ApiError> {
    #[cfg(test)]
    let env_ticket = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(env_ticket);
        let _credential_change = state.provider_switches.blocking_lock();
        if state.config.read().session_provider_keys_only
            && request.scope != ProviderKeyScope::Session
        {
            return Err(ApiError::conflict("This Engine accepts session-only keys"));
        }
        if request.expected_base_url.as_ref().is_some_and(|url| request.scope != ProviderKeyScope::Session || url.len() > 2048) {
            return Err(ApiError::bad_request("Endpoint-bound credentials require session scope and a bounded endpoint."));
        }
        // Both scopes validate the exact provider and credential authority;
        // only session writes may replace an existing eligible memory overlay.
        let identity = writable_provider(&state.config.read(), &id, request.scope)?;
        let kind = identity.provider;
        if kind == codewhale_config::ProviderKind::OpenaiCodex {
            return Err(ApiError::bad_request(
                codewhale_config::credentials::OPENAI_CODEX_API_KEY_REFUSAL,
            ));
        }

        let key = codewhale_secrets::normalize_api_key(&request.key);
        if key.is_empty() {
            return Err(ApiError::bad_request("key must not be empty"));
        }
        if key.len() > MAX_KEY_BYTES || key.chars().any(char::is_control) {
            return Err(ApiError::bad_request(
                "key must be a single-line credential at most 4 KiB",
            ));
        }

        if kind == ProviderKind::Orcarouter
            && let Some(attempt) = state.provider_connect.0.lock().as_ref()
        {
            attempt.cancel();
        }
        let provider_owned = identity.clone();
        let (backend, saved_config_path) = if request.scope == ProviderKeyScope::Session {
            ("memory".to_string(), None)
        } else {
            let secrets = codewhale_runtime::config::credential_secret_store().ok_or_else(|| {
                ApiError::internal("no credential store is available in this environment")
            })?;
            let store_path = state.config_path.clone();
            let kind_owned = kind;
            let key_owned = key.to_string();
            let (backend, saved_config_path) = {
                let mut store = ConfigStore::load(store_path).map_err(|error| {
                    ApiError::internal(format!("config store unavailable: {error}"))
                })?;
                let mut credential_store =
                    codewhale_config::credentials::credential_metadata_store(&store).map_err(
                        |error| ApiError::internal(format!("credential store: {error}")),
                    )?;
                let target = credential_store.as_mut().unwrap_or(&mut store);
                let slot = codewhale_config::credentials::provider_slot(kind_owned);
                codewhale_runtime::credentials::store::with_provider_write_lock(slot, || {
                    codewhale_config::credentials::set_provider_api_key(
                        target, &secrets, kind_owned, &key_owned,
                    )
                })
                .map_err(|error| {
                    // The credential-write errors name paths and backends only — the
                    // key material is never embedded in the message.
                    ApiError::internal(format!("credential write failed: {error}"))
                })?;
                Ok::<_, ApiError>((
                    secrets.backend_name().to_string(),
                    target.path().to_path_buf(),
                ))
            }?;
            (backend, Some(saved_config_path))
        };

        {
            let mut config = state.config.write();
            config
                .verify_provider_identity(&provider_owned)
                .map_err(ApiError::conflict)?;
            if request.scope == ProviderKeyScope::Session {
                writable_provider(&config, &id, request.scope)?;
                let endpoint = config.base_url_for_route(&provider_owned);
                if request.expected_base_url.as_ref().is_some_and(|expected| expected.trim_end_matches('/') != endpoint.trim_end_matches('/')) {
                    return Err(ApiError::conflict("The configured model endpoint differs from this computer. No credential was installed."));
                }
                config.session_provider_keys.write().insert(
                    provider_owned.clone(),
                    (endpoint, codewhale_runtime::credentials::Credential::ApiKey { key }),
                );
            } else {
                mirror_saved_key_markers(&mut config, &provider_owned)?;
                config.session_provider_keys.write().remove(&provider_owned);
            }
        }

        let credential_state: ProviderCredentialState =
            crate::provider_readiness::credential_state_for_provider(
                &state.config.read(),
                &provider_owned,
            )
            .into();

        let mut receipt = json!({
            "provider": provider_owned.key,
            "stored": request.scope == ProviderKeyScope::Durable,
            "backend": backend,
            "credentialState": credential_state,
        });
        if let Some(path) = saved_config_path {
            receipt["configPath"] = json!(path);
        } else {
            receipt["scope"] = json!("session");
        }
        Ok(Json(receipt))
    })
    .await
    .map_err(|_| ApiError::internal("credential write task failed"))?
}

fn mirror_saved_key_markers(
    config: &mut codewhale_runtime::config::Config,
    identity: &codewhale_runtime::config::ProviderIdentity,
) -> Result<(), ApiError> {
    // Match the shared writer: the root marker belongs only to the
    // active provider, while an inactive provider keeps its own marker.
    config
        .verify_provider_identity(identity)
        .map_err(ApiError::conflict)?;
    if config.active_provider_identity().as_ref().ok() == Some(identity) {
        config.auth_mode = Some("api_key".to_string());
    }
    let entry = config
        .provider_config_for_mut(identity)
        .map_err(|error| ApiError::conflict(error.to_string()))?;
    entry.auth_mode = Some("api_key".to_string());
    entry.external_credentials = None;
    entry.api_key = None;
    if identity.provider == ProviderKind::Xai {
        entry.oauth_credential_generation = None;
    }
    // No model is mirrored: saving a key never changes which model runs
    // (see `prepare_provider_api_key_metadata`).
    Ok(())
}

#[derive(Clone, Default)]
pub(super) struct ProviderConnect(Arc<parking_lot::Mutex<Option<ConnectAttempt>>>);

struct ConnectAttempt {
    id: String,
    status: Arc<parking_lot::Mutex<ConnectStatus>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectStatus {
    Waiting,
    Exchanging,
    Connected,
    Declined,
    Expired,
    Cancelled,
    Failed(&'static str),
}

impl ConnectStatus {
    fn parts(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Waiting | Self::Exchanging => ("waiting", None),
            Self::Connected => ("connected", None),
            Self::Declined => ("declined", None),
            Self::Expired => ("expired", None),
            Self::Cancelled => ("cancelled", None),
            Self::Failed(reason) => ("failed", Some(reason)),
        }
    }
}

impl ConnectAttempt {
    fn cancel(&self) -> ConnectStatus {
        let mut status = self.status.lock();
        if matches!(*status, ConnectStatus::Waiting | ConnectStatus::Exchanging) {
            *status = ConnectStatus::Cancelled;
        }
        *status
    }

    fn receipt(&self, status: ConnectStatus) -> Value {
        let (label, reason) = status.parts();
        let mut receipt = json!({ "attemptId": self.id, "status": label });
        if let Some(reason) = reason {
            receipt["reason"] = json!(reason);
        }
        receipt
    }
}

fn connect_provider_kind(id: &str) -> Result<(), ApiError> {
    let row = codewhale_config::descriptors::compatibility_for_selector(id)
        .ok_or_else(|| ApiError::bad_request(format!("Unknown provider id '{id}'")))?;
    if row.id != row.kind.as_str() || row.kind != ProviderKind::Orcarouter {
        return Err(ApiError::bad_request(
            "Browser connect is available only for 'orcarouter'.",
        ));
    }
    Ok(())
}

pub(super) async fn start_provider_connect(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let _credential_change = state.provider_switches.lock().await;
    connect_provider_kind(&id)?;
    if state.config.read().session_provider_keys_only {
        return Err(ApiError::conflict(
            "Browser sign-in is unavailable on an Engine that requires session-only keys",
        ));
    }
    let identity = writable_provider(&state.config.read(), &id, ProviderKeyScope::Durable)?;
    let endpoint = state.config.read().base_url_for_route(&identity);
    let target = (identity, endpoint);
    let (listeners, request, auth_base) = tokio::task::spawn_blocking(|| {
        let inputs = codewhale_runtime::oauth::OrcaLoginInputs::from_env();
        let listeners = codewhale_runtime::oauth::bind_orcarouter_callback()?;
        let request = codewhale_runtime::oauth::start_orcarouter_auth_request(&listeners, &inputs)?;
        Ok::<_, anyhow::Error>((listeners, request, inputs.auth_base))
    })
    .await
    .map_err(|_| ApiError::internal("OrcaRouter connect could not start."))?
    .map_err(|_| ApiError::internal("OrcaRouter connect could not open its local callback."))?;

    let attempt_id = uuid::Uuid::new_v4().simple().to_string();
    let status = Arc::new(parking_lot::Mutex::new(ConnectStatus::Waiting));
    let authorize_url = request.authorize_url.clone();
    let deadline = Instant::now() + codewhale_runtime::oauth::CALLBACK_TIMEOUT;
    let worker_state = state.clone();
    let worker_status = status.clone();
    std::thread::Builder::new()
        .name("orcarouter-connect".to_string())
        .spawn(move || {
            let finished = run_provider_connect(
                &worker_state,
                &target,
                listeners,
                request,
                &auth_base,
                deadline,
                &worker_status,
            );
            let _credential_change = worker_state.provider_switches.blocking_lock();
            let finished = {
                let mut current = worker_status.lock();
                if matches!(*current, ConnectStatus::Waiting | ConnectStatus::Exchanging) {
                    *current = finished;
                }
                *current
            };
            let (status, reason) = finished.parts();
            tracing::info!(
                target: "codewhale::oauth",
                status,
                reason = reason.unwrap_or(""),
                "OrcaRouter connect attempt finished"
            );
        })
        .map_err(|_| ApiError::internal("OrcaRouter connect could not start."))?;

    let mut current = state.provider_connect.0.lock();
    if let Some(previous) = current.take() {
        previous.cancel();
    }
    *current = Some(ConnectAttempt {
        id: attempt_id.clone(),
        status,
    });
    Ok(Json(json!({
        "attemptId": attempt_id,
        "authorizeUrl": authorize_url,
        "expiresIn": codewhale_runtime::oauth::CALLBACK_TIMEOUT.as_secs(),
    })))
}

fn run_provider_connect(
    state: &RuntimeApiState,
    target: &(codewhale_runtime::config::ProviderIdentity, String),
    listeners: Vec<std::net::TcpListener>,
    request: codewhale_runtime::oauth::BrowserAuthRequest,
    auth_base: &str,
    deadline: Instant,
    status: &parking_lot::Mutex<ConnectStatus>,
) -> ConnectStatus {
    let (identity, endpoint) = target;
    let cancelled = || *status.lock() == ConnectStatus::Cancelled;
    let callback = codewhale_runtime::oauth::wait_for_orcarouter_callback(
        &listeners,
        &request.state,
        deadline,
        &cancelled,
    );
    drop(listeners);
    let code = match callback {
        Ok(codewhale_runtime::oauth::OrcaCallback::Code(code)) => code,
        Ok(codewhale_runtime::oauth::OrcaCallback::Declined) => return ConnectStatus::Declined,
        Ok(codewhale_runtime::oauth::OrcaCallback::Refused) => {
            return ConnectStatus::Failed("authorization_error");
        }
        Ok(codewhale_runtime::oauth::OrcaCallback::Stopped) if cancelled() => {
            return ConnectStatus::Cancelled;
        }
        Ok(codewhale_runtime::oauth::OrcaCallback::Stopped) => return ConnectStatus::Expired,
        Err(_) => return ConnectStatus::Failed("callback_failed"),
    };
    if !credential_writeability(&state.config.read(), identity).writable {
        return ConnectStatus::Failed("not_writable");
    }
    {
        let mut current = status.lock();
        if *current == ConnectStatus::Cancelled {
            return ConnectStatus::Cancelled;
        }
        *current = ConnectStatus::Exchanging;
    }
    let Ok(credential) = codewhale_runtime::oauth::exchange_orcarouter_code(
        &codewhale_runtime::oauth::ReqwestOAuthFormClient,
        auth_base,
        &code,
        &request.pkce.verifier,
    ) else {
        return ConnectStatus::Failed("exchange_failed");
    };
    drop(code);
    drop(request);
    if !credential.scope_satisfies_purpose() {
        return ConnectStatus::Failed("scope_refused");
    }
    let _credential_change = state.provider_switches.blocking_lock();
    let current_attempt = state
        .provider_connect
        .0
        .lock()
        .as_ref()
        .is_some_and(|attempt| std::ptr::eq(attempt.status.as_ref(), status));
    if !current_attempt || *status.lock() != ConnectStatus::Exchanging {
        return ConnectStatus::Cancelled;
    }
    {
        let config = state.config.read();
        if config.session_provider_keys_only
            || config.verify_provider_identity(identity).is_err()
            || config.base_url_for_route(identity) != *endpoint
            || !credential_writeability(&config, identity).writable
        {
            return ConnectStatus::Failed("not_writable");
        }
    }
    let route_config = state.config.read().clone();
    if codewhale_runtime::oauth::activate_orcarouter_credential(&credential, &route_config).is_err()
    {
        return ConnectStatus::Failed("key_not_saved");
    }
    if mirror_saved_key_markers(&mut state.config.write(), identity).is_err() {
        tracing::warn!(
            target: "codewhale::oauth",
            "OrcaRouter key saved; the running config picks it up on reload"
        );
    }
    *status.lock() = ConnectStatus::Connected;
    ConnectStatus::Connected
}

fn current_connect_attempt<'a>(
    current: &'a Option<ConnectAttempt>,
    attempt_id: &str,
) -> Result<&'a ConnectAttempt, ApiError> {
    current
        .as_ref()
        .filter(|attempt| attempt.id == attempt_id)
        .ok_or_else(|| ApiError::not_found("No current OrcaRouter connect attempt has that id."))
}

pub(super) async fn get_provider_connect(
    State(state): State<RuntimeApiState>,
    Path((id, attempt_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    connect_provider_kind(&id)?;
    let current = state.provider_connect.0.lock();
    let attempt = current_connect_attempt(&current, &attempt_id)?;
    let status = *attempt.status.lock();
    Ok(Json(attempt.receipt(status)))
}

pub(super) async fn cancel_provider_connect(
    State(state): State<RuntimeApiState>,
    Path((id, attempt_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let _credential_change = state.provider_switches.lock().await;
    connect_provider_kind(&id)?;
    let current = state.provider_connect.0.lock();
    let attempt = current_connect_attempt(&current, &attempt_id)?;
    let status = attempt.cancel();
    Ok(Json(attempt.receipt(status)))
}

/// The underlying credential owner, independent of a session-key overlay,
/// as a *class* and never as a value, a path, or an environment variable name.
///
/// This exists so a client can disable its own credential control with a
/// truthful reason before submitting, instead of letting a write fail late or —
/// worse — appear to succeed against a source Codewhale does not own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProviderCredentialSource {
    /// Codewhale's own durable secret backend.
    SecretStore,
    /// The expiring account transform; disconnect through account sign-out.
    AccountSession,
    /// A literal value sitting in a config document.
    Config,
    /// An external consent or auth-command source (OAuth, `auth_source`).
    ExternalAuth,
    /// The route takes no credential at all.
    None,
}

/// Whether this route's credential can be written through the runtime API, and
/// the reason when it cannot. The reason is user-facing copy.
pub(super) struct CredentialWriteability {
    pub(super) source: ProviderCredentialSource,
    pub(super) writable: bool,
    pub(super) reason: Option<&'static str>,
}

/// Classify a route's underlying credential ownership without its session key.
///
/// Auth mode, consent state and configured `api_key` kinds are structural;
/// a bound account also requires source resolution with the session overlay
/// excluded. The result exposes only the owner, never credential material.
pub(super) fn credential_writeability(
    config: &codewhale_runtime::config::Config,
    identity: &codewhale_runtime::config::ProviderIdentity,
) -> CredentialWriteability {
    if config.verify_provider_identity(identity).is_err() {
        return CredentialWriteability {
            source: ProviderCredentialSource::None,
            writable: false,
            reason: Some(
                "This route has no admitted credential authority; repair its provider selection.",
            ),
        };
    }
    if config.required_account_model_owner.is_some() {
        return CredentialWriteability {
            source: ProviderCredentialSource::AccountSession,
            writable: false,
            reason: Some("This route uses your Codewhale account. Sign out to disconnect it."),
        };
    }
    let provider = identity.provider;
    let auth_mode = config.auth_mode_for_provider(identity);
    if provider == ProviderKind::Custom
        && config
            .provider_config_for(identity)
            .is_some_and(|entry| entry.oauth.is_some())
    {
        return CredentialWriteability {
            source: ProviderCredentialSource::ExternalAuth,
            writable: false,
            reason: Some(
                "This provider signs in through its reviewed plugin. Use the plugin login or logout command.",
            ),
        };
    }
    if codewhale_config::auth_mode_disables_api_key(auth_mode.as_deref()) {
        return CredentialWriteability {
            source: ProviderCredentialSource::None,
            writable: false,
            reason: Some("This route is configured to send no credential."),
        };
    }
    if provider == ProviderKind::Custom {
        return CredentialWriteability {
            source: ProviderCredentialSource::None,
            writable: false,
            reason: Some("This route has no credential slot."),
        };
    }
    // An active external consent owns the credential. Overwriting the key slot
    // would not change what the route sends, so a write here must refuse
    // rather than report a success the user cannot observe.
    if config
        .external_credential_consent_status(identity)
        .is_some_and(|status| status.route_state == "active")
    {
        return CredentialWriteability {
            source: ProviderCredentialSource::ExternalAuth,
            writable: false,
            reason: Some(
                "This route signs in through an external consent. Sign out of it before setting a key.",
            ),
        };
    }
    // A literal key in a config document is a plaintext credential Codewhale
    // did not put there. Writing the secret store would leave the literal in
    // place and still winning, so refuse and name the file-owned source.
    if let Some(entry) = config.provider_config_for(identity)
        && let Some(existing) = entry.api_key.as_deref()
        && codewhale_config::classify_config_api_key_value(existing)
            == codewhale_config::ConfigApiKeyValueKind::Literal
    {
        return CredentialWriteability {
            source: ProviderCredentialSource::Config,
            writable: false,
            reason: Some(
                "This route's key is set literally in a config file. Remove it there before managing it here.",
            ),
        };
    }
    let account_bound = config.account_model_access.read().is_some();
    if account_bound && {
        let mut underlying = config.clone();
        underlying.session_provider_keys = Default::default();
        matches!(
            codewhale_runtime::config::resolve_credential_source(&underlying, identity).source,
            codewhale_runtime::credentials::CredentialSource::AccountSession
        )
    } {
        return CredentialWriteability {
            source: ProviderCredentialSource::AccountSession,
            writable: false,
            reason: Some("This route uses your Codewhale account. Sign out to disconnect it."),
        };
    }
    CredentialWriteability {
        source: ProviderCredentialSource::SecretStore,
        writable: true,
        reason: None,
    }
}

/// Provider validation for writes and the durable leg of clears.
fn writable_provider(
    config: &codewhale_runtime::config::Config,
    id: &str,
    scope: ProviderKeyScope,
) -> Result<codewhale_runtime::config::ProviderIdentity, ApiError> {
    let identity = provider_key_identity(config, id)?;
    if scope == ProviderKeyScope::Session {
        if config.should_skip_secret_store_for_provider(&identity) {
            return Err(ApiError::conflict(
                "This route does not accept a provider key: check its endpoint and auth mode.",
            ));
        }
        if config.required_account_model_owner.is_none()
            && config.session_provider_api_key(&identity).is_some()
        {
            return Ok(identity);
        }
    }
    let writeability = credential_writeability(config, &identity);
    if !writeability.writable {
        return Err(ApiError::conflict(
            writeability
                .reason
                .unwrap_or("This route's credential is not managed by Codewhale."),
        ));
    }
    Ok(identity)
}

fn provider_key_identity(
    config: &codewhale_runtime::config::Config,
    id: &str,
) -> Result<codewhale_runtime::config::ProviderIdentity, ApiError> {
    let row = codewhale_config::descriptors::compatibility_for_selector(id)
        .ok_or_else(|| ApiError::bad_request(format!("Unknown provider id '{id}'")))?;
    if row.id != row.kind.as_str() {
        return Err(ApiError::bad_request(format!(
            "provider '{id}' is a legacy alias; use '{}' instead",
            row.kind.as_str()
        )));
    }
    let identity = config
        .builtin_provider_identity(row.kind)
        .map_err(ApiError::bad_request)?;
    if identity.provider == ProviderKind::Custom {
        return Err(ApiError::bad_request("provider has no credential slot"));
    }
    Ok(identity)
}

/// `DELETE /v1/providers/{id}/key` — remove a Codewhale-owned credential.
///
/// Shares `PUT`'s credential-ownership checks: reporting "cleared" for a
/// credential this route cannot reach would misrepresent a security action.
/// Unlike creating a new key, clearing can remove a legacy unused Codex key.
/// The secret-store leg is reported separately because the config write lands
/// first and the backend can still refuse.
pub(super) async fn clear_provider_key(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    #[cfg(test)]
    let env_ticket = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(env_ticket);
        let _credential_change = state.provider_switches.blocking_lock();
        let (identity, scope) = {
            let config = state.config.write();
            let identity = provider_key_identity(&config, &id)?;
            config.session_provider_keys.write().remove(&identity);
            let scope = if config.session_provider_keys_only {
                ProviderKeyScope::Session
            } else {
                ProviderKeyScope::Durable
            };
            if scope == ProviderKeyScope::Durable {
                writable_provider(&config, &id, scope)?;
            }
            (identity, scope)
        };
        if identity.provider == ProviderKind::Orcarouter
            && let Some(attempt) = state.provider_connect.0.lock().as_ref()
        {
            attempt.cancel();
        }
        if scope == ProviderKeyScope::Session {
            return Ok(Json(json!({
                "provider": identity.key,
                "cleared": true,
                "scope": "session",
                "backend": "memory",
            })));
        }
        let kind = identity.provider;

        let secrets = codewhale_runtime::config::credential_secret_store().ok_or_else(|| {
            ApiError::internal("no credential store is available in this environment")
        })?;

        let store_path = state.config_path.clone();
        let outcome = {
            let mut store = ConfigStore::load(store_path).map_err(|error| {
                ApiError::internal(format!("config store unavailable: {error}"))
            })?;
            let mut credential_store =
                codewhale_config::credentials::credential_metadata_store(&store)
                    .map_err(|error| ApiError::internal(format!("credential store: {error}")))?;
            let target = credential_store.as_mut().unwrap_or(&mut store);
            let slot = codewhale_config::credentials::provider_slot(kind);
            codewhale_runtime::credentials::store::with_provider_write_lock(slot, || {
                codewhale_config::credentials::clear_provider_api_key(target, &secrets, kind)
            })
            .map_err(|error| {
                // Clear errors name slots and paths only; no key material can
                // reach this message because none was read.
                ApiError::internal(format!("credential clear failed: {error}"))
            })
        }?;

        // Mirror the cleared markers into the live config for the same reason the
        // write path mirrors them: the durable clear may have landed on the
        // user-global document while this server's ambient config is
        // workspace-scoped, and `credential_state` would otherwise keep reporting
        // the provider as configured until the next process start.
        {
            let mut config = state.config.write();
            config
                .verify_provider_identity(&identity)
                .map_err(ApiError::conflict)?;
            let entry = config
                .provider_config_for_mut(&identity)
                .map_err(|error| ApiError::conflict(error.to_string()))?;
            entry.api_key = None;
            if kind == ProviderKind::Xai {
                entry.auth_mode = None;
                entry.external_credentials = None;
                entry.oauth_credential_generation = None;
            }
        }

        let credential_state: ProviderCredentialState =
            crate::provider_readiness::credential_state_for_provider(
                &state.config.read(),
                &identity,
            )
            .into();

        if let Some(error) = outcome.secret_store_error {
            return Err(ApiError::internal(format!(
                "the config entry was cleared, but the secret store refused to delete {}: {error}",
                outcome.slot
            )));
        }

        Ok(Json(json!({
            "provider": identity.key,
            "cleared": true,
            "credentialState": credential_state,
        })))
    })
    .await
    .map_err(|_| ApiError::internal("credential clear task failed"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_runtime::config::Config;

    fn saved_account_session(session_id: &str) -> codewhale_secrets::account::AccountSessionStore {
        let store = codewhale_secrets::account::AccountSessionStore::new(
            codewhale_secrets::account::secure_account_session_secrets().unwrap(),
            None,
            codewhale_secrets::account::DEFAULT_ACCOUNT_API_BASE,
        );
        store
            .save(codewhale_secrets::account::AccountAuthBundle {
                token_type: "Bearer".into(),
                access_token: "fixture-access-token-for-account-models".into(),
                refresh_token: "fixture-refresh-token-for-account-models".into(),
                session: Some(codewhale_secrets::account::AccountSession {
                    id: session_id.into(),
                    status: "active".into(),
                    expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
                    refresh_expires_at: (chrono::Utc::now() + chrono::Duration::days(1))
                        .to_rfc3339(),
                    ..Default::default()
                }),
                user: Some(codewhale_secrets::account::AccountUser {
                    id: "fixture-user".into(),
                    ..Default::default()
                }),
            })
            .unwrap();
        store
    }

    fn account_request(owner: Option<&str>) -> SetAccountModelAccessRequest {
        SetAccountModelAccessRequest {
            api_base: codewhale_config::descriptors::defaults::DEFAULT_CODEWHALE_BASE_URL.into(),
            session_id: "fixture-session".into(),
            expected_session_id: owner.map(str::to_string),
            key: "cwc_fixture_device_credential_not_real".into(),
            expires_at: chrono::Utc::now().timestamp() + 1800,
        }
    }

    #[test]
    fn account_overlay_resolves_without_disk_residue_and_logout_revokes_clones() {
        let _env = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let _base = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_CLOUD_API_BASE",
            codewhale_secrets::account::DEFAULT_ACCOUNT_API_BASE,
        );
        let _api_base = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_API_BASE",
            codewhale_config::descriptors::defaults::DEFAULT_CODEWHALE_BASE_URL,
        );
        let _api_key = crate::test_support::EnvVarGuard::set("CODEWHALE_API_KEY", "");
        let store = saved_account_session("fixture-session");
        let config = {
            let __cw_config_init_0_0 = Some("codewhale".into());
            let mut __cw_config_init_0_value: Config = Default::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value
        };
        let cloned_before_install = config.clone();
        assert!(install_account_model_access(&config, None, account_request(None)).unwrap());
        assert_eq!(
            cloned_before_install
                .active_route_api_key_read_only()
                .unwrap(),
            "cwc_fixture_device_credential_not_real"
        );
        assert!(crate::provider_readiness::has_api_key_for(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Codewhale)
        ));
        assert!(!format!("{config:?}").contains("cwc_fixture"));
        assert!(
            !account_model_access_receipt(&config)
                .to_string()
                .contains("cwc_fixture")
        );
        assert!(
            config
                .provider_config_for(&config.test_identity_for_kind(ProviderKind::Codewhale))
                .is_none()
        );
        assert!(
            codewhale_runtime::config::credential_secret_store()
                .unwrap()
                .get("codewhale")
                .unwrap()
                .is_none()
        );
        use codewhale_config::catalog::{
            CatalogStatus, ProviderCatalogDelta, base_url_fingerprint,
        };
        let endpoint = codewhale_config::descriptors::defaults::DEFAULT_CODEWHALE_BASE_URL;
        let ticket = crate::provider_catalog_live::begin_refresh_for_identity(
            ProviderKind::Codewhale,
            "codewhale",
            endpoint,
        );
        let delta = || ProviderCatalogDelta {
            provider: "codewhale".into(),
            base_url_fingerprint: base_url_fingerprint(endpoint),
            fetched_at: 1,
            offerings: vec![],
        };
        assert_eq!(
            crate::provider_catalog_live::record_success_if_current(&ticket, delta()),
            Some(CatalogStatus::Fresh)
        );
        assert!(
            !install_account_model_access(&config, None, account_request(Some("fixture-session")))
                .unwrap()
        );
        assert!(
            crate::provider_catalog_live::cached_entry_for_route(
                ProviderKind::Codewhale,
                "codewhale",
                endpoint
            )
            .unwrap()
            .is_some()
        );
        let mut extended = account_request(Some("fixture-session"));
        extended.expires_at += 60;
        assert!(!install_account_model_access(&config, None, extended).unwrap());
        assert_eq!(
            crate::provider_catalog_live::record_success_if_current(&ticket, delta()),
            Some(CatalogStatus::Fresh)
        );
        let mut strict = config.clone();
        strict.bind_account_model_owner("fixture-user").unwrap();
        strict
            .provider_config_for_mut(&strict.test_identity_for_kind(ProviderKind::Codewhale))
            .unwrap()
            .api_key = Some("other-local-key".into());
        assert_eq!(
            strict.active_route_api_key_read_only().unwrap(),
            "cwc_fixture_device_credential_not_real"
        );
        assert!(
            config
                .clone()
                .bind_account_model_owner("other-account")
                .is_err()
        );
        store.clear().unwrap();
        assert!(strict.active_route_api_key_read_only().is_err());
        invalidate_stale_account_catalog(&config);
        assert!(
            crate::provider_catalog_live::cached_entry_for_route(
                ProviderKind::Codewhale,
                "codewhale",
                endpoint
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(
            crate::provider_catalog_live::record_success_if_current(&ticket, delta()),
            None
        );
        assert!(
            cloned_before_install
                .active_route_api_key_read_only()
                .is_err()
        );
        assert!(
            install_account_model_access(&config, None, account_request(Some("fixture-session")))
                .is_err()
        );
    }

    #[test]
    fn account_overlay_refuses_endpoint_credentials_and_stale_session_ownership() {
        let _env = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let _base = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_CLOUD_API_BASE",
            codewhale_secrets::account::DEFAULT_ACCOUNT_API_BASE,
        );
        let _api_base = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_API_BASE",
            codewhale_config::descriptors::defaults::DEFAULT_CODEWHALE_BASE_URL,
        );
        let _api_key = crate::test_support::EnvVarGuard::set("CODEWHALE_API_KEY", "");
        saved_account_session("fixture-session");
        let mut config = {
            let __cw_config_init_1_0 = Some("codewhale".into());
            let mut __cw_config_init_1_value: Config = Default::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value
        };
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Codewhale))
            .unwrap()
            .base_url = Some("https://api.codewhale.net/other/v1".into());
        assert!(install_account_model_access(&config, None, account_request(None)).is_err());
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Codewhale))
            .unwrap()
            .base_url = None;
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Codewhale))
            .unwrap()
            .api_key = Some("existing-user-key".into());
        assert!(install_account_model_access(&config, None, account_request(None)).is_err());
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Codewhale))
            .unwrap()
            .api_key = None;
        let mut expired = account_request(None);
        expired.expires_at = chrono::Utc::now().timestamp() - 1;
        assert!(install_account_model_access(&config, None, expired).is_err());
        install_account_model_access(&config, None, account_request(None)).unwrap();
        assert!(install_account_model_access(&config, None, account_request(None)).is_err());
        install_account_model_access(&config, None, account_request(Some("fixture-session")))
            .unwrap();
        assert!(remove_account_model_access(&config, "stale-session").is_err());
        assert!(
            config
                .account_model_api_key(&config.test_identity_for_kind(ProviderKind::Codewhale))
                .is_some()
        );
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Codewhale))
            .unwrap()
            .api_key = Some("new-user-key".into());
        assert_eq!(
            config.active_route_api_key_read_only().unwrap(),
            "new-user-key"
        );
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Codewhale))
            .unwrap()
            .api_key = None;
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Codewhale))
            .unwrap()
            .base_url = Some("https://api.codewhale.net/other/v1".into());
        assert!(
            config
                .account_model_api_key(&config.test_identity_for_kind(ProviderKind::Codewhale))
                .is_none()
        );
    }

    #[test]
    fn account_overlay_clear_is_shared_and_preserves_manual_config() {
        let _env = crate::test_support::lock_test_env();
        let mut config = Config::default();
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Codewhale))
            .unwrap()
            .api_key = Some("manual-key".into());
        *config.account_model_access.write() =
            Some(codewhale_runtime::config::AccountModelAccess {
                session_id: "current".into(),
                credential: codewhale_runtime::credentials::Credential::ApiKey {
                    key: "cwc_fixture".into(),
                },
                expires_at: chrono::Utc::now().timestamp() + 60,
                profile: None,
            });
        let clone = config.clone();
        assert!(remove_account_model_access(&config, "stale").is_err());
        assert!(clone.account_model_access.read().is_some());
        remove_account_model_access(&config, "current").unwrap();
        assert!(clone.account_model_access.read().is_none());
        assert_eq!(
            config
                .provider_config_for(&config.test_identity_for_kind(ProviderKind::Codewhale))
                .unwrap()
                .api_key
                .as_deref(),
            Some("manual-key")
        );
    }

    #[test]
    fn plugin_oauth_credentials_cannot_be_replaced_by_an_api_key() {
        let mut config = {
            let __cw_config_init_2_0 = Some("plugin-test".into());
            let mut __cw_config_init_2_value: Config = Default::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value
        };
        let entry = config
            .providers
            .get_or_insert_with(Default::default)
            .custom
            .entry("plugin-test".into())
            .or_default();
        entry.kind = Some("openai-compatible".into());
        entry.base_url = Some("https://gateway.example/api".into());
        entry.auth_mode = Some("oauth".into());
        entry.oauth = Some(codewhale_runtime::oauth::PluginOAuthConfig {
            issuer: "https://gateway.example".into(),
            authorization_endpoint: "https://gateway.example/authorize".into(),
            token_endpoint: "https://gateway.example/token".into(),
            client_id: "plugin-test".into(),
            scopes: vec![],
            resource: None,
            callback_path: "/oauth/callback".into(),
        });
        let identity = config.active_provider_identity().unwrap();
        let writeability = credential_writeability(&config, &identity);
        assert!(!writeability.writable);
        assert_eq!(writeability.source, ProviderCredentialSource::ExternalAuth);
        assert!(writeability.reason.unwrap().contains("plugin"));
    }

    /// A route Codewhale owns is writable, and says its source is the store it
    /// would actually write.
    #[test]
    fn a_codewhale_owned_route_is_writable_through_the_secret_store() {
        let config = Config::default();
        let writeability = credential_writeability(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openai),
        );
        assert_eq!(writeability.source, ProviderCredentialSource::SecretStore);
        assert!(writeability.writable);
        assert!(writeability.reason.is_none());
    }

    /// The case #6179 exists for: a literal key in a config file still wins at
    /// request time, so a write here must refuse rather than report a success
    /// the user cannot observe. The reason names the file-owned source.
    #[test]
    fn a_literal_config_key_refuses_the_write_and_says_why() {
        let mut config = Config::default();
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Openai))
            .unwrap()
            .api_key = Some("sk-literal-in-a-config-file".to_string());

        let writeability = credential_writeability(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openai),
        );
        assert_eq!(writeability.source, ProviderCredentialSource::Config);
        assert!(!writeability.writable);
        let reason = writeability.reason.expect("a refusal must name its reason");
        assert!(reason.contains("config file"), "{reason}");
        // The reason is copy, not a credential: it can never carry the value.
        assert!(!reason.contains("sk-literal-in-a-config-file"));
    }

    /// The secret-store sentinel is routing metadata, not a credential, so it
    /// must not be mistaken for a file-owned literal and refused.
    #[test]
    fn the_secret_store_sentinel_is_not_a_file_owned_key() {
        let mut config = Config::default();
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Openai))
            .unwrap()
            .api_key = Some(codewhale_config::API_KEYRING_SENTINEL.to_string());

        let writeability = credential_writeability(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openai),
        );
        assert_eq!(writeability.source, ProviderCredentialSource::SecretStore);
        assert!(writeability.writable);
    }

    /// A route declared to send no credential has nothing to manage, and says
    /// so instead of offering a control that would do nothing.
    #[test]
    fn a_no_auth_route_reports_no_credential_source() {
        let mut config = Config::default();
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Openai))
            .unwrap()
            .auth_mode = Some("none".to_string());

        let writeability = credential_writeability(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openai),
        );
        assert_eq!(writeability.source, ProviderCredentialSource::None);
        assert!(!writeability.writable);
        assert!(writeability.reason.is_some());
    }
}
