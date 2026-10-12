//! Turn context and tracking.
//!
//! A "turn" is one user message and the resulting AI response,
//! including any tool calls that occur.
//!
//! ## Snapshot lifecycle hooks
//!
//! [`restore_point_snapshot`] (`pre-turn:`, `tool:`, `post-tool:`) and
//! [`post_turn_snapshot`] book-end a turn and its tool calls by
//! taking a workspace-level snapshot into a side git repo (see
//! `crate::snapshot`). They are intentionally non-blocking and
//! non-fatal: any IO error is logged at WARN and swallowed so a busted
//! filesystem or missing `git` binary never derails the agent loop.
//! `/restore N` and the `revert_turn` tool both consume these
//! snapshots.

use crate::core::events::TurnRoute;
use crate::snapshot::{SnapshotRepo, TakenSnapshot};
use codewhale_models::Usage;
use std::path::Path;
use std::time::Instant;

/// Which configured limit governs a turn's step budget (#5994).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepBudgetSource {
    /// The ordinary interactive ceiling (`max_steps`).
    Interactive,
    /// The goal-turn allowance (`[goal] max_steps`).
    Goal,
}

impl StepBudgetSource {
    /// The configuration key named in soft-landing and exhaustion notices.
    #[must_use]
    pub const fn key_label(self) -> &'static str {
        match self {
            Self::Interactive => "max_steps",
            Self::Goal => "[goal] max_steps",
        }
    }
}

/// Where a turn's user message sat in the session right after it was added:
/// the session length and messages revision then. Together they identify
/// that message; a later append or rewrite changes one of them (#6566).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnansweredUserMessage {
    pub len: usize,
    pub revision: u64,
}

/// Context for a single turn (user message + AI response).
#[derive(Debug)]
pub struct TurnContext {
    pub max_output_tokens: Option<std::num::NonZeroU32>,
    /// Turn ID
    pub id: String,

    /// When the turn started
    pub started_at: Instant,

    /// Current step in the turn (tool call iteration)
    pub step: u32,

    /// Configured steps, or `u32::MAX` for no limit. Use `step_limit` for
    /// budget decisions; the counter saturates without stopping an uncapped turn.
    pub max_steps: u32,

    /// Which configured limit `max_steps` came from.
    pub budget_source: StepBudgetSource,

    /// The turn's step budget was exhausted and the bounded final report was
    /// granted (#5994). Set by the turn loop; the cross-turn goal fence reads
    /// it so an exhausted goal pauses instead of re-arming.
    pub budget_exhausted_final_report: bool,
    /// This turn's user message while no model request has been accepted
    /// yet. A credential rejection while it is still the last message takes
    /// it back out of the session, so a retry after fixing the key is not a
    /// duplicate (#6566). Cleared once a request is accepted: from then on a
    /// model has seen the message.
    pub unanswered_user_message: Option<UnansweredUserMessage>,

    pub(crate) stop_diagnostics: crate::tool_inspection::TurnStopDiagnostics,
    pub(crate) last_request_snapshot: Option<crate::tool_inspection::ToolInspectionSnapshot>,

    /// Number of tool calls made in this turn.

    /// Usage for this turn
    pub usage: Usage,

    /// Subset of `usage` served by the parent turn's frozen route. Programmatic
    /// reviewer/RLM calls remain in the total above but are billed only from
    /// their own routed receipts.
    pub parent_route_usage: Usage,

    /// Provider calls whose usage became ambiguous after dispatch (for
    /// example an RLM timeout). A non-zero value makes cost coverage
    /// explicitly incomplete instead of inventing a zero-usage receipt.
    pub routed_usage_dropped_records: u64,

    /// Input tokens reported for the most recent parent-route model request.
    /// This is deliberately separate from `usage`, which accumulates every
    /// parent step and programmatic child call for billing.
    pub(crate) latest_parent_input_tokens: Option<u32>,

    /// `session.messages.len()` at the parent request whose billed prompt is
    /// in `latest_parent_input_tokens`. Tool results appended after that
    /// request are not in the bill; GrokBuild's pre-sampling gate adds a
    /// byte-estimate of that suffix so auto-compact can fire mid-turn.
    pub(crate) messages_len_at_last_parent_prompt: Option<usize>,

    /// One-shot latch: an automatic-compaction refusal has already been
    /// surfaced this turn. Pressure is re-checked every step, and repeating
    /// the same refusal on each of a long turn's steps would be noise.
    pub(crate) compaction_refusal_notified: bool,

    /// Route facts resolved for this turn but not timestamped until the first
    /// provider request is actually dispatched.
    pub(crate) pending_route: Option<TurnRoute>,

    /// The provider's answer when it rejected an emergency context-recovery
    /// request (capability, auth, unreachable). The turn then fails on that
    /// cause, not on the context budget the recovery was trying to fix.
    pub(crate) context_recovery_rejection: Option<anyhow::Error>,
}

impl TurnContext {
    /// Create a new turn context
    pub fn new(max_steps: u32) -> Self {
        Self::with_budget_source(max_steps, StepBudgetSource::Interactive)
    }

    /// Create a turn context with an explicit budget provenance (#5994).
    pub fn with_budget_source(max_steps: u32, budget_source: StepBudgetSource) -> Self {
        Self {
            max_output_tokens: None,
            id: uuid::Uuid::new_v4().to_string(),
            started_at: Instant::now(),
            step: 0,
            max_steps,
            budget_source,
            budget_exhausted_final_report: false,
            unanswered_user_message: None,
            stop_diagnostics: crate::tool_inspection::TurnStopDiagnostics {
                effective_max_steps: (max_steps != u32::MAX).then_some(max_steps),
                step_budget_source: budget_source.key_label(),
                ..Default::default()
            },
            last_request_snapshot: None,
            usage: Usage {
                input_tokens: 0,
                output_tokens: 0,
                ..Usage::default()
            },
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            latest_parent_input_tokens: None,
            messages_len_at_last_parent_prompt: None,
            compaction_refusal_notified: false,
            pending_route: None,
            context_recovery_rejection: None,
        }
    }

