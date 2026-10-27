//! Golden-events family: one full turn through the one turn loop.
//!
//! Each case drives `Engine::run` (and so `Engine::run_turn`) with a scripted
//! provider — a queue of normalized stream events per model request, written
//! in the [`super::stream_json`] format — and a small host driver that
//! answers approvals or cancels on cue. The golden is the protocol projection
//! (`protocol_parity::event_to_protocol`, the `EventMsg` wire shape every
//! frontend consumes), normalized, plus one trailing `harness_summary` line
//! with the provider request count and the workspace after the turn.
//!
//! Normalization (also stated in the fixture README): the per-session
//! `thread_id`/`session_id` routing envelope is dropped; liveness heartbeats
//! are dropped; UUIDs, timestamps, temp paths and durations are masked; the
//! system prompt and tool catalog bodies are replaced by a marker because the
//! prompt family owns those bytes; an uninterrupted run of completion events
//! is put in a canonical order because parallel completions race and their
//! pairs interleave: `operation_activity_completed` by span id, then
//! `tool_call_complete` by tool call id. The activity events of one announced
//! parallel batch (the events after the `Executing N ... parallel chunk(s)`
//! status) are ordered the same way, starts first, because a fast tool can
//! finish before its sibling has started. Only a causally valid batch is
//! reordered; no error or other event is crossed.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use codewhale_config::AppMode;
use codewhale_execpolicy::ApprovalMode;
use codewhale_models::{MessageRequest, MessageResponse, StreamEvent};
use codewhale_protocol::ids::{SessionId, ThreadId};

use super::golden::{self, Failures, Masker, Sandbox};
use super::stream_json;
use crate::compaction::CompactionConfig;
use crate::core::engine::{Engine, EngineConfig};
use crate::core::events::Event;
use crate::core::ops::{Op, TurnSpec, UserInputProvenance};
use crate::core::protocol_parity::{ProtocolIds, event_to_protocol};
use crate::llm_client::{LlmClient, StreamEventBox};
use codewhale_runtime::config::Config;

const FAMILY: &str = "events";
pub(super) const PREFIX_OWNED_BY_PROMPT_FAMILY: &str = "<pinned by the prompt family>";

/// Keys whose values are clocks or durations.
const VOLATILE_KEYS: &[&str] = &[
    "created_at",
    "duration_ms",
    "first_token_ms",
    "request_ms",
    "elapsed_ms",
    "pinned_combined_hash",
];

fn event_deadline() -> Duration {
    if cfg!(windows) {
        Duration::from_secs(60)
    } else {
        Duration::from_secs(20)
    }
}

enum Step {
    Stream {
        events: Vec<StreamEvent>,
        hang: bool,
    },
    Refuse(String),
}

/// The scripted fake provider: one queued step per model request. Unlike the
/// general-purpose `MockLlmClient` it appends nothing — a script without
/// `message_stop` is a truncated stream, exactly as written.
pub(super) struct ScriptedProvider {
    model: String,
    steps: Mutex<VecDeque<Step>>,
    stream_requests: AtomicUsize,
    other_requests: AtomicUsize,
    captured: Mutex<Vec<MessageRequest>>,
}

