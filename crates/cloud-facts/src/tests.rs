use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use codewhale_config::cloud_facts::{CloudFactsState, KeyStatus, TrustedKey, overlay};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

use super::*;

// All tests that read or mutate the process overlay/environment hold `lock()`.
pub(crate) struct TestEnv {
    name: &'static str,
    previous: Option<std::ffi::OsString>,
}
impl TestEnv {
    pub(crate) fn set(name: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(name);
        // SAFETY: the test module serializes environment-dependent work.
        unsafe { std::env::set_var(name, value) };
        Self { name, previous }
    }
}
impl Drop for TestEnv {
    fn drop(&mut self) {
        // SAFETY: the test module serializes environment-dependent work.
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }
}

/// Cross-language fixture signed with the TEST-ONLY key.
const FIXTURE_V7: &str = include_str!("../../../docs/cloud-facts/fixtures/envelope-stable-v7.json");
const FIXTURE_FUTURE_V8: &str =
    include_str!("../../../docs/cloud-facts/fixtures/envelope-future-only-v8.json");

fn test_keys() -> &'static [TrustedKey] {
    static KEYS: OnceLock<Vec<TrustedKey>> = OnceLock::new();
    KEYS.get_or_init(|| {
        vec![TrustedKey {
            key_id: "cwf-test-only",
            public_key: [
                243, 225, 75, 13, 110, 14, 162, 181, 4, 77, 69, 100, 179, 72, 105, 64, 8, 185, 46,
                62, 48, 131, 121, 35, 42, 55, 216, 23, 50, 219, 39, 181,
            ],
            status: KeyStatus::Active,
            role: codewhale_config::cloud_facts::KeyRole::Founder,
        }]
    })
}

/// The overlay/status are process-wide; serialize tests that touch them.
pub(crate) fn lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

fn settings(dir: &tempfile::TempDir, url: Option<String>) -> Settings {
    Settings {
        enabled: true,
        channel: "stable".into(),
        url,
        ttl_secs: 3600,
        cache_path: Some(dir.path().join("facts").join(CACHE_FILE)),
        local_path: None,
    }
}

// Explicit fixture transport: CI still blocks the production refresh path.
fn refresh_fixture<'a>(
    settings: &'a Settings,
    force: bool,
    keys: &'a [TrustedKey],
) -> impl std::future::Future<Output = Result<RefreshOutcome, RefreshError>> + 'a {
    configure_with_keys(settings, keys);
    refresh_using(settings, force, keys, None, false, fetch)
}

