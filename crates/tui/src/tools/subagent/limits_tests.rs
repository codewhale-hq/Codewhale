use super::tests::{make_snapshot, make_worker_spec, stub_runtime};
use super::*;
use tempfile::tempdir;

#[test]
fn depth_one_child_cannot_spawn_a_grandchild_even_with_a_wider_profile() {
    let root = stub_runtime().with_max_spawn_depth(1);
    let mut child = root.child_runtime();
    child.worker_profile = worker_profile_for_spawn(
        &child,
        &FleetRole::Worker,
        &AgentWorkerToolProfile::Inherited,
        "deepseek-v4-flash",
        None,
        false,
    );
    assert_eq!(child.spawn_depth, 1);
    assert_eq!(child.max_spawn_depth, 1);
    assert_eq!(child.worker_profile.max_spawn_depth, 1);
    assert_eq!(child.worker_profile.spawn_depth, 1);
    assert!(child.would_exceed_depth());
    assert!(!child.worker_profile.can_spawn_child());
    child.worker_profile.max_spawn_depth = u32::MAX;
    assert!(
        child.would_exceed_depth(),
        "a widened projection cannot bypass runtime ceiling"
    );
    assert!(child.background_runtime().would_exceed_depth());
}

#[test]
fn depth_overflow_fails_closed() {
    let mut runtime = stub_runtime();
    runtime.spawn_depth = u32::MAX;
    runtime.max_spawn_depth = u32::MAX;
    assert!(runtime.would_exceed_depth());
    assert_eq!(runtime.child_runtime().spawn_depth, u32::MAX);
}

#[test]
fn old_relative_profile_depth_does_not_gain_authority_on_recovery() {
    let tmp = tempdir().unwrap();
    let mut spec = make_worker_spec("legacy", tmp.path().to_path_buf());
    spec.spawn_depth = 2;
    spec.max_spawn_depth = 3;
    spec.runtime_profile.max_spawn_depth = 1; // old remaining allowance
    let record = AgentWorkerRecord::new(spec, epoch_millis_now());
    let mut encoded = serde_json::to_value(&record).unwrap();
    encoded["spec"]["runtime_profile"]
        .as_object_mut()
        .unwrap()
        .remove("spawn_depth");
    let decoded: AgentWorkerRecord = serde_json::from_value(encoded).unwrap();
    let recovered = normalize_worker_record(decoded);
    assert_eq!(recovered.spec.runtime_profile.spawn_depth, 2);
    assert_eq!(recovered.spec.max_spawn_depth, 1);
    assert_eq!(recovered.spec.runtime_profile.max_spawn_depth, 1);
    assert!(!recovered.spec.runtime_profile.can_spawn_child());
}

#[test]
fn operator_and_inherited_budgets_only_narrow_including_zero_sentinels() {
    assert_eq!(resolve_max_steps(FleetRole::Worker, Some(99), Some(7)), 7);
    assert_eq!(resolve_max_steps(FleetRole::Worker, Some(0), Some(7)), 7);
    let parent = WorkerRuntimeProfile {
        max_steps: 8,
        wall_time_secs: Some(40),
        wall_deadline_ms: Some(123_000),
        ..WorkerRuntimeProfile::default()
    };
    let requested = WorkerRuntimeProfile {
        wall_time_secs: Some(4_000),
        wall_deadline_ms: Some(456_000),
        ..WorkerRuntimeProfile::default()
    };
    let child = parent.derive_child(&requested);
    assert_eq!(child.max_steps, 8);
    assert_eq!(child.wall_time_secs, Some(40));
    assert_eq!(child.wall_deadline_ms, Some(123_000));
}

#[test]
fn child_runtime_budget_context_reports_every_resolved_limit() {
    let mut runtime = stub_runtime();
    runtime.worker_profile.wall_time_secs = Some(1_800);
    runtime.worker_profile.wall_deadline_ms = Some(epoch_millis_now() + 1_700_000);
    let context = child_runtime_budget_context(&runtime, 50);
    assert!(context.contains("Runtime limits:"));
    assert!(context.contains("wall clock: task work stops about"));
    assert!(context.contains("total run budget 30m 00s"));
    assert!(context.contains("50 model turns"));
    assert!(!context.contains("reserved"));
    assert!(context.contains("configured owner limits are enforced"));
    assert!(context.contains("There is no token budget and no per-step context cap"));
}

