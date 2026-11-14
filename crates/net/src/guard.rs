use crate::network_policy::{Decision, NetworkPolicy, NetworkPolicyDecider};
use anyhow::{anyhow, bail};
use codewhale_tools::ToolError;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

/// Wall-clock bound for one pre-flight DNS resolution. The resolution runs
/// before the guarded request (and once per redirect), so a hung resolver
/// would otherwise stall the tool far beyond the documented request timeout
/// envelope of the calling fetch tool.
pub const DNS_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(10);

/// DNS pin returned when a hostname was resolved to a validated public IP.
/// Callers should pass this to `reqwest::ClientBuilder::resolve` so the
/// connection uses the pre-validated address instead of re-resolving.
pub type DnsPin = Option<(String, IpAddr)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivateTargets {
    Refuse,
    TypedLocalHosts,
}

/// Build the transport used after a destination has passed SSRF validation.
/// Ambient HTTP(S)/SOCKS proxies are deliberately disabled: a proxy would
/// receive the original hostname, resolve it again outside this process, and
/// bypass the validated DNS pin.
pub fn guarded_reqwest_client_builder() -> reqwest::ClientBuilder {
    codewhale_release::tls::reqwest_client_builder().no_proxy()
}

const CLOUD_METADATA_V4: [Ipv4Addr; 4] = [
    Ipv4Addr::new(169, 254, 169, 254),
    Ipv4Addr::new(100, 100, 100, 200),
    Ipv4Addr::new(168, 63, 129, 16),
    Ipv4Addr::new(192, 0, 0, 192),
];

const CLOUD_METADATA_V6: [Ipv6Addr; 2] = [
    Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254),
    Ipv6Addr::new(0xfd20, 0x00ce, 0, 0, 0, 0, 0, 0x0254),
];

/// Check if an IP address is loopback, private, link-local, cloud-metadata,
/// multicast, or reserved — all addresses that should not be reachable via
/// an LLM-initiated fetch request (SSRF prevention).
pub fn is_restricted_ip(ip: &IpAddr) -> bool {
    if is_cloud_metadata_ip(ip) {
        return true;
    }
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.octets()[0] == 0
                // 100.64.0.0/10 — Carrier-grade NAT (CGNAT / shared address space)
                || matches!(v4.octets(), [100, 64..=127, ..])
                // 198.18.0.0/15 — IETF benchmark testing
                || matches!(v4.octets(), [198, 18..=19, ..])
                // 240.0.0.0/4 — reserved (former Class E)
                || v4.octets()[0] >= 240
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped IPv6 addresses (::ffff:a.b.c.d) are refused whole, so
            // ::ffff:127.0.0.1 and the like cannot bypass the IPv4 rules.
            if v6.is_unspecified() || matches!(v6.segments(), [0, 0, 0, 0, 0, 0xffff, ..]) {
                return true;
            }
            // A translator on the path reaches the embedded IPv4 target, so a
            // prefix must not hide a restricted address.
            if embedded_ipv4(*v6)
                .iter()
                .any(|v4| is_restricted_ip(&IpAddr::V4(*v4)))
            {
                return true;
            }
            v6.is_loopback()
                || v6.is_multicast()
                || matches!(v6.segments(), [0xfc00..=0xfdff, ..]) // ULA fc00::/7
                || matches!(v6.segments(), [0xfe80..=0xfebf, ..]) // Link-local fe80::/10
                || matches!(v6.segments(), [0xfec0..=0xfeff, ..]) // Site-local fec0::/10
                || matches!(v6.segments(), [0x2002, ..]) // 6to4 2002::/16
                || matches!(v6.segments(), [0x2001, 0x0000, ..]) // Teredo 2001::/32
                || matches!(v6.segments(), [0, 0, 0, 0, 0, 0, ..]) // IPv4-compatible ::a.b.c.d
                || matches!(v6.segments(), [0, 0, 0, 0, 0xffff, 0, ..])
        }
    }
}

fn local_use_nat64_ipv4_layouts(v6: Ipv6Addr) -> Option<[Ipv4Addr; 4]> {
    if !matches!(v6.segments(), [0x0064, 0xff9b, 0x0001, ..]) {
        return None;
    }
    let o = v6.octets();
    Some([
        Ipv4Addr::new(o[6], o[7], o[9], o[10]),
        Ipv4Addr::new(o[7], o[9], o[10], o[11]),
        Ipv4Addr::new(o[9], o[10], o[11], o[12]),
        Ipv4Addr::new(o[12], o[13], o[14], o[15]),
    ])
}

pub fn is_cloud_metadata_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local() || CLOUD_METADATA_V4.contains(v4),
        IpAddr::V6(v6) => {
            matches!(v6.segments(), [0xfe80..=0xfebf, ..])
                || CLOUD_METADATA_V6.contains(v6)
                || embedded_ipv4(*v6)
                    .iter()
                    .any(|v4| is_cloud_metadata_ip(&IpAddr::V4(*v4)))
                || local_use_nat64_ipv4_layouts(*v6)
                    .into_iter()
                    .flatten()
                    .any(|v4| is_cloud_metadata_ip(&IpAddr::V4(v4)))
        }
    }
}

