//! The sandboxed QuickJS VM that executes Workflow scripts.
//!
//! Threading model (design §2.2): `rquickjs` contexts and every `'js` value
//! are `!Send`, so each run gets a dedicated OS thread with its own
//! current-thread tokio reactor. Host functions do no heavy work inline —
//! only `Send` data (JSON strings, [`TaskRequest`]s, oneshot replies) crosses
//! to the driver; conversion back into JS values happens on the VM thread
//! after the await resolves.
//!
//! Sandbox: the context registers only standard ECMAScript intrinsics plus
//! the Workflow globals (`task`, `parallel`, `pipeline`, `log`, `phase`,
//! `budget`, `args`). There is no module loader, no fs/net/process access,
//! and `Date`/`Math.random` are overridden to throw so recorded runs stay
//! deterministic for replay.

use std::env;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use rquickjs::function::{Async, Func};
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, CaughtError, Ctx, Promise, Value};
use serde::Deserialize;
use tokio::runtime::Handle;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot, watch};
use tokio::task::JoinSet;

use crate::driver::{ProgressEvent, TaskCompletion, TaskRequest, WorkflowDriver};
use crate::error::{TaskError, TaskErrorKind, WorkflowJsError};
use crate::schema::{
    ReplyDecodeError, SCHEMA_REPAIR_MAX_ATTEMPTS, carried_raw, compile_schema, decode_reply,
    repair_prompt,
};
use crate::{PARALLEL_MAX_ITEMS, ToolCallRequest, ToolInvoker, normalize_profile};

const DEFAULT_VM_MEMORY_LIMIT_BYTES: usize = 32 * 1024 * 1024;
const MIN_VM_MEMORY_LIMIT_BYTES: usize = 4 * 1024 * 1024;
const MAX_VM_MEMORY_LIMIT_BYTES: usize = 512 * 1024 * 1024;
const DEFAULT_VM_STACK_BYTES: usize = 1024 * 1024;
const MIN_VM_STACK_BYTES: usize = 128 * 1024;
const MAX_VM_STACK_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_VM_THREAD_STACK_BYTES: usize = 2 * 1024 * 1024;
const MIN_VM_THREAD_STACK_BYTES: usize = 512 * 1024;
const MAX_VM_THREAD_STACK_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_CONCURRENT_VMS: usize = 4;
const MAX_CONCURRENT_VMS: usize = 256;

const VM_MEMORY_LIMIT_MB_ENV: &str = "CODEWHALE_WORKFLOW_JS_MEMORY_LIMIT_MB";
const VM_STACK_KB_ENV: &str = "CODEWHALE_WORKFLOW_JS_STACK_KB";
const VM_THREAD_STACK_KB_ENV: &str = "CODEWHALE_WORKFLOW_JS_THREAD_STACK_KB";
const VM_MAX_CONCURRENT_ENV: &str = "CODEWHALE_WORKFLOW_JS_MAX_CONCURRENT";

/// Resource limits applied to the QuickJS runtime before any script runs.
///
/// There is deliberately no wall-clock timeout here: cancellation (dropping
/// the run future, or the driver's cancel cascade) is the deadline mechanism.
#[derive(Debug, Clone, Copy)]
pub struct VmLimits {
    /// QuickJS heap ceiling in bytes (default 32 MiB).
    pub memory_limit_bytes: usize,
    /// Maximum interpreter stack in bytes (default 1 MiB).
    pub max_stack_bytes: usize,
}

impl Default for VmLimits {
    fn default() -> Self {
        Self::from_env()
    }
}

impl VmLimits {
    pub fn from_env() -> Self {
        Self {
            memory_limit_bytes: env_usize_bytes(
                VM_MEMORY_LIMIT_MB_ENV,
                1024 * 1024,
                MIN_VM_MEMORY_LIMIT_BYTES,
                MAX_VM_MEMORY_LIMIT_BYTES,
                DEFAULT_VM_MEMORY_LIMIT_BYTES,
            ),
            max_stack_bytes: env_usize_bytes(
                VM_STACK_KB_ENV,
                1024,
                MIN_VM_STACK_BYTES,
                MAX_VM_STACK_BYTES,
                DEFAULT_VM_STACK_BYTES,
            ),
        }
    }
}

fn env_usize_bytes(name: &str, unit: usize, min: usize, max: usize, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .and_then(|value| value.checked_mul(unit))
        .map(|bytes| bytes.clamp(min, max))
        .unwrap_or(default)
}

fn max_concurrent_vms() -> usize {
    env::var(VM_MAX_CONCURRENT_ENV)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .map(|value| value.clamp(1, MAX_CONCURRENT_VMS))
        .unwrap_or(DEFAULT_MAX_CONCURRENT_VMS)
}

fn vm_thread_stack_bytes() -> usize {
    env_usize_bytes(
        VM_THREAD_STACK_KB_ENV,
        1024,
        MIN_VM_THREAD_STACK_BYTES,
        MAX_VM_THREAD_STACK_BYTES,
        DEFAULT_VM_THREAD_STACK_BYTES,
    )
}

fn vm_admission() -> &'static Arc<Semaphore> {
    static ADMISSION: OnceLock<Arc<Semaphore>> = OnceLock::new();
    ADMISSION.get_or_init(|| Arc::new(Semaphore::new(max_concurrent_vms())))
}

/// Executes Workflow scripts, one isolated QuickJS runtime per run.
///
/// Every [`WorkflowVm::run_script`] call spins up a fresh interpreter on a
/// dedicated thread, so runs share nothing (globals, heap, interned atoms)
/// and a wedged script can never stall a sibling run.
#[derive(Debug, Clone, Default)]
pub struct WorkflowVm {
    limits: VmLimits,
}

impl WorkflowVm {
    /// A VM with the default [`VmLimits`].
    pub fn new() -> Self {
        Self::default()
    }

    /// A VM with explicit resource limits.
    pub fn with_limits(limits: VmLimits) -> Self {
        Self { limits }
    }

    /// Run one Workflow script to completion.
    ///
    /// * `source` is the script body; it is wrapped in an async function, so
    ///   top-level `await` and `return` both work. The returned value is the
    ///   script's `return` value, JSON-encoded (`undefined` becomes `null`).
    /// * `args` is exposed verbatim to the script as the `args` global.
    /// * `driver` executes `task()` spawns and receives progress events. A
    ///   driver instance is scoped to exactly one run: `cancel_all` is always
    ///   invoked at run teardown (success, script error, or cancellation), so
    ///   stray children never outlive the script that spawned them.
    ///
    /// Cancellation cascade (design §9): dropping the returned future cancels
    /// the run — the interrupt handler aborts executing JS, pending `task()`
    /// awaits resolve to errors, and `driver.cancel_all()` is invoked
    /// immediately from the dropping thread.
    pub async fn run_script(
        &self,
        source: &str,
        args: serde_json::Value,
        driver: Arc<dyn WorkflowDriver>,
    ) -> Result<serde_json::Value, WorkflowJsError> {
        self.run_script_with_cancel(source, args, driver, WorkflowRunCancel::new())
            .await
    }

    /// Like [`Self::run_script`], but accepts an external cancel handle so the
    /// host can interrupt the VM without dropping the run future.
    pub async fn run_script_with_cancel(
        &self,
        source: &str,
        args: serde_json::Value,
        driver: Arc<dyn WorkflowDriver>,
        cancel: WorkflowRunCancel,
    ) -> Result<serde_json::Value, WorkflowJsError> {
        self.run_inner(source, args, driver, None, cancel).await
    }

