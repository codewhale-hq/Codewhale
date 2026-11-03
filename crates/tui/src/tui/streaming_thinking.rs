//! Streaming-thinking lifecycle for the active cell.
//!
//! DeepSeek V4 emits `reasoning_content` chunks before final answers.
//! These get rendered as a "Thinking" entry inside the per-turn active
//! cell. This module is the single source of truth for:
//!
//! - creating a streaming thinking entry on first chunk
//! - appending chunks to the live entry
//! - showing a localized placeholder while a translation is in-flight
//!   (and animating its elapsed/spinner suffix)
//! - replacing the placeholder when the translation arrives
//! - finalizing the entry (stopping the spinner, stamping duration)
//!   when a thinking block ends
//! - stashing the reasoning buffer onto `app.last_reasoning` so the
//!   summary survives compaction

use std::time::Duration;
use std::time::Instant;

use crate::tui::active_cell::ActiveCell;
use crate::tui::app::App;
use crate::tui::history::HistoryCell;

/// Debounce window for active-cell revision bumps while a thinking block is
/// streaming (#1620). Reasoning deltas arrive far faster than the eye can
/// follow, and each revision bump invalidates the active cell's wrap cache,
/// forcing a full re-wrap of the live tail. Coalescing intermediate bumps to
/// one per window keeps the perceived stream smooth without re-wrapping per
/// character. ~100ms ≈ 10 intermediate repaints/sec, well below the 120 FPS
/// frame cap (see `frame_rate_limiter`) yet imperceptible as lag.
///
/// Correctness: this only skips *intermediate* repaints. Appended content is
/// never dropped — it lands in the cell immediately — and finalize always
/// forces a bump so the final reasoning text is fully rendered.
const THINKING_REVISION_THROTTLE: Duration = Duration::from_millis(100);

/// Bump the active-cell revision for a streaming thinking mutation, but at
/// most once per [`THINKING_REVISION_THROTTLE`] window. Returns whether a bump
/// was actually emitted. Skipped bumps coalesce into the next one (or into the
/// forced finalize bump), so no content is ever lost — only redundant
/// intermediate re-wraps are dropped.
fn bump_thinking_revision_throttled(app: &mut App, now: Instant) -> bool {
    let due = app
        .thinking_revision_last_bump_at
        .is_none_or(|last| now.saturating_duration_since(last) >= THINKING_REVISION_THROTTLE);
    if due {
        app.thinking_revision_last_bump_at = Some(now);
        app.bump_active_cell_revision();
    }
    due
}

/// Ensure an in-flight Thinking entry exists in `active_cell` and return its
/// entry index. If no thinking entry is currently streaming, push a fresh one.
/// P2.3: thinking shares the active cell with subsequent tool calls so the
/// pair render as one logical "Working…" block.
pub(super) fn ensure_active_entry(app: &mut App) -> usize {
    if let Some(idx) = app.streaming_thinking_active_entry {
        return idx;
    }
    if app.active_cell.is_none() {
        app.active_cell = Some(ActiveCell::new());
    }
    let active = app.active_cell.as_mut().expect("active_cell just ensured");
    let entry_idx = active.push_thinking(HistoryCell::Thinking {
        content: String::new(),
        streaming: true,
        duration_secs: None,
    });
    app.streaming_thinking_active_entry = Some(entry_idx);
    app.bump_active_cell_revision();
    entry_idx
}

/// Append text to a streaming Thinking entry inside `active_cell`. The text is
/// committed to the cell immediately; the active-cell revision bump that
/// triggers a re-wrap of the live tail is debounced to at most one per
/// [`THINKING_REVISION_THROTTLE`] window (#1620). Skipped bumps coalesce into
/// the next append or the forced finalize bump, so no content is ever lost.
pub(super) fn append(app: &mut App, entry_idx: usize, text: &str) {
    append_at(app, entry_idx, text, Instant::now());
}

/// `append` with an injectable clock so the debounce can be tested
/// deterministically.
fn append_at(app: &mut App, entry_idx: usize, text: &str, now: Instant) {
    if text.is_empty() {
        return;
    }
    let mutated = if let Some(active) = app.active_cell.as_mut()
        && let Some(HistoryCell::Thinking { content, .. }) = active.entry_mut(entry_idx)
    {
        content.push_str(text);
        true
    } else {
        false
    };
    if mutated {
        bump_thinking_revision_throttled(app, now);
    }
}

