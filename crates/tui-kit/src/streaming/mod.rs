//! Provider-delta ingest for display-clock rendering.
//!
//! Provider deltas arrive as dozens of tiny SSE chunks. This module buffers
//! them without mutating the visible transcript ([`StreamBuffer`]) and bounds
//! both how often that buffer is flushed ([`StreamDisplayClock`]) and how much
//! each beat may uncover ([`reveal_budget`]), so the pace a reader sees is set
//! by the clock rather than by how the provider happened to chunk its output.
//!
//! Deltas are *input*, never animation timing. A commit beat reveals a bounded
//! slice at [`REVEAL_PER_SECOND`], so the pace a reader sees is the same
//! whatever shape the provider's chunks arrive in. There is still no
//! per-grapheme typewriter and no adaptive drain policy. A two-gear "adaptive
//! chunking" policy lived here until v0.9.4; it could never emit anything other
//! than "drain everything available", so it was deleted rather than wired up.
//! What replaced it is not that policy: it takes a measured slice per beat
//! instead of deciding between two gears.
//!
//! Buffering never changes what the model emitted. `accumulated_text` and
//! `accumulated_thinking` always track the full raw stream, so anything
//! building API messages or retrying sees the whole receipt regardless of how
//! much of it is currently visible.
//!
//! # Known limitations
//!
//! - **Latency is backlog over rate, not a fixed window.** The ceiling caps how
//!   fast bytes become visible, so a receipt larger than one beat's slice takes
//!   `backlog / REVEAL_PER_SECOND` to finish. A 4 KiB burst takes about 1.7 s to
//!   uncover. That is the deliberate trade (even pace over minimum latency) and
//!   it is not configurable.
//! - **A receipt that lands after a long pause is shown whole.** A pause
//!   longer than the beat interval leaves the next beat due immediately, and
//!   finalization takes a [`StreamDisplayClock::flush_now`] forced beat that
//!   drains everything buffered, bypassing pacing entirely — so very large
//!   receipts still land in one step.
//! - **Tool output does not pass through here.** It is unbuffered and has no
//!   pacing of its own.
//! - **Catch-up is staged, not wired.** [`CATCH_UP_QUEUE_DEPTH`] and
//!   [`CATCH_UP_OLDEST_AGE`] exist and are tested, but every production drain
//!   site calls `note_delta` with a queue depth of 1, so catch-up never fires.
//!   See the honesty note in `docs/MOTION_CONTRACT.md`.
//! - **Nothing measures this.** There is no benchmark and no budget for reveal
//!   throughput or visible latency; the numbers in the commit that introduced
//!   paced reveal came from a throwaway harness. See the runtime-perf-gate gap.
//!
//! Newline-boundary safety (never showing a half-written code fence) is owned
//! by the incremental markdown parser downstream — see
//! `ParseState::commit_complete_lines` in `tui/markdown_render.rs`, which
//! leaves the trailing partial line uncommitted and re-parses it each tick.
//! A separate `LineBuffer` gate used to sit here, but both constructors
//! bypassed it, so it protected nothing and was removed with the policy.

use std::time::Duration;
use std::time::Instant;

/// Default cadence for moving queued provider deltas into visible transcript
/// text. This intentionally tracks animation frames rather than upstream SSE
/// cadence, so tiny bursty deltas coalesce into one history/cache mutation.
///
/// ~60 FPS (16ms). Full motion may catch up sooner when backlog crosses
/// [`CATCH_UP_QUEUE_DEPTH`] / [`CATCH_UP_OLDEST_AGE`]; reduced motion never
/// accelerates — it stays on this steady clock (not a slow typewriter).
pub const DEFAULT_STREAM_COMMIT_INTERVAL: Duration = Duration::from_millis(16);

/// Queue-depth threshold that pulls the display clock forward (catch-up).
///
/// Staged, not live: every production drain site calls
/// [`StreamDisplayClock::note_delta`] (queued = 1), so catch-up never fires
/// today. See the honesty note in `docs/MOTION_CONTRACT.md`.
pub const CATCH_UP_QUEUE_DEPTH: usize = 160;

/// Oldest-chunk age that pulls the display clock forward (catch-up).
/// Staged alongside [`CATCH_UP_QUEUE_DEPTH`].
pub const CATCH_UP_OLDEST_AGE: Duration = Duration::from_millis(1_200);

