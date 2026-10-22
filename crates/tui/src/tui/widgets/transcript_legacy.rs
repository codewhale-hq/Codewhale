//! Frozen mounted transcript rendering counterpart from the Composer-inherited base.
//! Unused ambient wrappers and the wall-clock constructor are omitted; the
//! retained rendering and selection fragments stay unchanged except for the
//! explicitly adopted viewport-relative prompt pin and its row reservation.
use super::*;
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget};
const JUMP_TO_LATEST_BUTTON_WIDTH: u16 = 3;
const JUMP_TO_LATEST_BUTTON_HEIGHT: u16 = 3;

pub struct ChatWidget {
    content_area: Rect,
    /// Scrollable/selectable transcript geometry. When the last prompt is
    /// pinned, this starts one row below `content_area`; the pinned header is
    /// intentionally outside transcript hit-testing.
    transcript_area: Rect,
    lines: Vec<Line<'static>>,
    line_links: Vec<Vec<crate::tui::osc8::LineLink>>,
    scrollbar: Option<TranscriptScrollbar>,
    jump_to_latest_button: Option<Rect>,
    background: Color,
    ocean_column: Option<crate::tui::ocean::OceanColumn>,
    /// Live-activity shape of the ambient scene (thinking/tools/subagents).
    ocean_activity: crate::tui::ambient_life::AmbientActivity,
    /// Ink for the selected underwater scene's idle fish/bubbles.
    ambient_inks: Option<(Color, Color)>,
    ocean_elapsed_ms: u128,
    ocean_animated: bool,
    /// Fixed-point (0..=1000) life presence; see `ocean::life_presence`.
    life_presence_fixed: u16,
    fish_flee_elapsed_ms: Option<u128>,
    ambient_life: bool,
    scroll_track: Color,
    scroll_thumb: Color,
    jump_border: Color,
    jump_arrow: Color,
}

#[derive(Debug, Clone, Copy)]
struct TranscriptScrollbar {
    top: usize,
    visible: usize,
    total: usize,
}

/// A `todo_write` result is a full replacement snapshot, not an incremental
/// transcript event. Keep only the newest successful snapshot in the visible
/// transcript while retaining every tool result in history for model context
/// and persistence.
fn superseded_todo_write_indices(
    history: &[HistoryCell],
    active_entries: &[HistoryCell],
) -> HashSet<usize> {
    let mut hidden = HashSet::new();
    let mut latest = None;

    for (index, cell) in history.iter().chain(active_entries).enumerate() {
        let HistoryCell::Tool(ToolCell::Generic(tool)) = cell else {
            continue;
        };
        if tool.name != "todo_write" || tool.status != ToolStatus::Success {
            continue;
        }

        if let Some(previous) = latest.replace(index) {
            hidden.insert(previous);
        }
    }

    if let Some(index) = latest {
        let cell = history
            .get(index)
            .or_else(|| active_entries.get(index.saturating_sub(history.len())));
        if cell.is_some_and(todo_write_snapshot_is_empty) {
            hidden.insert(index);
        }
    }

    hidden
}

fn todo_write_snapshot_is_empty(cell: &HistoryCell) -> bool {
    let HistoryCell::Tool(ToolCell::Generic(tool)) = cell else {
        return false;
    };
    let Some(output) = tool.output.as_deref() else {
        return false;
    };
    let Some(json_start) = output.find('{') else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&output[json_start..]) else {
        return false;
    };
    value
        .get("items")
        .and_then(serde_json::Value::as_array)
        .is_some_and(Vec::is_empty)
}

fn resolve_transcript_viewport_after_layout(
    viewport: &mut ViewportState,
    visible_lines: usize,
) -> (usize, usize, bool) {
    let total_lines = viewport.transcript_cache.total_lines();
    let line_meta = viewport.transcript_cache.line_meta();
    if viewport.pending_scroll_delta != 0 {
        viewport.transcript_scroll = viewport.transcript_scroll.scrolled_by(
            viewport.pending_scroll_delta,
            line_meta,
            visible_lines,
        );
        viewport.pending_scroll_delta = 0;
    }

    let max_start = total_lines.saturating_sub(visible_lines);
    // Snapshot tail intent before resolve: clamping an out-of-range fixed
    // offset can return `to_bottom()`, which must not masquerade as the user's
    // choice to resume following a streaming tail (v0.8.11).
    let was_explicit_tail = viewport.transcript_scroll.is_at_tail();
    let (scroll_state, top) = viewport.transcript_scroll.resolve_top(line_meta, max_start);
    viewport.transcript_scroll = scroll_state;
    viewport.last_transcript_top = top;
    viewport.last_transcript_visible = visible_lines;
    viewport.last_transcript_total = total_lines;
    (total_lines, top, was_explicit_tail)
}

