use super::http_client::McpHttpClient;
use crate::mcp::McpServerConfigExt;
use codewhale_net::network_policy::NetworkPolicyDecider;
use codewhale_runtime::credentials::AuthContext;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use oauth2::TokenResponse;
use reqwest::Url;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use rmcp::transport::AuthorizationManager;
use rmcp::transport::AuthorizationSession;
use rmcp::transport::auth::{
    AuthError, AuthorizationRequest, CredentialStore, InMemoryCredentialStore, InMemoryStateStore,
    OAuthHttpClient, OAuthHttpClientError, OAuthHttpClientFuture, OAuthHttpRedirectPolicy,
    OAuthHttpRequest, OAuthState, OAuthTokenResponse, StateStore, StoredCredentials,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, oneshot};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::McpServerConfig;

const REFRESH_SKEW_MILLIS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAuthStatus {
    Unsupported,
    NotLoggedIn,
    BearerToken,
    OAuth,
}

impl std::fmt::Display for McpAuthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Unsupported => "Unsupported",
            Self::NotLoggedIn => "Not logged in",
            Self::BearerToken => "Bearer token",
            Self::OAuth => "OAuth",
        };
        f.write_str(text)
    }
}

/// Context for a failed token refresh. An auth-required failure already
/// flips the server to `◆ auth required` and offers the login tool; any
/// other failure (a token endpoint answering something the client could
/// not parse, a transport error) names the same remedy in words, because
/// the operator otherwise sees only the provider's parse error (#5926).
/// When the token endpoint did answer, its receipt (status line,
/// content-type, masked excerpt) rides along so a provider outage — an HTML
/// 502 page — reads differently from a parser defect on JSON it should
/// have accepted.
fn refresh_failure_context(
    server_name: &str,
    names_remedy: bool,
    receipt: Option<&TokenEndpointReceipt>,
) -> String {
    if names_remedy {
        let answered =
            receipt.map_or_else(String::new, |receipt| format!(" (it answered {receipt})"));
        format!(
            "refreshing MCP OAuth token for server {server_name}: the token endpoint did not answer the way the client expects{answered}; \
             if this persists, run `codewhale mcp login {server_name}` (or `/mcp login {server_name}`) to re-authorize"
        )
    } else {
        format!("refreshing MCP OAuth token for server {server_name}")
    }
}

/// Longest excerpt of a token-endpoint body a refresh failure keeps.
const TOKEN_RECEIPT_EXCERPT_BYTES: usize = 200;

/// rmcp's own cap on an OAuth response body, mirrored so the recording
/// client refuses the same oversized answers the stock one does.
const MAX_OAUTH_HTTP_RESPONSE_BODY_BYTES: usize = 1024 * 1024;

/// Response fields whose values are credentials. Their values are masked
/// before any body excerpt is kept; the field names themselves are not
/// secrets and stay so the operator can see which fields the answer had.
const OAUTH_SECRET_FIELDS: &[&str] = &[
    "access_token",
    "refresh_token",
    "client_secret",
    "id_token",
    "authorization",
];

/// What the token endpoint actually answered. rmcp collapses an
/// unparseable answer to `Failed to parse server response` and drops the
/// body; this is the receipt it drops, with every credential-shaped value
/// masked and the body cut to its first [`TOKEN_RECEIPT_EXCERPT_BYTES`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenEndpointReceipt {
    status: u16,
    reason: Option<&'static str>,
    content_type: Option<String>,
    excerpt: String,
}

impl TokenEndpointReceipt {
    fn from_response(status: reqwest::StatusCode, headers: &HeaderMap, body: &[u8]) -> Self {
        let content_type = headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        Self {
            status: status.as_u16(),
            reason: status.canonical_reason(),
            content_type,
            excerpt: token_response_excerpt(body),
        }
    }
}

impl std::fmt::Display for TokenEndpointReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}", self.status)?;
        if let Some(reason) = self.reason {
            write!(f, " {reason}")?;
        }
        match self.content_type.as_deref() {
            Some(content_type) => write!(f, " ({content_type})")?,
            None => f.write_str(" (no content-type)")?,
        }
        if self.excerpt.is_empty() {
            f.write_str(" with an empty body")
        } else {
            write!(f, ": {}", self.excerpt)
        }
    }
}

/// Masked, whitespace-collapsed, byte-capped excerpt of a token-endpoint
/// body. Masking runs before the cut so a truncated credential never leaks
/// its prefix.
fn token_response_excerpt(body: &[u8]) -> String {
    let masked = mask_oauth_secrets(&String::from_utf8_lossy(body));
    let collapsed = masked.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() <= TOKEN_RECEIPT_EXCERPT_BYTES {
        return collapsed;
    }
    let mut end = TOKEN_RECEIPT_EXCERPT_BYTES;
    while !collapsed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &collapsed[..end])
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Replace every credential-shaped value in `text` with `***`: JSON
/// members (`"access_token": "…"`), form/query pairs (`refresh_token=…`),
/// and bearer schemes (`Bearer …`). Field names, separators and everything
/// else survive so the shape of the answer stays readable.
pub fn mask_oauth_secrets(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let at_word_start = index == 0 || !is_word_byte(bytes[index - 1]);
        if at_word_start
            && let Some((value_start, value_end)) = secret_value_span(text, &lower, index)
        {
            out.push_str(&text[index..value_start]);
            out.push_str("***");
            index = value_end;
            continue;
        }
        let ch = text[index..]
            .chars()
            .next()
            .expect("index sits on a char boundary");
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

/// The byte span of the secret value that starts at `start`, if a secret
/// field or bearer scheme begins there. Every scan step consumes ASCII
/// bytes only, so both ends land on char boundaries.
fn secret_value_span(text: &str, lower: &str, start: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let skip_spaces = |mut cursor: usize| {
        while bytes
            .get(cursor)
            .is_some_and(|byte| *byte == b' ' || *byte == b'\t')
        {
            cursor += 1;
        }
        cursor
    };
    let unquoted_end = |mut cursor: usize| {
        while bytes.get(cursor).is_some_and(|byte| {
            !matches!(byte, b'&' | b',' | b';' | b'}' | b'"' | b'\'') && !byte.is_ascii_whitespace()
        }) {
            cursor += 1;
        }
        cursor
    };
    if lower[start..].starts_with("bearer ") {
        let value_start = skip_spaces(start + "bearer".len());
        let value_end = unquoted_end(value_start);
        return (value_end > value_start).then_some((value_start, value_end));
    }
    for field in OAUTH_SECRET_FIELDS {
        if !lower[start..].starts_with(field) {
            continue;
        }
        let mut cursor = start + field.len();
        if bytes.get(cursor).is_some_and(|byte| is_word_byte(*byte)) {
            continue;
        }
        if bytes.get(cursor) == Some(&b'"') {
            cursor += 1;
        }
        cursor = skip_spaces(cursor);
        match bytes.get(cursor) {
            Some(b':' | b'=') => cursor += 1,
            _ => continue,
        }
        cursor = skip_spaces(cursor);
        if bytes.get(cursor) == Some(&b'"') {
            let value_start = cursor + 1;
            let mut value_end = value_start;
            while let Some(byte) = bytes.get(value_end) {
                match byte {
                    b'\\' => value_end += 2,
                    b'"' => break,
                    _ => value_end += 1,
                }
            }
            return Some((value_start, value_end.min(text.len())));
        }
        // An unquoted `Authorization: Bearer <token>` carries its scheme in
        // front of the credential; the whole value is the secret.
        let value_start = cursor;
        let mut value_end = unquoted_end(cursor);
        if matches!(lower[value_start..value_end].as_ref(), "bearer" | "basic")
            && bytes.get(value_end) == Some(&b' ')
        {
            value_end = unquoted_end(skip_spaces(value_end));
        }
        return Some((value_start, value_end));
    }
    None
}

/// Shared guarded HTTP client for discovery, login and stored credentials.
/// It honors each OAuth operation's redirect policy, caps response bodies and keeps
/// the receipt of the latest token-endpoint answer (every token request is
/// a POST; discovery is GET) so a failed refresh can say what came back.
pub struct RecordingOAuthHttpClient {
    client: McpHttpClient,
    last_token_response: std::sync::Mutex<Option<TokenEndpointReceipt>>,
}

impl RecordingOAuthHttpClient {
    fn new(client: McpHttpClient) -> Self {
        Self {
            client,
            last_token_response: std::sync::Mutex::new(None),
        }
    }

    fn take_token_endpoint_receipt(&self) -> Option<TokenEndpointReceipt> {
        self.last_token_response
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

impl OAuthHttpClient for RecordingOAuthHttpClient {
    fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        Box::pin(async move {
            let OAuthHttpRequest {
                request,
                timeout,
                redirect_policy,
                ..
            } = request;
            let is_token_request = request.method() == reqwest::Method::POST;
            let mut request = reqwest::Request::try_from(request)
                .map_err(|error| Box::new(error) as OAuthHttpClientError)?;
            if let Some(timeout) = timeout {
                *request.timeout_mut() = Some(timeout);
            }
            let mut response = self
                .client
                .execute(
                    request,
                    matches!(redirect_policy, OAuthHttpRedirectPolicy::Follow),
                )
                .await
                .map_err(|error| -> OAuthHttpClientError { error.into() })?;
            let status = response.status();
            let version = response.version();
            let headers = response.headers().clone();
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| Box::new(error) as OAuthHttpClientError)?
            {
                if chunk.len() > MAX_OAUTH_HTTP_RESPONSE_BODY_BYTES - body.len() {
                    return Err(anyhow!(
                        "OAuth HTTP response body exceeds {MAX_OAUTH_HTTP_RESPONSE_BODY_BYTES} bytes"
                    )
                    .into());
                }
                body.extend_from_slice(&chunk);
            }
            if is_token_request {
                *self
                    .last_token_response
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(TokenEndpointReceipt::from_response(status, &headers, &body));
            }
            let mut builder = oauth2::http::Response::builder()
                .status(status)
                .version(version);
            for (name, value) in &headers {
                builder = builder.header(name, value);
            }
            builder
                .body(body)
                .map_err(|error| Box::new(error) as OAuthHttpClientError)
        })
    }
}

pub fn error_looks_auth_required(error: &anyhow::Error) -> bool {
    error_text_looks_auth_required(&format!("{error:#}"))
}

/// Whether the error chain carries the OAuth `invalid_grant` code: the
/// authorization server definitively rejected the presented grant (typically
/// a stale or already-rotated refresh token).
fn error_is_invalid_grant(error: &anyhow::Error) -> bool {
    format!("{error:#}")
        .to_ascii_lowercase()
        .contains("invalid_grant")
}

/// The one auth-required classifier every surface consults: the pool's
/// `◆ auth required` state, the session-boot row, the `/mcp` manager
/// recovery verb, and the synthetic `mcp_<server>_authenticate` tool all
/// derive from this predicate so a failure is never "needs login" on one
/// surface and "failed" on another. `invalid_grant` belongs here because the
/// authorization server has definitively rejected the stored grant — only a
/// fresh login recovers it.
pub fn error_text_looks_auth_required(text: &str) -> bool {
    let status_401 = text_names_http_status(text, "401");
    let text = text.to_ascii_lowercase();
    // `auth required` and `requires oauth` are anchored to the shapes this
    // product and the Codex-compatible managers actually emit (`◆ auth
    // required`, `requires OAuth login/authentication/reauthentication`) —
    // bare substrings would misclassify incidental server errors like
    // "auth required parameter is missing".
    status_401
        || text.contains("unauthorized")
        || text.contains("authentication_required")
        || text.contains("invalid_grant")
        // rmcp 3.2 collapsed several unrecoverable refresh outcomes onto
        // `AuthError::AuthorizationRequired` (Display: "OAuth authorization
        // required") — a stored credential with no usable refresh grant, and
        // every refresh the server definitively rejected. In 2.2 those
        // arrived as `TokenRefreshFailed("No refresh token available")`, which
        // no surface recognised. The full phrase is matched so it stays
        // anchored to rmcp's own wording.
        || text.contains("oauth authorization required")
        || text.contains("◆ auth required")
        || text.contains("requires oauth login")
        || text.contains("requires oauth authentication")
        || text.contains("requires oauth reauthentication")
        || text.contains("not logged in")
        || text.contains("not-logged-in")
        || text.contains("re-authorize")
        || text.contains("/mcp login")
        || text.contains("mcp login")
}

/// Whether error text names an HTTP status as a status, not as digits inside
/// an address or a larger number. Transport errors carry the URL they failed
/// on, so a bare substring match read the reset on
/// `http://127.0.0.1:50401/mcp` as a 401 and put a healthy server into
/// `◆ auth required` — whenever the ephemeral port happened to contain it.
pub fn text_names_http_status(text: &str, status: &str) -> bool {
    text.split_whitespace()
        .filter(|token| !token.contains("://"))
        .any(|token| {
            token
                .split(|c: char| !c.is_ascii_digit())
                .any(|digits| digits == status)
        })
}

pub fn auth_required_login_hint(server_name: &str) -> String {
    format!(
        "MCP server '{server_name}' requires OAuth authentication. Run `codewhale mcp login {server_name}` to authenticate."
    )
}

/// The one recovery sentence for a server in the `◆ auth required` state,
/// chosen by how that server is allowed to authenticate. OAuth-servable
/// servers get the login command; plugin-contributed servers (OAuth is
/// disabled for them by review policy) and servers with a manual
/// Authorization configuration are told which environment-backed
/// credential to supply instead, so `/mcp login` is never named for a
/// server it would refuse. Environment variable *names* are not secrets;
/// their values never appear here.
pub fn auth_required_recovery_hint(server_name: &str, server: &McpServerConfig) -> String {
    let mut env_vars: Vec<&str> = server
        .env_headers
        .values()
        .map(String::as_str)
        .chain(server.bearer_token_env_var.as_deref())
        .collect();
    env_vars.sort_unstable();
    env_vars.dedup();
    let credential_source = if env_vars.is_empty() {
        "its configured Authorization header".to_string()
    } else {
        format!(
            "the environment variable{} {}",
            if env_vars.len() == 1 { "" } else { "s" },
            env_vars.join(", ")
        )
    };
    if let Some(source) = server.reviewed_plugin_source() {
        return format!(
            "MCP server '{server_name}' is contributed by plugin '{}' and its credential comes from {credential_source} (OAuth login is disabled for plugin-contributed servers). Set the credential, then run `/mcp reload`.",
            source.authority.plugin_name
        );
    }
    if server_has_manual_authorization(server) {
        return format!(
            "MCP server '{server_name}' authenticates with {credential_source}; the server rejected that credential. Correct it, then run `/mcp reload`."
        );
    }
    auth_required_login_hint(server_name)
}

/// TUI recovery for a stale Streamable HTTP OAuth session. `/mcp auth` is not a
/// command; login is `/mcp login <name>` (CLI: `codewhale mcp login <name>`).
pub fn tui_reauth_hint() -> &'static str {
    "Re-authorize this server (/mcp login <name>) to continue."
}

