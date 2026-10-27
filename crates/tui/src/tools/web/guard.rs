//! Shared SSRF guard for LLM-initiated HTTP fetches (`fetch_url`, `web.run`).
//!
//! Validates scheme/host, enforces network policy, resolves DNS and rejects
//! private/loopback/link-local/metadata addresses, and returns an optional
//! DNS pin so callers can bind the HTTP client to the validated address
//! (preventing TOCTOU rebinding). Callers that follow redirects must
//! re-invoke [`validate_fetch_target`] on every new Location.

use crate::tools::spec::{ToolContext, ToolError};
use codewhale_net::guard::{DnsPin, fetch_target_host, validate_fetch_destination};
use codewhale_net::network_policy::Decision;

/// Validate that `url` is a safe fetch target under SSRF and network policy.
///
/// On success returns an optional DNS pin `(hostname, ip)` for hostnames that
/// were resolved; literal public IPs return `None` (no pin needed).
///
/// `tool` is the policy/audit label (e.g. `"fetch_url"`, `"web_run"`).
pub(crate) async fn validate_fetch_target(
    url: &reqwest::Url,
    context: &ToolContext,
    tool: &str,
) -> Result<DnsPin, ToolError> {
    let host = fetch_target_host(url)?;
    validate_network_policy(&host, context, tool)?;
    validate_fetch_destination(host, context.network_policy.as_ref(), tool, true).await
}

pub(crate) fn validate_network_policy(
    host: &str,
    context: &ToolContext,
    tool: &str,
) -> Result<(), ToolError> {
    let Some(decider) = context.network_policy.as_ref() else {
        return Ok(());
    };

    match decider.evaluate(host, tool) {
        Decision::Allow => Ok(()),
        Decision::Deny => Err(ToolError::permission_denied(format!(
            "network call to '{host}' blocked by network policy"
        ))),
        Decision::Prompt => Err(ToolError::permission_denied(format!(
            "network call to '{host}' requires approval; \
             re-run after `/network allow {host}` or set network.default = \"allow\" in config"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_net::guard::DNS_PREFLIGHT_TIMEOUT;
    use std::path::PathBuf;

    fn ctx() -> ToolContext {
        ToolContext::new(PathBuf::from("."))
    }

    #[tokio::test]
    async fn redirected_localhost_hostname_is_rejected() {
        let url = reqwest::Url::parse("http://localhost:8080/admin").unwrap();
        let err = validate_fetch_target(&url, &ctx(), "fetch_url")
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("localhost"));
    }

    #[tokio::test]
    async fn redirected_private_ip_literal_is_rejected() {
        let url = reqwest::Url::parse("http://169.254.169.254/latest/meta-data").unwrap();
        let err = validate_fetch_target(&url, &ctx(), "fetch_url")
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("restricted address"));
    }

    // GHSA-88gh-2526-gfrr — regression coverage for bracketed IPv6 literals.
    #[tokio::test]
    async fn rejects_ipv6_literal_loopback() {
        let url = reqwest::Url::parse("http://[::1]/").unwrap();
        let err = validate_fetch_target(&url, &ctx(), "fetch_url")
            .await
            .expect_err("[::1] must be rejected as restricted");
        assert!(format!("{err}").contains("restricted"));
    }

    #[tokio::test]
    async fn rejects_ipv6_literal_ula() {
        let url = reqwest::Url::parse("http://[fc00::1]/").unwrap();
        let err = validate_fetch_target(&url, &ctx(), "fetch_url")
            .await
            .expect_err("[fc00::1] must be rejected as restricted");
        assert!(format!("{err}").contains("restricted"));
    }

    #[tokio::test]
    async fn rejects_ipv6_literal_link_local() {
        let url = reqwest::Url::parse("http://[fe80::1]/").unwrap();
        let err = validate_fetch_target(&url, &ctx(), "fetch_url")
            .await
            .expect_err("[fe80::1] must be rejected as restricted");
        assert!(format!("{err}").contains("restricted"));
    }

    #[tokio::test]
    async fn rejects_ipv6_literal_ipv4_mapped_loopback() {
        let url = reqwest::Url::parse("http://[::ffff:127.0.0.1]/").unwrap();
        let err = validate_fetch_target(&url, &ctx(), "fetch_url")
            .await
            .expect_err("[::ffff:127.0.0.1] must be rejected as restricted");
        assert!(format!("{err}").contains("restricted"));
    }

    #[tokio::test]
    async fn rejects_ipv6_literal_unspecified() {
        let url = reqwest::Url::parse("http://[::]/").unwrap();
        let err = validate_fetch_target(&url, &ctx(), "fetch_url")
            .await
            .expect_err("[::] must be rejected as restricted");
        assert!(format!("{err}").contains("restricted"));
    }

    #[tokio::test]
    async fn redirected_host_respects_network_policy() {
        use codewhale_net::network_policy::{Decision, NetworkPolicy, NetworkPolicyDecider};
        let policy = NetworkPolicy {
            default: Decision::Deny.into(),
            allow: vec!["api.deepseek.com".to_string()],
            deny: vec![],
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: false,
        };
        let decider = NetworkPolicyDecider::new(policy, None);
        let ctx = ToolContext::new(PathBuf::from(".")).with_network_policy(decider);
        let url = reqwest::Url::parse("https://example.com/redirect-target").unwrap();
        let err = validate_fetch_target(&url, &ctx, "fetch_url")
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("blocked"));
    }

    #[tokio::test]
    async fn unresolved_hostname_is_rejected_before_request() {
        let url =
            reqwest::Url::parse("https://codewhale-unresolvable-fetch-target.invalid/resource")
                .unwrap();
        let err = validate_fetch_target(&url, &ctx(), "fetch_url")
            .await
            .expect_err("unresolved host must fail preflight");
        let message = format!("{err}");
        assert!(
            message.contains("could not resolve host") || message.contains("restricted address"),
            "error must identify preflight DNS or restricted-IP failure; got {err}"
        );
    }

    #[tokio::test]
    async fn web_run_tool_label_is_used_in_dns_error() {
        let url =
            reqwest::Url::parse("https://codewhale-unresolvable-web-run-target.invalid/resource")
                .unwrap();
        let err = validate_fetch_target(&url, &ctx(), "web_run")
            .await
            .expect_err("unresolved host must fail preflight");
        let message = format!("{err}");
        // Either DNS failure (mentions web_run) or a restricted resolution.
        assert!(
            message.contains("web_run") || message.contains("restricted address"),
            "error should be labeled for web_run or report restricted IP; got {err}"
        );
    }

    #[test]
    fn dns_preflight_bound_stays_below_the_fetch_hard_cap() {
        // The pre-flight resolution runs before every guarded request (and
        // once per redirect); its own bound must leave room inside the fetch
        // tool's hard cap instead of being able to outlast it. A wedged
        // resolver surfaces as a timeout error, not a stalled tool.
        assert!(
            DNS_PREFLIGHT_TIMEOUT < super::super::fetch::HARD_MAX_TIMEOUT,
            "DNS pre-flight bound {:?} must stay below the fetch hard cap {:?}",
            DNS_PREFLIGHT_TIMEOUT,
            super::super::fetch::HARD_MAX_TIMEOUT
        );
    }
}
