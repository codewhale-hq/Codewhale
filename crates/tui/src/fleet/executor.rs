//! Fleet executor — runs a Fleet worker as a real `codewhale exec` subprocess.
//!
//! A Fleet worker IS a headless `codewhale exec` run. There is no separate
//! "Fleet worker" execution engine: the sub-agent runtime, full tool surface,
//! and recursion depth all come from the one `codewhale exec` runtime, so
//! Fleets and sub-agents are one substrate (not two moving targets).
//!
//! This module is the bridge:
//! - [`build_worker_exec_command`] turns a `FleetTaskSpec` + `FleetExecConfig`
//!   into the `codewhale [route flags] exec --output-format stream-json …`
//!   argv that a host adapter ([`super::host`]) launches locally or over SSH.
//! - [`map_exec_stream_line`] maps one stream-json line emitted by that worker
//!   into a [`FleetWorkerEventPayload`] for the durable ledger, so the ledger
//!   persists the worker's own event vocabulary instead of a simulated one.
//! - [`classify_worker_exit`] turns the process exit into a terminal event.
//!
//! The TUI/CLI/Runtime API observe the ledger's compact event stream — they
//! never render a child session, which is what keeps the orchestrator light at
//! high fanout.

#![allow(dead_code)]

use anyhow::Result;
use codewhale_config::FleetExecConfig;
use codewhale_protocol::fleet::{FleetHostSpec, FleetTaskSpec, FleetWorkerEventPayload};

use super::host::{FleetHostAdapter, FleetWorkerCommand};
use super::profile::AgentProfile;
use super::task_spec::FleetWorkerFinalAnswer;
use super::worker_runtime::{
    fleet_task_prompt, fleet_task_prompt_with_profiles, fleet_worker_launch_reasoning_effort,
    fleet_worker_launch_route,
};
use crate::tools::spec::{
    ToolAuthorityEnvelope, ToolMutationAuthority, ToolShellAuthority, ToolVerificationAuthority,
};
use crate::tools::subagent::{AgentWorkerSpec, FleetRole};

#[derive(Clone, Copy, Default)]
struct WorkerExecRoute<'a> {
    model: Option<&'a str>,
    provider: Option<&'a str>,
    reasoning_effort: Option<&'a str>,
}

#[derive(Clone, Copy, Default)]
struct WorkerExecLimits {
    max_turns: Option<u32>,
    max_tool_calls: Option<u32>,
}

/// Resolve the executable used for Fleet worker subprocesses.
///
/// Kept here so every long-lived surface (CLI and Runtime API) launches the
/// same configured worker binary instead of silently diverging.
pub fn configured_codewhale_binary() -> String {
    std::env::var("CODEWHALE_FLEET_CODEWHALE_BINARY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "codewhale".to_string())
}

/// Build the `codewhale exec` argv that runs a fleet task headlessly.
///
/// `--auto` is always passed: a headless worker has no human to approve tool
/// calls, so it runs with full (policy-gated) tool access. `--output-format
/// stream-json` makes the worker emit the NDJSON event stream this module
/// parses. A worker launched with the v0.9.1 machine-readable outer authority
/// cap is a truthful leaf (`max_spawn_depth = 0`): the nested-agent surface is
/// disabled until authority scopes can be intersected across
/// process/workspace boundaries.
///
/// Secrets are NEVER placed on the argv: provider credentials are resolved by
/// the worker process from its own config/keyring exactly like an interactive
/// run. The host adapter additionally refuses secret-bearing env keys. The
/// `--provider` flag threaded by [`build_worker_exec_command_with_profiles`] is
/// a non-secret provider *identifier* only (#4093) — the worker still resolves
/// that provider's credentials from its own env/config, so this invariant
/// holds.
pub fn build_worker_exec_command(
    codewhale_binary: &str,
    task_spec: &FleetTaskSpec,
    exec_config: &FleetExecConfig,
    model: Option<&str>,
) -> FleetWorkerCommand {
    let max_turns = effective_task_max_turns(task_spec, exec_config);
    let max_tool_calls = task_max_tool_calls(task_spec);
    build_worker_exec_command_from_prompt(
        codewhale_binary,
        fleet_task_prompt(task_spec),
        exec_config,
        WorkerExecRoute {
            model,
            ..WorkerExecRoute::default()
        },
        None,
        WorkerExecLimits {
            max_turns,
            max_tool_calls,
        },
    )
}

/// Build a worker command after resolving workspace Fleet profile input.
///
/// The launched subprocess runs on the worker's RESOLVED route, not blindly on
/// the run-level session model (#4093 AC #4): the per-worker model+provider are
/// resolved from the task's agent profile via the same explicit-only path the
/// receipt uses ([`fleet_worker_launch_route`]). A worker whose profile pins
/// provider B thus launches on provider B's model even when the parent session
/// is on provider A. Workers with no profile-bound provider fall back to the
/// run-level model and emit no `--provider`, so the worker keeps its own
/// session default (today's behavior, unchanged).
pub fn build_worker_exec_command_with_profiles(
    codewhale_binary: &str,
    task_spec: &FleetTaskSpec,
    exec_config: &FleetExecConfig,
    model: Option<&str>,
    agent_profiles: &[AgentProfile],
) -> Result<FleetWorkerCommand> {
    let (worker_model, worker_provider) =
        fleet_worker_launch_route(task_spec, agent_profiles, model.unwrap_or_default());
    let worker_reasoning_effort = fleet_worker_launch_reasoning_effort(task_spec, agent_profiles);
    let max_turns = effective_task_max_turns(task_spec, exec_config);
    let max_tool_calls = task_max_tool_calls(task_spec);
    Ok(build_worker_exec_command_from_prompt(
        codewhale_binary,
        fleet_task_prompt_with_profiles(task_spec, agent_profiles)?,
        exec_config,
        WorkerExecRoute {
            model: Some(worker_model.as_str()),
            provider: worker_provider.as_deref(),
            reasoning_effort: worker_reasoning_effort.as_deref(),
        },
        None,
        WorkerExecLimits {
            max_turns,
            max_tool_calls,
        },
    ))
}

/// Build the exact Fleet subprocess command from the coordination-registered
/// worker spec. Unlike the compatibility helpers above, production dispatch
/// uses the projected objective and carries a machine-readable outer authority
/// envelope into the child process.
pub fn build_worker_exec_command_with_launch_spec(
    codewhale_binary: &str,
    task_spec: &FleetTaskSpec,
    launch_spec: &AgentWorkerSpec,
    exec_config: &FleetExecConfig,
    model: Option<&str>,
    agent_profiles: &[AgentProfile],
) -> Result<FleetWorkerCommand> {
    let (worker_model, worker_provider) =
        fleet_worker_launch_route(task_spec, agent_profiles, model.unwrap_or_default());
    let worker_reasoning_effort = fleet_worker_launch_reasoning_effort(task_spec, agent_profiles);
    let authority = authority_envelope_for_worker(launch_spec, task_spec)?;
    // Production dispatch receives an already-hardened launch spec from the
    // Fleet manager. Its positive max_steps value has therefore already been
    // intersected with a positive FleetExecConfig.max_turns; zero remains the
    // explicit unbounded sentinel.
    let max_turns = (launch_spec.max_steps > 0).then_some(launch_spec.max_steps);
    let max_tool_calls = task_max_tool_calls(task_spec);
    Ok(build_worker_exec_command_from_prompt(
        codewhale_binary,
        launch_spec.objective.clone(),
        exec_config,
        WorkerExecRoute {
            model: Some(worker_model.as_str()),
            provider: worker_provider.as_deref(),
            reasoning_effort: worker_reasoning_effort.as_deref(),
        },
        Some(&authority),
        WorkerExecLimits {
            max_turns,
            max_tool_calls,
        },
    ))
}

fn task_max_steps(task_spec: &FleetTaskSpec) -> Option<u32> {
    task_spec
        .budget
        .as_ref()
        .and_then(|budget| budget.max_steps)
        .filter(|max_steps| *max_steps > 0)
}

fn task_max_tool_calls(task_spec: &FleetTaskSpec) -> Option<u32> {
    task_spec
        .budget
        .as_ref()
        .and_then(|budget| budget.max_tool_calls)
        .filter(|max_tool_calls| *max_tool_calls > 0)
}

fn effective_task_max_turns(
    task_spec: &FleetTaskSpec,
    exec_config: &FleetExecConfig,
) -> Option<u32> {
    match (task_max_steps(task_spec), exec_config.max_turns) {
        (Some(task_max_steps), fleet_max_turns) if fleet_max_turns > 0 => {
            Some(task_max_steps.min(fleet_max_turns))
        }
        (Some(task_max_steps), _) => Some(task_max_steps),
        (None, fleet_max_turns) if fleet_max_turns > 0 => Some(fleet_max_turns),
        (None, _) => None,
    }
}

pub(crate) fn authority_envelope_for_worker(
    spec: &AgentWorkerSpec,
    task_spec: &FleetTaskSpec,
) -> Result<ToolAuthorityEnvelope> {
    let (authority, writable_roots, writable_files, coordination_contracts) =
        if spec.runtime_profile.permissions.write {
            let manifest = spec.launch_manifest.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "write-capable Fleet worker '{}' has no launch manifest",
                    spec.worker_id
                )
            })?;
            (
                ToolMutationAuthority::ScopedWrite,
                super::worker_runtime::fleet_runtime_write_roots(task_spec)?,
                manifest.writable_files.clone(),
                manifest.coordination_contracts.clone(),
            )
        } else {
            (
                ToolMutationAuthority::ReadOnly,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
        };
    let shell = if authority == ToolMutationAuthority::ReadOnly
        && matches!(
            &spec.agent_type,
            FleetRole::Scout | FleetRole::Reviewer | FleetRole::Planner
        )
        && spec.runtime_profile.shell.allows_shell()
    {
        // Scout, Reviewer, and Planner get one classifier-bounded
        // foreground shell. Verifiers keep the dedicated Run surface,
        // consultants remain shell-less, and writers retain the historical
        // subprocess contract.
        ToolShellAuthority::ReadOnly
    } else {
        ToolShellAuthority::None
    };
    let verification = if authority == ToolMutationAuthority::ReadOnly
        && matches!(&spec.agent_type, FleetRole::Verifier)
        && spec.runtime_profile.shell.allows_shell()
    {
        ToolVerificationAuthority::Bounded
    } else {
        ToolVerificationAuthority::None
    };
    ToolAuthorityEnvelope {
        schema_version: 1,
        owner: spec.worker_id.clone(),
        authority,
        network_access: Some(spec.runtime_profile.permissions.network),
        shell,
        verification,
        writable_roots,
        writable_files,
        coordination_contracts,
    }
    .normalized()
    .map_err(anyhow::Error::msg)
}

