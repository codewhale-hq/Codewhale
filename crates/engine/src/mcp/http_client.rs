//! Request-time authority for MCP transports and every OAuth HTTP operation.
//!
//! Direct public endpoints use validated DNS pins even when configured by an
//! operator. Explicit local endpoints/private-network opt-ins and selected
//! operator proxy routes carry authority only on their exact configured origin.
//! Model-added endpoints and server-selected secondary origins stay public.
//! The same client owns MCP request-time auth/header resolution for HTTP,
//! Streamable HTTP and SSE. OAuth retains the raw guarded request path.

use crate::mcp::McpServerConfigExt;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{Method, Request, Response, Url, header};

use super::headers::{apply_safe_custom_headers, with_default_mcp_http_headers};
use super::{ReviewedPluginMcpSource, oauth};
use codewhale_mcp::config::McpServerConfig;
use codewhale_net::guard::{guarded_reqwest_client_builder, is_restricted_ip};
use codewhale_net::network_policy::{Decision, NetworkPolicyDecider};

#[derive(Clone)]
pub(super) struct McpHttpAuth {
    pub(super) auth_context: Arc<dyn codewhale_runtime::credentials::AuthContext>,
    pub(super) server_name: String,
    pub(super) headers: HashMap<String, String>,
    pub(super) env_headers: HashMap<String, String>,
    pub(super) bearer_token_env_var: Option<String>,
    pub(super) oauth: Option<oauth::McpOAuthRuntime>,
    /// Whether the server's *configuration* routes authentication through
    /// OAuth, independent of whether a credential is cached yet: a URL-based
    /// server that is neither plugin-contributed nor supplied a manual
    /// bearer/Authorization credential (#6030).
    ///
    /// This is [`oauth::server_supports_oauth_login`] — the same predicate the
    /// login flow itself is gated on — so the recovery copy it selects
    /// (`/mcp login <name>`) names a command that will actually run. A live
    /// [`Self::oauth`] runtime always implies it: the runtime is only built
    /// for a server that passes this predicate. A first-run OAuth server has
    /// no runtime yet, which is exactly the case that used to fall through to
    /// the bearer-token copy.
    pub(super) oauth_configured: bool,
    pub(super) suppress_server_error_details: bool,
    pub(super) reviewed_plugin: Option<ReviewedPluginMcpSource>,
}

impl Default for McpHttpAuth {
    fn default() -> Self {
        Self {
            auth_context: super::process_mcp_auth_context(),
            server_name: String::new(),
            headers: HashMap::new(),
            env_headers: HashMap::new(),
            bearer_token_env_var: None,
            oauth: None,
            oauth_configured: false,
            suppress_server_error_details: false,
            reviewed_plugin: None,
        }
    }
}

impl McpHttpAuth {
    #[cfg(test)]
    pub(super) fn from_config(
        server_name: &str,
        config: &McpServerConfig,
        oauth: Option<oauth::McpOAuthRuntime>,
    ) -> Self {
        Self::from_config_with_auth_context(
            server_name,
            config,
            oauth,
            super::process_mcp_auth_context(),
        )
    }

    pub(super) fn from_config_with_auth_context(
        server_name: &str,
        config: &McpServerConfig,
        oauth: Option<oauth::McpOAuthRuntime>,
        auth_context: Arc<dyn codewhale_runtime::credentials::AuthContext>,
    ) -> Self {
        Self {
            auth_context,
            server_name: server_name.to_string(),
            headers: config.headers.clone(),
            env_headers: config.env_headers.clone(),
            bearer_token_env_var: config.bearer_token_env_var.clone(),
            oauth,
            oauth_configured: oauth::server_supports_oauth_login(config),
            suppress_server_error_details: config.reviewed_plugin.is_some(),
            reviewed_plugin: config.reviewed_plugin_source().cloned(),
        }
    }

    pub(super) fn server_error_preview(&self, preview: &str) -> String {
        if self.suppress_server_error_details {
            "<server details suppressed for reviewed plugin>".to_string()
        } else {
            preview.to_string()
        }
    }

    pub(super) async fn resolved_headers(&self) -> Result<HashMap<String, String>> {
        if let Some(source) = self.reviewed_plugin.as_ref() {
            source.validate_before_use(&self.server_name, "authenticate request to")?;
        }
        let mut headers = self.headers.clone();
        for (name, env_var) in &self.env_headers {
            let value = super::mcp_secret_var(
                self.reviewed_plugin.as_ref(),
                env_var,
                self.auth_context.as_ref(),
            );
            if let Some(value) = value
                && !value.trim().is_empty()
            {
                headers.insert(name.clone(), value);
            }
        }
        if !mcp_headers_have_authorization(&headers)
            && let Some(env_var) = self.bearer_token_env_var.as_deref()
            && let Some(token) = super::mcp_secret_var(
                self.reviewed_plugin.as_ref(),
                env_var,
                self.auth_context.as_ref(),
            )
        {
            let token = token.trim();
            if !token.is_empty() {
                headers.insert("Authorization".to_string(), format!("Bearer {token}"));
            }
        }
        if !mcp_headers_have_authorization(&headers)
            && let Some(oauth) = &self.oauth
        {
            let authorization = match oauth.authorization_header().await {
                Ok(authorization) => authorization,
                Err(_) if self.suppress_server_error_details => {
                    anyhow::bail!(
                        "Reviewed plugin MCP authentication failed (provider details suppressed)"
                    )
                }
                Err(error) => return Err(error),
            };
            if let Some(value) = authorization {
                headers.insert("Authorization".to_string(), value);
            }
        }
        Ok(headers)
    }
}

pub(super) fn mcp_headers_have_authorization(headers: &HashMap<String, String>) -> bool {
    headers
        .keys()
        .any(|key| key.trim().eq_ignore_ascii_case("authorization"))
}

