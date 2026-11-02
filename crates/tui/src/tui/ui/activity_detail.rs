//! Reasoning Detail, Turn Inspector, raw tool-detail, and pager-text helpers
//! extracted from `ui.rs` (issue #4103).
//!
//! Ctrl+O opens the full recorded Reasoning Detail timeline for the selected
//! reasoning block or the current/latest turn. The whole-turn Turn Inspector
//! moved to a dedicated surface (Ctrl+Alt+O and `/turn inspect`). The `v` raw
//! tool-details pager (including #500 spillover folding), copy-cell actions, and
//! footer detail labels live here too.

use crate::tui::app::App;
use crate::tui::footer_ui::one_line_summary;
use crate::tui::history::text::{history_cell_to_clipboard_text, history_cell_to_text};
use crate::tui::history::{HistoryCell, ToolCell, ToolStatus, tool_status_label};
use crate::tui::pager::{PagerPage, PagerView};
use codewhale_localization::{MessageId, tr};
use codewhale_snapshot::SnapshotRepo;
use codewhale_tui_kit::ui_text::truncate_line_to_width;

fn selected_transcript_cell_index(app: &App) -> Option<usize> {
    if let Some(owner) = app.selection_owner_cell() {
        return Some(owner);
    }
    app.viewport
        .transcript_selection
        .ordered_endpoints()
        .and_then(|(start, _)| {
            app.viewport
                .transcript_cache
                .line_meta()
                .get(start.line_index)
                .and_then(|meta| meta.cell_line())
                .map(|(cell_index, _)| app.original_cell_index_for_rendered(cell_index))
        })
}

/// Open the full recorded-reasoning detail pager for the selected thinking
/// block, or for the current/latest turn when no reasoning block is selected.
/// Ctrl+O routes here; only provider-supplied reasoning is shown.
pub(super) fn open_reasoning_detail_pager(app: &mut App) -> bool {
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(80);
    let Some(text) = reasoning_detail_text(app) else {
        app.status_message = Some("No reasoning detail available".to_string());
        return true;
    };
    app.view_stack.push(PagerView::from_text(
        "Reasoning Detail",
        &text,
        width.saturating_sub(2),
    ));
    true
}

/// Resolve the turn range that contains the given virtual cell index.
/// The turn starts at the most recent user cell at or before the index and
/// ends at the next user cell after the index, or the end of the transcript.
fn turn_range_for_index(app: &App, index: usize) -> (usize, usize) {
    let end = app.virtual_cell_count();
    let start = (0..index.saturating_add(1))
        .rev()
        .find(|&idx| {
            matches!(
                app.cell_at_virtual_index(idx),
                Some(HistoryCell::User { .. })
            )
        })
        .unwrap_or(0);
    let turn_end = (index..end)
        .find(|&idx| {
            idx > index
                && matches!(
                    app.cell_at_virtual_index(idx),
                    Some(HistoryCell::User { .. })
                )
        })
        .unwrap_or(end);
    (start, turn_end)
}

/// Assemble the full recorded reasoning for the selected thinking block's
/// turn, or for the current/latest turn when nothing is selected. Empty
/// chunks are surfaced as "(no reasoning text recorded)" rather than invented.
pub(super) fn reasoning_detail_text(app: &App) -> Option<String> {
    let selected = selected_transcript_cell_index(app).filter(|&idx| {
        matches!(
            app.cell_at_virtual_index(idx),
            Some(HistoryCell::Thinking { .. })
        )
    });
    let (start, end) = selected
        .map(|idx| turn_range_for_index(app, idx))
        .unwrap_or_else(|| current_turn_range(app));
    reasoning_timeline_text(app, selected, start, end)
}

/// Build the full recorded-reasoning text for a turn-scoped set of thinking
/// cells. Only provider-supplied reasoning Codewhale actually recorded is
/// shown; nothing is fabricated when a chunk is empty.
pub(super) fn reasoning_timeline_text(
    app: &App,
    selected_cell_index: Option<usize>,
    start: usize,
    end: usize,
) -> Option<String> {
    let thinking_indices: Vec<usize> = (start..end)
        .filter(|&idx| {
            matches!(
                app.cell_at_virtual_index(idx),
                Some(HistoryCell::Thinking { .. })
            )
        })
        .collect();
    if thinking_indices.is_empty() {
        return None;
    }

    let selected_position = selected_cell_index.and_then(|selected| {
        thinking_indices
            .iter()
            .position(|&idx| idx == selected)
            .map(|idx| idx + 1)
    });
    let total = thinking_indices.len();
    let running = thinking_indices.iter().any(|&idx| {
        matches!(
            app.cell_at_virtual_index(idx),
            Some(HistoryCell::Thinking {
                streaming: true,
                ..
            })
        )
    });

    let mut sections = Vec::new();
    if let Some(turn_id) = app.runtime_turn_id.as_ref() {
        let status = humanized_turn_status(app);
        sections.push(format!("Turn {} \u{00B7} {status}", short_turn_id(turn_id)));
    }
    sections.push("Activity: reasoning timeline".to_string());
    sections.push(format!(
        "Status: {} · {total} chunk{}",
        if running { "running" } else { "done" },
        if total == 1 { "" } else { "s" }
    ));
    if let Some(position) = selected_position {
        sections.push(format!("Selected chunk: {position} of {total}"));
        if position > 1 {
            let previous_index = thinking_indices[position - 2];
            let preview = thinking_chunk_preview(app, previous_index);
            sections.push(format!(
                "Previous chunk: {} of {total} - {preview}",
                position - 1
            ));
        }
        if position < total {
            let next_index = thinking_indices[position];
            let preview = thinking_chunk_preview(app, next_index);
            sections.push(format!(
                "Next chunk: {} of {total} - {preview}",
                position + 1
            ));
        }
    }
    sections.push(String::new());

    for (position, cell_index) in thinking_indices.iter().copied().enumerate() {
        let Some(HistoryCell::Thinking {
            content,
            streaming,
            duration_secs,
        }) = app.cell_at_virtual_index(cell_index)
        else {
            continue;
        };
        let position = position + 1;
        let marker = if Some(position) == selected_position {
            " (selected)"
        } else {
            ""
        };
        let mut status = if *streaming {
            "running".to_string()
        } else {
            "done".to_string()
        };
        if let Some(duration_secs) = duration_secs {
            status.push_str(" · ");
            status.push_str(&codewhale_command_contract::elapsed::format_elapsed_ms(
                (duration_secs * 1000.0) as u64,
            ));
        }
        sections.push(format!("Thinking chunk {position} of {total}{marker}"));
        sections.push(format!("Status: {status}"));
        let body = content.trim();
        if body.is_empty() {
            sections.push("(no reasoning text recorded)".to_string());
        } else {
            sections.push(body.to_string());
        }
        sections.push(String::new());
    }

    Some(sections.join("\n"))
}

fn thinking_chunk_preview(app: &App, cell_index: usize) -> String {
    let Some(HistoryCell::Thinking { content, .. }) = app.cell_at_virtual_index(cell_index) else {
        return "thinking".to_string();
    };
    let preview = one_line_summary(content, 64);
    if preview.is_empty() {
        "thinking".to_string()
    } else {
        preview
    }
}

fn activity_cell_label(app: &App, cell_index: usize, cell: &HistoryCell) -> String {
    match cell {
        HistoryCell::Thinking { .. } => "thinking".to_string(),
        HistoryCell::Error { .. } => "error".to_string(),
        HistoryCell::SubAgent(_) => "sub-agent".to_string(),
        HistoryCell::Tool(ToolCell::Generic(generic)) => {
            crate::tui::widgets::tool_card::tool_activity_label_for_name(
                &generic.name,
                app.ui_locale,
            )
        }
        HistoryCell::Tool(_) => {
            detail_target_label(app, cell_index).unwrap_or_else(|| "tool activity".to_string())
        }
        _ => "message".to_string(),
    }
}

fn tool_duration_for_activity(tool: &ToolCell) -> Option<u64> {
    match tool {
        ToolCell::Exec(cell) => cell.duration_ms.or_else(|| {
            (cell.status == ToolStatus::Running).then(|| {
                u64::try_from(
                    cell.started_at
                        .map(|started| started.elapsed().as_millis())
                        .unwrap_or_default(),
                )
                .unwrap_or(u64::MAX)
            })
        }),
        _ => None,
    }
}

/// Empty-state hint shown when the selection has no raw leaf detail to open.
/// `v` / `Alt+V` only ever surface the raw detail of the ONE selected
/// tool/card/leaf, so when there is nothing leaf-level to show we point the
/// user at Ctrl+Alt+O for the whole-turn context instead of failing silently
/// (#4105).
const NO_RAW_DETAIL_HINT: &str =
    "No raw detail for this item — press Ctrl+Alt+O for the turn overview.";

/// Intro line prepended to the raw tool-detail pager body so the surface reads
/// as the raw detail of the single selected item — not the whole turn.
/// Ctrl+Alt+O is now the whole-turn Turn Inspector (#v092-reasoning-fix).
const RAW_DETAIL_PAGER_INTRO: &str =
    "Raw detail for the selected item — press Ctrl+Alt+O for the whole-turn overview.";

pub(super) fn open_tool_details_pager(app: &mut App) -> bool {
    let Some(cell_index) = detail_target_cell_index(app) else {
        app.status_message = Some(NO_RAW_DETAIL_HINT.to_string());
        return false;
    };
    open_details_pager_for_cell(app, cell_index)
}

