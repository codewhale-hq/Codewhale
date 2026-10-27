//! Behavior tests for the Models.dev-backed catalog cache (#3385).
//!
//! Fixtures use synthetic ids for anti-hardcoding guards, plus the GLM-5.2 and
//! hosted-DeepSeek rows the issue explicitly asks to exercise. No full hosted
//! provider model list is copied here.

use super::*;

/// Zhipu canonical + Zhipu/Z.AI provider offerings, and a hosted DeepSeek row
/// served by an aggregator under a prefixed wire id with an explicit canonical
/// `base_model` join.
const FIXTURE: &str = r#"{
  "models": {
    "zhipuai/glm-5.2": {
      "id": "zhipuai/glm-5.2",
      "family": "glm",
      "reasoning": true,
      "modalities": { "input": ["text"], "output": ["text"] },
      "limit": { "context": 1000000, "output": 131072 }
    }
  },
  "providers": {
    "zhipuai": {
      "id": "zhipuai",
      "models": {
        "glm-5.2": {
          "id": "glm-5.2",
          "family": "glm",
          "default": true,
          "attachment": false,
          "reasoning": true,
          "reasoning_options": [{ "type": "effort", "values": ["high", "max"] }],
          "tool_call": true,
          "structured_output": true,
          "modalities": { "input": ["text"], "output": ["text"] },
          "limit": { "context": 1000000, "output": 131072 },
          "cost": { "input": 1.4, "output": 4.4, "cache_read": 0.26 }
        },
        "glm-voice": {
          "id": "glm-voice",
          "modalities": { "input": ["text"], "output": ["audio"] }
        }
      }
    },
    "together": {
      "id": "together",
      "models": {
        "deepseek-ai/DeepSeek-V4-Pro": {
          "id": "deepseek-ai/DeepSeek-V4-Pro",
          "base_model": "deepseek-v4-pro",
          "family": "deepseek",
          "reasoning": false,
          "modalities": { "input": ["text"], "output": ["text"] },
          "cost": { "input": 0.9, "output": 0.9 }
        }
      }
    }
  }
}"#;

fn fixture() -> ModelsDevCatalog {
    ModelsDevCatalog::parse_json(FIXTURE).expect("fixture parses")
}

fn find<'a>(rows: &'a [CatalogOffering], provider: &str, wire: &str) -> &'a CatalogOffering {
    rows.iter()
        .find(|r| r.provider == provider && r.wire_model_id == wire)
        .unwrap_or_else(|| panic!("offering {provider}/{wire} not found"))
}

#[test]
fn hydrates_models_dev_offerings_preserving_offering_facts() {
    let rows = bundled_offerings_from_models_dev(&fixture());

    // glm-voice (audio output) is excluded; two chat offerings remain.
    assert_eq!(rows.len(), 2, "audio-only rows are not chat offerings");

    let glm = find(&rows, "zhipuai", "glm-5.2");
    assert!(glm.default_for_provider);
    assert_eq!(glm.family.as_deref(), Some("glm"));
    assert_eq!(glm.reasoning, Some(true));
    assert_eq!(glm.attachment, Some(false));
    assert_eq!(glm.tool_call, Some(true));
    assert_eq!(glm.structured_output, Some(true));
    // Provider-scoped reasoning options are preserved, not collapsed.
    assert_eq!(glm.reasoning_options.len(), 1);
    assert_eq!(glm.limit.as_ref().and_then(|l| l.context), Some(1_000_000));
    assert_eq!(glm.cost.as_ref().and_then(|c| c.cache_read), Some(0.26));
    // Provider row carried no base_model link → no inferred canonical model.
    assert_eq!(glm.canonical_model, None);
    assert_eq!(glm.source, CatalogSource::Bundled);
}

#[test]
fn hosted_offering_keeps_prefixed_wire_id_and_explicit_canonical_join() {
    let rows = bundled_offerings_from_models_dev(&fixture());
    let hosted = find(&rows, "together", "deepseek-ai/DeepSeek-V4-Pro");

    // The prefixed wire id is preserved verbatim under the serving provider.
    assert_eq!(hosted.wire_model_id, "deepseek-ai/DeepSeek-V4-Pro");
    assert_eq!(hosted.provider, "together");
    // Canonical link comes only from the explicit base_model.
    assert_eq!(hosted.canonical_model.as_deref(), Some("deepseek-v4-pro"));
    assert_eq!(hosted.reasoning, Some(false));
}

#[test]
fn model_only_rows_use_namespaced_keys_and_preserve_unpriced_facts() {
    let catalog = ModelsDevCatalog::parse_json(
        r#"{
          "models": {
            "moonshotai/synthetic/chat": {
              "id": "",
              "family": "synthetic",
              "attachment": true,
              "reasoning": true,
              "tool_call": true,
              "structured_output": false,
              "limit": { "context": 123456, "output": 7890 },
              "modalities": { "input": ["text", "image"], "output": ["text"] }
            },
            "new-vendor/solo": {},
            "xiaomi/synthetic-chat": {},
            "bare-model": {},
            "/missing-provider": {},
            "missing-model/": {},
            "new-vendor/voice": {
              "modalities": { "input": ["text"], "output": ["audio"] }
            }
          }
        }"#,
    )
    .expect("fixture parses");

    for live in [false, true] {
        let (rows, provider, source) = if live {
            (
                live_offerings_from_models_dev(&catalog, 1_700),
                "moonshot",
                CatalogSource::ModelsDevLive { fetched_at: 1_700 },
            )
        } else {
            (
                bundled_offerings_from_models_dev(&catalog),
                "moonshot",
                CatalogSource::Bundled,
            )
        };
        assert_eq!(
            rows.len(),
            3,
            "unqualified and audio-only rows stay excluded"
        );
        let row = find(&rows, provider, "synthetic/chat");
        assert_eq!(
            row.canonical_model.as_deref(),
            Some("moonshotai/synthetic/chat")
        );
        assert_eq!(row.endpoint_key, "chat");
        assert_eq!(row.family.as_deref(), Some("synthetic"));
        assert_eq!(
            row.limit.as_ref().and_then(|limit| limit.context),
            Some(123456)
        );
        assert_eq!(
            row.limit.as_ref().and_then(|limit| limit.output),
            Some(7890)
        );
        assert_eq!(row.attachment, Some(true));
        assert_eq!(row.reasoning, Some(true));
        assert_eq!(row.tool_call, Some(true));
        assert_eq!(row.structured_output, Some(false));
        assert_eq!(row.modalities.as_ref().unwrap().input, ["text", "image"]);
        assert_eq!(row.source, source);
        assert!(rows.iter().all(|row| {
            row.cost.is_none()
                // A MiMo row's price is withheld by a Codewhale correction.
                && (row.cost_source.is_none()
                    || matches!(row.cost_source, Some(CatalogSource::CodewhaleBundled { .. })))
                && !row.default_for_provider
                && row.reasoning_options.is_empty()
        }));
        // Vendor namespaces normalize offline too (#6396 slice B).
        find(&rows, "xiaomi-mimo", "synthetic-chat");
        find(&rows, "new-vendor", "solo");
        assert!(crate::ProviderKind::parse("new-vendor").is_none());
    }
}

#[test]
fn model_only_rows_yield_to_provider_facts_and_non_chat_exclusions() {
    let catalog = ModelsDevCatalog::parse_json(
        r#"{
          "models": {
            "moonshotai/chat": { "reasoning": true },
            "moonshotai/voice": {},
            "moonshot/duplicate": {},
            "moonshotai/duplicate": {}
          },
          "providers": {
            "moonshot": {
              "id": "moonshot",
              "models": {
                "chat": {
                  "id": "chat",
                  "base_model": "explicit/chat",
                  "reasoning": false,
                  "reasoning_options": [{ "type": "effort", "values": ["high"] }],
                  "default": true,
                  "cost": { "input": 2.0 }
                },
                "voice": {
                  "id": "voice",
                  "modalities": { "input": ["text"], "output": ["audio"] }
                }
              }
            }
          }
        }"#,
    )
    .expect("fixture parses");
    let rows = live_offerings_from_models_dev(&catalog, 1_700);
    assert_eq!(
        rows.len(),
        2,
        "aliases deduplicate; provider exclusions win"
    );
    let row = find(&rows, "moonshot", "chat");
    assert_eq!(row.canonical_model.as_deref(), Some("explicit/chat"));
    assert_eq!(row.reasoning, Some(false));
    assert!(row.default_for_provider);
    assert_eq!(row.cost.as_ref().and_then(|cost| cost.input), Some(2.0));
    assert_eq!(row.reasoning_options.len(), 1);
    find(&rows, "moonshot", "duplicate");
}