fn embedded_ipv4(v6: Ipv6Addr) -> Vec<Ipv4Addr> {
    let octets = v6.octets();
    let tail = Ipv4Addr::new(octets[12], octets[13], octets[14], octets[15]);
    let from_halves = |high: u16, low: u16| {
        Ipv4Addr::new((high >> 8) as u8, high as u8, (low >> 8) as u8, low as u8)
    };
    match v6.segments() {
        [0, 0, 0, 0, 0, 0xffff, ..] | [0, 0, 0, 0, 0xffff, 0, ..] | [0, 0, 0, 0, 0, 0, ..] => {
            vec![tail]
        }
        [0x0064, 0xff9b, 0, 0, 0, 0, ..] => vec![tail],
        [0x0064, 0xff9b, 0x0001, 0, 0, 0, ..] => vec![tail],
        [0x0064, 0xff9b, 0x0001, a, b, c, d, e] => {
            let layout_48 = Ipv4Addr::new((a >> 8) as u8, a as u8, b as u8, (c >> 8) as u8);
            if d | e == 0 {
                vec![layout_48]
            } else {
                vec![layout_48, tail]
            }
        }
        [0x2002, high, low, ..] => vec![from_halves(high, low)],
        [
            0x2001,
            0x0000,
            server_high,
            server_low,
            _,
            _,
            client_high,
            client_low,
        ] => {
            vec![
                from_halves(server_high, server_low),
                from_halves(!client_high, !client_low),
            ]
        }
        _ => Vec::new(),
    }
}

fn is_local_network_ip(ip: &IpAddr) -> bool {
    if is_cloud_metadata_ip(ip) {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || matches!(v4.octets(), [100, 64..=127, ..])
        }
        IpAddr::V6(v6) => v6.is_loopback() || matches!(v6.segments(), [0xfc00..=0xfdff, ..]),
    }
}

fn is_localhost_name(bare: &str) -> bool {
    bare == "localhost" || bare == "localhost.localdomain" || bare.ends_with(".localhost")
}

fn is_localhost_wildcard(bare: &str) -> bool {
    let Some(suffix) = bare.strip_prefix("*.").or_else(|| bare.strip_prefix('.')) else {
        return false;
    };
    if suffix.is_empty() {
        return false;
    }
    let policy = NetworkPolicy {
        allow: vec![bare.to_string()],
        ..NetworkPolicy::default()
    };
    policy.explicitly_allows("localhost.localdomain")
        || policy.explicitly_allows("sub.localhost")
        || is_localhost_name(&format!("sub.{suffix}"))
}

fn literal_ip(host: &str) -> Option<IpAddr> {
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
        .parse()
        .ok()
}

fn visibly_local_host(bare: &str) -> bool {
    is_localhost_name(bare) || literal_ip(bare).is_some_and(|ip| is_local_network_ip(&ip))
}

pub fn is_visibly_local_host(host: &str) -> bool {
    let bare = bare_host(host);
    visibly_local_host(&bare) || is_localhost_wildcard(&bare)
}

fn bare_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

pub fn fetch_target_host(url: &reqwest::Url) -> Result<String, ToolError> {
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(ToolError::invalid_input(
            "only http:// and https:// URLs are supported",
        ));
    }

    url.host_str()
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| ToolError::invalid_input("URL must include a host"))
}

fn explicitly_allowed(decider: Option<&NetworkPolicyDecider>, bare: &str) -> bool {
    decider.is_some_and(|decider| decider.policy().explicitly_allows(bare))
}

pub fn is_typed_local_host(
    decider: Option<&NetworkPolicyDecider>,
    host: &str,
    private: PrivateTargets,
) -> bool {
    if private != PrivateTargets::TypedLocalHosts {
        return false;
    }
    let bare = bare_host(host);
    visibly_local_host(&bare) && explicitly_allowed(decider, &bare)
}

fn restricted_host_hint(host: &str, private: PrivateTargets) -> String {
    match private {
        PrivateTargets::Refuse => {
            "this request never reaches local or private addresses".to_string()
        }
        PrivateTargets::TypedLocalHosts if visibly_local_host(&bare_host(host)) => format!(
            "add {host:?} with `/network allow {host}` (or `network.allow` in config) to permit it"
        ),
        PrivateTargets::TypedLocalHosts => {
            "`/network allow` admits only a localhost name or a private IP address that you \
             typed, and never a cloud metadata address"
                .to_string()
        }
    }
}

pub async fn validate_fetch_destination(
    host: String,
    decider: Option<&NetworkPolicyDecider>,
    tool: &str,
    resolve: bool,
    private: PrivateTargets,
) -> Result<DnsPin, ToolError> {
    // SSRF protection: reject localhost names and private/link-local/loopback
    // IP literals; when `resolve` is set, also resolve the hostname and reject
    // restricted answers. A caller that passes `TypedLocalHosts` lets a
    // `network.allow` entry admit a localhost name or private IP literal the
    // person typed. Cloud metadata stays blocked either way.
    let bare = bare_host(&host);
    let typed_local = is_typed_local_host(decider, &bare, private);
    if !typed_local && is_localhost_name(&bare) {
        return Err(ToolError::permission_denied(format!(
            "requests to localhost are not allowed; {}",
            restricted_host_hint(&bare, private)
        )));
    }
    // Normalize bracketed IPv6 literals before the literal-IP check so they
    // route through the same restricted-IP policy as unbracketed forms
    // (GHSA-88gh-2526-gfrr).
    if let Some(ip) = literal_ip(&bare) {
        if is_restricted_ip(&ip) && !(typed_local && is_local_network_ip(&ip)) {
            return Err(ToolError::permission_denied(format!(
                "IP {ip} is a restricted address (private/loopback/link-local); {}",
                restricted_host_hint(&bare, private)
            )));
        }
        return Ok(None);
    }
    if !resolve {
        return Ok(None);
    }

    let addrs = tokio::time::timeout(
        DNS_PREFLIGHT_TIMEOUT,
        tokio::net::lookup_host((host.as_str(), 0u16)),
    )
    .await
    .map_err(|_| {
        ToolError::permission_denied(format!("timed out resolving host before {tool} request"))
    })?
    .map_err(|e| {
        ToolError::permission_denied(format!("could not resolve host before {tool} request: {e}"))
    })?;
    let mut first_valid: Option<IpAddr> = None;
    for addr in addrs {
        validate_dns_resolved_ip(&host, &addr.ip(), decider, tool, private)?;
        if first_valid.is_none() {
            first_valid = Some(addr.ip());
        }
    }

    let Some(validated_ip) = first_valid else {
        return Err(ToolError::permission_denied(format!(
            "host resolved to no addresses before {tool} request"
        )));
    };
    Ok(Some((host, validated_ip)))
}

