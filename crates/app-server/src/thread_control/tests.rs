use super::*;
use crate::http::*;
use crate::stdio::*;
use crate::turns::*;
use axum::extract::Request;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use tokio::sync::Notify;

#[derive(Clone)]
struct FakeOwner {
    record: Arc<Mutex<Value>>,
    calls: Arc<Mutex<Vec<(String, String, Value)>>>,
    owner: RuntimeOwnerReceipt,
    block_import: bool,
    import_seen: Arc<Notify>,
    import_release: Arc<Notify>,
    operations: Arc<Mutex<HashMap<String, (Value, CanonicalThreadReceipt)>>>,
    fork_record: Arc<Mutex<Option<Value>>>,
    missing_after_commit: Arc<std::sync::atomic::AtomicBool>,
    goal: Arc<Mutex<Option<Value>>>,
    turn_seq: Arc<std::sync::atomic::AtomicU64>,
    pending_operation: Arc<std::sync::atomic::AtomicBool>,
    recovery_prepared: Arc<std::sync::atomic::AtomicBool>,
    recovery_refusal: Arc<std::sync::atomic::AtomicBool>,
    document_digest: Arc<Mutex<String>>,
}

async fn owned_http(State(fake): State<FakeOwner>, request: Request) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let authorization = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .cloned();
    let bytes = axum::body::to_bytes(request.into_body(), MAX_CANONICAL_HISTORY_BYTES)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    fake.calls
        .lock()
        .await
        .push((method.to_string(), path.clone(), body.clone()));
    if path == "/v1/config/reload" {
        assert_eq!(method, Method::POST);
        assert_eq!(
            authorization.as_ref().map(|value| value.as_bytes()),
            Some(b"Bearer private-compatibility-reload-fixture".as_slice())
        );
        return StatusCode::NO_CONTENT.into_response();
    }
    if path == "/v1/thread-history/operations/lookup"
        || path == "/v1/thread-history/operations/recover"
    {
        let recovery = if path.ends_with("/recover") {
            Some(serde_json::from_value::<CanonicalThreadOperationRecovery>(body.clone()).unwrap())
        } else {
            None
        };
        let input: CanonicalThreadOperationLookup = match recovery.as_ref() {
            Some(recovery) => recovery.operation.clone(),
            None => serde_json::from_value(body).unwrap(),
        };
        assert_eq!(input.expected_data_dir, fake.owner.data_dir);
        assert_eq!(input.expected_execution_scope, fake.owner.execution_scope);
        let operations = fake.operations.lock().await;
        let Some((prior, receipt)) = operations.get(&input.operation_key) else {
            return Json(CanonicalThreadOperationStatus::Absent).into_response();
        };
        let prior: CanonicalThreadMutationRequest = serde_json::from_value(prior.clone()).unwrap();
        assert_eq!(input.workspace, prior.workspace);
        let (kind, source) = match prior.mutation {
            CanonicalThreadMutation::Create { .. } => (CanonicalThreadOperationKind::Create, None),
            CanonicalThreadMutation::Resume { source, .. } => {
                (CanonicalThreadOperationKind::Resume, Some(source))
            }
            CanonicalThreadMutation::Fork { source, .. } => {
                (CanonicalThreadOperationKind::Fork, Some(source))
            }
        };
        let source_runtime_thread_id = source.as_ref().and_then(|source| match source {
            CanonicalHistorySource::Thread {
                runtime_thread_id, ..
            } => Some(runtime_thread_id.clone()),
            CanonicalHistorySource::SavedSession { .. } => None,
        });
        let association = codewhale_protocol::CanonicalThreadOperationAssociation {
            kind,
            source_runtime_thread_id,
            source_session_id: None,
        };
        if let Some(recovery) = recovery {
            if recovery.association != association {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({"error":"retained association changed"})),
                )
                    .into_response();
            }
            if fake
                .recovery_refusal
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({"error":"prepared target graph changed"})),
                )
                    .into_response();
            }
            if fake
                .recovery_prepared
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                fake.pending_operation
                    .store(false, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let status = if fake
            .pending_operation
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            CanonicalThreadOperationStatus::Pending {
                receipt: receipt.clone(),
                association,
            }
        } else {
            CanonicalThreadOperationStatus::Committed {
                receipt: receipt.clone(),
                association,
            }
        };
        return Json(status).into_response();
    }
    if path == "/v1/thread-history/mutate" {
        let input: CanonicalThreadMutationRequest = serde_json::from_value(body.clone()).unwrap();
        let mut operations = fake.operations.lock().await;
        if let Some((prior, receipt)) = operations.get(&input.operation_key) {
            if prior != &body {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({"error":"intent changed"})),
                )
                    .into_response();
            }
            return Json(receipt.clone()).into_response();
        }
        let mut record = fake.record.lock().await.clone();
        let id = if matches!(input.mutation, CanonicalThreadMutation::Fork { .. }) {
            record["id"] = json!("canonical-fork");
            record["workspace"] = json!(input.workspace);
            *fake.fork_record.lock().await = Some(record);
            "canonical-fork"
        } else {
            "canonical-1"
        };
        let receipt = CanonicalThreadReceipt {
            version: 1,
            data_dir: fake.owner.data_dir,
            execution_scope: fake.owner.execution_scope,
            operation_key: input.operation_key.clone(),
            request_digest: "1".repeat(64),
            history_digest: "2".repeat(64),
            runtime_thread_id: id.into(),
            session_id: if id == "canonical-fork" {
                "session-fork"
            } else {
                "session-1"
            }
            .into(),
        };
        operations.insert(input.operation_key, (body, receipt.clone()));
        return Json(receipt).into_response();
    }
    if path == "/v1/thread-history/import" {
        let input: CanonicalHistoryImportRequest = serde_json::from_value(body).unwrap();
        if input
            .target_runtime_thread_id
            .as_deref()
            .is_some_and(|id| id != "canonical-1")
        {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"existing canonical target missing; no replacement"})),
            )
                .into_response();
        }
        fake.import_seen.notify_one();
        if fake.block_import {
            fake.import_release.notified().await;
        }
        if let Some(mut goal) = input.history.goal {
            goal.thread_id = input
                .target_runtime_thread_id
                .clone()
                .unwrap_or_else(|| "canonical-1".into());
            if goal.status == codewhale_protocol::ThreadGoalStatus::Active {
                goal.status = codewhale_protocol::ThreadGoalStatus::Paused;
                goal.pause_reason = None;
            }
            let goal = serde_json::to_value(goal).unwrap();
            let mut current = fake.goal.lock().await;
            if current.as_ref().is_some_and(|prior| prior != &goal) {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({"error":"canonical goal conflict; source retained"})),
                )
                    .into_response();
            }
            *current = Some(goal);
        }
        return Json(CanonicalThreadReceipt {
            version: 1,
            data_dir: fake.owner.data_dir,
            execution_scope: fake.owner.execution_scope,
            operation_key: input.operation_key,
            request_digest: "1".repeat(64),
            history_digest: "2".repeat(64),
            runtime_thread_id: input
                .target_runtime_thread_id
                .unwrap_or_else(|| "canonical-1".into()),
            session_id: "session-1".into(),
        })
        .into_response();
    }
    if path == "/v1/threads/running" {
        return Json(json!([])).into_response();
    }
    if path == "/v1/threads/canonical-1/turns" && method == Method::POST {
        let seq = fake
            .turn_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        return Json(json!({"turn":{"id":format!("fixture-turn-{seq}")}})).into_response();
    }
    if path == "/v1/threads/canonical-1/events" {
        let seq = fake.turn_seq.load(std::sync::atomic::Ordering::SeqCst);
        let frame = json!({"seq":seq,"turn_id":format!("fixture-turn-{seq}"),
            "payload":{"turn":{"status":"completed"}}});
        return (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("event: turn.completed\ndata: {frame}\n\n"),
        )
            .into_response();
    }
    if path == "/v1/threads/canonical-1/goal" {
        if method == Method::PUT {
            let goal = json!({"thread_id":"canonical-1","goal_id":"fixture-goal",
                "objective":body["objective"],"token_budget":body["token_budget"],"status":"active",
                "tokens_used":0,"time_used_seconds":0,"continuation_count":0,
                "created_at":1,"updated_at":1,"repeated_gap_count":0});
            *fake.goal.lock().await = Some(goal.clone());
            return Json(goal).into_response();
        }
        let mut goal = fake.goal.lock().await;
        if method == Method::DELETE && goal.take().is_some() {
            return StatusCode::NO_CONTENT.into_response();
        }
        return match goal.clone() {
            Some(goal) => Json(goal).into_response(),
            None => (StatusCode::NOT_FOUND, Json(json!({"error":"no goal"}))).into_response(),
        };
    }
    if path == "/v1/threads" {
        return Json(json!([fake.record.lock().await.clone()])).into_response();
    }
    if path == "/v1/threads/canonical-1/history" {
        return Json(json!({"version":1,"data_dir":fake.owner.data_dir,"execution_scope":fake.owner.execution_scope,
            "runtime_thread_id":"canonical-1","saved_session_id":"session-1","saved_document_digest":"a".repeat(64),"session_goal_digest":"74234e98afe7498fb5daf1f36ac2d78acc339464f950703b8c019892f982b90b","document_digest":fake.document_digest.lock().await.clone(),
            "session":{"metadata":{"id":"session-1"},"messages":[{"role":"user","content":"active"}],
                "journal":{"entries":[{"id":"root"},{"id":"inactive-branch"},{"id":"active"}]},"leaf_id":"active"}})).into_response();
    }
    if path == "/v1/threads/canonical-fork" {
        return Json(fake.fork_record.lock().await.clone().unwrap()).into_response();
    }
    if path != "/v1/threads/canonical-1" {
        return (StatusCode::NOT_FOUND, Json(json!({"error":"missing"}))).into_response();
    }
    if fake
        .missing_after_commit
        .load(std::sync::atomic::Ordering::SeqCst)
        && !fake.operations.lock().await.is_empty()
    {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"metadata disappeared"})),
        )
            .into_response();
    }
    if method == Method::PATCH {
        let mut record = fake.record.lock().await;
        for (key, value) in body.as_object().unwrap() {
            record[key] = if key == "title" && value.as_str().is_some_and(|s| s.trim().is_empty()) {
                Value::Null
            } else {
                value.clone()
            };
        }
        return Json(record.clone()).into_response();
    }
    Json(json!({"thread":fake.record.lock().await.clone(),"items":[],"turns":[]})).into_response()
}