/// One canned HTTP response per connection; records the request line/headers.
struct MockServer {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

/// `(status, headers, body)` canned HTTP response.
type CannedResponse = (u16, Vec<(&'static str, String)>, String);

async fn mock_server(responses: Vec<CannedResponse>) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    tokio::spawn(async move {
        let mut responses = responses.into_iter();
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).into_owned();
            seen.lock().unwrap().push(head);
            let (status, headers, body) =
                responses
                    .next()
                    .unwrap_or((500, vec![], "no more canned responses".into()));
            let reason = match status {
                200 => "OK",
                304 => "Not Modified",
                404 => "Not Found",
                _ => "Error",
            };
            let mut out = format!("HTTP/1.1 {status} {reason}\r\nConnection: close\r\n");
            for (k, v) in headers {
                out.push_str(&format!("{k}: {v}\r\n"));
            }
            out.push_str(&format!("Content-Length: {}\r\n\r\n{}", body.len(), body));
            let _ = stream.write_all(out.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    MockServer {
        url: format!("http://{addr}/api/facts/v1/{{channel}}"),
        requests,
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

#[test]
fn flag_off_means_no_client_no_file_and_off_status() {
    let _lock = lock();
    overlay::clear();
    let dir = tempfile::tempdir().unwrap();
    let mut s = settings(&dir, Some("http://127.0.0.1:1/{channel}".into()));
    s.enabled = false;
    configure_with_keys(&s, test_keys());
    assert_eq!(maybe_load_persisted_cache_with_keys(&s, test_keys()), None);
    assert_eq!(status().state, CloudFactsState::Off);
    let err = rt()
        .block_on(refresh_fixture(&s, true, test_keys()))
        .unwrap_err();
    assert_eq!(err, RefreshError::Disabled);
    assert!(
        !dir.path().join("facts").exists(),
        "flag off must write nothing"
    );
    assert!(overlay::overlay().is_none());
}

#[test]
fn enabled_with_no_active_key_is_inert_and_never_fetches() {
    let _lock = lock();
    overlay::clear();
    let dir = tempfile::tempdir().unwrap();
    let s = settings(&dir, Some("http://127.0.0.1:1/{channel}".into()));
    configure_with_keys(&s, &[]);
    assert_eq!(maybe_load_persisted_cache_with_keys(&s, &[]), None);
    assert_eq!(status().state, CloudFactsState::Inert);
    let err = rt().block_on(refresh_fixture(&s, true, &[])).unwrap_err();
    assert_eq!(err, RefreshError::Inert);
    assert!(!dir.path().join("facts").exists());
}

#[test]
fn network_200_verifies_installs_caches_and_304_keeps_it() {
    let _lock = lock();
    overlay::clear();
    let rt = rt();
    let server = rt.block_on(mock_server(vec![
        (
            200,
            vec![
                ("ETag", "\"stable-v7-abc\"".into()),
                ("Content-Type", "application/json".into()),
            ],
            FIXTURE_V7.into(),
        ),
        (
            304,
            vec![("ETag", "\"stable-v7-abc\"".into())],
            String::new(),
        ),
    ]));
    let dir = tempfile::tempdir().unwrap();
    let s = settings(&dir, Some(server.url.clone()));

    let outcome = rt.block_on(refresh_fixture(&s, true, test_keys())).unwrap();
    assert_eq!(outcome, RefreshOutcome::Updated { facts_version: 7 });
    let st = status();
    assert!(
        matches!(
            st.state,
            CloudFactsState::Verified {
                facts_version: 7,
                origin: FactsOrigin::Network,
                patches: 5,
                defaults: 1,
                announcements: 1,
                ..
            }
        ),
        "{st:?}"
    );
    assert_eq!(st.etag.as_deref(), Some("\"stable-v7-abc\""));
    let overlay = overlay::overlay().expect("overlay installed");
    assert_eq!(overlay.facts_version, 7);
    assert_eq!(
        overlay::cloud_default_model("deepseek")
            .map(|(m, _)| m)
            .as_deref(),
        Some("deepseek-v4-pro")
    );

    // Cache file exists, is secret-free, and carries the envelope + etag.
    let cache = std::fs::read_to_string(s.cache_path.as_ref().unwrap()).unwrap();
    assert!(cache.contains("stable-v7-abc"));
    assert!(cache.contains("cwf-test-only"));
    for needle in ["api_key", "authorization", "bearer", "password"] {
        assert!(!cache.to_lowercase().contains(&format!("\"{needle}\"")));
    }

    // Second fetch sends If-None-Match and keeps the overlay on 304.
    let outcome = rt.block_on(refresh_fixture(&s, true, test_keys())).unwrap();
    assert_eq!(
        outcome,
        RefreshOutcome::NotModified {
            facts_version: Some(7)
        }
    );
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .to_lowercase()
            .contains("if-none-match: \"stable-v7-abc\""),
        "{}",
        requests[1]
    );
    for req in requests.iter() {
        assert!(
            req.contains(&format!("User-Agent: {USER_AGENT}"))
                || req.to_lowercase().contains("user-agent: codewhale/")
        );
        assert!(!req.to_lowercase().contains("cookie"));
        assert!(
            req.lines()
                .next()
                .unwrap()
                .contains("/api/facts/v1/stable HTTP/1.1"),
            "{}",
            req.lines().next().unwrap()
        );
    }
    assert!(matches!(
        status().state,
        CloudFactsState::Verified {
            facts_version: 7,
            ..
        }
    ));
    overlay::clear();
}

#[test]
fn persisted_cache_round_trips_and_a_tampered_cache_is_rejected_and_cleared() {
    let _lock = lock();
    overlay::clear();
    let rt = rt();
    let server = rt.block_on(mock_server(vec![(200, vec![], FIXTURE_V7.into())]));
    let dir = tempfile::tempdir().unwrap();
    let s = settings(&dir, Some(server.url.clone()));
    rt.block_on(refresh_fixture(&s, true, test_keys())).unwrap();
    overlay::clear();

    // A newly admitted startup seeds the overlay without a network call.
    configure_with_keys(&s, test_keys());
    assert_eq!(
        maybe_load_persisted_cache_with_keys(&s, test_keys()),
        Some(7)
    );
    assert!(matches!(
        status().state,
        CloudFactsState::Verified {
            origin: FactsOrigin::DiskCache,
            ..
        }
    ));
    assert!(overlay::overlay().is_some());
    overlay::clear();

    // Tamper one payload byte on disk.
    let path = s.cache_path.clone().unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let mut cache: serde_json::Value = serde_json::from_str(&text).unwrap();
    let env = cache["envelope"]
        .as_str()
        .unwrap()
        .replace("\"facts_version\": 7", "\"facts_version\": 9");
    cache["envelope"] = serde_json::Value::String(env);
    std::fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();
    configure_with_keys(&s, test_keys());
    assert_eq!(maybe_load_persisted_cache_with_keys(&s, test_keys()), None);
    assert!(
        matches!(status().state, CloudFactsState::Rejected { .. }),
        "{:?}",
        status().state
    );
    let cleared = load_cache(&path).expect("retain the rollback floor");
    assert!(cleared.envelope.is_empty());
    assert_eq!(cleared.highest_seen_version, Some(7));
    assert!(overlay::overlay().is_none());
}

#[test]
fn local_path_loads_without_network_and_scope_rejection_is_reported() {
    let _lock = lock();
    overlay::clear();
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("envelope.json");
    std::fs::write(&local, FIXTURE_V7).unwrap();
    let mut s = settings(&dir, Some("http://127.0.0.1:1/{channel}".into()));
    s.local_path = Some(local.clone());
    let outcome = rt()
        .block_on(refresh_fixture(&s, true, test_keys()))
        .unwrap();
    assert_eq!(outcome, RefreshOutcome::Updated { facts_version: 7 });
    assert!(matches!(
        status().state,
        CloudFactsState::Verified {
            origin: FactsOrigin::LocalFile,
            ..
        }
    ));

    std::fs::write(&local, FIXTURE_FUTURE_V8).unwrap();
    let err = rt()
        .block_on(refresh_fixture(&s, true, test_keys()))
        .unwrap_err();
    assert!(matches!(
        err,
        RefreshError::Rejected(FactsRejection::NotApplicable { .. })
    ));
    assert!(matches!(
        status().state,
        CloudFactsState::NotApplicable { .. }
    ));
    overlay::clear();
}

#[test]
fn server_errors_keep_prior_facts_and_persist_backoff() {
    let _lock = lock();
    overlay::clear();
    let rt = rt();
    let server = rt.block_on(mock_server(vec![
        (200, vec![], FIXTURE_V7.into()),
        (500, vec![], "boom".into()),
        (200, vec![], "x".repeat(MAX_BODY_BYTES + 1)),
    ]));
    let dir = tempfile::tempdir().unwrap();
    let s = settings(&dir, Some(server.url.clone()));
    rt.block_on(refresh_fixture(&s, true, test_keys())).unwrap();

    let err = rt
        .block_on(refresh_fixture(&s, true, test_keys()))
        .unwrap_err();
    assert_eq!(err, RefreshError::HttpStatus(500));
    assert!(
        matches!(
            status().state,
            CloudFactsState::Failed {
                keeping: Some(7),
                ..
            }
        ),
        "{:?}",
        status().state
    );
    assert!(
        overlay::overlay().is_some(),
        "prior verified facts survive a failure"
    );

    // Backoff is persisted and honoured by non-forced refreshes.
    let cache: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(s.cache_path.as_ref().unwrap()).unwrap())
            .unwrap();
    assert!(cache["backoff_until"].as_u64().unwrap() > now_unix());
    let err = rt
        .block_on(refresh_fixture(&s, false, test_keys()))
        .unwrap_err();
    assert!(matches!(err, RefreshError::BackingOff { .. }));

    // Oversized body is refused before verification.
    let err = rt
        .block_on(refresh_fixture(&s, true, test_keys()))
        .unwrap_err();
    assert!(matches!(err, RefreshError::TooLarge(_)));
    assert!(overlay::overlay().is_some());
    overlay::clear();
}

#[test]
fn not_found_means_no_facts_not_failure() {
    let _lock = lock();
    overlay::clear();
    let rt = rt();
    let server = rt.block_on(mock_server(vec![(
        404,
        vec![],
        "{\"error\":\"no-facts\"}".into(),
    )]));
    let dir = tempfile::tempdir().unwrap();
    let s = settings(&dir, Some(server.url.clone()));
    let outcome = rt.block_on(refresh_fixture(&s, true, test_keys())).unwrap();
    assert_eq!(outcome, RefreshOutcome::NoFacts);
    assert_eq!(status().state, CloudFactsState::BundledOnly);
    assert!(overlay::overlay().is_none());
}

#[test]
fn settings_resolve_url_template_and_channel_validation() {
    let s = Settings {
        channel: "beta".into(),
        ..Settings::default()
    };
    assert_eq!(s.url(), "https://codewhale.net/api/facts/v1/beta");
    assert!(valid_channel("stable"));
    assert!(valid_channel("beta-2"));
    assert!(!valid_channel("-bad"));
    assert!(!valid_channel("Stable"));
    assert!(!valid_channel(""));
    assert_eq!(
        Settings::default().cache_path.as_deref(),
        None::<&std::path::Path>,
        "default settings resolve the cache under CODEWHALE_HOME"
    );
    let _ = PathBuf::new();
}

#[test]
fn disable_rejects_late_200_and_304_and_preserves_cache_bytes() {
    let _lock = lock();
    for not_modified in [false, true] {
        overlay::clear();
        let dir = tempfile::tempdir().unwrap();
        let s = settings(&dir, Some("https://fixture.invalid/{channel}".into()));
        configure_with_keys(&s, test_keys());
        rt().block_on(async {
            refresh_using(&s, true, test_keys(), None, false, |_, _| async {
                Ok(Fetched::Body {
                    bytes: FIXTURE_V7.as_bytes().to_vec(),
                    etag: Some("v7".into()),
                })
            })
            .await
            .unwrap();
            let before = std::fs::read(s.cache_path.as_ref().unwrap()).unwrap();
            let admitted = ticket(&s, test_keys()).unwrap();
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            let refresh = refresh_using(
                &s,
                true,
                test_keys(),
                Some(admitted),
                false,
                |_, _| async move {
                    entered_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    Ok(if not_modified {
                        Fetched::NotModified
                    } else {
                        Fetched::Body {
                            bytes: FIXTURE_V7.as_bytes().to_vec(),
                            etag: Some("late".into()),
                        }
                    })
                },
            );
            let disable = async {
                entered_rx.await.unwrap();
                let mut disabled = s.clone();
                disabled.enabled = false;
                configure_with_keys(&disabled, test_keys());
                release_tx.send(()).unwrap();
            };
            let (result, ()) = tokio::join!(refresh, disable);
            assert!(
                std::fs::read(s.cache_path.as_ref().unwrap()).unwrap() == before,
                "a disabled refresh must not change cache bytes"
            );
            assert_eq!(result, Err(RefreshError::Superseded));
            assert!(overlay::overlay().is_none());
            assert_eq!(status().state, CloudFactsState::Off);
            assert_eq!(
                refresh_using(&s, true, test_keys(), None, false, |_, _| async {
                    panic!("old settings cannot create transport")
                })
                .await,
                Err(RefreshError::Superseded)
            );
        });
    }
}

#[test]
fn changed_source_drops_etag_and_304_reverifies_cached_envelope() {
    let _lock = lock();
    overlay::clear();
    let dir = tempfile::tempdir().unwrap();
    let mut s = settings(&dir, Some("https://first.invalid/{channel}".into()));
    configure_with_keys(&s, test_keys());
    rt().block_on(async {
        refresh_using(&s, true, test_keys(), None, false, |_, _| async {
            Ok(Fetched::Body {
                bytes: FIXTURE_V7.as_bytes().to_vec(),
                etag: Some("first-v7".into()),
            })
        })
        .await
        .unwrap();
        s.url = Some("https://second.invalid/{channel}".into());
        configure_with_keys(&s, test_keys());
        refresh_using(&s, true, test_keys(), None, false, |_, etag| async move {
            assert!(
                etag.is_none(),
                "validators must not cross source boundaries"
            );
            Ok(Fetched::Body {
                bytes: FIXTURE_V7.as_bytes().to_vec(),
                etag: Some("second-v7".into()),
            })
        })
        .await
        .unwrap();
        let path = s.cache_path.as_ref().unwrap();
        let mut cache = load_cache(path).unwrap();
        assert_eq!(cache.highest_seen_version, Some(7));
        cache.highest_seen_version = Some(8);
        save_cache(path, &cache);
        let result = refresh_using(&s, true, test_keys(), None, false, |_, etag| async move {
            assert_eq!(etag.as_deref(), Some("second-v7"));
            Ok(Fetched::NotModified)
        })
        .await;
        assert!(
            matches!(result, Err(RefreshError::Rejected(_))),
            "{result:?}"
        );
        assert!(overlay::overlay().is_none());
        let rejected = load_cache(path).unwrap();
        assert_eq!(rejected.highest_seen_version, Some(8));
        assert!(rejected.envelope.is_empty());
        assert!(rejected.etag.is_none());
    });
}

#[test]
fn fixture_transport_is_explicit_and_production_suppression_still_wins() {
    let _lock = lock();
    overlay::clear();
    let dir = tempfile::tempdir().unwrap();
    let s = settings(&dir, Some("https://fixture.invalid/{channel}".into()));
    configure_with_keys(&s, test_keys());
    let _ci = TestEnv::set("CI", "true");
    rt().block_on(async {
        assert_eq!(
            refresh_with_keys(&s, true, test_keys()).await,
            Err(RefreshError::Suppressed)
        );
        let suppressed = refresh_using(&s, true, test_keys(), None, true, |_, _| async {
            panic!("production suppression precedes transport")
        })
        .await;
        assert_eq!(suppressed, Err(RefreshError::Suppressed));
        let fixture = refresh_using(&s, true, test_keys(), None, false, |_, _| async {
            Ok(Fetched::NotFound)
        })
        .await;
        assert_eq!(fixture, Ok(RefreshOutcome::NoFacts));
    });
}

#[test]
fn bounded_files_reject_oversize_outer_cache_and_nonregular_inputs() {
    let _lock = lock();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large");
    std::fs::File::create(&path)
        .unwrap()
        .set_len((MAX_CACHE_BYTES + 1) as u64)
        .unwrap();
    assert!(matches!(
        read_bounded_regular(&path, MAX_CACHE_BYTES),
        Err(RefreshError::TooLarge(_))
    ));
    assert!(load_cache(&path).is_none());
    assert!(read_bounded_regular(dir.path(), MAX_BODY_BYTES).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let target = dir.path().join("target");
        std::fs::write(&target, FIXTURE_V7).unwrap();
        let linked = dir.path().join("symlink");
        symlink(&target, &linked).unwrap();
        assert!(read_bounded_regular(&linked, MAX_BODY_BYTES).is_err());
        let hardlink = dir.path().join("hardlink");
        std::fs::hard_link(&target, &hardlink).unwrap();
        assert!(read_bounded_regular(&target, MAX_BODY_BYTES).is_err());
        let fifo = dir.path().join("fifo");
        let raw = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
        assert!(read_bounded_regular(&fifo, MAX_BODY_BYTES).is_err());
    }
}

#[test]
fn chunked_fetch_stops_at_body_limit_without_waiting_for_end_of_stream() {
    let _lock = lock();
    rt().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/facts", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut head = [0u8; 4096];
            let _ = stream.read(&mut head).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            let chunk = vec![b'x'; 8192];
            for _ in 0..(MAX_BODY_BYTES / chunk.len() + 1) {
                if stream.write_all(b"2000\r\n").await.is_err() {
                    return;
                }
                if stream.write_all(&chunk).await.is_err() {
                    return;
                }
                if stream.write_all(b"\r\n").await.is_err() {
                    return;
                }
            }
            std::future::pending::<()>().await;
        });
        let result = tokio::time::timeout(Duration::from_secs(3), fetch(url, None))
            .await
            .expect("oversized stream must be rejected before timeout/end-of-body");
        assert!(matches!(result, Err(RefreshError::TooLarge(_))));
        server.abort();
    });
}

