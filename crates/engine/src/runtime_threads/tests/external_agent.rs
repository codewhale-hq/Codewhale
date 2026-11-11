use super::*;
use crate::fleet::agents::{FIXTURE_AGENT_ENV, FIXTURE_AGENT_ID};

fn fixture_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp-agent.mjs")
}

async fn node_available() -> bool {
    crate::fleet::agents::resolve("node").await.is_some()
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
        if !turn.status.is_active_work() && !manager.external_turn_is_active(&turn.thread_id).await
        {
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
    if !node_available().await {
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
    if !node_available().await {
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
    if !node_available().await {
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

#[cfg(unix)]
#[tokio::test]
async fn an_abandoned_external_startup_stops_its_agent_inside_the_shutdown_barrier() -> Result<()> {
    if !node_available().await {
        return Ok(());
    }
    let _env = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let source = std::fs::read_to_string(fixture_script())?;
    let original = "case 'initialize':";
    assert!(source.contains(original));
    let script = root.path().join("paused.mjs");
    std::fs::write(
        &script,
        source.replace(
            original,
            "case 'initialize':\n      writeFileSync(join(process.cwd(), 'initialize.pid'), String(process.pid));\n      break;\n    case 'initialize-answered':",
        ),
    )?;
    let _fixture = crate::test_support::EnvVarGuard::set(FIXTURE_AGENT_ENV, &script);
    let workspace = root.path().join("work");
    std::fs::create_dir_all(&workspace)?;
    let manager = test_manager(root.path().join("runtime"))?;
    let thread = manager
        .create_thread(fixture_thread_request(&workspace, "ask"))
        .await?;
    let starting_manager = manager.clone();
    let starting_thread = thread.id.clone();
    let starting = tokio::spawn(async move {
        starting_manager
            .start_turn(&starting_thread, prompt("never dispatched"))
            .await
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let pid = loop {
        if let Some(pid) = std::fs::read_to_string(workspace.join("initialize.pid"))
            .ok()
            .and_then(|pid| pid.parse::<u32>().ok())
        {
            break pid;
        }
        anyhow::ensure!(
            !starting.is_finished(),
            "startup ended before the agent paused"
        );
        anyhow::ensure!(
            Instant::now() < deadline,
            "the agent never reached initialize"
        );
        sleep(Duration::from_millis(10)).await;
    };
    let alive = || {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    assert!(alive(), "the paused agent is a real process");
    assert!(!starting.is_finished());
    starting.abort();
    assert!(
        starting
            .await
            .expect_err("the caller is gone")
            .is_cancelled()
    );
    tokio::time::timeout(Duration::from_secs(30), manager.shutdown_and_wait()).await??;
    assert!(
        !alive(),
        "shutdown returned while the abandoned agent process {pid} was still running"
    );
    assert!(manager.store.list_turns_for_thread(&thread.id)?.is_empty());
    assert_eq!(
        manager
            .store
            .load_thread(&thread.id)?
            .executor
            .and_then(|record| record.foreign_session_id),
        None
    );
    Ok(())
}

#[cfg(unix)]
fn options_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp-options-agent.mjs")
}

#[cfg(unix)]
fn received(workspace: &Path) -> Vec<Value> {
    std::fs::read_to_string(workspace.join("options-received.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[cfg(unix)]
fn pid_alive(pid: u64) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn refusal_code(error: &anyhow::Error) -> Option<&'static str> {
    error
        .downcast_ref::<crate::fleet::agents::ExecutorUnavailable>()
        .map(|refusal| refusal.code)
}

#[cfg(unix)]
fn options_thread_request(workspace: &Path, model: &str) -> CreateThreadRequest {
    let mut request = fixture_thread_request(workspace, "ask");
    request.executor.as_mut().unwrap().config.model = Some(model.to_string());
    request
}

#[cfg(unix)]
#[tokio::test]
async fn an_options_probe_reports_the_agents_own_offer_and_leaves_no_process() -> Result<()> {
    if !node_available().await {
        return Ok(());
    }
    let _env = crate::test_support::lock_test_env();
    let _fixture = crate::test_support::EnvVarGuard::set(FIXTURE_AGENT_ENV, options_fixture());
    let _secret = crate::test_support::EnvVarGuard::set("CODEWHALE_TEST_SECRET", "must-not-travel");
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("work");
    std::fs::create_dir_all(&workspace)?;
    let manager = test_manager(root.path().join("runtime"))?;

    let options = manager
        .external_agent_options(FIXTURE_AGENT_ID, &workspace)
        .await?;
    let json = serde_json::to_value(&options)?;
    let controls: Vec<&str> = options.controls.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        controls,
        ["model", "config:effort", "config:verbose", "mode"]
    );
    let model = &options.controls[0];
    assert_eq!(model.current.as_deref(), Some("model-a"));
    let values: Vec<&str> = model.choices.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(values, ["model-a", "model-b", "model-c"]);
    assert_eq!(
        json["controls"][0]["write"]["method"],
        "session/set_config_option"
    );
    assert_eq!(json["controls"][3]["write"]["method"], "session/set_mode");
    let verbose = &options.controls[2];
    assert_eq!(
        verbose.unavailable.as_ref().map(|u| u.code),
        Some("control_type_unsupported")
    );
    assert!(verbose.write.is_none());
    assert_eq!(json["capabilities"]["prompt_image"], true);
    assert_eq!(json["capabilities"]["fork_session"], true);
    let action = |name: &str| {
        json["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["action"] == name)
            .cloned()
            .unwrap()
    };
    assert_eq!(action("model_choice")["supported"], true);
    assert_eq!(action("steering")["code"], "unsupported_control");
    assert_eq!(action("images")["supported"], false);
    assert_eq!(action("images")["agent_reports"], true);

    let log = received(&workspace);
    let pid = log[0]["pid"].as_u64().expect("the probe agent's pid");
    assert!(!pid_alive(pid), "the probe's agent process was stopped");
    assert!(log.iter().all(|entry| entry["method"] != "session/prompt"));
    let dumped = std::fs::read_to_string(workspace.join("options-env.json"))?;
    assert!(!dumped.contains("CODEWHALE_TEST_SECRET"));

    let wrong = manager
        .external_agent_options(FIXTURE_AGENT_ID, &workspace.join("missing"))
        .await
        .unwrap_err();
    assert!(wrong.to_string().contains("unavailable"), "{wrong}");
    let unknown = manager
        .external_agent_options("nope", &workspace)
        .await
        .unwrap_err();
    assert_eq!(refusal_code(&unknown), Some("unknown_agent"));

    std::fs::write(
        workspace.join("options-fixture.json"),
        r#"{"auth_required":true}"#,
    )?;
    let unauthenticated = manager
        .external_agent_options(FIXTURE_AGENT_ID, &workspace)
        .await
        .unwrap_err();
    assert_eq!(refusal_code(&unauthenticated), Some("needs_sign_in"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn an_options_probe_ends_when_the_runtime_shuts_down() -> Result<()> {
    if !node_available().await {
        return Ok(());
    }
    let _env = crate::test_support::lock_test_env();
    let _fixture = crate::test_support::EnvVarGuard::set(FIXTURE_AGENT_ENV, options_fixture());
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("work");
    std::fs::create_dir_all(&workspace)?;
    std::fs::write(workspace.join("options-fixture.json"), r#"{"hang":true}"#)?;
    let manager = test_manager(root.path().join("runtime"))?;
    let probing = manager.clone();
    let probe_workspace = workspace.clone();
    let probe = tokio::spawn(async move {
        probing
            .external_agent_options(FIXTURE_AGENT_ID, &probe_workspace)
            .await
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let pid = loop {
        if let Some(pid) = received(&workspace)
            .iter()
            .find(|entry| entry["method"] == "session/new")
            .and_then(|entry| entry["pid"].as_u64())
        {
            break pid;
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "the probe never reached session/new"
        );
        sleep(Duration::from_millis(20)).await;
    };
    assert!(pid_alive(pid));
    manager.close_execution_admission().await;
    let ended = tokio::time::timeout(Duration::from_secs(20), probe).await??;
    assert!(ended.is_err());
    let deadline = Instant::now() + Duration::from_secs(10);
    while pid_alive(pid) {
        anyhow::ensure!(
            Instant::now() < deadline,
            "the probe's agent outlived shutdown"
        );
        sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_chosen_model_reaches_the_agent_survives_reopen_and_usage_is_agent_reported() -> Result<()>
{
    if !node_available().await {
        return Ok(());
    }
    let _env = crate::test_support::lock_test_env();
    let _fixture = crate::test_support::EnvVarGuard::set(FIXTURE_AGENT_ENV, options_fixture());
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("work");
    std::fs::create_dir_all(&workspace)?;
    let manager = test_manager(root.path().join("runtime"))?;
    let offered = manager
        .external_agent_options(FIXTURE_AGENT_ID, &workspace)
        .await?;
    let chosen = &offered.controls[0].choices[1].value;
    assert_eq!(chosen, "model-b");

    let thread = manager
        .create_thread(options_thread_request(&workspace, chosen))
        .await?;
    let turn = manager.start_turn(&thread.id, prompt("first")).await?;
    let settled = wait_for_turn(&manager, &turn.id).await?;
    assert_eq!(settled.status, RuntimeTurnStatus::Completed, "{settled:?}");
    let detail = manager.get_thread_detail(&thread.id).await?;
    let message = detail
        .items
        .iter()
        .find(|item| item.kind == TurnItemKind::AgentMessage)
        .and_then(|item| item.detail.clone())
        .unwrap_or_default();
    assert!(message.contains("model=model-b"), "{message}");

    let usage: Vec<_> = events(&manager, &thread.id)
        .await?
        .into_iter()
        .filter(|event| event.event == "turn.agent_usage")
        .collect();
    let window = usage
        .iter()
        .find(|event| event.payload["kind"] == "context_window")
        .expect("a context window update");
    assert_eq!(window.payload["used"], 1200);
    assert_eq!(window.payload["size"], 200000);
    let total = usage
        .iter()
        .find(|event| event.payload["kind"] == "turn_total")
        .expect("a turn total");
    assert_eq!(total.payload["source"], "agent reported");
    assert_eq!(total.payload["cost"], "unknown");
    assert_eq!(total.payload["tokens"]["total_tokens"], 150);
    assert_eq!(total.payload["tokens"]["input_tokens"], 100);
    assert_eq!(total.payload["context"]["used"], 1200);

    assert!(manager.stop_external_agent(&thread.id).await?);
    let reopened = manager.start_turn(&thread.id, prompt("second")).await?;
    let settled = wait_for_turn(&manager, &reopened.id).await?;
    assert_eq!(settled.status, RuntimeTurnStatus::Completed, "{settled:?}");
    let log = received(&workspace);
    let sets: Vec<_> = log
        .iter()
        .filter(|entry| entry["method"] == "session/set_config_option")
        .collect();
    assert_eq!(sets.len(), 2, "{log:?}");
    assert!(sets.iter().all(|entry| entry["value"] == "model-b"));
    assert_ne!(
        sets[0]["pid"], sets[1]["pid"],
        "a new agent process took the saved choice"
    );
    let prompts: Vec<_> = log
        .iter()
        .filter(|entry| entry["method"] == "session/prompt")
        .collect();
    assert_eq!(prompts.len(), 2);
    assert!(log.iter().any(|entry| entry["method"] == "session/load"));
    assert_eq!(prompts[1]["state"]["model"], "model-b");
    assert_eq!(
        manager
            .store
            .load_thread(&thread.id)?
            .executor
            .and_then(|record| record.model)
            .as_deref(),
        Some("model-b")
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn an_agent_that_reports_no_usage_says_unknown() -> Result<()> {
    if !node_available().await {
        return Ok(());
    }
    let _env = crate::test_support::lock_test_env();
    let _fixture = crate::test_support::EnvVarGuard::set(FIXTURE_AGENT_ENV, options_fixture());
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("work");
    std::fs::create_dir_all(&workspace)?;
    std::fs::write(
        workspace.join("options-fixture.json"),
        r#"{"no_usage":true}"#,
    )?;
    let manager = test_manager(root.path().join("runtime"))?;
    let thread = manager
        .create_thread(fixture_thread_request(&workspace, "ask"))
        .await?;
    let turn = manager.start_turn(&thread.id, prompt("quiet")).await?;
    wait_for_turn(&manager, &turn.id).await?;
    let events = events(&manager, &thread.id).await?;
    let usage: Vec<_> = events
        .iter()
        .filter(|event| event.event == "turn.agent_usage")
        .collect();
    assert_eq!(usage.len(), 1, "{usage:?}");
    assert_eq!(usage[0].payload["tokens"], "unknown");
    assert_eq!(usage[0].payload["context"], "unknown");
    assert_eq!(usage[0].payload["source"], "none");
    assert_eq!(usage[0].payload["cost"], "unknown");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_stale_or_withdrawn_choice_is_refused_before_any_prompt() -> Result<()> {
    if !node_available().await {
        return Ok(());
    }
    let _env = crate::test_support::lock_test_env();
    let _fixture = crate::test_support::EnvVarGuard::set(FIXTURE_AGENT_ENV, options_fixture());
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("work");
    std::fs::create_dir_all(&workspace)?;
    let manager = test_manager(root.path().join("runtime"))?;

    let invented = manager
        .create_thread(options_thread_request(&workspace, "model-z"))
        .await?;
    let error = manager
        .start_turn(&invented.id, prompt("never"))
        .await
        .unwrap_err();
    assert_eq!(
        refusal_code(&error),
        Some("choice_not_offered"),
        "{error:#}"
    );

    std::fs::write(
        workspace.join("options-fixture.json"),
        r#"{"withdraw":["model-b"]}"#,
    )?;
    let withdrawn = manager
        .create_thread(options_thread_request(&workspace, "model-b"))
        .await?;
    let error = manager
        .start_turn(&withdrawn.id, prompt("never"))
        .await
        .unwrap_err();
    assert_eq!(
        refusal_code(&error),
        Some("choice_not_offered"),
        "{error:#}"
    );

    let log = received(&workspace);
    assert!(log.iter().all(|entry| entry["method"] != "session/prompt"));
    assert!(
        log.iter()
            .all(|entry| entry["method"] != "session/set_config_option")
    );
    for pid in log.iter().filter_map(|entry| entry["pid"].as_u64()) {
        assert!(!pid_alive(pid), "a refused setup left agent {pid} running");
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn per_turn_controls_an_agent_cannot_honor_are_typed_refusals() -> Result<()> {
    if !node_available().await {
        return Ok(());
    }
    let _env = crate::test_support::lock_test_env();
    let _fixture = crate::test_support::EnvVarGuard::set(FIXTURE_AGENT_ENV, options_fixture());
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("work");
    std::fs::create_dir_all(&workspace)?;
    let manager = test_manager(root.path().join("runtime"))?;
    let thread = manager
        .create_thread(fixture_thread_request(&workspace, "ask"))
        .await?;
    for request in [
        StartTurnRequest {
            allowed_tools: Some(Vec::new()),
            ..prompt("x")
        },
        StartTurnRequest {
            model: Some("model-b".to_string()),
            ..prompt("x")
        },
        StartTurnRequest {
            reasoning_effort: Some("high".to_string()),
            ..prompt("x")
        },
    ] {
        let error = manager.start_turn(&thread.id, request).await.unwrap_err();
        assert_eq!(
            refusal_code(&error),
            Some("unsupported_control"),
            "{error:#}"
        );
    }
    assert!(
        received(&workspace).is_empty(),
        "no agent was started for a refused turn"
    );
    Ok(())
}

fn fixture_lines(workspace: &Path, name: &str) -> Vec<Value> {
    std::fs::read_to_string(workspace.join(name))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[tokio::test]
async fn interrupting_a_pending_approval_answers_the_agents_request_as_cancelled() -> Result<()> {
    if !node_available().await {
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
    let turn = manager.start_turn(&thread.id, prompt("stop me")).await?;
    wait_for_approval(&manager, &thread.id).await?;
    manager.interrupt_turn(&thread.id, &turn.id).await?;
    assert_eq!(
        wait_for_turn(&manager, &turn.id).await?.status,
        RuntimeTurnStatus::Interrupted
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let outcomes = loop {
        let outcomes = fixture_lines(&workspace, "permission-outcomes.jsonl");
        if !outcomes.is_empty() || Instant::now() >= deadline {
            break outcomes;
        }
        sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        outcomes,
        vec![json!({ "outcome": "cancelled" })],
        "the agent never received a cancelled answer for its open permission request"
    );
    assert!(!workspace.join("fixture.txt").exists());
    manager.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn an_agent_that_ignores_cancel_is_stopped_after_the_grace_period() -> Result<()> {
    if !node_available().await {
        return Ok(());
    }
    let _env = crate::test_support::lock_test_env();
    let _fixture = crate::test_support::EnvVarGuard::set(FIXTURE_AGENT_ENV, fixture_script());
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("work");
    std::fs::create_dir_all(&workspace)?;
    std::fs::write(
        workspace.join("fixture-plan.json"),
        json!({ "ignoreCancel": true }).to_string(),
    )?;
    let manager = test_manager(root.path().join("runtime"))?;
    let thread = manager
        .create_thread(fixture_thread_request(&workspace, "ask"))
        .await?;
    let turn = manager.start_turn(&thread.id, prompt("never stop")).await?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while fixture_lines(&workspace, "held-prompts.jsonl").is_empty() {
        anyhow::ensure!(Instant::now() < deadline, "the agent never got the prompt");
        sleep(Duration::from_millis(50)).await;
    }
    let interrupted = Instant::now();
    manager.interrupt_turn(&thread.id, &turn.id).await?;
    let settled = wait_for_turn(&manager, &turn.id).await?;
    let waited = interrupted.elapsed();
    assert_eq!(settled.status, RuntimeTurnStatus::Interrupted);
    assert!(
        settled
            .error
            .as_deref()
            .is_some_and(|reason| reason.contains("ended its process")),
        "the turn did not settle with the killed reason: {:?}",
        settled.error
    );
    assert!(
        waited >= Duration::from_secs(9) && waited < Duration::from_secs(15),
        "the forced stop came after {waited:?}, outside the grace period plus five seconds"
    );
    manager.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn additional_directories_reach_only_an_agent_that_advertises_them() -> Result<()> {
    if !node_available().await {
        return Ok(());
    }
    let _env = crate::test_support::lock_test_env();
    let _fixture = crate::test_support::EnvVarGuard::set(FIXTURE_AGENT_ENV, fixture_script());
    let root = tempfile::tempdir()?;
    let extra = root.path().join("extra");
    std::fs::create_dir_all(&extra)?;
    let extra = extra.canonicalize()?;
    let mut sent = Vec::new();
    for (name, advertises) in [("quiet", false), ("capable", true)] {
        let workspace = root.path().join(name);
        std::fs::create_dir_all(&workspace)?;
        std::fs::write(
            workspace.join("fixture-plan.json"),
            json!({ "additionalDirectories": advertises }).to_string(),
        )?;
        let manager = test_manager(root.path().join(format!("runtime-{name}")))?;
        let mut request = fixture_thread_request(&workspace, "ask");
        request.add_dirs = vec![extra.clone()];
        let thread = manager.create_thread(request).await?;
        let turn = manager.start_turn(&thread.id, prompt("hello")).await?;
        let approval = wait_for_approval(&manager, &thread.id).await?;
        manager
            .deliver_external_approval(
                &approval.id,
                ExternalApprovalDecision::Deny { remember: false },
            )
            .await?;
        wait_for_turn(&manager, &turn.id).await?;
        sent.push(fixture_lines(&workspace, "session-params.jsonl"));
        manager.shutdown_and_wait().await?;
    }
    assert_eq!(sent[0].len(), 1);
    assert!(
        sent[0][0]["params"].get("additionalDirectories").is_none(),
        "an agent that did not advertise the capability was sent directories: {:?}",
        sent[0]
    );
    assert_eq!(
        sent[1][0]["params"]["additionalDirectories"],
        json!([extra.display().to_string()])
    );
    Ok(())
}

#[tokio::test]
async fn plan_mode_and_command_updates_are_projected() -> Result<()> {
    if !node_available().await {
        return Ok(());
    }
    let _env = crate::test_support::lock_test_env();
    let _fixture = crate::test_support::EnvVarGuard::set(FIXTURE_AGENT_ENV, fixture_script());
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("work");
    std::fs::create_dir_all(&workspace)?;
    std::fs::write(
        workspace.join("fixture-plan.json"),
        json!({ "extraUpdates": true }).to_string(),
    )?;
    let manager = test_manager(root.path().join("runtime"))?;
    let thread = manager
        .create_thread(fixture_thread_request(&workspace, "ask"))
        .await?;
    let turn = manager.start_turn(&thread.id, prompt("hello")).await?;
    let approval = wait_for_approval(&manager, &thread.id).await?;
    manager
        .deliver_external_approval(
            &approval.id,
            ExternalApprovalDecision::Deny { remember: false },
        )
        .await?;
    wait_for_turn(&manager, &turn.id).await?;
    let events = events(&manager, &thread.id).await?;
    let mode = events
        .iter()
        .find(|event| event.event == "turn.agent_mode")
        .expect("a mode update is projected");
    assert_eq!(mode.payload["mode"], "review");
    let commands = events
        .iter()
        .find(|event| event.event == "turn.agent_commands")
        .expect("a command update is projected");
    assert_eq!(commands.payload["commands"][0]["name"], "compact");
    let detail = manager.get_thread_detail(&thread.id).await?;
    assert!(
        detail
            .items
            .iter()
            .any(|item| item.kind == TurnItemKind::Status
                && item
                    .detail
                    .as_deref()
                    .unwrap_or_default()
                    .contains("[in_progress] Write the fixture")),
        "plan_update never reached a plan item"
    );
    manager.shutdown_and_wait().await?;
    Ok(())
}
