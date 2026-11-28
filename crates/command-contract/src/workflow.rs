use crate::config::CommandWorkflowSummary;
use serde::Serialize;

pub const WORKFLOW_DRAFT_INSTRUCTION_PREFIX: &str = "[codewhale.workflow-draft.v1]";
pub const WORKFLOW_CONFIRM_INSTRUCTION_PREFIX: &str = "[codewhale.workflow-confirm.v1]";
pub const WORKFLOW_OBJECTIVE_MAX_CHARS: usize = 1_000;
pub const WORKFLOW_DISPLAY_MAX_CHARS: usize = 160;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct WorkflowDraftEnvelope {
    pub id: String,
    pub objective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
}

pub fn envelope_from_instruction(prefix: &str, instruction: &str) -> Option<WorkflowDraftEnvelope> {
    let first_line = instruction.lines().next()?;
    let encoded = first_line.strip_prefix(prefix)?;
    serde_json::from_str(encoded).ok()
}

pub enum PendingWorkflowDraft {
    Ready(WorkflowDraftEnvelope),
    NoDraft,
    NotReviewed,
}

/// One workflow run as the human-facing `/workflow` command reads it: a
/// bounded projection of the run record with no raw event payloads and no
/// filesystem paths beyond the source file name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostWorkflowRunLine {
    pub run_id: String,
    /// Human-facing stage derived from the durable owner record and its typed
    /// event tail: queued | running | waiting | completed | degraded | failed |
    /// cancelled. This is presentation state, not a second lifecycle owner.
    pub status: &'static str,
    /// Whether the canonical owner record is still nonterminal. Controls key
    /// off this bit instead of reverse-parsing the display stage.
    pub active: bool,
    /// The run's goal, its workflow id, or the source file name.
    pub label: String,
    pub started_at_ms: u64,
    pub completed_at_ms: Option<u64>,
    pub child_count: usize,
    pub last_progress: Option<String>,
    pub error: Option<String>,
}

pub trait CommandWorkflowReviewContext {
    fn pending_draft(&self) -> PendingWorkflowDraft;
}

pub trait CommandWorkflowContext {
    fn runs(&self) -> Vec<HostWorkflowRunLine>;
    fn cancel(&mut self, run_id: &str) -> Result<HostWorkflowRunLine, String>;
    fn settings(&self) -> CommandWorkflowSummary;
}

pub struct AutoReviewReceipt {
    pub plan_mode: bool,
    pub broad_allows_paused: bool,
}

pub trait CommandAutoReviewContext {
    fn enable_auto_review(&mut self) -> Result<AutoReviewReceipt, String>;
}

/// Render the `/config workflow` report from the typed effective facts.
pub fn render_workflow_summary(summary: CommandWorkflowSummary) -> String {
    let on = |value: bool| if value { "on" } else { "off" };
    [
        "[workflow] — config.toml".to_string(),
        format!(
            "automatic = {}  · the agent may start a workflow itself for broad or staged work; off means only /workflow starts one",
            on(summary.automatic)
        ),
        format!(
            "auto_start_read_only = {}  · read-only plans start without an approval card",
            on(summary.auto_start_read_only)
        ),
        format!(
            "require_approval_for_writes = {}  · plans that write, use shell/network, or elevate show an approval card first",
            on(summary.require_approval_for_writes)
        ),
        format!(
            "max_children = {} · max_concurrent = {} · max_depth = {}  · hard ceilings for one run",
            summary.max_children, summary.max_concurrent, summary.max_depth
        ),
        format!(
            "default_token_budget = {}  · shared admission cap for a run and its children (0 = none)",
            summary.default_token_budget
        ),
        String::new(),
        "[goal] — config.toml".to_string(),
        format!(
            "max_continuations = {}  · automatic continuation passes before a goal pauses; 0 = unlimited (completion, blocked, or you stop it)",
            summary.goal_max_continuations
        ),
        format!(
            "enforce_token_budget = {}  · true = a goal's token budget is a hard stop; false = advisory telemetry",
            on(summary.goal_enforce_token_budget)
        ),
    ]
    .join("\n")
}