impl ScriptedProvider {
    pub(super) fn from_script(model: &str, script: &Value) -> Self {
        assert!(
            script.as_array().is_some_and(|steps| !steps.is_empty()),
            "provider_script must contain at least one response"
        );
        let steps = script
            .as_array()
            .expect("provider_script is an array")
            .iter()
            .map(|step| {
                if let Some(message) = step.get("request_error").and_then(Value::as_str) {
                    return Step::Refuse(message.to_string());
                }
                let events = step["events"]
                    .as_array()
                    .expect("provider_script step has events")
                    .iter()
                    .map(|event| {
                        stream_json::from_json(event)
                            .unwrap_or_else(|error| panic!("script event {event}: {error}"))
                    })
                    .collect();
                Step::Stream {
                    events,
                    hang: step.get("then").and_then(Value::as_str) == Some("hang"),
                }
            })
            .collect();
        Self {
            model: model.to_string(),
            steps: Mutex::new(steps),
            stream_requests: AtomicUsize::new(0),
            other_requests: AtomicUsize::new(0),
            captured: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn stream_requests(&self) -> usize {
        self.stream_requests.load(Ordering::SeqCst)
    }

    pub(super) fn other_requests(&self) -> usize {
        self.other_requests.load(Ordering::SeqCst)
    }

    pub(super) fn captured(&self) -> Vec<MessageRequest> {
        self.captured.lock().expect("captured requests").clone()
    }
}

impl LlmClient for ScriptedProvider {
    fn provider_name(&self) -> &'static str {
        "conformance"
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn create_message(&self, _request: MessageRequest) -> Result<MessageResponse> {
        self.other_requests.fetch_add(1, Ordering::SeqCst);
        Err(anyhow!(
            "conformance provider serves streaming turns only; a non-streaming request is not scripted"
        ))
    }

    async fn create_message_stream(&self, request: MessageRequest) -> Result<StreamEventBox> {
        let call = self.stream_requests.fetch_add(1, Ordering::SeqCst) + 1;
        self.captured
            .lock()
            .expect("captured requests")
            .push(request);
        let step = self.steps.lock().expect("script").pop_front();
        match step {
            None => Err(anyhow!(
                "conformance script exhausted: model request #{call} has no scripted response"
            )),
            Some(Step::Refuse(message)) => Err(anyhow!(message)),
            Some(Step::Stream { events, hang }) => {
                let stream: StreamEventBox = Box::pin(async_stream::stream! {
                    for event in events {
                        yield Ok::<StreamEvent, anyhow::Error>(event);
                    }
                    if hang {
                        std::future::pending::<()>().await;
                    }
                });
                Ok(stream)
            }
        }
    }
}

fn app_mode(case: &Value) -> AppMode {
    match case["mode"].as_str().unwrap_or("agent") {
        "agent" => AppMode::Agent,
        "plan" => AppMode::Plan,
        "operate" => AppMode::Operate,
        other => panic!("unknown mode `{other}`"),
    }
}

fn approval_mode(case: &Value) -> ApprovalMode {
    match case["approval_mode"].as_str().unwrap_or("suggest") {
        "suggest" => ApprovalMode::Suggest,
        "auto" => ApprovalMode::Auto,
        "bypass" => ApprovalMode::Bypass,
        "never" => ApprovalMode::Never,
        other => panic!("unknown approval_mode `{other}`"),
    }
}

pub(super) fn send_message_op(case: &Value, config: &Config) -> Op {
    let model = case["model"].as_str().expect("case.model");
    let route = crate::route_runtime::resolve_runtime_route(
        config,
        config.active_provider_identity().unwrap().provider,
        Some(model),
    )
    .expect("resolve conformance route");
    Op::SendMessage(TurnSpec {
        profile_constitution: None,
        max_output_tokens: None,
        submission_id: None,
        content: case["user_message"]
            .as_str()
            .expect("case.user_message")
            .to_string(),
        images: Vec::new(),
        mode: app_mode(case),
        route: Box::new(route),
        compaction: Box::new(CompactionConfig::default()),
        initial_routed_usage: Box::default(),
        goal_objective: None,
        goal_token_budget: None,
        goal_status: crate::tools::goal::GoalStatus::Active,
        reasoning_effort: None,
        reasoning_effort_auto: false,
        auto_model: false,
        allow_shell: case["allow_shell"].as_bool().unwrap_or(true),
        trust_mode: false,
        auto_approve: case["auto_approve"].as_bool().unwrap_or(false),
        approval_mode: approval_mode(case),
        translation_enabled: false,
        allowed_tools: None,
        dynamic_tools: Vec::new(),
        hook_executor: None,
        verbosity: None,
        provenance: UserInputProvenance::ExternalUser,
    })
}

/// Relative path → sha256 prefix for every file left in the workspace.
fn workspace_listing(workspace: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if let Ok(bytes) = std::fs::read(&path) {
                let relative = path
                    .strip_prefix(root)
                    .expect("inside workspace")
                    .to_string_lossy()
                    .replace('\\', "/");
                let digest = codewhale_runtime::hashing::sha256_hex(&bytes);
                out.insert(relative, digest[..16].to_string());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(workspace, workspace, &mut out);
    out
}

pub(super) struct TurnRecord {
    pub(super) events: Vec<Event>,
    pub(super) failure: Option<String>,
}

/// Drive one scripted turn inside `sandbox` on a fresh current-thread
/// runtime, dropping the runtime (and its blocking post-turn work) before
/// returning so nothing outlives the sandboxed environment.
pub(super) fn run_scripted_turn(
    sandbox: &Sandbox,
    case: &Value,
    script: &Value,
) -> (TurnRecord, std::sync::Arc<ScriptedProvider>) {
    run_scripted_turn_with_deadline(sandbox, case, script, event_deadline())
}

fn run_scripted_turn_with_deadline(
    sandbox: &Sandbox,
    case: &Value,
    script: &Value,
    deadline: Duration,
) -> (TurnRecord, std::sync::Arc<ScriptedProvider>) {
    let model = case["model"].as_str().expect("case.model");
    let provider = std::sync::Arc::new(ScriptedProvider::from_script(model, script));
    let driver = case["driver"].as_array().cloned().unwrap_or_default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let (os, shell) = recorded_environment(case);
    // Held across the whole turn: the engine builds and refreshes its system
    // prompt on this thread's current-thread runtime.
    let _environment = crate::prompts::pin_recorded_environment(&os, &shell);
    let _host_tools = codewhale_dependencies::pin_recorded_host_tools(recorded_host_tools(case));
    let record = runtime.block_on(async {
        let config = Config::default();
        let engine_config = EngineConfig {
            workspace: sandbox.workspace.clone(),
            snapshots_enabled: false,
            subagents_enabled: false,
            session_id: Some("conformance-session".to_string()),
            ..EngineConfig::default()
        };
        let client: crate::core::model_client::SharedModelClient = provider.clone();
        let (mut engine, handle) = Engine::new_with_model_client(engine_config, &config, client);
        let (enforcement, no_new_privs_active) = recorded_platform(case);
        engine.pin_recorded_platform_posture(enforcement, no_new_privs_active);
        let op = send_message_op(case, &config);
        drive_turn(engine, handle, op, &driver, deadline).await
    });
    drop(runtime);
    (record, provider)
}

/// The execution boundary a case's golden was recorded under.
///
/// The Engine names its sandbox posture to the model in `<turn_meta>`, and a
/// live probe makes that line a fact about the runner: macOS applies a local
/// OS sandbox, a Linux runner without bubblewrap is policy-only, and Linux
/// startup may relax no-new-privs. Every scripted case therefore declares the
/// recorded facts and the harness replays exactly those, so a golden states
/// what the Engine does on that platform rather than which machine ran it.
/// A case without them fails loud instead of silently probing the host. The
/// label for each platform is owned and tested by `sandbox::policy`.
fn recorded_platform(
    case: &Value,
) -> (codewhale_sandbox::policy::SandboxEnforcement, Option<bool>) {
    use codewhale_sandbox::policy::SandboxEnforcement;
    let platform = case
        .get("recorded_platform")
        .expect("scripted conformance case must declare `recorded_platform`");
    let enforcement = match platform.get("sandbox_enforcement").and_then(Value::as_str) {
        Some("local_os") => SandboxEnforcement::LocalOs,
        Some("unavailable") => SandboxEnforcement::Unavailable,
        Some("external_backend") => SandboxEnforcement::ExternalBackend,
        other => panic!("unknown recorded_platform.sandbox_enforcement: {other:?}"),
    };
    let no_new_privs_active = match platform.get("no_new_privs_active") {
        Some(Value::Null) => None,
        Some(Value::Bool(active)) => Some(*active),
        other => panic!("recorded_platform.no_new_privs_active must be null or a bool: {other:?}"),
    };
    (enforcement, no_new_privs_active)
}

/// The OS and shell the golden's `## Environment` block was recorded with.
/// They enter the frozen prompt prefix, and so its hash in
/// `prefix_cache_change`; a case without them fails loud.
fn recorded_environment(case: &Value) -> (String, String) {
    let platform = case
        .get("recorded_platform")
        .expect("scripted conformance case must declare `recorded_platform`");
    let field = |name: &str| {
        platform
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| panic!("recorded_platform.{name} must be a non-empty string"))
            .to_string()
    };
    (field("os"), field("shell"))
}

/// The optional host-backed tools the recording machine had (see
/// `dependencies::host_tool_available`). They change the registry the golden
/// snapshot counts; a case without the list fails loud.
fn recorded_host_tools(case: &Value) -> Vec<String> {
    case.get("recorded_platform")
        .and_then(|platform| platform.get("host_tools"))
        .and_then(Value::as_array)
        .expect("recorded_platform.host_tools must list the recording host's optional tools")
        .iter()
        .map(|tool| {
            tool.as_str()
                .expect("recorded_platform.host_tools entries are tool names")
                .to_string()
        })
        .collect()
}

/// Run one turn and return every engine event up to and including
/// `TurnComplete`, then everything the engine emits while it shuts down.
async fn drive_turn(
    engine: Engine,
    handle: crate::core::engine::EngineHandle,
    op: Op,
    driver: &[Value],
    deadline: Duration,
) -> TurnRecord {
    let mut run = tokio::spawn(engine.run());
    handle.send(op).await.expect("send conformance turn");
    let rx = handle.rx_event.clone();
    let ids = protocol_ids();
    let mut events = Vec::new();
    let mut fired = vec![false; driver.len()];
    let mut failure = None;
    let mut completed = false;
    let expires = tokio::time::Instant::now() + deadline;
    loop {
        if tokio::time::Instant::now() >= expires {
            failure = Some(format!(
                "harness timeout: engine turn did not complete within {deadline:?}"
            ));
            break;
        }
        let next = golden::complete_within(
            "engine turn",
            expires.saturating_duration_since(tokio::time::Instant::now()),
            async { rx.write().await.recv().await },
        )
        .await;
        let event = match next {
            Ok(Some(event)) => event,
            Ok(None) => break,
            Err(error) => {
                failure = Some(error);
                break;
            }
        };
        let projected = serde_json::to_value(event_to_protocol(&event, &ids)).expect("EventMsg");
        for (index, rule) in driver.iter().enumerate() {
            if fired[index] || !rule_matches(&rule["when"], &projected) {
                continue;
            }
            fired[index] = true;
            match rule["action"].as_str().expect("driver action") {
                "cancel" => handle.cancel(),
                action @ ("approve" | "deny") => {
                    let Event::ApprovalRequired { id, .. } = &event else {
                        panic!("driver `{action}` must match an approval_required event");
                    };
                    let answered = if action == "approve" {
                        handle.approve_tool_call(id.clone()).await
                    } else {
                        handle.deny_tool_call(id.clone()).await
                    };
                    answered.expect("answer approval");
                }
                other => panic!("unknown driver action `{other}`"),
            }
        }
        let terminal = matches!(event, Event::TurnComplete { .. });
        events.push(event);
        if terminal {
            completed = true;
            break;
        }
    }
    if !completed && failure.is_none() {
        failure = Some("engine closed before TurnComplete".to_string());
    }
    handle.cancel();
    match golden::complete_within(
        "engine shutdown request",
        deadline,
        handle.send(Op::Shutdown),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            failure.get_or_insert_with(|| format!("engine shutdown request failed: {error}"));
        }
        Err(error) => {
            failure.get_or_insert(error);
        }
    }
    match golden::complete_within("engine shutdown", deadline, &mut run).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            failure.get_or_insert_with(|| format!("engine task failed: {error}"));
        }
        Err(error) => {
            failure.get_or_insert(error);
            run.abort();
            let _ = run.await;
        }
    }
    let mut rx = rx.write().await;
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    TurnRecord { events, failure }
}