pub(super) fn spillover_pager_reference(app: &App, cell_index: usize) -> Option<(String, String)> {
    use crate::tui::history::{GenericToolCell, HistoryCell, ToolCell};

    let session_id = app
        .current_session_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())?;
    let artifact = app
        .tool_detail_record_for_cell(cell_index)
        .and_then(|detail| {
            app.session_artifacts.iter().find(|artifact| {
                artifact.kind == codewhale_runtime::artifacts::ArtifactKind::ToolOutput
                    && artifact.tool_call_id == detail.tool_id
                    && artifact.session_id == session_id
            })
        });
    let reference = if let Some(artifact) = artifact {
        artifact.storage_path.to_str()?
    } else if let Some(HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
        spillover_path: Some(path),
        ..
    }))) = app.cell_at_virtual_index(cell_index)
    {
        path.to_str()?
    } else {
        return None;
    };
    if reference.len() > codewhale_engine::tools::tool_result_retrieval::MAX_REFERENCE_BYTES {
        return None;
    }
    Some((session_id.to_string(), reference.to_string()))
}

pub(crate) fn open_details_pager_for_cell(app: &mut App, cell_index: usize) -> bool {
    if let Some(detail) = crate::tui::shell_job_routing::shell_job_for_cell(app, cell_index) {
        crate::tui::shell_job_routing::open_shell_job_pager(app, &detail);
        return true;
    }
    if let Some(detail) = app.tool_detail_record_for_cell(cell_index) {
        let input = serde_json::to_string_pretty(&detail.input)
            .unwrap_or_else(|_| detail.input.to_string());
        let output = detail.output.as_deref().map_or(
            "(not available)".to_string(),
            std::string::ToString::to_string,
        );

        let retained = spillover_pager_reference(app, cell_index);
        let mutation_section = match app.cell_at_virtual_index(cell_index) {
            Some(HistoryCell::Tool(ToolCell::PatchSummary(cell))) => {
                cell.receipt.as_ref().map(|receipt| {
                    format!(
                        "── {} ──\n{}",
                        codewhale_localization::tr(
                            app.ui_locale,
                            codewhale_localization::MessageId::FileMutationExactHeading
                        ),
                        receipt.inspect_text(app.ui_locale)
                    )
                })
            }
            _ => None,
        };

        // Frame the recorded detail at leaf scope; retained bytes use the
        // owned evidence page rather than being joined into this body.
        let trailing_sections = [mutation_section]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n\n");
        let content = if !trailing_sections.is_empty() {
            format!(
                "{RAW_DETAIL_PAGER_INTRO}\n\nTool ID: {}\nTool: {}\n\nInput:\n{}\n\nOutput:\n{}\n\n{}",
                detail.tool_id, detail.tool_name, input, output, trailing_sections
            )
        } else {
            format!(
                "{RAW_DETAIL_PAGER_INTRO}\n\nTool ID: {}\nTool: {}\n\nInput:\n{}\n\nOutput:\n{}",
                detail.tool_id, detail.tool_name, input, output
            )
        };

        let width = app
            .viewport
            .last_transcript_area
            .map(|area| area.width)
            .unwrap_or(80);
        let mut pager = PagerView::from_text(
            format!("Raw detail — {}", detail.tool_name),
            &content,
            width.saturating_sub(2),
        );
        if let Some((session, reference)) = retained {
            pager = pager.with_owned_evidence(app.workspace.clone(), session, reference);
        }
        app.view_stack.push(pager);
        return true;
    }

    let Some(cell) = app.cell_at_virtual_index(cell_index) else {
        app.status_message = Some(NO_RAW_DETAIL_HINT.to_string());
        return false;
    };
    let title = match cell {
        HistoryCell::User { .. } => "You".to_string(),
        HistoryCell::Assistant { .. } => "Assistant".to_string(),
        HistoryCell::System { .. } => "Note".to_string(),
        HistoryCell::Error { .. } => "Error".to_string(),
        HistoryCell::Thinking { .. } => "Reasoning".to_string(),
        HistoryCell::Tool(_) => "Message".to_string(),
        HistoryCell::SubAgent(_) => "Sub-agent".to_string(),
        HistoryCell::Automation(_) => tr(app.ui_locale, MessageId::AutomationNoun).into_owned(),
        HistoryCell::ArchivedContext { .. } => "Archived Context".to_string(),
    };
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(80);
    let content = if matches!(cell, HistoryCell::Tool(_)) {
        history_cell_to_clipboard_text(cell, width)
    } else {
        history_cell_to_text(cell, width)
    };
    let mut pager = PagerView::from_text(title, &content, width.saturating_sub(2));
    // A completed assistant cell gets a clean `a` (copy answer) action so
    // this raw-detail pager can hand over the answer text without the
    // glyph/label scaffolding that `c`/`y` (rendered body) would include.
    if let Some(answer) = completed_assistant_answer_text(cell, width) {
        pager = pager.with_copy_answer(answer);
    }
    if let Some((session, reference)) = spillover_pager_reference(app, cell_index) {
        pager = pager.with_owned_evidence(app.workspace.clone(), session, reference);
    }
    app.view_stack.push(pager);
    true
}

/// Open the focused transcript cell as a full-screen readable pager.
pub(crate) fn open_focused_cell_pager(app: &mut App) -> bool {
    let Some(cell_index) = detail_target_cell_index(app) else {
        return false;
    };
    let Some(cell) = app.cell_at_virtual_index(cell_index) else {
        return false;
    };
    let title = match cell {
        HistoryCell::User { .. } => "You".to_string(),
        HistoryCell::Assistant { .. } => "Assistant".to_string(),
        HistoryCell::System { .. } => "Note".to_string(),
        HistoryCell::Error { .. } => "Error".to_string(),
        HistoryCell::Thinking { .. } => "Reasoning".to_string(),
        HistoryCell::Tool(_) => "Tool".to_string(),
        HistoryCell::SubAgent(_) => "Sub-agent".to_string(),
        HistoryCell::Automation(_) => tr(app.ui_locale, MessageId::AutomationNoun).into_owned(),
        HistoryCell::ArchivedContext { .. } => "Archived Context".to_string(),
    };
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(80);
    let content = if matches!(cell, HistoryCell::Tool(_)) {
        history_cell_to_clipboard_text(cell, width)
    } else {
        history_cell_to_text(cell, width)
    };
    let mut pager = PagerView::from_text(title, &content, width.saturating_sub(2));
    if let Some(answer) = completed_assistant_answer_text(cell, width) {
        pager = pager.with_copy_answer(answer);
    }
    app.view_stack.push(pager);
    true
}

/// Copy the "focused" transcript cell to the system clipboard.
/// The focused cell is determined by the detail-target heuristic
/// (viewport centre or most recent cell). Returns true when text
/// was actually copied.
pub(super) fn copy_focused_cell(app: &mut App) -> bool {
    let cell_index = detail_target_cell_index(app);
    let Some(index) = cell_index else {
        return false;
    };
    copy_cell_to_clipboard(app, index)
}

pub(crate) fn copy_cell_to_clipboard(app: &mut App, cell_index: usize) -> bool {
    let Some(cell) = app.cell_at_virtual_index(cell_index) else {
        app.status_message = Some("No message at that line".to_string());
        return false;
    };
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(80);
    let text = history_cell_to_clipboard_text(cell, width);
    if text.trim().is_empty() {
        app.status_message = Some("Message is empty".to_string());
        return false;
    }
    if let Ok(transport) = app.clipboard.write_text_status(&text) {
        app.status_message = Some(crate::tui::mouse_ui::copy_receipt(
            app,
            transport,
            "Message copied",
        ));
        true
    } else {
        app.status_message = Some("Copy failed".to_string());
        false
    }
}

/// Clean clipboard payload for a completed assistant answer cell.
///
/// Selection reuses the typed `HistoryCell::is_completed_assistant_answer`
/// projection so reasoning/thinking blocks, tool calls and results, runtime
/// status, and still-streaming partials never qualify; serialization reuses
/// `history_cell_to_clipboard_text`, the canonical clean-copy path that
/// returns the authored assistant Markdown with no glyph/label scaffolding.
pub(crate) fn completed_assistant_answer_text(cell: &HistoryCell, width: u16) -> Option<String> {
    cell.is_completed_assistant_answer()
        .then(|| history_cell_to_clipboard_text(cell, width))
}

/// Latest completed assistant answer inside the virtual-cell range
/// `[start, end)` — the payload behind the Turn Inspector's `a` (copy
/// answer) action. Scanning backwards keeps each inspector page scoped to
/// its own turn, so the latest page carries the latest completed answer.
fn turn_answer_payload(app: &App, start: usize, end: usize, width: u16) -> Option<String> {
    (start..end).rev().find_map(|idx| {
        app.cell_at_virtual_index(idx)
            .and_then(|cell| completed_assistant_answer_text(cell, width))
    })
}

pub(super) fn detail_target_cell_index(app: &App) -> Option<usize> {
    if let Some(owner) = app.selection_owner_cell() {
        return Some(owner);
    }
    if let Some((start, _)) = app.viewport.transcript_selection.ordered_endpoints() {
        return app
            .viewport
            .transcript_cache
            .line_meta()
            .get(start.line_index)
            .and_then(|meta| meta.cell_line())
            .map(|(cell_index, _)| app.original_cell_index_for_rendered(cell_index));
    }
    app.detail_cell_index_for_viewport(
        app.viewport.last_transcript_top,
        app.viewport.last_transcript_visible.max(1),
        app.viewport.transcript_cache.line_meta(),
    )
    .or_else(|| app.virtual_cell_count().checked_sub(1))
}

