use std::path::{Path, PathBuf};

use codewhale_workflow_js::ToolCallResponse;
use serde::Deserialize;
use serde_json::{Value, json};

use super::dsh_broker::{BrokerCall, BrokerFuture, BrokerMethods, BrokerRefusal, decode};
use super::protocol::DshService;
use crate::tools::execution_envelope::enforce_tool_denial;
use crate::tools::spec::{ToolContext, resolve_strict_authority_path};

const MAX_PATH_BYTES: usize = 512;
const MAX_WRITE_BYTES: usize = 1024 * 1024;
const MAX_READ_LINES: u64 = 10_000;
const MAX_ARGS: usize = 8;
const MAX_ARG_BYTES: usize = 1024;
const PROTECTED: &[&str] = &[".git", ".codewhale"];
const LOG: &str = "codewhale::dsh_workspace";

static CONFINE_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

#[derive(Deserialize)]
#[serde(tag = "method", content = "input", deny_unknown_fields)]
enum WorkspaceMethod {
    #[serde(rename = "read")]
    Read(ReadInput),
    #[serde(rename = "list")]
    List(ListInput),
    #[serde(rename = "write")]
    Write(WriteInput),
    #[serde(rename = "exec")]
    Exec(ExecInput),
}

impl BrokerMethods for WorkspaceMethod {
    const NAMES: &'static [&'static str] = &["read", "list", "write", "exec"];
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    path: String,
    #[serde(default)]
    start_line: Option<u64>,
    #[serde(default)]
    max_lines: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListInput {
    #[serde(default)]
    path: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteInput {
    path: String,
    content: String,
    #[serde(default)]
    expected_hash: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecInput {
    argv: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    Read,
    Overlay,
}

pub(super) fn serve(call: BrokerCall) -> BrokerFuture {
    Box::pin(async move {
        let method = decode::<WorkspaceMethod>(&call)?;
        if call.context.cancel().is_cancelled() {
            return Err(refused("the workspace call was withdrawn"));
        }
        match method {
            WorkspaceMethod::Read(ReadInput {
                path,
                start_line,
                max_lines,
            }) => {
                if start_line == Some(0) {
                    return Err(invalid("start_line starts at 1"));
                }
                if max_lines.is_some_and(|lines| !(1..=MAX_READ_LINES).contains(&lines)) {
                    return Err(invalid(format!(
                        "max_lines is between 1 and {MAX_READ_LINES}"
                    )));
                }
                let tool = caller(&call, "read_file")?;
                let (shown, resolved) = confine(&call, &tool, &path, Access::Read).await?;
                let mut input = json!({ "path": resolved });
                if let Some(start_line) = start_line {
                    input["start_line"] = json!(start_line);
                }
                if let Some(max_lines) = max_lines {
                    input["max_lines"] = json!(max_lines);
                }
                nested(&call, "read_file", input)
                    .await
                    .map(|response| file_outcome(&shown, response))
            }
            WorkspaceMethod::List(ListInput { path }) => {
                let tool = caller(&call, "list_dir")?;
                let raw = path.as_deref().unwrap_or(".");
                let (shown, resolved) = confine(&call, &tool, raw, Access::Read).await?;
                nested(&call, "list_dir", json!({ "path": resolved }))
                    .await
                    .map(|response| file_outcome(&shown, response))
            }
            WorkspaceMethod::Write(WriteInput {
                path,
                content,
                expected_hash,
            }) => {
                if content.len() > MAX_WRITE_BYTES {
                    return Err(invalid(format!(
                        "a write is at most {MAX_WRITE_BYTES} bytes"
                    )));
                }
                if expected_hash
                    .as_deref()
                    .is_some_and(|hash| hash.is_empty() || hash.len() > 128)
                {
                    return Err(invalid("expected_hash is 1 to 128 characters"));
                }
                if !overlay_granted() {
                    tracing::info!(target: LOG, plugin = %call.owner.plugin_id, "workspace write refused: no reviewed overlay grant");
                    return Err(refused("this plugin may not write to the workspace"));
                }
                let tool = caller(&call, "write_file")?;
                let (shown, resolved) = confine(&call, &tool, &path, Access::Overlay).await?;
                let mut input = json!({ "path": resolved, "content": content });
                if let Some(expected_hash) = expected_hash {
                    input["expected_hash"] = json!(expected_hash);
                }
                nested(&call, "write_file", input)
                    .await
                    .map(|response| file_outcome(&shown, response))
            }
            WorkspaceMethod::Exec(ExecInput { argv, cwd }) => {
                let query = literal_git(&argv)?;
                let tool = caller(&call, "Git")?;
                if let Some(cwd) = cwd.as_deref() {
                    let (shown, _) = confine(&call, &tool, cwd, Access::Read).await?;
                    if shown != "." {
                        tracing::info!(target: LOG, plugin = %call.owner.plugin_id, "workspace exec refused: cwd is not the workspace root");
                        return Err(refused("git runs only at the workspace root"));
                    }
                }
                let mut input = query;
                input["action"] = json!("review_scope");
                nested(&call, "Git", input).await.map(exec_outcome)
            }
        }
    })
}

fn invalid(message: impl Into<String>) -> BrokerRefusal {
    BrokerRefusal::InvalidInput {
        service: DshService::Workspace,
        message: message.into(),
    }
}

fn refused(message: &str) -> BrokerRefusal {
    BrokerRefusal::Refused {
        service: DshService::Workspace,
        message: message.to_string(),
    }
}

#[cfg(test)]
thread_local! {
    static TEST_OVERLAY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(super) struct TestOverlay(bool);

#[cfg(test)]
impl TestOverlay {
    pub(super) fn granted() -> Self {
        Self(TEST_OVERLAY.with(|overlay| overlay.replace(true)))
    }
}

#[cfg(test)]
impl Drop for TestOverlay {
    fn drop(&mut self) {
        TEST_OVERLAY.with(|overlay| overlay.set(self.0));
    }
}

fn overlay_granted() -> bool {
    #[cfg(test)]
    {
        TEST_OVERLAY.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        false
    }
}

fn caller(call: &BrokerCall, tool_name: &'static str) -> Result<ToolContext, BrokerRefusal> {
    let mut tool = call.context.tool().clone();
    let _ = tool.refresh_live_posture();
    if enforce_tool_denial(&tool, tool_name, &json!({})).is_err() {
        tracing::info!(target: LOG, plugin = %call.owner.plugin_id, tool = tool_name, "workspace refused: the caller may not use this tool");
        return Err(refused(
            "this workspace call is not available to this caller",
        ));
    }
    Ok(tool)
}

fn relative(workspace: &Path, raw: &str) -> Option<String> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Some(raw.to_string());
    }
    let canonical = workspace.canonicalize().ok();
    let rest = path.strip_prefix(workspace).ok().or_else(|| {
        canonical
            .as_deref()
            .and_then(|root| path.strip_prefix(root).ok())
    })?;
    let rest = rest.to_str()?;
    Some(if rest.is_empty() {
        ".".into()
    } else {
        rest.into()
    })
}

fn shape(raw: &str) -> Result<(), BrokerRefusal> {
    if raw.is_empty()
        || raw.len() > MAX_PATH_BYTES
        || raw.trim() != raw
        || raw.chars().any(char::is_control)
    {
        return Err(invalid(format!(
            "a path is 1 to {MAX_PATH_BYTES} bytes on one line with no surrounding spaces"
        )));
    }
    Ok(())
}

fn protected(relative: &str) -> bool {
    relative.replace('\\', "/").split('/').any(|segment| {
        PROTECTED
            .iter()
            .any(|name| segment.eq_ignore_ascii_case(name))
    })
}

fn resolve(tool: &ToolContext, raw: &str, access: Access) -> Result<(String, PathBuf), String> {
    let relative = relative(&tool.workspace, raw).ok_or_else(|| "outside".to_string())?;
    let resolved = resolve_strict_authority_path(tool, &relative).map_err(|e| e.to_string())?;
    let root = tool.workspace.canonicalize().map_err(|e| e.to_string())?;
    let shown = resolved
        .strip_prefix(&root)
        .map_err(|_| "outside".to_string())?
        .to_str()
        .ok_or_else(|| "not UTF-8".to_string())?
        .replace('\\', "/");
    let shown = if shown.is_empty() { ".".into() } else { shown };
    if access == Access::Overlay && (shown == "." || protected(&shown)) {
        return Err(format!("overlay excludes {shown}"));
    }
    Ok((shown, resolved))
}

async fn confine(
    call: &BrokerCall,
    tool: &ToolContext,
    raw: &str,
    access: Access,
) -> Result<(String, String), BrokerRefusal> {
    shape(raw)?;
    let owned = tool.clone();
    let raw_owned = raw.to_string();
    let work = async move {
        let permit = CONFINE_GATE
            .acquire()
            .await
            .map_err(|_| "closed".to_string())?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            resolve(&owned, &raw_owned, access)
        })
        .await
        .map_err(|error| error.to_string())?
    };
    let resolved = tokio::select! {
        biased;
        () = call.context.cancel().cancelled() => return Err(refused("the workspace call was withdrawn")),
        resolved = work => resolved,
    };
    match resolved {
        Ok((shown, path)) => {
            let path = path
                .to_str()
                .ok_or_else(|| refused("that path is not available to this plugin"))?
                .to_string();
            Ok((shown, path))
        }
        Err(detail) => {
            tracing::info!(target: LOG, plugin = %call.owner.plugin_id, %detail, "workspace path refused");
            Err(refused("that path is not available to this plugin"))
        }
    }
}

