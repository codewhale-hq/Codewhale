use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use codewhale_secrets::account::{AccountDeviceAccess, AccountSessionState, RuntimeAccountInfo};

use super::*;

pub fn account_api_base(api_base: Option<&str>) -> Result<String> {
    let requested_base = machine::resolve_api_base(
        api_base,
        std::env::var(machine::MACHINE_API_BASE_ENV).ok().as_deref(),
        std::env::var(CLOUD_API_BASE_ENV).ok().as_deref(),
        DEFAULT_API_BASE,
    );
    Ok(validate_api_base(&requested_base)?.display)
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub profile: String,
    pub api_base: String,
    pub account: RuntimeAccountInfo,
    pub user: Option<CloudUser>,
    pub revision: String,
    pub import_allowed: bool,
}

pub struct AccountAuthLease {
    pub bundle: AuthBundle,
    pub snapshot: AccountSessionSnapshot,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountModel {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub model: String,
    pub protocol: String,
    pub billing_mode: String,
    pub is_default: bool,
}

pub struct AccountModels {
    pub api_base: String,
    pub account_id: String,
    pub session_id: String,
    pub expires_at: i64,
    pub default_model: Option<String>,
    pub models: Vec<AccountModel>,
    pub credential: Option<String>,
    pub snapshot: AccountSessionSnapshot,
}

#[derive(Clone)]
pub struct AccountClient {
    transport: ReqwestTransport,
    store: AccountSessionStore,
    profile: String,
    api_base: String,
}

impl AccountClient {
    pub fn new(profile: Option<&str>, api_base: Option<&str>) -> Result<Self> {
        let requested_base = account_api_base(api_base)?;
        let base = validate_api_base(&requested_base)?;
        let profile = normalized_profile(profile);
        let secrets = cloud_session_secrets()?;
        Ok(Self {
            transport: ReqwestTransport::new(base.url)?,
            store: AccountSessionStore::new(secrets, Some(&profile), &base.display),
            profile,
            api_base: base.display,
        })
    }

    fn client(&self) -> CloudClient<'_, ReqwestTransport> {
        CloudClient {
            transport: &self.transport,
            account_store: self.store.clone(),
        }
    }

    fn revoke_session_later(&self, previous: Option<AccountSessionSnapshot>, bundle: AuthBundle) {
        let client = self.clone();
        let _ = std::thread::Builder::new()
            .name("cw-account-revoke".into())
            .spawn(move || {
                let _ = match previous {
                    Some(previous) => client.client().revoke_previous_session(&previous, &bundle),
                    None => client.client().revoke_session_bundle(&bundle),
                };
            });
    }

    pub fn snapshot(&self) -> Result<AccountSessionSnapshot> {
        Ok(self.store.snapshot()?)
    }

    pub fn status(&self) -> Result<AccountStatus> {
        let snapshot = self.snapshot()?;
        let user = snapshot.load()?.and_then(|stored| stored.bundle.user);
        Ok(AccountStatus {
            profile: self.profile.clone(),
            api_base: self.api_base.clone(),
            account: snapshot.runtime_info_at(Utc::now())?,
            user,
            revision: snapshot.fingerprint(),
            import_allowed: snapshot.is_pristine(),
        })
    }

    pub fn claim(&self) -> Result<AccountSessionSnapshot> {
        Ok(self.store.begin_login()?)
    }

    pub fn claim_if_unchanged(
        &self,
        expected_revision: &str,
    ) -> Result<Option<AccountSessionSnapshot>> {
        self.store.with_transaction(|transaction| {
            if transaction.snapshot().fingerprint() != expected_revision {
                return Ok(None);
            }
            Ok(Some(transaction.renew_revision()?))
        })
    }

    pub fn cancel_claim(&self, expected_revision: &str) -> Result<bool> {
        self.store.with_transaction(|transaction| {
            if transaction.snapshot().fingerprint() != expected_revision {
                return Ok(false);
            }
            transaction.renew_revision()?;
            Ok(true)
        })
    }

    pub fn import(
        &self,
        bundle: AuthBundle,
        expected_revision: &str,
        replace_existing: bool,
    ) -> Result<bool> {
        self.import_with_receipt(bundle, expected_revision, replace_existing)
            .map(|receipt| receipt.is_some())
    }