/// Build the spinner-decorated placeholder shown in the thinking entry
/// while a translation is in flight (`Thinking… (1.2s |)`).
fn translation_placeholder_spinner_frame(app: &App, elapsed: f32) -> &'static str {
    let elapsed = std::time::Duration::try_from_secs_f32(elapsed.max(0.0))
        .unwrap_or(std::time::Duration::MAX);
    let animated_frame =
        codewhale_ratatui::spin::frame(elapsed, codewhale_ratatui::MotionMode::Full, true);
    app.motion_policy().spinner_glyph(animated_frame, true)
}

pub(super) fn translation_placeholder_frame(app: &App) -> String {
    let base = codewhale_localization::thinking_translation_placeholder(app.ui_locale);
    let elapsed = app
        .thinking_started_at
        .or(app.turn_started_at)
        .map(|started| started.elapsed().as_secs_f32())
        .unwrap_or_default();
    let frame = translation_placeholder_spinner_frame(app, elapsed);
    format!("{base} ({elapsed:.1}s {frame})")
}

/// If the given entry is empty or still showing the translation
/// placeholder prefix, replace it with the latest animated frame.
pub(super) fn set_placeholder(app: &mut App, entry_idx: usize) {
    let base = codewhale_localization::thinking_translation_placeholder(app.ui_locale);
    let next = translation_placeholder_frame(app);
    let mutated = if let Some(active) = app.active_cell.as_mut()
        && let Some(HistoryCell::Thinking { content, .. }) = active.entry_mut(entry_idx)
        && (content.is_empty() || content.starts_with(base))
    {
        if *content != next {
            *content = next;
            true
        } else {
            false
        }
    } else {
        false
    };
    if mutated {
        app.bump_active_cell_revision();
    }
}

/// Advance the spinner suffix on every existing translation placeholder
/// in `active_cell`. Returns true when at least one cell was updated so
/// the dispatch loop can schedule another tick.
pub(super) fn animate_pending_translation(app: &mut App, translation_pending: bool) -> bool {
    if !app.translation_enabled {
        return false;
    }
    let thinking_streaming = app.streaming_thinking_active_entry.is_some();
    if !translation_pending && !thinking_streaming {
        return false;
    }
    let base = codewhale_localization::thinking_translation_placeholder(app.ui_locale);
    let next = translation_placeholder_frame(app);

    if let Some(active) = app.active_cell.as_mut() {
        for idx in (0..active.entry_count()).rev() {
            if let Some(HistoryCell::Thinking { content, .. }) = active.entry_mut(idx)
                && content.starts_with(base)
                && *content != next
            {
                *content = next.clone();
                app.bump_active_cell_revision();
                return true;
            }
        }
    }
    false
}

/// Replace a translation placeholder with the finished translated text.
pub(super) fn replace_pending_translation(
    app: &mut App,
    cell_index: usize,
    translated_text: String,
) {
    if let Some(HistoryCell::Thinking { content, .. }) = app.cell_at_virtual_index_mut(cell_index) {
        *content = translated_text;
        app.needs_redraw = true;
    }
}

/// Start a new streaming thinking block. If another thinking block is still
/// active, first drain its pending UI tail so a late block boundary cannot
/// discard content buffered inside `StreamingState`.
pub(super) fn start_block(app: &mut App) -> bool {
    let finalized_previous = if app.streaming_thinking_active_entry.is_some() {
        let finalized = finalize_current(app);
        stash_reasoning_buffer_into_last_reasoning(app);
        finalized
    } else {
        false
    };

    // #7123: a text block can still hold an uncommitted tail when a reasoning
    // block opens — the display beat commits once per interval, so the
    // boundary lands between beats. `reset` below discards whatever is left.
    flush_pending_text_tail(app);
    app.reasoning_buffer.clear();
    app.reasoning_header = None;
    app.thinking_started_at = Some(Instant::now());
    app.streaming_state.reset();
    app.streaming_state.start_thinking(0);
    let _ = ensure_active_entry(app);
    finalized_previous
}

