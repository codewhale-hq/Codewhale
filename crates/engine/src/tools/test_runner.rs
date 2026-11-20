//! Cargo test runner tool: `run_tests`.
//!
//! `cargo test` runs workspace code, so this tool follows the same explicit
//! approval policy as the other code-executing tools.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::cargo_failure_summary::summarize_cargo_failure;
use super::shell::{SharedShellManager, ShellOutputStream, ShellStatus};
use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_bool, optional_str,
};

use codewhale_dependencies::ExternalTool;

const DEFAULT_AUTO_BACKGROUND_TIMEOUT_MS: u64 = 30_000;
const MAX_RUN_TESTS_TIMEOUT_MS: u64 = 30 * 60 * 1_000;
const BACKGROUND_TESTS_TIMEOUT_MS: u64 = 600_000;
const FOREGROUND_POLL_INITIAL_MS: u64 = 5;
const FOREGROUND_POLL_MAX_MS: u64 = 100;

/// Tool for running `cargo test` in the workspace root.
pub struct RunTestsTool;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RunTestsOutput {
    success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    command: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    timed_out: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    backgrounded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_output_age_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    progress: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    zero_tests_matched: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tests_executed: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tests_filtered_out: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ZeroTestDetection {
    pub zero_tests_matched: bool,
    pub tests_executed: Option<usize>,
    pub tests_filtered_out: Option<usize>,
    pub runner: Option<&'static str>,
    pub reason: Option<String>,
}

#[async_trait]
impl ToolSpec for RunTestsTool {
    fn name(&self) -> &'static str {
        "run_tests"
    }

    fn model_visible(&self) -> bool {
        false
    }

    fn description(&self) -> &'static str {
        "Run `cargo test` in the workspace root with optional extra arguments. Auto-backgrounds after 30s by default unless background=false; set background=true to detach immediately."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "args": {
                    "type": "string",
                    "description": "Optional extra arguments to pass to `cargo test` (shell-style)."
                },
                "all_features": {
                    "type": "boolean",
                    "description": "When true, include `--all-features`."
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional working directory, relative to the workspace, to run `cargo test` in. Must exist inside the workspace."
                },
                "background": {
                    "type": "boolean",
                    "description": "When true, start immediately as a background job and return task_id. When omitted, wait up to timeout_ms (default 30000ms) and auto-background if still running. When false, block until completion or timeout_ms."
                },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_RUN_TESTS_TIMEOUT_MS,
                    "description": "Foreground wait budget in milliseconds before auto-backgrounding (default 30000), or maximum blocking timeout when background=false."
                }
            },
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ExecutesCode, ToolCapability::Sandboxable]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        // `run_tests` declares `ToolCapability::ExecutesCode` — match the
        // default approval policy for code-executing tools.
        ApprovalRequirement::Required
    }

    fn starts_detached_for(&self, input: &Value) -> bool {
        input.get("background").and_then(Value::as_bool) == Some(true)
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        crate::tools::execution_envelope::enforce_tool_denial(context, self.name(), &input)?;
        let all_features = optional_bool(&input, "all_features", false)?;
        let extra_args = optional_str(&input, "args")?
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let explicit_background = match input.get("background") {
            None | Some(Value::Null) => None,
            Some(Value::Bool(flag)) => Some(*flag),
            Some(_) => {
                return Err(ToolError::invalid_input(
                    "Field 'background' must be a boolean",
                ));
            }
        };
        let explicit_background = if context
            .child_host
            .as_ref()
            .is_some_and(|child| !child.is_tool_allowed("task_shell_wait"))
        {
            if explicit_background == Some(true) {
                return Err(ToolError::invalid_input(
                    "This child cannot collect a background test job; run in the foreground",
                ));
            }
            Some(false)
        } else {
            explicit_background
        };
        let timeout_ms = match input.get("timeout_ms") {
            None | Some(Value::Null) => None,
            Some(value) => {
                let ms = value.as_u64().ok_or_else(|| {
                    ToolError::invalid_input("Field 'timeout_ms' must be a positive integer")
                })?;
                if !(1..=MAX_RUN_TESTS_TIMEOUT_MS).contains(&ms) {
                    return Err(ToolError::invalid_input(format!(
                        "timeout_ms must be between 1 and {MAX_RUN_TESTS_TIMEOUT_MS}"
                    )));
                }
                Some(ms)
            }
        };
        let workdir = match optional_str(&input, "cwd")?
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            None => context.workspace.clone(),
            Some(raw) => context.resolve_existing_dir(raw, "cwd")?,
        };

        let mut args = vec!["test".to_string()];
        if all_features {
            args.push("--all-features".to_string());
        }
        let mut extra_tokens = Vec::new();
        if let Some(extra) = extra_args {
            let split = shlex::split(extra).ok_or_else(|| {
                ToolError::invalid_input("Failed to parse 'args' as shell-style tokens")
            })?;
            extra_tokens = split.clone();
            args.extend(split);
        }
        let filtered = has_test_filter_args(&extra_tokens);

        if context
            .cancel_token
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
        {
            return Err(ToolError::cancelled("cargo test cancelled"));
        }

        let command_str = format_command(&workdir, &args);
        match explicit_background {
            Some(true) => start_background_cargo(&workdir, &args, &command_str, context),
            None => {
                let wait =
                    Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_AUTO_BACKGROUND_TIMEOUT_MS));
                run_cargo_with_auto_background(
                    &workdir,
                    &args,
                    &command_str,
                    context,
                    wait,
                    filtered,
                )
                .await
            }
            Some(false) => {
                let timeout = timeout_ms
                    .map(Duration::from_millis)
                    .unwrap_or(RUN_TESTS_TIMEOUT);
                let run = run_cargo(&workdir, &args, context, timeout).await?;
                let output = run.output;
                let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                if run.stopped {
                    stderr.push_str(&format!(
                        "\n[run_tests: stopped after {} s; the output above is everything cargo wrote before it was killed]",
                        timeout.as_secs()
                    ));
                }
                let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                let exit_code = output.status.code().unwrap_or(-1);
                finish_completed_tests(
                    !run.stopped && output.status.success(),
                    exit_code,
                    stdout,
                    stderr,
                    command_str,
                    run.stopped,
                    filtered,
                )
            }
        }
    }
}