#[derive(Clone)]
pub struct McpHttpClient {
    origin: String,
    operator_configured: bool,
    private_origin_allowed: bool,
    #[cfg(any(test, feature = "test-support"))]
    dns_answers: Arc<Mutex<Option<std::collections::VecDeque<Vec<SocketAddr>>>>>,
    reviewed_plugin: bool,
    network_policy: Option<NetworkPolicyDecider>,
    connect_timeout: Duration,
    read_timeout: Duration,
    default_headers: header::HeaderMap,
    // OAuth form bodies carry credentials independently of MCP auth headers.
    credential_bearing: bool,
    // Bound once after OAuth setup; clones keep the same request-time authority.
    // Raw OAuth execute/send deliberately do not resolve this MCP auth policy.
    mcp_auth: McpHttpAuth,
    request_builder: reqwest::Client,
    clients: Arc<Mutex<HashMap<String, reqwest::Client>>>,
}

impl McpHttpClient {
    pub fn new(
        url: &str,
        runtime_added: bool,
        reviewed_plugin: bool,
        allow_private_network: bool,
        network_policy: Option<&NetworkPolicyDecider>,
        connect_timeout: Duration,
        read_timeout: Duration,
    ) -> Result<Self> {
        let url = Url::parse(url).context("invalid MCP HTTP endpoint")?;
        validate_url(&url)?;
        validate_network_policy(&url, network_policy)?;
        if (runtime_added || reviewed_plugin) && url_has_credentials(&url) {
            bail!("MCP HTTP URL must not contain credentials; use configured headers");
        }
        Ok(Self {
            origin: url.origin().ascii_serialization(),
            operator_configured: !runtime_added,
            private_origin_allowed: !runtime_added
                && (allow_private_network || explicit_local_target(&url)),
            #[cfg(any(test, feature = "test-support"))]
            dns_answers: Arc::new(Mutex::new(None)),
            reviewed_plugin,
            network_policy: network_policy.cloned(),
            connect_timeout,
            read_timeout,
            default_headers: header::HeaderMap::new(),
            credential_bearing: false,
            mcp_auth: McpHttpAuth::default(),
            request_builder: guarded_reqwest_client_builder().build()?,
            clients: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Bind the server's request-time credentials to its guarded HTTP session.
    pub(super) fn with_mcp_auth(mut self, auth: McpHttpAuth) -> Self {
        self.mcp_auth = auth;
        self
    }

    pub(super) fn with_credential_transport(mut self) -> Result<Self> {
        validate_credential_transport(&Url::parse(&self.origin)?, true)?;
        self.credential_bearing = true;
        Ok(self)
    }

    fn validate_request_transport(&self, request: &Request) -> Result<()> {
        // All configured/custom headers are potentially sensitive, regardless
        // of their names or whether an environment value is currently present.
        // Only the transport's fixed framing headers are non-credential input.
        let credentials = self.credential_bearing
            || !self.mcp_auth.headers.is_empty()
            || !self.mcp_auth.env_headers.is_empty()
            || self.mcp_auth.bearer_token_env_var.is_some()
            || self.mcp_auth.oauth.is_some()
            || !self.default_headers.is_empty()
            || request.headers().keys().any(|name| {
                !matches!(
                    name.as_str(),
                    "accept"
                        | "content-type"
                        | "content-length"
                        | "mcp-protocol-version"
                        | "mcp-method"
                        | "mcp-name"
                )
            });
        validate_credential_transport(request.url(), credentials)
    }

    /// Prepare the MCP request's fixed framing headers and live credentials.
    /// This remains separate from send so callers retain their existing auth,
    /// header and long-lived-body cancellation/deadline boundaries. OAuth uses
    /// raw execute/send and must never inherit the MCP bearer/header pass.
    pub async fn prepare_mcp_request(
        &self,
        request: reqwest::RequestBuilder,
        json_body: bool,
    ) -> Result<reqwest::RequestBuilder> {
        self.validate_request_transport(
            &request
                .try_clone()
                .context("MCP request body cannot be replayed")?
                .build()?,
        )?;
        let modern = request
            .try_clone()
            .context("MCP request body cannot be replayed")?
            .build()?
            .headers()
            .get("mcp-protocol-version")
            .is_some_and(|value| value == super::MCP_MODERN_PROTOCOL_VERSION);
        let mut headers = self.mcp_auth.resolved_headers().await?;
        if modern {
            headers.retain(|name, _| {
                ![
                    "mcp-session-id",
                    "mcp-protocol-version",
                    "mcp-method",
                    "mcp-name",
                ]
                .iter()
                .any(|reserved| name.trim().eq_ignore_ascii_case(reserved))
            });
        }
        Ok(apply_safe_custom_headers(
            with_default_mcp_http_headers(request, json_body),
            &headers,
        ))
    }

    /// Send one exact buffered MCP request. Only an explicit 401/403 permits
    /// one OAuth refresh and resend; transport failures and cancellation never
    /// reach the retry arm. The caller's authority is checked around every await
    /// and before each write. Both native Streamable HTTP and FetchProxy use it.
    pub async fn send_mcp_request(
        &self,
        request: reqwest::RequestBuilder,
        json_body: bool,
        event_stream: bool,
        reactive_refresh: bool,
        mut validate: impl FnMut() -> Result<()> + Send,
        mut observe: impl FnMut(&Response) -> Result<()>,
    ) -> Result<Response> {
        let mut request = request;
        let mut retried = false;
        loop {
            validate()?;
            let fresh = request
                .try_clone()
                .context("MCP request body cannot be replayed")?;
            let prepared = self.prepare_mcp_request(fresh, json_body).await?;
            validate()?;
            let mut prepared = prepared.build()?;
            for name in [&reqwest::header::ACCEPT, &reqwest::header::CONTENT_TYPE] {
                let mut values = prepared.headers().get_all(name).iter();
                let first = values.next().cloned();
                if values.next().is_some()
                    && let Some(first) = first
                {
                    prepared.headers_mut().insert(name, first);
                }
            }
            if !event_stream {
                prepared.timeout_mut().get_or_insert(self.read_timeout);
            }
            let response = self
                .execute_with_guard(prepared, true, &mut validate)
                .await?;
            validate()?;
            observe(&response)?;
            let status = response.status();
            if !reactive_refresh
                || !matches!(
                    status,
                    reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
                )
            {
                return Ok(response);
            }
            if !retried && let Some(refresh) = self.refresh_mcp_oauth().await {
                validate()?;
                match refresh {
                    Ok(()) => {
                        // Streamable HTTP observes session headers even on
                        // a rejected attempt, before its one authorized retry.
                        if let Some(sid) = response.headers().get("mcp-session-id") {
                            request = request.header("mcp-session-id", sid);
                        }
                        retried = true;
                        continue;
                    }
                    Err(error) => bail!(
                        "MCP server {} rejected the request with {status} and refreshing the OAuth session failed: {error:#}. {}",
                        super::mask_url_secrets(request.build()?.url().as_str()),
                        oauth::tui_reauth_refresh_failed_hint(),
                    ),
                }
            }
            let hint = if self.oauth_configured() {
                oauth::tui_reauth_hint()
            } else {
                "Check the configured bearer token (or its environment variable)."
            };
            bail!(
                "MCP server {} rejected the request with {status}; the session is no longer accepted. {hint}",
                super::mask_url_secrets(request.build()?.url().as_str()),
            );
        }
    }

    /// Only an explicit unauthorized response lets the transport request this
    /// refresh; the session performs no replay or automatic send of its own.
    pub async fn refresh_mcp_oauth(&self) -> Option<Result<()>> {
        let oauth = self.mcp_auth.oauth.as_ref()?;
        Some(oauth.force_refresh().await)
    }

    pub fn oauth_configured(&self) -> bool {
        self.mcp_auth.oauth_configured
    }

    pub fn server_error_preview(&self, preview: &str) -> String {
        self.mcp_auth.server_error_preview(preview)
    }

    pub fn with_default_headers(mut self, headers: header::HeaderMap) -> Self {
        self.default_headers = headers;
        self
    }

    pub fn get(&self, url: &str) -> reqwest::RequestBuilder {
        self.request_builder.get(url)
    }

    pub fn post(&self, url: &str) -> reqwest::RequestBuilder {
        self.request_builder.post(url)
    }

    pub async fn send(&self, request: reqwest::RequestBuilder) -> Result<Response> {
        self.execute(request.build()?, true).await
    }

    /// Send the long-lived GET that carries a legacy SSE event stream.
    ///
    /// Response headers are still bounded by `read_timeout`, but the body is
    /// not: a reqwest request timeout also covers body streaming, so it would
    /// cut a healthy, quiet stream at `read_timeout` and silently drop every
    /// later server message. The stream ending is reported through the
    /// transport's `probe_dead`; a dead direct peer is found by TCP keepalive
    /// (set in `client_for_target`). Through an operator HTTP(S) proxy,
    /// keepalive only covers the hop to the proxy. MCP servers are not required
    /// to send heartbeats, so no idle deadline can tell a quiet stream from a
    /// dead one.
    pub async fn send_event_stream(&self, request: reqwest::RequestBuilder) -> Result<Response> {
        self.execute_with(request.build()?, true).await
    }

    pub async fn execute(&self, mut request: Request, follow_redirects: bool) -> Result<Response> {
        // Every MCP and OAuth body must finish within `read_timeout` unless the
        // caller chose its own bound. This is a per-request timeout, not a
        // client-wide one, so the event stream shares the same pooled client
        // and connection as the POSTs that go with it.
        request.timeout_mut().get_or_insert(self.read_timeout);
        self.execute_with(request, follow_redirects).await
    }

    async fn execute_with(&self, request: Request, follow_redirects: bool) -> Result<Response> {
        self.execute_with_guard(request, follow_redirects, &mut || Ok(()))
            .await
    }

    async fn execute_with_guard(
        &self,
        mut request: Request,
        follow_redirects: bool,
        validate: &mut (dyn FnMut() -> Result<()> + Send),
    ) -> Result<Response> {
        if request.url().origin().ascii_serialization() == self.origin {
            let modern = request
                .headers()
                .get("mcp-protocol-version")
                .is_some_and(|value| value == super::MCP_MODERN_PROTOCOL_VERSION);
            for (name, value) in &self.default_headers {
                if modern
                    && [
                        "mcp-session-id",
                        "mcp-protocol-version",
                        "mcp-method",
                        "mcp-name",
                    ]
                    .contains(&name.as_str())
                {
                    continue;
                }
                if !request.headers().contains_key(name) {
                    request.headers_mut().insert(name.clone(), value.clone());
                }
            }
        }
        let timeout = request.timeout().copied().unwrap_or(self.read_timeout);
        tokio::time::timeout(
            timeout,
            self.execute_inner(request, follow_redirects, validate),
        )
        .await
        .context("MCP HTTP request timed out")?
    }

    async fn execute_inner(
        &self,
        mut request: Request,
        follow_redirects: bool,
        validate: &mut (dyn FnMut() -> Result<()> + Send),
    ) -> Result<Response> {
        for redirect_count in 0..=5 {
            let url = request.url().clone();
            validate()?;
            self.validate_request_transport(&request)?;
            let client = self.client_for_target(&url).await?;
            // DNS/guarded-client selection yielded. Owner generation, exact
            // operation expiry and reviewed source must still authorize the
            // write that follows, including every guarded redirect hop.
            validate()?;
            if let Some(source) = self.mcp_auth.reviewed_plugin.as_ref() {
                source.validate_before_use(
                    &self.mcp_auth.server_name,
                    "write authenticated request to",
                )?;
            }
            // MCP and OAuth requests have buffered bodies. Keep the exact request
            // to replay only after the Location has passed the same guard.
            let next_request = request
                .try_clone()
                .context("MCP request body cannot be replayed")?;
            let response = client.execute(request).await?;
            validate()?;
            if !follow_redirects
                || !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308)
            {
                return Ok(response);
            }
            let Some(location) = response.headers().get(header::LOCATION) else {
                return Ok(response);
            };
            if redirect_count == 5 {
                bail!("MCP HTTP redirect limit exceeded");
            }
            let next_url = url.join(location.to_str().context("invalid MCP redirect Location")?)?;
            validate_url(&next_url)?;
            if url_has_credentials(&next_url) {
                bail!("MCP HTTP redirect must not contain credentials");
            }
            if url.scheme() == "https" && next_url.scheme() != "https" {
                bail!("MCP HTTP redirect would downgrade HTTPS");
            }
            request = next_request;
            if (matches!(response.status().as_u16(), 301 | 302) && request.method() == Method::POST)
                || (response.status().as_u16() == 303 && request.method() != Method::HEAD)
            {
                *request.method_mut() = Method::GET;
                *request.body_mut() = None;
                request.headers_mut().remove(header::CONTENT_TYPE);
                request.headers_mut().remove(header::CONTENT_LENGTH);
                request.headers_mut().remove(header::TRANSFER_ENCODING);
            }
            if next_url.origin() != url.origin() {
                // Custom headers can contain credentials under arbitrary names;
                // retaining just Authorization/ Cookie exclusions is insufficient.
                let mut headers = header::HeaderMap::new();
                for name in [header::ACCEPT, header::CONTENT_TYPE] {
                    if let Some(value) = request.headers().get(&name) {
                        headers.insert(name, value.clone());
                    }
                }
                *request.headers_mut() = headers;
            }
            *request.url_mut() = next_url;
        }
        unreachable!("redirect loop is bounded")
    }

    async fn client_for_target(&self, url: &Url) -> Result<reqwest::Client> {
        validate_url(url)?;
        let same_origin = url.origin().ascii_serialization() == self.origin;
        if self.reviewed_plugin && !super::reviewed_redirect_matches_origin(url, &self.origin) {
            bail!("MCP redirect leaves the reviewed plugin origin");
        }
        validate_network_policy(url, self.network_policy.as_ref())?;
        let operator_origin = self.operator_configured && same_origin;
        if !operator_origin && url_has_credentials(url) {
            bail!("MCP HTTP discovered URL must not contain credentials");
        }
        let proxy = codewhale_net::proxy::configured_mcp_proxy(
            url,
            !operator_origin || self.reviewed_plugin,
            |key| std::env::var(key),
        )?;
        // A selected operator proxy resolves its own destinations. This is
        // delegated proxy authority, never evidence of a local DNS pin.
        let pin = if (self.private_origin_allowed && same_origin) || proxy.is_some() {
            None
        } else {
            self.public_dns_pin(url).await?
        };
        // Validate DNS before reusing a client too: a new private answer revokes
        // this request. Each cached client itself remains pinned to its old public
        // address, including reconnects after a keep-alive socket expires.
        let key = format!("{}:{pin:?}", url.origin().ascii_serialization());
        if proxy.is_none()
            && let Some(client) = self
                .clients
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&key)
        {
            return Ok(client.clone());
        }
        let mut builder = guarded_reqwest_client_builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(self.connect_timeout)
            // reqwest's current default, pinned here because a long-lived SSE
            // stream relies on it to notice a peer that vanished silently.
            .tcp_keepalive(Duration::from_secs(15));
        let proxied = proxy.is_some();
        if let Some(proxy) = proxy {
            builder = builder.proxy(proxy);
        } else if let Some((host, address)) = pin {
            builder = builder.resolve(&host, address);
        }
        let client = builder
            .build()
            .context("building guarded MCP HTTP client")?;
        let mut clients = self
            .clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !proxied && clients.len() < 32 {
            clients.insert(key, client.clone());
        }
        Ok(client)
    }
}

fn validate_network_policy(url: &Url, network_policy: Option<&NetworkPolicyDecider>) -> Result<()> {
    let host = url.host_str().context("MCP URL has no host")?;
    if let Some(policy) = network_policy {
        match policy.evaluate(host, "mcp") {
            Decision::Allow => {}
            Decision::Deny => bail!("MCP HTTP destination blocked by network policy"),
            Decision::Prompt => bail!("MCP HTTP destination requires network approval"),
        }
    }
    Ok(())
}

fn validate_url(url: &Url) -> Result<()> {
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("MCP HTTP requires an http:// or https:// URL with a host");
    }
    if url_has_credentials(url) {
        bail!("MCP HTTP URL must not contain credentials; use configured headers");
    }
    Ok(())
}

