//! Recorded-MCP family: one transcript, every dispatch implementation.
//!
//! A transcript (`<case>.case.json`) scripts an MCP server — what it answers
//! to `initialize`, `tools/list`, `resources/*`, `prompts/*`, and to each
//! `tools/call` (a result, an `isError` result, a JSON-RPC error, progress
//! notifications before the result, or no answer at all until the caller
//! cancels) — plus the steps a host takes against it. The harness serves the
//! transcript as a real Streamable HTTP MCP server on loopback
//! ([`server`]), so any client that can open a URL can be pointed at it,
//! including a child Node process; or, for `"transport": "stdio"`, as a
//! POSIX-shell child (`stdio_server.sh.fixture`, named so the repo's `*.sh`
//! ignore rule does not swallow it) the dispatch spawns itself.
//!
//! [`DISPATCHES`] is the seam for the TypeScript migration
//! (TS-EXTENSION-HOST-DESIGN §5.2, §9.3): native Rust and the default Host
//! SDK dispatch must produce the same semantic golden. Transport goldens
//! name each dispatch so discovery, authentication and session behavior stay
//! explicit. Host uses the real pinned SDK and Rust broker.
//!
//! Normalization: the server URL/port is masked; results are
//! `{"ok": {success, content, metadata, content_blocks}}` with JSON content
//! parsed, or `{"err": {kind, detail}}` with exact detail bytes.
//! `server_received` lists the side-effecting requests the server saw
//! (`tools/call`, `resources/read`, `prompts/get`) — a call that must not be
//! sent, or must not be replayed, shows up there.
//!
//! # Case fields beyond the original two cases
//!
//! All optional; an absent field is the original behaviour, so the original
//! goldens did not move.
//!
//! - `transport`: `"http"` (default) or `"stdio"`. A stdio case's `server`
//!   object is written to files for `stdio_server.sh.fixture`; only `initialize`,
//!   `tools/list` and `tools/call` (by `match.name`, with `"exit": true` for
//!   a server that dies after reading the call) are supported. Unix only.
//! - `config`: extra `McpServerConfig` fields merged over the harness's own.
//! - `disallowed_tools`: deny rules handed to the dispatch (pool-level and
//!   per-call, as the engine does).
//! - `expect_boot`: `"error"` records a failed boot as
//!   `{"op":"boot","outcome":{"err":detail}}` instead of failing the harness;
//!   steps still run, so the catalog after a failed boot is pinned too. A
//!   case that expects an error and boots cleanly fails the harness.
//! - `server_options`: `numbered_sessions`, `require_bearer` (see [`server`]).
//! - `bearer_secret`: a fake token the harness exports as the server's
//!   `bearer_token_env_var`. It must appear in no recorded byte: the outcome
//!   is scanned for it before a golden is compared or written, in update mode
//!   too.
//! - `sink`: start a second server and put its URL where the spec says
//!   `{{sink_url}}`; the outcome records how many requests reached it.
//! - `record_requests`: `"full"` adds every POST the server saw (session,
//!   protocol version, authorization shape, status) to the outcome as
//!   `requests`; `"summary"` adds only the distinct methods, authorization
//!   shapes and statuses (`requests_summary`), for cases that must not pin how
//!   often a client retries a connect.
//!
//! Steps: `catalog`, `call`, and `approval_hints` (`{"op":"approval_hints",
//! "tools":[model names]}`, read after a `catalog` step).
//!
//! What a host-backed dispatch is *not* tested on by this family is listed in
//! the fixture README's coverage boundaries: reviewed-plugin launches (hash
//! refusal, trusted-read-only approval hints), real OAuth login, the SSE
//! legacy transport, the 32 MiB aggregate catalog byte cap, concurrent
//! calls, and a held stdio call.

mod controls;
mod server;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio_util::sync::CancellationToken;

use self::server::{ServerOptions, TranscriptServer};
use super::golden::{self, Failures, Sandbox};
use crate::test_support::EnvVarGuard;
use codewhale_engine::core::engine::Engine;
use codewhale_engine::core::events::Event;
use codewhale_engine::mcp::{McpPool, McpToolApprovalHint};
use codewhale_engine::tools::spec::{RichToolResult, ToolError};
use codewhale_mcp::config::McpConfig;

