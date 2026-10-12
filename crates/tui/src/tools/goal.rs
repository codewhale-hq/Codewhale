//! Goal tools for the model-visible LLM-as-judge loop.
//!
//! The TUI already has a `/goal` command and passes its objective into the
//! engine prompt. This module keeps the runtime slice separate: a small
//! session-scoped state object plus tools the model can use to inspect and
//! close out that state.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, required_str,
};

/// Shared reference to the current runtime goal.
pub type SharedGoalState = Arc<Mutex<GoalState>>;

/// Create an empty shared goal state.
#[must_use]
pub fn new_shared_goal_state() -> SharedGoalState {
    Arc::new(Mutex::new(GoalState::default()))
}

/// Create shared state seeded from the host goal surface with an explicit status.
#[must_use]
pub fn new_shared_goal_state_from_host_status(
    objective: Option<String>,
    token_budget: Option<u32>,
    status: GoalStatus,
) -> SharedGoalState {
    let mut state = GoalState::default();
    state.sync_from_host_status(objective.as_deref(), token_budget, status);
    Arc::new(Mutex::new(state))
}

/// Restore the complete durable history; loading is not an explicit resume.
#[must_use]
pub fn new_shared_goal_state_from_snapshot(snapshot: &GoalSnapshot) -> SharedGoalState {
    Arc::new(Mutex::new(GoalState::from_snapshot(snapshot)))
}

/// Runtime status for a goal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GoalStatus {
    #[default]
    Active,
    Paused,
    Complete,
    Blocked,
}

impl GoalStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Complete => "complete",
            Self::Blocked => "blocked",
        }
    }
}

pub use codewhale_protocol::GoalPauseReason;

/// Whether a goal review is allowed to decide the judged contract.
///
/// Critical reviews fail closed and may satisfy the completion gate. Advisory
/// reviews are append-only context: malformed or negative advice must never
/// pause, block, or complete the goal.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalReviewRole {
    #[default]
    Critical,
    Advisory,
}

/// Best-effort review context kept separate from the judged completion
/// contract. Notes are append-only for the lifetime of one objective.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GoalAdvisoryNote {
    pub summary: String,
}

/// The model's own reported progress for the active goal: a coarse percent
/// plus what is happening now and what comes next. Runtime-only — the durable
/// record deliberately keeps no volatile progress projection. The percent is
/// the model's estimate, rendered as reported progress, never as a verified
/// fraction of the work.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GoalProgressReport {
    pub percent: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// Session-local goal state. `Instant` stays runtime-only; snapshots expose
/// elapsed seconds so tool output remains serializable and stable.
#[derive(Debug, Clone, Default)]
pub struct GoalState {
    goal_id: Option<String>,
    objective: Option<String>,
    token_budget: Option<u32>,
    status: Option<GoalStatus>,
    tokens_used: u64,
    time_used_seconds: u64,
    continuation_count: u32,
    started_at: Option<Instant>,
    finished_at: Option<Instant>,
    evidence: Option<String>,
    blocker: Option<String>,
    pause_reason: Option<GoalPauseReason>,
    completion_verification: Option<GoalCompletionVerification>,
    advisories: Vec<GoalAdvisoryNote>,
    last_gap_fingerprint: Option<String>,
    repeated_gap_count: u32,
    /// The continuation pass the repeated-gap counter last advanced on.
    /// The bound is "equivalent gaps on consecutive PASSES", so a verifier
    /// reporting the same gap several times inside one turn must not trip it
    /// before any continuation has happened.
    last_gap_pass: Option<u32>,
    /// Latest reported progress, kept out of the stall accounting entirely.
    progress: Option<GoalProgressReport>,
    /// The current blocker was set by the runtime (a continuation turn that
    /// failed, timed out or never started), not reported by the model or the
    /// user. Such a stop is not a judgement about the work, so the user's next
    /// message resumes the goal (see [`Self::resume_after_runtime_block`]).
    /// Known limitation: session-local, like the rest of this state's
    /// lifecycle detail; a restored Blocked goal needs `/goal resume`.
    runtime_blocked: bool,
}

impl GoalState {
    #[must_use]
    pub fn objective(&self) -> Option<&str> {
        self.objective.as_deref()
    }

    #[must_use]
    pub fn token_budget(&self) -> Option<u32> {
        self.token_budget
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.objective.is_some() && self.status == Some(GoalStatus::Active)
    }

    pub fn sync_from_host_status(
        &mut self,
        objective: Option<&str>,
        token_budget: Option<u32>,
        status: GoalStatus,
    ) {
        self.sync_from_host_status_with_reason(objective, token_budget, status, None);
    }

    /// [`Self::sync_from_host_status`], for a host that knows **why** the goal
    /// is paused. Without a reason a `Paused` projection is read as a user
    /// pause, which erases a hand-back and leaves the goal un-resumable by the
    /// user's next message.
    pub fn sync_from_host_status_with_reason(
        &mut self,
        objective: Option<&str>,
        token_budget: Option<u32>,
        status: GoalStatus,
        pause_reason: Option<GoalPauseReason>,
    ) {
        let objective = objective.map(str::trim).filter(|value| !value.is_empty());
        match objective {
            Some(objective) => {
                let changed = self.objective.as_deref() != Some(objective);
                let status_changed = self.status != Some(status);
                let resumed = !changed
                    && status == GoalStatus::Active
                    && self
                        .status
                        .is_some_and(|previous| previous != GoalStatus::Active);
                if changed {
                    self.goal_id = Some(uuid::Uuid::new_v4().to_string());
                    self.objective = Some(objective.to_string());
                    self.token_budget = token_budget;
                    self.tokens_used = 0;
                    self.time_used_seconds = 0;
                    self.continuation_count = 0;
                    self.started_at = Some(Instant::now());
                    self.evidence = None;
                    self.blocker = None;
                    self.pause_reason = None;
                    self.completion_verification = None;
                    self.advisories.clear();
                    self.last_gap_fingerprint = None;
                    self.repeated_gap_count = 0;
                    self.last_gap_pass = None;
                    self.progress = None;
                } else if self.token_budget != token_budget {
                    self.token_budget = token_budget;
                }

                if resumed {
                    self.goal_id = Some(uuid::Uuid::new_v4().to_string());
                    self.evidence = None;
                    self.blocker = None;
                    self.pause_reason = None;
                    self.completion_verification = None;
                    self.last_gap_fingerprint = None;
                    self.repeated_gap_count = 0;
                    self.last_gap_pass = None;
                    self.progress = None;
                }

                if changed || status_changed || self.status.is_none() {
                    self.status = Some(status);
                    self.pause_reason = if status == GoalStatus::Paused {
                        Some(pause_reason.unwrap_or(GoalPauseReason::User))
                    } else {
                        None
                    };
                    self.finished_at = if status == GoalStatus::Active {
                        None
                    } else {
                        Some(Instant::now())
                    };
                }
            }
            None => self.clear(),
        }
    }