impl ChatWidget {
    /// Build one render snapshot from an already sampled ocean clock.
    fn new_with_ocean_elapsed(app: &mut App, area: Rect, ocean_elapsed_ms: u128) -> Self {
        let content_area = area;
        let background = app.ui_theme.surface_bg;
        // The ordinary shell inherits its host/theme surface. Underwater life
        // is earned by the underwater theme, never painted over a user's
        // terminal simply because the app happens to be active.
        let underwater_atmosphere = app.theme_id == codewhale_palette::ThemeId::Underwater;
        let ocean_ramp = underwater_atmosphere
            .then(|| crate::tui::ocean::OceanRamp::for_theme(&app.ui_theme))
            .flatten();
        // Ink hue carries the live activity (reasoning deep-dim, tools bright,
        // sub-agents seafoam) so the marks read the state at a glance.
        let ocean_activity = crate::tui::ambient_life::AmbientActivity::from_kind(
            crate::tui::underwater::LiveActivity::from_app(app).kind(),
        );
        let ambient_inks = underwater_atmosphere
            .then(|| crate::tui::ocean::ambient_inks_for_activity(&app.ui_theme, ocean_activity));
        // The completion breath is authored decorative motion, so it rides the
        // same motion gate as everything else in the water. Both the column's
        // settle flourish and ambient life's presence read this one clock:
        // the pet needs the settle tail too; the column clips only its light pulse.
        let completion_life_clock = (underwater_atmosphere
            && app.motion_policy().allows_decorative())
        .then_some(())
        .and(app.ocean_completion_started_at)
        .map(|started| started.elapsed().as_millis());
        let completion_elapsed_ms = completion_life_clock
            .filter(|elapsed| *elapsed < crate::tui::ocean::COMPLETION_SETTLE_MS);
        let completion_life_active = completion_life_clock
            .is_some_and(|elapsed| elapsed < crate::tui::ocean::COMPLETION_SETTLE_MS);
        let render_empty_state = should_render_empty_state(app);
        let phase = ShellPhase::from_app(app);
        // Keep the selected underwater scene alive while a turn is doing
        // work, even after the transcript exists. The ordinary Flat shell
        // remains entirely still and lets the host terminal lead.
        let underwater_motion_enabled =
            underwater_atmosphere && crate::tui::underwater::decorative_shell_motion_enabled(app);
        let browsing_history = !app.viewport.transcript_scroll.is_at_tail();
        let ocean_animated = underwater_motion_enabled
            && (render_empty_state
                || browsing_history
                || matches!(phase, ShellPhase::Working | ShellPhase::Verifying));
        // Life presence eases the animated/static boundary as a pure function
        // of the monotonic clocks (see ocean::life_presence): bursty fast
        // streams ramp in, quiet waits settle out, never a hard snap.
        //
        // This deliberately takes the *gated* completion clock. Reading
        // `app.ocean_completion_started_at` raw here let the completion branch
        // of `life_presence` short-circuit the `!animated` check, so a
        // reduced-motion session got ~1.4 s of full ambient life after every
        // successful turn — precisely while the user was reading the result.
        let life_presence = crate::tui::ocean::life_presence(
            completion_life_clock,
            app.turn_started_at
                .map(|started| started.elapsed().as_millis()),
            ocean_animated,
            browsing_history,
            render_empty_state,
        );
        let life_presence_fixed = (life_presence * 1000.0).round().clamp(0.0, 1000.0) as u16;
        let ocean_column = ocean_ramp.map(|ramp| {
            let context_percent = crate::tui::phase_strip::context_percent_from_app(app);
            crate::tui::ocean::OceanColumn::new(
                ramp,
                content_area,
                ocean_elapsed_ms,
                completion_elapsed_ms,
                phase,
                ocean_animated,
                life_presence_fixed,
                context_percent,
            )
        });
        let fish_flee_elapsed_ms = underwater_motion_enabled
            .then_some(())
            .and(app.turn_started_at)
            .map(|started| started.elapsed().as_millis())
            .filter(|elapsed| *elapsed < 800)
            .filter(|_| matches!(phase, ShellPhase::Working | ShellPhase::Verifying));
        let scroll_track = app.ui_theme.border;
        let scroll_thumb = app.ui_theme.status_working;
        let jump_border = app.ui_theme.border;
        let jump_arrow = app.ui_theme.status_working;
        let visible_lines = content_area.height as usize;
        let render_options = app.transcript_render_options();

        if render_empty_state {
            let lines = build_empty_state_lines(app, content_area);
            app.viewport.last_transcript_area = Some(content_area);
            app.viewport.last_transcript_top = 0;
            app.viewport.last_transcript_visible = visible_lines;
            app.viewport.last_transcript_total = 0;
            app.viewport.last_transcript_padding_top = 0;
            app.viewport.jump_to_latest_button_area = None;
            return Self {
                content_area,
                transcript_area: content_area,
                lines,
                line_links: Vec::new(),
                scrollbar: None,
                jump_to_latest_button: None,
                background,
                ocean_column,
                ocean_activity,
                ambient_inks,
                ocean_elapsed_ms,
                ocean_animated,
                life_presence_fixed,
                fish_flee_elapsed_ms,
                // Reduced-motion users still get a quiet, static Deepsea scene;
                // Flat remains a normal host-owned terminal either way.
                ambient_life: underwater_atmosphere
                    && !app.attention_hold_active()
                    && matches!(
                        phase,
                        ShellPhase::Idle
                            | ShellPhase::Typing
                            | ShellPhase::Working
                            | ShellPhase::Verifying
                    ),
                scroll_track,
                scroll_thumb,
                jump_border,
                jump_arrow,
            };
        }

        // Reserve the scrollbar's column before wrapping, so painting it cannot
        // erase the final character of a line. Keep this width stable when the
        // history starts/stops scrolling; cached lines and copy metadata must
        // use the same layout on both sides of that transition.
        let transcript_width = content_area.width.saturating_sub(1).max(1);

        // Per-cell revision caching (fix for issue #78):
        //
        // Every committed history cell carries its own revision counter in
        // `app.history_revisions`. The transcript cache compares each cell's
        // current revision against the previously rendered one, so unchanged
        // cells reuse their cached wrapped lines instead of being re-wrapped
        // every frame. This is the difference between O(history.len()) and
        // O(changed_cells) per render — and was the root cause of scroll lag
        // on long transcripts.
        //
        // The active in-flight cell (if any) is appended as the last cell so
        // its mutations show up at the live tail. Each entry inside the
        // active cell becomes a virtual cell at index `history.len() + i`,
        // matching `App::cell_at_virtual_index`. Active-cell entries share
        // the same `active_cell_revision` salt so any mutation in the active
        // cell forces only those rows to re-render — committed history rows
        // are unaffected.
        app.resync_history_revisions();
        app.viewport.transcript_cache.set_streaming_source_receipt(
            app.streaming_source_receipt.map(|receipt| {
                crate::tui::transcript::StreamingSourceReceipt {
                    cell_index: receipt.cell_index,
                    from_revision: history_entry_revision(receipt.from_revision),
                    to_revision: history_entry_revision(receipt.to_revision),
                    content_len: receipt.content_len,
                }
            }),
        );
        let provisional_action_owner = app.transcript_action_owner();
        let active_entries: &[HistoryCell] = app
            .active_cell
            .as_ref()
            .map_or(&[], |active| active.entries());
        let history_len = app.history.len();
        // Cache the group projection, not the whole frame: filtered refs and
        // transcript bookkeeping below still walk the retained history.
        let cache_key_matches = app.tool_run_cache.history_version == app.history_version
            && app.tool_run_cache.history_len == history_len
            && app.tool_run_cache.active_cell_revision == app.active_cell_revision
            && app.tool_run_cache.active_len == active_entries.len()
            && app.tool_run_cache.threshold == app.tool_collapse_threshold
            && app.tool_run_cache.mode == app.tool_collapse_mode
            && app.tool_run_cache.calm_mode == app.calm_mode
            && app.tool_run_cache.expanded_runs == app.expanded_tool_runs;
        if !cache_key_matches {
            let superseded_todos = superseded_todo_write_indices(&app.history, active_entries);
            let runs = if app.tool_collapse_active() {
                crate::tui::history::detect_tool_runs_from_slices(
                    &app.history,
                    active_entries,
                    app.tool_collapse_threshold,
                )
            } else {
                Vec::new()
            };
            let cache = &mut app.tool_run_cache;
            #[cfg(test)]
            {
                cache.projection_builds += 1;
            }
            cache.generation = cache.generation.wrapping_add(1);
            cache.summaries.clear();
            cache.hidden_indices.clear();
            for run in &runs {
                // A hidden replacement snapshot cannot own a group summary.
                if app.expanded_tool_runs.contains(&run.start)
                    || (run.start..run.start.saturating_add(run.count))
                        .any(|index| superseded_todos.contains(&index))
                {
                    continue;
                }
                cache.summaries.insert(
                    run.start,
                    (
                        tool_run_summary_cell(run),
                        tool_run_summary_revision(
                            run,
                            &app.history_revisions,
                            history_len,
                            app.active_cell_revision,
                        ),
                    ),
                );
                cache
                    .hidden_indices
                    .extend(run.start + 1..run.start + run.count);
            }
            cache.superseded_todos = superseded_todos;
            cache.history_version = app.history_version;
            cache.history_len = history_len;
            cache.active_cell_revision = app.active_cell_revision;
            cache.active_len = active_entries.len();
            cache.threshold = app.tool_collapse_threshold;
            cache.mode = app.tool_collapse_mode;
            cache.calm_mode = app.calm_mode;
            cache.expanded_runs.clone_from(&app.expanded_tool_runs);
        }
        // v0.9.1: do not collapse concurrent sub-agent cards into an Enter-
        // expand shelf. Count lives in header chrome; full cards stay visible;
        // sidebar / SubAgents modal are the drill-in surface.
        let has_collapsed = !app.collapsed_cells.is_empty()
            || !app.tool_run_cache.summaries.is_empty()
            || !app.tool_run_cache.superseded_todos.is_empty();
        if has_collapsed {
            app.tool_run_cache
                .refresh_filtered(history_len + active_entries.len(), &app.collapsed_cells);
        }
        let summary_cells = &app.tool_run_cache.summaries;

        // Keyed steady state: a matching key lets the cache skip the O(n)
        // unchanged-prefix scan entirely (#6652). The projection generation
        // folds in view-level changes (collapse toggles) that never touch
        // history revisions. Staging precedes the availability check because
        // `ensure_iter` consumes the staged key.
        app.viewport
            .transcript_cache
            .note_revision_key(transcript_revision_key(
                app.history_version,
                app.history.len(),
                app.active_cell_revision,
                active_entries.len(),
                app.tool_run_cache.generation,
            ));
        // When every cache input is pinned equal (key, width, options,
        // folds, row count, owner), the ensure call itself is dead work:
        // skip building the per-frame revision/ref vectors too.
        let keyed_skip = app.viewport.transcript_cache.keyed_skip_available(
            transcript_width,
            render_options,
            &app.cell_folds,
            if has_collapsed {
                app.tool_run_cache.filtered.original.len()
            } else {
                app.history.len() + active_entries.len()
            },
            provisional_action_owner,
        );
        // Keyed skip: the frame is provably identical to the accepted one;
        // neither the per-frame vectors nor the ensure call are needed.
        if !keyed_skip {
            // Fast path: no collapsed cells — use original slices directly.
            if !has_collapsed {
                let mut cell_revisions: Vec<u64> =
                    Vec::with_capacity(app.history.len() + active_entries.len());
                cell_revisions.extend(
                    app.history_revisions
                        .iter()
                        .copied()
                        .map(history_entry_revision),
                );
                if !active_entries.is_empty() {
                    let active_rev = app.active_cell_revision;
                    for i in 0..active_entries.len() {
                        let salt = (i as u64).wrapping_add(1);
                        cell_revisions.push(active_entry_revision(active_rev, salt));
                    }
                }
                // Build identity mapping: filtered index == original index.
                // Reused across frames; identity maps are rebuilt only when the
                // row count changes.
                let row_count = app.history.len() + active_entries.len();
                if app.collapsed_cell_map.len() != row_count {
                    app.collapsed_cell_map = (0..row_count).collect();
                }

                let shards: [&[HistoryCell]; 2] = [&app.history, active_entries];
                app.viewport.transcript_cache.ensure_split(
                    &shards,
                    &cell_revisions,
                    transcript_width,
                    render_options,
                    &app.cell_folds,
                    None,
                    provisional_action_owner,
                );
            } else {
                // Slow path: borrow non-collapsed cells into a filtered ref list
                // so collapsed cells are excluded from rendering, and build the
                // filtered→original index mapping. Collapsed run starts render a
                // synthetic summary cell borrowed from the generation cache.
                // No history cells or summary bodies are cloned on scroll frames.
                // Which rows survive is cached per projection generation; only
                // the revisions are gathered fresh (#6652).
                let filtered = &app.tool_run_cache.filtered;
                let active_rev = app.active_cell_revision;
                let mut filtered_cells: Vec<&HistoryCell> =
                    Vec::with_capacity(filtered.original.len());
                let mut filtered_revs: Vec<u64> = Vec::with_capacity(filtered.original.len());
                for &original in &filtered.original {
                    if original < history_len {
                        filtered_cells.push(&app.history[original]);
                        filtered_revs.push(history_entry_revision(app.history_revisions[original]));
                    } else {
                        let active_index = original - history_len;
                        filtered_cells.push(&active_entries[active_index]);
                        filtered_revs.push(active_entry_revision(
                            active_rev,
                            (active_index as u64).wrapping_add(1),
                        ));
                    }
                }
                for &slot in &filtered.summary_slots {
                    if let Some((summary, revision)) = summary_cells.get(&filtered.original[slot]) {
                        filtered_cells[slot] = summary;
                        filtered_revs[slot] = *revision;
                    }
                }
                app.collapsed_cell_map.clone_from(&filtered.original);

                app.viewport.transcript_cache.ensure_filtered(
                    &filtered_cells,
                    &filtered_revs,
                    transcript_width,
                    render_options,
                    &app.cell_folds,
                    Some(&app.collapsed_cell_map),
                    provisional_action_owner,
                );
            }
        }

        let (mut total_lines, mut top, mut was_explicit_tail) =
            resolve_transcript_viewport_after_layout(&mut app.viewport, visible_lines);

        // A sticky prompt is layout chrome, not a synthetic transcript row.
        // First resolve against the full viewport, then reserve one real row
        // only when the prompt has actually scrolled above it. Resolving once
        // more with the smaller body keeps the newest tail line visible.
        let mut transcript_area = content_area;
        let mut pinned_prompt = (app.pin_last_prompt && content_area.height > 1)
            .then(|| {
                scrolled_user_prompt_pin(
                    &app.history,
                    app.viewport.transcript_cache.line_meta(),
                    &app.collapsed_cell_map,
                    top,
                    content_area.width,
                )
            })
            .flatten();
        let visible_lines = if pinned_prompt.is_some() {
            transcript_area.y = transcript_area.y.saturating_add(1);
            transcript_area.height = transcript_area.height.saturating_sub(1);
            let visible = usize::from(transcript_area.height);
            (total_lines, top, was_explicit_tail) =
                resolve_transcript_viewport_after_layout(&mut app.viewport, visible);
            // The approved pin follows the final viewport after its row is
            // reserved, including a newer prompt that was at the old top.
            pinned_prompt = scrolled_user_prompt_pin(
                &app.history,
                app.viewport.transcript_cache.line_meta(),
                &app.collapsed_cell_map,
                top,
                content_area.width,
            );
            visible
        } else {
            visible_lines
        };
        let owner = app.transcript_action_owner();
        let index_map = has_collapsed.then_some(app.collapsed_cell_map.as_slice());
        app.viewport.transcript_cache.retarget(owner, index_map);

        // The cache has now observed this revision (or the cell was filtered,
        // in which case a later reveal must cold-render). Start the next append
        // receipt from the current revision instead of chaining across an
        // already-consumed proof.
        if let Some(receipt) = app.streaming_source_receipt.as_mut() {
            receipt.from_revision = receipt.to_revision;
        }

        let line_meta = app.viewport.transcript_cache.line_meta();

        // If the user scrolled back to the live tail, the per-stream
        // "leave me alone" lock is over — new chunks should pin to bottom
        // again until they explicitly scroll up. Without this clear, content
        // piles up off-screen below the visible area and the view appears
        // frozen at the moment they returned to bottom.
        //
        // Only clear the lock when the user's INTENT was tail (their
        // stored state was already `to_bottom()` before resolve), AND
        // when the transcript actually has scrolling room to talk about
        // — if everything fits in one screen, "tail" is trivially true
        // and clearing here would yank the user back to bottom on the
        // next chunk even though they explicitly scrolled up.
        if was_explicit_tail && total_lines > visible_lines {
            app.user_scrolled_during_stream = false;
        }

        app.viewport.last_transcript_area = Some(transcript_area);
        app.viewport.last_transcript_padding_top = 0;

        let end = (top + visible_lines).min(total_lines);
        let mut lines = if total_lines == 0 {
            vec![Line::from("")]
        } else {
            app.viewport.transcript_cache.lines()[top..end].to_vec()
        };
        let mut line_links = if total_lines == 0 {
            vec![Vec::new()]
        } else {
            app.viewport.transcript_cache.line_links()[top..end].to_vec()
        };

        if !app.low_motion
            && app.fancy_animations
            && let (Some(start), Some(started)) = (
                app.ocean_receipt_settle_start,
                app.ocean_completion_started_at,
            )
        {
            apply_receipt_settle_cascade(
                &mut lines,
                top,
                line_meta,
                &app.collapsed_cell_map,
                &app.history,
                start,
                started.elapsed().as_millis(),
            );
        }

        // Brief flash highlight on the most recently sent user message. It is
        // a one-shot transition, so Reduced/Still clear the timestamp instead
        // of leaving a stale flash waiting for a later state-change redraw.
        if app.motion_policy().allows_decorative() {
            if let Some(send_at) = app.last_send_at {
                if send_at.elapsed() < SEND_FLASH_DURATION {
                    apply_send_flash(
                        &mut lines,
                        top,
                        &app.history,
                        line_meta,
                        &app.collapsed_cell_map,
                    );
                } else {
                    app.last_send_at = None;
                }
            }
        } else {
            app.last_send_at = None;
        }

        // No background "highlight" for the Alt+V detail target: it used to
        // paint the target cell's spans `Color::Reset`, which is invisible on
        // terminal-owned themes and a black hole on painted surfaces
        // (Underwater, Shoreline): the cell's text sat on the terminal's own
        // background, and CJK trailing columns left black remnants when the
        // cell scrolled (#6704). The footer's `Alt+V:details` hint names it.
        apply_selection(&mut lines, top, app);

        if let Some(pin) = pinned_prompt {
            lines.insert(0, pin);
            line_links.insert(0, Vec::new());
        }

        // The HTML contract is a top-first ledger. Bottom-padding the short
        // transcript made every newly wrapped stream line shift all prior
        // rows upward, producing repeated thousand-cell repaints and the
        // visible "slab" motion recorded in live QA. Empty-state centering is
        // handled separately; active work starts at the top and appends in
        // place until scrolling is genuinely necessary.
        app.viewport.last_transcript_padding_top = 0;

        let scrollbar = (total_lines > visible_lines && transcript_area.width > 1).then_some(
            TranscriptScrollbar {
                top,
                visible: visible_lines,
                total: total_lines,
            },
        );
        let jump_to_latest_button =
            if app.use_mouse_capture && !app.viewport.transcript_scroll.is_at_tail() {
                jump_to_latest_button_rect(transcript_area, scrollbar.is_some())
            } else {
                None
            };
        app.viewport.jump_to_latest_button_area = jump_to_latest_button;

        Self {
            content_area,
            transcript_area,
            lines,
            line_links,
            scrollbar,
            jump_to_latest_button,
            background,
            ocean_column,
            ocean_activity,
            ambient_inks,
            ocean_elapsed_ms,
            ocean_animated,
            life_presence_fixed,
            fish_flee_elapsed_ms,
            // Fish also accompany intentional transcript browsing in the
            // selected underwater scene. They only occupy blank cells and are
            // collision-checked, so history stays legible while the ocean
            // remains playful when scrolling upward.
            ambient_life: underwater_atmosphere
                && !app.attention_hold_active()
                && (browsing_history
                    || matches!(phase, ShellPhase::Working | ShellPhase::Verifying)
                    || completion_life_active),
            scroll_track,
            scroll_thumb,
            jump_border,
            jump_arrow,
        }
    }
}