fn literal_git(argv: &[String]) -> Result<Value, BrokerRefusal> {
    if argv.is_empty()
        || argv.len() > MAX_ARGS + 1
        || argv
            .iter()
            .any(|arg| arg.len() > MAX_ARG_BYTES || arg.chars().any(char::is_control))
    {
        return Err(invalid(format!(
            "argv is 1 to {} strings of at most {MAX_ARG_BYTES} bytes with no control characters",
            MAX_ARGS + 1
        )));
    }
    let refuse = || {
        tracing::info!(target: LOG, program = %argv[0].chars().take(64).collect::<String>(), "workspace exec refused: not a literal read-only git command");
        refused("only read-only git metadata commands run for plugins")
    };
    if argv[0] != "git" {
        return Err(refuse());
    }
    let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    let args = match args.split_first() {
        Some((&"--no-optional-locks", rest)) => rest,
        _ => &args[..],
    };
    let query = match args {
        ["for-each-ref", "--format=%(refname:short)", "refs/heads"] => {
            json!({ "query": "branches" })
        }
        ["log", "-n", "100", "--format=%H%x09%s"] => json!({ "query": "commits" }),
        ["rev-parse", "--show-toplevel"] => json!({ "query": "root" }),
        ["rev-parse", "--verify", "--end-of-options", rev] => {
            json!({ "query": "resolve", "rev": rev })
        }
        ["rev-list", "--left-right", "--count", range] => match range.split_once("...") {
            Some((ours, theirs)) if !theirs.contains("...") => {
                json!({ "query": "ahead_behind", "ours": ours, "theirs": theirs })
            }
            _ => return Err(refuse()),
        },
        ["merge-base", ours, theirs] => {
            json!({ "query": "merge_base", "ours": ours, "theirs": theirs })
        }
        _ => return Err(refuse()),
    };
    Ok(query)
}