/// Flush a text block's uncommitted tail into the assistant cell.
///
/// The display beat commits at most once per interval, so a block boundary
/// that arrives between beats leaves the last slice of prose sitting in
/// `StreamingState`. Both `start_block` and the finalize paths reset that
/// state; without this drain the tail before a reasoning card never reaches
/// the transcript (#7123).
fn flush_pending_text_tail(app: &mut App) {
    if app.streaming_state.block_is_thinking(0) {
        return;
    }
    // Nothing buffered: do not create an assistant cell just to write nothing
    // into it.
    if app.streaming_state.pending_len(0) == 0 {
        return;
    }
    // Resolve the destination before taking the buffer. Draining first and
    // then discovering there is nowhere to put the text discarded it (#7123).
    let index = app
        .streaming_message_index
        .unwrap_or_else(|| crate::tui::ui::frame::ensure_streaming_assistant_history_cell(app));
    let remaining = app.streaming_state.finalize_block_text(0);
    if remaining.is_empty() {
        return;
    }
    crate::tui::ui::frame::append_streaming_text(app, index, &remaining);
    crate::tui::ui::frame::accrue_streaming_token_estimate(app, &remaining);
}

/// Finalize the currently-streaming thinking entry: drain the pending
/// state buffer, compute elapsed duration, stop the spinner.
pub(super) fn finalize_current(app: &mut App) -> bool {
    if app.streaming_thinking_active_entry.is_none() {
        app.thinking_started_at = None;
        return false;
    }
    let duration = app
        .thinking_started_at
        .take()
        .map(|t| t.elapsed().as_secs_f32());
    // #7123: drain only the reasoning block. If a text block owns index 0 at
    // this point (a boundary lost an event), taking its buffer here would
    // append prose to the thinking card and drop it from the answer.
    let remaining = if app.streaming_state.block_is_thinking(0) {
        app.streaming_state.finalize_block_text(0)
    } else {
        String::new()
    };
    finalize_active_entry(app, duration, &remaining)
}

/// Move the in-flight reasoning buffer onto `app.last_reasoning` so the
/// summary survives compaction or transcript trimming.
pub(super) fn stash_reasoning_buffer_into_last_reasoning(app: &mut App) {
    if app.reasoning_buffer.is_empty() {
        return;
    }

    if let Some(existing) = app.last_reasoning.as_mut()
        && !existing.is_empty()
    {
        if !existing.ends_with('\n') {
            existing.push('\n');
        }
        existing.push_str(&app.reasoning_buffer);
    } else {
        app.last_reasoning = Some(app.reasoning_buffer.clone());
    }
    app.reasoning_buffer.clear();
}

