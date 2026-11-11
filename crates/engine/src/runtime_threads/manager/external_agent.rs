use std::sync::mpsc as std_mpsc;

use super::*;
use crate::fleet::acp::{
    AcpCanceller, AcpClient, AcpError, AcpHandler, AgentIdentity, AgentOptions, AgentTokenUsage,
    PermissionAnswer, PromptReply, SessionOffer, refusal,
};
use crate::fleet::agents::ExternalAgentLaunch;
use crate::fleet::host::{
    FleetHostAdapter, FleetWorkerCommand, FleetWorkerStartRequest, FleetWorkerTransport,
    LocalProcessFleetHostAdapter,
};

const SETUP_TIMEOUT: Duration = Duration::from_secs(60);
const PROMPT_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);
const CANCEL_GRACE: Duration = Duration::from_secs(10);
const APPROVAL_SETTLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_DETAIL_BYTES: usize = 256 * 1024;
const MAX_INPUT_BYTES: usize = 16 * 1024;
const MAX_LABEL_CHARS: usize = 200;
const MAX_ID_BYTES: usize = 128;
const MAX_TOOL_ITEMS_PER_TURN: usize = 512;
const MAX_LOCATIONS: usize = 32;
const MAX_OPTIONS: usize = 16;
const MAX_CONCURRENT_OPTION_PROBES: usize = 4;
static OPTIONS_PROBES: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| {
        Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_OPTION_PROBES))
    });
const KILLED_REASON: &str = "The agent did not stop; Codewhale ended its process.";

pub(crate) struct ExternalThreadState {
    pub(super) session: Arc<ExternalSession>,
    pub(super) active_turn: Option<ExternalActiveTurn>,
}

pub(crate) struct ExternalActiveTurn {
    pub(super) turn_id: String,
    pub(super) interrupt_requested: bool,
    pub(super) killed: bool,
    pub(super) cancel: CancellationToken,
}

enum SessionCommand {
    Prompt {
        text: String,
        signals: mpsc::Sender<AgentSignal>,
        cancel: CancellationToken,
    },
}

enum AgentSignal {
    Update(Value),
    Permission {
        params: Value,
        reply: std_mpsc::Sender<PermissionAnswer>,
    },
    Finished(Result<PromptReply, String>),
}

pub(crate) struct ExternalSession {
    commands: parking_lot::Mutex<Option<std_mpsc::Sender<SessionCommand>>>,
    canceller: AcpCanceller,
    host: parking_lot::Mutex<LocalProcessFleetHostAdapter>,
    worker_id: String,
}

impl ExternalSession {
    fn send(&self, command: SessionCommand) -> Result<()> {
        self.commands
            .lock()
            .as_ref()
            .ok_or_else(|| anyhow!("the agent session has ended"))?
            .send(command)
            .map_err(|_| anyhow!("the agent session has ended"))
    }

    pub(super) fn stop_blocking(&self) -> Result<()> {
        self.commands.lock().take();
        self.host
            .lock()
            .stop_worker(&self.worker_id)
            .map(|_| ())
            .map_err(|error| anyhow!("stopping external agent {} failed: {error}", self.worker_id))
    }
}

impl Drop for ExternalSession {
    fn drop(&mut self) {
        if let Err(error) = self.stop_blocking() {
            tracing::warn!(worker = %self.worker_id, %error, "stopping a dropped external agent failed");
        }
    }
}

struct ForwardingHandler {
    signals: mpsc::Sender<AgentSignal>,
}

impl AcpHandler for ForwardingHandler {
    fn update(&mut self, update: &Value) {
        let _ = self
            .signals
            .blocking_send(AgentSignal::Update(update.clone()));
    }

    fn permission(&mut self, params: &Value) -> PermissionAnswer {
        let (reply, answer) = std_mpsc::channel();
        if self
            .signals
            .blocking_send(AgentSignal::Permission {
                params: params.clone(),
                reply,
            })
            .is_err()
        {
            return PermissionAnswer::Cancelled;
        }
        answer.recv().unwrap_or(PermissionAnswer::Cancelled)
    }
}

fn run_session(
    mut client: AcpClient,
    session_id: String,
    commands: std_mpsc::Receiver<SessionCommand>,
) {
    while let Ok(SessionCommand::Prompt {
        text,
        signals,
        cancel,
    }) = commands.recv()
    {
        let mut handler = ForwardingHandler {
            signals: signals.clone(),
        };
        let result = client
            .prompt_with(
                &session_id,
                &text,
                PROMPT_TIMEOUT,
                &mut handler,
                Some(&cancel),
            )
            .map_err(|error| label(Some(&Value::String(error.to_string())), "the agent failed"));
        let _ = signals.blocking_send(AgentSignal::Finished(result));
    }
}

struct SessionStart {
    session: ExternalSession,
    identity: AgentIdentity,
    session_id: String,
}

struct AgentProcess {
    host: Option<LocalProcessFleetHostAdapter>,
    worker_id: String,
}

impl AgentProcess {
    fn spawn(
        log_root: &Path,
        worker_id: String,
        workspace: &Path,
        launch: &ExternalAgentLaunch,
    ) -> Result<(Self, AcpClient)> {
        let mut host = LocalProcessFleetHostAdapter::new(log_root);
        let mut request = FleetWorkerStartRequest::new(
            worker_id.clone(),
            FleetWorkerCommand::new(launch.program.display().to_string(), launch.args.clone()),
        );
        request.transport = FleetWorkerTransport::JsonRpc;
        request.cwd = Some(workspace.to_path_buf());
        request.env_allowlist = launch.env.keys().cloned().collect();
        request.env = launch.env.clone();
        host.start_worker(request)
            .map_err(|error| anyhow!("{} could not start: {error}", launch.agent_name))?;
        let io = host.take_worker_io(&worker_id);
        let process = Self {
            host: Some(host),
            worker_id,
        };
        let Some(io) = io else {
            bail!("{} started without a protocol channel", launch.agent_name);
        };
        Ok((process, AcpClient::new(io)))
    }

    fn release(mut self) -> LocalProcessFleetHostAdapter {
        self.host.take().expect("an agent process is released once")
    }

