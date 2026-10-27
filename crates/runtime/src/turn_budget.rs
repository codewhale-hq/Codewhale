//! Turn budgets shared by interactive hosts and headless execution.
//!
//! Model steps and the cumulative per-turn wall clock are uncapped by
//! default. Explicit positive limits still apply; neither a step counter nor
//! elapsed time is a measure of useful progress, and a long autonomous turn
//! should not stop at an arbitrary hour. Per-step stream budgets remain
//! finite: they bound one stuck request, not the whole turn.
//!
//! `EngineConfig` retains its integer representation for embedders:
//! `u32::MAX` represents no model-step limit. `TurnContext::step_limit`
//! resolves that representation to `None` before checking a ceiling or
//! emitting diagnostics. It is not a very large finite fallback.
//!
//! ## Honesty at the limit
//!
//! Hitting a budget is never a clean success. The step ceiling already ends
//! the turn as `TurnOutcomeStatus::Failed` with the limit named (or, when
//! the model still owes work, grants exactly one bounded final-report turn
//! first). [`TurnWallClock`] follows the same contract in `run_turn`.

use std::time::{Duration, Instant};

/// No model-step limit unless the caller configures one. This is the
/// compatibility representation, not a ceiling checked at `u32::MAX`.
pub const DEFAULT_MAX_MODEL_STEPS: u32 = u32::MAX;

/// No per-turn wall-clock limit unless the caller configures one.
///
/// `Duration::MAX` is the representation, mirroring
/// [`DEFAULT_MAX_MODEL_STEPS`]: [`TurnWallClock::exhausted`] can never reach
/// it, and callers turning it into an absolute deadline must use
/// `checked_add` (see `exec_agent`). When configured, the budget is measured
/// across every model step of one turn, not per request, and time blocked on
/// a human approval decision is excluded (see
/// [`TurnWallClock::begin_human_wait`]).
pub const DEFAULT_TURN_WALL_CLOCK: Duration = Duration::MAX;

/// Default per-step cap on accumulated streamed content, in bytes: the total
/// text, reasoning and tool-argument content before aborting the stream.
/// Preserves the pre-R1 hard-coded value; R1 only makes it overridable.
pub const DEFAULT_STREAM_MAX_CONTENT_BYTES: usize = 10 * 1024 * 1024;
/// Smallest accepted per-step stream content cap (64 KiB).
pub const MIN_STREAM_MAX_CONTENT_BYTES: usize = 64 * 1024;
/// Largest accepted per-step stream content cap (512 MiB).
pub const MAX_STREAM_MAX_CONTENT_BYTES: usize = 512 * 1024 * 1024;

/// Default per-step cap on a single stream's wall-clock duration, in
/// seconds. Preserves the pre-R1 hard-coded value.
///
/// Sanity backstop for total stream wall-clock duration. **Not** a routine
/// kill switch — the stream chunk idle timeout is the primary stall
/// detector. The wall-clock cap is here only to bound pathological cases
/// (e.g. a server that keeps sending heartbeats forever without progress).
///
/// History: this used to be 300s (5 min) which was too aggressive — V4
/// thinking turns on hard prompts legitimately exceed 5 minutes wall-clock
/// while still emitting reasoning_content chunks the whole way. Bumped to
/// 30 min in v0.6.6 after long-reasoning turns hit the old cap. Codex defaults to a
/// per-chunk idle of 300s with no wall-clock cap; we keep both layers but
/// give the wall-clock a generous window so it never fires in practice.
pub const DEFAULT_STREAM_MAX_DURATION_SECS: u64 = 1800;
/// Smallest accepted per-step stream duration cap.
pub const MIN_STREAM_MAX_DURATION_SECS: u64 = 10;
/// Largest accepted per-step stream duration cap (24 hours).
pub const MAX_STREAM_MAX_DURATION_SECS: u64 = 86_400;

