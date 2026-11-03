use super::*;
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

fn config(root: &Path, scope: &str) -> TaskManagerConfig {
    TaskManagerConfig {
        data_dir: root.to_path_buf(),
        worker_count: 1,
        default_workspace: root.join(scope),
        default_model: format!("{scope}-model"),
        default_mode: "plan".into(),
        allow_shell: false,
        trust_mode: false,
        execution_limits: TaskExecutionLimits::default(),
    }
}

struct RecordingExecutor {
    root: PathBuf,
    scope: String,
}

#[async_trait]
impl TaskExecutor for RecordingExecutor {
    async fn execute(
        &self,
        task: ExecutionTask,
        events: mpsc::Sender<TaskExecutionEvent>,
        cancel: CancellationToken,
    ) -> TaskExecutionResult {
        let mut receipt = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(format!("{}.executions", self.scope)))
            .unwrap();
        writeln!(
            receipt,
            "{}",
            serde_json::json!({
                "id": task.id, "scope": self.scope, "model": task.model,
                "provider": task.model_provider, "provider_id": task.model_provider_id,
                "workspace": task.workspace, "prompt": task.prompt,
                "allow_shell": task.allow_shell, "trust_mode": task.trust_mode,
            })
        )
        .unwrap();
        if task.prompt.ends_with("hold") {
            cancel.cancelled().await;
            fs::write(self.root.join(format!("{}.canceled", self.scope)), &task.id).unwrap();
        }
        events
            .send(TaskExecutionEvent::MessageDelta {
                content: format!("{} result", self.scope),
            })
            .await
            .ok();
        TaskExecutionResult {
            status: TaskStatus::Completed,
            result_text: Some(format!("{} completed", self.scope)),
            error: None,
            terminal_reason: TaskTerminalReason::Completed,
        }
    }
}

async fn manager(root: &Path, scope: &str) -> Result<SharedTaskManager> {
    TaskManager::start_with_executor_in_scope(
        config(root, scope),
        Arc::new(RecordingExecutor {
            root: root.to_path_buf(),
            scope: scope.into(),
        }),
        scope,
    )
    .await
}

async fn wait_file(path: &Path) -> Result<Vec<u8>> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match fs::read(path) {
                Ok(bytes) if !bytes.is_empty() => return Ok(bytes),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("fixture receipt timeout")?
}

fn executions(root: &Path, scope: &str) -> Vec<Value> {
    fs::read_to_string(root.join(format!("{scope}.executions")))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

struct ChildOwner(Child);
impl Drop for ChildOwner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn child_owner(root: &Path) -> Result<ChildOwner> {
    let log = fs::File::create(root.join("child.log"))?;
    Ok(ChildOwner(
        Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "task_manager::ownership_tests::ownership_process_child",
                "--ignored",
                "--nocapture",
            ])
            .env("CW_TASK_OWNER_FIXTURE_ROOT", root)
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()?,
    ))
}

#[tokio::test]
#[ignore = "subprocess entry; invoked by shared-store ownership fixtures"]
async fn ownership_process_child() -> Result<()> {
    let root = PathBuf::from(std::env::var("CW_TASK_OWNER_FIXTURE_ROOT")?);
    let owner = manager(&root, "A").await?;
    let active = owner
        .add_task(NewTaskRequest {
            owner_session_id: Some("session-a".into()),
            ..NewTaskRequest::from_prompt("A hold")
        })
        .await?;
    wait_file(&root.join("A.executions")).await?;
    let queued = owner
        .add_task(NewTaskRequest {
            owner_session_id: Some("session-a".into()),
            ..NewTaskRequest::from_prompt("A queued")
        })
        .await?;
    fs::write(
        root.join("ready"),
        serde_json::to_vec(&[active.id, queued.id])?,
    )?;
    wait_file(&root.join("stop")).await?;
    owner.shutdown_and_wait().await?;
    drop(owner);
    Ok(())
}

