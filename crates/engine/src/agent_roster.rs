//! Receipts-only projection of every agent that ran this session (#5479).
//!
//! Separated from roster glyphs and terminal rendering so engine events,
//! protocol parity, and headless audit records can track worker receipts
//! without depending on TUI layout modules.
//!
//! ## The truth rule
//!
//! Every number here is an `Option`, and `None` renders as `—`, never as `0`.
//! The distinction is the whole point: "this worker reported 96,300 input
//! tokens" and "no usage receipt exists for this worker" are different facts,
//! and a rail that prints `0` for the second one is lying in the direction that
//! makes Codewhale look cheap. Nothing here estimates, derives a token count
//! from text, or back-fills a missing receipt — values come from
//! `AgentRunUsage`, which is populated from immutable per-response route
//! audits, or they are absent.
//!
//! A finished agent keeps the numbers it finished with: rows are built from the
//! retained worker record, never recomputed from live state.

use serde::{Deserialize, Serialize};

use crate::tools::subagent::{AgentWorkerRecord, AgentWorkerStatus};

/// What a row is doing, collapsed to one glanceable state.
///
/// Deliberately coarser than `AgentWorkerStatus`: the rail needs a glyph and
/// a sort rank, not the full lifecycle. The precise status stays on the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RosterState {
    Running,
    Waiting,
    /// Settled because the parent's turn ended before this child did (#5906).
    ///
    /// Distinct from `Waiting`, which means a person can answer it. Nothing
    /// will answer a parked husk; it is continued with `resume_from` or
    /// dismissed with `cancel`.
    Parked,
    Done,
    Failed,
    Cancelled,
}

impl RosterState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Parked => "parked",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Single-width glyph. Filled = attention, hollow = at rest.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Running => "●",
            Self::Waiting => "◐",
            // Hollow, dotted: at rest but not finished.
            Self::Parked => "◌",
            Self::Done => "○",
            Self::Failed => "✗",
            Self::Cancelled => "⊘",
        }
    }

    /// Row state for one retained record.
    ///
    /// `parked_at_turn_end` is checked first and outranks the worker status:
    /// a parked child settles as `WaitingForUser` or `Interrupted` like any
    /// other, and only this flag separates it from a child that really asked
    /// (#5906).
    #[must_use]
    pub const fn from_record(record: &AgentWorkerRecord) -> Self {
        if record.parked_at_turn_end {
            return Self::Parked;
        }
        Self::from_worker(record.status)
    }

    #[must_use]
    pub const fn from_worker(status: AgentWorkerStatus) -> Self {
        match status {
            AgentWorkerStatus::Queued
            | AgentWorkerStatus::Starting
            | AgentWorkerStatus::Running
            | AgentWorkerStatus::ModelWait
            | AgentWorkerStatus::RunningTool => Self::Running,
            AgentWorkerStatus::WaitingForUser => Self::Waiting,
            AgentWorkerStatus::Completed => Self::Done,
            AgentWorkerStatus::Failed => Self::Failed,
            AgentWorkerStatus::Cancelled | AgentWorkerStatus::Interrupted => Self::Cancelled,
        }
    }
}

/// One agent's row. Every optional field means "no receipt", not "zero".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRosterRow {
    pub worker_id: String,
    /// Session name, else role, else the fleet type — whichever the user named.
    pub display_name: String,
    pub model: String,
    pub state: RosterState,
    pub status: AgentWorkerStatus,
    /// The agent's outcome, current step or last tool, in one line. `None`
    /// when the worker has not reported an event yet.
    pub activity: Option<String>,
    /// What a settled worker produced, in full (its result, or its error). The
    /// row shows a one-line headline; the focused view shows this, so the
    /// answer is never only available cut off (#6565). `None` while live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Wall time: elapsed for a live agent, final duration for a finished one.
    pub millis: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_microusd: Option<u64>,
    pub steps_taken: u32,
    /// Set when this agent was spawned by another; the parent aggregates it.
    pub parent_run_id: Option<String>,
    pub run_id: String,
}

impl AgentRosterRow {
    /// `n/m done` for a workflow parent, from its children's terminal states.
    #[must_use]
    pub fn workflow_progress(&self, rows: &[Self]) -> Option<(usize, usize)> {
        let children = rows
            .iter()
            .filter(|row| row.parent_run_id.as_deref() == Some(self.run_id.as_str()))
            .collect::<Vec<_>>();
        if children.is_empty() {
            return None;
        }
        let done = children
            .iter()
            .filter(|row| row.state.is_terminal())
            .count();
        Some((done, children.len()))
    }
}