    /// Increment the step counter
    pub fn next_step(&mut self) -> bool {
        self.step = self.step.saturating_add(1);
        self.step_limit().is_none_or(|limit| self.step <= limit)
    }

    /// A resolved integer default means no ceiling, including at counter
    /// saturation. Explicit positive configuration is clamped before here.
    #[must_use]
    pub fn step_limit(&self) -> Option<u32> {
        (self.max_steps != u32::MAX).then_some(self.max_steps)
    }

    /// Check if the turn has reached max steps
    pub fn at_max_steps(&self) -> bool {
        self.step_limit().is_some_and(|limit| self.step >= limit)
    }

    /// Model steps consumed so far (for soft-landing and reporting).
    #[must_use]
    pub fn steps_used(&self) -> u32 {
        self.step
    }

    /// Complete the existing request projection with observed turn-exit facts.
    /// A turn that never prepared a request has no request snapshot to publish.
    pub(crate) fn terminal_request_snapshot(
        &mut self,
        status: super::events::TurnOutcomeStatus,
    ) -> Option<crate::tool_inspection::ToolInspectionSnapshot> {
        use crate::tool_inspection::TurnStopReason;
        self.stop_diagnostics.status = Some(status);
        self.stop_diagnostics.model_step_index = self.step;
        self.stop_diagnostics.final_report_requested |= self.budget_exhausted_final_report;
        self.stop_diagnostics.last_reported_input_tokens = self.latest_parent_input_tokens;
        match status {
            super::events::TurnOutcomeStatus::Interrupted => {
                self.stop_diagnostics.reason = Some(TurnStopReason::Interrupted);
            }
            super::events::TurnOutcomeStatus::Failed if self.stop_diagnostics.reason.is_none() => {
                self.stop_diagnostics.reason = Some(TurnStopReason::Failed);
            }
            _ => {}
        }
        let mut snapshot = self.last_request_snapshot.take()?;
        snapshot.terminal = Some(self.stop_diagnostics.clone());
        Some(snapshot)
    }

    /// Add usage from an API response
    pub fn add_usage(&mut self, usage: &Usage) {
        add_usage_to(&mut self.usage, usage);
    }

    /// Record one parent-route response for both billing and live-context
    /// pressure. Child-model usage must call [`Self::add_usage`] directly so
    /// it cannot masquerade as the parent request's context size.
    pub fn add_parent_usage(&mut self, usage: &Usage) {
        self.latest_parent_input_tokens = (usage.input_tokens > 0).then_some(usage.input_tokens);
        self.add_usage(usage);
        add_usage_to(&mut self.parent_route_usage, usage);
    }

    pub fn add_routed_usage_dropped_records(&mut self, dropped_records: u64) {
        self.routed_usage_dropped_records = self
            .routed_usage_dropped_records
            .saturating_add(dropped_records);
    }

    /// Add programmatic child-call usage to the authoritative total and
    /// return the same batch aggregate for telemetry emission.
    pub fn add_routed_usages<'a>(&mut self, usages: impl IntoIterator<Item = &'a Usage>) -> Usage {
        let mut aggregate = Usage::default();
        for usage in usages {
            self.add_usage(usage);
            add_usage_to(&mut aggregate, usage);
        }
        aggregate
    }
}