    fn stop(&mut self) {
        if let Some(mut host) = self.host.take()
            && let Err(error) = host.stop_worker(&self.worker_id)
        {
            tracing::warn!(worker = %self.worker_id, %error, "stopping an external agent failed");
        }
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn sign_in_or(
    error: anyhow::Error,
    agent_name: &str,
    login: Option<&'static str>,
) -> anyhow::Error {
    if error
        .downcast_ref::<AcpError>()
        .is_some_and(AcpError::is_auth_required)
    {
        let hint = login
            .map(|command| format!(" Run `{command}` in a terminal, then try again."))
            .unwrap_or_default();
        return crate::fleet::agents::ExecutorUnavailable::new(
            crate::fleet::agents::ExecutorUnavailable::NEEDS_SIGN_IN,
            format!("{agent_name} needs you to sign in to it first.{hint}"),
        )
        .into();
    }
    if error
        .downcast_ref::<crate::fleet::agents::ExecutorUnavailable>()
        .is_some()
    {
        return error;
    }
    anyhow!("{agent_name}: {error}")
}

fn executor_choices(executor: &ExecutorRecord) -> Vec<(&'static str, String)> {
    executor
        .model
        .iter()
        .map(|model| ("model", model.clone()))
        .collect()
}

fn refuse(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    crate::fleet::agents::ExecutorUnavailable::new(code, message).into()
}

fn apply_choices(
    client: &mut AcpClient,
    session_id: &str,
    offer: &SessionOffer,
    choices: &[(&'static str, String)],
) -> Result<()> {
    for (id, value) in choices {
        let shown = label(Some(&Value::String(value.clone())), "");
        if offer.controls.is_empty() && *id == "model" {
            client
                .set_config_option(session_id, "model", value, SETUP_TIMEOUT)
                .map_err(|error| rejected_choice(id, &shown, error))?;
            continue;
        }
        let Some(control) = offer.control(id) else {
            return Err(refuse(
                refusal::CONTROL_NOT_OFFERED,
                format!("The agent does not offer a {id} choice, so `{shown}` was not applied."),
            ));
        };
        if let Some(why) = control.unavailable.as_ref() {
            return Err(refuse(refusal::CONTROL_UNAVAILABLE, why.reason.clone()));
        }
        if !control.offers(value) {
            return Err(refuse(
                refusal::CHOICE_NOT_OFFERED,
                format!(
                    "The agent no longer offers `{shown}` for {id}; choose one of its current options."
                ),
            ));
        }
        if control.current.as_deref() == Some(value.as_str()) {
            continue;
        }
        client
            .apply_control(session_id, control, value, SETUP_TIMEOUT)
            .map_err(|error| rejected_choice(id, &shown, error))?;
    }
    Ok(())
}

fn rejected_choice(id: &str, shown: &str, error: AcpError) -> anyhow::Error {
    if error.is_auth_required() {
        return error.into();
    }
    refuse(
        refusal::CHOICE_REJECTED,
        format!("The agent refused {id} `{shown}`: {error}"),
    )
}

fn start_session_blocking(
    log_root: PathBuf,
    thread_id: String,
    workspace: PathBuf,
    add_dirs: Vec<PathBuf>,
    launch: ExternalAgentLaunch,
    executor: ExecutorRecord,
    login: Option<&'static str>,
    abandoned: CancellationToken,
) -> Result<SessionStart> {
    let worker_id = format!("acp-{thread_id}");
    let (process, mut client) =
        AgentProcess::spawn(&log_root, worker_id.clone(), &workspace, &launch)?;
    client.abort_requests_on(Some(abandoned));
    let choices = executor_choices(&executor);
    let setup = (|| -> Result<(AgentIdentity, String)> {
        let identity = client.initialize(SETUP_TIMEOUT)?;
        let (session_id, offer) = match executor.foreign_session_id.as_deref() {
            Some(session_id) => {
                let offer = client.reopen_session(
                    &identity,
                    session_id,
                    &workspace,
                    &add_dirs,
                    SETUP_TIMEOUT,
                )?;
                (session_id.to_string(), offer)
            }
            None => {
                let started =
                    client.new_session(&identity, &workspace, &add_dirs, SETUP_TIMEOUT)?;
                (started.session_id, started.offer)
            }
        };
        apply_choices(&mut client, &session_id, &offer, &choices)?;
        Ok((identity, session_id))
    })();
    let (identity, session_id) =
        setup.map_err(|error| sign_in_or(error, &launch.agent_name, login))?;
    if !valid_id(&session_id) {
        bail!("{} named an invalid session id", launch.agent_name);
    }
    client.abort_requests_on(None);
    let canceller = client.canceller(&session_id);
    let (commands, receiver) = std_mpsc::channel();
    let session_for_thread = session_id.clone();
    std::thread::Builder::new()
        .name(format!("acp-{}", launch.agent_id))
        .spawn(move || run_session(client, session_for_thread, receiver))
        .map_err(|error| anyhow!("starting the agent session thread failed: {error}"))?;
    Ok(SessionStart {
        session: ExternalSession {
            commands: parking_lot::Mutex::new(Some(commands)),
            canceller,
            host: parking_lot::Mutex::new(process.release()),
            worker_id,
        },
        identity,
        session_id,
    })
}

fn probe_options_blocking(
    log_root: PathBuf,
    workspace: PathBuf,
    launch: ExternalAgentLaunch,
    login: Option<&'static str>,
    abandoned: CancellationToken,
) -> Result<AgentOptions> {
    let worker_id = format!("acp-options-{}", runtime_record_id("probe"));
    let (mut process, mut client) = AgentProcess::spawn(&log_root, worker_id, &workspace, &launch)?;
    client.abort_requests_on(Some(abandoned));
    let probed = (|| -> Result<AgentOptions> {
        let identity = client.initialize(SETUP_TIMEOUT)?;
        let started = client.new_session(&identity, &workspace, &[], SETUP_TIMEOUT)?;
        Ok(AgentOptions::new(
            &identity,
            &launch.agent_id,
            &workspace,
            started.offer,
        ))
    })();
    drop(client);
    process.stop();
    probed.map_err(|error| sign_in_or(error, &launch.agent_name, login))
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value
            .chars()
            .all(|ch| !ch.is_control() && !ch.is_whitespace())
}

fn bounded_id(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| valid_id(value))
        .map(str::to_string)
}

fn label(value: Option<&Value>, fallback: &str) -> String {
    let text: String = value
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_LABEL_CHARS)
        .collect();
    let text = text.trim();
    if text.is_empty() {
        fallback.to_string()
    } else {
        text.to_string()
    }
}

fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .collect()
}

fn push_bounded(detail: &mut String, text: &str) {
    if detail.len() >= MAX_DETAIL_BYTES {
        return;
    }
    let room = MAX_DETAIL_BYTES - detail.len();
    if text.len() <= room {
        detail.push_str(text);
        return;
    }
    let mut cut = room;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    detail.push_str(&text[..cut]);
    detail.push_str("\n[truncated]");
}

fn bounded_json(value: Option<&Value>) -> Option<Value> {
    let value = value.filter(|value| !value.is_null())?;
    let encoded = serde_json::to_string(value).ok()?;
    if encoded.len() <= MAX_INPUT_BYTES {
        Some(value.clone())
    } else {
        Some(Value::String(format!(
            "[{} bytes of tool input omitted]",
            encoded.len()
        )))
    }
}

fn tool_kind(update: &Value) -> &'static str {
    match update
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("other")
    {
        "read" => "read",
        "edit" => "edit",
        "delete" => "delete",
        "move" => "move",
        "search" => "search",
        "execute" => "execute",
        "think" => "think",
        "fetch" => "fetch",
        "switch_mode" => "switch_mode",
        _ => "other",
    }
}

fn item_kind_for_tool(kind: &str) -> TurnItemKind {
    match kind {
        "execute" => TurnItemKind::CommandExecution,
        "edit" | "delete" | "move" => TurnItemKind::FileChange,
        "switch_mode" => TurnItemKind::Status,
        _ => TurnItemKind::ToolCall,
    }
}

fn tool_status(update: &Value) -> Option<TurnItemLifecycleStatus> {
    match update.get("status").and_then(Value::as_str)? {
        "pending" | "in_progress" => Some(TurnItemLifecycleStatus::InProgress),
        "completed" => Some(TurnItemLifecycleStatus::Completed),
        "failed" => Some(TurnItemLifecycleStatus::Failed),
        _ => None,
    }
}

