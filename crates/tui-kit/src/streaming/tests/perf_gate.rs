//! Runtime performance gate for the streaming reveal path (#6193, first
//! slice).
//!
//! This module removes the "Nothing measures this" limitation from the
//! streaming module doc for the reveal path: fixed synthetic inputs built
//! from reviewed constants drive the REAL [`StreamDisplayClock`] and the
//! real block buffers, and the verdicts are deterministic integer gates —
//! no wall clock anywhere except the single macOS reference-lane test at
//! the bottom.
//!
//! Inputs are never recorded sessions, never ambient repositories, never
//! the network: every byte is generated from `'a'..='z'` cycles and the
//! production constants in the parent module. Budgets below are reviewed
//! source constants — they are deliberately NOT overridable by environment
//! variables, because a budget an env var can raise is not a budget.
//!
//! How each verdict is reached:
//!
//! - `max_bytes_per_beat` — absolute bound: every beat's committed byte
//!   count must be `<= reveal_budget(interval, backlog_at_beat_start)`
//!   computed with the production constants, across a backlog sweep at two
//!   clock intervals. Pure ASCII filler keeps one grapheme == one byte, so
//!   the bound is exact with no boundary rounding.
//! - `beats_to_first_visible` — absolute count: a delta pushed at t0 must
//!   become visible on the first due beat (never zero beats = invisible,
//!   never two = manufactured lag).
//! - `beats_to_drain` — exact integer ceiling: beats until a backlog is
//!   fully revealed must equal `backlog.div_ceil(per_beat_budget)` with
//!   `per_beat_budget = reveal_budget(interval, usize::MAX)`.
//! - cell-kind digest — fixed expected sequence: the (kind, bytes)
//!   sequence committed per beat for a mixed thinking/text stream must
//!   match the digest literal below; a beat committing the wrong cell,
//!   duplicating a cell, or dropping bytes changes the digest.
//! - `flush_now` after a long pause — absolute count: the whole backlog
//!   lands in exactly one forced step, as the module doc promises.

use std::time::Duration;
use std::time::Instant;

use super::DEFAULT_STREAM_COMMIT_INTERVAL;
use super::REVEAL_PER_SECOND;
use super::StreamDisplayClock;
use super::StreamingState;

/// Slow-clock variant for the sweep gates: 100 ms beats. At
/// [`REVEAL_PER_SECOND`] this admits 240 bytes per beat, so the same
/// budgets must hold on a cadence four times slower than production.
const SLOW_CLOCK_INTERVAL: Duration = Duration::from_millis(100);

/// Backlog sweep for the per-beat and drain gates (bytes):
///
/// 1. `1` — the smallest possible receipt;
/// 2. one beat's budget at the production interval (38 bytes:
///    `2_400 B/s * 16 ms`, truncated);
/// 3. `4 KiB` / `64 KiB` / `256 KiB` — burst shapes a fast model emits
///    when it front-loads a long answer.
///
/// Reviewed 2026-09-15; not configurable.
const SWEEP_BACKLOG_BYTES: [usize; 4] = [1, 38, 4 * 1024, 64 * 1024];

/// Largest burst in the sweep, kept as a named constant so the
/// flush-after-pause gate and the sweep cannot drift apart.
const LARGEST_BURST_BYTES: usize = 256 * 1024;

/// Beat bookkeeping cycles measured by the macOS reference lane.
/// 10_000 note_delta + take_due + budgeted take cycles is ~160 s of
/// virtual stream time — a full long-form answer's worth of beats.
/// Reviewed 2026-09-15; not configurable.
#[cfg(target_os = "macos")]
const WALL_CLOCK_CYCLES: usize = 10_000;

/// Wall-clock budget for [`WALL_CLOCK_CYCLES`] beat cycles on the macOS
/// reference lane. Observed 0.9–2.5 ms across repeated runs on Apple
/// silicon (2026-09-15, this repository's dev machine); 50 ms is
/// deliberately generous so a busy developer machine still passes. Other
/// platforms never assert wall clock — see the ignored companion test
/// below.
#[cfg(target_os = "macos")]
const WALL_CLOCK_BUDGET: Duration = Duration::from_millis(50);

