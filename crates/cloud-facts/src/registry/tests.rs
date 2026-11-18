use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use codewhale_config::cloud_facts::pulled_registry;
use codewhale_config::cloud_facts::registry::REGISTRY_CHANNEL;
use codewhale_config::cloud_facts::verify::signing_message;
use codewhale_config::cloud_facts::{KeyRole, KeyStatus};
use ring::signature::KeyPair as _;
use serde_json::json;

use super::*;
use crate::tests::{TestEnv, lock};

struct Signer {
    pair: ring::signature::Ed25519KeyPair,
}

impl Signer {
    fn new() -> Self {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).expect("pkcs8");
        Self {
            pair: ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("pair"),
        }
    }

    fn keys(&self) -> Vec<TrustedKey> {
        let mut public_key = [0u8; 32];
        public_key.copy_from_slice(self.pair.public_key().as_ref());
        vec![TrustedKey {
            key_id: "cwf-reg-client",
            public_key,
            status: KeyStatus::Active,
            role: KeyRole::Founder,
        }]
    }

    fn envelope(&self, revision: u64) -> Vec<u8> {
        let payload = serde_json::to_vec(&json!({
            "schema": REGISTRY_SCHEMA,
            "revision": revision,
            "generated_at": "2026-11-17T00:00:00Z",
            "providers": [{
                "id": "acme",
                "wire_policy": "openai-compatible",
                "base_url": "https://api.acme.example/v1",
            }],
        }))
        .expect("payload");
        use sha2::Digest as _;
        let sha = sha2::Sha256::digest(&payload)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        serde_json::to_vec(&json!({
            "envelope": 1,
            "channel": REGISTRY_CHANNEL,
            "facts_version": revision,
            "key_id": "cwf-reg-client",
            "alg": "ed25519",
            "published_at": "2026-11-17T00:00:00Z",
            "payload_b64": BASE64.encode(&payload),
            "sig_b64": BASE64.encode(self.pair.sign(&signing_message("cwf-reg-client", &payload))),
            "sha256": sha,
        }))
        .expect("envelope")
    }
}

fn settings(dir: &tempfile::TempDir) -> RegistrySettings {
    RegistrySettings {
        enabled: true,
        url: Some("https://registry.example/api/facts/v1/registry".into()),
        local_path: None,
        cache_path: Some(dir.path().join(REGISTRY_CACHE_FILE)),
    }
}