pub(crate) fn detail_target_label(app: &App, cell_index: usize) -> Option<String> {
    if let Some(detail) = app.tool_detail_record_for_cell(cell_index) {
        return Some(detail.tool_name.clone());
    }
    let cell = app.cell_at_virtual_index(cell_index)?;
    match cell {
        HistoryCell::Tool(ToolCell::Exec(exec)) => {
            Some(format!("run {}", one_line_summary(&exec.command, 80)))
        }
        HistoryCell::Tool(ToolCell::Exploring(explore)) => Some(format!(
            "workspace {} item{}",
            explore.entries.len(),
            if explore.entries.len() == 1 { "" } else { "s" }
        )),
        HistoryCell::Tool(ToolCell::PlanUpdate(_)) => Some("legacy plan update".to_string()),
        HistoryCell::Tool(ToolCell::PatchSummary(patch)) => Some(format!("patch {}", patch.path)),
        HistoryCell::Tool(ToolCell::Review(review)) => {
            let target = one_line_summary(&review.target, 80);
            Some(if target.is_empty() {
                "review".to_string()
            } else {
                format!("review {target}")
            })
        }
        HistoryCell::Tool(ToolCell::Mcp(mcp)) => Some(format!("tool {}", mcp.tool)),
        HistoryCell::Tool(ToolCell::ViewImage(image)) => {
            Some(format!("image {}", image.path.display()))
        }
        HistoryCell::Tool(ToolCell::WebSearch(search)) => Some(format!("search {}", search.query)),
        HistoryCell::Tool(ToolCell::Generic(generic)) => Some(
            crate::tui::widgets::tool_card::tool_activity_label_for_name(
                &generic.name,
                app.ui_locale,
            ),
        ),
        HistoryCell::SubAgent(_) => Some("sub-agent".to_string()),
        HistoryCell::Error { .. } => Some("full error message".to_string()),
        _ => None,
    }
}

pub(super) fn extract_reasoning_header(text: &str) -> Option<String> {
    let start = text.find("**")?;
    let rest = &text[start + 2..];
    let end = rest.find("**")?;
    let header = rest[..end].trim().trim_end_matches(':');
    if header.is_empty() {
        None
    } else {
        Some(header.to_string())
    }
}

// ============================================================================
// Turn Inspector (issue #4104)
//
// Ctrl+O opens a *turn-level* overview of the current in-flight turn — or the
// latest completed turn when idle — rather than the single-cell Activity
// Detail. `v` / `Alt+V` remain the raw leaf-detail command for the selected
// item; this surface never dumps a single tool's raw output.
//
// Each of the nine overview sections renders from whatever turn/cell/app state
// is cleanly reachable and DEGRADES the rest gracefully to a short "none"/"—"
// line — never a mysterious blank. The thinner sections (diagnostics loop,
// tests/verifier) are intentionally heuristic in this first pass; the leaf
// issues #4106/#4107/#4108 flesh them out with structured data later.
// ============================================================================

/// Open the whole-turn Turn Inspector pager (Ctrl+O).
///
/// Reuses the same `PagerView` text-section machinery as the Activity Detail
/// pager — no new modal system. Always succeeds: an empty transcript still
/// yields a coherent (degraded) overview rather than a dead keypress.
pub(super) fn open_turn_inspector_pager(app: &mut App) -> bool {
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(80);
    let ranges = turn_ranges(app);
    let page_count = ranges.len();
    let pages = ranges
        .into_iter()
        .enumerate()
        .map(|(page_index, (start, end))| {
            let latest = page_index + 1 == page_count;
            let text =
                turn_inspector_text_for_range(app, start, end, page_index, page_count, latest);
            let page = PagerPage::from_text("Turn Inspector", &text, width.saturating_sub(2))
                .with_copy_text(text);
            // `a` copies only this turn's final assistant answer — the clean
            // counterpart to `e` (whole-turn handoff markdown).
            let page = match turn_answer_payload(app, start, end, width) {
                Some(answer) => page.with_copy_answer(answer),
                None => page,
            };
            if latest {
                // The existing handoff remains attached only to the
                // current/latest turn it actually describes.
                page.with_export_markdown(turn_handoff_markdown(app))
            } else {
                page
            }
        })
        .collect();
    app.view_stack
        .push(PagerView::from_pages(pages, page_count.saturating_sub(1)));
    true
}

/// Chronological virtual-cell ranges for every recorded turn. A transcript
/// without a user prompt still gets one coherent page, matching the previous
/// Turn Inspector empty/degraded behavior.
fn turn_ranges(app: &App) -> Vec<(usize, usize)> {
    let end = app.virtual_cell_count();
    let starts: Vec<usize> = (0..end)
        .filter(|&idx| {
            matches!(
                app.cell_at_virtual_index(idx),
                Some(HistoryCell::User { .. })
            )
        })
        .collect();
    if starts.is_empty() {
        return vec![(0, end)];
    }
    starts
        .iter()
        .copied()
        .enumerate()
        .map(|(idx, start)| (start, starts.get(idx + 1).copied().unwrap_or(end)))
        .collect()
}

/// Virtual-cell range `[start, end)` of the turn under inspection.
///
/// The turn is the run of cells from the last user prompt through the end of
/// the transcript. Because `virtual_cell_count()` includes still-in-flight
/// `active_cell` entries, this scopes to the current in-flight turn during a
/// turn, and to the latest completed turn once the active cell has flushed to
/// history. When no user prompt exists yet the whole transcript is used.
fn current_turn_range(app: &App) -> (usize, usize) {
    let end = app.virtual_cell_count();
    let start = (0..end)
        .rev()
        .find(|&idx| {
            matches!(
                app.cell_at_virtual_index(idx),
                Some(HistoryCell::User { .. })
            )
        })
        .unwrap_or(0);
    (start, end)
}

/// Human form of the runtime turn status — raw enum-ish values like
/// "in_progress" must never reach the inspector (dogfood A6, #4102).
fn humanized_turn_status(app: &App) -> &str {
    match app.runtime_turn_status.as_deref() {
        Some("in_progress") | None => "in progress",
        Some(other) => other,
    }
}

/// Short display form of a runtime turn id. The full UUID reads as internal
/// state in the inspector header (dogfood A6); twelve characters is plenty
/// to correlate with logs.
fn short_turn_id(turn_id: &str) -> &str {
    turn_id.get(..12).unwrap_or(turn_id)
}

/// Assemble the Turn Inspector overview text from all available turn data.
#[cfg(test)]
pub(super) fn turn_inspector_text(app: &App) -> String {
    let (start, end) = current_turn_range(app);
    turn_inspector_text_for_range(app, start, end, 0, 1, true)
}

fn turn_inspector_text_for_range(
    app: &App,
    start: usize,
    end: usize,
    page_index: usize,
    page_count: usize,
    latest: bool,
) -> String {
    let mut out: Vec<String> = Vec::new();

    // Turn identity header. Lead with the human turn number and status; the
    // id is a short correlation suffix, never a raw UUID dump (dogfood A6).
    let status = if latest {
        std::borrow::Cow::Borrowed(humanized_turn_status(app))
    } else {
        tr(app.ui_locale, MessageId::AutomationRunStatusCompleted)
    };
    if !latest {
        let historical_offset = page_count.saturating_sub(page_index + 1) as u64;
        let number = app
            .turn_counter
            .checked_sub(historical_offset)
            .filter(|number| *number > 0)
            .unwrap_or(page_index as u64 + 1);
        out.push(format!("Turn #{number} \u{00B7} {status}"));
    } else if app.turn_counter > 0 {
        let mut line = format!("Turn #{} \u{00B7} {status}", app.turn_counter);
        if let Some(turn_id) = app.runtime_turn_id.as_ref() {
            line.push_str(&format!(" \u{00B7} id {}", short_turn_id(turn_id)));
        }
        out.push(line);
    } else if let Some(turn_id) = app.runtime_turn_id.as_ref() {
        out.push(format!("Turn {} \u{00B7} {status}", short_turn_id(turn_id)));
    } else {
        out.push("Turn: \u{2014} (no turn recorded yet)".to_string());
    }
    // Restate the Ctrl+O (overview) vs. Alt+V/⌥V (raw leaf detail) contract so
    // the two surfaces never get confused. Bare `v` is never a details shortcut.
    let details = crate::tui::shell_key_routing::display_chord(
        crate::tui::shell_key_routing::binding(
            crate::tui::shell_key_routing::ShellBindingId::ToolDetails,
        )
        .footer_chord,
    );
    if latest {
        out.push(format!(
            "Overview of the current/latest turn · press {details} for the selected item's raw detail"
        ));
    }

    push_section(&mut out, "Intent", vec![turn_intent_line(app, start)]);

    if latest && let Some(line) = selected_item_context_line(app) {
        push_section(&mut out, "Selected item", vec![line]);
    }

    push_section(
        &mut out,
        "To-do",
        if latest {
            turn_todo_lines(app)
        } else {
            Vec::new()
        },
    );
    let mut timeline = turn_full_conversation_lines(app, start, end);
    if !timeline.is_empty() {
        timeline.push(String::new());
    }
    timeline.extend(turn_timeline_lines(app, start, end));
    push_section(&mut out, "Turn timeline", timeline);
    push_section(
        &mut out,
        "Files changed",
        turn_files_changed(app, start, end),
    );
    push_section(
        &mut out,
        "Diagnostics loop",
        if latest {
            turn_diagnostics_lines(app)
        } else {
            Vec::new()
        },
    );
    push_section(
        &mut out,
        "Tests / verifier",
        turn_verifier_lines(app, start, end),
    );
    push_section(
        &mut out,
        "Approvals / denials",
        if latest {
            turn_approvals_lines(app)
        } else {
            Vec::new()
        },
    );
    push_section(
        &mut out,
        "Model route + tokens/cost",
        if latest {
            turn_route_lines(app)
        } else {
            Vec::new()
        },
    );
    push_section(
        &mut out,
        "Final result / status",
        turn_result_lines(app, start, end, ResultDetail::Full),
    );

    out.join("\n")
}

