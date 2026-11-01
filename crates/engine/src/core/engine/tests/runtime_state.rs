//! Engine state that must stay truthful across a boundary: the live MCP
//! catalog (C02-07/C02-16), a session switch (C02-08/C02-17), a user-input
//! wait (C02-19), an `/edit` whose replacement never starts (C02-02), and a
//! malformed MCP config (C02-18). No fixture reaches a provider or a shell.

use super::*;
use crate::llm_client::mock::MockLlmClient;

fn quiet_engine(config: EngineConfig) -> (Engine, EngineHandle) {
    Engine::new_with_model_client(
        config,
        &Config::default(),
        Arc::new(MockLlmClient::new(Vec::new())),
    )
}

async fn session_snapshot(handle: &EngineHandle) -> SessionSnapshot {
    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .send(Op::GetSessionSnapshot {
            tx: Arc::new(StdMutex::new(Some(tx))),
        })
        .await
        .unwrap();
    tokio::time::timeout(model_turn_event_timeout(), rx)
        .await
        .expect("snapshot response")
        .expect("snapshot")
}

#[test]
fn live_mcp_refresh_drops_tools_the_pool_no_longer_lists() {
    // C02-07: the caller's universe is what the pool lists *now*. After a
    // live 401 that is only the synthetic login tool — never the dead one.
    let mut catalog = vec![api_tool("mcp_alpha_search"), api_tool("exec_shell")];
    let mut active: HashSet<String> = ["mcp_alpha_search", "exec_shell"]
        .into_iter()
        .map(str::to_string)
        .collect();
    let universe: HashSet<String> = ["mcp_alpha_authenticate".to_string()].into();
    let changed = replace_runtime_mcp_tools(
        &mut catalog,
        &mut active,
        &universe,
        vec![api_tool("mcp_alpha_authenticate")],
        AppMode::Agent,
        &HashSet::new(),
        crate::model_profile::ToolSurfaceBudget::Standard,
    );
    let names: Vec<&str> = catalog.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, vec!["exec_shell", "mcp_alpha_authenticate"]);
    assert!(
        !active.contains("mcp_alpha_search"),
        "a dead tool is not callable"
    );
    assert!(changed);
}

#[test]
fn live_mcp_refresh_reports_a_schema_only_edit() {
    // C02-16: the same names with an edited description is a change the
    // prefix check must hear about; an identical refresh is not.
    let refresh = |catalog: &mut Vec<Tool>, active: &mut HashSet<String>, tool: Tool| {
        replace_runtime_mcp_tools(
            catalog,
            active,
            &["mcp_alpha_read".to_string()].into(),
            vec![tool],
            AppMode::Agent,
            &HashSet::new(),
            crate::model_profile::ToolSurfaceBudget::Standard,
        )
    };
    let mut catalog = vec![api_tool("exec_shell")];
    let mut active = HashSet::new();
    refresh(&mut catalog, &mut active, api_tool("mcp_alpha_read"));
    let mut edited = api_tool("mcp_alpha_read");
    edited.description = "Read a resource (now paginated)".to_string();
    assert!(refresh(&mut catalog, &mut active, edited.clone()));
    assert!(!refresh(&mut catalog, &mut active, edited));
}

async fn engine_mid_mcp_boot_with_usage() -> (Engine, EngineHandle, tokio::task::JoinHandle<()>) {
    let workspace = tempdir().unwrap();
    let (mut engine, handle) = quiet_engine(deterministic_engine_config(workspace.path()));
    engine.session.total_usage.input_tokens = 1_000;
    engine.session.total_usage.output_tokens = 200;
    let boot = tokio::spawn(std::future::pending::<()>());
    engine.mcp_boot_task = Some(boot.abort_handle());
    engine.mcp_boot_in_flight = true;
    engine.mcp_boot_generation = Some(7);
    engine.mcp_connection_errors.insert(
        "previous-server".to_string(),
        "previous failure".to_string(),
    );
    // A same-conversation re-sync keeps its usage and its boot pass.
    let same = engine.session.id.clone();
    assert!(engine.install_synced_session_id(same).is_none());
    assert_eq!(engine.session_snapshot().total_tokens, 1_200);
    assert!(engine.mcp_boot_in_flight);
    (engine, handle, boot)
}