fn locations(update: &Value) -> Vec<String> {
    update
        .get("locations")
        .and_then(Value::as_array)
        .map(|locations| {
            locations
                .iter()
                .take(MAX_LOCATIONS)
                .filter_map(|location| location.get("path").and_then(Value::as_str))
                .filter(|path| path.len() <= 4096 && !path.chars().any(char::is_control))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn content_text(content: Option<&Value>) -> String {
    let mut out = String::new();
    let Some(blocks) = content.and_then(Value::as_array) else {
        return out;
    };
    for block in blocks.iter().take(64) {
        match block.get("type").and_then(Value::as_str) {
            Some("content") => {
                if let Some(text) = block.pointer("/content/text").and_then(Value::as_str) {
                    push_bounded(&mut out, &clean_text(text));
                    out.push('\n');
                }
            }
            Some("diff") => {
                let path = label(block.get("path"), "file");
                push_bounded(&mut out, &format!("Changed {path}\n"));
            }
            Some("terminal") => push_bounded(&mut out, "Terminal output\n"),
            _ => {}
        }
    }
    out
}

fn chunk_text(update: &Value) -> Option<String> {
    update
        .pointer("/content/text")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|text| !text.is_empty())
}

#[derive(Default)]
struct ExternalProjection {
    message: Option<TurnItemRecord>,
    reasoning: Option<TurnItemRecord>,
    tools: HashMap<String, TurnItemRecord>,
    tool_items: usize,
    plan: Option<TurnItemRecord>,
    title: Option<String>,
    failure: Option<String>,
    context: Option<(Option<u64>, Option<u64>)>,
    tokens: Option<AgentTokenUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PermissionOption {
    id: String,
    name: String,
    kind: String,
}

fn permission_options(params: &Value) -> Vec<PermissionOption> {
    params
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .take(MAX_OPTIONS)
                .filter_map(|option| {
                    let id = bounded_id(option.get("optionId"))?;
                    let kind =
                        match option.get("kind").and_then(Value::as_str)? {
                            kind @ ("allow_once" | "allow_always" | "reject_once"
                            | "reject_always") => kind.to_string(),
                            _ => return None,
                        };
                    Some(PermissionOption {
                        id,
                        name: label(option.get("name"), &kind),
                        kind,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn pick(options: &[PermissionOption], preferred: &[&str]) -> PermissionAnswer {
    preferred
        .iter()
        .find_map(|kind| options.iter().find(|option| option.kind == *kind))
        .map_or(PermissionAnswer::Cancelled, |option| {
            PermissionAnswer::Selected(option.id.clone())
        })
}

fn answer_for(
    decision: ExternalApprovalDecision,
    options: &[PermissionOption],
) -> PermissionAnswer {
    match decision {
        ExternalApprovalDecision::Allow { remember: true } => {
            pick(options, &["allow_always", "allow_once"])
        }
        ExternalApprovalDecision::Allow { remember: false } => pick(options, &["allow_once"]),
        ExternalApprovalDecision::Deny { remember: true } => {
            pick(options, &["reject_always", "reject_once"])
        }
        ExternalApprovalDecision::Deny { remember: false } => pick(options, &["reject_once"]),
    }
}

async fn posture_answer(
    thread: &ThreadRecord,
    permission: ApprovalMode,
    plan_mode: bool,
    kind: &str,
    paths: &[String],
    options: &[PermissionOption],
) -> Option<(bool, PermissionAnswer)> {
    if plan_mode || permission == ApprovalMode::Never {
        return Some((false, pick(options, &["reject_once", "reject_always"])));
    }
    if permission == ApprovalMode::Bypass {
        return Some((true, pick(options, &["allow_once", "allow_always"])));
    }
    if permission == ApprovalMode::AcceptEdits
        && matches!(kind, "edit" | "delete" | "move")
        && !paths.is_empty()
        && inside_roots(thread, paths).await
    {
        return Some((true, pick(options, &["allow_once"])));
    }
    None
}

async fn canonical_or_parent(path: &Path) -> Option<PathBuf> {
    if let Ok(resolved) = tokio::fs::canonicalize(path).await {
        return Some(resolved);
    }
    let parent = path.parent()?;
    let name = path.file_name()?;
    tokio::fs::canonicalize(parent)
        .await
        .ok()
        .map(|parent| parent.join(name))
}

async fn inside_roots(thread: &ThreadRecord, paths: &[String]) -> bool {
    let mut roots = Vec::new();
    for root in std::iter::once(&thread.workspace).chain(thread.add_dirs.iter()) {
        if let Ok(root) = tokio::fs::canonicalize(root).await {
            roots.push(root);
        }
    }
    if roots.is_empty() {
        return false;
    }
    for path in paths {
        let path = Path::new(path);
        if !path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return false;
        }
        let Some(resolved) = canonical_or_parent(path).await else {
            return false;
        };
        if !roots.iter().any(|root| resolved.starts_with(root)) {
            return false;
        }
    }
    true
}

fn new_item(
    turn_id: &str,
    kind: TurnItemKind,
    summary: String,
    detail: String,
    metadata: Option<Value>,
) -> TurnItemRecord {
    TurnItemRecord {
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: runtime_record_id("item"),
        turn_id: turn_id.to_string(),
        kind,
        status: TurnItemLifecycleStatus::InProgress,
        summary,
        detail: Some(detail),
        metadata,
        artifact_refs: Vec::new(),
        artifacts: Vec::new(),
        started_at: Some(Utc::now()),
        ended_at: None,
    }
}

impl RuntimeThreadManager {
    pub(super) async fn external_session(
        &self,
        thread: &ThreadRecord,
    ) -> Result<Arc<ExternalSession>> {
        if let Some(state) = self.active.lock().await.external.get(&thread.id) {
            return Ok(Arc::clone(&state.session));
        }
        let loading = Arc::clone(&self.engine_load).lock_owned().await;
        if let Some(state) = self.active.lock().await.external.get(&thread.id) {
            return Ok(Arc::clone(&state.session));
        }
        self.ensure_accepting_execution()?;
        let executor = thread
            .executor
            .clone()
            .ok_or_else(|| anyhow!("thread {} has no external executor", thread.id))?;
        executor.validate()?;
        let agent = crate::fleet::agents::find(&executor.agent_id)
            .ok_or_else(|| anyhow!("No external agent is named `{}`.", executor.agent_id))?;
        let launch = crate::fleet::agents::launch(&executor.agent_id).await?;
        let log_root = self
            .session_store_binding()
            .data_dir
            .join("external-agents");
        tokio::fs::create_dir_all(&log_root)
            .await
            .context("creating the external agent log directory failed")?;
        let thread_id = thread.id.clone();
        let workspace = thread.workspace.clone();
        let add_dirs = thread.add_dirs.clone();
        let starting = executor.clone();
        let login = agent.login;
        let abandoned = self.cancel_token.child_token();
        let _abandon = abandoned.clone().drop_guard();
        let (started, _loading) = tokio::task::spawn_blocking(move || {
            let started = start_session_blocking(
                log_root, thread_id, workspace, add_dirs, launch, starting, login, abandoned,
            );
            (started, loading)
        })
        .await
        .context("external agent start worker failed")?;
        let SessionStart {
            session,
            identity,
            session_id,
        } = started?;
        self.ensure_accepting_execution()?;
        let session = Arc::new(session);
        let store = self.store.clone();
        let persisted_thread = thread.id.clone();
        let agent_name = label(Some(&Value::String(identity.name.clone())), agent.name);
        let agent_version = label(Some(&Value::String(identity.version.clone())), "0");
        let started_payload = json!({
            "agent_id": executor.agent_id,
            "agent_name": agent_name,
            "agent_version": agent_version,
            "protocol_version": identity.protocol_version,
            "foreign_session_id": session_id,
            "reopened": executor.foreign_session_id.is_some(),
        });
        let persisted = tokio::task::spawn_blocking(move || {
            let _thread_mutation = store.thread_mutation.lock();
            let mut current = store.load_thread(&persisted_thread)?;
            let record = current
                .executor
                .as_mut()
                .ok_or_else(|| anyhow!("thread executor changed while its agent started"))?;
            record.agent_name = Some(agent_name);
            record.agent_version = Some(agent_version);
            record.protocol_version = Some(identity.protocol_version);
            record.foreign_session_id = Some(session_id);
            current.updated_at = Utc::now();
            store.save_thread(&current)
        })
        .await
        .context("external agent record worker failed")?;
        if let Err(error) = persisted {
            let stopping = Arc::clone(&session);
            let _ = tokio::task::spawn_blocking(move || stopping.stop_blocking()).await;
            return Err(error);
        }
        self.active.lock().await.external.insert(
            thread.id.clone(),
            ExternalThreadState {
                session: Arc::clone(&session),
                active_turn: None,
            },
        );
        self.emit_event(&thread.id, None, None, "executor.started", started_payload)
            .await?;
        Ok(session)
    }

    pub async fn external_agent_options(&self, agent_id: &str, cwd: &Path) -> Result<AgentOptions> {
        self.ensure_accepting_execution()?;
        let agent = crate::fleet::agents::find(agent_id).ok_or_else(|| {
            crate::fleet::agents::ExecutorUnavailable::new(
                crate::fleet::agents::ExecutorUnavailable::UNKNOWN_AGENT,
                format!("No external agent is named `{agent_id}`."),
            )
        })?;
        let workspace = tokio::fs::canonicalize(cwd)
            .await
            .map_err(|error| anyhow!("cwd {} is unavailable: {error}", cwd.display()))?;
        anyhow::ensure!(
            tokio::fs::metadata(&workspace)
                .await
                .is_ok_and(|metadata| metadata.is_dir()),
            "cwd {} is not a directory",
            workspace.display()
        );
        let launch = crate::fleet::agents::launch(agent.id).await?;
        let log_root = self
            .session_store_binding()
            .data_dir
            .join("external-agents");
        tokio::fs::create_dir_all(&log_root)
            .await
            .context("creating the external agent log directory failed")?;
        let abandoned = self.cancel_token.child_token();
        let _abandon = abandoned.clone().drop_guard();
        let permit = tokio::select! {
            permit = Arc::clone(&OPTIONS_PROBES).acquire_owned() => permit?,
            () = abandoned.cancelled() => bail!("Runtime is shutting down; execution admission is closed"),
        };
        self.ensure_accepting_execution()?;
        let login = agent.login;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            probe_options_blocking(log_root, workspace, launch, login, abandoned)
        })
        .await
        .context("external agent options worker failed")?
    }

    pub(super) async fn external_thread_loaded(&self, thread_id: &str) -> bool {
        self.active.lock().await.external.contains_key(thread_id)
    }

    #[cfg(test)]
    pub(crate) async fn external_turn_is_active(&self, thread_id: &str) -> bool {
        self.active
            .lock()
            .await
            .external
            .get(thread_id)
            .is_some_and(|state| state.active_turn.is_some())
    }

    pub async fn stop_external_agent(&self, thread_id: &str) -> Result<bool> {
        let removed = {
            let mut active = self.active.lock().await;
            if active
                .external
                .get(thread_id)
                .is_some_and(|state| state.active_turn.is_some())
            {
                bail!("thread {thread_id} has an active turn; stop the turn first");
            }
            active.external.remove(thread_id)
        };
        let Some(state) = removed else {
            return Ok(false);
        };
        tokio::task::spawn_blocking(move || state.session.stop_blocking())
            .await
            .context("external agent stop worker failed")??;
        self.emit_event(
            thread_id,
            None,
            None,
            "executor.stopped",
            json!({ "reason": "stopped" }),
        )
        .await?;
        Ok(true)
    }

    pub(super) async fn stop_all_external_sessions(&self) -> Result<()> {
        let sessions = {
            let mut active = self.active.lock().await;
            active
                .external
                .iter_mut()
                .map(|(id, state)| {
                    if let Some(turn) = state.active_turn.as_mut() {
                        turn.interrupt_requested = true;
                        turn.cancel.cancel();
                        turn.killed = true;
                    }
                    (id.clone(), Arc::clone(&state.session))
                })
                .collect::<Vec<_>>()
        };
        self.pending_approvals
            .lock()
            .retain(|_, entry| !sessions.iter().any(|(id, _)| id == &entry.thread_id));
        let mut failure = None;
        for (_, session) in sessions {
            if let Err(error) = tokio::task::spawn_blocking(move || session.stop_blocking())
                .await
                .context("external agent stop worker failed")
                .and_then(|result| result)
            {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    pub(super) async fn start_external_turn(
        &self,
        thread: ThreadRecord,
        req: StartTurnRequest,
        input_source: &RuntimeTurnInputSource,
        reserved_turn_id: Option<&str>,
        prompt: String,
    ) -> Result<(TurnRecord, bool)> {
        anyhow::ensure!(
            matches!(input_source, RuntimeTurnInputSource::ExternalUser),
            "this kind of turn is not available in a session with another agent"
        );
        if !req.images.is_empty() {
            return Err(refuse(
                refusal::UNSUPPORTED_CONTROL,
                "Image inputs are not available in a session with another agent.",
            ));
        }
        if !req.dynamic_tools.is_empty() {
            return Err(refuse(
                refusal::UNSUPPORTED_CONTROL,
                "Dynamic tools are not available in a session with another agent.",
            ));
        }
        if req.model.is_some()
            || req.reasoning_effort.is_some()
            || req.max_output_tokens.is_some()
            || req.model_provider.is_some()
            || req.model_provider_id.is_some()
            || req.account_model_owner.is_some()
        {
            return Err(refuse(
                refusal::UNSUPPORTED_CONTROL,
                "A session with another agent keeps its saved agent model; per-turn model, reasoning, output and provider controls are unavailable.",
            ));
        }
        if req.allowed_tools.is_some() {
            return Err(refuse(
                refusal::UNSUPPORTED_CONTROL,
                "A session with another agent uses that agent's own tools; tool allowlists are unavailable.",
            ));
        }
        if req.environment_id.is_some() {
            return Err(crate::fleet::agents::ExecutorUnavailable::new(
                crate::fleet::agents::ExecutorUnavailable::ENVIRONMENT_NOT_SUPPORTED,
                "This build cannot move an external agent turn to another environment.",
            )
            .into());
        }
        let executor = thread
            .executor
            .clone()
            .ok_or_else(|| anyhow!("thread {} has no external executor", thread.id))?;
        executor.validate()?;
        let policy =
            if req.mode.is_some() || req.permission_posture.is_some() || req.auto_approve.is_some()
            {
                runtime_policy_with_overrides(
                    &thread,
                    req.mode.as_deref(),
                    req.permission_posture.as_deref(),
                    req.auto_approve,
                )?
            } else {
                RuntimePolicyProjection::from_persisted(
                    &thread.mode,
                    thread.permission_posture.as_deref(),
                    thread.auto_approve,
                )
            };
        let operation = match req.operation_key.as_deref() {
            Some(operation_key) => {
                validate_runtime_turn_operation_key(operation_key)?;
                let fingerprint = codewhale_runtime::hashing::sha256_hex(
                    codewhale_runtime::hashing::canonical_json(&json!({
                        "domain": "codewhale:external-agent-turn:v1",
                        "agent_id": executor.agent_id,
                        "prompt": prompt,
                        "input_summary": req.input_summary,
                        "permission": policy.permission_wire(),
                        "mode": policy.mode.as_setting(),
                    }))
                    .as_bytes(),
                );
                let prepared = self.prepare_runtime_turn_operation(
                    &thread.id,
                    Some(operation_key),
                    fingerprint,
                    reserved_turn_id,
                )?;
                if let Some(prepared) = prepared.as_ref() {
                    let manager = self.clone();
                    let prepared = prepared.clone();
                    if let Some(original) = tokio::task::spawn_blocking(move || {
                        manager.replay_turn_for_operation(&prepared)
                    })
                    .await
                    .context("external operation replay worker failed")??
                    {
                        return Ok((original, true));
                    }
                }
                prepared
            }
            None => None,
        };

        let session = self.external_session(&thread).await?;
        let now = Utc::now();
        let turn_id = operation
            .as_ref()
            .map(|operation| operation.binding.turn_id.clone())
            .unwrap_or_else(|| runtime_record_id("turn"));
        let input_summary = req
            .input_summary
            .clone()
            .unwrap_or_else(|| summarize_text(&prompt, SUMMARY_LIMIT));
        let user_item = TurnItemRecord {
            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
            id: runtime_record_id("item"),
            turn_id: turn_id.clone(),
            kind: TurnItemKind::UserMessage,
            status: TurnItemLifecycleStatus::Completed,
            summary: input_summary.clone(),
            detail: input_source.item_detail(&prompt),
            metadata: None,
            artifact_refs: Vec::new(),
            artifacts: Vec::new(),
            started_at: Some(now),
            ended_at: Some(now),
        };
        let turn = TurnRecord {
            source_usage_origin_turn_id: None,
            source_accounting: false,
            source_usage_records: Default::default(),
            prefix_checkpoint: None,
            decision_receipts: Vec::new(),
            max_output_tokens: None,
            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
            id: turn_id.clone(),
            thread_id: thread.id.clone(),
            status: RuntimeTurnStatus::InProgress,
            input_summary,
            created_at: now,
            started_at: Some(now),
            ended_at: None,
            duration_ms: None,
            usage: None,
            routing_settlement: false,
            effective_route_usage: None,
            permission_posture: Some(policy.permission_wire().to_string()),
            mode: Some(policy.mode.as_setting().to_string()),
            effective_provider: None,
            effective_provider_id: None,
            effective_openrouter_vendor: None,
            effective_billing_surface: None,
            effective_endpoint_fingerprint: None,
            effective_provider_live_pricing: None,
            effective_billing_mode: None,
            effective_dispatched_at: None,
            effective_model: executor.model.clone(),
            routed_usage: Vec::new(),
            routed_usage_drop_records: Vec::new(),
            routed_usage_source_ids: Vec::new(),
            routed_usage_dropped_records: 0,
            model_request_diagnostics: None,
            operation_activity: None,
            error: None,
            item_ids: vec![user_item.id.clone()],
            steer_count: 0,
            agent_mail_message_id: None,
            artifacts: Vec::new(),
            workspace: None,
            workspace_snapshots: Vec::new(),
        };
        let mut active = self.active.lock().await;
        let Some(state) = active.external.get_mut(&thread.id) else {
            bail!("the agent session ended before the turn could start");
        };
        if state.active_turn.is_some() {
            if let Some(operation) = operation.as_ref() {
                let manager = self.clone();
                let operation = operation.clone();
                if let Some(original) = tokio::task::spawn_blocking(move || {
                    manager.replay_turn_for_operation(&operation)
                })
                .await
                .context("external operation replay worker failed")??
                {
                    return Ok((original, true));
                }
            }
            bail!("Thread already has an active turn");
        }
        let cancel = self.cancel_token.child_token();
        state.active_turn = Some(ExternalActiveTurn {
            turn_id: turn_id.clone(),
            interrupt_requested: false,
            killed: false,
            cancel: cancel.clone(),
        });
        let manager = self.clone();
        let plan_mode = policy.mode == codewhale_config::AppMode::Plan;
        let permission = policy.permission;
        let (accepted, acceptance) = oneshot::channel();
        let worker = tokio::spawn(async move {
            let persisting = manager.clone();
            let persisted_thread = thread.clone();
            let persisted_turn = turn.clone();
            let persisted_item = user_item.clone();
            let persisted = tokio::task::spawn_blocking(move || {
                let store = &persisting.store;
                let persisted = (|| -> Result<()> {
                    if let Some(operation) = operation.as_ref() {
                        store.save_turn_operation_binding(&operation.binding)?;
                    }
                    store.save_item(&persisted_item)?;
                    store.save_turn(&persisted_turn)?;
                    let _thread_mutation = store.thread_mutation.lock();
                    let mut current = store.load_thread(&persisted_thread.id)?;
                    anyhow::ensure!(
                        current.executor.as_ref().map(|record| &record.agent_id)
                            == persisted_thread
                                .executor
                                .as_ref()
                                .map(|record| &record.agent_id),
                        "Thread executor changed while preparing the turn; retry"
                    );
                    current.latest_turn_id = Some(persisted_turn.id.clone());
                    current.updated_at = now;
                    store.save_thread(&current)
                })();
                if let Err(error) = persisted {
                    let cleanup = persisting.cleanup_unaccepted_turn_records(
                        &persisted_turn.id,
                        Some(&persisted_item.id),
                        operation
                            .as_ref()
                            .map(|operation| operation.binding.operation_key_fingerprint.as_str()),
                    );
                    return match cleanup {
                        Ok(()) => Err(anyhow!("Failed to persist turn: {error}")),
                        Err(cleanup) => Err(anyhow!(
                            "Failed to persist turn: {error}; cleanup also failed: {cleanup}"
                        )),
                    };
                }
                Ok(())
            })
            .await
            .context("external turn persistence worker failed")
            .and_then(|result| result);
            if let Err(error) = persisted {
                if let Some(state) = manager.active.lock().await.external.get_mut(&thread.id) {
                    state.active_turn = None;
                }
                let _ = accepted.send(Err(error.to_string()));
                return;
            }
            manager
                .emit_claimed_turn_started(
                    &turn,
                    Some(&user_item),
                    ClaimedTurnKind::Message {
                        structured_review: false,
                        narrowing: crate::core::engine::TurnNarrowing::Inherit,
                    },
                )
                .await;
            let (signals, receiver) = mpsc::channel(16);
            let sent = session.send(SessionCommand::Prompt {
                text: prompt,
                signals: signals.clone(),
                cancel,
            });
            if let Err(error) = sent {
                let _ = signals
                    .send(AgentSignal::Finished(Err(error.to_string())))
                    .await;
            }
            drop(signals);
            let _ = accepted.send(Ok(turn));
            manager
                .drive_external_turn(thread, turn_id, permission, plan_mode, receiver)
                .await;
        });
        self.track_receipt_worker(worker);
        drop(active);
        let turn = acceptance
            .await
            .context("external turn worker ended before acceptance")?
            .map_err(anyhow::Error::msg)?;
        Ok((turn, false))
    }

    async fn drive_external_turn(
        self,
        thread: ThreadRecord,
        turn_id: String,
        permission: ApprovalMode,
        plan_mode: bool,
        mut receiver: mpsc::Receiver<AgentSignal>,
    ) {
        let mut projection = ExternalProjection::default();
        let outcome = loop {
            match receiver.recv().await {
                Some(AgentSignal::Update(update)) => {
                    if projection.failure.is_none()
                        && let Err(error) = self
                            .apply_external_update(&thread, &turn_id, &mut projection, &update)
                            .await
                    {
                        projection.failure =
                            Some(format!("Recording the agent's update failed: {error}"));
                        let session = self
                            .active
                            .lock()
                            .await
                            .external
                            .get(&thread.id)
                            .map(|state| Arc::clone(&state.session));
                        if let Some(session) = session {
                            let _ =
                                tokio::task::spawn_blocking(move || session.stop_blocking()).await;
                        }
                    }
                }
                Some(AgentSignal::Permission { params, reply }) => {
                    let answer = if projection.failure.is_some() {
                        PermissionAnswer::Cancelled
                    } else {
                        self.bridge_external_permission(
                            &thread, &turn_id, permission, plan_mode, &params,
                        )
                        .await
                    };
                    let _ = reply.send(answer);
                }
                Some(AgentSignal::Finished(result)) => {
                    break result.map(|reply| {
                        projection.tokens = reply.usage;
                        reply.stop_reason
                    });
                }
                None => break Err("the agent session ended".to_string()),
            }
        };
        if let Err(error) = self
            .emit_agent_usage(&thread.id, &turn_id, &projection)
            .await
        {
            tracing::warn!(thread_id = %thread.id, turn_id = %turn_id, %error, "recording an external agent's usage failed");
        }
        if let Err(error) = self
            .settle_external_turn(&thread, &turn_id, projection, outcome)
            .await
        {
            tracing::warn!(thread_id = %thread.id, turn_id = %turn_id, %error, "settling an external agent turn failed");
        }
    }

    async fn emit_agent_usage(
        &self,
        thread_id: &str,
        turn_id: &str,
        projection: &ExternalProjection,
    ) -> Result<()> {
        let tokens = projection
            .tokens
            .as_ref()
            .filter(|tokens| !tokens.is_empty());
        let reported = tokens.is_some() || projection.context.is_some();
        let payload = json!({
            "kind": "turn_total",
            "source": if reported { "agent reported" } else { "none" },
            "tokens": tokens.map_or_else(|| json!("unknown"), |tokens| json!(tokens)),
            "context": projection.context.map_or_else(
                || json!("unknown"),
                |(used, size)| json!({ "used": used, "size": size }),
            ),
            "cost": "unknown",
        });
        self.emit_event(thread_id, Some(turn_id), None, "turn.agent_usage", payload)
            .await?;
        Ok(())
    }

    async fn open_external_item(
        &self,
        thread_id: &str,
        turn_id: &str,
        item: &TurnItemRecord,
        tool: Option<Value>,
    ) -> Result<()> {
        self.save_streaming_item(item).await?;
        let manager = self.clone();
        let attached_turn = turn_id.to_string();
        let attached_item = item.id.clone();
        tokio::task::spawn_blocking(move || {
            manager.attach_item_to_turn(&attached_turn, &attached_item)
        })
        .await
        .context("external item attachment worker failed")??;
        let mut payload = json!({ "item": item });
        if let Some(tool) = tool {
            payload["tool"] = tool;
        }
        self.emit_event(
            thread_id,
            Some(turn_id),
            Some(&item.id),
            "item.started",
            payload,
        )
        .await?;
        Ok(())
    }

    async fn stream_external_chunk(
        &self,
        thread_id: &str,
        turn_id: &str,
        projection: &mut ExternalProjection,
        reasoning: bool,
        text: &str,
    ) -> Result<()> {
        let (kind, delta_kind) = if reasoning {
            (TurnItemKind::AgentReasoning, "agent_reasoning")
        } else {
            (TurnItemKind::AgentMessage, "agent_message")
        };
        let (slot, other) = if reasoning {
            (&mut projection.reasoning, &mut projection.message)
        } else {
            (&mut projection.message, &mut projection.reasoning)
        };
        self.close_streaming_item(
            thread_id,
            turn_id,
            other,
            TurnItemLifecycleStatus::Completed,
        )
        .await?;
        if slot.is_none() {
            let item = new_item(turn_id, kind, String::new(), String::new(), None);
            self.open_external_item(thread_id, turn_id, &item, None)
                .await?;
            *slot = Some(item);
        }
        let Some(item) = slot.as_mut() else {
            return Ok(());
        };
        let detail = item.detail.get_or_insert_default();
        let before = detail.len();
        push_bounded(detail, text);
        if detail.len() == before {
            return Ok(());
        }
        let delta = detail[before..].to_string();
        item.summary = summarize_text(detail, SUMMARY_LIMIT);
        let projection_lock = self.projection_lock(thread_id);
        let _projection = projection_lock.lock().await;
        self.save_streaming_item(item).await?;
        self.emit_event(
            thread_id,
            Some(turn_id),
            Some(&item.id),
            "item.delta",
            json!({ "delta": delta, "kind": delta_kind }),
        )
        .await?;
        Ok(())
    }

    async fn apply_external_update(
        &self,
        thread: &ThreadRecord,
        turn_id: &str,
        projection: &mut ExternalProjection,
        update: &Value,
    ) -> Result<()> {
        let thread_id = thread.id.as_str();
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("agent_message_chunk") => {
                if let Some(text) = chunk_text(update) {
                    self.stream_external_chunk(thread_id, turn_id, projection, false, &text)
                        .await?;
                }
            }
            Some("agent_thought_chunk") => {
                if let Some(text) = chunk_text(update) {
                    self.stream_external_chunk(thread_id, turn_id, projection, true, &text)
                        .await?;
                }
            }
            Some("tool_call") => {
                let Some(call_id) = bounded_id(update.get("toolCallId")) else {
                    return Ok(());
                };
                self.close_streaming_item(
                    thread_id,
                    turn_id,
                    &mut projection.message,
                    TurnItemLifecycleStatus::Completed,
                )
                .await?;
                self.close_streaming_item(
                    thread_id,
                    turn_id,
                    &mut projection.reasoning,
                    TurnItemLifecycleStatus::Completed,
                )
                .await?;
                if projection.tools.contains_key(&call_id)
                    || projection.tool_items >= MAX_TOOL_ITEMS_PER_TURN
                {
                    return self
                        .update_external_tool(thread_id, turn_id, projection, &call_id, update)
                        .await;
                }
                let kind = tool_kind(update);
                let title = label(update.get("title"), "Tool call");
                let paths = locations(update);
                let input = bounded_json(update.get("rawInput"));
                let mut detail = String::new();
                if let Some(input) = input.as_ref() {
                    push_bounded(&mut detail, &input.to_string());
                    detail.push('\n');
                }
                push_bounded(&mut detail, &content_text(update.get("content")));
                let item = new_item(
                    turn_id,
                    item_kind_for_tool(kind),
                    title.clone(),
                    detail,
                    Some(json!({
                        "external_agent": { "tool_call_id": call_id, "kind": kind, "locations": paths }
                    })),
                );
                self.open_external_item(
                    thread_id,
                    turn_id,
                    &item,
                    Some(json!({ "id": call_id, "name": kind, "input": input })),
                )
                .await?;
                projection.tool_items += 1;
                projection.tools.insert(call_id.clone(), item);
                if matches!(
                    tool_status(update),
                    Some(TurnItemLifecycleStatus::Completed | TurnItemLifecycleStatus::Failed)
                ) {
                    self.update_external_tool(thread_id, turn_id, projection, &call_id, update)
                        .await?;
                }
            }
            Some("tool_call_update") => {
                if let Some(call_id) = bounded_id(update.get("toolCallId")) {
                    self.update_external_tool(thread_id, turn_id, projection, &call_id, update)
                        .await?;
                }
            }
            Some("plan" | "plan_update") => {
                let mut detail = String::new();
                for entry in update
                    .get("entries")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .take(64)
                {
                    let status = label(entry.get("status"), "pending");
                    let content = label(entry.get("content"), "");
                    push_bounded(&mut detail, &format!("[{status}] {content}\n"));
                }
                match projection.plan.as_mut() {
                    Some(item) => {
                        item.detail = Some(detail);
                        self.save_streaming_item(item).await?;
                    }
                    None => {
                        let item = new_item(
                            turn_id,
                            TurnItemKind::Status,
                            "Plan".to_string(),
                            detail,
                            Some(json!({ "external_agent": { "plan": true } })),
                        );
                        self.open_external_item(thread_id, turn_id, &item, None)
                            .await?;
                        projection.plan = Some(item);
                    }
                }
            }
            Some("usage_update") => {
                let used = update.get("used").and_then(Value::as_u64);
                let size = update.get("size").and_then(Value::as_u64);
                if used.is_some() || size.is_some() {
                    projection.context = Some((used, size));
                    self.emit_event(
                        thread_id,
                        Some(turn_id),
                        None,
                        "turn.agent_usage",
                        json!({ "kind": "context_window", "used": used, "size": size, "source": "agent reported", "cost": "unknown" }),
                    )
                    .await?;
                }
            }
            Some("current_mode_update") => {
                let mode = bounded_id(update.get("currentModeId").or_else(|| update.get("modeId")));
                if let Some(mode) = mode {
                    self.emit_event(
                        thread_id,
                        Some(turn_id),
                        None,
                        "turn.agent_mode",
                        json!({ "mode": mode, "source": "agent reported" }),
                    )
                    .await?;
                }
            }
            Some("available_commands_update") => {
                let commands: Vec<Value> = update
                    .get("availableCommands")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .take(64)
                    .filter_map(|command| {
                        let name = bounded_id(command.get("name"))?;
                        let description = label(command.get("description"), "");
                        Some(json!({ "name": name, "description": description }))
                    })
                    .collect();
                self.emit_event(
                    thread_id,
                    Some(turn_id),
                    None,
                    "turn.agent_commands",
                    json!({ "commands": commands, "source": "agent reported" }),
                )
                .await?;
            }
            Some("session_info_update") => {
                if let Some(title) = update.get("title").and_then(Value::as_str) {
                    let title = label(Some(&Value::String(title.to_string())), "");
                    if !title.is_empty() {
                        projection.title = Some(title);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn update_external_tool(
        &self,
        thread_id: &str,
        turn_id: &str,
        projection: &mut ExternalProjection,
        call_id: &str,
        update: &Value,
    ) -> Result<()> {
        let Some(item) = projection.tools.get_mut(call_id) else {
            return Ok(());
        };
        if item.ended_at.is_some() {
            return Ok(());
        }
        if update.get("title").is_some() {
            item.summary = label(update.get("title"), &item.summary.clone());
        }
        let output = content_text(update.get("content"));
        let raw = bounded_json(update.get("rawOutput"))
            .map(|value| match value {
                Value::String(text) => clean_text(&text),
                other => other.to_string(),
            })
            .unwrap_or_default();
        let detail = item.detail.get_or_insert_default();
        push_bounded(detail, &output);
        if !raw.is_empty() {
            push_bounded(detail, &raw);
            detail.push('\n');
        }
        match tool_status(update) {
            Some(
                status @ (TurnItemLifecycleStatus::Completed | TurnItemLifecycleStatus::Failed),
            ) => {
                item.status = status;
                item.ended_at = Some(Utc::now());
                self.save_streaming_item(item).await?;
                self.emit_event(
                    thread_id,
                    Some(turn_id),
                    Some(&item.id),
                    item_terminal_event(status),
                    json!({ "item": item }),
                )
                .await?;
            }
            _ => self.save_streaming_item(item).await?,
        }
        Ok(())
    }

    async fn bridge_external_permission(
        &self,
        thread: &ThreadRecord,
        turn_id: &str,
        permission: ApprovalMode,
        plan_mode: bool,
        params: &Value,
    ) -> PermissionAnswer {
        let accepting = self
            .active
            .lock()
            .await
            .external
            .get(&thread.id)
            .and_then(|state| state.active_turn.as_ref())
            .is_some_and(|turn| turn.turn_id == turn_id && !turn.cancel.is_cancelled());
        if !accepting {
            return PermissionAnswer::Cancelled;
        }
        let agent_id = thread
            .executor
            .as_ref()
            .map(|record| record.agent_id.clone())
            .unwrap_or_default();
        let tool = params.get("toolCall").cloned().unwrap_or(Value::Null);
        let tool_call_id = bounded_id(tool.get("toolCallId"));
        let title = label(tool.get("title"), "The agent wants to use a tool");
        let kind = tool_kind(&tool);
        let paths = locations(&tool);
        let permission_paths = tool
            .get("locations")
            .and_then(Value::as_array)
            .filter(|all| all.len() == paths.len())
            .map(|_| paths.as_slice())
            .unwrap_or_default();
        let input = bounded_json(tool.get("rawInput"));
        let options = permission_options(params);
        let option_names: Vec<Value> = options
            .iter()
            .map(|option| json!({ "id": option.id, "name": option.name, "kind": option.kind }))
            .collect();

        if let Some((allow, answer)) = posture_answer(
            thread,
            permission,
            plan_mode,
            kind,
            permission_paths,
            &options,
        )
        .await
        {
            let approval_id = Self::mint_approval_id();
            let option_id = match &answer {
                PermissionAnswer::Selected(id) => Some(id.clone()),
                PermissionAnswer::Cancelled => None,
            };
            if let Err(error) = self
                .emit_event(
                    &thread.id,
                    Some(turn_id),
                    None,
                    "approval.decided",
                    json!({
                        "approval_id": approval_id, "tool_call_id": tool_call_id,
                        "decision": if allow { "allow" } else { "deny" },
                        "remember": false, "auto": true, "decided_by": "policy",
                        "posture": codewhale_runtime::runtime_policy::approval_wire(permission),
                        "plan_mode": plan_mode, "option_id": option_id,
                        "external_agent": agent_id, "summary": title,
                    }),
                )
                .await
            {
                tracing::warn!(%error, "recording an automatic external approval failed");
                return PermissionAnswer::Cancelled;
            }
            return answer;
        }

        let request = PendingApprovalRequest {
            id: String::new(),
            turn_id: turn_id.to_string(),
            tool_name: format!("{agent_id}:{kind}"),
            input: input.clone(),
            description: title.clone(),
            intent_summary: None,
            tool_call_id: tool_call_id.clone(),
            summary: Some(title.clone()),
        };
        let (approval_id, decision, settled) = {
            let active = self.active.lock().await;
            let accepting = active
                .external
                .get(&thread.id)
                .and_then(|state| state.active_turn.as_ref())
                .is_some_and(|turn| turn.turn_id == turn_id && !turn.interrupt_requested);
            if !accepting {
                return PermissionAnswer::Cancelled;
            }
            self.register_agent_approval(&thread.id, request)
        };
        if let Err(error) = self
            .emit_event(
                &thread.id,
                Some(turn_id),
                None,
                "approval.required",
                json!({
                    "id": approval_id, "approval_id": approval_id, "tool_call_id": tool_call_id,
                    "tool_name": format!("{agent_id}:{kind}"), "input": input, "summary": title,
                    "description": title, "intent_summary": Value::Null,
                    "external_agent": agent_id, "options": option_names, "locations": paths,
                }),
            )
            .await
        {
            tracing::warn!(%error, "publishing an external approval failed");
            self.cancel_pending_approval(&approval_id);
            let _ = settled.send(Some(false));
            return PermissionAnswer::Cancelled;
        }
        let (answer, mut payload) = match decision.await {
            Ok(decision) => {
                let answer = answer_for(decision, &options);
                let (allow, remember) = match decision {
                    ExternalApprovalDecision::Allow { remember } => (true, remember),
                    ExternalApprovalDecision::Deny { remember } => (false, remember),
                };
                let option_id = match &answer {
                    PermissionAnswer::Selected(id) => Some(id.clone()),
                    PermissionAnswer::Cancelled => None,
                };
                (
                    answer,
                    json!({
                        "approval_id": approval_id, "tool_call_id": tool_call_id,
                        "decision": if allow { "allow" } else { "deny" },
                        "remember": remember, "decided_by": "user",
                        "option_id": option_id, "external_agent": agent_id,
                    }),
                )
            }
            Err(_) => (
                PermissionAnswer::Cancelled,
                json!({
                    "approval_id": approval_id, "tool_call_id": tool_call_id,
                    "decision": "deny", "remember": false, "cancelled": true,
                    "decided_by": "host", "external_agent": agent_id,
                }),
            ),
        };
        if let Some(responder) = self
            .pending_approvals
            .lock()
            .get(&approval_id)
            .and_then(|entry| entry.responder)
        {
            payload["responder"] = json!(responder);
        }
        let recorded = self
            .emit_event(&thread.id, Some(turn_id), None, "approval.decided", payload)
            .await;
        self.cancel_pending_approval(&approval_id);
        match recorded {
            Ok(_) => {
                let _ = settled.send(Some(true));
                answer
            }
            Err(error) => {
                tracing::warn!(%error, "recording an external approval decision failed");
                let _ = settled.send(Some(false));
                PermissionAnswer::Cancelled
            }
        }
    }

    async fn settle_external_turn(
        &self,
        thread: &ThreadRecord,
        turn_id: &str,
        mut projection: ExternalProjection,
        outcome: Result<String, String>,
    ) -> Result<()> {
        let (interrupted, killed) = self
            .active
            .lock()
            .await
            .external
            .get(&thread.id)
            .and_then(|state| state.active_turn.as_ref())
            .filter(|turn| turn.turn_id == turn_id)
            .map_or((false, false), |turn| {
                (turn.interrupt_requested, turn.killed)
            });
        let (status, error) = match (&outcome, projection.failure.take()) {
            (_, Some(failure)) => (RuntimeTurnStatus::Failed, Some(failure)),
            (Ok(reason), None) if reason == "end_turn" => (RuntimeTurnStatus::Completed, None),
            (Ok(reason), None) if reason == "cancelled" || interrupted => {
                (RuntimeTurnStatus::Interrupted, None)
            }
            (Ok(reason), None) => (
                RuntimeTurnStatus::Failed,
                Some(format!(
                    "The agent stopped: {}",
                    label(Some(&Value::String(reason.clone())), "unknown")
                )),
            ),
            (Err(_), None) if killed => (
                RuntimeTurnStatus::Interrupted,
                Some(KILLED_REASON.to_string()),
            ),
            (Err(_), None) if interrupted => (RuntimeTurnStatus::Interrupted, None),
            (Err(reason), None) => (RuntimeTurnStatus::Failed, Some(reason.clone())),
        };
        self.pending_approvals
            .lock()
            .retain(|_, entry| entry.thread_id != thread.id || entry.request.turn_id != turn_id);
        let item_status = terminal_item_status(status);
        self.close_streaming_item(&thread.id, turn_id, &mut projection.message, item_status)
            .await?;
        self.close_streaming_item(&thread.id, turn_id, &mut projection.reasoning, item_status)
            .await?;
        let mut plan = projection.plan.take();
        self.close_streaming_item(
            &thread.id,
            turn_id,
            &mut plan,
            TurnItemLifecycleStatus::Completed,
        )
        .await?;
        let open_tools: Vec<TurnItemRecord> = projection
            .tools
            .into_values()
            .filter(|item| item.ended_at.is_none())
            .collect();
        for item in open_tools {
            let tool_status = if status == RuntimeTurnStatus::Failed {
                TurnItemLifecycleStatus::Failed
            } else {
                TurnItemLifecycleStatus::Interrupted
            };
            let mut slot = Some(item);
            self.close_streaming_item(&thread.id, turn_id, &mut slot, tool_status)
                .await?;
        }

        let store = self.store.clone();
        let settled_turn = turn_id.to_string();
        let settled_thread = thread.id.clone();
        let title = projection.title.take();
        let turn = tokio::task::spawn_blocking(move || -> Result<TurnRecord> {
            let now = Utc::now();
            let turn = {
                let _turn_mutation = store.turn_mutation.lock();
                let mut turn = store.load_turn(&settled_turn)?;
                if turn.status.is_active_work() {
                    turn.status = status;
                    turn.error = error;
                    turn.ended_at = Some(now);
                    turn.duration_ms = turn.started_at.map(|started| {
                        now.signed_duration_since(started).num_milliseconds().max(0) as u64
                    });
                    store.save_turn(&turn)?;
                }
                turn
            };
            let _thread_mutation = store.thread_mutation.lock();
            let mut current = store.load_thread(&settled_thread)?;
            current.updated_at = now;
            if current.title.is_none() {
                current.title = title;
            }
            store.save_thread(&current)?;
            Ok(turn)
        })
        .await
        .context("external turn settlement worker failed")??;
        self.emit_turn_completed_if_missing(&turn, false).await?;

        let session_lost = outcome.is_err();
        let removed = {
            let mut active = self.active.lock().await;
            if let Some(state) = active.external.get_mut(&thread.id)
                && state
                    .active_turn
                    .as_ref()
                    .is_some_and(|active| active.turn_id == turn_id)
            {
                state.active_turn = None;
            }
            if session_lost {
                active.external.remove(&thread.id)
            } else {
                None
            }
        };
        if let Some(state) = removed {
            let _ = tokio::task::spawn_blocking(move || state.session.stop_blocking()).await;
            let reason = match &outcome {
                Err(reason) => label(Some(&Value::String(reason.clone())), "the agent exited"),
                Ok(_) => "the agent exited".to_string(),
            };
            self.emit_event(
                &thread.id,
                None,
                None,
                "executor.stopped",
                json!({ "reason": reason }),
            )
            .await?;
        }
        Ok(())
    }

    pub(super) async fn interrupt_external_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<TurnRecord> {
        let session = {
            let mut active = self.active.lock().await;
            let Some(state) = active.external.get_mut(thread_id) else {
                bail!("Thread is not loaded");
            };
            let Some(active_turn) = state.active_turn.as_mut() else {
                bail!("No active turn on thread {thread_id}");
            };
            if active_turn.turn_id != turn_id {
                bail!("Turn {turn_id} is not active on thread {thread_id}");
            }
            active_turn.interrupt_requested = true;
            active_turn.cancel.cancel();
            self.pending_approvals.lock().retain(|_, entry| {
                entry.thread_id != thread_id || entry.request.turn_id != turn_id
            });
            Arc::clone(&state.session)
        };
        let canceller = session.canceller.clone();
        let active = Arc::clone(&self.active);
        let watched_thread = thread_id.to_string();
        let watched_turn = turn_id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(CANCEL_GRACE).await;
            let still_running = {
                let mut active = active.lock().await;
                active
                    .external
                    .get_mut(&watched_thread)
                    .and_then(|state| state.active_turn.as_mut())
                    .filter(|turn| turn.turn_id == watched_turn)
                    .map(|turn| turn.killed = true)
                    .is_some()
            };
            if still_running {
                let _ = tokio::task::spawn_blocking(move || {
                    session.host.lock().stop_worker(&session.worker_id)
                })
                .await;
            }
        });
        match tokio::time::timeout(
            CANCEL_GRACE,
            tokio::task::spawn_blocking(move || canceller.cancel()),
        )
        .await
        {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => {
                tracing::warn!(thread_id, %error, "sending session/cancel failed")
            }
            Ok(Err(error)) => {
                tracing::warn!(thread_id, %error, "external agent cancel worker failed")
            }
            Err(_) => tracing::warn!(thread_id, "sending session/cancel timed out"),
        }
        self.emit_event(
            thread_id,
            Some(turn_id),
            None,
            "turn.interrupt_requested",
            json!({ "thread_id": thread_id, "turn_id": turn_id }),
        )
        .await?;
        let store = self.store.clone();
        let turn_id = turn_id.to_string();
        tokio::task::spawn_blocking(move || store.load_turn(&turn_id))
            .await
            .context("external interrupted turn read failed")?
    }

    pub(in crate::runtime_threads) fn register_agent_approval(
        &self,
        thread_id: &str,
        mut request: PendingApprovalRequest,
    ) -> (
        String,
        oneshot::Receiver<ExternalApprovalDecision>,
        watch::Sender<Option<bool>>,
    ) {
        let mut pending = self.pending_approvals.lock();
        loop {
            let id = Self::mint_approval_id();
            if let std::collections::hash_map::Entry::Vacant(entry) = pending.entry(id.clone()) {
                request.id = id.clone();
                let (tx, rx) = oneshot::channel();
                let (settled_tx, settled_rx) = watch::channel(None);
                entry.insert(PendingApprovalEntry {
                    thread_id: thread_id.to_string(),
                    request,
                    sender: Some(tx),
                    publication: None,
                    proposed: None,
                    responder: None,
                    agent_settled: Some(settled_rx),
                });
                return (id, rx, settled_tx);
            }
        }
    }
}

pub(super) async fn await_agent_settlement(
    mut settled: watch::Receiver<Option<bool>>,
) -> Result<bool> {
    let result = tokio::time::timeout(APPROVAL_SETTLE_TIMEOUT, async {
        settled
            .wait_for(|outcome| outcome.is_some())
            .await
            .map(|outcome| *outcome)
    })
    .await;
    match result {
        Ok(Ok(Some(true))) => Ok(true),
        Ok(Ok(Some(false))) => bail!("the agent's approval decision could not be recorded"),
        Ok(Ok(None) | Err(_)) => Ok(false),
        Err(_) => bail!("the agent's approval decision was not recorded in time"),
    }
}