/// Default whole-request resume budget after a failed stream (open failure,
/// dead stream, sleep, or mid-stream network drop). Shared by the
/// nothing-streamed outer retry (#103 Phase 3), the sleep-resume retry
/// (#2990), the network-drop resumes, and stream-open failures (#6699).
/// Overridable via `[tui].stream_max_resumes` (#6700).
pub const DEFAULT_STREAM_MAX_RESUMES: u32 = 3;
/// Largest accepted resume budget. `0` is accepted and disables resumes.
pub const MAX_STREAM_MAX_RESUMES: u32 = 10;
/// Default in-stream transparent retry budget. These retries only happen
/// when the wire dies before any content was streamed. The user has seen
/// nothing, but provider usage or billing may already exist. Two attempts
/// can ride out a flaky edge node without amplifying real outages (#103).
/// `[tui].stream_max_transparent_retries` overrides it (#6700).
pub const DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES: u32 = 2;
/// Largest accepted transparent retry budget. `0` disables them.
pub const MAX_STREAM_MAX_TRANSPARENT_RETRIES: u32 = 10;
/// Default streak of recoverable stream errors tolerated in one stream before
/// the turn fails. Bumped 3 → 5 in v0.6.7 along with the HTTP/2 keepalive
/// defaults (#103) — keepalive should make spurious decode errors rarer, so
/// we can tolerate a longer streak before giving up on the turn.
/// `[tui].stream_max_errors` overrides it (#6700).
pub const DEFAULT_STREAM_MAX_ERRORS: u32 = 5;
/// Smallest accepted error streak: the first error ends the stream. `0` is
/// not a value here; like the other finite stream budgets it selects
/// [`DEFAULT_STREAM_MAX_ERRORS`].
pub const MIN_STREAM_MAX_ERRORS: u32 = 1;
/// Largest accepted error streak.
pub const MAX_STREAM_MAX_ERRORS: u32 = 50;

/// Stream-level retry budgets for one engine (#6700). Every field stays
/// finite; the defaults are the historical compiled-in values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamRetryLimits {
    /// Whole-request re-issues after a failed stream, shared by every
    /// resume path and by stream-open failures (#6699).
    pub max_resumes: u32,
    /// In-stream re-requests while nothing has streamed yet (#103).
    pub max_transparent_retries: u32,
    /// Recoverable errors tolerated in one stream before it ends.
    pub max_errors: u32,
}

impl Default for StreamRetryLimits {
    fn default() -> Self {
        resolve_stream_retry_limits(None, None, None)
    }
}

/// Resolve configured stream retry budgets.
///
/// `None` selects each default. The two retry counts accept `0` (no
/// retries) and clamp to their maximum. The error streak is a finite stream
/// budget like `stream_max_content_mb`: `0` selects its default, and other
/// values clamp to `MIN_STREAM_MAX_ERRORS..=MAX_STREAM_MAX_ERRORS`.
#[must_use]
pub fn resolve_stream_retry_limits(
    max_resumes: Option<u32>,
    max_transparent_retries: Option<u32>,
    max_errors: Option<u32>,
) -> StreamRetryLimits {
    StreamRetryLimits {
        max_resumes: max_resumes
            .unwrap_or(DEFAULT_STREAM_MAX_RESUMES)
            .min(MAX_STREAM_MAX_RESUMES),
        max_transparent_retries: max_transparent_retries
            .unwrap_or(DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES)
            .min(MAX_STREAM_MAX_TRANSPARENT_RETRIES),
        max_errors: match max_errors {
            None | Some(0) => DEFAULT_STREAM_MAX_ERRORS,
            Some(value) => value.clamp(MIN_STREAM_MAX_ERRORS, MAX_STREAM_MAX_ERRORS),
        },
    }
}