#[tokio::test]
async fn a_session_boundary_does_not_inherit_the_previous_usage() {
    // C02-17: session A's tokens are not session B's.
    let (mut engine, _handle, _boot) = engine_mid_mcp_boot_with_usage().await;
    assert!(
        engine
            .install_synced_session_id("next-session".to_string())
            .is_some()
    );
    assert_eq!(engine.session_snapshot().total_tokens, 0);
}

#[tokio::test]
async fn a_session_boundary_abandons_the_previous_mcp_boot() {
    // C02-08: the dropped pool's pass is aborted and its state cleared.
    let (mut engine, _handle, boot) = engine_mid_mcp_boot_with_usage().await;
    assert!(
        engine
            .install_synced_session_id("next-session".to_string())
            .is_some()
    );
    assert!(!engine.mcp_boot_in_flight);
    assert_eq!(engine.mcp_boot_generation, None);
    assert!(engine.mcp_boot_rx.is_none());
    assert!(engine.mcp_connection_errors.is_empty());
    let joined = tokio::time::timeout(Duration::from_secs(5), boot)
        .await
        .expect("the previous boot pass must stop promptly");
    assert!(joined.unwrap_err().is_cancelled(), "the pass was aborted");
}

fn user_input_request() -> crate::tools::user_input::UserInputRequest {
    serde_json::from_value(serde_json::json!({
        "questions":[{"id":"choice","question":"Answer?","allow_free_text":true}]
    }))
    .unwrap()
}

