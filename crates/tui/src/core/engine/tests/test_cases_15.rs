

#[test]
fn subagent_results_are_summarized_before_parent_context_insertion() {
    let long_result = "verified detail\n".repeat(1_000);
    let output = ToolResult::success(
        json!({
            "agent_id": "agent_1234abcd",
            "agent_type": "explore",
            "assignment": {
                "objective": "Inspect the RLM rendering path and report the smallest fix."
            },
            "model": "deepseek-v4-flash",
            "status": "Completed",
            "result": long_result,
            "steps_taken": 12,
            "duration_ms": 3456
        })
        .to_string(),
    );

    let context = compact_tool_result_for_context("deepseek-v4-pro", "agent", &output);

    assert!(context.contains("[sub-agent result summarized for parent context]"));
    assert!(context.contains("agent_1234abcd (explore) status=Completed"));
    assert!(context.contains("Inspect the RLM rendering path"));
    assert!(context.contains("steps=12"));
    assert!(context.len() < output.content.len());
    assert!(context.contains("self-report"));
    assert!(context.contains("verify side effects"));
    assert!(context.contains("verify side effects with `read`"));
    assert!(!context.contains("read_file") && !context.contains("list_dir"));
    assert!(context.contains("handle_read"));
    // #6747: the guidance names only callable tools (no hidden `File`) and
    // teaches the deferred `handle_read` activation path.
    crate::tools::canonical_action::tests::assert_text_names_only_callable_tools(
        "sub-agent summary guidance",
        &super::context::subagent_summary_guidance(),
    );
}

#[test]
fn wait_payloads_survive_parent_context_compaction() {
    let raw = json!({
        "action": "wait",
        "until": "all",
        "all_settled": true,
        "settled": [{"agent_id": "agent_1234abcd", "status": "Completed"}],
        "still_running": [],
        "waited_ms": 1234,
        "timed_out": false,
        "note": "joined the fan-out"
    })
    .to_string();
    let output = ToolResult::success(raw.clone());

    let context = compact_tool_result_for_context("deepseek-v4-pro", "agent", &output);

    assert!(
        !context.contains("status=unknown"),
        "a wait envelope must not be projected as an unknown snapshot: {context}"
    );
    assert!(context.contains("agent_1234abcd"));
    assert!(context.contains("still_running"));
    assert!(context.contains("waited_ms"));
    assert_eq!(
        context, raw,
        "small coordination payloads pass through verbatim"
    );
}

#[test]
fn run_verifiers_results_are_structured_before_context_insertion() {
    let noisy_failure = "node lint failure detail\n".repeat(300);
    let noisy_success = "successful check output\n".repeat(300);
    let output = ToolResult::success(
        json!({
            "success": false,
            "profile": "auto",
            "level": "quick",
            "workspace": "/repo",
            "gate_count": 3,
            "passed": 1,
            "failed": 1,
            "skipped": 1,
            "summary": "1 passed, 1 failed, 1 skipped",
            "gates": [
                {
                    "name": "rust-check",
                    "ecosystem": "rust",
                    "status": "passed",
                    "command": "cargo check --workspace --locked",
                    "cwd": "/repo",
                    "exit_code": 0,
                    "duration_ms": 110,
                    "stdout": noisy_success.clone(),
                    "stderr": "",
                    "stdout_truncated": false,
                    "stderr_truncated": false,
                    "skipped_reason": null
                },
                {
                    "name": "node-lint",
                    "ecosystem": "node",
                    "status": "failed",
                    "command": "npm run lint",
                    "cwd": "/repo",
                    "exit_code": 1,
                    "duration_ms": 220,
                    "stdout": "",
                    "stderr": noisy_failure,
                    "stdout_truncated": false,
                    "stderr_truncated": false,
                    "skipped_reason": null
                },
                {
                    "name": "python-pytest",
                    "ecosystem": "python",
                    "status": "skipped",
                    "command": "",
                    "cwd": "/repo",
                    "exit_code": null,
                    "duration_ms": 0,
                    "stdout": "",
                    "stderr": "",
                    "stdout_truncated": false,
                    "stderr_truncated": false,
                    "skipped_reason": "pytest is not installed"
                }
            ]
        })
        .to_string(),
    );

    let context = compact_tool_result_for_context("deepseek-v4-pro", "run_verifiers", &output);

    assert!(context.contains("[run_verifiers result summarized for context]"));
    assert!(context.contains("summary: 1 passed, 1 failed, 1 skipped"));
    assert!(context.contains("selection: profile=auto, level=quick"));
    assert!(context.contains("- node-lint (node): failed exit=1"));
    assert!(context.contains("command: npm run lint"));
    assert!(context.contains("- python-pytest (python): skipped"));
    assert!(context.contains("pytest is not installed"));
    assert!(context.contains("- rust-check (rust): passed exit=0"));
    assert!(context.len() < output.content.len());
    assert!(
        !context.contains(&noisy_success),
        "successful gate stdout should not be copied into parent context"
    );
}

#[test]
fn run_tests_results_are_structured_before_context_insertion() {
    let stdout = "running test suite\n".repeat(500);
    let stderr = "error[E0425]: cannot find value `missing`\n".repeat(500);
    let output = ToolResult::success(
        json!({
            "success": false,
            "exit_code": 101,
            "stdout": stdout,
            "stderr": stderr,
            "command": "(cd /repo && cargo test --workspace --all-features)"
        })
        .to_string(),
    );

    let context = compact_tool_result_for_context("deepseek-v4-pro", "run_tests", &output);

    assert!(context.contains("[run_tests result summarized for context]"));
    assert!(context.contains("status: failed, exit_code: 101"));
    assert!(context.contains("cargo test --workspace --all-features"));
    assert!(context.contains("error[E0425]"));
    assert!(context.contains("running test suite"));
    assert!(context.len() < output.content.len());
}