    pub fn import_with_receipt(
        &self,
        bundle: AuthBundle,
        expected_revision: &str,
        replace_existing: bool,
    ) -> Result<Option<AccountSessionSnapshot>> {
        validate_auth_bundle(&bundle)?;
        let expected = self.snapshot()?;
        if expected.fingerprint() != expected_revision {
            return Ok(None);
        }
        if !replace_existing && !expected.is_pristine() {
            return Ok(None);
        }
        let Some(committed) = self.store.save_if_unchanged(&expected, bundle.clone())? else {
            return Ok(None);
        };
        self.revoke_session_later(Some(expected), bundle);
        Ok(Some(committed))
    }

    pub fn fresh_auth(&self, rejected_token: Option<&str>) -> Result<Option<AccountAuthLease>> {
        let snapshot = self.snapshot()?;
        let Some(stored) = snapshot.load()? else {
            return Ok(None);
        };
        if matches!(
            snapshot.runtime_info_at(Utc::now())?.state,
            AccountSessionState::Expired | AccountSessionState::Revoked
        ) {
            return Ok(None);
        }
        let refresh = rejected_token == Some(stored.bundle.access_token.as_str())
            || !stored
                .bundle
                .session
                .as_ref()
                .and_then(|session| DateTime::parse_from_rfc3339(&session.expires_at).ok())
                .is_some_and(|expires| expires.timestamp() > Utc::now().timestamp() + 120);
        let (bundle, snapshot) = if refresh {
            let Some(renewed) = self.client().refresh_snapshot(&snapshot)? else {
                return Ok(None);
            };
            renewed
        } else {
            (stored.bundle, snapshot)
        };
        let current = self.store.with_transaction(|transaction| {
            Ok::<_, anyhow::Error>(transaction.matches(&snapshot))
        })?;
        Ok(current.then_some(AccountAuthLease { bundle, snapshot }))
    }

    pub fn logout(&self) -> Result<bool> {
        self.client().logout()
    }

    pub fn logout_if_unchanged(&self, snapshot: &AccountSessionSnapshot) -> Result<bool> {
        self.client().logout_from_snapshot(snapshot)
    }

    pub fn connect_models(&self) -> Result<AccountModels> {
        self.connect_models_with_cancel(&AtomicBool::new(false))
    }

