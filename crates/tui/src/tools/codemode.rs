//! `execute_tools` — code mode: run a model-provided JavaScript program that
//! composes tool calls through `tools.call(name, args)`.
//!
//! The program handles loops, branching, filtering, and data movement;
//! intermediate results stay in the VM and only the bounded return value plus
//! a host-owned receipt reach the model. The engine-side precedent is the
//! synthetic interpreter dispatch (`js_execution` / `code_execution`): this
//! tool is engine-injected, never registered, and dispatched in
//! `core::engine::tool_execution`.
//!
//! Authority stays entirely in Rust, and there is one gate. In a session turn,
//! a sub-agent's included, the engine hands the program a [`NestedCallGate`];
//! every nested call — native, plugin, or MCP — is sent back to the turn loop
//! and planned by the same `plan_tool_calls` a direct call goes through
//! (deny/allow lists, preparation, hooks, ask-rules, Auto-Review, repo law, the
//! worker authority envelope, a sub-agent's grant, the K1 Computer Use
//! refusal), with the source tagged as code mode so a nested call never
//! activates a deferred schema. A nested call that needs approval suspends the
//! program: the engine raises the normal approval request
//! (`request_tool_approval`, same receipt log, same card) and the program
//! resumes on allow; on deny only that nested call fails, as an exception the
//! program can catch. Approving the program itself therefore grants nothing,
//! and `execute_tools` is prepared as auto-approved. A sub-agent's nested call
//! is then admitted at execution like its direct call
//! (`ToolRegistry::admit_child_call`: write-scope claim and live peers).
//!
//! Without a gate (a direct unit call, or a dispatch with no per-call context)
//! the program runs read-only, auto-approved native calls only, no MCP.
//!
//! Receipts (#6509): the host records every nested call when it starts, so a
//! program that hits its deadline still reports what finished, what was
//! refused, and what was in flight. Oversized nested results keep the
//! `{content, metadata, truncated}` keys, and the cut is never silent:
//! `truncated` names the original size and the spillover file holding the
//! full output, and `content` becomes the leading text of the raw output
//! (a JSON result cannot stay parsed once cut), so a script checks
//! `truncated` before reading fields. A failed call's text is bounded and
//! spilled the same way.
//!
//! KV-cache effect: `[features] code_mode` (on by default) makes
//! `execute_tools` eager from the first request of a session; with the flag
//! off it is deferred like the interpreter tools. The flag is session config,
//! so the prefix is stable within a session either way. The definition text
//! is static. Program text, nested results, and the describe-only nested
//! `tool_search` (which returns schemas without activating them) live in
//! append-only turn history, never in the prefix.
//!
//! Known limitations:
//! - Nested calls do not take per-tool locks against sibling top-level calls;
//!   the program runs under its own exclusive lock instead. Inside the
//!   program, calls the gate marks non-parallel run one at a time.
//! - No nested `agent`, `workflow`, `rlm`, `request_user_input`, interpreter,
//!   interactive shell, sandbox escalation, Computer Use consent/script, MCP
//!   sign-in, or recursive `execute_tools`. Those stay direct calls.
//! - Rich content blocks (images) from nested results are dropped; text and
//!   JSON payloads pass through bounded.
//! - Hidden from Plan mode and refused under a worker authority envelope,
//!   like the other execution surfaces. Also hidden from a sub-agent whose
//!   grant refuses it (the evidence surface, or an explicit tool list that
//!   leaves it out).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, RwLock, Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use codewhale_models::Tool;
use codewhale_workflow_js::{
    BudgetSnapshot, DriverError, ProgressEvent, SpawnedTask, TaskRequest, ToolCallRequest,
    ToolCallResponse, ToolInvoker, WorkflowDriver, WorkflowRunCancel, WorkflowVm,
};

use crate::core::events::Event;
use crate::mcp::McpPool;
use crate::tools::registry::{ToolRegistry, enforce_tool_authority};
use crate::tools::spec::{
    ApprovalRequirement, RichToolResult, ToolContext, ToolError, ToolResult, ToolSpec, required_str,
};

/// Tool name surfaced to the model. Dispatched alongside the synthetic
/// interpreter tools; see `core::engine::tool_execution`.
pub const EXECUTE_TOOLS_TOOL_NAME: &str = "execute_tools";

const EXECUTE_TOOLS_TOOL_TYPE: &str = "execute_tools_20260918";

/// Maximum program source accepted, in bytes.
const MAX_CODE_BYTES: usize = 64 * 1024;
/// Run deadline when no engine turn serves the program (a direct unit call,
/// or a dispatch with no per-call context). This is an hour-long backstop
/// rather than an invented short cap. A gated run takes the remaining turn
/// wall clock instead, which is unbounded unless `[tui].turn_wall_clock_secs`
/// is set. A sub-agent's program also stops at the sub-agent's tool timeout
/// or work deadline, and still returns its receipt.
const FALLBACK_RUN_DEADLINE: Duration = Duration::from_secs(3_600);
/// How often the run watchdog re-checks while the program is paused on the
/// gate (approval card, hook, review). Bounds the overrun after a pause.
const PAUSED_WATCHDOG_POLL: Duration = Duration::from_millis(200);
/// Maximum nested tool calls in flight at once, enforced host-side.
const MAX_CONCURRENT_CALLS: usize = 4;
/// Per nested-call result cap, in serialized bytes. The same threshold the
/// engine spills a direct tool result at, so a nested result is never cut
/// sooner than the same call made directly.
const PER_CALL_RESULT_CAP_BYTES: usize = crate::tools::truncate::SPILLOVER_THRESHOLD_BYTES;
/// Model-visible return cap, in serialized bytes.
const RETURN_CAP_BYTES: usize = 16 * 1024;

/// Names refused before the gate, with a message that names the supported
/// alternative. These either need the turn loop itself (a prompt, a
/// sub-agent, a schema activation) or would nest an execution surface.
const PROHIBITED_NESTED: &[&str] = &[
    EXECUTE_TOOLS_TOOL_NAME,
    "code_execution",
    "js_execution",
    "agent",
    "workflow",
    // Recursive RLM rounds are admitted by the turn loop serving the direct
    // `rlm` call; a program has no such server for a nested one.
    "rlm",
    crate::core::engine::tool_catalog::REQUEST_USER_INPUT_NAME,
    crate::core::engine::tool_catalog::MULTI_TOOL_PARALLEL_NAME,
];

/// Model-facing definition. `defer_loading` is decided by the catalog
/// (eager under code mode, deferred otherwise); `allowed_callers` mirrors
/// the interpreter tools. The text is static so it never moves the prefix.
pub fn execute_tools_tool_definition() -> Tool {
    Tool {
        tool_type: Some(EXECUTE_TOOLS_TOOL_TYPE.to_string()),
        name: EXECUTE_TOOLS_TOOL_NAME.to_string(),
        description: "Run a JavaScript program that composes tool calls with \
             `await tools.call(name, args)` and returns a bounded JSON result. Prefer it \
             whenever you would make several dependent or repetitive calls, MCP and plugin \
             tools included: intermediate results stay in the program and only what you \
             return reaches the conversation. Every nested call passes the same permission \
             checks as a direct call; a call that needs approval pauses the program until \
             the user decides, and a denied or refused call throws inside the program (catch \
             it to continue). Inside a program, tools.call('tool_search', {query}) returns \
             matching tool names with their input schemas without loading them into the \
             conversation. Each result is {content, metadata, truncated}. When a result is \
             cut, truncated names its full size and saved copy and content is the leading \
             text of the raw output instead of parsed JSON, so check truncated before \
             reading fields. Not \
             available inside programs: agent, workflow, request_user_input, nested \
             execute_tools, interactive shells, sandbox escalation, Computer Use consent or \
             scripts, and MCP sign-in. At most 50 nested calls, 4 concurrent; the return \
             value is capped at 16 KiB."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "JavaScript program. The return value (or thrown error) becomes the result; use tools.call(name, argsObject) for tool calls."
                }
            },
            "required": ["code"]
        }),
        allowed_callers: Some(vec!["direct".to_string()]),
        defer_loading: Some(false),
        input_examples: None,
        strict: None,
        cache_control: None,
    }
}

// ---------------------------------------------------------------------------
// The engine-served gate
// ---------------------------------------------------------------------------

/// How the gate decided one nested call. Named in the receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NestedDecision {
    /// Admitted without a prompt: an auto-approved tier, a remembered grant,
    /// or a posture that already covers it.
    Auto,
    /// A person approved this exact nested call.
    Approved,
    /// A person denied it.
    Denied,
    /// The approval card expired with no answer. The call did not run, and
    /// the user did not deny it.
    TimedOut,
    /// A gate refused it before any prompt (policy, hook, authority, or a
    /// name that stays a direct call).
    Refused,
}

