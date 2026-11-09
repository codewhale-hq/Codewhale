//! Behavior tests for the route foundation (#2608 / #3084 / #3384).

use super::descriptor::ProviderDescriptor;
use super::errors::RouteError;
use super::ids::{LogicalModelRef, ModelId, NamespaceHint, ProviderId, WireModelId};
use super::resolver::{RouteRequest, RouteResolver};
use super::{
    CapabilityState, LimitField, OverrideSource, RequestProtocol, ResolvedAuthSource,
    RouteCapabilities, SourcedLimitOverride,
};
use crate::ProviderKind;
use crate::models_dev::ModelsDevCatalog;

/// Build a request with only an explicit provider + a model selector string.
fn req(provider: Option<ProviderKind>, model: Option<&str>) -> RouteRequest {
    RouteRequest {
        explicit_provider: provider,
        model_selector: model.map(LogicalModelRef::from),
        saved_provider_model: None,
        base_url_override: None,
        limit_overrides: Vec::new(),
        wire_override: None,
        responses_profile_override: None,
    }
}

fn models_dev_route_resolver() -> RouteResolver {
    let raw = r#"{
      "providers": {
        "zai": {
          "models": {
            "glm-5.2": {
              "id": "glm-5.2",
              "base_model": "zhipuai/glm-5.2",
              "default": true,
              "modalities": { "input": ["text"], "output": ["text"] },
              "limit": { "context": 1000000, "input": 900000, "output": 131072 }
            }
          }
        },
        "openrouter": {
          "models": {
            "z-ai/glm-5.2": {
              "id": "z-ai/glm-5.2",
              "base_model": "zhipuai/glm-5.2",
              "modalities": { "input": ["text"], "output": ["text"] },
              "limit": { "context": 128000, "output": 32768 }
            }
          }
        }
      }
    }"#;
    let catalog = ModelsDevCatalog::parse_json(raw).expect("Models.dev fixture parses");
    let mut offerings = catalog
        .provider_offerings("zai")
        .expect("zai provider offerings");
    offerings.extend(
        catalog
            .provider_offerings("openrouter")
            .expect("openrouter provider offerings"),
    );
    RouteResolver::from_offerings(offerings)
}

/// Concentrate ids pass through verbatim (plain catalog id, upstream
/// `provider/model` pin) and only the gateway's own `concentrate/` namespace
/// is stripped, so `concentrate/auto` reaches the gateway's `auto` router
/// while Codewhale's bare `auto` stays the resolver sentinel (provider
/// default). Every selector rides the Responses protocol.
#[test]
fn concentrate_passes_ids_through_and_strips_only_its_own_namespace() {
    let r = models_dev_route_resolver();
    for (raw, wire) in [
        ("gpt-5.6-sol", "gpt-5.6-sol"),
        ("openai/gpt-5.6-sol", "openai/gpt-5.6-sol"),
        ("anthropic/claude-fable-5", "anthropic/claude-fable-5"),
        ("concentrate/auto", "auto"),
        ("concentrate/deepseek-v4-pro", "deepseek-v4-pro"),
    ] {
        let out = r
            .resolve(&req(Some(ProviderKind::Concentrate), Some(raw)))
            .unwrap_or_else(|e| panic!("{raw} should resolve on concentrate: {e}"));
        assert_eq!(out.provider_kind(), ProviderKind::Concentrate, "{raw}");
        assert_eq!(out.wire_model_id().as_str(), wire, "{raw} wire id");
        assert_eq!(
            out.protocol(),
            RequestProtocol::Responses,
            "{raw} must ride the Responses wire"
        );
        assert_eq!(
            out.responses_profile_id().map(|id| id.as_str()),
            Some("concentrate"),
            "{raw} profile"
        );
    }

    // Codewhale's own `auto` sentinel resolves to the provider default, never
    // to a literal "auto" wire id.
    let out = r
        .resolve(&req(Some(ProviderKind::Concentrate), Some("auto")))
        .expect("auto resolves");
    assert_eq!(out.wire_model_id().as_str(), "deepseek-v4-pro");
    assert_eq!(out.protocol(), RequestProtocol::Responses);
}

#[test]
fn provider_id_from_kind_uses_canonical_id() {
    assert_eq!(
        ProviderId::from_kind(ProviderKind::Deepseek).as_str(),
        "deepseek"
    );
    assert_eq!(
        ProviderId::from_kind(ProviderKind::Openrouter).as_str(),
        "openrouter"
    );
}

#[test]
fn model_id_and_wire_model_id_are_distinct_types() {
    // This test asserts the values; the *type* distinction is enforced by the
    // compiler: a function taking `WireModelId` rejects a `ModelId` argument.
    let canonical = ModelId::from("deepseek-v4-pro");
    let wire = WireModelId::from("deepseek-ai/DeepSeek-V4-Pro");
    assert_eq!(canonical.as_str(), "deepseek-v4-pro");
    assert_eq!(wire.as_str(), "deepseek-ai/DeepSeek-V4-Pro");
}

#[test]
fn logical_model_ref_auto_is_sentinel() {
    assert!(LogicalModelRef::from("auto").is_auto());
    assert!(!LogicalModelRef::from("deepseek-v4-pro").is_auto());
}

#[test]
fn logical_model_ref_namespace_hint_parses_curated_prefixes() {
    let cases = [
        ("deepseek-ai/DeepSeek-V4-Pro", NamespaceHint::DeepseekAi),
        ("deepseek/deepseek-v4-pro", NamespaceHint::Deepseek),
        ("anthropic/claude-foo", NamespaceHint::Anthropic),
        ("openai/gpt-foo", NamespaceHint::Openai),
        ("qwen/qwen-foo", NamespaceHint::Qwen),
    ];
    for (raw, expected) in cases {
        assert_eq!(
            LogicalModelRef::from(raw).namespace_hint(),
            Some(expected),
            "{raw} should parse to {expected:?}"
        );
    }
    assert_eq!(LogicalModelRef::from("plain-model").namespace_hint(), None);
    assert_eq!(LogicalModelRef::from("auto").namespace_hint(), None);
}

/// By construction there is NO path from a namespace prefix to a provider.
///
/// This is enforced by the *absence* of any `From<NamespaceHint>` /
/// `From<LogicalModelRef>` for `ProviderId`. The following lines are the
/// canonical way to mint a `ProviderId` and demonstrate the only supported
/// source is an explicit `ProviderKind`, never a parsed prefix.
#[test]
fn no_namespace_hint_or_logical_ref_to_provider_id_conversion() {
    let hint = LogicalModelRef::from("deepseek-ai/DeepSeek-V4-Pro").namespace_hint();
    assert_eq!(hint, Some(NamespaceHint::DeepseekAi));
    // A ProviderId may ONLY be built from an explicit ProviderKind or string,
    // never derived from the hint above. (If a `From<NamespaceHint>` for
    // `ProviderId` were ever added, this seam would silently break #2608.)
    let provider = ProviderId::from_kind(ProviderKind::Together);
    assert_eq!(provider.as_str(), "together");
}

#[test]
fn newtypes_serialize_transparently() {
    let id = ProviderId::from("deepseek");
    assert_eq!(serde_json::to_string(&id).unwrap(), "\"deepseek\"");
    let wire = WireModelId::from("deepseek-ai/DeepSeek-V4-Pro");
    assert_eq!(
        serde_json::to_string(&wire).unwrap(),
        "\"deepseek-ai/DeepSeek-V4-Pro\""
    );
}

#[test]
fn resolver_reports_auth_as_unresolved_until_preflight() {
    let candidate = RouteResolver::new()
        .resolve(&req(Some(ProviderKind::Deepseek), Some("deepseek-v4-pro")))
        .expect("route resolves");

    assert_eq!(candidate.auth(), &ResolvedAuthSource::Unresolved);
    assert_eq!(
        serde_json::to_value(&candidate).expect("candidate serializes")["auth"],
        "unresolved"
    );
}

#[test]
fn resolver_applies_every_limit_override_field_before_construction() {
    let mut request = req(Some(ProviderKind::Deepseek), Some("deepseek-v4-pro"));
    request.limit_overrides = vec![
        SourcedLimitOverride {
            field: LimitField::ContextTokens,
            value: Some(111),
            source: OverrideSource::UserContextWindow,
        },
        SourcedLimitOverride {
            field: LimitField::InputTokens,
            value: Some(222),
            source: OverrideSource::CodexPublicApiLimitStrip,
        },
        SourcedLimitOverride {
            field: LimitField::OutputTokens,
            value: Some(333),
            source: OverrideSource::CodexRosterCorrection,
        },
    ];

    let candidate = RouteResolver::new()
        .resolve(&request)
        .expect("route with overrides resolves");

    assert_eq!(candidate.limits().context_tokens, Some(111));
    assert_eq!(candidate.limits().input_tokens, Some(222));
    assert_eq!(candidate.limits().output_tokens, Some(333));
}

#[test]
fn resolver_applies_repeated_limit_overrides_in_request_order() {
    let mut request = req(Some(ProviderKind::Deepseek), Some("deepseek-v4-pro"));
    request.limit_overrides = vec![
        SourcedLimitOverride {
            field: LimitField::ContextTokens,
            value: Some(111),
            source: OverrideSource::CodexRosterCorrection,
        },
        SourcedLimitOverride {
            field: LimitField::ContextTokens,
            value: None,
            source: OverrideSource::CodexPublicApiLimitStrip,
        },
        SourcedLimitOverride {
            field: LimitField::ContextTokens,
            value: Some(333),
            source: OverrideSource::UserContextWindow,
        },
    ];

    let candidate = RouteResolver::new()
        .resolve(&request)
        .expect("route with ordered overrides resolves");

    assert_eq!(candidate.limits().context_tokens, Some(333));
}

#[test]
fn resolver_records_exact_limit_override_provenance() {
    let overrides = vec![
        SourcedLimitOverride {
            field: LimitField::InputTokens,
            value: None,
            source: OverrideSource::CodexPublicApiLimitStrip,
        },
        SourcedLimitOverride {
            field: LimitField::ContextTokens,
            value: Some(128_000),
            source: OverrideSource::CodexRosterCorrection,
        },
    ];
    let mut request = req(Some(ProviderKind::OpenaiCodex), None);
    request.limit_overrides = overrides.clone();

    let candidate = RouteResolver::new()
        .resolve(&request)
        .expect("Codex route with sourced overrides resolves");

    assert_eq!(candidate.applied_limit_overrides(), overrides.as_slice());
    assert_eq!(candidate.protocol(), RequestProtocol::Responses);
    assert_eq!(
        candidate.responses_profile_id().map(|id| id.as_str()),
        Some("codex")
    );
    assert_ne!(candidate.endpoint().endpoint_key, "responses");
}

#[test]
fn resolved_auth_source_existing_variants_keep_wire_compatibility() {
    let cases = [
        (ResolvedAuthSource::Cli, "cli"),
        (ResolvedAuthSource::ConfigFile, "config_file"),
        (ResolvedAuthSource::Keyring, "keyring"),
        (ResolvedAuthSource::Env, "env"),
        (ResolvedAuthSource::Command, "command"),
        (ResolvedAuthSource::Secret, "secret"),
        (ResolvedAuthSource::Missing, "missing"),
    ];

    for (source, wire) in cases {
        let encoded = serde_json::to_string(&source).expect("auth source serializes");
        assert_eq!(encoded, format!("\"{wire}\""));
        let decoded: ResolvedAuthSource =
            serde_json::from_str(&encoded).expect("auth source deserializes");
        assert_eq!(decoded, source);
    }
}