fn apply_receipt_settle_cascade(
    lines: &mut [Line<'static>],
    top: usize,
    line_meta: &[TranscriptLineMeta],
    filtered_to_original: &[usize],
    history: &[HistoryCell],
    start: usize,
    elapsed_ms: u128,
) {
    for (visible_index, line) in lines.iter_mut().enumerate() {
        let Some((filtered_cell, _)) = line_meta
            .get(top + visible_index)
            .and_then(TranscriptLineMeta::cell_line)
        else {
            continue;
        };
        let original_cell = filtered_to_original
            .get(filtered_cell)
            .copied()
            .unwrap_or(filtered_cell);
        if original_cell < start
            || !matches!(
                history.get(original_cell),
                Some(HistoryCell::Tool(_) | HistoryCell::SubAgent(_))
            )
            || !receipt_is_settling(original_cell - start, elapsed_ms)
        {
            continue;
        }
        for span in &mut line.spans {
            span.style = span.style.add_modifier(Modifier::DIM);
        }
    }
}

#[must_use]
fn receipt_is_settling(receipt_order: usize, elapsed_ms: u128) -> bool {
    let delay = u128::try_from(receipt_order.min(6)).unwrap_or(6) * 70;
    elapsed_ms < delay + 140
}

fn tool_run_summary_cell(run: &ToolRun) -> HistoryCell {
    HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
        name: "activity_group".to_string(),
        status: ToolStatus::Success,
        input_summary: Some(crate::tui::history::tool_run_summary(run)),
        output: None,
        prompts: None,
        spillover_path: None,
        output_summary: Some(format!("+{}", run.count)),
        is_diff: false,
    }))
}