    /// Run a code-mode script: the same VM, plus the `tools.call()` host
    /// binding backed by `invoker`. Workflow runs (no invoker) never see the
    /// binding, so the Workflow sandbox keeps its documented host surface.
    pub async fn run_tools_script(
        &self,
        source: &str,
        args: serde_json::Value,
        driver: Arc<dyn WorkflowDriver>,
        invoker: Arc<dyn ToolInvoker>,
        cancel: WorkflowRunCancel,
    ) -> Result<serde_json::Value, WorkflowJsError> {
        self.run_inner(source, args, driver, Some(invoker), cancel)
            .await
    }

    async fn run_inner(
        &self,
        source: &str,
        args: serde_json::Value,
        driver: Arc<dyn WorkflowDriver>,
        invoker: Option<Arc<dyn ToolInvoker>>,
        cancel: WorkflowRunCancel,
    ) -> Result<serde_json::Value, WorkflowJsError> {
        let args_json = serde_json::to_string(&args)
            .map_err(|err| WorkflowJsError::InvalidArgs(err.to_string()))?;
        let cancel = cancel.0;
        let (result_tx, result_rx) = oneshot::channel();
        let mut guard = RunGuard {
            cancel: cancel.clone(),
            driver: driver.clone(),
            armed: true,
        };

        let permit = vm_admission()
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| WorkflowJsError::VmInit("VM admission gate closed".to_string()))?;
        let limits = self.limits;
        let source = source.to_string();
        let thread_driver = driver.clone();
        let thread_cancel = cancel.clone();
        let thread_invoker = invoker.clone();
        // Native admission belongs to the originating host executor. The VM
        // thread carries only Send requests/replies, never the Child Engine's
        // Rust future on its bounded interpreter stack.
        let host_runtime = Handle::try_current().map_err(|_| {
            WorkflowJsError::VmInit("workflow host Tokio runtime is unavailable".to_string())
        })?;
        let spawned = std::thread::Builder::new()
            .name("workflow-js-vm".to_string())
            .stack_size(vm_thread_stack_bytes())
            .spawn(move || {
                let _permit: OwnedSemaphorePermit = permit;
                let outcome = vm_thread_main(
                    source,
                    args_json,
                    thread_driver.clone(),
                    thread_cancel,
                    thread_invoker,
                    limits,
                    host_runtime,
                );
                // Run teardown: this driver is scoped to one run, so any task
                // still in flight is unreachable now — cancel the cascade.
                thread_driver.cancel_all();
                let _ = result_tx.send(outcome);
            });
        if let Err(err) = spawned {
            guard.armed = false;
            return Err(WorkflowJsError::VmInit(format!(
                "failed to spawn VM thread: {err}"
            )));
        }

        match result_rx.await {
            Ok(outcome) => {
                // The VM thread has already torn down and cancelled children.
                guard.armed = false;
                outcome
            }
            // VM thread panicked before reporting; leave the guard armed so
            // its drop (right now, at return) cancels outstanding tasks.
            Err(_) => Err(WorkflowJsError::VmTerminated(
                "VM thread exited without reporting a result".to_string(),
            )),
        }
    }
}

/// Cooperative cancel signal shared by the run future (guard side) and the VM
/// thread. The atomic flag feeds the QuickJS interrupt handler (sync, called
/// mid-bytecode); the watch channel wakes host futures parked on driver
/// completions.
#[derive(Clone)]
pub struct WorkflowRunCancel(CancelHandle);

impl WorkflowRunCancel {
    #[must_use]
    pub fn new() -> Self {
        Self(CancelHandle::new())
    }

    pub fn cancel(&self) {
        self.0.cancel();
    }
}

impl Default for WorkflowRunCancel {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
struct CancelHandle {
    flag: Arc<AtomicBool>,
    tx: Arc<watch::Sender<bool>>,
}

impl CancelHandle {
    fn new() -> Self {
        let (tx, _rx) = watch::channel(false);
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            tx: Arc::new(tx),
        }
    }

    fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.tx.send_replace(true);
    }

    fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    async fn cancelled(&self) {
        let mut rx = self.tx.subscribe();
        let _ = rx.wait_for(|cancelled| *cancelled).await;
    }

    fn flag_arc(&self) -> Arc<AtomicBool> {
        self.flag.clone()
    }
}

/// Fires the cancel cascade if the caller drops the run future before the VM
/// reports a result.
struct RunGuard {
    cancel: CancelHandle,
    driver: Arc<dyn WorkflowDriver>,
    armed: bool,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if self.armed {
            self.cancel.cancel();
            self.driver.cancel_all();
        }
    }
}

fn vm_thread_main(
    source: String,
    args_json: String,
    driver: Arc<dyn WorkflowDriver>,
    cancel: CancelHandle,
    invoker: Option<Arc<dyn ToolInvoker>>,
    limits: VmLimits,
    host_runtime: Handle,
) -> Result<serde_json::Value, WorkflowJsError> {
    let reactor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| WorkflowJsError::VmInit(format!("failed to build VM reactor: {err}")))?;
    reactor.block_on(run_in_vm(
        source,
        args_json,
        driver,
        cancel,
        invoker,
        limits,
        host_runtime,
    ))
}

async fn run_in_vm(
    source: String,
    args_json: String,
    driver: Arc<dyn WorkflowDriver>,
    cancel: CancelHandle,
    invoker: Option<Arc<dyn ToolInvoker>>,
    limits: VmLimits,
    host_runtime: Handle,
) -> Result<serde_json::Value, WorkflowJsError> {
    let runtime = AsyncRuntime::new().map_err(|err| WorkflowJsError::VmInit(err.to_string()))?;
    runtime.set_memory_limit(limits.memory_limit_bytes).await;
    runtime.set_max_stack_size(limits.max_stack_bytes).await;
    let interrupt_flag = cancel.flag_arc();
    runtime
        .set_interrupt_handler(Some(Box::new(move || {
            interrupt_flag.load(Ordering::Acquire)
        })))
        .await;
    let context = AsyncContext::full(&runtime)
        .await
        .map_err(|err| WorkflowJsError::VmInit(err.to_string()))?;

    let result = context
        .async_with(async |ctx| {
            run_in_ctx(
                ctx,
                source,
                args_json,
                driver,
                invoker,
                cancel,
                host_runtime,
            )
            .await
        })
        .await;
    drop(context);
    runtime.run_gc().await;
    result
}

async fn run_in_ctx(
    ctx: Ctx<'_>,
    source: String,
    args_json: String,
    driver: Arc<dyn WorkflowDriver>,
    invoker: Option<Arc<dyn ToolInvoker>>,
    cancel: CancelHandle,
    host_runtime: Handle,
) -> Result<serde_json::Value, WorkflowJsError> {
    install_host(
        &ctx,
        driver,
        invoker.clone(),
        cancel.clone(),
        &args_json,
        host_runtime,
    )?;
    ctx.eval::<(), _>(prelude())
        .catch(&ctx)
        .map_err(|err| WorkflowJsError::VmInit(format!("prelude failed: {err}")))?;
    if invoker.is_some() {
        ctx.eval::<(), _>(CODEMODE_PRELUDE)
            .catch(&ctx)
            .map_err(|err| WorkflowJsError::VmInit(format!("codemode prelude failed: {err}")))?;
    }

    let desugared = desugar_export_default(&source);
    let wrapped = format!("(async () => {{\n{desugared}\n}})()");
    let promise = ctx
        .eval::<Promise, _>(wrapped)
        .catch(&ctx)
        .map_err(|err| script_error(&cancel, err))?;
    let value = promise
        .into_future::<Value>()
        .await
        .catch(&ctx)
        .map_err(|err| script_error(&cancel, err))?;
    js_value_to_json(&ctx, value)
}