/// Resolve a configured model-step ceiling.
///
/// `None` and `0` select the uncapped default. Positive values are preserved;
/// `u32::MAX` remains the compatibility representation for no limit.
#[must_use]
pub fn resolve_max_model_steps(raw: Option<u32>) -> u32 {
    match raw {
        None | Some(0) => DEFAULT_MAX_MODEL_STEPS,
        Some(value) => value,
    }
}

/// Resolve a configured per-turn wall-clock budget.
///
/// `None` and `0` select the uncapped default ([`DEFAULT_TURN_WALL_CLOCK`]).
/// Explicit positive values, in seconds, are preserved.
#[must_use]
pub fn resolve_turn_wall_clock(raw: Option<u64>) -> Duration {
    match raw {
        None | Some(0) => DEFAULT_TURN_WALL_CLOCK,
        Some(value) => Duration::from_secs(value),
    }
}

/// Resolve a configured per-step stream content cap, given megabytes.
///
/// `None` and `0` both resolve to [`DEFAULT_STREAM_MAX_CONTENT_BYTES`].
#[must_use]
pub fn resolve_stream_max_content_bytes(raw_mb: Option<u64>) -> usize {
    match raw_mb {
        None | Some(0) => DEFAULT_STREAM_MAX_CONTENT_BYTES,
        Some(mb) => usize::try_from(mb.saturating_mul(1024 * 1024))
            .unwrap_or(MAX_STREAM_MAX_CONTENT_BYTES)
            .clamp(MIN_STREAM_MAX_CONTENT_BYTES, MAX_STREAM_MAX_CONTENT_BYTES),
    }
}

/// Resolve a configured per-step stream duration cap, in seconds.
///
/// `None` and `0` both resolve to [`DEFAULT_STREAM_MAX_DURATION_SECS`].
#[must_use]
pub fn resolve_stream_max_duration_secs(raw: Option<u64>) -> u64 {
    match raw {
        None | Some(0) => DEFAULT_STREAM_MAX_DURATION_SECS,
        Some(value) => value.clamp(MIN_STREAM_MAX_DURATION_SECS, MAX_STREAM_MAX_DURATION_SECS),
    }
}

/// Cumulative wall-clock budget for one turn.
///
/// Started once at the top of `Engine::run_turn` and checked at the
/// provider-request boundary, so a turn that trips the budget stops before
/// authorizing another billable request and keeps every tool result already
/// in the transcript.
///
/// Time blocked on a human approval decision is excluded: the budget bounds
/// what the agent spends on its own, not how long a person takes to answer.
/// Without that exclusion an approval prompt left open overnight would fail
/// the turn — and discard the work the user just approved — the moment they
/// came back.
#[derive(Debug)]
pub struct TurnWallClock {
    budget: Duration,
    started_at: Instant,
    /// Total time already excluded because the turn was blocked on a human.
    excluded: Duration,
    /// Set while currently blocked on a human decision.
    blocked_since: Option<Instant>,
}

impl TurnWallClock {
    /// Start a fresh budget. A zero budget is legal here (and only here):
    /// it is how tests assert the stop path without sleeping. Configuration
    /// never produces one — [`resolve_turn_wall_clock`] reads `0` as no limit.
    pub fn start(budget: Duration) -> Self {
        Self {
            budget,
            started_at: Instant::now(),
            excluded: Duration::ZERO,
            blocked_since: None,
        }
    }

    /// The budget this clock was started with.
    pub fn budget(&self) -> Duration {
        self.budget
    }

    /// Wall-clock time this turn has spent on its own work, excluding time
    /// blocked on a human decision.
    pub fn spent(&self) -> Duration {
        let blocked_now = self
            .blocked_since
            .map_or(Duration::ZERO, |since| since.elapsed());
        self.started_at
            .elapsed()
            .saturating_sub(self.excluded)
            .saturating_sub(blocked_now)
    }

    /// Whether the cumulative budget is spent.
    pub fn exhausted(&self) -> bool {
        self.spent() >= self.budget
    }

