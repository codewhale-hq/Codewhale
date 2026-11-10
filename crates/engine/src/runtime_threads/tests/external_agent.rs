use super::*;
use crate::fleet::agents::{FIXTURE_AGENT_ENV, FIXTURE_AGENT_ID};

fn fixture_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp-agent.mjs")
}

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn fixture_thread_request(workspace: &Path, posture: &str) -> CreateThreadRequest {
    CreateThreadRequest {
        workspace: Some(workspace.to_path_buf()),
        permission_posture: Some(posture.to_string()),
        executor: Some(CreateThreadExecutor {
            kind: "acp".to_string(),
            agent_id: FIXTURE_AGENT_ID.to_string(),
            config: CreateThreadExecutorConfig::default(),
        }),
        ..Default::default()
    }
}

fn prompt(text: &str) -> StartTurnRequest {
    StartTurnRequest {
        prompt: text.to_string(),
        ..Default::default()
    }
}

async fn wait_for_turn(manager: &RuntimeThreadManager, turn_id: &str) -> Result<TurnRecord> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let turn = manager.store.load_turn(turn_id)?;
        if !turn.status.is_active_work() {
            return Ok(turn);
        }
        anyhow::ensure!(Instant::now() < deadline, "turn {turn_id} never settled");
        sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_approval(
    manager: &RuntimeThreadManager,
    thread_id: &str,
) -> Result<PendingApprovalRequest> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(approval) = manager
            .get_thread_detail(thread_id)
            .await?
            .pending_approvals
            .into_iter()
            .next()
        {
            return Ok(approval);
        }
        anyhow::ensure!(Instant::now() < deadline, "no approval was requested");
        sleep(Duration::from_millis(50)).await;
    }
}

async fn events(
    manager: &RuntimeThreadManager,
    thread_id: &str,
) -> Result<Vec<RuntimeEventRecord>> {
    manager.events_since_async(thread_id, None).await
}

#[test]
fn executor_record_round_trips_and_legacy_threads_stay_native() -> Result<()> {
    let legacy = serde_json::to_value(sample_thread("thr_legacy"))?;
    assert!(
        legacy.get("executor").is_none(),
        "a native record writes no executor"
    );
    let reread: ThreadRecord = serde_json::from_value(legacy.clone())?;
    assert_eq!(reread.executor, None);

    let mut external = sample_thread("thr_external");
    external.executor = Some(ExecutorRecord {
        version: EXECUTOR_RECORD_VERSION,
        kind: EXECUTOR_KIND_ACP.to_string(),
        agent_id: "codex".to_string(),
        agent_name: Some("Codex".to_string()),
        agent_version: Some("2.5.0".to_string()),
        protocol_version: Some(1),
        foreign_session_id: Some("sess-1".to_string()),
        model: Some("gpt-5".to_string()),
    });
    let encoded = serde_json::to_string(&external)?;
    assert!(
        !encoded.contains("env"),
        "no environment travels in the record"
    );
    let decoded: ThreadRecord = serde_json::from_str(&encoded)?;
    assert_eq!(decoded, external);
    decoded.executor.as_ref().unwrap().validate()?;

    let mut minimal = legacy;
    minimal["executor"] = json!({ "kind": "acp", "agent_id": "opencode" });
    let minimal: ThreadRecord = serde_json::from_value(minimal)?;
    let record = minimal.executor.expect("executor parsed");
    assert_eq!(record.version, EXECUTOR_RECORD_VERSION);
    assert_eq!(record.foreign_session_id, None);

    let future = ExecutorRecord {
        version: EXECUTOR_RECORD_VERSION + 1,
        ..record.clone()
    };
    assert!(
        future.validate().is_err(),
        "a newer record is refused, not guessed"
    );
    let foreign = ExecutorRecord {
        kind: "sdk".to_string(),
        ..record
    };
    assert!(foreign.validate().is_err());
    Ok(())
}