/// Ceiling on how fast received text may become visible, in bytes per second.
///
/// This is the pace the reader sees. At the display clock's beat it is also the
/// size of one step, so the visible text advances by roughly the same amount
/// every beat instead of in provider-sized chunks.
///
/// Set several times a fast model's own output rate (a brisk stream is a few
/// hundred bytes per second): ordinary streaming is then limited by when the
/// bytes arrive, not by this ceiling, and receives no artificial delay. The
/// ceiling only spreads a burst that lands ahead of that rate.
pub const REVEAL_PER_SECOND: usize = 2_400;

/// How many bytes may become visible on a beat that is `interval` long, given
/// `backlog` bytes waiting.
#[must_use]
pub fn reveal_budget(interval: Duration, backlog: usize) -> usize {
    if backlog == 0 {
        return 0;
    }
    let per_beat = (REVEAL_PER_SECOND as u128 * interval.as_nanos() / 1_000_000_000) as usize;
    per_beat.max(1).min(backlog)
}

/// Frame-clock gate for stream display commits.
///
/// Provider deltas may arrive in dozens of tiny chunks inside one event-loop
/// drain. This clock lets the TUI ingest those bytes cheaply, then mutate the
/// visible transcript at most once per display beat unless the stream is being
/// finalized or measured backlog demands catch-up.
#[derive(Debug, Clone)]
pub struct StreamDisplayClock {
    interval: Duration,
    pending: bool,
    next_due_at: Option<Instant>,
    last_commit_at: Option<Instant>,
    commit_count: u64,
    catch_up_count: u64,
    /// When true, backlog may pull `next_due_at` forward to `now`.
    allow_catch_up: bool,
}

impl Default for StreamDisplayClock {
    fn default() -> Self {
        Self::new(DEFAULT_STREAM_COMMIT_INTERVAL)
    }
}

impl StreamDisplayClock {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            pending: false,
            next_due_at: None,
            last_commit_at: None,
            commit_count: 0,
            catch_up_count: 0,
            allow_catch_up: true,
        }
    }

    /// Enable/disable catch-up acceleration. Reduced motion keeps the steady
    /// clock and never pulls beats forward.
    pub fn set_allow_catch_up(&mut self, allow: bool) {
        self.allow_catch_up = allow;
    }

    /// Note that at least one stream delta is waiting to become visible.
    pub fn note_delta(&mut self, now: Instant) {
        self.note_delta_with_backlog(now, 1, None);
    }

    /// Note a delta together with measured queue pressure.
    ///
    /// Normal motion coalesces onto the steady interval unless backlog crosses
    /// the catch-up thresholds, in which case the next beat is due immediately.
    pub fn note_delta_with_backlog(
        &mut self,
        now: Instant,
        queued: usize,
        oldest_age: Option<Duration>,
    ) {
        self.pending = true;
        let catch_up = self.allow_catch_up
            && (queued >= CATCH_UP_QUEUE_DEPTH
                || oldest_age.is_some_and(|age| age >= CATCH_UP_OLDEST_AGE));

        if catch_up {
            self.next_due_at = Some(now);
            self.catch_up_count = self.catch_up_count.saturating_add(1);
            return;
        }

        if self.next_due_at.is_some() {
            return;
        }
        self.next_due_at = Some(match self.last_commit_at {
            Some(last) => last.checked_add(self.interval).unwrap_or(now).max(now),
            None => now,
        });
    }

    /// Returns the time until the pending commit is due, if any.
    pub fn due_in(&self, now: Instant) -> Option<Duration> {
        let due = self.next_due_at?;
        Some(due.saturating_duration_since(now))
    }

    /// Consume a due commit beat.
    pub fn take_due(&mut self, now: Instant) -> bool {
        if !self.pending {
            self.next_due_at = None;
            return false;
        }
        let Some(due) = self.next_due_at else {
            return false;
        };
        if now < due {
            return false;
        }
        self.pending = false;
        self.next_due_at = None;
        self.last_commit_at = Some(now);
        self.commit_count = self.commit_count.saturating_add(1);
        true
    }

    /// Force a commit beat, used when the stream is being finalized.
    pub fn flush_now(&mut self, now: Instant) -> bool {
        let had_pending = self.pending;
        self.pending = false;
        self.next_due_at = None;
        if had_pending {
            self.last_commit_at = Some(now);
            self.commit_count = self.commit_count.saturating_add(1);
        }
        had_pending
    }

    pub fn reset(&mut self) {
        self.pending = false;
        self.next_due_at = None;
        self.last_commit_at = None;
        self.commit_count = 0;
        self.catch_up_count = 0;
    }

    /// The configured beat length, so a caller can size work per beat.
    #[must_use]
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Number of commit beats consumed since the last reset (observability).
    #[cfg(test)]
    pub fn commit_count(&self) -> u64 {
        self.commit_count
    }

    /// Number of beats pulled forward by backlog since the last reset
    /// (observability; see the staged-catch-up note on
    /// [`CATCH_UP_QUEUE_DEPTH`]).
    #[cfg(test)]
    pub fn catch_up_count(&self) -> u64 {
        self.catch_up_count
    }
}

