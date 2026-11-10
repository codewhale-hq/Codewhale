use std::collections::{BTreeSet, HashSet};

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDialects {
    pub default_profile: String,
    pub custom_allowed: Vec<String>,
    pub profiles: Vec<ParsedProfile>,
    pub bindings: Vec<ParsedBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedProfile {
    pub id: String,
    pub endpoint_path: String,
    pub protocol_headers: String,
    pub store: String,
    pub include: String,
    pub instructions: String,
    pub max_output_tokens: String,
    pub sampling: String,
    pub effort_mapping: String,
    pub shape: String,
    pub summary: String,
    pub encrypted_state: String,
    pub plain_reasoning: String,
    pub system_role: String,
    pub envelope: String,
    pub namespace: Option<String>,
    pub parallel_calls: String,
    pub malformed_json: String,
    pub unknown_event: String,
    pub done_marker: String,
    pub completion: String,
    pub incomplete: String,
    pub cache_fields: String,
    pub reasoning_fields: String,
    pub audio_fields: String,
    pub function_tools: String,
    pub server_side_tools: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedBinding {
    pub provider_kind: String,
    pub endpoint_key: Option<String>,
    pub urls: Option<Vec<String>>,
    pub profile: String,
}

const FORBIDDEN_KEYS: &[&str] = &[
    "authorization",
    "api_key",
    "api-key",
    "token",
    "access_token",
    "account_id",
    "account-id",
    "secret",
    "password",
    "credential",
    "bearer",
];

pub fn parse(raw: &str) -> Result<ParsedDialects, String> {
    let value: Value = serde_json::from_str(raw).map_err(|err| format!("invalid JSON: {err}"))?;
    reject_secrets(&value, "responses dialects")?;
    let root = as_object(&value, "responses dialects")?;
    keys_exact(
        root,
        "responses dialects",
        &[
            "schema_version",
            "default_profile",
            "custom_allowed_profiles",
            "profiles",
            "bindings",
        ],
    )?;
    let version = root
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or("schema_version must be 1")?;
    if version != 1 {
        return Err(format!("unsupported responses dialect schema {version}"));
    }
    let profiles = root
        .get("profiles")
        .and_then(Value::as_array)
        .ok_or("profiles must be an array")?;
    if profiles.is_empty() {
        return Err("profiles must not be empty".to_string());
    }
    let mut parsed_profiles = Vec::with_capacity(profiles.len());
    let mut ids = BTreeSet::new();
    for (index, profile) in profiles.iter().enumerate() {
        let profile = parse_profile(profile, index)?;
        if !ids.insert(profile.id.clone()) {
            return Err(format!("duplicate profile id `{}`", profile.id));
        }
        parsed_profiles.push(profile);
    }
    let default_profile = req_str(root, "responses dialects", "default_profile")?;
    if !valid_id(default_profile) {
        return Err(format!("invalid default profile id `{default_profile}`"));
    }
    let default_profile = default_profile.to_string();
    let Some(default) = parsed_profiles
        .iter()
        .find(|profile| profile.id == default_profile)
    else {
        return Err(format!(
            "default profile `{default_profile}` does not exist"
        ));
    };
    if default.endpoint_path != "versioned_responses"
        || default.encrypted_state == "account_api_model_scoped"
    {
        return Err("default profile must be an ordinary versioned responses dialect".to_string());
    }
    let allowed_values = root
        .get("custom_allowed_profiles")
        .and_then(Value::as_array)
        .ok_or("custom_allowed_profiles must be an array")?;
    if allowed_values.is_empty() {
        return Err("custom_allowed_profiles must not be empty".to_string());
    }
    let mut custom_allowed = Vec::with_capacity(allowed_values.len());
    let mut seen_allowed = BTreeSet::new();
    for value in allowed_values {
        let id = value
            .as_str()
            .filter(|id| valid_id(id))
            .ok_or("custom_allowed_profiles entries must be profile ids")?;
        if !seen_allowed.insert(id.to_string()) {
            return Err(format!("duplicate custom allowed profile `{id}`"));
        }
        let Some(profile) = parsed_profiles.iter().find(|profile| profile.id == id) else {
            return Err(format!("custom allowed profile `{id}` does not exist"));
        };
        if !custom_safe(profile) {
            return Err(format!(
                "profile `{id}` cannot be selected by a custom provider"
            ));
        }
        custom_allowed.push(id.to_string());
    }
    if !custom_allowed.iter().any(|id| id == &default_profile) {
        return Err("default profile must be allowed for custom providers".to_string());
    }
    let bindings = root
        .get("bindings")
        .and_then(Value::as_array)
        .ok_or("bindings must be an array")?;
    if bindings.is_empty() {
        return Err("bindings must not be empty".to_string());
    }
    let mut parsed_bindings = Vec::with_capacity(bindings.len());
    for (index, binding) in bindings.iter().enumerate() {
        parsed_bindings.push(parse_binding(binding, index, &parsed_profiles)?);
    }
    for left in 0..parsed_bindings.len() {
        for right in (left + 1)..parsed_bindings.len() {
            if !binding_pair_ok(&parsed_bindings[left], &parsed_bindings[right]) {
                return Err(format!(
                    "responses profile bindings overlap for {}",
                    parsed_bindings[left].provider_kind
                ));
            }
        }
    }
    for profile in &parsed_profiles {
        if profile.endpoint_path != "unversioned_responses" {
            continue;
        }
        if custom_allowed.iter().any(|id| id == &profile.id) || profile.id == default_profile {
            return Err(format!(
                "unversioned profile `{}` cannot be a default or custom profile",
                profile.id
            ));
        }
        let bound = parsed_bindings.iter().any(|binding| {
            binding.profile == profile.id
                && binding.urls.as_ref().is_some_and(|urls| !urls.is_empty())
        });
        if !bound {
            return Err(format!(
                "unversioned profile `{}` requires an exact base url binding",
                profile.id
            ));
        }
    }
    Ok(ParsedDialects {
        default_profile,
        custom_allowed,
        profiles: parsed_profiles,
        bindings: parsed_bindings,
    })
}

pub fn codegen(doc: &ParsedDialects) -> String {
    let mut out = String::from("// Generated from responses_dialects.json. Do not edit.\n");
    out.push_str("const BUILTIN_PROFILES: &[ResponsesDialectProfile] = &[\n");
    for profile in &doc.profiles {
        out.push_str(&emit_profile(profile));
        out.push_str(",\n");
    }
    out.push_str("];\nconst BUILTIN_BINDINGS: &[ResponsesProfileBinding] = &[\n");
    for binding in &doc.bindings {
        out.push_str(&emit_binding(binding));
        out.push_str(",\n");
    }
    out.push_str(
        "];\nconst DEFAULT_PROFILE_ID: ResponsesProfileId = ResponsesProfileId::from_static(",
    );
    out.push_str(&format!("{:?}", doc.default_profile));
    out.push_str(");\nconst CUSTOM_ALLOWED_PROFILES: &[ResponsesProfileId] = &[\n");
    for id in &doc.custom_allowed {
        out.push_str("    ResponsesProfileId::from_static(");
        out.push_str(&format!("{id:?}"));
        out.push_str("),\n");
    }
    out.push_str("];\n");
    out
}

fn emit_profile(profile: &ParsedProfile) -> String {
    let namespace = match &profile.namespace {
        Some(namespace) => format!("Some({namespace:?})"),
        None => "None".to_string(),
    };
    format!(
        "ResponsesDialectProfile {{
    id: ResponsesProfileId::from_static({id:?}),
    endpoint: ResponsesEndpointPolicy {{ path: {path} }},
    headers: ResponsesHeaderPolicy {{ protocol_headers: {headers} }},
    request: ResponsesRequestPolicy {{
        store: {store},
        include: {include},
        instructions: {instructions},
        max_output_tokens: {max_output_tokens},
        sampling: {sampling},
    }},
    reasoning: ResponsesReasoningPolicy {{
        effort_mapping: {effort},
        shape: {shape},
        summary: {summary},
    }},
    replay: ResponsesReplayPolicy {{
        encrypted_state: {encrypted},
        plain_reasoning: {plain},
        system_role: {system_role},
    }},
    tools: ResponsesToolPolicy {{
        envelope: {envelope},
        namespace: {namespace},
        parallel_calls: {parallel},
    }},
    stream: ResponsesStreamPolicy {{
        malformed_json: {malformed},
        unknown_event: {unknown_event},
        done_marker: {done_marker},
        completion: {completion},
        incomplete: {incomplete},
    }},
    usage: ResponsesUsagePolicy {{
        cache_fields: {cache_fields},
        reasoning_fields: {reasoning_fields},
        audio_fields: {audio_fields},
    }},
    capabilities: ResponsesCapabilityPolicy {{
        function_tools: {function_tools},
        server_side_tools: {server_side_tools},
    }},
}}",
        id = profile.id,
        path = variant("path", &profile.endpoint_path),
        headers = variant("headers", &profile.protocol_headers),
        store = variant("store", &profile.store),
        include = variant("include", &profile.include),
        instructions = variant("instructions", &profile.instructions),
        max_output_tokens = variant("max_output_tokens", &profile.max_output_tokens),
        sampling = variant("sampling", &profile.sampling),
        effort = variant("effort", &profile.effort_mapping),
        shape = variant("shape", &profile.shape),
        summary = variant("summary", &profile.summary),
        encrypted = variant("encrypted", &profile.encrypted_state),
        plain = variant("plain", &profile.plain_reasoning),
        system_role = variant("system_role", &profile.system_role),
        envelope = variant("envelope", &profile.envelope),
        parallel = variant("parallel", &profile.parallel_calls),
        malformed = variant("malformed", &profile.malformed_json),
        unknown_event = variant("unknown_event", &profile.unknown_event),
        done_marker = variant("done_marker", &profile.done_marker),
        completion = variant("completion", &profile.completion),
        incomplete = variant("incomplete", &profile.incomplete),
        cache_fields = variant("cache_fields", &profile.cache_fields),
        reasoning_fields = variant("reasoning_fields", &profile.reasoning_fields),
        audio_fields = variant("audio_fields", &profile.audio_fields),
        function_tools = variant("function_tools", &profile.function_tools),
        server_side_tools = variant("server_side_tools", &profile.server_side_tools),
    )
}