fn finish_completed_tests(
    status_ok: bool,
    exit_code: i32,
    stdout: String,
    stderr: String,
    command: String,
    timed_out: bool,
    filtered: bool,
) -> Result<ToolResult, ToolError> {
    let detection = if status_ok && !timed_out {
        detect_zero_tests_matched(&stdout, &stderr, filtered)
    } else {
        ZeroTestDetection::default()
    };
    let success = status_ok && !timed_out && !detection.zero_tests_matched;
    let warning = detection.zero_tests_matched.then(|| {
        detection.reason.clone().unwrap_or_else(|| {
            "0 tests matched; verify your test filter args because a 0-test pass proves nothing"
                .to_string()
        })
    });
    run_tests_result(RunTestsOutput {
        success,
        exit_code: Some(exit_code),
        stdout,
        stderr,
        command,
        timed_out,
        backgrounded: false,
        status: None,
        task_id: None,
        last_output_age_ms: None,
        progress: None,
        zero_tests_matched: detection.zero_tests_matched,
        tests_executed: detection.tests_executed,
        tests_filtered_out: detection.tests_filtered_out,
        summary: warning.clone(),
        warning,
    })
}

fn run_tests_result(result: RunTestsOutput) -> Result<ToolResult, ToolError> {
    let mut tool_result =
        ToolResult::json(&result).map_err(|e| ToolError::execution_failed(e.to_string()))?;
    let mut metadata = serde_json::Map::new();
    if let Some(summary) = summarize_cargo_failure(
        &result.command,
        &result.stdout,
        &result.stderr,
        result.exit_code,
    ) {
        metadata.insert("summary".to_string(), json!(summary.summary));
        metadata.insert(
            "cargo_failure_summary".to_string(),
            summary.to_metadata_value(),
        );
    }
    if result.zero_tests_matched {
        metadata.insert("zero_tests_matched".to_string(), json!(true));
        if let Some(executed) = result.tests_executed {
            metadata.insert("tests_executed".to_string(), json!(executed));
        }
        if let Some(filtered_out) = result.tests_filtered_out {
            metadata.insert("tests_filtered_out".to_string(), json!(filtered_out));
        }
        if let Some(warning) = result.warning.as_ref()
            && !metadata.contains_key("summary")
        {
            metadata.insert("summary".to_string(), json!(warning));
        }
    }
    if !metadata.is_empty() {
        tool_result = tool_result.with_metadata(Value::Object(metadata));
    }
    Ok(tool_result)
}

fn spawn_cargo_in_shell_manager(
    workspace: &Path,
    args: &[String],
    context: &ToolContext,
) -> Result<String, ToolError> {
    if context.sandbox_backend.is_some() {
        return Err(ToolError::not_available(
            "this tool starts a local process, and this session runs commands in an external sandbox; run the command through the shell tool instead",
        ));
    }
    let Some(cargo) = codewhale_dependencies::Cargo::resolve() else {
        return Err(ToolError::not_available(
            "cargo is not installed or not in PATH",
        ));
    };
    let shell_command = render_cargo_shell_command(&cargo, args)?;
    let cwd = workspace.display().to_string();
    let mut env = HashMap::new();
    crate::tools::shell::insert_agent_mark_env(&mut env, context);
    let mut manager = context
        .shell_manager
        .lock()
        .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
    let spawned = manager
        .execute_with_options_env_for_owner_and_work(
            &shell_command,
            Some(&cwd),
            BACKGROUND_TESTS_TIMEOUT_MS,
            true,
            None,
            false,
            context.elevated_sandbox_policy.clone(),
            env,
            super::shell::shell_job_owner_from_context(context),
            context.state_namespace.clone(),
            context.origin_tool_call_id.clone(),
            context.origin_turn_id.clone(),
            super::shell::shell_work_lifecycle_from_context(context),
            None,
            false,
            (1_000, BACKGROUND_TESTS_TIMEOUT_MS),
        )
        .map_err(|err| ToolError::execution_failed(format!("Failed to start cargo test: {err}")))?;
    let task_id = spawned
        .task_id
        .ok_or_else(|| ToolError::execution_failed("cargo test did not return a task_id"))?;
    let _ = manager.write_stdin(&task_id, "", true);
    let _ = manager.tag_description(&task_id, Some(format!("cargo {}", args.join(" "))));
    if let Some(active_task_id) = context.runtime.active_task_id.clone() {
        let _ = manager.tag_linked_task(&task_id, Some(active_task_id));
    }
    Ok(task_id)
}

fn background_tests_tool_result(
    output: RunTestsOutput,
    context: &ToolContext,
    detached_start: bool,
) -> Result<ToolResult, ToolError> {
    let task_id = output.task_id.clone();
    let task_ids: Vec<String> = task_id.clone().into_iter().collect();
    let child_owned = context.owner_agent_id.is_some();
    let tool_result =
        ToolResult::json(&output).map_err(|e| ToolError::execution_failed(e.to_string()))?;
    Ok(tool_result.with_metadata(json!({
        "backgrounded": true,
        "detached_start": detached_start,
        "task_id": task_id,
        "task_ids": task_ids,
        "auto_resume_on_completion": !child_owned,
        "completion_surface": if child_owned {
            "task_status_and_explicit_wait"
        } else {
            "runtime_event_and_task_status"
        },
        "background_policy": "nonblocking",
        "poll_with": ["task_shell_wait"],
    })))
}

