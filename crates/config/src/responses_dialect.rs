//! Responses dialect profiles loaded from `assets/responses_dialects.json`.
//!
//! Known limits: selection does not mint a route or build a request. A
//! DeepSeek Responses URL outside the binding's exact list does not take the
//! DeepSeek profile. Custom DeepSeek endpoints are forced to Chat by the route
//! resolver before selection. Replay API scope strings and account identity
//! stay with the caller. All four profiles share one usage policy.

use serde::Serialize;

use crate::ProviderKind;
use crate::provider::WireFormat;

#[cfg(test)]
#[path = "responses_dialect_data.rs"]
mod data;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ResponsesProfileId(&'static str);

impl ResponsesProfileId {
    const fn from_static(id: &'static str) -> Self {
        Self(id)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponsesDialectProfile {
    pub id: ResponsesProfileId,
    pub endpoint: ResponsesEndpointPolicy,
    pub headers: ResponsesHeaderPolicy,
    pub request: ResponsesRequestPolicy,
    pub reasoning: ResponsesReasoningPolicy,
    pub replay: ResponsesReplayPolicy,
    pub tools: ResponsesToolPolicy,
    pub stream: ResponsesStreamPolicy,
    pub usage: ResponsesUsagePolicy,
    pub capabilities: ResponsesCapabilityPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponsesEndpointPolicy {
    pub path: ResponsesPathPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsesPathPolicy {
    VersionedResponses,
    UnversionedResponses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponsesHeaderPolicy {
    pub protocol_headers: ProtocolHeaderPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolHeaderPolicy {
    StandardJson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponsesRequestPolicy {
    pub store: StorePolicy,
    pub include: IncludePolicy,
    pub instructions: InstructionsPlacement,
    pub max_output_tokens: OutputTokenFieldPolicy,
    pub sampling: SamplingPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorePolicy {
    ExplicitFalse,
    Omit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncludePolicy {
    EncryptedReasoning,
    Omit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionsPlacement {
    TopLevel,
    LeadingSystemInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputTokenFieldPolicy {
    WhenPositive,
    Omit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplingPolicy {
    TemperatureAndTopP,
    Omit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponsesReasoningPolicy {
    pub effort_mapping: ReasoningEffortMapping,
    pub shape: ReasoningRequestShape,
    pub summary: ReasoningSummaryPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffortMapping {
    OpenaiCompatible,
    Codex,
    Deepseek,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningRequestShape {
    EffortWithSummary,
    EffortOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningSummaryPolicy {
    Auto,
    Omit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponsesReplayPolicy {
    pub encrypted_state: EncryptedReasoningReplayPolicy,
    pub plain_reasoning: PlainReasoningReplayPolicy,
    pub system_role: SystemRoleReplayPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptedReasoningReplayPolicy {
    ProviderApiModelScoped,
    AccountApiModelScoped,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlainReasoningReplayPolicy {
    Ignore,
    PlainText,
    PlainTextWithToolPlaceholder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemRoleReplayPolicy {
    Preserve,
    SystemToDeveloper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponsesToolPolicy {
    pub envelope: ToolEnvelopePolicy,
    pub namespace: Option<&'static str>,
    pub parallel_calls: ParallelToolCallPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEnvelopePolicy {
    FunctionArray,
    NamespaceWrapped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParallelToolCallPolicy {
    Allow,
    Disallow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponsesStreamPolicy {
    pub malformed_json: MalformedEventPolicy,
    pub unknown_event: UnknownEventPolicy,
    pub done_marker: DoneMarkerPolicy,
    pub completion: CompletionPolicy,
    pub incomplete: IncompleteResponsePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MalformedEventPolicy {
    Fail,
    SkipWithDiagnostic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownEventPolicy {
    Ignore,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoneMarkerPolicy {
    Terminal,
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionPolicy {
    AcceptDocumentedTerminalEvents,
    RequireSuccessfulResponseCompleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncompleteResponsePolicy {
    PreserveProviderIncompleteResult,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponsesUsagePolicy {
    pub cache_fields: CacheUsageFieldPolicy,
    pub reasoning_fields: ReasoningUsageFieldPolicy,
    pub audio_fields: AudioUsageFieldPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheUsageFieldPolicy {
    TopLevelThenNestedFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningUsageFieldPolicy {
    OutputTokensDetails,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioUsageFieldPolicy {
    NestedDetails,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponsesCapabilityPolicy {
    pub function_tools: FunctionToolCapability,
    pub server_side_tools: ServerSideToolCapability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionToolCapability {
    Supported,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerSideToolCapability {
    RouteAndAdapterGated,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponsesProfileBinding {
    pub provider_kind: ProviderKind,
    pub endpoint_key: Option<&'static str>,
    pub base_urls: Option<&'static [&'static str]>,
    pub profile_id: ResponsesProfileId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponsesProfileQuery<'a> {
    pub protocol: WireFormat,
    pub provider: ProviderKind,
    pub endpoint_key: &'a str,
    pub base_url: &'a str,
    pub explicit_profile: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponsesProfileError {
    NotResponses { profile: String },
    UnknownProfile { profile: String, allowed: String },
    ProfileNotAllowed { profile: String, allowed: String },
    BuiltinOverride { provider: ProviderKind },
    AmbiguousBinding { provider: ProviderKind },
}

impl std::fmt::Display for ResponsesProfileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotResponses { profile } => write!(
                formatter,
                "responses profile `{profile}` cannot be set when the wire is not responses"
            ),
            Self::UnknownProfile { profile, allowed } => write!(
                formatter,
                "unknown responses profile `{profile}`; allowed: {allowed}"
            ),
            Self::ProfileNotAllowed { profile, allowed } => write!(
                formatter,
                "responses profile `{profile}` is not allowed for a custom provider; allowed: {allowed}"
            ),
            Self::BuiltinOverride { provider } => write!(
                formatter,
                "built-in provider {} cannot set a responses profile",
                provider.as_str()
            ),
            Self::AmbiguousBinding { provider } => write!(
                formatter,
                "responses profile bindings overlap for {}",
                provider.as_str()
            ),
        }
    }
}

impl std::error::Error for ResponsesProfileError {}

include!(concat!(env!("OUT_DIR"), "/responses_dialects.rs"));

#[must_use]
pub fn builtin_profiles() -> &'static [ResponsesDialectProfile] {
    BUILTIN_PROFILES
}

pub fn profile(id: &str) -> Option<&'static ResponsesDialectProfile> {
    find_profile(id)
}

/// Path policy for a base URL that matches an exact binding. Unmatched URLs
/// stay on the versioned Responses path. Matching strips a trailing slash and
/// lowercases; it does not trim surrounding whitespace.
pub fn bound_responses_path(provider: ProviderKind, base_url: &str) -> ResponsesPathPolicy {
    let normalized = base_url.trim_end_matches('/').to_ascii_lowercase();
    for binding in BUILTIN_BINDINGS {
        if binding.provider_kind != provider {
            continue;
        }
        let Some(urls) = binding.base_urls else {
            continue;
        };
        if !urls.iter().any(|url| *url == normalized) {
            continue;
        }
        if let Some(profile) = find_profile(binding.profile_id.as_str()) {
            return profile.endpoint.path;
        }
    }
    ResponsesPathPolicy::VersionedResponses
}

pub fn accept_custom_responses_profile(
    profile: &str,
) -> Result<&'static ResponsesDialectProfile, ResponsesProfileError> {
    let Some(found) = find_profile(profile) else {
        return Err(ResponsesProfileError::UnknownProfile {
            profile: profile.to_string(),
            allowed: allowed_list(),
        });
    };
    if !CUSTOM_ALLOWED_PROFILES.contains(&found.id) {
        return Err(ResponsesProfileError::ProfileNotAllowed {
            profile: profile.to_string(),
            allowed: allowed_list(),
        });
    }
    Ok(found)
}

pub fn select_responses_profile(
    query: ResponsesProfileQuery<'_>,
) -> Result<Option<&'static ResponsesDialectProfile>, ResponsesProfileError> {
    if query.protocol != WireFormat::Responses {
        return match query.explicit_profile {
            Some(profile) => Err(ResponsesProfileError::NotResponses {
                profile: profile.to_string(),
            }),
            None => Ok(None),
        };
    }
    if let Some(explicit) = query.explicit_profile {
        if query.provider != ProviderKind::Custom {
            return Err(ResponsesProfileError::BuiltinOverride {
                provider: query.provider,
            });
        }
        let Some(profile) = find_profile(explicit) else {
            return Err(ResponsesProfileError::UnknownProfile {
                profile: explicit.to_string(),
                allowed: allowed_list(),
            });
        };
        if !CUSTOM_ALLOWED_PROFILES.contains(&profile.id) {
            return Err(ResponsesProfileError::ProfileNotAllowed {
                profile: explicit.to_string(),
                allowed: allowed_list(),
            });
        }
        return Ok(Some(profile));
    }
    let normalized = normalize_base_url(query.base_url);
    let mut best: Option<&ResponsesProfileBinding> = None;
    for binding in BUILTIN_BINDINGS {
        if !binding_matches(binding, query.provider, query.endpoint_key, &normalized) {
            continue;
        }
        match best {
            None => best = Some(binding),
            Some(current) if specificity(binding) > specificity(current) => best = Some(binding),
            Some(current) if specificity(binding) == specificity(current) => {
                return Err(ResponsesProfileError::AmbiguousBinding {
                    provider: query.provider,
                });
            }
            Some(_) => {}
        }
    }
    let id = best
        .map(|binding| binding.profile_id)
        .unwrap_or(DEFAULT_PROFILE_ID);
    Ok(Some(
        find_profile(id.as_str()).expect("responses profile binding"),
    ))
}

fn find_profile(id: &str) -> Option<&'static ResponsesDialectProfile> {
    BUILTIN_PROFILES
        .iter()
        .find(|profile| profile.id.as_str() == id)
}

fn allowed_list() -> String {
    CUSTOM_ALLOWED_PROFILES
        .iter()
        .map(|id| id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn binding_matches(
    binding: &ResponsesProfileBinding,
    provider: ProviderKind,
    endpoint_key: &str,
    base_url: &str,
) -> bool {
    if binding.provider_kind != provider {
        return false;
    }
    if let Some(expected) = binding.endpoint_key
        && expected != endpoint_key
    {
        return false;
    }
    if let Some(urls) = binding.base_urls
        && !urls.contains(&base_url)
    {
        return false;
    }
    true
}

fn specificity(binding: &ResponsesProfileBinding) -> u8 {
    u8::from(binding.endpoint_key.is_some()) + u8::from(binding.base_urls.is_some())
}

fn normalize_base_url(url: &str) -> String {
    url.trim_end_matches('/').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_asset_matches_the_generated_table() {
        let parsed = data::parse(include_str!("../assets/responses_dialects.json"))
            .expect("responses dialect asset");
        let generated = data::codegen(&parsed);
        assert_eq!(parsed.profiles.len(), BUILTIN_PROFILES.len());
        assert_eq!(parsed.bindings.len(), BUILTIN_BINDINGS.len());
        for profile in &parsed.profiles {
            assert!(
                BUILTIN_PROFILES
                    .iter()
                    .any(|item| item.id.as_str() == profile.id),
                "missing generated profile {}",
                profile.id
            );
            assert!(generated.contains(&profile.id));
        }
    }

    const DEEPSEEK_URLS: &[&str] = &[
        "https://api.deepseek.com",
        "https://api.deepseek.com/v1",
        "https://api.deepseek.com/beta",
        "https://api.deepseeki.com",
        "https://api.deepseeki.com/v1",
        "https://api.deepseeki.com/beta",
    ];

    fn query<'a>(
        protocol: WireFormat,
        provider: ProviderKind,
        endpoint_key: &'a str,
        base_url: &'a str,
        explicit_profile: Option<&'a str>,
    ) -> ResponsesProfileQuery<'a> {
        ResponsesProfileQuery {
            protocol,
            provider,
            endpoint_key,
            base_url,
            explicit_profile,
        }
    }

    fn selected(query: ResponsesProfileQuery<'_>) -> &'static str {
        select_responses_profile(query)
            .expect("profile")
            .expect("responses profile")
            .id
            .as_str()
    }

    fn asset() -> serde_json::Value {
        serde_json::from_str(include_str!("../assets/responses_dialects.json")).expect("asset")
    }

    #[test]
    fn builtin_table_has_four_profiles_and_one_usage_policy() {
        let profiles = builtin_profiles();
        assert_eq!(profiles.len(), 4);
        let usage = profiles[0].usage;
        assert!(profiles.iter().all(|profile| profile.usage == usage));
        assert_eq!(
            profiles
                .iter()
                .find(|profile| profile.id.as_str() == "codex")
                .expect("codex")
                .tools
                .namespace,
            Some("codewhale")
        );
    }

    #[test]
    fn deepseek_binding_is_the_responses_url_allowlist() {
        let binding = BUILTIN_BINDINGS
            .iter()
            .find(|binding| binding.provider_kind == ProviderKind::Deepseek)
            .expect("deepseek binding");
        assert_eq!(binding.endpoint_key, Some("responses"));
        assert_eq!(binding.base_urls, Some(DEEPSEEK_URLS));
        assert!(
            !binding
                .base_urls
                .expect("urls")
                .contains(&"https://api.deepseek.com/anthropic")
        );
    }

    #[test]
    fn selection_follows_protocol_then_binding() {
        assert!(
            select_responses_profile(query(
                WireFormat::ChatCompletions,
                ProviderKind::OpenaiCodex,
                "responses",
                "https://api.openai.com/v1",
                None,
            ))
            .expect("chat")
            .is_none()
        );
        assert_eq!(
            selected(query(
                WireFormat::Responses,
                ProviderKind::Openai,
                "responses",
                "https://api.openai.com/v1",
                None,
            )),
            "standard"
        );
        assert_eq!(
            selected(query(
                WireFormat::Responses,
                ProviderKind::OpenaiCodex,
                "chat",
                "https://api.openai.com/v1",
                None,
            )),
            "codex"
        );
        assert_eq!(
            selected(query(
                WireFormat::Responses,
                ProviderKind::Concentrate,
                "models",
                "https://api.concentrate.ai/v1",
                None,
            )),
            "concentrate"
        );
        for url in DEEPSEEK_URLS {
            assert_eq!(
                selected(query(
                    WireFormat::Responses,
                    ProviderKind::Deepseek,
                    "responses",
                    url,
                    None,
                )),
                "deepseek_stateless_plain_reasoning"
            );
        }
        assert_eq!(
            selected(query(
                WireFormat::Responses,
                ProviderKind::Deepseek,
                "responses",
                "https://API.DEEPSEEK.COM/v1/",
                None,
            )),
            "deepseek_stateless_plain_reasoning"
        );
        assert_eq!(
            selected(query(
                WireFormat::Responses,
                ProviderKind::Deepseek,
                "responses",
                "https://api.deepseek.com/anthropic",
                None,
            )),
            "standard"
        );
        assert_eq!(
            selected(query(
                WireFormat::Responses,
                ProviderKind::Deepseek,
                "Responses",
                "https://api.deepseek.com",
                None,
            )),
            "standard"
        );
    }

    #[test]
    fn custom_override_is_allowlisted_and_built_ins_cannot_set_one() {
        assert_eq!(
            selected(query(
                WireFormat::Responses,
                ProviderKind::Custom,
                "responses",
                "https://gateway.example/v1",
                None,
            )),
            "standard"
        );
        assert_eq!(
            selected(query(
                WireFormat::Responses,
                ProviderKind::Custom,
                "responses",
                "https://gateway.example/v1",
                Some("standard"),
            )),
            "standard"
        );
        let denied = select_responses_profile(query(
            WireFormat::Responses,
            ProviderKind::Custom,
            "responses",
            "https://gateway.example/v1",
            Some("codex"),
        ))
        .expect_err("codex");
        assert!(denied.to_string().contains("allowed: standard"), "{denied}");
        let unknown = select_responses_profile(query(
            WireFormat::Responses,
            ProviderKind::Custom,
            "responses",
            "https://gateway.example/v1",
            Some("missing"),
        ))
        .expect_err("missing");
        assert!(unknown.to_string().contains("unknown responses profile"));
        assert!(
            select_responses_profile(query(
                WireFormat::ChatCompletions,
                ProviderKind::Custom,
                "chat",
                "https://gateway.example/v1",
                Some("standard"),
            ))
            .expect_err("wire")
            .to_string()
            .contains("not responses")
        );
        assert!(
            select_responses_profile(query(
                WireFormat::Responses,
                ProviderKind::Openai,
                "responses",
                "https://api.openai.com/v1",
                Some("standard"),
            ))
            .expect_err("built-in")
            .to_string()
            .contains("openai")
        );
    }

    #[test]
    fn parser_rejects_unknown_fields_conflicts_and_unsafe_custom_profiles() {
        let mut unknown = asset();
        unknown["profiles"][0]["request"]["store"] = serde_json::json!("false");
        assert!(super::data::parse(&unknown.to_string()).is_err());

        let mut extra = asset();
        extra["profiles"][0]["request"]["patch"] = serde_json::json!("x");
        assert!(super::data::parse(&extra.to_string()).is_err());

        let mut duplicate = asset();
        duplicate["profiles"][1]["id"] = duplicate["profiles"][0]["id"].clone();
        assert!(super::data::parse(&duplicate.to_string()).is_err());

        let mut overlap = asset();
        let binding = overlap["bindings"][0].clone();
        overlap["bindings"]
            .as_array_mut()
            .expect("bindings")
            .push(binding);
        assert!(super::data::parse(&overlap.to_string()).is_err());

        let mut custom = asset();
        custom["custom_allowed_profiles"] = serde_json::json!(["standard", "codex"]);
        assert!(super::data::parse(&custom.to_string()).is_err());

        let mut secret = asset();
        secret["profiles"][0]["request"]["api_key"] = serde_json::json!("sk-test");
        assert!(super::data::parse(&secret.to_string()).is_err());

        let mut urls = asset();
        let deepseek = urls["bindings"]
            .as_array_mut()
            .expect("bindings")
            .iter_mut()
            .find(|binding| binding["provider_kind"] == "Deepseek")
            .expect("deepseek");
        deepseek["base_url_match"]["urls"]
            .as_array_mut()
            .expect("urls")
            .push(serde_json::json!("https://api.deepseek.com/"));
        assert!(super::data::parse(&urls.to_string()).is_err());
    }
}