pub fn tui_reauth_refresh_failed_hint() -> &'static str {
    "Re-authorize this server (/mcp login <name>) or configure a fresh bearer token."
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpOAuthBinding {
    pub issuer: String,
    pub resource: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMcpOAuthTokens {
    pub server_name: String,
    pub url: String,
    pub client_id: String,
    #[serde(default)]
    pub binding: Option<McpOAuthBinding>,
    pub token_response: WrappedOAuthTokenResponse,
    #[serde(default)]
    pub expires_at: Option<u64>,
}

impl PartialEq for StoredMcpOAuthTokens {
    fn eq(&self, other: &Self) -> bool {
        if self.server_name != other.server_name
            || self.url != other.url
            || self.client_id != other.client_id
            || self.binding != other.binding
            || self.expires_at != other.expires_at
        {
            return false;
        }
        if self.expires_at.is_none() {
            return self.token_response == other.token_response;
        }
        // Loading a credential derives a decreasing expires_in from the
        // durable expires_at. That countdown is not a peer token rotation:
        // comparing it would adopt the same rejected grant after one second
        // instead of invalidating it. Preserve every other response field.
        let mut left = self.token_response.clone();
        let mut right = other.token_response.clone();
        left.0.set_expires_in(None);
        right.0.set_expires_in(None);
        left == right
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedOAuthTokenResponse(pub OAuthTokenResponse);

impl PartialEq for WrappedOAuthTokenResponse {
    fn eq(&self, other: &Self) -> bool {
        match (serde_json::to_string(self), serde_json::to_string(other)) {
            (Ok(left), Ok(right)) => left == right,
            _ => false,
        }
    }
}

#[derive(Clone)]
pub struct McpOAuthRuntime {
    inner: Arc<McpOAuthRuntimeInner>,
}

struct McpOAuthRuntimeInner {
    server_name: String,
    url: String,
    binding: McpOAuthBinding,
    configured_client_id: Option<String>,
    manager: Arc<Mutex<AuthorizationManager>>,
    last_tokens: Mutex<Option<StoredMcpOAuthTokens>>,
    /// Why the held credential was invalidated (the provider's error code,
    /// e.g. `invalid_grant`, or `logged_out` when a refresh found the stored
    /// entry removed), so every later failure names the cause even though
    /// the rejected grant is never replayed. `None` while a credential is
    /// held.
    rejection: Mutex<Option<String>>,
    /// The HTTP client the runtime was built with, shared with the manager
    /// so an adopted on-disk rotation rebuilds it with identical HTTP shape
    /// and a failed refresh can read the token endpoint's receipt.
    http_client: Arc<RecordingOAuthHttpClient>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpOAuthDiscovery {
    pub scopes_supported: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMcpOAuthScopes {
    pub scopes: Vec<String>,
    pub source: McpOAuthScopesSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpOAuthScopesSource {
    Explicit,
    Configured,
    Discovered,
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthProviderError {
    error: Option<String>,
    error_description: Option<String>,
}

impl OAuthProviderError {
    fn new(error: Option<String>, error_description: Option<String>) -> Self {
        Self {
            error,
            error_description,
        }
    }
}

impl std::fmt::Display for OAuthProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.error.as_deref(), self.error_description.as_deref()) {
            (Some(error), Some(description)) => {
                write!(f, "OAuth provider returned `{error}`: {description}")
            }
            (Some(error), None) => write!(f, "OAuth provider returned `{error}`"),
            (None, Some(description)) => write!(f, "OAuth error: {description}"),
            (None, None) => write!(f, "OAuth provider returned an error"),
        }
    }
}

impl std::error::Error for OAuthProviderError {}

/// Build an `AuthorizationManager` preloaded with stored credentials, the
/// shared construction step for initial load and for adopting a credential
/// that another process rotated on disk.
async fn manager_from_stored_tokens(
    url: &str,
    tokens: &StoredMcpOAuthTokens,
    http_client: &Arc<RecordingOAuthHttpClient>,
) -> Result<AuthorizationManager> {
    let binding = tokens.binding.as_ref().ok_or_else(|| anyhow!(
        "Stored MCP credential requires OAuth reauthentication: issuer/resource binding is absent; run /mcp login. The old credential was preserved."
    ))?;
    anyhow::ensure!(
        tokens.url == url,
        "Stored MCP credential requires OAuth reauthentication: server URL changed"
    );
    let (mut manager, discovered) = prepare_authorization_manager(url, http_client).await?;
    anyhow::ensure!(
        *binding == discovered,
        "Stored MCP credential requires OAuth reauthentication: authorization issuer or resource changed. The old credential was preserved."
    );
    let store = InMemoryCredentialStore::new();
    store
        .save(
            StoredCredentials::new(
                tokens.client_id.clone(),
                Some(tokens.token_response.0.clone()),
                tokens
                    .token_response
                    .0
                    .scopes()
                    .map(|scopes| {
                        scopes
                            .iter()
                            .map(|scope| scope.as_str().to_owned())
                            .collect()
                    })
                    .unwrap_or_default(),
                Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                ),
            )
            .with_issuer(Some(binding.issuer.clone())),
        )
        .await?;
    manager.set_credential_store(store);
    anyhow::ensure!(
        manager.initialize_from_store().await?,
        "Stored MCP credential requires OAuth reauthentication"
    );
    Ok(manager)
}

async fn prepare_authorization_manager(
    url: &str,
    http_client: &Arc<RecordingOAuthHttpClient>,
) -> Result<(AuthorizationManager, McpOAuthBinding)> {
    let client = Arc::clone(http_client) as Arc<dyn OAuthHttpClient>;
    let mut manager = AuthorizationManager::new_with_oauth_http_client(url, client).await?;
    let resolution = manager.resolve_metadata().await?;
    anyhow::ensure!(
        resolution.source.is_discovered(),
        "MCP OAuth requires published authorization server metadata"
    );
    let issuer = resolution
        .metadata
        .issuer
        .clone()
        .filter(|issuer| !issuer.is_empty())
        .ok_or_else(|| anyhow!("MCP OAuth metadata omitted its validated issuer"))?;
    manager.set_metadata(resolution.metadata);
    manager.configure_client_id("codewhale-binding-preparation")?;
    let probe_url = manager.get_authorization_url(&[]).await?;
    let resource = authorization_request_parameter(&probe_url, "resource")?;
    manager.set_state_store(InMemoryStateStore::new());
    Ok((manager, McpOAuthBinding { issuer, resource }))
}

fn authorization_request_parameter(url: &str, key: &str) -> Result<String> {
    let parsed = Url::parse(url).context("invalid SDK OAuth authorization URL")?;
    let mut values = parsed.query_pairs().filter(|(name, _)| name == key);
    let value = values
        .next()
        .map(|(_, value)| value.into_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("SDK OAuth authorization URL omitted {key}"))?;
    anyhow::ensure!(
        values.next().is_none(),
        "SDK OAuth authorization URL duplicated {key}"
    );
    Ok(value)
}

impl McpOAuthRuntime {
    #[cfg(test)]
    pub(super) async fn from_server_config(
        server_name: &str,
        server: &McpServerConfig,
        default_headers: HeaderMap,
    ) -> Result<Option<Self>> {
        if server.reviewed_plugin.is_some() || server_has_manual_authorization(server) {
            return Ok(None);
        }
        let Some(url) = server.url.as_deref() else {
            return Ok(None);
        };
        let client = oauth_http_client(server, url, None)?;
        Self::from_server_config_with_client(server_name, server, default_headers, client).await
    }

    pub(super) async fn from_server_config_with_client(
        server_name: &str,
        server: &McpServerConfig,
        default_headers: HeaderMap,
        client: McpHttpClient,
    ) -> Result<Option<Self>> {
        if server.reviewed_plugin.is_some() {
            return Ok(None);
        }
        let Some(url) = server.url.as_deref() else {
            return Ok(None);
        };
        if server_has_manual_authorization(server) {
            return Ok(None);
        }
        let Some(tokens) = load_oauth_tokens(server_name, url)? else {
            return Ok(None);
        };
        anyhow::ensure!(
            tokens.server_name == server_name,
            "Stored MCP credential requires OAuth reauthentication: server identity changed"
        );
        if let Some(binding) = tokens.binding.as_ref() {
            if let Some(issuer) = server
                .oauth
                .as_ref()
                .and_then(|oauth| oauth.issuer.as_deref())
            {
                anyhow::ensure!(
                    issuer == binding.issuer,
                    "Configured MCP OAuth issuer changed; requires OAuth reauthentication"
                );
            }
            if let Some(resource) = server.oauth_resource.as_deref() {
                anyhow::ensure!(
                    resource == binding.resource,
                    "Configured MCP OAuth resource changed; requires OAuth reauthentication"
                );
            }
        }
        if let Some(client_id) = server.oauth_client_id() {
            anyhow::ensure!(
                client_id == tokens.client_id,
                "Configured MCP OAuth client changed; requires OAuth reauthentication"
            );
        }
        Self::from_stored_tokens(
            server_name,
            url,
            tokens,
            server.oauth_client_id(),
            client.with_default_headers(default_headers),
        )
        .await
        .map(Some)
    }

    async fn from_stored_tokens(
        server_name: &str,
        url: &str,
        mut tokens: StoredMcpOAuthTokens,
        configured_client_id: Option<&str>,
        client: McpHttpClient,
    ) -> Result<Self> {
        refresh_expires_in_from_timestamp(&mut tokens);
        let client = client.with_credential_transport()?;
        let http_client = Arc::new(RecordingOAuthHttpClient::new(client));
        let manager = manager_from_stored_tokens(url, &tokens, &http_client).await?;
        let binding = tokens
            .binding
            .clone()
            .ok_or_else(|| anyhow!("MCP OAuth credential binding is absent"))?;

        Ok(Self {
            inner: Arc::new(McpOAuthRuntimeInner {
                server_name: server_name.to_string(),
                url: url.to_string(),
                binding,
                configured_client_id: configured_client_id.map(str::to_owned),
                manager: Arc::new(Mutex::new(manager)),
                last_tokens: Mutex::new(Some(tokens)),
                rejection: Mutex::new(None),
                http_client,
            }),
        })
    }

    pub async fn authorization_header(&self) -> Result<Option<String>> {
        self.refresh_if_needed().await?;
        // Never send a credential the provider already rejected; the request
        // goes out unauthenticated and the server's 401 drives the reactive
        // refresh, which adopts a peer's login or reports auth-required.
        if self.is_invalidated().await {
            return Ok(None);
        }
        let credentials = {
            let guard = self.inner.manager.lock().await;
            let (_client_id, credentials) = guard
                .get_credentials()
                .await
                .context("reading MCP OAuth credentials")?;
            credentials
        };
        let Some(credentials) = credentials else {
            return Ok(None);
        };
        let token = credentials.access_token().secret().trim();
        if token.is_empty() {
            Ok(None)
        } else {
            Ok(Some(format!("Bearer {token}")))
        }
    }

    async fn refresh_if_needed(&self) -> Result<()> {
        let expires_at = {
            let guard = self.inner.last_tokens.lock().await;
            guard.as_ref().and_then(|tokens| tokens.expires_at)
        };
        if !token_needs_refresh(expires_at) {
            return Ok(());
        }
        self.refresh_and_persist().await
    }

    /// Force a token refresh regardless of the local expiry clock (T4): a
    /// 401/403 means the server no longer accepts the token — clock skew,
    /// server-side revocation, or rotation — so the expiry-based gate must
    /// not decide alone.
    pub async fn force_refresh(&self) -> Result<()> {
        self.refresh_and_persist().await
    }

    /// Whether this runtime's credential was invalidated: definitively
    /// rejected by the provider, or removed from the store (a logout) before
    /// a refresh could persist. `last_tokens` is `Some` from construction and
    /// after every persisted refresh; [`Self::clear_stored_tokens`] and a
    /// refresh that returns no credential or finds the stored entry removed
    /// empty it.
    async fn is_invalidated(&self) -> bool {
        self.inner.last_tokens.lock().await.is_none()
    }

    async fn refresh_and_persist(&self) -> Result<()> {
        // A credential the provider definitively rejected is never replayed:
        // the `AuthorizationManager` still holds it, but every later refresh
        // with that grant is a guaranteed `invalid_grant`. The only way back
        // is a credential another process stored since (a completed login),
        // so adopt that when present and otherwise report auth-required
        // without touching the token endpoint.
        if self.is_invalidated().await {
            if !self.adopt_rotated_on_disk_tokens().await? {
                let reason = self
                    .inner
                    .rejection
                    .lock()
                    .await
                    .clone()
                    .unwrap_or_else(|| "unauthorized".to_string());
                if reason == "logged_out" {
                    bail!(
                        "stored MCP OAuth credential for server {} was removed by a logout; the server requires OAuth login again",
                        self.inner.server_name
                    );
                }
                bail!(
                    "stored MCP OAuth credential for server {} was rejected by the provider ({reason}) and removed; the server requires OAuth login again",
                    self.inner.server_name
                );
            }
            let adopted_needs_refresh = {
                let last = self.inner.last_tokens.lock().await;
                token_needs_refresh(last.as_ref().and_then(|tokens| tokens.expires_at))
            };
            if !adopted_needs_refresh {
                return Ok(());
            }
        }
        // Only this refresh's answer may explain this refresh's failure.
        self.inner.http_client.take_token_endpoint_receipt();
        let mut err = match self.try_refresh_and_persist().await {
            Ok(()) => return Ok(()),
            Err(err) => err,
        };
        // Refresh-race tolerance: another codewhale process sharing this token
        // store (a concurrent `mcp login`, or a peer session's refresh) may
        // have rotated the credential after this runtime loaded its copy, and
        // single-use refresh tokens then fail here with `invalid_grant`.
        // Re-read the store once; when the on-disk credential changed, adopt
        // it — using it directly while fresh, or retrying the refresh exactly
        // once with the rotated grant — before surfacing failure. When the
        // store is unchanged the grant is simply dead: the auth-required
        // branch below invalidates it so the server flips to `◆ auth
        // required` and the self-serve login tool appears, instead of every
        // later connect replaying the same rejected refresh.
        if error_is_invalid_grant(&err) && self.adopt_rotated_on_disk_tokens().await? {
            let adopted_needs_refresh = {
                let last = self.inner.last_tokens.lock().await;
                token_needs_refresh(last.as_ref().and_then(|tokens| tokens.expires_at))
            };
            if !adopted_needs_refresh {
                return Ok(());
            }
            match self.try_refresh_and_persist().await {
                Ok(()) => return Ok(()),
                Err(retry_err) => err = retry_err,
            }
        }
        if error_looks_auth_required(&err) {
            let reason = if error_is_invalid_grant(&err) {
                "invalid_grant"
            } else {
                "unauthorized"
            };
            self.clear_stored_tokens(reason).await?;
        }
        let server_name = self.inner.server_name.clone();
        let names_remedy = !error_looks_auth_required(&err);
        let receipt = self.inner.http_client.take_token_endpoint_receipt();
        Err(err)
            .with_context(|| refresh_failure_context(&server_name, names_remedy, receipt.as_ref()))
    }

    async fn try_refresh_and_persist(&self) -> Result<()> {
        let refresh_result = {
            let guard = self.inner.manager.lock().await;
            guard.refresh_token().await
        };
        refresh_result.map_err(|err| anyhow!(err))?;
        self.persist_if_needed().await
    }

    /// Re-read the on-disk credential after an `invalid_grant` refresh
    /// failure and, when another process rotated it, rebuild the manager
    /// around the rotated token exactly like initial construction. Returns
    /// `true` only when the stored credential actually changed; an unchanged
    /// store means the failure is ours to report.
    async fn adopt_rotated_on_disk_tokens(&self) -> Result<bool> {
        let Some(stored) = load_oauth_tokens(&self.inner.server_name, &self.inner.url)? else {
            return Ok(false);
        };
        let changed = {
            let last = self.inner.last_tokens.lock().await;
            last.as_ref() != Some(&stored)
        };
        if !changed {
            return Ok(false);
        }
        self.adopt_stored_tokens(stored).await?;
        Ok(true)
    }

    async fn adopt_stored_tokens(&self, stored: StoredMcpOAuthTokens) -> Result<()> {
        anyhow::ensure!(
            stored.server_name == self.inner.server_name
                && stored.binding.as_ref() == Some(&self.inner.binding),
            "Stored MCP credential requires OAuth reauthentication: server, issuer or resource changed"
        );
        if let Some(client_id) = self.inner.configured_client_id.as_deref() {
            anyhow::ensure!(
                stored.client_id == client_id,
                "Stored MCP credential requires OAuth reauthentication: configured client changed"
            );
        }
        let manager =
            manager_from_stored_tokens(&self.inner.url, &stored, &self.inner.http_client).await?;
        *self.inner.manager.lock().await = manager;
        *self.inner.last_tokens.lock().await = Some(stored);
        *self.inner.rejection.lock().await = None;
        Ok(())
    }

    /// Invalidate the credential this runtime holds after the provider
    /// definitively rejected it. Never deletes a newer durable winner: when
    /// the on-disk credential no longer matches the one we hold, another
    /// process rotated it after our copy loaded, and that credential — not
    /// ours — is the one the next connect must try. The manager is rebuilt
    /// around that winner exactly like initial construction; remembering
    /// the rotated token while keeping our dead grant would make the next
    /// `invalid_grant` compare an "unchanged" store and delete the newer
    /// valid credential.
    ///
    /// The comparison and the delete are one step under the secret store's
    /// entry lock (the same lock every save takes), so a login that lands
    /// between them is never the credential that gets deleted.
    async fn clear_stored_tokens(&self, reason: &str) -> Result<()> {
        let held = { self.inner.last_tokens.lock().await.take() };
        let Some(held) = held else {
            return Ok(());
        };
        match delete_oauth_tokens_if_held(&self.inner.server_name, &self.inner.url, &held)? {
            Some(stored) => {
                tracing::debug!(
                    target: "mcp",
                    server = %self.inner.server_name,
                    "MCP OAuth credential was rotated by another process; keeping the on-disk winner"
                );
                self.adopt_stored_tokens(stored).await?;
            }
            None => {
                *self.inner.rejection.lock().await = Some(reason.to_string());
            }
        }
        Ok(())
    }

    async fn persist_if_needed(&self) -> Result<()> {
        let (client_id, credentials) = {
            let guard = self.inner.manager.lock().await;
            guard
                .get_credentials()
                .await
                .context("reading refreshed MCP OAuth credentials")?
        };
        let Some(credentials) = credentials else {
            let mut last = self.inner.last_tokens.lock().await;
            if let Some(previous) = last.take() {
                // Only our own credential goes; a newer login stays.
                delete_oauth_tokens_if_held(&self.inner.server_name, &self.inner.url, &previous)?;
            }
            return Ok(());
        };

        let new_response = WrappedOAuthTokenResponse(credentials.clone());
        let mut last = self.inner.last_tokens.lock().await;
        let same_token = last
            .as_ref()
            .map(|previous| previous.token_response == new_response)
            .unwrap_or(false);
        let expires_at = if same_token {
            last.as_ref().and_then(|previous| previous.expires_at)
        } else {
            compute_expires_at_millis(&credentials)
        };
        let stored = StoredMcpOAuthTokens {
            server_name: self.inner.server_name.clone(),
            url: self.inner.url.clone(),
            client_id,
            binding: Some(self.inner.binding.clone()),
            token_response: new_response,
            expires_at,
        };
        let Some(held) = last.as_ref().filter(|held| **held != stored) else {
            return Ok(());
        };
        match save_oauth_tokens_if_held(held, &stored)? {
            HeldTokenSave::Saved => *last = Some(stored),
            HeldTokenSave::Removed => {
                *last = None;
                drop(last);
                *self.inner.rejection.lock().await = Some("logged_out".to_string());
            }
            HeldTokenSave::Replaced(current) => {
                drop(last);
                self.adopt_stored_tokens(*current).await?;
            }
        }
        Ok(())
    }
}

pub async fn auth_status_for_server(
    name: &str,
    server: &McpServerConfig,
    network_policy: Option<&NetworkPolicyDecider>,
) -> McpAuthStatus {
    auth_status_for_server_with_auth_context(
        name,
        server,
        network_policy,
        super::process_mcp_auth_context(),
    )
    .await
}

pub async fn auth_status_for_server_with_auth_context(
    name: &str,
    server: &McpServerConfig,
    network_policy: Option<&NetworkPolicyDecider>,
    auth_context: Arc<dyn AuthContext>,
) -> McpAuthStatus {
    if server.reviewed_plugin.is_some() || !server.is_enabled() || server.url.is_none() {
        return McpAuthStatus::Unsupported;
    }
    if server_has_manual_authorization(server) {
        return McpAuthStatus::BearerToken;
    }
    let Some(url) = server.url.as_deref() else {
        return McpAuthStatus::Unsupported;
    };
    match load_oauth_tokens(name, url) {
        Ok(Some(tokens)) if oauth_tokens_are_usable(&tokens) => return McpAuthStatus::OAuth,
        Ok(Some(_)) => return McpAuthStatus::NotLoggedIn,
        Ok(None) => {}
        Err(err) => {
            tracing::warn!(target: "mcp", server = %name, error = %err, "failed to read MCP OAuth tokens");
        }
    }

    let headers = match build_default_headers_with_auth_context(server, auth_context.as_ref()) {
        Ok(headers) => headers,
        Err(err) => {
            tracing::warn!(target: "mcp", server = %name, error = %err, "failed to build MCP OAuth discovery headers");
            return McpAuthStatus::Unsupported;
        }
    };
    match discover_streamable_http_oauth_for_server(server, url, headers, network_policy).await {
        Ok(Some(_)) => McpAuthStatus::NotLoggedIn,
        Ok(None) => McpAuthStatus::Unsupported,
        Err(err) => {
            tracing::debug!(target: "mcp", server = %name, error = %err, "MCP OAuth discovery failed");
            McpAuthStatus::Unsupported
        }
    }
}

pub async fn oauth_login_support(
    server: &McpServerConfig,
    network_policy: Option<&NetworkPolicyDecider>,
) -> Result<Option<McpOAuthDiscovery>> {
    oauth_login_support_with_auth_context(server, network_policy, super::process_mcp_auth_context())
        .await
}

pub async fn oauth_login_support_with_auth_context(
    server: &McpServerConfig,
    network_policy: Option<&NetworkPolicyDecider>,
    auth_context: Arc<dyn AuthContext>,
) -> Result<Option<McpOAuthDiscovery>> {
    if server.reviewed_plugin.is_some() {
        return Ok(None);
    }
    let Some(url) = server.url.as_deref() else {
        return Ok(None);
    };
    if server_has_manual_authorization(server) {
        return Ok(None);
    }
    let headers = build_default_headers_with_auth_context(server, auth_context.as_ref())?;
    discover_streamable_http_oauth_for_server(server, url, headers, network_policy).await
}

fn oauth_http_client(
    server: &McpServerConfig,
    url: &str,
    network_policy: Option<&NetworkPolicyDecider>,
) -> Result<McpHttpClient> {
    let timeouts = super::McpTimeouts::default();
    McpHttpClient::new(
        url,
        server.runtime_added,
        server.reviewed_plugin.is_some(),
        server.allow_private_network,
        network_policy,
        Duration::from_secs(server.effective_connect_timeout(&timeouts)),
        Duration::from_secs(server.effective_read_timeout(&timeouts)),
    )
    .and_then(McpHttpClient::with_credential_transport)
}

fn oauth_login_client(
    server: &McpServerConfig,
    url: &str,
    network_policy: Option<&NetworkPolicyDecider>,
    auth_context: &dyn AuthContext,
) -> Result<McpHttpClient> {
    let headers = build_default_headers_with_auth_context(server, auth_context)?;
    Ok(oauth_http_client(server, url, network_policy)?.with_default_headers(headers))
}

async fn discover_streamable_http_oauth_for_server(
    server: &McpServerConfig,
    url: &str,
    default_headers: HeaderMap,
    network_policy: Option<&NetworkPolicyDecider>,
) -> Result<Option<McpOAuthDiscovery>> {
    let client =
        oauth_http_client(server, url, network_policy)?.with_default_headers(default_headers);
    discover_streamable_http_oauth_with_client(url, client).await
}

async fn discover_streamable_http_oauth_with_client(
    url: &str,
    client: McpHttpClient,
) -> Result<Option<McpOAuthDiscovery>> {
    let client = Arc::new(RecordingOAuthHttpClient::new(client));
    let manager = AuthorizationManager::new_with_oauth_http_client(url, client).await?;
    match tokio::time::timeout(Duration::from_secs(5), manager.resolve_metadata()).await? {
        Ok(resolution) if resolution.source.is_discovered() => Ok(Some(McpOAuthDiscovery {
            scopes_supported: normalize_scopes(resolution.metadata.scopes_supported),
        })),
        Ok(_) | Err(AuthError::NoAuthorizationSupport) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub fn resolve_oauth_scopes(
    explicit_scopes: Option<Vec<String>>,
    configured_scopes: Vec<String>,
    discovered_scopes: Option<Vec<String>>,
) -> ResolvedMcpOAuthScopes {
    if let Some(scopes) = explicit_scopes {
        return ResolvedMcpOAuthScopes {
            scopes,
            source: McpOAuthScopesSource::Explicit,
        };
    }
    if !configured_scopes.is_empty() {
        return ResolvedMcpOAuthScopes {
            scopes: configured_scopes,
            source: McpOAuthScopesSource::Configured,
        };
    }
    if let Some(scopes) = discovered_scopes
        && !scopes.is_empty()
    {
        return ResolvedMcpOAuthScopes {
            scopes,
            source: McpOAuthScopesSource::Discovered,
        };
    }
    ResolvedMcpOAuthScopes {
        scopes: Vec::new(),
        source: McpOAuthScopesSource::Empty,
    }
}

pub async fn perform_oauth_login_for_server(
    name: &str,
    server: &McpServerConfig,
    explicit_scopes: Option<Vec<String>>,
    callback_port: Option<u16>,
    callback_url: Option<&str>,
    network_policy: Option<&NetworkPolicyDecider>,
) -> Result<()> {
    perform_oauth_login_for_server_with_auth_context(
        name,
        server,
        explicit_scopes,
        callback_port,
        callback_url,
        network_policy,
        super::process_mcp_auth_context(),
    )
    .await
}

pub async fn perform_oauth_login_for_server_with_auth_context(
    name: &str,
    server: &McpServerConfig,
    explicit_scopes: Option<Vec<String>>,
    callback_port: Option<u16>,
    callback_url: Option<&str>,
    network_policy: Option<&NetworkPolicyDecider>,
    auth_context: Arc<dyn AuthContext>,
) -> Result<()> {
    if server.reviewed_plugin.is_some() {
        bail!(
            "OAuth is disabled for plugin-contributed MCP servers; use a reviewed environment-backed header or bearer token"
        );
    }
    perform_oauth_login_for_server_inner(
        name,
        server,
        explicit_scopes,
        callback_port,
        callback_url,
        network_policy,
        auth_context,
    )
    .await
}

/// Run an MCP OAuth login that can be stopped by the caller.
///
/// Cancellation drops the in-flight OAuth future before this function returns,
/// which also closes its callback listener. A caller that replaces one login
/// with another should await the cancelled call before starting the replacement.
pub async fn perform_oauth_login_for_server_with_cancel(
    name: &str,
    server: &McpServerConfig,
    explicit_scopes: Option<Vec<String>>,
    callback_port: Option<u16>,
    callback_url: Option<&str>,
    cancellation_token: CancellationToken,
    network_policy: Option<&NetworkPolicyDecider>,
) -> Result<()> {
    if server.reviewed_plugin.is_some() {
        bail!(
            "OAuth is disabled for plugin-contributed MCP servers; use a reviewed environment-backed header or bearer token"
        );
    }
    run_cancellable_oauth(
        &cancellation_token,
        perform_oauth_login_for_server_with_auth_context(
            name,
            server,
            explicit_scopes,
            callback_port,
            callback_url,
            network_policy,
            super::process_mcp_auth_context(),
        ),
    )
    .await
}

async fn run_cancellable_oauth<F, T>(cancellation_token: &CancellationToken, future: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    tokio::select! {
        biased;
        _ = cancellation_token.cancelled() => bail!("OAuth login was cancelled"),
        result = future => result,
    }
}

/// Shared gate + scope resolution for `/mcp login` and the model-driven
/// authenticate tool: URL-based servers only, no manual Authorization config,
/// scopes from explicit argument, config, or discovery (in that order).
async fn resolve_oauth_login(
    name: &str,
    server: &McpServerConfig,
    explicit_scopes: Option<Vec<String>>,
    network_policy: Option<&NetworkPolicyDecider>,
    auth_context: Arc<dyn AuthContext>,
) -> Result<(String, ResolvedMcpOAuthScopes)> {
    let Some(url) = server.url.as_deref() else {
        bail!("OAuth login is only supported for URL-based MCP servers");
    };
    if server_has_manual_authorization(server) {
        bail!("MCP server '{name}' already has bearer/static Authorization configured");
    }

    let discovery = if explicit_scopes.is_none() && server.scopes.is_empty() {
        oauth_login_support_with_auth_context(server, network_policy, Arc::clone(&auth_context))
            .await?
    } else {
        None
    };
    let resolved_scopes = resolve_oauth_scopes(
        explicit_scopes,
        server.scopes.clone(),
        discovery.and_then(|discovery| discovery.scopes_supported),
    );
    Ok((url.to_string(), resolved_scopes))
}

async fn perform_oauth_login_for_server_inner(
    name: &str,
    server: &McpServerConfig,
    explicit_scopes: Option<Vec<String>>,
    callback_port: Option<u16>,
    callback_url: Option<&str>,
    network_policy: Option<&NetworkPolicyDecider>,
    auth_context: Arc<dyn AuthContext>,
) -> Result<()> {
    let (url, resolved_scopes) = resolve_oauth_login(
        name,
        server,
        explicit_scopes,
        network_policy,
        Arc::clone(&auth_context),
    )
    .await?;

    match perform_oauth_login(
        name,
        &url,
        oauth_login_client(server, &url, network_policy, auth_context.as_ref())?,
        &resolved_scopes.scopes,
        server.oauth.as_ref(),
        server.oauth_resource.as_deref(),
        callback_port,
        callback_url,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(err)
            if resolved_scopes.source == McpOAuthScopesSource::Discovered
                && err
                    .downcast_ref::<OAuthProviderError>()
                    .is_some_and(|error| error.error.as_deref() == Some("invalid_scope")) =>
        {
            println!("OAuth provider rejected discovered scopes. Retrying without scopes...");
            perform_oauth_login(
                name,
                &url,
                oauth_login_client(server, &url, network_policy, auth_context.as_ref())?,
                &[],
                server.oauth.as_ref(),
                server.oauth_resource.as_deref(),
                callback_port,
                callback_url,
            )
            .await
        }
        Err(err) => Err(err),
    }
}

#[allow(clippy::too_many_arguments)]
async fn perform_oauth_login(
    server_name: &str,
    server_url: &str,
    client: McpHttpClient,
    scopes: &[String],
    oauth: Option<&super::McpServerOAuthConfig>,
    oauth_resource: Option<&str>,
    callback_port: Option<u16>,
    callback_url: Option<&str>,
) -> Result<()> {
    OauthLoginFlow::new(
        server_name,
        server_url,
        client,
        scopes,
        oauth,
        oauth_resource,
        callback_port,
        callback_url,
    )
    .await?
    .finish()
    .await
}

/// How an OAuth login announces its authorization URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OAuthLoginAnnounce {
    /// `/mcp login` in a terminal: print the URL and open a browser.
    Terminal,
    /// Model-driven `mcp_<server>_authenticate` tool: never write to stdout
    /// (a tool call inside a running session is not a terminal); the result
    /// carries the URL for the model to relay to the user verbatim.
    Tool { open_browser: bool },
}

/// An in-flight OAuth login started by the model-driven authenticate tool.
///
/// The authorization URL is available immediately so the model can relay it
/// to the user verbatim; [`McpOAuthToolLogin::finish`] then blocks on the
/// loopback callback (up to 15 minutes, same as `/mcp login`) and persists
/// the issued tokens to the shared store on success.
pub struct McpOAuthToolLogin {
    server_name: String,
    server: McpServerConfig,
    scopes_source: McpOAuthScopesSource,
    auth_context: Arc<dyn AuthContext>,
    network_policy: Option<NetworkPolicyDecider>,
    flow: OauthLoginFlow,
    open_browser: bool,
}

impl McpOAuthToolLogin {
    /// The exact authorization URL the user must visit. Treat it as
    /// sensitive: never modify it or strip query parameters.
    #[must_use]
    pub fn authorization_url(&self) -> &str {
        &self.flow.auth_url
    }

    /// Block on the browser callback and persist the issued tokens. Mirrors
    /// the `/mcp login` retry: when the provider rejects scopes that came
    /// from discovery (rather than explicit config), restart once without
    /// scopes.
    pub async fn finish(self) -> Result<()> {
        let announce = OAuthLoginAnnounce::Tool {
            open_browser: self.open_browser,
        };
        let retry_without_scopes = self.scopes_source == McpOAuthScopesSource::Discovered;
        match self.flow.finish_with_announce(announce).await {
            Ok(()) => Ok(()),
            Err(err)
                if retry_without_scopes
                    && err
                        .downcast_ref::<OAuthProviderError>()
                        .is_some_and(|error| error.error.as_deref() == Some("invalid_scope")) =>
            {
                let server = &self.server;
                let url = server
                    .url
                    .as_deref()
                    .expect("tool login is gated to URL-based servers at begin");
                OauthLoginFlow::new(
                    &self.server_name,
                    url,
                    oauth_login_client(
                        server,
                        url,
                        self.network_policy.as_ref(),
                        self.auth_context.as_ref(),
                    )?,
                    &[],
                    server.oauth.as_ref(),
                    server.oauth_resource.as_deref(),
                    None,
                    None,
                )
                .await?
                .finish_with_announce(announce)
                .await
            }
            Err(err) => Err(err),
        }
    }
}

/// Begin the same OAuth login flow `/mcp login` runs, for the model-driven
/// `mcp_<server>_authenticate` tool. The callback listener binds an
/// ephemeral loopback port; callers needing a pre-registered redirect URI
/// keep the terminal `/mcp login <name>` path, which honors the configured
/// callback overrides.
pub async fn begin_oauth_login_for_server_tool(
    name: &str,
    server: &McpServerConfig,
    explicit_scopes: Option<Vec<String>>,
    callback_port: Option<u16>,
    callback_url: Option<&str>,
    network_policy: Option<&NetworkPolicyDecider>,
) -> Result<McpOAuthToolLogin> {
    begin_oauth_login_for_server_tool_with_auth_context(
        name,
        server,
        explicit_scopes,
        callback_port,
        callback_url,
        network_policy,
        super::process_mcp_auth_context(),
    )
    .await
}

pub async fn begin_oauth_login_for_server_tool_with_auth_context(
    name: &str,
    server: &McpServerConfig,
    explicit_scopes: Option<Vec<String>>,
    callback_port: Option<u16>,
    callback_url: Option<&str>,
    network_policy: Option<&NetworkPolicyDecider>,
    auth_context: Arc<dyn AuthContext>,
) -> Result<McpOAuthToolLogin> {
    if server.reviewed_plugin.is_some() {
        bail!(
            "OAuth is disabled for plugin-contributed MCP servers; use a reviewed environment-backed header or bearer token"
        );
    }
    let (url, resolved_scopes) = resolve_oauth_login(
        name,
        server,
        explicit_scopes,
        network_policy,
        Arc::clone(&auth_context),
    )
    .await?;
    let flow = OauthLoginFlow::new(
        name,
        &url,
        oauth_login_client(server, &url, network_policy, auth_context.as_ref())?,
        &resolved_scopes.scopes,
        server.oauth.as_ref(),
        server.oauth_resource.as_deref(),
        callback_port,
        callback_url,
    )
    .await?;
    Ok(McpOAuthToolLogin {
        server_name: name.to_string(),
        server: server.clone(),
        scopes_source: resolved_scopes.source,
        auth_context,
        network_policy: network_policy.cloned(),
        flow,
        // The test build drives the loopback callback itself; a real browser
        // launch from a unit test would hijack the developer's desktop.
        open_browser: !cfg!(any(test, feature = "test-support")),
    })
}

/// Whether the self-serve OAuth login flow can run for this server at all:
/// URL-based, not plugin-contributed (plugin servers authenticate through
/// reviewed environment-backed headers, and OAuth storage is disabled for
/// them), and without a manual Authorization configuration that an OAuth
/// login would conflict with.
pub fn server_supports_oauth_login(server: &McpServerConfig) -> bool {
    server.reviewed_plugin.is_none()
        && server.url.is_some()
        && !server_has_manual_authorization(server)
}

/// Whether the shared token store already holds a usable credential for this
/// server, with its binding revalidated against discovery. Used by the
/// authenticate tool's already-authorized branch.
pub async fn has_usable_stored_tokens(
    name: &str,
    server: &McpServerConfig,
    network_policy: Option<&NetworkPolicyDecider>,
) -> Result<bool> {
    has_usable_stored_tokens_with_auth_context(
        name,
        server,
        network_policy,
        super::process_mcp_auth_context(),
    )
    .await
}

pub async fn has_usable_stored_tokens_with_auth_context(
    name: &str,
    server: &McpServerConfig,
    network_policy: Option<&NetworkPolicyDecider>,
    auth_context: Arc<dyn AuthContext>,
) -> Result<bool> {
    let Some(url) = server.url.as_deref() else {
        return Ok(false);
    };
    if !server_supports_oauth_login(server)
        || !load_oauth_tokens(name, url)?.is_some_and(|tokens| oauth_tokens_are_usable(&tokens))
    {
        return Ok(false);
    }
    match McpOAuthRuntime::from_server_config_with_client(
        name,
        server,
        build_default_headers_with_auth_context(server, auth_context.as_ref())?,
        oauth_login_client(server, url, network_policy, auth_context.as_ref())?,
    )
    .await
    {
        Ok(runtime) => Ok(runtime.is_some()),
        Err(error) if error_looks_auth_required(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Model-facing description for the synthetic `mcp_<server>_authenticate`
/// tool. The coaching contract (show the URL verbatim, the call blocks, real
/// tools replace this one on success) is pinned by tests.
pub fn authenticate_tool_description(server_name: &str) -> String {
    format!(
        "Authenticate with MCP server \"{server_name}\" via OAuth.\n\n\
This server requires an OAuth login that has not yet been completed, so its \
real tools are currently unavailable. Calling this tool starts the \
authorization flow:\n\n\
1. A browser window is opened for the user to sign in and approve the \
Codewhale client, and the exact authorization URL is shown to the user in \
the session status while this call waits. The same URL is returned in this \
call's result; if the user reports the browser did not open, show that URL \
to the user verbatim and ask them to complete the sign-in there.\n\
2. The call blocks (up to 15 minutes) until the browser flow completes on the \
local callback listener, is declined, or times out. Do not assume success \
before the call returns.\n\
3. On success the server reconnects and its real MCP tools replace this \
synthetic authenticate tool, becoming callable from the next model request \
in this session.\n\n\
Treat the URL as sensitive — do not modify it or strip query parameters. If \
the flow is declined, cancelled, or times out, the call returns an error; \
relay it truthfully and suggest `/mcp login {server_name}` in the TUI or \
`codewhale mcp login {server_name}` from a terminal."
    )
}

pub fn delete_oauth_tokens_for_server(name: &str, server: &McpServerConfig) -> Result<bool> {
    if server.reviewed_plugin.is_some() {
        bail!("OAuth storage is disabled for plugin-contributed MCP servers");
    }
    let Some(url) = server.url.as_deref() else {
        bail!("OAuth logout is only supported for URL-based MCP servers");
    };
    delete_oauth_tokens(name, url)
}

pub fn server_has_manual_authorization(server: &McpServerConfig) -> bool {
    server.bearer_token_env_var.is_some()
        || contains_authorization_header(&server.headers)
        || contains_authorization_header(&server.env_headers)
}

pub(super) fn build_default_headers_with_auth_context(
    server: &McpServerConfig,
    auth_context: &dyn AuthContext,
) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    for (name, value) in &server.headers {
        insert_header(&mut headers, name, value)?;
    }
    for (name, env_var) in &server.env_headers {
        if let Some(value) =
            super::mcp_secret_var(server.reviewed_plugin_source(), env_var, auth_context)
            && !value.trim().is_empty()
        {
            insert_header(&mut headers, name, &value)?;
        }
    }
    Ok(headers)
}

fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) -> Result<()> {
    if !super::headers::is_safe_custom_header(name, value) {
        bail!("unsafe MCP HTTP header '{name}'");
    }
    let name = HeaderName::from_bytes(name.as_bytes())
        .with_context(|| format!("invalid MCP HTTP header name '{name}'"))?;
    let value = HeaderValue::from_str(value).with_context(|| "invalid MCP HTTP header value")?;
    headers.insert(name, value);
    Ok(())
}

fn contains_authorization_header(headers: &HashMap<String, String>) -> bool {
    headers
        .keys()
        .any(|key| key.trim().eq_ignore_ascii_case("authorization"))
}

fn normalize_scopes(scopes_supported: Option<Vec<String>>) -> Option<Vec<String>> {
    let scopes_supported = scopes_supported?;
    let mut normalized = Vec::new();
    for scope in scopes_supported {
        let scope = scope.trim();
        if scope.is_empty() {
            continue;
        }
        let scope = scope.to_string();
        if !normalized.contains(&scope) {
            normalized.push(scope);
        }
    }
    (!normalized.is_empty()).then_some(normalized)
}

pub fn load_oauth_tokens(server_name: &str, url: &str) -> Result<Option<StoredMcpOAuthTokens>> {
    let secrets = codewhale_secrets::Secrets::auto_detect();
    let key = store_key(server_name, url);
    let Some(serialized) = secrets
        .get(&key)
        .with_context(|| format!("reading MCP OAuth token for '{server_name}'"))?
    else {
        return Ok(None);
    };
    decode_stored_oauth_tokens(&serialized, server_name).map(Some)
}

fn decode_stored_oauth_tokens(serialized: &str, server_name: &str) -> Result<StoredMcpOAuthTokens> {
    let mut tokens = parse_stored_oauth_tokens(serialized, server_name)?;
    refresh_expires_in_from_timestamp(&mut tokens);
    Ok(tokens)
}

/// Delete the stored credential only while it is still exactly `held`,
/// comparing and deleting under the store's entry lock. Returns the different
/// credential found in its place (left untouched), or `None` when the entry
/// was `held` and is now gone, or was already absent. An unreadable entry is
/// an error and is left in place, as a plain load would report it.
fn delete_oauth_tokens_if_held(
    server_name: &str,
    url: &str,
    held: &StoredMcpOAuthTokens,
) -> Result<Option<StoredMcpOAuthTokens>> {
    let secrets = codewhale_secrets::Secrets::auto_detect();
    let key = store_key(server_name, url);
    secrets
        .with_entry_transaction(&key, |current| {
            let Some(serialized) = current.as_deref() else {
                return Ok(Ok(None));
            };
            Ok(match decode_stored_oauth_tokens(serialized, server_name) {
                Ok(stored) if stored == *held => {
                    *current = None;
                    Ok(None)
                }
                Ok(stored) => Ok(Some(stored)),
                Err(error) => Err(error),
            })
        })
        .with_context(|| format!("clearing the MCP OAuth token for '{server_name}'"))?
}

enum HeldTokenSave {
    Saved,
    Removed,
    Replaced(Box<StoredMcpOAuthTokens>),
}

fn save_oauth_tokens_if_held(
    held: &StoredMcpOAuthTokens,
    tokens: &StoredMcpOAuthTokens,
) -> Result<HeldTokenSave> {
    let secrets = codewhale_secrets::Secrets::auto_detect();
    let key = store_key(&tokens.server_name, &tokens.url);
    let replacement = serde_json::to_string(tokens).context("serializing MCP OAuth token")?;
    secrets
        .with_entry_transaction(&key, |current| {
            let Some(serialized) = current.as_deref() else {
                return Ok(Ok(HeldTokenSave::Removed));
            };
            Ok(
                match decode_stored_oauth_tokens(serialized, &tokens.server_name) {
                    Ok(stored) if stored == *held => {
                        *current = Some(replacement);
                        Ok(HeldTokenSave::Saved)
                    }
                    Ok(stored) => Ok(HeldTokenSave::Replaced(Box::new(stored))),
                    Err(error) => Err(error),
                },
            )
        })
        .with_context(|| format!("saving MCP OAuth token for '{}'", tokens.server_name))?
}

static DYNAMIC_REGISTRATION_UNAVAILABLE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<(String, String)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

fn note_dynamic_registration(server_name: &str, url: &str, available: bool) {
    let mut unavailable = DYNAMIC_REGISTRATION_UNAVAILABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = (server_name.to_string(), url.to_string());
    if available {
        unavailable.remove(&key);
    } else {
        unavailable.insert(key);
    }
}

pub fn dynamic_registration_unavailable(server_name: &str, url: &str) -> bool {
    DYNAMIC_REGISTRATION_UNAVAILABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&(server_name.to_string(), url.to_string()))
}

fn error_is_dynamic_registration_unsupported(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<AuthError>(),
        Some(AuthError::RegistrationFailed(message))
            if message.contains("Dynamic client registration not supported")
    )
}

pub fn dynamic_registration_unavailable_hint(server_name: &str) -> String {
    format!(
        "MCP server '{server_name}' cannot complete an OAuth login: its authorization server offers no usable client registration. Configure `oauth.client_metadata_url` for a hosted client document, or `oauth.client_id` with its exact `oauth.issuer`, then log in again. Alternatively set `bearer_token_env_var` to an environment variable holding a least-privilege personal access token and run `/mcp reload`"
    )
}

fn parse_stored_oauth_tokens(serialized: &str, server_name: &str) -> Result<StoredMcpOAuthTokens> {
    serde_json::from_str(serialized).map_err(|_| {
        anyhow!(
            "stored MCP OAuth token for '{server_name}' is not valid credential JSON; contents were omitted"
        )
    })
}

pub fn save_oauth_tokens(tokens: &StoredMcpOAuthTokens) -> Result<()> {
    let secrets = codewhale_secrets::Secrets::auto_detect();
    let key = store_key(&tokens.server_name, &tokens.url);
    let serialized = serde_json::to_string(tokens).context("serializing MCP OAuth token")?;
    secrets
        .set(&key, &serialized)
        .with_context(|| format!("saving MCP OAuth token for '{}'", tokens.server_name))
}

fn delete_oauth_tokens(server_name: &str, url: &str) -> Result<bool> {
    let secrets = codewhale_secrets::Secrets::auto_detect();
    let key = store_key(server_name, url);
    let existed = secrets
        .get(&key)
        .with_context(|| format!("reading MCP OAuth token for '{server_name}'"))?
        .is_some();
    secrets
        .delete(&key)
        .with_context(|| format!("deleting MCP OAuth token for '{server_name}'"))?;
    Ok(existed)
}

fn store_key(server_name: &str, url: &str) -> String {
    let mut payload = Vec::with_capacity(server_name.len() + url.len() + 1);
    payload.extend_from_slice(server_name.as_bytes());
    payload.push(0);
    payload.extend_from_slice(url.as_bytes());
    let digest = Sha256::digest(&payload);
    format!("mcp_oauth_{}", URL_SAFE_NO_PAD.encode(digest))
}

fn oauth_tokens_are_usable(tokens: &StoredMcpOAuthTokens) -> bool {
    if tokens.client_id.trim().is_empty()
        || tokens
            .binding
            .as_ref()
            .is_none_or(|binding| binding.issuer.is_empty() || binding.resource.is_empty())
    {
        return false;
    }
    let response = &tokens.token_response.0;
    if token_needs_refresh(tokens.expires_at) {
        return response
            .refresh_token()
            .is_some_and(|token| !token.secret().trim().is_empty());
    }
    !response.access_token().secret().trim().is_empty()
}

fn refresh_expires_in_from_timestamp(tokens: &mut StoredMcpOAuthTokens) {
    let Some(expires_at) = tokens.expires_at else {
        return;
    };
    match expires_in_from_timestamp(expires_at) {
        Some(seconds) => {
            let duration = Duration::from_secs(seconds);
            tokens.token_response.0.set_expires_in(Some(&duration));
        }
        None => {
            tokens
                .token_response
                .0
                .set_expires_in(Some(&Duration::ZERO));
        }
    }
}

fn compute_expires_at_millis(response: &OAuthTokenResponse) -> Option<u64> {
    let expires = response.expires_in()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    Some(now.saturating_add(expires.as_millis() as u64))
}

fn expires_in_from_timestamp(expires_at: u64) -> Option<u64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    if expires_at <= now {
        return None;
    }
    Some((expires_at - now) / 1000)
}

fn token_needs_refresh(expires_at: Option<u64>) -> bool {
    let Some(expires_at) = expires_at else {
        return false;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    now.saturating_add(REFRESH_SKEW_MILLIS) >= expires_at
}

struct CallbackServerGuard {
    accept_task: tokio::task::JoinHandle<()>,
}

impl Drop for CallbackServerGuard {
    fn drop(&mut self) {
        // Aborting drops the accept future and its owned listener instead of
        // leaving a detached task holding a fixed callback port indefinitely.
        self.accept_task.abort();
    }
}

struct OauthLoginFlow {
    auth_url: String,
    oauth_state: OAuthState,
    rx: oneshot::Receiver<CallbackResult>,
    guard: CallbackServerGuard,
    server_name: String,
    server_url: String,
    binding: McpOAuthBinding,
}

impl OauthLoginFlow {
    #[allow(clippy::too_many_arguments)]
    async fn new(
        server_name: &str,
        server_url: &str,
        client: McpHttpClient,
        scopes: &[String],
        oauth: Option<&super::McpServerOAuthConfig>,
        oauth_resource: Option<&str>,
        callback_port: Option<u16>,
        callback_url: Option<&str>,
    ) -> Result<Self> {
        let bind_host = callback_bind_host(callback_url);
        let bind_addr = match callback_port {
            Some(0) => bail!("invalid MCP OAuth callback port 0"),
            Some(port) => format!("{bind_host}:{port}"),
            None => format!("{bind_host}:0"),
        };
        let listener = TcpListener::bind(&bind_addr)
            .await
            .map_err(|err| anyhow!(err))?;
        let redirect_uri = resolve_redirect_uri(&listener, callback_url)?;
        let callback_id = callback_id_from_server_url(server_url)?;
        let redirect_uri = append_callback_id_to_redirect_uri(&redirect_uri, &callback_id)?;
        let callback_path = callback_path_from_redirect_uri(&redirect_uri)?;

        let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
        let (oauth_state, binding, callback_states) = match start_authorization(
            server_url,
            client,
            &scope_refs,
            &redirect_uri,
            oauth,
            oauth_resource,
        )
        .await
        {
            Ok(prepared) => {
                note_dynamic_registration(server_name, server_url, true);
                prepared
            }
            Err(error) if error_is_dynamic_registration_unsupported(&error) => {
                note_dynamic_registration(server_name, server_url, false);
                return Err(error.context(dynamic_registration_unavailable_hint(server_name)));
            }
            Err(error) => return Err(error),
        };
        let auth_url = oauth_state.get_authorization_url().await?;
        anyhow::ensure!(
            authorization_request_parameter(&auth_url, "resource")? == binding.resource,
            "MCP OAuth authorization resource changed during preparation"
        );
        let (tx, rx) = oneshot::channel();
        let guard = CallbackServerGuard {
            accept_task: spawn_callback_server(listener, tx, callback_path, callback_states),
        };
        // #6040: logout clears this machine's token only — the provider keeps
        // its standing grant. Without a forced prompt the next login silently
        // re-grants it (same account/workspace, no picker ever shown), so an
        // explicit login could never change the authorized workspace.
        let auth_url = append_query_param(&auth_url, "prompt", Some("consent"));

        Ok(Self {
            auth_url,
            oauth_state,
            rx,
            guard,
            server_name: server_name.to_string(),
            server_url: server_url.to_string(),
            binding,
        })
    }

    async fn finish(self) -> Result<()> {
        self.finish_with_announce(OAuthLoginAnnounce::Terminal)
            .await
    }

    async fn finish_with_announce(mut self, announce: OAuthLoginAnnounce) -> Result<()> {
        match announce {
            OAuthLoginAnnounce::Terminal => {
                println!(
                    "Authorize `{}` by opening this URL in your browser:\n{}\n",
                    self.server_name, self.auth_url
                );
                if webbrowser::open(&self.auth_url).is_err() {
                    eprintln!("Browser launch failed; copy the URL above manually.");
                }
                println!(
                    "Waiting for browser authorization for MCP server '{}'...",
                    self.server_name
                );
            }
            OAuthLoginAnnounce::Tool { open_browser } => {
                // A tool call is not a terminal: nothing goes to stdout. The
                // tool result carries the URL for the model to relay; the
                // browser open is a best-effort convenience on top.
                if open_browser {
                    let _ = webbrowser::open(&self.auth_url);
                }
            }
        }

        let result = async {
            // Allow time for the user to finish browser authentication and 2FA.
            let callback = timeout(Duration::from_secs(900), &mut self.rx)
                .await
                .with_context(|| {
                    let retry_hint = match announce {
                        OAuthLoginAnnounce::Terminal => "Retry from a terminal, or use a background shell (background=true) if an agent is running the login flow.".to_string(),
                        OAuthLoginAnnounce::Tool { .. } => format!(
                            "The user can complete the sign-in directly via `/mcp login {}` or `codewhale mcp login {}`, then this tool can be called again.",
                            self.server_name, self.server_name
                        ),
                    };
                    format!(
                        "timed out waiting for OAuth callback for MCP server '{}'. {retry_hint}",
                        self.server_name
                    )
                })?
                .context("OAuth callback was cancelled")?;
            let OauthCallbackResult {
                code,
                state,
                issuer,
            } = match callback {
                CallbackResult::Success(callback) => callback,
                CallbackResult::Error(error) => return Err(anyhow!(error)),
            };

            // RFC 9207: servers that advertise
            // `authorization_response_iss_parameter_supported` send `iss` on the
            // redirect and rmcp requires it back; forward it so the callback binds
            // to the discovered issuer instead of failing as "missing".
            self.oauth_state
                .handle_callback_with_issuer(&code, &state, issuer.as_deref())
                .await
                .context("handling MCP OAuth callback")?;

            let (client_id, credentials) = self
                .oauth_state
                .get_credentials()
                .await
                .context("reading MCP OAuth credentials")?;
            let credentials =
                credentials.ok_or_else(|| anyhow!("OAuth provider did not return credentials"))?;
            let stored = StoredMcpOAuthTokens {
                server_name: self.server_name.clone(),
                url: self.server_url.clone(),
                client_id,
                binding: Some(self.binding.clone()),
                expires_at: compute_expires_at_millis(&credentials),
                token_response: WrappedOAuthTokenResponse(credentials),
            };
            save_oauth_tokens(&stored)
        }
        .await;

        drop(self.guard);
        result
    }
}

async fn start_authorization(
    server_url: &str,
    client: McpHttpClient,
    scopes: &[&str],
    redirect_uri: &str,
    oauth: Option<&super::McpServerOAuthConfig>,
    oauth_resource: Option<&str>,
) -> Result<(OAuthState, McpOAuthBinding, InMemoryStateStore)> {
    let http_client = Arc::new(RecordingOAuthHttpClient::new(client));
    let (mut manager, binding) = prepare_authorization_manager(server_url, &http_client).await?;
    if let Some(resource) = oauth_resource {
        anyhow::ensure!(
            resource == binding.resource,
            "Configured MCP OAuth resource differs from the discovered resource"
        );
    }
    let callback_states = InMemoryStateStore::new();
    manager.set_state_store(callback_states.clone());
    if let Some(issuer) = oauth.and_then(|oauth| oauth.issuer.as_deref()) {
        anyhow::ensure!(
            issuer == binding.issuer,
            "Configured MCP OAuth issuer does not match the discovered authorization server"
        );
    }
    if let Some(client_id) = oauth.and_then(|oauth| oauth.client_id.as_deref()) {
        anyhow::ensure!(
            !client_id.trim().is_empty(),
            "MCP OAuth client_id must not be empty"
        );
        anyhow::ensure!(
            oauth.and_then(|oauth| oauth.issuer.as_deref()).is_some(),
            "Pre-registered MCP OAuth clients require oauth.issuer matching the registration authority"
        );
    }
    if let Some(metadata_url) = oauth.and_then(|oauth| oauth.client_metadata_url.as_deref()) {
        let url = Url::parse(metadata_url).context("invalid MCP OAuth client metadata URL")?;
        anyhow::ensure!(
            url.scheme() == "https"
                && url.host_str().is_some()
                && url.path() != "/"
                && !url.path().is_empty()
                && url.fragment().is_none()
                && url.username().is_empty()
                && url.password().is_none(),
            "MCP OAuth client metadata URL must be an HTTPS URL with a non-root path and no credentials or fragment"
        );
    }
    let mut attempt_scopes: Vec<String> = scopes.iter().map(|scope| (*scope).to_string()).collect();
    for retried in [false, true] {
        let mut request = AuthorizationRequest::new(redirect_uri)
            .with_scopes(attempt_scopes.iter().map(String::as_str))
            .with_client_name("Codewhale")
            .with_application_type("native");
        if let Some(client_id) = oauth.and_then(|oauth| oauth.client_id.as_deref()) {
            request = request.with_preregistered_client(client_id);
        }
        if let Some(metadata_url) = oauth.and_then(|oauth| oauth.client_metadata_url.as_deref()) {
            request = request.with_client_metadata_url(metadata_url);
        }
        match AuthorizationSession::new(manager, request).await {
            Ok(session) => return Ok((OAuthState::Session(session), binding, callback_states)),
            Err((returned, error)) => {
                manager = returned;
                if retried || attempt_scopes.is_empty() {
                    return Err(error.into());
                }
                let Some(narrowed) =
                    scopes_after_registration_rejection(&attempt_scopes, &error.to_string())
                else {
                    return Err(error.into());
                };
                tracing::warn!(target: "mcp::oauth", server_url,
                    dropped = attempt_scopes.len() - narrowed.len(),
                    "OAuth client registration rejected part of the requested scope list; retrying with the accepted scopes");
                attempt_scopes = narrowed;
            }
        }
    }
    unreachable!("registration retry loop returns on success or error")
}

/// Given a registration failure message, return the scopes to retry with, or
/// `None` when the failure is not about scopes. Servers that validate the
/// `scope` field report positions like `scope.3: Invalid option`; those exact
/// entries are dropped. A scope error without positions retries with no
/// scopes at all, letting the server grant its defaults.
fn scopes_after_registration_rejection(scopes: &[String], message: &str) -> Option<Vec<String>> {
    let lower = message.to_ascii_lowercase();
    if !(lower.contains("registration") && lower.contains("scope")) {
        return None;
    }
    let mut rejected = std::collections::BTreeSet::new();
    for (start, _) in message.match_indices("scope.") {
        let digits: String = message[start + "scope.".len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(index) = digits.parse::<usize>() {
            rejected.insert(index);
        }
    }
    let narrowed: Vec<String> = scopes
        .iter()
        .enumerate()
        .filter(|(index, _)| !rejected.contains(index))
        .map(|(_, scope)| scope.clone())
        .collect();
    if narrowed.len() == scopes.len() {
        // The server complained about scopes without naming any position:
        // the only safe retry is to omit the field.
        return Some(Vec::new());
    }
    Some(narrowed)
}

fn spawn_callback_server(
    listener: TcpListener,
    tx: oneshot::Sender<CallbackResult>,
    expected_callback_path: String,
    callback_states: InMemoryStateStore,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // The sender is wrapped in Option so we can take it on success/error
        let mut tx_opt = Some(tx);
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            let path = match timeout(Duration::from_secs(5), read_http_path(&mut stream)).await {
                Ok(Some(p)) => p,
                _ => {
                    let _ = write_http_response(&mut stream, 400, "Invalid OAuth callback").await;
                    continue;
                }
            };
            let callback = parse_oauth_callback(&path, &expected_callback_path);
            let binding = match &callback {
                CallbackOutcome::Success(callback) => {
                    Some((callback.state.as_str(), callback.issuer.as_deref()))
                }
                CallbackOutcome::Error { state, issuer, .. } => {
                    Some((state.as_str(), issuer.as_deref()))
                }
                CallbackOutcome::Invalid => None,
            };
            let valid = if let Some((state, issuer)) = binding {
                callback_binding_is_valid(&callback_states, state, issuer).await
            } else {
                false
            };
            if !valid {
                let _ = write_http_response(&mut stream, 400, "Invalid OAuth callback").await;
                continue;
            }
            match callback {
                CallbackOutcome::Success(callback) => {
                    let _ = write_http_response(
                        &mut stream,
                        200,
                        "Authorization response received. Return to Codewhale to finish signing in.",
                    )
                    .await;
                    if let Some(tx) = tx_opt.take() {
                        let _ = tx.send(CallbackResult::Success(callback));
                    }
                    break;
                }
                CallbackOutcome::Error { error, state, .. } => {
                    if callback_states.delete(&state).await.is_err() {
                        let _ =
                            write_http_response(&mut stream, 400, "Invalid OAuth callback").await;
                        continue;
                    }
                    let msg = mask_oauth_secrets(&error.to_string());
                    let _ = write_http_response(&mut stream, 400, &msg).await;
                    if let Some(tx) = tx_opt.take() {
                        let _ = tx.send(CallbackResult::Error(error));
                    }
                    break;
                }
                CallbackOutcome::Invalid => {
                    let _ = write_http_response(&mut stream, 400, "Invalid OAuth callback").await;
                }
            }
        }
    })
}

async fn callback_binding_is_valid(
    states: &InMemoryStateStore,
    state: &str,
    issuer: Option<&str>,
) -> bool {
    let Ok(Some(stored)) = states.load(state).await else {
        return false;
    };
    let Some(expected) = stored.expected_issuer.as_deref() else {
        return false;
    };
    issuer.map_or(!stored.require_issuer, |issuer| issuer == expected)
}

async fn read_http_path(stream: &mut tokio::net::TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    // Read until we have \r\n\r\n or exceed limit
    loop {
        match stream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.len() > 8192 {
                    return None;
                }
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => return None,
        }
    }
    let request = String::from_utf8_lossy(&buf);
    let first_line = request.lines().next()?;
    // Expected: GET /callback?code=... HTTP/1.1
    let mut parts = first_line.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let path = parts.next()?.to_string();
    Some(path)
}