#[test]
fn descriptor_for_every_kind_has_nonempty_transport_facts() {
    for kind in ProviderKind::ALL {
        let d = ProviderDescriptor::for_kind(kind);
        assert!(!d.id().as_str().is_empty(), "{kind:?} id empty");
        assert!(
            !d.default_base_url().is_empty(),
            "{kind:?} default_base_url empty"
        );
        assert!(
            !d.default_wire_model().as_str().is_empty(),
            "{kind:?} default_wire_model empty"
        );
        if d.wire_policy().fixed().is_some() {
            let _: RequestProtocol = d
                .protocol_for_endpoint("chat")
                .expect("fixed provider protocol");
        }
    }
}

#[test]
fn descriptor_protocol_matches_provider_wire() {
    for kind in ProviderKind::ALL {
        let d = ProviderDescriptor::for_kind(kind);
        if matches!(
            kind,
            ProviderKind::Deepseek
                | ProviderKind::OpencodeZen
                | ProviderKind::OpencodeGo
                | ProviderKind::Codewhale
        ) {
            assert_eq!(d.wire_policy(), crate::provider::WirePolicy::ModelAware);
            assert_eq!(
                d.protocol_for_endpoint("chat"),
                Some(RequestProtocol::ChatCompletions)
            );
            assert_eq!(
                d.protocol_for_endpoint("responses"),
                Some(RequestProtocol::Responses)
            );
            if kind == ProviderKind::Deepseek {
                assert_eq!(
                    d.protocol_for_endpoint("messages"),
                    Some(RequestProtocol::AnthropicMessages)
                );
                assert_eq!(d.protocol_for_endpoint("models/gemini"), None);
                continue;
            }
            assert_eq!(
                d.protocol_for_endpoint("messages"),
                Some(RequestProtocol::AnthropicMessages)
            );
            assert_eq!(d.protocol_for_endpoint("models/gemini"), None);
            continue;
        }
        assert_eq!(
            d.protocol_for_endpoint("chat"),
            kind.provider().wire_policy().fixed(),
            "{kind:?} protocol must equal the provider wire policy"
        );
        let expected = match kind {
            ProviderKind::OpenaiCodex | ProviderKind::Concentrate => {
                Some(RequestProtocol::Responses)
            }
            ProviderKind::DeepseekAnthropic
            | ProviderKind::Anthropic
            | ProviderKind::MinimaxAnthropic
            | ProviderKind::Openmodel
            | ProviderKind::ModelstudioTokenPlanAnthropic
            | ProviderKind::ModelstudioCodingPlanAnthropic => {
                Some(RequestProtocol::AnthropicMessages)
            }
            _ => Some(RequestProtocol::ChatCompletions),
        };
        assert_eq!(
            d.protocol_for_endpoint("chat"),
            expected,
            "{kind:?} protocol mismatch"
        );
    }
}

#[test]
fn resolver_explicit_provider_scoped_model_maps_to_wire_id() {
    let r = RouteResolver::new();
    let out = r
        .resolve(&req(Some(ProviderKind::Deepseek), Some("deepseek-v4-pro")))
        .expect("should resolve");
    assert_eq!(out.provider_kind(), ProviderKind::Deepseek);
    assert_eq!(out.wire_model_id().as_str(), "deepseek-v4-pro");
    assert_eq!(
        out.canonical_model().map(ModelId::as_str),
        Some("deepseek-v4-pro")
    );
}

#[test]
fn resolver_routes_only_official_deepseek_flash_over_responses() {
    let resolver = RouteResolver::new();

    let default_flash = resolver
        .resolve(&req(Some(ProviderKind::Deepseek), Some("deepseek-flash")))
        .expect("official default Flash route resolves");
    assert_eq!(default_flash.protocol(), RequestProtocol::Responses);
    assert_eq!(default_flash.endpoint().endpoint_key, "responses");
    assert_eq!(
        default_flash.responses_profile_id().map(|id| id.as_str()),
        Some("deepseek_stateless_plain_reasoning")
    );
    assert_eq!(
        default_flash.capabilities().server_side_web_search,
        CapabilityState::Supported
    );

    let flash = resolver
        .resolve(&req(
            Some(ProviderKind::Deepseek),
            Some("deepseek-v4-flash"),
        ))
        .expect("official Flash route resolves");
    assert_eq!(flash.protocol(), RequestProtocol::Responses);
    assert_eq!(flash.endpoint().endpoint_key, "responses");
    assert_eq!(
        flash.responses_profile_id().map(|id| id.as_str()),
        Some("deepseek_stateless_plain_reasoning")
    );
    assert_eq!(
        flash.capabilities().server_side_web_search,
        CapabilityState::Supported
    );

    let pro = resolver
        .resolve(&req(Some(ProviderKind::Deepseek), Some("deepseek-v4-pro")))
        .expect("official Pro route resolves");
    assert_eq!(pro.protocol(), RequestProtocol::ChatCompletions);
    assert_eq!(pro.endpoint().endpoint_key, "chat");
    assert!(pro.responses_profile_id().is_none());
    assert_eq!(
        pro.capabilities().server_side_web_search,
        CapabilityState::Supported
    );

    let future = resolver
        .resolve(&req(
            Some(ProviderKind::Deepseek),
            Some("deepseek-v5-future"),
        ))
        .expect("unknown future model preserves direct-provider pass-through");
    assert_eq!(future.protocol(), RequestProtocol::ChatCompletions);
    assert_eq!(future.endpoint().endpoint_key, "chat");
    assert!(future.responses_profile_id().is_none());

    let legacy_unknown = resolver
        .resolve(&req(
            Some(ProviderKind::Deepseek),
            Some("deepseek-coder-future"),
        ))
        .expect("unknown unversioned model preserves historical Chat pass-through");
    assert_eq!(legacy_unknown.protocol(), RequestProtocol::ChatCompletions);
    assert_eq!(legacy_unknown.endpoint().endpoint_key, "chat");
    assert!(legacy_unknown.responses_profile_id().is_none());

    let custom = resolver
        .resolve(&RouteRequest {
            explicit_provider: Some(ProviderKind::Deepseek),
            model_selector: Some(LogicalModelRef::from("deepseek-v4-flash")),
            saved_provider_model: None,
            base_url_override: Some("https://compatible.example/v1".to_string()),
            limit_overrides: Vec::new(),
            wire_override: None,
            responses_profile_override: None,
        })
        .expect("custom compatible Flash route remains pass-through");
    assert_eq!(custom.protocol(), RequestProtocol::ChatCompletions);
    assert_eq!(custom.endpoint().endpoint_key, "chat");
    assert!(custom.responses_profile_id().is_none());
    assert_eq!(
        custom.capabilities().server_side_web_search,
        CapabilityState::Unknown
    );
}

#[test]
fn resolver_sources_deepseek_files_api_only_on_exact_official_flash_routes() {
    let resolver = RouteResolver::new();

    for model in ["deepseek-flash", "deepseek-v4-flash"] {
        let route = resolver
            .resolve(&req(Some(ProviderKind::Deepseek), Some(model)))
            .expect("official Flash route resolves");
        assert_eq!(
            route.capabilities().files_api,
            CapabilityState::Supported,
            "{model}"
        );
    }

    for model in [
        "deepseek-v4-pro",
        "deepseek-v4-flash-vision-exp",
        "deepseek-v5-future",
    ] {
        let route = resolver
            .resolve(&req(Some(ProviderKind::Deepseek), Some(model)))
            .expect("direct DeepSeek route resolves");
        assert_eq!(
            route.capabilities().files_api,
            CapabilityState::Unknown,
            "{model} must not inherit the Files API fact"
        );
    }

    let custom = resolver
        .resolve(&RouteRequest {
            explicit_provider: Some(ProviderKind::Deepseek),
            model_selector: Some(LogicalModelRef::from("deepseek-flash")),
            saved_provider_model: None,
            base_url_override: Some("https://compatible.example/v1".to_string()),
            limit_overrides: Vec::new(),
            wire_override: None,
            responses_profile_override: None,
        })
        .expect("custom compatible endpoint resolves");
    assert_eq!(custom.capabilities().files_api, CapabilityState::Unknown);

    let aggregator = resolver
        .resolve(&req(
            Some(ProviderKind::Openrouter),
            Some("deepseek/deepseek-v4-flash"),
        ))
        .expect("aggregator route resolves");
    assert_eq!(
        aggregator.capabilities().files_api,
        CapabilityState::Unknown
    );
}

#[test]
fn resolver_uncatalogued_deepseek_preview_keeps_chat_and_verbatim_model() {
    let model = "deepseek-v4.1-flash-expires-on-0910";
    for base_url in [
        "https://api.deepseek.com",
        "https://api.deepseek.com/v1",
        "https://api.deepseek.com/beta",
    ] {
        let route = RouteResolver::new()
            .resolve(&RouteRequest {
                base_url_override: Some(base_url.to_string()),
                ..req(Some(ProviderKind::Deepseek), Some(model))
            })
            .expect("uncatalogued preview keeps direct-provider pass-through");
        assert_eq!(route.provider_kind(), ProviderKind::Deepseek);
        assert_eq!(route.wire_model_id().as_str(), model);
        assert!(
            route.canonical_model().is_none(),
            "no fallback to a known model"
        );
        assert_eq!(route.protocol(), RequestProtocol::ChatCompletions);
        assert_eq!(route.endpoint().endpoint_key, "chat");
        assert_eq!(route.endpoint().base_url, base_url);
        assert_eq!(route.capabilities().image_input, CapabilityState::Unknown);
        assert_eq!(route.limits().output_tokens, None);
    }
}

#[test]
fn resolver_routes_deepseek_vision_exp_over_chat_with_image_input() {
    for base_url_override in [
        None,
        Some("https://api.deepseek.com/v1"),
        Some("https://api.deepseek.com/beta"),
    ] {
        let route = RouteResolver::new()
            .resolve(&RouteRequest {
                explicit_provider: Some(ProviderKind::Deepseek),
                model_selector: Some(LogicalModelRef::from("deepseek-v4-flash-vision-exp")),
                saved_provider_model: None,
                base_url_override: base_url_override.map(str::to_string),
                limit_overrides: Vec::new(),
                wire_override: None,
                responses_profile_override: None,
            })
            .expect("experimental vision route resolves");

        assert_eq!(route.provider_kind(), ProviderKind::Deepseek);
        assert_eq!(
            route.canonical_model().map(ModelId::as_str),
            Some("deepseek-v4-flash-vision-exp")
        );
        assert_eq!(
            route.wire_model_id().as_str(),
            "deepseek-v4-flash-vision-exp"
        );
        assert_eq!(route.protocol(), RequestProtocol::ChatCompletions);
        assert_eq!(route.endpoint().endpoint_key, "chat");
        assert_eq!(
            route.capabilities().image_input,
            CapabilityState::Supported,
            "official endpoint {base_url_override:?} must retain the exact vision fact"
        );
        assert_eq!(route.capabilities().reasoning, CapabilityState::Supported);
        assert_eq!(
            route.capabilities().native_tool_calls,
            CapabilityState::Supported
        );
        assert_eq!(route.limits().context_tokens, Some(1_000_000));
        assert_eq!(route.limits().output_tokens, Some(384_000));
    }
}

