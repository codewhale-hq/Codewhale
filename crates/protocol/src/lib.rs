// Serde-only leaf crate shared by every surface, including the TUI
// alt-screen. Raw stdio prints must never appear here (spec §7,
// `no_stdout_from_core`).
#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod agent_mail;
pub mod agent_run;
pub mod engine_owner;
pub mod event_msg;
pub mod fleet;
pub mod ids;
pub mod journal;
pub mod op;
pub mod runtime;

/// Common trait for lifecycle status enums across the protocol layer.
///
/// Every status enum — thread, goal, fleet run, worker, and job status —
/// implements this trait so generic code can ask three universal questions
/// without matching on every variant.
pub trait Status {
    /// Returns `true` when this status represents a final, non-progressable state
    /// (e.g. Completed, Failed, Cancelled, Archived, Retired).
    fn is_terminal(&self) -> bool;

    /// Returns `true` when work is currently in-flight
    /// (e.g. Running, Active, Busy, Queued, Pending).
    fn is_active(&self) -> bool;

    /// Returns `true` when the item has been explicitly paused by the user
    /// or system (e.g. Paused).
    fn is_paused(&self) -> bool;
}

/// A single message entry in a conversation thread.
///
/// Messages form a tree structure via [`parent_entry_id`](Self::parent_entry_id),
/// enabling conversation branching and forking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRecord {
    /// Auto-incremented unique identifier for this message.
    pub id: i64,
    /// ID of the thread this message belongs to.
    pub thread_id: String,
    /// Role of the message sender (e.g. `"user"`, `"assistant"`, `"system"`).
    pub role: String,
    /// Text content of the message.
    pub content: String,
    /// Optional structured item payload (tool calls, tool results, etc.).
    pub item: Option<Value>,
    /// Unix timestamp (seconds) when the message was created.
    pub created_at: i64,
    /// ID of the parent message, forming a tree structure. `None` for root messages.
    pub parent_entry_id: Option<i64>,
}

/// A complete immutable legacy SQLite graph, not a limit or active-branch projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyThreadHistory {
    pub version: u32,
    pub state_store_id: String,
    pub thread_id: String,
    pub current_leaf_id: Option<i64>,
    pub messages: Vec<MessageRecord>,
    /// Captured in the same SQLite snapshot; imported Active goals stay paused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<ThreadGoal>,
}

pub const MAX_CANONICAL_HISTORY_ENTRIES: usize = 16_384;
pub const MAX_CANONICAL_HISTORY_BYTES: usize = 8 * 1024 * 1024;

/// An authenticated import proposal carries conversation data only. Routing and
/// permissions remain the current owner's create-thread policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalHistoryImportRequest {
    pub version: u32,
    pub operation_key: String,
    pub expected_data_dir: PathBuf,
    pub expected_execution_scope: String,
    /// Existing bare compatibility links attach their full source graph here;
    /// the owner verifies the actual bound canonical document, never remints it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_runtime_thread_id: Option<String>,
    pub workspace: PathBuf,
    pub model: Option<String>,
    pub history: LegacyThreadHistory,
}

/// Durable result of the actual canonical owner operation. Its scope fields are
/// captured from RuntimeStoreBinding; historical receipts never authenticate a
/// current process or convey credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalThreadReceipt {
    pub version: u32,
    pub data_dir: PathBuf,
    pub execution_scope: String,
    pub operation_key: String,
    pub request_digest: String,
    pub history_digest: String,
    pub runtime_thread_id: String,
    pub session_id: String,
}

/// A complete read projection from the actual Engine and its existing saved
/// journal. `session` retains the established SavedSession wire shape, including
/// every branch; it is data, never a policy or operation-commit receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalThreadSnapshot {
    pub version: u32,
    pub data_dir: PathBuf,
    pub execution_scope: String,
    pub runtime_thread_id: String,
    pub saved_session_id: Option<String>,
    pub saved_document_digest: Option<String>,
    pub document_digest: String,
    pub session_goal_digest: String,
    pub session: Value,
}

