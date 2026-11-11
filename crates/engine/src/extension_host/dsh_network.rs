use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use codewhale_secrets::Secrets;
use serde::Deserialize;
use serde_json::{Value, json};

use super::dsh_broker::{BrokerCall, BrokerFuture, BrokerMethods, BrokerRefusal, decode};
use super::protocol::DshService;
use crate::tools::spec::ToolContext;
use crate::tools::web::guard::validate_fetch_target;

pub const CREDENTIAL_HANDLE_PREFIX: &str = "codewhale-credential:";
const MAX_REF_LEN: usize = 128;
const MAX_REDIRECTS: usize = 5;
const MAX_HEADERS: usize = 64;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TIMEOUT: Duration = Duration::from_secs(120);
const AUDIT_LABEL: &str = "dsh_network";
const FORBIDDEN_HEADERS: &[&str] = &[
    "host",
    "connection",
    "content-length",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "keep-alive",
    "proxy-authorization",
    "proxy-connection",
];
const METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

#[derive(Deserialize)]
#[serde(tag = "method", content = "input", deny_unknown_fields)]
enum NetworkMethod {
    #[serde(rename = "fetch")]
    Fetch(FetchInput),
    #[serde(rename = "credentials.describe")]
    Describe(CredentialRef),
    #[serde(rename = "credentials.resolve")]
    Resolve(CredentialRef),
    #[serde(rename = "credentials.set")]
    Set(CredentialWrite),
}