#[test]
fn provider_map_keys_preserve_precedence_when_model_ids_are_missing() {
    let catalog = ModelsDevCatalog::parse_json(
        r#"{
          "models": {
            "moonshotai/chat": { "reasoning": true },
            "moonshotai/voice": {},
            "moonshotai/explicit": { "reasoning": true }
          },
          "providers": {
            "moonshotai": {
              "models": {
                " chat ": { "id": " ", "reasoning": false },
                "voice": { "modalities": { "output": ["audio"] } },
                "different-map-key": { "id": " explicit ", "reasoning": false },
                " ": {}
              }
            }
          }
        }"#,
    )
    .expect("fixture parses");

    for (rows, provider) in [
        (live_offerings_from_models_dev(&catalog, 1_700), "moonshot"),
        (bundled_offerings_from_models_dev(&catalog), "moonshotai"),
    ] {
        assert_eq!(rows.len(), 2, "provider identities and exclusions win");
        for wire_model_id in ["chat", "explicit"] {
            let row = find(&rows, provider, wire_model_id);
            assert_eq!(row.reasoning, Some(false));
            assert_eq!(row.canonical_model, None);
        }
    }
}

#[test]
fn to_offering_projects_routing_identity_and_limits() {
    let rows = bundled_offerings_from_models_dev(&fixture());
    let glm = find(&rows, "zhipuai", "glm-5.2").to_offering();

    assert_eq!(glm.provider.as_str(), "zhipuai");
    assert_eq!(glm.wire_model_id.as_str(), "glm-5.2");
    assert_eq!(glm.canonical_model, None);
    assert_eq!(glm.endpoint_key, "chat");
    assert_eq!(glm.limits.context_tokens, Some(1_000_000));
    assert_eq!(glm.limits.output_tokens, Some(131_072));
    assert_eq!(
        glm.capabilities.attachments,
        crate::route::CapabilityState::Unsupported
    );
    assert_eq!(
        glm.capabilities.reasoning,
        crate::route::CapabilityState::Supported
    );
    assert_eq!(
        glm.capabilities.native_tool_calls,
        crate::route::CapabilityState::Supported
    );
    assert_eq!(
        glm.capabilities.structured_output,
        crate::route::CapabilityState::Supported
    );
    assert_eq!(
        glm.capabilities.streaming,
        crate::route::CapabilityState::Unknown
    );
}

#[test]
fn compiler_merges_layers_with_override_precedence() {
    // Bundled default for synthetic provider "acme".
    let bundled = vec![CatalogOffering {
        provider: "acme".into(),
        wire_model_id: "synth-chat-1".into(),
        endpoint_key: "chat".into(),
        default_for_provider: true,
        family: Some("synth".into()),
        source: CatalogSource::Bundled,
        ..Default::default()
    }];
    // Live refresh adds a new row AND restates the bundled one with a cost.
    let live = vec![
        CatalogOffering {
            provider: "acme".into(),
            wire_model_id: "synth-chat-1".into(),
            endpoint_key: "chat".into(),
            cost: Some(ModelsDevCost {
                input: Some(2.0),
                ..Default::default()
            }),
            source: CatalogSource::Live {
                base_url_fingerprint: "fp".into(),
                fetched_at: 100,
            },
            ..Default::default()
        },
        CatalogOffering {
            provider: "acme".into(),
            wire_model_id: "synth-chat-2".into(),
            endpoint_key: "chat".into(),
            source: CatalogSource::Live {
                base_url_fingerprint: "fp".into(),
                fetched_at: 100,
            },
            ..Default::default()
        },
    ];
    // User override pins a custom canonical model on synth-chat-1.
    let overrides = vec![CatalogOffering {
        provider: "acme".into(),
        wire_model_id: "synth-chat-1".into(),
        canonical_model: Some("acme-canonical".into()),
        endpoint_key: "chat".into(),
        source: CatalogSource::UserOverride,
        ..Default::default()
    }];

    let snapshot = CatalogCompiler::new()
        .with_bundled(bundled)
        .with_live(live)
        .with_overrides(overrides)
        .compile();

    // Two distinct (provider, wire) identities survive de-duplication.
    assert_eq!(snapshot.offerings.len(), 2);

    let one = find(&snapshot.offerings, "acme", "synth-chat-1");
    // Highest-precedence layer (override) wins the identity collision.
    assert_eq!(one.source, CatalogSource::UserOverride);
    assert_eq!(one.canonical_model.as_deref(), Some("acme-canonical"));

    let two = find(&snapshot.offerings, "acme", "synth-chat-2");
    assert!(matches!(two.source, CatalogSource::Live { .. }));
}

#[test]
fn compiler_layer_order_and_policy_deny_never_overridden() {
    let row = |source: CatalogSource, model: &str, family: &str| CatalogOffering {
        provider: "zai-coding-cn".into(),
        wire_model_id: model.into(),
        endpoint_key: "chat".into(),
        family: Some(family.into()),
        source,
        ..Default::default()
    };
    let policy = crate::route::CatalogPolicy {
        rules: vec![crate::route::PolicyRule {
            effect: crate::route::PolicyEffect::Deny,
            action: crate::route::PolicyAction::ModelUse,
            resource: "*-cn/*".to_string(),
        }],
    };
    let snapshot = CatalogCompiler::new()
        .with_bundled(vec![row(CatalogSource::Bundled, "glm-5", "bundled")])
        .with_models_dev_live(vec![row(
            CatalogSource::ModelsDevLive { fetched_at: 1 },
            "glm-5",
            "models-dev",
        )])
        .with_provider_live(vec![row(
            CatalogSource::Live {
                base_url_fingerprint: "fp".into(),
                fetched_at: 2,
            },
            "glm-5",
            "provider",
        )])
        .with_config(vec![row(CatalogSource::ConfigOverride, "glm-5", "config")])
        .with_overrides(vec![row(CatalogSource::UserOverride, "glm-5", "user")])
        .with_policy(policy)
        .compile();

    assert!(
        snapshot.offerings.is_empty(),
        "policy DENY after every layer must drop the row; layers cannot override it"
    );

    let allowed = CatalogCompiler::new()
        .with_bundled(vec![row(CatalogSource::Bundled, "glm-5", "bundled")])
        .with_overrides(vec![row(CatalogSource::UserOverride, "glm-5", "user")])
        .compile();
    let kept = find(&allowed.offerings, "zai-coding-cn", "glm-5");
    assert_eq!(kept.source, CatalogSource::UserOverride);
    assert_eq!(kept.family.as_deref(), Some("user"));
}

#[test]
fn cache_scopes_by_provider_and_base_url_fingerprint() {
    let fp_a = base_url_fingerprint("https://api.example.com/v1");
    let fp_b = base_url_fingerprint("https://other.example.com/v1");
    assert_ne!(fp_a, fp_b, "different hosts must not share a fingerprint");

    let mut cache = ProviderCatalogCache::new();
    let row = |id: &str| CatalogOffering {
        provider: "acme".into(),
        wire_model_id: id.into(),
        endpoint_key: "chat".into(),
        ..Default::default()
    };

    // Same provider, two different base URLs.
    cache.record_success(
        ProviderCatalogDelta {
            provider: "acme".into(),
            base_url_fingerprint: fp_a.clone(),
            fetched_at: 1_000,
            offerings: vec![row("from-a")],
        },
        3_600,
    );
    cache.record_success(
        ProviderCatalogDelta {
            provider: "acme".into(),
            base_url_fingerprint: fp_b.clone(),
            fetched_at: 1_000,
            offerings: vec![row("from-b")],
        },
        3_600,
    );
    // Different provider, SAME base URL as fp_a.
    cache.record_success(
        ProviderCatalogDelta {
            provider: "beta".into(),
            base_url_fingerprint: fp_a.clone(),
            fetched_at: 1_000,
            offerings: vec![row("from-beta")],
        },
        3_600,
    );

    let a = cache.fresh_offerings("acme", &fp_a, 1_100);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].wire_model_id, "from-a");
    // Same provider, different base URL must not leak rows across.
    let b = cache.fresh_offerings("acme", &fp_b, 1_100);
    assert_eq!(b[0].wire_model_id, "from-b");
    // Different provider on the same base URL must not share rows either.
    let beta = cache.fresh_offerings("beta", &fp_a, 1_100);
    assert_eq!(beta[0].wire_model_id, "from-beta");
    assert_eq!(cache.entries.len(), 3);
}