async fn nested(
    call: &BrokerCall,
    tool: &'static str,
    input: Value,
) -> Result<ToolCallResponse, BrokerRefusal> {
    let plugin = &call.owner.plugin_id;
    match call.context.core_call(tool, input).await {
        Ok(response) => {
            tracing::info!(target: LOG, %plugin, tool, ok = response.ok, "workspace call ran through the gate");
            Ok(response)
        }
        Err(refusal) => {
            let code = refusal.code();
            let detail = match &refusal {
                BrokerRefusal::Denied { message, .. } | BrokerRefusal::Refused { message, .. } => {
                    message.as_str()
                }
                _ => "",
            };
            tracing::info!(target: LOG, %plugin, tool, code, detail, "workspace call not run");
            Err(match refusal {
                BrokerRefusal::Denied { .. } => BrokerRefusal::Denied {
                    service: DshService::Workspace,
                    message: "the workspace call was denied".to_string(),
                },
                _ => refused("the workspace call was refused"),
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

fn file_outcome(path: &str, response: ToolCallResponse) -> Value {
    if !response.ok {
        return json!({ "ok": false, "path": path, "output": text(&response.result) });
    }
    json!({
        "ok": true,
        "path": path,
        "output": text(&response.result["content"]),
        "truncated": !response.result["truncated"].is_null(),
    })
}

fn exec_outcome(response: ToolCallResponse) -> Value {
    if !response.ok {
        return json!({ "ok": false, "stdout": "", "stderr": text(&response.result), "exit_code": null });
    }
    let parsed = serde_json::from_str::<Value>(&text(&response.result["content"]))
        .ok()
        .filter(|value| value["stdout"].is_string() && value["stderr"].is_string());
    match parsed {
        Some(value) => json!({
            "ok": value["exit_code"] == json!(0),
            "stdout": value["stdout"],
            "stderr": value["stderr"],
            "exit_code": value["exit_code"],
        }),
        None => json!({
            "ok": false,
            "stdout": "",
            "stderr": "git output exceeded what a plugin call returns",
            "exit_code": null,
        }),
    }
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
            service: DshService::Workspace,
            method: method.into(),
            input,
            context: BrokerContext::detached_for_test(owner, DshService::Workspace),
        }
    }

    fn owner() -> OwnerRef {
        OwnerRef {
            plugin_id: "a".into(),
            generation: 1,
            owner_token: "t".into(),
        }
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_string()).collect()
    }

    #[tokio::test]
    async fn workspace_methods_decode_strictly_and_bound_every_input_before_anything_runs() {
        let owner = owner();
        for method in ["delete", "rename", "spawn", "shell", "watch", "core", "Git"] {
            let refusal = serve(call(&owner, method, json!({}))).await.unwrap_err();
            assert_eq!(refusal.code(), "unknown_method", "{method}");
        }
        let long_path = "p".repeat(MAX_PATH_BYTES + 1);
        let long_content = "c".repeat(MAX_WRITE_BYTES + 1);
        for (method, input) in [
            ("read", json!({})),
            ("read", json!({ "path": "" })),
            ("read", json!({ "path": " x" })),
            ("read", json!({ "path": "a\nb" })),
            ("read", json!({ "path": long_path })),
            ("read", json!({ "path": "x", "start_line": 0 })),
            ("read", json!({ "path": "x", "max_lines": 0 })),
            (
                "read",
                json!({ "path": "x", "max_lines": MAX_READ_LINES + 1 }),
            ),
            ("read", json!({ "path": "x", "follow_symlinks": true })),
            ("list", json!({ "path": "x", "recursive": true })),
            ("write", json!({ "path": "x" })),
            ("write", json!({ "path": "x", "content": long_content })),
            (
                "write",
                json!({ "path": "x", "content": "", "expected_hash": "" }),
            ),
            ("write", json!({ "path": "x", "content": "", "mode": 493 })),
            ("exec", json!({})),
            ("exec", json!({ "argv": [] })),
            ("exec", json!({ "argv": ["git", "a\u{0}b"] })),
            ("exec", json!({ "argv": ["git"], "env": {} })),
            ("exec", json!({ "argv": ["git"], "shell": true })),
        ] {
            let refusal = serve(call(&owner, method, input.clone()))
                .await
                .unwrap_err();
            assert_eq!(refusal.code(), "invalid_input", "{method} {input}");
        }
    }

    #[tokio::test]
    async fn a_workspace_call_whose_cancel_has_fired_has_no_effect() {
        let owner = owner();
        let _overlay = TestOverlay::granted();
        for (method, input) in [
            ("read", json!({ "path": "x" })),
            ("list", json!({})),
            ("write", json!({ "path": "x", "content": "y" })),
            (
                "exec",
                json!({ "argv": ["git", "rev-parse", "--show-toplevel"] }),
            ),
        ] {
            let fired = call(&owner, method, input);
            fired.context.cancel().cancel();
            let refusal = serve(fired).await.unwrap_err();
            assert_eq!(refusal.code(), "refused", "{method}");
        }
    }

    #[tokio::test]
    async fn a_write_without_the_reviewed_overlay_grant_is_refused_before_any_path_is_read() {
        let owner = owner();
        let refusal = serve(call(
            &owner,
            "write",
            json!({ "path": "x", "content": "y" }),
        ))
        .await
        .unwrap_err();
        assert_eq!(refusal.code(), "refused");
        assert_eq!(
            refusal.wire().message,
            "this plugin may not write to the workspace"
        );
    }

    #[test]
    fn only_literal_read_only_git_metadata_argv_maps_to_the_git_tool() {
        for (args, expected) in [
            (
                argv(&["git", "rev-parse", "--show-toplevel"]),
                json!({ "query": "root" }),
            ),
            (
                argv(&[
                    "git",
                    "--no-optional-locks",
                    "for-each-ref",
                    "--format=%(refname:short)",
                    "refs/heads",
                ]),
                json!({ "query": "branches" }),
            ),
            (
                argv(&["git", "log", "-n", "100", "--format=%H%x09%s"]),
                json!({ "query": "commits" }),
            ),
            (
                argv(&["git", "rev-parse", "--verify", "--end-of-options", "main"]),
                json!({ "query": "resolve", "rev": "main" }),
            ),
            (
                argv(&["git", "rev-list", "--left-right", "--count", "a...b"]),
                json!({ "query": "ahead_behind", "ours": "a", "theirs": "b" }),
            ),
            (
                argv(&["git", "merge-base", "a", "b"]),
                json!({ "query": "merge_base", "ours": "a", "theirs": "b" }),
            ),
        ] {
            assert_eq!(literal_git(&args).unwrap(), expected, "{args:?}");
        }
        for args in [
            argv(&["sh", "-c", "git status"]),
            argv(&["/usr/bin/git", "rev-parse", "--show-toplevel"]),
            argv(&["git"]),
            argv(&["git", "status"]),
            argv(&["git", "rev-parse", "--show-toplevel", "--git-dir"]),
            argv(&[
                "git",
                "-c",
                "core.fsmonitor=x",
                "rev-parse",
                "--show-toplevel",
            ]),
            argv(&[
                "git",
                "--no-optional-locks",
                "--no-optional-locks",
                "rev-parse",
                "--show-toplevel",
            ]),
            argv(&["git", "log", "-n", "100", "--format=%H%x09%s", "-p"]),
            argv(&["git", "rev-list", "--left-right", "--count", "a..b"]),
            argv(&["git", "rev-list", "--left-right", "--count", "a...b...c"]),
            argv(&["git", "push", "origin", "main"]),
            argv(&["git", "config", "core.hooksPath", "/tmp"]),
        ] {
            assert_eq!(
                literal_git(&args).unwrap_err().code(),
                "refused",
                "{args:?}"
            );
        }
        let too_many = vec!["git".to_string(); MAX_ARGS + 2];
        assert_eq!(literal_git(&too_many).unwrap_err().code(), "invalid_input");
    }

    #[test]
    fn paths_are_confined_to_the_workspace_and_the_overlay_excludes_its_internals() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.txt"), "a").unwrap();
        let tool = ToolContext::new(&root);
        let read = |raw: &str| resolve(&tool, raw, Access::Read).map(|(shown, _)| shown);
        let write = |raw: &str| resolve(&tool, raw, Access::Overlay).map(|(shown, _)| shown);

        assert_eq!(read("src/a.txt").unwrap(), "src/a.txt");
        assert_eq!(read("./src//a.txt").unwrap(), "src/a.txt");
        assert_eq!(
            read(root.join("src/a.txt").to_str().unwrap()).unwrap(),
            "src/a.txt"
        );
        assert_eq!(read(root.to_str().unwrap()).unwrap(), ".");
        assert_eq!(read(".").unwrap(), ".");
        assert_eq!(write("src/new/b.txt").unwrap(), "src/new/b.txt");

        let outside = tempfile::tempdir().unwrap();
        for raw in [
            "../x",
            "src/../../x",
            outside.path().to_str().unwrap(),
            "/etc/passwd",
        ] {
            assert!(read(raw).is_err(), "{raw}");
            assert!(write(raw).is_err(), "{raw}");
        }
        for raw in [
            ".",
            ".git/config",
            ".GIT/hooks/pre-commit",
            "src/.git",
            ".codewhale/x",
        ] {
            assert!(write(raw).is_err(), "{raw}");
        }
        assert_eq!(read(".git/HEAD").unwrap(), ".git/HEAD");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_anywhere_on_the_path_is_refused_even_when_it_points_inside() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.txt"), "a").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("out")).unwrap();
        std::os::unix::fs::symlink(root.join("src"), root.join("in")).unwrap();
        std::os::unix::fs::symlink(root.join("src/a.txt"), root.join("link.txt")).unwrap();
        let tool = ToolContext::new(&root);
        for raw in ["out/secret", "out", "in/a.txt", "link.txt", "out/new.txt"] {
            assert!(resolve(&tool, raw, Access::Read).is_err(), "{raw}");
            assert!(resolve(&tool, raw, Access::Overlay).is_err(), "{raw}");
        }
    }

    #[test]
    fn results_are_typed_projections() {
        let read = file_outcome(
            "src/a.txt",
            ToolCallResponse {
                ok: true,
                result: json!({ "content": "1: a", "truncated": null, "metadata": { "path": "/private" } }),
            },
        );
        assert_eq!(
            read,
            json!({ "ok": true, "path": "src/a.txt", "output": "1: a", "truncated": false })
        );
        let ran = exec_outcome(ToolCallResponse {
            ok: true,
            result: json!({ "content": json!({ "stdout": "/w\n", "stderr": "", "exit_code": 0 }).to_string() }),
        });
        assert_eq!(
            ran,
            json!({ "ok": true, "stdout": "/w\n", "stderr": "", "exit_code": 0 })
        );
        let cut = exec_outcome(ToolCallResponse {
            ok: true,
            result: json!({ "content": "{\"stdout\": \"/w [output truncated" }),
        });
        assert_eq!(cut["ok"], false);
        assert_eq!(cut["exit_code"], Value::Null);
    }
}