fn base() -> u64 {
    overlay::highest_seen(REGISTRY_SCHEMA).unwrap_or(0)
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

fn body(
    bytes: Vec<u8>,
) -> impl FnOnce(String, Option<String>, usize) -> std::future::Ready<Result<Fetched, RefreshError>>
{
    move |_, _, _| std::future::ready(Ok(Fetched::Body { bytes, etag: None }))
}

#[test]
fn pull_installs_caches_and_refuses_a_downgrade_or_a_failed_fetch_without_losing_the_copy() {
    let _guard = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let signer = Signer::new();
    let keys = signer.keys();
    let settings = settings(&dir);
    overlay::configure(false, "");
    let ticket = overlay::configure(true, "registry-client-test").expect("ticket");
    let rev = base() + 7;

    let pulled = rt().block_on(refresh_registry_using(
        &settings,
        &ticket,
        &keys,
        false,
        body(signer.envelope(rev)),
    ));
    assert_eq!(pulled, Ok(Some(rev)));
    let installed = pulled_registry().expect("installed");
    assert_eq!(installed.providers[0].id, "acme");

    let older = rt().block_on(refresh_registry_using(
        &settings,
        &ticket,
        &keys,
        false,
        body(signer.envelope(rev - 1)),
    ));
    assert!(matches!(
        older,
        Err(RefreshError::Rejected(FactsRejection::Rollback { got, .. })) if got == rev - 1
    ));
    let failed = rt().block_on(refresh_registry_using(
        &settings,
        &ticket,
        &keys,
        false,
        |_, _, _| std::future::ready(Err(RefreshError::Network("offline".into()))),
    ));
    assert!(failed.is_err());
    assert_eq!(pulled_registry().expect("kept").revision, rev);

    overlay::configure(false, "");
    assert!(pulled_registry().is_none());
    let ticket = overlay::configure(true, "registry-client-test").expect("ticket");
    assert_eq!(load_cached_registry(&settings, &ticket, &keys), Some(rev));
    assert_eq!(pulled_registry().expect("from cache").revision, rev);

    let moved = RegistrySettings {
        url: Some("https://other.example/api/facts/v1/registry".into()),
        ..settings.clone()
    };
    assert_eq!(load_cached_registry(&moved, &ticket, &keys), None);
    assert!(pulled_registry().is_none());

    let other = Signer::new();
    assert_eq!(
        load_cached_registry(&settings, &ticket, &other.keys()),
        None
    );
    assert!(pulled_registry().is_none());
    let cache = load_cache(settings.cache_path.as_deref());
    assert!(cache.envelope.is_empty());
    assert_eq!(cache.highest_revision, Some(rev));
    overlay::configure(false, "");
}

#[test]
fn a_refresh_that_verified_against_a_stale_floor_cannot_replace_a_newer_registry() {
    let _guard = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let signer = Signer::new();
    let keys = signer.keys();
    let settings = settings(&dir);
    overlay::configure(false, "");
    let ticket = overlay::configure(true, "registry-race-test").expect("ticket");
    let rev = base() + 5;

    let racing = rt().block_on(refresh_registry_using(
        &settings,
        &ticket,
        &keys,
        false,
        |_, _, _| {
            let (newer_settings, newer_ticket, newer_keys) =
                (settings.clone(), ticket.clone(), keys.clone());
            let newer = signer.envelope(rev + 1);
            std::thread::spawn(move || {
                rt().block_on(refresh_registry_using(
                    &newer_settings,
                    &newer_ticket,
                    &newer_keys,
                    false,
                    body(newer),
                ))
            })
            .join()
            .expect("newer refresh")
            .expect("newer installed");
            std::future::ready(Ok(Fetched::Body {
                bytes: signer.envelope(rev),
                etag: None,
            }))
        },
    ));
    assert_eq!(racing, Err(RefreshError::Superseded));
    assert_eq!(pulled_registry().expect("newer kept").revision, rev + 1);
    assert_eq!(
        load_cache(settings.cache_path.as_deref()).highest_revision,
        Some(rev + 1)
    );
    overlay::configure(false, "");
}

#[test]
fn registry_uses_default_https_endpoint_stays_off_by_default_and_clears_on_disable() {
    let _guard = lock();
    let _url = TestEnv::set(ENV_REGISTRY_URL, "");
    let _path = TestEnv::set(ENV_REGISTRY_PATH, "");
    let _facts_url = TestEnv::set(crate::ENV_URL, "");
    let _disable = TestEnv::set(crate::ENV_DISABLE, "");
    let dir = tempfile::tempdir().expect("tempdir");

    let off = RegistrySettings::from_facts(&Settings::default());
    assert!(!off.enabled);
    assert!(!off.active());

    let facts = Settings {
        enabled: true,
        cache_path: Some(dir.path().join("cloud-facts.json")),
        ..Settings::default()
    };
    let derived = RegistrySettings::from_facts(&facts);
    assert!(derived.active());
    assert_eq!(
        derived.url.as_deref(),
        Some("https://codewhale.net/api/facts/v1/registry")
    );
    assert_eq!(
        derived.cache_path,
        Some(dir.path().join(REGISTRY_CACHE_FILE))
    );

    let templated = RegistrySettings::from_facts(&Settings {
        enabled: true,
        url: Some("https://staging.codewhale.example/api/facts/v1/{channel}".into()),
        ..Settings::default()
    });
    assert_eq!(
        templated.url.as_deref(),
        Some("https://staging.codewhale.example/api/facts/v1/registry")
    );

    let fixed_channel = RegistrySettings::from_facts(&Settings {
        enabled: true,
        url: Some("https://staging.codewhale.example/api/facts/v1/stable".into()),
        ..Settings::default()
    });
    assert_eq!(
        fixed_channel.url.as_deref(),
        Some("https://codewhale.net/api/facts/v1/registry")
    );

    let ticket = overlay::configure(true, "registry-inert-test").expect("ticket");
    assert_eq!(
        rt().block_on(refresh_registry_using(
            &off,
            &ticket,
            &Signer::new().keys(),
            false,
            |_, _, _| std::future::ready(Ok(Fetched::NotFound)),
        )),
        Err(RefreshError::Disabled)
    );
    assert_eq!(
        rt().block_on(refresh_registry_using(
            &derived,
            &ticket,
            &Signer::new().keys(),
            true,
            |_, _, _| std::future::ready(Ok(Fetched::NotFound)),
        )),
        Err(RefreshError::Suppressed)
    );

    let signer = Signer::new();
    let active = settings(&dir);
    let rev = base() + 3;
    rt().block_on(refresh_registry_using(
        &active,
        &ticket,
        &signer.keys(),
        false,
        body(signer.envelope(rev)),
    ))
    .expect("pulled");
    assert!(pulled_registry().is_some());

    {
        let _kill = TestEnv::set(crate::ENV_DISABLE, "1");
        assert!(!RegistrySettings::from_facts(&facts).active());
        assert!(pulled_registry().is_none());
    }

    let ticket = overlay::configure(true, "registry-inert-test").expect("ticket");
    rt().block_on(refresh_registry_using(
        &active,
        &ticket,
        &signer.keys(),
        false,
        body(signer.envelope(rev + 1)),
    ))
    .expect("pulled after re-enable");
    assert!(pulled_registry().is_some());
    crate::configure(&Settings::default());
    assert!(pulled_registry().is_none());
    assert_eq!(
        rt().block_on(refresh_registry_using(
            &active,
            &ticket,
            &signer.keys(),
            false,
            body(signer.envelope(rev + 2)),
        )),
        Err(RefreshError::Superseded)
    );
    assert!(pulled_registry().is_none());

    let ticket = overlay::configure(true, "registry-inert-test").expect("ticket");
    rt().block_on(refresh_registry_using(
        &active,
        &ticket,
        &signer.keys(),
        false,
        body(signer.envelope(rev + 3)),
    ))
    .expect("pulled again");
    crate::configure(&Settings {
        enabled: true,
        url: Some("ftp://facts.example/{channel}".into()),
        ..Settings::default()
    });
    assert!(pulled_registry().is_none());
}