/// The gate's answer for one nested call.
pub(crate) enum NestedCallVerdict {
    /// Run the call with the final (possibly hook-rewritten) name and input.
    Run {
        name: String,
        input: Value,
        supports_parallel: bool,
        decision: NestedDecision,
        /// `additionalContext` from tool_call_before hooks (#3026), recorded
        /// on the call's receipt so it reaches the model.
        hook_context: Option<String>,
    },
    /// The engine answered in place (describe-only `tool_search`, a guard).
    Answered {
        result: ToolResult,
        hook_context: Option<String>,
    },
    /// Do not run it; the error is what the program sees.
    Refused {
        error: ToolError,
        decision: NestedDecision,
    },
}

/// One nested call waiting on the turn loop's gate.
pub(crate) struct NestedCallRequest {
    pub(crate) name: String,
    pub(crate) input: Value,
    pub(crate) reply: oneshot::Sender<NestedCallVerdict>,
    /// Fires when the caller no longer wants an answer (an extension's host
    /// cancelled the request, its owner was revoked, the host exited, the
    /// invocation ended). The server must not start work for a request whose
    /// token has fired ([`Self::is_stale`]) and must stop waiting on a person
    /// when it fires: an approval is withdrawn, never silently decided.
    pub(crate) withdraw: Option<CancellationToken>,
}

impl NestedCallRequest {
    /// Nobody is waiting for the answer any more.
    pub(crate) fn is_stale(&self) -> bool {
        self.reply.is_closed()
            || self
                .withdraw
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
    }
}

/// Who an extension's `core/call` is made for, as the core knows it: composed
/// by Rust from the extension tool's registration, never from anything the host
/// says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCaller {
    /// `extension:<plugin>`: named on the approval card and in audit records.
    pub(crate) origin: String,
    /// The extension tool the calls are made inside.
    pub(crate) tool: String,
    /// The approval-grant scope of that plugin build (`ext:<id>@<hash>`); the
    /// approval keys of its calls are scoped by it, so a grant given to the
    /// model never covers an extension's call and the reverse.
    pub(crate) scope: String,
}

/// What an extension tool's gate carries beyond the channel: who it is for,
/// and the tool snapshot its calls run against.
struct ExtensionGate {
    caller: ExtensionCaller,
    specs: Vec<Arc<dyn ToolSpec>>,
}

/// Handle a running program uses to reach the engine turn that launched it.
/// Built per `execute_tools` call by the turn loop and carried on that call's
/// [`ToolContext`]; dropped with the call.
#[derive(Clone)]
pub(crate) struct NestedCallGate {
    requests: mpsc::Sender<NestedCallRequest>,
    mcp_pool: Option<Arc<AsyncMutex<McpPool>>>,
    tx_event: mpsc::Sender<Event>,
    deadline: Duration,
    /// Set only on the gate of an extension tool's call
    /// ([`Self::for_extension`]).
    extension: Option<Arc<ExtensionGate>>,
}

impl NestedCallGate {
    /// A gate plus the receiver the turn loop serves. `deadline` bounds the
    /// program's own run time; time spent waiting on the gate (approval
    /// cards, hooks, reviews) does not count against it.
    pub(crate) fn new(
        mcp_pool: Option<Arc<AsyncMutex<McpPool>>>,
        tx_event: mpsc::Sender<Event>,
        deadline: Duration,
    ) -> (Self, mpsc::Receiver<NestedCallRequest>) {
        let (requests, receiver) = mpsc::channel(MAX_CONCURRENT_CALLS);
        (
            Self {
                requests,
                mcp_pool,
                tx_event,
                deadline,
                extension: None,
            },
            receiver,
        )
    }

    /// This gate serves one extension tool's call: `caller` is who it is for
    /// and `specs` the tool snapshot its core calls run against.
    pub(crate) fn for_extension(
        mut self,
        caller: ExtensionCaller,
        specs: Vec<Arc<dyn ToolSpec>>,
    ) -> Self {
        self.extension = Some(Arc::new(ExtensionGate { caller, specs }));
        self
    }

    /// The caller and tool snapshot of an extension tool's gate; `None` for
    /// the gate of an `execute_tools` program or an `rlm` call.
    pub(crate) fn extension(&self) -> Option<(&ExtensionCaller, &[Arc<dyn ToolSpec>])> {
        self.extension
            .as_deref()
            .map(|gate| (&gate.caller, gate.specs.as_slice()))
    }

    /// Ask the serving turn loop to decide one nested call. A gate nobody
    /// serves any more refuses.
    pub(crate) async fn ask(&self, name: String, input: Value) -> NestedCallVerdict {
        self.ask_withdrawable(name, input, None).await
    }

    /// [`Self::ask`], withdrawn when `withdraw` fires: the request is dropped
    /// if the server has not started it, and the server stops waiting on a
    /// person if it has (the approval is recorded cancelled).
    pub(crate) async fn ask_withdrawable(
        &self,
        name: String,
        input: Value,
        withdraw: Option<CancellationToken>,
    ) -> NestedCallVerdict {
        let unavailable = || NestedCallVerdict::Refused {
            error: ToolError::not_available(
                "the turn that launched this program is no longer serving its permission gate",
            ),
            decision: NestedDecision::Refused,
        };
        let cancelled = || NestedCallVerdict::Refused {
            error: ToolError::cancelled("the call was withdrawn before the gate answered"),
            decision: NestedDecision::Refused,
        };
        let (reply, answer) = oneshot::channel();
        let request = NestedCallRequest {
            name,
            input,
            reply,
            withdraw: withdraw.clone(),
        };
        let wait = async {
            if self.requests.send(request).await.is_err() {
                return unavailable();
            }
            answer.await.unwrap_or_else(|_| unavailable())
        };
        match withdraw {
            None => wait.await,
            Some(withdraw) => tokio::select! {
                biased;
                () = withdraw.cancelled() => cancelled(),
                verdict = wait => verdict,
            },
        }
    }
}

#[cfg(test)]
impl NestedCallGate {
    /// A gate whose server admits every call exactly as asked, for tests of
    /// consumers that are not about admission. Needs a Tokio runtime.
    pub(crate) fn admitting_for_test() -> Self {
        Self::answering_for_test(|name, input| NestedCallVerdict::Run {
            name: name.to_string(),
            input: input.clone(),
            supports_parallel: false,
            decision: NestedDecision::Auto,
            hook_context: None,
        })
    }

    /// A gate whose server answers every call with `answer`.
    pub(crate) fn answering_for_test(
        answer: impl Fn(&str, &Value) -> NestedCallVerdict + Send + 'static,
    ) -> Self {
        let (tx_event, mut rx_event) = mpsc::channel(64);
        tokio::spawn(async move { while rx_event.recv().await.is_some() {} });
        let (gate, mut requests) = Self::new(None, tx_event, Duration::from_secs(60));
        tokio::spawn(async move {
            while let Some(request) = requests.recv().await {
                let _ = request.reply.send(answer(&request.name, &request.input));
            }
        });
        gate
    }
}

/// Refusals decided from the request alone: calls that need the turn loop
/// itself or their own approval card stay direct. Checked on the name the
/// program sent, and again by the turn loop on the name planning resolved it
/// to (`Agent` resolves to `agent`) and the final, hook-rewritten input.
///
/// Names compare ASCII case-insensitively: dispatch resolves `Agent` to
/// `agent`, so a case-sensitive list would be a bypass, not a policy.
pub(crate) fn refusal_before_gate(name: &str, input: &Value, gated: bool) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if PROHIBITED_NESTED.contains(&lower.as_str())
        || (!gated && crate::core::engine::tool_catalog::is_tool_search_tool(&lower))
    {
        return Some(format!(
            "`{name}` is not available inside execute_tools programs; call it directly (use workflow/task() for fan-out)"
        ));
    }
    if matches!(lower.as_str(), "bash" | "exec_shell")
        && input.get("interactive").and_then(Value::as_bool) == Some(true)
    {
        return Some(format!(
            "`{name}` with interactive:true needs the terminal; call it directly"
        ));
    }
    if input.get("sandbox_permissions").is_some() {
        return Some(format!(
            "`{name}` requests a sandbox escalation, which needs its own exact-call approval; call it directly"
        ));
    }
    if crate::tools::approval_cache::computer_use_user_gate(name, input).is_some()
        || crate::tools::approval_cache::computer_use_batch_hidden_gate(name, input).is_some()
    {
        return Some(format!(
            "Computer Use call `{name}` grants consent or runs a script; it needs its own approval card, so call it directly and let the user decide"
        ));
    }
    if McpPool::is_mcp_tool(name)
        && name.ends_with(&format!("_{}", crate::mcp::AUTHENTICATE_TOOL_NAME))
    {
        return Some(format!("`{name}` starts an MCP sign-in; call it directly"));
    }
    None
}