fn build_worker_exec_command_from_prompt(
    codewhale_binary: &str,
    task_prompt: String,
    exec_config: &FleetExecConfig,
    route: WorkerExecRoute<'_>,
    authority: Option<&ToolAuthorityEnvelope>,
    limits: WorkerExecLimits,
) -> FleetWorkerCommand {
    let mut args: Vec<String> = Vec::new();

    // The canonical `codewhale` dispatcher owns these route overrides as
    // global flags and deliberately rejects them after `exec`. Keep them in
    // front of the subcommand so Fleet commands work through the installed
    // dispatcher as well as when a host points directly at `codewhale-tui`.
    if let Some(model) = route.model.map(str::trim).filter(|m| !m.is_empty()) {
        args.push("--model".to_string());
        args.push(model.to_string());
    }

    // Non-secret provider identifier only (#4093): the worker resolves the
    // provider's credentials from its own env/config. Emitted ONLY when the
    // worker's profile explicitly pins a provider, so profile-less workers keep
    // their own session default exactly as before.
    if let Some(provider) = route.provider.map(str::trim).filter(|p| !p.is_empty()) {
        args.push("--provider".to_string());
        args.push(provider.to_string());
    }

    args.extend([
        "exec".to_string(),
        "--auto".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        // R7: the worker shuts itself down when the manager dies (stdin EOF).
        "--parent-death-watch".to_string(),
    ]);

    // Non-secret thinking tier only (#4137). This is profile metadata and
    // follows the same explicit-only policy as provider: omit it when the
    // worker profile inherits the session/default reasoning setting.
    if let Some(reasoning_effort) = route
        .reasoning_effort
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        args.push("--reasoning-effort".to_string());
        args.push(reasoning_effort.to_string());
    }

    if !exec_config.allowed_tools.is_empty() {
        args.push("--allowed-tools".to_string());
        args.push(exec_config.allowed_tools.join(","));
    }
    if !exec_config.disallowed_tools.is_empty() {
        args.push("--disallowed-tools".to_string());
        args.push(exec_config.disallowed_tools.join(","));
    }
    if let Some(max_turns) = limits.max_turns {
        args.push("--max-turns".to_string());
        args.push(max_turns.to_string());
    }
    if let Some(max_tool_calls) = limits.max_tool_calls {
        args.push("--max-tool-calls".to_string());
        args.push(max_tool_calls.to_string());
    }
    if !exec_config.append_system_prompt.trim().is_empty() {
        // One `--flag=value` argument, so the policy is always the value: as a
        // separate argument, text that reads like one of exec's own options
        // (`--hooks`) is parsed as that option and the worker fails to start.
        args.push(format!(
            "--append-system-prompt={}",
            exec_config.append_system_prompt
        ));
    }

    if let Some(authority) = authority {
        args.push("--tool-authority-json".to_string());
        args.push(
            serde_json::to_string(authority)
                .expect("validated Fleet tool authority envelope must serialize"),
        );
    }

    // The composed task prompt is the final positional argument.
    args.push(task_prompt);

    FleetWorkerCommand::new(codewhale_binary.to_string(), args)
}

/// Map one `codewhale exec` stream-json line into a fleet ledger event.
///
/// Returns `None` for lines that don't correspond to a worker lifecycle
/// transition (e.g. `session_capture`, `metadata`). The exec event schema is
/// `{"type": "...", ...}` (see `ExecStreamEvent` in `main.rs`).
pub fn map_exec_stream_line(line: &str) -> Option<FleetWorkerEventPayload> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    map_exec_stream_value(&value)
}

/// [`map_exec_stream_line`] on an already-parsed line, so the incremental
/// stream reader parses each frame exactly once.
fn map_exec_stream_value(value: &serde_json::Value) -> Option<FleetWorkerEventPayload> {
    match value.get("type").and_then(serde_json::Value::as_str)? {
        "tool_use" => {
            let tool = value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("tool")
                .to_string();
            let call_id = value
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            Some(FleetWorkerEventPayload::RunningTool { tool, call_id })
        }
        "workflow_event" => Some(FleetWorkerEventPayload::WorkflowEvent {
            workflow_run_id: value.get("run_id")?.as_str()?.to_string(),
            event: value.get("event")?.clone(),
        }),
        // Streaming model output / tool results mean the worker is alive and
        // making progress; surface a coarse Running heartbeat.
        "content" | "tool_result" => Some(FleetWorkerEventPayload::Running),
        // Per-step usage receipts feed the run-level accumulator behind the
        // fleet usage ceiling (R6, #5567); a malformed line still counts as
        // liveness.
        "turn_usage" => {
            let tokens = |field: &str| {
                value
                    .get(field)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
            };
            let input_tokens = tokens("input_tokens");
            let output_tokens = tokens("output_tokens");
            if input_tokens == 0 && output_tokens == 0 {
                Some(FleetWorkerEventPayload::Running)
            } else {
                Some(FleetWorkerEventPayload::UsageReport {
                    input_tokens,
                    output_tokens,
                })
            }
        }
        "done" => Some(FleetWorkerEventPayload::Completed {
            exit_code: Some(0),
            summary: None,
        }),
        "error" => {
            let reason = value
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("worker reported an error")
                .to_string();
            Some(FleetWorkerEventPayload::Failed {
                reason,
                recoverable: false,
            })
        }
        _ => None,
    }
}

#[derive(Debug)]
enum ParsedTerminalRoute {
    NotTerminal,
    Valid(FleetWorkerReportedRoute),
    Invalid,
}

/// The `meta` object of a terminal exec receipt, or `None` for every other
/// stream line.
fn exec_terminal_meta(
    value: &serde_json::Value,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    if value.get("type").and_then(serde_json::Value::as_str) != Some("metadata") {
        return None;
    }
    let meta = value.get("meta").and_then(serde_json::Value::as_object)?;
    (meta.get("receipt_kind").and_then(serde_json::Value::as_str) == Some("terminal"))
        .then_some(meta)
}

/// The worker's visible final answer from a terminal exec receipt: the
/// emitter already bounded and redacted `visible_final_answer_excerpt`, and
/// `visible_final_answer_chars` is the real pre-truncation length.
fn parse_exec_terminal_final_answer(value: &serde_json::Value) -> Option<FleetWorkerFinalAnswer> {
    let meta = exec_terminal_meta(value)?;
    let excerpt = meta
        .get("visible_final_answer_excerpt")
        .and_then(serde_json::Value::as_str)?
        .trim();
    if excerpt.is_empty() {
        return None;
    }
    let chars = meta
        .get("visible_final_answer_chars")
        .and_then(serde_json::Value::as_u64)
        .and_then(|chars| usize::try_from(chars).ok())
        .unwrap_or_else(|| excerpt.chars().count());
    Some(FleetWorkerFinalAnswer {
        excerpt: exec_stream_final_answer_excerpt(excerpt),
        chars,
    })
}

/// Parse one allowlisted, secret-free route identity from terminal exec
/// metadata. Once a line declares itself as a terminal receipt, malformed
/// route fields are distinct from ordinary non-terminal stream noise so a
/// prior valid record cannot survive contradictory evidence.
fn parse_exec_terminal_route(value: &serde_json::Value) -> ParsedTerminalRoute {
    let Some(meta) = exec_terminal_meta(value) else {
        return ParsedTerminalRoute::NotTerminal;
    };

    let route = (|| {
        let provider = meta.get("provider")?.as_str()?.trim();
        let model = meta.get("model")?.as_str()?.trim();
        if provider.is_empty() || model.is_empty() {
            return None;
        }
        let provider_kind = crate::config::ProviderKind::parse(provider)?;
        let provider_exact_id = match meta.get("provider_id") {
            None => None,
            Some(value) => {
                let id = value.as_str()?.trim();
                if id.is_empty() {
                    return None;
                }
                Some(id.to_string())
            }
        };
        if provider_exact_id.is_some() && provider_kind != crate::config::ProviderKind::Custom {
            return None;
        }
        Some(FleetWorkerReportedRoute {
            provider: provider.to_string(),
            provider_exact_id,
            model: model.to_string(),
        })
    })();

    route.map_or(ParsedTerminalRoute::Invalid, ParsedTerminalRoute::Valid)
}

#[cfg(test)]
fn map_exec_terminal_route(line: &str) -> Option<FleetWorkerReportedRoute> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    match parse_exec_terminal_route(&value) {
        ParsedTerminalRoute::Valid(route) => Some(route),
        ParsedTerminalRoute::NotTerminal | ParsedTerminalRoute::Invalid => None,
    }
}

/// Classify a worker process exit into a terminal fleet event.
///
/// `stopped` means the operator stopped the worker (cancellation), which takes
/// precedence over the exit code.
pub fn classify_worker_exit(exit_code: Option<i32>, stopped: bool) -> FleetWorkerEventPayload {
    if stopped {
        return FleetWorkerEventPayload::Cancelled { cancelled_by: None };
    }
    match exit_code {
        Some(0) => FleetWorkerEventPayload::Completed {
            exit_code: Some(0),
            summary: None,
        },
        Some(code) => FleetWorkerEventPayload::Failed {
            reason: format!("worker exited with code {code}"),
            recoverable: true,
        },
        None => FleetWorkerEventPayload::Failed {
            reason: "worker exited without a status code".to_string(),
            recoverable: true,
        },
    }
}

