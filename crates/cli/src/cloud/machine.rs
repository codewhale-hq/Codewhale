//! Codewhale-account machine tokens (`CODEWHALE_API_KEY`).
//!
//! This is the CI path. A machine token authenticates as the account with no
//! local session file and no browser, and it is deliberately weaker than the
//! interactive session: it may call exactly two read-only routes and can never
//! mint, list, or revoke a key. Those asymmetries are enforced by the control
//! plane; this module refuses to blur them locally so a CI failure reads as a
//! credential problem, a configuration problem, or a CLI bug — never as an
//! ambiguous 403.
//!
//! Two rules are load-bearing and are held by tests below:
//!
//! * The token value never leaves this module. It is written to no file, no
//!   log, and no diagnostic. Everything that identifies a key in output is the
//!   32-character non-secret head (`cwc_key_` + the 24-hex id), which is
//!   exactly what an operator needs to match a leaked key to a listing row.
//! * A machine credential never silently downgrades to the interactive
//!   session. Falling back would run CI as the wrong identity.

use std::fmt;
use std::io::Write;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use codewhale_config::ProviderKind;
use codewhale_config::route::parse_route_kind;
use serde::{Deserialize, Serialize};

use super::{
    CloudClient, CloudRequest, CloudResponse, CloudTransport, HttpMethod, printable,
    validate_api_base,
};

/// The only environment variable that carries a machine token.
pub(crate) const MACHINE_KEY_ENV: &str = "CODEWHALE_API_KEY";
/// Machine-token API origin override, checked before the device-flow origin.
pub(crate) const MACHINE_API_BASE_ENV: &str = "CODEWHALE_API_BASE";

/// Fixed literal no other Codewhale credential uses, so `grep -r cwc_key_`
/// finds every leaked key.
const TOKEN_PREFIX: &str = "cwc_key_";
/// `cwc_key_` + 24 hex + `_` + 43 base64url.
const TOKEN_LEN: usize = 76;
/// `cwc_key_` + the 24-hex key id. Non-secret by design: it is the display
/// prefix a listing shows, so printing it is how an operator maps a token in a
/// build log to the one row they must revoke.
const TOKEN_HEAD_LEN: usize = 32;
const KEY_ID_LEN: usize = 24;

/// The closed scope set. Widening it is a control-plane change, not a CLI one.
///
/// `models:infer` is what lets a key reach the Codewhale API's model routes
/// (`/v1/models`, `/v1/chat/completions`, `/v1/messages`); the other two stay
/// the account and agent read/run scopes.
const SCOPES: [&str; 3] = ["account:read", "agent:run", "models:infer"];

/// Per-account ceiling quoted by `api_key_limit_reached`.
const MAX_LIVE_KEYS: usize = 25;

const MAX_KEY_NAME_CHARS: usize = 64;
const MAX_EXPIRY_DAYS: u32 = 365;

/// Exit codes. CI logs must be able to tell a bad credential from a missing
/// model without parsing English, so the classes are distinct integers.
pub(crate) const EXIT_INPUT: i32 = 2;
pub(crate) const EXIT_AUTH: i32 = 3;
pub(crate) const EXIT_AGENT_UNCONFIGURED: i32 = 4;
pub(crate) const EXIT_LIMIT: i32 = 5;
pub(crate) const EXIT_UNAVAILABLE: i32 = 6;
pub(crate) const EXIT_TRANSPORT: i32 = 7;

/// Attempts for an idempotent request, including the first.
const MAX_ATTEMPTS: u32 = 3;
const BASE_BACKOFF_MS: u64 = 500;
const MAX_BACKOFF_MS: u64 = 30_000;

// ---------------------------------------------------------------------------
// Token
// ---------------------------------------------------------------------------

/// A validated machine token.
///
/// No `Display`, and `Debug` prints only the non-secret head, so the value
/// cannot reach a panic message or a `{:?}` dump by accident.
#[derive(Clone)]
pub(crate) struct MachineKey(String);

impl MachineKey {
    /// Validate a raw environment value without sending it anywhere.
    ///
    /// A malformed value is almost always a truncated or shell-mangled paste.
    /// Saying so locally is strictly more useful than a server 401, which
    /// cannot distinguish "you pasted half a key" from "this key was deleted".
    pub(crate) fn parse(raw: &str) -> Result<Self> {
        let value = unwrap_quoted(raw);
        if !token_is_well_formed(value) {
            bail!(
                "{MACHINE_KEY_ENV} is not a well-formed Codewhale API key, so it was not sent. \
Expected {TOKEN_LEN} characters shaped `cwc_key_<24 hex>_<43 chars>`; got {} characters. \
That is almost always a truncated or shell-mangled paste — re-copy the value, or create a \
new key with `codewhale account api-keys create`.",
                value.chars().count()
            );
        }
        Ok(Self(value.to_string()))
    }

    /// The non-secret 32-character head: `cwc_key_` plus the 24-hex key id.
    #[must_use]
    pub(crate) fn head(&self) -> &str {
        &self.0[..TOKEN_HEAD_LEN]
    }