impl BrokerMethods for NetworkMethod {
    const NAMES: &'static [&'static str] = &[
        "fetch",
        "credentials.describe",
        "credentials.resolve",
        "credentials.set",
    ];
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FetchInput {
    url: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    body_base64: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialRef {
    #[serde(rename = "ref")]
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialWrite {
    #[serde(rename = "ref")]
    name: String,
    value: String,
}

pub(super) struct NetworkAuthority {
    pub hosts: BTreeSet<String>,
    pub tool: Option<ToolContext>,
    pub vault: Option<Secrets>,
}

impl NetworkAuthority {
    fn withheld() -> Self {
        Self {
            hosts: BTreeSet::new(),
            tool: None,
            vault: None,
        }
    }
}

pub(super) fn serve(call: BrokerCall) -> BrokerFuture {
    serve_with(call, NetworkAuthority::withheld())
}

pub(super) fn serve_with(call: BrokerCall, authority: NetworkAuthority) -> BrokerFuture {
    Box::pin(async move {
        let method = decode::<NetworkMethod>(&call)?;
        let plugin = call.owner.plugin_id.clone();
        match method {
            NetworkMethod::Fetch(input) => {
                let Some(tool) = authority.tool.as_ref() else {
                    return Err(BrokerRefusal::NotAvailable(DshService::Network));
                };
                fetch(
                    &plugin,
                    &authority.hosts,
                    tool,
                    authority.vault.as_ref(),
                    input,
                )
                .await
            }
            NetworkMethod::Describe(CredentialRef { name }) => {
                let vault = vault(&authority)?;
                let name = checked_ref(&name)?;
                let set = read_secret(vault, &plugin, name)?.is_some();
                tracing::info!(target: "codewhale::dsh_network", plugin = %plugin, credential = %name, set, "credential described");
                Ok(json!({ "ref": name, "set": set, "handle": handle(name) }))
            }
            NetworkMethod::Resolve(CredentialRef { name }) => {
                let vault = vault(&authority)?;
                let name = checked_ref(&name)?;
                let set = read_secret(vault, &plugin, name)?.is_some();
                tracing::info!(target: "codewhale::dsh_network", plugin = %plugin, credential = %name, set, "credential resolved to its handle");
                Ok(json!({ "ref": name, "value": set.then(|| handle(name)) }))
            }
            NetworkMethod::Set(CredentialWrite { name, value }) => {
                let vault = vault(&authority)?;
                let name = checked_ref(&name)?;
                if value.is_empty() || value.contains(CREDENTIAL_HANDLE_PREFIX) {
                    return Err(invalid(
                        "a credential value must be a non-empty secret, not a handle",
                    ));
                }
                vault
                    .set(&vault_slot(&plugin, name), &value)
                    .map_err(|_| invalid("the credential vault refused the write"))?;
                tracing::info!(target: "codewhale::dsh_network", plugin = %plugin, credential = %name, "credential stored");
                Ok(json!({ "ref": name, "set": true, "handle": handle(name) }))
            }
        }
    })
}

fn invalid(message: impl Into<String>) -> BrokerRefusal {
    BrokerRefusal::InvalidInput {
        service: DshService::Network,
        message: message.into(),
    }
}

fn vault(authority: &NetworkAuthority) -> Result<&Secrets, BrokerRefusal> {
    authority
        .vault
        .as_ref()
        .ok_or(BrokerRefusal::NotAvailable(DshService::Network))
}

fn checked_ref(name: &str) -> Result<&str, BrokerRefusal> {
    let ok = !name.is_empty()
        && name.len() <= MAX_REF_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if ok {
        Ok(name)
    } else {
        Err(invalid(format!(
            "a credential ref is 1 to {MAX_REF_LEN} letters, digits, `_`, `-` or `.`"
        )))
    }
}

fn handle(name: &str) -> String {
    format!("{CREDENTIAL_HANDLE_PREFIX}{name}")
}

fn vault_slot(plugin: &str, name: &str) -> String {
    format!("dsh-plugin:{plugin}:{name}")
}

fn read_secret(vault: &Secrets, plugin: &str, name: &str) -> Result<Option<String>, BrokerRefusal> {
    vault
        .get(&vault_slot(plugin, name))
        .map(|value| value.filter(|value| !value.is_empty()))
        .map_err(|_| invalid("the credential vault could not be read"))
}

fn normalized_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

fn same_origin(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str().map(normalized_host) == b.host_str().map(normalized_host)
        && a.port_or_known_default() == b.port_or_known_default()
}

fn egress_host(url: &reqwest::Url, hosts: &BTreeSet<String>) -> Result<String, BrokerRefusal> {
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(invalid(
            "egress refused: only http and https URLs are brokered",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("egress refused: URLs may not carry userinfo"));
    }
    let host = url
        .host_str()
        .map(normalized_host)
        .ok_or_else(|| invalid("egress refused: the URL has no host"))?;
    if hosts.iter().any(|allowed| normalized_host(allowed) == host) {
        Ok(host)
    } else {
        Err(invalid(format!(
            "egress refused: `{}` is not one of this plugin's reviewed network hosts",
            host.chars().take(253).collect::<String>()
        )))
    }
}

struct Substituted {
    headers: Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)>,
    secrets: Vec<(String, String)>,
}

fn substitute_headers(
    plugin: &str,
    vault: Option<&Secrets>,
    headers: &[(String, String)],
) -> Result<Substituted, BrokerRefusal> {
    let mut out = Vec::with_capacity(headers.len());
    let mut secrets: Vec<(String, String)> = Vec::new();
    for (name, value) in headers {
        let mut resolved = String::with_capacity(value.len());
        let mut rest = value.as_str();
        while let Some(at) = rest.find(CREDENTIAL_HANDLE_PREFIX) {
            resolved.push_str(&rest[..at]);
            let tail = &rest[at + CREDENTIAL_HANDLE_PREFIX.len()..];
            let end = tail
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
                .unwrap_or(tail.len());
            let reference = checked_ref(&tail[..end])?;
            let vault = vault.ok_or(BrokerRefusal::NotAvailable(DshService::Network))?;
            let secret = read_secret(vault, plugin, reference)?.ok_or_else(|| {
                invalid(format!(
                    "credential `{reference}` is not set for this plugin"
                ))
            })?;
            resolved.push_str(&secret);
            if !secrets.iter().any(|(_, known)| *known == secret) {
                secrets.push((handle(reference), secret));
            }
            rest = &tail[end..];
        }
        resolved.push_str(rest);
        let header_name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| invalid("a request header name is not valid"))?;
        let header_value = reqwest::header::HeaderValue::from_str(&resolved).map_err(|_| {
            invalid(format!(
                "request header `{header_name}` has an invalid value"
            ))
        })?;
        out.push((header_name, header_value));
    }
    Ok(Substituted {
        headers: out,
        secrets,
    })
}