/// Deterministic ASCII filler: one grapheme per byte, so byte budgets and
/// grapheme boundaries coincide and the integer gates are exact.
fn filler(len: usize) -> String {
    (0..len)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect()
}

/// Drive one paced reveal over `state`'s block `index` on a synthetic
/// timeline, mirroring the production call shape in `tui/ui/frame.rs`
/// (`reveal_budget(interval, pending_len(index))` per beat). Like the
/// production caller, the clock is only kept ticking while the block still
/// holds pending text (`has_pending_stream_text`). Returns the bytes
/// committed per beat.
fn run_paced_beats(
    clock: &mut StreamDisplayClock,
    state: &mut StreamingState,
    index: usize,
    interval: Duration,
    max_beats: usize,
) -> Vec<usize> {
    let t0 = Instant::now();
    let mut per_beat = Vec::new();
    for beat in 0..max_beats {
        if !state.has_pending_stream_text(index) {
            break;
        }
        let now = t0 + interval * (beat as u32);
        clock.note_delta(now);
        if clock.take_due(now) {
            let backlog = state.pending_len(index);
            let taken = state.commit_text(index, super::reveal_budget(interval, backlog));
            per_beat.push(taken.len());
        }
    }
    per_beat
}

#[test]
fn max_bytes_per_beat_no_beat_reveals_more_than_the_budget() {
    for interval in [DEFAULT_STREAM_COMMIT_INTERVAL, SLOW_CLOCK_INTERVAL] {
        for backlog in SWEEP_BACKLOG_BYTES
            .iter()
            .copied()
            .chain([LARGEST_BURST_BYTES])
        {
            let mut clock = StreamDisplayClock::new(interval);
            let mut state = StreamingState::default();
            state.start_text(0);
            let burst = filler(backlog);
            state.push_content(0, &burst);

            let per_beat = run_paced_beats(&mut clock, &mut state, 0, interval, 10_000);
            assert!(
                !per_beat.is_empty(),
                "interval {interval:?}, backlog {backlog}: no beat ever committed"
            );
            for (beat, taken) in per_beat.iter().enumerate() {
                // Recompute the backlog that beat started from; `taken` is
                // the only mutation, so this stays pure integer arithmetic.
                let revealed_before: usize = per_beat[..beat].iter().sum();
                assert!(
                    revealed_before <= backlog,
                    "interval {interval:?}, backlog {backlog}: beats revealed {revealed_before} \
                     bytes before beat {beat}"
                );
                let backlog_at_beat_start = backlog - revealed_before;
                let budget = super::reveal_budget(interval, backlog_at_beat_start);
                assert!(
                    taken <= &budget,
                    "interval {interval:?}, backlog {backlog}: beat {beat} revealed \
                     {taken} bytes, budget was {budget}"
                );
            }
            let total: usize = per_beat.iter().sum();
            assert_eq!(total, backlog, "bytes must neither duplicate nor drop");
            assert_eq!(state.pending_len(0), 0, "drain must complete");
        }
    }
}

#[test]
fn beats_to_first_visible_is_exactly_one_due_beat() {
    for interval in [DEFAULT_STREAM_COMMIT_INTERVAL, SLOW_CLOCK_INTERVAL] {
        let mut clock = StreamDisplayClock::new(interval);
        let mut state = StreamingState::default();
        state.start_text(0);
        let delta = "visible on the first due beat";
        state.push_content(0, delta);
        assert!(delta.len() <= super::reveal_budget(interval, usize::MAX));

        let t0 = Instant::now();
        clock.note_delta(t0);

        // Probe at half-beat granularity so an off-by-one that pushes the
        // reveal to a second beat is visible, not hidden by the step size.
        let mut beats_until_visible = 0usize;
        let mut visible_len = 0usize;
        for half in 0..4u32 {
            let now = t0 + interval / 2 * half;
            if clock.take_due(now) {
                beats_until_visible += 1;
                if visible_len == 0 {
                    let taken =
                        state.commit_text(0, super::reveal_budget(interval, state.pending_len(0)));
                    visible_len = taken.len();
                }
            }
        }
        assert_eq!(
            beats_until_visible, 1,
            "interval {interval:?}: a t0 delta must show on the FIRST due beat"
        );
        assert_eq!(
            visible_len,
            delta.len(),
            "interval {interval:?}: the first beat must reveal the whole small delta"
        );
        assert_eq!(state.pending_len(0), 0);
    }
}