    /// Hand the full token to the transport. The only caller is this module.
    fn expose(&self) -> String {
        self.0.clone()
    }
}

impl fmt::Debug for MachineKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Head only. A machine token must survive being logged.
        write!(formatter, "MachineKey({}…)", self.head())
    }
}

/// Trim whitespace and one wrapping pair of quotes. CI secret pasting adds
/// both, and neither is part of the credential.
fn unwrap_quoted(raw: &str) -> &str {
    let trimmed = raw.trim();
    for quote in ['"', '\''] {
        if trimmed.len() >= 2 && trimmed.starts_with(quote) && trimmed.ends_with(quote) {
            return trimmed[1..trimmed.len() - 1].trim();
        }
    }
    trimmed
}

/// `^cwc_key_[0-9a-f]{24}_[A-Za-z0-9_-]{43}$`, hand-rolled to avoid a regex
/// dependency in a credential path.
fn token_is_well_formed(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != TOKEN_LEN || !value.is_ascii() || !value.starts_with(TOKEN_PREFIX) {
        return false;
    }
    let id_end = TOKEN_PREFIX.len() + KEY_ID_LEN;
    if bytes[TOKEN_PREFIX.len()..id_end]
        .iter()
        .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(byte))
    {
        return false;
    }
    if bytes[id_end] != b'_' {
        return false;
    }
    bytes[id_end + 1..]
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// Deferred read of `CODEWHALE_API_KEY`.
///
/// Presence and validity are separate questions: a management command must
/// refuse locally when a key is *present* (rule: a key cannot manage keys)
/// without first demanding that the key be well formed.
#[derive(Clone, Default)]
pub(crate) struct MachineKeyEnv {
    raw: Option<String>,
}

impl MachineKeyEnv {
    /// Read the process environment.
    #[must_use]
    pub(crate) fn from_process_env() -> Self {
        Self::from_raw(std::env::var(MACHINE_KEY_ENV).ok().as_deref())
    }

    /// Build from an explicit value. Empty and whitespace-only mean "unset";
    /// an exported-but-empty CI secret is an unset secret, not a bad one.
    #[must_use]
    pub(crate) fn from_raw(raw: Option<&str>) -> Self {
        Self {
            raw: raw
                .map(str::to_string)
                .filter(|value| !value.trim().is_empty()),
        }
    }

    /// Whether a machine credential is present at all, valid or not.
    #[must_use]
    pub(crate) fn is_present(&self) -> bool {
        self.raw.is_some()
    }

    /// Validate the key if one is set.
    pub(crate) fn resolve(&self) -> Result<Option<MachineKey>> {
        self.raw.as_deref().map(MachineKey::parse).transpose()
    }

    /// Validate the key, requiring one to be set.
    pub(crate) fn require(&self) -> Result<MachineKey> {
        self.resolve()?.ok_or_else(|| {
            anyhow!(
                "This command authenticates with a Codewhale account API key. \
Set {MACHINE_KEY_ENV}, or create one with `codewhale account api-keys create` after \
`codewhale login`."
            )
        })
    }
}

// ---------------------------------------------------------------------------
// Base URL
// ---------------------------------------------------------------------------

/// Resolve the account API origin.
///
/// Order: explicit `--api-base`, then `CODEWHALE_API_BASE`, then whatever the
/// device flow already uses, then the production default. The flag outranks
/// the variable for the same reason `--api-key` would outrank the environment:
/// the nearer, more deliberate signal wins.
pub(crate) fn resolve_api_base(
    flag: Option<&str>,
    machine_base: Option<&str>,
    device_base: Option<&str>,
    default_base: &str,
) -> String {
    [flag, machine_base, device_base]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map_or_else(
            || default_base.to_string(),
            |value| value.trim_end_matches('/').to_string(),
        )
}

/// Reject a plaintext origin for a non-loopback host.
///
/// A machine token is a bearer credential with no replay protection, so
/// sending it over cleartext to a remote host is a hard error rather than a
/// warning: a warning in CI is a line nobody reads.
pub(crate) fn require_secure_base(api_base: &str) -> Result<()> {
    validate_api_base(api_base)
        .map(|_| ())
        .with_context(|| format!("refusing to send a {MACHINE_KEY_ENV} credential to {api_base}"))
}

// ---------------------------------------------------------------------------
// Error envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct ErrorEnvelope {
    #[serde(default)]
    message: String,
    #[serde(default)]
    details: ErrorDetails,
}

#[derive(Debug, Default, Deserialize)]
struct ErrorDetails {
    #[serde(default)]
    code: String,
    #[serde(default)]
    fields: Vec<String>,
}

/// A classified control-plane failure.
///
/// `code` is read from `details.code`, never inferred from the HTTP status:
/// three different 401s and two different 403s need three and two different
/// fixes, and only the code tells them apart.
#[derive(Debug, Clone)]
pub(crate) struct MachineError {
    pub(crate) status: u16,
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) exit_code: i32,
    pub(crate) retryable: bool,
}