#[test]
fn hard_disable_blocks_local_cache_transport_and_late_publication() {
    let _lock = lock();
    overlay::clear();
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("local.json");
    std::fs::write(&local, FIXTURE_V7).unwrap();
    let mut s = settings(&dir, None);
    s.local_path = Some(local);
    rt().block_on(refresh_fixture(&s, true, test_keys()))
        .unwrap();
    let path = s.cache_path.as_ref().unwrap();
    let before = std::fs::read(path).unwrap();
    let prior = ticket(&s, test_keys()).unwrap();
    let _disable = TestEnv::set(ENV_DISABLE, "1");
    assert!(overlay::overlay().is_none());
    assert_eq!(maybe_load_persisted_cache_with_keys(&s, test_keys()), None);
    let result = rt().block_on(refresh_using(
        &s,
        true,
        test_keys(),
        Some(prior),
        false,
        |_, _| async { panic!("hard disable must precede local reads and transport") },
    ));
    assert_eq!(result, Err(RefreshError::Disabled));
    assert_eq!(std::fs::read(path).unwrap(), before);
    configure_with_keys(&s, test_keys());
    assert_eq!(status().state, CloudFactsState::Off);
    assert!(!s.resolve().enabled);
}

#[test]
fn source_switch_and_disable_keep_channel_floor_after_cache_removal() {
    let _lock = lock();
    overlay::clear();
    let dir = tempfile::tempdir().unwrap();
    let mut s = settings(&dir, Some("https://first.invalid/{channel}".into()));
    configure_with_keys(&s, test_keys());
    rt().block_on(refresh_using(
        &s,
        true,
        test_keys(),
        None,
        false,
        |_, _| async {
            Ok(Fetched::Body {
                bytes: FIXTURE_V7.as_bytes().to_vec(),
                etag: Some("first-v7".into()),
            })
        },
    ))
    .unwrap();
    std::fs::remove_file(s.cache_path.as_ref().unwrap()).unwrap();
    overlay::clear();
    s.url = Some("https://second.invalid/{channel}".into());
    configure_with_keys(&s, test_keys());
    assert_eq!(overlay::highest_seen("stable"), Some(7));
    let cache = cache_for(&s, test_keys()).unwrap();
    assert_eq!(cache.highest_seen_version, Some(7));
    assert!(cache.envelope.is_empty());
    assert!(cache.etag.is_none());
}