// ---------------------------------------------------------------------------
// Receipts
// ---------------------------------------------------------------------------

/// What was cut from an oversized value, and where the whole value went.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
struct Truncation {
    original_bytes: usize,
    kept_bytes: usize,
    spill_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum CallStatus {
    /// Started and not yet finished. Left in place when the run ends early,
    /// so the receipt says the call may have partially run.
    InFlight,
    Ok,
    Failed,
    Refused,
}

/// One nested call, as recorded by the host — not the script. The receipt is
/// what makes "no failures found" distinguishable from "nothing ran".
#[derive(Debug, Clone, serde::Serialize)]
struct CallReceipt {
    seq: usize,
    tool: String,
    decision: Option<NestedDecision>,
    status: CallStatus,
    ok: bool,
    elapsed_ms: u64,
    bytes: usize,
    truncated: Option<Truncation>,
    note: Option<String>,
    /// `additionalContext` a tool_call_before hook attached to this call,
    /// the same text a direct call appends to its result (#3026).
    #[serde(skip_serializing_if = "Option::is_none")]
    hook_context: Option<String>,
}

/// Run clock that stops while the program waits on the gate, so a person
/// taking a minute on an approval card does not spend the program's budget.
/// An extension tool's `tool/call` deadline runs on the same clock
/// (`extension_host::supervisor::HostProcess::call_with_clock`), paused while
/// one of its `core/call`s waits on the gate.
pub(crate) struct PauseClock {
    started: Instant,
    paused: Duration,
    depth: usize,
    since: Option<Instant>,
}

impl PauseClock {
    pub(crate) fn new() -> Self {
        Self {
            started: Instant::now(),
            paused: Duration::ZERO,
            depth: 0,
            since: None,
        }
    }

    fn pause(&mut self) {
        if self.depth == 0 {
            self.since = Some(Instant::now());
        }
        self.depth += 1;
    }

    fn resume(&mut self) {
        self.depth = self.depth.saturating_sub(1);
        if self.depth == 0
            && let Some(since) = self.since.take()
        {
            self.paused += since.elapsed();
        }
    }

    fn active(&self) -> Duration {
        let paused = self.paused + self.since.map_or(Duration::ZERO, |since| since.elapsed());
        self.started.elapsed().saturating_sub(paused)
    }

    /// Time left before `deadline`, or `None` once it has passed. While the
    /// clock is paused its budget is frozen, so the caller looks again shortly
    /// ([`PAUSED_WATCHDOG_POLL`]): sleeping out the whole deadline would let it
    /// overrun by that much once the pause ends.
    pub(crate) fn remaining(&self, deadline: Duration) -> Option<Duration> {
        if self.depth > 0 {
            return Some(PAUSED_WATCHDOG_POLL.min(deadline));
        }
        deadline
            .checked_sub(self.active())
            .filter(|left| !left.is_zero())
    }
}

struct PauseGuard<'a>(&'a Mutex<PauseClock>);

impl Drop for PauseGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut clock) = self.0.lock() {
            clock.resume();
        }
    }
}

/// Abort a spawned nested call when the program stops waiting for it.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// ---------------------------------------------------------------------------
// Invoker
// ---------------------------------------------------------------------------

/// [`ToolInvoker`] over a snapshot of the parent turn's registry.
///
/// The snapshot (a registry of the spec Arcs under a cloned [`ToolContext`])
/// is taken at dispatch so the invoker is `'static` for the VM thread. Nested
/// calls run on the engine's runtime (captured at dispatch), not the VM
/// thread's.
pub(crate) struct CodemodeInvoker {
    registry: Arc<ToolRegistry>,
    context: ToolContext,
    gate: Option<NestedCallGate>,
    runtime: Option<tokio::runtime::Handle>,
    semaphore: Arc<Semaphore>,
    /// Non-parallel nested calls take this exclusively; parallel-safe ones
    /// share it, mirroring how the engine schedules direct calls.
    order: RwLock<()>,
    receipts: Mutex<Vec<CallReceipt>>,
    clock: Arc<Mutex<PauseClock>>,
    spill_prefix: String,
    /// Refusals beyond code mode's own, decided from the request alone before
    /// the gate is asked (an extension's list: `extension_host::core_call`).
    extra_refusal: Option<ExtraRefusal>,
}

/// A refusal decided from the specs, the tool name and its input.
pub(crate) type ExtraRefusal = fn(&[Arc<dyn ToolSpec>], &str, &Value) -> Option<String>;

/// Why a gated call did not produce a result. The decision is what the gate
/// recorded; a caller that is not code mode maps it to its own error codes.
#[derive(Debug)]
pub(crate) enum NestedFailure {
    /// Refused or denied: nothing ran.
    Rejected {
        decision: NestedDecision,
        message: String,
    },
    /// The seam broke, the call timed out or was cancelled.
    Unavailable(String),
}

impl From<NestedFailure> for DriverError {
    fn from(failure: NestedFailure) -> Self {
        match failure {
            NestedFailure::Rejected { message, .. } => DriverError::Rejected(message),
            NestedFailure::Unavailable(message) => DriverError::Unavailable(message),
        }
    }
}

impl CodemodeInvoker {
    fn new(specs: Vec<Arc<dyn ToolSpec>>, context: ToolContext) -> Self {
        let mut context = context;
        let gate = context.execution.nested_call_gate.take();
        let spill_prefix = context
            .origin_tool_call_id
            .clone()
            .unwrap_or_else(|| EXECUTE_TOOLS_TOOL_NAME.to_string());
        Self::with_gate(specs, context, gate, spill_prefix, None)
    }

    /// The machinery for an extension tool's `core/call`s: the same gate,
    /// executor, receipts, concurrency cap and pausable clock as a program's
    /// nested calls, with `extra_refusal` added to what is refused outright.
    /// `context` is the extension tool's own; its gate is the one given here,
    /// which the tool's calls then never see again.
    pub(crate) fn for_extension(
        specs: Vec<Arc<dyn ToolSpec>>,
        mut context: ToolContext,
        gate: NestedCallGate,
        spill_prefix: String,
        extra_refusal: ExtraRefusal,
    ) -> Self {
        context.execution.nested_call_gate = None;
        Self::with_gate(
            specs,
            context,
            Some(gate),
            spill_prefix,
            Some(extra_refusal),
        )
    }

