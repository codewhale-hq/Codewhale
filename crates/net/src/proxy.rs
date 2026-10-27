use anyhow::Result;

/// Redact the userinfo segment (`username[:password]@…` portion) from
/// a proxy URL so it can be safely included in `tracing::warn!` output
/// without leaking the
/// password into the on-disk log. URLs without userinfo are returned
/// unchanged. Garbage input (no `://` scheme separator) is also returned
/// unchanged — the malformed-URL warning path is the only caller, so an
/// unparseable input is already the failure case.
fn redact_proxy_userinfo(proxy_url: &str) -> String {
    let Some(scheme_end) = proxy_url.find("://") else {
        return proxy_url.to_string();
    };
    let after_scheme = scheme_end + 3;
    // The userinfo segment ends at the next `@`, but only if that `@`
    // comes before the next `/`, `?`, or `#` (otherwise the `@` is in a
    // path / query and the URL has no userinfo at all).
    let rest = &proxy_url[after_scheme..];
    let at_idx = rest.find('@');
    let path_idx = rest.find(['/', '?', '#']);
    let userinfo_end = match (at_idx, path_idx) {
        (Some(a), Some(p)) if a < p => Some(a),
        (Some(a), None) => Some(a),
        _ => None,
    };
    if let Some(end) = userinfo_end {
        let mut out = String::with_capacity(proxy_url.len());
        out.push_str(&proxy_url[..after_scheme]);
        out.push_str("***@");
        out.push_str(&rest[end + 1..]);
        out
    } else {
        proxy_url.to_string()
    }
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
        tracing::warn!(target: "mcp", proxy = %redact_proxy_userinfo(&proxy_url), "ignoring malformed HTTP(S)_PROXY URL");
        return Ok(None);
    };
    let no_proxy = read_environment("NO_PROXY")
        .or_else(|_| read_environment("no_proxy"))
        .unwrap_or_default()
        .replace(char::is_whitespace, ",");
    if no_proxy.split(',').any(|entry| entry == "*") {
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
        assert_eq!(redacted, "http://***@proxy.example/");
        assert!(!redacted.contains(placeholder_pass));
        assert!(!redacted.contains(placeholder_user));

        // User only (no password) — still redacted.
        let with_user_only = format!("https://{placeholder_user}@proxy.example:8080");
        let redacted = redact_proxy_userinfo(&with_user_only);
        assert_eq!(redacted, "https://***@proxy.example:8080");

        // No userinfo segment — pass through.
        let redacted = redact_proxy_userinfo("http://proxy.example:3128/");
        assert_eq!(redacted, "http://proxy.example:3128/");

        // `@` appears only in the path, not as userinfo separator —
        // must not be mistaken for credentials.
        let redacted = redact_proxy_userinfo("http://proxy.example/path@thing");
        assert_eq!(redacted, "http://proxy.example/path@thing");

        // Garbage input (no `://`) returned unchanged — the
        // surrounding warning log is the only caller and is already
        // handling the malformed-URL case.
        assert_eq!(redact_proxy_userinfo("not-a-url"), "not-a-url");
    }
}
