//! Parallel verifier ensemble tool: `run_verifiers`.
//!
//! This is the agent-facing path for "parallelize the verifier, not the
//! generator": one tool call fans out to independent project checks across
//! common ecosystems and returns a single structured verdict.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shlex::try_join;

use codewhale_dependencies::ExternalTool;

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

const DEFAULT_MAX_PYTHON_FILES: usize = 200;
const MAX_CUSTOM_GATES: usize = 12;
const BACKGROUND_GATE_TIMEOUT_MS: u64 = 600_000;
/// Wall-clock bound for a built-in (detected) foreground gate unless the call
/// sets `timeout_ms`.
const DEFAULT_GATE_TIMEOUT_MS: u64 = 600_000;
/// Wall-clock bound for a custom foreground gate when `timeout_ms` is absent.
/// Custom gates run arbitrary programs (headless browsers, servers) that can
/// finish their work and never exit, so the default is deliberately tighter.
const DEFAULT_CUSTOM_GATE_TIMEOUT_MS: u64 = 180_000;
const MAX_GATE_TIMEOUT_MS: u64 = 1_800_000;

/// Wall-clock bounds a foreground gate plan is built with. Background gates
/// are shell jobs, which the shell manager does not bound; they run until they
/// finish or are cancelled through task state, so `timeout_ms` is refused there.
#[derive(Debug, Clone, Copy)]
struct GateTimeouts {
    /// Bound for every built-in (detected) gate.
    builtin_ms: u64,
    /// Bound for a custom gate that does not set its own `timeout_ms`.
    custom_default_ms: u64,
}

impl GateTimeouts {
    /// A custom gate that never exits (a headless browser that wrote its
    /// output) holds the turn, so its default is tighter than a built-in's.
    const DEFAULT: Self = Self {
        builtin_ms: DEFAULT_GATE_TIMEOUT_MS,
        custom_default_ms: DEFAULT_CUSTOM_GATE_TIMEOUT_MS,
    };

    /// `timeout_ms` on the call overrides the built-in gates' bound.
    fn for_call(builtin_override_ms: Option<u64>) -> Result<Self, ToolError> {
        let mut timeouts = Self::DEFAULT;
        if let Some(ms) = builtin_override_ms {
            check_gate_timeout("timeout_ms", ms)?;
            timeouts.builtin_ms = ms;
        }
        Ok(timeouts)
    }
}

fn check_gate_timeout(field: &str, ms: u64) -> Result<(), ToolError> {
    if (1..=MAX_GATE_TIMEOUT_MS).contains(&ms) {
        Ok(())
    } else {
        Err(ToolError::invalid_input(format!(
            "{field} must be between 1 and {MAX_GATE_TIMEOUT_MS}"
        )))
    }
}
/// Bytes of a gate stream kept in memory from its start, and from its end,
/// once the stream is longer than both together. Every byte also goes to a
/// session artifact, so the middle stays readable (#6508). What the model
/// sees of the result is the engine's one recoverable budget.
const GATE_CAPTURE_HEAD_BYTES: usize = 512 * 1024;
const GATE_CAPTURE_TAIL_BYTES: usize = 512 * 1024;
/// After a gate's own process exits, how long a helper it started may keep
/// stdout/stderr open before the gate's process group is killed.
const GATE_PIPE_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Tool for running independent verifier gates concurrently.
pub struct RunVerifiersTool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum VerifierProfile {
    Auto,
    Rust,
    Node,
    Python,
    Go,
}