#[test]
fn task_gate_run_results_are_structured_before_context_insertion() {
    let output = ToolResult::success(
        json!({
            "gate": {
                "id": "gate_abcd1234",
                "gate": "clippy",
                "command": "cargo clippy -p codewhale-tui --all-targets --all-features --locked -- -D warnings",
                "cwd": "/repo",
                "exit_code": 1,
                "status": "failed",
                "classification": "compile_failure",
                "duration_ms": 5000,
                "summary": "warning promoted to error in verifier.rs",
                "log_path": "/repo/.codewhale/runtime/gate.log",
                "recorded_at": "2026-06-01T12:00:00Z"
            },
            "stdout_summary": "",
            "stderr_summary": "warning promoted to error"
        })
        .to_string(),
    );

    let context = compact_tool_result_for_context("deepseek-v4-pro", "task_gate_run", &output);

    assert!(context.contains("[task_gate_run result summarized for context]"));
    assert!(context.contains("gate: clippy, status: failed, exit_code: 1"));
    assert!(context.contains("cargo clippy -p codewhale-tui"));
    assert!(context.contains("summary: warning promoted to error"));
    assert!(context.contains("log_path: /repo/.codewhale/runtime/gate.log"));
}

#[test]
fn refresh_system_prompt_leaves_working_set_out_of_system_prompt() {
    let tmp = tempdir().expect("tempdir");
    fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
    fs::write(tmp.path().join("src/lib.rs"), "pub fn sample() {}").expect("write");

    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    engine
        .session
        .working_set
        .observe_user_message("please inspect src/lib.rs", tmp.path());

    engine.refresh_system_prompt();

    let prompt = match &engine.session.system_prompt {
        Some(SystemPrompt::Text(text)) => text.clone(),
        Some(SystemPrompt::Blocks(blocks)) => blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        None => panic!("expected system prompt"),
    };
    assert!(!prompt.contains(WORKING_SET_SUMMARY_MARKER));
}

#[test]
fn working_set_reaches_model_as_turn_metadata() {
    let tmp = tempdir().expect("tempdir");
    fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
    fs::write(tmp.path().join("src/lib.rs"), "pub fn sample() {}").expect("write");

    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    engine
        .session
        .working_set
        .observe_user_message("please inspect src/lib.rs", tmp.path());
    let user_msg =
        engine.user_text_message_with_turn_metadata("please inspect src/lib.rs".to_string());
    engine.session.add_message(user_msg);

    let messages = engine.messages_with_turn_metadata();
    let last_block = messages
        .first()
        .and_then(|message| message.content.last())
        .expect("turn metadata block");
    let ContentBlock::Text { text, .. } = last_block else {
        panic!("expected text metadata block");
    };
    assert!(text.starts_with("<turn_meta>\n"));
    assert!(text.contains(WORKING_SET_SUMMARY_MARKER));
    assert!(text.contains("src/lib.rs"));
}