#[test]
fn fingerprint_folds_cosmetic_base_url_differences() {
    let canonical = base_url_fingerprint("https://API.Example.com/v1");
    assert_eq!(canonical.len(), 64, "endpoint fingerprints use SHA-256");
    assert_eq!(
        canonical,
        base_url_fingerprint("https://api.example.com/v1/"),
        "trailing slash + host case must not change the cache scope"
    );
    assert_eq!(
        canonical,
        base_url_fingerprint("  https://api.example.com:443/v1  "),
        "default https port + surrounding whitespace must fold away"
    );
    // Path case is significant (providers can be case-sensitive on the path).
    assert_ne!(
        canonical,
        base_url_fingerprint("https://api.example.com/V1")
    );

    // Port stripping is scheme-aware: :80 is http's default (folds away), but
    // :443 on http is a non-default port and must stay distinct from bare http.
    assert_eq!(
        base_url_fingerprint("http://h.example.com:80/v1"),
        base_url_fingerprint("http://h.example.com/v1"),
        "http default port :80 must fold away"
    );
    assert_ne!(
        base_url_fingerprint("http://h.example.com:443/v1"),
        base_url_fingerprint("http://h.example.com/v1"),
        ":443 is not http's default port and must not fold"
    );
}

#[test]
fn fingerprint_never_hashes_secret_bearing_url_text() {
    let expected = base_url_fingerprint("https://api.example.com/v1");
    for url in [
        "https://user:secret@api.example.com/v1",
        "https://api.example.com/v1?api_key=secret",
        "https://api.example.com/v1#secret",
    ] {
        assert_eq!(base_url_fingerprint(url), expected, "{url}");
    }
}

#[test]
fn fingerprint_strips_userinfo_from_a_scheme_less_base_url() {
    // A base_url typed without a scheme took the fall-through branch, which
    // only split off `?`/`#` — so `user:pass@host` went into SHA-256 verbatim,
    // against the documented "userinfo never enters the digest function".
    let expected = base_url_fingerprint("api.example.com/v1");
    for url in [
        "user:secret@api.example.com/v1",
        "user:other-secret@api.example.com/v1",
        "token@api.example.com/v1",
    ] {
        assert_eq!(base_url_fingerprint(url), expected, "{url}");
    }
}

#[test]
fn fingerprint_of_an_empty_base_url_is_the_redacted_constant() {
    // The fall-through's `unwrap_or(REDACTED)` never fired — `split` always
    // yields at least one (possibly empty) piece — so an empty base URL
    // fingerprinted the empty string instead of the redacted sentinel.
    let redacted = base_url_fingerprint("ftp://api.example.com");
    for url in ["", "   ", "?api_key=secret"] {
        assert_eq!(base_url_fingerprint(url), redacted, "{url:?}");
    }
    // SHA-256("") is what empty/whitespace hashed to before the sentinel
    // mapping. That digest is a persisted cache/receipt key, so flipping it
    // back would be another undeclared persisted-key change.
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    assert_ne!(redacted, EMPTY_SHA256);
}

#[test]
fn ttl_marks_entries_stale_and_excludes_them_from_fresh() {
    let fp = base_url_fingerprint("https://api.example.com");
    let mut cache = ProviderCatalogCache::new();
    cache.record_success(
        ProviderCatalogDelta {
            provider: "acme".into(),
            base_url_fingerprint: fp.clone(),
            fetched_at: 1_000,
            offerings: vec![CatalogOffering {
                provider: "acme".into(),
                wire_model_id: "synth-chat-1".into(),
                endpoint_key: "chat".into(),
                ..Default::default()
            }],
        },
        100, // ttl
    );

    // Within TTL: fresh.
    assert_eq!(cache.status("acme", &fp, 1_050), CatalogStatus::Fresh);
    assert_eq!(cache.fresh_offerings("acme", &fp, 1_050).len(), 1);

    // Past TTL: stale, and excluded from fresh offerings.
    match cache.status("acme", &fp, 1_200) {
        CatalogStatus::Stale { age_secs } => assert_eq!(age_secs, 200),
        other => panic!("expected stale, got {other:?}"),
    }
    assert!(cache.fresh_offerings("acme", &fp, 1_200).is_empty());
    // But the rows are still present in the cache for explicit fallback display.
    assert_eq!(cache.get("acme", &fp).unwrap().offerings.len(), 1);
}

#[test]
fn ttl_zero_is_always_stale() {
    let fp = base_url_fingerprint("https://api.example.com");
    let mut cache = ProviderCatalogCache::new();
    cache.record_success(
        ProviderCatalogDelta {
            provider: "acme".into(),
            base_url_fingerprint: fp.clone(),
            fetched_at: 1_000,
            offerings: vec![],
        },
        0,
    );
    assert!(cache.get("acme", &fp).unwrap().is_stale(1_000));
}

#[test]
fn unknown_scope_reports_unknown_status() {
    let cache = ProviderCatalogCache::new();
    let fp = base_url_fingerprint("https://api.example.com");
    assert_eq!(cache.status("acme", &fp, 1_000), CatalogStatus::Unknown);
    assert!(cache.fresh_offerings("acme", &fp, 1_000).is_empty());
}

#[test]
fn refresh_failure_preserves_prior_rows_and_marks_failed() {
    let fp = base_url_fingerprint("https://api.example.com");
    let mut cache = ProviderCatalogCache::new();
    cache.record_success(
        ProviderCatalogDelta {
            provider: "acme".into(),
            base_url_fingerprint: fp.clone(),
            fetched_at: 1_000,
            offerings: vec![CatalogOffering {
                provider: "acme".into(),
                wire_model_id: "synth-chat-1".into(),
                endpoint_key: "chat".into(),
                ..Default::default()
            }],
        },
        3_600,
    );

    for reason in [
        CatalogRefreshError::Unauthorized,
        CatalogRefreshError::Forbidden,
        CatalogRefreshError::NotFound,
        CatalogRefreshError::RateLimited,
        CatalogRefreshError::InvalidResponse,
        CatalogRefreshError::EmptyList,
        CatalogRefreshError::Network,
    ] {
        cache.record_failure("acme", &fp, reason);
        let entry = cache.get("acme", &fp).expect("entry survives failure");
        // Prior successful rows remain available after a failed refresh.
        assert_eq!(entry.offerings.len(), 1, "{reason:?} dropped prior rows");
        assert_eq!(entry.status, CatalogStatus::Failed { reason });
        // fetched_at is NOT bumped by a failure.
        assert_eq!(entry.fetched_at, 1_000);
        // ...but a Failed entry must NOT contribute to fresh offerings even
        // while still within its TTL window (now=1_100, ttl=3_600). The rows
        // are reachable only via get() for explicit fallback display.
        assert!(
            cache.fresh_offerings("acme", &fp, 1_100).is_empty(),
            "{reason:?}: failed entry served fresh offerings within TTL"
        );
        assert!(cache.all_fresh_offerings(1_100).is_empty());
        assert_eq!(
            cache.status("acme", &fp, 1_100),
            CatalogStatus::Failed { reason }
        );
    }
}

