//! One admitted process operation. The Builtin orchestrates; Rust owns all launch and caller facts.
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use super::protocol::{
    CoreRequest, ExecutionRedeemParams, HarnessRunParams, RpcErrorWire, error_code,
};
use super::registry::OwnerState;
use super::supervisor::HostRequestContext;
use super::ticket::{Grant, Presented, Ticket, TicketKind};
use super::tier::HostTier;
use super::{ExtensionHostManager, ManagerShared, activation};
use crate::plugins::PluginRegistry;
use crate::plugins::activation::PluginActivationCapability;
use crate::tools::spec::{ToolContext, ToolError, ToolResult};
use codewhale_config::OwnerRef;
use codewhale_config::PluginAuthority;

const DEADLINE: Duration = Duration::from_secs(120);
const MAX_JOBS: usize = 32;
const MAX_INPUT: usize = 1024 * 1024;
const MAX_REVIEW: usize = 16 * 1024 * 1024;
const MAX_STDOUT: usize = 1024 * 1024;
const MAX_STDERR: usize = 64 * 1024;

#[derive(Clone)]
struct Caller {
    workspace: std::path::PathBuf,
    plugins: Option<Arc<PluginRegistry>>,
    session_id: Option<String>,
    agent_id: Option<String>,
    origin_turn_id: Option<String>,
    origin_call_id: Option<String>,
    authorities: Vec<PluginAuthority>,
    native_owners: Vec<OwnerRef>,
    native_host_generation: Option<u64>,
    requires_native_policy: bool,
}
impl Caller {
    fn capture(context: &ToolContext, shared: &ManagerShared) -> Result<Self, String> {
        Self::from_hook(crate::hooks::HookCaller::from_tool(context), shared, None)
    }
    fn from_hook(
        caller: crate::hooks::HookCaller,
        shared: &ManagerShared,
        only_plugin: Option<&str>,
    ) -> Result<Self, String> {
        let plugins = caller.plugins.clone();
        let mut authorities = Vec::new();
        if let Some(plugins) = plugins.as_ref() {
            for entry in plugins
                .selected_native_entries()
                .iter()
                .filter(|entry| only_plugin.is_none_or(|id| entry.plugin_id == id))
            {
                let authority = plugins
                    .authority_for(&entry.plugin_id)
                    .ok_or("selected Native source has no authority")?;
                if authority.content_hash != entry.content_hash {
                    return Err("selected Native source changed".into());
                }
                if !authorities
                    .iter()
                    .any(|old: &PluginAuthority| old.plugin_id == authority.plugin_id)
                {
                    authorities.push(authority);
                }
            }
        }
        let native_host_generation = if authorities.is_empty() {
            None
        } else {
            Some(
                shared
                    .ready_host(HostTier::Plugin)
                    .map_err(|status| status.to_string())?
                    .generation,
            )
        };
        let native_owners = {
            let registry = shared.registry.lock().expect("registry lock");
            authorities
                .iter()
                .map(|authority| {
                    registry
                        .owner(authority.plugin_id.as_str())
                        .filter(|owner| {
                            owner.state == OwnerState::Active
                                && owner.content_hash == authority.content_hash
                        })
                        .map(|owner| owner.owner.clone())
                        .ok_or_else(|| "Native caller owner is no longer live".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(Self {
            workspace: caller.workspace,
            plugins,
            session_id: caller.session_id.filter(|id| !id.is_empty()),
            agent_id: caller.agent_id,
            origin_turn_id: caller.origin_turn_id,
            origin_call_id: caller.origin_call_id,
            authorities,
            native_owners,
            native_host_generation,
            requires_native_policy: true,
        })
    }
    fn check(&self, shared: &ManagerShared) -> Result<(), String> {
        if (self.requires_native_policy || !self.authorities.is_empty())
            && !activation::extension_host_policy_enabled()
        {
            return Err("extension host is disabled".into());
        }
        if let Some(generation) = self.native_host_generation {
            if shared
                .ready_host(HostTier::Plugin)
                .map_err(|status| status.to_string())?
                .generation
                != generation
            {
                return Err("Native caller host restarted".into());
            }
            let registry = shared.registry.lock().expect("registry lock");
            for owner in &self.native_owners {
                let active = registry
                    .owner(&owner.plugin_id)
                    .filter(|entry| entry.owner == *owner && entry.state == OwnerState::Active)
                    .ok_or("Native caller owner changed")?;
                for entry in self
                    .plugins
                    .as_ref()
                    .expect("Native view")
                    .selected_native_entries()
                    .iter()
                    .filter(|entry| entry.plugin_id == owner.plugin_id)
                {
                    if active.scopes.get(&entry.entry) != Some(&OwnerState::Active) {
                        return Err("Native caller entry was withdrawn".into());
                    }
                }
            }
        }
        if let Some(plugins) = self.plugins.as_ref() {
            if plugins.workspace() != self.workspace {
                return Err("execution workspace does not match its caller".into());
            }
            if let Some(revision) = plugins.caller_selection() {
                let (current, agent) = shared
                    .plugins_for_selection(revision, self.session_id.as_deref())
                    .ok_or("execution caller was detached or changed")?;
                if agent != self.agent_id || !Arc::ptr_eq(plugins, &current) {
                    return Err("execution caller identity changed".into());
                }
            } else if !plugins.selected_native_entries().is_empty() {
                return Err("selected Native execution has no caller receipt".into());
            }
        }
        Ok(())
    }
    fn target(&self, id: &str) -> Value {
        json!({"execution_id":id,"workspace":self.workspace,"session_id":self.session_id,"agent_id":self.agent_id,"origin_turn_id":self.origin_turn_id,"origin_call_id":self.origin_call_id,"selection":self.plugins.as_ref().and_then(|plugins| plugins.caller_selection())})
    }
}

/// An exhaustive pure-transform inventory, never an arbitrary module/function selector.
#[derive(Clone, Copy)]
pub(crate) enum StockOperation {
    WebFilters,
    WebRequest,
    WebProvider,
    WebEntries,
    WebFinalize,
    WebExtract,
    WebImages,
    FinanceQuote,
    FinanceChart,
    ValidateData,
    SpeechOptions,
    SpeechFormat,
    ReviewSourcePrompt,
    ReviewPassPrompt,
    ReviewInteractivePr,
    ReviewReport,
}
impl StockOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::WebFilters => "web_filters",
            Self::WebRequest => "web_request",
            Self::WebProvider => "web_provider",
            Self::WebEntries => "web_entries",
            Self::WebFinalize => "web_finalize",
            Self::WebExtract => "web_extract",
            Self::WebImages => "web_images",
            Self::FinanceQuote => "finance_quote",
            Self::FinanceChart => "finance_chart",
            Self::ValidateData => "validate_data",
            Self::SpeechOptions => "speech_options",
            Self::SpeechFormat => "speech_format",
            Self::ReviewSourcePrompt => "review_source_prompt",
            Self::ReviewPassPrompt => "review_pass_prompt",
            Self::ReviewInteractivePr => "review_interactive_pr",
            Self::ReviewReport => "review_report",
        }
    }
    fn is_review(self) -> bool {
        matches!(
            self,
            Self::ReviewSourcePrompt
                | Self::ReviewPassPrompt
                | Self::ReviewInteractivePr
                | Self::ReviewReport
        )
    }
    fn slots(self) -> u32 {
        if self.is_review() { 16 } else { 1 }
    }
}

fn review_envelope_fits(value: &Value) -> Result<bool, ToolError> {
    super::protocol::encode_frame(&json!({"jsonrpc":"2.0","id":u64::MAX,"result":value}))
        .map(|frame| frame.len() <= MAX_REVIEW)
        .map_err(|_| {
            ToolError::invalid_input("Serialized review envelope exceeds the existing frame limit")
        })
}
fn check_stock_snapshot(operation: StockOperation, input: &Value) -> Result<(), ToolError> {
    if operation.is_review() {
        let projection =
            json!({"kind":"stock_adapter","operation":operation.as_str(),"input":input});
        if !review_envelope_fits(&projection)? {
            return Err(ToolError::invalid_input(
                "Serialized review snapshot/envelope exceeds 16 MiB; no source was truncated",
            ));
        }
    } else if serde_json::to_vec(input)
        .map_err(|_| ToolError::invalid_input("adapter snapshot is not JSON"))?
        .len()
        > MAX_INPUT
    {
        return Err(ToolError::invalid_input("adapter snapshot exceeds 1 MiB"));
    }
    Ok(())
}

type HookRun = Box<dyn FnOnce(CancellationToken) -> crate::hooks::HookResult + Send>;
struct HookLaunch {
    run: HookRun,
    hook: codewhale_config::hooks::Hook,
}
struct PdfLaunch {
    input: crate::tools::pdf::CapturedPdf,
    output: Arc<Mutex<Option<crate::tools::pdf::PdfProcessOutcome>>>,
}
enum OcrLaunch {
    Native {
        input: crate::tools::image_ocr::CapturedOcr,
        output: Arc<Mutex<Option<crate::tools::image_ocr::OcrOutcome>>>,
    },
    Tesseract {
        step: crate::tools::image_ocr::OcrNativeStep,
        output: Arc<Mutex<Option<crate::tools::image_ocr::OcrOutcome>>>,
    },
}
struct Continuation {
    ticket: Ticket,
    target: Value,
}
struct GithubLaunch {
    request: crate::tools::github::host::Request,
    context: Option<ToolContext>,
    output: Arc<Mutex<crate::tools::github::host::RunState>>,
}
struct Launch {
    github: Option<GithubLaunch>,
    ocr: Option<OcrLaunch>,
    pdf: Option<PdfLaunch>,
    stock: Option<(StockOperation, Value)>,
    hook: Option<HookLaunch>,
    command: Option<tokio::process::Command>,
    input: Vec<u8>,
}
struct Job {
    id: String,
    owner: OwnerRef,
    generation: u64,
    caller: Caller,
    target: Value,
    expires: Instant,
    cancel: CancellationToken,
    launch: Mutex<Option<Launch>>,
    ticket: Ticket,
    continuation: Mutex<Option<Continuation>>,
    hook_result: Mutex<Option<crate::hooks::HookResult>>,
    // The worker owns this Arc until process cleanup and blocking receipt checks finish.
    _permit: OwnedSemaphorePermit,
}
impl Job {
    fn check(&self, shared: &ManagerShared) -> Result<(), String> {
        if self.cancel.is_cancelled() || Instant::now() >= self.expires {
            return Err("execution cancelled or expired".into());
        }
        if shared.builtin.host_generation.load(Ordering::SeqCst) != self.generation {
            return Err("execution host restarted".into());
        }
        shared.live_owner_authority(HostTier::Builtin, |registry| {
            registry
                .owner("host:harness")
                .filter(|entry| entry.owner == self.owner && entry.state == OwnerState::Active)
                .map(|entry| entry.owner.clone())
                .ok_or_else(|| "execution builtin owner is no longer live".to_string())
        })?;
        self.caller.check(shared)
    }
    async fn checked(self: &Arc<Self>, shared: &Arc<ManagerShared>) -> Result<(), String> {
        self.check(shared)?;
        let job = Arc::clone(self);
        let policy = activation::extension_host_policy_enabled();
        #[cfg(test)]
        let env_scope = crate::test_support::env_scope_ticket();
        shared
            .engine_handle()?
            .spawn_blocking(move || {
                let _policy = activation::PolicyScope::propagate(policy);
                #[cfg(test)]
                let _env_scope = crate::test_support::join_env_scope(env_scope);
                for authority in &job.caller.authorities {
                    crate::plugins::registry::verify_plugin_component_authority(
                        authority,
                        PluginActivationCapability::Native,
                    )?;
                }
                Ok::<(), String>(())
            })
            .await
            .map_err(|_| "execution receipt check failed".to_string())??;
        self.check(shared)
    }
}

pub(super) struct Broker {
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    slots: Arc<Semaphore>,
}
impl Default for Broker {
    fn default() -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(MAX_JOBS)),
        }
    }
}
impl Drop for Broker {
    fn drop(&mut self) {
        for job in self.jobs.get_mut().expect("execution lock").values() {
            job.cancel.cancel();
        }
    }
}
impl Broker {
    fn admit(&self, units: u32) -> Result<OwnedSemaphorePermit, ToolError> {
        Arc::clone(&self.slots)
            .try_acquire_many_owned(units)
            .map_err(|_| ToolError::not_available("execution concurrency limit reached"))
    }
    pub(super) fn revoke_owner(&self, owner: &str) {
        for job in self.jobs.lock().expect("execution lock").values() {
            if job.owner.plugin_id == owner
                || job
                    .caller
                    .authorities
                    .iter()
                    .any(|authority| authority.plugin_id.as_str() == owner)
            {
                job.cancel.cancel();
            }
        }
    }
    pub(super) fn revoke_host(&self, tier: HostTier, generation: u64) {
        for job in self.jobs.lock().expect("execution lock").values() {
            if (tier == HostTier::Builtin && job.generation == generation)
                || (tier == HostTier::Plugin
                    && job.caller.native_host_generation == Some(generation))
            {
                job.cancel.cancel();
            }
        }
    }
    pub(super) fn revoke_scope(&self, plugin_id: &str, scope: &codewhale_config::EntryRef) {
        for job in self.jobs.lock().expect("execution lock").values() {
            if job.caller.plugins.as_ref().is_some_and(|plugins| {
                plugins
                    .selected_native_entries()
                    .iter()
                    .any(|entry| entry.plugin_id == plugin_id && entry.entry == *scope)
            }) {
                job.cancel.cancel();
            }
        }
    }
    pub(super) fn revoke_attachment(&self, id: u64) {
        for job in self.jobs.lock().expect("execution lock").values() {
            if job
                .caller
                .plugins
                .as_ref()
                .and_then(|plugins| plugins.caller_selection())
                .is_some_and(|selected| selected.attachment_id == id)
            {
                job.cancel.cancel();
            }
        }
    }
    pub(super) async fn serve(
        &self,
        shared: &Arc<ManagerShared>,
        generation: u64,
        params: ExecutionRedeemParams,
        cx: HostRequestContext,
    ) -> Result<Value, RpcErrorWire> {
        let refused = |message: String| RpcErrorWire {
            code: error_code::REFUSED,
            message,
            data: None,
        };
        if params.owner.plugin_id != "host:harness" || cx.cancel.is_cancelled() {
            return Err(refused("execution is not admitted".into()));
        }
        let job = self
            .jobs
            .lock()
            .expect("execution lock")
            .get(&params.execution_id)
            .cloned()
            .ok_or_else(|| refused("execution is no longer admitted".into()))?;
        if generation != job.generation || params.owner != job.owner {
            return Err(refused("execution owner or generation changed".into()));
        }
        job.check(shared).map_err(refused)?;
        let target = job
            .continuation
            .lock()
            .expect("execution continuation lock")
            .as_ref()
            .map(|grant| grant.target.clone())
            .unwrap_or_else(|| job.target.clone());
        if let Err(error) = shared.core_calls.tickets.redeem(&Presented {
            ticket: &params.ticket,
            kind: TicketKind::Execution,
            tier: HostTier::Builtin,
            host_generation: generation,
            owner: &params.owner,
            method: "exec/redeem",
            target: Some(&target),
        }) {
            if error.violation {
                cx.violation("too many invalid execution tickets".to_string());
            }
            return Err(RpcErrorWire {
                code: error_code::REFUSED,
                message: error.reason.describe().to_string(),
                data: None,
            });
        }
        let mut guard = CancelOnDrop(job.cancel.clone());
        let shared_worker = Arc::clone(shared);
        let job_worker = Arc::clone(&job);
        let work = dispatch_job(
            shared.engine_handle().map_err(refused)?,
            job_worker,
            move |job| run_job(shared_worker, job),
        );
        let result = tokio::select! {
            result = work => result.map_err(|_| refused("execution worker failed".into()))?,
            _ = cx.cancel.cancelled() => { job.cancel.cancel(); return Err(refused("execution request cancelled".into())); }
        }.map_err(refused)?;
        job.check(shared).map_err(refused)?;
        guard.0 = CancellationToken::new();
        Ok(result)
    }
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
struct Demand<'a>(&'a AtomicU64);
impl Drop for Demand<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
struct Invocation {
    shared: Arc<ManagerShared>,
    job: Arc<Job>,
}
impl Drop for Invocation {
    fn drop(&mut self) {
        self.job.cancel.cancel();
        self.shared.core_calls.tickets.revoke(&self.job.ticket);
        if let Some(grant) = self
            .job
            .continuation
            .lock()
            .expect("execution continuation lock")
            .as_ref()
        {
            self.shared.core_calls.tickets.revoke(&grant.ticket);
        }
        self.shared
            .execution_broker
            .jobs
            .lock()
            .expect("execution lock")
            .remove(&self.job.id);
    }
}