async fn write_http_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: &str,
) -> std::io::Result<()> {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "OK",
    };
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OauthCallbackResult {
    code: String,
    state: String,
    /// RFC 9207 `iss` from the redirect, when the authorization server sends it.
    issuer: Option<String>,
}

enum CallbackResult {
    Success(OauthCallbackResult),
    Error(OAuthProviderError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CallbackOutcome {
    Success(OauthCallbackResult),
    Error {
        error: OAuthProviderError,
        state: String,
        issuer: Option<String>,
    },
    Invalid,
}

fn parse_oauth_callback(path: &str, expected_callback_path: &str) -> CallbackOutcome {
    let Some((route, query)) = path.split_once('?') else {
        return CallbackOutcome::Invalid;
    };
    if route != expected_callback_path {
        return CallbackOutcome::Invalid;
    }

    let mut parameters = HashMap::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if matches!(
            key.as_ref(),
            "code" | "state" | "iss" | "error" | "error_description"
        ) && parameters
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return CallbackOutcome::Invalid;
        }
    }
    let Some(state) = parameters.remove("state").filter(|state| !state.is_empty()) else {
        return CallbackOutcome::Invalid;
    };
    let code = parameters.remove("code");
    let issuer = parameters.remove("iss");
    let error = parameters.remove("error");
    let error_description = parameters.remove("error_description");
    if code.is_some() && (error.is_some() || error_description.is_some()) {
        return CallbackOutcome::Invalid;
    }
    if let Some(code) = code.filter(|code| !code.is_empty()) {
        return CallbackOutcome::Success(OauthCallbackResult {
            code,
            state,
            issuer,
        });
    }
    if let Some(error) = error.filter(|error| !error.is_empty()) {
        return CallbackOutcome::Error {
            error: OAuthProviderError::new(Some(error), error_description),
            state,
            issuer,
        };
    }
    CallbackOutcome::Invalid
}