/// Rewrite the documented module-style authoring shape
/// (`export default async function (args) { ... }`) into the script form the
/// VM actually evals. Sources are wrapped in an async IIFE, where the
/// module-only `export` keyword is a syntax error, so without this every
/// imperative `export default` workflow (including the #4131 dogfood
/// fixtures) failed to parse. The default export is captured, invoked with
/// the `args` global when it is a function, and its result becomes the run
/// result; a non-function default export is returned as-is.
fn desugar_export_default(source: &str) -> String {
    const EXPORT_DEFAULT: &str = "export default";
    let Some(offset) = line_leading_export_default(source) else {
        return source.to_string();
    };
    let mut out = source.to_string();
    out.replace_range(
        offset..offset + EXPORT_DEFAULT.len(),
        "globalThis.__workflow_default =",
    );
    out.push('\n');
    out.push_str(
        ";{\n  const __wf_default = globalThis.__workflow_default;\n  delete globalThis.__workflow_default;\n  if (typeof __wf_default === \"function\") {\n    return await __wf_default(args);\n  }\n  if (__wf_default !== undefined) {\n    return __wf_default;\n  }\n}\n",
    );
    out
}

/// Return the byte offset of a line-leading `export default` token that is
/// actual JavaScript syntax, not text inside a string, template literal, or
/// comment. This intentionally recognizes only the documented authoring shape
/// instead of attempting to implement a general JavaScript module parser.
fn line_leading_export_default(source: &str) -> Option<usize> {
    const EXPORT_DEFAULT: &[u8] = b"export default";
    let bytes = source.as_bytes();
    let mut idx = 0usize;
    let mut quote = None;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut line_has_only_whitespace = true;

    while idx < bytes.len() {
        let byte = bytes[idx];

        if line_comment {
            if byte == b'\n' {
                line_comment = false;
                line_has_only_whitespace = true;
            }
            idx += 1;
            continue;
        }

        if block_comment {
            if byte == b'*' && bytes.get(idx + 1) == Some(&b'/') {
                block_comment = false;
                line_has_only_whitespace = false;
                idx += 2;
                continue;
            }
            if byte == b'\n' {
                line_has_only_whitespace = true;
            } else if !byte.is_ascii_whitespace() {
                line_has_only_whitespace = false;
            }
            idx += 1;
            continue;
        }

        if let Some(active_quote) = quote {
            if byte == b'\n' {
                line_has_only_whitespace = true;
                escaped = false;
            } else {
                if !byte.is_ascii_whitespace() {
                    line_has_only_whitespace = false;
                }
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == active_quote {
                    quote = None;
                }
            }
            idx += 1;
            continue;
        }

        if byte == b'\n' {
            line_has_only_whitespace = true;
            idx += 1;
            continue;
        }
        if line_has_only_whitespace && byte.is_ascii_whitespace() {
            idx += 1;
            continue;
        }
        if line_has_only_whitespace && bytes[idx..].starts_with(EXPORT_DEFAULT) {
            return Some(idx);
        }

        line_has_only_whitespace = false;
        if byte == b'/' && bytes.get(idx + 1) == Some(&b'/') {
            line_comment = true;
            idx += 2;
        } else if byte == b'/' && bytes.get(idx + 1) == Some(&b'*') {
            block_comment = true;
            idx += 2;
        } else {
            if matches!(byte, b'\'' | b'"' | b'`') {
                quote = Some(byte);
            }
            idx += 1;
        }
    }

    None
}

fn script_error(cancel: &CancelHandle, err: CaughtError<'_>) -> WorkflowJsError {
    if cancel.is_cancelled() {
        WorkflowJsError::Cancelled
    } else {
        WorkflowJsError::Script(err.to_string())
    }
}

fn js_value_to_json<'js>(
    ctx: &Ctx<'js>,
    value: Value<'js>,
) -> Result<serde_json::Value, WorkflowJsError> {
    if value.is_undefined() {
        return Ok(serde_json::Value::Null);
    }
    let text = ctx
        .json_stringify(value)
        .map_err(|err| WorkflowJsError::ResultEncoding(err.to_string()))?;
    match text {
        None => Ok(serde_json::Value::Null),
        Some(text) => {
            let text = text
                .to_string()
                .map_err(|err| WorkflowJsError::ResultEncoding(err.to_string()))?;
            serde_json::from_str(&text)
                .map_err(|err| WorkflowJsError::ResultEncoding(err.to_string()))
        }
    }
}

/// Prelude fragment for code-mode runs only: captures `__codemode_call` into
/// the frozen `globalThis.tools` surface, then deletes the raw binding.
/// Workflow runs never eval this, so their documented host surface is
/// unchanged — `tools` simply does not exist there.
const CODEMODE_PRELUDE: &str = r#"
(() => {
  const hostCall = __codemode_call;
  globalThis.tools = Object.freeze({
    call: async (tool, input) => {
      const envelope = JSON.parse(
        await hostCall(JSON.stringify({ tool, input: input === undefined ? {} : input }))
      );
      if (envelope.error !== undefined) {
        const err = new Error(envelope.error);
        err.kind = envelope.error_kind;
        throw err;
      }
      return envelope.result;
    },
  });
  try { delete globalThis.__codemode_call; } catch (_) { /* frozen shape */ }
})();
"#;

// Construct and poll native host futures on the captured host executor.
// JoinSet aborts on drop; explicit cancellation also joins that abort before
// the VM receives its existing typed cancellation error.
async fn call_on_host<T, F>(
    runtime: &Handle,
    cancel: &CancelHandle,
    cancel_error: TaskError,
    call: impl FnOnce() -> F + Send + 'static,
) -> Result<T, TaskError>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    let mut pending = JoinSet::new();
    let host_cancel = cancel.clone();
    let host_error = cancel_error.clone();
    pending.spawn_on(
        async move {
            if host_cancel.is_cancelled() {
                Err(host_error)
            } else {
                Ok(call().await)
            }
        },
        runtime,
    );
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            pending.shutdown().await;
            Err(cancel_error)
        }
        outcome = pending.join_next() => match outcome {
            Some(Ok(value)) => value,
            Some(Err(error)) if error.is_panic() => {
                std::panic::resume_unwind(error.into_panic())
            }
            _ => Err(TaskError::new(
                TaskErrorKind::Driver,
                "workflow host callback ended without a reply",
            )),
        },
    }
}

/// The `tools.call()` host call. Infallible at the binding level, like
/// `task_host`: outcomes return through the `{result}` /
/// `{error, error_kind}` envelope so the prelude rethrows typed errors.
/// Gate refusals arrive as admission errors (nothing ran); a nested tool
/// that ran and failed arrives as an agent error (work failed).
async fn tools_call_host(
    call_json: String,
    invoker: Arc<dyn ToolInvoker>,
    cancel: CancelHandle,
    host_runtime: Handle,
) -> String {
    let outcome = tools_call_host_inner(call_json, invoker, cancel, host_runtime).await;
    let envelope = match outcome {
        Ok(result) => serde_json::json!({ "result": result }),
        Err(TaskError { kind, message }) => {
            serde_json::json!({ "error": message, "error_kind": kind.as_str() })
        }
    };
    envelope.to_string()
}

