//! Scripted-provider fixture for the one context-pressure number (0.10.1
//! item 9).
//!
//! A tool-heavy turn crosses the auto-compaction threshold mid-turn, compacts,
//! then the next turn switches route and endpoint and continues. At every
//! model request the engine actually sent, the context meter, the
//! auto-compaction gate, the compaction preflight, and the `/context` headline
//! must read the same number — and it must be the pressure estimate, not the
//! 1.5x-inflated overflow guard, which `/context` shows only as a labeled
//! secondary line.

use std::time::Duration;

use codewhale_models::MessageRequest;
use serde_json::json;
use tempfile::tempdir;

use super::{build_context_report, format_context_report, format_context_summary};
use crate::compaction::{
    CompactionConfig, compaction_pressure_reached_with_billed, estimate_input_tokens_conservative,
    estimate_input_tokens_for_pressure,
};
use crate::core::engine::{Engine, EngineConfig};
use crate::core::events::{Event, TurnOutcomeStatus};
use crate::core::ops::{Op, TurnSpec, UserInputProvenance};
use crate::llm_client::mock::{MockLlmClient, canned};
use crate::route_runtime::{ResolvedRuntimeRoute, resolve_runtime_route};
use crate::test_support::{EnvVarGuard, lock_test_env};
use crate::tui::app::App;
use codewhale_runtime::config::Config;

const THRESHOLD: usize = 40_000;
const PRIVATE_BASE_URL: &str = "https://private-fixture.test/v1";
const PRIVATE_MODEL: &str = "private-fixture-deployment";
const PRIVATE_WINDOW: u32 = 200_000;

