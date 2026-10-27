//! Cargo test runner tool: `run_tests`.
//!
//! `cargo test` runs workspace code, so this tool follows the same explicit
//! approval policy as the other code-executing tools.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::cargo_failure_summary::summarize_cargo_failure;
use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_bool, optional_str,
};

use codewhale_dependencies::ExternalTool;

/// Tool for running `cargo test` in the workspace root.
pub struct RunTestsTool;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunTestsOutput {
    success: bool,
    exit_code: i32,
    stdout: String,
    stderr: String,
    command: String,
    /// The run hit [`RUN_TESTS_TIMEOUT`] and was killed; stdout/stderr are
    /// what it wrote until then.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    timed_out: bool,
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
        "Run `cargo test` in the workspace root with optional extra arguments."
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

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        crate::tools::execution_envelope::enforce_tool_denial(context, self.name(), &input)?;
        let all_features = optional_bool(&input, "all_features", false)?;
        let extra_args = optional_str(&input, "args")?
            .map(str::trim)
            .filter(|s| !s.is_empty());
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
        if let Some(extra) = extra_args {
            let split = shlex::split(extra).ok_or_else(|| {
                ToolError::invalid_input("Failed to parse 'args' as shell-style tokens")
            })?;
            args.extend(split);
        }

        let command_str = format_command(&workdir, &args);
        let run = run_cargo(&workdir, &args, context, RUN_TESTS_TIMEOUT).await?;
        let output = run.output;
        let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if run.stopped {
            stderr.push_str(&format!(
                "\n[run_tests: stopped after {} s; the output above is everything cargo wrote before it was killed]",
                RUN_TESTS_TIMEOUT.as_secs()
            ));
        }

        // The whole output: the end of a cargo run is where the failures and
        // the `test result:` line are. Size is the engine's one recoverable
        // budget (#6508), so the failure summary sees everything.
        run_tests_result(RunTestsOutput {
            success: !run.stopped && output.status.success(),
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr,
            command: command_str,
            timed_out: run.stopped,
        })
    }
}

fn run_tests_result(result: RunTestsOutput) -> Result<ToolResult, ToolError> {
    let mut tool_result =
        ToolResult::json(&result).map_err(|e| ToolError::execution_failed(e.to_string()))?;
    if let Some(summary) = summarize_cargo_failure(
        &result.command,
        &result.stdout,
        &result.stderr,
        Some(result.exit_code),
    ) {
        tool_result = tool_result.with_metadata(json!({
            "summary": summary.summary,
            "cargo_failure_summary": summary.to_metadata_value(),
        }));
    }
    Ok(tool_result)
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
        assert_eq!(parsed.exit_code, 0);
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
        assert_ne!(parsed.exit_code, 0);
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
            exit_code: 101,
            stdout: stdout.clone(),
            stderr: String::new(),
            command: "(cd /repo && cargo test)".to_string(),
            timed_out: false,
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
}