fn local_redirect_uri(listener: &TcpListener) -> Result<String> {
    let addr = listener.local_addr()?;
    match addr {
        std::net::SocketAddr::V4(v4) => Ok(format!("http://{}:{}/callback", v4.ip(), v4.port())),
        std::net::SocketAddr::V6(v6) => Ok(format!("http://[{}]:{}/callback", v6.ip(), v6.port())),
    }
}

fn resolve_redirect_uri(listener: &TcpListener, callback_url: Option<&str>) -> Result<String> {
    let Some(callback_url) = callback_url else {
        return local_redirect_uri(listener);
    };
    Url::parse(callback_url)
        .with_context(|| format!("invalid MCP OAuth callback URL '{callback_url}'"))?;
    Ok(callback_url.to_string())
}

fn callback_bind_host(callback_url: Option<&str>) -> &'static str {
    let Some(callback_url) = callback_url else {
        return "127.0.0.1";
    };
    let Ok(parsed) = Url::parse(callback_url) else {
        return "127.0.0.1";
    };
    match parsed.host_str() {
        Some("localhost" | "127.0.0.1" | "::1") | None => "127.0.0.1",
        Some(_) => "0.0.0.0",
    }
}

fn callback_id_from_server_url(server_url: &str) -> Result<String> {
    let mut parsed =
        Url::parse(server_url).with_context(|| format!("invalid MCP server URL '{server_url}'"))?;
    parsed
        .host_str()
        .ok_or_else(|| anyhow!("MCP server URL '{server_url}' must include a host"))?;
    parsed.set_fragment(None);
    let digest = Sha256::digest(parsed.as_str().as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(&digest[..9]))
}