/// Build the roster from retained worker records.
///
/// `now_ms` is passed in rather than read from the clock so the projection is a
/// pure function — the caller supplies the same instant it renders with, and
/// tests get deterministic elapsed values.
#[must_use]
pub fn build_agent_roster(records: &[AgentWorkerRecord], now_ms: u64) -> Vec<AgentRosterRow> {
    let mut rows: Vec<AgentRosterRow> = records
        .iter()
        .map(|record| row_from_record(record, now_ms))
        .collect();
    // Oldest first: the rail is a history of the session, and a list that
    // reorders itself as agents finish is unreadable while you are watching it.
    rows.sort_by(|a, b| a.worker_id.cmp(&b.worker_id));
    rows.sort_by_key(|row| creation_key(records, &row.worker_id));
    // ...except parked husks, which sink below everything still live or
    // answerable (#5906). They are the one class of row the operator is not
    // meant to scan past to find real work, and the sort is stable so the
    // history order survives inside each group.
    rows.sort_by_key(|row| row.state == RosterState::Parked);
    rows
}

fn creation_key(records: &[AgentWorkerRecord], worker_id: &str) -> u64 {
    records
        .iter()
        .find(|record| record.spec.worker_id == worker_id)
        .map_or(u64::MAX, |record| record.created_at_ms)
}

#[must_use]
pub fn row_from_record(record: &AgentWorkerRecord, now_ms: u64) -> AgentRosterRow {
    let state = RosterState::from_record(record);
    AgentRosterRow {
        worker_id: record.spec.worker_id.clone(),
        display_name: display_name(record),
        model: record.spec.model.clone(),
        state,
        status: record.status,
        activity: activity_line(record),
        outcome: settled_outcome(record).map(str::to_string),
        millis: wall_millis(record, now_ms),
        input_tokens: record.usage.input_tokens,
        output_tokens: record.usage.output_tokens,
        cost_microusd: record.usage.cost_microusd,
        steps_taken: record.steps_taken,
        parent_run_id: record.parent_run_id.clone(),
        run_id: record.spec.run_id.clone(),
    }
}

#[must_use]
pub fn display_name(record: &AgentWorkerRecord) -> String {
    record
        .spec
        .session_name
        .clone()
        .or_else(|| {
            record
                .spec
                .child_route
                .as_ref()
                .and_then(|route| route.resolved_profile_id.clone())
                .filter(|profile| !profile.trim().is_empty())
        })
        .or_else(|| record.spec.role.clone())
        .unwrap_or_else(|| record.spec.agent_type.as_str().to_string())
}

/// The agent's outcome, current step or last tool, in one line.
///
/// A settled worker leads with what it produced (#6565): a finished one shows
/// the headline of its result, and a failed or cancelled one its error, not
/// the last tool it happened to call. A live worker's preference order is
/// most-specific-first: the newest event naming a tool, then the newest event
/// carrying a message, then the worker's latest message.
#[must_use]
pub fn activity_line(record: &AgentWorkerRecord) -> Option<String> {
    if let Some(headline) = settled_outcome(record).and_then(result_headline) {
        return Some(one_line(&headline));
    }
    let from_events = record.events.iter().rev().find_map(|event| {
        event
            .tool_name
            .as_ref()
            .map(|tool| match event.step {
                Some(step) => format!("step {step} · {tool}"),
                None => tool.clone(),
            })
            .or_else(|| event.message.clone())
    });
    from_events
        .or_else(|| record.latest_message.clone())
        .or_else(|| record.result_summary.clone())
        .map(|line| one_line(&line))
}

/// What a settled worker produced, in full: its result when it finished, its
/// error (else its partial result) when it failed or was stopped. `None` while
/// it is live or parked.
#[must_use]
pub fn settled_outcome(record: &AgentWorkerRecord) -> Option<&str> {
    let outcome = match RosterState::from_record(record) {
        RosterState::Done => record.result_summary.as_deref(),
        RosterState::Failed | RosterState::Cancelled => {
            record.error.as_deref().or(record.result_summary.as_deref())
        }
        RosterState::Running | RosterState::Waiting | RosterState::Parked => None,
    };
    outcome.filter(|text| !text.trim().is_empty())
}

