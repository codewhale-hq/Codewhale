/// A repeated provider ID is a different host execution each time. The FIFO
/// child waiter proves the stale answer was consumed while the second Ask was
/// pending, without relying on a sleep or a second execution racing an assert.
#[cfg(unix)]
#[test]
fn repeated_provider_id_ask_rejects_stale_answer_and_keeps_local_artifact_origin() {
    use crate::approval_log::{ApprovalOutcome, ApprovalReceipt};
    use crate::llm_client::mock::{MockLlmClient, canned};
    use crate::tools::subagent::ChildApprovalOutcome;

    with_artifact_home(|home| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let command = "printf 'identity:%10000s:done\\n' x";
                let arguments = json!({"command": command}).to_string();
                let mock = Arc::new(MockLlmClient::new(vec![
                    canned::tool_call_turn("reused-ask", "bash", &arguments),
                    canned::tool_call_turn("reused-ask", "bash", &arguments),
                    canned::simple_text_turn("done"),
                ]));
                let mut config = Config::default();
                let mut op = external_user_message_op(
                    "Run the same output fixture twice.",
                    AppMode::Agent,
                    &config,
                );
                let Op::SendMessage(turn) = &mut op else {
                    unreachable!("fixture creates a model turn");
                };
                // Declare both limits on the actual route. A 64K window alone
                // still reserves the model's 64K output cap, leaving 1K input.
                config.custom_models = Some(vec![
                    serde_json::from_value(json!({
                        "provider": turn.route.identity.key,
                        "base_url": turn.route.candidate.endpoint().base_url,
                        "id": turn.route.model,
                        "tool_call": true,
                        "limit": {"context": 64_000, "output": 4_096},
                    }))
                    .expect("fixture model limits"),
                ]);
                turn.route = resolved_route_for_test(&config, &turn.route.model);
                let limits = turn.route.candidate.limits();
                let provider = turn.route.identity.provider;
                assert_eq!(limits.context_tokens, Some(64_000));
                assert_eq!(limits.output_tokens, Some(4_096));
                assert!(
                    context_input_budget_for_route(provider, &turn.route.model, Some(limits), 0)
                        .unwrap()
                        >= 58_880,
                    "the resolved input budget must fit the real instructions and two calls"
                );
                assert!(
                    crate::route_budget::route_inline_char_budget_for_route(
                        provider,
                        &turn.route.model,
                        Some(limits),
                    ) < 10_000,
                    "the unchanged output fixture must still require an artifact"
                );
                let mut engine_config = deterministic_engine_config(home);
                engine_config.exec_policy_engine = ask_rule_engine(command);
                let (engine, handle) =
                    Engine::new_with_model_client(engine_config, &config, mock.clone());
                let shell_manager = engine.shell_manager.clone();
                let child_manager = engine.subagent_manager.clone();
                let session_id = engine.session.id.clone();
                let receipt_store = engine.approval_receipt_store.clone().unwrap();
                let task = tokio::spawn(engine.run());
                handle.send(op).await.unwrap();

                let mut starts = Vec::new();
                let mut approvals = Vec::new();
                let mut completions = Vec::new();
                let mut artifact_paths = Vec::new();
                let mut rx = handle.rx_event.write().await;
                loop {
                    match tokio::time::timeout(model_turn_event_timeout(), rx.recv())
                        .await
                        .expect("the fixture turn must settle")
                        .expect("engine event stream")
                    {
                        Event::ToolCallStarted { id, model_call, .. } => {
                            assert_eq!(model_call.unwrap().provider_id, "reused-ask");
                            assert!(uuid::Uuid::parse_str(&id).is_ok());
                            assert!(!starts.contains(&id));
                            starts.push(id);
                        }
                        Event::ApprovalRequired { id, .. } => {
                            assert_eq!(starts.last(), Some(&id));
                            if let Some(first) = approvals.first() {
                                assert_ne!(first, &id);
                                assert_eq!(completions, approvals);
                                let (barrier_id, barrier) = child_manager
                                    .write()
                                    .await
                                    .register_child_approval(
                                        "agent_barrier",
                                        &format!("agent:agent_barrier:approval:{}", uuid::Uuid::new_v4()),
                                        "bash",
                                        "approval-channel barrier",
                                    )
                                    .unwrap();
                                handle.approve_tool_call(first).await.unwrap();
                                handle.approve_tool_call(&barrier_id).await.unwrap();
                                assert_eq!(
                                    tokio::time::timeout(model_turn_event_timeout(), barrier)
                                        .await
                                        .expect("queued decisions are consumed")
                                        .unwrap(),
                                    ChildApprovalOutcome::Approved { by: crate::approval_log::ApprovalDecider::User }
                                );
                                let receipts = receipt_store.load(&session_id).unwrap();
                                assert_eq!(receipts.len(), 3, "stale answer cannot decide the second Ask");
                                assert!(matches!(receipts.last(), Some(ApprovalReceipt::Asked { tool_call_id, .. }) if tool_call_id == &id));
                                let jobs = shell_manager.lock().unwrap().list_jobs_for_session(&session_id);
                                assert_eq!(jobs.len(), 1, "the second command is still unexecuted");
                                assert_eq!(jobs[0].origin_tool_call_id.as_ref(), Some(first));
                            }
                            handle.approve_tool_call(&id).await.unwrap();
                            approvals.push(id);
                        }
                        Event::ToolCallComplete { id, model_call, result, .. } => {
                            assert_eq!(model_call.unwrap().provider_id, "reused-ask");
                            assert_eq!(approvals.last(), Some(&id));
                            let output = result.expect("approved shell fixture succeeds");
                            assert!(output.success, "{output:?}");
                            let metadata = output.metadata.as_ref().expect("over-budget output is archived");
                            assert_eq!(metadata["artifact_id"], codewhale_runtime::artifacts::artifact_id_for_tool_call(&id));
                            let path = metadata["artifact_path"].as_str().unwrap().to_string();
                            assert_eq!(fs::read_to_string(&path).unwrap(), output.content);
                            assert!(!artifact_paths.contains(&path));
                            artifact_paths.push(path);
                            completions.push(id);
                        }
                        Event::TurnComplete { status, error, .. } => {
                            assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                            break;
                        }
                        _ => {}
                    }
                }
                drop(rx);
                assert_eq!(starts.len(), 2);
                assert_eq!(starts, approvals);
                assert_eq!(starts, completions);
                let receipts = receipt_store.load(&session_id).unwrap();
                assert_eq!(receipts.len(), 4);
                for (pair, id) in receipts.as_chunks::<2>().0.iter().zip(&starts) {
                    assert!(matches!(&pair[0], ApprovalReceipt::Asked { approval_id, tool_call_id, .. } if approval_id == id && tool_call_id == id));
                    assert!(matches!(&pair[1], ApprovalReceipt::Decided { approval_id, tool_call_id, outcome: ApprovalOutcome::ApprovedOnce, .. } if approval_id == id && tool_call_id == id));
                }
                let origins = shell_manager
                    .lock()
                    .unwrap()
                    .list_jobs_for_session(&session_id)
                    .into_iter()
                    .map(|job| job.origin_tool_call_id.unwrap())
                    .collect::<HashSet<_>>();
                assert_eq!(origins, starts.iter().cloned().collect());
                let requests = mock.captured_requests();
                assert_eq!(requests.len(), 3);
                for id in &starts {
                    let pairs = requests[2].messages.iter().flat_map(|message| &message.content)
                        .filter(|block| block.tool_call_key() == Some(codewhale_models::ToolCallKey::Execution(id)))
                        .collect::<Vec<_>>();
                    assert_eq!(pairs.len(), 2);
                    assert!(matches!(pairs[0], ContentBlock::ToolUse { id, .. } if id == "reused-ask"));
                    assert!(matches!(pairs[1], ContentBlock::ToolResult { tool_use_id, content, .. } if tool_use_id == "reused-ask" && content.contains(&codewhale_runtime::artifacts::artifact_id_for_tool_call(id))));
                }
                handle.send(Op::Shutdown).await.unwrap();
                task.await.unwrap();
            });
    });
}

