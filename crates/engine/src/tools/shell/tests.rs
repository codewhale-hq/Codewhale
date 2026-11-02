use super::*;

use crate::tools::spec::ToolContext;
use serde_json::{Value, json};
#[cfg(unix)]
use std::sync::OnceLock;
use tempfile::tempdir;

#[cfg(windows)]
use windows::Win32::Foundation::{DUPLICATE_HANDLE_OPTIONS, DuplicateHandle, HANDLE};
#[cfg(windows)]
use windows::Win32::System::Threading::GetCurrentProcess;

const BACKGROUND_COMPLETION_WAIT_MS: u64 = 30_000;

#[test]
fn shell_catalog_guidance_matches_execution() {
    let tool = LowercaseBashTool;
    let schema = tool.input_schema();
    let command = schema["properties"]["command"]["description"]
        .as_str()
        .unwrap();
    let dispatcher = codewhale_sandbox::shell_dispatcher::global_dispatcher();
    assert!(command.contains(dispatcher.kind().binary()));
    assert!(
        !tool.description().contains(command),
        "the command's interpreter guidance must appear once in each request"
    );
    assert_eq!(tool.name(), "bash");
    assert!(tool.model_visible());
    assert!(!command.contains("action=run"));
    assert!(!tool.description().contains("background=true"));
    assert!(
        tool.description().contains(
            "In Ask, after a sandbox denial, retry the exact command once with sandbox_permissions (the narrowest wider mode that suffices) and a one-sentence justification; the approval prompt asks the user."
        ),
        "foreground guidance must preserve the sandbox retry and approval contract"
    );
    let legacy = BashTool::new("Bash");
    assert!(!legacy.model_visible());
    assert!(legacy.description().contains(command));
    assert!(legacy.description().contains("background=true"));
    assert!(legacy.description().contains("wait=false"));
    let readonly = BashTool::read_only("Bash");
    assert!(readonly.description().contains("never through a shell"));
    assert!(
        !readonly
            .input_schema()
            .to_string()
            .contains("Actual execution shell")
    );
    let alias = BashTool::alias("exec_shell", "run");
    assert_eq!(alias.description(), legacy.description());
    let workspace = tempdir().unwrap();
    let mut registry = crate::tools::ToolRegistry::new(ToolContext::new(workspace.path()));
    registry.register(std::sync::Arc::new(BashTool::new("Bash")));
    registry.register(std::sync::Arc::new(LowercaseBashTool));
    let catalog = registry.to_api_tools();
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].name, "bash");
    assert_eq!(catalog[0].description, tool.description());
    assert_eq!(
        catalog[0].input_schema["properties"]["command"]["description"],
        command
    );
}

#[test]
#[ignore = "Exports model-visible shell fixtures for opt-in live model evaluation"]
fn export_shell_guidance_eval_fixture() {
    let path = std::env::var_os("SHELL_GUIDANCE_FIXTURE").expect("SHELL_GUIDANCE_FIXTURE");
    let workspace = tempdir().unwrap();
    let mut registry = crate::tools::ToolRegistry::new(ToolContext::new(workspace.path()));
    registry.register(std::sync::Arc::new(BashTool::new("Bash")));
    registry.register(std::sync::Arc::new(LowercaseBashTool));
    let catalog = registry.to_api_tools();
    let tool = catalog.iter().find(|tool| tool.name == "bash").unwrap();
    let fixture = json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.input_schema,
        "shell": codewhale_sandbox::shell_dispatcher::global_dispatcher().kind().binary(),
    });
    std::fs::write(path, serde_json::to_vec_pretty(&fixture).unwrap()).unwrap();
}

#[test]
fn lowercase_bash_schema_is_small_contract() {
    let schema = LowercaseBashTool.input_schema();
    assert_eq!(schema["required"], json!(["command"]));
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "command",
            "description",
            "justification",
            "read_only",
            "sandbox_permissions",
            "timeout"
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    );
    assert!(!BashTool::new("Bash").model_visible());
}

#[test]
fn lowercase_bash_description_matches_the_timeout_it_actually_applies() {
    use super::{
        CONTRACT_BASH_FOREGROUND_DEFAULT_TIMEOUT_MS, contract_bash_legacy_input,
        contract_bash_timeout_ms,
    };

    // `bash {command}` with no `timeout` translates to a legacy input carrying
    // no `timeout_ms`. The contract delegate waits the 30 s default in the
    // foreground, then moves a still-running process to the background.
    let translated =
        contract_bash_legacy_input(&json!({"command": "sleep 600"})).expect("translated input");
    assert!(
        translated.get("timeout_ms").is_none(),
        "omitting `timeout` must not synthesise one during translation: {translated}"
    );
    assert_eq!(
        contract_bash_timeout_ms(true, None, false, false),
        Some(CONTRACT_BASH_FOREGROUND_DEFAULT_TIMEOUT_MS)
    );

    // The tool description is the only place the model learns this. It used to
    // say "when omitted there is no default timeout", so a model running a
    // four-minute build had every reason not to pass a timeout, and got the
    // process killed at two minutes anyway. It now names that wait and says
    // the process is not killed there.
    let description = LowercaseBashTool.description();
    assert!(
        !description.contains("no default timeout"),
        "description contradicts the applied default: {description}"
    );
    let default_seconds = CONTRACT_BASH_FOREGROUND_DEFAULT_TIMEOUT_MS / 1_000;
    assert!(
        description.contains(&format!("{default_seconds} seconds")),
        "description must name the default it applies: {description}"
    );
    let schema = LowercaseBashTool.input_schema();
    let timeout_doc = schema["properties"]["timeout"]["description"]
        .as_str()
        .expect("timeout description");
    assert!(
        !timeout_doc.contains("no default timeout"),
        "schema contradicts the applied default: {timeout_doc}"
    );
    assert!(
        timeout_doc.contains(&format!("{default_seconds} seconds")),
        "schema must name the default it applies: {timeout_doc}"
    );
}

#[test]
fn contract_bash_foreground_without_a_timeout_is_bounded_not_endless() {
    use super::{
        BASH_MAX_TIMEOUT_MS, CONTRACT_BASH_FOREGROUND_DEFAULT_TIMEOUT_MS, contract_bash_timeout_ms,
    };

    // The reported hang: `bash` in the foreground with no timeout. It used to
    // resolve to BASH_MAX_TIMEOUT_MS (~24.8 days), so an unauthenticated CLI
    // waiting on a prompt held the turn open indefinitely. It now takes the
    // default the tool's own schema advertises, which is what arms the
    // kill-and-rerun-in-background recovery.
    assert_eq!(
        contract_bash_timeout_ms(true, None, false, false),
        Some(CONTRACT_BASH_FOREGROUND_DEFAULT_TIMEOUT_MS)
    );
    const { assert!(CONTRACT_BASH_FOREGROUND_DEFAULT_TIMEOUT_MS < BASH_MAX_TIMEOUT_MS) };

    // An explicit request still wins, including one far above the default:
    // long foreground work stays possible when the model asks for it.
    assert_eq!(
        contract_bash_timeout_ms(true, Some(1_800_000), false, false),
        Some(1_800_000)
    );
    assert_eq!(
        contract_bash_timeout_ms(true, Some(5), false, false),
        Some(5)
    );

    // Background and interactive runs are meant to outlive the call, so they
    // keep "no timeout" and are never bounded by the foreground default.
    assert_eq!(contract_bash_timeout_ms(true, None, true, false), None);
    assert_eq!(contract_bash_timeout_ms(true, None, false, true), None);

    // The standalone Bash tools already resolve their own default upstream;
    // this helper must not second-guess the value they pass in.
    assert_eq!(
        contract_bash_timeout_ms(false, Some(120_000), false, false),
        Some(120_000)
    );
}

#[cfg(all(unix, not(target_env = "ohos")))]
#[test]
fn inherited_interactive_terminal_fails_closed_before_spawn() {
    let workspace = tempdir().expect("workspace");
    let mut manager = ShellManager::new(workspace.path().to_path_buf());
    let err = manager
        .execute_interactive_with_policy_env("codew", None, 10_000, None, HashMap::new())
        .expect_err("Unix inherited-terminal takeover must not spawn");
    let message = err.to_string();
    assert!(message.contains("foreground TTY ownership"), "{message}");
    assert!(message.contains("background: true, tty: true"), "{message}");
    assert!(message.contains("action: \"interact\""), "{message}");
    assert!(message.contains("task_id"), "{message}");
    assert!(message.contains("terminal/run"), "{message}");
    assert!(message.contains("terminal/send"), "{message}");
}

#[cfg(all(unix, target_env = "ohos"))]
#[test]
fn inherited_interactive_terminal_offers_only_ohos_recovery_paths() {
    let workspace = tempdir().expect("workspace");
    let mut manager = ShellManager::new(workspace.path().to_path_buf());
    let err = manager
        .execute_interactive_with_policy_env("codew", None, 10_000, None, HashMap::new())
        .expect_err("OHOS inherited-terminal takeover must not spawn");
    let message = err.to_string();
    assert!(message.contains("foreground TTY ownership"), "{message}");
    assert!(message.contains("new terminal"), "{message}");
    assert!(message.contains("omit `interactive: true`"), "{message}");
    assert!(!message.contains("background: true"), "{message}");
    assert!(!message.contains("terminal/run"), "{message}");
}

#[test]
fn contract_bash_nonzero_is_an_error_with_status_after_output() {
    let error = finish_contract_bash_result(
        ShellResult {
            task_id: None,
            status: ShellStatus::Failed,
            exit_code: Some(7),
            stdout: "before".to_string(),
            stderr: String::new(),
            duration_ms: 1,
            stdout_len: 6,
            stderr_len: 0,
            stdout_omitted: 0,
            stderr_omitted: 0,
            stdout_truncated: false,
            stderr_truncated: false,
            sandboxed: false,
            sandbox_type: None,
            sandbox_denied: false,
        },
        None,
        &ToolContext::new("."),
        None,
        false,
        None,
    )
    .expect_err("nonzero must be a failed tool call");
    assert!(
        error
            .to_string()
            .ends_with("before\n\nCommand exited with code 7")
    );
    // The error keeps the metadata a success carries, so hooks still see the
    // exit code and status of a failing command.
    let metadata = error.metadata().expect("failure metadata");
    assert_eq!(metadata["exit_code"], 7);
    assert_eq!(metadata["status"], "Failed");
}

#[cfg(unix)]
#[tokio::test]
async fn lowercase_bash_returns_one_ordered_stream() {
    let workspace = tempdir().expect("workspace");
    let context = ToolContext::new(workspace.path());
    let result = LowercaseBashTool
        .execute(
            json!({"command": "printf out-1; printf err-2 >&2; printf out-3"}),
            &context,
        )
        .await
        .expect("bash");
    assert_eq!(result.content, "out-1err-2out-3");
}

/// #6689: the receipt is exact-or-absent, fits the hook bound after JSON
/// escaping, and reports how the run ended without inventing an exit code.
#[test]
fn execution_receipt_is_bounded_and_reports_truthful_state() {
    let tmp = tempdir().unwrap();
    let identity = |command: &str, cwd: &Path| ShellExecutionIdentity {
        command: command.to_string(),
        cwd: cwd.to_path_buf(),
    };
    let cwd = tmp.path().canonicalize().unwrap();
    let mut result = failed_network_shell_result(
        &"\u{1f40b}\n\"\u{1}".repeat(12_000),
        &"err\n".repeat(12_000),
    );
    result.sandboxed = false;
    result.sandbox_type = None;
    result.stdout_truncated = true;
    result.exit_code = None;
    result.status = ShellStatus::Killed;

    let receipt = shell_execution_receipt(&identity("printf hi", &cwd), &result, "separate")
        .expect("receipt");
    let encoded = serde_json::to_string(&receipt).unwrap();
    assert!(encoded.len() <= crate::hooks::HOOK_EXECUTION_RECEIPT_MAX_BYTES);
    assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), receipt);
    assert_eq!(receipt["schema_version"], 1);
    assert_eq!(receipt["command"], "printf hi");
    assert_eq!(receipt["cwd"], cwd.to_str().unwrap());
    assert_eq!(receipt["state"], "interrupted");
    assert!(receipt["exit_code"].is_null());
    assert_eq!(receipt["stdout_truncated"], true);
    assert_eq!(receipt["stderr_truncated"], true);
    assert!(
        receipt["stderr"]
            .as_str()
            .unwrap()
            .contains("[receipt preview truncated]")
    );

    // A nonzero exit is a completed run, and a 64-bit code survives.
    result.status = ShellStatus::Failed;
    result.exit_code = Some(3_221_225_477);
    let receipt = shell_execution_receipt(&identity("x", &cwd), &result, "combined").unwrap();
    assert_eq!(receipt["state"], "completed");
    assert_eq!(receipt["exit_code"], 3_221_225_477_i64);
    assert_eq!(receipt["output_kind"], "combined");

    result.status = ShellStatus::TimedOut;
    result.exit_code = None;
    let receipt = shell_execution_receipt(&identity("x", &cwd), &result, "separate").unwrap();
    assert_eq!(receipt["state"], "interrupted");

    // Absent, never truncated or guessed.
    result.status = ShellStatus::Running;
    assert!(shell_execution_receipt(&identity("x", &cwd), &result, "separate").is_none());
    result.status = ShellStatus::Completed;
    for command in ["", "bad\0command"] {
        assert!(shell_execution_receipt(&identity(command, &cwd), &result, "separate").is_none());
    }
    let long = "x".repeat(EXECUTION_RECEIPT_IDENTITY_MAX_BYTES + 1);
    assert!(shell_execution_receipt(&identity(&long, &cwd), &result, "separate").is_none());
    let long_cwd = PathBuf::from(format!("/{long}"));
    assert!(shell_execution_receipt(&identity("x", &long_cwd), &result, "separate").is_none());
    assert!(
        shell_execution_receipt(&identity("x", Path::new("relative")), &result, "separate")
            .is_none()
    );
    result.sandboxed = true;
    assert!(shell_execution_receipt(&identity("x", &cwd), &result, "separate").is_none());
}

/// #6689: a settled foreground run records the command and directory the
/// process manager spawned, on success and on failure, and background runs
/// carry no receipt. The workspace is opened through a symlink so the default
/// directory and an explicit `cwd` would otherwise be spelled differently.
#[cfg(unix)]
#[tokio::test]
async fn foreground_shell_results_carry_the_spawned_execution_receipt() {
    let tmp = tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir_all(real.join("child")).unwrap();
    let workspace = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &workspace).unwrap();
    let mut context = ToolContext::new(workspace.clone())
        .with_elevated_sandbox_policy(ExecutionSandboxPolicy::DangerFullAccess);
    context.auto_approve = true;
    let tool = BashTool::new("Bash");

    // No completion hook registered: nothing reads a receipt, so none rides
    // along in the metadata the Runtime API persists.
    let unobserved = tool
        .execute(json!({"command": "pwd"}), &context)
        .await
        .unwrap();
    assert!(
        unobserved
            .metadata
            .as_ref()
            .unwrap()
            .get("execution_receipt")
            .is_none()
    );

    let hooks = crate::hooks::HookExecutor::new(
        codewhale_config::hooks::HooksConfig {
            enabled: true,
            hooks: vec![codewhale_config::hooks::Hook::new(
                codewhale_config::hooks::HookEvent::ToolCallAfter,
                "true",
            )],
            ..codewhale_config::hooks::HooksConfig::default()
        },
        workspace.clone(),
    );
    context.runtime.hook_executor = Some(std::sync::Arc::new(hooks));
    // Exact strings, not re-canonicalized: both spellings must already agree.
    let real = real.canonicalize().unwrap();
    let real_str = real.to_str().unwrap();
    let child_str = real.join("child");
    let child_str = child_str.to_str().unwrap();

    let default_dir = tool
        .execute(json!({"command": "pwd"}), &context)
        .await
        .unwrap();
    let explicit_dir = tool
        .execute(json!({"command": "pwd", "cwd": "."}), &context)
        .await
        .unwrap();
    for result in [&default_dir, &explicit_dir] {
        assert_eq!(
            result.metadata.as_ref().unwrap()["execution_receipt"]["cwd"],
            real_str
        );
    }

    let command = "printf effective; printf diagnostic >&2; exit 7";
    let result = tool
        .execute(json!({"command": command, "cwd": "child"}), &context)
        .await
        .unwrap();
    let receipt = &result.metadata.as_ref().unwrap()["execution_receipt"];
    assert_eq!(receipt["command"], command);
    assert_eq!(receipt["cwd"], child_str);
    assert_eq!(receipt["stdout"], "effective");
    assert_eq!(receipt["stderr"], "diagnostic");
    assert_eq!(receipt["exit_code"], 7);
    assert_eq!(receipt["state"], "completed");
    assert_eq!(receipt["output_kind"], "separate");

    let interrupted = tool
        .execute(json!({"command": "kill -TERM $$"}), &context)
        .await
        .unwrap();
    let receipt = &interrupted.metadata.as_ref().unwrap()["execution_receipt"];
    assert_eq!(receipt["state"], "interrupted");
    assert!(receipt["exit_code"].is_null());

    let background = tool
        .execute(
            json!({"command": "printf background", "background": true}),
            &context,
        )
        .await
        .unwrap();
    assert!(
        background
            .metadata
            .as_ref()
            .unwrap()
            .get("execution_receipt")
            .is_none()
    );

    // Lowercase `bash` shares one pipe: the preview is combined output, and a
    // failing command's error still carries the receipt for hooks.
    let result = LowercaseBashTool
        .execute(
            json!({"command": "printf merged; printf diagnostic >&2"}),
            &context,
        )
        .await
        .unwrap();
    let receipt = &result.metadata.as_ref().unwrap()["execution_receipt"];
    assert_eq!(receipt["output_kind"], "combined");
    assert_eq!(receipt["stdout"], "mergeddiagnostic");
    assert_eq!(receipt["stderr"], "");
    assert_eq!(receipt["state"], "completed");
    let error = LowercaseBashTool
        .execute(json!({"command": "printf partial; exit 3"}), &context)
        .await
        .expect_err("nonzero exit is a failed bash call");
    let receipt = &error.metadata().expect("failure metadata")["execution_receipt"];
    assert_eq!(receipt["command"], "printf partial; exit 3");
    assert_eq!(receipt["exit_code"], 3);
    assert_eq!(receipt["state"], "completed");
    assert_eq!(receipt["cwd"], real_str);
}

/// A post-run lookup of a retargeted workspace symlink would falsely name
/// the replacement directory. The receipt must keep the actual spawn path.
#[cfg(unix)]
#[tokio::test]
async fn execution_receipt_keeps_spawn_cwd_when_the_command_retargets_the_workspace() {
    let tmp = tempdir().unwrap();
    let real = tmp.path().join("real");
    let other = tmp.path().join("other");
    std::fs::create_dir(&real).unwrap();
    std::fs::create_dir(&other).unwrap();
    let workspace = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &workspace).unwrap();
    let mut context = ToolContext::new(workspace.clone())
        .with_elevated_sandbox_policy(ExecutionSandboxPolicy::DangerFullAccess);
    context.auto_approve = true;
    context.runtime.hook_executor = Some(std::sync::Arc::new(crate::hooks::HookExecutor::new(
        codewhale_config::hooks::HooksConfig {
            enabled: true,
            hooks: vec![codewhale_config::hooks::Hook::new(
                codewhale_config::hooks::HookEvent::ToolCallAfter,
                "true",
            )],
            ..codewhale_config::hooks::HooksConfig::default()
        },
        workspace.clone(),
    )));
    let command = "pwd -P; rm ../link; ln -s other ../link; pwd -P";
    let result = BashTool::new("Bash")
        .execute(json!({"command": command}), &context)
        .await
        .unwrap();
    assert!(result.success);
    let receipt = &result.metadata.as_ref().unwrap()["execution_receipt"];
    let actual = real.canonicalize().unwrap();
    let actual = actual.to_str().unwrap();
    assert_eq!(receipt["command"], command);
    assert_eq!(receipt["cwd"], actual);
    assert_eq!(receipt["stdout"], format!("{actual}\n{actual}\n"));
    assert_eq!(receipt["exit_code"], 0);
    assert_eq!(
        workspace.canonicalize().unwrap(),
        other.canonicalize().unwrap()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn lowercase_bash_keeps_raw_command_under_readonly_policy() {
    let workspace = tempdir().expect("workspace");
    let context = ToolContext::new(workspace.path())
        .with_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly);
    let result = LowercaseBashTool
        .execute(json!({"command": "pwd"}), &context)
        .await
        .expect("read-only bash");
    assert_eq!(
        result.content.trim(),
        workspace
            .path()
            .canonicalize()
            .expect("canonical workspace")
            .display()
            .to_string()
    );
}

#[tokio::test]
async fn lowercase_bash_readonly_refusal_names_work_mode() {
    let workspace = tempdir().expect("workspace");
    let context = ToolContext::new(workspace.path())
        .with_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly);
    let error = LowercaseBashTool
        .execute(json!({"command": "touch blocked-by-plan"}), &context)
        .await
        .expect_err("policy refusal is a typed denial");

    assert!(
        matches!(error, ToolError::PermissionDenied { .. }),
        "{error}"
    );
    let message = error.to_string();
    assert!(
        message.contains("The user can change modes with /mode."),
        "{message}"
    );
    assert!(!message.contains("Work mode"));
    assert!(!message.contains("switch to"), "{message}");
    assert!(!workspace.path().join("blocked-by-plan").exists());
}

/// Regression for the wedge that took out the owner's own session under swap
/// exhaustion: the lowercase `bash` spill file could not be created (full temp
/// volume), every call — including `echo ok` — failed with the harness-internal
/// "Failed to create streaming shell output", and nothing recovered. Spill
/// failure must be soft: the command still runs, the tail is still returned,
/// the next call still works, and no job state leaks.
#[cfg(unix)]
#[tokio::test]
async fn lowercase_bash_survives_spill_file_failure_and_stays_usable() {
    let workspace = tempdir().expect("workspace");
    let context = ToolContext::new(workspace.path());
    let missing_spill_dir = workspace.path().join("no-such-temp-volume");
    assert!(!missing_spill_dir.exists());
    context
        .shell_manager
        .lock()
        .expect("shell manager")
        .set_output_spill_dir_for_test(Some(missing_spill_dir.clone()));

    // (ii) the command runs and (i) no harness-internal error leaks.
    let first = LowercaseBashTool
        .execute(json!({"command": echo_command("ok")}), &context)
        .await
        .expect("bash runs even when the spill file cannot be created");
    assert!(first.success, "{}", first.content);
    assert_eq!(first.content.trim(), "ok");
    assert!(
        !first
            .content
            .contains("Failed to create streaming shell output")
    );

    // The next call must work too — the whole point of the fix.
    let second = LowercaseBashTool
        .execute(json!({"command": echo_command("still-ok")}), &context)
        .await
        .expect("second bash call after a spill failure");
    assert!(second.success, "{}", second.content);
    assert_eq!(second.content.trim(), "still-ok");

    // Output past the bound is still delivered, and the notice explains why the
    // full-output path is missing instead of pointing at a file that was never
    // written.
    let long = LowercaseBashTool
        .execute(
            json!({"command": "i=0; while [ $i -lt 2100 ]; do echo line-$i; i=$((i+1)); done"}),
            &context,
        )
        .await
        .expect("long bash output without a spill file");
    assert!(long.success, "{}", long.content);
    assert!(long.content.contains("line-2099"));
    assert!(
        long.content.contains("Full output was not persisted:"),
        "{}",
        long.content
    );
    assert!(!long.content.contains("Full output: "));

    // (iii) no leaked session state: nothing is still running or unowned-pending.
    let mut manager = context.shell_manager.lock().expect("shell manager");
    let running = manager
        .list_jobs()
        .into_iter()
        .filter(|job| job.status == ShellStatus::Running)
        .count();
    assert_eq!(running, 0, "no shell job may be left running");
    assert!(
        !missing_spill_dir.exists(),
        "fail-soft must not create the dir"
    );
}