#[test]
fn turn_metadata_includes_git_workspace_snapshot_in_repo() {
    use crate::dependencies::ExternalTool;

    if !crate::dependencies::Git::available() {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let init = crate::dependencies::Git::output(&["init", "-q"], root);
    if init.is_err() || !init.unwrap().status.success() {
        return;
    }

    let config = EngineConfig {
        workspace: root.to_path_buf(),
        ..Default::default()
    };
    let (engine, _handle) = Engine::new(config, &Config::default());
    let user_msg = engine.user_text_message_with_turn_metadata("inspect repo state".to_string());
    let last_block = user_msg.content.last().expect("turn metadata block");
    let ContentBlock::Text { text, .. } = last_block else {
        panic!("expected text metadata block");
    };

    if let Some(snapshot) = crate::git_status::collect(root) {
        assert!(
            text.contains(&format!("Git workspace: {snapshot}")),
            "turn_meta should include git snapshot: {text}"
        );
    }
}

/// #5187 (k3-gap F3): the git snapshot line is emitted only when it actually
/// changes — an unchanged workspace must not re-emit the line (churning the
/// block's bytes and priming caution), a changed one must re-emit it once.
#[test]
fn turn_metadata_git_snapshot_emitted_only_on_change() {
    use crate::dependencies::ExternalTool;

    if !crate::dependencies::Git::available() {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let init = crate::dependencies::Git::output(&["init", "-q"], root);
    if init.is_err() || !init.unwrap().status.success() {
        return;
    }
    if crate::git_status::collect(root).is_none() {
        return;
    }

    let config = EngineConfig {
        workspace: root.to_path_buf(),
        ..Default::default()
    };
    let (engine, _handle) = Engine::new(config, &Config::default());
    let meta_of = |msg: Message| -> String {
        let ContentBlock::Text { text, .. } = msg.content.last().expect("turn metadata block")
        else {
            panic!("expected text metadata block");
        };
        text.clone()
    };

    let first = meta_of(engine.user_text_message_with_turn_metadata("first turn".to_string()));
    assert!(
        first.contains("Git workspace:"),
        "first turn must emit the git snapshot: {first}"
    );

    let second = meta_of(engine.user_text_message_with_turn_metadata("second turn".to_string()));
    assert!(
        !second.contains("Git workspace:"),
        "unchanged git state must not re-emit the snapshot line: {second}"
    );

    // Dirty the workspace: the snapshot changes, so the line is emitted once.
    std::fs::write(root.join("turn-meta-gating.txt"), "changed").expect("write file");
    let third = meta_of(engine.user_text_message_with_turn_metadata("third turn".to_string()));
    assert!(
        third.contains("Git workspace:"),
        "changed git state must re-emit the snapshot line: {third}"
    );

    let fourth = meta_of(engine.user_text_message_with_turn_metadata("fourth turn".to_string()));
    assert!(
        !fourth.contains("Git workspace:"),
        "the re-emitted snapshot must be cached again: {fourth}"
    );
}

#[test]
fn turn_metadata_includes_current_local_date_without_working_set() {
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        model: "deepseek-v4-flash".to_string(),
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    let user_msg = engine.user_text_message_with_turn_metadata("what is today's date?".to_string());
    engine.session.add_message(user_msg);

    let messages = engine.messages_with_turn_metadata();
    let last_block = messages
        .first()
        .and_then(|message| message.content.last())
        .expect("turn metadata block");
    let ContentBlock::Text { text, .. } = last_block else {
        panic!("expected text metadata block");
    };

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    assert!(text.starts_with("<turn_meta>\n"));
    assert!(text.contains(&format!("Current local date: {today}")));
    assert!(
        text.contains(&format!("Current workspace: {}", tmp.path().display())),
        "workspace must remain in the block: {text}"
    );
    assert!(
        text.contains("Current permission posture: Ask"),
        "the active posture must remain model-visible: {text}"
    );
    // Turn-meta diet: no telemetry may re-enter the per-turn block.
    for telemetry in [
        "Current model:",
        "Current mode:",
        "Input provenance:",
        "Input authority:",
        "Auto model route:",
        "Auto reasoning effort:",
        "Session token usage:",
        "Active goal resource usage:",
        "Active goal token budget:",
    ] {
        assert!(
            !text.contains(telemetry),
            "{telemetry} leaked into turn_meta: {text}"
        );
    }
}

#[test]
fn turn_metadata_surfaces_goal_budget_only_while_goal_active() {
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        model: "deepseek-v4-flash".to_string(),
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    // Even with session usage recorded, the per-turn block must not surface
    // it: totals/cache figures are UI telemetry, not model steering signal.
    engine.session.total_usage.add(&Usage {
        input_tokens: 1_200,
        output_tokens: 300,
        prompt_cache_hit_tokens: Some(800),
        prompt_cache_miss_tokens: Some(400),
        prompt_cache_write_tokens: Some(400),
        ..Default::default()
    });
    {
        let mut goal = engine.config.goal_state.lock().expect("goal lock");
        goal.create("Finish telemetry visibility".to_string(), Some(2_000))
            .expect("create goal");
        goal.record_usage(1_000, 100, std::time::Instant::now());
    }

    let user_msg = engine
        .user_text_message_with_turn_metadata("continue the long-running release task".to_string());
    let last_block = user_msg.content.last().expect("turn metadata block");
    let ContentBlock::Text { text, .. } = last_block else {
        panic!("expected text metadata block");
    };

    // The goal budget stays (model pacing), and only while the goal is active.
    assert!(
        text.contains("Active goal token budget: 2000"),
        "goal budget should be model-visible: {text}"
    );
    // Usage/time deltas, rates, and continuation counts are telemetry.
    for telemetry in [
        "Session token usage:",
        "cache hits",
        "cache writes",
        "Active goal resource usage:",
        "tok/s",
        "continuations",
        "50% budget",
    ] {
        assert!(
            !text.contains(telemetry),
            "{telemetry} leaked into turn_meta: {text}"
        );
    }

    // Without an active goal the budget line must vanish entirely.
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        model: "deepseek-v4-flash".to_string(),
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (engine, _handle) = Engine::new(config, &Config::default());
    let user_msg = engine.user_text_message_with_turn_metadata("no goal".to_string());
    let ContentBlock::Text { text, .. } = user_msg.content.last().expect("turn metadata block")
    else {
        panic!("expected text metadata block");
    };
    assert!(
        !text.contains("Active goal token budget:"),
        "budget must not be emitted when no goal is active: {text}"
    );
}

#[test]
fn context_pressure_message_emits_only_at_warning_and_critical_thresholds() {
    const WARNING: &str = "Context pressure: warning — ESCALATED: prefer /compact, narrow scope, or finish the current task";
    const CRITICAL: &str = "Context pressure: critical — CRITICAL: stop expanding scope; run /compact immediately or finish the current task";

    assert_eq!(context_pressure_message(84.99), None);
    assert_eq!(context_pressure_message(85.0), Some(WARNING));
    assert_eq!(context_pressure_message(94.99), Some(WARNING));
    assert_eq!(context_pressure_message(95.0), Some(CRITICAL));
    assert_eq!(context_pressure_message(100.0), Some(CRITICAL));

    // Threshold labels steer a decision without exposing a continuously
    // changing percentage, token count, or headroom value.
    for line in [WARNING, CRITICAL] {
        assert!(!line.contains('%'), "{line}");
        assert!(!line.contains("tokens"), "{line}");
        assert!(!line.contains("headroom"), "{line}");
    }
}

#[test]
fn runtime_turn_metadata_condenses_non_authoritative_provenance_to_one_line() {
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (engine, _handle) = Engine::new(config, &Config::default());
    let msg = engine.runtime_text_message_with_turn_metadata(
        "改吧".to_string(),
        UserInputProvenance::AssistantGenerated,
    );
    let last_block = msg.content.last().expect("turn metadata block");
    let ContentBlock::Text { text, .. } = last_block else {
        panic!("expected text metadata block");
    };

    // Reduced authority on a non-external turn is the sole signal: one
    // condensed line, not the former two-line provenance/authority pair.
    assert!(
        text.contains("Input provenance: assistant_generated (non-authoritative)"),
        "{text}"
    );
    assert!(!text.contains("Input authority:"), "{text}");
    assert!(!text.contains("Input provenance: external_user"), "{text}");
}

#[test]
fn turn_metadata_omits_route_and_reasoning_effort_telemetry() {
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (engine, _handle) = Engine::new(config, &Config::default());

    let user_msg = engine.user_text_message_with_turn_metadata_for_route(
        "debug this regression".to_string(),
        "deepseek-v4-pro",
        true,
        Some("max"),
        true,
    );
    let last_block = user_msg.content.last().expect("turn metadata block");
    let ContentBlock::Text { text, .. } = last_block else {
        panic!("expected text metadata block");
    };

    // Model, auto-route, and auto-reasoning-effort lines were pure telemetry
    // and must never re-enter the per-turn block.
    assert!(!text.contains("Current model:"), "{text}");
    assert!(!text.contains("Auto model route:"), "{text}");
    assert!(!text.contains("Auto reasoning effort:"), "{text}");
    assert!(!text.contains("debug this regression"));
    assert!(
        text.starts_with(
            "<turn_meta>
Current local date:"
        ),
        "{text}"
    );
}

#[test]
fn turn_metadata_keeps_stable_fields_while_pressure_reports_live_estimates() {
    // Live estimates belong in appended turn metadata, never in the pinned
    // system prefix. Unrelated metadata remains stable as the transcript grows.
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        model: "deepseek-v4-flash".to_string(),
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());

    // Use explicit route limits so the fixture exercises the critical band
    // without depending on a model catalog entry or provider default.
    engine.session.messages.push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "x".repeat(100_000),
            cache_control: None,
        }],
    });
    let prompt_context = NextTurnPromptContext::for_planned_turn(
        ProviderKind::Deepseek,
        "deepseek-v4-flash".to_string(),
        Some(codewhale_config::route::RouteLimits {
            context_tokens: Some(10_000),
            input_tokens: None,
            output_tokens: Some(512),
        }),
        AppMode::Agent,
        None,
        crate::tools::goal::GoalStatus::Active,
        None,
        false,
        None,
    );

    let meta_of = |msg: &Message| -> String {
        let ContentBlock::Text { text, .. } = msg.content.last().expect("turn metadata block")
        else {
            panic!("expected text metadata block");
        };
        text.clone()
    };
    let message_for = |engine: &Engine| {
        engine.user_text_message_from_snapshot(
            "stable input".to_string(),
            &prompt_context.model,
            false,
            None,
            false,
            UserInputProvenance::ExternalUser,
            TurnMetadataSnapshot {
                prompt_context: &prompt_context,
                system_prompt: None,
                approval_mode: engine.session.approval_mode,
                working_set: &engine.session.working_set,
                policy_narrowing: None,
            },
        )
    };

    let first = message_for(&engine);
    let first_meta = meta_of(&first);
    assert!(
        !first_meta.contains("Context pressure:"),
        "automatic continuity must not ask the user to manage context: {first_meta}"
    );
    assert!(!first_meta.contains("/compact"));
    engine.config.compaction.enabled = false;
    let first = message_for(&engine);
    let first_meta = meta_of(&first);
    assert!(
        first_meta.contains("Context pressure: critical"),
        "fixture must exercise the pressure line: {first_meta}"
    );

    engine.session.add_message(first);
    let second = message_for(&engine);
    let second_meta = meta_of(&second);
    let without_pressure = |metadata: &str| {
        metadata
            .lines()
            .filter(|line| !line.contains("Context pressure:"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        without_pressure(&first_meta),
        without_pressure(&second_meta)
    );
    assert!(second_meta.contains("Estimated input:"));
    assert!(second_meta.contains("Making room automatically is off"));
}

#[tokio::test]
async fn interrupted_turn_names_surviving_background_shell_jobs() {
    // DGF-03 (dogfood 2026-08-02): Esc says "Turn interrupted" while
    // detached background shells keep writing files. The interrupt path
    // must name the survivors so the copy stops lying about what stopped.
    let tmp = tempdir().expect("tempdir");
    let marker = tmp.path().join("survivor-marker.txt");
    let shell_manager = crate::tools::shell::new_shared_shell_manager(tmp.path().to_path_buf());

    let runtime_services = crate::tools::spec::RuntimeToolServices {
        shell_manager: Some(shell_manager.clone()),
        ..crate::tools::spec::RuntimeToolServices::default()
    };
    let engine_config = EngineConfig {
        model: "deepseek-v4-flash".to_string(),
        workspace: tmp.path().to_path_buf(),
        snapshots_enabled: false,
        terminal_chrome_enabled: false,
        runtime_services,
        ..EngineConfig::default()
    };
    let (engine, handle) = Engine::new(engine_config, &Config::default());

    // Background sleep-then-write: still running at interrupt time, and its
    // write lands only after the UI would have said "interrupted". Stamp the
    // engine's immutable session owner so a replacement session cannot see or
    // control it.
    let task_id = {
        let mut manager = shell_manager.lock().expect("shell manager");
        let result = manager
            .execute_with_options_env_for_session(
                &format!("sleep 5 && touch '{}'", marker.display()),
                None,
                60_000,
                true,
                None,
                false,
                None,
                HashMap::new(),
                &engine.session.id,
            )
            .expect("spawn background job");
        result.task_id.expect("background task id")
    };
    assert!(
        !marker.exists(),
        "marker must not exist before the interrupt"
    );

    engine.emit_interrupted_survivor_status().await;

    let mut events = handle.rx_event.write().await;
    let mut survivor_line = None;
    while let Ok(event) = events.try_recv() {
        if let Event::Status { message } = event
            && message.contains("background shell job")
        {
            survivor_line = Some(message);
        }
    }
    let survivor_line = survivor_line.expect("interrupt must name surviving background jobs");
    assert!(survivor_line.contains(&task_id), "{survivor_line}");
    assert!(
        survivor_line.contains("may still write files"),
        "{survivor_line}"
    );
    assert!(
        !marker.exists(),
        "the honesty line must fire while the job is still running"
    );

    // Cleanup: don't leave the sleeper running after the test.
    let _ = shell_manager.lock().expect("shell manager").kill(&task_id);
}

/// R6 injection-size regression: the per-turn `<turn_meta>` block, built
/// (never sent) from the same snapshot path production uses. Measured 254B
/// on 2026-08-02; the unavailable-backend qualifier adds 48B (Linux without
/// bwrap, all Windows). Ceiling is that host's measured size +10%
/// so growth is a reviewed act.
const TURN_META_BYTE_CEILING: usize = 333;

#[test]
fn turn_meta_block_stays_within_measured_ceiling() {
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        model: "deepseek-v4-flash".to_string(),
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (engine, _handle) = Engine::new(config, &Config::default());
    let prompt_context = NextTurnPromptContext::for_planned_turn(
        ProviderKind::Deepseek,
        "deepseek-v4-flash".to_string(),
        None,
        AppMode::Agent,
        None,
        crate::tools::goal::GoalStatus::Active,
        None,
        false,
        None,
    );
    let message = engine.user_text_message_from_snapshot(
        "hello".to_string(),
        &prompt_context.model,
        false,
        None,
        false,
        UserInputProvenance::ExternalUser,
        TurnMetadataSnapshot {
            prompt_context: &prompt_context,
            system_prompt: None,
            approval_mode: engine.session.approval_mode,
            working_set: &engine.session.working_set,
            policy_narrowing: None,
        },
    );
    let ContentBlock::Text { text, .. } = message.content.last().expect("turn metadata block")
    else {
        panic!("expected text metadata block");
    };
    assert!(
        text.len() <= TURN_META_BYTE_CEILING,
        "turn_meta grew past its reviewed ceiling: {}B > {TURN_META_BYTE_CEILING}B. If deliberate, re-measure and raise the ceiling in the same commit.",
        text.len()
    );
}

#[test]
fn turn_metadata_names_the_effective_sandbox_posture() {
    // DGF-02 (dogfood 2026-08-02): the model must know its own sandbox
    // posture, derived from the same resolver tool execution uses, so an
    // approved-then-sandbox-blocked write never reads as a mystery failure.
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        model: "deepseek-v4-flash".to_string(),
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());

    let meta_for_mode = |engine: &Engine, mode: AppMode| -> String {
        let prompt_context = NextTurnPromptContext::for_planned_turn(
            ProviderKind::Deepseek,
            "deepseek-v4-flash".to_string(),
            None,
            mode,
            None,
            crate::tools::goal::GoalStatus::Active,
            None,
            false,
            None,
        );
        let message = engine.user_text_message_from_snapshot(
            "hello".to_string(),
            &prompt_context.model,
            false,
            None,
            false,
            UserInputProvenance::ExternalUser,
            TurnMetadataSnapshot {
                prompt_context: &prompt_context,
                system_prompt: None,
                approval_mode: engine.session.approval_mode,
                working_set: &engine.session.working_set,
                policy_narrowing: None,
            },
        );
        let ContentBlock::Text { text, .. } = message.content.last().expect("turn metadata block")
        else {
            panic!("expected text metadata block");
        };
        text.clone()
    };

    let agent_meta = meta_for_mode(&engine, AppMode::Agent);
    assert!(
        agent_meta.contains("Current sandbox posture: workspace-write"),
        "{agent_meta}"
    );

    // Plan mode must surface the read-only clamp without promising that this
    // non-interactive posture can open an escalation prompt.
    let plan_meta = meta_for_mode(&engine, AppMode::Plan);
    assert!(
        plan_meta.contains(
            "Current sandbox posture: read-only (shell writes are blocked; ordinary approval does not change this)"
        ),
        "{plan_meta}"
    );

    // Pin deterministic states instead of branching on the CI host. The
    // production value is also captured once at engine construction.
    engine.sandbox_enforcement = crate::sandbox::policy::SandboxEnforcement::LocalOs;
    let local_meta = meta_for_mode(&engine, AppMode::Agent);
    assert!(
        local_meta.contains("local OS sandbox applied"),
        "{local_meta}"
    );

    engine.sandbox_enforcement = crate::sandbox::policy::SandboxEnforcement::Unavailable;
    let unavailable_meta = meta_for_mode(&engine, AppMode::Agent);
    assert!(
        unavailable_meta.contains("policy only; no execution sandbox available"),
        "{unavailable_meta}"
    );

    engine.sandbox_enforcement = crate::sandbox::policy::SandboxEnforcement::ExternalBackend;
    let external_meta = meta_for_mode(&engine, AppMode::Agent);
    assert!(
        external_meta.contains("workspace-write policy"),
        "{external_meta}"
    );
    assert!(
        external_meta.contains("external execution backend configured"),
        "{external_meta}"
    );
    assert!(
        external_meta.contains("isolation unverified by Codewhale"),
        "{external_meta}"
    );
    assert!(
        !external_meta.contains("writes inside the workspace"),
        "{external_meta}"
    );
}