/// Buffers raw provider deltas between display-clock beats.
///
/// One buffer per active block (assistant / thinking). Tool output is
/// unbuffered and bypasses this path entirely. Each commit beat takes a
/// bounded slice ([`StreamBuffer::take_up_to`]) so the visible text advances
/// at a steady rate rather than in provider-sized steps.
#[derive(Debug, Default, Clone)]
pub struct StreamBuffer {
    pending: String,
}

impl StreamBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a raw model delta.
    pub fn push_delta(&mut self, delta: &str) {
        self.pending.push_str(delta);
    }

    /// Whether any text is waiting for the next commit beat.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Take at most `budget` bytes, leaving the rest for a later beat.
    ///
    /// The cut lands on a grapheme boundary: slicing through a cluster would
    /// flash a broken glyph for one beat. A budget too small to hold the next
    /// cluster still takes that cluster whole, so a drain can never stall.
    pub fn take_up_to(&mut self, budget: usize) -> String {
        use unicode_segmentation::UnicodeSegmentation;
        if budget >= self.pending.len() {
            return std::mem::take(&mut self.pending);
        }
        let mut end = budget.min(self.pending.len());
        while end > 0 && !self.pending.is_char_boundary(end) {
            end -= 1;
        }
        if end > 0 {
            end = self
                .pending
                .grapheme_indices(true)
                .map(|(index, _)| index)
                .take_while(|index| *index <= end)
                .last()
                .unwrap_or(0);
        }
        if end == 0 {
            end = self.pending.graphemes(true).next().map_or(0, str::len);
        }
        let rest = self.pending.split_off(end);
        std::mem::replace(&mut self.pending, rest)
    }

    /// Bytes waiting for a later beat.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Take everything buffered since the previous beat.
    pub fn take(&mut self) -> String {
        std::mem::take(&mut self.pending)
    }
}

/// Per-block streaming substate.
///
/// ```text
/// raw delta -> StreamBuffer.push_delta -> commit beat -> take -> transcript
/// ```
#[derive(Debug, Default)]
struct BlockState {
    /// Thinking blocks route to `accumulated_thinking`; text blocks to
    /// `accumulated_text`.
    is_thinking: bool,
    /// Cleared once the block has been finalized.
    is_streaming: bool,
    /// Deltas received but not yet flushed to the transcript.
    buffer: StreamBuffer,
}

/// State for managing multiple stream buffers (one per content block)
#[derive(Debug, Default)]
pub struct StreamingState {
    /// Per-block state by index.
    blocks: Vec<Option<BlockState>>,
    /// Whether any stream is currently active
    pub is_active: bool,
    /// Accumulated text for display
    pub accumulated_text: String,
    /// Accumulated thinking for display
    pub accumulated_thinking: String,
}

impl StreamingState {
    /// Create a new streaming state
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new text block. Assistant prose is buffered until the next
    /// display-clock beat so provider bursts produce one visible mutation.
    pub fn start_text(&mut self, index: usize) {
        self.start_block(index, false);
    }

    /// Start a new thinking block. Thinking deltas are buffered exactly like
    /// assistant prose — long reasoning often arrives as one paragraph with no
    /// intermediate newlines, so nothing here waits on a line boundary.
    pub fn start_thinking(&mut self, index: usize) {
        self.start_block(index, true);
    }