pub(crate) fn add_usage_to(total: &mut Usage, delta: &Usage) {
    total.input_tokens = total.input_tokens.saturating_add(delta.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(delta.output_tokens);
    total.prompt_cache_hit_tokens =
        add_optional_usage(total.prompt_cache_hit_tokens, delta.prompt_cache_hit_tokens);
    total.prompt_cache_miss_tokens = add_optional_usage(
        total.prompt_cache_miss_tokens,
        delta.prompt_cache_miss_tokens,
    );
    total.prompt_cache_write_tokens = add_optional_usage(
        total.prompt_cache_write_tokens,
        delta.prompt_cache_write_tokens,
    );
    total.reasoning_tokens = add_optional_usage(total.reasoning_tokens, delta.reasoning_tokens);
    total.reasoning_replay_tokens =
        add_optional_usage(total.reasoning_replay_tokens, delta.reasoning_replay_tokens);
    if let Some(delta) = delta.server_tool_use.as_ref() {
        let server_total = total.server_tool_use.get_or_insert_default();
        server_total.code_execution_requests = add_optional_usage(
            server_total.code_execution_requests,
            delta.code_execution_requests,
        );
        server_total.tool_search_requests = add_optional_usage(
            server_total.tool_search_requests,
            delta.tool_search_requests,
        );
    }
}

impl TurnContext {
    /// Billed prompt the compaction gate should honor: this turn's latest
    /// parent request, else the session-carried receipt from the previous
    /// turn. A fresh `TurnContext` starts empty, so without the session
    /// fallback an 842k DeepSeek bill dies at the turn boundary and the
    /// next send never auto-compacts (#5577).
    #[must_use]
    pub(crate) fn billed_input_tokens_for_compaction(
        &self,
        session_billed: Option<u32>,
    ) -> Option<u64> {
        self.latest_parent_input_tokens
            .or(session_billed)
            .map(u64::from)
    }

    /// Record how long the transcript was when the latest parent prompt was
    /// billed. Call immediately after `add_parent_usage`, before this
    /// response's assistant/tool messages are appended.
    pub(crate) fn note_parent_prompt_len(&mut self, message_count: usize) {
        self.messages_len_at_last_parent_prompt = Some(message_count);
    }

    /// Live context for the auto-compact gate: last billed prompt plus a
    /// /4 estimate of messages appended since that prompt (tool results,
    /// the assistant reply that will be replayed on the next request).
    ///
    /// `max(billed, estimate(full list))` hides mid-turn growth when the
    /// estimator undercounts the whole transcript below the last bill —
    /// which is why auto-compact never fired even with the UI meter above
    /// 80%. GrokBuild's `check_auto_compact_needed` uses the same split:
    /// exact prior count + byte-estimate of items since last response.
    #[must_use]
    pub(crate) fn live_input_tokens_for_compaction(
        &self,
        messages: &[codewhale_models::Message],
        system_prompt: Option<&codewhale_models::SystemPrompt>,
        session_billed: Option<u32>,
    ) -> Option<u64> {
        let billed = self.billed_input_tokens_for_compaction(session_billed);
        let suffix_start = self
            .messages_len_at_last_parent_prompt
            .unwrap_or(messages.len())
            .min(messages.len());
        let suffix = &messages[suffix_start..];
        let growth = if suffix.is_empty() {
            0
        } else {
            u64::try_from(crate::compaction::estimate_input_tokens_for_pressure(
                suffix, None,
            ))
            .unwrap_or(u64::MAX)
        };
        let estimated = u64::try_from(crate::compaction::estimate_input_tokens_for_pressure(
            messages,
            system_prompt,
        ))
        .unwrap_or(u64::MAX);
        let live = estimated.max(billed.unwrap_or(0).saturating_add(growth));
        (live > 0).then_some(live)
    }

    /// Drop the turn-local billed receipt after history is rewritten so the
    /// next step cannot compact again on the pre-compaction prompt.
    pub(crate) fn clear_parent_input_tokens(&mut self) {
        self.latest_parent_input_tokens = None;
        self.messages_len_at_last_parent_prompt = None;
    }
}

fn add_optional_usage(total: Option<u32>, delta: Option<u32>) -> Option<u32> {
    match (total, delta) {
        (Some(total), Some(delta)) => Some(total.saturating_add(delta)),
        (None, Some(delta)) => Some(delta),
        (Some(total), None) => Some(total),
        (None, None) => None,
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;
    use codewhale_models::ServerToolUsage;

    #[test]
    fn add_usage_preserves_replay_and_saturates_server_tool_counters() {
        let mut turn = TurnContext::new(2);
        turn.add_usage(&Usage {
            reasoning_replay_tokens: Some(u32::MAX - 1),
            server_tool_use: Some(ServerToolUsage {
                code_execution_requests: Some(u32::MAX),
                tool_search_requests: Some(2),
            }),
            ..Usage::default()
        });
        turn.add_usage(&Usage {
            reasoning_replay_tokens: Some(9),
            server_tool_use: Some(ServerToolUsage {
                code_execution_requests: Some(1),
                tool_search_requests: Some(3),
            }),
            ..Usage::default()
        });

        assert_eq!(turn.usage.reasoning_replay_tokens, Some(u32::MAX));
        let server = turn.usage.server_tool_use.expect("server tool usage");
        assert_eq!(server.code_execution_requests, Some(u32::MAX));
        assert_eq!(server.tool_search_requests, Some(5));
    }

    fn below_threshold(messages: &[codewhale_models::Message], turn: &TurnContext) -> bool {
        let config = crate::compaction::CompactionConfig {
            enabled: true,
            token_threshold: 100_000,
            ..Default::default()
        };
        !crate::compaction::compaction_pressure_reached_with_billed(
            messages,
            None,
            &config,
            turn.latest_parent_input_tokens.map(u64::from),
        )
    }

    #[test]
    fn cumulative_low_context_parent_steps_cannot_trigger_compaction() {
        let mut turn = TurnContext::new(4);
        turn.add_parent_usage(&Usage {
            input_tokens: 60_000,
            ..Usage::default()
        });
        turn.add_parent_usage(&Usage {
            input_tokens: 70_000,
            ..Usage::default()
        });

        assert_eq!(turn.usage.input_tokens, 130_000);
        assert_eq!(turn.latest_parent_input_tokens, Some(70_000));
        assert!(below_threshold(&[], &turn));
    }

    #[test]
    fn child_usage_cannot_replace_parent_context_pressure() {
        let mut turn = TurnContext::new(4);
        turn.add_parent_usage(&Usage {
            input_tokens: 70_000,
            ..Usage::default()
        });
        turn.add_usage(&Usage {
            input_tokens: 250_000,
            ..Usage::default()
        });

        assert_eq!(turn.usage.input_tokens, 320_000);
        assert_eq!(turn.latest_parent_input_tokens, Some(70_000));
        assert!(below_threshold(&[], &turn));
    }

    #[test]
    fn fresh_turn_inherits_session_billed_prompt_for_compaction() {
        let turn = TurnContext::new(4);
        assert_eq!(turn.latest_parent_input_tokens, None);
        assert_eq!(
            turn.billed_input_tokens_for_compaction(Some(842_000)),
            Some(842_000)
        );
        assert_eq!(turn.billed_input_tokens_for_compaction(None), None);
    }

    #[test]
    fn live_turn_billed_outranks_stale_session_billed() {
        let mut turn = TurnContext::new(4);
        turn.add_parent_usage(&Usage {
            input_tokens: 12_000,
            ..Usage::default()
        });
        assert_eq!(
            turn.billed_input_tokens_for_compaction(Some(842_000)),
            Some(12_000)
        );
        turn.clear_parent_input_tokens();
        assert_eq!(turn.latest_parent_input_tokens, None);
        assert_eq!(turn.messages_len_at_last_parent_prompt, None);
        assert_eq!(
            turn.billed_input_tokens_for_compaction(Some(842_000)),
            Some(842_000)
        );
    }

    #[test]
    fn live_compaction_tokens_include_tool_results_after_the_billed_prompt() {
        // GrokBuild/Codex: last billed prompt + items since that request.
        // A 70k bill plus a large tool result must cross an 80k trigger even
        // when the full-list /4 estimate stays below the bill (the failure
        // mode that kept auto-compact from firing mid-turn above 80%).
        let mut turn = TurnContext::new(4);
        turn.add_parent_usage(&Usage {
            input_tokens: 70_000,
            ..Usage::default()
        });
        let prompt = vec![codewhale_models::Message {
            role: codewhale_models::Role::User,
            content: vec![codewhale_models::ContentBlock::Text {
                text: "do the work".to_string(),
                cache_control: None,
            }],
        }];
        turn.note_parent_prompt_len(prompt.len());

        let mut with_tool = prompt;
        with_tool.push(codewhale_models::Message {
            role: codewhale_models::Role::User,
            content: vec![codewhale_models::ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "call-1".to_string(),
                content: "x".repeat(80_000),
                is_error: None,
                content_blocks: None,
            }],
        });

        let config = crate::compaction::CompactionConfig {
            enabled: true,
            token_threshold: 80_000,
            ..Default::default()
        };
        assert!(
            !crate::compaction::compaction_pressure_reached_with_billed(
                &with_tool,
                None,
                &config,
                turn.billed_input_tokens_for_compaction(None),
            ),
            "stale billed prompt alone must not be the live gate"
        );
        let live = turn
            .live_input_tokens_for_compaction(&with_tool, None, None)
            .expect("live tokens");
        assert!(
            live >= 80_000,
            "tool-result suffix must lift live tokens over the trigger, got {live}"
        );
        assert!(crate::compaction::compaction_pressure_reached_with_billed(
            &with_tool,
            None,
            &config,
            Some(live),
        ));
    }
}

/// Maximum characters of the user prompt snippet to embed in a snapshot
/// label. Longer prompts are truncated with an ellipsis.
const USER_PROMPT_LABEL_MAX: usize = 100;

/// Format a snapshot label that includes the user prompt for readability
/// in `/restore` listings.
///
/// Takes the first line of the prompt (up to `USER_PROMPT_LABEL_MAX`
/// characters) and appends it to the traditional `type:seq` label so
/// users can identify which turn each snapshot belongs to.
pub(crate) fn format_snapshot_label(
    prefix: &str,
    turn_seq: u64,
    user_prompt: Option<&str>,
) -> String {
    let base = format!("{prefix}:{turn_seq}");
    match user_prompt {
        None | Some("") => base,
        Some(prompt) => match snapshot_label_prompt_snippet(prompt) {
            None => base,
            Some(snippet) => format!("{base}: {snippet}"),
        },
    }
}

/// The exact prompt snippet [`format_snapshot_label`] embeds after `type:seq`.
///
/// Read surfaces that want to correlate a recorded prompt back to a restore
/// point must go through this function rather than re-deriving the truncation,
/// so the reader and the writer can never disagree about what a label means.
/// Returns `None` when the prompt contributes no snippet at all.
pub(crate) fn snapshot_label_prompt_snippet(prompt: &str) -> Option<String> {
    if prompt.is_empty() {
        return None;
    }
    let first_line = prompt.lines().next().unwrap_or("");
    let truncated: String = first_line.chars().take(USER_PROMPT_LABEL_MAX).collect();
    if truncated.chars().count() < first_line.chars().count() {
        Some(format!("{truncated}…"))
    } else {
        Some(truncated)
    }
}

/// A snapshot label parsed back into its parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedSnapshotLabel {
    /// `pre-turn`, `post-turn`, `tool`, or whatever prefix produced it.
    pub kind: String,
    /// The turn sequence for `pre-turn`/`post-turn` labels. `tool` labels
    /// carry a call id rather than a sequence, so this stays `None` for them.
    pub seq: Option<u64>,
    /// The embedded prompt snippet, exactly as
    /// [`snapshot_label_prompt_snippet`] produced it.
    pub prompt_snippet: Option<String>,
}