const FAMILY: &str = "mcp";
const CALL_DEADLINE: Duration = Duration::from_secs(30);
/// The environment variable a case's `bearer_secret` is exported through.
const BEARER_ENV: &str = "CODEWHALE_CONFORMANCE_MCP_TOKEN";

// === The dispatch seam ===

/// Everything a dispatch is built from: the connection config, and the deny
/// rules the engine would also hand to the pool (`--disallowed-tools`).
struct DispatchSetup {
    config: McpConfig,
    disallowed_tools: Vec<String>,
}

/// One implementation of "call a tool on an external MCP server", as the
/// engine sees it. Planning, hooks and approval happen before `call`;
/// implementors never gate (design §5.2) — except that deny rules are part of
/// the dispatch's setup and refuse a call before anything reaches a server.
#[async_trait::async_trait]
trait McpDispatchUnderTest: Send + Sync {
    /// Connect to every configured server. An `Err` is a connect failure
    /// (including needs-auth); the dispatch must still answer `catalog` and
    /// `call` afterwards, with whatever it can still advertise.
    async fn boot(&self) -> Result<(), String>;
    /// The model-visible tool catalog this dispatch advertises.
    async fn catalog(&self) -> Vec<codewhale_models::Tool>;
    /// Call one model-facing tool; `cancel` is the turn interrupt.
    async fn call(
        &self,
        model_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<RichToolResult, ToolError>;
    /// Addition for the approval-hint case: how the approval path may treat
    /// `model_name` given what the server's catalog declared, read after
    /// `catalog()`. `"destructive"` (each call keeps its prompt),
    /// `"trusted_read_only"` (a reviewed plugin's read-only tool runs
    /// unprompted), or `None`. Implementation-neutral: any dispatch that
    /// consumes MCP tool annotations has this answer.
    async fn approval_hint(&self, model_name: &str) -> Option<&'static str>;
    async fn shutdown(&self);
}

type DispatchFactory = fn(setup: DispatchSetup) -> Box<dyn McpDispatchUnderTest>;

/// Every dispatch shares semantic goldens and retains its transport goldens.
const DISPATCHES: &[(&str, DispatchFactory)] = &[
    ("mcp_pool", McpPoolDispatch::boxed),
    ("host_sdk", HostMcpDispatch::boxed),
];

/// The explicitly selected native Rust path through the shared engine pool.
struct McpPoolDispatch {
    pool: Arc<AsyncMutex<McpPool>>,
    tx_event: mpsc::Sender<Event>,
    disallowed_tools: Vec<String>,
    // Kept open so status events the dispatch emits never hit a closed channel.
    _rx_event: Mutex<mpsc::Receiver<Event>>,
}

impl McpPoolDispatch {
    fn boxed(setup: DispatchSetup) -> Box<dyn McpDispatchUnderTest> {
        let (tx_event, rx_event) = mpsc::channel(64);
        let pool = McpPool::new(setup.config)
            .with_backend(codewhale_config::McpBackend::Rust)
            .with_disallowed_tools(setup.disallowed_tools.clone());
        Box::new(Self {
            pool: Arc::new(AsyncMutex::new(pool)),
            tx_event,
            disallowed_tools: setup.disallowed_tools,
            _rx_event: Mutex::new(rx_event),
        })
    }
}

#[async_trait::async_trait]
impl McpDispatchUnderTest for McpPoolDispatch {
    async fn boot(&self) -> Result<(), String> {
        let errors = self.pool.lock().await.connect_all().await;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors
                .iter()
                .map(|(name, error)| format!("{name}: {error:#}"))
                .collect::<Vec<_>>()
                .join("; "))
        }
    }

    async fn catalog(&self) -> Vec<codewhale_models::Tool> {
        self.pool.lock().await.to_api_tools()
    }

    async fn call(
        &self,
        model_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<RichToolResult, ToolError> {
        // The engine interrupts a tool by dropping its future; do the same.
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ToolError::cancelled("tool call interrupted by the host")),
            result = Engine::execute_mcp_tool_with_pool(
                Arc::clone(&self.pool),
                &self.tx_event,
                model_name,
                input,
                // The engine hands the registry context's rules to every call
                // as well as to the pool; do both.
                &self.disallowed_tools,
                // Conformance probes cannot supply a person's card decision.
                None,
            ) => result,
        }
    }

    async fn approval_hint(&self, model_name: &str) -> Option<&'static str> {
        codewhale_engine::mcp::mcp_tool_approval_hint(model_name).map(|hint| match hint {
            McpToolApprovalHint::TrustedReadOnly => "trusted_read_only",
            McpToolApprovalHint::Destructive => "destructive",
        })
    }

    async fn shutdown(&self) {
        self.pool.lock().await.shutdown_all().await;
    }
}