fn private_route_config() -> Config {
    {
        let __cw_config_init_0_0 = Some("custom".to_string());
        let __cw_config_init_0_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom: std::collections::HashMap::from([(
                "custom".to_string(),
                codewhale_runtime::config::ProviderConfig {
                    kind: Some("openai-compatible".to_string()),
                    api_key: Some("test-private-key".to_string()),
                    base_url: Some(PRIVATE_BASE_URL.to_string()),
                    model: Some(PRIVATE_MODEL.to_string()),
                    context_window: Some(PRIVATE_WINDOW),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        });
        let mut __cw_config_init_0_value: Config = Default::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value.providers = __cw_config_init_0_1;
        __cw_config_init_0_value
    }
}

fn resolve(config: &Config, model: &str) -> ResolvedRuntimeRoute {
    resolve_runtime_route(
        config,
        config.active_provider_identity().unwrap().provider,
        Some(model),
    )
    .expect("resolve route")
}

fn fixture_compaction() -> CompactionConfig {
    CompactionConfig {
        token_threshold: THRESHOLD,
        ..CompactionConfig::default()
    }
}

fn turn_op(content: &str, route: &ResolvedRuntimeRoute) -> Op {
    let compaction = fixture_compaction();
    Op::SendMessage(TurnSpec {
        profile_constitution: None,
        max_output_tokens: None,
        content: content.to_string(),
        images: Vec::new(),
        mode: codewhale_config::AppMode::Agent,
        route: Box::new(route.clone()),
        compaction: Box::new(compaction),
        initial_routed_usage: Box::default(),
        goal_objective: None,
        goal_token_budget: None,
        goal_status: crate::tools::goal::GoalStatus::Active,
        reasoning_effort: None,
        reasoning_effort_auto: false,
        auto_model: false,
        allow_shell: true,
        trust_mode: false,
        auto_approve: true,
        approval_mode: codewhale_execpolicy::ApprovalMode::Suggest,
        translation_enabled: false,
        allowed_tools: None,
        dynamic_tools: Vec::new(),
        hook_executor: None,
        verbosity: None,
        provenance: UserInputProvenance::ExternalUser,
        submission_id: None,
    })
}

fn tool_step(step: usize, payload_chars: usize) -> Vec<codewhale_models::StreamEvent> {
    vec![
        canned::message_start(&format!("response-{step}")),
        canned::text_block_start(0),
        canned::text_delta(0, &format!("Step {step}: {}", "x".repeat(payload_chars))),
        canned::block_stop(0),
        canned::tool_use_block_start(1, &format!("read-{step}"), "File"),
        canned::tool_input_delta(1, r#"{"action":"read","path":"README.md"}"#),
        canned::block_stop(1),
        canned::message_delta("tool_use", None),
        canned::message_stop(),
    ]
}

#[derive(Debug, Default)]
struct TurnEvents {
    auto_compactions: usize,
    receipts: Vec<(String, Option<u64>)>,
}

async fn run_turn(handle: &crate::core::engine::EngineHandle, op: Op) -> TurnEvents {
    handle.send(op).await.expect("send turn");
    let mut events = TurnEvents::default();
    let mut rx = handle.rx_event.write().await;
    loop {
        match tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("engine event before timeout")
            .expect("engine event channel open")
        {
            Event::CompactionCompleted {
                auto: true,
                message,
                post_input_tokens,
                ..
            } => {
                events.auto_compactions += 1;
                events.receipts.push((message, post_input_tokens));
            }
            Event::CompactionFailed { message, .. } => {
                panic!("fixture compaction must succeed: {message}")
            }
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                return events;
            }
            _ => {}
        }
    }
}

/// Mirror the engine's installed route and the exact request it sent into a
/// TUI `App`, then read every surface. Returns the one agreed number.
fn assert_one_pressure_number(
    app: &mut App,
    label: &str,
    request: &MessageRequest,
    route: &ResolvedRuntimeRoute,
) -> usize {
    app.api_provider = route.identity.provider;
    app.model = route.model.clone();
    app.active_route_limits = crate::route_budget::known_route_limits(route.candidate.limits());
    app.active_context_window_source = route.context_window.source;
    // Install the transcript the way the host applies an engine session
    // projection (`apply_engine_session_projection`): the meter's per-message
    // cache is dropped before the rewritten history lands, so a compaction
    // cannot leave stale per-index counts behind.
    app.context_token_cache.borrow_mut().clear();
    app.set_api_messages(std::sync::Arc::new(request.messages.clone()));
    app.system_prompt = request.system.clone();
    // Mock usage bills no prompt tokens; the number is the estimate alone.
    app.last_billed_input_tokens = None;

    let messages = &request.messages;
    let system = request.system.as_ref();

    // Auto-compaction gate.
    let gate = estimate_input_tokens_for_pressure(messages, system);
    let compaction = fixture_compaction();
    assert_eq!(
        compaction_pressure_reached_with_billed(messages, system, &compaction, None),
        gate >= THRESHOLD,
        "{label}: gate decision must follow the gate number {gate}"
    );

    // Compaction preflight (live input the turn loop measures).
    let preflight = crate::core::turn::TurnContext::new(8)
        .live_input_tokens_for_compaction(messages, system, None)
        .expect("non-empty request");

    // Context meter (footer).
    let (meter, meter_window, meter_percent) =
        crate::tui::ui::context_usage_snapshot(app).expect("meter reading");

    // `/context` headline.
    let report = build_context_report(app);

    assert_eq!(preflight, gate as u64, "{label}: preflight vs gate");
    assert_eq!(meter, gate as i64, "{label}: meter vs gate");
    assert_eq!(
        report.active_context_estimated_tokens, gate,
        "{label}: /context headline vs gate"
    );
    assert_eq!(
        report.context_window_tokens,
        Some(meter_window),
        "{label}: /context window vs meter window"
    );
    let report_percent = report.budget_used_percent.expect("window known");
    assert!(
        (report_percent - meter_percent).abs() < 1e-9,
        "{label}: /context {report_percent}% vs meter {meter_percent}%"
    );

    // The inflated figure is only the labeled secondary overflow-guard line.
    let guard = estimate_input_tokens_conservative(messages, system);
    assert!(
        guard > gate,
        "{label}: fixture must separate the estimators"
    );
    assert_eq!(report.overflow_guard_estimated_tokens, Some(guard));
    for text in [
        format_context_report(&report),
        format_context_summary(&report),
    ] {
        assert!(
            text.contains(&format!("Estimated active context: {gate} tokens")),
            "{label}: {text}"
        );
        assert!(
            text.contains(&format!("Overflow guard: {guard} tokens")),
            "{label}: {text}"
        );
    }

    // Once the provider bills a prompt above the local estimate, every
    // surface lifts to the bill together — the footer meter included.
    let billed = gate + 7_000;
    app.last_billed_input_tokens = Some(u32::try_from(billed).expect("fixture bill"));
    let billed_u64 = billed as u64;
    assert_eq!(
        compaction_pressure_reached_with_billed(messages, system, &compaction, Some(billed_u64)),
        billed >= THRESHOLD,
        "{label}: billed gate decision"
    );
    let billed_preflight = crate::core::turn::TurnContext::new(8)
        .live_input_tokens_for_compaction(
            messages,
            system,
            Some(u32::try_from(billed).expect("fixture bill")),
        )
        .expect("non-empty request");
    let (billed_meter, _, _) = crate::tui::ui::context_usage_snapshot(app).expect("meter reading");
    let billed_report = build_context_report(app);
    assert_eq!(billed_preflight, billed_u64, "{label}: billed preflight");
    assert_eq!(billed_meter, billed as i64, "{label}: billed meter");
    assert_eq!(
        billed_report.active_context_estimated_tokens, billed,
        "{label}: billed /context headline"
    );
    app.last_billed_input_tokens = None;
    gate
}

/// The `~before → ~after tokens` pair a compaction receipt prints.
fn receipt_token_pair(message: &str) -> (usize, usize) {
    let (_, tail) = message.split_once("), ~").expect("receipt token clause");
    let (before, rest) = tail.split_once(" → ~").expect("receipt arrow");
    let (after, _) = rest.split_once(" tokens").expect("receipt tokens");
    (
        before.parse().expect("before tokens"),
        after.parse().expect("after tokens"),
    )
}

fn streaming(requests: &[MessageRequest]) -> Vec<MessageRequest> {
    requests
        .iter()
        .filter(|request| request.stream == Some(true))
        .cloned()
        .collect()
}

#[test]
fn one_pressure_number_across_mid_turn_compaction_and_route_switch() {
    let _env = lock_test_env();
    let home = tempdir().expect("home");
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let _user_home = EnvVarGuard::set("HOME", home.path());
    let _user_profile = EnvVarGuard::set("USERPROFILE", home.path());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let workspace = tempdir().expect("workspace");
        std::fs::write(workspace.path().join("README.md"), "verified fixture evidence")
            .expect("write fixture");

        let default_config = Config::default();
        let route_a = resolve(&default_config, codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL);
        let private_config = private_route_config();
        let route_b = resolve(&private_config, PRIVATE_MODEL);
        assert_ne!(
            route_a.candidate.endpoint().base_url,
            route_b.candidate.endpoint().base_url,
            "the second turn must switch endpoint"
        );
        assert_ne!(route_a.model, route_b.model, "and route");

        // Turn 1: tool-heavy, crosses the threshold mid-turn.
        let mock = std::sync::Arc::new(MockLlmClient::new(Vec::new()));
        for step in 0..8 {
            mock.push_turn(tool_step(step, 32_000));
        }
        mock.push_turn(canned::simple_text_turn("All reads verified on route A."));
        // Turn 2: continues on the new route and endpoint.
        mock.push_turn(tool_step(100, 400));
        mock.push_turn(canned::simple_text_turn("Continued on route B."));
        for checkpoint in 0..6 {
            mock.push_message_response(
                serde_json::from_value(json!({
                    "id": format!("summary-{checkpoint}"),
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "text", "text": format!(
                        "Current objective: finish the README reads. Checkpoint {checkpoint}: earlier reads verified; continue the remaining reads, then report."
                    )}],
                    "model": "mock-model",
                    "usage": {"input_tokens": 0, "output_tokens": 0}
                }))
                .expect("summary response"),
            );
        }

        let engine_config = EngineConfig {
            workspace: workspace.path().to_path_buf(),
            snapshots_enabled: false,
            subagents_enabled: false,
            ..EngineConfig::default()
        };
        let (engine, handle) =
            Engine::new_with_model_client(engine_config, &default_config, mock.clone());
        let task = tokio::spawn(engine.run());

        let turn_one = run_turn(
            &handle,
            turn_op("Read README.md repeatedly and verify it.", &route_a),
        )
        .await;
        let after_turn_one = mock.captured_requests().len();
        let turn_two = run_turn(&handle, turn_op("Continue on the new route.", &route_b)).await;
        handle.send(Op::Shutdown).await.expect("shutdown");
        task.await.expect("engine task");

        let requests = mock.captured_requests();
        let turn_one_requests = streaming(&requests[..after_turn_one]);
        let turn_two_requests = streaming(&requests[after_turn_one..]);
        assert_eq!(turn_one_requests.len(), 9, "one request per scripted step");
        assert_eq!(turn_two_requests.len(), 2, "turn two continues");
        assert!(
            turn_one.auto_compactions >= 1,
            "turn one must compact mid-turn: {turn_one:?}"
        );
        assert_eq!(
            turn_two.auto_compactions, 0,
            "turn two stays under the threshold: {turn_two:?}"
        );
        for request in &turn_two_requests {
            assert_eq!(request.model, PRIVATE_MODEL, "turn two uses route B");
        }

        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(workspace.path()),
        );
        let mut readings = Vec::new();
        for (index, request) in turn_one_requests.iter().enumerate() {
            readings.push(assert_one_pressure_number(
                &mut app,
                &format!("turn 1 request {index}"),
                request,
                &route_a,
            ));
        }
        // The compaction happened mid-turn: the transcript shrank between two
        // requests of the same turn, and the pressure number fell with it.
        let shrink = turn_one_requests
            .windows(2)
            .position(|pair| pair[1].messages.len() < pair[0].messages.len())
            .expect("a mid-turn compaction shrinks the next request");
        assert!(
            readings[shrink + 1] < readings[shrink],
            "pressure falls across the compaction: {readings:?}"
        );
        assert!(
            readings[..=shrink].iter().any(|tokens| *tokens + 16_000 >= THRESHOLD),
            "the turn approached the threshold before compacting: {readings:?}"
        );

        let (_, window_a, _) = crate::tui::ui::context_usage_snapshot(&app).expect("meter");
        for (index, request) in turn_two_requests.iter().enumerate() {
            assert_one_pressure_number(
                &mut app,
                &format!("turn 2 request {index}"),
                request,
                &route_b,
            );
        }
        let (_, window_b, _) = crate::tui::ui::context_usage_snapshot(&app).expect("meter");
        assert_eq!(window_b, PRIVATE_WINDOW, "meter follows the switched route");
        assert_ne!(window_a, window_b, "the route switch changes the window");

        // Compaction receipts report the same pressure number: the printed
        // `after` equals `post_input_tokens`, which is the reading of the
        // first request sent after that compaction. The printed `before`
        // crossed the gate's threshold.
        let shrinks: Vec<usize> = turn_one_requests
            .windows(2)
            .enumerate()
            .filter(|(_, pair)| pair[1].messages.len() < pair[0].messages.len())
            .map(|(index, _)| index + 1)
            .collect();
        assert_eq!(
            shrinks.len(),
            turn_one.receipts.len(),
            "one shrink per receipt: {:?}",
            turn_one.receipts
        );
        for ((message, post_input_tokens), next) in turn_one.receipts.iter().zip(&shrinks) {
            let (before, after) = receipt_token_pair(message);
            assert_eq!(Some(after as u64), *post_input_tokens, "{message}");
            assert_eq!(after, readings[*next], "receipt vs next request: {message}");
            assert!(before >= THRESHOLD, "receipt before crossed the gate: {message}");
        }
    });
}