/// #4415 AC(b): a 4-call parallel batch proposed with 2 calls remaining is
/// truncated to the first 2 calls in proposal order; the excess 2 are
/// rejected with the same typed reason, and the batch is counted in full.
#[tokio::test]
async fn tool_call_budget_truncates_an_over_budget_parallel_batch() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    let mut calls = Vec::new();
    for index in 1..=4 {
        let name = format!("fixture-{index}.txt");
        fs::write(workspace.path().join(&name), format!("fixture-{index}\n"))
            .expect("write fixture");
        calls.push((
            format!("call-{index}"),
            "read_file".to_string(),
            format!(r#"{{"path":"{name}"}}"#),
        ));
    }
    let call_refs = calls
        .iter()
        .map(|(id, name, args)| (id.as_str(), name.as_str(), args.as_str()))
        .collect::<Vec<_>>();
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        tool_batch_turn(&call_refs),
        canned::simple_text_turn("done"),
    ]));

    let (status, error, completions) =
        run_budgeted_read_turn(workspace.path(), Some(2), mock.clone()).await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    assert_eq!(mock.call_count(), 2, "batch turn then the final text turn");
    assert_eq!(completions.len(), 4);

    let mut admitted = 0;
    for (id, result) in &completions {
        match id.as_str() {
            "call-1" | "call-2" => {
                admitted += 1;
                assert!(result.is_ok(), "{id} must execute: {result:?}");
            }
            "call-3" | "call-4" => {
                let rejection = result.as_ref().expect_err("excess calls are rejected");
                let reason = rejection.to_string();
                assert!(reason.contains("budget of 2"), "{reason}");
                assert!(reason.contains("remaining=0"), "{reason}");
            }
            other => panic!("unexpected call id {other}"),
        }
    }
    assert_eq!(admitted, 2, "exactly the remaining 2 calls are admitted");
}

/// #4415: the budget is per-turn, not per-batch — a counter that survives
/// every model step of the turn. A full 8-call first batch leaves the next
/// step's single call with `remaining=0`.
#[tokio::test]
async fn tool_call_budget_persists_across_model_steps_within_a_turn() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    let mut first_batch = Vec::new();
    for index in 1..=8 {
        let name = format!("fixture-{index}.txt");
        fs::write(workspace.path().join(&name), format!("fixture-{index}\n"))
            .expect("write fixture");
        first_batch.push((
            format!("call-{index}"),
            "read_file".to_string(),
            format!(r#"{{"path":"{name}"}}"#),
        ));
    }
    let first_refs = first_batch
        .iter()
        .map(|(id, name, args)| (id.as_str(), name.as_str(), args.as_str()))
        .collect::<Vec<_>>();
    fs::write(workspace.path().join("fixture-9.txt"), "fixture-9\n").expect("write fixture");
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        tool_batch_turn(&first_refs),
        canned::tool_call_turn("call-9", "read_file", r#"{"path":"fixture-9.txt"}"#),
        canned::simple_text_turn("done"),
    ]));

    let (status, error, completions) =
        run_budgeted_read_turn(workspace.path(), Some(8), mock.clone()).await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    assert_eq!(
        mock.call_count(),
        3,
        "two tool steps then the final text turn"
    );
    assert_eq!(completions.len(), 9);

    let (id, ninth) = completions
        .iter()
        .find(|(id, _)| id == "call-9")
        .expect("the ninth call still reports a completion");
    assert_eq!(id, "call-9");
    let reason = ninth
        .as_ref()
        .expect_err("ninth call exceeds the turn budget");
    let reason = reason.to_string();
    assert!(reason.contains("remaining=0"), "{reason}");
    assert!(
        completions
            .iter()
            .filter(|(id, result)| id != "call-9" && result.is_ok())
            .count()
            == 8,
        "the first batch of 8 all executed: {completions:?}"
    );
}

/// #5986: a provider that cuts the stream at its output limit omits the
/// closing `ContentBlockStop` for the tool block in flight. The mid-stream
/// mirror had already assigned the truncated buffer's best-effort parse
/// (the repair ladder appends the missing `}`), and dispatch reads
/// `tool.input` directly — so the cut call used to execute with a partial
/// argument. The post-stream finalization pass must send it down the same
/// malformed-arguments gate a normal block stop applies.
#[tokio::test]
async fn truncated_tool_call_without_block_stop_never_dispatches() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    fs::write(workspace.path().join("fixture.txt"), "fixture\n").expect("write fixture");
    // Cut mid-argument, right after a complete string value: stage 4 of the
    // repair ladder appends one `}` and the text parses — synthesized.
    let cut_turn = vec![
        canned::message_start("mock_msg_cut"),
        canned::tool_use_block_start(0, "call-cut", "read_file"),
        canned::tool_input_delta(0, r#"{"path": "fixture.txt""#),
        // Deliberately no block_stop(0): the output limit ended the turn.
        canned::message_delta("max_tokens", None),
        canned::message_stop(),
    ];
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        cut_turn,
        canned::simple_text_turn("done"),
    ]));

    let (status, error, completions) =
        run_budgeted_read_turn(workspace.path(), None, mock.clone()).await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");

    let (_, result) = completions
        .iter()
        .find(|(id, _)| id == "call-cut")
        .expect("the cut tool call still reports a completion");
    let reason = result
        .as_ref()
        .expect_err("a truncated tool call must never execute")
        .to_string();
    assert!(
        reason.contains("malformed tool arguments"),
        "expected the malformed-arguments gate, got: {reason}"
    );
}