fn append_callback_id_to_redirect_uri(redirect_uri: &str, callback_id: &str) -> Result<String> {
    let mut parsed = Url::parse(redirect_uri)
        .with_context(|| format!("invalid redirect URI '{redirect_uri}'"))?;
    let path = parsed.path();
    let new_path = if path.ends_with('/') {
        format!("{path}{callback_id}")
    } else {
        format!("{path}/{callback_id}")
    };
    parsed.set_path(&new_path);
    Ok(parsed.to_string())
}

fn callback_path_from_redirect_uri(redirect_uri: &str) -> Result<String> {
    let parsed = Url::parse(redirect_uri)
        .with_context(|| format!("invalid redirect URI '{redirect_uri}'"))?;
    Ok(parsed.path().to_string())
}

fn append_query_param(url: &str, key: &str, value: Option<&str>) -> String {
    let Some(value) = value else {
        return url.to_string();
    };
    let value = value.trim();
    if value.is_empty() {
        return url.to_string();
    }
    if let Ok(mut parsed) = Url::parse(url) {
        parsed.query_pairs_mut().append_pair(key, value);
        return parsed.to_string();
    }
    let separator = if url.contains('?') { "&" } else { "?" };
    format!("{url}{separator}{key}={}", urlencoding::encode(value))
}