/// A spawn/stream failure caused by host exhaustion must reach the model as
/// an actionable message (cause chain + likely reason + retry), never as a
/// bare harness-internal context string.
#[test]
fn shell_execution_failure_names_resource_exhaustion_and_says_retry() {
    let error = anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::StorageFull))
        .context("Failed to open PTY");
    let message = shell_execution_failed_message(&error);
    assert!(
        message.starts_with("Shell execution failed: Failed to open PTY"),
        "{message}"
    );
    assert!(
        message.contains("Likely host resource exhaustion"),
        "{message}"
    );
    assert!(message.contains("disk"), "{message}");
    assert!(message.contains("retry"), "{message}");
    assert!(message.contains("still usable"), "{message}");

    #[cfg(unix)]
    {
        let error = anyhow::Error::from(std::io::Error::from_raw_os_error(libc::EMFILE))
            .context("Failed to spawn PTY command: echo ok");
        let message = shell_execution_failed_message(&error);
        assert!(message.contains("file descriptors"), "{message}");
        assert!(message.contains("echo ok"), "{message}");
    }

    let plain = anyhow::anyhow!("working directory does not exist");
    let message = shell_execution_failed_message(&plain);
    assert_eq!(
        message,
        "Shell execution failed: working directory does not exist"
    );
}

fn execute_shell(
    manager: &mut ShellManager,
    command: &str,
    working_dir: Option<&str>,
    timeout_ms: u64,
    background: bool,
) -> Result<ShellResult> {
    manager.execute_with_options_env_for_session(
        command,
        working_dir,
        timeout_ms,
        background,
        None,
        false,
        None,
        HashMap::new(),
        "workspace",
    )
}

#[test]
fn deleted_saved_workspace_reports_path_and_recovery_before_spawn() {
    let workspace = tempdir().expect("workspace");
    let stale = workspace.path().join("deleted-session-workspace");
    let mut manager = ShellManager::new(stale.clone());

    let error = execute_shell(&mut manager, "echo should-not-run", None, 1_000, false)
        .expect_err("missing saved workspace must fail before shell spawn");
    let message = error.to_string();
    assert!(message.contains("saved session workspace is unavailable"));
    assert!(message.contains(&stale.display().to_string()));
    assert!(message.contains("working_dir") || message.contains("cwd"));
    assert!(message.contains("resume/fork"));
}

#[test]
fn explicit_missing_working_dir_is_not_misreported_as_session_corruption() {
    let workspace = tempdir().expect("workspace");
    let missing = workspace.path().join("explicit-missing");
    let mut manager = ShellManager::new(workspace.path().to_path_buf());

    let error = execute_shell(
        &mut manager,
        "echo should-not-run",
        missing.to_str(),
        1_000,
        false,
    )
    .expect_err("missing explicit cwd must fail before shell spawn");
    let message = error.to_string();
    assert!(message.contains("requested working directory is unavailable"));
    assert!(message.contains(&missing.display().to_string()));
    assert!(!message.contains("saved session workspace"));
}

#[cfg(not(target_env = "ohos"))]
#[test]
fn pty_exit_status_preserves_high_windows_code_losslessly() {
    let raw = 0xC000_0005;
    let status = ShellExitStatus::from_pty(portable_pty::ExitStatus::with_exit_code(raw));

    assert!(!status.success);
    assert_eq!(status.code, Some(i64::from(raw)));
    assert_eq!(
        exit_code_label(status.code),
        "exit code 3221225477 (0xC0000005)"
    );
    assert_eq!(exit_code_hex(status.code).as_deref(), Some("0xC0000005"));
}

#[cfg(not(target_env = "ohos"))]
#[test]
fn ordinary_pty_exit_status_keeps_concise_label() {
    let status = ShellExitStatus::from_pty(portable_pty::ExitStatus::with_exit_code(127));

    assert_eq!(status.code, Some(127));
    assert_eq!(exit_code_label(status.code), "exit code 127");
    assert_eq!(exit_code_hex(status.code), None);
}

#[cfg(windows)]
#[test]
fn std_windows_exit_status_reinterprets_signed_dword() {
    assert_eq!(std_exit_code_i64(0xC000_0005_u32 as i32), 0xC000_0005);
}

#[cfg(windows)]
const JOB_OBJECT_QUERY_ACCESS: u32 = 0x0004;

#[cfg(windows)]
fn duplicate_job_without_terminate_access(job: WindowsJob) -> WindowsJob {
    let process = unsafe { GetCurrentProcess() };
    let mut limited_handle = HANDLE::default();

    unsafe {
        DuplicateHandle(
            process,
            job.handle,
            process,
            &mut limited_handle,
            JOB_OBJECT_QUERY_ACCESS,
            false,
            DUPLICATE_HANDLE_OPTIONS(0),
        )
        .expect("duplicate job handle without terminate access");
    }

    drop(job);
    WindowsJob {
        handle: limited_handle,
    }
}

fn echo_command(message: &str) -> String {
    format!("echo {message}")
}

fn sleep_command(seconds: u64) -> String {
    let dispatcher = codewhale_sandbox::shell_dispatcher::global_dispatcher();
    if dispatcher.kind().is_powershell() {
        return format!("Start-Sleep -Seconds {seconds}");
    }
    #[cfg(windows)]
    {
        let ping_count = seconds.saturating_add(1);
        format!("ping 127.0.0.1 -n {ping_count} > NUL")
    }
    #[cfg(not(windows))]
    {
        format!("sleep {seconds}")
    }
}

fn sleep_then_echo_command(seconds: u64, message: &str) -> String {
    let dispatcher = codewhale_sandbox::shell_dispatcher::global_dispatcher();
    if dispatcher.kind().is_powershell() {
        return format!("Start-Sleep -Seconds {seconds}; echo {message}");
    }
    #[cfg(windows)]
    {
        let ping_count = seconds.saturating_add(1);
        format!("ping 127.0.0.1 -n {ping_count} > NUL && echo {message}")
    }
    #[cfg(not(windows))]
    {
        format!("sleep {seconds} && echo {message}")
    }
}

fn echo_stdin_command() -> String {
    let dispatcher = codewhale_sandbox::shell_dispatcher::global_dispatcher();
    if dispatcher.kind().is_powershell() {
        return "[Console]::In.ReadToEnd()".to_string();
    }
    #[cfg(windows)]
    {
        "more".to_string()
    }
    #[cfg(not(windows))]
    {
        "cat".to_string()
    }
}

fn network_restricted_context(tmp: &std::path::Path) -> ToolContext {
    ToolContext::new(tmp)
        .with_elevated_sandbox_policy(ExecutionSandboxPolicy::WorkspaceWrite {
            writable_roots: vec![tmp.to_path_buf()],
            network_access: false,
            exclude_tmpdir: false,
            exclude_slash_tmp: false,
        })
        .with_shell_network_denied_hint(
            "Shell command blocked: Plan mode runs shell commands in a network-restricted sandbox.",
        )
}

fn failed_network_shell_result(stdout: &str, stderr: &str) -> ShellResult {
    ShellResult {
        task_id: None,
        status: ShellStatus::Failed,
        exit_code: Some(6),
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
        duration_ms: 25,
        stdout_len: stdout.len(),
        stderr_len: stderr.len(),
        stdout_omitted: 0,
        stderr_omitted: 0,
        stdout_truncated: false,
        stderr_truncated: false,
        sandboxed: true,
        sandbox_type: Some("seatbelt".to_string()),
        sandbox_denied: false,
    }
}

#[cfg(unix)]
const SHELL_DESCENDANT_HELPER_ENV: &str = "CODEWHALE_SHELL_DESCENDANT_HELPER";
#[cfg(unix)]
const SHELL_DESCENDANT_PID_FILE_ENV: &str = "CODEWHALE_SHELL_DESCENDANT_PID_FILE";

#[cfg(unix)]
#[test]
fn shell_descendant_helper_process() {
    if std::env::var(SHELL_DESCENDANT_HELPER_ENV).ok().as_deref() != Some("1") {
        return;
    }
    let pid_file =
        PathBuf::from(std::env::var(SHELL_DESCENDANT_PID_FILE_ENV).expect("descendant pid file"));
    let mut child = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn cheap descendant");
    std::fs::write(pid_file, child.id().to_string()).expect("write descendant pid");
    std::thread::sleep(Duration::from_secs(30));
    let _ = child.wait();
}

#[cfg(unix)]
fn wait_for_shell_pid_file(path: &Path) -> libc::pid_t {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(raw) = std::fs::read_to_string(path)
            && let Ok(pid) = raw.trim().parse()
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "descendant pid file never appeared"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
fn wait_for_shell_pid_exit(pid: libc::pid_t) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if unsafe { libc::kill(pid, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_completed_shell(manager: &mut ShellManager, task_id: &str) -> ShellResult {
    let deadline = Instant::now() + Duration::from_millis(BACKGROUND_COMPLETION_WAIT_MS);

    loop {
        let result = manager
            .get_output(task_id, true, 1_000)
            .expect("get_output");
        if result.status != ShellStatus::Running || Instant::now() >= deadline {
            return result;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn shell_owner_registers_before_spawn_and_silent_work_stays_live() {
    let work = crate::work_graph::new_shared_work_runtime(
        crate::tools::todo::new_shared_todo_list(),
        crate::tools::plan::new_shared_plan_state(),
    );
    let lifecycle = ShellWorkLifecycle {
        work: work.clone(),
        session_id: "shell-session".to_string(),
    };

    {
        let _guard = ShellSpawnIntentGuard::new(
            Some(lifecycle.clone()),
            "shell_spawn_failure",
            "missing-program",
        );
    }
    lifecycle
        .register("shell_silent", "sleep 30")
        .expect("register silent shell");
    lifecycle
        .observe("shell_silent", &ShellStatus::Running, 1, 0)
        .expect("live owner observation");
    lifecycle
        .observe("shell_silent", &ShellStatus::Running, 2, 512)
        .expect("growing output observation");

    let graph = work
        .capture(Some("shell-session"))
        .expect("capture")
        .expect("graph")
        .graph;
    let operation = |external: &str| {
        graph.nodes.iter().find(|node| {
            node.binding
                .as_ref()
                .is_some_and(|binding| binding.external == external)
        })
    };
    assert_eq!(
        operation("shell:shell_spawn_failure").map(|node| node.state),
        Some(crate::work_graph::NodeState::Failed),
        "dropping an armed spawn guard must terminalize pre-spawn failure"
    );
    let silent = operation("shell:shell_silent").expect("silent shell operation");
    assert_eq!(silent.state, crate::work_graph::NodeState::Active);
    let observation = silent
        .binding
        .as_ref()
        .and_then(|binding| binding.last_observation.as_ref())
        .expect("last shell observation");
    assert_eq!(observation.seq, 2);
    assert_eq!(
        observation
            .output
            .as_ref()
            .and_then(crate::work_graph::EvidenceRef::raw_bytes),
        Some(512)
    );
}

#[test]
fn exec_shell_parallel_flags_are_input_aware() {
    let tool = BashTool::new("Bash");
    let readonly = json!({"command": "git status -s"});
    assert!(tool.supports_parallel_for(&readonly));
    assert!(tool.is_read_only_for(&readonly));
    assert_eq!(
        tool.approval_requirement_for(&readonly),
        ApprovalRequirement::Auto
    );

    for input in [
        json!({"command": "fd -e rs ."}),
        json!({"command": "fd -H --type f src"}),
        json!({"command": "git grep TODO crates/engine/src/tools"}),
        json!({"action": "run", "command": "gh issue list --limit 10"}),
        json!({"action": "run", "command": "gh issue view 5287"}),
    ] {
        assert!(tool.supports_parallel_for(&input), "{input:?}");
        assert!(tool.is_read_only_for(&input), "{input:?}");
        assert_eq!(
            tool.approval_requirement_for(&input),
            ApprovalRequirement::Auto,
            "{input:?}"
        );
    }

    for input in [
        json!({"command": "git status -s", "background": true}),
        json!({"command": "git status -s", "background": "false"}),
        json!({"command": "git status -s", "stdin": ""}),
        json!({"action": "wait", "command": "pwd", "task_id": "shell_1"}),
        json!({"action": "interact", "command": "pwd", "task_id": "shell_1"}),
        json!({"action": "cancel", "command": "pwd", "task_id": "shell_1"}),
        json!({"action": 3, "command": "pwd"}),
        json!({"command": "pwd", "unexpected": true}),
        json!({"command": "cargo build"}),
        json!({"command": "bash -lc 'git status'"}),
        json!({"command": "sh -c 'rg TODO crates'"}),
        json!({"command": "PAGER=./pwn.sh git log"}),
        json!({"command": "GH_PAGER=./pwn.sh gh issue view 5287"}),
        json!({"command": "rg ${9:---pre=./repo-script} needle ."}),
        json!({"command": "rg ${9:---hostname-bin=./repo-script} needle ."}),
        json!({"command": "fd ${9:---exec} ./repo-script"}),
        json!({"command": "rg $PATTERN ."}),
        json!({"command": "rg *.rs ."}),
        json!({"command": "bash -lc 'rg TODO crates | head'"}),
        json!({"command": "fd -x ./pwn.sh"}),
        json!({"command": "fd --exec ./pwn.sh"}),
        json!({"command": "fd -uHtx ./pwn.sh"}),
        json!({"command": "rg --pre /tmp/evil.sh needle ."}),
        json!({"command": "rg --hostname-bin ./repo-script --hyperlink-format=file://{host}{path} needle ."}),
        json!({"command": "rg --search-zip needle ."}),
        json!({"command": "rg -z needle ."}),
        json!({"command": "git grep -O needle"}),
        json!({"command": "git grep -nO needle"}),
        json!({"command": "git grep --textconv needle"}),
        json!({"command": "git diff --ext-diff HEAD"}),
        json!({"command": "git diff --textconv HEAD"}),
        json!({"command": "git log --show-signature -1"}),
        json!({"command": "git show --format=%GS HEAD"}),
        json!({"command": "gh issue close 5287"}),
        json!({"command": "gh issue view 5287 > issue.txt"}),
        json!({"command": "gh pr checks 42 --watch"}),
        json!({"command": "gh issue view 5287 -R git.example.com/o/r"}),
    ] {
        assert!(!tool.supports_parallel_for(&input), "{input:?}");
        assert!(!tool.is_read_only_for(&input), "{input:?}");
        assert_eq!(
            tool.approval_requirement_for(&input),
            ApprovalRequirement::Required,
            "{input:?}"
        );
    }

    assert!(tool.starts_detached_for(&json!({
        "command": "cargo check --workspace",
        "background": true
    })));
    assert!(tool.starts_detached_for(&json!({
        "command": "cargo test -p codewhale-tui --bins",
        "tty": true
    })));
    assert!(!tool.starts_detached_for(&json!({
        "command": "cargo check --workspace"
    })));
    assert!(!tool.starts_detached_for(&json!({
        "command": "cargo check --workspace",
        "background": true,
        "interactive": true
    })));
}

#[tokio::test]
async fn readonly_shell_refuses_raw_string_external_backend() {
    struct Backend(std::sync::atomic::AtomicBool);
    #[async_trait::async_trait]
    impl codewhale_sandbox::backend::SandboxBackend for Backend {
        fn kind(&self) -> codewhale_sandbox::backend::SandboxKind {
            codewhale_sandbox::backend::SandboxKind::OpenSandbox
        }
        async fn exec(
            &self,
            _cmd: &str,
            _env: &std::collections::HashMap<String, String>,
        ) -> anyhow::Result<codewhale_sandbox::backend::SandboxOutput> {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(codewhale_sandbox::backend::SandboxOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
            })
        }
    }

    let tmp = tempdir().expect("tempdir");
    let backend = std::sync::Arc::new(Backend(std::sync::atomic::AtomicBool::new(false)));
    let mut context = ToolContext::new(tmp.path().to_path_buf())
        .with_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly);
    context.sandbox_backend = Some(backend.clone());
    let error = BashTool::read_only("Bash")
        .execute(json!({"action": "run", "command": "pwd"}), &context)
        .await
        .expect_err("raw-string backend must not receive a classifier-approved argv")
        .to_string();
    assert!(error.contains("raw command string"), "{error}");
    assert!(!backend.0.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn lowercase_bash_refuses_non_streaming_external_backend() {
    struct Backend(std::sync::atomic::AtomicBool);
    #[async_trait::async_trait]
    impl codewhale_sandbox::backend::SandboxBackend for Backend {
        fn kind(&self) -> codewhale_sandbox::backend::SandboxKind {
            codewhale_sandbox::backend::SandboxKind::OpenSandbox
        }
        async fn exec(
            &self,
            _cmd: &str,
            _env: &std::collections::HashMap<String, String>,
        ) -> anyhow::Result<codewhale_sandbox::backend::SandboxOutput> {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            unreachable!("lowercase bash must fail before external dispatch")
        }
    }

    let workspace = tempdir().expect("workspace");
    let backend = std::sync::Arc::new(Backend(std::sync::atomic::AtomicBool::new(false)));
    let mut context = ToolContext::new(workspace.path());
    context.sandbox_backend = Some(backend.clone());
    let error = LowercaseBashTool
        .execute(json!({"command": "pwd", "timeout": 1}), &context)
        .await
        .expect_err("non-streaming backend must be rejected");
    assert!(error.to_string().contains("combined streaming output"));
    assert!(!backend.0.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn readonly_argv_is_shell_free_and_disables_git_helpers() {
    let (program, args) = hardened_readonly_argv("git show HEAD").expect("argv");
    assert_eq!(program, "git");
    assert_eq!(
        &args[..6],
        [
            "show",
            "--no-ext-diff",
            "--no-textconv",
            "--submodule=short",
            "--ignore-submodules=dirty",
            "--no-show-signature"
        ]
    );
    assert_eq!(args.last().map(String::as_str), Some("HEAD"));

    let (_, args) = hardened_readonly_argv("git -C sub blame f.txt").expect("blame argv");
    assert_eq!(args, ["-C", "sub", "blame", "--no-textconv", "f.txt"]);
    assert_eq!(
        readonly_git_dirs(
            "git -C sub diff | git --no-pager -C /abs status | cat",
            std::path::Path::new("/ws")
        ),
        [
            std::path::PathBuf::from("/ws/sub"),
            std::path::PathBuf::from("/abs")
        ]
    );
    // Chains are split like the classifier splits them, so a `git` read
    // after `&&`, `||` or `;` still gets filter overrides.
    assert_eq!(
        readonly_git_dirs(
            "pwd && git diff; ls || git -C sub status",
            std::path::Path::new("/ws")
        ),
        [
            std::path::PathBuf::from("/ws"),
            std::path::PathBuf::from("/ws/sub")
        ]
    );

    let (program, args) = hardened_readonly_argv("rg $PATTERN .").expect("literal argv");
    assert_eq!(program, "rg");
    assert_eq!(args, ["$PATTERN", "."]);
}

/// Classifier-approved working-tree reads must not run the repository's clean
/// filter or textconv driver. The helpers print a sentinel instead of the
/// content, so this holds whether or not a kernel sandbox blocks their writes.
#[cfg(unix)]
#[tokio::test]
async fn readonly_git_reads_run_no_clean_filter_or_textconv() {
    use codewhale_dependencies::ExternalTool as _;
    use std::os::unix::fs::PermissionsExt as _;
    let workspace = tempdir().expect("workspace");
    let outside = tempdir().expect("outside");
    let helper = outside.path().join("helper.sh");
    std::fs::write(&helper, "#!/bin/sh\necho HELPER-RAN\n").expect("helper");
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let helper = helper.display().to_string();
    let repo = workspace.path();
    let git = |args: &[&str]| {
        let status = codewhale_dependencies::Git::status(args, repo).expect("git should spawn");
        assert!(status.success(), "git {args:?} failed");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "Test"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join(".gitattributes"), "c.txt filter=x diff=conv\n").expect("attrs");
    std::fs::write(repo.join("c.txt"), "c1\n").expect("write");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);
    git(&["config", "filter.x.clean", &helper]);
    git(&["config", "diff.conv.textconv", &helper]);
    std::fs::write(repo.join("c.txt"), "c2\n").expect("modify");

    let ctx = ToolContext::new(repo)
        .with_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly);
    let tool = BashTool::new("Bash");
    for command in ["git diff", "git blame c.txt"] {
        let result = tool
            .execute(json!({"command": command}), &ctx)
            .await
            .expect(command);
        assert!(result.success, "{command}: {}", result.content);
        assert!(
            !result.content.contains("HELPER-RAN"),
            "{command} ran a repository-configured command: {}",
            result.content
        );
        assert!(
            result.content.contains("c2"),
            "{command}: {}",
            result.content
        );
    }
}

#[cfg(any(unix, windows))]
#[test]
fn readonly_program_resolution_ignores_workspace_shadow_executables() {
    let workspace = tempdir().expect("workspace");
    let trusted = tempdir().expect("trusted bin");
    let path = std::env::join_paths([workspace.path(), trusted.path()]).expect("test PATH");

    for program in ["git", "gh", "rg"] {
        let file = if cfg!(windows) {
            format!("{program}.exe")
        } else {
            program.to_string()
        };
        for directory in [workspace.path(), trusted.path()] {
            let executable = directory.join(&file);
            std::fs::write(&executable, b"fixture").expect("fixture executable");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mut permissions = executable.metadata().unwrap().permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(&executable, permissions).unwrap();
            }
        }
        let resolved =
            resolve_readonly_program_from_path(program, workspace.path(), &path).expect("resolved");
        assert_eq!(resolved, trusted.path().join(file).canonicalize().unwrap());
        assert!(resolved.is_absolute() && !resolved.starts_with(workspace.path()));
    }
}

#[test]
fn readonly_child_env_removes_git_and_github_redirects() {
    let mut command = std::process::Command::new("unused");
    let redirects = [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_EXEC_PATH",
        "GIT_OBJECT_DIRECTORY",
        "GIT_SSH_COMMAND",
        "GH_CONFIG_DIR",
        "GH_OTHER_PATH",
    ];
    for key in redirects {
        command.env(key, "outside");
    }
    command.env(READONLY_ENV_MARKER, "1");
    let env = HashMap::from([(READONLY_ENV_MARKER.to_string(), "1".to_string())]);
    remove_readonly_redirect_env(&mut command, &env);
    for key in redirects {
        assert!(
            command
                .get_envs()
                .any(|(name, value)| name == std::ffi::OsStr::new(key) && value.is_none()),
            "{key} must be removed from the child environment"
        );
    }
    assert!(
        command
            .get_envs()
            .any(|(name, value)| name == READONLY_ENV_MARKER && value.is_none())
    );
}

#[test]
fn readonly_operands_are_workspace_bounded_and_symlink_aware() {
    let workspace = tempdir().expect("workspace");
    let outside = tempdir().expect("outside");
    std::fs::write(workspace.path().join("inside.txt"), "inside").expect("inside file");
    std::fs::write(outside.path().join("secret.txt"), "secret").expect("outside file");

    enforce_readonly_workspace_operands("cat inside.txt", workspace.path(), workspace.path())
        .expect("in-workspace operand");
    let inside_absolute = workspace
        .path()
        .join("inside.txt")
        .canonicalize()
        .expect("canonical inside file");
    enforce_readonly_workspace_operands(
        &format!("cat {}", inside_absolute.display()),
        workspace.path(),
        workspace.path(),
    )
    .expect("absolute in-workspace operand");

    let outside_absolute = outside
        .path()
        .join("secret.txt")
        .canonicalize()
        .expect("canonical outside file");
    let error = enforce_readonly_workspace_operands(
        &format!("cat {}", outside_absolute.display()),
        workspace.path(),
        workspace.path(),
    )
    .expect_err("absolute outside operand must fail")
    .to_string();
    assert!(error.contains("operand.outside_workspace"), "{error}");

    for command in [
        "cat ../secret.txt",
        "cat ~/.ssh/id_rsa",
        "cat /rooted-current-drive.txt",
        "cat C:secret",
        r"cat C:\secret",
        r"cat \\server\share\secret",
    ] {
        let error =
            enforce_readonly_workspace_operands(command, workspace.path(), workspace.path())
                .expect_err("out-of-workspace operand must fail")
                .to_string();
        assert!(error.contains("inside the workspace"), "{command}: {error}");
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            workspace.path().join("secret-link"),
        )
        .expect("outside symlink");
        let error = enforce_readonly_workspace_operands(
            "cat secret-link",
            workspace.path(),
            workspace.path(),
        )
        .expect_err("symlink escape must fail")
        .to_string();
        assert!(error.contains("resolves outside"), "{error}");

        let subdir = workspace.path().join("subdir");
        std::fs::create_dir(&subdir).expect("subdir");
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            subdir.join("secret-link"),
        )
        .expect("cwd-relative outside symlink");
        enforce_readonly_workspace_operands("cat secret-link", workspace.path(), &subdir)
            .expect_err("operands must resolve relative to the effective cwd");
    }
}

#[test]
fn windows_verbatim_and_drive_operands_survive_posix_split() {
    // `shell_words` splits with POSIX backslash-escaping, which silently eats
    // the separators of Windows absolute paths (`C:\Users\...` becomes
    // `C:Users...`) and mangles the `\\?\` verbatim prefix before operand
    // classification can see it. The protection doubles those backslashes so
    // the splitter round-trips the real path (the `\\?\` cases that the
    // verbatim strip alone could never reach).
    for (raw, expected) in [
        (r"\\?\C:\Users\foo\inside.txt", r"C:\Users\foo\inside.txt"),
        (r"C:\Users\foo\inside.txt", r"C:\Users\foo\inside.txt"),
        (r"\\server\share\secret", r"\\server\share\secret"),
        (r"\\.\device\path", r"\\.\device\path"),
    ] {
        let protected = normalize_windows_command_paths(&format!("cat {raw}"));
        let argv = shell_words::split(&protected).expect("split must succeed");
        assert_eq!(
            argv,
            vec!["cat".to_string(), expected.to_string()],
            "{raw} must survive the POSIX split"
        );
    }
}

#[test]
fn windows_path_protection_leaves_other_words_untouched() {
    // POSIX escapes, drive-relative spellings, and plain relative operands
    // are not Windows absolute paths and must round-trip unchanged.
    assert_eq!(
        normalize_windows_command_paths("echo a\\ b && cat inside.txt"),
        "echo a\\ b && cat inside.txt"
    );
    assert_eq!(
        normalize_windows_command_paths("cat C:secret"),
        "cat C:secret"
    );
    assert_eq!(
        normalize_windows_command_paths("cat inside.txt"),
        "cat inside.txt"
    );
}

#[test]
fn readonly_github_shell_calls_obey_the_host_network_policy_before_spawn() {
    let tmp = tempdir().expect("tempdir");
    let context = |default| {
        ToolContext::new(tmp.path()).with_network_policy(
            codewhale_net::network_policy::NetworkPolicyDecider::new(
                codewhale_net::network_policy::NetworkPolicy {
                    default,
                    ..codewhale_net::network_policy::NetworkPolicy::default()
                },
                None,
            ),
        )
    };

    let allow = context(codewhale_net::network_policy::DecisionToml::Allow);
    enforce_readonly_network_reads("gh issue view 5287", &allow)
        .expect("allowed github.com policy");

    let deny = context(codewhale_net::network_policy::DecisionToml::Deny);
    let denied = enforce_readonly_network_reads("gh issue list", &deny)
        .expect_err("deny must stop before spawning gh")
        .to_string();
    assert!(denied.contains("blocked by the active network policy"));
    enforce_readonly_network_reads("git status", &deny)
        .expect("local reads do not consult the network policy");

    let prompt = context(codewhale_net::network_policy::DecisionToml::Prompt);
    let prompted = enforce_readonly_network_reads("gh issue view 5287", &prompt)
        .expect_err("headless Scout cannot prompt interactively")
        .to_string();
    assert!(prompted.contains("requires network approval"));

    // Read-only agents: every admitted network segment is judged.
    let readonly_deny = context(codewhale_net::network_policy::DecisionToml::Deny)
        .with_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly);
    for command in ["gh pr view 1 | head", "ls && gh issue list"] {
        let denied = enforce_readonly_network_reads(command, &readonly_deny)
            .expect_err("a network read inside a composition is still judged")
            .to_string();
        assert!(denied.contains("blocked"), "{command}: {denied}");
    }
    // npm is refused by the command authority before a configured registry
    // could be mistaken for the fixed public-registry network label.
    let npm = json!({"command": "npm view x"});
    let refusal = exec_shell_input_agent_readonly_verdict(&npm).expect_err("npm needs approval");
    assert!(refusal.detail.contains("configuration"));
    // An ordinary full shell retains its existing policy/approval path.
    enforce_readonly_network_reads("npm view x", &deny)
        .expect("npm is not granted or refused by full-shell read-only detection");
    // A full shell keeps its historical scope: only a lone gh read.
    enforce_readonly_network_reads("gh pr view 1 | head", &deny)
        .expect("full shell pipelines are governed elsewhere");
}

#[test]
fn exec_shell_interact_requires_approval() {
    let tool = BashTool::alias("exec_shell_interact", "interact");
    assert_eq!(tool.approval_requirement(), ApprovalRequirement::Required);
    assert!(
        tool.capabilities()
            .contains(&ToolCapability::RequiresApproval)
    );
}

#[tokio::test]
async fn read_only_shell_policy_blocks_non_readonly_commands() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path())
        .with_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly);
    let tool = BashTool::new("Bash");

    for (input, rule) in [
        (json!({"command": "cargo build"}), "program:"),
        (
            json!({"command": "git status -s", "background": true}),
            "shape:",
        ),
        (
            json!({"command": "git --config-env=core.fsmonitor=SHELL status"}),
            "option:",
        ),
        (
            json!({"command": "git -cdiff.foo.textconv=./repo-script diff HEAD"}),
            "option:",
        ),
        (json!({"command": "rg -f/etc/passwd needle ."}), "option:"),
        (json!({"command": "touch x && ls"}), "program:"),
    ] {
        // A typed denial, so a Fleet worker's no-progress guard counts it.
        let error = tool
            .execute(input.clone(), &ctx)
            .await
            .expect_err("classifier refusal");
        assert!(
            matches!(error, ToolError::PermissionDenied { .. }),
            "{input}: {error}"
        );
        let message = error.to_string();
        assert!(message.contains("[shell.readonly.command]"), "{message}");
        assert!(message.contains(rule), "{input}: {message}");
    }
    assert!(!tmp.path().join("x").exists());
}