fn emit_binding(binding: &ParsedBinding) -> String {
    let endpoint = match &binding.endpoint_key {
        Some(endpoint) => format!("Some({endpoint:?})"),
        None => "None".to_string(),
    };
    let urls = match &binding.urls {
        Some(urls) => format!(
            "Some(&[{}])",
            urls.iter()
                .map(|url| format!("{url:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None => "None".to_string(),
    };
    format!(
        "ResponsesProfileBinding {{
    provider_kind: crate::ProviderKind::{kind},
    endpoint_key: {endpoint},
    base_urls: {urls},
    profile_id: ResponsesProfileId::from_static({profile:?}),
}}",
        kind = binding.provider_kind,
        profile = binding.profile,
    )
}

fn variant(group: &str, value: &str) -> &'static str {
    match (group, value) {
        ("path", "versioned_responses") => "ResponsesPathPolicy::VersionedResponses",
        ("path", "unversioned_responses") => "ResponsesPathPolicy::UnversionedResponses",
        ("headers", "standard_json") => "ProtocolHeaderPolicy::StandardJson",
        ("store", "explicit_false") => "StorePolicy::ExplicitFalse",
        ("store", "omit") => "StorePolicy::Omit",
        ("include", "encrypted_reasoning") => "IncludePolicy::EncryptedReasoning",
        ("include", "omit") => "IncludePolicy::Omit",
        ("instructions", "top_level") => "InstructionsPlacement::TopLevel",
        ("instructions", "leading_system_input") => "InstructionsPlacement::LeadingSystemInput",
        ("max_output_tokens", "when_positive") => "OutputTokenFieldPolicy::WhenPositive",
        ("max_output_tokens", "omit") => "OutputTokenFieldPolicy::Omit",
        ("sampling", "temperature_and_top_p") => "SamplingPolicy::TemperatureAndTopP",
        ("sampling", "omit") => "SamplingPolicy::Omit",
        ("effort", "openai_compatible") => "ReasoningEffortMapping::OpenaiCompatible",
        ("effort", "codex") => "ReasoningEffortMapping::Codex",
        ("effort", "deepseek") => "ReasoningEffortMapping::Deepseek",
        ("shape", "effort_with_summary") => "ReasoningRequestShape::EffortWithSummary",
        ("shape", "effort_only") => "ReasoningRequestShape::EffortOnly",
        ("summary", "auto") => "ReasoningSummaryPolicy::Auto",
        ("summary", "omit") => "ReasoningSummaryPolicy::Omit",
        ("encrypted", "provider_api_model_scoped") => {
            "EncryptedReasoningReplayPolicy::ProviderApiModelScoped"
        }
        ("encrypted", "account_api_model_scoped") => {
            "EncryptedReasoningReplayPolicy::AccountApiModelScoped"
        }
        ("encrypted", "disabled") => "EncryptedReasoningReplayPolicy::Disabled",
        ("plain", "ignore") => "PlainReasoningReplayPolicy::Ignore",
        ("plain", "plain_text") => "PlainReasoningReplayPolicy::PlainText",
        ("plain", "plain_text_with_tool_placeholder") => {
            "PlainReasoningReplayPolicy::PlainTextWithToolPlaceholder"
        }
        ("system_role", "preserve") => "SystemRoleReplayPolicy::Preserve",
        ("system_role", "system_to_developer") => "SystemRoleReplayPolicy::SystemToDeveloper",
        ("envelope", "function_array") => "ToolEnvelopePolicy::FunctionArray",
        ("envelope", "namespace_wrapped") => "ToolEnvelopePolicy::NamespaceWrapped",
        ("parallel", "allow") => "ParallelToolCallPolicy::Allow",
        ("parallel", "disallow") => "ParallelToolCallPolicy::Disallow",
        ("malformed", "fail") => "MalformedEventPolicy::Fail",
        ("malformed", "skip_with_diagnostic") => "MalformedEventPolicy::SkipWithDiagnostic",
        ("unknown_event", "ignore") => "UnknownEventPolicy::Ignore",
        ("unknown_event", "fail") => "UnknownEventPolicy::Fail",
        ("done_marker", "terminal") => "DoneMarkerPolicy::Terminal",
        ("done_marker", "ignore") => "DoneMarkerPolicy::Ignore",
        ("completion", "accept_documented_terminal_events") => {
            "CompletionPolicy::AcceptDocumentedTerminalEvents"
        }
        ("completion", "require_successful_response_completed") => {
            "CompletionPolicy::RequireSuccessfulResponseCompleted"
        }
        ("incomplete", "preserve_provider_incomplete_result") => {
            "IncompleteResponsePolicy::PreserveProviderIncompleteResult"
        }
        ("incomplete", "fail") => "IncompleteResponsePolicy::Fail",
        ("cache_fields", "top_level_then_nested_fallback") => {
            "CacheUsageFieldPolicy::TopLevelThenNestedFallback"
        }
        ("reasoning_fields", "output_tokens_details") => {
            "ReasoningUsageFieldPolicy::OutputTokensDetails"
        }
        ("audio_fields", "nested_details") => "AudioUsageFieldPolicy::NestedDetails",
        ("function_tools", "supported") => "FunctionToolCapability::Supported",
        ("function_tools", "unsupported") => "FunctionToolCapability::Unsupported",
        ("server_side_tools", "route_and_adapter_gated") => {
            "ServerSideToolCapability::RouteAndAdapterGated"
        }
        ("server_side_tools", "unsupported") => "ServerSideToolCapability::Unsupported",
        _ => unreachable!("validator accepted an unknown responses dialect value"),
    }
}