#[test]
fn provenance_gate_preserves_standing_yolo_for_runtime_and_subagent_continuations() {
    let all_provenances = [
        UserInputProvenance::ExternalUser,
        UserInputProvenance::Runtime,
        UserInputProvenance::SubAgentHandoff,
        UserInputProvenance::ImportedTranscript,
        UserInputProvenance::MemoryRecall,
        UserInputProvenance::AssistantGenerated,
    ];
    let inheriting_provenances = [
        UserInputProvenance::ExternalUser,
        UserInputProvenance::Runtime,
        UserInputProvenance::SubAgentHandoff,
    ];

    for provenance in all_provenances {
        let policy = effective_input_policy(
            provenance,
            AppMode::Agent,
            "continue",
            true,
            true,
            true,
            ApprovalMode::Auto,
        );

        if inheriting_provenances.contains(&provenance) {
            assert_eq!(policy.mode, AppMode::Agent, "{provenance:?}");
            assert!(policy.allow_shell, "{provenance:?}");
            assert!(policy.trust_mode, "{provenance:?}");
            assert!(policy.auto_approve, "{provenance:?}");
            assert_eq!(policy.approval_mode, ApprovalMode::Auto, "{provenance:?}");
            assert!(policy.status().is_none(), "{provenance:?}");
        } else {
            assert_eq!(policy.mode, AppMode::Agent, "{provenance:?}");
            assert!(policy.allow_shell, "{provenance:?}");
            assert!(!policy.trust_mode, "{provenance:?}");
            assert!(!policy.auto_approve, "{provenance:?}");
            assert_eq!(
                policy.approval_mode,
                ApprovalMode::Suggest,
                "{provenance:?}"
            );
            assert!(
                policy.status().as_deref().is_some_and(
                    |status| status.contains("cannot inherit standing auto-approval authority")
                ),
                "{provenance:?}"
            );
        }
    }
}