#[tokio::test]
async fn read_only_refusal_names_child_alternatives_instead_of_mode_switch() {
    // #6298: a child has no `/mode` to switch to — a refusal that tells it to
    // switch modes is a dead end beside an available absurd path. The child
    // branch must name the child's own alternatives and the escalation path,
    // and only tools a read-only child actually has (#6015).
    let tmp = tempdir().expect("tempdir");
    let child_ctx = ToolContext::new(tmp.path())
        .with_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly)
        .with_owner_agent("agent_child", "child");
    let tool = BashTool::new("Bash");
    let message = tool
        .execute(json!({"command": "touch evil.txt"}), &child_ctx)
        .await
        .expect_err("refused")
        .to_string();
    let rejection = codewhale_execpolicy::command_safety::agent_readonly_verdict("touch evil.txt")
        .expect_err("touch is not a read");
    assert!(message.contains(&rejection.to_string()), "{message}");
    assert!(message.contains("program: `touch`"), "{message}");
    assert!(message.contains("File tool"), "{message}");
    assert!(
        message.contains("return your findings and the blocked probe to the parent"),
        "{message}"
    );
    for absent in ["/mode", "Git", "Run tests", "merge_tree"] {
        assert!(!message.contains(absent), "{absent} in {message}");
    }
    assert!(!tmp.path().join("evil.txt").exists());

    let parent_ctx = ToolContext::new(tmp.path())
        .with_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly);
    let message = tool
        .execute(json!({"command": "cargo build"}), &parent_ctx)
        .await
        .expect_err("refused")
        .to_string();
    assert!(
        message.contains("The user can change modes with /mode."),
        "{message}"
    );
}

#[test]
fn leading_cd_moves_into_cwd_for_every_gate() {
    let rewritten = normalize_readonly_cd(&json!({"command": "cd sub && ls"}));
    assert_eq!(rewritten, json!({"command": "ls", "cwd": "sub"}));
    let rewritten =
        normalize_readonly_cd(&json!({"command": "cd inner && git diff", "cwd": "sub"}));
    assert_eq!(rewritten["command"], "git diff");
    assert_eq!(
        std::path::Path::new(rewritten["cwd"].as_str().unwrap()),
        std::path::Path::new("sub").join("inner")
    );
    for command in ["cd a; ls", "ls && cd b && ls", "cd && ls", "cd $X && ls"] {
        let input = json!({"command": command});
        assert_eq!(normalize_readonly_cd(&input), input, "{command}");
        assert!(!agent_readonly_bash_input(&input), "{command}");
    }
    // The enforced lane runs its command unchanged.
    let enforced = json!({"command": "cd sub && ls", "read_only": true});
    assert_eq!(normalize_readonly_cd(&enforced), enforced);
    assert!(agent_readonly_bash_input(
        &json!({"command": "cd sub && ls"})
    ));
    assert!(agent_readonly_bash_input(
        &json!({"command": "cd sub && git diff && echo '=== FILES ===' && ls -la"})
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn read_only_shell_runs_chains_and_leading_cd() {
    let workspace = tempdir().expect("workspace");
    std::fs::create_dir(workspace.path().join("sub")).expect("sub");
    std::fs::write(workspace.path().join("sub").join("a.txt"), "alpha\n").expect("a");
    std::fs::write(workspace.path().join("b.txt"), "beta\n").expect("b");
    let ctx = ToolContext::new(workspace.path())
        .with_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly);
    let tool = BashTool::new("Bash");
    let result = tool
        .execute(json!({"command": "cd sub && ls"}), &ctx)
        .await
        .expect("cd is moved into cwd");
    assert!(result.success, "{}", result.content);
    assert!(result.content.contains("a.txt"), "{}", result.content);
    assert!(!result.content.contains("b.txt"), "{}", result.content);

    let result = tool
        .execute(
            json!({"command": "cat b.txt && echo --- && cat sub/a.txt 2>/dev/null"}),
            &ctx,
        )
        .await
        .expect("chain of reads");
    if !result.success && result.content.contains("require bash or zsh") {
        return;
    }
    assert!(result.success, "{}", result.content);
    assert!(result.content.contains("beta"), "{}", result.content);
    assert!(result.content.contains("---"), "{}", result.content);
    assert!(result.content.contains("alpha"), "{}", result.content);

    let error = tool
        .execute(json!({"command": "cd .. && ls"}), &ctx)
        .await
        .expect_err("a cd outside the workspace is refused")
        .to_string();
    assert!(
        error.contains("escapes workspace") || error.contains("outside_workspace"),
        "{error}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn read_only_operand_checks_apply_to_every_segment() {
    let workspace = tempdir().expect("workspace");
    let ctx = ToolContext::new(workspace.path())
        .with_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly);
    let error = BashTool::new("Bash")
        .execute(json!({"command": "gh pr view 1 && cat /etc/passwd"}), &ctx)
        .await
        .expect_err("the gh exemption covers only the gh segment");
    assert!(
        error.to_string().contains("shell.readonly.operand"),
        "{error}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn read_only_shell_resolves_operands_from_the_effective_cwd() {
    let workspace = tempdir().expect("workspace");
    let outside = tempdir().expect("outside");
    let subdir = workspace.path().join("subdir");
    std::fs::create_dir(&subdir).expect("subdir");
    std::fs::write(outside.path().join("secret"), "secret").expect("outside secret");
    std::os::unix::fs::symlink(outside.path().join("secret"), subdir.join("secret-link"))
        .expect("symlink");
    let ctx = ToolContext::new(workspace.path())
        .with_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly);
    let error = BashTool::new("Bash")
        .execute(
            json!({"action": "run", "command": "cat secret-link", "cwd": "subdir"}),
            &ctx,
        )
        .await
        .expect_err("cwd-relative symlink escape must fail before spawn")
        .to_string();
    assert!(error.contains("resolves outside"), "{error}");
}

#[cfg(unix)]
#[tokio::test]
async fn read_only_shell_skips_shell_env_hooks() {
    let tmp = tempdir().expect("tempdir");
    let marker = tmp.path().join("hook-ran");
    let hook = codewhale_config::hooks::Hook::new(
        codewhale_config::hooks::HookEvent::ShellEnv,
        &format!("printf hit > '{}'", marker.display()),
    );
    let executor = crate::hooks::HookExecutor::new(
        codewhale_config::hooks::HooksConfig {
            enabled: true,
            hooks: vec![hook],
            ..codewhale_config::hooks::HooksConfig::default()
        },
        tmp.path().to_path_buf(),
    );
    let mut context = ToolContext::new(tmp.path())
        .with_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly);
    context.runtime.hook_executor = Some(std::sync::Arc::new(executor));

    let result = BashTool::read_only("Bash")
        .execute(json!({"command": "pwd"}), &context)
        .await
        .expect("read-only inspection");
    assert!(result.success, "{}", result.content);
    assert!(!marker.exists(), "shell_env hook must not run for ReadOnly");
}

#[tokio::test]
async fn read_only_shell_policy_allows_readonly_inspection() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path())
        .with_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly);

    let result = BashTool::new("Bash")
        .execute(json!({"command": "pwd"}), &ctx)
        .await
        .expect("execute");

    assert!(
        result.success,
        "unexpected shell failure: {}",
        result.content
    );
    assert_eq!(
        result
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("status"))
            .and_then(Value::as_str),
        Some("Completed")
    );
}

#[tokio::test]
async fn exec_shell_multiline_block_explains_allow_shell_boundary() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());

    let result = BashTool::new("Bash")
        .execute(
            json!({"command": "python3 -c \"print(1)\nprint(2)\""}),
            &ctx,
        )
        .await
        .expect("execute");

    assert!(!result.success);
    assert!(result.content.contains("Command contains multiple lines"));
    assert!(
        result
            .content
            .contains("allow_shell=true exposes shell tools"),
        "{}",
        result.content
    );
    assert!(
        result
            .content
            .contains("Write multiline scripts to a file first"),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("background=true"),
        "{}",
        result.content
    );
}

#[test]
fn exec_shell_wait_schema_defaults_to_blocking() {
    let schema = BashTool::alias("exec_shell_wait", "wait").input_schema();
    assert!(
        schema["properties"]["wait"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("default: true"))
    );
    assert!(
        BashTool::alias("exec_shell_wait", "wait")
            .description()
            .contains("wait")
    );
}

#[tokio::test]
async fn exec_shell_wait_without_wait_arg_blocks_until_completion() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let start_result = BashTool::new("Bash")
        .execute(
            json!({"command": sleep_command(1), "background": true}),
            &ctx,
        )
        .await
        .expect("start background");
    let task_id = start_result
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("task_id"))
        .and_then(Value::as_str)
        .expect("task id")
        .to_string();

    let wait_result = BashTool::new("Bash")
        .execute(
            json!({"action": "wait", "task_id": task_id, "timeout_ms": 5_000}),
            &ctx,
        )
        .await
        .expect("wait for completion");

    assert_eq!(
        wait_result
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("status"))
            .and_then(Value::as_str),
        Some("Completed")
    );
}

#[tokio::test]
async fn exec_shell_wait_false_returns_nonblocking_snapshot() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let start_result = BashTool::new("Bash")
        .execute(
            json!({"command": sleep_command(2), "background": true}),
            &ctx,
        )
        .await
        .expect("start background");
    let task_id = start_result
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("task_id"))
        .and_then(Value::as_str)
        .expect("task id")
        .to_string();

    let started = Instant::now();
    let wait_result = BashTool::new("Bash")
        .execute(
            json!({"action": "wait", "task_id": task_id, "timeout_ms": 5_000, "wait": false}),
            &ctx,
        )
        .await
        .expect("poll snapshot");

    assert!(
        started.elapsed() < Duration::from_millis(1_000),
        "wait=false should return a snapshot without blocking"
    );
    assert_eq!(
        wait_result
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("status"))
            .and_then(Value::as_str),
        Some("Running")
    );
}

#[tokio::test]
async fn exec_shell_wait_without_wait_arg_returns_running_at_timeout() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let start_result = BashTool::new("Bash")
        .execute(
            json!({"command": sleep_command(5), "background": true}),
            &ctx,
        )
        .await
        .expect("start background");
    let task_id = start_result
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("task_id"))
        .and_then(Value::as_str)
        .expect("task id")
        .to_string();

    let started = Instant::now();
    let result = BashTool::new("Bash")
        .execute(
            json!({"action": "wait", "task_id": task_id, "timeout_ms": 1_000}),
            &ctx,
        )
        .await
        .expect("bounded wait");
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(
        result
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("status"))
            .and_then(Value::as_str),
        Some("Running")
    );

    BashTool::new("Bash")
        .execute(json!({"action": "cancel", "task_id": task_id}), &ctx)
        .await
        .expect("cancel background");
}

#[tokio::test]
async fn exec_shell_wait_many_until_any_returns_when_the_first_task_settles() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let short = BashTool::new("Bash")
        .execute(
            json!({"command": sleep_command(1), "background": true}),
            &ctx,
        )
        .await
        .expect("start short");
    let long = BashTool::new("Bash")
        .execute(
            json!({"command": sleep_command(30), "background": true}),
            &ctx,
        )
        .await
        .expect("start long");
    let short_id = short
        .metadata
        .as_ref()
        .and_then(|m| m.get("task_id"))
        .and_then(Value::as_str)
        .expect("short id")
        .to_string();
    let long_id = long
        .metadata
        .as_ref()
        .and_then(|m| m.get("task_id"))
        .and_then(Value::as_str)
        .expect("long id")
        .to_string();

    let started = Instant::now();
    let result = BashTool::new("Bash")
        .execute(
            json!({
                "action": "wait",
                "task_ids": [short_id, long_id],
                "until": "any",
                "timeout_ms": 8_000
            }),
            &ctx,
        )
        .await
        .expect("multi wait");

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "until=any must not wait for the long task"
    );
    let statuses = result
        .metadata
        .as_ref()
        .expect("metadata")
        .get("statuses")
        .expect("statuses");
    assert_eq!(statuses[short_id.as_str()], "Completed");
    assert_eq!(statuses[long_id.as_str()], "Running");
    assert_eq!(
        result.metadata.as_ref().and_then(|m| m.get("until")),
        Some(&json!("any"))
    );
    assert_eq!(
        result.metadata.as_ref().and_then(|m| m.get("timed_out")),
        Some(&json!(false))
    );
}

#[tokio::test]
async fn exec_shell_wait_many_all_reports_timeout_and_still_running() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let first = BashTool::new("Bash")
        .execute(
            json!({"command": sleep_command(30), "background": true}),
            &ctx,
        )
        .await
        .expect("start first");
    let second = BashTool::new("Bash")
        .execute(
            json!({"command": sleep_command(30), "background": true}),
            &ctx,
        )
        .await
        .expect("start second");
    let first_id = first
        .metadata
        .as_ref()
        .and_then(|m| m.get("task_id"))
        .and_then(Value::as_str)
        .expect("first id")
        .to_string();
    let second_id = second
        .metadata
        .as_ref()
        .and_then(|m| m.get("task_id"))
        .and_then(Value::as_str)
        .expect("second id")
        .to_string();

    let result = BashTool::new("Bash")
        .execute(
            json!({ "action": "wait", "task_ids": [first_id, second_id], "timeout_ms": 1_500 }),
            &ctx,
        )
        .await
        .expect("multi wait times out");
    assert_eq!(
        result.metadata.as_ref().and_then(|m| m.get("timed_out")),
        Some(&json!(true))
    );
    assert!(result.content.contains("still running"));
    let statuses = result
        .metadata
        .as_ref()
        .expect("metadata")
        .get("statuses")
        .expect("statuses");
    assert_eq!(statuses[first_id.as_str()], "Running");
    assert_eq!(statuses[second_id.as_str()], "Running");
}

#[tokio::test]
async fn exec_shell_wait_many_rejects_an_unknown_task_id() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let err = BashTool::new("Bash")
        .execute(
            json!({ "action": "wait", "task_ids": ["nope-1", "nope-2"], "wait": false }),
            &ctx,
        )
        .await
        .expect_err("unknown ids must fail the same way as the single-task path");
    assert!(err.to_string().contains("nope-1"), "{err}");
}

#[tokio::test]
async fn background_start_advertises_task_status_completion() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let result = BashTool::new("Bash")
        .execute(
            json!({"command": sleep_command(1), "background": true}),
            &ctx,
        )
        .await
        .expect("start background");
    assert!(result.content.contains("completion is delivered"));
    assert!(result.content.contains("session exits") && result.content.contains("persist=true"));
    let metadata = result.metadata.as_ref().expect("metadata");
    assert_eq!(
        metadata
            .get("auto_resume_on_completion")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        metadata.get("completion_surface").and_then(Value::as_str),
        Some("runtime_event_and_task_status")
    );
    assert_eq!(
        metadata.get("background_policy").and_then(Value::as_str),
        Some("nonblocking")
    );
}

#[tokio::test]
async fn background_shell_job_preserves_origin_identity() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path())
        .with_origin_turn_id("turn-origin")
        .with_origin_tool_call_id("tool-origin")
        .with_owner_agent("agent_owner", "verifier");
    let result = BashTool::new("Bash")
        .execute(
            json!({"command": sleep_command(2), "background": true}),
            &ctx,
        )
        .await
        .expect("start owned background shell");

    let metadata = result.metadata.as_ref().expect("metadata");
    assert_eq!(
        metadata.get("owner_agent_id").and_then(Value::as_str),
        Some("agent_owner")
    );
    assert_eq!(
        metadata.get("owner_agent_name").and_then(Value::as_str),
        Some("verifier")
    );
    assert!(
        result
            .content
            .contains("not injected into the parent model"),
        "owned background work must describe its real completion route: {}",
        result.content
    );
    assert!(
        result.content.contains("`task_shell_wait` directly"),
        "owned background work must name the eager wait tool: {}",
        result.content
    );
    assert_eq!(
        metadata
            .get("auto_resume_on_completion")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        metadata.get("completion_surface").and_then(Value::as_str),
        Some("task_status_and_explicit_wait")
    );
    let task_id = metadata
        .get("task_id")
        .and_then(Value::as_str)
        .expect("task id")
        .to_string();

    {
        let mut manager = ctx.shell_manager.lock().expect("shell manager");
        let snapshot = manager
            .list_jobs()
            .into_iter()
            .find(|job| job.id == task_id)
            .expect("owned shell job snapshot");
        assert_eq!(snapshot.owner_agent_id.as_deref(), Some("agent_owner"));
        assert_eq!(snapshot.owner_agent_name.as_deref(), Some("verifier"));
        assert_eq!(snapshot.origin_tool_call_id.as_deref(), Some("tool-origin"));
        assert_eq!(snapshot.origin_turn_id.as_deref(), Some("turn-origin"));
        let mut legacy_json = serde_json::to_value(&snapshot).expect("serialize snapshot");
        let legacy_object = legacy_json.as_object_mut().expect("snapshot object");
        legacy_object.remove("origin_tool_call_id");
        legacy_object.remove("origin_turn_id");
        let legacy_snapshot: ShellJobSnapshot =
            serde_json::from_value(legacy_json).expect("deserialize legacy snapshot");
        assert_eq!(legacy_snapshot.origin_tool_call_id, None);
        assert_eq!(legacy_snapshot.origin_turn_id, None);
        let owners = manager.running_owner_agent_ids();
        assert_eq!(owners, vec!["agent_owner".to_string()]);
    }

    BashTool::alias("exec_shell_cancel", "cancel")
        .execute(json!({"task_id": task_id}), &ctx)
        .await
        .expect("cancel owned background shell");
}

