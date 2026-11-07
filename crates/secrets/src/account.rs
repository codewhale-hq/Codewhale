//! Shared secure-storage contract for Codewhale account sessions.
//!
//! The CLI owns device authorization and refresh traffic. This module owns the
//! durable record written by that flow so the TUI and Runtime API can recognize
//! the same account without copying tokens or inventing another login protocol.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{Secrets, SecretsError};

/// Production account API origin used when no override is configured.
pub const DEFAULT_ACCOUNT_API_BASE: &str = "https://api.codewhale.net";
/// Environment variable that selects the account API origin.
pub const ACCOUNT_API_BASE_ENV: &str = "CODEWHALE_CLOUD_API_BASE";
/// Current serialized account-session record version.
pub const ACCOUNT_SESSION_SCHEMA_VERSION: u8 = 1;

const MAX_TOKEN_BYTES: usize = 64 * 1024;
const MAX_SCOPES: usize = 64;
const MAX_SCOPE_BYTES: usize = 128;

/// A short-lived access credential and its durable refresh/session metadata.
///
/// This type intentionally does not implement `Debug`, preventing accidental
/// token disclosure through ordinary diagnostic formatting.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountAuthBundle {
    /// Authentication scheme returned by the account service.
    pub token_type: String,
    /// Short-lived bearer credential. Never serialize this outside secure storage.
    pub access_token: String,
    /// Refresh credential. Never serialize this outside secure storage.
    pub refresh_token: String,
    /// Durable server session metadata, when returned by the service.
    #[serde(default)]
    pub session: Option<AccountSession>,
    /// Cached non-secret account record, when returned by the service.
    #[serde(default)]
    pub user: Option<AccountUser>,
}

/// Durable account-session metadata supplied by the account service.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSession {
    /// Durable session identifier shared across Codewhale surfaces.
    pub id: String,
    /// Authorization provider used to establish the session.
    #[serde(default)]
    pub provider: String,
    /// Linked authorization providers recorded by the account service.
    #[serde(default)]
    pub providers: Vec<String>,
    /// Explicit bounded scopes granted to this session.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Access-credential expiration in RFC 3339 format.
    #[serde(default)]
    pub expires_at: String,
    /// Refresh/session expiration in RFC 3339 format.
    #[serde(default)]
    pub refresh_expires_at: String,
    /// Explicit server session state when supplied.
    #[serde(default)]
    pub status: String,
    /// Explicit revocation timestamp when supplied.
    #[serde(default)]
    pub revoked_at: String,
}

/// Cached account identity and non-secret presentation fields.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUser {
    /// Stable Codewhale account identifier.
    #[serde(default)]
    pub id: String,
    /// User-facing account name.
    #[serde(default)]
    pub display_name: String,
    /// Account email returned by the service. Runtime metadata never exposes it.
    #[serde(default)]
    pub email: String,
    /// Account residency region returned by the service.
    #[serde(default)]
    pub region: String,
    /// Account plan returned by the service.
    #[serde(default)]
    pub plan: String,
    /// Provider-key presence metadata; values never contain provider credentials.
    #[serde(default)]
    pub model_keys: BTreeMap<String, AccountModelKeyState>,
}

/// Non-secret provider-key presence metadata returned by the account service.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountModelKeyState {
    /// Whether the account service reports a credential for this provider.
    #[serde(default)]
    pub configured: bool,
    /// Service-reported credential state (for example `active`, `invalid`).
    ///
    /// Non-secret presentation metadata: it says whether a stored key is
    /// usable without ever revealing the key. Absent when the service did not
    /// state one.
    #[serde(default)]
    pub state: Option<String>,
    /// Non-secret label the account holder gave the stored credential.
    #[serde(default)]
    pub label: Option<String>,
}

/// Versioned secure-storage envelope shared by every local Codewhale surface.
///
/// This type intentionally does not implement `Debug` because `bundle`
/// contains access and refresh credentials.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredAccountAuth {
    /// Serialized record version.
    pub schema_version: u8,
    /// Exact canonical API origin that owns this session.
    pub api_base: String,
    /// Secret account bundle held in the secret store.
    pub bundle: AccountAuthBundle,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[doc = "A session-bound device key held inside the shared private account record."]
pub struct AccountDeviceAccess {
    #[doc = "Stable device UUID scoped to the account profile and API origin."]
    pub device_id: String,
    #[doc = "Secret device credential; never include it in runtime receipts or diagnostics."]
    pub credential: String,
    #[doc = "Server identifier used to revoke this device key."]
    pub api_key_id: String,
    #[doc = "Account that owns the device key."]
    pub account_id: String,
    #[doc = "Parent authentication session that authorizes this key."]
    pub session_id: String,
    #[doc = "Device-key expiry as Unix seconds, bounded by the parent refresh lifetime."]
    pub expires_at: i64,
}