#[test]
fn failure_without_prior_creates_observable_empty_entry() {
    let fp = base_url_fingerprint("https://api.example.com");
    let mut cache = ProviderCatalogCache::new();
    cache.record_failure("acme", &fp, CatalogRefreshError::Unauthorized);

    let entry = cache.get("acme", &fp).expect("failure is observable");
    assert!(entry.offerings.is_empty());
    assert_eq!(
        entry.status,
        CatalogStatus::Failed {
            reason: CatalogRefreshError::Unauthorized
        }
    );
}

#[test]
fn record_success_stamps_live_provenance_on_rows() {
    let fp = base_url_fingerprint("https://api.example.com");
    let mut cache = ProviderCatalogCache::new();
    // Row arrives mislabeled as Bundled; ingest must normalize provenance.
    cache.record_success(
        ProviderCatalogDelta {
            provider: "acme".into(),
            base_url_fingerprint: fp.clone(),
            fetched_at: 4_242,
            offerings: vec![CatalogOffering {
                provider: "acme".into(),
                wire_model_id: "synth-chat-1".into(),
                endpoint_key: "chat".into(),
                source: CatalogSource::Bundled,
                ..Default::default()
            }],
        },
        3_600,
    );
    let entry = cache.get("acme", &fp).unwrap();
    assert_eq!(
        entry.offerings[0].source,
        CatalogSource::Live {
            base_url_fingerprint: fp,
            fetched_at: 4_242,
        }
    );
}

#[test]
fn cache_serialization_round_trips_and_contains_no_secrets() {
    let fp = base_url_fingerprint("https://api.example.com/v1");
    let mut cache = ProviderCatalogCache::new();
    cache.record_success(
        ProviderCatalogDelta {
            provider: "zhipuai".into(),
            base_url_fingerprint: fp.clone(),
            fetched_at: 1_700,
            offerings: bundled_offerings_from_models_dev(&fixture()),
        },
        3_600,
    );

    let json = serde_json::to_string_pretty(&cache).expect("cache serializes");
    let round: ProviderCatalogCache = serde_json::from_str(&json).expect("cache round-trips");
    assert_eq!(round, cache);

    // The persisted shape carries model facts but has no field that could hold
    // a credential. Guard against a future field reintroducing one.
    let lower = json.to_lowercase();
    for needle in [
        "api_key",
        "apikey",
        "api-key",
        "authorization",
        "secret",
        "password",
        "bearer",
        "access_token",
    ] {
        assert!(
            !lower.contains(needle),
            "cache JSON unexpectedly contains `{needle}`"
        );
    }
    // Sanity: it did serialize meaningful provider/model facts.
    assert!(json.contains("glm-5.2"));
    assert!(json.contains("base_url_fingerprint"));
}

#[test]
fn all_fresh_offerings_spans_providers_and_skips_stale() {
    let fp = base_url_fingerprint("https://api.example.com");
    let mut cache = ProviderCatalogCache::new();
    cache.record_success(
        ProviderCatalogDelta {
            provider: "acme".into(),
            base_url_fingerprint: fp.clone(),
            fetched_at: 1_000,
            offerings: vec![CatalogOffering {
                provider: "acme".into(),
                wire_model_id: "fresh-row".into(),
                endpoint_key: "chat".into(),
                ..Default::default()
            }],
        },
        3_600,
    );
    cache.record_success(
        ProviderCatalogDelta {
            provider: "beta".into(),
            base_url_fingerprint: fp.clone(),
            fetched_at: 0,
            offerings: vec![CatalogOffering {
                provider: "beta".into(),
                wire_model_id: "stale-row".into(),
                endpoint_key: "chat".into(),
                ..Default::default()
            }],
        },
        10, // tiny ttl → stale at now=1_100
    );

    let fresh = cache.all_fresh_offerings(1_100);
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].wire_model_id, "fresh-row");

    // #4139: pickers still see stale rows; only the fresh helper drops them.
    let visible = cache.all_visible_offerings(1_100);
    assert_eq!(visible.len(), 2);
    assert!(visible.iter().any(|row| row.wire_model_id == "fresh-row"));
    assert!(visible.iter().any(|row| row.wire_model_id == "stale-row"));
}

#[test]
fn snapshot_feeds_route_resolver_offerings() {
    // The compiled snapshot projects into the exact type RouteResolver consumes,
    // proving catalog rows reach routing only through the offering seam.
    let snapshot = CatalogCompiler::new().with_models_dev(&fixture()).compile();
    let offerings = snapshot.to_offerings();

    let glm = offerings
        .iter()
        .find(|o| o.provider.as_str() == "zhipuai" && o.wire_model_id.as_str() == "glm-5.2")
        .expect("GLM offering reaches the route resolver seam");
    assert_eq!(glm.limits.context_tokens, Some(1_000_000));
    assert_eq!(glm.limits.output_tokens, Some(131_072));
    // Audio-only row never becomes a routing offering.
    assert!(
        !offerings
            .iter()
            .any(|o| o.wire_model_id.as_str() == "glm-voice")
    );
}

// ---------------------------------------------------------------------------
// #3385 / #4188: the committed offline/stale bundled Models.dev asset.
// ---------------------------------------------------------------------------

#[test]
fn bundled_asset_parses() {
    // The committed asset must `include_str!`-load and deserialize into the
    // parser's `ModelsDevCatalog` shape. This is the build-time guard that keeps
    // `bundled_models_dev_catalog()` panic-free in shipped builds.
    let catalog = ModelsDevCatalog::parse_json(BUNDLED_MODELS_DEV_JSON)
        .expect("committed bundled asset must be valid Models.dev JSON");
    assert!(
        !catalog.providers.is_empty(),
        "bundled asset must carry provider rows"
    );
    // The helper returns the same parsed catalog.
    assert_eq!(*bundled_models_dev_catalog(), catalog);
}

#[test]
fn bundled_deepseek_flash_routes_support_image_input() {
    // #6421: the official Vision guide documents deepseek-flash; the pricing
    // guide maps both legacy Flash names to it (verified 2026-09-23).
    // https://api-docs.deepseek.com/guides/vision/
    // https://api-docs.deepseek.com/quick_start/pricing/
    let rows = bundled_catalog_offerings();
    for model in [
        "deepseek-flash",
        "deepseek-v4-flash",
        "deepseek-v4-flash-vision-exp",
    ] {
        let row = find(&rows, "deepseek", model);
        assert_eq!(
            crate::models_dev::image_input_support(row.modalities.as_ref()),
            crate::route::CapabilityState::Supported,
            "native DeepSeek route {model} must retain its documented vision capability"
        );
    }
    let text_only = find(&rows, "deepseek", "deepseek-v4-pro");
    assert_eq!(
        crate::models_dev::image_input_support(text_only.modalities.as_ref()),
        crate::route::CapabilityState::Unsupported,
        "a Flash correction must not widen other routes"
    );
}

#[test]
fn bundled_seed_canonical_entries_use_upstream_keys_and_yield_to_provider_rows() {
    // #6396 slice B: canonical entries carry upstream `vendor/model` keys. The
    // xiaomi-mimo provider rows still win offline, and a vendor namespace
    // never leaks out as its own (non-route) provider.
    let catalog = bundled_models_dev_catalog();
    for key in ["xiaomi/mimo-v2.6-pro", "xiaomi/mimo-v2.6-flash"] {
        assert!(catalog.model(key).is_some(), "{key} canonical entry");
    }
    let rows = bundled_catalog_offerings();
    assert!(
        rows.iter().all(|row| row.provider != "xiaomi"),
        "vendor namespace must normalize onto xiaomi-mimo"
    );
    for model in ["mimo-v2.6-pro", "mimo-v2.6-flash"] {
        let row = find(&rows, "xiaomi-mimo", model);
        assert_eq!(
            row.canonical_model.as_deref(),
            Some(format!("xiaomi/{model}").as_str()),
            "{model} keeps the canonical join upstream now states on the provider row"
        );
        // The text-only hold is gone (#6396): upstream lists image input,
        // and a wrong claim costs one rejected, retried request.
        assert_eq!(
            row.to_offering().capabilities.image_input,
            crate::route::CapabilityState::Supported,
            "{model} carries upstream's image input"
        );
    }
}