async fn tools_call_host_inner(
    call_json: String,
    invoker: Arc<dyn ToolInvoker>,
    cancel: CancelHandle,
    host_runtime: Handle,
) -> Result<serde_json::Value, TaskError> {
    let admission = |message: String| TaskError::new(TaskErrorKind::Admission, message);
    let request: ToolCallRequest = serde_json::from_str(&call_json).map_err(|err| {
        admission(format!(
            "tools.call(): expected {{\"tool\", \"input\"}} JSON: {err}"
        ))
    })?;
    if request.tool.trim().is_empty() {
        return Err(admission(
            "tools.call(): `tool` must be a non-empty string".to_string(),
        ));
    }
    if !request.input.is_object() {
        return Err(admission(
            "tools.call(): `input` must be a JSON object".to_string(),
        ));
    }
    if cancel.is_cancelled() {
        return Err(TaskError::new(
            TaskErrorKind::Cancelled,
            "tools.call(): run cancelled".to_string(),
        ));
    }
    let response = call_on_host(
        &host_runtime,
        &cancel,
        TaskError::new(TaskErrorKind::Cancelled, "tools.call(): run cancelled"),
        move || async move { invoker.invoke(request).await },
    )
    .await?
    .map_err(|err| TaskError::new(TaskErrorKind::from(&err), format!("tools.call(): {err}")))?;
    if response.ok {
        Ok(response.result)
    } else {
        let message = response
            .result
            .as_str()
            .unwrap_or("tools.call(): tool failed without a message")
            .to_string();
        Err(TaskError::new(TaskErrorKind::Agent, message))
    }
}

fn install_host(
    ctx: &Ctx<'_>,
    driver: Arc<dyn WorkflowDriver>,
    invoker: Option<Arc<dyn ToolInvoker>>,
    cancel: CancelHandle,
    args_json: &str,
    host_runtime: Handle,
) -> Result<(), WorkflowJsError> {
    let globals = ctx.globals();

    let args_value: Value = ctx
        .json_parse(args_json)
        .map_err(|err| WorkflowJsError::InvalidArgs(err.to_string()))?;
    globals.set("args", args_value).map_err(init_err)?;

    let task_driver = driver.clone();
    let task_cancel = cancel.clone();
    let task_runtime = host_runtime.clone();
    globals
        .set(
            "__workflow_task",
            Func::from(Async(move |opts_json: String| {
                let driver = task_driver.clone();
                let cancel = task_cancel.clone();
                let host_runtime = task_runtime.clone();
                async move { task_host(opts_json, driver, cancel, host_runtime).await }
            })),
        )
        .map_err(init_err)?;

    if let Some(invoker) = invoker {
        let call_invoker = invoker.clone();
        let call_cancel = cancel.clone();
        globals
            .set(
                "__codemode_call",
                Func::from(Async(move |call_json: String| {
                    let invoker = call_invoker.clone();
                    let cancel = call_cancel.clone();
                    let host_runtime = host_runtime.clone();
                    async move { tools_call_host(call_json, invoker, cancel, host_runtime).await }
                })),
            )
            .map_err(init_err)?;
    }

    let log_driver = driver.clone();
    globals
        .set(
            "__workflow_log",
            Func::from(move |message: String| {
                log_driver.progress(ProgressEvent::Log { message });
            }),
        )
        .map_err(init_err)?;

    // Structured twin of the prelude's "every slot failed" breadcrumb (R9):
    // a dead fan-out of script-thrown thunks leaves no task record behind,
    // so the host needs a typed event — not a log line — to keep the run's
    // terminal status honest.
    let fanout_driver = driver.clone();
    globals
        .set(
            "__workflow_every_slot_failed",
            Func::from(move |construct: String, failed: u32, total: u32| {
                fanout_driver.progress(ProgressEvent::FanoutAllSlotsFailed {
                    construct,
                    failed,
                    total,
                });
            }),
        )
        .map_err(init_err)?;

    // Structured twin of the per-slot "dropped a failed slot as null"
    // breadcrumb (R9): a PARTIALLY failed fan-out still resolves and leaves
    // no task record for the dropped slot, so without this event the ledger
    // cannot see the loss and records a clean Completed.
    let fanout_driver = driver.clone();
    globals
        .set(
            "__workflow_slot_dropped",
            Func::from(move |construct: String, kind: String, slot: u32| {
                fanout_driver.progress(ProgressEvent::FanoutSlotDropped {
                    construct,
                    kind,
                    slot,
                });
            }),
        )
        .map_err(init_err)?;

    let phase_driver = driver.clone();
    globals
        .set(
            "__workflow_phase",
            Func::from(move |title: String| {
                phase_driver.progress(ProgressEvent::Phase { title });
            }),
        )
        .map_err(init_err)?;

    // Budget reads are live driver snapshots (design §5.2). NaN encodes
    // "no ceiling" for `total`; the prelude maps it to `null`.
    let total_driver = driver.clone();
    globals
        .set(
            "__workflow_budget_total",
            Func::from(move || -> f64 {
                match total_driver.budget().total {
                    Some(total) => total as f64,
                    None => f64::NAN,
                }
            }),
        )
        .map_err(init_err)?;

    let spent_driver = driver.clone();
    globals
        .set(
            "__workflow_budget_spent",
            Func::from(move || -> f64 {
                if spent_driver.budget_is_metered() {
                    spent_driver.budget().spent as f64
                } else {
                    f64::NAN
                }
            }),
        )
        .map_err(init_err)?;

    globals
        .set(
            "__workflow_budget_remaining",
            Func::from(move || -> f64 {
                if !driver.budget_is_metered() {
                    return f64::NAN;
                }
                match driver.budget().remaining() {
                    Some(remaining) => remaining as f64,
                    None => f64::INFINITY,
                }
            }),
        )
        .map_err(init_err)?;

    Ok(())
}

fn init_err(err: rquickjs::Error) -> WorkflowJsError {
    WorkflowJsError::VmInit(err.to_string())
}

/// The `task()` host call. Everything that can go wrong is reported through
/// the JSON envelope (`{"error": ..., "error_kind": ...}`) so the prelude
/// re-throws it as a real JS `Error` with a script-side stack and a typed
/// [`TaskErrorKind`] on `.kind` (R9). The kind is assigned here, where the
/// failure actually happened — never re-derived from the message text.
async fn task_host(
    opts_json: String,
    driver: Arc<dyn WorkflowDriver>,
    cancel: CancelHandle,
    host_runtime: Handle,
) -> String {
    let outcome = task_host_inner(opts_json, driver, cancel, host_runtime).await;
    let envelope = match outcome {
        Ok(value) => serde_json::json!({ "value": value }),
        Err(TaskError { kind, message }) => {
            serde_json::json!({ "error": message, "error_kind": kind.as_str() })
        }
    };
    envelope.to_string()
}

/// Best-effort `label`/`phase` from raw `task()` options, for rejection
/// receipts when the options never survived parsing.
fn task_identity_hint(opts_json: &str) -> (Option<String>, Option<String>) {
    let value: serde_json::Value =
        serde_json::from_str(opts_json).unwrap_or(serde_json::Value::Null);
    let pluck = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    (pluck("label"), pluck("phase"))
}

/// Record a pre-spawn `task()` rejection on the host ledger, then hand the
/// message back for the JS throw. Rejections that never reach `spawn_task`
/// would otherwise be invisible to the run record (#5035's surviving gap).
fn reject_task(driver: &Arc<dyn WorkflowDriver>, opts_json: &str, message: String) -> String {
    let (label, phase) = task_identity_hint(opts_json);
    driver.progress(ProgressEvent::TaskRejected {
        label,
        phase,
        message: message.clone(),
    });
    message
}

/// Emit the terminal schema-failure receipt for a `task()` whose reply failed
/// `responseSchema` with no repair left to try, and hand the message back for
/// the JS throw. `note` (when present) names why a repair was skipped, so the
/// operator can tell "repair refused to run" from "repair also failed".
fn fail_schema(
    driver: &Arc<dyn WorkflowDriver>,
    task_id: String,
    attempt: u32,
    error: &ReplyDecodeError,
    raw: String,
    raw_truncated: bool,
    note: Option<String>,
) -> String {
    let message = match note {
        Some(note) => format!("{} (repair skipped: {note})", error.message()),
        None => error.message().to_string(),
    };
    driver.progress(ProgressEvent::TaskSchemaValidationFailed {
        task_id,
        kind: error.kind().to_string(),
        attempt,
        message: message.clone(),
        raw,
        raw_truncated,
    });
    message
}

