//! Measurement-only persistence backlog tests.

use super::*;
use std::time::{Duration, Instant};

use codewhale_models::Role;
use codewhale_models::{ContentBlock, Message};

const BACKLOG_RECEIPT_PATH_ENV: &str = "CODEWHALE_TEST_PERSISTENCE_BACKLOG_RECEIPT_PATH";
const BACKLOG_SOURCE_SHA_ENV: &str = "CODEWHALE_TEST_PERSISTENCE_BACKLOG_SOURCE_SHA";
const BACKLOG_SOURCE_DIRTY_ENV: &str = "CODEWHALE_TEST_PERSISTENCE_BACKLOG_SOURCE_DIRTY";
const BACKLOG_RUSTC_VERSION_ENV: &str = "CODEWHALE_TEST_PERSISTENCE_BACKLOG_RUSTC_VERSION";
const BACKLOG_CARGO_VERSION_ENV: &str = "CODEWHALE_TEST_PERSISTENCE_BACKLOG_CARGO_VERSION";
const BACKLOG_FIXTURE_ID: &str = "paused-production-channel-session-snapshot-v1";
const BACKLOG_REQUESTS_ATTEMPTED: usize = 128;
const BACKLOG_CONTENT_BYTES_PER_REQUEST: usize = 64 * 1024;
const BACKLOG_EXPECTED_APPLIED_VERSION: usize = BACKLOG_REQUESTS_ATTEMPTED - 1;
const BACKLOG_SESSION_ID: &str = "persistence-backlog-single-session";
const BACKLOG_PAYLOAD_ESTIMATOR: &str = "retained-saved-session-json-bytes-v1";
const BACKLOG_REQUEST_VARIANT: &str = "session_snapshot";

#[derive(Debug)]
struct PersistenceBacklogObservation {
    accepted_requests: usize,
    retained_queued_requests: usize,
    estimated_retained_payload_bytes: usize,
    applied_version: Option<usize>,
    enqueue_elapsed_ns: u128,
    rss_before_bytes: Option<u64>,
    rss_during_bytes: Option<u64>,
    rss_after_bytes: Option<u64>,
}

#[derive(Debug)]
struct PersistenceBacklogProvenance {
    source_sha: String,
    source_dirty: bool,
    rustc_version: String,
    cargo_version: String,
}

fn persistence_backlog_receipt(
    observation: &PersistenceBacklogObservation,
    provenance: &PersistenceBacklogProvenance,
) -> serde_json::Value {
    let rss_supported = observation.rss_before_bytes.is_some()
        && observation.rss_during_bytes.is_some()
        && observation.rss_after_bytes.is_some();
    serde_json::json!({
        "document_kind": "codewhale.persistence_backlog_receipt",
        "schema_version": 2,
        "source_sha": provenance.source_sha,
        "source_dirty": provenance.source_dirty,
        "rustc_version": provenance.rustc_version,
        "cargo_version": provenance.cargo_version,
        "build_profile": "test",
        "sample_count": 1,
        "fixture_id": BACKLOG_FIXTURE_ID,
        "platform": std::env::consts::OS,
        "request_variant": BACKLOG_REQUEST_VARIANT,
        "payload_estimator": BACKLOG_PAYLOAD_ESTIMATOR,
        "paused_consumer": true,
        "requests_attempted": BACKLOG_REQUESTS_ATTEMPTED,
        "content_bytes_per_request": BACKLOG_CONTENT_BYTES_PER_REQUEST,
        "single_session_id": true,
        "expected_applied_version": BACKLOG_EXPECTED_APPLIED_VERSION,
        "accepted_requests": observation.accepted_requests,
        "retained_queued_requests": observation.retained_queued_requests,
        "estimated_retained_payload_bytes": observation.estimated_retained_payload_bytes,
        "applied_version": observation.applied_version,
        "final_version_applied": observation.applied_version == Some(BACKLOG_EXPECTED_APPLIED_VERSION),
        "enqueue_elapsed_ns": observation.enqueue_elapsed_ns,
        "rss_supported": rss_supported,
        "rss_before_bytes": observation.rss_before_bytes,
        "rss_during_bytes": observation.rss_during_bytes,
        "rss_after_bytes": observation.rss_after_bytes,
        "rss_during_delta_bytes": observation.rss_during_bytes.zip(observation.rss_before_bytes)
            .map(|(during, before)| during.saturating_sub(before)),
        "rss_after_delta_bytes": observation.rss_after_bytes.zip(observation.rss_before_bytes)
            .map(|(after, before)| after.saturating_sub(before)),
        "limitations": [
            "current-process RSS samples are available on macOS only",
            "serialized SavedSession bytes estimate retained heap payload after draining the paused production receiver; allocator and channel overhead are observed only through RSS",
            "one bounded sample characterizes backlog retention but does not prove an asymptotic growth rate",
        ],
    })
}