#[test]
fn beats_to_drain_matches_ceiling_of_backlog_over_per_beat_budget() {
    for interval in [DEFAULT_STREAM_COMMIT_INTERVAL, SLOW_CLOCK_INTERVAL] {
        let per_beat = super::reveal_budget(interval, usize::MAX);
        assert!(per_beat >= 1, "a beat must always admit progress");
        // Pin the integer arithmetic the ceiling below is derived from.
        let expected_per_beat = (REVEAL_PER_SECOND * interval.as_millis() as usize) / 1000;
        assert_eq!(per_beat, expected_per_beat.max(1));

        for backlog in SWEEP_BACKLOG_BYTES
            .iter()
            .copied()
            .chain([LARGEST_BURST_BYTES])
        {
            let expected_beats = backlog.div_ceil(per_beat);
            let mut clock = StreamDisplayClock::new(interval);
            let mut state = StreamingState::default();
            state.start_text(0);
            state.push_content(0, &filler(backlog));

            let per_beat_sizes = run_paced_beats(&mut clock, &mut state, 0, interval, 10_000);
            assert_eq!(
                per_beat_sizes.len(),
                expected_beats,
                "interval {interval:?}, backlog {backlog}: expected \
                 ceil({backlog}/{per_beat}) = {expected_beats} beats, got {}",
                per_beat_sizes.len()
            );
            assert_eq!(state.pending_len(0), 0);
        }
    }
}

#[test]
fn cell_kind_digest_matches_the_fixed_expected_sequence() {
    // Mixed thinking/text stream: block 0 is thinking, block 1 is text.
    // Distinct filler letters so a byte committed to the wrong cell, a
    // duplicated commit, or a dropped byte all change the reconstruction.
    let interval = DEFAULT_STREAM_COMMIT_INTERVAL;
    let per_beat = super::reveal_budget(interval, usize::MAX);
    let thinking_len = 2 * per_beat + 24; // ragged tail differs from text's
    let text_len = 2 * per_beat + 14;
    let thinking_src: String = "K".repeat(thinking_len);
    let text_src: String = "A".repeat(text_len);

    let mut clock = StreamDisplayClock::new(interval);
    let mut state = StreamingState::default();
    state.start_thinking(0);
    state.push_content(0, &thinking_src);
    state.start_text(1);
    state.push_content(1, &text_src);

    let t0 = Instant::now();
    let mut digest_beats: Vec<String> = Vec::new();
    let mut thinking_out = String::new();
    let mut text_out = String::new();
    for beat in 0..3u32 {
        let now = t0 + interval * beat;
        clock.note_delta(now);
        assert!(clock.take_due(now), "beat {beat} must be due");
        let mut tokens: Vec<String> = Vec::new();
        for (index, kind, out) in [(0usize, 'T', &mut thinking_out), (1, 'A', &mut text_out)] {
            let backlog = state.pending_len(index);
            let taken = state.commit_text(index, super::reveal_budget(interval, backlog));
            if !taken.is_empty() {
                tokens.push(format!("{kind}{}", taken.len()));
                out.push_str(&taken);
            }
        }
        digest_beats.push(tokens.join(","));
    }

    // Fixed expected digest for per_beat == 38, thinking 100 bytes, text
    // 90 bytes. Any wrong-cell commit, duplicate, or omission shows here.
    assert_eq!(
        per_beat, 38,
        "per-beat budget changed; recompute the fixed digest below"
    );
    assert_eq!(digest_beats.join("|"), "T38,A38|T38,A38|T24,A14");

    assert_eq!(
        thinking_out, thinking_src,
        "thinking bytes must arrive whole"
    );
    assert_eq!(text_out, text_src, "text bytes must arrive whole");
    assert_eq!(state.accumulated_thinking, thinking_src);
    assert_eq!(state.accumulated_text, text_src);
    assert_eq!(state.pending_len(0), 0);
    assert_eq!(state.pending_len(1), 0);
}