/// Source-faithful, turn-scoped transcript for the inspector page. The normal
/// transcript can stay compact/folded; this explicit detail surface preserves
/// complete recorded input, reasoning, tool results, and assistant output.
fn turn_full_conversation_lines(app: &App, start: usize, end: usize) -> Vec<String> {
    let thinking_total = (start..end)
        .filter(|&idx| {
            matches!(
                app.cell_at_virtual_index(idx),
                Some(HistoryCell::Thinking { .. })
            )
        })
        .count();
    let mut thinking_position = 0usize;
    let mut out = Vec::new();

    for idx in start..end {
        let Some(cell) = app.cell_at_virtual_index(idx) else {
            continue;
        };
        let tag = match cell {
            HistoryCell::User { .. } => "[›]".to_string(),
            HistoryCell::Thinking { streaming, .. } => {
                thinking_position += 1;
                format!(
                    "[∿ {} {thinking_position}/{thinking_total} · {}]",
                    tr(app.ui_locale, MessageId::PhaseReasoning),
                    tr(
                        app.ui_locale,
                        if *streaming {
                            MessageId::AutomationRunStatusRunning
                        } else {
                            MessageId::PhaseDone
                        }
                    )
                )
            }
            HistoryCell::Tool(_) => format!("[⚙ {}]", tr(app.ui_locale, MessageId::PhaseUsingTool)),
            HistoryCell::SubAgent(_) => "[↗]".to_string(),
            HistoryCell::Assistant { streaming, .. } => format!(
                "[◆ · {}]",
                tr(
                    app.ui_locale,
                    if *streaming {
                        MessageId::AutomationRunStatusRunning
                    } else {
                        MessageId::PhaseDone
                    }
                )
            ),
            HistoryCell::Error { .. } => "[!]".to_string(),
            HistoryCell::Automation(_) => "[⏱]".to_string(),
            HistoryCell::System { .. } | HistoryCell::ArchivedContext { .. } => "[i]".to_string(),
        };
        if !out.is_empty() {
            out.push(String::new());
        }
        out.push(tag);
        let body = history_cell_to_clipboard_text(cell, 120);
        if body.trim().is_empty() {
            out.push("—".to_string());
        } else {
            out.push(body);
        }
    }

    out
}

/// Build a compact, pasteable Markdown handoff of the current/latest turn
/// (issue #4108).
///
/// Reuses the exact same turn scope (`current_turn_range`) and the same
/// per-section data helpers as the Turn Inspector (#4104), so the handoff can
/// never drift from what Ctrl+O shows — it only re-renders that data as
/// Markdown headings + bullets instead of the inspector's box-drawn rules.
/// Unavailable sections degrade to a short `—` (and the optional Plan section
/// is dropped entirely when empty) so the artifact stays paste-ready without
/// leaving a heading over a blank void — the same graceful-degrade contract the
/// inspector already follows.
pub(crate) fn turn_handoff_markdown(app: &App) -> String {
    let (start, end) = current_turn_range(app);
    let mut out: Vec<String> = Vec::new();

    // Title + identity — turn id when known, else the turn counter, else a
    // bare heading so an empty transcript still yields a coherent artifact.
    let heading = if app.turn_counter > 0 {
        format!("# Turn handoff — Turn #{}", app.turn_counter)
    } else if let Some(turn_id) = app.runtime_turn_id.as_ref() {
        format!("# Turn handoff — {}", short_turn_id(turn_id))
    } else {
        "# Turn handoff".to_string()
    };
    out.push(heading);

    let status = match app.runtime_turn_status.as_deref() {
        Some("in_progress") => "in progress",
        Some(other) => other,
        None => "idle",
    };
    out.push(format!(
        "_Status: {status} · generated {}_",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    ));

    push_md_section(&mut out, "Intent", vec![turn_intent_line(app, start)]);

    // To-do is optional context: include it only when the canonical list has
    // items, keeping the handoff compact without recreating a second plan.
    let todos = turn_todo_lines(app);
    if !todos.is_empty() {
        push_md_section(&mut out, "To-do", md_bullets(todos));
    }

    push_md_section(
        &mut out,
        "Files changed",
        md_bullets(turn_files_changed(app, start, end)),
    );
    push_md_section(
        &mut out,
        "Turn timeline",
        md_bullets(turn_timeline_lines(app, start, end)),
    );
    push_md_section(
        &mut out,
        "Tests / verifier",
        md_bullets(turn_verifier_lines(app, start, end)),
    );
    push_md_section(
        &mut out,
        "Model route + tokens/cost",
        md_bullets(turn_route_lines(app)),
    );
    push_md_section(
        &mut out,
        "Result / status",
        md_bullets(turn_result_lines(app, start, end, ResultDetail::Compact)),
    );

    // Trailing newline keeps the artifact clean when pasted into a PR body.
    out.push(String::new());
    out.join("\n")
}

/// Append a `## Title` Markdown section. An empty body degrades to a single
/// `—` line so a heading is never followed by a void — the Markdown analogue
/// of [`push_section`]'s `none` degrade.
fn push_md_section(out: &mut Vec<String>, title: &str, body: Vec<String>) {
    out.push(String::new());
    out.push(format!("## {title}"));
    if body.is_empty() {
        out.push("—".to_string());
    } else {
        out.extend(body);
    }
}

/// Convert Turn Inspector section lines into Markdown bullet rows. Inspector
/// list helpers prefix rows with `• `; swap that for `- `, and bullet the
/// key/value rows (route, tokens, status) too so the whole section is valid
/// Markdown.
fn md_bullets(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| {
            let body = line.strip_prefix("• ").unwrap_or(line.as_str());
            format!("- {body}")
        })
        .collect()
}

/// Append a `── Title ──` section. An empty body degrades to a single
/// `none` line so the section header is never followed by a blank void.
fn push_section(out: &mut Vec<String>, title: &str, body: Vec<String>) {
    out.push(String::new());
    out.push(format!("── {title} ──"));
    if body.is_empty() {
        out.push("none".to_string());
    } else {
        out.extend(body);
    }
}

/// Section 1 — intent / user-prompt summary for the turn.
fn turn_intent_line(app: &App, start: usize) -> String {
    if let Some(HistoryCell::User { content }) = app.cell_at_virtual_index(start) {
        let summary = one_line_summary(content, 240);
        if !summary.is_empty() {
            return summary;
        }
    }
    if let Some(prompt) = app.last_submitted_prompt.as_deref() {
        let summary = one_line_summary(prompt, 240);
        if !summary.is_empty() {
            return summary;
        }
    }
    "—".to_string()
}

/// Optional selected-item context. The first view is the turn overview, but
/// when the user has an activity cell selected we surface it plus the Alt+V
/// affordance so the Ctrl+O / Alt+V split stays discoverable.
fn selected_item_context_line(app: &App) -> Option<String> {
    let idx = selected_transcript_cell_index(app)?;
    let cell = app.cell_at_virtual_index(idx)?;
    let label = truncate_line_to_width(&activity_cell_label(app, idx, cell), 48);
    let hint = if app.cell_has_detail_target(idx) {
        let details = crate::tui::shell_key_routing::display_chord(
            crate::tui::shell_key_routing::binding(
                crate::tui::shell_key_routing::ShellBindingId::ToolDetails,
            )
            .footer_chord,
        );
        if matches!(cell, HistoryCell::Error { .. }) {
            format!(" · {details} opens the full error")
        } else {
            format!(" · {details} opens its raw detail")
        }
    } else {
        String::new()
    };
    Some(format!("{label}{hint}"))
}

/// Section 2 — canonical To-do state.
fn turn_todo_lines(app: &App) -> Vec<String> {
    let mut lines = Vec::new();

    if let Ok(todos) = app.todos.try_lock() {
        let snapshot = todos.snapshot();
        if !snapshot.items.is_empty() {
            lines.push(format!("To-do: {}% settled", snapshot.completion_pct));
            for item in &snapshot.items {
                lines.push(format!(
                    "{} {}",
                    todo_status_glyph(&item.status),
                    truncate_line_to_width(&item.content, 72)
                ));
            }
        }
    }

    lines
}

fn todo_status_glyph(status: &codewhale_engine::tools::todo::TodoStatus) -> &'static str {
    match status {
        codewhale_engine::tools::todo::TodoStatus::Completed => "[x]",
        codewhale_engine::tools::todo::TodoStatus::InProgress => "[~]",
        codewhale_engine::tools::todo::TodoStatus::Pending => "[ ]",
        codewhale_engine::tools::todo::TodoStatus::Cancelled => "[-]",
    }
}