#[test]
fn resolver_keeps_deepseek_flash_image_input_on_the_official_route() {
    // #6421: the curated DeepSeek rows win over the Models.dev asset, so the
    // resolved route (what the engine strips images against) must carry the
    // documented Flash vision fact itself — default selector included.
    for selector in [None, Some("deepseek-flash"), Some("deepseek-v4-flash")] {
        let route = RouteResolver::new()
            .resolve(&req(Some(ProviderKind::Deepseek), selector))
            .expect("official DeepSeek Flash route resolves");
        assert_eq!(
            route.capabilities().image_input,
            CapabilityState::Supported,
            "{selector:?} must keep image input on the official endpoint"
        );
    }
    let pro = RouteResolver::new()
        .resolve(&req(Some(ProviderKind::Deepseek), Some("deepseek-v4-pro")))
        .expect("official DeepSeek Pro route resolves");
    assert_eq!(
        pro.capabilities().image_input,
        CapabilityState::Unsupported,
        "the Flash correction must not widen Pro"
    );
}

#[test]
fn resolver_keeps_deepseek_flash_image_input_on_the_messages_route() {
    // #6521 review: Flash vision is documented for Messages too. Both
    // spellings of that route must resolve with image input — the legacy
    // `deepseek-anthropic` kind and canonical DeepSeek with `wire =
    // "anthropic"` (which selects the `/anthropic` base URL) — while a custom
    // compatible host and Pro stay unwidened.
    let messages = |kind: ProviderKind, selector: &str, base_url: Option<&str>| {
        let mut request = req(Some(kind), Some(selector));
        request.base_url_override = base_url.map(str::to_string);
        RouteResolver::new()
            .resolve(&request)
            .expect("DeepSeek Messages route resolves")
            .capabilities()
            .image_input
    };
    for selector in ["deepseek-flash", "deepseek-v4-flash"] {
        assert_eq!(
            messages(ProviderKind::DeepseekAnthropic, selector, None),
            CapabilityState::Supported,
            "deepseek-anthropic {selector}"
        );
        assert_eq!(
            messages(
                ProviderKind::Deepseek,
                selector,
                Some("https://api.deepseek.com/anthropic")
            ),
            CapabilityState::Supported,
            "deepseek wire=anthropic {selector}"
        );
        assert_ne!(
            messages(
                ProviderKind::Deepseek,
                selector,
                Some("https://proxy.example.com/anthropic")
            ),
            CapabilityState::Supported,
            "a custom compatible host is not DeepSeek's documented route"
        );
    }
    assert_ne!(
        messages(ProviderKind::DeepseekAnthropic, "deepseek-v4-pro", None),
        CapabilityState::Supported,
        "Pro stays text-only on Messages"
    );
}

#[test]
fn resolver_keeps_custom_deepseek_same_name_capabilities_unverified() {
    let route = RouteResolver::new()
        .resolve(&RouteRequest {
            explicit_provider: Some(ProviderKind::Deepseek),
            model_selector: Some(LogicalModelRef::from("deepseek-v4-flash-vision-exp")),
            saved_provider_model: None,
            base_url_override: Some("https://deepseek-proxy.example.test/v1".to_string()),
            limit_overrides: Vec::new(),
            wire_override: None,
            responses_profile_override: None,
        })
        .expect("same-name custom proxy route resolves");

    assert_eq!(route.protocol(), RequestProtocol::ChatCompletions);
    assert_eq!(route.endpoint().endpoint_key, "chat");
    assert_eq!(
        route.wire_model_id().as_str(),
        "deepseek-v4-flash-vision-exp"
    );
    assert_eq!(
        route.capabilities(),
        RouteCapabilities::default(),
        "a custom proxy cannot inherit first-party capability facts by reusing the model id"
    );
}

#[test]
fn resolver_aggregator_preserves_prefixed_wire_id_without_inferring_deepseek() {
    let r = RouteResolver::new();
    let out = r
        .resolve(&req(
            Some(ProviderKind::Together),
            Some("deepseek-ai/DeepSeek-V4-Pro"),
        ))
        .expect("aggregator should resolve");
    // Provider stays Together, NOT Deepseek, despite the deepseek-ai/ prefix.
    assert_eq!(out.provider_kind(), ProviderKind::Together);
    assert_ne!(out.provider_kind(), ProviderKind::Deepseek);
    // Wire id preserved verbatim.
    assert_eq!(out.wire_model_id().as_str(), "deepseek-ai/DeepSeek-V4-Pro");
}

#[test]
fn resolver_openrouter_keeps_provider_for_every_namespace_prefix() {
    let r = RouteResolver::new();
    let prefixes = [
        "deepseek-ai/DeepSeek-V4-Pro",
        "deepseek/deepseek-v4-pro",
        "anthropic/claude-foo",
        "openai/gpt-foo",
        "qwen/qwen-foo",
    ];
    for raw in prefixes {
        let selector = LogicalModelRef::from(raw);
        // The selector DOES carry a namespace hint...
        assert!(
            selector.namespace_hint().is_some(),
            "{raw} should have a namespace hint"
        );
        let out = r
            .resolve(&req(Some(ProviderKind::Openrouter), Some(raw)))
            .unwrap_or_else(|e| panic!("{raw} should resolve on openrouter: {e}"));
        // ...but the provider stays Openrouter regardless.
        assert_eq!(
            out.provider_kind(),
            ProviderKind::Openrouter,
            "{raw} must not change provider"
        );
        assert_eq!(out.wire_model_id().as_str(), raw, "{raw} wire id verbatim");
    }
}

#[test]
fn resolver_no_explicit_provider_does_not_infer_deepseek_from_prefix() {
    let r = RouteResolver::new();
    // explicit_provider=None => default scope (Deepseek). A prefixed selector
    // is foreign for the strict-direct default, so it ERRORS rather than being
    // silently accepted as a deepseek model: the prefix never *selects* it.
    let out = r.resolve(&req(None, Some("deepseek/deepseek-v4-pro")));
    match out {
        Err(RouteError::ForeignModelForDirectProvider { provider, model }) => {
            assert_eq!(provider.as_str(), "deepseek");
            assert_eq!(model, "deepseek/deepseek-v4-pro");
        }
        other => panic!("expected ForeignModelForDirectProvider, got {other:?}"),
    }
}

#[test]
fn resolver_auto_is_sentinel_not_literal_model() {
    let r = RouteResolver::new();
    let out = r
        .resolve(&req(Some(ProviderKind::Deepseek), Some("auto")))
        .expect("auto should resolve");
    // The logical selector is the auto sentinel...
    assert!(out.logical_model().is_auto());
    // ...and "auto" is NOT put on the wire as a literal model.
    assert_ne!(out.wire_model_id().as_str(), "auto");
    assert_eq!(out.wire_model_id().as_str(), "deepseek-flash");
    assert_eq!(out.protocol(), RequestProtocol::Responses);
}

#[test]
fn resolver_can_use_models_dev_offering_for_provider_scoped_route() {
    let r = models_dev_route_resolver();
    let out = r
        .resolve(&req(Some(ProviderKind::Zai), Some("glm-5.2")))
        .expect("Models.dev-backed Z.ai route should resolve");

    assert_eq!(out.provider_kind(), ProviderKind::Zai);
    assert_eq!(out.provider_id().as_str(), "zai");
    assert_eq!(out.wire_model_id().as_str(), "glm-5.2");
    assert_eq!(
        out.canonical_model().map(ModelId::as_str),
        Some("zhipuai/glm-5.2")
    );
}

#[test]
fn resolver_auto_uses_models_dev_default_offering_when_available() {
    let r = models_dev_route_resolver();
    let out = r
        .resolve(&req(Some(ProviderKind::Zai), Some("auto")))
        .expect("auto should resolve through catalog default");

    assert!(out.logical_model().is_auto());
    assert_eq!(
        out.wire_model_id().as_str(),
        "glm-5.2",
        "catalog default should win over the built-in Z.ai spelling"
    );
    assert_eq!(
        out.canonical_model().map(ModelId::as_str),
        Some("zhipuai/glm-5.2")
    );
}

#[test]
fn resolver_auto_falls_back_to_descriptor_default_without_catalog_default() {
    // Z.ai offerings exist in the catalog snapshot but none is marked
    // `default: true`. `auto` must then fall back to the provider descriptor's
    // built-in default wire model rather than picking an arbitrary catalog row.
    let raw = r#"{
      "providers": {
        "zai": {
          "models": {
            "glm-5-turbo": {
              "id": "glm-5-turbo",
              "modalities": { "input": ["text"], "output": ["text"] }
            }
          }
        }
      }
    }"#;
    let catalog = ModelsDevCatalog::parse_json(raw).expect("Models.dev fixture parses");
    let offerings = catalog
        .provider_offerings("zai")
        .expect("zai provider offerings");
    let r = RouteResolver::from_offerings(offerings);

    let out = r
        .resolve(&req(Some(ProviderKind::Zai), Some("auto")))
        .expect("auto should resolve to the descriptor default");

    assert!(out.logical_model().is_auto());
    assert_eq!(
        out.wire_model_id().as_str(),
        "GLM-5.3",
        "no catalog default → descriptor built-in default wins"
    );
    assert_eq!(
        out.canonical_model(),
        None,
        "descriptor fallback carries no catalog canonical link"
    );
}

#[test]
fn resolver_models_dev_prefixed_wire_id_stays_inside_provider_scope() {
    let r = models_dev_route_resolver();
    let out = r
        .resolve(&req(Some(ProviderKind::Openrouter), Some("z-ai/glm-5.2")))
        .expect("OpenRouter Models.dev row should resolve");

    assert_eq!(out.provider_kind(), ProviderKind::Openrouter);
    assert_ne!(out.provider_kind(), ProviderKind::Zai);
    assert_eq!(out.wire_model_id().as_str(), "z-ai/glm-5.2");
    assert_eq!(
        out.canonical_model().map(ModelId::as_str),
        Some("zhipuai/glm-5.2")
    );
}

#[test]
fn resolver_carries_models_dev_limits_into_ready_candidate() {
    let r = models_dev_route_resolver();
    let out = r
        .resolve(&req(Some(ProviderKind::Zai), Some("glm-5.2")))
        .expect("Z.AI Models.dev row should resolve");

    assert_eq!(out.limits().context_tokens, Some(1_000_000));
    assert_eq!(out.limits().input_tokens, Some(900_000));
    assert_eq!(out.limits().output_tokens, Some(131_072));
    assert!(out.limits().has_known_limit());
}

#[test]
fn minimax_anthropic_routes_use_catalog_limits_and_messages_protocol() {
    let resolver = RouteResolver::new();

    for (model, context) in [("MiniMax-M3", 1_000_000), ("MiniMax-M2.7", 204_800)] {
        let route = resolver
            .resolve(&req(Some(ProviderKind::MinimaxAnthropic), Some(model)))
            .expect("MiniMax Messages route should resolve");

        assert_eq!(route.provider_kind(), ProviderKind::MinimaxAnthropic);
        assert_eq!(route.wire_model_id().as_str(), model);
        assert_eq!(route.protocol(), RequestProtocol::AnthropicMessages);
        assert_eq!(
            route.endpoint().protocol,
            RequestProtocol::AnthropicMessages
        );
        assert_eq!(
            route.endpoint().base_url,
            "https://api.minimax.io/anthropic"
        );
        assert_eq!(route.limits().context_tokens, Some(context));
    }
}