/// One client-captured intent. The key is retained after an uncertain reply;
/// retrying it never allocates another canonical identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalThreadMutationRequest {
    pub version: u32,
    pub operation_key: String,
    pub expected_data_dir: PathBuf,
    pub expected_execution_scope: String,
    pub workspace: PathBuf,
    pub mutation: CanonicalThreadMutation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum CanonicalThreadMutation {
    /// The existing CreateThreadRequest shape, checked by the owner. Conversation
    /// data and an imported document never supply a routing or permission ceiling.
    Create { config: Value },
    Resume {
        source: CanonicalHistorySource,
        #[serde(default)]
        options: CanonicalHistoryOptions,
    },
    Fork {
        source: CanonicalHistorySource,
        #[serde(default)]
        options: CanonicalHistoryOptions,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected_entry_id: Option<String>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalHistoryOptions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offered_history: Vec<Value>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub overrides: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_session_goal_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CanonicalHistorySource {
    Thread {
        runtime_thread_id: String,
        expected_document_digest: String,
    },
    /// Local mounted handoff supplies the already protected complete document;
    /// the actual owner reopens it and compares the same whole-document digest.
    SavedSession {
        session: Value,
        expected_document_digest: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalThreadOperationKind {
    Create,
    Resume,
    Fork,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalThreadOperationAssociation {
    pub kind: CanonicalThreadOperationKind,
    pub source_runtime_thread_id: Option<String>,
    pub source_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalThreadOperationLookup {
    pub version: u32,
    pub operation_key: String,
    pub expected_data_dir: PathBuf,
    pub expected_execution_scope: String,
    pub workspace: PathBuf,
}

/// Explicit completion of an already-prepared retained intent, with no source proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalThreadOperationRecovery {
    pub operation: CanonicalThreadOperationLookup,
    pub association: CanonicalThreadOperationAssociation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum CanonicalThreadOperationStatus {
    Absent,
    Pending {
        receipt: CanonicalThreadReceipt,
        association: CanonicalThreadOperationAssociation,
    },
    Committed {
        receipt: CanonicalThreadReceipt,
        association: CanonicalThreadOperationAssociation,
    },
}

/// Closed routing receipt captured from the held Runtime owner, never a bearer.
/// Authentication also requires the actual connected kernel peer identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeOwnerReceipt {
    pub version: u32,
    pub data_dir: PathBuf,
    pub execution_scope: String,
    pub lease_generation: String,
    pub pid: u32,
    pub process_start: String,
    pub principal: String,
    pub socket_path: PathBuf,
    pub config_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub body: T,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    Running,
    Idle,
    Completed,
    Failed,
    Paused,
    Archived,
}

impl Status for ThreadStatus {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Archived)
    }
    fn is_active(&self) -> bool {
        matches!(self, Self::Running)
    }
    fn is_paused(&self) -> bool {
        matches!(self, Self::Paused)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    Interactive,
    Resume,
    Fork,
    Api,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub preview: String,
    pub ephemeral: bool,
    pub model_provider: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub status: ThreadStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    pub cwd: PathBuf,
    pub cli_version: String,
    pub source: SessionSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadGoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

impl Status for ThreadGoalStatus {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete)
    }
    fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }
    fn is_paused(&self) -> bool {
        matches!(self, Self::Paused)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadGoal {
    pub thread_id: String,
    pub goal_id: String,
    pub objective: String,
    pub status: ThreadGoalStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub continuation_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_gap_fingerprint: Option<String>,
    #[serde(default)]
    pub repeated_gap_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_gap_pass: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_reason: Option<GoalPauseReason>,
}

/// Why an unfinished goal is paused. Shared by every durable host projection.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalPauseReason {
    User,
    Backoff,
    NoProgress,
    UsageLimit,
    BudgetLimit,
    /// The model finished a stage and handed control back, rather than a
    /// reported blocker or a completion. It is not a judgement about the work,
    /// so the user's next message resumes the goal (the same shape as a
    /// runtime stop, which is also resumable by writing to it).
    Yielded,
    /// A reason a **newer** build wrote. Kept so an older binary can still read
    /// the durable record instead of failing the whole goal load on an unknown
    /// variant; nothing constructs it. The resume path treats it as "not a
    /// hand-back", which is the conservative reading.
    #[serde(other)]
    Unrecognized,
}

impl GoalPauseReason {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Backoff => "run limit",
            Self::NoProgress => "no progress",
            Self::UsageLimit => "usage limit",
            Self::BudgetLimit => "budget limit",
            Self::Yielded => "handed back",
            Self::Unrecognized => "unrecognized",
        }
    }
}

/// Validate the compact stall history without retaining verifier prose.
/// Legacy records with the entire history absent start with an empty window.
pub const MAX_REPEATED_GAP_COUNT: u32 = 3;

pub fn validate_goal_stall_state(
    fingerprint: Option<&str>,
    count: u32,
    pass: Option<u32>,
    continuation_count: u32,
) -> Result<(), &'static str> {
    match (fingerprint, count, pass) {
        (None, 0, None) => Ok(()),
        (Some(digest), 1..=MAX_REPEATED_GAP_COUNT, Some(pass))
            if digest.len() == 64
                && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                && pass <= continuation_count
                && count <= pass.saturating_add(1) =>
        {
            Ok(())
        }
        _ => Err("invalid persisted goal stall history"),
    }
}