#[tokio::test]
async fn drain_finished_jobs_reports_once() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path())
        .with_origin_turn_id("turn-origin")
        .with_origin_tool_call_id("tool-origin");
    let result = BashTool::new("Bash")
        .execute(
            json!({"command": echo_command("drain-finished-once"), "background": true}),
            &ctx,
        )
        .await
        .expect("start background");
    let task_id = result
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("task_id"))
        .and_then(Value::as_str)
        .expect("task id")
        .to_string();

    let mut manager = ctx.shell_manager.lock().expect("shell manager");
    assert!(manager.may_have_undelivered_completion());
    assert!(
        manager.may_have_undelivered_completion(),
        "read-only detection must not consume the pending completion"
    );
    let completed = wait_for_completed_shell(&mut manager, &task_id);
    assert_ne!(completed.status, ShellStatus::Running);
    assert!(manager.may_have_undelivered_completion());

    let first = manager
        .drain_finished_jobs_with_evidence()
        .into_iter()
        .map(|completion| completion.event)
        .collect::<Vec<_>>();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].task_id, task_id);
    assert_eq!(first[0].status, ShellStatus::Completed);
    assert!(first[0].stdout_tail.contains("drain-finished-once"));
    assert_eq!(first[0].origin_tool_call_id.as_deref(), Some("tool-origin"));
    assert_eq!(first[0].origin_turn_id.as_deref(), Some("turn-origin"));
    let mut legacy_json = serde_json::to_value(&first[0]).expect("serialize completion");
    let legacy_object = legacy_json.as_object_mut().expect("completion object");
    legacy_object.remove("origin_tool_call_id");
    legacy_object.remove("origin_turn_id");
    let legacy_completion: ShellCompletionEvent =
        serde_json::from_value(legacy_json).expect("deserialize legacy completion");
    assert_eq!(legacy_completion.origin_tool_call_id, None);
    assert_eq!(legacy_completion.origin_turn_id, None);

    let second = manager.drain_finished_jobs_with_evidence();
    assert!(second.is_empty(), "completion should be reported only once");
    assert!(!manager.may_have_undelivered_completion());
}

#[tokio::test]
async fn background_job_is_hidden_from_replacement_session_and_resumes_once_for_owner() {
    let tmp = tempdir().expect("tempdir");
    let ctx_a = ToolContext::new(tmp.path()).with_state_namespace("session-a");
    let ctx_b = ctx_a.clone().with_state_namespace("session-b");
    let result = BashTool::new("Bash")
        .execute(
            json!({"command": echo_command("owned-by-a"), "background": true}),
            &ctx_a,
        )
        .await
        .expect("start A background job");
    let task_id = result
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("task_id"))
        .and_then(Value::as_str)
        .expect("task id")
        .to_string();

    let mut manager = ctx_b.shell_manager.lock().expect("shell manager");
    assert!(manager.list_jobs_for_session("session-b").is_empty());
    assert!(
        manager
            .inspect_job_for_session("session-b", &task_id)
            .is_err()
    );
    assert!(
        manager
            .write_stdin_for_session("session-b", &task_id, "foreign", false)
            .is_err()
    );
    assert!(manager.kill_for_session("session-b", &task_id).is_err());

    let completed = wait_for_completed_shell(&mut manager, &task_id);
    assert_ne!(completed.status, ShellStatus::Running);
    let owned = manager
        .list_jobs_for_session("session-a")
        .into_iter()
        .find(|job| job.id == task_id)
        .expect("A job remains visible to A");
    assert_eq!(owned.owner_session_id, "session-a");
    assert!(
        manager
            .drain_finished_jobs_with_evidence_for_session("session-b")
            .is_empty(),
        "B must not claim A's completion"
    );
    assert!(manager.has_finished_unreported_jobs_for_session("session-a"));
    let first = manager.drain_finished_jobs_with_evidence_for_session("session-a");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].event.owner_session_id, "session-a");
    assert!(first[0].event.stdout_tail.contains("owned-by-a"));
    assert!(
        manager
            .drain_finished_jobs_with_evidence_for_session("session-a")
            .is_empty(),
        "A completion is delivered exactly once"
    );
}

#[test]
fn completion_evidence_preserves_arbitrary_stream_bytes() {
    use base64::Engine as _;

    let stdout = vec![b'o', 0, 0xff, b'k'];
    let stderr = vec![0xfe, b'e', b'r', b'r'];
    let evidence = ShellCompletionEvidence {
        event: ShellCompletionEvent {
            task_id: "shell_binary".to_string(),
            command: "binary-output".to_string(),
            status: ShellStatus::Completed,
            exit_code: Some(0),
            duration_ms: 17,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            stdout_len: stdout.len(),
            stderr_len: stderr.len(),
            evidence_ref: None,
            linked_task_id: None,
            owner_agent_id: None,
            owner_agent_name: None,
            origin_tool_call_id: Some("tool-origin".to_string()),
            origin_turn_id: Some("turn-origin".to_string()),
            owner_session_id: "session-test".to_string(),
        },
        stdout: stdout.clone(),
        stderr: stderr.clone(),
        stdout_omitted: 0,
        stderr_omitted: 0,
    };

    let payload: serde_json::Value =
        serde_json::from_slice(&evidence.artifact_bytes()).expect("evidence JSON");
    assert_eq!(payload["stdout"]["encoding"], "base64");
    assert_eq!(payload["stderr"]["encoding"], "base64");
    assert_eq!(payload["origin_tool_call_id"], "tool-origin");
    assert_eq!(payload["origin_turn_id"], "turn-origin");
    let decoded_stdout = base64::engine::general_purpose::STANDARD
        .decode(payload["stdout"]["content"].as_str().expect("stdout data"))
        .expect("decode stdout");
    let decoded_stderr = base64::engine::general_purpose::STANDARD
        .decode(payload["stderr"]["content"].as_str().expect("stderr data"))
        .expect("decode stderr");
    assert_eq!(decoded_stdout, stdout);
    assert_eq!(decoded_stderr, stderr);
}

#[test]
#[cfg(unix)]
fn shell_execution_scrubs_parent_env_and_keeps_explicit_env() {
    let _lock = crate::test_support::lock_test_env();
    let _secret =
        crate::test_support::EnvVarGuard::set("DEEPSEEK_CHILD_ENV_SHELL_SECRET", "parent-secret");

    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());
    let mut extra = std::collections::HashMap::new();
    extra.insert(
        "DEEPSEEK_CHILD_ENV_EXPLICIT".to_string(),
        "explicit-value".to_string(),
    );

    let result = manager
        .execute_with_options_env(
            "sh -c 'printf \"%s\\n%s\\n\" \"${DEEPSEEK_CHILD_ENV_SHELL_SECRET-unset}\" \"${DEEPSEEK_CHILD_ENV_EXPLICIT-unset}\"'",
            None,
            5000,
            false,
            None,
            false,
            None,
            extra,
        )
        .expect("execute");

    assert_eq!(result.status, ShellStatus::Completed);
    assert_eq!(result.stdout, "unset\nexplicit-value\n");
}

#[test]
#[cfg(windows)]
fn shell_execution_preserves_custom_windows_sdk_root_env() {
    let _lock = crate::test_support::lock_test_env();
    let _sdk = crate::test_support::EnvVarGuard::set("BIMRV_SDK_ROOT", r"F:\Lib\BimRv27.5");
    let _secret = crate::test_support::EnvVarGuard::set("MY_SECRET_ROOT", r"F:\Secrets");

    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());
    let command = if codewhale_sandbox::shell_dispatcher::global_dispatcher()
        .kind()
        .is_powershell()
    {
        r#"[Console]::WriteLine($env:BIMRV_SDK_ROOT); if ($null -eq $env:MY_SECRET_ROOT) { [Console]::WriteLine("secret-unset") } else { [Console]::WriteLine("secret-set") }"#
            .to_string()
    } else {
        r#"echo %BIMRV_SDK_ROOT% & if defined MY_SECRET_ROOT (echo secret-set) else (echo secret-unset)"#
            .to_string()
    };

    let result = execute_shell(&mut manager, &command, None, 5000, false).expect("execute");

    assert_eq!(result.status, ShellStatus::Completed);
    assert!(
        result.stdout.contains(r"F:\Lib\BimRv27.5"),
        "custom SDK root should reach exec_shell stdout: {:?}",
        result
    );
    assert!(
        result.stdout.contains("secret-unset"),
        "secret-like env should stay scrubbed: {:?}",
        result
    );
}

#[test]
fn test_sync_execution() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let result =
        execute_shell(&mut manager, &echo_command("hello"), None, 5000, false).expect("execute");

    assert_eq!(result.status, ShellStatus::Completed);
    assert!(result.stdout.contains("hello"));
    assert!(result.task_id.is_none());
}

#[test]
fn test_background_execution() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let result = execute_shell(
        &mut manager,
        &sleep_then_echo_command(1, "done"),
        None,
        5000,
        true,
    )
    .expect("execute");

    assert_eq!(result.status, ShellStatus::Running);
    assert!(result.task_id.is_some());

    let task_id = result
        .task_id
        .expect("background execution should return task_id");

    let final_result = wait_for_completed_shell(&mut manager, &task_id);

    assert_eq!(final_result.status, ShellStatus::Completed);
    assert!(final_result.stdout.contains("done"));
}

#[test]
fn test_timeout() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let result =
        execute_shell(&mut manager, &sleep_command(10), None, 1000, false).expect("execute");

    assert_eq!(result.status, ShellStatus::TimedOut);
}

#[test]
fn test_kill() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let result =
        execute_shell(&mut manager, &sleep_command(60), None, 5000, true).expect("execute");

    let task_id = result
        .task_id
        .expect("background execution should return task_id");

    // Kill it
    let killed = manager.kill(&task_id).expect("kill");
    assert_eq!(killed.status, ShellStatus::Killed);
}

#[test]
fn test_write_stdin_streams_output() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let result = manager
        .execute_with_options_env(
            &echo_stdin_command(),
            None,
            5000,
            true,
            None,
            false,
            None,
            HashMap::new(),
        )
        .expect("execute");

    let task_id = result
        .task_id
        .expect("background execution should return task_id");

    manager
        .write_stdin(&task_id, "hello\n", true)
        .expect("write stdin");

    let delta = manager
        .get_output_delta(&task_id, true, 5000)
        .expect("get_output_delta");

    assert!(delta.result.stdout.contains("hello"));

    let delta2 = manager
        .get_output_delta(&task_id, false, 0)
        .expect("get_output_delta");
    assert!(delta2.result.stdout.is_empty());
}

#[test]
#[cfg(all(unix, not(target_env = "ohos")))]
fn background_tty_command_has_controlling_terminal() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let result = manager
        .execute_with_options_env(
            "sh -c 'exec 3<>/dev/tty && printf tty-ok && exec 3>&-'",
            None,
            5000,
            true,
            None,
            true,
            Some(ExecutionSandboxPolicy::DangerFullAccess),
            HashMap::new(),
        )
        .expect("execute tty command");

    let task_id = result
        .task_id
        .expect("background tty execution should return task_id");

    let done = manager
        .get_output(&task_id, true, 10_000)
        .expect("get tty command output");

    assert_eq!(done.status, ShellStatus::Completed);
    assert_eq!(done.exit_code, Some(0));
    assert!(
        done.stdout.contains("tty-ok"),
        "tty output should confirm /dev/tty opened; got {done:?}"
    );
}

#[test]
fn test_job_list_poll_cancel_and_stale_snapshot() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let started = execute_shell(
        &mut manager,
        &sleep_then_echo_command(1, "done"),
        None,
        5000,
        true,
    )
    .expect("execute");
    let task_id = started.task_id.expect("task id");
    manager
        .tag_linked_task(&task_id, Some("task_123".to_string()))
        .expect("tag linked task");

    let running = manager.list_jobs();
    let job = running
        .iter()
        .find(|job| job.id == task_id)
        .expect("running job");
    assert_eq!(job.status, ShellStatus::Running);
    assert_eq!(job.linked_task_id.as_deref(), Some("task_123"));
    assert!(job.command.contains("done"));
    assert_eq!(job.cwd, tmp.path());

    let completed = manager
        .poll_delta(&task_id, true, 5000)
        .expect("poll delta");
    assert_eq!(completed.result.status, ShellStatus::Completed);
    assert!(completed.result.stdout.contains("done"));

    let detail = manager.inspect_job(&task_id).expect("inspect");
    assert!(detail.stdout.contains("done"));
    assert_eq!(detail.snapshot.status, ShellStatus::Completed);

    manager.remember_stale_job(
        "shell_stale",
        "cargo test",
        tmp.path().to_path_buf(),
        Some("task_old".to_string()),
    );
    let stale = manager
        .list_jobs()
        .into_iter()
        .find(|job| job.id == "shell_stale")
        .expect("stale job");
    assert!(stale.stale);
    assert_eq!(stale.linked_task_id.as_deref(), Some("task_old"));
}

#[test]
fn running_job_snapshot_keeps_silent_live_job_live() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let started =
        execute_shell(&mut manager, &sleep_command(5), None, 5000, true).expect("execute");
    let task_id = started.task_id.expect("task id");

    {
        let shell = manager.processes.get_mut(&task_id).expect("live shell");
        shell.last_output_at = Instant::now() - QUIET_NO_OUTPUT_AFTER - Duration::from_millis(1);
    }

    let job = manager
        .list_jobs()
        .into_iter()
        .find(|job| job.id == task_id)
        .expect("running job");

    assert_eq!(job.status, ShellStatus::Running);
    assert!(!job.stale, "silence alone must not mark a live job stale");
    assert!(
        job.elapsed_since_output_ms
            .is_some_and(|elapsed| elapsed >= QUIET_NO_OUTPUT_AFTER.as_millis() as u64),
        "elapsed no-output time should be exposed: {job:?}"
    );
}

#[test]
fn running_job_snapshot_keeps_recent_no_output_fresh() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let started =
        execute_shell(&mut manager, &sleep_command(5), None, 5000, true).expect("execute");
    let task_id = started.task_id.expect("task id");

    let job = manager
        .list_jobs()
        .into_iter()
        .find(|job| job.id == task_id)
        .expect("running job");

    assert_eq!(job.status, ShellStatus::Running);
    assert!(!job.stale, "fresh running job should not start stale");
    assert!(job.elapsed_since_output_ms.is_some());
}

#[test]
fn test_job_cancel_updates_completion_state() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let started =
        execute_shell(&mut manager, &sleep_command(60), None, 5000, true).expect("execute");
    let task_id = started.task_id.expect("task id");

    let killed = manager.kill(&task_id).expect("kill");
    assert_eq!(killed.status, ShellStatus::Killed);
    let job = manager.inspect_job(&task_id).expect("inspect");
    assert_eq!(job.snapshot.status, ShellStatus::Killed);
    assert!(!job.snapshot.stdin_available);
}

#[test]
fn test_output_truncation() {
    let long_output = "x".repeat(50_000);
    let (truncated, _meta) = truncate_with_meta(&long_output);

    assert!(truncated.len() < long_output.len());
    assert!(truncated.contains("truncated"));
}

#[test]
fn test_truncate_with_meta_reports_omission_counts() {
    let long_output = format!("line1\nline2\n{}", "x".repeat(60_000));
    let (truncated, meta) = truncate_with_meta(&long_output);

    assert!(meta.truncated);
    assert!(meta.original_len >= long_output.len());
    assert!(meta.omitted > 0);
    assert!(truncated.contains("bytes omitted"));
}

#[test]
fn network_restricted_hint_detects_silent_curl_failure() {
    let tmp = tempdir().expect("tempdir");
    let ctx = network_restricted_context(tmp.path());
    let result = failed_network_shell_result("000", "");

    let hint = shell_network_restricted_hint(
        &ctx,
        "curl -s -o /dev/null -w '%{http_code}' https://api.github.com",
        &result,
    )
    .expect("network-restricted hint");

    assert!(hint.contains("Plan mode"));
}

#[test]
fn sandbox_denied_hint_names_the_effective_posture() {
    // DGF-02: an approved write blocked by a read-only sandbox must come
    // back naming the sandbox as the blocker, never as a bare failure.
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path())
        .with_elevated_sandbox_policy(ExecutionSandboxPolicy::read_only());
    let mut result =
        failed_network_shell_result("", "sh: cannot create out.txt: Operation not permitted");
    result.sandbox_denied = true;

    let hint = shell_sandbox_denied_hint(&ctx, &result).expect("sandbox-denied hint");

    assert!(hint.contains("read-only"), "{hint}");
    assert!(hint.contains("Ask-only escalation"), "{hint}");
    assert!(
        hint.contains("retry this exact command once with sandbox_permissions"),
        "{hint}"
    );
    assert!(hint.contains("justification"), "{hint}");
}

#[test]
fn contract_bash_denial_surfaces_the_escalation_shape() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path())
        .with_elevated_sandbox_policy(ExecutionSandboxPolicy::read_only());
    let mut result = failed_network_shell_result("", "Operation not permitted");
    result.sandbox_denied = true;

    let error = finish_contract_bash_result(result, None, &ctx, None, false, None)
        .expect_err("sandbox denial is a failed call");

    assert!(
        error
            .to_string()
            .contains("retry this exact command once with sandbox_permissions"),
        "{error}"
    );
}

#[test]
fn sandbox_denied_hint_absent_without_denial_or_policy() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path())
        .with_elevated_sandbox_policy(ExecutionSandboxPolicy::read_only());
    let undenied = failed_network_shell_result("", "No such file or directory");
    assert!(shell_sandbox_denied_hint(&ctx, &undenied).is_none());

    let mut denied = failed_network_shell_result("", "");
    denied.sandbox_denied = true;
    let no_policy_ctx = ToolContext::new(tmp.path());
    assert!(shell_sandbox_denied_hint(&no_policy_ctx, &denied).is_none());
}

#[test]
fn shell_delta_result_surfaces_sandbox_denied_hint() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path())
        .with_elevated_sandbox_policy(ExecutionSandboxPolicy::read_only());
    let mut result = failed_network_shell_result("", "Operation not permitted");
    result.sandbox_denied = true;

    let tool_result = build_shell_delta_tool_result(
        ShellDeltaResult {
            command: "touch out.txt".to_string(),
            result,
            stdout_total_len: 0,
            stderr_total_len: 0,
        },
        &ctx,
    );

    assert!(
        tool_result
            .content
            .contains("The execution sandbox blocked this command"),
        "{}",
        tool_result.content
    );
    let metadata = tool_result.metadata.expect("metadata");
    assert!(metadata.get("sandbox_denied_hint").is_some());
}

#[test]
fn network_restricted_hint_ignores_local_failures() {
    let tmp = tempdir().expect("tempdir");
    let ctx = network_restricted_context(tmp.path());
    let result = failed_network_shell_result("", "No such file or directory");

    assert!(shell_network_restricted_hint(&ctx, "cat missing.txt", &result).is_none());
}

#[test]
fn shell_delta_result_surfaces_network_restricted_hint() {
    let tmp = tempdir().expect("tempdir");
    let ctx = network_restricted_context(tmp.path());
    let result = failed_network_shell_result("000", "");

    let tool_result = build_shell_delta_tool_result(
        ShellDeltaResult {
            command: "gh issue list".to_string(),
            result,
            stdout_total_len: 3,
            stderr_total_len: 0,
        },
        &ctx,
    );

    assert!(!tool_result.success);
    assert!(tool_result.content.starts_with("Shell command blocked"));
    let metadata = tool_result.metadata.expect("metadata");
    assert_eq!(
        metadata
            .get("sandbox_network_restricted")
            .and_then(Value::as_bool),
        Some(true)
    );
}

#[test]
fn shell_delta_result_exposes_lossless_high_exit_code_and_hex() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let mut result = failed_network_shell_result("", "");
    result.exit_code = Some(0xC000_0005);

    let tool_result = build_shell_delta_tool_result(
        ShellDeltaResult {
            command: "echo probe".to_string(),
            result,
            stdout_total_len: 0,
            stderr_total_len: 0,
        },
        &ctx,
    );

    assert!(
        tool_result
            .content
            .contains("exit code 3221225477 (0xC0000005)"),
        "{}",
        tool_result.content
    );
    let metadata = tool_result.metadata.expect("metadata");
    assert_eq!(metadata["exit_code"], json!(3221225477_i64));
    assert_eq!(metadata["exit_code_hex"], json!("0xC0000005"));
}

#[test]
fn shell_delta_result_surfaces_elapsed_time_in_content() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let mut result = failed_network_shell_result("", "");
    result.status = ShellStatus::Running;
    result.duration_ms = 42_500;
    result.task_id = Some("shell-7".to_string());

    let tool_result = build_shell_delta_tool_result(
        ShellDeltaResult {
            command: "cargo test --workspace".to_string(),
            result,
            stdout_total_len: 0,
            stderr_total_len: 0,
        },
        &ctx,
    );

    assert!(
        tool_result
            .content
            .starts_with("Task shell-7 still running after 42.5 s."),
        "{}",
        tool_result.content
    );
}

#[test]
fn shell_delta_timing_line_omits_task_id_when_unknown() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    // failed_network_shell_result: ShellStatus::Failed, duration_ms: 25, task_id: None.
    let result = failed_network_shell_result("", "");

    let tool_result = build_shell_delta_tool_result(
        ShellDeltaResult {
            command: "echo probe".to_string(),
            result,
            stdout_total_len: 0,
            stderr_total_len: 0,
        },
        &ctx,
    );

    assert!(
        tool_result.content.starts_with("Task failed after 25 ms."),
        "{}",
        tool_result.content
    );
}

#[test]
fn shell_delta_timing_line_phrases_cover_terminal_statuses() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    for (status, phrase) in [
        (ShellStatus::Completed, "completed"),
        (ShellStatus::Killed, "killed"),
        (ShellStatus::TimedOut, "timed out"),
    ] {
        let mut result = failed_network_shell_result("", "");
        result.status = status;
        result.duration_ms = 5_000;
        let tool_result = build_shell_delta_tool_result(
            ShellDeltaResult {
                command: "echo probe".to_string(),
                result,
                stdout_total_len: 0,
                stderr_total_len: 0,
            },
            &ctx,
        );
        assert!(
            tool_result
                .content
                .starts_with(&format!("Task {phrase} after 5 s.")),
            "{}",
            tool_result.content
        );
    }
}

#[test]
fn shell_delta_timing_line_handles_zero_duration() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let mut result = failed_network_shell_result("", "");
    result.status = ShellStatus::Completed;
    result.duration_ms = 0;
    let tool_result = build_shell_delta_tool_result(
        ShellDeltaResult {
            command: "echo probe".to_string(),
            result,
            stdout_total_len: 0,
            stderr_total_len: 0,
        },
        &ctx,
    );
    assert!(
        tool_result
            .content
            .starts_with("Task completed after 0 ms."),
        "{}",
        tool_result.content
    );
}

#[test]
fn shell_delta_timing_line_sits_below_network_hint() {
    let tmp = tempdir().expect("tempdir");
    let ctx = network_restricted_context(tmp.path());
    let result = failed_network_shell_result("000", "");
    let tool_result = build_shell_delta_tool_result(
        ShellDeltaResult {
            command: "gh issue list".to_string(),
            result,
            stdout_total_len: 3,
            stderr_total_len: 0,
        },
        &ctx,
    );
    let content = tool_result.content;
    let hint_pos = content.find("Shell command blocked").expect("hint present");
    let timing_pos = content
        .find("failed after 25 ms")
        .expect("timing line present");
    assert!(
        hint_pos < timing_pos,
        "hint must precede timing line: {content}"
    );
}

