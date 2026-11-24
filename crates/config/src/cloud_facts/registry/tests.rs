use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ring::signature::KeyPair as _;
use serde_json::{Value, json};

use super::*;
use crate::cloud_facts::keys::KeyStatus;
use crate::cloud_facts::verify::signing_message;

const NOW: u64 = 1_795_000_000;
const GENERATED_AT: &str = "2026-11-17T00:00:00Z";

struct Signer {
    key_id: &'static str,
    pair: ring::signature::Ed25519KeyPair,
}

impl Signer {
    fn new(key_id: &'static str) -> Self {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).expect("pkcs8");
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("pair");
        Self { key_id, pair }
    }

    fn key(&self, role: KeyRole) -> TrustedKey {
        let mut public_key = [0u8; 32];
        public_key.copy_from_slice(self.pair.public_key().as_ref());
        TrustedKey {
            key_id: self.key_id,
            public_key,
            status: KeyStatus::Active,
            role,
        }
    }

    fn envelope(&self, payload: &Value) -> Vec<u8> {
        let bytes = serde_json::to_vec(payload).expect("payload");
        let sig = BASE64.encode(self.pair.sign(&signing_message(self.key_id, &bytes)));
        let mut env = json!({
            "envelope": 1,
            "channel": REGISTRY_CHANNEL,
            "facts_version": payload["revision"],
            "key_id": self.key_id,
            "alg": "ed25519",
            "published_at": payload["generated_at"],
            "payload_b64": BASE64.encode(&bytes),
            "sig_b64": sig,
            "sha256": sha256_hex(&bytes),
        });
        if let Some(not_after) = payload.get("not_after") {
            env["not_after"] = not_after.clone();
        }
        serde_json::to_vec(&env).expect("envelope")
    }
}

fn registry(revision: u64, providers: Value) -> Value {
    json!({
        "schema": REGISTRY_SCHEMA,
        "revision": revision,
        "generated_at": GENERATED_AT,
        "providers": providers,
        "models": [{"id": "acme-large"}],
        "offerings": [],
        "dialects": {
            "default_profile": "openai-responses",
            "custom_allowed_profiles": [],
            "profiles": {},
            "bindings": [],
        },
    })
}

fn acme() -> Value {
    json!({
        "id": "acme",
        "label": "Acme Inference",
        "wire_policy": "openai-compatible",
        "base_url": "https://api.acme.example/v1",
        "auth": {"secret_slot": "acme", "env_vars": ["ACME_API_KEY"]},
        "default_model": "acme-large",
    })
}

fn builtins() -> BuiltinNames {
    BuiltinNames {
        names: ["deepseek", "deep-seek", "openai"]
            .into_iter()
            .map(String::from)
            .collect(),
        env_vars: ["DEEPSEEK_API_KEY", "OPENAI_API_KEY", "SHADOW_API_KEY"]
            .into_iter()
            .map(String::from)
            .collect(),
    }
}

fn admitted(providers: Value) -> AdmittedRegistry {
    let signer = Signer::new("cwf-reg-test");
    let verified = verify_registry_envelope(
        &signer.envelope(&registry(5, providers)),
        None,
        &[signer.key(KeyRole::Founder)],
        NOW,
    )
    .expect("verified");
    admit_registry(verified, &builtins())
}

#[test]
fn founder_signed_registry_verifies_and_admits_a_new_openai_compatible_provider() {
    let admitted = admitted(json!([acme()]));
    assert_eq!(admitted.revision, 5);
    assert_eq!(admitted.registry.models.len(), 1);
    assert_eq!(
        admitted.registry.dialects["default_profile"],
        "openai-responses"
    );
    assert!(admitted.refused.is_empty(), "{:?}", admitted.refused);
    let provider = &admitted.providers[0];
    assert_eq!(provider.id, "acme");
    assert_eq!(provider.base_url, "https://api.acme.example/v1");
    assert_eq!(provider.secret_slot(), "acme");
    assert_eq!(provider.api_key_env(), "ACME_API_KEY");
}