fn start_background_cargo(
    workspace: &Path,
    args: &[String],
    command_str: &str,
    context: &ToolContext,
) -> Result<ToolResult, ToolError> {
    let task_id = spawn_cargo_in_shell_manager(workspace, args, context)?;
    let summary = format!(
        "Started cargo test in the background (task_id: {task_id}). Call `task_shell_wait` with this task_id to read output, wait for completion, or cancel."
    );
    background_tests_tool_result(
        RunTestsOutput {
            success: true,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            command: command_str.to_string(),
            timed_out: false,
            backgrounded: true,
            status: Some("running".to_string()),
            task_id: Some(task_id),
            last_output_age_ms: None,
            progress: None,
            zero_tests_matched: false,
            tests_executed: None,
            tests_filtered_out: None,
            warning: None,
            summary: Some(summary),
        },
        context,
        true,
    )
}

struct ForegroundTestGuard {
    manager: SharedShellManager,
    task_id: String,
    armed: bool,
}

impl Drop for ForegroundTestGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut manager = self
            .manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Ok(killed) = manager.kill(&self.task_id) {
            manager.mark_cancel_reported(&killed);
        }
    }
}

async fn run_cargo_with_auto_background(
    workspace: &Path,
    args: &[String],
    command_str: &str,
    context: &ToolContext,
    foreground_wait: Duration,
    filtered: bool,
) -> Result<ToolResult, ToolError> {
    let task_id = spawn_cargo_in_shell_manager(workspace, args, context)?;
    let mut guard = ForegroundTestGuard {
        manager: context.shell_manager.clone(),
        task_id: task_id.clone(),
        armed: true,
    };
    let deadline = Instant::now() + foreground_wait;
    let mut poll_ms = FOREGROUND_POLL_INITIAL_MS;

    loop {
        if context
            .cancel_token
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
        {
            let mut manager = context
                .shell_manager
                .lock()
                .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
            if let Ok(killed) = manager.kill(&task_id) {
                manager.mark_cancel_reported(&killed);
            }
            guard.armed = false;
            return Err(ToolError::cancelled("cargo test cancelled"));
        }

        let finished = {
            let mut manager = context
                .shell_manager
                .lock()
                .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
            let probe = manager
                .read_output_chunk(&task_id, ShellOutputStream::Stdout, usize::MAX, 0, 0)
                .map_err(|err| ToolError::execution_failed(err.to_string()))?;
            if probe.status == ShellStatus::Running {
                None
            } else {
                let stdout = manager
                    .read_output_chunk(&task_id, ShellOutputStream::Stdout, 0, usize::MAX, 0)
                    .map(|chunk| String::from_utf8_lossy(&chunk.bytes).into_owned())
                    .unwrap_or_default();
                let stderr = manager
                    .read_output_chunk(&task_id, ShellOutputStream::Stderr, 0, usize::MAX, 0)
                    .map(|chunk| String::from_utf8_lossy(&chunk.bytes).into_owned())
                    .unwrap_or_default();
                manager.acknowledge_foreground_completion(&task_id);
                Some((probe.status, probe.exit_code, stdout, stderr))
            }
        };

        if let Some((status, exit_code, stdout, stderr)) = finished {
            guard.armed = false;
            let status_ok = status == ShellStatus::Completed && exit_code == Some(0);
            return finish_completed_tests(
                status_ok,
                exit_code
                    .and_then(|code| i32::try_from(code).ok())
                    .unwrap_or(-1),
                stdout,
                stderr,
                command_str.to_string(),
                status == ShellStatus::TimedOut,
                filtered,
            );
        }

        if Instant::now() >= deadline {
            guard.armed = false;
            let (snapshot, last_output_age_ms, progress) = {
                let mut manager = context
                    .shell_manager
                    .lock()
                    .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
                let snapshot = manager
                    .get_output(&task_id, false, 0)
                    .map_err(|err| ToolError::execution_failed(err.to_string()))?;
                if snapshot.status != ShellStatus::Running {
                    manager.acknowledge_foreground_completion(&task_id);
                }
                let age = manager.last_output_age_ms(&task_id);
                let progress = manager.running_progress(&task_id);
                (snapshot, age, progress)
            };
            if snapshot.status != ShellStatus::Running {
                let status_ok =
                    snapshot.status == ShellStatus::Completed && snapshot.exit_code == Some(0);
                return finish_completed_tests(
                    status_ok,
                    snapshot
                        .exit_code
                        .and_then(|code| i32::try_from(code).ok())
                        .unwrap_or(-1),
                    snapshot.stdout,
                    snapshot.stderr,
                    command_str.to_string(),
                    snapshot.status == ShellStatus::TimedOut,
                    filtered,
                );
            }
            let summary = format!(
                "cargo test is still running after {}ms and moved to the background (task_id: {task_id}). Call `task_shell_wait` with this task_id to read output, wait for completion, or cancel.",
                foreground_wait.as_millis()
            );
            return background_tests_tool_result(
                RunTestsOutput {
                    success: true,
                    exit_code: None,
                    stdout: snapshot.stdout,
                    stderr: snapshot.stderr,
                    command: command_str.to_string(),
                    timed_out: false,
                    backgrounded: true,
                    status: Some("running".to_string()),
                    task_id: Some(task_id),
                    last_output_age_ms,
                    progress,
                    zero_tests_matched: false,
                    tests_executed: None,
                    tests_filtered_out: None,
                    warning: None,
                    summary: Some(summary),
                },
                context,
                false,
            );
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        tokio::time::sleep(remaining.min(Duration::from_millis(poll_ms))).await;
        poll_ms = (poll_ms * 2).min(FOREGROUND_POLL_MAX_MS);
    }
}

fn render_cargo_shell_command(cargo: &str, args: &[String]) -> Result<String, ToolError> {
    #[cfg(windows)]
    {
        let _ = cargo;
        let mut parts = vec!["cargo".to_string()];
        for arg in args {
            if arg.is_empty()
                || arg
                    .bytes()
                    .any(|b| b.is_ascii_whitespace() || matches!(b, b'"' | b'\'' | b'$' | b'`'))
            {
                parts.push(format!("'{}'", arg.replace('\'', "''")));
            } else {
                parts.push(arg.clone());
            }
        }
        Ok(parts.join(" "))
    }
    #[cfg(not(windows))]
    {
        shlex::try_join(std::iter::once(cargo).chain(args.iter().map(String::as_str))).map_err(
            |err| ToolError::execution_failed(format!("failed to render cargo command: {err}")),
        )
    }
}

pub(crate) fn has_test_filter_args(extra_tokens: &[String]) -> bool {
    const FLAGS_WITH_VALUE: &[&str] = &[
        "-p",
        "--package",
        "--bin",
        "--example",
        "--test",
        "--bench",
        "--features",
        "-F",
        "--target",
        "--profile",
        "-j",
        "--jobs",
        "--color",
        "--message-format",
        "--manifest-path",
        "--target-dir",
        "--config",
    ];
    let mut after_sep = false;
    let mut skip_next = false;
    for token in extra_tokens {
        if skip_next {
            skip_next = false;
            continue;
        }
        if !after_sep && token == "--" {
            after_sep = true;
            continue;
        }
        if after_sep {
            if !token.starts_with('-') || matches!(token.as_str(), "--exact" | "--ignored") {
                return true;
            }
            continue;
        }
        if token == "--test" || token.starts_with("--test=") {
            return true;
        }
        if token.starts_with('-') {
            if !token.contains('=') && FLAGS_WITH_VALUE.contains(&token.as_str()) {
                skip_next = true;
            }
            continue;
        }
        return true;
    }
    false
}

fn parse_libtest_summary_line(line: &str) -> Option<(usize, usize, usize, usize, Option<usize>)> {
    let rest = line.trim().strip_prefix("test result:")?;
    let (_, counts) = rest.split_once('.')?;
    let mut passed = None;
    let mut failed = None;
    let mut ignored = 0usize;
    let mut measured = 0usize;
    let mut filtered_out = None;
    for part in counts.split(';') {
        let part = part.trim();
        if let Some(n) = part
            .strip_suffix(" passed")
            .and_then(|v| v.trim().parse::<usize>().ok())
        {
            passed = Some(n);
        } else if let Some(n) = part
            .strip_suffix(" failed")
            .and_then(|v| v.trim().parse::<usize>().ok())
        {
            failed = Some(n);
        } else if let Some(n) = part
            .strip_suffix(" ignored")
            .and_then(|v| v.trim().parse::<usize>().ok())
        {
            ignored = n;
        } else if let Some(n) = part
            .strip_suffix(" measured")
            .and_then(|v| v.trim().parse::<usize>().ok())
        {
            measured = n;
        } else if let Some(n) = part
            .strip_suffix(" filtered out")
            .and_then(|v| v.trim().parse::<usize>().ok())
        {
            filtered_out = Some(n);
        }
    }
    Some((passed?, failed?, ignored, measured, filtered_out))
}

fn parse_number_before(haystack: &str, suffix: &str) -> Option<usize> {
    let idx = haystack.find(suffix)?;
    let prefix = haystack[..idx].trim_end();
    let token = prefix
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()
        .filter(|s| !s.is_empty())?;
    token.parse::<usize>().ok()
}

fn normalize_ascii_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn line_has_positive_pass_or_fail(lower_norm: &str) -> bool {
    parse_number_before(lower_norm, " passed").is_some_and(|n| n > 0)
        || parse_number_before(lower_norm, " failed").is_some_and(|n| n > 0)
}

pub(crate) fn detect_zero_tests_matched(
    stdout: &str,
    stderr: &str,
    filtered: bool,
) -> ZeroTestDetection {
    let mut clean_stdout = String::with_capacity(stdout.len());
    let mut clean_stderr = String::with_capacity(stderr.len());
    codewhale_sanitize::sanitize::strip_ansi_into(stdout, &mut clean_stdout);
    codewhale_sanitize::sanitize::strip_ansi_into(stderr, &mut clean_stderr);

    let all_lines: Vec<&str> = clean_stdout.lines().chain(clean_stderr.lines()).collect();

    let mut libtest_lines = 0usize;
    let mut libtest_passed = 0usize;
    let mut libtest_failed = 0usize;
    let mut libtest_ignored = 0usize;
    let mut libtest_measured = 0usize;
    let mut libtest_filtered = 0usize;
    let mut saw_filtered_field = false;
    for line in &all_lines {
        if let Some((passed, failed, ignored, measured, filtered_out)) =
            parse_libtest_summary_line(line)
        {
            libtest_lines += 1;
            libtest_passed = libtest_passed.saturating_add(passed);
            libtest_failed = libtest_failed.saturating_add(failed);
            libtest_ignored = libtest_ignored.saturating_add(ignored);
            libtest_measured = libtest_measured.saturating_add(measured);
            if let Some(fo) = filtered_out {
                saw_filtered_field = true;
                libtest_filtered = libtest_filtered.saturating_add(fo);
            }
        }
    }
    if libtest_lines > 0 {
        let executed = libtest_passed
            .saturating_add(libtest_failed)
            .saturating_add(libtest_measured);
        let filtered_out = saw_filtered_field.then_some(libtest_filtered);
        if executed == 0 && libtest_failed == 0 && (libtest_filtered > 0 || filtered) {
            return ZeroTestDetection {
                zero_tests_matched: true,
                tests_executed: Some(0),
                tests_filtered_out: filtered_out,
                runner: Some("cargo"),
                reason: Some(format!(
                    "0 tests executed ({libtest_filtered} filtered out, {libtest_ignored} ignored); check your test filter — a 0-test pass proves nothing"
                )),
            };
        }
        return ZeroTestDetection {
            zero_tests_matched: false,
            tests_executed: Some(executed),
            tests_filtered_out: filtered_out,
            runner: Some("cargo"),
            reason: None,
        };
    }

    let mut saw_nextest = false;
    let mut nextest_run = 0usize;
    let mut nextest_skipped = None;
    for line in &all_lines {
        let trimmed = line.trim();
        if trimmed.contains("Summary [")
            && let Some(n) = parse_number_before(trimmed, " tests run")
                .or_else(|| parse_number_before(trimmed, " test run"))
        {
            saw_nextest = true;
            nextest_run = nextest_run.saturating_add(n);
            if let Some(sk) = parse_number_before(trimmed, " skipped") {
                nextest_skipped = Some(nextest_skipped.unwrap_or(0usize).saturating_add(sk));
            }
        } else if trimmed.starts_with("Starting 0 tests across")
            || trimmed.eq_ignore_ascii_case("error: no tests to run")
        {
            saw_nextest = true;
            if let Some(sk) = parse_number_before(trimmed, " skipped") {
                nextest_skipped = Some(nextest_skipped.unwrap_or(0usize).saturating_add(sk));
            }
        }
    }
    if saw_nextest {
        if nextest_run == 0 {
            return ZeroTestDetection {
                zero_tests_matched: true,
                tests_executed: Some(0),
                tests_filtered_out: nextest_skipped,
                runner: Some("nextest"),
                reason: Some(
                    "nextest executed 0 tests; check your test filter — a 0-test run proves nothing"
                        .to_string(),
                ),
            };
        }
        return ZeroTestDetection {
            zero_tests_matched: false,
            tests_executed: Some(nextest_run),
            tests_filtered_out: nextest_skipped,
            runner: Some("nextest"),
            reason: None,
        };
    }

    let mut any_positive = false;
    let mut saw_go_pass_or_fail = false;
    let mut pytest_zero = false;
    let mut pytest_deselected = None;
    let mut js_zero = false;
    let mut go_zero = false;

    for line in &all_lines {
        let trimmed = line.trim();
        if trimmed.starts_with("--- PASS:") || trimmed.starts_with("--- FAIL:") {
            saw_go_pass_or_fail = true;
            any_positive = true;
        }
        let lower_norm = normalize_ascii_spaces(&trimmed.to_ascii_lowercase());
        if line_has_positive_pass_or_fail(&lower_norm) {
            any_positive = true;
        }
        if lower_norm.contains("collected 0 items")
            || lower_norm.contains("no tests ran in ")
            || (lower_norm.contains("deselected") && lower_norm.contains("0 selected"))
        {
            pytest_zero = true;
            if let Some(d) = parse_number_before(&lower_norm, " deselected") {
                pytest_deselected = Some(d);
            }
        }
        if lower_norm.contains("no tests found")
            || lower_norm.contains("no test files found")
            || lower_norm.contains("tests: 0 total")
            || lower_norm.contains("0 tests passed")
            || lower_norm == "tests 0 passed"
            || lower_norm == "test files 0 passed"
        {
            js_zero = true;
        }
        if lower_norm.contains("testing: warning: no tests to run") {
            go_zero = true;
        }
    }

    if pytest_zero && !any_positive {
        return ZeroTestDetection {
            zero_tests_matched: true,
            tests_executed: Some(0),
            tests_filtered_out: pytest_deselected,
            runner: Some("pytest"),
            reason: Some(
                "pytest collected/selected 0 tests; check your test filter — a 0-test run proves nothing"
                    .to_string(),
            ),
        };
    }
    if js_zero && !any_positive {
        return ZeroTestDetection {
            zero_tests_matched: true,
            tests_executed: Some(0),
            tests_filtered_out: None,
            runner: Some("jest/vitest"),
            reason: Some(
                "test runner found 0 matching tests; check your test filter — a 0-test run proves nothing"
                    .to_string(),
            ),
        };
    }
    if go_zero && !saw_go_pass_or_fail {
        return ZeroTestDetection {
            zero_tests_matched: true,
            tests_executed: Some(0),
            tests_filtered_out: None,
            runner: Some("go test"),
            reason: Some(
                "go test reported no tests to run; check your -run filter — a 0-test run proves nothing"
                    .to_string(),
            ),
        };
    }

    ZeroTestDetection::default()
}

// === Helpers ===

/// Ceiling for one `cargo test` run. Long suites fit; a hung test does not
/// hold the turn forever.
const RUN_TESTS_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Run cargo without blocking a runtime worker. Stop and the timeout both end
/// the whole process tree (cargo and the test binaries it started). A timeout
/// still returns the output written so far (`stopped`): it is the only
/// evidence of which test hung.
async fn run_cargo(
    workspace: &Path,
    args: &[String],
    context: &ToolContext,
    timeout: Duration,
) -> Result<codewhale_runtime::process_tree::ContainedOutput, ToolError> {
    let Some(cargo) = codewhale_dependencies::Cargo::resolve() else {
        return Err(ToolError::not_available(
            "cargo is not installed or not in PATH",
        ));
    };
    // `cargo test` builds and runs workspace code: it starts like an
    // `exec_shell` command, inside this session's sandbox and without parent
    // credentials.
    let mut cmd = crate::tools::shell::sandboxed_runner_command(
        context,
        &cargo,
        args.to_vec(),
        workspace,
        timeout,
    )?;
    let run = codewhale_runtime::process_tree::contained_output_until(
        &mut cmd,
        tokio::time::sleep(timeout),
    );
    let cancelled = async {
        match context.cancel_token.as_ref() {
            Some(token) => token.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    };
    // Dropping `run` on cancel kills the tree.
    let output = tokio::select! {
        output = run => output,
        () = cancelled => return Err(ToolError::cancelled("cargo test cancelled")),
    };
    output.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ToolError::not_available("cargo is not installed or not in PATH")
        } else {
            ToolError::execution_failed(format!("Failed to run cargo: {e}"))
        }
    })
}