/// The line of an agent's result that says what it found (#6565).
///
/// A report usually opens with scaffolding: a `## Summary` heading, a rule, a
/// code fence, or the machine-readable completion envelope. Showing its first
/// line showed the scaffolding and hid the answer. This skips blank lines, the
/// `<codewhale:…>` envelope, headings, rule-only lines and fence markers, and
/// returns the first sentence of the first line of prose. A result that is
/// only headings still yields its first heading, without the `#` marks.
/// `None` when nothing is left.
///
/// The headline is not bounded here; a one-row surface bounds it with
/// [`one_line`], which marks the cut with `…`. The full result stays one Enter
/// away in the agent's focused view.
#[must_use]
pub fn result_headline(text: &str) -> Option<String> {
    let is_rule = |line: &str| {
        line.chars().filter(|c| !c.is_whitespace()).count() >= 3
            && line
                .chars()
                .all(|c| matches!(c, '-' | '*' | '_' | '=') || c.is_whitespace())
    };
    let is_fence = |line: &str| line.starts_with("```") || line.starts_with("~~~");
    let candidates = || {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("<codewhale:"))
            .filter(|line| !is_rule(line) && !is_fence(line))
    };
    let line = match candidates().find(|line| !line.starts_with('#')) {
        Some(line) => line,
        None => candidates().next()?.trim_start_matches('#').trim(),
    };
    let sentence = first_sentence(line).trim();
    (!sentence.is_empty()).then(|| sentence.to_string())
}

/// Up to and including the first sentence terminator followed by a space, so
/// `v0.10.1` or `foo.rs` inside a line does not end the sentence. CJK full
/// stops end it wherever they fall.
fn first_sentence(line: &str) -> &str {
    let mut chars = line.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let end = index + c.len_utf8();
        if matches!(c, '。' | '！' | '？') {
            return &line[..end];
        }
        if matches!(c, '.' | '!' | '?')
            && chars.peek().is_some_and(|(_, next)| next.is_whitespace())
        {
            return &line[..end];
        }
    }
    line
}

/// Collapse to a single line and bound it. Rail rows are one row.
#[must_use]
pub fn one_line(text: &str) -> String {
    const MAX_CHARS: usize = 72;
    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= MAX_CHARS {
        return flattened;
    }
    let kept = flattened.chars().take(MAX_CHARS - 1).collect::<String>();
    format!("{kept}…")
}

/// Elapsed for a live agent, final duration for a finished one.
///
/// `None` when the worker has no start timestamp — a queued worker has not
/// started, and reporting `0s` would imply it had.
#[must_use]
pub fn wall_millis(record: &AgentWorkerRecord, now_ms: u64) -> Option<u64> {
    let started = record.started_at_ms?;
    let end = record.completed_at_ms.unwrap_or(now_ms);
    Some(end.saturating_sub(started))
}

/// `3m 29s`, `12s`, `450ms`. Compact because it shares a row.
#[must_use]
pub fn format_duration(millis: u64) -> String {
    if millis < 1_000 {
        return format!("{millis}ms");
    }
    let seconds = millis / 1_000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    let rest = seconds % 60;
    if minutes < 60 {
        return format!("{minutes}m {rest}s");
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

/// `96.3k`, `1.2M`, `812`. Never rounds a real count to zero.
#[must_use]
pub fn format_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }
    if tokens < 1_000_000 {
        return format!("{:.1}k", tokens as f64 / 1_000.0);
    }
    format!("{:.1}M", tokens as f64 / 1_000_000.0)
}

/// Usage totals across the roster, for a footer line.
///
/// Returns `None` for a field when *no* row reported it — summing absent
/// receipts into `0` would restate the same lie the per-row rule forbids.
#[must_use]
pub fn roster_totals(rows: &[AgentRosterRow]) -> (Option<u64>, Option<u64>) {
    fn total(values: impl Iterator<Item = Option<u64>>) -> Option<u64> {
        let reported: Vec<u64> = values.flatten().collect();
        (!reported.is_empty()).then(|| reported.into_iter().fold(0u64, u64::saturating_add))
    }
    (
        total(rows.iter().map(|row| row.input_tokens)),
        total(rows.iter().map(|row| row.output_tokens)),
    )
}

/// True when at least one row reported a usage receipt. Callers use this to
/// label the totals line honestly ("partial receipts") instead of implying the
/// number covers every agent.
#[must_use]
pub fn all_rows_have_usage(rows: &[AgentRosterRow]) -> bool {
    !rows.is_empty()
        && rows
            .iter()
            .all(|row| row.input_tokens.is_some() || row.output_tokens.is_some())
}