#[test]
fn shell_delta_result_includes_cargo_failure_summary() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let result = ShellResult {
        task_id: None,
        status: ShellStatus::Failed,
        exit_code: Some(101),
        stdout: "running 1 test\ntest tests::fails ... FAILED\n\nfailures:\n\n---- tests::fails stdout ----\nthread 'tests::fails' panicked at src/lib.rs:7:9:\nboom\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; finished in 0.00s\n".to_string(),
        stderr: "error: test failed, to rerun pass `--lib`".to_string(),
        duration_ms: 12,
        stdout_len: 0,
        stderr_len: 0,
        stdout_omitted: 0,
        stderr_omitted: 0,
        stdout_truncated: false,
        stderr_truncated: false,
        sandboxed: false,
        sandbox_type: None,
        sandbox_denied: false,
    };

    let tool_result = build_shell_delta_tool_result(
        ShellDeltaResult {
            command: "cargo test".to_string(),
            result,
            stdout_total_len: 0,
            stderr_total_len: 0,
        },
        &ctx,
    );

    let metadata = tool_result.metadata.expect("metadata");
    assert_eq!(
        metadata["cargo_failure_summary"]["kind"],
        json!("test_failure")
    );
    assert!(
        metadata["cargo_failure_summary"]["summary"]
            .as_str()
            .unwrap()
            .contains("Failing tests: tests::fails")
    );
    assert!(
        metadata["summary"]
            .as_str()
            .unwrap()
            .contains("error: test failed")
    );
}

#[test]
fn shell_delta_result_keeps_existing_summary_for_generic_cargo_failure() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let result = ShellResult {
        task_id: None,
        status: ShellStatus::Failed,
        exit_code: Some(1),
        stdout: "build failed".to_string(),
        stderr: "command failed without structured cargo diagnostics".to_string(),
        duration_ms: 12,
        stdout_len: 0,
        stderr_len: 0,
        stdout_omitted: 0,
        stderr_omitted: 0,
        stdout_truncated: false,
        stderr_truncated: false,
        sandboxed: false,
        sandbox_type: None,
        sandbox_denied: false,
    };

    let tool_result = build_shell_delta_tool_result(
        ShellDeltaResult {
            command: "cargo test".to_string(),
            result,
            stdout_total_len: 0,
            stderr_total_len: 0,
        },
        &ctx,
    );

    let metadata = tool_result.metadata.expect("metadata");
    assert!(metadata.get("cargo_failure_summary").is_none());
    assert_eq!(
        metadata["summary"],
        json!("command failed without structured cargo diagnostics")
    );
}

#[test]
fn shell_delta_result_surfaces_python_build_dependency_hint() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let result = ShellResult {
        task_id: None,
        status: ShellStatus::Failed,
        exit_code: Some(1),
        stdout: String::new(),
        stderr: "running build_ext\nModuleNotFoundError: No module named 'setuptools'\n"
            .to_string(),
        duration_ms: 12,
        stdout_len: 0,
        stderr_len: 72,
        stdout_omitted: 0,
        stderr_omitted: 0,
        stdout_truncated: false,
        stderr_truncated: false,
        sandboxed: false,
        sandbox_type: None,
        sandbox_denied: false,
    };

    let tool_result = build_shell_delta_tool_result(
        ShellDeltaResult {
            command: "python setup.py build_ext --inplace".to_string(),
            result,
            stdout_total_len: 0,
            stderr_total_len: 72,
        },
        &ctx,
    );

    assert!(!tool_result.success);
    assert!(
        tool_result
            .content
            .starts_with("Python build dependency missing")
    );
    let metadata = tool_result.metadata.expect("metadata");
    assert_eq!(
        metadata["python_build_dependency_hint"]["kind"],
        json!("missing_setuptools")
    );
    assert!(
        metadata["python_build_dependency_hint"]["hint"]
            .as_str()
            .unwrap()
            .contains("setuptools")
    );
}

#[test]
fn test_summarize_output_strips_truncation_note() {
    let long_output = "x".repeat(60_000);
    let (truncated, _meta) = truncate_with_meta(&long_output);
    let summary = summarize_output(&truncated);
    assert!(!summary.contains("Output truncated at"));
}

#[tokio::test]
async fn test_exec_shell_metadata_includes_summaries() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let tool = BashTool::new("Bash");

    let result = tool
        .execute(json!({"command": echo_command("hello")}), &ctx)
        .await
        .expect("execute");
    assert!(result.success);

    let meta = result.metadata.expect("metadata");
    let summary = meta
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(summary.contains("hello"));
    assert!(meta.get("stdout_len").is_some());
    assert!(meta.get("stdout_truncated").is_some());
}

#[cfg(not(windows))]
#[tokio::test]
async fn test_exec_shell_combined_output_uses_single_stream() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let tool = BashTool::new("Bash");
    let command = "printf 'out\\n'; printf 'err\\n' >&2";

    let result = tool
        .execute(json!({"command": command, "combined_output": true}), &ctx)
        .await
        .expect("execute");
    assert!(result.success, "{}", result.content);
    assert!(result.content.contains("out"), "{}", result.content);
    assert!(result.content.contains("err"), "{}", result.content);

    let meta = result.metadata.expect("metadata");
    assert_eq!(
        meta.get("combined_output").and_then(Value::as_bool),
        Some(true)
    );
}

#[test]
fn background_schema_distinguishes_temporary_jobs_from_persistent_services() {
    let schema = BashTool::new("Bash").input_schema();
    let d = schema["properties"]["background"]["description"]
        .as_str()
        .expect("background description");
    assert!(d.contains("killed") && d.contains("background:true") && d.contains("persist:true"));
}

#[tokio::test]
async fn test_exec_shell_foreground_cancel_kills_process() {
    let tmp = tempdir().expect("tempdir");
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let ctx = ToolContext::new(tmp.path()).with_cancel_token(cancel_token.clone());
    let command = sleep_command(30);

    let task = tokio::spawn(async move {
        BashTool::new("Bash")
            .execute(
                json!({
                    "command": command,
                    "timeout_ms": 600_000
                }),
                &ctx,
            )
            .await
            .expect("execute")
    });

    tokio::time::sleep(Duration::from_millis(150)).await;
    cancel_token.cancel();

    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("foreground shell should observe cancellation")
        .expect("task should not panic");

    assert!(!result.success);
    assert!(result.content.contains("Command canceled"));
    let meta = result.metadata.expect("metadata");
    assert_eq!(meta.get("status").and_then(Value::as_str), Some("Killed"));
    assert_eq!(meta.get("canceled").and_then(Value::as_bool), Some(true));
}

#[tokio::test]
async fn test_exec_shell_foreground_can_move_to_background() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let shell_manager = ctx.shell_manager.clone();
    let command = sleep_command(30);
    let task_ctx = ctx.clone();

    let task = tokio::spawn(async move {
        BashTool::new("Bash")
            .execute(
                json!({
                    "command": command,
                    "timeout_ms": 600_000
                }),
                &task_ctx,
            )
            .await
            .expect("execute")
    });

    await_shell_wait_registration(&ctx).await;
    shell_manager
        .lock()
        .expect("shell manager lock")
        .request_shell_wait_detach("workspace");

    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("foreground shell should detach")
        .expect("task should not panic");

    assert!(result.success);
    assert!(result.content.contains("moved to the background"));
    // The detach message points the model at a tool it can actually call.
    // `Bash` is hidden; `task_shell_wait` is eager, so the message names it directly.
    assert!(
        result.content.contains("`task_shell_wait` directly"),
        "{}",
        result.content
    );
    assert!(result.content.contains("task_id="), "{}", result.content);
    assert!(result.content.contains("cancel=true"), "{}", result.content);
    assert!(!result.content.contains("exec_shell"), "{}", result.content);

    let meta = result.metadata.expect("metadata");
    assert_eq!(meta.get("status").and_then(Value::as_str), Some("Running"));
    assert_eq!(
        meta.get("backgrounded").and_then(Value::as_bool),
        Some(true)
    );
    let task_id = meta
        .get("task_id")
        .and_then(Value::as_str)
        .expect("task id")
        .to_string();

    let mut manager = shell_manager.lock().expect("shell manager lock");
    let job = manager.inspect_job(&task_id).expect("inspect job");
    assert_eq!(job.snapshot.status, ShellStatus::Running);
    assert!(
        job.snapshot.background,
        "detached foreground work needs a background receipt"
    );
    let killed = manager.kill(&task_id).expect("kill");
    assert_eq!(killed.status, ShellStatus::Killed);
}

#[cfg(unix)]
#[tokio::test]
async fn dropped_foreground_wait_kills_descendants_and_retains_unreceived_output() {
    let tmp = tempdir().unwrap();
    let pid_file = tmp.path().join("descendant.pid");
    let command = format!(
        "printf 'retained-before-drop\\n'; printf '%{}s\\n' x; \
         CODEWHALE_SHELL_DESCENDANT_HELPER=1 \
         CODEWHALE_SHELL_DESCENDANT_PID_FILE={} {} --exact \
         tools::shell::tests::shell_descendant_helper_process --nocapture",
        RAW_STREAM_SETTLED_TAIL_BYTES + 1024,
        shell_words::quote(&pid_file.display().to_string()),
        shell_words::quote(&std::env::current_exe().unwrap().display().to_string()),
    );
    let ctx = ToolContext::new(tmp.path()).with_state_namespace("foreground-drop".to_string());
    let manager = ctx.shell_manager.clone();
    let task_ctx = ctx.clone();
    let task = tokio::spawn(async move {
        BashTool::new("Bash")
            .execute(
                json!({"command": command, "timeout_ms": 600_000}),
                &task_ctx,
            )
            .await
    });
    let descendant = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(raw) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = raw.trim().parse::<libc::pid_t>()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("descendant must start");
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(
        wait_for_shell_pid_exit(descendant),
        "dropping the wait must stop its descendant"
    );
    let mut manager = manager.lock().unwrap();
    let jobs = manager.list_jobs_for_session("foreground-drop");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status, ShellStatus::Killed);
    let detail = manager
        .inspect_job(&jobs[0].id)
        .expect("inspect cancelled job");
    assert!(detail.stdout.contains("retained-before-drop"));
    assert!(detail.stdout.len() > RAW_STREAM_SETTLED_TAIL_BYTES);
    assert!(!manager.has_finished_unreported_jobs_for_session("foreground-drop"));
}

#[tokio::test]
async fn lowercase_bash_foreground_detach_is_a_successful_running_receipt() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let shell_manager = ctx.shell_manager.clone();
    let command = sleep_command(30);
    let task_ctx = ctx.clone();

    let task = tokio::spawn(async move {
        LowercaseBashTool
            .execute(json!({"command": command}), &task_ctx)
            .await
            .expect("execute")
    });

    await_shell_wait_registration(&ctx).await;
    shell_manager
        .lock()
        .expect("shell manager lock")
        .request_shell_wait_detach("workspace");

    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("foreground shell should detach")
        .expect("task should not panic");

    assert!(result.success, "{}", result.content);
    assert!(
        result.content.contains("moved to the background")
            && result.content.contains("not killed")
            && result.content.contains("`task_shell_wait` directly"),
        "{}",
        result.content
    );
    // A model that owns a stuck command learns here how to stop it; no other
    // tool in its catalog can.
    assert!(result.content.contains("cancel=true"), "{}", result.content);
    assert!(!result.content.contains("code -1"), "{}", result.content);
    let metadata = result.metadata.expect("metadata");
    assert_eq!(metadata["status"], "Running");
    assert_eq!(metadata["backgrounded"], true);
    let task_id = metadata["task_id"].as_str().expect("task id");

    let mut manager = shell_manager.lock().expect("shell manager lock");
    let job = manager.inspect_job(task_id).expect("inspect job");
    assert_eq!(job.snapshot.status, ShellStatus::Running);
    manager.kill(task_id).expect("kill test job");
}

async fn start_wait_detach_job(context: &ToolContext, command: String) -> String {
    BashTool::new("Bash")
        .execute(json!({"command": command, "background": true}), context)
        .await
        .expect("start owned background job")
        .metadata
        .unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn await_shell_wait_registration(context: &ToolContext) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if context
                .shell_manager
                .lock()
                .unwrap()
                .shell_wait_active_for_test(&context.state_namespace)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("blocking shell wait registered");
}

fn wait_detach_output_command(marker: &str) -> String {
    let separator = if cfg!(windows)
        && !codewhale_sandbox::shell_dispatcher::global_dispatcher()
            .kind()
            .is_powershell()
    {
        "&"
    } else {
        ";"
    };
    format!("{} {separator} {}", echo_command(marker), sleep_command(30))
}

async fn await_wait_detach_output(context: &ToolContext, id: &str, marker: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if context
                .shell_manager
                .lock()
                .unwrap()
                .inspect_job(id)
                .unwrap()
                .stdout
                .contains(marker)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("job produced output before wait");
}

#[tokio::test]
async fn background_wait_detaches_promptly_preserves_output_and_owned_process() {
    let tmp = tempdir().unwrap();
    let context = ToolContext::new(tmp.path()).with_state_namespace("detach-owner");
    let id =
        start_wait_detach_job(&context, wait_detach_output_command("detach-output-canary")).await;
    await_wait_detach_output(&context, &id, "detach-output-canary").await;
    let wait_context = context.clone();
    let wait_id = id.clone();
    let mut waiting = tokio::spawn(async move {
        BashTool::alias("exec_shell_wait", "wait")
            .execute(
                json!({"task_id": wait_id, "timeout_ms": 600_000}),
                &wait_context,
            )
            .await
    });
    await_shell_wait_registration(&context).await;
    assert!(
        !context
            .shell_manager
            .lock()
            .unwrap()
            .request_shell_wait_detach("other-session")
    );
    assert!(!waiting.is_finished());
    assert!(
        context
            .shell_manager
            .lock()
            .unwrap()
            .request_shell_wait_detach(&context.state_namespace)
    );
    let returned = tokio::time::timeout(Duration::from_secs(3), &mut waiting).await;
    if returned.is_err() {
        waiting.abort();
        context
            .shell_manager
            .lock()
            .unwrap()
            .kill_for_session(&context.state_namespace, &id)
            .unwrap();
    }
    let result = returned
        .expect("detach must release a ten-minute wait promptly")
        .unwrap()
        .unwrap();
    let alive = context
        .shell_manager
        .lock()
        .unwrap()
        .inspect_job_for_session(&context.state_namespace, &id)
        .unwrap()
        .snapshot
        .status;
    let next = BashTool::alias("exec_shell_wait", "wait")
        .execute(json!({"task_id": id, "wait": false}), &context)
        .await
        .unwrap();
    context
        .shell_manager
        .lock()
        .unwrap()
        .kill_for_session(&context.state_namespace, &id)
        .unwrap();
    assert!(result.success);
    assert_eq!(result.metadata.as_ref().unwrap()["status"], "Running");
    assert_eq!(result.metadata.as_ref().unwrap()["wait_detached"], true);
    assert!(
        result
            .metadata
            .as_ref()
            .unwrap()
            .get("wait_canceled")
            .is_none()
    );
    assert_eq!(alive, ShellStatus::Running);
    assert_eq!(result.content.matches("detach-output-canary").count(), 1);
    assert!(
        !next.content.contains("detach-output-canary"),
        "detach consumes each output byte once"
    );
    assert!(
        !context
            .shell_manager
            .lock()
            .unwrap()
            .request_shell_wait_detach(&context.state_namespace)
    );
}

#[tokio::test]
async fn multi_background_wait_detach_is_session_scoped_and_rejects_foreign_jobs() {
    let tmp = tempdir().unwrap();
    let owner = ToolContext::new(tmp.path()).with_state_namespace("multi-owner");
    let foreign = owner.clone().with_state_namespace("multi-foreign");
    let first =
        start_wait_detach_job(&owner, wait_detach_output_command("multi-output-canary")).await;
    await_wait_detach_output(&owner, &first, "multi-output-canary").await;
    let second = start_wait_detach_job(&owner, sleep_command(30)).await;
    let other = start_wait_detach_job(&foreign, sleep_command(30)).await;
    let rejected = BashTool::alias("exec_shell_wait", "wait")
        .execute(
            json!({"task_ids": [first, other], "timeout_ms": 600_000}),
            &owner,
        )
        .await;
    assert!(
        rejected.is_err(),
        "every job must belong to the waiting session"
    );
    assert!(
        !owner
            .shell_manager
            .lock()
            .unwrap()
            .shell_wait_active_for_test(&owner.state_namespace)
    );
    let wait_context = owner.clone();
    let ids = vec![first.clone(), second.clone()];
    let mut waiting = tokio::spawn(async move {
        BashTool::alias("exec_shell_wait", "wait")
            .execute(
                json!({"task_ids": ids, "until": "all", "timeout_ms": 600_000}),
                &wait_context,
            )
            .await
    });
    let foreign_context = foreign.clone();
    let other_id = other.clone();
    let mut foreign_wait = tokio::spawn(async move {
        BashTool::alias("exec_shell_wait", "wait")
            .execute(
                json!({"task_id": other_id, "timeout_ms": 600_000}),
                &foreign_context,
            )
            .await
    });
    await_shell_wait_registration(&owner).await;
    await_shell_wait_registration(&foreign).await;
    assert!(
        owner
            .shell_manager
            .lock()
            .unwrap()
            .request_shell_wait_detach(&owner.state_namespace)
    );
    let returned = tokio::time::timeout(Duration::from_secs(3), &mut waiting).await;
    let foreign_still_waiting = !foreign_wait.is_finished();
    assert!(
        owner
            .shell_manager
            .lock()
            .unwrap()
            .request_shell_wait_detach(&foreign.state_namespace)
    );
    let foreign_returned = tokio::time::timeout(Duration::from_secs(3), &mut foreign_wait).await;
    waiting.abort();
    foreign_wait.abort();
    let first_output = BashTool::alias("exec_shell_wait", "wait")
        .execute(json!({"task_id": first, "wait": false}), &owner)
        .await
        .unwrap();
    let next_output = BashTool::alias("exec_shell_wait", "wait")
        .execute(json!({"task_id": first, "wait": false}), &owner)
        .await
        .unwrap();
    let mut manager = owner.shell_manager.lock().unwrap();
    let statuses: Vec<_> = [&first, &second, &other]
        .into_iter()
        .map(|id| manager.inspect_job(id).unwrap().snapshot.status)
        .collect();
    manager
        .kill_for_session(&owner.state_namespace, &first)
        .unwrap();
    manager
        .kill_for_session(&owner.state_namespace, &second)
        .unwrap();
    manager
        .kill_for_session(&foreign.state_namespace, &other)
        .unwrap();
    drop(manager);
    let result = returned
        .expect("multi-wait must detach promptly")
        .unwrap()
        .unwrap();
    assert!(result.success);
    assert_eq!(result.metadata.as_ref().unwrap()["wait_detached"], true);
    assert_eq!(
        result.metadata.as_ref().unwrap()["statuses"][&first],
        "Running"
    );
    assert_eq!(
        result.metadata.as_ref().unwrap()["statuses"][&second],
        "Running"
    );
    assert!(foreign_still_waiting, "detaching A must not release B");
    let foreign_result = foreign_returned
        .expect("foreign session can release its own wait")
        .unwrap()
        .unwrap();
    assert_eq!(
        foreign_result.metadata.as_ref().unwrap()["wait_detached"],
        true
    );
    assert_eq!(
        first_output.content.matches("multi-output-canary").count(),
        1
    );
    assert!(
        !next_output.content.contains("multi-output-canary"),
        "multi-wait must not consume output cursors"
    );
    assert!(
        statuses
            .into_iter()
            .all(|status| status == ShellStatus::Running)
    );
    assert!(
        !owner
            .shell_manager
            .lock()
            .unwrap()
            .request_shell_wait_detach(&owner.state_namespace)
    );
}

#[tokio::test]
async fn shell_wait_detach_expires_and_turn_cancellation_keeps_precedence() {
    let tmp = tempdir().unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let context = ToolContext::new(tmp.path())
        .with_state_namespace("cancel-owner")
        .with_cancel_token(cancel.clone());
    let id = start_wait_detach_job(&context, sleep_command(30)).await;
    {
        let mut manager = context.shell_manager.lock().unwrap();
        let previous = manager.register_shell_wait(&context.state_namespace);
        assert!(manager.request_shell_wait_detach(&context.state_namespace));
        drop(previous);
        assert!(!manager.request_shell_wait_detach(&context.state_namespace));
    }
    // An old request must not free a fresh wait. Dropping its future must also
    // unregister it, so a later request cannot affect the next call.
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            BashTool::alias("exec_shell_wait", "wait")
                .execute(json!({"task_id": id, "timeout_ms": 600_000}), &context)
        )
        .await
        .is_err()
    );
    assert!(
        !context
            .shell_manager
            .lock()
            .unwrap()
            .request_shell_wait_detach(&context.state_namespace)
    );
    let wait_context = context.clone();
    let wait_id = id.clone();
    let mut waiting = tokio::spawn(async move {
        BashTool::alias("exec_shell_wait", "wait")
            .execute(
                json!({"task_id": wait_id, "timeout_ms": 600_000}),
                &wait_context,
            )
            .await
    });
    await_shell_wait_registration(&context).await;
    context
        .shell_manager
        .lock()
        .unwrap()
        .request_shell_wait_detach(&context.state_namespace);
    cancel.cancel();
    let returned = tokio::time::timeout(Duration::from_secs(3), &mut waiting).await;
    waiting.abort();
    let alive = context
        .shell_manager
        .lock()
        .unwrap()
        .inspect_job(&id)
        .unwrap()
        .snapshot
        .status;
    context
        .shell_manager
        .lock()
        .unwrap()
        .kill_for_session(&context.state_namespace, &id)
        .unwrap();
    let result = returned
        .expect("cancel releases wait promptly")
        .unwrap()
        .unwrap();
    assert_eq!(result.metadata.as_ref().unwrap()["wait_canceled"], true);
    assert!(
        result
            .metadata
            .as_ref()
            .unwrap()
            .get("wait_detached")
            .is_none()
    );
    assert_eq!(alive, ShellStatus::Running);
}

#[tokio::test]
async fn test_exec_shell_wait_cancel_leaves_background_process_running() {
    let tmp = tempdir().expect("tempdir");
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let ctx = ToolContext::new(tmp.path()).with_cancel_token(cancel_token.clone());
    let shell_manager = ctx.shell_manager.clone();
    let started = execute_shell(
        &mut shell_manager.lock().expect("shell manager lock"),
        &sleep_command(30),
        None,
        600_000,
        true,
    )
    .expect("execute");
    let task_id = started.task_id.expect("task id");
    let wait_task_id = task_id.clone();
    let task_ctx = ctx.clone();

    let task = tokio::spawn(async move {
        BashTool::new("Bash")
            .execute(
                json!({
                    "action": "wait",
                    "task_id": wait_task_id,
                    "timeout_ms": 600_000
                }),
                &task_ctx,
            )
            .await
            .expect("wait")
    });

    tokio::time::sleep(Duration::from_millis(150)).await;
    cancel_token.cancel();

    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("wait should observe cancellation")
        .expect("task should not panic");

    assert!(result.success);
    assert!(result.content.contains("still running"));
    let meta = result.metadata.expect("metadata");
    assert_eq!(meta.get("status").and_then(Value::as_str), Some("Running"));
    assert_eq!(
        meta.get("wait_canceled").and_then(Value::as_bool),
        Some(true)
    );

    let mut manager = shell_manager.lock().expect("shell manager lock");
    let job = manager.inspect_job(&task_id).expect("inspect job");
    assert_eq!(job.snapshot.status, ShellStatus::Running);
    let killed = manager.kill(&task_id).expect("kill");
    assert_eq!(killed.status, ShellStatus::Killed);
}