#[tokio::test]
async fn a_session_boundary_discards_and_stops_the_previous_mcp_supervisor() {
    let workspace = tempdir().unwrap();
    let (mut engine, _handle) = quiet_engine(deterministic_engine_config(workspace.path()));
    let (tx, rx) = mpsc::channel(1);
    tx.send(McpSupervisorUpdate {
        died: vec![("previous-server".to_string(), "stale diagnosis".to_string())],
        failed: Vec::new(),
        recovered: Vec::new(),
        parked: Vec::new(),
        refreshed: Vec::new(),
    })
    .await
    .unwrap();
    engine.mcp_supervisor_rx = Some(rx);

    // A reconnect future retains its connection resources across await. The
    // boundary must abort that future, not merely drop a Weak pool owner.
    let held = Arc::new(());
    let weak = Arc::downgrade(&held);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let supervisor = tokio::spawn(async move {
        let _held = held;
        started_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    started_rx.await.unwrap();
    engine.mcp_supervisor_task = Some(supervisor.abort_handle());
    assert!(
        engine
            .install_synced_session_id("next-session".to_string())
            .is_some()
    );
    assert!(
        engine.mcp_supervisor_rx.is_none(),
        "queued diagnoses retired"
    );
    assert!(engine.mcp_supervisor_task.is_none());
    let joined = tokio::time::timeout(Duration::from_secs(1), supervisor)
        .await
        .expect("the previous supervisor must stop promptly");
    assert!(joined.unwrap_err().is_cancelled());
    assert!(weak.upgrade().is_none(), "reconnect resources released");
    assert!(tx.is_closed(), "old diagnoses cannot cross the boundary");

    engine.ensure_mcp_pool().await.unwrap();
    assert!(engine.mcp_supervisor_rx.is_some(), "the next pool is armed");
    assert!(engine.mcp_supervisor_task.is_some());
    assert!(engine.mcp_connection_errors.is_empty());
    engine.drop_mcp_pool();
}

#[tokio::test]
async fn an_undeliverable_user_input_request_fails_fast() {
    // C02-19: the question cannot reach a host (its event channel is closed,
    // while the answer channel stays open), so nobody can ever answer it.
    let workspace = tempdir().unwrap();
    let (mut engine, handle) = quiet_engine(EngineConfig {
        user_input_responder: true,
        ..deterministic_engine_config(workspace.path())
    });
    engine.config.user_input_responder = false;
    assert!(matches!(
        engine
            .await_user_input("unavailable", user_input_request())
            .await,
        Err(ToolError::ExecutionFailed { .. })
    ));
    assert!(handle.rx_event.write().await.try_recv().is_err());
    engine.config.user_input_responder = true;
    engine.host_profile = EngineHostProfile::Child;
    assert!(matches!(
        engine.await_user_input("child", user_input_request()).await,
        Err(ToolError::ExecutionFailed { .. })
    ));
    engine.host_profile = EngineHostProfile::Normal;
    let (tiny, _tiny_rx) = mpsc::channel(1);
    let regular = std::mem::replace(&mut engine.tx_event, tiny);
    assert!(matches!(
        engine.await_user_input("tiny", user_input_request()).await,
        Err(ToolError::ExecutionFailed { .. })
    ));
    engine.tx_event = regular;
    for timed_out in [false, true] {
        let (full, mut full_rx) = mpsc::channel(2);
        full.send(Event::Status {
            message: "first".into(),
        })
        .await
        .unwrap();
        full.send(Event::Status {
            message: "second".into(),
        })
        .await
        .unwrap();
        let regular = std::mem::replace(&mut engine.tx_event, full);
        // The probe below must be able to elapse before the user-input timer.
        // A 2 ms probe against a 10 ms timer left only 8 ms of headroom, which
        // Windows' ~15.6 ms timer granularity exceeds, so the timer won and the
        // "still waiting" assertion inverted. Keep a gap no scheduler can eat.
        engine.config.user_input_timeout = timed_out.then_some(Duration::from_millis(200));
        let withdrawal = CancellationToken::new();
        let invocation_clock = std::sync::Mutex::new(crate::tools::codemode::PauseClock::new());
        {
            let wait = engine.await_user_input_withdrawal(
                "undisplayed",
                user_input_request(),
                withdrawal.clone(),
                Some(&invocation_clock),
            );
            tokio::pin!(wait);
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut wait)
                    .await
                    .is_err()
            );
            assert!(
                invocation_clock
                    .lock()
                    .unwrap()
                    .remaining(Duration::from_millis(1))
                    .is_none()
            );
            if timed_out {
                assert!(matches!(wait.await, Err(ToolError::Timeout { .. })));
            } else {
                withdrawal.cancel();
                assert!(matches!(wait.await, Err(ToolError::Cancelled { .. })));
            }
        }
        assert!(matches!(full_rx.try_recv(), Ok(Event::Status { .. })));
        assert!(matches!(full_rx.try_recv(), Ok(Event::Status { .. })));
        assert!(full_rx.try_recv().is_err());
        engine.tx_event = regular;
    }
    engine.config.user_input_timeout = None;
    {
        let wait = engine.await_user_input("disconnected", user_input_request());
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(Duration::from_millis(2), &mut wait)
                .await
                .is_err()
        );
        assert!(
            matches!(handle.rx_event.write().await.recv().await,Some(Event::UserInputRequired {id,..}) if id == "disconnected")
        );
        handle.rx_event.write().await.close();
        assert!(matches!(wait.await, Err(ToolError::ExecutionFailed { .. })));
    }
    assert!(
        matches!(handle.rx_event.write().await.recv().await,Some(Event::UserInputClosed {id,outcome:crate::core::events::UserInputClosedOutcome::Unavailable}) if id == "disconnected")
    );
    handle.rx_event.write().await.close();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        engine.await_user_input("ask-undeliverable", user_input_request()),
    )
    .await
    .expect("an undeliverable question must not wait for an answer");
    assert!(
        matches!(result, Err(ToolError::ExecutionFailed { .. })),
        "{result:?}"
    );
}