pub trait McpServerConfigOAuthExt {
    fn oauth_client_id(&self) -> Option<&str>;
}

impl McpServerConfigOAuthExt for McpServerConfig {
    fn oauth_client_id(&self) -> Option<&str> {
        self.oauth
            .as_ref()
            .and_then(|oauth| oauth.client_id.as_deref())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn typed_oauth_client_requires_https_or_explicit_loopback_without_header_guessing() {
        codewhale_release::tls::ensure_rustls_crypto_provider();
        let server: super::McpServerConfig = serde_json::from_value(serde_json::json!({
            "url": "https://example.invalid/mcp"
        }))
        .unwrap();
        let error = super::oauth_http_client(&server, "http://example.invalid/mcp", None)
            .err()
            .expect("OAuth form credentials must not use public HTTP");
        assert!(error.to_string().contains("credentials require HTTPS"));
        assert!(super::oauth_http_client(&server, "https://example.invalid/mcp", None).is_ok());
        assert!(super::oauth_http_client(&server, "http://127.0.0.1/mcp", None).is_ok());
        assert!(
            super::oauth_http_client(
                &server,
                "https://fixture-user:fixture-password@example.invalid/mcp",
                None
            )
            .is_err()
        );
    }

    #[test]
    fn registration_rejection_drops_exactly_the_named_scopes() {
        let scopes: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let message = concat!(
            "Registration failed: Dynamic registration failed: HTTP 400 Bad Request: ",
            "{\"message\":\"scope.1: Invalid option: expected one of \\\"a\\\"|\\\"c\\\",",
            "scope.3: Invalid option\"}"
        );
        assert_eq!(
            super::scopes_after_registration_rejection(&scopes, message),
            Some(vec!["a".to_string(), "c".to_string()])
        );
        // A scope complaint without positions retries without scopes.
        assert_eq!(
            super::scopes_after_registration_rejection(
                &scopes,
                "Registration failed: invalid scope"
            ),
            Some(Vec::new())
        );
        // Unrelated registration failures are not retried.
        assert_eq!(
            super::scopes_after_registration_rejection(&scopes, "Registration failed: HTTP 500"),
            None
        );
        assert_eq!(
            super::scopes_after_registration_rejection(&scopes, "network unreachable"),
            None
        );
    }

    #[test]
    fn a_refresh_parse_failure_names_the_login_remedy_and_the_server() {
        let text = super::refresh_failure_context("supabase", true, None);
        assert!(text.contains("server supabase"));
        assert!(text.contains("codewhale mcp login supabase"), "{text}");
        assert!(text.contains("/mcp login supabase"), "{text}");
        assert!(!text.contains("it answered"), "{text}");
        // An auth-required failure keeps the plain context: the typed state
        // and the login tool already carry the remedy.
        let plain = super::refresh_failure_context("supabase", false, None);
        assert_eq!(plain, "refreshing MCP OAuth token for server supabase");
    }

    #[test]
    fn a_refresh_parse_failure_keeps_the_token_endpoints_receipt() {
        // The supabase receipt (#5926): rmcp said only "Failed to parse
        // server response". With the status line and a masked excerpt the
        // operator can tell a provider's HTML 502 from our parser.
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        let receipt = TokenEndpointReceipt::from_response(
            reqwest::StatusCode::BAD_GATEWAY,
            &headers,
            b"<html>\n  <body>502 Bad Gateway</body>\n</html>",
        );
        let text = super::refresh_failure_context("supabase", true, Some(&receipt));
        assert!(
            text.contains(
                "it answered HTTP 502 Bad Gateway (text/html; charset=utf-8): <html> <body>502 Bad Gateway</body> </html>"
            ),
            "{text}"
        );
        assert!(text.contains("codewhale mcp login supabase"), "{text}");
    }

    #[test]
    fn a_token_receipt_masks_credentials_before_it_cuts_the_body() {
        let secret = "sk-live-0123456789abcdef";
        let long_tail = "x".repeat(400);
        let body = format!(
            "{{\"token_type\":\"Bearer\",\"access_token\":\"{secret}\",\"refresh_token\": \"{secret}-r\",\"id_token\":\"{secret}-id\",\"expires_in\":\"soon\",\"note\":\"{long_tail}\"}}"
        );
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let receipt =
            TokenEndpointReceipt::from_response(reqwest::StatusCode::OK, &headers, body.as_bytes());
        let text = receipt.to_string();
        assert!(!text.contains(secret), "{text}");
        assert!(text.contains("\"access_token\":\"***\""), "{text}");
        assert!(text.contains("\"refresh_token\": \"***\""), "{text}");
        assert!(text.contains("\"id_token\":\"***\""), "{text}");
        // Non-secret members survive so the shape of the answer is readable.
        assert!(text.contains("\"expires_in\":\"soon\""), "{text}");
        assert!(text.contains("\"token_type\":\"Bearer\""), "{text}");
        assert!(text.ends_with('…'), "{text}");
        assert!(receipt.excerpt.len() <= TOKEN_RECEIPT_EXCERPT_BYTES + '…'.len_utf8());
    }

    #[test]
    fn oauth_secret_masking_covers_form_pairs_bearer_schemes_and_case() {
        assert_eq!(
            mask_oauth_secrets("client_secret=abc123&grant_type=refresh_token&refresh_token=zzz"),
            "client_secret=***&grant_type=refresh_token&refresh_token=***"
        );
        assert_eq!(
            mask_oauth_secrets("Authorization: Bearer eyJhbGciOi.payload.sig, retry"),
            "Authorization: ***, retry"
        );
        assert_eq!(
            mask_oauth_secrets("{\"Access_Token\": \"quoted \\\" inside\", \"scope\": \"read\"}"),
            "{\"Access_Token\": \"***\", \"scope\": \"read\"}"
        );
        // A field name that merely contains a secret name is not a secret.
        assert_eq!(
            mask_oauth_secrets("{\"error_code\":\"invalid_request\",\"my_access_token_count\":3}"),
            "{\"error_code\":\"invalid_request\",\"my_access_token_count\":3}"
        );
        // Multi-byte text around a secret stays intact.
        assert_eq!(
            mask_oauth_secrets("トークン access_token=秘密 終わり"),
            "トークン access_token=*** 終わり"
        );
    }

    #[test]
    fn a_token_receipt_names_an_empty_body_and_a_missing_content_type() {
        let receipt = TokenEndpointReceipt::from_response(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            &HeaderMap::new(),
            b"",
        );
        assert_eq!(
            receipt.to_string(),
            "HTTP 503 Service Unavailable (no content-type) with an empty body"
        );
    }

    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn stored_credential_identity_ignores_only_a_derived_expiry_countdown() {
        let original = serde_json::json!({
            "server_name": "fixture", "url": "https://example.invalid/mcp",
            "client_id": "fixture-client", "expires_at": 9_999_999_999_999_u64,
            "binding": { "issuer": "https://example.invalid", "resource": "https://example.invalid/mcp" },
            "token_response": {
                "access_token": "fixture-access", "refresh_token": "fixture-refresh",
                "token_type": "Bearer", "expires_in": 3600, "scope": "read"
            }
        });
        let held: StoredMcpOAuthTokens = serde_json::from_value(original.clone()).unwrap();
        let mut aged = original.clone();
        aged["token_response"]["expires_in"] = serde_json::json!(3598);
        let loaded: StoredMcpOAuthTokens = serde_json::from_value(aged.clone()).unwrap();
        assert!(
            held == loaded,
            "elapsed time alone is not credential rotation"
        );
        for (field, value) in [
            ("access_token", "new-access"),
            ("refresh_token", "new-refresh"),
            ("scope", "read write"),
            ("token_type", "Mac"),
        ] {
            let mut rotated = aged.clone();
            rotated["token_response"][field] = serde_json::json!(value);
            let rotated: StoredMcpOAuthTokens = serde_json::from_value(rotated).unwrap();
            assert!(
                held != rotated,
                "a changed {field} remains a distinct credential"
            );
        }
        for (field, value) in [
            ("server_name", serde_json::json!("other")),
            ("client_id", serde_json::json!("other-client")),
            ("url", serde_json::json!("https://other.invalid/mcp")),
            (
                "binding",
                serde_json::json!({"issuer":"https://other.invalid", "resource":"https://example.invalid/mcp"}),
            ),
            (
                "binding",
                serde_json::json!({"issuer":"https://example.invalid", "resource":"https://other.invalid/mcp"}),
            ),
            ("expires_at", serde_json::json!(9_999_999_999_998_u64)),
        ] {
            let mut rotated = aged.clone();
            rotated[field] = value;
            let rotated: StoredMcpOAuthTokens = serde_json::from_value(rotated).unwrap();
            assert!(
                held != rotated,
                "a changed {field} remains a distinct credential"
            );
        }
        let mut unbound = original.clone();
        unbound.as_object_mut().unwrap().remove("binding");
        let unbound: StoredMcpOAuthTokens = serde_json::from_value(unbound).unwrap();
        assert!(unbound.binding.is_none());
        assert!(!oauth_tokens_are_usable(&unbound));
        assert!(oauth_tokens_are_usable(&held));
        let mut legacy = held.clone();
        legacy.expires_at = None;
        let mut legacy_aged = loaded;
        legacy_aged.expires_at = None;
        assert!(
            legacy != legacy_aged,
            "without a durable deadline the stored lifetime is meaningful"
        );
    }

    #[test]
    fn resolve_oauth_scopes_prefers_explicit() {
        let resolved = resolve_oauth_scopes(
            Some(vec!["explicit".to_string()]),
            vec!["configured".to_string()],
            Some(vec!["discovered".to_string()]),
        );
        assert_eq!(resolved.source, McpOAuthScopesSource::Explicit);
        assert_eq!(resolved.scopes, vec!["explicit"]);
    }

    #[test]
    fn parse_oauth_callback_accepts_success() {
        let parsed = parse_oauth_callback("/callback/id?code=abc&state=xyz", "/callback/id");
        assert_eq!(
            parsed,
            CallbackOutcome::Success(OauthCallbackResult {
                code: "abc".to_string(),
                state: "xyz".to_string(),
                issuer: None,
            })
        );
    }

    #[test]
    fn parse_oauth_callback_keeps_rfc9207_issuer() {
        // Cloudflare's MCP authorization server advertises
        // authorization_response_iss_parameter_supported and sends `iss` back;
        // dropping it makes rmcp reject the callback as missing a required issuer.
        let parsed = parse_oauth_callback(
            "/callback/id?code=abc&state=xyz&iss=https%3A%2F%2Fmcp.cloudflare.com",
            "/callback/id",
        );
        assert_eq!(
            parsed,
            CallbackOutcome::Success(OauthCallbackResult {
                code: "abc".to_string(),
                state: "xyz".to_string(),
                issuer: Some("https://mcp.cloudflare.com".to_string()),
            })
        );
    }

    #[test]
    fn parse_oauth_callback_accepts_provider_error() {
        let parsed = parse_oauth_callback(
            "/callback/id?error=invalid_scope&error_description=not+allowed&state=xyz&iss=https%3A%2F%2Fissuer.invalid",
            "/callback/id",
        );
        assert_eq!(
            parsed,
            CallbackOutcome::Error {
                error: OAuthProviderError::new(
                    Some("invalid_scope".to_string()),
                    Some("not allowed".to_string())
                ),
                state: "xyz".to_string(),
                issuer: Some("https://issuer.invalid".to_string()),
            }
        );
        for query in [
            "error=denied",
            "error=denied&state=",
            "error=denied&state=a&state=b",
            "code=ok&error=denied&state=a",
            "code=ok&state=a&iss=a&iss=b",
            "error_description=denied&state=a",
            "code=&state=a",
        ] {
            assert_eq!(
                parse_oauth_callback(&format!("/callback/id?{query}"), "/callback/id"),
                CallbackOutcome::Invalid
            );
        }
    }

    #[test]
    fn store_key_does_not_include_raw_url_or_name() {
        let key = store_key("github", "https://example.com/mcp");
        assert!(key.starts_with("mcp_oauth_"));
        assert!(!key.contains("github"));
        assert!(!key.contains("example.com"));
    }

    #[test]
    fn malformed_stored_oauth_diagnostic_omits_secret_contents_and_keys() {
        let secret = "cw-secret-mcp-oauth-4507";
        let serialized =
            format!(r#"{{"token_response":{{"access_token":"{secret}"}} trailing-junk}}"#);
        let error = parse_stored_oauth_tokens(&serialized, "private")
            .expect_err("malformed credential JSON must fail");
        let diagnostic = format!("{error:#}");
        assert!(!diagnostic.contains(secret), "{diagnostic}");
        assert!(!diagnostic.contains("access_token"), "{diagnostic}");
        assert!(diagnostic.contains("contents were omitted"), "{diagnostic}");
    }

    #[test]
    fn auth_required_classifier_matches_http_401_shapes() {
        let err = anyhow!("MCP Streamable HTTP rejected status=401 Unauthorized");
        assert!(error_looks_auth_required(&err));

        let err = anyhow!("authentication_required for remote server");
        assert!(error_looks_auth_required(&err));

        let err = anyhow!("connection refused");
        assert!(!error_looks_auth_required(&err));

        assert!(error_text_looks_auth_required("HTTP 401 from upstream"));
        assert!(error_text_looks_auth_required("request failed (401)"));
        // The failure the classifier used to misread: a reset on a loopback
        // server whose ephemeral port contains 401 is a transport error.
        let reset = "error sending request for url (http://127.0.0.1:50401/mcp): \
                     client error (SendRequest): connection closed before message completed";
        assert!(!error_text_looks_auth_required(reset), "{reset}");
        assert!(!error_text_looks_auth_required(
            "read 14010 bytes before the stream reset"
        ));
    }

    #[test]
    fn auth_required_classifier_treats_rejected_grants_as_auth_required() {
        // A definitively rejected refresh grant is recoverable only by a
        // fresh login, so it must classify like a 401 on every surface.
        let err = anyhow!("refreshing MCP OAuth token for server wiki")
            .context("Server returned error response: invalid_grant: stale grant");
        assert!(error_looks_auth_required(&err));
        assert!(error_text_looks_auth_required(
            "wiki requires OAuth — run /mcp login wiki"
        ));
        assert!(error_text_looks_auth_required("wiki: ◆ auth required"));
        assert!(!error_text_looks_auth_required(
            "invalid_request: missing parameter"
        ));
        // rmcp's own `AuthError::AuthorizationRequired` wording: a stored
        // credential that can no longer be refreshed is a login, not a
        // transport failure.
        assert!(error_text_looks_auth_required(
            "refreshing MCP OAuth token for server wiki: OAuth authorization required"
        ));
        assert!(!error_text_looks_auth_required(
            "authorization required for the requested file"
        ));
    }

    #[test]
    fn auth_required_login_hint_names_server() {
        let hint = auth_required_login_hint("nordic-mcp");
        assert!(hint.contains("nordic-mcp"));
        assert!(hint.contains("codewhale mcp login nordic-mcp"));
        assert!(!hint.contains("/mcp auth"));
    }

    #[test]
    fn tui_reauth_hints_name_the_login_command() {
        for hint in [tui_reauth_hint(), tui_reauth_refresh_failed_hint()] {
            assert!(
                hint.contains("/mcp login <name>"),
                "OAuth recovery must name the implemented command"
            );
            assert!(
                !hint.contains("/mcp auth"),
                "OAuth recovery must not advertise a missing /mcp auth command"
            );
        }
        assert!(error_text_looks_auth_required(
            "MCP server rejected the request with 401 Unauthorized"
        ));
        assert!(!error_text_looks_auth_required("connection refused"));
    }

    #[tokio::test]
    async fn cancellable_oauth_drops_in_flight_flow_before_returning() {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let cancellation_token = CancellationToken::new();
        let cancel_from_task = cancellation_token.clone();
        let dropped = Arc::new(AtomicBool::new(false));
        let flow_dropped = Arc::clone(&dropped);
        let pending_flow = async move {
            let _guard = DropFlag(flow_dropped);
            std::future::pending::<Result<()>>().await
        };
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            cancel_from_task.cancel();
        });

        let error = run_cancellable_oauth(&cancellation_token, pending_flow)
            .await
            .expect_err("cancellation should stop the pending OAuth flow");

        assert!(error.to_string().contains("OAuth login was cancelled"));
        assert!(
            dropped.load(Ordering::SeqCst),
            "the callback-server guard must be dropped before cancellation returns"
        );
    }