#[tokio::test]
async fn test_completed_background_shell_releases_process_handles() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let shell_manager = ctx.shell_manager.clone();
    let started = execute_shell(
        &mut shell_manager.lock().expect("shell manager lock"),
        &echo_command("done"),
        None,
        600_000,
        true,
    )
    .expect("execute");
    let task_id = started.task_id.expect("task id");

    let result = BashTool::alias("exec_shell_wait", "wait")
        .execute(
            json!({
                "task_id": task_id.clone(),
                "wait": true,
                "timeout_ms": BACKGROUND_COMPLETION_WAIT_MS
            }),
            &ctx,
        )
        .await
        .expect("wait");

    assert!(result.success);
    let mut manager = shell_manager.lock().expect("shell manager lock");
    let result = wait_for_completed_shell(&mut manager, &task_id);
    assert_eq!(result.status, ShellStatus::Completed);
    let shell = manager.processes.get_mut(&task_id).expect("tracked shell");
    shell.poll();
    assert_eq!(shell.status, ShellStatus::Completed);
    assert!(shell.stdin.is_none());
    assert!(shell.child.is_none());
    assert!(shell.stdout_thread.is_none());
    assert!(shell.stderr_thread.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn exec_shell_cancel_kills_descendant_process_group() {
    let tmp = tempdir().expect("tempdir");
    let pid_file = tmp.path().join("descendant.pid");
    let test_binary = std::env::current_exe().expect("current test binary");
    let command = format!(
        "{} --exact {} --nocapture",
        shell_words::quote(&test_binary.display().to_string()),
        shell_words::quote("tools::shell::tests::shell_descendant_helper_process"),
    );
    let ctx = ToolContext::new(tmp.path());
    let mut env = std::collections::HashMap::new();
    env.insert(SHELL_DESCENDANT_HELPER_ENV.to_string(), "1".to_string());
    env.insert(
        SHELL_DESCENDANT_PID_FILE_ENV.to_string(),
        pid_file.display().to_string(),
    );
    let started = ctx
        .shell_manager
        .lock()
        .expect("shell manager")
        .execute_with_options_env_for_session(
            &command,
            None,
            60_000,
            true,
            None,
            false,
            None,
            env,
            &ctx.state_namespace,
        )
        .expect("start descendant tree");
    let task_id = started.task_id.expect("task id");
    let descendant = wait_for_shell_pid_file(&pid_file);

    let result = BashTool::alias("exec_shell_cancel", "cancel")
        .execute(json!({"task_id": task_id}), &ctx)
        .await
        .expect("cancel process group");
    assert!(result.success);
    assert!(
        wait_for_shell_pid_exit(descendant),
        "descendant {descendant} survived shell process-group cancellation"
    );
}

#[tokio::test]
async fn test_exec_shell_cancel_tool_kills_background_process() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let shell_manager = ctx.shell_manager.clone();
    let started = execute_shell(
        &mut shell_manager.lock().expect("shell manager lock"),
        &sleep_command(30),
        None,
        600_000,
        true,
    )
    .expect("execute");
    let task_id = started.task_id.expect("task id");

    let result = BashTool::alias("exec_shell_cancel", "cancel")
        .execute(json!({ "task_id": task_id }), &ctx)
        .await
        .expect("cancel");

    assert!(result.success);
    assert!(result.content.contains("Canceled background command"));
    let meta = result.metadata.expect("metadata");
    assert_eq!(meta.get("status").and_then(Value::as_str), Some("Killed"));

    let task_id = meta
        .get("task_id")
        .and_then(Value::as_str)
        .expect("task id");
    let mut manager = shell_manager.lock().expect("shell manager lock");
    let job = manager.inspect_job(task_id).expect("inspect job");
    assert_eq!(job.snapshot.status, ShellStatus::Killed);
}

#[tokio::test]
async fn test_exec_shell_cancel_tool_can_kill_all_running_processes() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let shell_manager = ctx.shell_manager.clone();
    let first = execute_shell(
        &mut shell_manager.lock().expect("shell manager lock"),
        &sleep_command(30),
        None,
        600_000,
        true,
    )
    .expect("execute first")
    .task_id
    .expect("first task id");
    let second = execute_shell(
        &mut shell_manager.lock().expect("shell manager lock"),
        &sleep_command(30),
        None,
        600_000,
        true,
    )
    .expect("execute second")
    .task_id
    .expect("second task id");

    let result = BashTool::alias("exec_shell_cancel", "cancel")
        .execute(json!({ "all": true }), &ctx)
        .await
        .expect("cancel all");

    assert!(result.success);
    let meta = result.metadata.expect("metadata");
    assert_eq!(meta.get("status").and_then(Value::as_str), Some("Killed"));
    assert_eq!(meta.get("canceled").and_then(Value::as_u64), Some(2));

    let mut manager = shell_manager.lock().expect("shell manager lock");
    let first_job = manager.inspect_job(&first).expect("inspect first");
    let second_job = manager.inspect_job(&second).expect("inspect second");
    assert_eq!(first_job.snapshot.status, ShellStatus::Killed);
    assert_eq!(second_job.snapshot.status, ShellStatus::Killed);
}

fn make_failed_result(stderr: &str) -> ShellResult {
    ShellResult {
        task_id: None,
        status: ShellStatus::Failed,
        exit_code: Some(1),
        stdout: String::new(),
        stderr: stderr.to_string(),
        duration_ms: 0,
        stdout_len: 0,
        stderr_len: stderr.len(),
        stdout_omitted: 0,
        stderr_omitted: 0,
        stdout_truncated: false,
        sandboxed: false,
        sandbox_type: None,
        sandbox_denied: false,
        stderr_truncated: false,
    }
}

#[test]
fn test_macos_provenance_detected_by_activity_time_message() {
    let result = make_failed_result(
        "failed to update builder last activity time: open \
         /Users/user/.docker/buildx/activity/.tmp-abc: operation not permitted",
    );
    assert!(looks_like_macos_provenance_failure(&result));
}

#[test]
fn test_macos_provenance_detected_by_activity_path_and_eperm() {
    let result = make_failed_result(
        "error: open /home/user/.docker/buildx/activity/foo: operation not permitted",
    );
    assert!(looks_like_macos_provenance_failure(&result));
}

#[test]
fn test_macos_provenance_not_triggered_on_success() {
    let mut result = make_failed_result(
        "failed to update builder last activity time: open \
         /Users/user/.docker/buildx/activity/.tmp-abc: operation not permitted",
    );
    result.status = ShellStatus::Completed;
    result.exit_code = Some(0);
    assert!(!looks_like_macos_provenance_failure(&result));
}

#[test]
fn test_macos_provenance_not_triggered_on_unrelated_eperm() {
    let result = make_failed_result("open /some/other/path: operation not permitted");
    assert!(!looks_like_macos_provenance_failure(&result));
}

// Regression test for #828: shell spawns an orphaned background subprocess
// (simulating `nohup curl`) that keeps the pipe write-end open after the shell
// exits. collect_output() must not block indefinitely — it kills the whole
// process group first, allowing reader threads to get EOF and exit.
#[cfg(unix)]
#[test]
fn test_orphaned_subprocess_does_not_block_collect_output() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    // sh spawns `sleep 100 &` and exits; the sleep subprocess inherits the
    // pipe write-ends and would keep reader threads blocked without the fix.
    let result =
        execute_shell(&mut manager, "sh -c 'sleep 100 &'", None, 5000, true).expect("execute");
    let task_id = result.task_id.expect("task id");

    // Drive to completion with a tight timeout — must not hang.
    let done = manager
        .get_output(&task_id, true, 3000)
        .expect("get_output must complete, not hang");
    assert_eq!(done.status, ShellStatus::Completed);
}

#[cfg(unix)]
#[test]
fn foreground_shell_does_not_block_on_orphaned_subprocess_pipe() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let started = std::time::Instant::now();
    let result = execute_shell(&mut manager, "sh -c 'sleep 100 &'", None, 5000, false)
        .expect("foreground execute must complete, not hang");

    assert!(
        started.elapsed() < std::time::Duration::from_secs(4),
        "foreground execute blocked on descendant pipe handles"
    );
    assert_eq!(result.status, ShellStatus::Completed);
}

// Windows equivalent of the orphaned pipe-handle regression. `cmd /c start /b`
// launches a descendant process that inherits stdout/stderr and outlives the
// shell. Job-object cleanup must terminate that descendant before reader-thread
// joins, otherwise get_output() blocks until ping exits.
#[cfg(windows)]
#[test]
fn background_collection_does_not_block_on_detached_descendant_pipe() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let result = execute_shell(
        &mut manager,
        r#"cmd /c start "" /b ping 127.0.0.1 -n 4"#,
        None,
        5000,
        true,
    )
    .expect("execute");
    let task_id = result.task_id.expect("task id");

    let started = std::time::Instant::now();
    let done = manager
        .get_output(&task_id, true, 3000)
        .expect("get_output must complete, not hang");

    assert!(
        started.elapsed() < std::time::Duration::from_secs(6),
        "get_output blocked on descendant pipe handles"
    );
    assert_eq!(done.status, ShellStatus::Completed);
}

#[cfg(windows)]
#[test]
fn windows_job_terminate_denied_falls_back_to_child_kill() {
    let mut child = Command::new("ping")
        .args(["127.0.0.1", "-n", "20"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ping");

    let job = WindowsJob::attach_to_child(&child).expect("attach job");
    let limited_job = duplicate_job_without_terminate_access(job);

    assert!(
        limited_job.terminate().is_err(),
        "limited job handle should not allow TerminateJobObject"
    );

    terminate_child_and_close_windows_job(Some(limited_job), &mut child)
        .expect("fallback child kill");

    let status = child
        .wait_timeout(std::time::Duration::from_secs(3))
        .expect("wait after fallback kill");
    assert!(
        status.is_some(),
        "fallback child kill should terminate child"
    );
}

#[cfg(windows)]
#[test]
fn windows_job_close_releases_foreground_reader_threads_when_terminate_denied() {
    let mut child = Command::new("ping")
        .args(["127.0.0.1", "-n", "8"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ping");

    let job = WindowsJob::attach_to_child(&child).expect("attach job");
    let limited_job = duplicate_job_without_terminate_access(job);
    assert!(
        limited_job.terminate().is_err(),
        "limited job handle should not allow TerminateJobObject"
    );

    let stdout_handle = child.stdout.take().expect("stdout pipe");
    let stderr_handle = child.stderr.take().expect("stderr pipe");
    let stdout_thread = std::thread::spawn(move || {
        let mut reader = stdout_handle;
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        buf
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut reader = stderr_handle;
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        buf
    });

    let started = std::time::Instant::now();
    terminate_and_close_windows_job(Some(limited_job));
    let _ = stdout_thread.join().unwrap_or_default();
    let _ = stderr_thread.join().unwrap_or_default();
    let status = child
        .wait_timeout(std::time::Duration::from_secs(3))
        .expect("wait after kill-on-close");

    assert!(
        started.elapsed() < std::time::Duration::from_secs(4),
        "reader joins waited for natural descendant exit instead of kill-on-close"
    );
    assert!(status.is_some(), "kill-on-close should terminate child");
}

#[cfg(windows)]
#[test]
fn windows_job_kill_on_close_releases_reader_threads_when_terminate_denied() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let result = execute_shell(
        &mut manager,
        r#"cmd /c start "" /b ping 127.0.0.1 -n 8"#,
        None,
        5000,
        true,
    )
    .expect("execute");
    let task_id = result.task_id.expect("task id");

    {
        let shell = manager
            .processes
            .get_mut(&task_id)
            .expect("background shell");
        let job = shell.windows_job.take().expect("windows job attached");
        let limited_job = duplicate_job_without_terminate_access(job);
        assert!(
            limited_job.terminate().is_err(),
            "limited job handle should not allow TerminateJobObject"
        );
        shell.windows_job = Some(limited_job);
    }

    let started = std::time::Instant::now();
    let done = manager
        .get_output(&task_id, true, 3000)
        .expect("get_output must complete via kill-on-close fallback");

    assert!(
        started.elapsed() < std::time::Duration::from_secs(4),
        "get_output waited for natural descendant exit instead of kill-on-close"
    );
    assert_eq!(done.status, ShellStatus::Completed);
}

#[cfg(windows)]
#[test]
fn killed_shell_does_not_wait_for_blocked_reader_threads() {
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let stdout_thread = std::thread::spawn(move || {
        let _ = release_rx.recv();
    });
    let now = std::time::Instant::now();
    let mut shell = BackgroundShell {
        id: "killed-reader".to_string(),
        owner_session_id: "windows-test-session".to_string(),
        command: "test".to_string(),
        working_dir: std::path::PathBuf::from("."),
        status: ShellStatus::Killed,
        exit_code: None,
        started_at: now,
        finished_at: Some(now),
        finished_at_utc: Some(chrono::Utc::now()),
        background: true,
        last_output_at: now,
        last_observed_output_len: 0,
        sandbox_type: SandboxType::None,
        ownership: ShellOwnership::Managed,
        linked_task_id: None,
        owner_agent: None,
        origin_tool_call_id: None,
        origin_turn_id: None,
        stdout_buffer: super::new_shared_raw_output(),
        stderr_buffer: None,
        heavy_permit: None,
        stdout_cursor: 0,
        stderr_cursor: 0,
        completion_reported: false,
        bounded_output: None,
        stdin: None,
        pty_master: None,
        terminal_size: None,
        child: None,
        windows_job: None,
        stdout_thread: Some(stdout_thread),
        stderr_thread: None,
        work_lifecycle: None,
        lifecycle_seq: 0,
        last_lifecycle_status: None,
        last_lifecycle_bytes: 0,
        wait_failed: false,
    };

    let started = std::time::Instant::now();
    shell.collect_output();

    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "killed shell must not synchronously join a blocked reader"
    );
    release_tx.send(()).expect("release detached reader");
}

#[test]
fn test_list_jobs_cleans_up_completed_old_processes() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let bg =
        execute_shell(&mut manager, &echo_command("bg"), None, 5000, true).expect("execute bg");
    let bg_id = bg.task_id.expect("bg task id");
    manager.get_output(&bg_id, true, 3000).expect("bg done");

    // Both the completed job and any tracking state should be present.
    assert!(!manager.processes.is_empty());

    // cleanup(ZERO) removes every delivered completed process immediately.
    manager.drain_finished_jobs_with_evidence();
    manager.cleanup(Duration::ZERO);
    assert!(
        manager.processes.is_empty(),
        "completed processes should be evicted by cleanup"
    );
}

/// A job that ran longer than the retention age must survive until its
/// completion is delivered: age counts from the finish, not the start.
#[test]
fn cleanup_ages_jobs_from_finish() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());
    let long_run = FINISHED_SHELL_MAX_AGE + Duration::from_secs(600);
    manager.seed_finished_record_for_test("long-job", long_run);

    // Started 70 minutes ago, finished just now, not yet delivered.
    manager.list_jobs();
    assert!(
        manager.inspect_job("long-job").is_ok(),
        "a long job must not be evicted the moment it finishes"
    );
    let delivered = manager.drain_finished_jobs_with_evidence();
    assert_eq!(delivered.len(), 1, "the completion must still be delivered");
    assert_eq!(delivered[0].event.task_id, "long-job");

    // Delivered and recently finished: still inside the retention window.
    manager.cleanup(FINISHED_SHELL_MAX_AGE);
    assert!(manager.inspect_job("long-job").is_ok());

    // Delivered and finished longer ago than the window: evicted.
    let shell = manager.processes.get_mut("long-job").expect("record");
    shell.finished_at = Instant::now().checked_sub(long_run);
    manager.cleanup(FINISHED_SHELL_MAX_AGE);
    assert!(manager.inspect_job("long-job").is_err());
}

/// Nothing ever drains a completion owned by a Runtime API scope or by a
/// session that is no longer active, so an undelivered job must still age out
/// (counted from when it finished) instead of staying until the count cap.
#[test]
fn cleanup_ages_out_undelivered_completions_after_finish() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());
    let max_age = Duration::from_secs(30);
    manager.seed_finished_record_for_test("api-job", Duration::from_secs(50));
    let shell = manager.processes.get_mut("api-job").expect("record");
    shell.owner_session_id = "api:thread-1".to_string();
    shell.finished_at = Instant::now().checked_sub(Duration::from_secs(40));
    assert!(!shell.completion_reported);

    manager.cleanup(max_age);
    assert!(
        manager.inspect_job("api-job").is_err(),
        "an undelivered completion finished longer ago than max_age must age out"
    );
}

/// The count ceiling evicts the records that finished longest ago, the same
/// clock the age rule uses: a long job that just finished is not "oldest".
#[test]
fn finished_job_bounds_evict_by_finish_time() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());
    let now = Instant::now();
    for index in 0..MAX_FINISHED_SHELL_RECORDS {
        let id = format!("short-{index:03}");
        // Started 20 s ago, finished 10 s ago.
        manager.seed_finished_record_for_test(id.clone(), Duration::from_secs(20));
        let shell = manager.processes.get_mut(&id).expect("record");
        shell.finished_at = now.checked_sub(Duration::from_secs(10));
        shell.completion_reported = true;
    }
    // Started 60 s ago, finished just now.
    manager.seed_finished_record_for_test("long-job", Duration::from_secs(60));
    manager
        .processes
        .get_mut("long-job")
        .expect("record")
        .completion_reported = true;

    manager.cleanup(FINISHED_SHELL_MAX_AGE);
    assert_eq!(manager.tracked_job_count(), MAX_FINISHED_SHELL_RECORDS);
    assert!(
        manager.inspect_job("long-job").is_ok(),
        "the most recently finished job was evicted first"
    );
}

/// The synchronous path closes stdin when there is no input: `cat` gets EOF
/// instead of reading, or blocking on, Codewhale's own stdin.
#[cfg(unix)]
#[test]
fn sync_command_without_input_gets_eof_not_inherited_stdin() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());
    let started = Instant::now();
    let result = manager
        .execute_with_options_env(
            "cat; echo after-eof",
            None,
            15_000,
            false,
            None,
            false,
            None,
            HashMap::new(),
        )
        .expect("run");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "stdin read blocked for {:?}",
        started.elapsed()
    );
    assert!(result.stdout.contains("after-eof"), "{}", result.stdout);
}

/// A backgrounded lowercase `bash` job: each read returns only output the
/// caller has not seen, not the whole retained tail again.
#[cfg(unix)]
#[test]
fn bounded_job_delta_returns_only_new_output() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());
    let started = manager
        .execute_with_options_env_for_owner_and_work(
            "printf 'first-chunk\\n'; while [ ! -e go ]; do sleep 0.05; done; printf 'second-chunk\\n'",
            None,
            60_000,
            true,
            None,
            false,
            None,
            HashMap::new(),
            None,
            String::new(),
            None,
            None,
            None,
            None,
            false,
            (1, BASH_MAX_TIMEOUT_MS),
        )
        .expect("spawn bounded job");
    let task_id = started.task_id.expect("task id");
    assert!(
        manager.processes[&task_id].bounded_output.is_some(),
        "fixture must exercise the bounded accumulator"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = String::new();
    while !seen.contains("first-chunk") {
        assert!(Instant::now() < deadline, "first chunk never arrived");
        let delta = manager
            .get_output_delta(&task_id, false, 0)
            .expect("first delta");
        seen.push_str(&delta.result.stdout);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(seen.matches("first-chunk").count(), 1, "{seen}");
    // The fixture writes its second chunk only now, so the first delta cannot
    // have raced past it.
    std::fs::write(tmp.path().join("go"), "").expect("release fixture");

    let rest = manager
        .get_output_delta(&task_id, true, 10_000)
        .expect("remaining delta");
    assert_ne!(rest.result.status, ShellStatus::Running);
    assert!(
        rest.result.stdout.contains("second-chunk"),
        "{}",
        rest.result.stdout
    );
    assert!(
        !rest.result.stdout.contains("first-chunk"),
        "a delta must not repeat output already returned: {:?}",
        rest.result.stdout
    );
}

/// A foreground command that reads stdin gets EOF instead of blocking until
/// the timeout kills it.
#[cfg(unix)]
#[tokio::test]
async fn foreground_command_reading_stdin_gets_eof() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let started = Instant::now();
    let result = BashTool::new("Bash")
        .execute(
            json!({"command": "cat; echo after-eof", "timeout_ms": 15_000}),
            &ctx,
        )
        .await
        .expect("execute");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "stdin read blocked for {:?}",
        started.elapsed()
    );
    assert!(result.content.contains("after-eof"), "{}", result.content);
}