impl fmt::Display for MachineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MachineError {}

impl MachineError {
    fn new(status: u16, code: &str, message: impl Into<String>, exit_code: i32) -> Self {
        Self {
            status,
            code: code.to_string(),
            message: message.into(),
            exit_code,
            retryable: false,
        }
    }

    fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    /// Transport failure: no HTTP status was ever produced.
    pub(crate) fn transport(message: impl Into<String>) -> Self {
        Self::new(0, "transport_error", message, EXIT_TRANSPORT).retryable()
    }
}

/// Map a non-success response onto an actionable local error.
pub(crate) fn classify(response: &CloudResponse) -> MachineError {
    let envelope = serde_json::from_slice::<ErrorEnvelope>(&response.body).unwrap_or_default();
    let code = sanitize_code(&envelope.details.code);
    let server_message = sanitize_message(&envelope.message);
    let status = response.status;

    match code.as_str() {
        "api_key_invalid" => MachineError::new(
            status,
            &code,
            "That Codewhale API key is not valid. It may have been mistyped, truncated, or \
deleted. Create a new one with `codewhale account api-keys create`.",
            EXIT_AUTH,
        ),
        "api_key_revoked" => MachineError::new(
            status,
            &code,
            format!(
                "This Codewhale API key was revoked. Create a new one and update {MACHINE_KEY_ENV}."
            ),
            EXIT_AUTH,
        ),
        "api_key_expired" => MachineError::new(
            status,
            &code,
            format!(
                "This Codewhale API key expired. Create a new one and update {MACHINE_KEY_ENV}."
            ),
            EXIT_AUTH,
        ),
        "api_key_required" => MachineError::new(
            status,
            &code,
            "This Codewhale route accepts only an account API key, but the CLI sent an \
interactive session token. That is a Codewhale CLI bug, not a problem with your \
credentials — please report it.",
            EXIT_AUTH,
        ),
        "auth_required" => MachineError::new(
            status,
            &code,
            format!(
                "No Codewhale credential reached the server. Set {MACHINE_KEY_ENV}, or run \
`codewhale login`."
            ),
            EXIT_AUTH,
        ),
        "api_key_route_denied" => MachineError::new(
            status,
            &code,
            "A Codewhale API key cannot be used for this command. Managing API keys needs an \
interactive login. Run `codewhale login`.",
            EXIT_AUTH,
        ),
        "api_key_scope_denied" => MachineError::new(
            status,
            &code,
            join_message(
                &server_message,
                "This key does not have the scope this command needs. Create a new key with it \
using `codewhale account api-keys create --scope <scope>`.",
            ),
            EXIT_AUTH,
        ),
        "account_agent_model_unconfigured" => MachineError::new(
            status,
            &code,
            "This Codewhale account has no agent model configured. Choose one in the Codewhale \
app, or run `codewhale account keys set <provider>`.",
            EXIT_AGENT_UNCONFIGURED,
        ),
        "api_key_limit_reached" => MachineError::new(
            status,
            &code,
            format!(
                "This account already has {MAX_LIVE_KEYS} active API keys. Revoke one first with \
`codewhale account api-keys revoke <id>`."
            ),
            EXIT_LIMIT,
        ),
        "api_key_field_unknown" => {
            let fields = envelope
                .details
                .fields
                .iter()
                .filter_map(|field| {
                    let field = sanitize_code(field);
                    (!field.is_empty()).then_some(field)
                })
                .collect::<Vec<_>>();
            let detail = if fields.is_empty() {
                String::new()
            } else {
                format!("Unknown field(s): {}.", fields.join(", "))
            };
            MachineError::new(
                status,
                &code,
                join_message(&server_message, &detail),
                EXIT_INPUT,
            )
        }
        "api_key_name_invalid"
        | "api_key_expiry_invalid"
        | "api_key_scopes_invalid"
        | "api_key_body_invalid" => MachineError::new(
            status,
            &code,
            // Input errors are the server describing the request this CLI
            // built, so its own wording is the most precise thing available.
            join_message(&server_message, "Adjust the command and run it again."),
            EXIT_INPUT,
        ),
        "api_key_not_found" => MachineError::new(
            status,
            &code,
            // An unknown id, a malformed id, and another account's id are
            // deliberately indistinguishable, so revoke cannot probe for
            // foreign key ids. The message must not pretend otherwise.
            "No such Codewhale API key. Run `codewhale account api-keys list` to see the ids \
this account owns.",
            EXIT_INPUT,
        ),
        "api_key_unavailable" => MachineError::new(
            status,
            &code,
            "This Codewhale deployment does not support API keys yet. Retrying will not help; \
the deployment needs a control-plane upgrade.",
            EXIT_UNAVAILABLE,
        ),
        "control_plane_not_attached" => MachineError::new(
            status,
            &code,
            "This Codewhale edge deployment has not been attached to the account control plane, \
so the API key routes are not reachable from it. This is a routing/deployment fix, not a \
missing feature and not a bad key.",
            EXIT_UNAVAILABLE,
        ),
        _ => classify_by_status(status, &code, &server_message),
    }
}