#[test]
fn bad_signature_unknown_key_and_automation_key_are_rejected() {
    let signer = Signer::new("cwf-reg-test");
    let other = Signer::new("cwf-reg-other");
    let mut envelope: Value =
        serde_json::from_slice(&signer.envelope(&registry(5, json!([acme()])))).expect("json");
    let mut tampered = envelope.clone();
    tampered["payload_b64"] =
        json!(BASE64.encode(
            serde_json::to_vec(&registry(5, json!([acme(), {"id": "evil"}]))).expect("json")
        ));
    assert_eq!(
        verify_registry_envelope(
            &serde_json::to_vec(&tampered).expect("json"),
            None,
            &[signer.key(KeyRole::Founder)],
            NOW
        ),
        Err(FactsRejection::BadSignature)
    );
    let bytes = serde_json::to_vec(&envelope).expect("json");
    assert!(matches!(
        verify_registry_envelope(&bytes, None, &[other.key(KeyRole::Founder)], NOW),
        Err(FactsRejection::UnknownKey { .. })
    ));
    assert!(matches!(
        verify_registry_envelope(&bytes, None, &[signer.key(KeyRole::Automation)], NOW),
        Err(FactsRejection::OutOfScope(_))
    ));
    envelope["channel"] = json!("stable");
    assert!(matches!(
        verify_registry_envelope(
            &serde_json::to_vec(&envelope).expect("json"),
            None,
            &[signer.key(KeyRole::Founder)],
            NOW
        ),
        Err(FactsRejection::WrongChannel { .. })
    ));
}

#[test]
fn wrong_schema_downgrade_and_future_payloads_are_rejected() {
    let signer = Signer::new("cwf-reg-test");
    let keys = [signer.key(KeyRole::Founder)];
    let mut v2 = registry(5, json!([]));
    v2["schema"] = json!("codewhale.registry.v2");
    assert!(matches!(
        verify_registry_envelope(&signer.envelope(&v2), None, &keys, NOW),
        Err(FactsRejection::WrongSchema { .. })
    ));
    let older = signer.envelope(&registry(4, json!([])));
    assert_eq!(
        verify_registry_envelope(&older, Some(5), &keys, NOW),
        Err(FactsRejection::Rollback {
            got: 4,
            highest_seen: 5
        })
    );
    assert!(verify_registry_envelope(&older, Some(4), &keys, NOW).is_ok());
    let mut future = registry(6, json!([]));
    future["generated_at"] = json!("2027-01-01T00:00:00Z");
    assert!(matches!(
        verify_registry_envelope(&signer.envelope(&future), None, &keys, NOW),
        Err(FactsRejection::BadPayload(_))
    ));
    let mut unparsable = registry(6, json!([]));
    unparsable["providers"] = json!("not a list");
    assert!(matches!(
        verify_registry_envelope(&signer.envelope(&unparsable), None, &keys, NOW),
        Err(FactsRejection::BadPayload(_))
    ));
}

#[test]
fn pulled_rows_cannot_override_or_shadow_a_built_in_provider() {
    let mut evil_builtin = acme();
    evil_builtin["id"] = json!("deepseek");
    evil_builtin["base_url"] = json!("https://attacker.example/v1");
    let mut alias_shadow = acme();
    alias_shadow["id"] = json!("shadow-a");
    alias_shadow["aliases"] = json!(["open_ai", "openai"]);
    let mut kinded = acme();
    kinded["id"] = json!("kinded");
    kinded["kind"] = json!("Deepseek");
    let mut duplicate = acme();
    duplicate["base_url"] = json!("https://other.example/v1");
    let admitted = admitted(json!([
        evil_builtin,
        alias_shadow,
        kinded,
        acme(),
        duplicate
    ]));
    assert_eq!(admitted.builtin_rows, vec!["deepseek".to_string()]);
    assert_eq!(
        admitted
            .providers
            .iter()
            .map(|p| (p.id.as_str(), p.base_url.as_str()))
            .collect::<Vec<_>>(),
        vec![("acme", "https://api.acme.example/v1")]
    );
    let refused: Vec<&str> = admitted.refused.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(refused, vec!["shadow-a", "kinded", "acme"]);
    let bundled = BuiltinNames::bundled();
    for name in [
        "deepseek",
        "openai",
        "glm",
        "zhipu-glm",
        "anthropic-api",
        "openai-chatgpt",
        "deepseek-official",
        "deepseek-china",
        "z-ai",
    ] {
        assert!(bundled.reserved(name), "{name} is a built-in name");
    }
}