/// The selected production Host pool, not a semantic fake. The sandbox
/// already seals CODEWHALE_HOME; manager materialization and native child
/// launch use that same root. Guards live through shutdown on this runner's
/// current-thread runtime, so host workers never inherit another case's policy.
struct HostMcpDispatch {
    inner: McpPoolDispatch,
    manager: Arc<codewhale_engine::extension_host::ExtensionHostManager>,
    _manager: codewhale_engine::extension_host::TestManagerGuard,
    _policy: codewhale_plugins::activation::TestPolicyGuard,
}
impl HostMcpDispatch {
    fn boxed(setup: DispatchSetup) -> Box<dyn McpDispatchUnderTest> {
        let node = codewhale_engine::extension_host::test_support::node_for_tests(
            "recorded MCP Host SDK parity",
        )
        .expect("recorded Host SDK parity requires a supported local Node runtime");
        let root =
            PathBuf::from(std::env::var_os("CODEWHALE_HOME").expect("sealed conformance home"));
        let manager = Arc::new(codewhale_engine::extension_host::ExtensionHostManager::new(
            codewhale_engine::extension_host::ExtensionHostOptions {
                node_override: Some(node),
                root: Some(root),
                ..Default::default()
            },
        ));
        let policy = codewhale_plugins::activation::TestPolicyGuard::extension_host(true);
        let manager_guard =
            codewhale_engine::extension_host::TestManagerGuard::install(Arc::clone(&manager));
        let (tx_event, rx_event) = mpsc::channel(64);
        let pool = McpPool::new(setup.config)
            .with_disallowed_tools(setup.disallowed_tools.clone())
            .with_backend(codewhale_config::McpBackend::Host);
        Box::new(Self {
            inner: McpPoolDispatch {
                pool: Arc::new(AsyncMutex::new(pool)),
                tx_event,
                disallowed_tools: setup.disallowed_tools,
                _rx_event: Mutex::new(rx_event),
            },
            manager,
            _manager: manager_guard,
            _policy: policy,
        })
    }
}
#[async_trait::async_trait]
impl McpDispatchUnderTest for HostMcpDispatch {
    async fn boot(&self) -> Result<(), String> {
        self.inner.boot().await
    }
    async fn catalog(&self) -> Vec<codewhale_models::Tool> {
        self.inner.catalog().await
    }
    async fn call(
        &self,
        name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<RichToolResult, ToolError> {
        self.inner.call(name, input, cancel).await
    }
    async fn approval_hint(&self, name: &str) -> Option<&'static str> {
        self.inner.approval_hint(name).await
    }
    async fn shutdown(&self) {
        self.inner.shutdown().await;
        self.manager.shutdown().await;
    }
}

// === The server a case runs against ===

struct Backend {
    server: Option<TranscriptServer>,
    sink: Option<TranscriptServer>,
    stdio_dir: Option<PathBuf>,
}

impl Backend {
    /// Start what the case scripts and return it with the `McpServerConfig`
    /// the dispatch connects with.
    async fn start(case: &Value, workspace: &Path) -> (Self, Value) {
        let mut config = if case["transport"].as_str() == Some("stdio") {
            let dir = workspace.join("stdio_server");
            write_stdio_server(&dir, &case["server"]);
            let script = dir.join("stdio_server.sh");
            let config = json!({
                "command": "sh",
                "args": [script, dir],
                "connect_timeout": 10,
                "execute_timeout": 10,
            });
            return (
                Self {
                    server: None,
                    sink: None,
                    stdio_dir: Some(dir),
                },
                merged(config, &case["config"]),
            );
        } else {
            json!({ "connect_timeout": 10, "execute_timeout": 10 })
        };
        let sink = if case["sink"].as_bool() == Some(true) {
            Some(TranscriptServer::start(json!({}), ServerOptions::default()).await)
        } else {
            None
        };
        // The sink's URL is not known until it is listening; the spec names
        // it as `{{sink_url}}`.
        let spec = match &sink {
            Some(sink) => serde_json::from_str(
                &case["server"]
                    .to_string()
                    .replace("{{sink_url}}", &sink.url),
            )
            .expect("spec with sink url"),
            None => case["server"].clone(),
        };
        let secret = case["bearer_secret"].as_str();
        let options = ServerOptions {
            numbered_sessions: case["server_options"]["numbered_sessions"].as_bool() == Some(true),
            require_bearer: case["server_options"]["require_bearer"]
                .as_bool()
                .filter(|required| *required)
                .and_then(|_| secret.map(str::to_string)),
        };
        let server = TranscriptServer::start(spec, options).await;
        config["url"] = json!(server.url);
        if secret.is_some() {
            config["bearer_token_env_var"] = json!(BEARER_ENV);
        }
        (
            Self {
                server: Some(server),
                sink,
                stdio_dir: None,
            },
            merged(config, &case["config"]),
        )
    }

