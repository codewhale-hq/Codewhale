use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ring::signature::KeyPair as _;
use serde_json::{Value, json};

use super::catalog_patch::apply_model_patches;
use super::keys::{AUTOMATION_MAX_VALIDITY_SECS, KeyRole, KeyStatus, TrustedKey};
use super::overlay;
use super::provenance::{CloudFactsState, CloudFactsStatus, FactsOrigin};
use super::scope::{base_url_allowed, scoped_view};
use super::types::ModelOp;
use super::verify::{FactsRejection, parse_rfc3339_utc, signing_message, verify_envelope};
use crate::catalog::{CatalogCompiler, CatalogOffering, CatalogSource};

/// Cross-language fixture signed by `web/scripts/facts-publish.mjs` with the
/// TEST-ONLY key (`docs/cloud-facts/fixtures/test-only-signing-key.pem`).
const FIXTURE_V7: &str =
    include_str!("../../../../docs/cloud-facts/fixtures/envelope-stable-v7.json");
const FIXTURE_FUTURE_V8: &str =
    include_str!("../../../../docs/cloud-facts/fixtures/envelope-future-only-v8.json");
const TEST_ONLY_PUB_B64: &str = "8+FLDW4OorUETUVks0hpQAi5Lj4wg3kjKjfYFzLbJ7U=";
const NOW: u64 = 1_790_000_000; // 2026-09-21T...Z

fn test_only_key(status: KeyStatus) -> TrustedKey {
    let raw = BASE64.decode(TEST_ONLY_PUB_B64).expect("pub b64");
    let mut public_key = [0u8; 32];
    public_key.copy_from_slice(&raw);
    TrustedKey {
        key_id: "cwf-test-only",
        public_key,
        status,
        role: KeyRole::Founder,
    }
}

fn v(s: &str) -> semver::Version {
    semver::Version::parse(s).expect("semver")
}

/// Ephemeral signer for in-Rust variants (tamper, rotation, schema bumps).
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

    fn trusted(&self, status: KeyStatus) -> TrustedKey {
        let mut public_key = [0u8; 32];
        public_key.copy_from_slice(self.pair.public_key().as_ref());
        TrustedKey {
            key_id: self.key_id,
            public_key,
            status,
            role: KeyRole::Founder,
        }
    }

    fn automation(&self) -> TrustedKey {
        TrustedKey {
            role: KeyRole::Automation,
            ..self.trusted(KeyStatus::Active)
        }
    }

    fn sign(&self, payload: &[u8]) -> String {
        BASE64.encode(self.pair.sign(&signing_message(self.key_id, payload)))
    }

    fn envelope(&self, payload: &Value) -> Value {
        let bytes = serde_json::to_vec(payload).expect("payload json");
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        json!({
            "envelope": 1,
            "channel": payload["channel"],
            "facts_version": payload["facts_version"],
            "schema_version": payload["schema_version"],
            "key_id": self.key_id,
            "alg": "ed25519",
            "applies_to": payload["applies_to"],
            "published_at": payload["published_at"],
            "sha256": digest,
            "payload_b64": BASE64.encode(&bytes),
            "sig_b64": self.sign(&bytes),
            "sigs": [],
        })
    }
}

fn payload(channel: &str, version: u64, applies_to: &str) -> Value {
    json!({
        "schema_version": 1,
        "channel": channel,
        "facts_version": version,
        "published_at": "2026-08-30T00:00:00Z",
        "applies_to": applies_to,
        "models": [],
        "provider_defaults": {},
        "release": null,
        "announcements": [],
    })
}

fn verify(bytes: &[u8], keys: &[TrustedKey]) -> Result<super::VerifiedFacts, FactsRejection> {
    verify_envelope(bytes, "stable", &v("0.9.11"), None, keys, NOW)
}

#[test]
fn node_signed_fixture_verifies_under_the_test_only_key() {
    let keys = [test_only_key(KeyStatus::Active)];
    let verified = verify(FIXTURE_V7.as_bytes(), &keys).expect("fixture verifies");
    assert_eq!(verified.key_id, "cwf-test-only");
    assert_eq!(verified.facts.facts_version, 7);
    assert_eq!(verified.facts.channel, "stable");
    assert_eq!(verified.facts.models.len(), 6);
    assert!(!verified.stale);
    // sha256 in the envelope matches the recomputed digest of the signed bytes.
    let env: Value = serde_json::from_str(FIXTURE_V7).expect("json");
    assert_eq!(env["sha256"].as_str(), Some(verified.sha256.as_str()));
}

#[test]
fn fixture_is_rejected_with_no_trusted_keys() {
    let err = verify(FIXTURE_V7.as_bytes(), &[]).expect_err("no keys");
    assert!(matches!(err, FactsRejection::UnknownKey { .. }), "{err:?}");
}

#[test]
fn retired_key_is_rejected_distinctly() {
    let keys = [test_only_key(KeyStatus::Retired)];
    let err = verify(FIXTURE_V7.as_bytes(), &keys).expect_err("retired");
    assert_eq!(
        err,
        FactsRejection::RetiredKey {
            key_id: "cwf-test-only".into()
        }
    );
}

#[test]
fn flipping_payload_signature_or_key_id_bytes_breaks_verification() {
    let keys = [test_only_key(KeyStatus::Active)];
    let mut env: Value = serde_json::from_str(FIXTURE_V7).expect("json");

    // Payload tamper (decode → flip → encode keeps the envelope well-formed).
    let mut bytes = BASE64.decode(env["payload_b64"].as_str().unwrap()).unwrap();
    bytes[10] ^= 0x01;
    let mut tampered = env.clone();
    tampered["payload_b64"] = Value::String(BASE64.encode(&bytes));
    let err = verify(&serde_json::to_vec(&tampered).unwrap(), &keys).unwrap_err();
    assert_eq!(err, FactsRejection::BadSignature);

    // Signature tamper.
    let mut sig = BASE64.decode(env["sig_b64"].as_str().unwrap()).unwrap();
    sig[3] ^= 0x80;
    let mut tampered = env.clone();
    tampered["sig_b64"] = Value::String(BASE64.encode(&sig));
    let err = verify(&serde_json::to_vec(&tampered).unwrap(), &keys).unwrap_err();
    assert_eq!(err, FactsRejection::BadSignature);

    // Re-labelling under another pinned key id must fail: key_id is signed.
    let other = TrustedKey {
        key_id: "cwf-other",
        ..test_only_key(KeyStatus::Active)
    };
    env["key_id"] = Value::String("cwf-other".into());
    let err = verify(&serde_json::to_vec(&env).unwrap(), &[other]).unwrap_err();
    assert_eq!(err, FactsRejection::BadSignature);
}

