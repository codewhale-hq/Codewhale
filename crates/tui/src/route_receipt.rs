//! Secret-free lifecycle receipts for the exact route a turn was launched on.
//!
//! A [`TurnRouteReceipt`] is minted from the **installed, preflighted** client —
//! the one that was actually constructed to serve the turn — and travels with
//! the turn's lifecycle event. Consumers that need to prove "this later request
//! goes to the same base route, with the same credential, as the turn it
//! descends from" compare receipts instead of re-reading mutable config.
//!
//! Everything in a receipt is safe to carry through events, state, and `Debug`:
//!
//! - the provider enum and the non-secret configured route key,
//! - the canonical wire model id,
//! - a **normalized, redacted** endpoint identity (URL userinfo and sensitive
//!   query values masked by [`codewhale_secrets::sanitize::redact_url_for_display`]),
//! - a one-way credential *generation* digest that is never rendered.
//!
//! The credential generation is deliberately taken over the endpoint **and** the
//! credential together. Redaction is lossy on purpose — `https://a:b@host/v1`
//! and `https://c:d@host/v1` share one endpoint identity — so folding the raw
//! endpoint into the digest is what keeps a userinfo swap detectable.

use std::fmt;

use codewhale_config::ProviderKind;
use codewhale_config::credentials::CredentialGeneration;
use codewhale_runtime::config::ProviderIdentity;

/// Endpoint identity for a string that is not a parseable URL.
///
/// Deliberately opaque: an unparseable endpoint could be a filesystem path, and
/// absolute paths must never reach an event, log, or `Debug` rendering. Two
/// different unparseable endpoints therefore collide here — the credential
/// generation digest below is what still tells them apart.
const OPAQUE_ENDPOINT: &str = "<opaque-endpoint>";

/// Normalized, redacted, comparable identity for an API endpoint.
///
/// Safe to print. Trailing slashes are folded so `…/v1` and `…/v1/` are one
/// endpoint; nothing else is folded, so a host, scheme, port, or path change is
/// always a different identity.
#[must_use]
pub fn endpoint_identity(base_url: &str) -> String {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if reqwest::Url::parse(trimmed).is_err() {
        return OPAQUE_ENDPOINT.to_string();
    }
    codewhale_secrets::sanitize::redact_url_for_display(trimmed)
        .trim_end_matches('/')
        .to_string()
}

/// Immutable proof of the exact base route a turn's client was installed on.
#[derive(Clone, PartialEq, Eq)]
pub struct TurnRouteReceipt {
    identity: ProviderIdentity,
    wire_model: String,
    endpoint_identity: String,
    credential_generation: CredentialGeneration,
    openrouter_vendor: Option<String>,
}

impl TurnRouteReceipt {
    /// Mint a receipt from the values the installed client is bound to.
    ///
    /// `base_url` and `credential` are consumed here and never stored: only the
    /// redacted endpoint identity and the one-way generation digest survive.
    #[must_use]
    pub(crate) fn from_admitted(
        identity: &ProviderIdentity,
        wire_model: &str,
        base_url: &str,
        credential: &str,
    ) -> Self {
        Self {
            identity: identity.clone(),
            wire_model: wire_model.trim().to_string(),
            endpoint_identity: endpoint_identity(base_url),
            credential_generation: CredentialGeneration::derive(base_url, credential),
            openrouter_vendor: None,
        }
    }

    /// Test facts do not read a runtime config; production mints from admission.
    #[cfg(test)]
    pub(crate) fn new(
        provider: ProviderKind,
        provider_identity: &str,
        wire_model: &str,
        base_url: &str,
        credential: &str,
    ) -> Self {
        let identity = ProviderIdentity::test_exact_identity(provider, provider_identity);
        Self::from_admitted(&identity, wire_model, base_url, credential)
    }

    /// Non-executing fixture projection for cache scope tests. It uses the
    /// canonical Config admission and only an already provable read-only
    /// credential generation. An opaque source gets distinct synthetic facts;
    /// the production lookup still refuses to reuse that evidence.
    #[cfg(test)]
    pub(crate) fn for_test_fixture(
        config: &codewhale_runtime::config::Config,
        kind: ProviderKind,
        model: &str,
    ) -> Self {
        let identity = config.test_identity_for_kind(kind);
        let base_url = config.base_url_for_route(&identity);
        let generation = config
            .readonly_health_credential_generation(&identity)
            .unwrap_or_else(|| {
                CredentialGeneration::derive(&base_url, "opaque-test-only-generation")
            });
        Self {
            identity,
            wire_model: model.to_string(),
            endpoint_identity: endpoint_identity(&base_url),
            credential_generation: generation,
            openrouter_vendor: None,
        }
    }

    /// Keep upstream vendor restrictions frozen with the installed route.
    #[must_use]
    pub(crate) fn with_openrouter_vendor(mut self, vendor: Option<&str>) -> Self {
        self.openrouter_vendor = vendor.map(str::to_string);
        self
    }

    #[must_use]
    pub(crate) fn openrouter_vendor(&self) -> Option<&str> {
        self.openrouter_vendor.as_deref()
    }

    #[must_use]
    pub fn provider(&self) -> ProviderKind {
        self.identity.provider
    }

    #[must_use]
    pub fn provider_identity(&self) -> &str {
        self.identity.key.as_str()
    }