/// Build the repair request for `next_attempt` (#5583): the same child
/// identity, budget fields, and schema as the original, with a repair prompt
/// carrying the original task, the schema, the failed reply, and why it failed.
fn repair_request(
    original: &TaskRequest,
    next_attempt: u32,
    error: &ReplyDecodeError,
    failed_raw: &str,
) -> TaskRequest {
    let mut request = original.clone();
    let schema = original
        .response_schema
        .as_ref()
        .expect("repair only runs when responseSchema is set");
    // The bracket prefix identifies the repair at a glance on progress
    // surfaces and lets tests script the repair reply by rule order.
    request.description = format!(
        "[schema repair {next_attempt}] {}",
        repair_prompt(&original.description, schema, failed_raw, error)
    );
    // Label and phase stay inherited so progress surfaces group the repair
    // with its task.
    request
}

/// The one cancellation error: the run's deadline fired. Fatal in every
/// `parallel()` / `pipeline()` mode — a cancelled run must never resolve into
/// a slot value.
fn cancelled_task() -> TaskError {
    TaskError::new(TaskErrorKind::Cancelled, "task(): run cancelled")
}

/// A terminal `responseSchema` failure, already receipted by [`fail_schema`].
fn schema_task(message: String) -> TaskError {
    TaskError::new(TaskErrorKind::Schema, message)
}

async fn task_host_inner(
    opts_json: String,
    driver: Arc<dyn WorkflowDriver>,
    cancel: CancelHandle,
    host_runtime: Handle,
) -> Result<serde_json::Value, TaskError> {
    let admission = |message: String| TaskError::new(TaskErrorKind::Admission, message);
    let workspace = driver.workspace_root();
    let request = parse_task_options(&opts_json, workspace.as_deref())
        .map_err(|message| admission(reject_task(&driver, &opts_json, message)))?;
    // Compile the schema before spawning so a malformed one fails fast
    // instead of burning a subagent.
    let validator = request
        .response_schema
        .as_ref()
        .map(compile_schema)
        .transpose()
        .map_err(|message| admission(reject_task(&driver, &opts_json, message)))?;

    // Fast-fail budget gate. The authoritative reservation lives in the
    // driver (design §5.3); this only stops obviously-doomed spawns early.
    let snapshot = driver.budget();
    if driver.budget_is_metered() && snapshot.exhausted() {
        return Err(TaskError::new(
            TaskErrorKind::Budget,
            reject_task(
                &driver,
                &opts_json,
                format!(
                    "task(): budget exhausted ({} of {} tokens spent)",
                    snapshot.spent,
                    snapshot.total.unwrap_or(0)
                ),
            ),
        ));
    }
    if cancel.is_cancelled() {
        return Err(cancelled_task());
    }

    // Bounded schema repair (#5583): after a failed `responseSchema` decode,
    // re-ask the same route before throwing. `None` is the default single
    // repair; `Some(0)` disables it. The first attempt is attempt 1, so the
    // task is schema-terminal once `attempt` reaches this ceiling.
    let last_attempt = 1 + request.schema_repair_attempts.unwrap_or(1);
    let mut current = request.clone();
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        // Admission can wait (a saturated concurrency gate, a routing call),
        // so it races the run's cancel exactly as the completion does below
        // and as `tools.call()` does: a cancel that only closed the driver's
        // gate used to be the one thing that could wake it.
        let task_driver = driver.clone();
        let task_request = current.clone();
        let spawned_task = call_on_host(
            &host_runtime,
            &cancel,
            cancelled_task(),
            move || async move { task_driver.spawn_task(task_request).await },
        )
        .await?
        .map_err(|err| TaskError::new(TaskErrorKind::from(&err), err.to_string()))?;
        let task_id = spawned_task.task_id;
        let completion_rx = spawned_task.completion;
        let completion = tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled_task()),
            completion = completion_rx => completion.map_err(|_| {
                TaskError::new(
                    TaskErrorKind::Driver,
                    "task(): driver dropped the completion channel",
                )
            })?,
        };

        let text = match completion {
            TaskCompletion::Completed { text } => text,
            TaskCompletion::Failed { message } => {
                return Err(TaskError::new(
                    TaskErrorKind::Agent,
                    format!("task(): subagent failed: {message}"),
                ));
            }
            TaskCompletion::Cancelled => {
                return Err(TaskError::new(
                    TaskErrorKind::Cancelled,
                    "task(): subagent cancelled",
                ));
            }
            TaskCompletion::BudgetExhausted { message } => {
                return Err(TaskError::new(
                    TaskErrorKind::Budget,
                    format!("task(): budget exhausted: {message}"),
                ));
            }
        };
        // Without a schema the raw text is the contract; with one, the decode
        // decides — and a failure may still be repaired.
        let Some(validator) = validator.as_ref() else {
            return Ok(serde_json::Value::String(text));
        };
        let error = match decode_reply(&text, validator) {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let (raw, raw_truncated) = carried_raw(&text);
        if attempt >= last_attempt {
            return Err(schema_task(fail_schema(
                &driver,
                task_id,
                attempt,
                &error,
                raw,
                raw_truncated,
                None,
            )));
        }
        // The attempt failed but a repair remains: record it as a receipt
        // (visible even when the repair succeeds), then re-run the admission
        // gates — a repair is a real child, not a free retry.
        driver.progress(ProgressEvent::TaskSchemaRepairAttempted {
            task_id: task_id.clone(),
            kind: error.kind().to_string(),
            attempt,
            message: error.message().to_string(),
            raw: raw.clone(),
            raw_truncated,
        });
        let snapshot = driver.budget();
        if driver.budget_is_metered() && snapshot.exhausted() {
            return Err(schema_task(fail_schema(
                &driver,
                task_id,
                attempt,
                &error,
                raw,
                raw_truncated,
                Some("budget exhausted".to_string()),
            )));
        }
        if cancel.is_cancelled() {
            return Err(cancelled_task());
        }
        current = repair_request(&request, attempt + 1, &error, &raw);
    }
}

/// JS-facing option names for `task()` (design §3.3). Unknown fields are
/// rejected so a typo (`responseschema`) fails loudly instead of being
/// silently dropped. Every multi-word field also accepts its snake_case
/// spelling, and the `agent` tool's `workspace_policy` name is accepted as an
/// alias for worktree isolation — the two spawn surfaces are written by the
/// same authors (often models), so a schema that runs on one must not be an
/// unknown-field error on the other.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskOptions {
    #[serde(alias = "title")]
    description: Option<String>,
    prompt: Option<String>,
    #[serde(alias = "type", alias = "subagent_type")]
    subagent_type: Option<String>,
    /// Fleet role name (#4177). Preferred step identity field.
    role: Option<String>,
    profile: Option<String>,
    model: Option<String>,
    #[serde(alias = "model_strength")]
    model_strength: Option<String>,
    thinking: Option<String>,
    cwd: Option<String>,
    #[serde(default)]
    worktree: bool,
    /// `agent`-tool alias for worktree isolation: "shared" | "worktree".
    #[serde(default, alias = "workspace_policy")]
    workspace_policy: Option<String>,
    #[serde(alias = "write_authority")]
    write_authority: Option<String>,
    #[serde(default, alias = "write_roots")]
    write_roots: Vec<String>,
    #[serde(default, alias = "exact_files")]
    exact_files: Vec<String>,
    #[serde(default, alias = "coordination_contracts")]
    coordination_contracts: Vec<String>,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    acceptance: Vec<String>,
    #[serde(alias = "allowed_tools")]
    allowed_tools: Option<Vec<String>>,
    #[serde(alias = "max_depth")]
    max_depth: Option<u32>,
    #[serde(alias = "token_budget")]
    token_budget: Option<u64>,
    #[serde(alias = "max_steps")]
    max_steps: Option<u32>,
    #[serde(alias = "wall_time_secs")]
    wall_time_secs: Option<u64>,
    #[serde(alias = "response_schema")]
    response_schema: Option<serde_json::Value>,
    /// Bounded `responseSchema` repair attempts after a failed decode
    /// (#5583): re-ask the same route with the schema and the failed reply.
    /// Defaults to one; `0` disables; capped at
    /// [`SCHEMA_REPAIR_MAX_ATTEMPTS`] so repair stays a bounded recovery.
    #[serde(default, alias = "schema_repair_attempts")]
    schema_repair_attempts: Option<u32>,
    label: Option<String>,
    phase: Option<String>,
}