#[test]
fn bundled_seed_cold_start_joins_namespaced_entries_without_provider_rows() {
    // #6396: with the provider rows gone, the upstream-shaped canonical
    // entries alone still put MiMo 2.6 on the xiaomi-mimo route offline.
    let mut catalog = bundled_models_dev_catalog().clone();
    let mimo = catalog
        .providers
        .get_mut("xiaomi-mimo")
        .expect("xiaomi-mimo provider");
    mimo.models.remove("mimo-v2.6-pro");
    mimo.models.remove("mimo-v2.6-flash");
    let rows = bundled_offerings_from_models_dev(&catalog);
    for model in ["mimo-v2.6-pro", "mimo-v2.6-flash"] {
        let row = find(&rows, "xiaomi-mimo", model);
        let canonical = format!("xiaomi/{model}");
        assert_eq!(row.canonical_model.as_deref(), Some(canonical.as_str()));
        assert_eq!(row.source, CatalogSource::Bundled);
        assert_eq!(row.reasoning, Some(true));
        assert_eq!(row.tool_call, Some(true));
        assert!(row.cost.is_none(), "MiMo stays unpriced offline");
        assert!(!row.default_for_provider);
    }
}

/// The bundled Claude row as it read before the seed was generated: text-only.
fn stale_text_only_seed_row() -> CatalogOffering {
    let mut row = find(&bundled_catalog_offerings(), "anthropic", "claude-opus-5").clone();
    row.modalities = Some(ModelsDevModalities {
        input: vec!["text".into()],
        output: vec!["text".into()],
    });
    row
}

#[test]
fn bundled_text_only_rows_leave_image_input_unknown_not_unsupported() {
    // #6396: the hand-kept seed listed every Anthropic row as `input: [text]`
    // while the provider accepts images, and a cold start stripped them. The
    // seed is generated now, but it still lags providers, so simulate a stale
    // text-only row: it must not strip the user's images.
    let seed = &stale_text_only_seed_row();
    assert_eq!(seed.source, CatalogSource::Bundled);
    assert_eq!(
        crate::models_dev::image_input_support(seed.modalities.as_ref()),
        crate::route::CapabilityState::Unsupported,
        "fixture premise: the seed row is text-only"
    );
    assert_eq!(
        seed.to_offering().capabilities.image_input,
        crate::route::CapabilityState::Unknown
    );

    // The same statement from a live catalog is a real refusal.
    let live = CatalogOffering {
        source: CatalogSource::ModelsDevLive { fetched_at: 1 },
        ..seed.clone()
    };
    assert_eq!(
        live.to_offering().capabilities.image_input,
        crate::route::CapabilityState::Unsupported
    );

    // A layer that re-sources the row without restating modalities keeps the
    // seed's low trust for them.
    let re_sourced = CatalogOffering {
        source: CatalogSource::Live {
            base_url_fingerprint: "fp".into(),
            fetched_at: 1,
        },
        modalities_source: Some(CatalogSource::Bundled),
        ..seed.clone()
    };
    assert_eq!(
        re_sourced.to_offering().capabilities.image_input,
        crate::route::CapabilityState::Unknown
    );
}

#[test]
fn offline_resolver_keeps_images_for_a_text_only_seed_row() {
    let route = crate::route::RouteResolver::new()
        .resolve(&crate::route::RouteRequest {
            explicit_provider: Some(crate::ProviderKind::Anthropic),
            model_selector: Some(crate::route::LogicalModelRef::from("claude-opus-5")),
            saved_provider_model: None,
            base_url_override: None,
            limit_overrides: Vec::new(),
            wire_override: None,
        })
        .expect("bundled Anthropic route resolves offline");
    assert_ne!(
        route.capabilities().image_input,
        crate::route::CapabilityState::Unsupported
    );
}

#[test]
fn signed_patch_keeps_the_seed_as_the_modality_authority() {
    let seed = stale_text_only_seed_row();
    let key = seed.merge_key();
    let mut rows = BTreeMap::from([(key.clone(), seed)]);
    let facts = crate::cloud_facts::ScopedFacts {
        facts_version: 1,
        key_id: "cwf-test-only".into(),
        models: vec![crate::cloud_facts::ModelFact {
            provider: key.0.clone(),
            id: key.1.clone(),
            context_window: Some(9000),
            ..Default::default()
        }],
        ..Default::default()
    };
    crate::cloud_facts::catalog_patch::apply_model_patches(&mut rows, &facts, 1);
    let row = &rows[&key];
    assert!(matches!(row.source, CatalogSource::CloudFacts { .. }));
    assert_eq!(row.modalities_source(), &CatalogSource::Bundled);
    assert_eq!(
        row.to_offering().capabilities.image_input,
        crate::route::CapabilityState::Unknown
    );
}

#[test]
fn bundled_asset_meta_describes_offline_fallback_not_competing_truth() {
    // #4188: the asset must document itself as offline/stale fallback, not a
    // competing curated source of truth alongside live Models.dev.
    let raw: serde_json::Value =
        serde_json::from_str(BUNDLED_MODELS_DEV_JSON).expect("bundled JSON");
    let meta = raw
        .get("_meta")
        .and_then(|m| m.as_object())
        .expect("_meta object");
    let role = meta
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        role.to_ascii_lowercase().contains("not a competing"),
        "_meta.role must demote the bundled asset: {role}"
    );
    assert!(
        role.to_ascii_lowercase().contains("live"),
        "_meta.role must point at live Models.dev preference: {role}"
    );
}

#[test]
fn bundled_asset_yields_real_chat_offerings_for_key_models() {
    let rows = bundled_catalog_offerings();
    assert!(
        rows.len() >= 20,
        "expected dozens of bundled chat offerings, got {}",
        rows.len()
    );

    // A GLM and a Kimi row carry their real (non-default) context windows,
    // proving real facts flow rather than `RouteLimits::default()` (unknown).
    let glm = find(&rows, "zai", "GLM-5.2");
    assert_eq!(glm.limit.as_ref().and_then(|l| l.context), Some(1_000_000));
    assert!(
        !glm.default_for_provider,
        "GLM-5.2 is no longer the Z.ai default"
    );

    // GLM-5.3 is the Z.ai default (matching DEFAULT_ZAI_MODEL); its limits
    // still inherit from glm-5.2 until Z.ai publishes distinct 5.3 numbers.
    let glm53 = find(&rows, "zai", "GLM-5.3");
    assert_eq!(
        glm53.limit.as_ref().and_then(|l| l.context),
        glm.limit.as_ref().and_then(|l| l.context)
    );
    assert_eq!(
        glm53.limit.as_ref().and_then(|l| l.output),
        glm.limit.as_ref().and_then(|l| l.output)
    );
    assert!(
        glm53.default_for_provider,
        "GLM-5.3 must be the Z.ai default"
    );

    let kimi_k27 = find(&rows, "moonshot", "kimi-k2.7-code");
    assert_eq!(
        kimi_k27.limit.as_ref().and_then(|l| l.context),
        Some(262_144)
    );

    let kimi_k3 = find(&rows, "moonshot", "kimi-k3");
    assert_eq!(
        kimi_k3.limit.as_ref().and_then(|l| l.context),
        Some(1_048_576)
    );
    // Upstream's output figure; requests stay capped at 131,072 by the
    // compatibility limit table in crates/models (intersected at dispatch).
    assert_eq!(
        kimi_k3.limit.as_ref().and_then(|l| l.output),
        Some(1_048_576)
    );
    let kimi_k3_input_modalities = kimi_k3
        .modalities
        .as_ref()
        .expect("K3 modalities")
        .input
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(kimi_k3_input_modalities, ["text", "image", "video"]);

    let minimax_m3 = find(&rows, "minimax-anthropic", "MiniMax-M3");
    assert_eq!(
        minimax_m3.limit.as_ref().and_then(|limit| limit.context),
        Some(1_000_000)
    );
    let input_modalities = minimax_m3
        .modalities
        .as_ref()
        .expect("M3 modalities")
        .input
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(input_modalities, ["text", "image", "video"]);
    // Codewhale's recorded controls, applied by a correction (#6396).
    assert_eq!(
        minimax_m3.reasoning_options[0]
            .get("default")
            .and_then(serde_json::Value::as_str),
        Some("disabled")
    );

    let grok_46 = find(&rows, "xai", "grok-4.6");
    assert!(grok_46.default_for_provider);
    assert_eq!(
        grok_46.limit.as_ref().and_then(|limit| limit.context),
        Some(500_000)
    );
    assert_eq!(grok_46.attachment, Some(true));
    assert_eq!(grok_46.structured_output, Some(true));
    let grok_input_modalities = grok_46
        .modalities
        .as_ref()
        .expect("Grok 4.6 modalities")
        .input
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(grok_input_modalities, ["text", "image", "pdf"]);
    assert_eq!(
        grok_46.reasoning_options[0]
            .get("default")
            .and_then(serde_json::Value::as_str),
        Some("high")
    );
    let grok_45 = find(&rows, "xai", "grok-4.5");
    assert_eq!(
        grok_45.reasoning_options[0]
            .get("default")
            .and_then(serde_json::Value::as_str),
        Some("high")
    );
    let grok_45_values = grok_45.reasoning_options[0]
        .get("values")
        .and_then(serde_json::Value::as_array)
        .expect("Grok 4.5 effort values");
    assert_eq!(
        grok_45_values
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>(),
        ["low", "medium", "high"]
    );

    let minimax_m2_7 = find(&rows, "minimax-anthropic", "MiniMax-M2.7");
    assert_eq!(
        minimax_m2_7.limit.as_ref().and_then(|limit| limit.context),
        Some(204_800)
    );
    assert_eq!(
        minimax_m2_7.reasoning_options[0]
            .get("default")
            .and_then(serde_json::Value::as_str),
        Some("always_on")
    );

    // Audio/TTS rows are absent (the asset only ships chat models, but assert
    // the filter contract anyway).
    assert!(
        rows.iter().all(|r| !r.wire_model_id.contains("tts")),
        "no TTS rows should reach the offering layer"
    );
}

