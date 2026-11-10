//! Built-in provider metadata.
//!
//! This module is a metadata foundation for collapsing provider drift over
//! time. It deliberately does not mutate request bodies or choose fallback
//! providers; `ConfigToml::resolve_runtime_options` now mints the executable
//! route through `RouteResolver` (Phase 1). Auth/key resolution stays here.

use crate::ProviderKind;
pub use crate::descriptors::defaults::{
    CODEWHALE_API_BASE_ENV, CODEWHALE_API_KEY_URL, KIMI_CODE_MEMBERSHIP_PLAN_CONSOLE_URL,
    OLLAMA_CLOUD_API_KEY_URL, OLLAMA_CLOUD_BASE_URL, OPENAI_DEFAULT_MODEL,
};
use crate::descriptors::{self, BuiltinProviderDescriptor};
use std::sync::OnceLock;

/// Wire protocol spoken by a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireFormat {
    /// OpenAI-compatible `/v1/chat/completions` style payloads.
    ChatCompletions,
    /// OpenAI Responses API (`/responses`).
    Responses,
    /// Native Anthropic Messages API (`/v1/messages`).
    AnthropicMessages,
}

/// How a user obtains or supplies credentials for a built-in provider.
///
/// Keeping this typed prevents API-key onboarding from accidentally describing
/// a local runtime, OAuth-only route, or user-defined endpoint as though it had
/// a vendor key console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialAcquisition {
    /// A provider-issued API key or access token.
    ApiKey,
    /// Either a provider-issued API key or the provider's supported OAuth path.
    ApiKeyOrOAuth,
    /// A self-hosted route that is keyless by default but can be configured with auth.
    LocalOptional,
    /// An OAuth-only route; Codewhale does not collect an API key for it.
    OAuth,
    /// A user-defined route whose credential source belongs in configuration.
    Configuration,
}

impl CredentialAcquisition {
    /// Stable machine-readable label for diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::ApiKeyOrOAuth => "api_key_or_oauth",
            Self::LocalOptional => "local_optional",
            Self::OAuth => "oauth",
            Self::Configuration => "configuration",
        }
    }
}

/// How a provider selects its request wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WirePolicy {
    /// Every model served by the provider uses the same wire format.
    Fixed(WireFormat),
    /// The provider catalog selects a wire format per model/endpoint.
    ModelAware,
}

impl WirePolicy {
    /// Return the fixed format, or `None` for model-aware providers.
    #[must_use]
    pub const fn fixed(self) -> Option<WireFormat> {
        match self {
            Self::Fixed(format) => Some(format),
            Self::ModelAware => None,
        }
    }

    /// Resolve a concrete format from an offering endpoint key.
    #[must_use]
    pub fn resolve(self, endpoint_key: &str) -> Option<WireFormat> {
        if let Self::Fixed(format) = self {
            return Some(format);
        }

        match endpoint_key.trim().to_ascii_lowercase().as_str() {
            "chat" | "chat_completions" | "chat-completions" => Some(WireFormat::ChatCompletions),
            "responses" => Some(WireFormat::Responses),
            "messages" | "anthropic_messages" | "anthropic-messages" => {
                Some(WireFormat::AnthropicMessages)
            }
            _ => None,
        }
    }
}

/// Canonical, non-secret help for configuring one provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialHelp {
    pub acquisition: CredentialAcquisition,
    /// Stable provider-owned page for creating or locating credentials.
    ///
    /// `None` is deliberate for local, OAuth-only, and user-defined routes; UI
    /// callers must show [`Self::guidance`] instead of guessing a URL.
    pub credential_url: Option<&'static str>,
    /// Provider-owned documentation when the repository already has a stable link.
    pub docs_url: Option<&'static str>,
    /// Concise fallback or qualification for non-key and mixed-auth routes.
    pub guidance: &'static str,
}

/// Resolve the Codewhale API base URL from the environment.
///
/// Returns `None` when the variable is unset, empty, or names an origin this
/// route refuses to send a `cwc_key_…` bearer to. A bearer token has no replay
/// protection, so cleartext is allowed only on loopback — the same rule the
/// account control plane applies to `CODEWHALE_CLOUD_API_BASE`.
#[must_use]
pub fn codewhale_api_base_from_env() -> Option<String> {
    let raw = std::env::var(CODEWHALE_API_BASE_ENV).ok()?;
    codewhale_api_base(&raw)
}

/// Validate one candidate Codewhale API base URL. See [`codewhale_api_base_from_env`].
#[must_use]
pub fn codewhale_api_base(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let (scheme, host, has_credentials) = crate::device_code::url_scheme_and_host(trimmed).ok()?;
    if has_credentials {
        return None;
    }
    let allowed =
        scheme == "https" || (scheme == "http" && crate::device_code::is_loopback_host(&host));
    allowed.then(|| trimmed.to_string())
}

/// Static metadata for a built-in model provider.
pub trait Provider: Send + Sync {
    /// Provider enum variant represented by this entry.
    fn kind(&self) -> ProviderKind;