fn parse_profile(value: &Value, index: usize) -> Result<ParsedProfile, String> {
    let at = format!("profiles[{index}]");
    let map = as_object(value, &at)?;
    keys_exact(
        map,
        &at,
        &[
            "id",
            "endpoint",
            "headers",
            "request",
            "reasoning",
            "replay",
            "tools",
            "stream",
            "usage",
            "capabilities",
        ],
    )?;
    let id = req_str(map, &at, "id")?;
    if !valid_id(id) {
        return Err(format!("{at} has invalid id `{id}`"));
    }
    let endpoint = nested(map, &at, "endpoint", &["path"])?;
    let headers = nested(map, &at, "headers", &["protocol_headers"])?;
    let request = nested(
        map,
        &at,
        "request",
        &[
            "store",
            "include",
            "instructions",
            "max_output_tokens",
            "sampling",
        ],
    )?;
    let reasoning = nested(
        map,
        &at,
        "reasoning",
        &["effort_mapping", "shape", "summary"],
    )?;
    let replay = nested(
        map,
        &at,
        "replay",
        &["encrypted_state", "plain_reasoning", "system_role"],
    )?;
    let tools = nested(
        map,
        &at,
        "tools",
        &["envelope", "namespace", "parallel_calls"],
    )?;
    let stream = nested(
        map,
        &at,
        "stream",
        &[
            "malformed_json",
            "unknown_event",
            "done_marker",
            "completion",
            "incomplete",
        ],
    )?;
    let usage = nested(
        map,
        &at,
        "usage",
        &["cache_fields", "reasoning_fields", "audio_fields"],
    )?;
    let capabilities = nested(
        map,
        &at,
        "capabilities",
        &["function_tools", "server_side_tools"],
    )?;
    let endpoint_path = enum_str(
        &endpoint,
        &format!("{at}.endpoint"),
        "path",
        &["versioned_responses", "unversioned_responses"],
    )?;
    let shape = enum_str(
        &reasoning,
        &format!("{at}.reasoning"),
        "shape",
        &["effort_with_summary", "effort_only"],
    )?;
    let summary = enum_str(
        &reasoning,
        &format!("{at}.reasoning"),
        "summary",
        &["auto", "omit"],
    )?;
    match (shape, summary) {
        ("effort_with_summary", "auto") | ("effort_only", "omit") => {}
        _ => {
            return Err(format!(
                "{at} reasoning shape `{shape}` does not match summary `{summary}`"
            ));
        }
    }
    let envelope = enum_str(
        &tools,
        &format!("{at}.tools"),
        "envelope",
        &["function_array", "namespace_wrapped"],
    )?;
    let namespace = optional_namespace(&tools, &format!("{at}.tools"))?;
    match (envelope, namespace.as_deref()) {
        ("namespace_wrapped", Some(namespace)) if valid_id(namespace) => {}
        ("function_array", None) => {}
        _ => {
            return Err(format!(
                "{at} tool namespace does not match envelope `{envelope}`"
            ));
        }
    }
    let encrypted_state = enum_str(
        &replay,
        &format!("{at}.replay"),
        "encrypted_state",
        &[
            "provider_api_model_scoped",
            "account_api_model_scoped",
            "disabled",
        ],
    )?;
    Ok(ParsedProfile {
        id: id.to_string(),
        endpoint_path: endpoint_path.to_string(),
        protocol_headers: enum_str(
            &headers,
            &format!("{at}.headers"),
            "protocol_headers",
            &["standard_json"],
        )?
        .to_string(),
        store: enum_str(
            &request,
            &format!("{at}.request"),
            "store",
            &["explicit_false", "omit"],
        )?
        .to_string(),
        include: enum_str(
            &request,
            &format!("{at}.request"),
            "include",
            &["encrypted_reasoning", "omit"],
        )?
        .to_string(),
        instructions: enum_str(
            &request,
            &format!("{at}.request"),
            "instructions",
            &["top_level", "leading_system_input"],
        )?
        .to_string(),
        max_output_tokens: enum_str(
            &request,
            &format!("{at}.request"),
            "max_output_tokens",
            &["when_positive", "omit"],
        )?
        .to_string(),
        sampling: enum_str(
            &request,
            &format!("{at}.request"),
            "sampling",
            &["temperature_and_top_p", "omit"],
        )?
        .to_string(),
        effort_mapping: enum_str(
            &reasoning,
            &format!("{at}.reasoning"),
            "effort_mapping",
            &["openai_compatible", "codex", "deepseek"],
        )?
        .to_string(),
        shape: shape.to_string(),
        summary: summary.to_string(),
        encrypted_state: encrypted_state.to_string(),
        plain_reasoning: enum_str(
            &replay,
            &format!("{at}.replay"),
            "plain_reasoning",
            &["ignore", "plain_text", "plain_text_with_tool_placeholder"],
        )?
        .to_string(),
        system_role: enum_str(
            &replay,
            &format!("{at}.replay"),
            "system_role",
            &["preserve", "system_to_developer"],
        )?
        .to_string(),
        envelope: envelope.to_string(),
        namespace,
        parallel_calls: enum_str(
            &tools,
            &format!("{at}.tools"),
            "parallel_calls",
            &["allow", "disallow"],
        )?
        .to_string(),
        malformed_json: enum_str(
            &stream,
            &format!("{at}.stream"),
            "malformed_json",
            &["fail", "skip_with_diagnostic"],
        )?
        .to_string(),
        unknown_event: enum_str(
            &stream,
            &format!("{at}.stream"),
            "unknown_event",
            &["ignore", "fail"],
        )?
        .to_string(),
        done_marker: enum_str(
            &stream,
            &format!("{at}.stream"),
            "done_marker",
            &["terminal", "ignore"],
        )?
        .to_string(),
        completion: enum_str(
            &stream,
            &format!("{at}.stream"),
            "completion",
            &[
                "accept_documented_terminal_events",
                "require_successful_response_completed",
            ],
        )?
        .to_string(),
        incomplete: enum_str(
            &stream,
            &format!("{at}.stream"),
            "incomplete",
            &["preserve_provider_incomplete_result", "fail"],
        )?
        .to_string(),
        cache_fields: enum_str(
            &usage,
            &format!("{at}.usage"),
            "cache_fields",
            &["top_level_then_nested_fallback"],
        )?
        .to_string(),
        reasoning_fields: enum_str(
            &usage,
            &format!("{at}.usage"),
            "reasoning_fields",
            &["output_tokens_details"],
        )?
        .to_string(),
        audio_fields: enum_str(
            &usage,
            &format!("{at}.usage"),
            "audio_fields",
            &["nested_details"],
        )?
        .to_string(),
        function_tools: enum_str(
            &capabilities,
            &format!("{at}.capabilities"),
            "function_tools",
            &["supported", "unsupported"],
        )?
        .to_string(),
        server_side_tools: enum_str(
            &capabilities,
            &format!("{at}.capabilities"),
            "server_side_tools",
            &["route_and_adapter_gated", "unsupported"],
        )?
        .to_string(),
    })
}

