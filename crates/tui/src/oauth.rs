//! Codewhale-owned OAuth login and protected credential lifecycle.
//! ChatGPT uses its official dynamic registration and direct inference grant.
//! Providers are data rows in the parameter table below, not modules.
//!
//! External Codex CLI credentials are read only after an exact, provider-scoped
//! consent grant. Codewhale never refreshes or rewrites that external file.
//!
//! # Security
//!
//! Token values are never logged or printed. All debug representations
//! redact sensitive fields.

use std::collections::BTreeMap;
use std::io::{BufRead as _, IsTerminal, Read, Write as _};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use codewhale_config::ExternalCredentialReadGrant;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::{Config, ProviderKind};

/// OAuth token payload stored in `auth.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct AuthTokens {
    access_token: Option<String>,
}

/// Top-level structure of Codex CLI's `auth.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct CodexAuthFile {
    tokens: Option<AuthTokens>,
}

/// Resolved OAuth credentials ready for API use.
#[derive(Debug, Clone)]
pub struct CodexCredentials {
    pub access_token: String,
}

/// JWT claims subset for expiry extraction.
#[derive(Debug, Deserialize)]
struct JwtClaims {
    exp: Option<u64>,
}

/// Resolve the path to the Codex auth file.
///
/// Priority:
/// 1. `OPENAI_CODEX_AUTH_FILE` env var
/// 2. `$CODEX_HOME/auth.json`
/// 3. `~/.codex/auth.json`
pub fn auth_file_path() -> PathBuf {
    if let Ok(path) = std::env::var("OPENAI_CODEX_AUTH_FILE") {
        let p = PathBuf::from(&path);
        if !p.as_os_str().is_empty() {
            return codewhale_config::resolve_external_credential_path(&p).unwrap_or(p);
        }
    }
    let codex_home = std::env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            crate::config::effective_home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".codex")
        });
    let path = codex_home.join("auth.json");
    codewhale_config::resolve_external_credential_path(&path).unwrap_or(path)
}

/// Try to extract `exp` (epoch seconds) from a JWT without verifying
/// the signature. Returns `None` on any parse failure.
fn jwt_expiry_seconds(token: &str) -> Option<u64> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    let payload = parts[1];
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: JwtClaims = serde_json::from_slice(&decoded).ok()?;
    claims.exp
}

/// Check whether an access token is expired, with a 60-second safety margin.
fn token_is_expired(access_token: &str) -> bool {
    match jwt_expiry_seconds(access_token) {
        Some(exp) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_secs();
            // 60-second safety margin
            now + 60 >= exp
        }
        // If we can't prove freshness, fail closed. External credentials are
        // never refreshed by Codewhale.
        None => true,
    }
}

/// Load Codex credentials from the auth file.
///
/// Returns `Ok(None)` if the file doesn't exist or has no usable tokens.
/// Returns `Err` only on parse/IO errors that aren't "file not found".
fn load_credentials(grant: &ExternalCredentialReadGrant) -> Result<Option<CodexCredentials>> {
    let Some(contents) = crate::external_credentials::read_to_string(grant)? else {
        return Ok(None);
    };
    let auth: CodexAuthFile = serde_json::from_str(&contents).map_err(|_| {
        anyhow::anyhow!(
            "Codex credential file {} is not valid credential JSON",
            codewhale_config::quote_os_path(grant.path())
        )
    })?;
    let tokens = match auth.tokens {
        Some(t) => t,
        None => return Ok(None),
    };
    let access_token = match tokens.access_token {
        Some(t) if !t.trim().is_empty() => t,
        _ => return Ok(None),
    };
    Ok(Some(CodexCredentials { access_token }))
}

/// Validate only the stored OAuth file, excluding token environment
/// overrides so config-vs-env provenance remains truthful.
///
/// This consumes a grant, so it can only run for a path the user explicitly
/// consented to. Consent is not a credential (#5772): status surfaces call
/// this to find out whether the consented file *still* holds a usable token,
/// because a record that outlives its token would otherwise read as stored.
#[must_use]
#[cfg(test)]
pub fn stored_credentials_present(grant: &ExternalCredentialReadGrant) -> bool {
    load_credentials(grant)
        .ok()
        .flatten()
        .is_some_and(|credentials| !token_is_expired(&credentials.access_token))
}

/// Load read-only credentials from the exact external path authorized by
/// `grant`. Expired tokens fail with guidance; they are never refreshed.
pub fn get_credentials(grant: &ExternalCredentialReadGrant) -> Result<CodexCredentials> {
    let creds =
        load_credentials(grant)?.with_context(|| missing_auth_message(OAuthProvider::Chatgpt))?;

    // Check if the access token is still valid.
    if !token_is_expired(&creds.access_token) {
        return Ok(creds);
    }

    bail!(
        "Codex access token in {} is expired. Read-only consent never refreshes or rewrites another CLI's credentials. Use `codewhale auth chatgpt` for the official ChatGPT plan route. To inspect this legacy credential file again, renew its login with `codex login`.",
        codewhale_config::quote_os_path(grant.path())
    )
}

// ── ONE access-route flow ─────────────────────────────────────────────
// Providers are DATA, not files. Every subscription login — xAI device
// code today, ChatGPT PKCE next — runs through the parameter table below
// and the shared device-code core; per-provider OAuth modules are deleted,
// not repaired.

/// How this run reaches a provider: an OAuth login Codewhale owns, a
/// read-only import from another CLI, a pasted key, or (reserved) an ACP
/// subscription bridge. The bridge arm lands with the ACP work; until then
/// it resolves to a clear error, never a silent fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "3b-ii wires the access-route dispatch that consumes this"
)]
pub enum AccessMethod {
    /// Browser/device OAuth login whose tokens Codewhale stores and refreshes.
    OwnedOAuth(OAuthProvider),
    /// Read-only credentials owned by another CLI, behind a consent grant.
    ExternalImport(ExternalImportSource),
    /// A pasted API key. No login, no refresh, no storage beyond config.
    ApiKey,
    /// Subscription access through an ACP bridge (Antigravity, Copilot).
    /// Reserved: no producer yet.
    AcpBridge,
}

/// Providers with an OAuth login Codewhale can own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthProvider {
    Xai,
    /// No device-code producer yet; the PKCE path (3b-i(b)) constructs this.
    #[allow(
        dead_code,
        reason = "3b-i(b) wires the PKCE login that constructs this"
    )]
    Chatgpt,
    Claude,
}

impl AccessMethod {
    /// Short user-facing name for picker and status surfaces.
    #[must_use]
    #[allow(
        dead_code,
        reason = "3b-ii wires the access-route dispatch that consumes this"
    )]
    pub fn label(&self) -> &'static str {
        match self {
            AccessMethod::OwnedOAuth(OAuthProvider::Xai) => "xAI subscription",
            AccessMethod::OwnedOAuth(OAuthProvider::Chatgpt) => "ChatGPT subscription",
            AccessMethod::OwnedOAuth(OAuthProvider::Claude) => "Claude subscription",
            AccessMethod::ExternalImport(ExternalImportSource::GrokCli) => "Grok CLI import",
            AccessMethod::ExternalImport(ExternalImportSource::CodexCli) => "Codex CLI import",
            AccessMethod::ExternalImport(ExternalImportSource::Antigravity) => "Antigravity import",
            AccessMethod::ExternalImport(ExternalImportSource::Dsh) => "DSH import",
            AccessMethod::ApiKey => "API key",
            AccessMethod::AcpBridge => "ACP bridge",
        }
    }
}

/// Another CLI whose credentials can be imported read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code, reason = "3b-ii wires the import table that consumes this")]
pub enum ExternalImportSource {
    /// `~/.grok/auth.json` / `XAI_AUTH_PATH`, keyed by issuer::client-id.
    GrokCli,
    /// `~/.codex/auth.json`, `{tokens:{access_token, account_id}}`.
    CodexCli,
    /// Antigravity `state.vscdb` SQLite row, opened read-only.
    Antigravity,
    /// `$DSH_HOME/.credentials.yaml` flat mapping, `DEEPSEEK_API_KEY`.
    Dsh,
}

/// Test/dev knobs a provider reads from the environment. Data, so a new
/// provider adds rows here instead of a new module.
pub struct OAuthEnvOverrides {
    pub issuer_vars: &'static [&'static str],
    pub client_id_vars: &'static [&'static str],
    pub scope_vars: &'static [&'static str],
    pub no_browser_var: &'static str,
    /// Set (to anything) to drop [`OAuthProviderParams::account_choice_extras`]
    /// from the authorize URL, in case the issuer rejects them.
    pub no_account_prompt_var: Option<&'static str>,
}

/// Everything about one provider's OAuth login that is not logic.
pub struct OAuthProviderParams<'a> {
    /// Human name for prompts and errors: "xAI", "ChatGPT".
    pub display_name: &'a str,
    pub default_issuer: &'a str,
    pub default_client_id: &'a str,
    pub default_scopes: &'a str,
    pub env: OAuthEnvOverrides,
    /// `Some` device-authorization path under the issuer (xAI); `None`
    /// means the issuer offers no device flow and device login must fail
    /// loudly instead of guessing (ChatGPT).
    pub device_code_path: Option<&'a str>,
    /// `Some` browser authorization path under the issuer (ChatGPT PKCE);
    /// `None` means the issuer offers no browser flow and browser login
    /// fails the same loud way (xAI is device-code only).
    pub authorize_path: Option<&'a str>,
    /// Token path under the issuer.
    pub token_path: &'a str,
    /// Whether the issuer was discovered (xAI) or pinned (ChatGPT paths).
    pub discover_endpoints: bool,
    /// Seconds the device-code poll runs past the server's `expires_in`.
    pub device_poll_floor_secs: u64,
    /// Extra authorize-endpoint parameters beyond the standard OAuth set,
    /// sent verbatim so the issuer sees exactly who is calling.
    pub authorize_extras: &'a [(&'a str, &'a str)],
    /// Authorize parameters that ask the issuer to let the user choose the
    /// account instead of reusing the browser's session. Sent unless
    /// [`OAuthEnvOverrides::no_account_prompt_var`] is set.
    pub account_choice_extras: &'a [(&'a str, &'a str)],
    /// Honest client identity for issuers that require one (ChatGPT's
    /// `originator`). Never impersonate another CLI.
    pub originator: Option<&'a str>,
    /// Remote revoke path under the issuer, pinned rather than discovered:
    /// revoke must still clear local credentials when the issuer is
    /// unreachable, so a discovery fetch would only add a failure mode to a
    /// path whose contract is to clean up regardless. `None` when
    /// revocation is purely local (xAI).
    pub revoke_path: Option<&'a str>,
    /// Registered loopback redirect for browser flows.
    pub callback_path: &'a str,
    /// Loopback ports the public client registered, in preference order.
    pub loopback_ports: &'a [u16],
    /// The command that re-runs this provider's login, for error guidance.
    pub relogin_hint: &'a str,
    /// The slash command that re-runs this login inside a running session
    /// and switches that session's live client.
    pub session_login_hint: &'a str,
    /// What to tell the user when every callback port is taken.
    pub callback_conflict_hint: &'a str,
}

pub const XAI_OAUTH_PARAMS: OAuthProviderParams<'static> = OAuthProviderParams {
    display_name: "xAI",
    // Single source: the legacy module still owns these strings until its
    // activation path unifies and they move here in 3b-iii.
    default_issuer: XAI_OIDC_ISSUER,
    default_client_id: GROK_OIDC_CLIENT_ID,
    default_scopes: DEFAULT_SCOPES,
    env: OAuthEnvOverrides {
        issuer_vars: &["GROK_OIDC_ISSUER", "XAI_OIDC_ISSUER"],
        client_id_vars: &["GROK_OIDC_CLIENT_ID", "XAI_OIDC_CLIENT_ID"],
        scope_vars: &["GROK_OIDC_SCOPES", "XAI_OIDC_SCOPES"],
        no_browser_var: "CODEWHALE_XAI_OAUTH_NO_BROWSER",
        no_account_prompt_var: None,
    },
    device_code_path: Some("oauth2/device/code"),
    authorize_path: None,
    token_path: "oauth2/token",
    discover_endpoints: true,
    device_poll_floor_secs: 30,
    authorize_extras: &[],
    account_choice_extras: &[],
    originator: None,
    revoke_path: None,
    callback_path: "",
    loopback_ports: &[],
    relogin_hint: "codewhale auth xai-device",
    session_login_hint: "/auth xai-device",
    callback_conflict_hint: "",
};

pub const CLAUDE_OAUTH_PARAMS: OAuthProviderParams<'static> = OAuthProviderParams {
    display_name: "Claude",
    default_issuer: "https://console.anthropic.com",
    default_client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
    default_scopes: "org:create_api_key user:profile user:inference",
    env: OAuthEnvOverrides {
        issuer_vars: &[],
        client_id_vars: &[],
        scope_vars: &[],
        no_browser_var: "CODEWHALE_CLAUDE_OAUTH_NO_BROWSER",
        no_account_prompt_var: None,
    },
    device_code_path: None,
    authorize_path: Some("oauth/authorize"),
    token_path: "v1/oauth/token",
    discover_endpoints: false,
    device_poll_floor_secs: 0,
    authorize_extras: &[],
    account_choice_extras: &[],
    originator: None,
    revoke_path: None,
    callback_path: "/oauth/code/callback",
    loopback_ports: &[],
    relogin_hint: "codewhale auth claude",
    session_login_hint: "/auth claude",
    callback_conflict_hint: "",
};

pub const CHATGPT_OAUTH_PARAMS: OAuthProviderParams<'static> = OAuthProviderParams {
    display_name: "ChatGPT",
    // Single source: same arrangement as the xAI row above.
    default_issuer: CHATGPT_OAUTH_ISSUER,
    default_client_id: CHATGPT_OAUTH_CLIENT_ID,
    default_scopes: CHATGPT_OAUTH_SCOPE,
    originator: None,
    env: OAuthEnvOverrides {
        issuer_vars: &["CODEWHALE_CHATGPT_OAUTH_ISSUER"],
        client_id_vars: &["CODEWHALE_CHATGPT_OAUTH_CLIENT_ID"],
        scope_vars: &[],
        no_browser_var: "CODEWHALE_CHATGPT_OAUTH_NO_BROWSER",
        no_account_prompt_var: Some("CODEWHALE_CHATGPT_OAUTH_NO_PROMPT"),
    },
    device_code_path: None,
    authorize_path: Some("api/accounts/authorize"),
    token_path: "api/accounts/oauth/token",
    discover_endpoints: false,
    device_poll_floor_secs: 30,
    authorize_extras: &[("resource", CHATGPT_OAUTH_RESOURCE)],
    // `prompt=login` (OIDC Core 1.0 §3.1.2.1) asks the issuer to re-prompt
    // instead of silently reusing whichever ChatGPT account the browser is
    // already signed into. Returning registrations still require the same
    // verified account; a new account explicitly starts dynamic registration.
    // Every browser login here is user-initiated; refresh
    // never visits the authorize endpoint. CODEWHALE_CHATGPT_OAUTH_NO_PROMPT
    // drops it should the issuer ever refuse it.
    account_choice_extras: &[("prompt", "login")],
    revoke_path: Some("api/accounts/oauth/revoke"),
    callback_path: "/auth/callback",
    loopback_ports: &[1455, 1457, 0],
    relogin_hint: "codewhale auth chatgpt",
    session_login_hint: "/auth chatgpt",
    callback_conflict_hint: "Close the process holding the callback port and retry `codewhale auth chatgpt`.",
};

/// The parameter table. A provider login looks its row up here; adding a
/// provider means adding a row, never a module.
#[must_use]
pub fn oauth_provider_params(provider: OAuthProvider) -> &'static OAuthProviderParams<'static> {
    match provider {
        OAuthProvider::Xai => &XAI_OAUTH_PARAMS,
        OAuthProvider::Chatgpt => &CHATGPT_OAUTH_PARAMS,
        OAuthProvider::Claude => &CLAUDE_OAUTH_PARAMS,
    }
}

/// OrcaRouter's public authentication origin. Inference lives on a different
/// host (`https://api.orcarouter.ai/v1`); neither origin is derived from the
/// other.
pub const ORCAROUTER_AUTH_BASE: &str = "https://www.orcarouter.ai";
/// OrcaRouter's public inference/catalog origin.
pub const ORCAROUTER_API_BASE: &str = "https://api.orcarouter.ai/v1";
/// OrcaRouter has no client registration step: the public client id is a
/// constant label, not a secret. PKCE — not a client secret — binds the auth
/// code to this process.
pub const ORCAROUTER_CLIENT_ID: &str = "codewhale";
/// Shown on the OrcaRouter consent screen.
pub const ORCAROUTER_APP_NAME: &str = "Codewhale";
/// Consent endpoint path. Not an API route: the browser is pointed at it.
pub const ORCAROUTER_AUTHORIZE_PATH: &str = "auth";
/// Code-for-key exchange path under the **auth** origin. The relay's
/// `/v1/auth/keys` is a different route and a 404; the auth API lives at
/// `/api/v1/auth/keys`.
pub const ORCAROUTER_EXCHANGE_PATH: &str = "/api/v1/auth/keys";
/// The scope this client asks for, and the only scope it accepts back.
pub const ORCAROUTER_SCOPE: &str = "api";

pub const ORCAROUTER_OAUTH_PARAMS: OAuthProviderParams<'static> = OAuthProviderParams {
    display_name: "OrcaRouter",
    default_issuer: ORCAROUTER_AUTH_BASE,
    default_client_id: ORCAROUTER_CLIENT_ID,
    default_scopes: ORCAROUTER_SCOPE,
    env: OAuthEnvOverrides {
        // Explicit overrides win over the shared fallback, which wins over the
        // public default. See [`resolve_orcarouter_auth_base`].
        issuer_vars: &[
            "ORCA_AUTH_BASE_URL",
            "ORCA_BASE_URL",
            "ORCAROUTER_AUTH_BASE_URL",
        ],
        client_id_vars: &["ORCAROUTER_OAUTH_CLIENT_ID"],
        scope_vars: &["ORCAROUTER_OAUTH_SCOPE"],
        no_browser_var: "CODEWHALE_ORCAROUTER_OAUTH_NO_BROWSER",
        no_account_prompt_var: None,
    },
    device_code_path: None,
    // Empty so the generic builder emits no `client_id`/`redirect_uri`;
    // OrcaRouter's authorize contract is built by
    // [`build_orcarouter_authorize_url`] instead.
    authorize_path: Some(""),
    // Unused: the exchange is JSON-formatted by
    // [`exchange_orcarouter_code`]. Kept empty so a future generic caller
    // cannot silently hit the wrong path.
    token_path: "",
    discover_endpoints: false,
    device_poll_floor_secs: 30,
    authorize_extras: &[],
    account_choice_extras: &[],
    originator: None,
    revoke_path: None,
    callback_path: "/callback",
    // OrcaRouter validates `callback_url` per request and accepts any
    // loopback port, so ask the OS for a free one rather than guessing.
    loopback_ports: &[0],
    relogin_hint: "codewhale auth orcarouter",
    session_login_hint: "/auth orcarouter",
    callback_conflict_hint: "Close the process holding the OrcaRouter callback port and retry `codewhale auth orcarouter`.",
};

/// Resolve the OrcaRouter **authentication** origin.
///
/// Precedence: an explicit auth override, then the shared self-hosted
/// fallback, then the public default. The inference origin is never derived
/// from this value.
#[must_use]
pub fn resolve_orcarouter_auth_base() -> String {
    let first_set = |vars: &[&str]| {
        vars.iter()
            .filter_map(|var| std::env::var(var).ok())
            .find(|value| !value.trim().is_empty())
    };
    let params = &ORCAROUTER_OAUTH_PARAMS;
    first_set(params.env.issuer_vars).unwrap_or_else(|| ORCAROUTER_AUTH_BASE.to_string())
}

/// Resolve the OrcaRouter **inference/catalog** origin.
///
/// Precedence: an explicit API override, then the shared self-hosted
/// fallback, then the public default. The auth origin is never derived from
/// this value.
#[must_use]
pub fn resolve_orcarouter_api_base() -> String {
    let first_set = |vars: &[&str]| {
        vars.iter()
            .filter_map(|var| std::env::var(var).ok())
            .find(|value| !value.trim().is_empty())
    };
    first_set(&["ORCA_API_BASE_URL", "ORCA_BASE_URL", "ORCAROUTER_BASE_URL"])
        .unwrap_or_else(|| ORCAROUTER_API_BASE.to_string())
}

/// The credential the rest of Codewhale consumes for OrcaRouter.
///
/// Both authentication adapters produce **this same value**: the hand-typed
/// API-key adapter wraps the pasted `sk-orca-…` string, and the PKCE adapter
/// wraps the key the exchange returned. Downstream code — the secret-store
/// write, the route binding, model discovery, and every AI entry point — must
/// read only this type and never branch on how the key was obtained.
///
/// No `Debug`: the key must not reach a log, an error, or a snapshot.
#[derive(Clone)]
pub struct OrcaCredential {
    key: String,
    /// The scope OrcaRouter actually granted, read back from the response.
    /// Never the scope this client requested.
    granted_scope: String,
    source: OrcaCredentialSource,
}

/// How an [`OrcaCredential`] was obtained. Presentation only: it selects copy,
/// never behaviour. Nothing downstream of the credential seam may read it to
/// decide whether the key is usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrcaCredentialSource {
    /// Pasted by the user through the API-key path.
    ApiKey,
    /// Issued by the OrcaRouter PKCE exchange.
    Pkce,
}

impl OrcaCredentialSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::Pkce => "pkce",
        }
    }
}

impl OrcaCredential {
    /// The API-key adapter: a key the user already holds.
    ///
    /// Prefix checking is deliberately only a shape check — an `sk-orca-`
    /// prefix is not proof the credential is valid, and no billing request is
    /// sent from a settings form to find out.
    pub fn from_api_key(raw: &str) -> Result<Self> {
        let key = codewhale_secrets::normalize_api_key(raw);
        anyhow::ensure!(!key.is_empty(), "OrcaRouter API key must not be empty");
        anyhow::ensure!(
            key.starts_with("sk-orca-"),
            "OrcaRouter API keys start with `sk-orca-`; paste the full key from the OrcaRouter console"
        );
        Ok(Self {
            key,
            // A pasted key carries no response scope; the API-key path asks
            // for `api` and accepts it.
            granted_scope: ORCAROUTER_SCOPE.to_string(),
            source: OrcaCredentialSource::ApiKey,
        })
    }

    pub(crate) fn from_exchange(
        key: String,
        granted_scope: String,
        source: OrcaCredentialSource,
    ) -> Self {
        Self {
            key,
            granted_scope,
            source,
        }
    }

    /// The key material. Callers must not log, echo, or embed this value.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.key
    }

    #[must_use]
    pub fn granted_scope(&self) -> &str {
        &self.granted_scope
    }

    #[must_use]
    pub const fn source(&self) -> OrcaCredentialSource {
        self.source
    }

    /// Whether the granted scope satisfies this client's purpose. A
    /// downgraded grant must be surfaced, not assumed away.
    #[must_use]
    pub fn scope_satisfies_purpose(&self) -> bool {
        self.granted_scope == ORCAROUTER_SCOPE
    }
}

/// Where an injected OrcaRouter login reads its inputs from. Tests override
/// this so the whole adapter — bind, authorize URL, callback, exchange — runs
/// against a local fake auth server with no network egress.
pub struct OrcaLoginInputs {
    pub auth_base: String,
    pub api_base: String,
    pub app_name: String,
    pub open_browser: bool,
}

impl OrcaLoginInputs {
    /// Production inputs: env-resolved origins, the real app name, and the
    /// browser open flag (off when the no-browser var is set).
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            auth_base: resolve_orcarouter_auth_base(),
            api_base: resolve_orcarouter_api_base(),
            app_name: ORCAROUTER_APP_NAME.to_string(),
            open_browser: std::env::var_os(ORCAROUTER_OAUTH_PARAMS.env.no_browser_var).is_none(),
        }
    }
}

/// The OrcaRouter authorize URL.
///
/// Deliberately NOT [`build_authorize_url`]: OrcaRouter's contract is
/// `GET {auth}/auth?callback_url=…&code_challenge=…&code_challenge_method=S256&state=…&app_name=…&scope=api`.
/// There is no `client_id`, no `response_type`, and the redirect parameter is
/// named `callback_url` — three facts the generic builder gets wrong.
pub fn build_orcarouter_authorize_url(
    inputs: &OrcaLoginInputs,
    callback_url: &str,
    state: &str,
    pkce: &PkceChallenge,
) -> Result<String> {
    let mut url = oauth_endpoint_url(&format!(
        "{}/{}",
        inputs.auth_base.trim_end_matches('/'),
        ORCAROUTER_AUTHORIZE_PATH
    ))
    .with_context(|| "OrcaRouter auth base is not a valid secure URL — check ORCA_AUTH_BASE_URL")?;
    url.query_pairs_mut()
        .append_pair("callback_url", callback_url)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("app_name", &inputs.app_name)
        .append_pair("scope", ORCAROUTER_SCOPE);
    Ok(url.to_string())
}

/// The OrcaRouter code-for-key exchange endpoint, always on the **auth**
/// origin.
pub fn orcarouter_exchange_url(auth_base: &str) -> String {
    format!(
        "{}{}",
        auth_base.trim_end_matches('/'),
        ORCAROUTER_EXCHANGE_PATH
    )
}

/// The response body shape of a successful exchange:
/// `{ "key": "sk-orca-…", "user_id": "…", "scope": "api" }`.
#[derive(Deserialize)]
struct OrcaExchangeResponse {
    #[serde(default, alias = "access_token")]
    key: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Exchange an OrcaRouter auth code for a durable API key.
///
/// A PKCE-issued key is **not** a refresh token; there is no refresh grant and
/// no long-lived secret is stored. The response's granted `scope` is read back
/// and returned, never the scope this client asked for.
pub(crate) fn exchange_orcarouter_code(
    client: &dyn OAuthFormClient,
    auth_base: &str,
    code: &str,
    verifier: &str,
) -> Result<OrcaCredential> {
    let url = orcarouter_exchange_url(auth_base);
    let (status, body) = client.post_form(
        &url,
        &[
            ("code", code),
            ("code_verifier", verifier),
            ("code_challenge_method", "S256"),
        ],
    )?;
    let parsed: OrcaExchangeResponse = serde_json::from_str(&body).map_err(|_| {
        // Never echo the body: it carries the freshly minted key.
        anyhow::anyhow!(
            "OrcaRouter code exchange returned HTTP {status} that was not exchange JSON"
        )
    })?;
    if !(200..300).contains(&status) {
        // The server may reflect credentials or control text in `error` too.
        // Only fixed protocol codes may cross the display/log boundary.
        let err = match parsed.error.as_deref() {
            Some("invalid_grant") => "invalid_grant",
            Some("invalid_request") => "invalid_request",
            Some("access_denied") => "access_denied",
            Some("server_error") => "server_error",
            Some("temporarily_unavailable") => "temporarily_unavailable",
            _ => "exchange_failed",
        };
        // 400 = method mismatch / downgrade defence; 403 = code unknown,
        // expired, or already used, or verifier mismatch. Both are terminal
        // for this attempt; neither is retried.
        bail!(
            "OrcaRouter sign-in could not exchange the authorization code (HTTP {status}, {err}). Run `codewhale auth orcarouter` again."
        );
    }
    let key = parsed
        .key
        .filter(|key| !key.trim().is_empty())
        .context("OrcaRouter sign-in returned no API key")?;
    let key = codewhale_secrets::normalize_api_key(&key);
    anyhow::ensure!(
        key.starts_with("sk-orca-"),
        "OrcaRouter sign-in returned a key this client cannot use"
    );
    let granted_scope = parsed
        .scope
        .filter(|scope| !scope.trim().is_empty())
        .unwrap_or_else(|| ORCAROUTER_SCOPE.to_string());
    let _ = parsed.user_id;
    Ok(OrcaCredential::from_exchange(
        key,
        granted_scope,
        OrcaCredentialSource::Pkce,
    ))
}

/// Bind the OrcaRouter loopback callback: always `127.0.0.1`, ephemeral port.
///
/// OrcaRouter validates `http://127.0.0.1:<port>` (and `localhost`/`[::1]`)
/// per request and has no pre-registered redirect URI, so a fresh port per
/// attempt is correct rather than a conflict.
pub fn bind_orcarouter_callback() -> Result<Vec<TcpListener>> {
    let params = &ORCAROUTER_OAUTH_PARAMS;
    let mut listeners = Vec::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).with_context(|| {
        format!(
            "{} callback could not bind 127.0.0.1; {}",
            params.display_name, params.callback_conflict_hint
        )
    })?;
    listener
        .set_nonblocking(true)
        .with_context(|| format!("{} callback listener is not pollable", params.display_name))?;
    listeners.push(listener);
    Ok(listeners)
}

/// One interactive OrcaRouter PKCE login.
///
/// Protocol-identical to the ChatGPT flow — the crypto, the loopback listener,
/// the callback handling, the timeout and the terminal gate are the same code
/// paths — but it produces an [`OrcaCredential`] instead of refreshable token
/// material, because what OrcaRouter hands back is a durable API key.
pub fn orcarouter_pkce_login(
    inputs: &OrcaLoginInputs,
    challenge: &mut dyn std::io::Write,
) -> Result<OrcaCredential> {
    let params = &ORCAROUTER_OAUTH_PARAMS;
    let listeners = bind_orcarouter_callback()?;
    let request = start_orcarouter_auth_request(&listeners, inputs)?;
    writeln!(
        challenge,
        "{} sign-in (OAuth 2.0 + PKCE)",
        params.display_name
    )?;
    writeln!(challenge, "  Open:  {}", request.authorize_url)?;
    if inputs.open_browser && crate::utils::open_url(&request.authorize_url).is_err() {
        writeln!(
            challenge,
            "  Browser could not be opened; copy the URL above into a browser."
        )?;
    }
    let code = wait_for_callback(&listeners, params, &request.state)?;
    let client = ReqwestOAuthFormClient;
    let credential =
        exchange_orcarouter_code(&client, &inputs.auth_base, &code.0, &request.pkce.verifier)?;
    if !credential.scope_satisfies_purpose() {
        writeln!(
            challenge,
            "  Note: OrcaRouter granted scope \"{}\" while this client asked for \"{}\"; continuing with the narrower grant.",
            credential.granted_scope(),
            ORCAROUTER_SCOPE
        )?;
    }
    Ok(credential)
}

/// Persist an [`OrcaCredential`] through the host's ordinary, transactional
/// provider-credential path.
///
/// This is the single activation seam for **both** OrcaRouter adapters: the
/// pasted API key and the PKCE exchange both arrive here and are stored
/// identically, under the existing `orcarouter` secret-store slot with
/// `auth_mode = "api_key"` metadata. Nothing downstream can tell which adapter
/// produced the key, and nothing here treats it as refreshable OAuth material.
///
/// Live-config mirroring is the caller's job, exactly as it is for the other
/// owned logins: a shell command reloads config, while the in-session path
/// mutates the running `Config`.
pub fn activate_orcarouter_credential(
    credential: &OrcaCredential,
    config_path: Option<&Path>,
) -> Result<crate::config::SavedCredential> {
    let route_config = Config::load(config_path.map(Path::to_path_buf), None)?;
    let identity = route_config
        .builtin_provider_identity(crate::config::ProviderKind::Orcarouter)
        .map_err(anyhow::Error::msg)?;
    // Audit only the adapter, never the key: `api_key` or `pkce`. Both
    // adapters reach this seam and nothing downstream distinguishes them.
    tracing::info!(
        target: "codewhale::oauth",
        source = credential.source().as_str(),
        "OrcaRouter credential activated"
    );
    crate::config::save_api_key_for_identity(&identity, &route_config, credential.expose())
}

/// Build the loopback callback URL + authorize URL for one OrcaRouter attempt.
///
/// `redirect_uri` uses the literal `127.0.0.1` the listener bound, so the
/// value the user's browser is sent cannot disagree with the socket that is
/// waiting for it.
pub fn start_orcarouter_auth_request(
    listeners: &[TcpListener],
    inputs: &OrcaLoginInputs,
) -> Result<BrowserAuthRequest> {
    let port = listeners
        .first()
        .context("OrcaRouter OAuth callback has no bound listener")?
        .local_addr()
        .context("OrcaRouter OAuth callback listener has no local address")?
        .port();
    let redirect_uri = format!(
        "http://127.0.0.1:{port}{}",
        ORCAROUTER_OAUTH_PARAMS.callback_path
    );
    let pkce = generate_pkce();
    let state = generate_state();
    let authorize_url = build_orcarouter_authorize_url(inputs, &redirect_uri, &state, &pkce)?;
    Ok(BrowserAuthRequest {
        state,
        nonce: String::new(),
        pkce,
        redirect_uri,
        authorize_url,
    })
}

/// Resolved login inputs: schema defaults, environment-tested in order.
#[derive(Clone)]
pub struct ResolvedOAuthInputs {
    pub issuer: String,
    pub client_id: String,
    pub scopes: String,
    pub open_browser: bool,
}

impl OAuthProviderParams<'_> {
    /// Resolve issuer/client/scopes from the environment, first var wins.
    #[must_use]
    pub fn resolve_inputs(&self) -> ResolvedOAuthInputs {
        let first_set = |vars: &[&str], fallback: &str| {
            vars.iter()
                .filter_map(|var| std::env::var(var).ok())
                .find(|value| !value.trim().is_empty())
                .unwrap_or_else(|| fallback.to_string())
        };
        ResolvedOAuthInputs {
            issuer: first_set(self.env.issuer_vars, self.default_issuer),
            client_id: first_set(self.env.client_id_vars, self.default_client_id),
            scopes: first_set(self.env.scope_vars, self.default_scopes),
            open_browser: std::env::var_os(self.env.no_browser_var).is_none(),
        }
    }
}

/// Token material from a completed grant. No Debug: the tokens never print.
/// `interval` rides along because a `slow_down` error response may carry
/// the server's new minimum, which the loop prefers over its own tracked
/// value (RFC 8628 §3.5; WSL/VM clock drift).
#[derive(Clone, Deserialize)]
pub struct OAuthTokenMaterial {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub earliest_refresh_at: Option<Value>,
    /// OpenID Connect id token; carries the account claim when issued.
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
    /// Set only after cryptographic validation, never deserialized from a server.
    #[serde(skip)]
    pub(crate) verified_chatgpt: Option<ChatgptRegistration>,
    #[serde(default)]
    pub interval: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}

/// A completed login awaiting activation (owned-generation commit).
/// The provider is the table row it resolved through; unified activation
/// (3b-ii) matches on it.
pub struct PendingOAuthLogin {
    #[allow(dead_code, reason = "3b-ii unified activation matches on this")]
    pub provider: OAuthProvider,
    pub issuer: String,
    pub client_id: String,
    pub token: OAuthTokenMaterial,
}

/// Device-authorization response. Error fields ride along so a refused
/// grant classifies instead of failing to parse.
#[derive(Clone, Deserialize)]
struct DeviceGrantResponse {
    device_code: Option<String>,
    user_code: Option<String>,
    verification_uri: Option<String>,
    verification_uri_complete: Option<String>,
    expires_in: Option<u64>,
    interval: Option<u64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Bounds copied with the behavior: 20 s requests, 64 KiB bodies, 256 B of
/// error detail. A server that will not fit in the budget is an error, and
/// error text is whitespace-collapsed so a hostile endpoint cannot smuggle
/// terminal controls into diagnostics.
const OAUTH_REQUEST_TIMEOUT_SECS: u64 = 20;
const OAUTH_RESPONSE_BODY_LIMIT: u64 = 64 * 1024;
const OAUTH_ERROR_DETAIL_LIMIT: usize = 256;

/// Apply the existing OAuth browser URI policy to the URL that reqwest will
/// actually use. Parsing first keeps transport and loopback interpretation
/// identical; an issuer override does not authorize remote plaintext forms.
fn oauth_endpoint_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw).context("OAuth endpoint is not a valid URL")?;
    codewhale_config::device_code::validate_browser_verification_uri(
        url.as_str(),
        "OAuth endpoint",
    )
    .context("OAuth endpoints require HTTPS, except for local loopback HTTP")?;
    Ok(url)
}

fn oauth_http_client(endpoint: &reqwest::Url, purpose: &str) -> Result<reqwest::blocking::Client> {
    let builder = crate::tls::reqwest_blocking_client_builder()
        // An issuer-approved endpoint cannot delegate credential-bearing forms
        // to a redirect destination, including HTTPS-to-HTTP downgrades.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(OAUTH_REQUEST_TIMEOUT_SECS));
    // Every caller admits this URL through oauth_endpoint_url. Its HTTP
    // exception authorizes only a local exchange, never forwarding the form
    // through an ambient system or environment proxy.
    let builder = if endpoint.scheme() == "http" {
        builder.no_proxy()
    } else {
        builder
    };
    builder
        .build()
        .with_context(|| format!("Failed to build OAuth {purpose} client"))
}

fn parse_oauth_json<T: serde::de::DeserializeOwned>(
    response: reqwest::blocking::Response,
    operation: &str,
) -> Result<(reqwest::StatusCode, T)> {
    let status = response.status();
    let mut reader = response.take(OAUTH_RESPONSE_BODY_LIMIT + 1);
    let mut body = Vec::new();
    reader
        .read_to_end(&mut body)
        .with_context(|| format!("reading {operation} response"))?;
    let truncated = body.len() as u64 > OAUTH_RESPONSE_BODY_LIMIT;
    if truncated {
        body.truncate(OAUTH_RESPONSE_BODY_LIMIT as usize);
    }
    let parsed = serde_json::from_slice(&body).map_err(|_| {
        let limit = if truncated {
            " (body exceeded the 64 KiB diagnostic limit)"
        } else {
            ""
        };
        anyhow::anyhow!("{operation} returned HTTP {status}; expected JSON{limit}")
    })?;
    Ok((status, parsed))
}

fn bounded_oauth_error_text(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len().min(OAUTH_ERROR_DETAIL_LIMIT));
    let mut previous_was_space = false;
    let mut written = 0;
    for character in raw.chars() {
        let character = if character.is_whitespace() {
            ' '
        } else if character.is_control() {
            continue;
        } else {
            character
        };
        if character == ' ' && previous_was_space {
            continue;
        }
        if written == OAUTH_ERROR_DETAIL_LIMIT {
            break;
        }
        output.push(character);
        previous_was_space = character == ' ';
        written += 1;
    }
    output.trim().to_string()
}

fn oauth_failure_detail(
    error: Option<&str>,
    description: Option<&str>,
    status: reqwest::StatusCode,
) -> String {
    let mut code = bounded_oauth_error_text(error.unwrap_or("request_failed"));
    if code.is_empty() {
        code = "request_failed".to_string();
    }
    let description = description
        .map(bounded_oauth_error_text)
        .filter(|description| !description.is_empty() && description != &code);
    match description {
        Some(description) => format!("{code}: {description}; HTTP {status}"),
        None => format!("{code}; HTTP {status}"),
    }
}

/// Resolved token + device endpoints for one login.
#[derive(Clone, Debug, PartialEq, Eq)]
struct OAuthEndpoints {
    device_authorization_endpoint: Option<String>,
    token_endpoint: String,
}

/// OIDC discovery document. Only the fields a login needs.
#[derive(Clone, Deserialize)]
struct OidcDiscoveryDocument {
    issuer: Option<String>,
    device_authorization_endpoint: Option<String>,
    token_endpoint: Option<String>,
}

/// Resolve endpoints: OIDC discovery when the provider row asks for it,
/// documented-path fallback otherwise — and fallback on ANY discovery
/// failure, loudly logged. A hostile or broken discovery document must
/// never brick login; it only loses the custom endpoints.
fn resolve_oauth_endpoints(params: &OAuthProviderParams, issuer: &str) -> OAuthEndpoints {
    let fallback = || fallback_oauth_endpoints(params, issuer);
    if !params.discover_endpoints {
        return fallback();
    }
    match discover_oauth_endpoints(params, issuer) {
        Ok(endpoints) => endpoints,
        Err(err) => {
            tracing::warn!(
                target: "codewhale::oauth",
                error = %err,
                "{} OIDC discovery failed; using documented endpoint fallback",
                params.display_name
            );
            fallback()
        }
    }
}

fn discover_oauth_endpoints(params: &OAuthProviderParams, issuer: &str) -> Result<OAuthEndpoints> {
    let name = params.display_name;
    let discovery_url = oauth_endpoint_url(&format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    ))?;
    let client = oauth_http_client(&discovery_url, "OIDC discovery")?;
    #[cfg(test)]
    crate::external_credentials::record_oauth_network();
    let response = client
        .get(discovery_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .with_context(|| format!("{name} OIDC discovery request failed"))?;
    let (status, discovery): (_, OidcDiscoveryDocument) =
        parse_oauth_json(response, &format!("{name} OIDC discovery"))?;
    if !status.is_success() {
        bail!("{name} OIDC discovery failed with HTTP {status}");
    }
    validate_discovered_issuer(discovery.issuer, issuer)
        .with_context(|| format!("{name} OIDC discovery"))?;
    Ok(OAuthEndpoints {
        device_authorization_endpoint: params
            .device_code_path
            .map(|_| {
                validate_discovered_oauth_endpoint(
                    discovery.device_authorization_endpoint,
                    "device_authorization_endpoint",
                    issuer,
                )
            })
            .transpose()?,
        token_endpoint: validate_discovered_oauth_endpoint(
            discovery.token_endpoint,
            "token_endpoint",
            issuer,
        )?,
    })
}

/// Validate that an OIDC discovery document's issuer matches the requested issuer.
fn validate_discovered_issuer(discovered: Option<String>, expected: &str) -> Result<()> {
    let discovered = discovered
        .as_deref()
        .map(str::trim)
        .filter(|issuer| !issuer.is_empty())
        .context("OIDC discovery missing issuer")?;
    if discovered.trim_end_matches('/') != expected.trim_end_matches('/') {
        bail!("OIDC discovery issuer does not match the requested issuer");
    }
    let _ = oauth_endpoint_url(expected).context("OIDC issuer is not a trusted URL")?;
    Ok(())
}

/// Validate one discovered endpoint against the issuer: https-or-http scheme,
/// no plaintext downgrade, no embedded credentials, same origin.
fn validate_discovered_oauth_endpoint(
    endpoint: Option<String>,
    field: &str,
    issuer: &str,
) -> Result<String> {
    let endpoint = endpoint
        .as_deref()
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty())
        .with_context(|| format!("OIDC discovery missing {field}"))?;
    let parsed = reqwest::Url::parse(endpoint)
        .with_context(|| format!("OIDC discovery returned an invalid {field}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("OIDC discovery returned unsupported {field} scheme");
    }
    let issuer = oauth_endpoint_url(issuer).context("OIDC issuer is not a trusted URL")?;
    if issuer.scheme() == "https" && parsed.scheme() != "https" {
        bail!("OIDC discovery attempted to downgrade {field} from HTTPS");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        bail!("OIDC discovery returned credentials in {field}");
    }
    if parsed.origin() != issuer.origin() {
        bail!("OIDC discovery returned {field} on a different origin than the issuer");
    }
    let _ = oauth_endpoint_url(parsed.as_str())?;
    Ok(endpoint.to_string())
}

/// Documented-path endpoints for a provider row, no discovery.
fn fallback_oauth_endpoints(params: &OAuthProviderParams, issuer: &str) -> OAuthEndpoints {
    OAuthEndpoints {
        device_authorization_endpoint: params
            .device_code_path
            .map(|path| format!("{}/{}", issuer.trim_end_matches('/'), path)),
        token_endpoint: format!("{}/{}", issuer.trim_end_matches('/'), params.token_path),
    }
}

/// POST a device-authorization request. Pure transport over an explicit
/// endpoint: discovery (or its absence) is the caller's decision.
fn request_device_grant(
    device_authorization_endpoint: &str,
    client_id: &str,
    scopes: &str,
) -> Result<DeviceGrantResponse> {
    let device_authorization_endpoint = oauth_endpoint_url(device_authorization_endpoint)?;
    let client = oauth_http_client(&device_authorization_endpoint, "device-code")?;
    let params = [("client_id", client_id), ("scope", scopes)];
    #[cfg(test)]
    crate::external_credentials::record_oauth_network();
    let response = client
        .post(device_authorization_endpoint)
        .form(&params)
        .send()
        .context("OAuth device-code request failed")?;
    let (status, body): (_, DeviceGrantResponse) =
        parse_oauth_json(response, "OAuth device-code request")?;
    if !status.is_success() || body.error.is_some() {
        let detail = oauth_failure_detail(
            body.error.as_deref(),
            body.error_description.as_deref(),
            status,
        );
        bail!("OAuth device-code request failed ({detail})");
    }
    if body
        .device_code
        .as_deref()
        .is_some_and(|code| !code.trim().is_empty())
        && body
            .user_code
            .as_deref()
            .is_some_and(|code| !code.trim().is_empty())
    {
        return Ok(body);
    }
    bail!("OAuth device-code request returned success without a device and user code");
}

/// Poll the token endpoint once, classifying the RFC 8628 outcome. Matches
/// the legacy per-provider poll so the ported tests pin identical behavior.
fn poll_device_grant(
    token_endpoint: &str,
    client_id: &str,
    device_code: &str,
) -> Result<codewhale_config::device_code::DevicePollOutcome<OAuthTokenMaterial>> {
    use codewhale_config::device_code::DevicePollOutcome;
    let token_endpoint = oauth_endpoint_url(token_endpoint)?;
    let client = oauth_http_client(&token_endpoint, "device-code poll")?;
    let params = [
        ("client_id", client_id),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ("device_code", device_code),
    ];
    #[cfg(test)]
    crate::external_credentials::record_oauth_network();
    let response = client
        .post(token_endpoint)
        .form(&params)
        .send()
        .context("OAuth device-code poll failed")?;
    let (status, body): (_, OAuthTokenMaterial) =
        parse_oauth_json(response, "OAuth device-code poll")?;
    if status.is_success() && body.error.is_none() {
        return Ok(DevicePollOutcome::Complete(body));
    }
    match body.error.as_deref().unwrap_or("") {
        "authorization_pending" => Ok(DevicePollOutcome::Pending),
        "slow_down" => Ok(DevicePollOutcome::SlowDown {
            interval_seconds: body.interval,
        }),
        _ => {
            let detail = oauth_failure_detail(
                body.error.as_deref(),
                body.error_description.as_deref(),
                status,
            );
            bail!("OAuth device-code poll failed ({detail})");
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum OAuthChallengeStream {
    Stderr,
    Stdout,
}

fn oauth_challenge_stream(
    stderr_terminal: bool,
    stdout_terminal: bool,
) -> Result<OAuthChallengeStream> {
    if stderr_terminal {
        Ok(OAuthChallengeStream::Stderr)
    } else if stdout_terminal {
        Ok(OAuthChallengeStream::Stdout)
    } else {
        bail!(
            "Sign-in requires a terminal for the private login challenge; run codewhale auth in an interactive terminal without redirecting both output streams"
        );
    }
}

fn oauth_challenge_writer() -> Result<Box<dyn std::io::Write + Send>> {
    match oauth_challenge_stream(
        std::io::stderr().is_terminal(),
        std::io::stdout().is_terminal(),
    )? {
        OAuthChallengeStream::Stderr => Ok(Box::new(std::io::stderr())),
        OAuthChallengeStream::Stdout => Ok(Box::new(std::io::stdout())),
    }
}

/// Interactive device-code login for any provider whose row offers it.
/// Shows the verification URL + user code only on a terminal and polls until
/// approved. A provider with no device flow (ChatGPT) fails here with the
/// reason, instead of deep in transport code.
pub async fn device_code_login(provider: OAuthProvider) -> Result<PendingOAuthLogin> {
    // Endpoint resolution does blocking HTTP (discovery): it must run on the
    // blocking worker, never on the async executor. Providers with no device
    // flow fail here, before any thread spawns and before any network.
    let params = oauth_provider_params(provider);
    if params.device_code_path.is_none() {
        bail!(
            "{} offers no device-code flow; sign in through the browser login instead",
            params.display_name
        );
    }
    let inputs = params.resolve_inputs();
    let display_name = params.display_name;
    let challenge = oauth_challenge_writer()?;
    device_code_login_on_worker(provider, inputs, challenge)
        .await
        .with_context(|| format!("{display_name} device-code login worker failed"))
}

async fn device_code_login_on_worker(
    provider: OAuthProvider,
    inputs: ResolvedOAuthInputs,
    mut challenge: Box<dyn std::io::Write + Send>,
) -> Result<PendingOAuthLogin> {
    tokio::task::spawn_blocking(move || {
        device_code_login_with(provider, &inputs, challenge.as_mut())
    })
    .await
    .context("device-code protocol worker failed")?
}

/// Blocking protocol worker. Production callers capture a terminal first;
/// protocol tests inject a private output buffer or sink.
fn device_code_login_with(
    provider: OAuthProvider,
    inputs: &ResolvedOAuthInputs,
    challenge: &mut dyn std::io::Write,
) -> Result<PendingOAuthLogin> {
    let params = oauth_provider_params(provider);
    let display_name = params.display_name;
    let endpoints = resolve_oauth_endpoints(params, &inputs.issuer);
    let Some(device_endpoint) = endpoints.device_authorization_endpoint else {
        bail!(
            "{display_name} offers no device-code flow; sign in through the browser login instead"
        );
    };
    let token_endpoint = endpoints.token_endpoint;
    let poll_floor_secs = params.device_poll_floor_secs;
    let grant = request_device_grant(&device_endpoint, &inputs.client_id, &inputs.scopes)?;
    let verify = grant
        .verification_uri_complete
        .clone()
        .or(grant.verification_uri.clone())
        .unwrap_or_else(|| format!("{}/device", inputs.issuer.trim_end_matches('/')));
    // Off the wire, headed for `webbrowser::open`: must be a bare
    // navigation, never a scheme or credential smuggle.
    let verify = codewhale_config::device_code::validate_browser_verification_uri(
        &verify,
        &format!("{display_name} device-code request"),
    )?;
    let user_code = grant.user_code.unwrap_or_default();
    anyhow::ensure!(
        user_code.len() <= 1024 && !user_code.chars().any(char::is_control),
        "device-code login returned invalid user-code display data"
    );

    writeln!(challenge, "{display_name} device-code login")?;
    writeln!(challenge, "  Open:  {verify}")?;
    writeln!(challenge, "  Code:  {user_code}")?;
    writeln!(challenge, "{}", account_choice_hint(display_name))?;
    writeln!(
        challenge,
        "Waiting for approval in the browser… (Ctrl+C to abort)"
    )?;
    challenge.flush()?;
    if inputs.open_browser && webbrowser::open(&verify).is_err() {
        writeln!(
            challenge,
            "Could not open the browser automatically; open the displayed URL manually"
        )?;
    }

    let lifetime = Duration::from_secs(
        grant
            .expires_in
            .unwrap_or(DEVICE_POLL_MAX_SECS)
            .max(poll_floor_secs),
    );
    let token = codewhale_config::device_code::DeviceCodePoll::new(
        lifetime,
        format!(
            "{display_name} device-code authorization timed out. Re-run device login \
             and approve the code before it expires."
        ),
    )
    .interval_seconds(grant.interval)
    .wait_before_first_poll(true)
    .slow_down_timeout_message(format!(
        "{display_name} device-code authorization timed out after one or more slow_down \
         responses. That is usually clock drift in a WSL or VM environment; \
         sync the clock, then re-run device login and approve the code before \
         it expires."
    ))
    .run(std::thread::sleep, || {
        poll_device_grant(
            &token_endpoint,
            &inputs.client_id,
            grant.device_code.as_deref().unwrap_or(""),
        )
    })?;

    Ok(PendingOAuthLogin {
        provider,
        issuer: inputs.issuer.clone(),
        client_id: inputs.client_id.clone(),
        token,
    })
}

/// One login entry point for every provider: the params row decides whether
/// the grant is device-code or browser PKCE. A provider with neither fails
/// here with the reason.
async fn claude_login() -> Result<PendingOAuthLogin> {
    let mut output = oauth_challenge_writer()?;
    tokio::task::spawn_blocking(move || {
        let params = &CLAUDE_OAUTH_PARAMS;
        let pkce = generate_pkce();
        let redirect = "https://console.anthropic.com/oauth/code/callback";
        let mut url = reqwest::Url::parse("https://claude.ai/oauth/authorize")?;
        url.query_pairs_mut().extend_pairs([
            ("code", "true"), ("client_id", params.default_client_id),
            ("response_type", "code"), ("redirect_uri", redirect),
            ("scope", params.default_scopes), ("code_challenge", pkce.challenge.as_str()),
            ("code_challenge_method", "S256"), ("state", pkce.verifier.as_str()),
        ]);
        writeln!(output, "Open this URL to sign in with Claude:\n{url}\nPaste the authorization code (code#state):")?;
        output.flush()?;
        if std::env::var_os(params.env.no_browser_var).is_none() {
            let _ = webbrowser::open(url.as_str());
        }
        let mut input = String::new();
        std::io::stdin().lock().take(8193).read_line(&mut input)?;
        anyhow::ensure!(input.len() <= 8192, "Claude authorization code is too long");
        let (code, state) = input.trim().split_once('#').context("Expected Claude authorization code#state")?;
        anyhow::ensure!(!code.is_empty() && !code.chars().any(char::is_control)
            && codewhale_core::secret_eq::constant_time_eq(state.as_bytes(), pkce.verifier.as_bytes()),
            "Claude authorization state did not match the pending login");
        let (status, body) = ReqwestOAuthFormClient.post_form(
            &form_token_url(params, params.default_issuer), &[
                ("grant_type", "authorization_code"), ("client_id", params.default_client_id),
                ("code", code), ("state", state), ("redirect_uri", redirect),
                ("code_verifier", &pkce.verifier),
            ],
        )?;
        let token = parse_oauth_form_response(status, &body, "sign-in", params)?;
        Ok(PendingOAuthLogin {
            provider: OAuthProvider::Claude, issuer: params.default_issuer.to_string(),
            client_id: params.default_client_id.to_string(), token,
        })
    }).await.context("Claude login worker failed")?
}

pub async fn login(provider: OAuthProvider) -> Result<PendingOAuthLogin> {
    if provider == OAuthProvider::Claude {
        return claude_login().await;
    }
    if provider == OAuthProvider::Chatgpt {
        let config = Config::load(None, None)?;
        return login_with_config(provider, &config).await;
    }
    let params = oauth_provider_params(provider);
    if params.device_code_path.is_some() {
        device_code_login(provider).await
    } else if params.authorize_path.is_some() {
        pkce_login(provider).await
    } else {
        bail!("{} offers no sign-in flow", params.display_name);
    }
}

/// Reauthorize the registration selected by the caller's actual config.
pub async fn login_with_config(
    provider: OAuthProvider,
    config: &Config,
) -> Result<PendingOAuthLogin> {
    if provider == OAuthProvider::Claude {
        return claude_login().await;
    }
    if provider != OAuthProvider::Chatgpt {
        let params = oauth_provider_params(provider);
        return if params.device_code_path.is_some() {
            device_code_login(provider).await
        } else {
            pkce_login(provider).await
        };
    }
    let selected = official_chatgpt_registration(config).ok();
    let inputs = oauth_provider_params(provider).resolve_inputs();
    anyhow::ensure!(
        inputs.issuer == CHATGPT_OAUTH_ISSUER,
        "Official ChatGPT sign-in requires https://auth.openai.com; remove the issuer override"
    );
    let mut challenge = oauth_challenge_writer()?;
    tokio::task::spawn_blocking(move || {
        pkce_login_with_selected(provider, &inputs, selected, challenge.as_mut())
    })
    .await
    .context("ChatGPT PKCE login worker failed")?
}

// ── form-post transport seam ──────────────────────────────────────────
//
// One seam for every OAuth form post (PKCE exchange, refresh, revoke): the
// production client is reqwest with the shared bounds; tests substitute a
// mock issuer. The seam records network/refresh in test builds so the
// side-effect trap can still prove "zero external I/O" assertions.

pub(crate) trait OAuthFormClient {
    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, String)>;
    fn chatgpt_jwks(&self, issuer: &str) -> Result<JwkSet> {
        fetch_chatgpt_jwks(issuer)
    }
    fn revocation_endpoint(&self, params: &OAuthProviderParams, issuer: &str) -> Result<String> {
        remote_revoke_url(params, issuer).context("OAuth has no remote revoke endpoint")
    }
}

pub(crate) struct ReqwestOAuthFormClient;

impl OAuthFormClient for ReqwestOAuthFormClient {
    fn revocation_endpoint(&self, params: &OAuthProviderParams, issuer: &str) -> Result<String> {
        if params.display_name != "ChatGPT" {
            return remote_revoke_url(params, issuer)
                .context("OAuth has no remote revoke endpoint");
        }
        anyhow::ensure!(
            issuer == CHATGPT_OAUTH_ISSUER,
            "ChatGPT revocation issuer is invalid"
        );
        let discovery_url =
            oauth_endpoint_url(&format!("{issuer}/.well-known/openid-configuration"))?;
        let response = oauth_http_client(&discovery_url, "revocation discovery")?
            .get(discovery_url)
            .send()
            .context("ChatGPT revocation discovery failed")?;
        let (status, document): (_, Value) =
            parse_oauth_json(response, "ChatGPT revocation discovery")?;
        anyhow::ensure!(
            status.is_success() && document.get("issuer").and_then(Value::as_str) == Some(issuer),
            "ChatGPT revocation discovery issuer is invalid"
        );
        let endpoint = document
            .get("revocation_endpoint")
            .and_then(Value::as_str)
            .context("ChatGPT did not publish a revocation endpoint")?;
        let url = oauth_endpoint_url(endpoint)?;
        anyhow::ensure!(
            url.origin() == oauth_endpoint_url(issuer)?.origin()
                && url.query().is_none()
                && url.fragment().is_none(),
            "ChatGPT revocation endpoint is invalid"
        );
        Ok(url.to_string())
    }
    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, String)> {
        let url = oauth_endpoint_url(url)?;
        #[cfg(test)]
        crate::external_credentials::record_oauth_network();
        let client = oauth_http_client(&url, "form")?;
        let request = client.post(url.clone());
        let request = if url.as_str() == "https://console.anthropic.com/v1/oauth/token" {
            request.json(&form.iter().copied().collect::<BTreeMap<_, _>>())
        } else {
            request.form(form)
        };
        let response = request.send().context("OAuth token request failed")?;
        let status = response.status().as_u16();
        let mut reader = response.take(OAUTH_RESPONSE_BODY_LIMIT + 1);
        let mut body = Vec::new();
        reader
            .read_to_end(&mut body)
            .context("reading OAuth form response")?;
        if body.len() as u64 > OAUTH_RESPONSE_BODY_LIMIT {
            body.truncate(OAUTH_RESPONSE_BODY_LIMIT as usize);
        }
        Ok((status, String::from_utf8(body).unwrap_or_default()))
    }
}

/// Token/authorize endpoint URLs for one provider row.
pub(crate) fn form_token_url(params: &OAuthProviderParams, issuer: &str) -> String {
    format!("{}/{}", issuer.trim_end_matches('/'), params.token_path)
}

/// Pinned remote revoke URL; `None` when the provider revokes locally only.
pub(crate) fn remote_revoke_url(params: &OAuthProviderParams, issuer: &str) -> Option<String> {
    params
        .revoke_path
        .map(|path| format!("{}/{}", issuer.trim_end_matches('/'), path))
}

/// Parse a form-post token response. Error bodies are never echoed: the
/// detail names the error code only, so a hostile issuer cannot smuggle
/// secret-bearing text back through diagnostics.
pub(crate) fn parse_oauth_form_response(
    status: u16,
    body: &str,
    operation: &str,
    params: &OAuthProviderParams,
) -> Result<OAuthTokenMaterial> {
    let name = params.display_name;
    let parsed: OAuthTokenMaterial = serde_json::from_str(body).map_err(|_| {
        anyhow::anyhow!("{name} OAuth {operation} returned HTTP {status} that was not token JSON")
    })?;
    if !(200..300).contains(&status) || parsed.error.is_some() {
        let err = parsed.error.as_deref().unwrap_or("token_error");
        let err = if params.display_name == "Claude"
            && !matches!(
                err,
                "invalid_grant" | "invalid_client" | "invalid_request" | "access_denied"
            ) {
            "token_error"
        } else {
            err
        };
        if matches!(
            err,
            "invalid_grant"
                | "refresh_token_reused"
                | "refresh_token_expired"
                | "refresh_token_invalidated"
        ) || status == 401
        {
            bail!(
                "{name} OAuth {operation} failed permanently ({err}). Sign in again with `{}`.",
                params.relogin_hint
            );
        }
        bail!("{name} OAuth {operation} failed ({err})");
    }
    anyhow::ensure!(
        parsed
            .access_token
            .as_deref()
            .is_some_and(|token| !token.trim().is_empty()),
        "{name} OAuth {operation} returned an empty access token"
    );
    Ok(parsed)
}

fn compact_form_error(body: &str) -> String {
    body.chars().filter(|c| !c.is_control()).take(80).collect()
}

/// Refresh an owned token through the seam at an explicit token URL —
/// discovered when the provider row demands it, pinned otherwise. Refresh is
/// a Codewhale-owned credential operation only: external imports never
/// refresh.
pub(crate) fn refresh_access_token_via(
    client: &dyn OAuthFormClient,
    params: &OAuthProviderParams,
    token_url: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<OAuthTokenMaterial> {
    #[cfg(test)]
    crate::external_credentials::record_oauth_refresh();
    let mut fields = vec![
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", refresh_token),
    ];
    if params.display_name == "ChatGPT" {
        fields.push(("resource", CHATGPT_OAUTH_RESOURCE));
    }
    let (status, body) = client.post_form(token_url, &fields)?;
    parse_oauth_form_response(status, &body, "refresh", params)
}

/// Best-effort remote revoke through the seam. Callers clear local
/// credentials regardless of this outcome.
pub(crate) fn revoke_remote_token_via(
    client: &dyn OAuthFormClient,
    params: &OAuthProviderParams,
    issuer: &str,
    client_id: &str,
    token: &str,
) -> Result<()> {
    let revoke_url = client.revocation_endpoint(params, issuer)?;
    let mut fields = vec![("token", token), ("client_id", client_id)];
    if params.display_name == "ChatGPT" {
        fields.push(("token_type_hint", "refresh_token"));
    }
    let (status, _) = client.post_form(&revoke_url, &fields)?;
    anyhow::ensure!(
        (200..300).contains(&status),
        "{} OAuth revoke failed with HTTP {status}",
        params.display_name
    );
    Ok(())
}

// ── PKCE browser login ────────────────────────────────────────────────

/// RFC 7636 S256 PKCE pair. Custom Debug: the verifier is exchanged for
/// bearer material and never prints.
#[derive(Clone)]
pub struct PkceChallenge {
    pub verifier: String,
    pub challenge: String,
}

impl std::fmt::Debug for PkceChallenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PkceChallenge")
            .field("verifier", &"<redacted>")
            .field("challenge", &self.challenge)
            .finish()
    }
}

/// A browser authorization request in flight: state, PKCE pair, and the
/// registered redirect the callback server answers on.
#[derive(Clone)]
pub struct BrowserAuthRequest {
    pub state: String,
    pub nonce: String,
    pub pkce: PkceChallenge,
    pub redirect_uri: String,
    pub authorize_url: String,
}

impl std::fmt::Debug for BrowserAuthRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserAuthRequest")
            .field("state", &self.state)
            .field("pkce", &self.pkce)
            .field("redirect_uri", &self.redirect_uri)
            .field("authorize_url", &"<redacted>")
            .finish()
    }
}

/// Parsed callback query: a code+state pair, or the issuer's refusal.
#[derive(Clone, PartialEq, Eq)]
pub enum CallbackOutcome {
    Success {
        code: String,
        state: String,
        client_id: Option<String>,
    },
    Error {
        error: String,
        description: Option<String>,
        state: Option<String>,
    },
}

impl std::fmt::Debug for CallbackOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Success {
                state, client_id, ..
            } => f
                .debug_struct("Success")
                .field("code", &"<redacted>")
                .field("state", state)
                .field("client_id", client_id)
                .finish(),
            Self::Error { state, .. } => f
                .debug_struct("Error")
                .field("error", &"<redacted>")
                .field("state", state)
                .finish(),
        }
    }
}

/// RFC 7636 S256 PKCE pair.
#[must_use]
pub fn generate_pkce() -> PkceChallenge {
    let verifier = random_url_token(32);
    let digest = Sha256::digest(verifier.as_bytes());
    PkceChallenge {
        verifier,
        challenge: URL_SAFE_NO_PAD.encode(digest),
    }
}

#[must_use]
pub fn generate_state() -> String {
    random_url_token(16)
}

fn random_url_token(nbytes: usize) -> String {
    let mut bytes = vec![0u8; nbytes.max(16)];
    let mut offset = 0;
    while offset < bytes.len() {
        let chunk = uuid::Uuid::new_v4();
        let take = (bytes.len() - offset).min(16);
        bytes[offset..offset + take].copy_from_slice(&chunk.as_bytes()[..take]);
        offset += take;
    }
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn build_authorize_url(
    params: &OAuthProviderParams,
    issuer: &str,
    client_id: &str,
    scopes: &str,
    redirect_uri: &str,
    state: &str,
    pkce: &PkceChallenge,
) -> Result<String> {
    let Some(authorize_path) = params.authorize_path else {
        bail!("{} offers no browser sign-in flow", params.display_name);
    };
    // A malformed configured issuer must fail loudly. Silently redirecting
    // the browser to the production authorize endpoint would hand the
    // issuer a sign-in the user aimed somewhere else.
    let issuer_var = params
        .env
        .issuer_vars
        .first()
        .copied()
        .unwrap_or("the issuer environment variable");
    let mut url = oauth_endpoint_url(&format!(
        "{}/{}",
        issuer.trim_end_matches('/'),
        authorize_path
    ))
    .with_context(|| {
        format!(
            "{} OAuth issuer is not a valid URL or uses an insecure endpoint — check {issuer_var}",
            params.display_name
        )
    })?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", scopes)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state);
    if let Some(originator) = params.originator {
        url.query_pairs_mut().append_pair("originator", originator);
    }
    for (key, value) in params.authorize_extras {
        url.query_pairs_mut().append_pair(key, value);
    }
    let account_prompt_disabled = params
        .env
        .no_account_prompt_var
        .is_some_and(|var| std::env::var_os(var).is_some());
    if !account_prompt_disabled {
        for (key, value) in params.account_choice_extras {
            url.query_pairs_mut().append_pair(key, value);
        }
    }
    Ok(url.to_string())
}

pub fn parse_callback_query(params: &OAuthProviderParams, query: &str) -> Result<CallbackOutcome> {
    let parsed = reqwest::Url::parse(&format!("http://127.0.0.1{}?{query}", params.callback_path))
        .context("OAuth callback query is not valid")?;
    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut description = None;
    let mut client_id = None;
    let mut seen = std::collections::HashSet::new();
    for (key, value) in parsed.query_pairs() {
        if matches!(
            key.as_ref(),
            "code" | "state" | "error" | "error_description" | "client_id"
        ) {
            anyhow::ensure!(
                seen.insert(key.to_string()),
                "OAuth callback contains a duplicate parameter"
            );
        }
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            "error_description" => description = Some(value.into_owned()),
            "client_id" => client_id = Some(value.into_owned()),
            _ => {}
        }
    }
    anyhow::ensure!(
        code.is_none() || error.is_none(),
        "OAuth callback contains both success and error"
    );
    if let Some(error) = error {
        return Ok(CallbackOutcome::Error {
            error,
            description,
            state,
        });
    }
    let code = code
        .filter(|c| !c.trim().is_empty())
        .context("OAuth callback missing authorization code")?;
    let state = state
        .filter(|s| !s.trim().is_empty())
        .context("OAuth callback missing state")?;
    Ok(CallbackOutcome::Success {
        code,
        state,
        client_id,
    })
}

#[cfg(test)]
pub fn accept_callback(expected_state: &str, outcome: CallbackOutcome) -> Result<String> {
    accept_callback_with_client(expected_state, outcome).map(|(code, _)| code)
}

fn accept_callback_with_client(
    expected_state: &str,
    outcome: CallbackOutcome,
) -> Result<(String, Option<String>)> {
    match outcome {
        CallbackOutcome::Success {
            code,
            state,
            client_id,
        } => {
            anyhow::ensure!(
                codewhale_core::secret_eq::constant_time_eq(
                    state.as_bytes(),
                    expected_state.as_bytes(),
                ),
                "OAuth callback state did not match the pending login"
            );
            Ok((code, client_id))
        }
        CallbackOutcome::Error {
            error,
            description,
            state,
        } => {
            anyhow::ensure!(
                state.as_deref() == Some(expected_state),
                "OAuth error callback state did not match the pending login"
            );
            let _ = description;
            let error = compact_form_error(&error);
            bail!("sign-in was not completed ({error})")
        }
    }
}

fn parse_http_request_target(request_line: &str) -> Result<String> {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    anyhow::ensure!(
        method.eq_ignore_ascii_case("GET"),
        "OAuth callback must be GET"
    );
    let target = parts
        .next()
        .context("OAuth callback missing request target")?;
    Ok(target.to_string())
}

fn query_from_target<'a>(params: &OAuthProviderParams, target: &'a str) -> Result<&'a str> {
    let path = target.split('?').next().unwrap_or(target);
    anyhow::ensure!(
        path == params.callback_path,
        "OAuth callback path was not {}",
        params.callback_path
    );
    Ok(target.split_once('?').map(|(_, q)| q).unwrap_or(""))
}

/// Bind ChatGPT's exact 127.0.0.1 callback, with an ephemeral-port fallback.
/// Other providers using localhost bind both IP stacks when available.
pub fn bind_loopback_callback(params: &OAuthProviderParams) -> Result<Vec<TcpListener>> {
    let name = params.display_name;
    let mut last_error = None;
    for port in params.loopback_ports {
        let mut bound = Vec::new();
        for addr in [
            SocketAddr::from((Ipv4Addr::LOCALHOST, *port)),
            SocketAddr::from((Ipv6Addr::LOCALHOST, *port)),
        ] {
            if params.display_name == "ChatGPT" && addr.is_ipv6() {
                continue;
            }
            match TcpListener::bind(addr) {
                Ok(listener) => {
                    listener.set_nonblocking(true).with_context(|| {
                        format!("{name} OAuth callback listener could not be set non-blocking")
                    })?;
                    bound.push(listener);
                }
                Err(error) => last_error = Some(error),
            }
        }
        if !bound.is_empty() {
            return Ok(bound);
        }
    }
    let ports = params
        .loopback_ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(" or ");
    let hint = if params.callback_conflict_hint.is_empty() {
        String::new()
    } else {
        format!(" {}", params.callback_conflict_hint)
    };
    Err(last_error
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow::anyhow!("unable to bind {name} OAuth callback ports")))
    .with_context(|| format!("{name} sign-in needs loopback port {ports}.{hint}"))
}

pub(crate) fn start_auth_request_on(
    listeners: &[TcpListener],
    params: &OAuthProviderParams,
    inputs: &ResolvedOAuthInputs,
) -> Result<BrowserAuthRequest> {
    let port = listeners
        .first()
        .with_context(|| {
            format!(
                "{} OAuth callback has no bound listener",
                params.display_name
            )
        })?
        .local_addr()
        .with_context(|| {
            format!(
                "{} OAuth callback listener has no local address",
                params.display_name
            )
        })?
        .port();
    let host = if params.display_name == "ChatGPT" {
        "127.0.0.1"
    } else {
        "localhost"
    };
    let redirect_uri = format!("http://{host}:{port}{}", params.callback_path);
    let pkce = generate_pkce();
    let state = generate_state();
    let nonce = generate_state();
    let mut authorize_url = reqwest::Url::parse(&build_authorize_url(
        params,
        &inputs.issuer,
        &inputs.client_id,
        &inputs.scopes,
        &redirect_uri,
        &state,
        &pkce,
    )?)?;
    if params.display_name == "ChatGPT" {
        authorize_url.query_pairs_mut().append_pair("nonce", &nonce);
        if inputs.client_id == CHATGPT_OAUTH_CLIENT_ID {
            authorize_url
                .query_pairs_mut()
                .append_pair("agent_name_hint", "Codewhale");
        }
    }
    Ok(BrowserAuthRequest {
        state,
        nonce,
        pkce,
        redirect_uri,
        authorize_url: authorize_url.to_string(),
    })
}

// Allow time for browser authentication and 2FA, as in the MCP OAuth flow.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(900);
const CALLBACK_HTML_OK: &str = "<!doctype html><html><body><p>Signed in to Codewhale. You can close this tab.</p></body></html>";
const CALLBACK_HTML_ERR: &str = "<!doctype html><html><body><p>Sign-in did not complete. You can close this tab and retry in Codewhale.</p></body></html>";

fn wait_for_callback(
    listeners: &[TcpListener],
    params: &OAuthProviderParams,
    expected_state: &str,
) -> Result<(String, Option<String>)> {
    wait_for_callback_validated(listeners, params, expected_state, None)
}

fn wait_for_callback_validated(
    listeners: &[TcpListener],
    params: &OAuthProviderParams,
    expected_state: &str,
    expected_issuer: Option<&str>,
) -> Result<(String, Option<String>)> {
    let deadline = Instant::now() + CALLBACK_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            bail!(
                "{} sign-in timed out waiting for the browser callback",
                params.display_name
            );
        }
        // Whichever stack `localhost` resolved to for the browser is the one
        // that gets the connection; poll them all.
        for listener in listeners {
            match listener.accept() {
                Ok((stream, _)) => {
                    return handle_callback_stream_validated(
                        stream,
                        params,
                        expected_state,
                        expected_issuer,
                    );
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => {
                    return Err(error).context(format!(
                        "{} OAuth callback accept failed",
                        params.display_name
                    ));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
fn handle_callback_stream(
    stream: TcpStream,
    params: &OAuthProviderParams,
    expected_state: &str,
) -> Result<(String, Option<String>)> {
    handle_callback_stream_validated(stream, params, expected_state, None)
}

fn handle_callback_stream_validated(
    mut stream: TcpStream,
    params: &OAuthProviderParams,
    expected_state: &str,
    expected_issuer: Option<&str>,
) -> Result<(String, Option<String>)> {
    // BSD sockets (macOS) hand the accepted stream the listener's O_NONBLOCK;
    // the bounded read below needs a blocking socket with a timeout.
    stream.set_nonblocking(false).with_context(|| {
        format!(
            "{} OAuth callback stream could not be set blocking",
            params.display_name
        )
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    // One read is not one request: TCP may deliver the callback in
    // fragments, and a truncated query parses as a missing parameter.
    // Read until the blank line that ends the HTTP headers.
    let mut buf = [0u8; 4096];
    let mut len = 0usize;
    loop {
        if len == buf.len() {
            break;
        }
        let n = stream
            .read(&mut buf[len..])
            .with_context(|| format!("reading {} OAuth callback request", params.display_name))?;
        if n == 0 {
            break;
        }
        len += n;
        if buf[..len].windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&buf[..len]);
    let request_line = request.lines().next().unwrap_or_default();
    let result = (|| {
        let target = parse_http_request_target(request_line)?;
        let query = query_from_target(params, &target)?;
        if let Some(issuer) = expected_issuer {
            validate_plugin_callback(query, issuer)?;
        }
        let outcome = parse_callback_query(params, query)?;
        accept_callback_with_client(expected_state, outcome)
    })();
    let (status, body) = match &result {
        Ok(_) => ("200 OK", CALLBACK_HTML_OK),
        Err(_) => ("400 Bad Request", CALLBACK_HTML_ERR),
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    result
}

pub(crate) fn exchange_authorization_code(
    client: &dyn OAuthFormClient,
    params: &OAuthProviderParams,
    token_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    code: &str,
    verifier: &str,
) -> Result<OAuthTokenMaterial> {
    let mut fields = vec![
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("code", code),
        ("code_verifier", verifier),
    ];
    if params.display_name == "ChatGPT" {
        fields.push(("resource", CHATGPT_OAUTH_RESOURCE));
    }
    let (status, body) = client.post_form(token_endpoint, &fields)?;
    parse_oauth_form_response(status, &body, "authorization code exchange", params)
}

/// Terminal-or-stdout challenge writer for shell commands that drive a login
/// outside the TUI (`codewhale auth ...`).
pub fn cli_challenge_writer() -> Result<Box<dyn std::io::Write + Send>> {
    oauth_challenge_writer()
}

/// Interactive PKCE browser login for any provider whose row offers it.
/// Shows the authorize URL only on a terminal, opens a browser, and waits for the loopback
/// callback. A provider with no browser flow (xAI) fails here with the
/// reason, before any listener binds.
pub async fn pkce_login(provider: OAuthProvider) -> Result<PendingOAuthLogin> {
    let params = oauth_provider_params(provider);
    if params.authorize_path.is_none() {
        bail!(
            "{} offers no browser sign-in flow; sign in through the device-code login instead",
            params.display_name
        );
    }
    let inputs = params.resolve_inputs();
    if provider == OAuthProvider::Chatgpt {
        anyhow::ensure!(
            inputs.issuer == CHATGPT_OAUTH_ISSUER,
            "Official ChatGPT sign-in requires https://auth.openai.com; remove the issuer override"
        );
    }
    let display_name = params.display_name;
    let mut challenge = oauth_challenge_writer()?;
    tokio::task::spawn_blocking(move || {
        pkce_login_with_selected(provider, &inputs, None, challenge.as_mut())
    })
    .await
    .with_context(|| format!("{display_name} PKCE login worker failed"))?
}

/// Blocking browser protocol worker with a previously admitted terminal.
fn pkce_login_with_selected(
    provider: OAuthProvider,
    inputs: &ResolvedOAuthInputs,
    selected: Option<ChatgptRegistration>,
    challenge: &mut dyn std::io::Write,
) -> Result<PendingOAuthLogin> {
    let params = oauth_provider_params(provider);
    let display_name = params.display_name;
    let mut host = if provider == OAuthProvider::Chatgpt {
        Some(prepare_chatgpt_host()?)
    } else {
        None
    };
    if let Some(host) = &mut host {
        if !std::env::var("CODEWHALE_CHATGPT_NEW_ACCOUNT").is_ok_and(|value| value == "1")
            && let Some(mut selected) = selected
        {
            selected.host_id = host.host_id.clone();
            host.registration = Some(selected);
        }
        if let Some(registration) = &host.registration {
            anyhow::ensure!(
                registration.issuer == inputs.issuer,
                "Saved ChatGPT registration belongs to a different issuer; credentials were not replaced"
            );
        }
    }
    let mut inputs = inputs.clone();
    if let Some(host) = &host {
        inputs.client_id = host.registration.as_ref().map_or_else(
            || CHATGPT_OAUTH_CLIENT_ID.to_string(),
            |registration| registration.client_id.clone(),
        );
    }
    let listeners = bind_loopback_callback(params)?;
    let mut request = start_auth_request_on(&listeners, params, &inputs)?;
    if let Some(host) = &host {
        let mut url = reqwest::Url::parse(&request.authorize_url)?;
        url.query_pairs_mut()
            .append_pair("ext_agent_host_id", &host.host_id);
        if let Some(email) = host
            .registration
            .as_ref()
            .and_then(|registration| registration.email.as_deref())
        {
            url.query_pairs_mut().append_pair("login_hint", email);
        }
        request.authorize_url = url.to_string();
    }
    writeln!(challenge, "{display_name} sign-in (PKCE)")?;
    writeln!(challenge, "  Open:  {}", request.authorize_url)?;
    writeln!(challenge, "{}", account_choice_hint(display_name))?;
    writeln!(
        challenge,
        "Waiting for the browser callback… (Ctrl+C to abort)"
    )?;
    challenge.flush()?;
    if inputs.open_browser && webbrowser::open(&request.authorize_url).is_err() {
        writeln!(
            challenge,
            "Could not open the browser automatically; open the displayed URL manually"
        )?;
    }
    let (code, callback_id) = wait_for_callback(&listeners, params, &request.state)?;
    let client_id = if provider == OAuthProvider::Chatgpt {
        issued_callback_client_id(&inputs.client_id, callback_id.as_deref())?
    } else {
        inputs.client_id.clone()
    };
    let mut token = exchange_authorization_code(
        &ReqwestOAuthFormClient,
        params,
        &form_token_url(params, &inputs.issuer),
        &client_id,
        &request.redirect_uri,
        &code,
        &request.pkce.verifier,
    )?;
    if let Some(host) = &host {
        require_chatgpt_scopes(token.scope.as_deref())?;
        anyhow::ensure!(
            token
                .token_type
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("bearer")),
            "ChatGPT token response did not return Bearer credentials"
        );
        let keys = fetch_chatgpt_jwks(&inputs.issuer)?;
        let registration = verify_chatgpt_id_token(
            token
                .id_token
                .as_deref()
                .context("ChatGPT sign-in omitted its ID token")?,
            &keys,
            &inputs.issuer,
            &client_id,
            Some(&request.nonce),
            host.registration
                .as_ref()
                .map(|registration| registration.subject.as_str()),
            &host.host_id,
        )?;
        verify_chatgpt_access_token(
            token
                .access_token
                .as_deref()
                .context("ChatGPT sign-in omitted its access token")?,
            &keys,
            &registration,
        )?;
        token.verified_chatgpt = Some(registration);
    }
    Ok(PendingOAuthLogin {
        provider,
        issuer: inputs.issuer.clone(),
        client_id,
        token,
    })
}

// ── owned credential storage (one shared store) ───────────────
//
// Codewhale-owned OAuth generations live in the config crate's credential
// store under per-provider generation prefixes. xAI historically stored the
// access token as `key` (Grok CLI shape); ChatGPT as `access_token`. One
// entry type reads both; new generations write the unified shape.

/// xAI issuer and public client (constants the params rows and the route
/// surfaces share).
pub const XAI_OIDC_ISSUER: &str = "https://auth.x.ai";
pub const GROK_OIDC_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const DEFAULT_SCOPES: &str = "openid profile email offline_access api:access grok-cli:access";
/// Hard ceiling past a device grant's own `expires_in`.
pub(crate) const DEVICE_POLL_MAX_SECS: u64 = 900;

/// ChatGPT issuer and public client.
pub const CHATGPT_OAUTH_ISSUER: &str = "https://auth.openai.com";
pub const CHATGPT_OAUTH_CLIENT_ID: &str = "dynamic_agent_client";
pub const CHATGPT_OAUTH_RESOURCE: &str = "https://api.openai.com/v1";
pub const CHATGPT_OAUTH_SCOPE: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

/// Verified registration identity. The client ID also binds the selected workspace.
/// One active registration is supported; use CODEWHALE_CHATGPT_NEW_ACCOUNT=1
/// to explicitly replace it after a new, fully validated browser sign-in.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ChatgptRegistration {
    pub issuer: String,
    pub client_id: String,
    pub subject: String,
    pub email: Option<String>,
    pub host_id: String,
}

#[derive(Serialize, Deserialize)]
struct ChatgptHost {
    host_id: String,
    #[serde(default)]
    registration: Option<ChatgptRegistration>,
}

fn valid_issued_chatgpt_client_id(value: &str) -> bool {
    value.starts_with("oaiapp_")
        && value.len() > 7
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn validate_chatgpt_host(host: &ChatgptHost) -> Result<()> {
    let id = host
        .host_id
        .strip_prefix("urn:uuid:")
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .context("ChatGPT host identifier is invalid")?;
    anyhow::ensure!(
        id.get_version_num() == 4,
        "ChatGPT host identifier must be a UUIDv4"
    );
    if let Some(registration) = &host.registration {
        anyhow::ensure!(
            registration.host_id == host.host_id
                && valid_issued_chatgpt_client_id(&registration.client_id)
                && !registration.subject.trim().is_empty(),
            "Saved ChatGPT registration is invalid"
        );
    }
    Ok(())
}

fn prepare_chatgpt_host() -> Result<ChatgptHost> {
    codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
        let name = codewhale_config::CHATGPT_HOST_FILE_NAME;
        let mut host: ChatgptHost = match store.read_to_string(name)? {
            Some(raw) => {
                serde_json::from_str(&raw).context("Saved ChatGPT host metadata is invalid")?
            }
            None => ChatgptHost {
                host_id: format!("urn:uuid:{}", uuid::Uuid::new_v4()),
                registration: None,
            },
        };
        validate_chatgpt_host(&host)?;
        // Persist before opening a browser, including after a cancelled first login.
        store.write(name, &serde_json::to_vec(&host)?, true)?;
        if std::env::var("CODEWHALE_CHATGPT_NEW_ACCOUNT").is_ok_and(|value| value == "1") {
            host.registration = None;
        }
        Ok(host)
    })
}

fn require_chatgpt_scopes(scope: Option<&str>) -> Result<()> {
    let scope = scope.context("ChatGPT token response omitted granted scopes; sign in again")?;
    for required in ["chatgpt.tokens.use.direct", "resource.invoke"] {
        anyhow::ensure!(
            scope
                .split_ascii_whitespace()
                .any(|value| value == required),
            "ChatGPT plan permission was not granted; authorize plan usage in ChatGPT and sign in again"
        );
    }
    Ok(())
}

#[derive(Clone, Deserialize)]
struct ChatgptIdClaims {
    sub: String,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

fn fetch_chatgpt_jwks(issuer: &str) -> Result<JwkSet> {
    let url = oauth_endpoint_url(&format!(
        "{}/.well-known/jwks.json",
        issuer.trim_end_matches('/')
    ))?;
    let response = oauth_http_client(&url, "identity verification")?
        .get(url)
        .send()
        .context("ChatGPT identity verification keys could not be retrieved")?;
    let (status, keys) = parse_oauth_json(response, "ChatGPT verification keys")?;
    anyhow::ensure!(
        status.is_success(),
        "ChatGPT identity verification keys are unavailable"
    );
    Ok(keys)
}

fn verify_chatgpt_jwt<T: serde::de::DeserializeOwned + Clone>(
    token: &str,
    keys: &JwkSet,
    issuer: &str,
    audience: &str,
) -> Result<T> {
    let header = decode_header(token).context("ChatGPT identity token header is invalid")?;
    anyhow::ensure!(
        matches!(
            header.alg,
            Algorithm::RS256
                | Algorithm::RS384
                | Algorithm::RS512
                | Algorithm::ES256
                | Algorithm::ES384
                | Algorithm::EdDSA
        ),
        "ChatGPT identity token uses an unsupported signature algorithm"
    );
    let kid = header
        .kid
        .as_deref()
        .context("ChatGPT identity token omitted its verification key")?;
    let jwk = keys
        .find(kid)
        .context("ChatGPT identity token verification key is unknown")?;
    let key = DecodingKey::from_jwk(jwk).context("ChatGPT identity verification key is invalid")?;
    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[audience]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    validation.validate_nbf = true;
    validation.leeway = 0;
    let claims = decode::<Value>(token, &key, &validation)
        .map_err(|_| {
            anyhow::anyhow!("ChatGPT token signature or identity claims failed verification")
        })?
        .claims;
    let exact_audience = claims.get("aud").is_some_and(|aud| {
        aud.as_str() == Some(audience)
            || aud
                .as_array()
                .is_some_and(|values| values.len() == 1 && values[0].as_str() == Some(audience))
    });
    anyhow::ensure!(
        exact_audience,
        "ChatGPT token audience does not match this registration"
    );
    serde_json::from_value(claims).context("ChatGPT token identity claims are invalid")
}

fn verify_chatgpt_id_token(
    token: &str,
    keys: &JwkSet,
    issuer: &str,
    client_id: &str,
    nonce: Option<&str>,
    expected_subject: Option<&str>,
    host_id: &str,
) -> Result<ChatgptRegistration> {
    let claims: ChatgptIdClaims = verify_chatgpt_jwt(token, keys, issuer, client_id)?;
    anyhow::ensure!(
        !claims.sub.trim().is_empty(),
        "ChatGPT identity token has no subject"
    );
    if let Some(nonce) = nonce {
        anyhow::ensure!(
            claims.nonce.as_deref() == Some(nonce),
            "ChatGPT identity token nonce did not match the pending login"
        );
    }
    if let Some(subject) = expected_subject {
        anyhow::ensure!(
            claims.sub == subject,
            "ChatGPT sign-in returned a different account; the selected registration was not replaced"
        );
    }
    Ok(ChatgptRegistration {
        issuer: issuer.to_string(),
        client_id: client_id.to_string(),
        subject: claims.sub,
        email: claims.email,
        host_id: host_id.to_string(),
    })
}

#[derive(Clone, Deserialize)]
struct ChatgptAccessClaims {
    sub: String,
    client_id: String,
    scope: String,
}

fn verify_chatgpt_access_token(
    token: &str,
    keys: &JwkSet,
    registration: &ChatgptRegistration,
) -> Result<()> {
    let claims: ChatgptAccessClaims =
        verify_chatgpt_jwt(token, keys, &registration.issuer, CHATGPT_OAUTH_RESOURCE)?;
    anyhow::ensure!(
        claims.sub == registration.subject && claims.client_id == registration.client_id,
        "ChatGPT access token does not match the selected account registration"
    );
    require_chatgpt_scopes(Some(&claims.scope))
}

fn issued_callback_client_id(pending_id: &str, callback_id: Option<&str>) -> Result<String> {
    if pending_id == CHATGPT_OAUTH_CLIENT_ID {
        let issued =
            callback_id.context("ChatGPT registration did not return an issued client ID")?;
        anyhow::ensure!(
            valid_issued_chatgpt_client_id(issued),
            "ChatGPT registration returned an invalid client ID"
        );
        Ok(issued.to_string())
    } else {
        anyhow::ensure!(
            valid_issued_chatgpt_client_id(pending_id),
            "Saved ChatGPT client ID is invalid; register again"
        );
        anyhow::ensure!(
            callback_id.is_none_or(|value| value == pending_id),
            "ChatGPT callback changed the selected registration; credentials were not replaced"
        );
        Ok(pending_id.to_string())
    }
}

fn registration_from_entry(entry: &OwnedAuthEntry) -> Result<ChatgptRegistration> {
    let registration: ChatgptRegistration =
        serde_json::from_value(entry.extra.get("siwc_registration").cloned().context(
            "Existing ChatGPT credentials need reauthorization; run `codewhale auth chatgpt`",
        )?)
        .context("Saved official ChatGPT registration is invalid; sign in again")?;
    anyhow::ensure!(
        registration.issuer == CHATGPT_OAUTH_ISSUER
            && entry.oidc_issuer.as_deref() == Some(registration.issuer.as_str())
            && entry.oidc_client_id.as_deref() == Some(registration.client_id.as_str())
            && entry.account_id.as_deref() == Some(registration.subject.as_str())
            && valid_issued_chatgpt_client_id(&registration.client_id)
            && !registration.subject.trim().is_empty(),
        "Saved ChatGPT grant does not match its verified registration; sign in again"
    );
    require_chatgpt_scopes(entry.extra.get("siwc_scope").and_then(Value::as_str))?;
    let access = entry
        .access_token
        .as_deref()
        .context("Saved ChatGPT access token is missing")?;
    let fingerprint = URL_SAFE_NO_PAD.encode(Sha256::digest(access.as_bytes()));
    anyhow::ensure!(
        entry.extra.get("siwc_token_sha256").and_then(Value::as_str) == Some(fingerprint.as_str()),
        "Saved ChatGPT token does not match its validated grant; sign in again"
    );
    Ok(registration)
}

/// Metadata only: no network, refresh, token return, or external credential import.
pub(crate) fn official_chatgpt_registration(config: &Config) -> Result<ChatgptRegistration> {
    let identity = config
        .builtin_provider_identity(OAuthProvider::Chatgpt.api())
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        config
            .provider_config_for(&identity)
            .and_then(|entry| entry.auth_mode.as_deref())
            == Some("oauth"),
        "Sign in with ChatGPT using `codewhale auth chatgpt`; the selected route has no official OAuth grant"
    );
    let path = configured_owned_auth_file_path(OAuthProvider::Chatgpt, config)?
        .context("Sign in with ChatGPT using `codewhale auth chatgpt`")?;
    let mut file = load_owned_auth_file(&path)?.context("Saved ChatGPT credentials are missing")?;
    let (_, entry) = select_entry(OAuthProvider::Chatgpt, &mut file)
        .context("Saved ChatGPT credentials are missing")?;
    registration_from_entry(&entry)
}

impl OAuthProvider {
    /// The engine provider this OAuth row belongs to.
    #[must_use]
    pub fn api(self) -> crate::config::ProviderKind {
        match self {
            OAuthProvider::Xai => crate::config::ProviderKind::Xai,
            OAuthProvider::Chatgpt => crate::config::ProviderKind::OpenaiCodex,
            OAuthProvider::Claude => crate::config::ProviderKind::Anthropic,
        }
    }

    /// `[providers.<key>]` table this provider's auth state lives under.
    fn config_key(self) -> &'static str {
        codewhale_config::descriptors::compatibility_for_kind(self.api()).config_key
    }

    fn legacy_file_name(self) -> &'static str {
        match self {
            OAuthProvider::Xai => codewhale_config::LEGACY_XAI_OAUTH_FILE_NAME,
            OAuthProvider::Chatgpt => codewhale_config::LEGACY_CHATGPT_OAUTH_FILE_NAME,
            OAuthProvider::Claude => codewhale_config::LEGACY_CLAUDE_OAUTH_FILE_NAME,
        }
    }

    #[must_use]
    pub fn is_valid_generation(self, name: &str) -> bool {
        match self {
            OAuthProvider::Xai => codewhale_config::is_valid_xai_oauth_generation(name),
            OAuthProvider::Chatgpt => codewhale_config::is_valid_chatgpt_oauth_generation(name),
            OAuthProvider::Claude => codewhale_config::is_valid_claude_oauth_generation(name),
        }
    }

    fn validate_generation(self, name: &str) -> Result<()> {
        match self {
            OAuthProvider::Xai => codewhale_config::validate_xai_oauth_generation(name),
            OAuthProvider::Chatgpt => codewhale_config::validate_chatgpt_oauth_generation(name),
            OAuthProvider::Claude => codewhale_config::validate_claude_oauth_generation(name),
        }
        .map(|_| ())
    }

    fn generation_path(self, name: &str) -> Result<PathBuf> {
        match self {
            OAuthProvider::Xai => codewhale_config::xai_oauth_generation_path(name),
            OAuthProvider::Chatgpt => codewhale_config::chatgpt_oauth_generation_path(name),
            OAuthProvider::Claude => codewhale_config::claude_oauth_generation_path(name),
        }
    }

    /// A fresh, uniquely named generation file name for this provider.
    fn new_generation(self) -> String {
        let (prefix, suffix) = match self {
            OAuthProvider::Xai => (
                codewhale_config::XAI_OAUTH_GENERATION_PREFIX,
                codewhale_config::XAI_OAUTH_GENERATION_SUFFIX,
            ),
            OAuthProvider::Claude => (
                codewhale_config::CLAUDE_OAUTH_GENERATION_PREFIX,
                codewhale_config::CLAUDE_OAUTH_GENERATION_SUFFIX,
            ),
            OAuthProvider::Chatgpt => (
                codewhale_config::CHATGPT_OAUTH_GENERATION_PREFIX,
                codewhale_config::CHATGPT_OAUTH_GENERATION_SUFFIX,
            ),
        };
        format!("{prefix}{}{suffix}", uuid::Uuid::new_v4().simple())
    }
}

/// One entry in a Codewhale-owned auth generation. Reads both historical
/// on-disk shapes; `key` was the xAI/Grok field name for the access token.
#[derive(Clone, Serialize, Deserialize)]
pub struct OwnedAuthEntry {
    #[serde(default, alias = "key", skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oidc_issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oidc_client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub originator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl std::fmt::Debug for OwnedAuthEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedAuthEntry")
            .field("access_token", &redacted(self.access_token.is_some()))
            .field("refresh_token", &redacted(self.refresh_token.is_some()))
            .field("expires_at", &self.expires_at)
            .field("id_token", &redacted(self.id_token.is_some()))
            .field("account_id", &self.account_id)
            .field("oidc_issuer", &self.oidc_issuer)
            .field("oidc_client_id", &self.oidc_client_id)
            .field("originator", &self.originator)
            .field("auth_mode", &self.auth_mode)
            .field("extra_keys", &self.extra.keys().collect::<Vec<_>>())
            .finish()
    }
}

fn redacted(present: bool) -> &'static str {
    if present { "<redacted>" } else { "<none>" }
}

/// Resolved owned credentials ready for API use. No Debug: bearer material
/// never prints; consumers redact explicitly.
#[derive(Clone)]
pub struct OwnedOAuthCredentials {
    pub access_token: String,
    pub account_id: Option<String>,
    /// Display label (email, plan) of the entry these credentials came from.
    pub account_label: Option<String>,
    #[allow(dead_code, reason = "read by provider routes and tests as needed")]
    pub refresh_token: Option<String>,
    #[allow(dead_code, reason = "diagnostic surface only")]
    pub expires_at: Option<String>,
    #[allow(dead_code, reason = "route provenance only")]
    pub issuer: String,
    #[allow(dead_code, reason = "route provenance only")]
    pub client_id: String,
}

/// Receipt for a committed Codewhale-owned OAuth generation.
pub struct OAuthActivation {
    #[expect(dead_code)]
    pub credentials: OwnedOAuthCredentials,
    pub config_path: PathBuf,
    pub auth_path: PathBuf,
    pub provider: OAuthProvider,
    /// Non-secret label (email, plan) of the account just signed in.
    pub account_label: Option<String>,
    /// `Some` when this login replaced an existing Codewhale-owned sign-in;
    /// the inner value is that account's label when it had one.
    pub replaced: Option<Option<String>>,
}

impl OAuthActivation {
    /// "Signed in to ChatGPT as you@example.com (plus)." plus, on its own
    /// line, which sign-in this login replaced. Never token material. Shell
    /// commands pass [`Locale::En`](codewhale_localization::Locale::En); the
    /// TUI passes its UI locale.
    #[must_use]
    pub fn summary(&self, locale: codewhale_localization::Locale) -> String {
        use codewhale_localization::{MessageId, tr};
        let name = oauth_provider_params(self.provider).display_name;
        let signed_in = match self.account_label.as_deref() {
            Some(label) => tr(locale, MessageId::AuthSignedInAs)
                .replace("{provider}", name)
                .replace("{account}", label),
            None => tr(locale, MessageId::AuthSignedInWithoutEmail).replace("{provider}", name),
        };
        let replaced = match &self.replaced {
            Some(Some(previous)) if Some(previous) != self.account_label.as_ref() => Some(
                tr(locale, MessageId::AuthReplacedPreviousSignInAs)
                    .replace("{provider}", name)
                    .replace("{account}", previous),
            ),
            Some(Some(_)) => {
                Some(tr(locale, MessageId::AuthSameAccountAsBefore).replace("{provider}", name))
            }
            Some(None) => {
                Some(tr(locale, MessageId::AuthReplacedPreviousSignIn).replace("{provider}", name))
            }
            None => None,
        };
        match replaced {
            Some(replaced) => format!("{signed_in}\n{replaced}"),
            None => signed_in,
        }
    }

    /// Official ChatGPT sign-in never uses an ambient process token.
    #[must_use]
    pub fn env_override_warning(&self, _locale: codewhale_localization::Locale) -> Option<String> {
        None
    }
}

impl std::fmt::Debug for OAuthActivation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthActivation")
            .field("credentials", &redacted(true))
            .field("config_path", &self.config_path)
            .field("auth_path", &self.auth_path)
            .field("provider", &self.provider)
            .field("account_label", &self.account_label)
            .field("replaced", &self.replaced)
            .finish()
    }
}

type AuthFile = BTreeMap<String, OwnedAuthEntry>;

fn load_owned_auth_file(path: &Path) -> Result<Option<AuthFile>> {
    let Some(raw) = crate::external_credentials::read_codewhale_owned_to_string(path)? else {
        return Ok(None);
    };
    parse_auth_file(&raw, path).map(Some)
}

fn load_owned_auth_file_from_store(
    store: &codewhale_config::XaiOAuthCredentialStore,
    name: &str,
) -> Result<Option<AuthFile>> {
    let Some(raw) = store.read_to_string(name)? else {
        return Ok(None);
    };
    parse_auth_file(&raw, &store.path_for(name)?).map(Some)
}

fn parse_auth_file(raw: &str, path: &Path) -> Result<AuthFile> {
    let value: Value = serde_json::from_str(raw).map_err(|_| {
        anyhow::anyhow!(
            "credential file {} is not valid credential JSON",
            codewhale_config::quote_os_path(path)
        )
    })?;
    let obj = value.as_object().ok_or_else(|| {
        anyhow::anyhow!(
            "credential file {} must be a JSON object of entries",
            codewhale_config::quote_os_path(path)
        )
    })?;
    let mut out = BTreeMap::new();
    for (k, v) in obj {
        match serde_json::from_value::<OwnedAuthEntry>(v.clone()) {
            Ok(entry) => {
                out.insert(k.clone(), entry);
            }
            Err(_) => {
                tracing::warn!(
                    target: "codewhale::oauth",
                    "skipping unreadable owned auth entry"
                );
            }
        }
    }
    Ok(out)
}

fn write_auth_file_to_store(
    store: &codewhale_config::XaiOAuthCredentialStore,
    name: &str,
    file: &AuthFile,
    allow_replace: bool,
) -> Result<()> {
    let serialized =
        serde_json::to_vec_pretty(file).context("serializing owned OAuth credentials")?;
    store
        .write(name, &serialized, allow_replace)
        .with_context(|| {
            format!(
                "writing owned OAuth credentials to {}",
                codewhale_config::quote_os_path(&store.directory().join(name))
            )
        })?;
    #[cfg(test)]
    crate::external_credentials::record_owned_credential_write();
    Ok(())
}

/// Read-only parse of another CLI's granted credential file.
fn load_external_auth_file(
    grant: &codewhale_config::ExternalCredentialReadGrant,
) -> Result<AuthFile> {
    let Some(raw) = crate::external_credentials::read_to_string(grant)? else {
        bail!(
            "external credential file not found at {}",
            codewhale_config::quote_os_path(grant.path())
        );
    };
    parse_auth_file(&raw, grant.path())
}

fn select_entry(provider: OAuthProvider, file: &mut AuthFile) -> Option<(String, OwnedAuthEntry)> {
    if provider == OAuthProvider::Chatgpt {
        let mut official = file
            .iter()
            .filter(|(_, entry)| registration_from_entry(entry).is_ok());
        if let Some((scope, entry)) = official.next() {
            // Ambiguous profile selection fails closed. Login writes one account.
            return official
                .next()
                .is_none()
                .then(|| (scope.clone(), entry.clone()));
        }
    }
    // Prefer this provider's registered client-id scope when present.
    let preferred_suffix = format!("::{}", oauth_provider_params(provider).default_client_id);
    if let Some((k, v)) = file
        .iter()
        .find(|(k, e)| k.ends_with(&preferred_suffix) && entry_has_usable_secret(e))
    {
        return Some((k.clone(), v.clone()));
    }
    file.iter()
        .find(|(_, e)| entry_has_usable_secret(e))
        .map(|(k, v)| (k.clone(), v.clone()))
}

fn entry_has_usable_secret(entry: &OwnedAuthEntry) -> bool {
    entry
        .access_token
        .as_deref()
        .is_some_and(|t| !t.trim().is_empty())
        || entry
            .refresh_token
            .as_deref()
            .is_some_and(|t| !t.trim().is_empty())
}

fn chatgpt_refresh_not_before(value: &Value) -> Result<i64> {
    value
        .as_i64()
        .or_else(|| {
            value.as_str().and_then(|value| {
                value
                    .parse::<i64>()
                    .ok()
                    .or_else(|| parse_rfc3339_secs(value))
            })
        })
        .filter(|value| *value >= 0)
        .context("ChatGPT earliest refresh time is invalid; sign in again")
}

const REFRESH_SKEW_SECS: i64 = 60;

fn entry_access_token_is_fresh(entry: &OwnedAuthEntry) -> bool {
    let Some(token) = entry
        .access_token
        .as_deref()
        .filter(|t| !t.trim().is_empty())
    else {
        return false;
    };
    let stored_expiry = entry.expires_at.as_deref().and_then(parse_rfc3339_secs);
    let token_expiry = jwt_expiry_seconds(token).and_then(|exp| i64::try_from(exp).ok());
    // A later stored expiry must not hide an already-expired access token.
    // Opaque tokens still use stored expiry; no known expiry remains stale.
    let now = now_unix_secs().unwrap_or(0);
    let refresh_blocked = entry
        .extra
        .get("siwc_earliest_refresh_at")
        .and_then(|value| chatgpt_refresh_not_before(value).ok())
        .is_some_and(|earliest| earliest > now);
    stored_expiry
        .into_iter()
        .chain(token_expiry)
        .min()
        .is_some_and(|exp| {
            exp.saturating_sub(now) > REFRESH_SKEW_SECS || (refresh_blocked && exp > now)
        })
}

fn credentials_from_entry(
    provider: OAuthProvider,
    scope: &str,
    entry: &OwnedAuthEntry,
    access_token: String,
) -> OwnedOAuthCredentials {
    OwnedOAuthCredentials {
        access_token,
        account_id: entry.account_id.clone(),
        account_label: entry.account_label(),
        refresh_token: entry.refresh_token.clone(),
        expires_at: entry.expires_at.clone(),
        issuer: entry
            .oidc_issuer
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| issuer_from_scope(provider, scope)),
        client_id: entry
            .oidc_client_id
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| client_id_from_scope(provider, scope)),
    }
}

fn issuer_from_scope(provider: OAuthProvider, scope: &str) -> String {
    scope
        .split_once("::")
        .map(|(issuer, _)| issuer.to_string())
        .unwrap_or_else(|| oauth_provider_params(provider).default_issuer.to_string())
}

fn client_id_from_scope(provider: OAuthProvider, scope: &str) -> String {
    scope
        .split_once("::")
        .map(|(_, id)| id.to_string())
        .unwrap_or_else(|| {
            oauth_provider_params(provider)
                .default_client_id
                .to_string()
        })
}

fn apply_token_response(
    provider: OAuthProvider,
    entry: &mut OwnedAuthEntry,
    issuer: &str,
    client_id: &str,
    token: &OAuthTokenMaterial,
) -> Result<()> {
    if provider == OAuthProvider::Chatgpt {
        let registration = token
            .verified_chatgpt
            .as_ref()
            .context("ChatGPT credentials were not cryptographically verified; sign in again")?;
        anyhow::ensure!(
            registration.issuer == issuer && registration.client_id == client_id,
            "Verified ChatGPT registration does not match the token exchange"
        );
        require_chatgpt_scopes(token.scope.as_deref())?;
        entry.extra.insert(
            "siwc_registration".to_string(),
            serde_json::to_value(registration)?,
        );
        entry.extra.insert(
            "siwc_scope".to_string(),
            Value::String(token.scope.clone().unwrap_or_default()),
        );
        let access = token
            .access_token
            .as_deref()
            .context("token response missing access_token")?;
        entry.extra.insert(
            "siwc_token_sha256".to_string(),
            Value::String(URL_SAFE_NO_PAD.encode(Sha256::digest(access.as_bytes()))),
        );
        if let Some(value) = &token.earliest_refresh_at {
            let earliest = chatgpt_refresh_not_before(value)?;
            entry.extra.insert(
                "siwc_earliest_refresh_at".to_string(),
                Value::from(earliest),
            );
        } else {
            entry.extra.remove("siwc_earliest_refresh_at");
        }
        entry.account_id = Some(registration.subject.clone());
    }
    if provider == OAuthProvider::Claude {
        anyhow::ensure!(
            token
                .expires_in
                .is_some_and(|seconds| seconds > 60 && seconds < 31_536_000),
            "Claude token response omitted a usable expiration"
        );
        anyhow::ensure!(
            token
                .refresh_token
                .as_deref()
                .is_some_and(|token| !token.trim().is_empty()),
            "Claude token response omitted its rotating refresh token"
        );
    }
    let access = token
        .access_token
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .context("token response missing access_token")?;
    entry.access_token = Some(access.to_string());
    if let Some(rt) = token
        .refresh_token
        .as_deref()
        .filter(|t| !t.trim().is_empty())
    {
        entry.refresh_token = Some(rt.to_string());
    }
    entry.oidc_issuer = Some(issuer.to_string());
    entry.oidc_client_id = Some(client_id.to_string());
    entry.auth_mode = Some("oidc".to_string());
    entry.originator = oauth_provider_params(provider)
        .originator
        .map(ToOwned::to_owned);
    if let Some(id_token) = token.id_token.clone() {
        if provider != OAuthProvider::Chatgpt
            && let Some(account_id) = account_id_from_id_token(&id_token)
        {
            entry.account_id = Some(account_id);
        }
        entry.id_token = Some(id_token);
    }
    if let Some(expires_in) = token.expires_in {
        entry.expires_at = Some(rfc3339_from_now(expires_in));
    } else if let Some(exp) = jwt_expiry_seconds(access) {
        entry.expires_at = Some(rfc3339_from_unix(exp as i64));
    }
    Ok(())
}

fn account_id_from_id_token(token: &str) -> Option<String> {
    let payload = jwt_payload(token)?;
    if let Some(id) = payload.get("chatgpt_account_id").and_then(Value::as_str) {
        let trimmed = id.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    payload
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
}

/// Longest email the label keeps (RFC 5321 path limit); plan names are short.
const ACCOUNT_EMAIL_MAX_CHARS: usize = 254;
const ACCOUNT_PLAN_MAX_CHARS: usize = 32;
/// Leading characters of a ChatGPT account id shown to tell two workspaces
/// on one email apart.
const ACCOUNT_ID_PREFIX_CHARS: usize = 8;
/// ChatGPT plans that belong to one person, where email and plan already
/// identify the account. Any other plan (team, business, enterprise, edu,
/// or one this list has not seen) is a workspace a person may hold several
/// of under one email, so its label carries an account-id prefix.
const PERSONAL_CHATGPT_PLANS: &[&str] = &["free", "go", "plus", "pro"];

/// Non-secret account label from an ID token's claims, decoded locally:
/// `email`, `email (plan)`, or for a workspace plan
/// `email (plan, workspace 1a2b3c4d)`. The signature is not verified: the
/// label is for display only and never authorizes anything. Claim text is
/// bounded and stripped of control characters so a hostile token cannot
/// smuggle terminal escapes into status output. `None` when no usable email
/// claim exists.
#[must_use]
pub fn account_label_from_id_token(token: &str) -> Option<String> {
    let payload = jwt_payload(token)?;
    let claim_text = |value: Option<&Value>, max: usize| {
        let text: String = value?
            .as_str()?
            .chars()
            .filter(|ch| !ch.is_control())
            .take(max)
            .collect();
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_string())
    };
    let email = claim_text(payload.get("email"), ACCOUNT_EMAIL_MAX_CHARS).or_else(|| {
        claim_text(
            payload
                .get("https://api.openai.com/profile")
                .and_then(|profile| profile.get("email")),
            ACCOUNT_EMAIL_MAX_CHARS,
        )
    })?;
    let plan = claim_text(
        payload
            .get("https://api.openai.com/auth")
            .and_then(|auth| auth.get("chatgpt_plan_type")),
        ACCOUNT_PLAN_MAX_CHARS,
    );
    let workspace = plan
        .as_deref()
        .filter(|plan| {
            !PERSONAL_CHATGPT_PLANS
                .iter()
                .any(|personal| plan.eq_ignore_ascii_case(personal))
        })
        .and_then(|_| account_id_from_id_token(token))
        .map(|id| {
            id.chars()
                .filter(char::is_ascii_alphanumeric)
                .take(ACCOUNT_ID_PREFIX_CHARS)
                .collect::<String>()
        })
        .filter(|prefix| !prefix.is_empty());
    Some(match (plan, workspace) {
        (Some(plan), Some(workspace)) => format!("{email} ({plan}, workspace {workspace})"),
        (Some(plan), None) => format!("{email} ({plan})"),
        (None, _) => email,
    })
}

impl OwnedAuthEntry {
    /// Display label for the account this entry signs in as.
    fn account_label(&self) -> Option<String> {
        self.id_token
            .as_deref()
            .and_then(account_label_from_id_token)
    }
}

/// Whether an owned entry can still produce a bearer: a fresh access token,
/// or a refresh token to mint one. The same test gates [`credentials_valid`]
/// and the account label, so a label is never shown for an entry the
/// runtime would refuse.
fn owned_entry_is_usable(entry: &OwnedAuthEntry) -> bool {
    entry_access_token_is_fresh(entry)
        || entry
            .refresh_token
            .as_deref()
            .is_some_and(|token| !token.trim().is_empty())
}

/// Account label of the Codewhale-owned sign-in stored in one generation
/// file. Reads only that file; never refreshes, writes, or touches the
/// network. `Ok(None)` when the usable entry's ID token carries no email;
/// `Err` (a fixed, token-free reason) when the generation name is invalid,
/// the file is missing or unreadable, or it holds no usable sign-in.
pub fn owned_account_label_for_generation(
    provider: OAuthProvider,
    generation: &str,
) -> Result<Option<String>> {
    let path = provider
        .generation_path(generation)
        .map_err(|_| anyhow::anyhow!("invalid generation pointer"))?;
    owned_account_label_at(provider, &path)
}

fn owned_account_label_at(provider: OAuthProvider, path: &Path) -> Result<Option<String>> {
    let mut file = load_owned_auth_file(path)
        .map_err(|_| anyhow::anyhow!("sign-in file is unreadable"))?
        .ok_or_else(|| anyhow::anyhow!("sign-in file is missing"))?;
    let (_, entry) = select_entry(provider, &mut file)
        .filter(|(_, entry)| {
            (provider != OAuthProvider::Chatgpt || registration_from_entry(entry).is_ok())
                && owned_entry_is_usable(entry)
        })
        .ok_or_else(|| anyhow::anyhow!("sign-in file holds no usable sign-in"))?;
    Ok(entry.account_label())
}

/// What to tell someone whose subscription sign-in hit a plan limit: which
/// account made the request (label only, never a token) and how to sign in
/// with a different one, both inside a running session and from a shell.
/// A shell login does not reach a session that is already open, so the
/// shell route says to restart.
#[must_use]
pub fn usage_limit_guidance(provider: OAuthProvider, account_label: Option<&str>) -> String {
    let params = oauth_provider_params(provider);
    let name = params.display_name;
    let switch = if provider == OAuthProvider::Chatgpt {
        "To continue with a different ChatGPT account, run `CODEWHALE_CHATGPT_NEW_ACCOUNT=1 codewhale auth chatgpt` in a shell and then restart open Codewhale sessions. `/auth chatgpt` reauthorizes the selected account.".to_string()
    } else {
        format!(
            "To continue with a different {name} account, run `{}` in Codewhale, or `{}` in a shell and then restart open Codewhale sessions.",
            params.session_login_hint, params.relogin_hint
        )
    };
    match account_label {
        Some(label) => format!("This request used the {name} account {label}. {switch}"),
        None => switch,
    }
}

/// Printed beside a login URL: the issuer signs in whichever account the
/// browser already holds unless the user picks another.
fn account_choice_hint(display_name: &str) -> String {
    if display_name == "ChatGPT" {
        return "Returning sign-in must use the selected ChatGPT account. To replace it, start a new login with `CODEWHALE_CHATGPT_NEW_ACCOUNT=1 codewhale auth chatgpt`. If the browser shows a different account, sign out there first or open the login URL in a private window.".to_string();
    }
    format!(
        "Approve with the {display_name} account Codewhale should use. If the page is already \
         signed in to a different account, sign out there first or open the URL above in a \
         private window."
    )
}

fn jwt_payload(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&decoded).ok()
}

fn now_unix_secs() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
}

fn parse_rfc3339_secs(raw: &str) -> Option<i64> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(dt.timestamp());
    }
    // Simple UTC forms chrono's strict parser rejects, e.g. a missing
    // fractional second on an offset-less timestamp.
    let trimmed = raw.trim().trim_end_matches('Z');
    let (date, time) = trimmed.split_once('T')?;
    let mut d = date.split('-');
    let y: i32 = d.next()?.parse().ok()?;
    let m: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    let time = time.split('+').next()?.split('-').next()?;
    let mut t = time.split(':');
    let hh: u32 = t.next()?.parse().ok()?;
    let mm: u32 = t.next()?.parse().ok()?;
    let ss: u32 = t
        .next()
        .and_then(|s| s.split('.').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let ndt = chrono::NaiveDate::from_ymd_opt(y, m, day)?.and_hms_opt(hh, mm, ss)?;
    Some(ndt.and_utc().timestamp())
}

fn rfc3339_from_now(expires_in: u64) -> String {
    let ts = now_unix_secs().unwrap_or(0) + expires_in as i64;
    rfc3339_from_unix(ts)
}

fn rfc3339_from_unix(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(|| format!("{ts}"))
}

/// Refresh through the seam, resolving the token endpoint the provider's
/// row demands (discovered for xAI, pinned for ChatGPT).
fn refresh_for_provider(
    provider: OAuthProvider,
    client: &dyn OAuthFormClient,
    issuer: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<OAuthTokenMaterial> {
    let params = oauth_provider_params(provider);
    let token_url = if params.discover_endpoints {
        resolve_oauth_endpoints(params, issuer).token_endpoint
    } else {
        form_token_url(params, issuer)
    };
    refresh_access_token_via(client, params, &token_url, client_id, refresh_token)
}

fn configured_owned_auth_file_path(
    provider: OAuthProvider,
    config: &Config,
) -> Result<Option<PathBuf>> {
    let identity = config
        .builtin_provider_identity(provider.api())
        .map_err(anyhow::Error::msg)?;
    let generation = config
        .provider_config_for(&identity)
        .and_then(|entry| entry.oauth_credential_generation.as_deref());
    match generation {
        Some(generation) => provider.generation_path(generation).map(Some),
        None => Ok(None),
    }
}

/// A subscription sign-in the structural check found usable, named by the
/// same read that proved it usable. Holds a display label only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsableSignIn {
    /// Email (and plan) of the account that entry signs in as; `None` when
    /// its ID token carries no email claim.
    pub account_label: Option<String>,
}

/// Prompt-free structural check for owned OAuth material. Never refreshes,
/// writes, or makes network requests. External storage is not inspected
/// until exact read-only consent has been persisted, and then only at the
/// exact consented path — no ambient candidate is ever resolved or opened
/// (#5772).
#[must_use]
pub fn credentials_valid(provider: OAuthProvider, config: &Config) -> bool {
    usable_sign_in(provider, config).is_some()
}

/// [`credentials_valid`], also naming the account of the entry that passed:
/// the configured Codewhale-owned generation, else (xAI) the legacy owned
/// file or the consented Grok CLI import — the credential the route would
/// send. Readiness surfaces take the label from here so naming the account
/// costs no second credential read.
#[must_use]
pub fn usable_sign_in(provider: OAuthProvider, config: &Config) -> Option<UsableSignIn> {
    let identity = config.builtin_provider_identity(provider.api()).ok()?;
    let usable = |entry: &OwnedAuthEntry| UsableSignIn {
        account_label: entry.account_label(),
    };
    // Codewhale-owned OAuth bytes are inert until the provider route
    // explicitly selects OAuth. A failed post-login config finalization can
    // therefore never make a newly written token silently ready on the next
    // launch.
    if matches!(provider, OAuthProvider::Chatgpt | OAuthProvider::Claude)
        && config
            .provider_config_for(&identity)
            .and_then(|entry| entry.auth_mode.as_deref())
            != Some("oauth")
    {
        return None;
    }
    if provider == OAuthProvider::Xai
        && !config
            .provider_config_for(&identity)
            .and_then(|entry| entry.auth_mode.as_deref())
            .is_some_and(auth_mode_uses_xai_oauth)
    {
        return None;
    }
    if let Ok(Some(path)) = configured_owned_auth_file_path(provider, config)
        && let Ok(Some(mut file)) = load_owned_auth_file(&path)
        && let Some((_, entry)) = select_entry(provider, &mut file)
        && (provider != OAuthProvider::Chatgpt || registration_from_entry(&entry).is_ok())
        && (provider != OAuthProvider::Claude
            || (entry.oidc_issuer.as_deref() == Some(CLAUDE_OAUTH_PARAMS.default_issuer)
                && entry.oidc_client_id.as_deref() == Some(CLAUDE_OAUTH_PARAMS.default_client_id)))
        && owned_entry_is_usable(&entry)
    {
        return Some(usable(&entry));
    }
    if config
        .provider_config_for(&identity)
        .and_then(|entry| entry.oauth_credential_generation.as_deref())
        .is_some()
    {
        // A configured generation is authoritative. Invalid, missing, unsafe,
        // or malformed owned storage must not fall through to an external CLI.
        return None;
    }
    if provider == OAuthProvider::Xai {
        // The pre-generation legacy file is the last owned location.
        if let Ok(path) = codewhale_config::legacy_xai_oauth_path()
            && let Ok(Some(mut file)) = load_owned_auth_file(&path)
            && let Some((_, entry)) = select_entry(provider, &mut file)
            && owned_entry_is_usable(&entry)
        {
            return Some(usable(&entry));
        }
        // #5772: with no persisted consent record there is no external path
        // to resolve and nothing to open.
        if let Some(consent_path) = config
            .provider_config_for(&identity)
            .and_then(|entry| entry.external_credentials.as_ref())
            .map(|consent| consent.path.clone())
            && let Ok(grant) = config.external_credential_read_grant(
                &identity,
                codewhale_config::ExternalCredentialSource::GrokCli,
                &consent_path,
            )
            && let Ok(mut file) = load_external_auth_file(&grant)
        {
            return select_entry(provider, &mut file)
                .filter(|(_, entry)| entry_access_token_is_fresh(entry))
                .map(|(_, entry)| usable(&entry));
        }
    }
    None
}

#[must_use]
pub fn credentials_present(provider: OAuthProvider, config: &Config) -> bool {
    credentials_valid(provider, config)
}

/// Grant-time validation for an external Grok CLI credential file (#5772).
///
/// Reads exactly the granted path through the secure adapter and requires a
/// usable, unexpired entry. Never refreshes, rewrites, or makes a network
/// request. Consent is persisted only after this succeeds, so a consent
/// record can never be written for a file that holds nothing usable.
pub fn validate_grok_external_credentials(
    grant: &codewhale_config::ExternalCredentialReadGrant,
) -> Result<()> {
    let mut file = load_external_auth_file(grant)?;
    let (_, entry) = select_entry(OAuthProvider::Xai, &mut file).ok_or_else(|| {
        anyhow::anyhow!(
            "xAI OAuth credentials at {} have no usable entry. Run `grok login` again or use `codewhale auth xai-device` for Codewhale-owned storage.",
            codewhale_config::quote_os_path(grant.path())
        )
    })?;
    if !entry_access_token_is_fresh(&entry) {
        bail!(
            "xAI OAuth access token in {} is expired. Read-only consent never refreshes or rewrites another CLI's credentials. Run `grok login` again or use `codewhale auth xai-device`.",
            codewhale_config::quote_os_path(grant.path())
        );
    }
    Ok(())
}

/// Load xAI OAuth credentials with full precedence: configured generation,
/// legacy owned file, then the consented Grok CLI import. Codewhale-owned
/// credentials may refresh and rewrite Codewhale-owned storage; external
/// credentials are read-only.
pub fn get_xai_credentials(config: &Config) -> Result<OwnedOAuthCredentials> {
    let identity = config
        .active_provider_identity()
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        identity.provider == crate::config::ProviderKind::Xai
            && identity.key.as_str() == crate::config::ProviderKind::Xai.as_str()
            && config
                .provider_config_for(&identity)
                .and_then(|entry| entry.auth_mode.as_deref())
                .is_some_and(auth_mode_uses_xai_oauth),
        "Codewhale-owned xAI OAuth credentials are inactive until the xAI route explicitly selects OAuth"
    );
    if let Some(owned_path) = configured_owned_auth_file_path(OAuthProvider::Xai, config)? {
        return get_owned_credentials_at(OAuthProvider::Xai, &owned_path);
    }
    let owned_path = codewhale_config::legacy_xai_oauth_path()?;
    if load_owned_auth_file(&owned_path)?.is_some() {
        return get_owned_credentials_at(OAuthProvider::Xai, &owned_path);
    }

    let external_path = grok_auth_file_path();
    let grant = config.external_credential_read_grant(
        &identity,
        codewhale_config::ExternalCredentialSource::GrokCli,
        &external_path,
    )?;
    let mut file = load_external_auth_file(&grant)?;
    let (scope, entry) = select_entry(OAuthProvider::Xai, &mut file).ok_or_else(|| {
        anyhow::anyhow!(
            "xAI OAuth credentials at {} have no usable entry. Run `grok login` again or use `codewhale auth xai-device` for Codewhale-owned storage.",
            codewhale_config::quote_os_path(grant.path())
        )
    })?;
    if !entry_access_token_is_fresh(&entry) {
        bail!(
            "xAI OAuth access token in {} is expired. Read-only consent never refreshes or rewrites another CLI's credentials. Run `grok login` again or use `codewhale auth xai-device`.",
            codewhale_config::quote_os_path(grant.path())
        );
    }
    let token = entry
        .access_token
        .clone()
        .filter(|token| !token.trim().is_empty())
        .context("xAI OAuth access token is empty")?;
    Ok(credentials_from_entry(
        OAuthProvider::Xai,
        &scope,
        &entry,
        token,
    ))
}

/// Load owned subscription credentials from the configured generation,
/// refreshing through the seam when stale.
pub fn get_owned_credentials(
    provider: OAuthProvider,
    config: &Config,
) -> Result<OwnedOAuthCredentials> {
    let identity = config
        .builtin_provider_identity(provider.api())
        .map_err(anyhow::Error::msg)?;
    if matches!(provider, OAuthProvider::Chatgpt | OAuthProvider::Claude) {
        anyhow::ensure!(
            config
                .provider_config_for(&identity)
                .and_then(|entry| entry.auth_mode.as_deref())
                == Some("oauth"),
            "Owned subscription credentials are inactive; sign in again"
        );
    }
    get_owned_credentials_with(provider, config, &ReqwestOAuthFormClient)
}

/// Read-only diagnostics: never refreshes or changes protected storage.
pub(crate) fn get_owned_credentials_read_only(
    provider: OAuthProvider,
    config: &Config,
) -> Result<OwnedOAuthCredentials> {
    anyhow::ensure!(
        matches!(provider, OAuthProvider::Chatgpt | OAuthProvider::Claude),
        "Read-only credentials require an owned subscription sign-in"
    );
    if provider == OAuthProvider::Chatgpt {
        official_chatgpt_registration(config)?;
    }
    let label = oauth_provider_params(provider).display_name;
    let path = configured_owned_auth_file_path(provider, config)?
        .with_context(|| format!("{label} credentials are not configured"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("Codewhale-owned OAuth path must have a UTF-8 basename")?;
    if provider == OAuthProvider::Claude {
        validate_saved_claude_generation(config, name, false)?;
    }
    let mut file =
        load_owned_auth_file(&path)?.with_context(|| format!("{label} credentials are missing"))?;
    let (scope, entry) = select_entry(provider, &mut file)
        .with_context(|| format!("{label} credentials are unavailable"))?;
    if provider == OAuthProvider::Chatgpt {
        registration_from_entry(&entry)?;
    }
    if provider == OAuthProvider::Claude {
        anyhow::ensure!(
            entry.oidc_issuer.as_deref() == Some(CLAUDE_OAUTH_PARAMS.default_issuer)
                && entry.oidc_client_id.as_deref() == Some(CLAUDE_OAUTH_PARAMS.default_client_id),
            "Claude credential issuer or client is invalid"
        );
    }
    anyhow::ensure!(
        entry_access_token_is_fresh(&entry),
        "{label} access token needs runtime refresh; read-only diagnostics do not refresh"
    );
    let access = entry
        .access_token
        .clone()
        .with_context(|| format!("{label} access token is missing"))?;
    if provider == OAuthProvider::Claude {
        validate_saved_claude_generation(config, name, false)?;
    }
    Ok(credentials_from_entry(provider, &scope, &entry, access))
}

fn get_owned_credentials_with(
    provider: OAuthProvider,
    config: &Config,
    client: &dyn OAuthFormClient,
) -> Result<OwnedOAuthCredentials> {
    let Some(path) = configured_owned_auth_file_path(provider, config)? else {
        bail!(
            "Codewhale-owned {} OAuth credentials are not configured",
            oauth_provider_params(provider).display_name
        );
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("Codewhale-owned OAuth path must have a UTF-8 basename")?;
    codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
        if provider == OAuthProvider::Claude {
            validate_saved_claude_generation(config, name, true)?;
        }
        let credentials = get_owned_credentials_locked(
            provider,
            store,
            name,
            client,
            |issuer, client_id, refresh| {
                refresh_for_provider(provider, client, issuer, client_id, refresh)
            },
        )?;
        if provider == OAuthProvider::Claude {
            validate_saved_claude_generation(config, name, true)?;
        }
        Ok(credentials)
    })
}

fn validate_saved_claude_generation(config: &Config, name: &str, locked: bool) -> Result<()> {
    let path = crate::config_persistence::config_toml_path(config.loaded_config_path.as_deref())?;
    let validate = |path: &Path| {
        let store = codewhale_config::ConfigStore::load(Some(path.to_path_buf()))
            .context("Could not verify current Claude sign-in configuration")?;
        let contents = store
            .original_body()
            .context("Current Claude sign-in configuration is missing")?;
        let saved = Config::from_saved_document(contents, config.account_profile.as_deref())
            .map_err(|_| anyhow::anyhow!("Current Claude sign-in configuration is invalid"))?;
        let identity = saved
            .builtin_provider_identity(ProviderKind::Anthropic)
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            saved.provider_config_for(&identity).is_some_and(|entry| {
                entry.auth_mode.as_deref() == Some("oauth")
                    && entry.oauth_credential_generation.as_deref() == Some(name)
            }),
            "Claude sign-in was removed or changed; sign in again"
        );
        Ok(())
    };
    if locked {
        codewhale_config::with_config_write_lock(&path, validate)
    } else {
        validate(&path)
    }
}

fn get_owned_credentials_at(provider: OAuthProvider, path: &Path) -> Result<OwnedOAuthCredentials> {
    let directory = codewhale_config::xai_oauth_credentials_dir()?;
    anyhow::ensure!(
        path.parent() == Some(directory.as_path()),
        "Codewhale-owned OAuth path escaped the credentials directory"
    );
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("Codewhale-owned OAuth path must have a UTF-8 basename")?;
    anyhow::ensure!(
        name == provider.legacy_file_name() || provider.is_valid_generation(name),
        "Codewhale-owned OAuth path has an invalid basename"
    );
    codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
        get_owned_credentials_locked(
            provider,
            store,
            name,
            &ReqwestOAuthFormClient,
            |issuer, client_id, refresh| {
                refresh_for_provider(
                    provider,
                    &ReqwestOAuthFormClient,
                    issuer,
                    client_id,
                    refresh,
                )
            },
        )
    })
}

fn get_owned_credentials_locked<F>(
    provider: OAuthProvider,
    store: &codewhale_config::XaiOAuthCredentialStore,
    name: &str,
    client: &dyn OAuthFormClient,
    refresh_access: F,
) -> Result<OwnedOAuthCredentials>
where
    F: FnOnce(&str, &str, &str) -> Result<OAuthTokenMaterial>,
{
    let hint = oauth_provider_params(provider).relogin_hint;
    let path = store.path_for(name)?;
    let mut file = load_owned_auth_file_from_store(store, name)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Codewhale-owned OAuth credentials were not found at {}. Run `{hint}` again.",
            codewhale_config::quote_os_path(&path)
        )
    })?;
    let (scope, mut entry) = select_entry(provider, &mut file).ok_or_else(|| {
        anyhow::anyhow!(
            "Codewhale-owned OAuth credentials at {} have no usable entry. Run `{hint}` again.",
            codewhale_config::quote_os_path(&path)
        )
    })?;
    let registration = if provider == OAuthProvider::Chatgpt {
        Some(registration_from_entry(&entry)?)
    } else {
        None
    };

    if provider == OAuthProvider::Claude {
        anyhow::ensure!(
            entry.oidc_issuer.as_deref() == Some(CLAUDE_OAUTH_PARAMS.default_issuer)
                && entry.oidc_client_id.as_deref() == Some(CLAUDE_OAUTH_PARAMS.default_client_id),
            "Claude credential issuer or client is invalid; sign in again"
        );
    }
    if entry_access_token_is_fresh(&entry) {
        let token = entry
            .access_token
            .clone()
            .filter(|t| !t.trim().is_empty())
            .context("OAuth access token is empty")?;
        return Ok(credentials_from_entry(provider, &scope, &entry, token));
    }

    let refresh = entry
        .refresh_token
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .context(format!(
            "OAuth access token expired and no refresh_token is stored. Run `{hint}` again."
        ))?;
    let issuer = entry
        .oidc_issuer
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| issuer_from_scope(provider, &scope));
    let client_id = entry
        .oidc_client_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| client_id_from_scope(provider, &scope));

    if let Some(value) = entry.extra.get("siwc_earliest_refresh_at") {
        anyhow::ensure!(
            chatgpt_refresh_not_before(value)?
                <= now_unix_secs().context("System clock is unavailable for ChatGPT refresh")?,
            "ChatGPT refresh window has not opened; retry later"
        );
    }
    let mut refreshed = refresh_access(&issuer, &client_id, refresh)?;
    if let Some(registration) = registration {
        require_chatgpt_scopes(refreshed.scope.as_deref())?;
        anyhow::ensure!(
            refreshed
                .token_type
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("bearer")),
            "ChatGPT refresh did not return Bearer credentials"
        );
        anyhow::ensure!(
            refreshed
                .refresh_token
                .as_deref()
                .is_some_and(|token| !token.trim().is_empty()),
            "ChatGPT refresh omitted its rotating refresh token; sign in again"
        );
        let keys = client.chatgpt_jwks(&issuer)?;
        verify_chatgpt_access_token(
            refreshed
                .access_token
                .as_deref()
                .context("ChatGPT refresh omitted its access token")?,
            &keys,
            &registration,
        )?;
        if let Some(id_token) = &refreshed.id_token {
            let refreshed_registration = verify_chatgpt_id_token(
                id_token,
                &keys,
                &issuer,
                &client_id,
                None,
                Some(&registration.subject),
                &registration.host_id,
            )?;
            refreshed.verified_chatgpt = Some(refreshed_registration);
        } else {
            refreshed.verified_chatgpt = Some(registration);
        }
    }
    apply_token_response(provider, &mut entry, &issuer, &client_id, &refreshed)?;
    file.insert(scope.clone(), entry.clone());
    write_auth_file_to_store(store, name, &file, true)?;

    let token = entry
        .access_token
        .clone()
        .filter(|t| !t.trim().is_empty())
        .context("OAuth refresh returned an empty access token")?;
    Ok(credentials_from_entry(provider, &scope, &entry, token))
}

/// Commit a pending login as a uniquely named owned generation and
/// atomically point the provider's config table at it under the shared
/// config lock.
///
/// The credential file is staged while the config lock is held. If config
/// persistence fails, the unreferenced stage is removed. Only after the new
/// pointer commits is the previously selected generation removed best-effort.
pub fn activate_login(
    pending: PendingOAuthLogin,
    config_path: Option<&Path>,
    live_config: Option<&mut Config>,
) -> Result<OAuthActivation> {
    codewhale_config::with_xai_oauth_lifecycle_lock(move |store| {
        activate_login_locked(pending, config_path, live_config, store)
    })
}

fn activate_login_locked(
    pending: PendingOAuthLogin,
    config_path: Option<&Path>,
    live_config: Option<&mut Config>,
    store: &codewhale_config::XaiOAuthCredentialStore,
) -> Result<OAuthActivation> {
    let provider = pending.provider;
    let display_name = oauth_provider_params(provider).display_name;
    let config_path = crate::config_persistence::config_toml_path(config_path)?;
    let generation = provider.new_generation();
    provider.validate_generation(&generation)?;
    let auth_path = store.path_for(&generation)?;
    let key_inside = provider.config_key();
    let mut stage_written = false;

    let activation = codewhale_config::mutate_config_document(&config_path, |document| {
        let previous_generation_item = document
            .get("providers")
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|providers| providers.get(key_inside))
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|provider| provider.get("oauth_credential_generation"));
        let previous_generation = previous_generation_item
            .map(|item| {
                item.as_str()
                    .context(format!(
                        "refusing {display_name} login because the existing credential generation pointer is not a string"
                    ))
                    .map(ToOwned::to_owned)
            })
            .transpose()?;
        if let Some(previous) = previous_generation.as_deref() {
            provider.validate_generation(previous).with_context(|| {
                format!(
                    "refusing {display_name} login because the existing credential generation pointer is invalid"
                )
            })?;
        }

        let previous_owned_name = match previous_generation.as_deref() {
            Some(previous) => Some(previous.to_string()),
            None if store.read_to_string(provider.legacy_file_name())?.is_some() => {
                Some(provider.legacy_file_name().to_string())
            }
            None => None,
        };
        // Read the previous generation only to name the account it signed
        // in as. A valid pointer whose file is gone (interrupted revocation,
        // external cleanup) must not brick login: only a successful
        // activation can ever rewrite the pointer, so treat the missing
        // generation like a fresh start instead of failing (#5032).
        let mut previous_file = match previous_owned_name.as_deref() {
            Some(name) => load_owned_auth_file_from_store(store, name)?.unwrap_or_else(|| {
                tracing::warn!(
                    target: "codewhale::oauth",
                    generation = name,
                    "config pointed at a missing owned OAuth generation; starting a fresh credential file"
                );
                BTreeMap::new()
            }),
            None => BTreeMap::new(),
        };
        let scope = format!("{}::{}", pending.issuer, pending.client_id);
        // A new login replaces the whole sign-in: the new generation holds
        // only this entry. Merging into the old entry would let a previous
        // account's refresh token, id token or account id survive whenever
        // the new grant omits one; carrying other scopes forward would let
        // an older account's entry under a differently spelled issuer
        // outrank this login in `select_entry` and keep its refresh token
        // on disk. `replaced` names the entry the runtime was actually using.
        let replaced =
            select_entry(provider, &mut previous_file).map(|(_, entry)| entry.account_label());
        let mut file = AuthFile::new();
        let mut entry = OwnedAuthEntry {
            access_token: None,
            refresh_token: None,
            expires_at: None,
            id_token: None,
            account_id: None,
            oidc_issuer: Some(pending.issuer.clone()),
            oidc_client_id: Some(pending.client_id.clone()),
            originator: None,
            auth_mode: Some("oidc".to_string()),
            extra: BTreeMap::new(),
        };
        apply_token_response(
            provider,
            &mut entry,
            &pending.issuer,
            &pending.client_id,
            &pending.token,
        )?;
        let access = entry
            .access_token
            .clone()
            .filter(|token| !token.trim().is_empty())
            .context(format!(
                "{display_name} login returned an empty access token"
            ))?;
        file.insert(scope.clone(), entry.clone());
        write_auth_file_to_store(store, &generation, &file, false)?;
        stage_written = true;

        codewhale_config::set_config_document_value(
            document,
            &["providers", key_inside, "auth_mode"],
            "oauth",
        )?;
        codewhale_config::set_config_document_value(
            document,
            &["providers", key_inside, "oauth_credential_generation"],
            generation.clone(),
        )?;
        codewhale_config::unset_config_document_value(
            document,
            &["providers", key_inside, "external_credentials"],
        )?;
        Ok((
            previous_owned_name,
            credentials_from_entry(provider, &scope, &entry, access),
            entry.account_label(),
            replaced,
        ))
    });

    let (previous_owned_name, credentials, account_label, replaced) = match activation {
        Ok(activation) => activation,
        Err(error) => {
            if stage_written && let Err(cleanup_error) = store.remove(&generation) {
                return Err(error).context(format!(
                    "{display_name} login was not activated; also failed to remove unreferenced staged credentials at {}: {cleanup_error}",
                    codewhale_config::quote_os_path(&auth_path)
                ));
            }
            return Err(error).context(format!(
                "{display_name} login was not activated; provider configuration is unchanged"
            ));
        }
    };
    if let Some(registration) = pending.token.verified_chatgpt.as_ref() {
        let host = ChatgptHost {
            host_id: registration.host_id.clone(),
            registration: Some(registration.clone()),
        };
        if let Err(error) = serde_json::to_vec(&host)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| store.write(codewhale_config::CHATGPT_HOST_FILE_NAME, &bytes, true))
        {
            tracing::warn!(target: "codewhale::oauth", error = %error, "ChatGPT grant activated; retaining its signed-out registration failed");
        }
    }

    if let Some(config) = live_config {
        match provider {
            OAuthProvider::Claude => {
                let identity = config
                    .builtin_provider_identity(provider.api())
                    .map_err(anyhow::Error::msg)?;
                let entry = config.provider_config_for_mut(&identity)?;
                entry.auth_mode = Some("oauth".to_string());
                entry.oauth_credential_generation = Some(generation.clone());
                entry.external_credentials = None;
            }
            OAuthProvider::Xai => config.mark_codewhale_owned_xai_oauth(generation.clone())?,
            OAuthProvider::Chatgpt => {
                config.mark_codewhale_owned_chatgpt_oauth(generation.clone())?;
            }
        }
    }
    if let Some(previous) = previous_owned_name
        && previous != generation
        && let Err(error) = store.remove(&previous)
    {
        tracing::warn!(
            target: "codewhale::oauth",
            error = %error,
            "new OAuth generation committed but superseded generation cleanup failed"
        );
    }
    Ok(OAuthActivation {
        credentials,
        config_path,
        auth_path,
        provider,
        account_label,
        replaced,
    })
}

/// Remove Codewhale-owned tokens and the config pointer for one provider.
///
/// ChatGPT's remote revoke is best-effort against the row's pinned revoke
/// path (see [`OAuthProviderParams::revoke_path`]): a failed or unreachable
/// revoke must never stop the local credentials from being removed. xAI
/// revokes locally only. External CLI consent is left untouched.
pub fn revoke_owned_login(
    provider: OAuthProvider,
    config_path: Option<&Path>,
    live_config: Option<&mut Config>,
) -> Result<()> {
    codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
        revoke_owned_login_locked(provider, config_path, live_config, store)
    })
}

fn revoke_owned_login_locked(
    provider: OAuthProvider,
    config_path: Option<&Path>,
    live_config: Option<&mut Config>,
    store: &codewhale_config::XaiOAuthCredentialStore,
) -> Result<()> {
    revoke_owned_login_locked_with(
        provider,
        config_path,
        live_config,
        store,
        &ReqwestOAuthFormClient,
    )
}

fn revoke_owned_login_locked_with(
    provider: OAuthProvider,
    config_path: Option<&Path>,
    live_config: Option<&mut Config>,
    store: &codewhale_config::XaiOAuthCredentialStore,
    client: &dyn OAuthFormClient,
) -> Result<()> {
    let config_path = crate::config_persistence::config_toml_path(config_path)?;
    let key_inside = provider.config_key();
    let previous = codewhale_config::mutate_config_document(&config_path, |document| {
        let previous = document
            .get("providers")
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|providers| providers.get(key_inside))
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|provider| provider.get("oauth_credential_generation"))
            .and_then(toml_edit::Item::as_str)
            .map(ToOwned::to_owned);
        codewhale_config::unset_config_document_value(
            document,
            &["providers", key_inside, "oauth_credential_generation"],
        )?;
        let auth_mode_is_oauth = document
            .get("providers")
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|providers| providers.get(key_inside))
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|provider| provider.get("auth_mode"))
            .and_then(toml_edit::Item::as_str)
            == Some("oauth");
        if auth_mode_is_oauth && provider != OAuthProvider::Claude {
            codewhale_config::unset_config_document_value(
                document,
                &["providers", key_inside, "auth_mode"],
            )?;
        }
        Ok(previous)
    })?;
    let live_config_clear = match live_config {
        Some(config) if provider == OAuthProvider::Claude => {
            let identity = config
                .builtin_provider_identity(provider.api())
                .map_err(anyhow::Error::msg)?;
            let entry = config.provider_config_for_mut(&identity)?;
            entry.oauth_credential_generation = None;
            Ok(())
        }
        Some(config) if provider == OAuthProvider::Chatgpt => {
            config.clear_codewhale_owned_chatgpt_oauth()
        }
        _ => Ok(()),
    };
    if provider == OAuthProvider::Claude {
        store.clear_claude().context(
            "Claude sign-in was disabled, but local credential cleanup failed; retry sign-out",
        )?;
        return live_config_clear
            .context("Signed out locally, but the live route could not be refreshed");
    }
    let names = match previous.as_deref() {
        Some(generation) if provider.is_valid_generation(generation) => {
            vec![generation.to_string()]
        }
        _ => vec![provider.legacy_file_name().to_string()],
    };
    let mut remote_unconfirmed = false;
    for name in names {
        if let Ok(Some(raw)) = store.read_to_string(&name)
            && let Ok(file) = parse_auth_file(&raw, &store.path_for(&name)?)
        {
            for entry in file.values() {
                if let Some(token) = entry
                    .refresh_token
                    .as_deref()
                    .or(entry.access_token.as_deref())
                    .filter(|token| !token.trim().is_empty())
                {
                    let issuer = entry
                        .oidc_issuer
                        .as_deref()
                        .unwrap_or_else(|| oauth_provider_params(provider).default_issuer);
                    let client_id = entry
                        .oidc_client_id
                        .as_deref()
                        .unwrap_or_else(|| oauth_provider_params(provider).default_client_id);
                    if let Err(error) = revoke_remote_token_via(
                        client,
                        oauth_provider_params(provider),
                        issuer,
                        client_id,
                        token,
                    ) {
                        remote_unconfirmed |= provider == OAuthProvider::Chatgpt;
                        tracing::warn!(
                            target: "codewhale::oauth",
                            error = %error,
                            "remote OAuth revoke failed; local credentials will still be removed"
                        );
                    }
                }
            }
        }
        store
            .remove(&name)
            .context("OAuth sign-out could not clear local credential storage")?;
    }
    // A refused live mirror must not leave durable tokens behind. Report it
    // only after local removal and preserve an unconfirmed remote outcome.
    if remote_unconfirmed {
        return live_config_clear.context(
            "Signed out locally, but the live route could not be refreshed and remote revocation was not confirmed.",
        ).and_then(|()| anyhow::bail!(
            "Signed out locally, but remote revocation was not confirmed. Disconnect Codewhale in ChatGPT Settings to end the renewable session."
        ));
    }
    live_config_clear.context("Signed out locally, but the live route could not be refreshed")?;
    Ok(())
}

/// Detect the [#5032] bricked-launch state: the provider's config selects
/// OAuth and points `oauth_credential_generation` at a Codewhale-owned
/// credential file that no longer exists. This is a distinct, more specific
/// failure than "unconfigured" — the pointer is present and authoritative,
/// so [`credentials_valid`] returns false and cannot fall through to a
/// legacy or external credential, which is exactly what bricked the
/// dogfood machine.
///
/// Returns false for any other state: OAuth not selected, no generation
/// configured, a malformed generation pointer (a different, already
/// fail-closed failure), or a generation whose owned file is present.
///
/// [#5032]: https://github.com/codewhale-hq/CodeWhale/issues/5032
#[must_use]
pub fn owned_generation_is_dangling(provider: OAuthProvider, config: &Config) -> bool {
    let Ok(identity) = config.builtin_provider_identity(provider.api()) else {
        return false;
    };
    if provider == OAuthProvider::Xai
        && !config
            .provider_config_for(&identity)
            .and_then(|entry| entry.auth_mode.as_deref())
            .is_some_and(auth_mode_uses_xai_oauth)
    {
        return false;
    }
    match configured_owned_auth_file_path(provider, config) {
        Ok(Some(path)) => !path.exists(),
        // `None` => no generation configured (not a dangling pointer). `Err` =>
        // the generation is malformed/invalid; that is a different, already
        // fail-closed failure, not the missing-file state this detects.
        _ => false,
    }
}

/// Best-effort repair for the [#5032] bricked-launch state: remove the stale
/// `oauth_credential_generation` pointer from the PERSISTED config file so
/// the next launch is no longer bricked. Mirrors the document edits in
/// [`activate_login_locked`] (which replaces the pointer under the config
/// lock) and [`crate::config::clear_api_key`]'s unlocked scrub.
///
/// Leaves `auth_mode = "oauth"` intact: the user still wants OAuth, they
/// simply need to re-authenticate. The launch-path caller must treat any
/// error as non-fatal — log a warning and continue. Returns `Ok(())` when
/// the stale pointer was removed (or was already absent).
///
/// [#5032]: https://github.com/codewhale-hq/CodeWhale/issues/5032
pub fn clear_dangling_generation(
    provider: OAuthProvider,
    config_path: Option<&Path>,
) -> Result<()> {
    let config_path = crate::config_persistence::config_toml_path(config_path)?;
    let key_inside = provider.config_key();
    codewhale_config::mutate_config_document(&config_path, |document| {
        codewhale_config::unset_config_document_value(
            document,
            &["providers", key_inside, "oauth_credential_generation"],
        )?;
        Ok(())
    })
}

/// Whether `[providers.xai] auth_mode` selects the OAuth path.
#[must_use]
pub fn auth_mode_uses_xai_oauth(mode: &str) -> bool {
    matches!(
        normalize_auth_mode(mode).as_str(),
        "oauth"
            | "xai_oauth"
            | "xai"
            | "grok"
            | "grok_oauth"
            | "grok_cli"
            | "device"
            | "device_code"
            | "device_auth"
    )
}

fn normalize_auth_mode(mode: &str) -> String {
    mode.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

/// Resolve the Grok CLI auth file path.
///
/// Priority:
/// 1. `GROK_AUTH_PATH` / `XAI_AUTH_PATH`
/// 2. `$GROK_HOME/auth.json`
/// 3. `~/.grok/auth.json`
#[must_use]
pub fn grok_auth_file_path() -> PathBuf {
    for key in ["GROK_AUTH_PATH", "XAI_AUTH_PATH"] {
        if let Ok(path) = std::env::var(key) {
            let p = PathBuf::from(path.trim());
            if !p.as_os_str().is_empty() {
                return codewhale_config::resolve_external_credential_path(&p).unwrap_or(p);
            }
        }
    }
    if let Ok(home) = std::env::var("GROK_HOME") {
        let p = PathBuf::from(home.trim());
        if !p.as_os_str().is_empty() {
            let path = p.join("auth.json");
            return codewhale_config::resolve_external_credential_path(&path).unwrap_or(path);
        }
    }
    let path = crate::config::effective_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".grok")
        .join("auth.json");
    codewhale_config::resolve_external_credential_path(&path).unwrap_or(path)
}

#[must_use]
pub fn missing_auth_message(provider: OAuthProvider) -> String {
    match provider {
        OAuthProvider::Claude => "Claude sign-in is unavailable. Run `codewhale auth claude`, or use an Anthropic API key with separate API billing.".to_string(),
        OAuthProvider::Xai => format!(
            "xAI OAuth credentials not found.\n\
             Options:\n\
             1. Run `codewhale auth xai-device` for Codewhale-owned OAuth storage\n\
             2. To read an existing Grok CLI login without changing it, run \
             `codewhale auth external-consent --provider xai --mode read-only --path {}`\n\
             3. Or use API-key auth: export XAI_API_KEY=... / \
             codewhale auth set --provider xai",
            codewhale_config::quote_os_path(&grok_auth_file_path())
        ),
        OAuthProvider::Chatgpt => "Official ChatGPT credentials are unavailable.
\
             Run `codewhale auth chatgpt` or /provider setup openai-codex.
\
             Authorize ChatGPT plan usage to use your plan allowance.
\
             Revoke Codewhale-owned tokens with `codewhale auth chatgpt-revoke`.
\
             The openai API-key route uses separate API billing."
            .to_string(),
    }
}

/// Declarative public OAuth configuration for a reviewed plugin provider.
// Plugin OAuth uses the same PKCE, callback, bounded HTTP and secure-store
// primitives as built-in logins. This declarative boundary intentionally does
// not execute plugin callbacks, expose refresh tokens, support confidential
// clients, or discover endpoints: plugins name reviewed, same-issuer endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginOAuthConfig {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub client_id: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default = "plugin_callback_path")]
    pub callback_path: String,
}

fn plugin_callback_path() -> String {
    "/oauth/callback".into()
}

impl PluginOAuthConfig {
    pub fn validate(&self) -> Result<()> {
        let issuer = oauth_endpoint_url(&self.issuer)?;
        for endpoint in [&self.authorization_endpoint, &self.token_endpoint] {
            let url = oauth_endpoint_url(endpoint)?;
            anyhow::ensure!(
                url.origin() == issuer.origin(),
                "Plugin OAuth endpoints must belong to the issuer origin"
            );
            anyhow::ensure!(
                url.query().is_none()
                    && url.fragment().is_none()
                    && url.username().is_empty()
                    && url.password().is_none(),
                "Plugin OAuth endpoint must not contain credentials, query or fragment"
            );
        }
        anyhow::ensure!(
            issuer.query().is_none()
                && issuer.fragment().is_none()
                && issuer.username().is_empty()
                && issuer.password().is_none(),
            "Plugin OAuth issuer must not contain credentials, query or fragment"
        );
        anyhow::ensure!(
            !self.client_id.trim().is_empty(),
            "Plugin OAuth client_id must not be empty"
        );
        anyhow::ensure!(
            self.callback_path.starts_with('/')
                && !self.callback_path.starts_with("//")
                && !self.callback_path.contains(['?', '#'])
                && !self.callback_path.chars().any(char::is_control),
            "Plugin OAuth callback_path must be an absolute path without query or fragment"
        );
        anyhow::ensure!(
            self.scopes
                .iter()
                .all(|scope| !scope.is_empty() && !scope.chars().any(char::is_whitespace)),
            "Plugin OAuth scopes must be nonempty individual scope names"
        );
        if let Some(resource) = &self.resource {
            let resource = oauth_endpoint_url(resource)?;
            anyhow::ensure!(
                resource.fragment().is_none()
                    && resource.username().is_empty()
                    && resource.password().is_none(),
                "Plugin OAuth resource must not contain credentials or a fragment"
            );
        }
        Ok(())
    }

    fn callback_params(&self) -> OAuthProviderParams<'_> {
        OAuthProviderParams {
            display_name: "Plugin provider",
            default_issuer: &self.issuer,
            default_client_id: &self.client_id,
            default_scopes: "",
            env: OAuthEnvOverrides {
                issuer_vars: &[],
                client_id_vars: &[],
                scope_vars: &[],
                no_browser_var: "CODEWHALE_PLUGIN_OAUTH_NO_BROWSER",
                no_account_prompt_var: None,
            },
            device_code_path: None,
            authorize_path: None,
            token_path: "",
            discover_endpoints: false,
            device_poll_floor_secs: 0,
            authorize_extras: &[],
            account_choice_extras: &[],
            originator: None,
            revoke_path: None,
            callback_path: &self.callback_path,
            loopback_ports: &[],
            relogin_hint: "codewhale auth plugin-login --provider <provider>",
            session_login_hint: "/login <provider>",
            callback_conflict_hint: "",
        }
    }

    fn authorize_url(
        &self,
        redirect_uri: &str,
        state: &str,
        pkce: &PkceChallenge,
    ) -> Result<String> {
        self.validate()?;
        let mut url = oauth_endpoint_url(&self.authorization_endpoint)?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", &self.scopes.join(" "))
            .append_pair("state", state)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256");
        if let Some(resource) = &self.resource {
            url.query_pairs_mut().append_pair("resource", resource);
        }
        Ok(url.into())
    }
}

fn validate_plugin_callback(query: &str, issuer: &str) -> Result<()> {
    let url = reqwest::Url::parse(&format!("http://127.0.0.1/?{query}"))?;
    let mut fields = BTreeMap::new();
    for (key, value) in url.query_pairs() {
        if matches!(
            key.as_ref(),
            "code" | "state" | "iss" | "error" | "error_description"
        ) {
            anyhow::ensure!(
                fields
                    .insert(key.into_owned(), value.into_owned())
                    .is_none(),
                "Plugin OAuth callback contains duplicate parameters"
            );
        }
    }
    anyhow::ensure!(
        fields.get("state").is_some_and(|state| !state.is_empty()),
        "Plugin OAuth callback missing state"
    );
    // RFC 9207: when an authorization server supplies `iss`, bind it exactly
    // to the reviewed issuer. Servers not advertising that extension still
    // have the single in-flight endpoint and PKCE/state binding.
    if let Some(actual) = fields.get("iss") {
        anyhow::ensure!(
            actual == issuer,
            "Plugin OAuth callback issuer does not match"
        );
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct PluginOAuthTokens {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: u64,
}

fn plugin_oauth_slot(
    provider: &str,
    base_url: &str,
    descriptor: &PluginOAuthConfig,
) -> Result<String> {
    descriptor.validate()?;
    oauth_endpoint_url(base_url)?;
    anyhow::ensure!(!provider.trim().is_empty(), "Plugin provider name is empty");
    let binding = serde_json::to_vec(&(provider, base_url, descriptor))?;
    let hash: String = Sha256::digest(binding)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(format!("plugin-oauth:{hash}"))
}

#[derive(Deserialize)]
struct PluginTokenResponse {
    #[serde(flatten)]
    material: OAuthTokenMaterial,
    #[serde(default)]
    token_type: Option<String>,
}

fn plugin_token_response(
    descriptor: &PluginOAuthConfig,
    form: &[(&str, &str)],
    previous_refresh: Option<String>,
) -> Result<PluginOAuthTokens> {
    let endpoint = oauth_endpoint_url(&descriptor.token_endpoint)?;
    let response = oauth_http_client(&endpoint, "plugin token exchange")?
        .post(endpoint)
        .form(form)
        .send()?;
    let (status, response): (_, PluginTokenResponse) =
        parse_oauth_json(response, "Plugin OAuth token exchange")?;
    let token = response.material;
    anyhow::ensure!(
        status.is_success() && token.error.is_none(),
        "Plugin OAuth token exchange failed with HTTP {}",
        status.as_u16()
    );
    anyhow::ensure!(
        response
            .token_type
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("bearer")),
        "Plugin OAuth token response requires Bearer token_type"
    );
    let access_token = token
        .access_token
        .filter(|token| !token.trim().is_empty())
        .context("Plugin OAuth token exchange returned no access token")?;
    let lifetime = token
        .expires_in
        .filter(|seconds| *seconds > 0)
        .context("Plugin OAuth token response requires a positive expires_in")?;
    Ok(PluginOAuthTokens {
        access_token,
        refresh_token: token
            .refresh_token
            .filter(|token| !token.trim().is_empty())
            .or(previous_refresh),
        expires_at: (now_unix_secs().context("System clock before UNIX epoch")? as u64)
            .saturating_add(lifetime),
    })
}

/// Core-owned standard public-client PKCE login. Blocking sockets and secure
/// storage stay on the dedicated worker; plugins never receive token material.
pub async fn plugin_oauth_login(
    provider: String,
    base_url: String,
    descriptor: PluginOAuthConfig,
    authority: crate::plugins::types::PluginAuthority,
) -> Result<()> {
    let policy = crate::plugins::activation::extension_host_policy_enabled();
    tokio::task::spawn_blocking(move || {
        let _scope = crate::plugins::activation::PolicyScope::propagate(policy);
        crate::plugins::providers::verify_provider_binding(
            &authority,
            &provider,
            &base_url,
            &descriptor,
            None,
        )
        .map_err(anyhow::Error::msg)?;
        let slot = plugin_oauth_slot(&provider, &base_url, &descriptor)?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let redirect_uri = format!(
            "http://127.0.0.1:{}{}",
            listener.local_addr()?.port(),
            descriptor.callback_path
        );
        let pkce = generate_pkce();
        let state = generate_state();
        let authorize_url = descriptor.authorize_url(&redirect_uri, &state, &pkce)?;
        eprintln!("{provider} sign-in (PKCE)\n  Open: {authorize_url}");
        if std::env::var_os("CODEWHALE_PLUGIN_OAUTH_NO_BROWSER").is_none() {
            let _ = webbrowser::open(&authorize_url);
        }
        let (code, _) = wait_for_callback_validated(
            &[listener],
            &descriptor.callback_params(),
            &state,
            Some(&descriptor.issuer),
        )?;
        crate::plugins::providers::verify_provider_binding(
            &authority,
            &provider,
            &base_url,
            &descriptor,
            None,
        )
        .map_err(anyhow::Error::msg)?;
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("client_id", descriptor.client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("code", code.as_str()),
            ("code_verifier", pkce.verifier.as_str()),
        ];
        if let Some(resource) = &descriptor.resource {
            form.push(("resource", resource));
        }
        let token = plugin_token_response(&descriptor, &form, None)?;
        crate::plugins::providers::verify_provider_binding(
            &authority,
            &provider,
            &base_url,
            &descriptor,
            None,
        )
        .map_err(anyhow::Error::msg)?;
        codewhale_secrets::Secrets::auto_detect().set(&slot, &serde_json::to_string(&token)?)?;
        Ok(())
    })
    .await
    .context("Plugin OAuth login worker failed")?
}

/// Prompt-free stored-login status. Readiness reads only this exact host-owned
/// secure slot, without refreshing, migrating or contacting the issuer.
pub fn plugin_oauth_credentials_present(
    provider: &str,
    base_url: &str,
    descriptor: &PluginOAuthConfig,
) -> Result<bool> {
    let slot = plugin_oauth_slot(provider, base_url, descriptor)?;
    let secrets = codewhale_secrets::Secrets::auto_detect_read_only();
    let Some(raw) = secrets.get(&slot)? else {
        return Ok(false);
    };
    plugin_oauth_saved_token(&raw)
}

fn plugin_oauth_saved_token(raw: &str) -> Result<bool> {
    let token: PluginOAuthTokens = serde_json::from_str(raw)
        .map_err(|_| anyhow::anyhow!("Plugin OAuth credential store contains invalid data"))?;
    let now = now_unix_secs().context("System clock before UNIX epoch")? as u64;
    Ok(!token.access_token.trim().is_empty()
        && (token.expires_at > now
            || token
                .refresh_token
                .as_deref()
                .is_some_and(|token| !token.trim().is_empty())))
}

/// Resolve only the exact provider, endpoint and descriptor-bound credential.
/// Async consumers must call this sync secure-store/HTTP worker off-runtime.
pub fn plugin_oauth_access_token(
    provider: &str,
    base_url: &str,
    descriptor: &PluginOAuthConfig,
    read_only: bool,
) -> Result<String> {
    let slot = plugin_oauth_slot(provider, base_url, descriptor)?;
    let secrets = if read_only {
        codewhale_secrets::Secrets::auto_detect_read_only()
    } else {
        codewhale_secrets::Secrets::auto_detect()
    };
    plugin_oauth_access_token_with_store(&slot, descriptor, read_only, &secrets)
}

fn plugin_oauth_access_token_with_store(
    slot: &str,
    descriptor: &PluginOAuthConfig,
    read_only: bool,
    secrets: &codewhale_secrets::Secrets,
) -> Result<String> {
    let resolve = |raw: &mut Option<String>| -> Result<String> {
        let stored = raw.as_ref().context(
            "Plugin OAuth login missing; use /login <provider> in Codewhale or codewhale auth plugin-login --provider <provider> in a terminal",
        )?;
        let mut token: PluginOAuthTokens = serde_json::from_str(stored)
            .map_err(|_| anyhow::anyhow!("Plugin OAuth credential store contains invalid data"))?;
        let now = now_unix_secs().context("System clock before UNIX epoch")? as u64;
        // The refresh window is not expiration. Non-refreshable grants and
        // read-only diagnostics may use a token for its actual valid lifetime.
        let expired = now >= token.expires_at;
        let refresh_due = !read_only
            && token.refresh_token.is_some()
            && now.saturating_add(60) >= token.expires_at;
        if expired || refresh_due {
            anyhow::ensure!(
                !read_only,
                "Plugin OAuth token expired; diagnostics never refresh credentials"
            );
            let refresh = token
                .refresh_token
                .as_ref()
                .context("Plugin OAuth token expired; sign in again")?;
            let mut form = vec![
                ("grant_type", "refresh_token"),
                ("client_id", descriptor.client_id.as_str()),
                ("refresh_token", refresh.as_str()),
            ];
            if let Some(resource) = &descriptor.resource {
                form.push(("resource", resource));
            }
            token = plugin_token_response(descriptor, &form, Some(refresh.clone()))?;
            *raw = Some(serde_json::to_string(&token)?);
        }
        Ok(token.access_token)
    };
    if read_only {
        return resolve(&mut secrets.get(slot)?);
    }
    // Serialize rotating refresh grants with the existing backend authority:
    // concurrent inference cannot replay an already consumed refresh token.
    secrets
        .with_entry_transaction(slot, |raw| {
            resolve(raw)
                .map_err(|error| codewhale_secrets::SecretsError::Keyring(error.to_string()))
        })
        .map_err(Into::into)
}

/// Local logout is authoritative; declarative plugins do not get a remote
/// revocation hook or raw refresh token.
pub fn plugin_oauth_logout(
    provider: &str,
    base_url: &str,
    descriptor: &PluginOAuthConfig,
) -> Result<()> {
    let slot = plugin_oauth_slot(provider, base_url, descriptor)?;
    codewhale_secrets::Secrets::auto_detect().delete(&slot)?;
    Ok(())
}

/// Pending-login test constructor shared by the activation tests.
#[cfg(test)]
pub(crate) fn pending_login_for_test(
    provider: OAuthProvider,
    access_token: &str,
    refresh_token: &str,
) -> PendingOAuthLogin {
    pending_login_with_id_token_for_test(provider, access_token, refresh_token, None)
}

#[cfg(test)]
pub(crate) fn pending_login_with_id_token_for_test(
    provider: OAuthProvider,
    access_token: &str,
    refresh_token: &str,
    id_token: Option<&str>,
) -> PendingOAuthLogin {
    let registration = (provider == OAuthProvider::Chatgpt).then(|| ChatgptRegistration {
        issuer: CHATGPT_OAUTH_ISSUER.to_string(),
        client_id: "oaiapp_codewhale_test".to_string(),
        subject: id_token
            .and_then(account_id_from_id_token)
            .unwrap_or_else(|| "test-sub".to_string()),
        email: None,
        host_id: format!("urn:uuid:{}", uuid::Uuid::new_v4()),
    });
    PendingOAuthLogin {
        provider,
        issuer: oauth_provider_params(provider).default_issuer.to_string(),
        client_id: registration.as_ref().map_or_else(
            || {
                oauth_provider_params(provider)
                    .default_client_id
                    .to_string()
            },
            |registration| registration.client_id.clone(),
        ),
        token: OAuthTokenMaterial {
            earliest_refresh_at: None,
            scope: registration
                .as_ref()
                .map(|_| CHATGPT_OAUTH_SCOPE.to_string()),
            token_type: Some("Bearer".to_string()),
            verified_chatgpt: registration,
            access_token: Some(access_token.to_string()),
            refresh_token: Some(refresh_token.to_string()),
            expires_in: Some(3600),
            id_token: id_token.map(ToOwned::to_owned),
            interval: None,
            error: None,
            error_description: None,
        },
    }
}

/// Local stored-proof fixture only; signature verification has separate tests.
/// The caller must isolate CODEWHALE_HOME before using this helper.
#[cfg(test)]
pub(crate) fn install_test_chatgpt_registration(config: &mut Config) -> Result<String> {
    install_test_chatgpt_registration_for(config, "test-sub", "oaiapp_codewhale_test")
}

#[cfg(test)]
pub(crate) fn install_test_chatgpt_registration_for(
    config: &mut Config,
    subject: &str,
    client_id: &str,
) -> Result<String> {
    let mut pending = pending_login_for_test(
        OAuthProvider::Chatgpt,
        "siwc-test-access",
        "siwc-test-refresh",
    );
    pending.client_id = client_id.to_string();
    let registration = pending
        .token
        .verified_chatgpt
        .as_mut()
        .expect("fixture registration");
    registration.subject = subject.to_string();
    registration.client_id = client_id.to_string();
    let generation = OAuthProvider::Chatgpt.new_generation();
    let mut entry = OwnedAuthEntry {
        access_token: None,
        refresh_token: None,
        expires_at: None,
        id_token: None,
        account_id: None,
        oidc_issuer: None,
        oidc_client_id: None,
        originator: None,
        auth_mode: None,
        extra: BTreeMap::new(),
    };
    apply_token_response(
        OAuthProvider::Chatgpt,
        &mut entry,
        &pending.issuer,
        &pending.client_id,
        &pending.token,
    )?;
    let file = BTreeMap::from([(format!("{}::{}", pending.issuer, pending.client_id), entry)]);
    codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
        write_auth_file_to_store(store, &generation, &file, false)
    })?;
    config.mark_codewhale_owned_chatgpt_oauth(generation)?;
    Ok("siwc-test-access".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderKind;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn oauth_endpoint_requires_https_or_parsed_loopback_http() {
        for raw in [
            "https://issuer.example/token",
            "http://localhost:8123/token",
            "http://127.0.0.1:8123/token",
            "http://127.0.0.2/token",
            "http://[::1]:8123/token",
        ] {
            assert!(oauth_endpoint_url(raw).is_ok());
        }
        for raw in [
            "http://issuer.example/token",
            "http://localhost.example/token",
            "http://127.0.0.1.example/token",
            "http://192.0.2.1/token",
            "https://user:example@issuer.example/token",
            "file:///tmp/token",
            "not a URL",
        ] {
            assert!(oauth_endpoint_url(raw).is_err());
        }
    }

    #[test]
    fn oauth_discovery_rejects_an_initial_plaintext_remote_issuer() {
        let issuer = "http://issuer.example";
        assert!(validate_discovered_issuer(Some(issuer.into()), issuer).is_err());
        assert!(
            validate_discovered_oauth_endpoint(
                Some(format!("{issuer}/token")),
                "token_endpoint",
                issuer,
            )
            .is_err()
        );
        let fallback = fallback_oauth_endpoints(&XAI_OAUTH_PARAMS, issuer);
        assert!(oauth_endpoint_url(&fallback.token_endpoint).is_err());
        assert!(
            oauth_endpoint_url(fallback.device_authorization_endpoint.as_deref().unwrap()).is_err()
        );
    }

    #[test]
    fn oauth_authorize_url_refuses_plaintext_remote_issuer_before_browser_use() {
        let pkce = PkceChallenge {
            verifier: "test-verifier".into(),
            challenge: "test-challenge".into(),
        };
        for issuer in [
            "http://issuer.example",
            "https://user:example@issuer.example",
        ] {
            assert!(
                build_authorize_url(
                    &CHATGPT_OAUTH_PARAMS,
                    issuer,
                    "test-client",
                    "openid",
                    "http://localhost:1455/auth/callback",
                    "test-state",
                    &pkce,
                )
                .is_err()
            );
        }
    }

    fn grant(path: &std::path::Path) -> ExternalCredentialReadGrant {
        codewhale_config::ExternalCredentialConsentToml::read_only(
            codewhale_config::ProviderKind::OpenaiCodex,
            codewhale_config::ExternalCredentialSource::CodexCli,
            path.to_path_buf(),
        )
        .read_grant(
            codewhale_config::ProviderKind::OpenaiCodex,
            codewhale_config::ExternalCredentialSource::CodexCli,
            path,
        )
        .expect("test read grant")
    }

    #[test]
    fn jwt_expiry_parses_valid_token() {
        // A minimal JWT with {"exp": 9999999999} as payload.
        let payload = URL_SAFE_NO_PAD.encode(b"{\"exp\":9999999999}");
        let token = format!("header.{payload}.signature");
        assert_eq!(jwt_expiry_seconds(&token), Some(9999999999));
    }

    #[test]
    fn jwt_expiry_returns_none_for_malformed() {
        assert_eq!(jwt_expiry_seconds("not.a.jwt"), None);
        assert_eq!(jwt_expiry_seconds(""), None);
        assert_eq!(jwt_expiry_seconds("x"), None);
    }

    #[test]
    fn token_is_expired_detects_future() {
        // Far future — should not be expired.
        let payload = URL_SAFE_NO_PAD.encode(b"{\"exp\":9999999999}");
        let token = format!("header.{payload}.sig");
        assert!(!token_is_expired(&token));
    }

    #[test]
    fn token_is_expired_detects_past() {
        // Way in the past.
        let payload = URL_SAFE_NO_PAD.encode(b"{\"exp\":1000000000}");
        let token = format!("header.{payload}.sig");
        assert!(token_is_expired(&token));
    }

    #[test]
    fn owned_token_freshness_honors_both_expiries_and_preserves_fallbacks() {
        let now = now_unix_secs().expect("clock");
        let future = rfc3339_from_unix(now + 3600);
        let past = rfc3339_from_unix(now - 3600);
        let near = rfc3339_from_unix(now + 30);
        let fresh_token = jwt_with_exp((now + 3600) as u64);
        let expired_token = jwt_with_exp((now - 3600) as u64);
        let near_token = jwt_with_exp((now + 30) as u64);

        for (stored, token, expected) in [
            (Some(future.as_str()), expired_token.as_str(), false),
            (Some(past.as_str()), fresh_token.as_str(), false),
            (Some(future.as_str()), fresh_token.as_str(), true),
            (Some(future.as_str()), near_token.as_str(), false),
            (Some(near.as_str()), fresh_token.as_str(), false),
            (None, fresh_token.as_str(), true),
            (None, expired_token.as_str(), false),
            (Some("invalid-date"), fresh_token.as_str(), true),
            (Some(future.as_str()), "opaque-token", true),
            (Some(past.as_str()), "opaque-token", false),
            (None, "opaque-token", false),
            (Some("invalid-date"), "opaque-token", false),
            (Some(future.as_str()), "", false),
        ] {
            let entry: OwnedAuthEntry = serde_json::from_value(serde_json::json!({
                "access_token": token,
                "expires_at": stored,
            }))
            .expect("synthetic owned entry");
            assert_eq!(
                entry_access_token_is_fresh(&entry),
                expected,
                "stored={stored:?}, JWT expiry={:?}",
                jwt_expiry_seconds(token),
            );
        }
    }

    #[test]
    fn credential_presence_rejects_empty_and_malformed_files_without_refresh() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("temp Codex home");
        let auth_path = home
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("auth.json");
        let _auth = crate::test_support::EnvVarGuard::set("OPENAI_CODEX_AUTH_FILE", &auth_path);
        let _access = crate::test_support::EnvVarGuard::remove("OPENAI_CODEX_ACCESS_TOKEN");
        let _legacy_access = crate::test_support::EnvVarGuard::remove("CODEX_ACCESS_TOKEN");
        let grant = grant(&auth_path);

        std::fs::write(&auth_path, "{}").expect("empty auth");
        crate::external_credentials::reset_side_effect_trap();
        assert!(!stored_credentials_present(&grant));
        assert_eq!(
            crate::external_credentials::side_effect_trap_counts(),
            (1, 1)
        );
        assert_eq!(
            crate::external_credentials::complete_side_effect_trap_counts(),
            (1, 1, 0, 0, 0)
        );
        std::fs::write(&auth_path, "{not-json").expect("malformed auth");
        crate::external_credentials::reset_side_effect_trap();
        assert!(!stored_credentials_present(&grant));
        assert_eq!(
            crate::external_credentials::side_effect_trap_counts(),
            (1, 1)
        );

        let payload = URL_SAFE_NO_PAD.encode(b"{\"exp\":9999999999}");
        let access_token = format!("header.{payload}.signature");
        std::fs::write(
            &auth_path,
            serde_json::to_vec(&serde_json::json!({
                "tokens": {"access_token": access_token}
            }))
            .expect("valid auth json"),
        )
        .expect("valid auth");
        crate::external_credentials::reset_side_effect_trap();
        assert!(stored_credentials_present(&grant));
        assert_eq!(
            crate::external_credentials::side_effect_trap_counts(),
            (1, 1)
        );
    }

    #[test]
    fn expired_external_token_fails_without_refresh_or_rewrite() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("temp Codex home");
        let auth_path = home
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("auth.json");
        let payload = URL_SAFE_NO_PAD.encode(b"{\"exp\":1000000000}");
        let access_token = format!("header.{payload}.signature");
        let raw = serde_json::to_string_pretty(&serde_json::json!({
            "tokens": {
                "access_token": access_token,
                "refresh_token": "must-never-be-used",
                "account_id": "acct-test",
                "future_field": {"preserve": true}
            },
            "future_top_level": [1, 2, 3]
        }))
        .expect("auth fixture");
        std::fs::write(&auth_path, &raw).expect("expired auth fixture");

        crate::external_credentials::reset_side_effect_trap();
        let error = get_credentials(&grant(&auth_path))
            .expect_err("read-only external tokens must not refresh");
        assert!(error.to_string().contains("never refreshes or rewrites"));
        assert_eq!(
            crate::external_credentials::side_effect_trap_counts(),
            (1, 1)
        );
        assert_eq!(
            std::fs::read_to_string(&auth_path).expect("unchanged auth file"),
            raw
        );
    }

    #[test]
    fn auth_file_path_respects_env() {
        // Just verify it returns a path without panicking.
        let path = auth_file_path();
        assert!(path.to_string_lossy().contains("auth.json"));
    }

    #[test]
    fn missing_auth_message_guides_official_sign_in() {
        let message = missing_auth_message(OAuthProvider::Chatgpt);
        assert!(message.contains("Official ChatGPT credentials"));
        assert!(message.contains("codewhale auth chatgpt"));
        assert!(message.contains("ChatGPT plan usage"));
        assert!(message.contains("openai API-key"));
        assert!(message.contains("chatgpt-revoke"));
        assert!(!message.contains("external-consent"));
        assert!(!message.contains("CODEX_ACCESS_TOKEN"));
    }

    #[test]
    fn provider_table_separates_device_flow_from_browser_flow() {
        let xai = oauth_provider_params(OAuthProvider::Xai);
        assert_eq!(xai.device_code_path, Some("oauth2/device/code"));
        assert!(xai.discover_endpoints);
        let chatgpt = oauth_provider_params(OAuthProvider::Chatgpt);
        assert_eq!(chatgpt.device_code_path, None);
        assert!(!chatgpt.discover_endpoints);
        assert_ne!(xai.default_client_id, chatgpt.default_client_id);
    }

    #[test]
    fn provider_inputs_resolve_from_defaults_without_env() {
        let _lock = crate::test_support::lock_test_env();
        let _guards: Vec<_> = [
            "GROK_OIDC_ISSUER",
            "XAI_OIDC_ISSUER",
            "GROK_OIDC_CLIENT_ID",
            "XAI_OIDC_CLIENT_ID",
            "GROK_OIDC_SCOPES",
            "XAI_OIDC_SCOPES",
            "CODEWHALE_XAI_OAUTH_NO_BROWSER",
        ]
        .into_iter()
        .map(crate::test_support::EnvVarGuard::remove)
        .collect();
        let inputs = XAI_OAUTH_PARAMS.resolve_inputs();
        assert_eq!(inputs.issuer, "https://auth.x.ai");
        assert!(inputs.scopes.contains("grok-cli:access"));
        assert!(inputs.open_browser);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn request_device_grant_round_trips_a_mock_grant() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/device/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "device-123",
                "user_code": "USER-456",
                "verification_uri": "https://example.com/device",
                "expires_in": 900
            })))
            .expect(1)
            .mount(&server)
            .await;
        let grant = tokio::task::block_in_place(|| {
            request_device_grant(
                &format!("{}/oauth2/device/code", server.uri()),
                "test-client",
                "openid",
            )
        })
        .expect("mock grant");
        assert_eq!(grant.device_code.as_deref(), Some("device-123"));
        assert_eq!(grant.user_code.as_deref(), Some("USER-456"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn request_device_grant_rejects_success_without_codes() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/device/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let result = tokio::task::block_in_place(|| {
            request_device_grant(
                &format!("{}/oauth2/device/code", server.uri()),
                "test-client",
                "openid",
            )
        });
        let Err(error) = result else {
            panic!("a grant without codes must fail");
        };
        assert!(error.to_string().contains("without a device and user code"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn poll_device_grant_classifies_rfc8628_states() {
        use codewhale_config::device_code::DevicePollOutcome;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        async fn outcome(
            body: serde_json::Value,
            status: u16,
        ) -> Result<DevicePollOutcome<OAuthTokenMaterial>> {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/oauth2/token"))
                .respond_with(ResponseTemplate::new(status).set_body_json(body))
                .expect(1)
                .mount(&server)
                .await;
            tokio::task::block_in_place(|| {
                poll_device_grant(
                    &format!("{}/oauth2/token", server.uri()),
                    "test-client",
                    "device-token",
                )
            })
        }
        assert!(matches!(
            outcome(serde_json::json!({ "error": "authorization_pending" }), 400).await,
            Ok(DevicePollOutcome::Pending)
        ));
        assert!(matches!(
            outcome(
                serde_json::json!({ "error": "slow_down", "interval": 12 }),
                400
            )
            .await,
            Ok(DevicePollOutcome::SlowDown {
                interval_seconds: Some(12)
            })
        ));
        for error in ["access_denied", "expired_token"] {
            let result = outcome(serde_json::json!({ "error": error }), 400).await;
            let Err(failure) = result else {
                panic!("{error} must stop polling");
            };
            assert!(failure.to_string().contains(error), "{failure}");
        }
        let result = outcome(
            serde_json::json!({ "access_token": "at", "expires_in": 3600 }),
            200,
        )
        .await;
        let Ok(DevicePollOutcome::Complete(material)) = result else {
            panic!("success must complete");
        };
        assert_eq!(material.access_token.as_deref(), Some("at"));
    }

    #[test]
    fn oauth_challenges_require_a_terminal_and_avoid_redirected_streams() {
        assert_eq!(
            oauth_challenge_stream(true, false).unwrap(),
            OAuthChallengeStream::Stderr
        );
        assert_eq!(
            oauth_challenge_stream(true, true).unwrap(),
            OAuthChallengeStream::Stderr
        );
        assert_eq!(
            oauth_challenge_stream(false, true).unwrap(),
            OAuthChallengeStream::Stdout
        );
        let error = oauth_challenge_stream(false, false).unwrap_err();
        assert!(error.to_string().contains("requires a terminal"));
        assert!(!error.to_string().contains("token"));
    }

    #[tokio::test]
    async fn device_login_without_a_device_flow_fails_before_network() {
        let result = device_code_login(OAuthProvider::Chatgpt).await;
        let Err(error) = result else {
            panic!("ChatGPT has no device flow");
        };
        assert!(
            error.to_string().contains("no device-code flow"),
            "{error:#}"
        );
    }

    #[test]
    fn oauth_loopback_forms_bypass_ambient_proxies() {
        const PROBE_ENDPOINT: &str = "CODEWHALE_TEST_OAUTH_PROXY_ENDPOINT";
        if let Ok(endpoint) = std::env::var(PROBE_ENDPOINT) {
            let (status, _) = ReqwestOAuthFormClient
                .post_form(&endpoint, &[("refresh_token", "synthetic-refresh")])
                .expect("local OAuth form remains usable with an ambient proxy");
            assert_eq!(status, 200);
            return;
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            use wiremock::matchers::{body_string_contains, method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};
            let issuer = MockServer::start().await;
            let proxy = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/token"))
                .and(body_string_contains("refresh_token=synthetic-refresh"))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&issuer)
                .await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(502))
                .expect(0)
                .mount(&proxy)
                .await;
            // A fresh process gives reqwest an uncontaminated proxy cache and
            // keeps ambient proxy changes out of the shared test process.
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "oauth::tests::oauth_loopback_forms_bypass_ambient_proxies",
                    "--test-threads=1",
                ])
                .env(PROBE_ENDPOINT, format!("{}/token", issuer.uri()))
                .env_remove("NO_PROXY")
                .env_remove("no_proxy");
            for variable in [
                "HTTP_PROXY",
                "http_proxy",
                "HTTPS_PROXY",
                "https_proxy",
                "ALL_PROXY",
                "all_proxy",
            ] {
                child.env(variable, proxy.uri());
            }
            let output = tokio::task::spawn_blocking(move || child.output().unwrap())
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(issuer.received_requests().await.unwrap().len(), 1);
            assert!(proxy.received_requests().await.unwrap().is_empty());
        });
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oauth_transports_never_forward_forms_to_redirect_destinations() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let issuer = MockServer::start().await;
        let destination = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "synthetic-access",
            })))
            .expect(0)
            .mount(&destination)
            .await;
        for status in [301, 302, 303, 307, 308] {
            let endpoint = format!("{}/redirect-{status}", issuer.uri());
            Mock::given(path(format!("/redirect-{status}")))
                .respond_with(
                    ResponseTemplate::new(status)
                        .insert_header("Location", format!("{}/token", destination.uri()))
                        .set_body_json(serde_json::json!({})),
                )
                .expect(3)
                .mount(&issuer)
                .await;
            tokio::task::block_in_place(|| {
                assert!(request_device_grant(&endpoint, "synthetic-client", "scope").is_err());
                assert!(
                    poll_device_grant(&endpoint, "synthetic-client", "synthetic-device").is_err()
                );
                let (actual_status, _) = ReqwestOAuthFormClient
                    .post_form(&endpoint, &[("refresh_token", "synthetic-refresh")])
                    .unwrap();
                assert_eq!(actual_status, status);
            });
        }
        // An explicitly selected issuer remains usable without a redirect.
        Mock::given(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "synthetic-device",
                "user_code": "synthetic-user",
                "access_token": "synthetic-access",
            })))
            .expect(3)
            .mount(&issuer)
            .await;
        let endpoint = format!("{}/token", issuer.uri());
        tokio::task::block_in_place(|| {
            assert!(request_device_grant(&endpoint, "synthetic-client", "scope").is_ok());
            assert!(poll_device_grant(&endpoint, "synthetic-client", "synthetic-device").is_ok());
            assert_eq!(
                ReqwestOAuthFormClient
                    .post_form(&endpoint, &[("refresh_token", "synthetic-refresh")])
                    .unwrap()
                    .0,
                200
            );
        });
        assert!(destination.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn discovery_honors_advertised_endpoints() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": server.uri(),
                "device_authorization_endpoint": format!("{}/custom/device", server.uri()),
                "token_endpoint": format!("{}/custom/token", server.uri()),
            })))
            .expect(1)
            .mount(&server)
            .await;
        let endpoints = tokio::task::block_in_place(|| {
            resolve_oauth_endpoints(&XAI_OAUTH_PARAMS, &server.uri())
        });
        assert_eq!(
            endpoints.device_authorization_endpoint.as_deref(),
            Some(format!("{}/custom/device", server.uri()).as_str())
        );
        assert_eq!(
            endpoints.token_endpoint,
            format!("{}/custom/token", server.uri())
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn discovery_issuer_mismatch_falls_back_to_documented_paths() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": "https://someone-else.example",
                "device_authorization_endpoint": "https://someone-else.example/device",
                "token_endpoint": "https://someone-else.example/token",
            })))
            .expect(1)
            .mount(&server)
            .await;
        let endpoints = tokio::task::block_in_place(|| {
            resolve_oauth_endpoints(&XAI_OAUTH_PARAMS, &server.uri())
        });
        assert_eq!(
            endpoints.device_authorization_endpoint.as_deref(),
            Some(format!("{}/oauth2/device/code", server.uri()).as_str())
        );
        assert_eq!(
            endpoints.token_endpoint,
            format!("{}/oauth2/token", server.uri())
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn device_login_aborts_on_untrusted_verification_uri() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": server.uri(),
                "device_authorization_endpoint": format!("{}/oauth2/device-advertised", server.uri()),
                "token_endpoint": format!("{}/oauth2/token", server.uri()),
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth2/device-advertised"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "device-token",
                "user_code": "CW-TEST",
                "verification_uri": "https://auth.x.ai/device",
                "verification_uri_complete": "vscode://attacker/run?code=CW-TEST",
                "expires_in": 60,
                "interval": 1
            })))
            .expect(1)
            .mount(&server)
            .await;
        // No token-endpoint mock: the flow must fail before it ever polls.
        let inputs = ResolvedOAuthInputs {
            issuer: server.uri(),
            client_id: "test-client".to_string(),
            scopes: "openid".to_string(),
            open_browser: false,
        };
        let result = tokio::task::block_in_place(|| {
            device_code_login_with(OAuthProvider::Xai, &inputs, &mut std::io::sink())
        });
        let Err(error) = result else {
            panic!("a non-web verification URI must abort login");
        };
        assert!(
            format!("{error:#}").contains("untrusted verification URI"),
            "{error:#}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn device_login_refuses_terminal_controls_before_display_or_polling() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": server.uri(),
                "device_authorization_endpoint": format!("{}/device", server.uri()),
                "token_endpoint": format!("{}/token", server.uri())
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "fixture-private-device-code",
                "user_code": "CW-\u{1b}]52;clipboard-payload",
                "verification_uri": format!("{}/verify", server.uri()),
                "expires_in": 60,
                "interval": 1
            })))
            .expect(1)
            .mount(&server)
            .await;
        let inputs = ResolvedOAuthInputs {
            issuer: server.uri(),
            client_id: "test-client".to_string(),
            scopes: "openid".to_string(),
            open_browser: false,
        };
        let mut challenge = Vec::new();
        let error = tokio::task::block_in_place(|| {
            device_code_login_with(OAuthProvider::Xai, &inputs, &mut challenge)
        })
        .err()
        .expect("terminal controls must be refused");
        assert!(error.to_string().contains("invalid user-code display data"));
        assert!(challenge.is_empty());
        assert!(!error.to_string().contains("clipboard-payload"));
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    /// Discovery + device grant run on the blocking worker: this fails with
    /// the grant refusal, never with a runtime-drop panic.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn device_login_runs_blocking_http_off_the_executor() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let _lock = crate::test_support::lock_test_env();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": server.uri(),
                "device_authorization_endpoint": format!("{}/oauth2/device-advertised", server.uri()),
                "token_endpoint": format!("{}/oauth2/token-advertised", server.uri())
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth2/device-advertised"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_scope",
                "error_description": "mock refusal before browser or polling"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let _issuer =
            crate::test_support::EnvVarGuard::set("GROK_OIDC_ISSUER", server.uri().as_str());
        let _no_browser =
            crate::test_support::EnvVarGuard::set("CODEWHALE_XAI_OAUTH_NO_BROWSER", "1");

        let result = device_code_login_on_worker(
            OAuthProvider::Xai,
            XAI_OAUTH_PARAMS.resolve_inputs(),
            Box::new(std::io::sink()),
        )
        .await;
        let Err(error) = result else {
            panic!("mock device request must fail without a runtime-drop panic");
        };
        let message = format!("{error:#}");
        assert!(message.contains("invalid_scope"), "{message}");
        assert!(message.contains("HTTP 400"), "{message}");
    }

    /// Full orchestration against mocks: discovery, grant, one poll, done.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn device_login_exchanges_and_returns_token_material() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": server.uri(),
                "device_authorization_endpoint": format!("{}/oauth2/device-advertised", server.uri()),
                "token_endpoint": format!("{}/oauth2/token-advertised", server.uri())
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth2/device-advertised"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "device-token",
                "user_code": "CW-TEST",
                "verification_uri": format!("{}/verify", server.uri()),
                "expires_in": 60,
                "interval": 1
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token-advertised"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "unified-access",
                "refresh_token": "unified-refresh",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        let inputs = ResolvedOAuthInputs {
            issuer: server.uri(),
            client_id: "test-client".to_string(),
            scopes: "openid".to_string(),
            open_browser: false,
        };
        let mut challenge = Vec::new();
        let pending = tokio::task::block_in_place(|| {
            device_code_login_with(OAuthProvider::Xai, &inputs, &mut challenge)
        })
        .expect("mock login exchanges");
        let challenge = String::from_utf8(challenge).unwrap();
        assert!(challenge.contains(&format!("{}/verify", server.uri())));
        assert!(challenge.contains("CW-TEST"));
        assert!(!challenge.contains("unified-access"));
        assert!(!challenge.contains("unified-refresh"));
        assert_eq!(pending.issuer, server.uri());
        assert_eq!(
            pending.token.access_token.as_deref(),
            Some("unified-access")
        );
        assert_eq!(
            pending.token.refresh_token.as_deref(),
            Some("unified-refresh")
        );
    }

    /// The shared poll loop walks pending and slow_down to completion.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn device_login_polls_through_pending_and_slow_down() {
        use codewhale_config::device_code::DeviceCodePoll;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        // wiremock matches mocks in mount order, so mount the one-shot
        // transient-error responses before the terminal success response:
        // poll 1 -> authorization_pending, poll 2 -> slow_down, poll 3 -> ok.
        for (body, status) in [
            (serde_json::json!({ "error": "authorization_pending" }), 400),
            (serde_json::json!({ "error": "slow_down" }), 400),
        ] {
            Mock::given(method("POST"))
                .and(path("/oauth2/token"))
                .respond_with(ResponseTemplate::new(status).set_body_json(body))
                .up_to_n_times(1)
                .expect(1)
                .mount(&server)
                .await;
        }
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "access_token": "loop-access", "expires_in": 3600 }),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let endpoint = format!("{}/oauth2/token", server.uri());
        let material = tokio::task::block_in_place(|| {
            DeviceCodePoll::new(
                std::time::Duration::from_secs(60),
                "mock poll must complete",
            )
            .run(
                |_| {},
                || poll_device_grant(&endpoint, "test-client", "device-token"),
            )
        })
        .expect("poll loop completes");
        assert!(matches!(
            material.access_token.as_deref(),
            Some("loop-access")
        ));
    }

    /// A denied grant stops the full login with the server's reason.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn device_login_surfaces_user_denial() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": server.uri(),
                "device_authorization_endpoint": format!("{}/oauth2/device-advertised", server.uri()),
                "token_endpoint": format!("{}/oauth2/token-advertised", server.uri())
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth2/device-advertised"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "device-token",
                "user_code": "CW-TEST",
                "verification_uri": format!("{}/verify", server.uri()),
                "expires_in": 60,
                "interval": 1
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token-advertised"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "access_denied",
                "error_description": "The user denied the authorization request"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let inputs = ResolvedOAuthInputs {
            issuer: server.uri(),
            client_id: "test-client".to_string(),
            scopes: "openid".to_string(),
            open_browser: false,
        };
        let result = tokio::task::block_in_place(|| {
            device_code_login_with(OAuthProvider::Xai, &inputs, &mut std::io::sink())
        });
        let Err(error) = result else {
            panic!("user denial must stop the login");
        };
        let message = format!("{error:#}");
        assert!(message.contains("access_denied"), "{message}");
        assert!(message.contains("HTTP 400"), "{message}");
    }

    /// Non-JSON answers disclose neither response metadata nor the body.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn device_transport_reports_non_json_without_echoing_body() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/device-code"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes("<html>sentinel-body-bytes</html>".as_bytes())
                    .insert_header(
                        "content-type",
                        "text/html; credential=sentinel-header-bytes",
                    ),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes("<html>sentinel-body-bytes</html>".as_bytes())
                    .insert_header(
                        "content-type",
                        "text/html; credential=sentinel-header-bytes",
                    ),
            )
            .expect(1)
            .mount(&server)
            .await;
        let grant = tokio::task::block_in_place(|| {
            request_device_grant(
                &format!("{}/oauth2/device-code", server.uri()),
                "test-client",
                "openid",
            )
        });
        let Err(grant_error) = grant else {
            panic!("non-JSON grant must fail");
        };
        let poll = tokio::task::block_in_place(|| {
            poll_device_grant(
                &format!("{}/oauth2/token", server.uri()),
                "test-client",
                "device-token",
            )
        });
        let Err(poll_error) = poll else {
            panic!("non-JSON poll must fail");
        };
        for message in [format!("{grant_error:#}"), format!("{poll_error:#}")] {
            assert!(message.contains("HTTP 200"), "{message}");
            assert!(message.contains("expected JSON"), "{message}");
            assert!(!message.contains("sentinel-header-bytes"), "{message}");
            assert!(!message.contains("sentinel-body-bytes"), "{message}");
        }
    }

    /// The wire format is a contract: form-encoded posts carrying the exact
    /// client, scope, and grant-type parameters the issuers expect.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn device_transport_posts_exact_oauth_form_parameters() {
        use wiremock::matchers::{body_string_contains, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": server.uri(),
                "device_authorization_endpoint": format!("{}/oauth2/device-advertised", server.uri()),
                "token_endpoint": format!("{}/oauth2/token-advertised", server.uri())
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth2/device-advertised"))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(body_string_contains("client_id=test-client"))
            .and(body_string_contains("scope=openid"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "device-token",
                "user_code": "CW-TEST",
                "verification_uri": format!("{}/verify", server.uri()),
                "expires_in": 60,
                "interval": 1
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token-advertised"))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(body_string_contains(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
            ))
            .and(body_string_contains("client_id=test-client"))
            .and(body_string_contains("device_code=device-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "form-access",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        let inputs = ResolvedOAuthInputs {
            issuer: server.uri(),
            client_id: "test-client".to_string(),
            scopes: "openid".to_string(),
            open_browser: false,
        };
        let pending = tokio::task::block_in_place(|| {
            device_code_login_with(OAuthProvider::Xai, &inputs, &mut std::io::sink())
        })
        .expect("mock login exchanges");
        assert_eq!(pending.token.access_token.as_deref(), Some("form-access"));
    }

    #[test]
    fn access_method_labels_name_every_route() {
        assert_eq!(
            AccessMethod::OwnedOAuth(OAuthProvider::Xai).label(),
            "xAI subscription"
        );
        assert_eq!(
            AccessMethod::ExternalImport(ExternalImportSource::CodexCli).label(),
            "Codex CLI import"
        );
        assert_eq!(AccessMethod::ApiKey.label(), "API key");
        assert_eq!(AccessMethod::AcpBridge.label(), "ACP bridge");
    }

    #[test]
    fn malformed_codex_credential_errors_never_echo_file_contents() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("temp Codex home");
        let path = home.path().canonicalize().unwrap().join("auth.json");
        let sentinel = "must-not-appear-in-diagnostics";
        std::fs::write(
            &path,
            format!(r#"{{"tokens":{{"access_token":{{"secret":"{sentinel}"}}}}}}"#),
        )
        .unwrap();

        let error = load_credentials(&grant(&path)).expect_err("malformed schema");
        let message = format!("{error:#}");
        assert!(message.contains("not valid credential JSON"), "{message}");
        assert!(!message.contains(sentinel), "{message}");
    }

    // ── unified PKCE core (ported from the deleted chatgpt_oauth flow) ──

    use std::sync::Mutex;

    type MockForm = Vec<(String, String)>;
    type MockPost = (String, MockForm);

    struct MockFormClient {
        responses: Mutex<Vec<(u16, String)>>,
        posts: Mutex<Vec<MockPost>>,
    }

    impl MockFormClient {
        fn new(responses: Vec<(u16, String)>) -> Self {
            Self {
                responses: Mutex::new(responses),
                posts: Mutex::new(Vec::new()),
            }
        }
    }

    impl OAuthFormClient for MockFormClient {
        fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, String)> {
            self.posts.lock().expect("posts").push((
                url.to_string(),
                form.iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
            ));
            let mut responses = self.responses.lock().expect("responses");
            anyhow::ensure!(
                !responses.is_empty(),
                "mock issuer has no remaining responses"
            );
            Ok(responses.remove(0))
        }
    }

    fn jwt_with_account(account: &str) -> String {
        let payload = URL_SAFE_NO_PAD.encode(format!(
            r#"{{"https://api.openai.com/auth":{{"chatgpt_account_id":"{account}"}}}}"#
        ));
        format!("header.{payload}.sig")
    }

    fn chatgpt() -> &'static OAuthProviderParams<'static> {
        oauth_provider_params(OAuthProvider::Chatgpt)
    }

    #[test]
    fn pkce_verifier_and_challenge_are_s256() {
        let pkce = generate_pkce();
        assert!(pkce.verifier.len() >= 43);
        assert_eq!(
            pkce.challenge,
            URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier.as_bytes()))
        );
        let other = generate_pkce();
        assert_ne!(pkce.verifier, other.verifier);
        assert_ne!(generate_state(), generate_state());
    }

    #[test]
    fn malformed_issuer_fails_loudly_not_to_production() {
        let pkce = PkceChallenge {
            verifier: "verifier".into(),
            challenge: "challenge".into(),
        };
        let err = build_authorize_url(
            chatgpt(),
            "not a url \\ ",
            "client",
            "openid",
            "http://localhost:1455/auth/callback",
            "state-1",
            &pkce,
        )
        .expect_err("malformed issuer must not produce an authorize URL");
        assert!(
            format!("{err:#}").contains("CODEWHALE_CHATGPT_OAUTH_ISSUER"),
            "{err:#}"
        );
    }

    #[test]
    fn authorize_url_uses_documented_chatgpt_parameters_and_pkce() {
        let _lock = crate::test_support::lock_test_env();
        let _prompt = crate::test_support::EnvVarGuard::remove("CODEWHALE_CHATGPT_OAUTH_NO_PROMPT");
        let pkce = PkceChallenge {
            verifier: "verifier".into(),
            challenge: "challenge".into(),
        };
        let url = build_authorize_url(
            chatgpt(),
            CHATGPT_OAUTH_ISSUER,
            CHATGPT_OAUTH_CLIENT_ID,
            CHATGPT_OAUTH_SCOPE,
            "http://localhost:1455/auth/callback",
            "state-1",
            &pkce,
        )
        .expect("static issuer parses");
        assert!(url.starts_with("https://auth.openai.com/api/accounts/authorize?"));
        assert!(url.contains("code_challenge=challenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(!url.contains("originator="), "{url}");
        assert!(!url.contains("codex_cli_rs"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
        assert!(url.contains("resource=https%3A%2F%2Fapi.openai.com%2Fv1"));
        // Account choice: the issuer must re-prompt rather than reuse the
        // browser's current ChatGPT session.
        let query: BTreeMap<String, String> = reqwest::Url::parse(&url)
            .expect("authorize URL parses")
            .query_pairs()
            .into_owned()
            .collect();
        assert_eq!(query.get("prompt").map(String::as_str), Some("login"));
        assert_eq!(url.matches("prompt=").count(), 1, "{url}");

        // Opt-out, should the issuer ever refuse the parameter.
        let _opt_out =
            crate::test_support::EnvVarGuard::set("CODEWHALE_CHATGPT_OAUTH_NO_PROMPT", "1");
        let url = build_authorize_url(
            chatgpt(),
            CHATGPT_OAUTH_ISSUER,
            CHATGPT_OAUTH_CLIENT_ID,
            CHATGPT_OAUTH_SCOPE,
            "http://localhost:1455/auth/callback",
            "state-1",
            &pkce,
        )
        .expect("static issuer parses");
        assert!(!url.contains("prompt="), "{url}");
        assert!(
            url.contains("resource=https%3A%2F%2Fapi.openai.com%2Fv1"),
            "{url}"
        );
    }

    fn id_token(claims: &serde_json::Value) -> String {
        format!(
            "header.{}.sig",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("claims serialize"))
        )
    }

    #[test]
    fn account_label_reads_email_and_plan_claims() {
        let chatgpt = id_token(&serde_json::json!({
            "email": "a@example.com",
            "https://api.openai.com/auth": {"chatgpt_plan_type": "plus", "chatgpt_account_id": "acct-1"},
        }));
        assert_eq!(
            account_label_from_id_token(&chatgpt).as_deref(),
            Some("a@example.com (plus)")
        );
        // Profile-namespaced email is the fallback the ChatGPT issuer uses.
        let profile = id_token(&serde_json::json!({
            "https://api.openai.com/profile": {"email": "b@example.com"},
        }));
        assert_eq!(
            account_label_from_id_token(&profile).as_deref(),
            Some("b@example.com")
        );
        // A workspace plan names its account-id prefix: one email can hold
        // several workspaces, and email + plan alone would read the same.
        let team = id_token(&serde_json::json!({
            "email": "a@corp.com",
            "https://api.openai.com/auth": {"chatgpt_plan_type": "team", "chatgpt_account_id": "1a2b3c4d-5e6f-7081-92a3-b4c5d6e7f809"},
        }));
        assert_eq!(
            account_label_from_id_token(&team).as_deref(),
            Some("a@corp.com (team, workspace 1a2b3c4d)")
        );
        // xAI: plain OIDC email, no plan claim.
        let xai = id_token(&serde_json::json!({"email": "grok@example.com", "sub": "u1"}));
        assert_eq!(
            account_label_from_id_token(&xai).as_deref(),
            Some("grok@example.com")
        );
    }

    #[test]
    fn account_label_rejects_malformed_tokens_and_missing_claims() {
        for token in [
            "",
            "not-a-jwt",
            "header.%%%.sig",
            &format!("header.{}.sig", URL_SAFE_NO_PAD.encode("not json")),
            &id_token(&serde_json::json!({"sub": "u1"})),
            &id_token(&serde_json::json!({"email": "   "})),
            &id_token(&serde_json::json!({"email": 42})),
            // A plan alone does not identify an account.
            &id_token(&serde_json::json!({
                "https://api.openai.com/auth": {"chatgpt_plan_type": "pro"},
            })),
        ] {
            assert_eq!(account_label_from_id_token(token), None, "{token}");
        }
    }

    #[test]
    fn account_label_strips_control_characters_and_bounds_length() {
        let hostile = id_token(&serde_json::json!({
            "email": "\u{1b}[31mevil@example.com\u{7}",
            "https://api.openai.com/auth": {"chatgpt_plan_type": "x".repeat(500)},
        }));
        let label = account_label_from_id_token(&hostile).expect("label");
        assert!(!label.chars().any(char::is_control), "{label:?}");
        assert!(label.starts_with("[31mevil@example.com ("), "{label}");
        assert!(label.len() <= ACCOUNT_EMAIL_MAX_CHARS + ACCOUNT_PLAN_MAX_CHARS + 3);
    }

    #[test]
    fn usage_limit_guidance_names_account_and_switch_command() {
        let chatgpt = usage_limit_guidance(OAuthProvider::Chatgpt, Some("a@example.com (plus)"));
        assert!(
            chatgpt.contains("a@example.com (plus)"),
            "usage guidance must name the selected account"
        );
        assert!(
            chatgpt.contains("`CODEWHALE_CHATGPT_NEW_ACCOUNT=1 codewhale auth chatgpt`"),
            "usage guidance must name the explicit account replacement command"
        );
        // A running session does not see a shell login. Returning login
        // stays on its verified account; explicit replacement needs a restart.
        assert!(
            chatgpt.contains("`/auth chatgpt` reauthorizes the selected account"),
            "{chatgpt}"
        );
        assert!(
            chatgpt.contains("restart open Codewhale sessions"),
            "{chatgpt}"
        );
        let xai = usage_limit_guidance(OAuthProvider::Xai, None);
        assert!(xai.contains("`codewhale auth xai-device`"), "{xai}");
        assert!(xai.contains("`/auth xai-device`"), "{xai}");
        assert!(!xai.contains("None"), "{xai}");
    }

    #[test]
    fn relogin_with_another_account_replaces_the_owned_entry() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("temp home");
        let root = home.path().canonicalize().expect("canonical home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
        let config_path = root.join("config.toml");
        std::fs::write(&config_path, "").expect("empty config");
        let token_a = id_token(&serde_json::json!({
            "email": "a@example.com",
            "https://api.openai.com/auth": {"chatgpt_plan_type": "plus", "chatgpt_account_id": "acct-a"},
        }));
        let first = activate_login(
            pending_login_with_id_token_for_test(
                OAuthProvider::Chatgpt,
                "access-a",
                "refresh-a",
                Some(&token_a),
            ),
            Some(&config_path),
            None,
        )
        .expect("first login");
        assert_eq!(first.account_label.as_deref(), Some("a@example.com (plus)"));
        assert_eq!(first.replaced, None);
        assert_eq!(
            first.summary(codewhale_localization::Locale::En),
            "Signed in to ChatGPT as a@example.com (plus)."
        );

        // Account B has its own offline grant and no account claim in the
        // display token: nothing from account A may survive into B's credential.
        let token_b = id_token(&serde_json::json!({"email": "b@example.com"}));
        let pending_b = pending_login_with_id_token_for_test(
            OAuthProvider::Chatgpt,
            "access-b",
            "refresh-b",
            Some(&token_b),
        );
        let second = activate_login(pending_b, Some(&config_path), None).expect("second login");
        assert_eq!(second.account_label.as_deref(), Some("b@example.com"));
        assert_eq!(
            second.replaced,
            Some(Some("a@example.com (plus)".to_string()))
        );
        let summary = second.summary(codewhale_localization::Locale::En);
        assert_eq!(
            summary,
            "Signed in to ChatGPT as b@example.com.\nReplaced the previous Codewhale ChatGPT sign-in (a@example.com (plus))."
        );
        assert!(!summary.contains("access-"), "{summary}");
        // The TUI transcript renders the same facts in the UI locale.
        let localized = second.summary(codewhale_localization::Locale::Ja);
        assert_ne!(localized, summary);
        assert!(localized.contains("b@example.com"), "{localized}");
        assert!(localized.contains("a@example.com (plus)"), "{localized}");

        let persisted = std::fs::read_to_string(&second.auth_path).expect("generation");
        assert!(!persisted.contains("refresh-a"), "{persisted}");
        assert!(!persisted.contains("acct-a"), "{persisted}");
        assert!(!persisted.contains("access-a"), "{persisted}");
        let generation = second
            .auth_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("generation name");
        assert_eq!(
            owned_account_label_for_generation(OAuthProvider::Chatgpt, generation)
                .expect("usable sign-in")
                .as_deref(),
            Some("b@example.com")
        );
        let mut config = Config::default();
        config
            .mark_codewhale_owned_chatgpt_oauth(generation.to_string())
            .expect("admitted OAuth fixture");
        assert_eq!(
            usable_sign_in(OAuthProvider::Chatgpt, &config),
            Some(UsableSignIn {
                account_label: Some("b@example.com".to_string())
            })
        );
        // An invalid generation name never resolves to a path.
        assert!(
            owned_account_label_for_generation(OAuthProvider::Chatgpt, "../auth.json").is_err()
        );
    }

    /// A login replaces the whole sign-in, not just its own scope: an older
    /// account's entry under a differently spelled issuer must neither
    /// outrank the new login nor keep its refresh token on disk.
    #[test]
    fn relogin_drops_other_scopes_and_names_the_account_it_replaced() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("temp home");
        let root = home.path().canonicalize().expect("canonical home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
        let config_path = root.join("config.toml");
        std::fs::write(&config_path, "").expect("empty config");
        let token_a = id_token(&serde_json::json!({"email": "a@example.com"}));
        let mut pending_a = pending_login_with_id_token_for_test(
            OAuthProvider::Xai,
            "access-a",
            "refresh-a",
            Some(&token_a),
        );
        // '/' sorts before ':', so this scope comes first in the file.
        pending_a.issuer = format!("{XAI_OIDC_ISSUER}/");
        activate_login(pending_a, Some(&config_path), None).expect("login A");

        let token_b = id_token(&serde_json::json!({"email": "b@example.com"}));
        let second = activate_login(
            pending_login_with_id_token_for_test(
                OAuthProvider::Xai,
                "access-b",
                "refresh-b",
                Some(&token_b),
            ),
            Some(&config_path),
            None,
        )
        .expect("login B");
        assert_eq!(second.replaced, Some(Some("a@example.com".to_string())));
        let persisted = std::fs::read_to_string(&second.auth_path).expect("generation");
        assert!(!persisted.contains("refresh-a"), "{persisted}");
        assert!(!persisted.contains("access-a"), "{persisted}");
        let generation = second
            .auth_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("generation name");
        let mut config = Config::default();
        config
            .mark_codewhale_owned_xai_oauth(generation.to_string())
            .expect("admitted OAuth fixture");
        assert_eq!(
            usable_sign_in(OAuthProvider::Xai, &config)
                .and_then(|sign_in| sign_in.account_label)
                .as_deref(),
            Some("b@example.com")
        );
    }

    /// #6715 review: a login under a non-default client id (for example
    /// `XAI_OIDC_CLIENT_ID`) must become the account requests use.
    /// `select_entry` prefers the built-in client id, so an old account's
    /// entry under that id, carried into the new generation, would keep
    /// winning both the runtime credential and the displayed account.
    #[test]
    fn relogin_under_another_client_id_switches_runtime_credentials_and_label() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("temp home");
        let root = home.path().canonicalize().expect("canonical home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
        let config_path = root.join("config.toml");
        std::fs::write(&config_path, "").expect("empty config");
        let mut config = Config {
            provider: Some(ProviderKind::Xai.as_str().to_string()),
            ..Config::default()
        };
        let token_a = id_token(&serde_json::json!({"email": "a@example.com"}));
        activate_login(
            pending_login_with_id_token_for_test(
                OAuthProvider::Xai,
                "access-a",
                "refresh-a",
                Some(&token_a),
            ),
            Some(&config_path),
            Some(&mut config),
        )
        .expect("login A");
        assert_eq!(
            get_xai_credentials(&config)
                .expect("account A")
                .access_token,
            "access-a"
        );

        let token_b = id_token(&serde_json::json!({"email": "b@example.com"}));
        let mut pending_b = pending_login_with_id_token_for_test(
            OAuthProvider::Xai,
            "access-b",
            "refresh-b",
            Some(&token_b),
        );
        pending_b.client_id = "alternate-client".to_string();
        let second =
            activate_login(pending_b, Some(&config_path), Some(&mut config)).expect("login B");
        assert_eq!(second.account_label.as_deref(), Some("b@example.com"));
        assert_eq!(second.replaced, Some(Some("a@example.com".to_string())));

        // Requests: the runtime credential is account B's, under B's client.
        let runtime = get_xai_credentials(&config).expect("account B");
        assert_eq!(runtime.access_token, "access-b");
        assert_eq!(runtime.client_id, "alternate-client");
        assert_eq!(runtime.account_label.as_deref(), Some("b@example.com"));
        // Status: readiness/picker and `auth status` name account B too.
        assert_eq!(
            usable_sign_in(OAuthProvider::Xai, &config)
                .and_then(|sign_in| sign_in.account_label)
                .as_deref(),
            Some("b@example.com")
        );
        let generation = second
            .auth_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("generation name");
        assert_eq!(
            owned_account_label_for_generation(OAuthProvider::Xai, generation)
                .expect("usable sign-in")
                .as_deref(),
            Some("b@example.com")
        );
        let persisted = std::fs::read_to_string(&second.auth_path).expect("generation");
        assert!(!persisted.contains("refresh-a"), "{persisted}");
    }

    /// The label follows the same usability test as the runtime: an entry
    /// with an expired access token and no refresh token names no account.
    #[test]
    fn stale_owned_entry_names_no_account() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("temp home");
        let root = home.path().canonicalize().expect("canonical home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
        let generation = "chatgpt-auth-0123456789abcdef0123456789abcdef.json";
        let token_a = id_token(&serde_json::json!({"email": "a@example.com"}));
        let file = serde_json::json!({
            format!("{CHATGPT_OAUTH_ISSUER}::{CHATGPT_OAUTH_CLIENT_ID}"): {
                "access_token": "access-stale",
                "expires_at": "2000-01-01T00:00:00Z",
                "id_token": token_a,
            }
        });
        codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
            store.write(generation, file.to_string().as_bytes(), false)
        })
        .expect("seed stale generation");
        let mut config = Config::default();
        config
            .mark_codewhale_owned_chatgpt_oauth(generation.to_string())
            .expect("admitted OAuth fixture");
        assert!(!credentials_valid(OAuthProvider::Chatgpt, &config));
        assert_eq!(usable_sign_in(OAuthProvider::Chatgpt, &config), None);
        let reason = owned_account_label_for_generation(OAuthProvider::Chatgpt, generation)
            .expect_err("stale entry is unusable");
        assert_eq!(reason.to_string(), "sign-in file holds no usable sign-in");
    }

    #[test]
    fn authorize_url_rejects_providers_without_a_browser_flow() {
        let pkce = PkceChallenge {
            verifier: "verifier".into(),
            challenge: "challenge".into(),
        };
        let err = build_authorize_url(
            oauth_provider_params(OAuthProvider::Xai),
            "https://auth.x.ai",
            "client",
            "openid",
            "http://localhost:1455/auth/callback",
            "state-1",
            &pkce,
        )
        .expect_err("xAI has no browser flow");
        assert!(
            format!("{err:#}").contains("no browser sign-in flow"),
            "{err:#}"
        );
    }

    #[test]
    fn callback_success_requires_matching_state() {
        let ok = parse_callback_query(chatgpt(), "code=abc&state=s1").unwrap();
        assert_eq!(accept_callback("s1", ok).unwrap(), "abc");
        let mismatch = parse_callback_query(chatgpt(), "code=abc&state=other").unwrap();
        let err = accept_callback("s1", mismatch).unwrap_err().to_string();
        assert!(err.contains("state did not match"), "{err}");
    }

    #[test]
    fn callback_error_is_user_visible_without_code() {
        let outcome = parse_callback_query(
            chatgpt(),
            "error=access_denied&error_description=nope&state=s1",
        )
        .unwrap();
        let err = accept_callback("s1", outcome).unwrap_err().to_string();
        assert!(err.contains("access_denied"), "{err}");
        assert!(!err.contains("nope"), "{err}");
        assert!(!err.contains("access_token"));
    }

    #[test]
    fn callback_missing_code_fails() {
        let err = parse_callback_query(chatgpt(), "state=s1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing authorization code"), "{err}");
    }

    #[test]
    fn token_exchange_uses_pkce_verifier_against_mock_issuer() {
        let client = MockFormClient::new(vec![(
            200,
            serde_json::json!({
                "access_token": "at-1",
                "refresh_token": "rt-1",
                "expires_in": 3600,
                "id_token": jwt_with_account("acct-9")
            })
            .to_string(),
        )]);
        let token = exchange_authorization_code(
            &client,
            chatgpt(),
            &form_token_url(chatgpt(), CHATGPT_OAUTH_ISSUER),
            CHATGPT_OAUTH_CLIENT_ID,
            "http://localhost:1455/auth/callback",
            "auth-code",
            "verifier",
        )
        .unwrap();
        assert_eq!(token.access_token.as_deref(), Some("at-1"));
        let posts = client.posts.lock().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(
            posts[0].0,
            "https://auth.openai.com/api/accounts/oauth/token"
        );
        let form: std::collections::BTreeMap<_, _> = posts[0].1.iter().cloned().collect();
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["code_verifier"], "verifier");
        assert_eq!(form["code"], "auth-code");
    }

    #[test]
    fn token_exchange_error_does_not_echo_body_secrets() {
        let client = MockFormClient::new(vec![(
            400,
            serde_json::json!({
                "error": "invalid_grant",
                "error_description": "secret-must-not-leak"
            })
            .to_string(),
        )]);
        // OAuthTokenMaterial is deliberately Debug-free; extract the error
        // without demanding a Debug bound on the success type.
        let err = match exchange_authorization_code(
            &client,
            chatgpt(),
            &form_token_url(chatgpt(), CHATGPT_OAUTH_ISSUER),
            CHATGPT_OAUTH_CLIENT_ID,
            "http://localhost:1455/auth/callback",
            "bad",
            "verifier",
        ) {
            Ok(_) => panic!("invalid_grant must fail"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("permanently"), "{err}");
        assert!(!err.contains("secret-must-not-leak"), "{err}");
    }

    #[test]
    fn callback_server_handles_success_and_error_requests() {
        use std::io::Write as _;
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test port");
        listener.set_nonblocking(false).unwrap();
        let addr = listener.local_addr().unwrap();
        let state = "state-xyz".to_string();
        let expected = state.clone();
        let params = chatgpt();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            handle_callback_stream(stream, params, &expected)
        });
        let mut client = std::net::TcpStream::connect(addr).expect("connect");
        write!(
            client,
            "GET /auth/callback?code=tok&state={state} HTTP/1.1\r\nHost: localhost\r\n\r\n"
        )
        .unwrap();
        let code = server.join().expect("server").expect("callback ok");
        assert_eq!(code.0, "tok");

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind error port");
        listener.set_nonblocking(false).unwrap();
        let addr = listener.local_addr().unwrap();
        let params = chatgpt();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            handle_callback_stream(stream, params, "state-xyz")
        });
        let mut client = std::net::TcpStream::connect(addr).expect("connect");
        write!(
            client,
            "GET /auth/callback?error=access_denied&state=state-xyz HTTP/1.1\r\nHost: localhost\r\n\r\n"
        )
        .unwrap();
        let err = server.join().expect("server").unwrap_err().to_string();
        assert!(err.contains("not completed"), "{err}");
    }

    /// The registered redirect URI says `localhost`, which resolves to `::1`
    /// as readily as `127.0.0.1`. A callback arriving on the IPv6 listener has
    /// to be accepted, or an IPv6-first browser hangs until the timeout.
    #[test]
    fn callback_is_accepted_on_either_loopback_family() {
        use std::io::Write as _;
        for addr in [
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            SocketAddr::from((Ipv6Addr::LOCALHOST, 0)),
        ] {
            let Ok(target) = TcpListener::bind(addr) else {
                // A host without this stack cannot exercise it; the other arm
                // still covers the polling loop.
                continue;
            };
            target.set_nonblocking(true).unwrap();
            let target_addr = target.local_addr().unwrap();

            // A second, permanently idle listener stands in for the family the
            // browser did not pick: `wait_for_callback` must poll past it.
            let idle = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
                .expect("bind idle listener");
            idle.set_nonblocking(true).unwrap();

            let listeners = vec![idle, target];
            let params = chatgpt();
            let server =
                std::thread::spawn(move || wait_for_callback(&listeners, params, "state-xyz"));
            let mut client = std::net::TcpStream::connect(target_addr).expect("connect");
            write!(
                client,
                "GET /auth/callback?code=tok&state=state-xyz HTTP/1.1\r\nHost: localhost\r\n\r\n"
            )
            .unwrap();
            let code = server
                .join()
                .expect("server")
                .unwrap_or_else(|error| panic!("callback on {target_addr} rejected: {error}"));
            assert_eq!(code.0, "tok", "callback on {target_addr}");
        }
    }

    #[test]
    fn form_refresh_and_revoke_target_the_row_endpoints() {
        let client = MockFormClient::new(vec![
            (
                200,
                serde_json::json!({"access_token": "fresh", "expires_in": 3600}).to_string(),
            ),
            (200, String::new()),
        ]);
        let refreshed = refresh_access_token_via(
            &client,
            chatgpt(),
            &form_token_url(chatgpt(), CHATGPT_OAUTH_ISSUER),
            CHATGPT_OAUTH_CLIENT_ID,
            "rt-1",
        )
        .expect("refresh");
        assert_eq!(refreshed.access_token.as_deref(), Some("fresh"));
        revoke_remote_token_via(
            &client,
            chatgpt(),
            CHATGPT_OAUTH_ISSUER,
            CHATGPT_OAUTH_CLIENT_ID,
            "rt-1",
        )
        .expect("revoke");
        let posts = client.posts.lock().unwrap();
        assert_eq!(posts.len(), 2, "{posts:?}");
        assert!(posts[0].0.ends_with("/oauth/token"), "{posts:?}");
        assert!(
            posts[0]
                .1
                .iter()
                .any(|(k, v)| k == "grant_type" && v == "refresh_token")
        );
        assert!(
            posts[1].0.ends_with("/api/accounts/oauth/revoke"),
            "{posts:?}"
        );
    }

    // ────────────────────────────────────────────────────────────────────
    // Ported from the deleted per-provider modules (§D security pins).
    // ────────────────────────────────────────────────────────────────────

    use tempfile::TempDir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn auth_mode_accepts_oauth_aliases() {
        for mode in [
            "oauth",
            "xai_oauth",
            "XAI-OAuth",
            "grok",
            "grok_cli",
            "device_code",
            "device-auth",
        ] {
            assert!(
                auth_mode_uses_xai_oauth(mode),
                "expected oauth mode: {mode}"
            );
        }
        assert!(!auth_mode_uses_xai_oauth("api_key"));
        assert!(!auth_mode_uses_xai_oauth("keyring"));
    }

    #[test]
    fn loads_fresh_token_from_grok_auth_json() {
        let _guard = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().expect("canonical temp root");
        let path = root.join("auth.json");
        let future = rfc3339_from_now(3600);
        let scope = format!("{XAI_OIDC_ISSUER}::{GROK_OIDC_CLIENT_ID}");
        let file = serde_json::json!({
            scope: {
                "key": "test-access-token",
                "refresh_token": "test-refresh",
                "expires_at": future,
                "oidc_issuer": XAI_OIDC_ISSUER,
                "oidc_client_id": GROK_OIDC_CLIENT_ID,
                "auth_mode": "oidc"
            }
        });
        fs::write(&path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
        let _home_guard = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
        let _path_guard = crate::test_support::EnvVarGuard::set("GROK_AUTH_PATH", &path);
        let config = Config {
            provider: Some(ProviderKind::Xai.as_str().to_string()),
            providers: Some(crate::config::ProvidersConfig {
                xai: crate::config::ProviderConfig {
                    auth_mode: Some("oauth".to_string()),
                    external_credentials: Some(
                        codewhale_config::ExternalCredentialConsentToml::read_only(
                            codewhale_config::ProviderKind::Xai,
                            codewhale_config::ExternalCredentialSource::GrokCli,
                            path.clone(),
                        ),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        crate::external_credentials::reset_side_effect_trap();
        let result = get_xai_credentials(&config);
        let creds = result.expect("load");
        assert_eq!(creds.access_token, "test-access-token");
        assert_eq!(creds.client_id, GROK_OIDC_CLIENT_ID);
        assert_eq!(
            crate::external_credentials::side_effect_trap_counts(),
            (1, 1)
        );
    }

    #[test]
    fn disabled_external_grok_credentials_cause_zero_external_io() {
        let _guard = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().expect("canonical temp root");
        let path = root.join("external-grok-auth.json");
        let raw = serde_json::json!({
            format!("{XAI_OIDC_ISSUER}::{GROK_OIDC_CLIENT_ID}"): {
                "key": "must-never-be-read",
                "refresh_token": "must-never-be-used",
                "expires_at": rfc3339_from_now(3600),
                "future_field": {"preserve": true}
            }
        })
        .to_string();
        fs::write(&path, &raw).unwrap();
        let owned_home = root.join("codewhale-owned");
        let _home_guard = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &owned_home);
        let _path_guard = crate::test_support::EnvVarGuard::set("GROK_AUTH_PATH", &path);
        let config = Config {
            provider: Some(ProviderKind::Xai.as_str().to_string()),
            providers: Some(crate::config::ProvidersConfig {
                xai: crate::config::ProviderConfig {
                    auth_mode: Some("oauth".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        crate::external_credentials::reset_side_effect_trap();
        assert!(!credentials_valid(OAuthProvider::Xai, &config));
        let error = match get_xai_credentials(&config) {
            Ok(_) => panic!("external access is disabled"),
            Err(e) => e,
        };
        assert!(error.to_string().contains("are disabled"));
        assert_eq!(
            crate::external_credentials::side_effect_trap_counts(),
            (0, 0)
        );
        assert_eq!(
            crate::external_credentials::complete_side_effect_trap_counts(),
            (0, 0, 0, 0, 0),
            "disabled external authority must reach no credential or OAuth sink"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), raw);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn expired_read_only_external_credentials_never_refresh_rewrite_or_network() {
        let _guard = crate::test_support::lock_test_env();
        let server = MockServer::start().await;
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().expect("canonical temp root");
        let path = root.join("external-grok-auth.json");
        let scope = format!("{}::{GROK_OIDC_CLIENT_ID}", server.uri());
        let raw = serde_json::json!({
            scope: {
                "key": "expired-external-access",
                "refresh_token": "must-never-be-submitted",
                "expires_at": rfc3339_from_unix(now_unix_secs().unwrap_or(0) - 3600),
                "oidc_issuer": server.uri(),
                "oidc_client_id": GROK_OIDC_CLIENT_ID,
                "future_field": {"preserve": true}
            }
        })
        .to_string();
        fs::write(&path, &raw).unwrap();
        let owned_home = root.join("codewhale-owned");
        let _home_guard = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &owned_home);
        let _path_guard = crate::test_support::EnvVarGuard::set("GROK_AUTH_PATH", &path);
        let config = Config {
            provider: Some(ProviderKind::Xai.as_str().to_string()),
            providers: Some(crate::config::ProvidersConfig {
                xai: crate::config::ProviderConfig {
                    auth_mode: Some("oauth".to_string()),
                    external_credentials: Some(
                        codewhale_config::ExternalCredentialConsentToml::read_only(
                            codewhale_config::ProviderKind::Xai,
                            codewhale_config::ExternalCredentialSource::GrokCli,
                            path.clone(),
                        ),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        crate::external_credentials::reset_side_effect_trap();
        let error = match tokio::task::block_in_place(|| get_xai_credentials(&config)) {
            Ok(_) => panic!("read-only external credentials must fail instead of refreshing"),
            Err(e) => e,
        };
        assert!(
            error
                .to_string()
                .contains("Read-only consent never refreshes")
        );
        assert_eq!(
            crate::external_credentials::side_effect_trap_counts(),
            (1, 1)
        );
        assert_eq!(
            crate::external_credentials::complete_side_effect_trap_counts(),
            (1, 1, 0, 0, 0),
            "read-only external expiry must not reach write, refresh, or network sinks"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), raw);
        assert!(!owned_home.join("credentials/xai-auth.json").exists());
        assert!(
            server
                .received_requests()
                .await
                .expect("recorded requests")
                .is_empty(),
            "external refresh tokens must never be sent over the network"
        );
    }

    /// #4763 root trigger, re-pinned for #5772. A returning xAI-OAuth user
    /// whose only material is an external Grok CLI grant loses readiness the
    /// moment that CLI's short-lived access token expires, even though a
    /// refresh token sits right beside it — read-only consent deliberately
    /// never refreshes or rewrites another CLI's file, so there is nothing to
    /// renew it with. `needs_api_key` therefore flips to true and onboarding
    /// reopens. That is the intended invariant, not a leak, and it is what
    /// stops a surviving consent record from reading as a stored credential;
    /// this test pins it so the onboarding entry point stays explainable.
    #[test]
    fn expired_external_grok_grant_reads_as_missing_key_despite_refresh_token() {
        let _guard = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().expect("canonical temp root");
        let path = root.join("external-grok-auth.json");
        let scope = format!("https://auth.x.ai::{GROK_OIDC_CLIENT_ID}");
        fs::write(
            &path,
            serde_json::json!({
                scope.clone(): {
                    "key": "expired-external-access",
                    "refresh_token": "present-but-unusable-under-read-only-consent",
                    "expires_at": rfc3339_from_unix(now_unix_secs().unwrap_or(0) - 3600),
                    "oidc_client_id": GROK_OIDC_CLIENT_ID,
                }
            })
            .to_string(),
        )
        .unwrap();
        let _home_guard =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.join("codewhale-owned"));
        let _path_guard = crate::test_support::EnvVarGuard::set("GROK_AUTH_PATH", &path);
        let _key_guard = crate::test_support::EnvVarGuard::remove("XAI_API_KEY");
        let config = Config {
            provider: Some(ProviderKind::Xai.as_str().to_string()),
            providers: Some(crate::config::ProvidersConfig {
                xai: crate::config::ProviderConfig {
                    auth_mode: Some("oauth".to_string()),
                    external_credentials: Some(
                        codewhale_config::ExternalCredentialConsentToml::read_only(
                            codewhale_config::ProviderKind::Xai,
                            codewhale_config::ExternalCredentialSource::GrokCli,
                            path.clone(),
                        ),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        crate::external_credentials::reset_side_effect_trap();
        assert!(
            !credentials_present(OAuthProvider::Xai, &config),
            "an expired external access token is not usable material"
        );
        assert!(
            !crate::config::has_api_key_for(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Xai)
            ),
            "expired external xAI OAuth must fall through to the missing-key path"
        );
        assert_eq!(
            crate::external_credentials::complete_side_effect_trap_counts().2,
            0,
            "a consented read never rewrites Codewhale-owned storage"
        );
        assert_eq!(
            crate::external_credentials::complete_side_effect_trap_counts().3,
            0,
            "a consented read never refreshes another CLI's token"
        );

        // The same file with a live access token is ready, so the check is
        // expiry-driven rather than a blanket rejection of external grants.
        fs::write(
            &path,
            serde_json::json!({
                scope: {
                    "key": "fresh-external-access",
                    "refresh_token": "unused",
                    "expires_at": rfc3339_from_now(3600),
                    "oidc_client_id": GROK_OIDC_CLIENT_ID,
                }
            })
            .to_string(),
        )
        .unwrap();
        assert!(credentials_present(OAuthProvider::Xai, &config));
        assert!(crate::config::has_api_key_for(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Xai)
        ));
    }

    #[test]
    fn native_login_storage_is_codewhale_owned() {
        let _guard = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let grok_path = dir.path().join("external-grok-auth.json");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let _grok = crate::test_support::EnvVarGuard::set("GROK_AUTH_PATH", &grok_path);

        let owned = codewhale_config::legacy_xai_oauth_path().expect("Codewhale-owned auth path");
        assert_eq!(owned, dir.path().join("credentials/xai-auth.json"));
        assert_ne!(owned, grok_auth_file_path());
    }

    /// #4257 storage contract: the on-disk credential file is a JSON object
    /// keyed `{issuer}::{client_id}` whose entries use the Grok CLI's field
    /// names. Consolidating the device-code poller must not touch it, so pin
    /// the format with literal bytes rather than a round-trip through the
    /// writer — a round-trip would follow the code if the code drifted.
    #[test]
    fn a_token_stored_in_the_current_on_disk_format_still_loads() {
        let _guard = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("owned-home");
        fs::create_dir_all(&home).unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);

        let generation = "xai-auth-fedcba9876543210fedcba9876543210.json";
        let expires_at = rfc3339_from_now(3600);
        let stored = format!(
            r#"{{
  "https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828": {{
    "key": "stored-access-token",
    "refresh_token": "stored-refresh-token",
    "expires_at": "{expires_at}",
    "oidc_issuer": "https://auth.x.ai",
    "oidc_client_id": "b1a00492-073a-47ea-816f-4c329264a828",
    "auth_mode": "oidc",
    "unknown_cli_field": "preserved"
  }}
}}"#
        );

        let credentials = codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
            store.write(generation, stored.as_bytes(), false)?;
            // A fresh stored token must be used as-is: refreshing here would
            // mean an existing login stopped working offline.
            get_owned_credentials_locked(
                OAuthProvider::Xai,
                store,
                generation,
                &ReqwestOAuthFormClient,
                |_, _, _| panic!("a fresh stored token must not be refreshed"),
            )
        })
        .expect("read back a credential stored in the current format");

        assert_eq!(credentials.access_token, "stored-access-token");
        assert_eq!(
            credentials.refresh_token.as_deref(),
            Some("stored-refresh-token")
        );
        assert_eq!(credentials.issuer, XAI_OIDC_ISSUER);
        assert_eq!(credentials.client_id, GROK_OIDC_CLIENT_ID);
    }

    /// `Debug` reaches production through tracing's `?` sigil, anyhow context,
    /// and panic messages. Nothing that holds bearer material may print it.
    #[test]
    fn debug_output_never_contains_bearer_material() {
        let entry = OwnedAuthEntry {
            access_token: Some("secret-access-token".to_string()),
            refresh_token: Some("secret-refresh-token".to_string()),
            expires_at: Some("2030-01-01T00:00:00.000Z".to_string()),
            id_token: None,
            account_id: None,
            oidc_issuer: Some(XAI_OIDC_ISSUER.to_string()),
            oidc_client_id: Some(GROK_OIDC_CLIENT_ID.to_string()),
            originator: None,
            auth_mode: Some("oidc".to_string()),
            extra: BTreeMap::new(),
        };
        let credentials = credentials_from_entry(
            OAuthProvider::Xai,
            &format!("{XAI_OIDC_ISSUER}::{GROK_OIDC_CLIENT_ID}"),
            &entry,
            "secret-access-token".to_string(),
        );
        let activation = OAuthActivation {
            credentials: credentials.clone(),
            config_path: PathBuf::from("/tmp/config.toml"),
            auth_path: PathBuf::from("/tmp/auth.json"),
            provider: OAuthProvider::Xai,
            account_label: None,
            replaced: None,
        };

        let rendered = format!("{entry:?} {activation:?}");
        for secret in ["secret-access-token", "secret-refresh-token"] {
            assert!(!rendered.contains(secret), "{secret} leaked: {rendered}");
        }
        // The shape stays useful for diagnosis.
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains(GROK_OIDC_CLIENT_ID), "{rendered}");
    }

    fn pending_login(access: &str, refresh: &str) -> PendingOAuthLogin {
        pending_login_for_test(OAuthProvider::Xai, access, refresh)
    }

    fn seed_expired_owned_generation() -> String {
        let generation = "xai-auth-0123456789abcdef0123456789abcdef.json".to_string();
        codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
            let scope = format!("{}::{}", XAI_OIDC_ISSUER, GROK_OIDC_CLIENT_ID);
            let mut file = AuthFile::new();
            file.insert(
                scope,
                OwnedAuthEntry {
                    access_token: Some("expired-access".to_string()),
                    refresh_token: Some("initial-refresh".to_string()),
                    expires_at: Some("1970-01-01T00:00:00.000Z".to_string()),
                    id_token: None,
                    account_id: None,
                    oidc_issuer: Some(XAI_OIDC_ISSUER.to_string()),
                    oidc_client_id: Some(GROK_OIDC_CLIENT_ID.to_string()),
                    originator: None,
                    auth_mode: Some("oidc".to_string()),
                    extra: BTreeMap::new(),
                },
            );
            write_auth_file_to_store(store, &generation, &file, false)
        })
        .expect("seed expired owned generation");
        generation
    }

    fn seed_legacy_owned_credentials() -> PathBuf {
        codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
            let scope = format!("{}::{}", XAI_OIDC_ISSUER, GROK_OIDC_CLIENT_ID);
            let mut legacy = AuthFile::new();
            legacy.insert(
                scope,
                OwnedAuthEntry {
                    access_token: Some("legacy-access".to_string()),
                    refresh_token: Some("legacy-refresh".to_string()),
                    expires_at: Some(rfc3339_from_now(3600)),
                    id_token: None,
                    account_id: None,
                    oidc_issuer: Some(XAI_OIDC_ISSUER.to_string()),
                    oidc_client_id: Some(GROK_OIDC_CLIENT_ID.to_string()),
                    originator: None,
                    auth_mode: Some("oidc".to_string()),
                    extra: BTreeMap::new(),
                },
            );
            write_auth_file_to_store(
                store,
                codewhale_config::LEGACY_XAI_OAUTH_FILE_NAME,
                &legacy,
                false,
            )?;
            store.path_for(codewhale_config::LEGACY_XAI_OAUTH_FILE_NAME)
        })
        .expect("seed legacy credentials")
    }

    #[test]
    fn concurrent_refreshes_share_one_rotated_epoch() {
        let _guard = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("owned-home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
        let generation = seed_expired_owned_generation();
        let refreshes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();

        let first_generation = generation.clone();
        let first_refreshes = refreshes.clone();
        let first = std::thread::spawn(move || {
            codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
                get_owned_credentials_locked(
                    OAuthProvider::Xai,
                    store,
                    &first_generation,
                    &ReqwestOAuthFormClient,
                    |_, _, refresh| {
                        assert_eq!(refresh, "initial-refresh");
                        first_refreshes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(OAuthTokenMaterial {
                            earliest_refresh_at: None,
                            scope: None,
                            token_type: None,
                            verified_chatgpt: None,
                            id_token: None,
                            access_token: Some("rotated-access".to_string()),
                            refresh_token: Some("rotated-refresh".to_string()),
                            expires_in: Some(3600),
                            error: None,
                            error_description: None,
                            interval: None,
                        })
                    },
                )
            })
        });
        entered_rx.recv().expect("first refresh reached barrier");

        let second_generation = generation.clone();
        let second_refreshes = refreshes.clone();
        let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
        let second = std::thread::spawn(move || {
            attempt_tx.send(()).unwrap();
            codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
                get_owned_credentials_locked(
                    OAuthProvider::Xai,
                    store,
                    &second_generation,
                    &ReqwestOAuthFormClient,
                    |_, _, _| {
                        second_refreshes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        bail!("second refresh must observe the first thread's committed token")
                    },
                )
            })
        });
        attempt_rx.recv().expect("second refresh attempted lock");
        release_tx.send(()).expect("release first refresh");

        let first = first.join().unwrap().expect("first refresh");
        let second = second.join().unwrap().expect("second refresh");
        assert_eq!(first.access_token, "rotated-access");
        assert_eq!(second.access_token, "rotated-access");
        assert_eq!(refreshes.load(std::sync::atomic::Ordering::SeqCst), 1);
        codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
            let mut file = load_owned_auth_file_from_store(store, &generation)?
                .context("generation must remain active")?;
            let (_, entry) = select_entry(OAuthProvider::Xai, &mut file).context("stored entry")?;
            assert_eq!(entry.refresh_token.as_deref(), Some("rotated-refresh"));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn logout_waits_for_refresh_then_revokes_the_committed_epoch() {
        let _guard = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("owned-home");
        fs::create_dir_all(&home).unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
        let generation = seed_expired_owned_generation();
        fs::write(
            home.join("config.toml"),
            format!(
                "[providers.xai]\nauth_mode = \"oauth\"\noauth_credential_generation = \"{generation}\"\n"
            ),
        )
        .unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();

        let refresh_generation = generation.clone();
        let refresh = std::thread::spawn(move || {
            codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
                get_owned_credentials_locked(
                    OAuthProvider::Xai,
                    store,
                    &refresh_generation,
                    &ReqwestOAuthFormClient,
                    |_, _, _| {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(OAuthTokenMaterial {
                            earliest_refresh_at: None,
                            scope: None,
                            token_type: None,
                            verified_chatgpt: None,
                            id_token: None,
                            access_token: Some("last-refresh-access".to_string()),
                            refresh_token: Some("last-refresh-rotation".to_string()),
                            expires_in: Some(3600),
                            error: None,
                            error_description: None,
                            interval: None,
                        })
                    },
                )
            })
        });
        entered_rx.recv().expect("refresh reached barrier");

        let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
        let config_path = home.join("config.toml");
        let logout = std::thread::spawn(move || {
            attempt_tx.send(()).unwrap();
            codewhale_config::with_xai_oauth_revocation_transaction(|| {
                codewhale_config::mutate_config_document(&config_path, |document| {
                    codewhale_config::unset_config_document_value(
                        document,
                        &["providers", "xai", "oauth_credential_generation"],
                    )?;
                    codewhale_config::unset_config_document_value(
                        document,
                        &["providers", "xai", "auth_mode"],
                    )?;
                    Ok(())
                })
            })
        });
        attempt_rx.recv().expect("logout attempted lifecycle lock");
        release_tx.send(()).expect("release refresh");

        assert_eq!(
            refresh.join().unwrap().expect("refresh").access_token,
            "last-refresh-access"
        );
        logout.join().unwrap().expect("logout");
        let auth_path = home.join("credentials").join(&generation);
        assert!(
            !auth_path.exists(),
            "logout must retire the generation written by the preceding refresh"
        );
        let config = fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(!config.contains("oauth_credential_generation"));
        assert!(!config.contains("auth_mode"));
    }

    #[test]
    fn activation_commits_unique_generation_pointer_and_revokes_external_consent() {
        let _guard = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("owned-home");
        let config_path = dir.path().join("config.toml");
        let external_path = dir.path().join("grok-external.json");
        fs::write(&external_path, "external owner bytes").unwrap();
        fs::write(
            &config_path,
            format!(
                r#"# operator note
[providers.xai]
model = "grok-code-fast-1" # model note
future_setting = "preserve"

[providers.xai.external_credentials]
access = "read_only"
provider = "xai"
source = "grok_cli"
path = {}
consent_version = 1
"#,
                toml::Value::String(external_path.display().to_string())
            ),
        )
        .unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
        let consent = codewhale_config::ExternalCredentialConsentToml::read_only(
            codewhale_config::ProviderKind::Xai,
            codewhale_config::ExternalCredentialSource::GrokCli,
            external_path.clone(),
        );
        let mut live = Config {
            providers: Some(crate::config::ProvidersConfig {
                xai: crate::config::ProviderConfig {
                    model: Some("grok-code-fast-1".to_string()),
                    external_credentials: Some(consent),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        crate::external_credentials::reset_side_effect_trap();
        let activation = activate_login(
            pending_login("activation-access", "activation-refresh"),
            Some(&config_path),
            Some(&mut live),
        )
        .expect("activate login");

        assert_eq!(activation.config_path, config_path);
        let generation = activation
            .auth_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("generation basename");
        assert!(codewhale_config::is_valid_xai_oauth_generation(generation));
        let persisted = fs::read_to_string(&config_path).unwrap();
        assert!(persisted.contains("# operator note"));
        assert!(persisted.contains("model = \"grok-code-fast-1\" # model note"));
        assert!(persisted.contains("future_setting = \"preserve\""));
        assert!(persisted.contains("auth_mode = \"oauth\""));
        assert!(persisted.contains(&format!("oauth_credential_generation = \"{generation}\"")));
        assert!(!persisted.contains("external_credentials"));
        assert_eq!(
            fs::read_to_string(&external_path).unwrap(),
            "external owner bytes"
        );
        let owned = fs::read_to_string(&activation.auth_path).unwrap();
        assert!(owned.contains("activation-access"));
        assert!(owned.contains("activation-refresh"));
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&activation.auth_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let live_xai = live
            .provider_config_for(&live.test_identity_for_kind(ProviderKind::Xai))
            .unwrap();
        assert_eq!(live_xai.auth_mode.as_deref(), Some("oauth"));
        assert_eq!(
            live_xai.oauth_credential_generation.as_deref(),
            Some(generation)
        );
        assert!(live_xai.external_credentials.is_none());
        assert_eq!(
            crate::external_credentials::complete_side_effect_trap_counts(),
            (0, 0, 1, 0, 0),
            "activation must reach exactly the owned write sink"
        );
    }

    #[test]
    fn activation_retires_legacy_owned_file_only_after_config_commit() {
        let _guard = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("owned-home");
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, "[providers.xai]\nmodel = \"grok-4.5\"\n").unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
        let legacy_path = seed_legacy_owned_credentials();
        assert!(legacy_path.exists());

        let activation = activate_login(
            pending_login("new-access", "new-refresh"),
            Some(&config_path),
            None,
        )
        .expect("activate replacement generation");

        assert!(activation.auth_path.exists());
        assert!(
            !legacy_path.exists(),
            "legacy duplicate must be removed after the generation pointer commits"
        );
        let persisted = fs::read_to_string(config_path).unwrap();
        assert!(persisted.contains(activation.auth_path.file_name().unwrap().to_str().unwrap()));
    }

    #[test]
    fn activation_rotation_cleans_only_the_superseded_generation_after_commit() {
        let _guard = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("owned-home");
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, "[providers.xai]\nmodel = \"grok-4.5\"\n").unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
        let mut live = Config::default();

        let first = activate_login(
            pending_login("first-access", "first-refresh"),
            Some(&config_path),
            Some(&mut live),
        )
        .expect("first activation");
        assert!(first.auth_path.exists());
        let first_name = first
            .auth_path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let second = activate_login(
            pending_login("second-access", "second-refresh"),
            Some(&config_path),
            Some(&mut live),
        )
        .expect("second activation");
        assert_ne!(first.auth_path, second.auth_path);
        assert!(second.auth_path.exists());
        assert!(
            !first.auth_path.exists(),
            "superseded generation must be removed only after the new pointer commits"
        );
        let persisted = fs::read_to_string(&config_path).unwrap();
        assert!(!persisted.contains(&first_name));
        assert!(persisted.contains(second.auth_path.file_name().unwrap().to_str().unwrap()));
        assert!(
            fs::read_to_string(second.auth_path)
                .unwrap()
                .contains("second-access")
        );
    }

    #[test]
    fn activation_recovers_from_a_dangling_generation_pointer() {
        let _guard = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("owned-home");
        let config_path = dir.path().join("config.toml");
        // A valid-looking generation pointer whose credential file does not
        // exist: the state Hunter's dogfood machine was bricked in (#5032).
        let stale = "xai-auth-0123456789abcdef0123456789abcdef.json";
        fs::write(
            &config_path,
            format!(
                "[providers.xai]\nauth_mode = \"oauth\"\noauth_credential_generation = \"{stale}\"\n"
            ),
        )
        .unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
        let mut live = Config::default();

        let activation = activate_login(
            pending_login("recovered-access", "recovered-refresh"),
            Some(&config_path),
            Some(&mut live),
        )
        .expect("a dangling generation pointer must not brick login");
        assert!(activation.auth_path.exists());
        assert!(
            fs::read_to_string(&activation.auth_path)
                .unwrap()
                .contains("recovered-access")
        );
        let persisted = fs::read_to_string(&config_path).unwrap();
        assert!(
            !persisted.contains(stale),
            "stale pointer must be replaced: {persisted}"
        );
        assert!(persisted.contains(activation.auth_path.file_name().unwrap().to_str().unwrap()));
        assert!(persisted.contains("auth_mode = \"oauth\""));
    }

    #[test]
    fn dangling_generation_pointer_is_detected_and_repaired() {
        let _guard = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("owned-home");
        let config_path = dir.path().join("config.toml");
        // A valid-looking generation pointer whose credential file does not
        // exist: the state Hunter's dogfood machine was bricked in (#5032).
        let stale = "xai-auth-0123456789abcdef0123456789abcdef.json";
        fs::write(
            &config_path,
            format!(
                "[providers.xai]\nauth_mode = \"oauth\"\noauth_credential_generation = \"{stale}\"\n"
            ),
        )
        .unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);

        let config = Config {
            provider: Some(ProviderKind::Xai.as_str().to_string()),
            providers: Some(crate::config::ProvidersConfig {
                xai: crate::config::ProviderConfig {
                    auth_mode: Some("oauth".to_string()),
                    oauth_credential_generation: Some(stale.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        assert!(
            owned_generation_is_dangling(OAuthProvider::Xai, &config),
            "OAuth mode pointing at a missing owned file is the #5032 bricked state"
        );
        // Specificity: OAuth selected but no generation configured is the normal
        // "needs auth" state, not a dangling pointer.
        let unconfigured = Config {
            provider: Some(ProviderKind::Xai.as_str().to_string()),
            providers: Some(crate::config::ProvidersConfig {
                xai: crate::config::ProviderConfig {
                    auth_mode: Some("oauth".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(
            !owned_generation_is_dangling(OAuthProvider::Xai, &unconfigured),
            "an unconfigured OAuth mode must not be reported as dangling"
        );

        clear_dangling_generation(OAuthProvider::Xai, Some(&config_path))
            .expect("best-effort repair must clear the stale pointer");

        let persisted = fs::read_to_string(&config_path).unwrap();
        assert!(
            !persisted.contains(stale),
            "stale generation pointer must be cleared: {persisted}"
        );
        assert!(
            persisted.contains("auth_mode = \"oauth\""),
            "the user's OAuth mode selection must be preserved: {persisted}"
        );
    }

    #[test]
    fn activation_rejects_a_non_string_generation_pointer_without_staging_credentials() {
        let _guard = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("owned-home");
        let config_path = dir.path().join("config.toml");
        let original = "[providers.xai]\noauth_credential_generation = { path = \"attacker\" }\n";
        fs::write(&config_path, original).unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);

        let error = activate_login(
            pending_login("must-not-stage", "must-not-persist"),
            Some(&config_path),
            None,
        )
        .expect_err("non-string generation pointers must fail closed");
        assert!(error.to_string().contains("not activated"), "{error:#}");
        assert_eq!(fs::read_to_string(&config_path).unwrap(), original);
        let credentials = home.join("credentials");
        assert!(credentials.exists(), "lifecycle lock directory is durable");
        assert!(fs::read_dir(credentials).unwrap().all(|entry| {
            let name = entry.unwrap().file_name();
            let name = name.to_string_lossy();
            name != codewhale_config::LEGACY_XAI_OAUTH_FILE_NAME
                && !codewhale_config::is_valid_xai_oauth_generation(&name)
        }));
    }

    #[cfg(unix)]
    #[test]
    fn activation_failure_cleans_unreferenced_stage_and_keeps_live_config_inert() {
        let _guard = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("owned-home");
        let config_dir = dir.path().join("config-parent");
        fs::create_dir(&config_dir).unwrap();
        let config_path = config_dir.join("config.toml");
        fs::write(&config_path, "[providers.xai]\nauth_mode = \"api_key\"\n").unwrap();
        fs::create_dir(config_dir.join("config.toml.bak")).unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
        let legacy_path = seed_legacy_owned_credentials();
        let legacy_before = fs::read(&legacy_path).unwrap();
        let mut live = Config {
            providers: Some(crate::config::ProvidersConfig {
                xai: crate::config::ProviderConfig {
                    auth_mode: Some("api_key".to_string()),
                    api_key: Some("still-selected".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        let result = activate_login(
            pending_login("must-be-cleaned", "must-not-persist"),
            Some(&config_path),
            Some(&mut live),
        );
        let error = result.expect_err("invalid backup path must fail activation");
        assert!(error.to_string().contains("not activated"), "{error:#}");
        let live_xai = live
            .provider_config_for(&live.test_identity_for_kind(ProviderKind::Xai))
            .unwrap();
        assert_eq!(live_xai.auth_mode.as_deref(), Some("api_key"));
        assert!(live_xai.oauth_credential_generation.is_none());
        assert_eq!(
            fs::read(&legacy_path).unwrap(),
            legacy_before,
            "legacy owned credentials must remain byte-identical until activation commits"
        );
        let credentials = home.join("credentials");
        if credentials.exists() {
            assert!(
                fs::read_dir(credentials).unwrap().all(|entry| {
                    let name = entry.unwrap().file_name();
                    let name = name.to_string_lossy();
                    name == codewhale_config::LEGACY_XAI_OAUTH_FILE_NAME
                        || !codewhale_config::is_valid_xai_oauth_generation(&name)
                }),
                "failed activation must remove every unreferenced generation but retain legacy"
            );
        }
        assert!(
            !fs::read_to_string(config_path)
                .unwrap()
                .contains("must-be-cleaned")
        );
    }

    #[test]
    fn missing_file_message_mentions_oauth_paths() {
        let _guard = crate::test_support::lock_test_env();
        let msg = missing_auth_message(OAuthProvider::Xai);
        assert!(msg.contains("xAI OAuth credentials not found"), "{msg}");
        assert!(msg.contains("external-consent"), "{msg}");
        assert!(msg.contains("Codewhale-owned OAuth storage"), "{msg}");
        assert!(msg.contains("XAI_API_KEY"), "{msg}");
    }

    #[test]
    fn parse_rfc3339_accepts_zulu() {
        let ts = parse_rfc3339_secs("2026-07-09T12:00:00.000Z").expect("parse");
        assert!(ts > 0);
    }

    #[test]
    fn device_code_constants_match_discovery_shape() {
        assert_eq!(
            DEFAULT_SCOPES.split_whitespace().collect::<Vec<_>>(),
            [
                "openid",
                "profile",
                "email",
                "offline_access",
                "api:access",
                "grok-cli:access",
            ]
        );
        assert_eq!(XAI_OIDC_ISSUER, "https://auth.x.ai");
        assert_eq!(GROK_OIDC_CLIENT_ID.len(), 36);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn discovery_binds_to_advertised_endpoints() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": server.uri(),
                "device_authorization_endpoint": format!("{}/oauth2/device-advertised", server.uri()),
                "token_endpoint": format!("{}/oauth2/token-advertised", server.uri())
            })))
            .expect(1)
            .mount(&server)
            .await;

        let endpoints = tokio::task::block_in_place(|| {
            discover_oauth_endpoints(&XAI_OAUTH_PARAMS, &server.uri()).expect("discover endpoints")
        });

        assert_eq!(
            endpoints,
            OAuthEndpoints {
                device_authorization_endpoint: Some(format!(
                    "{}/oauth2/device-advertised",
                    server.uri()
                )),
                token_endpoint: format!("{}/oauth2/token-advertised", server.uri()),
            }
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refresh_uses_discovered_token_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": server.uri(),
                "device_authorization_endpoint": format!("{}/oauth2/device-advertised", server.uri()),
                "token_endpoint": format!("{}/oauth2/token-advertised", server.uri())
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token-advertised"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "refreshed-access",
                "refresh_token": "rotated-refresh",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;

        let token = tokio::task::block_in_place(|| {
            refresh_for_provider(
                OAuthProvider::Xai,
                &ReqwestOAuthFormClient,
                &server.uri(),
                GROK_OIDC_CLIENT_ID,
                "refresh-secret",
            )
            .expect("refresh token")
        });

        assert_eq!(token.access_token.as_deref(), Some("refreshed-access"));
        assert_eq!(token.refresh_token.as_deref(), Some("rotated-refresh"));
    }

    #[test]
    fn https_discovery_rejects_plaintext_endpoint_downgrade() {
        let error = validate_discovered_oauth_endpoint(
            Some("http://auth.x.ai/oauth2/device/code".to_string()),
            "device_authorization_endpoint",
            XAI_OIDC_ISSUER,
        )
        .expect_err("HTTPS issuer must reject an HTTP endpoint");

        assert!(error.to_string().contains("downgrade"), "{error}");
    }

    #[test]
    fn https_discovery_accepts_same_origin_with_explicit_default_port() {
        let endpoint = "https://auth.x.ai:443/oauth2/token";
        let validated = validate_discovered_oauth_endpoint(
            Some(endpoint.to_string()),
            "token_endpoint",
            XAI_OIDC_ISSUER,
        )
        .expect("URL origins normalize the explicit default HTTPS port");

        assert_eq!(validated, endpoint);
    }

    #[test]
    fn https_discovery_rejects_cross_origin_endpoint() {
        let error = validate_discovered_oauth_endpoint(
            Some("https://oauth.attacker.example/oauth2/token".to_string()),
            "token_endpoint",
            XAI_OIDC_ISSUER,
        )
        .expect_err("discovered OAuth endpoints must stay on the issuer origin");

        assert!(error.to_string().contains("different origin"), "{error}");
    }

    #[test]
    fn discovery_rejects_mismatched_issuer() {
        let error = validate_discovered_issuer(
            Some("https://attacker.example".to_string()),
            XAI_OIDC_ISSUER,
        )
        .expect_err("discovery issuer must bind to the request issuer");

        assert!(error.to_string().contains("does not match"), "{error}");
    }

    #[test]
    fn oauth_error_details_collapse_control_whitespace() {
        let detail = oauth_failure_detail(
            Some("invalid_scope\nforged"),
            Some("bad\t scope\r\nnext line"),
            reqwest::StatusCode::BAD_REQUEST,
        );

        assert!(
            !detail
                .chars()
                .any(|character| matches!(character, '\n' | '\r' | '\t')),
            "{detail}"
        );
        assert!(detail.contains("invalid_scope forged"), "{detail}");
        assert!(detail.contains("bad scope next line"), "{detail}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn discovery_failure_uses_documented_endpoint_fallback() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(
                ResponseTemplate::new(503)
                    .set_body_raw("<html>temporarily unavailable</html>", "text/html"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let endpoints = tokio::task::block_in_place(|| {
            resolve_oauth_endpoints(&XAI_OAUTH_PARAMS, &server.uri())
        });

        assert_eq!(
            endpoints,
            OAuthEndpoints {
                device_authorization_endpoint: Some(format!("{}/oauth2/device/code", server.uri())),
                token_endpoint: format!("{}/oauth2/token", server.uri()),
            }
        );
    }

    /// End-to-end regression for the v0.9.4 dogfood failure (#5032): starting
    /// from the exact state the dogfood machine was bricked in — a
    /// `providers.xai.oauth_credential_generation` pointer whose credential
    /// file no longer exists — the full device flow (discovery, device-code
    /// request, token poll, activation) must succeed and replace the stale
    /// pointer instead of dying with "xAI login was not activated; provider
    /// configuration is unchanged".
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn device_login_end_to_end_recovers_from_dangling_generation_pointer() {
        let _guard = crate::test_support::lock_test_env();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": server.uri(),
                "device_authorization_endpoint": format!("{}/oauth2/device-advertised", server.uri()),
                "token_endpoint": format!("{}/oauth2/token-advertised", server.uri())
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth2/device-advertised"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "device-token",
                "user_code": "CW-TEST",
                "verification_uri": format!("{}/verify", server.uri()),
                "expires_in": 60,
                "interval": 1
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token-advertised"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "e2e-xai-access",
                "refresh_token": "e2e-xai-refresh",
                "expires_in": 3600,
                "token_type": "Bearer"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let dir = TempDir::new().unwrap();
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("owned-home");
        let config_path = dir.path().join("config.toml");
        // The exact dogfood-machine state: valid-looking generation pointer,
        // missing credential file.
        let stale = "xai-auth-39a2f3e766ab47f89490002cd04fe187.json";
        fs::write(
            &config_path,
            format!(
                "[providers.xai]\nauth_mode = \"oauth\"\noauth_credential_generation = \"{stale}\"\n"
            ),
        )
        .unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);

        // The login half runs through the unified flow; only activation is
        // still legacy here (3b-ii unifies it).
        let inputs = crate::oauth::ResolvedOAuthInputs {
            issuer: server.uri(),
            client_id: GROK_OIDC_CLIENT_ID.to_string(),
            scopes: DEFAULT_SCOPES.to_string(),
            open_browser: false,
        };
        let unified = tokio::task::block_in_place(|| {
            crate::oauth::device_code_login_with(
                crate::oauth::OAuthProvider::Xai,
                &inputs,
                &mut std::io::sink(),
            )
        })
        .expect("device login against mock xAI");
        let pending = unified;
        let mut live = Config::default();
        let activation = activate_login(pending, Some(&config_path), Some(&mut live))
            .expect("dangling pointer must not brick activation");

        assert!(activation.auth_path.exists());
        let owned = fs::read_to_string(&activation.auth_path).unwrap();
        assert!(owned.contains("e2e-xai-access"));
        assert!(owned.contains("e2e-xai-refresh"));
        let persisted = fs::read_to_string(&config_path).unwrap();
        assert!(
            !persisted.contains(stale),
            "stale pointer must be replaced: {persisted}"
        );
        assert!(persisted.contains(activation.auth_path.file_name().unwrap().to_str().unwrap()));
        assert!(persisted.contains("auth_mode = \"oauth\""));
        assert!(
            credentials_valid(OAuthProvider::Xai, &live),
            "activated login must be usable"
        );
    }

    #[test]
    fn apply_token_response_sets_expiry_from_expires_in() {
        let mut entry = OwnedAuthEntry {
            access_token: None,
            refresh_token: None,
            expires_at: None,
            id_token: None,
            account_id: None,
            oidc_issuer: None,
            oidc_client_id: None,
            originator: None,
            auth_mode: None,
            extra: BTreeMap::new(),
        };
        let token = OAuthTokenMaterial {
            earliest_refresh_at: None,
            scope: None,
            token_type: None,
            verified_chatgpt: None,
            id_token: None,
            access_token: Some("fresh-access".to_string()),
            refresh_token: Some("fresh-refresh".to_string()),
            expires_in: Some(3600),
            error: None,
            error_description: None,
            interval: None,
        };
        let before = now_unix_secs().expect("clock");

        apply_token_response(
            OAuthProvider::Xai,
            &mut entry,
            XAI_OIDC_ISSUER,
            GROK_OIDC_CLIENT_ID,
            &token,
        )
        .expect("apply token");

        assert_eq!(entry.access_token.as_deref(), Some("fresh-access"));
        assert_eq!(entry.refresh_token.as_deref(), Some("fresh-refresh"));
        let expires_at = entry
            .expires_at
            .as_deref()
            .and_then(parse_rfc3339_secs)
            .expect("expires_at set from expires_in");
        let after = now_unix_secs().expect("clock");
        assert!(
            expires_at >= before + 3600,
            "{expires_at} < {before} + 3600"
        );
        assert!(expires_at <= after + 3600, "{expires_at} > {after} + 3600");
    }

    #[test]
    fn apply_token_response_rejects_missing_access_token() {
        let mut entry = OwnedAuthEntry {
            access_token: None,
            refresh_token: None,
            expires_at: None,
            id_token: None,
            account_id: None,
            oidc_issuer: None,
            oidc_client_id: None,
            originator: None,
            auth_mode: None,
            extra: BTreeMap::new(),
        };
        let token = OAuthTokenMaterial {
            earliest_refresh_at: None,
            scope: None,
            token_type: None,
            verified_chatgpt: None,
            id_token: None,
            access_token: None,
            refresh_token: None,
            expires_in: None,
            error: None,
            error_description: None,
            interval: None,
        };

        let error = apply_token_response(
            OAuthProvider::Xai,
            &mut entry,
            XAI_OIDC_ISSUER,
            GROK_OIDC_CLIENT_ID,
            &token,
        )
        .expect_err("missing access_token must fail");

        assert!(
            error.to_string().contains("missing access_token"),
            "{error}"
        );
    }

    struct MockTokenClient {
        responses: Mutex<Vec<(u16, String)>>,
        posts: Mutex<Vec<MockPost>>,
    }

    impl MockTokenClient {
        fn new(responses: Vec<(u16, String)>) -> Self {
            Self {
                responses: Mutex::new(responses),
                posts: Mutex::new(Vec::new()),
            }
        }
    }

    impl crate::oauth::OAuthFormClient for MockTokenClient {
        fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, String)> {
            self.posts.lock().expect("posts").push((
                url.to_string(),
                form.iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
            ));
            let mut responses = self.responses.lock().expect("responses");
            anyhow::ensure!(
                !responses.is_empty(),
                "mock issuer has no remaining responses"
            );
            Ok(responses.remove(0))
        }
    }

    fn jwt_with_exp(exp: u64) -> String {
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#));
        format!("header.{payload}.sig")
    }

    #[test]
    fn siwc_legacy_credentials_require_reauthorization_and_revoke_retains_host() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("temp home");
        let root = home.path().canonicalize().expect("canonical home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
        let config_path = root.join("config.toml");
        std::fs::write(&config_path, "").expect("empty config");

        let pending = pending_login_with_id_token_for_test(
            OAuthProvider::Chatgpt,
            "access-1",
            "refresh-1",
            Some(&jwt_with_account("acct-7")),
        );
        let activation = activate_login(pending, Some(&config_path), None).expect("activate");
        assert!(activation.auth_path.exists());
        let persisted = std::fs::read_to_string(root.join("config.toml")).expect("config");
        assert!(persisted.contains("chatgpt-auth-"));
        assert!(persisted.contains("auth_mode = \"oauth\""));
        assert!(!persisted.contains("access-1"), "{persisted}");

        let generation = toml::from_str::<toml::Value>(&persisted)
            .unwrap()["providers"]["openai_codex"]["oauth_credential_generation"]
            .as_str()
            .unwrap()
            .to_string();
        let mut config = Config {
            provider: Some(ProviderKind::OpenaiCodex.as_str().to_string()),
            ..Config::default()
        };
        config
            .mark_codewhale_owned_chatgpt_oauth(generation.clone())
            .expect("admitted OAuth fixture");
        assert!(credentials_valid(OAuthProvider::Chatgpt, &config));

        let stale = jwt_with_exp(1_000_000_000);
        let scope = format!("{CHATGPT_OAUTH_ISSUER}::{CHATGPT_OAUTH_CLIENT_ID}");
        let raw = serde_json::json!({
            &scope: {
                "access_token": stale,
                "refresh_token": "refresh-old",
                // Conflicting metadata must not suppress the existing refresh.
                "expires_at": rfc3339_from_now(3600),
                "oidc_issuer": CHATGPT_OAUTH_ISSUER,
                "oidc_client_id": CHATGPT_OAUTH_CLIENT_ID,
                "originator": "codewhale"
            }
        });
        codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
            store.write(
                &generation,
                serde_json::to_vec_pretty(&raw).unwrap().as_slice(),
                true,
            )
        })
        .unwrap();

        let mock = MockTokenClient::new(vec![(
            200,
            serde_json::json!({
                "access_token": "access-2",
                "refresh_token": "refresh-2",
                "expires_in": 3600
            })
            .to_string(),
        )]);
        let error = match get_owned_credentials_with(OAuthProvider::Chatgpt, &config, &mock) {
            Ok(_) => panic!("legacy credentials must not be sent to the official API"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("reauthorization"), "{error}");
        assert!(
            mock.posts.lock().unwrap().is_empty(),
            "legacy tokens must not be refreshed"
        );

        let revoke_mock = MockTokenClient::new(vec![(200, String::new())]);
        codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
            revoke_owned_login_locked_with(
                OAuthProvider::Chatgpt,
                Some(&config_path),
                None,
                store,
                &revoke_mock,
            )
        })
        .expect("revoke");
        let after = std::fs::read_to_string(&config_path).expect("config after revoke");
        assert!(!after.contains("chatgpt-auth-"), "{after}");
        assert!(
            root.join("credentials")
                .join(codewhale_config::CHATGPT_HOST_FILE_NAME)
                .exists(),
            "signout retains the host and registration"
        );
        let posts = revoke_mock.posts.lock().unwrap();
        assert!(
            posts.iter().any(|(url, _)| url.contains("/oauth/revoke")),
            "{posts:?}"
        );
    }

    #[test]
    fn chatgpt_revoke_removes_durable_tokens_when_live_mirror_refuses() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("temp home");
        let root = home.path().canonicalize().expect("canonical home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
        let config_path = root.join("config.toml");
        std::fs::write(&config_path, "").expect("empty config");
        let pending = pending_login_with_id_token_for_test(
            OAuthProvider::Chatgpt,
            "access-1",
            "refresh-1",
            Some(&jwt_with_account("acct-7")),
        );
        let activation = activate_login(pending, Some(&config_path), None).expect("activate");
        assert!(activation.auth_path.exists());
        let mut live = Config::default();
        live.providers
            .get_or_insert_with(Default::default)
            .custom
            .insert(
                ProviderKind::OpenaiCodex.as_str().to_string(),
                crate::config::ProviderConfig {
                    kind: Some("openai-compatible".to_string()),
                    base_url: Some("http://localhost:1234/v1".to_string()),
                    ..Default::default()
                },
            );
        assert!(live.clear_codewhale_owned_chatgpt_oauth().is_err());
        let mock = MockTokenClient::new(vec![(200, String::new())]);
        let error = codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
            revoke_owned_login_locked_with(
                OAuthProvider::Chatgpt,
                Some(&config_path),
                Some(&mut live),
                store,
                &mock,
            )
        })
        .expect_err("live mirror refusal remains visible");
        assert!(error.to_string().contains("Signed out locally"));
        assert!(
            !activation.auth_path.exists(),
            "local tokens cannot survive a refused mirror"
        );
        let after = std::fs::read_to_string(&config_path).expect("config after revoke");
        assert!(
            !after.contains("chatgpt-auth-"),
            "durable generation pointer removed"
        );
        assert_eq!(
            mock.posts.lock().unwrap().len(),
            1,
            "one captured remote revoke"
        );
    }

    #[test]
    fn debug_impls_redact_secrets() {
        let entry = OwnedAuthEntry {
            access_token: Some("secret-access".into()),
            refresh_token: Some("secret-refresh".into()),
            expires_at: None,
            id_token: Some("secret-id".into()),
            account_id: None,
            oidc_issuer: None,
            oidc_client_id: None,
            originator: None,
            auth_mode: None,
            extra: BTreeMap::new(),
        };
        let rendered = format!("{entry:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("secret-access"));
        assert!(!rendered.contains("secret-refresh"));
        assert!(!rendered.contains("secret-id"));

        let activation = OAuthActivation {
            credentials: OwnedOAuthCredentials {
                access_token: "secret-access".into(),
                account_id: None,
                account_label: None,
                refresh_token: Some("secret-refresh".into()),
                expires_at: None,
                issuer: "issuer".into(),
                client_id: "client".into(),
            },
            config_path: PathBuf::from("/tmp/config.toml"),
            auth_path: PathBuf::from("/tmp/auth.json"),
            provider: OAuthProvider::Chatgpt,
            account_label: Some("a@example.com (plus)".into()),
            replaced: None,
        };
        let rendered = format!("{activation:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("secret-access"));
    }
    struct SiwcSigningFixture {
        pair: ring::signature::Ed25519KeyPair,
        keys: JwkSet,
    }

    impl SiwcSigningFixture {
        fn new() -> Self {
            use ring::signature::KeyPair as _;
            let pkcs8 =
                ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
                    .unwrap();
            let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
            let keys = serde_json::from_value(serde_json::json!({"keys": [{
                "kty": "OKP", "crv": "Ed25519", "kid": "siwc-fixture", "alg": "EdDSA", "use": "sig",
                "x": URL_SAFE_NO_PAD.encode(pair.public_key().as_ref())
            }]}))
            .unwrap();
            Self { pair, keys }
        }
        fn token(&self, claims: Value) -> String {
            let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"EdDSA","kid":"siwc-fixture"}"#);
            let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
            let message = format!("{header}.{payload}");
            format!(
                "{message}.{}",
                URL_SAFE_NO_PAD.encode(self.pair.sign(message.as_bytes()).as_ref())
            )
        }
        fn id_claims(&self) -> Value {
            serde_json::json!({"iss": CHATGPT_OAUTH_ISSUER, "aud": "oaiapp_codewhale_test", "sub": "test-sub", "nonce": "pending-nonce", "exp": now_unix_secs().unwrap() + 3600})
        }
        fn access(&self, subject: &str) -> String {
            self.token(serde_json::json!({"iss": CHATGPT_OAUTH_ISSUER, "aud": CHATGPT_OAUTH_RESOURCE, "sub": subject, "client_id": "oaiapp_codewhale_test", "scope": CHATGPT_OAUTH_SCOPE, "exp": now_unix_secs().unwrap() + 3600}))
        }
    }

    #[test]
    fn siwc_signed_identity_requires_signature_issuer_audience_expiry_nonce_and_account() {
        let signer = SiwcSigningFixture::new();
        let verify = |token: &str| {
            verify_chatgpt_id_token(
                token,
                &signer.keys,
                CHATGPT_OAUTH_ISSUER,
                "oaiapp_codewhale_test",
                Some("pending-nonce"),
                Some("test-sub"),
                "urn:uuid:00000000-0000-4000-8000-000000000001",
            )
        };
        let claims = signer.id_claims();
        let signed = signer.token(claims.clone());
        assert_eq!(verify(&signed).unwrap().subject, "test-sub");
        for (field, wrong) in [
            ("iss", Value::from("https://other.invalid")),
            ("aud", Value::from("oaiapp_other")),
            (
                "aud",
                serde_json::json!(["oaiapp_codewhale_test", "another-audience"]),
            ),
            ("exp", Value::from(1)),
            ("nonce", Value::from("another-nonce")),
            ("sub", Value::from("another-subject")),
            ("sub", Value::from("")),
        ] {
            let mut changed = claims.clone();
            changed[field] = wrong;
            assert!(
                verify(&signer.token(changed)).is_err(),
                "accepted invalid {field}"
            );
        }
        for required in ["iss", "aud", "exp", "sub", "nonce"] {
            let mut changed = claims.clone();
            changed.as_object_mut().unwrap().remove(required);
            assert!(
                verify(&signer.token(changed)).is_err(),
                "accepted missing {required}"
            );
        }
        let other_signer = SiwcSigningFixture::new();
        assert!(verify(&other_signer.token(claims.clone())).is_err());
        let parts: Vec<_> = signed.split('.').collect();
        let bad_payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"attacker"}"#);
        assert!(verify(&format!("{}.{}.{}", parts[0], bad_payload, parts[2])).is_err());
        let symmetric_header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","kid":"siwc-fixture"}"#);
        assert!(verify(&format!("{}.{}.{}", symmetric_header, parts[1], parts[2])).is_err());
    }

    #[test]
    fn siwc_callback_and_grant_guards_reject_registration_substitution() {
        assert!(issued_callback_client_id(CHATGPT_OAUTH_CLIENT_ID, None).is_err());
        assert!(
            issued_callback_client_id(CHATGPT_OAUTH_CLIENT_ID, Some(CHATGPT_OAUTH_CLIENT_ID))
                .is_err()
        );
        assert_eq!(
            issued_callback_client_id(CHATGPT_OAUTH_CLIENT_ID, Some("oaiapp_first")).unwrap(),
            "oaiapp_first"
        );
        assert_eq!(
            issued_callback_client_id("oaiapp_first", None).unwrap(),
            "oaiapp_first"
        );
        assert!(issued_callback_client_id("oaiapp_first", Some("oaiapp_other")).is_err());
        assert!(require_chatgpt_scopes(Some("openid resource.invoke")).is_err());
        assert!(require_chatgpt_scopes(Some("chatgpt.tokens.use.direct")).is_err());
        assert!(require_chatgpt_scopes(Some(CHATGPT_OAUTH_SCOPE)).is_ok());
        assert!(parse_callback_query(&CHATGPT_OAUTH_PARAMS, "code=x&state=a&state=b").is_err());
        assert!(
            parse_callback_query(&CHATGPT_OAUTH_PARAMS, "code=x&state=a&error=denied").is_err()
        );
        let error = parse_callback_query(&CHATGPT_OAUTH_PARAMS, "error=access_denied").unwrap();
        assert!(accept_callback("pending", error).is_err());
        let success = parse_callback_query(
            &CHATGPT_OAUTH_PARAMS,
            "code=secret-code&state=a&client_id=oaiapp_first",
        )
        .unwrap();
        assert!(!format!("{success:?}").contains("secret-code"));
    }

    #[test]
    fn siwc_signed_access_requires_resource_account_registration_and_plan_scope() {
        let signer = SiwcSigningFixture::new();
        let registration = verify_chatgpt_id_token(
            &signer.token(signer.id_claims()),
            &signer.keys,
            CHATGPT_OAUTH_ISSUER,
            "oaiapp_codewhale_test",
            Some("pending-nonce"),
            None,
            "host",
        )
        .unwrap();
        assert!(
            verify_chatgpt_access_token(&signer.access("test-sub"), &signer.keys, &registration)
                .is_ok()
        );
        assert!(
            verify_chatgpt_access_token(
                &signer.access("another-subject"),
                &signer.keys,
                &registration
            )
            .is_err()
        );
        for (field, wrong) in [
            ("aud", CHATGPT_OAUTH_ISSUER),
            ("client_id", "oaiapp_other"),
            ("scope", "openid resource.invoke"),
        ] {
            let mut claims = serde_json::json!({"iss": CHATGPT_OAUTH_ISSUER, "aud": CHATGPT_OAUTH_RESOURCE, "sub": "test-sub", "client_id": "oaiapp_codewhale_test", "scope": CHATGPT_OAUTH_SCOPE, "exp": now_unix_secs().unwrap() + 3600});
            claims[field] = Value::from(wrong);
            let token = signer.token(claims);
            assert!(
                verify_chatgpt_access_token(&token, &signer.keys, &registration).is_err(),
                "accepted invalid {field}"
            );
        }
    }

    struct SiwcRefreshClient {
        keys: JwkSet,
        access: String,
        posts: Mutex<Vec<MockPost>>,
    }
    impl OAuthFormClient for SiwcRefreshClient {
        fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, String)> {
            self.posts.lock().unwrap().push((
                url.to_string(),
                form.iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ));
            Ok((200, serde_json::json!({"access_token": self.access, "refresh_token": "rotated-refresh", "expires_in": 3600, "scope": CHATGPT_OAUTH_SCOPE, "token_type": "Bearer"}).to_string()))
        }
        fn chatgpt_jwks(&self, _: &str) -> Result<JwkSet> {
            Ok(self.keys.clone())
        }
    }

    #[test]
    fn siwc_refresh_rotates_atomically_and_rejects_a_different_account() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().unwrap();
        let root = home.path().canonicalize().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
        let signer = SiwcSigningFixture::new();
        let mut config = Config {
            provider: Some("openai-codex".into()),
            ..Default::default()
        };
        install_test_chatgpt_registration(&mut config).unwrap();
        let original = get_owned_credentials_read_only(OAuthProvider::Chatgpt, &config).unwrap();
        let original_scope = crate::client::CodewhaleClient::new(&config)
            .unwrap()
            .chatgpt_reasoning_api;
        let path = configured_owned_auth_file_path(OAuthProvider::Chatgpt, &config)
            .unwrap()
            .unwrap();
        let name = path.file_name().unwrap().to_str().unwrap();
        codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
            let mut file = load_owned_auth_file_from_store(store, name)?.unwrap();
            for entry in file.values_mut() {
                entry.expires_at = Some("2000-01-01T00:00:00Z".to_string());
            }
            write_auth_file_to_store(store, name, &file, true)
        })
        .unwrap();
        let before = std::fs::read(&path).unwrap();
        let wrong = SiwcRefreshClient {
            keys: signer.keys.clone(),
            access: signer.access("another-account"),
            posts: Mutex::new(Vec::new()),
        };
        assert!(get_owned_credentials_with(OAuthProvider::Chatgpt, &config, &wrong).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let right = SiwcRefreshClient {
            keys: signer.keys.clone(),
            access: signer.access("test-sub"),
            posts: Mutex::new(Vec::new()),
        };
        let refreshed =
            get_owned_credentials_with(OAuthProvider::Chatgpt, &config, &right).unwrap();
        assert_ne!(original.access_token, refreshed.access_token);
        assert_eq!(
            original_scope,
            crate::client::CodewhaleClient::new(&config)
                .unwrap()
                .chatgpt_reasoning_api
        );
        assert_eq!(refreshed.refresh_token.as_deref(), Some("rotated-refresh"));
        assert_eq!(
            official_chatgpt_registration(&config).unwrap().subject,
            "test-sub"
        );
        assert!(get_owned_credentials_read_only(OAuthProvider::Chatgpt, &config).is_ok());
        let posts = right.posts.lock().unwrap();
        assert_eq!(
            posts[0].0,
            "https://auth.openai.com/api/accounts/oauth/token"
        );
        assert!(
            posts[0]
                .1
                .contains(&("client_id".to_string(), "oaiapp_codewhale_test".to_string()))
        );
        assert!(
            posts[0]
                .1
                .contains(&("resource".to_string(), CHATGPT_OAUTH_RESOURCE.to_string()))
        );
        assert!(
            posts[0]
                .1
                .contains(&("refresh_token".to_string(), "siwc-test-refresh".to_string()))
        );
    }

    #[test]
    fn siwc_replacing_account_drops_previous_registration_and_token() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().unwrap();
        let root = home.path().canonicalize().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
        let config_path = root.join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mut config = Config::default();
        let first = pending_login_for_test(OAuthProvider::Chatgpt, "first-access", "first-refresh");
        activate_login(first, Some(&config_path), Some(&mut config)).unwrap();
        let mut next =
            pending_login_for_test(OAuthProvider::Chatgpt, "next-access", "next-refresh");
        next.client_id = "oaiapp_next".to_string();
        let registration = next.token.verified_chatgpt.as_mut().unwrap();
        registration.client_id = "oaiapp_next".to_string();
        registration.subject = "next-sub".to_string();
        let activated = activate_login(next, Some(&config_path), Some(&mut config)).unwrap();
        let registration = official_chatgpt_registration(&config).unwrap();
        assert_eq!(registration.subject, "next-sub");
        assert_eq!(registration.client_id, "oaiapp_next");
        let raw = std::fs::read_to_string(activated.auth_path).unwrap();
        assert!(!raw.contains("first-access"));
        assert!(!raw.contains("first-refresh"));
        let mut file: AuthFile = serde_json::from_str(&raw).unwrap();
        assert_eq!(file.len(), 1);
        let (_, mut entry) = select_entry(OAuthProvider::Chatgpt, &mut file).unwrap();
        entry.account_id = Some("unverified-account".to_string());
        assert!(registration_from_entry(&entry).is_err());
        entry.account_id = Some("next-sub".to_string());
        entry.access_token = Some("unverified-replacement".to_string());
        assert!(registration_from_entry(&entry).is_err());
    }

    // === OrcaRouter: API-key + PKCE adapters over one credential seam =======

    fn orca_inputs(auth_base: &str) -> OrcaLoginInputs {
        OrcaLoginInputs {
            auth_base: auth_base.to_string(),
            api_base: "https://api.orcarouter.ai/v1".to_string(),
            app_name: ORCAROUTER_APP_NAME.to_string(),
            open_browser: false,
        }
    }

    const ORCA_FAKE_KEY: &str = "sk-orca-test-key-not-a-real-credential";

    #[test]
    fn orcarouter_api_key_adapter_shapes_and_rejects() {
        let from_key = OrcaCredential::from_api_key(&format!("  {ORCA_FAKE_KEY}  ")).unwrap();
        assert_eq!(
            from_key.expose(),
            ORCA_FAKE_KEY,
            "outer whitespace is trimmed"
        );
        assert_eq!(from_key.source(), OrcaCredentialSource::ApiKey);
        assert_eq!(from_key.granted_scope(), ORCAROUTER_SCOPE);
        assert!(from_key.scope_satisfies_purpose());

        for bad in ["", "   ", "sk-openai-not-orca", "orca-key"] {
            let err = match OrcaCredential::from_api_key(bad) {
                Ok(_) => panic!("non-OrcaRouter key must be refused: {bad:?}"),
                Err(err) => err.to_string(),
            };
            assert!(!err.contains(bad) || bad.trim().is_empty(), "{err}");
        }
    }

    /// Both adapters must produce the same credential type, and nothing about
    /// the adapter may be observable downstream beyond the presentation label.
    #[test]
    fn orcarouter_adapters_produce_the_same_credential_result() {
        let api_key = OrcaCredential::from_api_key(ORCA_FAKE_KEY).unwrap();
        let exchanged = exchange_orcarouter_code(
            &MockFormClient::new(vec![(
                200,
                serde_json::json!({"key": ORCA_FAKE_KEY, "user_id": "u-1", "scope": "api"})
                    .to_string(),
            )]),
            ORCAROUTER_AUTH_BASE,
            "auth-code",
            "verifier-value",
        )
        .unwrap();
        assert_eq!(api_key.expose(), exchanged.expose());
        assert_eq!(api_key.granted_scope(), exchanged.granted_scope());
        assert!(api_key.scope_satisfies_purpose() && exchanged.scope_satisfies_purpose());
        assert_ne!(
            api_key.source(),
            exchanged.source(),
            "the source is presentation only and never changes the key"
        );
        assert_eq!(exchanged.source(), OrcaCredentialSource::Pkce);
        assert_eq!(api_key.source().as_str(), "api_key");
        assert_eq!(exchanged.source().as_str(), "pkce");
    }

    #[test]
    fn orcarouter_authorize_url_is_the_documented_contract() {
        let inputs = orca_inputs("https://www.orcarouter.ai");
        let pkce = PkceChallenge {
            verifier: "verifier".into(),
            challenge: "challenge-abc".into(),
        };
        let url = build_orcarouter_authorize_url(
            &inputs,
            "http://127.0.0.1:41234/callback",
            "state-1",
            &pkce,
        )
        .unwrap();
        assert!(
            url.starts_with("https://www.orcarouter.ai/auth?"),
            "authorize path is fixed at /auth: {url}"
        );
        let parsed = reqwest::Url::parse(&url).unwrap();
        let form: std::collections::BTreeMap<_, _> = parsed.query_pairs().collect();
        assert_eq!(form["callback_url"], "http://127.0.0.1:41234/callback");
        assert_eq!(form["code_challenge"], "challenge-abc");
        assert_eq!(form["code_challenge_method"], "S256");
        assert_eq!(form["state"], "state-1");
        assert_eq!(form["app_name"], ORCAROUTER_APP_NAME);
        assert_eq!(form["scope"], ORCAROUTER_SCOPE);
        assert!(!form.contains_key("client_id"), "no client id in the URL");
        assert!(
            !form.contains_key("client_secret"),
            "PKCE never carries a client secret"
        );
        assert!(!form.contains_key("response_type"));
        assert!(
            !url.contains(&pkce.verifier),
            "the verifier must never reach the browser"
        );
    }

    #[test]
    fn orcarouter_exchange_uses_the_auth_origin_path_and_body() {
        let client = MockFormClient::new(vec![(
            200,
            serde_json::json!({"key": ORCA_FAKE_KEY, "user_id": "u", "scope": "api"}).to_string(),
        )]);
        exchange_orcarouter_code(&client, ORCAROUTER_AUTH_BASE, "code-1", "verifier-1").unwrap();
        let posts = client.posts.lock().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(
            posts[0].0, "https://www.orcarouter.ai/api/v1/auth/keys",
            "the exchange is on the auth origin at /api/v1/auth/keys"
        );
        assert!(
            !posts[0].0.contains("api.orcarouter.ai"),
            "the exchange must never target the inference origin"
        );
        let form: std::collections::BTreeMap<_, _> = posts[0].1.iter().cloned().collect();
        assert_eq!(form["code"], "code-1");
        assert_eq!(form["code_verifier"], "verifier-1");
        assert_eq!(form["code_challenge_method"], "S256");
        assert!(
            !form.contains_key("client_secret"),
            "no client secret is ever sent"
        );
    }

    #[test]
    fn orcarouter_exchange_error_does_not_echo_the_response_body() {
        let client = MockFormClient::new(vec![(
            403,
            serde_json::json!({
                "error": "invalid_grant",
                "error_description": "secret-must-not-leak"
            })
            .to_string(),
        )]);
        let err = match exchange_orcarouter_code(&client, ORCAROUTER_AUTH_BASE, "used", "verifier")
        {
            Ok(_) => panic!("a rejected code must fail"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("invalid_grant"), "{err}");
        assert!(err.contains("codewhale auth orcarouter"), "{err}");
        assert!(!err.contains("secret-must-not-leak"), "{err}");
        assert!(!err.contains(ORCA_FAKE_KEY), "{err}");
    }

    #[test]
    fn orcarouter_exchange_error_field_never_discloses_untrusted_text() {
        let code = "authorization-code-sentinel";
        let verifier = "verifier-sentinel";
        for server_error in [
            ORCA_FAKE_KEY.to_string(),
            format!("invalid_grant\r\n{code} {verifier}\u{1b}[2J"),
        ] {
            let client = MockFormClient::new(vec![(
                403,
                serde_json::json!({"error": server_error, "key": ORCA_FAKE_KEY}).to_string(),
            )]);
            let error = exchange_orcarouter_code(&client, ORCAROUTER_AUTH_BASE, code, verifier)
                .err()
                .expect("a rejected exchange must fail")
                .to_string();
            assert!(!error.contains(ORCA_FAKE_KEY));
            assert!(!error.contains(code));
            assert!(!error.contains(verifier));
            assert!(!error.chars().any(char::is_control));
            assert!(error.contains("HTTP 403"));
            assert!(error.contains("exchange_failed"));
            assert_eq!(client.posts.lock().unwrap().len(), 1);
        }
    }

    /// 400 is the method-mismatch / downgrade defence; it is terminal for the
    /// attempt, never retried, and never prints the code or verifier.
    #[test]
    fn orcarouter_exchange_400_is_terminal_and_leaks_nothing() {
        let client = MockFormClient::new(vec![(
            400,
            serde_json::json!({"error": "invalid_request", "error_description": "downgrade"})
                .to_string(),
        )]);
        let err = match exchange_orcarouter_code(&client, ORCAROUTER_AUTH_BASE, "c", "v") {
            Ok(_) => panic!("400 must fail"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("invalid_request"), "{err}");
        assert!(!err.contains("downgrade"), "{err}");
        assert_eq!(client.posts.lock().unwrap().len(), 1, "no retry");
    }

    /// A granted scope other than the requested one is surfaced, not assumed.
    #[test]
    fn orcarouter_exchange_records_the_granted_scope_not_the_requested_one() {
        let narrowed = exchange_orcarouter_code(
            &MockFormClient::new(vec![(
                200,
                serde_json::json!({"key": ORCA_FAKE_KEY, "scope": "read"}).to_string(),
            )]),
            ORCAROUTER_AUTH_BASE,
            "c",
            "v",
        )
        .unwrap();
        assert_eq!(narrowed.granted_scope(), "read");
        assert!(
            !narrowed.scope_satisfies_purpose(),
            "a downgraded grant must not be treated as satisfying the purpose"
        );

        let silent = exchange_orcarouter_code(
            &MockFormClient::new(vec![(
                200,
                serde_json::json!({"key": ORCA_FAKE_KEY}).to_string(),
            )]),
            ORCAROUTER_AUTH_BASE,
            "c",
            "v",
        )
        .unwrap();
        assert_eq!(silent.granted_scope(), ORCAROUTER_SCOPE);
    }

    #[test]
    fn orcarouter_exchange_rejects_non_orca_and_empty_keys() {
        for body in [
            serde_json::json!({"key": "sk-not-orca", "scope": "api"}),
            serde_json::json!({"key": "   ", "scope": "api"}),
            serde_json::json!({"user_id": "u", "scope": "api"}),
        ] {
            assert!(
                exchange_orcarouter_code(
                    &MockFormClient::new(vec![(200, body.to_string())]),
                    ORCAROUTER_AUTH_BASE,
                    "c",
                    "v",
                )
                .is_err(),
                "a key this client cannot use must be refused"
            );
        }
    }

    /// Flow A state handling: the callback state is compared before the code is
    /// accepted, a denial is terminal, and a reused code is not retried.
    #[test]
    fn orcarouter_callback_state_mismatch_and_denial_are_terminal() {
        let params = &ORCAROUTER_OAUTH_PARAMS;

        let success = parse_callback_query(params, "code=code-1&state=state-1").unwrap();
        assert_eq!(
            accept_callback_with_client("state-1", success).unwrap().0,
            "code-1"
        );

        let mismatched = parse_callback_query(params, "code=code-1&state=other").unwrap();
        let err = accept_callback_with_client("state-1", mismatched)
            .expect_err("state mismatch must be refused")
            .to_string();
        assert!(err.contains("state did not match"), "{err}");

        let denied = parse_callback_query(
            params,
            "error=access_denied&error_description=nope&state=state-1",
        )
        .unwrap();
        let err = accept_callback_with_client("state-1", denied)
            .expect_err("a denial must be terminal")
            .to_string();
        assert!(err.contains("access_denied"), "{err}");
        assert!(
            !err.contains("nope"),
            "the description is not echoed: {err}"
        );

        let duplicated = parse_callback_query(params, "code=a&code=b&state=s");
        assert!(duplicated.is_err(), "a duplicate code is refused");
    }

    #[test]
    fn orcarouter_pkce_pair_is_s256_and_ephemeral() {
        let pkce = generate_pkce();
        assert!(pkce.verifier.len() >= 43);
        assert_eq!(
            pkce.challenge,
            URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier.as_bytes()))
        );
        assert!(!pkce.challenge.contains('='), "challenge is unpadded");
        assert_ne!(generate_state(), generate_state());
    }

    /// Both origins are explicit and independently overridable; a remote plain
    /// HTTP auth origin is refused because PKCE must not run in the clear.
    #[test]
    fn orcarouter_origins_are_separate_and_enforce_https() {
        assert_eq!(ORCAROUTER_AUTH_BASE, "https://www.orcarouter.ai");
        assert_eq!(ORCAROUTER_API_BASE, "https://api.orcarouter.ai/v1");
        assert!(
            !ORCAROUTER_API_BASE.contains("www.orcarouter.ai"),
            "the inference origin is not derived from the auth origin"
        );
        assert_eq!(
            orcarouter_exchange_url("https://www.orcarouter.ai/"),
            "https://www.orcarouter.ai/api/v1/auth/keys"
        );
        let exchange = orcarouter_exchange_url(ORCAROUTER_AUTH_BASE);
        assert_eq!(exchange, "https://www.orcarouter.ai/api/v1/auth/keys");
        assert!(
            !exchange.starts_with("https://api.orcarouter.ai"),
            "the exchange runs on the auth origin, not the inference origin"
        );
        let inputs = orca_inputs("http://evil.example");
        let pkce = PkceChallenge {
            verifier: "v".into(),
            challenge: "c".into(),
        };
        assert!(
            build_orcarouter_authorize_url(&inputs, "http://127.0.0.1:1/callback", "s", &pkce)
                .is_err(),
            "a remote plain-HTTP auth origin must be refused"
        );
    }

    /// OrcaRouter has no owned OAuth generation: PKCE hands back a durable API
    /// key, not refreshable token material. It therefore never appears in
    /// `OAuthProvider` and is driven by its own [`orcarouter_pkce_login`] path.
    #[test]
    fn orcarouter_is_not_an_owned_generation_provider() {
        for provider in [OAuthProvider::Xai, OAuthProvider::Chatgpt] {
            assert!(provider.is_valid_generation(&provider.new_generation()));
        }
        assert_eq!(ORCAROUTER_EXCHANGE_PATH, "/api/v1/auth/keys");
        assert!(
            !ORCAROUTER_EXCHANGE_PATH.starts_with("/v1/auth/keys"),
            "the exchange path is not the relay model route"
        );
    }

    /// The loopback listener binds an ephemeral port on 127.0.0.1 and the
    /// authorize URL's callback matches that exact port.
    #[test]
    fn orcarouter_loopback_request_matches_the_bound_port() {
        let listeners = bind_orcarouter_callback().unwrap();
        let inputs = orca_inputs(ORCAROUTER_AUTH_BASE);
        let request = start_orcarouter_auth_request(&listeners, &inputs).unwrap();
        assert!(request.redirect_uri.starts_with("http://127.0.0.1:"));
        assert!(request.redirect_uri.ends_with("/callback"));
        let port = listeners[0].local_addr().unwrap().port();
        assert!(request.redirect_uri.contains(&port.to_string()));
        assert!(request.authorize_url.contains(&request.state));
        assert!(request.authorize_url.contains(&request.pkce.challenge));
        assert!(!request.authorize_url.contains(&request.pkce.verifier));
        assert!(
            !format!("{request:?}").contains(&request.pkce.verifier),
            "Debug output must not print the verifier"
        );
    }

    #[test]
    fn orcarouter_credential_debug_never_prints_the_key() {
        let credential = OrcaCredential::from_api_key(ORCA_FAKE_KEY).unwrap();
        // OrcaCredential is deliberately Debug-free; prove the key does not
        // leak through the presentation label or the scope accessors.
        assert!(!credential.source().as_str().contains(ORCA_FAKE_KEY));
        assert!(!credential.granted_scope().contains(ORCA_FAKE_KEY));
        assert_eq!(credential.source().as_str(), "api_key");
    }

    /// One local fake auth server that plays both the browser and the
    /// exchange endpoint over real sockets. It answers `GET {auth}/auth` with a
    /// 302 to the `callback_url` the client sent — carrying the client's own
    /// `state`, which is the only way the callback is accepted — and answers
    /// `POST {auth}/api/v1/auth/keys` with the minted key. The PKCE verifier
    /// never reaches this server: only the S256 challenge does.
    struct FakeOrcaAuthServer {
        port: u16,
        requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl FakeOrcaAuthServer {
        fn start() -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = requests.clone();
            std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(25);
                while std::time::Instant::now() < deadline {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let head = read_fixture_request(&mut stream);
                            seen.lock().unwrap().push(head.clone());
                            let request_line = head.lines().next().unwrap_or_default();
                            let mut parts = request_line.split_whitespace();
                            let method = parts.next().unwrap_or("");
                            let target = parts.next().unwrap_or("");
                            let url = reqwest::Url::parse("http://127.0.0.1")
                                .and_then(|base| base.join(target))
                                .ok();
                            let query = |name: &str| -> Option<String> {
                                url.as_ref().and_then(|url| {
                                    url.query_pairs()
                                        .find(|(key, _)| key == name)
                                        .map(|(_, value)| value.into_owned())
                                })
                            };
                            let reply = if method == "GET" {
                                match (query("callback_url"), query("state")) {
                                    (Some(callback), Some(state)) => format!(
                                        "HTTP/1.1 302 Found\r\nLocation: {callback}?code=local-flows-code&state={state}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                                    ),
                                    _ => "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                                        .to_string(),
                                }
                            } else if method == "POST"
                                && url
                                    .as_ref()
                                    .is_some_and(|url| url.path() == ORCAROUTER_EXCHANGE_PATH)
                            {
                                let body = serde_json::json!({
                                    "key": ORCA_FAKE_KEY,
                                    "user_id": "u",
                                    "scope": "api"
                                })
                                .to_string();
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                    body.len()
                                )
                            } else {
                                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                                    .to_string()
                            };
                            let _ = stream.write_all(reply.as_bytes());
                        }
                        Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self { port, requests }
        }

        fn port(&self) -> u16 {
            self.port
        }

        fn urls(&self) -> String {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter_map(|head| head.lines().next().map(str::to_string))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    /// Read one whole request for the fake auth server: the head, then the
    /// `Content-Length` bytes of body. The fixture's listener is non-blocking
    /// and Windows and BSD sockets hand that mode to the accepted stream, so a
    /// single `read` can return `WouldBlock` before the client has written a
    /// byte. Answering that empty read puts a response on the wire ahead of
    /// the request, which the client reports as "received unexpected message
    /// from connection"; closing with the body still unread resets the
    /// connection under the reply.
    fn read_fixture_request(stream: &mut TcpStream) -> String {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut raw = Vec::new();
        let mut chunk = [0u8; 4096];
        while raw.len() < 64 * 1024 {
            if let Some(head_end) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&raw[..head_end]);
                let body_len = header_value(&head, "content-length")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                if raw.len() >= head_end + 4 + body_len {
                    break;
                }
            }
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => raw.extend_from_slice(&chunk[..read]),
            }
        }
        String::from_utf8_lossy(&raw).to_string()
    }

    /// The full connect adapter over a local fake auth server: the adapter
    /// binds its own loopback callback, the fake server redirects the code to
    /// exactly that callback with the adapter's own state, and the exchange
    /// POST returns the key. Nothing here fakes the flow for the adapter — it
    /// runs `orcarouter_pkce_login` end to end over real sockets.
    /// Captures the `Open: <url>` line the login prints, so the test can play
    /// the browser leg. The PKCE verifier is never part of that line.
    struct AuthorizeUrlCapture {
        seen: String,
        sent: std::sync::mpsc::Sender<String>,
        done: bool,
    }

    impl std::io::Write for AuthorizeUrlCapture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.seen.push_str(&String::from_utf8_lossy(buf));
            if !self.done
                && let Some(start) = self.seen.find("http")
            {
                let url: String = self.seen[start..]
                    .chars()
                    .take_while(|c| !c.is_whitespace())
                    .collect();
                if url.contains("/auth?") {
                    self.done = true;
                    let _ = self.sent.send(url);
                }
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// One raw HTTP/1.1 GET over a fresh socket. The fake server speaks plain
    /// loopback HTTP, so the browser leg needs no client library.
    fn raw_get(url: &str) -> String {
        let parsed = reqwest::Url::parse(url).expect("callback url");
        let host = parsed.host_str().expect("host").to_string();
        let port = parsed.port_or_known_default().expect("port");
        let target = match parsed.query() {
            Some(query) => format!("{}?{query}", parsed.path()),
            None => parsed.path().to_string(),
        };
        let mut stream = TcpStream::connect((host.as_str(), port)).expect("connect");
        write!(
            stream,
            "GET {target} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
        )
        .expect("write request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        response
    }

    fn header_value(response: &str, name: &str) -> Option<String> {
        response.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
    }

    /// The fake auth server must answer the request, not the accept: a client
    /// that connects, pauses, and then sends its head and body in separate
    /// segments still gets the exchange reply, and the server saw the body.
    #[test]
    fn fake_orca_auth_server_waits_for_a_late_fragmented_request() {
        let auth = FakeOrcaAuthServer::start();
        let mut stream = TcpStream::connect(("127.0.0.1", auth.port())).expect("connect");
        // Longer than the fixture's accept poll, so it accepts an empty socket.
        std::thread::sleep(Duration::from_millis(100));
        let body = r#"{"code":"late-fragment"}"#;
        write!(
            stream,
            "POST {ORCAROUTER_EXCHANGE_PATH} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .expect("write head");
        std::thread::sleep(Duration::from_millis(100));
        stream.write_all(body.as_bytes()).expect("write body");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        let recorded = auth.requests.lock().unwrap().join("\n");
        assert!(recorded.contains("late-fragment"), "{recorded}");
    }

    #[test]
    fn orcarouter_connect_adapter_runs_authorize_callback_exchange_end_to_end() {
        let auth = FakeOrcaAuthServer::start();
        let auth_base = format!("http://127.0.0.1:{}", auth.port());
        let inputs = OrcaLoginInputs {
            auth_base: auth_base.clone(),
            api_base: ORCAROUTER_API_BASE.to_string(),
            app_name: ORCAROUTER_APP_NAME.to_string(),
            open_browser: false,
        };
        let (url_tx, url_rx) = std::sync::mpsc::channel();
        let login = std::thread::spawn(move || {
            let mut capture = AuthorizeUrlCapture {
                seen: String::new(),
                sent: url_tx,
                done: false,
            };
            orcarouter_pkce_login(&inputs, &mut capture)
        });

        let authorize_url = url_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("the adapter prints the authorize URL before it waits for the callback");
        assert!(
            authorize_url.starts_with(&format!("{auth_base}/auth?")),
            "{authorize_url}"
        );

        // Browser leg 1: the authorize endpoint redirects to the adapter's own
        // loopback callback, carrying the code and the adapter's own state.
        let redirect = raw_get(&authorize_url);
        let callback = header_value(&redirect, "location")
            .expect("the authorize endpoint returns a loopback redirect");
        assert!(callback.starts_with("http://127.0.0.1:"), "{callback}");

        // Browser leg 2: the loopback callback receives it; the login thread
        // then POSTs the code and the S256 verifier to the exchange endpoint.
        let _ = raw_get(&callback);

        let credential = login.join().expect("login worker").expect("login");
        assert_eq!(credential.expose(), ORCA_FAKE_KEY);
        assert_eq!(credential.source(), OrcaCredentialSource::Pkce);
        assert!(credential.scope_satisfies_purpose());

        // The auth server saw the authorize GET and the exchange POST, both on
        // the auth origin, and never the verifier or the minted key.
        let seen = auth.urls();
        let expected_post = format!("POST {ORCAROUTER_EXCHANGE_PATH}");
        assert!(seen.contains("GET /auth?"), "{seen}");
        assert!(seen.contains(&expected_post), "{seen}");
        assert!(!seen.contains("code_verifier"), "{seen}");
        assert!(!seen.contains(ORCA_FAKE_KEY), "{seen}");
    }
}

#[cfg(test)]
mod plugin_oauth_tests {
    use super::*;

    fn descriptor() -> PluginOAuthConfig {
        PluginOAuthConfig {
            issuer: "https://issuer.example".into(),
            authorization_endpoint: "https://issuer.example/authorize".into(),
            token_endpoint: "https://issuer.example/token".into(),
            client_id: "public-client".into(),
            scopes: vec!["models:invoke".into()],
            resource: Some("https://api.example/oauth".into()),
            callback_path: plugin_callback_path(),
        }
    }

    #[test]
    fn plugin_oauth_rejects_insecure_or_cross_origin_endpoints() {
        let mut config = descriptor();
        assert!(config.validate().is_ok());
        config.token_endpoint = "https://attacker.example/token".into();
        assert!(config.validate().is_err());
        config.token_endpoint = "http://issuer.example/token".into();
        assert!(config.validate().is_err());
        config.token_endpoint = "https://issuer.example/token?secret=value".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn plugin_oauth_callback_rejects_duplicate_state_and_issuer_mixup() {
        assert!(
            validate_plugin_callback(
                "code=x&state=s&iss=https%3A%2F%2Fissuer.example",
                "https://issuer.example"
            )
            .is_ok()
        );
        assert!(
            validate_plugin_callback("code=x&state=s&state=other", "https://issuer.example")
                .is_err()
        );
        assert!(
            validate_plugin_callback(
                "code=x&state=s&iss=https%3A%2F%2Fattacker.example",
                "https://issuer.example"
            )
            .is_err()
        );
        assert!(validate_plugin_callback("error=access_denied", "https://issuer.example").is_err());
    }

    #[test]
    fn plugin_callback_preserves_client_identity_and_rejects_issuer_mixup() {
        fn callback(query: &str) -> Result<(String, Option<String>)> {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
            let address = listener.local_addr()?;
            let server = std::thread::spawn(move || {
                let config = descriptor();
                let (stream, _) = listener.accept()?;
                handle_callback_stream_validated(
                    stream,
                    &config.callback_params(),
                    "expected-state",
                    Some(&config.issuer),
                )
            });
            let mut client = TcpStream::connect(address)?;
            write!(
                client,
                "GET /oauth/callback?{query} HTTP/1.1\r\nHost: localhost\r\n\r\n"
            )?;
            server.join().expect("callback worker")
        }
        let (code, client_id) = callback(
            "code=code-sentinel&state=expected-state&client_id=oaiapp_callback_fixture&iss=https%3A%2F%2Fissuer.example",
        ).unwrap();
        assert_eq!(code, "code-sentinel");
        assert_eq!(client_id.as_deref(), Some("oaiapp_callback_fixture"));
        assert!(
            callback("code=code-sentinel&state=expected-state&iss=https%3A%2F%2Fother.example",)
                .is_err()
        );
    }

    #[test]
    fn plugin_oauth_binding_is_exact_and_authorization_uses_pkce_resource() {
        let config = descriptor();
        let slot = plugin_oauth_slot("example", "https://api.example/v1", &config).unwrap();
        assert_ne!(
            slot,
            plugin_oauth_slot("other", "https://api.example/v1", &config).unwrap()
        );
        assert_ne!(
            slot,
            plugin_oauth_slot("example", "https://other.example/v1", &config).unwrap()
        );
        let mut changed = config.clone();
        changed.scopes.push("balance:read".into());
        assert_ne!(
            slot,
            plugin_oauth_slot("example", "https://api.example/v1", &changed).unwrap()
        );
        let pkce = generate_pkce();
        let url = reqwest::Url::parse(
            &config
                .authorize_url("http://127.0.0.1:1234/oauth/callback", "state", &pkce)
                .unwrap(),
        )
        .unwrap();
        let pairs: BTreeMap<_, _> = url.query_pairs().collect();
        assert_eq!(pairs.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(pairs.get("resource").unwrap(), "https://api.example/oauth");
        assert!(!url.as_str().contains(&pkce.verifier));
    }

    #[test]
    fn plugin_oauth_diagnostics_do_not_refresh_or_mutate_expired_credentials() {
        let secrets = codewhale_secrets::Secrets::new(std::sync::Arc::new(
            codewhale_secrets::InMemoryKeyringStore::default(),
        ));
        let config = descriptor();
        let raw = serde_json::to_string(&PluginOAuthTokens {
            access_token: "expired".into(),
            refresh_token: Some("refresh".into()),
            expires_at: 1,
        })
        .unwrap();
        secrets.set("test-slot", &raw).unwrap();
        assert!(plugin_oauth_saved_token(&raw).unwrap());
        let unusable = serde_json::to_string(&PluginOAuthTokens {
            access_token: "expired".into(),
            refresh_token: None,
            expires_at: 1,
        })
        .unwrap();
        assert!(!plugin_oauth_saved_token(&unusable).unwrap());
        let error =
            plugin_oauth_access_token_with_store("test-slot", &config, true, &secrets).unwrap_err();
        assert!(error.to_string().contains("diagnostics never refresh"));
        assert_eq!(
            secrets.get("test-slot").unwrap().as_deref(),
            Some(raw.as_str())
        );
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plugin_oauth_short_lived_nonrefreshable_token_uses_its_actual_lifetime() -> Result<()>
    {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "short-lived",
                "expires_in": 30,
                "token_type": "Bearer"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let mut config = descriptor();
        config.issuer = server.uri();
        config.authorization_endpoint = format!("{}/authorize", server.uri());
        config.token_endpoint = format!("{}/token", server.uri());
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut token = plugin_token_response(
                &config,
                &[("grant_type", "authorization_code"), ("code", "test-code")],
                None,
            )?;
            assert!(token.refresh_token.is_none());
            assert!(
                token.expires_at > now_unix_secs().context("test clock before UNIX epoch")? as u64
            );
            let secrets = codewhale_secrets::Secrets::new(std::sync::Arc::new(
                codewhale_secrets::InMemoryKeyringStore::default(),
            ));
            let raw = serde_json::to_string(&token)?;
            secrets.set("short-lifetime", &raw)?;
            for read_only in [true, false] {
                assert_eq!(
                    plugin_oauth_access_token_with_store(
                        "short-lifetime",
                        &config,
                        read_only,
                        &secrets,
                    )?,
                    "short-lived"
                );
                assert_eq!(
                    secrets.get("short-lifetime")?.as_deref(),
                    Some(raw.as_str())
                );
            }
            // Read-only use also preserves a still-valid refreshable grant.
            token.refresh_token = Some("unused-refresh".into());
            let refreshable = serde_json::to_string(&token)?;
            secrets.set("short-lifetime", &refreshable)?;
            assert_eq!(
                plugin_oauth_access_token_with_store("short-lifetime", &config, true, &secrets)?,
                "short-lived"
            );
            assert_eq!(
                secrets.get("short-lifetime")?.as_deref(),
                Some(refreshable.as_str())
            );
            // Model the actual expiry boundary without a wall-clock sleep.
            token.refresh_token = None;
            token.expires_at = now_unix_secs().context("test clock before UNIX epoch")? as u64;
            let expired = serde_json::to_string(&token)?;
            secrets.set("short-lifetime", &expired)?;
            for read_only in [true, false] {
                let error = plugin_oauth_access_token_with_store(
                    "short-lifetime",
                    &config,
                    read_only,
                    &secrets,
                )
                .expect_err("an actually expired token must be refused");
                assert!(error.to_string().contains("expired"));
                assert_eq!(
                    secrets.get("short-lifetime")?.as_deref(),
                    Some(expired.as_str())
                );
            }
            Ok(())
        })
        .await??;
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        server.verify().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plugin_oauth_refresh_rotation_serializes_concurrent_requests() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=old-refresh"))
            .and(body_string_contains("resource=https%3A%2F%2Fapi.example%2Foauth"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token":"fresh", "refresh_token":"rotated", "expires_in":3600, "token_type":"Bearer"})))
            .expect(1).mount(&server).await;
        let mut config = descriptor();
        config.issuer = server.uri();
        config.authorization_endpoint = format!("{}/authorize", server.uri());
        config.token_endpoint = format!("{}/token", server.uri());
        let secrets = std::sync::Arc::new(codewhale_secrets::Secrets::new(std::sync::Arc::new(
            codewhale_secrets::InMemoryKeyringStore::default(),
        )));
        secrets
            .set(
                "rotation",
                &serde_json::to_string(&PluginOAuthTokens {
                    access_token: "expired".into(),
                    refresh_token: Some("old-refresh".into()),
                    expires_at: 1,
                })
                .unwrap(),
            )
            .unwrap();
        let mut workers = Vec::new();
        for _ in 0..2 {
            let config = config.clone();
            let secrets = secrets.clone();
            workers.push(tokio::task::spawn_blocking(move || {
                plugin_oauth_access_token_with_store("rotation", &config, false, &secrets)
            }));
        }
        for worker in workers {
            assert_eq!(worker.await.unwrap().unwrap(), "fresh");
        }
        let stored: PluginOAuthTokens =
            serde_json::from_str(&secrets.get("rotation").unwrap().unwrap()).unwrap();
        assert_eq!(stored.refresh_token.as_deref(), Some("rotated"));
        secrets.delete("rotation").unwrap();
        assert!(
            plugin_oauth_access_token_with_store("rotation", &config, false, &secrets).is_err()
        );
        server.verify().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plugin_oauth_failed_refresh_preserves_secret_without_exposing_response() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let responses = [
            ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "secret-response-material"
            })),
            ResponseTemplate::new(400)
                .set_body_bytes(b"secret-response-material")
                .insert_header(
                    "content-type",
                    "text/html; credential=secret-header-material",
                ),
        ];
        for response in responses {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/token"))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
            let mut config = descriptor();
            config.issuer = server.uri();
            config.authorization_endpoint = format!("{}/authorize", server.uri());
            config.token_endpoint = format!("{}/token", server.uri());
            let secrets = codewhale_secrets::Secrets::new(std::sync::Arc::new(
                codewhale_secrets::InMemoryKeyringStore::default(),
            ));
            let raw = serde_json::to_string(&PluginOAuthTokens {
                access_token: "still-valid".into(),
                refresh_token: Some("refresh".into()),
                expires_at: (now_unix_secs().unwrap() as u64).saturating_add(30),
            })
            .unwrap();
            secrets.set("denied", &raw).unwrap();
            let (error, unchanged) = tokio::task::spawn_blocking(move || {
                let error =
                    plugin_oauth_access_token_with_store("denied", &config, false, &secrets)
                        .unwrap_err();
                (error.to_string(), secrets.get("denied").unwrap().unwrap())
            })
            .await
            .unwrap();
            assert!(error.contains("HTTP 400"));
            assert!(!error.contains("secret-response-material"));
            assert!(!error.contains("secret-header-material"));
            assert_eq!(unchanged, raw);
            server.verify().await;
        }
    }
    #[test]
    fn plugin_oauth_reviewed_provider_real_client_refresh_chat_catalog_and_revocation() {
        use crate::client::CodewhaleClient;
        use crate::llm_client::LlmClient;
        use crate::plugins::discovery::{DiscoveryConfig, discover_with_config};
        use futures_util::StreamExt;
        use wiremock::matchers::{body_string_contains, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let _env = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir().unwrap();
        let _home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", temp.path().join("owned"));
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let root = temp.path().to_owned();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let server = MockServer::start().await;
            Mock::given(method("POST")).and(path("/token"))
                .and(body_string_contains("refresh_token=old-refresh"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token":"rotated-access", "refresh_token":"rotated-refresh", "expires_in":3600, "token_type":"Bearer"})))
                .expect(1).mount(&server).await;
            Mock::given(method("GET")).and(path("/v1/models"))
                .and(header("authorization", "Bearer rotated-access"))
                .and(header("x-fixture-route", "main"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data":[{"id":"fixture-model", "object":"model"}]})))
                .expect(1).mount(&server).await;
            Mock::given(method("POST")).and(path("/v1/chat/completions"))
                .and(header("authorization", "Bearer rotated-access"))
                .and(header("x-fixture-route", "main"))
                .and(|request: &wiremock::Request| serde_json::from_slice::<serde_json::Value>(&request.body).is_ok_and(|body| body.get("stream").is_none()))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"fixture", "object":"chat.completion", "model":"fixture-model", "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}], "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}})))
                .expect(1).mount(&server).await;
            Mock::given(method("POST")).and(path("/v1/chat/completions"))
                .and(header("authorization", "Bearer rotated-access"))
                .and(header("x-fixture-route", "main"))
                .and(body_string_contains("\"stream\":true"))
                .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string("data: {\"id\":\"fixture\",\"object\":\"chat.completion.chunk\",\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n"))
                .expect(1).mount(&server).await;
            let issuer = server.uri();
            let (live, discovery, slot) = tokio::task::spawn_blocking(move || {
                let workspace = root.join("workspace");
                let plugin = root.join("plugins/provider-fixture");
                std::fs::create_dir_all(&workspace).unwrap();
                std::fs::create_dir_all(&plugin).unwrap();
                let mut config = descriptor();
                config.issuer = issuer.clone();
                config.authorization_endpoint = format!("{issuer}/authorize");
                config.token_endpoint = format!("{issuer}/token");
                let declared_base_url = format!("{issuer}/v1/");
                let manifest = serde_json::json!({
                    "$schema": crate::plugins::agent_plugin::PLUGIN_SCHEMA_URL,
                    "name":"provider-fixture", "version":"1.0.0",
                    "extensions":{"net.codewhale":{"providers":{"fixture-gateway":{
                        "base_url":declared_base_url, "model":"fixture-model", "models":["fixture-model"], "http_headers":{"X-Fixture-Route":"main"}, "oauth":config
                    }}}}
                });
                std::fs::write(plugin.join("plugin.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
                let discovery = DiscoveryConfig { workspace:workspace.clone(), user_plugins_dir:root.join("plugins"), workspace_plugins_dir:workspace.join(".codewhale/plugins"), builtin_plugin_dirs:vec![], state_path:root.join("state/plugins.json") };
                let mut registry = discover_with_config(&discovery);
                registry.trust("provider-fixture").unwrap();
                registry.enable("provider-fixture").unwrap();
                let registry = discover_with_config(&discovery);
                let mut live = Config { provider:Some("fixture-gateway".into()), http_headers:Some(std::collections::HashMap::from([
                    ("Authorization".into(), "Bearer ambient-global".into()),
                    ("X-Api-Key".into(), "ambient-global".into()),
                    ("Cookie".into(), "secret=ambient-global".into()),
                    ("X-Unreviewed".into(), "ambient-global".into()),
                ])), ..Config::default() };
                crate::plugins::providers::apply_providers(&mut live, &registry).unwrap();
                let auth_entry = crate::plugins::providers::plugin_auth_entry(
                    &live,
                    "fixture-gateway",
                )
                .unwrap();
                let base_url = auth_entry.base_url.unwrap();
                assert_eq!(base_url, format!("{issuer}/v1"));
                assert_eq!(base_url, live.base_url_for_route(&live.resolve_provider_pin_identity("fixture-gateway").unwrap()));
                let slot = plugin_oauth_slot("fixture-gateway", &base_url, &config).unwrap();
                let secrets = codewhale_secrets::Secrets::auto_detect();
                let expired = PluginOAuthTokens { access_token:"private-expired-token".into(), refresh_token:Some("old-refresh".into()), expires_at:1 };
                secrets.set(&slot, &serde_json::to_string(&expired).unwrap()).unwrap();
                assert!(crate::config::has_api_key(&live));
                assert!(crate::config::has_api_key_for(&live, &live.active_provider_identity().unwrap()));
                (live, discovery, slot)
            }).await.unwrap();
            // Exercise the real async constructor with an already-expired
            // stored credential: only the actual request worker may refresh.
            let (generic_key, source) = live.active_route_api_key_with_source().unwrap();
            assert!(generic_key.is_empty());
            assert_eq!(source, "host-managed plugin OAuth");
            assert!(live.active_route_api_key_read_only().unwrap().is_empty());
            let diagnostic = live.with_read_only_api_key_for_diagnostic().unwrap();
            assert!(diagnostic.plugin_oauth_read_only);
            assert!(!live.plugin_oauth_read_only);
            let diagnostic_client = CodewhaleClient::new(&diagnostic).unwrap();
            assert!(diagnostic_client.list_models().await.is_err());
            assert!(server.received_requests().await.unwrap().is_empty());
            // Even a valid stored credential for an in-memory endpoint edit
            // cannot reuse the receipt for the originally reviewed declaration.
            let mut altered = live.clone();
            let altered_base = format!("{}/other", server.uri());
            let altered_entry = altered.providers.as_mut().unwrap().custom.get_mut("fixture-gateway").unwrap();
            altered_entry.base_url = Some(altered_base.clone());
            let altered_descriptor = altered_entry.oauth.clone().unwrap();
            tokio::task::spawn_blocking(move || {
                let altered_slot = plugin_oauth_slot("fixture-gateway", &altered_base, &altered_descriptor).unwrap();
                let credential = PluginOAuthTokens { access_token:"review-bypass-token".into(), refresh_token:None, expires_at:(now_unix_secs().unwrap() as u64).saturating_add(3600) };
                codewhale_secrets::Secrets::auto_detect().set(&altered_slot, &serde_json::to_string(&credential).unwrap()).unwrap();
            }).await.unwrap();
            let altered_client = CodewhaleClient::new(&altered).unwrap();
            assert!(altered_client.list_models().await.is_err());
            assert!(server.received_requests().await.unwrap().is_empty());
            let client = CodewhaleClient::new(&live).unwrap();
            assert!(server.received_requests().await.unwrap().is_empty());
            let models = client.list_models().await.unwrap();
            assert!(models.iter().any(|model| model.id == "fixture-model"));
            let input = codewhale_models::MessageRequest {
                model:"fixture-model".into(), messages:vec![codewhale_models::Message { role:codewhale_models::Role::User, content:vec![codewhale_models::ContentBlock::Text { text:"hello".into(), cache_control:None }] }], max_tokens:8,
                system:None, tools:None, tool_choice:None, metadata:None, thinking:None, reasoning_effort:None, stream:Some(false), temperature:None, top_p:None,
            };
            client.create_message(input.clone()).await.unwrap();
            let mut stream_input = input.clone();
            stream_input.stream = Some(true);
            let mut stream = client.create_message_stream(stream_input).await.unwrap();
            let mut count = 0;
            while let Some(event) = stream.next().await { event.unwrap(); count += 1; }
            assert!(count > 0);
            let refresh_started = std::sync::Arc::new(tokio::sync::Notify::new());
            let notify_refresh = std::sync::Arc::clone(&refresh_started);
            Mock::given(method("POST")).and(path("/token"))
                .and(body_string_contains("refresh_token=rotated-refresh"))
                .respond_with(move |_request: &wiremock::Request| {
                    notify_refresh.notify_one();
                    ResponseTemplate::new(200)
                        .set_delay(Duration::from_secs(2))
                        .set_body_json(serde_json::json!({"access_token":"revoked-during-refresh", "refresh_token":"new-refresh", "expires_in":3600, "token_type":"Bearer"}))
                })
                .expect(1).mount(&server).await;
            let seed_slot = slot.clone();
            tokio::task::spawn_blocking(move || {
                let expired = PluginOAuthTokens { access_token:"rotated-access".into(), refresh_token:Some("rotated-refresh".into()), expires_at:1 };
                codewhale_secrets::Secrets::auto_detect().set(&seed_slot, &serde_json::to_string(&expired).unwrap()).unwrap();
            }).await.unwrap();
            let before_refresh = server.received_requests().await.unwrap().len();
            let revoked_config = live.clone();
            let revoke = async move {
                refresh_started.notified().await;
                tokio::task::spawn_blocking(move || {
                let mut registry = discover_with_config(&discovery);
                registry.disable("provider-fixture").unwrap();
                assert!(!crate::config::has_api_key_for(&revoked_config, &revoked_config.active_provider_identity().unwrap()));
                // Private material can remain in secure storage; receipt revocation
                // must still prevent reuse by an already constructed client.
                assert!(codewhale_secrets::Secrets::auto_detect().get(&slot).unwrap().is_some());
                }).await.unwrap();
            };
            let (during_refresh, ()) = tokio::join!(client.list_models(), revoke);
            assert!(during_refresh.is_err());
            // The already-authorized refresh may complete; the model request
            // must not leave the host after receipt revocation during that wait.
            assert_eq!(server.received_requests().await.unwrap().len(), before_refresh + 1);
            let before = server.received_requests().await.unwrap().len();
            assert!(client.list_models().await.is_err());
            // Two failures trigger the actual /models recovery probe. It must
            // obey the same review check and make no extra network request.
            for _ in 0..2 { assert!(client.create_message(input.clone()).await.is_err()); }
            assert_eq!(server.received_requests().await.unwrap().len(), before);
            for request in server.received_requests().await.unwrap() {
                for header in ["x-api-key", "cookie", "x-unreviewed"] {
                    assert!(!request.headers.contains_key(header));
                }
                assert!(request.headers.get("authorization").is_none_or(|value| value != "Bearer ambient-global"));
            }
            server.verify().await;
        });
    }
}