/// Parse a label produced by [`format_snapshot_label`].
///
/// This is deliberately total: an unrecognized label still yields a record with
/// the raw text as `kind`, because a read surface must describe what is really
/// stored rather than silently dropping rows it does not recognize.
pub(crate) fn parse_snapshot_label(label: &str) -> ParsedSnapshotLabel {
    let (head, snippet) = match label.split_once(": ") {
        Some((head, rest)) => (head, Some(rest.to_string())),
        None => (label, None),
    };
    match head.split_once(':') {
        Some((kind, seq)) => ParsedSnapshotLabel {
            kind: kind.to_string(),
            seq: seq.parse::<u64>().ok(),
            prompt_snippet: snippet,
        },
        None => ParsedSnapshotLabel {
            kind: head.to_string(),
            seq: None,
            prompt_snippet: snippet,
        },
    }
}

/// Take a `pre-turn:<seq>` workspace snapshot.
///
/// `cap_bytes` is the workspace-size ceiling that gates first-init
/// (passed through to [`SnapshotRepo::open_or_init_with_cap`]); pass
/// `0` to disable the cap.
/// `user_prompt` is an optional snippet of the user's message for this
/// turn, embedded in the snapshot label so `/restore` listings are
/// human-readable.
///
/// Returns the snapshot (commit and tree) on success, `None` on any error.
/// Errors are logged at WARN; the turn loop must not block on this. The
/// engine takes its pre-turn snapshots through [`restore_point_snapshot`].
#[cfg(test)]
pub fn pre_turn_snapshot(
    workspace: &Path,
    turn_seq: u64,
    cap_bytes: u64,
    user_prompt: Option<&str>,
    session_id: Option<&str>,
) -> Option<TakenSnapshot> {
    snapshot_with_label(
        workspace,
        &format_snapshot_label("pre-turn", turn_seq, user_prompt),
        cap_bytes,
        session_id,
    )
}

/// Take a workspace snapshot under `label` for the running turn, and report
/// which paths changed since the turn's previous snapshot `since` (a tree or
/// commit id).
///
/// The comparison runs before the count prune that follows every snapshot,
/// so `since` is still in the store; `None` for the changed paths means there
/// was no `since` or it could not be compared. `None` on any error, logged
/// at WARN; the turn loop must not block on it.
pub fn restore_point_snapshot(
    workspace: &Path,
    label: &str,
    cap_bytes: u64,
    session_id: Option<&str>,
    since: Option<&crate::snapshot::SnapshotId>,
) -> Option<(TakenSnapshot, Option<Vec<std::path::PathBuf>>)> {
    snapshot_with_label_since(workspace, label, cap_bytes, session_id, since)
}

/// Take a `post-turn:<seq>` workspace snapshot. Same failure model as
/// [`restore_point_snapshot`].
pub fn post_turn_snapshot(
    workspace: &Path,
    turn_seq: u64,
    cap_bytes: u64,
    user_prompt: Option<&str>,
    session_id: Option<&str>,
) -> Option<TakenSnapshot> {
    snapshot_with_label(
        workspace,
        &format_snapshot_label("post-turn", turn_seq, user_prompt),
        cap_bytes,
        session_id,
    )
}