impl ThreadGoal {
    pub fn validate_stall_state(&self) -> Result<(), &'static str> {
        validate_goal_stall_state(
            self.last_gap_fingerprint.as_deref(),
            self.repeated_gap_count,
            self.last_gap_pass,
            u32::try_from(self.continuation_count.max(0)).unwrap_or(u32::MAX),
        )
    }

    /// Restore a durably impossible record as paused. The engine pauses
    /// NoProgress in the same locked mutation that fills the stall window, so
    /// a persisted record that is still Active at the ceiling is corrupt
    /// (e.g. a crash between the counter write and the pause). Returns true
    /// when the record was healed.
    pub fn normalize_restored_stall_state(&mut self) -> bool {
        if matches!(self.status, ThreadGoalStatus::Active)
            && self.repeated_gap_count >= MAX_REPEATED_GAP_COUNT
        {
            self.status = ThreadGoalStatus::Paused;
            self.pause_reason = Some(GoalPauseReason::NoProgress);
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadStartParams {
    /// Captured once for this user intent and retained for an uncertain reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub persist_extended_history: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadResumeParams {
    /// Captured once for this user intent and retained for an uncertain reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_key: Option<String>,
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
    #[serde(default)]
    pub persist_extended_history: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadForkParams {
    /// Captured once for this user intent and retained for an uncertain reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_key: Option<String>,
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<String>,
    #[serde(default)]
    pub persist_extended_history: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadListParams {
    #[serde(default)]
    pub include_archived: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadReadParams {
    pub thread_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSetNameParams {
    pub thread_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadGoalSetParams {
    pub thread_id: String,
    pub objective: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadGoalGetParams {
    pub thread_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadGoalClearParams {
    pub thread_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadGoalProgressParams {
    pub thread_id: String,
    #[serde(default)]
    pub token_delta: i64,
    #[serde(default)]
    pub time_delta_seconds: i64,
    #[serde(default)]
    pub record_continuation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ThreadRequest {
    Create {
        #[serde(default)]
        metadata: Value,
    },
    Start(ThreadStartParams),
    Resume(ThreadResumeParams),
    Fork(ThreadForkParams),
    List(ThreadListParams),
    Read(ThreadReadParams),
    SetName(ThreadSetNameParams),
    GoalSet(ThreadGoalSetParams),
    GoalGet(ThreadGoalGetParams),
    GoalClear(ThreadGoalClearParams),
    GoalRecordProgress(ThreadGoalProgressParams),
    Archive {
        thread_id: String,
    },
    Unarchive {
        thread_id: String,
    },
    Message {
        thread_id: String,
        input: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<runtime::RuntimeImageInput>,
        #[serde(
            default,
            rename = "maxOutputTokens",
            alias = "max_output_tokens",
            skip_serializing_if = "Option::is_none"
        )]
        max_output_tokens: Option<std::num::NonZeroU32>,
    },
}

/// Response to a [`ThreadRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadResponse {
    /// The thread this response pertains to.
    pub thread_id: String,
    /// Human-readable status string (e.g. `"ok"`, `"error"`).
    pub status: String,
    /// The thread details, when a single thread is returned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<Thread>,
    /// List of threads, populated by `List` requests.
    #[serde(default)]
    pub threads: Vec<Thread>,
    /// Thread goal returned by goal get/set requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<ThreadGoal>,
    /// The model used for the thread, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The model provider used for the thread.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    /// The working directory of the thread.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// The active approval policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    /// The active sandbox configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    /// Streaming events associated with this response.
    #[serde(default)]
    pub events: Vec<EventFrame>,
    /// Arbitrary additional response data.
    #[serde(default)]
    pub data: Value,
}

/// Application-level requests that are not tied to a specific thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppRequest {
    /// Query the server's capabilities.
    Capabilities,
    /// Read a configuration value by key.
    ConfigGet { key: String },
    /// Set a configuration key to a value.
    ConfigSet { key: String, value: String },
    /// Remove a configuration key.
    ConfigUnset { key: String },
    /// List all configuration entries.
    ConfigList,
    /// Reload configuration from disk and apply to the live runtime.
    ///
    /// Re-reads both `config.toml` and the sibling `permissions.toml`,
    /// refreshing the live `Runtime.config` and `Runtime.exec_policy`
    /// so headless clients can pick up external config-file *and*
    /// permission-rule edits without restarting.
    ///
    /// Mirrors the TUI `reload_runtime_config` codepath for everything
    /// reachable from the headless `Runtime`. MCP server connections
    /// are not refreshed — changing `mcp_config_path` or the referenced
    /// `mcp.json` still requires a headless-runtime restart. The TUI's
    /// explicit `/mcp reload` operation is not part of this protocol path.
    ConfigReload,
    /// List available models.
    Models,
    /// List threads that are currently loaded in memory.
    ThreadLoadedList,
    /// Submit answers to a prior [`EventFrame::UserInputRequest`].
    ///
    /// `request_id` must match a pending clarification request. Headless
    /// clients use this to return the user's selections back to the runtime.
    SubmitUserInput {
        request_id: String,
        answers: Vec<UserInputAnswerEvent>,
    },
}

/// Response to an [`AppRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppResponse {
    /// Whether the request succeeded.
    pub ok: bool,
    /// The response payload.
    pub data: Value,
    /// Streaming events associated with this response.
    #[serde(default)]
    pub events: Vec<EventFrame>,
}

/// A simple prompt request that sends text to the model and returns output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptRequest {
    #[serde(
        default,
        rename = "maxOutputTokens",
        alias = "max_output_tokens",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_output_tokens: Option<std::num::NonZeroU32>,
    /// Optional thread context for the prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// The prompt text.
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<runtime::RuntimeImageInput>,
    /// Model override, or the default if omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Response to a [`PromptRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptResponse {
    /// The model's output text.
    pub output: String,
    /// The model that produced the output.
    pub model: String,
    /// Streaming events associated with this response.
    #[serde(default)]
    pub events: Vec<EventFrame>,
}

/// Policy controlling when the agent must ask the user for approval before acting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AskForApproval {
    /// Ask for approval unless the action is on a trusted path/resource.
    UnlessTrusted,
    /// Only ask after a tool call fails.
    OnFailure,
    /// Ask every time a tool call is requested.
    OnRequest,
    /// Reject the action without asking, with details on which categories are blocked.
    Reject {
        sandbox_approval: bool,
        rules: bool,
        mcp_elicitations: bool,
    },
    /// Never ask; auto-approve all actions.
    Never,
}

/// Classification of tool invocation origin.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// A built-in function tool.
    Function,
    /// An MCP (Model Context Protocol) tool.
    Mcp,
}

/// Parameters for executing a local shell command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalShellParams {
    /// The shell command to execute.
    pub command: String,
    /// Working directory for the command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Timeout in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// The payload of a tool call, discriminated by tool type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolPayload {
    /// A built-in function call with JSON-encoded arguments.
    Function { arguments: String },
    /// A custom tool invocation with a free-form input string.
    Custom { input: String },
    /// A local shell command execution.
    LocalShell { params: LocalShellParams },
    /// An MCP tool invocation targeting a specific server and tool.
    Mcp {
        server: String,
        tool: String,
        raw_arguments: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        raw_tool_call_id: Option<String>,
    },
}

/// The result of a tool call, discriminated by tool type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolOutput {
    /// Result of a built-in function call.
    Function {
        /// The output body, if any.
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<Value>,
        /// Whether the call succeeded.
        success: bool,
    },
    /// Result of an MCP tool call.
    Mcp {
        /// The result value returned by the MCP server.
        result: Value,
    },
}

impl ToolOutput {
    /// Returns the tool's application-level success independently of transport.
    ///
    /// MCP success requires the top-level `isError` field to be omitted or the
    /// literal boolean `false`; malformed present metadata fails closed.
    pub fn success(&self) -> bool {
        match self {
            Self::Function { success, .. } => *success,
            Self::Mcp { result } => {
                matches!(result.get("isError"), None | Some(Value::Bool(false)))
            }
        }
    }
}

/// Action to take for a network policy rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicyRuleAction {
    /// Allow network access to the host.
    Allow,
    /// Deny network access to the host.
    Deny,
}

