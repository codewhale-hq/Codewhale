use std::path::Path;

use codewhale_agent_contract::worker_profile::ShellPolicy;
use codewhale_workflow_js::ToolCallResponse;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::dsh_broker::{BrokerCall, BrokerFuture, BrokerMethods, BrokerRefusal, decode};
use super::protocol::DshService;
use crate::tools::execution_envelope::enforce_tool_denial;

const SUPPORTED: bool = cfg!(all(unix, not(target_env = "ohos")));
const DEFAULT_SESSION: &str = "term-1";
const MAX_SESSION_NAME: usize = 64;
const MAX_COMMAND_BYTES: usize = 16 * 1024;
const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_TIMEOUT_SECS: u64 = 600;
const MAX_SETTLE_MS: u64 = 60_000;
pub(super) const MAX_TERMINALS: usize = 8;
const LOG: &str = "codewhale::dsh_terminal";

static REGISTRY_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

#[derive(Deserialize)]
#[serde(tag = "method", content = "input", deny_unknown_fields)]
enum TerminalMethod {
    #[serde(rename = "list")]
    List(NoInput),
    #[serde(rename = "run")]
    Run(RunInput),
    #[serde(rename = "send")]
    Send(SendInput),
    #[serde(rename = "wait")]
    Wait(WaitInput),
    #[serde(rename = "cancel")]
    Cancel(SessionInput),
    #[serde(rename = "reset")]
    Reset(SessionInput),
}

