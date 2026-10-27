//! Hook-receipt family: what a configured shell hook receives and what its
//! answer does, before hook orchestration moves to the host (Phase 2).
//!
//! A case is user hook configuration (the `[[hooks.hooks]]` shape, as JSON)
//! plus one tool call. `tool_call_before` cases run the engine's admission
//! fold (`turn_loop::run_tool_call_before_hooks`, the gate every frontend
//! shares); `tool_call_after` cases build the completion context the way the
//! TUI and Runtime API do (`HookContext::with_tool_outcome`) and run
//! `HookExecutor::execute`. Each hook command records its stdin and
//! environment under `{{capture}}`; the golden pins the verdict (or the
//! observer results), the schema-1 stdin document, and the documented
//! `DEEPSEEK_*` / `CODEWHALE_*` environment contract.
//!
//! POSIX shell fixtures: Unix only. The hook session id and temp paths are
//! masked.

use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};

use codewhale_config::AppMode;

use super::golden::{self, Failures, Sandbox};
use crate::hooks::{HookContext, HookExecutor};
use crate::tools::spec::{ToolError, ToolResult};
use codewhale_config::hooks::{HookEvent, HooksConfig};

const FAMILY: &str = "hooks";

/// The environment keys `HookContext::to_env_vars` documents. Anything else a
/// hook child sees is inherited process state, not contract.
const HOOK_ENV_CONTRACT: &[&str] = &[
    "CODEWHALE_SESSION_ID",
    "CODEWHALE_TOOL_CALL_ID",
    "DEEPSEEK_ERROR",
    "DEEPSEEK_MESSAGE",
    "DEEPSEEK_MODE",
    "DEEPSEEK_MODEL",
    "DEEPSEEK_PREVIOUS_MODE",
    "DEEPSEEK_SESSION_COST",
    "DEEPSEEK_SESSION_ID",
    "DEEPSEEK_TOOL_ARGS",
    "DEEPSEEK_TOOL_CALL_ID",
    "DEEPSEEK_TOOL_EXECUTION_RECEIPT",
    "DEEPSEEK_TOOL_EXIT_CODE",
    "DEEPSEEK_TOOL_NAME",
    "DEEPSEEK_TOOL_RESULT",
    "DEEPSEEK_TOOL_STATUS",
    "DEEPSEEK_TOOL_SUCCESS",
    "DEEPSEEK_TOTAL_TOKENS",
    "DEEPSEEK_WORKSPACE",
];

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}

/// Write the capture helper a fixture command invokes as `{{capture}} <name>`:
/// it saves the hook's stdin and every contract variable that is set, as
/// NUL-separated key/value pairs (values may span lines).
fn install_capture_helper(capture: &Path) -> String {
    let script = capture.join("capture.sh");
    let keys = HOOK_ENV_CONTRACT.join(" ");
    std::fs::write(
        &script,
        format!(
            "dir=$(dirname \"$0\")\n\
             cat > \"$dir/$1.stdin\"\n\
             for key in {keys}; do\n\
             \x20 eval \"present=\\${{$key+x}}\"\n\
             \x20 if [ -n \"$present\" ]; then eval \"value=\\${{$key}}\"; printf '%s\\0%s\\0' \"$key\" \"$value\"; fi\n\
             done > \"$dir/$1.env\"\n"
        ),
    )
    .expect("write capture helper");
    format!("sh {}", shell_quote(&script))
}

fn hooks_config(case: &Value, capture_command: &str) -> HooksConfig {
    let raw = serde_json::to_string(&case["hooks"]).expect("case.hooks");
    // Substitute inside the JSON text, escaping the command for JSON.
    let quoted = serde_json::to_string(capture_command).expect("quote");
    let substituted = raw.replace("{{capture}}", &quoted[1..quoted.len() - 1]);
    let hooks: Value = serde_json::from_str(&substituted).expect("substituted hooks");
    serde_json::from_value(json!({ "hooks": hooks })).expect("case.hooks is HooksConfig")
}

fn read_capture(capture: &Path, hook: &str) -> Value {
    let stdin = std::fs::read_to_string(capture.join(format!("{hook}.stdin")));
    let env = std::fs::read(capture.join(format!("{hook}.env")));
    let (Ok(stdin), Ok(env)) = (stdin, env) else {
        return json!({ "name": hook, "ran": false });
    };
    let stdin = if stdin.trim().is_empty() {
        Value::String(String::new())
    } else {
        serde_json::from_str::<Value>(&stdin).unwrap_or(Value::String(stdin))
    };
    let fields: Vec<String> = env
        .split(|byte| *byte == 0)
        .map(|field| String::from_utf8_lossy(field).into_owned())
        .collect();
    let env: serde_json::Map<String, Value> = fields
        .as_chunks::<2>()
        .0
        .iter()
        .filter(|pair| HOOK_ENV_CONTRACT.contains(&pair[0].as_str()))
        .map(|pair| (pair[0].clone(), Value::String(pair[1].clone())))
        .collect();
    json!({ "name": hook, "ran": true, "stdin": stdin, "env": env })
}