fn fixture_state(block_import: bool) -> (AppState, tempfile::TempDir, FakeOwner) {
    let temp = tempfile::tempdir().unwrap();
    let config_path = temp.path().join("config.toml");
    std::fs::write(&config_path, "").unwrap();
    let mut state = build_state(Some(config_path.clone()), None).unwrap();
    let owner = RuntimeOwnerReceipt {
        version: 1,
        data_dir: temp.path().to_owned(),
        execution_scope: "fixture-owner".into(),
        lease_generation: "fixture-generation".into(),
        pid: std::process::id(),
        process_start: "fixture".into(),
        principal: "fixture".into(),
        socket_path: temp.path().join("owner.sock"),
        config_path: Some(config_path),
    };
    state.captured_owner = Some(owner.clone());
    state.frontend_workspace = Some(temp.path().to_owned());
    let fake = FakeOwner {
        record: Arc::new(Mutex::new(
            json!({"id":"canonical-1","created_at":"2026-10-02T00:00:00Z",
        "updated_at":"2026-10-02T00:00:01Z","model":"fixture-model","model_provider":"custom","model_provider_id":"fixture-account",
        "workspace":temp.path(),"archived":false,"title":"named"}),
        )),
        calls: Arc::default(),
        owner,
        block_import,
        import_seen: Arc::new(Notify::new()),
        import_release: Arc::new(Notify::new()),
        operations: Arc::default(),
        fork_record: Arc::default(),
        missing_after_commit: Arc::default(),
        goal: Arc::default(),
        turn_seq: Arc::default(),
        pending_operation: Arc::default(),
        recovery_prepared: Arc::default(),
        recovery_refusal: Arc::default(),
        document_digest: Arc::new(Mutex::new("b".repeat(64))),
    };
    (state, temp, fake)
}