#[test]
fn envelope_shape_errors_come_before_crypto() {
    let keys = [test_only_key(KeyStatus::Active)];
    let base: Value = serde_json::from_str(FIXTURE_V7).expect("json");

    let mut e = base.clone();
    e["alg"] = json!("rsa");
    assert!(matches!(
        verify(&serde_json::to_vec(&e).unwrap(), &keys).unwrap_err(),
        FactsRejection::BadEnvelope(_)
    ));

    let mut e = base.clone();
    e["envelope"] = json!(2);
    assert!(matches!(
        verify(&serde_json::to_vec(&e).unwrap(), &keys).unwrap_err(),
        FactsRejection::BadEnvelope(_)
    ));

    let big = vec![b' '; super::MAX_ENVELOPE_BYTES + 1];
    assert!(matches!(
        verify(&big, &keys).unwrap_err(),
        FactsRejection::TooLarge { .. }
    ));

    // Oversized payload_b64 is refused before base64 decode / crypto.
    let mut e = base.clone();
    e["payload_b64"] = Value::String("A".repeat(super::MAX_PAYLOAD_BYTES * 4 / 3 + 64));
    assert!(matches!(
        verify(&serde_json::to_vec(&e).unwrap(), &keys).unwrap_err(),
        FactsRejection::TooLarge { .. }
    ));
}

#[test]
fn outer_inner_mismatch_wrong_channel_and_schema_too_new_are_rejected() {
    let signer = Signer::new("cwf-unit");
    let keys = [signer.trusted(KeyStatus::Active)];

    let mut env = signer.envelope(&payload("stable", 3, "*"));
    env["facts_version"] = json!(4);
    let err = verify(&serde_json::to_vec(&env).unwrap(), &keys).unwrap_err();
    assert!(matches!(err, FactsRejection::Mismatch(_)), "{err:?}");

    let env = signer.envelope(&payload("beta", 3, "*"));
    let err = verify(&serde_json::to_vec(&env).unwrap(), &keys).unwrap_err();
    assert_eq!(
        err,
        FactsRejection::WrongChannel {
            expected: "stable".into(),
            got: "beta".into()
        }
    );

    let mut p = payload("stable", 3, "*");
    p["schema_version"] = json!(2);
    let env = signer.envelope(&p);
    let err = verify(&serde_json::to_vec(&env).unwrap(), &keys).unwrap_err();
    assert_eq!(err, FactsRejection::SchemaTooNew { schema_version: 2 });
}

#[test]
fn applies_to_scopes_the_whole_payload_by_binary_version() {
    let keys = [test_only_key(KeyStatus::Active)];
    let err = verify(FIXTURE_FUTURE_V8.as_bytes(), &keys).unwrap_err();
    assert_eq!(
        err,
        FactsRejection::NotApplicable {
            applies_to: ">=99.0.0".into()
        }
    );
    // Same envelope is accepted by a binary inside the range.
    let ok = verify_envelope(
        FIXTURE_FUTURE_V8.as_bytes(),
        "stable",
        &v("99.1.0"),
        None,
        &keys,
        NOW,
    );
    assert!(ok.is_ok(), "{ok:?}");

    let signer = Signer::new("cwf-unit");
    let keys = [signer.trusted(KeyStatus::Active)];
    let env = signer.envelope(&payload("stable", 1, "not a range"));
    let err = verify(&serde_json::to_vec(&env).unwrap(), &keys).unwrap_err();
    assert!(matches!(err, FactsRejection::BadVersionReq(_)));

    // Prerelease binaries follow semver: a plain range does not match them.
    let env = signer.envelope(&payload("stable", 1, ">=0.9.0, <1.0.0"));
    let bytes = serde_json::to_vec(&env).unwrap();
    assert!(verify_envelope(&bytes, "stable", &v("0.9.12-beta.1"), None, &keys, NOW).is_err());
    assert!(verify_envelope(&bytes, "stable", &v("0.9.12"), None, &keys, NOW).is_ok());
}

#[test]
fn rollback_protection_accepts_equal_and_higher_only() {
    let signer = Signer::new("cwf-unit");
    let keys = [signer.trusted(KeyStatus::Active)];
    let env = serde_json::to_vec(&signer.envelope(&payload("stable", 5, "*"))).unwrap();
    assert!(verify_envelope(&env, "stable", &v("0.9.11"), Some(4), &keys, NOW).is_ok());
    assert!(verify_envelope(&env, "stable", &v("0.9.11"), Some(5), &keys, NOW).is_ok());
    let err = verify_envelope(&env, "stable", &v("0.9.11"), Some(6), &keys, NOW).unwrap_err();
    assert_eq!(
        err,
        FactsRejection::Rollback {
            got: 5,
            highest_seen: 6
        }
    );
}

#[test]
fn not_after_only_downgrades_to_stale_after_grace() {
    let signer = Signer::new("cwf-unit");
    let keys = [signer.trusted(KeyStatus::Active)];
    let mut p = payload("stable", 5, "*");
    p["not_after"] = json!("2026-09-01T00:00:00Z");
    let env = serde_json::to_vec(&signer.envelope(&p)).unwrap();
    let not_after = parse_rfc3339_utc("2026-09-01T00:00:00Z").unwrap();
    let fresh = verify_envelope(&env, "stable", &v("0.9.11"), None, &keys, not_after - 1).unwrap();
    assert!(!fresh.stale);
    let in_grace =
        verify_envelope(&env, "stable", &v("0.9.11"), None, &keys, not_after + 3600).unwrap();
    assert!(!in_grace.stale);
    let stale = verify_envelope(
        &env,
        "stable",
        &v("0.9.11"),
        None,
        &keys,
        not_after + super::verify::NOT_AFTER_GRACE_SECS + 1,
    )
    .unwrap();
    assert!(stale.stale, "past grace must be stale, not rejected");
}