// A dropped request awaits no result, but the Engine worker retains its job and quota
// until its admitted process/receipt cleanup completes. No transient runtime.
fn dispatch_job<F, R>(
    handle: tokio::runtime::Handle,
    job: Arc<Job>,
    run: F,
) -> tokio::task::JoinHandle<Result<Value, String>>
where
    F: FnOnce(Arc<Job>) -> R + Send + 'static,
    R: std::future::Future<Output = Result<Value, String>> + Send + 'static,
{
    handle.spawn(async move {
        let result = run(Arc::clone(&job)).await;
        drop(job);
        result
    })
}

async fn run_job(shared: Arc<ManagerShared>, job: Arc<Job>) -> Result<Value, String> {
    job.checked(&shared).await?;
    let mut launch = job
        .launch
        .lock()
        .expect("execution launch lock")
        .take()
        .ok_or("execution was already redeemed")?;
    if let Some(github) = launch.github.take() {
        let shared_check = Arc::clone(&shared);
        let job_check = Arc::clone(&job);
        let result = crate::tools::github::host::run(
            github.request,
            github.context,
            job.cancel.clone(),
            move || {
                let shared = Arc::clone(&shared_check);
                let job = Arc::clone(&job_check);
                async move { job.checked(&shared).await.map_err(ToolError::not_available) }
            },
            Arc::clone(&github.output),
        )
        .await;
        let projection = match result.as_ref() {
            Ok(outcome) => Ok(
                json!({"kind":"stock_adapter","operation":"github_result","input":&outcome.projection}),
            ),
            Err(_) => Err("GitHub Core operation failed".to_string()),
        };
        github.output.lock().expect("GitHub result lock").result = Some(result);
        // A Core fault never crosses into the Host diagnostic/projection.
        let projection = projection?;
        job.checked(&shared).await?;
        return Ok(projection);
    }
    if let Some(ocr) = launch.ocr.take() {
        match ocr {
            OcrLaunch::Native { input, output } => {
                let worker = Arc::clone(&job);
                let shared_worker = Arc::clone(&shared);
                #[cfg(test)]
                let scope = crate::test_support::env_scope_ticket();
                let policy = activation::extension_host_policy_enabled();
                let step = shared
                    .engine_handle()?
                    .spawn_blocking(move || {
                        let _policy = activation::PolicyScope::propagate(policy);
                        #[cfg(test)]
                        let _scope = crate::test_support::join_env_scope(scope);
                        worker.check(&shared_worker)?;
                        // Retain the same job/quota until non-preemptible Vision FFI exits.
                        let step = input.native_step();
                        worker.check(&shared_worker)?;
                        Ok::<_, String>(step)
                    })
                    .await
                    .map_err(|_| "OCR Native worker failed".to_string())??;
                job.checked(&shared).await?;
                let mut projection = step.projection();
                if step.needs_tesseract() {
                    let mut continuation = job
                        .continuation
                        .lock()
                        .expect("execution continuation lock");
                    job.check(&shared)?;
                    if continuation.is_some() {
                        return Err("OCR continuation was already issued".into());
                    }
                    let remaining = job.expires.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err("OCR deadline exhausted".into());
                    }
                    let mut target = job.target.clone();
                    target["operation"] = json!("ocr_tesseract");
                    target["captured_sha256"] = json!(step.digest());
                    let ticket = shared.core_calls.tickets.mint(Grant {
                        kind: TicketKind::Execution,
                        tier: HostTier::Builtin,
                        host_generation: job.generation,
                        owner: job.owner.clone(),
                        method: "exec/redeem",
                        target: target.clone(),
                        ttl: remaining,
                        uses: 1,
                    });
                    projection["next_ticket"] = json!(ticket.expose());
                    *job.launch.lock().expect("execution launch lock") = Some(Launch {
                        github: None,
                        ocr: Some(OcrLaunch::Tesseract { step, output }),
                        pdf: None,
                        stock: None,
                        hook: None,
                        command: None,
                        input: Vec::new(),
                    });
                    *continuation = Some(Continuation { ticket, target });
                } else {
                    *output.lock().expect("OCR result lock") = Some(step.finish());
                }
                job.checked(&shared).await?;
                return Ok(projection);
            }
            OcrLaunch::Tesseract { step, output } => {
                let result = step
                    .tesseract(
                        Some(&job.cancel),
                        tokio::time::Instant::from_std(job.expires),
                    )
                    .await;
                let projection = result.projection();
                *output.lock().expect("OCR result lock") = Some(result);
                job.checked(&shared).await?;
                return Ok(projection);
            }
        }
    }
    if let Some(pdf) = launch.pdf.take() {
        let result = crate::tools::pdf::run_pdf_driver(pdf.input, Some(&job.cancel)).await;
        let projection = result.projection();
        *pdf.output.lock().expect("PDF result lock") = Some(result);
        job.checked(&shared).await?;
        return Ok(projection);
    }
    if let Some((operation, input)) = launch.stock.take() {
        job.checked(&shared).await?;
        return Ok(json!({"kind":"stock_adapter","operation":operation.as_str(),"input":input}));
    }
    if let Some(launch_hook) = launch.hook.take() {
        let job_worker = Arc::clone(&job);
        let shared_worker = Arc::clone(&shared);
        let policy = activation::extension_host_policy_enabled();
        #[cfg(test)]
        let env_scope = crate::test_support::env_scope_ticket();
        let result=shared.engine_handle()?.spawn_blocking(move || {
            let _policy=activation::PolicyScope::propagate(policy);
            #[cfg(test)] let _env_scope=crate::test_support::join_env_scope(env_scope);
            job_worker.check(&shared_worker)?;
            crate::hooks::authority::verify_hook(&launch_hook.hook)?;
            if let Some(native)=launch_hook.hook.native_shell.as_ref() {
                shared_worker.registry.lock().expect("registry lock").check_shell_hook(native)?;
            }
            let result=(launch_hook.run)(job_worker.cancel.clone());
            crate::hooks::authority::verify_hook(&launch_hook.hook)?;
            if let Some(native)=launch_hook.hook.native_shell.as_ref() {
                shared_worker.registry.lock().expect("registry lock").check_shell_hook(native)?;
            }
            job_worker.check(&shared_worker)?;
            // ShellEnv stdout may contain credentials. It is retained only in Rust.
            let projection=if launch_hook.hook.event==codewhale_config::hooks::HookEvent::ShellEnv {
                json!({"kind":"hook","event":"shell_env","success":result.success,"exit_code":result.exit_code,"keys":crate::hooks::shell_env_keys(&result.stdout)})
            } else if launch_hook.hook.native_shell.is_some() {
                json!({"kind":"hook","event":launch_hook.hook.event.as_str(),"success":result.success,"exit_code":result.exit_code,"stdout":result.stdout,"stderr":result.stderr})
            } else {
                json!({"kind":"hook","event":launch_hook.hook.event.as_str(),"success":result.success,"exit_code":result.exit_code})
            };
            *job_worker.hook_result.lock().expect("hook result lock")=Some(result);
            Ok::<Value,String>(projection)
        }).await.map_err(|_|"hook worker was lost".to_string())??;
        job.checked(&shared).await?;
        return Ok(result);
    }
    let command = launch
        .command
        .as_mut()
        .ok_or("execution command is missing")?;
    // The selected Builtin never sees the environment, argv, input, cwd or decision.
    codewhale_runtime::child_env::apply_to_tokio_command(
        command,
        std::iter::empty::<(&str, &str)>(),
    );
    job.check(&shared)?;
    let stop = async {
        let remaining = job.expires.saturating_duration_since(Instant::now());
        tokio::select! { _ = job.cancel.cancelled() => {}, _ = tokio::time::sleep(remaining) => {}, }
    };
    let output = codewhale_runtime::process_tree::contained_output_with_input_bounded(
        command,
        launch.input,
        MAX_STDOUT,
        MAX_STDERR,
        stop,
    )
    .await
    .map_err(|_| "execution spawn or output collection failed".to_string())?;
    if output.stopped {
        return Err("execution cancelled or timed out".into());
    }
    job.checked(&shared).await?;
    Ok(
        json!({"success":output.output.status.success(),"stdout":String::from_utf8_lossy(&output.output.stdout),"stderr":String::from_utf8_lossy(&output.output.stderr)}),
    )
}