fn tool_run_summary_revision(
    run: &ToolRun,
    revisions: &[u64],
    history_len: usize,
    active_rev: u64,
) -> u64 {
    let mut revision = 0xA11C_EA5E_D00D_2692u64 ^ ((run.start as u64) << 32) ^ (run.count as u64);
    for idx in run.start..run.start.saturating_add(run.count) {
        let cell_revision = revisions
            .get(idx)
            .copied()
            .map(history_entry_revision)
            .unwrap_or_else(|| {
                let active_idx = idx.saturating_sub(history_len);
                active_entry_revision(active_rev, (active_idx as u64).wrapping_add(1))
            });
        revision = revision.rotate_left(7) ^ cell_revision;
    }
    let extends_into_active = run.start.saturating_add(run.count) > history_len;
    revision_in_domain(revision, extends_into_active)
}

const ACTIVE_REVISION_DOMAIN: u64 = 1 << 63;

fn revision_in_domain(revision: u64, active: bool) -> u64 {
    // The top bit is exclusively a cache-domain tag. Clearing it means raw
    // counters that differ only by bit 63 can theoretically alias within one
    // domain after 2^63 updates; that lifetime is acceptable, while active and
    // committed-history keys must never alias each other.
    let payload = revision & !ACTIVE_REVISION_DOMAIN;
    if active {
        ACTIVE_REVISION_DOMAIN | payload
    } else {
        payload
    }
}