#[test]
fn flush_now_after_long_pause_lands_the_whole_backlog_in_one_step() {
    // Pins the module-doc promise: a pause longer than the beat interval
    // leaves the next beat due immediately, but pacing still spreads a
    // paced beat; only the forced `flush_now` finalization beat lands a
    // very large receipt in one step.
    let interval = DEFAULT_STREAM_COMMIT_INTERVAL;
    let mut clock = StreamDisplayClock::new(interval);
    let mut state = StreamingState::default();
    state.start_text(0);
    state.push_content(0, &filler(LARGEST_BURST_BYTES));

    let t0 = Instant::now();
    clock.note_delta(t0);
    assert!(clock.take_due(t0), "first beat is due immediately");
    let first = state.commit_text(0, super::reveal_budget(interval, state.pending_len(0)));
    assert!(first.len() <= super::reveal_budget(interval, LARGEST_BURST_BYTES));

    // Idle past the documented pause threshold (interval), then a large
    // receipt lands: the next beat is due immediately...
    let after_pause = t0 + 10 * interval;
    clock.note_delta(after_pause);
    assert_eq!(
        clock.due_in(after_pause),
        Some(Duration::ZERO),
        "a pause longer than the interval must leave the next beat due now"
    );
    // ...yet that paced beat still takes only one budget's worth.
    assert!(clock.take_due(after_pause));
    let paced = state.commit_text(0, super::reveal_budget(interval, state.pending_len(0)));
    assert!(paced.len() <= super::reveal_budget(interval, usize::MAX));

    // Finalization: one forced step drains everything remaining. The
    // caller keeps the clock ticking while backlog remains (see
    // `has_pending_stream_text`), so the finalization beat is noted first.
    let remaining_before = state.pending_len(0);
    assert!(remaining_before > super::reveal_budget(interval, usize::MAX));
    let finalize_at = after_pause + interval / 2;
    clock.note_delta(finalize_at);
    assert!(clock.flush_now(finalize_at));
    let landed = state.finalize_block_text(0);
    assert_eq!(
        landed.len(),
        remaining_before,
        "the whole backlog must land in ONE step"
    );
    assert_eq!(state.pending_len(0), 0);
    assert!(!state.has_pending_stream_text(0));
    // No second step exists to double-commit: a follow-up beat finds nothing.
    assert!(!clock.take_due(after_pause + interval));
}

// The wall-clock lane below is the ONLY place this file reads a real
// clock, and only on the macOS reference machine. The deterministic gates
// above carry the performance contract everywhere else.
#[cfg(target_os = "macos")]
#[test]
fn macos_reference_lane_beat_bookkeeping_stays_under_budget() {
    let interval = DEFAULT_STREAM_COMMIT_INTERVAL;
    let chunk = filler(38); // one beat's budget per cycle: steady state
    let mut clock = StreamDisplayClock::new(interval);
    let mut buffer = super::StreamBuffer::new();
    let t0 = Instant::now(); // synthetic timeline; Instant used as pure arithmetic
    let start = Instant::now(); // the only real wall-clock read
    for i in 0..WALL_CLOCK_CYCLES {
        let now = t0 + interval * (i as u32);
        clock.note_delta(now);
        buffer.push_delta(&chunk);
        if clock.take_due(now) {
            let budget = super::reveal_budget(interval, buffer.pending_len());
            let _ = buffer.take_up_to(budget);
        }
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < WALL_CLOCK_BUDGET,
        "{WALL_CLOCK_CYCLES} beat cycles of clock + buffer bookkeeping took \
         {elapsed:?}, budget is {WALL_CLOCK_BUDGET:?}"
    );
}

// Deliberate skip marker for non-macOS platforms: no wall-clock assertion
// runs there, by design. CI on linux must not inherit this machine's time
// scale (see #6193: "separate expectations by machine").
#[cfg(not(target_os = "macos"))]
#[test]
#[ignore = "wall-clock reference lane is macOS-only; deterministic gates carry the contract here"]
fn macos_wall_clock_lane_is_not_asserted_off_macos() {}
