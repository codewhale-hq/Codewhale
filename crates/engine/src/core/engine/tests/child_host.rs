//! Actual Engine admission/registry contracts for captured child authority.
use super::*;
use crate::tools::subagent::engine::ChildAuthority;
use anyhow::anyhow;

fn fixture(
    workspace: &Path,
    scope: Option<Vec<String>>,
) -> (Engine, EngineHandle, Arc<ChildAuthority>, TurnRouteContext) {
    let api = {
        let __cw_config_init_0_0 = Some(true);
        let mut __cw_config_init_0_value: Config = Default::default();
        __cw_config_init_0_value.allow_shell = __cw_config_init_0_0;
        __cw_config_init_0_value
    }
    .with_legacy_root(Some("child-local-fixture".into()), None);
    let (mut parent, _parent_handle) = Engine::new(
        EngineConfig {
            workspace: workspace.into(),
            session_id: Some("origin-session".into()),
            allow_shell: true,
            snapshots_enabled: false,
            memory_enabled: false,
            ..Default::default()
        },
        &api,
    );
    parent.config.features.disable(Feature::Mcp);
    let route = TurnRouteContext {
        provider: ProviderKind::Deepseek,
        model: DEFAULT_TEXT_MODEL.into(),
        capabilities: Default::default(),
        limits: None,
        client: parent.codewhale_client.clone(),
        api_config: Box::new(api.clone()),
        locale_tag: parent.config.locale_tag.clone(),
        role_models: HashMap::new(),
        auto_model: false,
        reasoning_effort: None,
        reasoning_effort_auto: false,
    };
    let policy = TurnAuthority::from_effective_fields(
        AppMode::Agent,
        true,
        false,
        false,
        ApprovalMode::Suggest,
    );
    let context = parent.build_tool_context_for_turn(&policy, &route);
    let runtime = SubAgentRuntime::new(
        parent.codewhale_client.clone().unwrap(),
        DEFAULT_TEXT_MODEL.into(),
        context,
        true,
        None,
        parent.subagent_manager.clone(),
    )
    .with_api_config(api.clone())
    .with_approval_receipt_store(parent.approval_receipt_store.clone());
    let authority = ChildAuthority::capture(
        runtime,
        FleetRole::Scout,
        "child-a".into(),
        "inspect".into(),
        scope,
    );
    let (engine, handle) = Engine::new_child_admitted(
        EngineConfig {
            workspace: workspace.into(),
            model: DEFAULT_TEXT_MODEL.into(),
            ..Default::default()
        },
        &api,
        authority.clone(),
        SystemPrompt::Text("captured child role".into()),
        None,
    )
    .unwrap();
    (engine, handle, authority, route)
}

#[tokio::test(flavor = "current_thread")]
async fn child_constructor_reuses_captured_manager_services_namespace_and_live_authority() {
    let dir = tempdir().unwrap();
    let _home = crate::test_support::SealedHome::at(dir.path());
    let (engine, _handle, authority, route) = fixture(dir.path(), None);
    assert!(Arc::ptr_eq(
        &engine.subagent_manager,
        &authority.runtime.manager
    ));
    assert!(Arc::ptr_eq(
        &engine.shell_manager,
        &authority.runtime.context.shell_manager
    ));
    assert!(Arc::ptr_eq(
        &engine.file_read_tracker,
        &authority.runtime.context.file_read_tracker
    ));
    assert_eq!(engine.session.id, "origin-session");
    assert_eq!(engine.host_profile, EngineHostProfile::Child);
    assert!(!Arc::ptr_eq(
        &engine.live_runtime_authority,
        &authority
            .runtime
            .context
            .live_posture
            .as_ref()
            .unwrap()
            .state
    ));
    let context = engine.build_tool_context_for_turn(
        &TurnAuthority::from_effective_fields(
            AppMode::Agent,
            true,
            false,
            true,
            ApprovalMode::Auto,
        ),
        &route,
    );
    assert!(Arc::ptr_eq(
        &context.live_posture.as_ref().unwrap().state,
        &authority
            .runtime
            .context
            .live_posture
            .as_ref()
            .unwrap()
            .state,
    ));
    assert_eq!(context.owner_agent_id.as_deref(), Some("child-a"));
    assert_eq!(context.state_namespace, "origin-session");
    assert_eq!(context.shell_policy, authority.grant.shell_policy());
    let id = engine.new_tool_execution_id();
    assert!(uuid::Uuid::parse_str(id.strip_prefix("agent:child-a:approval:").unwrap()).is_ok());
}

#[tokio::test(flavor = "current_thread")]
async fn child_ceiling_filters_acp_foreground_catalog_without_optional_allow_list() {
    let dir = tempdir().unwrap();
    let _home = crate::test_support::SealedHome::at(dir.path());
    let (engine, _handle, _authority, route) = fixture(dir.path(), Some(vec!["read".into()]));
    let build = engine.acp_tool_build(
        &TurnAuthority::from_effective_fields(
            AppMode::Agent,
            true,
            false,
            true,
            ApprovalMode::Auto,
        ),
        &route,
        None,
    );
    assert!(build.surface.catalog.iter().any(|tool| tool.name == "read"));
    assert!(
        !build
            .surface
            .catalog
            .iter()
            .any(|tool| tool.name == "write")
    );
    assert!(
        !build
            .surface
            .catalog
            .iter()
            .any(|tool| tool.name == "agent")
    );
    let error = build
        .surface
        .registry
        .execute_full(
            "File",
            json!({"action":"write","path":"blocked.txt","content":"must not write"}),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ToolError::PermissionDenied { .. }));
    assert!(!dir.path().join("blocked.txt").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn child_registry_rejects_context_override_identity_before_tool_effect() {
    let dir = tempdir().unwrap();
    let _home = crate::test_support::SealedHome::at(dir.path());
    let (engine, _handle, _authority, route) = fixture(dir.path(), None);
    let build = engine.acp_tool_build(
        &TurnAuthority::from_effective_fields(
            AppMode::Agent,
            true,
            false,
            true,
            ApprovalMode::Auto,
        ),
        &route,
        None,
    );
    let override_context = build
        .surface
        .registry
        .context()
        .clone()
        .with_owner_agent("different-child", "different-child");
    let error = build
        .surface
        .registry
        .execute_rich_full_with_context(
            "File",
            json!({"action":"write","path":"blocked.txt","content":"must not write"}),
            Some(&override_context),
            || panic!("refused child admission must not publish activity"),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("identity"));
    assert!(!dir.path().join("blocked.txt").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn child_shell_ceiling_survives_every_context_policy_setter() {
    let dir = tempdir().unwrap();
    let _home = crate::test_support::SealedHome::at(dir.path());
    let (_engine, _handle, authority, _route) = fixture(dir.path(), None);
    let ceiling = authority.grant.shell_policy();
    assert_ne!(
        ceiling,
        codewhale_agent_contract::worker_profile::ShellPolicy::Full
    );
    let mut context = authority
        .context()
        .with_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::Full);
    assert_eq!(context.shell_policy, ceiling);
    context.set_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::None);
    assert_eq!(
        context.shell_policy,
        codewhale_agent_contract::worker_profile::ShellPolicy::None
    );
    context.set_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::Full);
    assert_eq!(context.shell_policy, ceiling);
}