    pub fn create(
        &mut self,
        objective: String,
        token_budget: Option<u32>,
    ) -> Result<(), &'static str> {
        if self.objective.is_some() && self.status != Some(GoalStatus::Complete) {
            return Err(
                "An unfinished goal already exists. Complete or clear it before creating another.",
            );
        }
        self.goal_id = Some(uuid::Uuid::new_v4().to_string());
        self.objective = Some(objective);
        self.token_budget = token_budget;
        self.status = Some(GoalStatus::Active);
        self.tokens_used = 0;
        self.time_used_seconds = 0;
        self.continuation_count = 0;
        self.started_at = Some(Instant::now());
        self.finished_at = None;
        self.evidence = None;
        self.blocker = None;
        self.pause_reason = None;
        self.completion_verification = None;
        self.advisories.clear();
        self.last_gap_fingerprint = None;
        self.repeated_gap_count = 0;
        self.last_gap_pass = None;
        self.progress = None;
        Ok(())
    }

    /// Restore goal state from a persisted runtime goal, keeping the
    /// accumulated usage and continuation counters.
    ///
    /// Unlike [`Self::sync_from_host_status`], which resets the counters
    /// whenever the objective changes, this constructor treats the persisted
    /// values as the authoritative history: the durable store owns them and
    /// the engine is rehydrating, not re-declaring, the goal. Evidence,
    /// blockers, and review notes are runtime-only and start empty; the
    /// durable loop re-derives them on the next pass.
    ///
    #[must_use]
    pub fn from_persisted(
        objective: &str,
        token_budget: Option<u32>,
        status: GoalStatus,
        pause_reason: Option<GoalPauseReason>,
        tokens_used: u64,
        time_used_seconds: u64,
        continuation_count: u32,
    ) -> Self {
        Self {
            goal_id: None,
            objective: Some(objective.to_string()),
            token_budget,
            status: Some(status),
            tokens_used,
            time_used_seconds,
            continuation_count,
            started_at: Some(Instant::now()),
            finished_at: (status != GoalStatus::Active).then(Instant::now),
            evidence: None,
            blocker: None,
            pause_reason,
            completion_verification: None,
            advisories: Vec::new(),
            last_gap_fingerprint: None,
            repeated_gap_count: 0,
            last_gap_pass: None,
            progress: None,
            // The origin of a persisted blocker is not recorded; treat it as
            // reported so only an explicit resume clears it.
            runtime_blocked: false,
        }
    }

    /// Keep the pre-pause review window on ordinary load. Invalid in-memory
    /// input is held paused; durable stores reject it before this constructor.
    #[must_use]
    pub fn from_snapshot(snapshot: &GoalSnapshot) -> Self {
        let Some(objective) = snapshot.objective.as_deref() else {
            return Self::default();
        };
        let status = match snapshot.status.as_str() {
            "active" => GoalStatus::Active,
            "complete" => GoalStatus::Complete,
            "blocked" => GoalStatus::Blocked,
            _ => GoalStatus::Paused,
        };
        let mut state = Self::from_persisted(
            objective,
            snapshot.token_budget,
            status,
            snapshot.pause_reason,
            snapshot.tokens_used,
            snapshot.time_used_seconds,
            snapshot.continuation_count,
        );
        state.goal_id.clone_from(&snapshot.goal_id);
        state
            .last_gap_fingerprint
            .clone_from(&snapshot.last_gap_fingerprint);
        state.repeated_gap_count = snapshot.repeated_gap_count;
        state.last_gap_pass = snapshot.last_gap_pass;
        state.progress = snapshot.progress.clone();
        let now = Instant::now();
        state.started_at = now
            .checked_sub(std::time::Duration::from_secs(
                snapshot
                    .elapsed_seconds
                    .unwrap_or(snapshot.time_used_seconds),
            ))
            .or(Some(now));
        let stall_window_exhausted = state.status == Some(GoalStatus::Active)
            && state.repeated_gap_count >= crate::goal_loop::MAX_REPEATED_GAP_PASSES;
        if let Err(error) = snapshot.validate_stall_state() {
            tracing::warn!("holding invalid restored goal paused: {error}");
            state.status = Some(GoalStatus::Paused);
            state.pause_reason = Some(GoalPauseReason::NoProgress);
            state.finished_at = Some(now);
        } else if stall_window_exhausted {
            // The engine pauses NoProgress in the same mutation that fills
            // the stall window, so a restored Active goal at the ceiling is
            // corrupt; hold it paused rather than re-arming spent passes.
            tracing::warn!("holding exhausted-stall-window restored goal paused");
            state.status = Some(GoalStatus::Paused);
            state.pause_reason = Some(GoalPauseReason::NoProgress);
            state.finished_at = Some(now);
        }
        state
    }

    /// An accepted user resume is a new control revision, even when already
    /// active. Cached loads never call this path.
    pub fn resume(&mut self, goal_id: Option<String>) {
        let objective = self.objective.clone();
        self.sync_from_host_status(objective.as_deref(), self.token_budget, GoalStatus::Active);
        if self.objective.is_some() {
            self.goal_id = Some(goal_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()));
            self.last_gap_fingerprint = None;
            self.repeated_gap_count = 0;
            self.last_gap_pass = None;
            self.progress = None;
        }
    }

    /// A new explicit declaration replaces the old revision, including when
    /// the user repeats the same objective text.
    pub fn replace(&mut self, objective: &str, token_budget: Option<u32>, goal_id: Option<String>) {
        self.clear();
        self.sync_from_host_status(Some(objective), token_budget, GoalStatus::Active);
        if let Some(goal_id) = goal_id {
            self.goal_id = Some(goal_id);
        }
    }

    pub fn record_usage(&mut self, token_delta: u64, time_delta_seconds: u64, since: Instant) {
        if self.is_active()
            || (self.objective.is_some()
                && self.finished_at.is_some_and(|finished| finished >= since))
        {
            self.tokens_used = self.tokens_used.saturating_add(token_delta);
            self.time_used_seconds = self.time_used_seconds.saturating_add(time_delta_seconds);
        }
    }

    pub fn record_continuation(&mut self) {
        if self.is_active() {
            self.continuation_count = self.continuation_count.saturating_add(1);
        }
    }

    pub fn mark_complete(
        &mut self,
        evidence: String,
        mut verification: GoalCompletionVerification,
    ) -> Result<(), &'static str> {
        if self.objective.is_none() {
            return Err("No active goal exists to complete.");
        }
        if self.status == Some(GoalStatus::Complete) || self.completion_verification.is_some() {
            return Err("The judged completion contract is already sealed and cannot be replaced.");
        }
        if verification.role != GoalReviewRole::Critical {
            return Err("An advisory review cannot complete the judged goal contract.");
        }
        verification.contract_fingerprint = completion_contract_fingerprint(
            self.objective.as_deref().unwrap_or_default(),
            &verification,
        );
        self.status = Some(GoalStatus::Complete);
        self.finished_at = Some(Instant::now());
        self.evidence = Some(evidence);
        self.blocker = None;
        self.pause_reason = None;
        self.completion_verification = Some(verification);
        Ok(())
    }

    /// Replace the reported progress projection. This never touches the
    /// stall window or lifecycle state; it is display context only.
    pub fn record_progress(&mut self, progress: GoalProgressReport) {
        if self.is_active() {
            self.progress = Some(progress);
        }
    }

    pub fn record_advisory(&mut self, summary: String) -> Result<(), &'static str> {
        if !self.is_active() {
            return Err("Advisory notes require an active goal.");
        }
        const MAX_ADVISORY_NOTES: usize = 16;
        if self.advisories.len() == MAX_ADVISORY_NOTES {
            self.advisories.remove(0);
        }
        self.advisories.push(GoalAdvisoryNote { summary });
        Ok(())
    }

    pub fn record_not_achieved(
        &mut self,
        verification: GoalProgressVerification,
    ) -> Result<(), &'static str> {
        if !self.is_active() {
            return Err("Verifier progress requires an active goal.");
        }
        if verification.role == GoalReviewRole::Advisory {
            return self
                .record_advisory(format!("{}: {}", verification.check, verification.summary));
        }

        let fingerprint = gap_fingerprint(&verification.gaps)
            .ok_or("Critical not-achieved verification requires at least one concrete gap.")?;
        // Advance at most once per continuation pass. `record_not_achieved`
        // runs per `update_goal` tool call, so counting calls would let a
        // verifier that reports one gap three times in a single turn pause the
        // goal before a single continuation had been spent — stopping valid
        // work rather than a stall.
        let same_gap = self.last_gap_fingerprint.as_deref() == Some(&fingerprint);
        let already_counted_this_pass = self.last_gap_pass == Some(self.continuation_count);
        self.repeated_gap_count = if !same_gap {
            1
        } else if already_counted_this_pass {
            self.repeated_gap_count
        } else {
            self.repeated_gap_count.saturating_add(1)
        };
        self.last_gap_pass = Some(self.continuation_count);
        self.last_gap_fingerprint = Some(fingerprint);

        // The stall bound the continuation prompt promises. Pausing *is* the
        // stop: both continuation dispatchers refuse to re-dispatch a goal
        // whose snapshot is not "active", and the runtime host mirrors a
        // non-limit pause into the durable `ThreadGoalStatus::Paused`, so this
        // needs no second gate in `decide_continuation` and survives a restart
        // until someone explicitly resumes.
        if self.repeated_gap_count >= crate::goal_loop::MAX_REPEATED_GAP_PASSES {
            tracing::warn!(
                repeated_gap_count = self.repeated_gap_count,
                max_repeated_gap_passes = crate::goal_loop::MAX_REPEATED_GAP_PASSES,
                "goal stall pause: critical verifier reported an equivalent gap set on \
                 consecutive passes; pausing for inspection instead of spending further"
            );
            self.mark_paused(GoalPauseReason::NoProgress)?;
        }

        Ok(())
    }

    /// Block on a runtime stop rather than a reported blocker; see
    /// [`Self::runtime_blocked`].
    pub fn mark_runtime_blocked(&mut self, blocker: String) -> Result<(), &'static str> {
        self.mark_blocked(blocker)?;
        self.runtime_blocked = true;
        Ok(())
    }

    /// Resume a goal whose only blocker was a runtime stop, as a new control
    /// revision. Returns false, changing nothing, for any other state: a
    /// reported blocker stays until an explicit resume.
    pub fn resume_after_runtime_block(&mut self) -> bool {
        if !(self.runtime_blocked && self.status == Some(GoalStatus::Blocked)) {
            return false;
        }
        self.resume(None);
        self.runtime_blocked = false;
        true
    }

    /// Resume a goal the model handed back at a milestone, as a new control
    /// revision. A yield is a hand-back rather than a judgement about the work,
    /// so the user's next message continues it.
    ///
    /// Returns false, changing nothing, for any other state: a pause the user
    /// asked for, and a pause the loop imposed on itself, both stay until an
    /// explicit resume.
    pub fn resume_after_yield(&mut self) -> bool {
        if !(self.status == Some(GoalStatus::Paused)
            && self.pause_reason == Some(GoalPauseReason::Yielded))
        {
            return false;
        }
        self.resume(None);
        true
    }

    /// Hand the goal back to the user at a milestone.
    ///
    /// Only an **active** goal can be handed back. A goal that is already paused
    /// or blocked carries a reason someone else set — a user pause, a budget
    /// stop, a reported blocker — and converting it would make it resumable by
    /// the next message, which is exactly the contract this state exists to keep
    /// narrow.
    pub fn mark_yielded(&mut self) -> Result<(), &'static str> {
        if self.objective.is_none() {
            return Err("No active goal exists to hand back.");
        }
        if self.status != Some(GoalStatus::Active) {
            return Err(
                "Only an active goal can be handed back; this one is already paused or \
                 blocked for a reason someone else set.",
            );
        }
        self.mark_paused(GoalPauseReason::Yielded)
    }

    /// Whether a judged completion has sealed this goal. A sealed goal is
    /// terminal: blocking or pausing it would overwrite the verified
    /// completion, so only an explicit resume or a new goal moves it on.
    fn completion_sealed(&self) -> bool {
        self.status == Some(GoalStatus::Complete) || self.completion_verification.is_some()
    }

    pub fn mark_blocked(&mut self, blocker: String) -> Result<(), &'static str> {
        if self.objective.is_none() {
            return Err("No active goal exists to block.");
        }
        if self.completion_sealed() {
            return Err(
                "The goal is already complete with a judged verification; it cannot be blocked.",
            );
        }
        self.runtime_blocked = false;
        self.status = Some(GoalStatus::Blocked);
        self.finished_at = Some(Instant::now());
        self.blocker = Some(blocker);
        self.evidence = None;
        self.pause_reason = None;
        self.completion_verification = None;
        Ok(())
    }

    pub fn mark_paused(&mut self, reason: GoalPauseReason) -> Result<(), &'static str> {
        if self.objective.is_none() {
            return Err("No active goal exists to pause.");
        }
        if self.completion_sealed() {
            return Err(
                "The goal is already complete with a judged verification; it cannot be paused.",
            );
        }
        self.status = Some(GoalStatus::Paused);
        self.finished_at = Some(Instant::now());
        self.pause_reason = Some(reason);
        self.evidence = None;
        self.blocker = None;
        self.completion_verification = None;
        Ok(())
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    #[must_use]
    pub fn snapshot(&self) -> GoalSnapshot {
        // Once the goal is terminal, freeze elapsed at the finish time so the
        // sidebar timer (and any tool snapshot) stops growing after completion.
        let elapsed_seconds = match (self.started_at, self.finished_at) {
            (Some(started), Some(finished)) => {
                Some(finished.saturating_duration_since(started).as_secs())
            }
            (Some(started), None) => Some(started.elapsed().as_secs()),
            (None, _) => None,
        };
        GoalSnapshot {
            goal_id: self.goal_id.clone(),
            objective: self.objective.clone(),
            status: self
                .status
                .map(GoalStatus::as_str)
                .unwrap_or("none")
                .to_string(),
            token_budget: self.token_budget,
            tokens_used: self.tokens_used,
            time_used_seconds: self.time_used_seconds,
            continuation_count: self.continuation_count,
            elapsed_seconds,
            evidence: self.evidence.clone(),
            blocker: self.blocker.clone(),
            pause_reason: self.pause_reason,
            completion_verification: self.completion_verification.clone(),
            advisories: self.advisories.clone(),
            last_gap_fingerprint: self.last_gap_fingerprint.clone(),
            repeated_gap_count: self.repeated_gap_count,
            last_gap_pass: self.last_gap_pass,
            progress: self.progress.clone(),
        }
    }
}