#[test]
fn resolver_keeps_limits_provider_scoped_for_same_canonical_model() {
    let r = models_dev_route_resolver();
    let direct = r
        .resolve(&req(Some(ProviderKind::Zai), Some("glm-5.2")))
        .expect("direct Z.AI route should resolve");
    let hosted = r
        .resolve(&req(Some(ProviderKind::Openrouter), Some("z-ai/glm-5.2")))
        .expect("hosted OpenRouter route should resolve");

    assert_eq!(
        direct.canonical_model().map(ModelId::as_str),
        hosted.canonical_model().map(ModelId::as_str)
    );
    assert_eq!(direct.limits().context_tokens, Some(1_000_000));
    assert_eq!(hosted.limits().context_tokens, Some(128_000));
    assert_eq!(hosted.limits().output_tokens, Some(32_768));
}

#[test]
fn resolver_strict_direct_rejects_clearly_foreign_selector() {
    let r = RouteResolver::new();
    let out = r.resolve(&req(Some(ProviderKind::Zai), Some("anthropic/claude-foo")));
    match out {
        Err(RouteError::ForeignModelForDirectProvider { provider, model }) => {
            assert_eq!(provider.as_str(), "zai");
            assert_eq!(model, "anthropic/claude-foo");
        }
        other => panic!("expected ForeignModelForDirectProvider, got {other:?}"),
    }
}

#[test]
fn resolver_direct_owned_row_match_survives_casing_mismatch() {
    let r = RouteResolver::new();
    let out = r
        .resolve(&req(Some(ProviderKind::Zai), Some("glm-5.2")))
        .expect("lowercase selector on the owning Z.ai row must resolve");
    assert_eq!(out.provider_kind(), ProviderKind::Zai);
    assert_eq!(out.wire_model_id().as_str(), "GLM-5.2");
    assert!(out.limits().has_known_limit());

    let saved = RouteRequest {
        saved_provider_model: Some(WireModelId::from("glm-5.2")),
        ..req(Some(ProviderKind::Zai), None)
    };
    let out = r
        .resolve(&saved)
        .expect("saved lowercase Z.ai model must resolve");
    assert_eq!(out.wire_model_id().as_str(), "GLM-5.2");

    let out = r
        .resolve(&req(Some(ProviderKind::Deepseek), Some("Deepseek-V4-Pro")))
        .expect("DeepSeek's own casing variant must resolve");
    assert_eq!(out.provider_kind(), ProviderKind::Deepseek);
    assert_eq!(out.wire_model_id().as_str(), "deepseek-v4-pro");

    let custom = RouteRequest {
        explicit_provider: Some(ProviderKind::Zai),
        model_selector: Some(LogicalModelRef::from("glm-5.2")),
        saved_provider_model: None,
        base_url_override: Some("https://compatible.example.test/v1".to_string()),
        limit_overrides: Vec::new(),
        wire_override: None,
        responses_profile_override: None,
    };
    let out = r
        .resolve(&custom)
        .expect("custom endpoint keeps model pass-through semantics");
    assert_eq!(out.wire_model_id().as_str(), "glm-5.2");
    assert!(!out.limits().has_known_limit());
}

#[test]
fn resolver_direct_casefold_match_requires_one_owned_row() {
    let raw = r#"{
      "providers": {
        "zai": {
          "models": {
            "CaseModel": {
              "id": "CaseModel",
              "modalities": { "input": ["text"], "output": ["text"] },
              "limit": { "context": 1000 }
            },
            "casemodel": {
              "id": "casemodel",
              "modalities": { "input": ["text"], "output": ["text"] },
              "limit": { "context": 2000 }
            }
          }
        }
      }
    }"#;
    let catalog = ModelsDevCatalog::parse_json(raw).expect("Models.dev fixture parses");
    let offerings = catalog
        .provider_offerings("zai")
        .expect("Z.ai provider offerings");
    let r = RouteResolver::from_offerings(offerings);

    let out = r
        .resolve(&req(Some(ProviderKind::Zai), Some("CASEMODEL")))
        .expect("ambiguous casefold stays an unknown direct model");
    assert_eq!(out.wire_model_id().as_str(), "CASEMODEL");
    assert!(
        !out.limits().has_known_limit(),
        "ambiguous rows must not lend arbitrary catalog metadata"
    );
}

#[test]
fn resolver_strict_direct_rejects_other_provider_known_bare_offering() {
    let r = RouteResolver::new();
    let out = r.resolve(&req(Some(ProviderKind::Zai), Some("deepseek-v4-pro")));
    match out {
        Err(RouteError::ForeignModelForDirectProvider { provider, model }) => {
            assert_eq!(provider.as_str(), "zai");
            assert_eq!(model, "deepseek-v4-pro");
        }
        other => panic!("expected ForeignModelForDirectProvider, got {other:?}"),
    }
}

#[test]
fn resolver_custom_endpoint_allows_namespaced_selector_for_strict_provider() {
    let r = RouteResolver::new();
    let request = RouteRequest {
        explicit_provider: Some(ProviderKind::Deepseek),
        model_selector: Some(LogicalModelRef::from("vendor/custom-coder")),
        saved_provider_model: None,
        base_url_override: Some("https://example.local/v1".to_string()),
        limit_overrides: Vec::new(),
        wire_override: None,
        responses_profile_override: None,
    };
    let out = r
        .resolve(&request)
        .expect("custom endpoint should defer model validation upstream");
    assert_eq!(out.provider_kind(), ProviderKind::Deepseek);
    assert_eq!(out.wire_model_id().as_str(), "vendor/custom-coder");
    assert_eq!(out.endpoint().base_url, "https://example.local/v1");
}

#[test]
fn resolver_treats_every_official_deepseek_endpoint_as_strict_direct() {
    let resolver = RouteResolver::new();
    for base_url in [
        "https://api.deepseek.com",
        "https://api.deepseek.com/v1/",
        "https://api.deepseek.com/beta",
    ] {
        let request = RouteRequest {
            explicit_provider: Some(ProviderKind::Deepseek),
            model_selector: Some(LogicalModelRef::from("anthropic/claude-foo")),
            saved_provider_model: None,
            base_url_override: Some(base_url.to_string()),
            limit_overrides: Vec::new(),
            wire_override: None,
            responses_profile_override: None,
        };
        assert!(
            matches!(
                resolver.resolve(&request),
                Err(RouteError::ForeignModelForDirectProvider { .. })
            ),
            "official endpoint {base_url} must retain DeepSeek's strict namespace"
        );
    }
}

#[test]
fn resolver_does_not_trust_deepseek_hostname_substrings() {
    let resolver = RouteResolver::new();
    let request = RouteRequest {
        explicit_provider: Some(ProviderKind::Deepseek),
        model_selector: Some(LogicalModelRef::from("vendor/custom-coder")),
        saved_provider_model: None,
        base_url_override: Some("https://api.deepseek.com.evil.example/v1".to_string()),
        limit_overrides: Vec::new(),
        wire_override: None,
        responses_profile_override: None,
    };
    let route = resolver
        .resolve(&request)
        .expect("lookalike host must be treated as a custom endpoint");
    assert_eq!(route.wire_model_id().as_str(), "vendor/custom-coder");
}

#[test]
fn resolver_explicit_custom_with_base_url_override_passes_model_through_verbatim() {
    // #1519: an explicit `Custom` provider with a base_url override resolves via
    // the LocalOrCustom pass-through, preserving even a namespaced selector as
    // the verbatim wire id and binding the override endpoint + Chat Completions.
    let r = RouteResolver::new();
    let request = RouteRequest {
        explicit_provider: Some(ProviderKind::Custom),
        model_selector: Some(LogicalModelRef::from("vendor/custom-model-v1")),
        saved_provider_model: None,
        base_url_override: Some("https://api.example.com/v1".to_string()),
        limit_overrides: Vec::new(),
        wire_override: None,
        responses_profile_override: None,
    };
    let out = r
        .resolve(&request)
        .expect("custom provider should resolve via pass-through");
    assert_eq!(out.provider_kind(), ProviderKind::Custom);
    assert_eq!(out.provider_id().as_str(), "custom");
    assert_eq!(out.wire_model_id().as_str(), "vendor/custom-model-v1");
    assert_eq!(out.endpoint().base_url, "https://api.example.com/v1");
    assert_eq!(
        out.protocol(),
        crate::route::RequestProtocol::ChatCompletions
    );
    assert!(out.responses_profile_id().is_none());
    assert!(out.validation().ok);
    assert!(out.validation().messages.is_empty());
}

#[test]
fn custom_wire_override_mints_a_wire_true_candidate() {
    // A named `[providers.<name>]` table's `wire` setting reaches the
    // resolver through `RouteRequest::wire_override`: a table saying
    // `wire = "responses"` (or `"anthropic"`) must mint a candidate whose
    // protocol speaks that wire, even on a relay base URL, instead of
    // Custom's backward-compatible static Chat policy.
    let r = RouteResolver::new();
    for (dialect, protocol) in [
        (
            crate::provider::WireFormat::Responses,
            crate::route::RequestProtocol::Responses,
        ),
        (
            crate::provider::WireFormat::AnthropicMessages,
            crate::route::RequestProtocol::AnthropicMessages,
        ),
    ] {
        let request = RouteRequest {
            explicit_provider: Some(ProviderKind::Custom),
            model_selector: Some(LogicalModelRef::from("gpt-5.5")),
            saved_provider_model: None,
            base_url_override: Some("https://relay.example.com/v1".to_string()),
            limit_overrides: Vec::new(),
            wire_override: Some(dialect),
            responses_profile_override: None,
        };
        let out = r
            .resolve(&request)
            .expect("custom provider should resolve via pass-through");
        assert_eq!(out.protocol(), protocol, "{dialect:?} must mint its wire");
        assert_eq!(
            out.responses_profile_id().map(|id| id.as_str()),
            (protocol == RequestProtocol::Responses).then_some("standard"),
            "{dialect:?} profile"
        );
        assert_eq!(out.endpoint().base_url, "https://relay.example.com/v1");
        assert_eq!(out.wire_model_id().as_str(), "gpt-5.5");
        assert!(out.validation().ok);
        // The override must not resurrect first-party catalog facts.
        assert!(out.canonical_model().is_none());
    }
}

#[test]
fn custom_responses_profile_override_stays_on_the_allowlist() {
    let resolver = RouteResolver::new();
    let mut request = RouteRequest {
        explicit_provider: Some(ProviderKind::Custom),
        model_selector: Some(LogicalModelRef::from("gpt-5.5")),
        saved_provider_model: None,
        base_url_override: Some("https://relay.example.com/v1".to_string()),
        limit_overrides: Vec::new(),
        wire_override: Some(crate::provider::WireFormat::Responses),
        responses_profile_override: Some("standard".to_string()),
    };
    let standard = resolver.resolve(&request).expect("standard profile");
    assert_eq!(
        standard.responses_profile_id().map(|id| id.as_str()),
        Some("standard")
    );
    request.responses_profile_override = None;
    let omitted = resolver.resolve(&request).expect("default profile");
    assert_eq!(
        omitted.responses_profile_id().map(|id| id.as_str()),
        Some("standard")
    );
    request.responses_profile_override = Some("codex".to_string());
    let denied = resolver.resolve(&request).expect_err("codex is not custom");
    assert!(denied.to_string().contains("allowed: standard"), "{denied}");
    request.wire_override = None;
    request.responses_profile_override = Some("standard".to_string());
    let chat = resolver
        .resolve(&request)
        .expect_err("profile requires responses");
    assert!(chat.to_string().contains("not responses"), "{chat}");
}