    /// Server URL/port literals the golden masks.
    fn literals(&self) -> Vec<(String, String)> {
        let mut literals = Vec::new();
        for (server, url_label, addr_label) in [
            (&self.server, "<SERVER_URL>", "<SERVER_ADDR>"),
            (&self.sink, "<SINK_URL>", "<SINK_ADDR>"),
        ] {
            if let Some(server) = server {
                literals.push((server.url.clone(), url_label.to_string()));
                literals.push((server.addr.clone(), addr_label.to_string()));
            }
        }
        literals
    }

    /// Side-effecting requests the server was asked to run.
    fn received(&self) -> Vec<Value> {
        if let Some(server) = &self.server {
            return server.received();
        }
        let dir = self.stdio_dir.as_ref().expect("a backend has a server");
        std::fs::read_to_string(dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let request: Value = serde_json::from_str(line).expect("logged request");
                json!({
                    "method": "tools/call",
                    "name": request["params"]["name"],
                    "arguments": request["params"].get("arguments").cloned().unwrap_or(Value::Null),
                })
            })
            .collect()
    }
}

fn merged(mut base: Value, extra: &Value) -> Value {
    if let Some(extra) = extra.as_object() {
        for (key, value) in extra {
            base[key] = value.clone();
        }
    }
    base
}

/// Materialize a transcript for `stdio_server.sh.fixture`: the script itself plus one
/// reply file per scripted answer (see the script's header).
fn write_stdio_server(dir: &Path, spec: &Value) {
    std::fs::create_dir_all(dir).expect("stdio server dir");
    std::fs::copy(
        golden::family_dir(FAMILY).join("stdio_server.sh.fixture"),
        dir.join("stdio_server.sh"),
    )
    .expect("copy stdio_server.sh.fixture");
    // `"result":{…}` / `"error":{…}`: a JSON-RPC reply minus its id.
    let fragment = |entry: &Value| match entry.get("error") {
        Some(error) => format!("\"error\":{error}"),
        None => format!("\"result\":{}", entry["result"]),
    };
    for (method, file) in [
        ("initialize", "initialize.reply"),
        ("tools/list", "tools_list.reply"),
    ] {
        let entry = spec
            .get(method)
            .unwrap_or_else(|| panic!("a stdio case scripts `{method}`"));
        std::fs::write(dir.join(file), fragment(entry)).expect("write reply");
    }
    for call in spec["tools/call"].as_array().into_iter().flatten() {
        let tool = call["match"]["name"]
            .as_str()
            .expect("tools/call match.name");
        if call["exit"].as_bool() == Some(true) {
            std::fs::write(dir.join(format!("call.{tool}.exit")), "").expect("write exit");
        } else {
            std::fs::write(dir.join(format!("call.{tool}.reply")), fragment(call))
                .expect("write reply");
        }
    }
}

// === Running a transcript ===