/// Drives fleet workers as real `codewhale exec` subprocesses on the local
/// host, incrementally draining each worker's stream-json output into fleet
/// ledger events.
///
/// The caller (the `codewhale fleet run` loop / `FleetManager`) owns the
/// ledger; the executor owns the OS process boundary and the incremental log
/// parse. Because the worker is a separate process, its heavy runtime/tool
/// construction never touches the orchestrator — the parent only ingests a
/// compact event stream, which is what keeps it light at high fanout.
pub struct FleetExecutor {
    workspace: std::path::PathBuf,
    sessions_dir: Option<std::path::PathBuf>,
    adapter: super::host::LocalProcessFleetHostAdapter,
    ssh_adapters: std::collections::BTreeMap<String, super::host::SshFleetHostAdapter>,
    streams: std::collections::BTreeMap<String, WorkerStream>,
}

/// Durable lease identity owned by one concrete host process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetExecutorAttempt {
    pub run_id: codewhale_protocol::fleet::FleetRunId,
    pub task_id: String,
    pub attempt: u32,
}

struct WorkerStream {
    log_path: std::path::PathBuf,
    host: WorkerStreamHost,
    attempt: Option<FleetExecutorAttempt>,
    offset: u64,
    // Keep incomplete stream frames as bytes. Decoding each read separately
    // corrupts valid UTF-8 when a multibyte code point crosses a read boundary.
    pending: Vec<u8>,
    terminal: bool,
    terminal_route: TerminalRouteEvidence,
    /// When this worker process was started, for per-task wall-clock limits (R5).
    started_at: std::time::Instant,
    /// The worker's visible final answer from its terminal exec receipt. This
    /// is the task's deliverable for report/summary work that produces no
    /// file artifact; surfaced as `Completed.summary` and in the receipt note
    /// so receipts stop reporting "no verifiable output" for a worker that
    /// wrote a full report. Bounded by the emitter, so nothing accumulates
    /// here.
    final_answer: Option<FleetWorkerFinalAnswer>,
    /// Saved exec session id reported by the worker's `session_capture` event.
    /// Resolving it via `GET /v1/sessions/{id}` yields the full transcript
    /// (the worker's final assistant reply).
    saved_session_id: Option<String>,
    /// Parent-owned capture identity in the Runtime's existing session store.
    session_capture: Option<(String, std::path::PathBuf)>,
}

impl WorkerStream {
    /// Observe one raw stream-json frame: record route evidence, the final
    /// answer, and the saved-session id, and map it to a ledger payload. The
    /// frame is parsed exactly once.
    fn observe_line(&mut self, line: &[u8]) -> Option<FleetWorkerEventPayload> {
        let Ok(line) = std::str::from_utf8(line) else {
            // stream-json is a UTF-8 contract. Never accept a lossy-decoded route
            // receipt: replacement characters could turn corrupt provider/model
            // bytes into apparently valid provenance.
            self.terminal_route.observe(ParsedTerminalRoute::Invalid);
            return None;
        };
        let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
        self.terminal_route
            .observe(parse_exec_terminal_route(&value));
        if exec_terminal_meta(&value).is_some() {
            self.final_answer = parse_exec_terminal_final_answer(&value);
        }
        if value.get("type").and_then(serde_json::Value::as_str) == Some("session_capture")
            && let Some(id) = value
                .get("saved_session_id")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
        {
            if self
                .session_capture
                .as_ref()
                .is_some_and(|(expected, _)| expected == id)
            {
                self.saved_session_id = Some(id.to_string());
            } else {
                // A worker log cannot select an unrelated local transcript.
                self.session_capture = None;
                self.saved_session_id = None;
            }
        }
        map_exec_stream_value(&value)
    }
}

#[derive(Debug, Clone, Default)]
enum TerminalRouteEvidence {
    #[default]
    Missing,
    Valid(FleetWorkerReportedRoute),
    InvalidOrAmbiguous,
}

impl TerminalRouteEvidence {
    fn observe(&mut self, parsed: ParsedTerminalRoute) {
        match parsed {
            ParsedTerminalRoute::NotTerminal => {}
            ParsedTerminalRoute::Invalid => *self = Self::InvalidOrAmbiguous,
            ParsedTerminalRoute::Valid(route) => {
                *self = if matches!(&*self, Self::Missing) {
                    Self::Valid(route)
                } else {
                    // The stream contract emits exactly one terminal receipt.
                    // Any second record, even an identical one, is ambiguous
                    // provenance and must permanently fail closed.
                    Self::InvalidOrAmbiguous
                };
            }
        }
    }

    fn reported_route(&self) -> Option<&FleetWorkerReportedRoute> {
        match self {
            Self::Valid(route) => Some(route),
            Self::Missing | Self::InvalidOrAmbiguous => None,
        }
    }
}

enum WorkerStreamHost {
    Local,
    Ssh(String),
}

#[derive(Debug, Clone)]
pub struct FleetWorkerReportedRoute {
    pub provider: String,
    pub provider_exact_id: Option<String>,
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct FleetWorkerTerminalEvent {
    pub payload: FleetWorkerEventPayload,
    pub exit_code: Option<i32>,
    /// Non-terminal payloads discovered by the mandatory post-exit drain.
    pub tail_payloads: Vec<FleetWorkerEventPayload>,
    pub reported_route: Option<FleetWorkerReportedRoute>,
    /// The worker's visible final answer from its terminal exec receipt,
    /// whatever the outcome: a worker that fails after writing most of a
    /// report keeps the text on its receipt.
    pub final_answer: Option<FleetWorkerFinalAnswer>,
    /// Saved exec session id reported by the worker's `session_capture` event,
    /// when one was persisted on completion.
    pub saved_session_id: Option<String>,
    /// A real headless exec process must report its actual route. Callers use
    /// this bit to distinguish a missing/invalid report (fail closed) from
    /// pre-launch or simulated paths that only have declared route intent.
    pub requires_reported_route: bool,
}

impl FleetExecutor {
    pub fn new(workspace: impl AsRef<std::path::Path>) -> Self {
        let workspace = workspace.as_ref().to_path_buf();
        Self {
            adapter: super::host::LocalProcessFleetHostAdapter::new(&workspace),
            workspace,
            sessions_dir: None,
            ssh_adapters: std::collections::BTreeMap::new(),
            streams: std::collections::BTreeMap::new(),
        }
    }

    /// Share the caller's SessionManager directory; never create a second store.
    pub fn with_sessions_dir(mut self, sessions_dir: std::path::PathBuf) -> Self {
        self.sessions_dir = Some(sessions_dir);
        self
    }

    /// Start a worker process and begin tracking its event stream.
    pub fn start_worker(
        &mut self,
        worker_id: &str,
        command: FleetWorkerCommand,
        cwd: Option<std::path::PathBuf>,
    ) -> super::host::FleetHostResult<super::host::FleetWorkerHandle> {
        self.start_worker_on_host(worker_id, &FleetHostSpec::Local, command, cwd)
    }

    /// Start a worker on the requested fleet host.
    pub fn start_worker_on_host(
        &mut self,
        worker_id: &str,
        host: &FleetHostSpec,
        command: FleetWorkerCommand,
        cwd: Option<std::path::PathBuf>,
    ) -> super::host::FleetHostResult<super::host::FleetWorkerHandle> {
        self.start_worker_on_host_inner(worker_id, host, command, cwd, None)
    }

    /// Start the concrete process for one exact durable Fleet lease.
    pub fn start_worker_attempt_on_host(
        &mut self,
        worker_id: &str,
        host: &FleetHostSpec,
        command: FleetWorkerCommand,
        cwd: Option<std::path::PathBuf>,
        attempt: FleetExecutorAttempt,
    ) -> super::host::FleetHostResult<super::host::FleetWorkerHandle> {
        self.start_worker_on_host_inner(worker_id, host, command, cwd, Some(attempt))
    }

    fn start_worker_on_host_inner(
        &mut self,
        worker_id: &str,
        host: &FleetHostSpec,
        command: FleetWorkerCommand,
        cwd: Option<std::path::PathBuf>,
        attempt: Option<FleetExecutorAttempt>,
    ) -> super::host::FleetHostResult<super::host::FleetWorkerHandle> {
        let mut request = super::host::FleetWorkerStartRequest::new(worker_id, command);
        request.cwd = cwd;
        let session_capture = if matches!(host, FleetHostSpec::Local) {
            self.sessions_dir.as_ref().map(|dir| {
                let id = uuid::Uuid::new_v4().to_string();
                for (key, value) in [
                    ("CODEWHALE_FLEET_CAPTURE_ID", id.clone()),
                    (
                        "CODEWHALE_FLEET_CAPTURE_DIR",
                        dir.to_string_lossy().into_owned(),
                    ),
                ] {
                    request.env.insert(key.to_string(), value);
                    request.env_allowlist.insert(key.to_string());
                }
                // Preserve an explicit local account/config root as well.
                if let Some(home) = std::env::var_os("CODEWHALE_HOME") {
                    request
                        .env
                        .insert("CODEWHALE_HOME".into(), home.to_string_lossy().into_owned());
                    request.env_allowlist.insert("CODEWHALE_HOME".into());
                }
                (id, dir.clone())
            })
        } else {
            // SSH transcripts live remotely and have no local retrieval link.
            None
        };
        let (handle, host) = match host {
            FleetHostSpec::Local => {
                let handle = self.adapter.start_worker(request)?;
                (handle, WorkerStreamHost::Local)
            }
            FleetHostSpec::Ssh { .. } => {
                let config = super::host::SshFleetHostConfig::from_host_spec(host)?;
                let key = worker_id.to_string();
                let adapter = self.ssh_adapters.entry(key.clone()).or_insert(
                    super::host::SshFleetHostAdapter::new(&self.workspace, config)?,
                );
                let handle = adapter.start_worker(request)?;
                (handle, WorkerStreamHost::Ssh(key))
            }
            FleetHostSpec::Docker { image, .. } => {
                return Err(super::host::FleetHostError {
                    kind: super::host::FleetHostErrorKind::Configuration,
                    message: format!("docker Fleet workers are not wired yet (image {image})"),
                });
            }
        };
        self.streams.insert(
            worker_id.to_string(),
            WorkerStream {
                log_path: handle.log_path.clone(),
                host,
                attempt,
                offset: 0,
                pending: Vec::new(),
                terminal: false,
                terminal_route: TerminalRouteEvidence::default(),
                started_at: std::time::Instant::now(),
                final_answer: None,
                saved_session_id: None,
                session_capture,
            },
        );
        Ok(handle)
    }