    fn with_gate(
        specs: Vec<Arc<dyn ToolSpec>>,
        context: ToolContext,
        gate: Option<NestedCallGate>,
        spill_prefix: String,
        extra_refusal: Option<ExtraRefusal>,
    ) -> Self {
        let mut registry = ToolRegistry::new(context.clone());
        registry.register_all(specs);
        Self {
            registry: Arc::new(registry),
            context,
            gate,
            runtime: tokio::runtime::Handle::try_current().ok(),
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_CALLS)),
            order: RwLock::new(()),
            receipts: Mutex::new(Vec::new()),
            clock: Arc::new(Mutex::new(PauseClock::new())),
            spill_prefix,
            extra_refusal,
        }
    }

    fn deadline(&self) -> Duration {
        self.gate
            .as_ref()
            .map_or(FALLBACK_RUN_DEADLINE, |gate| gate.deadline)
    }

    fn pause(&self) -> PauseGuard<'_> {
        if let Ok(mut clock) = self.clock.lock() {
            clock.pause();
        }
        PauseGuard(&self.clock)
    }

    /// Time left before the deadline ([`PauseClock::remaining`]).
    fn remaining(&self, deadline: Duration) -> Option<Duration> {
        self.clock.lock().ok()?.remaining(deadline)
    }

    /// The clock the run's deadline is measured on, for a caller whose own
    /// deadline must stop while this run waits on the gate.
    pub(crate) fn clock(&self) -> Arc<Mutex<PauseClock>> {
        Arc::clone(&self.clock)
    }

    fn begin(&self, tool: &str) -> usize {
        let Ok(mut receipts) = self.receipts.lock() else {
            return 0;
        };
        let seq = receipts.len() + 1;
        receipts.push(CallReceipt {
            seq,
            tool: tool.to_string(),
            decision: None,
            status: CallStatus::InFlight,
            ok: false,
            elapsed_ms: 0,
            bytes: 0,
            truncated: None,
            note: None,
            hook_context: None,
        });
        seq
    }

    fn finish(&self, seq: usize, update: impl FnOnce(&mut CallReceipt)) {
        if let Ok(mut receipts) = self.receipts.lock()
            && let Some(receipt) = receipts.iter_mut().find(|receipt| receipt.seq == seq)
        {
            update(receipt);
        }
    }

    fn drain(&self) -> Vec<CallReceipt> {
        self.receipts
            .lock()
            .map(|receipts| receipts.clone())
            .unwrap_or_default()
    }

    fn refused(
        &self,
        seq: usize,
        started: Instant,
        decision: NestedDecision,
        note: String,
    ) -> NestedFailure {
        self.finish(seq, |receipt| {
            receipt.decision = Some(decision);
            receipt.status = CallStatus::Refused;
            receipt.elapsed_ms = started.elapsed().as_millis() as u64;
            receipt.note = Some(note.clone());
        });
        NestedFailure::Rejected {
            decision,
            message: note,
        }
    }

    /// Admission when no engine gate serves the program.
    fn ungated_admission(&self, name: &str, input: &Value) -> Result<(), String> {
        if McpPool::is_mcp_tool(name) {
            return Err(format!(
                "`{name}` is an MCP tool; no engine turn serves this program's session permission gate, so call it directly"
            ));
        }
        let Some(spec) = self.registry.get(name) else {
            return Err(format!(
                "unknown tool `{name}`; discover names with tool_search before writing the program"
            ));
        };
        if !spec.is_read_only_for(input) {
            return Err(format!(
                "`{name}` can mutate; without a session permission gate code mode executes read-only calls only"
            ));
        }
        if spec.approval_requirement_for(input) != ApprovalRequirement::Auto {
            return Err(format!(
                "`{name}` needs approval; without a session permission gate code mode executes only auto-approved calls"
            ));
        }
        enforce_tool_authority(name, input, spec.as_ref(), &self.context)
            .map_err(|err| err.to_string())
    }

    /// Execute an admitted call on the engine runtime: MCP through the
    /// session pool (same dispatcher as a direct call), everything else
    /// through the registry under the turn's authority envelope. Either way a
    /// sub-agent's call is first admitted against its grant, as its direct
    /// call is.
    async fn execute(&self, name: &str, input: Value) -> Result<RichToolResult, ToolError> {
        use crate::core::engine::tool_execution::OperationSpanGuard;
        let action_id = super::activity::action_id(name, &input);
        let mcp = if McpPool::is_mcp_tool(name) {
            let Some(gate) = self.gate.as_ref() else {
                return Err(ToolError::not_available(format!(
                    "MCP tool `{name}` needs the session MCP pool"
                )));
            };
            let Some(pool) = gate.mcp_pool.clone() else {
                return Err(ToolError::not_available(format!(
                    "MCP is not connected for this turn, so `{name}` cannot run"
                )));
            };
            Some((pool, gate.tx_event.clone()))
        } else {
            None
        };
        let activity_kind = match mcp.as_ref() {
            Some((pool, _)) => pool
                .lock()
                .await
                .resolved_tool_servers()
                .get(name)
                .map(|server| super::activity::mcp_activity_kind(server)),
            None => super::activity::registry_activity_kind(name, &input),
        };
        let registry = Arc::clone(&self.registry);
        let context = self.context.clone();
        let name = name.to_string();
        let tx = self.gate.as_ref().map(|gate| gate.tx_event.clone());
        let cancel = self.context.cancel_token.clone();
        let call_id = self
            .context
            .origin_tool_call_id
            .clone()
            .unwrap_or_else(|| self.spill_prefix.clone());
        let future = async move {
            if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
                return Err(ToolError::cancelled("Nested dispatch admission cancelled."));
            }
            let span = match (tx.as_ref(), activity_kind) {
                (Some(tx), Some(kind)) => Some(
                    OperationSpanGuard::reserve(
                        tx,
                        &call_id,
                        kind,
                        Some(action_id),
                        cancel.clone(),
                    )
                    .await,
                ),
                _ => None,
            };
            if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
                return Err(ToolError::cancelled("Nested dispatch admission cancelled."));
            }
            let Some((pool, tx_event)) = mcp else {
                return registry
                    .execute_rich_full_with_context(&name, input, Some(&context), || {
                        Ok(span.map(OperationSpanGuard::activate))
                    })
                    .await;
            };
            let child = registry.admit_child_call(&name, &input, &context).await?;
            let span = span.map(OperationSpanGuard::activate);
            let outcome = crate::core::engine::Engine::execute_mcp_tool_with_pool(
                pool,
                &tx_event,
                &name,
                input,
                &context.disallowed_tools,
                // Calls a program makes never carry a person's decision.
                None,
            )
            .await;
            if let Some(span) = span {
                span.complete(super::activity::operation_outcome(
                    &outcome,
                    cancel.as_ref().is_some_and(CancellationToken::is_cancelled),
                ))
                .await;
            }
            if outcome.as_ref().is_ok_and(|result| result.result.success)
                && let Some((authority, writes)) = child
            {
                authority.record_settled_writes(writes).await;
            }
            outcome
        };
        match self.runtime.as_ref() {
            Some(runtime) => {
                let mut task = AbortOnDrop(runtime.spawn(future));
                match (&mut task.0).await {
                    Ok(result) => result,
                    Err(err) => Err(ToolError::execution_failed(format!(
                        "nested call did not complete: {err}"
                    ))),
                }
            }
            None => future.await,
        }
    }

    /// Shape one finished call for the program and complete its receipt.
    fn deliver(
        &self,
        seq: usize,
        started: Instant,
        name: &str,
        decision: NestedDecision,
        outcome: Result<RichToolResult, ToolError>,
    ) -> Result<ToolCallResponse, NestedFailure> {
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match outcome {
            Ok(rich) => {
                let result = rich.into_result();
                let success = result.success;
                let (payload, raw_len, truncated) = if success {
                    self.bounded_envelope(seq, name, &result)
                } else {
                    let raw_len = result.content.len();
                    let (text, truncated) = bound_text(
                        result.content,
                        PER_CALL_RESULT_CAP_BYTES,
                        &self.spill_id(seq, name),
                    );
                    (text, raw_len, truncated)
                };
                self.finish(seq, |receipt| {
                    receipt.decision = Some(decision);
                    receipt.status = if success {
                        CallStatus::Ok
                    } else {
                        CallStatus::Failed
                    };
                    receipt.ok = success;
                    receipt.elapsed_ms = elapsed_ms;
                    receipt.bytes = raw_len;
                    receipt.truncated = truncated;
                });
                Ok(ToolCallResponse {
                    ok: success,
                    result: payload,
                })
            }
            Err(err) => {
                let message = err.to_string();
                // Validation-shaped failures mean nothing ran (admission);
                // execution failures ran and failed (agent kind via ok:false);
                // seam breaks are unavailable.
                match err {
                    ToolError::InvalidInput { .. }
                    | ToolError::MissingField { .. }
                    | ToolError::PathEscape { .. }
                    | ToolError::PermissionDenied { .. } => {
                        Err(self.refused(seq, started, decision, message))
                    }
                    ToolError::Timeout { .. }
                    | ToolError::Cancelled { .. }
                    | ToolError::NotAvailable { .. } => {
                        self.finish(seq, |receipt| {
                            receipt.decision = Some(decision);
                            receipt.status = CallStatus::Failed;
                            receipt.elapsed_ms = elapsed_ms;
                            receipt.note = Some(message.clone());
                        });
                        Err(NestedFailure::Unavailable(message))
                    }
                    ToolError::ExecutionFailed { .. } => {
                        let bytes = message.len();
                        let (text, truncated) = bound_text(
                            message,
                            PER_CALL_RESULT_CAP_BYTES,
                            &self.spill_id(seq, name),
                        );
                        self.finish(seq, |receipt| {
                            receipt.decision = Some(decision);
                            receipt.status = CallStatus::Failed;
                            receipt.elapsed_ms = elapsed_ms;
                            receipt.bytes = bytes;
                            receipt.truncated = truncated;
                        });
                        Ok(ToolCallResponse {
                            ok: false,
                            result: text,
                        })
                    }
                }
            }
        }
    }

    /// Spillover id for one nested call's full output.
    fn spill_id(&self, seq: usize, name: &str) -> String {
        format!("{}-nested-{seq}-{name}", self.spill_prefix)
    }

    /// Stable envelope: the tool's text content (parsed as JSON when it is
    /// JSON), its structured metadata, and `truncated` (null unless cut).
    /// An oversized result keeps the same keys: `content` becomes the head
    /// of the raw text (no longer parsed JSON) and `truncated` says how much
    /// there was and where the whole output was saved.
    fn bounded_envelope(
        &self,
        seq: usize,
        name: &str,
        result: &ToolResult,
    ) -> (Value, usize, Option<Truncation>) {
        let metadata = result.metadata.clone().unwrap_or(Value::Null);
        let content = serde_json::from_str(&result.content)
            .unwrap_or_else(|_| Value::String(result.content.clone()));
        let payload = json!({ "content": content, "metadata": metadata, "truncated": Value::Null });
        let raw = payload.to_string();
        if raw.len() <= PER_CALL_RESULT_CAP_BYTES {
            return (payload, raw.len(), None);
        }
        let spill_path = spill(&self.spill_id(seq, name), &raw);
        let metadata_len = metadata.to_string().len();
        let metadata = if metadata_len <= PER_CALL_RESULT_CAP_BYTES / 4 {
            metadata
        } else {
            Value::Null
        };
        let head = char_prefix(&result.content, PER_CALL_RESULT_CAP_BYTES / 2);
        let truncation = Truncation {
            original_bytes: raw.len(),
            kept_bytes: head.len(),
            spill_path,
        };
        (
            json!({
                "content": head,
                "metadata": metadata,
                "truncated": truncation,
            }),
            raw.len(),
            Some(truncation),
        )
    }
}