/// Normalized account states exposed by the token-free Runtime API contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountSessionState {
    /// No valid secure-store session was found for the selected profile/origin.
    SignedOut,
    /// The cached session and access credential are within their recorded lifetime.
    Authenticated,
    /// Durable identity remains cached, but the access credential has expired.
    OfflineCached,
    /// The durable refresh/session lifetime has ended.
    Expired,
    /// The stored session carries an explicit revocation receipt.
    Revoked,
}

/// Token-free account receipt returned by `GET /v1/runtime/info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeAccountInfo {
    /// Runtime account receipt schema version.
    pub schema_version: u8,
    /// Current account-session state.
    pub state: AccountSessionState,
    /// Exact account API origin used to locate the secure session.
    pub api_base: String,
    /// Stable account identifier, present only when read from secure storage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Durable session identifier, present only when read from secure storage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Explicit session scopes from secure storage; never inferred from identity.
    pub scopes: Vec<String>,
    /// Access-credential expiration, when the stored value is valid RFC 3339.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

impl RuntimeAccountInfo {
    /// Build the fail-closed signed-out receipt for an API origin.
    #[must_use]
    pub fn signed_out(api_base: impl Into<String>) -> Self {
        Self {
            schema_version: ACCOUNT_SESSION_SCHEMA_VERSION,
            state: AccountSessionState::SignedOut,
            api_base: api_base.into(),
            account_id: None,
            session_id: None,
            scopes: Vec::new(),
            expires_at: None,
        }
    }
}

/// Failures while selecting, decoding, or validating account session storage.
#[derive(Debug, Error)]
pub enum AccountSessionError {
    /// Underlying credential-store failure.
    #[error(transparent)]
    Secrets(#[from] SecretsError),
    /// Stored session JSON could not be decoded.
    #[error("the local Codewhale account session is unreadable")]
    UnreadableRecord(#[source] serde_json::Error),
    /// Stored or newly returned authentication credentials are malformed.
    #[error("the Codewhale account session contains invalid credentials")]
    InvalidCredentials,
    /// No approved secure session backend is available.
    #[error(
        "Codewhale account sessions could not open a secret store; check that HOME is writable or set CODEWHALE_HOME to an absolute path"
    )]
    SecureStoreUnavailable,
}

/// Profile- and origin-scoped view of the shared account credential record.
#[derive(Clone)]
pub struct AccountSessionStore {
    secrets: Secrets,
    auth_slot: String,
    api_base: String,
}

/// Opaque account-store revision. Contains credentials; deliberately no Debug.
#[derive(Clone)]
pub struct AccountSessionSnapshot {
    raw: Option<String>,
    slot: String,
    api_base: String,
}

impl AccountSessionSnapshot {
    #[doc = "Read validated device custody from this snapshot; the caller checks key expiry."]
    pub fn device_access(&self) -> Result<Option<AccountDeviceAccess>, AccountSessionError> {
        let Some(stored) = self.load()? else {
            return Ok(None);
        };
        let value: serde_json::Value =
            serde_json::from_str(self.raw.as_deref().unwrap_or_default())
                .map_err(AccountSessionError::UnreadableRecord)?;
        let Some(access) = value.get("deviceAccess") else {
            return Ok(None);
        };
        let access: AccountDeviceAccess = serde_json::from_value(access.clone())
            .map_err(AccountSessionError::UnreadableRecord)?;
        validate_device_access(&stored.bundle, &access)?;
        Ok(Some(access))
    }

    #[doc = "Whether the captured slot contains no record, including no logout tombstone."]
    pub fn is_pristine(&self) -> bool {
        self.raw.is_none()
    }

    #[doc = "Project this exact captured revision into token-free account state."]
    pub fn runtime_info_at(
        &self,
        now: DateTime<Utc>,
    ) -> Result<RuntimeAccountInfo, AccountSessionError> {
        Ok(match self.load()? {
            Some(stored) => runtime_account_info_from_stored(stored, now),
            None => RuntimeAccountInfo::signed_out(self.api_base.clone()),
        })
    }

