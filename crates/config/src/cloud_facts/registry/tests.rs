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
        serde_json::to_vec(&json!({
            "envelope": 1,
            "channel": REGISTRY_CHANNEL,
            "facts_version": payload["revision"],
            "key_id": self.key_id,
            "alg": "ed25519",
            "published_at": payload["generated_at"],
            "payload_b64": BASE64.encode(&bytes),
            "sig_b64": sig,
            "sha256": sha256_hex(&bytes),
        }))
        .expect("envelope")
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