#[cfg(target_os = "macos")]
fn current_process_rss_bytes() -> Option<u64> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let kib = String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    kib.checked_mul(1024)
}

#[cfg(not(target_os = "macos"))]
fn current_process_rss_bytes() -> Option<u64> {
    None
}

fn backlog_session(workspace: &std::path::Path, index: usize) -> SavedSession {
    let messages = [Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "x".repeat(BACKLOG_CONTENT_BYTES_PER_REQUEST),
            cache_control: None,
        }],
    }];
    let mut session = codewhale_engine::session_manager::create_saved_session_with_mode(
        &messages,
        "measurement-model",
        workspace,
        0,
        None,
        Some("agent"),
    );
    session.metadata.id = BACKLOG_SESSION_ID.to_string();
    session.metadata.title = format!("Persistence backlog version {index:04}");
    session
}

fn retained_session_payload_bytes(session: &SavedSession) -> usize {
    serde_json::to_vec(session)
        .expect("retained measurement session must serialize")
        .len()
}

fn saved_session_version(session: &SavedSession) -> Option<usize> {
    session
        .metadata
        .title
        .strip_prefix("Persistence backlog version ")
        .and_then(|value| value.parse::<usize>().ok())
}

/// Measure what the seam still owns for a consumer that has never polled:
/// with sender-side coalescing that is the latest-wins pending state (one
/// entry per session), plus the serialized bytes of each entry — the same
/// estimator the pre-coalescing receipt used per queued request.
fn drain_retained_requests(receiver: &mut PersistRequestReceiver) -> (usize, usize, Option<usize>) {
    let pending = receiver.take_pending();
    assert!(
        pending.checkpoints.is_empty()
            && pending.completed_commits.is_empty()
            && pending.offline_queue.is_empty(),
        "backlog fixture retained an unexpected request variant"
    );
    let mut retained_queued_requests = 0;
    let mut estimated_retained_payload_bytes = 0;
    for session in pending.sessions.values() {
        let (PendingSession::Snapshot(session) | PendingSession::Completion(session)) = session;
        retained_queued_requests += 1;
        estimated_retained_payload_bytes += retained_session_payload_bytes(session);
    }
    let applied_version =
        pending
            .sessions
            .get(BACKLOG_SESSION_ID)
            .and_then(|session| match session {
                PendingSession::Snapshot(session) | PendingSession::Completion(session) => {
                    saved_session_version(session)
                }
            });
    (
        retained_queued_requests,
        estimated_retained_payload_bytes,
        applied_version,
    )
}

fn measure_paused_persistence_backlog() -> PersistenceBacklogObservation {
    let tmp = tempfile::tempdir().expect("isolated measurement home");
    let _env_lock = crate::test_support::lock_test_env();
    let _home = crate::test_support::EnvVarGuard::set("HOME", tmp.path());
    let _userprofile = crate::test_support::EnvVarGuard::set("USERPROFILE", tmp.path());
    let _codewhale_home =
        crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join(".codewhale"));

    let (tx, mut receiver) = persistence_request_channel();
    let handle = PersistActorHandle { tx };
    let rss_before_bytes = current_process_rss_bytes();
    let mut accepted_requests = 0;
    let mut enqueue_elapsed_ns = 0;

    for index in 0..BACKLOG_REQUESTS_ATTEMPTED {
        let session = backlog_session(tmp.path(), index);
        let started = Instant::now();
        let accepted = handle.try_send(PersistRequest::SessionSnapshot(session));
        enqueue_elapsed_ns += started.elapsed().as_nanos();
        if accepted {
            accepted_requests += 1;
        }
    }

    // The receiver has deliberately never been polled: RSS is sampled while
    // the production channel still owns every representation it retained.
    std::hint::black_box(&receiver);
    let rss_during_bytes = current_process_rss_bytes();

    let (retained_queued_requests, estimated_retained_payload_bytes, applied_version) =
        drain_retained_requests(&mut receiver);
    drop(handle);
    drop(receiver);
    std::thread::sleep(Duration::from_millis(50));
    let rss_after_bytes = current_process_rss_bytes();

    PersistenceBacklogObservation {
        accepted_requests,
        retained_queued_requests,
        estimated_retained_payload_bytes,
        applied_version,
        enqueue_elapsed_ns,
        rss_before_bytes,
        rss_during_bytes,
        rss_after_bytes,
    }
}