fn rule_matches(when: &Value, projected: &Value) -> bool {
    if when["event"].as_str() != projected["event"].as_str() {
        return false;
    }
    if let Some(needle) = when.get("delta_contains").and_then(Value::as_str) {
        return projected["delta"]
            .as_str()
            .is_some_and(|delta| delta.contains(needle));
    }
    true
}

fn protocol_ids() -> ProtocolIds {
    ProtocolIds {
        thread_id: ThreadId::from_string("conformance-thread"),
        session_id: SessionId::from_string("conformance-session"),
    }
}

/// Project, drop, mask and order engine events into golden lines.
pub(super) fn normalize_events(events: &[Event], masker: &mut Masker) -> Vec<Value> {
    let ids = protocol_ids();
    let mut lines: Vec<Value> = Vec::new();
    for event in events {
        let mut value = serde_json::to_value(event_to_protocol(event, &ids)).expect("EventMsg");
        let kind = value["event"].as_str().unwrap_or_default().to_string();
        if kind == "tool_call_heartbeat" {
            continue;
        }
        if let Some(map) = value.as_object_mut() {
            map.remove("thread_id");
            map.remove("session_id");
            for owned in ["tool_catalog", "system_prompt"] {
                if map.get(owned).is_some_and(|value| !value.is_null()) {
                    map.insert(owned.to_string(), json!(PREFIX_OWNED_BY_PROMPT_FAMILY));
                }
            }
        }
        masker.value(&mut value);
        lines.push(golden::canonical(&value));
    }
    order_parallel_batches(&mut lines);
    order_parallel_completions(&mut lines);
    lines
}