fn history_entry_revision(revision: u64) -> u64 {
    revision_in_domain(revision, false)
}

/// Whole-vector freshness key for the transcript cache's O(1) steady-state
/// short-circuit (`TranscriptViewCache::note_revision_key`). Composition is
/// contractual: every mutation path that can change a cell revision supplied
/// to `ensure_*` moves at least one component — `history_version` (bumped by
/// push/insert/extend/clear/truncate/bump_history_cell/mark_history_updated),
/// history length, the active cell revision, the active entry count — and
/// every path that can change the collapsed-row projection moves the
/// `ToolRunCache` generation. Mirrors the copy in `widgets::mod`.
fn transcript_revision_key(
    history_version: u64,
    history_len: usize,
    active_cell_revision: u64,
    active_len: usize,
    projection_generation: u64,
) -> u64 {
    let mut key = history_version;
    key ^= (history_len as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    key = key.rotate_left(17) ^ active_cell_revision;
    key ^ (active_len as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ projection_generation.wrapping_mul(0x1656_67B1_9E37_79F9)
}

pub(crate) fn active_entry_revision(active_rev: u64, salt: u64) -> u64 {
    // Active entries and committed history cells can occupy the same
    // positional cache slot across `flush_active_cell`. Keep their revision
    // domains distinct so the first active entry (`active_rev = 0`,
    // `salt = 1`) cannot collide with the first history revision (`1`) and
    // reuse a stale `running` render after cancellation.
    let mixed = active_rev
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(salt);
    revision_in_domain(mixed, true)
}

/// Build the last-user-prompt header when that message is above the resolved
/// transcript viewport. The caller owns the one-row layout reservation so
/// the header never masquerades as `top` or displaces the newest tail line.
fn scrolled_user_prompt_pin(
    history: &[HistoryCell],
    line_meta: &[TranscriptLineMeta],
    collapsed_cell_map: &[usize],
    top: usize,
    width: u16,
) -> Option<Line<'static>> {
    if width == 0 || top == 0 {
        return None;
    }
    // Freeze the approved viewport-relative pin semantics while retaining
    // this renderer's independent full-buffer and layout comparison.
    let user_first_line = |meta: &TranscriptLineMeta| -> Option<usize> {
        let TranscriptLineMeta::CellLine {
            cell_index,
            line_in_cell: 0,
            ..
        } = meta
        else {
            return None;
        };
        let original = collapsed_cell_map
            .get(*cell_index)
            .copied()
            .unwrap_or(*cell_index);
        let content = match history.get(original) {
            Some(HistoryCell::User { content }) => content,
            _ => return None,
        };
        if content.lines().next().unwrap_or("").trim().is_empty() {
            return None;
        }
        Some(original)
    };
    let orig_idx = line_meta.iter().take(top).rev().find_map(user_first_line)?;
    let content = match history.get(orig_idx) {
        Some(HistoryCell::User { content }) => content,
        _ => return None,
    };

    let first = content.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        return None;
    }
    let budget = usize::from(width.saturating_sub(4)).max(1);
    let mut shown = String::new();
    let mut used = 0usize;
    for ch in first.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > budget {
            break;
        }
        shown.push(ch);
        used += w;
    }
    if used < UnicodeWidthStr::width(first) && !shown.is_empty() {
        shown.push('…');
    }

    Some(Line::from(vec![
        Span::styled(
            format!("{} ", crate::tui::glyphs::USER),
            Style::default()
                .fg(palette::WHALE_HUMAN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(shown, Style::default().fg(palette::TEXT_PRIMARY)),
    ]))
}

impl Renderable for ChatWidget {
    fn render(&self, _area: Rect, buf: &mut Buffer) {
        // Use the passed render area, not self.content_area — those can
        // drift when layout changes (e.g. file-tree pane toggle), and
        // using the stale self.content_area is the root cause of text
        // bleed-through (#400). In debug builds, assert the two match to
        // catch future drift early.
        debug_assert_eq!(
            _area, self.content_area,
            "ChatWidget content_area drifted from render area: \
             content_area={:?} render_area={:?}",
            self.content_area, _area
        );

        let area = _area;
        // Repaint the full chat area with the codewhale-ink background each
        // frame. Ratatui's `Paragraph` only writes cells that contain text,
        // so cells the current frame's paragraph doesn't touch would
        // otherwise hold the *previous* frame's contents (the `:24Z`
        // timestamp-tail bleed-through reported in v0.8.5 testing). Using
        // `Clear` reset cells to terminal default, which read as a brown-
        // gray on most user setups; an explicit ink fill keeps the chat
        // area on-brand.
        Block::default()
            .style(Style::default().bg(self.background))
            .render(area, buf);

        let paragraph =
            Paragraph::new(self.lines.clone()).style(Style::default().bg(self.background));
        paragraph.render(area, buf);

        self.render_underwater_field(area, buf);

        // Link targets travel beside the wrapped lines, never inside Span
        // content. Convert relative line columns to absolute viewport regions
        // for the backend; clip the final column when a scrollbar owns it.
        let link_area = Rect {
            width: area
                .width
                .saturating_sub(u16::from(self.scrollbar.is_some())),
            ..area
        };
        let regions = crate::tui::osc8::link_regions_for_lines(link_area, &self.line_links);
        crate::tui::osc8::set_frame_links(regions);

        if let Some(scrollbar) = self.scrollbar {
            let scrollable_range = scrollbar.total.saturating_sub(scrollbar.visible);
            let mut state = ScrollbarState::new(scrollable_range)
                .position(scrollbar.top.min(scrollable_range))
                .viewport_content_length(scrollbar.visible);
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("│"))
                .track_style(Style::default().fg(self.scroll_track))
                .thumb_symbol("┃")
                .thumb_style(Style::default().fg(self.scroll_thumb))
                .render(self.transcript_area, buf, &mut state);
        }

        if let Some(button_area) = self.jump_to_latest_button {
            render_jump_to_latest_button(
                button_area,
                buf,
                self.background,
                self.jump_border,
                self.jump_arrow,
            );
        }

        // Hover: register OSC-8 link regions (copyable), then apply aura.
        let link_area = Rect {
            width: area
                .width
                .saturating_sub(u16::from(self.scrollbar.is_some())),
            ..area
        };
        for region in crate::tui::osc8::link_regions_for_lines(link_area, &self.line_links) {
            let width = region
                .col_end
                .saturating_sub(region.col_start)
                .saturating_add(1);
            let hit = Rect::new(region.col_start, region.row, width, 1);
            crate::tui::hover_layer::register_rect(
                crate::tui::hover_hit::HoverTargetKind::Link,
                hit,
                region.target,
                true,
            );
        }
    }

    fn desired_height(&self, _width: u16) -> u16 {
        1
    }
}