#[test]
fn child_runtime_budget_context_names_unbounded_limits_honestly() {
    let runtime = stub_runtime();
    let context = child_runtime_budget_context(&runtime, 0);
    assert!(context.contains("wall clock: no wall-clock limit."));
    assert!(context.contains("model steps: no per-run step cap."));
    assert!(context.contains("There is no token budget"));
    assert!(context.contains("Continue until the task is complete"));
    assert!(!context.contains("reserved hand-back turn"));
}

#[test]
fn child_runtime_budget_context_tells_the_child_it_compacts_like_the_parent() {
    let mut runtime = stub_runtime();
    let context = child_runtime_budget_context(&runtime, 0);
    assert!(
        !context.contains("no automatic compaction"),
        "children compact like the parent: {context}"
    );
    assert!(
        context.contains("compacts it automatically") && context.contains("the run continues"),
        "{context}"
    );
    assert!(
        !context.contains("input tokens"),
        "no per-step input cap: {context}"
    );

    // The parent's opt-out is the child's opt-out, and the prompt says so.
    runtime.compaction.enabled = false;
    let context = child_runtime_budget_context(&runtime, 0);
    assert!(
        context.contains("Automatic context compaction is disabled for this session"),
        "{context}"
    );
    assert!(!context.contains("compacts it automatically"), "{context}");
}

#[test]
fn child_compaction_config_inherits_the_parent_policy_for_the_child_route() {
    let mut runtime = stub_runtime();
    let route = runtime
        .client
        .effective_route_envelope(&runtime.model, chrono::Utc::now());
    let window = crate::route_budget::route_context_window_tokens(
        route.provider,
        &route.model,
        runtime.client.route_limits(),
    );
    runtime.compaction = crate::compaction::CompactionConfig {
        enabled: false,
        token_threshold: 12_345,
        effective_context_window: Some(window),
        summary_instructions: Some("keep file paths".to_string()),
        retained_user_message_tokens: 777,
        focus: Some("manual focus never leaks into auto passes".to_string()),
        ..crate::compaction::CompactionConfig::default()
    };

    let config = child_compaction_config(
        &runtime,
        &route,
        crate::model_profile::SupportState::Supported,
    );
    assert!(!config.enabled, "the parent's opt-out holds for children");
    assert_eq!(config.token_threshold, 12_345, "same window, same trigger");
    assert_eq!(config.effective_context_window, Some(window));
    assert_eq!(config.model, runtime.model);
    assert_eq!(
        config.image_input,
        crate::model_profile::SupportState::Supported
    );
    assert_eq!(
        config.summary_instructions.as_deref(),
        Some("keep file paths")
    );
    assert_eq!(config.retained_user_message_tokens, 777);
    assert_eq!(config.focus, None);
    assert_eq!(config.workspace, Some(runtime.context.workspace.clone()));

    // A parent on a different window carries its trigger as the same
    // fraction of the child's window, through the shared route helper.
    runtime.compaction.enabled = true;
    runtime.compaction.effective_context_window = Some(window.saturating_mul(2));
    runtime.compaction.token_threshold = usize::try_from(window).unwrap();
    let config = child_compaction_config(
        &runtime,
        &route,
        crate::model_profile::SupportState::Unknown,
    );
    assert!(config.enabled);
    assert_eq!(
        config.token_threshold,
        crate::route_budget::compaction_threshold_for_route_at_percent(
            route.provider,
            &route.model,
            runtime.client.route_limits(),
            50.0,
        )
    );
}

#[test]
fn per_call_budget_fields_reject_empty_zero_null_negative_and_oversized_values() {
    for field in ["max_steps", "wall_time_secs"] {
        for invalid in [
            json!(0),
            json!(-1),
            json!(null),
            json!(""),
            json!("7"),
            json!(false),
            json!(1.5),
        ] {
            let mut input = json!({"prompt": "inspect"});
            input[field] = invalid;
            assert!(parse_spawn_request(&input).is_err(), "{input}");
        }
    }
    for (field, value) in [
        ("max_steps", u64::from(u32::MAX) + 1),
        ("wall_time_secs", u64::MAX / 1_000 + 1),
    ] {
        let mut input = json!({"prompt": "inspect"});
        input[field] = json!(value);
        assert!(parse_spawn_request(&input).is_err(), "{input}");
    }
}