impl ExtensionHostManager {
    /// Same weighted quota for the nonpreemptible Core capture worker.
    pub(crate) fn admit_review_capture(&self) -> Result<OwnedSemaphorePermit, ToolError> {
        self.shared
            .execution_broker
            .admit(StockOperation::ReviewPassPrompt.slots())
    }

    /// Called only by the already planned/gated Script and Command ToolSpecs.
    pub(crate) async fn execute_script(
        &self,
        command: tokio::process::Command,
        input: Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let input = serde_json::to_vec(&input)
            .map_err(|_| ToolError::invalid_input("script input is not JSON"))?;
        if input.len() > MAX_INPUT {
            return Err(ToolError::invalid_input("script input exceeds 1 MiB"));
        }
        self.execute_launch(
            Launch {
                github: None,
                ocr: None,
                pdf: None,
                command: Some(command),
                input,
                hook: None,
                stock: None,
            },
            context,
            DEADLINE,
            None,
        )
        .await
    }

    /// Captured data only. Rust has already performed the planned network/file access
    /// and parser checks. No URL, command, environment or writable handle is granted.
    pub(crate) async fn execute_stock(
        &self,
        operation: StockOperation,
        input: Value,
        context: &ToolContext,
        budget: Duration,
    ) -> Result<ToolResult, ToolError> {
        if operation.is_review()
            && !context
                .features
                .enabled(codewhale_runtime::features::Feature::ReviewHost)
        {
            return Err(ToolError::not_available(
                "Review Host backend is not selected",
            ));
        }
        // Admit before constructing the encoded snapshot, and carry this exact
        // permit into the actual job. Concurrent validation cannot outrun quota.
        let admission = self.shared.execution_broker.admit(operation.slots())?;
        check_stock_snapshot(operation, &input)?;
        // Adopt the existing Engine scheduler; this never creates a runtime.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            self.bind_engine_handle(handle);
        }
        self.execute_launch(
            Launch {
                github: None,
                ocr: None,
                pdf: None,
                command: None,
                input: Vec::new(),
                hook: None,
                stock: Some((operation, input)),
            },
            context,
            budget,
            Some(admission),
        )
        .await
    }

    /// Only the canonical already-planned ToolSpec can capture a write operation.
    pub(crate) async fn execute_github(
        &self,
        request: crate::tools::github::host::Request,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        if !context
            .features
            .enabled(codewhale_runtime::features::Feature::GithubHost)
        {
            return Err(ToolError::not_available(
                "GitHub Host backend is not selected",
            ));
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            self.bind_engine_handle(handle);
        }
        let caller = Caller::capture(context, &self.shared).map_err(ToolError::not_available)?;
        let cancel = context
            .cancel_token
            .as_ref()
            .map(CancellationToken::child_token)
            .unwrap_or_default();
        let mut budget = DEADLINE;
        if let Some(deadline) = context.turn_deadline {
            budget = budget.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
        }
        self.execute_github_captured(request, Some(context.clone()), caller, cancel, budget)
            .await
    }
    /// Read-only operator path: actual app/session/agent identity, no synthetic tool approval.
    pub(crate) async fn execute_github_review(
        &self,
        caller: crate::hooks::HookCaller,
        id: String,
    ) -> Result<ToolResult, ToolError> {
        let session = caller
            .session_id
            .clone()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                ToolError::not_available("Feedback review requires the current session")
            })?;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            self.bind_engine_handle(handle);
        }
        let caller =
            Caller::from_hook(caller, &self.shared, None).map_err(ToolError::not_available)?;
        self.execute_github_captured(
            crate::tools::github::host::Request::Read {
                session,
                id,
                operator: true,
            },
            None,
            caller,
            CancellationToken::new(),
            Duration::from_secs(30),
        )
        .await
    }
    async fn execute_github_captured(
        &self,
        request: crate::tools::github::host::Request,
        context: Option<ToolContext>,
        caller: Caller,
        cancel: CancellationToken,
        budget: Duration,
    ) -> Result<ToolResult, ToolError> {
        let output = Arc::new(Mutex::new(crate::tools::github::host::RunState::default()));
        let result = self
            .execute_launch_for_caller(
                Launch {
                    github: Some(GithubLaunch {
                        request,
                        context,
                        output: Arc::clone(&output),
                    }),
                    ocr: None,
                    pdf: None,
                    stock: None,
                    hook: None,
                    command: None,
                    input: Vec::new(),
                },
                caller,
                cancel,
                budget,
                None,
            )
            .await;
        crate::tools::github::host::finish_run(&output, result)
    }

    /// Only the planned/gated PDF consumers can construct this captured parser job.
    pub(crate) async fn execute_pdf(
        &self,
        input: crate::tools::pdf::CapturedPdf,
        context: &ToolContext,
    ) -> Result<(crate::tools::pdf::PdfProcessOutcome, ToolResult), ToolError> {
        if !context
            .features
            .enabled(codewhale_runtime::features::Feature::PdfHost)
        {
            return Err(ToolError::not_available("PDF Host backend is not selected"));
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            self.bind_engine_handle(handle);
        }
        let mut budget = input.timeout;
        if let Some(deadline) = context.turn_deadline {
            budget = budget.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
        }
        let output = Arc::new(Mutex::new(None));
        let result = self
            .execute_launch(
                Launch {
                    github: None,
                    ocr: None,
                    pdf: Some(PdfLaunch {
                        input,
                        output: Arc::clone(&output),
                    }),
                    stock: None,
                    hook: None,
                    command: None,
                    input: Vec::new(),
                },
                context,
                budget,
                None,
            )
            .await?;
        let output = output
            .lock()
            .expect("PDF result lock")
            .take()
            .ok_or_else(|| ToolError::execution_failed("PDF driver has no captured output"))?;
        Ok((output, result))
    }

    pub(crate) async fn execute_ocr(
        &self,
        input: crate::tools::image_ocr::CapturedOcr,
        context: &ToolContext,
    ) -> Result<(crate::tools::image_ocr::OcrOutcome, ToolResult), ToolError> {
        if !context
            .features
            .enabled(codewhale_runtime::features::Feature::OcrHost)
        {
            return Err(ToolError::not_available("OCR Host backend is not selected"));
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            self.bind_engine_handle(handle);
        }
        let budget = context
            .turn_deadline
            .map(|deadline| deadline.saturating_duration_since(tokio::time::Instant::now()))
            .unwrap_or(DEADLINE)
            .min(DEADLINE);
        let output = Arc::new(Mutex::new(None));
        let result = self
            .execute_launch(
                Launch {
                    github: None,
                    ocr: Some(OcrLaunch::Native {
                        input,
                        output: Arc::clone(&output),
                    }),
                    pdf: None,
                    stock: None,
                    hook: None,
                    command: None,
                    input: Vec::new(),
                },
                context,
                budget,
                None,
            )
            .await;
        let decision = match result {
            Err(_)
                if context
                    .cancel_token
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled) =>
            {
                return Err(ToolError::cancelled("Image OCR was cancelled"));
            }
            other => other?,
        };
        let outcome = output
            .lock()
            .expect("OCR result lock")
            .take()
            .ok_or_else(|| ToolError::execution_failed("OCR driver has no captured output"))?;
        Ok((outcome, decision))
    }

    async fn execute_launch(
        &self,
        launch: Launch,
        context: &ToolContext,
        budget: Duration,
        admission: Option<OwnedSemaphorePermit>,
    ) -> Result<ToolResult, ToolError> {
        let caller = Caller::capture(context, &self.shared).map_err(ToolError::not_available)?;
        let cancel = context
            .cancel_token
            .as_ref()
            .map(CancellationToken::child_token)
            .unwrap_or_default();
        self.execute_launch_for_caller(launch, caller, cancel, budget, admission)
            .await
    }

    async fn execute_launch_for_caller(
        &self,
        launch: Launch,
        mut caller: Caller,
        cancel: CancellationToken,
        budget: Duration,
        admission: Option<OwnedSemaphorePermit>,
    ) -> Result<ToolResult, ToolError> {
        self.shared
            .engine_handle()
            .map_err(ToolError::not_available)?;
        let budget = budget.min(DEADLINE);
        if budget.is_zero() {
            return Err(ToolError::Timeout { seconds: 0 });
        }
        let expires = Instant::now() + budget;
        let stock = launch.stock.is_some()
            || launch.pdf.is_some()
            || launch.ocr.is_some()
            || launch.github.is_some();
        caller.requires_native_policy = !stock;
        caller
            .check(&self.shared)
            .map_err(ToolError::not_available)?;
        let operation = launch.stock.as_ref().map(|(operation, _)| *operation);
        let review = operation.is_some_and(StockOperation::is_review);
        let permit = match admission {
            Some(admission) => admission,
            None => self
                .shared
                .execution_broker
                .admit(operation.map_or(1, StockOperation::slots))?,
        };
        let demand = if stock {
            &self.shared.stock_users
        } else {
            &self.shared.harness_users
        };
        demand.fetch_add(1, Ordering::SeqCst);
        let _demand = Demand(demand);
        let ready = async {
            if stock {
                self.ensure_stock_builtin().await
            } else {
                self.ensure_harness_builtin().await
            }
        };

        let deadline = tokio::time::Instant::from_std(expires);
        let (host, owner, generation) = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ToolError::not_available("execution cancelled")),
            ready = tokio::time::timeout_at(deadline, ready) => ready
                .map_err(|_| ToolError::Timeout { seconds: budget.as_secs().saturating_add(u64::from(budget.subsec_nanos() != 0)) })?
                .map_err(ToolError::not_available)?,
        };
        caller
            .check(&self.shared)
            .map_err(ToolError::not_available)?;
        let id = uuid::Uuid::new_v4().to_string();
        let mut target = caller.target(&id);
        if let Some(github) = launch.github.as_ref() {
            target["operation"] = json!("github_capture");
            target["snapshot_sha256"] = json!(codewhale_runtime::hashing::sha256_hex(
                &serde_json::to_vec(&github.request)
                    .map_err(|_| ToolError::invalid_input("GitHub operation is not JSON"))?
            ));
        }
        if let Some(OcrLaunch::Native { input, .. }) = launch.ocr.as_ref() {
            target["operation"] = json!("ocr_native");
            target["captured_sha256"] = json!(input.digest());
        }
        if let Some(pdf) = launch.pdf.as_ref() {
            target["operation"] = json!("pdf_extract");
            target["captured_sha256"] = json!(pdf.input.digest());
        }
        if let Some((operation, input)) = launch.stock.as_ref() {
            target["operation"] = json!(operation.as_str());
            target["snapshot_sha256"] = json!(codewhale_runtime::hashing::sha256_hex(
                &serde_json::to_vec(input)
                    .map_err(|_| ToolError::invalid_input("adapter snapshot is not JSON"))?
            ));
        }
        let remaining = expires.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ToolError::not_available("execution deadline exhausted"));
        }
        let ticket = self.shared.core_calls.tickets.mint(Grant {
            kind: TicketKind::Execution,
            tier: HostTier::Builtin,
            host_generation: generation,
            owner: owner.clone(),
            method: "exec/redeem",
            target: target.clone(),
            ttl: remaining,
            uses: 1,
        });
        let job = Arc::new(Job {
            id: id.clone(),
            owner: owner.clone(),
            generation,
            caller,
            target,
            expires,
            cancel,
            launch: Mutex::new(Some(launch)),
            ticket: ticket.clone(),
            continuation: Mutex::new(None),
            hook_result: Mutex::new(None),
            _permit: permit,
        });
        self.shared
            .execution_broker
            .jobs
            .lock()
            .expect("execution lock")
            .insert(id.clone(), Arc::clone(&job));
        let _invocation = Invocation {
            shared: Arc::clone(&self.shared),
            job: Arc::clone(&job),
        };
        let request = host.call(
            CoreRequest::HarnessRun(HarnessRunParams {
                owner,
                execution_id: id,
                ticket: ticket.expose().to_string(),
                // Round the peer timer up: Core owns the exact deadline and must
                // settle expiry before a rounded-down Host timer reports a runner fault.
                deadline_ms: remaining.as_nanos().div_ceil(1_000_000).max(1) as u64,
                hook: None,
            }),
            Some("host:harness".into()),
        );
        let value = tokio::select! {
            biased;
            _ = job.cancel.cancelled() => return Err(ToolError::not_available("execution cancelled")),
            value = tokio::time::timeout_at(deadline, request) => value
                .map_err(|_| ToolError::Timeout { seconds: budget.as_secs().saturating_add(u64::from(budget.subsec_nanos() != 0)) })?
                .map_err(|_| ToolError::execution_failed("Builtin runner failed; no Rust fallback was attempted"))?,
        };
        job.checked(&self.shared)
            .await
            .map_err(ToolError::not_available)?;
        if review
            && !review_envelope_fits(&value).map_err(|_| {
                ToolError::execution_failed(
                    "Serialized review result exceeds the existing frame limit",
                )
            })?
        {
            return Err(ToolError::execution_failed(
                "Serialized review result/envelope exceeds 16 MiB",
            ));
        }
        if stock
            && !review
            && serde_json::to_vec(&value)
                .map_err(|_| ToolError::execution_failed("Builtin result is not JSON"))?
                .len()
                > MAX_INPUT
        {
            return Err(ToolError::execution_failed("Builtin result exceeds 1 MiB"));
        }
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            serde_json::from_value(
                value
                    .get("result")
                    .cloned()
                    .ok_or_else(|| ToolError::execution_failed("Builtin result missing"))?,
            )
            .map_err(|_| ToolError::execution_failed("Builtin result malformed"))
        } else {
            Err(ToolError::execution_failed(
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("Builtin result failed"),
            ))
        }
    }
}