#[test]
fn wire_override_is_ignored_for_builtin_kinds() {
    // Built-ins keep their descriptor policy: a caller-supplied override can
    // neither promote a fixed-Chat provider nor demote a fixed-Responses one.
    let r = RouteResolver::new();
    let request = RouteRequest {
        explicit_provider: Some(ProviderKind::Deepseek),
        model_selector: Some(LogicalModelRef::from("deepseek-chat")),
        saved_provider_model: None,
        base_url_override: None,
        limit_overrides: Vec::new(),
        wire_override: Some(crate::provider::WireFormat::Responses),
        responses_profile_override: None,
    };
    let out = r.resolve(&request).expect("deepseek resolves");
    assert_eq!(
        out.protocol(),
        crate::route::RequestProtocol::ChatCompletions,
        "a builtin's descriptor policy wins over the override"
    );
}

#[test]
fn custom_wire_override_defaults_to_the_chat_policy() {
    // Absent or explicit-chat dialects keep Custom's backward-compatible
    // static Chat policy; the shared parse degrades unknowns deliberately.
    let r = RouteResolver::new();
    for wire_override in [None, Some(crate::provider::WireFormat::ChatCompletions)] {
        let request = RouteRequest {
            explicit_provider: Some(ProviderKind::Custom),
            model_selector: Some(LogicalModelRef::from("vendor/custom-model")),
            saved_provider_model: None,
            base_url_override: Some("https://api.example.com/v1".to_string()),
            limit_overrides: Vec::new(),
            wire_override,
            responses_profile_override: None,
        };
        let out = r.resolve(&request).expect("custom resolves");
        assert_eq!(
            out.protocol(),
            crate::route::RequestProtocol::ChatCompletions,
            "{wire_override:?} must keep the static Chat policy"
        );
    }
}