fn parse_binding(
    value: &Value,
    index: usize,
    profiles: &[ParsedProfile],
) -> Result<ParsedBinding, String> {
    let at = format!("bindings[{index}]");
    let map = as_object(value, &at)?;
    keys_exact(
        map,
        &at,
        &[
            "provider_kind",
            "protocol",
            "endpoint_key",
            "base_url_match",
            "profile",
        ],
    )?;
    let provider_kind = req_str(map, &at, "provider_kind")?;
    if !provider_kind.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return Err(format!("{at} has invalid provider kind `{provider_kind}`"));
    }
    let protocol = req_str(map, &at, "protocol")?;
    if protocol != "responses" {
        return Err(format!("{at} protocol must be responses"));
    }
    let endpoint_key = match map.get("endpoint_key") {
        None => None,
        Some(Value::String(endpoint)) if valid_id(endpoint.as_str()) => Some(endpoint.clone()),
        _ => return Err(format!("{at}.endpoint_key must be a profile-style id")),
    };
    let profile = req_str(map, &at, "profile")?;
    if !profiles.iter().any(|item| item.id == profile) {
        return Err(format!("{at} references unknown profile `{profile}`"));
    }
    let urls = parse_url_match(map.get("base_url_match").unwrap_or(&Value::Null), &at)?;
    Ok(ParsedBinding {
        provider_kind: provider_kind.to_string(),
        endpoint_key,
        urls,
        profile: profile.to_string(),
    })
}