    /// Stop counting: the turn is now waiting on a human decision.
    /// Idempotent — a second call while already blocked does nothing, so a
    /// nested or re-entered approval cannot double-exclude.
    pub fn begin_human_wait(&mut self) {
        if self.blocked_since.is_none() {
            self.blocked_since = Some(Instant::now());
        }
    }

    /// Resume counting after a human decision, banking the blocked time.
    pub fn end_human_wait(&mut self) {
        if let Some(since) = self.blocked_since.take() {
            self.excluded = self.excluded.saturating_add(since.elapsed());
        }
    }

    /// Test-only: pretend `elapsed` more wall-clock time has passed, so the
    /// exhaustion path can be exercised without sleeping. An in-progress
    /// human wait is rewound too — otherwise the simulated time would count
    /// as agent-owned work that never actually happened.
    #[cfg(any(test, feature = "test-support"))]
    pub fn rewind_for_test(&mut self, elapsed: Duration) {
        self.started_at = self
            .started_at
            .checked_sub(elapsed)
            .unwrap_or(self.started_at);
        if let Some(since) = self.blocked_since {
            self.blocked_since = Some(since.checked_sub(elapsed).unwrap_or(since));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_step_defaults_do_not_install_a_ceiling() {
        assert_eq!(resolve_max_model_steps(None), DEFAULT_MAX_MODEL_STEPS);
        assert_eq!(resolve_max_model_steps(Some(0)), DEFAULT_MAX_MODEL_STEPS);
        assert_eq!(DEFAULT_MAX_MODEL_STEPS, u32::MAX);
        assert_eq!(resolve_max_model_steps(Some(u32::MAX)), u32::MAX);
    }

    #[test]
    fn model_step_ceiling_preserves_the_owner_limit() {
        assert_eq!(resolve_max_model_steps(Some(7)), 7);
        assert_eq!(resolve_max_model_steps(Some(1)), 1);
        assert_eq!(
            resolve_max_model_steps(Some(200_000)),
            200_000,
            "explicit positive overrides retain the configured ceiling"
        );
    }

    #[test]
    fn turn_wall_clock_defaults_to_no_limit() {
        assert_eq!(resolve_turn_wall_clock(None), DEFAULT_TURN_WALL_CLOCK);
        assert_eq!(resolve_turn_wall_clock(Some(0)), DEFAULT_TURN_WALL_CLOCK);
        let mut clock = TurnWallClock::start(resolve_turn_wall_clock(None));
        clock.rewind_for_test(Duration::from_secs(10 * 24 * 60 * 60));
        assert!(!clock.exhausted(), "the default never stops a turn");
    }

    /// Code mode and headless exec sleep toward what is left of the default
    /// budget; a near-`Duration::MAX` sleep must park, not panic.
    #[tokio::test]
    async fn an_unbounded_remaining_budget_is_a_safe_sleep() {
        let remaining = DEFAULT_TURN_WALL_CLOCK.saturating_sub(Duration::from_secs(1));
        let woke =
            tokio::time::timeout(Duration::from_millis(20), tokio::time::sleep(remaining)).await;
        assert!(woke.is_err(), "the sleep parks until cancelled");
        let started = std::time::Instant::now();
        assert!(started.checked_add(remaining).is_none());
    }

    #[test]
    fn turn_wall_clock_preserves_the_owner_limit() {
        assert_eq!(resolve_turn_wall_clock(Some(120)), Duration::from_secs(120));
        assert_eq!(resolve_turn_wall_clock(Some(1)), Duration::from_secs(1));
        assert_eq!(
            resolve_turn_wall_clock(Some(u64::MAX)),
            Duration::from_secs(u64::MAX)
        );
    }

    #[test]
    fn stream_caps_default_reject_zero_and_are_overridable() {
        assert_eq!(
            resolve_stream_max_content_bytes(None),
            DEFAULT_STREAM_MAX_CONTENT_BYTES
        );
        assert_eq!(
            resolve_stream_max_content_bytes(Some(0)),
            DEFAULT_STREAM_MAX_CONTENT_BYTES
        );
        assert_eq!(resolve_stream_max_content_bytes(Some(1)), 1024 * 1024);
        assert_eq!(
            resolve_stream_max_content_bytes(Some(u64::MAX)),
            MAX_STREAM_MAX_CONTENT_BYTES
        );

        assert_eq!(
            resolve_stream_max_duration_secs(None),
            DEFAULT_STREAM_MAX_DURATION_SECS
        );
        assert_eq!(
            resolve_stream_max_duration_secs(Some(0)),
            DEFAULT_STREAM_MAX_DURATION_SECS
        );
        assert_eq!(resolve_stream_max_duration_secs(Some(60)), 60);
        assert_eq!(
            resolve_stream_max_duration_secs(Some(u64::MAX)),
            MAX_STREAM_MAX_DURATION_SECS
        );
    }

    #[test]
    fn stream_retry_limits_default_to_historical_values_and_clamp() {
        let defaults = resolve_stream_retry_limits(None, None, None);
        assert_eq!(defaults, StreamRetryLimits::default());
        assert_eq!(defaults.max_resumes, 3);
        assert_eq!(defaults.max_transparent_retries, 2);
        assert_eq!(defaults.max_errors, 5);

        let disabled = resolve_stream_retry_limits(Some(0), Some(0), Some(0));
        assert_eq!(disabled.max_resumes, 0, "0 disables resumes");
        assert_eq!(disabled.max_transparent_retries, 0);
        assert_eq!(
            disabled.max_errors, DEFAULT_STREAM_MAX_ERRORS,
            "0 selects the error-streak default, like the other finite stream budgets"
        );
        assert_eq!(
            resolve_stream_retry_limits(None, None, Some(1)).max_errors,
            MIN_STREAM_MAX_ERRORS
        );

        let huge = resolve_stream_retry_limits(Some(u32::MAX), Some(u32::MAX), Some(u32::MAX));
        assert_eq!(huge.max_resumes, MAX_STREAM_MAX_RESUMES);
        assert_eq!(
            huge.max_transparent_retries,
            MAX_STREAM_MAX_TRANSPARENT_RETRIES
        );
        assert_eq!(huge.max_errors, MAX_STREAM_MAX_ERRORS);
    }

    #[test]
    fn wall_clock_exhausts_once_the_budget_is_spent() {
        let mut clock = TurnWallClock::start(Duration::from_secs(60));
        assert!(!clock.exhausted());
        clock.rewind_for_test(Duration::from_secs(61));
        assert!(clock.exhausted());
        assert!(clock.spent() >= clock.budget());
    }

    #[test]
    fn wall_clock_excludes_time_blocked_on_a_human_decision() {
        let mut clock = TurnWallClock::start(Duration::from_secs(60));
        clock.begin_human_wait();
        // The human took two minutes; the agent spent none of its budget.
        clock.rewind_for_test(Duration::from_secs(120));
        assert!(
            !clock.exhausted(),
            "an unanswered approval prompt must not burn the turn budget"
        );
        clock.end_human_wait();
        assert!(!clock.exhausted());
        // Agent-owned time after the decision still counts.
        clock.rewind_for_test(Duration::from_secs(61));
        assert!(clock.exhausted());
    }

    #[test]
    fn nested_human_waits_cannot_double_exclude() {
        let mut clock = TurnWallClock::start(Duration::from_secs(60));
        clock.begin_human_wait();
        clock.begin_human_wait();
        clock.rewind_for_test(Duration::from_secs(30));
        clock.end_human_wait();
        // A second unmatched end is a no-op, not another exclusion.
        clock.end_human_wait();
        clock.rewind_for_test(Duration::from_secs(61));
        assert!(clock.exhausted());
    }
}