/// A proposed amendment to the network access policy for a specific host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkPolicyAmendment {
    /// The host to amend the policy for.
    pub host: String,
    /// The action to apply.
    pub action: NetworkPolicyRuleAction,
}

/// A user's decision on an approval request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReviewDecision {
    /// Approve the action.
    Approved,
    /// Approve and also amend the execution policy.
    ApprovedExecpolicyAmendment,
    /// Approve for the remainder of this session only.
    ApprovedForSession,
    /// Approve with a network policy amendment.
    NetworkPolicyAmendment {
        host: String,
        action: NetworkPolicyRuleAction,
    },
    /// Deny the action.
    Denied,
    /// Abort the entire turn.
    Abort,
}

/// Status of an MCP server during startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpStartupStatus {
    /// The server is in the process of starting.
    Starting,
    /// The server is ready to accept requests.
    Ready,
    /// The server failed to start.
    Failed { error: String },
    /// Startup was cancelled.
    Cancelled,
}

/// A progress update for a single MCP server's startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpStartupUpdateEvent {
    /// Name of the MCP server.
    pub server_name: String,
    /// Current startup status.
    pub status: McpStartupStatus,
}

/// Details of an MCP server that failed to start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpStartupFailure {
    /// Name of the MCP server that failed.
    pub server_name: String,
    /// Error description.
    pub error: String,
}