#[test]
fn pulled_rows_must_be_https_and_openai_compatible() {
    let mut http = acme();
    http["id"] = json!("plain-http");
    http["base_url"] = json!("http://api.acme.example/v1");
    let mut userinfo = acme();
    userinfo["id"] = json!("userinfo");
    userinfo["base_url"] = json!("https://user:pw@api.acme.example/v1");
    let mut anthropic = acme();
    anthropic["id"] = json!("messages");
    anthropic["wire_policy"] = json!("anthropic_messages");
    let mut no_wire = acme();
    no_wire["id"] = json!("no-wire");
    no_wire
        .as_object_mut()
        .expect("object")
        .remove("wire_policy");
    let mut rows = vec![http, userinfo, anthropic, no_wire];
    for (index, url) in [
        "https://127.0.0.1/v1",
        "https://localhost/v1",
        "https://api.localhost/v1",
        "https://[::1]/v1",
        "https://169.254.169.254/latest",
        "https://10.0.0.1/v1",
        "https://metadata.google.internal/v1",
        "https://intranet/v1",
        "https://api.acme.example:8443/v1",
    ]
    .into_iter()
    .enumerate()
    {
        let mut row = acme();
        row["id"] = json!(format!("host-{index}"));
        row["base_url"] = json!(url);
        rows.push(row);
    }
    let count = rows.len();
    let admitted = admitted(Value::Array(rows));
    assert!(admitted.providers.is_empty(), "{:?}", admitted.providers);
    assert_eq!(admitted.refused.len(), count);
}

#[test]
fn pulled_rows_take_credentials_only_from_their_own_slot_or_env_var() {
    let mut borrows_env = acme();
    borrows_env["id"] = json!("borrow-env");
    borrows_env["auth"] = json!({"env_vars": ["BORROW_ENV_API_KEY", "DEEPSEEK_API_KEY"]});
    let mut borrows_slot = acme();
    borrows_slot["id"] = json!("borrow-slot");
    borrows_slot["auth"] = json!({"secret_slot": "deepseek"});
    let mut env_collision = acme();
    env_collision["id"] = json!("shadow");
    env_collision
        .as_object_mut()
        .expect("object")
        .remove("auth");
    let admitted = admitted(json!([borrows_env, borrows_slot, env_collision, acme()]));
    let refused: Vec<&str> = admitted.refused.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(refused, vec!["borrow-env", "borrow-slot", "shadow"]);
    assert_eq!(admitted.providers.len(), 1);

    let deepseek_only = |name: &str| (name == "DEEPSEEK_API_KEY").then(|| "sk-x".to_string());
    assert!(
        admitted
            .usable_providers(|slot| slot == "deepseek", deepseek_only)
            .is_empty()
    );
    assert_eq!(
        admitted
            .usable_providers(
                |_| false,
                |name| (name == "ACME_API_KEY").then(|| "k".to_string())
            )
            .len(),
        1
    );
    assert_eq!(
        admitted
            .usable_providers(|slot| slot == "acme", |_| None)
            .len(),
        1
    );
    assert!(
        admitted
            .usable_providers(|_| false, |_| Some("  ".to_string()))
            .is_empty()
    );
}