/// The control for the cut-stream pass: when the omitted block stop is the
/// only irregularity and the buffered arguments were structurally complete,
/// the tool still dispatches. Otherwise every provider that skips closing
/// events would lose all of its tool calls.
#[tokio::test]
async fn complete_tool_call_without_block_stop_still_dispatches() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    fs::write(workspace.path().join("fixture.txt"), "fixture\n").expect("write fixture");
    let no_stop_turn = vec![
        canned::message_start("mock_msg_nostop"),
        canned::tool_use_block_start(0, "call-complete", "read_file"),
        canned::tool_input_delta(0, r#"{"path": "fixture.txt"}"#),
        canned::message_delta("tool_use", None),
        canned::message_stop(),
    ];
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        no_stop_turn,
        canned::simple_text_turn("done"),
    ]));

    let (status, error, completions) =
        run_budgeted_read_turn(workspace.path(), None, mock.clone()).await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");

    let (_, result) = completions
        .iter()
        .find(|(id, _)| id == "call-complete")
        .expect("the tool call reports a completion");
    let output = result
        .as_ref()
        .expect("structurally complete arguments still dispatch")
        .content
        .clone();
    assert!(output.contains("fixture"), "{output}");
}

/// #4415 AC(c): a write-first named-file task carries a scoped-write
/// authority envelope naming its exact files. The existing allowed-paths
/// machinery (`ToolAuthorityEnvelope`, enforced at the registry boundary)
/// permits mutating a named file and denies mutating anything outside it
/// with a typed permission error, and the denied write never executes.
///
/// Seam: the envelope is a MUTATION boundary only — `read_file` outside the
/// named files is NOT denied by policy today (read-only tools pass the
/// envelope by design). Denying out-of-scope reads for write-first tasks is
/// a #4415 follow-up; this test pins the current contract so the seam is
/// explicit rather than assumed.
#[tokio::test]
async fn named_file_write_scope_denies_mutation_outside_the_named_files() {
    let workspace = tempdir().expect("tempdir");
    fs::create_dir_all(workspace.path().join("src")).expect("src dir");
    fs::create_dir_all(workspace.path().join("docs")).expect("docs dir");
    fs::write(workspace.path().join("docs/other.md"), "outside\n").expect("write fixture");
    let envelope = crate::tools::spec::ToolAuthorityEnvelope {
        schema_version: 1,
        owner: "test-worker".to_string(),
        authority: crate::tools::spec::ToolMutationAuthority::ScopedWrite,
        network_access: None,
        shell: crate::tools::spec::ToolShellAuthority::None,
        verification: crate::tools::spec::ToolVerificationAuthority::None,
        writable_roots: Vec::new(),
        writable_files: vec!["src/named.rs".to_string()],
        coordination_contracts: Vec::new(),
    };
    let context = crate::tools::ToolContext::new(workspace.path().to_path_buf())
        .with_tool_authority(envelope)
        .expect("valid envelope");
    let mut registry = crate::tools::ToolRegistry::new(context);
    registry.register(std::sync::Arc::new(crate::tools::file::ReadFileTool));
    registry.register(std::sync::Arc::new(crate::tools::file::WriteFileTool));

    // The named file is writable under the envelope.
    let named = registry
        .execute_full(
            "write_file",
            json!({"path": "src/named.rs", "content": "fn named() {}\n"}),
        )
        .await
        .expect("mutation of the named file is permitted");
    assert!(named.success, "{named:?}");
    assert!(workspace.path().join("src/named.rs").exists());

    // A mutation outside the named files is denied by policy and never runs.
    let denied = registry
        .execute_full(
            "write_file",
            json!({"path": "docs/other.md", "content": "rewritten\n"}),
        )
        .await
        .expect_err("mutation outside the named files is denied");
    assert!(
        denied.to_string().contains("authority envelope"),
        "{denied}"
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join("docs/other.md")).expect("read back"),
        "outside\n",
        "the denied write must not have executed"
    );

    // Pin the read-side seam: a read outside the named files is allowed
    // through the mutation-scoped envelope today.
    let read = registry
        .execute_full("read_file", json!({"path": "docs/other.md"}))
        .await
        .expect("reads are not path-scoped by the mutation envelope today");
    assert!(read.content.contains("outside"), "{read:?}");
}

#[test]
fn empty_allowed_tools_surface_is_empty_and_sends_no_tools_field() {
    let surface = policy_for_catalog(vec![catalog_tool("read_file")], Some(Vec::new()), None);

    assert!(surface.catalog.is_empty());
    assert!(surface.active_names.is_empty());
    assert!(surface.active.is_none());
    assert!(!surface.allows_tool("read_file"));
}

/// The turn-start capture carries mode/workspace/working-set state only. Work
/// used to be rendered here; it moved to the fork seam (#3983) because this
/// block is captured before the turn's first tool call. Fork-seam Work parity is
/// covered by `fork_state_block_reuses_the_canonical_work_body`.
#[test]
fn structured_state_block_carries_stable_state_without_work() {
    let state = StructuredState {
        mode_label: "Agent".to_string(),
        workspace: PathBuf::from("/workspace/codewhale"),
        cwd: Some(PathBuf::from("/workspace/codewhale")),
        working_set_summary: None,
        subagent_snapshots: Vec::new(),
    };

    let block = state.to_system_block().expect("fork state block");

    assert!(block.contains("- Mode: `Agent`"));
    assert!(!block.contains(crate::todo_snapshot::FORK_TODO_SECTION_HEADING));
    assert!(!block.contains("To-do ("));
    assert!(!block.contains("Strategy"));
}

#[test]
fn env_only_auth_error_gets_recovery_hint() {
    let _guard = lock_test_env();
    let _env = ScopedDeepSeekApiKey::set("stale-env-key");
    let (engine, _handle) = Engine::new(EngineConfig::default(), &Config::default());

    let message =
        engine.decorate_auth_error_message("Authentication failed: invalid API key".to_string());

    assert!(message.contains("DEEPSEEK_API_KEY"));
    assert!(message.contains("no saved config key is present"));
    assert!(message.contains("codewhale auth status"));
    assert!(message.contains("codewhale auth set --provider deepseek"));
}

