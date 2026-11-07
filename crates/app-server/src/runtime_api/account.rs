use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderValue, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use codewhale_account_cli::{AccountClient, AccountStatus};
use codewhale_secrets::account::AccountAuthBundle;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, RuntimeApiState, auth};

pub(super) fn routes(state: &RuntimeApiState) -> Router<RuntimeApiState> {
    Router::new()
        .route("/v1/account/status", get(status))
        .route("/v1/account/lease", post(lease))
        .route("/v1/account/import", post(import))
        .route("/v1/account/session", delete(logout))
        .route("/v1/account/claim", post(claim).delete(cancel_claim))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_host_token,
        ))
        .layer(middleware::from_fn(no_store))
}

async fn no_store(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

fn client(state: &RuntimeApiState) -> Result<AccountClient, ApiError> {
    AccountClient::new(
        state.config_profile.as_deref(),
        Some(&codewhale_engine::profile_constitution::runtime_account_api_base()),
    )
    .map_err(|_| ApiError::internal("Codewhale account storage is unavailable"))
}

fn receipt(status: AccountStatus) -> Value {
    let display_name = status
        .user
        .as_ref()
        .map(|user| {
            if user.display_name.trim().is_empty() {
                user.id.as_str()
            } else {
                user.display_name.as_str()
            }
        })
        .map(|name| name.chars().take(120).collect::<String>());
    json!({
        "apiBase": status.api_base,
        "profile": status.profile,
        "state": status.account.state,
        "revision": status.revision,
        "accountId": status.account.account_id,
        "sessionId": status.account.session_id,
        "expiresAt": status.account.expires_at,
        "displayName": display_name,
        "importAllowed": status.import_allowed,
    })
}

fn read_status(client: &AccountClient) -> Result<Value, ApiError> {
    client
        .status()
        .map(receipt)
        .map_err(|_| ApiError::internal("Codewhale account storage could not be read"))
}

async fn blocking(
    operation: impl FnOnce() -> Result<Value, ApiError> + Send + 'static,
) -> Result<Response, ApiError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| ApiError::internal("Codewhale account operation failed"))?
        .map(|body| Json(body).into_response())
}