/// The activity and tool events a parallel batch emits.
fn batch_rank(line: &Value) -> Option<(u8, &'static str)> {
    match line["event"].as_str() {
        Some("operation_activity_started") => Some((0, "span_id")),
        Some("operation_activity_completed") => Some((1, "span_id")),
        Some("tool_call_complete") => Some((2, "tool_call_id")),
        _ => None,
    }
}

/// Whether `run` is a legal interleaving of parallel tools: every span starts
/// once and completes at most once after its start, and a tool's completion
/// follows its own activity completion. A run that fails this is left in
/// its recorded order so the golden reports the violation.
fn batch_is_causal(run: &[Value]) -> bool {
    let mut started = std::collections::BTreeSet::new();
    let mut completed = std::collections::BTreeSet::new();
    let mut finished_calls = std::collections::BTreeSet::new();
    for line in run {
        let span = line["span_id"].as_str().unwrap_or_default();
        let call = span.split('#').next().unwrap_or_default();
        match line["event"].as_str() {
            Some("operation_activity_started") => {
                if !started.insert(span) {
                    return false;
                }
            }
            Some("operation_activity_completed") => {
                if !started.contains(span) || !completed.insert(span) {
                    return false;
                }
            }
            Some("tool_call_complete") => {
                finished_calls.insert(line["tool_call_id"].as_str().unwrap_or_default());
            }
            _ => {}
        }
        // A tool completion may not precede its own activity completion.
        if line["event"].as_str() == Some("operation_activity_completed")
            && finished_calls.contains(call)
        {
            return false;
        }
    }
    true
}