#[test]
fn budget_partial_handback_is_bounded_and_keeps_unknown_usage_honest() {
    let mut snapshot = make_snapshot(SubAgentStatus::Running);
    snapshot.result = Some("partial 🐳 ".repeat(2_000));
    let result = budget_partial_result(snapshot, "child wall-time budget exhausted", None);
    assert_eq!(result.status, SubAgentStatus::BudgetExhausted);
    let summary = result.result.unwrap();
    assert!(summary.chars().count() < 4_500);
    assert!(summary.contains("usage has not been reported"));
    assert!(summary.contains("makes no further model request"));
    let checkpoint = result.checkpoint.unwrap();
    assert!(!checkpoint.continuable);
    assert_eq!(
        subagent_failure_class(&result.status, &checkpoint.reason),
        "wall_time_budget"
    );
}

#[tokio::test]
async fn launch_narrows_all_limits_and_continuation_cannot_restart_deadline() {
    let tmp = tempdir().unwrap();
    let manager = Arc::new(RwLock::new(
        SubAgentManager::new(tmp.path().to_path_buf(), 4)
            .with_default_max_steps(Some(4))
            .with_default_wall_time(Some(Duration::from_secs(10))),
    ));
    let mut runtime = stub_runtime().child_runtime();
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    runtime.manager = Arc::clone(&manager);
    runtime.cancel_token.cancel(); // inspect admission; no provider request may run
    let options = SubAgentSpawnOptions {
        max_steps: Some(999),
        wall_time: Some(Duration::from_secs(999)),
        ..Default::default()
    };
    let mut guard = manager.write().await;
    let child = guard
        .spawn_background_with_assignment_options(
            Arc::clone(&manager),
            runtime.clone(),
            FleetRole::Scout,
            "inspect".to_string(),
            SubAgentAssignment::new("inspect".to_string(), None),
            Some(vec![]),
            options,
            None,
        )
        .unwrap();
    let profile = &guard.worker_records[&child.agent_id].spec.runtime_profile;
    assert_eq!(profile.max_steps, 4);
    assert!(profile.wall_time_secs.unwrap() <= 10);
    // Continuation admission requires the same actor to settle before its saved deadline is checked.
    let requested = guard.cancel_agent(&child.agent_id).unwrap();
    drop(guard);
    let settled = settle_requested_child(&manager, requested).await;
    assert_eq!(settled.status, SubAgentStatus::Cancelled);
    let mut guard = manager.write().await;
    guard
        .worker_records
        .get_mut(&child.agent_id)
        .unwrap()
        .spec
        .runtime_profile
        .wall_deadline_ms = Some(1);
    let refused = guard.spawn_background_with_assignment_options(
        Arc::clone(&manager),
        runtime,
        FleetRole::Scout,
        "continue".to_string(),
        SubAgentAssignment::new("continue".to_string(), None),
        Some(vec![]),
        SubAgentSpawnOptions {
            resume_from_agent_id: Some(child.agent_id),
            ..Default::default()
        },
        None,
    );
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("cannot reset its deadline")
    );
}