fn snapshot_with_label(
    workspace: &Path,
    label: &str,
    cap_bytes: u64,
    session_id: Option<&str>,
) -> Option<TakenSnapshot> {
    snapshot_with_label_since(workspace, label, cap_bytes, session_id, None).map(|(taken, _)| taken)
}

fn snapshot_with_label_since(
    workspace: &Path,
    label: &str,
    cap_bytes: u64,
    session_id: Option<&str>,
    since: Option<&crate::snapshot::SnapshotId>,
) -> Option<(TakenSnapshot, Option<Vec<std::path::PathBuf>>)> {
    match SnapshotRepo::open_or_init_with_cap(workspace, cap_bytes) {
        Ok(repo) => {
            // Undo that silently stops working is the failure this guards
            // (B2): a repaired history and a failing snapshot both reach the
            // user through the same notice as the gates, never only a log.
            let taken = repo.repair_broken_head().and_then(|repaired| {
                repo.take_snapshot(label, session_id)
                    .map(|taken| (taken, repaired))
            });
            let (id, repaired) = match taken {
                Ok((taken, repaired)) => (Some(taken), repaired),
                Err(e) => {
                    tracing::warn!(target: "snapshot", "snapshot '{label}' failed: {e}");
                    record_snapshot_notice(
                        workspace,
                        session_id,
                        SnapshotsDisabledScope::Failing,
                        snapshot_failure_detail(&e),
                    );
                    return None;
                }
            };
            clear_snapshots_disabled_status(workspace, session_id);
            if repaired {
                record_snapshot_notice(
                    workspace,
                    session_id,
                    SnapshotsDisabledScope::HistoryRepaired,
                    String::new(),
                );
            }
            // What changed since the turn's previous snapshot, compared
            // before the prune below can drop that snapshot.
            let changed = match (&id, since) {
                (Some(taken), Some(since)) if since.as_str() == taken.tree.as_str() => {
                    Some(Vec::new())
                }
                (Some(taken), Some(since)) => {
                    match repo.changed_paths_between(since, &taken.tree) {
                        Ok(paths) => Some(paths),
                        Err(e) => {
                            tracing::warn!(
                                target: "snapshot",
                                "comparing snapshot '{label}' with the turn's previous one failed: {e}"
                            );
                            None
                        }
                    }
                }
                _ => None,
            };
            // Prune oldest snapshots to cap disk usage (#1112).
            if let Err(e) = repo.prune_keep_last_n_batched(max_snapshots_for(workspace)) {
                tracing::warn!(target: "snapshot", "snapshot prune failed: {e}");
            }
            id.map(|taken| (taken, changed))
        }
        Err(e) => {
            // The first gated failure belongs to this session, even when other
            // sessions use the same workspace in this process (#5930).
            if maybe_notify_snapshots_disabled_once(workspace, session_id, cap_bytes, &e) {
                tracing::warn!(target: "snapshot", session_id, "snapshot repo init failed: {e}");
            } else {
                tracing::debug!(target: "snapshot", "snapshot repo init still failing: {e}");
            }
            None
        }
    }
}

/// The count cap [`SnapshotRepo::prune_keep_last_n`] applies after each
/// snapshot: [`crate::snapshot::DEFAULT_MAX_SNAPSHOTS`], which tests can
/// lower per workspace to exercise pruning without taking fifty snapshots.
fn max_snapshots_for(workspace: &Path) -> usize {
    #[cfg(test)]
    if let Some(max) = test_max_snapshots::get(workspace) {
        return max;
    }
    let _ = workspace;
    crate::snapshot::DEFAULT_MAX_SNAPSHOTS
}

#[cfg(test)]
pub(crate) mod test_max_snapshots {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    static OVERRIDES: parking_lot::Mutex<Option<HashMap<PathBuf, usize>>> =
        parking_lot::Mutex::new(None);

    fn key(workspace: &Path) -> PathBuf {
        workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf())
    }

    /// Prune `workspace`'s snapshots to `max` (plus as many turn
    /// boundaries) after each snapshot, for the rest of the test process.
    pub(crate) fn set(workspace: &Path, max: usize) {
        OVERRIDES
            .lock()
            .get_or_insert_with(HashMap::new)
            .insert(key(workspace), max);
    }

    pub(super) fn get(workspace: &Path) -> Option<usize> {
        OVERRIDES
            .lock()
            .as_ref()
            .and_then(|overrides| overrides.get(&key(workspace)).copied())
    }
}

// Shared snapshot data; host localization and session retention stay here.
#[cfg(test)]
pub use codewhale_command_contract::config_policy::SNAPSHOTS_CAP_CONFIG_KEY;
pub use codewhale_command_contract::config_policy::{
    StatusSnapshotNotice as SnapshotsDisabledNotice, StatusSnapshotScope as SnapshotsDisabledScope,
};

/// Host localization of the shared semantic notice, including the toast surface.
pub trait SnapshotsDisabledNoticeUi {
    fn localize(&self, locale: codewhale_localization::Locale) -> String;
}
impl SnapshotsDisabledNoticeUi for SnapshotsDisabledNotice {
    fn localize(&self, locale: codewhale_localization::Locale) -> String {
        use codewhale_localization::MessageId;
        let id = match self.scope {
            SnapshotsDisabledScope::WorkspaceTooLarge => MessageId::SnapshotsDisabledTooLarge,
            SnapshotsDisabledScope::TooManyFiles => MessageId::SnapshotsDisabledTooManyFiles,
            SnapshotsDisabledScope::UnsafeLocation => MessageId::SnapshotsDisabledUnsafeLocation,
            SnapshotsDisabledScope::HistoryRepaired => MessageId::SnapshotsHistoryRepaired,
            SnapshotsDisabledScope::Failing => MessageId::SnapshotsFailing,
        };
        self.render(&codewhale_localization::tr(locale, id))
    }
}