#[test]
fn config_auth_error_does_not_blame_env() {
    let _guard = lock_test_env();
    let _env = ScopedDeepSeekApiKey::set("stale-env-key");
    let cfg = Config::default().with_legacy_root(Some("fresh-config-key".to_string()), None);
    let (engine, _handle) = Engine::new(EngineConfig::default(), &cfg);

    let message =
        engine.decorate_auth_error_message("Authentication failed: invalid API key".to_string());

    assert_eq!(message, "Authentication failed: invalid API key");
}

#[test]
fn plugin_tools_dir_honors_missing_custom_directory_without_fallback() {
    let missing = PathBuf::from("definitely-missing-codewhale-plugin-dir");
    let tools_config = codewhale_runtime::config::ToolsConfig {
        plugin_dir: Some(missing.to_string_lossy().to_string()),
        ..Default::default()
    };

    assert_eq!(plugin_tools_dir(Some(&tools_config)), missing);
}

#[test]
fn configure_plugin_tools_applies_overrides_after_discovered_plugins() {
    let tmp = tempdir().expect("tempdir");
    let plugin_dir = tmp.path().join("tools");
    fs::create_dir(&plugin_dir).expect("plugin dir");
    fs::write(
        plugin_dir.join("same-name.sh"),
        "# name: same_tool\n# description: discovered plugin\n",
    )
    .expect("plugin script");

    let mut overrides = HashMap::new();
    overrides.insert(
        "same_tool".to_string(),
        codewhale_runtime::config::ToolOverride::Command {
            command: "configured-command".to_string(),
            args: None,
        },
    );
    // Everything registered before configuration is built in: D4 refuses a
    // replacement for it.
    overrides.insert(
        "File".to_string(),
        codewhale_runtime::config::ToolOverride::Command {
            command: "configured-file".to_string(),
            args: None,
        },
    );
    let tools_config = codewhale_runtime::config::ToolsConfig {
        plugin_dir: Some(plugin_dir.to_string_lossy().to_string()),
        overrides: Some(overrides),
        ..Default::default()
    };

    let ctx = crate::tools::ToolContext::new(tmp.path().to_path_buf());
    let mut registry = crate::tools::ToolRegistryBuilder::new()
        .with_file_tools()
        .build(ctx);
    let file = registry.get("File").expect("built-in File");

    let (plugin_names, refused) = configure_plugin_tools(&mut registry, Some(&tools_config));

    let tool = registry.get("same_tool").expect("same_tool registered");
    assert!(tool.description().contains("configured-command"));
    assert!(plugin_names.contains("same_tool"));
    assert!(Arc::ptr_eq(
        &registry.get("File").expect("File kept"),
        &file
    ));
    assert!(!plugin_names.contains("File"));
    // The refusal reaches the engine so it can name it to the user.
    assert_eq!(refused, vec!["File".to_string()]);
    assert!(
        crate::tools::registry::override_refusal_notice("File").contains("[tools.overrides.File]")
    );
}

fn make_plan(
    read_only: bool,
    supports_parallel: bool,
    approval_required: bool,
    interactive: bool,
) -> ToolExecutionPlan {
    make_plan_at(
        0,
        read_only,
        supports_parallel,
        approval_required,
        interactive,
    )
}

fn make_plan_at(
    index: usize,
    read_only: bool,
    supports_parallel: bool,
    approval_required: bool,
    interactive: bool,
) -> ToolExecutionPlan {
    ToolExecutionPlan {
        model_call: None,
        index,
        id: format!("tool-{index}"),
        name: "grep_files".to_string(),
        input: json!({"pattern": "test"}),
        caller: None,
        interactive,
        approval_required,
        approval_description: "desc".to_string(),
        approval_force_prompt: false,
        supports_parallel,
        read_only,
        detached_start: false,
        resources: vec![ResourceClaim::ReadPath(PathBuf::from(format!(
            "src-{index}.rs"
        )))],
        blocked_error: None,
        guard_result: None,
    }
}

fn parallel_batch_indices(batch: &ToolExecutionBatch) -> Vec<usize> {
    match batch {
        ToolExecutionBatch::Parallel(plans) => plans.iter().map(|plan| plan.index).collect(),
        ToolExecutionBatch::Serial(_) => panic!("expected parallel batch"),
    }
}

fn ask_rule_engine(command: &str) -> codewhale_execpolicy::ExecPolicyEngine {
    codewhale_execpolicy::ExecPolicyEngine::with_rulesets(vec![
        codewhale_execpolicy::Ruleset::user(vec![], vec![])
            .with_ask_rules(vec![codewhale_execpolicy::ToolAskRule::exec_shell(command)]),
    ])
}

fn file_ask_rule_engine(tool: &str, path: &str) -> codewhale_execpolicy::ExecPolicyEngine {
    codewhale_execpolicy::ExecPolicyEngine::with_rulesets(vec![
        codewhale_execpolicy::Ruleset::user(vec![], vec![]).with_ask_rules(vec![
            codewhale_execpolicy::ToolAskRule::file_path(tool, path),
        ]),
    ])
}

fn model_turn_event_timeout() -> Duration {
    if cfg!(windows) {
        // The Windows CI runner executes the full TUI test binary with thousands of
        // tests competing for CPU. Keep this high enough that an approval-gated
        // model turn is not mistaken for a lifecycle failure under runner load.
        Duration::from_secs(60)
    } else {
        Duration::from_secs(10)
    }
}

fn resolved_route_for_test(
    config: &Config,
    model: &str,
) -> Box<crate::route_runtime::ResolvedRuntimeRoute> {
    Box::new(
        resolve_runtime_route(
            config,
            config.active_provider_identity().unwrap().provider,
            Some(model),
        )
        .expect("resolve test route"),
    )
}