pub fn validate_dns_resolved_ip(
    host: &str,
    ip: &IpAddr,
    decider: Option<&NetworkPolicyDecider>,
    tool: &str,
    private: PrivateTargets,
) -> Result<(), ToolError> {
    if !is_restricted_ip(ip) {
        return Ok(());
    }

    let bare = bare_host(host);
    if is_localhost_name(&bare) && ip.is_loopback() && is_typed_local_host(decider, &bare, private)
    {
        return Ok(());
    }

    // A fake-IP exception requires both an explicitly trusted hostname and an
    // explicitly trusted placeholder CIDR. The CIDR parser admits only subnets
    // inside 198.18.0.0/15, so real private/loopback/link-local/metadata/ULA
    // addresses remain blocked even when the hostname is trusted.
    if let Some(decider) = decider
        && decider.is_trusted_fakeip_addr(ip)
        && decider.trusts_proxy_fakeip_host(host)
    {
        decider.record_trusted_proxy_fakeip_allow(host, tool);
        return Ok(());
    }

    Err(ToolError::permission_denied(format!(
        "resolved IP {ip} is a restricted address (private/loopback/link-local); {}",
        match private {
            PrivateTargets::Refuse => restricted_host_hint(host, private),
            PrivateTargets::TypedLocalHosts =>
                "`/network allow` does not admit a host name that resolves to a private address"
                    .to_string(),
        }
    )))
}

pub type RedirectCheck = dyn Fn(&reqwest::Url) -> anyhow::Result<()> + Send + Sync;

pub struct GuardedGet<'a> {
    url: &'a str,
    network: &'a NetworkPolicy,
    tool: &'a str,
    private: PrivateTargets,
    if_none_match: Option<&'a str>,
    accept: Option<&'a str>,
    connect_timeout: Duration,
    total_timeout: Duration,
    max_redirects: usize,
    redirect_check: Option<&'a RedirectCheck>,
    trust_first_hop: bool,
}

impl<'a> GuardedGet<'a> {
    pub fn new(
        url: &'a str,
        network: &'a NetworkPolicy,
        tool: &'a str,
        private: PrivateTargets,
    ) -> Self {
        Self {
            url,
            network,
            tool,
            private,
            if_none_match: None,
            accept: None,
            connect_timeout: Duration::from_secs(10),
            total_timeout: Duration::from_secs(60),
            max_redirects: 5,
            redirect_check: None,
            trust_first_hop: false,
        }
    }

    #[must_use]
    pub fn if_none_match(mut self, etag: Option<&'a str>) -> Self {
        self.if_none_match = etag;
        self
    }

    #[must_use]
    pub fn accept(mut self, accept: &'a str) -> Self {
        self.accept = Some(accept);
        self
    }

    #[must_use]
    pub fn timeouts(mut self, connect: Duration, total: Duration) -> Self {
        self.connect_timeout = connect;
        self.total_timeout = total;
        self
    }

    #[must_use]
    pub fn max_redirects(mut self, max_redirects: usize) -> Self {
        self.max_redirects = max_redirects;
        self
    }