impl ExtensionHostManager {
    pub(crate) async fn execute_hook<F>(
        &self,
        caller: crate::hooks::HookCaller,
        hook: codewhale_config::hooks::Hook,
        timeout: Duration,
        query: String,
        run: F,
    ) -> Result<crate::hooks::HookResult, String>
    where
        F: FnOnce(CancellationToken) -> crate::hooks::HookResult + Send + 'static,
    {
        self.shared.engine_handle()?;
        let deadline_ms = u64::try_from(timeout.as_millis())
            .map_err(|_| "hook timeout exceeds the execution bound")?;
        if deadline_ms == 0 || deadline_ms > i32::MAX as u64 {
            return Err("hook timeout exceeds the execution bound".into());
        }
        let caller = Caller::from_hook(
            caller,
            &self.shared,
            Some(
                hook.native_shell
                    .as_ref()
                    .map_or("", |native| native.owner.plugin_id.as_str()),
            ),
        )?;
        caller.check(&self.shared)?;
        let permit = Arc::clone(&self.shared.execution_broker.slots)
            .try_acquire_owned()
            .map_err(|_| "execution concurrency limit reached")?;
        self.shared.harness_users.fetch_add(1, Ordering::SeqCst);
        let _demand = Demand(&self.shared.harness_users);
        let (host, owner, generation) = self.ensure_harness_builtin().await?;
        caller.check(&self.shared)?;
        let id = uuid::Uuid::new_v4().to_string();
        let metadata = super::protocol::HookDispatchWire {
            event: hook.event.as_str().into(),
            dialect: hook
                .native_shell
                .as_ref()
                .map_or("codewhale".into(), |n| n.dialect.clone()),
            point: hook
                .native_shell
                .as_ref()
                .map_or(hook.event.as_str().into(), |n| n.point.clone()),
            matcher: hook.native_shell.as_ref().and_then(|n| n.matcher.clone()),
            query,
        };
        let target = json!({"caller":caller.target(&id),"hook":metadata});
        let ticket = self.shared.core_calls.tickets.mint(Grant {
            kind: TicketKind::Execution,
            tier: HostTier::Builtin,
            host_generation: generation,
            owner: owner.clone(),
            method: "exec/redeem",
            target: target.clone(),
            ttl: timeout,
            uses: 1,
        });
        let job = Arc::new(Job {
            id: id.clone(),
            owner: owner.clone(),
            generation,
            caller,
            target,
            expires: Instant::now()
                .checked_add(timeout)
                .ok_or("hook timeout is not representable")?,
            cancel: CancellationToken::new(),
            launch: Mutex::new(Some(Launch {
                github: None,
                ocr: None,
                pdf: None,
                stock: None,
                command: None,
                input: Vec::new(),
                hook: Some(HookLaunch {
                    run: Box::new(run),
                    hook: hook.clone(),
                }),
            })),
            ticket: ticket.clone(),
            continuation: Mutex::new(None),
            hook_result: Mutex::new(None),
            _permit: permit,
        });
        self.shared
            .execution_broker
            .jobs
            .lock()
            .expect("execution lock")
            .insert(id.clone(), Arc::clone(&job));
        let _invocation = Invocation {
            shared: Arc::clone(&self.shared),
            job: Arc::clone(&job),
        };
        let value = host
            .call(
                CoreRequest::HarnessRun(HarnessRunParams {
                    owner,
                    execution_id: id,
                    ticket: ticket.expose().into(),
                    deadline_ms,
                    hook: Some(metadata),
                }),
                Some("host:harness".into()),
            )
            .await
            .map_err(|_| "Builtin hook runner failed; no legacy fallback was attempted")?;
        job.checked(&self.shared).await?;
        if value.get("hook_skipped").and_then(Value::as_bool) == Some(true) {
            if job.hook_result.lock().expect("hook result lock").is_some() {
                return Err("hook ran despite its nonmatching receipt".into());
            }
            return Ok(crate::hooks::HookResult {
                name: hook.name,
                success: true,
                background: false,
                strict: false,
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
                duration: Duration::ZERO,
                error: None,
            });
        }
        if value.get("hook_completed").and_then(Value::as_bool) != Some(true) {
            return Err("Builtin hook receipt is missing".into());
        }
        let mut result = job
            .hook_result
            .lock()
            .expect("hook result lock")
            .take()
            .ok_or("hook process produced no receipt")?;
        // Only Native dialect codecs may project a bounded proposal; final steering stays Rust-owned.
        if hook.native_shell.is_some()
            && hook.event != codewhale_config::hooks::HookEvent::ShellEnv
            && let Some(stdout) = value.get("proposal").and_then(Value::as_str)
        {
            if stdout.len() > 64 * 1024 {
                return Err("hook proposal exceeds 64 KiB".into());
            }
            result.stdout = stdout.into();
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ExtensionHostOptions, ticket::TicketTable};
    use super::*;
    fn caller() -> Caller {
        Caller {
            workspace: std::path::PathBuf::from("."),
            plugins: None,
            session_id: Some("session".into()),
            agent_id: Some("agent".into()),
            origin_turn_id: Some("turn".into()),
            origin_call_id: Some("call".into()),
            authorities: Vec::new(),
            native_owners: Vec::new(),
            native_host_generation: None,
            requires_native_policy: true,
        }
    }
    fn owner() -> OwnerRef {
        OwnerRef {
            plugin_id: "host:harness".into(),
            generation: 3,
            owner_token: "test-owner".into(),
        }
    }
    fn grant(owner: OwnerRef, target: Value) -> Grant {
        Grant {
            kind: TicketKind::Execution,
            tier: HostTier::Builtin,
            host_generation: 4,
            owner,
            method: "exec/redeem",
            target,
            ttl: DEADLINE,
            uses: 1,
        }
    }
    #[test]
    fn review_purpose_bounds_use_the_encoded_frame_and_keep_other_helpers_at_one_mib() {
        let large = json!({"kind":"cli_diff","diff":"漢".repeat(400_000)+"FINAL_END"});
        assert!(serde_json::to_vec(&large).unwrap().len() > MAX_INPUT);
        assert!(check_stock_snapshot(StockOperation::ReviewSourcePrompt, &large).is_ok());
        assert!(check_stock_snapshot(StockOperation::SpeechOptions, &large).is_err());
        let escaped = json!({"kind":"cli_diff","diff":"\0".repeat(3*1024*1024)});
        assert!(escaped["diff"].as_str().unwrap().len() < MAX_REVIEW);
        assert!(check_stock_snapshot(StockOperation::ReviewSourcePrompt, &escaped).is_err());
        let result = json!({"ok":true,"result":{"content":"\0".repeat(3*1024*1024),"success":true,"metadata":null}});
        assert!(!review_envelope_fits(&result).unwrap());
        let frame = super::super::protocol::encode_frame(
            &json!({"jsonrpc":"2.0","id":u64::MAX,"result":large}),
        )
        .unwrap();
        assert_eq!(
            review_envelope_fits(&large).unwrap(),
            frame.len() <= MAX_REVIEW
        );
    }

    #[test]
    fn weighted_review_admission_reuses_all_thirty_two_execution_slots() {
        let broker = Broker::default();
        let first = broker.admit(StockOperation::ReviewReport.slots()).unwrap();
        let second = broker
            .admit(StockOperation::ReviewPassPrompt.slots())
            .unwrap();
        assert_eq!(broker.slots.available_permits(), 0);
        assert!(broker.admit(1).is_err());
        assert!(broker.admit(16).is_err());
        drop(first);
        assert_eq!(broker.slots.available_permits(), 16);
        let ordinary = broker.admit(1).unwrap();
        assert!(broker.admit(16).is_err());
        drop(second);
        drop(ordinary);
        assert_eq!(broker.slots.available_permits(), MAX_JOBS);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn abandoned_review_capture_worker_retains_weighted_quota_until_completion() {
        let manager = ExtensionHostManager::new(ExtensionHostOptions::default());
        let permit = manager.admit_review_capture().unwrap();
        let occupied = manager.admit_review_capture().unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let waiter = tokio::spawn(crate::tools::github::host::report_worker(
            permit,
            move || {
                started_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                Ok(())
            },
        ));
        started_rx.await.unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        assert!(manager.admit_review_capture().is_err());
        assert_eq!(manager.shared.execution_broker.slots.available_permits(), 0);
        drop(occupied);
        assert_eq!(
            manager.shared.execution_broker.slots.available_permits(),
            16
        );
        finish_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while manager.shared.execution_broker.slots.available_permits() != MAX_JOBS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(manager.admit_review_capture().is_ok());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn actual_review_host_matches_frozen_core_cases_and_accepts_over_one_mib() {
        let _home = crate::test_support::SealedHome::new();
        let _policy = crate::plugins::activation::TestPolicyGuard::extension_host(false);
        let Some(node) = super::super::tests::node_for_tests("review_captured_parity") else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let manager = ExtensionHostManager::new(ExtensionHostOptions {
            runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
            node_override: Some(node),
            root: Some(root.path().join("host")),
            ..Default::default()
        });
        let mut features = codewhale_runtime::features::Features::with_defaults();
        features.enable(codewhale_runtime::features::Feature::ReviewHost);
        let context = ToolContext::new(root.path()).with_features(features);
        let fixture: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/review-host-parity.json"))
                .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let operation = match case["operation"].as_str().unwrap() {
                "review_source_prompt" => StockOperation::ReviewSourcePrompt,
                "review_interactive_pr" => StockOperation::ReviewInteractivePr,
                "review_report" => StockOperation::ReviewReport,
                _ => panic!("unadmitted fixture operation"),
            };
            let result = manager
                .execute_stock(
                    operation,
                    case["input"].clone(),
                    &context,
                    Duration::from_secs(10),
                )
                .await
                .unwrap();
            assert_eq!(result.content, case["content"].as_str().unwrap());
            assert!(result.success);
            assert!(result.metadata.is_none());
        }
        let input = json!({"kind":"cli_diff","diff":"漢".repeat(400_000)+"FINAL_END"});
        let result = manager
            .execute_stock(
                StockOperation::ReviewSourcePrompt,
                input,
                &context,
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert!(result.content.ends_with("FINAL_END\n\nEnd of diff."));
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let error = manager
            .execute_stock(
                StockOperation::ReviewReport,
                json!({"review":null,"output":"late","posted":false}),
                &context.with_cancel_token(cancelled),
                Duration::from_secs(10),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ToolError::NotAvailable { .. } | ToolError::Cancelled { .. }
        ));
        manager.shutdown().await;
    }

    #[test]
    fn execution_ticket_cannot_redeem_mcp_or_replay_or_change_caller() {
        let table = TicketTable::default();
        let owner = owner();
        let caller = caller();
        let target = caller.target("exact");
        let ticket = table.mint(grant(owner.clone(), target.clone()));
        let mut present = Presented {
            ticket: ticket.expose(),
            kind: TicketKind::McpOperation,
            tier: HostTier::Builtin,
            host_generation: 4,
            owner: &owner,
            method: "exec/redeem",
            target: Some(&target),
        };
        assert!(table.redeem(&present).is_err());
        present.kind = TicketKind::Execution;
        let wrong = json!({"execution_id":"exact","session_id":"another"});
        present.target = Some(&wrong);
        assert!(table.redeem(&present).is_err());
        present.target = Some(&target);
        assert!(table.redeem(&present).is_ok());
        assert!(table.redeem(&present).is_err());
    }
    #[test]
    fn execution_caller_checks_exact_session_agent_and_selected_view() {
        let _policy = activation::PolicyScope::propagate(true);
        let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions::default()));
        let attachment = manager.attach(Arc::new(PluginRegistry::empty(std::path::Path::new("."))));
        attachment.set_identity(Some("session".into()), Some("agent".into()));
        let mut caller = caller();
        caller.plugins = Some(attachment.plugin_view());
        assert!(caller.check(&manager.shared).is_ok());
        caller.session_id = Some("other".into());
        assert!(caller.check(&manager.shared).is_err());
        caller.session_id = Some("session".into());
        caller.agent_id = None;
        assert!(caller.check(&manager.shared).is_err());
        caller.agent_id = Some("agent".into());
        drop(attachment);
        assert!(caller.check(&manager.shared).is_err());
    }
    #[tokio::test(flavor = "current_thread")]
    async fn actual_github_broker_returns_private_core_permission_fault() {
        use codewhale_net::network_policy::{DecisionToml, NetworkPolicy, NetworkPolicyDecider};
        let _home = crate::test_support::SealedHome::new();
        let _policy = crate::plugins::activation::TestPolicyGuard::extension_host(false);
        let Some(node) = super::super::tests::node_for_tests("github_private_core_fault") else {
            return;
        };
        let temp = tempfile::tempdir().unwrap();
        let _repo = crate::test_support::EnvVarGuard::set("GH_REPO", "owner/name");
        let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
            runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
            node_override: Some(node),
            root: Some(temp.path().join("host")),
            ..Default::default()
        }));
        let mut context =
            ToolContext::new(temp.path()).with_network_policy(NetworkPolicyDecider::new(
                NetworkPolicy {
                    default: DecisionToml::Deny,
                    ..Default::default()
                },
                None,
            ));
        context
            .features
            .enable(codewhale_runtime::features::Feature::GithubHost);
        let error = manager
            .execute_github(
                crate::tools::github::host::Request::Comment {
                    target: "issue".into(),
                    number: 1,
                    body: "never sent".into(),
                    dry: false,
                },
                &context,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::PermissionDenied { .. }));
        assert!(
            error
                .to_string()
                .contains("blocked or awaiting network approval")
        );
        assert!(!error.to_string().contains("Builtin runner failed"));
        assert!(!error.to_string().contains("never sent"));
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn actual_ocr_native_cancel_retains_job_quota_until_framework_worker_exits() {
        let _home = crate::test_support::SealedHome::new();
        let _policy = crate::plugins::activation::TestPolicyGuard::extension_host(false);
        let Some(node) = super::super::tests::node_for_tests("ocr_native_quota") else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
            runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
            node_override: Some(node),
            root: Some(root.path().join("host")),
            ..ExtensionHostOptions::default()
        }));
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let announce = Arc::clone(&started);
        let latch = Arc::clone(&release);
        let input = crate::tools::image_ocr::CapturedOcr::for_test(
            root.path(),
            Arc::new(move |_| {
                announce.notify_one();
                let (lock, wake) = &*latch;
                let mut ready = lock.lock().unwrap();
                while !*ready {
                    ready = wake.wait(ready).unwrap();
                }
                Ok(Some("discarded late Native text".into()))
            }),
            None,
        );
        let mut flags = codewhale_runtime::features::Features::with_defaults();
        flags.enable(codewhale_runtime::features::Feature::OcrHost);
        let cancel = CancellationToken::new();
        let context = ToolContext::new(root.path())
            .with_features(flags)
            .with_cancel_token(cancel.clone());
        let future = manager.execute_ocr(input, &context);
        tokio::pin!(future);
        tokio::select! {result=&mut future=>panic!("OCR settled before its blocking worker: {:?}",result.err()),_=started.notified()=>{},}
        cancel.cancel();
        assert!(matches!(future.await, Err(ToolError::Cancelled { .. })));
        assert_eq!(
            manager.shared.execution_broker.slots.available_permits(),
            MAX_JOBS - 1
        );
        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
        tokio::time::timeout(Duration::from_secs(5), async {
            while manager.shared.execution_broker.slots.available_permits() != MAX_JOBS {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn actual_ocr_broker_decodes_exact_ordered_grants_and_refuses_replay_or_withdrawal() {
        use std::os::unix::fs::PermissionsExt;
        let _home = crate::test_support::SealedHome::new();
        let _policy = crate::plugins::activation::TestPolicyGuard::extension_host(false);
        let Some(node) = super::super::tests::node_for_tests("ocr_exact_grants") else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("written");
        let binary = root.path().join("tesseract");
        std::fs::write(
            &binary,
            format!(
                "#!/bin/sh\nprintf x >> '{}'; printf 'private extracted text\\n'\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
            runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
            node_override: Some(node),
            root: Some(root.path().join("host")),
            ..ExtensionHostOptions::default()
        }));
        manager.bind_engine_handle(tokio::runtime::Handle::current());
        manager.shared.stock_users.fetch_add(1, Ordering::SeqCst);
        let _demand = Demand(&manager.shared.stock_users);
        let (_host, owner, generation) = manager.ensure_stock_builtin().await.unwrap();
        let context = ToolContext::new(root.path());
        let make_job = |id: &str| {
            let input = crate::tools::image_ocr::CapturedOcr::for_test(
                root.path(),
                Arc::new(|_| Err(ToolError::execution_failed("Native refusal"))),
                Some(binary.clone().into_os_string()),
            );
            let output = Arc::new(Mutex::new(None));
            let mut caller = Caller::capture(&context, &manager.shared).unwrap();
            caller.requires_native_policy = false;
            let mut target = caller.target(id);
            target["operation"] = json!("ocr_native");
            target["captured_sha256"] = json!(input.digest());
            let ticket = manager.shared.core_calls.tickets.mint(Grant {
                kind: TicketKind::Execution,
                tier: HostTier::Builtin,
                host_generation: generation,
                owner: owner.clone(),
                method: "exec/redeem",
                target: target.clone(),
                ttl: DEADLINE,
                uses: 1,
            });
            let job = Arc::new(Job {
                id: id.into(),
                owner: owner.clone(),
                generation,
                caller,
                target,
                expires: Instant::now() + DEADLINE,
                cancel: CancellationToken::new(),
                launch: Mutex::new(Some(Launch {
                    github: None,
                    ocr: Some(OcrLaunch::Native {
                        input,
                        output: Arc::clone(&output),
                    }),
                    pdf: None,
                    stock: None,
                    hook: None,
                    command: None,
                    input: Vec::new(),
                })),
                ticket: ticket.clone(),
                continuation: Mutex::new(None),
                hook_result: Mutex::new(None),
                _permit: Arc::clone(&manager.shared.execution_broker.slots)
                    .try_acquire_owned()
                    .unwrap(),
            });
            manager
                .shared
                .execution_broker
                .jobs
                .lock()
                .unwrap()
                .insert(id.into(), Arc::clone(&job));
            let invocation = Invocation {
                shared: Arc::clone(&manager.shared),
                job: Arc::clone(&job),
            };
            (job, invocation, output)
        };
        let (job, invocation, output) = make_job("ordered-ocr");
        let decoded = |id: &str, ticket: &str| {
            serde_json::from_value::<ExecutionRedeemParams>(
                json!({"owner":owner,"execution_id":id,"ticket":ticket}),
            )
            .unwrap()
        };
        let cx = || HostRequestContext::for_test(99).0;
        let first = decoded(&job.id, job.ticket.expose());
        let mut wrong_owner = first.clone();
        wrong_owner.owner.plugin_id = "native:forged".into();
        assert!(
            manager
                .shared
                .execution_broker
                .serve(&manager.shared, generation, wrong_owner, cx())
                .await
                .is_err()
        );
        assert!(
            manager
                .shared
                .execution_broker
                .serve(&manager.shared, generation + 1, first.clone(), cx())
                .await
                .is_err()
        );
        assert!(
            manager
                .shared
                .execution_broker
                .serve(
                    &manager.shared,
                    generation,
                    decoded("other-ocr", job.ticket.expose()),
                    cx()
                )
                .await
                .is_err()
        );
        assert!(!marker.exists());
        let native = manager
            .shared
            .execution_broker
            .serve(&manager.shared, generation, first.clone(), cx())
            .await
            .unwrap();
        let next = native["next_ticket"].as_str().unwrap().to_string();
        assert_eq!(native["status"], "error");
        assert!(!marker.exists());
        // The first grant is already spent and never authorizes the later write.
        assert!(
            manager
                .shared
                .execution_broker
                .serve(&manager.shared, generation, first, cx())
                .await
                .is_err()
        );
        let target = job
            .continuation
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .target
            .clone();
        assert_eq!(target["operation"], "ocr_tesseract");
        assert_ne!(target["captured_sha256"], job.target["captured_sha256"]);
        let wrong_kind = manager.shared.core_calls.tickets.mint(Grant {
            kind: TicketKind::McpOperation,
            tier: HostTier::Builtin,
            host_generation: generation,
            owner: owner.clone(),
            method: "exec/redeem",
            target,
            ttl: DEADLINE,
            uses: 1,
        });
        assert!(
            manager
                .shared
                .execution_broker
                .serve(
                    &manager.shared,
                    generation,
                    decoded(&job.id, wrong_kind.expose()),
                    cx()
                )
                .await
                .is_err()
        );
        assert!(!marker.exists());
        let mut altered_target = job
            .continuation
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .target
            .clone();
        altered_target["captured_sha256"] = json!("changed-command-or-image");
        let mismatch = manager.shared.core_calls.tickets.mint(Grant {
            kind: TicketKind::Execution,
            tier: HostTier::Builtin,
            host_generation: generation,
            owner: owner.clone(),
            method: "exec/redeem",
            target: altered_target,
            ttl: DEADLINE,
            uses: 1,
        });
        assert!(
            manager
                .shared
                .execution_broker
                .serve(
                    &manager.shared,
                    generation,
                    decoded(&job.id, mismatch.expose()),
                    cx()
                )
                .await
                .is_err()
        );
        assert!(
            !marker.exists(),
            "parsed request must match its exact captured-operation digest before write"
        );
        assert!(serde_json::from_value::<ExecutionRedeemParams>(json!({"owner":owner,"execution_id":job.id,"ticket":next,"path":"unreviewed-image.png"})).is_err());
        let next = decoded(&job.id, &next);
        let projected = manager
            .shared
            .execution_broker
            .serve(&manager.shared, generation, next.clone(), cx())
            .await
            .unwrap();
        assert_eq!(projected["state"], "tesseract");
        assert!(projected.get("stdout").is_none());
        assert!(output.lock().unwrap().is_some());
        assert_eq!(std::fs::read(&marker).unwrap(), b"x");
        assert!(
            manager
                .shared
                .execution_broker
                .serve(&manager.shared, generation, next, cx())
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&marker).unwrap(), b"x");
        drop(invocation);
        let (withdrawn, invocation, _) = make_job("withdrawn-ocr");
        let native = manager
            .shared
            .execution_broker
            .serve(
                &manager.shared,
                generation,
                decoded(&withdrawn.id, withdrawn.ticket.expose()),
                cx(),
            )
            .await
            .unwrap();
        let next = decoded(&withdrawn.id, native["next_ticket"].as_str().unwrap());
        manager.shared.execution_broker.revoke_owner("host:harness");
        assert!(
            manager
                .shared
                .execution_broker
                .serve(&manager.shared, generation, next, cx())
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&marker).unwrap(), b"x");
        drop(invocation);
        manager.shutdown().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn abandoned_execution_worker_keeps_quota_until_cleanup_completes() {
        let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions::default()));
        manager.bind_engine_handle(tokio::runtime::Handle::current());
        let broker = &manager.shared.execution_broker;
        let owner = owner();
        let caller = caller();
        let target = caller.target("job");
        let ticket = manager
            .shared
            .core_calls
            .tickets
            .mint(grant(owner.clone(), target.clone()));
        let job = Arc::new(Job {
            id: "job".into(),
            owner,
            generation: 4,
            caller,
            target,
            expires: Instant::now() + DEADLINE,
            cancel: CancellationToken::new(),
            launch: Mutex::new(None),
            ticket,
            continuation: Mutex::new(None),
            hook_result: Mutex::new(None),
            _permit: Arc::clone(&broker.slots).acquire_owned().await.unwrap(),
        });
        broker
            .jobs
            .lock()
            .unwrap()
            .insert("job".into(), Arc::clone(&job));
        let invocation = Invocation {
            shared: Arc::clone(&manager.shared),
            job: Arc::clone(&job),
        };
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let worker = dispatch_job(
            manager.shared.engine_handle().unwrap(),
            Arc::clone(&job),
            move |job| async move {
                started_tx.send(()).unwrap();
                job.cancel.cancelled().await;
                finish_rx.await.unwrap();
                done_tx.send(()).unwrap();
                Ok(json!({}))
            },
        );
        started_rx.await.unwrap();
        drop(worker);
        drop(invocation);
        drop(job);
        assert_eq!(broker.slots.available_permits(), MAX_JOBS - 1);
        finish_tx.send(()).unwrap();
        done_rx.await.unwrap();
        tokio::task::yield_now().await;
        assert_eq!(broker.slots.available_permits(), MAX_JOBS);
    }
}