fn parse_url_match(value: &Value, at: &str) -> Result<Option<Vec<String>>, String> {
    let at = format!("{at}.base_url_match");
    let map = as_object(value, &at)?;
    let kind = req_str(map, &at, "type")?;
    match kind {
        "any" => {
            keys_exact(map, &at, &["type"])?;
            Ok(None)
        }
        "exact_normalized" => {
            keys_exact(map, &at, &["type", "urls"])?;
            let urls = map
                .get("urls")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("{at}.urls must be an array"))?;
            if urls.is_empty() {
                return Err(format!("{at}.urls must not be empty"));
            }
            let mut parsed = Vec::with_capacity(urls.len());
            let mut seen = HashSet::new();
            for url in urls {
                let url = url
                    .as_str()
                    .ok_or_else(|| format!("{at}.urls entries must be strings"))?;
                if url.is_empty()
                    || url
                        .chars()
                        .any(|ch| ch.is_whitespace() || matches!(ch, '?' | '#' | '@'))
                    || !url.starts_with("https://")
                    || url != url.trim_end_matches('/').to_ascii_lowercase()
                {
                    return Err(format!(
                        "{at} url `{url}` is not an exact normalized https url"
                    ));
                }
                if !seen.insert(url.to_string()) {
                    return Err(format!("{at} repeats url `{url}`"));
                }
                parsed.push(url.to_string());
            }
            Ok(Some(parsed))
        }
        _ => Err(format!("{at} has unknown match `{kind}`")),
    }
}