fn parse_task_options(
    opts_json: &str,
    workspace: Option<&std::path::Path>,
) -> Result<TaskRequest, String> {
    let mut options: TaskOptions =
        serde_json::from_str(opts_json).map_err(|err| format!("task(): invalid options: {err}"))?;
    if let Some(policy) = options.workspace_policy.take() {
        match policy.trim().to_ascii_lowercase().as_str() {
            "worktree" => options.worktree = true,
            "shared" => {
                if options.worktree {
                    return Err(
                        "task(): workspacePolicy 'shared' conflicts with worktree: true"
                            .to_string(),
                    );
                }
            }
            other => {
                return Err(format!(
                    "task(): workspacePolicy must be shared or worktree; got {other:?}"
                ));
            }
        }
    }
    let description = options
        .prompt
        .or(options.description)
        .filter(|description| !description.trim().is_empty())
        .ok_or_else(|| "task(): 'description' (or 'prompt') is required".to_string())?;
    let role = options
        .role
        .as_deref()
        .map(normalize_profile)
        .transpose()
        .map_err(|err| format!("task(): role: {err}"))?;
    let profile = options
        .profile
        .as_deref()
        .map(normalize_profile)
        .transpose()
        .map_err(|err| format!("task(): {err}"))?;
    options.write_roots = normalize_task_paths("writeRoots", options.write_roots, 32)?;
    options.exact_files = normalize_task_paths("exactFiles", options.exact_files, 32)?;
    let cwd = options
        .cwd
        .as_deref()
        .map(|cwd| normalize_task_cwd_in(cwd, workspace))
        .transpose()?;
    options.coordination_contracts =
        normalize_task_string_list("coordinationContracts", options.coordination_contracts, 16)?;
    options.dependencies = normalize_task_string_list("dependencies", options.dependencies, 8)?;
    options.acceptance = normalize_task_string_list("acceptance", options.acceptance, 8)?;
    let write_authority = options
        .write_authority
        .as_deref()
        .map(|value| value.trim().to_ascii_lowercase())
        .map(|value| match value.as_str() {
            "read_only" | "workspace_write" | "worktree_write" => Ok(value),
            _ => Err(format!(
                "task(): writeAuthority must be read_only, workspace_write, or worktree_write; got {value:?}"
            )),
        })
        .transpose()?;
    if write_authority.as_deref() == Some("worktree_write") && !options.worktree {
        return Err("task(): writeAuthority worktree_write requires worktree: true".to_string());
    }
    let role_kind = role.as_deref().and_then(task_role_kind);
    let type_kind = options.subagent_type.as_deref().and_then(task_role_kind);
    if let (Some(role_kind), Some(type_kind)) = (role_kind, type_kind)
        && role_kind != type_kind
    {
        return Err("task(): role and subagentType declare contradictory authorities".to_string());
    }
    let declared_kind = role_kind.or(type_kind);
    if matches!(declared_kind, Some(TaskRoleKind::ReadOnly))
        && write_authority
            .as_deref()
            .is_some_and(|authority| authority != "read_only")
    {
        return Err("task(): read-only roles cannot declare write-capable authority".to_string());
    }
    // A write-capable task with no declared scope is not refused here: the
    // spawn boundary (`validate_spawn_write_contract`) claims the workspace
    // root, or the task's deliverables, exactly as it does for a plain Agent
    // spawn, and the coordination ledger arbitrates contention with live peers.
    if let Some(attempts) = options.schema_repair_attempts
        && attempts > SCHEMA_REPAIR_MAX_ATTEMPTS
    {
        return Err(format!(
            "task(): schemaRepairAttempts is bounded to {SCHEMA_REPAIR_MAX_ATTEMPTS}; \
             repair is a bounded recovery, not a retry loop"
        ));
    }
    Ok(TaskRequest {
        description,
        subagent_type: options.subagent_type,
        role,
        profile,
        model: options.model,
        model_strength: options.model_strength,
        thinking: options.thinking,
        cwd,
        worktree: options.worktree,
        write_authority,
        write_roots: options.write_roots,
        exact_files: options.exact_files,
        coordination_contracts: options.coordination_contracts,
        dependencies: options.dependencies,
        acceptance: options.acceptance,
        allowed_tools: options.allowed_tools,
        // Host-imposed only: a script cannot set (or clear) a deny list.
        disallowed_tools: Vec::new(),
        max_depth: options.max_depth,
        token_budget: options.token_budget,
        max_steps: options.max_steps,
        wall_time_secs: options.wall_time_secs,
        response_schema: options.response_schema,
        schema_repair_attempts: options.schema_repair_attempts,
        label: options.label,
        phase: options.phase,
    })
}

fn normalize_task_string_list(
    field: &str,
    values: Vec<String>,
    limit: usize,
) -> Result<Vec<String>, String> {
    if values.len() > limit {
        return Err(format!("task(): {field} accepts at most {limit} entries"));
    }
    let mut normalized = Vec::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 512 {
            return Err(format!(
                "task(): {field} entries must be 1..=512 characters"
            ));
        }
        if !normalized.iter().any(|existing| existing == value) {
            normalized.push(value.to_string());
        }
    }
    Ok(normalized)
}

/// Normalize a task working directory at both plan preflight and VM dispatch.
/// The same bounded repo-relative policy applies to both entry points.
pub fn normalize_task_cwd(value: &str) -> Result<String, String> {
    normalize_task_paths("cwd", vec![value.to_owned()], 1).map(|mut paths| paths.remove(0))
}

/// [`normalize_task_cwd`] for a run that knows its workspace root. An
/// absolute path that lies inside `workspace` is rewritten to the same
/// bounded repo-relative form; an absolute path outside it, or one that
/// escapes through `..`, is still rejected. Without a workspace every
/// absolute path is rejected, exactly as before.
///
/// The comparison is lexical and component-wise (no filesystem access), so
/// `/ws-other` is never inside `/ws`. On Windows a drive letter or UNC share
/// matches with or without the verbatim `\\?\` prefix, `/` and `\` are both
/// separators, and components compare case-insensitively.
pub fn normalize_task_cwd_in(
    value: &str,
    workspace: Option<&std::path::Path>,
) -> Result<String, String> {
    let raw = value.trim();
    let path = std::path::Path::new(raw);
    let Some(workspace) = workspace.filter(|_| path.is_absolute()) else {
        return normalize_task_cwd(value);
    };
    let outside = || {
        format!(
            "task(): cwd {raw:?} is outside the workspace {}; cwd entries must be bounded repo-relative paths or absolute paths inside the workspace",
            workspace.display()
        )
    };
    let mut remainder = path_keys(path).ok_or_else(outside)?.into_iter();
    for expected in path_keys(workspace).ok_or_else(outside)? {
        match remainder.next() {
            Some(actual) if actual.matches(&expected) => {}
            // `..` right after the workspace prefix is an escape, not a
            // different root: name it as traversal.
            Some(PathKey::Parent) => {
                return Err("task(): cwd paths cannot contain parent traversal".to_string());
            }
            _ => return Err(outside()),
        }
    }
    let mut segments = Vec::new();
    for key in remainder {
        match key {
            PathKey::Name { original, .. } => segments.push(original),
            PathKey::Parent => {
                return Err("task(): cwd paths cannot contain parent traversal".to_string());
            }
            PathKey::Root => return Err(outside()),
        }
    }
    if segments.is_empty() {
        return Ok(".".to_string());
    }
    normalize_task_cwd(&segments.join("/"))
}