    /// Canonical provider identifier.
    fn id(&self) -> &'static str {
        self.kind().as_str()
    }

    /// Human-readable provider label for UIs and diagnostics.
    fn display_name(&self) -> &'static str;

    /// Default base URL used when no config/env/CLI override is present.
    fn default_base_url(&self) -> &'static str;

    /// Default model used when no config/env/CLI override is present.
    fn default_model(&self) -> &'static str;

    /// Environment variable candidates used for this provider's API key.
    fn env_vars(&self) -> &'static [&'static str];

    /// TOML table key under `[providers.<key>]`.
    fn provider_config_key(&self) -> &'static str;

    /// Alternate names accepted during provider resolution.
    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// Policy used to select the request wire format.
    fn wire_policy(&self) -> WirePolicy {
        WirePolicy::Fixed(WireFormat::ChatCompletions)
    }

    /// Credential acquisition metadata shared by onboarding, setup, diagnostics,
    /// and provider-help surfaces.
    fn credential_help(&self) -> CredentialHelp {
        credential_help(self.kind())
    }
}

/// Return the canonical credential-acquisition metadata for a provider kind.
///
/// URLs here are provider-owned links already documented in this repository.
/// If no stable vendor credential page is known, the URL remains absent and the
/// guidance explains the supported local, OAuth, or configuration path.
/// This is provider-level fallback metadata: callers that know a concrete base
/// URL must use [`credential_help_for_route`] so route-owned credentials do not
/// inherit a default endpoint's console.
#[must_use]
pub const fn credential_help(kind: ProviderKind) -> CredentialHelp {
    descriptors::builtin_provider_descriptor(kind).credential_help
}

fn is_exact_https_route(base_url: &str, expected_authority: &str, expected_path: &str) -> bool {
    // URL schemes and host names are ASCII case-insensitive; paths are not.
    // Do not lowercase the whole URL here: a differently-cased path is a
    // neighboring route, not the official endpoint. Keep this intentionally
    // dependency-free because provider metadata is used by low-level config
    // callers that should not need URL parsing machinery just for this guard.
    let trimmed = base_url.trim();
    let normalized = trimmed.strip_suffix('/').unwrap_or(trimmed);
    let Some((scheme, authority_and_path)) = normalized.split_once("://") else {
        return false;
    };
    let Some((authority, path)) = authority_and_path.split_once('/') else {
        return false;
    };

    scheme.eq_ignore_ascii_case("https")
        && authority.eq_ignore_ascii_case(expected_authority)
        && path == expected_path
}

/// Whether a configured route is exactly the official Kimi Code endpoint.
///
/// A trailing slash is insignificant, but neighboring Kimi-hosted paths must
/// not inherit membership-plan credentials merely because they share a host.
#[must_use]
pub fn is_exact_kimi_code_route(kind: ProviderKind, base_url: &str) -> bool {
    if kind != ProviderKind::Moonshot {
        return false;
    }

    is_exact_https_route(base_url, "api.kimi.com", "coding/v1")
}

/// Whether a configured Ollama route is exactly the hosted OpenAI-compatible
/// endpoint.
///
/// Local Ollama remains keyless. Neighboring paths, HTTP downgrades, and
/// lookalike hosts remain custom routes so they cannot inherit an Ollama Cloud
/// credential or durable secret-store slot.
#[must_use]
pub fn is_exact_ollama_cloud_route(kind: ProviderKind, base_url: &str) -> bool {
    matches!(kind, ProviderKind::Ollama | ProviderKind::OllamaCloud)
        && is_exact_https_route(base_url, "ollama.com", "v1")
}

/// In-memory compatibility classifier for the released route-sensitive shape.
///
/// Only the old `ollama` identity at the exact hosted endpoint migrates. This
/// deliberately rejects neighboring paths, HTTP downgrades, and lookalike
/// hosts so no local/custom route can consume Ollama Cloud credentials.
#[must_use]
pub fn migrates_legacy_ollama_cloud_route(kind: ProviderKind, base_url: &str) -> bool {
    kind == ProviderKind::Ollama && is_exact_ollama_cloud_route(kind, base_url)
}

/// Whether a configured route is exactly Moonshot's direct API endpoint.
///
/// Direct K3 owns a different reasoning-control dialect from the Kimi Code
/// membership endpoint. Keep this route guard exact so custom gateways and
/// neighboring Moonshot paths do not inherit direct-K3 wire semantics.
#[must_use]
pub fn is_exact_moonshot_platform_route(kind: ProviderKind, base_url: &str) -> bool {
    kind == ProviderKind::Moonshot
        && (is_exact_https_route(base_url, "api.moonshot.ai", "v1")
            || is_exact_https_route(base_url, "api.moonshot.cn", "v1"))
}

/// Whether a configured route is exactly xAI's first-party OpenAI-compatible
/// API endpoint.
///
/// Grok-specific request fields must not leak to a custom compatible gateway
/// merely because the operator selected the `xai` provider identity.
#[must_use]
pub fn is_exact_xai_platform_route(kind: ProviderKind, base_url: &str) -> bool {
    kind == ProviderKind::Xai && is_exact_https_route(base_url, "api.x.ai", "v1")
}