/// Parallel tools in one announced batch run concurrently, so one tool can
/// start, finish and report before its sibling has started (a blocking-pool
/// read that completes before its first poll). The run of activity and tool
/// events after the batch's `Executing N ... parallel chunk(s)` status is put
/// in one canonical order: starts by span, activity completions by span, tool
/// completions by call id. Only a causally valid run is reordered, and every
/// event, outcome and boundary is kept.
fn order_parallel_batches(lines: &mut [Value]) {
    let mut at = 0;
    while at < lines.len() {
        let announces_batch = lines[at]["event"].as_str() == Some("status")
            && lines[at]["message"].as_str().is_some_and(|message| {
                message.starts_with("Executing ") && message.ends_with(" parallel chunk(s)")
            });
        at += 1;
        if !announces_batch {
            continue;
        }
        let start = at;
        let mut end = start;
        while end < lines.len() && batch_rank(&lines[end]).is_some() {
            end += 1;
        }
        if batch_is_causal(&lines[start..end]) {
            lines[start..end].sort_by(|left, right| {
                let (left_rank, left_key) = batch_rank(left).unwrap_or((0, "span_id"));
                let (right_rank, right_key) = batch_rank(right).unwrap_or((0, "span_id"));
                left_rank
                    .cmp(&right_rank)
                    .then_with(|| left[left_key].as_str().cmp(&right[right_key].as_str()))
            });
        }
        at = end;
    }
}