#[tokio::test]
async fn child_wall_time_is_unbounded_unless_an_owner_sets_it() {
    let tmp = tempdir().unwrap();
    let manager = Arc::new(RwLock::new(SubAgentManager::new(
        tmp.path().to_path_buf(),
        4,
    )));
    let mut runtime = stub_runtime().child_runtime();
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    runtime.manager = Arc::clone(&manager);
    runtime.worker_profile.wall_time_secs = None;
    runtime.cancel_token.cancel(); // inspect admission; no provider request may run
    let spawn = |guard: &mut SubAgentManager, wall_time: Option<Duration>| {
        let child = guard
            .spawn_background_with_assignment_options(
                Arc::clone(&manager),
                runtime.clone(),
                FleetRole::Scout,
                "inspect".to_string(),
                SubAgentAssignment::new("inspect".to_string(), None),
                Some(vec![]),
                SubAgentSpawnOptions {
                    wall_time,
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        guard.worker_records[&child.agent_id]
            .spec
            .runtime_profile
            .clone()
    };
    let mut guard = manager.write().await;
    let unbounded = spawn(&mut guard, None);
    assert_eq!(unbounded.wall_time_secs, None);
    assert_eq!(unbounded.wall_deadline_ms, None);
    assert_eq!(
        spawn(&mut guard, Some(Duration::from_secs(48 * 60 * 60))).wall_time_secs,
        Some(48 * 60 * 60),
        "an owner-selected limit is not shortened by a built-in ceiling"
    );
    drop(guard);
    for owner_steps in [0, 7] {
        runtime.worker_profile.max_steps = owner_steps;
        let result = AgentTool::new(Arc::clone(&manager), runtime.clone())
            .execute(
                json!({
                    "action": "start",
                    "type": "explore",
                    "prompt": "inspect",
                    "max_steps": 1,
                    "wall_time_secs": 1,
                    "max_output_tokens": 1,
                    "max_depth": 0
                }),
                &runtime.context,
            )
            .await
            .expect("legacy model caps cannot restrict the child");
        let receipt: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(receipt["effective_limits"]["max_steps"], owner_steps);
        assert!(receipt["effective_limits"]["wall_deadline_ms"].is_null());
        assert_eq!(receipt["ignored_limit_fields"].as_array().unwrap().len(), 4);
        let guard = manager.read().await;
        let record = &guard.worker_records[receipt["agent_id"].as_str().unwrap()];
        assert_eq!(record.spec.max_spawn_depth, runtime.max_spawn_depth);
    }
}

#[tokio::test]
async fn resume_intersects_saved_write_shell_and_tool_permissions_with_current_caller() {
    let tmp = tempdir().unwrap();
    let manager = Arc::new(RwLock::new(SubAgentManager::new(
        tmp.path().to_path_buf(),
        4,
    )));
    let mut runtime = stub_runtime().child_runtime();
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    runtime.manager = Arc::clone(&manager);
    runtime.cancel_token.cancel();
    runtime.worker_profile.permissions.write = false;
    runtime.worker_profile.permissions.network = false;
    runtime.worker_profile.shell = ShellPolicy::ReadOnly;
    runtime.worker_profile.tools = ToolScope::Explicit(vec!["read_file".to_string()]);
    runtime.worker_profile.denied_tools = vec!["exec_shell".to_string()];
    let saved = WorkerRuntimeProfile::for_role(FleetRole::Worker);
    let mut guard = manager.write().await;
    let child = guard
        .spawn_background_with_assignment_options(
            Arc::clone(&manager),
            runtime,
            FleetRole::Worker,
            "resume".to_string(),
            SubAgentAssignment::new("resume".to_string(), None),
            None,
            SubAgentSpawnOptions {
                preserve_runtime_profile: Some(saved),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    let profile = &guard.worker_records[&child.agent_id].spec.runtime_profile;
    assert!(!profile.permissions.write);
    assert!(!profile.permissions.network);
    assert_eq!(profile.shell, ShellPolicy::ReadOnly);
    assert_eq!(
        profile.tools,
        ToolScope::Explicit(vec!["read_file".to_string()])
    );
    assert!(profile.denied_tools.contains(&"exec_shell".to_string()));
}

#[tokio::test]
async fn root_fork_of_depth_two_leaf_cannot_regain_a_generation() {
    let tmp = tempdir().unwrap();
    let manager = Arc::new(RwLock::new(SubAgentManager::new(
        tmp.path().to_path_buf(),
        4,
    )));
    let mut runtime = stub_runtime().with_max_spawn_depth(3).child_runtime();
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    runtime.manager = Arc::clone(&manager);
    runtime.cancel_token.cancel();
    let mut guard = manager.write().await;
    let (canonical, directory) =
        codewhale_util::workspace_dir::open_workspace_directory(tmp.path()).unwrap();
    guard
        .admit_coordination_workspace(tmp.path().to_path_buf(), canonical, Arc::new(directory))
        .unwrap();
    let source_id = guard.insert_test_running_direct_child("leaf", tmp.path());
    guard.cancel_agent(&source_id).unwrap();
    let mut source = make_worker_spec(&source_id, tmp.path().to_path_buf());
    source.spawn_depth = 2;
    source.max_spawn_depth = 2;
    source.runtime_profile.spawn_depth = 2;
    source.runtime_profile.max_spawn_depth = 2;
    // Preserve the existing actor/origin/session receipt while installing the depth-2 profile.
    guard.worker_records.get_mut(&source_id).unwrap().spec = source;
    let child = guard
        .spawn_background_with_assignment_options(
            Arc::clone(&manager),
            runtime,
            FleetRole::Scout,
            "fork leaf".to_string(),
            SubAgentAssignment::new("fork leaf".to_string(), None),
            Some(vec![]),
            SubAgentSpawnOptions {
                resume_from_agent_id: Some(source_id),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    let spec = &guard.worker_records[&child.agent_id].spec;
    assert_eq!(spec.spawn_depth, 2);
    assert_eq!(spec.max_spawn_depth, 2);
    assert_eq!(spec.runtime_profile.spawn_depth, 2);
    assert!(!spec.runtime_profile.can_spawn_child());
}

// ── #6282 tool-result cap tests ───────────────────────────────────────────

fn cap_tokens(n: u32) -> std::num::NonZeroU32 {
    std::num::NonZeroU32::new(n).expect("n > 0")
}

#[test]
fn hard_cap_passes_through_content_below_both_caps() {
    let content = "small".to_string();
    let capped = hard_cap_tool_result(content.clone(), cap_tokens(10_000));
    assert_eq!(capped, content);
    assert!(!capped.contains("truncated"));
}

#[test]
fn hard_cap_truncates_content_above_byte_cap_and_stamps_truncated_marker() {
    let content = "X".repeat(1_048_577); // 1 byte over the 1 MiB cap
    let capped = hard_cap_tool_result(content, cap_tokens(u32::MAX));
    assert!(capped.len() <= 1_048_576 + "\n[truncated: true]".len());
    assert!(capped.ends_with("\n[truncated: true]"));
    assert!(capped.starts_with('X'));
}

#[test]
fn hard_cap_truncates_content_above_token_cap() {
    // 10k tokens × 3 bytes/token = 30k byte cap. 100k chars should trigger it.
    let content = "A".repeat(100_000);
    let capped = hard_cap_tool_result(content, cap_tokens(10_000));
    // The effective cap is min(30k bytes, 1 MiB) = 30k bytes.
    let token_cap_bytes = 10_000usize.saturating_mul(3);
    assert!(capped.len() <= token_cap_bytes + "\n[truncated: true]".len());
    assert!(capped.ends_with("\n[truncated: true]"));
}

#[test]
fn hard_cap_truncates_at_valid_utf8_boundary() {
    // Build content where 1 MiB boundary falls mid-char.
    // '好' is 3 bytes in UTF-8.
    let mut content = String::new();
    let target = 1_048_576; // exactly at boundary
    while content.len() < target + 2 {
        content.push('好');
    }
    assert!(content.len() > target);
    let capped = hard_cap_tool_result(content, cap_tokens(u32::MAX));
    // Must be valid UTF-8 (no panic during slicing or display).
    assert!(capped.ends_with("\n[truncated: true]"));
    // The marker is ASCII; verify the rest is still valid UTF-8.
    let without_marker = &capped[..capped.len() - "\n[truncated: true]".len()];
    assert!(std::str::from_utf8(without_marker.as_bytes()).is_ok());
}

#[test]
fn hard_cap_respects_custom_max_output_tokens_nonzero() {
    let content = "X".repeat(10_000);
    // 100 tokens × 3 bytes = 300 byte cap — much tighter than default.
    let capped = hard_cap_tool_result(content.clone(), cap_tokens(100));
    assert!(capped.len() <= 300 + "\n[truncated: true]".len());
    assert!(capped.ends_with("\n[truncated: true]"));
}

#[test]
fn hard_cap_min_token_value_produces_three_byte_cap() {
    // NonZeroU32::MIN = 1 token → cap at 3 bytes.
    let content = "hello world".to_string();
    let capped = hard_cap_tool_result(content, std::num::NonZeroU32::MIN);
    assert!(capped.len() <= 3 + "\n[truncated: true]".len());
    assert!(capped.ends_with("\n[truncated: true]"));
}