    #[doc = "Hash the slot, API origin and exact record bytes into an opaque revision."]
    pub fn fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"codewhale-account-revision-v1\0");
        digest.update(self.slot.as_bytes());
        digest.update([0]);
        digest.update(self.api_base.as_bytes());
        digest.update([0]);
        if let Some(raw) = &self.raw {
            digest.update([1]);
            digest.update(raw.as_bytes());
        } else {
            digest.update([0]);
        }
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Decode and validate the captured record without re-reading storage.
    pub fn load(&self) -> Result<Option<StoredAccountAuth>, AccountSessionError> {
        let Some(raw) = &self.raw else {
            return Ok(None);
        };
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(AccountSessionError::UnreadableRecord)?;
        if value.get("signedOut").is_some() {
            let signed_out: SignedOutAccount =
                serde_json::from_value(value).map_err(AccountSessionError::UnreadableRecord)?;
            if signed_out.schema_version != ACCOUNT_SESSION_SCHEMA_VERSION
                || signed_out.api_base != self.api_base
                || !signed_out.signed_out
                || uuid::Uuid::parse_str(&signed_out.revision).is_err()
            {
                return Err(AccountSessionError::InvalidCredentials);
            }
            return Ok(None);
        }
        let stored: StoredAccountAuth =
            serde_json::from_value(value).map_err(AccountSessionError::UnreadableRecord)?;
        if stored.schema_version != ACCOUNT_SESSION_SCHEMA_VERSION
            || stored.api_base != self.api_base
        {
            return Ok(None);
        }
        validate_account_auth_bundle(&stored.bundle)?;
        Ok(Some(stored))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SignedOutAccount {
    schema_version: u8,
    api_base: String,
    signed_out: bool,
    revision: String,
}

fn signed_out_record(api_base: &str) -> String {
    serde_json::json!({
        "schemaVersion": ACCOUNT_SESSION_SCHEMA_VERSION,
        "apiBase": api_base,
        "signedOut": true,
        "revision": uuid::Uuid::new_v4().to_string(),
    })
    .to_string()
}

fn validate_device_access(
    bundle: &AccountAuthBundle,
    access: &AccountDeviceAccess,
) -> Result<(), AccountSessionError> {
    let now = Utc::now();
    let parent_expiry = bundle.session.as_ref().and_then(|session| {
        (!matches!(
            classify_session_state(session, now),
            AccountSessionState::Expired | AccountSessionState::Revoked
        ))
        .then(|| parse_rfc3339(&session.refresh_expires_at))
        .flatten()
    });
    let credential_valid = access
        .credential
        .strip_prefix(&format!("cwc_key_{}_", access.api_key_id))
        .is_some_and(|suffix| {
            suffix.len() == 43
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        });
    if access.account_id.is_empty()
        || access.session_id.is_empty()
        || bundle.user.as_ref().map(|user| user.id.as_str()) != Some(access.account_id.as_str())
        || bundle.session.as_ref().map(|session| session.id.as_str())
            != Some(access.session_id.as_str())
        || uuid::Uuid::parse_str(&access.device_id).is_err()
        || access.expires_at <= 0
        || parent_expiry
            .is_none_or(|expiry| expiry <= now || access.expires_at > expiry.timestamp())
        || !credential_valid
        || access.api_key_id.len() != 24
        || !access
            .api_key_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(AccountSessionError::InvalidCredentials);
    }
    Ok(())
}

fn auth_record(
    api_base: &str,
    bundle: AccountAuthBundle,
    previous: Option<&str>,
) -> Result<String, AccountSessionError> {
    let access = previous
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| value.get("deviceAccess").cloned())
        .and_then(|value| serde_json::from_value::<AccountDeviceAccess>(value).ok())
        .filter(|access| validate_device_access(&bundle, access).is_ok());
    let mut value = serde_json::to_value(StoredAccountAuth {
        schema_version: ACCOUNT_SESSION_SCHEMA_VERSION,
        api_base: api_base.into(),
        bundle,
    })
    .map_err(AccountSessionError::UnreadableRecord)?;
    value["revision"] = uuid::Uuid::new_v4().to_string().into();
    if let Some(access) = access {
        value["deviceAccess"] =
            serde_json::to_value(access).map_err(AccountSessionError::UnreadableRecord)?;
    }
    serde_json::to_string(&value).map_err(AccountSessionError::UnreadableRecord)
}