impl ChatWidget {
    /// Paint the explicitly selected underwater field. Flat keeps the theme
    /// surface, Solarized Light keeps canonical Base3, and Terminal keeps its
    /// inherited background without inherited aquarium decoration.
    fn render_underwater_field(&self, area: Rect, buf: &mut Buffer) {
        if let Some(column) = self.ocean_column {
            // Cache per-row ocean colors; invalidate only on phase/size/breath.
            let phase_tag = column.phase_tag();
            let fingerprint = column.ramp_fingerprint();
            let ramp = crate::tui::ambient_life::frame_ocean_ramp(
                &column,
                area.height,
                area.y,
                self.ocean_elapsed_ms,
                phase_tag,
                fingerprint,
            );
            for local_y in 0..area.height {
                let protected = self
                    .lines
                    .get(usize::from(local_y))
                    .and_then(occupied_text_bounds);
                let row_bg = ramp
                    .get(usize::from(local_y))
                    .copied()
                    .unwrap_or_else(|| column.color_at_y(area.y.saturating_add(local_y)));
                for local_x in 0..area.width {
                    let is_protected = protected.is_some_and(|(start, end)| {
                        usize::from(local_x) >= start && usize::from(local_x) < end
                    });
                    let cell = &mut buf[(area.x + local_x, area.y + local_y)];
                    // Plain transcript text participates in the water column;
                    // explicit semantic surfaces (selection, code, warnings)
                    // retain their own background.
                    if !is_protected || cell.bg == self.background {
                        cell.set_bg(row_bg);
                    }
                }
            }
        }

        if self.ambient_life
            && let Some(inks) = self.ambient_inks
        {
            // The scatter has a centre. It used to be column 0 with a row in
            // the middle of the field, which is neither where the school
            // swims nor anywhere the eye is: the flee proximity test
            // (|dy| < 6) could not even fire on a tall field, and when it did
            // every fish was to the right of the anchor so the whole school
            // slid the same way. Anchored on the composer's centre line and
            // the school's own band, a turn beginning reads as the shoal
            // parting around the thing that just happened.
            let cursor = crate::tui::ambient_life::AmbientCursor {
                column: area.x.saturating_add(area.width / 2),
                row: area
                    .y
                    .saturating_add(crate::tui::ambient_life::school_band_row(area)),
                flee_elapsed_ms: self.fish_flee_elapsed_ms,
            };
            // Whale cameo rides the completion breath clock when present.
            let whale = crate::tui::ambient_life::WhaleCameo {
                elapsed_ms: self.ocean_column.and_then(|c| c.completion_elapsed_ms()),
                anchor_x: area.x.saturating_add(area.width / 2),
                anchor_y: area.y.saturating_add(area.height.saturating_mul(2) / 3),
            };
            // Per-frame budget counters (built/painted/skipped/clipped);
            // consumed by ambient-life tests and debug tooling, not by the
            // widget itself.
            let _ambient_stats = crate::tui::ambient_life::render_ambient_life(
                area,
                buf,
                inks,
                &self.lines,
                self.ocean_elapsed_ms,
                self.ocean_presence_f32(),
                cursor,
                whale,
                self.ocean_activity,
            );
            if let Some(column) = self.ocean_column {
                crate::tui::ambient_life::apply_caustic_shimmer(
                    area,
                    buf,
                    &column,
                    self.ocean_elapsed_ms,
                    self.ocean_animated,
                    &self.lines,
                );
            }
        }
    }
}