/// Human-readable byte cap for the size-gate notice. Keeps small test caps
/// from rendering as a misleading `0 GB`.
fn format_cap_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let value = bytes as f64;
    if value >= KIB.powi(3) {
        format!("{:.1} GB", value / KIB.powi(3))
    } else if value >= KIB.powi(2) {
        format!("{:.1} MB", value / KIB.powi(2))
    } else if value >= KIB {
        format!("{:.1} KB", value / KIB)
    } else {
        format!("{bytes} bytes")
    }
}

type SnapshotNoticeKey = (std::path::PathBuf, Option<String>);

#[derive(Default)]
struct SnapshotNoticeState {
    warned: bool,
    pending: bool,
    disabled: Option<SnapshotsDisabledNotice>,
}

fn snapshot_notices()
-> &'static std::sync::Mutex<std::collections::HashMap<SnapshotNoticeKey, SnapshotNoticeState>> {
    static NOTICES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<SnapshotNoticeKey, SnapshotNoticeState>>,
    > = std::sync::OnceLock::new();
    NOTICES.get_or_init(Default::default)
}

fn snapshot_notice_key(workspace: &Path, session_id: Option<&str>) -> SnapshotNoticeKey {
    (workspace.to_path_buf(), session_id.map(str::to_owned))
}

/// Take only this session's pending delivery. Other sessions in the same
/// workspace keep their own notice; the observed disabled status remains.
pub fn take_snapshots_disabled_notices(
    workspace: &Path,
    session_id: Option<&str>,
) -> Vec<SnapshotsDisabledNotice> {
    let mut states = snapshot_notices()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(state) = states.get_mut(&snapshot_notice_key(workspace, session_id)) else {
        return Vec::new();
    };
    if !std::mem::take(&mut state.pending) {
        return Vec::new();
    }
    state.disabled.iter().cloned().collect()
}

/// Non-consuming availability projection for the current session's status.
pub fn snapshots_disabled_status(
    workspace: &Path,
    session_id: Option<&str>,
) -> Option<SnapshotsDisabledNotice> {
    snapshot_notices()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&snapshot_notice_key(workspace, session_id))
        .and_then(|state| state.disabled.clone())
}

fn clear_snapshots_disabled_status(workspace: &Path, session_id: Option<&str>) {
    if let Some(state) = snapshot_notices()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_mut(&snapshot_notice_key(workspace, session_id))
    {
        state.disabled = None;
        state.pending = false;
    }
}

// Keep stderr for headless sessions. The TUI receives the same notice via the
// existing Engine event, and `/status` reads the retained observation.
// Production snapshot callers always supply the current Engine session id;
// callers without one retain the legacy workspace scope.
#[allow(clippy::print_stderr)]
fn maybe_notify_snapshots_disabled_once(
    workspace: &Path,
    session_id: Option<&str>,
    cap_bytes: u64,
    error: &std::io::Error,
) -> bool {
    let message = error.to_string();
    // The gate markers are declared by the snapshot policy that produces them,
    // so this stays one classifier rather than a second copy of the rules.
    let scope = if message.contains(crate::snapshot::GATE_TOO_LARGE_MARKER) {
        SnapshotsDisabledScope::WorkspaceTooLarge
    } else if message.contains(crate::snapshot::GATE_TOO_MANY_ENTRIES_MARKER) {
        SnapshotsDisabledScope::TooManyFiles
    } else if message.contains(crate::snapshot::GATE_UNSAFE_LOCATION_MARKER) {
        SnapshotsDisabledScope::UnsafeLocation
    } else {
        // A real snapshot/data-loss error, not a gate: say snapshots are
        // failing and why, never a "snapshots are off" gate notice.
        return record_snapshot_notice(
            workspace,
            session_id,
            SnapshotsDisabledScope::Failing,
            snapshot_failure_detail(error),
        );
    };
    let limit = match scope {
        SnapshotsDisabledScope::WorkspaceTooLarge => format_cap_bytes(cap_bytes),
        SnapshotsDisabledScope::TooManyFiles => crate::snapshot::SIZE_WALK_MAX_ENTRIES.to_string(),
        _ => String::new(),
    };
    record_snapshot_notice(workspace, session_id, scope, limit)
}

/// One line of a snapshot error for the notice: the first line, bounded.
fn snapshot_failure_detail(error: &std::io::Error) -> String {
    const MAX_CHARS: usize = 200;
    let text = error.to_string();
    let line = text.lines().next().unwrap_or_default().trim();
    if line.chars().count() > MAX_CHARS {
        let cut: String = line.chars().take(MAX_CHARS).collect();
        format!("{cut}…")
    } else {
        line.to_string()
    }
}

/// Record the session's snapshot availability and, the first time in this
/// session, queue its notice for the TUI and print it for headless runs.
/// Returns whether this call delivered the notice.
#[allow(clippy::print_stderr)]
fn record_snapshot_notice(
    workspace: &Path,
    session_id: Option<&str>,
    scope: SnapshotsDisabledScope,
    limit: String,
) -> bool {
    let notice = SnapshotsDisabledNotice {
        workspace: workspace.to_string_lossy().into_owned(),
        scope,
        limit,
    };
    let mut states = snapshot_notices()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let state = states
        .entry(snapshot_notice_key(workspace, session_id))
        .or_default();
    state.disabled = Some(notice.clone());
    if std::mem::replace(&mut state.warned, true) {
        return false;
    }
    state.pending = true;
    drop(states);
    // Headless stderr has no session locale to resolve; English is the pack
    // this path has always printed. The TUI and `/status` localize properly.
    eprintln!(
        "warning: {}",
        notice.localize(codewhale_localization::Locale::En)
    );
    true
}