/// Locked account entry. Never reacquire its store lock from a callback.
pub struct AccountSessionTransaction<'a> {
    raw: &'a mut Option<String>,
    slot: &'a str,
    api_base: &'a str,
}
impl AccountSessionTransaction<'_> {
    #[doc = "Renew the revision while preserving the current authentication state."]
    pub fn renew_revision(&mut self) -> Result<AccountSessionSnapshot, AccountSessionError> {
        match self.load()? {
            Some(stored) => self.replace(stored.bundle)?,
            None => self.clear(),
        }
        Ok(self.snapshot())
    }

    #[doc = "Store a validated device credential inside this locked account record."]
    pub fn set_device_access(
        &mut self,
        access: AccountDeviceAccess,
    ) -> Result<(), AccountSessionError> {
        let Some(stored) = self.load()? else {
            return Err(AccountSessionError::InvalidCredentials);
        };
        validate_device_access(&stored.bundle, &access)?;
        let raw = auth_record(self.api_base, stored.bundle, None)?;
        let mut value: serde_json::Value =
            serde_json::from_str(&raw).map_err(AccountSessionError::UnreadableRecord)?;
        value["deviceAccess"] =
            serde_json::to_value(access).map_err(AccountSessionError::UnreadableRecord)?;
        *self.raw = Some(value.to_string());
        Ok(())
    }

    /// Capture the exact current revision without unlocking.
    pub fn snapshot(&self) -> AccountSessionSnapshot {
        AccountSessionSnapshot {
            raw: self.raw.clone(),
            slot: self.slot.into(),
            api_base: self.api_base.into(),
        }
    }
    /// Decode the current record; errors may be recovered by explicit logout.
    pub fn load(&self) -> Result<Option<StoredAccountAuth>, AccountSessionError> {
        self.snapshot().load()
    }
    /// Compare an admitted snapshot without reacquiring the lock.
    pub fn matches(&self, snapshot: &AccountSessionSnapshot) -> bool {
        self.slot == snapshot.slot
            && self.api_base == snapshot.api_base
            && *self.raw == snapshot.raw
    }
    /// Replace this entry on successful transaction completion.
    pub fn replace(&mut self, bundle: AccountAuthBundle) -> Result<(), AccountSessionError> {
        validate_account_auth_bundle(&bundle)?;
        *self.raw = Some(auth_record(self.api_base, bundle, self.raw.as_deref())?);
        Ok(())
    }
    /// Remove this entry on successful transaction completion.
    pub fn clear(&mut self) {
        *self.raw = Some(signed_out_record(self.api_base));
    }
}

impl AccountSessionStore {
    /// Create a store view for one local profile and one validated API origin.
    #[must_use]
    pub fn new(secrets: Secrets, profile: Option<&str>, api_base: &str) -> Self {
        let profile = normalize_account_profile(profile);
        let api_base = api_base.trim().trim_end_matches('/').to_string();
        Self {
            auth_slot: account_auth_slot(&profile, &api_base),
            secrets,
            api_base,
        }
    }

    /// Load and validate the selected account session from secure storage.
    pub fn load(&self) -> Result<Option<StoredAccountAuth>, AccountSessionError> {
        self.snapshot()?.load()
    }

    /// Capture exact account-store bytes, including malformed/obsolete records
    /// so logout can clear only the revision it inspected.
    pub fn snapshot(&self) -> Result<AccountSessionSnapshot, AccountSessionError> {
        Ok(AccountSessionSnapshot {
            raw: self.secrets.get(&self.auth_slot)?,
            slot: self.auth_slot.clone(),
            api_base: self.api_base.clone(),
        })
    }

    #[doc = "Renew the shared revision to fence older login completions."]
    pub fn begin_login(&self) -> Result<AccountSessionSnapshot, AccountSessionError> {
        self.with_transaction(|transaction| transaction.renew_revision())
    }

    #[doc = "Read or create this profile and API origin's stable device UUID."]
    pub fn device_id(&self) -> Result<String, AccountSessionError> {
        let slot =
            self.auth_slot
                .replacen("codewhale-cloud-auth-v1-", "codewhale-device-id-v1-", 1);
        self.secrets
            .with_entry_transaction(&slot, |raw| {
                let id = match raw {
                    Some(id) if uuid::Uuid::parse_str(id).is_ok() => id.clone(),
                    Some(_) => {
                        return Err(SecretsError::Keyring(
                            "Invalid Codewhale device identity".into(),
                        ));
                    }
                    None => uuid::Uuid::new_v4().to_string(),
                };
                *raw = Some(id.clone());
                Ok(id)
            })
            .map_err(AccountSessionError::from)
    }

    /// Save a renewed bundle only if the captured session has not changed.
    /// Returns the exact committed revision, or None on a concurrent change.
    pub fn save_if_unchanged(
        &self,
        expected: &AccountSessionSnapshot,
        bundle: AccountAuthBundle,
    ) -> Result<Option<AccountSessionSnapshot>, AccountSessionError> {
        validate_account_auth_bundle(&bundle)?;
        if expected.slot != self.auth_slot || expected.api_base != self.api_base {
            return Err(AccountSessionError::InvalidCredentials);
        }
        let raw = auth_record(&self.api_base, bundle, expected.raw.as_deref())?;
        if !self
            .secrets
            .compare_exchange(&self.auth_slot, expected.raw.as_deref(), Some(&raw))?
        {
            return Ok(None);
        }
        Ok(Some(AccountSessionSnapshot {
            raw: Some(raw),
            slot: self.auth_slot.clone(),
            api_base: self.api_base.clone(),
        }))
    }