fn custom_safe(profile: &ParsedProfile) -> bool {
    profile.encrypted_state != "account_api_model_scoped"
        && profile.endpoint_path != "unversioned_responses"
        && profile.envelope != "namespace_wrapped"
}

fn binding_pair_ok(left: &ParsedBinding, right: &ParsedBinding) -> bool {
    if !bindings_overlap(left, right) {
        return true;
    }
    strictly_more_specific(left, right) || strictly_more_specific(right, left)
}

fn bindings_overlap(left: &ParsedBinding, right: &ParsedBinding) -> bool {
    if left.provider_kind != right.provider_kind {
        return false;
    }
    if let (Some(left_endpoint), Some(right_endpoint)) = (&left.endpoint_key, &right.endpoint_key)
        && left_endpoint != right_endpoint
    {
        return false;
    }
    match (&left.urls, &right.urls) {
        (Some(left_urls), Some(right_urls)) => left_urls.iter().any(|url| right_urls.contains(url)),
        _ => true,
    }
}

fn strictly_more_specific(specific: &ParsedBinding, general: &ParsedBinding) -> bool {
    specificity(specific) > specificity(general) && constraints_fit(specific, general)
}

fn specificity(binding: &ParsedBinding) -> u8 {
    u8::from(binding.endpoint_key.is_some()) + u8::from(binding.urls.is_some())
}