async fn fixture(
    block_import: bool,
) -> (
    AppState,
    tempfile::TempDir,
    FakeOwner,
    tokio::task::JoinHandle<()>,
) {
    let (state, temp, fake) = fixture_state(block_import);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().fallback(owned_http).with_state(fake.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut bridge = RuntimeBridge::from_base_url_for_test(format!("http://{address}"));
    bridge.auth_token = Some("private-compatibility-reload-fixture".into());
    *state.runtime_bridge.lock().await = Some(Arc::new(Mutex::new(bridge)));
    (state, temp, fake, server)
}

pub(super) fn compatibility_router() -> (AppState, tempfile::TempDir, Router) {
    let (state, temp, fake) = fixture_state(false);
    (
        state,
        temp,
        Router::new().fallback(owned_http).with_state(fake),
    )
}

pub(super) async fn compatibility_fixture()
-> (AppState, tempfile::TempDir, tokio::task::JoinHandle<()>) {
    let (state, temp, _fake, server) = fixture(false).await;
    (state, temp, server)
}

async fn seed(state: &AppState, id: &str, workspace: &Path) -> StateStore {
    seed_archive(state, id, workspace, Vec::new(), None).await
}

async fn seed_archive(
    state: &AppState,
    id: &str,
    workspace: &Path,
    messages: Vec<codewhale_state::MessageRecord>,
    goal: Option<codewhale_state::ThreadGoalRecord>,
) -> StateStore {
    let store = state.runtime.read().await.state_store().clone();
    let thread = codewhale_state::ThreadMetadata {
        id: id.into(),
        rollout_path: None,
        preview: "legacy preview".into(),
        ephemeral: false,
        model_provider: "legacy-provider".into(),
        created_at: 1,
        updated_at: 1,
        status: codewhale_state::ThreadStatus::Idle,
        path: None,
        cwd: workspace.to_owned(),
        cli_version: "old".into(),
        source: codewhale_state::SessionSource::Api,
        name: Some("old name".into()),
        sandbox_policy: None,
        approval_mode: None,
        archived: false,
        archived_at: None,
        git_sha: None,
        git_branch: None,
        git_origin_url: None,
        memory_mode: None,
        current_leaf_id: messages.last().map(|message| message.id),
    };
    store
        .restore_legacy_thread_archive(&codewhale_state::LegacyThreadArchive {
            thread,
            messages,
            goal,
            checkpoints: Vec::new(),
        })
        .unwrap();
    store
}

fn archive_message(
    id: i64,
    text: &str,
    parent_entry_id: Option<i64>,
) -> codewhale_state::MessageRecord {
    codewhale_state::MessageRecord {
        id,
        thread_id: "legacy".into(),
        role: "user".into(),
        content: text.into(),
        item: None,
        created_at: 1,
        parent_entry_id,
    }
}

// Model an external historical writer only in test-owned SQLite. Production
// controls cannot append/replace history or mint bare links through State APIs.
fn seed_bare_link(store: &StateStore, thread: &str, runtime: &str) {
    rusqlite::Connection::open(store.db_path()).unwrap().execute(
        "INSERT INTO thread_runtime_links(thread_id,runtime_thread_id,created_at) VALUES(?1,?2,1)",
        rusqlite::params![thread,runtime],
    ).unwrap();
}

#[tokio::test]
async fn legacy_resolution_imports_all_branches_and_publishes_the_same_operation_once() {
    let (state, temp, fake, server) = fixture(false).await;
    let store = seed_archive(
        &state,
        "legacy",
        temp.path(),
        vec![
            archive_message(1, "root", None),
            archive_message(2, "old branch", Some(1)),
            archive_message(3, "new branch", Some(1)),
        ],
        None,
    )
    .await;
    let before = store.snapshot_legacy_thread_history("legacy").unwrap();
    let first = resolve(&state, "legacy", true).await.unwrap();
    let second = resolve(&state, "legacy", true).await.unwrap();
    assert_eq!(first, second);
    let calls = fake.calls.lock().await;
    let imports: Vec<_> = calls
        .iter()
        .filter(|(_, path, _)| path == "/v1/thread-history/import")
        .collect();
    assert_eq!(imports.len(), 1);
    assert_eq!(
        imports[0].2["history"],
        serde_json::to_value(&before).unwrap()
    );
    assert_eq!(imports[0].2["target_runtime_thread_id"], Value::Null);
    let receipt = store
        .get_canonical_runtime_link("legacy", state.captured_owner.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(receipt.operation_key, migration_key(&before).unwrap());
    assert_eq!(receipt.runtime_thread_id, "canonical-1");
    server.abort();
}

#[tokio::test]
async fn existing_bare_link_adopts_exact_target_without_empty_creation() {
    let (state, temp, fake, server) = fixture(false).await;
    let store = seed_archive(
        &state,
        "legacy",
        temp.path(),
        vec![archive_message(1, "retained source", None)],
        None,
    )
    .await;
    seed_bare_link(&store, "legacy", "canonical-1");
    resolve(&state, "legacy", true).await.unwrap();
    let calls = fake.calls.lock().await;
    let import = calls
        .iter()
        .find(|(_, p, _)| p == "/v1/thread-history/import")
        .unwrap();
    assert_eq!(import.2["target_runtime_thread_id"], "canonical-1");
    assert_eq!(
        import.2["history"]["messages"][0]["content"],
        "retained source"
    );
    assert!(
        !calls
            .iter()
            .any(|(m, p, _)| m == "POST" && p == "/v1/threads")
    );
    server.abort();
}

#[tokio::test]
async fn cancelled_waiter_cannot_drop_committed_alias_publication() {
    let (state, temp, fake, server) = fixture(true).await;
    let store = seed(&state, "legacy", temp.path()).await;
    let worker_state = state.clone();
    let waiter = tokio::spawn(async move { resolve(&worker_state, "legacy", true).await });
    fake.import_seen.notified().await;
    waiter.abort();
    fake.import_release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if store
                .get_canonical_runtime_link("legacy", state.captured_owner.as_ref().unwrap())
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        store.get_runtime_thread_link("legacy").unwrap().as_deref(),
        Some("canonical-1")
    );
    server.abort();
}

#[tokio::test]
async fn source_graph_change_during_import_refuses_publication_and_preserves_both_histories() {
    let (state, temp, fake, server) = fixture(true).await;
    let store = seed_archive(
        &state,
        "legacy",
        temp.path(),
        vec![archive_message(1, "first", None)],
        None,
    )
    .await;
    let worker_state = state.clone();
    let waiter = tokio::spawn(async move { resolve(&worker_state, "legacy", true).await });
    fake.import_seen.notified().await;
    let mut foreign = rusqlite::Connection::open(store.db_path()).unwrap();
    let transaction = foreign
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    transaction.execute("INSERT INTO messages(id,thread_id,role,content,item_json,created_at,parent_entry_id) VALUES(2,'legacy','user','changed while waiting',NULL,1,1)", []).unwrap();
    transaction
        .execute("UPDATE threads SET current_leaf_id=2 WHERE id='legacy'", [])
        .unwrap();
    transaction.commit().unwrap();
    fake.import_release.notify_one();
    let error = waiter.await.unwrap().unwrap_err();
    assert!(format!("{error:#}").contains("legacy history changed"));
    assert!(
        store
            .get_canonical_runtime_link("legacy", state.captured_owner.as_ref().unwrap())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .snapshot_legacy_thread_history("legacy")
            .unwrap()
            .messages
            .len(),
        2
    );
    assert_eq!(fake.record.lock().await["id"], "canonical-1");
    server.abort();
}

#[tokio::test]
async fn list_is_global_read_only_union_and_bound_alias_dedupes_actual_metadata() {
    let (state, temp, fake, server) = fixture(false).await;
    let store = seed(&state, "legacy", temp.path()).await;
    resolve(&state, "legacy", true).await.unwrap();
    let other = temp.path().join("other-workspace");
    seed(&state, "other-legacy", &other).await;
    fake.record.lock().await["title"] = json!("canonical title");
    let before =
        serde_json::to_value(store.snapshot_legacy_thread_history("legacy").unwrap()).unwrap();
    let result = handle(
        &state,
        ThreadRequest::List(codewhale_protocol::ThreadListParams {
            include_archived: false,
            limit: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(result.threads.len(), 2);
    assert!(
        result
            .threads
            .iter()
            .any(|t| t.id == "other-legacy" && t.cwd == other)
    );
    let alias = result.threads.iter().find(|t| t.id == "legacy").unwrap();
    assert_eq!(alias.name.as_deref(), Some("canonical title"));
    assert!(!result.threads.iter().any(|t| t.id == "canonical-1"));
    assert_eq!(
        serde_json::to_value(store.snapshot_legacy_thread_history("legacy").unwrap()).unwrap(),
        before
    );
    assert_eq!(
        fake.calls
            .lock()
            .await
            .iter()
            .filter(|(_, p, _)| p == "/v1/thread-history/import")
            .count(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn canonical_title_clear_and_archive_filter_ignore_stale_sqlite_names() {
    let (state, temp, fake, server) = fixture(false).await;
    let store = seed(&state, "legacy", temp.path()).await;
    resolve(&state, "legacy", true).await.unwrap();
    let clear = handle(
        &state,
        ThreadRequest::SetName(codewhale_protocol::ThreadSetNameParams {
            thread_id: "legacy".into(),
            name: String::new(),
        }),
    )
    .await
    .unwrap();
    assert!(clear.thread.unwrap().name.is_none());
    assert_eq!(fake.record.lock().await["title"], Value::Null);
    assert_eq!(
        store.get_thread("legacy").unwrap().unwrap().name.as_deref(),
        Some("old name")
    );
    handle(
        &state,
        ThreadRequest::Archive {
            thread_id: "legacy".into(),
        },
    )
    .await
    .unwrap();
    let active = list(
        &state,
        codewhale_protocol::ThreadListParams {
            include_archived: false,
            limit: None,
        },
    )
    .await
    .unwrap();
    assert!(active.threads.is_empty());
    let all = list(
        &state,
        codewhale_protocol::ThreadListParams {
            include_archived: true,
            limit: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(all.threads.len(), 1);
    assert_eq!(all.threads[0].status, ThreadStatus::Archived);
    server.abort();
}

#[tokio::test]
async fn read_returns_full_journal_including_inactive_branch() {
    let (state, _temp, _fake, server) = fixture(false).await;
    let value = handle(
        &state,
        ThreadRequest::Read(codewhale_protocol::ThreadReadParams {
            thread_id: "canonical-1".into(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        value.data["history"]["session"]["journal"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        value.data["history"]["session"]["journal"]["entries"][1]["id"],
        "inactive-branch"
    );
    server.abort();
}

#[tokio::test]
async fn unknown_canonical_target_never_creates_or_changes_an_alias() {
    let (state, _temp, fake, server) = fixture(false).await;
    let error = resolve(&state, "missing", true).await.unwrap_err();
    assert!(format!("{error:#}").contains("no replacement thread"));
    assert!(state.runtime_thread_map.lock().await.is_empty());
    assert!(!fake.calls.lock().await.iter().any(|(m, _, _)| m == "POST"));
    server.abort();
}

#[tokio::test]
async fn unreceipted_goal_deltas_and_unqualified_controls_are_refused_before_io() {
    let (state, _temp, fake, server) = fixture(false).await;
    for request in [
        ThreadRequest::GoalRecordProgress(codewhale_protocol::ThreadGoalProgressParams {
            thread_id: "canonical-1".into(),
            token_delta: 1,
            time_delta_seconds: 1,
            record_continuation: true,
        }),
        ThreadRequest::Create {
            metadata: Value::Null,
        },
    ] {
        assert!(handle(&state, request).await.is_err());
    }
    assert!(fake.calls.lock().await.is_empty());
    server.abort();
}

#[tokio::test]
async fn keyed_start_carries_exact_captured_config_and_never_opens_a_legacy_writer() {
    let (state, temp, fake, server) = fixture(false).await;
    let request: ThreadRequest = serde_json::from_value(json!({"kind":"start",
        "operation_key":"start-intent","model":"fixture-model",
        "model_provider":"fixture-account","cwd":temp.path(),"persist_extended_history":true}))
    .unwrap();
    let result = handle(&state, request).await.unwrap();
    assert_eq!(result.status, "started");
    assert_eq!(result.thread_id, "canonical-1");
    assert_eq!(result.data["receipt"]["operation_key"], "start-intent");
    let operations = fake.operations.lock().await;
    assert_eq!(operations.len(), 1);
    let (body, _) = operations.get("start-intent").unwrap();
    assert_eq!(
        body["mutation"]["config"],
        json!({"workspace":temp.path(),
        "model":"fixture-model","model_provider":"fixture-account"})
    );
    assert_eq!(body["expected_data_dir"], json!(fake.owner.data_dir));
    assert_eq!(body["expected_execution_scope"], fake.owner.execution_scope);
    let store = state.runtime.read().await.state_store().clone();
    assert!(
        store
            .list_threads(codewhale_state::ThreadListFilters {
                include_archived: true,
                limit: None
            })
            .unwrap()
            .is_empty()
    );
    server.abort();
}

#[tokio::test]
async fn create_retry_keeps_exact_client_intent_and_returns_same_owner_receipt() {
    let (state, temp, fake, server) = fixture(false).await;
    let request = ThreadRequest::Create {
        metadata: json!({"operation_key":"create-intent",
        "workspace":temp.path(),"model":"fixture-model","allowed_tools":[],"allow_shell":false}),
    };
    let first = handle(&state, request.clone()).await.unwrap();
    let second = handle(&state, request).await.unwrap();
    assert_eq!(first.thread_id, second.thread_id);
    assert_eq!(first.data["receipt"], second.data["receipt"]);
    assert_eq!(fake.operations.lock().await.len(), 1);
    let calls = fake.calls.lock().await;
    let requests: Vec<_> = calls
        .iter()
        .filter(|(_, path, _)| path == "/v1/thread-history/mutate")
        .map(|(_, _, body)| body)
        .collect();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        calls
            .iter()
            .filter(|(_, path, _)| path.ends_with("/operations/lookup"))
            .count(),
        2
    );
    assert!(
        requests[0]["mutation"]["config"]
            .get("operation_key")
            .is_none()
    );
    server.abort();
}

#[tokio::test]
async fn creation_without_acknowledged_scope_or_stable_key_refuses_before_io() {
    let (mut state, temp, fake, server) = fixture(false).await;
    for input in [
        json!({"kind":"start"}),
        json!({"kind":"create","metadata":{"operation_key":"wrong-scope","workspace":temp.path().join("other")}}),
    ] {
        assert!(
            handle(&state, serde_json::from_value(input).unwrap())
                .await
                .is_err()
        );
    }
    state.frontend_workspace = None;
    let request =
        serde_json::from_value(json!({"kind":"start","operation_key":"no-default"})).unwrap();
    assert!(
        handle(&state, request)
            .await
            .unwrap_err()
            .message
            .contains("acknowledged workspace")
    );
    assert!(fake.calls.lock().await.is_empty());
    server.abort();
}

#[tokio::test]
async fn ordinary_resume_binds_full_document_digest_and_preserves_public_alias() {
    let (state, temp, fake, server) = fixture(false).await;
    let store = seed(&state, "legacy", temp.path()).await;
    resolve(&state, "legacy", true).await.unwrap();
    let before = store.snapshot_legacy_thread_history("legacy").unwrap();
    let request = serde_json::from_value(json!({"kind":"resume","thread_id":"legacy",
        "operation_key":"resume-intent","persist_extended_history":true}))
    .unwrap();
    let result = handle(&state, request).await.unwrap();
    assert_eq!(result.thread_id, "legacy");
    assert_eq!(result.status, "resumed");
    let operations = fake.operations.lock().await;
    assert_eq!(
        operations["resume-intent"].0["mutation"]["source"],
        json!({"kind":"thread",
        "runtime_thread_id":"canonical-1","expected_document_digest":"b".repeat(64)})
    );
    assert_eq!(
        serde_json::to_value(store.snapshot_legacy_thread_history("legacy").unwrap()).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    server.abort();
}

#[tokio::test]
async fn ordinary_fork_returns_canonical_identity_and_preserves_original_branches() {
    let (state, temp, fake, server) = fixture(false).await;
    let store = seed(&state, "legacy", temp.path()).await;
    resolve(&state, "legacy", true).await.unwrap();
    let before = store.snapshot_legacy_thread_history("legacy").unwrap();
    let request = serde_json::from_value(json!({"kind":"fork","thread_id":"legacy",
        "operation_key":"fork-intent"}))
    .unwrap();
    let result = handle(&state, request).await.unwrap();
    assert_eq!(result.thread_id, "canonical-fork");
    assert_eq!(result.status, "forked");
    assert_eq!(result.data["receipt"]["session_id"], "session-fork");
    assert_eq!(fake.record.lock().await["id"], "canonical-1");
    assert_eq!(
        store.get_runtime_thread_link("legacy").unwrap().as_deref(),
        Some("canonical-1")
    );
    assert_eq!(
        serde_json::to_value(store.snapshot_legacy_thread_history("legacy").unwrap()).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    server.abort();
}

#[tokio::test]
async fn confirmed_mutation_metadata_404_keeps_completed_effect_and_retry_identity() {
    let (state, _temp, fake, server) = fixture(false).await;
    fake.missing_after_commit
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let request =
        serde_json::from_value(json!({"kind":"start","operation_key":"completed-intent"})).unwrap();
    let error = handle(&state, request).await.unwrap_err();
    assert_eq!(fake.operations.lock().await.len(), 1);
    assert!(error.message.contains("completed-intent"));
    assert!(
        error
            .message
            .contains("completed as thread canonical-1 / session session-1")
    );
    assert!(error.message.contains("without replay"));
    assert_ne!(
        error.code,
        JsonRpcError::thread_not_found("canonical-1").code
    );
    server.abort();
}

#[tokio::test]
async fn encoded_oversize_creation_refuses_before_owner_effect() {
    let (state, _temp, fake, server) = fixture(false).await;
    let request = ThreadRequest::Create {
        metadata: json!({"operation_key":"oversize-intent",
        "system_prompt":"x".repeat(MAX_CANONICAL_HISTORY_BYTES)}),
    };
    let error = handle(&state, request).await.unwrap_err();
    assert!(
        error
            .message
            .contains("encoded canonical control exceeds bound")
    );
    assert!(fake.operations.lock().await.is_empty());
    assert!(
        fake.calls
            .lock()
            .await
            .iter()
            .all(|(_, path, _)| path.ends_with("/operations/lookup"))
    );
    server.abort();
}

#[tokio::test]
async fn declared_oversize_response_refuses_complete_document_without_truncation() {
    crate::install_test_crypto_provider();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/large",
        get(|| async {
            (
                [(header::CONTENT_TYPE, "application/json")],
                format!("\"{}\"", "x".repeat(MAX_CANONICAL_HISTORY_BYTES)),
            )
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let response = reqwest::get(format!("http://{address}/large"))
        .await
        .unwrap();
    let error = read_json_response(response).await.unwrap_err();
    assert!(format!("{error:#}").contains("complete-document bound"));
    server.abort();
}

#[tokio::test]
async fn http_and_stdio_dispatch_share_the_same_full_history_owner() {
    let (state, _temp, fake, server) = fixture(false).await;
    let request = ThreadRequest::Read(codewhale_protocol::ThreadReadParams {
        thread_id: "canonical-1".into(),
    });
    let http = thread_handler(State(state.clone()), Json(request.clone())).await;
    assert_eq!(http.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(http.into_body(), MAX_CANONICAL_HISTORY_BYTES)
        .await
        .unwrap();
    let http: ThreadResponse = serde_json::from_slice(&bytes).unwrap();
    let stdio = dispatch_stdio_request(
        &state,
        "thread/request",
        serde_json::to_value(request).unwrap(),
    )
    .await
    .unwrap();
    let stdio: ThreadResponse = serde_json::from_value(stdio.result).unwrap();
    assert_eq!(http.data["history"], stdio.data["history"]);
    assert_eq!(
        fake.calls
            .lock()
            .await
            .iter()
            .filter(|(_, path, _)| path.ends_with("/history"))
            .count(),
        2
    );
    server.abort();
}

#[tokio::test]
async fn completed_resume_retry_reads_original_operation_before_changed_history() {
    let (state, _temp, fake, server) = fixture(false).await;
    let request: ThreadRequest =
        serde_json::from_value(json!({"kind":"resume","thread_id":"canonical-1",
        "operation_key":"resume-recovery"}))
        .unwrap();
    let first = handle(&state, request.clone()).await.unwrap();
    *fake.document_digest.lock().await = "c".repeat(64);
    let count = fake.calls.lock().await.len();
    let second = handle(&state, request).await.unwrap();
    assert_eq!(first.data["receipt"], second.data["receipt"]);
    let calls = fake.calls.lock().await;
    assert!(
        calls[count..]
            .iter()
            .all(|(_, path, _)| path.ends_with("/operations/lookup")
                || path == "/v1/threads/canonical-1")
    );
    assert_eq!(
        calls
            .iter()
            .filter(|(_, path, _)| path == "/v1/thread-history/mutate")
            .count(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn pending_retained_operation_never_reconstructs_or_replays_source() {
    let (state, _temp, fake, server) = fixture(false).await;
    let request: ThreadRequest =
        serde_json::from_value(json!({"kind":"resume","thread_id":"canonical-1",
        "operation_key":"pending-recovery"}))
        .unwrap();
    handle(&state, request.clone()).await.unwrap();
    fake.pending_operation
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let count = fake.calls.lock().await.len();
    let error = handle(&state, request).await.unwrap_err();
    assert!(
        error.message.contains("pending-recovery")
            && error.message.contains("pending")
            && error.message.contains("no resume, replay or replacement")
    );
    let calls = fake.calls.lock().await;
    assert_eq!(calls.len(), count + 2);
    assert!(calls[count].1.ends_with("/operations/lookup"));
    assert!(calls[count + 1].1.ends_with("/operations/recover"));
    assert_eq!(calls[count + 1].2["operation"], calls[count].2);
    assert!(
        calls[count..]
            .iter()
            .all(|(_, path, _)| !path.ends_with("/history") && !path.ends_with("/mutate"))
    );
    server.abort();
}

#[tokio::test]
async fn prepared_pending_resume_settles_reserved_receipt_without_reconstructing_source() {
    let (state, _temp, fake, server) = fixture(false).await;
    let request: ThreadRequest =
        serde_json::from_value(json!({"kind":"resume","thread_id":"canonical-1",
        "operation_key":"prepared-recovery"}))
        .unwrap();
    let first = handle(&state, request.clone()).await.unwrap();
    fake.pending_operation
        .store(true, std::sync::atomic::Ordering::SeqCst);
    fake.recovery_prepared
        .store(true, std::sync::atomic::Ordering::SeqCst);
    *fake.document_digest.lock().await = "c".repeat(64);
    let count = fake.calls.lock().await.len();
    let second = handle(&state, request.clone()).await.unwrap();
    assert_eq!(first.data["receipt"], second.data["receipt"]);
    let calls = fake.calls.lock().await;
    assert_eq!(calls[count].1, "/v1/thread-history/operations/lookup");
    assert_eq!(calls[count + 1].1, "/v1/thread-history/operations/recover");
    assert_eq!(
        calls[count + 1].2["association"]["source_runtime_thread_id"],
        "canonical-1"
    );
    assert!(
        calls[count..]
            .iter()
            .all(|(_, path, _)| !path.ends_with("/history") && !path.ends_with("/mutate"))
    );
    drop(calls);
    let again = handle(&state, request).await.unwrap();
    assert_eq!(again.data["receipt"], first.data["receipt"]);
    assert_eq!(
        fake.calls
            .lock()
            .await
            .iter()
            .filter(|(_, path, _)| path.ends_with("/recover"))
            .count(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn pending_wrong_action_refuses_before_any_prepared_recovery() {
    let (state, _temp, fake, server) = fixture(false).await;
    let create =
        serde_json::from_value(json!({"kind":"start","operation_key":"wrong-pending-action"}))
            .unwrap();
    handle(&state, create).await.unwrap();
    fake.pending_operation
        .store(true, std::sync::atomic::Ordering::SeqCst);
    fake.recovery_prepared
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let count = fake.calls.lock().await.len();
    let request = serde_json::from_value(
        json!({"kind":"fork","thread_id":"canonical-1","operation_key":"wrong-pending-action"}),
    )
    .unwrap();
    let error = handle(&state, request).await.unwrap_err();
    assert!(
        error.message.contains("another action or source")
            && error.message.contains("wrong-pending-action")
    );
    let calls = fake.calls.lock().await;
    assert_eq!(calls.len(), count + 1);
    assert!(calls[count].1.ends_with("/lookup"));
    assert!(
        fake.pending_operation
            .load(std::sync::atomic::Ordering::SeqCst)
    );
    server.abort();
}

#[tokio::test]
async fn changed_prepared_target_refusal_keeps_reserved_operation_and_no_replay() {
    let (state, _temp, fake, server) = fixture(false).await;
    let request: ThreadRequest =
        serde_json::from_value(json!({"kind":"resume","thread_id":"canonical-1",
        "operation_key":"changed-prepared-target"}))
        .unwrap();
    handle(&state, request.clone()).await.unwrap();
    fake.pending_operation
        .store(true, std::sync::atomic::Ordering::SeqCst);
    fake.recovery_refusal
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let count = fake.calls.lock().await.len();
    let error = handle(&state, request).await.unwrap_err();
    assert!(
        error.message.contains("changed-prepared-target")
            && error.message.contains("canonical-1 / session session-1")
            && error.message.contains("prepared target graph changed")
    );
    let calls = fake.calls.lock().await;
    assert_eq!(calls.len(), count + 2);
    assert!(
        calls[count..]
            .iter()
            .all(|(_, path, _)| path.ends_with("/lookup") || path.ends_with("/recover"))
    );
    assert_eq!(fake.operations.lock().await.len(), 1);
    server.abort();
}

#[tokio::test]
async fn retained_operation_rejects_another_action_or_source_with_completed_facts() {
    let (state, _temp, fake, server) = fixture(false).await;
    let create: ThreadRequest =
        serde_json::from_value(json!({"kind":"start","operation_key":"action-bound"})).unwrap();
    handle(&state, create).await.unwrap();
    let request = serde_json::from_value(json!({"kind":"fork","thread_id":"canonical-1",
        "operation_key":"action-bound"}))
    .unwrap();
    let error = handle(&state, request).await.unwrap_err();
    assert!(
        error
            .message
            .contains("completed as thread canonical-1 / session session-1")
    );
    assert!(error.message.contains("another action or source"));
    assert_eq!(fake.operations.lock().await.len(), 1);
    server.abort();
}

#[tokio::test]
async fn resume_options_are_forwarded_exactly_to_the_owning_decoder() {
    let (state, temp, fake, server) = fixture(false).await;
    let history = json!([{"role":"user","content":"offered"}]);
    let config = json!({"allow_shell":false,"mode":"ask","system_prompt":"captured override"});
    let source_path = temp.path().join("sessions/session-1.json");
    let request = serde_json::from_value(json!({"kind":"resume","thread_id":"canonical-1",
        "operation_key":"options-intent","history":history,"path":source_path,
        "model":"fixture-model","model_provider":"fixture-account","cwd":temp.path(),
        "approval_policy":"on-request","sandbox":"captured-ceiling","config":config,
        "base_instructions":"base","developer_instructions":"developer","personality":"brief",
        "persist_extended_history":false}))
    .unwrap();
    handle(&state, request).await.unwrap();
    let operations = fake.operations.lock().await;
    let options = &operations["options-intent"].0["mutation"]["options"];
    assert_eq!(options["offered_history"], history);
    assert_eq!(options["source_path"], json!(source_path));
    assert!(options.get("expected_session_goal_digest").is_none());
    assert_eq!(
        options["overrides"],
        json!({"model":"fixture-model","model_provider":"fixture-account",
        "cwd":temp.path(),"approval_policy":"on-request","sandbox":"captured-ceiling","config":config,
        "base_instructions":"base","developer_instructions":"developer","personality":"brief"})
    );
    server.abort();
}

#[tokio::test]
async fn fork_reads_other_workspace_source_but_uses_acknowledged_target_scope() {
    let (state, temp, fake, server) = fixture(false).await;
    let original = temp.path().join("original");
    fake.record.lock().await["workspace"] = json!(original);
    let request = serde_json::from_value(json!({"kind":"fork","thread_id":"canonical-1",
        "operation_key":"cross-workspace-fork","cwd":temp.path()}))
    .unwrap();
    let response = handle(&state, request).await.unwrap();
    assert_eq!(response.cwd.as_deref(), Some(temp.path()));
    assert_eq!(
        fake.operations.lock().await["cross-workspace-fork"].0["workspace"],
        json!(temp.path())
    );
    assert_eq!(fake.record.lock().await["workspace"], json!(original));
    let request = serde_json::from_value(json!({"kind":"resume","thread_id":"canonical-1",
        "operation_key":"cross-workspace-resume","cwd":temp.path()}))
    .unwrap();
    assert!(handle(&state, request).await.is_err());
    assert_eq!(fake.operations.lock().await.len(), 1);
    server.abort();
}

#[tokio::test]
async fn bridged_turn_sends_the_observed_owner_workspace_as_final_narrowing() {
    let (state, temp, fake, server) = fixture(false).await;
    run_http_thread_message(
        &state,
        "canonical-1".into(),
        "hello".into(),
        Some(Vec::new()),
        Vec::new(),
        None,
    )
    .await
    .unwrap();
    let calls = fake.calls.lock().await;
    let turn = calls
        .iter()
        .find(|(_, path, _)| path.ends_with("/turns"))
        .unwrap();
    assert_eq!(turn.2["expected_workspace"], json!(temp.path()));
    server.abort();
}

fn legacy_goal_record(status: &str, objective: &str) -> codewhale_state::ThreadGoalRecord {
    serde_json::from_value(
        json!({"thread_id":"legacy","goal_id":"old-goal","objective":objective,
        "status":status,"token_budget":1000,"tokens_used":137,"time_used_seconds":23,
        "continuation_count":2,"created_at":1,"updated_at":2,"repeated_gap_count":0}),
    )
    .unwrap()
}

#[tokio::test]
async fn legacy_goal_remains_visible_and_cannot_silently_restart_or_be_replaced() {
    let (state, temp, fake, server) = fixture(false).await;
    let goal = legacy_goal_record("active", "retained work");
    let store = seed_archive(&state, "legacy", temp.path(), Vec::new(), Some(goal)).await;
    let before = store.snapshot_legacy_thread_history("legacy").unwrap();
    assert_eq!(
        before.goal.as_ref().unwrap().status,
        codewhale_protocol::ThreadGoalStatus::Active
    );
    let request = ThreadRequest::GoalGet(codewhale_protocol::ThreadGoalGetParams {
        thread_id: "legacy".into(),
    });
    let result = handle(&state, request.clone()).await.unwrap();
    assert_eq!(result.status, "ok");
    let recovered = result.goal.unwrap();
    assert_eq!(recovered.thread_id, "legacy");
    assert_eq!(recovered.goal_id, "old-goal");
    assert_eq!(recovered.tokens_used, 137);
    assert_eq!(recovered.time_used_seconds, 23);
    assert_eq!(recovered.continuation_count, 2);
    assert_eq!(recovered.created_at, 1);
    assert_eq!(recovered.updated_at, 2);
    assert_eq!(recovered.token_budget, Some(1000));
    assert_eq!(
        recovered.status,
        codewhale_protocol::ThreadGoalStatus::Paused
    );
    assert_eq!(recovered.pause_reason, None);
    assert_eq!(
        serde_json::to_value(store.snapshot_legacy_thread_history("legacy").unwrap()).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    // Later owner progress is authoritative. A repeated alias read neither
    // imports the old goal again nor resets its real current progress.
    fake.goal.lock().await.as_mut().unwrap()["tokens_used"] = json!(151);
    let again = handle(&state, request).await.unwrap();
    assert_eq!(again.goal.unwrap().tokens_used, 151);
    let calls = fake.calls.lock().await;
    assert_eq!(
        calls
            .iter()
            .filter(|(_, p, _)| p.ends_with("/import"))
            .count(),
        1
    );
    assert!(
        calls
            .iter()
            .all(|(m, p, _)| !(p.ends_with("/turns") || p.ends_with("/goal") && m != "GET"))
    );
    server.abort();
}

#[tokio::test]
async fn changed_legacy_goal_during_import_refuses_alias_publication() {
    let (state, temp, fake, server) = fixture(true).await;
    let store = seed_archive(
        &state,
        "legacy",
        temp.path(),
        Vec::new(),
        Some(legacy_goal_record("paused", "original")),
    )
    .await;
    let worker_state = state.clone();
    let waiter = tokio::spawn(async move { resolve(&worker_state, "legacy", true).await });
    fake.import_seen.notified().await;
    rusqlite::Connection::open(store.db_path())
        .unwrap()
        .execute(
            "UPDATE thread_goals SET objective='changed during import' WHERE thread_id='legacy'",
            [],
        )
        .unwrap();
    fake.import_release.notify_one();
    let error = waiter.await.unwrap().unwrap_err();
    assert!(format!("{error:#}").contains("legacy history changed"));
    assert!(
        store
            .get_canonical_runtime_link("legacy", state.captured_owner.as_ref().unwrap())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.get_thread_goal("legacy").unwrap().unwrap().objective,
        "changed during import"
    );
    assert_eq!(
        fake.goal.lock().await.as_ref().unwrap()["objective"],
        "original"
    );
    server.abort();
}

#[tokio::test]
async fn conflicting_existing_owner_goal_is_visible_refusal_without_empty_alias() {
    let (state, temp, fake, server) = fixture(false).await;
    let store = seed_archive(
        &state,
        "legacy",
        temp.path(),
        Vec::new(),
        Some(legacy_goal_record("paused", "legacy objective")),
    )
    .await;
    let mut existing =
        serde_json::to_value(legacy_goal_record("paused", "owned objective")).unwrap();
    existing["thread_id"] = json!("canonical-1");
    existing["goal_id"] = json!("owned-goal");
    *fake.goal.lock().await = Some(existing.clone());
    let request = ThreadRequest::GoalGet(codewhale_protocol::ThreadGoalGetParams {
        thread_id: "legacy".into(),
    });
    let error = handle(&state, request).await.unwrap_err();
    assert!(error.message.contains("canonical goal conflict"));
    assert!(
        store
            .get_canonical_runtime_link("legacy", state.captured_owner.as_ref().unwrap())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.get_thread_goal("legacy").unwrap().unwrap().objective,
        "legacy objective"
    );
    assert_eq!(fake.goal.lock().await.as_ref().unwrap(), &existing);
    server.abort();
}