/// Serializable tool output and prompt input for the current goal.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct GoalSnapshot {
    pub goal_id: Option<String>,
    pub objective: Option<String>,
    pub status: String,
    pub token_budget: Option<u32>,
    pub tokens_used: u64,
    pub time_used_seconds: u64,
    pub continuation_count: u32,
    pub elapsed_seconds: Option<u64>,
    pub evidence: Option<String>,
    pub blocker: Option<String>,
    pub pause_reason: Option<GoalPauseReason>,
    pub completion_verification: Option<GoalCompletionVerification>,
    pub advisories: Vec<GoalAdvisoryNote>,
    pub last_gap_fingerprint: Option<String>,
    pub repeated_gap_count: u32,
    pub last_gap_pass: Option<u32>,
    /// Latest reported progress. Skipped when absent so tool output and the
    /// continuation prompt stay stable for goals that never report one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<GoalProgressReport>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoalCompletionVerification {
    pub status: String,
    pub check: String,
    pub summary: String,
    #[serde(default)]
    pub role: GoalReviewRole,
    #[serde(default)]
    pub contract_fingerprint: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct GoalProgressVerification {
    pub status: String,
    pub check: String,
    pub summary: String,
    #[serde(default)]
    pub role: GoalReviewRole,
    #[serde(default)]
    pub gaps: Vec<String>,
}

fn completion_contract_fingerprint(
    objective: &str,
    verification: &GoalCompletionVerification,
) -> String {
    let mut hasher = Sha256::new();
    for field in [
        objective.trim(),
        verification.status.trim(),
        verification.check.trim(),
        verification.summary.trim(),
    ] {
        hasher.update(field.as_bytes());
        hasher.update([0]);
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn gap_fingerprint(gaps: &[String]) -> Option<String> {
    let mut normalized = gaps
        .iter()
        .map(|gap| {
            gap.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        })
        .filter(|gap| !gap.is_empty())
        .collect::<Vec<_>>();
    normalized.sort_unstable();
    normalized.dedup();
    if normalized.is_empty() {
        return None;
    }

    let mut hasher = Sha256::new();
    hasher.update(b"codewhale-goal-gaps-v1\0");
    for gap in normalized {
        hasher.update(gap.as_bytes());
        hasher.update([0]);
    }
    Some(
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

impl GoalSnapshot {
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.objective.is_some() && self.status == GoalStatus::Active.as_str()
    }

    pub fn validate_stall_state(&self) -> Result<(), &'static str> {
        codewhale_protocol::validate_goal_stall_state(
            self.last_gap_fingerprint.as_deref(),
            self.repeated_gap_count,
            self.last_gap_pass,
            self.continuation_count,
        )
    }

    #[must_use]
    pub fn from_thread_goal(goal: &codewhale_protocol::ThreadGoal) -> Self {
        let (status, pause_reason) =
            thread_goal_status_projection(goal.status.clone(), goal.pause_reason);
        Self {
            goal_id: Some(goal.goal_id.clone()),
            objective: Some(goal.objective.clone()),
            status: status.as_str().to_string(),
            token_budget: goal
                .token_budget
                .and_then(|value| u32::try_from(value.max(0)).ok()),
            tokens_used: u64::try_from(goal.tokens_used.max(0)).unwrap_or(u64::MAX),
            time_used_seconds: u64::try_from(goal.time_used_seconds.max(0)).unwrap_or(u64::MAX),
            continuation_count: u32::try_from(goal.continuation_count.max(0)).unwrap_or(u32::MAX),
            elapsed_seconds: None,
            evidence: None,
            blocker: None,
            pause_reason,
            completion_verification: None,
            advisories: Vec::new(),
            last_gap_fingerprint: goal.last_gap_fingerprint.clone(),
            repeated_gap_count: goal.repeated_gap_count,
            last_gap_pass: goal.last_gap_pass,
            progress: None,
        }
    }
}

#[must_use]
pub fn thread_goal_status_projection(
    status: codewhale_protocol::ThreadGoalStatus,
    pause_reason: Option<GoalPauseReason>,
) -> (GoalStatus, Option<GoalPauseReason>) {
    match status {
        codewhale_protocol::ThreadGoalStatus::Active => (GoalStatus::Active, None),
        // The durable status alone cannot say why a goal is paused; the record's
        // own reason can, and dropping it turns a hand-back into a user pause.
        codewhale_protocol::ThreadGoalStatus::Paused => (
            GoalStatus::Paused,
            pause_reason.or(Some(GoalPauseReason::User)),
        ),
        codewhale_protocol::ThreadGoalStatus::Complete => (GoalStatus::Complete, None),
        codewhale_protocol::ThreadGoalStatus::Blocked => (GoalStatus::Blocked, None),
        codewhale_protocol::ThreadGoalStatus::UsageLimited => {
            (GoalStatus::Paused, Some(GoalPauseReason::UsageLimit))
        }
        codewhale_protocol::ThreadGoalStatus::BudgetLimited => {
            (GoalStatus::Paused, Some(GoalPauseReason::BudgetLimit))
        }
    }
}

/// Render the continuation prompt injected when a goal is still active after a
/// turn. This shows progress and lets the circuit breaker remain an
/// implementation detail rather than encouraging the model to spend the cap.
#[must_use]
pub fn render_continuation_prompt(snapshot: &GoalSnapshot, continuation_index: u32) -> String {
    let goal_json = serde_json::to_string_pretty(snapshot).unwrap_or_else(|_| "{}".to_string());
    format!(
        "{}\n\n## Active Goal State\n\n```json\n{}\n```\n\nContinuation pass #{}.\nIf a critical verifier finds remaining work, call `update_goal` with `status: \"not_achieved\"` and its concrete `verification.gaps`; {} equivalent gap sets in a row pause this goal (`no progress`) for inspection instead of spending indefinitely, so report what actually still fails rather than restating the previous pass. If the goal is complete, first run or cite a concrete verifier/check when one applies, then call `update_goal` with `status: \"complete\"`, concrete evidence, and `verification: {{\"status\":\"passed\",\"check\":\"...\",\"summary\":\"...\"}}`. For non-verifiable work (docs, research, writing), use `verification: {{\"status\":\"not_applicable\",\"check\":\"...\",\"summary\":\"...\"}}` with a clear rationale instead of fabricating a verifier receipt. If it is blocked, call `update_goal` with `status: \"blocked\"` and the blocker. Otherwise continue making progress toward the objective.",
        crate::prompts::GOAL_CONTINUATION_PROMPT.trim(),
        goal_json,
        continuation_index,
        crate::goal_loop::MAX_REPEATED_GAP_PASSES,
    )
}

/// Render the reported-progress bar used by the transcript receipt and the
/// metrics line: eight cells, filled in proportion to the percent. The bar
/// visualizes a model-reported estimate; it is not a verified fraction.
#[must_use]
pub fn goal_progress_bar(percent: u8) -> String {
    const CELLS: usize = 8;
    let filled = (usize::from(percent.min(100)) * CELLS + 50) / 100;
    let mut bar = String::with_capacity(CELLS * 3);
    bar.push_str(&"▓".repeat(filled));
    bar.push_str(&"░".repeat(CELLS - filled));
    bar
}

fn lock_goal_state(
    state: &SharedGoalState,
) -> Result<std::sync::MutexGuard<'_, GoalState>, ToolError> {
    state
        .lock()
        .map_err(|_| ToolError::execution_failed("goal state lock poisoned"))
}

fn parse_token_budget(input: &Value) -> Result<Option<u32>, ToolError> {
    let Some(raw) = input.get("token_budget") else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let Some(value) = raw.as_u64() else {
        return Err(ToolError::invalid_input(
            "token_budget must be a non-negative integer",
        ));
    };
    u32::try_from(value)
        .map(Some)
        .map_err(|_| ToolError::invalid_input("token_budget is too large"))
}

fn parse_completion_verification(input: &Value) -> Result<GoalCompletionVerification, ToolError> {
    let Some(raw) = input.get("verification") else {
        return Err(ToolError::invalid_input(
            "verification is required when status is complete; run a verifier/check and pass verification: {status, check, summary}",
        ));
    };
    let verification: GoalCompletionVerification = serde_json::from_value(raw.clone())
        .map_err(|err| ToolError::invalid_input(format!("invalid verification: {err}")))?;
    let status = verification.status.trim();
    let normalized_status = match status {
        "passed" | "not_applicable" => status,
        other => {
            return Err(ToolError::invalid_input(format!(
                "verification.status must be 'passed' or 'not_applicable' before update_goal can mark a goal complete; got '{other}'"
            )));
        }
    };
    if verification.check.trim().is_empty() {
        return Err(ToolError::invalid_input("verification.check is required"));
    }
    if verification.summary.trim().is_empty() {
        return Err(ToolError::invalid_input("verification.summary is required"));
    }
    Ok(GoalCompletionVerification {
        status: normalized_status.to_string(),
        check: verification.check.trim().to_string(),
        summary: verification.summary.trim().to_string(),
        role: verification.role,
        contract_fingerprint: String::new(),
    })
}

fn parse_progress_verification(input: &Value) -> Result<GoalProgressVerification, ToolError> {
    let Some(raw) = input.get("verification") else {
        return Err(ToolError::invalid_input(
            "verification is required when status is not_achieved",
        ));
    };
    let mut verification: GoalProgressVerification = serde_json::from_value(raw.clone())
        .map_err(|err| ToolError::invalid_input(format!("invalid verification: {err}")))?;
    if verification.status.trim() != "not_achieved" {
        return Err(ToolError::invalid_input(
            "verification.status must be 'not_achieved' for progress review",
        ));
    }
    verification.check = verification.check.trim().to_string();
    verification.summary = verification.summary.trim().to_string();
    if verification.check.is_empty() {
        return Err(ToolError::invalid_input("verification.check is required"));
    }
    if verification.summary.is_empty() {
        return Err(ToolError::invalid_input("verification.summary is required"));
    }
    Ok(verification)
}

fn parse_progress_report(input: &Value) -> Result<Option<GoalProgressReport>, ToolError> {
    let Some(raw) = input.get("progress") else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let percent = raw.get("percent").and_then(Value::as_u64).ok_or_else(|| {
        ToolError::invalid_input("progress.percent must be an integer from 0 to 100")
    })?;
    let percent = u8::try_from(percent)
        .ok()
        .filter(|percent| *percent <= 100)
        .ok_or_else(|| {
            ToolError::invalid_input("progress.percent must be an integer from 0 to 100")
        })?;
    let note = |key: &str| -> Option<String> {
        raw.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.chars().take(160).collect())
    };
    Ok(Some(GoalProgressReport {
        percent,
        now: note("now"),
        next: note("next"),
    }))
}