#[test]
fn harness_timeout_rejects_a_real_unanswered_mcp_call() {
    let mut case = golden::read_case(FAMILY, "tools_resources_prompts");
    let held = case["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|step| step["cancel"] == "after_server_holds")
        .expect("held-call fixture")
        .clone();
    let mut held = held;
    held.as_object_mut().expect("step").remove("cancel");
    case["steps"] = json!([held]);
    let outcome = run_case(
        "unanswered_call",
        &case,
        "mcp_pool",
        McpPoolDispatch::boxed,
        Duration::from_secs(1),
        true,
    );
    let mut failures = Failures::default();
    match outcome {
        Err(error) => failures.push("unanswered_call", error),
        Ok(()) => panic!("an unanswered MCP call became recordable output"),
    }
    assert!(failures.contains("harness timeout: MCP call"));
}

fn normalize_call(result: &Result<RichToolResult, ToolError>) -> Value {
    match result {
        Ok(rich) => json!({ "ok": {
            "success": rich.result.success,
            "content": serde_json::from_str::<Value>(&rich.result.content)
                .unwrap_or_else(|_| Value::String(rich.result.content.clone())),
            "metadata": rich.result.metadata,
            "content_blocks": rich.content_blocks,
        } }),
        Err(error) => {
            json!({ "err": { "kind": golden::tool_error_kind(error), "detail": error.to_string() } })
        }
    }
}

async fn run_transcript(
    case: &Value,
    factory: DispatchFactory,
    workspace: &Path,
    deadline: Duration,
) -> Result<(Value, Vec<(String, String)>), String> {
    let scripted_steps = case["steps"].as_array().expect("case.steps");
    if scripted_steps.is_empty() {
        return Err("MCP transcript has no host steps".to_string());
    }
    let (backend, server_config) = Backend::start(case, workspace).await;
    let server_name = case["server_name"].as_str().unwrap_or("conformance");
    let config: McpConfig =
        serde_json::from_value(json!({ "servers": { server_name: server_config } }))
            .expect("transcript MCP config");
    let dispatch = factory(DispatchSetup {
        config,
        disallowed_tools: case["disallowed_tools"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|rule| rule.as_str().expect("deny rule").to_string())
            .collect(),
    });
    let mut steps = Vec::new();
    let expect_boot_error = case["expect_boot"].as_str() == Some("error");
    let boot = match golden::complete_within("MCP boot", deadline, dispatch.boot()).await? {
        Ok(()) if expect_boot_error => {
            return Err("MCP transcript expected boot to fail but it connected".to_string());
        }
        Ok(()) => json!("ok"),
        Err(detail) if expect_boot_error => json!({ "err": detail }),
        Err(detail) => return Err(format!("MCP transcript could not boot: {detail}")),
    };
    steps.push(json!({ "op": "boot", "outcome": boot }));
    for step in scripted_steps {
        match step["op"].as_str().expect("step.op") {
            "catalog" => {
                let catalog = serde_json::to_value(
                    golden::complete_within("MCP catalog", deadline, dispatch.catalog()).await?,
                )
                .expect("catalog");
                steps.push(json!({ "op": "catalog", "tools": catalog }));
            }
            "approval_hints" => {
                let mut hints = serde_json::Map::new();
                for tool in step["tools"].as_array().expect("step.tools") {
                    let tool = tool.as_str().expect("tool name");
                    let hint = golden::complete_within(
                        "MCP approval hint",
                        deadline,
                        dispatch.approval_hint(tool),
                    )
                    .await?;
                    hints.insert(tool.to_string(), json!(hint));
                }
                steps.push(json!({ "op": "approval_hints", "hints": hints }));
            }
            "call" => {
                let tool = step["tool"].as_str().expect("step.tool");
                let cancel = CancellationToken::new();
                let canceller =
                    (step["cancel"].as_str() == Some("after_server_holds")).then(|| {
                        let state = Arc::clone(
                            &backend
                                .server
                                .as_ref()
                                .expect("only an HTTP case can hold a call")
                                .state,
                        );
                        let cancel = cancel.clone();
                        tokio::spawn(async move {
                            state.held.notified().await;
                            cancel.cancel();
                        })
                    });
                let outcome = golden::complete_within(
                    "MCP call",
                    deadline,
                    dispatch.call(tool, step["input"].clone(), cancel),
                )
                .await;
                if let Some(canceller) = canceller {
                    canceller.abort();
                }
                let outcome = normalize_call(&outcome?);
                steps.push(json!({ "op": "call", "tool": tool, "outcome": outcome }));
            }
            other => panic!("unknown transcript op `{other}`"),
        }
    }
    golden::complete_within("MCP shutdown", deadline, dispatch.shutdown()).await?;
    let mut outcome = json!({ "steps": steps, "server_received": backend.received() });
    let requests = backend
        .server
        .as_ref()
        .map(TranscriptServer::requests)
        .unwrap_or_default();
    match case["record_requests"].as_str() {
        Some("full") => outcome["requests"] = Value::Array(requests),
        Some("summary") => outcome["requests_summary"] = summarize_requests(&requests),
        Some(other) => panic!("unknown record_requests `{other}`"),
        None => {}
    }
    if let Some(sink) = &backend.sink {
        outcome["sink_requests"] = json!(sink.requests().len());
    }
    Ok((outcome, backend.literals()))
}

/// The distinct RPC methods, `Authorization` shapes and HTTP statuses the
/// server saw, without their order or count. For cases where *that* a request
/// carried a credential (or never got past `initialize`) is the contract, but
/// how often a client retries a connect is its own business.
fn summarize_requests(requests: &[Value]) -> Value {
    let distinct = |field: &str| {
        let mut values: Vec<Value> = Vec::new();
        for request in requests {
            if !values.contains(&request[field]) {
                values.push(request[field].clone());
            }
        }
        values.sort_by_key(Value::to_string);
        Value::Array(values)
    };
    json!({
        "rpcs": distinct("rpc"),
        "authorization_shapes": distinct("authorization"),
        "statuses": distinct("status"),
    })
}

/// No recorded byte may carry a credential. The needle is the secret itself,
/// so a bearer header, a URL, a JSON string or an error detail all trip it.
fn assert_no_secret(secret: &str, recorded: &str) -> Result<(), String> {
    if recorded.contains(secret) {
        return Err(format!(
            "secret leak: the case's bearer secret appears in the recorded output:\n{}",
            recorded
                .lines()
                .filter(|line| line.contains(secret))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    Ok(())
}

/// Run one case through one dispatch and compare with its golden.
///
/// `compare_only` forces compare mode even under `CODEWHALE_CONFORMANCE_UPDATE`:
/// the negative controls feed deliberately changed cases through here and must
/// never rewrite a golden.
fn run_case(
    name: &str,
    case: &Value,
    dispatch: &str,
    factory: DispatchFactory,
    deadline: Duration,
    compare_only: bool,
) -> Result<(), String> {
    if case["transport"].as_str() == Some("stdio") && !cfg!(unix) {
        eprintln!("conformance: `{name}` needs a POSIX shell; skipped on this platform");
        return Ok(());
    }
    // The pool may touch its state directory; keep that hermetic. Guards are
    // declared after the sandbox so they restore the environment before the
    // sandbox releases the lock.
    let sandbox = Sandbox::new(&Value::Null);
    let _compare_only = compare_only.then(|| EnvVarGuard::set(golden::UPDATE_ENV, "0"));
    let secret = case["bearer_secret"].as_str();
    let _bearer = secret.map(|secret| EnvVarGuard::set(BEARER_ENV, secret));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let (mut outcome, literals) =
        runtime.block_on(run_transcript(case, factory, &sandbox.workspace, deadline))?;
    drop(runtime);
    let mut masker = sandbox.masker(&[]);
    for (literal, label) in literals {
        masker = masker.literal(&literal, &label);
    }
    masker.value(&mut outcome);
    let recorded = golden::pretty(&golden::canonical(&outcome));
    if let Some(secret) = secret {
        assert_no_secret(secret, &recorded)?;
    }
    let transport = if let Some(kind) = case["record_requests"].as_str() {
        let field = match kind {
            "full" => "requests",
            "summary" => "requests_summary",
            other => panic!("unknown record_requests `{other}`"),
        };
        Some(
            json!({field: outcome.as_object_mut().expect("outcome").remove(field).expect("recorded transport")}),
        )
    } else {
        None
    };
    golden::check_golden(
        &golden::family_dir(FAMILY).join(format!("{name}.golden.json")),
        &golden::pretty(&golden::canonical(&outcome)),
    )?;
    if let Some(transport) = transport {
        golden::check_golden(
            &golden::family_dir(FAMILY).join(format!("{name}.{dispatch}.transport.golden.json")),
            &golden::pretty(&golden::canonical(&transport)),
        )?;
    }
    Ok(())
}

#[test]
fn mcp_transcripts_match_goldens_for_every_dispatch() {
    let names = golden::case_names(FAMILY);
    let mut failures = Failures::default();
    for name in &names {
        let case = golden::read_case(FAMILY, name);
        for (dispatch, factory) in DISPATCHES {
            failures.record(
                &format!("{name} via {dispatch}"),
                run_case(name, &case, dispatch, *factory, CALL_DEADLINE, false),
            );
        }
    }
    failures.finish(FAMILY, names.len() * DISPATCHES.len());
}