impl ChatWidget {
    /// Life presence as a 0..=1 fraction; drives ambient-life ink fading.
    fn ocean_presence_f32(&self) -> f32 {
        (f32::from(self.life_presence_fixed) / 1000.0).clamp(0.0, 1.0)
    }
}

fn occupied_text_bounds(line: &Line<'_>) -> Option<(usize, usize)> {
    crate::tui::ambient_life::occupied_text_bounds(line)
}

fn jump_to_latest_button_rect(area: Rect, has_scrollbar: bool) -> Option<Rect> {
    if area.width < JUMP_TO_LATEST_BUTTON_WIDTH + u16::from(has_scrollbar)
        || area.height < JUMP_TO_LATEST_BUTTON_HEIGHT
    {
        return None;
    }

    let scrollbar_gutter = u16::from(has_scrollbar);
    Some(Rect {
        x: area
            .x
            .saturating_add(area.width)
            .saturating_sub(scrollbar_gutter)
            .saturating_sub(JUMP_TO_LATEST_BUTTON_WIDTH),
        y: area
            .y
            .saturating_add(area.height)
            .saturating_sub(JUMP_TO_LATEST_BUTTON_HEIGHT),
        width: JUMP_TO_LATEST_BUTTON_WIDTH,
        height: JUMP_TO_LATEST_BUTTON_HEIGHT,
    })
}