/// Parallel tools race to report: each emits its activity completion and then
/// its tool completion, and under load the two tools' pairs interleave. One
/// uninterrupted run of completion events is therefore put in a canonical
/// order: activity completions by span, then tool completions by call id.
/// Every event, its outcome and the run's boundaries are kept; only the order
/// inside the run is normalized.
fn order_parallel_completions(lines: &mut [Value]) {
    fn completion(line: &Value) -> Option<(u8, &'static str)> {
        match line["event"].as_str() {
            Some("operation_activity_completed") => Some((0, "span_id")),
            Some("tool_call_complete") => Some((1, "tool_call_id")),
            _ => None,
        }
    }
    let mut start = 0;
    while start < lines.len() {
        if completion(&lines[start]).is_none() {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < lines.len() && completion(&lines[end]).is_some() {
            end += 1;
        }
        lines[start..end].sort_by(|left, right| {
            let (left_kind, left_key) = completion(left).unwrap_or((0, "span_id"));
            let (right_kind, right_key) = completion(right).unwrap_or((0, "span_id"));
            left_kind
                .cmp(&right_kind)
                .then_with(|| left[left_key].as_str().cmp(&right[right_key].as_str()))
        });
        start = end;
    }
}

#[test]
fn parallel_completion_projection_keeps_outcomes_spans_and_causal_boundaries() {
    let first = json!({"event": "operation_activity_completed", "span_id": "call_a#1", "outcome": "succeeded"});
    let second = json!({"event": "operation_activity_completed", "span_id": "call_b#2", "outcome": "failed"});
    let boundary = json!({"event": "error", "message": "exact error bytes"});
    let start = json!({"event": "operation_activity_started", "span_id": "call_a#1"});
    let original = vec![
        start.clone(),
        second.clone(),
        first.clone(),
        boundary.clone(),
        second.clone(),
        start.clone(),
        first.clone(),
    ];
    let expected = vec![
        start.clone(),
        first.clone(),
        second.clone(),
        boundary,
        second,
        start,
        first,
    ];
    let mut normalized = original.clone();
    order_parallel_completions(&mut normalized);
    assert_eq!(normalized, expected);
    assert_eq!(normalized.len(), original.len());

    // Two tools' completion pairs interleaved under load normalize to the
    // same order as the uninterleaved run.
    let tool = |id: &str| json!({"event": "tool_call_complete", "tool_call_id": id});
    let activity = |span: &str| json!({"event": "operation_activity_completed", "span_id": span, "outcome": "succeeded"});
    let mut interleaved = vec![
        activity("call_b#2"),
        tool("call_b"),
        activity("call_a#1"),
        tool("call_a"),
    ];
    order_parallel_completions(&mut interleaved);
    assert_eq!(
        interleaved,
        vec![
            activity("call_a#1"),
            activity("call_b#2"),
            tool("call_a"),
            tool("call_b")
        ]
    );

    let mut wrong_outcome = original.clone();
    wrong_outcome[1]["outcome"] = json!("succeeded");
    order_parallel_completions(&mut wrong_outcome);
    assert_ne!(wrong_outcome, expected, "wrong outcome was normalized away");

    let mut wrong_span = original;
    wrong_span[1]["span_id"] = json!("call_other#2");
    order_parallel_completions(&mut wrong_span);
    assert_ne!(
        wrong_span, expected,
        "wrong span relationship was normalized away"
    );
}

#[test]
fn parallel_batch_projection_orders_legal_interleavings_and_keeps_violations() {
    let status =
        json!({"event": "status", "message": "Executing 2 read-only tools in 1 parallel chunk(s)"});
    let start = |span: &str| json!({"event": "operation_activity_started", "span_id": span});
    let done = |span: &str, outcome: &str| json!({"event": "operation_activity_completed", "span_id": span, "outcome": outcome});
    let tool = |id: &str| json!({"event": "tool_call_complete", "tool_call_id": id});
    let tail = json!({"event": "session_updated"});
    let canonical = vec![
        status.clone(),
        start("a#1"),
        start("b#2"),
        done("a#1", "succeeded"),
        done("b#2", "succeeded"),
        tool("a"),
        tool("b"),
        tail.clone(),
    ];
    let with_tail = |run: Vec<Value>| {
        let mut lines = vec![status.clone()];
        lines.extend(run);
        lines.push(tail.clone());
        lines
    };
    // Recorded: both start, then both finish. Hosted CI also saw one tool
    // finish before its sibling started, and both pairs fully interleaved.
    for run in [
        vec![
            start("a#1"),
            start("b#2"),
            done("a#1", "succeeded"),
            done("b#2", "succeeded"),
            tool("a"),
            tool("b"),
        ],
        vec![
            start("a#1"),
            done("a#1", "succeeded"),
            start("b#2"),
            done("b#2", "succeeded"),
            tool("a"),
            tool("b"),
        ],
        vec![
            start("a#1"),
            done("a#1", "succeeded"),
            tool("a"),
            start("b#2"),
            done("b#2", "succeeded"),
            tool("b"),
        ],
        vec![
            start("b#2"),
            start("a#1"),
            done("b#2", "succeeded"),
            tool("b"),
            done("a#1", "succeeded"),
            tool("a"),
        ],
    ] {
        let mut lines = with_tail(run);
        order_parallel_batches(&mut lines);
        order_parallel_completions(&mut lines);
        assert_eq!(lines, canonical);
    }

    // Outcomes survive the reordering.
    let mut lines = with_tail(vec![
        start("a#1"),
        done("a#1", "failed"),
        start("b#2"),
        done("b#2", "succeeded"),
        tool("a"),
        tool("b"),
    ]);
    order_parallel_batches(&mut lines);
    assert_eq!(lines[3], done("a#1", "failed"));

    // An illegal run is left exactly as recorded: a completion with no start,
    // a repeated start, a repeated completion, a tool completion before its
    // activity completion.
    for run in [
        vec![done("a#1", "succeeded"), start("a#1"), tool("a")],
        vec![
            start("a#1"),
            start("a#1"),
            done("a#1", "succeeded"),
            tool("a"),
        ],
        vec![
            start("a#1"),
            done("a#1", "succeeded"),
            done("a#1", "succeeded"),
            tool("a"),
        ],
        vec![start("a#1"), tool("a"), done("a#1", "succeeded")],
    ] {
        let recorded = with_tail(run);
        let mut lines = recorded.clone();
        order_parallel_batches(&mut lines);
        assert_eq!(lines, recorded, "an illegal run was normalized away");
    }

    // Without an announced parallel batch nothing is reordered: a serial run
    // that starts its second tool early is a real change.
    let serial = vec![start("b#2"), done("b#2", "succeeded"), start("a#1")];
    let mut lines = serial.clone();
    order_parallel_batches(&mut lines);
    assert_eq!(lines, serial);
}

fn check_invariants(case: &Value, workspace: &Path, provider: &ScriptedProvider) -> Vec<String> {
    let mut violations = Vec::new();
    for invariant in case["invariants"].as_array().into_iter().flatten() {
        if let Some(file) = invariant
            .get("workspace_file_absent")
            .and_then(Value::as_str)
            && workspace.join(file).exists()
        {
            violations.push(format!(
                "`{file}` exists: a tool side effect ran that the scenario forbids"
            ));
        }
        if let Some(file) = invariant
            .get("workspace_file_present")
            .and_then(Value::as_str)
            && !workspace.join(file).exists()
        {
            violations.push(format!(
                "`{file}` is gone: a tool side effect ran that the scenario forbids"
            ));
        }
        if let Some(max) = invariant.get("max_model_requests").and_then(Value::as_u64)
            && provider.stream_requests() as u64 > max
        {
            violations.push(format!(
                "{} model requests were issued; at most {max} allowed",
                provider.stream_requests()
            ));
        }
    }
    violations
}

fn run_case(name: &str, case: &Value, failures: &mut Failures) {
    run_case_with_deadline(name, case, failures, event_deadline());
}

fn run_case_with_deadline(name: &str, case: &Value, failures: &mut Failures, deadline: Duration) {
    let sandbox = Sandbox::new(case);
    let workspace = sandbox.workspace.clone();
    let (record, provider) =
        run_scripted_turn_with_deadline(&sandbox, case, &case["provider_script"], deadline);
    if let Some(error) = record.failure {
        failures.push(name, error);
        return;
    }
    let mut masker = sandbox.masker(VOLATILE_KEYS);
    let mut lines = normalize_events(&record.events, &mut masker);
    let summary = json!({
        "model_requests": provider.stream_requests(),
        "non_streaming_requests": provider.other_requests(),
        "workspace_after": workspace_listing(&workspace),
    });
    lines.push(json!({ "harness_summary": summary }));

    let violations = check_invariants(case, &workspace, &provider);
    if !violations.is_empty() {
        failures.push(name, format!("invariant broken: {}", violations.join("; ")));
        return;
    }

    failures.record(
        name,
        golden::check_golden(
            &golden::family_dir(FAMILY).join(format!("{name}.golden.jsonl")),
            &golden::jsonl(&lines),
        ),
    );
}

#[test]
fn harness_timeout_rejects_a_real_stalled_turn() {
    let mut case = golden::read_case(FAMILY, "plain_answer");
    case["provider_script"][0]["then"] = json!("hang");
    case["provider_script"][0]["events"] = json!([]);
    let mut failures = Failures::default();
    // This is the same recording path as normal cases. In update mode it must
    // still fail and must not create this deliberately absent golden.
    let name = "harness_timeout_control";
    let path = golden::family_dir(FAMILY).join(format!("{name}.golden.jsonl"));
    assert!(!path.exists());
    run_case_with_deadline(name, &case, &mut failures, Duration::from_secs(1));
    assert!(failures.contains("harness timeout"));
    assert!(
        !path.exists(),
        "an incomplete turn was recorded as a golden"
    );
}

#[test]
fn golden_turn_events_match() {
    // The goldens carry the span numbers of one pass over the cases in an
    // otherwise idle process (`one_tool_call` #1, `parallel_tool_calls` #2
    // and #3); replay that numbering whatever else this process runs.
    let _spans = crate::core::engine::pin_replay_span_sequence();
    let names = golden::case_names(FAMILY);
    let mut failures = Failures::default();
    for name in &names {
        let case = golden::read_case(FAMILY, name);
        run_case(name, &case, &mut failures);
    }
    failures.finish(FAMILY, names.len());
}