impl VerifierProfile {
    fn parse(raw: &str) -> Result<Self, ToolError> {
        match raw {
            "auto" => Ok(Self::Auto),
            "rust" => Ok(Self::Rust),
            "node" => Ok(Self::Node),
            "python" => Ok(Self::Python),
            "go" => Ok(Self::Go),
            other => Err(ToolError::invalid_input(format!(
                "Unsupported profile '{other}'. Expected one of: auto, rust, node, python, go"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Rust => "rust",
            Self::Node => "node",
            Self::Python => "python",
            Self::Go => "go",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum VerifierLevel {
    Quick,
    Full,
}

impl VerifierLevel {
    fn parse(raw: &str) -> Result<Self, ToolError> {
        match raw {
            "quick" => Ok(Self::Quick),
            "full" => Ok(Self::Full),
            other => Err(ToolError::invalid_input(format!(
                "Unsupported level '{other}'. Expected one of: quick, full"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Quick => "quick",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RunVerifiersInput {
    profile: String,
    level: String,
    max_python_files: usize,
    commands: Vec<CustomVerifierInput>,
    background: bool,
    cwd: Option<String>,
    timeout_ms: Option<u64>,
}

impl Default for RunVerifiersInput {
    fn default() -> Self {
        Self {
            profile: "auto".to_string(),
            level: "quick".to_string(),
            max_python_files: DEFAULT_MAX_PYTHON_FILES,
            commands: Vec::new(),
            background: false,
            cwd: None,
            timeout_ms: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CustomVerifierInput {
    name: String,
    program: String,
    args: Vec<String>,
    cwd: Option<String>,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Clone)]
struct VerifierGate {
    name: String,
    ecosystem: String,
    cwd: PathBuf,
    program: Option<String>,
    args: Vec<String>,
    env: Vec<(String, String)>,
    skipped_reason: Option<String>,
    timeout: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GateResult {
    name: String,
    ecosystem: String,
    status: GateStatus,
    command: String,
    cwd: String,
    exit_code: Option<i32>,
    duration_ms: u64,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
    /// Session artifact (`art_<id>`) holding the whole stream when it was
    /// longer than what the result keeps in memory (#6508).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stdout_log_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stderr_log_ref: Option<String>,
    skipped_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GateStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum VerifierVerdict {
    Pass,
    Partial,
    Fail,
}

impl VerifierVerdict {
    fn from_counts(gate_count: usize, failed: usize, skipped: usize) -> Self {
        if failed > 0 {
            Self::Fail
        } else if skipped > 0 || gate_count == 0 {
            Self::Partial
        } else {
            Self::Pass
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunVerifiersOutput {
    success: bool,
    profile: String,
    level: String,
    workspace: String,
    gate_count: usize,
    passed: usize,
    failed: usize,
    skipped: usize,
    verifier_verdict: VerifierVerdict,
    summary: String,
    gates: Vec<GateResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackgroundGateJob {
    name: String,
    ecosystem: String,
    status: String,
    command: String,
    cwd: String,
    task_id: Option<String>,
    skipped_reason: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunVerifiersBackgroundOutput {
    success: bool,
    profile: String,
    level: String,
    workspace: String,
    background: bool,
    gate_count: usize,
    started: usize,
    skipped: usize,
    failed_to_start: usize,
    summary: String,
    jobs: Vec<BackgroundGateJob>,
}

#[async_trait]
impl ToolSpec for RunVerifiersTool {
    fn name(&self) -> &'static str {
        "run_verifiers"
    }

    fn model_visible(&self) -> bool {
        false
    }

    fn description(&self) -> &'static str {
        "Run independent verifier gates in parallel across detected Rust, Node, Python, and Go projects. Supports explicit custom verifier commands as program+args without requiring Bash."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "profile": {
                    "type": "string",
                    "enum": ["auto", "rust", "node", "python", "go"],
                    "default": "auto",
                    "description": "Which ecosystem verifier set to run. 'auto' detects all supported project types in the workspace."
                },
                "level": {
                    "type": "string",
                    "enum": ["quick", "full"],
                    "default": "quick",
                    "description": "Quick runs fast syntax/drift/build checks. Full adds heavier test/lint gates where available."
                },
                "max_python_files": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 1000,
                    "default": DEFAULT_MAX_PYTHON_FILES,
                    "description": "Maximum Python files to syntax-parse in the built-in python-syntax gate."
                },
                "commands": {
                    "type": "array",
                    "description": "Optional explicit verifier gates. Commands run directly as program+args, not through a shell. Use program='bash', args=['-lc', '...'] only when Bash is intentionally part of the verifier.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": {
                                "type": "string",
                                "description": "Short unique gate name."
                            },
                            "program": {
                                "type": "string",
                                "description": "Executable to spawn, for example 'uv', 'pytest', 'npm', 'make', 'cmd', 'powershell', or 'bash'."
                            },
                            "args": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Arguments passed directly to the executable."
                            },
                            "cwd": {
                                "type": "string",
                                "description": "Optional working directory relative to the workspace."
                            },
                            "timeout_ms": {
                                "type": "integer",
                                "minimum": 1,
                                "maximum": MAX_GATE_TIMEOUT_MS,
                                "description": format!("Wall-clock limit for this gate (foreground only; refused with background). When it elapses the gate's whole process tree is killed and the gate fails as timed out. Defaults to {DEFAULT_CUSTOM_GATE_TIMEOUT_MS}. Raise it for long test suites.")
                            }
                        },
                        "required": ["name", "program"],
                        "additionalProperties": false
                    },
                },
                "background": {
                    "type": "boolean",
                    "default": false,
                    "description": "Start verifier gates as background shell jobs and return task_ids immediately. Use for long build/test/lint gates; completion is tracked in task/status state, and `Bash` with action 'wait' / task_shell_wait are only for early output, final output, or true dependency barriers."
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional working directory, relative to the workspace, to detect projects and run gates in. Per-command cwd stays relative to it. Must exist inside the workspace."
                },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_GATE_TIMEOUT_MS,
                    "default": DEFAULT_GATE_TIMEOUT_MS,
                    "description": "Wall-clock limit for each built-in (detected) gate. When it elapses the gate's whole process tree is killed and the gate fails as timed out. Raise it for a `full` run on a large workspace. Foreground only; refused with background. Custom commands use their own timeout_ms."
                }
            },
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ExecutesCode, ToolCapability::Sandboxable]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    fn starts_detached_for(&self, input: &Value) -> bool {
        input.get("background").and_then(Value::as_bool) == Some(true)
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        crate::tools::execution_envelope::enforce_tool_denial(context, self.name(), &input)?;
        let input: RunVerifiersInput = serde_json::from_value(input)
            .map_err(|err| ToolError::invalid_input(err.to_string()))?;
        // `cwd` scopes the whole call — project detection, gate roots, and
        // reported paths — to an existing in-workspace subdirectory.
        let scoped;
        let context = match input
            .cwd
            .as_deref()
            .map(str::trim)
            .filter(|cwd| !cwd.is_empty())
        {
            None => context,
            Some(raw) => {
                let root = context.resolve_existing_dir(raw, "cwd")?;
                let mut narrowed = context.clone();
                narrowed.workspace = root;
                scoped = narrowed;
                &scoped
            }
        };
        let profile = VerifierProfile::parse(input.profile.as_str())?;
        let level = VerifierLevel::parse(input.level.as_str())?;
        if input.max_python_files == 0 || input.max_python_files > 1000 {
            return Err(ToolError::invalid_input(
                "max_python_files must be between 1 and 1000",
            ));
        }
        if input.commands.len() > MAX_CUSTOM_GATES {
            return Err(ToolError::invalid_input(format!(
                "commands may contain at most {MAX_CUSTOM_GATES} custom gates"
            )));
        }

        if input.background
            && (input.timeout_ms.is_some()
                || input
                    .commands
                    .iter()
                    .any(|custom| custom.timeout_ms.is_some()))
        {
            return Err(ToolError::invalid_input(
                "timeout_ms applies only to foreground gates; background gates run as shell jobs until they finish or are cancelled through task state",
            ));
        }
        let timeouts = GateTimeouts::for_call(input.timeout_ms)?;
        let gates = build_gate_plan(
            context,
            profile,
            level,
            input.max_python_files,
            &input.commands,
            timeouts,
        )?;
        if gates.is_empty() {
            let verifier_verdict = VerifierVerdict::from_counts(0, 0, 0);
            let output = RunVerifiersOutput {
                success: false,
                profile: profile.as_str().to_string(),
                level: level.as_str().to_string(),
                workspace: context.workspace.display().to_string(),
                gate_count: 0,
                passed: 0,
                failed: 0,
                skipped: 0,
                verifier_verdict,
                summary: "No verifier gates were detected. Provide custom commands or choose a profile that matches this workspace.".to_string(),
                gates: Vec::new(),
            };
            return verifier_tool_result(&output);
        }

        if input.background {
            return start_background_gates(context, profile, level, gates);
        }

        // Gates run as futures of this call, not detached blocking tasks: when
        // Stop drops the tool future, every running gate's process group is
        // killed with it instead of being orphaned.
        let mut results = futures_util::future::join_all(
            gates
                .into_iter()
                .map(|gate| run_gate(gate, &context.state_namespace)),
        )
        .await;
        results.sort_by(|a, b| a.name.cmp(&b.name));

        let passed = results
            .iter()
            .filter(|result| result.status == GateStatus::Passed)
            .count();
        let failed = results
            .iter()
            .filter(|result| result.status == GateStatus::Failed)
            .count();
        let skipped = results
            .iter()
            .filter(|result| result.status == GateStatus::Skipped)
            .count();
        let success = failed == 0 && skipped == 0;
        let verifier_verdict = VerifierVerdict::from_counts(results.len(), failed, skipped);
        let summary = if success {
            format!("All {passed} verifier gates passed.")
        } else {
            format!("{passed} passed, {failed} failed, {skipped} skipped.")
        };

        let output = RunVerifiersOutput {
            success,
            profile: profile.as_str().to_string(),
            level: level.as_str().to_string(),
            workspace: context.workspace.display().to_string(),
            gate_count: results.len(),
            passed,
            failed,
            skipped,
            verifier_verdict,
            summary,
            gates: results,
        };

        verifier_tool_result(&output)
    }
}

/// Run quick auto verifier gates after a successful workflow completion (#4013).
pub async fn run_workflow_completion_gates(context: &ToolContext) -> Result<Value, ToolError> {
    let gates = build_gate_plan(
        context,
        VerifierProfile::Auto,
        VerifierLevel::Quick,
        DEFAULT_MAX_PYTHON_FILES,
        &[],
        GateTimeouts::DEFAULT,
    )?;
    if gates.is_empty() {
        return Ok(json!({
            "success": false,
            "profile": "auto",
            "level": "quick",
            "gate_count": 0,
            "summary": "No verifier gates detected for this workspace.",
            "gates": [],
        }));
    }

    let mut results = futures_util::future::join_all(
        gates
            .into_iter()
            .map(|gate| run_gate(gate, &context.state_namespace)),
    )
    .await;
    results.sort_by(|a, b| a.name.cmp(&b.name));

    let passed = results
        .iter()
        .filter(|result| result.status == GateStatus::Passed)
        .count();
    let failed = results
        .iter()
        .filter(|result| result.status == GateStatus::Failed)
        .count();
    let skipped = results
        .iter()
        .filter(|result| result.status == GateStatus::Skipped)
        .count();
    let success = failed == 0 && skipped == 0;
    if !success {
        return Err(ToolError::execution_failed(format!(
            "{passed} passed, {failed} failed, {skipped} skipped"
        )));
    }
    Ok(json!({
        "success": true,
        "profile": "auto",
        "level": "quick",
        "gate_count": results.len(),
        "passed": passed,
        "failed": failed,
        "skipped": skipped,
        "summary": format!("All {passed} verifier gates passed."),
        "gates": results,
    }))
}

fn verifier_tool_result(output: &RunVerifiersOutput) -> Result<ToolResult, ToolError> {
    ToolResult::json(output)
        .map_err(|err| ToolError::execution_failed(err.to_string()))
        .map(|result| {
            result.with_metadata(json!({
                "verifier_verdict": output.verifier_verdict,
            }))
        })
}

fn start_background_gates(
    context: &ToolContext,
    profile: VerifierProfile,
    level: VerifierLevel,
    gates: Vec<VerifierGate>,
) -> Result<ToolResult, ToolError> {
    let mut jobs = Vec::with_capacity(gates.len());
    let mut started = 0usize;
    let mut skipped = 0usize;
    let mut failed_to_start = 0usize;

    for gate in gates {
        let cwd = gate.cwd.display().to_string();
        let Some(program) = gate.program.as_deref() else {
            skipped += 1;
            jobs.push(BackgroundGateJob {
                name: gate.name,
                ecosystem: gate.ecosystem,
                status: "skipped".to_string(),
                command: String::new(),
                cwd,
                task_id: None,
                skipped_reason: gate.skipped_reason,
                error: None,
            });
            continue;
        };

        let command = render_gate_command(program, &gate.args)?;
        let env: HashMap<String, String> = gate.env.into_iter().collect();
        let spawn_result = {
            let mut manager = context
                .shell_manager
                .lock()
                .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
            manager.execute_with_options_env_for_session(
                &command,
                Some(&cwd),
                BACKGROUND_GATE_TIMEOUT_MS,
                true,
                None,
                false,
                context.elevated_sandbox_policy.clone(),
                env,
                &context.state_namespace,
            )
        };

        match spawn_result {
            Ok(result) => {
                started += 1;
                jobs.push(BackgroundGateJob {
                    name: gate.name,
                    ecosystem: gate.ecosystem,
                    status: "running".to_string(),
                    command,
                    cwd,
                    task_id: result.task_id,
                    skipped_reason: None,
                    error: None,
                });
            }
            Err(err) => {
                failed_to_start += 1;
                jobs.push(BackgroundGateJob {
                    name: gate.name,
                    ecosystem: gate.ecosystem,
                    status: "failed_to_start".to_string(),
                    command,
                    cwd,
                    task_id: None,
                    skipped_reason: None,
                    error: Some(err.to_string()),
                });
            }
        }
    }

    jobs.sort_by(|a, b| a.name.cmp(&b.name));
    let success = failed_to_start == 0 && started > 0;
    let summary = if failed_to_start == 0 {
        format!(
            "Started {started} verifier gate(s) in the background; {skipped} skipped. Completion is tracked in task/status state. Continue inspecting or implementing while they run."
        )
    } else {
        format!(
            "Started {started} verifier gate(s), failed to start {failed_to_start}, and skipped {skipped}. Completion is tracked in task/status state. Continue inspecting or implementing while they run."
        )
    };
    let task_ids = jobs
        .iter()
        .filter_map(|job| job.task_id.clone())
        .collect::<Vec<_>>();
    let output = RunVerifiersBackgroundOutput {
        success,
        profile: profile.as_str().to_string(),
        level: level.as_str().to_string(),
        workspace: context.workspace.display().to_string(),
        background: true,
        gate_count: jobs.len(),
        started,
        skipped,
        failed_to_start,
        summary,
        jobs,
    };

    let mut result =
        ToolResult::json(&output).map_err(|err| ToolError::execution_failed(err.to_string()))?;
    result.success = success;
    Ok(result.with_metadata(json!({
        "backgrounded": true,
        "detached_start": true,
        "verifier_background": true,
        "auto_resume_on_completion": false,
        "completion_surface": "task_status",
        "background_policy": "nonblocking",
        "task_ids": task_ids,
        "poll_with": ["task_shell_wait"]
    })))
}

fn render_gate_command(program: &str, args: &[String]) -> Result<String, ToolError> {
    try_join(std::iter::once(program).chain(args.iter().map(String::as_str)))
        .map_err(|err| ToolError::execution_failed(format!("failed to render gate command: {err}")))
}

fn build_gate_plan(
    context: &ToolContext,
    profile: VerifierProfile,
    level: VerifierLevel,
    max_python_files: usize,
    custom_commands: &[CustomVerifierInput],
    timeouts: GateTimeouts,
) -> Result<Vec<VerifierGate>, ToolError> {
    let workspace = &context.workspace;
    let mut gates = Vec::new();

    if profile == VerifierProfile::Auto && workspace.join(".git").exists() {
        gates.push(gate(
            "git-whitespace",
            "git",
            workspace,
            "git",
            ["diff", "--check"],
        ));
    }

    if profile_matches(profile, VerifierProfile::Rust) && workspace.join("Cargo.toml").exists() {
        add_rust_gates(&mut gates, workspace, level);
    }
    if profile_matches(profile, VerifierProfile::Node) && workspace.join("package.json").exists() {
        add_node_gates(&mut gates, workspace, level);
    }
    if profile_matches(profile, VerifierProfile::Python) && has_python_project(workspace) {
        add_python_gates(&mut gates, workspace, level, max_python_files);
    }
    if profile_matches(profile, VerifierProfile::Go) && workspace.join("go.mod").exists() {
        add_go_gates(&mut gates, workspace, level);
    }

    let builtin_timeout = Duration::from_millis(timeouts.builtin_ms);
    for gate in &mut gates {
        gate.timeout = builtin_timeout;
    }
    for custom in custom_commands {
        gates.push(custom_gate(context, custom, timeouts)?);
    }

    Ok(gates)
}

fn profile_matches(selected: VerifierProfile, candidate: VerifierProfile) -> bool {
    selected == VerifierProfile::Auto || selected == candidate
}

fn add_rust_gates(gates: &mut Vec<VerifierGate>, workspace: &Path, level: VerifierLevel) {
    let locked = workspace.join("Cargo.lock").exists();
    gates.push(gate(
        "rust-fmt",
        "rust",
        workspace,
        "cargo",
        ["fmt", "--all", "--", "--check"],
    ));

    let metadata_args = if locked {
        vec!["metadata", "--locked", "--format-version", "1", "--no-deps"]
    } else {
        vec!["metadata", "--format-version", "1", "--no-deps"]
    };
    gates.push(gate_vec(
        "rust-metadata",
        "rust",
        workspace,
        "cargo",
        metadata_args,
    ));

    let mut check_args = vec!["check", "--workspace", "--all-targets"];
    if locked {
        check_args.push("--locked");
    }
    gates.push(gate_vec(
        "rust-check",
        "rust",
        workspace,
        "cargo",
        check_args,
    ));

    if level == VerifierLevel::Full {
        let mut clippy_args = vec!["clippy", "--workspace", "--all-targets", "--all-features"];
        if locked {
            clippy_args.push("--locked");
        }
        clippy_args.extend(["--", "-D", "warnings"]);
        gates.push(gate_vec(
            "rust-clippy",
            "rust",
            workspace,
            "cargo",
            clippy_args,
        ));

        let mut test_args = vec!["test", "--workspace", "--all-features"];
        if locked {
            test_args.push("--locked");
        }
        gates.push(gate_vec("rust-test", "rust", workspace, "cargo", test_args));
    }
}

fn add_node_gates(gates: &mut Vec<VerifierGate>, workspace: &Path, level: VerifierLevel) {
    let scripts = package_json_scripts(workspace);
    let Some(scripts) = scripts else {
        gates.push(skipped_gate(
            "node-package-json",
            "node",
            workspace,
            "package.json is missing or could not be parsed",
        ));
        return;
    };
    let package_manager = detect_node_package_manager(workspace);
    for script in ["format:check", "check", "typecheck", "lint"] {
        if has_meaningful_script(&scripts, script) {
            gates.push(node_script_gate(workspace, &package_manager, script));
        }
    }
    if level == VerifierLevel::Full && has_meaningful_script(&scripts, "test") {
        gates.push(node_script_gate(workspace, &package_manager, "test"));
    }
}

fn add_python_gates(
    gates: &mut Vec<VerifierGate>,
    workspace: &Path,
    level: VerifierLevel,
    max_python_files: usize,
) {
    let python_files = collect_python_files(workspace, max_python_files);
    match python_files {
        PythonFiles::Files(files) if !files.is_empty() => {
            gates.push(python_syntax_gate(workspace, &files));
        }
        PythonFiles::TooMany { limit, found } => gates.push(skipped_gate(
            "python-syntax",
            "python",
            workspace,
            format!(
                "found more than {limit} Python files ({found}); raise max_python_files to verify them"
            ),
        )),
        PythonFiles::Files(_) => {}
    }

    if level == VerifierLevel::Full && has_pytest_signal(workspace) {
        gates.push(python_module_gate(
            "python-pytest",
            workspace,
            ["-m", "pytest"],
        ));
    }
}

fn add_go_gates(gates: &mut Vec<VerifierGate>, workspace: &Path, level: VerifierLevel) {
    gates.push(gate("go-test", "go", workspace, "go", ["test", "./..."]));
    if level == VerifierLevel::Full {
        gates.push(gate("go-vet", "go", workspace, "go", ["vet", "./..."]));
    }
}

fn gate<const N: usize>(
    name: &str,
    ecosystem: &str,
    cwd: &Path,
    program: &str,
    args: [&str; N],
) -> VerifierGate {
    gate_vec(name, ecosystem, cwd, program, args)
}

fn gate_vec<I, S>(name: &str, ecosystem: &str, cwd: &Path, program: &str, args: I) -> VerifierGate
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    VerifierGate {
        name: name.to_string(),
        ecosystem: ecosystem.to_string(),
        cwd: cwd.to_path_buf(),
        program: Some(program.to_string()),
        args: args
            .into_iter()
            .map(|arg| arg.as_ref().to_string())
            .collect(),
        env: Vec::new(),
        skipped_reason: None,
        timeout: Duration::from_millis(DEFAULT_GATE_TIMEOUT_MS),
    }
}

fn skipped_gate(
    name: &str,
    ecosystem: &str,
    cwd: &Path,
    reason: impl Into<String>,
) -> VerifierGate {
    VerifierGate {
        name: name.to_string(),
        ecosystem: ecosystem.to_string(),
        cwd: cwd.to_path_buf(),
        program: None,
        args: Vec::new(),
        env: Vec::new(),
        skipped_reason: Some(reason.into()),
        timeout: Duration::from_millis(DEFAULT_GATE_TIMEOUT_MS),
    }
}

fn custom_gate(
    context: &ToolContext,
    custom: &CustomVerifierInput,
    timeouts: GateTimeouts,
) -> Result<VerifierGate, ToolError> {
    if custom.name.trim().is_empty() {
        return Err(ToolError::invalid_input(
            "Custom verifier command is missing 'name'",
        ));
    }
    if custom.program.trim().is_empty() {
        return Err(ToolError::invalid_input(format!(
            "Custom verifier '{}' is missing 'program'",
            custom.name
        )));
    }
    let cwd = match custom.cwd.as_deref() {
        Some(raw) if !raw.trim().is_empty() => context.resolve_path(raw)?,
        _ => context.workspace.clone(),
    };
    let timeout_ms = custom.timeout_ms.unwrap_or(timeouts.custom_default_ms);
    check_gate_timeout(
        &format!("Custom verifier '{}' timeout_ms", custom.name),
        timeout_ms,
    )?;
    Ok(VerifierGate {
        name: custom.name.clone(),
        ecosystem: "custom".to_string(),
        cwd,
        program: Some(custom.program.clone()),
        args: custom.args.clone(),
        env: Vec::new(),
        skipped_reason: None,
        timeout: Duration::from_millis(timeout_ms),
    })
}

fn node_script_gate(
    workspace: &Path,
    package_manager: &NodePackageManager,
    script: &str,
) -> VerifierGate {
    let (program, args) = package_manager.command_for_script(script);
    gate_vec(&format!("node-{script}"), "node", workspace, program, args)
}

fn python_syntax_gate(workspace: &Path, files: &[PathBuf]) -> VerifierGate {
    let Some((program, mut args)) = python_command_parts() else {
        return skipped_gate(
            "python-syntax",
            "python",
            workspace,
            "Python interpreter is not installed or not in PATH",
        );
    };
    args.push("-c".to_string());
    args.push(PYTHON_SYNTAX_SCRIPT.to_string());
    args.extend(files.iter().map(|path| path.display().to_string()));
    let mut gate = gate_vec("python-syntax", "python", workspace, &program, args);
    gate.env
        .push(("PYTHONDONTWRITEBYTECODE".to_string(), "1".to_string()));
    gate
}

fn python_module_gate<const N: usize>(
    name: &str,
    workspace: &Path,
    module_args: [&str; N],
) -> VerifierGate {
    let Some((program, mut args)) = python_command_parts() else {
        return skipped_gate(
            name,
            "python",
            workspace,
            "Python interpreter is not installed or not in PATH",
        );
    };
    args.extend(module_args.into_iter().map(str::to_string));
    gate_vec(name, "python", workspace, &program, args)
}

fn python_command_parts() -> Option<(String, Vec<String>)> {
    let spec = codewhale_dependencies::Python::resolve()?;
    Some(codewhale_dependencies::split_interpreter_spec(&spec))
}

const PYTHON_SYNTAX_SCRIPT: &str = r#"
import ast
import pathlib
import sys

failures = []
for raw in sys.argv[1:]:
    path = pathlib.Path(raw)
    try:
        source = path.read_text(encoding="utf-8")
        ast.parse(source, filename=raw)
    except Exception as exc:
        failures.append(f"{raw}: {exc.__class__.__name__}: {exc}")

if failures:
    print("\n".join(failures), file=sys.stderr)
    sys.exit(1)

print(f"parsed {len(sys.argv) - 1} Python file(s)")
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodePackageManager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

impl NodePackageManager {
    fn command_for_script(self, script: &str) -> (&'static str, Vec<String>) {
        match self {
            Self::Npm => ("npm", vec!["run".to_string(), script.to_string()]),
            Self::Pnpm => ("pnpm", vec!["run".to_string(), script.to_string()]),
            Self::Yarn => ("yarn", vec!["run".to_string(), script.to_string()]),
            Self::Bun => ("bun", vec!["run".to_string(), script.to_string()]),
        }
    }
}

fn detect_node_package_manager(workspace: &Path) -> NodePackageManager {
    if workspace.join("pnpm-lock.yaml").exists() {
        NodePackageManager::Pnpm
    } else if workspace.join("yarn.lock").exists() {
        NodePackageManager::Yarn
    } else if workspace.join("bun.lock").exists() || workspace.join("bun.lockb").exists() {
        NodePackageManager::Bun
    } else {
        NodePackageManager::Npm
    }
}

fn package_json_scripts(workspace: &Path) -> Option<HashMap<String, String>> {
    let raw = fs::read_to_string(workspace.join("package.json")).ok()?;
    let parsed = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
    let scripts = parsed.get("scripts")?.as_object()?;
    Some(
        scripts
            .iter()
            .filter_map(|(key, value)| {
                value
                    .as_str()
                    .map(|script| (key.clone(), script.to_string()))
            })
            .collect(),
    )
}

fn has_meaningful_script(scripts: &HashMap<String, String>, name: &str) -> bool {
    let Some(script) = scripts.get(name).map(|value| value.trim()) else {
        return false;
    };
    !(script.is_empty()
        || name == "test"
            && script.contains("Error: no test specified")
            && script.contains("exit 1"))
}

fn has_python_project(workspace: &Path) -> bool {
    workspace.join("pyproject.toml").exists()
        || workspace.join("setup.py").exists()
        || workspace.join("setup.cfg").exists()
        || workspace.join("requirements.txt").exists()
        || match collect_python_files(workspace, 1) {
            PythonFiles::Files(files) => !files.is_empty(),
            PythonFiles::TooMany { .. } => true,
        }
}

fn has_pytest_signal(workspace: &Path) -> bool {
    if workspace.join("pytest.ini").exists()
        || workspace.join("tox.ini").exists()
        || workspace.join("tests").is_dir()
    {
        return true;
    }
    let pyproject = workspace.join("pyproject.toml");
    fs::read_to_string(pyproject)
        .map(|raw| raw.contains("pytest") || raw.contains("[tool.pytest"))
        .unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PythonFiles {
    Files(Vec<PathBuf>),
    TooMany { limit: usize, found: usize },
}

fn collect_python_files(workspace: &Path, limit: usize) -> PythonFiles {
    let mut files = BTreeSet::new();
    collect_python_files_inner(workspace, workspace, limit, &mut files);
    let found = files.len();
    if found > limit {
        PythonFiles::TooMany { limit, found }
    } else {
        PythonFiles::Files(files.into_iter().collect())
    }
}

fn collect_python_files_inner(
    root: &Path,
    dir: &Path,
    limit: usize,
    files: &mut BTreeSet<PathBuf>,
) {
    if files.len() > limit {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if files.len() > limit {
            return;
        }
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if should_skip_dir_name(&name.to_string_lossy()) {
                continue;
            }
            collect_python_files_inner(root, &path, limit, files);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("py")
            && let Ok(relative) = path.strip_prefix(root)
        {
            files.insert(relative.to_path_buf());
        }
    }
}

fn should_skip_dir_name(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".hg"
            | ".svn"
            | ".venv"
            | "venv"
            | "env"
            | "__pycache__"
            | ".mypy_cache"
            | ".pytest_cache"
            | ".tox"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
    )
}

async fn run_gate(gate: VerifierGate, session_id: &str) -> GateResult {
    let command = render_command(gate.program.as_deref(), &gate.args);
    if let Some(reason) = gate.skipped_reason {
        return GateResult {
            name: gate.name,
            ecosystem: gate.ecosystem,
            status: GateStatus::Skipped,
            command,
            cwd: gate.cwd.display().to_string(),
            exit_code: None,
            duration_ms: 0,
            stdout: String::new(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            stdout_log_ref: None,
            stderr_log_ref: None,
            skipped_reason: Some(reason),
        };
    }

    let Some(program) = gate.program else {
        return GateResult {
            name: gate.name,
            ecosystem: gate.ecosystem,
            status: GateStatus::Skipped,
            command,
            cwd: gate.cwd.display().to_string(),
            exit_code: None,
            duration_ms: 0,
            stdout: String::new(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            stdout_log_ref: None,
            stderr_log_ref: None,
            skipped_reason: Some("verifier has no executable program".to_string()),
        };
    };

    let started = Instant::now();
    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(&gate.args)
        .current_dir(&gate.cwd)
        // A gate never reads input; an inherited stdin could block it on the
        // Engine's own terminal or pipe.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Its own process group, so a timeout or Stop reaches every helper the
    // gate started (headless Chrome forks several), not just the leader.
    #[cfg(unix)]
    cmd.process_group(0);
    // Gates run workspace code: start from the sanitized child environment
    // and layer only the gate's own declared variables on top.
    codewhale_runtime::child_env::apply_to_tokio_command(
        &mut cmd,
        gate.env
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return GateResult {
                name: gate.name,
                ecosystem: gate.ecosystem,
                status: GateStatus::Skipped,
                command,
                cwd: gate.cwd.display().to_string(),
                exit_code: None,
                duration_ms: started.elapsed().as_millis() as u64,
                stdout: String::new(),
                stderr: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
                stdout_log_ref: None,
                stderr_log_ref: None,
                skipped_reason: Some(format!("{program} is not installed or not in PATH")),
            };
        }
        Err(err) => {
            return GateResult {
                name: gate.name,
                ecosystem: gate.ecosystem,
                status: GateStatus::Failed,
                command,
                cwd: gate.cwd.display().to_string(),
                exit_code: None,
                duration_ms: started.elapsed().as_millis() as u64,
                stdout: String::new(),
                stderr: format!("Failed to spawn verifier: {err}"),
                stdout_truncated: false,
                stderr_truncated: false,
                stdout_log_ref: None,
                stderr_log_ref: None,
                skipped_reason: None,
            };
        }
    };
    // Armed until the gate settles cleanly; dropping this future (Stop)
    // kills the whole group.
    let mut group = GateProcessGroup::new(child.id());

    let mut stdout = GateCapture::new(session_id, &gate.name, "stdout");
    let mut stderr = GateCapture::new(session_id, &gate.name, "stderr");
    let mut exit: Option<std::io::Result<std::process::ExitStatus>> = None;
    let mut pipes_open = true;
    let mut timed_out = false;
    {
        let read_stdout = read_capped(child.stdout.take(), &mut stdout);
        let read_stderr = read_capped(child.stderr.take(), &mut stderr);
        let mut read_pipes = std::pin::pin!(async move {
            tokio::join!(read_stdout, read_stderr);
        });
        let deadline = tokio::time::sleep(gate.timeout);
        let mut deadline = std::pin::pin!(deadline);
        let mut drain_deadline: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
        loop {
            if exit.is_some() && !pipes_open {
                break;
            }
            tokio::select! {
                status = child.wait(), if exit.is_none() => {
                    exit = Some(status);
                    drain_deadline = Some(Box::pin(tokio::time::sleep(GATE_PIPE_DRAIN_GRACE)));
                }
                () = &mut read_pipes, if pipes_open => pipes_open = false,
                () = async { drain_deadline.as_mut().expect("guarded").await },
                    if drain_deadline.is_some() => break,
                () = &mut deadline => {
                    timed_out = true;
                    break;
                }
            }
        }
    }

    let mut notes = Vec::new();
    if timed_out || pipes_open {
        group.kill();
        if exit.is_none() {
            // The group is SIGKILLed; reaping the leader is bounded anyway so
            // an uninterruptible child cannot wedge the verifier again.
            exit = tokio::time::timeout(GATE_PIPE_DRAIN_GRACE, child.wait())
                .await
                .ok();
        }
    } else {
        group.disarm();
    }
    if timed_out {
        notes.push(format!(
            "[verifier gate timed out after {}s and its process group was killed; raise timeout_ms if it legitimately needs longer]",
            gate.timeout.as_secs_f64()
        ));
    } else if pipes_open {
        notes.push(
            "[the gate exited but a process it started kept stdout/stderr open; its process group was killed]"
                .to_string(),
        );
    }

    let exit_status = match exit {
        Some(Ok(status)) => Some(status),
        Some(Err(err)) => {
            notes.push(format!("[failed to wait for verifier: {err}]"));
            None
        }
        None => None,
    };
    let stdout = stdout.finish().await;
    let stderr = stderr.finish().await;
    let mut stderr_text = stderr.text;
    for note in notes {
        if !stderr_text.is_empty() && !stderr_text.ends_with('\n') {
            stderr_text.push('\n');
        }
        stderr_text.push_str(&note);
    }
    let passed = !timed_out && exit_status.is_some_and(|status| status.success());
    GateResult {
        name: gate.name,
        ecosystem: gate.ecosystem,
        status: if passed {
            GateStatus::Passed
        } else {
            GateStatus::Failed
        },
        command,
        cwd: gate.cwd.display().to_string(),
        exit_code: exit_status.and_then(|status| status.code()),
        duration_ms: started.elapsed().as_millis() as u64,
        stdout: stdout.text,
        stderr: stderr_text,
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
        stdout_log_ref: stdout.log_ref,
        stderr_log_ref: stderr.log_ref,
        skipped_reason: None,
    }
}

/// Read a gate's pipe to EOF into `capture`.
async fn read_capped<R>(pipe: Option<R>, capture: &mut GateCapture)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let Some(mut pipe) = pipe else {
        return;
    };
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => capture.push(&chunk[..n]).await,
        }
    }
}

/// One gate stream: every byte while it fits in memory, then its first
/// [`GATE_CAPTURE_HEAD_BYTES`] and a rolling last [`GATE_CAPTURE_TAIL_BYTES`],
/// with every byte teed to a session artifact so nothing is lost.
struct GateCapture {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    total: u64,
    session_id: String,
    artifact_id: String,
    log: Option<tokio::fs::File>,
    /// Opening or writing the log failed; the middle is gone, and the result
    /// says so instead of naming a ref.
    log_failed: bool,
}

/// What a gate stream contributes to its [`GateResult`].
struct CapturedStream {
    text: String,
    truncated: bool,
    log_ref: Option<String>,
}

impl GateCapture {
    fn new(session_id: &str, gate_name: &str, stream: &str) -> Self {
        let id = uuid::Uuid::new_v4().simple().to_string();
        Self {
            head: Vec::new(),
            tail: std::collections::VecDeque::new(),
            total: 0,
            session_id: session_id.to_string(),
            artifact_id: codewhale_runtime::artifacts::artifact_id_for_tool_call(&format!(
                "gate_{gate_name}_{stream}_{}",
                &id[..12]
            )),
            log: None,
            log_failed: false,
        }
    }

    fn overflowed(&self) -> bool {
        self.total > (GATE_CAPTURE_HEAD_BYTES + GATE_CAPTURE_TAIL_BYTES) as u64
    }

    async fn push(&mut self, bytes: &[u8]) {
        use tokio::io::AsyncWriteExt;
        self.total += bytes.len() as u64;
        if !self.overflowed() {
            self.head.extend_from_slice(bytes);
            return;
        }
        if self.log.is_none() && !self.log_failed {
            // First overflow: everything so far is still in `head`. Save it,
            // then keep only the head window in memory.
            self.log = self.open_log().await;
            self.log_failed = self.log.is_none();
            let kept = std::mem::take(&mut self.head);
            if let Some(log) = self.log.as_mut()
                && log.write_all(&kept).await.is_err()
            {
                self.log = None;
                self.log_failed = true;
            }
            self.head = kept[..GATE_CAPTURE_HEAD_BYTES.min(kept.len())].to_vec();
            self.tail
                .extend(&kept[GATE_CAPTURE_HEAD_BYTES.min(kept.len())..]);
        }
        if let Some(log) = self.log.as_mut()
            && log.write_all(bytes).await.is_err()
        {
            self.log = None;
            self.log_failed = true;
        }
        if self.head.len() < GATE_CAPTURE_HEAD_BYTES {
            let room = GATE_CAPTURE_HEAD_BYTES - self.head.len();
            self.head.extend_from_slice(&bytes[..room.min(bytes.len())]);
            self.tail.extend(&bytes[room.min(bytes.len())..]);
        } else {
            self.tail.extend(bytes);
        }
        let excess = self.tail.len().saturating_sub(GATE_CAPTURE_TAIL_BYTES);
        self.tail.drain(..excess);
    }

    async fn open_log(&self) -> Option<tokio::fs::File> {
        let relative =
            codewhale_runtime::artifacts::session_artifact_relative_path(&self.artifact_id);
        let path = codewhale_runtime::artifacts::session_artifact_absolute_path(
            &self.session_id,
            &relative,
        )?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok()?;
        }
        tokio::fs::File::create(&path).await.ok()
    }

    async fn finish(mut self) -> CapturedStream {
        use tokio::io::AsyncWriteExt;
        if !self.overflowed() {
            return CapturedStream {
                text: String::from_utf8_lossy(&self.head).into_owned(),
                truncated: false,
                log_ref: None,
            };
        }
        let saved = match self.log.as_mut() {
            Some(log) => log.flush().await.is_ok() && !self.log_failed,
            None => false,
        };
        let omitted = self.total - (self.head.len() + self.tail.len()) as u64;
        let middle = if saved {
            format!(
                "[{omitted} bytes omitted from the middle; the whole stream is artifact {}: read it with retrieve_tool_result]",
                self.artifact_id
            )
        } else {
            format!("[{omitted} bytes omitted from the middle; the full stream could not be saved]")
        };
        let tail: Vec<u8> = self.tail.into_iter().collect();
        CapturedStream {
            text: format!(
                "{}\n\n{middle}\n\n{}",
                String::from_utf8_lossy(&self.head),
                String::from_utf8_lossy(&tail)
            ),
            truncated: true,
            log_ref: saved.then_some(self.artifact_id),
        }
    }
}

/// The process group a foreground gate runs in. Killed on timeout, on
/// held-open pipes, and on drop unless the gate settled cleanly first.
struct GateProcessGroup {
    pid: Option<u32>,
}

impl GateProcessGroup {
    fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }

    fn disarm(&mut self) {
        self.pid = None;
    }

    fn kill(&mut self) {
        let Some(pid) = self.pid.take() else {
            return;
        };
        #[cfg(unix)]
        if let Ok(pgid) = libc::pid_t::try_from(pid)
            && pgid > 0
        {
            // SAFETY: kill(2) dereferences no pointers; a negative pid targets
            // the group this gate created with process_group(0).
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
        // Elsewhere `kill_on_drop` on the child covers the leader.
        #[cfg(not(unix))]
        let _ = pid;
    }
}

impl Drop for GateProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

fn render_command(program: Option<&str>, args: &[String]) -> String {
    let mut parts = Vec::new();
    parts.push(program.unwrap_or("<unavailable>").to_string());
    parts.extend(args.iter().cloned());
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::shell::ShellStatus;
    use std::time::Duration;
    use tempfile::tempdir;

    const BACKGROUND_COMPLETION_WAIT_MS: u64 = 30_000;

    fn capture_stream(session_id: &str, content: &[u8]) -> CapturedStream {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(async {
                let mut capture = GateCapture::new(session_id, "cargo test", "stdout");
                for chunk in content.chunks(8192) {
                    capture.push(chunk).await;
                }
                capture.finish().await
            })
    }

    #[test]
    fn gate_output_over_1mib_keeps_head_and_tail_and_writes_full_log() {
        // #6508: the verifier kept only the first 1 MiB of a stream and
        // dropped the end, where a failing gate reports its failure.
        let home = tempdir().expect("tempdir");
        crate::tools::truncate::with_test_home(home.path(), || {
            let content = format!(
                "FIRST LINE\n{}LAST LINE: test result: FAILED\n",
                "gate output line\n".repeat(100_000)
            );
            assert!(content.len() > GATE_CAPTURE_HEAD_BYTES + GATE_CAPTURE_TAIL_BYTES);

            let captured = capture_stream("session-6508", content.as_bytes());

            assert!(captured.truncated);
            assert!(captured.text.starts_with("FIRST LINE"));
            assert!(captured.text.ends_with("LAST LINE: test result: FAILED\n"));
            let reference = captured.log_ref.expect("full log ref");
            assert!(captured.text.contains(&reference));
            let path = codewhale_runtime::artifacts::session_artifact_absolute_path(
                "session-6508",
                &codewhale_runtime::artifacts::session_artifact_relative_path(&reference),
            )
            .expect("artifact path");
            let log = fs::read(path).expect("full log");
            assert_eq!(log.len(), content.len());
            assert_eq!(log, content.as_bytes());
        });
    }

    #[test]
    fn gate_output_that_fits_is_kept_whole_without_a_log() {
        let content = "short gate output\n".repeat(1_000);
        let captured = capture_stream("session-6508", content.as_bytes());
        assert!(!captured.truncated);
        assert_eq!(captured.text, content);
        assert!(captured.log_ref.is_none());
    }

    #[test]
    fn gate_output_whose_log_cannot_be_saved_says_so() {
        // An invalid session id has no artifact directory, so no ref is
        // promised; head and tail are still kept.
        let content = format!("HEAD\n{}TAIL\n", "x".repeat(1_200_000));
        let captured = capture_stream("", content.as_bytes());
        assert!(captured.truncated);
        assert!(captured.log_ref.is_none());
        assert!(captured.text.starts_with("HEAD"));
        assert!(captured.text.ends_with("TAIL\n"));
        assert!(captured.text.contains("the full stream could not be saved"));
    }

    fn wait_for_completed_shell(
        manager: &mut crate::tools::shell::ShellManager,
        task_id: &str,
    ) -> crate::tools::shell::ShellResult {
        let deadline = Instant::now() + Duration::from_millis(BACKGROUND_COMPLETION_WAIT_MS);

        loop {
            let result = manager
                .get_output(task_id, true, 1_000)
                .expect("background output");
            if result.status != ShellStatus::Running || Instant::now() >= deadline {
                return result;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn run_verifiers_requires_user_approval() {
        let tool = RunVerifiersTool;
        assert_eq!(
            tool.approval_requirement(),
            ApprovalRequirement::Required,
            "run_verifiers executes project code and must require approval"
        );
    }

    #[test]
    fn run_verifiers_background_advertises_detached_start() {
        let tool = RunVerifiersTool;
        let schema = tool.input_schema();
        let background_description = schema["properties"]["background"]["description"]
            .as_str()
            .expect("background description");

        assert!(background_description.contains("Bash"));
        assert!(background_description.contains("task_shell_wait"));
        assert!(
            !background_description.contains("exec_shell"),
            "live descriptions must not teach the retired exec_shell name"
        );
        assert!(tool.starts_detached_for(&json!({"background": true})));
        assert!(!tool.starts_detached_for(&json!({"profile": "auto"})));
    }

    #[test]
    fn auto_profile_detects_multiple_ecosystems_without_bash() {
        let tmp = tempdir().expect("tempdir");
        fs::write(tmp.path().join("Cargo.toml"), "[workspace]\n").expect("cargo manifest");
        fs::write(
            tmp.path().join("package.json"),
            r#"{"scripts":{"lint":"eslint .","test":"echo ok"}}"#,
        )
        .expect("package json");
        fs::write(tmp.path().join("main.py"), "print('ok')\n").expect("python file");
        fs::write(tmp.path().join("go.mod"), "module example.com/app\n").expect("go mod");

        let ctx = ToolContext::new(tmp.path());
        let gates = build_gate_plan(
            &ctx,
            VerifierProfile::Auto,
            VerifierLevel::Quick,
            DEFAULT_MAX_PYTHON_FILES,
            &[],
            GateTimeouts::DEFAULT,
        )
        .expect("plan");
        let names: BTreeSet<&str> = gates.iter().map(|gate| gate.name.as_str()).collect();

        assert!(names.contains("rust-fmt"));
        assert!(names.contains("node-lint"));
        assert!(names.contains("python-syntax"));
        assert!(names.contains("go-test"));
        assert!(
            gates
                .iter()
                .filter_map(|gate| gate.program.as_deref())
                .all(|program| program != "bash"),
            "built-in verifier gates must not require bash"
        );
    }

    #[test]
    fn custom_commands_can_choose_bash_explicitly() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path());
        let custom = CustomVerifierInput {
            name: "shell-check".to_string(),
            program: "bash".to_string(),
            args: vec!["-lc".to_string(), "echo ok".to_string()],
            cwd: None,
            timeout_ms: None,
        };

        let gate = custom_gate(&ctx, &custom, GateTimeouts::DEFAULT).expect("custom gate");

        assert_eq!(gate.program.as_deref(), Some("bash"));
        assert_eq!(gate.args, vec!["-lc", "echo ok"]);
    }

    #[test]
    fn node_default_npm_init_test_script_is_not_a_verifier() {
        let mut scripts = HashMap::new();
        scripts.insert(
            "test".to_string(),
            "echo \"Error: no test specified\" && exit 1".to_string(),
        );

        assert!(!has_meaningful_script(&scripts, "test"));
    }

    #[tokio::test]
    async fn run_verifiers_executes_custom_direct_command() {
        if !codewhale_dependencies::RustC::available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path());
        let tool = RunVerifiersTool;
        let result = tool
            .execute(
                json!({
                    "profile": "auto",
                    "commands": [
                        {
                            "name": "rustc-version",
                            "program": codewhale_dependencies::RustC::resolve().expect("rustc"),
                            "args": ["--version"]
                        }
                    ]
                }),
                &ctx,
            )
            .await
            .expect("execute");

        let parsed: RunVerifiersOutput =
            serde_json::from_str(&result.content).expect("verifier output json");
        assert!(parsed.success, "result: {}", result.content);
        assert_eq!(parsed.passed, 1);
        assert_eq!(parsed.failed, 0);
        assert_eq!(parsed.skipped, 0);
        assert!(
            parsed.gates[0].stdout.contains("rustc"),
            "stdout should include rustc version: {:?}",
            parsed.gates[0].stdout
        );
    }

    #[tokio::test]
    async fn run_verifiers_emits_verdict_mapping() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path());
        let tool = RunVerifiersTool;

        let partial = tool
            .execute(json!({"profile": "auto"}), &ctx)
            .await
            .expect("execute partial verifier");
        assert_verdict(&partial, "partial");

        if !codewhale_dependencies::RustC::available() {
            return;
        }

        let pass = tool
            .execute(
                json!({
                    "profile": "auto",
                    "commands": [
                        {
                            "name": "rustc-version",
                            "program": codewhale_dependencies::RustC::resolve().expect("rustc"),
                            "args": ["--version"]
                        }
                    ]
                }),
                &ctx,
            )
            .await
            .expect("execute passing verifier");
        assert_verdict(&pass, "pass");

        let fail = tool
            .execute(
                json!({
                    "profile": "auto",
                    "commands": [
                        {
                            "name": "rustc-bad-flag",
                            "program": codewhale_dependencies::RustC::resolve().expect("rustc"),
                            "args": ["--definitely-not-a-rustc-flag"]
                        }
                    ]
                }),
                &ctx,
            )
            .await
            .expect("execute failing verifier");
        assert_verdict(&fail, "fail");
    }

    #[tokio::test]
    async fn run_verifiers_cwd_scopes_detection_to_subdir() {
        let tmp = tempdir().expect("tempdir");
        let sub = tmp.path().join("nested");
        std::fs::create_dir(&sub).expect("subdir");
        let ctx = ToolContext::new(tmp.path());

        // Empty subdir: no gates detected, and the reported workspace is the
        // scoped root rather than the parent workspace.
        let result = RunVerifiersTool
            .execute(json!({"profile": "auto", "cwd": "nested"}), &ctx)
            .await
            .expect("cwd-scoped execute");
        let parsed: RunVerifiersOutput =
            serde_json::from_str(&result.content).expect("verifier output json");
        assert_eq!(parsed.gate_count, 0);
        assert_eq!(
            parsed.workspace,
            sub.canonicalize().expect("canonical").display().to_string()
        );

        // Missing dir: refused with the fallback named.
        let err = RunVerifiersTool
            .execute(json!({"profile": "auto", "cwd": "no-such-dir"}), &ctx)
            .await
            .expect_err("missing dir must be refused");
        let message = err.to_string();
        assert!(message.contains("not an existing directory"), "{message}");
        assert!(message.contains("drop `cwd`"), "{message}");
    }

    fn assert_verdict(result: &ToolResult, verifier: &str) {
        let parsed: Value = serde_json::from_str(&result.content).expect("verifier output json");
        assert_eq!(parsed["verifier_verdict"], verifier, "{}", result.content);
        assert!(parsed.get("hunt_verdict").is_none(), "{}", result.content);
        assert!(parsed.get("goal_status").is_none(), "{}", result.content);
        let metadata = result.metadata.as_ref().expect("verifier metadata");
        assert_eq!(metadata["verifier_verdict"], verifier, "{metadata}");
        assert!(metadata.get("hunt_verdict").is_none(), "{metadata}");
        assert!(metadata.get("task_updates").is_none(), "{metadata}");
    }

    #[tokio::test]
    async fn run_verifiers_background_starts_shell_jobs_and_returns_task_ids() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path());
        let tool = RunVerifiersTool;
        // Unix: drive this very libtest binary with `--list` (no rustup, no
        // $HOME). Windows: the background gate is rendered with POSIX
        // quoting and handed to PowerShell, which cannot parse a quoted
        // absolute path with backslashes, so use a bare `cmd` there — the
        // listing shape (`name: test`) is the same either way.
        #[cfg(not(windows))]
        let (program, args) = (
            std::env::current_exe()
                .expect("test executable")
                .to_string_lossy()
                .into_owned(),
            vec!["--list".to_string()],
        );
        #[cfg(windows)]
        let (program, args) = (
            "cmd".to_string(),
            vec!["/c".to_string(), "echo test-list: test".to_string()],
        );
        let result = tool
            .execute(
                json!({
                    "profile": "auto",
                    "background": true,
                    "commands": [
                        {
                            "name": "test-list",
                            "program": program,
                            "args": args
                        }
                    ]
                }),
                &ctx,
            )
            .await
            .expect("execute");

        let parsed: RunVerifiersBackgroundOutput =
            serde_json::from_str(&result.content).expect("background verifier output json");
        assert!(parsed.success, "result: {}", result.content);
        assert!(parsed.background);
        assert_eq!(parsed.started, 1);
        assert_eq!(parsed.failed_to_start, 0);
        assert!(parsed.summary.contains("Completion is tracked"));
        let task_id = parsed.jobs[0]
            .task_id
            .as_deref()
            .expect("background task id");
        let metadata = result.metadata.as_ref().expect("metadata");
        assert!(
            metadata
                .get("verifier_background")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            "metadata should mark verifier background start"
        );
        assert_eq!(
            metadata
                .get("auto_notify_on_completion")
                .and_then(Value::as_bool),
            None
        );
        assert_eq!(
            metadata
                .get("auto_resume_on_completion")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            metadata.get("completion_surface").and_then(Value::as_str),
            Some("task_status")
        );
        assert_eq!(
            metadata.get("background_policy").and_then(Value::as_str),
            Some("nonblocking")
        );

        let output = wait_for_completed_shell(
            &mut ctx.shell_manager.lock().expect("shell manager"),
            task_id,
        );
        assert_eq!(
            output.status,
            ShellStatus::Completed,
            "stdout: {:?} stderr: {:?}",
            output.stdout,
            output.stderr
        );
        assert!(
            output.stdout.lines().any(|line| line.ends_with(": test")),
            "stdout should include the test listing: {:?}",
            output.stdout
        );
    }

    /// Headless Chrome (`--print-to-pdf` / `--dump-dom`) writes its output and
    /// then never exits while a helper keeps stdout/stderr open. This script
    /// reproduces that shape deterministically: a leader plus a background
    /// child, both holding the gate's pipes, neither ever exiting.
    #[cfg(unix)]
    fn hung_gate_script(pidfile: &Path) -> String {
        format!(
            "sleep 600 & echo $! > '{p}'; echo $$ >> '{p}'; echo started; wait",
            p = pidfile.display()
        )
    }

    #[cfg(unix)]
    async fn read_gate_pids(pidfile: &Path) -> Vec<libc::pid_t> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let pids: Vec<libc::pid_t> = fs::read_to_string(pidfile)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| line.trim().parse().ok())
                .collect();
            if pids.len() >= 2 {
                return pids;
            }
            assert!(Instant::now() < deadline, "hung gate never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(unix)]
    async fn assert_pids_gone(pids: &[libc::pid_t]) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // SAFETY: signal 0 only probes for existence.
            let alive: Vec<_> = pids
                .iter()
                .copied()
                .filter(|pid| unsafe { libc::kill(*pid, 0) } == 0)
                .collect();
            if alive.is_empty() {
                return;
            }
            if Instant::now() >= deadline {
                for pid in &alive {
                    // SAFETY: best-effort cleanup of the leaked test processes.
                    unsafe {
                        libc::kill(*pid, libc::SIGKILL);
                    }
                }
                panic!("verifier gate processes still running: {alive:?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_verifiers_hung_gate_times_out_and_kills_its_process_group() {
        let tmp = tempdir().expect("tempdir");
        let pidfile = tmp.path().join("pids");
        let ctx = ToolContext::new(tmp.path());
        let started = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(20),
            RunVerifiersTool.execute(
                json!({
                    "profile": "auto",
                    "commands": [{
                        "name": "hung",
                        "program": "/bin/sh",
                        "args": ["-c", hung_gate_script(&pidfile)],
                        "timeout_ms": 500
                    }]
                }),
                &ctx,
            ),
        )
        .await
        .expect("a hung verifier gate must not hang run_verifiers")
        .expect("execute");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "timed-out gate took {:?}",
            started.elapsed()
        );

        let parsed: RunVerifiersOutput =
            serde_json::from_str(&result.content).expect("verifier output json");
        assert_eq!(parsed.failed, 1, "result: {}", result.content);
        let gate = &parsed.gates[0];
        assert_eq!(gate.status, GateStatus::Failed);
        assert!(
            gate.stderr.contains("timed out"),
            "stderr: {:?}",
            gate.stderr
        );
        assert!(
            gate.stdout.contains("started"),
            "partial stdout kept: {:?}",
            gate.stdout
        );
        assert_pids_gone(&read_gate_pids(&pidfile).await).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_verifiers_dropped_on_stop_kills_running_gates() {
        let tmp = tempdir().expect("tempdir");
        let pidfile = tmp.path().join("pids");
        let ctx = ToolContext::new(tmp.path());
        let script = hung_gate_script(&pidfile);
        // Stop drops the active tool future (turn_loop's biased select on the
        // cancel token). That drop must take the gate's processes with it.
        let task = tokio::spawn(async move {
            RunVerifiersTool
                .execute(
                    json!({
                        "profile": "auto",
                        "commands": [{
                            "name": "hung",
                            "program": "/bin/sh",
                            "args": ["-c", script]
                        }]
                    }),
                    &ctx,
                )
                .await
        });
        let pids = read_gate_pids(&pidfile).await;
        task.abort();
        let _ = task.await;
        assert_pids_gone(&pids).await;
    }

    #[test]
    fn built_in_gates_take_the_call_timeout_and_custom_gates_keep_their_own() {
        let tmp = tempdir().expect("tempdir");
        fs::write(tmp.path().join("Cargo.toml"), "[workspace]\n").expect("cargo manifest");
        let ctx = ToolContext::new(tmp.path());
        let custom = [
            CustomVerifierInput {
                name: "default".to_string(),
                program: "true".to_string(),
                ..CustomVerifierInput::default()
            },
            CustomVerifierInput {
                name: "explicit".to_string(),
                program: "true".to_string(),
                timeout_ms: Some(42_000),
                ..CustomVerifierInput::default()
            },
        ];
        let long = MAX_GATE_TIMEOUT_MS;
        let gates = build_gate_plan(
            &ctx,
            VerifierProfile::Rust,
            VerifierLevel::Full,
            DEFAULT_MAX_PYTHON_FILES,
            &custom,
            GateTimeouts::for_call(Some(long)).expect("in range"),
        )
        .expect("plan");
        let rust: Vec<_> = gates
            .iter()
            .filter(|gate| gate.ecosystem == "rust")
            .collect();
        assert!(!rust.is_empty(), "rust gates detected");
        // A `full` run on a large workspace is no longer capped at 600s.
        for gate in rust {
            assert_eq!(gate.timeout, Duration::from_millis(long), "{}", gate.name);
        }
        let timeout_of = |name: &str| {
            gates
                .iter()
                .find(|gate| gate.name == name)
                .unwrap_or_else(|| panic!("gate {name}"))
                .timeout
        };
        assert_eq!(
            timeout_of("default"),
            Duration::from_millis(DEFAULT_CUSTOM_GATE_TIMEOUT_MS)
        );
        assert_eq!(timeout_of("explicit"), Duration::from_millis(42_000));
        assert!(GateTimeouts::for_call(Some(0)).is_err());
        assert!(GateTimeouts::for_call(Some(long + 1)).is_err());
    }

    #[tokio::test]
    async fn run_verifiers_background_refuses_timeout_ms_it_cannot_enforce() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path());
        for input in [
            json!({"background": true, "timeout_ms": 1000,
                   "commands": [{"name": "a", "program": "true"}]}),
            json!({"background": true,
                   "commands": [{"name": "a", "program": "true", "timeout_ms": 1000}]}),
        ] {
            let err = RunVerifiersTool
                .execute(input.clone(), &ctx)
                .await
                .expect_err("background timeout_ms must be refused");
            assert!(err.to_string().contains("foreground"), "{input}: {err}");
        }
        let jobs = ctx.shell_manager.lock().expect("shell manager").list_jobs();
        assert!(jobs.is_empty(), "nothing may start: {jobs:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_gate_does_not_inherit_parent_secret_env_but_keeps_gate_env() {
        use crate::test_support::{EnvVarGuard, lock_test_env};
        let _env_lock = lock_test_env();
        let _secret = EnvVarGuard::set("CODEWHALE_TEST_VERIFIER_SECRET", "verifier-secret-value");
        let tmp = tempdir().expect("tempdir");
        let gate = VerifierGate {
            name: "env-probe".to_string(),
            ecosystem: "custom".to_string(),
            cwd: tmp.path().to_path_buf(),
            program: Some("/bin/sh".to_string()),
            args: vec![
                "-c".to_string(),
                "printf '%s|%s' \"${CODEWHALE_TEST_VERIFIER_SECRET-unset}\" \"${GATE_DECLARED-missing}\""
                    .to_string(),
            ],
            env: vec![("GATE_DECLARED".to_string(), "declared".to_string())],
            skipped_reason: None,
            timeout: Duration::from_secs(30),
        };
        let result = run_gate(gate, "env-probe-test").await;
        assert_eq!(result.stdout.trim(), "unset|declared", "{result:?}");
    }
}