fn active_goal_message_op(
    config: &Config,
    content: &str,
    objective: &str,
    token_budget: Option<u32>,
) -> Op {
    Op::SendMessage(TurnSpec {
        profile_constitution: None,
        max_output_tokens: None,
        content: content.to_string(),
        images: Vec::new(),
        mode: AppMode::Agent,
        route: resolved_route_for_test(config, "local-model"),
        compaction: Box::new(CompactionConfig::default()),
        initial_routed_usage: Box::default(),
        goal_objective: Some(objective.to_string()),
        goal_token_budget: token_budget,
        goal_status: crate::tools::goal::GoalStatus::Active,
        reasoning_effort: None,
        reasoning_effort_auto: false,
        auto_model: false,
        allow_shell: false,
        trust_mode: false,
        auto_approve: false,
        approval_mode: ApprovalMode::Suggest,
        translation_enabled: false,
        allowed_tools: None,
        dynamic_tools: Vec::new(),
        hook_executor: None,
        verbosity: None,
        provenance: UserInputProvenance::ExternalUser,
        submission_id: None,
    })
}

fn system_prompt_text(prompt: SystemPrompt) -> String {
    match prompt {
        SystemPrompt::Text(text) => text,
        SystemPrompt::Blocks(blocks) => blocks
            .into_iter()
            .map(|block| block.text)
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn external_user_message_op(content: &str, mode: AppMode, config: &Config) -> Op {
    Op::SendMessage(TurnSpec {
        profile_constitution: None,
        max_output_tokens: None,
        content: content.to_string(),
        images: Vec::new(),
        mode,
        route: resolved_route_for_test(
            config,
            codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL,
        ),
        compaction: Box::new(CompactionConfig::default()),
        initial_routed_usage: Box::default(),
        goal_objective: None,
        goal_token_budget: None,
        goal_status: crate::tools::goal::GoalStatus::Active,
        reasoning_effort: None,
        reasoning_effort_auto: false,
        auto_model: false,
        allow_shell: true,
        trust_mode: false,
        auto_approve: false,
        approval_mode: ApprovalMode::Suggest,
        translation_enabled: false,
        allowed_tools: None,
        dynamic_tools: Vec::new(),
        hook_executor: None,
        verbosity: None,
        provenance: UserInputProvenance::ExternalUser,
        submission_id: None,
    })
}

fn external_user_message_op_with_submission(
    content: &str,
    mode: AppMode,
    config: &Config,
    submission_id: Option<String>,
) -> Op {
    let mut op = external_user_message_op(content, mode, config);
    if let Op::SendMessage(TurnSpec {
        submission_id: token,
        ..
    }) = &mut op
    {
        *token = submission_id;
    }
    op
}

fn auto_review_message_op(content: &str, config: &Config) -> Op {
    Op::SendMessage(TurnSpec {
        profile_constitution: None,
        max_output_tokens: None,
        content: content.to_string(),
        images: Vec::new(),
        mode: AppMode::Agent,
        route: resolved_route_for_test(
            config,
            codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL,
        ),
        compaction: Box::new(CompactionConfig::default()),
        initial_routed_usage: Box::default(),
        goal_objective: None,
        goal_token_budget: None,
        goal_status: crate::tools::goal::GoalStatus::Active,
        reasoning_effort: None,
        reasoning_effort_auto: false,
        auto_model: false,
        allow_shell: true,
        trust_mode: false,
        auto_approve: false,
        approval_mode: ApprovalMode::Auto,
        translation_enabled: false,
        allowed_tools: None,
        dynamic_tools: Vec::new(),
        hook_executor: None,
        verbosity: None,
        provenance: UserInputProvenance::ExternalUser,
        submission_id: None,
    })
}

struct DropSignal(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

struct BlockingModelClient {
    entered: std::sync::Arc<tokio::sync::Notify>,
    request_dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

struct BlockingGuardianModelClient {
    guardian_entered: std::sync::Arc<tokio::sync::Notify>,
    guardian_dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    streaming_calls: std::sync::atomic::AtomicUsize,
}

struct FailingGuardianModelClient {
    inner: crate::llm_client::mock::MockLlmClient,
}

#[async_trait::async_trait]
impl crate::core::model_client::ModelClient for FailingGuardianModelClient {
    fn provider_name(&self) -> &str {
        self.inner.provider_name()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    async fn create_message(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<codewhale_models::MessageResponse> {
        anyhow::bail!("fixture guardian transport failure")
    }

    async fn create_message_stream(
        &self,
        request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
        crate::core::model_client::ModelClient::create_message_stream(&self.inner, request).await
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

#[async_trait::async_trait]
impl crate::core::model_client::ModelClient for BlockingGuardianModelClient {
    fn provider_name(&self) -> &str {
        "deterministic-blocking-guardian"
    }

    fn model(&self) -> &str {
        "deterministic-blocking-guardian-model"
    }

    async fn create_message(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<codewhale_models::MessageResponse> {
        let _drop_signal = DropSignal(std::sync::Arc::clone(&self.guardian_dropped));
        self.guardian_entered.notify_one();
        std::future::pending().await
    }

    async fn create_message_stream(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
        use crate::llm_client::mock::canned;

        assert_eq!(
            self.streaming_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
            0,
            "cancellation must prevent a follow-up model request"
        );
        let events = canned::tool_call_turn(
            "call-cancelled-guardian",
            "File",
            r#"{"action":"write","path":".env","content":"must-not-run\n"}"#,
        );
        Ok(Box::pin(futures_util::stream::iter(
            events.into_iter().map(Ok),
        )))
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

#[async_trait::async_trait]
impl crate::core::model_client::ModelClient for BlockingModelClient {
    fn provider_name(&self) -> &str {
        "deterministic-blocking"
    }

    fn model(&self) -> &str {
        "deterministic-blocking-model"
    }

    async fn create_message(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<codewhale_models::MessageResponse> {
        std::future::pending().await
    }

    async fn create_message_stream(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
        let _drop_signal = DropSignal(std::sync::Arc::clone(&self.request_dropped));
        self.entered.notify_one();
        std::future::pending().await
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

fn test_tool_surface(
    engine: &Engine,
    registry: crate::tools::ToolRegistry,
    tools: Option<Vec<codewhale_models::Tool>>,
    mode: AppMode,
) -> ToolSurfacePolicy {
    ToolSurfacePolicy::new(
        registry,
        tools,
        mode,
        &engine.config.tools_always_load,
        &[],
        engine.config.strict_tool_mode,
        engine.config.allowed_tools.clone(),
        engine.config.disallowed_tools.clone(),
        engine.config.max_tool_calls,
        crate::core::engine::tool_catalog::ToolMode::Direct,
    )
}

#[tokio::test]
async fn tool_request_snapshot_matches_the_exact_mock_request_payload() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![canned::simple_text_turn("Done.")]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    let context = crate::tools::ToolContext::new(workspace.path().to_path_buf());
    let mut registry = crate::tools::ToolRegistry::new(context);
    registry.register(std::sync::Arc::new(crate::tools::file::ReadFileTool));
    let tools = Some(registry.to_api_tools_with_cache(true));
    let surface = test_tool_surface(&engine, registry, tools, AppMode::Agent);
    let mut turn = crate::core::turn::TurnContext::new(4);

    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");

    let request = mock.last_request().expect("mock request");
    let mut events = handle.rx_event.write().await;
    let snapshot = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event {
            Event::ToolRequestSnapshot { snapshot } => Some(snapshot),
            _ => None,
        })
        .expect("request snapshot event");

    assert_eq!(snapshot.tools_field_present, request.tools.is_some());
    assert_eq!(
        snapshot.tool_count,
        request.tools.as_ref().map_or(0, Vec::len)
    );
    if let Some(request_tool) = request.tools.as_ref().and_then(|tools| tools.first()) {
        assert_eq!(
            snapshot.tools.first().expect("projected tool").name.value,
            request_tool.name
        );
    }
    assert_eq!(snapshot.turn_id.value, turn.id);
    assert_eq!(snapshot.step, 0);
    assert!(snapshot.delivery_status.starts_with("unknown"));
}

/// #6510: the inline ```repl kernel runs model-written Python, so it answers
/// to the `code_execution` gate. A surface that leaves `code_execution` out of
/// its allowlist (plain `exec`'s zero-tool surface, or a narrowed
/// `--allowed-tools`) or denies it must not execute a fence: the reply is the
/// answer, no kernel starts, and no second model call is made.
#[tokio::test]
async fn repl_fence_does_not_run_when_code_execution_is_not_allowed() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    use codewhale_models::{ContentBlock, Message};

    let fence = "```repl\nprint('fence ran')\n```";
    for (allowed, disallowed) in [
        (Some(Vec::new()), None),
        (Some(vec!["read_file".to_string()]), None),
        (None, Some(vec!["code_execution".to_string()])),
    ] {
        let workspace = tempdir().expect("tempdir");
        let mock = std::sync::Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(fence)]));
        let client: crate::core::model_client::SharedModelClient = mock.clone();
        let config = EngineConfig {
            allowed_tools: allowed.clone(),
            disallowed_tools: disallowed.clone(),
            ..deterministic_engine_config(workspace.path())
        };
        let (mut engine, _handle) =
            Engine::new_with_model_client(config, &Config::default(), client);
        engine.session.add_message(Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Answer.".to_string(),
                cache_control: None,
            }],
        });
        let registry = crate::tools::ToolRegistry::new(crate::tools::ToolContext::new(
            workspace.path().to_path_buf(),
        ));
        let policy = test_tool_surface(&engine, registry, None, AppMode::Agent);
        let mut turn = crate::core::turn::TurnContext::new(4);
        let (status, error) = engine.run_turn(&mut turn, policy, None, None).await;

        let case = format!("allowed={allowed:?} disallowed={disallowed:?}");
        assert_eq!(status, TurnOutcomeStatus::Completed, "{case}: {error:?}");
        assert!(
            engine.repl_kernel.is_none(),
            "{case}: kernel must not start"
        );
        assert_eq!(mock.call_count(), 1, "{case}: no follow-up model call");
    }
}

#[tokio::test]
async fn normal_repl_kernel_persists_across_user_turns() {
    use crate::core::engine::tests::rlm_host::{
        admitted_context, fixture_config, install_fixture_route,
    };
    use crate::llm_client::mock::{MockLlmClient, canned};
    use codewhale_models::{ContentBlock, Message, Usage};

    let workspace = tempdir().expect("tempdir");
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        canned::simple_text_turn(
            "```repl\nchild_verdict = sub_query('Return exactly: child route works')\nproof_from_first_turn = f'kernel state survives; {child_verdict}'\nprint('kernel primed', child_verdict)\n```",
        ),
        vec![
            canned::message_start("kernel-child"),
            canned::text_block_start(0),
            canned::text_delta(0, "child route works"),
            canned::block_stop(0),
            canned::message_delta(
                "end_turn",
                Some(Usage {
                    input_tokens: 7,
                    output_tokens: 11,
                    ..Usage::default()
                }),
            ),
            canned::message_stop(),
        ],
        canned::simple_text_turn("First turn complete."),
        canned::simple_text_turn(
            "```repl\nprint(proof_from_first_turn)\nfinalize('persistent kernel verified')\n```",
        ),
    ]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let config = fixture_config("captured-working-model");
    let mut engine_config = deterministic_engine_config(workspace.path());
    engine_config.model = "captured-working-model".into();
    let (mut engine, handle) = Engine::new_with_model_client(engine_config, &config, client);
    install_fixture_route(&mut engine);
    let selected_model = engine.session.model.clone();
    // Full Access: the fence takes `code_execution`'s approval, which this
    // posture already grants.
    engine.session.auto_approve = true;

    engine.session.add_message(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Prime the working kernel.".to_string(),
            cache_control: None,
        }],
    });
    let first_registry =
        crate::tools::ToolRegistry::new(admitted_context(&engine, "first-fixture-turn"));
    let first_policy = test_tool_surface(
        &engine,
        first_registry,
        Some(vec![catalog_tool(CODE_EXECUTION_TOOL_NAME)]),
        AppMode::Agent,
    );
    let mut first_turn = crate::core::turn::TurnContext::new(4);
    let (status, error) = engine
        .run_turn(&mut first_turn, first_policy, None, None)
        .await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    assert_eq!(first_turn.usage.input_tokens, 7);
    assert_eq!(first_turn.usage.output_tokens, 11);
    let child_usage_event = {
        let mut events = handle.rx_event.write().await;
        // Kernel child calls carry their own routed cost receipt, so their
        // per-call telemetry arrives as `RoutedTurnUsage` rather than the
        // parent-route `TurnUsage` receipt.
        std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
            Event::RoutedTurnUsage { usage, .. }
                if usage.input_tokens == 7 && usage.output_tokens == 11 =>
            {
                Some(usage)
            }
            _ => None,
        })
    }
    .expect("kernel child usage must be visible to the cost UI");
    assert_eq!(child_usage_event.input_tokens, 7);
    assert_eq!(child_usage_event.output_tokens, 11);

    engine.session.add_message(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Use the state from the prior turn.".to_string(),
            cache_control: None,
        }],
    });
    let second_registry =
        crate::tools::ToolRegistry::new(admitted_context(&engine, "second-fixture-turn"));
    let second_policy = test_tool_surface(
        &engine,
        second_registry,
        Some(vec![catalog_tool(CODE_EXECUTION_TOOL_NAME)]),
        AppMode::Agent,
    );
    let mut second_turn = crate::core::turn::TurnContext::new(4);
    let (status, error) = engine
        .run_turn(&mut second_turn, second_policy, None, None)
        .await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");

    let kernel = engine.repl_kernel.as_ref().expect("persistent kernel");
    assert!(
        kernel.round_count() >= 4,
        "each REPL call should refresh context and then execute code"
    );
    let final_text = engine
        .session
        .messages
        .last()
        .and_then(|message| {
            message.content.iter().find_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
        })
        .expect("final assistant text");
    assert_eq!(final_text, "persistent kernel verified");

    let child_request = mock
        .captured_requests()
        .into_iter()
        .find(|request| request.messages.iter().any(|message| message.content.iter().any(|block| matches!(block, ContentBlock::Text { text, .. } if text == "Return exactly: child route works"))))
        .expect("the injected model client must service a kernel child query");
    assert_eq!(child_request.model, selected_model);
    assert_eq!(
        child_request.stream,
        Some(true),
        "the canonical Core stream producer services the nested call"
    );
    assert!(child_request.tools.as_ref().is_none_or(Vec::is_empty));
    assert_eq!(mock.captured_requests().len(), 4);
}