    fn start_block(&mut self, index: usize, is_thinking: bool) {
        self.ensure_capacity(index);
        self.blocks[index] = Some(BlockState {
            is_thinking,
            is_streaming: true,
            buffer: StreamBuffer::new(),
        });
        self.is_active = true;
    }

    /// Push content to a block.
    ///
    /// `accumulated_text` / `accumulated_thinking` always track the full raw
    /// stream so callers building API messages or doing retries see exactly
    /// what the model emitted, regardless of UI pacing.
    pub fn push_content(&mut self, index: usize, content: &str) {
        if let Some(Some(block)) = self.blocks.get_mut(index) {
            if block.is_thinking {
                self.accumulated_thinking.push_str(content);
            } else {
                self.accumulated_text.push_str(content);
            }
            block.buffer.push_delta(content);
        }
    }

    /// Run one commit beat and return the text to flush to the transcript,
    /// taking at most `budget` bytes. Empty when nothing arrived since the
    /// previous beat or the budget admits none.
    pub fn commit_text(&mut self, index: usize, budget: usize) -> String {
        match self.blocks.get_mut(index) {
            Some(Some(block)) => block.buffer.take_up_to(budget),
            _ => String::new(),
        }
    }

    /// Whether the block at `index` carries reasoning text.
    ///
    /// Display beats must route a committed delta by the block's own kind.
    /// An assistant cell can already be live when a later thinking block
    /// starts inside the same response, so the mere presence of a streaming
    /// message index does not say which cell the delta belongs to (#7123).
    pub fn block_is_thinking(&self, index: usize) -> bool {
        matches!(self.blocks.get(index), Some(Some(block)) if block.is_thinking)
    }

    /// Bytes waiting in a block's buffer.
    pub fn pending_len(&self, index: usize) -> usize {
        self.blocks
            .get(index)
            .and_then(|b| b.as_ref())
            .map_or(0, |b| b.buffer.pending_len())
    }

    /// Whether a block holds text waiting to be flushed by the next commit
    /// beat. Callers use this to keep the display clock ticking while a
    /// buffer drains.
    pub fn has_pending_stream_text(&self, index: usize) -> bool {
        self.blocks
            .get(index)
            .and_then(|b| b.as_ref())
            .is_some_and(|b| b.buffer.has_pending())
    }

    /// Finalize a block and return whatever text it still holds.
    pub fn finalize_block_text(&mut self, index: usize) -> String {
        let out = match self.blocks.get_mut(index) {
            Some(Some(block)) => {
                block.is_streaming = false;
                block.buffer.take()
            }
            _ => return String::new(),
        };
        self.check_active();
        out
    }

    /// Check if any stream is still active
    fn check_active(&mut self) {
        self.is_active = self.blocks.iter().flatten().any(|b| b.is_streaming);
    }

    /// Ensure capacity for the given index
    fn ensure_capacity(&mut self, index: usize) {
        while self.blocks.len() <= index {
            self.blocks.push(None);
        }
    }

