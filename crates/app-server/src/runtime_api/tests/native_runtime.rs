use super::*;
use crate::{RuntimeOwnerFrontend, RuntimeOwnerRequest};
use codewhale_app_server_protocol::runtime::NativeRuntimeRequest;

struct Rig {
    _root: tempfile::TempDir,
    state: RuntimeApiState,
    owner: Arc<CapturedRuntimeFrontend>,
    other: PathBuf,
}

impl Rig {
    async fn new() -> Result<Self> {
        #[cfg(unix)]
        let root = tempfile::Builder::new()
            .prefix("cw-nr-")
            .tempdir_in("/tmp")?;
        #[cfg(not(unix))]
        let root = tempfile::tempdir()?;
        let base = root.path().canonicalize()?;
        let workspace = base.join("a");
        let other = base.join("b");
        fs::create_dir_all(&other)?;
        let (state_tx, state_rx) = oneshot::channel();
        let parts = build_test_server(
            base.join("owner"),
            base.join("sessions"),
            None,
            false,
            workspace,
            TestServerOverrides {
                state_handle: Some(state_tx),
                ..Default::default()
            },
        )
        .await?
        .context("fixture listener unavailable")?;
        drop(parts);
        let state = state_rx.await?;
        let owner =
            CapturedRuntimeFrontend::capture(state.clone(), "deepseek-v4-pro".into(), 1, false)
                .await?;
        Ok(Self {
            _root: root,
            state,
            owner,
            other,
        })
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        call(
            self.owner.clone(),
            self.state.workspace.clone(),
            method,
            params,
        )
        .await
    }

    async fn create(&self, key: &str) -> Result<String> {
        let created = self
            .call(
                "runtime/thread/create",
                json!({"body":{"operation_key":key,"model":"deepseek-v4-pro","mode":"agent"}}),
            )
            .await?;
        Ok(created["id"]
            .as_str()
            .context("created thread ID")?
            .to_owned())
    }

    async fn foreign(&self) -> Result<String> {
        Ok(self
            .state
            .runtime_threads
            .create_thread(CreateThreadRequest {
                workspace: Some(self.other.clone()),
                model: Some("deepseek-v4-pro".into()),
                ..Default::default()
            })
            .await?
            .id)
    }

    async fn move_thread(&self, id: &str) -> Result<()> {
        self.state
            .runtime_threads
            .update_thread_with_shell_policy(
                id,
                UpdateThreadRequest {
                    workspace: Some(self.other.clone()),
                    ..Default::default()
                },
                None,
                None,
            )
            .await?;
        Ok(())
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.state.shutdown.requested.cancel();
    }
}

async fn call(
    owner: Arc<CapturedRuntimeFrontend>,
    workspace: PathBuf,
    method: &str,
    params: Value,
) -> Result<Value> {
    let request: NativeRuntimeRequest =
        serde_json::from_value(json!({"method":method,"params":params}))?;
    owner
        .execute(RuntimeOwnerRequest::Native { workspace, request })
        .await
}

fn status(error: anyhow::Error, expected: u16) {
    let failure = error
        .downcast_ref::<crate::RuntimeOwnerFailure>()
        .expect("typed owner refusal");
    assert_eq!(failure.status, expected, "{failure}");
}

fn run_on_product_stack<F>(body: F) -> Result<()>
where
    F: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let stack = codewhale_runtime::CODEWHALE_MAIN_STACK_BYTES;
    std::thread::Builder::new()
        .name("native-runtime-test".to_owned())
        .stack_size(stack)
        .spawn(move || {
            let _env = lock_test_env();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .thread_stack_size(stack)
                .build()?;
            runtime.block_on(body)
        })?
        .join()
        .map_err(|_| anyhow!("native runtime test thread panicked"))?
}