    pub fn is_tracking(&self, worker_id: &str) -> bool {
        self.streams.contains_key(worker_id)
    }

    pub fn worker_ids(&self) -> Vec<String> {
        self.streams.keys().cloned().collect()
    }

    pub fn tracked_attempt(&self, worker_id: &str) -> Option<FleetExecutorAttempt> {
        self.streams
            .get(worker_id)
            .and_then(|stream| stream.attempt.clone())
    }

    /// Wall-clock time this worker process has been running (R5). `None` when
    /// the worker is not tracked or its exit was already observed: a process
    /// that has exited is not running, so a deadline must not rewrite the
    /// outcome that exit produced as a timeout.
    #[must_use]
    pub fn worker_running_for(&self, worker_id: &str) -> Option<std::time::Duration> {
        self.streams
            .get(worker_id)
            .filter(|stream| !stream.terminal)
            .map(|stream| stream.started_at.elapsed())
    }

    /// Stop a tracked worker at the host boundary.
    ///
    /// Operator controls run in a separate process from the foreground Fleet
    /// manager, so they communicate cancellation through the durable ledger.
    /// The manager calls this method after observing that terminal state; the
    /// executor is the only owner that can reliably reach the live local/SSH
    /// adapter handle.
    pub fn stop_worker(&mut self, worker_id: &str) -> Result<()> {
        let ssh_key = match self.streams.get(worker_id).map(|stream| &stream.host) {
            Some(WorkerStreamHost::Local) => None,
            Some(WorkerStreamHost::Ssh(key)) => Some(key.clone()),
            None => return Ok(()),
        };
        if let Some(key) = ssh_key {
            let adapter = self.ssh_adapters.get_mut(&key).ok_or_else(|| {
                anyhow::anyhow!("tracked SSH Fleet worker {worker_id} has no host adapter")
            })?;
            adapter.stop_worker(worker_id)?;
        } else {
            self.adapter.stop_worker(worker_id)?;
        }
        Ok(())
    }

    /// Stop tracking a terminal worker so the scheduler can reuse the same
    /// logical worker id for the next queued task.
    pub fn forget_worker(&mut self, worker_id: &str) {
        let Some(stream) = self.streams.remove(worker_id) else {
            return;
        };
        match stream.host {
            WorkerStreamHost::Local => {
                let _ = self.adapter.cleanup_worker(worker_id);
            }
            WorkerStreamHost::Ssh(key) => {
                if let Some(adapter) = self.ssh_adapters.get_mut(&key) {
                    let _ = adapter.cleanup_worker(worker_id);
                }
                self.ssh_adapters.remove(&key);
            }
        }
    }

    /// Maximum bytes drained from a worker log per call, and the cap for the
    /// buffered partial line. Event lines are small; the remainder stays for
    /// the next drain.
    const MAX_DRAIN_BYTES: u64 = 1024 * 1024;

    /// Read any newly-written stream-json lines for a worker and map them to
    /// fleet ledger events. Safe to call repeatedly; only new bytes are parsed,
    /// and a trailing partial line is buffered until its newline arrives.
    pub fn drain_events(&mut self, worker_id: &str) -> Vec<FleetWorkerEventPayload> {
        let Some(stream) = self.streams.get_mut(worker_id) else {
            return Vec::new();
        };
        let mut events = Vec::new();
        let Ok(mut file) = std::fs::File::open(&stream.log_path) else {
            return events;
        };
        use std::io::{Read, Seek, SeekFrom};
        if file.seek(SeekFrom::Start(stream.offset)).is_err() {
            return events;
        }
        let mut buf = Vec::new();
        if let Ok(read) = file.take(Self::MAX_DRAIN_BYTES).read_to_end(&mut buf) {
            stream.offset += read as u64;
            stream.pending.extend_from_slice(&buf);
            while let Some(idx) = stream.pending.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = stream.pending.drain(..=idx).collect();
                if let Some(event) = stream.observe_line(&line) {
                    events.push(event);
                }
            }
            // Whatever remains has no newline; drop it rather than buffering
            // a newline-free flood forever.
            if stream.pending.len() as u64 > Self::MAX_DRAIN_BYTES {
                tracing::debug!(
                    worker_id,
                    dropped_bytes = stream.pending.len(),
                    "fleet drain dropped a newline-free flood exceeding the per-call budget"
                );
                stream.pending.clear();
            }
        }
        events
    }

    /// Poll the worker process; once it exits, return the terminal event exactly
    /// once. Returns `None` while the worker is still running or already
    /// finalized.
    pub fn poll_terminal(&mut self, worker_id: &str) -> Option<FleetWorkerEventPayload> {
        self.poll_terminal_with_status(worker_id)
            .map(|event| event.payload)
    }

    /// Poll the worker process and include the raw exit code for receipt
    /// verification.
    pub fn poll_terminal_with_status(
        &mut self,
        worker_id: &str,
    ) -> Option<FleetWorkerTerminalEvent> {
        if self.streams.get(worker_id).is_none_or(|s| s.terminal) {
            return None;
        }
        let status = match self.streams.get(worker_id).map(|s| &s.host)? {
            WorkerStreamHost::Local => self.adapter.read_status(worker_id).ok()?,
            WorkerStreamHost::Ssh(key) => self
                .ssh_adapters
                .get_mut(key)
                .and_then(|adapter| adapter.read_status(worker_id).ok())?,
        };
        let mut terminal = match status.state {
            super::host::FleetHostWorkerState::Running
            | super::host::FleetHostWorkerState::Draining
            | super::host::FleetHostWorkerState::Unknown => return None,
            super::host::FleetHostWorkerState::Stopped => {
                classify_worker_exit(status.exit_code, true)
            }
            super::host::FleetHostWorkerState::Exited
            | super::host::FleetHostWorkerState::Failed => {
                classify_worker_exit(status.exit_code, false)
            }
        };
        // Once status is terminal the worker can no longer append. Drain one
        // final time before snapshotting route evidence so metadata written
        // between the scheduler's ordinary drain and this status poll cannot
        // be lost when the worker is forgotten.
        let mut tail_payloads = self.drain_events(worker_id);
        let stream = self.streams.get_mut(worker_id)?;
        let trailing_line = std::mem::take(&mut stream.pending);
        if trailing_line.iter().any(|byte| !byte.is_ascii_whitespace())
            && let Some(payload) = stream.observe_line(&trailing_line)
        {
            tail_payloads.push(payload);
        }
        stream.terminal = true;
        let final_answer = stream.final_answer.take();
        // Surface the visible final answer on a successful completion so
        // report/summary tasks (no scorer, no file artifact) show their
        // deliverable instead of "no verifiable output". `Failed` has no
        // summary slot; the receipt keeps the text via `final_answer`.
        if let (Some(answer), FleetWorkerEventPayload::Completed { summary, .. }) =
            (final_answer.as_ref(), &mut terminal)
        {
            *summary = Some(answer.excerpt.clone());
        }
        Some(FleetWorkerTerminalEvent {
            payload: terminal,
            exit_code: status.exit_code,
            tail_payloads,
            reported_route: stream.terminal_route.reported_route().cloned(),
            final_answer,
            saved_session_id: stream.saved_session_id.take().filter(|id| {
                stream
                    .session_capture
                    .as_ref()
                    .is_some_and(|(expected, dir)| {
                        id == expected
                            && crate::session_manager::SessionManager::new(dir.clone())
                                .and_then(|manager| manager.load_session(id))
                                .is_ok()
                    })
            }),
            requires_reported_route: true,
        })
    }

    /// True once every started worker has reached a terminal state.
    pub fn all_terminal(&self) -> bool {
        !self.streams.is_empty() && self.streams.values().all(|s| s.terminal)
    }
}

/// Character bound for `metadata.visible_final_answer_excerpt`. The excerpt
/// is a status surface (fleet receipts, event labels, runtime API payloads),
/// not the transcript: the full answer lives in the saved session and the
/// worker's stream-json log, and `visible_final_answer_chars` carries the real
/// length so a consumer can tell a bounded excerpt from a short answer.
pub(crate) const EXEC_STREAM_FINAL_ANSWER_EXCERPT_CHARS: usize = 4_000;