/// Section 3 — chronological turn timeline with compact action affordances.
fn turn_timeline_lines(app: &App, start: usize, end: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for idx in start..end {
        let Some(cell) = app.cell_at_virtual_index(idx) else {
            continue;
        };
        match cell {
            HistoryCell::User { content } => {
                let summary = one_line_summary(content, 96);
                rows.push(timeline_row("user prompt", &summary, None, None, &[]));
            }
            HistoryCell::Thinking {
                content,
                streaming,
                duration_secs,
            } => {
                let summary = one_line_summary(content, 88);
                let status = streaming.then_some("running").unwrap_or("done");
                let duration = duration_secs.map(|secs| {
                    codewhale_command_contract::elapsed::format_elapsed_ms((secs * 1000.0) as u64)
                });
                let actions = timeline_cell_actions(app, idx, cell);
                rows.push(timeline_row(
                    "reasoning",
                    &summary,
                    Some(status),
                    duration.as_deref(),
                    &actions,
                ));
            }
            HistoryCell::Tool(tool) => {
                let (kind, summary) = timeline_tool_summary(app, idx, tool);
                let duration = tool_duration_for_activity(tool)
                    .map(codewhale_command_contract::elapsed::format_elapsed_ms);
                let status = tool
                    .status()
                    .map(|status| tool_status_label(status, app.ui_locale));
                let actions = timeline_cell_actions(app, idx, cell);
                rows.push(timeline_row(
                    kind,
                    &summary,
                    status.as_deref(),
                    duration.as_deref(),
                    &actions,
                ));
            }
            HistoryCell::SubAgent(_) => {
                let summary = detail_target_label(app, idx).unwrap_or_else(|| "sub-agent".into());
                let actions = timeline_cell_actions(app, idx, cell);
                rows.push(timeline_row("sub-agent", &summary, None, None, &actions));
            }
            HistoryCell::Assistant { content, streaming } => {
                let summary = one_line_summary(content, 96);
                let status = streaming.then_some("streaming").unwrap_or("done");
                rows.push(timeline_row(
                    "assistant result",
                    &summary,
                    Some(status),
                    None,
                    &[],
                ));
            }
            HistoryCell::Error { message, severity } => {
                let summary = one_line_summary(message, 96);
                let status = severity.to_string();
                rows.push(timeline_row("error", &summary, Some(&status), None, &[]));
            }
            HistoryCell::Automation(cell) => {
                let summary = one_line_summary(&cell.plain_summary(), 96);
                rows.push(timeline_row("automation", &summary, None, None, &[]));
            }
            HistoryCell::System { content }
            | HistoryCell::ArchivedContext {
                summary: content, ..
            } => {
                let summary = one_line_summary(content, 96);
                rows.push(timeline_row("system note", &summary, None, None, &[]));
            }
        }
    }
    rows.push(turn_checkpoint_timeline_row(app));
    rows.into_iter()
        .enumerate()
        .map(|(idx, row)| format!("{}. {row}", idx + 1))
        .collect()
}

fn timeline_tool_summary(app: &App, idx: usize, tool: &ToolCell) -> (&'static str, String) {
    match tool {
        ToolCell::Exec(exec) if command_looks_like_verifier(&exec.command) => {
            ("test/verifier", truncate_line_to_width(&exec.command, 88))
        }
        ToolCell::Exec(exec) => ("shell command", truncate_line_to_width(&exec.command, 88)),
        ToolCell::Exploring(explore) => (
            "read/search",
            format!(
                "{} item{}",
                explore.entries.len(),
                if explore.entries.len() == 1 { "" } else { "s" }
            ),
        ),
        ToolCell::PlanUpdate(_) => ("legacy plan", "Legacy plan metadata replayed".to_string()),
        ToolCell::PatchSummary(patch) => {
            let summary = one_line_summary(&patch.summary, 72);
            if summary.is_empty() {
                ("edit", truncate_line_to_width(&patch.path, 88))
            } else {
                (
                    "edit",
                    truncate_line_to_width(&format!("{} — {summary}", patch.path), 88),
                )
            }
        }
        ToolCell::Review(review) => {
            let target = one_line_summary(&review.target, 88);
            (
                "review",
                if target.is_empty() {
                    "code review".to_string()
                } else {
                    target
                },
            )
        }
        ToolCell::Mcp(mcp) => ("MCP tool", truncate_line_to_width(&mcp.tool, 88)),
        ToolCell::ViewImage(image) => (
            "image",
            truncate_line_to_width(&image.path.display().to_string(), 88),
        ),
        ToolCell::WebSearch(search) => ("web search", truncate_line_to_width(&search.query, 88)),
        ToolCell::Generic(generic) => {
            let mut label =
                detail_target_label(app, idx).unwrap_or_else(|| generic.name.replace('_', " "));
            if let Some(input) = generic.input_summary.as_deref().map(str::trim)
                && !input.is_empty()
            {
                label.push_str(" · ");
                label.push_str(input);
            }
            (
                generic_tool_timeline_kind(generic),
                truncate_line_to_width(&label, 88),
            )
        }
    }
}

fn generic_tool_timeline_kind(generic: &crate::tui::history::GenericToolCell) -> &'static str {
    let name = generic.name.as_str();
    if generic.is_diff || name.contains("diff") {
        "diff"
    } else if matches!(name, "read_file" | "list_files" | "glob" | "grep_files")
        || name.contains("read")
        || name.contains("search")
        || name.contains("grep")
    {
        "read/search"
    } else if matches!(name, "apply_patch" | "edit_file" | "write_file")
        || name.contains("patch")
        || name.contains("edit")
        || name.contains("write")
    {
        "edit"
    } else if name.contains("approval") {
        "approval"
    } else if name.contains("diagnostic") || name.contains("lsp") {
        "diagnostics"
    } else {
        "tool"
    }
}

fn timeline_cell_actions(app: &App, idx: usize, cell: &HistoryCell) -> Vec<String> {
    let mut actions = Vec::new();
    if app.cell_has_detail_target(idx) {
        let details = crate::tui::shell_key_routing::display_chord(
            crate::tui::shell_key_routing::binding(
                crate::tui::shell_key_routing::ShellBindingId::ToolDetails,
            )
            .footer_chord,
        );
        // Diff-bearing cells open their diff through the same details chord;
        // bare `v` / `d` always type text (TUI-DOG-002), so no bare-key claim.
        let is_diff = matches!(cell, HistoryCell::Tool(ToolCell::PatchSummary(_)))
            || matches!(
                cell,
                HistoryCell::Tool(ToolCell::Generic(generic)) if generic.is_diff
            );
        if is_diff {
            actions.push(format!("{details} diff"));
        } else if matches!(cell, HistoryCell::Error { .. }) {
            actions.push(format!("{details} full error"));
        } else {
            actions.push(format!("{details} raw detail"));
        }
    }
    actions
}

fn timeline_row(
    kind: &str,
    summary: &str,
    status: Option<&str>,
    duration: Option<&str>,
    actions: &[String],
) -> String {
    let mut line = if summary.trim().is_empty() {
        kind.to_string()
    } else {
        format!("{kind}: {}", summary.trim())
    };
    if let Some(status) = status.filter(|s| !s.trim().is_empty()) {
        line.push_str(" — ");
        line.push_str(status);
    }
    if let Some(duration) = duration.filter(|s| !s.trim().is_empty()) {
        line.push_str(" · ");
        line.push_str(duration);
    }
    if !actions.is_empty() {
        line.push_str(" · actions: ");
        line.push_str(&actions.join(", "));
    }
    line
}

fn turn_checkpoint_timeline_row(app: &App) -> String {
    if app.turn_counter == 0 {
        return "checkpoint: unavailable — no numbered turn snapshot yet · action: e export handoff"
            .to_string();
    }

    let repo = match SnapshotRepo::open_existing(&app.workspace) {
        Ok(Some(repo)) => repo,
        Ok(None) => {
            return "checkpoint: unavailable — no snapshot repo found · action: e export handoff"
                .to_string();
        }
        Err(err) => {
            return format!(
                "checkpoint: unknown — snapshot repo could not be opened ({}) · action: e export handoff",
                truncate_line_to_width(&err.to_string(), 72)
            );
        }
    };
    let snapshots = match repo.list(20) {
        Ok(snapshots) => snapshots,
        Err(err) => {
            return format!(
                "checkpoint: unknown — snapshot list failed ({}) · action: e export handoff",
                truncate_line_to_width(&err.to_string(), 72)
            );
        }
    };
    let prefix = format!("pre-turn:{}", app.turn_counter);
    let matching = snapshots
        .iter()
        .find(|snapshot| {
            snapshot.label == prefix || snapshot.label.starts_with(&format!("{prefix}:"))
        })
        .or_else(|| {
            snapshots
                .iter()
                .find(|snapshot| snapshot.label.starts_with("pre-turn:"))
        });
    if let Some(snapshot) = matching {
        let short = &snapshot.id.as_str()[..snapshot.id.as_str().len().min(8)];
        format!(
            "checkpoint: {} ({short}) available · actions: r restore via /restore (guarded), e export handoff",
            truncate_line_to_width(&snapshot.label, 72)
        )
    } else {
        "checkpoint: unavailable — no pre-turn snapshot found · action: e export handoff"
            .to_string()
    }
}