#[test]
fn bundled_asset_pricing_is_honest() {
    let rows = bundled_catalog_offerings();

    // DeepSeek-native rows are intentionally unpriced here (priced via the
    // time-aware DeepSeek table elsewhere); pricing them would also break the
    // route layer's `unpriced_offering_stays_unknown` invariant.
    let deepseek = find(&rows, "deepseek", "deepseek-v4-pro");
    assert!(
        deepseek.cost.is_none(),
        "DeepSeek-native rows must stay unpriced in the bundled asset"
    );

    // Any row that *does* carry a cost must expose a usable input/output rate
    // (the honesty rule: no cache-only / empty cost objects that would render as
    // a rate-less Token at the route layer).
    for row in &rows {
        if let Some(cost) = row.cost.as_ref() {
            assert!(
                cost.input.is_some() || cost.output.is_some(),
                "{}/{}: priced row must have an input or output rate",
                row.provider,
                row.wire_model_id
            );
        }
    }

    // A sampled priced row matches the in-repo USD table (crates/tui pricing):
    // GLM-5.1 at the 2026-07-09 Z.ai published rates.
    let glm51 = find(&rows, "zai", "glm-5.1");
    let cost = glm51.cost.as_ref().expect("glm-5.1 is priced");
    assert_eq!(cost.input, Some(1.40));
    assert_eq!(cost.output, Some(4.40));
    assert_eq!(cost.cache_read, Some(0.26));

    // The default Z.ai route is the GLM Coding Plan, which bills credit
    // multipliers, so a Codewhale correction keeps GLM-5.3 unpriced there even
    // though Models.dev lists a USD rate (#6396).
    let glm53 = find(&rows, "zai", "GLM-5.3");
    assert!(glm53.cost.is_none());

    let glm53_flash = find(&rows, "zai", "GLM-5.3-Flash");
    let cost = glm53_flash
        .cost
        .as_ref()
        .expect("GLM-5.3-Flash must ship priced at durable list rates");
    assert_eq!(cost.input, Some(0.15));
    assert_eq!(cost.output, Some(0.50));
    assert_eq!(cost.cache_read, Some(0.03));
    assert_eq!(
        glm53_flash.limit.as_ref().and_then(|l| l.context),
        Some(1_000_000)
    );
    assert!(
        !glm53_flash.default_for_provider,
        "GLM-5.3-Flash is a picker row, not the Z.ai default"
    );

    // OpenRouter qwen3.8-flash lists durable (non-promo) rates on models.dev
    // as of 2026-08-26. Unlike GLM-5.3-Flash's explicit 50% promo, this row
    // must ship priced. It is not a family default.
    let qwen38_flash = find(&rows, "openrouter", "qwen/qwen3.8-flash");
    assert!(
        !qwen38_flash.default_for_provider,
        "qwen3.8-flash is a suffix variant and must not be the OpenRouter default"
    );
    let cost = qwen38_flash
        .cost
        .as_ref()
        .expect("qwen/qwen3.8-flash must ship priced (durable list rates, no promo)");
    // The rates are upstream's, pinned by the seed lock; a re-lock that moves
    // them shows up in `seed lock`'s review report, not here.
    assert!(cost.input.is_some_and(|rate| rate > 0.0));
    assert!(cost.output.is_some_and(|rate| rate > 0.0));
    assert_eq!(
        qwen38_flash.limit.as_ref().and_then(|l| l.context),
        Some(1_000_000)
    );
    assert_eq!(
        qwen38_flash.limit.as_ref().and_then(|l| l.output),
        Some(131_072)
    );

    // M3 publishes a context tier at 512K tokens. The row ships priced only
    // because the tier travels with it; dropping the tier would underbill.
    let minimax_m3 = find(&rows, "minimax-anthropic", "MiniMax-M3");
    let cost = minimax_m3
        .cost
        .as_ref()
        .expect("M3 is priced with its tier");
    assert_eq!(cost.input, Some(0.30));
    assert_eq!(cost.output, Some(1.20));
    let tiers = cost.tiers.as_ref().expect("M3 keeps its context tier");
    assert!(tiers.iter().any(|t| t.tier.size == 512_000));

    // Grok 4.6 also has a prompt-length tier, starting at 200K input tokens.
    // It likewise ships priced with the tier preserved for usage-aware pricing.
    let grok_46 = find(&rows, "xai", "grok-4.6");
    let cost = grok_46
        .cost
        .as_ref()
        .expect("Grok 4.6 is priced with its tier");
    assert_eq!(cost.input, Some(2.0));
    assert_eq!(cost.output, Some(6.0));
    let tiers = cost
        .tiers
        .as_ref()
        .expect("Grok 4.6 keeps its context tier");
    assert!(tiers.iter().any(|t| t.tier.size == 200_000));

    let minimax_m2_7 = find(&rows, "minimax-anthropic", "MiniMax-M2.7");
    let cost = minimax_m2_7.cost.as_ref().expect("M2.7 is priced");
    assert_eq!(cost.input, Some(0.30));
    assert_eq!(cost.output, Some(1.20));
    assert_eq!(cost.cache_read, Some(0.06));
    assert_eq!(cost.cache_write, Some(0.375));
}