/// Summary event emitted once all MCP servers have finished starting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpStartupCompleteEvent {
    /// Servers that started successfully.
    pub ready: Vec<String>,
    /// Servers that failed to start.
    pub failed: Vec<McpStartupFailure>,
    /// Servers whose startup was cancelled.
    pub cancelled: Vec<String>,
}

/// Context about a network access request that requires approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkApprovalContext {
    /// The host being accessed.
    pub host: String,
    /// The network protocol (e.g. `"https"`, `"tcp"`).
    pub protocol: String,
}

/// A selectable option presented to the user in a clarification question.
///
/// Headless serialization shape for the `request_user_input` model tool,
/// mirrored after the TUI's `UserInputOption`. Shared by the
/// [`EventFrame::UserInputRequest`] frame and the [`AppRequest::SubmitUserInput`]
/// reply path so both surfaces agree on the question schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserInputOptionEvent {
    /// Short label for the option (also the value submitted when picked).
    pub label: String,
    /// Longer description shown alongside the label.
    pub description: String,
}

/// A single clarification question posed to the user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserInputQuestionEvent {
    /// Compact header shown as the question title.
    pub header: String,
    /// Stable identifier used to correlate answers back to this question.
    pub id: String,
    /// The question body.
    pub question: String,
    /// 2-4 suggested answers.
    pub options: Vec<UserInputOptionEvent>,
    /// When `true`, the client should also offer a free-text response.
    #[serde(default)]
    pub allow_free_text: bool,
    /// When `true`, the user may select more than one option.
    #[serde(default)]
    pub multi_select: bool,
}

/// An event requesting structured user input via a model-tool call.
///
/// Sibling of [`ExecApprovalRequestEvent`] for the clarification-question
/// flow. Emitted fire-and-return by `Runtime::invoke_tool` when the model
/// invokes `request_user_input` in a headless context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserInputRequestEvent {
    /// Identifier of the tool call requesting input.
    pub call_id: String,
    /// The turn during which the request was made.
    pub turn_id: String,
    /// Unique identifier for this user-input request (clients reply with it).
    pub request_id: String,
    /// 1-3 questions to present.
    pub questions: Vec<UserInputQuestionEvent>,
}

/// One answer to a clarification question.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserInputAnswerEvent {
    /// The `id` of the question this answer corresponds to.
    pub id: String,
    /// The selected option's label, or `"Other"` for a free-text response.
    pub label: String,
    /// The resolved value (option label, or the typed free-text).
    pub value: String,
}