fn format_command(workspace: &Path, args: &[String]) -> String {
    format!(
        "(cd {} && cargo {})",
        workspace.display(),
        args.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tempfile::tempdir;

    static NEXT_CARGO_PROJECT: AtomicU64 = AtomicU64::new(0);

    fn cargo_available() -> bool {
        Command::new("cargo")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn init_cargo_project(root: &Path) -> std::path::PathBuf {
        let project_dir = root.join("project");
        let package_name = format!(
            "eval_project_{}_{}",
            std::process::id(),
            NEXT_CARGO_PROJECT.fetch_add(1, Ordering::Relaxed)
        );
        fs::create_dir_all(&project_dir).expect("create project dir");
        let status = codewhale_dependencies::Cargo::command()
            .expect("cargo not found")
            .args(["init", "--lib", "--vcs", "none", "-q"])
            .arg("--name")
            .arg(package_name)
            .current_dir(&project_dir)
            .status()
            .expect("cargo should spawn");
        assert!(status.success(), "cargo init failed");
        project_dir
    }

    /// `run_tests` is `ToolCapability::ExecutesCode`, so it must follow the
    /// explicit-approval policy that applies to other code-executing tools.
    #[test]
    fn run_tests_requires_user_approval() {
        let tool = RunTestsTool;
        assert_eq!(
            tool.approval_requirement(),
            ApprovalRequirement::Required,
            "run_tests must gate cargo test behind user approval"
        );
    }

    /// Stop must reach `cargo test`: the call returns as cancelled instead of
    /// blocking a runtime worker until cargo exits on its own.
    #[tokio::test]
    async fn run_tests_honors_cancellation() {
        if !cargo_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let ctx = ToolContext::new(tmp.path()).with_cancel_token(token);
        let result = RunTestsTool.execute(json!({}), &ctx).await;
        assert!(
            matches!(result, Err(ToolError::Cancelled { .. })),
            "cancelled run_tests must not run to completion: {result:?}"
        );
    }

    #[tokio::test]
    async fn run_tests_succeeds_on_fresh_project() {
        if !cargo_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        // Release jobs commonly export one CARGO_TARGET_DIR for the whole
        // workspace. Give concurrent nested Cargo fixtures distinct package
        // identities so their test artifacts cannot replace each other.
        let project_dir = init_cargo_project(tmp.path());

        let ctx = ToolContext::new(&project_dir);
        let tool = RunTestsTool;
        let result = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(result.success);

        let parsed: RunTestsOutput =
            serde_json::from_str(&result.content).expect("tool result should be json");
        assert!(
            parsed.success,
            "nested cargo test unexpectedly failed:\n{}",
            parsed.stderr
        );
        assert_eq!(parsed.exit_code, Some(0));
        assert!(parsed.command.contains("cargo test"));
    }

    #[tokio::test]
    async fn run_tests_reports_failures_without_hard_error() {
        if !cargo_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        let project_dir = init_cargo_project(tmp.path());

        let lib_rs = project_dir.join("src/lib.rs");
        let failing = r#"
pub fn add(a: i32, b: i32) -> i32 { a + b }

#[cfg(test)]
mod tests {
    #[test]
    fn fails() {
        assert_eq!(2 + 2, 5);
    }
}
"#;
        fs::write(&lib_rs, failing).expect("write failing test");

        let ctx = ToolContext::new(&project_dir);
        let tool = RunTestsTool;
        let result = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(result.success);

        let parsed: RunTestsOutput =
            serde_json::from_str(&result.content).expect("tool result should be json");
        assert!(
            !parsed.success,
            "nested cargo test unexpectedly passed:\nstdout:\n{}\nstderr:\n{}",
            parsed.stdout, parsed.stderr
        );
        assert!(parsed.exit_code.is_some_and(|code| code != 0));
        let metadata = result.metadata.expect("metadata");
        assert_eq!(
            metadata["cargo_failure_summary"]["kind"],
            json!("test_failure")
        );
        assert!(
            metadata["cargo_failure_summary"]["summary"]
                .as_str()
                .unwrap()
                .contains("Failing tests:")
        );
    }

    #[test]
    fn long_failing_output_comes_back_whole_and_the_summary_sees_its_end() {
        // #6508: stdout used to be cut at 40,000 characters, so the model
        // read passing tests and never saw which one failed, and the failure
        // summary ran on the truncated text.
        let stdout = format!(
            "{}test tools::git::tests::diff_keeps_the_last_file ... FAILED\n\ntest result: FAILED. 3000 passed; 1 failed; 0 ignored",
            "test tools::ok ... ok\n".repeat(3_000)
        );
        assert!(stdout.chars().count() > 40_000);
        let result = run_tests_result(RunTestsOutput {
            success: false,
            exit_code: Some(101),
            stdout: stdout.clone(),
            stderr: String::new(),
            command: "(cd /repo && cargo test)".to_string(),
            timed_out: false,
            ..RunTestsOutput::default()
        })
        .expect("result");

        let parsed: Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(parsed["stdout"], json!(stdout));
        let summary = result
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("summary"))
            .and_then(Value::as_str)
            .expect("failure summary");
        assert!(
            summary.contains("diff_keeps_the_last_file"),
            "summary: {summary}"
        );
    }

    /// A child parked at a workspace root that is not the project root (the
    /// #6296 verifier) runs the suite where the manifest lives instead of
    /// failing on cwd.
    #[tokio::test]
    async fn run_tests_cwd_scopes_cargo_to_subdir() {
        if !cargo_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        let project_dir = init_cargo_project(tmp.path());

        let ctx = ToolContext::new(tmp.path());
        let result = RunTestsTool
            .execute(json!({"cwd": "project"}), &ctx)
            .await
            .expect("cwd-scoped execute");
        assert!(result.success);

        let parsed: RunTestsOutput =
            serde_json::from_str(&result.content).expect("tool result should be json");
        assert!(
            parsed.success,
            "nested cargo test unexpectedly failed:\\n{}",
            parsed.stderr
        );
        // `resolve_existing_dir` returns the canonical path, which on Windows
        // carries the `\\?\` verbatim prefix the raw tempdir lacks (#6346).
        let scoped_dir = project_dir.canonicalize().expect("canonical project dir");
        assert!(
            parsed.command.contains(&scoped_dir.display().to_string()),
            "cargo must run in the scoped dir, ran: {}",
            parsed.command
        );
    }

    #[tokio::test]
    async fn run_tests_cwd_fails_closed_with_a_named_fallback() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path());

        let escape = RunTestsTool
            .execute(json!({"cwd": "../escape"}), &ctx)
            .await
            .expect_err("workspace escape must be refused");
        assert!(escape.to_string().contains("escapes workspace"), "{escape}");

        let missing = RunTestsTool
            .execute(json!({"cwd": "no-such-dir"}), &ctx)
            .await
            .expect_err("missing dir must be refused");
        let message = missing.to_string();
        assert!(message.contains("not an existing directory"), "{message}");
        assert!(message.contains("drop `cwd`"), "{message}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_cargo_does_not_inherit_parent_secret_env() {
        use crate::test_support::{EnvVarGuard, lock_test_env};
        use std::os::unix::fs::PermissionsExt;
        if !cargo_available() {
            return;
        }
        let _env_lock = lock_test_env();
        let bin = tempdir().expect("bin dir");
        // cargo resolves `cargo envprobe` to a `cargo-envprobe` on PATH, which
        // lets the test observe the environment cargo hands its children.
        let probe = bin.path().join("cargo-envprobe");
        fs::write(
            &probe,
            "#!/bin/sh\nprintf 'secret=%s target=%s' \"${CODEWHALE_TEST_CARGO_SECRET-unset}\" \"${CARGO_TARGET_DIR-unset}\"\n",
        )
        .expect("write probe");
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).expect("chmod probe");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![bin.path().to_path_buf()];
        paths.extend(std::env::split_paths(&path));
        let _path = EnvVarGuard::set("PATH", std::env::join_paths(paths).expect("join PATH"));
        let _secret = EnvVarGuard::set("CODEWHALE_TEST_CARGO_SECRET", "cargo-secret-value");
        // Non-secret build configuration still reaches cargo.
        let _target = EnvVarGuard::set("CARGO_TARGET_DIR", "/tmp/codewhale-fixture-target");
        let workspace = tempdir().expect("workspace");

        let ctx = ToolContext::new(workspace.path());
        let run = run_cargo(
            workspace.path(),
            &["envprobe".to_string()],
            &ctx,
            RUN_TESTS_TIMEOUT,
        )
        .await
        .expect("cargo runs");
        assert!(!run.stopped);
        let output = run.output;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout} {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            stdout.trim(),
            "secret=unset target=/tmp/codewhale-fixture-target"
        );
    }

    /// A run that hits the timeout is killed, tree and all, and still returns
    /// what it wrote: the only evidence of which test hung.
    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_run_cargo_keeps_partial_output_and_kills_the_tree() {
        use crate::test_support::{EnvVarGuard, lock_test_env};
        use std::os::unix::fs::PermissionsExt;
        if !cargo_available() {
            return;
        }
        let _env_lock = lock_test_env();
        let bin = tempdir().expect("bin dir");
        let probe = bin.path().join("cargo-hangprobe");
        fs::write(
            &probe,
            "#!/bin/sh\necho 'test slow_case ... started'\nsleep 300 &\necho $! > hang.pid\nwait\n",
        )
        .expect("write probe");
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).expect("chmod probe");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![bin.path().to_path_buf()];
        paths.extend(std::env::split_paths(&path));
        let _path = EnvVarGuard::set("PATH", std::env::join_paths(paths).expect("join PATH"));
        let workspace = tempdir().expect("workspace");
        let pid_file = workspace.path().join("hang.pid");

        let ctx = ToolContext::new(workspace.path());
        let run = run_cargo(
            workspace.path(),
            &["hangprobe".to_string()],
            &ctx,
            Duration::from_secs(10),
        )
        .await
        .expect("a timed-out run still returns its output");
        assert!(run.stopped);
        assert!(
            String::from_utf8_lossy(&run.output.stdout).contains("test slow_case ... started"),
            "{:?}",
            run.output
        );
        let hung =
            codewhale_runtime::process_tree::read_pid_file(&pid_file, Duration::from_secs(5));
        assert!(
            codewhale_runtime::process_tree::wait_for_pid_exit(hung, Duration::from_secs(5)),
            "the timed-out run's process is still running"
        );
    }

    #[test]
    fn zero_test_detector_flags_empty_filters_across_runners_and_allows_real_runs() {
        let cargo_filtered = detect_zero_tests_matched(
            "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 26 filtered out; finished in 0.00s\n",
            "",
            true,
        );
        assert!(cargo_filtered.zero_tests_matched);
        assert_eq!(cargo_filtered.tests_executed, Some(0));
        assert_eq!(cargo_filtered.tests_filtered_out, Some(26));

        let cargo_multi_target_pass = detect_zero_tests_matched(
            "test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 21 filtered out; finished in 0.02s\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
            "",
            true,
        );
        assert!(!cargo_multi_target_pass.zero_tests_matched);
        assert_eq!(cargo_multi_target_pass.tests_executed, Some(5));

        let cargo_unfiltered_empty = detect_zero_tests_matched(
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
            "",
            false,
        );
        assert!(!cargo_unfiltered_empty.zero_tests_matched);

        let nextest_zero = detect_zero_tests_matched(
            "",
            "\x1b[1mSummary\x1b[0m [   0.001s] 0 tests run: 0 passed, 12 skipped\n",
            true,
        );
        assert!(nextest_zero.zero_tests_matched);
        assert_eq!(nextest_zero.tests_executed, Some(0));
        assert_eq!(nextest_zero.tests_filtered_out, Some(12));

        let pytest_zero = detect_zero_tests_matched(
            "collected 26 items / 26 deselected / 0 selected\n=== 26 deselected in 0.01s ===\n",
            "",
            true,
        );
        assert!(pytest_zero.zero_tests_matched);
        assert_eq!(pytest_zero.tests_filtered_out, Some(26));

        let pytest_collected_zero =
            detect_zero_tests_matched("collected 0 items\n\nno tests ran in 0.00s\n", "", false);
        assert!(pytest_collected_zero.zero_tests_matched);

        let vitest_zero =
            detect_zero_tests_matched("No test files found, exiting with code 0\n", "", true);
        assert!(vitest_zero.zero_tests_matched);

        let jest_zero =
            detect_zero_tests_matched("No tests found, exiting with code 0\n", "", true);
        assert!(jest_zero.zero_tests_matched);

        let go_zero = detect_zero_tests_matched(
            "testing: warning: no tests to run\nPASS\nok  \texample.com/pkg\t0.002s\n",
            "",
            true,
        );
        assert!(go_zero.zero_tests_matched);

        let go_pass = detect_zero_tests_matched(
            "--- PASS: TestOk (0.00s)\nPASS\nok  \texample.com/pkg\t0.002s\n",
            "",
            true,
        );
        assert!(!go_pass.zero_tests_matched);
    }

    #[tokio::test]
    async fn background_cargo_preserves_child_ownership_and_settlement() {
        if !cargo_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        let project_dir = init_cargo_project(tmp.path());
        fs::write(
            project_dir.join("src/lib.rs"),
            "#[test] fn waits() { std::thread::sleep(std::time::Duration::from_secs(2)); }",
        )
        .expect("write test");
        let ctx = ToolContext::new(&project_dir)
            .with_owner_agent("test-child", "verifier")
            .with_origin_tool_call_id("test-call")
            .with_origin_turn_id("test-turn");
        let result = RunTestsTool
            .execute(json!({"background": true}), &ctx)
            .await
            .expect("launch cargo");
        let output: RunTestsOutput = serde_json::from_str(&result.content).expect("json");
        let task_id = output.task_id.expect("task id");
        {
            let mut manager = ctx.shell_manager.lock().expect("shell manager");
            let job = manager
                .list_jobs_for_session(&ctx.state_namespace)
                .into_iter()
                .find(|job| job.id == task_id)
                .expect("child job");
            assert_eq!(job.owner_agent_id.as_deref(), Some("test-child"));
            assert_eq!(job.owner_agent_name.as_deref(), Some("verifier"));
            assert_eq!(job.origin_tool_call_id.as_deref(), Some("test-call"));
            assert_eq!(job.origin_turn_id.as_deref(), Some("test-turn"));
            assert_eq!(
                manager.running_owner_agent_ids_for_session(&ctx.state_namespace),
                vec!["test-child".to_string()]
            );
        }
        let settled = super::super::shell::BashTool::alias("task_shell_wait", "wait")
            .execute(json!({"task_id": task_id, "timeout_ms": 30_000}), &ctx)
            .await
            .expect("wait for child cargo");
        assert!(settled.success, "{}", settled.content);
        let mut manager = ctx.shell_manager.lock().expect("shell manager");
        assert!(
            manager
                .running_owner_agent_ids_for_session(&ctx.state_namespace)
                .is_empty()
        );
        assert!(
            manager
                .drain_finished_jobs_with_evidence_for_session(&ctx.state_namespace)
                .is_empty()
        );
    }

    #[tokio::test]
    async fn run_tests_backgrounds_and_flags_zero_test_filters() {
        if !cargo_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        let project_dir = init_cargo_project(tmp.path());
        let ctx = ToolContext::new(&project_dir);

        let zero_result = RunTestsTool
            .execute(json!({"args": "--lib definitely_no_such_test_name"}), &ctx)
            .await
            .expect("zero-test execute");
        let parsed_zero: RunTestsOutput = serde_json::from_str(&zero_result.content).expect("json");
        assert!(
            parsed_zero.zero_tests_matched,
            "expected zero_tests_matched: {}",
            zero_result.content
        );
        assert!(!parsed_zero.success, "{}", zero_result.content);
        assert_eq!(parsed_zero.tests_executed, Some(0));
        assert!(parsed_zero.tests_filtered_out.is_some_and(|n| n >= 1));
        assert!(parsed_zero.warning.is_some());

        let bg_result = RunTestsTool
            .execute(json!({"background": true}), &ctx)
            .await
            .expect("background execute");
        let parsed_bg: RunTestsOutput = serde_json::from_str(&bg_result.content).expect("json");
        assert!(parsed_bg.success, "{}", bg_result.content);
        assert!(parsed_bg.backgrounded);
        assert_eq!(parsed_bg.status.as_deref(), Some("running"));
        let bg_task_id = parsed_bg.task_id.as_deref().expect("task_id");
        let bg_meta = bg_result.metadata.as_ref().expect("metadata");
        assert_eq!(bg_meta["backgrounded"], json!(true));
        assert_eq!(bg_meta["detached_start"], json!(true));
        assert_eq!(bg_meta["task_id"], json!(bg_task_id));
        let _ = ctx.shell_manager.lock().expect("lock").kill(bg_task_id);

        let slow_lib = r#"
#[cfg(test)]
mod tests {
    #[test]
    fn slow_test() {
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
}
"#;
        fs::write(project_dir.join("src/lib.rs"), slow_lib).expect("write slow test");
        let started = Instant::now();
        let auto_bg = RunTestsTool
            .execute(json!({"timeout_ms": 150}), &ctx)
            .await
            .expect("auto-background execute");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "auto-background took {:?}",
            started.elapsed()
        );
        let parsed_auto: RunTestsOutput = serde_json::from_str(&auto_bg.content).expect("json");
        assert!(parsed_auto.success, "{}", auto_bg.content);
        assert!(parsed_auto.backgrounded, "{}", auto_bg.content);
        assert_eq!(parsed_auto.status.as_deref(), Some("running"));
        let auto_task_id = parsed_auto.task_id.as_deref().expect("auto task_id");
        let _ = ctx.shell_manager.lock().expect("lock").kill(auto_task_id);
    }
}