    pub fn connect_models_with_cancel(&self, cancelled: &AtomicBool) -> Result<AccountModels> {
        if cancelled.load(Ordering::Acquire) {
            bail!("Codewhale account model connection cancelled");
        }
        let lease = self
            .fresh_auth(None)?
            .ok_or_else(|| anyhow!("Sign in to Codewhale before connecting account models"))?;
        let (response, snapshot) = self.client().execute_from_snapshot(
            HttpMethod::Get,
            "/api/account/model-api",
            None,
            lease.snapshot,
        )?;
        let catalog: serde_json::Value = expect_json(response, &[200])?;
        let data = catalog
            .get("data")
            .and_then(serde_json::Value::as_array)
            .filter(|rows| rows.len() <= 8192)
            .ok_or_else(|| anyhow!("The Codewhale account model catalog was invalid"))?;
        let mut models = Vec::new();
        for row in data {
            let facts = &row["codewhale"];
            if facts["usable"].as_bool() != Some(true) {
                continue;
            }
            if facts["protocol"].as_str() != Some("chat-completions")
                || facts["billingMode"].as_str() != Some("byok_external")
            {
                continue;
            }
            let id = model_text(row, "id")?;
            let provider = model_text(facts, "provider")?;
            let model = model_text(facts, "model")?;
            if id != format!("{provider}/{model}") {
                bail!("The Codewhale account model catalog was invalid");
            }
            models.push(AccountModel {
                id,
                label: facts["label"]
                    .as_str()
                    .map(|label| printable_max(label, 200))
                    .filter(|label| !label.is_empty())
                    .unwrap_or_else(|| model.clone()),
                provider,
                model,
                protocol: model_text(facts, "protocol")?,
                billing_mode: model_text(facts, "billingMode")?,
                is_default: facts["default"].as_bool() == Some(true),
            });
        }
        let stored = snapshot
            .load()?
            .ok_or_else(|| anyhow!("The signed-in account changed. Refresh and try again."))?;
        let account_id = stored
            .bundle
            .user
            .as_ref()
            .map(|user| user.id.clone())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow!("Sign in again before connecting account models"))?;
        let session = stored
            .bundle
            .session
            .as_ref()
            .filter(|session| !session.id.is_empty())
            .ok_or_else(|| anyhow!("Sign in again before connecting account models"))?;
        let session_id = session.id.clone();
        let access_expires = DateTime::parse_from_rfc3339(&session.expires_at)
            .context("The account session expiry was invalid")?
            .timestamp();
        let refresh_expires = DateTime::parse_from_rfc3339(&session.refresh_expires_at)
            .context("The account session expiry was invalid")?
            .timestamp();
        let default_model = catalog["codewhale"]["defaultModel"]
            .as_str()
            .filter(|id| models.iter().any(|model| model.id == *id))
            .map(str::to_string)
            .or_else(|| {
                models
                    .iter()
                    .find(|model| model.is_default)
                    .map(|model| model.id.clone())
            });
        let mut result = AccountModels {
            api_base: format!("{}/v1", self.api_base),
            account_id: account_id.clone(),
            session_id: session_id.clone(),
            expires_at: 0,
            default_model,
            models,
            credential: None,
            snapshot,
        };
        if result.models.is_empty() {
            let current = self.store.with_transaction(|transaction| {
                Ok::<_, anyhow::Error>(transaction.matches(&result.snapshot))
            })?;
            if !current {
                bail!("The signed-in account changed. Refresh and try again.");
            }
            return Ok(result);
        }
        if cancelled.load(Ordering::Acquire) {
            bail!("Codewhale account model connection cancelled");
        }
        let device_id = self.store.device_id()?;
        let mut issued_id: Option<String> = None;
        let connected = self.store.with_transaction(|transaction| {
            if !transaction.matches(&result.snapshot) || access_expires <= Utc::now().timestamp() {
                bail!("The signed-in account changed. Refresh and try again.");
            }
            if cancelled.load(Ordering::Acquire) {
                bail!("Codewhale account model connection cancelled");
            }
            let current = transaction.snapshot();
            if let Some(access) = current.device_access()? {
                if access.device_id != device_id {
                    bail!("The Codewhale device connection changed. Reconnect your account.");
                }
                machine::MachineKey::parse(&access.credential)?;
                if access.expires_at > Utc::now().timestamp() {
                    let checked = self.transport.execute(CloudRequest {
                        method: HttpMethod::Get,
                        path: "/v1/models".into(),
                        bearer: Some(access.credential.clone()),
                        body: None,
                    })?;
                    match checked.status {
                        200 => return Ok((access, current)),
                        401 => {}
                        _ => return Err(response_error(&checked)),
                    }
                }
            }
            if cancelled.load(Ordering::Acquire) {
                bail!("Codewhale account model connection cancelled");
            }
            let response = self.transport.execute(CloudRequest {
                method: HttpMethod::Post,
                path: "/api/account/device-link".into(),
                bearer: Some(stored.bundle.access_token.clone()),
                body: Some(serde_json::to_vec(&serde_json::json!({
                    "deviceId": device_id,
                    "deviceName": "Codewhale",
                    "expectedAccountId": account_id,
                }))?),
            })?;
            let linked: LinkedDevice = expect_json(response, &[201])?;
            if linked.api_key.id.len() == 24
                && linked
                    .api_key
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                issued_id = Some(linked.api_key.id.clone());
            }
            let key = machine::MachineKey::parse(&linked.secret)?;
            let expires_at = DateTime::parse_from_rfc3339(&linked.api_key.expires_at)
                .context("The account device expiry was invalid")?
                .timestamp();
            if !linked.session_bound
                || linked.api_key.scopes.len() != 1
                || linked.api_key.scopes[0] != "models:infer"
                || linked.api_key.revoked_at.is_some()
                || linked.api_key.context.kind != "device"
                || linked.api_key.context.device_id != device_id
                || linked.api_key.context.session_id != session_id
                || key.head().strip_prefix("cwc_key_") != Some(linked.api_key.id.as_str())
                || expires_at <= Utc::now().timestamp()
                || expires_at > refresh_expires
            {
                bail!("The Codewhale device connection could not be verified");
            }
            let access = AccountDeviceAccess {
                device_id: device_id.clone(),
                credential: linked.secret,
                api_key_id: linked.api_key.id,
                account_id: account_id.clone(),
                session_id: session_id.clone(),
                expires_at,
            };
            transaction.set_device_access(access.clone())?;
            Ok((access, transaction.snapshot()))
        });
        let (access, snapshot) = match connected {
            Ok(connected) => connected,
            Err(error) => {
                if let Some(id) = issued_id {
                    let _ = self.transport.execute(CloudRequest {
                        method: HttpMethod::Delete,
                        path: format!("/api/account/api-keys/{id}"),
                        bearer: Some(stored.bundle.access_token),
                        body: None,
                    });
                }
                return Err(error);
            }
        };
        result.expires_at = access.expires_at.min(access_expires);
        result.credential = Some(access.credential);
        result.snapshot = snapshot;
        Ok(result)
    }

    pub fn start_login(&self, timeout: Duration) -> Result<AccountLogin> {
        self.start_login_with_cancel(timeout, Arc::new(AtomicBool::new(false)))
    }

    pub fn start_login_with_cancel(
        &self,
        timeout: Duration,
        cancelled: Arc<AtomicBool>,
    ) -> Result<AccountLogin> {
        if timeout.is_zero() || timeout > Duration::from_secs(MAX_LOGIN_TIMEOUT_SECONDS) {
            bail!("Invalid Codewhale account login timeout");
        }
        let expected = self.store.with_transaction(|transaction| {
            if cancelled.load(Ordering::Acquire) {
                bail!("Codewhale account login cancelled");
            }
            Ok::<_, anyhow::Error>(transaction.renew_revision()?)
        })?;
        let started = Instant::now();
        if cancelled.load(Ordering::Acquire) {
            bail!("Codewhale account login cancelled");
        }
        let device = self.client().start_device()?;
        validate_device_code(&device.device_code)?;
        validate_user_code(&device.user_code)?;
        let verification_uri = validate_verification_url(
            &device.verification_uri,
            &self.api_base,
            &device.user_code,
            false,
        )?;
        let verification_uri_complete = validate_verification_url(
            &device.verification_uri_complete,
            &self.api_base,
            &device.user_code,
            true,
        )?;
        let lifetime = timeout.min(Duration::from_secs(
            device.expires_in.clamp(1, MAX_LOGIN_TIMEOUT_SECONDS),
        ));
        let interval = if device.interval == 0 {
            codewhale_config::device_code::DEFAULT_POLL_INTERVAL_SECS
        } else {
            device.interval.min(10)
        };
        Ok(AccountLogin {
            client: self.clone(),
            expected,
            prompt: AccountLoginPrompt {
                user_code: device.user_code.clone(),
                verification_uri,
                verification_uri_complete,
                expires_in: lifetime.as_secs(),
                interval,
            },
            device,
            cancelled: AccountLoginCancellation(cancelled),
            deadline: started + lifetime,
            next_poll: started,
            pending_bundle: None,
            finished: false,
        })
    }
}