#[tokio::test]
async fn second_process_preserves_live_owner_and_durable_cancel_reaches_that_owner() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut child = child_owner(root.path())?;
    let ids: Vec<String> = serde_json::from_slice(&wait_file(&root.path().join("ready")).await?)?;
    let active_path = root.path().join("tasks").join(format!("{}.json", ids[0]));
    let before = fs::read(&active_path)?;
    let other = manager(root.path(), "B").await?;
    assert_eq!(other.get_task(&ids[0]).await?.status, TaskStatus::Running);
    assert_eq!(
        fs::read(&active_path)?,
        before,
        "opening a foreign scope cannot rewrite live work"
    );
    assert!(
        other
            .get_task_for_owner(&ids[0], "session-b")
            .await
            .is_err()
    );
    assert!(
        other
            .cancel_task_for_owner(&ids[0][..12], "session-b")
            .await
            .is_err()
    );
    assert!(
        other
            .list_tasks_for_owner(None, None, "session-b")
            .await?
            .is_empty()
    );
    let own = other
        .add_task(NewTaskRequest::from_prompt("B quick"))
        .await?;
    let done = wait_for_terminal_state(&other, &own.id, Duration::from_secs(10)).await?;
    assert_eq!(done.status, TaskStatus::Completed);
    assert_eq!(executions(root.path(), "B").len(), 1);
    assert_eq!(executions(root.path(), "B")[0]["model"], "B-model");
    assert_eq!(
        executions(root.path(), "A").len(),
        1,
        "B cannot claim A's queued request"
    );
    other.cancel_task(&ids[1]).await?;
    other.cancel_task(&ids[0]).await?;
    wait_file(&root.path().join("A.canceled")).await?;
    let canceled = wait_for_terminal_state(&other, &ids[0], Duration::from_secs(10)).await?;
    assert_eq!(canceled.status, TaskStatus::Canceled);
    assert!(canceled.cancel_requested_seq > 0);
    assert_eq!(other.get_task(&ids[1]).await?.status, TaskStatus::Canceled);
    assert_eq!(executions(root.path(), "A").len(), 1);
    fs::write(root.path().join("stop"), "stop")?;
    assert!(child.0.wait()?.success());
    other.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn killed_process_is_reconciled_once_and_only_its_scope_resumes_queued_work() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut child = child_owner(root.path())?;
    let ids: Vec<String> = serde_json::from_slice(&wait_file(&root.path().join("ready")).await?)?;
    let observer = manager(root.path(), "B").await?;
    assert!(
        manager(root.path(), "A").await.is_err(),
        "live scope cannot have a second executor"
    );
    child.0.kill()?;
    child.0.wait()?;
    let restarted = manager(root.path(), "A").await?;
    let interrupted = restarted.get_task(&ids[0]).await?;
    assert_eq!(interrupted.status, TaskStatus::Failed);
    assert!(
        interrupted
            .error
            .as_deref()
            .unwrap()
            .contains("Interrupted by process restart")
    );
    let terminal = wait_for_terminal_state(&restarted, &ids[1], Duration::from_secs(10)).await?;
    assert_eq!(terminal.status, TaskStatus::Completed);
    let receipt = executions(root.path(), "A");
    assert_eq!(receipt.len(), 2);
    assert_eq!(
        receipt.iter().filter(|r| r["id"] == ids[0]).count(),
        1,
        "accepted Running work cannot replay"
    );
    assert_eq!(receipt[1]["id"], ids[1]);
    assert_eq!(receipt[1]["model"], "A-model");
    assert!(executions(root.path(), "B").is_empty());
    restarted.shutdown_and_wait().await?;
    drop(restarted);
    let again = manager(root.path(), "A").await?;
    assert_eq!(
        again.get_task(&ids[0]).await?.lifecycle_seq,
        interrupted.lifecycle_seq
    );
    assert_eq!(executions(root.path(), "A").len(), 2);
    again.shutdown_and_wait().await?;
    observer.shutdown_and_wait().await?;
    Ok(())
}

async fn stop_idle_workers(manager: &TaskManager) {
    for worker in std::mem::take(&mut *manager.workers.lock().await) {
        worker.join.abort();
        let _ = worker.join.await;
    }
}

struct CountExecutor(Arc<AtomicUsize>);
#[async_trait]
impl TaskExecutor for CountExecutor {
    async fn execute(
        &self,
        _: ExecutionTask,
        _: mpsc::Sender<TaskExecutionEvent>,
        _: CancellationToken,
    ) -> TaskExecutionResult {
        self.0.fetch_add(1, Ordering::SeqCst);
        TaskExecutionResult::from_reason(TaskTerminalReason::Completed, None)
    }
}