/// One lexical path component, keyed for comparison: a drive or UNC prefix
/// with its verbatim marker dropped, and names case-folded on Windows.
#[derive(Debug)]
enum PathKey {
    Root,
    Parent,
    Name { key: String, original: String },
}

impl PathKey {
    fn matches(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Root, Self::Root) | (Self::Parent, Self::Parent) => true,
            (Self::Name { key: left, .. }, Self::Name { key: right, .. }) => left == right,
            _ => false,
        }
    }
}

fn path_keys(path: &std::path::Path) -> Option<Vec<PathKey>> {
    use std::path::{Component, Prefix};
    let fold = |value: &str| {
        if cfg!(windows) {
            value.to_lowercase()
        } else {
            value.to_string()
        }
    };
    let mut keys = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                let key = match prefix.kind() {
                    Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                        format!("{}:", drive.to_ascii_lowercase() as char)
                    }
                    Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => format!(
                        "//{}/{}",
                        server.to_str()?.to_lowercase(),
                        share.to_str()?.to_lowercase()
                    ),
                    _ => prefix.as_os_str().to_str()?.to_lowercase(),
                };
                keys.push(PathKey::Name {
                    original: key.clone(),
                    key,
                });
            }
            Component::RootDir => keys.push(PathKey::Root),
            Component::CurDir => {}
            Component::ParentDir => keys.push(PathKey::Parent),
            Component::Normal(name) => {
                let original = name.to_str()?.to_string();
                keys.push(PathKey::Name {
                    key: fold(&original),
                    original,
                });
            }
        }
    }
    Some(keys)
}

fn normalize_task_paths(
    field: &str,
    values: Vec<String>,
    limit: usize,
) -> Result<Vec<String>, String> {
    if values.len() > limit {
        return Err(format!("task(): {field} accepts at most {limit} entries"));
    }
    let mut normalized = Vec::new();
    for raw in values {
        let raw = raw.trim().replace('\\', "/");
        let windows_drive = raw.as_bytes().get(1) == Some(&b':')
            && raw.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
        if raw.is_empty()
            || raw.chars().count() > 512
            || raw.starts_with('/')
            || raw.starts_with("//")
            || windows_drive
            || raw.chars().any(|ch| matches!(ch, '\0' | '\r' | '\n'))
        {
            return Err(format!(
                "task(): {field} entries must be bounded repo-relative paths"
            ));
        }
        let mut segments = Vec::new();
        for segment in raw.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    return Err(format!(
                        "task(): {field} paths cannot contain parent traversal"
                    ));
                }
                value => segments.push(value),
            }
        }
        let path = if segments.is_empty() {
            ".".to_string()
        } else {
            segments.join("/")
        };
        if !normalized.contains(&path) {
            normalized.push(path);
        }
    }
    Ok(normalized)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskRoleKind {
    ReadOnly,
    General,
    Implementer,
}

fn task_role_kind(value: &str) -> Option<TaskRoleKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "explore" | "explorer" | "scout" | "plan" | "planner" | "review" | "reviewer"
        | "verify" | "verifier" => Some(TaskRoleKind::ReadOnly),
        "general" | "worker" => Some(TaskRoleKind::General),
        "implement" | "implementer" | "builder" => Some(TaskRoleKind::Implementer),
        _ => None,
    }
}

/// The JS prelude injected before every script: determinism bans, the
/// `task`/`parallel`/`pipeline`/`log`/`phase` stdlib (design §7), and the
/// `budget` global.
fn prelude() -> String {
    PRELUDE_TEMPLATE.replace("__MAX_ITEMS__", &PARALLEL_MAX_ITEMS.to_string())
}