#[tokio::test]
async fn an_unknown_or_missing_agent_cannot_create_a_thread() -> Result<()> {
    let _env = crate::test_support::lock_test_env();
    let _fixture = crate::test_support::EnvVarGuard::remove(FIXTURE_AGENT_ENV);
    let root = tempfile::tempdir()?;
    let manager = test_manager(root.path().join("runtime"))?;
    let error = manager
        .create_thread(fixture_thread_request(root.path(), "ask"))
        .await
        .expect_err("the fixture is not configured");
    assert!(error.to_string().contains("fixture"), "{error}");
    let mut request = fixture_thread_request(root.path(), "ask");
    request.executor.as_mut().unwrap().agent_id = "../../bin/sh".to_string();
    assert!(manager.create_thread(request).await.is_err());
    let mut request = fixture_thread_request(root.path(), "ask");
    request.executor.as_mut().unwrap().kind = "shell".to_string();
    assert!(manager.create_thread(request).await.is_err());
    Ok(())
}

#[tokio::test]
async fn a_fixture_agent_turn_runs_end_to_end_and_never_auto_approves() -> Result<()> {
    if !node_available() {
        return Ok(());
    }
    let _env = crate::test_support::lock_test_env();
    let _fixture = crate::test_support::EnvVarGuard::set(FIXTURE_AGENT_ENV, fixture_script());
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("work");
    std::fs::create_dir_all(&workspace)?;
    let manager = test_manager(root.path().join("runtime"))?;
    let thread = manager
        .create_thread(fixture_thread_request(&workspace, "ask"))
        .await?;
    assert_eq!(thread.model_provider, None);
    assert_eq!(
        thread
            .executor
            .as_ref()
            .map(|record| record.agent_id.as_str()),
        Some(FIXTURE_AGENT_ID)
    );

    let turn = manager
        .start_turn(&thread.id, prompt("hello agent"))
        .await?;
    let approval = wait_for_approval(&manager, &thread.id).await?;
    sleep(Duration::from_millis(300)).await;
    assert!(
        !workspace.join("fixture.txt").exists(),
        "the edit waits for the person under Ask"
    );
    assert_eq!(
        manager.store.load_turn(&turn.id)?.status,
        RuntimeTurnStatus::InProgress
    );
    assert!(
        manager
            .deliver_approval_from(
                &approval.id,
                Some(&ExternalApprovalBinding {
                    thread_id: thread.id.clone(),
                    turn_id: turn.id.clone(),
                    tool_call_id: approval.tool_call_id.clone(),
                }),
                ExternalApprovalDecision::Allow { remember: false },
                Some(ApprovalResponder::Desktop),
            )
            .await?
    );
    let settled = wait_for_turn(&manager, &turn.id).await?;
    assert_eq!(settled.status, RuntimeTurnStatus::Completed, "{settled:?}");
    assert!(workspace.join("fixture.txt").exists());

    let detail = manager.get_thread_detail(&thread.id).await?;
    let message = detail
        .items
        .iter()
        .find(|item| item.kind == TurnItemKind::AgentMessage)
        .expect("agent message item");
    assert!(
        message
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("hello agent"),
        "{message:?}"
    );
    assert!(
        detail
            .items
            .iter()
            .any(|item| item.kind == TurnItemKind::AgentReasoning)
    );
    let edit = detail
        .items
        .iter()
        .find(|item| item.kind == TurnItemKind::FileChange)
        .expect("file change item");
    assert_eq!(edit.status, TurnItemLifecycleStatus::Completed);
    assert!(
        detail
            .items
            .iter()
            .all(|item| !item.status.eq(&TurnItemLifecycleStatus::InProgress)),
        "every item is terminal"
    );
    let session = detail
        .thread
        .executor
        .as_ref()
        .and_then(|record| record.foreign_session_id.clone())
        .expect("the agent's session id is recorded");
    assert!(session.starts_with("fixture-session-"));

    let events = events(&manager, &thread.id).await?;
    let names: Vec<&str> = events.iter().map(|event| event.event.as_str()).collect();
    for expected in [
        "thread.started",
        "turn.started",
        "item.delta",
        "approval.required",
        "approval.decided",
        "turn.completed",
    ] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
    let decided = events
        .iter()
        .find(|event| event.event == "approval.decided")
        .unwrap();
    assert_eq!(decided.payload["decided_by"], "user");
    assert_eq!(decided.payload["option_id"], "allow");
    assert_eq!(decided.payload["responder"], "desktop");

    let dumped = std::fs::read_to_string(workspace.join("fixture-env.json"))?;
    assert!(
        !dumped.contains(FIXTURE_AGENT_ENV),
        "Codewhale variables stay home"
    );

    std::fs::remove_file(workspace.join("fixture.txt"))?;
    let denied = manager.start_turn(&thread.id, prompt("again")).await?;
    let approval = wait_for_approval(&manager, &thread.id).await?;
    assert!(
        manager
            .deliver_external_approval(
                &approval.id,
                ExternalApprovalDecision::Deny { remember: false }
            )
            .await?
    );
    let settled = wait_for_turn(&manager, &denied.id).await?;
    assert_eq!(settled.status, RuntimeTurnStatus::Completed);
    assert!(
        !workspace.join("fixture.txt").exists(),
        "a denied edit wrote nothing"
    );

    let mut constrained = prompt("disable tools");
    constrained.allowed_tools = Some(Vec::new());
    assert!(manager.start_turn(&thread.id, constrained).await.is_err());
    let mut misplaced = prompt("run remotely");
    misplaced.environment_id = Some("env_remote".to_string());
    assert!(manager.start_turn(&thread.id, misplaced).await.is_err());
    let mut foreign_model = fixture_thread_request(&workspace, "ask");
    foreign_model.model_provider = Some("openai".to_string());
    assert!(manager.create_thread(foreign_model).await.is_err());

    let interrupted = manager.start_turn(&thread.id, prompt("stop me")).await?;
    wait_for_approval(&manager, &thread.id).await?;
    manager.interrupt_turn(&thread.id, &interrupted.id).await?;
    let settled = wait_for_turn(&manager, &interrupted.id).await?;
    assert_eq!(settled.status, RuntimeTurnStatus::Interrupted);
    assert!(!workspace.join("fixture.txt").exists());
    assert!(
        manager
            .get_thread_detail(&thread.id)
            .await?
            .pending_approvals
            .is_empty()
    );

    let shutdown_turn = manager
        .start_turn(&thread.id, prompt("shutdown while waiting"))
        .await?;
    wait_for_approval(&manager, &thread.id).await?;
    tokio::time::timeout(Duration::from_secs(30), manager.shutdown_and_wait()).await??;
    assert_eq!(
        manager.store.load_turn(&shutdown_turn.id)?.status,
        RuntimeTurnStatus::Interrupted
    );
    drop(manager);

    let reopened = test_manager(root.path().join("runtime"))?;
    let resumed = reopened.resume_thread(&thread.id).await?;
    assert_eq!(
        resumed
            .executor
            .and_then(|record| record.foreign_session_id),
        Some(session.clone()),
        "resume reopens the same agent session"
    );
    let after = reopened
        .start_turn(&thread.id, prompt("after restart"))
        .await?;
    let approval = wait_for_approval(&reopened, &thread.id).await?;
    reopened
        .deliver_external_approval(
            &approval.id,
            ExternalApprovalDecision::Allow { remember: false },
        )
        .await?;
    let settled = wait_for_turn(&reopened, &after.id).await?;
    assert_eq!(settled.status, RuntimeTurnStatus::Completed);
    let detail = reopened.get_thread_detail(&thread.id).await?;
    assert!(
        detail.items.iter().all(|item| !item
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("replayed")),
        "a reloaded session's replay never duplicates history"
    );
    assert!(
        reopened
            .compact_thread(&thread.id, Default::default())
            .await
            .is_err()
    );
    reopened.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn external_postures_allow_only_complete_in_workspace_edit_requests() -> Result<()> {
    if !node_available() {
        return Ok(());
    }
    let _env = crate::test_support::lock_test_env();
    let _fixture = crate::test_support::EnvVarGuard::set(FIXTURE_AGENT_ENV, fixture_script());
    let root = tempfile::tempdir()?;
    let manager = test_manager(root.path().join("runtime"))?;

    let full = root.path().join("full");
    std::fs::create_dir_all(&full)?;
    let thread = manager
        .create_thread(fixture_thread_request(&full, "full_access"))
        .await?;
    let turn = manager.start_turn(&thread.id, prompt("go")).await?;
    assert_eq!(
        wait_for_turn(&manager, &turn.id).await?.status,
        RuntimeTurnStatus::Completed
    );
    assert!(full.join("fixture.txt").exists());
    let decided = events(&manager, &thread.id)
        .await?
        .into_iter()
        .find(|event| event.event == "approval.decided")
        .expect("the automatic decision is recorded");
    assert_eq!(decided.payload["auto"], true);
    assert_eq!(decided.payload["posture"], "full_access");

    let plan = root.path().join("plan");
    std::fs::create_dir_all(&plan)?;
    let mut request = fixture_thread_request(&plan, "ask");
    request.mode = Some("plan".to_string());
    let thread = manager.create_thread(request).await?;
    let turn = manager.start_turn(&thread.id, prompt("go")).await?;
    assert_eq!(
        wait_for_turn(&manager, &turn.id).await?.status,
        RuntimeTurnStatus::Completed
    );
    assert!(
        !plan.join("fixture.txt").exists(),
        "Plan mode never lets an agent edit"
    );
    let source = std::fs::read_to_string(fixture_script())?;
    let original = "locations: [{ path: join(cwd, 'fixture.txt') }]";
    assert!(source.contains(original));
    for (name, locations, asks) in [
        ("complete", original, false),
        (
            "overflow",
            "locations: [...Array.from({length: 32}, () => ({path: join(cwd, 'fixture.txt')})), {path: join(cwd, '..', 'outside.txt')}]",
            true,
        ),
        (
            "malformed",
            "locations: [{path: join(cwd, 'fixture.txt')}, {}]",
            true,
        ),
    ] {
        let script = root.path().join(format!("{name}.mjs"));
        std::fs::write(&script, source.replace(original, locations))?;
        let _variant = crate::test_support::EnvVarGuard::set(FIXTURE_AGENT_ENV, &script);
        let workspace = root.path().join(name);
        std::fs::create_dir_all(&workspace)?;
        let thread = manager
            .create_thread(fixture_thread_request(&workspace, "accept_edits"))
            .await?;
        let turn = manager.start_turn(&thread.id, prompt("edit")).await?;
        if asks {
            let approval = wait_for_approval(&manager, &thread.id).await?;
            assert!(
                !workspace.join("fixture.txt").exists(),
                "{name} auto-allowed incomplete locations"
            );
            assert!(
                manager
                    .deliver_external_approval(
                        &approval.id,
                        ExternalApprovalDecision::Deny { remember: false }
                    )
                    .await?
            );
        }
        assert_eq!(
            wait_for_turn(&manager, &turn.id).await?.status,
            RuntimeTurnStatus::Completed
        );
        assert_eq!(workspace.join("fixture.txt").exists(), !asks, "{name}");
    }
    manager.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn a_session_in_another_environment_or_engine_is_refused_not_run_here() -> Result<()> {
    use crate::fleet::agents::ExecutorUnavailable;
    let _env = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let manager = test_manager(root.path().join("runtime"))?;

    let mut placed = sample_thread("thr_placed_elsewhere");
    placed.workspace = root.path().to_path_buf();
    placed.environment = Some(ThreadEnvironment {
        id: "env_1".to_string(),
        provider_kind: "runtime".to_string(),
    });
    manager.store.save_thread(&placed)?;
    let error = manager
        .start_turn(&placed.id, prompt("run here?"))
        .await
        .expect_err("a placed session never runs locally");
    assert_eq!(
        error
            .downcast_ref::<ExecutorUnavailable>()
            .map(|refusal| refusal.code),
        Some(ExecutorUnavailable::ENVIRONMENT_NOT_SUPPORTED)
    );
    assert!(manager.store.list_turns_for_thread(&placed.id)?.is_empty());
    let error = manager
        .compact_thread(&placed.id, CompactThreadRequest::default())
        .await
        .expect_err("compaction cannot load a remote environment locally");
    assert_eq!(
        error
            .downcast_ref::<ExecutorUnavailable>()
            .map(|refusal| refusal.code),
        Some(ExecutorUnavailable::ENVIRONMENT_NOT_SUPPORTED)
    );

    assert!(
        manager
            .thread_shell_manager(&placed.id, true)
            .await
            .is_err()
    );
    assert!(manager.shell_managers_snapshot().await.is_empty());

    let request = CreateThreadRequest {
        workspace: Some(root.path().to_path_buf()),
        environment: placed.environment.clone(),
        ..Default::default()
    };
    let error = manager.create_thread(request).await.unwrap_err();
    assert!(error.downcast_ref::<ExecutorUnavailable>().is_some());

    let mut request = fixture_thread_request(root.path(), "ask");
    request.executor.as_mut().unwrap().kind = EXECUTOR_KIND_ENGINE.to_string();
    let error = manager.create_thread(request).await.unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<ExecutorUnavailable>()
            .map(|refusal| refusal.code),
        Some(ExecutorUnavailable::EXECUTOR_NOT_SUPPORTED)
    );
    let record: ExecutorRecord =
        serde_json::from_value(json!({ "kind": "engine", "agent_id": "env_1" }))?;
    assert!(
        record.validate().is_err(),
        "the reserved kind parses and is refused"
    );
    Ok(())
}

#[tokio::test]
async fn duplicate_external_answers_wait_for_the_same_durable_settlement() -> Result<()> {
    let root = tempfile::tempdir()?;
    let manager = test_manager(root.path().join("runtime"))?;
    let (id, decision, settled) = manager.register_agent_approval(
        "thread",
        PendingApprovalRequest {
            id: String::new(),
            turn_id: "turn".to_string(),
            tool_name: "fixture:edit".to_string(),
            input: None,
            description: "Write fixture".to_string(),
            intent_summary: None,
            tool_call_id: Some("call".to_string()),
            summary: None,
        },
    );
    let first_manager = manager.clone();
    let first_id = id.clone();
    let answer = ExternalApprovalDecision::Allow { remember: false };
    let first = tokio::spawn(async move {
        first_manager
            .deliver_approval_from(&first_id, None, answer, Some(ApprovalResponder::Desktop))
            .await
    });
    assert_eq!(decision.await?, answer);
    let second = manager.deliver_approval_from(&id, None, answer, Some(ApprovalResponder::Phone));
    tokio::pin!(second);
    tokio::select! {
        biased;
        result = &mut second => bail!("duplicate approval acknowledged before settlement: {result:?}"),
        _ = std::future::ready(()) => {}
    }
    assert!(!first.is_finished());
    assert_eq!(
        manager
            .pending_approvals
            .lock()
            .get(&id)
            .and_then(|entry| entry.responder),
        Some(ApprovalResponder::Desktop)
    );
    settled.send(Some(true))?;
    assert!(first.await??);
    assert!(second.await?);
    manager.cancel_pending_approval(&id);
    manager.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn interrupt_during_external_persistence_never_dispatches_prompt() -> Result<()> {
    if !node_available() {
        return Ok(());
    }
    let _env = crate::test_support::lock_test_env();
    let _fixture = crate::test_support::EnvVarGuard::set(FIXTURE_AGENT_ENV, fixture_script());
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("work");
    std::fs::create_dir_all(&workspace)?;
    let manager = test_manager(root.path().join("runtime"))?;
    let thread = manager
        .create_thread(fixture_thread_request(&workspace, "full_access"))
        .await?;
    manager.resume_thread(&thread.id).await?;
    let store = manager.store.clone();
    let (locked, ready) = oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let blocked = tokio::task::spawn_blocking(move || {
        let _mutation = store.thread_mutation.lock();
        let _ = locked.send(());
        let _ = released.recv_timeout(Duration::from_secs(30));
    });
    ready.await?;
    let starting_manager = manager.clone();
    let starting_thread = thread.id.clone();
    let starting = tokio::spawn(async move {
        starting_manager
            .start_turn(&starting_thread, prompt("cancel before dispatch"))
            .await
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let turn = loop {
        if let Some(turn) = manager.store.list_turns_for_thread(&thread.id)?.pop() {
            break turn;
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "turn preparation never persisted"
        );
        sleep(Duration::from_millis(10)).await;
    };
    assert!(!starting.is_finished());
    manager.interrupt_turn(&thread.id, &turn.id).await?;
    release.send(())?;
    blocked.await?;
    starting.await??;
    assert_eq!(
        wait_for_turn(&manager, &turn.id).await?.status,
        RuntimeTurnStatus::Interrupted
    );
    assert!(!workspace.join("fixture.txt").exists());
    let events = events(&manager, &thread.id).await?;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.event.as_str(), "approval.required" | "item.delta")),
        "cancelled preparation reached the agent: {events:?}"
    );
    manager.shutdown_and_wait().await?;
    Ok(())
}