/// Regression for #1691: a `git commit -m "feat: complete sub-pages"` shell
/// command must reach the OS shell with its quoted message intact (one argv
/// slot), never split into `feat:` / `complete` / `sub-pages"`.
#[test]
fn issue_1691_quoted_commit_message_round_trips() {
    let cmd = r#"git commit -m "feat: complete sub-pages""#;
    let spec = CommandSpec::shell(
        cmd,
        std::path::PathBuf::from("/tmp"),
        Duration::from_secs(5),
    );

    let dispatcher = codewhale_sandbox::shell_dispatcher::global_dispatcher();
    // The whole command (with quotes) is a single argv entry. The actual
    // shell binary can vary by platform — and the dispatcher may wrap the
    // payload (encoding prefix, exit-code capture) — but the payload itself
    // must stay intact in ONE shell arg. We never split the command string
    // ourselves. This single-line ASCII command never takes the PowerShell
    // temp `-File` path, so the payload stays on the argv.
    assert_eq!(spec.program, dispatcher.kind().binary());
    let carriers = spec
        .args
        .iter()
        .filter(|arg| arg.contains(r#""feat: complete sub-pages""#))
        .count();
    assert_eq!(carriers, 1, "args: {:?}", spec.args);
    assert!(
        !spec
            .args
            .iter()
            .any(|arg| arg == "feat:" || arg == "complete" || arg == "sub-pages\""),
        "args: {:?}",
        spec.args
    );
    assert_eq!(spec.display_command(), cmd);

    let mut built = Command::new(&spec.program);
    push_shell_args(&mut built, &spec.program, &spec.args);
    let got: Vec<String> = built
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(got, spec.args);
}

/// When no `cwd` is provided, the shell should run in `context.workspace`,
/// not in the ShellManager's default_workspace. This ensures sub-agents in
/// worktrees run commands in the worktree directory rather than the parent.
///
/// Without the `context.workspace` default (stashed): runs in sm_dir → FAILS
/// With the `context.workspace` default (unstashed): runs in ctx_dir → PASSES
#[tokio::test]
async fn default_cwd_uses_context_workspace_not_shell_manager_default() {
    let ctx_dir = tempdir().expect("ctx tempdir");
    let sm_dir = tempdir().expect("sm tempdir");

    // Create distinct dirs — write a marker in each so we can tell them apart.
    std::fs::write(ctx_dir.path().join("I_AM_CTX_DIR"), "").unwrap();
    std::fs::write(sm_dir.path().join("I_AM_SM_DIR"), "").unwrap();

    // ToolContext whose workspace is ctx_dir...
    let ctx = ToolContext::new(ctx_dir.path())
        // ...but whose ShellManager's default_workspace is sm_dir.
        .with_shell_manager(new_shared_shell_manager(sm_dir.path().to_path_buf()));

    // Assert directory identity through marker files instead of comparing the
    // shell's printed path. PowerShell and `canonicalize` can spell the same
    // Windows path differently (for example, with a verbatim-path prefix).
    let command = if cfg!(windows) {
        "if (Test-Path -LiteralPath 'I_AM_CTX_DIR') { Write-Output 'context-workspace' } elseif (Test-Path -LiteralPath 'I_AM_SM_DIR') { Write-Output 'manager-workspace' } else { Write-Output 'missing-workspace' }"
    } else {
        "if [ -f I_AM_CTX_DIR ]; then printf 'context-workspace'; elif [ -f I_AM_SM_DIR ]; then printf 'manager-workspace'; else printf 'missing-workspace'; fi"
    };
    let result = BashTool::new("Bash")
        .execute(json!({"command": command}), &ctx)
        .await
        .expect("shell execute");
    assert!(result.success, "command failed: {:?}", result.content);

    assert!(
        result
            .content
            .lines()
            .any(|line| line.trim() == "context-workspace"),
        "expected context.workspace marker, but shell reported: {:?}",
        result.content
    );
}

// ── Kill-path overshoot regression tests (FINISH-0.9.4 #52 multiplier 2) ─────
//
// The foreground Bash kill path must return at ~timeout + a small bounded
// grace, even when the command ignores SIGTERM or a descendant escapes the
// process group while holding the output pipe open. Before the fix, an
// escaped descendant wedged the blocking reader-thread join inside kill()
// until the descendant exited on its own (observed: ~180s past a 120s
// timeout in the wild).

#[cfg(unix)]
const SHELL_SIGTERM_HELPER_ENV: &str = "CODEWHALE_SHELL_SIGTERM_HELPER";
#[cfg(unix)]
const SHELL_ESCAPE_HELPER_ENV: &str = "CODEWHALE_SHELL_ESCAPE_HELPER";
#[cfg(unix)]
const SHELL_ESCAPED_GRANDCHILD_ENV: &str = "CODEWHALE_SHELL_ESCAPED_GRANDCHILD";

/// Helper role: ignore SIGTERM and idle. Runs as the shell's direct child
/// (same process group), so only the SIGKILL escalation can stop it.
#[cfg(unix)]
#[test]
fn shell_sigterm_ignoring_helper_process() {
    if std::env::var(SHELL_SIGTERM_HELPER_ENV).ok().as_deref() != Some("1") {
        return;
    }
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
    let pid_file = PathBuf::from(
        std::env::var(SHELL_DESCENDANT_PID_FILE_ENV).expect("sigterm helper pid file"),
    );
    std::fs::write(pid_file, std::process::id().to_string()).expect("write sigterm helper pid");
    loop {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Helper role: spawn a grandchild in its OWN process group (escaping the
/// shell's group) that inherits the output pipe, then exit immediately. The
/// wrapper shell keeps running (`sleep` after `&`), so the job stays Running
/// while the escaped grandchild holds the reader thread's pipe open.
#[cfg(unix)]
// The grandchild deliberately outlives this helper and is never wait()ed on —
// escaping reaping is exactly what the regression exercises; the test reaps
// it directly via SIGKILL at the end.
#[allow(clippy::zombie_processes)]
#[test]
fn shell_group_escape_helper_process() {
    if std::env::var(SHELL_ESCAPE_HELPER_ENV).ok().as_deref() != Some("1") {
        return;
    }
    let test_binary = std::env::current_exe().expect("current test binary");
    let pid_file = std::env::var(SHELL_DESCENDANT_PID_FILE_ENV).expect("escape pid file");
    let mut cmd = Command::new(test_binary);
    cmd.arg("--exact")
        .arg("tools::shell::tests::shell_escaped_grandchild_helper_process")
        .arg("--nocapture")
        .env(SHELL_ESCAPED_GRANDCHILD_ENV, "1")
        .env(SHELL_DESCENDANT_PID_FILE_ENV, pid_file);
    // A distinct process group is enough to escape `kill(-wrapper_pgid)`;
    // stdout/stderr are inherited, so the grandchild keeps the pipe open.
    #[cfg(unix)]
    cmd.process_group(0);
    let _child = cmd.spawn().expect("spawn escaped grandchild");
}

/// Helper role: the escaped grandchild — ignores SIGTERM, reports its pid,
/// then idles (holding the inherited output pipe open the whole time).
#[cfg(unix)]
#[test]
fn shell_escaped_grandchild_helper_process() {
    if std::env::var(SHELL_ESCAPED_GRANDCHILD_ENV).ok().as_deref() != Some("1") {
        return;
    }
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
    let pid_file =
        PathBuf::from(std::env::var(SHELL_DESCENDANT_PID_FILE_ENV).expect("grandchild pid file"));
    std::fs::write(pid_file, std::process::id().to_string()).expect("write grandchild pid");
    std::thread::sleep(Duration::from_secs(30));
}

/// Regression for the ~180s kill-path overshoot: a descendant that escaped
/// the process group keeps the output pipe open after the group is killed.
/// kill() must still return within a bounded grace instead of blocking on
/// the reader-thread join until the descendant exits on its own.
#[cfg(unix)]
#[tokio::test]
async fn kill_returns_promptly_when_escaped_descendant_holds_pipe_open() {
    let tmp = tempdir().expect("tempdir");
    let pid_file = tmp.path().join("escaped-grandchild.pid");
    let test_binary = std::env::current_exe().expect("current test binary");
    let command = format!(
        "{SHELL_ESCAPE_HELPER_ENV}=1 {SHELL_DESCENDANT_PID_FILE_ENV}={} {} --exact {} --nocapture & sleep 60",
        shell_words::quote(&pid_file.display().to_string()),
        shell_words::quote(&test_binary.display().to_string()),
        shell_words::quote("tools::shell::tests::shell_group_escape_helper_process"),
    );
    let mut manager = ShellManager::new(tmp.path().to_path_buf());
    let started_bg =
        execute_shell(&mut manager, &command, None, 600_000, true).expect("start wrapper");
    let task_id = started_bg.task_id.expect("task id");
    let grandchild = wait_for_shell_pid_file(&pid_file);

    let started = Instant::now();
    let killed = manager.kill(&task_id).expect("kill");
    let wall = started.elapsed();

    assert_eq!(killed.status, ShellStatus::Killed);
    assert!(
        wall < Duration::from_secs(10),
        "kill blocked {wall:?} on a reader wedged by an escaped descendant"
    );

    // Cleanup: the escaped grandchild is out of reach of the group kill by
    // construction; reap it directly so the test does not leak a sleeper.
    unsafe {
        libc::kill(grandchild, libc::SIGKILL);
    }
    assert!(wait_for_shell_pid_exit(grandchild));
}

/// `Bash` was the only action wrapper whose catch-all fell through to its most
/// dangerous branch: an unrecognised action ran the command instead.
#[tokio::test]
async fn unknown_bash_action_is_refused_instead_of_running_the_command() {
    let workspace = tempdir().expect("workspace");
    let context = ToolContext::new(workspace.path().to_path_buf());
    let marker = workspace.path().join("should-not-exist");

    let error = BashTool::new("Bash")
        .execute(
            json!({
                "action": "kill",
                "command": format!("touch {}", marker.display()),
            }),
            &context,
        )
        .await
        .expect_err("unknown action must be refused");

    let message = error.to_string();
    assert!(message.contains("Unknown Bash action"), "{message}");
    assert!(message.contains("kill"), "{message}");
    assert!(
        message.contains("run, wait, interact, cancel"),
        "must name the actions that dispatch: {message}"
    );
    assert!(!marker.exists(), "the command must not have run");
}

/// A NUL byte cannot cross the `exec` boundary: `Command` panics on it.
/// Refuse with the byte offset before anything spawns (#5529).
#[tokio::test]
async fn nul_byte_in_shell_command_is_refused_before_spawn() {
    let workspace = tempdir().expect("workspace");
    let context = ToolContext::new(workspace.path().to_path_buf());
    let marker = workspace.path().join("should-not-exist");

    let error = BashTool::new("Bash")
        .execute(
            json!({
                "command": format!("echo hi\0; touch {}", marker.display()),
            }),
            &context,
        )
        .await
        .expect_err("NUL byte must be refused");

    let message = error.to_string();
    assert!(message.contains("NUL byte"), "{message}");
    assert!(message.contains("byte offset 7"), "{message}");
    assert!(!marker.exists(), "the command must not have run");
}

/// `cwd` crosses the same boundary via `current_dir`, so the guard covers it
/// too (#5529).
#[tokio::test]
async fn nul_byte_in_shell_cwd_is_refused_before_spawn() {
    let workspace = tempdir().expect("workspace");
    let context = ToolContext::new(workspace.path().to_path_buf());

    let error = BashTool::new("Bash")
        .execute(
            json!({
                "command": "echo hi",
                "cwd": "sub\0dir",
            }),
            &context,
        )
        .await
        .expect_err("NUL byte must be refused");

    let message = error.to_string();
    assert!(message.contains("NUL byte"), "{message}");
    assert!(message.contains("cwd"), "{message}");
    assert!(message.contains("byte offset 3"), "{message}");
}

/// The same hole one type down. `and_then(as_str).unwrap_or("run")` read a
/// non-string `action` as absent and fell through to the branch that executes
/// arbitrary code, so `Bash{action: 3, command: "…"}` ran the command. `File`,
/// `Git`, `Web`, and `Run` all refuse a non-string action; the tool that runs
/// shell commands must not be the lenient one.
#[tokio::test]
async fn non_string_bash_action_is_refused_instead_of_running_the_command() {
    let workspace = tempdir().expect("workspace");
    let context = ToolContext::new(workspace.path().to_path_buf());

    for action in [json!(3), json!(true), json!(["run"]), json!({"run": true})] {
        let marker = workspace.path().join(format!("marker-{action}"));
        let error = BashTool::new("Bash")
            .execute(
                json!({
                    "action": action,
                    "command": format!("touch {}", marker.display()),
                }),
                &context,
            )
            .await
            .expect_err("a non-string action must be refused");

        let message = error.to_string();
        assert!(
            message.contains("'action'"),
            "must name the parameter: {message}"
        );
        assert!(
            message.contains("must be a string"),
            "must name the expected type: {message}"
        );
        assert!(!marker.exists(), "the command must not have run: {action}");
    }
}

/// The same hole for the data fields (2026-08-04 review). A non-string
/// `stdin` was silently dropped — the command ran with NO stdin and reported
/// success, the silent-drop failure this lane exists to close. A non-string
/// `cwd` silently ran in the workspace default. And a numeric `task_id` was
/// reported as "missing", steering the model's retry the wrong way.
#[tokio::test]
async fn wrongly_typed_stdin_cwd_and_task_id_are_refused_not_dropped() {
    let workspace = tempdir().expect("workspace");
    let context = ToolContext::new(workspace.path().to_path_buf());

    let marker = workspace.path().join("stdin-marker");
    let error = BashTool::new("Bash")
        .execute(
            json!({
                "command": format!("touch {}", marker.display()),
                "stdin": 12345,
            }),
            &context,
        )
        .await
        .expect_err("non-string stdin must be refused, never silently dropped");
    let message = error.to_string();
    assert!(message.contains("'stdin'"), "names the field: {message}");
    assert!(
        message.contains("must be a string"),
        "names the expected type: {message}"
    );
    assert!(!marker.exists(), "the command must not have run");

    let error = BashTool::new("Bash")
        .execute(json!({ "command": "pwd", "cwd": 123 }), &context)
        .await
        .expect_err("non-string cwd must be refused, never defaulted");
    assert!(error.to_string().contains("'cwd'"), "{error}");

    let error = BashTool::new("Bash")
        .execute(json!({ "action": "wait", "task_id": 42 }), &context)
        .await
        .expect_err("non-string task_id is a type error");
    let message = error.to_string();
    assert!(
        message.contains("'task_id'") && message.contains("must be a string"),
        "a supplied-but-mistyped task_id must not read as missing: {message}"
    );
}

/// `null` is the wire spelling of absence, and `action` documents a `run`
/// default — so the strictness above must not swallow the default.
#[tokio::test]
async fn absent_or_null_bash_action_still_defaults_to_run() {
    let workspace = tempdir().expect("workspace");
    let context = ToolContext::new(workspace.path().to_path_buf());

    for input in [
        json!({"command": "echo defaulted"}),
        json!({"action": null, "command": "echo defaulted"}),
    ] {
        let result = BashTool::new("Bash")
            .execute(input.clone(), &context)
            .await
            .unwrap_or_else(|err| panic!("{input} must still run: {err}"));
        assert!(result.success, "{input}: {}", result.content);
        assert!(result.content.contains("defaulted"), "{}", result.content);
    }
}

/// Negative case for the strictness above: every legitimate action still
/// dispatches to its own handler rather than the action refusal. `wait`,
/// `interact`, and `cancel` are checked by the error they raise *after*
/// dispatch (a missing/unknown task), which only their own handlers produce.
#[tokio::test]
async fn every_valid_bash_action_still_dispatches() {
    let workspace = tempdir().expect("workspace");
    let context = ToolContext::new(workspace.path().to_path_buf());
    let tool = BashTool::new("Bash");

    let ran = tool
        .execute(
            json!({"action": "run", "command": "echo dispatched"}),
            &context,
        )
        .await
        .expect("action=run must dispatch");
    assert!(ran.success, "{}", ran.content);

    for input in [
        json!({"action": "wait", "task_id": "no-such-task"}),
        json!({"action": "interact", "task_id": "no-such-task", "stdin": "y\n"}),
        json!({"action": "cancel", "task_id": "no-such-task"}),
    ] {
        let outcome = tool.execute(input.clone(), &context).await;
        let message = match outcome {
            Ok(result) => result.content,
            Err(err) => err.to_string(),
        };
        assert!(
            !message.contains("Unknown Bash action") && !message.contains("must be a string"),
            "{input} must reach its own handler, got: {message}"
        );
    }

    // `cancel` with `all` needs no task at all and must stay a success.
    let cancelled = tool
        .execute(json!({"action": "cancel", "all": true}), &context)
        .await
        .expect("action=cancel all=true must dispatch");
    assert!(cancelled.success, "{}", cancelled.content);
}

/// The stdin aliases were real but undocumented: a model that wrote `input`
/// or `data` got them honoured with nothing in the schema saying so, and a
/// maintainer reading the schema would have removed them as dead. Advertise
/// them, and hold every spelling to the same behavior.
#[tokio::test]
async fn every_advertised_stdin_spelling_reaches_the_command() {
    let workspace = tempdir().expect("workspace");
    let context = ToolContext::new(workspace.path().to_path_buf());
    let schema = BashTool::new("Bash").input_schema();

    // `cat` is Unix-only; the dispatcher runs PowerShell or `cmd` on Windows,
    // where it is either absent or an alias for `Get-Content`, which reads a
    // file and not stdin. Ask for this platform's echo-stdin spelling — the
    // same helper `test_write_stdin_streams_output` uses.
    let echo_stdin = echo_stdin_command();
    for spelling in ["stdin", "input", "data"] {
        assert!(
            schema["properties"][spelling].is_object(),
            "`{spelling}` is honoured at runtime and must be advertised"
        );
        let result = BashTool::new("Bash")
            .execute(
                json!({"command": echo_stdin, spelling: "PIPED_THROUGH_ALIAS\n"}),
                &context,
            )
            .await
            .unwrap_or_else(|err| panic!("`{spelling}` must deliver stdin: {err}"));
        assert!(
            result.content.contains("PIPED_THROUGH_ALIAS"),
            "`{spelling}` did not reach the command: {}",
            result.content
        );
    }

    // `id` is the same undocumented shape one parameter over.
    assert!(
        schema["properties"]["id"].is_object(),
        "`id` is accepted for `task_id` at runtime and must be advertised"
    );
    assert!(
        schema["properties"]["task_id"]["description"]
            .as_str()
            .is_some_and(|text| text.contains("`id`")),
        "task_id must name its alias"
    );
}

/// The schema declared no `required` key at all, so `Bash{}` — no command, no
/// task — was schema-valid for the tool that runs shell commands. What is
/// required is per-action, so it is spelled as root `anyOf` required groups,
/// the same shape `finance` and `apply_patch` already use.
#[test]
fn bash_schema_declares_what_each_action_requires() {
    let schema = BashTool::new("Bash").input_schema();
    let groups: Vec<Vec<String>> = schema["anyOf"]
        .as_array()
        .expect("root anyOf required groups")
        .iter()
        .map(|group| {
            group["required"]
                .as_array()
                .expect("required group")
                .iter()
                .map(|name| name.as_str().expect("required name").to_string())
                .collect()
        })
        .collect();

    for expected in [["command"], ["task_id"], ["id"], ["all"]] {
        assert!(
            groups.iter().any(|group| group.as_slice() == expected),
            "missing required group {expected:?} in {groups:?}"
        );
    }
    // A required name the same schema does not advertise would be
    // unsatisfiable: the model could not learn what to send.
    for group in &groups {
        for name in group {
            assert!(
                schema["properties"][name].is_object(),
                "`{name}` is required but not advertised"
            );
        }
    }
}

/// A root `anyOf` is not portable to every provider, so prove the fallback
/// the sanitizer promises: Responses/xAI drop root composition, and the
/// constraint has to survive as a description note rather than vanishing.
#[test]
fn bash_required_groups_survive_a_provider_that_drops_root_composition() {
    let mut schema = BashTool::new("Bash").input_schema();
    let note = codewhale_models::schema_sanitize::sanitize_for_responses(&mut schema)
        .expect("dropped required groups must be restated for the model");

    assert!(note.contains("At least one"), "{note}");
    for name in ["`command`", "`task_id`", "`id`", "`all`"] {
        assert!(note.contains(name), "note must name {name}: {note}");
    }
    assert_eq!(schema["type"], "object");
    assert!(schema.get("anyOf").is_none(), "root anyOf must be removed");
    assert!(schema["properties"]["command"].is_object());
}

/// One documented default hid three real ones: `wait` uses 30s and
/// `interact` 1s, so a model omitting `timeout_ms` on `wait` got a quarter of
/// the timeout the schema promised.
#[test]
fn timeout_ms_description_covers_every_action_default() {
    let schema = BashTool::new("Bash").input_schema();
    let description = schema["properties"]["timeout_ms"]["description"]
        .as_str()
        .expect("timeout_ms description");

    for expected in ["600000", "30000", "1000"] {
        assert!(
            description.contains(expected),
            "missing {expected}: {description}"
        );
    }
}

#[cfg(unix)]
fn authorized_persistent_service_context(workspace: &Path) -> ToolContext {
    let mut context = ToolContext::new(workspace.to_path_buf())
        .with_elevated_sandbox_policy(ExecutionSandboxPolicy::DangerFullAccess);
    context.persist_services_enabled = true;
    context.tool_authority = None;
    context.shell_policy = ShellPolicy::Full;
    context.auto_approve = true;
    context
}

#[cfg(unix)]
fn persistent_service_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(unix)]
#[tokio::test]
async fn persistent_service_requires_explicit_headless_exec_authority() {
    let workspace = tempdir().expect("workspace");
    let context = ToolContext::new(workspace.path().to_path_buf());

    let error = BashTool::new("Bash")
        .execute(
            json!({
                "action": "run",
                "command": "sleep 30",
                "background": true,
                "persist": true,
            }),
            &context,
        )
        .await
        .expect_err("ordinary tool contexts must reject ownership transfer");

    assert!(error.to_string().contains("real headless `codewhale exec`"));
}

#[cfg(unix)]
#[tokio::test]
async fn committed_persistent_service_survives_manager_drop_and_reports_identity() {
    let _guard = persistent_service_test_lock().lock().await;
    let workspace = tempdir().expect("workspace");
    let marker = workspace.path().join("persistent-service-finished");
    let context = authorized_persistent_service_context(workspace.path());

    let result = BashTool::new("Bash")
        .execute(
            json!({
                "action": "run",
                "command": format!("sleep 1; printf released > '{}'", marker.display()),
                "background": true,
                "persist": true,
            }),
            &context,
        )
        .await
        .expect("stage persistent service");
    assert!(result.success, "{result:?}");
    assert_eq!(
        result
            .metadata
            .as_ref()
            .and_then(|metadata| metadata["ownership"].as_str()),
        Some("managed_pending_exec_success")
    );
    let task_id = result.metadata.as_ref().unwrap()["task_id"]
        .as_str()
        .expect("task id")
        .to_string();

    let receipts = context
        .shell_manager
        .lock()
        .expect("shell manager")
        .commit_persistent_services()
        .expect("commit persistent service");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].task_id, task_id);
    assert_eq!(receipts[0].process_group_id, receipts[0].pid);
    assert_eq!(receipts[0].ownership, "external");

    drop(context);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        marker.exists(),
        "released service must survive Codewhale manager teardown"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn signal_cleanup_kills_staged_persistent_service_group() {
    let _guard = persistent_service_test_lock().lock().await;
    let workspace = tempdir().expect("workspace");
    let context = authorized_persistent_service_context(workspace.path());

    let result = BashTool::new("Bash")
        .execute(
            json!({
                "action": "run",
                "command": "sleep 30",
                "background": true,
                "persist": true,
            }),
            &context,
        )
        .await
        .expect("stage persistent service");
    let task_id = result.metadata.as_ref().unwrap()["task_id"]
        .as_str()
        .expect("task id")
        .to_string();
    let pid = context
        .shell_manager
        .lock()
        .expect("shell manager")
        .processes[&task_id]
        .child
        .as_ref()
        .and_then(ShellChild::process_id)
        .expect("persistent process id");

    abort_pending_persistent_process_groups_for_exit();
    let pid = i32::try_from(pid).expect("pid fits pid_t");
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        let mut status = 0;
        // SAFETY: `pid` is the direct child owned by this test's manager; the
        // nonblocking wait only reaps that exact process.
        let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if waited == pid {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "signal cleanup must kill the staged service process group"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(libc::WIFSIGNALED(status));
    assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
}

// === #5472: in-memory retention must be bounded ===

/// ~1.1 MB on stdout, fast: 30,000 lines of 37 bytes.
#[cfg(unix)]
fn chatty_command() -> String {
    "yes 0123456789abcdefghijklmnopqrstuvwxyz | head -n 30000".to_string()
}

/// The finding-1 regression: before this bound, a single uppercase `Bash` call
/// left its entire stdout resident in `ShellManager.processes` until the 1 h
/// `cleanup`, and only if the user happened to open the jobs panel.
#[cfg(unix)]
#[tokio::test]
async fn foreground_bash_releases_its_output_once_the_result_is_returned() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let result = BashTool::new("Bash")
        .execute(json!({"command": chatty_command()}), &ctx)
        .await
        .expect("run chatty foreground command");

    // The result itself is unaffected: still the same 30 KB truncation.
    assert!(
        result
            .content
            .contains("0123456789abcdefghijklmnopqrstuvwxyz")
    );

    let manager = ctx.shell_manager.lock().expect("shell manager");
    let retained = manager.retained_output_bytes_total();
    assert!(
        retained <= RAW_STREAM_SETTLED_TAIL_BYTES * 2,
        "a finished foreground call must not keep its full stdout resident: \
         {retained} bytes still held (bound {})",
        RAW_STREAM_SETTLED_TAIL_BYTES * 2
    );
}

/// Releasing memory must not rewrite history: the job panel keeps reporting how
/// much the command actually printed.
#[cfg(unix)]
#[tokio::test]
async fn released_output_still_reports_the_real_stream_length() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    BashTool::new("Bash")
        .execute(json!({"command": chatty_command()}), &ctx)
        .await
        .expect("run chatty foreground command");

    let mut manager = ctx.shell_manager.lock().expect("shell manager");
    let jobs = manager.list_jobs();
    let job = jobs.first().expect("the finished job is still listed");
    assert!(
        job.stdout_len >= 1_000_000,
        "stdout_len must stay honest after release, got {}",
        job.stdout_len
    );
    assert!(
        !job.stdout_tail.is_empty(),
        "a diagnostic tail must survive the release"
    );
}

/// A background job's bytes become a durable session artifact at drain time;
/// keeping a second copy in the manager afterwards is the pure-waste term.
#[cfg(unix)]
#[tokio::test]
async fn draining_completion_evidence_releases_the_retained_copy() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let started = BashTool::new("Bash")
        .execute(
            json!({"command": chatty_command(), "background": true}),
            &ctx,
        )
        .await
        .expect("start background");
    let task_id = started
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("task_id"))
        .and_then(Value::as_str)
        .expect("task id")
        .to_string();

    let mut manager = ctx.shell_manager.lock().expect("shell manager");
    let completed = wait_for_completed_shell(&mut manager, &task_id);
    assert_ne!(completed.status, ShellStatus::Running);

    let evidence = manager.drain_finished_jobs_with_evidence();
    assert_eq!(evidence.len(), 1);
    assert!(
        evidence[0].event.stdout_len >= 1_000_000,
        "the event still reports the full length"
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&evidence[0].artifact_bytes()).expect("evidence JSON");
    assert!(
        payload["stdout"]["content"]
            .as_str()
            .expect("stdout content")
            .len()
            >= 1_000_000,
        "the artifact carries the exact bytes; only the manager's copy is dropped"
    );

    let retained = manager.retained_output_bytes_total();
    assert!(
        retained <= RAW_STREAM_SETTLED_TAIL_BYTES * 2,
        "{retained} bytes still held after the evidence was published"
    );
}