/// A recursive `rlm_query` makes a child model write Python. That round runs
/// only after the turn serving the `rlm` call admits it like `code_execution`:
/// outside Full Access it waits on a card of its own, beyond the approval the
/// direct `rlm` call already took. Denied, nothing runs; approved, it runs.
#[tokio::test]
async fn recursive_rlm_round_waits_on_the_code_execution_gate() {
    use crate::core::engine::tests::rlm_host::{
        admitted_context, fixture_config, install_fixture_route,
    };
    use crate::llm_client::mock::{MockLlmClient, canned};
    use codewhale_models::{ContentBlock, Message};

    for approve in [false, true] {
        let workspace = tempdir().expect("tempdir");
        let marker = workspace.path().join("nested-round-ran");
        let nested = format!(
            "```repl\nopen({:?}, 'w').write('x')\nFINAL('nested done')\n```",
            marker.display().to_string()
        );
        let rlm = crate::tools::rlm::RLM_TOOL_NAME;
        let mock = std::sync::Arc::new(MockLlmClient::new(vec![
            canned::tool_call_turn(
                "rlm-open",
                rlm,
                r#"{"action":"open","name":"ctx","content":"fixture context"}"#,
            ),
            canned::tool_call_turn(
                "rlm-eval",
                rlm,
                r#"{"action":"eval","name":"ctx","code":"print(rlm_query('nested context'))"}"#,
            ),
            canned::simple_text_turn(&nested),
            canned::simple_text_turn("Done."),
        ]));
        let client: crate::core::model_client::SharedModelClient = mock.clone();
        let config = fixture_config("captured-working-model");
        let mut engine_config = deterministic_engine_config(workspace.path());
        engine_config.model = "captured-working-model".into();
        let (mut engine, handle) = Engine::new_with_model_client(engine_config, &config, client);
        install_fixture_route(&mut engine);
        engine.session.auto_approve = false;
        engine.session.approval_mode = ApprovalMode::Suggest;
        engine.session.add_message(Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Recurse.".to_string(),
                cache_control: None,
            }],
        });
        let registry = crate::tools::ToolRegistryBuilder::new()
            .with_rlm_tool()
            .build(admitted_context(&engine, "nested-gate-fixture-turn"));
        let policy = test_tool_surface(
            &engine,
            registry,
            Some(vec![
                catalog_tool(rlm),
                catalog_tool(CODE_EXECUTION_TOOL_NAME),
            ]),
            AppMode::Agent,
        );
        let task = tokio::spawn(async move {
            let mut turn = crate::core::turn::TurnContext::new(6);
            engine.run_turn(&mut turn, policy, None, None).await
        });

        let events = handle.rx_event.clone();
        let nested_card = tokio::time::timeout(Duration::from_secs(30), async {
            let mut rx = events.write().await;
            while let Some(event) = rx.recv().await {
                if let Event::ApprovalRequired { id, tool_name, .. } = event {
                    if tool_name == CODE_EXECUTION_TOOL_NAME {
                        return id;
                    }
                    // The direct `rlm` call's own approval.
                    handle.approve_tool_call(&id).await.expect("approve rlm");
                }
            }
            panic!("event stream closed before the nested round asked for approval");
        })
        .await
        .expect("the nested round must wait on its own approval card");
        assert!(
            nested_card.ends_with(".1"),
            "the round is the rlm call's first nested request: {nested_card}"
        );
        assert!(!marker.exists(), "nothing runs before the decision");

        if approve {
            handle
                .approve_tool_call(&nested_card)
                .await
                .expect("approve");
        } else {
            handle.deny_tool_call(&nested_card).await.expect("deny");
        }
        let (status, error) = tokio::time::timeout(Duration::from_secs(60), task)
            .await
            .expect("turn deadline")
            .expect("turn task");
        assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
        assert_eq!(marker.exists(), approve, "approve={approve}");
    }
}