#[test]
fn live_offerings_normalize_models_dev_provider_aliases() {
    // Live Models.dev ids that must map onto CodeWhale kinds (#4186/#4187).
    let raw = r#"{
      "models": {},
      "providers": {
        "moonshotai": {
          "id": "moonshotai",
          "models": {
            "kimi-k2.5": {
              "id": "kimi-k2.5",
              "modalities": { "input": ["text"], "output": ["text"] }
            }
          }
        },
        "togetherai": {
          "id": "togetherai",
          "models": {
            "deepseek-ai/DeepSeek-V4-Pro": {
              "id": "deepseek-ai/DeepSeek-V4-Pro",
              "modalities": { "input": ["text"], "output": ["text"] }
            }
          }
        },
        "zhipuai": {
          "id": "zhipuai",
          "models": {
            "glm-5.2": {
              "id": "glm-5.2",
              "modalities": { "input": ["text"], "output": ["text"] }
            }
          }
        },
        "brand-new-gateway": {
          "id": "brand-new-gateway",
          "models": {
            "x-1": {
              "id": "x-1",
              "modalities": { "input": ["text"], "output": ["text"] }
            }
          }
        }
      }
    }"#;
    let catalog = ModelsDevCatalog::parse_json(raw).expect("fixture parses");
    let rows = live_offerings_from_models_dev(&catalog, 1_700);

    // Layer 10, and no endpoint fingerprint: a models.dev row is external
    // enrichment about a model, not a provider's statement about an endpoint.
    // Stamping `Live` here put every enriched row above the signed layer that
    // is supposed to be able to correct it.
    assert_eq!(
        find(&rows, "moonshot", "kimi-k2.5").source,
        CatalogSource::ModelsDevLive { fetched_at: 1_700 }
    );
    find(&rows, "together", "deepseek-ai/DeepSeek-V4-Pro");
    find(&rows, "zai", "glm-5.2");
    // Unknown upstream providers keep their Models.dev id.
    find(&rows, "brand-new-gateway", "x-1");
    assert!(rows.iter().all(|r| r.provider != "moonshotai"));
    assert!(rows.iter().all(|r| r.provider != "togetherai"));
    assert!(rows.iter().all(|r| r.provider != "zhipuai"));
}

fn offering(provider: &str, wire: &str, source: CatalogSource) -> CatalogOffering {
    CatalogOffering {
        provider: provider.to_string(),
        wire_model_id: wire.to_string(),
        endpoint_key: "chat".to_string(),
        source,
        ..CatalogOffering::default()
    }
}

/// The layer-25 "signed CWC catalog" this test used to pin is gone: it never
/// had a fetcher, and signed cloud facts (layer 15, under the provider roster)
/// is the client's one online catalog authority. What still has to hold on the
/// same wire is that the provider's own roster outranks models.dev.
#[test]
fn provider_live_beats_models_dev_on_the_same_wire() {
    let snapshot = CatalogCompiler::new()
        .with_bundled(vec![offering(
            "command-code",
            "deepseek/deepseek-v4-flash",
            CatalogSource::Bundled,
        )])
        .with_models_dev_live(vec![offering(
            "command-code",
            "deepseek/deepseek-v4-flash",
            CatalogSource::ModelsDevLive { fetched_at: 1 },
        )])
        .with_provider_live(vec![offering(
            "command-code",
            "deepseek/deepseek-v4-flash",
            CatalogSource::Live {
                base_url_fingerprint: "fixture".into(),
                fetched_at: 2,
            },
        )])
        .compile();
    let row = find(
        &snapshot.offerings,
        "command-code",
        "deepseek/deepseek-v4-flash",
    );
    assert!(matches!(
        row.source,
        CatalogSource::Live { ref base_url_fingerprint, fetched_at: 2 }
            if base_url_fingerprint == "fixture"
    ));
}

#[test]
fn endpoint_is_baseten_recognizes_the_host_not_the_spelling() {
    assert!(endpoint_is_baseten(BASETEN_BASE_URL));
    assert!(endpoint_is_baseten(&format!("{BASETEN_BASE_URL}/")));
    assert!(endpoint_is_baseten("HTTPS://INFERENCE.BASETEN.CO/v1"));
    assert!(!endpoint_is_baseten("https://api.groq.com/openai/v1"));
    assert!(!endpoint_is_baseten("https://127.0.0.1:9/v1"));
    assert!(!endpoint_is_baseten(""));
}

#[test]
fn stepfun_bundled_coding_models_preserve_default_and_plan_pricing_boundary() {
    let rows: Vec<_> = bundled_catalog_offerings()
        .into_iter()
        .filter(|row| row.provider == "stepfun")
        .collect();
    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows.iter()
            .find(|row| row.default_for_provider)
            .unwrap()
            .wire_model_id,
        "step-5-preview"
    );
    for row in &rows {
        assert_eq!(row.reasoning, Some(true));
        assert_eq!(row.tool_call, Some(true));
        assert!(row.cost.is_none(), "Step Plan shares ids, not PAYG billing");
        assert_eq!(row.modalities.as_ref().unwrap().output, ["text"]);
    }
    let step5 = rows
        .iter()
        .find(|row| row.wire_model_id == "step-5-preview")
        .unwrap();
    assert_eq!(step5.limit.as_ref().unwrap().context, Some(1_000_000));
    // Models.dev lists 65,536; the old seed said 1,000,000. The lower bound is
    // the one a request can always use (#6396).
    assert_eq!(step5.limit.as_ref().unwrap().output, Some(65_536));
    assert_eq!(
        step5.modalities.as_ref().unwrap().input,
        ["text", "image", "video"]
    );
    assert_eq!(
        step5.reasoning_options[0]["values"],
        serde_json::json!(["low", "medium", "high"])
    );
    let march = rows
        .iter()
        .find(|row| row.wire_model_id == "step-3.5-flash-2603")
        .unwrap();
    assert_eq!(
        march.reasoning_options[0]["values"],
        serde_json::json!(["low", "high"])
    );
    assert_eq!(march.limit.as_ref().unwrap().output, Some(256_000));
}

// ---- Codewhale corrections (#6396) -------------------------------------

const LIVE_CORRECTION_FIXTURE: &str = r#"{
  "providers": {
    "xiaomi": {
      "id": "xiaomi",
      "models": {
        "mimo-v2.6-pro": {
          "id": "mimo-v2.6-pro",
          "modalities": { "input": ["text", "image"], "output": ["text"] },
          "limit": { "context": 1048576, "output": 131072 },
          "cost": { "input": 0.435, "output": 0.87, "cache_read": 0.0036 }
        }
      }
    },
    "alibaba-token-plan": {
      "id": "alibaba-token-plan",
      "models": {
        "qwen3.8-max": {
          "id": "qwen3.8-max",
          "limit": { "context": 1000000, "output": 131072 },
          "cost": { "input": 0, "output": 0, "cache_read": 0, "cache_write": 0 }
        }
      }
    },
    "deepseek": {
      "id": "deepseek",
      "models": {
        "deepseek-v4-pro": {
          "id": "deepseek-v4-pro",
          "limit": { "context": 1000000, "output": 393216 },
          "cost": { "input": 0.435, "output": 0.87 }
        }
      }
    },
    "anthropic": {
      "id": "anthropic",
      "models": {
        "claude-opus-5": {
          "id": "claude-opus-5",
          "limit": { "context": 1000000, "output": 128000 },
          "cost": { "input": 5, "output": 25 }
        }
      }
    }
  }
}"#;

fn live_corrected_rows() -> Vec<CatalogOffering> {
    let catalog = ModelsDevCatalog::parse_json(LIVE_CORRECTION_FIXTURE).expect("fixture parses");
    live_offerings_from_models_dev(&catalog, 1_700_000_000)
}

fn codewhale_source() -> CatalogSource {
    CatalogSource::CodewhaleBundled {
        revision: corrections::bundled_corrections().revision.clone(),
    }
}

#[test]
fn committed_corrections_parse_and_name_rows_codewhale_carries() {
    let parsed = corrections::CatalogCorrections::parse(corrections::CATALOG_CORRECTIONS_JSON)
        .expect("committed corrections are valid");
    let seed = bundled_offerings_from_models_dev(bundled_models_dev_catalog());
    for rule in &parsed.providers {
        assert!(
            seed.iter().any(|row| row.provider == rule.provider),
            "provider rule {} names no seed provider",
            rule.provider
        );
    }
    for correction in &parsed.models {
        let fact = &correction.fact;
        find(&seed, &fact.provider, &fact.id);
    }
}