#[test]
fn rotation_accepts_any_pinned_active_signature() {
    let old = Signer::new("cwf-old");
    let new = Signer::new("cwf-new");
    let p = payload("stable", 9, "*");
    let bytes = serde_json::to_vec(&p).unwrap();
    // Primary signature by the old key, extra by the new key.
    let mut env = old.envelope(&p);
    env["sigs"] = json!([{ "key_id": "cwf-new", "sig_b64": new.sign(&bytes) }]);
    let bytes = serde_json::to_vec(&env).unwrap();
    // A client that only pins the new key still accepts it.
    let ok = verify(&bytes, &[new.trusted(KeyStatus::Active)]).unwrap();
    assert_eq!(ok.key_id, "cwf-new");
    // A client that retired the old key and pins the new one accepts it too.
    let ok = verify(
        &bytes,
        &[
            old.trusted(KeyStatus::Retired),
            new.trusted(KeyStatus::Active),
        ],
    )
    .unwrap();
    assert_eq!(ok.key_id, "cwf-new");
    // A client pinning only the old key (still active) accepts via primary.
    let ok = verify(&bytes, &[old.trusted(KeyStatus::Active)]).unwrap();
    assert_eq!(ok.key_id, "cwf-old");
}

#[test]
fn unknown_payload_fields_are_preserved_not_acted_on() {
    let signer = Signer::new("cwf-unit");
    let keys = [signer.trusted(KeyStatus::Active)];
    let mut p = payload("stable", 5, "*");
    p["policy"] = json!({ "deny": ["everything"] });
    let env = serde_json::to_vec(&signer.envelope(&p)).unwrap();
    let ok = verify(&env, &keys).unwrap();
    assert!(ok.facts.unknown.contains_key("policy"));
    let scoped = scoped_view(&ok, &v("0.9.11"), NOW);
    assert!(scoped.models.is_empty() && scoped.provider_defaults.is_empty());
}

#[test]
fn scoped_view_filters_items_and_enforces_the_base_url_allowlist() {
    let keys = [test_only_key(KeyStatus::Active)];
    let verified = verify(FIXTURE_V7.as_bytes(), &keys).unwrap();
    let scoped = scoped_view(&verified, &v("0.9.11"), NOW);

    let ids: Vec<&str> = scoped.models.iter().map(|m| m.id.as_str()).collect();
    assert!(
        !ids.contains(&"future-only"),
        "per-item applies_to must filter: {ids:?}"
    );
    assert_eq!(scoped.models.len(), 5);

    assert_eq!(
        scoped.provider_defaults["deepseek"]
            .default_model
            .as_deref(),
        Some("deepseek-v4-pro")
    );
    assert_eq!(
        scoped.provider_defaults["deepseek"].base_url.as_deref(),
        Some("https://api.deepseek.com/beta")
    );
    assert!(
        !scoped.provider_defaults.contains_key("openai"),
        "off-family base_url must be dropped entirely"
    );
    assert!(
        !scoped.provider_defaults.contains_key("ollama"),
        "local providers accept no cloud base_url"
    );
    assert!(
        scoped
            .dropped
            .iter()
            .any(|d| d.contains("official HTTPS endpoint"))
    );

    let announcements: Vec<&str> = scoped.announcements.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(announcements, vec!["fixture-live"]);
    assert_eq!(scoped.release.as_ref().unwrap().yanked, vec!["0.9.10"]);
}

#[test]
fn base_url_allowlist_is_https_and_official_host_family_only() {
    assert!(base_url_allowed(
        "deepseek",
        "https://api.deepseek.com/beta"
    ));
    assert!(!base_url_allowed(
        "deepseek",
        "https://eu.api.deepseek.com/v1"
    ));
    assert!(!base_url_allowed(
        "deepseek",
        "https://api.deepseek.com/other"
    ));
    assert!(!base_url_allowed(
        "deepseek",
        "https://evil.example\\api.deepseek.com/v1"
    ));
    assert!(!base_url_allowed(
        "deepseek",
        "http://api.deepseek.com/beta"
    ));
    assert!(!base_url_allowed(
        "deepseek",
        "https://api.deepseek.com.evil.example/"
    ));
    assert!(!base_url_allowed(
        "deepseek",
        "https://user@api.deepseek.com/"
    ));
    assert!(!base_url_allowed("openai", "https://evil.example/v1"));
    assert!(!base_url_allowed("ollama", "https://localhost:11434/v1"));
    assert!(!base_url_allowed("no-such-provider", "https://x.example/"));
}

#[test]
fn catalog_layer_15_patches_sit_between_models_dev_and_provider_live() {
    let keys = [test_only_key(KeyStatus::Active)];
    let verified = verify(FIXTURE_V7.as_bytes(), &keys).unwrap();
    let scoped = scoped_view(&verified, &v("0.9.11"), NOW);

    let row = |id: &str, source: CatalogSource| CatalogOffering {
        provider: "deepseek".into(),
        wire_model_id: id.into(),
        endpoint_key: "chat".into(),
        limit: Some(crate::models_dev::ModelsDevLimit {
            context: Some(1000),
            input: None,
            output: Some(10),
        }),
        reasoning: Some(false),
        source,
        ..CatalogOffering::default()
    };
    let snapshot = CatalogCompiler::new()
        .with_bundled(vec![
            row("deepseek-v4-pro", CatalogSource::Bundled),
            row("deepseek-chat", CatalogSource::Bundled),
            row("deepseek-reasoner", CatalogSource::Bundled),
        ])
        .with_cloud_facts(&scoped, NOW)
        .with_provider_live(vec![row(
            "deepseek-reasoner",
            CatalogSource::Live {
                base_url_fingerprint: "fp".into(),
                fetched_at: NOW,
            },
        )])
        .compile();

    let find = |id: &str| {
        snapshot
            .offerings
            .iter()
            .find(|o| o.wire_model_id == id)
            .cloned()
    };
    // Upsert patched only the fields it set; untouched fields survive.
    let pro = find("deepseek-v4-pro").expect("patched row");
    assert_eq!(pro.limit.as_ref().unwrap().context, Some(262_144));
    assert_eq!(pro.limit.as_ref().unwrap().output, Some(32_768));
    assert_eq!(pro.reasoning, Some(false), "unpatched field must survive");
    assert!(matches!(
        pro.source,
        CatalogSource::CloudFacts {
            facts_version: 7,
            ..
        }
    ));
    // New row materialized because it carried a context window.
    let new_row = find("fixture-new-model").expect("new row");
    assert_eq!(new_row.reasoning, Some(true));
    // Patch without context for a missing row is skipped.
    assert!(find("fixture-needs-context").is_none());
    // Deprecate annotates, never removes.
    let chat = find("deepseek-chat").expect("deprecated row stays");
    assert!(
        chat.reasoning_options
            .iter()
            .any(|v| v["cloud_facts"]["op"] == "deprecated")
    );
    // Hide removed the bundled row, but the provider-live row above layer 15
    // re-adds it: cloud can never hide a gateway's own live row.
    let reasoner = find("deepseek-reasoner").expect("provider-live row wins");
    assert!(matches!(reasoner.source, CatalogSource::Live { .. }));

    // Direct map application: hide on a higher-layer row is a receipt, not a removal.
    let mut rows: BTreeMap<(String, String), CatalogOffering> = BTreeMap::new();
    rows.insert(
        ("deepseek".into(), "deepseek-reasoner".into()),
        row("deepseek-reasoner", CatalogSource::UserOverride),
    );
    let skipped = apply_model_patches(&mut rows, &scoped, NOW);
    assert!(rows.contains_key(&("deepseek".into(), "deepseek-reasoner".into())));
    assert!(skipped.iter().any(|s| s.id == "deepseek-reasoner"));
    assert_eq!(scoped.models[0].op, ModelOp::Upsert);
}