/// A kernel keeps what it loaded under the posture it ran in, so a narrower
/// posture starts later code in a fresh interpreter; an unchanged or broader
/// posture keeps the working kernel.
#[tokio::test]
async fn narrowing_the_posture_discards_python_kernels() {
    use crate::llm_client::mock::MockLlmClient;

    let workspace = tempdir().expect("tempdir");
    let client: crate::core::model_client::SharedModelClient =
        std::sync::Arc::new(MockLlmClient::new(Vec::new()));
    let (mut engine, _handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    // Without a Python interpreter there is no kernel to discard.
    let Ok(kernel) = codewhale_repl::runtime::PythonRuntime::new().await else {
        return;
    };
    engine.repl_kernel = Some(kernel);
    let posture = engine.applied_runtime_authority();

    // Re-applying the same posture is not a narrowing.
    engine
        .apply_change_mode(
            posture.mode,
            posture.allow_shell,
            posture.trust_mode,
            posture.auto_approve,
            posture.approval_mode,
            posture.configured_sandbox_mode.clone(),
        )
        .await;
    assert!(
        engine.repl_kernel.is_some(),
        "an unchanged posture keeps the kernel"
    );

    engine
        .apply_change_mode(
            posture.mode,
            posture.allow_shell,
            posture.trust_mode,
            posture.auto_approve,
            posture.approval_mode,
            Some("read-only".to_string()),
        )
        .await;
    assert!(
        engine.applied_runtime_authority().narrows(&posture),
        "fixture must actually narrow"
    );
    assert!(
        engine.repl_kernel.is_none(),
        "a narrower posture drops the kernel"
    );
}

/// A turn dropped mid-round leaves its kernel broken, and a broken kernel
/// refuses every later round. The next turn starts a fresh kernel instead of
/// failing once on the stale one.
#[tokio::test]
async fn a_broken_repl_kernel_is_replaced_by_the_next_turn() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    use codewhale_models::{ContentBlock, Message};

    let workspace = tempdir().expect("tempdir");
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
        "```repl\nfinalize('fresh kernel ran')\n```",
    )]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (mut engine, _handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    engine.session.auto_approve = true;

    // What a turn dropped mid-round leaves behind.
    let mut stale = codewhale_repl::PythonRuntime::new().await.expect("spawn");
    let dropped = tokio::time::timeout(
        Duration::from_millis(50),
        stale.execute("import time\ntime.sleep(30)"),
    )
    .await;
    assert!(dropped.is_err(), "the round must still be running");
    assert!(stale.is_broken());
    engine.repl_kernel = Some(stale);

    engine.session.add_message(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Run the kernel again.".to_string(),
            cache_control: None,
        }],
    });
    let registry = crate::tools::ToolRegistry::new(crate::tools::ToolContext::new(
        workspace.path().to_path_buf(),
    ));
    let policy = test_tool_surface(
        &engine,
        registry,
        Some(vec![catalog_tool(CODE_EXECUTION_TOOL_NAME)]),
        AppMode::Agent,
    );
    let mut turn = crate::core::turn::TurnContext::new(4);
    let (status, error) = engine.run_turn(&mut turn, policy, None, None).await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    assert!(
        !engine
            .repl_kernel
            .as_ref()
            .expect("a fresh kernel")
            .is_broken()
    );
}