#[test]
fn native_real_owner_rejects_bypass_aliases_and_scopes_lists_before_limit() -> Result<()> {
    run_on_product_stack(async move {
        let rig = Rig::new().await?;
        let baseline = rig
            .state
            .runtime_threads
            .list_threads(ThreadListFilter::IncludeArchived, None)
            .await?
            .len();
        for mode in [
            "yolo",
            "4",
            "bypass",
            "bypass-permissions",
            "bypasspermissions",
            " YOLO ",
            "ByPaSs",
            "agent ",
            "auto",
        ] {
            assert!(
                rig.call(
                    "runtime/thread/create",
                    json!({"body":{"operation_key":format!("rejected-{mode}"),"mode":mode}})
                )
                .await
                .is_err()
            );
        }
        assert_eq!(
            rig.state
                .runtime_threads
                .list_threads(ThreadListFilter::IncludeArchived, None)
                .await?
                .len(),
            baseline
        );
        let selected = rig.create("normal-agent").await?;
        let plan = rig
        .call(
            "runtime/thread/create",
            json!({"body":{"operation_key":"normal-plan","model":"deepseek-v4-pro","mode":"plan"}}),
        )
        .await?;
        assert_ne!(plan["permission_posture"], "full_access");
        assert_ne!(plan["auto_approve"], true);
        let foreign = rig.foreign().await?;
        let list = rig.call("runtime/threads/list", json!({"limit":1})).await?;
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["workspace"], json!(rig.state.workspace));
        assert_ne!(list[0]["id"], foreign);
        let record = rig
            .call("runtime/thread/read", json!({"thread_id":selected}))
            .await?;
        assert_eq!(record["thread"]["id"], selected);
        for (method, params) in [
            ("runtime/thread/read", json!({"thread_id":foreign})),
            (
                "runtime/thread/update",
                json!({"thread_id":foreign,"body":{"title":"no"}}),
            ),
            (
                "runtime/turn/start",
                json!({"thread_id":foreign,"body":{"operation_key":"foreign-turn","prompt":"no","model":"deepseek-v4-pro"}}),
            ),
            (
                "runtime/turn/interrupt",
                json!({"thread_id":foreign,"turn_id":"foreign-turn"}),
            ),
            (
                "runtime/events/subscribe",
                json!({"thread_id":foreign,"since_seq":0}),
            ),
        ] {
            status(rig.call(method, params).await.unwrap_err(), 403);
        }
        status(
            rig.call("runtime/threads/list", json!({"limit":0}))
                .await
                .unwrap_err(),
            400,
        );
        assert!(
            rig.state
                .runtime_threads
                .get_thread_detail(&foreign)
                .await?
                .turns
                .is_empty()
        );
        Ok(())
    })
}