/// Whether a configured route is one of Z.ai's exact first-party Chat
/// Completions endpoints.
///
/// Z.ai-only request fields must not leak to compatible gateways merely
/// because they expose the same model id. Both api.z.ai products (Coding
/// Plan and general platform) and BigModel's general platform endpoint are
/// first-party: `open.bigmodel.cn/api/paas/v4` is the same open platform
/// whose docs prescribe the same `thinking` / `reasoning_effort` dialect
/// (including the forced-thinking GLM-5.3 family), and the bundled catalog
/// already lists it as the Z.ai catalog API. Neighboring paths — including
/// BigModel's `/preview` — remain distinct, mirroring the web-search and
/// official-endpoint families.
#[must_use]
pub fn is_exact_zai_chat_route(kind: ProviderKind, base_url: &str) -> bool {
    kind == ProviderKind::Zai
        && (is_exact_https_route(base_url, "api.z.ai", "api/coding/paas/v4")
            || is_exact_https_route(base_url, "api.z.ai", "api/paas/v4")
            || is_exact_https_route(base_url, "open.bigmodel.cn", "api/paas/v4"))
}

/// Whether a configured route is one of MiniMax's exact first-party OpenAI
/// Chat Completions endpoints.
///
/// This deliberately excludes the `/anthropic` routes: those use the native
/// Messages adapter and do not share Chat Completions token-limit fields.
#[must_use]
pub fn is_exact_minimax_chat_route(kind: ProviderKind, base_url: &str) -> bool {
    kind == ProviderKind::Minimax
        && (is_exact_https_route(base_url, "api.minimax.io", "v1")
            || is_exact_https_route(base_url, "api.minimaxi.com", "v1"))
}

/// Whether a configured route is one of MiniMax's exact first-party
/// Anthropic-compatible Messages endpoints.
///
/// M3 exposes only adaptive/disabled thinking on these routes; it does not
/// expose distinct effort tiers. Keep the guard exact so a compatible gateway
/// cannot inherit first-party effective-state claims from its provider label.
#[must_use]
pub fn is_exact_minimax_anthropic_route(kind: ProviderKind, base_url: &str) -> bool {
    kind == ProviderKind::MinimaxAnthropic
        && (is_exact_https_route(base_url, "api.minimax.io", "anthropic")
            || is_exact_https_route(base_url, "api.minimaxi.com", "anthropic"))
}

/// Whether a configured route is exactly CSDN 星图's official OpenAI-compatible
/// platform endpoint.
///
/// Coding Plan keys and general marketplace keys share this one endpoint, so
/// the URL proves neither product — only that the route is first-party.
/// Neighboring paths, HTTP downgrades, and lookalike hosts must not inherit
/// CSDN billing or wire semantics.
#[must_use]
pub fn is_exact_csdn_platform_route(kind: ProviderKind, base_url: &str) -> bool {
    kind == ProviderKind::Csdn && is_exact_https_route(base_url, "ai.csdn.net", "api/model/v1")
}

/// Return credential help for one concrete provider route.
///
/// This protects non-UI callers such as diagnostics and command surfaces from
/// presenting Moonshot's direct API console for a Kimi Code membership-plan
/// endpoint. It performs no discovery, credential lookup, or network I/O.
#[must_use]
pub fn credential_help_for_route(kind: ProviderKind, base_url: &str) -> CredentialHelp {
    if is_exact_ollama_cloud_route(kind, base_url) {
        return descriptors::OLLAMA_CLOUD_CREDENTIAL_HELP;
    }
    if is_exact_kimi_code_route(kind, base_url) {
        return descriptors::KIMI_CODE_CREDENTIAL_HELP;
    }
    credential_help(kind)
}

impl Provider for BuiltinProviderDescriptor {
    fn kind(&self) -> ProviderKind {
        self.kind
    }
    fn id(&self) -> &'static str {
        self.id
    }
    fn display_name(&self) -> &'static str {
        self.label
    }
    fn default_base_url(&self) -> &'static str {
        self.base_url
    }
    fn default_model(&self) -> &'static str {
        self.default_model
    }
    fn env_vars(&self) -> &'static [&'static str] {
        self.env_vars
    }
    fn provider_config_key(&self) -> &'static str {
        self.config_key
    }
    fn aliases(&self) -> &'static [&'static str] {
        self.aliases
    }
    fn wire_policy(&self) -> WirePolicy {
        self.wire_policy
    }
    fn credential_help(&self) -> CredentialHelp {
        self.credential_help
    }
}

static PROVIDER_REGISTRY: OnceLock<Vec<&'static dyn Provider>> = OnceLock::new();

/// Return all built-in and legacy provider metadata entries.
///
/// The full registry retains legacy entries needed to read old configuration.
/// It is intentionally NOT a user-facing provider list; for browsing/picker
/// surfaces use [`providers_sorted_for_display`].
#[must_use]
pub fn all_providers() -> &'static [&'static dyn Provider] {
    PROVIDER_REGISTRY
        .get_or_init(|| {
            descriptors::BUILTIN_DESCRIPTORS
                .iter()
                .map(|row| row as &dyn Provider)
                .collect()
        })
        .as_slice()
}

/// Return all built-in providers ordered for user-facing display.
///
/// Providers are sorted alphabetically (case-insensitively) by
/// [`Provider::display_name`] so model/provider browsing surfaces present a
/// neutral, predictable list rather than leading with whichever provider
/// happens to sit first in [`ProviderKind::ALL`] (historically DeepSeek). The
/// ordering policy intentionally differs from internal parsing/default order:
///
/// - [`all_providers`] — full compatibility registry for internal identity
///   matching, including legacy entries.
/// - [`ProviderKind::ALL`] — stable selectable catalog order. Do not reorder.
/// - [`providers_sorted_for_display`] — neutral alphabetical order for UI
///   browsing, with legacy tombstones omitted. DeepSeek stays present and
///   searchable but is not hard-coded first; a caller may still highlight/pin
///   the active provider separately.
///
/// Returns an owned `Vec` because the sorted order is computed, not static.
#[must_use]
pub fn providers_sorted_for_display() -> Vec<&'static dyn Provider> {
    let mut providers: Vec<_> = all_providers()
        .iter()
        .copied()
        .filter(|provider| !descriptors::builtin_provider_descriptor(provider.kind()).retired)
        .collect();
    providers.sort_by(|a, b| {
        a.display_name()
            .to_ascii_lowercase()
            .cmp(&b.display_name().to_ascii_lowercase())
    });
    providers
}