/// An event requesting user approval for a command execution or patch application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecApprovalRequestEvent {
    /// Identifier of the tool call requesting approval.
    pub call_id: String,
    /// Unique identifier for this approval request.
    pub approval_id: String,
    /// The turn during which the request was made.
    pub turn_id: String,
    /// The command that would be executed.
    pub command: String,
    /// The working directory for the command.
    pub cwd: String,
    /// Human-readable reason why approval is needed.
    pub reason: String,
    /// Policy rule that matched this approval request, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_rule: Option<Box<str>>,
    /// Network context if the approval involves network access.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_approval_context: Option<NetworkApprovalContext>,
    /// Proposed execution policy rule amendments.
    #[serde(default)]
    pub proposed_execpolicy_amendment: Vec<String>,
    /// Proposed network policy amendments.
    #[serde(default)]
    pub proposed_network_policy_amendments: Vec<NetworkPolicyAmendment>,
    /// Additional permissions being requested.
    #[serde(default)]
    pub additional_permissions: Vec<String>,
    /// The set of decisions the user can choose from.
    #[serde(default)]
    pub available_decisions: Vec<ReviewDecision>,
}

/// The channel a response delta is being written to.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseChannel {
    /// The main visible text output.
    #[default]
    Text,
    /// Internal reasoning / chain-of-thought output.
    Reasoning,
}

impl ResponseChannel {
    /// Returns `true` if this is the `Text` channel.
    pub const fn is_text(&self) -> bool {
        matches!(self, ResponseChannel::Text)
    }
}

/// A user's approval decision sent in response to an approval request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDecisionRequest {
    /// The decision identifier (e.g. `"approved"`, `"denied"`).
    pub decision: String,
    /// Whether to remember this decision for future similar requests.
    #[serde(default)]
    pub remember: bool,
}

/// A single streaming event frame emitted during agent execution.
///
/// Events are tagged by the `event` field and cover the full lifecycle of a
/// turn: response streaming, tool calls, MCP lifecycle, command execution,
/// patch application, approvals, and errors.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventFrame {
    /// A new model response has started.
    ResponseStart { response_id: String },
    /// A incremental text delta for an in-progress response.
    ResponseDelta {
        response_id: String,
        delta: String,
        #[serde(default, skip_serializing_if = "ResponseChannel::is_text")]
        channel: ResponseChannel,
    },
    /// The model response has finished.
    ResponseEnd { response_id: String },
    /// A tool call has begun.
    ToolCallStart {
        response_id: String,
        tool_name: String,
        arguments: Value,
    },
    /// A tool call has completed and produced a result.
    ToolCallResult {
        response_id: String,
        tool_name: String,
        output: Value,
    },
    /// Progress update for an MCP server starting up.
    McpStartupUpdate { update: McpStartupUpdateEvent },
    /// All MCP servers have finished starting.
    McpStartupComplete { summary: McpStartupCompleteEvent },
    /// An MCP tool call has begun.
    McpToolCallBegin {
        server_name: String,
        tool_name: String,
    },
    /// An MCP tool call has finished.
    McpToolCallEnd {
        server_name: String,
        tool_name: String,
        ok: bool,
    },
    /// User approval is needed for a command execution.
    ExecApprovalRequest { request: ExecApprovalRequestEvent },
    /// User approval is needed for applying a patch.
    ApplyPatchApprovalRequest { request: ExecApprovalRequestEvent },
    /// A model tool is requesting structured clarification input from the user.
    ///
    /// Headless sibling of the TUI's `request_user_input` modal flow.
    /// `request_id` correlates with an [`AppRequest::SubmitUserInput`] reply.
    UserInputRequest { request: UserInputRequestEvent },
    /// An MCP server is requesting user input (elicitation).
    ElicitationRequest {
        server_name: String,
        request_id: String,
        prompt: String,
    },
    /// A command has started executing.
    ExecCommandBegin { command: String, cwd: String },
    /// Incremental output from a running command.
    ExecCommandOutputDelta { command: String, delta: String },
    /// A command has finished executing.
    ExecCommandEnd { command: String, exit_code: i32 },
    /// A patch has started being applied to a file.
    PatchApplyBegin { path: String },
    /// A patch has finished being applied.
    PatchApplyEnd { path: String, ok: bool },
    /// A new turn has started within a thread.
    TurnStarted { turn_id: String },
    /// A turn has completed successfully.
    TurnComplete { turn_id: String },
    /// A turn was aborted before completion.
    TurnAborted { turn_id: String, reason: String },
    /// A thread goal was set or updated.
    ThreadGoalUpdated { goal: ThreadGoal },
    /// A thread goal was cleared.
    ThreadGoalCleared { thread_id: String },
    /// An error occurred during processing.
    Error {
        response_id: String,
        message: String,
    },
}

pub mod request;
pub mod role;

/// Pure provenance data shared by hosts and portable status reports.
pub mod cloud_facts;

pub mod display;