#[test]
fn live_refresh_cannot_undo_a_withheld_price() {
    let rows = live_corrected_rows();
    for (provider, model) in [
        ("xiaomi-mimo", "mimo-v2.6-pro"),
        ("modelstudio-token-plan", "qwen3.8-max"),
        ("deepseek", "deepseek-v4-pro"),
    ] {
        let row = find(&rows, provider, model);
        assert_eq!(row.cost, None, "{provider}/{model} keeps no flat price");
        assert!(
            matches!(row.source, CatalogSource::ModelsDevLive { .. }),
            "a corrected row stays on its own layer"
        );
        assert_eq!(row.pricing_source(), &codewhale_source());
        assert_eq!(
            row.to_offering().pricing,
            crate::route::PricingSku::UnknownOrStale,
            "{provider}/{model} must read as unknown, never as a rate or free"
        );
    }
    // Facts the correction does not touch stay the live layer's.
    let mimo = find(&rows, "xiaomi-mimo", "mimo-v2.6-pro");
    assert_eq!(
        mimo.modalities_source(),
        &CatalogSource::ModelsDevLive {
            fetched_at: 1_700_000_000
        }
    );
    assert_eq!(
        mimo.to_offering().capabilities.image_input,
        crate::route::CapabilityState::Supported
    );
    assert_eq!(mimo.limit.as_ref().and_then(|l| l.context), Some(1_048_576));

    // Rows no correction names are untouched.
    let claude = find(&rows, "anthropic", "claude-opus-5");
    assert!(matches!(claude.source, CatalogSource::ModelsDevLive { .. }));
    assert_eq!(claude.cost.as_ref().and_then(|c| c.input), Some(5.0));
}

#[test]
fn deepseek_output_limit_correction_holds_live_and_yields_to_signed_facts() {
    let rows = live_corrected_rows();
    let row = find(&rows, "deepseek", "deepseek-v4-pro").clone();
    assert_eq!(row.limit.as_ref().and_then(|l| l.output), Some(384_000));
    assert_eq!(row.to_offering().limits.output_tokens, Some(384_000));

    let key = row.merge_key();
    let mut map = BTreeMap::from([(key.clone(), row)]);
    let facts = crate::cloud_facts::ScopedFacts {
        facts_version: 1,
        key_id: "cwf-test-only".into(),
        models: vec![crate::cloud_facts::ModelFact {
            provider: key.0.clone(),
            id: key.1.clone(),
            max_output: Some(393_216),
            pricing: Some(crate::cloud_facts::PricingFact {
                input_per_m: Some(0.435),
                output_per_m: Some(0.87),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    };
    crate::cloud_facts::catalog_patch::apply_model_patches(&mut map, &facts, 1);
    let signed = &map[&key];
    assert!(matches!(signed.source, CatalogSource::CloudFacts { .. }));
    assert_eq!(signed.limit.as_ref().and_then(|l| l.output), Some(393_216));
    assert!(matches!(
        signed.to_offering().pricing,
        crate::route::PricingSku::Token { .. }
    ));
}

#[test]
fn offline_seed_rows_carry_the_same_corrections() {
    let rows = bundled_catalog_offerings();
    let deepseek = find(&rows, "deepseek", "deepseek-v4-pro");
    assert_eq!(deepseek.source, CatalogSource::Bundled);
    assert_eq!(deepseek.pricing_source(), &codewhale_source());
    assert_eq!(deepseek.cost, None);
    assert_eq!(
        deepseek.limit.as_ref().and_then(|l| l.output),
        Some(384_000)
    );
    // The seed still owns the modality statement, so its low trust holds.
    assert_eq!(deepseek.modalities_source(), &CatalogSource::Bundled);
    assert_eq!(deepseek.modalities_source, None);

    let claude = find(&rows, "anthropic", "claude-opus-5");
    assert_eq!(claude.source, CatalogSource::Bundled);
}

#[test]
fn corrections_never_add_or_hide_rows() {
    let parsed = corrections::CatalogCorrections::parse(
        r#"{
          "revision": "test",
          "models": [
            { "provider": "nobody", "id": "ghost", "context_window": 9000, "reason": "r" }
          ]
        }"#,
    )
    .expect("valid");
    let mut rows = bundled_offerings_from_models_dev(bundled_models_dev_catalog());
    let before = rows.len();
    parsed.apply_to(&mut rows);
    assert_eq!(rows.len(), before);
    assert!(!rows.iter().any(|row| row.wire_model_id == "ghost"));

    for (json, why) in [
        (
            r#"{"revision":"t","models":[{"provider":"p","id":"m","op":"hide","pricing_withheld":"x"}]}"#,
            "hide",
        ),
        (
            r#"{"revision":"t","models":[{"provider":"p","id":"m","allow_unlisted":true,"context_window":1,"reason":"x"}]}"#,
            "allow_unlisted",
        ),
        (
            r#"{"revision":"t","models":[{"provider":"p","id":"m","max_output":1}]}"#,
            "missing reason",
        ),
        (
            r#"{"revision":"t","models":[{"provider":"p","id":"m","reason":"x"}]}"#,
            "changes nothing",
        ),
        (
            r#"{"revision":"t","models":[{"provider":"p","id":"m","note":"n","pricing_withheld":"x"}]}"#,
            "annotation",
        ),
        (
            r#"{"revision":"t","providers":[{"provider":"p","pricing_withheld":""}]}"#,
            "empty provider reason",
        ),
        (r#"{"revision":"","models":[]}"#, "empty revision"),
    ] {
        assert!(
            corrections::CatalogCorrections::parse(json).is_err(),
            "{why} must be refused"
        );
    }
}

#[test]
fn pricing_withheld_round_trips_and_older_payloads_still_parse() {
    let fact = crate::cloud_facts::ModelFact {
        provider: "p".into(),
        id: "m".into(),
        pricing_withheld: Some("tiered".into()),
        ..Default::default()
    };
    let json = serde_json::to_string(&fact).expect("serializes");
    assert!(json.contains("pricing_withheld"));
    let back: crate::cloud_facts::ModelFact = serde_json::from_str(&json).expect("parses");
    assert_eq!(back, fact);

    let older: crate::cloud_facts::ModelFact =
        serde_json::from_str(r#"{"provider":"p","id":"m","context_window":9000}"#)
            .expect("payload without the field parses");
    assert_eq!(older.pricing_withheld, None);
    let plain = serde_json::to_string(&older).expect("serializes");
    assert!(!plain.contains("pricing_withheld"));
}

#[test]
fn reasoning_controls_correction_holds_on_a_live_row() {
    let catalog = ModelsDevCatalog::parse_json(
        r#"{
          "providers": {
            "xai": {
              "id": "xai",
              "models": {
                "grok-4.6": {
                  "id": "grok-4.6",
                  "reasoning": true,
                  "reasoning_options": [
                    { "type": "effort", "values": ["low", "medium", "high", "xhigh"] }
                  ]
                }
              }
            }
          }
        }"#,
    )
    .expect("fixture parses");
    let rows = live_offerings_from_models_dev(&catalog, 1);
    let grok = find(&rows, "xai", "grok-4.6");
    assert_eq!(
        grok.reasoning_options[0]
            .get("default")
            .and_then(serde_json::Value::as_str),
        Some("high"),
        "Codewhale's documented default survives a live refresh"
    );
    assert!(matches!(grok.source, CatalogSource::ModelsDevLive { .. }));

    // A signed annotation already on the row is kept alongside the controls.
    let mut row = grok.clone();
    row.reasoning_options
        .push(serde_json::json!({ "cloud_facts": { "op": "upsert" } }));
    let key = row.merge_key();
    let mut map = BTreeMap::from([(key.clone(), row)]);
    let options = vec![serde_json::json!({ "type": "effort", "values": ["high"] })];
    crate::cloud_facts::catalog_patch::apply_model_patches(
        &mut map,
        &crate::cloud_facts::ScopedFacts {
            facts_version: 1,
            key_id: "cwf-test-only".into(),
            models: vec![crate::cloud_facts::ModelFact {
                provider: key.0.clone(),
                id: key.1.clone(),
                reasoning_options: Some(options.clone()),
                ..Default::default()
            }],
            ..Default::default()
        },
        1,
    );
    let patched = &map[&key].reasoning_options;
    assert_eq!(patched[0], options[0]);
    assert!(
        patched
            .iter()
            .any(|value| value.get("cloud_facts").is_some())
    );

    assert!(
        corrections::CatalogCorrections::parse(
            r#"{"revision":"t","models":[{"provider":"p","id":"m","reasoning_options":[]}]}"#
        )
        .is_err(),
        "a reasoning correction needs a reason"
    );
}