fn json_result(snapshot: &GoalSnapshot) -> Result<ToolResult, ToolError> {
    ToolResult::json(snapshot).map_err(|err| ToolError::execution_failed(err.to_string()))
}

fn require_root_goal_mutation(context: &ToolContext) -> Result<(), ToolError> {
    if context.owner_agent_id.is_some() {
        return Err(ToolError::invalid_input(
            "Goal lifecycle mutation is root-agent only; sub-agents may inspect the parent goal with get_goal.",
        ));
    }
    Ok(())
}

pub struct CreateGoalTool {
    goal_state: SharedGoalState,
}

impl CreateGoalTool {
    #[must_use]
    pub fn new(goal_state: SharedGoalState) -> Self {
        Self { goal_state }
    }
}

#[async_trait]
impl ToolSpec for CreateGoalTool {
    fn name(&self) -> &'static str {
        "create_goal"
    }

    fn description(&self) -> &'static str {
        "Create the session's one persistent goal: a completion objective Codewhale keeps working toward across turns until it is verified complete, blocked, or the user stops it. A goal is for a durable objective that outlasts one turn: a multi-step outcome the user wants continued and verified. A question, a greeting, or a one-shot edit completes as an ordinary turn. When the user explicitly asks to use `/goal` or to make something the goal, `create_goal` is what records it; acknowledging it in prose records nothing. The objective is the user's full objective, not a shortened one-turn version. token_budget carries a budget the user stated; with none stated it stays unset. Creating a goal shows the user a one-line receipt (they can /goal pause or /goal clear), so it needs no separate confirmation. Only one unfinished goal exists at a time: an existing one is completed or cleared before another is created."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "objective": {
                    "type": "string",
                    "description": "The full objective to pursue. Keep the complete user goal, not a shortened one-turn version."
                },
                "token_budget": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional soft token budget for the goal."
                }
            },
            "required": ["objective"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        Vec::new()
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        require_root_goal_mutation(context)?;
        let objective = required_str(&input, "objective")?.trim().to_string();
        if objective.is_empty() {
            return Err(ToolError::invalid_input("objective cannot be empty"));
        }
        let token_budget = parse_token_budget(&input)?;
        let snapshot = {
            let mut state = lock_goal_state(&self.goal_state)?;
            state
                .create(objective, token_budget)
                .map_err(ToolError::invalid_input)?;
            state.snapshot()
        };
        json_result(&snapshot)
    }
}

pub struct GetGoalTool {
    goal_state: SharedGoalState,
}

impl GetGoalTool {
    #[must_use]
    pub fn new(goal_state: SharedGoalState) -> Self {
        Self { goal_state }
    }
}

#[async_trait]
impl ToolSpec for GetGoalTool {
    fn name(&self) -> &'static str {
        "get_goal"
    }

    fn description(&self) -> &'static str {
        "Inspect the current runtime goal state, including objective, status, token budget, elapsed time, evidence, and blocker."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        _input: Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let snapshot = {
            let state = lock_goal_state(&self.goal_state)?;
            state.snapshot()
        };
        json_result(&snapshot)
    }
}

pub struct UpdateGoalTool {
    goal_state: SharedGoalState,
}

impl UpdateGoalTool {
    #[must_use]
    pub fn new(goal_state: SharedGoalState) -> Self {
        Self { goal_state }
    }
}