async fn status(State(state): State<RuntimeApiState>) -> Result<Response, ApiError> {
    blocking(move || read_status(&client(&state)?)).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LeaseRequest {
    rejected_access_token: Option<String>,
}

async fn lease(
    State(state): State<RuntimeApiState>,
    Json(request): Json<LeaseRequest>,
) -> Result<Response, ApiError> {
    if request
        .rejected_access_token
        .as_ref()
        .is_some_and(|token| token.len() > 64 * 1024)
    {
        return Err(ApiError::bad_request("Invalid rejected account credential"));
    }
    blocking(move || {
        let client = client(&state)?;
        let lease = client
            .fresh_auth(request.rejected_access_token.as_deref())
            .map_err(|_| ApiError::conflict("Reconnect your Codewhale account and try again"))?;
        let mut status = read_status(&client)?;
        if let Some(lease) = lease {
            if status["revision"].as_str() != Some(lease.snapshot.fingerprint().as_str())
                || status["accountId"].as_str()
                    != lease.bundle.user.as_ref().map(|user| user.id.as_str())
                || status["sessionId"].as_str()
                    != lease
                        .bundle
                        .session
                        .as_ref()
                        .map(|session| session.id.as_str())
                || status["accountId"].as_str().is_none()
                || status["sessionId"].as_str().is_none()
            {
                return Err(ApiError::conflict(
                    "The Codewhale account changed; try again",
                ));
            }
            status["accessToken"] = json!(lease.bundle.access_token);
        }
        Ok(status)
    })
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportRequest {
    expected_revision: String,
    auth: AccountAuthBundle,
    #[serde(default)]
    replace_existing: bool,
}

async fn import(
    State(state): State<RuntimeApiState>,
    Json(request): Json<ImportRequest>,
) -> Result<Response, ApiError> {
    blocking(move || {
        let client = client(&state)?;
        let committed = client
            .import_with_receipt(
                request.auth,
                &request.expected_revision,
                request.replace_existing,
            )
            .map_err(|_| {
                ApiError::internal("The account connection could not be confirmed; try again")
            })?
            .ok_or_else(|| {
                ApiError::conflict("The Codewhale account changed; try again")
                    .with_code("account_import_superseded")
            })?;
        let stored = committed
            .load()
            .map_err(|_| ApiError::internal("The account connection could not be confirmed"))?
            .ok_or_else(|| ApiError::internal("The account connection could not be confirmed"))?;
        let revision = committed.fingerprint();
        let mut status = receipt(AccountStatus {
            profile: codewhale_secrets::account::normalize_account_profile(
                state.config_profile.as_deref(),
            ),
            api_base: stored.api_base,
            account: committed
                .runtime_info_at(chrono::Utc::now())
                .map_err(|_| ApiError::internal("The account connection could not be confirmed"))?,
            user: stored.bundle.user,
            revision: revision.clone(),
            import_allowed: false,
        });
        status["committedRevision"] = json!(revision);
        Ok(status)
    })
    .await
}

async fn claim(
    State(state): State<RuntimeApiState>,
    Json(request): Json<CancelClaimRequest>,
) -> Result<Response, ApiError> {
    blocking(move || {
        let client = client(&state)?;
        let claimed = client
            .claim_if_unchanged(&request.expected_revision)
            .map_err(|_| ApiError::internal("The account connection could not be started"))?
            .ok_or_else(|| ApiError::conflict("The Codewhale account changed; try again"))?;
        let status = read_status(&client)?;
        if status["revision"].as_str() != Some(claimed.fingerprint().as_str()) {
            return Err(ApiError::conflict(
                "The Codewhale account changed; try again",
            ));
        }
        Ok(status)
    })
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CancelClaimRequest {
    expected_revision: String,
}

async fn cancel_claim(
    State(state): State<RuntimeApiState>,
    Json(request): Json<CancelClaimRequest>,
) -> Result<Response, ApiError> {
    blocking(move || {
        let client = client(&state)?;
        let cancelled = client
            .cancel_claim(&request.expected_revision)
            .map_err(|_| ApiError::internal("The account connection could not be cancelled"))?;
        let mut status = read_status(&client)?;
        status["cancelled"] = json!(cancelled);
        Ok(status)
    })
    .await
}

async fn logout(
    State(state): State<RuntimeApiState>,
    Json(request): Json<CancelClaimRequest>,
) -> Result<Response, ApiError> {
    blocking(move || {
        let client = client(&state)?;
        let expected = client
            .snapshot()
            .map_err(|_| ApiError::internal("Codewhale account storage could not be read"))?;
        if expected.fingerprint() != request.expected_revision {
            return Err(ApiError::conflict(
                "The Codewhale account changed; try again",
            ));
        }
        let expected_session = expected
            .load()
            .ok()
            .flatten()
            .and_then(|stored| stored.bundle.session)
            .map(|session| session.id);
        let expected_profile =
            codewhale_secrets::account::normalize_account_profile(state.config_profile.as_deref());
        let revoked = client
            .logout_if_unchanged(&expected)
            .map_err(|_| ApiError::internal("The Codewhale account could not be signed out"))?;
        {
            let config = state.config.read();
            let mut access = config.account_model_access.write();
            if access.as_ref().is_some_and(|access| {
                expected_session.as_deref() == Some(access.session_id.as_str())
                    && codewhale_secrets::account::normalize_account_profile(
                        access.profile.as_deref(),
                    ) == expected_profile
            }) {
                access.take();
            }
        }
        let mut status = read_status(&client)?;
        status["remoteRevoked"] = json!(revoked);
        Ok(status)
    })
    .await
}