#[test]
fn forged_cache_timestamps_cannot_make_facts_fresh_or_block_refresh() {
    let _lock = lock();
    overlay::clear();
    let dir = tempfile::tempdir().unwrap();
    let s = settings(&dir, Some("https://fixture.invalid/{channel}".into()));
    configure_with_keys(&s, test_keys());
    rt().block_on(async {
        refresh_using(&s, true, test_keys(), None, false, |_, _| async {
            Ok(Fetched::Body {
                bytes: FIXTURE_V7.as_bytes().to_vec(),
                etag: Some("v7".into()),
            })
        })
        .await
        .unwrap();
        let path = s.cache_path.as_ref().unwrap();
        let mut cache = load_cache(path).unwrap();
        cache.fetched_at = u64::MAX;
        cache.backoff_until = Some(u64::MAX);
        save_cache(path, &cache);
        assert_eq!(
            maybe_load_persisted_cache_with_keys(&s, test_keys()),
            Some(7)
        );
        assert!(
            overlay::overlay().is_none(),
            "future local metadata cannot grant authority"
        );
        let result = refresh_using(&s, false, test_keys(), None, false, |_, etag| async move {
            assert_eq!(etag.as_deref(), Some("v7"));
            Ok(Fetched::NotModified)
        })
        .await;
        assert_eq!(
            result,
            Ok(RefreshOutcome::NotModified {
                facts_version: Some(7)
            })
        );
        assert!(overlay::overlay().is_some());
        let repaired = load_cache(path).unwrap();
        assert!(repaired.fetched_at <= now_unix());
        assert!(repaired.backoff_until.is_none());
        assert_eq!(repaired.highest_seen_version, Some(7));
    });
}