fn tool_outcome(case: &Value) -> Result<ToolResult, ToolError> {
    let outcome = &case["tool"]["outcome"];
    if let Some(ok) = outcome.get("ok") {
        return Ok(serde_json::from_value(ok.clone()).expect("tool.outcome.ok is a ToolResult"));
    }
    let failed = &outcome["execution_failed"];
    let message = failed["message"]
        .as_str()
        .expect("execution_failed.message");
    Err(match failed.get("metadata") {
        Some(metadata) => ToolError::execution_failed_with_metadata(message, metadata.clone()),
        None => ToolError::execution_failed(message),
    })
}

fn run_case(name: &str, case: &Value, failures: &mut Failures) {
    assert!(
        case["hooks"]
            .as_array()
            .is_some_and(|hooks| !hooks.is_empty()),
        "hook case must contain a hook"
    );
    let sandbox = Sandbox::new(case);
    let capture = sandbox.workspace.join(".conformance-capture");
    std::fs::create_dir_all(&capture).expect("capture dir");
    let capture_command = install_capture_helper(&capture);
    let executor = Arc::new(HookExecutor::new(
        hooks_config(case, &capture_command),
        sandbox.workspace.clone(),
    ));
    let tool = &case["tool"];
    let tool_name = tool["name"].as_str().expect("tool.name");
    let call_id = tool["call_id"].as_str().expect("tool.call_id");

    let outcome = match case["event"].as_str().expect("case.event") {
        "tool_call_before" => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            let verdict =
                runtime.block_on(crate::core::engine::turn_loop::run_tool_call_before_hooks(
                    Some(&executor),
                    None, // This surface has no TypeScript host attachment.
                    tool_name,
                    call_id,
                    &tool["input"],
                    AppMode::Agent,
                    &sandbox.workspace,
                    case["model"].as_str().unwrap_or("deepseek-v4-pro"),
                ));
            drop(runtime);
            match verdict {
                Ok(admitted) => json!({ "admit": {
                    "requires_approval": admitted.requires_approval,
                    "updated_input": admitted.updated_input,
                    "additional_context": admitted.additional_context,
                } }),
                Err(error) => json!({ "refuse": {
                    "kind": golden::tool_error_kind(&error),
                    "detail": error.to_string(),
                } }),
            }
        }
        "tool_call_after" => {
            let context = HookContext::new()
                .with_workspace(sandbox.workspace.clone())
                .with_session_id(executor.session_id())
                .with_tool_name(tool_name)
                .with_tool_call_id(call_id)
                .with_tool_outcome(&tool_outcome(case));
            let results = executor.execute(HookEvent::ToolCallAfter, &context);
            json!({ "observers": results.iter().map(|result| json!({
                "name": result.name,
                "success": result.success,
                "exit_code": result.exit_code,
                "background": result.background,
                "stdout": result.stdout,
                "stderr": result.stderr,
                "error": result.error,
            })).collect::<Vec<_>>() })
        }
        other => panic!("unknown hook event `{other}`"),
    };

    let hooks: Vec<Value> = case["hooks"]
        .as_array()
        .expect("case.hooks")
        .iter()
        .map(|hook| {
            read_capture(
                &capture,
                hook["name"].as_str().expect("every hook is named"),
            )
        })
        .collect();
    let mut golden_value = json!({ "outcome": outcome, "hooks": hooks });
    if !hooks.iter().any(|hook| hook["ran"] == true) {
        failures.push(name, "no configured hook produced a capture");
        return;
    }
    let mut masker = sandbox
        .masker(&[])
        .literal(executor.session_id(), "<HOOK_SESSION>");
    masker.value(&mut golden_value);
    failures.record(
        name,
        golden::check_golden(
            &golden::family_dir(FAMILY).join(format!("{name}.golden.json")),
            &golden::pretty(&golden::canonical(&golden_value)),
        ),
    );
}

#[test]
fn hook_receipts_match_goldens() {
    let names = golden::case_names(FAMILY);
    let mut failures = Failures::default();
    for name in &names {
        let case = golden::read_case(FAMILY, name);
        run_case(name, &case, &mut failures);
    }
    failures.finish(FAMILY, names.len());
}