    pub(crate) fn admitted_identity(&self) -> &ProviderIdentity {
        &self.identity
    }

    #[must_use]
    pub fn wire_model(&self) -> &str {
        &self.wire_model
    }

    #[must_use]
    pub fn endpoint_identity(&self) -> &str {
        &self.endpoint_identity
    }

    #[must_use]
    pub fn credential_generation(&self) -> &CredentialGeneration {
        &self.credential_generation
    }

    /// Whether a live re-resolution of this route still lands on the same
    /// endpoint and the same credential generation.
    #[must_use]
    pub fn matches_live_route(&self, base_url: &str, credential: &str) -> bool {
        endpoint_identity(base_url) == self.endpoint_identity
            && CredentialGeneration::derive(base_url, credential) == self.credential_generation
    }
}

/// Redacted by construction: every field here is already non-secret, and the
/// generation digest renders as `<redacted>`.
impl fmt::Debug for TurnRouteReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnRouteReceipt")
            .field("provider", &self.identity.provider)
            .field("provider_identity", &self.identity.key)
            .field("wire_model", &self.wire_model)
            .field("endpoint_identity", &self.endpoint_identity)
            .field("credential_generation", &self.credential_generation)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{CredentialGeneration, ProviderKind, TurnRouteReceipt, endpoint_identity};

    const USERINFO_URL: &str = "https://svc-user:hunter2@api.example.com/v1?api_key=sk-live-abc123\
                                &token=tok-secret-xyz&region=us-east";

    #[test]
    fn endpoint_identity_masks_userinfo_and_sensitive_query_values() {
        let identity = endpoint_identity(USERINFO_URL);

        for secret in ["svc-user", "hunter2", "sk-live-abc123", "tok-secret-xyz"] {
            assert!(
                !identity.contains(secret),
                "endpoint identity leaked {secret}: {identity}"
            );
        }
        // …and it is still a useful endpoint identity.
        assert!(identity.contains("api.example.com"), "{identity}");
        assert!(identity.contains("/v1"), "{identity}");
        assert!(identity.contains("region=us-east"), "{identity}");
    }

    #[test]
    fn endpoint_identity_folds_only_trailing_slashes() {
        assert_eq!(
            endpoint_identity("https://api.deepseek.com/v1/"),
            endpoint_identity("  https://api.deepseek.com/v1  ")
        );
        assert_ne!(
            endpoint_identity("https://api.deepseek.com/v1"),
            endpoint_identity("https://api.deepseek.com/v2")
        );
        assert_ne!(
            endpoint_identity("https://api.deepseek.com/v1"),
            endpoint_identity("https://exfil.example.com/v1")
        );
        assert_ne!(
            endpoint_identity("https://api.deepseek.com/v1"),
            endpoint_identity("https://api.deepseek.com:8443/v1")
        );
    }

    #[test]
    fn unparseable_endpoints_never_render_a_path() {
        let identity = endpoint_identity("/Users/someone/secret-project/socket");
        assert!(!identity.contains("someone"), "{identity}");
        assert!(!identity.contains("secret-project"), "{identity}");
        assert_eq!(identity, "<opaque-endpoint>");
    }

    #[test]
    fn debug_never_renders_credential_material() {
        let receipt = TurnRouteReceipt::new(
            ProviderKind::Deepseek,
            "deepseek",
            "deepseek-chat",
            USERINFO_URL,
            "sk-deepseek-secret",
        );

        for rendered in [
            format!("{receipt:?}"),
            format!("{receipt:#?}"),
            format!("{:?}", receipt.credential_generation()),
        ] {
            for secret in [
                "sk-deepseek-secret",
                "hunter2",
                "sk-live-abc123",
                "tok-secret-xyz",
            ] {
                assert!(
                    !rendered.contains(secret),
                    "Debug leaked {secret}: {rendered}"
                );
            }
        }
        let rendered = format!("{receipt:?}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("api.example.com"), "{rendered}");
        assert!(rendered.contains("deepseek-chat"), "{rendered}");
    }

    #[test]
    fn credential_generation_separates_endpoint_from_credential() {
        // Length prefixing: no byte can be moved across the field boundary to
        // forge a matching generation.
        assert_ne!(
            CredentialGeneration::derive("https://host/v1a", "bc"),
            CredentialGeneration::derive("https://host/v1", "abc")
        );
    }

    #[test]
    fn matches_live_route_detects_userinfo_swap_behind_identical_redaction() {
        let receipt = TurnRouteReceipt::new(
            ProviderKind::Deepseek,
            "deepseek",
            "deepseek-chat",
            "https://svc:original@api.deepseek.com/v1",
            "sk-key",
        );
        // Same redacted endpoint identity, different real credentials in the
        // URL. Redaction alone would call this a match; the generation digest
        // does not.
        assert_eq!(
            endpoint_identity("https://svc:rotated@api.deepseek.com/v1"),
            receipt.endpoint_identity()
        );
        assert!(!receipt.matches_live_route("https://svc:rotated@api.deepseek.com/v1", "sk-key"));
        assert!(receipt.matches_live_route("https://svc:original@api.deepseek.com/v1", "sk-key"));
        assert!(
            !receipt.matches_live_route("https://svc:original@api.deepseek.com/v1", "sk-other")
        );
    }
}