#[test]
fn provenance_gate_never_invents_auto_authority_for_non_yolo_sessions() {
    let all_provenances = [
        UserInputProvenance::ExternalUser,
        UserInputProvenance::Runtime,
        UserInputProvenance::SubAgentHandoff,
        UserInputProvenance::ImportedTranscript,
        UserInputProvenance::MemoryRecall,
        UserInputProvenance::AssistantGenerated,
    ];

    for provenance in all_provenances {
        let policy = effective_input_policy(
            provenance,
            AppMode::Agent,
            "continue",
            true,
            false,
            false,
            ApprovalMode::Suggest,
        );

        assert_eq!(policy.mode, AppMode::Agent, "{provenance:?}");
        assert!(policy.allow_shell, "{provenance:?}");
        assert!(!policy.trust_mode, "{provenance:?}");
        assert!(!policy.auto_approve, "{provenance:?}");
        assert_eq!(
            policy.approval_mode,
            ApprovalMode::Suggest,
            "{provenance:?}"
        );
        assert!(policy.status().is_none(), "{provenance:?}");
    }
}

#[test]
fn full_access_posture_normalizes_a_stale_auto_approve_bit() {
    let policy = effective_input_policy(
        UserInputProvenance::SubAgentHandoff,
        AppMode::Agent,
        "continue",
        true,
        true,
        false,
        ApprovalMode::Bypass,
    );

    assert_eq!(policy.mode, AppMode::Agent);
    assert_eq!(policy.approval_mode, ApprovalMode::Bypass);
    assert!(policy.auto_approve);
    assert!(policy.status().is_none());
}