fn validate_credential_transport(url: &Url, credentials: bool) -> Result<()> {
    validate_url(url)?;
    if credentials && url.scheme() != "https" && !explicit_loopback_target(url) {
        bail!("MCP HTTP credentials require HTTPS except on an explicit loopback endpoint");
    }
    Ok(())
}

fn url_has_credentials(url: &Url) -> bool {
    !url.username().is_empty() || url.password().is_some()
}

impl McpHttpClient {
    async fn public_dns_pin(&self, url: &Url) -> Result<Option<(String, SocketAddr)>> {
        let host = url.host_str().context("MCP URL has no host")?;
        let literal = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = literal.parse::<IpAddr>() {
            if is_restricted_ip(&ip) {
                bail!("MCP HTTP destination is a restricted IP address");
            }
            return Ok(None);
        }
        let port = url.port_or_known_default().context("MCP URL has no port")?;
        #[cfg(any(test, feature = "test-support"))]
        let injected = self
            .dns_answers
            .lock()
            .unwrap()
            .as_mut()
            .map(|answers| answers.pop_front().expect("DNS fixture answer available"));
        #[cfg(not(any(test, feature = "test-support")))]
        let injected: Option<Vec<SocketAddr>> = None;
        let addresses: Vec<_> = if let Some(addresses) = injected {
            addresses
        } else {
            tokio::time::timeout(self.connect_timeout, tokio::net::lookup_host((host, port)))
                .await
                .context("MCP HTTP DNS resolution timed out")?
                .context("MCP HTTP DNS resolution failed")?
                .collect()
        };
        let address = validated_public_address(&addresses)?;
        Ok(Some((host.to_string(), address)))
    }
}

