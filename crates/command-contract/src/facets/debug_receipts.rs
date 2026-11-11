//! Receipt output data, moved from the shared receipt builder without schema changes.
//! The builder stays host-owned; CLI, API and slash commands share these shapes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Maximum number of listed actions; totals include omitted actions.
pub const MAX_RECEIPT_ACTIONS: usize = 2_000;
pub const RECEIPT_SCHEMA_ID: &str = "codewhale.receipt/v1";

/// Who resolved an approval request. Recorded on the decision half so a
/// receipt says "approved by you" only when a person answered. Records
/// written before this field existed carry no decider; readers report it as
/// not recorded rather than guessing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecider {
    /// A person answered the prompt: the terminal card, the app, the web
    /// mirror, or a Runtime API client acting for them.
    User,
    /// A remembered "allow/deny for this session" rule answered it.
    SessionRule,
    /// The active mode or permission posture answered it without a prompt.
    Posture,
    /// The host resolved it without a person: the turn had ended, was
    /// cancelled, or the decision channel closed.
    Host,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Receipt {
    pub schema_id: &'static str,
    pub source: ReceiptSource,
    /// Set when the receipt covers one turn instead of the whole session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<String>,
    /// Permission postures the covered turns ran under, in first-seen order
    /// (`Ask`, `Auto-Review`, `Full Access`, `Never`). Read from each turn's
    /// own record; empty when no turn recorded one.
    pub postures: Vec<&'static str>,
    pub totals: ReceiptTotals,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<ReceiptUsage>,
    pub actions: Vec<ReceiptAction>,
    /// Actions left off the list because it hit [`MAX_RECEIPT_ACTIONS`].
    pub omitted_actions: usize,
    /// Facts this record does not hold, stated instead of guessed.
    pub not_recorded: Vec<String>,
    pub claim_ceiling: [&'static str; 3],
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReceiptUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<ReceiptTokens>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<ReceiptCost>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ReceiptTokens {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<u64>,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calls: Option<usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReceiptCost {
    pub state: ReceiptCostState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    pub currency: &'static str,
    pub priced_turns: u32,
    pub unpriced_turns: u32,
    pub report: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptCostState {
    Priced,
    Partial,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A terminal session (saved transcript + approval log).
    Session,
    /// A Runtime thread (turn/item records + event log).
    Thread,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReceiptSource {
    pub kind: SourceKind,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ReceiptTotals {
    /// Distinct paths changed, by file tools or (from the turn's workspace
    /// snapshots) by anything else during the turn.
    pub files_changed: usize,
    /// Of `files_changed`, paths no file tool changed: a command, a build, or
    /// another process wrote them during the turn.
    pub files_changed_outside_file_tools: usize,
    pub files_created: usize,
    pub files_deleted: usize,
    /// Sum over changes whose line counts are recorded.
    pub lines_added: u64,
    pub lines_removed: u64,
    /// False when at least one file change has no recorded line counts, so
    /// the sums above are a floor.
    pub line_counts_complete: bool,
    pub commands: usize,
    pub commands_failed: usize,
    pub code_runs: usize,
    pub network: usize,
    pub mcp_calls: usize,
    pub plugin_calls: usize,
    pub subagents: usize,
    pub approvals: ApprovalTotals,
    /// File changes, commands, code runs, web and MCP calls, and agents that
    /// ran with no approval on record: the posture, an allow rule, or a
    /// remembered grant let them run without a prompt. A call Codewhale
    /// refused before it started, or one the record does not show starting,
    /// is not counted. Zero, with a `not_recorded` note, for a session older
    /// than its approval log.
    pub ran_without_asking: usize,
    /// Actions that ran and failed, plus failed turns.
    pub failures: usize,
    /// Calls Codewhale refused before they started: an Auto-Review or
    /// guardian block, a tool-policy or allow-list denial, a sandbox
    /// escalation the posture cannot grant, invalid input, or a tool that is
    /// not available. Not counted as run, as failed, or as ran without asking.
    pub blocked: usize,
    /// Reads, searches, and other calls that are counted but not listed
    /// unless they failed.
    pub other_tool_calls: usize,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ApprovalTotals {
    pub total: usize,
    pub approved: usize,
    pub denied: usize,
    pub timed_out: usize,
    /// Cancelled, or resolved by Codewhale because nobody could be asked
    /// (the turn had ended or stopped). Never a person's no.
    pub not_answered: usize,
    pub pending: usize,
    /// Who gave each approval counted in `approved`.
    pub approved_by: DeciderCounts,
    /// Who gave each denial counted in `denied`.
    pub denied_by: DeciderCounts,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct DeciderCounts {
    pub you: usize,
    pub session_rule: usize,
    pub posture: usize,
    /// The record predates Codewhale keeping who decided, or came from a
    /// sub-agent's request, which does not carry it yet.
    pub not_recorded: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReceiptAction {
    /// 1-based position in the session's action order.
    pub seq: usize,
    /// Runtime turn id, or the 1-based turn number in a terminal session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// The tool name exactly as called.
    pub tool: String,
    #[serde(flatten)]
    pub what: ActionKind,
    pub status: ActionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalFact>,
    /// First line of the failure, bounded and redacted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionKind {
    FileChange {
        files: Vec<FileTouch>,
    },
    /// Files that changed in the workspace during a turn with no file tool
    /// naming them, read from the turn's before/after snapshots. A command
    /// changed them, or something else writing to the workspace did.
    WorkspaceChange {
        files: Vec<FileTouch>,
        /// More paths changed than are listed.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    Command {
        command: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i64>,
    },
    Code {
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i64>,
        /// Tool calls the program made (`execute_tools`), when recorded.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        nested: Vec<NestedCall>,
    },
    Network {
        action: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        host: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        query: Option<String>,
    },
    Mcp {
        #[serde(skip_serializing_if = "Option::is_none")]
        server: Option<String>,
        plugin: bool,
    },
    Subagent {
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        /// Last status the record holds for this agent.
        #[serde(skip_serializing_if = "Option::is_none")]
        outcome: Option<String>,
    },
    /// An approval with no matching call in the record (for example a
    /// sub-agent's request).
    Approval,
    /// Any other tool. Listed only when it failed.
    Tool,
    /// A Runtime turn that ended in failure.
    TurnFailed,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    Edited,
    Created,
    Deleted,
    /// Written whole; the record does not say whether the file existed.
    Written,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FileTouch {
    pub path: String,
    pub change: FileChangeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_added: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_removed: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NestedCall {
    pub tool: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Ok,
    /// Ran and failed.
    Failed,
    /// Did not run: held at approval (denied, timed out, never answered).
    NotRun,
    /// Did not run: Codewhale refused it before it started (an Auto-Review
    /// or guardian block, a policy or allow-list denial, a sandbox escalation
    /// the posture cannot grant, invalid input, a tool that is not
    /// available). `error` carries the reason.
    Blocked,
    Interrupted,
    Running,
    /// The record does not show whether it ran: there is no result, or a
    /// command returned an error with no exit code or shell status.
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionLabel {
    Approved,
    /// Approved with a wider sandbox after a sandbox denial.
    ApprovedWithPolicy,
    Denied,
    TimedOut,
    Cancelled,
    /// The host answered because nobody could be asked.
    Unavailable,
    Pending,
}

impl ApprovalDecisionLabel {
    pub fn ran(self) -> bool {
        matches!(self, Self::Approved | Self::ApprovedWithPolicy)
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct ApprovalFact {
    pub decision: ApprovalDecisionLabel,
    /// `None` when the decision names its own cause (timeout, pending) or the
    /// record predates deciders.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<ApprovalDecider>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<DateTime<Utc>>,
}

impl ReceiptAction {
    /// A change, command, code run, web or MCP call, or agent that ran with
    /// no approval on record.
    pub fn ran_without_asking(&self) -> bool {
        // `Unknown`, `NotRun`, and `Blocked` are left out: the call did not
        // start, or the record does not show that it did. A workspace change
        // is not a call.
        self.approval.is_none()
            && matches!(
                self.status,
                ActionStatus::Ok
                    | ActionStatus::Failed
                    | ActionStatus::Interrupted
                    | ActionStatus::Running
            )
            && matches!(
                self.what,
                ActionKind::FileChange { .. }
                    | ActionKind::Command { .. }
                    | ActionKind::Code { .. }
                    | ActionKind::Network { .. }
                    | ActionKind::Mcp { .. }
                    | ActionKind::Subagent { .. }
            )
    }
}