    /// Remove exactly the captured revision; never erase a newer sign-in.
    pub fn clear_if_unchanged(
        &self,
        expected: &AccountSessionSnapshot,
    ) -> Result<bool, AccountSessionError> {
        if expected.slot != self.auth_slot || expected.api_base != self.api_base {
            return Err(AccountSessionError::InvalidCredentials);
        }
        Ok(self.secrets.compare_exchange(
            &self.auth_slot,
            expected.raw.as_deref(),
            Some(&signed_out_record(&self.api_base)),
        )?)
    }

    /// Serialize an account lifecycle operation with every backend writer.
    /// Return errors roll back local changes; no nested store operations allowed.
    pub fn with_transaction<T, E>(
        &self,
        operation: impl FnOnce(&mut AccountSessionTransaction<'_>) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<AccountSessionError>,
    {
        let mut outcome = None;
        let result = self.secrets.with_entry_transaction(&self.auth_slot, |raw| {
            let mut transaction = AccountSessionTransaction {
                raw,
                slot: &self.auth_slot,
                api_base: &self.api_base,
            };
            match operation(&mut transaction) {
                Ok(value) => {
                    outcome = Some(Ok(value));
                    Ok(())
                }
                Err(error) => {
                    outcome = Some(Err(error));
                    Err(SecretsError::Keyring("Account transaction aborted".into()))
                }
            }
        });
        match outcome {
            Some(Err(error)) => Err(error),
            Some(Ok(value)) => {
                result.map_err(AccountSessionError::from).map_err(E::from)?;
                Ok(value)
            }
            None => {
                result.map_err(AccountSessionError::from).map_err(E::from)?;
                Err(E::from(AccountSessionError::InvalidCredentials))
            }
        }
    }

    /// Validate and save an account bundle in the selected secure-store slot.
    pub fn save(&self, bundle: AccountAuthBundle) -> Result<(), AccountSessionError> {
        validate_account_auth_bundle(&bundle)?;
        self.with_transaction(|transaction| transaction.replace(bundle))
    }

    /// Remove only the selected profile/origin account session.
    pub fn clear(&self) -> Result<(), AccountSessionError> {
        self.with_transaction(|transaction| {
            transaction.clear();
            Ok(())
        })
    }

    /// Read a token-free runtime receipt at a caller-supplied clock instant.
    pub fn runtime_info_at(
        &self,
        now: DateTime<Utc>,
    ) -> Result<RuntimeAccountInfo, AccountSessionError> {
        self.snapshot()?.runtime_info_at(now)
    }
}

/// Select the approved account-session backend shared by CLI, TUI, and Runtime.
///
/// Account sessions live in the private `0600` Codewhale secrets file. The OS
/// keyring is not used: on macOS every unsigned or rebuilt `codewhale` binary
/// is a new Keychain ACL principal, so reading `codewhale-cloud-auth-v1-*`
/// under the legacy `deepseek` service pops a password dialog on every start.
pub fn secure_account_session_secrets() -> Result<Secrets, AccountSessionError> {
    Ok(Secrets::file_backed())
}

/// Normalize an optional CLI/TUI profile to the durable account slot label.
#[must_use]
pub fn normalize_account_profile(profile: Option<&str>) -> String {
    profile
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default")
        .to_string()
}

/// Derive the opaque secure-store slot for a profile and account API origin.
#[must_use]
pub fn account_auth_slot(profile: &str, api_base: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(profile.as_bytes());
    digest.update([0]);
    digest.update(api_base.as_bytes());
    let digest = digest.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    format!("codewhale-cloud-auth-v1-{encoded}")
}

/// Validate the credential-bearing portion of an account response or record.
pub fn validate_account_auth_bundle(bundle: &AccountAuthBundle) -> Result<(), AccountSessionError> {
    if !bundle.token_type.eq_ignore_ascii_case("bearer")
        || bundle.access_token.trim().is_empty()
        || bundle.refresh_token.trim().is_empty()
        || bundle.access_token.len() > MAX_TOKEN_BYTES
        || bundle.refresh_token.len() > MAX_TOKEN_BYTES
        || bundle
            .access_token
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        || bundle
            .refresh_token
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(AccountSessionError::InvalidCredentials);
    }
    Ok(())
}

fn runtime_account_info_from_stored(
    stored: StoredAccountAuth,
    now: DateTime<Utc>,
) -> RuntimeAccountInfo {
    let account_id = stored
        .bundle
        .user
        .as_ref()
        .map(|user| user.id.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let session = stored.bundle.session.as_ref();
    let session_id = session
        .map(|session| session.id.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let expires_at = session
        .map(|session| session.expires_at.trim())
        .filter(|value| parse_rfc3339(value).is_some())
        .map(str::to_string);
    let scopes = normalized_scopes(session.map_or(&[], |session| &session.scopes));
    let state = session.map_or(AccountSessionState::OfflineCached, |session| {
        classify_session_state(session, now)
    });
    RuntimeAccountInfo {
        schema_version: ACCOUNT_SESSION_SCHEMA_VERSION,
        state,
        api_base: stored.api_base,
        account_id,
        session_id,
        scopes,
        expires_at,
    }
}

fn classify_session_state(session: &AccountSession, now: DateTime<Utc>) -> AccountSessionState {
    let explicit = session.status.trim().to_ascii_lowercase();
    if explicit == "revoked" || !session.revoked_at.trim().is_empty() {
        return AccountSessionState::Revoked;
    }
    if explicit == "expired"
        || parse_rfc3339(&session.refresh_expires_at).is_some_and(|expiry| expiry <= now)
    {
        return AccountSessionState::Expired;
    }
    if explicit == "offline_cached"
        || parse_rfc3339(&session.expires_at).is_some_and(|expiry| expiry <= now)
    {
        return AccountSessionState::OfflineCached;
    }
    AccountSessionState::Authenticated
}

fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn normalized_scopes(scopes: &[String]) -> Vec<String> {
    let mut scopes = scopes
        .iter()
        .map(|scope| scope.trim())
        .filter(|scope| {
            !scope.is_empty()
                && scope.len() <= MAX_SCOPE_BYTES
                && scope.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b'_' | b'-' | b'/')
                })
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    scopes.sort();
    scopes.dedup();
    scopes.truncate(MAX_SCOPES);
    scopes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::InMemoryKeyringStore;

    /// Account sessions must never touch the OS keyring. Unsigned rebuilds
    /// on macOS otherwise prompt on every `codewhale web` / TUI start.
    #[test]
    fn session_secrets_use_the_file_store_not_keychain() {
        let _lock = crate::tests::env_lock();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let home = dir.path().join("codewhale-home");
        std::fs::create_dir_all(&home).expect("home");
        unsafe { std::env::set_var("CODEWHALE_HOME", &home) };
        unsafe { std::env::remove_var("CODEWHALE_SECRET_BACKEND") };
        unsafe { std::env::remove_var("DEEPSEEK_SECRET_BACKEND") };
        let secrets =
            secure_account_session_secrets().expect("session secrets must resolve without opt-in");
        let name = secrets.backend_name();
        assert!(
            name.to_lowercase().contains("file"),
            "account sessions must not use Keychain: {name}"
        );
        assert!(!name.to_lowercase().contains("keychain"));
        assert!(!name.to_lowercase().contains("keyring"));
    }

    fn auth(
        account_id: &str,
        session_id: &str,
        expires_at: &str,
        refresh_expires_at: &str,
    ) -> AccountAuthBundle {
        AccountAuthBundle {
            token_type: "Bearer".to_string(),
            access_token: "access-never-serialize".to_string(),
            refresh_token: "refresh-never-serialize".to_string(),
            session: Some(AccountSession {
                id: session_id.to_string(),
                scopes: vec!["identity:read".to_string(), "session:sync".to_string()],
                expires_at: expires_at.to_string(),
                refresh_expires_at: refresh_expires_at.to_string(),
                ..AccountSession::default()
            }),
            user: Some(AccountUser {
                id: account_id.to_string(),
                email: "private@example.test".to_string(),
                ..AccountUser::default()
            }),
        }
    }

    fn test_store() -> (Secrets, Arc<InMemoryKeyringStore>) {
        let store = Arc::new(InMemoryKeyringStore::new());
        (Secrets::new(store.clone()), store)
    }

    #[test]
    fn runtime_receipt_is_token_free_and_preserves_only_explicit_scopes() {
        let (secrets, _) = test_store();
        let store = AccountSessionStore::new(secrets, Some("work"), "https://api.codewhale.net");
        store
            .save(auth(
                "acct-1",
                "session-1",
                "2030-01-01T00:00:00Z",
                "2031-01-01T00:00:00Z",
            ))
            .unwrap();

        let info = store.runtime_info_at(Utc::now()).unwrap();
        assert_eq!(info.state, AccountSessionState::Authenticated);
        assert_eq!(info.account_id.as_deref(), Some("acct-1"));
        assert_eq!(info.session_id.as_deref(), Some("session-1"));
        assert_eq!(info.scopes, ["identity:read", "session:sync"]);
        let json = serde_json::to_string(&info).unwrap();
        for secret in [
            "access-never-serialize",
            "refresh-never-serialize",
            "private@example.test",
        ] {
            assert!(!json.contains(secret));
        }
    }

    #[test]
    fn profiles_and_origins_preserve_same_account_and_cross_account_isolation() {
        let (secrets, _) = test_store();
        let default = AccountSessionStore::new(secrets.clone(), None, "https://api.codewhale.net");
        let work =
            AccountSessionStore::new(secrets.clone(), Some("work"), "https://api.codewhale.net");
        let local = AccountSessionStore::new(secrets, None, "http://127.0.0.1:8787");
        default.save(auth("acct-a", "session-a", "", "")).unwrap();
        work.save(auth("acct-a", "session-b", "", "")).unwrap();
        local.save(auth("acct-b", "session-c", "", "")).unwrap();

        let now = Utc::now();
        assert_eq!(
            default.runtime_info_at(now).unwrap().account_id.as_deref(),
            Some("acct-a")
        );
        assert_eq!(
            work.runtime_info_at(now).unwrap().account_id.as_deref(),
            Some("acct-a")
        );
        assert_eq!(
            local.runtime_info_at(now).unwrap().account_id.as_deref(),
            Some("acct-b")
        );
        default.clear().unwrap();
        assert_eq!(
            default.runtime_info_at(now).unwrap().state,
            AccountSessionState::SignedOut
        );
        assert_eq!(
            work.runtime_info_at(now).unwrap().state,
            AccountSessionState::Authenticated
        );
    }

    #[test]
    fn signed_out_expired_offline_and_revoked_states_are_distinct() {
        let (secrets, _) = test_store();
        let store = AccountSessionStore::new(secrets, None, DEFAULT_ACCOUNT_API_BASE);
        let now = DateTime::parse_from_rfc3339("2029-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            store.runtime_info_at(now).unwrap().state,
            AccountSessionState::SignedOut
        );

        store
            .save(auth(
                "acct",
                "offline",
                "2028-12-31T23:59:59Z",
                "2029-12-31T23:59:59Z",
            ))
            .unwrap();
        assert_eq!(
            store.runtime_info_at(now).unwrap().state,
            AccountSessionState::OfflineCached
        );

        store
            .save(auth(
                "acct",
                "expired",
                "2028-12-31T23:59:59Z",
                "2028-12-31T23:59:59Z",
            ))
            .unwrap();
        assert_eq!(
            store.runtime_info_at(now).unwrap().state,
            AccountSessionState::Expired
        );

        let mut revoked = auth("acct", "revoked", "2030-01-01T00:00:00Z", "");
        revoked.session.as_mut().unwrap().status = "revoked".to_string();
        store.save(revoked).unwrap();
        assert_eq!(
            store.runtime_info_at(now).unwrap().state,
            AccountSessionState::Revoked
        );
    }
}

/// Keep device custody tied to the account session, inside the file store's
/// existing transaction. Refreshing access tokens within that owner preserves it.
pub(crate) fn invalidate_device_companion(
    entries: &mut std::collections::HashMap<String, String>,
    slot: &str,
    replacement: Option<&str>,
) {
    let Some(suffix) = slot.strip_prefix("codewhale-cloud-auth-v1-") else {
        return;
    };
    if suffix.len() != 64 || !suffix.bytes().all(|c| c.is_ascii_hexdigit()) {
        return;
    }
    fn identity(raw: &str) -> Option<(String, String, String)> {
        let stored: StoredAccountAuth = serde_json::from_str(raw).ok()?;
        if stored.schema_version != ACCOUNT_SESSION_SCHEMA_VERSION {
            return None;
        }
        let account = stored.bundle.user?.id;
        let session = stored.bundle.session?.id;
        if stored.api_base.is_empty() || account.is_empty() || session.is_empty() {
            return None;
        }
        Some((stored.api_base, account, session))
    }
    let old = entries.get(slot).and_then(|raw| identity(raw));
    let next = replacement.and_then(identity);
    if old.is_none() || old != next {
        entries.remove(&format!("codewhale-cloud-device-v1-{suffix}"));
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use std::sync::Arc;
    fn bundle(id: &str) -> AccountAuthBundle {
        AccountAuthBundle {
            token_type: "Bearer".into(),
            access_token: format!("access-{id}"),
            refresh_token: format!("refresh-{id}"),
            session: Some(AccountSession {
                id: id.into(),
                expires_at: "2099-01-01T00:00:00Z".into(),
                refresh_expires_at: "2100-01-01T00:00:00Z".into(),
                ..Default::default()
            }),
            user: Some(AccountUser {
                id: id.into(),
                ..Default::default()
            }),
        }
    }
    #[test]
    fn exact_revision_cas_rejects_late_replace_and_clear_on_file_and_memory() {
        let dir = tempfile::tempdir().unwrap();
        for secrets in [
            Secrets::new(Arc::new(crate::InMemoryKeyringStore::new())),
            Secrets::new(Arc::new(crate::FileKeyringStore::new(
                dir.path().join("secrets.json"),
            ))),
        ] {
            let store = AccountSessionStore::new(secrets, None, DEFAULT_ACCOUNT_API_BASE);
            let pristine = store.snapshot().unwrap();
            assert!(pristine.is_pristine());
            let first_login = store.begin_login().unwrap();
            assert!(first_login.load().unwrap().is_none());
            store.clear().unwrap();
            let signed_out = store.snapshot().unwrap();
            assert!(!signed_out.is_pristine());
            assert_ne!(pristine.fingerprint(), signed_out.fingerprint());
            assert_ne!(first_login.fingerprint(), signed_out.fingerprint());
            assert!(
                store
                    .save_if_unchanged(&pristine, bundle("late-pristine-login"))
                    .unwrap()
                    .is_none()
            );
            assert!(
                store
                    .save_if_unchanged(&first_login, bundle("late-claimed-login"))
                    .unwrap()
                    .is_none()
            );
            store.save(bundle("old")).unwrap();
            store
                .with_transaction(|transaction| {
                    transaction.set_device_access(AccountDeviceAccess {
                        device_id: uuid::Uuid::new_v4().to_string(),
                        credential: format!("cwc_key_{}_{}", "b".repeat(24), "A".repeat(43)),
                        api_key_id: "b".repeat(24),
                        account_id: "old".into(),
                        session_id: "old".into(),
                        expires_at: Utc::now().timestamp() + 3600,
                    })
                })
                .unwrap();
            store.save(bundle("old")).unwrap();
            assert!(store.snapshot().unwrap().device_access().unwrap().is_some());
            let old = store.snapshot().unwrap();
            let other = store.clone();
            std::thread::spawn(move || other.save(bundle("new")))
                .join()
                .unwrap()
                .unwrap();
            assert!(
                store
                    .save_if_unchanged(&old, bundle("stale-refresh"))
                    .unwrap()
                    .is_none()
            );
            assert!(!store.clear_if_unchanged(&old).unwrap());
            assert_eq!(
                store.load().unwrap().unwrap().bundle.user.unwrap().id,
                "new"
            );
            assert!(store.snapshot().unwrap().device_access().unwrap().is_none());
            let latest = store.snapshot().unwrap();
            assert!(store.clear_if_unchanged(&latest).unwrap());
            assert!(store.load().unwrap().is_none());
            assert!(store.snapshot().unwrap().device_access().unwrap().is_none());
        }
    }
    #[test]
    fn snapshot_compares_original_unknown_fields_and_scopes_store_identity() {
        let secrets = Secrets::new(Arc::new(crate::InMemoryKeyringStore::new()));
        let store = AccountSessionStore::new(secrets.clone(), None, DEFAULT_ACCOUNT_API_BASE);
        store.save(bundle("account")).unwrap();
        let slot = account_auth_slot("default", DEFAULT_ACCOUNT_API_BASE);
        let mut value: serde_json::Value =
            serde_json::from_str(&secrets.get(&slot).unwrap().unwrap()).unwrap();
        value["futureField"] = true.into();
        secrets.set(&slot, &value.to_string()).unwrap();
        let snapshot = store.snapshot().unwrap();
        let other =
            AccountSessionStore::new(secrets.clone(), Some("other"), DEFAULT_ACCOUNT_API_BASE);
        assert!(other.clear_if_unchanged(&snapshot).is_err());
        assert!(
            store
                .save_if_unchanged(&snapshot, bundle("account"))
                .unwrap()
                .is_some()
        );
        secrets.set(&slot, "malformed").unwrap();
        let corrupt = store.snapshot().unwrap();
        assert!(corrupt.load().is_err());
        assert!(store.clear_if_unchanged(&corrupt).unwrap());
    }
}