    /// Reset the streaming state
    pub fn reset(&mut self) {
        self.blocks.clear();
        self.is_active = false;
        self.accumulated_text.clear();
        self.accumulated_thinking.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_text_streams_before_newline() {
        let mut state = StreamingState::new();
        state.start_text(0);
        state.push_content(0, "hello world");

        assert_eq!(state.commit_text(0, usize::MAX), "hello world");
        assert!(!state.has_pending_stream_text(0));
    }

    #[test]
    fn thinking_text_streams_before_newline() {
        let mut state = StreamingState::new();
        state.start_thinking(0);
        state.push_content(0, "thinking deeply");

        assert_eq!(state.commit_text(0, usize::MAX), "thinking deeply");
        assert!(!state.has_pending_stream_text(0));
    }

    #[test]
    fn commit_beat_drains_everything_received_since_the_previous_beat() {
        // A burst arriving "at once" is displayed at the same cadence instead
        // of being synthetically dripped and flushed at end of turn.
        let mut state = StreamingState::new();
        state.start_text(0);

        let burst = "abcdefghijklmnopqrstuvwxyz".repeat(8);
        state.push_content(0, &burst);
        assert_eq!(state.commit_text(0, usize::MAX), burst);
        // Second beat with nothing new is empty, not a replay.
        assert_eq!(state.commit_text(0, usize::MAX), "");
    }

    #[test]
    fn combining_marks_stay_with_their_base_letter() {
        let mut state = StreamingState::new();
        state.start_text(0);
        state.push_content(0, "e\u{301}x");
        assert_eq!(state.commit_text(0, usize::MAX), "e\u{301}x");
    }

    #[test]
    fn finalize_drains_partial_tail() {
        // The final, possibly-unterminated line must survive finalization.
        let mut state = StreamingState::new();
        state.start_text(0);
        state.push_content(0, "done\nno-newline-here");
        assert_eq!(state.finalize_block_text(0), "done\nno-newline-here");
        assert!(!state.is_active);
    }

    #[test]
    fn finalize_after_commit_has_nothing_left() {
        let mut state = StreamingState::new();
        state.start_text(0);
        state.push_content(0, "abc");
        assert_eq!(state.commit_text(0, usize::MAX), "abc");
        assert_eq!(state.finalize_block_text(0), "");
    }

    #[test]
    fn accumulators_track_raw_stream_by_block_kind() {
        let mut state = StreamingState::new();
        state.start_thinking(0);
        state.push_content(0, "reasoning");
        state.start_text(1);
        state.push_content(1, "answer");

        assert_eq!(state.accumulated_thinking, "reasoning");
        assert_eq!(state.accumulated_text, "answer");
    }

    #[test]
    fn bursty_stream_state_has_no_text_loss_after_coalesced_flushes() {
        let mut state = StreamingState::new();
        state.start_text(0);
        let mut expected = String::new();

        for idx in 0..250 {
            let chunk = format!("{idx}.");
            expected.push_str(&chunk);
            state.push_content(0, &chunk);
        }

        let first_flush = state.commit_text(0, usize::MAX);
        assert_eq!(first_flush, expected);
        assert_eq!(state.finalize_block_text(0), "");
    }

    #[test]
    fn stream_display_clock_coalesces_bursty_tiny_deltas() {
        let interval = Duration::from_millis(33);
        let mut clock = StreamDisplayClock::new(interval);
        let t0 = Instant::now();

        for _ in 0..100 {
            clock.note_delta(t0);
        }

        assert_eq!(clock.due_in(t0), Some(Duration::ZERO));
        assert!(clock.take_due(t0));
        assert_eq!(clock.commit_count(), 1);

        for _ in 0..25 {
            clock.note_delta(t0 + Duration::from_millis(5));
        }
        assert!(!clock.take_due(t0 + Duration::from_millis(5)));
        assert_eq!(
            clock.due_in(t0 + Duration::from_millis(5)),
            Some(Duration::from_millis(28))
        );
        assert!(clock.take_due(t0 + interval));
        assert_eq!(clock.commit_count(), 2);
    }

    #[test]
    fn stream_display_clock_bounds_long_reasoning_commit_count() {
        let interval = Duration::from_millis(33);
        let mut clock = StreamDisplayClock::new(interval);
        let t0 = Instant::now();
        let mut commits = 0u64;

        for millis in 0..300 {
            let now = t0 + Duration::from_millis(millis);
            clock.note_delta(now);
            if clock.take_due(now) {
                commits += 1;
            }
        }

        assert!(commits > 1, "long streams should keep advancing visibly");
        assert!(
            commits <= 11,
            "300 one-ms deltas should not commit on provider cadence: {commits}"
        );
        assert_eq!(commits, clock.commit_count());
    }

    #[test]
    fn stream_display_clock_final_flush_consumes_pending_delta() {
        let mut clock = StreamDisplayClock::new(Duration::from_millis(33));
        let t0 = Instant::now();

        clock.note_delta(t0);
        assert!(clock.take_due(t0));
        clock.note_delta(t0 + Duration::from_millis(4));

        assert!(!clock.take_due(t0 + Duration::from_millis(4)));
        assert!(clock.flush_now(t0 + Duration::from_millis(5)));
        assert_eq!(clock.due_in(t0 + Duration::from_millis(5)), None);
        assert!(!clock.take_due(t0 + Duration::from_millis(33)));
        assert_eq!(clock.commit_count(), 2);
    }

    #[test]
    fn normal_clock_catch_up_only_when_backlog_crosses_threshold() {
        let interval = Duration::from_millis(33);
        let mut clock = StreamDisplayClock::new(interval);
        let t0 = Instant::now();

        clock.note_delta(t0);
        assert!(clock.take_due(t0));
        // Small backlog after a commit stays on the steady interval.
        clock.note_delta_with_backlog(
            t0 + Duration::from_millis(1),
            3,
            Some(Duration::from_millis(5)),
        );
        assert!(!clock.take_due(t0 + Duration::from_millis(1)));
        assert_eq!(clock.catch_up_count(), 0);

        // Measured backlog crosses the catch-up threshold → due immediately.
        clock.note_delta_with_backlog(
            t0 + Duration::from_millis(2),
            CATCH_UP_QUEUE_DEPTH,
            Some(Duration::from_millis(10)),
        );
        assert!(clock.take_due(t0 + Duration::from_millis(2)));
        assert!(clock.catch_up_count() >= 1);
    }

    #[test]
    fn reduced_motion_keeps_steady_clock_without_catch_up_or_typewriter() {
        let interval = Duration::from_millis(33);
        let mut clock = StreamDisplayClock::new(interval);
        clock.set_allow_catch_up(false);
        let t0 = Instant::now();

        clock.note_delta(t0);
        assert!(clock.take_due(t0));
        clock.note_delta_with_backlog(
            t0 + Duration::from_millis(1),
            CATCH_UP_QUEUE_DEPTH * 2,
            Some(CATCH_UP_OLDEST_AGE),
        );
        // Reduced motion must not pull the beat forward.
        assert!(!clock.take_due(t0 + Duration::from_millis(1)));
        assert_eq!(clock.catch_up_count(), 0);
        assert_eq!(
            clock.due_in(t0 + Duration::from_millis(1)),
            Some(Duration::from_millis(32))
        );
        assert!(clock.take_due(t0 + interval));
        // Same interval as full motion — not a slower artificial typewriter.
        assert_eq!(clock.commit_count(), 2);
    }

    #[test]
    fn reveal_budget_is_a_steady_step_and_always_progresses() {
        let interval = DEFAULT_STREAM_COMMIT_INTERVAL;
        let full = reveal_budget(interval, usize::MAX);
        assert_eq!(full, REVEAL_PER_SECOND * 16 / 1000);
        // A backlog smaller than one step is taken whole, so a small receipt
        // is never slowed down by the ceiling.
        assert_eq!(reveal_budget(interval, 3), 3);
        // Nothing waiting means nothing to reveal.
        assert_eq!(reveal_budget(interval, 0), 0);
        // A budget too small to hold a character still admits progress.
        assert!(reveal_budget(Duration::from_nanos(1), 10_000) >= 1);
    }

    #[test]
    fn a_burst_is_spread_across_beats_instead_of_landing_at_once() {
        // The defect this guards: the whole buffer used to move to the
        // transcript on the first beat, so the reader saw provider-sized jumps.
        let interval = DEFAULT_STREAM_COMMIT_INTERVAL;
        let burst = "x".repeat(2_000);
        let mut state = StreamingState::default();
        state.start_text(0);
        state.push_content(0, &burst);

        let step = reveal_budget(interval, state.pending_len(0));
        assert!(
            step < burst.len() / 4,
            "one beat took {step} of {} bytes",
            burst.len()
        );

        let mut revealed = String::new();
        for beat in 0..200 {
            let budget = reveal_budget(interval, state.pending_len(0));
            let taken = state.commit_text(0, budget);
            assert!(taken.len() <= budget, "beat {beat} overspent its budget");
            revealed.push_str(&taken);
            if !state.has_pending_stream_text(0) {
                break;
            }
        }
        assert_eq!(revealed, burst, "every byte must arrive, in order");
    }

    #[test]
    fn take_up_to_never_splits_a_character_and_always_progresses() {
        let mut buffer = StreamBuffer::new();
        buffer.push_delta("a👩🏽‍💻b");
        // A budget that cannot hold the emoji still yields it whole rather
        // than stalling the drain forever.
        assert_eq!(buffer.take_up_to(1), "a");
        assert_eq!(buffer.take_up_to(1), "👩🏽‍💻");
        assert!(buffer.has_pending());
        assert_eq!(buffer.take_up_to(usize::MAX), "b");
        assert!(!buffer.has_pending());
    }
}

/// Runtime performance gate for the reveal path (#6193 first slice).
#[cfg(test)]
#[path = "tests/perf_gate.rs"]
mod perf_gate;