#[test]
fn cache_restart_recovers_authenticated_floor_before_source_switch() {
    const CHILD_CACHE: &str = "CODEWHALE_TEST_CLOUD_RESTART_CACHE";
    let _lock = lock();
    if let Some(path) = std::env::var_os(CHILD_CACHE) {
        // This child runs only this test, so there is no inherited process
        // overlay to mask a lost disk rollback receipt.
        assert_eq!(overlay::highest_seen("stable"), None);
        let s = Settings {
            enabled: true,
            url: Some("https://second.invalid/{channel}".into()),
            cache_path: Some(path.into()),
            ..Settings::default()
        };
        configure_with_keys(&s, test_keys());
        let cache = cache_for(&s, test_keys()).unwrap();
        assert_eq!(cache.highest_seen_version, Some(7));
        assert!(cache.envelope.is_empty());
        assert!(cache.etag.is_none());
        return;
    }
    overlay::clear();
    let dir = tempfile::tempdir().unwrap();
    let s = settings(&dir, Some("https://first.invalid/{channel}".into()));
    configure_with_keys(&s, test_keys());
    rt().block_on(refresh_using(
        &s,
        true,
        test_keys(),
        None,
        false,
        |_, _| async {
            Ok(Fetched::Body {
                bytes: FIXTURE_V7.as_bytes().to_vec(),
                etag: Some("first-v7".into()),
            })
        },
    ))
    .unwrap();
    let path = s.cache_path.as_ref().unwrap();
    let mut cache = load_cache(path).unwrap();
    cache.highest_seen_version = Some(0);
    save_cache(path, &cache);
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tests::cache_restart_recovers_authenticated_floor_before_source_switch",
            "--nocapture",
        ])
        .env(CHILD_CACHE, path)
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "fresh-process check failed: {}{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
}