/// Fallback when the body carried no recognizable `details.code`.
fn classify_by_status(status: u16, code: &str, server_message: &str) -> MachineError {
    let code = if code.is_empty() { "unknown" } else { code };
    let context = if server_message.is_empty() {
        format!("The Codewhale service returned HTTP {status}.")
    } else {
        format!("The Codewhale service returned HTTP {status}: {server_message}")
    };
    match status {
        401 | 403 => MachineError::new(status, code, context, EXIT_AUTH),
        429 => MachineError::new(
            status,
            code,
            format!("{context} The request was rate limited."),
            EXIT_TRANSPORT,
        )
        .retryable(),
        400 | 404 | 409 | 422 => MachineError::new(status, code, context, EXIT_INPUT),
        500..=504 => MachineError::new(status, code, context, EXIT_TRANSPORT).retryable(),
        _ => MachineError::new(status, code, context, EXIT_TRANSPORT),
    }
}

fn join_message(primary: &str, fallback: &str) -> String {
    match (primary.is_empty(), fallback.is_empty()) {
        (true, _) => fallback.to_string(),
        (false, true) => primary.to_string(),
        (false, false) => format!("{primary} {fallback}"),
    }
}

fn sanitize_code(code: &str) -> String {
    let code = code.trim();
    if code.is_empty()
        || code.len() > 80
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return String::new();
    }
    code.to_string()
}

/// Server prose is untrusted output. Strip control characters and bound it so
/// a hostile or broken body cannot rewrite the terminal.
fn sanitize_message(message: &str) -> String {
    super::vendor_safe_prose(printable(message))
}

// ---------------------------------------------------------------------------
// Retry
// ---------------------------------------------------------------------------

/// Whether a request may be replayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Retry {
    /// Safe to replay: GET, and DELETE revoke, which the server defines as
    /// idempotent (a repeat returns the identical `revokedAt`).
    Idempotent,
    /// Never replayed. `POST /api/account/api-keys` is the whole reason this
    /// variant exists: a retry that actually succeeded server-side mints a
    /// second key whose secret the caller never sees, and therefore can never
    /// revoke by id from the output they have.
    Never,
}

/// Backoff for `attempt` (1-based), honoring a server `Retry-After`.
///
/// Jitter is derived from the attempt number rather than a clock or an RNG so
/// the schedule is deterministic and can be asserted in tests.
pub(crate) fn backoff_delay(attempt: u32, retry_after_seconds: Option<u64>) -> Duration {
    if let Some(seconds) = retry_after_seconds {
        return Duration::from_millis((seconds.saturating_mul(1000)).min(MAX_BACKOFF_MS));
    }
    let exponential = BASE_BACKOFF_MS.saturating_mul(1u64 << attempt.min(6).saturating_sub(1));
    let jitter = u64::from(attempt).saturating_mul(137) % 250;
    Duration::from_millis(exponential.saturating_add(jitter).min(MAX_BACKOFF_MS))
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Non-secret account record returned beside a machine-key whoami.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MachineAccount {
    #[serde(default)]
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) display_name: String,
    #[serde(default)]
    pub(crate) email: String,
    #[serde(default)]
    pub(crate) region: String,
    #[serde(default)]
    pub(crate) plan: String,
}

/// Machine-key metadata. Deliberately has no `secret` field: the plaintext
/// exists in one create response and nowhere else, and a type that cannot
/// represent it cannot accidentally carry it into a listing or a log.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiKeyMetadata {
    #[serde(default)]
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) display_prefix: String,
    #[serde(default)]
    pub(crate) scopes: Vec<String>,
    #[serde(default)]
    pub(crate) created_at: String,
    #[serde(default)]
    pub(crate) expires_at: Option<String>,
    #[serde(default)]
    pub(crate) last_used_at: Option<String>,
    #[serde(default)]
    pub(crate) revoked_at: Option<String>,
}