#[test]
fn resolver_strict_direct_rejects_models_dev_offering_from_another_provider() {
    let r = models_dev_route_resolver();
    let out = r.resolve(&req(Some(ProviderKind::Deepseek), Some("glm-5.2")));
    match out {
        Err(RouteError::ForeignModelForDirectProvider { provider, model }) => {
            assert_eq!(provider.as_str(), "deepseek");
            assert_eq!(model, "glm-5.2");
        }
        other => panic!("expected ForeignModelForDirectProvider, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// #3385: the DEFAULT resolver now sources the bundled Models.dev catalog asset,
// so real provider/model facts (context windows) reach candidates.
// ---------------------------------------------------------------------------

#[test]
fn default_resolver_yields_real_facts_from_bundled_catalog() {
    let r = RouteResolver::new();

    // A GLM row (Z.ai) resolves to a real, non-default context window — proof
    // the bundled asset feeds the default resolver rather than the old 4-row
    // seam, which only knew deepseek/together/openrouter and left everything
    // else at `RouteLimits::default()` (unknown).
    let glm = r
        .resolve(&req(Some(ProviderKind::Zai), Some("GLM-5.2")))
        .expect("Z.ai GLM-5.2 should resolve from the bundled catalog");
    assert_eq!(glm.provider_kind(), ProviderKind::Zai);
    assert_eq!(glm.wire_model_id().as_str(), "GLM-5.2");
    assert_eq!(
        glm.limits().context_tokens,
        Some(1_000_000),
        "GLM-5.2 must carry its real context window, not the unknown default"
    );
    assert_eq!(glm.limits().output_tokens, Some(131_072));
    assert!(glm.limits().has_known_limit());

    // A Kimi row (Moonshot) likewise resolves with its real window — a model
    // the 4-row seam never knew about at all.
    let kimi_k27 = r
        .resolve(&req(Some(ProviderKind::Moonshot), Some("kimi-k2.7-code")))
        .expect("Moonshot kimi-k2.7-code should resolve from the bundled catalog");
    assert_eq!(kimi_k27.limits().context_tokens, Some(262_144));
    assert_eq!(kimi_k27.limits().output_tokens, Some(262_144));

    let kimi_k3 = r
        .resolve(&req(Some(ProviderKind::Moonshot), Some("kimi-k3")))
        .expect("Moonshot kimi-k3 should resolve from the bundled catalog");
    assert_eq!(kimi_k3.limits().context_tokens, Some(1_048_576));
    assert_eq!(kimi_k3.limits().output_tokens, Some(1_048_576));

    // With the #3085 pricing keystone present on the release branch, the asset's
    // provider-scoped `cost` now projects onto the candidate via
    // `route_pricing_sku`, so a priced Z.ai row carries a real per-token meter
    // rather than `UnknownOrStale` — the "lighting up" that #3385 + #3085 deliver
    // together.
    let glm51 = r
        .resolve(&req(Some(ProviderKind::Zai), Some("glm-5.1")))
        .expect("Z.ai glm-5.1 should resolve from the bundled catalog");
    assert_eq!(glm51.limits().context_tokens, Some(200_000));
    assert!(matches!(
        glm51.pricing(),
        Some(super::candidate::PricingSku::Token { .. })
    ));
}

#[test]
fn default_resolver_preserves_seam_canonical_joins() {
    // The bundled asset is merged UNDER the hand seam, so the seam's curated
    // canonical-model joins still win: a DeepSeek-native selector keeps its
    // canonical id, and an aggregator-prefixed wire id still maps back to the
    // canonical DeepSeek model. (This is what keeps the existing route
    // invariants green after the asset was wired in.)
    let r = RouteResolver::new();

    let direct = r
        .resolve(&req(Some(ProviderKind::Deepseek), Some("deepseek-v4-pro")))
        .expect("deepseek-v4-pro resolves");
    assert_eq!(
        direct.canonical_model().map(ModelId::as_str),
        Some("deepseek-v4-pro")
    );

    let hosted = r
        .resolve(&req(
            Some(ProviderKind::Together),
            Some("deepseek-ai/DeepSeek-V4-Pro"),
        ))
        .expect("together hosted deepseek resolves");
    assert_eq!(
        hosted.canonical_model().map(ModelId::as_str),
        Some("deepseek-v4-pro"),
        "seam canonical join must survive the asset merge"
    );
    assert_eq!(
        hosted.wire_model_id().as_str(),
        "deepseek-ai/DeepSeek-V4-Pro"
    );
}

#[test]
fn together_inkling_aliases_use_the_exact_wire_identity_without_invented_metadata() {
    let resolver = RouteResolver::new();

    for requested in ["inkling", "together-inkling", "thinkingmachines/inkling"] {
        let route = resolver
            .resolve(&req(Some(ProviderKind::Together), Some(requested)))
            .expect("Together Inkling route should resolve");
        assert_eq!(route.provider_kind(), ProviderKind::Together, "{requested}");
        assert_eq!(
            route.wire_model_id().as_str(),
            "thinkingmachines/inkling",
            "{requested}"
        );
        assert!(route.canonical_model().is_none(), "{requested}");
        assert!(!route.limits().has_known_limit(), "{requested}");
        assert!(matches!(
            route.pricing(),
            Some(super::candidate::PricingSku::UnknownOrStale)
        ));
    }
}

#[test]
fn together_custom_endpoint_preserves_its_explicit_model_id() {
    let resolver = RouteResolver::new();
    let route = resolver
        .resolve(&RouteRequest {
            explicit_provider: Some(ProviderKind::Together),
            model_selector: Some(LogicalModelRef::from("inkling")),
            saved_provider_model: None,
            base_url_override: Some("http://127.0.0.1:8000/v1".to_string()),
            limit_overrides: Vec::new(),
            wire_override: None,
            responses_profile_override: None,
        })
        .expect("custom Together-compatible endpoint should resolve");

    assert_eq!(route.wire_model_id().as_str(), "inkling");
}

#[test]
fn openrouter_qwen37_plus_aliases_use_exact_catalog_wire_identity() {
    let resolver = RouteResolver::new();

    for requested in ["qwen3.7-plus", "qwen-3.7-plus", "qwen/qwen3.7-plus"] {
        let route = resolver
            .resolve(&req(Some(ProviderKind::Openrouter), Some(requested)))
            .expect("OpenRouter Qwen 3.7 Plus route should resolve");
        assert_eq!(route.wire_model_id().as_str(), "qwen/qwen3.7-plus");
        assert_eq!(route.limits().context_tokens, Some(1_000_000));
        assert!(matches!(
            route.pricing(),
            Some(super::candidate::PricingSku::Token {
                input_per_mtok: Some(_),
                output_per_mtok: Some(_)
            })
        ));
    }
}

#[test]
fn openrouter_custom_endpoint_preserves_qwen37_alias() {
    let route = RouteResolver::new()
        .resolve(&RouteRequest {
            explicit_provider: Some(ProviderKind::Openrouter),
            model_selector: Some(LogicalModelRef::from("qwen3.7-plus")),
            saved_provider_model: None,
            base_url_override: Some("https://gateway.example.test/v1".to_string()),
            limit_overrides: Vec::new(),
            wire_override: None,
            responses_profile_override: None,
        })
        .expect("custom OpenRouter-compatible endpoint should resolve");

    assert_eq!(route.wire_model_id().as_str(), "qwen3.7-plus");
}

#[test]
fn opencode_go_resolver_preserves_documented_chat_routes() {
    let resolver = RouteResolver::new();
    // Literal review fixture: the 2026-09-08 endpoint table plus previously
    // accepted IDs retained for compatibility. Do not derive from the contract.
    let chat_models = [
        "deepseek-v4-pro",
        "grok-4.5",
        "glm-5.2",
        "glm-5.1",
        "kimi-k3",
        "kimi-k2.7-code",
        "kimi-k2.6",
        "deepseek-v4-flash",
        "mimo-v2.5",
        "mimo-v2.5-pro",
        "glm-5.3-flash",
        "glm-5.3",
        "longcat-2.0",
        "deepseek-v4-flash-vision-exp",
        "hy4-preview",
        "hy3",
        "omen-alpha",
    ];
    assert!(
        chat_models
            .iter()
            .all(|id| crate::opencode_go_models().contains(id))
    );

    for model in chat_models {
        for requested in [model.to_string(), format!("opencode-go/{model}")] {
            let route = resolver
                .resolve(&req(Some(ProviderKind::OpencodeGo), Some(&requested)))
                .unwrap_or_else(|error| panic!("{requested} should resolve: {error}"));
            assert_eq!(
                route.provider_kind(),
                ProviderKind::OpencodeGo,
                "{requested}"
            );
            assert_eq!(route.wire_model_id().as_str(), model, "{requested}");
            assert_eq!(route.protocol(), RequestProtocol::ChatCompletions);
        }
    }

    let automatic = resolver
        .resolve(&req(Some(ProviderKind::OpencodeGo), Some("auto")))
        .expect("OpenCode Go auto should resolve to its Chat default");
    assert_eq!(automatic.wire_model_id().as_str(), "deepseek-v4-pro");
}

#[test]
fn opencode_go_resolver_uses_documented_protocols_and_refuses_unknown_models() {
    let resolver = RouteResolver::new();
    let fixtures = [
        ("deepseek-v4.1-flash", RequestProtocol::ChatCompletions),
        ("grok-4.6", RequestProtocol::Responses),
        ("gpt-5.6-luna", RequestProtocol::Responses),
        ("muse-spark-1.3-contributor", RequestProtocol::Responses),
        ("muse-spark-1.2-contributor", RequestProtocol::Responses),
        ("minimax-m3", RequestProtocol::AnthropicMessages),
        ("minimax-m2.7", RequestProtocol::AnthropicMessages),
        ("minimax-m2.5", RequestProtocol::AnthropicMessages),
        ("qwen3.8-max", RequestProtocol::AnthropicMessages),
        ("qwen3.8-flash", RequestProtocol::AnthropicMessages),
        ("qwen3.7-max", RequestProtocol::AnthropicMessages),
        ("qwen3.7-plus", RequestProtocol::AnthropicMessages),
        ("qwen3.6-plus", RequestProtocol::AnthropicMessages),
    ];
    for (model, protocol) in fixtures {
        for requested in [model.to_string(), format!("opencode-go/{model}")] {
            for base_url_override in [None, Some("https://go-gateway.example.test/v1".to_string())]
            {
                let request = RouteRequest {
                    explicit_provider: Some(ProviderKind::OpencodeGo),
                    model_selector: Some(LogicalModelRef::from(requested.as_str())),
                    base_url_override,
                    ..RouteRequest::default()
                };
                let route = resolver.resolve(&request).expect("documented Go route");
                assert_eq!(route.wire_model_id().as_str(), model);
                assert_eq!(route.protocol(), protocol, "{requested}");
                if request.base_url_override.is_some() {
                    assert!(
                        !route.limits().has_known_limit(),
                        "custom endpoint has no sourced limits"
                    );
                }
            }
        }
    }
    for model in [
        "claude-unproven",
        "opencode-go/unknown",
        "openai/gpt-5.6-luna",
    ] {
        for base_url_override in [None, Some("https://go-gateway.example.test/v1".into())] {
            assert!(
                resolver
                    .resolve(&RouteRequest {
                        explicit_provider: Some(ProviderKind::OpencodeGo),
                        model_selector: Some(LogicalModelRef::from(model)),
                        base_url_override,
                        ..RouteRequest::default()
                    })
                    .is_err(),
                "unknown {model} must not select a default or another wire"
            );
        }
    }
}

#[test]
fn opencode_go_protocol_cannot_be_downgraded_by_stale_catalog_metadata() {
    let mut offerings = super::offering::bundled_offerings();
    let row = offerings
        .iter_mut()
        .find(|row| {
            row.provider.as_str() == "opencode-go" && row.wire_model_id.as_str() == "minimax-m3"
        })
        .unwrap();
    row.endpoint_key = "chat".into();
    let route = RouteResolver::from_offerings(offerings)
        .resolve(&req(Some(ProviderKind::OpencodeGo), Some("minimax-m3")))
        .unwrap();
    assert_eq!(route.protocol(), RequestProtocol::AnthropicMessages);
}

#[test]
fn codewhale_route_picks_its_wire_per_model_and_stays_open_to_the_account_catalog() {
    use super::CODEWHALE_FALLBACK_MODELS;

    let resolver = RouteResolver::new();
    for model in CODEWHALE_FALLBACK_MODELS {
        let route = resolver
            .resolve(&req(Some(ProviderKind::Codewhale), Some(model)))
            .unwrap_or_else(|error| panic!("{model} should resolve: {error}"));
        assert_eq!(route.provider_kind(), ProviderKind::Codewhale);
        // Ids pass through exactly as the account catalog returns them.
        assert_eq!(route.wire_model_id().as_str(), *model);
        let expected = if model.starts_with("anthropic/") {
            RequestProtocol::AnthropicMessages
        } else {
            RequestProtocol::ChatCompletions
        };
        assert_eq!(route.protocol(), expected, "{model}");
    }

    let automatic = resolver
        .resolve(&req(Some(ProviderKind::Codewhale), Some("auto")))
        .expect("the Codewhale route has a bootstrap default");
    assert_eq!(
        automatic.wire_model_id().as_str(),
        "deepseek/deepseek-v4-pro"
    );
    assert_eq!(automatic.protocol(), RequestProtocol::ChatCompletions);

    // A provider the account connected after this build shipped must still
    // resolve: the account catalog, not a compiled roster, owns the list.
    let unseen = resolver
        .resolve(&req(Some(ProviderKind::Codewhale), Some("xai/grok-4.6")))
        .expect("an account-connected provider this build never heard of");
    assert_eq!(unseen.wire_model_id().as_str(), "xai/grok-4.6");
    assert_eq!(unseen.protocol(), RequestProtocol::ChatCompletions);
    let unseen_anthropic = resolver
        .resolve(&req(
            Some(ProviderKind::Codewhale),
            Some("anthropic/claude-opus-4-8"),
        ))
        .expect("the Anthropic namespace routes to the Messages passthrough");
    assert_eq!(
        unseen_anthropic.protocol(),
        RequestProtocol::AnthropicMessages
    );
}

#[test]
fn codewhale_api_base_env_refuses_cleartext_off_loopback() {
    use crate::provider::codewhale_api_base;

    assert_eq!(
        codewhale_api_base("https://api.codewhale.net/v1/").as_deref(),
        Some("https://api.codewhale.net/v1")
    );
    assert_eq!(
        codewhale_api_base("http://127.0.0.1:8787/v1").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert!(codewhale_api_base("http://localhost:8787/v1").is_some());
    for refused in [
        "",
        "   ",
        "http://api.codewhale.net/v1",
        "https://user:pass@api.codewhale.net/v1",
        "ftp://api.codewhale.net/v1",
        "not a url",
    ] {
        assert!(codewhale_api_base(refused).is_none(), "{refused:?}");
    }
}

#[test]
fn opencode_zen_resolver_selects_protocol_from_documented_model_catalog() {
    use crate::catalog::reviewed::constants::{
        OPENCODE_ZEN_CHAT_MODELS, OPENCODE_ZEN_MESSAGES_MODELS, OPENCODE_ZEN_RESPONSES_MODELS,
    };

    let resolver = RouteResolver::new();
    let groups = [
        (
            OPENCODE_ZEN_RESPONSES_MODELS,
            "responses",
            RequestProtocol::Responses,
        ),
        (
            OPENCODE_ZEN_MESSAGES_MODELS,
            "messages",
            RequestProtocol::AnthropicMessages,
        ),
        (
            OPENCODE_ZEN_CHAT_MODELS,
            "chat",
            RequestProtocol::ChatCompletions,
        ),
    ];

    for (models, endpoint_key, protocol) in groups {
        for model in models {
            for requested in [model.to_string(), format!("opencode/{model}")] {
                let route = resolver
                    .resolve(&req(Some(ProviderKind::OpencodeZen), Some(&requested)))
                    .unwrap_or_else(|error| panic!("{requested} should resolve: {error}"));
                assert_eq!(route.provider_kind(), ProviderKind::OpencodeZen);
                assert_eq!(route.wire_model_id().as_str(), *model, "{requested}");
                assert_eq!(route.endpoint().endpoint_key, endpoint_key, "{requested}");
                assert_eq!(route.protocol(), protocol, "{requested}");
            }
        }
    }

    let automatic = resolver
        .resolve(&req(Some(ProviderKind::OpencodeZen), Some("auto")))
        .expect("OpenCode Zen auto should resolve to its documented default");
    assert_eq!(automatic.wire_model_id().as_str(), "gpt-5.6-sol");
    assert_eq!(automatic.protocol(), RequestProtocol::Responses);

    let logical_gpt_5_6 = resolver
        .resolve(&req(Some(ProviderKind::OpencodeZen), Some("gpt-5.6")))
        .expect("the logical GPT-5.6 picker row resolves to Zen's proven Sol wire route");
    assert_eq!(logical_gpt_5_6.wire_model_id().as_str(), "gpt-5.6-sol");
    assert_eq!(logical_gpt_5_6.protocol(), RequestProtocol::Responses);
}

#[test]
fn opencode_zen_resolver_fails_closed_for_unproven_protocols() {
    let resolver = RouteResolver::new();

    for model in [
        "gemini-3.1-pro",
        "opencode/gemini-3.5-flash",
        "unknown-model",
    ] {
        for base_url_override in [
            None,
            Some("https://zen-gateway.example.test/v1".to_string()),
        ] {
            let request = RouteRequest {
                explicit_provider: Some(ProviderKind::OpencodeZen),
                model_selector: Some(LogicalModelRef::from(model)),
                saved_provider_model: None,
                base_url_override,
                limit_overrides: Vec::new(),
                wire_override: None,
                responses_profile_override: None,
            };
            assert!(
                matches!(
                    resolver.resolve(&request),
                    Err(RouteError::UnsupportedModelProtocol { .. })
                ),
                "{model} must fail closed without a supported protocol mapping"
            );
        }
    }
}

/// #6705: a Zen model newer than this build routes on the wire its Models.dev
/// catalog row names; the catalog's Google and deprecated rows and ids it
/// never lists still fail closed, with the endpoint named.
#[test]
fn opencode_zen_routes_models_dev_rows_on_their_declared_wire() {
    let raw = r#"{
      "providers": {
        "opencode": {
          "id": "opencode",
          "npm": "@ai-sdk/openai-compatible",
          "models": {
            "gpt-9-nova": { "id": "gpt-9-nova", "provider": { "npm": "@ai-sdk/openai" } },
            "claude-opus-9": { "id": "claude-opus-9", "provider": { "npm": "@ai-sdk/anthropic" } },
            "moon-rabbit-free": { "id": "moon-rabbit-free" },
            "gemini-9-flash": { "id": "gemini-9-flash", "provider": { "npm": "@ai-sdk/google" } },
            "odd-sdk-model": { "id": "odd-sdk-model", "provider": { "npm": "@ai-sdk/unknown" } },
            "claude-2-retired": {
              "id": "claude-2-retired",
              "status": "deprecated",
              "provider": { "npm": "@ai-sdk/anthropic" }
            }
          }
        }
      }
    }"#;
    let catalog = ModelsDevCatalog::parse_json(raw).expect("fixture parses");
    let mut offerings: Vec<_> = crate::catalog::live_offerings_from_models_dev(&catalog, 1)
        .iter()
        .map(crate::catalog::CatalogOffering::to_offering)
        .collect();
    offerings.extend(super::bundled_offerings());
    let resolver = RouteResolver::from_offerings(offerings);

    for (model, protocol) in [
        ("gpt-9-nova", RequestProtocol::Responses),
        ("opencode/claude-opus-9", RequestProtocol::AnthropicMessages),
        ("moon-rabbit-free", RequestProtocol::ChatCompletions),
        // A curated-only row still routes beside the catalog's rows.
        ("gpt-5.6-sol", RequestProtocol::Responses),
    ] {
        let route = resolver
            .resolve(&req(Some(ProviderKind::OpencodeZen), Some(model)))
            .unwrap_or_else(|error| panic!("{model} should resolve: {error}"));
        assert_eq!(route.protocol(), protocol, "{model}");
    }

    for (model, endpoint) in [
        ("gemini-9-flash", "google"),
        ("odd-sdk-model", "unproven"),
        ("never-listed", "unproven"),
        // Deprecated rows stay in the catalog but are not routes.
        ("claude-2-retired", "deprecated"),
    ] {
        match resolver.resolve(&req(Some(ProviderKind::OpencodeZen), Some(model))) {
            Err(RouteError::UnsupportedModelProtocol { endpoint_key, .. }) => {
                assert_eq!(endpoint_key, endpoint, "{model}");
            }
            other => panic!("{model} must fail closed, got {other:?}"),
        }
    }
}

#[test]
fn resolver_deepseek_none_selector_uses_default_wire_id() {
    let r = RouteResolver::new();
    let out = r
        .resolve(&req(Some(ProviderKind::Deepseek), None))
        .expect("none selector should use provider default");
    assert_eq!(out.provider_kind(), ProviderKind::Deepseek);
    assert_eq!(out.wire_model_id().as_str(), "deepseek-flash");
    assert_eq!(out.protocol(), RequestProtocol::Responses);
}

#[test]
fn resolver_empty_string_selector_is_empty_model_error() {
    let r = RouteResolver::new();
    let out = r.resolve(&req(Some(ProviderKind::Deepseek), Some("")));
    assert!(matches!(out, Err(RouteError::EmptyModel)));
}

#[test]
fn resolver_rejects_retired_antigravity_before_minting_a_route() {
    let error = RouteResolver::new()
        .resolve(&req(Some(ProviderKind::Antigravity), Some("gemini-3-pro")))
        .expect_err("the legacy tombstone must never produce an executable route");
    let rendered = error.to_string();
    assert!(matches!(error, RouteError::InvalidProvider(_)));
    assert!(rendered.contains("non-runnable"), "{rendered}");
    assert!(
        rendered.contains("auth clear --provider antigravity"),
        "{rendered}"
    );
    assert!(rendered.contains("google"), "{rendered}");
    assert!(rendered.contains("GEMINI_API_KEY"), "{rendered}");
}

#[test]
fn resolver_empty_saved_provider_model_is_empty_model_error() {
    // An empty selector from the saved-model fallback must be rejected too, not
    // just an empty explicit selector (the guard covers every selector source).
    let r = RouteResolver::new();
    let request = RouteRequest {
        explicit_provider: Some(ProviderKind::Deepseek),
        model_selector: None,
        saved_provider_model: Some(WireModelId::from("")),
        base_url_override: None,
        limit_overrides: Vec::new(),
        wire_override: None,
        responses_profile_override: None,
    };
    assert!(matches!(r.resolve(&request), Err(RouteError::EmptyModel)));
}

#[test]
fn resolver_passthrough_provider_preserves_custom_id_verbatim() {
    let r = RouteResolver::new();
    let out = r
        .resolve(&req(Some(ProviderKind::Ollama), Some("my-local:7b")))
        .expect("local passthrough should resolve");
    assert_eq!(out.provider_kind(), ProviderKind::Ollama);
    assert_eq!(out.wire_model_id().as_str(), "my-local:7b");
    assert_eq!(out.limits(), Default::default());
    assert!(out.validation().ok);
}

#[test]
fn resolved_candidate_serializes_secret_free() {
    let r = RouteResolver::new();
    // Cover a direct, an aggregator, and a local/passthrough route.
    let candidates = [
        r.resolve(&req(Some(ProviderKind::Deepseek), Some("deepseek-v4-pro")))
            .expect("direct resolves"),
        r.resolve(&req(
            Some(ProviderKind::Together),
            Some("deepseek-ai/DeepSeek-V4-Pro"),
        ))
        .expect("aggregator resolves"),
        r.resolve(&req(Some(ProviderKind::Ollama), Some("my-local:7b")))
            .expect("local resolves"),
    ];
    for out in candidates {
        let json = serde_json::to_string(&out).expect("candidate serializes");
        // Carries provider/model/wire/protocol/auth-source class.
        assert!(json.contains("provider_id"), "{json}");
        assert!(json.contains("provider_kind"), "{json}");
        assert!(json.contains("wire_model_id"), "{json}");
        assert!(json.contains("protocol"), "{json}");
        assert!(json.contains("auth"), "{json}");
        // Never any secret/api-key material.
        let lower = json.to_lowercase();
        assert!(!lower.contains("api_key"), "leaked api_key: {json}");
        assert!(!lower.contains("apikey"), "leaked apikey: {json}");
        assert!(!lower.contains("secret_id"), "leaked secret_id: {json}");
        assert!(!lower.contains("password"), "leaked password: {json}");
        assert!(!lower.contains("bearer"), "leaked bearer: {json}");
        assert!(
            !lower.contains("authorization"),
            "leaked authorization: {json}"
        );
    }
}

#[test]
fn resolver_protocol_matches_descriptor_for_every_provider() {
    let r = RouteResolver::new();
    for kind in ProviderKind::ALL {
        // Use each provider's own default wire id as the selector so strict
        // direct providers do not reject; this exercises the resolver across
        // the whole provider set.
        let default_wire = ProviderDescriptor::for_kind(kind).default_wire_model();
        let request = req(Some(kind), Some(default_wire.as_str()));
        let out = r
            .resolve(&request)
            .unwrap_or_else(|e| panic!("{kind:?} should resolve its own default: {e}"));
        assert_eq!(
            out.protocol(),
            ProviderDescriptor::for_kind(kind)
                .protocol_for_endpoint(&out.endpoint().endpoint_key)
                .expect("resolved endpoint protocol"),
            "{kind:?} candidate protocol must match descriptor"
        );
        assert_eq!(
            out.endpoint().protocol,
            out.protocol(),
            "{kind:?} endpoint protocol"
        );
    }
}

// ---------------------------------------------------------------------------
// #3085: honest pricing on resolved candidates.
// ---------------------------------------------------------------------------

/// A resolver whose single offering is a DeepSeek-priced catalog row, projected
/// through the wired `CatalogOffering::to_offering` pricing seam.
fn priced_deepseek_resolver() -> RouteResolver {
    use crate::catalog::{CatalogOffering, CatalogSource};
    use crate::models_dev::ModelsDevCost;

    let priced = CatalogOffering {
        provider: "deepseek".into(),
        wire_model_id: "deepseek-v4-pro".into(),
        canonical_model: Some("deepseek-v4-pro".into()),
        endpoint_key: "chat".into(),
        default_for_provider: true,
        cost: Some(ModelsDevCost {
            input: Some(0.28),
            output: Some(0.42),
            cache_read: Some(0.028),
            cache_write: None,
            ..Default::default()
        }),
        source: CatalogSource::Bundled,
        ..Default::default()
    };
    RouteResolver::from_offerings(vec![priced.to_offering()])
}

#[test]
fn resolver_carries_exact_offering_capabilities_without_protocol_inference() {
    use crate::catalog::{CatalogOffering, CatalogSource};
    use crate::route::CapabilityState;

    let offering = CatalogOffering {
        provider: "deepseek".into(),
        wire_model_id: "deepseek-v4-pro".into(),
        canonical_model: Some("deepseek-v4-pro".into()),
        endpoint_key: "chat".into(),
        default_for_provider: true,
        attachment: Some(false),
        reasoning: Some(true),
        tool_call: Some(false),
        structured_output: Some(true),
        source: CatalogSource::Bundled,
        ..Default::default()
    };
    let resolver = RouteResolver::from_offerings(vec![offering.to_offering()]);

    let candidate = resolver
        .resolve(&req(Some(ProviderKind::Deepseek), Some("deepseek-v4-pro")))
        .expect("matching offering resolves");
    let capabilities = candidate.capabilities();

    assert_eq!(capabilities.attachments, CapabilityState::Unsupported);
    assert_eq!(capabilities.reasoning, CapabilityState::Supported);
    assert_eq!(capabilities.native_tool_calls, CapabilityState::Unsupported);
    assert_eq!(capabilities.structured_output, CapabilityState::Supported);
    assert_eq!(capabilities.streaming, CapabilityState::Unknown);
    assert_eq!(
        capabilities.server_side_web_search,
        CapabilityState::Supported
    );
}

#[test]
fn provider_native_web_search_requires_exact_direct_endpoint_offering() {
    use crate::route::CapabilityState;

    let resolver = RouteResolver::new();
    let automatic = resolver
        .resolve(&req(Some(ProviderKind::Xai), None))
        .expect("xAI default resolves");
    assert_eq!(automatic.wire_model_id().as_str(), "grok-4.7");

    let direct = resolver
        .resolve(&req(Some(ProviderKind::Xai), Some("grok-4.6")))
        .expect("bundled direct xAI offering resolves");
    assert_eq!(direct.wire_model_id().as_str(), "grok-4.6");
    assert_eq!(
        direct.capabilities().attachments,
        CapabilityState::Supported
    );
    assert_eq!(
        direct.capabilities().image_input,
        CapabilityState::Supported
    );
    assert_eq!(direct.capabilities().reasoning, CapabilityState::Supported);
    assert_eq!(
        direct.capabilities().native_tool_calls,
        CapabilityState::Supported
    );
    assert_eq!(
        direct.capabilities().structured_output,
        CapabilityState::Supported
    );
    assert_eq!(
        direct.capabilities().server_side_web_search,
        CapabilityState::Supported
    );

    let aggregator = resolver
        .resolve(&req(Some(ProviderKind::OpencodeGo), Some("grok-4.5")))
        .expect("aggregator offering resolves independently");
    assert_eq!(
        aggregator.capabilities().server_side_web_search,
        CapabilityState::Unknown
    );

    let custom_endpoint = resolver
        .resolve(&RouteRequest {
            explicit_provider: Some(ProviderKind::Xai),
            model_selector: Some(LogicalModelRef::from("grok-4.6")),
            saved_provider_model: None,
            base_url_override: Some("https://gateway.example.test/v1".to_string()),
            limit_overrides: Vec::new(),
            wire_override: None,
            responses_profile_override: None,
        })
        .expect("custom compatible endpoint resolves");
    assert_eq!(
        custom_endpoint.capabilities().server_side_web_search,
        CapabilityState::Unknown
    );
}

#[test]
fn mimo_native_search_is_exact_to_documented_chat_models() {
    use crate::route::CapabilityState;

    let resolver = RouteResolver::new();
    for model in ["mimo-v2.6-pro", "mimo-v2.6-flash"] {
        let direct = resolver
            .resolve(&req(Some(ProviderKind::XiaomiMimo), Some(model)))
            .expect("official MiMo chat route resolves");
        assert_eq!(
            direct.capabilities().server_side_web_search,
            CapabilityState::Supported
        );
    }

    let neighboring = resolver
        .resolve(&req(
            Some(ProviderKind::XiaomiMimo),
            Some("mimo-v2.5-pro-ultraspeed"),
        ))
        .expect("neighboring MiMo model resolves");
    assert_eq!(
        neighboring.capabilities().server_side_web_search,
        CapabilityState::Unknown
    );

    let custom = resolver
        .resolve(&RouteRequest {
            explicit_provider: Some(ProviderKind::XiaomiMimo),
            model_selector: Some(LogicalModelRef::from("mimo-v2.5-pro")),
            saved_provider_model: None,
            base_url_override: Some("https://compatible.example.test/v1".to_string()),
            limit_overrides: Vec::new(),
            wire_override: None,
            responses_profile_override: None,
        })
        .expect("custom MiMo-compatible route resolves");
    assert_eq!(
        custom.capabilities().server_side_web_search,
        CapabilityState::Unknown
    );
}

#[test]
fn zai_native_search_requires_exact_general_api_product() {
    use crate::route::CapabilityState;

    let resolver = RouteResolver::new();
    for base_url in [
        "https://api.z.ai/api/paas/v4",
        "https://open.bigmodel.cn/api/paas/v4",
    ] {
        let direct = resolver
            .resolve(&RouteRequest {
                explicit_provider: Some(ProviderKind::Zai),
                model_selector: Some(LogicalModelRef::from("GLM-5.3")),
                saved_provider_model: None,
                base_url_override: Some(base_url.to_string()),
                limit_overrides: Vec::new(),
                wire_override: None,
                responses_profile_override: None,
            })
            .expect("general API route resolves");
        assert_eq!(
            direct.capabilities().server_side_web_search,
            CapabilityState::Supported,
            "{base_url} should expose structured web search"
        );
    }

    for base_url in [
        "https://api.z.ai/api/coding/paas/v4",
        "https://open.bigmodel.cn/api/paas/v4/preview",
        "https://compatible.example.test/v1",
    ] {
        let adjacent = resolver
            .resolve(&RouteRequest {
                explicit_provider: Some(ProviderKind::Zai),
                model_selector: Some(LogicalModelRef::from("GLM-5.3")),
                saved_provider_model: None,
                base_url_override: Some(base_url.to_string()),
                limit_overrides: Vec::new(),
                wire_override: None,
                responses_profile_override: None,
            })
            .expect("adjacent route resolves");
        assert_eq!(
            adjacent.capabilities().server_side_web_search,
            CapabilityState::Unknown,
            "{base_url} must remain fail-closed"
        );
    }
}

#[test]
fn qwen_native_search_is_exact_to_token_plan_responses_routes() {
    use crate::route::CapabilityState;

    let resolver = RouteResolver::new();
    let direct = resolver
        .resolve(&req(
            Some(ProviderKind::ModelstudioTokenPlan),
            Some("qwen3.8-max"),
        ))
        .expect("Token Plan Qwen route resolves");
    assert_eq!(
        direct.capabilities().server_side_web_search,
        CapabilityState::Supported
    );

    let preview = resolver
        .resolve(&req(
            Some(ProviderKind::ModelstudioTokenPlan),
            Some("qwen3.8-max-preview"),
        ))
        .expect("neighboring preview route resolves");
    assert_eq!(
        preview.capabilities().server_side_web_search,
        CapabilityState::Unknown
    );

    for base_url in [
        "https://coding-intl.dashscope.aliyuncs.com/v1",
        "https://token-plan.ap-southeast-1.maas.aliyuncs.com/apps/anthropic",
        "https://compatible.example.test/v1",
    ] {
        let alternate = resolver
            .resolve(&RouteRequest {
                explicit_provider: Some(ProviderKind::ModelstudioTokenPlan),
                model_selector: Some(LogicalModelRef::from("qwen3.8-max")),
                saved_provider_model: None,
                base_url_override: Some(base_url.to_string()),
                limit_overrides: Vec::new(),
                wire_override: None,
                responses_profile_override: None,
            })
            .expect("alternate product route resolves");
        assert_eq!(
            alternate.capabilities().server_side_web_search,
            CapabilityState::Unknown,
            "{base_url} must not inherit Token Plan Responses search"
        );
    }
}

#[test]
fn moonshot_native_search_requires_exact_product_model_pair() {
    use crate::route::CapabilityState;

    let resolver = RouteResolver::new();
    for (model, base_url) in [
        ("kimi-k3", "https://api.moonshot.ai/v1"),
        ("kimi-k3", "https://api.moonshot.cn/v1"),
        ("kimi-k2.6", "https://api.moonshot.ai/v1"),
        ("k3", "https://api.kimi.com/coding/v1"),
        ("kimi-for-coding", "https://api.kimi.com/coding/v1"),
    ] {
        let direct = resolver
            .resolve(&RouteRequest {
                explicit_provider: Some(ProviderKind::Moonshot),
                model_selector: Some(LogicalModelRef::from(model)),
                saved_provider_model: None,
                base_url_override: Some(base_url.to_string()),
                limit_overrides: Vec::new(),
                wire_override: None,
                responses_profile_override: None,
            })
            .expect("documented Moonshot/Kimi route resolves");
        assert_eq!(
            direct.capabilities().server_side_web_search,
            CapabilityState::Supported,
            "{base_url}/{model} should expose native search"
        );
    }

    for (model, base_url) in [
        ("kimi-k3", "https://api.kimi.com/coding/v2"),
        ("kimi-k2.6", "https://api.kimi.com/coding/v1/preview"),
        ("k3", "https://api.moonshot.ai/v1"),
        ("kimi-k2.7-code", "https://api.moonshot.ai/v1"),
    ] {
        let adjacent = resolver
            .resolve(&RouteRequest {
                explicit_provider: Some(ProviderKind::Moonshot),
                model_selector: Some(LogicalModelRef::from(model)),
                saved_provider_model: None,
                base_url_override: Some(base_url.to_string()),
                limit_overrides: Vec::new(),
                wire_override: None,
                responses_profile_override: None,
            })
            .expect("adjacent Moonshot/Kimi route resolves");
        assert_eq!(
            adjacent.capabilities().server_side_web_search,
            CapabilityState::Unknown,
            "{base_url}/{model} must remain fail-closed"
        );
    }
}

#[test]
fn priced_offering_yields_token_pricing_sku() {
    use super::candidate::PricingSku;

    let r = priced_deepseek_resolver();
    let out = r
        .resolve(&req(Some(ProviderKind::Deepseek), Some("deepseek-v4-pro")))
        .expect("priced DeepSeek route should resolve");

    match out.pricing() {
        Some(PricingSku::Token {
            input_per_mtok,
            output_per_mtok,
        }) => {
            assert_eq!(*input_per_mtok, Some(0.28));
            assert_eq!(*output_per_mtok, Some(0.42));
        }
        other => panic!("expected Some(Token), got {other:?}"),
    }
}

#[test]
fn custom_endpoint_does_not_inherit_first_party_pricing() {
    use super::candidate::PricingSku;

    let out = priced_deepseek_resolver()
        .resolve(&RouteRequest {
            explicit_provider: Some(ProviderKind::Deepseek),
            model_selector: Some(LogicalModelRef::from("deepseek-v4-pro")),
            saved_provider_model: None,
            base_url_override: Some("https://deepseek-proxy.example.test/v1".to_string()),
            limit_overrides: Vec::new(),
            wire_override: None,
            responses_profile_override: None,
        })
        .expect("same-name custom proxy route resolves");

    assert!(
        matches!(out.pricing(), Some(PricingSku::UnknownOrStale)),
        "a custom proxy cannot inherit first-party pricing by reusing the model id: {:?}",
        out.pricing()
    );
}

#[test]
fn exact_endpoint_catalog_may_carry_its_own_pricing_on_a_custom_base_url() {
    use super::candidate::PricingSku;

    let request = RouteRequest {
        explicit_provider: Some(ProviderKind::Deepseek),
        model_selector: Some(LogicalModelRef::from("deepseek-v4-pro")),
        saved_provider_model: None,
        base_url_override: Some("https://authenticated-catalog.example.test/v1".to_string()),
        limit_overrides: Vec::new(),
        wire_override: None,
        responses_profile_override: None,
    };
    let out = priced_deepseek_resolver()
        .resolve_with_endpoint_catalog_authority(&request)
        .expect("exact endpoint-owned catalog route resolves");

    match out.pricing() {
        Some(PricingSku::Token {
            input_per_mtok,
            output_per_mtok,
        }) => {
            assert_eq!(*input_per_mtok, Some(0.28));
            assert_eq!(*output_per_mtok, Some(0.42));
        }
        other => panic!("expected exact endpoint pricing, got {other:?}"),
    }
}

#[test]
fn unpriced_offering_stays_unknown() {
    use super::candidate::PricingSku;

    // The bundled seam (`RouteResolver::new`) carries no sourced cost, so a
    // matched offering must surface honest UnknownOrStale, never a fabricated
    // zero price (#2608 / #3085 honesty rule).
    let r = RouteResolver::new();
    let out = r
        .resolve(&req(Some(ProviderKind::Deepseek), Some("deepseek-v4-pro")))
        .expect("bundled DeepSeek route should resolve");
    assert!(
        matches!(out.pricing(), Some(PricingSku::UnknownOrStale)),
        "bundled offering carries no price → UnknownOrStale, got {:?}",
        out.pricing()
    );

    // A pass-through route with no matched offering is likewise unknown.
    let passthrough = r
        .resolve(&req(Some(ProviderKind::Ollama), Some("my-local:7b")))
        .expect("local passthrough should resolve");
    assert!(matches!(
        passthrough.pricing(),
        Some(PricingSku::UnknownOrStale)
    ));
}

// ---------------------------------------------------------------------------
// #1519: advisory insecure-http warning, loopback-exempt.
// ---------------------------------------------------------------------------

/// Build a request with an explicit base-URL override.
fn req_with_base(provider: ProviderKind, model: &str, base_url: &str) -> RouteRequest {
    RouteRequest {
        explicit_provider: Some(provider),
        model_selector: Some(LogicalModelRef::from(model)),
        saved_provider_model: None,
        base_url_override: Some(base_url.to_string()),
        limit_overrides: Vec::new(),
        wire_override: None,
        responses_profile_override: None,
    }
}

#[test]
fn http_custom_endpoint_emits_insecure_warning() {
    let r = RouteResolver::new();
    let out = r
        .resolve(&req_with_base(
            ProviderKind::Openai,
            "gpt-whatever",
            "http://example.com/v1",
        ))
        .expect("custom http endpoint should still resolve");

    // Advisory only: the route stays usable.
    assert!(
        out.validation().ok,
        "insecure http is advisory, not a hard fail"
    );
    assert!(
        out.validation()
            .messages
            .iter()
            .any(|m| m.contains("insecure http")),
        "expected an insecure-http advisory, got {:?}",
        out.validation().messages
    );
}

#[test]
fn loopback_http_endpoint_does_not_warn() {
    let r = RouteResolver::new();
    // localhost, 127.0.0.1, and ::1 are all loopback and must stay clean.
    for base in [
        "http://localhost:11434/v1",
        "http://127.0.0.1:8000/v1",
        "http://[::1]:8080/v1",
    ] {
        let out = r
            .resolve(&req_with_base(ProviderKind::Ollama, "my-local:7b", base))
            .unwrap_or_else(|e| panic!("loopback route {base} should resolve: {e}"));
        assert!(out.validation().ok);
        assert!(
            out.validation().messages.is_empty(),
            "loopback {base} must not warn, got {:?}",
            out.validation().messages
        );
    }
}

#[test]
fn https_endpoint_has_no_warning() {
    let r = RouteResolver::new();
    let out = r
        .resolve(&req_with_base(
            ProviderKind::Openai,
            "gpt-whatever",
            "https://example.com/v1",
        ))
        .expect("https endpoint should resolve");
    assert!(out.validation().ok);
    assert!(
        out.validation().messages.is_empty(),
        "https must not warn, got {:?}",
        out.validation().messages
    );
}