#[test]
fn self_generated_fake_approvals_cannot_authorize_work() {
    let non_authoritative_origins = [
        UserInputProvenance::ImportedTranscript,
        UserInputProvenance::MemoryRecall,
        UserInputProvenance::AssistantGenerated,
    ];

    for provenance in non_authoritative_origins {
        for content in ["改吧", "嗯"] {
            let policy = effective_input_policy(
                provenance,
                AppMode::Agent,
                content,
                true,
                true,
                true,
                ApprovalMode::Bypass,
            );

            assert_eq!(policy.mode, AppMode::Agent, "{provenance:?} {content}");
            assert!(policy.allow_shell, "{provenance:?} {content}");
            assert!(!policy.trust_mode, "{provenance:?} {content}");
            assert!(!policy.auto_approve, "{provenance:?} {content}");
            assert_eq!(
                policy.approval_mode,
                ApprovalMode::Suggest,
                "{provenance:?} {content}"
            );
            assert!(
                policy.status().as_deref().is_some_and(
                    |status| status.contains("cannot inherit standing auto-approval authority")
                ),
                "{provenance:?} {content}"
            );
        }
    }
}

#[test]
fn external_prompt_wording_never_changes_effective_mode_or_authority() {
    let cases = [
        (
            AppMode::Agent,
            ApprovalMode::Suggest,
            false,
            false,
            "你在帮我看看 外卖部分还哪里没有使用多语言",
        ),
        (
            AppMode::Agent,
            ApprovalMode::Bypass,
            true,
            true,
            "check the failing tests and review the logs",
        ),
        (
            AppMode::Agent,
            ApprovalMode::Suggest,
            false,
            false,
            "检查外卖模块并修复缺少的多语言注入",
        ),
    ];

    for (requested_mode, approval_mode, trust_mode, auto_approve, content) in cases {
        let policy = effective_input_policy(
            UserInputProvenance::ExternalUser,
            requested_mode,
            content,
            true,
            trust_mode,
            auto_approve,
            approval_mode,
        );

        assert_eq!(policy.mode, requested_mode, "{content}");
        assert_eq!(policy.trust_mode, trust_mode, "{content}");
        assert_eq!(policy.auto_approve, auto_approve, "{content}");
        assert_eq!(policy.approval_mode, approval_mode, "{content}");
        assert!(policy.allow_shell, "{content}");
        assert!(policy.dynamic_active_tools.is_empty(), "{content}");
        assert!(policy.status().is_none(), "{content}");
    }
}

#[test]
fn external_user_wording_does_not_downgrade_standing_authority() {
    let review_wording = effective_input_policy(
        UserInputProvenance::ExternalUser,
        AppMode::Agent,
        "你在帮我看看 外卖部分还哪里没有使用多语言 我看看要不要加",
        true,
        true,
        true,
        ApprovalMode::Bypass,
    );
    assert_eq!(review_wording.mode, AppMode::Agent);
    assert!(review_wording.allow_shell);
    assert!(review_wording.trust_mode);
    assert!(review_wording.auto_approve);
    assert_eq!(review_wording.approval_mode, ApprovalMode::Bypass);
    assert!(
        review_wording.status().is_none(),
        "external user wording must not content-downgrade standing authority"
    );

    let later_user_instruction = effective_input_policy(
        UserInputProvenance::ExternalUser,
        AppMode::Agent,
        "需要修复下",
        true,
        true,
        true,
        ApprovalMode::Bypass,
    );
    assert_eq!(later_user_instruction.mode, AppMode::Agent);
    assert!(later_user_instruction.allow_shell);
    assert!(later_user_instruction.trust_mode);
    assert!(later_user_instruction.auto_approve);
    assert_eq!(later_user_instruction.approval_mode, ApprovalMode::Bypass);
    assert!(
        later_user_instruction.status().is_none(),
        "a fresh external write instruction must not inherit the prior review-only downgrade"
    );
}

#[test]
fn turn_metadata_leaves_mode_entirely_to_runtime_policy() {
    // Mode permissions and capabilities are already concrete in runtime policy
    // and the live tool catalog. Prompt prose must not create a parallel mode.
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    engine.current_mode = AppMode::Plan;

    let user_msg = engine.user_text_message_with_turn_metadata_for_route(
        "explain the refactor plan before editing".to_string(),
        "deepseek-v4-flash",
        false,
        None,
        false,
    );
    let last_block = user_msg.content.last().expect("turn metadata block");
    let ContentBlock::Text { text, .. } = last_block else {
        panic!("expected text metadata block");
    };

    assert!(!text.contains("Current mode:"), "got: {text}");
    assert!(
        !text.contains("Current mode policy"),
        "mode doctrine must not re-enter turn_meta: {text}"
    );
    assert!(
        !text.contains("##### Mode: Plan"),
        "mode overlay text must not re-enter turn_meta: {text}"
    );
    assert!(
        !text.contains("All writes, patches, shell commands,"),
        "mode doctrine must not re-enter turn_meta: {text}"
    );
}