/// Find a provider by canonical id only.
#[must_use]
pub fn lookup_provider(id: &str) -> Option<&'static dyn Provider> {
    let id = id.trim();
    all_providers()
        .iter()
        .copied()
        .find(|provider| provider.id() == id)
}

/// Resolve a provider by canonical id or supported legacy alias.
#[must_use]
pub fn resolve_provider(id_or_alias: &str) -> Option<&'static dyn Provider> {
    ProviderKind::parse(id_or_alias).map(provider_for_kind)
}

/// Return metadata for a known provider kind.
#[must_use]
pub fn provider_for_kind(kind: ProviderKind) -> &'static dyn Provider {
    descriptors::builtin_provider_descriptor(kind)
}

/// Whether a per-config `wire` dialect string names the Anthropic Messages
/// endpoint. Canonical parse shared by the tui wire-format/capability readers
/// and the route resolver's `RouteRequest::wire_override`, so one alias list
/// cannot drift from another.
#[must_use]
pub fn wire_dialect_prefers_anthropic(wire: Option<&str>) -> bool {
    let Some(raw) = wire.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let normalized = raw.to_ascii_lowercase().replace(['_', ' '], "-");
    matches!(
        normalized.as_str(),
        "anthropic"
            | "anthropic-messages"
            | "messages"
            | "claude"
            | "anthropic-compatible"
            | "anthropic-compat"
    )
}

/// Whether a per-config `wire` dialect string names the OpenAI Responses
/// endpoint. See [`wire_dialect_prefers_anthropic`] for the sharing contract.
#[must_use]
pub fn wire_dialect_prefers_responses(wire: Option<&str>) -> bool {
    let Some(raw) = wire.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let normalized = raw.to_ascii_lowercase().replace(['_', ' '], "-");
    matches!(
        normalized.as_str(),
        "responses"
            | "responses-api"
            | "openai-responses"
            | "openai-responses-api"
            | "response"
            | "response-api"
            | "openai-responses-compat"
            | "responses-compat"
    )
}

/// The wire override a per-config `wire` dialect asks for: `Some(Responses)` /
/// `Some(AnthropicMessages)` when the string names that endpoint, `None` for
/// `chat` / absent / unrecognized values (the static descriptor policy
/// applies). The resolver honors the override only for `ProviderKind::Custom`,
/// and every other consumer gates the same way, so the warning below can only
/// fire for a custom table — `table` names it in the log, since several
/// tables can coexist and the typo must be locatable. A non-empty
/// unrecognized value is most likely a typo of the one string that switches
/// the endpoint's protocol, so it is logged before degrading to the default
/// policy — the parse stays total and forward-compatible.
#[must_use]
pub fn wire_dialect_override(table: &str, wire: Option<&str>) -> Option<WireFormat> {
    if wire_dialect_prefers_responses(wire) {
        Some(WireFormat::Responses)
    } else if wire_dialect_prefers_anthropic(wire) {
        Some(WireFormat::AnthropicMessages)
    } else {
        if let Some(raw) = wire.map(str::trim).filter(|value| !value.is_empty()) {
            let normalized = raw.to_ascii_lowercase().replace(['_', ' '], "-");
            if !matches!(
                normalized.as_str(),
                "chat" | "chat-completions" | "openai" | "openai-chat" | "openai-chat-completions"
            ) {
                tracing::warn!(
                    table = %table,
                    dialect = %raw,
                    "unrecognized custom-provider wire dialect; using the default Chat Completions policy"
                );
            }
        }
        None
    }
}