/// The overlay is process-global: a test that configures it while another is
/// mid-sequence invalidates that test's ticket. Every overlay test holds this.
static OVERLAY_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn overlay_test_guard() -> std::sync::MutexGuard<'static, ()> {
    OVERLAY_TESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[test]
fn overlay_supplies_provider_defaults_only_from_the_scoped_view() {
    let _overlay = overlay_test_guard();
    let keys = [test_only_key(KeyStatus::Active)];
    let verified = verify(FIXTURE_V7.as_bytes(), &keys).unwrap();
    let scoped = scoped_view(&verified, &v("0.9.11"), NOW);

    overlay::clear();
    assert!(overlay::cloud_default_model("deepseek").is_none());
    let ticket = overlay::configure(true, "config-default-test").unwrap();
    assert!(overlay::publish(
        &ticket,
        Some(scoped),
        CloudFactsStatus::default()
    ));
    let (model, source) = overlay::cloud_default_model("deepseek").expect("cloud default");
    assert_eq!(model, "deepseek-v4-pro");
    assert_eq!(
        source,
        overlay::DefaultSource::CloudFacts { facts_version: 7 }
    );
    assert!(overlay::cloud_default_base_url("openai").is_none());
    overlay::clear();
    assert!(overlay::cloud_default_model("deepseek").is_none());
}

#[test]
fn status_labels_cover_every_state() {
    let now = NOW;
    let label = |state: CloudFactsState| {
        CloudFactsStatus {
            state,
            ..CloudFactsStatus::default()
        }
        .label(now)
    };
    assert_eq!(label(CloudFactsState::Off), "off (bundled)");
    assert!(label(CloudFactsState::Inert).contains("inert"));
    assert!(label(CloudFactsState::BundledOnly).contains("bundled"));
    let verified = label(CloudFactsState::Verified {
        channel: "stable".into(),
        facts_version: 42,
        key_id: "cwf-2026-08".into(),
        fetched_at: now - 12 * 60,
        origin: FactsOrigin::Network,
        stale: false,
        patches: 3,
        defaults: 1,
        announcements: 0,
    });
    assert_eq!(
        verified,
        "stable v42 · verified cwf-2026-08 · fetched 12m ago (network) · 3 patches, 1 default, 0 notices"
    );
    assert!(
        label(CloudFactsState::Rejected {
            reason: "bad signature".into(),
            at: now
        })
        .contains("bundled in use")
    );
    assert!(
        label(CloudFactsState::NotApplicable {
            applies_to: ">=1.0".into()
        })
        .contains(">=1.0")
    );
    assert!(
        label(CloudFactsState::Failed {
            last_error: "HTTP 503".into(),
            at: now - 3 * 86_400,
            keeping: Some(42)
        })
        .contains("keeping v42")
    );
}