    #[tokio::test]
    async fn callback_guard_aborts_accept_task_and_releases_fixed_port() -> Result<()> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let (tx, _rx) = oneshot::channel();
        let guard = CallbackServerGuard {
            accept_task: spawn_callback_server(
                listener,
                tx,
                "/callback/test".to_string(),
                InMemoryStateStore::new(),
            ),
        };

        drop(guard);

        let rebound = timeout(Duration::from_secs(1), async {
            loop {
                match TcpListener::bind(addr).await {
                    Ok(listener) => break Ok(listener),
                    Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
                        tokio::task::yield_now().await;
                    }
                    Err(err) => break Err(err),
                }
            }
        })
        .await
        .context("callback listener did not release its fixed port")??;
        drop(rebound);
        for success in [false, true] {
            let states = InMemoryStateStore::new();
            states
                .save(
                    "expected-state",
                    serde_json::from_value(serde_json::json!({
                        "pkce_verifier": "fixture-verifier",
                        "csrf_token": "expected-state",
                        "expected_issuer": "https://issuer.invalid",
                        "require_issuer": true,
                        "created_at": 0,
                        "requested_scopes": []
                    }))?,
                )
                .await?;
            let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
            let addr = listener.local_addr()?;
            let (tx, mut rx) = oneshot::channel();
            let guard = CallbackServerGuard {
                accept_task: spawn_callback_server(
                    listener,
                    tx,
                    "/callback/test".to_string(),
                    states.clone(),
                ),
            };
            for query in [
                "error=denied&state=wrong&iss=https%3A%2F%2Fissuer.invalid",
                "error=denied&state=expected-state",
                "error=denied&state=expected-state&iss=https%3A%2F%2Fother.invalid",
                "code=ok&state=expected-state&iss=https%3A%2F%2Fissuer.invalid%2F",
            ] {
                let mut stream = tokio::net::TcpStream::connect(addr).await?;
                stream
                    .write_all(
                        format!("GET /callback/test?{query} HTTP/1.1\r\nHost: localhost\r\n\r\n")
                            .as_bytes(),
                    )
                    .await?;
                let mut response = String::new();
                timeout(Duration::from_secs(1), stream.read_to_string(&mut response)).await??;
                assert!(response.starts_with("HTTP/1.1 400"));
                assert!(matches!(
                    rx.try_recv(),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                ));
                assert!(states.load("expected-state").await?.is_some());
            }
            let outcome = if success { "code=ok" } else { "error=denied" };
            let mut stream = tokio::net::TcpStream::connect(addr).await?;
            stream.write_all(format!("GET /callback/test?{outcome}&state=expected-state&iss=https%3A%2F%2Fissuer.invalid HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes()).await?;
            let result = timeout(Duration::from_secs(1), rx).await??;
            assert_eq!(matches!(result, CallbackResult::Success(_)), success);
            assert_eq!(states.load("expected-state").await?.is_some(), success);
            if success {
                states.delete("expected-state").await?;
            }
            assert!(
                !callback_binding_is_valid(
                    &states,
                    "expected-state",
                    Some("https://issuer.invalid")
                )
                .await
            );
            drop(guard);
        }
        Ok(())
    }

    async fn guarded_oauth_fixture(
        token_target: Option<String>,
        redirect_token: bool,
    ) -> (
        String,
        Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let captured = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&captured);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 2048];
                loop {
                    let n = socket.read(&mut buffer).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    if bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&bytes);
                let path = request.split_whitespace().nth(1).unwrap_or("");
                let (status, extra, body) = if path == "/.well-known/oauth-authorization-server" {
                    ("200 OK", String::new(), serde_json::json!({
                        "issuer": format!("http://{addr}"),
                        "authorization_endpoint": format!("http://{addr}/authorize"),
                        "token_endpoint": token_target.clone().unwrap_or_else(|| format!("http://{addr}/token")),
                        "registration_endpoint": format!("http://{addr}/register"),
                        "response_types_supported": ["code"]
                    }).to_string())
                } else if path == "/token" && redirect_token {
                    (
                        "307 Redirect",
                        "Location: /capture\r\n".to_string(),
                        String::new(),
                    )
                } else if path == "/token" || path == "/capture" {
                    seen.fetch_add(1, Ordering::SeqCst);
                    ("200 OK", String::new(), r#"{"access_token":"new-fixture","token_type":"Bearer","refresh_token":"fixture-refresh"}"#.to_string())
                } else if path == "/register" {
                    (
                        "200 OK",
                        String::new(),
                        r#"{"client_id":"fixture-client","redirect_uris":[]}"#.to_string(),
                    )
                } else {
                    ("404 Not Found", String::new(), String::new())
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\n{extra}Content-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (format!("http://{addr}/mcp"), captured, task)
    }

    async fn guarded_oauth_state(
        url: &str,
        network_policy: Option<&NetworkPolicyDecider>,
    ) -> AuthorizationManager {
        let client = McpHttpClient::new(
            url,
            false,
            false,
            false,
            network_policy,
            Duration::from_secs(1),
            Duration::from_secs(3),
        )
        .unwrap();
        let http = Arc::new(RecordingOAuthHttpClient::new(client));
        let stored: StoredMcpOAuthTokens = serde_json::from_value(serde_json::json!({
            "server_name":"fixture", "url":url, "client_id":"fixture-client",
            "binding":{"issuer":Url::parse(url).unwrap().origin().ascii_serialization(), "resource":Url::parse(url).unwrap().to_string()},
            "token_response":{"access_token":"fixture-access", "token_type":"Bearer", "refresh_token":"fixture-refresh"}
        })).unwrap();
        let mut unbound = stored.clone();
        unbound.binding = None;
        assert!(
            manager_from_stored_tokens(url, &unbound, &http)
                .await
                .is_err()
        );
        let mut mismatched = stored.clone();
        mismatched.binding.as_mut().unwrap().issuer.push('/');
        assert!(
            manager_from_stored_tokens(url, &mismatched, &http)
                .await
                .is_err()
        );
        mismatched = stored.clone();
        mismatched.binding.as_mut().unwrap().resource.push('/');
        assert!(
            manager_from_stored_tokens(url, &mismatched, &http)
                .await
                .is_err()
        );
        manager_from_stored_tokens(url, &stored, &http)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn guarded_oauth_refresh_honors_stop_and_preserves_normal_local_refresh() {
        use std::sync::atomic::Ordering;
        let _env = crate::test_support::lock_test_env();
        let _proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*");
        for redirect in [true, false] {
            let (url, captured, task) = guarded_oauth_fixture(None, redirect).await;
            let state = guarded_oauth_state(&url, None).await;
            let result = state.refresh_token().await;
            if redirect {
                assert!(result.is_err(), "redirected refresh must not be followed");
                assert_eq!(captured.load(Ordering::SeqCst), 0);
            } else {
                result.unwrap();
                assert_eq!(captured.load(Ordering::SeqCst), 1);
            }
            task.abort();
        }
    }

    #[tokio::test]
    async fn guarded_oauth_discovered_private_token_endpoint_never_receives_credentials() {
        let _env = crate::test_support::lock_test_env();
        let _proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*");
        let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = format!("http://{}/token", destination.local_addr().unwrap());
        let (url, _, task) = guarded_oauth_fixture(Some(target), false).await;
        let state = guarded_oauth_state(&url, None).await;
        let error = tokio::time::timeout(Duration::from_secs(1), state.refresh_token())
            .await
            .expect("the destination guard rejects before attempting a network request")
            .unwrap_err();
        // rmcp intentionally wraps HTTP client failures as `Request failed`.
        // The observable invariant is an immediate failed refresh and no socket
        // at the private destination, rather than an SDK-specific error string.
        assert!(
            matches!(error, AuthError::TokenRefreshFailed(_)),
            "{error:#}"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(30), destination.accept())
                .await
                .is_err()
        );
        task.abort();
    }

    #[tokio::test]
    async fn guarded_oauth_network_deny_applies_to_standalone_and_synthetic_login() {
        use crate::mcp::{AuthenticateToolStart, McpPool};
        use codewhale_mcp::config::McpConfig;
        use codewhale_net::network_policy::{DecisionToml, NetworkPolicy};
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let _proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server: McpServerConfig =
            serde_json::from_value(serde_json::json!({"url":url})).unwrap();
        let denied = NetworkPolicyDecider::new(
            NetworkPolicy {
                default: DecisionToml::Deny,
                ..NetworkPolicy::default()
            },
            None,
        );
        let mut config = McpConfig::default();
        config
            .servers
            .insert("network-guard".to_string(), server.clone());
        let pool = McpPool::new(config).with_network_policy(denied.clone());
        let error = match pool.begin_authenticate_tool("network-guard").await {
            Err(error) => error,
            Ok(_) => panic!("a configured denied origin must not start synthetic authentication"),
        };
        assert!(error.to_string().contains("network policy"), "{error:#}");
        assert!(oauth_login_support(&server, Some(&denied)).await.is_err());
        assert_eq!(
            auth_status_for_server("network-guard", &server, Some(&denied)).await,
            McpAuthStatus::Unsupported
        );
        assert!(
            perform_oauth_login_for_server(
                "network-guard",
                &server,
                Some(vec!["explicit".to_string()]),
                None,
                None,
                Some(&denied)
            )
            .await
            .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );

        let (url, _, task) = guarded_oauth_fixture(None, false).await;
        let server: McpServerConfig =
            serde_json::from_value(serde_json::json!({"url":url})).unwrap();
        let allowed = NetworkPolicyDecider::new(
            NetworkPolicy {
                default: DecisionToml::Allow,
                ..NetworkPolicy::default()
            },
            None,
        );
        let mut config = McpConfig::default();
        config.servers.insert("network-control".to_string(), server);
        let pool = McpPool::new(config).with_network_policy(allowed.clone());
        let AuthenticateToolStart::Login(login) = pool
            .begin_authenticate_tool("network-control")
            .await
            .unwrap()
        else {
            panic!("the configured local control must start a fresh login");
        };
        assert!(login.authorization_url().contains("/authorize"));
        // A later retry carries the same shared session ceiling.
        allowed.deny_session("127.0.0.1", "mcp");
        assert_eq!(
            login
                .network_policy
                .as_ref()
                .unwrap()
                .evaluate("127.0.0.1", "mcp"),
            codewhale_net::network_policy::Decision::Deny
        );
        drop(login);
        task.abort();
    }

    #[tokio::test]
    async fn interactive_login_forces_the_consent_screen() {
        use crate::mcp::{AuthenticateToolStart, McpPool};
        use codewhale_mcp::config::McpConfig;
        use codewhale_net::network_policy::{DecisionToml, NetworkPolicy};
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let _proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*");
        let (url, _, task) = guarded_oauth_fixture(None, false).await;
        let server: McpServerConfig =
            serde_json::from_value(serde_json::json!({"url":url})).unwrap();
        let allowed = NetworkPolicyDecider::new(
            NetworkPolicy {
                default: DecisionToml::Allow,
                ..NetworkPolicy::default()
            },
            None,
        );
        let mut config = McpConfig::default();
        config.servers.insert("consent-probe".to_string(), server);
        let pool = McpPool::new(config).with_network_policy(allowed);
        let AuthenticateToolStart::Login(login) =
            pool.begin_authenticate_tool("consent-probe").await.unwrap()
        else {
            panic!("a fresh server must start an interactive login");
        };

        // #6040: logout only clears this machine's token; the provider keeps
        // its standing grant, so the login URL must force the consent screen
        // or the same account/workspace is silently re-granted.
        // The URL carries the PKCE challenge and state, so the assertion
        // message reports only the fact that is being checked, never the URL.
        let forces_consent = login.authorization_url().contains("prompt=consent");
        assert!(
            forces_consent,
            "an interactive login must force consent (prompt=consent is missing from the authorization URL)"
        );

        drop(login);
        task.abort();
    }

    #[tokio::test]
    async fn guarded_oauth_refresh_keeps_live_session_network_denials() {
        use codewhale_net::network_policy::{DecisionToml, NetworkPolicy};
        let _env = crate::test_support::lock_test_env();
        let _proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*");
        let (url, captured, task) = guarded_oauth_fixture(None, false).await;
        let policy = NetworkPolicyDecider::new(
            NetworkPolicy {
                default: DecisionToml::Allow,
                ..NetworkPolicy::default()
            },
            None,
        );
        let state = guarded_oauth_state(&url, Some(&policy)).await;
        state.refresh_token().await.unwrap();
        assert_eq!(captured.load(Ordering::SeqCst), 1);
        policy.deny_session("127.0.0.1", "mcp");
        assert!(state.refresh_token().await.is_err());
        assert_eq!(
            captured.load(Ordering::SeqCst),
            1,
            "no refresh request after session denial"
        );
        task.abort();
    }
}