#[tokio::test]
async fn cancellation_after_claim_before_first_poll_never_invokes_executor() -> Result<()> {
    let root = tempfile::tempdir()?;
    let calls = Arc::new(AtomicUsize::new(0));
    let owner = TaskManager::start_with_executor_in_scope(
        config(root.path(), "A"),
        Arc::new(CountExecutor(calls.clone())),
        "A",
    )
    .await?;
    stop_idle_workers(&owner).await;
    let task = owner
        .add_task(NewTaskRequest::from_prompt("claim barrier"))
        .await?;
    let (id, request, cancel) = owner.claim_next_task().await?.context("claim")?;
    let other = manager(root.path(), "B").await?;
    other.cancel_task(&task.id).await?;
    owner.run_task(id, request, cancel).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(owner.get_task(&task.id).await?.status, TaskStatus::Canceled);
    let healthy = owner
        .add_task(NewTaskRequest::from_prompt("uncanceled control"))
        .await?;
    let (id, request, cancel) = owner.claim_next_task().await?.context("control claim")?;
    owner.run_task(id, request, cancel).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the same executor must run without cancellation"
    );
    assert_eq!(
        owner.get_task(&healthy.id).await?.status,
        TaskStatus::Completed
    );
    owner.shutdown_and_wait().await?;
    other.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn pending_flush_and_finish_merge_fresh_metadata_and_monotonic_cancel() -> Result<()> {
    let root = tempfile::tempdir()?;
    let owner = manager(root.path(), "A").await?;
    stop_idle_workers(&owner).await;
    let task = owner
        .add_task(NewTaskRequest::from_prompt("bounded deltas"))
        .await?;
    let (_, request, cancel) = owner.claim_next_task().await?.context("claim")?;
    owner
        .apply_execution_event(
            &task.id,
            TaskExecutionEvent::MessageDelta {
                content: "accepted progress".into(),
            },
        )
        .await?;
    let other = manager(root.path(), "B").await?;
    other
        .record_tool_metadata(
            &task.id,
            &serde_json::json!({"task_updates": {"checklist": {
                "items": [{"id": 1, "content": "merged checklist", "status": "done"}],
                "completion_pct": 100,
                "in_progress_id": null,
                "updated_at": null
            }}}),
        )
        .await?;
    other.cancel_task(&task.id).await?;
    let seq = other.get_task(&task.id).await?.cancel_requested_seq;
    owner.flush_task(&task.id).await?;
    owner
        .finish_task(
            &task.id,
            TaskExecutionResult::from_reason(TaskTerminalReason::Completed, None),
            cancel,
            &request.mode_label,
        )
        .await?;
    let final_task = other.get_task(&task.id).await?;
    assert_eq!(final_task.status, TaskStatus::Canceled);
    assert_eq!(final_task.cancel_requested_seq, seq);
    assert_eq!(final_task.checklist.items.len(), 1);
    assert_eq!(final_task.checklist.completion_pct, 100);
    assert_eq!(
        final_task
            .timeline
            .iter()
            .filter(|event| event.summary == "accepted progress")
            .count(),
        1
    );
    owner.shutdown_and_wait().await?;
    other.shutdown_and_wait().await?;
    Ok(())
}