#[test]
fn turn_metadata_projects_permission_posture_as_fact_only() {
    // #4780 + turn-meta diet: the active posture remains an actionable fact.
    // Never adds one actionable constraint so the model cannot waste a turn
    // asking for an approval the host is configured not to provide.
    use ApprovalMode;

    let cases = [
        (ApprovalMode::Suggest, "Ask"),
        (ApprovalMode::Auto, "Auto-Review"),
        (ApprovalMode::Bypass, "Full Access"),
        (ApprovalMode::Never, "Never"),
    ];

    for (approval_mode, posture) in cases {
        let tmp = tempdir().expect("tempdir");
        let config = EngineConfig {
            workspace: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (mut engine, _handle) = Engine::new(config, &Config::default());
        engine.session.approval_mode = approval_mode;

        let message = engine.user_text_message_with_turn_metadata("continue".to_string());
        let ContentBlock::Text { text, .. } = message
            .content
            .last()
            .expect("turn metadata must be present")
        else {
            panic!("expected text turn metadata");
        };

        assert!(
            text.contains(&format!("Current permission posture: {posture}")),
            "{posture}: {text}"
        );
        assert!(
            !text.contains("Current permission policy source"),
            "{posture}: doctrine must not re-enter turn_meta: {text}"
        );
        assert!(
            !text.contains("Current question discipline"),
            "{posture}: question discipline must not re-enter turn_meta: {text}"
        );
        assert_eq!(
            text.contains(
                "Approval prompts are disabled; do not request escalation for this turn."
            ),
            approval_mode == ApprovalMode::Never,
            "{posture}: {text}"
        );
    }
}

#[test]
fn turn_metadata_preserves_standing_full_access_for_subagent_handoff() {
    use ApprovalMode;

    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    let authority = effective_input_policy(
        UserInputProvenance::SubAgentHandoff,
        AppMode::Agent,
        "continue from child",
        true,
        true,
        true,
        ApprovalMode::Bypass,
    );
    engine.apply_runtime_mode_policy(&authority);

    let message = engine.runtime_text_message_with_turn_metadata(
        "continue from child".to_string(),
        UserInputProvenance::SubAgentHandoff,
    );
    let ContentBlock::Text { text, .. } = message
        .content
        .last()
        .expect("turn metadata must be present")
    else {
        panic!("expected text turn metadata");
    };

    // A child handoff cannot grant new authority, but it retains the standing
    // posture and names its reduced provenance in one condensed line.
    assert!(!text.contains("Current mode:"), "{text}");
    assert!(
        text.contains("Current permission posture: Full Access"),
        "{text}"
    );
    assert!(
        text.contains("Input provenance: subagent_handoff (non-authoritative)"),
        "{text}"
    );
}

#[test]
fn current_mode_field_assignment_takes_effect_synchronously() {
    // Basic unit-level invariant: the current_mode field mutates as expected.
    // Op::ChangeMode dispatch through the run loop is exercised by the
    // integration test change_mode_op_updates_current_mode_and_emits_status.
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        model: "deepseek-v4-pro".to_string(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    assert_eq!(engine.current_mode, AppMode::Agent);

    engine.current_mode = AppMode::Operate;
    assert_eq!(engine.current_mode, AppMode::Operate);
}

#[test]
fn user_text_message_keeps_current_turn_input_after_turn_metadata() {
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (engine, _handle) = Engine::new(config, &Config::default());

    let user_msg =
        engine.user_text_message_with_turn_metadata("explain the cache metrics".to_string());

    // User text is now at position 0, turn_meta at position 1.
    let first_text = user_msg
        .content
        .iter()
        .find_map(|block| {
            if let ContentBlock::Text { text, .. } = block {
                Some(text.as_str())
            } else {
                None
            }
        })
        .expect("user text block");
    assert_eq!(first_text, "explain the cache metrics");
}

#[test]
fn messages_with_turn_metadata_preserves_stored_messages_for_prefix_cache() {
    let tmp = tempdir().expect("tempdir");
    fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
    fs::write(tmp.path().join("src/lib.rs"), "pub fn sample() {}").expect("write");

    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    engine
        .session
        .working_set
        .observe_user_message("inspect src/lib.rs", tmp.path());

    let first_user = engine.user_text_message_with_turn_metadata("inspect src/lib.rs".to_string());
    engine.session.add_message(first_user.clone());
    let first_request = engine.messages_with_turn_metadata();
    assert_eq!(
        &first_request[..engine.session.messages.len()],
        &engine.session.messages[..]
    );
    assert_eq!(first_request.len(), engine.session.messages.len());
    assert_eq!(first_request.first(), Some(&first_user));
    assert_eq!(
        first_request.last().map(|message| message.role.as_str()),
        Some("user")
    );

    engine.session.add_message(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "I inspected it.".to_string(),
            cache_control: None,
        }],
    });
    engine
        .session
        .working_set
        .observe_user_message("now summarize it", tmp.path());
    let second_user = engine.user_text_message_with_turn_metadata("now summarize it".to_string());
    engine.session.add_message(second_user);

    let second_request = engine.messages_with_turn_metadata();
    assert_eq!(
        &second_request[..engine.session.messages.len()],
        &engine.session.messages[..]
    );
    assert_eq!(second_request.len(), engine.session.messages.len());
    assert_eq!(second_request.first(), Some(&first_user));
    assert_eq!(second_request.last(), engine.session.messages.last());
}