#[cfg(test)]
mod snapshot_notice_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tracing_subscriber::prelude::*;

    #[derive(Clone, Default)]
    struct SnapshotWarnings(Arc<AtomicUsize>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SnapshotWarnings {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _context: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if event.metadata().target() == "snapshot"
                && *event.metadata().level() == tracing::Level::WARN
            {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    #[test]
    fn oversized_workspace_warns_once_per_session_and_retains_status_after_delivery() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let _user_home = crate::test_support::EnvVarGuard::set("HOME", root.path());
        let _user_profile = crate::test_support::EnvVarGuard::set("USERPROFILE", root.path());
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("large.txt"), vec![b'x'; 4096]).unwrap();
        let warnings = SnapshotWarnings::default();
        let subscriber = tracing_subscriber::registry().with(warnings.clone());
        tracing::subscriber::with_default(subscriber, || {
            for session in ["session-a", "session-b"] {
                for turn in 1..=3 {
                    assert!(
                        pre_turn_snapshot(&workspace, turn, 1024, None, Some(session)).is_none()
                    );
                    assert!(
                        post_turn_snapshot(&workspace, turn, 1024, None, Some(session)).is_none()
                    );
                }
            }
        });
        assert_eq!(
            warnings.0.load(Ordering::SeqCst),
            2,
            "exactly one real WARN for each session"
        );
        for session in ["session-b", "session-a"] {
            let notices = take_snapshots_disabled_notices(&workspace, Some(session));
            assert_eq!(notices.len(), 1, "each session receives its own notice");
            assert_eq!(notices[0].scope, SnapshotsDisabledScope::WorkspaceTooLarge);
            let line = notices[0].localize(codewhale_localization::Locale::En);
            assert_eq!(line.lines().count(), 1, "one line, not a stacked notice");
            assert_eq!(
                line.matches(&workspace.display().to_string()).count(),
                1,
                "the workspace is named exactly once: {line}"
            );
            assert_eq!(
                line.matches(SNAPSHOTS_CAP_CONFIG_KEY).count(),
                1,
                "the remedy is stated exactly once: {line}"
            );
            assert!(line.contains("1.0 KB"), "the tripped cap is named: {line}");
            assert!(take_snapshots_disabled_notices(&workspace, Some(session)).is_empty());
            assert_eq!(
                snapshots_disabled_status(&workspace, Some(session)),
                notices.first().cloned(),
                "delivery must not erase /status"
            );
        }
        assert!(snapshots_disabled_status(&workspace, Some("session-c")).is_none());
        assert!(snapshots_disabled_status(&root.path().join("other"), Some("session-a")).is_none());
    }

    /// The quiet case: a workspace under the cap snapshots and says nothing.
    #[test]
    fn small_workspace_snapshots_with_no_notice_at_all() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let _user_home = crate::test_support::EnvVarGuard::set("HOME", root.path());
        let _user_profile = crate::test_support::EnvVarGuard::set("USERPROFILE", root.path());
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("small.txt"), b"tiny").unwrap();
        assert!(pre_turn_snapshot(&workspace, 1, 1024 * 1024, None, Some("session")).is_some());
        assert!(snapshots_disabled_status(&workspace, Some("session")).is_none());
        assert!(take_snapshots_disabled_notices(&workspace, Some("session")).is_empty());
    }

    #[test]
    fn successful_snapshot_clears_disabled_status_and_pending_notice() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let _user_home = crate::test_support::EnvVarGuard::set("HOME", root.path());
        let _user_profile = crate::test_support::EnvVarGuard::set("USERPROFILE", root.path());
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("large.txt"), vec![b'x'; 4096]).unwrap();
        assert!(pre_turn_snapshot(&workspace, 1, 1024, None, Some("session")).is_none());
        assert!(snapshots_disabled_status(&workspace, Some("session")).is_some());
        assert!(pre_turn_snapshot(&workspace, 2, 0, None, Some("session")).is_some());
        assert!(snapshots_disabled_status(&workspace, Some("session")).is_none());
        assert!(take_snapshots_disabled_notices(&workspace, Some("session")).is_empty());
    }

    /// A real error is not a gate: the user is told snapshots are failing
    /// and why, never that they are "off" behind a limit.
    #[test]
    fn unrelated_snapshot_errors_say_snapshots_are_failing() {
        let workspace = tempfile::tempdir().unwrap();
        let error = std::io::Error::other("disk full");
        assert!(maybe_notify_snapshots_disabled_once(
            workspace.path(),
            Some("session"),
            1024,
            &error
        ));
        let notices = take_snapshots_disabled_notices(workspace.path(), Some("session"));
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].scope, SnapshotsDisabledScope::Failing);
        let line = notices[0].localize(codewhale_localization::Locale::En);
        assert!(
            line.contains("failing") && line.contains("disk full"),
            "{line}"
        );
        assert!(!line.contains(SNAPSHOTS_CAP_CONFIG_KEY), "{line}");
        assert!(snapshots_disabled_status(workspace.path(), Some("session")).is_some());
        assert!(
            !maybe_notify_snapshots_disabled_once(workspace.path(), Some("session"), 1024, &error),
            "told once per session"
        );
    }

    /// B2: a side repo whose HEAD names a missing commit is repaired on the
    /// next turn snapshot, and the user is told earlier restore points are
    /// gone instead of /undo silently dying.
    #[test]
    fn broken_snapshot_history_is_repaired_and_the_user_is_told() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let _user_home = crate::test_support::EnvVarGuard::set("HOME", root.path());
        let _user_profile = crate::test_support::EnvVarGuard::set("USERPROFILE", root.path());
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("a.txt"), b"alpha").unwrap();
        assert!(pre_turn_snapshot(&workspace, 1, 0, None, Some("session")).is_some());
        let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
        repo.point_head_at_missing_commit_for_test();
        // With a reflog the repair recovers the last good commit silently
        // (restore points survive); without one history restarts, which is
        // the case the user must be told about.
        std::fs::remove_dir_all(repo.git_dir().join("logs")).expect("drop reflogs");

        std::fs::write(workspace.join("a.txt"), b"beta").unwrap();
        assert!(
            post_turn_snapshot(&workspace, 1, 0, None, Some("session")).is_some(),
            "the snapshot succeeds after the repair"
        );
        let notices = take_snapshots_disabled_notices(&workspace, Some("session"));
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].scope, SnapshotsDisabledScope::HistoryRepaired);
        let line = notices[0].localize(codewhale_localization::Locale::En);
        assert!(line.contains("restarted"), "{line}");
        assert!(line.contains(&workspace.display().to_string()), "{line}");
        assert!(pre_turn_snapshot(&workspace, 2, 0, None, Some("session")).is_some());
        assert!(
            take_snapshots_disabled_notices(&workspace, Some("session")).is_empty(),
            "a healthy history says nothing more"
        );
    }

    /// Every gate must state a recovery that actually lifts *that* gate. The
    /// entry ceiling and the home/root refusal are not raised by the GB cap,
    /// so naming it there is the unhelpful follow-up this packet removes.
    #[test]
    fn each_gate_gets_its_own_accurate_recovery() {
        let workspace = tempfile::tempdir().unwrap();
        for (gate_message, scope, cap_bytes) in [
            (
                format!(
                    "{}: over 2 bytes in x",
                    crate::snapshot::GATE_TOO_MANY_ENTRIES_MARKER
                ),
                SnapshotsDisabledScope::TooManyFiles,
                0,
            ),
            (
                format!(
                    "{} for home directory: x",
                    crate::snapshot::GATE_UNSAFE_LOCATION_MARKER
                ),
                SnapshotsDisabledScope::UnsafeLocation,
                2 * 1024 * 1024 * 1024,
            ),
        ] {
            let session = format!("{scope:?}");
            let error = std::io::Error::new(std::io::ErrorKind::InvalidInput, gate_message);
            assert!(maybe_notify_snapshots_disabled_once(
                workspace.path(),
                Some(&session),
                cap_bytes,
                &error
            ));
            let notice = snapshots_disabled_status(workspace.path(), Some(&session))
                .expect("gated error must be retained for /status");
            assert_eq!(notice.scope, scope);
            let line = notice.localize(codewhale_localization::Locale::En);
            assert_eq!(line.lines().count(), 1, "one line, not a stacked notice");
            assert!(
                !line.contains(SNAPSHOTS_CAP_CONFIG_KEY),
                "{scope:?} must not advertise a config key that cannot lift it: {line}"
            );
            assert!(line.contains("/undo"), "the consequence is named: {line}");
            if scope == SnapshotsDisabledScope::TooManyFiles {
                // The `{limit}` this notice carries is the entry ceiling.
                // Nothing else asserts it reaches the user, so a dropped
                // placeholder would render "more than  files" silently.
                assert!(
                    line.contains(&crate::snapshot::SIZE_WALK_MAX_ENTRIES.to_string()),
                    "the entry ceiling must be stated, not left as a blank limit: {line}"
                );
            }
        }
    }

    #[test]
    fn oversize_notice_names_the_cap_and_only_then_the_config_key() {
        let workspace = tempfile::tempdir().unwrap();
        let error = std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{}: over x bytes in y",
                crate::snapshot::GATE_TOO_LARGE_MARKER
            ),
        );
        assert!(maybe_notify_snapshots_disabled_once(
            workspace.path(),
            Some("session"),
            2 * 1024 * 1024 * 1024,
            &error
        ));
        let notice =
            snapshots_disabled_status(workspace.path(), Some("session")).expect("retained");
        let line = notice.localize(codewhale_localization::Locale::En);
        assert!(line.contains("2.0 GB"), "{line}");
        assert!(line.contains(SNAPSHOTS_CAP_CONFIG_KEY), "{line}");
    }
}