#[test]
fn concurrent_refreshes_do_not_hold_lock_across_transport_await() {
    let _lock = lock();
    overlay::clear();
    let dir = tempfile::tempdir().unwrap();
    let s = settings(&dir, Some("https://fixture.invalid/{channel}".into()));
    configure_with_keys(&s, test_keys());
    rt().block_on(async {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();

        let first = refresh_using(&s, true, test_keys(), None, false, |_, _| async move {
            entered_tx.send(()).expect("first entered transport");
            release_rx.await.expect("first released");
            Err(RefreshError::Network("slow first refresh".into()))
        });
        let second = async {
            entered_rx.await.expect("wait for first in transport");
            let outcome = refresh_using(&s, true, test_keys(), None, false, |_, _| async {
                Ok(Fetched::Body {
                    bytes: FIXTURE_V7.as_bytes().to_vec(),
                    etag: Some("v7".into()),
                })
            })
            .await;
            release_tx.send(()).expect("release first refresh");
            outcome
        };

        let (first_result, second_result) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(first, second)
        })
        .await
        .expect("second refresh must not block on first refresh's transport await");

        assert_eq!(
            second_result,
            Ok(RefreshOutcome::Updated { facts_version: 7 })
        );
        assert!(matches!(first_result, Err(RefreshError::Network(_))));
        assert_eq!(
            overlay::overlay().map(|facts| facts.facts_version),
            Some(7),
            "slower failing refresh must not overwrite newer verified facts"
        );
        assert!(matches!(
            status().state,
            CloudFactsState::Verified {
                facts_version: 7,
                ..
            }
        ));
    });
}

#[test]
fn publish_off_runtime_does_not_block_current_thread_worker() {
    rt().block_on(async {
        let worker_id = std::thread::current().id();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);

        let publish = publish_off_runtime(move || {
            assert_ne!(std::thread::current().id(), worker_id);
            entered_tx.send(()).expect("signal entered");
            release_rx.recv().expect("wait for worker");
            true
        });
        let worker = async {
            entered_rx.await.expect("blocking task entered");
            release_tx.send(()).expect("unblock blocking task");
        };

        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(publish, worker)
        })
        .await
        .expect("off-runtime publication must not stall current_thread worker");
        assert_eq!(result, Ok(()));
    });
}