/// Agent-model presence. The provider *identifier* only — a machine key can
/// cause the account's provider credential to be spent, never to be read back.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentState {
    #[serde(default)]
    pub(crate) configured: bool,
    #[serde(default)]
    pub(crate) model_provider: String,
    #[serde(default)]
    pub(crate) account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WhoamiResponse {
    #[serde(default)]
    pub(crate) account: MachineAccount,
    #[serde(default)]
    pub(crate) api_key: ApiKeyMetadata,
    #[serde(default)]
    pub(crate) agent: AgentState,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AgentResponse {
    #[serde(default)]
    pub(crate) agent: AgentState,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiKeyListResponse {
    #[serde(default)]
    api_keys: Vec<ApiKeyMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiKeyCreateResponse {
    #[serde(default)]
    api_key: ApiKeyMetadata,
    #[serde(default)]
    secret: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiKeyRevokeResponse {
    #[serde(default)]
    api_key: ApiKeyMetadata,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiKeyCreateRequest<'a> {
    name: &'a str,
    /// Absent means "never expires". Sent as absent, not null, so the body
    /// stays inside the server's closed field set.
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_in_days: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scopes: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Machine client
// ---------------------------------------------------------------------------

/// Read-only client authenticated by a machine token.
pub(crate) struct MachineClient<'a, T: CloudTransport> {
    transport: &'a T,
    key: MachineKey,
}

impl<'a, T: CloudTransport> MachineClient<'a, T> {
    pub(crate) fn new(transport: &'a T, key: MachineKey) -> Self {
        Self { transport, key }
    }

    /// The non-secret head of the key in use, for diagnostics.
    #[must_use]
    pub(crate) fn key_head(&self) -> &str {
        self.key.head()
    }

    /// `GET /api/account/api-key/whoami` — the diagnosis surface.
    pub(crate) fn whoami(&self, sleeper: &mut dyn FnMut(Duration)) -> Result<WhoamiResponse> {
        self.get_json("/api/account/api-key/whoami", sleeper)
    }

    /// `GET /api/account/api-key/agent` — the precondition for machine work.
    pub(crate) fn agent(&self, sleeper: &mut dyn FnMut(Duration)) -> Result<AgentResponse> {
        self.get_json("/api/account/api-key/agent", sleeper)
    }

    fn get_json<R: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        sleeper: &mut dyn FnMut(Duration),
    ) -> Result<R> {
        let response = send_with_retry(self.transport, Retry::Idempotent, sleeper, || {
            CloudRequest {
                method: HttpMethod::Get,
                path: path.to_string(),
                // Exactly one credential per request. The transport carries a
                // single Authorization header, so a machine key and a session
                // bearer cannot both be presented.
                bearer: Some(self.key.expose()),
                body: None,
            }
        })?;
        decode_json(response)
    }
}

/// Issue a request, retrying only what is safe to replay.
fn send_with_retry<T: CloudTransport>(
    transport: &T,
    retry: Retry,
    sleeper: &mut dyn FnMut(Duration),
    build: impl Fn() -> CloudRequest,
) -> Result<CloudResponse> {
    let max_attempts = if retry == Retry::Idempotent {
        MAX_ATTEMPTS
    } else {
        1
    };
    let mut attempt = 1;
    loop {
        let (error, retry_after) = match transport.execute(build()) {
            Ok(response) if (200..300).contains(&response.status) => return Ok(response),
            Ok(response) => {
                let retry_after = response.retry_after;
                (classify(&response), retry_after)
            }
            Err(err) => (
                MachineError::transport(format!(
                    "Could not reach the Codewhale service: {}",
                    printable(&err.to_string())
                )),
                None,
            ),
        };
        if !error.retryable || attempt >= max_attempts {
            return Err(anyhow::Error::new(error));
        }
        sleeper(backoff_delay(attempt, retry_after));
        attempt += 1;
    }
}

fn decode_json<R: serde::de::DeserializeOwned>(response: CloudResponse) -> Result<R> {
    serde_json::from_slice(&response.body)
        .context("The Codewhale service returned an invalid JSON response")
}

// ---------------------------------------------------------------------------
// Management (interactive session only)
// ---------------------------------------------------------------------------

/// `codewhale account api-keys …` — machine tokens.
///
/// Deliberately a different noun from `codewhale account keys`, which manages
/// the BYOK provider vault. Those are opposite directions of trust: a provider
/// key is what Codewhale presents *to* DeepSeek, while a machine token is what
/// a customer presents *to* Codewhale. Merging them would let one typo revoke
/// the wrong credential.
#[derive(Debug, Args)]
pub(crate) struct ApiKeysArgs {
    #[command(subcommand)]
    command: ApiKeysCommand,
}

#[derive(Debug, Subcommand)]
enum ApiKeysCommand {
    /// Mint a machine token. The secret is printed once and never again.
    Create(ApiKeyCreateArgs),
    /// List this account's machine tokens. Metadata only; never the secret.
    List,
    /// Revoke a machine token by its 24-hex id.
    Revoke {
        /// The 24-hex key id, which is also the tail of the display prefix.
        id: String,
    },
}

#[derive(Debug, Args)]
pub(crate) struct ApiKeyCreateArgs {
    /// Human label shown in listings and audit events.
    #[arg(long)]
    name: String,
    /// Optional lifetime in days. Omit for a key that never expires.
    #[arg(long = "expires-in-days", value_parser = clap::value_parser!(u32).range(1..=i64::from(MAX_EXPIRY_DAYS)))]
    expires_in_days: Option<u32>,
    /// Repeatable. Omit for all of `account:read`, `agent:run`, `models:infer`.
    #[arg(long = "scope", value_name = "SCOPE")]
    scopes: Vec<String>,
    /// Also save the new secret as this machine's local `codewhale` provider
    /// credential, so the CLI can immediately use Codewhale API models.
    ///
    /// The key never leaves this machine: it goes to the same secret store
    /// `codewhale auth` writes, and nothing is uploaded anywhere.
    #[arg(long = "use", default_value_t = false)]
    use_locally: bool,
}

/// `/^[A-Za-z0-9][A-Za-z0-9 ._:@\/-]{0,63}$/`, checked locally so a bad name
/// costs a message instead of a round trip.
pub(crate) fn validate_key_name(name: &str) -> Result<&str> {
    let invalid = || {
        anyhow!(
            "API key name must be 1-{MAX_KEY_NAME_CHARS} characters, start with a letter or \
digit, and contain only letters, digits, spaces, and `. _ : @ / -`."
        )
    };
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return Err(invalid());
    };
    if !first.is_ascii_alphanumeric() || name.chars().count() > MAX_KEY_NAME_CHARS {
        return Err(invalid());
    }
    if characters.any(|character| {
        !character.is_ascii_alphanumeric()
            && !matches!(character, ' ' | '.' | '_' | ':' | '@' | '/' | '-')
    }) {
        return Err(invalid());
    }
    Ok(name)
}

/// Normalize `--scope` into the closed set.
///
/// An omitted `--scope` means every scope, and is sent explicitly rather than
/// left to a server default: a key minted by this CLI should carry exactly the
/// scopes the CLI's own help promised, whatever the control plane's default is
/// this week.
pub(crate) fn validate_scopes(scopes: &[String]) -> Result<Option<Vec<String>>> {
    if scopes.is_empty() {
        return Ok(Some(
            SCOPES.iter().map(|scope| (*scope).to_string()).collect(),
        ));
    }
    let mut normalized = Vec::new();
    for scope in scopes {
        let scope = scope.trim();
        if !SCOPES.contains(&scope) {
            bail!(
                "unknown scope `{}`; Codewhale API keys accept only {}",
                printable(scope),
                SCOPES.join(", ")
            );
        }
        if !normalized.iter().any(|existing| existing == scope) {
            normalized.push(scope.to_string());
        }
    }
    Ok(Some(normalized))
}

/// The 24-hex key id, checked locally.
///
/// This is a paste check, not an existence check: the server answers 404
/// identically for a malformed id, an unknown id, and another account's id, so
/// nothing here can or should try to distinguish them.
pub(crate) fn validate_key_id(id: &str) -> Result<&str> {
    let id = id.trim();
    if id.len() != KEY_ID_LEN
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!(
            "API key id must be {KEY_ID_LEN} lowercase hex characters — the part after `cwc_key_` \
in the display prefix. Run `codewhale account api-keys list` to see them."
        );
    }
    Ok(id)
}

/// Refuse a management command that would be authenticated by a machine key.
///
/// The server would answer 403 `api_key_route_denied`, but a local refusal is
/// better: it never puts the credential on the wire, and it names the fix.
/// This is the load-bearing rule of the whole design — a stolen key must not
/// be able to bootstrap a successor that outlives the revocation of the key
/// that was stolen — so the CLI states it rather than discovering it.
pub(crate) fn reject_machine_key_for_management(
    machine: &MachineKeyEnv,
    has_session: bool,
) -> Result<()> {
    if machine.is_present() && !has_session {
        bail!(
            "Managing API keys needs an interactive login. Run `codewhale login`.\n\
{MACHINE_KEY_ENV} is set, but a Codewhale API key deliberately cannot create, list, or \
revoke keys — that is what stops a leaked key from minting a replacement for itself."
        );
    }
    Ok(())
}

/// Run `codewhale account api-keys …` against the interactive session.
pub(crate) fn run_api_keys<T: CloudTransport, W: Write>(
    args: ApiKeysArgs,
    client: &CloudClient<'_, T>,
    machine: &MachineKeyEnv,
    provider_secrets: &codewhale_secrets::Secrets,
    out: &mut W,
    sleeper: &mut dyn FnMut(Duration),
) -> Result<()> {
    reject_machine_key_for_management(machine, client.has_session()?)?;
    match args.command {
        ApiKeysCommand::Create(create) => {
            let name = validate_key_name(create.name.trim())?;
            let scopes = validate_scopes(&create.scopes)?;
            let body = serde_json::to_vec(&ApiKeyCreateRequest {
                name,
                expires_in_days: create.expires_in_days,
                scopes,
            })
            .context("failed to encode the Codewhale API key request")?;
            // `Retry::Never` is the whole point of the enum here: a POST that
            // actually succeeded server-side would mint a second key whose
            // one-time secret the caller never saw, and therefore can never
            // revoke by id from the output they hold.
            let response = client.execute_authenticated_with_retry(
                HttpMethod::Post,
                "/api/account/api-keys",
                Some(body),
                Retry::Never,
                sleeper,
            )?;
            if !(200..300).contains(&response.status) {
                return Err(anyhow::Error::new(classify(&response)));
            }
            let created: ApiKeyCreateResponse = decode_json(response)?;
            write_created_key(out, &created)?;
            if create.use_locally {
                save_key_as_local_codewhale_credential(provider_secrets, &created.secret, out)?;
            }
            Ok(())
        }
        ApiKeysCommand::List => {
            let response = client.execute_authenticated_with_retry(
                HttpMethod::Get,
                "/api/account/api-keys",
                None,
                Retry::Idempotent,
                sleeper,
            )?;
            if !(200..300).contains(&response.status) {
                return Err(anyhow::Error::new(classify(&response)));
            }
            let listing: ApiKeyListResponse = decode_json(response)?;
            write_key_listing(out, &listing.api_keys)
        }
        ApiKeysCommand::Revoke { id } => {
            let id = validate_key_id(&id)?;
            let path = format!("/api/account/api-keys/{id}");
            let response = client.execute_authenticated_with_retry(
                HttpMethod::Delete,
                &path,
                None,
                Retry::Idempotent,
                sleeper,
            )?;
            if !(200..300).contains(&response.status) {
                return Err(anyhow::Error::new(classify(&response)));
            }
            let revoked: ApiKeyRevokeResponse = decode_json(response)?;
            writeln!(
                out,
                "Revoked Codewhale API key {}.",
                printable(&revoked.api_key.id)
            )?;
            if let Some(revoked_at) = revoked.api_key.revoked_at.as_deref() {
                writeln!(out, "Revoked at: {}", printable(revoked_at))?;
            }
            // Re-revoking returns the identical revokedAt, so a retried CI
            // cleanup step is a no-op rather than a failure. Say so, because
            // an operator who sees the same timestamp twice should not worry.
            writeln!(
                out,
                "Revocation takes effect on the key's next request. Repeating this command is safe."
            )?;
            Ok(())
        }
    }
}

/// Print a freshly minted key.
///
/// The secret goes to stdout exactly once, whether or not stdout is a TTY: CI
/// captures stdout, and a secret written to stderr would land in a diagnostics
/// stream that is far more likely to be archived and shared.
///
/// A 2xx whose secret is missing or malformed is not a created key the caller
/// can use: it fails before anything reads "Created", naming the id (when the
/// server sent one) so the half-minted key can be revoked.
fn write_created_key<W: Write>(out: &mut W, created: &ApiKeyCreateResponse) -> Result<()> {
    let metadata = &created.api_key;
    if !token_is_well_formed(&created.secret) {
        let id = if metadata.id.trim().is_empty() {
            "(no id returned)".to_string()
        } else {
            printable(&metadata.id)
        };
        bail!(
            "Codewhale answered the API key request without a well-formed secret, so no usable \
key was created. The server may still have recorded key {id}: run `codewhale account api-keys \
list` and revoke it with `codewhale account api-keys revoke <id>`, then create another."
        );
    }
    writeln!(
        out,
        "Created Codewhale API key {}.",
        printable(&metadata.id)
    )?;
    writeln!(out, "Name: {}", printable(&metadata.name))?;
    writeln!(out, "Scopes: {}", printable(&metadata.scopes.join(", ")))?;
    writeln!(
        out,
        "Expires: {}",
        metadata
            .expires_at
            .as_deref()
            .map_or_else(|| "never".to_string(), printable)
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "-- THIS IS THE ONLY TIME YOU WILL SEE THIS SECRET ------------------"
    )?;
    writeln!(out, "{}", created.secret)?;
    writeln!(
        out,
        "-------------------------------------------------------------------"
    )?;
    writeln!(
        out,
        "Codewhale stores only a hash of it and cannot show it again. Copy it now into \
{MACHINE_KEY_ENV}. If you lose it, revoke this key and create another."
    )?;
    Ok(())
}

/// Save a freshly minted key as this machine's local `codewhale` credential.
///
/// `--use` is the one place a Codewhale API key is written to disk by this
/// surface, and it writes only locally: the same secret store `codewhale auth`
/// uses, under the `codewhale` provider's own slot. Nothing is uploaded, and
/// no other provider's credential is touched.
fn save_key_as_local_codewhale_credential<W: Write>(
    secrets: &codewhale_secrets::Secrets,
    secret: &str,
    out: &mut W,
) -> Result<()> {
    // Refuse to store a value this CLI would not accept as a key: a truncated
    // response is better caught here than as a 401 on the next model call.
    let key = MachineKey::parse(secret)?;
    let slot = ProviderKind::Codewhale.secret_store_slot();
    secrets.set(slot, secret).map_err(|error| {
        anyhow!(
            "The key was created, but saving it to the local secret store ({slot}) failed: {error}. Copy the secret above into {MACHINE_KEY_ENV} instead."
        )
    })?;
    writeln!(out)?;
    writeln!(
        out,
        "Saved {} as this machine's local `codewhale` provider credential.",
        key.head()
    )?;
    writeln!(
        out,
        "Select it with `codewhale config set provider codewhale`; models come from the account's own connected providers."
    )?;
    Ok(())
}

fn write_key_listing<W: Write>(out: &mut W, keys: &[ApiKeyMetadata]) -> Result<()> {
    if keys.is_empty() {
        writeln!(out, "This Codewhale account has no API keys.")?;
        writeln!(
            out,
            "Create one with `codewhale account api-keys create --name <name>`."
        )?;
        return Ok(());
    }
    for key in keys {
        // Revoked and expired keys stay listed so an owner can audit history.
        let state = if key.revoked_at.is_some() {
            "revoked"
        } else {
            "active"
        };
        writeln!(
            out,
            "{}  {}  [{state}]  scopes={}",
            printable(&key.id),
            printable(&key.name),
            printable(&key.scopes.join(","))
        )?;
        writeln!(out, "  prefix: {}", printable(&key.display_prefix))?;
        writeln!(out, "  created: {}", printable(&key.created_at))?;
        writeln!(
            out,
            "  expires: {}",
            key.expires_at
                .as_deref()
                .map_or_else(|| "never".to_string(), printable)
        )?;
        writeln!(
            out,
            "  last used: {}",
            key.last_used_at
                .as_deref()
                .map_or_else(|| "never".to_string(), printable)
        )?;
        if let Some(revoked_at) = key.revoked_at.as_deref() {
            writeln!(out, "  revoked: {}", printable(revoked_at))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// whoami / agent output
// ---------------------------------------------------------------------------

/// Render a machine-key whoami.
///
/// `agent.configured == false` arrives on a 200. Authentication succeeded, so
/// this prints the account and then one distinct, actionable line — a
/// diagnosis surface that failed on unrelated configuration would tell the
/// operator nothing about the credential they came here to check.
pub(crate) fn write_whoami<W: Write>(
    out: &mut W,
    who: &WhoamiResponse,
    api_base: &str,
    key_head: &str,
) -> Result<()> {
    writeln!(out, "Authenticated to Codewhale with an account API key.")?;
    writeln!(out, "Account ID: {}", printable(&who.account.id))?;
    if !who.account.display_name.trim().is_empty() {
        writeln!(out, "Name: {}", printable(&who.account.display_name))?;
    }
    if !who.account.email.trim().is_empty() {
        writeln!(out, "Email: {}", printable(&who.account.email))?;
    }
    if !who.account.region.trim().is_empty() {
        writeln!(out, "Region: {}", printable(&who.account.region))?;
    }
    if !who.account.plan.trim().is_empty() {
        writeln!(out, "Plan: {}", printable(&who.account.plan))?;
    }
    writeln!(out, "API: {api_base}")?;
    // The head is the whole key id, not a truncated fingerprint: it maps to
    // exactly one row in a listing, which is what makes revocation possible
    // from a build log.
    writeln!(out, "Key: {key_head} ({})", printable(&who.api_key.name))?;
    writeln!(
        out,
        "Key scopes: {}",
        printable(&who.api_key.scopes.join(", "))
    )?;
    if let Some(expires_at) = who.api_key.expires_at.as_deref() {
        writeln!(out, "Key expires: {}", printable(expires_at))?;
    }
    if who.agent.configured {
        writeln!(out, "Agent model: {}", printable(&who.agent.model_provider))?;
    } else {
        writeln!(
            out,
            "Agent model: not configured — this key authenticates, but agent work will refuse \
until a model is chosen in the Codewhale app or with `codewhale account keys set <provider>`."
        )?;
    }
    Ok(())
}

/// Render the agent precondition.
pub(crate) fn write_agent<W: Write>(out: &mut W, agent: &AgentState) -> Result<()> {
    writeln!(
        out,
        "Codewhale agent model: {}",
        printable(&agent.model_provider)
    )?;
    if let Some(account_id) = agent.account_id.as_deref() {
        writeln!(out, "Account ID: {}", printable(account_id))?;
    }
    writeln!(out, "This account is ready to run machine work.")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Review wiring
// ---------------------------------------------------------------------------

/// Map the account's configured provider identifier onto a local route.
///
/// `codewhale review` hard-errors when a model resolves to several configured
/// routes. When CI authenticates with a machine key the account has already
/// answered that question, so the account's own provider is the disambiguator
/// — no new flag, and no guess.
pub(crate) fn review_provider_from_agent(agent: &AgentState) -> Result<ProviderKind> {
    if !agent.configured {
        return Err(anyhow::Error::new(MachineError::new(
            409,
            "account_agent_model_unconfigured",
            "This Codewhale account has no agent model configured. Choose one in the Codewhale \
app, or run `codewhale account keys set <provider>`.",
            EXIT_AGENT_UNCONFIGURED,
        )));
    }
    let provider = agent.model_provider.trim();
    parse_route_kind(provider).ok_or_else(|| {
        anyhow!(
            "This Codewhale account is configured for agent provider `{}`, which this CLI build \
does not know. Upgrade `codewhale`, or choose a supported provider in the Codewhale app.",
            printable(provider)
        )
    })
}

#[cfg(test)]
mod tests;