/// Plan mode withholds `code_execution`, and a ```repl fence is not a way
/// around that: even under Full Access and with the tool named in the
/// supplied catalog, the fenced Python does not run in Plan mode.
#[tokio::test]
async fn plan_mode_repl_fence_does_not_execute() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    use codewhale_models::{ContentBlock, Message};

    let workspace = tempdir().expect("tempdir");
    let marker = workspace.path().join("fence-ran");
    let fence = format!(
        "```repl\nopen({:?}, 'w').write('x')\n```",
        marker.display().to_string()
    );
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(&fence)]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (mut engine, _handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    engine.session.auto_approve = true;
    engine.session.add_message(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Plan only.".to_string(),
            cache_control: None,
        }],
    });
    let registry = crate::tools::ToolRegistry::new(crate::tools::ToolContext::new(
        workspace.path().to_path_buf(),
    ));
    let policy = test_tool_surface(
        &engine,
        registry,
        Some(vec![catalog_tool(CODE_EXECUTION_TOOL_NAME)]),
        AppMode::Plan,
    );
    let mut turn = crate::core::turn::TurnContext::new(4);
    let (status, error) = engine.run_turn(&mut turn, policy, None, None).await;

    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    assert!(
        engine.repl_kernel.is_none(),
        "kernel must not start in Plan"
    );
    assert!(!marker.exists(), "fenced Python must not run in Plan");
    assert_eq!(mock.call_count(), 1, "no follow-up model call");
}

/// A ```repl fence runs model-written Python, so outside Full Access it waits
/// on the same approval card as `code_execution`. Denied, it does not run and
/// the turn says so; approved, it runs.
#[tokio::test]
async fn repl_fence_requires_code_execution_approval() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    use codewhale_models::{ContentBlock, Message};

    for approve in [false, true] {
        let workspace = tempdir().expect("tempdir");
        let marker = workspace.path().join("fence-ran");
        let fence = format!(
            "```repl\nopen({:?}, 'w').write('x')\nfinalize('done')\n```",
            marker.display().to_string()
        );
        let mock = std::sync::Arc::new(MockLlmClient::new(vec![
            canned::simple_text_turn(&fence),
            canned::simple_text_turn("Done."),
        ]));
        let client: crate::core::model_client::SharedModelClient = mock.clone();
        let (mut engine, handle) = Engine::new_with_model_client(
            deterministic_engine_config(workspace.path()),
            &Config::default(),
            client,
        );
        engine.session.auto_approve = false;
        engine.session.approval_mode = ApprovalMode::Suggest;
        engine.session.add_message(Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Compute.".to_string(),
                cache_control: None,
            }],
        });
        let registry = crate::tools::ToolRegistry::new(crate::tools::ToolContext::new(
            workspace.path().to_path_buf(),
        ));
        let policy = test_tool_surface(
            &engine,
            registry,
            Some(vec![catalog_tool(CODE_EXECUTION_TOOL_NAME)]),
            AppMode::Agent,
        );
        let task = tokio::spawn(async move {
            let mut turn = crate::core::turn::TurnContext::new(4);
            let outcome = engine.run_turn(&mut turn, policy, None, None).await;
            (engine, outcome)
        });

        let events = handle.rx_event.clone();
        let approval_id = tokio::time::timeout(Duration::from_secs(10), async {
            let mut rx = events.write().await;
            while let Some(event) = rx.recv().await {
                if let Event::ApprovalRequired { id, tool_name, .. } = event {
                    assert_eq!(tool_name, CODE_EXECUTION_TOOL_NAME);
                    return id;
                }
            }
            panic!("event stream closed before the fence asked for approval");
        })
        .await
        .expect("the fence must wait on an approval card");
        assert!(!marker.exists(), "nothing runs before the decision");

        if approve {
            handle
                .approve_tool_call(&approval_id)
                .await
                .expect("approve");
        } else {
            handle.deny_tool_call(&approval_id).await.expect("deny");
        }
        let (engine, (status, error)) = tokio::time::timeout(Duration::from_secs(30), task)
            .await
            .expect("turn deadline")
            .expect("turn task");
        assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
        assert_eq!(marker.exists(), approve, "approve={approve}");
        if !approve {
            assert!(
                engine.repl_kernel.is_none(),
                "denied fence starts no kernel"
            );
            let note = {
                let mut rx = events.write().await;
                std::iter::from_fn(|| rx.try_recv().ok()).any(|event| {
                    matches!(event, Event::Status { message } if message.starts_with("REPL block not run: not approved"))
                })
            };
            assert!(note, "a denied fence leaves a visible status note");
        }
    }
}