fn rfc3339(unix_secs: u64) -> String {
    chrono::DateTime::from_timestamp(i64::try_from(unix_secs).expect("ts"), 0)
        .expect("utc")
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

#[test]
fn automation_signed_registry_with_not_after_verifies_and_resolves_models_offerings_and_aliases() {
    use std::sync::Arc;

    use crate::ProviderKind;
    use crate::catalog::{bundled_catalog_offerings, now_unix, reviewed::intrinsic_model};
    use crate::cloud_facts::overlay;
    use crate::cloud_facts::tests::overlay_test_guard;
    use crate::route::{
        CapabilityState, LogicalModelRef, PricingSku, RouteRequest, RouteResolver, resolve_model,
    };

    let _guard = overlay_test_guard();
    overlay::clear();
    let now = now_unix().max(1_795_000_000);
    let generated = now.saturating_sub(60);
    let expires = generated + 24 * 60 * 60;
    let rev = overlay::highest_seen(REGISTRY_SCHEMA).unwrap_or(0) + 10;

    let payload = json!({
        "schema": REGISTRY_SCHEMA,
        "revision": rev,
        "generated_at": rfc3339(generated),
        "not_after": rfc3339(expires),
        "providers": [
            {
                "id": "google",
                "kind": "Google",
                "base_url": "https://generativelanguage.googleapis.com/v1beta/openai/",
                "wire_policy": "chat_completions",
                "models": {
                    "ids": "catalog",
                    "strip_prefixes": ["models/"],
                    "aliases": {
                        "gemini-4.9-fast-alias": "gemini-4.9-pulled"
                    },
                    "wire_slugs": {}
                }
            },
            {
                "id": "together",
                "models": {
                    "ids": "catalog",
                    "aliases": {},
                    "wire_slugs": {
                        "deepseek-v4.9-pulled": "deepseek-ai/DeepSeek-V4.9-Pulled"
                    }
                }
            }
        ],
        "models": [
            {
                "id": "gemini-4.9-pulled",
                "catalog": {
                    "name": "Gemini 4.9 Pulled",
                    "family": "gemini",
                    "reasoning": true,
                    "tool_call": true,
                    "structured_output": true,
                    "limit": {
                        "context": 512000,
                        "output": 131072
                    }
                }
            }
        ],
        "offerings": [
            {
                "provider": "google",
                "id": "gemini-4.9-pulled",
                "model": "gemini-4.9-pulled",
                "catalog": {
                    "limit": {
                        "context": 512000,
                        "output": 131072
                    },
                    "cost": {
                        "input": 3.5,
                        "output": 14.0,
                        "cache_read": 0.35
                    },
                    "reasoning": true,
                    "tool_call": true
                }
            },
            {
                "provider": "together",
                "id": "deepseek-ai/DeepSeek-V4.9-Pulled",
                "model": "gemini-4.9-pulled",
                "catalog": {
                    "limit": {
                        "context": 262144,
                        "output": 65536
                    },
                    "cost": {
                        "input": 0.8,
                        "output": 2.4
                    }
                }
            }
        ],
        "dialects": {}
    });

    let signer = Signer::new("cwf-auto-reg");
    let verified = verify_registry_envelope(
        &signer.envelope(&payload),
        None,
        &[signer.key(KeyRole::Automation)],
        now,
    )
    .expect("automation model/offering registry verifies");
    assert_eq!(verified.key_role, KeyRole::Automation);
    assert_eq!(verified.not_after, Some(expires));

    let admitted = admit_registry(verified, &BuiltinNames::bundled());
    assert!(admitted.providers.is_empty());
    assert!(admitted.refused.is_empty());

    let ticket = overlay::configure(true, "auto-reg-wire-test").expect("ticket");
    assert!(overlay::publish_registry(
        &ticket,
        Some(Arc::new(admitted)),
        |_| {}
    ));

    let resolved = resolve_model(ProviderKind::Google, "models/gemini-4.9-fast-alias", None)
        .expect("resolve_model");
    assert!(resolved.alias_matched);
    assert_eq!(resolved.logical, "gemini-4.9-pulled");
    assert_eq!(resolved.wire_id, "gemini-4.9-pulled");
    assert_eq!(
        resolved
            .offering
            .as_ref()
            .and_then(|o| o.limits.context_tokens),
        Some(512_000)
    );

    let slugged = resolve_model(ProviderKind::Together, "deepseek-v4.9-pulled", None)
        .expect("together wire slug");
    assert_eq!(slugged.logical, "deepseek-v4.9-pulled");
    assert_eq!(slugged.wire_id, "deepseek-ai/DeepSeek-V4.9-Pulled");

    for resolver in [RouteResolver::new(), RouteResolver::bundled().clone()] {
        let candidate = resolver
            .resolve(&RouteRequest {
                explicit_provider: Some(ProviderKind::Google),
                model_selector: Some(LogicalModelRef::from("gemini-4.9-fast-alias")),
                ..RouteRequest::default()
            })
            .expect("route candidate");
        assert_eq!(candidate.wire_model_id().as_str(), "gemini-4.9-pulled");
        assert_eq!(
            candidate.endpoint().base_url,
            "https://generativelanguage.googleapis.com/v1beta/openai/"
        );
        assert_eq!(candidate.limits().context_tokens, Some(512_000));
        assert_eq!(candidate.limits().output_tokens, Some(131_072));
        assert_eq!(
            candidate.capabilities().reasoning,
            CapabilityState::Supported
        );
        assert_eq!(
            candidate.capabilities().native_tool_calls,
            CapabilityState::Supported
        );
        assert_eq!(
            candidate.pricing(),
            Some(&PricingSku::Token {
                input_per_mtok: Some(3.5),
                output_per_mtok: Some(14.0),
            })
        );
    }

    let intrinsic = intrinsic_model("gemini-4.9-pulled").expect("pulled intrinsic model");
    assert_eq!(intrinsic.context_window, Some(512_000));
    assert_eq!(intrinsic.max_output, Some(131_072));
    assert_eq!(intrinsic.reasoning, Some(true));
    assert!(
        bundled_catalog_offerings()
            .iter()
            .any(|row| row.provider == "google" && row.wire_model_id == "gemini-4.9-pulled")
    );

    assert!(overlay::pulled_registry_at(expires).is_none());
    overlay::clear();

    let reverted = resolve_model(ProviderKind::Google, "gemini-4.9-fast-alias", None)
        .expect("catalog fallback after clear");
    assert!(!reverted.alias_matched);
    assert_eq!(reverted.wire_id, "gemini-4.9-fast-alias");
    assert!(reverted.offering.is_none());
    assert!(intrinsic_model("gemini-4.9-pulled").is_none());
    assert!(
        !bundled_catalog_offerings()
            .iter()
            .any(|row| row.provider == "google" && row.wire_model_id == "gemini-4.9-pulled")
    );
}

#[test]
fn automation_signed_registry_rejects_missing_expired_excessive_not_after_and_provider_mutations() {
    use crate::cloud_facts::MAX_AUTOMATION_WINDOW_SECS;
    use crate::cloud_facts::verify::parse_rfc3339_utc;

    let signer = Signer::new("cwf-auto-reject");
    let keys = [signer.key(KeyRole::Automation)];
    let gen_unix = parse_rfc3339_utc(GENERATED_AT).expect("gen");
    let valid_not_after = rfc3339(gen_unix + 3600);
    let now = gen_unix + 60;

    let without_not_after = registry(6, json!([]));
    assert!(matches!(
        verify_registry_envelope(&signer.envelope(&without_not_after), None, &keys, now),
        Err(FactsRejection::OutOfScope(_))
    ));

    let mut unsigned_outer: Value =
        serde_json::from_slice(&signer.envelope(&without_not_after)).expect("env");
    unsigned_outer["not_after"] = json!(valid_not_after);
    assert!(matches!(
        verify_registry_envelope(
            &serde_json::to_vec(&unsigned_outer).expect("bytes"),
            None,
            &keys,
            now
        ),
        Err(FactsRejection::Mismatch(_))
    ));

    let mut expired = registry(6, json!([]));
    expired["not_after"] = json!(valid_not_after);
    assert!(matches!(
        verify_registry_envelope(&signer.envelope(&expired), None, &keys, gen_unix + 3600),
        Err(FactsRejection::OutOfScope(_))
    ));

    let mut window_too_wide = registry(6, json!([]));
    window_too_wide["not_after"] = json!(rfc3339(gen_unix + MAX_AUTOMATION_WINDOW_SECS + 1));
    assert!(matches!(
        verify_registry_envelope(&signer.envelope(&window_too_wide), None, &keys, now),
        Err(FactsRejection::OutOfScope(_))
    ));

    let mut with_new_provider = registry(6, json!([acme()]));
    with_new_provider["not_after"] = json!(valid_not_after);
    assert!(matches!(
        verify_registry_envelope(&signer.envelope(&with_new_provider), None, &keys, now),
        Err(FactsRejection::OutOfScope(_))
    ));

    let mut with_founder_admission = registry(
        6,
        json!([{
            "id": "groq",
            "admission": {
                "id": "adm_1",
                "reviewed_by": "founder",
                "reviewed_at": GENERATED_AT,
                "founder_signature": "required"
            }
        }]),
    );
    with_founder_admission["not_after"] = json!(valid_not_after);
    assert!(matches!(
        verify_registry_envelope(&signer.envelope(&with_founder_admission), None, &keys, now),
        Err(FactsRejection::OutOfScope(_))
    ));

    for mutated in [
        json!({"id": "openai", "base_url": "https://evil.example/v1"}),
        json!({"id": "openai", "wire_policy": "chat"}),
        json!({"id": "openai", "kind": "Deepseek"}),
        json!({"id": "openai", "auth": {"secret_slot": "evil", "env_vars": ["EVIL_API_KEY"]}}),
        json!({"id": "groq", "base_url": "https://evil.example/v1"}),
        json!({"id": "groq", "api_key_env": "EVIL_API_KEY"}),
        json!({"id": "cheaperinference", "auth": {"secret_slot": "cheaperinference", "env_vars": ["CHEAPERINFERENCE_API_KEY"]}}),
        json!({"id": "deepseek", "models": {"accept": ["*"]}}),
        json!({"id": "google", "models": {"strip_prefixes": ["evil/"]}}),
        json!({"id": "together", "models": {"ids": "verbatim"}}),
        json!({"id": "together", "default_model": "evil-model"}),
        json!({"id": "cheaperinference", "default_model": "evil-model"}),
        json!({"id": "together", "catalog": {"api": "https://evil.example/v1"}}),
        json!({"id": "together", "corrections": [{"pricing_withheld": "evil"}]}),
        json!({"id": "together", "label": "Evil"}),
    ] {
        let mut doc = registry(6, json!([mutated.clone()]));
        doc["not_after"] = json!(valid_not_after);
        assert!(
            matches!(
                verify_registry_envelope(&signer.envelope(&doc), None, &keys, now),
                Err(FactsRejection::OutOfScope(_))
            ),
            "expected OutOfScope for {mutated}"
        );
    }

    for offering in [
        json!({"provider": "together", "id": "evil-model", "pricing_withheld": "evil"}),
        json!({"provider": "together", "id": "evil-model", "corrections": [{"pricing_withheld": "evil"}]}),
        json!({"provider": "together", "id": "evil-model", "catalog": {"default": true}}),
        json!({"provider": "together", "id": "evil-model", "default_for_provider": true}),
        json!({"provider": "together", "id": "evil-model", "transport": {"default_for_provider": true}}),
    ] {
        let mut doc = registry(6, json!([]));
        doc["not_after"] = json!(valid_not_after);
        doc["offerings"] = json!([offering.clone()]);
        assert!(
            matches!(
                verify_registry_envelope(&signer.envelope(&doc), None, &keys, now),
                Err(FactsRejection::OutOfScope(_))
            ),
            "expected OutOfScope for {offering}"
        );
        assert!(
            verify_registry_envelope(
                &signer.envelope(&doc),
                None,
                &[signer.key(KeyRole::Founder)],
                now
            )
            .is_ok(),
            "founder may sign {offering}"
        );
    }

    let mut widened = registry(6, json!([{"id": "deepseek", "models": {"accept": ["*"]}}]));
    widened["not_after"] = json!(valid_not_after);
    assert!(
        verify_registry_envelope(
            &signer.envelope(&widened),
            None,
            &[signer.key(KeyRole::Founder)],
            now
        )
        .is_ok()
    );

    let mut echoed = registry(
        6,
        json!([{
            "id": "together",
            "label": "Together AI",
            "default_model": "deepseek-ai/DeepSeek-V4-Pro",
            "models": {
                "ids": "catalog",
                "accept": [],
                "strip_prefixes": [],
                "aliases": {"together-pulled": "deepseek-ai/DeepSeek-V4-Pro"},
                "wire_slugs": {"deepseek-v4.9-pulled": "deepseek-ai/DeepSeek-V4.9-Pulled"}
            }
        }]),
    );
    echoed["not_after"] = json!(valid_not_after);
    echoed["offerings"] =
        json!([{"provider": "together", "id": "new-model", "catalog": {"default": false}}]);
    assert!(verify_registry_envelope(&signer.envelope(&echoed), None, &keys, now).is_ok());

    let mut valid_bundled_descriptor = registry(
        6,
        json!([{
            "id": "cheaperinference",
            "wire": "openai-compatible",
            "wire_policy": "openai-compatible",
            "base_url": "https://api.cheaperinference.com/v1",
            "api_key_env": "CHEAPER_INFERENCE_API_KEY",
            "auth": {
                "secret_slot": "cheaperinference",
                "env_vars": ["CHEAPER_INFERENCE_API_KEY"]
            }
        }]),
    );
    valid_bundled_descriptor["not_after"] = json!(valid_not_after);
    assert!(
        verify_registry_envelope(
            &signer.envelope(&valid_bundled_descriptor),
            None,
            &keys,
            now
        )
        .is_ok()
    );
}

#[test]
fn founder_signed_registry_admits_new_provider_discovers_it_and_clears_on_disable() {
    use std::sync::Arc;

    use crate::catalog::{bundled_catalog_offerings, now_unix, reviewed::intrinsic_model};
    use crate::cloud_facts::overlay;
    use crate::cloud_facts::tests::overlay_test_guard;
    use crate::descriptors::{
        DescriptorDiscovery, active_provider_descriptors, bundled_provider_descriptors,
        provider_descriptor,
    };

    let _guard = overlay_test_guard();
    overlay::clear();
    let now = now_unix().max(1_795_000_000);
    let generated = now.saturating_sub(60);
    let rev = overlay::highest_seen(REGISTRY_SCHEMA).unwrap_or(0) + 15;

    let payload = json!({
        "schema": REGISTRY_SCHEMA,
        "revision": rev,
        "generated_at": rfc3339(generated),
        "providers": [
            {
                "id": "acme-cloud",
                "label": "Acme Cloud",
                "origin": "descriptor",
                "wire_policy": "openai-compatible",
                "base_url": "https://api.acme-cloud.example/v1",
                "auth": {
                    "secret_slot": "acme-cloud",
                    "env_vars": ["ACME_CLOUD_API_KEY"]
                },
                "default_model": "acme-sonnet",
                "aliases": ["acme-ai"],
                "discovery": "models_endpoint",
                "docs_url": "https://docs.acme-cloud.example/openai",
                "credential_url": "https://console.acme-cloud.example/keys",
                "guidance": "Create ACME_CLOUD_API_KEY in the Acme console.",
                "admission": {
                    "id": "adm_acme",
                    "reviewed_by": "founder",
                    "reviewed_at": rfc3339(generated),
                    "founder_signature": "required"
                }
            },
            {
                "id": "tavily",
                "label": "Tavily Collision",
                "wire_policy": "openai-compatible",
                "base_url": "https://api.tavily.example/v1"
            },
            {
                "id": "codewhale-cli",
                "label": "CLI Collision",
                "wire_policy": "openai-compatible",
                "base_url": "https://api.cli.example/v1"
            }
        ],
        "models": [
            {
                "id": "acme-sonnet",
                "catalog": {
                    "name": "Acme Sonnet",
                    "limit": {
                        "context": 200000,
                        "output": 64000
                    },
                    "reasoning": true,
                    "tool_call": true
                }
            }
        ],
        "offerings": [
            {
                "provider": "acme-cloud",
                "id": "acme-sonnet",
                "model": "acme-sonnet",
                "catalog": {
                    "default": true,
                    "limit": {
                        "context": 200000,
                        "output": 64000
                    },
                    "cost": {
                        "input": 1.5,
                        "output": 6.0
                    },
                    "reasoning": true,
                    "tool_call": true
                }
            },
            {
                "provider": "tavily",
                "id": "tavily-leaked-offering",
                "catalog": {
                    "limit": {
                        "context": 128000,
                        "output": 16384
                    }
                }
            }
        ],
        "dialects": {}
    });

    let signer = Signer::new("cwf-founder-reg");
    let verified = verify_registry_envelope(
        &signer.envelope(&payload),
        None,
        &[signer.key(KeyRole::Founder)],
        now,
    )
    .expect("founder registry verifies");
    let admitted = admit_registry(verified, &BuiltinNames::bundled());
    assert_eq!(admitted.providers.len(), 1);
    assert_eq!(admitted.providers[0].id, "acme-cloud");
    let refused_ids: Vec<&str> = admitted.refused.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(refused_ids, vec!["tavily", "codewhale-cli"]);

    let ticket = overlay::configure(true, "founder-reg-wire-test").expect("ticket");
    assert!(overlay::publish_registry(
        &ticket,
        Some(Arc::new(admitted)),
        |_| {}
    ));

    let desc = provider_descriptor("acme-cloud").expect("acme-cloud discoverable");
    assert_eq!(desc.label, "Acme Cloud");
    assert_eq!(desc.base_url, "https://api.acme-cloud.example/v1");
    assert_eq!(desc.api_key_env, "ACME_CLOUD_API_KEY");
    assert_eq!(desc.default_model, "acme-sonnet");
    assert_eq!(desc.discovery, DescriptorDiscovery::ModelsEndpoint);
    assert_eq!(
        provider_descriptor("acme-ai")
            .as_ref()
            .map(|d| d.id.as_str()),
        Some("acme-cloud")
    );
    assert_eq!(
        active_provider_descriptors().len(),
        bundled_provider_descriptors().len() + 1
    );

    let offerings = bundled_catalog_offerings();
    assert!(
        offerings
            .iter()
            .any(|row| row.provider == "acme-cloud" && row.wire_model_id == "acme-sonnet")
    );
    assert!(
        !offerings
            .iter()
            .any(|row| row.provider == "tavily" || row.wire_model_id == "tavily-leaked-offering")
    );
    assert!(intrinsic_model("acme-sonnet").is_some());
    assert!(crate::descriptors::is_registry_admitted_base_url(
        "https://API.acme-cloud.example./v1/chat/completions"
    ));
    assert!(!crate::descriptors::is_registry_admitted_base_url(
        "https://api.other.example/v1"
    ));
    assert!(!crate::descriptors::is_registry_admitted_base_url(
        "http://localhost:11434/v1"
    ));

    overlay::clear();
    assert!(!crate::descriptors::is_registry_admitted_base_url(
        "https://api.acme-cloud.example/v1"
    ));
    assert!(provider_descriptor("acme-cloud").is_none());
    assert!(provider_descriptor("acme-ai").is_none());
    assert_eq!(
        active_provider_descriptors().len(),
        bundled_provider_descriptors().len()
    );
    assert!(
        !bundled_catalog_offerings()
            .iter()
            .any(|row| row.provider == "acme-cloud")
    );
    assert!(intrinsic_model("acme-sonnet").is_none());
}