/// Section 4 — files touched by patch/diff tool cells in the turn.
fn turn_files_changed(app: &App, start: usize, end: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for idx in start..end {
        let Some(HistoryCell::Tool(tool)) = app.cell_at_virtual_index(idx) else {
            continue;
        };
        match tool {
            ToolCell::PatchSummary(patch) if seen.insert(patch.path.clone()) => {
                lines.push(format!(
                    "• {} — {}",
                    truncate_line_to_width(&patch.path, 60),
                    tool_status_label(patch.status, app.ui_locale)
                ));
            }
            _ => {}
        }
    }
    lines
}

/// Section 5 — diagnostics / LSP repair loop (#4107).
///
/// Shows the observable repair loop when LSP produced diagnostics this turn.
/// Stays quiet when LSP is disabled or no diagnostics were found.
fn turn_diagnostics_lines(app: &App) -> Vec<String> {
    if !app.lsp_enabled {
        return Vec::new();
    }
    let repair = &app.lsp_repair;
    if repair.diagnostics_found == 0 && !repair.injected && !repair.repair_attempted {
        return Vec::new();
    }
    let mut lines = Vec::new();
    if repair.diagnostics_found > 0 {
        lines.push(format!(
            "Found {} diagnostic{} across {} file{}",
            repair.diagnostics_found,
            if repair.diagnostics_found == 1 {
                ""
            } else {
                "s"
            },
            repair.files_touched.max(1),
            if repair.files_touched == 1 { "" } else { "s" },
        ));
    }
    lines.push(if repair.injected {
        "Injected into the next model request".to_string()
    } else {
        "Queued — not yet injected".to_string()
    });
    if repair.repair_attempted {
        lines.push("Model attempted a repair after injection".to_string());
    }
    let latest = match repair.latest {
        "resolved" => "Latest: resolved",
        "still_failing" => "Latest: still failing",
        "unavailable" => "Latest: unavailable",
        _ => "Latest: unknown",
    };
    lines.push(latest.to_string());
    lines
}

/// Section 6 — tests / verifier results.
///
/// Heuristic first pass (issue #4107): scans the turn's exec/review tool cells
/// for verifier-shaped commands and reports their status. Degrades to `none`
/// when nothing test-shaped ran.
fn turn_verifier_lines(app: &App, start: usize, end: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for idx in start..end {
        let Some(HistoryCell::Tool(tool)) = app.cell_at_virtual_index(idx) else {
            continue;
        };
        match tool {
            ToolCell::Exec(exec) if command_looks_like_verifier(&exec.command) => {
                lines.push(format!(
                    "• {} — {}",
                    truncate_line_to_width(&exec.command, 56),
                    tool_status_label(exec.status, app.ui_locale)
                ));
            }
            ToolCell::Review(review) => {
                let target = truncate_line_to_width(review.target.trim(), 48);
                let target = if target.is_empty() {
                    "review".to_string()
                } else {
                    format!("review {target}")
                };
                lines.push(format!(
                    "• {target} — {}",
                    tool_status_label(review.status, app.ui_locale)
                ));
            }
            _ => {}
        }
    }
    lines
}