#[cfg(test)]
mod snapshot_label_tests {
    use super::*;

    #[test]
    fn label_writer_and_parser_agree_on_prompt_snippet() {
        let prompt = "rename the widget\nsecond line is dropped";
        let label = format_snapshot_label("pre-turn", 7, Some(prompt));
        assert_eq!(label, "pre-turn:7: rename the widget");

        let parsed = parse_snapshot_label(&label);
        assert_eq!(parsed.kind, "pre-turn");
        assert_eq!(parsed.seq, Some(7));
        assert_eq!(
            parsed.prompt_snippet.as_deref(),
            snapshot_label_prompt_snippet(prompt).as_deref(),
            "a reader must recover exactly the snippet the writer embedded"
        );
    }

    #[test]
    fn truncated_prompt_round_trips_with_its_ellipsis() {
        let prompt = "x".repeat(USER_PROMPT_LABEL_MAX + 25);
        let label = format_snapshot_label("post-turn", 2, Some(&prompt));
        let parsed = parse_snapshot_label(&label);
        let snippet = parsed.prompt_snippet.expect("snippet");
        assert!(snippet.ends_with('…'));
        assert_eq!(snippet.chars().count(), USER_PROMPT_LABEL_MAX + 1);
        assert_eq!(
            Some(snippet),
            snapshot_label_prompt_snippet(&prompt),
            "truncated snippets must also round-trip"
        );
    }

    #[test]
    fn labels_without_a_prompt_parse_without_inventing_one() {
        let label = format_snapshot_label("pre-turn", 3, None);
        assert_eq!(label, "pre-turn:3");
        let parsed = parse_snapshot_label(&label);
        assert_eq!(parsed.kind, "pre-turn");
        assert_eq!(parsed.seq, Some(3));
        assert_eq!(parsed.prompt_snippet, None);
    }

    #[test]
    fn tool_labels_carry_a_call_id_not_a_sequence() {
        let label = format!("tool:{}", "call_abc123");
        let parsed = parse_snapshot_label(&label);
        assert_eq!(parsed.kind, "tool");
        assert_eq!(parsed.seq, None, "a call id is not a turn sequence");
        assert_eq!(parsed.prompt_snippet, None);
    }

    #[test]
    fn unrecognized_labels_are_reported_rather_than_dropped() {
        let parsed = parse_snapshot_label("manual checkpoint");
        assert_eq!(parsed.kind, "manual checkpoint");
        assert_eq!(parsed.seq, None);
        assert_eq!(parsed.prompt_snippet, None);
    }

    #[test]
    fn empty_prompt_contributes_no_snippet() {
        assert_eq!(snapshot_label_prompt_snippet(""), None);
        assert_eq!(format_snapshot_label("pre-turn", 1, Some("")), "pre-turn:1");
    }
}