fn model_text(value: &serde_json::Value, key: &str) -> Result<String> {
    value[key]
        .as_str()
        .filter(|text| {
            !text.is_empty()
                && text.len() <= 512
                && !text.chars().any(char::is_control)
                && !text.chars().any(char::is_whitespace)
        })
        .map(str::to_string)
        .ok_or_else(|| anyhow!("The Codewhale account model catalog was invalid"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinkedDevice {
    secret: String,
    api_key: LinkedDeviceKey,
    session_bound: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinkedDeviceKey {
    id: String,
    scopes: Vec<String>,
    expires_at: String,
    revoked_at: Option<String>,
    context: LinkedDeviceContext,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinkedDeviceContext {
    kind: String,
    device_id: String,
    session_id: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginPrompt {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Clone)]
pub struct AccountLoginCancellation(Arc<AtomicBool>);

impl AccountLoginCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}

pub enum AccountLoginPoll {
    Pending { retry_after: Duration },
    Complete { user: CloudUser },
    Cancelled,
    Expired,
}

pub struct AccountLogin {
    client: AccountClient,
    expected: AccountSessionSnapshot,
    device: DeviceStart,
    prompt: AccountLoginPrompt,
    cancelled: AccountLoginCancellation,
    deadline: Instant,
    next_poll: Instant,
    pending_bundle: Option<AuthBundle>,
    finished: bool,
}

impl AccountLogin {
    pub fn prompt(&self) -> &AccountLoginPrompt {
        &self.prompt
    }

    pub fn cancellation(&self) -> AccountLoginCancellation {
        self.cancelled.clone()
    }

    pub fn poll(&mut self) -> Result<AccountLoginPoll> {
        let result = self.poll_once();
        if !matches!(result, Ok(AccountLoginPoll::Pending { .. })) {
            self.finished = true;
        }
        if matches!(
            result,
            Ok(AccountLoginPoll::Cancelled | AccountLoginPoll::Expired)
        ) {
            self.revoke_pending();
        }
        result
    }

    fn revoke_pending(&mut self) {
        if let Some(bundle) = self.pending_bundle.take() {
            self.client.revoke_session_later(None, bundle);
        }
    }

    fn poll_once(&mut self) -> Result<AccountLoginPoll> {
        if self.finished || self.cancelled.0.load(Ordering::Acquire) {
            return Ok(AccountLoginPoll::Cancelled);
        }
        let now = Instant::now();
        if now >= self.deadline {
            return Ok(AccountLoginPoll::Expired);
        }
        if now < self.next_poll {
            return Ok(AccountLoginPoll::Pending {
                retry_after: (self.next_poll - now).min(self.deadline - now),
            });
        }
        if !self.client.store.with_transaction(|transaction| {
            Ok::<_, anyhow::Error>(transaction.matches(&self.expected))
        })? {
            return Ok(AccountLoginPoll::Cancelled);
        }
        if let Some(bundle) = self.pending_bundle.as_mut() {
            if bundle.user.is_none() {
                let response = self.client.transport.execute(CloudRequest {
                    method: HttpMethod::Get,
                    path: "/api/me".into(),
                    bearer: Some(bundle.access_token.clone()),
                    body: None,
                })?;
                bundle.user = Some(expect_json::<MeResponse>(response, &[200])?.user);
            }
        } else {
            match self.client.client().poll_device_once(&self.device)? {
                DevicePollOutcome::Complete(bundle) => self.pending_bundle = Some(bundle),
                DevicePollOutcome::Pending | DevicePollOutcome::SlowDown { .. } => {
                    self.next_poll = Instant::now() + Duration::from_secs(self.prompt.interval);
                    return Ok(AccountLoginPoll::Pending {
                        retry_after: self
                            .next_poll
                            .min(self.deadline)
                            .saturating_duration_since(Instant::now()),
                    });
                }
            }
        }
        let bundle = self
            .pending_bundle
            .as_ref()
            .ok_or_else(|| anyhow!("The Codewhale account authorization was unavailable"))?;
        if bundle.user.is_none() {
            return Ok(AccountLoginPoll::Pending {
                retry_after: Duration::ZERO,
            });
        }
        let Some(user) = bundle
            .user
            .as_ref()
            .filter(|user| !user.id.trim().is_empty())
        else {
            bail!("The Codewhale service returned an account without an ID");
        };
        let user = user.clone();
        validate_auth_bundle(bundle)?;
        if Instant::now() >= self.deadline {
            return Ok(AccountLoginPoll::Expired);
        }
        let bundle = self
            .pending_bundle
            .take()
            .ok_or_else(|| anyhow!("The Codewhale account authorization was unavailable"))?;
        let committed = self.client.store.with_transaction(|transaction| {
            if self.cancelled.0.load(Ordering::Acquire)
                || !transaction.matches(&self.expected)
                || Instant::now() >= self.deadline
            {
                return Ok::<_, anyhow::Error>(false);
            }
            transaction.replace(bundle.clone())?;
            Ok(true)
        })?;
        if !committed {
            self.client.revoke_session_later(None, bundle);
            return Ok(AccountLoginPoll::Cancelled);
        }
        self.client
            .revoke_session_later(Some(self.expected.clone()), bundle);
        Ok(AccountLoginPoll::Complete { user })
    }
}

impl Drop for AccountLogin {
    fn drop(&mut self) {
        if !self.finished {
            self.cancelled.cancel();
        }
        self.revoke_pending();
    }
}