fn explicit_local_target(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_end_matches('.');
    host.eq_ignore_ascii_case("localhost")
        || host.to_ascii_lowercase().ends_with(".localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| is_restricted_ip(&ip))
}

fn explicit_loopback_target(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_end_matches('.');
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn validated_public_address(addresses: &[SocketAddr]) -> Result<SocketAddr> {
    if addresses
        .iter()
        .any(|address| is_restricted_ip(&address.ip()))
    {
        bail!("MCP HTTP DNS resolved to a restricted IP address");
    }
    addresses
        .first()
        .copied()
        .context("MCP HTTP DNS resolved to no addresses")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn client(url: &str, runtime_added: bool) -> McpHttpClient {
        McpHttpClient::new(
            url,
            runtime_added,
            false,
            false,
            None,
            Duration::from_secs(1),
            Duration::from_secs(2),
        )
        .unwrap()
    }

    async fn reply_once(listener: TcpListener, response: String) -> String {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 2048];
        loop {
            let n = socket.read(&mut buffer).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buffer[..n]);
            if bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                break;
            }
        }
        socket.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn credential_transport_requires_https_or_an_explicit_loopback_endpoint() {
        for endpoint in [
            "https://example.invalid/mcp",
            "http://127.0.0.1/mcp",
            "http://127.9.8.7/mcp",
            "http://[::1]/mcp",
            "http://localhost/mcp",
            "http://localhost./mcp",
        ] {
            assert!(validate_credential_transport(&Url::parse(endpoint).unwrap(), true).is_ok());
        }
        for endpoint in [
            "http://example.invalid/mcp",
            "http://192.0.2.1/mcp",
            "http://10.0.0.1/mcp",
            "http://169.254.169.254/mcp",
            "http://service.localhost/mcp",
        ] {
            let url = Url::parse(endpoint).unwrap();
            assert!(validate_credential_transport(&url, true).is_err());
            assert!(validate_credential_transport(&url, false).is_ok());
        }
        for endpoint in [
            "https://fixture-user:fixture-password@example.invalid/mcp",
            "http://fixture-user@127.0.0.1/mcp",
        ] {
            let url = Url::parse(endpoint).unwrap();
            assert!(validate_credential_transport(&url, true).is_err());
            assert!(validate_credential_transport(&url, false).is_err());
        }
    }

    #[tokio::test]
    async fn configured_mcp_credentials_refuse_remote_http_before_header_resolution() {
        let _env = crate::test_support::lock_test_env();
        codewhale_release::tls::ensure_rustls_crypto_provider();
        let _token = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_TEST_MCP_TLS_BEARER",
            "fixture-private-value",
        );
        let _header = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_TEST_MCP_TLS_HEADER",
            "fixture-private-value",
        );
        let url = "http://mcp-guard-fixture.invalid/mcp";
        for auth in [
            McpHttpAuth {
                headers: HashMap::from([(
                    "X-Arbitrary".to_string(),
                    "fixture-private-value".to_string(),
                )]),
                ..Default::default()
            },
            McpHttpAuth {
                // Configured framing overrides are still operator-supplied input.
                headers: HashMap::from([(
                    "Accept".to_string(),
                    "fixture-private-value".to_string(),
                )]),
                ..Default::default()
            },
            McpHttpAuth {
                env_headers: HashMap::from([(
                    "X-Arbitrary".to_string(),
                    "CODEWHALE_TEST_MCP_TLS_HEADER".to_string(),
                )]),
                ..Default::default()
            },
            McpHttpAuth {
                bearer_token_env_var: Some("CODEWHALE_TEST_MCP_TLS_BEARER".to_string()),
                ..Default::default()
            },
            McpHttpAuth {
                // An absent environment value cannot turn an unsafe credential
                // configuration into an authorized transport.
                env_headers: HashMap::from([(
                    "X-Arbitrary".to_string(),
                    "CODEWHALE_TEST_MCP_TLS_ABSENT".to_string(),
                )]),
                ..Default::default()
            },
        ] {
            let session = client(url, false).with_mcp_auth(auth);
            let error = session
                .prepare_mcp_request(session.post(url).body("{}"), true)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("credentials require HTTPS"));
            assert!(!error.to_string().contains("fixture-private-value"));
            assert!(session.clients.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn remote_http_custom_credentials_never_reach_an_operator_proxy() {
        let _env = crate::test_support::lock_test_env();
        codewhale_release::tls::ensure_rustls_crypto_provider();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_url = format!("http://{}", listener.local_addr().unwrap());
        let _https_proxy = crate::test_support::EnvVarGuard::set("HTTPS_PROXY", &proxy_url);
        let _http_proxy = crate::test_support::EnvVarGuard::set("HTTP_PROXY", &proxy_url);
        let _no_proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "");
        let _lower_no_proxy = crate::test_support::EnvVarGuard::set("no_proxy", "");
        let url = "http://mcp-guard-fixture.invalid/mcp";
        let mut defaults = header::HeaderMap::new();
        defaults.insert(
            "X-Arbitrary",
            header::HeaderValue::from_static("fixture-private-value"),
        );
        let session = client(url, false).with_default_headers(defaults);
        let error = session
            .send(session.post(url).body("{}"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("credentials require HTTPS"));
        assert!(!error.to_string().contains("fixture-private-value"));
        let session = client(url, false);
        for name in ["X-Arbitrary", "Mcp-Session-Id"] {
            let error = session
                .send(
                    session
                        .post(url)
                        .header(name, "fixture-private-value")
                        .body("{}"),
                )
                .await
                .unwrap_err();
            assert!(error.to_string().contains("credentials require HTTPS"));
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );

        // The same configured public HTTP route remains usable without credentials.
        let server = tokio::spawn(reply_once(
            listener,
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok".to_string(),
        ));
        let response = session
            .send_mcp_request(
                session
                    .post(url)
                    .header("mcp-protocol-version", "2025-03-26")
                    .body("{}"),
                true,
                false,
                false,
                || Ok(()),
                |_| Ok(()),
            )
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
        let seen = server.await.unwrap().to_ascii_lowercase();
        assert!(seen.contains("accept: application/json, text/event-stream"));
        assert!(!seen.contains("fixture-private-value"));
        assert!(seen.contains("mcp-protocol-version: 2025-03-26"));
    }

    #[tokio::test]
    async fn loopback_mcp_credentials_keep_live_auth_and_protocol_framing() {
        let _env = crate::test_support::lock_test_env();
        let _no_proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*,127.0.0.1");
        codewhale_release::tls::ensure_rustls_crypto_provider();
        let _token =
            crate::test_support::EnvVarGuard::set("CODEWHALE_TEST_MCP_TLS_BEARER", "fixture-token");
        let _live =
            crate::test_support::EnvVarGuard::set("CODEWHALE_TEST_MCP_TLS_HEADER", "fixture-live");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = tokio::spawn(reply_once(
            listener,
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok".to_string(),
        ));
        let session = client(&url, false).with_mcp_auth(McpHttpAuth {
            headers: HashMap::from([("X-Arbitrary".to_string(), "fixture-static".to_string())]),
            env_headers: HashMap::from([(
                "X-Live".to_string(),
                "CODEWHALE_TEST_MCP_TLS_HEADER".to_string(),
            )]),
            bearer_token_env_var: Some("CODEWHALE_TEST_MCP_TLS_BEARER".to_string()),
            ..Default::default()
        });
        let response = session
            .send_mcp_request(
                session.post(&url).body("{}"),
                true,
                false,
                false,
                || Ok(()),
                |_| Ok(()),
            )
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
        let seen = server.await.unwrap().to_ascii_lowercase();
        assert!(seen.contains("authorization: bearer fixture-token"));
        assert!(seen.contains("x-arbitrary: fixture-static"));
        assert!(seen.contains("x-live: fixture-live"));
        assert!(seen.contains("accept: application/json, text/event-stream"));
        assert!(seen.contains("content-type: application/json"));
    }

    #[tokio::test]
    async fn oauth_form_credentials_refuse_http_redirect_before_destination_resolution() {
        let _env = crate::test_support::lock_test_env();
        let _no_proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*,127.0.0.1");
        codewhale_release::tls::ensure_rustls_crypto_provider();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let server = tokio::spawn(reply_once(listener, "HTTP/1.1 307 Redirect\r\nLocation: http://mcp-guard-fixture.invalid/token\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".to_string()));
        let session = client(&url, false).with_credential_transport().unwrap();
        *session.dns_answers.lock().unwrap() = Some(std::collections::VecDeque::from([vec![
            "93.184.216.34:80".parse().unwrap(),
        ]]));
        let error = session
            .clone()
            .execute(
                session
                    .post(&url)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body("refresh_token=fixture-private-value")
                    .build()
                    .unwrap(),
                true,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("credentials require HTTPS"));
        assert!(!error.to_string().contains("fixture-private-value"));
        assert_eq!(
            session.dns_answers.lock().unwrap().as_ref().unwrap().len(),
            1
        );
        assert!(server.await.unwrap().contains("POST /token"));
    }

    #[tokio::test]
    async fn shared_mcp_send_revalidates_after_client_selection_before_first_write() {
        let _env = crate::test_support::lock_test_env();
        let _proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let client = client(&url, false);
        let mut checks = 0;
        let error = client
            .send_mcp_request(
                client.post(&url).body("{}"),
                true,
                false,
                false,
                || {
                    checks += 1;
                    // Before/after auth, then before/after guarded client
                    // selection. The fourth check is the actual write boundary.
                    if checks == 4 {
                        bail!("fixture owner revoked after client selection");
                    }
                    Ok(())
                },
                |_| Ok(()),
            )
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("fixture owner revoked"));
        assert_eq!(checks, 4);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn shared_guard_rechecks_same_origin_redirect_before_any_second_write() {
        let _env = crate::test_support::lock_test_env();
        let _proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*,127.0.0.1");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 2048];
            assert!(socket.read(&mut buffer).await.unwrap() > 0);
            socket.write_all(b"HTTP/1.1 307 Redirect\r\nLocation: /second\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").await.unwrap();
            drop(socket);
            tokio::time::timeout(Duration::from_millis(80), listener.accept())
                .await
                .is_err()
        });
        let client = client(&url, false);
        let mut checks = 0;
        let error = client
            .execute_with_guard(
                client.post(&url).body("{}").build().unwrap(),
                true,
                &mut || {
                    checks += 1;
                    if checks == 5 {
                        bail!("fixture owner revoked at redirect write boundary");
                    }
                    Ok(())
                },
            )
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("fixture owner revoked"));
        assert_eq!(checks, 5);
        assert!(server.await.unwrap(), "revoked redirect must not connect");
    }

    #[tokio::test]
    async fn model_added_http_rejects_private_literals_and_local_dns_before_connecting() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        for host in [
            "127.0.0.1",
            "127.1",
            "2130706433",
            "0x7f000001",
            "localhost",
            "[::1]",
            "[::ffff:127.0.0.1]",
            "169.254.169.254",
            "10.0.0.1",
        ] {
            let url = format!("http://{host}:{port}/mcp");
            let client = client(&url, true);
            for method in [Method::GET, Method::POST] {
                let request = client.request_builder.request(method, &url);
                let error = client.send(request).await.unwrap_err();
                assert!(
                    format!("{error:#}").contains("restricted"),
                    "{host}: {error:#}"
                );
            }
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }

    #[test]
    fn mixed_dns_answers_and_empty_resolution_fail_closed() {
        let public = "8.8.8.8:443".parse().unwrap();
        for private in [
            "127.0.0.1:443",
            "10.0.0.2:443",
            "169.254.169.254:443",
            "[fc00::1]:443",
        ] {
            let private = private.parse().unwrap();
            assert!(validated_public_address(&[public, private]).is_err());
            assert!(validated_public_address(&[private, public]).is_err());
        }
        assert!(validated_public_address(&[]).is_err());
        assert_eq!(validated_public_address(&[public]).unwrap(), public);
    }

    #[tokio::test]
    async fn operator_origin_remains_usable_but_does_not_authorize_private_redirects() {
        let _env = crate::test_support::lock_test_env();
        let _proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*,127.0.0.1");
        let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
        for status in [301, 302, 303, 307, 308] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/mcp", listener.local_addr().unwrap());
            let response = format!(
                "HTTP/1.1 {status} Redirect\r\nLocation: http://{}/private\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                destination.local_addr().unwrap()
            );
            let server = tokio::spawn(reply_once(listener, response));
            let client = client(&url, false);
            let error = client
                .send(
                    client
                        .post(&url)
                        .header("Authorization", "Bearer fixture")
                        .body("{}"),
                )
                .await
                .unwrap_err();
            assert!(format!("{error:#}").contains("restricted"), "{error:#}");
            assert!(server.await.unwrap().contains("Bearer fixture"));
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(30), destination.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn redirect_stop_returns_response_without_following_even_same_origin() {
        let _env = crate::test_support::lock_test_env();
        let _proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*,127.0.0.1");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/token");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let read = socket.read(&mut buf).await.unwrap();
            assert!(read > 0, "fixture request must contain bytes");
            socket.write_all(b"HTTP/1.1 307 Redirect\r\nLocation: /capture\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").await.unwrap();
            drop(socket);
            tokio::time::timeout(Duration::from_millis(80), listener.accept())
                .await
                .is_err()
        });
        let client = client(&url, false);
        let response = client
            .execute(
                client.post(&url).body("code=fixture").build().unwrap(),
                false,
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 307);
        assert!(server.await.unwrap());
    }

    #[tokio::test]
    async fn configured_local_same_origin_redirect_and_connection_reuse_work() {
        let _env = crate::test_support::lock_test_env();
        let _proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "127.0.0.1");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let read = socket.read(&mut buf).await.unwrap();
            assert!(read > 0, "fixture request must contain bytes");
            socket
                .write_all(
                    b"HTTP/1.1 307 Redirect\r\nLocation: /mcp/v2\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .unwrap();
            let size = socket.read(&mut buf).await.unwrap();
            assert!(String::from_utf8_lossy(&buf[..size]).starts_with("GET /mcp/v2 "));
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok")
                .await
                .unwrap();
        });
        let client = client(&url, false);
        let response = client.send(client.get(&url)).await.unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn model_added_configuration_marker_cannot_be_spoofed_or_lost_on_clone() {
        let config: super::super::McpServerConfig = serde_json::from_value(serde_json::json!({
            "url":"http://127.0.0.1:1/mcp", "runtime_added": false, "allow_private_network": true
        }))
        .unwrap();
        let pool = super::super::McpPool::new(super::super::McpConfig::default());
        pool.add_runtime_server_config("dynamic".to_string(), config)
            .unwrap();
        let config = pool.dynamic_servers.read().get("dynamic").unwrap().clone();
        assert!(config.runtime_added);
        assert!(config.allow_private_network);
        let client = McpHttpClient::new(
            config.url.as_deref().unwrap(),
            config.runtime_added,
            false,
            config.allow_private_network,
            None,
            Duration::from_secs(1),
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(
            client
                .client_for_target(&Url::parse(config.url.as_deref().unwrap()).unwrap())
                .await
                .is_err()
        );
        assert!(
            serde_json::to_value(&config)
                .unwrap()
                .get("runtime_added")
                .is_none()
        );
    }

    #[tokio::test]
    async fn model_added_endpoint_cannot_use_ambient_proxy_but_operator_can() {
        let _env = crate::test_support::lock_test_env();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_url = format!("http://{}", listener.local_addr().unwrap());
        let _https_proxy = crate::test_support::EnvVarGuard::set("HTTPS_PROXY", &proxy_url);
        let _http_proxy = crate::test_support::EnvVarGuard::set("HTTP_PROXY", &proxy_url);
        let _no_proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "");
        let _lower_no_proxy = crate::test_support::EnvVarGuard::set("no_proxy", "");
        let url = "http://mcp-guard-fixture.invalid/mcp";
        let strict = client(url, true);
        assert!(strict.send(strict.get(url)).await.is_err());
        // This documentation-only address passes the public-IP classifier. A
        // mistakenly enabled proxy would receive it without any DNS lookup.
        let public_literal = "http://192.0.2.1:9/mcp";
        let strict_literal = client(public_literal, true);
        assert!(
            strict_literal
                .send(strict_literal.get(public_literal))
                .await
                .is_err()
        );
        // A mistakenly enabled proxy would connect immediately; the generous
        // window only absorbs full-suite scheduler load.
        assert!(
            tokio::time::timeout(Duration::from_millis(500), listener.accept())
                .await
                .is_err()
        );
        let server = tokio::spawn(reply_once(
            listener,
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok".to_string(),
        ));
        // This path proves proxy routing, not a two-second scheduling bound.
        let configured = McpHttpClient::new(
            url,
            false,
            false,
            false,
            None,
            Duration::from_secs(5),
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(
            configured
                .send(configured.get(url))
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "ok"
        );
        assert!(
            server
                .await
                .unwrap()
                .starts_with("GET http://mcp-guard-fixture.invalid/mcp ")
        );
    }

    #[tokio::test]
    async fn configured_public_origin_rejects_rebinding_before_reusing_its_pinned_client() {
        let _env = crate::test_support::lock_test_env();
        let _proxy = crate::test_support::EnvVarGuard::set("HTTPS_PROXY", "http://127.0.0.1:9");
        let _no_proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "mcp-guard-fixture.test");
        let url = Url::parse("https://mcp-guard-fixture.test/mcp").unwrap();
        let configured = client(url.as_str(), false);
        *configured.dns_answers.lock().unwrap() = Some(std::collections::VecDeque::from([
            vec!["8.8.8.8:443".parse().unwrap()],
            vec!["127.0.0.1:443".parse().unwrap()],
        ]));
        configured.client_for_target(&url).await.unwrap();
        assert_eq!(configured.clients.lock().unwrap().len(), 1);
        let error = configured.client_for_target(&url).await.unwrap_err();
        assert!(error.to_string().contains("restricted"), "{error:#}");
        assert!(
            configured
                .dns_answers
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn private_dns_requires_an_explicit_operator_opt_in() {
        let _env = crate::test_support::lock_test_env();
        let _proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*");
        let url = Url::parse("https://internal-service.example.test/mcp").unwrap();
        let configured = client(url.as_str(), false);
        *configured.dns_answers.lock().unwrap() = Some(std::collections::VecDeque::from([vec![
            "10.0.0.3:443".parse().unwrap(),
        ]]));
        assert!(configured.client_for_target(&url).await.is_err());
        let approved = McpHttpClient::new(
            url.as_str(),
            false,
            false,
            true,
            None,
            Duration::from_secs(1),
            Duration::from_secs(2),
        )
        .unwrap();
        approved.client_for_target(&url).await.unwrap();
    }

    #[tokio::test]
    async fn event_stream_shares_the_client_and_other_bodies_keep_the_read_deadline() {
        let _env = crate::test_support::lock_test_env();
        let _no_proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "127.0.0.1");
        codewhale_release::tls::ensure_rustls_crypto_provider();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        // Every response promises 10 body bytes, sends 3 and then stalls.
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut seen = Vec::new();
                    let mut buffer = [0u8; 2048];
                    while !seen.windows(4).any(|part| part == b"\r\n\r\n") {
                        match socket.read(&mut buffer).await {
                            Ok(n) if n > 0 => seen.extend_from_slice(&buffer[..n]),
                            _ => return,
                        }
                    }
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc")
                        .await;
                    std::future::pending::<()>().await;
                });
            }
        });
        let client = McpHttpClient::new(
            &url,
            false,
            false,
            false,
            None,
            Duration::from_secs(5),
            Duration::from_millis(300),
        )
        .unwrap();
        let stream = client.send_event_stream(client.get(&url)).await.unwrap();
        let response = client.send(client.post(&url)).await.unwrap();
        let body = tokio::time::timeout(Duration::from_secs(5), response.bytes())
            .await
            .expect("a stalled body must hit read_timeout, not hang");
        assert!(body.is_err());
        // The event stream and its POSTs share one pooled client.
        assert_eq!(client.clients.lock().unwrap().len(), 1);
        drop(stream);
    }

    #[tokio::test]
    async fn mcp_session_clones_resolve_current_credentials_and_keep_framing() {
        let _env = crate::test_support::lock_test_env();
        codewhale_release::tls::ensure_rustls_crypto_provider();
        let _first_token = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_TEST_MCP_SESSION_BEARER",
            "first-fixture",
        );
        let _first_header = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_TEST_MCP_SESSION_HEADER",
            "first-header",
        );
        let url = "https://example.invalid/mcp";
        let session = client(url, false).with_mcp_auth(McpHttpAuth {
            headers: HashMap::from([
                ("Accept".to_string(), "incorrect-accept".to_string()),
                ("Content-Type".to_string(), "incorrect-type".to_string()),
                ("X-Live".to_string(), "static-header".to_string()),
                ("X-Unsafe".to_string(), "fixture\r\ninjected".to_string()),
            ]),
            env_headers: HashMap::from([(
                "X-Live".to_string(),
                "CODEWHALE_TEST_MCP_SESSION_HEADER".to_string(),
            )]),
            bearer_token_env_var: Some("CODEWHALE_TEST_MCP_SESSION_BEARER".to_string()),
            ..Default::default()
        });
        let cloned = session.clone();
        let first = session
            .prepare_mcp_request(
                session
                    .post(url)
                    .header("Mcp-Session-Id", "session-fixture"),
                true,
            )
            .await
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            first.headers().get(header::AUTHORIZATION).unwrap(),
            "Bearer first-fixture"
        );
        assert_eq!(first.headers().get("X-Live").unwrap(), "first-header");
        assert_eq!(
            first.headers().get(header::ACCEPT).unwrap(),
            super::super::headers::MCP_HTTP_ACCEPT
        );
        assert_eq!(
            first.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(
            first.headers().get("Mcp-Session-Id").unwrap(),
            "session-fixture"
        );
        assert!(!first.headers().contains_key("X-Unsafe"));

        let _next_token = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_TEST_MCP_SESSION_BEARER",
            "second-fixture",
        );
        let _next_header = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_TEST_MCP_SESSION_HEADER",
            "second-header",
        );
        let next = cloned
            .prepare_mcp_request(cloned.get(url), false)
            .await
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            next.headers().get(header::AUTHORIZATION).unwrap(),
            "Bearer second-fixture"
        );
        assert_eq!(next.headers().get("X-Live").unwrap(), "second-header");
        assert_eq!(
            next.headers().get(header::ACCEPT).unwrap(),
            super::super::headers::MCP_HTTP_ACCEPT
        );
        assert!(!next.headers().contains_key(header::CONTENT_TYPE));
        assert!(!next.headers().contains_key("X-Unsafe"));
    }

    #[tokio::test]
    async fn raw_oauth_execute_does_not_inherit_bound_mcp_credentials() {
        let _env = crate::test_support::lock_test_env();
        let _no_proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*,127.0.0.1");
        codewhale_release::tls::ensure_rustls_crypto_provider();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let url = format!("{origin}/mcp");
        let server = tokio::spawn(async move {
            let mut seen = Vec::new();
            for _ in 0..2 {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(5), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 2048];
                while !bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                    let n = socket.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                }
                seen.push(String::from_utf8(bytes).unwrap().to_ascii_lowercase());
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok",
                    )
                    .await
                    .unwrap();
            }
            seen
        });
        let session = client(&url, false).with_mcp_auth(McpHttpAuth {
            headers: HashMap::from([
                (
                    "Authorization".to_string(),
                    "Bearer mcp-only-fixture".to_string(),
                ),
                (
                    "X-Mcp-Credential".to_string(),
                    "mcp-custom-fixture".to_string(),
                ),
            ]),
            ..Default::default()
        });
        let request = session
            .prepare_mcp_request(session.post(&url), true)
            .await
            .unwrap();
        assert_eq!(
            session
                .send(request.body("{}"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "ok"
        );
        let token_url = format!("{origin}/token");
        let oauth_request = session
            .clone()
            .post(&token_url)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body("code=fixture")
            .build()
            .unwrap();
        assert_eq!(
            session
                .execute(oauth_request, false)
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "ok"
        );
        let seen = server.await.unwrap();
        assert!(seen[0].contains("authorization: bearer mcp-only-fixture"));
        assert!(seen[0].contains("x-mcp-credential: mcp-custom-fixture"));
        assert!(!seen[1].contains("mcp-only-fixture"));
        assert!(!seen[1].contains("mcp-custom-fixture"));
        assert!(!seen[1].contains("authorization:"));
        assert!(!seen[1].contains("accept: application/json, text/event-stream"));
        assert!(seen[1].contains("content-type: application/x-www-form-urlencoded"));
    }
}