#[test]
fn native_real_owner_read_and_update_refuse_workspace_change_at_admission() -> Result<()> {
    run_on_product_stack(async move {
        let rig = Rig::new().await?;
        let read_id = rig.create("scoped-read").await?;
        let (hook_tx, mut hook_rx) = mpsc::unbounded_channel();
        rig.state.runtime_threads.set_snapshot_test_hook(hook_tx);
        let reader = tokio::spawn(call(
            rig.owner.clone(),
            rig.state.workspace.clone(),
            "runtime/thread/read",
            json!({"thread_id":read_id}),
        ));
        let paused = tokio::time::timeout(Duration::from_secs(5), hook_rx.recv())
            .await?
            .context("snapshot hook")?;
        let manager = rig.state.runtime_threads.clone();
        let moved_id = read_id.clone();
        let other = rig.other.clone();
        let mover = tokio::spawn(async move {
            manager
                .update_thread_with_shell_policy(
                    &moved_id,
                    UpdateThreadRequest {
                        workspace: Some(other),
                        ..Default::default()
                    },
                    None,
                    None,
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if rig
                    .state
                    .runtime_threads
                    .get_thread(&read_id)
                    .await?
                    .workspace
                    == rig.other
                {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        paused
            .resume
            .send(())
            .map_err(|_| anyhow!("resume snapshot"))?;
        status(reader.await?.unwrap_err(), 403);
        mover.await??;
        let update_id = rig.create("scoped-update").await?;
        let (hook_tx, mut hook_rx) = mpsc::unbounded_channel();
        rig.state
            .runtime_threads
            .set_update_admission_test_hook(hook_tx);
        let updater = tokio::spawn(call(
            rig.owner.clone(),
            rig.state.workspace.clone(),
            "runtime/thread/update",
            json!({"thread_id":update_id,"body":{"title":"must-not-apply"}}),
        ));
        let resume = tokio::time::timeout(Duration::from_secs(5), hook_rx.recv())
            .await?
            .context("update hook")?;
        rig.move_thread(&update_id).await?;
        resume.send(()).map_err(|_| anyhow!("resume update"))?;
        status(updater.await?.unwrap_err(), 403);
        assert_ne!(
            rig.state
                .runtime_threads
                .get_thread(&update_id)
                .await?
                .title
                .as_deref(),
            Some("must-not-apply")
        );
        Ok(())
    })
}

#[test]
fn native_real_owner_approval_binding_and_known_policy_refusals_do_not_settle() -> Result<()> {
    run_on_product_stack(async move {
        let rig = Rig::new().await?;
        let id = rig.create("approval-thread").await?;
        let other = rig.create("approval-other").await?;
        let foreign = rig.foreign().await?;
        let (foreign_approval, mut foreign_receiver) = rig
            .state
            .runtime_threads
            .register_pending_approval_for_thread_for_test(&foreign, "foreign-call");
        status(
        rig.call(
            "runtime/approval/decide",
            json!({"approval_id":foreign_approval,"thread_id":foreign,"turn_id":"test-turn","tool_call_id":"foreign-call","decision":"allow","remember":false}),
        )
        .await
        .unwrap_err(),
        403,
    );
        assert!(matches!(
            foreign_receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        let (approval, mut receiver) = rig
            .state
            .runtime_threads
            .register_pending_approval_for_thread_for_test(&id, "call-1");
        for (thread, turn, tool, remember, expected) in [
            (&other, "test-turn", "call-1", false, 403),
            (&id, "other-turn", "call-1", false, 403),
            (&id, "test-turn", "other-tool", false, 403),
            (&id, "test-turn", "call-1", true, 400),
        ] {
            status(rig.call("runtime/approval/decide",json!({"approval_id":approval,"thread_id":thread,"turn_id":turn,"tool_call_id":tool,"decision":"allow","remember":remember})).await.unwrap_err(),expected);
            assert!(matches!(
                receiver.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
        }
        status(rig.call("runtime/turn/start",json!({"thread_id":id,"body":{"operation_key":"bad-tools","prompt":"never-run","allowed_tools":["shell"]}})).await.unwrap_err(),400);
        let settled=rig.call("runtime/approval/decide",json!({"approval_id":approval,"thread_id":id,"turn_id":"test-turn","tool_call_id":"call-1","decision":"deny","remember":false})).await?;
        assert_eq!(settled["delivered"], true);
        assert_eq!(
            receiver.await?,
            ExternalApprovalDecision::Deny { remember: false }
        );
        Ok(())
    })
}

#[test]
fn native_real_owner_empty_tools_reaches_turn_and_expected_idle_is_preserved() -> Result<()> {
    run_on_product_stack(async move {
        let rig = Rig::new().await?;
        let id = rig.create("ask-thread").await?;
        let mut engine = install_mock_engine(&rig.state.runtime_threads, &id).await;
        let started=rig.call("runtime/turn/start",json!({"thread_id":id,"body":{"operation_key":"ask-1","prompt":"no provider fixture","model":"deepseek-v4-pro","allowed_tools":[]}})).await?;
        let op = tokio::time::timeout(Duration::from_secs(5), engine.rx_op.recv())
            .await?
            .context("turn operation")?;
        let Op::SendMessage(spec) = op else {
            bail!("expected SendMessage")
        };
        assert_eq!(spec.allowed_tools, Some(Vec::new()));
        status(
            rig.call(
                "runtime/thread/update",
                json!({"thread_id":id,"body":{"title":"busy","expected_idle":true}}),
            )
            .await
            .unwrap_err(),
            409,
        );
        let turn_id = started["turn"]["id"].as_str().context("turn ID")?;
        rig.call(
            "runtime/turn/interrupt",
            json!({"thread_id":id,"turn_id":turn_id}),
        )
        .await?;
        assert!(engine.cancel_token.is_cancelled());
        let moved = rig.create("start-workspace-assertion").await?;
        rig.move_thread(&moved).await?;
        let error = rig
            .state
            .runtime_threads
            .start_turn(
                &moved,
                StartTurnRequest {
                    prompt: "never-run".into(),
                    model: Some("deepseek-v4-pro".into()),
                    expected_workspace: Some(rig.state.workspace.clone()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(error.is::<codewhale_engine::runtime_threads::manager::ThreadWorkspaceMismatch>());
        assert!(error.to_string().contains("workspace"));
        Ok(())
    })
}

#[test]
fn native_real_owner_automation_destination_and_revision_are_preserved() -> Result<()> {
    run_on_product_stack(async move {
        let rig = Rig::new().await?;
        let id = rig.create("automation-destination").await?;
        let foreign = rig.foreign().await?;
        let body = |destination: &str| json!({"name":"local fixture","prompt":"no provider","rrule":"FREQ=WEEKLY;BYDAY=MO;BYHOUR=12;BYMINUTE=0","destination_thread_id":destination,"model":"deepseek-v4-pro","status":"paused"});
        status(
            rig.call(
                "runtime/automation/create",
                json!({"operation_key":"foreign-auto","body":body(&foreign)}),
            )
            .await
            .unwrap_err(),
            403,
        );
        let mut elevated = body(&id);
        elevated["auto_approve"] = json!(true);
        status(
            rig.call(
                "runtime/automation/create",
                json!({"operation_key":"elevated-auto","body":elevated}),
            )
            .await
            .unwrap_err(),
            400,
        );
        let created = rig
            .call(
                "runtime/automation/create",
                json!({"operation_key":"local-auto","body":body(&id)}),
            )
            .await?;
        let revision = created["revision"]
            .as_u64()
            .context("automation revision")?;
        let resumed=rig.call("runtime/automation/resume",json!({"automation_id":created["id"],"expected_revision":revision,"operation_key":"resume-auto"})).await?;
        assert!(resumed["revision"].as_u64().unwrap() > revision);
        status(rig.call("runtime/automation/pause",json!({"automation_id":created["id"],"expected_revision":revision,"operation_key":"stale-pause"})).await.unwrap_err(),409);
        assert_eq!(
            rig.call("runtime/automations/list", json!({}))
                .await?
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let raced_dest = rig.create("automation-raced-dest").await?;
        let (arrived_tx, arrived_rx) = oneshot::channel();
        let (resume_tx, resume_rx) = oneshot::channel();
        *rig.state.automation_admission_test_hook.lock() = Some(AutomationAdmissionTestHook {
            arrived: arrived_tx,
            resume: resume_rx,
        });
        let creating = tokio::spawn(call(
            rig.owner.clone(),
            rig.state.workspace.clone(),
            "runtime/automation/create",
            json!({"operation_key":"raced-create","body":body(&raced_dest)}),
        ));
        tokio::time::timeout(Duration::from_secs(5), arrived_rx).await??;
        rig.move_thread(&raced_dest).await?;
        resume_tx
            .send(())
            .map_err(|_| anyhow!("resume automation create"))?;
        status(creating.await?.unwrap_err(), 403);
        assert_eq!(
            rig.call("runtime/automations/list", json!({}))
                .await?
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let latest_revision = resumed["revision"].as_u64().unwrap();
        let (arrived_tx, arrived_rx) = oneshot::channel();
        let (resume_tx, resume_rx) = oneshot::channel();
        *rig.state.automation_admission_test_hook.lock() = Some(AutomationAdmissionTestHook {
            arrived: arrived_tx,
            resume: resume_rx,
        });
        let pausing = tokio::spawn(call(
            rig.owner.clone(),
            rig.state.workspace.clone(),
            "runtime/automation/pause",
            json!({"automation_id":created["id"],"expected_revision":latest_revision,"operation_key":"raced-pause"}),
        ));
        tokio::time::timeout(Duration::from_secs(5), arrived_rx).await??;
        rig.move_thread(&id).await?;
        resume_tx
            .send(())
            .map_err(|_| anyhow!("resume automation pause"))?;
        status(pausing.await?.unwrap_err(), 403);
        assert_eq!(
            rig.state
                .automations
                .lock()
                .await
                .get_automation(created["id"].as_str().unwrap())?
                .revision,
            latest_revision
        );
        Ok(())
    })
}

#[test]
fn native_real_owner_replay_live_seam_deduplicates_journal_overlap() -> Result<()> {
    run_on_product_stack(async move {
        let rig = Rig::new().await?;
        let id = rig.create("journal-thread").await?;
        let before = rig
            .state
            .runtime_threads
            .emit_event_for_test(&id, None, "fixture.before", json!({"value":1}))
            .await?;
        rig.call(
            "runtime/events/subscribe",
            json!({"thread_id":id,"since_seq":before.seq-1}),
        )
        .await?;
        let (hook_tx, mut hook_rx) = mpsc::unbounded_channel();
        rig.state.runtime_threads.set_replay_test_hook(hook_tx);
        let owner = rig.owner.clone();
        let stream_id = id.clone();
        let opening = tokio::spawn(async move { owner.events(stream_id, before.seq - 1).await });
        let paused = tokio::time::timeout(Duration::from_secs(5), hook_rx.recv())
            .await?
            .context("replay hook")?;
        let overlap = rig
            .state
            .runtime_threads
            .emit_event_for_test(&id, None, "fixture.overlap", json!({"value":2}))
            .await?;
        paused
            .resume
            .send(())
            .map_err(|_| anyhow!("resume replay"))?;
        let mut stream = opening.await??;
        let live = rig
            .state
            .runtime_threads
            .emit_event_for_test(&id, None, "fixture.live", json!({"value":3}))
            .await?;
        let mut observed = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while observed.len() < 3 {
                let frame = stream.next().await.context("event stream ended")?;
                if let Some(seq) = frame.sequence {
                    observed.push(seq);
                }
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        assert_eq!(observed, vec![before.seq, overlap.seq, live.seq]);
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn native_real_owner_helper_retains_initialize_ack_during_concurrent_input_and_rejects_envelopes()
-> Result<()> {
    run_on_product_stack(async move {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
        let rig = Rig::new().await?;
        let socket = rig._root.path().canonicalize()?.join("run/daemon.sock");
        let (daemon, _) = bind_captured_runtime_frontends(
            &rig.state,
            Some(socket.clone()),
            "deepseek-v4-pro".into(),
            1,
            false,
            None,
        )
        .await?;
        let daemon = daemon.context("native owner fixture endpoint")?;
        let shutdown = daemon.shutdown_handle();
        let server = tokio::spawn(daemon.serve());
        let observed =
            crate::daemon_client::connect(rig.state.config_path.clone(), Some(socket)).await?;
        let receipt = observed.receipt().clone();
        drop(observed);
        let mut client =
            crate::daemon_client::connect_native(receipt, rig.state.workspace.clone()).await?;
        let (hook_tx, mut hook_rx) = mpsc::unbounded_channel();
        client.set_receive_test_hook(hook_tx);
        let (browser, helper) = tokio::io::duplex(65536);
        let (helper_input, helper_output) = tokio::io::split(helper);
        let forwarding = tokio::spawn(client.forward_native(helper_input, helper_output));
        let (browser_read, mut browser_write) = tokio::io::split(browser);
        let mut browser_read = BufReader::new(browser_read);
        browser_write.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"native-real-owner-fixture\",\"version\":\"1\"}}}\n").await?;
        let resume_init = tokio::time::timeout(Duration::from_secs(5), hook_rx.recv())
            .await?
            .context("initialize receive hook")?;
        browser_write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"initialized\",\"params\":{}}\n")
            .await?;
        tokio::task::yield_now().await;
        resume_init
            .send(())
            .map_err(|_| anyhow!("resume consumed initialization response"))?;
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), browser_read.read_line(&mut line)).await??;
        let ack: Value = serde_json::from_str(&line)?;
        assert_eq!(ack["id"], 1);
        assert_eq!(ack["result"]["capabilities"], json!(["native_runtime_v1"]));
        let base = json!({"jsonrpc":"2.0","id":2,"method":"runtime/thread/create","params":{"body":{"operation_key":"must-not-execute","model":"deepseek-v4-pro","mode":"agent"}}});
        let mut malformed = Vec::new();
        let mut frame = base.clone();
        frame["jsonrpc"] = json!("1.0");
        malformed.push(frame);
        let mut frame = base.clone();
        frame.as_object_mut().unwrap().remove("jsonrpc");
        malformed.push(frame);
        let mut frame = base.clone();
        frame["extra"] = json!({"permission_posture":"full_access"});
        malformed.push(frame);
        let mut frame = base.clone();
        frame["id"] = json!({"nested":2});
        malformed.push(frame);
        let mut frame = base.clone();
        frame["id"] = Value::Null;
        malformed.push(frame);
        malformed
            .push(json!({"jsonrpc":"2.0","method":"initialized","params":{"unexpected":true}}));
        malformed.push(json!({"jsonrpc":"2.0","method":"daemon/input_closed","params":{}}));
        for frame in malformed {
            browser_write
                .write_all(format!("{frame}\n").as_bytes())
                .await?;
            line.clear();
            tokio::time::timeout(Duration::from_secs(5), browser_read.read_line(&mut line))
                .await??;
            let response: Value = serde_json::from_str(&line)?;
            assert_eq!(response["error"]["code"], -32600);
            assert_eq!(response["error"]["data"]["outcome"], "not_started");
        }
        for (index, alias) in [
            "yolo",
            "4",
            "bypass",
            "bypass-permissions",
            "bypasspermissions",
            " YOLO ",
            "ByPaSs",
        ]
        .into_iter()
        .enumerate()
        {
            let frame = json!({"jsonrpc":"2.0","id":100 + index,"method":"runtime/thread/create","params":{"body":{"operation_key":format!("wire-bypass-{index}"),"model":"deepseek-v4-pro","mode":alias}}});
            browser_write
                .write_all(format!("{frame}\n").as_bytes())
                .await?;
            let resume = tokio::time::timeout(Duration::from_secs(5), hook_rx.recv())
                .await?
                .context("bypass refusal hook")?;
            resume.send(()).map_err(|_| anyhow!("resume bypass"))?;
            line.clear();
            tokio::time::timeout(Duration::from_secs(5), browser_read.read_line(&mut line))
                .await??;
            let response: Value = serde_json::from_str(&line)?;
            assert_eq!(response["id"], 100 + index);
            assert_eq!(response["error"]["code"], -32602);
            assert_eq!(response["error"]["data"]["outcome"], "not_started");
        }
        assert!(
            rig.state
                .runtime_threads
                .list_threads(ThreadListFilter::IncludeArchived, None)
                .await?
                .is_empty()
        );
        browser_write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":150,\"method\":\"runtime/threads/list\",\"params\":{\"limit\":0}}\n")
        .await?;
        let resume = tokio::time::timeout(Duration::from_secs(5), hook_rx.recv())
            .await?
            .context("limit refusal hook")?;
        resume.send(()).map_err(|_| anyhow!("resume limit"))?;
        line.clear();
        tokio::time::timeout(Duration::from_secs(5), browser_read.read_line(&mut line)).await??;
        let limit_err: Value = serde_json::from_str(&line)?;
        assert_eq!(limit_err["error"]["code"], -32602);
        assert_eq!(limit_err["error"]["data"]["status"], 400);
        assert_eq!(limit_err["error"]["data"]["code"], "native_runtime_refused");
        assert_eq!(limit_err["error"]["data"]["outcome"], "known");
        let foreign = rig.foreign().await?;
        let read_foreign = json!({"jsonrpc":"2.0","id":151,"method":"runtime/thread/read","params":{"thread_id":foreign}});
        browser_write
            .write_all(format!("{read_foreign}\n").as_bytes())
            .await?;
        let resume = tokio::time::timeout(Duration::from_secs(5), hook_rx.recv())
            .await?
            .context("foreign refusal hook")?;
        resume.send(()).map_err(|_| anyhow!("resume foreign"))?;
        line.clear();
        tokio::time::timeout(Duration::from_secs(5), browser_read.read_line(&mut line)).await??;
        let foreign_err: Value = serde_json::from_str(&line)?;
        assert_eq!(foreign_err["error"]["data"]["status"], 403);
        assert_eq!(foreign_err["error"]["data"]["code"], "workspace_mismatch");
        assert_eq!(foreign_err["error"]["data"]["outcome"], "known");
        browser_write
            .write_all(format!("{base}\n").as_bytes())
            .await?;
        let resume = tokio::time::timeout(Duration::from_secs(5), hook_rx.recv())
            .await?
            .context("create receive hook")?;
        resume.send(()).map_err(|_| anyhow!("resume create"))?;
        line.clear();
        tokio::time::timeout(Duration::from_secs(5), browser_read.read_line(&mut line)).await??;
        let created: Value = serde_json::from_str(&line)?;
        assert_eq!(created["id"], 2);
        assert!(created.get("error").is_none(), "{created}");
        assert_ne!(created["result"]["permission_posture"], "full_access");
        assert_ne!(created["result"]["auto_approve"], true);
        let thread_id = created["result"]["id"].as_str().context("created id")?;
        assert_eq!(
            rig.state
                .runtime_threads
                .list_threads(ThreadListFilter::IncludeArchived, None)
                .await?
                .len(),
            2
        );
        browser_write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":200,\"method\":\"runtime/threads/list\",\"params\":{\"limit\":10}}\n")
        .await?;
        let resume_list = tokio::time::timeout(Duration::from_secs(5), hook_rx.recv())
            .await?
            .context("list receive hook")?;
        browser_write
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":201,\"method\":\"runtime/capabilities\",\"params\":{}}\n",
        )
        .await?;
        tokio::task::yield_now().await;
        resume_list
            .send(())
            .map_err(|_| anyhow!("resume list response"))?;
        let resume_caps = tokio::time::timeout(Duration::from_secs(5), hook_rx.recv())
            .await?
            .context("capabilities receive hook")?;
        resume_caps
            .send(())
            .map_err(|_| anyhow!("resume capabilities response"))?;
        line.clear();
        tokio::time::timeout(Duration::from_secs(5), browser_read.read_line(&mut line)).await??;
        let list_resp: Value = serde_json::from_str(&line)?;
        assert_eq!(list_resp["id"], 200);
        assert_eq!(list_resp["result"].as_array().unwrap().len(), 1);
        line.clear();
        tokio::time::timeout(Duration::from_secs(5), browser_read.read_line(&mut line)).await??;
        let caps_resp: Value = serde_json::from_str(&line)?;
        assert_eq!(caps_resp["id"], 201);
        let seed = rig
            .state
            .runtime_threads
            .emit_event_for_test(thread_id, None, "fixture.seed", json!({}))
            .await?;
        let subscribe = json!({"jsonrpc":"2.0","id":202,"method":"runtime/events/subscribe","params":{"thread_id":thread_id,"since_seq":seed.seq}});
        browser_write
            .write_all(format!("{subscribe}\n").as_bytes())
            .await?;
        let resume_sub = tokio::time::timeout(Duration::from_secs(5), hook_rx.recv())
            .await?
            .context("subscribe ack hook")?;
        resume_sub
            .send(())
            .map_err(|_| anyhow!("resume subscribe ack"))?;
        line.clear();
        tokio::time::timeout(Duration::from_secs(5), browser_read.read_line(&mut line)).await??;
        let sub_ack: Value = serde_json::from_str(&line)?;
        assert_eq!(sub_ack["id"], 202);
        let emitted = rig
            .state
            .runtime_threads
            .emit_event_for_test(thread_id, None, "fixture.wire", json!({"ok":true}))
            .await?;
        let resume_event = tokio::time::timeout(Duration::from_secs(5), hook_rx.recv())
            .await?
            .context("event receive hook")?;
        browser_write
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":203,\"method\":\"runtime/capabilities\",\"params\":{}}\n",
        )
        .await?;
        tokio::task::yield_now().await;
        drop(hook_rx);
        resume_event
            .send(())
            .map_err(|_| anyhow!("resume event delivery"))?;
        line.clear();
        tokio::time::timeout(Duration::from_secs(5), browser_read.read_line(&mut line)).await??;
        let event_frame: Value = serde_json::from_str(&line)?;
        assert_eq!(event_frame["method"], "runtime/event");
        assert_eq!(event_frame["params"]["frame"]["sequence"], emitted.seq);
        line.clear();
        tokio::time::timeout(Duration::from_secs(5), browser_read.read_line(&mut line)).await??;
        let post_event_caps: Value = serde_json::from_str(&line)?;
        assert_eq!(post_event_caps["id"], 203);
        drop(browser_write);
        drop(browser_read);
        tokio::time::timeout(Duration::from_secs(5), forwarding).await???;
        shutdown.trigger();
        tokio::time::timeout(Duration::from_secs(5), server).await???;
        Ok(())
    })
}