#[tokio::test]
async fn an_open_user_input_question_is_not_charged_to_the_turn() {
    // C02-19: a delivered question the person leaves open is human time, not
    // the agent's; the per-turn wall clock does not advance across it.
    let workspace = tempdir().unwrap();
    let wait = Duration::from_millis(400);
    let (mut engine, _handle) = quiet_engine(EngineConfig {
        user_input_responder: true,
        user_input_timeout: Some(wait),
        ..deterministic_engine_config(workspace.path())
    });
    let before = engine.turn_wall_clock.spent();
    let result = engine
        .await_user_input("ask-unanswered", user_input_request())
        .await;
    assert!(
        matches!(result, Err(ToolError::Timeout { .. })),
        "{result:?}"
    );
    let charged = engine.turn_wall_clock.spent().saturating_sub(before);
    assert!(
        charged < wait / 2,
        "the human wait was charged to the turn budget: {charged:?}"
    );
    for withdraw in [false, true] {
        let (mut engine, handle) = quiet_engine(EngineConfig {
            user_input_responder: true,
            ..deterministic_engine_config(workspace.path())
        });
        let withdrawal = CancellationToken::new();
        let invocation_clock = std::sync::Mutex::new(crate::tools::codemode::PauseClock::new());
        let (accepted, rejected) = tokio::sync::oneshot::channel();
        {
            let wait = engine.await_user_input_withdrawal(
                "withdrawn",
                user_input_request(),
                withdrawal.clone(),
                Some(&invocation_clock),
            );
            tokio::pin!(wait);
            assert!(
                tokio::time::timeout(Duration::from_millis(5), &mut wait)
                    .await
                    .is_err()
            );
            assert!(
                invocation_clock
                    .lock()
                    .unwrap()
                    .remaining(Duration::from_millis(1))
                    .is_some()
            );
            handle
                .tx_user_input
                .send(UserInputDecision::Submitted {
                    id: "withdrawn".into(),
                    response: UserInputResponse {
                        answers: Vec::new(),
                    },
                    accepted,
                })
                .await
                .unwrap();
            if withdraw {
                withdrawal.cancel();
                assert!(matches!(wait.await, Err(ToolError::Cancelled { .. })));
            }
        }
        assert!(!rejected.await.unwrap());
        assert!(
            matches!(handle.rx_event.write().await.recv().await, Some(Event::UserInputRequired {id,..}) if id == "withdrawn")
        );
        assert!(
            matches!(handle.rx_event.write().await.recv().await, Some(Event::UserInputClosed {id,outcome:crate::core::events::UserInputClosedOutcome::Cancelled}) if id == "withdrawn")
        );
        let resumed = engine.turn_wall_clock.spent();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            invocation_clock
                .lock()
                .unwrap()
                .remaining(Duration::from_millis(1))
                .is_none()
        );
        assert!(
            engine.turn_wall_clock.spent().saturating_sub(resumed) >= Duration::from_millis(15)
        );
    }
}

