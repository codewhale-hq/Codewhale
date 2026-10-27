//! Long-lived Python REPL runtime.
//!
//! One Python subprocess lives for the duration of an RLM turn (or an
//! inline `repl` block sequence in the agent loop). Code blocks are sent
//! over stdin framed by `__RLM_RUN__`/`__RLM_END__` sentinels; the bootstrap
//! `exec()`s them into the same global namespace so variables, imports,
//! and even open file handles persist naturally across rounds.
//!
//! Sub-LLM helpers (`sub_query`, `sub_query_batch`, `sub_rlm`, plus legacy
//! `llm_query`, `llm_query_batched`, `rlm_query`, `rlm_query_batched`) are
//! wired through a stdin/stdout RPC protocol:
//! Python emits `__RLM_REQ_<sid>__::{json}` on stdout, Rust dispatches the
//! request and writes `__RLM_RESP_<sid>__::{json}` back on stdin. No HTTP
//! sidecar, no temp ports — the same pipes carry both control and data.
//!
//! The session id (`<sid>`) is a UUID generated per spawn, so user output
//! that happens to contain "REQ" or "FINAL" can't be confused with control
//! messages.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use uuid::Uuid;

use codewhale_dependencies::ExternalTool;
use codewhale_runtime::child_env;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Result of executing one code block.
#[derive(Debug, Clone)]
pub struct ReplRound {
    /// Stdout shown to the model as metadata next round.
    pub stdout: String,
    /// Full stdout (with sentinels stripped, but otherwise raw).
    pub full_stdout: String,
    /// Stderr from this round (if any).
    pub stderr: String,
    /// `True` if the user code raised an unhandled Python exception.
    pub has_error: bool,
    /// Captured `finalize(value, confidence=...)` payload, if any.
    pub final_value: Option<String>,
    /// Captured final value before string fallback. Structured `finalize`
    /// payloads use this so `handle_read` can expose JSON instead of a Python
    /// repr string.
    pub final_json: Option<Value>,
    /// Optional confidence supplied to `finalize(...)`.
    pub final_confidence: Option<Value>,
    /// Number of `sub_query`/`sub_rlm` RPCs the round issued.
    pub rpc_count: u32,
    /// Wall-clock duration of the round.
    pub elapsed: Duration,
}

/// One RPC request emitted by Python during a round.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcRequest {
    /// `llm_query(prompt, model=None, max_tokens=None, system=None)`
    Llm {
        prompt: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        max_tokens: Option<u32>,
        #[serde(default)]
        system: Option<String>,
    },
    /// `llm_query_batched(prompts, model=None, dependency_mode="independent")`
    LlmBatch {
        prompts: Vec<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        dependency_mode: Option<String>,
        #[serde(default)]
        safety_note: Option<String>,
    },
    /// `rlm_query(prompt, model=None)` — recursive sub-RLM (paper's `sub_RLM`).
    Rlm {
        prompt: String,
        #[serde(default)]
        model: Option<String>,
    },
    /// `rlm_query_batched(prompts, model=None, dependency_mode="independent")`
    RlmBatch {
        prompts: Vec<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        dependency_mode: Option<String>,
        #[serde(default)]
        safety_note: Option<String>,
    },
}