/// Finalize the in-flight thinking entry in `active_cell`: append the
/// collector's remaining buffered text, stop the spinner, and stamp the
/// duration. Returns `true` when a thinking entry was finalized (so the
/// dispatch loop knows the transcript was touched). No-op if no thinking
/// entry is currently streaming.
pub(super) fn finalize_active_entry(app: &mut App, duration: Option<f32>, remaining: &str) -> bool {
    let Some(entry_idx) = app.streaming_thinking_active_entry.take() else {
        return false;
    };
    if !remaining.is_empty() && !app.translation_enabled {
        append(app, entry_idx, remaining);
    }
    if let Some(active) = app.active_cell.as_mut()
        && let Some(HistoryCell::Thinking {
            streaming,
            duration_secs,
            ..
        }) = active.entry_mut(entry_idx)
    {
        *streaming = false;
        *duration_secs = duration;
    }
    // Red line (#1620): finalize must force a bump so the final reasoning text
    // is fully rendered even if the last appended chunk was throttled. Reset
    // the debounce window so the next thinking block's first chunk renders
    // immediately rather than being coalesced into a stale window.
    app.thinking_revision_last_bump_at = None;
    app.bump_active_cell_revision();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{App, TuiOptions};
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    fn test_app() -> App {
        let options = TuiOptions {
            start_in_agent_mode: true,
            skip_onboarding: false,
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        App::new(options, &Config::default())
    }

    fn thinking_content(app: &App, entry_idx: usize) -> String {
        match app
            .active_cell
            .as_ref()
            .and_then(|active| active.entries().get(entry_idx))
        {
            Some(HistoryCell::Thinking { content, .. }) => content.clone(),
            other => panic!("expected a Thinking entry at {entry_idx}, got {other:?}"),
        }
    }

    #[test]
    fn translation_placeholder_spinner_uses_full_motion_only() {
        let mut app = test_app();
        app.low_motion = false;
        app.fancy_animations = true;
        assert_eq!(translation_placeholder_spinner_frame(&app, 0.0), ">");
        assert_eq!(translation_placeholder_spinner_frame(&app, 0.6), "\\");

        app.low_motion = true;
        assert_eq!(translation_placeholder_spinner_frame(&app, 0.0), "●");
        assert_eq!(translation_placeholder_spinner_frame(&app, 0.6), "●");

        app.low_motion = false;
        app.fancy_animations = false;
        assert_eq!(translation_placeholder_spinner_frame(&app, 0.0), "›");
        assert_eq!(translation_placeholder_spinner_frame(&app, 0.6), "›");
    }

    /// #1620: a burst of reasoning chunks inside one throttle window must
    /// coalesce to a single active-cell revision bump (so the renderer
    /// re-wraps the live tail ~10x/sec instead of once per character), while
    /// every byte of content is preserved and finalize forces a final bump.
    #[test]
    fn issue_1620_throttles_thinking_bumps_without_losing_content() {
        let mut app = test_app();
        let entry = ensure_active_entry(&mut app);
        // `ensure_active_entry` bumped once on creation; start the measurement
        // from a clean throttle window so the first append renders immediately.
        app.thinking_revision_last_bump_at = None;
        let rev_before = app.active_cell_revision;

        let t0 = Instant::now();
        let chunks = [
            "Hel", "lo, ", "this", " is", " a", " lo", "ng", " re", "ason", "ing",
        ];
        // All ten chunks land within a single 100ms window (5ms apart).
        for (i, chunk) in chunks.iter().enumerate() {
            append_at(
                &mut app,
                entry,
                chunk,
                t0 + Duration::from_millis(i as u64 * 5),
            );
        }
        assert_eq!(
            app.active_cell_revision.wrapping_sub(rev_before),
            1,
            "rapid chunks within one throttle window must coalesce to one bump"
        );

        // A chunk after the window expires is allowed to bump again.
        append_at(
            &mut app,
            entry,
            " stream",
            t0 + THINKING_REVISION_THROTTLE + Duration::from_millis(10),
        );
        assert_eq!(
            app.active_cell_revision.wrapping_sub(rev_before),
            2,
            "a chunk past the throttle window should bump once more"
        );

        // No content was dropped despite the skipped intermediate bumps.
        let expected = format!("{} stream", chunks.concat());
        assert_eq!(thinking_content(&app, entry), expected);

        // Red line: finalize forces exactly one bump and flushes the tail.
        let rev_pre_final = app.active_cell_revision;
        let finalized = finalize_active_entry(&mut app, Some(1.5), " [end]");
        assert!(finalized, "finalize should report it finalized an entry");
        assert_eq!(
            app.active_cell_revision,
            rev_pre_final.wrapping_add(1),
            "finalize must always force exactly one revision bump"
        );
        assert_eq!(
            thinking_content(&app, entry),
            format!("{expected} [end]"),
            "finalize must not drop the trailing reasoning text"
        );
        assert!(
            app.thinking_revision_last_bump_at.is_none(),
            "finalize should reset the throttle window for the next block"
        );
    }

    /// #7123: a reasoning block that opens while the display beat still holds
    /// an uncommitted slice of prose must not drop that slice. `start_block`
    /// resets `StreamingState`, so the tail has to be delivered first.
    #[test]
    fn issue_7123_reasoning_block_keeps_the_prose_tail_it_interrupts() {
        let mut app = test_app();

        // Prose streams, and the beat has not committed the last slice yet.
        app.streaming_state.reset();
        app.streaming_state.start_text(0);
        app.streaming_state.push_content(0, "answer tail");
        app.add_message(HistoryCell::Assistant {
            content: "answer head ".to_string(),
            streaming: true,
        });
        app.streaming_message_index = Some(app.history.len() - 1);

        start_block(&mut app);

        let prose = match app.history.last() {
            Some(HistoryCell::Assistant { content, .. }) => content.clone(),
            other => panic!("expected the assistant cell: {other:?}"),
        };
        assert_eq!(
            prose, "answer head answer tail",
            "the prose tail must reach the transcript before the reasoning block"
        );
        assert!(
            app.streaming_state.block_is_thinking(0),
            "the new block must be the reasoning block"
        );
    }

    /// #7123: `MessageStarted` must deliver a live reasoning tail *before*
    /// `flush_active_cell`, because that flush clears
    /// `streaming_thinking_active_entry` on every path and only marks the cell
    /// finished. Draining afterwards is dead code and the tail is lost.
    #[test]
    fn issue_7123_message_started_order_keeps_the_reasoning_tail() {
        let mut app = test_app();
        let entry = ensure_active_entry(&mut app);
        app.thinking_started_at = Some(Instant::now());
        app.streaming_state.reset();
        app.streaming_state.start_thinking(0);
        app.streaming_state.push_content(0, "reasoning tail");

        // The real handler order.
        assert!(finalize_current(&mut app));
        stash_reasoning_buffer_into_last_reasoning(&mut app);
        let before_flush = thinking_content(&app, entry);
        app.flush_active_cell();

        let flushed = app
            .history
            .iter()
            .find_map(|cell| match cell {
                HistoryCell::Thinking { content, .. } => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert_eq!(
            before_flush, "reasoning tail",
            "finalize must deliver the tail into the entry while it is still live"
        );
        assert_eq!(
            flushed, "reasoning tail",
            "the flush must carry that tail into the transcript"
        );
        assert!(
            app.streaming_thinking_active_entry.is_none(),
            "flush_active_cell clears the pointer"
        );

        let mut translated = test_app();
        translated.translation_enabled = true;
        translated.thinking_started_at = Some(Instant::now());
        let entry = ensure_active_entry(&mut translated);
        set_placeholder(&mut translated, entry);
        let placeholder = thinking_content(&translated, entry);
        translated.streaming_state.start_thinking(0);
        translated
            .streaming_state
            .push_content(0, "untranslated tail");
        translated.reasoning_buffer.push_str("untranslated tail");
        assert!(finalize_current(&mut translated));
        assert_eq!(thinking_content(&translated, entry), placeholder);
        stash_reasoning_buffer_into_last_reasoning(&mut translated);
        assert_eq!(
            translated.last_reasoning.as_deref(),
            Some("untranslated tail")
        );
    }

    /// #7123: with no live entry there is no cell to deliver into, so the
    /// buffer must be left alone rather than drained and discarded.
    #[test]
    fn issue_7123_finalize_current_keeps_the_buffer_without_an_entry() {
        let mut app = test_app();
        app.streaming_thinking_active_entry = None;
        app.streaming_state.reset();
        app.streaming_state.start_thinking(0);
        app.streaming_state.push_content(0, "orphan reasoning");

        assert!(!finalize_current(&mut app));
        assert_eq!(
            app.streaming_state.pending_len(0),
            "orphan reasoning".len(),
            "text with nowhere to go must stay buffered"
        );
    }

    /// #7123: `finalize_current` must take a reasoning tail but never consume a
    /// text block that lost its boundary event.
    #[test]
    fn issue_7123_finalize_current_leaves_a_text_block_alone() {
        let mut app = test_app();
        let entry = ensure_active_entry(&mut app);
        app.thinking_started_at = Some(Instant::now());

        // A text block owns index 0 while a reasoning entry is live: a lost
        // `ContentBlockStop` leaves the state in exactly this shape.
        app.streaming_state.reset();
        app.streaming_state.start_text(0);
        app.streaming_state.push_content(0, "stray prose");

        let _ = finalize_current(&mut app);

        assert_eq!(
            thinking_content(&app, entry),
            "",
            "prose must not be appended to the reasoning card"
        );
    }
}