impl BrokerMethods for TerminalMethod {
    const NAMES: &'static [&'static str] = &["list", "run", "send", "wait", "cancel", "reset"];
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunInput {
    command: String,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendInput {
    session: String,
    text: String,
    #[serde(default)]
    wait_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitInput {
    session: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionInput {
    session: String,
}

#[derive(Serialize)]
struct Terminal {
    terminal_id: String,
    session: String,
    state: String,
    running: bool,
    exit_code: Option<u32>,
    updated_at: String,
}

struct Live {
    instance: String,
    terminals: Vec<Terminal>,
}

impl Live {
    fn holds(&self, session: &str) -> bool {
        self.terminals
            .iter()
            .any(|terminal| terminal.session == session)
    }

    fn existing(&self, session: &str) -> Result<(), BrokerRefusal> {
        if self.holds(session) {
            Ok(())
        } else {
            Err(invalid(
                "this plugin has no live terminal with that name in this session; `list` reports the live ones",
            ))
        }
    }
}

pub(super) fn serve(call: BrokerCall) -> BrokerFuture {
    Box::pin(async move {
        let method = decode::<TerminalMethod>(&call)?;
        if call.context.cancel().is_cancelled() {
            return Err(refused("the terminal call was withdrawn"));
        }
        match method {
            TerminalMethod::List(NoInput {}) => {
                let live = live(&call, "terminal/wait").await?;
                tracing::info!(target: LOG, plugin = %call.owner.plugin_id, terminals = live.terminals.len(), "terminals listed");
                Ok(json!({
                    "instance": live.instance,
                    "limit": MAX_TERMINALS,
                    "terminals": live.terminals,
                }))
            }
            TerminalMethod::Run(RunInput {
                command,
                session,
                timeout_secs,
            }) => {
                let session = session_name(session.as_deref().unwrap_or(DEFAULT_SESSION))?;
                if command.trim().is_empty()
                    || command.len() > MAX_COMMAND_BYTES
                    || command.contains('\0')
                {
                    return Err(invalid(format!(
                        "a command is 1 to {MAX_COMMAND_BYTES} bytes with no NUL"
                    )));
                }
                let timeout_secs = timeout(timeout_secs)?;
                let live = live(&call, "terminal/run").await?;
                if !live.holds(session) && live.terminals.len() >= MAX_TERMINALS {
                    tracing::info!(target: LOG, plugin = %call.owner.plugin_id, "terminal limit reached");
                    return Err(refused(
                        "this plugin already holds the most terminals it may open in this session",
                    ));
                }
                let mut input =
                    json!({ "command": command, "session": owned_name(&call, session) });
                if let Some(timeout_secs) = timeout_secs {
                    input["timeout_secs"] = json!(timeout_secs);
                }
                nested(&call, "terminal/run", session, input).await
            }
            TerminalMethod::Send(SendInput {
                session,
                text,
                wait_ms,
            }) => {
                let session = session_name(&session)?;
                if text.is_empty() || text.len() > MAX_INPUT_BYTES {
                    return Err(invalid(format!(
                        "terminal input is 1 to {MAX_INPUT_BYTES} bytes"
                    )));
                }
                if wait_ms.is_some_and(|wait_ms| wait_ms > MAX_SETTLE_MS) {
                    return Err(invalid(format!("wait_ms is at most {MAX_SETTLE_MS}")));
                }
                live(&call, "terminal/send").await?.existing(session)?;
                let mut input = json!({ "session": owned_name(&call, session), "text": text });
                if let Some(wait_ms) = wait_ms {
                    input["wait_ms"] = json!(wait_ms);
                }
                nested(&call, "terminal/send", session, input).await
            }
            TerminalMethod::Wait(WaitInput {
                session,
                timeout_secs,
            }) => {
                let session = session_name(&session)?;
                let timeout_secs = timeout(timeout_secs)?;
                live(&call, "terminal/wait").await?.existing(session)?;
                let mut input = json!({ "session": owned_name(&call, session) });
                if let Some(timeout_secs) = timeout_secs {
                    input["timeout_secs"] = json!(timeout_secs);
                }
                nested(&call, "terminal/wait", session, input).await
            }
            TerminalMethod::Cancel(SessionInput { session }) => {
                let session = session_name(&session)?;
                live(&call, "terminal/cancel").await?.existing(session)?;
                let input = json!({ "session": owned_name(&call, session) });
                nested(&call, "terminal/cancel", session, input).await
            }
            TerminalMethod::Reset(SessionInput { session }) => {
                let session = session_name(&session)?;
                live(&call, "terminal/reset").await?.existing(session)?;
                let input = json!({ "session": owned_name(&call, session) });
                nested(&call, "terminal/reset", session, input).await
            }
        }
    })
}

fn invalid(message: impl Into<String>) -> BrokerRefusal {
    BrokerRefusal::InvalidInput {
        service: DshService::Terminal,
        message: message.into(),
    }
}

fn refused(message: &str) -> BrokerRefusal {
    BrokerRefusal::Refused {
        service: DshService::Terminal,
        message: message.to_string(),
    }
}

fn session_name(name: &str) -> Result<&str, BrokerRefusal> {
    let ok = !name.is_empty()
        && name.len() <= MAX_SESSION_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if ok {
        Ok(name)
    } else {
        Err(invalid(format!(
            "a terminal name is 1 to {MAX_SESSION_NAME} letters, digits, `_`, `-` or `.`"
        )))
    }
}

fn timeout(secs: Option<u64>) -> Result<Option<u64>, BrokerRefusal> {
    match secs {
        Some(secs) if !(1..=MAX_TIMEOUT_SECS).contains(&secs) => Err(invalid(format!(
            "timeout_secs is between 1 and {MAX_TIMEOUT_SECS}"
        ))),
        other => Ok(other),
    }
}

fn prefix(plugin: &str) -> String {
    format!("dsh:{plugin}:")
}

fn owned_name(call: &BrokerCall, session: &str) -> String {
    format!("{}{session}", prefix(&call.owner.plugin_id))
}

async fn live(call: &BrokerCall, tool_name: &'static str) -> Result<Live, BrokerRefusal> {
    if !SUPPORTED {
        return Err(BrokerRefusal::NotAvailable(DshService::Terminal));
    }
    let mut tool = call.context.tool().clone();
    let _ = tool.refresh_live_posture();
    let needs_full = matches!(
        tool_name,
        "terminal/run" | "terminal/send" | "terminal/reset"
    );
    if !tool.shell_policy.allows_shell()
        || (needs_full && tool.shell_policy != ShellPolicy::Full)
        || enforce_tool_denial(&tool, tool_name, &json!({})).is_err()
    {
        tracing::info!(target: LOG, plugin = %call.owner.plugin_id, tool = tool_name, "terminal refused: the caller may not use this shell tool");
        return Err(refused("terminals are not available to this caller"));
    }
    let workspace = tool.workspace.clone();
    let owner = tool.state_namespace.clone();
    let mine = prefix(&call.owner.plugin_id);
    let read = async move {
        let permit = REGISTRY_GATE.acquire().await.map_err(|_| ())?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            owned(&workspace, &owner, &mine)
        })
        .await
        .map_err(|_| ())?
        .map_err(|error| {
            tracing::warn!(target: LOG, %error, "terminal registry read failed");
        })
    };
    tokio::select! {
        biased;
        () = call.context.cancel().cancelled() => Err(refused("the terminal call was withdrawn")),
        live = read => live.map_err(|()| refused("the terminal registry could not be read")),
    }
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn owned(workspace: &Path, owner: &str, mine: &str) -> Result<Live, String> {
    use crate::tools::terminal_session;

    let terminals = terminal_session::owned_sessions(workspace, owner)?
        .into_iter()
        .filter_map(|info| {
            let session = info.name.strip_prefix(mine)?;
            session_name(session).ok()?;
            Some(Terminal {
                session: session.to_string(),
                state: match info.exit_code {
                    Some(_) => "exited".to_string(),
                    None => serde_json::to_value(info.state)
                        .ok()
                        .and_then(|state| state.as_str().map(str::to_owned))
                        .unwrap_or_default(),
                },
                running: info.exit_code.is_none(),
                exit_code: info.exit_code,
                terminal_id: info.id,
                updated_at: info.updated_at,
            })
        })
        .collect();
    Ok(Live {
        instance: terminal_session::instance_id().to_string(),
        terminals,
    })
}

#[cfg(not(all(unix, not(target_env = "ohos"))))]
fn owned(_workspace: &Path, _owner: &str, _mine: &str) -> Result<Live, String> {
    Err("terminal sessions are not supported on this platform".to_string())
}

async fn nested(
    call: &BrokerCall,
    tool: &'static str,
    session: &str,
    input: Value,
) -> Result<Value, BrokerRefusal> {
    let plugin = &call.owner.plugin_id;
    match call.context.core_call(tool, input).await {
        Ok(response) => {
            tracing::info!(target: LOG, %plugin, tool, session, ok = response.ok, "terminal call ran through the gate");
            Ok(outcome(session, response))
        }
        Err(refusal) => {
            let code = refusal.code();
            let detail = match &refusal {
                BrokerRefusal::Denied { message, .. } | BrokerRefusal::Refused { message, .. } => {
                    message.as_str()
                }
                _ => "",
            };
            tracing::info!(target: LOG, %plugin, tool, session, code, detail, "terminal call not run");
            Err(match refusal {
                BrokerRefusal::Denied { .. } => BrokerRefusal::Denied {
                    service: DshService::Terminal,
                    message: "the terminal call was denied".to_string(),
                },
                _ => refused("the terminal call was refused"),
            })
        }
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn outcome(session: &str, response: ToolCallResponse) -> Value {
    if !response.ok {
        return json!({ "ok": false, "session": session, "output": text(&response.result) });
    }
    let metadata = &response.result["metadata"];
    json!({
        "ok": true,
        "session": session,
        "output": text(&response.result["content"]),
        "truncated": !response.result["truncated"].is_null(),
        "status": metadata["status"],
        "exit_code": metadata["exit_code"],
        "cwd": metadata["cwd"],
        "terminal_id": metadata["terminal_session_id"],
        "state": metadata["terminal_state"],
        "previous_terminal_id": metadata["previous_session_id"],
    })
}

#[cfg(test)]
mod tests {
    use super::super::dsh_broker::BrokerContext;
    use super::*;
    use codewhale_config::OwnerRef;

    fn call(owner: &OwnerRef, method: &str, input: Value) -> BrokerCall {
        BrokerCall {
            owner: owner.clone(),
            session_id: "session".into(),
            service: DshService::Terminal,
            method: method.into(),
            input,
            context: BrokerContext::detached_for_test(owner, DshService::Terminal),
        }
    }

    fn owner() -> OwnerRef {
        OwnerRef {
            plugin_id: "a".into(),
            generation: 1,
            owner_token: "t".into(),
        }
    }

    #[tokio::test]
    async fn terminal_methods_decode_strictly_and_bound_every_input_before_anything_runs() {
        let owner = owner();
        for method in ["spawn", "open", "kill", "resize", "read", "exec", "core"] {
            let refusal = serve(call(&owner, method, json!({}))).await.unwrap_err();
            assert_eq!(refusal.code(), "unknown_method", "{method}");
        }
        let long_name = "n".repeat(MAX_SESSION_NAME + 1);
        let long_command = "c".repeat(MAX_COMMAND_BYTES + 1);
        let long_text = "t".repeat(MAX_INPUT_BYTES + 1);
        for (method, input) in [
            ("list", json!({ "workspace": "/" })),
            ("run", json!({})),
            ("run", json!({ "command": "" })),
            ("run", json!({ "command": "  \n" })),
            ("run", json!({ "command": "a\u{0}b" })),
            ("run", json!({ "command": long_command })),
            ("run", json!({ "command": "true", "session": "" })),
            ("run", json!({ "command": "true", "session": "../term-1" })),
            ("run", json!({ "command": "true", "session": "dsh:b:x" })),
            ("run", json!({ "command": "true", "session": long_name })),
            ("run", json!({ "command": "true", "timeout_secs": 0 })),
            ("run", json!({ "command": "true", "timeout_secs": 601 })),
            (
                "run",
                json!({ "command": "true", "sandbox_permissions": "danger" }),
            ),
            ("run", json!({ "command": "true", "name": "exec_shell" })),
            ("send", json!({ "session": "x" })),
            ("send", json!({ "session": "x", "text": "" })),
            ("send", json!({ "session": "x", "text": long_text })),
            (
                "send",
                json!({ "session": "x", "text": "y", "wait_ms": 60_001 }),
            ),
            ("wait", json!({ "session": "x", "timeout_secs": 601 })),
            ("wait", json!({})),
            ("cancel", json!({ "session": "a b" })),
            ("reset", json!({ "session": "x", "force": true })),
        ] {
            let refusal = serve(call(&owner, method, input.clone()))
                .await
                .unwrap_err();
            assert_eq!(refusal.code(), "invalid_input", "{method} {input}");
        }
    }

    #[tokio::test]
    async fn a_terminal_call_whose_cancel_has_fired_has_no_effect() {
        let owner = owner();
        let fired = call(&owner, "run", json!({ "command": "true" }));
        fired.context.cancel().cancel();
        let refusal = serve(fired).await.unwrap_err();
        assert_eq!(refusal.code(), "refused");
    }

    #[test]
    fn terminal_names_are_scoped_to_the_plugin_and_a_result_is_a_typed_projection() {
        assert_eq!(prefix("a"), "dsh:a:");
        assert!(
            "dsh:a:b:x"
                .strip_prefix(&prefix("a"))
                .is_some_and(|rest| session_name(rest).is_err())
        );
        let ran = outcome(
            "build",
            ToolCallResponse {
                ok: true,
                result: json!({
                    "content": 42,
                    "truncated": null,
                    "metadata": {
                        "status": "completed",
                        "exit_code": 0,
                        "cwd": "/w",
                        "terminal_session_id": "id-1",
                        "terminal_state": "idle",
                        "state_path": "/private/state.json",
                    },
                }),
            },
        );
        assert_eq!(
            ran,
            json!({
                "ok": true,
                "session": "build",
                "output": "42",
                "truncated": false,
                "status": "completed",
                "exit_code": 0,
                "cwd": "/w",
                "terminal_id": "id-1",
                "state": "idle",
                "previous_terminal_id": null,
            })
        );
        let failed = outcome(
            "build",
            ToolCallResponse {
                ok: false,
                result: json!("boom"),
            },
        );
        assert_eq!(
            failed,
            json!({ "ok": false, "session": "build", "output": "boom" })
        );
    }
}