/// Response for one RPC request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcResponse {
    /// Single-text reply (Llm / Rlm).
    Single(SingleResp),
    /// Batch reply (LlmBatch / RlmBatch).
    Batch(BatchResp),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleResp {
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResp {
    pub results: Vec<SingleResp>,
}

/// Trait-object handle for dispatching Python RPCs back into Rust.
///
/// Each RLM turn supplies one. Implementations forward to the LLM client
/// (and recursively through the same captured Engine for `Rlm` / `RlmBatch`).
pub trait RpcDispatcher: Send + Sync {
    fn dispatch<'a>(
        &'a self,
        req: RpcRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = RpcResponse> + Send + 'a>>;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DEFAULT_STDOUT_LIMIT: usize = 8_192;
/// Longest single stdout line (protocol or plain output) held in memory. An
/// RPC request is one line, so this also bounds what a request can make Rust
/// deserialize before the bridge's own caps apply; an over-long request is
/// truncated, fails to parse, and is answered with an error.
const MAX_STDOUT_LINE_BYTES: usize = 32 * 1024 * 1024;
/// Most stdout one round retains (`full_stdout`); the rest is counted and
/// reported, never buffered.
const MAX_ROUND_STDOUT_BYTES: usize = 32 * 1024 * 1024;
const ROUND_TIMEOUT: Duration = Duration::from_secs(180);
#[cfg(not(windows))]
const SPAWN_READY_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(windows)]
const SPAWN_READY_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// PythonRuntime
// ---------------------------------------------------------------------------

/// Long-lived Python REPL.
#[derive(Debug)]
pub struct PythonRuntime {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// Per-spawn session id used in protocol sentinels.
    session_id: String,
    /// Path to the file holding `context` (kept around for cleanup).
    context_path: Option<PathBuf>,
    stdout_limit: usize,
    round_count: u64,
    started: Instant,
    round_timeout: Option<Duration>,
    /// Set while a round has not yet read its own DONE sentinel, and kept
    /// when a round ends without it (timeout, I/O failure, or a caller that
    /// dropped the round mid-flight). The protocol stream is then out of step
    /// — the interrupted round may still print — so a failed round kills the
    /// interpreter and every later round refuses instead of reading another
    /// round's output as its own.
    broken: Option<String>,
}

impl PythonRuntime {
    /// Spawn a REPL with no `context` variable and no LLM helpers wired up.
    /// Used by the agent loop for inline `repl` blocks the model emits in
    /// regular conversation.
    pub async fn new() -> Result<Self, String> {
        Self::spawn_inner(None, Some(ROUND_TIMEOUT)).await
    }

    /// Compatibility shim — older RLM code path used to pass a state file.
    /// The state file is no longer used, but the path doubles as an extra
    /// scratch location callers can rely on for cleanup symmetry.
    pub fn with_state_path(_path: PathBuf) -> Self {
        // Synchronous constructor is no longer meaningful: spawning Python
        // is async. Callers in turn.rs already use `spawn_with_context` —
        // this stub is kept only so the public surface compiles for any
        // out-of-tree user. It returns a deliberately broken runtime that
        // panics on first use, which is preferable to silently lying.
        unreachable!(
            "PythonRuntime::with_state_path is deprecated — \
             use PythonRuntime::new() or PythonRuntime::spawn_with_context()"
        )
    }

    /// Spawn a REPL with the long input preloaded from a file. Used by the
    /// RLM turn loop.
    pub async fn spawn_with_context(context_path: &Path) -> Result<Self, String> {
        Self::spawn_inner(Some(context_path), None).await
    }

    async fn spawn_inner(
        context_path: Option<&Path>,
        round_timeout: Option<Duration>,
    ) -> Result<Self, String> {
        let session_id = Uuid::new_v4().simple().to_string();
        let bootstrap = render_bootstrap(&session_id);

        let mut cmd = codewhale_dependencies::Python::tokio_command().ok_or_else(|| {
            "no Python interpreter found on PATH (tried python3, python, py -3). \
             Install Python 3 and restart codewhale."
                .to_string()
        })?;
        cmd.arg("-u")
            .arg("-c")
            .arg(&bootstrap)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let context_env = context_path
            .map(|path| {
                vec![(
                    OsString::from("RLM_CONTEXT_FILE"),
                    path.as_os_str().to_os_string(),
                )]
            })
            .unwrap_or_default();
        child_env::apply_to_tokio_command(&mut cmd, context_env);

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn Python interpreter: {e}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Python interpreter stdin pipe missing".to_string())?;
        let raw_stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Python interpreter stdout pipe missing".to_string())?;
        let stdout = BufReader::new(raw_stdout);

        let mut rt = Self {
            child,
            stdin,
            stdout,
            session_id: session_id.clone(),
            context_path: context_path.map(Path::to_path_buf),
            stdout_limit: DEFAULT_STDOUT_LIMIT,
            round_count: 0,
            started: Instant::now(),
            round_timeout,
            broken: None,
        };

        // Wait for `__RLM_READY_<sid>__` before handing control back. If
        // Python failed to start (missing module, syntax error in the
        // bootstrap, etc.), this is where we'll find out.
        let ready_sentinel = format!("__RLM_READY_{session_id}__");
        match tokio::time::timeout(SPAWN_READY_TIMEOUT, rt.read_until_ready(&ready_sentinel)).await
        {
            Ok(Ok(())) => Ok(rt),
            Ok(Err(e)) => {
                let _ = rt.child.kill().await;
                Err(format!("Python interpreter bootstrap failed: {e}"))
            }
            Err(_) => {
                let _ = rt.child.kill().await;
                Err(format!(
                    "Python interpreter bootstrap did not signal ready within {}s",
                    SPAWN_READY_TIMEOUT.as_secs()
                ))
            }
        }
    }

    async fn read_until_ready(&mut self, ready_sentinel: &str) -> Result<(), String> {
        loop {
            let line = match self.read_stdout_line_lossy().await? {
                Some(line) => line,
                None => {
                    return Err("Python interpreter closed stdout before ready signal".to_string());
                }
            };
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if trimmed == ready_sentinel {
                return Ok(());
            }
            // Pre-ready output is rare; ignore it.
        }
    }

    /// One stdout line, holding at most [`MAX_STDOUT_LINE_BYTES`] of it: the
    /// rest of an over-long line is consumed and replaced by an omission note.
    async fn read_stdout_line_lossy(&mut self) -> Result<Option<String>, String> {
        let mut buf = Vec::new();
        let mut omitted = 0usize;
        loop {
            let available = self
                .stdout
                .fill_buf()
                .await
                .map_err(|e| format!("stdout read: {e}"))?;
            if available.is_empty() {
                break;
            }
            let (take, ends_line) = match available.iter().position(|&byte| byte == b'\n') {
                Some(index) => (index + 1, true),
                None => (available.len(), false),
            };
            let keep = take.min(MAX_STDOUT_LINE_BYTES.saturating_sub(buf.len()));
            buf.extend_from_slice(&available[..keep]);
            omitted += take - keep;
            self.stdout.consume(take);
            if ends_line {
                break;
            }
        }
        if buf.is_empty() && omitted == 0 {
            return Ok(None);
        }
        let mut line = String::from_utf8_lossy(&buf).into_owned();
        if omitted > 0 {
            line.truncate(line.trim_end_matches(['\n', '\r']).len());
            line.push_str(&format!(
                "[... {omitted} bytes of this line omitted by the REPL line limit ...]\n"
            ));
        }
        Ok(Some(line))
    }

    /// Execute a Python code block with no RPC dispatcher. Used for inline
    /// `repl` blocks where `llm_query()` should fall back to a sentinel.
    pub async fn execute(&mut self, code: &str) -> Result<ReplRound, String> {
        self.run(code, None::<&dyn RpcDispatcher>).await
    }

    /// Replace the long context visible to bounded helpers without restarting
    /// the Python process. User-created variables, imports, and handles stay
    /// alive across the refresh, which is what lets the normal agent loop use
    /// one working kernel instead of rebuilding a throwaway REPL each turn.
    ///
    /// The payload travels through a temporary file rather than a generated
    /// Python string so large transcripts neither bloat the command stream nor
    /// acquire quoting semantics. The old owned file is released only after
    /// the kernel has successfully loaded the replacement.
    pub async fn replace_context(&mut self, body: &str) -> Result<(), String> {
        let path = crate::write_context_file(body)
            .map_err(|e| format!("write refreshed REPL context: {e}"))?;
        let path_literal = serde_json::to_string(&path.to_string_lossy())
            .map_err(|e| format!("encode refreshed REPL context path: {e}"))?;
        let code = format!("_replace_context_file({path_literal})");

        match self.execute(&code).await {
            Ok(round) if !round.has_error => {
                if let Some(previous) = self.context_path.replace(path) {
                    let _ = tokio::fs::remove_file(previous).await;
                }
                Ok(())
            }
            Ok(round) => {
                let _ = tokio::fs::remove_file(&path).await;
                Err(format!(
                    "refresh REPL context failed: {}{}",
                    round.stdout,
                    if round.stderr.is_empty() {
                        String::new()
                    } else {
                        format!("\nstderr: {}", round.stderr)
                    }
                ))
            }
            Err(error) => {
                let _ = tokio::fs::remove_file(&path).await;
                Err(error)
            }
        }
    }

    /// Execute a code block, dispatching any sub-LLM RPCs through `bridge`.
    ///
    /// Returns once Python emits `__RLM_DONE_<sid>__` or the round timeout
    /// elapses (whichever happens first).
    ///
    /// Known limitations (F02-07): the sentinels share stdout with the code
    /// they frame, and `_SID`/`_DONE` are ordinary Python globals, so code in
    /// this round (or a subprocess inheriting fd 1) can still print its own
    /// round's DONE line and end the round early; only another round's DONE
    /// is refused. A failed or timed-out round kills the interpreter process
    /// itself, not its process group, so a grandchild it spawned may outlive
    /// the kernel. Closing either needs an out-of-band protocol channel.
    pub async fn run<D>(&mut self, code: &str, bridge: Option<&D>) -> Result<ReplRound, String>
    where
        D: RpcDispatcher + ?Sized,
    {
        if let Some(reason) = &self.broken {
            return Err(format!(
                "Python REPL is unusable after an earlier round failed ({reason}); start a new REPL"
            ));
        }
        // Pessimistic until this round reads its own DONE: a caller that
        // drops this future mid-round leaves the stream out of step too.
        self.broken = Some("an earlier round was cancelled before it finished".to_string());
        let started = Instant::now();
        self.round_count += 1;
        let round_id = self.round_count;
        let round_tag = round_id.to_string();

        // Send the code header + body + end marker in one write.
        let header = format!("__RLM_RUN_{}__::{round_id}\n", self.session_id);
        let footer = format!("__RLM_END_{}__\n", self.session_id);
        let payload = format!("{header}{code}\n{footer}");
        self.stdin
            .write_all(payload.as_bytes())
            .await
            .map_err(|e| format!("stdin write: {e}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("stdin flush: {e}"))?;

        // Sentinels for this session.
        let req_prefix = format!("__RLM_REQ_{}__::", self.session_id);
        let final_prefix = format!("__RLM_FINAL_{}__::", self.session_id);
        let err_prefix = format!("__RLM_ERR_{}__::", self.session_id);
        let done_prefix = format!("__RLM_DONE_{}__::", self.session_id);

        let mut stdout_buf = String::new();
        let mut stdout_omitted = 0usize;
        let mut final_value: Option<String> = None;
        let mut final_json: Option<Value> = None;
        let mut final_confidence: Option<Value> = None;
        let mut had_error = false;
        let mut rpc_count: u32 = 0;
        let round_timeout = self.round_timeout;

        let read_loop = async {
            loop {
                let line = match self.read_stdout_line_lossy().await? {
                    Some(line) => line,
                    None => {
                        return Err("Python interpreter closed stdout mid-round".to_string());
                    }
                };
                let trimmed = line.trim_end_matches(['\n', '\r']);

                if let Some(rest) = trimmed.strip_prefix(&done_prefix) {
                    if rest == round_tag {
                        break;
                    }
                    // Another round's completion (or user output imitating
                    // one) never ends this round.
                    push_bounded(&mut stdout_buf, &line, &mut stdout_omitted);
                    continue;
                }
                if let Some(rest) = trimmed.strip_prefix(&final_prefix) {
                    // New sessions emit an object with value/confidence;
                    // legacy helpers emitted a JSON string.
                    match serde_json::from_str::<Value>(rest) {
                        Ok(Value::Object(map)) => {
                            let value_json = map
                                .get("value")
                                .cloned()
                                .unwrap_or(Value::String(rest.to_string()));
                            let value = value_json
                                .as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| value_json.to_string());
                            final_json = Some(value_json);
                            final_value = Some(value);
                            final_confidence = map.get("confidence").cloned();
                        }
                        Ok(Value::String(value)) => {
                            final_json = Some(Value::String(value.clone()));
                            final_value = Some(value);
                            final_confidence = None;
                        }
                        Ok(other) => {
                            final_json = Some(other.clone());
                            final_value = Some(other.to_string());
                            final_confidence = None;
                        }
                        Err(_) => {
                            final_value = Some(rest.to_string());
                            final_confidence = None;
                        }
                    }
                    continue;
                }
                if let Some(rest) = trimmed.strip_prefix(&err_prefix) {
                    let traceback =
                        serde_json::from_str::<String>(rest).unwrap_or_else(|_| rest.to_string());
                    had_error = true;
                    push_bounded(
                        &mut stdout_buf,
                        &format!("[traceback]\n{traceback}\n"),
                        &mut stdout_omitted,
                    );
                    continue;
                }
                if let Some(rest) = trimmed.strip_prefix(&req_prefix) {
                    rpc_count = rpc_count.saturating_add(1);
                    let req: RpcRequest = match serde_json::from_str(rest) {
                        Ok(r) => r,
                        Err(e) => {
                            // Send an error response so Python isn't blocked.
                            self.send_resp(&RpcResponse::Single(SingleResp {
                                text: String::new(),
                                error: Some(format!("malformed RPC: {e}")),
                            }))
                            .await?;
                            continue;
                        }
                    };
                    let resp = match bridge {
                        Some(b) => b.dispatch(req).await,
                        None => RpcResponse::Single(SingleResp {
                            text: String::new(),
                            error: Some("no LLM bridge bound to this REPL".to_string()),
                        }),
                    };
                    self.send_resp(&resp).await?;
                    continue;
                }

                push_bounded(&mut stdout_buf, &line, &mut stdout_omitted);
            }
            Ok::<_, String>(())
        };

        let outcome = match round_timeout {
            Some(round_timeout) => tokio::time::timeout(round_timeout, read_loop)
                .await
                .unwrap_or_else(|_| {
                    Err(format!(
                        "REPL round timed out after {}s",
                        round_timeout.as_secs()
                    ))
                }),
            None => read_loop.await,
        };
        if let Err(error) = outcome {
            // The interpreter may still be executing this round. Kill it now
            // (the handle is reaped on drop) rather than leave it running and
            // let its late output land in a later round.
            let _ = self.child.start_kill();
            self.broken = Some(error.clone());
            return Err(error);
        }
        self.broken = None;
        if stdout_omitted > 0 {
            stdout_buf.push_str(&format!(
                "\n[... REPL round output capped: {stdout_omitted} bytes omitted ...]\n"
            ));
        }

        let stderr = self.drain_stderr().await;
        let display = truncate_stdout(stdout_buf.trim_end_matches('\n'), self.stdout_limit);

        Ok(ReplRound {
            stdout: display,
            full_stdout: stdout_buf,
            stderr,
            has_error: had_error,
            final_value,
            final_json,
            final_confidence,
            rpc_count,
            elapsed: started.elapsed(),
        })
    }

    async fn send_resp(&mut self, resp: &RpcResponse) -> Result<(), String> {
        let body = serde_json::to_string(resp).map_err(|e| format!("encode rpc resp: {e}"))?;
        let line = format!("__RLM_RESP_{}__::{body}\n", self.session_id);
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("stdin write resp: {e}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("stdin flush resp: {e}"))?;
        Ok(())
    }

    async fn drain_stderr(&mut self) -> String {
        // We don't continuously read stderr — drain whatever's pending after
        // a round so it can show up in error reports without deadlocking
        // anything during normal operation.
        let Some(stderr) = self.child.stderr.as_mut() else {
            return String::new();
        };
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        // Best-effort read with a tight deadline; we don't want to block.
        let fut = async {
            let mut chunk = [0u8; 4096];
            loop {
                match tokio::time::timeout(Duration::from_millis(20), stderr.read(&mut chunk)).await
                {
                    Ok(Ok(0)) => break,
                    Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
                    _ => break,
                }
            }
        };
        let _ = fut.await;
        String::from_utf8_lossy(&buf).to_string()
    }

    /// True once a round failed or was dropped before reading its own DONE
    /// sentinel. Every later round refuses, so a holder that wants to keep
    /// going replaces the kernel instead of reusing it.
    pub fn is_broken(&self) -> bool {
        self.broken.is_some()
    }

    /// Total rounds executed.
    pub fn round_count(&self) -> u64 {
        self.round_count
    }

    /// Current per-round timeout policy. RLM context runs intentionally return
    /// `None` so long map-reduce jobs are not killed by the old 180s cap.
    pub fn round_timeout(&self) -> Option<Duration> {
        self.round_timeout
    }

    /// Wall-clock uptime since spawn.
    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    /// Cleanly tear down the subprocess.
    pub async fn shutdown(mut self) {
        let _ = self.stdin.shutdown().await;
        let _ = self.child.kill().await;
        if let Some(path) = self.context_path.take() {
            let _ = tokio::fs::remove_file(path).await;
        }
    }
}

impl Drop for PythonRuntime {
    fn drop(&mut self) {
        // tokio sets `kill_on_drop(true)` on the child; the context file
        // (if any) is removed on `shutdown()` — drop is best-effort.
        if let Some(path) = self.context_path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

// ---------------------------------------------------------------------------
// Bootstrap script
// ---------------------------------------------------------------------------

/// Render the Python bootstrap with session-specific sentinels baked in.
/// The sentinels include a UUID to prevent user prints from being mistaken
/// for control messages.
fn render_bootstrap(session_id: &str) -> String {
    BOOTSTRAP_TEMPLATE.replace("__SID__", session_id)
}

const BOOTSTRAP_TEMPLATE: &str = r#"
import json as _json
import os as _os
import re as _re
import sys as _sys
import traceback as _traceback

_SID = "__SID__"
_REQ = f"__RLM_REQ_{_SID}__::"
_RESP = f"__RLM_RESP_{_SID}__::"
_FINAL = f"__RLM_FINAL_{_SID}__::"
_ERR = f"__RLM_ERR_{_SID}__::"
_RUN = f"__RLM_RUN_{_SID}__::"
_END = f"__RLM_END_{_SID}__"
_DONE = f"__RLM_DONE_{_SID}__::"
_READY = f"__RLM_READY_{_SID}__"

def _rpc(req):
    _sys.stdout.write(_REQ + _json.dumps(req) + "\n")
    _sys.stdout.flush()
    line = _sys.stdin.readline()
    if not line:
        return {"error": "rust driver closed stdin"}
    if line.startswith(_RESP):
        try:
            return _json.loads(line[len(_RESP):])
        except Exception as e:
            return {"error": f"malformed rpc resp: {e}"}
    return {"error": f"unexpected protocol line: {line[:120]!r}"}

def llm_query(prompt, model=None, max_tokens=None, system=None):
    """One-shot sub-LLM call. The model arg is accepted for compatibility but ignored by Rust."""
    resp = _rpc({"type":"llm","prompt":str(prompt),"model":model,
                 "max_tokens":max_tokens,"system":system})
    if isinstance(resp, dict) and resp.get("error"):
        return f"[llm_query error: {resp['error']}]"
    if isinstance(resp, dict):
        return resp.get("text","")
    return str(resp)

def _normalize_dependency_mode(mode):
    if mode is None:
        return ""
    return str(mode).strip().lower().replace("-", "_").replace(" ", "_")

def _batch_dependency_error(helper, prompts, dependency_mode):
    mode = _normalize_dependency_mode(dependency_mode)
    if mode in ("independent", "parallel_safe", "map_reduce"):
        return None
    if mode in ("sequential", "dependent", "ordered", "chain", "serial"):
        return (
            f"[{helper}: refused parallel batch because dependency_mode={dependency_mode!r}. "
            "Use sub_query_sequence(...) or an explicit for-loop with sub_query(...) so each step can consume the previous result.]"
        )
    return (
        f"[{helper}: batch helpers require dependency_mode='independent'. "
        "Use only for independent slices/items; for A->B dependencies, global-state refactors, migrations, or rollback-sensitive work, use sub_query_sequence(...).]"
    )

def llm_query_batched(prompts, model=None, dependency_mode=None, safety_note=None):
    """Run independent sub-LLM calls concurrently. Declare dependency_mode='independent'."""
    if not isinstance(prompts, (list, tuple)):
        return ["[llm_query_batched: prompts must be a list]"]
    err = _batch_dependency_error("llm_query_batched", prompts, dependency_mode)
    if err is not None:
        return [err for _ in prompts]
    resp = _rpc({
        "type":"llm_batch",
        "prompts":[str(p) for p in prompts],
        "model":model,
        "dependency_mode":dependency_mode,
        "safety_note":safety_note,
    })
    if isinstance(resp, dict) and resp.get("error"):
        return [f"[llm_query_batched: {resp['error']}]" for _ in prompts]
    results = (resp or {}).get("results", []) if isinstance(resp, dict) else []
    if len(results) != len(prompts):
        return [f"[llm_query_batched: size mismatch ({len(results)}/{len(prompts)})]" for _ in prompts]
    out = []
    for r in results:
        if r.get("error"):
            out.append(f"[child err: {r['error']}]")
        else:
            out.append(r.get("text",""))
    return out

def _rlm_child_text(r, err_label):
    # A sub-RLM can end with a partial answer AND an error (it ran out of
    # rounds before FINAL). Keep the text and mark it incomplete; only an
    # error with no text collapses to the error marker.
    text = r.get("text") or ""
    err = r.get("error")
    if not err:
        return text
    if text.strip():
        return f"{text}\n[rlm_query incomplete: {err}]"
    return f"[{err_label}: {err}]"

def rlm_query(prompt, model=None):
    """Recursive sub-RLM. The model arg is accepted for compatibility but ignored by Rust."""
    resp = _rpc({"type":"rlm","prompt":str(prompt),"model":model})
    if isinstance(resp, dict):
        return _rlm_child_text(resp, "rlm_query error")
    return str(resp)

def rlm_query_batched(prompts, model=None, dependency_mode=None, safety_note=None):
    """Run independent recursive sub-RLMs in parallel. Declare dependency_mode='independent'."""
    if not isinstance(prompts, (list, tuple)):
        return ["[rlm_query_batched: prompts must be a list]"]
    err = _batch_dependency_error("rlm_query_batched", prompts, dependency_mode)
    if err is not None:
        return [err for _ in prompts]
    resp = _rpc({
        "type":"rlm_batch",
        "prompts":[str(p) for p in prompts],
        "model":model,
        "dependency_mode":dependency_mode,
        "safety_note":safety_note,
    })
    if isinstance(resp, dict) and resp.get("error"):
        return [f"[rlm_query_batched: {resp['error']}]" for _ in prompts]
    results = (resp or {}).get("results", []) if isinstance(resp, dict) else []
    if len(results) != len(prompts):
        return [f"[rlm_query_batched: size mismatch ({len(results)}/{len(prompts)})]" for _ in prompts]
    return [_rlm_child_text(r, "child err") for r in results]

def _slice_text(slice_value):
    if slice_value is None:
        return ""
    if isinstance(slice_value, dict):
        if "text" in slice_value:
            return str(slice_value["text"])
        return _json.dumps(slice_value, ensure_ascii=False)
    return str(slice_value)

def _prompt_with_slice(prompt, slice_value):
    text = _slice_text(slice_value)
    if not text:
        return str(prompt)
    if isinstance(slice_value, dict) and ("index" in slice_value or ("start" in slice_value and "end" in slice_value)):
        label = f"slice index={slice_value.get('index', '?')} range={slice_value.get('start', '?')}:{slice_value.get('end', '?')}"
    else:
        label = "slice"
    return f"{prompt}\n\n--- {label} ---\n{text}"

def sub_query(prompt, slice=None, timeout_secs=None, **kwargs):
    """One child LLM call, optionally scoped to a bounded slice."""
    return llm_query(_prompt_with_slice(prompt, slice))

def sub_query_batch(prompt, slices, timeout_secs=None, dependency_mode=None, safety_note=None, **kwargs):
    """Apply one prompt to many independent bounded slices concurrently."""
    if not isinstance(slices, (list, tuple)):
        return ["[sub_query_batch: slices must be a list]"]
    return llm_query_batched(
        [_prompt_with_slice(prompt, s) for s in slices],
        dependency_mode=dependency_mode,
        safety_note=safety_note,
    )

def sub_query_map(prompts, slices=None, timeout_secs=None, dependency_mode=None, safety_note=None, **kwargs):
    """Run N distinct independent prompts, optionally paired with N bounded slices."""
    if not isinstance(prompts, (list, tuple)):
        return ["[sub_query_map: prompts must be a list]"]
    if slices is None:
        return llm_query_batched(
            [str(p) for p in prompts],
            dependency_mode=dependency_mode,
            safety_note=safety_note,
        )
    if not isinstance(slices, (list, tuple)):
        return ["[sub_query_map: slices must be a list]"]
    if len(prompts) != len(slices):
        return [f"[sub_query_map: size mismatch ({len(prompts)}/{len(slices)})]" for _ in prompts]
    return llm_query_batched(
        [_prompt_with_slice(p, s) for p, s in zip(prompts, slices)],
        dependency_mode=dependency_mode,
        safety_note=safety_note,
    )

def sub_query_sequence(prompt, slices, carry_prompt=None, timeout_secs=None, **kwargs):
    """Apply one prompt to slices sequentially, feeding each result into the next step."""
    if not isinstance(slices, (list, tuple)):
        return ["[sub_query_sequence: slices must be a list]"]
    out = []
    previous = ""
    carry = str(carry_prompt or "Previous step result; treat it as required input for this step:")
    total = len(slices)
    for i, s in enumerate(slices):
        step_prompt = _prompt_with_slice(prompt, s)
        if previous:
            step_prompt = (
                f"{step_prompt}\n\n--- dependency_state step {i}/{total} ---\n"
                f"{carry}\n{previous}"
            )
        result = llm_query(step_prompt)
        out.append(result)
        previous = result
    return out

def sub_rlm(prompt, source=None, timeout_secs=None, **kwargs):
    """Recursive sub-RLM call for tasks that need their own decomposition."""
    return rlm_query(_prompt_with_slice(prompt, source))

def _json_safe(value):
    try:
        _json.dumps(value, ensure_ascii=False)
        return value
    except Exception:
        return str(value)

def _emit_final(value, confidence=None):
    safe_value = _json_safe(value)
    _sys.stdout.write(_FINAL + _json.dumps({
        "value": safe_value,
        "confidence": confidence,
    }, ensure_ascii=False) + "\n")
    _sys.stdout.flush()

def FINAL(value):
    """Legacy compatibility alias for finalize(value)."""
    _emit_final(value)

def FINAL_VAR(name):
    """Legacy compatibility alias for finalize(repl_get(name))."""
    name_str = str(name).strip().strip("'\"")
    if name_str in globals():
        _emit_final(globals()[name_str])
    else:
        print(f"FINAL_VAR error: variable '{name_str}' not found. "
              f"Use SHOW_VARS() to list available variables.", flush=True)

def SHOW_VARS():
    """Return a dict of {name: type-name} for all user variables in the REPL."""
    out = {}
    for k, v in list(globals().items()):
        if k.startswith('_') or k in _BOOTSTRAP_NAMES:
            continue
        out[k] = type(v).__name__
    return out

def repl_get(name, default=None):
    return globals().get(str(name), default)

def repl_set(name, value):
    globals()[str(name)] = value

def context_meta():
    """Return bounded metadata about the loaded input; never includes the full text."""
    text = _context
    line_count = 0 if text == "" else text.count("\n") + (0 if text.endswith("\n") else 1)
    return {
        "chars": len(text),
        "lines": line_count,
        "preview": text[:500],
        "tail_preview": text[-500:] if len(text) > 500 else text,
    }

def _slice_chars(start, end):
    total = len(_context)
    s = max(0, int(start))
    e = max(s, min(total, int(end)))
    return _context[s:e]

def _slice_lines(start, end):
    lines = _context.splitlines()
    s = max(0, int(start))
    e = max(s, min(len(lines), int(end)))
    return "\n".join(lines[s:e])

def peek(start, end, unit="chars"):
    """Return a bounded slice of the input by char offsets or line numbers."""
    if str(unit).lower() in ("line", "lines"):
        return _slice_lines(start, end)
    if str(unit).lower() not in ("char", "chars"):
        raise ValueError("unit must be 'chars' or 'lines'")
    return _slice_chars(start, end)

def search(pattern, max_hits=100):
    """Regex-search the input and return bounded hit records with snippets."""
    max_hits = max(0, int(max_hits))
    hits = []
    if max_hits == 0:
        return hits
    rx = _re.compile(str(pattern), _re.MULTILINE)
    for i, m in enumerate(rx.finditer(_context)):
        if i >= max_hits:
            break
        start, end = m.span()
        snippet_start = max(0, start - 120)
        snippet_end = min(len(_context), end + 120)
        hits.append({
            "index": i,
            "start": start,
            "end": end,
            "match": m.group(0),
            "snippet": _context[snippet_start:snippet_end],
        })
    return hits

def chunk(max_chars=20000, overlap=0):
    """Return full-coverage input chunks with index/start/end/text fields."""
    max_chars = int(max_chars)
    overlap = max(0, int(overlap))
    if max_chars <= 0:
        raise ValueError("max_chars must be > 0")
    if overlap >= max_chars:
        raise ValueError("overlap must be smaller than max_chars")
    chunks = []
    start = 0
    idx = 0
    total = len(_context)
    while start < total:
        end = min(total, start + max_chars)
        chunks.append({"index": idx, "start": start, "end": end, "text": _context[start:end]})
        idx += 1
        if end >= total:
            break
        start = end - overlap
    return chunks

def chunk_context(max_chars=20000, overlap=0):
    """Compatibility alias for chunk()."""
    return chunk(max_chars=max_chars, overlap=overlap)

def chunk_coverage(chunks):
    """Summarize coverage for chunks produced by chunk()."""
    spans = []
    for c in chunks:
        try:
            spans.append((int(c["start"]), int(c["end"])))
        except Exception:
            continue
    spans.sort()
    covered = 0
    cursor = 0
    gaps = []
    for start, end in spans:
        if start > cursor:
            gaps.append((cursor, start))
        if end > cursor:
            covered += end - max(start, cursor)
            cursor = end
    if cursor < len(_context):
        gaps.append((cursor, len(_context)))
    return {
        "chunks": len(chunks),
        "context_chars": len(_context),
        "input_chars": len(_context),
        "covered_chars": covered,
        "gaps": gaps,
        "complete": covered >= len(_context) and not gaps,
    }

def finalize(value, confidence=None):
    """Signal the session's final answer and persist confidence metadata."""
    global final_answer, final_confidence, final_result
    final_answer = _json_safe(value)
    final_confidence = confidence
    final_result = {
        "value": final_answer,
        "confidence": confidence,
    }
    _emit_final(final_answer, confidence=confidence)
    return final_answer

def evaluate_progress():
    """Return lightweight state useful before deciding the next REPL step."""
    vars_now = SHOW_VARS()
    return {
        "has_final_answer": "final_answer" in globals(),
        "final_confidence": globals().get("final_confidence", None),
        "user_variables": vars_now,
    }

# Load the long input from a file. This keeps the big string out of the
# process command-line and out of the LLM's window.
_ctx_file = _os.environ.get("RLM_CONTEXT_FILE","")
_context = ""
if _ctx_file:
    try:
        with open(_ctx_file, "r", encoding="utf-8", errors="replace") as f:
            _context = f.read()
    except Exception as e:
        _sys.stderr.write(f"[bootstrap] failed to load context: {e}\n")
content = _context

def _replace_context_file(path):
    """Atomically switch bounded helpers to a freshly written context file."""
    global _context, content
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        _context = f.read()
    content = _context
    return context_meta()

_BOOTSTRAP_NAMES = {
    "_SID","_REQ","_RESP","_FINAL","_ERR","_RUN","_END","_DONE","_READY",
    "_rpc","_ctx_file","_context","_slice_chars","_slice_lines","_replace_context_file","_BOOTSTRAP_NAMES","_main_loop",
    "_emit_final","_json_safe","_slice_text","_prompt_with_slice",
    "_normalize_dependency_mode","_batch_dependency_error",
    "llm_query","llm_query_batched","rlm_query","rlm_query_batched",
    "sub_query","sub_query_batch","sub_query_map","sub_query_sequence","sub_rlm",
    "FINAL","FINAL_VAR","SHOW_VARS","repl_get","repl_set",
    "context_meta","peek","search","chunk","chunk_context","chunk_coverage",
    "finalize","evaluate_progress","content",
    "_json","_os","_re","_sys","_traceback",
}

def _main_loop():
    _sys.stdout.write(_READY + "\n")
    _sys.stdout.flush()
    while True:
        header = _sys.stdin.readline()
        if not header:
            return
        if not header.startswith(_RUN):
            continue
        round_id = header.rstrip("\n")[len(_RUN):]
        code_lines = []
        while True:
            line = _sys.stdin.readline()
            if not line:
                return
            if line.rstrip("\n") == _END:
                break
            code_lines.append(line)
        code = "".join(code_lines)
        try:
            exec(compile(code, f"<repl-{round_id}>", "exec"), globals())
        except SystemExit:
            _sys.stdout.write(_DONE + round_id + "\n")
            _sys.stdout.flush()
            return
        except BaseException:
            tb = _traceback.format_exc()
            _sys.stdout.write(_ERR + _json.dumps(tb) + "\n")
            _sys.stdout.flush()
        _sys.stdout.write(_DONE + round_id + "\n")
        _sys.stdout.flush()

_main_loop()
"#;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Append to a round's retained stdout without exceeding
/// [`MAX_ROUND_STDOUT_BYTES`]; whatever does not fit is only counted.
fn push_bounded(buf: &mut String, text: &str, omitted: &mut usize) {
    let room = MAX_ROUND_STDOUT_BYTES.saturating_sub(buf.len());
    if text.len() <= room {
        buf.push_str(text);
        return;
    }
    let cut = (0..=room)
        .rev()
        .find(|&index| text.is_char_boundary(index))
        .unwrap_or(0);
    buf.push_str(&text[..cut]);
    *omitted += text.len() - cut;
}

fn truncate_stdout(stdout: &str, limit: usize) -> String {
    if stdout.len() <= limit {
        return stdout.to_string();
    }
    let take = limit.saturating_sub(80);
    let mut out: String = stdout.chars().take(take).collect();
    let omitted = stdout.len().saturating_sub(out.len());
    out.push_str(&format!(
        "\n\n[... REPL output truncated: {omitted} bytes omitted ...]\n"
    ));
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::sync::Mutex;

    /// In-process dispatcher that records what was asked and replies with
    /// canned text. Lets tests verify the round-trip without real network.
    struct StubBridge {
        calls: Arc<Mutex<Vec<RpcRequest>>>,
        canned: Arc<AtomicU32>,
    }

    impl StubBridge {
        fn new() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                canned: Arc::new(AtomicU32::new(0)),
            }
        }
    }

    impl RpcDispatcher for StubBridge {
        fn dispatch<'a>(
            &'a self,
            req: RpcRequest,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = RpcResponse> + Send + 'a>> {
            Box::pin(async move {
                self.calls.lock().await.push(req.clone());
                let n = self.canned.fetch_add(1, Ordering::Relaxed);
                match req {
                    RpcRequest::Llm { prompt, .. } | RpcRequest::Rlm { prompt, .. } => {
                        RpcResponse::Single(SingleResp {
                            text: format!("stub#{n}: {prompt}"),
                            error: None,
                        })
                    }
                    RpcRequest::LlmBatch { prompts, .. } | RpcRequest::RlmBatch { prompts, .. } => {
                        let results = prompts
                            .into_iter()
                            .enumerate()
                            .map(|(i, p)| SingleResp {
                                text: format!("stub#{n}.{i}: {p}"),
                                error: None,
                            })
                            .collect();
                        RpcResponse::Batch(BatchResp { results })
                    }
                }
            })
        }
    }

    fn write_temp_context(body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("deepseek_repl_runtime_tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("ctx_{}_{}.txt", std::process::id(), Uuid::new_v4()));
        std::fs::write(&path, body).unwrap();
        path
    }

    #[tokio::test]
    async fn spawns_and_executes_simple_print() {
        let mut rt = PythonRuntime::new().await.expect("spawn");
        let round = rt.execute("print('hello world')").await.expect("execute");
        assert!(round.stdout.contains("hello world"));
        assert!(!round.has_error);
        assert!(round.final_value.is_none());
        assert_eq!(round.rpc_count, 0);
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn non_utf8_stdout_decodes_lossy_and_runtime_survives() {
        let mut rt = PythonRuntime::new().await.expect("spawn");
        let round = rt
            .execute(
                "import sys\n\
                 sys.stdout.buffer.write(b'bad:\\xff\\n')\n\
                 sys.stdout.buffer.flush()\n\
                 print('after invalid')",
            )
            .await
            .expect("execute");

        assert!(round.stdout.contains("bad:\u{fffd}"), "{}", round.stdout);
        assert!(round.stdout.contains("after invalid"), "{}", round.stdout);
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn variables_persist_across_rounds() {
        let mut rt = PythonRuntime::new().await.expect("spawn");
        rt.execute("x = [1, 2, 3]").await.expect("r1");
        rt.execute("x.append(99)").await.expect("r2");
        let round = rt.execute("print(x)").await.expect("r3");
        assert!(round.stdout.contains("[1, 2, 3, 99]"));
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn imports_persist_across_rounds() {
        let mut rt = PythonRuntime::new().await.expect("spawn");
        rt.execute("import math").await.expect("r1");
        let round = rt.execute("print(math.pi)").await.expect("r2");
        assert!(round.stdout.contains("3.14"));
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn context_loads_from_file() {
        let path = write_temp_context("the quick brown fox");
        let mut rt = PythonRuntime::spawn_with_context(&path)
            .await
            .expect("spawn");
        let round = rt
            .execute("print(context_meta()['chars'], peek(0, 5))")
            .await
            .expect("execute");
        assert!(round.stdout.contains("19"));
        assert!(round.stdout.contains("the q"));
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn context_aliases_keep_common_content_name_bounded() {
        let path = write_temp_context("aleph-style");
        let mut rt = PythonRuntime::spawn_with_context(&path)
            .await
            .expect("spawn");
        let round = rt
            .execute("print(content == _context, 'context' in globals(), 'ctx' in globals())")
            .await
            .expect("execute");
        assert!(round.stdout.contains("True False False"));
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn replacing_context_keeps_kernel_variables_and_refreshes_helpers() {
        let mut rt = PythonRuntime::new().await.expect("spawn");
        rt.execute("remembered = {'answer': 42}")
            .await
            .expect("seed persistent variable");
        rt.replace_context("fresh transcript\nwith a needle")
            .await
            .expect("refresh context");

        let round = rt
            .execute(
                "print(remembered['answer'])\n\
                 print(context_meta()['chars'])\n\
                 print(search('needle')[0]['match'])",
            )
            .await
            .expect("inspect refreshed context");

        assert!(round.stdout.contains("42"), "{}", round.stdout);
        assert!(round.stdout.contains("30"), "{}", round.stdout);
        assert!(round.stdout.contains("needle"), "{}", round.stdout);
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn context_chunk_helpers_report_full_coverage() {
        let path = write_temp_context("abcdefghijklmnopqrstuvwxyz");
        let mut rt = PythonRuntime::spawn_with_context(&path)
            .await
            .expect("spawn");
        let round = rt
            .execute(
                "chunks = chunk_context(max_chars=10)\n\
                 coverage = chunk_coverage(chunks)\n\
                 print(len(chunks), coverage['covered_chars'], coverage['complete'])",
            )
            .await
            .expect("execute");
        assert!(round.stdout.contains("3 26 True"), "{}", round.stdout);
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn bounded_input_helpers_work() {
        let path = write_temp_context("alpha\nbeta needle\ngamma needle\nomega");
        let mut rt = PythonRuntime::spawn_with_context(&path)
            .await
            .expect("spawn");
        let round = rt
            .execute(
                "meta = context_meta()\n\
                 hits = search('needle', max_hits=1)\n\
                 print(meta['chars'], meta['lines'])\n\
                 print(peek(6, 17))\n\
                 print(peek(1, 3, unit='lines'))\n\
                 print(len(hits), hits[0]['match'], hits[0]['start'])",
            )
            .await
            .expect("execute");
        let stdout = round.stdout.replace("\r\n", "\n");
        assert!(stdout.contains("36 4"), "{stdout}");
        assert!(stdout.contains("beta needle"), "{stdout}");
        assert!(stdout.contains("beta needle\ngamma needle"), "{stdout}");
        assert!(stdout.contains("1 needle 11"), "{stdout}");
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn new_chunk_helper_reports_full_coverage() {
        let path = write_temp_context("abcdefghijklmnopqrstuvwxyz");
        let mut rt = PythonRuntime::spawn_with_context(&path)
            .await
            .expect("spawn");
        let round = rt
            .execute(
                "chunks = chunk(max_chars=10)\n\
                 coverage = chunk_coverage(chunks)\n\
                 print(len(chunks), coverage['input_chars'], coverage['covered_chars'], coverage['complete'])",
            )
            .await
            .expect("execute");
        assert!(round.stdout.contains("3 26 26 True"), "{}", round.stdout);
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn finalize_helper_is_captured_directly() {
        let mut rt = PythonRuntime::new().await.expect("spawn");
        let round = rt
            .execute("finalize('computed answer', confidence='high')")
            .await
            .expect("execute");
        assert_eq!(round.final_value.as_deref(), Some("computed answer"));
        assert_eq!(
            round.final_json.as_ref().and_then(Value::as_str),
            Some("computed answer")
        );
        assert_eq!(
            round.final_confidence.as_ref().and_then(Value::as_str),
            Some("high")
        );
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn finalize_preserves_json_values_for_handles() {
        let mut rt = PythonRuntime::new().await.expect("spawn");
        let round = rt
            .execute("finalize({'answer': 42, 'items': ['a', 'b']})")
            .await
            .expect("execute");

        assert_eq!(
            round.final_value.as_deref(),
            Some(r#"{"answer":42,"items":["a","b"]}"#)
        );
        assert_eq!(
            round.final_json,
            Some(serde_json::json!({"answer": 42, "items": ["a", "b"]}))
        );
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn sub_query_accepts_timeout_keyword_for_agent_guesses() {
        let bridge = StubBridge::new();
        let mut rt = PythonRuntime::new().await.expect("spawn");
        let round = rt
            .run(
                "answer = sub_query('summarize', timeout_secs=2)\nprint(answer)",
                Some(&bridge),
            )
            .await
            .expect("execute");

        assert!(!round.has_error, "{}", round.stdout);
        assert!(
            round.stdout.contains("stub#0: summarize"),
            "{}",
            round.stdout
        );
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn rlm_context_runtime_has_no_fixed_round_timeout() {
        let path = write_temp_context("long input");
        let rt = PythonRuntime::spawn_with_context(&path)
            .await
            .expect("spawn");
        assert!(
            rt.round_timeout().is_none(),
            "RLM context runs must not inherit the old 180s REPL round timeout"
        );
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn timed_out_round_kills_the_interpreter_and_refuses_later_rounds() {
        let mut rt = PythonRuntime::new().await.expect("spawn");
        rt.round_timeout = Some(Duration::from_millis(300));
        let error = rt
            .execute("import time\ntime.sleep(30)\nprint('late output')")
            .await
            .expect_err("the round must time out");
        assert!(error.contains("timed out"), "{error}");

        let refused = rt
            .execute("print('next round')")
            .await
            .expect_err("a desynchronized REPL must not run another round");
        assert!(refused.contains("unusable"), "{refused}");

        let deadline = Instant::now() + Duration::from_secs(5);
        while rt.child.try_wait().expect("child status").is_none() {
            assert!(
                Instant::now() < deadline,
                "the timed-out interpreter must be killed"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    #[tokio::test]
    async fn another_rounds_done_sentinel_does_not_end_this_round() {
        let mut rt = PythonRuntime::new().await.expect("spawn");
        let round = rt
            .execute("print(_DONE + '999')\nprint('still this round')")
            .await
            .expect("execute");
        assert!(
            round.stdout.contains("still this round"),
            "{}",
            round.stdout
        );
        let next = rt.execute("print('second')").await.expect("next round");
        assert!(next.stdout.contains("second"), "{}", next.stdout);
        assert!(!next.stdout.contains("still this round"), "{}", next.stdout);
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn inline_runtime_keeps_bounded_round_timeout() {
        let rt = PythonRuntime::new().await.expect("spawn");
        assert_eq!(rt.round_timeout(), Some(ROUND_TIMEOUT));
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn legacy_final_is_captured() {
        let mut rt = PythonRuntime::new().await.expect("spawn");
        let round = rt
            .execute("FINAL('the answer is 42')")
            .await
            .expect("execute");
        assert_eq!(round.final_value.as_deref(), Some("the answer is 42"));
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn legacy_final_var_is_captured() {
        let mut rt = PythonRuntime::new().await.expect("spawn");
        rt.execute("answer = 'computed'").await.expect("r1");
        let round = rt.execute("FINAL_VAR('answer')").await.expect("r2");
        assert_eq!(round.final_value.as_deref(), Some("computed"));
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn errors_are_reported_without_killing_runtime() {
        let mut rt = PythonRuntime::new().await.expect("spawn");
        let r1 = rt.execute("raise ValueError('boom')").await.expect("r1");
        assert!(r1.has_error);
        assert!(r1.full_stdout.contains("boom") || r1.stdout.contains("boom"));
        // The runtime is still alive — next round should work.
        let r2 = rt.execute("print('still here')").await.expect("r2");
        assert!(r2.stdout.contains("still here"));
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn rpc_dispatcher_round_trips_llm_query() {
        let bridge = StubBridge::new();
        let calls = Arc::clone(&bridge.calls);

        let mut rt = PythonRuntime::new().await.expect("spawn");
        let round = rt
            .run("print(llm_query('hello'))", Some(&bridge))
            .await
            .expect("execute");
        assert!(
            round.stdout.contains("stub#0: hello"),
            "stdout: {:?}",
            round.stdout
        );
        assert_eq!(round.rpc_count, 1);

        let recorded = calls.lock().await;
        assert_eq!(recorded.len(), 1);
        match &recorded[0] {
            RpcRequest::Llm { prompt, .. } => assert_eq!(prompt, "hello"),
            other => panic!("expected Llm request, got {other:?}"),
        }
        drop(recorded);
        rt.shutdown().await;
    }

    /// A sub-RLM that ran out of rounds returns its last answer AND an
    /// error. Answers every recursive request with that shape.
    struct ExhaustedRlmBridge;

    impl RpcDispatcher for ExhaustedRlmBridge {
        fn dispatch<'a>(
            &'a self,
            req: RpcRequest,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = RpcResponse> + Send + 'a>> {
            Box::pin(async move {
                let partial = |i: usize| SingleResp {
                    text: if i == 1 {
                        String::new()
                    } else {
                        format!("partial answer {i}")
                    },
                    error: Some("RLM loop exhausted after 25 iterations without FINAL".to_string()),
                };
                match req {
                    RpcRequest::RlmBatch { prompts, .. } => RpcResponse::Batch(BatchResp {
                        results: (0..prompts.len()).map(partial).collect(),
                    }),
                    _ => RpcResponse::Single(partial(0)),
                }
            })
        }
    }

    /// #6511: the Rust loop kept the exhausted sub-RLM's last answer, but the
    /// Python helpers returned only the error marker whenever `error` was
    /// set, so the calling code never saw the text.
    #[tokio::test]
    async fn rlm_query_keeps_partial_answer_from_an_exhausted_child() {
        let mut rt = PythonRuntime::new().await.expect("spawn");
        let round = rt
            .run(
                "print(rlm_query('go'))\nfor r in rlm_query_batched(['a', 'b'], dependency_mode='independent'):\n    print(r)",
                Some(&ExhaustedRlmBridge),
            )
            .await
            .expect("execute");
        // Windows Python prints CRLF; compare line content, not line endings.
        let out = &round.stdout.replace("\r\n", "\n");
        assert!(
            out.contains("partial answer 0\n[rlm_query incomplete: RLM loop exhausted"),
            "{out}"
        );
        // Batched: a child with text keeps it; one with none keeps the old marker.
        assert!(out.matches("partial answer 0").count() == 2, "{out}");
        assert!(out.contains("[child err: RLM loop exhausted"), "{out}");
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn rpc_dispatcher_round_trips_sub_query_alias() {
        let bridge = StubBridge::new();
        let calls = Arc::clone(&bridge.calls);

        let mut rt = PythonRuntime::new().await.expect("spawn");
        let round = rt
            .run("print(sub_query('hello from sub'))", Some(&bridge))
            .await
            .expect("execute");
        assert!(
            round.stdout.contains("stub#0: hello from sub"),
            "stdout: {:?}",
            round.stdout
        );
        assert_eq!(round.rpc_count, 1);

        let recorded = calls.lock().await;
        assert_eq!(recorded.len(), 1);
        match &recorded[0] {
            RpcRequest::Llm { prompt, .. } => assert_eq!(prompt, "hello from sub"),
            other => panic!("expected Llm request, got {other:?}"),
        }
        drop(recorded);
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn rpc_dispatcher_round_trips_batch() {
        let bridge = StubBridge::new();
        let mut rt = PythonRuntime::new().await.expect("spawn");
        let round = rt
            .run(
                "outs = llm_query_batched(['a','b','c'], dependency_mode='independent', safety_note='same independent classification')\n\
                 print('|'.join(outs))",
                Some(&bridge),
            )
            .await
            .expect("execute");
        assert!(round.stdout.contains("stub#0.0: a"));
        assert!(round.stdout.contains("stub#0.1: b"));
        assert!(round.stdout.contains("stub#0.2: c"));
        assert_eq!(round.rpc_count, 1);
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn batched_helpers_require_independence_declaration() {
        let bridge = StubBridge::new();
        let mut rt = PythonRuntime::new().await.expect("spawn");
        let round = rt
            .run(
                "outs = sub_query_batch('summarize', [{'text': 'a'}, {'text': 'b'}])\n\
                 print(outs[0])",
                Some(&bridge),
            )
            .await
            .expect("execute");

        assert!(
            round.stdout.contains("dependency_mode='independent'"),
            "{}",
            round.stdout
        );
        assert_eq!(round.rpc_count, 0);
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn dependent_batch_mode_points_to_sequence_helper() {
        let bridge = StubBridge::new();
        let mut rt = PythonRuntime::new().await.expect("spawn");
        let round = rt
            .run(
                "outs = llm_query_batched(['migrate A', 'migrate B'], dependency_mode='sequential')\n\
                 print(outs[0])",
                Some(&bridge),
            )
            .await
            .expect("execute");

        assert!(
            round.stdout.contains("sub_query_sequence"),
            "{}",
            round.stdout
        );
        assert_eq!(round.rpc_count, 0);
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn sub_query_sequence_feeds_prior_result_into_next_prompt() {
        let bridge = StubBridge::new();
        let calls = Arc::clone(&bridge.calls);

        let mut rt = PythonRuntime::new().await.expect("spawn");
        let round = rt
            .run(
                "outs = sub_query_sequence('process this step', [{'text': 'A'}, {'text': 'B'}])\n\
                 print(len(outs))",
                Some(&bridge),
            )
            .await
            .expect("execute");

        assert!(round.stdout.contains("2"), "{}", round.stdout);
        assert_eq!(round.rpc_count, 2);

        let recorded = calls.lock().await;
        assert_eq!(recorded.len(), 2);
        let second_prompt = match &recorded[1] {
            RpcRequest::Llm { prompt, .. } => prompt,
            other => panic!("expected second Llm request, got {other:?}"),
        };
        assert!(second_prompt.contains("--- dependency_state step 1/2 ---"));
        assert!(second_prompt.contains("stub#0: process this step"));
        drop(recorded);
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn no_dispatcher_returns_unavailable_sentinel() {
        let mut rt = PythonRuntime::new().await.expect("spawn");
        let round = rt.execute("print(llm_query('hi'))").await.expect("execute");
        assert!(
            round.stdout.contains("[llm_query error:") || round.stdout.contains("no LLM bridge"),
            "stdout: {:?}",
            round.stdout
        );
        rt.shutdown().await;
    }

    #[test]
    fn truncate_keeps_short_unchanged() {
        assert_eq!(truncate_stdout("hello", 100), "hello");
    }

    #[test]
    fn truncate_clips_long() {
        let long = "a".repeat(10_000);
        let out = truncate_stdout(&long, 1024);
        assert!(out.len() < 1500);
        assert!(out.contains("truncated"));
    }
}