#[tokio::test]
async fn user_input_acknowledges_only_live_answer_or_cancellation() {
    for timeout in [None, Some(Duration::ZERO)] {
        for cancel in [false, true] {
            for typed in [false, true] {
                let workspace = tempdir().unwrap();
                let (mut engine, handle) = quiet_engine(EngineConfig {
                    user_input_responder: true,
                    user_input_timeout: timeout,
                    ..deterministic_engine_config(workspace.path())
                });
                let mut request = user_input_request();
                request.require_typed_answers = typed;
                let waiter =
                    tokio::spawn(
                        async move { engine.await_user_input("live-question", request).await },
                    );
                assert!(matches!(handle.rx_event.write().await.recv().await,
                    Some(Event::UserInputRequired {id, ..}) if id == "live-question"));
                if cancel {
                    handle
                        .cancel_user_input("live-question")
                        .await
                        .expect("live cancel accepted");
                    let error = waiter.await.unwrap().unwrap_err();
                    if typed {
                        assert_eq!(
                            error.metadata(),
                            Some(&serde_json::json!({"user_question_error": "ASK_CANCELLED"}))
                        );
                    } else {
                        assert!(matches!(&error, ToolError::Cancelled { .. }));
                        assert!(error.metadata().is_none());
                    }
                } else {
                    if typed {
                        assert!(
                            handle
                                .submit_user_input(
                                    "live-question",
                                    UserInputResponse {
                                        answers: Vec::new()
                                    }
                                )
                                .await
                                .is_err()
                        );
                    }
                    let response = if typed {
                        serde_json::from_value(serde_json::json!({"answers":[{"id":"choice","kind":"custom","label":"","value":"answer"}]})).unwrap()
                    } else {
                        UserInputResponse {
                            answers: Vec::new(),
                        }
                    };
                    handle
                        .submit_user_input("live-question", response)
                        .await
                        .expect("live answer accepted");
                    assert!(waiter.await.unwrap().is_ok());
                }
                loop {
                    if let Some(Event::UserInputClosed { id, outcome }) = tokio::time::timeout(
                        Duration::from_secs(1),
                        handle.rx_event.write().await.recv(),
                    )
                    .await
                    .expect("close receipt must arrive")
                    {
                        assert_eq!(id, "live-question");
                        assert_eq!(
                            outcome,
                            if cancel {
                                crate::core::events::UserInputClosedOutcome::Cancelled
                            } else {
                                crate::core::events::UserInputClosedOutcome::Answered
                            }
                        );
                        break;
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn user_input_expired_deadline_or_cancellation_rejects_ready_reply() {
    for canceled in [false, true] {
        let workspace = tempdir().unwrap();
        let (mut engine, handle) = quiet_engine(EngineConfig {
            user_input_responder: true,
            user_input_timeout: (!canceled).then_some(Duration::from_millis(200)),
            ..deterministic_engine_config(workspace.path())
        });
        let cancel = engine.cancel_token.clone();
        let wait = engine.await_user_input("expired-question", user_input_request());
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(Duration::from_millis(5), &mut wait)
                .await
                .is_err()
        );
        if canceled {
            cancel.cancel();
        } else {
            // Stop polling the wait until its actual absolute deadline has
            // elapsed, then make both the mailbox and terminal bound ready.
            tokio::time::sleep(Duration::from_millis(230)).await;
        }
        let submission = handle.submit_user_input(
            "expired-question",
            UserInputResponse {
                answers: Vec::new(),
            },
        );
        tokio::pin!(submission);
        assert!(
            tokio::time::timeout(Duration::from_millis(5), &mut submission)
                .await
                .is_err()
        );
        let outcome = wait.await;
        if canceled {
            assert!(matches!(outcome, Err(ToolError::Cancelled { .. })));
        } else {
            assert!(matches!(outcome, Err(ToolError::Timeout { .. })));
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(1), submission)
                .await
                .expect("wait exit must reject the queued verdict")
                .is_err()
        );
    }
}

#[tokio::test]
async fn user_input_mismatched_and_abandoned_replies_do_not_end_live_wait() {
    let workspace = tempdir().unwrap();
    let (mut engine, handle) = quiet_engine(EngineConfig {
        user_input_responder: true,
        ..deterministic_engine_config(workspace.path())
    });
    let waiter = tokio::spawn(async move {
        engine
            .await_user_input("current-question", user_input_request())
            .await
    });
    assert!(matches!(
        handle.rx_event.write().await.recv().await,
        Some(Event::UserInputRequired { .. })
    ));
    assert!(
        handle
            .submit_user_input(
                "old-question",
                UserInputResponse {
                    answers: Vec::new()
                }
            )
            .await
            .is_err()
    );
    assert!(handle.cancel_user_input("old-question").await.is_err());
    for cancel in [false, true] {
        let (accepted, receiver) = tokio::sync::oneshot::channel();
        drop(receiver);
        let decision = if cancel {
            UserInputDecision::Cancelled {
                id: "current-question".into(),
                accepted,
            }
        } else {
            UserInputDecision::Submitted {
                id: "current-question".into(),
                response: UserInputResponse {
                    answers: Vec::new(),
                },
                accepted,
            }
        };
        handle.tx_user_input.send(decision).await.unwrap();
    }
    handle
        .submit_user_input(
            "current-question",
            UserInputResponse {
                answers: vec![crate::tools::user_input::UserInputAnswer {
                    kind: None,
                    id: "choice".into(),
                    label: "Live".into(),
                    value: "accepted-live-reply".into(),
                }],
            },
        )
        .await
        .expect("current response accepted after rejected decisions");
    let response = waiter.await.unwrap().expect("live waiter completed");
    assert_eq!(response.answers[0].value, "accepted-live-reply");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn edit_last_turn_restores_the_exchange_when_the_replacement_never_starts() {
    // C02-02: `/edit` cuts the last exchange, then dispatches the new text.
    // A dispatch that never starts must hand the cut exchange back.
    let _lock = lock_test_env();
    let tmp = tempdir().unwrap();
    let api_config = Config::default().with_legacy_root(
        Some("test-key".to_string()),
        Some("http://127.0.0.1:9".to_string()),
    );
    let (mut engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            workspace: tmp.path().to_path_buf(),
            model: "deepseek-v4-pro".to_string(),
            snapshots_enabled: false,
            subagents_enabled: false,
            ..Default::default()
        },
        &api_config,
        Arc::new(MockLlmClient::new(Vec::new())),
    );
    // No model client: the replacement send returns NotStarted.
    engine.model_client = None;
    let run = tokio::spawn(engine.run());
    let text = |role: Role, text: &str| Message {
        role,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    };
    handle
        .send(Op::SyncSession {
            session_id: Some("edit-restore".to_string()),
            messages: vec![
                text(Role::User, "original prompt"),
                text(Role::Assistant, "original answer"),
            ],
            system_prompt: None,
            system_prompt_override: false,
            model: "deepseek-v4-pro".to_string(),
            workspace: tmp.path().to_path_buf(),
            mode: AppMode::Agent,
        })
        .await
        .unwrap();
    let before = session_snapshot(&handle).await;
    assert_eq!(before.messages.len(), 2);
    handle
        .send(Op::EditLastTurn {
            new_message: "edited prompt".to_string(),
            submission_id: None,
        })
        .await
        .unwrap();
    let after = session_snapshot(&handle).await;
    assert_eq!(
        after.messages, before.messages,
        "a replacement that never started must not cost the original exchange"
    );
    handle.send(Op::Shutdown).await.unwrap();
    run.await.unwrap();
}

#[tokio::test]
async fn malformed_mcp_config_is_reported_instead_of_silently_empty() {
    // C02-18: an unreadable config still yields an empty, reloadable pool,
    // but the person is told the configured servers are gone.
    let workspace = tempdir().unwrap();
    let config_path = workspace.path().join("mcp.json");
    fs::write(&config_path, "{ this is not json").unwrap();
    let (mut engine, handle) = quiet_engine(deterministic_engine_config(workspace.path()));
    engine.session.mcp_config_path = config_path;
    engine
        .ensure_mcp_pool()
        .await
        .expect("a reloadable empty pool still exists");
    let mut rx = handle.rx_event.write().await;
    let reported = std::iter::from_fn(|| rx.try_recv().ok()).any(|event| {
        matches!(
            event,
            Event::Status { ref message } if message.contains("MCP config could not be loaded")
        )
    });
    assert!(reported, "a malformed MCP config must not fail silently");
}