const PRELUDE_TEMPLATE: &str = r#""use strict";
(() => {
  const banned = (name) => () => {
    throw new Error(name + " is unavailable in Workflow scripts: runs must be deterministic for record/replay");
  };
  const BannedDate = function Date() {
    throw new Error("new Date()/Date() is unavailable in Workflow scripts: runs must be deterministic for record/replay");
  };
  BannedDate.now = banned("Date.now()");
  BannedDate.parse = banned("Date.parse()");
  BannedDate.UTC = banned("Date.UTC()");
  globalThis.Date = BannedDate;
  Math.random = banned("Math.random()");

  // Capture temporary host bindings into this closure, then strip them from
  // globalThis so scripts only see the documented Workflow surface (#4129).
  const hostTask = __workflow_task;
  const hostLog = __workflow_log;
  const hostEverySlotFailed = __workflow_every_slot_failed;
  const hostSlotDropped = __workflow_slot_dropped;
  const hostPhase = __workflow_phase;
  const hostBudgetTotal = __workflow_budget_total;
  const hostBudgetSpent = __workflow_budget_spent;
  const hostBudgetRemaining = __workflow_budget_remaining;

  const MAX_ITEMS = __MAX_ITEMS__;
  const taskErrorText = (err) => String(err && err.message !== undefined ? err.message : err);

  // Typed slot errors (R9). Every error thrown by task() carries a
  // host-assigned `kind` copied off the task envelope; anything else that
  // reaches a slot was thrown by the script itself and reports as "script".
  //
  // The kind is read from the error object and never guessed from message
  // text. A substring classifier let a child's own words ("...budget
  // exhausted...", "...responseSchema...") forge a fatal classification and
  // abort a healthy run, and it could not tell a genuine subagent failure
  // apart from a plain `throw new Error(...)` in a stage.
  const HOST_KINDS = ["admission", "budget", "cancelled", "agent", "schema", "driver"];
  const SCRIPT_KIND = "script";
  const taskErrorKind = (err) =>
    err !== null && typeof err === "object" && HOST_KINDS.indexOf(err.kind) !== -1
      ? err.kind
      : SCRIPT_KIND;
  // Fatal kinds are never absorbed into a slot value: cancellation is the
  // run's own deadline, and a schema breach means the contract the caller
  // explicitly asked for was not met. `mode: "partial"` opts out for schema
  // (and only schema) by keeping it as a structured slot value instead.
  const isFatalTaskError = (err) => {
    const kind = taskErrorKind(err);
    return kind === "cancelled" || kind === "schema";
  };

  // Stamp the resolved kind onto an error that is about to be rethrown, so a
  // script's own `catch (err) { err.kind }` reads the same vocabulary the
  // slot classifier used. Host errors already carry theirs; this only names
  // the script throws, which would otherwise surface as `undefined`.
  const stampKind = (err, kind) => {
    if (err !== null && typeof err === "object" && err.kind === undefined) {
      try {
        err.kind = kind;
      } catch (_) {
        // A frozen error keeps whatever it has; the log line still names it.
      }
    }
    return err;
  };

  const SLOT_MODES = ["settled", "fail-fast", "partial"];
  // `settled` is the default and is exactly today's behavior: a non-fatal
  // slot failure resolves to `null` so an author need not handle every error.
  // An unrecognized mode throws rather than silently falling back — a typo
  // like `mode: "failfast"` used to read as `settled` and quietly keep
  // dropping slots the author believed were now fatal.
  const slotMode = (fn, opts) => {
    if (opts === null || typeof opts !== "object" || opts.mode === undefined) {
      return "settled";
    }
    if (SLOT_MODES.indexOf(opts.mode) === -1) {
      throw new Error(
        fn + "(): unknown mode " + JSON.stringify(opts.mode) +
        "; expected one of " + SLOT_MODES.join(", ")
      );
    }
    return opts.mode;
  };

  // The failure ledger for one fan-out, attached to the resolved array as a
  // non-enumerable `errors` property. Non-enumerable and non-index, so the
  // array's contents, length, and JSON encoding are byte-identical to before:
  // `results.filter(Boolean)` still works, and a script that wants to know
  // WHY a slot is null can now ask instead of guessing.
  const attachSlotErrors = (results, errors) => {
    errors.sort((a, b) => a.index - b.index);
    Object.defineProperty(results, "errors", {
      value: Object.freeze(errors.map((entry) => Object.freeze(entry))),
      enumerable: false,
      configurable: false,
      writable: false,
    });
    return results;
  };

  globalThis.task = async (opts) => {
    if (opts === null || typeof opts !== "object") {
      throw new TypeError("task(): expected an options object");
    }
    const envelope = JSON.parse(await hostTask(JSON.stringify(opts)));
    if (envelope.error !== undefined) {
      const err = new Error(envelope.error);
      // The host always names the kind; a missing one means an envelope this
      // prelude did not produce, which is not a typed task failure.
      err.kind = HOST_KINDS.indexOf(envelope.error_kind) !== -1
        ? envelope.error_kind
        : SCRIPT_KIND;
      throw err;
    }
    return envelope.value;
  };

  globalThis.parallel = (thunks, opts) => {
    if (!Array.isArray(thunks)) {
      throw new TypeError("parallel(): expected an array of thunks");
    }
    if (thunks.length > MAX_ITEMS) {
      throw new Error("parallel(): max " + MAX_ITEMS + " items per call");
    }
    const mode = slotMode("parallel", opts);
    const failFast = mode === "fail-fast";
    const partial = mode === "partial";
    const errors = [];
    // Returns the slot value, or throws to reject the whole fan-out.
    const onSlotError = (index, err) => {
      const kind = taskErrorKind(err);
      const message = taskErrorText(err);
      stampKind(err, kind);
      // Cancellation is the run's deadline in every mode, partial included.
      if (kind === "cancelled") throw err;
      if (partial) {
        // Opt-in partial mode: every non-cancellation slot failure becomes a
        // structured value the script can branch on. It never masquerades as
        // a success -- `__taskError` is the whole point of the shape.
        errors.push({ index: index, kind: kind, message: message });
        hostLog(
          "parallel(): partial mode kept a failed slot as __taskError (kind=" +
          kind + ", slot " + index + "): " + message
        );
        return { __taskError: { index: index, kind: kind, message: message } };
      }
      if (isFatalTaskError(err)) throw err;
      if (failFast) {
        hostLog(
          "parallel(): fail-fast slot error (kind=" + kind + ", slot " + index + "): " + message
        );
        throw err;
      }
      errors.push({ index: index, kind: kind, message: message });
      hostLog(
        "parallel(): dropped a failed slot as null (kind=" + kind + ", slot " + index + "): " +
        message
      );
      hostSlotDropped("parallel", kind, index);
      return null;
    };
    const slots = thunks.map((thunk, index) => {
      try {
        return Promise.resolve(typeof thunk === "function" ? thunk() : thunk)
          .catch((err) => onSlotError(index, err));
      } catch (err) {
        try {
          return onSlotError(index, err);
        } catch (rethrown) {
          return Promise.reject(rethrown);
        }
      }
    });
    return Promise.all(slots).then((results) => {
      // A fan-out where nothing survived is a dead fan-out, not resilience.
      // The default stays ergonomic (the array still resolves) but the run
      // log says so in one line an operator can grep for, and the structured
      // event below lets the host status classifier refuse to call such a
      // run a plain success.
      if (results.length > 0 && errors.length === results.length) {
        hostLog(
          "parallel(): every slot failed (" + errors.length + " of " + results.length +
          "); no work survived this fan-out"
        );
        hostEverySlotFailed("parallel", errors.length, results.length);
      }
      return attachSlotErrors(results, errors);
    });
  };

  globalThis.pipeline = (items, ...stages) => {
    if (!Array.isArray(items)) {
      throw new TypeError("pipeline(): expected an array of items");
    }
    if (items.length > MAX_ITEMS) {
      throw new Error("pipeline(): max " + MAX_ITEMS + " items per call");
    }
    // Options overload: pipeline(items, { stages: [...], mode: "fail-fast" }).
    let mode = "settled";
    if (
      stages.length === 1 &&
      stages[0] !== null &&
      typeof stages[0] === "object" &&
      Array.isArray(stages[0].stages)
    ) {
      mode = slotMode("pipeline", stages[0]);
      stages = stages[0].stages;
    }
    const failFast = mode === "fail-fast";
    const partial = mode === "partial";
    const errors = [];
    return Promise.all(items.map(async (item, index) => {
      let value = item;
      for (const stage of stages) {
        try {
          value = await stage(value, item, index);
        } catch (err) {
          const kind = taskErrorKind(err);
          const message = taskErrorText(err);
          stampKind(err, kind);
          if (kind === "cancelled") throw err;
          if (partial) {
            errors.push({ index: index, kind: kind, message: message });
            hostLog(
              "pipeline(): partial mode kept item " + index +
              " as __taskError (kind=" + kind + "): " + message
            );
            return { __taskError: { index: index, kind: kind, message: message } };
          }
          if (isFatalTaskError(err)) throw err;
          if (failFast) {
            hostLog(
              "pipeline(): fail-fast stage error on item " + index +
              " (kind=" + kind + "): " + message
            );
            throw err;
          }
          errors.push({ index: index, kind: kind, message: message });
          hostLog(
            "pipeline(): dropped item " + index + " as null (kind=" + kind + "): " + message
          );
          hostSlotDropped("pipeline", kind, index);
          return null;
        }
      }
      return value;
    })).then((results) => {
      if (results.length > 0 && errors.length === results.length) {
        hostLog(
          "pipeline(): every item failed (" + errors.length + " of " + results.length +
          "); no work survived this pipeline"
        );
        hostEverySlotFailed("pipeline", errors.length, results.length);
      }
      return attachSlotErrors(results, errors);
    });
  };

  globalThis.log = (message) => {
    hostLog(typeof message === "string" ? message : (JSON.stringify(message) ?? String(message)));
  };
  globalThis.phase = (title) => {
    hostPhase(String(title));
  };

  const total = hostBudgetTotal();
  globalThis.budget = Object.freeze({
    total: Number.isNaN(total) ? null : total,
    metered: !Number.isNaN(hostBudgetSpent()),
    spent: () => { const value = hostBudgetSpent(); return Number.isNaN(value) ? null : value; },
    remaining: () => { const value = hostBudgetRemaining(); return Number.isNaN(value) ? null : value; },
  });

  for (const name of [
    "__workflow_task",
    "__workflow_log",
    "__workflow_every_slot_failed",
    "__workflow_phase",
    "__workflow_budget_total",
    "__workflow_budget_spent",
    "__workflow_budget_remaining",
  ]) {
    try {
      delete globalThis[name];
    } catch (_) {
      // Non-configurable bindings stay; the inventory test will fail closed.
    }
  }
})();
"#;