#[tokio::test(flavor = "current_thread")]
async fn child_role_prompt_is_retained_by_the_existing_route_refresh_composer() {
    let dir = tempdir().unwrap();
    let _home = crate::test_support::SealedHome::at(dir.path());
    let (mut engine, _handle, _authority, _route) = fixture(dir.path(), None);
    engine.current_mode = AppMode::Plan;
    engine.refresh_system_prompt_with_reason("mode");
    match engine.session.system_prompt.as_ref().unwrap() {
        SystemPrompt::Text(text) => assert_eq!(text, "captured child role"),
        _ => panic!("fixture expected the captured role text"),
    }
    assert!(engine.host_managed_turns());
}

fn activate_fixture_control(engine: &mut Engine) -> u64 {
    let mut controls = engine.turn_controls.lock().unwrap();
    let control = controls.fresh();
    let id = control.id;
    controls.active = Some(control);
    id
}

#[tokio::test(flavor = "current_thread")]
async fn replacing_child_steer_drops_only_uncommitted_same_control_inputs() {
    let dir = tempdir().unwrap();
    let _home = crate::test_support::SealedHome::at(dir.path());
    let (mut engine, handle, _authority, _route) = fixture(dir.path(), None);
    let active = activate_fixture_control(&mut engine);
    let old = handle
        .reserve_steer()
        .await
        .unwrap()
        .send_with_outcome("superseded".into());
    // A future turn is retained, including its own exact original outcome.
    let future = {
        let mut controls = engine.turn_controls.lock().unwrap();
        let control = controls.fresh();
        let id = control.id;
        controls.pending.push_back(control);
        id
    };
    let (future_tx, mut future_rx) = tokio::sync::oneshot::channel();
    handle
        .tx_steer
        .send(handle::SteerInput {
            turn_id: Some(future),
            replace_pending: false,
            content: "future input".into(),
            outcome: Some(future_tx),
        })
        .await
        .unwrap();
    let replacement = handle
        .reserve_steer()
        .await
        .unwrap()
        .send_replacing_with_outcome("current replacement".into());
    let pending = engine.next_turn_steer().unwrap();
    assert!(pending.replace_pending);
    assert_eq!(old.await.unwrap(), handle::SteerOutcome::Dropped);
    assert!(matches!(
        future_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(pending.commit(), "current replacement");
    assert_eq!(replacement.await.unwrap(), handle::SteerOutcome::Accepted);
    assert!(engine.next_turn_steer().is_none());
    assert_eq!(
        engine
            .turn_controls
            .lock()
            .unwrap()
            .active
            .as_ref()
            .unwrap()
            .id,
        active
    );
    {
        let mut controls = engine.turn_controls.lock().unwrap();
        controls.active = controls.pending.pop_front();
    }
    assert_eq!(engine.next_turn_steer().unwrap().commit(), "future input");
    assert_eq!(future_rx.await.unwrap(), handle::SteerOutcome::Accepted);
}

#[tokio::test(flavor = "current_thread")]
async fn replacing_child_steer_preserves_committed_history_and_drops_on_cancelled_claim() {
    let dir = tempdir().unwrap();
    let _home = crate::test_support::SealedHome::at(dir.path());
    let (mut engine, handle, _authority, _route) = fixture(dir.path(), None);
    activate_fixture_control(&mut engine);
    let original = handle
        .reserve_steer()
        .await
        .unwrap()
        .send_with_outcome("already committed".into());
    let text = engine.next_turn_steer().unwrap().commit();
    engine
        .add_session_message(engine.user_text_message_with_turn_metadata(text))
        .await;
    assert_eq!(original.await.unwrap(), handle::SteerOutcome::Accepted);
    let replaced = handle
        .reserve_steer()
        .await
        .unwrap()
        .send_replacing_with_outcome("new instruction".into());
    let claimed = engine.next_turn_steer().unwrap();
    handle.cancel();
    drop(claimed);
    assert_eq!(replaced.await.unwrap(), handle::SteerOutcome::Dropped);
    assert!(
        engine
            .session
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .any(|b| matches!(b, ContentBlock::Text { text, .. } if text == "already committed"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn future_control_lookahead_stays_bounded_and_never_retargets_input() {
    let dir = tempdir().unwrap();
    let _home = crate::test_support::SealedHome::at(dir.path());
    let (mut engine, handle, _authority, _route) = fixture(dir.path(), None);
    activate_fixture_control(&mut engine);
    let future = {
        let mut controls = engine.turn_controls.lock().unwrap();
        let control = controls.fresh();
        let id = control.id;
        controls.pending.push_back(control);
        id
    };
    let cap = handle.tx_steer.max_capacity();
    for _ in 0..cap {
        handle
            .tx_steer
            .send(handle::SteerInput {
                turn_id: Some(future),
                replace_pending: false,
                content: "future only".into(),
                outcome: None,
            })
            .await
            .unwrap();
    }
    assert!(engine.next_turn_steer().is_none());
    assert_eq!(engine.queued_steers.len(), cap);
    for _ in 0..cap {
        handle
            .tx_steer
            .send(handle::SteerInput {
                turn_id: Some(future),
                replace_pending: false,
                content: "also future".into(),
                outcome: None,
            })
            .await
            .unwrap();
    }
    for _ in 0..3 {
        assert!(engine.next_turn_steer().is_none());
    }
    assert_eq!(engine.queued_steers.len(), cap);
    assert_eq!(engine.rx_steer.len(), cap);
    assert!(handle.tx_steer.try_reserve().is_err());
    handle.cancel();
    assert!(engine.cancel_token.is_cancelled());
    assert!(engine.session.messages.is_empty());
}

async fn actor_fixture(
    workspace: &Path,
    scripts: Vec<Vec<StreamEvent>>,
    max_steps: u32,
) -> (
    Engine,
    EngineHandle,
    Arc<crate::tools::subagent::engine::ChildJob>,
    Arc<crate::llm_client::mock::MockLlmClient>,
) {
    let (_old_engine, _old_handle, authority, _route) =
        fixture(workspace, Some(vec!["read".into()]));
    actor_fixture_from_authority(workspace, authority, scripts, max_steps).await
}

async fn actor_fixture_from_authority(
    workspace: &Path,
    authority: Arc<ChildAuthority>,
    scripts: Vec<Vec<StreamEvent>>,
    max_steps: u32,
) -> (
    Engine,
    EngineHandle,
    Arc<crate::tools::subagent::engine::ChildJob>,
    Arc<crate::llm_client::mock::MockLlmClient>,
) {
    let mut authority = authority.as_ref().clone();
    {
        let mut manager = authority.runtime.manager.write().await;
        authority.owner_agent_id = manager.insert_test_running_agent("core-child", workspace);
        manager.assign_test_session_owner(&authority.owner_agent_id, "origin-session");
    }
    let api = authority.runtime.api_config.as_deref().unwrap().clone();
    let authority = Arc::new(authority);
    let job = crate::tools::subagent::engine::ChildJob::admitted(
        authority.clone(),
        crate::tools::subagent::SubAgentAssignment {
            objective: "inspect evidence".into(),
            role: None,
            native_preset: None,
        },
        Instant::now(),
        max_steps,
        false,
        None,
    )
    .await
    .unwrap();
    let (mut engine, handle) = Engine::new_child_admitted(
        EngineConfig {
            workspace: workspace.into(),
            model: DEFAULT_TEXT_MODEL.into(),
            max_steps: job.max_steps,
            ..Default::default()
        },
        &api,
        authority,
        SystemPrompt::Text("inspect evidence".into()),
        None,
    )
    .unwrap();
    let mock = Arc::new(
        crate::llm_client::mock::MockLlmClient::new(scripts).with_model(DEFAULT_TEXT_MODEL),
    );
    engine.model_client = Some(mock.clone());
    engine.install_child_job(job.clone(), Vec::new()).unwrap();
    let spec = engine
        .child_turn_spec("inspect evidence".into(), job.authority.grant.scope.clone())
        .unwrap();
    handle.send(Op::SendMessage(spec)).await.unwrap();
    (engine, handle, job, mock)
}

#[tokio::test(flavor = "current_thread")]
async fn child_work_can_finish_on_the_final_owner_selected_turn() {
    use crate::llm_client::mock::canned;
    let dir = tempdir().unwrap();
    let _home = crate::test_support::SealedHome::at(dir.path());
    std::fs::write(dir.path().join("evidence.txt"), "exact evidence bytes").unwrap();
    let (engine, handle, job, mock) = actor_fixture(
        dir.path(),
        vec![
            canned::tool_call_turn("read-evidence", "read", r#"{"path":"evidence.txt"}"#),
            canned::simple_text_turn("Read evidence.txt; work remains."),
        ],
        2,
    )
    .await;
    let collect = async {
        let mut events = handle.rx_event.write().await;
        let mut seen = Vec::new();
        while let Some(event) = events.recv().await {
            seen.push(event);
        }
        seen
    };
    let (result, events) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(Box::pin(engine.run_child()), collect)
    })
    .await
    .expect("same actor must settle the full work allowance");
    let result = result.unwrap();
    assert_eq!(
        result.status,
        crate::tools::subagent::SubAgentStatus::Completed
    );
    assert_eq!(mock.call_count(), 2);
    let requests = mock.captured_requests();
    assert!(
        requests[0]
            .tools
            .as_ref()
            .unwrap()
            .iter()
            .any(|tool| tool.name == "read")
    );
    assert!(
        requests[1]
            .tools
            .as_ref()
            .unwrap()
            .iter()
            .any(|tool| tool.name == "read")
    );
    assert_eq!(job.steps(), 2);
    assert!(requests[1].messages.iter().flat_map(|m| &m.content).any(
        |b| matches!(b, ContentBlock::ToolResult { content, .. } if content.contains("exact evidence bytes"))
    ));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::ToolCallComplete { .. }))
            .count(),
        1
    );
    assert!(result.checkpoint.is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn queued_child_cancel_preserves_checkpoint_without_dispatching_a_fresh_request() {
    use crate::llm_client::mock::canned;
    let dir = tempdir().unwrap();
    let _home = crate::test_support::SealedHome::at(dir.path());
    let (engine, handle, job, mock) = actor_fixture(
        dir.path(),
        vec![canned::simple_text_turn("must not dispatch")],
        2,
    )
    .await;
    job.authority.runtime.cancel_token.cancel();
    handle.cancel();
    let collect = async {
        let mut events = handle.rx_event.write().await;
        while events.recv().await.is_some() {}
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(Box::pin(engine.run_child()), collect)
    })
    .await
    .unwrap();
    let result = result.unwrap();
    assert_eq!(
        result.status,
        crate::tools::subagent::SubAgentStatus::Cancelled
    );
    assert_eq!(mock.call_count(), 0);
    assert!(result.checkpoint.is_some());
    let checkpoint = result.checkpoint.as_ref().unwrap();
    let durable = crate::tools::subagent::load_subagent_transcript_artifact(
        dir.path(),
        &job.authority.owner_agent_id,
    )
    .expect("cancelled Core Session is durably saved under its captured worker");
    assert_eq!(checkpoint.agent_id, job.authority.owner_agent_id);
    assert_eq!(checkpoint.message_count, durable.len());
    assert_eq!(
        serde_json::to_value(&checkpoint.messages).unwrap(),
        serde_json::to_value(&durable).unwrap()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn actual_child_transport_services_approval_and_cancel_with_saturated_steer_queue() {
    use crate::approval_log::{ApprovalDecider, ApprovalOutcome};
    use crate::llm_client::mock::canned;
    use crate::tools::subagent::engine::{drive_child_actor, send_test_child_input};
    use crate::tools::subagent::{ChildApprovalOutcome, SubAgentStatus};
    for (cancel, by, approved) in [
        (false, ApprovalDecider::User, true),
        (false, ApprovalDecider::SessionRule, true),
        (false, ApprovalDecider::Posture, false),
        (false, ApprovalDecider::Host, false),
        (true, ApprovalDecider::User, true),
    ] {
        let dir = tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(dir.path());
        let (_old, _old_handle, captured, _) = fixture(dir.path(), Some(vec!["bash".into()]));
        let mut runtime = captured.runtime.clone();
        runtime.worker_profile =
            codewhale_agent_contract::worker_profile::WorkerRuntimeProfile::for_role(
                FleetRole::Worker,
            );
        runtime.parent_can_prompt = true;
        runtime.context.auto_approve = false;
        runtime.context.approval_mode = ApprovalMode::Suggest;
        let (parent_tx, mut parent_rx) = tokio::sync::mpsc::channel(16);
        runtime.event_tx = Some(parent_tx);
        let authority = ChildAuthority::capture(
            runtime,
            FleetRole::Worker,
            "temporary".into(),
            "worker".into(),
            Some(vec!["bash".into()]),
        );
        let (core, handle, job, mock) = actor_fixture_from_authority(
            dir.path(),
            authority,
            vec![
                canned::tool_call_turn(
                    "held-effect",
                    "bash",
                    r#"{"command":"printf child > effect.txt"}"#,
                ),
                canned::simple_text_turn("Effect settled."),
            ],
            4,
        )
        .await;
        let receipt_store = core.approval_receipt_store.clone().unwrap();
        let receipt_session = core.session.id.clone();
        let manager = job.authority.runtime.manager.clone();
        let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel();
        let pending = Arc::new(std::sync::atomic::AtomicUsize::new(2));
        let completed = CancellationToken::new();
        let control = async {
            let approval_id = loop {
                if let Event::ApprovalRequired { id, .. } = parent_rx
                    .recv()
                    .await
                    .expect("actual parent approval stream")
                {
                    break id;
                }
            };
            let cap = handle.tx_steer.max_capacity();
            let mut outcomes = Vec::new();
            for index in 0..cap {
                outcomes.push(
                    handle
                        .reserve_steer()
                        .await
                        .unwrap()
                        .send_with_outcome(format!("queued {index}")),
                );
            }
            assert_eq!(handle.tx_steer.capacity(), 0);
            send_test_child_input(
                &input_tx,
                "replace old queued inputs",
                true,
                pending.clone(),
            );
            send_test_child_input(&input_tx, "latest instruction", true, pending.clone());
            // The transport is live with an exact pending Core card and a
            // full existing steer queue; reserving input cannot own the poll.
            for _ in 0..32 {
                tokio::task::yield_now().await;
            }
            assert!(
                !manager
                    .read()
                    .await
                    .pending_requests_for_agent(&job.authority.owner_agent_id)
                    .is_empty()
            );
            if cancel {
                job.authority.runtime.cancel_token.cancel();
            } else {
                let answer = if approved {
                    ChildApprovalOutcome::Approved { by }
                } else {
                    ChildApprovalOutcome::Denied { by }
                };
                assert!(
                    manager
                        .write()
                        .await
                        .resolve_child_approval(&approval_id, answer)
                );
            }
            drop(input_tx);
            loop {
                tokio::select! {
                    () = completed.cancelled() => break,
                    event = parent_rx.recv() => if event.is_none() { break; },
                }
            }
            for outcome in outcomes {
                let settled = outcome.await.unwrap();
                if cancel {
                    assert_eq!(settled, handle::SteerOutcome::Dropped);
                }
            }
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                async {
                    let result =
                        drive_child_actor(core, handle.clone(), job.clone(), input_rx).await;
                    completed.cancel();
                    result
                },
                control
            )
        })
        .await
        .expect("approval/cancellation must stay serviceable with a full steer queue");
        let result = result.unwrap();
        assert_eq!(
            pending.load(std::sync::atomic::Ordering::Acquire),
            0,
            "every input settles after actor join"
        );
        assert!(
            manager
                .read()
                .await
                .pending_requests_for_agent(&job.authority.owner_agent_id)
                .is_empty()
        );
        if cancel {
            assert_eq!(result.status, SubAgentStatus::Cancelled);
            assert!(!dir.path().join("effect.txt").exists());
            assert_eq!(mock.call_count(), 1);
        } else {
            assert_eq!(result.status, SubAgentStatus::Completed);
            if approved {
                assert_eq!(
                    std::fs::read_to_string(dir.path().join("effect.txt")).unwrap(),
                    "child"
                );
            } else {
                assert!(!dir.path().join("effect.txt").exists());
            }
            assert_eq!(mock.call_count(), 2);
            let replay = receipt_store.replay(&receipt_session).unwrap();
            assert!(replay.unmatched_asks.is_empty());
            assert_eq!(replay.completed.len(), 1);
            assert_eq!(replay.completed[0].decided_by, Some(by));
            assert_eq!(
                replay.completed[0].outcome,
                if approved {
                    ApprovalOutcome::ApprovedOnce
                } else {
                    ApprovalOutcome::Denied
                }
            );
        }
        assert!(result.checkpoint.is_some());
        let checkpoint = result.checkpoint.as_ref().unwrap();
        let durable = crate::tools::subagent::load_subagent_transcript_artifact(
            dir.path(),
            &job.authority.owner_agent_id,
        )
        .expect("approval/cancellation returns the saved canonical Core Session");
        assert!(!durable.is_empty());
        assert_eq!(checkpoint.agent_id, job.authority.owner_agent_id);
        assert_eq!(checkpoint.message_count, durable.len());
        assert_eq!(
            serde_json::to_value(&checkpoint.messages).unwrap(),
            serde_json::to_value(&durable).unwrap()
        );
    }
}

struct ChildTimeoutClient {
    successful: Arc<crate::llm_client::mock::MockLlmClient>,
    attempts: std::sync::atomic::AtomicUsize,
    held_attempts: usize,
    body: bool,
}
impl crate::llm_client::LlmClient for ChildTimeoutClient {
    fn provider_name(&self) -> &'static str {
        "mock"
    }
    fn model(&self) -> &str {
        DEFAULT_TEXT_MODEL
    }
    async fn create_message(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> Result<codewhale_models::MessageResponse> {
        Err(anyhow!("child must use the canonical streaming producer"))
    }
    async fn create_message_stream(
        &self,
        request: codewhale_models::MessageRequest,
    ) -> Result<crate::llm_client::StreamEventBox> {
        use futures_util::StreamExt;
        let attempt = self
            .attempts
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if attempt < self.held_attempts {
            if !self.body {
                return std::future::pending().await;
            }
            let mut start = crate::llm_client::mock::canned::message_start("timed-out-body");
            if let StreamEvent::MessageStart { message } = &mut start {
                message.usage = Usage {
                    input_tokens: 17,
                    output_tokens: 3,
                    ..Default::default()
                };
            }
            return Ok(Box::pin(
                futures_util::stream::once(async { Ok(start) })
                    .chain(futures_util::stream::pending()),
            ));
        }
        crate::llm_client::LlmClient::create_message_stream(self.successful.as_ref(), request).await
    }
}

#[tokio::test(flavor = "current_thread")]
async fn child_step_timeout_bounds_open_and_body_with_one_outer_retry_and_exact_usage_sources() {
    use crate::llm_client::mock::canned;
    for body in [false, true] {
        let dir = tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(dir.path());
        let (_old, _old_handle, captured, _) = fixture(dir.path(), Some(Vec::new()));
        let mut authority = captured.as_ref().clone();
        authority.runtime.step_api_timeout = Duration::from_millis(25);
        authority.runtime.api_timeout_retry_base_backoff = Duration::from_millis(1);
        let authority = Arc::new(authority);
        let mut success = canned::simple_text_turn("Recovered without restarting the step.");
        for event in &mut success {
            if matches!(event, StreamEvent::MessageDelta { .. }) {
                *event = canned::message_delta(
                    "end_turn",
                    Some(Usage {
                        input_tokens: 10,
                        output_tokens: 6,
                        ..Default::default()
                    }),
                );
            }
        }
        let (mut core, handle, job, successful) =
            actor_fixture_from_authority(dir.path(), authority, vec![success], 3).await;
        let timeout_client = Arc::new(ChildTimeoutClient {
            successful: successful.clone(),
            attempts: std::sync::atomic::AtomicUsize::new(0),
            held_attempts: 2,
            body,
        });
        core.model_client = Some(timeout_client.clone());
        let (_input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            crate::tools::subagent::engine::drive_child_actor(core, handle, job.clone(), input_rx),
        )
        .await
        .expect("per-step timeout includes both stream opening and reading")
        .unwrap();
        assert_eq!(
            result.status,
            crate::tools::subagent::SubAgentStatus::Completed
        );
        assert_eq!(
            timeout_client
                .attempts
                .load(std::sync::atomic::Ordering::Acquire),
            3
        );
        assert_eq!(
            job.steps(),
            1,
            "retries do not consume a fresh logical work step"
        );
        assert_eq!(successful.call_count(), 1);
        let record = job
            .authority
            .runtime
            .manager
            .read()
            .await
            .get_worker_record(&job.authority.owner_agent_id)
            .unwrap();
        if body {
            assert_eq!(
                record.usage_source_fingerprints.len(),
                3,
                "each opened response settles once before its retry"
            );
            assert_eq!(record.usage.input_tokens, Some(44));
            assert_eq!(record.usage.output_tokens, Some(12));
        } else {
            assert_eq!(record.usage_source_fingerprints.len(), 3);
            assert_eq!(record.missing_usage_sources.len(), 2);
            assert_eq!(record.usage.input_tokens, Some(10));
            assert_eq!(record.usage.output_tokens, Some(6));
            assert!(record.missing_usage_sources.values().all(|coverage| {
                coverage.reason
                    == crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown
            }));
            assert!(record.has_unreported_usage);
        }
        assert!(
            !job.can_replace_first_request(),
            "opened/accepted response forbids route replacement"
        );
        assert!(result.checkpoint.is_some());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn child_timeout_exhaustion_interrupts_with_checkpoint_and_never_dispatches_report_as_retry()
{
    use crate::llm_client::mock::canned;
    let dir = tempdir().unwrap();
    let _home = crate::test_support::SealedHome::at(dir.path());
    let (_old, _old_handle, captured, _) = fixture(dir.path(), Some(Vec::new()));
    let mut authority = captured.as_ref().clone();
    authority.runtime.step_api_timeout = Duration::from_millis(10);
    authority.runtime.api_timeout_retry_base_backoff = Duration::from_millis(1);
    let (mut core, handle, job, successful) = actor_fixture_from_authority(
        dir.path(),
        Arc::new(authority),
        vec![canned::simple_text_turn("must not execute")],
        3,
    )
    .await;
    let timeout_client = Arc::new(ChildTimeoutClient {
        successful: successful.clone(),
        attempts: std::sync::atomic::AtomicUsize::new(0),
        held_attempts: usize::MAX,
        body: false,
    });
    core.model_client = Some(timeout_client.clone());
    let (_input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        crate::tools::subagent::engine::drive_child_actor(core, handle, job.clone(), input_rx),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(result.status, crate::tools::subagent::SubAgentStatus::Interrupted(ref reason)
        if reason.contains("6 API attempt(s)"))
    );
    assert_eq!(
        timeout_client
            .attempts
            .load(std::sync::atomic::Ordering::Acquire),
        6
    );
    assert_eq!(successful.call_count(), 0);
    assert_eq!(job.steps(), 1);
    assert!(result.checkpoint.is_some());
    assert!(result.needs_input.is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn child_cancelled_dispatched_open_keeps_unknown_origin_while_queued_cancel_has_none() {
    use crate::llm_client::mock::canned;
    let dir = tempdir().unwrap();
    let _home = crate::test_support::SealedHome::at(dir.path());
    let _cost = crate::cost_status::test_scope();
    let (_old, _handle, captured, _) = fixture(dir.path(), Some(Vec::new()));
    let (mut core, handle, job, successful) = actor_fixture_from_authority(
        dir.path(),
        captured,
        vec![canned::simple_text_turn("never received")],
        3,
    )
    .await;
    let held = Arc::new(ChildTimeoutClient {
        successful: successful.clone(),
        attempts: std::sync::atomic::AtomicUsize::new(0),
        held_attempts: usize::MAX,
        body: false,
    });
    core.model_client = Some(held.clone());
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let transport =
        crate::tools::subagent::engine::drive_child_actor(core, handle.clone(), job.clone(), rx);
    let cancel = async {
        while held.attempts.load(std::sync::atomic::Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
        handle.cancel();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(transport, cancel)
    })
    .await
    .expect("cancellation stays serviced while stream opening is pending");
    let result = result.unwrap();
    assert!(matches!(
        result.status,
        crate::tools::subagent::SubAgentStatus::Interrupted(_)
    ));
    assert_eq!(successful.call_count(), 0);
    let record = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let record = job
                .authority
                .runtime
                .manager
                .read()
                .await
                .get_worker_record(&job.authority.owner_agent_id)
                .unwrap();
            if !record.missing_usage_sources.is_empty() {
                break record;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(record.missing_usage_sources.len(), 1);
    assert!(
        record
            .missing_usage_sources
            .values()
            .all(|coverage| coverage.reason
                == crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown)
    );
    assert!(record.usage.total_tokens.is_none());
    assert!(record.has_unreported_usage);
    let pending = crate::cost_status::drain();
    assert_eq!(pending.priced_turns, 0);
    assert_eq!(pending.unpriced_turns, 1);
    assert!(pending.unpriced_reasons.contains("request_outcome_unknown"));
    // The separate queued-cancellation case above asserts zero dispatches.
    // This case requires the actual model invocation to begin before cancel.
}

#[tokio::test(flavor = "current_thread")]
async fn child_completion_and_cancel_do_not_consume_parent_pending_posture_revision() {
    use crate::llm_client::mock::canned;
    for cancelled in [false, true] {
        let dir = tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(dir.path());
        let (engine, handle, job, mock) =
            actor_fixture(dir.path(), vec![canned::simple_text_turn("done")], 2).await;
        let parent = job
            .authority
            .runtime
            .context
            .live_posture
            .as_ref()
            .unwrap()
            .state
            .clone();
        let prior_applied = {
            let mut state = parent.lock().unwrap();
            state.authority.approval_mode = ApprovalMode::Never;
            state.revision += 1;
            state.applied_revision
        };
        if cancelled {
            job.authority.runtime.cancel_token.cancel();
            handle.cancel();
        }
        let collect = async {
            let mut events = handle.rx_event.write().await;
            while events.recv().await.is_some() {}
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(Box::pin(engine.run_child()), collect)
        })
        .await
        .unwrap();
        assert_eq!(
            result.unwrap().status,
            if cancelled {
                crate::tools::subagent::SubAgentStatus::Cancelled
            } else {
                crate::tools::subagent::SubAgentStatus::Completed
            }
        );
        assert_eq!(mock.call_count(), usize::from(!cancelled));
        let state = parent.lock().unwrap();
        assert_eq!(state.applied_revision, prior_applied);
        assert!(state.revision > state.applied_revision);
        assert_eq!(state.authority.approval_mode, ApprovalMode::Never);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn actual_child_tool_output_cap_reaches_core_fanout_with_full_artifact() {
    use crate::llm_client::mock::canned;
    let dir = tempdir().unwrap();
    let _home = crate::test_support::SealedHome::at(dir.path());
    let raw = "owned-cap-evidence".repeat(32);
    std::fs::write(dir.path().join("evidence.txt"), &raw).unwrap();
    let (_old, _old_handle, authority, _route) = fixture(dir.path(), Some(vec!["read".into()]));
    let mut authority = authority.as_ref().clone();
    authority.runtime.max_output_tokens = Some(std::num::NonZeroU32::new(2).unwrap());
    let (engine, handle, _job, mock) = actor_fixture_from_authority(
        dir.path(),
        Arc::new(authority),
        vec![
            canned::tool_call_turn("read-capped", "read", r#"{"path":"evidence.txt"}"#),
            canned::simple_text_turn("Read the evidence."),
        ],
        2,
    )
    .await;
    let collect = async {
        let mut rx = handle.rx_event.write().await;
        let mut seen = Vec::new();
        while let Some(event) = rx.recv().await {
            seen.push(event);
        }
        seen
    };
    let (result, events) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(Box::pin(engine.run_child()), collect)
    })
    .await
    .expect("actual child output projection must settle");
    result.unwrap();
    assert_eq!(mock.call_count(), 2);
    let completed = events
        .iter()
        .filter_map(|event| match event {
            Event::ToolCallComplete {
                result: Ok(output), ..
            } => Some(output),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(completed.len(), 1);
    let output = completed[0];
    assert!(output.success);
    assert!(output.content.len() <= 6 + "\n[truncated: true]".len());
    assert!(output.content.ends_with("\n[truncated: true]"));
    let metadata = output
        .metadata
        .as_ref()
        .expect("small explicit cap still saves raw bytes");
    let path = metadata["artifact_path"].as_str().unwrap();
    let saved = std::fs::read_to_string(path).unwrap();
    assert!(
        saved.contains(&raw),
        "artifact must contain full original tool evidence"
    );
    assert!(saved.len() > output.content.len());
    assert_eq!(
        metadata["content_digest"],
        format!(
            "sha256:{}",
            codewhale_runtime::hashing::sha256_hex(saved.as_bytes())
        )
    );
    assert_eq!(metadata["truncated"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn captured_child_output_caps_preserve_utf8_error_kinds_metadata_and_raw_bytes() {
    use super::super::turn_loop::preserve_tool_output_before_fanout;
    use crate::tools::spec::ToolSpec as _;
    use base64::Engine as _;
    let dir = tempdir().unwrap();
    let _home = crate::test_support::SealedHome::at(dir.path());
    let (default_child, _default_handle, authority, _route) = fixture(dir.path(), None);
    assert_eq!(
        default_child.child_tool_result_token_cap().unwrap().get(),
        10_000
    );
    let default_raw = "好".repeat(15_000);
    let output = preserve_tool_output_before_fanout(
        Ok(RichToolResult::plain(ToolResult::success(
            default_raw.clone(),
        ))),
        ProviderKind::Deepseek,
        DEFAULT_TEXT_MODEL,
        (None, None),
        "origin-session",
        ("default-cap", "read"),
        default_child.child_tool_result_token_cap(),
    )
    .await
    .unwrap()
    .into_result();
    assert_eq!(output.content.len(), 30_000 + "\n[truncated: true]".len());
    assert!(output.content.ends_with("\n[truncated: true]"));
    let default_path = output.metadata.as_ref().unwrap()["artifact_path"]
        .as_str()
        .unwrap();
    assert_eq!(std::fs::read_to_string(default_path).unwrap(), default_raw);

    let mut authority = authority.as_ref().clone();
    authority.runtime.max_output_tokens = Some(std::num::NonZeroU32::new(2).unwrap());
    let retrieval_context = authority.runtime.context.clone();
    let api = authority.runtime.api_config.as_deref().unwrap().clone();
    let (child, _child_handle) = Engine::new_child_admitted(
        EngineConfig {
            workspace: dir.path().into(),
            model: DEFAULT_TEXT_MODEL.into(),
            ..Default::default()
        },
        &api,
        Arc::new(authority),
        SystemPrompt::Text("captured child".into()),
        None,
    )
    .unwrap();
    let raw = "好好好1234567".to_string();
    assert_eq!(raw.len(), 16);
    let cases = [
        ("denied-cap", ToolError::permission_denied(raw.clone())),
        ("cancelled-cap", ToolError::cancelled(raw.clone())),
        (
            "failed-cap",
            ToolError::execution_failed_with_metadata(
                raw.clone(),
                json!({"exit_code": 42, "status": "failed"}),
            ),
        ),
        (
            "failed-scalar-cap",
            ToolError::execution_failed_with_metadata(raw.clone(), json!(["original", 42])),
        ),
    ];
    for (id, original) in cases {
        let error = preserve_tool_output_before_fanout(
            Err(original),
            ProviderKind::Deepseek,
            DEFAULT_TEXT_MODEL,
            (None, None),
            "origin-session",
            (id, "read"),
            child.child_tool_result_token_cap(),
        )
        .await
        .unwrap_err();
        let content = match &error {
            ToolError::PermissionDenied { message } if id == "denied-cap" => message,
            ToolError::Cancelled { message } if id == "cancelled-cap" => message,
            ToolError::ExecutionFailed { message, metadata } if id == "failed-cap" => {
                assert_eq!(metadata.as_ref().unwrap()["exit_code"], 42);
                assert_eq!(metadata.as_ref().unwrap()["status"], "failed");
                assert!(metadata.as_ref().unwrap()["artifact_id"].is_string());
                message
            }
            ToolError::ExecutionFailed { message, metadata } if id == "failed-scalar-cap" => {
                assert_eq!(metadata.as_ref(), Some(&json!(["original", 42])));
                message
            }
            _ => panic!("output projection changed typed error classification: {error:?}"),
        };
        assert!(content.starts_with("好好\n[truncated: true]\n"));
        let artifact = codewhale_runtime::artifacts::artifact_id_for_tool_call(id);
        assert!(content.ends_with(&format!(
            "omitted range recovery: retrieve_tool_result ref=\"{artifact}\""
        )));
        assert!(content.len() <= 6 + "\n[truncated: true]".len() + 80);
        let relative = codewhale_runtime::artifacts::session_artifact_relative_path(&artifact);
        let path = codewhale_runtime::artifacts::session_artifact_absolute_path(
            "origin-session",
            &relative,
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), raw);
        let retrieved = crate::tools::tool_result_retrieval::RetrieveToolResultTool
            .execute(
                json!({"ref": artifact, "mode": "bytes"}),
                &retrieval_context,
            )
            .await
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&retrieved.content).unwrap();
        assert_eq!(payload["total_bytes"], raw.len());
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(payload["data"].as_str().unwrap())
                .unwrap(),
            raw.as_bytes(),
        );
    }
    // An immutable-ID conflict is a real save failure, even for a tiny cap.
    // The existing evidence must survive and no false retrieval receipt may
    // describe the different, unsaved bytes.
    let artifact = codewhale_runtime::artifacts::artifact_id_for_tool_call("save-failed-cap");
    let (path, _) = codewhale_runtime::artifacts::write_session_artifact_immutable(
        "origin-session",
        &artifact,
        b"prior immutable bytes",
    )
    .unwrap();
    let error = preserve_tool_output_before_fanout(
        Err(ToolError::execution_failed_with_metadata(
            raw.clone(),
            json!({"exit_code": 13, "status": "refused"}),
        )),
        ProviderKind::Deepseek,
        DEFAULT_TEXT_MODEL,
        (None, None),
        "origin-session",
        ("save-failed-cap", "read"),
        child.child_tool_result_token_cap(),
    )
    .await
    .unwrap_err();
    let ToolError::ExecutionFailed { message, metadata } = error else {
        panic!("output preservation changed the typed error");
    };
    assert!(message.starts_with("好好\n[truncated: true]\n"));
    assert!(message.contains("full output could not be saved"));
    assert!(message.ends_with("omitted range recovery: re-run with narrower output"));
    assert!(!message.contains("retrieve_tool_result"));
    assert!(!message.contains(&artifact));
    assert!(message.len() <= 6 + "\n[truncated: true]".len() + 100);
    let metadata = metadata.unwrap();
    assert_eq!(metadata["exit_code"], 13);
    assert_eq!(metadata["status"], "refused");
    assert_eq!(metadata["output_persistence_failed"], true);
    assert_eq!(metadata["truncated"], true);
    assert!(metadata.get("artifact_id").is_none());
    assert_eq!(std::fs::read(path).unwrap(), b"prior immutable bytes");
    // The shared Normal/RLM lane remains byte-identical with no Child cap.
    let ordinary = preserve_tool_output_before_fanout(
        Err(ToolError::permission_denied(raw.clone())),
        ProviderKind::Deepseek,
        DEFAULT_TEXT_MODEL,
        (None, None),
        "origin-session",
        ("ordinary", "read"),
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(ordinary, ToolError::PermissionDenied { message } if message == raw));
}

#[tokio::test(flavor = "current_thread")]
async fn detached_child_catastrophic_shell_floor_uses_captured_origin_in_every_posture() {
    use crate::llm_client::mock::canned;
    for approval_mode in [
        ApprovalMode::Bypass,
        ApprovalMode::Auto,
        ApprovalMode::Suggest,
        ApprovalMode::Never,
    ] {
        let dir = tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(dir.path());
        let (_old, _handle, captured, _) = fixture(dir.path(), Some(vec!["bash".into()]));
        let mut runtime = captured.runtime.clone();
        runtime.worker_profile =
            codewhale_agent_contract::worker_profile::WorkerRuntimeProfile::for_role(
                FleetRole::Worker,
            );
        runtime.context.live_posture = None;
        runtime.context.approval_mode = approval_mode;
        runtime.context.auto_approve = approval_mode == ApprovalMode::Bypass;
        assert!(!runtime.has_foreground_ownership());
        runtime.parent_can_prompt = false;
        let authority = ChildAuthority::capture(
            runtime,
            FleetRole::Worker,
            "temporary".into(),
            "worker".into(),
            Some(vec!["bash".into()]),
        );
        let (core, handle, _job, mock) = actor_fixture_from_authority(
            dir.path(),
            authority,
            vec![
                canned::tool_call_turn(
                    "detached-floor",
                    "bash",
                    r#"{"command":"dd if=/dev/zero of=/dev/null count=0"}"#,
                ),
                canned::simple_text_turn("Held the destructive background call."),
            ],
            4,
        )
        .await;
        let collect = async {
            let mut receiver = handle.rx_event.write().await;
            let mut events = Vec::new();
            while let Some(event) = receiver.recv().await {
                events.push(event);
            }
            events
        };
        let (result, events) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(Box::pin(core.run_child()), collect)
        })
        .await
        .expect("detached safety floor never waits for a missing human host");
        result.unwrap();
        assert!(mock.call_count() >= 1);
        let held = !matches!(approval_mode, ApprovalMode::Bypass);
        if held {
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    Event::ToolCallComplete { result: Err(error), .. }
                        if error.to_string().contains("destructive background")
                            || error.to_string().contains("no host that can answer this approval")
                )),
                "{approval_mode:?}: the canonical child planner must hold the call: {events:?}"
            );
            assert!(
                !events.iter().any(|event| matches!(
                    event,
                    Event::ToolGateDecision {
                        gate: crate::core::events::ToolGate::AutoReviewGuardian,
                        ..
                    }
                )),
                "the deterministic catastrophic-action floor cannot become model self-approval"
            );
            assert!(
                !events.iter().any(|event| matches!(
                    event,
                    Event::ToolCallComplete { result: Ok(result), .. }
                        if result.content.contains("records out")
                )),
                "the harmless dd fixture must never reach the shell"
            );
        } else {
            // Full Access skips the floor: the captured origin does not
            // strand the call, and the harmless fixture actually runs.
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, Event::ToolCallComplete { result: Ok(_), .. })),
                "Full Access runs the detached call: {events:?}"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn child_gate_observation_is_owned_once_and_full_channel_respects_original_cancellation() {
    use crate::core::events::{ToolGate, ToolGateVerdict};
    use crate::tools::subagent::engine::forward_child_gate_observation;
    for cancel_parent in [false, true] {
        let dir = tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(dir.path());
        let (_engine, _handle, authority, _) = fixture(dir.path(), None);
        let mut runtime = authority.runtime.clone();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        runtime.event_tx = Some(sender.clone());
        runtime.tool_timeout = Duration::from_secs(60);
        let turn_cancel = tokio_util::sync::CancellationToken::new();
        let receipt = Event::ToolGateDecision {
            agent_id: None,
            tool_id: "canonical-call".into(),
            tool_name: "bash".into(),
            gate: ToolGate::AutoReviewGuardian,
            decision: ToolGateVerdict::Denied,
            risk: Some("high".into()),
            reason: "actual reviewer held the call".into(),
        };
        sender
            .send(Event::status("preexisting caller observation"))
            .await
            .unwrap();
        let forward = forward_child_gate_observation(
            &runtime,
            &authority.owner_agent_id,
            receipt.clone(),
            &turn_cancel,
            None,
        );
        tokio::pin!(forward);
        tokio::select! {
            () = &mut forward => panic!("full channel must exercise the capacity wait"),
            () = tokio::time::sleep(Duration::from_millis(10)) => {},
        }
        if cancel_parent {
            runtime.cancel_token.cancel();
        } else {
            turn_cancel.cancel();
        }
        tokio::time::timeout(Duration::from_millis(250), &mut forward)
            .await
            .expect("original cancellation bounds a full observational channel");
        assert!(
            matches!(receiver.recv().await, Some(Event::Status { message, .. })
            if message == "preexisting caller observation")
        );
        assert!(
            receiver.try_recv().is_err(),
            "a full cancelled queue cannot duplicate a receipt"
        );
        // Available capacity may carry the already-decided terminal receipt,
        // even after cancellation, without making another gate decision.
        forward_child_gate_observation(
            &runtime,
            &authority.owner_agent_id,
            receipt,
            &turn_cancel,
            None,
        )
        .await;
        assert!(
            matches!(receiver.recv().await, Some(Event::ToolGateDecision {
            agent_id: Some(owner), tool_id, gate: ToolGate::AutoReviewGuardian,
            decision: ToolGateVerdict::Denied, risk: Some(risk), reason, ..
        }) if owner == authority.owner_agent_id && tool_id == "canonical-call"
            && risk == "high" && reason == "actual reviewer held the call")
        );
        assert!(
            receiver.try_recv().is_err(),
            "exactly one canonical observation was projected"
        );
    }
}