#[test]
fn persistence_backlog_receipt_contract_keeps_required_fields() {
    let receipt = persistence_backlog_receipt(
        &PersistenceBacklogObservation {
            accepted_requests: 2,
            retained_queued_requests: 1,
            estimated_retained_payload_bytes: 4_096,
            applied_version: Some(BACKLOG_EXPECTED_APPLIED_VERSION),
            enqueue_elapsed_ns: 1_000,
            rss_before_bytes: Some(10_000),
            rss_during_bytes: Some(14_000),
            rss_after_bytes: Some(11_000),
        },
        &PersistenceBacklogProvenance {
            source_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
            source_dirty: false,
            rustc_version: "rustc test".to_string(),
            cargo_version: "cargo test".to_string(),
        },
    );
    let object = receipt.as_object().expect("receipt object");
    for field in [
        "document_kind",
        "schema_version",
        "source_sha",
        "source_dirty",
        "rustc_version",
        "cargo_version",
        "build_profile",
        "sample_count",
        "fixture_id",
        "platform",
        "request_variant",
        "payload_estimator",
        "paused_consumer",
        "requests_attempted",
        "content_bytes_per_request",
        "single_session_id",
        "expected_applied_version",
        "accepted_requests",
        "retained_queued_requests",
        "estimated_retained_payload_bytes",
        "applied_version",
        "final_version_applied",
        "enqueue_elapsed_ns",
        "rss_supported",
        "rss_before_bytes",
        "rss_during_bytes",
        "rss_after_bytes",
        "rss_during_delta_bytes",
        "rss_after_delta_bytes",
        "limitations",
    ] {
        assert!(object.contains_key(field), "receipt lost `{field}`");
    }
    assert_eq!(receipt["final_version_applied"], true);
    assert_eq!(receipt["rss_during_delta_bytes"], 4_000);
    assert_eq!(receipt["rss_after_delta_bytes"], 1_000);
}

#[test]
fn applied_version_follows_production_drain_order_instead_of_numeric_maximum() {
    let tmp = tempfile::tempdir().expect("isolated measurement workspace");
    let (tx, mut receiver) = persistence_request_channel();
    let handle = PersistActorHandle { tx };
    assert!(
        handle.try_send(PersistRequest::SessionSnapshot(backlog_session(
            tmp.path(),
            BACKLOG_EXPECTED_APPLIED_VERSION,
        )))
    );
    assert!(
        handle.try_send(PersistRequest::SessionSnapshot(backlog_session(
            tmp.path(),
            BACKLOG_EXPECTED_APPLIED_VERSION - 1,
        )))
    );

    let (_, _, applied_version) = drain_retained_requests(&mut receiver);
    assert_eq!(
        applied_version,
        Some(BACKLOG_EXPECTED_APPLIED_VERSION - 1),
        "production PendingState must expose stale last-drained ordering instead of hiding it behind max(version)"
    );
}

/// Exact, ignored child-process measurement invoked by
/// `scripts/measure-persistence-backlog.py`.
#[test]
#[ignore = "one-shot metric for scripts/measure-persistence-backlog.py"]
fn write_paused_persistence_backlog_measurement_receipt() {
    let Ok(path) = std::env::var(BACKLOG_RECEIPT_PATH_ENV) else {
        return;
    };
    let provenance = PersistenceBacklogProvenance {
        source_sha: std::env::var(BACKLOG_SOURCE_SHA_ENV)
            .expect("measurement wrapper must provide the exact source SHA"),
        source_dirty: std::env::var(BACKLOG_SOURCE_DIRTY_ENV)
            .expect("measurement wrapper must provide source dirty state")
            .parse::<bool>()
            .expect("source dirty state must be true or false"),
        rustc_version: std::env::var(BACKLOG_RUSTC_VERSION_ENV)
            .expect("measurement wrapper must provide rustc version"),
        cargo_version: std::env::var(BACKLOG_CARGO_VERSION_ENV)
            .expect("measurement wrapper must provide cargo version"),
    };
    let receipt = persistence_backlog_receipt(&measure_paused_persistence_backlog(), &provenance);
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&receipt).expect("serialize backlog receipt"),
    )
    .expect("write backlog receipt");
}