/// The receipts' bounds when they are attached to another tool's result.
const RECEIPT_NOTE_BYTES: usize = 256;

impl CodemodeInvoker {
    /// The receipts of the calls so far as JSON, at most `max` of them with
    /// each note cut short: what an extension tool's result records about the
    /// core calls it made. The count says how many there were in all.
    pub(crate) fn receipts_json(&self, max: usize) -> Value {
        let receipts = self.drain();
        let total = receipts.len();
        let shown: Vec<CallReceipt> = receipts
            .into_iter()
            .take(max)
            .map(|mut receipt| {
                receipt.note = receipt
                    .note
                    .map(|note| char_prefix(&note, RECEIPT_NOTE_BYTES));
                receipt.hook_context = None;
                receipt
            })
            .collect();
        json!({ "total": total, "calls": shown })
    }

    /// One gated call: refuse it outright if it is on a refusal list, ask the
    /// engine's gate (the program is paused meanwhile), then run it and shape
    /// the answer, recording a receipt throughout. `withdraw` stops the wait
    /// for a slot, the gate's wait on a person, and the execution.
    pub(crate) async fn call(
        &self,
        tool: String,
        input: Value,
        withdraw: Option<&CancellationToken>,
    ) -> Result<ToolCallResponse, NestedFailure> {
        let permit = self.semaphore.clone().acquire_owned();
        let _permit = match withdraw {
            None => permit.await,
            Some(withdraw) => tokio::select! {
                biased;
                () = withdraw.cancelled() => {
                    return Err(NestedFailure::Unavailable("the call was withdrawn".to_string()));
                }
                permit = permit => permit,
            },
        }
        .map_err(|_| NestedFailure::Unavailable("code-mode run shut down".to_string()))?;
        let started = Instant::now();
        let seq = self.begin(&tool);

        if let Some(note) = refusal_before_gate(&tool, &input, self.gate.is_some()).or_else(|| {
            self.extra_refusal
                .and_then(|refuse| refuse(&self.registry.all(), &tool, &input))
        }) {
            return Err(self.refused(seq, started, NestedDecision::Refused, note));
        }

        let (name, input, supports_parallel, decision, hook_context) = match self.gate.as_ref() {
            Some(gate) => {
                let verdict = {
                    let _paused = self.pause();
                    gate.ask_withdrawable(tool.clone(), input, withdraw.cloned())
                        .await
                };
                match verdict {
                    NestedCallVerdict::Run {
                        name,
                        input,
                        supports_parallel,
                        decision,
                        hook_context,
                    } => (name, input, supports_parallel, decision, hook_context),
                    NestedCallVerdict::Answered {
                        result,
                        hook_context,
                    } => {
                        self.finish(seq, |receipt| receipt.hook_context = hook_context);
                        return self.deliver(
                            seq,
                            started,
                            &tool,
                            NestedDecision::Auto,
                            Ok(RichToolResult::plain(result)),
                        );
                    }
                    NestedCallVerdict::Refused { error, decision } => {
                        return Err(self.refused(seq, started, decision, error.to_string()));
                    }
                }
            }
            None => {
                if let Err(note) = self.ungated_admission(&tool, &input) {
                    return Err(self.refused(seq, started, NestedDecision::Refused, note));
                }
                (tool.clone(), input, true, NestedDecision::Auto, None)
            }
        };
        self.finish(seq, |receipt| {
            if name != tool {
                receipt.tool = name.clone();
            }
            receipt.hook_context = hook_context;
        });

        let run = async {
            if supports_parallel {
                let _shared = self.order.read().await;
                self.execute(&name, input).await
            } else {
                let _exclusive = self.order.write().await;
                self.execute(&name, input).await
            }
        };
        let outcome = match withdraw {
            None => run.await,
            Some(withdraw) => tokio::select! {
                biased;
                () = withdraw.cancelled() => {
                    Err(ToolError::cancelled("the call was withdrawn while it ran"))
                }
                outcome = run => outcome,
            },
        };
        self.deliver(seq, started, &name, decision, outcome)
    }
}

#[async_trait]
impl ToolInvoker for CodemodeInvoker {
    async fn invoke(&self, request: ToolCallRequest) -> Result<ToolCallResponse, DriverError> {
        let ToolCallRequest { tool, input } = request;
        self.call(tool, input, None)
            .await
            .map_err(DriverError::from)
    }
}

/// [`WorkflowDriver`] for code-mode runs: `task()` is refused (fan-out stays
/// with `workflow`), the token budget is unconstrained, and progress events
/// feed the run receipt.
pub(crate) struct CodemodeDriver {
    events: Mutex<Vec<ProgressEvent>>,
}

impl Default for CodemodeDriver {
    fn default() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }
}