/// Bound and secret-redact the visible final answer once, at the emitter, so
/// every downstream consumer reads the same excerpt.
pub(crate) fn exec_stream_final_answer_excerpt(output: &str) -> String {
    let redacted = codewhale_config::persistence::redact_secrets(output.trim());
    let mut chars = redacted.chars();
    let excerpt: String = chars
        .by_ref()
        .take(EXEC_STREAM_FINAL_ANSWER_EXCERPT_CHARS)
        .collect();
    if chars.next().is_some() {
        format!("{excerpt}...")
    } else {
        excerpt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_config::{
        FleetDelegationHints, FleetLoadout, FleetProfile, FleetProfilePermissions, FleetRole,
        FleetSlot,
    };
    use codewhale_protocol::fleet::{
        FleetHostSpec, FleetTaskBudget, FleetTaskSpec, FleetTaskWorkerProfile, FleetWorkerSpec,
        FleetWorkspaceRequirements,
    };
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn task(instructions: &str) -> FleetTaskSpec {
        FleetTaskSpec {
            id: "t1".to_string(),
            name: "Smoke".to_string(),
            description: None,
            objective: Some("prove it runs".to_string()),
            instructions: instructions.to_string(),
            worker: Some(FleetTaskWorkerProfile {
                agent_profile: None,
                role: Some("reviewer".to_string()),
                loadout: None,
                model_class: None,
                model: None,
                tool_profile: Some("read-only".to_string()),
                tools: vec![],
                capabilities: vec![],
            }),
            workspace: None,
            input_files: vec![],
            context: vec![],
            budget: None,
            tags: vec![],
            expected_artifacts: vec![],
            scorer: None,
            retry_policy: None,
            alert_policy: None,
            timeout_seconds: None,
            metadata: BTreeMap::new(),
        }
    }

    fn agent_profile(id: &str, role: &str, instructions: &str) -> AgentProfile {
        AgentProfile {
            native_preset: None,
            id: id.to_string(),
            display_name: Some(format!("{role} profile")),
            description: Some(format!("{role} description")),
            requires: Vec::new(),
            profile: FleetProfile {
                slot: FleetSlot::from_name(role),
                role: FleetRole {
                    name: role.to_string(),
                    description: None,
                    instructions: Some(instructions.to_string()),
                },
                loadout: FleetLoadout::Inherit,
                model: None,
                provider: None,
                reasoning_effort: None,
                permissions: FleetProfilePermissions::default(),
                delegation: FleetDelegationHints::default(),
            },
            source: std::path::PathBuf::from(format!("{id}.toml")),
            origin: crate::fleet::roster::ProfileOrigin::Workspace,
            plugin_authority: None,
        }
    }

    fn launch_spec(task: &FleetTaskSpec, workspace: &std::path::Path) -> AgentWorkerSpec {
        let worker = FleetWorkerSpec {
            id: "worker-1".to_string(),
            name: "Worker 1".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: BTreeMap::new(),
            capabilities: Vec::new(),
            max_concurrent_tasks: Some(1),
        };
        crate::fleet::worker_runtime::fleet_task_to_worker_spec_with_profiles(
            "worker-1",
            "run-1",
            task,
            &worker,
            "auto",
            workspace,
            workspace,
            &[],
            None,
        )
        .unwrap()
    }

    fn track_test_stream(
        executor: &mut FleetExecutor,
        worker_id: &str,
        log_path: std::path::PathBuf,
    ) {
        executor.streams.insert(
            worker_id.to_string(),
            WorkerStream {
                log_path,
                host: WorkerStreamHost::Local,
                attempt: None,
                offset: 0,
                pending: Vec::new(),
                terminal: false,
                terminal_route: TerminalRouteEvidence::default(),
                started_at: std::time::Instant::now(),
                final_answer: None,
                saved_session_id: None,
                session_capture: None,
            },
        );
    }

    fn append_test_stream(path: &std::path::Path, bytes: &[u8]) {
        use std::io::Write as _;

        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    #[test]
    fn worker_command_is_a_headless_codewhale_exec_run() {
        let exec = FleetExecConfig::default();
        let cmd = build_worker_exec_command("codewhale", &task("read the file"), &exec, None);
        assert_eq!(cmd.program, "codewhale");
        assert_eq!(cmd.args[0], "exec");
        assert!(cmd.args.contains(&"--auto".to_string()));
        // stream-json so the executor can ingest the worker's event stream.
        let joined = cmd.args.join(" ");
        assert!(joined.contains("--output-format stream-json"));
        // The task instructions ride in the positional prompt (last arg).
        assert!(cmd.args.last().unwrap().contains("read the file"));
    }

    #[test]
    fn worker_command_threads_exec_hardening_flags() {
        let exec = FleetExecConfig {
            allowed_tools: vec!["read_file".to_string(), "grep_files".to_string()],
            disallowed_tools: vec!["exec_shell".to_string()],
            max_turns: 40,
            append_system_prompt: "never push to main".to_string(),
            ..FleetExecConfig::default()
        };
        let cmd = build_worker_exec_command("codewhale", &task("audit"), &exec, Some("glm-5.1"));
        let exec_idx = cmd
            .args
            .iter()
            .position(|arg| arg == "exec")
            .expect("worker command must contain exec");
        let model_idx = cmd
            .args
            .iter()
            .position(|arg| arg == "--model")
            .expect("worker command must contain --model");
        assert!(
            model_idx < exec_idx,
            "global --model must precede exec: {:?}",
            cmd.args
        );
        let joined = cmd.args.join(" ");
        assert!(joined.contains("--model glm-5.1"));
        assert!(joined.contains("--allowed-tools read_file,grep_files"));
        assert!(joined.contains("--disallowed-tools exec_shell"));
        assert!(joined.contains("--max-turns 40"));
        assert!(
            cmd.args
                .iter()
                .any(|a| a == "--append-system-prompt=never push to main")
        );
    }

    #[test]
    fn worker_command_threads_positive_task_budgets_and_caps_steps() {
        let mut task = task("audit");
        task.budget = Some(FleetTaskBudget {
            max_steps: Some(75),
            max_tool_calls: Some(11),
            ..FleetTaskBudget::default()
        });
        let exec = FleetExecConfig {
            max_turns: 40,
            ..FleetExecConfig::default()
        };

        let cmd = build_worker_exec_command("codewhale", &task, &exec, None);
        let max_turns_idx = cmd
            .args
            .iter()
            .position(|arg| arg == "--max-turns")
            .expect("positive max_steps must reach exec");
        let max_tool_calls_idx = cmd
            .args
            .iter()
            .position(|arg| arg == "--max-tool-calls")
            .expect("positive max_tool_calls must reach exec");

        assert_eq!(cmd.args[max_turns_idx + 1], "40");
        assert_eq!(cmd.args[max_tool_calls_idx + 1], "11");
    }

    #[test]
    fn production_worker_command_uses_hardened_launch_spec_steps() {
        let tmp = TempDir::new().unwrap();
        let mut task = task("audit");
        task.budget = Some(FleetTaskBudget {
            max_steps: Some(75),
            max_tool_calls: Some(11),
            ..FleetTaskBudget::default()
        });
        let mut launch_spec = launch_spec(&task, tmp.path());
        // The manager owns this hardening step. A different value here proves
        // production argv comes from the registered spec, not a second budget
        // projection from the task document.
        launch_spec.max_steps = 13;

        let cmd = build_worker_exec_command_with_launch_spec(
            "codewhale",
            &task,
            &launch_spec,
            &FleetExecConfig {
                max_turns: 40,
                ..FleetExecConfig::default()
            },
            None,
            &[],
        )
        .unwrap();
        let max_turns_idx = cmd
            .args
            .iter()
            .position(|arg| arg == "--max-turns")
            .expect("hardened launch max_steps must reach exec");
        let max_tool_calls_idx = cmd
            .args
            .iter()
            .position(|arg| arg == "--max-tool-calls")
            .expect("task max_tool_calls must reach exec");

        assert_eq!(cmd.args[max_turns_idx + 1], "13");
        assert_eq!(cmd.args[max_tool_calls_idx + 1], "11");
    }

    #[test]
    fn worker_command_threads_agent_profile_prompt() {
        let mut task = task("audit");
        task.worker.as_mut().unwrap().agent_profile = Some("reviewer".to_string());
        let cmd = build_worker_exec_command_with_profiles(
            "codewhale",
            &task,
            &FleetExecConfig::default(),
            None,
            &[agent_profile(
                "reviewer",
                "reviewer",
                "Focus on defects, regressions, and missing tests.",
            )],
        )
        .unwrap();
        let prompt = cmd.args.last().unwrap();

        assert!(prompt.contains("Fleet profile: reviewer"));
        assert!(prompt.contains("Focus on defects, regressions, and missing tests."));
    }

    #[test]
    fn consultant_launch_spec_carries_read_only_authority_with_network_reads() {
        let tmp = TempDir::new().unwrap();
        let mut task = task("advise on the release candidate");
        task.worker.as_mut().unwrap().role = Some("consultant".to_string());
        let launch_spec = launch_spec(&task, tmp.path());
        assert_eq!(
            launch_spec.agent_type,
            crate::tools::subagent::FleetRole::Consultant
        );
        // Counsel only: never writes the workspace, but may read the web.
        assert!(!launch_spec.runtime_profile.permissions.write);
        assert!(launch_spec.runtime_profile.permissions.network);

        let cmd = build_worker_exec_command_with_launch_spec(
            "codewhale",
            &task,
            &launch_spec,
            &FleetExecConfig::default(),
            None,
            &[],
        )
        .unwrap();

        assert_eq!(cmd.args.last(), Some(&launch_spec.objective));
        let authority_index = cmd
            .args
            .iter()
            .position(|arg| arg == "--tool-authority-json")
            .expect("launch command must carry machine-readable authority");
        let authority = ToolAuthorityEnvelope::from_json(&cmd.args[authority_index + 1]).unwrap();
        assert_eq!(authority.owner, "worker-1");
        assert_eq!(authority.authority, ToolMutationAuthority::ReadOnly);
        assert_eq!(authority.network_access, Some(true));
        assert_eq!(authority.shell, ToolShellAuthority::None);
        assert_eq!(authority.verification, ToolVerificationAuthority::None);
        assert!(authority.writable_roots.is_empty());
        assert!(authority.writable_files.is_empty());
        assert!(authority.coordination_contracts.is_empty());
    }

    #[test]
    fn scout_reviewer_and_planner_launches_carry_only_read_only_shell_authority() {
        let tmp = TempDir::new().unwrap();
        for role in ["scout", "reviewer", "planner"] {
            let mut task = task("inspect repository and GitHub state");
            task.worker.as_mut().unwrap().role = Some(role.to_string());
            let launch_spec = launch_spec(&task, tmp.path());
            let cmd = build_worker_exec_command_with_launch_spec(
                "codewhale",
                &task,
                &launch_spec,
                &FleetExecConfig::default(),
                None,
                &[],
            )
            .unwrap();
            let authority_index = cmd
                .args
                .iter()
                .position(|arg| arg == "--tool-authority-json")
                .expect("launch command must carry machine-readable authority");
            let authority =
                ToolAuthorityEnvelope::from_json(&cmd.args[authority_index + 1]).unwrap();

            assert_eq!(authority.authority, ToolMutationAuthority::ReadOnly);
            assert_eq!(authority.network_access, Some(true));
            assert_eq!(authority.shell, ToolShellAuthority::ReadOnly);
            assert_eq!(authority.verification, ToolVerificationAuthority::None);
        }
    }

    #[test]
    fn verifier_launch_carries_only_bounded_verification_process_authority() {
        let tmp = TempDir::new().unwrap();
        let mut task = task("run the focused release checks");
        task.worker.as_mut().unwrap().role = Some("verifier".to_string());
        let mut launch_spec = launch_spec(&task, tmp.path());
        let authority = authority_envelope_for_worker(&launch_spec, &task).unwrap();

        assert_eq!(authority.authority, ToolMutationAuthority::ReadOnly);
        assert_eq!(authority.shell, ToolShellAuthority::None);
        assert_eq!(authority.verification, ToolVerificationAuthority::Bounded);

        launch_spec.runtime_profile.shell = crate::worker_profile::ShellPolicy::None;
        let shellless = authority_envelope_for_worker(&launch_spec, &task).unwrap();
        assert_eq!(
            shellless.verification,
            ToolVerificationAuthority::None,
            "the parent shell ceiling also removes bounded verification process authority"
        );
    }

    #[test]
    fn launch_spec_command_preserves_exact_write_scope() {
        let tmp = TempDir::new().unwrap();
        let mut task = task("edit the bounded source tree");
        let worker = task.worker.as_mut().unwrap();
        worker.role = Some("implementer".to_string());
        worker.tool_profile = None;
        task.workspace = Some(FleetWorkspaceRequirements {
            writable_paths: vec![std::path::PathBuf::from("src")],
            ..FleetWorkspaceRequirements::default()
        });
        let launch_spec = launch_spec(&task, tmp.path());

        let cmd = build_worker_exec_command_with_launch_spec(
            "codewhale",
            &task,
            &launch_spec,
            &FleetExecConfig::default(),
            None,
            &[],
        )
        .unwrap();
        let authority_index = cmd
            .args
            .iter()
            .position(|arg| arg == "--tool-authority-json")
            .expect("launch command must carry machine-readable authority");
        let authority = ToolAuthorityEnvelope::from_json(&cmd.args[authority_index + 1]).unwrap();

        assert_eq!(authority.authority, ToolMutationAuthority::ScopedWrite);
        assert_eq!(authority.network_access, Some(true));
        assert_eq!(authority.shell, ToolShellAuthority::None);
        assert_eq!(authority.verification, ToolVerificationAuthority::None);
        assert_eq!(authority.writable_roots, ["src"]);
        assert!(authority.writable_files.is_empty());
        assert!(authority.coordination_contracts.is_empty());
        assert_eq!(cmd.args.last(), Some(&launch_spec.objective));
    }

    /// #4093 AC #4 at the LAUNCH boundary (not just the receipt): a worker whose
    /// profile pins a DIFFERENT provider+model than the parent session must
    /// actually launch on the profile's route and saved reasoning tier. The
    /// parent session is DeepSeek here (`--model deepseek-v4-pro`); the profile
    /// pins OpenRouter + glm-5.2 + max thinking. The emitted argv must carry
    /// OpenRouter's id, the profile's model, and the profile's thinking tier as
    /// paired flag/values — never the parent's model. This is the gap the
    /// save→load→resolve receipt tests never covered.
    #[test]
    fn worker_command_launches_profile_bound_provider_and_model_not_the_parent() {
        let mut task = task("audit");
        task.worker.as_mut().unwrap().agent_profile = Some("cross".to_string());

        let mut profile = agent_profile("cross", "scout", "Read first.");
        profile.profile.provider = Some("openrouter".to_string());
        profile.profile.model = Some("glm-5.2".to_string());
        profile.profile.reasoning_effort = Some("max".to_string());

        let cmd = build_worker_exec_command_with_profiles(
            "codewhale",
            &task,
            &FleetExecConfig::default(),
            Some("deepseek-v4-pro"), // parent/session model on provider A.
            &[profile],
        )
        .unwrap();

        // Assert the flag/value PAIRS, so the provider and model are proven to
        // ride together rather than merely appearing somewhere on the argv.
        let provider_idx = cmd
            .args
            .iter()
            .position(|a| a == "--provider")
            .expect("--provider must be threaded for a provider-pinned worker");
        let exec_idx = cmd
            .args
            .iter()
            .position(|a| a == "exec")
            .expect("worker command must contain exec");
        assert_eq!(
            cmd.args.get(provider_idx + 1).map(String::as_str),
            Some("openrouter"),
            "{:?}",
            cmd.args
        );
        assert!(
            provider_idx < exec_idx,
            "global --provider must precede exec: {:?}",
            cmd.args
        );
        let model_idx = cmd
            .args
            .iter()
            .position(|a| a == "--model")
            .expect("--model must be present");
        assert_eq!(
            cmd.args.get(model_idx + 1).map(String::as_str),
            Some("glm-5.2"),
            "{:?}",
            cmd.args
        );
        assert!(
            model_idx < exec_idx,
            "global --model must precede exec: {:?}",
            cmd.args
        );
        let reasoning_idx = cmd
            .args
            .iter()
            .position(|a| a == "--reasoning-effort")
            .expect("--reasoning-effort must be present for a thinking-pinned worker");
        assert_eq!(
            cmd.args.get(reasoning_idx + 1).map(String::as_str),
            Some("max"),
            "{:?}",
            cmd.args
        );
        assert!(
            reasoning_idx > exec_idx,
            "exec-only --reasoning-effort must follow exec: {:?}",
            cmd.args
        );

        assert_eq!(
            &cmd.args[..exec_idx],
            ["--model", "glm-5.2", "--provider", "openrouter"],
            "route flags must form the complete global prefix: {:?}",
            cmd.args
        );
        assert_eq!(
            &cmd.args[exec_idx..exec_idx + 5],
            [
                "exec",
                "--auto",
                "--output-format",
                "stream-json",
                "--parent-death-watch"
            ],
            "exec flags must remain behind the subcommand: {:?}",
            cmd.args
        );

        // The parent/session model must NOT leak onto the argv.
        assert!(
            !cmd.args.iter().any(|a| a == "deepseek-v4-pro"),
            "parent model leaked into a profile-pinned worker's argv: {:?}",
            cmd.args
        );
    }

    #[test]
    fn worker_command_threads_custom_profile_provider_name() {
        let mut task = task("format");
        task.worker.as_mut().unwrap().agent_profile = Some("local".to_string());

        let mut profile = agent_profile("local", "formatter", "Keep edits tight.");
        profile.profile.provider = Some("lm-studio".to_string());
        profile.profile.model = Some("qwen-2.5-7b".to_string());

        let cmd = build_worker_exec_command_with_profiles(
            "codewhale",
            &task,
            &FleetExecConfig::default(),
            Some("deepseek-v4-pro"),
            &[profile],
        )
        .unwrap();

        let provider_idx = cmd
            .args
            .iter()
            .position(|a| a == "--provider")
            .expect("--provider must be threaded for a custom provider pin");
        assert_eq!(
            cmd.args.get(provider_idx + 1).map(String::as_str),
            Some("lm-studio"),
            "{:?}",
            cmd.args
        );
        let exec_idx = cmd
            .args
            .iter()
            .position(|a| a == "exec")
            .expect("worker command must contain exec");
        assert!(
            provider_idx < exec_idx,
            "global --provider must precede exec: {:?}",
            cmd.args
        );
        let model_idx = cmd
            .args
            .iter()
            .position(|a| a == "--model")
            .expect("--model must be present");
        assert_eq!(
            cmd.args.get(model_idx + 1).map(String::as_str),
            Some("qwen-2.5-7b"),
            "{:?}",
            cmd.args
        );
        assert!(
            model_idx < exec_idx,
            "global --model must precede exec: {:?}",
            cmd.args
        );
    }

    /// A worker with no profile-bound provider preserves today's behavior: the
    /// run-level model on `--model`, and NO `--provider` (the worker keeps its
    /// own session default). Guards against regressing profile-less workers.
    #[test]
    fn worker_command_without_profile_provider_omits_provider_and_keeps_run_model() {
        let cmd = build_worker_exec_command_with_profiles(
            "codewhale",
            &task("read"),
            &FleetExecConfig::default(),
            Some("deepseek-v4-pro"),
            &[],
        )
        .unwrap();

        assert!(
            !cmd.args.iter().any(|a| a == "--provider"),
            "profile-less worker must not carry --provider: {:?}",
            cmd.args
        );
        assert!(
            !cmd.args.iter().any(|a| a == "--reasoning-effort"),
            "profile-less worker must not carry --reasoning-effort: {:?}",
            cmd.args
        );
        let model_idx = cmd
            .args
            .iter()
            .position(|a| a == "--model")
            .expect("--model must be present");
        assert_eq!(
            cmd.args.get(model_idx + 1).map(String::as_str),
            Some("deepseek-v4-pro"),
            "{:?}",
            cmd.args
        );
        let exec_idx = cmd
            .args
            .iter()
            .position(|a| a == "exec")
            .expect("worker command must contain exec");
        assert!(
            model_idx < exec_idx,
            "global --model must precede exec: {:?}",
            cmd.args
        );
    }

    #[test]
    fn zero_max_turns_is_not_passed() {
        // Zero task budgets and max_turns mean "no cap"; neither flag should
        // appear in the command.
        let exec = FleetExecConfig {
            max_turns: 0,
            ..Default::default()
        };
        let mut task = task("x");
        task.budget = Some(FleetTaskBudget {
            max_steps: Some(0),
            max_tool_calls: Some(0),
            ..FleetTaskBudget::default()
        });
        let cmd = build_worker_exec_command("codewhale", &task, &exec, None);
        assert!(!cmd.args.join(" ").contains("--max-turns"));
        assert!(!cmd.args.join(" ").contains("--max-tool-calls"));
    }

    #[test]
    fn stream_line_maps_tool_use_to_running_tool() {
        let line = r#"{"type":"tool_use","name":"read_file","id":"call-7","input":{}}"#;
        match map_exec_stream_line(line) {
            Some(FleetWorkerEventPayload::RunningTool { tool, call_id }) => {
                assert_eq!(tool, "read_file");
                assert_eq!(call_id.as_deref(), Some("call-7"));
            }
            other => panic!("expected RunningTool, got {other:?}"),
        }
    }

    #[test]
    fn stream_line_maps_done_and_error() {
        assert!(matches!(
            map_exec_stream_line(r#"{"type":"done"}"#),
            Some(FleetWorkerEventPayload::Completed { .. })
        ));
        match map_exec_stream_line(r#"{"type":"error","error":"boom"}"#) {
            Some(FleetWorkerEventPayload::Failed { reason, .. }) => assert_eq!(reason, "boom"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn stream_line_maps_workflow_receipt_to_typed_event() {
        let line =
            r#"{"type":"workflow_event","run_id":"workflow_1","event":{"type":"task_completed"}}"#;
        match map_exec_stream_line(line) {
            Some(FleetWorkerEventPayload::WorkflowEvent {
                workflow_run_id,
                event,
            }) => {
                assert_eq!(workflow_run_id, "workflow_1");
                assert_eq!(event["type"], "task_completed");
            }
            other => panic!("expected typed workflow receipt, got {other:?}"),
        }
    }

    #[test]
    fn stream_line_ignores_noise_and_bad_json() {
        assert!(map_exec_stream_line(r#"{"type":"session_capture","content":"x"}"#).is_none());
        assert!(map_exec_stream_line("not json").is_none());
        assert!(map_exec_stream_line("").is_none());
    }

    #[test]
    fn terminal_route_keeps_exact_literal_custom_distinct_from_idless_root_and_redacts() {
        let exact = map_exec_terminal_route(
            r#"{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"custom","provider_id":"custom","model":"literal-model","base_url":"https://must-not-cross.invalid/v1","api_key":"sk-must-not-cross"}}"#,
        )
        .expect("literal custom terminal route");
        assert_eq!(exact.provider, "custom");
        assert_eq!(exact.provider_exact_id.as_deref(), Some("custom"));
        assert_eq!(exact.model, "literal-model");

        let root = map_exec_terminal_route(
            r#"{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"custom","model":"root-model"}}"#,
        )
        .expect("idless root custom terminal route");
        assert_eq!(root.provider, "custom");
        assert_eq!(root.provider_exact_id, None);
        assert_eq!(root.model, "root-model");

        let named = map_exec_terminal_route(
            r#"{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"custom","provider_id":"lm-studio","model":"local-model"}}"#,
        )
        .expect("named custom terminal route");
        assert_eq!(named.provider, "custom");
        assert_eq!(named.provider_exact_id.as_deref(), Some("lm-studio"));
        assert_eq!(named.model, "local-model");

        let reported = format!("{exact:?}").to_ascii_lowercase();
        for forbidden in ["base_url", "https://", "api_key", "sk-must-not-cross"] {
            assert!(
                !reported.contains(forbidden),
                "allowlisted terminal route leaked {forbidden:?}: {reported}"
            );
        }

        for malformed in [
            r#"{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"custom","provider_id":"","model":"root-model"}}"#,
            r#"{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"custom","provider_id":"   ","model":"root-model"}}"#,
            r#"{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"custom","provider_id":7,"model":"root-model"}}"#,
            r#"{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"deepseek","provider_id":"custom-x","model":"deepseek-v4-pro"}}"#,
            r#"{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"unknown-kind","model":"unknown-model"}}"#,
        ] {
            assert!(
                map_exec_terminal_route(malformed).is_none(),
                "malformed present exact id must not collapse to idless root: {malformed}"
            );
        }
    }

    #[test]
    fn terminal_route_evidence_requires_exactly_one_valid_envelope() {
        let route_x = r#"{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"custom","provider_id":"remote-x","model":"worker-model-x"}}"#;
        let route_y = r#"{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"custom","provider_id":"remote-y","model":"worker-model-y"}}"#;
        let malformed = r#"{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"custom","provider_id":"","model":"worker-model-x"}}"#;
        let noise = r#"{"type":"content","delta":"progress"}"#;

        let observe = |lines: &[&str]| {
            let mut evidence = TerminalRouteEvidence::default();
            for line in lines {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                evidence.observe(parse_exec_terminal_route(&value));
            }
            evidence.reported_route().cloned()
        };

        let only = observe(&[noise, route_x]).expect("one valid route");
        assert_eq!(only.provider_exact_id.as_deref(), Some("remote-x"));
        assert!(
            observe(&[route_x, malformed]).is_none(),
            "valid then malformed must invalidate stale evidence"
        );
        assert!(
            observe(&[malformed, route_x]).is_none(),
            "malformed then valid must remain invalid"
        );
        assert!(
            observe(&[route_x, route_y]).is_none(),
            "conflicting valid routes must be ambiguous"
        );
        assert!(
            observe(&[route_x, route_x]).is_none(),
            "even identical duplicates violate the exactly-one contract"
        );
    }

    #[test]
    fn exit_classification() {
        assert!(matches!(
            classify_worker_exit(Some(0), false),
            FleetWorkerEventPayload::Completed { .. }
        ));
        assert!(matches!(
            classify_worker_exit(Some(1), false),
            FleetWorkerEventPayload::Failed {
                recoverable: true,
                ..
            }
        ));
        assert!(matches!(
            classify_worker_exit(Some(0), true),
            FleetWorkerEventPayload::Cancelled { .. }
        ));
    }

    /// End-to-end: run a REAL subprocess that emits stream-json (standing in for
    /// `codewhale exec`), and prove the executor drains its events and terminal
    /// exit through the real host adapter — no codewhale binary needed. This is
    /// the verifiable proof that a fleet worker is an out-of-process exec run.
    #[cfg(unix)]
    #[test]
    fn executor_runs_real_process_and_drains_stream_json_into_ledger_events() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut exec = FleetExecutor::new(tmp.path());
        let script = r#"printf '{"type":"tool_use","name":"read_file","id":"c1","input":{}}\n'; printf '{"type":"done"}\n'"#;
        let command = FleetWorkerCommand::new("sh", vec!["-c".to_string(), script.to_string()]);
        exec.start_worker("w1", command, None).unwrap();

        let mut events = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            events.extend(exec.drain_events("w1"));
            if let Some(term) = exec.poll_terminal("w1") {
                events.extend(exec.drain_events("w1")); // final flush after exit
                events.push(term);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not terminate; events so far: {events:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        assert!(
            events.iter().any(|e| matches!(
                e,
                FleetWorkerEventPayload::RunningTool { tool, .. } if tool == "read_file"
            )),
            "expected a RunningTool(read_file) event, got {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, FleetWorkerEventPayload::Completed { .. })),
            "expected a terminal Completed event, got {events:?}"
        );
        assert!(exec.all_terminal());
    }

    #[cfg(unix)]
    fn run_worker_to_terminal(script: &str, worker_id: &str) -> FleetWorkerTerminalEvent {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut exec = FleetExecutor::new(tmp.path());
        let command = FleetWorkerCommand::new("sh", vec!["-c".to_string(), script.to_string()]);
        exec.start_worker(worker_id, command, None).unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            exec.drain_events(worker_id);
            if let Some(term) = exec.poll_terminal_with_status(worker_id) {
                break term;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not terminate in time"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[cfg(unix)]
    #[test]
    fn completed_worker_surfaces_terminal_final_answer_as_summary() {
        // Report/summary tasks produce their deliverable as the final
        // assistant reply, not a file artifact. The exec side emits a bounded
        // excerpt plus the real length on its terminal receipt; the executor
        // reads that (never the streamed `content` deltas, which are the run
        // thinking out loud) and attaches it to `Completed.summary` and the
        // terminal event so a receipt can show the actual result.
        let script = r#"printf '%s\n' '{"type":"content","content":"let me look first"}' '{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"custom","provider_id":"remote-x","model":"worker-model","visible_final_answer_chars":9000,"visible_final_answer_excerpt":"the report..."}}' '{"type":"done"}'"#;
        let terminal = run_worker_to_terminal(script, "w1");

        match &terminal.payload {
            FleetWorkerEventPayload::Completed { summary, .. } => {
                assert_eq!(summary.as_deref(), Some("the report..."));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        assert_eq!(
            terminal.final_answer,
            Some(FleetWorkerFinalAnswer {
                excerpt: "the report...".to_string(),
                chars: 9000,
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_worker_keeps_terminal_final_answer_on_terminal_event() {
        // A worker that fails after writing most of a report still reports
        // its visible answer on the terminal receipt; the executor keeps it
        // on the terminal event so the receipt can retain the text.
        let script = r#"printf '%s\n' '{"type":"error","error":"boom"}' '{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"custom","provider_id":"remote-x","model":"worker-model","visible_final_answer_chars":12,"visible_final_answer_excerpt":"partial text"}}'; exit 1"#;
        let terminal = run_worker_to_terminal(script, "w-failed");

        assert!(
            matches!(terminal.payload, FleetWorkerEventPayload::Failed { .. }),
            "{:?}",
            terminal.payload
        );
        assert_eq!(
            terminal
                .final_answer
                .as_ref()
                .map(|answer| answer.excerpt.as_str()),
            Some("partial text")
        );
    }

    #[cfg(unix)]
    #[test]
    fn unbound_worker_session_claim_is_not_advertised() {
        // The worker persists its full transcript as a saved session and
        // reports the recoverable id via `session_capture.saved_session_id`.
        // The executor must capture that id on the terminal event so a caller
        // can resolve the final assistant reply through `GET /v1/sessions/{id}`.
        let script = r#"printf '%s\n' '{"type":"session_capture","content":"<redacted:log-only>","saved_session_id":"session-abc"}' '{"type":"done"}'"#;
        let terminal = run_worker_to_terminal(script, "w-session");

        assert!(terminal.saved_session_id.is_none());
        assert!(terminal.final_answer.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn worker_session_capture_requires_the_parent_id_and_a_saved_local_transcript() {
        for (persist, forged) in [(true, false), (false, false), (true, true)] {
            let tmp = tempfile::TempDir::new().unwrap();
            let sessions_dir = tmp.path().join("runtime-sessions");
            let manager =
                crate::session_manager::SessionManager::new(sessions_dir.clone()).unwrap();
            let mut executor = FleetExecutor::new(tmp.path()).with_sessions_dir(sessions_dir);
            let script = if forged {
                r#"printf '{"type":"session_capture","saved_session_id":"%s"}\n' "$CODEWHALE_FLEET_CAPTURE_ID"; printf '%s\n' '{"type":"session_capture","saved_session_id":"unrelated-session"}'"#
            } else {
                r#"printf '{"type":"session_capture","saved_session_id":"%s"}\n' "$CODEWHALE_FLEET_CAPTURE_ID""#
            };
            executor
                .start_worker(
                    "capture-worker",
                    FleetWorkerCommand::new("sh", ["-c", script]),
                    None,
                )
                .unwrap();
            let expected = executor.streams["capture-worker"]
                .session_capture
                .as_ref()
                .unwrap()
                .0
                .clone();
            if persist {
                let saved = crate::session_manager::create_saved_session_with_id_and_mode(
                    expected.clone(),
                    &[],
                    "fixture-model",
                    tmp.path(),
                    0,
                    None,
                    Some("exec"),
                );
                manager.save_session(&saved).unwrap();
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let terminal = loop {
                if let Some(terminal) = executor.poll_terminal_with_status("capture-worker") {
                    break terminal;
                }
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(20));
            };
            assert_eq!(
                terminal.saved_session_id,
                (persist && !forged).then_some(expected)
            );
        }
    }

    #[test]
    fn terminal_final_answer_is_bounded_and_redacted_at_the_worker_boundary() {
        let answer = parse_exec_terminal_final_answer(&serde_json::json!({
            "type": "metadata", "meta": { "receipt_kind": "terminal",
                "visible_final_answer_excerpt": format!("sk-ant-must-not-leak-1234567890 {}", "x".repeat(8000)),
                "visible_final_answer_chars": 9000,
            }
        })).unwrap();
        assert!(!answer.excerpt.contains("sk-ant-must-not-leak"));
        assert!(
            answer.excerpt.chars().count() <= super::EXEC_STREAM_FINAL_ANSWER_EXCERPT_CHARS + 3
        );
        assert_eq!(answer.chars, 9000);
    }

    #[test]
    fn terminal_final_answer_ignores_empty_and_nonterminal_receipts() {
        let parse =
            |line: &str| parse_exec_terminal_final_answer(&serde_json::from_str(line).unwrap());
        assert!(parse(r#"{"type":"content","content":"streamed"}"#).is_none());
        assert!(
            parse(r#"{"type":"metadata","meta":{"receipt_kind":"turn","visible_final_answer_excerpt":"x"}}"#)
                .is_none()
        );
        assert!(
            parse(r#"{"type":"metadata","meta":{"receipt_kind":"terminal","visible_final_answer_excerpt":"   "}}"#)
                .is_none()
        );
        // A receipt without the count falls back to the excerpt length.
        assert_eq!(
            parse(
                r#"{"type":"metadata","meta":{"receipt_kind":"terminal","visible_final_answer_excerpt":"héllo"}}"#
            ),
            Some(FleetWorkerFinalAnswer {
                excerpt: "héllo".to_string(),
                chars: 5,
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn terminal_poll_final_drains_route_metadata_and_tail_payloads() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut exec = FleetExecutor::new(tmp.path());
        let script = r#"printf '%s\n' '{"type":"content","delta":"tail progress"}'; printf '%s' '{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"custom","provider_id":"remote-x","model":"worker-model-x"}}'"#;
        let command = FleetWorkerCommand::new("sh", vec!["-c".to_string(), script.to_string()]);
        exec.start_worker("tail-worker", command, None).unwrap();

        // Deliberately do not call the ordinary event drain. Poll only after
        // exit, reproducing the scheduler gap where the previous poll saw EOF
        // just before the worker wrote its terminal tail.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let terminal = loop {
            if let Some(terminal) = exec.poll_terminal_with_status("tail-worker") {
                break terminal;
            }
            assert!(std::time::Instant::now() < deadline, "terminal worker");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let route = terminal.reported_route.expect("final-drained route");
        assert_eq!(route.provider, "custom");
        assert_eq!(route.provider_exact_id.as_deref(), Some("remote-x"));
        assert_eq!(route.model, "worker-model-x");
        assert!(
            terminal
                .tail_payloads
                .iter()
                .any(|payload| matches!(payload, FleetWorkerEventPayload::Running))
        );
    }

    #[cfg(unix)]
    #[test]
    fn terminal_poll_trailing_malformed_route_invalidates_prior_valid_route() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut exec = FleetExecutor::new(tmp.path());
        let script = r#"printf '%s\n' '{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"custom","provider_id":"remote-x","model":"worker-model-x"}}'; printf '%s' '{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"custom","provider_id":"","model":"worker-model-x"}}'"#;
        let command = FleetWorkerCommand::new("sh", vec!["-c".to_string(), script.to_string()]);
        exec.start_worker("ambiguous-tail-worker", command, None)
            .unwrap();

        // Keep all output for the terminal drain, but wait for real worker
        // exit instead of assuming the shell finishes within one timer tick.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let terminal = loop {
            if let Some(terminal) = exec.poll_terminal_with_status("ambiguous-tail-worker") {
                break terminal;
            }
            assert!(std::time::Instant::now() < deadline, "terminal worker");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(
            terminal.reported_route.is_none(),
            "malformed trailing terminal evidence must invalidate the prior valid route"
        );
    }

    /// Dogfood smoke (#3166): several concurrent exec-style workers with one
    /// injected failure. Proves the executor drives a small fleet to terminal
    /// outcomes and that a failing worker is classified distinctly from the
    /// passing ones — all without the codewhale binary.
    #[cfg(unix)]
    #[test]
    fn executor_drives_concurrent_workers_with_injected_failure() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut exec = FleetExecutor::new(tmp.path());

        // Three healthy workers emit a tool_use + done; one injected-failure
        // worker emits an error event and exits non-zero.
        let ok = r#"printf '{"type":"tool_use","name":"grep_files","id":"c","input":{}}\n{"type":"done"}\n'"#;
        let bad = r#"printf '{"type":"error","error":"injected failure"}\n'; exit 7"#;
        for id in ["w1", "w2", "w3"] {
            exec.start_worker(
                id,
                FleetWorkerCommand::new("sh", vec!["-c".to_string(), ok.to_string()]),
                None,
            )
            .unwrap();
        }
        exec.start_worker(
            "w-fail",
            FleetWorkerCommand::new("sh", vec!["-c".to_string(), bad.to_string()]),
            None,
        )
        .unwrap();

        let ids = ["w1", "w2", "w3", "w-fail"];
        let mut terminals: std::collections::BTreeMap<&str, FleetWorkerEventPayload> =
            std::collections::BTreeMap::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while terminals.len() < ids.len() {
            for id in ids {
                let _ = exec.drain_events(id);
                if let Some(term) = exec.poll_terminal(id) {
                    terminals.insert(id, term);
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "not all workers terminated: {terminals:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        assert!(exec.all_terminal());
        for id in ["w1", "w2", "w3"] {
            assert!(
                matches!(terminals[id], FleetWorkerEventPayload::Completed { .. }),
                "{id} should pass, got {:?}",
                terminals[id]
            );
        }
        assert!(
            matches!(terminals["w-fail"], FleetWorkerEventPayload::Failed { .. }),
            "injected-failure worker should fail, got {:?}",
            terminals["w-fail"]
        );
    }

    #[test]
    fn terminal_route_preserves_multibyte_identity_across_read_boundaries() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_path = tmp.path().join("split-utf8.jsonl");
        std::fs::write(&log_path, []).unwrap();
        let mut executor = FleetExecutor::new(tmp.path());
        track_test_stream(&mut executor, "split-utf8", log_path.clone());

        let provider_id = "深海鲸-供应商";
        let model = "深潜-模型";
        let line = format!(
            "{{\"type\":\"metadata\",\"meta\":{{\"receipt_kind\":\"terminal\",\"provider\":\"custom\",\"provider_id\":\"{provider_id}\",\"model\":\"{model}\"}}}}\n"
        );
        let bytes = line.as_bytes();
        let provider_start = bytes
            .windows("鲸".len())
            .position(|window| window == "鲸".as_bytes())
            .unwrap();
        let model_start = bytes
            .windows("潜".len())
            .position(|window| window == "潜".as_bytes())
            .unwrap();
        let provider_split = provider_start + 1;
        let model_split = model_start + 2;

        append_test_stream(&log_path, &bytes[..provider_split]);
        assert!(executor.drain_events("split-utf8").is_empty());
        append_test_stream(&log_path, &bytes[provider_split..model_split]);
        assert!(executor.drain_events("split-utf8").is_empty());
        append_test_stream(&log_path, &bytes[model_split..]);
        assert!(executor.drain_events("split-utf8").is_empty());

        let route = executor
            .streams
            .get("split-utf8")
            .and_then(|stream| stream.terminal_route.reported_route())
            .expect("one exact terminal route");
        assert_eq!(route.provider, "custom");
        assert_eq!(route.provider_exact_id.as_deref(), Some(provider_id));
        assert_eq!(route.model, model);
    }

    #[test]
    fn invalid_utf8_terminal_route_fails_closed_without_lossy_identity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_path = tmp.path().join("invalid-utf8.jsonl");
        let mut line = br#"{"type":"metadata","meta":{"receipt_kind":"terminal","provider":"custom","provider_id":"remote-x","model":"worker-model"}}"#.to_vec();
        let invalid_at = line
            .windows(b"remote-x".len())
            .position(|window| window == b"remote-x")
            .unwrap()
            + 3;
        line[invalid_at] = 0xff;
        line.push(b'\n');
        std::fs::write(&log_path, line).unwrap();

        let mut executor = FleetExecutor::new(tmp.path());
        track_test_stream(&mut executor, "invalid-utf8", log_path);
        assert!(executor.drain_events("invalid-utf8").is_empty());
        assert!(matches!(
            executor
                .streams
                .get("invalid-utf8")
                .map(|stream| &stream.terminal_route),
            Some(TerminalRouteEvidence::InvalidOrAmbiguous)
        ));
    }

    #[test]
    fn invalid_utf8_nonterminal_line_cannot_synthesize_route_evidence() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_path = tmp.path().join("invalid-nonterminal.jsonl");
        let mut line = br#"{"type":"content","delta":"ordinary-output"}"#.to_vec();
        let invalid_at = line
            .windows(b"ordinary-output".len())
            .position(|window| window == b"ordinary-output")
            .unwrap()
            + 4;
        line[invalid_at] = 0xff;
        line.push(b'\n');
        std::fs::write(&log_path, line).unwrap();

        let mut executor = FleetExecutor::new(tmp.path());
        track_test_stream(&mut executor, "invalid-nonterminal", log_path);
        assert!(executor.drain_events("invalid-nonterminal").is_empty());
        assert!(
            executor
                .streams
                .get("invalid-nonterminal")
                .and_then(|stream| stream.terminal_route.reported_route())
                .is_none()
        );
    }
}