#[cfg(unix)]
struct ReadOnlyFixtureDir(PathBuf);
#[cfg(unix)]
impl ReadOnlyFixtureDir {
    fn new(path: &Path) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o500))?;
        Ok(Self(path.to_path_buf()))
    }
}
#[cfg(unix)]
impl Drop for ReadOnlyFixtureDir {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn failed_running_persistence_leaves_queued_work_without_executor_poll() -> Result<()> {
    let root = tempfile::tempdir()?;
    let calls = Arc::new(AtomicUsize::new(0));
    let owner = TaskManager::start_with_executor_in_scope(
        config(root.path(), "A"),
        Arc::new(CountExecutor(calls.clone())),
        "A",
    )
    .await?;
    stop_idle_workers(&owner).await;
    let task = owner
        .add_task(NewTaskRequest::from_prompt("claim write failure"))
        .await?;
    let blocked = ReadOnlyFixtureDir::new(&owner.tasks_dir)?;
    assert!(owner.claim_next_task().await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(owner.get_task(&task.id).await?.status, TaskStatus::Queued);
    drop(blocked);
    let (id, request, cancel) = owner.claim_next_task().await?.context("retry claim")?;
    owner.run_task(id, request, cancel).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    owner.shutdown_and_wait().await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn failed_event_persistence_is_bounded_and_retains_each_accepted_delta_once() -> Result<()> {
    let root = tempfile::tempdir()?;
    let owner = manager(root.path(), "A").await?;
    stop_idle_workers(&owner).await;
    let task = owner
        .add_task(NewTaskRequest::from_prompt("bounded write failure"))
        .await?;
    let (_, request, cancel) = owner.claim_next_task().await?.context("claim")?;
    let blocked = ReadOnlyFixtureDir::new(&owner.tasks_dir)?;
    for seq in 0..TASK_EVENT_CHANNEL_CAPACITY {
        owner
            .apply_execution_event(
                &task.id,
                TaskExecutionEvent::RuntimeEvent {
                    seq: seq as u64,
                    event: "fixture".into(),
                    summary: seq.to_string(),
                },
            )
            .await?;
    }
    let rejected = TaskExecutionEvent::RuntimeEvent {
        seq: 999,
        event: "fixture".into(),
        summary: "after recovery".into(),
    };
    assert!(
        owner
            .apply_execution_event(&task.id, rejected.clone())
            .await
            .is_err()
    );
    assert_eq!(
        owner.state.lock().await.pending_events[&task.id].len(),
        TASK_EVENT_CHANNEL_CAPACITY
    );
    assert!(
        cancel.is_cancelled(),
        "failed persistence requests actual-owner cancellation"
    );
    drop(blocked);
    owner.apply_execution_event(&task.id, rejected).await?;
    owner.flush_task(&task.id).await?;
    assert_eq!(
        owner.get_task(&task.id).await?.runtime_event_count,
        TASK_EVENT_CHANNEL_CAPACITY + 1
    );
    assert!(
        !owner
            .state
            .lock()
            .await
            .pending_events
            .contains_key(&task.id)
    );
    owner
        .finish_task(
            &task.id,
            TaskExecutionResult::from_reason(TaskTerminalReason::Canceled, None),
            cancel,
            &request.mode_label,
        )
        .await?;
    owner.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn legacy_queued_and_running_records_remain_visible_unverified_and_byte_identical()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let owner = manager(root.path(), "A").await?;
    stop_idle_workers(&owner).await;
    let first = owner
        .add_task(NewTaskRequest {
            owner_session_id: Some("legacy-owner".into()),
            ..NewTaskRequest::from_prompt("legacy running")
        })
        .await?;
    owner.claim_next_task().await?.context("claim")?;
    let second = owner
        .add_task(NewTaskRequest::from_prompt("legacy queued"))
        .await?;
    let mut snapshots = Vec::new();
    for id in [&first.id, &second.id] {
        let path = owner.tasks_dir.join(format!("{id}.json"));
        let mut legacy: Value = serde_json::from_slice(&fs::read(&path)?)?;
        let object = legacy.as_object_mut().unwrap();
        object.remove("execution_scope");
        object.remove("execution_generation");
        object.remove("cancel_requested_seq");
        object.insert("schema_version".into(), Value::from(3));
        let bytes = serde_json::to_vec(&legacy)?;
        fs::write(&path, &bytes)?;
        snapshots.push((path, bytes));
    }
    let other = manager(root.path(), "B").await?;
    sleep(STORE_REFRESH_INTERVAL * 2).await;
    let rows = other.list_tasks(None).await?;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| !row.execution_binding_known));
    for (path, bytes) in snapshots {
        assert_eq!(fs::read(path)?, bytes);
    }
    assert_eq!(
        other
            .get_task_for_owner(&first.id, "legacy-owner")
            .await?
            .status,
        TaskStatus::Running
    );
    assert!(executions(root.path(), "B").is_empty());
    owner.shutdown_and_wait().await?;
    other.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn unreadable_owner_before_first_poll_waits_for_storage_without_executing() -> Result<()> {
    for already_canceled in [false, true] {
        let root = tempfile::tempdir()?;
        let calls = Arc::new(AtomicUsize::new(0));
        let owner = TaskManager::start_with_executor_in_scope(
            config(root.path(), "A"),
            Arc::new(CountExecutor(calls.clone())),
            "A",
        )
        .await?;
        stop_idle_workers(&owner).await;
        let task = owner
            .add_task(NewTaskRequest::from_prompt("unavailable before poll"))
            .await?;
        let (id, request, cancel) = owner.claim_next_task().await?.context("claim")?;
        if already_canceled {
            cancel.cancel();
        }
        let path = owner.tasks_dir.join(format!("{}.json", task.id));
        let stored = fs::read(&path)?;
        fs::write(&path, b"{corrupt")?;
        let running = tokio::spawn({
            let owner = owner.clone();
            async move { owner.run_task(id, request, cancel).await }
        });
        sleep(STORE_REFRESH_INTERVAL * 2).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(
            !running.is_finished(),
            "unpersisted terminal state is still pending recovery"
        );
        fs::write(&path, stored)?;
        tokio::time::timeout(Duration::from_secs(5), running).await??;
        assert_eq!(owner.get_task(&task.id).await?.status, TaskStatus::Canceled);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        owner.shutdown_and_wait().await?;
    }
    Ok(())
}