#[test]
fn rfc3339_utc_parser_matches_known_epochs() {
    assert_eq!(parse_rfc3339_utc("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(
        parse_rfc3339_utc("2026-08-30T00:00:00Z"),
        Some(1_788_048_000)
    );
    assert_eq!(
        parse_rfc3339_utc("2026-08-30T00:00:00.123Z"),
        Some(1_788_048_000)
    );
    assert_eq!(parse_rfc3339_utc("2026-08-30T00:00:00+02:00"), None);
    assert_eq!(parse_rfc3339_utc("garbage"), None);
}

#[test]
fn pinned_keys_are_well_formed() {
    for key in super::TRUSTED_KEYS {
        assert!(key.key_id.starts_with("cwf-"));
        assert_ne!(key.public_key, [0u8; 32]);
    }
}

#[test]
fn signed_envelope_rejects_metadata_digest_future_and_malformed_times() {
    let signer = Signer::new("cwf-contract");
    let keys = [signer.trusted(KeyStatus::Active)];
    let valid = signer.envelope(&payload("stable", 5, "*"));
    for field in ["schema_version", "applies_to", "published_at", "sha256"] {
        let mut bad = valid.clone();
        bad.as_object_mut().unwrap().remove(field);
        assert!(
            verify(&serde_json::to_vec(&bad).unwrap(), &keys).is_err(),
            "{field}"
        );
    }
    for (field, value) in [
        ("published_at", json!("2026-08-31T00:00:00Z")),
        ("sha256", json!("0".repeat(64))),
    ] {
        let mut bad = valid.clone();
        bad[field] = value;
        assert!(matches!(
            verify(&serde_json::to_vec(&bad).unwrap(), &keys),
            Err(FactsRejection::Mismatch(_))
        ));
    }
    for (field, value) in [
        ("schema_version", json!(0)),
        ("facts_version", json!(0)),
        ("applies_to", json!("")),
        ("published_at", json!("2099-01-01T00:00:00Z")),
        ("not_after", json!("2026-02-30T00:00:00Z")),
        ("not_after", json!("2026-01-01T00:00:00Z")),
        ("not_after", json!("2026-08-30T00:00:00Z")),
    ] {
        let mut bad = payload("stable", 5, "*");
        bad[field] = value;
        assert!(
            verify(&serde_json::to_vec(&signer.envelope(&bad)).unwrap(), &keys).is_err(),
            "{field}"
        );
    }
    let mut excessive = valid.clone();
    excessive["sigs"] = json!(vec![
        json!({"key_id":"cwf-contract", "sig_b64":valid["sig_b64"]});
        8
    ]);
    assert!(matches!(
        verify(&serde_json::to_vec(&excessive).unwrap(), &keys),
        Err(FactsRejection::BadEnvelope(_))
    ));
    for invalid in [
        "2026-02-30T00:00:00Z",
        "2026-13-01T00:00:00Z",
        "2026-01-01T25:00:00Z",
    ] {
        assert!(parse_rfc3339_utc(invalid).is_none());
    }
}

#[test]
fn capability_patch_preserves_cost_authority_and_price_block_is_atomic() {
    use super::scope::ScopedFacts;
    use super::types::{ModelFact, PricingFact};
    use crate::models_dev::ModelsDevCost;
    let key = ("openai".to_string(), "fixture".to_string());
    let row = CatalogOffering {
        provider: key.0.clone(),
        wire_model_id: key.1.clone(),
        source: CatalogSource::ModelsDevLive { fetched_at: 123 },
        cost: Some(ModelsDevCost {
            input: Some(1.0),
            output: Some(2.0),
            cache_read: Some(0.1),
            cache_write: Some(3.0),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut rows = BTreeMap::from([(key.clone(), row)]);
    let mut facts = ScopedFacts {
        facts_version: 7,
        key_id: "cwf-test-only".into(),
        models: vec![ModelFact {
            provider: key.0.clone(),
            id: key.1.clone(),
            context_window: Some(9000),
            ..Default::default()
        }],
        ..Default::default()
    };
    apply_model_patches(&mut rows, &facts, NOW);
    assert!(matches!(
        rows[&key].source,
        CatalogSource::CloudFacts { .. }
    ));
    assert_eq!(
        rows[&key].pricing_source(),
        &CatalogSource::ModelsDevLive { fetched_at: 123 }
    );
    assert_eq!(rows[&key].cost.as_ref().unwrap().cache_write, Some(3.0));
    facts.models[0].pricing = Some(PricingFact {
        input_per_m: Some(4.0),
        ..Default::default()
    });
    apply_model_patches(&mut rows, &facts, NOW);
    assert!(matches!(
        rows[&key].pricing_source(),
        CatalogSource::CloudFacts { .. }
    ));
    assert_eq!(
        rows[&key].cost,
        Some(ModelsDevCost {
            input: Some(4.0),
            output: None,
            cache_read: None,
            cache_write: None,
            ..Default::default()
        })
    );
    facts.valid_until = Some(0);
    facts.models[0].pricing.as_mut().unwrap().input_per_m = Some(999.0);
    apply_model_patches(&mut rows, &facts, NOW);
    assert_eq!(rows[&key].cost.as_ref().unwrap().input, Some(4.0));
}

#[test]
fn overlay_tickets_disable_expiry_and_source_changes_keep_channel_rollback_floors() {
    let _overlay = overlay_test_guard();
    use super::scope::ScopedFacts;
    let facts = |channel: &str, version| ScopedFacts {
        channel: channel.into(),
        facts_version: version,
        ..Default::default()
    };
    let first = overlay::configure(true, "gate-test-first").unwrap();
    assert!(overlay::publish(
        &first,
        Some(facts("gate-stable", 7)),
        CloudFactsStatus::default()
    ));
    let generation = overlay::snapshot().generation;
    let second = overlay::configure(true, "gate-test-second").unwrap();
    assert!(overlay::snapshot().generation > generation);
    let mut wrote = false;
    assert!(!overlay::publish_with(
        &first,
        Some(facts("gate-stable", 8)),
        CloudFactsStatus::default(),
        || wrote = true
    ));
    assert!(!wrote);
    assert!(!overlay::publish(
        &second,
        Some(facts("gate-stable", 6)),
        CloudFactsStatus::default()
    ));
    assert!(overlay::publish(
        &second,
        Some(facts("gate-beta", 2)),
        CloudFactsStatus::default()
    ));
    overlay::clear();
    assert!(overlay::overlay().is_none());
    assert!(!overlay::is_current(&second));
    let third = overlay::configure(true, "gate-test-first").unwrap();
    assert_eq!(overlay::highest_seen("gate-stable"), Some(7));
    assert_eq!(overlay::highest_seen("gate-beta"), Some(2));
    assert!(!overlay::publish(
        &third,
        Some(facts("gate-stable", 6)),
        CloudFactsStatus::default()
    ));
    let mut expired = facts("gate-stable", 8);
    expired.valid_until = Some(0);
    assert!(overlay::publish(
        &third,
        Some(expired),
        CloudFactsStatus::default()
    ));
    let generation = overlay::snapshot().generation;
    assert!(overlay::overlay().is_none());
    assert_eq!(overlay::snapshot().generation, generation);
    assert_eq!(overlay::highest_seen("gate-stable"), Some(8));
    overlay::clear();
}

#[test]
fn cloud_endpoint_contract_rejects_dynamic_origins_and_prefix_expansion() {
    assert!(base_url_allowed(
        "codewhale",
        crate::DEFAULT_CODEWHALE_BASE_URL
    ));
    assert!(!base_url_allowed("codewhale", "https://private.example/v1"));
    assert!(base_url_allowed(
        "moonshot",
        "https://api.kimi.com/coding/v1"
    ));
    for url in [
        "https://api.kimi.com/coding/private",
        "https://api.kimi.com/coding/v1?key=fixture",
        "https://api.kimi.com/coding/v1#fixture",
        "https://fixture@api.kimi.com/coding/v1",
        "https://api.kimi.com/coding/v1\\private",
    ] {
        assert!(!base_url_allowed("moonshot", url), "{url}");
    }
    assert!(!base_url_allowed(
        "xiaomimimo",
        "https://api.xiaomimimo.com/v1/private"
    ));
}

#[test]
fn legacy_live_and_operator_layers_cannot_be_overwritten_by_cloud_patches() {
    use super::{ModelFact, PricingFact, ScopedFacts};
    let key = ("openai".to_string(), "layer-fixture".to_string());
    let facts = ScopedFacts {
        facts_version: 1,
        models: vec![ModelFact {
            provider: key.0.clone(),
            id: key.1.clone(),
            context_window: Some(9000),
            pricing: Some(PricingFact {
                input_per_m: Some(999.0),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    };
    for source in [
        CatalogSource::Live {
            base_url_fingerprint: "fixture".into(),
            fetched_at: NOW,
        },
        CatalogSource::ConfigOverride,
        CatalogSource::UserOverride,
    ] {
        let row = CatalogOffering {
            provider: key.0.clone(),
            wire_model_id: key.1.clone(),
            source,
            ..Default::default()
        };
        let mut rows = BTreeMap::from([(key.clone(), row.clone())]);
        assert_eq!(apply_model_patches(&mut rows, &facts, NOW).len(), 1);
        assert_eq!(rows[&key], row);
    }
    let row = CatalogOffering {
        provider: key.0.clone(),
        wire_model_id: key.1.clone(),
        source: CatalogSource::Live {
            base_url_fingerprint: "fixture".into(),
            fetched_at: NOW,
        },
        ..Default::default()
    };
    let snapshot = CatalogCompiler::new()
        .with_live(vec![row.clone()])
        .with_cloud_facts(&facts, NOW)
        .compile();
    assert_eq!(snapshot.offerings, vec![row]);
}

/// The complement of the layer test above: a correction must actually *reach* a
/// live Models.dev row, because that layer describes most of the models a user
/// sees. Both sides go through [`crate::catalog::CatalogCompiler::with_live`],
/// which routes a row by its own source, and the enriched rows come from the
/// real producer — so this fails if the refresh ever goes back to stamping
/// `Live`, which put every enriched row above the layer meant to correct it and
/// left a published rate change reaching nobody without a reinstall.
#[test]
fn signed_facts_correct_a_live_models_dev_row_but_never_the_provider_roster() {
    use super::{ModelFact, PricingFact, ScopedFacts};
    use crate::catalog::live_offerings_from_models_dev;
    use crate::models_dev::ModelsDevCatalog;

    let raw = r#"{
      "models": {},
      "providers": {
        "deepseek": {
          "id": "deepseek",
          "models": {
            "deepseek-chat": {
              "id": "deepseek-chat",
              "modalities": { "input": ["text"], "output": ["text"] },
              "limit": { "context": 65536, "output": 4096 },
              "cost": { "input": 2.0, "output": 8.0 }
            }
          }
        }
      }
    }"#;
    let catalog = ModelsDevCatalog::parse_json(raw).expect("fixture parses");
    let enriched = live_offerings_from_models_dev(&catalog, NOW);
    assert_eq!(enriched.len(), 1, "one text-chat row");

    let facts = ScopedFacts {
        facts_version: 3,
        key_id: "cwf-test-only".into(),
        models: vec![ModelFact {
            provider: "deepseek".into(),
            id: "deepseek-chat".into(),
            context_window: Some(131_072),
            pricing: Some(PricingFact {
                input_per_m: Some(0.5),
                output_per_m: Some(1.5),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    };

    let corrected = CatalogCompiler::new()
        .with_live(enriched.clone())
        .with_cloud_facts(&facts, NOW)
        .compile();
    let row = corrected
        .offerings
        .iter()
        .find(|row| row.wire_model_id == "deepseek-chat")
        .expect("the row survives its own correction");
    let limit = row.limit.as_ref().expect("limits are kept");
    assert_eq!(
        limit.context,
        Some(131_072),
        "the stale window is corrected"
    );
    assert_eq!(
        limit.output,
        Some(4_096),
        "and only what the payload states is replaced"
    );
    assert_eq!(row.cost.as_ref().and_then(|cost| cost.input), Some(0.5));
    assert_eq!(row.cost.as_ref().and_then(|cost| cost.output), Some(1.5));
    assert!(matches!(
        row.pricing_source(),
        CatalogSource::CloudFacts { .. }
    ));

    // The same payload, same wire, once the provider's own roster owns the id:
    // an authenticated endpoint outranks a signed correction, field for field.
    let roster = CatalogOffering {
        source: CatalogSource::Live {
            base_url_fingerprint: "deepseek-endpoint".into(),
            fetched_at: NOW,
        },
        ..enriched[0].clone()
    };
    let untouched = CatalogCompiler::new()
        .with_live(vec![roster.clone()])
        .with_cloud_facts(&facts, NOW)
        .compile();
    assert_eq!(untouched.offerings, vec![roster]);
}

#[test]
fn unsigned_metadata_cannot_amplify_rejection_messages() {
    let signer = Signer::new("cwf-bounds");
    let keys = [signer.trusted(KeyStatus::Active)];
    for field in ["alg", "channel", "applies_to", "published_at", "sha256"] {
        let mut envelope = signer.envelope(&payload("stable", 1, "*"));
        envelope[field] = json!("x".repeat(16_384));
        let error = verify(&serde_json::to_vec(&envelope).unwrap(), &keys).unwrap_err();
        assert!(error.to_string().len() < 256, "{field}");
    }
}

/// The unlisted assertion is the only thing that may override a provider
/// roster's own omission, so the scoped view is where it is bounded: an
/// `upsert` in a payload that expires. The rest of the patch survives either
/// way — only the assertion is withdrawn, with a receipt.
#[test]
fn unlisted_assertion_needs_an_upsert_in_an_expiring_payload() {
    let signer = Signer::new("cwf-unlisted");
    let keys = [signer.trusted(KeyStatus::Active)];
    let models = json!([
        {"provider": "deepseek", "id": "preview-a", "allow_unlisted": true},
        {"provider": "deepseek", "id": "preview-b", "op": "hide", "allow_unlisted": true},
        {"provider": "deepseek", "id": "listed-c", "context_window": 1000},
    ]);

    let mut bounded = payload("stable", 11, "*");
    bounded["models"] = models.clone();
    bounded["not_after"] = json!("2026-12-31T00:00:00Z");
    let verified = verify(
        &serde_json::to_vec(&signer.envelope(&bounded)).unwrap(),
        &keys,
    )
    .unwrap();
    let scoped = scoped_view(&verified, &v("0.9.11"), NOW);
    assert!(scoped.valid_until.is_some());
    assert_eq!(scoped.models.len(), 3, "patches themselves are not dropped");
    assert!(super::catalog_patch::is_unlisted_attested(
        &scoped,
        "deepseek",
        "preview-a"
    ));
    assert!(
        !super::catalog_patch::is_unlisted_attested(&scoped, "deepseek", "preview-b"),
        "a hide cannot carry an existence assertion"
    );
    assert!(!super::catalog_patch::is_unlisted_attested(
        &scoped, "deepseek", "listed-c"
    ));
    assert!(
        !super::catalog_patch::is_unlisted_attested(&scoped, "deepseek", "preview-a-x"),
        "the assertion names one exact id, never a prefix"
    );

    let mut unbounded = payload("stable", 12, "*");
    unbounded["models"] = models;
    let verified = verify(
        &serde_json::to_vec(&signer.envelope(&unbounded)).unwrap(),
        &keys,
    )
    .unwrap();
    let scoped = scoped_view(&verified, &v("0.9.11"), NOW);
    assert!(scoped.valid_until.is_none());
    assert_eq!(scoped.models.len(), 3);
    assert!(
        !super::catalog_patch::is_unlisted_attested(&scoped, "deepseek", "preview-a"),
        "an assertion that cannot expire is not honored"
    );
    assert_eq!(
        scoped
            .dropped
            .iter()
            .filter(|receipt| receipt.contains("allow_unlisted"))
            .count(),
        2
    );
}

/// An attested id needs no invented context window to be listed, and gets no
/// invented anything else either.
#[test]
fn attested_id_only_upsert_creates_a_row_with_everything_unknown() {
    use super::{ModelFact, ScopedFacts};
    let bare = |allow_unlisted| ModelFact {
        provider: "deepseek".into(),
        id: "preview-a".into(),
        allow_unlisted,
        ..Default::default()
    };
    let key = ("deepseek".to_string(), "preview-a".to_string());

    let mut rows = BTreeMap::new();
    let skipped = apply_model_patches(
        &mut rows,
        &ScopedFacts {
            models: vec![bare(false)],
            ..Default::default()
        },
        NOW,
    );
    assert!(rows.is_empty(), "an id alone still creates nothing");
    assert_eq!(skipped.len(), 1);
    assert!(
        skipped[0]
            .reason
            .contains("context_window or allow_unlisted")
    );

    let mut rows = BTreeMap::new();
    assert!(
        apply_model_patches(
            &mut rows,
            &ScopedFacts {
                facts_version: 3,
                key_id: "cwf-unlisted".into(),
                valid_until: Some(crate::catalog::now_unix() + 3600),
                models: vec![bare(true)],
                ..Default::default()
            },
            NOW,
        )
        .is_empty()
    );
    let row = &rows[&key];
    assert_eq!(row.wire_model_id, "preview-a");
    assert_eq!(row.limit, None, "no limits were stated, so none are known");
    assert_eq!(row.cost, None);
    assert_eq!(row.cost_source, None);
    assert_eq!(row.reasoning, None);
    assert_eq!(row.tool_call, None);
    assert_eq!(row.modalities, None);
    assert_eq!(row.attachment, None);
    assert!(matches!(row.source, CatalogSource::CloudFacts { .. }));
}

/// A roster that answers with ids alone has said nothing about limits — not
/// that they are unknown. Completion fills only that silence.
#[test]
fn provider_live_rows_are_completed_only_where_the_provider_is_silent() {
    use super::catalog_patch::complete_provider_live_row;
    use super::{ModelFact, PricingFact, ScopedFacts};
    use crate::models_dev::{ModelsDevCost, ModelsDevLimit};
    let live = CatalogSource::Live {
        base_url_fingerprint: "fixture".into(),
        fetched_at: NOW,
    };
    let facts = ScopedFacts {
        facts_version: 4,
        key_id: "cwf-unlisted".into(),
        models: vec![ModelFact {
            provider: "deepseek".into(),
            id: "roster-model".into(),
            context_window: Some(131_072),
            max_output: Some(8_192),
            reasoning: Some(true),
            pricing: Some(PricingFact {
                input_per_m: Some(1.0),
                output_per_m: Some(2.0),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    };
    let id_only = CatalogOffering {
        provider: "deepseek".into(),
        wire_model_id: "roster-model".into(),
        source: live.clone(),
        ..Default::default()
    };

    let mut row = id_only.clone();
    assert!(complete_provider_live_row(&mut row, &facts));
    assert_eq!(row.limit.as_ref().unwrap().context, Some(131_072));
    assert_eq!(row.limit.as_ref().unwrap().output, Some(8_192));
    assert_eq!(row.reasoning, Some(true));
    assert_eq!(row.source, live, "the row is still the provider's");
    assert_eq!(row.cost, None, "a signed price is not billable here");
    assert_eq!(row.cost_source, None);

    // Anything the provider actually stated wins, field by field.
    let mut row = CatalogOffering {
        limit: Some(ModelsDevLimit {
            context: Some(64_000),
            ..Default::default()
        }),
        reasoning: Some(false),
        cost: Some(ModelsDevCost {
            input: Some(9.0),
            ..Default::default()
        }),
        ..id_only.clone()
    };
    assert!(complete_provider_live_row(&mut row, &facts));
    assert_eq!(row.limit.as_ref().unwrap().context, Some(64_000));
    assert_eq!(row.limit.as_ref().unwrap().output, Some(8_192));
    assert_eq!(row.reasoning, Some(false));
    assert_eq!(row.cost.as_ref().unwrap().input, Some(9.0));

    // Only provider-live rows are completed, and only by current facts.
    for source in [
        CatalogSource::Bundled,
        CatalogSource::ConfigOverride,
        CatalogSource::UserOverride,
        CatalogSource::ModelsDevLive { fetched_at: NOW },
    ] {
        let mut row = CatalogOffering {
            source,
            ..id_only.clone()
        };
        let before = row.clone();
        assert!(!complete_provider_live_row(&mut row, &facts));
        assert_eq!(row, before);
    }
    let mut row = id_only.clone();
    let expired = ScopedFacts {
        valid_until: Some(0),
        ..facts.clone()
    };
    assert!(!complete_provider_live_row(&mut row, &expired));
    assert_eq!(row, id_only);
}

/// Producer/consumer parity for the additive field: an older payload without it
/// keeps roster dominance, and a newer one round-trips through the same wire
/// shape the publisher signs.
#[test]
fn allow_unlisted_defaults_to_false_and_round_trips() {
    use super::ModelFact;
    let without: ModelFact =
        serde_json::from_value(json!({"provider": "deepseek", "id": "preview-a"})).unwrap();
    assert!(!without.allow_unlisted);
    assert_eq!(
        serde_json::to_value(&without)
            .unwrap()
            .get("allow_unlisted"),
        None,
        "an unset assertion must not appear in the signed bytes"
    );
    let with: ModelFact = serde_json::from_value(
        json!({"provider": "deepseek", "id": "preview-a", "allow_unlisted": true}),
    )
    .unwrap();
    assert!(with.allow_unlisted);
    assert_eq!(
        serde_json::to_value(&with).unwrap()["allow_unlisted"],
        json!(true)
    );
}

#[test]
fn cloud_route_identity_preserves_legacy_wire_endpoint_boundaries() {
    let chat = crate::default_base_url_for_provider(crate::ProviderKind::Minimax);
    let anthropic = crate::default_base_url_for_provider(crate::ProviderKind::MinimaxAnthropic);
    assert!(base_url_allowed("minimax", chat));
    assert!(base_url_allowed("minimax-anthropic", anthropic));
    assert!(!base_url_allowed("minimax-anthropic", chat));
    assert!(!base_url_allowed("minimax", anthropic));
    assert!(
        !base_url_allowed("minimax_anthropic", anthropic),
        "signed identities must use their exact canonical spelling"
    );
    let signer = Signer::new("cwf-wire-identity");
    let mut p = payload("stable", 1, "*");
    p["models"] =
        json!([{"provider":"minimax-anthropic", "id":"MiniMax-M3", "context_window":1000}]);
    p["provider_defaults"] =
        json!({"minimax-anthropic":{"default_model":"MiniMax-M3", "base_url":anthropic}});
    let verified = verify(
        &serde_json::to_vec(&signer.envelope(&p)).unwrap(),
        &[signer.trusted(KeyStatus::Active)],
    )
    .unwrap();
    let scoped = scoped_view(&verified, &v("0.9.11"), NOW);
    assert_eq!(scoped.models[0].provider, "minimax-anthropic");
    assert_eq!(
        scoped.provider_defaults["minimax-anthropic"]
            .base_url
            .as_deref(),
        Some(anthropic)
    );
    assert!(!scoped.provider_defaults.contains_key("minimax"));
}

const AUTOMATION_CASES: &str =
    include_str!("../../../../docs/cloud-facts/fixtures/automation-scope-cases.json");

fn automation_cases() -> (u64, Vec<Value>) {
    let fixture: Value = serde_json::from_str(AUTOMATION_CASES).expect("fixture json");
    (
        fixture["now"].as_u64().expect("now"),
        fixture["cases"].as_array().expect("cases").clone(),
    )
}

#[test]
fn automation_scope_cases_are_judged_as_the_shared_fixture_says() {
    let signer = Signer::new("cwf-auto");
    let (now, cases) = automation_cases();
    assert_eq!(cases.len(), 25);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let channel = case["channel"].as_str().unwrap();
        let bytes = serde_json::to_vec(&signer.envelope(&case["payload"])).unwrap();
        let check =
            |keys: &[TrustedKey]| verify_envelope(&bytes, channel, &v("0.9.11"), None, keys, now);
        match (case["out_of_scope"].as_str(), check(&[signer.automation()])) {
            (None, Ok(verified)) => {
                assert_eq!(verified.key_id, "cwf-auto", "{name}");
                assert!(!verified.stale, "{name}");
            }
            (Some(expected), Err(FactsRejection::OutOfScope(reason))) => {
                assert!(reason.contains(expected), "{name}: got {reason}");
            }
            (expected, other) => panic!("{name}: expected {expected:?}, got {other:?}"),
        }
        assert!(
            check(&[signer.trusted(KeyStatus::Active)]).is_ok(),
            "{name}: a founder key is not bound by the automation scope"
        );
    }
}

#[test]
fn a_founder_signature_lifts_the_automation_scope_in_either_order() {
    let auto = Signer::new("cwf-auto");
    let founder = Signer::new("cwf-founder");
    let (now, cases) = automation_cases();
    let case = cases
        .iter()
        .find(|case| case["name"] == "default model")
        .unwrap();
    let bytes = serde_json::to_vec(&case["payload"]).unwrap();
    let keys = [auto.automation(), founder.trusted(KeyStatus::Active)];
    let check = |env: &Value| {
        verify_envelope(
            &serde_json::to_vec(env).unwrap(),
            "stable-models",
            &v("0.9.11"),
            None,
            &keys,
            now,
        )
    };

    let mut auto_first = auto.envelope(&case["payload"]);
    auto_first["sigs"] = json!([{ "key_id": "cwf-founder", "sig_b64": founder.sign(&bytes) }]);
    assert_eq!(check(&auto_first).unwrap().key_id, "cwf-founder");

    let mut founder_first = founder.envelope(&case["payload"]);
    founder_first["sigs"] = json!([{ "key_id": "cwf-auto", "sig_b64": auto.sign(&bytes) }]);
    assert_eq!(check(&founder_first).unwrap().key_id, "cwf-founder");

    let mut forged = auto.envelope(&case["payload"]);
    forged["sigs"] = json!([{ "key_id": "cwf-founder", "sig_b64": auto.sign(&bytes) }]);
    assert!(matches!(check(&forged), Err(FactsRejection::OutOfScope(_))));

    let retired = [auto.automation(), founder.trusted(KeyStatus::Retired)];
    assert!(matches!(
        verify_envelope(
            &serde_json::to_vec(&auto_first).unwrap(),
            "stable-models",
            &v("0.9.11"),
            None,
            &retired,
            now
        ),
        Err(FactsRejection::OutOfScope(_))
    ));
}

#[test]
fn an_automation_key_cannot_raise_the_founder_channel_floor() {
    let auto = Signer::new("cwf-auto");
    let (now, cases) = automation_cases();
    let mut payload = cases[0]["payload"].clone();
    payload["channel"] = json!("stable");
    let bytes = serde_json::to_vec(&auto.envelope(&payload)).unwrap();
    let rejected = verify_envelope(
        &bytes,
        "stable",
        &v("0.9.11"),
        None,
        &[auto.automation()],
        now,
    );
    assert!(
        matches!(rejected, Err(FactsRejection::OutOfScope(reason)) if reason.contains("channel"))
    );
}

#[test]
fn automation_limits_and_the_empty_production_roster() {
    assert_eq!(AUTOMATION_MAX_VALIDITY_SECS, 604_800);
    assert_eq!(super::AUTOMATION_CHANNEL_SUFFIX, "-models");
    assert_eq!(super::AUTOMATION_MAX_MODELS, 256);
    assert!(
        super::TRUSTED_KEYS
            .iter()
            .all(|key| key.role == KeyRole::Founder)
    );
    let auto = Signer::new("cwf-auto");
    let dup = TrustedKey {
        key_id: "cwf-founder-alias",
        role: KeyRole::Founder,
        ..auto.automation()
    };
    let (now, cases) = automation_cases();
    let bytes = serde_json::to_vec(&auto.envelope(&cases[0]["payload"])).unwrap();
    assert!(matches!(
        verify_envelope(
            &bytes,
            "stable-models",
            &v("0.9.11"),
            None,
            &[auto.automation(), dup],
            now
        ),
        Err(FactsRejection::UnknownKey { .. })
    ));
}