impl CodemodeDriver {
    fn log_lines(&self) -> Vec<String> {
        self.events
            .lock()
            .map(|events| {
                events
                    .iter()
                    .filter_map(|event| match event {
                        ProgressEvent::Log { message } => Some(message.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[async_trait]
impl WorkflowDriver for CodemodeDriver {
    async fn spawn_task(&self, _request: TaskRequest) -> Result<SpawnedTask, DriverError> {
        Err(DriverError::Rejected(
            "task() is unavailable in execute_tools programs; tools.call() composes direct tool calls"
                .to_string(),
        ))
    }

    fn budget(&self) -> BudgetSnapshot {
        BudgetSnapshot {
            total: None,
            spent: 0,
        }
    }

    fn progress(&self, event: ProgressEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }

    fn cancel_all(&self) {}
}

/// Longest prefix of `text` within `max_bytes` that ends on a char boundary.
fn char_prefix(text: &str, max_bytes: usize) -> String {
    let cut = max_bytes.min(text.len());
    let cut = (0..=cut)
        .rev()
        .find(|&index| text.is_char_boundary(index))
        .unwrap_or(0);
    text[..cut].to_string()
}

/// Save a full value through the session spillover store; `None` when the
/// store is unavailable (the truncation is still reported).
fn spill(id: &str, content: &str) -> Option<String> {
    crate::tools::truncate::write_spillover(id, content)
        .map(|path| path.display().to_string())
        .map_err(|error| {
            tracing::warn!(target: "codemode", %error, "nested result spillover failed");
        })
        .ok()
}

/// Bound the program's return value to `cap` serialized bytes. An oversized
/// value becomes the head of its JSON text, and the truncation record says so.
fn bound_json(value: Value, cap: usize, spill_id: &str) -> (Value, Option<Truncation>) {
    let raw = value.to_string();
    if raw.len() <= cap {
        return (value, None);
    }
    bound_text(raw, cap, spill_id)
}

/// Bound `text` to `cap` bytes: an oversized text becomes its head, the
/// whole text is saved, and the truncation record says so.
fn bound_text(text: String, cap: usize, spill_id: &str) -> (Value, Option<Truncation>) {
    if text.len() <= cap {
        return (Value::String(text), None);
    }
    let head = char_prefix(&text, cap / 2);
    let truncation = Truncation {
        original_bytes: text.len(),
        kept_bytes: head.len(),
        spill_path: spill(spill_id, &text),
    };
    (Value::String(head), Some(truncation))
}

fn receipt_payload(
    success: bool,
    body: Value,
    invoker: &CodemodeInvoker,
    driver: &CodemodeDriver,
) -> ToolResult {
    let calls = invoker.drain();
    let ran = calls.iter().any(|call| call.status != CallStatus::Refused);
    let content = json!({
        "success": success,
        "body": body,
        "nested_calls": calls.len(),
        "calls": calls,
        "log": driver.log_lines(),
    })
    .to_string();
    ToolResult {
        content,
        success,
        metadata: (!ran).then(|| json!({ "executed": false })),
    }
}

/// Execute one `execute_tools` call: validate, run the program under its
/// deadline, and return the bounded program value plus the host-owned
/// receipt. A script failure or a deadline is a `success: false` payload
/// that still lists every nested call — only VM setup failures are `Err`.
pub async fn execute_tools_tool(
    input: &Value,
    registry: &ToolRegistry,
    context: &ToolContext,
) -> Result<ToolResult, ToolError> {
    let code = required_str(input, "code")?;
    if code.trim().is_empty() {
        return Err(ToolError::missing_field("code"));
    }
    if code.len() > MAX_CODE_BYTES {
        return Err(ToolError::invalid_input(format!(
            "code exceeds {MAX_CODE_BYTES} bytes"
        )));
    }
    let invoker = Arc::new(CodemodeInvoker::new(registry.all(), context.clone()));
    let driver = Arc::new(CodemodeDriver::default());
    let deadline = invoker.deadline();
    let cancel = WorkflowRunCancel::new();
    let vm = WorkflowVm::new();
    let mut run = Box::pin(vm.run_tools_script(
        code,
        Value::Null,
        driver.clone(),
        invoker.clone(),
        cancel.clone(),
    ));
    let watchdog = async {
        loop {
            let Some(remaining) = invoker.remaining(deadline) else {
                return Err(format!(
                    "its {}s run deadline (time waiting on approvals is not counted)",
                    deadline.as_secs()
                ));
            };
            tokio::select! {
                result = &mut run => return Ok(result),
                () = tokio::time::sleep(remaining) => {}
            }
        }
    };
    let outcome = match context.child_host.as_ref() {
        Some(child) => child
            .run_tool_bounded(async { Ok::<_, ToolError>(watchdog.await) })
            .await
            .unwrap_or_else(|_| {
                Err("the sub-agent's tool timeout (time waiting on approvals is not counted) or work deadline".to_string())
            }),
        None => watchdog.await,
    };
    let program_result = match outcome {
        Err(stopped_at) => {
            // Stop the VM (and with it every in-flight nested call) before
            // reading the receipt, so the listed state is final.
            cancel.cancel();
            drop(run);
            return Ok(receipt_payload(
                false,
                json!({
                    "error": format!(
                        "execute_tools stopped at {stopped_at}. Calls with status ok/failed/refused finished; calls still in_flight were cancelled and may have partially run."
                    ),
                    "timed_out": true,
                }),
                &invoker,
                &driver,
            ));
        }
        Ok(Err(err)) => {
            return Ok(receipt_payload(
                false,
                json!({ "error": err.to_string() }),
                &invoker,
                &driver,
            ));
        }
        Ok(Ok(value)) => value,
    };
    let spill_id = format!("{}-return", invoker.spill_prefix);
    let (bounded, truncated) = bound_json(program_result, RETURN_CAP_BYTES, &spill_id);
    Ok(receipt_payload(
        true,
        json!({ "return": bounded, "return_truncated": truncated }),
        &invoker,
        &driver,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_advertises_execute_tools_deferred_outside_plan() {
        use codewhale_config::AppMode;
        use std::collections::HashSet;
        let empty = HashSet::new();
        for mode in [AppMode::Agent, AppMode::Operate] {
            let mut catalog = Vec::new();
            crate::core::engine::tool_catalog::ensure_advanced_tooling(
                &mut catalog,
                mode,
                &empty,
                ToolMode::Direct,
            );
            let tool = catalog
                .iter()
                .find(|tool| tool.name == EXECUTE_TOOLS_TOOL_NAME)
                .unwrap_or_else(|| panic!("{mode:?} catalog must advertise execute_tools"));
            assert_eq!(tool.defer_loading, Some(true), "{mode:?} must defer it");
        }
        let mut catalog = Vec::new();
        crate::core::engine::tool_catalog::ensure_advanced_tooling(
            &mut catalog,
            AppMode::Plan,
            &empty,
            ToolMode::Direct,
        );
        assert!(
            catalog
                .iter()
                .all(|tool| tool.name != EXECUTE_TOOLS_TOOL_NAME)
        );
    }

    #[test]
    fn definition_is_deferred_direct_only() {
        let tool = execute_tools_tool_definition();
        assert_eq!(tool.name, EXECUTE_TOOLS_TOOL_NAME);
        assert_eq!(tool.tool_type.as_deref(), Some(EXECUTE_TOOLS_TOOL_TYPE));
        assert!(tool.description.contains("tools.call"));
        // Catalog decides deferral; the name is absent from the eager set,
        // so injection marks it deferred like the interpreter tools.
        assert!(
            !crate::core::engine::tool_catalog::DEFAULT_ACTIVE_NATIVE_TOOLS
                .contains(&tool.name.as_str())
        );
        assert_eq!(tool.allowed_callers, Some(vec!["direct".to_string()]));
    }

    #[test]
    fn bound_json_keeps_small_values_verbatim() {
        let (value, truncated) = bound_json(json!({"a": 1}), 1024, "bound-small");
        assert!(truncated.is_none());
        assert_eq!(value, json!({"a": 1}));
    }

    #[test]
    fn bound_json_names_what_it_cut() {
        let _guard = crate::tools::truncate::TEST_SPILLOVER_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let spill_root = tempfile::tempdir().unwrap();
        let previous =
            crate::tools::truncate::set_test_spillover_root(Some(spill_root.path().to_path_buf()));
        let big = "x".repeat(100);
        let (value, truncated) = bound_json(json!({ "blob": big }), 64, "bound-big");
        crate::tools::truncate::set_test_spillover_root(previous);
        let truncated = truncated.expect("oversized value is reported");
        assert_eq!(truncated.original_bytes, 111);
        assert_eq!(truncated.kept_bytes, 32);
        assert!(
            value
                .as_str()
                .is_some_and(|head| head.starts_with("{\"blob\""))
        );
        let saved = truncated.spill_path.expect("full value saved");
        assert_eq!(std::fs::read_to_string(saved).unwrap().len(), 111);
    }

    use crate::core::engine::tool_catalog::ToolMode;
    use crate::tools::file_tool::{ReadTool, WriteTool};
    use crate::tools::registry::ToolRegistryBuilder;

    fn workspace_with_note() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("note.txt");
        std::fs::write(&note, "alpha\nbeta\n").unwrap();
        (dir, note)
    }

    #[tokio::test]
    async fn nested_read_passes_gates_and_records_receipt() {
        let (_dir, note) = workspace_with_note();
        let context = ToolContext::new(note.parent().unwrap());
        let invoker = CodemodeInvoker::new(vec![Arc::new(ReadTool)], context);
        let response = invoker
            .invoke(ToolCallRequest {
                tool: "read".to_string(),
                input: json!({ "path": note.to_string_lossy() }),
            })
            .await
            .unwrap();
        assert!(response.ok);
        assert!(response.result.to_string().contains("alpha"));
        let receipts = invoker.drain();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].tool, "read");
        assert!(receipts[0].ok);
    }

    #[tokio::test]
    async fn nested_write_is_refused_and_writes_nothing() {
        let (_dir, note) = workspace_with_note();
        let target = note.parent().unwrap().join("evil.txt");
        let context = ToolContext::new(note.parent().unwrap());
        let invoker = CodemodeInvoker::new(vec![Arc::new(WriteTool)], context);
        let err = invoker
            .invoke(ToolCallRequest {
                tool: "write".to_string(),
                input: json!({ "path": target.to_string_lossy(), "content": "x" }),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DriverError::Rejected(message) if message.contains("read-only")),
            "unexpected: {err:?}"
        );
        assert!(!target.exists());
        assert_eq!(invoker.drain().len(), 1);
    }

    #[tokio::test]
    async fn prohibited_nested_names_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let context = ToolContext::new(dir.path());
        let invoker = CodemodeInvoker::new(vec![], context);
        for name in [
            "agent",
            "workflow",
            "rlm",
            "execute_tools",
            "tool_search",
            "nope",
        ] {
            let err = invoker
                .invoke(ToolCallRequest {
                    tool: name.to_string(),
                    input: json!({}),
                })
                .await
                .unwrap_err();
            assert!(matches!(err, DriverError::Rejected(_)), "{name}: {err:?}");
        }
    }

    #[tokio::test]
    async fn native_navigation_resolves_real_source_and_refuses_stale_or_escaped_reads() {
        let dir = tempfile::tempdir().unwrap();
        let source = "const LURE: &str = r#\"\nfn forged() {}\n\"#;\npub fn wanted() -> &'static str {\n    \"NAV_BODY\"\n}\nfn sibling() {}\n";
        std::fs::write(dir.path().join("nav.rs"), source).unwrap();
        let context = ToolContext::new(dir.path());
        let registry = ToolRegistryBuilder::new()
            .with_tool(Arc::new(ReadTool))
            .build(context.clone());
        let code = "const r = await tools.call('read', {path: 'nav.rs', limit: 1}); return r.metadata.navigation || null;";
        let result = execute_tools_tool(&json!({"code": code}), &registry, &context)
            .await
            .unwrap();
        assert!(result.success, "{}", result.content);
        let body: Value = serde_json::from_str(&result.content).unwrap();
        let navigation = &body["body"]["return"];
        assert_eq!(
            navigation["producer"], "rust_syntax_v1",
            "read must publish revision-qualified syntax evidence"
        );
        let symbols = navigation["symbols"].as_array().unwrap();
        assert!(
            !symbols.iter().any(|symbol| symbol["name"] == "forged"),
            "a function-looking raw string is not a declaration"
        );
        let reference = symbols
            .iter()
            .find(|symbol| symbol["name"] == "wanted")
            .unwrap()["read"]
            .clone();
        assert_eq!(reference["path"], "nav.rs");
        assert_eq!(reference["offset"], 4);
        assert_eq!(reference["limit"], 3);
        let wanted = ReadTool.execute(reference.clone(), &context).await.unwrap();
        assert!(
            wanted
                .content
                .starts_with("pub fn wanted() -> &'static str {\n    \"NAV_BODY\"\n}"),
            "{}",
            wanted.content
        );
        assert!(
            !wanted
                .content
                .split("\n\n[")
                .next()
                .unwrap()
                .contains("fn sibling")
        );

        let child = context.clone().with_state_namespace("navigation-child");
        let child_read = ReadTool.execute(reference.clone(), &child).await.unwrap();
        assert_eq!(
            child_read.metadata.as_ref().unwrap()["navigation"],
            *navigation,
            "shared physical knowledge is revision-stable, not parent delivery state"
        );
        let child_workspace = dir.path().join("child");
        std::fs::create_dir(&child_workspace).unwrap();
        let restricted =
            ToolContext::new(&child_workspace).with_runtime_services(context.runtime.clone());
        let mut escaped = reference.clone();
        escaped["path"] = json!("../nav.rs");
        assert!(
            matches!(
                ReadTool.execute(escaped, &restricted).await,
                Err(ToolError::PathEscape { .. })
            ),
            "a warmed shared cache must not grant workspace access"
        );

        std::fs::write(
            dir.path().join("nav.rs"),
            source.replace("NAV_BODY", "NAV_EDIT"),
        )
        .unwrap();
        let stale = ReadTool
            .execute(reference, &child)
            .await
            .expect_err("same-size external edit must invalidate the reference");
        assert!(stale.to_string().contains("stale"), "{stale}");
        let refreshed = ReadTool
            .execute(json!({"path": "nav.rs", "limit": 1}), &child)
            .await
            .unwrap();
        assert_ne!(
            refreshed.metadata.as_ref().unwrap()["navigation"]["source_revision"],
            navigation["source_revision"]
        );
    }

    #[tokio::test]
    async fn program_composes_nested_read_and_returns_receipt() {
        let (_dir, note) = workspace_with_note();
        let workspace = note.parent().unwrap().to_path_buf();
        let context = ToolContext::new(workspace.clone());
        let registry = ToolRegistryBuilder::new()
            .with_tool(Arc::new(ReadTool))
            .build(context.clone());
        let path = note.to_string_lossy().replace('\\', "\\\\");
        let code = format!(
            "const r = await tools.call('read', {{ path: '{path}' }}); return {{ hasAlpha: JSON.stringify(r).includes('alpha') }};"
        );
        let result = execute_tools_tool(&json!({ "code": code }), &registry, &context)
            .await
            .unwrap();
        assert!(result.success, "{}", result.content);
        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["nested_calls"], 1);
        assert_eq!(body["body"]["return"]["hasAlpha"], true);
    }

    #[tokio::test]
    async fn program_loads_skills_at_runtime_through_load_skill() {
        // Skills-as-tools composes with code mode: `load_skill` is
        // read-only and auto-approved, so a program can list and load
        // skills at runtime without widening its authority.
        // A configured skills dir, not a project root: project skills load
        // only in a trusted workspace, which this composition test is not
        // about.
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let skills_root = dir.path().join("configured-skills");
        let skill_dir = skills_root.join("greet");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: greet\ndescription: Say hello\n---\n# Greet\nSay hello warmly.\n",
        )
        .unwrap();
        let context = ToolContext::new(&workspace)
            .with_skills_config(&skills_root, crate::skills::SkillDiscoveryMode::Compatible);
        let registry = ToolRegistryBuilder::new()
            .with_tool(Arc::new(crate::tools::skill::LoadSkillTool))
            .build(context.clone());
        let code = "const list = await tools.call('load_skill', { name: 'list' }); \
             const body = await tools.call('load_skill', { name: 'greet' }); \
             return { listed: JSON.stringify(list).includes('greet'), \
             loaded: JSON.stringify(body).includes('warmly') };";
        let result = execute_tools_tool(&json!({ "code": code }), &registry, &context)
            .await
            .unwrap();
        assert!(result.success, "{}", result.content);
        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["nested_calls"], 2);
        assert_eq!(body["body"]["return"]["listed"], true);
        assert_eq!(body["body"]["return"]["loaded"], true);
    }

    // --- Gated (engine-served) programs -----------------------------------

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A stand-in for the turn loop's gate: answers every nested call with
    /// `answer` after `delay`, counting how often it was asked.
    fn gated_context(
        workspace: &std::path::Path,
        deadline: Duration,
        delay: Duration,
        mcp_pool: Option<Arc<AsyncMutex<McpPool>>>,
        answer: impl Fn(&str, &Value) -> NestedCallVerdict + Send + Sync + 'static,
    ) -> (ToolContext, Arc<AtomicUsize>) {
        let (tx_event, mut rx_event) = mpsc::channel(64);
        tokio::spawn(async move { while rx_event.recv().await.is_some() {} });
        let (gate, mut requests) = NestedCallGate::new(mcp_pool, tx_event, deadline);
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = asked.clone();
        tokio::spawn(async move {
            while let Some(request) = requests.recv().await {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(delay).await;
                let verdict = answer(&request.name, &request.input);
                let _ = request.reply.send(verdict);
            }
        });
        let mut context = ToolContext::new(workspace);
        context.execution.nested_call_gate = Some(gate);
        (context, asked)
    }

    fn run_as_asked(name: &str, input: &Value) -> NestedCallVerdict {
        NestedCallVerdict::Run {
            name: name.to_string(),
            input: input.clone(),
            supports_parallel: true,
            decision: NestedDecision::Auto,
            hook_context: None,
        }
    }

    fn body(result: &ToolResult) -> Value {
        serde_json::from_str(&result.content).expect("receipt is JSON")
    }

    /// A tool that takes longer than any test deadline.
    struct SlowTool;

    #[async_trait]
    impl ToolSpec for SlowTool {
        fn name(&self) -> &str {
            "slow"
        }
        fn description(&self) -> &str {
            "sleeps"
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
        fn capabilities(&self) -> Vec<crate::tools::spec::ToolCapability> {
            vec![crate::tools::spec::ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            _input: Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(ToolResult::success("late"))
        }
    }

    /// A tool whose output is larger than the nested result cap.
    struct HugeTool;

    #[async_trait]
    impl ToolSpec for HugeTool {
        fn name(&self) -> &str {
            "huge"
        }
        fn description(&self) -> &str {
            "big output"
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
        fn capabilities(&self) -> Vec<crate::tools::spec::ToolCapability> {
            vec![crate::tools::spec::ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            input: Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            let text = "y".repeat(PER_CALL_RESULT_CAP_BYTES * 2);
            if input.get("fail").and_then(Value::as_bool) == Some(true) {
                return Ok(ToolResult::error(text));
            }
            Ok(ToolResult::success(text))
        }
    }

    #[tokio::test]
    async fn gated_nested_mcp_call_runs_through_the_session_pool() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(AsyncMutex::new(McpPool::new(
            crate::mcp::McpConfig::default(),
        )));
        let (context, asked) = gated_context(
            dir.path(),
            Duration::from_secs(30),
            Duration::ZERO,
            Some(pool),
            run_as_asked,
        );
        let registry = ToolRegistryBuilder::new().build(context.clone());
        let code = "const r = await tools.call('list_mcp_resources', {}); \
                    return { hasContent: r.content !== undefined, truncated: r.truncated };";
        let result = execute_tools_tool(&json!({ "code": code }), &registry, &context)
            .await
            .unwrap();
        assert!(result.success, "{}", result.content);
        let body = body(&result);
        assert_eq!(body["body"]["return"]["hasContent"], true);
        assert_eq!(body["body"]["return"]["truncated"], Value::Null);
        assert_eq!(body["calls"][0]["tool"], "list_mcp_resources");
        assert_eq!(body["calls"][0]["decision"], "auto");
        assert_eq!(body["calls"][0]["status"], "ok");
        assert_eq!(
            asked.load(Ordering::SeqCst),
            1,
            "the MCP call went through the gate"
        );
    }

    #[tokio::test]
    async fn gated_denial_fails_only_that_nested_call() {
        let (_dir, note) = workspace_with_note();
        let workspace = note.parent().unwrap().to_path_buf();
        let target = workspace.join("denied.txt");
        let (context, _asked) = gated_context(
            &workspace,
            Duration::from_secs(30),
            Duration::ZERO,
            None,
            |name, input| {
                if name == "write" {
                    NestedCallVerdict::Refused {
                        error: ToolError::permission_denied("Tool 'write' denied by user"),
                        decision: NestedDecision::Denied,
                    }
                } else {
                    run_as_asked(name, input)
                }
            },
        );
        let registry = ToolRegistryBuilder::new()
            .with_tool(Arc::new(ReadTool))
            .with_tool(Arc::new(WriteTool))
            .build(context.clone());
        let note_path = note.to_string_lossy().replace('\\', "\\\\");
        let target_path = target.to_string_lossy().replace('\\', "\\\\");
        let code = format!(
            "let denied = null; \
             try {{ await tools.call('write', {{ path: '{target_path}', content: 'x' }}); }} \
             catch (e) {{ denied = String(e.message || e); }} \
             const r = await tools.call('read', {{ path: '{note_path}' }}); \
             return {{ denied, read: JSON.stringify(r).includes('alpha') }};"
        );
        let result = execute_tools_tool(&json!({ "code": code }), &registry, &context)
            .await
            .unwrap();
        assert!(result.success, "{}", result.content);
        let body = body(&result);
        assert!(
            body["body"]["return"]["denied"]
                .as_str()
                .is_some_and(|message| message.contains("denied by user")),
            "{body}"
        );
        assert_eq!(body["body"]["return"]["read"], true);
        assert_eq!(body["calls"][0]["decision"], "denied");
        assert_eq!(body["calls"][0]["status"], "refused");
        assert_eq!(body["calls"][1]["status"], "ok");
        assert!(!target.exists(), "a denied write never runs");
    }

    #[tokio::test]
    async fn computer_use_consent_is_refused_inside_a_program_before_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let (context, asked) = gated_context(
            dir.path(),
            Duration::from_secs(30),
            Duration::ZERO,
            None,
            run_as_asked,
        );
        let invoker = CodemodeInvoker::new(Vec::new(), context);
        for (name, input) in [
            (
                "mcp_plugin-12-computer-use-computer_consent",
                json!({"action": "allow", "app": "Safari", "bundle_id": "com.apple.Safari"}),
            ),
            (
                "mcp_plugin-12-computer-use-computer_app_script",
                json!({"script": "do shell script \"id\""}),
            ),
            ("mcp_github_authenticate", json!({})),
            ("exec_shell", json!({"command": "ls", "interactive": true})),
            ("request_user_input", json!({})),
            // Dispatch resolves names case-insensitively, so the refusals do.
            ("Agent", json!({})),
            ("WORKFLOW", json!({})),
            ("Execute_Tools", json!({"code": "return 1;"})),
            ("BASH", json!({"command": "ls", "interactive": true})),
            ("Exec_Shell", json!({"command": "ls", "interactive": true})),
        ] {
            let err = invoker
                .invoke(ToolCallRequest {
                    tool: name.to_string(),
                    input,
                })
                .await
                .unwrap_err();
            assert!(matches!(err, DriverError::Rejected(_)), "{name}: {err:?}");
        }
        assert_eq!(asked.load(Ordering::SeqCst), 0, "nothing reached the gate");
        assert!(
            invoker
                .drain()
                .iter()
                .all(|receipt| receipt.status == CallStatus::Refused)
        );
    }

    #[tokio::test]
    async fn deadline_keeps_finished_receipts_and_names_in_flight_calls() {
        let (_dir, note) = workspace_with_note();
        let workspace = note.parent().unwrap().to_path_buf();
        let (context, _asked) = gated_context(
            &workspace,
            Duration::from_millis(400),
            Duration::ZERO,
            None,
            run_as_asked,
        );
        let registry = ToolRegistryBuilder::new()
            .with_tool(Arc::new(ReadTool))
            .with_tool(Arc::new(SlowTool))
            .build(context.clone());
        let note_path = note.to_string_lossy().replace('\\', "\\\\");
        let code = format!(
            "await tools.call('read', {{ path: '{note_path}' }}); \
             await tools.call('slow', {{}}); return 'unreachable';"
        );
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            execute_tools_tool(&json!({ "code": code }), &registry, &context),
        )
        .await
        .expect("the run deadline ends the program")
        .expect("a deadline is a receipt, not a host error");
        assert!(!result.success);
        let body = body(&result);
        assert_eq!(body["body"]["timed_out"], true, "{body}");
        assert_eq!(body["nested_calls"], 2);
        assert_eq!(body["calls"][0]["tool"], "read");
        assert_eq!(body["calls"][0]["status"], "ok");
        assert_eq!(body["calls"][1]["tool"], "slow");
        assert_eq!(body["calls"][1]["status"], "in_flight");
    }

    #[tokio::test]
    async fn time_waiting_on_the_gate_does_not_spend_the_deadline() {
        let (_dir, note) = workspace_with_note();
        let workspace = note.parent().unwrap().to_path_buf();
        // Each gate answer takes longer than the whole run deadline, as a
        // person deciding an approval would.
        let (context, _asked) = gated_context(
            &workspace,
            Duration::from_millis(300),
            Duration::from_millis(500),
            None,
            run_as_asked,
        );
        let registry = ToolRegistryBuilder::new()
            .with_tool(Arc::new(ReadTool))
            .build(context.clone());
        let note_path = note.to_string_lossy().replace('\\', "\\\\");
        let code = format!(
            "const a = await tools.call('read', {{ path: '{note_path}' }}); \
             const b = await tools.call('read', {{ path: '{note_path}' }}); \
             return JSON.stringify([a, b]).includes('alpha');"
        );
        let result = execute_tools_tool(&json!({ "code": code }), &registry, &context)
            .await
            .unwrap();
        assert!(result.success, "{}", result.content);
        assert_eq!(body(&result)["body"]["return"], true);
    }

    #[test]
    fn watchdog_rechecks_promptly_while_paused_on_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let invoker = CodemodeInvoker::new(Vec::new(), ToolContext::new(dir.path()));
        let deadline = Duration::from_secs(600);
        let paused = invoker.pause();
        // A paused program never times out, but the watchdog must not sleep
        // out the whole deadline or the run would overrun by that much once
        // the gate answers.
        assert_eq!(invoker.remaining(deadline), Some(PAUSED_WATCHDOG_POLL));
        drop(paused);
        assert!(
            invoker
                .remaining(deadline)
                .is_some_and(|left| left > PAUSED_WATCHDOG_POLL)
        );
    }

    // This test deliberately serializes access to process-global spillover
    // state while awaiting the program.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn oversized_nested_result_keeps_its_envelope_and_says_what_was_cut() {
        let _guard = crate::tools::truncate::TEST_SPILLOVER_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let spill_root = tempfile::tempdir().unwrap();
        let previous =
            crate::tools::truncate::set_test_spillover_root(Some(spill_root.path().to_path_buf()));
        let dir = tempfile::tempdir().unwrap();
        let (context, _asked) = gated_context(
            dir.path(),
            Duration::from_secs(30),
            Duration::ZERO,
            None,
            run_as_asked,
        );
        let registry = ToolRegistryBuilder::new()
            .with_tool(Arc::new(HugeTool))
            .build(context.clone());
        let code = "const r = await tools.call('huge', {}); \
                    let failed = null; \
                    try { await tools.call('huge', { fail: true }); } \
                    catch (e) { failed = String(e.message || e).length; } \
                    return { keys: Object.keys(r).sort(), cut: r.truncated, \
                             head: r.content.length, kind: typeof r.content, failed };";
        let result = execute_tools_tool(&json!({ "code": code }), &registry, &context)
            .await
            .unwrap();
        crate::tools::truncate::set_test_spillover_root(previous);
        assert!(result.success, "{}", result.content);
        let body = body(&result);
        let returned = &body["body"]["return"];
        assert_eq!(
            returned["keys"],
            json!(["content", "metadata", "truncated"])
        );
        assert!(
            returned["cut"]["original_bytes"].as_u64().unwrap() > PER_CALL_RESULT_CAP_BYTES as u64
        );
        assert_eq!(returned["head"], json!(PER_CALL_RESULT_CAP_BYTES / 2));
        assert_eq!(returned["kind"], "string", "a cut result is its raw head");
        assert!(returned["cut"]["spill_path"].as_str().is_some(), "{body}");
        assert!(body["calls"][0]["truncated"]["original_bytes"].is_u64());
        // A failed call's text is bounded and spilled the same way.
        assert_eq!(returned["failed"], json!(PER_CALL_RESULT_CAP_BYTES / 2));
        assert_eq!(body["calls"][1]["status"], "failed");
        assert_eq!(
            body["calls"][1]["truncated"]["original_bytes"],
            json!(PER_CALL_RESULT_CAP_BYTES * 2)
        );
        assert!(
            body["calls"][1]["truncated"]["spill_path"]
                .as_str()
                .is_some(),
            "{body}"
        );
    }
}