/// v0.8.11 regression: tool-result messages serialize to role="tool" on
/// the wire but are stored as role="user" internally. `<turn_meta>` must
/// be stored only on actual user-text messages. Request-time runtime metadata
/// must not mutate tool-result messages.
#[test]
fn turn_metadata_skips_tool_result_messages() {
    let tmp = tempdir().expect("tempdir");
    fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
    fs::write(tmp.path().join("src/lib.rs"), "pub fn sample() {}").expect("write");

    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    engine
        .session
        .working_set
        .observe_user_message("inspect src/lib.rs", tmp.path());

    // Real user message — should be eligible for injection.
    let user_msg = engine.user_text_message_with_turn_metadata("inspect src/lib.rs".to_string());
    engine.session.add_message(user_msg);
    // Assistant tool-call.
    engine.session.add_message(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            execution_id: None,
            id: "call_42".to_string(),
            name: "read_file".to_string(),
            input: serde_json::json!({"path": "src/lib.rs"}),
            caller: None,
            thought_signature: None,
        }],
    });
    // Tool result, stored as role="user" internally.
    engine.session.add_message(Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            execution_id: None,
            tool_use_id: "call_42".to_string(),
            content: "pub fn sample() {}".to_string(),
            is_error: None,
            content_blocks: None,
        }],
    });

    let messages = engine.messages_with_turn_metadata();

    // The stored trailing message is the tool result and MUST be untouched —
    // no Text block sneaking in front of the ToolResult block.
    let trailing = messages.last().expect("stored trailing message");
    assert_eq!(trailing.role, "user");
    assert_eq!(trailing.content.len(), 1);
    assert!(matches!(
        trailing.content.first(),
        Some(ContentBlock::ToolResult { .. })
    ));

    // The earlier real user message carries user text first, turn_meta last.
    let real_user = messages.first().expect("first user message");
    assert_eq!(real_user.role, "user");
    let ContentBlock::Text { text, .. } = real_user.content.first().expect("user text content")
    else {
        panic!("expected Text block on real user message");
    };
    assert_eq!(text, "inspect src/lib.rs");
    // turn_meta is at the tail of the content array.
    let last_block = real_user.content.last().expect("turn_meta block");
    let ContentBlock::Text { text: meta, .. } = last_block else {
        panic!("expected Text block for turn_meta at tail");
    };
    assert!(meta.starts_with("<turn_meta>\n"));
    assert!(meta.contains("src/lib.rs"));
}

/// User text must appear before turn_meta in the content array so that
/// the leading bytes of each user message stay stable across date changes.
/// DeepSeek's KV prefix cache matches byte sequences from the start of
/// each message; placing the volatile date-bearing turn_meta at position
/// 0 would invalidate the entire user message prefix at every date
/// boundary. Moving it to the tail preserves the user-input prefix.
#[test]
fn user_message_turn_meta_is_appended_not_prepended() {
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (engine, _handle) = Engine::new(config, &Config::default());

    let msg = engine.user_text_message_with_turn_metadata("hello world".to_string());
    assert_eq!(msg.role, "user");
    assert_eq!(msg.content.len(), 2);

    // First content block: user text.
    let ContentBlock::Text { text, .. } = &msg.content[0] else {
        panic!("expected Text block at position 0");
    };
    assert_eq!(text, "hello world");

    // Second content block: turn_meta.
    let ContentBlock::Text { text: meta, .. } = &msg.content[1] else {
        panic!("expected Text block for turn_meta at position 1");
    };
    assert!(
        meta.starts_with("<turn_meta>\n"),
        "turn_meta must be at the tail"
    );
    assert!(
        meta.contains("Current local date:"),
        "turn_meta must contain the date"
    );
}

/// When the turn is mid-execution and the trailing user message is a
/// tool result, no turn_meta is injected into that tool-result message. The
/// working_set surfaces again on the next stored user-text message.
#[test]
fn turn_metadata_skips_when_only_tool_results_trail() {
    let tmp = tempdir().expect("tempdir");
    fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
    fs::write(tmp.path().join("src/lib.rs"), "pub fn sample() {}").expect("write");

    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    engine
        .session
        .working_set
        .observe_user_message("inspect src/lib.rs", tmp.path());

    // Only a tool-result message in history — simulates the corner case
    // where the prior real user message has already been compacted away
    // but a tool-result is still pending. We must not retroactively
    // inject.
    engine.session.add_message(Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            execution_id: None,
            tool_use_id: "call_42".to_string(),
            content: "pub fn sample() {}".to_string(),
            is_error: None,
            content_blocks: None,
        }],
    });

    let messages = engine.messages_with_turn_metadata();

    // Stored tool-result message is unchanged: no Text prefix, content length == 1.
    let only = messages.first().expect("stored tool result message");
    assert_eq!(only.content.len(), 1);
    assert!(matches!(
        only.content.first(),
        Some(ContentBlock::ToolResult { .. })
    ));
    assert_eq!(messages.len(), 1);
}

#[test]
fn declared_refresh_sets_pending_prefix_change_reason() {
    let _lock = lock_test_env();
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    // Construction pins the initial prompt; clear any construction-time flag.
    engine.session.pending_prefix_change_reason = None;

    // A no-op refresh (unchanged bytes) declares nothing.
    engine.refresh_system_prompt_with_reason("system");
    assert_eq!(engine.session.pending_prefix_change_reason, None);

    // A refresh that actually changes the bytes records the declared reason.
    engine.config.goal_objective = Some("ship the release".to_string());
    engine.config.goal_status = crate::tools::goal::GoalStatus::Active;
    engine.refresh_system_prompt_with_reason("goal");
    assert_eq!(
        engine.session.pending_prefix_change_reason.as_deref(),
        Some("goal")
    );
}

#[test]
fn workspace_file_change_never_moves_the_frozen_prefix() {
    // The old bug: the tool loop recomposed the system prompt from disk on
    // every step, so an agent writing a file changed the project pack and
    // busted DeepSeek's KV prefix cache mid-turn. The header is now frozen
    // for the session: only an explicit refresh (a declared header change)
    // recomposes it, and the tool loop no longer calls one.
    let _lock = lock_test_env();
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        project_context_pack_enabled: true,
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    let frozen_prompt = engine.session.system_prompt.clone();
    engine.session.pending_prefix_change_reason = None;

    // Simulate the agent writing a file into the workspace mid-turn.
    fs::write(tmp.path().join("NEWFILE.md"), "brand new content").expect("write");

    // What a fresh compose WOULD produce now differs — the bug precondition.
    let recomposed =
        engine.compose_stable_system_prompt(&engine.installed_next_turn_prompt_context());
    assert_ne!(
        recomposed, frozen_prompt,
        "a workspace file change must change what a fresh compose would produce"
    );

    // But the session's pinned prompt is untouched and nothing was declared,
    // because the tool loop performs no mid-loop refresh.
    assert_eq!(engine.session.system_prompt, frozen_prompt);
    assert_eq!(engine.session.pending_prefix_change_reason, None);
}