    #[must_use]
    pub fn redirect_check(mut self, check: &'a RedirectCheck) -> Self {
        self.redirect_check = Some(check);
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn trust_first_hop(mut self) -> Self {
        self.trust_first_hop = true;
        self
    }

    pub async fn send(self) -> anyhow::Result<reqwest::Response> {
        let decider = NetworkPolicyDecider::new(self.network.clone(), None);
        let mut current = reqwest::Url::parse(self.url).map_err(|_| anyhow!("invalid url"))?;
        let mut trusted = self.trust_first_hop;
        for _ in 0..=self.max_redirects {
            let host = fetch_target_host(&current)?;
            if matches!(self.network.decide(&host), Decision::Deny) {
                bail!("network policy denies {host}");
            }
            let local = is_typed_local_host(Some(&decider), &host, self.private);
            let proxy = if trusted || local {
                None
            } else {
                crate::proxy::configured_mcp_proxy(&current, false, |key| std::env::var(key))?
            };
            let pin = if std::mem::take(&mut trusted) {
                None
            } else {
                validate_fetch_destination(
                    host,
                    Some(&decider),
                    self.tool,
                    proxy.is_none(),
                    self.private,
                )
                .await?
            };
            let mut builder = guarded_reqwest_client_builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(self.connect_timeout)
                .timeout(self.total_timeout);
            if let Some(proxy) = proxy {
                builder = builder.proxy(proxy);
            } else if let Some((host, ip)) = pin {
                builder = builder.resolve(&host, SocketAddr::new(ip, 0));
            }
            let client = builder.build().map_err(|err| {
                anyhow!("building the download client failed: {}", err.without_url())
            })?;
            let mut request = client.get(current.clone());
            if let Some(etag) = self.if_none_match {
                request = request.header(reqwest::header::IF_NONE_MATCH, etag);
            }
            if let Some(accept) = self.accept {
                request = request.header(reqwest::header::ACCEPT, accept);
            }
            let origin = current.origin().ascii_serialization();
            let response = request.send().await.map_err(|err| {
                anyhow::Error::new(err.without_url()).context(format!("failed to GET {origin}"))
            })?;
            if !response.status().is_redirection() {
                return Ok(response);
            }
            let Some(location) = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
            else {
                return Ok(response);
            };
            let next = response
                .url()
                .join(location)
                .map_err(|_| anyhow!("invalid redirect location from {origin}"))?;
            if let Some(check) = self.redirect_check {
                check(&next)?;
            }
            if current.scheme() == "https" && next.scheme() != "https" {
                bail!("redirect from {origin} would downgrade HTTPS");
            }
            current = next;
        }
        bail!(
            "too many redirects from {}; the limit is {}",
            reqwest::Url::parse(self.url)
                .map(|url| url.origin().ascii_serialization())
                .unwrap_or_default(),
            self.max_redirects
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(windows))]
    use std::io::{Read, Write};
    #[cfg(not(windows))]
    use std::net::{Ipv4Addr, SocketAddr, TcpListener};
    #[cfg(not(windows))]
    use std::process::Command;
    #[cfg(not(windows))]
    use std::sync::Arc;
    #[cfg(not(windows))]
    use std::sync::atomic::{AtomicBool, Ordering};
    #[cfg(not(windows))]
    use std::time::{Duration, Instant};

    #[cfg(not(windows))]
    #[derive(Clone, Copy, Debug)]
    enum AmbientProxyKind {
        Http,
        HttpsConnect,
        SocksRemoteDns,
    }

    #[cfg(not(windows))]
    fn spawn_accept_probe(
        stop: Arc<AtomicBool>,
        respond_http: bool,
        drain_http_headers: bool,
    ) -> (u16, std::thread::JoinHandle<bool>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind probe listener");
        let port = listener.local_addr().expect("probe address").port();
        listener
            .set_nonblocking(true)
            .expect("nonblocking probe listener");
        let handle = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // Accepted sockets inherit nonblocking from the listener
                        // on several Unixes. Restore blocking mode before timed
                        // header reads so a not-yet-ready socket is not treated
                        // as a hard failure (WouldBlock / EAGAIN).
                        stream
                            .set_nonblocking(false)
                            .expect("blocking probe stream");
                        if drain_http_headers {
                            stream
                                .set_read_timeout(Some(Duration::from_secs(2)))
                                .expect("probe read timeout");
                            let mut request = Vec::new();
                            let mut chunk = [0_u8; 1024];
                            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                                let read = match stream.read(&mut chunk) {
                                    Ok(n) => n,
                                    Err(err)
                                        if matches!(
                                            err.kind(),
                                            std::io::ErrorKind::WouldBlock
                                                | std::io::ErrorKind::TimedOut
                                                | std::io::ErrorKind::Interrupted
                                        ) =>
                                    {
                                        continue;
                                    }
                                    Err(err) => panic!("read probe request: {err}"),
                                };
                                if read == 0 {
                                    break;
                                }
                                request.extend_from_slice(&chunk[..read]);
                                if Instant::now() >= deadline {
                                    break;
                                }
                            }
                        }
                        if respond_http {
                            let _ = stream.write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                            );
                        }
                        return true;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(err) => panic!("probe accept failed: {err}"),
                }
                if stop.load(Ordering::SeqCst) || Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        (port, handle)
    }

    #[cfg(not(windows))]
    fn run_ambient_proxy_probe(kind: AmbientProxyKind, guarded: bool) -> (bool, bool) {
        let stop = Arc::new(AtomicBool::new(false));
        let (target_port, target_handle) = spawn_accept_probe(
            Arc::clone(&stop),
            matches!(
                kind,
                AmbientProxyKind::Http | AmbientProxyKind::SocksRemoteDns
            ),
            matches!(
                kind,
                AmbientProxyKind::Http | AmbientProxyKind::SocksRemoteDns
            ),
        );
        let (proxy_port, proxy_handle) = spawn_accept_probe(Arc::clone(&stop), true, false);

        let mut command = Command::new(std::env::current_exe().expect("current test executable"));
        command.args([
            "--exact",
            "guard::tests::guarded_transport_proxy_probe_child",
            "--ignored",
            "--nocapture",
        ]);
        for key in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
            "NO_PROXY",
            "no_proxy",
            "REQUEST_METHOD",
        ] {
            command.env_remove(key);
        }
        command
            .env("CODEWHALE_PROXY_PROBE_CHILD", "1")
            .env(
                "CODEWHALE_PROXY_PROBE_GUARDED",
                if guarded { "1" } else { "0" },
            )
            .env("CODEWHALE_PROXY_PROBE_TARGET_PORT", target_port.to_string());
        match kind {
            AmbientProxyKind::Http => {
                let proxy = format!("http://127.0.0.1:{proxy_port}");
                command
                    .env("CODEWHALE_PROXY_PROBE_SCHEME", "http")
                    .env("HTTP_PROXY", &proxy)
                    .env("http_proxy", proxy);
            }
            AmbientProxyKind::HttpsConnect => {
                let proxy = format!("http://127.0.0.1:{proxy_port}");
                command
                    .env("CODEWHALE_PROXY_PROBE_SCHEME", "https")
                    .env("HTTPS_PROXY", &proxy)
                    .env("https_proxy", proxy);
            }
            AmbientProxyKind::SocksRemoteDns => {
                let proxy = format!("socks5h://127.0.0.1:{proxy_port}");
                command
                    .env("CODEWHALE_PROXY_PROBE_SCHEME", "http")
                    .env("ALL_PROXY", &proxy)
                    .env("all_proxy", proxy);
            }
        }

        let output = command.output().expect("run proxy probe child");
        stop.store(true, Ordering::SeqCst);
        let target_hit = target_handle.join().expect("join target probe");
        let proxy_hit = proxy_handle.join().expect("join proxy probe");
        assert!(
            output.status.success(),
            "proxy probe child failed for {kind:?} guarded={guarded}:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        (target_hit, proxy_hit)
    }

    #[cfg(not(windows))]
    #[tokio::test]
    #[ignore = "subprocess helper for ambient proxy regression test"]
    async fn guarded_transport_proxy_probe_child() {
        if std::env::var_os("CODEWHALE_PROXY_PROBE_CHILD").is_none() {
            return;
        }
        let guarded = std::env::var("CODEWHALE_PROXY_PROBE_GUARDED").as_deref() == Ok("1");
        let scheme = std::env::var("CODEWHALE_PROXY_PROBE_SCHEME").expect("probe scheme");
        let target_port = std::env::var("CODEWHALE_PROXY_PROBE_TARGET_PORT")
            .expect("probe target port")
            .parse::<u16>()
            .expect("numeric target port");
        let host = "guarded-proxy-probe.example.invalid";
        let builder = if guarded {
            guarded_reqwest_client_builder()
        } else {
            codewhale_release::tls::reqwest_client_builder()
        };
        let client = builder
            .timeout(Duration::from_secs(2))
            .resolve(
                host,
                SocketAddr::new(Ipv4Addr::LOCALHOST.into(), target_port),
            )
            .build()
            .expect("build proxy probe client");
        let result = client
            .get(format!("{scheme}://{host}:{target_port}/"))
            .send()
            .await;
        if guarded && scheme == "http" {
            assert!(
                result.is_ok(),
                "guarded HTTP target should answer: {result:?}"
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn guarded_transport_bypasses_ambient_http_https_and_remote_dns_socks_proxies() {
        for kind in [
            AmbientProxyKind::Http,
            AmbientProxyKind::HttpsConnect,
            AmbientProxyKind::SocksRemoteDns,
        ] {
            let (unguarded_target, unguarded_proxy) = run_ambient_proxy_probe(kind, false);
            assert!(
                unguarded_proxy && !unguarded_target,
                "control client must demonstrate ambient {kind:?} proxy interception"
            );

            let (guarded_target, guarded_proxy) = run_ambient_proxy_probe(kind, true);
            assert!(
                guarded_target && !guarded_proxy,
                "guarded client must preserve its DNS pin and bypass ambient {kind:?} proxy"
            );
        }
    }

    #[test]
    fn rejects_private_localhost_literal() {
        assert!(is_restricted_ip(&"127.0.0.1".parse().unwrap()));
        assert!(is_restricted_ip(&"::1".parse().unwrap()));
    }

    #[test]
    fn rejects_private_rfc1918() {
        assert!(is_restricted_ip(&"10.0.0.1".parse().unwrap()));
        assert!(is_restricted_ip(&"172.16.0.1".parse().unwrap()));
        assert!(is_restricted_ip(&"192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn rejects_cloud_metadata() {
        assert!(is_restricted_ip(&"169.254.169.254".parse().unwrap()));
    }

    #[test]
    fn rejects_this_network_block() {
        assert!(is_restricted_ip(&"0.1.2.3".parse().unwrap()));
        assert!(is_restricted_ip(&"0.0.0.0".parse().unwrap()));
    }

    #[test]
    fn rejects_link_local() {
        assert!(is_restricted_ip(&"169.254.1.1".parse().unwrap()));
    }

    #[test]
    fn rejects_cgnat() {
        assert!(is_restricted_ip(&"100.64.0.1".parse().unwrap()));
        assert!(!is_restricted_ip(&"100.63.0.1".parse().unwrap()));
        assert!(!is_restricted_ip(&"100.128.0.1".parse().unwrap()));
    }

    #[test]
    fn rejects_ipv6_ula() {
        assert!(is_restricted_ip(&"fc00::1".parse().unwrap()));
        assert!(is_restricted_ip(&"fd12:3456::1".parse().unwrap()));
    }

    #[test]
    fn rejects_ipv4_mapped_ipv6() {
        // ::ffff:127.0.0.1 — IPv4-mapped IPv6 loopback bypass
        assert!(is_restricted_ip(&"::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_restricted_ip(&"::ffff:10.0.0.1".parse().unwrap()));
        assert!(is_restricted_ip(&"::ffff:169.254.169.254".parse().unwrap()));
        assert!(is_restricted_ip(&"::ffff:192.168.1.1".parse().unwrap()));
        // :: (unspecified)
        assert!(is_restricted_ip(&"::".parse().unwrap()));
    }

    #[test]
    fn allows_public_ips() {
        assert!(!is_restricted_ip(&"8.8.8.8".parse().unwrap()));
        assert!(!is_restricted_ip(&"1.1.1.1".parse().unwrap()));
        assert!(!is_restricted_ip(&"93.184.216.34".parse().unwrap()));
        assert!(!is_restricted_ip(&"2606:4700::1".parse().unwrap()));
    }

    #[test]
    fn restricted_dns_result_is_denied_without_proxy_opt_in() {
        let ip = "198.18.0.1".parse().unwrap();

        let err =
            validate_dns_resolved_ip("github.com", &ip, None, "fetch_url", PrivateTargets::Refuse)
                .expect_err("fake-IP DNS result must be denied by default");

        assert!(format!("{err}").contains("resolved IP 198.18.0.1 is a restricted address"));
    }

    #[test]
    fn proxy_host_and_fakeip_cidr_allow_matching_placeholder() {
        use crate::network_policy::{Decision, NetworkPolicy, NetworkPolicyDecider};

        let policy = NetworkPolicy {
            default: Decision::Allow.into(),
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: vec!["github.com".to_string()],
            proxy_fake_ip_cidrs: vec!["198.18.0.0/15".to_string()],
            audit: false,
        };
        let decider = NetworkPolicyDecider::new(policy, None);
        let ip = "198.18.0.1".parse().unwrap();

        validate_dns_resolved_ip(
            "github.com",
            &ip,
            Some(&decider),
            "fetch_url",
            PrivateTargets::Refuse,
        )
        .expect("matching host and fake-IP CIDR should allow the placeholder");
    }

    #[test]
    fn proxy_host_without_fakeip_cidr_does_not_allow_restricted_dns() {
        use crate::network_policy::{Decision, NetworkPolicy, NetworkPolicyDecider};

        let policy = NetworkPolicy {
            default: Decision::Allow.into(),
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: vec!["github.com".to_string()],
            proxy_fake_ip_cidrs: Vec::new(),
            audit: false,
        };
        let decider = NetworkPolicyDecider::new(policy, None);
        let ip = "198.18.0.1".parse().unwrap();

        validate_dns_resolved_ip(
            "github.com",
            &ip,
            Some(&decider),
            "fetch_url",
            PrivateTargets::Refuse,
        )
        .expect_err("hostname trust alone must not allow a restricted address");
    }

    #[test]
    fn fakeip_cidr_without_proxy_host_does_not_allow_restricted_dns() {
        use crate::network_policy::{Decision, NetworkPolicy, NetworkPolicyDecider};

        let policy = NetworkPolicy {
            default: Decision::Allow.into(),
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: vec!["198.18.0.0/15".to_string()],
            audit: false,
        };
        let decider = NetworkPolicyDecider::new(policy, None);
        let ip = "198.18.0.1".parse().unwrap();

        validate_dns_resolved_ip(
            "github.com",
            &ip,
            Some(&decider),
            "fetch_url",
            PrivateTargets::Refuse,
        )
        .expect_err("fake-IP CIDR alone must not allow an untrusted hostname");
    }

    #[test]
    fn proxy_host_never_exempts_real_private_or_local_addresses() {
        use crate::network_policy::{Decision, NetworkPolicy, NetworkPolicyDecider};

        let policy = NetworkPolicy {
            default: Decision::Allow.into(),
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: vec!["github.com".to_string()],
            proxy_fake_ip_cidrs: vec![
                "198.18.0.0/15".to_string(),
                "127.0.0.0/8".to_string(),
                "10.0.0.0/8".to_string(),
                "169.254.0.0/16".to_string(),
            ],
            audit: false,
        };
        let decider = NetworkPolicyDecider::new(policy, None);

        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "fc00::1",
        ] {
            let ip = ip.parse().unwrap();
            assert!(
                validate_dns_resolved_ip(
                    "github.com",
                    &ip,
                    Some(&decider),
                    "fetch_url",
                    PrivateTargets::Refuse
                )
                .is_err(),
                "{ip} must remain restricted"
            );
        }
    }

    #[test]
    fn proxy_opt_in_does_not_allow_unlisted_host() {
        use crate::network_policy::{Decision, NetworkPolicy, NetworkPolicyDecider};

        let policy = NetworkPolicy {
            default: Decision::Allow.into(),
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: vec!["github.com".to_string()],
            proxy_fake_ip_cidrs: vec!["198.18.0.0/15".to_string()],
            audit: false,
        };
        let decider = NetworkPolicyDecider::new(policy, None);
        let ip = "198.18.0.1".parse().unwrap();

        let err = validate_dns_resolved_ip(
            "example.com",
            &ip,
            Some(&decider),
            "fetch_url",
            PrivateTargets::Refuse,
        )
        .expect_err("proxy opt-in must be scoped to configured hosts");

        assert!(format!("{err}").contains("resolved IP 198.18.0.1 is a restricted address"));
    }

    #[test]
    fn proxy_dns_allow_is_audited() {
        use crate::network_policy::{
            Decision, NetworkAuditor, NetworkPolicy, NetworkPolicyDecider,
        };
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let auditor = NetworkAuditor::new(dir.path().join("audit.log"), true);
        let policy = NetworkPolicy {
            default: Decision::Allow.into(),
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: vec!["github.com".to_string()],
            proxy_fake_ip_cidrs: vec!["198.18.0.0/15".to_string()],
            audit: true,
        };
        let decider = NetworkPolicyDecider::new(policy, Some(auditor));
        let ip = "198.18.0.1".parse().unwrap();

        validate_dns_resolved_ip(
            "github.com",
            &ip,
            Some(&decider),
            "fetch_url",
            PrivateTargets::Refuse,
        )
        .expect("proxy DNS allow");

        let body = std::fs::read_to_string(dir.path().join("audit.log")).expect("audit log");
        assert!(body.contains("github.com"));
        assert!(body.contains("TrustedProxyFakeIp-Allow"));
    }
    fn allow_decider(hosts: &[&str]) -> NetworkPolicyDecider {
        use crate::network_policy::NetworkPolicy;
        let policy = NetworkPolicy {
            default: Decision::Prompt.into(),
            allow: hosts.iter().map(|host| host.to_string()).collect(),
            deny: Vec::new(),
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: false,
        };
        NetworkPolicyDecider::new(policy, None)
    }

    #[test]
    fn allow_listed_name_never_admits_a_private_dns_answer() {
        let decider = allow_decider(&["skills.example"]);
        for private in [PrivateTargets::Refuse, PrivateTargets::TypedLocalHosts] {
            for ip in [
                "127.0.0.1",
                "10.0.0.5",
                "192.168.1.1",
                "100.64.1.2",
                "100.100.100.200",
                "169.254.170.2",
                "169.254.169.254",
                "::1",
                "fd00:ec2::254",
                "::ffff:169.254.169.254",
                "64:ff9b::a9fe:a9fe",
            ] {
                let ip = ip.parse().unwrap();
                assert!(
                    validate_dns_resolved_ip(
                        "skills.example",
                        &ip,
                        Some(&decider),
                        "skill_install",
                        private
                    )
                    .is_err(),
                    "{ip} must stay refused for an allow-listed name under {private:?}"
                );
            }
        }
    }

    #[test]
    fn a_typed_localhost_name_resolves_to_loopback_only() {
        let decider = allow_decider(&["localhost", "dev.localhost"]);
        let admit = |host: &str, ip: &str, private| {
            validate_dns_resolved_ip(
                host,
                &ip.parse().unwrap(),
                Some(&decider),
                "fetch_url",
                private,
            )
        };
        for host in ["localhost", "dev.localhost"] {
            admit(host, "127.0.0.1", PrivateTargets::TypedLocalHosts).expect(host);
            admit(host, "::1", PrivateTargets::TypedLocalHosts).expect(host);
            for ip in ["10.0.0.5", "100.64.1.2", "169.254.169.254", "fd00::1"] {
                admit(host, ip, PrivateTargets::TypedLocalHosts).expect_err(ip);
            }
            admit(host, "127.0.0.1", PrivateTargets::Refuse).expect_err(host);
        }
        let unlisted = allow_decider(&[]);
        validate_dns_resolved_ip(
            "localhost",
            &"127.0.0.1".parse().unwrap(),
            Some(&unlisted),
            "fetch_url",
            PrivateTargets::TypedLocalHosts,
        )
        .expect_err("a localhost name the person did not list stays refused");
    }

    #[tokio::test]
    async fn a_typed_local_literal_is_admitted_only_when_listed_and_asked_for() {
        for literal in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.9",
            "100.64.1.2",
            "[::1]",
            "[fd12:3456::1]",
        ] {
            let decider = allow_decider(&[literal]);
            validate_fetch_destination(
                literal.to_string(),
                Some(&decider),
                "fetch_url",
                true,
                PrivateTargets::TypedLocalHosts,
            )
            .await
            .expect(literal);
            validate_fetch_destination(
                literal.to_string(),
                Some(&decider),
                "fetch_url",
                true,
                PrivateTargets::Refuse,
            )
            .await
            .expect_err(literal);
            validate_fetch_destination(
                literal.to_string(),
                Some(&allow_decider(&[])),
                "fetch_url",
                true,
                PrivateTargets::TypedLocalHosts,
            )
            .await
            .expect_err(literal);
        }
        let decider = allow_decider(&["::1"]);
        validate_fetch_destination(
            "[::1]".to_string(),
            Some(&decider),
            "fetch_url",
            true,
            PrivateTargets::TypedLocalHosts,
        )
        .await
        .expect("an unbracketed entry names the bracketed literal");
    }

    #[tokio::test]
    async fn typed_and_allowed_literals_outside_the_local_ranges_stay_refused() {
        for literal in [
            "169.254.169.254",
            "169.254.170.2",
            "169.254.1.1",
            "100.100.100.200",
            "168.63.129.16",
            "0.0.0.0",
            "198.18.0.1",
            "224.0.0.1",
            "[::ffff:127.0.0.1]",
            "[::ffff:10.0.0.5]",
            "[::ffff:169.254.169.254]",
            "[64:ff9b::a9fe:a9fe]",
            "[64:ff9b::a00:1]",
            "[2002:a9fe:a9fe::1]",
            "[2001:0:a9fe:a9fe::1]",
            "[fd00:ec2::254]",
            "[fd20:ce::254]",
            "[fe80::1]",
            "[fec0::1]",
            "[::]",
        ] {
            let decider = allow_decider(&[literal]);
            for private in [PrivateTargets::Refuse, PrivateTargets::TypedLocalHosts] {
                validate_fetch_destination(
                    literal.to_string(),
                    Some(&decider),
                    "fetch_url",
                    true,
                    private,
                )
                .await
                .expect_err(literal);
            }
        }
    }

    #[test]
    fn cloud_metadata_set_covers_provider_endpoints_and_embedded_forms() {
        for literal in [
            "169.254.169.254",
            "169.254.170.2",
            "169.254.0.23",
            "100.100.100.200",
            "168.63.129.16",
            "192.0.0.192",
            "fd00:ec2::254",
            "fd20:ce::254",
            "fe80::1",
            "febf::1",
            "::ffff:169.254.169.254",
            "::169.254.169.254",
            "::ffff:0:a9fe:a9fe",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::a9fe:a9fe",
            "64:ff9b:1:8a9:fe:a9fe::",
            "64:ff9b:1:864:64:64c8::",
            "64:ff9b:1:808:a9:fea9:fe00:0",
            "64:ff9b:1:808:64:6464:c800:0",
            "2002:a9fe:a9fe::1",
            "2001:0:a9fe:a9fe::1",
            "2001:0:4136:e378:8000:63bf:5601:5601",
        ] {
            let ip = literal.parse().unwrap();
            assert!(is_restricted_ip(&ip), "{literal} must be restricted");
        }
        for literal in [
            "169.254.169.254",
            "169.254.170.2",
            "100.100.100.200",
            "fd00:ec2::254",
            "fe80::1",
            "::ffff:169.254.169.254",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::a9fe:a9fe",
            "64:ff9b:1:8a9:fe:a9fe::",
            "64:ff9b:1:864:64:64c8::",
            "64:ff9b:1:808:a9:fea9:fe00:0",
            "64:ff9b:1:808:64:6464:c800:0",
            "2002:a9fe:a9fe::1",
        ] {
            let ip = literal.parse().unwrap();
            assert!(is_cloud_metadata_ip(&ip), "{literal} must be metadata");
        }
        for literal in [
            "10.0.0.5",
            "127.0.0.1",
            "100.64.1.2",
            "fd12::1",
            "::1",
            "8.8.8.8",
        ] {
            let ip = literal.parse().unwrap();
            assert!(!is_cloud_metadata_ip(&ip), "{literal} is not metadata");
        }
        for literal in [
            "64:ff9b::808:808",
            "64:ff9b:1::808:808",
            "64:ff9b:1:808:808::",
            "64:ff9b:1:abcd::808:808",
            "64:ff9b:1:808:8:808:800:0",
        ] {
            let ip = literal.parse().unwrap();
            assert!(!is_cloud_metadata_ip(&ip), "{literal} is not metadata");
            assert!(!is_restricted_ip(&ip), "{literal} must stay allowed");
        }
    }

    #[test]
    fn translated_and_local_use_nat64_forms_embedding_internal_addresses_are_restricted() {
        for literal in [
            "64:ff9b:1::a9fe:a9fe",
            "64:ff9b:1::a00:1",
            "64:ff9b:1:abcd::a00:1",
            "64:ff9b:1:a00:1::",
            "64:ff9b:1:7f00:1::",
            "::ffff:0:a9fe:a9fe",
            "::ffff:0:7f00:1",
        ] {
            assert!(
                is_restricted_ip(&literal.parse().unwrap()),
                "{literal} must be restricted"
            );
        }
        for literal in [
            "64:ff9b::808:808",
            "64:ff9b:1::808:808",
            "64:ff9b:1:808:8::",
        ] {
            assert!(
                !is_restricted_ip(&literal.parse().unwrap()),
                "{literal} embeds a public address and must stay allowed"
            );
        }
    }

    fn serve_once(response: String) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener =
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("bind fixture");
        let port = listener.local_addr().expect("fixture address").port();
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&hits);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut chunk = [0_u8; 1024];
                let _ = stream.read(&mut chunk);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (port, hits)
    }

    fn redirect_to(location: &str) -> String {
        format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    fn policy_allowing(hosts: &[&str]) -> NetworkPolicy {
        NetworkPolicy {
            default: Decision::Prompt.into(),
            allow: hosts.iter().map(|host| host.to_string()).collect(),
            deny: Vec::new(),
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: false,
        }
    }

    #[tokio::test]
    async fn a_hop_to_an_allow_listed_local_literal_is_refused_unless_the_caller_asks() {
        use std::sync::atomic::Ordering;
        let ok = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
        let (internal, internal_hits) = serve_once(ok.to_string());
        let (entry, _) = serve_once(redirect_to(&format!("http://127.0.0.1:{internal}/secret")));
        let policy = policy_allowing(&["127.0.0.1"]);
        let url = format!("http://127.0.0.1:{entry}/start");

        let refused = GuardedGet::new(&url, &policy, "skill_install", PrivateTargets::Refuse)
            .trust_first_hop()
            .send()
            .await;
        let error = format!(
            "{:#}",
            refused.expect_err("an installer-style download must refuse the hop")
        );
        assert!(error.contains("restricted address"), "{error}");
        assert!(!error.contains("secret"), "{error}");
        assert_eq!(internal_hits.load(Ordering::SeqCst), 0);

        let admitted = GuardedGet::new(&url, &policy, "fetch_url", PrivateTargets::TypedLocalHosts)
            .trust_first_hop()
            .send()
            .await
            .expect("a typed local literal is admitted when the caller asks for it");
        assert_eq!(admitted.status(), reqwest::StatusCode::OK);
        assert_eq!(internal_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_hop_to_an_unlisted_local_literal_is_refused_even_for_a_caller_that_asks() {
        use std::sync::atomic::Ordering;
        let ok = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
        let (internal, internal_hits) = serve_once(ok.to_string());
        let (entry, _) = serve_once(redirect_to(&format!("http://127.0.0.1:{internal}/")));
        let policy = policy_allowing(&[]);
        let url = format!("http://127.0.0.1:{entry}/start");

        GuardedGet::new(
            &url,
            &policy,
            "config_import",
            PrivateTargets::TypedLocalHosts,
        )
        .trust_first_hop()
        .send()
        .await
        .expect_err("the hop must be refused");
        assert_eq!(internal_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn the_redirect_check_runs_before_a_hop_is_followed() {
        let (entry, _) = serve_once(redirect_to("http://example.invalid/x?token=secret-value"));
        let policy = policy_allowing(&[]);
        let url = format!("http://127.0.0.1:{entry}/start");
        let check =
            |_: &reqwest::Url| -> anyhow::Result<()> { anyhow::bail!("hop refused by caller") };

        let error = GuardedGet::new(&url, &policy, "config_import", PrivateTargets::Refuse)
            .trust_first_hop()
            .redirect_check(&check)
            .send()
            .await
            .expect_err("the caller's check refuses the hop");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("hop refused by caller"), "{rendered}");
        assert!(!rendered.contains("secret-value"), "{rendered}");
    }

    #[test]
    fn visibly_local_host_covers_localhost_wildcards() {
        for host in [
            "localhost",
            "localhost..",
            "localhost.localdomain",
            "dev.localhost",
            ".localdomain",
            "*.localdomain",
            "*.LOCALDOMAIN..",
            ".localhost",
            "*.localhost",
            ".dev.localhost",
            "*.dev.localhost",
            "127.0.0.1",
            "10.0.0.5",
            "[::1]",
            "fd00::1",
        ] {
            assert!(
                is_visibly_local_host(host),
                "{host} must be classified as visibly local"
            );
        }
        for host in [
            "example.com",
            ".example.com",
            "*.example.com",
            ".localhost.localdomain",
            "*.localhost.localdomain",
            "8.8.8.8",
            "169.254.169.254",
            "fe80::1",
        ] {
            assert!(
                !is_visibly_local_host(host),
                "{host} must not be classified as visibly local"
            );
        }
    }

    #[tokio::test]
    async fn trailing_dot_localhost_and_ip_literals_fail_fast() {
        for resolve in [false, true] {
            for host in [
                "localhost..",
                "LOCALHOST..",
                "dev.localhost..",
                "localhost.localdomain..",
            ] {
                let err = validate_fetch_destination(
                    host.to_string(),
                    None,
                    "fetch_url",
                    resolve,
                    PrivateTargets::TypedLocalHosts,
                )
                .await
                .expect_err("trailing-dot localhost name must fail fast");
                assert!(
                    format!("{err}").contains("requests to localhost are not allowed"),
                    "{host} (resolve={resolve}) got unexpected error: {err}"
                );
            }

            for literal in ["127.0.0.1..", "[::1].."] {
                let err = validate_fetch_destination(
                    literal.to_string(),
                    None,
                    "fetch_url",
                    resolve,
                    PrivateTargets::TypedLocalHosts,
                )
                .await
                .expect_err("trailing-dot local IP literal must fail fast");
                assert!(
                    format!("{err}").contains("restricted address"),
                    "{literal} (resolve={resolve}) got unexpected error: {err}"
                );
            }
        }
    }
}