/// The explicit Responses profile a custom table may request.
///
/// An empty value selects nothing, so an existing `wire = "responses"` table
/// keeps the default profile. A set value is rejected unless this is a custom
/// provider whose wire dialect is Responses and the id is on the custom
/// allowlist. Built-in profiles stay on their route bindings.
pub fn responses_profile_override(
    kind: crate::ProviderKind,
    table: &str,
    wire: Option<&str>,
    profile: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(profile) = profile.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if kind != crate::ProviderKind::Custom {
        return Err(format!("provider {table} cannot set responses_profile"));
    }
    if !wire_dialect_prefers_responses(wire) {
        return Err(format!(
            "provider {table} responses_profile `{profile}` requires wire = \"responses\""
        ));
    }
    crate::responses_dialect::accept_custom_responses_profile(profile)
        .map_err(|err| format!("provider {table}: {err}"))?;
    Ok(Some(profile.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptors::defaults::*;

    #[test]
    fn responses_profile_override_rejects_the_wrong_wire_and_identity() {
        assert_eq!(
            responses_profile_override(
                crate::ProviderKind::Custom,
                "gateway",
                Some("responses"),
                None
            )
            .expect("absent"),
            None
        );
        assert_eq!(
            responses_profile_override(
                crate::ProviderKind::Custom,
                "gateway",
                Some("responses"),
                Some(" standard "),
            )
            .expect("standard"),
            Some("standard".to_string())
        );
        for (kind, wire, profile, needle) in [
            (
                crate::ProviderKind::Openai,
                Some("responses"),
                Some("standard"),
                "cannot set",
            ),
            (
                crate::ProviderKind::Custom,
                Some("chat"),
                Some("standard"),
                "requires wire",
            ),
            (
                crate::ProviderKind::Custom,
                Some("anthropic"),
                Some("standard"),
                "requires wire",
            ),
            (
                crate::ProviderKind::Custom,
                Some("responses"),
                Some("codex"),
                "allowed: standard",
            ),
            (
                crate::ProviderKind::Custom,
                Some("responses"),
                Some("missing"),
                "unknown responses profile",
            ),
        ] {
            let error = responses_profile_override(kind, "gateway", wire, profile)
                .expect_err("rejected profile");
            assert!(error.contains(needle), "{error}");
            assert!(error.contains("gateway"), "{error}");
        }
    }

    #[test]
    fn wire_dialect_override_parses_the_canonical_alias_sets() {
        // The exact alias sets are the cross-crate contract: the tui route
        // layer, the ambient client, the capability reporter, and the route
        // resolver all resolve dialects through these two lists.
        for dialect in [
            "responses",
            "responses-api",
            "openai-responses",
            "openai-responses-api",
            "response",
            "response-api",
            "openai-responses-compat",
            "responses-compat",
        ] {
            assert_eq!(
                wire_dialect_override("test", Some(dialect)),
                Some(WireFormat::Responses),
                "{dialect} must parse as the Responses dialect"
            );
        }
        for dialect in [
            "anthropic",
            "anthropic-messages",
            "messages",
            "claude",
            "anthropic-compatible",
            "anthropic-compat",
        ] {
            assert_eq!(
                wire_dialect_override("test", Some(dialect)),
                Some(WireFormat::AnthropicMessages),
                "{dialect} must parse as the Anthropic Messages dialect"
            );
        }
    }

    #[test]
    fn wire_dialect_override_normalizes_case_whitespace_and_separators() {
        for dialect in ["OpenAI_Responses", " responses-api ", "ANTHROPIC COMPAT"] {
            let parsed = wire_dialect_override("test", Some(dialect))
                .unwrap_or_else(|| panic!("{dialect} must parse"));
            assert_ne!(
                parsed,
                WireFormat::ChatCompletions,
                "{dialect} must not degrade to the default policy"
            );
        }
    }

    #[test]
    fn wire_dialect_override_defaults_chat_and_degrades_unknowns() {
        assert_eq!(wire_dialect_override("test", None), None);
        assert_eq!(wire_dialect_override("test", Some("")), None);
        for chat in [
            "chat",
            "chat-completions",
            "openai",
            "openai-chat",
            "openai-chat-completions",
        ] {
            assert_eq!(
                wire_dialect_override("test", Some(chat)),
                None,
                "an explicit chat dialect keeps the default policy"
            );
        }
        // A typo must not accidentally select a protocol; the static
        // descriptor policy applies and the warn names the table.
        assert_eq!(wire_dialect_override("my_table", Some("respones")), None);
    }

    #[test]
    fn credential_help_covers_every_provider_without_guessing_non_key_urls() {
        for provider in all_providers() {
            let help = provider.credential_help();
            assert!(
                !help.guidance.trim().is_empty(),
                "{} credential guidance must not be empty",
                provider.id()
            );

            match help.acquisition {
                CredentialAcquisition::ApiKey | CredentialAcquisition::ApiKeyOrOAuth => {
                    assert!(
                        help.credential_url.is_some(),
                        "{} needs a stable provider-owned credential link",
                        provider.id()
                    );
                }
                CredentialAcquisition::LocalOptional
                | CredentialAcquisition::OAuth
                | CredentialAcquisition::Configuration => assert!(
                    help.credential_url.is_none(),
                    "{} must explain its non-key route instead of inventing a credential link",
                    provider.id()
                ),
            }
        }
    }

    #[test]
    fn kimi_credential_help_uses_the_durable_api_key_console_only() {
        let help = provider_for_kind(ProviderKind::Moonshot).credential_help();

        assert_eq!(help.acquisition, CredentialAcquisition::ApiKey);
        assert_eq!(
            help.credential_url,
            Some("https://platform.kimi.ai/console/api-keys")
        );
        assert_eq!(
            help.docs_url,
            Some("https://platform.kimi.ai/docs/overview")
        );
        assert!(help.guidance.contains("create and copy an API key"));
        assert!(help.guidance.contains("OAuth is not available"));
    }

    #[test]
    fn kimi_code_route_credential_help_is_distinct_from_direct_moonshot() {
        let direct = credential_help_for_route(ProviderKind::Moonshot, DEFAULT_MOONSHOT_BASE_URL);
        let kimi_code =
            credential_help_for_route(ProviderKind::Moonshot, "https://api.kimi.com/coding/v1/");

        assert_eq!(
            direct.credential_url,
            Some("https://platform.kimi.ai/console/api-keys")
        );
        assert_eq!(
            kimi_code.credential_url,
            Some(KIMI_CODE_MEMBERSHIP_PLAN_CONSOLE_URL)
        );
        assert_eq!(kimi_code.docs_url, None);
        assert!(kimi_code.guidance.contains("membership-plan API key"));
        assert!(
            kimi_code
                .guidance
                .contains("does not import Kimi CLI credentials")
        );
        assert!(!is_exact_kimi_code_route(
            ProviderKind::Moonshot,
            "https://api.kimi.com/coding/v1/preview"
        ));

        // Scheme and hostname casing are insignificant, but the endpoint
        // path is a route identifier and must remain exact.
        assert!(is_exact_kimi_code_route(
            ProviderKind::Moonshot,
            "HTTPS://API.KIMI.COM/coding/v1/"
        ));
        for neighboring_route in [
            "https://api.kimi.com/CODING/v1",
            "https://api.kimi.com/coding/V1",
            "http://api.kimi.com/coding/v1",
            "https://api.kimi.com:443/coding/v1",
            "https://api.kimi.com/coding/v1?preview=1",
            "https://api.kimi.com/coding/v1#fragment",
            "https://api.kimi.com/coding/v1//",
        ] {
            assert!(
                !is_exact_kimi_code_route(ProviderKind::Moonshot, neighboring_route),
                "{neighboring_route} must not inherit Kimi Code membership semantics"
            );
        }
    }

    #[test]
    fn ollama_cloud_route_is_exact_and_requires_its_own_key() {
        for base_url in [
            OLLAMA_CLOUD_BASE_URL,
            "https://ollama.com/v1/",
            "  HTTPS://OLLAMA.COM/v1/  ",
        ] {
            for provider in [ProviderKind::Ollama, ProviderKind::OllamaCloud] {
                assert!(is_exact_ollama_cloud_route(provider, base_url));
                let help = credential_help_for_route(provider, base_url);
                assert_eq!(help.acquisition, CredentialAcquisition::ApiKey);
                assert_eq!(help.credential_url, Some(OLLAMA_CLOUD_API_KEY_URL));
                assert_eq!(
                    help.docs_url,
                    Some("https://docs.ollama.com/api/authentication")
                );
                assert!(help.guidance.contains("OLLAMA_CLOUD_API_KEY"));
                assert!(help.guidance.contains("OLLAMA_API_KEY"));
            }
        }

        for base_url in [
            "http://ollama.com/v1",
            "https://ollama.com",
            "https://ollama.com/api",
            "https://ollama.com/v1/preview",
            "https://ollama.com.evil.example/v1",
            "https://api.ollama.com/v1",
            "https://ollama.com/v1?tenant=other",
        ] {
            assert!(!is_exact_ollama_cloud_route(ProviderKind::Ollama, base_url));
            assert!(!is_exact_ollama_cloud_route(
                ProviderKind::OllamaCloud,
                base_url
            ));
        }
        assert!(!is_exact_ollama_cloud_route(
            ProviderKind::Openai,
            OLLAMA_CLOUD_BASE_URL
        ));

        let local = credential_help_for_route(ProviderKind::Ollama, DEFAULT_OLLAMA_BASE_URL);
        assert_eq!(local.acquisition, CredentialAcquisition::LocalOptional);
        assert_eq!(local.credential_url, None);
        assert!(local.guidance.contains("keyless by default"));
    }

    #[test]
    fn direct_moonshot_route_matching_is_exact() {
        for route in ["HTTPS://API.MOONSHOT.AI/v1/", "HTTPS://API.MOONSHOT.CN/v1/"] {
            assert!(is_exact_moonshot_platform_route(
                ProviderKind::Moonshot,
                route
            ));
        }
        for neighboring_route in [
            "https://api.moonshot.ai/V1",
            "http://api.moonshot.ai/v1",
            "https://api.moonshot.ai:443/v1",
            "https://api.moonshot.ai/v1?preview=1",
            "https://api.moonshot.ai/v1#fragment",
            "https://api.moonshot.ai/v1//",
            "https://api.moonshot.ai/v1/chat/completions",
            "https://api.moonshot.cn/v1/chat/completions",
            "https://api.kimi.com/coding/v1",
        ] {
            assert!(
                !is_exact_moonshot_platform_route(ProviderKind::Moonshot, neighboring_route),
                "{neighboring_route} must not inherit direct Moonshot semantics"
            );
        }
        assert!(!is_exact_moonshot_platform_route(
            ProviderKind::Openai,
            crate::MOONSHOT_CN_BASE_URL
        ));
    }

    #[test]
    fn direct_xai_route_matching_is_exact() {
        assert!(is_exact_xai_platform_route(
            ProviderKind::Xai,
            "HTTPS://API.X.AI/v1/"
        ));
        for neighboring_route in [
            "https://api.x.ai/V1",
            "http://api.x.ai/v1",
            "https://api.x.ai:443/v1",
            "https://api.x.ai/v1?preview=1",
            "https://api.x.ai/v1#fragment",
            "https://api.x.ai/v1//",
            "https://api.x.ai/v1/chat/completions",
            "https://gateway.example/v1",
        ] {
            assert!(
                !is_exact_xai_platform_route(ProviderKind::Xai, neighboring_route),
                "{neighboring_route} must not inherit xAI-only request fields"
            );
        }
        assert!(!is_exact_xai_platform_route(
            ProviderKind::Openai,
            DEFAULT_XAI_BASE_URL
        ));
    }

    #[test]
    fn zai_chat_route_matching_is_exact() {
        for route in [
            "https://api.z.ai/api/coding/paas/v4",
            "https://api.z.ai/api/paas/v4/",
            "HTTPS://API.Z.AI/api/paas/v4",
            // BigModel's general platform endpoint is the same first-party
            // open platform; authority case stays insignificant.
            "https://open.bigmodel.cn/api/paas/v4",
            "https://open.bigmodel.cn/api/paas/v4/",
            "HTTPS://OPEN.BIGMODEL.CN/api/paas/v4",
        ] {
            assert!(is_exact_zai_chat_route(ProviderKind::Zai, route), "{route}");
        }
        for neighboring_route in [
            "http://api.z.ai/api/paas/v4",
            "https://api.z.ai:443/api/paas/v4",
            "https://api.z.ai/API/paas/v4",
            "https://api.z.ai/api/paas/v4?preview=1",
            "https://api.z.ai/api/paas/v4#fragment",
            "https://api.z.ai/api/paas/v4//",
            "https://api.z.ai/api/paas/v4/chat/completions",
            // BigModel neighbors: the undocumented coding path and the
            // preview product stay fail-closed, like the official-endpoint
            // and web-search families.
            "https://open.bigmodel.cn/api/paas/v4/preview",
            "https://open.bigmodel.cn/api/coding/paas/v4",
            "http://open.bigmodel.cn/api/paas/v4",
            "https://open.bigmodel.cn/API/paas/v4",
            "https://gateway.example/v1",
        ] {
            assert!(
                !is_exact_zai_chat_route(ProviderKind::Zai, neighboring_route),
                "{neighboring_route} must not inherit Z.ai-only request fields"
            );
        }
        assert!(!is_exact_zai_chat_route(
            ProviderKind::Openai,
            DEFAULT_ZAI_BASE_URL
        ));
        assert!(!is_exact_zai_chat_route(
            ProviderKind::Openai,
            "https://open.bigmodel.cn/api/paas/v4"
        ));
    }

    #[test]
    fn minimax_chat_route_matching_is_exact_and_excludes_messages() {
        for route in [
            "https://api.minimax.io/v1",
            "https://api.minimaxi.com/v1/",
            "HTTPS://API.MINIMAX.IO/v1",
        ] {
            assert!(
                is_exact_minimax_chat_route(ProviderKind::Minimax, route),
                "{route}"
            );
        }
        for neighboring_route in [
            "http://api.minimax.io/v1",
            "https://api.minimax.io:443/v1",
            "https://api.minimax.io/V1",
            "https://api.minimax.io/v1?preview=1",
            "https://api.minimax.io/v1#fragment",
            "https://api.minimax.io/v1//",
            "https://api.minimax.io/v1/chat/completions",
            "https://api.minimax.io/anthropic",
            "https://api.minimaxi.com/anthropic",
            "https://gateway.example/v1",
        ] {
            assert!(
                !is_exact_minimax_chat_route(ProviderKind::Minimax, neighboring_route),
                "{neighboring_route} must not inherit MiniMax Chat request fields"
            );
        }
        assert!(!is_exact_minimax_chat_route(
            ProviderKind::MinimaxAnthropic,
            DEFAULT_MINIMAX_BASE_URL
        ));
    }

    #[test]
    fn minimax_anthropic_route_matching_is_exact_and_excludes_chat() {
        for route in [
            "https://api.minimax.io/anthropic",
            "https://api.minimaxi.com/anthropic/",
            "HTTPS://API.MINIMAX.IO/anthropic",
        ] {
            assert!(
                is_exact_minimax_anthropic_route(ProviderKind::MinimaxAnthropic, route),
                "{route}"
            );
        }
        for neighboring_route in [
            "http://api.minimax.io/anthropic",
            "https://api.minimax.io:443/anthropic",
            "https://api.minimax.io/Anthropic",
            "https://api.minimax.io/anthropic?preview=1",
            "https://api.minimax.io/anthropic#fragment",
            "https://api.minimax.io/anthropic//",
            "https://api.minimax.io/anthropic/v1/messages",
            "https://api.minimax.io/v1",
            "https://gateway.example/anthropic",
        ] {
            assert!(
                !is_exact_minimax_anthropic_route(
                    ProviderKind::MinimaxAnthropic,
                    neighboring_route
                ),
                "{neighboring_route} must not inherit MiniMax Messages semantics"
            );
        }
        assert!(!is_exact_minimax_anthropic_route(
            ProviderKind::Minimax,
            DEFAULT_MINIMAX_ANTHROPIC_BASE_URL
        ));
    }

    #[test]
    fn non_key_and_mixed_routes_are_typed_explicitly() {
        for kind in [
            ProviderKind::Sglang,
            ProviderKind::Vllm,
            ProviderKind::Ollama,
        ] {
            assert_eq!(
                provider_for_kind(kind).credential_help().acquisition,
                CredentialAcquisition::LocalOptional
            );
        }
        assert_eq!(
            provider_for_kind(ProviderKind::OpenaiCodex)
                .credential_help()
                .acquisition,
            CredentialAcquisition::OAuth
        );
        assert_eq!(
            provider_for_kind(ProviderKind::Xai)
                .credential_help()
                .acquisition,
            CredentialAcquisition::ApiKeyOrOAuth
        );
        assert_eq!(
            provider_for_kind(ProviderKind::Custom)
                .credential_help()
                .acquisition,
            CredentialAcquisition::Configuration
        );
    }

    #[test]
    fn antigravity_registry_entry_is_a_non_runnable_legacy_tombstone() {
        let legacy = provider_for_kind(ProviderKind::Antigravity);
        assert_eq!(legacy.id(), "antigravity");
        assert!(legacy.env_vars().is_empty());
        assert!(legacy.default_base_url().ends_with(".invalid"));
        assert_eq!(legacy.default_model(), "legacy-antigravity-disabled");

        let help = legacy.credential_help();
        assert_eq!(help.acquisition, CredentialAcquisition::Configuration);
        assert_eq!(help.credential_url, None);
        assert_eq!(help.docs_url, None);
        assert!(
            help.guidance
                .contains("codewhale auth clear --provider antigravity")
        );
        assert!(help.guidance.contains("provider `google`"));
        assert!(help.guidance.contains("GEMINI_API_KEY"));
    }

    #[test]
    fn live_verified_console_replacements_do_not_regress_to_404_links() {
        let openmodel = provider_for_kind(ProviderKind::Openmodel).credential_help();
        assert_eq!(
            openmodel.credential_url,
            Some("https://console.openmodel.ai/")
        );
        assert_eq!(
            openmodel.docs_url,
            Some("https://docs.openmodel.ai/en/docs/getting-started/authentication")
        );

        let sakana = provider_for_kind(ProviderKind::Sakana).credential_help();
        assert_eq!(
            sakana.credential_url,
            Some("https://console.sakana.ai/api-keys")
        );
        assert_eq!(
            sakana.docs_url,
            Some("https://console.sakana.ai/get-started")
        );
    }

    #[test]
    fn model_aware_wire_policy_resolves_only_supported_endpoint_keys() {
        let policy = WirePolicy::ModelAware;
        assert_eq!(policy.resolve("chat"), Some(WireFormat::ChatCompletions));
        assert_eq!(policy.resolve("responses"), Some(WireFormat::Responses));
        assert_eq!(
            policy.resolve("messages"),
            Some(WireFormat::AnthropicMessages)
        );
        assert_eq!(policy.resolve("models/gemini-3.1-pro"), None);
        assert_eq!(policy.resolve(""), None);
    }

    #[test]
    fn fixed_wire_policy_ignores_catalog_endpoint_keys() {
        let policy = WirePolicy::Fixed(WireFormat::Responses);
        assert_eq!(policy.resolve("chat"), Some(WireFormat::Responses));
        assert_eq!(policy.resolve("unknown"), Some(WireFormat::Responses));
    }

    #[test]
    fn display_order_is_alphabetical_by_display_name() {
        let display = providers_sorted_for_display();
        let names: Vec<String> = display
            .iter()
            .map(|p| p.display_name().to_ascii_lowercase())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(
            names, sorted,
            "providers_sorted_for_display must be alphabetical (case-insensitive) by display name"
        );
    }

    #[test]
    fn display_order_differs_from_internal_all_order() {
        // The whole point of the helper is that UI ordering is NOT the
        // internal compatibility-registry insertion order.
        let display_ids: Vec<&str> = providers_sorted_for_display()
            .iter()
            .map(|p| p.id())
            .collect();
        let internal_ids: Vec<&str> = all_providers().iter().map(|p| p.id()).collect();
        assert_ne!(
            display_ids, internal_ids,
            "display order should not match internal ALL order"
        );
    }

    #[test]
    fn display_order_is_complete_and_unique() {
        // Every selectable provider is retained exactly once; legacy
        // configuration tombstones stay in the internal registry only.
        let display = providers_sorted_for_display();
        assert_eq!(
            display.len(),
            all_providers().len() - 1,
            "display order must include every selectable built-in provider"
        );
        assert!(
            all_providers()
                .iter()
                .any(|provider| provider.kind() == ProviderKind::Antigravity),
            "legacy config identity must remain in the internal registry"
        );
        assert!(
            display
                .iter()
                .all(|provider| provider.kind() != ProviderKind::Antigravity),
            "legacy Antigravity tombstone must not appear in provider pickers"
        );
        let mut ids: Vec<&str> = display.iter().map(|p| p.id()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(
            before,
            ids.len(),
            "display order must not contain duplicates"
        );
    }

    #[test]
    fn deepseek_is_present_but_not_first_in_display_order() {
        // Acceptance: DeepSeek stays searchable but is no longer hard-coded
        // first in provider browsing UI. (It is first in internal ALL order.)
        let display = providers_sorted_for_display();
        assert_eq!(
            all_providers()[0].kind(),
            ProviderKind::Deepseek,
            "DeepSeek is expected to remain first in the stable internal order"
        );
        assert!(
            display.iter().any(|p| p.kind() == ProviderKind::Deepseek),
            "DeepSeek must remain present in display order"
        );
        assert_ne!(
            display[0].kind(),
            ProviderKind::Deepseek,
            "DeepSeek must not be hard-coded first in display order"
        );
        // Alibaba Cloud Model Studio sorts before 'Anthropic' and 'DeepSeek'
        // alphabetically, so it is a stable check that the neutral ordering
        // actually took effect.
        assert_eq!(
            display[0].display_name(),
            "Alibaba Cloud Model Studio",
            "alphabetical display order should lead with Alibaba Cloud Model Studio"
        );
    }
}