#[async_trait]
impl ToolSpec for UpdateGoalTool {
    fn name(&self) -> &'static str {
        "update_goal"
    }

    fn description(&self) -> &'static str {
        "Update the runtime goal completion gate by calling this tool; a prose status in your answer does not change the goal or stop continuation. Critical verification may seal one immutable completion contract. Advisory review is append-only context and never completes, blocks, or pauses the goal. Mark blocked when progress requires user input. Mark yield when you finished a stage and the next step needs the user's decision: the goal stays unfinished, the turn ends, and their next message resumes it."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["complete", "blocked", "not_achieved", "advisory", "yield"],
                    "description": "Use complete only when a critical verifier proves the goal; not_achieved to record verifier gaps; blocked when meaningful progress cannot continue; yield when a stage is finished and the next step is the user's call; advisory to append best-effort context without changing lifecycle state."
                },
                "evidence": {
                    "type": "string",
                    "description": "Required when status is complete. Briefly cite the proof that the goal is done."
                },
                "verification": {
                    "type": "object",
                    "description": "Required when status is complete or not_achieved. A verifier-as-judge receipt from a concrete check, such as Run action=\"verifiers\" or an equivalent project-specific gate.",
                    "properties": {
                        "status": {
                            "type": "string",
                            "enum": ["passed", "not_applicable", "not_achieved"],
                            "description": "Use passed when a concrete verifier/check succeeded; not_applicable when no automated verifier applies; not_achieved when the verifier found concrete remaining gaps."
                        },
                        "check": {
                            "type": "string",
                            "description": "The verifier/check that passed."
                        },
                        "summary": {
                            "type": "string",
                            "description": "Brief result summary from the verifier/check."
                        },
                        "role": {
                            "type": "string",
                            "enum": ["critical", "advisory"],
                            "description": "Critical reviews may satisfy the judged completion contract. Advisory reviews are fail-open and cannot complete it. Defaults to critical for compatibility."
                        },
                        "gaps": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Concrete remaining gaps. Required for critical not_achieved reviews; order and duplicate wording do not affect the stall fingerprint."
                        }
                    },
                    "required": ["status", "check", "summary"],
                    "additionalProperties": false
                },
                "blocker": {
                    "type": "string",
                    "description": "Required when status is blocked. Explain the condition preventing progress."
                },
                "advisory": {
                    "type": "string",
                    "description": "Required when status is advisory. Appended separately from the judged completion contract."
                },
                "progress": {
                    "type": "object",
                    "description": "Optional with not_achieved, advisory or yield: your current best estimate of overall completion, shown to the user as reported progress. Keep percent honest — it is an estimate, never a verified fraction.",
                    "properties": {
                        "percent": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": 100,
                            "description": "Estimated percent complete, 0-100."
                        },
                        "now": {
                            "type": "string",
                            "description": "One short line: what is being worked on right now."
                        },
                        "next": {
                            "type": "string",
                            "description": "One short line: what comes next."
                        }
                    },
                    "required": ["percent"],
                    "additionalProperties": false
                }
            },
            "required": ["status"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        Vec::new()
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        require_root_goal_mutation(context)?;
        // #5123-class: `objective` used to be accepted and silently ignored
        // with a success receipt. The objective is immutable after
        // create_goal; fail fast and name the corrective path.
        if input
            .get("objective")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Err(ToolError::invalid_input(
                "update_goal cannot change the objective — it is immutable after create_goal. \
                 Mark the current goal complete or blocked, then create_goal with the new objective",
            ));
        }
        let status = required_str(&input, "status")?.trim().to_ascii_lowercase();
        let progress = parse_progress_report(&input)?;
        if progress.is_some() && !matches!(status.as_str(), "not_achieved" | "advisory" | "yield") {
            return Err(ToolError::invalid_input(
                "progress is only accepted with status not_achieved, advisory or yield",
            ));
        }
        let snapshot = {
            let mut state = lock_goal_state(&self.goal_state)?;
            // #6542: with no goal there is nothing to update. Say so as a
            // successful no-op rather than an error the model retries.
            if state.objective.is_none() {
                return Ok(ToolResult::success(format!(
                    "No goal is set, so update_goal(status: {status}) changed nothing. \
                     Continue the user's request directly; create_goal only if the user \
                     asked for a tracked goal."
                )));
            }
            match status.as_str() {
                "complete" => {
                    let evidence = input
                        .get("evidence")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .unwrap_or_default()
                        .to_string();
                    if evidence.is_empty() {
                        return Err(ToolError::invalid_input(
                            "evidence is required when status is complete",
                        ));
                    }
                    let verification = parse_completion_verification(&input)?;
                    state
                        .mark_complete(evidence, verification)
                        .map_err(ToolError::invalid_input)?;
                }
                "blocked" => {
                    let blocker = input
                        .get("blocker")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .unwrap_or_default()
                        .to_string();
                    if blocker.is_empty() {
                        return Err(ToolError::invalid_input(
                            "blocker is required when status is blocked",
                        ));
                    }
                    state
                        .mark_blocked(blocker)
                        .map_err(ToolError::invalid_input)?;
                }
                "not_achieved" => {
                    let verification = parse_progress_verification(&input)?;
                    state
                        .record_not_achieved(verification)
                        .map_err(ToolError::invalid_input)?;
                    if let Some(progress) = progress {
                        state.record_progress(progress);
                    }
                }
                "yield" => {
                    state.mark_yielded().map_err(ToolError::invalid_input)?;
                    if progress.is_some() {
                        state.progress = progress;
                    }
                }
                "advisory" => {
                    let advisory = input
                        .get("advisory")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .unwrap_or_default()
                        .to_string();
                    if advisory.is_empty() {
                        return Err(ToolError::invalid_input(
                            "advisory is required when status is advisory",
                        ));
                    }
                    state
                        .record_advisory(advisory)
                        .map_err(ToolError::invalid_input)?;
                    if let Some(progress) = progress {
                        state.record_progress(progress);
                    }
                }
                other => {
                    return Err(ToolError::invalid_input(format!(
                        "unsupported goal status '{other}'; update_goal can only mark complete, blocked, or yield, record not_achieved verifier gaps, or append advisory context"
                    )));
                }
            }
            state.snapshot()
        };
        json_result(&snapshot)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// A yield is a hand-back, not a judgement about the work: it stops the
    /// auto-continuation, and the user's next message resumes it. A pause the
    /// user asked for stays put.
    #[test]
    fn a_yield_stops_continuation_and_the_users_message_resumes_it() {
        let mut state = GoalState::default();
        state.replace("ship the slice", None, Some("goal-1".to_string()));
        assert!(state.is_active());

        state.mark_yielded().expect("yield an active goal");
        assert_eq!(state.snapshot().status, GoalStatus::Paused.as_str());
        assert_eq!(
            state.snapshot().pause_reason,
            Some(GoalPauseReason::Yielded),
            "the pause names the hand-back so the UI can say what it is"
        );
        assert!(
            !state.is_active(),
            "neither continuation dispatcher re-arms a non-active goal"
        );

        assert!(
            state.resume_after_yield(),
            "the user's next message continues the work"
        );
        assert!(state.is_active());
        assert_eq!(state.snapshot().pause_reason, None);
    }

    #[test]
    fn a_user_pause_is_not_resumed_by_a_yield_resume() {
        let mut state = GoalState::default();
        state.replace("ship the slice", None, Some("goal-1".to_string()));
        state
            .mark_paused(GoalPauseReason::User)
            .expect("user pause");

        assert!(
            !state.resume_after_yield(),
            "only a hand-back is resumed by answering it"
        );
        assert_eq!(state.snapshot().status, GoalStatus::Paused.as_str());
    }

    #[test]
    fn a_yield_cannot_overwrite_a_pause_reason_someone_else_set() {
        let mut state = GoalState::default();
        state.replace("ship the slice", None, Some("goal-1".to_string()));

        for reason in [
            GoalPauseReason::User,
            GoalPauseReason::BudgetLimit,
            GoalPauseReason::NoProgress,
            GoalPauseReason::UsageLimit,
        ] {
            state.mark_paused(reason).expect("pause for another reason");
            assert!(
                state.mark_yielded().is_err(),
                "only an active goal can be handed back, not one paused for {reason:?}"
            );
            assert_eq!(
                state.snapshot().pause_reason,
                Some(reason),
                "the pause someone else set survives the rejected hand-back"
            );
        }

        // And a blocked goal keeps its blocker rather than becoming resumable.
        state
            .mark_blocked("waiting on the vendor".to_string())
            .expect("block");
        assert!(state.mark_yielded().is_err());
        assert_eq!(
            state.snapshot().blocker.as_deref(),
            Some("waiting on the vendor")
        );
    }

    #[test]
    fn a_pause_reason_written_by_a_newer_build_still_reads() {
        let reason: GoalPauseReason = serde_json::from_str("\"invented-later\"")
            .expect("an unknown pause reason must not fail the durable load");
        assert_eq!(reason, GoalPauseReason::Unrecognized);
        // And the known values still round-trip.
        assert_eq!(
            serde_json::from_str::<GoalPauseReason>("\"yielded\"").expect("yielded reads"),
            GoalPauseReason::Yielded
        );
    }

    #[tokio::test]
    async fn update_goal_yield_pauses_without_completing_or_blocking() {
        let state = new_shared_goal_state();
        let ctx = ToolContext::new(".");
        CreateGoalTool::new(state.clone())
            .execute(json!({"objective": "ship the runtime slice"}), &ctx)
            .await
            .expect("create goal");

        let result = UpdateGoalTool::new(state.clone())
            .execute(json!({"status": "yield"}), &ctx)
            .await
            .expect("yield is a supported status");
        assert!(result.success, "yield must not be refused");

        let snapshot = state.lock().expect("goal state").snapshot();
        assert_eq!(snapshot.status, GoalStatus::Paused.as_str());
        assert_eq!(snapshot.pause_reason, Some(GoalPauseReason::Yielded));
        assert_eq!(snapshot.blocker, None, "a hand-back reports no blocker");
        assert_eq!(snapshot.evidence, None, "and claims no completion");
    }

    #[tokio::test]
    async fn update_goal_rejects_objective_knob_instead_of_ignoring_it() {
        // #5123-class: `objective` used to return a success receipt with no
        // behavior. It is immutable after create_goal; the knob is gone from
        // the schema and supplying it fails fast with the corrective path.
        let state = new_shared_goal_state();
        let ctx = ToolContext::new(".");
        let create = CreateGoalTool::new(state.clone());
        create
            .execute(json!({"objective": "ship the runtime slice"}), &ctx)
            .await
            .expect("create goal");

        let update = UpdateGoalTool::new(state.clone());
        let schema = update.input_schema();
        assert!(
            schema["properties"].get("objective").is_none(),
            "ignored objective knob must not be advertised: {schema}"
        );

        let err = update
            .execute(
                json!({"status": "blocked", "blocker": "x", "objective": "different goal"}),
                &ctx,
            )
            .await
            .expect_err("objective must not be silently ignored");
        let message = format!("{err}");
        assert!(message.contains("immutable"), "{message}");
        assert!(message.contains("create_goal"), "{message}");
        // The rejected call must not have mutated goal state.
        assert!(state.lock().expect("goal lock").is_active());
    }

    #[tokio::test]
    async fn update_goal_without_a_goal_is_a_clear_no_op() {
        let state = new_shared_goal_state();
        let update = UpdateGoalTool::new(state.clone());
        for input in [
            json!({"status": "complete", "evidence": "done"}),
            json!({"status": "blocked", "blocker": "x"}),
            json!({"status": "advisory", "advisory": "note"}),
        ] {
            let result = update
                .execute(input, &ToolContext::new("."))
                .await
                .expect("no goal is a no-op, not an error");
            assert!(result.success);
            assert!(
                result.content.contains("No goal is set"),
                "{}",
                result.content
            );
        }
        assert!(state.lock().expect("goal lock").objective.is_none());
    }

    #[tokio::test]
    async fn create_get_and_complete_goal() {
        let state = new_shared_goal_state();
        let ctx = ToolContext::new(".");

        let create = CreateGoalTool::new(state.clone());
        let created = create
            .execute(
                json!({
                    "objective": "ship the runtime slice",
                    "token_budget": 1200
                }),
                &ctx,
            )
            .await
            .expect("create goal");
        assert!(created.success);
        let created_json: Value = serde_json::from_str(&created.content).expect("created json");
        assert_eq!(
            created_json.get("status").and_then(Value::as_str),
            Some("active")
        );

        let get = GetGoalTool::new(state.clone());
        let current = get.execute(json!({}), &ctx).await.expect("get goal");
        assert!(current.content.contains("ship the runtime slice"));
        let current_json: Value = serde_json::from_str(&current.content).expect("current json");
        assert_eq!(
            current_json.get("token_budget").and_then(Value::as_u64),
            Some(1200)
        );

        let update = UpdateGoalTool::new(state.clone());
        let completed = update
            .execute(
                json!({
                    "status": "complete",
                    "evidence": "focused tests passed",
                    "verification": {
                        "status": "passed",
                        "check": "cargo test -p codewhale-tui goal_loop",
                        "summary": "focused tests passed"
                    }
                }),
                &ctx,
            )
            .await
            .expect("complete goal");
        let completed_json: Value =
            serde_json::from_str(&completed.content).expect("completed json");
        assert_eq!(
            completed_json.get("status").and_then(Value::as_str),
            Some("complete")
        );
        assert!(completed.content.contains("focused tests passed"));
        assert!(!state.lock().expect("goal lock").is_active());
    }

    #[test]
    fn unfinished_goal_replacement_fails_closed_without_mutating_state() {
        for status in [GoalStatus::Active, GoalStatus::Paused, GoalStatus::Blocked] {
            let mut state = GoalState::default();
            state.sync_from_host_status(
                Some("preserve the current objective"),
                Some(1_200),
                status,
            );
            state.record_usage(300, 12, std::time::Instant::now());
            state.record_continuation();
            let before = state.snapshot();

            let error = state
                .create("replace it silently".to_string(), Some(99))
                .expect_err("unfinished goal replacement must fail");

            assert!(
                error.contains("unfinished goal"),
                "status {status:?}: {error}"
            );
            assert_eq!(
                state.snapshot(),
                before,
                "status {status:?} must preserve the entire goal snapshot"
            );
        }
    }

    #[test]
    fn same_objective_goal_host_resume_clears_terminal_payloads_and_preserves_progress() {
        let mut blocked = GoalState::default();
        blocked
            .create("resume the release goal".to_string(), Some(4_000))
            .expect("create blocked fixture");
        blocked.record_usage(750, 44, std::time::Instant::now());
        blocked.record_continuation();
        blocked
            .mark_blocked("provider failed".to_string())
            .expect("block goal");

        blocked.sync_from_host_status(
            Some("resume the release goal"),
            Some(4_000),
            GoalStatus::Active,
        );

        let resumed = blocked.snapshot();
        assert_eq!(resumed.status, "active");
        assert_eq!(resumed.tokens_used, 750);
        assert_eq!(resumed.time_used_seconds, 44);
        assert_eq!(resumed.continuation_count, 1);
        assert_eq!(resumed.evidence, None);
        assert_eq!(resumed.blocker, None);
        assert_eq!(resumed.completion_verification, None);
        let prompt = render_continuation_prompt(&resumed, resumed.continuation_count);
        assert!(prompt.contains("\"blocker\": null"), "{prompt}");

        let mut completed = GoalState::default();
        completed
            .create("resume verified work".to_string(), None)
            .expect("create completed fixture");
        completed
            .mark_complete(
                "focused tests passed".to_string(),
                GoalCompletionVerification {
                    status: "passed".to_string(),
                    check: "cargo test".to_string(),
                    summary: "goal tests passed".to_string(),
                    ..Default::default()
                },
            )
            .expect("complete goal");

        completed.sync_from_host_status(Some("resume verified work"), None, GoalStatus::Active);
        let resumed = completed.snapshot();
        assert_eq!(resumed.status, "active");
        assert_eq!(resumed.evidence, None);
        assert_eq!(resumed.blocker, None);
        assert_eq!(resumed.completion_verification, None);
    }

    /// #6561 D04-12: blocking or pausing used to overwrite a sealed, judged
    /// completion and clear its verification.
    #[test]
    fn sealed_completion_cannot_be_blocked_or_paused() {
        let mut state = GoalState::default();
        state
            .create("ship the verified change".to_string(), None)
            .expect("create goal");
        state
            .mark_complete(
                "focused tests passed".to_string(),
                GoalCompletionVerification {
                    status: "passed".to_string(),
                    check: "cargo test".to_string(),
                    summary: "goal tests passed".to_string(),
                    ..Default::default()
                },
            )
            .expect("complete goal");
        let sealed = state.snapshot();

        assert!(state.mark_blocked("late blocker".to_string()).is_err());
        assert!(
            state
                .mark_runtime_blocked("runtime stop".to_string())
                .is_err()
        );
        assert!(state.mark_paused(GoalPauseReason::NoProgress).is_err());

        let after = state.snapshot();
        assert_eq!(after.status, "complete");
        assert_eq!(after.evidence, sealed.evidence);
        assert_eq!(after.blocker, None);
        assert!(after.completion_verification.is_some());
        assert_eq!(
            after.completion_verification,
            sealed.completion_verification
        );
    }

    #[test]
    fn completed_goal_can_be_replaced_with_fresh_accounting() {
        let mut state = GoalState::default();
        state
            .create("finish the first objective".to_string(), Some(1_200))
            .expect("create first goal");
        state.record_usage(300, 12, std::time::Instant::now());
        state.record_continuation();
        state
            .mark_complete(
                "focused tests passed".to_string(),
                GoalCompletionVerification {
                    status: "passed".to_string(),
                    check: "cargo test".to_string(),
                    summary: "goal tests passed".to_string(),
                    ..Default::default()
                },
            )
            .expect("complete first goal");

        state
            .create("start the next objective".to_string(), Some(2_400))
            .expect("completed goal may be replaced");

        let snapshot = state.snapshot();
        assert_eq!(
            snapshot.objective.as_deref(),
            Some("start the next objective")
        );
        assert_eq!(snapshot.status, "active");
        assert_eq!(snapshot.token_budget, Some(2_400));
        assert_eq!(snapshot.tokens_used, 0);
        assert_eq!(snapshot.time_used_seconds, 0);
        assert_eq!(snapshot.continuation_count, 0);
        assert_eq!(snapshot.evidence, None);
        assert_eq!(snapshot.blocker, None);
        assert_eq!(snapshot.completion_verification, None);
    }

    #[tokio::test]
    async fn subagent_context_cannot_mutate_parent_goal() {
        let state = new_shared_goal_state_from_host_status(
            Some("keep root lifecycle authority".to_string()),
            Some(1_200),
            GoalStatus::Active,
        );
        let before = state.lock().expect("goal lock").snapshot();
        let child_context = ToolContext::new(".").with_owner_agent("agent_child", "child verifier");

        let create_error = CreateGoalTool::new(state.clone())
            .execute(
                json!({"objective": "replace the parent goal"}),
                &child_context,
            )
            .await
            .expect_err("child create_goal must fail");
        assert!(create_error.to_string().contains("root-agent only"));

        let update_error = UpdateGoalTool::new(state.clone())
            .execute(
                json!({"status": "blocked", "blocker": "child decided to stop"}),
                &child_context,
            )
            .await
            .expect_err("child update_goal must fail");
        assert!(update_error.to_string().contains("root-agent only"));

        assert_eq!(
            state.lock().expect("goal lock").snapshot(),
            before,
            "rejected child mutations must leave the parent goal unchanged"
        );
    }

    #[tokio::test]
    async fn update_goal_requires_completion_evidence() {
        let state = new_shared_goal_state_from_host_status(
            Some("prove completion".to_string()),
            None,
            GoalStatus::Active,
        );
        let update = UpdateGoalTool::new(state);
        let err = update
            .execute(json!({"status": "complete"}), &ToolContext::new("."))
            .await
            .expect_err("missing evidence should fail");

        assert!(err.to_string().contains("evidence is required"));
    }

    #[tokio::test]
    async fn update_goal_accepts_not_applicable_verification_for_non_verifiable_goals() {
        let state = new_shared_goal_state_from_host_status(
            Some("write the release notes".to_string()),
            None,
            GoalStatus::Active,
        );
        let update = UpdateGoalTool::new(state.clone());
        let completed = update
            .execute(
                json!({
                    "status": "complete",
                    "evidence": "release notes drafted and reviewed in thread",
                    "verification": {
                        "status": "not_applicable",
                        "check": "no automated verifier applies",
                        "summary": "writing task completed with evidence in thread"
                    }
                }),
                &ToolContext::new("."),
            )
            .await
            .expect("non-verifiable goal should complete");

        let completed_json: Value =
            serde_json::from_str(&completed.content).expect("completed json");
        assert_eq!(
            completed_json.get("status").and_then(Value::as_str),
            Some("complete")
        );
        assert_eq!(
            completed_json
                .get("completion_verification")
                .and_then(|verification| verification.get("status"))
                .and_then(Value::as_str),
            Some("not_applicable")
        );
        assert!(!state.lock().expect("goal lock").is_active());
    }

    #[tokio::test]
    async fn update_goal_requires_passed_verification_to_complete() {
        let state = new_shared_goal_state_from_host_status(
            Some("prove completion".to_string()),
            None,
            GoalStatus::Active,
        );
        let update = UpdateGoalTool::new(state.clone());
        let err = update
            .execute(
                json!({
                    "status": "complete",
                    "evidence": "all checks look good"
                }),
                &ToolContext::new("."),
            )
            .await
            .expect_err("missing verifier gate should fail");

        assert!(err.to_string().contains("verification is required"));
        assert!(state.lock().expect("goal lock").is_active());
    }

    #[tokio::test]
    async fn advisory_review_is_append_only_and_fail_open() {
        let state = new_shared_goal_state_from_host_status(
            Some("keep the judged contract authoritative".to_string()),
            None,
            GoalStatus::Active,
        );
        let update = UpdateGoalTool::new(state.clone());
        update
            .execute(
                json!({
                    "status": "advisory",
                    "advisory": "Consider a narrower compatibility test."
                }),
                &ToolContext::new("."),
            )
            .await
            .expect("advisory note");
        let result = state.lock().expect("goal lock").snapshot();

        assert_eq!(result.status, "active");
        assert_eq!(result.advisories.len(), 1);
        assert_eq!(
            result.advisories[0].summary,
            "Consider a narrower compatibility test."
        );
        assert!(result.completion_verification.is_none());
    }

    #[tokio::test]
    async fn advisory_verification_cannot_complete_goal() {
        let state = new_shared_goal_state_from_host_status(
            Some("require a critical judge".to_string()),
            None,
            GoalStatus::Active,
        );
        let err = UpdateGoalTool::new(state.clone())
            .execute(
                json!({
                    "status": "complete",
                    "evidence": "an advisor liked it",
                    "verification": {
                        "status": "passed",
                        "check": "advisory review",
                        "summary": "looks reasonable",
                        "role": "advisory"
                    }
                }),
                &ToolContext::new("."),
            )
            .await
            .expect_err("advisory completion must fail closed");

        assert!(err.to_string().contains("advisory review cannot complete"));
        assert!(state.lock().expect("goal lock").is_active());
    }

    #[test]
    fn judged_completion_contract_is_fingerprinted_and_immutable() {
        let mut state = GoalState::default();
        state
            .create("seal the release candidate".to_string(), None)
            .expect("create goal");
        state
            .mark_complete(
                "locked tests passed".to_string(),
                GoalCompletionVerification {
                    status: "passed".to_string(),
                    check: "cargo test --locked".to_string(),
                    summary: "all required tests passed".to_string(),
                    ..Default::default()
                },
            )
            .expect("seal judged contract");
        let sealed = state.snapshot();
        let fingerprint = &sealed
            .completion_verification
            .as_ref()
            .expect("completion contract")
            .contract_fingerprint;
        assert_eq!(fingerprint.len(), 64);

        let err = state
            .mark_complete(
                "replace the evidence".to_string(),
                GoalCompletionVerification {
                    status: "passed".to_string(),
                    check: "different check".to_string(),
                    summary: "different result".to_string(),
                    ..Default::default()
                },
            )
            .expect_err("sealed contract must be immutable");
        assert!(err.contains("already sealed"));
        assert_eq!(state.snapshot(), sealed);
    }

    fn not_achieved_review(role: GoalReviewRole, gaps: &[&str]) -> GoalProgressVerification {
        GoalProgressVerification {
            status: "not_achieved".to_string(),
            check: "critical verifier".to_string(),
            summary: "remaining work found".to_string(),
            role,
            gaps: gaps.iter().map(|gap| (*gap).to_string()).collect(),
        }
    }

    #[test]
    fn equivalent_gap_sets_have_one_stable_fingerprint() {
        let first = gap_fingerprint(&[
            "  Add   a regression test ".to_string(),
            "Fix provider copy".to_string(),
        ]);
        let reordered = gap_fingerprint(&[
            "fix PROVIDER copy".to_string(),
            "add a regression test".to_string(),
            "Add a regression test".to_string(),
        ]);
        assert_eq!(first, reordered);
        assert_eq!(first.expect("fingerprint").len(), 64);
    }

    #[test]
    fn changed_gaps_reset_stall_counter_and_advice_never_advances_it() {
        let mut state = GoalState::default();
        state
            .create("keep making measurable progress".to_string(), None)
            .expect("create goal");
        state
            .record_not_achieved(not_achieved_review(
                GoalReviewRole::Critical,
                &["first gap"],
            ))
            .expect("first critical review");
        // Two reports of one gap only count twice when they land on separate
        // continuation passes; several inside one turn are one pass.
        state.record_continuation();
        state
            .record_not_achieved(not_achieved_review(
                GoalReviewRole::Critical,
                &["first gap"],
            ))
            .expect("repeat critical review");
        assert_eq!(state.snapshot().repeated_gap_count, 2);

        state
            .record_not_achieved(not_achieved_review(
                GoalReviewRole::Advisory,
                &["advisor-only concern"],
            ))
            .expect("advisory review is fail-open");
        let after_advice = state.snapshot();
        assert_eq!(after_advice.repeated_gap_count, 2);
        assert_eq!(after_advice.advisories.len(), 1);
        assert_eq!(after_advice.status, "active");

        state
            .record_not_achieved(not_achieved_review(
                GoalReviewRole::Critical,
                &["a different remaining gap"],
            ))
            .expect("changed critical review");
        let progressed = state.snapshot();
        assert_eq!(progressed.repeated_gap_count, 1);
        assert_eq!(progressed.status, "active");
    }

    #[test]
    fn repeated_equivalent_gap_sets_pause_the_loop_for_no_progress() {
        // The continuation prompt promises this stop, and until it existed the
        // default Operate goal had none: `DEFAULT_MAX_GOAL_CONTINUATIONS` is 0,
        // so only the model volunteering complete/blocked ended a run.
        let mut state = GoalState::default();
        state
            .create("stall on purpose".to_string(), None)
            .expect("create goal");

        for pass in 1..crate::goal_loop::MAX_REPEATED_GAP_PASSES {
            state
                .record_not_achieved(not_achieved_review(
                    GoalReviewRole::Critical,
                    &["provider copy still wrong", "  Regression   test MISSING "],
                ))
                .expect("critical review below the stall bound");
            let snapshot = state.snapshot();
            assert_eq!(snapshot.repeated_gap_count, pass);
            assert!(
                snapshot.is_active(),
                "pass {pass} is under the bound and must keep working",
            );
            // The bound counts continuation PASSES, so each iteration has to
            // actually be one. Without this the loop would be several reports
            // inside a single turn, which deliberately no longer advances it.
            state.record_continuation();
        }

        // Reordered and re-cased wording is the same gap set, so restating the
        // previous pass cannot buy another pass.
        state
            .record_not_achieved(not_achieved_review(
                GoalReviewRole::Critical,
                &["Regression test missing", "PROVIDER copy still wrong"],
            ))
            .expect("stall review is recorded, not rejected");

        let stalled = state.snapshot();
        assert_eq!(
            stalled.repeated_gap_count,
            crate::goal_loop::MAX_REPEATED_GAP_PASSES
        );
        assert_eq!(stalled.status, "paused");
        assert_eq!(stalled.pause_reason, Some(GoalPauseReason::NoProgress));
        assert!(
            !stalled.is_active(),
            "an inactive goal is what stops both continuation dispatchers",
        );

        // The pause holds: a stalled goal cannot keep reporting gaps at itself.
        let err = state
            .record_not_achieved(not_achieved_review(
                GoalReviewRole::Critical,
                &["provider copy still wrong"],
            ))
            .expect_err("a paused goal takes no further verifier progress");
        assert!(err.contains("active goal"));
    }

    #[test]
    fn repeating_one_gap_inside_a_single_turn_does_not_trip_the_stall_bound() {
        // `record_not_achieved` runs per `update_goal` tool call. Counting
        // calls rather than passes meant a verifier that restated the same gap
        // three times in ONE turn paused the goal before a single continuation
        // had been spent — stopping valid work and calling it a stall.
        let mut state = GoalState::default();
        state
            .create("one turn, several reports".to_string(), None)
            .expect("create goal");

        for _ in 0..(crate::goal_loop::MAX_REPEATED_GAP_PASSES + 2) {
            state
                .record_not_achieved(not_achieved_review(
                    GoalReviewRole::Critical,
                    &["provider copy still wrong"],
                ))
                .expect("repeated reports inside one turn are recorded");
        }

        let snapshot = state.snapshot();
        assert_eq!(
            snapshot.repeated_gap_count, 1,
            "many reports in one turn are still one pass",
        );
        assert!(
            snapshot.is_active(),
            "no continuation was spent, so there is no stall to pause on",
        );
    }

    #[tokio::test]
    async fn update_goal_rejects_model_resume() {
        let state = new_shared_goal_state_from_host_status(
            Some("pause remains host controlled".to_string()),
            None,
            GoalStatus::Paused,
        );
        let update = UpdateGoalTool::new(state);
        let err = update
            .execute(json!({"status": "active"}), &ToolContext::new("."))
            .await
            .expect_err("model resume should fail");

        assert!(
            err.to_string().contains("complete, blocked, or yield"),
            "model resume stays rejected: {err}"
        );
    }

    #[test]
    fn paused_host_goal_is_not_active() {
        let state = new_shared_goal_state_from_host_status(
            Some("wait for user".to_string()),
            Some(42),
            GoalStatus::Paused,
        );
        let snapshot = state.lock().expect("goal lock").snapshot();

        assert_eq!(snapshot.status, "paused");
        assert_eq!(snapshot.token_budget, Some(42));
        assert_eq!(snapshot.pause_reason, Some(GoalPauseReason::User));
        assert!(!snapshot.is_active());
    }

    #[test]
    fn goal_state_projects_usage_and_continuations() {
        let state = new_shared_goal_state_from_host_status(
            Some("persist accounting".to_string()),
            Some(1_000),
            GoalStatus::Active,
        );
        {
            let mut goal = state.lock().expect("goal lock");
            goal.record_usage(300, 12, std::time::Instant::now());
            goal.record_continuation();
        }

        let snapshot = state.lock().expect("goal lock").snapshot();
        assert_eq!(snapshot.tokens_used, 300);
        assert_eq!(snapshot.time_used_seconds, 12);
        assert_eq!(snapshot.continuation_count, 1);
    }

    #[test]
    fn completed_goal_snapshot_freezes_elapsed() {
        // Regression: a completed goal's snapshot elapsed_seconds must not keep
        // growing. Before the fix, snapshot() always used started_at.elapsed(),
        // so a finished goal's elapsed kept ticking in the sidebar/tool output.
        let state = new_shared_goal_state_from_host_status(
            Some("freeze on completion".to_string()),
            None,
            GoalStatus::Active,
        );
        let first = {
            let mut goal = state.lock().expect("goal lock");
            goal.mark_complete(
                "evidence".to_string(),
                GoalCompletionVerification {
                    status: "passed".to_string(),
                    check: "cargo test".to_string(),
                    summary: "ok".to_string(),
                    ..Default::default()
                },
            )
            .expect("mark complete");
            goal.snapshot()
        };
        let elapsed_at_completion = first.elapsed_seconds.expect("elapsed present");

        // Sleep past a whole-second boundary. Under the old (buggy) code,
        // snapshot() returned started_at.elapsed().as_secs(), so this would
        // tick up by at least one second and the assertion below would fail.
        // With the freeze, the completed snapshot stays at the captured value.
        std::thread::sleep(std::time::Duration::from_millis(1_100));
        let second = state.lock().expect("goal lock").snapshot();
        assert_eq!(second.status, "complete");
        assert_eq!(
            second.elapsed_seconds,
            Some(elapsed_at_completion),
            "completed goal elapsed must be frozen, not keep ticking"
        );
    }

    #[test]
    fn protocol_thread_goal_converts_to_runtime_snapshot() {
        let snapshot = GoalSnapshot::from_thread_goal(&codewhale_protocol::ThreadGoal {
            thread_id: "thread-1".to_string(),
            goal_id: "goal-1".to_string(),
            objective: "Bridge the goal models".to_string(),
            status: codewhale_protocol::ThreadGoalStatus::Active,
            token_budget: Some(2_000),
            tokens_used: 750,
            time_used_seconds: 44,
            continuation_count: 3,
            last_gap_fingerprint: None,
            repeated_gap_count: 0,
            last_gap_pass: None,
            pause_reason: None,
            created_at: 1,
            updated_at: 2,
        });

        assert_eq!(
            snapshot.objective.as_deref(),
            Some("Bridge the goal models")
        );
        assert_eq!(snapshot.status, "active");
        assert_eq!(snapshot.token_budget, Some(2_000));
        assert_eq!(snapshot.tokens_used, 750);
        assert_eq!(snapshot.time_used_seconds, 44);
        assert_eq!(snapshot.continuation_count, 3);
    }

    #[test]
    fn protocol_limit_statuses_keep_distinct_pause_reasons() {
        for (status, reason) in [
            (
                codewhale_protocol::ThreadGoalStatus::UsageLimited,
                GoalPauseReason::UsageLimit,
            ),
            (
                codewhale_protocol::ThreadGoalStatus::BudgetLimited,
                GoalPauseReason::BudgetLimit,
            ),
        ] {
            let (projected, projected_reason) = thread_goal_status_projection(status, None);
            assert_eq!(projected, GoalStatus::Paused);
            assert_eq!(projected_reason, Some(reason));
        }
    }

    #[test]
    fn continuation_prompt_includes_bound_and_goal_state() {
        let snapshot = GoalSnapshot {
            objective: Some("finish issue 2199".to_string()),
            status: "active".to_string(),
            token_budget: None,
            tokens_used: 0,
            time_used_seconds: 0,
            continuation_count: 0,
            elapsed_seconds: Some(5),
            evidence: None,
            blocker: None,
            pause_reason: None,
            completion_verification: None,
            ..Default::default()
        };

        let prompt = render_continuation_prompt(&snapshot, 2);
        assert!(prompt.contains("Goal Continuation"));
        assert!(prompt.contains("finish issue 2199"));
        assert!(prompt.contains("Continuation pass #2"));
        // The named bound has to be the one the state machine actually
        // enforces; the prompt used to promise a stall stop that nothing
        // implemented.
        assert!(
            prompt.contains(&format!(
                "{} equivalent gap sets in a row",
                crate::goal_loop::MAX_REPEATED_GAP_PASSES
            )),
            "{prompt}"
        );
    }

    #[test]
    fn update_goal_contract_treats_required_user_input_as_blocking() {
        let update = UpdateGoalTool::new(new_shared_goal_state());
        assert!(update.description().contains("requires user input"));
    }

    #[test]
    fn goal_progress_bar_fills_in_proportion() {
        assert_eq!(goal_progress_bar(0), "░░░░░░░░");
        assert_eq!(goal_progress_bar(50), "▓▓▓▓░░░░");
        assert_eq!(goal_progress_bar(100), "▓▓▓▓▓▓▓▓");
        assert_eq!(goal_progress_bar(200), "▓▓▓▓▓▓▓▓");
    }

    #[tokio::test]
    async fn update_goal_records_progress_with_not_achieved_and_advisory() {
        let state = new_shared_goal_state();
        {
            let mut guard = state.lock().expect("goal lock");
            guard
                .create("ship the release".to_string(), None)
                .expect("create");
        }
        let tool = UpdateGoalTool::new(state.clone());
        let context = ToolContext::new(".");
        let result = tool
            .execute(
                json!({
                    "status": "not_achieved",
                    "verification": {
                        "status": "not_achieved",
                        "check": "cargo test",
                        "summary": "two failures remain",
                        "gaps": ["picker test", "pricing test"]
                    },
                    "progress": {"percent": 40, "now": "fixing the picker", "next": "rerun gates"}
                }),
                &context,
            )
            .await
            .expect("not_achieved accepted");
        let snapshot: Value = serde_json::from_str(&result.content).expect("snapshot json");
        let progress = snapshot.get("progress").expect("progress recorded");
        assert_eq!(progress.get("percent").and_then(Value::as_u64), Some(40));
        assert_eq!(
            progress.get("now").and_then(Value::as_str),
            Some("fixing the picker")
        );
        assert_eq!(
            progress.get("next").and_then(Value::as_str),
            Some("rerun gates")
        );

        let result = tool
            .execute(
                json!({
                    "status": "advisory",
                    "advisory": "cache eviction is likely",
                    "progress": {"percent": 55}
                }),
                &context,
            )
            .await
            .expect("advisory accepted");
        let snapshot: Value = serde_json::from_str(&result.content).expect("snapshot json");
        assert_eq!(
            snapshot
                .get("progress")
                .and_then(|progress| progress.get("percent"))
                .and_then(Value::as_u64),
            Some(55)
        );
    }

    #[tokio::test]
    async fn update_goal_rejects_progress_on_terminal_status_and_bad_percent() {
        let state = new_shared_goal_state();
        {
            let mut guard = state.lock().expect("goal lock");
            guard
                .create("ship the release".to_string(), None)
                .expect("create");
        }
        let tool = UpdateGoalTool::new(state.clone());
        let context = ToolContext::new(".");
        let err = tool
            .execute(
                json!({
                    "status": "complete",
                    "evidence": "all gates pass",
                    "verification": {"status": "passed", "check": "cargo test", "summary": "ok"},
                    "progress": {"percent": 100}
                }),
                &context,
            )
            .await
            .expect_err("progress is not terminal evidence");
        assert!(
            err.to_string().contains("not_achieved, advisory or yield"),
            "{err}"
        );

        let err = tool
            .execute(
                json!({
                    "status": "advisory",
                    "advisory": "note",
                    "progress": {"percent": 140}
                }),
                &context,
            )
            .await
            .expect_err("percent above 100 must fail");
        assert!(err.to_string().contains("0 to 100"), "{err}");
    }

    #[test]
    fn from_snapshot_holds_exhausted_stall_window_paused() {
        // A restored snapshot that is still Active with a full stall window
        // is corrupt: the engine pauses NoProgress in the same mutation that
        // reaches the ceiling. The rehydrated state must stay paused instead
        // of arming another pass.
        let snapshot = GoalSnapshot {
            goal_id: Some("goal-stall".to_string()),
            objective: Some("finish issue 2199".to_string()),
            status: "active".to_string(),
            continuation_count: 3,
            last_gap_fingerprint: Some("b".repeat(64)),
            repeated_gap_count: crate::goal_loop::MAX_REPEATED_GAP_PASSES,
            last_gap_pass: Some(3),
            ..Default::default()
        };
        snapshot.validate_stall_state().expect("structurally valid");
        let state = GoalState::from_snapshot(&snapshot);
        assert_eq!(state.status, Some(GoalStatus::Paused));
        assert_eq!(state.pause_reason, Some(GoalPauseReason::NoProgress));
        assert!(!state.is_active());

        // One below the ceiling restores as ordinary Active state.
        let below = GoalSnapshot {
            repeated_gap_count: crate::goal_loop::MAX_REPEATED_GAP_PASSES - 1,
            ..snapshot
        };
        let state = GoalState::from_snapshot(&below);
        assert_eq!(state.status, Some(GoalStatus::Active));
        assert!(state.is_active());
    }

    #[test]
    fn from_persisted_keeps_counters_that_sync_from_host_status_resets() {
        // Rehydration treats the durable record as history: the counters
        // survive, evidence starts empty, and only a terminal status carries
        // a finish time.
        let restored = GoalState::from_persisted(
            "ship the goal loop",
            Some(50_000),
            GoalStatus::Active,
            None,
            1_234,
            56,
            3,
        );
        assert_eq!(restored.objective.as_deref(), Some("ship the goal loop"));
        assert_eq!(restored.token_budget, Some(50_000));
        assert_eq!(restored.status, Some(GoalStatus::Active));
        assert_eq!(restored.tokens_used, 1_234);
        assert_eq!(restored.time_used_seconds, 56);
        assert_eq!(restored.continuation_count, 3);
        assert!(restored.started_at.is_some());
        assert!(restored.finished_at.is_none());
        assert!(restored.evidence.is_none());
        assert!(restored.blocker.is_none());
        assert!(restored.advisories.is_empty());

        let blocked = GoalState::from_persisted(
            "ship the goal loop",
            None,
            GoalStatus::Blocked,
            None,
            0,
            0,
            0,
        );
        assert!(blocked.finished_at.is_some());

        // The same objective through the host-status path keeps the counters
        // (it is not a re-declaration)…
        let mut state = restored;
        state.sync_from_host_status(Some("ship the goal loop"), Some(50_000), GoalStatus::Active);
        assert_eq!(state.tokens_used, 1_234);
        assert_eq!(state.continuation_count, 3);

        // …while a changed objective resets them — the contrast that makes
        // `from_persisted` necessary for rehydration.
        state.sync_from_host_status(Some("a different objective"), None, GoalStatus::Active);
        assert_eq!(state.tokens_used, 0);
        assert_eq!(state.time_used_seconds, 0);
        assert_eq!(state.continuation_count, 0);
    }
}