/// Age was the only bound, and it only ran from `list_jobs()`. Hundreds of
/// finished records inside one hour were all retained.
#[test]
fn cleanup_bounds_finished_records_by_count() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());
    let seeded_count = MAX_FINISHED_SHELL_RECORDS + 40;
    for index in 0..seeded_count {
        // Give every fixture a deterministic ordering while keeping all of
        // them far younger than the age ceiling. Lower ids are older.
        manager.seed_finished_record_for_test(
            format!("record-{index}"),
            Duration::from_millis((seeded_count - index) as u64),
        );
    }
    manager.cleanup(FINISHED_SHELL_MAX_AGE);
    assert_eq!(manager.tracked_job_count(), MAX_FINISHED_SHELL_RECORDS);
    assert!(
        manager.inspect_job("record-39").is_err(),
        "the oldest overflow record must be evicted"
    );
    assert!(
        manager.inspect_job("record-40").is_ok(),
        "the first record inside the cap must survive"
    );
    assert!(
        manager
            .inspect_job(&format!("record-{}", seeded_count - 1))
            .is_ok(),
        "the newest record must survive"
    );
}

/// #5478 nit: `/jobs` reported `2m 07s` for a 12-second command, because
/// elapsed was `started_at.elapsed()` even after the job finished. A completed
/// job reports the duration it finished with.
#[test]
fn a_finished_job_reports_its_duration_not_a_growing_elapsed() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());
    let result = manager
        .execute_with_options_env(
            &echo_command("frozen-elapsed"),
            None,
            10_000,
            true,
            None,
            false,
            None,
            std::collections::HashMap::new(),
        )
        .expect("spawn");
    let task_id = result.task_id.expect("task id");
    let completed = wait_for_completed_shell(&mut manager, &task_id);
    assert_ne!(completed.status, ShellStatus::Running);

    let first = manager
        .list_jobs()
        .into_iter()
        .find(|job| job.id == task_id)
        .expect("job listed")
        .elapsed_ms;

    std::thread::sleep(Duration::from_millis(400));

    let second = manager
        .list_jobs()
        .into_iter()
        .find(|job| job.id == task_id)
        .expect("job still listed")
        .elapsed_ms;

    assert_eq!(
        first, second,
        "a finished job's elapsed must stop moving; it read {first}ms then {second}ms"
    );
    assert!(
        second < 10_000,
        "the frozen value must be the real duration, not the timeout: {second}ms"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn readonly_pipeline_preserves_arguments_and_disables_git_helpers() {
    const PROBE: &str = "CODEWHALE_TEST_READONLY_PIPELINE_SHELL";
    if std::env::var_os(PROBE).is_none() {
        // The dispatcher is process-pinned. Exercise both the supported shell
        // and the fail-closed POSIX fallback without inheriting the CI shell.
        for shell in ["/bin/bash", "/bin/sh"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tools::shell::tests::readonly_pipeline_preserves_arguments_and_disables_git_helpers",
                    "--test-threads=1",
                ])
                .env(PROBE, shell)
                .env("SHELL", shell)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "read-only pipeline probe failed ({shell})\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    }
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let sentinel = outside.path().join("secret");
    std::fs::write(&sentinel, "private-marker\n").unwrap();
    std::os::unix::fs::symlink(&sentinel, workspace.path().join("linked-secret")).unwrap();
    std::fs::write(workspace.path().join("input.txt"), "hello\n").unwrap();
    for name in ["-i", "-e", "-f", "--output=changed", "-o"] {
        std::fs::write(workspace.path().join(name), "option-shaped filename\n").unwrap();
    }
    let ctx = ToolContext::new(workspace.path())
        .with_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly);
    let tool = BashTool::new("Bash");
    for command in ["sort * | cat", "sed -n 1p * | cat", "cat * | cat"] {
        // #6675: a word-leading unquoted `*` is refused before anything runs
        // (its matches could be option-shaped names like the ones above).
        let result = tool.execute(json!({"command": command}), &ctx).await;
        match result {
            Err(error) => assert!(
                error.to_string().contains("unquoted `*`"),
                "{command}: {error}"
            ),
            Ok(result) => panic!("{command} must be refused, ran: {}", result.content),
        }
        assert_eq!(
            std::fs::read_to_string(&sentinel).unwrap(),
            "private-marker\n"
        );
        assert!(!workspace.path().join("changed").exists());
    }
    let ordinary = tool
        .execute(json!({"command": "cat input.txt | wc -l"}), &ctx)
        .await
        .unwrap();
    if std::env::var(PROBE).as_deref() == Ok("/bin/sh") {
        assert!(!ordinary.success);
        assert!(
            ordinary
                .content
                .contains("read-only pipelines and chains require bash or zsh")
        );
        return;
    }
    assert!(ordinary.success, "{}", ordinary.content);
    assert!(
        tool.execute(json!({"command": "cat linked-secret | cat"}), &ctx)
            .await
            .is_err()
    );
    let pipeline = hardened_readonly_script("git show HEAD | cat", workspace.path()).unwrap();
    assert!(pipeline.contains("--no-ext-diff"));
    assert!(pipeline.contains("--no-textconv"));
    assert!(pipeline.contains("--no-show-signature"));
}

#[tokio::test]
async fn readonly_sed_extra_options_never_mutate_files() {
    let workspace = tempdir().unwrap();
    let source = workspace.path().join("input.txt");
    std::fs::write(&source, "first\nsecond\n").unwrap();
    let ctx = ToolContext::new(workspace.path())
        .with_shell_policy(codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly);
    for command in [
        "sed -n 1p input.txt -i",
        "sed -n 1p -i.bak input.txt",
        "sed -n 1p -e 1e input.txt",
        "sed -n 1p -f script input.txt",
    ] {
        let error = BashTool::new("Bash")
            .execute(json!({"command": command}), &ctx)
            .await
            .expect_err("refused");
        assert!(
            error.to_string().contains("[shell.readonly.command]"),
            "{command}: {error}"
        );
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "first\nsecond\n");
    }
    let result = BashTool::new("Bash")
        .execute(json!({"command": "sed -n 1p input.txt"}), &ctx)
        .await
        .unwrap();
    assert!(result.success, "{}", result.content);
}

/// A transiently busy Work-graph must not veto the command.
///
/// `register_operation` acquires the To-do/Plan locks with a short try-lock
/// spin. Before this guard existed, a shell call landing in that window failed
/// outright with "To-do state is busy; operation was not registered" — observed
/// twice in one live session, each time right after another tool call. The
/// registration is the same bookkeeping whose `observe` half is already
/// best-effort, so a busy state now degrades to an unbound run.
#[tokio::test]
async fn busy_work_graph_degrades_the_spawn_intent_instead_of_failing_it() {
    use crate::tools::plan::new_shared_plan_state;
    use crate::tools::todo::new_shared_todo_list;
    use crate::work_graph::new_shared_work_runtime;

    let todos = new_shared_todo_list();
    let plan = new_shared_plan_state();
    let lifecycle = || ShellWorkLifecycle {
        work: new_shared_work_runtime(todos.clone(), plan.clone()),
        session_id: "session-test".to_string(),
    };

    // Control: with the graph free, the intent binds.
    let bound = ShellSpawnIntentGuard::new(Some(lifecycle()), "shell_free", "echo hi");
    assert!(
        bound.lifecycle.is_some(),
        "a free work-graph must bind the spawn intent"
    );

    // Busy: hold the To-do lock so the try-lock spin cannot win.
    let _held = todos.lock().await;
    // The raw register call still reports the busy state — this is exactly what
    // used to propagate out of the spawn path and fail the command.
    assert!(
        lifecycle()
            .register("shell_busy_direct", "echo hi")
            .is_err(),
        "the raw register call must observe the held lock as busy"
    );
    let busy = ShellSpawnIntentGuard::new(Some(lifecycle()), "shell_busy", "echo hi");
    assert!(
        busy.lifecycle.is_none(),
        "a busy work-graph must degrade to an unbound guard, not fail the spawn"
    );
}

/// #6435: the guard went unbound, but its sibling handle still published the
/// spawn to the unregistered operation, killed the child and reported
/// "operation binding shell:<id> is not registered". A busy Work-graph must
/// let the command run end to end.
#[cfg(unix)]
#[tokio::test]
async fn busy_work_graph_still_runs_the_command() {
    use crate::tools::plan::new_shared_plan_state;
    use crate::tools::todo::new_shared_todo_list;
    use crate::work_graph::new_shared_work_runtime;

    let workspace = tempdir().expect("workspace");
    let todos = new_shared_todo_list();
    let plan = new_shared_plan_state();
    let lifecycle = ShellWorkLifecycle {
        work: new_shared_work_runtime(todos.clone(), plan.clone()),
        session_id: "session-test".to_string(),
    };
    let _held = todos.lock().await;
    let mut manager = ShellManager::new(workspace.path().to_path_buf());
    for background in [false, true] {
        let marker = format!("ran-{background}.txt");
        let result = manager
            .execute_with_options_env_for_owner_and_work(
                &format!("echo ran > {marker}"),
                None,
                10_000,
                background,
                None,
                false,
                None,
                HashMap::new(),
                None,
                "session-test".to_string(),
                None,
                None,
                Some(lifecycle.clone()),
                None,
                false,
                (1_000, 60_000),
            )
            .unwrap_or_else(|err| panic!("background={background}: {err:#}"));
        if background {
            let task_id = result.task_id.expect("background task id");
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !workspace.path().join(&marker).exists() {
                assert!(std::time::Instant::now() < deadline, "{task_id} never ran");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        assert!(
            workspace.path().join(&marker).exists(),
            "background={background}: the command ran"
        );
    }
}

#[test]
fn pty_dimensions_reject_zero_and_unbounded_grid() {
    assert_eq!(
        PtyDimensions::default(),
        PtyDimensions { rows: 24, cols: 80 }
    );
    for size in [
        PtyDimensions { rows: 0, cols: 80 },
        PtyDimensions { rows: 24, cols: 0 },
        PtyDimensions {
            rows: 1001,
            cols: 80,
        },
        PtyDimensions {
            rows: 24,
            cols: u16::MAX,
        },
    ] {
        assert!(size.validate().is_err());
    }
    assert!(
        PtyDimensions {
            rows: 1000,
            cols: 1000
        }
        .validate()
        .is_ok()
    );
}

#[test]
#[cfg(not(target_env = "ohos"))]
fn pty_stdin_preserves_bytes_and_reports_flush_failure() {
    struct FlushFailure(Arc<Mutex<Vec<u8>>>);
    impl Write for FlushFailure {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "fixture flush failure",
            ))
        }
    }
    let tmp = tempdir().unwrap();
    let mut manager = ShellManager::new(tmp.path().to_path_buf());
    manager.seed_finished_record_for_test("input-fixture", Duration::ZERO);
    let bytes = Arc::new(Mutex::new(Vec::new()));
    manager.processes.get_mut("input-fixture").unwrap().stdin =
        Some(StdinWriter::Pty(Box::new(FlushFailure(bytes.clone()))));
    let input = b"\0\xff\x1b[A\x03";
    let error = manager
        .write_stdin_bytes("input-fixture", input, false)
        .unwrap_err();
    assert!(error.to_string().contains("flush"));
    assert_eq!(bytes.lock().unwrap().as_slice(), input);
}

#[test]
#[cfg(all(unix, not(target_env = "ohos")))]
fn pty_resize_updates_live_terminal_and_rejects_finished_or_pipe_jobs() {
    let tmp = tempdir().unwrap();
    let mut manager = ShellManager::new(tmp.path().to_path_buf());
    let launched = manager
        .execute_with_options_env(
            "stty -echo; printf ready; while IFS= read -r line; do stty size; done",
            None,
            5000,
            true,
            None,
            true,
            Some(ExecutionSandboxPolicy::DangerFullAccess),
            HashMap::new(),
        )
        .unwrap();
    let id = launched.task_id.unwrap();
    assert_eq!(
        manager.job_terminal_size(&id),
        Some(PtyDimensions::default())
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let chunk = manager
            .read_output_chunk(&id, ShellOutputStream::Stdout, 0, 4096, 50)
            .unwrap();
        if chunk.bytes.windows(5).any(|bytes| bytes == b"ready") {
            break;
        }
        assert!(Instant::now() < deadline, "PTY did not become ready");
    }
    // Reopening a client after an idle minute must retain this live PTY's
    // identity and resize eligibility; output silence is normal at a prompt.
    manager.processes.get_mut(&id).unwrap().last_output_at =
        Instant::now() - QUIET_NO_OUTPUT_AFTER - Duration::from_millis(1);
    let idle = manager.inspect_job(&id).unwrap().snapshot;
    assert_eq!(idle.status, ShellStatus::Running);
    assert!(idle.stdin_available);
    assert!(!idle.stale, "an idle live PTY must remain reconnectable");
    assert!(
        idle.elapsed_since_output_ms
            .is_some_and(|ms| ms >= QUIET_NO_OUTPUT_AFTER.as_millis() as u64)
    );
    let size = PtyDimensions {
        rows: 37,
        cols: 111,
    };
    manager.resize_pty(&id, size).unwrap();
    assert_eq!(manager.job_terminal_size(&id), Some(size));
    assert!(
        manager
            .resize_pty(&id, PtyDimensions { rows: 0, cols: 1 })
            .is_err()
    );
    assert_eq!(manager.job_terminal_size(&id), Some(size));
    manager.write_stdin_bytes(&id, b"size\n", false).unwrap();
    loop {
        let chunk = manager
            .read_output_chunk(&id, ShellOutputStream::Stdout, 0, 4096, 0)
            .unwrap();
        if String::from_utf8_lossy(&chunk.bytes).contains("37 111") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "actual terminal size did not change"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    manager.kill(&id).unwrap();
    assert!(manager.resize_pty(&id, size).is_err());
    assert!(manager.processes[&id].pty_master.is_none());
    let pipe = manager
        .execute_with_options_env(
            "cat",
            None,
            5000,
            true,
            None,
            false,
            Some(ExecutionSandboxPolicy::DangerFullAccess),
            HashMap::new(),
        )
        .unwrap()
        .task_id
        .unwrap();
    assert!(manager.resize_pty(&pipe, size).is_err());
    manager.kill(&pipe).unwrap();
}

#[test]
#[cfg(all(unix, not(target_env = "ohos")))]
fn pty_raw_stdin_roundtrips_nul_and_non_utf8_bytes() {
    let tmp = tempdir().unwrap();
    let mut manager = ShellManager::new(tmp.path().to_path_buf());
    let input = b"\0\xff\x1b[A\x03\n";
    let command = format!(
        "stty raw -echo; printf ready; dd bs=1 count={} 2>/dev/null",
        input.len()
    );
    let launched = manager
        .execute_with_options_env(
            &command,
            None,
            5000,
            true,
            None,
            true,
            Some(ExecutionSandboxPolicy::DangerFullAccess),
            HashMap::new(),
        )
        .unwrap();
    let id = launched.task_id.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let chunk = manager
            .read_output_chunk(&id, ShellOutputStream::Stdout, 0, 4096, 0)
            .unwrap();
        if chunk.bytes == b"ready" {
            break;
        }
        assert!(Instant::now() < deadline, "raw PTY did not become ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    manager.write_stdin_bytes(&id, input, false).unwrap();
    loop {
        let chunk = manager
            .read_output_chunk(&id, ShellOutputStream::Stdout, 5, 4096, 0)
            .unwrap();
        if chunk.status != ShellStatus::Running {
            assert_eq!(chunk.bytes, input);
            assert_eq!(chunk.next_offset, 5 + input.len());
            break;
        }
        assert!(Instant::now() < deadline, "raw PTY did not finish");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// #6654: a `Managed` background shell must (a) survive the exit of the thread
/// that spawned it — callers such as `spawn_blocking` retire their threads —
/// and (b) die with its whole process group when the TUI process is
/// SIGKILLed, including a grandchild the shell did not `exec` (the shell
/// itself keeps waiting on it). The TUI is played by a re-exec of this test
/// binary.
#[cfg(unix)]
#[test]
fn managed_background_shell_outlives_spawning_thread_but_not_the_tui() {
    const PROBE: &str = "CODEWHALE_TEST_BG_PDEATHSIG_PIDFILE";

    #[cfg(target_os = "linux")]
    fn is_alive(pid: libc::pid_t) -> bool {
        // A killed child may linger as a zombie until its new parent reaps it
        // (a container's init may never do so).
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next())
                .is_some_and(|state| state != "Z" && state != "X"),
            Err(_) => false,
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn is_alive(pid: libc::pid_t) -> bool {
        // Orphans are reparented to launchd, which reaps them promptly.
        // SAFETY: kill(2) with signal 0 only checks for existence.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    if let Some(pid_file) = std::env::var_os(PROBE) {
        // Probe role: spawn from a short-lived thread, let the thread exit,
        // mark readiness (libtest captures stdout, so through a file), then
        // hang until SIGKILLed.
        let pid_file = PathBuf::from(pid_file);
        let workspace = pid_file.parent().unwrap().to_path_buf();
        // No `exec`: the shell forks `sleep` and waits, as `npm run dev; echo
        // done` or `a | b` would.
        let command = format!(
            "sleep 600 & echo \"$$ $!\" > '{}'; wait",
            pid_file.display()
        );
        let manager = std::thread::spawn(move || {
            let mut manager = ShellManager::new(workspace);
            let result = manager
                .execute_with_options_env(
                    &command,
                    None,
                    600_000,
                    true,
                    None,
                    false,
                    Some(ExecutionSandboxPolicy::DangerFullAccess),
                    HashMap::new(),
                )
                .expect("spawn background shell");
            assert_eq!(result.status, ShellStatus::Running);
            manager
        })
        .join()
        .expect("spawning thread");
        std::fs::write(pid_file.with_extension("ready"), b"").expect("write ready marker");
        let _keep_alive = manager;
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    let tmp = tempdir().expect("tempdir");
    let pid_file = tmp.path().join("bg.pid");
    let ready_file = pid_file.with_extension("ready");
    let mut tui = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tools::shell::tests::managed_background_shell_outlives_spawning_thread_but_not_the_tui",
            "--test-threads=1",
        ])
        .env(PROBE, &pid_file)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn probe process");
    let deadline = Instant::now() + Duration::from_secs(30);
    let pids = loop {
        let pids = if ready_file.exists() {
            std::fs::read_to_string(&pid_file).ok().and_then(|text| {
                let pids = text
                    .split_whitespace()
                    .map(|pid| pid.parse::<libc::pid_t>().ok())
                    .collect::<Option<Vec<_>>>()?;
                (pids.len() == 2).then_some(pids)
            })
        } else {
            None
        };
        if let Some(pids) = pids {
            break pids;
        }
        if Instant::now() >= deadline || tui.try_wait().ok().flatten().is_some() {
            let _ = tui.kill();
            let _ = tui.wait();
            panic!("probe process never reported the background shell pid");
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    // The spawning thread is gone by now; give a mis-armed signal time to land.
    std::thread::sleep(Duration::from_millis(500));
    let survived_thread_exit = pids.iter().all(|&pid| is_alive(pid));

    tui.kill().expect("SIGKILL the probe process");
    tui.wait().expect("reap the probe process");
    let deadline = Instant::now() + Duration::from_secs(10);
    while pids.iter().any(|&pid| is_alive(pid)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let survivors = pids
        .iter()
        .copied()
        .filter(|&pid| is_alive(pid))
        .collect::<Vec<_>>();
    for &pid in &survivors {
        // SAFETY: plain kill(2) on a pid this test started.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }

    assert!(
        survived_thread_exit,
        "the background shell died when its spawning thread exited"
    );
    assert!(
        survivors.is_empty(),
        "background processes outlived the SIGKILLed TUI: {survivors:?} of shell/grandchild {pids:?}"
    );
}

/// The auto-approved `note` tool appends to the configured notes file. A
/// committed symlink (`notes.md -> ~/.zshrc`) or a symlinked notes directory
/// must not redirect that append outside the workspace.
#[cfg(unix)]
#[tokio::test]
async fn note_tool_refuses_symlinked_targets_that_leave_the_workspace() {
    let workspace = tempdir().expect("workspace");
    let outside = tempdir().expect("outside");
    let rc = outside.path().join(".zshrc");
    std::fs::write(&rc, "# rc\n").expect("write rc");

    let linked_file = workspace.path().join("notes.md");
    std::os::unix::fs::symlink(&rc, &linked_file).expect("symlink notes file");
    let context = ToolContext::with_options(workspace.path(), false, &linked_file);
    let err = NoteTool
        .execute(json!({"content": "appended text"}), &context)
        .await
        .expect_err("a symlinked notes file must be refused");
    assert!(err.to_string().contains("symlink"), "{err}");

    let linked_dir = workspace.path().join("notes");
    std::os::unix::fs::symlink(outside.path(), &linked_dir).expect("symlink notes dir");
    let context =
        ToolContext::with_options(workspace.path(), false, linked_dir.join("sub/notes.md"));
    let err = NoteTool
        .execute(json!({"content": "appended text"}), &context)
        .await
        .expect_err("a notes dir resolving outside the workspace must be refused");
    assert!(err.to_string().contains("outside the workspace"), "{err}");

    assert_eq!(std::fs::read_to_string(&rc).expect("read rc"), "# rc\n");
    assert!(!outside.path().join("sub").exists());

    let plain = workspace.path().join("docs/notes.md");
    let context = ToolContext::with_options(workspace.path(), false, &plain);
    NoteTool
        .execute(json!({"content": "kept"}), &context)
        .await
        .expect("an ordinary in-workspace notes file is writable");
    assert!(
        std::fs::read_to_string(&plain)
            .expect("read notes")
            .contains("kept")
    );
}

#[tokio::test]
async fn a_foreground_command_past_its_wait_moves_to_the_background_alive() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let result = BashTool::new("Bash")
        .execute(
            json!({"command": "echo started; sleep 3; echo finished", "timeout_ms": 1_000}),
            &ctx,
        )
        .await
        .expect("bash");
    assert!(result.success, "{}", result.content);
    assert!(result.content.contains("not killed"), "{}", result.content);
    assert!(result.content.contains("started"), "{}", result.content);
    let task_id = result.metadata.as_ref().expect("metadata")["task_id"]
        .as_str()
        .expect("task_id")
        .to_string();
    let mut manager = ctx.shell_manager.lock().expect("shell manager lock");
    assert_eq!(
        manager.poll_status(&task_id).expect("status"),
        ShellStatus::Running
    );
    assert!(manager.processes.get(&task_id).expect("tracked").background);
}