fn render_jump_to_latest_button(
    area: Rect,
    buf: &mut Buffer,
    background: Color,
    border: Color,
    arrow: Color,
) {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(background))
        .render(area, buf);

    let arrow_x = area.x.saturating_add(1);
    let arrow_y = area.y.saturating_add(1);
    buf[(arrow_x, arrow_y)]
        .set_symbol("↓")
        .set_style(Style::default().fg(arrow).add_modifier(Modifier::BOLD));
}

fn apply_selection(lines: &mut [Line<'static>], top: usize, app: &App) {
    let Some((start, end)) = app.viewport.transcript_selection.ordered_endpoints() else {
        return;
    };

    let selection_style = Style::default()
        .bg(app.ui_theme.selection_bg)
        .fg(palette::SELECTION_TEXT);

    for (idx, line) in lines.iter_mut().enumerate() {
        let line_index = top + idx;
        if line_index < start.line_index || line_index > end.line_index {
            continue;
        }

        let (col_start, col_end) = if start.line_index == end.line_index {
            (start.column, end.column)
        } else if line_index == start.line_index {
            (start.column, usize::MAX)
        } else if line_index == end.line_index {
            (0, end.column)
        } else {
            (0, usize::MAX)
        };

        if col_start == 0 && col_end == usize::MAX {
            for span in &mut line.spans {
                span.style = span.style.patch(selection_style);
            }
            continue;
        }

        line.spans = apply_selection_to_line(line, col_start, col_end, selection_style);
    }
}

/// Apply a brief background tint to the last user message's visible lines.
fn apply_send_flash(
    lines: &mut [Line<'static>],
    top: usize,
    history: &[HistoryCell],
    line_meta: &[TranscriptLineMeta],
    original_index_map: &[usize],
) {
    // Find the last User cell index.
    let last_user_cell = history
        .iter()
        .rposition(|cell| matches!(cell, HistoryCell::User { .. }));
    let Some(target_cell) = last_user_cell else {
        return;
    };

    let flash_bg = palette::SURFACE_TOOL_ACTIVE; // subtle dark-blue tint

    for (idx, line) in lines.iter_mut().enumerate() {
        let line_index = top + idx;
        if let Some(TranscriptLineMeta::CellLine { cell_index, .. }) = line_meta.get(line_index)
            && original_index_map
                .get(*cell_index)
                .copied()
                .unwrap_or(*cell_index)
                == target_cell
        {
            for span in &mut line.spans {
                span.style = span.style.bg(flash_bg);
            }
        }
    }
}

fn apply_selection_to_line(
    line: &Line<'static>,
    col_start: usize,
    col_end: usize,
    selection_style: Style,
) -> Vec<Span<'static>> {
    let mut result = Vec::with_capacity(line.spans.len().saturating_add(2));
    let mut current_col = 0usize;

    for span in &line.spans {
        let span_text: &str = span.content.as_ref();
        let span_width = text_display_width(span_text);
        let span_end = current_col.saturating_add(span_width);

        if span_end <= col_start || current_col >= col_end {
            result.push(span.clone());
        } else if current_col >= col_start && span_end <= col_end {
            result.push(Span::styled(
                span.content.clone(),
                span.style.patch(selection_style),
            ));
        } else {
            let mut before = String::new();
            let mut selected = String::new();
            let mut after = String::new();
            let mut grapheme_col = current_col;

            for grapheme in span_text.graphemes(true) {
                let grapheme_width = grapheme_display_width(grapheme);
                let grapheme_start = grapheme_col;
                let grapheme_end = grapheme_col.saturating_add(grapheme_width);
                if grapheme_end <= col_start {
                    before.push_str(grapheme);
                } else if grapheme_start >= col_end {
                    after.push_str(grapheme);
                } else {
                    selected.push_str(grapheme);
                }
                grapheme_col = grapheme_end;
            }

            if !before.is_empty() {
                result.push(Span::styled(before, span.style));
            }
            if !selected.is_empty() {
                result.push(Span::styled(selected, span.style.patch(selection_style)));
            }
            if !after.is_empty() {
                result.push(Span::styled(after, span.style));
            }
        }

        current_col = span_end;
    }

    result
}

// Frozen source boundary.

pub(super) fn selected_spans(
    line: &Line<'static>,
    start: usize,
    end: usize,
    style: Style,
) -> Vec<Span<'static>> {
    apply_selection_to_line(line, start, end, style)
}

// Public test adapter outside the byte-exact frozen production fragments.
pub(super) fn snapshot(app: &mut App, area: Rect, elapsed_ms: u128) -> ChatWidget {
    ChatWidget::new_with_ocean_elapsed(app, area, elapsed_ms)
}