fn constraints_fit(specific: &ParsedBinding, general: &ParsedBinding) -> bool {
    if let Some(endpoint) = &general.endpoint_key
        && specific.endpoint_key.as_ref() != Some(endpoint)
    {
        return false;
    }
    if let Some(general_urls) = &general.urls {
        let Some(specific_urls) = &specific.urls else {
            return false;
        };
        if specific_urls.iter().any(|url| !general_urls.contains(url)) {
            return false;
        }
    }
    true
}

fn nested(
    map: &Map<String, Value>,
    at: &str,
    key: &str,
    fields: &[&str],
) -> Result<Map<String, Value>, String> {
    let value = map
        .get(key)
        .ok_or_else(|| format!("{at} is missing `{key}`"))?;
    let child = as_object(value, &format!("{at}.{key}"))?;
    keys_exact(child, &format!("{at}.{key}"), fields)?;
    Ok(child.clone())
}

fn optional_namespace(map: &Map<String, Value>, at: &str) -> Result<Option<String>, String> {
    match map.get("namespace") {
        None => Err(format!("{at} is missing `namespace`")),
        Some(Value::Null) => Ok(None),
        Some(Value::String(namespace)) if !namespace.is_empty() => Ok(Some(namespace.clone())),
        _ => Err(format!("{at}.namespace must be a string or null")),
    }
}

fn enum_str<'a>(
    map: &'a Map<String, Value>,
    at: &str,
    key: &str,
    allowed: &[&str],
) -> Result<&'a str, String> {
    let value = req_str(map, at, key)?;
    if allowed.contains(&value) {
        Ok(value)
    } else {
        Err(format!("{at}.{key} has unknown value `{value}`"))
    }
}

fn req_str<'a>(map: &'a Map<String, Value>, at: &str, key: &str) -> Result<&'a str, String> {
    match map.get(key) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value),
        Some(_) => Err(format!("{at}.{key} must be a non-empty string")),
        None => Err(format!("{at} is missing `{key}`")),
    }
}

fn as_object<'a>(value: &'a Value, at: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{at} must be an object"))
}

fn keys_exact(map: &Map<String, Value>, at: &str, allowed: &[&str]) -> Result<(), String> {
    for key in map.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("{at} has unknown field `{key}`"));
        }
    }
    Ok(())
}

fn valid_id(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn reject_secrets(value: &Value, at: &str) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if FORBIDDEN_KEYS
                    .iter()
                    .any(|forbidden| key.eq_ignore_ascii_case(forbidden))
                {
                    return Err(format!("{at}.{key} is not allowed in a responses profile"));
                }
                reject_secrets(child, &format!("{at}.{key}"))?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                reject_secrets(child, &format!("{at}[{index}]"))?;
            }
            Ok(())
        }
        Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            if lower.contains("bearer ") || lower.starts_with("sk-") {
                Err(format!("{at} contains a credential-like value"))
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}
