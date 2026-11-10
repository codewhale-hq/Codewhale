use anyhow::Result;
use std::io::ErrorKind;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How long the probe waits between checks once one finishes.
const LOOPBACK_PROXY_PROBE_INTERVAL: Duration = Duration::from_secs(2);
/// Upper bound for one loopback TCP probe. Windows only reports a refused
/// loopback connection after roughly two seconds, so a shorter bound would
/// turn every closed port into an inconclusive timeout and the fallback would
/// never engage.
const LOOPBACK_PROXY_PROBE_TIMEOUT: Duration = Duration::from_secs(4);
/// Keeps reqwest's snapshot behaviour when set to `0`.
const LOOPBACK_PROXY_FALLBACK_ENV: &str = "CODEWHALE_LOCAL_PROXY_FALLBACK";

/// Redact userinfo and omit paths, queries, and fragments from proxy log labels.
fn redact_proxy_userinfo(proxy_url: &str) -> String {
    let Some(scheme_end) = proxy_url.find("://") else {
        return "<invalid proxy URL>".to_string();
    };
    let after_scheme = scheme_end + 3;
    let rest = &proxy_url[after_scheme..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if let Some((_, host)) = authority.rsplit_once('@') {
        format!("{}***@{host}", &proxy_url[..after_scheme])
    } else {
        format!("{}{authority}", &proxy_url[..after_scheme])
    }
}

/// Install the loopback proxy fallback on a client builder.
///
/// `no_proxy()` clears every proxy registered before it, so it must come first:
/// afterwards the fallback route is the only proxy source, and a request the
/// callback declines cannot fall back to the dead address reqwest reads when the
/// client is built.
pub fn install_loopback_proxy(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    install_proxy_route(builder, loopback_proxy_fallback())
}

/// `no_proxy()` clears every proxy registered before it, so the route must be
/// installed after it. Getting this backwards leaves a builder with no proxies
/// at all, which looks like the fix works until the proxy is up again.
fn install_proxy_route(
    builder: reqwest::ClientBuilder,
    proxy: Option<reqwest::Proxy>,
) -> reqwest::ClientBuilder {
    let Some(proxy) = proxy else {
        return builder;
    };
    builder.no_proxy().proxy(proxy)
}

/// Route through the ambient loopback proxy while it answers and go direct
/// while its port refuses connections.
///
/// A long-lived session builds its reqwest clients once, and reqwest snapshots
/// `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` when the client is built. A local
/// proxy process (Clash, v2rayN, sing-box) that closes after startup would then
/// keep every request dialing a dead loopback port until the process restarts —
/// including providers that need no proxy at all. Return a proxy that is
/// re-probed in the background and bypasses the proxy while it is down.
///
/// The proxy environment is read once, on the first client this process builds;
/// changing `HTTPS_PROXY` afterwards needs a restart. That matches how reqwest
/// already snapshots the environment at build time, and it keeps one probe
/// thread for the process instead of one per client.
///
/// Each configured proxy is watched on its own port, so a live `HTTP_PROXY` does
/// not keep a dead `HTTPS_PROXY` in the path.
///
/// Only a definite `ECONNREFUSED` moves routing to direct. A probe that times
/// out, hits an fd limit or cannot resolve says nothing about the proxy, so the
/// previous state stands: routing the session direct on that evidence would put
/// traffic on a network path the operator deliberately avoided.
///
/// Known limitations: only loopback proxies are watched, and reachability is a
/// TCP connect rather than a proxy handshake, so a process that accepts the
/// port without forwarding traffic still counts as up. An unreachable remote
/// proxy is a network outage to report, not a boundary to route around, so
/// those keep reqwest's own behaviour.
///
/// Set `CODEWHALE_LOCAL_PROXY_FALLBACK=0` to keep reqwest's snapshot behaviour.
///
/// Returns `None` when no ambient proxy is configured, when none of them is
/// loopback, when no configured proxy is parseable, or when the fallback is
/// disabled; callers then leave the client builder untouched. An unparseable
/// variable next to a usable one is ignored rather than fatal. `new()` also
/// declines a request whose CGI environment marks the variables as request data,
/// and declines any configuration `NO_PROXY` disables outright.
pub fn loopback_proxy_fallback() -> Option<reqwest::Proxy> {
    let watch = ambient_loopback_proxy()?;
    Some(reqwest::Proxy::custom(move |url| watch.route(url)))
}

/// One probe thread per process, shared by every client built afterwards, so a
/// client rebuilt on a route change does not add another thread or another
/// connect every two seconds. The environment is consequently read once.
fn ambient_loopback_proxy() -> Option<Arc<LoopbackProxy>> {
    static WATCH: OnceLock<Option<Arc<LoopbackProxy>>> = OnceLock::new();
    WATCH
        .get_or_init(|| {
            let watch = LoopbackProxy::from_environment()?;
            spawn_probe(&watch);
            Some(watch)
        })
        .clone()
}

/// Ambient proxy routing plus the reachability of every loopback proxy target
/// it can select.
struct LoopbackProxy {
    matcher: hyper_util::client::proxy::matcher::Matcher,
    all: Option<reqwest::Url>,
    http: Option<reqwest::Url>,
    https: Option<reqwest::Url>,
    targets: Vec<LoopbackTarget>,
}

struct LoopbackTarget {
    host: String,
    port: u16,
    /// The configured URL that selects this target, kept for the one warning
    /// that names the proxy that stopped answering.
    configured: String,
    /// Starts false: no probe has said this port is gone yet, so requests keep
    /// reqwest's behaviour. Only a definite refusal sets it.
    refused: AtomicBool,
}

enum Reachability {
    /// At least one address accepted the connection.
    Open,
    /// Every address actively refused it: nothing is listening.
    Refused,
    /// Timeout, fd or resolver trouble. Not evidence about the proxy.
    Unknown,
}

impl LoopbackProxy {
    fn from_environment() -> Option<Arc<Self>> {
        // A CGI request's HTTP_PROXY is request data, not proxy configuration.
        if std::env::var_os("REQUEST_METHOD").is_some() {
            return None;
        }
        if std::env::var(LOOPBACK_PROXY_FALLBACK_ENV).is_ok_and(|value| value.trim() == "0") {
            return None;
        }
        let all = ambient_proxy_url(&["ALL_PROXY", "all_proxy"]);
        let http = ambient_proxy_url(&["HTTP_PROXY", "http_proxy"]);
        let https = ambient_proxy_url(&["HTTPS_PROXY", "https_proxy"]);
        Self::new(all, http, https, &read_no_proxy())
    }

    /// Build the router for an explicit proxy configuration. `None` when there
    /// is nothing to watch, when none of the configured proxies is loopback,
    /// when a loopback proxy's port cannot be determined, or when `NO_PROXY`
    /// disables proxying altogether. A remote proxy next to a loopback one is
    /// simply never watched.
    fn new(
        all: Option<reqwest::Url>,
        http: Option<reqwest::Url>,
        https: Option<reqwest::Url>,
        no_proxy: &str,
    ) -> Option<Arc<Self>> {
        let configured: Vec<&reqwest::Url> = [all.as_ref(), http.as_ref(), https.as_ref()]
            .into_iter()
            .flatten()
            .collect();
        if configured.is_empty() {
            return None;
        }
        // Watch only the loopback proxies. A remote proxy in the same
        // configuration keeps reqwest's behaviour: it is never marked down, so
        // traffic to it never diverts, and a local proxy next to it is still
        // watched on its own.
        let mut targets: Vec<LoopbackTarget> = Vec::new();
        for url in configured {
            let Some((host, port)) = loopback_target(url) else {
                // A loopback proxy whose port cannot be determined is a
                // misconfiguration, not a reason to quietly stop watching it.
                if is_loopback_url(url) {
                    tracing::warn!(
                        target: "net",
                        proxy = %redact_proxy_userinfo(url.as_str()),
                        "cannot determine this local proxy's port; leaving proxy routing to reqwest"
                    );
                    return None;
                }
                continue;
            };
            let known = targets
                .iter()
                .any(|target| target.host == host && target.port == port);
            if !known {
                targets.push(LoopbackTarget {
                    host,
                    port,
                    configured: url.to_string(),
                    refused: AtomicBool::new(false),
                });
            }
        }
        if targets.is_empty() {
            return None;
        }
        if no_proxy_disables_all(no_proxy) {
            return None;
        }
        let mut builder = hyper_util::client::proxy::matcher::Matcher::builder();
        if let Some(url) = &all {
            builder = builder.all(url.as_str());
        }
        if let Some(url) = &http {
            builder = builder.http(url.as_str());
        }
        if let Some(url) = &https {
            builder = builder.https(url.as_str());
        }
        let matcher = builder.no(normalize_no_proxy(no_proxy)).build();

        Some(Arc::new(Self {
            matcher,
            all,
            http,
            https,
            targets,
        }))
    }

    fn route(&self, url: &reqwest::Url) -> Option<reqwest::Url> {
        // Only ask the matcher whether this destination is proxied (scheme plus
        // NO_PROXY). The route itself comes from the configured URL so proxy
        // credentials survive; the matcher strips userinfo into an auth header.
        let port = url
            .port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default();
        let destination: http::Uri = format!("{}://{}{}", url.scheme(), url.host_str()?, port)
            .parse()
            .ok()?;
        self.matcher.intercept(&destination)?;
        let source = match url.scheme() {
            "http" => self.http.as_ref().or(self.all.as_ref()),
            "https" => self.https.as_ref().or(self.all.as_ref()),
            _ => None,
        }?;
        // Reachability is per proxy, not per watcher: with HTTP_PROXY up and
        // HTTPS_PROXY down, only the https route may go direct. Every watched
        // target is registered from the same URL the matcher selects, so an
        // unregistered source is a remote proxy, which is never watched.
        match self.target_for(source) {
            Some(target) if target.refused.load(Ordering::Relaxed) => return None,
            Some(_) | None => {}
        }
        let mut target = source.clone();
        // Remote DNS stays with the proxy, exactly as configured_mcp_proxy
        // keeps it for the same proxy URL.
        let scheme = match target.scheme() {
            "socks5" => Some("socks5h"),
            "socks4" => Some("socks4a"),
            _ => None,
        };
        if let Some(scheme) = scheme {
            target.set_scheme(scheme).ok()?;
        }
        Some(target)
    }

    /// The watched target behind a configured proxy URL, matched on the same
    /// host and port the probe uses.
    fn target_for(&self, source: &reqwest::Url) -> Option<&LoopbackTarget> {
        let (host, port) = loopback_target(source)?;
        self.targets
            .iter()
            .find(|target| target.host == host && target.port == port)
    }

    fn refresh(&self) {
        for target in &self.targets {
            let reachability = probe_target(target);
            let was_refused = target.refused.load(Ordering::Relaxed);
            let refused = match reachability {
                // A timeout or an fd limit says nothing about the proxy, so the
                // last definite answer stands.
                Reachability::Unknown => was_refused,
                Reachability::Open => false,
                Reachability::Refused => true,
            };
            if was_refused == refused {
                continue;
            }
            target.refused.store(refused, Ordering::Relaxed);
            if refused {
                tracing::warn!(
                    target: "net",
                    proxy = %redact_proxy_userinfo(&target.configured),
                    "local proxy refused connections; requests go direct until it returns"
                );
            } else {
                tracing::info!(
                    target: "net",
                    proxy = %redact_proxy_userinfo(&target.configured),
                    "local proxy is reachable again; resuming proxy routing"
                );
            }
        }
    }
}

fn spawn_probe(watch: &Arc<LoopbackProxy>) {
    // A Weak probe handle keeps the thread from owning the router. The process
    // keeps one watch in a static, so this normally never expires; a probe that
    // stops instead of pinning a watcher is the safer default if that changes.
    let weak = Arc::downgrade(watch);
    let spawned = std::thread::Builder::new()
        .name("codewhale-proxy-probe".to_string())
        .spawn(move || {
            loop {
                // Probe first, then rest: one check may take seconds, and
                // sleeping between checks would put two probes back to back.
                let Some(watch) = weak.upgrade() else {
                    return;
                };
                watch.refresh();
                drop(watch);
                std::thread::sleep(LOOPBACK_PROXY_PROBE_INTERVAL);
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(
            target: "net",
            %error,
            "could not start the local proxy probe thread"
        );
    }
}

fn probe_target(target: &LoopbackTarget) -> Reachability {
    let Ok(addresses) = (target.host.as_str(), target.port).to_socket_addrs() else {
        return Reachability::Unknown;
    };
    // Only a refusal from every resolved address is evidence the port is gone.
    // `localhost` resolving to `::1` (refused) and `127.0.0.1` (dropped) is the
    // shape of "cannot tell", not the shape of "nothing is listening".
    let mut resolved = 0_u32;
    let mut refusals = 0_u32;
    for address in addresses {
        resolved += 1;
        match TcpStream::connect_timeout(&address, LOOPBACK_PROXY_PROBE_TIMEOUT) {
            Ok(_) => return Reachability::Open,
            Err(error) if error.kind() == ErrorKind::ConnectionRefused => refusals += 1,
            Err(_) => {}
        }
    }
    if resolved > 0 && refusals == resolved {
        Reachability::Refused
    } else {
        Reachability::Unknown
    }
}

/// `NO_PROXY` entries separated by whitespace mean the same as comma-separated
/// ones, and every Rust proxy path in this process reads the list this way so a
/// value cannot mean "direct" to one subsystem and "proxied" to another. Child
/// processes still inherit the variable verbatim.
fn normalize_no_proxy(raw: &str) -> String {
    raw.replace(char::is_whitespace, ",")
}

fn no_proxy_disables_all(no_proxy: &str) -> bool {
    normalize_no_proxy(no_proxy)
        .split(',')
        .any(|entry| entry == "*")
}

fn read_no_proxy() -> String {
    std::env::var("NO_PROXY")
        .or_else(|_| std::env::var("no_proxy"))
        .unwrap_or_default()
}

fn ambient_proxy_url(names: &[&str]) -> Option<reqwest::Url> {
    let raw = names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })?;
    reqwest::Url::parse(&raw)
        .ok()
        .filter(|url| url.has_host())
        .or_else(|| {
            reqwest::Url::parse(&format!("http://{raw}"))
                .ok()
                .filter(|url| url.has_host())
        })
}

/// The bare loopback address behind a proxy URL's host, or `None` when it is
/// not loopback. `Url::host_str` returns an IPv6 literal with its brackets
/// (`[::1]`), so the brackets come off before the address is parsed and before
/// the host is stored for a probe.
fn loopback_host(url: &reqwest::Url) -> Option<String> {
    let host = url.host_str()?;
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    if unbracketed.eq_ignore_ascii_case("localhost") {
        return Some(unbracketed.to_string());
    }
    unbracketed
        .parse::<std::net::IpAddr>()
        .ok()
        .filter(is_loopback_address)
        .map(|address| address.to_string())
}

/// `Ipv6Addr::is_loopback` only accepts `::1`, so an IPv4-mapped local proxy
/// (`[::ffff:127.0.0.1]`) is unwrapped before the check. It is the same host
/// with a different spelling, and a proxy bound that way is still local.
fn is_loopback_address(address: &std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(v4) => v4.is_loopback(),
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

fn is_loopback_url(url: &reqwest::Url) -> bool {
    loopback_host(url).is_some()
}

fn loopback_target(url: &reqwest::Url) -> Option<(String, u16)> {
    let host = loopback_host(url)?;
    // reqwest defaults a portless socks proxy to 1080; matching that here keeps
    // `ALL_PROXY=socks5://127.0.0.1` watched instead of silently unwatched.
    let port = url.port_or_known_default().or(match url.scheme() {
        "socks4" | "socks4a" | "socks5" | "socks5h" => Some(1080),
        _ => None,
    })?;
    Some((host, port))
}

/// Resolve the operator's proxy route for this exact request using the same
/// matcher as reqwest. A NO_PROXY match returns None: direct requests must keep
/// their public DNS validation and pins. Model and reviewed-plugin requests
/// return before even reading proxy credentials.
pub fn configured_mcp_proxy<F>(
    url: &reqwest::Url,
    disallow_ambient_proxy: bool,
    mut read_environment: F,
) -> Result<Option<reqwest::Proxy>>
where
    F: FnMut(&str) -> std::result::Result<String, std::env::VarError>,
{
    if disallow_ambient_proxy {
        return Ok(None);
    }
    let proxy_url = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]
        .into_iter()
        .find_map(|key| {
            read_environment(key)
                .ok()
                .filter(|value| !value.trim().is_empty())
        });
    let Some(proxy_url) = proxy_url else {
        return Ok(None);
    };
    // Normalize userinfo and Unicode with the URL parser before passing the
    // URL to reqwest's own underlying matcher. Keep its missing-scheme support.
    let normalized = reqwest::Url::parse(&proxy_url)
        .ok()
        .filter(|url| url.has_host())
        .or_else(|| reqwest::Url::parse(&format!("http://{proxy_url}")).ok());
    let Some(normalized) = normalized else {
        tracing::warn!(target: "mcp", "ignoring malformed HTTP(S)_PROXY URL");
        return Ok(None);
    };
    let no_proxy = normalize_no_proxy(
        &read_environment("NO_PROXY")
            .or_else(|_| read_environment("no_proxy"))
            .unwrap_or_default(),
    );
    if no_proxy_disables_all(&no_proxy) {
        return Ok(None);
    }
    let matcher = hyper_util::client::proxy::matcher::Matcher::builder()
        .all(normalized.as_str())
        .no(no_proxy)
        .build();
    let destination: http::Uri = url.as_str().parse()?;
    let Some(route) = matcher.intercept(&destination) else {
        return Ok(None);
    };
    // Build the actual proxy from the matched route itself so the decision
    // that grants delegated DNS authority cannot diverge from the transport.
    let route_uri = route.uri().to_string();
    let route_uri = match route.uri().scheme_str() {
        Some("socks5") => route_uri.replacen("socks5://", "socks5h://", 1),
        Some("socks4") => route_uri.replacen("socks4://", "socks4a://", 1),
        _ => route_uri,
    };
    let mut proxy = reqwest::Proxy::all(route_uri)?;
    if let Some(auth) = route.basic_auth() {
        proxy = proxy.custom_http_auth(auth.clone());
    }
    if let Some((user, password)) = route.raw_auth() {
        proxy = proxy.basic_auth(user, password);
    }
    Ok(Some(proxy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn loopback_listener() -> TcpListener {
        TcpListener::bind("127.0.0.1:0").expect("bind loopback proxy")
    }

    fn port_of(listener: &TcpListener) -> u16 {
        listener.local_addr().expect("loopback address").port()
    }

    /// A port that was bound and released, so nothing answers it. The window
    /// before the OS reuses the port only has to outlast one probe.
    fn closed_port() -> u16 {
        let listener = loopback_listener();
        let port = port_of(&listener);
        drop(listener);
        port
    }

    fn proxy_url(port: u16) -> reqwest::Url {
        reqwest::Url::parse(&format!("http://127.0.0.1:{port}")).expect("proxy url")
    }

    fn engine_target() -> reqwest::Url {
        reqwest::Url::parse("https://api.deepseek.com/v1/chat/completions").unwrap()
    }

    fn watch(https_port: u16) -> Arc<LoopbackProxy> {
        LoopbackProxy::new(None, None, Some(proxy_url(https_port)), "")
            .expect("loopback proxy is watched")
    }

    /// A byte-for-byte HTTP server that answers every request with `200 ok`.
    fn spawn_target_server() -> (u16, std::thread::JoinHandle<()>) {
        let listener = loopback_listener();
        let port = port_of(&listener);
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
            }
        });
        (port, handle)
    }

    /// A server that answers like a proxy refusing to forward: `502`.
    fn spawn_refusing_proxy() -> (u16, std::thread::JoinHandle<()>) {
        let listener = loopback_listener();
        let port = port_of(&listener);
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        (port, handle)
    }

    #[test]
    fn a_portless_socks_proxy_defaults_to_1080() {
        let socks = reqwest::Url::parse("socks5://127.0.0.1").unwrap();

        let watch = LoopbackProxy::new(Some(socks), None, None, "").expect("watched");

        assert_eq!(watch.targets[0].port, 1080);
    }

    /// A socks5 route keeps remote DNS: building the matcher for that scheme
    /// must not panic, and the returned route must name socks5h so the proxy
    /// resolves the hostname rather than the client.
    #[test]
    fn a_socks5_proxy_route_keeps_remote_dns() {
        let listener = loopback_listener();
        let port = port_of(&listener);
        let socks = reqwest::Url::parse(&format!("socks5://127.0.0.1:{port}")).unwrap();
        let watch = LoopbackProxy::new(Some(socks), None, None, "").expect("watched");

        let route = watch.route(&engine_target()).expect("socks route");

        assert_eq!(route.scheme(), "socks5h");
        assert_eq!(route.port(), Some(port));
        drop(listener);
    }

    #[test]
    fn each_proxy_is_watched_separately() {
        let http_listener = loopback_listener();
        let http_port = port_of(&http_listener);
        let dead_https = closed_port();
        let watch = LoopbackProxy::new(
            None,
            Some(proxy_url(http_port)),
            Some(proxy_url(dead_https)),
            "",
        )
        .expect("watched");

        watch.refresh();

        let http_target = reqwest::Url::parse("http://example.com/").unwrap();
        let https_target = reqwest::Url::parse("https://example.com/").unwrap();
        assert!(
            watch.route(&http_target).is_some(),
            "a live http proxy must keep routing"
        );
        assert!(
            watch.route(&https_target).is_none(),
            "a dead https proxy must not swallow https traffic"
        );
        drop(http_listener);
    }

    /// Proves the install order: with a live proxy the request must reach the
    /// proxy (502), not the target (200). Installing `no_proxy()` after
    /// `proxy()` would clear the route and let the request go direct.
    ///
    /// Coverage boundary: this pins the order, not the presence of
    /// `no_proxy()`. Dropping that call would leave reqwest's ambient matcher in
    /// place, which is only observable when the process environment carries a
    /// proxy — and mutating the process environment from a test is what this
    /// suite stopped doing.
    #[tokio::test]
    async fn the_route_is_installed_after_the_proxy_list_is_cleared() {
        let (target_port, target) = spawn_target_server();
        let (proxy_port, refusing) = spawn_refusing_proxy();
        let watch =
            LoopbackProxy::new(Some(proxy_url(proxy_port)), None, None, "").expect("watched");
        let route = reqwest::Proxy::custom({
            let watch = Arc::clone(&watch);
            move |url| watch.route(url)
        });
        let client = install_proxy_route(
            codewhale_release::tls::reqwest_client_builder(),
            Some(route),
        )
        .timeout(Duration::from_secs(5))
        .build()
        .expect("client");

        let response = client
            .get(format!("http://127.0.0.1:{target_port}/"))
            .send()
            .await
            .expect("request");

        assert_eq!(
            response.status(),
            502,
            "the request must reach the proxy, not the target"
        );
        drop(target);
        drop(refusing);
    }

    #[test]
    fn no_configured_proxy_is_not_watched() {
        assert!(LoopbackProxy::new(None, None, None, "").is_none());
    }

    #[test]
    fn remote_proxy_is_not_watched() {
        let remote = reqwest::Url::parse("http://proxy.example.test:8080").unwrap();

        assert!(
            LoopbackProxy::new(None, None, Some(remote), "").is_none(),
            "a remote proxy's outage is a network outage, not a routing decision"
        );
    }

    #[test]
    fn a_remote_proxy_in_a_mixed_configuration_leaves_the_local_one_watched() {
        let local_listener = loopback_listener();
        let local_port = port_of(&local_listener);
        let remote = reqwest::Url::parse("http://proxy.example.test:8080").unwrap();

        let watch = LoopbackProxy::new(None, Some(proxy_url(local_port)), Some(remote), "")
            .expect("the local proxy is still watched");

        watch.refresh();

        assert_eq!(watch.targets.len(), 1, "only loopback proxies are probed");
        let http_target = reqwest::Url::parse("http://example.com/").unwrap();
        let https_target = reqwest::Url::parse("https://example.com/").unwrap();
        assert!(
            watch.route(&http_target).is_some(),
            "the live local http proxy keeps routing"
        );
        assert_eq!(
            watch.route(&https_target).map(|url| url.to_string()),
            Some("http://proxy.example.test:8080/".to_string()),
            "a remote proxy is routed through, never watched"
        );
        drop(local_listener);
    }

    #[test]
    fn ipv6_loopback_is_watched() {
        // Skipped rather than failed on a host with IPv6 disabled: the feature
        // under test needs a local IPv6 listener to probe.
        let listener = match TcpListener::bind("[::1]:0") {
            Ok(listener) => listener,
            Err(_) => return,
        };
        let port = port_of(&listener);
        let url = reqwest::Url::parse(&format!("http://[::1]:{port}")).expect("ipv6 url");

        let watch = LoopbackProxy::new(None, None, Some(url), "").expect("watched");

        assert_eq!(watch.targets[0].host, "::1");
        assert_eq!(watch.targets[0].port, port);
        let route = watch.route(&engine_target()).expect("proxy route");
        assert_eq!(
            route.host_str(),
            Some("[::1]"),
            "reqwest needs the brackets"
        );
        drop(listener);
    }

    /// The route a request takes must agree with the matcher's decision and with
    /// the documented variable precedence. With `ALL_PROXY`, `HTTP_PROXY` and
    /// `HTTPS_PROXY` on three live ports, each scheme follows its own variable
    /// and `ALL_PROXY` is only the fallback.
    #[test]
    fn the_route_follows_the_same_precedence_as_the_matcher() {
        let all_listener = loopback_listener();
        let all_port = port_of(&all_listener);
        let http_listener = loopback_listener();
        let http_port = port_of(&http_listener);
        let https_listener = loopback_listener();
        let https_port = port_of(&https_listener);
        let watch = LoopbackProxy::new(
            Some(proxy_url(all_port)),
            Some(proxy_url(http_port)),
            Some(proxy_url(https_port)),
            "",
        )
        .expect("watched");

        let http_route = watch
            .route(&reqwest::Url::parse("http://example.com/").unwrap())
            .expect("http route");
        let https_route = watch
            .route(&reqwest::Url::parse("https://example.com/").unwrap())
            .expect("https route");

        assert_eq!(
            http_route.port(),
            Some(http_port),
            "HTTP_PROXY outranks ALL_PROXY"
        );
        assert_eq!(
            https_route.port(),
            Some(https_port),
            "HTTPS_PROXY outranks ALL_PROXY"
        );
        drop(all_listener);
        drop(http_listener);
        drop(https_listener);
    }

    /// `ALL_PROXY` alone still covers both schemes.
    #[test]
    fn all_proxy_alone_covers_both_schemes() {
        let all_listener = loopback_listener();
        let all_port = port_of(&all_listener);
        let watch = LoopbackProxy::new(Some(proxy_url(all_port)), None, None, "").expect("watched");

        for target in ["http://example.com/", "https://example.com/"] {
            let route = watch
                .route(&reqwest::Url::parse(target).unwrap())
                .expect("route from ALL_PROXY");
            assert_eq!(route.port(), Some(all_port));
        }
        drop(all_listener);
    }

    /// With an https proxy configured and no http one, a plain-http target must
    /// go direct: `HTTPS_PROXY` does not proxy http destinations, and nothing
    /// else may quietly stand in for it.
    #[test]
    fn an_https_only_proxy_leaves_http_targets_direct() {
        let https_listener = loopback_listener();
        let https_port = port_of(&https_listener);
        let watch =
            LoopbackProxy::new(None, None, Some(proxy_url(https_port)), "").expect("watched");

        assert!(
            watch
                .route(&reqwest::Url::parse("http://example.com/").unwrap())
                .is_none(),
            "HTTPS_PROXY must not intercept http destinations"
        );
        assert_eq!(
            watch
                .route(&reqwest::Url::parse("https://example.com/").unwrap())
                .expect("https route")
                .port(),
            Some(https_port)
        );
        drop(https_listener);
    }

    /// `[::ffff:127.0.0.1]` is the same local host spelled as IPv6, and
    /// `Ipv6Addr::is_loopback` alone would call it remote.
    #[test]
    fn an_ipv4_mapped_ipv6_loopback_is_watched() {
        let url = reqwest::Url::parse("http://[::ffff:127.0.0.1]:7890").expect("url");

        let watch = LoopbackProxy::new(None, None, Some(url), "").expect("watched");

        assert_eq!(watch.targets[0].port, 7890);
    }

    #[test]
    fn no_proxy_star_disables_the_watcher() {
        let local = proxy_url(closed_port());

        assert!(LoopbackProxy::new(None, None, Some(local), "*").is_none());
    }

    #[test]
    fn whitespace_separated_no_proxy_entries_match_commas() {
        assert_eq!(
            normalize_no_proxy("api.deepseek.com other.example"),
            "api.deepseek.com,other.example"
        );
        assert_eq!(
            normalize_no_proxy(" a.example,\n b.example "),
            ",a.example,,,b.example,"
        );
        assert!(no_proxy_disables_all("a.example *"));
        assert!(!no_proxy_disables_all("a.example"));
    }

    #[test]
    fn answering_loopback_port_keeps_proxy_routing() {
        let listener = loopback_listener();
        let port = port_of(&listener);
        let watch = watch(port);

        let route = watch.route(&engine_target()).expect("proxy route");

        assert_eq!(route.host_str(), Some("127.0.0.1"));
        assert_eq!(route.port(), Some(port));
        drop(listener);
    }

    #[test]
    fn loopback_proxy_credentials_survive_the_route() {
        let listener = loopback_listener();
        let port = port_of(&listener);
        let url = reqwest::Url::parse(&format!(
            "http://placeholder-user:placeholder-pass@127.0.0.1:{port}"
        ))
        .unwrap();
        let watch = LoopbackProxy::new(None, None, Some(url), "").expect("watched");

        let route = watch.route(&engine_target()).expect("proxy route");

        assert_eq!(route.username(), "placeholder-user");
        assert_eq!(route.password(), Some("placeholder-pass"));
        drop(listener);
    }

    #[test]
    fn no_proxy_hosts_bypass_the_watched_proxy() {
        let listener = loopback_listener();
        let port = port_of(&listener);
        let url = proxy_url(port);
        let watch = LoopbackProxy::new(None, None, Some(url), "api.deepseek.com").expect("watched");

        assert!(
            watch.route(&engine_target()).is_none(),
            "NO_PROXY must keep winning while the proxy is up"
        );
        drop(listener);
    }

    #[test]
    fn a_refused_port_moves_routing_to_direct() {
        let watch = watch(closed_port());
        assert!(
            watch.route(&engine_target()).is_some(),
            "an unprobed proxy keeps reqwest's behaviour"
        );

        watch.refresh();

        assert!(
            watch.route(&engine_target()).is_none(),
            "a refused port must stop intercepting requests"
        );
    }

    #[test]
    fn a_returning_port_moves_routing_back_to_the_proxy() {
        let listener = loopback_listener();
        let port = port_of(&listener);
        let watch = watch(port);
        drop(listener);
        watch.refresh();
        assert!(watch.route(&engine_target()).is_none(), "proxy is gone");

        let listener = TcpListener::bind(("127.0.0.1", port)).expect("rebind proxy port");
        watch.refresh();

        assert!(
            watch.route(&engine_target()).is_some(),
            "a answering port must restore proxy routing"
        );
        drop(listener);
    }

    /// End to end over a real client: while the configured proxy refuses
    /// connections the request must reach the target directly, and while it
    /// answers the request must go to the proxy instead.
    #[tokio::test]
    async fn a_closed_proxy_no_longer_blocks_the_request() {
        let (target_port, target) = spawn_target_server();
        let target_url = format!("http://127.0.0.1:{target_port}/");
        // ALL_PROXY so the plain-http target is proxied too; an HTTPS_PROXY
        // entry deliberately does not intercept http destinations.
        let watch = LoopbackProxy::new(Some(proxy_url(closed_port())), None, None, "")
            .expect("watched loopback proxy");
        let client = codewhale_release::tls::reqwest_client_builder()
            .no_proxy()
            .proxy(reqwest::Proxy::custom({
                let watch = Arc::clone(&watch);
                move |url| watch.route(url)
            }))
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client");

        assert!(
            client.get(&target_url).send().await.is_err(),
            "before a probe the dead proxy is still selected"
        );

        watch.refresh();
        let response = client
            .get(&target_url)
            .send()
            .await
            .expect("direct request");

        assert_eq!(response.status(), 200);
        drop(target);
    }

    #[test]
    fn reviewed_plugin_remote_proxy_policy_never_reads_ambient_environment() {
        let reads = std::cell::Cell::new(0_u32);
        let proxy = configured_mcp_proxy(
            &reqwest::Url::parse("https://example.com/mcp").unwrap(),
            true,
            |_| {
                reads.set(reads.get() + 1);
                Ok("http://127.0.0.1:9999".to_string())
            },
        );

        assert_eq!(
            reads.get(),
            0,
            "reviewed remotes must not read proxy values"
        );
        assert!(proxy.unwrap().is_none());
    }

    #[test]
    fn user_authored_mcp_proxy_policy_keeps_environment_support() {
        let requested = std::cell::RefCell::new(Vec::new());
        let proxy = configured_mcp_proxy(
            &reqwest::Url::parse("https://example.com/mcp").unwrap(),
            false,
            |name| {
                requested.borrow_mut().push(name.to_string());
                match name {
                    "HTTPS_PROXY" => Ok("http://127.0.0.1:8080".to_string()),
                    _ => Err(std::env::VarError::NotPresent),
                }
            },
        );

        assert_eq!(
            requested.into_inner(),
            vec![
                "HTTPS_PROXY".to_string(),
                "NO_PROXY".to_string(),
                "no_proxy".to_string(),
            ]
        );
        assert!(proxy.unwrap().is_some());
    }

    #[test]
    fn redact_proxy_userinfo_strips_password() {
        // Corporate-style proxy URL with embedded creds — the
        // password must never reach the on-disk log file. URL strings
        // are assembled from placeholder constants via `format!` so the
        // literal source never contains a scheme-prefixed username +
        // password pair (colon-separated, `@`-terminated) that
        // GitGuardian's "Basic Auth String" detector would flag as a
        // committed credential.
        let (placeholder_user, placeholder_pass) = ("PLACEHOLDER_USER", "PLACEHOLDER_PASS");
        let with_creds = format!("http://{placeholder_user}:{placeholder_pass}@proxy.example/");
        let redacted = redact_proxy_userinfo(&with_creds);
        assert_eq!(redacted, "http://***@proxy.example");
        assert!(!redacted.contains(placeholder_pass));
        assert!(!redacted.contains(placeholder_user));

        // User only (no password) — still redacted.
        let with_user_only = format!("https://{placeholder_user}@proxy.example:8080");
        let redacted = redact_proxy_userinfo(&with_user_only);
        assert_eq!(redacted, "https://***@proxy.example:8080");

        let redacted = redact_proxy_userinfo("http://proxy.example:3128/");
        assert_eq!(redacted, "http://proxy.example:3128");

        let redacted =
            redact_proxy_userinfo("http://proxy.example/path@thing?token=PLACEHOLDER#fragment");
        assert_eq!(redacted, "http://proxy.example");

        assert_eq!(redact_proxy_userinfo("not-a-url"), "<invalid proxy URL>");

        for host in [
            "ho st:8080",
            "proxy.example:port-not-a-number",
            "[::1:8080",
            "",
        ] {
            let malformed = format!("{placeholder_user}:{placeholder_pass}@{host}");
            let redacted = redact_proxy_userinfo(&malformed);
            assert_eq!(redacted, "<invalid proxy URL>", "{host:?}");
            assert!(!redacted.contains(placeholder_pass));
        }
    }
}