fn command_looks_like_verifier(command: &str) -> bool {
    let lower = command.to_lowercase();
    [
        "test",
        "pytest",
        "jest",
        "cargo check",
        "cargo clippy",
        "verif",
        "lint",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Section 7 — approvals / denials.
///
/// "Approve for session" grants last the conversation; a Deny lasts only
/// the user turn it answered (cleared on `TurnStarted`), so each count names
/// its own scope.
fn turn_approvals_lines(app: &App) -> Vec<String> {
    let mut lines = Vec::new();
    let approved = app.approval_session_approved.len();
    let denied = app.approval_session_denied.len();
    if approved > 0 {
        lines.push(format!("Approved (session): {approved}"));
    }
    if denied > 0 {
        lines.push(format!("Denied (this turn): {denied}"));
    }
    lines
}

/// Section 8 — model route plus token/cost accounting.
fn turn_route_lines(app: &App) -> Vec<String> {
    let mut lines = Vec::new();

    let (provider, model) = if let Some(route) = app
        .active_turn
        .as_ref()
        .and_then(|turn| turn.route.as_ref())
    {
        let provider = if route.provider == codewhale_config::ProviderKind::Custom {
            route.provider_identity.clone()
        } else {
            route.provider.provider().display_name().to_string()
        };
        (provider, route.model.clone())
    } else {
        // Pending and last Auto routes use the same billing-authoritative
        // display contract as the header; do not fall back to `auto` after the
        // concrete turn route has resolved.
        app.effective_route_identity_display()
    };
    lines.push(format!("Route: {provider} · {model}"));

    let auto_receipt = app
        .active_turn
        .as_ref()
        .filter(|turn| turn.route.as_ref().is_some_and(|route| route.auto_model))
        .and_then(|turn| turn.auto_route_receipt.as_ref())
        .or_else(|| {
            app.pending_turn_route
                .as_ref()
                .filter(|(_, _, auto_model)| *auto_model)
                .and(app.pending_auto_route_receipt.as_ref())
        })
        .or_else(|| {
            app.auto_model
                .then_some(app.last_auto_route_receipt.as_ref())
                .flatten()
        });
    if let Some(receipt) = auto_receipt {
        lines.push(format!(
            "Auto decision: {} · {}",
            receipt.tier.label(),
            receipt.reason.label()
        ));
        let pair = receipt.pair.fast.as_deref().map_or_else(
            || format!("{} (no runnable fast sibling)", receipt.pair.strong),
            |fast| format!("{} strong · {fast} fast", receipt.pair.strong),
        );
        lines.push(format!("Auto pair: {pair}"));
        lines.push(format!("Auto scope: {}", receipt.scope.label()));
        lines.push(format!("Auto data: {}", receipt.data_path.label()));
        lines.extend(auto_router_receipt_lines(receipt));
    }

    let session = &app.session;
    match (session.last_prompt_tokens, session.last_completion_tokens) {
        (Some(prompt), Some(completion)) => {
            lines.push(format!(
                "Tokens (last turn): {prompt} in · {completion} out"
            ));
        }
        (Some(prompt), None) => lines.push(format!("Tokens (last turn): {prompt} in")),
        (None, Some(completion)) => lines.push(format!("Tokens (last turn): {completion} out")),
        (None, None) => {
            if session.total_tokens > 0 {
                lines.push(format!("Tokens (session): {}", session.total_tokens));
            }
        }
    }

    let chip = app.cumulative_usage_chip();
    match &chip {
        codewhale_engine::route_billing::UsageChip::Money(amount) => {
            lines.push(format!("Cost (session): {amount}"));
        }
        codewhale_engine::route_billing::UsageChip::PricedSubtotal { .. } => {
            lines.push(format!(
                "Cost (session): {}",
                codewhale_engine::route_billing::format_usage_chip(&chip, app.ui_locale)
                    .unwrap_or_default()
            ));
        }
        codewhale_engine::route_billing::UsageChip::Allowance { label, used_pct } => {
            lines.push(match used_pct {
                Some(pct) => format!("Usage plan: {label} ({pct:.0}% used)"),
                None => format!("Usage plan: {label}"),
            });
        }
        codewhale_engine::route_billing::UsageChip::Local => {
            lines.push("Cost: local".to_string());
        }
        codewhale_engine::route_billing::UsageChip::Unknown(_) => {
            lines.push(
                codewhale_engine::route_billing::format_usage_chip(&chip, app.ui_locale)
                    .unwrap_or_default(),
            );
        }
        codewhale_engine::route_billing::UsageChip::Hidden => {}
    }

    lines
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultDetail {
    /// Pager content is the review surface and must retain the complete final
    /// response. Width wrapping belongs to `PagerView`, not data assembly.
    Full,
    /// The exported handoff is intentionally a compact overview.
    Compact,
}

/// `/status` lines for the router behind an Auto receipt (#6525): the
/// decision-model answer, its cost and latency, and a failing router.
fn auto_router_receipt_lines(
    receipt: &codewhale_engine::model_routing::AutoRouteReceipt,
) -> Vec<String> {
    let percent = |bp: u16| format!("{}%", (u32::from(bp) + 50) / 100);
    let mut lines = Vec::new();
    if let Some(decision) = receipt.decision.as_ref() {
        let cost = decision.provider_reported_cost_usd.as_deref().map_or_else(
            || "cost not reported".to_string(),
            |cost| format!("${cost} (provider-reported)"),
        );
        let model = decision
            .response_model
            .as_deref()
            .map(|model| format!(" · {model}"))
            .unwrap_or_default();
        lines.push(format!(
            "Auto router: decision model{model} · {} ms · {cost}",
            decision.latency_ms
        ));
        let mut probabilities: Vec<(&String, &u16)> = decision.probabilities_bp.iter().collect();
        probabilities.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        let probabilities = probabilities
            .into_iter()
            .map(|(option, bp)| format!("{option} {}", percent(*bp)))
            .collect::<Vec<_>>()
            .join(" · ");
        let mut choice = format!(
            "Auto choice: {} ({probabilities}) · confidence {} (min {})",
            decision.choice,
            percent(decision.confidence_bp),
            percent(decision.min_confidence_bp)
        );
        if decision.cost_saving_kept_fast {
            choice.push_str(" · cost-saving kept fast");
        }
        if let Some(thinking) = decision.thinking.as_deref() {
            choice.push_str(&format!(" · thinking {thinking}"));
        }
        lines.push(choice);
    }
    if let Some(failure) = receipt.router_failure {
        lines.push(format!(
            "Auto router: failing — {} (used the local fallback)",
            failure.label()
        ));
    }
    lines
}

fn cleaned_turn_text(text: &str, detail: ResultDetail, max_width: usize) -> String {
    if detail == ResultDetail::Compact {
        return one_line_summary(text, max_width);
    }

    let mut cleaned = String::with_capacity(text.len());
    codewhale_tui_kit::osc8::strip_ansi_into(text, &mut cleaned);
    cleaned.trim().to_string()
}

/// Section 9 — final result / current status.
fn turn_result_lines(app: &App, start: usize, end: usize, detail: ResultDetail) -> Vec<String> {
    let mut lines = Vec::new();

    let status = match app.runtime_turn_status.as_deref() {
        Some("in_progress") => "in progress",
        Some(other) => other,
        None => "idle",
    };
    lines.push(format!("Status: {status}"));

    let final_text = (start..end)
        .rev()
        .find_map(|idx| match app.cell_at_virtual_index(idx) {
            Some(HistoryCell::Assistant { content, .. }) => {
                let text = cleaned_turn_text(content, detail, 200);
                (!text.is_empty()).then_some(text)
            }
            _ => None,
        });
    if let Some(text) = final_text {
        lines.push(format!("Result: {text}"));
    } else if status == "in progress" {
        lines.push("Result: turn still running".to_string());
    } else {
        lines.push("Result: —".to_string());
    }

    let error_text = (start..end)
        .rev()
        .find_map(|idx| match app.cell_at_virtual_index(idx) {
            Some(HistoryCell::Error { message, .. }) => {
                let text = cleaned_turn_text(message, detail, 160);
                (!text.is_empty()).then_some(text)
            }
            _ => None,
        });
    if let Some(err) = error_text {
        lines.push(format!("Error: {err}"));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{App, LspRepairState, TuiOptions};
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    fn test_app() -> App {
        let options = TuiOptions {
            model: "deepseek-v4-flash".to_string(),
            start_in_agent_mode: true,
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        App::new(options, &Config::default())
    }

    #[test]
    fn turn_diagnostics_lines_quiet_when_no_activity() {
        let mut app = test_app();
        app.lsp_enabled = true;
        assert!(turn_diagnostics_lines(&app).is_empty());
        app.lsp_enabled = false;
        assert!(turn_diagnostics_lines(&app).is_empty());
    }

    #[test]
    fn turn_diagnostics_lines_summarize_repair_loop() {
        let mut app = test_app();
        app.lsp_enabled = true;
        app.lsp_repair = LspRepairState {
            diagnostics_found: 2,
            files_touched: 1,
            injected: true,
            repair_attempted: true,
            latest: "still_failing",
        };
        let joined = turn_diagnostics_lines(&app).join("\n");
        assert!(joined.contains("Found 2 diagnostics"), "{joined}");
        assert!(
            joined.contains("Injected into the next model request"),
            "{joined}"
        );
        assert!(joined.contains("Model attempted a repair"), "{joined}");
        assert!(joined.contains("still failing"), "{joined}");
    }

    #[test]
    fn turn_route_lines_show_decision_evidence_and_a_failing_router() {
        let mut app = test_app();
        app.auto_model = true;
        app.last_effective_provider = Some(codewhale_config::ProviderKind::Deepseek);
        app.last_effective_model = Some("deepseek-v4-pro".to_string());
        let decision = codewhale_models::auto_route_evidence::AutoRouteDecisionEvidence {
            choice: "strong".to_string(),
            probabilities_bp: [("fast".to_string(), 1800), ("strong".to_string(), 8200)]
                .into_iter()
                .collect(),
            confidence_bp: 6400,
            min_confidence_bp: 5000,
            cost_saving_kept_fast: false,
            thinking: Some("max".to_string()),
            provider_reported_cost_usd: Some("0.000019992".to_string()),
            latency_ms: 180,
            response_model: Some("typesafe/jev-1.13-20260917".to_string()),
        };
        let receipt = codewhale_engine::model_routing::AutoRouteReceipt {
            tier: codewhale_engine::model_routing::AutoRouteTier::Strong,
            pair: codewhale_engine::model_routing::AutoRoutePair {
                strong: "deepseek-v4-pro".to_string(),
                fast: Some("deepseek-v4-flash".to_string()),
            },
            scope: codewhale_engine::model_routing::AutoRouteScope::ActiveProvider,
            data_path: codewhale_engine::model_routing::AutoRouteDataPath::Decision {
                route: codewhale_engine::client::system_one::DecisionRouterRoute::Openrouter,
                model: "typesafe/jev-1.13".to_string(),
            },
            reason: codewhale_engine::model_routing::AutoRouteReason::ClassifierRecommendation,
            decision: Some(decision),
            router_failure: None,
        };
        app.last_auto_route_receipt = Some(receipt.clone());
        let joined = turn_route_lines(&app).join("\n");
        assert!(joined.contains("-> OpenRouter / typesafe/jev-1.13 (decision model)"));
        assert!(joined.contains(
            "Auto router: decision model · typesafe/jev-1.13-20260917 · 180 ms · $0.000019992 (provider-reported)"
        ));
        assert!(joined.contains(
            "Auto choice: strong (strong 82% · fast 18%) · confidence 64% (min 50%) · thinking max"
        ));
        assert!(!joined.contains("failing"));

        app.last_auto_route_receipt = Some(codewhale_engine::model_routing::AutoRouteReceipt {
            reason: codewhale_engine::model_routing::AutoRouteReason::ClassifierFallback(
                codewhale_engine::model_routing::AutoRouteHeuristicReason::DeclaredDefault,
            ),
            decision: None,
            router_failure: Some(codewhale_engine::model_routing::AutoRouterFailure::Http {
                status: 402,
            }),
            ..receipt
        });
        let joined = turn_route_lines(&app).join("\n");
        assert!(
            joined.contains("Auto router: failing — HTTP 402 (used the local fallback)"),
            "{joined}"
        );
    }

    #[test]
    fn turn_route_lines_include_truthful_auto_receipt() {
        let mut app = test_app();
        app.auto_model = true;
        app.last_effective_provider = Some(codewhale_config::ProviderKind::Zai);
        app.last_effective_model =
            Some(codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_TURBO_MODEL.to_string());
        app.last_auto_route_receipt = Some(codewhale_engine::model_routing::AutoRouteReceipt {
            tier: codewhale_engine::model_routing::AutoRouteTier::Fast,
            pair: codewhale_engine::model_routing::AutoRoutePair {
                strong: codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL
                    .to_string(),
                fast: Some(
                    codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_TURBO_MODEL
                        .to_string(),
                ),
            },
            scope: codewhale_engine::model_routing::AutoRouteScope::RunnableProviders,
            data_path: codewhale_engine::model_routing::AutoRouteDataPath::Classifier {
                provider: "deepseek".into(),
                provider_kind: codewhale_config::ProviderKind::Deepseek,
                model: "deepseek-v4-flash".to_string(),
            },
            reason: codewhale_engine::model_routing::AutoRouteReason::ClassifierRecommendation,
            decision: None,
            router_failure: None,
        });

        let joined = turn_route_lines(&app).join("\n");

        assert!(joined.contains("Route: Zhipu AI / Z.ai · GLM-5-Turbo"));
        assert!(joined.contains("Auto decision: fast · classifier recommendation"));
        assert!(joined.contains("GLM-5.2 strong · GLM-5-Turbo fast"));
        assert!(joined.contains("Auto scope: runnable providers"));
        assert!(joined.contains(
            "Auto data: latest request + bounded recent context -> DeepSeek / deepseek-v4-flash"
        ));
        assert!(!joined.contains("API_KEY"));
    }

    #[test]
    fn reasoning_detail_text_empty_when_no_thinking() {
        let app = test_app();
        assert!(reasoning_detail_text(&app).is_none());
    }

    #[test]
    fn reasoning_detail_text_includes_active_cell_reasoning() {
        let mut app = test_app();
        let mut active = crate::tui::active_cell::ActiveCell::new();
        active.push_thinking(HistoryCell::Thinking {
            content: "active reasoning one".to_string(),
            streaming: true,
            duration_secs: None,
        });
        active.push_thinking(HistoryCell::Thinking {
            content: "active reasoning two".to_string(),
            streaming: false,
            duration_secs: Some(1.0),
        });
        app.active_cell = Some(active);
        app.runtime_turn_id = Some("turn-active-123".to_string());
        app.runtime_turn_status = Some("in_progress".to_string());

        let body = reasoning_detail_text(&app).expect("active reasoning should produce detail");
        assert!(body.contains("Thinking chunk 1 of 2"), "{body}");
        assert!(body.contains("Thinking chunk 2 of 2"), "{body}");
        assert!(body.contains("active reasoning one"), "{body}");
        assert!(body.contains("active reasoning two"), "{body}");
        assert!(body.contains("running"), "{body}");
    }

    #[test]
    fn reasoning_detail_text_scopes_to_latest_turn_without_selection() {
        let mut app = test_app();
        app.history = vec![
            HistoryCell::User {
                content: "first prompt".to_string(),
            },
            HistoryCell::Thinking {
                content: "first turn reasoning".to_string(),
                streaming: false,
                duration_secs: Some(1.0),
            },
            HistoryCell::Assistant {
                content: "first reply".to_string(),
                streaming: false,
            },
            HistoryCell::User {
                content: "second prompt".to_string(),
            },
            HistoryCell::Thinking {
                content: "second turn reasoning".to_string(),
                streaming: false,
                duration_secs: Some(1.0),
            },
            HistoryCell::Assistant {
                content: "second reply".to_string(),
                streaming: false,
            },
        ];
        app.resync_history_revisions();

        let body =
            reasoning_detail_text(&app).expect("latest turn reasoning should produce detail");
        assert!(body.contains("second turn reasoning"), "{body}");
        assert!(
            !body.contains("first turn reasoning"),
            "reasoning detail without selection must scope to the latest turn: {body}"
        );
    }

    #[test]
    fn turn_range_for_index_scopes_to_containing_turn() {
        let mut app = test_app();
        app.history = vec![
            HistoryCell::User {
                content: "first prompt".to_string(),
            },
            HistoryCell::Thinking {
                content: "first turn reasoning".to_string(),
                streaming: false,
                duration_secs: Some(1.0),
            },
            HistoryCell::Assistant {
                content: "first reply".to_string(),
                streaming: false,
            },
            HistoryCell::User {
                content: "second prompt".to_string(),
            },
            HistoryCell::Thinking {
                content: "second turn reasoning".to_string(),
                streaming: false,
                duration_secs: Some(1.0),
            },
            HistoryCell::Assistant {
                content: "second reply".to_string(),
                streaming: false,
            },
        ];
        app.resync_history_revisions();

        let (start, end) = turn_range_for_index(&app, 1);
        assert_eq!(start, 0, "first turn should start at user cell 0");
        assert_eq!(end, 3, "first turn should end before second user cell");

        let (start, end) = turn_range_for_index(&app, 4);
        assert_eq!(start, 3, "second turn should start at user cell 3");
        assert_eq!(end, 6, "second turn should run to end of transcript");
    }

    #[test]
    fn open_reasoning_detail_pager_pushes_reasoning_detail_pager() {
        let mut app = test_app();
        app.history = vec![HistoryCell::Thinking {
            content: "recorded reasoning".to_string(),
            streaming: false,
            duration_secs: Some(1.0),
        }];
        app.resync_history_revisions();
        let revisions = app.history_revisions.clone();
        app.viewport.transcript_cache.ensure(
            &app.history,
            &revisions,
            100,
            app.transcript_render_options(),
        );
        app.viewport.last_transcript_area = Some(ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });

        assert!(open_reasoning_detail_pager(&mut app));
        let top = app.view_stack.top_kind();
        assert_eq!(top, Some(crate::tui::views::ModalKind::Pager));
    }

    #[test]
    fn copy_cell_to_clipboard_uses_canonical_assistant_source() {
        let mut app = test_app();
        let content = "A long response with literal ● and ▏ glyphs that wraps visually.";
        app.history = vec![HistoryCell::Assistant {
            content: content.to_string(),
            streaming: false,
        }];
        app.resync_history_revisions();
        app.viewport.last_transcript_area = Some(ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 12,
            height: 24,
        });

        assert!(copy_cell_to_clipboard(&mut app, 0));
        assert_eq!(app.clipboard.last_written_text(), Some(content));
    }

    #[test]
    fn focused_pager_and_copy_use_the_same_cell_target() {
        let mut app = test_app();
        app.history = vec![HistoryCell::Assistant {
            content: "focused markdown **answer**".to_string(),
            streaming: false,
        }];
        app.resync_history_revisions();
        app.viewport.last_transcript_area = Some(ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });

        assert!(open_focused_cell_pager(&mut app));
        assert_eq!(
            app.view_stack.top_kind(),
            Some(crate::tui::views::ModalKind::Pager)
        );
        app.view_stack.pop();
        assert!(copy_focused_cell(&mut app));
        assert_eq!(
            app.clipboard.last_written_text(),
            Some("focused markdown **answer**")
        );
    }

    #[test]
    fn turn_inspector_copy_answer_copies_only_the_latest_completed_answer() {
        use crate::tui::history::GenericToolCell;
        use crate::tui::views::{ModalView, ViewAction, ViewEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = test_app();
        app.history = vec![
            HistoryCell::User {
                content: "please summarize".to_string(),
            },
            HistoryCell::Thinking {
                content: "private reasoning trace".to_string(),
                streaming: false,
                duration_secs: Some(1.0),
            },
            HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
                name: "read_file".to_string(),
                status: ToolStatus::Success,
                input_summary: Some("src/lib.rs".to_string()),
                output: Some("raw tool result body".to_string()),
                prompts: None,
                spillover_path: None,
                output_summary: None,
                is_diff: false,
            })),
            HistoryCell::System {
                content: "runtime status note".to_string(),
            },
            HistoryCell::Assistant {
                content: "still streaming partial".to_string(),
                streaming: true,
            },
            HistoryCell::Assistant {
                content: "FINAL ANSWER\nauthored markdown".to_string(),
                streaming: false,
            },
        ];

        assert!(open_turn_inspector_pager(&mut app));
        let mut view = app.view_stack.pop().expect("turn inspector pager");
        let pager = view
            .as_any_mut()
            .downcast_mut::<PagerView>()
            .expect("turn inspector should reuse PagerView");
        let copied = match pager.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)) {
            ViewAction::Emit(ViewEvent::CopyToClipboard { text, label }) => {
                assert_eq!(label, "Answer");
                text
            }
            other => panic!("expected answer copy event, got {other:?}"),
        };

        assert_eq!(copied, "FINAL ANSWER\nauthored markdown");
        for excluded in [
            "please summarize",
            "private reasoning trace",
            "raw tool result body",
            "runtime status note",
            "still streaming partial",
        ] {
            assert!(
                !copied.contains(excluded),
                "answer copy leaked {excluded:?}"
            );
        }
    }
    struct PrivateArtifactRoot {
        sessions: Option<PathBuf>,
        spillover: Option<PathBuf>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }
    impl PrivateArtifactRoot {
        fn set(path: PathBuf) -> Self {
            let guard = codewhale_engine::tools::truncate::TEST_SPILLOVER_GUARD
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let spillover = codewhale_engine::tools::truncate::set_test_spillover_root(Some(
                path.join("legacy"),
            ));
            Self {
                sessions: codewhale_runtime::artifacts::set_test_artifact_sessions_root(Some(path)),
                spillover,
                _guard: guard,
            }
        }
    }
    impl Drop for PrivateArtifactRoot {
        fn drop(&mut self) {
            codewhale_runtime::artifacts::set_test_artifact_sessions_root(self.sessions.take());
            codewhale_engine::tools::truncate::set_test_spillover_root(self.spillover.take());
        }
    }
    fn owned_artifact_fixture(reference: &str) -> Option<String> {
        use crate::tui::views::{ModalView, ViewAction, ViewEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mut pager = PagerView::from_text("fixture", "", 80).with_owned_evidence(
                    PathBuf::from("."),
                    "session-a".to_string(),
                    reference.to_string(),
                );
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    while !matches!(pager.tick(), ViewAction::Redraw) {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    }
                })
                .await
                .expect("owned evidence pager did not finish");
                match pager.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE)) {
                    ViewAction::Emit(ViewEvent::CopyToClipboard { text, .. }) => Some(text),
                    _ => None,
                }
            })
    }

    #[test]
    fn owned_artifact_pager_reads_regular_exact_session_output() {
        let _guard = codewhale_runtime::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let _root = PrivateArtifactRoot::set(temp.path().join("sessions"));
        codewhale_runtime::artifacts::write_session_artifact(
            "session-a",
            "mutable",
            "exact pager output",
        )
        .unwrap();
        assert_eq!(
            owned_artifact_fixture("artifacts/mutable.txt").as_deref(),
            Some("exact pager output")
        );
        assert!(owned_artifact_fixture("other/mutable.txt").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn owned_artifact_pager_refuses_linked_session_parent() {
        let _guard = codewhale_runtime::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let _root = PrivateArtifactRoot::set(sessions.clone());
        std::fs::create_dir(&sessions).unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(outside.join("artifacts")).unwrap();
        std::fs::write(
            outside.join("artifacts/mutable.txt"),
            b"outside-pager-synthetic",
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, sessions.join("session-a")).unwrap();
        assert!(
            owned_artifact_fixture("artifacts/mutable.txt").is_none(),
            "pager accepted output from a linked outside session"
        );
    }

    #[cfg(unix)]
    #[test]
    fn owned_artifact_pager_refuses_hardlinked_outside_bytes() {
        let _guard = codewhale_runtime::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let _root = PrivateArtifactRoot::set(sessions.clone());
        std::fs::create_dir_all(sessions.join("session-a/artifacts")).unwrap();
        let outside = temp.path().join("outside.txt");
        std::fs::write(&outside, b"outside-hardlink-synthetic").unwrap();
        std::fs::hard_link(&outside, sessions.join("session-a/artifacts/mutable.txt")).unwrap();
        assert!(
            owned_artifact_fixture("artifacts/mutable.txt").is_none(),
            "pager exposed outside hardlinked bytes"
        );
    }

    #[test]
    fn owned_artifact_pager_refuses_oversized_output_without_deleting_it() {
        let _guard = codewhale_runtime::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let _root = PrivateArtifactRoot::set(sessions.clone());
        let directory = sessions.join("session-a/artifacts");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("mutable.txt");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(512 * 1024 * 1024 + 1).unwrap();
        assert!(
            owned_artifact_fixture("artifacts/mutable.txt").is_none(),
            "pager allocated oversized session output"
        );
        assert_eq!(
            std::fs::metadata(path).unwrap().len(),
            512 * 1024 * 1024 + 1
        );
    }
}