fn scrub(bytes: &[u8], secrets: &[(String, String)]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    for (handle, secret) in secrets {
        let needle = secret.as_bytes();
        if needle.is_empty() {
            continue;
        }
        let mut next = Vec::with_capacity(out.len());
        let mut i = 0;
        while i < out.len() {
            if out[i..].starts_with(needle) {
                next.extend_from_slice(handle.as_bytes());
                i += needle.len();
            } else {
                next.push(out[i]);
                i += 1;
            }
        }
        out = next;
    }
    out
}

async fn fetch(
    plugin: &str,
    hosts: &BTreeSet<String>,
    tool: &ToolContext,
    vault: Option<&Secrets>,
    input: FetchInput,
) -> Result<Value, BrokerRefusal> {
    let mut url = reqwest::Url::parse(&input.url).map_err(|_| invalid("the URL is not valid"))?;
    if input.url.contains(CREDENTIAL_HANDLE_PREFIX) {
        return Err(invalid(
            "credential handles are substituted only in request header values",
        ));
    }
    let mut method = input
        .method
        .as_deref()
        .unwrap_or("GET")
        .to_ascii_uppercase();
    if !METHODS.contains(&method.as_str()) {
        return Err(invalid(
            "the request method is not one the network broker sends",
        ));
    }
    if input.headers.len() > MAX_HEADERS
        || input
            .headers
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum::<usize>()
            > MAX_HEADER_BYTES
    {
        return Err(invalid("too many or too large request headers"));
    }
    if let Some((name, _)) = input.headers.iter().find(|(name, _)| {
        FORBIDDEN_HEADERS
            .iter()
            .any(|forbidden| name.eq_ignore_ascii_case(forbidden))
    }) {
        return Err(invalid(format!(
            "request header `{}` is set by the broker, not the plugin",
            name.chars().take(64).collect::<String>()
        )));
    }
    let mut body = match input.body_base64.as_deref() {
        Some(encoded) => {
            let bytes = STANDARD
                .decode(encoded)
                .map_err(|_| invalid("body_base64 is not valid base64"))?;
            if bytes.len() > MAX_REQUEST_BYTES {
                return Err(invalid("the request body is too large"));
            }
            Some(bytes)
        }
        None => None,
    };
    let timeout = input
        .timeout_ms
        .map_or(DEFAULT_TIMEOUT, Duration::from_millis)
        .min(MAX_TIMEOUT);
    let deadline = Instant::now() + timeout;
    let origin = url.clone();
    let host = egress_host(&url, hosts)?;
    let mut redirects = 0usize;
    let mut substituted: Option<Substituted> = None;
    let response = loop {
        egress_host(&url, hosts)?;
        let pin = validate_fetch_target(&url, tool, AUDIT_LABEL)
            .await
            .map_err(|error| invalid(format!("egress refused: {error}")))?;
        let substitution = match substituted.take() {
            Some(done) => done,
            None => substitute_headers(plugin, vault, &input.headers)?,
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(invalid("the request timed out"));
        }
        let mut builder = codewhale_net::guard::guarded_reqwest_client_builder()
            .timeout(remaining)
            .redirect(reqwest::redirect::Policy::none());
        if let Some((name, ip)) = pin {
            builder = builder.resolve(&name, std::net::SocketAddr::new(ip, 0));
        }
        let client = builder
            .build()
            .map_err(|_| invalid("the network broker could not build its client"))?;
        let verb = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| invalid("the request method is not valid"))?;
        let mut request = client.request(verb, url.clone());
        for (name, value) in &substitution.headers {
            request = request.header(name, value);
        }
        if let Some(bytes) = body.clone() {
            request = request.body(bytes);
        }
        let response = request.send().await.map_err(|error| {
            invalid(format!(
                "the request failed: {}",
                if error.is_timeout() {
                    "timed out"
                } else if error.is_connect() {
                    "could not connect"
                } else {
                    "transport error"
                }
            ))
        })?;
        substituted = Some(substitution);
        let status = response.status();
        if !status.is_redirection() {
            break response;
        }
        let Some(location) = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
        else {
            break response;
        };
        if redirects >= MAX_REDIRECTS {
            return Err(invalid("the request exceeded the five-redirect limit"));
        }
        let next = url
            .join(location)
            .map_err(|_| invalid("the redirect location is not valid"))?;
        if !same_origin(&origin, &next) {
            tracing::info!(target: "codewhale::dsh_network", plugin = %plugin, host = %host, "cross-origin redirect refused");
            return Err(invalid(
                "egress refused: a redirect may not leave the requested origin",
            ));
        }
        if status == reqwest::StatusCode::SEE_OTHER
            || (matches!(status.as_u16(), 301 | 302) && method == "POST")
        {
            method = "GET".into();
            body = None;
        }
        url = next;
        redirects += 1;
    };
    let secrets = substituted.map(|done| done.secrets).unwrap_or_default();
    let status = response.status().as_u16();
    let final_url = response.url().to_string();
    let headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            let value = String::from_utf8(scrub(value.as_bytes(), &secrets)).ok()?;
            Some((name.as_str().to_string(), value))
        })
        .collect();
    let mut response = response;
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let chunk = tokio::time::timeout(remaining, response.chunk())
            .await
            .map_err(|_| invalid("the response timed out"))?
            .map_err(|_| invalid("the response body could not be read"))?;
        let Some(chunk) = chunk else { break };
        if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(invalid(
                "the response body is larger than the broker returns",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    tracing::info!(target: "codewhale::dsh_network", plugin = %plugin, host = %host, status, redirects, "brokered fetch");
    Ok(json!({
        "status": status,
        "url": final_url,
        "redirected": redirects > 0,
        "headers": headers,
        "body_base64": STANDARD.encode(scrub(&bytes, &secrets)),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_config::OwnerRef;
    use codewhale_net::network_policy::{Decision, NetworkPolicy, NetworkPolicyDecider};
    use codewhale_secrets::InMemoryKeyringStore;
    use std::sync::Arc;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    const SECRET: &str = "sk-live-0123456789abcdef";

    fn owner(plugin: &str) -> OwnerRef {
        OwnerRef {
            plugin_id: plugin.into(),
            generation: 1,
            owner_token: "t".into(),
        }
    }

    fn call(plugin: &str, method: &str, input: Value) -> BrokerCall {
        BrokerCall {
            owner: owner(plugin),
            session_id: "s".into(),
            service: DshService::Network,
            method: method.into(),
            input,
            context: super::super::dsh_broker::BrokerContext::detached_for_test(
                &owner(plugin),
                DshService::Network,
            ),
        }
    }

    fn policy(allow: &[&str], deny: &[&str]) -> ToolContext {
        let policy = NetworkPolicy {
            default: Decision::Deny.into(),
            allow: allow.iter().map(|h| (*h).to_string()).collect(),
            deny: deny.iter().map(|h| (*h).to_string()).collect(),
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: false,
        };
        ToolContext::new(std::path::PathBuf::from("."))
            .with_network_policy(NetworkPolicyDecider::new(policy, None))
    }

    fn authority(hosts: &[&str], tool: ToolContext, vault: &Secrets) -> NetworkAuthority {
        NetworkAuthority {
            hosts: hosts.iter().map(|h| (*h).to_string()).collect(),
            tool: Some(tool),
            vault: Some(vault.clone()),
        }
    }

    fn vault() -> Secrets {
        Secrets::new(Arc::new(InMemoryKeyringStore::new()))
    }

    fn code(result: &Result<Value, BrokerRefusal>) -> &'static str {
        result.as_ref().expect_err("a refusal").code()
    }

    fn message(result: &Result<Value, BrokerRefusal>) -> String {
        format!("{:?}", result.as_ref().expect_err("a refusal"))
    }

    struct Echo;
    impl Respond for Echo {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let auth = request
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            ResponseTemplate::new(200)
                .insert_header("x-echo", auth.as_str())
                .set_body_string(format!("you sent {auth}"))
        }
    }

    #[tokio::test]
    async fn withheld_authority_refuses_typed_and_unknown_methods_are_named() {
        let unknown = serve(call("a", "connect", json!({}))).await;
        assert_eq!(code(&unknown), "unknown_method");
        let bad = serve(call("a", "fetch", json!({"url": 1}))).await;
        assert_eq!(code(&bad), "invalid_input");
        for (method, input) in [
            ("fetch", json!({"url": "https://example.com/"})),
            ("credentials.resolve", json!({"ref": "K"})),
            ("credentials.describe", json!({"ref": "K"})),
            ("credentials.set", json!({"ref": "K", "value": "v"})),
        ] {
            assert_eq!(
                code(&serve(call("a", method, input)).await),
                "not_available",
                "{method}"
            );
        }
    }

    #[tokio::test]
    async fn credentials_never_leave_rust_and_are_scoped_to_their_plugin() {
        let vault = vault();
        let tool = policy(&[], &[]);
        let set = serve_with(
            call(
                "a",
                "credentials.set",
                json!({"ref": "OPENAI_API_KEY", "value": SECRET}),
            ),
            authority(&[], tool.clone(), &vault),
        )
        .await
        .unwrap();
        assert!(!set.to_string().contains(SECRET));
        let resolved = serve_with(
            call("a", "credentials.resolve", json!({"ref": "OPENAI_API_KEY"})),
            authority(&[], tool.clone(), &vault),
        )
        .await
        .unwrap();
        assert_eq!(resolved["value"], "codewhale-credential:OPENAI_API_KEY");
        assert!(!resolved.to_string().contains(SECRET));
        let foreign = serve_with(
            call("b", "credentials.resolve", json!({"ref": "OPENAI_API_KEY"})),
            authority(&[], tool.clone(), &vault),
        )
        .await
        .unwrap();
        assert_eq!(foreign["value"], Value::Null);
        let traversal = serve_with(
            call(
                "b",
                "credentials.describe",
                json!({"ref": "../a:OPENAI_API_KEY"}),
            ),
            authority(&[], tool, &vault),
        )
        .await;
        assert_eq!(code(&traversal), "invalid_input");
    }

    #[tokio::test]
    async fn allowed_host_gets_the_secret_and_the_wire_back_never_carries_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", format!("Bearer {SECRET}").as_str()))
            .respond_with(Echo)
            .expect(1)
            .mount(&server)
            .await;
        let vault = vault();
        vault.set(&vault_slot("a", "KEY"), SECRET).unwrap();
        let result = serve_with(
            call(
                "a",
                "fetch",
                json!({
                    "url": format!("{}/v1/models", server.uri()),
                    "headers": [["authorization", "Bearer codewhale-credential:KEY"]],
                }),
            ),
            authority(&["127.0.0.1"], policy(&["127.0.0.1"], &[]), &vault),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], 200);
        let wire = result.to_string();
        assert!(!wire.contains(SECRET), "{wire}");
        let body = STANDARD
            .decode(result["body_base64"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "you sent Bearer codewhale-credential:KEY"
        );
        assert!(
            result["headers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|pair| pair[0] == "x-echo" && pair[1] == "Bearer codewhale-credential:KEY")
        );
    }

    #[tokio::test]
    async fn unreviewed_denied_and_restricted_hosts_are_refused_before_any_connection() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let vault = vault();
        let url = format!("{}/x", server.uri());
        let unreviewed = serve_with(
            call("a", "fetch", json!({"url": url})),
            authority(&["api.example.com"], policy(&["127.0.0.1"], &[]), &vault),
        )
        .await;
        assert_eq!(code(&unreviewed), "invalid_input");
        assert!(message(&unreviewed).contains("reviewed network hosts"));
        let denied = serve_with(
            call("a", "fetch", json!({"url": url})),
            authority(&["127.0.0.1"], policy(&[], &["127.0.0.1"]), &vault),
        )
        .await;
        assert!(
            message(&denied).contains("blocked by network policy"),
            "{}",
            message(&denied)
        );
        let unapproved = serve_with(
            call("a", "fetch", json!({"url": url})),
            authority(&["127.0.0.1"], policy(&[], &[]), &vault),
        )
        .await;
        assert!(message(&unapproved).contains("egress refused"));
        let metadata = serve_with(
            call(
                "a",
                "fetch",
                json!({"url": "http://169.254.169.254/latest"}),
            ),
            authority(
                &["169.254.169.254"],
                policy(&["169.254.169.254"], &[]),
                &vault,
            ),
        )
        .await;
        assert!(
            message(&metadata).contains("restricted"),
            "{}",
            message(&metadata)
        );
        let rebound = serve_with(
            call("a", "fetch", json!({"url": "http://localhost/"})),
            authority(
                &["localhost"],
                ToolContext::new(std::path::PathBuf::from(".")),
                &vault,
            ),
        )
        .await;
        assert!(
            message(&rebound).contains("localhost"),
            "{}",
            message(&rebound)
        );
        let in_url = serve_with(
            call(
                "a",
                "fetch",
                json!({"url": format!("{url}?k=codewhale-credential:KEY")}),
            ),
            authority(&["127.0.0.1"], policy(&["127.0.0.1"], &[]), &vault),
        )
        .await;
        assert_eq!(code(&in_url), "invalid_input");
        let host_header = serve_with(
            call(
                "a",
                "fetch",
                json!({"url": url, "headers": [["Host", "evil.example"]]}),
            ),
            authority(&["127.0.0.1"], policy(&["127.0.0.1"], &[]), &vault),
        )
        .await;
        assert_eq!(code(&host_header), "invalid_input");
    }

    #[tokio::test]
    async fn redirects_stay_on_origin_and_never_carry_the_secret_elsewhere() {
        let server = MockServer::start().await;
        let elsewhere = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&elsewhere)
            .await;
        Mock::given(path("/away"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/steal", elsewhere.uri()).as_str()),
            )
            .mount(&server)
            .await;
        Mock::given(path("/rename"))
            .respond_with(ResponseTemplate::new(302).insert_header(
                "location",
                server.uri().replace("127.0.0.1", "localhost").as_str(),
            ))
            .mount(&server)
            .await;
        Mock::given(path("/hop"))
            .respond_with(ResponseTemplate::new(307).insert_header("location", "/landing"))
            .mount(&server)
            .await;
        Mock::given(path("/landing"))
            .and(header("authorization", SECRET))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .expect(1)
            .mount(&server)
            .await;
        let vault = vault();
        vault.set(&vault_slot("a", "KEY"), SECRET).unwrap();
        let headers = json!([["authorization", "codewhale-credential:KEY"]]);
        let both = authority(
            &["127.0.0.1", "localhost"],
            policy(&["127.0.0.1", "localhost"], &[]),
            &vault,
        );
        let away = serve_with(
            call(
                "a",
                "fetch",
                json!({"url": format!("{}/away", server.uri()), "headers": headers}),
            ),
            both,
        )
        .await;
        assert!(message(&away).contains("may not leave the requested origin"));
        let renamed = serve_with(
            call(
                "a",
                "fetch",
                json!({"url": format!("{}/rename", server.uri()), "headers": headers}),
            ),
            authority(
                &["127.0.0.1", "localhost"],
                policy(&["127.0.0.1", "localhost"], &[]),
                &vault,
            ),
        )
        .await;
        assert!(message(&renamed).contains("may not leave the requested origin"));
        let hop = serve_with(
            call(
                "a",
                "fetch",
                json!({"url": format!("{}/hop", server.uri()), "headers": headers}),
            ),
            authority(&["127.0.0.1"], policy(&["127.0.0.1"], &[]), &vault),
        )
        .await
        .unwrap();
        assert_eq!(hop["status"], 200);
        assert_eq!(hop["redirected"], true);
    }
}
