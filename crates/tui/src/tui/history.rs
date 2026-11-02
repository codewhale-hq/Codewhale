//! TUI rendering helpers for chat history and tool output.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::tui::app::TranscriptSpacing;
use crate::tui::motion::MotionMode;
use codewhale_engine::runtime_handoff::is_turn_metadata_block;
use codewhale_engine::tools::plan::PlanSnapshot;
use codewhale_engine::tools::review::ReviewOutput;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_models::{ContentBlock, Message};
use codewhale_palette as palette;
use codewhale_tui_kit::diff_render;
use codewhale_tui_kit::ui_text::CopyLineSeparator;

mod agent_activity;
mod archived_context;
mod automation;
mod checklist;
mod constants;
mod file_mutation;
mod latex_render;
mod message;
mod plan;
pub(crate) mod text;
mod thinking;
mod tool_output;
mod tool_run;

use archived_context::{parse_archived_context, render_archived_context};
pub use automation::{AutomationCell, AutomationCellKind};
pub(crate) use checklist::is_checklist_tool_name;
use checklist::{
    parse_checklist_snapshot, parse_update_prefix, render_checklist_card,
    render_checklist_change_card,
};

#[cfg(test)]
use checklist::{ChecklistChange, ChecklistItemSnapshot, ChecklistSnapshot};
use constants::{
    ASSISTANT_GLYPH, CALM_HEADLINE_MIN_WIDTH, TOOL_COMMAND_LINE_LIMIT, TOOL_DONE_SYMBOL,
    TOOL_FAILED_SYMBOL, TOOL_FAILURE_PREVIEW_LINES, TOOL_HEADER_SUMMARY_LIMIT,
    TOOL_OUTPUT_LINE_LIMIT, TOOL_SUCCESS_OUTPUT_PREVIEW_LINES, TOOL_SUMMARY_CARD_LINES,
    TRANSCRIPT_RAIL, USER_GLYPH,
};
#[cfg(test)]
use constants::{TOOL_RUNNING_SYMBOLS, TOOL_STATUS_SYMBOL_MS};
use message::{
    RenderedTranscriptLine, assistant_label_style_for, escape_note_backslashes,
    hard_break_copy_lines, message_body_style, render_message,
    render_message_with_copy_metadata_for_palette, render_plain_message, render_user_message,
    system_body_style, system_label_style, update_streaming_message_render, user_body_style,
    user_label_style,
};
#[cfg(test)]
pub(super) use thinking::render_thinking_with_analysis;
use thinking::{render_hidden_thinking_activity, render_thinking};
use tool_output::{
    for_each_wrapped_text, render_exec_output_mode, render_tool_output_mode, wrap_plain_line,
    wrap_text,
};

#[cfg(test)]
use agent_activity::extract_agent_id;
pub use file_mutation::FileMutationReceipt;
pub use plan::PlanUpdateCell;
#[cfg(test)]
use thinking::extract_reasoning_summary;
#[cfg(test)]
use tool_run::ToolRunActivitySummary;
#[cfg(test)]
pub use tool_run::detect_tool_runs;
pub use tool_run::{ToolRun, detect_tool_runs_from_slices, tool_run_summary};

#[cfg(test)]
use thinking::{REASONING_CURSOR, REASONING_OPENER, REASONING_RAIL};
#[cfg(test)]
pub use tool_output::OutputRow;
pub(crate) use tool_output::output_looks_like_diff;
pub use tool_output::{
    OutputProjection, summarize_mcp_output, summarize_tool_args, summarize_tool_output,
};

/// Render mode controlling compact live, bounded transcript, or explicit source output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// Live in-stream view: thinking is collapsed to a summary, tool output is
    /// truncated with a visible details-pager affordance.
    Live,
    /// Expanded transcript view with bounded tool-output display.
    Transcript,
    Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CellFoldAction {
    Expand,
    Collapse,
}

const FOLDED_CELL_PREVIEW_LINES: usize = 3;

/// A user's explicit decision about one transcript cell.
///
/// The absence of a `TranscriptFold` — `None` at a call site, no entry in
/// `App::cell_folds` — means the user has not touched that cell, so the
/// thinking display preferences decide a thinking cell's default; other
/// cells start expanded. An explicit intent is *absolute*: expanded or collapsed
/// outright, never "the opposite of whatever the preference currently says".
/// That is what lets a choice outlive a later preference change (#5847).
///
/// Known limitation: the intent is per session and per virtual cell index.
/// It is not persisted across restarts, and destructive transcript edits drop
/// it along with the other per-index state (`prune_transcript_index_state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFold {
    /// The user expanded this cell; show the whole body whatever the
    /// preference says.
    Expanded,
    /// The user collapsed this cell; show the preview whatever the
    /// preference says.
    Collapsed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TranscriptActionOwner {
    pub cell_index: usize,
    /// Rejects same-index replacements after destructive transcript changes.
    pub identity_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellFoldActionTarget {
    pub owner: TranscriptActionOwner,
    pub action: CellFoldAction,
}

// === History Cells ===

/// Renderable history cell for user/assistant/system entries.
#[derive(Debug, Clone)]
pub enum HistoryCell {
    User {
        content: String,
    },
    Assistant {
        content: String,
        streaming: bool,
    },
    System {
        content: String,
    },
    /// Categorized engine-error cell. Severity drives the label glyph + color
    /// (red for `Error`/`Critical`, amber for `Warning`, dim for `Info`) so
    /// the user can prioritize at a glance.
    Error {
        message: String,
        severity: codewhale_error_taxonomy::ErrorSeverity,
    },
    Thinking {
        content: String,
        streaming: bool,
        duration_secs: Option<f32>,
    },
    /// An `<archived_context>` seam block produced by the Flash seam manager
    /// (issue #159). Rendered dimmed/italic with a level + range label so
    /// the user can see at a glance where context seams exist.
    ArchivedContext {
        /// Seam level (1, 2, 3, or 0 for cycle-level).
        level: u8,
        /// Message range covered (e.g. "msg 0-128").
        range: String,
        /// Token estimate string (e.g. "~2500").
        tokens: String,
        /// Density label (e.g. "~2,500 tokens").
        density: String,
        /// Model that produced the summary.
        model: String,
        /// RFC 3339 timestamp.
        timestamp: String,
        /// The summary text content.
        summary: String,
    },
    Tool(ToolCell),
    /// Typed receipt for durable scheduled automations (fired / started /
    /// completed / failed / coalesced / missed / expired / mutated) — the
    /// one-line bulleted card that replaced bare-String `System` receipts
    /// (AUTOMATION-VISIBILITY-SPEC §2.2).
    Automation(AutomationCell),
    /// Live in-transcript card for sub-agent activity (issue #128). Owns
    /// either a single `DelegateCard` or a multi-worker `FanoutCard`; the
    /// UI re-binds it from the mailbox stream as envelopes arrive.
    SubAgent(SubAgentCell),
}

/// In-transcript sub-agent cell — either a single delegate or a fanout.
/// State mutates over the turn as mailbox envelopes are drained.
/// `Shelf` is a synthetic collapsed projector for concurrent live agents.
#[derive(Debug, Clone)]
pub enum SubAgentCell {
    Delegate(crate::tui::widgets::agent_card::DelegateCard),
    Fanout(crate::tui::widgets::agent_card::FanoutCard),
}

impl SubAgentCell {
    pub fn lines(&self, width: u16) -> Vec<Line<'static>> {
        match self {
            SubAgentCell::Delegate(card) => card.render_lines(width, &codewhale_palette::UI_THEME),
            SubAgentCell::Fanout(card) => card.render_lines(width, &codewhale_palette::UI_THEME),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptRenderOptions {
    pub locale: Locale,
    pub show_thinking: bool,
    pub thinking_default_expanded: bool,
    /// Collapsed completed-thought preview rows (settings.toml).
    pub thinking_preview_lines: usize,
    pub thinking_highlight: bool,
    pub verbose: bool,
    pub show_tool_details: bool,
    pub inline_diff_mode: codewhale_runtime::settings::InlineDiffMode,
    pub calm_mode: bool,
    pub low_motion: bool,
    pub motion_mode: MotionMode,
    pub spacing: TranscriptSpacing,
    /// Resolved application theme mode. This keeps cached markdown syntax
    /// colors aligned with an explicit theme selection.
    pub palette_mode: palette::PaletteMode,
    /// Wrap cap (columns) for prose cells — user messages, assistant answers,
    /// and reasoning/thinking blocks. `None` spends the full content width,
    /// matching tool/status cells (#5436); `Some(n)` caps prose at `n` columns
    /// for a bounded reading measure. Resolved once per render pass from
    /// `[transcript] prose_measure` so the main cache and the full-screen
    /// overlay agree on the same effective width.
    pub(crate) prose_measure: Option<u16>,
    /// This cell is a durable Work receipt that a later one has replaced.
    ///
    /// `todo_write` writes the whole list every time, so a long session grew a
    /// stack of full checklist cards that could only be cleared by `/clear` or
    /// `/new` — both of which also drop `api_messages` and the compaction
    /// summary, so tidying the view cost the conversation (#5871). A
    /// superseded snapshot collapses to its summary line in the live
    /// transcript; the full card stays in the transcript overlay and in the
    /// tool detail record, because the receipt that the tool ran is evidence.
    pub(crate) superseded_work_receipt: bool,
    /// This cell is the newest user turn in the transcript. Only it carries
    /// the elevated-surface background; every older prompt renders on the
    /// bare ground, so the eye lands on the turn in play.
    pub(crate) newest_user_turn: bool,
}

impl Default for TranscriptRenderOptions {
    fn default() -> Self {
        Self {
            superseded_work_receipt: false,
            newest_user_turn: false,
            locale: Locale::En,
            show_thinking: true,
            thinking_highlight: true,
            thinking_default_expanded: false,
            thinking_preview_lines: 2,
            verbose: false,
            show_tool_details: true,
            inline_diff_mode: codewhale_runtime::settings::InlineDiffMode::Full,
            calm_mode: false,
            low_motion: false,
            motion_mode: MotionMode::Full,
            spacing: TranscriptSpacing::Spacious,
            palette_mode: palette::PaletteMode::detect(),
            prose_measure: None,
        }
    }
}

impl TranscriptRenderOptions {
    /// Effective wrap width for a prose cell at the given content width.
    ///
    /// `prose_measure` caps the measure; without a cap the prose cell uses
    /// the full content width, like tool/status cells. Applied only at the
    /// live-transcript render entry points so the main cache and the
    /// full-screen overlay agree on the same effective width.
    #[must_use]
    pub(crate) fn prose_width(self, width: u16) -> u16 {
        match self.prose_measure {
            Some(cap) => width.clamp(1, cap),
            None => width.max(1),
        }
    }

    #[must_use]
    pub(crate) fn calm_cards(self) -> bool {
        !self.show_tool_details || self.calm_mode
    }
}

impl HistoryCell {
    #[must_use]
    pub(crate) fn has_live_motion(&self) -> bool {
        match self {
            HistoryCell::Assistant { streaming, .. } => *streaming,
            HistoryCell::Tool(ToolCell::Generic(tool))
                if tool.name == "agent" && !agent_activity::is_agent_inspection(tool) =>
            {
                false
            }
            HistoryCell::Tool(ToolCell::Exec(exec)) if exec.waiting.is_some() => false,
            HistoryCell::Tool(tool) => tool.is_running(),
            HistoryCell::User { .. }
            | HistoryCell::System { .. }
            | HistoryCell::Error { .. }
            | HistoryCell::Thinking { .. }
            | HistoryCell::ArchivedContext { .. }
            | HistoryCell::Automation(_)
            | HistoryCell::SubAgent(_) => false,
        }
    }

    /// Whether this cell is a completed (non-streaming) assistant answer —
    /// the only variant a "copy the answer" affordance may serialize.
    ///
    /// This is the typed "final answer only" projection for copy surfaces:
    /// everything else — user prompts, reasoning/thinking blocks, tool calls
    /// and results, sub-agent transcripts, runtime status/system notes, and
    /// archived-context seams — is excluded by construction, and a
    /// still-streaming partial answer never qualifies. Pair it with
    /// `history_cell_to_clipboard_text` for the canonical clean payload.
    #[must_use]
    pub fn is_completed_assistant_answer(&self) -> bool {
        matches!(
            self,
            HistoryCell::Assistant {
                streaming: false,
                ..
            }
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update_incremental_streaming_render(
        &self,
        width: u16,
        options: TranscriptRenderOptions,
        verified_append: bool,
        cache: &mut codewhale_tui_kit::markdown_render::IncrementalMarkdownRenderCache,
        lines: &mut Vec<Line<'static>>,
        links: &mut Vec<Vec<codewhale_tui_kit::osc8::LineLink>>,
        copy_separators: &mut Vec<CopyLineSeparator>,
        copy_prefix_widths: &mut Vec<usize>,
    ) -> Option<usize> {
        let HistoryCell::Assistant {
            content,
            streaming: true,
        } = self
        else {
            return None;
        };
        if content.trim().is_empty() {
            lines.clear();
            links.clear();
            copy_separators.clear();
            copy_prefix_widths.clear();
            *cache = codewhale_tui_kit::markdown_render::IncrementalMarkdownRenderCache::default();
            return Some(0);
        }
        let width = options.prose_width(width);
        Some(update_streaming_message_render(
            cache,
            content,
            width,
            assistant_label_style_for(true, options.low_motion),
            message_body_style(),
            options.palette_mode,
            verified_append,
            lines,
            links,
            copy_separators,
            copy_prefix_widths,
        ))
    }

    /// Render the cell into a set of terminal lines.
    ///
    /// This is the live-display path used by widgets that don't already pass
    /// `TranscriptRenderOptions`. Tool output is capped, but thinking is shown
    /// in full because callers using bare `lines()` historically expected the
    /// uncollapsed body. For the in-stream transcript view prefer
    /// `lines_with_options`; expanded display uses `transcript_lines` and
    /// explicit copying uses the source projection.
    pub fn lines(&self, width: u16) -> Vec<Line<'static>> {
        match self {
            HistoryCell::User { content } => render_user_message(content, width, false),
            HistoryCell::Assistant { content, streaming } => render_message(
                ASSISTANT_GLYPH,
                assistant_label_style_for(*streaming, /*low_motion*/ false),
                message_body_style(),
                content,
                width,
            ),
            HistoryCell::System { content } => render_message(
                "Note",
                system_label_style(),
                system_body_style(),
                &escape_note_backslashes(content),
                width,
            ),
            HistoryCell::Error { message, severity } => {
                render_error_message(message, *severity, width, true)
            }
            HistoryCell::Thinking {
                content,
                streaming,
                duration_secs,
            } => render_thinking(content, width, *streaming, *duration_secs, false, false),
            HistoryCell::Tool(cell) => cell.lines_with_motion(width, false),
            HistoryCell::SubAgent(cell) => cell.lines(width),
            HistoryCell::Automation(cell) => cell.render(width),
            HistoryCell::ArchivedContext { .. } => render_archived_context(
                self,
                width,
                codewhale_tui_kit::markdown_render::detected_palette_mode(),
            ),
        }
    }

    #[cfg(test)]
    pub fn lines_with_options(
        &self,
        width: u16,
        options: TranscriptRenderOptions,
    ) -> Vec<Line<'static>> {
        self.lines_with_options_folded(width, options, None).0
    }

    /// Render with the user's explicit per-cell fold intent.
    ///
    /// Untouched ordinary cells are expanded. Thinking follows the session's
    /// display preferences. An explicit choice outranks that baseline in both
    /// directions, so changing a preference never rewrites it (#5847).
    pub fn lines_with_options_folded(
        &self,
        width: u16,
        options: TranscriptRenderOptions,
        fold: Option<TranscriptFold>,
    ) -> (Vec<Line<'static>>, Option<CellFoldAction>) {
        if fold == Some(TranscriptFold::Collapsed) && !matches!(self, HistoryCell::Thinking { .. })
        {
            let (lines, action) = self.lines_with_copy_metadata_folded(width, options, fold);
            return (
                lines.into_iter().map(|rendered| rendered.line).collect(),
                action,
            );
        }
        let mut fold_action = None;
        let mut lines = match self {
            HistoryCell::Thinking {
                streaming,
                duration_secs,
                ..
            } if !options.show_thinking => {
                if *streaming {
                    render_hidden_thinking_activity(width, *duration_secs, options.low_motion)
                } else {
                    Vec::new()
                }
            }
            HistoryCell::Thinking {
                content,
                streaming,
                duration_secs,
            } => {
                let collapsed = match fold {
                    Some(TranscriptFold::Expanded) => false,
                    Some(TranscriptFold::Collapsed) => true,
                    None => !(options.verbose || options.thinking_default_expanded),
                };
                let (lines, expandable) = thinking::render_thinking_with_preview_limit(
                    content,
                    width,
                    *streaming,
                    *duration_secs,
                    collapsed,
                    options.low_motion,
                    options.thinking_highlight && !options.calm_mode,
                    options.locale,
                    if options.calm_mode {
                        0
                    } else {
                        options.thinking_preview_lines
                    },
                    options.palette_mode,
                );
                fold_action = expandable.then_some(if collapsed {
                    CellFoldAction::Expand
                } else {
                    CellFoldAction::Collapse
                });
                lines
            }
            HistoryCell::Tool(ToolCell::PatchSummary(cell)) => cell.render(
                width,
                options.low_motion,
                RenderMode::Live,
                options.inline_diff_mode,
                options.locale,
            ),
            HistoryCell::Tool(cell)
                if options.calm_cards()
                    && (!cell.is_failed() || matches!(cell, ToolCell::Exec(_))) =>
            {
                let mut lines = match cell {
                    ToolCell::Exec(exec) => {
                        exec.render_calm(width, options.low_motion, options.locale)
                    }
                    _ => {
                        cell.lines_with_motion_and_locale(width, options.low_motion, options.locale)
                    }
                };
                if !cell.is_failed() && lines.len() > TOOL_SUMMARY_CARD_LINES {
                    lines.truncate(TOOL_SUMMARY_CARD_LINES.saturating_sub(1));
                    lines.push(details_affordance_line(
                        &crate::tui::key_shortcuts::tool_details_shortcut_action_hint("details"),
                        Style::default().fg(palette::TEXT_MUTED).italic(),
                    ));
                }
                lines
            }
            HistoryCell::Tool(cell) if options.superseded_work_receipt => {
                // A durable Work receipt a later one replaced keeps its header
                // — the progress reading — and drops the body (#5871). The full
                // card stays in the transcript overlay and the detail record,
                // so the evidence that the tool ran is not rewritten away.
                let mut lines =
                    cell.lines_with_motion_and_locale(width, options.low_motion, options.locale);
                lines.truncate(1);
                lines.push(details_affordance_line(
                    &crate::tui::key_shortcuts::tool_details_shortcut_action_hint("details"),
                    Style::default().fg(palette::TEXT_MUTED).italic(),
                ));
                lines
            }
            HistoryCell::Tool(cell) => {
                cell.lines_with_motion_and_locale(width, options.low_motion, options.locale)
            }
            HistoryCell::User { content } => {
                render_user_message(content, width, options.newest_user_turn)
            }
            HistoryCell::Assistant { content, streaming } => {
                let mut lines: Vec<Line<'static>> = render_message_with_copy_metadata_for_palette(
                    ASSISTANT_GLYPH,
                    assistant_label_style_for(*streaming, options.low_motion),
                    message_body_style(),
                    content,
                    width,
                    options.palette_mode,
                )
                .into_iter()
                .map(|rendered| rendered.line)
                .collect();
                if *streaming {
                    apply_hot_tail_to_last_line(&mut lines, options.low_motion);
                }
                lines
            }
            HistoryCell::System { .. } => self.lines(width),
            HistoryCell::Error { message, severity } => {
                render_error_message(message, *severity, width, true)
            }
            HistoryCell::SubAgent(cell) => cell.lines(width),
            HistoryCell::Automation(cell) => cell.render(width),
            HistoryCell::ArchivedContext { .. } => {
                render_archived_context(self, width, options.palette_mode)
            }
        };
        if matches!(self, HistoryCell::Tool(_)) {
            match options.motion_mode {
                MotionMode::Reduced => apply_static_tool_markers(
                    &mut lines,
                    codewhale_tui_kit::spinner::BRAILLE_SPINNER_STILL_FRAME,
                ),
                MotionMode::Still => apply_static_tool_markers(
                    &mut lines,
                    codewhale_tui_kit::spinner::LIVE_STATIC_MARKER,
                ),
                MotionMode::Full => {}
            }
        }
        (lines, fold_action)
    }

    pub(crate) fn lines_with_copy_metadata(
        &self,
        width: u16,
        options: TranscriptRenderOptions,
    ) -> Vec<RenderedTranscriptLine> {
        self.lines_with_copy_metadata_folded(width, options, None).0
    }

    pub(crate) fn lines_with_copy_metadata_folded(
        &self,
        width: u16,
        options: TranscriptRenderOptions,
        fold: Option<TranscriptFold>,
    ) -> (Vec<RenderedTranscriptLine>, Option<CellFoldAction>) {
        if matches!(self, HistoryCell::Thinking { .. }) {
            let (lines, action) =
                self.lines_with_options_folded(options.prose_width(width), options, fold);
            return (hard_break_copy_lines(lines), action);
        }
        let mut lines = match self {
            HistoryCell::User { content } => hard_break_copy_lines(render_user_message(
                content,
                options.prose_width(width),
                options.newest_user_turn,
            )),
            HistoryCell::Assistant { content, streaming } => {
                let width = options.prose_width(width);
                let mut rendered = render_message_with_copy_metadata_for_palette(
                    ASSISTANT_GLYPH,
                    assistant_label_style_for(*streaming, options.low_motion),
                    message_body_style(),
                    content,
                    width,
                    options.palette_mode,
                );
                if *streaming && let Some(last) = rendered.last_mut() {
                    apply_hot_tail_to_line(&mut last.line, options.low_motion);
                }
                rendered
            }
            HistoryCell::System { content } => render_message_with_copy_metadata_for_palette(
                "Note",
                system_label_style(),
                system_body_style(),
                &escape_note_backslashes(content),
                width,
                options.palette_mode,
            ),
            HistoryCell::Tool(_) => self
                .lines_with_options_folded(width, options, None)
                .0
                .into_iter()
                .enumerate()
                .map(|(index, line)| {
                    let copy_prefix_width = tool_copy_prefix_width(&line, index == 0);
                    RenderedTranscriptLine {
                        line,
                        links: Vec::new(),
                        copy_prefix_width,
                        copy_separator_after: CopyLineSeparator::Newline,
                    }
                })
                .collect(),
            HistoryCell::Thinking { .. } => unreachable!("reasoning handled above"),
            _ => hard_break_copy_lines(self.lines_with_options_folded(width, options, None).0),
        };
        if fold == Some(TranscriptFold::Collapsed) && !lines.is_empty() {
            // Keep the cell in the rendered projection so Space can restore
            // that same owner (#6876). This control row is separate from the
            // body, whose copy separators and links must remain intact.
            if lines.len() > FOLDED_CELL_PREVIEW_LINES {
                lines.truncate(FOLDED_CELL_PREVIEW_LINES);
                lines
                    .last_mut()
                    .expect("retained preview body")
                    .copy_separator_after = CopyLineSeparator::Newline;
            }
            let style = Style::default().fg(palette::TEXT_MUTED);
            let header = Line::from(vec![
                Span::styled("…", style),
                Span::styled(if width >= 3 { " ›" } else { "" }, style),
            ]);
            let header_width = header.width();
            lines.insert(
                0,
                RenderedTranscriptLine {
                    line: header,
                    links: Vec::new(),
                    copy_prefix_width: header_width,
                    copy_separator_after: CopyLineSeparator::Newline,
                },
            );
            return (lines, Some(CellFoldAction::Expand));
        }
        (lines, None)
    }

    /// Render the expanded transcript, including bounded tool-output previews.
    pub fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        match self {
            HistoryCell::User { content } => render_plain_message(
                USER_GLYPH,
                user_label_style(),
                user_body_style(),
                content,
                width,
            ),
            HistoryCell::Assistant { content, streaming } => render_message(
                ASSISTANT_GLYPH,
                // Pager / clipboard surface — pin the glyph at full
                // brightness so a screenshot reads the same as a live frame.
                assistant_label_style_for(*streaming, /*low_motion*/ true),
                message_body_style(),
                content,
                width,
            ),
            HistoryCell::System { .. } => self.lines(width),
            HistoryCell::Error { message, severity } => {
                render_error_message(message, *severity, width, false)
            }
            HistoryCell::Thinking {
                content,
                streaming,
                duration_secs,
            } => render_thinking(
                content,
                width,
                *streaming,
                *duration_secs,
                /*collapsed*/ false,
                /*low_motion*/ false,
            ),
            HistoryCell::Tool(cell) => cell.transcript_lines(width),
            HistoryCell::SubAgent(cell) => cell.lines(width),
            HistoryCell::Automation(cell) => cell.render(width),
            HistoryCell::ArchivedContext { .. } => render_archived_context(
                self,
                width,
                codewhale_tui_kit::markdown_render::detected_palette_mode(),
            ),
        }
    }
}

/// Convert a message into history cells for rendering.
#[must_use]
pub fn history_cells_from_message(msg: &Message) -> Vec<HistoryCell> {
    // Model-facing Operate contract; the live receipt is the goal line.
    if codewhale_engine::runtime_handoff::is_operate_contract_message(msg) {
        return Vec::new();
    }
    if let Some(display) =
        codewhale_models::turn_classify::restored_subagent_checkpoint_display(msg)
    {
        return vec![HistoryCell::System {
            content: display.to_string(),
        }];
    }
    // MCP server guidance is third-party text the model was shown; keep it
    // auditable in the transcript rather than hiding it with other handoffs.
    if let Some(guidance) = codewhale_engine::runtime_handoff::mcp_server_instructions_display(msg)
    {
        return vec![HistoryCell::System {
            content: format!("MCP server guidance shown to the model:\n{guidance}"),
        }];
    }
    if let Some(instructions) =
        codewhale_engine::runtime_handoff::extension_prompt_contributions_display(msg)
    {
        return vec![HistoryCell::System {
            content: instructions.to_string(),
        }];
    }
    // Raw runtime handoffs have live tool/status receipts, not user cells.
    if let Some(instructions) = codewhale_engine::runtime_handoff::constitution_display(msg) {
        return vec![HistoryCell::System {
            content: instructions.to_string(),
        }];
    }
    // Keep their model-facing payload intact and filter only the display.
    if codewhale_engine::runtime_handoff::is_internal_runtime_handoff(msg) {
        return Vec::new();
    }

    let mut cells = Vec::new();
    let runtime_owned = codewhale_models::turn_classify::is_runtime_owned_user_message(msg);

    for (block_index, block) in msg.content.iter().enumerate() {
        match block {
            ContentBlock::Text { text, .. } => {
                if is_turn_metadata_block(msg, block_index, text) {
                    continue;
                }
                if text.starts_with("[tool_history_repair]") {
                    cells.push(HistoryCell::System {
                        content: text.clone(),
                    });
                    continue;
                }
                // Check if this is an `<archived_context>` block.
                if (msg.role == "assistant"
                    || msg.role == codewhale_models::INTERRUPTED_ASSISTANT_ROLE)
                    && let Some(archived) = parse_archived_context(text)
                {
                    cells.push(archived);
                    continue;
                }
                match msg.role.as_str() {
                    "user" if runtime_owned => {
                        if let Some(HistoryCell::System { content }) = cells.last_mut() {
                            content.push('\n');
                            content.push_str(text);
                        } else {
                            cells.push(HistoryCell::System {
                                content: format!("Runtime input:\n{text}"),
                            });
                        }
                    }
                    "user" => {
                        if let Some(HistoryCell::User { content }) = cells.last_mut() {
                            if !content.is_empty() {
                                content.push('\n');
                            }
                            content.push_str(text);
                        } else {
                            cells.push(HistoryCell::User {
                                content: text.clone(),
                            });
                        }
                    }
                    "assistant" | codewhale_models::INTERRUPTED_ASSISTANT_ROLE => {
                        if let Some(HistoryCell::Assistant { content, .. }) = cells.last_mut() {
                            if !content.is_empty() {
                                content.push('\n');
                            }
                            content.push_str(text);
                        } else {
                            cells.push(HistoryCell::Assistant {
                                content: if msg.role == codewhale_models::INTERRUPTED_ASSISTANT_ROLE
                                    && !text.starts_with("[interrupted]")
                                {
                                    format!("[interrupted] {text}")
                                } else {
                                    text.clone()
                                },
                                streaming: false,
                            });
                        }
                    }
                    "system" => {
                        if let Some(HistoryCell::System { content }) = cells.last_mut() {
                            if !content.is_empty() {
                                content.push('\n');
                            }
                            content.push_str(text);
                        } else {
                            cells.push(HistoryCell::System {
                                content: text.clone(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            ContentBlock::Thinking { thinking, .. } => {
                // Older sessions may contain this transport-only fallback from
                // thinking-mode tool-call replay. It was never model output,
                // so do not surface it when restoring their transcripts.
                if thinking == "(reasoning omitted)" {
                    continue;
                }
                if let Some(HistoryCell::Thinking { content, .. }) = cells.last_mut() {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str(thinking);
                } else {
                    cells.push(HistoryCell::Thinking {
                        content: thinking.clone(),
                        streaming: false,
                        duration_secs: None,
                    });
                }
            }
            ContentBlock::ToolUse { name, input, .. } if name == "update_plan" => {
                cells.push(HistoryCell::Tool(ToolCell::PlanUpdate(PlanUpdateCell {
                    snapshot: PlanSnapshot::from_tool_input(input),
                    status: ToolStatus::Success,
                })));
            }
            _ => {}
        }
    }

    cells
}

// === Tool Cells ===

/// Variants describing a tool result cell.
#[derive(Debug, Clone)]
pub enum ToolCell {
    Exec(ExecCell),
    Exploring(ExploringCell),
    PlanUpdate(PlanUpdateCell),
    PatchSummary(PatchSummaryCell),
    Review(ReviewCell),
    Mcp(McpToolCell),
    ViewImage(ViewImageCell),
    WebSearch(WebSearchCell),
    Generic(GenericToolCell),
}

impl ToolCell {
    /// Whether this tool cell projects durable Work state rather than a
    /// transient action receipt. Transcript rhythm uses this semantic split
    /// to keep plans, checklists, and workflows legible without teaching the
    /// renderer about individual tool payloads.
    #[must_use]
    pub(crate) fn is_durable_work_receipt(&self) -> bool {
        matches!(self, ToolCell::PlanUpdate(_))
            || matches!(
                self,
                ToolCell::Generic(cell)
                    if cell.name == "workflow" || is_checklist_tool_name(&cell.name)
            )
    }

    /// Status for cells that have a concrete lifecycle state.
    pub fn status(&self) -> Option<ToolStatus> {
        match self {
            ToolCell::Exec(cell) => Some(cell.status),
            ToolCell::Exploring(cell) => Some(cell.status()),
            ToolCell::PlanUpdate(cell) => Some(cell.status),
            ToolCell::PatchSummary(cell) => Some(cell.status),
            ToolCell::Review(cell) => Some(cell.status),
            ToolCell::Mcp(cell) => Some(cell.status),
            ToolCell::WebSearch(cell) => Some(cell.status),
            ToolCell::Generic(cell) => Some(cell.status),
            ToolCell::ViewImage(_) => Some(ToolStatus::Success),
        }
    }

    #[must_use]
    pub fn is_success(&self) -> bool {
        self.status() == Some(ToolStatus::Success)
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        self.status() == Some(ToolStatus::Running)
    }

    #[must_use]
    pub fn is_failed(&self) -> bool {
        self.status() == Some(ToolStatus::Failed)
    }

    /// Whether this cell should stay visible even inside a dense tool run.
    #[must_use]
    pub fn is_collapsible_guard(&self) -> bool {
        self.is_running()
            || self.is_failed()
            || matches!(
                self,
                ToolCell::Exec(_)
                    | ToolCell::Mcp(_)
                    | ToolCell::PatchSummary(_)
                    | ToolCell::Review(_)
                    | ToolCell::PlanUpdate(_)
            )
            || matches!(self, ToolCell::Generic(cell) if tool_run::generic_tool_name_is_collapse_guard(&cell.name) || cell.is_diff)
    }

    /// Render the tool cell into lines.
    pub fn lines(&self, width: u16) -> Vec<Line<'static>> {
        self.lines_with_motion(width, false)
    }

    pub fn lines_with_motion(&self, width: u16, low_motion: bool) -> Vec<Line<'static>> {
        self.lines_with_motion_and_locale(width, low_motion, Locale::En)
    }

    pub fn lines_with_motion_and_locale(
        &self,
        width: u16,
        low_motion: bool,
        locale: Locale,
    ) -> Vec<Line<'static>> {
        self.render_with_locale(width, low_motion, RenderMode::Live, locale)
    }

    /// Expanded rendering with bounded tool-output previews.
    pub fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.transcript_lines_with_locale(width, Locale::En)
    }

    pub fn transcript_lines_with_locale(&self, width: u16, locale: Locale) -> Vec<Line<'static>> {
        self.render_with_locale(
            width,
            /*low_motion*/ false,
            RenderMode::Transcript,
            locale,
        )
    }

    pub(crate) fn source_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.render_with_locale(width, false, RenderMode::Source, Locale::En)
    }

    fn render_with_locale(
        &self,
        width: u16,
        low_motion: bool,
        mode: RenderMode,
        locale: Locale,
    ) -> Vec<Line<'static>> {
        match self {
            ToolCell::Exec(cell) => cell.render_with_locale(width, low_motion, mode, locale),
            ToolCell::Exploring(cell) => cell.render_with_locale(width, low_motion, mode, locale),
            ToolCell::PlanUpdate(cell) => {
                cell.lines_with_motion_and_locale(width, low_motion, locale)
            }
            ToolCell::PatchSummary(cell) => cell.render(
                width,
                low_motion,
                mode,
                codewhale_runtime::settings::InlineDiffMode::Full,
                locale,
            ),
            ToolCell::Review(cell) => cell.render(width, low_motion, mode, locale),
            ToolCell::Mcp(cell) => cell.render(width, low_motion, mode, locale),
            ToolCell::ViewImage(cell) => {
                cell.lines_with_motion_and_locale(width, low_motion, locale)
            }
            ToolCell::WebSearch(cell) => {
                cell.lines_with_motion_and_locale(width, low_motion, locale)
            }
            ToolCell::Generic(cell) => {
                cell.lines_with_mode_and_locale(width, low_motion, mode, locale)
            }
        }
    }
}

/// Overall status for a tool execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Success,
    Hydrated,
    Stopped,
    /// Terminal result with usable output that still needs attention.
    Warning,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecWait {
    Approval,
    Queue,
}

/// Shell command execution rendering data.
#[derive(Debug, Clone)]
pub struct ExecCell {
    pub command: String,
    pub description: Option<String>,
    pub status: ToolStatus,
    pub output: Option<String>,
    pub live_output: Option<String>,
    pub shell_task_id: Option<String>,
    pub owner_agent_id: Option<String>,
    pub owner_agent_name: Option<String>,
    pub started_at: Option<Instant>,
    pub duration_ms: Option<u64>,
    pub stale_elapsed_since_output_ms: Option<u64>,
    pub waiting: Option<ExecWait>,
    pub source: ExecSource,
    pub interaction: Option<String>,
    /// Cached output summary — avoids re-parsing JSON every frame.
    pub output_summary: Option<String>,
}

impl ExecCell {
    /// Render the execution cell into lines (live view, capped output).
    #[cfg(test)]
    pub fn lines_with_motion(&self, width: u16, low_motion: bool) -> Vec<Line<'static>> {
        self.render(width, low_motion, RenderMode::Live)
    }

    /// Foreground `exec_shell` blocking the turn.
    fn is_foreground_shell_wait(&self) -> bool {
        self.status == ToolStatus::Running
            && self.source == ExecSource::Assistant
            && self.interaction.is_none()
    }

    #[cfg(test)]
    pub(super) fn render(
        &self,
        width: u16,
        low_motion: bool,
        mode: RenderMode,
    ) -> Vec<Line<'static>> {
        self.render_with_locale(width, low_motion, mode, Locale::En)
    }

    pub(super) fn render_with_locale(
        &self,
        width: u16,
        low_motion: bool,
        mode: RenderMode,
        locale: Locale,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let command_summary = command_header_summary(&self.command);
        // The header names the command, always. A long wait moves itself to
        // the background (see `execute_foreground_via_background`), so the
        // card never needs to advertise a key to rescue the turn.
        let header_summary = self
            .interaction
            .as_deref()
            .or(Some(command_summary.as_str()));
        let held_status = self.held_status(locale);
        let receipt = tool_receipt_label(
            crate::tui::widgets::tool_card::ToolFamily::Run,
            self.status,
            self.output.as_deref(),
            locale,
        );
        let status_text = held_status.as_deref().map(Cow::Borrowed).unwrap_or(receipt);
        lines.push(render_tool_header_with_summary(
            "Shell",
            header_summary,
            if self.status == ToolStatus::Success || held_status.is_some() {
                Some(status_text.as_ref())
            } else {
                None
            },
            self.status,
            self.started_at,
            low_motion || held_status.is_some(),
            locale,
        ));

        lines.extend(self.description_lines(width, locale));

        if mode != RenderMode::Source
            && self.interaction.is_none()
            && self.command.trim_end().lines().nth(1).is_some()
            && (self.is_foreground_shell_wait()
                || (mode == RenderMode::Live && self.status == ToolStatus::Success))
        {
            lines.extend(render_command_mode(
                &self.command,
                width,
                mode,
                false,
                locale,
            ));
        }

        // A foreground shell wait stays a compact card — command, optional note,
        // spinner and running badge. Live output and artifact paths belong in the
        // Activity sidebar and `/jobs` detail surfaces.
        if self.is_foreground_shell_wait() && mode != RenderMode::Source {
            return wrap_card_rail(lines, self.status);
        }

        // Routine successes get a three-row glimpse (the opening row plus
        // two closing rows) so they take less space in the live transcript.
        // Failures and expanded display keep a larger bounded preview.
        if mode == RenderMode::Live
            && self
                .output
                .as_deref()
                .is_some_and(is_truncated_output_preview)
        {
            lines.push(render_spillover_annotation(width, locale));
            return wrap_card_rail(lines, self.status);
        }
        if mode == RenderMode::Live && self.status == ToolStatus::Success {
            if self.interaction.is_none()
                && let Some(output) = self.output.as_ref().or(self.live_output.as_ref())
            {
                lines.extend(render_exec_output_mode(
                    output,
                    width,
                    TOOL_SUCCESS_OUTPUT_PREVIEW_LINES,
                    mode,
                    locale,
                    true,
                ));
            }
            if let Some(duration_ms) = self.duration_ms
                && duration_ms >= 1000
            {
                lines.extend(render_compact_kv(
                    "time",
                    &codewhale_command_contract::elapsed::format_elapsed_ms(duration_ms),
                    Style::default().fg(palette::TEXT_DIM),
                    width,
                ));
            }
            return wrap_card_rail(lines, self.status);
        }

        if self.status == ToolStatus::Success && self.source == ExecSource::User {
            lines.extend(render_compact_kv(
                "source",
                "started by you",
                Style::default().fg(palette::TEXT_MUTED),
                width,
            ));
        }

        if let Some(owner) = self
            .owner_agent_name
            .as_deref()
            .or(self.owner_agent_id.as_deref())
        {
            lines.extend(render_compact_kv(
                "owner",
                owner,
                Style::default().fg(palette::TEXT_MUTED),
                width,
            ));
        }

        if mode == RenderMode::Source {
            lines.extend(render_command_mode(
                &self.command,
                width,
                mode,
                false,
                locale,
            ));
        }
        if let Some(interaction) = self.interaction.as_ref() {
            lines.extend(wrap_plain_line(
                &format!("  {interaction}"),
                Style::default().fg(palette::TEXT_MUTED),
                width,
            ));
        } else if mode != RenderMode::Source {
            lines.extend(render_command_mode(
                &self.command,
                width,
                mode,
                false,
                locale,
            ));
        }

        if self.interaction.is_none() || mode == RenderMode::Source {
            if let Some(output) = self.output.as_ref().or(self.live_output.as_ref()) {
                lines.extend(render_exec_output_mode(
                    output,
                    width,
                    TOOL_OUTPUT_LINE_LIMIT,
                    mode,
                    locale,
                    true,
                ));
            } else if self.status != ToolStatus::Running && mode != RenderMode::Live {
                // #3031: Suppress "(no output)" in compact/Live mode;
                // the success header is enough signal.
                lines.push(Line::from(Span::styled(
                    "  (no output)",
                    Style::default().fg(palette::TEXT_MUTED).italic(),
                )));
            }
        }

        if let Some(duration_ms) = self.duration_ms {
            // #3031: Suppress sub-second timing in compact mode.
            // Transcript mode always shows timing.
            if mode != RenderMode::Live || duration_ms >= 1000 {
                lines.extend(render_compact_kv(
                    "time",
                    &codewhale_command_contract::elapsed::format_elapsed_ms(duration_ms),
                    Style::default().fg(palette::TEXT_DIM),
                    width,
                ));
            }
        }

        wrap_card_rail(lines, self.status)
    }

    pub(super) fn render_calm(
        &self,
        width: u16,
        low_motion: bool,
        locale: Locale,
    ) -> Vec<Line<'static>> {
        let description = self.description_text();
        let (mut lines, headline_clipped) =
            self.calm_header(description.as_deref(), width, low_motion, locale);
        let multi_line_command = self.command.trim_end().lines().nth(1).is_some();

        if self.interaction.is_none() {
            let show_command = match self.status {
                ToolStatus::Failed => {
                    description.is_some() || headline_clipped || multi_line_command
                }
                ToolStatus::Running | ToolStatus::Success => multi_line_command,
                ToolStatus::Hydrated | ToolStatus::Stopped | ToolStatus::Warning => false,
            };
            if show_command {
                lines.extend(render_command_mode(
                    &self.command,
                    width,
                    RenderMode::Live,
                    true,
                    locale,
                ));
            } else if description.is_some() {
                let more = if multi_line_command { " \u{2026}" } else { "" };
                lines.push(render_card_detail_line_single(
                    None,
                    &truncate_text(
                        &format!("$ {}{more}", command_header_summary(&self.command)),
                        usize::from(width).saturating_sub(2),
                    ),
                    tool_value_style(),
                ));
            }
        }

        if self.is_foreground_shell_wait() {
            return lines;
        }
        if self
            .output
            .as_deref()
            .is_some_and(is_truncated_output_preview)
        {
            lines.push(render_spillover_annotation(width, locale));
            return lines;
        }
        let output = self
            .interaction
            .is_none()
            .then(|| self.output.as_ref().or(self.live_output.as_ref()))
            .flatten();
        if self.status == ToolStatus::Success {
            if let Some(output) = output {
                lines.extend(render_exec_output_mode(
                    output,
                    width,
                    TOOL_SUCCESS_OUTPUT_PREVIEW_LINES,
                    RenderMode::Live,
                    locale,
                    false,
                ));
            }
            return lines;
        }

        if let Some(owner) = self
            .owner_agent_name
            .as_deref()
            .or(self.owner_agent_id.as_deref())
        {
            lines.extend(render_compact_kv(
                "owner",
                owner,
                Style::default().fg(palette::TEXT_MUTED),
                width,
            ));
        }
        if let Some(interaction) = self.interaction.as_ref()
            && headline_clipped
        {
            lines.extend(wrap_plain_line(
                &format!("  {interaction}"),
                Style::default().fg(palette::TEXT_MUTED),
                width,
            ));
        }
        if let Some(output) = output {
            lines.extend(render_exec_output_mode(
                output,
                width,
                TOOL_OUTPUT_LINE_LIMIT,
                RenderMode::Live,
                locale,
                false,
            ));
        }
        lines
    }

    fn calm_header(
        &self,
        description: Option<&str>,
        width: u16,
        low_motion: bool,
        locale: Locale,
    ) -> (Vec<Line<'static>>, bool) {
        let family = crate::tui::widgets::tool_card::ToolFamily::Run;
        let held = self.held_status(locale);
        let mark = format!(
            "{} ",
            status_symbol(
                self.started_at,
                self.status,
                low_motion || held.is_some(),
                family
            )
        );
        let (headline, headline_style) = if let Some(interaction) = self
            .interaction
            .as_deref()
            .and_then(normalize_header_summary)
        {
            (interaction, tool_title_style(self.status))
        } else if let Some(description) = description {
            (
                description.to_string(),
                tool_title_style(self.status).italic(),
            )
        } else {
            let more = if self.command.trim_end().lines().nth(1).is_some() {
                " \u{2026}"
            } else {
                ""
            };
            (
                format!("$ {}{more}", command_header_summary(&self.command)),
                tool_title_style(self.status),
            )
        };

        let separator = Style::default().fg(palette::TEXT_DIM);
        let state = held.or_else(|| match self.status {
            ToolStatus::Success => None,
            ToolStatus::Running => Some(self.started_at.map_or_else(
                || tool_status_label(ToolStatus::Running, locale).to_string(),
                |started| running_status_label_with_elapsed(started.elapsed().as_secs(), locale),
            )),
            status => Some(tool_status_label(status, locale).to_string()),
        });
        let mut suffix = Vec::new();
        if let Some(state) = state {
            suffix.push(Span::styled(" \u{00B7} ", separator));
            suffix.push(Span::styled(state, tool_status_style(self.status, family)));
        }
        if let Some(duration) = self.finished_duration() {
            suffix.push(Span::styled(" \u{00B7} ", separator));
            suffix.push(Span::styled(duration, separator));
        }

        let mark_width = UnicodeWidthStr::width(mark.as_str());
        let inline_budget = usize::from(width)
            .saturating_sub(mark_width + suffix.iter().map(Span::width).sum::<usize>());
        let state_row = !suffix.is_empty()
            && inline_budget
                < codewhale_tui_kit::ui_text::text_display_width(&headline)
                    .min(CALM_HEADLINE_MIN_WIDTH);
        let budget = if state_row {
            usize::from(width).saturating_sub(mark_width)
        } else {
            inline_budget
        }
        .min(TOOL_HEADER_SUMMARY_LIMIT);
        let shown = truncate_text(&headline, budget);
        let clipped = shown != headline;

        let mut spans = vec![
            Span::styled(
                mark,
                Style::default().fg(tool_glyph_color(self.status, family)),
            ),
            Span::styled(shown, headline_style),
        ];
        if state_row {
            suffix.remove(0);
            (vec![Line::from(spans), Line::from(suffix)], clipped)
        } else {
            spans.extend(suffix);
            (vec![Line::from(spans)], clipped)
        }
    }

    fn held_status(&self, locale: Locale) -> Option<String> {
        match (self.waiting, self.status) {
            (Some(ExecWait::Approval), ToolStatus::Running) => Some(
                codewhale_localization::tr(
                    locale,
                    codewhale_localization::MessageId::ToolStatusWaitingForApproval,
                )
                .into_owned(),
            ),
            (Some(ExecWait::Queue), ToolStatus::Running) => Some(
                codewhale_localization::tr(
                    locale,
                    codewhale_localization::MessageId::ToolStatusQueued,
                )
                .into_owned(),
            ),
            _ => self
                .stale_elapsed_since_output_ms
                .map(|ms| stale_shell_status_label(ms, locale)),
        }
    }

    fn description_text(&self) -> Option<String> {
        let mut plain = String::new();
        codewhale_tui_kit::osc8::strip_ansi_into(self.description.as_deref()?, &mut plain);
        normalize_header_summary(&plain)
    }

    fn description_lines(&self, width: u16, locale: Locale) -> Vec<Line<'static>> {
        let Some(description) = self.description_text() else {
            return Vec::new();
        };
        let label = codewhale_localization::tr(
            locale,
            codewhale_localization::MessageId::ToolDescriptionNote,
        );
        render_card_detail_line(
            Some(label.as_ref()),
            &truncate_text(&description, TOOL_HEADER_SUMMARY_LIMIT),
            Style::default().fg(palette::TEXT_MUTED).italic(),
            width,
        )
    }

    fn finished_duration(&self) -> Option<String> {
        if self.status == ToolStatus::Running {
            return None;
        }
        self.duration_ms
            .filter(|duration_ms| *duration_ms >= 1000)
            .map(codewhale_command_contract::elapsed::format_elapsed_ms)
    }
}

/// Source of a shell command execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecSource {
    User,
    Assistant,
}

/// Aggregate cell for tool exploration runs.
#[derive(Debug, Clone)]
pub struct ExploringCell {
    pub entries: Vec<ExploringEntry>,
}

impl ExploringCell {
    /// The cell's own lifecycle state, rolled up from its parallel entries.
    ///
    /// One derivation, used by the card header, by [`ToolCell::status`], and by
    /// the Activity surfaces — three copies of this fold had drifted apart, and
    /// the header's copy could not produce `Failed` at all, so a fan-out with a
    /// failed read painted itself `done` in the success colour.
    ///
    /// Precedence: any entry still running keeps the whole cell running; after
    /// that the loudest terminal state wins, so a single failure is never
    /// averaged away by its successful siblings.
    #[must_use]
    pub fn status(&self) -> ToolStatus {
        let any = |wanted: ToolStatus| self.entries.iter().any(|entry| entry.status == wanted);
        if any(ToolStatus::Running) {
            ToolStatus::Running
        } else if any(ToolStatus::Failed) {
            ToolStatus::Failed
        } else if any(ToolStatus::Warning) {
            ToolStatus::Warning
        } else if any(ToolStatus::Stopped) {
            ToolStatus::Stopped
        } else if any(ToolStatus::Hydrated) {
            ToolStatus::Hydrated
        } else {
            ToolStatus::Success
        }
    }

    /// Render the exploring cell into lines.
    #[cfg(test)]
    pub fn lines_with_motion(&self, width: u16, low_motion: bool) -> Vec<Line<'static>> {
        self.lines_with_motion_and_locale(width, low_motion, Locale::En)
    }

    #[cfg(test)]
    pub fn lines_with_motion_and_locale(
        &self,
        width: u16,
        low_motion: bool,
        locale: Locale,
    ) -> Vec<Line<'static>> {
        self.render_with_locale(width, low_motion, RenderMode::Live, locale)
    }

    pub(super) fn render_with_locale(
        &self,
        width: u16,
        low_motion: bool,
        mode: RenderMode,
        locale: Locale,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let status = self.status();
        let all_done = status != ToolStatus::Running;
        let header_summary = exploring_header_summary(&self.entries);
        let multi_entry = self.entries.len() > 1;
        let header_state: Cow<'static, str> = if multi_entry {
            Cow::Borrowed("")
        } else if all_done {
            if status == ToolStatus::Success {
                codewhale_localization::tr(
                    locale,
                    codewhale_localization::MessageId::ToolReceiptDone,
                )
            } else {
                tool_status_label(status, locale)
            }
        } else {
            tr(locale, MessageId::ToolReceiptRunning)
        };
        // Search-only exploration cards read with the `find` verb so a
        // completed grep renders `find done · Searching for …` instead of the
        // incoherent `read done · Searching …` (#4145). Read/list or mixed
        // cards keep the neutral `read` verb the Workspace card has always used.
        let family = exploring_card_family(&self.entries);
        let header = render_tool_header_with_family_and_summary(
            family,
            header_summary.as_deref(),
            if status == ToolStatus::Success {
                Some(header_state.as_ref())
            } else {
                None
            },
            status,
            None,
            low_motion,
            locale,
        );
        let header_shows_label = !multi_entry
            && header_summary
                .as_deref()
                .is_some_and(|label| header_shows_in_full(&header, label, usize::from(width)));
        lines.push(header);

        // Dot-grid status strip — one glyph per entry, showing parallel
        // fanout at a glance: ●=done ◐=running ✕=failed.
        if self.entries.len() > 1 {
            let (done, running, failed, stopped) = self.entries.iter().fold(
                (0usize, 0usize, 0usize, 0usize),
                |(d, r, f, s), e| match e.status {
                    ToolStatus::Success | ToolStatus::Hydrated => (d + 1, r, f, s),
                    ToolStatus::Warning | ToolStatus::Failed => (d, r, f + 1, s),
                    ToolStatus::Running => (d, r + 1, f, s),
                    ToolStatus::Stopped => (d, r, f, s + 1),
                },
            );
            // Each dot carries its own entry's state. Painting the strip one
            // flat colour made a failed parallel read indistinguishable from a
            // finished one, which is exactly the narration the cell is
            // supposed to make unnecessary.
            let mut dot_spans: Vec<Span<'static>> = vec![Span::raw("  ")];
            dot_spans.extend(self.entries.iter().map(|e| {
                let glyph = match e.status {
                    ToolStatus::Success | ToolStatus::Hydrated => "\u{25CF}",
                    ToolStatus::Warning => "!",
                    ToolStatus::Stopped => codewhale_tui_kit::glyphs::NEUTRAL,
                    ToolStatus::Running => "\u{25D0}",
                    ToolStatus::Failed => "\u{2715}",
                };
                Span::styled(
                    glyph,
                    Style::default().fg(tool_glyph_color(e.status, family)),
                )
            }));
            let counts = [
                (done, codewhale_localization::MessageId::WorkflowCountDone),
                (
                    running,
                    codewhale_localization::MessageId::WorkflowCountRunning,
                ),
                (
                    failed,
                    codewhale_localization::MessageId::WorkflowCountFailed,
                ),
                (
                    stopped,
                    codewhale_localization::MessageId::WorkflowCountCancelled,
                ),
            ]
            .into_iter()
            .filter(|(count, _)| *count > 0)
            .map(|(count, id)| {
                codewhale_localization::tr(locale, id).replace("{count}", &count.to_string())
            })
            .collect::<Vec<_>>()
            .join(", ");
            dot_spans.push(Span::styled(
                format!("  {counts}"),
                Style::default().fg(tool_glyph_color(status, family)),
            ));
            lines.push(Line::from(dot_spans));
        }

        for entry in &self.entries {
            if multi_entry {
                lines.extend(render_card_detail_line(
                    None,
                    &entry.label,
                    tool_value_style(),
                    width,
                ));
            } else if !header_shows_label {
                let prefix = match entry.status {
                    ToolStatus::Running => "live",
                    ToolStatus::Success => "done",
                    ToolStatus::Hydrated => "loaded",
                    ToolStatus::Stopped => "stopped",
                    ToolStatus::Warning => "issue",
                    ToolStatus::Failed => "failed",
                };
                lines.extend(render_compact_kv(
                    prefix,
                    &entry.label,
                    tool_value_style(),
                    width,
                ));
            }
            if let Some(error) = entry.error.as_deref() {
                if multi_entry {
                    if let Some(reason) = error.lines().map(str::trim).find(|line| !line.is_empty())
                    {
                        lines.extend(render_card_detail_line(
                            None,
                            reason,
                            tool_value_style(),
                            width,
                        ));
                    }
                } else {
                    lines.extend(render_tool_output_mode(
                        error,
                        width,
                        TOOL_FAILURE_PREVIEW_LINES,
                        mode,
                        locale,
                    ));
                }
            }
        }
        lines
    }
}

/// Single entry for exploring tool output.
#[derive(Debug, Clone)]
pub struct ExploringEntry {
    pub label: String,
    pub status: ToolStatus,
    pub error: Option<String>,
}

/// Calm outcome and exact evidence for a structured File mutation.
#[derive(Debug, Clone)]
pub struct PatchSummaryCell {
    pub path: String,
    pub summary: String,
    pub status: ToolStatus,
    pub error: Option<String>,
    pub receipt: Option<FileMutationReceipt>,
}

impl PatchSummaryCell {
    pub(super) fn render(
        &self,
        width: u16,
        low_motion: bool,
        mode: RenderMode,
        inline_diff_mode: codewhale_runtime::settings::InlineDiffMode,
        locale: Locale,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let header_summary = self
            .receipt
            .as_ref()
            .map(|receipt| {
                if inline_diff_mode == codewhale_runtime::settings::InlineDiffMode::Off {
                    receipt.outcome_label(locale)
                } else {
                    receipt.semantic_summary(locale)
                }
            })
            .unwrap_or_else(|| self.path.clone());
        lines.push(render_tool_header_with_summary(
            "File",
            Some(&header_summary),
            None,
            self.status,
            None,
            low_motion,
            locale,
        ));
        if self.status == ToolStatus::Success
            && let Some(receipt) = self.receipt.as_ref()
        {
            if mode == RenderMode::Source {
                lines.push(Line::from(receipt.inspect_text(locale)));
            } else {
                lines.extend(receipt.render_inline(width, inline_diff_mode, locale));
            }
        } else {
            lines.extend(render_compact_kv(
                "file",
                &self.path,
                tool_value_style(),
                width,
            ));
            lines.extend(render_tool_output_mode(
                &self.summary,
                width,
                TOOL_COMMAND_LINE_LIMIT,
                mode,
                locale,
            ));
        }
        if let Some(error) = self.error.as_ref() {
            lines.extend(render_tool_output_mode(
                error,
                width,
                TOOL_COMMAND_LINE_LIMIT,
                mode,
                locale,
            ));
        }
        lines
    }
}

/// Cell for structured review output.
#[derive(Debug, Clone)]
pub struct ReviewCell {
    pub target: String,
    pub status: ToolStatus,
    pub output: Option<ReviewOutput>,
    pub error: Option<String>,
}

impl ReviewCell {
    pub(super) fn render(
        &self,
        width: u16,
        low_motion: bool,
        mode: RenderMode,
        locale: Locale,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(render_tool_header(
            "Review",
            None,
            self.status,
            None,
            low_motion,
            locale,
        ));

        if !self.target.trim().is_empty() {
            lines.extend(render_compact_kv(
                "target",
                self.target.trim(),
                tool_value_style(),
                width,
            ));
        }

        if self.status == ToolStatus::Running {
            return lines;
        }

        if let Some(error) = self.error.as_ref() {
            lines.extend(render_tool_output_mode(
                error,
                width,
                TOOL_COMMAND_LINE_LIMIT,
                mode,
                locale,
            ));
            return lines;
        }

        let Some(output) = self.output.as_ref() else {
            return lines;
        };

        if !output.summary.trim().is_empty() {
            lines.extend(wrap_plain_line(
                &format!("Summary: {}", output.summary.trim()),
                Style::default().fg(palette::TEXT_PRIMARY),
                width,
            ));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Issues",
            Style::default()
                .fg(palette::WHALE_ACTION)
                .add_modifier(Modifier::BOLD),
        )));
        if output.issues.is_empty() {
            lines.extend(wrap_plain_line(
                "  (none)",
                Style::default().fg(palette::TEXT_MUTED),
                width,
            ));
        } else {
            for issue in &output.issues {
                let severity = issue.severity.trim().to_ascii_lowercase();
                let color = review_severity_color(&severity);
                let location = format_review_location(issue.path.as_ref(), issue.line);
                let label = if location.is_empty() {
                    format!("  - [{}] {}", severity, issue.title.trim())
                } else {
                    format!("  - [{}] {} ({})", severity, issue.title.trim(), location)
                };
                lines.extend(wrap_plain_line(&label, Style::default().fg(color), width));
                if !issue.description.trim().is_empty() {
                    lines.extend(wrap_plain_line(
                        &format!("    {}", issue.description.trim()),
                        Style::default().fg(palette::TEXT_MUTED),
                        width,
                    ));
                }
            }
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Suggestions",
            Style::default()
                .fg(palette::WHALE_ACTION)
                .add_modifier(Modifier::BOLD),
        )));
        if output.suggestions.is_empty() {
            lines.extend(wrap_plain_line(
                "  (none)",
                Style::default().fg(palette::TEXT_MUTED),
                width,
            ));
        } else {
            for suggestion in &output.suggestions {
                let location = format_review_location(suggestion.path.as_ref(), suggestion.line);
                let label = if location.is_empty() {
                    format!("  - {}", suggestion.suggestion.trim())
                } else {
                    format!("  - {} ({})", suggestion.suggestion.trim(), location)
                };
                lines.extend(wrap_plain_line(
                    &label,
                    Style::default().fg(palette::TEXT_PRIMARY),
                    width,
                ));
            }
        }

        if !output.overall_assessment.trim().is_empty() {
            lines.push(Line::from(""));
            lines.extend(wrap_plain_line(
                &format!("Overall: {}", output.overall_assessment.trim()),
                Style::default().fg(palette::TEXT_PRIMARY),
                width,
            ));
        }

        lines
    }
}

/// Cell representing an MCP tool execution.
#[derive(Debug, Clone)]
pub struct McpToolCell {
    pub tool: String,
    pub status: ToolStatus,
    pub content: Option<String>,
    pub is_image: bool,
}

impl McpToolCell {
    pub(super) fn render(
        &self,
        width: u16,
        low_motion: bool,
        mode: RenderMode,
        locale: Locale,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(render_tool_header_with_summary(
            "Tool",
            Some(&self.tool),
            None,
            self.status,
            None,
            low_motion,
            locale,
        ));

        if self.is_image {
            lines.extend(render_compact_kv(
                "result",
                "image",
                tool_value_style(),
                width,
            ));
        }

        if let Some(content) = self.content.as_ref() {
            if mode == RenderMode::Live && is_truncated_output_preview(content) {
                lines.push(render_spillover_annotation(width, locale));
                return lines;
            }
            lines.extend(render_tool_output_mode(
                content,
                width,
                if self.status == ToolStatus::Failed {
                    TOOL_FAILURE_PREVIEW_LINES
                } else {
                    TOOL_COMMAND_LINE_LIMIT
                },
                mode,
                locale,
            ));
        }
        lines
    }
}

/// Cell for image view actions.
#[derive(Debug, Clone)]
pub struct ViewImageCell {
    pub path: PathBuf,
}

impl ViewImageCell {
    /// Render the image view cell into lines.
    pub fn lines_with_motion_and_locale(
        &self,
        width: u16,
        low_motion: bool,
        locale: Locale,
    ) -> Vec<Line<'static>> {
        let path = self.path.display().to_string();
        let header = render_tool_header_with_summary(
            "Image",
            Some(&path),
            None,
            ToolStatus::Success,
            None,
            low_motion,
            locale,
        );
        let header_shows_path = header_shows_in_full(&header, &path, usize::from(width));
        let mut lines = vec![header];
        if !header_shows_path {
            lines.extend(render_compact_kv("path", &path, tool_value_style(), width));
        }
        lines
    }
}

/// Cell for web search tool output.
#[derive(Debug, Clone)]
pub struct WebSearchCell {
    pub query: String,
    pub status: ToolStatus,
    pub summary: Option<String>,
    pub source: Option<String>,
    pub degraded: Option<String>,
    pub ref_count: usize,
}

impl WebSearchCell {
    /// Render the web search cell into lines.
    #[cfg(test)]
    pub fn lines_with_motion(&self, width: u16, low_motion: bool) -> Vec<Line<'static>> {
        self.lines_with_motion_and_locale(width, low_motion, Locale::En)
    }

    pub fn lines_with_motion_and_locale(
        &self,
        width: u16,
        low_motion: bool,
        locale: Locale,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let header = render_tool_header_with_summary(
            "Search",
            Some(&self.query),
            None,
            self.status,
            None,
            low_motion,
            locale,
        );
        let header_shows_query = header_shows_in_full(&header, &self.query, usize::from(width));
        lines.push(header);
        if !header_shows_query {
            lines.extend(render_compact_kv(
                "query",
                &self.query,
                tool_value_style(),
                width,
            ));
        }
        if let Some(source) = self.source.as_ref() {
            lines.extend(render_compact_kv(
                "source",
                source,
                tool_value_style(),
                width,
            ));
        }
        if let Some(degraded) = self.degraded.as_ref() {
            lines.extend(render_compact_kv(
                "degraded",
                degraded,
                tool_value_style(),
                width,
            ));
        }
        if self.ref_count > 0 {
            lines.extend(render_compact_kv(
                "citations",
                &self.ref_count.to_string(),
                tool_value_style(),
                width,
            ));
        }
        if let Some(summary) = self.summary.as_ref() {
            lines.extend(render_compact_kv(
                "result",
                summary,
                tool_value_style(),
                width,
            ));
        }
        lines
    }
}

/// Generic cell for tool output when no specialized rendering exists.
#[derive(Debug, Clone)]
pub struct GenericToolCell {
    pub name: String,
    pub status: ToolStatus,
    pub input_summary: Option<String>,
    pub output: Option<String>,
    /// Optional list of per-child prompts. When populated (by any future
    /// fan-out tool), each prompt is shown on its own indented row instead
    /// of the inline `args:` summary. `None` for ordinary tools.
    pub prompts: Option<Vec<String>>,
    /// Filesystem path to the full output's spillover file (#422/#423).
    /// Set by the tool-routing layer when `ToolResult.metadata` carried a
    /// `spillover_path` field. The truncation affordance includes the
    /// path so the user can `read_file` it (or Cmd+click in
    /// OSC 8-aware terminals — the path renders as a hyperlink when
    /// `tui.osc8_links` is enabled).
    pub spillover_path: Option<std::path::PathBuf>,
    // --- Pre-computed render cache (populated once at cell creation) ---
    /// Cached output summary — avoids re-parsing JSON every frame.
    pub output_summary: Option<String>,
    /// Whether the output looks like a unified diff (cached after first check).
    pub is_diff: bool,
}

fn should_show_raw_tool_name(
    name: &str,
    family: crate::tui::widgets::tool_card::ToolFamily,
    mode: RenderMode,
) -> bool {
    !matches!(mode, RenderMode::Live)
        || matches!(family, crate::tui::widgets::tool_card::ToolFamily::Generic)
        || name.starts_with("mcp_")
}

impl GenericToolCell {
    /// Render the generic tool cell into lines.
    ///
    /// `mode` controls multi-line output handling: `Live` caps at
    /// `TOOL_OUTPUT_LINE_LIMIT` rows with a "+N more" affordance;
    /// `Transcript` uses a larger bounded display projection.
    #[cfg(test)]
    pub fn lines_with_mode(
        &self,
        width: u16,
        low_motion: bool,
        mode: RenderMode,
    ) -> Vec<Line<'static>> {
        self.lines_with_mode_and_locale(width, low_motion, mode, Locale::En)
    }

    pub fn lines_with_mode_and_locale(
        &self,
        width: u16,
        low_motion: bool,
        mode: RenderMode,
        locale: Locale,
    ) -> Vec<Line<'static>> {
        if mode != RenderMode::Source && self.name == "activity_group" {
            return agent_activity::render_activity_group(self, width, locale);
        }

        // Issue #241: when the underlying tool is a checklist/todo update and
        // the output is parseable, render a purpose-built progress card
        // instead of dumping the JSON into the generic tool block.
        if mode != RenderMode::Source
            && let Some(lines) = self.try_render_as_checklist(width, low_motion, mode, locale)
        {
            return lines;
        }

        // #4038 / #4122: purpose-built workflow run card (compact in live,
        // expanded in transcript) shared with the WorkflowPanel state machine.
        if mode != RenderMode::Source
            && let Some(lines) = self.try_render_as_workflow(width, low_motion, mode, locale)
        {
            return lines;
        }

        // Sub-agent launch already gets a dedicated `DelegateCard`
        // that owns the live action tree, status, and final summary (#4133).
        // Spawns therefore render nothing here in either mode — one visible
        // artifact per delegated unit. Inspection/join calls (peek/status/
        // wait) stay as a single compact line (#4112 dogfood A5).
        if mode != RenderMode::Source && self.name == "agent" {
            if agent_activity::is_agent_inspection(self) {
                return agent_activity::render_agent_compact(self, low_motion, locale);
            }
            // Spawn / start / run: suppress the generic tool card entirely.
            return Vec::new();
        }

        // A call to a tool that doesn't exist carries exactly one useful
        // fact: the catalog error. The full name:/args:/result: block turns
        // each model slip into a four-line card (dogfood A5) — collapse it
        // to a single header line in both render modes.
        if mode != RenderMode::Source
            && self.status == ToolStatus::Failed
            && let Some(output) = self.output.as_deref()
            && output.contains("is not available in the current tool catalog")
        {
            let family = crate::tui::widgets::tool_card::tool_family_for_name(&self.name);
            let summary = truncate_text(output.trim(), 200);
            return wrap_card_rail(
                vec![render_tool_header_with_family_and_summary(
                    family,
                    Some(summary.as_str()),
                    None,
                    self.status,
                    None,
                    low_motion,
                    locale,
                )],
                self.status,
            );
        }

        // Live mode stays calm: successful tool calls collapse to one header
        // line, and non-read in-flight tools do the same. Failures keep their
        // body visible because error output is the useful part.
        if matches!(mode, RenderMode::Live) {
            let family = crate::tui::widgets::tool_card::tool_family_for_name(&self.name);
            let is_read_family = matches!(
                family,
                crate::tui::widgets::tool_card::ToolFamily::Read
                    | crate::tui::widgets::tool_card::ToolFamily::Find
            );
            let should_collapse = self.status == ToolStatus::Success
                || (self.status != ToolStatus::Failed && !is_read_family);
            if should_collapse || self.spillover_path.is_some() {
                let header_summary = crate::tui::widgets::tool_card::tool_header_summary_for_name(
                    &self.name,
                    self.input_summary.as_deref(),
                );
                let mut collapsed = vec![render_tool_header_with_family_and_summary(
                    family,
                    header_summary.as_deref(),
                    Some(
                        tool_receipt_label(family, self.status, self.output.as_deref(), locale)
                            .as_ref(),
                    ),
                    self.status,
                    None,
                    low_motion,
                    locale,
                )];
                if self.spillover_path.is_some() {
                    collapsed.push(render_spillover_annotation(width, locale));
                }
                return wrap_card_rail(collapsed, self.status);
            }
        }

        let mut lines = Vec::new();
        // Map the actual tool name (e.g. `agent`, `apply_patch`) to a
        // family rather than the catch-all `"Tool"` title — this is what
        // gives a `GenericToolCell` the right verb glyph (◐ delegate, ⋮⋮
        // fanout, etc.) instead of falling back to the neutral bullet.
        let family = crate::tui::widgets::tool_card::tool_family_for_name(&self.name);
        let header_summary = crate::tui::widgets::tool_card::tool_header_summary_for_name(
            &self.name,
            self.input_summary.as_deref(),
        );
        let header = render_tool_header_with_family_and_summary(
            family,
            header_summary.as_deref(),
            Some(tool_receipt_label(family, self.status, self.output.as_deref(), locale).as_ref()),
            self.status,
            None,
            low_motion,
            locale,
        );
        let header_has_args = self.input_summary.as_deref().is_some_and(|args| {
            header_repeats_args(&header, header_summary.as_deref(), args, usize::from(width))
        });
        lines.push(header);
        if should_show_raw_tool_name(&self.name, family, mode)
            && !crate::tui::widgets::tool_card::should_show_tool_name_in_header(&self.name, family)
        {
            lines.extend(render_compact_kv(
                "name",
                &self.name,
                tool_value_style(),
                width,
            ));
        }

        // Prefer per-prompt rows over the generic args summary when the tool
        // exposes a list of child prompts. One row per child with a `[i]`
        // index makes the fan-out legible without expanding JSON.
        let show_prompts = matches!(self.status, ToolStatus::Running) || self.output.is_none();
        if show_prompts
            && let Some(prompts) = self.prompts.as_ref()
            && !prompts.is_empty()
        {
            for (idx, prompt) in prompts.iter().enumerate() {
                let label = if idx == 0 { "prompts" } else { "" };
                let value = format!("[{idx}] {}", truncate_text(prompt.trim(), 200));
                lines.extend(render_card_detail_line(
                    if label.is_empty() { None } else { Some(label) },
                    &value,
                    tool_value_style(),
                    width,
                ));
            }
        } else {
            let show_args = (matches!(self.status, ToolStatus::Running | ToolStatus::Failed)
                || self.output.is_none())
                && !header_has_args;
            if show_args && let Some(summary) = self.input_summary.as_ref() {
                lines.extend(render_compact_kv(
                    "args",
                    summary,
                    tool_value_style(),
                    width,
                ));
            }
        }

        if let Some(output) = self.output.as_ref() {
            if self.is_diff {
                let diff_summary = diff_render::diff_summary_label(output);
                lines.push(render_tool_header_with_summary(
                    "Diff",
                    diff_summary.as_deref(),
                    None,
                    self.status,
                    None,
                    low_motion,
                    locale,
                ));
                if mode == RenderMode::Source {
                    lines.push(Line::from(output.clone()));
                } else {
                    let limit = if mode == RenderMode::Transcript {
                        512
                    } else if self.status == ToolStatus::Failed {
                        TOOL_FAILURE_PREVIEW_LINES
                    } else {
                        TOOL_OUTPUT_LINE_LIMIT
                    };
                    let rendered = diff_render::render_diff_bounded(output, width, limit);
                    lines.extend(rendered.lines);
                    if rendered.omitted_rows > 0 {
                        let detail_hint =
                            crate::tui::key_shortcuts::tool_details_shortcut_action_hint("diff");
                        let body = tr(locale, MessageId::FileMutationMoreDiff)
                            .replace("{count}", &rendered.omitted_body_rows.to_string())
                            .replace("{hint}", &detail_hint);
                        let omitted = if rendered.omitted_summary_rows > 0 {
                            format!(
                                "{} {} · {body}",
                                tr(locale, MessageId::ConfigChoiceDiffSummary),
                                tr(locale, MessageId::ApprovalMoreLines)
                                    .replace("{count}", &rendered.omitted_summary_rows.to_string())
                                    .trim_start()
                            )
                        } else {
                            body
                        };
                        lines.push(details_affordance_line(
                            &omitted,
                            Style::default().fg(palette::TEXT_MUTED).italic(),
                        ));
                    }
                }
            } else {
                let limit = if self.status == ToolStatus::Failed {
                    TOOL_FAILURE_PREVIEW_LINES
                } else {
                    TOOL_OUTPUT_LINE_LIMIT
                };
                lines.extend(render_tool_output_mode(output, width, limit, mode, locale));
            }

            if matches!(mode, RenderMode::Live) && self.spillover_path.is_some() {
                lines.push(render_spillover_annotation(width, locale));
            }
        }
        wrap_card_rail(lines, self.status)
    }

    /// If this cell is a checklist/todo write/add/update and the output is
    /// parseable as a checklist snapshot, render a purpose-built checklist
    /// card instead of the generic `name: ... { json }` block (issue #241).
    fn try_render_as_checklist(
        &self,
        width: u16,
        low_motion: bool,
        mode: RenderMode,
        locale: Locale,
    ) -> Option<Vec<Line<'static>>> {
        if !is_checklist_tool_name(&self.name) {
            return None;
        }
        let output = self.output.as_ref()?;
        let snapshot = parse_checklist_snapshot(output)?;

        // Concise update rendering (#403). When the tool emits an
        // "Updated todo #N to STATUS" prefix line — which `todo_update` /
        // `checklist_update` always do on a successful match — render
        // only the changed item plus a `M/N · pct%` summary instead of
        // dumping the full list every time. The full list is still
        // reachable via `v` on the tool detail record. This keeps the
        // transcript scannable in long sessions.
        if matches!(mode, RenderMode::Live)
            && let Some(change) = parse_update_prefix(output)
        {
            return Some(render_checklist_change_card(
                &self.name,
                self.status,
                &snapshot,
                &change,
                width,
                low_motion,
                locale,
            ));
        }

        Some(render_checklist_card(
            &self.name,
            self.status,
            &snapshot,
            width,
            low_motion,
            mode,
            locale,
        ))
    }

    /// Render the `workflow` tool as the transcript's one row per run
    /// (#4122): while the run is live, the call that launched it is one
    /// `started` line naming the run; when it settles,
    /// `App::announce_settled_workflows` swaps that card's record for the
    /// finish, so the same row becomes the final state — outcome counts,
    /// elapsed, tokens — with the result or the reason under it. A card that
    /// returned a settled record (a foreground `run`, a `status` poll) shows
    /// only the finish. Live progress is the workbar's, not the transcript's.
    /// Transcript mode expands the finish card's phase/child detail.
    /// Status-list payloads keep a multi-run summary card.
    fn try_render_as_workflow(
        &self,
        width: u16,
        low_motion: bool,
        mode: RenderMode,
        locale: Locale,
    ) -> Option<Vec<Line<'static>>> {
        if self.name != "workflow" {
            return None;
        }
        let output = self.output.as_ref()?;
        let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
            // A call the runtime refused (invalid plan, unknown fleet) never
            // started a run. It says why — the error's first sentence, wrapped
            // under the header — instead of echoing `action: start`; the full
            // error and validation feedback stay behind the details key.
            if self.status != ToolStatus::Failed {
                return None;
            }
            let reason = workflow_call_error_reason(output)?;
            let family = crate::tui::widgets::tool_card::tool_family_for_name("workflow");
            let mut lines = vec![render_tool_header_with_family_and_summary(
                family,
                None,
                None,
                self.status,
                None,
                low_motion,
                locale,
            )];
            lines.extend(render_card_detail_line(
                None,
                &reason,
                tool_value_style(),
                width,
            ));
            if !matches!(mode, RenderMode::Live) {
                lines.extend(render_tool_output_mode(
                    output,
                    width,
                    TOOL_OUTPUT_LINE_LIMIT,
                    mode,
                    locale,
                ));
            }
            return Some(wrap_card_rail(lines, self.status));
        };
        let is_status_list =
            value.get("action").and_then(serde_json::Value::as_str) == Some("status");
        if value.get("run_id").is_none() && !is_status_list {
            return None;
        }
        let family = crate::tui::widgets::tool_card::tool_family_for_name("workflow");
        let mut lines = Vec::new();

        if is_status_list {
            let runs = value.get("runs").and_then(serde_json::Value::as_array);
            let count = value
                .get("count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_else(|| runs.map(|r| r.len() as u64).unwrap_or(0));
            let header = format!("{count} run(s)");
            lines.push(render_tool_header_with_family_and_summary(
                family,
                Some(header.as_str()),
                None,
                self.status,
                None,
                low_motion,
                locale,
            ));
            if let Some(runs) = runs {
                for run in runs {
                    let run_id = run
                        .get("run_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("?");
                    let status = run
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("?");
                    let children = run
                        .get("child_count")
                        .and_then(serde_json::Value::as_u64)
                        .or_else(|| {
                            run.get("child_ids")
                                .and_then(serde_json::Value::as_array)
                                .map(|a| a.len() as u64)
                        })
                        .unwrap_or(0);
                    lines.extend(render_card_detail_line(
                        None,
                        &format!("{run_id} · {status} · {children} child(ren)"),
                        tool_value_style(),
                        width,
                    ));
                }
            }
            return Some(wrap_card_rail(lines, self.status));
        }

        use crate::tui::widgets::workflow_panel::{WorkflowHistoryExtras, WorkflowPanel};
        let mut panel = WorkflowPanel::from_run_json(&value)?;
        panel.locale = locale;
        let finish_card = value
            .get("transcript_line")
            .and_then(serde_json::Value::as_str)
            == Some("finished");
        // A foreground `run` card returns the settled record itself, so it
        // carries its own finish; a detached start is finished by the card
        // `App::announce_settled_workflows` writes later.
        let owns_finish =
            finish_card || (self.status != ToolStatus::Running && panel.lifecycle.is_terminal());
        if !owns_finish {
            // The start line: the run's name. Its progress lives in the
            // workbar, never here. A card that already holds the settled
            // record shows only the finish: the final state replaces
            // `started`, one row per run.
            let name = panel.short_title();
            lines.push(render_tool_header_with_family_and_summary(
                family,
                Some(name.as_str()),
                Some(
                    codewhale_localization::tr(
                        locale,
                        codewhale_localization::MessageId::WorkflowLineStarted,
                    )
                    .as_ref(),
                ),
                if owns_finish {
                    ToolStatus::Success
                } else {
                    self.status
                },
                None,
                low_motion,
                locale,
            ));
        }
        let finish = owns_finish.then(|| panel.finish_line()).flatten();
        let Some((state, facts, detail)) = finish else {
            return Some(wrap_card_rail(lines, self.status));
        };
        let finish_status = match panel.lifecycle {
            crate::tui::widgets::workflow_panel::WorkflowPanelLifecycle::Succeeded => {
                ToolStatus::Success
            }
            crate::tui::widgets::workflow_panel::WorkflowPanelLifecycle::Degraded => {
                ToolStatus::Warning
            }
            _ => ToolStatus::Failed,
        };
        lines.push(render_tool_header_with_family_and_summary(
            family,
            Some(facts.as_str()),
            Some(&state),
            finish_status,
            None,
            low_motion,
            locale,
        ));
        if let Some(detail) = detail {
            // Why a run fell short is the one fact worth reading: its first
            // sentence wraps instead of being cut. A success summary keeps to
            // one row, cut at a word boundary; the expand key shows the rest.
            let detail = if finish_status == ToolStatus::Failed {
                detail
            } else {
                let room = usize::from(width).saturating_sub(4).max(8);
                codewhale_tui_kit::ui_text::semantic_truncate(&detail, room)
            };
            lines.extend(render_card_detail_line(
                None,
                &detail,
                tool_value_style(),
                width,
            ));
        }
        let expanded = !matches!(mode, RenderMode::Live);
        if expanded {
            let extras = WorkflowHistoryExtras {
                result_summary: panel.result_summary.clone(),
                source_path: panel.source_path.clone().or_else(|| {
                    value
                        .get("source_path")
                        .and_then(serde_json::Value::as_str)
                        .map(std::path::PathBuf::from)
                }),
                spillover_path: self.spillover_path.clone(),
                verification_summary: value
                    .get("verification")
                    .and_then(|v| v.get("summary"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            };
            for detail in panel.history_expanded_lines(width, &extras) {
                // history_expanded_lines omit the leading indent; re-use the
                // card detail path so rails and spacing stay consistent.
                let text: String = detail.spans.iter().map(|s| s.content.as_ref()).collect();
                lines.extend(render_card_detail_line(
                    None,
                    &text,
                    tool_value_style(),
                    width,
                ));
            }
        }
        Some(wrap_card_rail(lines, finish_status))
    }
}

/// The first sentence of a refused `workflow` call's error, without the
/// dispatcher's `Error: Invalid input for tool 'workflow':` preamble, which
/// names the tool the card already names.
fn workflow_call_error_reason(output: &str) -> Option<String> {
    let first = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let first = first.strip_prefix("Error:").map_or(first, str::trim_start);
    let first = if first.starts_with("Invalid input for tool") {
        first.split_once("': ").map_or(first, |(_, rest)| rest)
    } else {
        first
    };
    let reason = crate::tui::widgets::workflow_panel::first_sentence(first);
    (!reason.is_empty()).then_some(reason)
}

/// Render the inline annotation for a tool cell whose full output was
/// retained internally and replaced by a bounded preview. The annotation
/// stays calm and path-free: it only says the output was shortened and that
/// the details shortcut opens the full retained output.
fn render_spillover_annotation(width: u16, locale: Locale) -> Line<'static> {
    // Matches the model-facing preview footer (truncate.rs) and the existing
    // "Alt+V opens …" hint style (#3256): one quiet line, no handles or paths.
    let affordance = tr(locale, MessageId::ToolOutputShortened).replace(
        "{key}",
        &crate::tui::key_shortcuts::tool_details_shortcut_label(),
    );
    let room = usize::from(width)
        .saturating_sub(UnicodeWidthStr::width(TRANSCRIPT_RAIL))
        .max(8);
    details_affordance_line(
        &truncate_text(&affordance, room),
        Style::default().fg(palette::TEXT_MUTED).italic(),
    )
}

/// Detect a truncated-output preview: the current model-facing footer (which
/// names the artifact path and recovery instruction), the previous plain
/// footer, or the legacy receipt header still present in older saved
/// sessions. Live cards collapse to the expand affordance for all of them.
fn is_truncated_output_preview(content: &str) -> bool {
    content.contains(codewhale_tools::names::SPILLOVER_RECOVERY_HINT)
        || content.contains(codewhale_engine::tools::truncate::SPILLOVER_PREVIEW_HINT)
        || content.trim_start().starts_with("[Exact evidence retained")
}

fn render_command_mode(
    command: &str,
    width: u16,
    mode: RenderMode,
    calm: bool,
    locale: Locale,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if mode == RenderMode::Source {
        lines.push(render_card_detail_line_single(
            Some("command"),
            command,
            tool_value_style(),
        ));
        return lines;
    }
    let cap = match mode {
        RenderMode::Live => TOOL_COMMAND_LINE_LIMIT,
        RenderMode::Transcript | RenderMode::Source => 512,
    };
    let command = if calm {
        command.trim_start_matches("$ ")
    } else {
        command
    };
    let wrap_width = width.saturating_sub(4).max(1) as usize;
    let mut count = 0usize;
    for line in command.lines() {
        for_each_wrapped_text(line, wrap_width, |chunk| {
            if count < cap && chunk.len() > 4 * 1024 {
                lines.push(render_spillover_annotation(width, locale));
            } else if count < cap {
                if calm {
                    let prompt = if count == 0 { "$ " } else { "  " };
                    lines.extend(render_card_detail_line(
                        None,
                        &format!("{prompt}{chunk}"),
                        tool_value_style(),
                        width,
                    ));
                } else {
                    lines.extend(render_card_detail_line(
                        if count == 0 { Some("command") } else { None },
                        chunk,
                        tool_value_style(),
                        width,
                    ));
                }
            }
            count = count.saturating_add(1);
        });
    }
    if count > cap {
        lines.push(details_affordance_line(
            &crate::tui::key_shortcuts::tool_details_shortcut_action_hint(&tr(
                locale,
                MessageId::ApprovalLabelCommand,
            )),
            Style::default().fg(palette::TEXT_MUTED),
        ));
    }
    lines
}

fn command_header_summary(command: &str) -> String {
    command
        .lines()
        .next()
        .unwrap_or(command)
        .trim_start_matches("$ ")
        .trim()
        .to_string()
}

fn exploring_header_summary(entries: &[ExploringEntry]) -> Option<String> {
    match entries {
        [] => None,
        [entry] => Some(entry.label.clone()),
        entries => Some(format!("{} items", entries.len())),
    }
}

/// Choose the verb family for an exploring card's header. A card whose entries
/// are all searches reads with the `find` verb so the completed action agrees
/// with its `Searching for …` labels (#4145); every other exploration mix keeps
/// the neutral `read` verb the Workspace card uses. The search signal is the
/// English label prefix produced by `exploring_label` in `tool_routing`.
fn exploring_card_family(entries: &[ExploringEntry]) -> crate::tui::widgets::tool_card::ToolFamily {
    use crate::tui::widgets::tool_card::ToolFamily;
    let all_search = !entries.is_empty()
        && entries
            .iter()
            .all(|entry| entry.label.starts_with("Searching"));
    if all_search {
        ToolFamily::Find
    } else {
        ToolFamily::Read
    }
}

fn render_compact_kv(label: &str, value: &str, style: Style, width: u16) -> Vec<Line<'static>> {
    render_card_detail_line(Some(label.trim_end_matches(':')), value, style, width)
}

/// Wrap rendered tool-card lines with card-rail glyphs (╭ │ ╰).
/// First non-empty line gets `╭`, middle lines get `│`, last line gets `╰`.
/// Single-line cards get a single `─` prefix.
///
/// The rail is the card's border, so it carries the cell's own state: it is
/// painted with [`tool_rail_color`], OMP's border rule. A running card is
/// bordered in the action accent, a failed one in the error colour, and a
/// settled one recedes to muted — which is what makes a tool cell legible
/// without a neighbouring row narrating what the card already shows. The
/// header glyph inside it follows [`tool_glyph_color`] instead, so a finished
/// card still says what it was.
fn wrap_card_rail(mut lines: Vec<Line<'static>>, status: ToolStatus) -> Vec<Line<'static>> {
    let n = lines.len();
    if n == 0 {
        return lines;
    }
    let rail_style = Style::default().fg(tool_rail_color(status));
    if n == 1 {
        lines[0].spans.insert(0, Span::styled("─ ", rail_style));
        return lines;
    }
    for (i, line) in lines.iter_mut().enumerate() {
        let rail = if i == 0 {
            "\u{256D} " // ╭
        } else if i == n - 1 {
            "\u{2570} " // ╰
        } else {
            "\u{2502} " // │
        };
        line.spans.insert(0, Span::styled(rail, rail_style));
    }
    lines
}

/// The legacy tool renderers accept only a low-motion boolean, which gives
/// both Reduced and Still a mix of legacy static frames. Preserve that stable
/// rendering path, then apply the central mode's exact fallback at the typed
/// header span (never by rewriting user/tool output text).
fn apply_static_tool_markers(lines: &mut [Line<'static>], marker: &'static str) {
    for (line_index, line) in lines.iter_mut().enumerate() {
        let mut index = 0;
        if line
            .spans
            .first()
            .is_some_and(|span| matches!(span.content.as_ref(), "─ " | "╭ " | "│ " | "╰ "))
        {
            index = 1;
        }
        let Some(status) = line.spans.get(index).map(|span| span.content.as_ref()) else {
            continue;
        };
        let family = line.spans.get(index + 1).map(|span| span.content.as_ref());
        let header = line_index == 0
            || family.is_some_and(|family| {
                family.ends_with(' ') && is_tool_family_glyph(family.trim_end())
            });
        if !status.ends_with(' ') || !is_tool_status_glyph(status.trim_end()) || !header {
            continue;
        }
        let mut chars = status.trim_end().chars();
        if matches!(chars.next(), Some('\u{2800}'..='\u{28FF}')) && chars.next().is_none() {
            line.spans[index].content = format!("{marker} ").into();
        }
    }
}

/// Return the width of tool-cell chrome that remains after the transcript
/// cache removes the cell-local card rail. Labeled tool headers have two
/// additional visual tokens (`✓`/spinner and the family glyph); a calm shell
/// header has only its status mark, and only a spinner frame of it is chrome;
/// detail rows have the thin transcript rail. Keeping this width with the
/// rendered line avoids making selection copy infer chrome from glyph ranges,
/// which can consume real code or CJK text that happens to begin with a
/// box-drawing character.
fn tool_copy_prefix_width(line: &Line<'static>, first_line: bool) -> usize {
    let spans = line.spans.as_slice();
    let mut index = 0;

    // The cache removes these exact local card rails before flattening.
    if spans
        .first()
        .is_some_and(|span| matches!(span.content.as_ref(), "─ " | "╭ " | "│ " | "╰ "))
    {
        index = 1;
    }

    // Detail rows and pager affordances use the transcript rail as their
    // first span. The live transcript's general rail accounting removes it;
    // do not report it again as cell-local copy chrome.
    if spans
        .get(index)
        .is_some_and(|span| span.content.as_ref() == TRANSCRIPT_RAIL)
    {
        return 0;
    }

    // A labeled tool header starts with `<status> <family> `; a calm shell
    // header starts with `<status> ` on the cell's first row. Only consume
    // these tokens in those positions, so output beginning with `✓` or a
    // braille character remains copyable content.
    let Some(status) = spans.get(index).map(|span| span.content.as_ref()) else {
        return 0;
    };
    if !status.ends_with(' ') || !is_tool_status_glyph(status.trim_end()) {
        return 0;
    }
    if let Some(family) = spans.get(index + 1).map(|span| span.content.as_ref())
        && family.ends_with(' ')
        && is_tool_family_glyph(family.trim_end())
    {
        return UnicodeWidthStr::width(status) + UnicodeWidthStr::width(family);
    }
    let mark = status.trim_end();
    if first_line
        && (mark == codewhale_tui_kit::spinner::LIVE_STATIC_MARKER
            || mark.chars().all(|ch| matches!(ch, '\u{2800}'..='\u{28FF}')))
    {
        return UnicodeWidthStr::width(status);
    }
    0
}

fn is_tool_status_glyph(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(ch) = chars.next() else {
        return false;
    };
    chars.next().is_none()
        && matches!(
            ch,
            '\u{2713}' // ✓
                | '\u{2715}' // ✕
                | '\u{00B7}' // ·
                | '!' // terminal warning
                | '\u{203A}' // › static still-mode marker
                | '\u{2800}'..='\u{28FF}' // braille spinner frames
        )
}

fn is_tool_family_glyph(text: &str) -> bool {
    use crate::tui::widgets::tool_card::{ToolFamily, family_glyph};

    [
        ToolFamily::Read,
        ToolFamily::Patch,
        ToolFamily::Run,
        ToolFamily::Find,
        ToolFamily::Delegate,
        ToolFamily::Fanout,
        ToolFamily::Rlm,
        ToolFamily::Verify,
        ToolFamily::Think,
        ToolFamily::Generic,
    ]
    .into_iter()
    .any(|family| family_glyph(family) == text)
}

fn review_severity_color(severity: &str) -> Color {
    match severity {
        "error" => palette::STATUS_ERROR,
        "warning" => palette::STATUS_WARNING,
        _ => palette::WHALE_ACTION,
    }
}

fn format_review_location(path: Option<&String>, line: Option<u32>) -> String {
    let path = path.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
    match (path, line) {
        (Some(path), Some(line)) => format!("{path}:{line}"),
        (Some(path), None) => path,
        (None, Some(line)) => format!("line {line}"),
        (None, None) => String::new(),
    }
}

fn status_symbol(
    started_at: Option<Instant>,
    status: ToolStatus,
    low_motion: bool,
    family: crate::tui::widgets::tool_card::ToolFamily,
) -> String {
    match status {
        ToolStatus::Running if family == crate::tui::widgets::tool_card::ToolFamily::Verify => {
            codewhale_tui_kit::spinner::verification_tick_frame(started_at, low_motion).to_string()
        }
        ToolStatus::Running => {
            codewhale_tui_kit::spinner::braille_spinner_frame(started_at, low_motion).to_string()
        }
        ToolStatus::Success => TOOL_DONE_SYMBOL.to_string(),
        ToolStatus::Hydrated | ToolStatus::Stopped => {
            codewhale_tui_kit::glyphs::NEUTRAL.to_string()
        }
        ToolStatus::Warning => "!".to_string(),
        ToolStatus::Failed => TOOL_FAILED_SYMBOL.to_string(),
    }
}

fn details_affordance_line(text: &str, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            TRANSCRIPT_RAIL.to_string(),
            Style::default().fg(palette::TEXT_DIM),
        ),
        Span::styled(text.to_string(), style),
    ])
}

/// Truncate to `max_len` display cells, cutting only at grapheme-cluster
/// boundaries. Counting chars let CJK text overflow a width budget and could
/// split a ZWJ or combining sequence (U04-m5); ASCII output is unchanged.
fn truncate_text(text: &str, max_len: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    if codewhale_tui_kit::ui_text::text_display_width(text) <= max_len {
        return text.to_string();
    }
    let budget = max_len.saturating_sub(3);
    let mut out = String::new();
    let mut used = 0usize;
    for grapheme in text.graphemes(true) {
        let width = codewhale_tui_kit::ui_text::text_display_width(grapheme);
        if used + width > budget {
            break;
        }
        used += width;
        out.push_str(grapheme);
    }
    out.push_str("...");
    out
}

/// Label glyph for an error cell. `Critical`/`Error` get the loudest marker;
/// `Warning` is softer; `Info` is neutral. Kept as ASCII so it survives any
/// terminal font fallback.
fn error_label_text(severity: codewhale_error_taxonomy::ErrorSeverity) -> &'static str {
    match severity {
        codewhale_error_taxonomy::ErrorSeverity::Critical
        | codewhale_error_taxonomy::ErrorSeverity::Error => "Error",
        codewhale_error_taxonomy::ErrorSeverity::Warning => "Warn",
        codewhale_error_taxonomy::ErrorSeverity::Info => "Info",
    }
}

/// Label color for an error cell — drives the leading rail glyph.
fn error_label_style(severity: codewhale_error_taxonomy::ErrorSeverity) -> Style {
    let color = match severity {
        codewhale_error_taxonomy::ErrorSeverity::Critical
        | codewhale_error_taxonomy::ErrorSeverity::Error => palette::STATUS_ERROR,
        codewhale_error_taxonomy::ErrorSeverity::Warning => palette::STATUS_WARNING,
        codewhale_error_taxonomy::ErrorSeverity::Info => palette::TEXT_DIM,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

/// Body color for an error cell — softer than the label so the rail draws
/// the eye but the prose stays readable.
fn error_body_style(severity: codewhale_error_taxonomy::ErrorSeverity) -> Style {
    let color = match severity {
        codewhale_error_taxonomy::ErrorSeverity::Critical
        | codewhale_error_taxonomy::ErrorSeverity::Error => palette::STATUS_ERROR,
        codewhale_error_taxonomy::ErrorSeverity::Warning => palette::STATUS_WARNING,
        codewhale_error_taxonomy::ErrorSeverity::Info => palette::TEXT_MUTED,
    };
    Style::default().fg(color)
}

/// Render an engine error without markdown interpretation. The live transcript
/// advertises the dedicated full-error pager once the error wraps past four
/// rows: terminal height, scroll position, and adjacent tool cards can
/// otherwise make a multiline recovery instruction look like a clipped
/// one-line failure. Transcript/pager mode omits the recursive affordance
/// while preserving every character.
fn render_error_message(
    message: &str,
    severity: codewhale_error_taxonomy::ErrorSeverity,
    width: u16,
    show_full_error_affordance: bool,
) -> Vec<Line<'static>> {
    // Error messages are machine-generated and should not be run through
    // markdown rendering, which would mangle env-var names containing
    // underscores (e.g. CODEWHALE_ALLOW_INSECURE_HTTP would lose them as
    // italic markers).
    let label = error_label_text(severity);
    let label_style = error_label_style(severity);
    let body_style = error_body_style(severity);
    let prefix_width = UnicodeWidthStr::width(label);
    let content_width = width.saturating_sub(2 + prefix_width as u16).max(1);
    let mut lines: Vec<_> = message
        .split('\n')
        .flat_map(|line| wrap_plain_line(line, body_style, content_width))
        .collect();
    if let Some(first) = lines.get_mut(0) {
        first.spans.insert(0, Span::raw(" "));
        first.spans.insert(0, Span::styled(label, label_style));
    }
    let rail = format!("{}{}", '\u{258F}', " ".repeat(prefix_width));
    let rail_style = Style::default().fg(palette::TEXT_DIM);
    for line in lines.iter_mut().skip(1) {
        line.spans.insert(0, Span::styled(rail.clone(), rail_style));
    }
    if show_full_error_affordance && lines.len() > 4 {
        lines.push(details_affordance_line(
            &crate::tui::key_shortcuts::tool_details_shortcut_action_hint("full error"),
            Style::default().fg(palette::TEXT_MUTED).italic(),
        ));
    }
    lines
}

fn render_tool_header(
    title: &str,
    state: Option<&str>,
    status: ToolStatus,
    started_at: Option<Instant>,
    low_motion: bool,
    locale: Locale,
) -> Line<'static> {
    let family = crate::tui::widgets::tool_card::tool_family_for_title(title);
    render_tool_header_with_family_and_summary(
        family, None, state, status, started_at, low_motion, locale,
    )
}

fn render_tool_header_with_summary(
    title: &str,
    summary: Option<&str>,
    state: Option<&str>,
    status: ToolStatus,
    started_at: Option<Instant>,
    low_motion: bool,
    locale: Locale,
) -> Line<'static> {
    let family = crate::tui::widgets::tool_card::tool_family_for_title(title);
    render_tool_header_with_family_and_summary(
        family, summary, state, status, started_at, low_motion, locale,
    )
}

/// Render a tool-card header with an explicit verb family. Lets callers
/// (e.g. `GenericToolCell`) bypass the legacy title→family mapping when
/// they already know the actual tool name.
fn render_tool_header_with_family_and_summary(
    family: crate::tui::widgets::tool_card::ToolFamily,
    summary: Option<&str>,
    state: Option<&str>,
    status: ToolStatus,
    started_at: Option<Instant>,
    low_motion: bool,
    locale: Locale,
) -> Line<'static> {
    // For long-running tools, append elapsed seconds so the user can see the
    // call isn't stuck. Threshold matches the eye's "did this hang?" reflex
    // — under 3s we stay quiet so quick reads/greps don't visually churn.
    let state_owned = match (state, status, started_at) {
        (Some(state), _, _) => state.to_string(),
        (None, ToolStatus::Running, Some(started)) => {
            running_status_label_with_elapsed(started.elapsed().as_secs(), locale)
        }
        _ => tool_status_label(status, locale).into_owned(),
    };

    let glyph = crate::tui::widgets::tool_card::family_glyph(family);
    let verb = crate::tui::widgets::tool_card::family_label(family, locale);

    let glyph_style = Style::default().fg(tool_glyph_color(status, family));
    let mut spans = vec![
        Span::styled(
            format!("{} ", status_symbol(started_at, status, low_motion, family)),
            glyph_style,
        ),
        Span::styled(format!("{glyph} "), glyph_style),
        Span::styled(verb.to_string(), tool_title_style(status)),
    ];
    if !state_owned.is_empty() {
        spans.push(Span::styled(" ", Style::default()));
        spans.push(Span::styled(state_owned, tool_status_style(status, family)));
    }

    // #4148: don't let the summary echo the verb it sits next to — an
    // identity/summary that resolves to the family word itself would render a
    // duplicate like "delegate · delegate". When the summary collapses to the
    // verb, the verb already carries the signal, so drop the redundant tail.
    if let Some(summary) = summary
        .and_then(normalize_header_summary)
        .filter(|summary| !summary.eq_ignore_ascii_case(verb.as_ref()))
    {
        spans.push(Span::styled(" · ", Style::default().fg(palette::TEXT_DIM)));
        spans.push(Span::styled(
            truncate_text(&summary, TOOL_HEADER_SUMMARY_LIMIT),
            Style::default().fg(palette::TEXT_MUTED),
        ));
    }

    Line::from(spans)
}

fn header_shows_in_full(header: &Line<'static>, summary: &str, available: usize) -> bool {
    normalize_header_summary(summary).is_none_or(|summary| {
        codewhale_tui_kit::ui_text::text_display_width(&summary) <= TOOL_HEADER_SUMMARY_LIMIT
    }) && header.width() <= available
}

fn header_repeats_args(
    header: &Line<'static>,
    header_summary: Option<&str>,
    args: &str,
    available: usize,
) -> bool {
    let args = args.trim();
    let shown = match args.split_once(": ") {
        Some((_, value)) if !args.contains(", ") => value.trim(),
        _ => args,
    };
    header_summary.is_some_and(|summary| {
        (summary == shown || summary.ends_with(&format!(" \u{00B7} {shown}")))
            && header_shows_in_full(header, summary, available)
    })
}

fn normalize_header_summary(summary: &str) -> Option<String> {
    let normalized = summary
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

/// Build the "running" label with an elapsed-seconds badge for long-running
/// tools. Below 3s the badge is suppressed to avoid visual churn for tools
/// that resolve in milliseconds; at 3s and beyond the badge appears and ticks
/// every second the tool stays in flight.
pub(crate) fn running_status_label_with_elapsed(elapsed_secs: u64, locale: Locale) -> String {
    let state = tool_status_label(ToolStatus::Running, locale);
    if elapsed_secs < 3 {
        state.into_owned()
    } else {
        format!("{state} ({elapsed_secs}s)")
    }
}

pub(crate) fn stale_shell_status_label(elapsed_since_output_ms: u64, locale: Locale) -> String {
    tr(locale, MessageId::ToolReceiptQuiet).replace(
        "{duration}",
        &codewhale_command_contract::elapsed::format_elapsed_ms(elapsed_since_output_ms),
    )
}

fn render_card_detail_line(
    label: Option<&str>,
    value: &str,
    value_style: Style,
    width: u16,
) -> Vec<Line<'static>> {
    let label_text = label.map(|text| format!("{text}:"));
    let prefix_width = UnicodeWidthStr::width(TRANSCRIPT_RAIL)
        + label_text.as_deref().map_or(0, UnicodeWidthStr::width)
        + usize::from(label.is_some());
    let content_width = usize::from(width).saturating_sub(prefix_width).max(1);

    let mut lines = Vec::new();
    for (idx, part) in wrap_text(value, content_width).into_iter().enumerate() {
        let mut spans = vec![Span::styled(
            TRANSCRIPT_RAIL.to_string(),
            Style::default().fg(palette::TEXT_DIM),
        )];
        if idx == 0 {
            if let Some(label_text) = label_text.as_deref() {
                spans.push(Span::styled(
                    label_text.to_string(),
                    tool_detail_label_style(),
                ));
                spans.push(Span::raw(" "));
            }
        } else if let Some(label_text) = label_text.as_deref() {
            spans.push(Span::raw(
                " ".repeat(UnicodeWidthStr::width(label_text) + 1),
            ));
        }
        spans.push(Span::styled(part, value_style));
        lines.push(Line::from(spans));
    }
    lines
}

/// `render_card_detail_line_single` for a coloured row: one line, never
/// wrapped, so an intact path or URL keeps the same hitbox with or without
/// colour.
fn render_card_detail_line_single_styled(
    label: Option<&str>,
    segments: &[tool_output::StyledSegment],
    value_style: Style,
) -> Line<'static> {
    let mut line = render_card_detail_line_single(label, "", value_style);
    line.spans.pop();
    for (text, style) in segments {
        line.spans
            .push(Span::styled(text.clone(), value_style.patch(*style)));
    }
    line
}

fn render_card_detail_line_single(
    label: Option<&str>,
    value: &str,
    value_style: Style,
) -> Line<'static> {
    let label_text = label.map(|text| format!("{text}:"));
    let mut spans = vec![Span::styled(
        TRANSCRIPT_RAIL.to_string(),
        Style::default().fg(palette::TEXT_DIM),
    )];
    if let Some(label_text) = label_text {
        spans.push(Span::styled(label_text, tool_detail_label_style()));
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled(value.to_string(), value_style));
    Line::from(spans)
}

// Tool-card ink. The transcript paints tool cards with the dark whale tokens
// regardless of the selected theme, as every other cell in this file does by
// reading the same `palette` constants directly.

fn tool_title_style(status: ToolStatus) -> Style {
    if status == ToolStatus::Success {
        Style::default().fg(palette::TEXT_MUTED)
    } else {
        Style::default()
            .fg(palette::TEXT_SOFT)
            .add_modifier(Modifier::BOLD)
    }
}

/// Right-side status text ("running", "done", "issue"). Reads as the glyph it
/// sits beside, not as the rail.
fn tool_status_style(
    status: ToolStatus,
    family: crate::tui::widgets::tool_card::ToolFamily,
) -> Style {
    Style::default().fg(tool_glyph_color(status, family))
}

/// Detail label style ("command:", "time:", step markers).
fn tool_detail_label_style() -> Style {
    Style::default().fg(palette::TEXT_DIM)
}

/// Colour of a tool cell's **rail** — the card border.
///
/// This is OMP's `output-block.ts` rule verbatim: a block takes a state and
/// its border colour follows it. In-flight takes the action accent, a settled
/// success recedes into muted text so finished work stops competing for the
/// eye, and only warning and failure keep a loud colour. `Hydrated` is a
/// stalled "loaded, not run", not live work, so it takes the hint
/// colour instead of borrowing the running accent and reading as in-flight.
///
/// The glyph and status word use the same lifecycle ink; the glyph shape
/// preserves the tool's identity after its successful result settles.
fn tool_rail_color(status: ToolStatus) -> Color {
    match status {
        ToolStatus::Running => palette::WHALE_ACTION,
        ToolStatus::Success | ToolStatus::Stopped => palette::TEXT_MUTED,
        ToolStatus::Hydrated => palette::TEXT_DIM,
        ToolStatus::Warning => palette::WHALE_HUMAN,
        ToolStatus::Failed => palette::WHALE_ERROR,
    }
}

/// Settled success recedes; warning and failure retain their attention ink.
fn tool_glyph_color(
    status: ToolStatus,
    _family: crate::tui::widgets::tool_card::ToolFamily,
) -> Color {
    tool_rail_color(status)
}

pub(crate) fn tool_status_label(status: ToolStatus, locale: Locale) -> Cow<'static, str> {
    tr(
        locale,
        match status {
            ToolStatus::Running => MessageId::ToolReceiptRunning,
            ToolStatus::Success => MessageId::ToolReceiptDone,
            ToolStatus::Hydrated => MessageId::ToolReceiptHydrated,
            ToolStatus::Stopped => MessageId::ToolReceiptStopped,
            ToolStatus::Warning => MessageId::ToolReceiptIssue,
            ToolStatus::Failed => MessageId::ToolReceiptFailed,
        },
    )
}

/// A finished read/find card can truthfully name how many rendered lines came
/// back. Generic command output does not preserve typed stdout/stderr streams,
/// so Run receipts stay at localized `done` instead of inventing per-stream
/// counts from display text.
pub(crate) fn tool_receipt_label(
    family: crate::tui::widgets::tool_card::ToolFamily,
    status: ToolStatus,
    output: Option<&str>,
    locale: Locale,
) -> Cow<'static, str> {
    if status != ToolStatus::Success {
        return tool_status_label(status, locale);
    }
    use crate::tui::widgets::tool_card::ToolFamily;
    match family {
        ToolFamily::Read | ToolFamily::Find => {
            let lines = output.map(count_output_lines).unwrap_or(0);
            if lines == 0 {
                codewhale_localization::tr(
                    locale,
                    codewhale_localization::MessageId::ToolReceiptDone,
                )
            } else if lines == 1 {
                codewhale_localization::tr(
                    locale,
                    codewhale_localization::MessageId::ToolReceiptLinesSingular,
                )
            } else {
                Cow::Owned(
                    codewhale_localization::tr(
                        locale,
                        codewhale_localization::MessageId::ToolReceiptLinesPlural,
                    )
                    .replace("{count}", &lines.to_string()),
                )
            }
        }
        ToolFamily::Run => {
            codewhale_localization::tr(locale, codewhale_localization::MessageId::ToolReceiptDone)
        }
        _ => codewhale_localization::tr(locale, codewhale_localization::MessageId::ToolReceiptDone),
    }
}

fn count_output_lines(output: &str) -> usize {
    if output.is_empty() {
        0
    } else {
        output.lines().count()
    }
}

/// Default value style for tool detail rows.
fn tool_value_style() -> Style {
    Style::default().fg(palette::TEXT_MUTED)
}

/// Find the first `path:line` reference in a rendered cell, else the first
/// bare workspace file path, at line 1.
///
/// Pure: it resolves and stats candidate paths but never launches anything.
/// Spawning the editor belongs to `external_editor`, which owns the terminal
/// handoff — this used to build its own `Command` and `spawn()` it detached
/// while the TUI still held raw mode, the alt screen and mouse capture, and it
/// did that once per matching line, so one click could leave N editors fighting
/// the TUI for the same tty (#6235).
///
/// Returns the first match rather than every match: a click is one request to
/// open one file.
pub(crate) fn first_file_line_reference(text: &str, workspace: &Path) -> Option<(PathBuf, u32)> {
    text.lines()
        .find_map(|line| file_line_reference(line, workspace))
        .or_else(|| {
            text.lines()
                .find_map(|line| bare_file_reference(line, workspace))
        })
}

/// The first `path:line` on one line of text that names a file inside the
/// workspace, as `(absolute path, line)`.
///
/// Accepts the forms tools and models print: `src/a.rs:12`, `src/a.rs:12:5`,
/// `--> src/a.rs:12:5` (rustc), `` `src/a.rs:12` `` and a reference inside a
/// sentence or brackets. The text is model output, so it is not trusted to
/// name a file: every candidate goes through [`workspace_file`], which
/// refuses `..`, absolute paths outside the workspace and links.
pub(crate) fn file_line_reference(line: &str, workspace: &Path) -> Option<(PathBuf, u32)> {
    reference_tokens(line).find_map(|token| {
        let (path_str, line_no) = split_path_line(token)?;
        if !looks_like_file_path(path_str) {
            return None;
        }
        let path_str = path_str.strip_prefix("./").unwrap_or(path_str);
        workspace_file(workspace, path_str).map(|absolute| (absolute, line_no))
    })
}

pub(crate) fn bare_file_reference(line: &str, workspace: &Path) -> Option<(PathBuf, u32)> {
    reference_tokens(line).find_map(|token| {
        let token = token.trim_end_matches(':');
        if !looks_like_file_path(token) {
            return None;
        }
        let token = token.strip_prefix("./").unwrap_or(token);
        workspace_file(workspace, token).map(|absolute| (absolute, 1))
    })
}

fn reference_tokens(line: &str) -> impl Iterator<Item = &str> {
    line.split_whitespace().map(|token| {
        // Leading `.` stays: `./src/a.rs` is relative, not `/src/a.rs`.
        token
            .trim_start_matches(['`', '\'', '"', '(', '[', '<'])
            .trim_end_matches(['`', '\'', '"', ')', ']', '>', ',', ';', '.'])
    })
}

/// The regular file `raw` names inside `workspace`, as
/// `workspace.join(relative)`, or `None`.
///
/// [`codewhale_snapshot::workspace_relative_path`] checks only the text, and
/// `is_file()` follows links, so a link inside the workspace (`vendor -> /`,
/// `notes.md -> ~/.ssh/config`) used to pass. Each part below the workspace
/// is read with `symlink_metadata` and a link is refused, as
/// `runtime_api::workspace::confined_directory` does, and the resolved path
/// must still sit under the resolved workspace. Callers that act later check
/// again at that point: this is a check at one moment, not a lock.
pub(crate) fn workspace_file(workspace: &Path, raw: &str) -> Option<PathBuf> {
    let relative = codewhale_snapshot::workspace_relative_path(workspace, raw)?;
    let mut path = workspace.to_path_buf();
    let mut is_file = false;
    for component in relative.components() {
        path.push(component);
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        if codewhale_util::path_identity::metadata_is_link_or_reparse(&metadata) {
            return None;
        }
        is_file = metadata.is_file();
    }
    let resolved_workspace = workspace.canonicalize().ok()?;
    let inside = path.canonicalize().ok()?.starts_with(&resolved_workspace);
    (is_file && inside).then_some(path)
}

/// Split `path:N`, `path:N:C` or `path:N:text` at the first all-digit
/// segment after the path. Earlier colons stay in the path (`C:\x.rs:3`).
fn split_path_line(token: &str) -> Option<(&str, u32)> {
    let mut offset = 0;
    for (index, part) in token.split(':').enumerate() {
        if index > 0 && !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()) {
            let path = token[..offset].strip_suffix(':')?;
            let line = part.parse().ok().filter(|line| *line > 0)?;
            return (!path.is_empty()).then_some((path, line));
        }
        offset += part.len() + 1;
    }
    None
}

/// Heuristic check whether a string looks like a file path (contains a
/// directory separator or a known source file extension).
fn looks_like_file_path(s: &str) -> bool {
    if s.contains('/') || s.contains('\\') {
        return true;
    }
    // Check for a known file extension
    if let Some((_, ext)) = s.rsplit_once('.') {
        let ext = ext.trim();
        matches!(
            ext,
            "rs" | "toml"
                | "md"
                | "sh"
                | "py"
                | "js"
                | "ts"
                | "json"
                | "yaml"
                | "yml"
                | "css"
                | "html"
                | "go"
                | "c"
                | "h"
                | "cpp"
                | "hpp"
                | "java"
                | "kt"
                | "swift"
                | "rb"
                | "php"
                | "lua"
                | "zig"
                | "mod"
                | "sum"
                | "lock"
                | "txt"
                | "ini"
                | "cfg"
                | "conf"
                | "env"
                | "gitignore"
                | "dockerfile"
                | "sql"
                | "r"
                | "ex"
                | "exs"
                | "vue"
                | "svelte"
                | "tsx"
                | "jsx"
                | "scss"
                | "sass"
                | "less"
                | "gradle"
                | "properties"
                | "xml"
                | "proto"
                | "nix"
        )
    } else {
        false
    }
}

/// Aggregated file activity for compact Work panel display (#4636).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileActivitySummary {
    pub files_read: u32,
    pub dirs_listed: u32,
    pub patterns_searched: u32,
    pub files_written: u32,
}

impl FileActivitySummary {
    pub fn is_empty(&self) -> bool {
        self.files_read == 0
            && self.dirs_listed == 0
            && self.patterns_searched == 0
            && self.files_written == 0
    }

    pub fn compact_display(&self, locale: Locale) -> Vec<String> {
        let mut parts = Vec::new();
        for (count, clause, singular, plural) in [
            (
                self.files_read,
                MessageId::ActivityRead,
                MessageId::ActivityFileOne,
                MessageId::ActivityFileMany,
            ),
            (
                self.dirs_listed,
                MessageId::ActivityListed,
                MessageId::ActivityDirectoryOne,
                MessageId::ActivityDirectoryMany,
            ),
            (
                self.patterns_searched,
                MessageId::ActivitySearched,
                MessageId::ActivityPatternOne,
                MessageId::ActivityPatternMany,
            ),
            (
                self.files_written,
                MessageId::ActivityWrote,
                MessageId::ActivityFileOne,
                MessageId::ActivityFileMany,
            ),
        ] {
            if count > 0 {
                let count = tool_run::counted(count as usize, locale, singular, plural);
                parts.push(tr(locale, clause).replace("{items}", &count));
            }
        }
        parts
    }

    pub fn from_tool_name(name: &str) -> Option<FileActivityKind> {
        match name {
            "read_file" | "Read" | "read" => Some(FileActivityKind::Read),
            "list_dir" | "list_directory" | "Glob" | "glob" => Some(FileActivityKind::List),
            "search" | "grep" | "Grep" | "grep_files" | "file_search" | "codebase_search" => {
                Some(FileActivityKind::Search)
            }
            "write_file" | "Write" | "apply_patch" | "Edit" | "edit_file" | "fim_edit" => {
                Some(FileActivityKind::Write)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileActivityKind {
    Read,
    List,
    Search,
    Write,
}

impl FileActivitySummary {
    pub fn record(&mut self, kind: FileActivityKind) {
        match kind {
            FileActivityKind::Read => self.files_read += 1,
            FileActivityKind::List => self.dirs_listed += 1,
            FileActivityKind::Search => self.patterns_searched += 1,
            FileActivityKind::Write => self.files_written += 1,
        }
    }
}

/// Illuminate the newest graphemes of an actively streaming assistant line.
fn apply_hot_tail_to_last_line(lines: &mut [Line<'static>], low_motion: bool) {
    if let Some(last) = lines.last_mut() {
        apply_hot_tail_to_line(last, low_motion);
    }
}

pub(crate) fn apply_hot_tail_to_line(line: &mut Line<'static>, low_motion: bool) {
    if line.spans.is_empty() {
        return;
    }
    // Reconstruct plain text from spans, split hot tail, re-style.
    let plain: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    if plain.trim().is_empty() {
        return;
    }
    let (_settled, hot) = crate::tui::hot_tail::split_hot_tail(
        &plain,
        true,
        crate::tui::hot_tail::HOT_TAIL_GRAPHEMES,
    );
    if hot.is_empty() {
        return;
    }
    let hot_start = plain.len().saturating_sub(hot.len());
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let base_fg = palette::TEXT_PRIMARY;
    let hot_style = crate::tui::hot_tail::hot_tail_style(base_fg, elapsed, low_motion);

    // Walk spans and re-style the trailing hot graphemes.
    let mut cursor = 0usize;
    let mut new_spans = Vec::with_capacity(line.spans.len() + 2);
    for span in line.spans.drain(..) {
        let content = span.content.to_string();
        let len = content.len();
        let span_end = cursor + len;
        if span_end <= hot_start {
            new_spans.push(span);
        } else if cursor >= hot_start {
            new_spans.push(Span::styled(content, hot_style));
        } else {
            // Split this span across the boundary.
            let local = hot_start - cursor;
            let (left, right) = content.split_at(local.min(content.len()));
            if !left.is_empty() {
                new_spans.push(Span::styled(left.to_string(), span.style));
            }
            if !right.is_empty() {
                new_spans.push(Span::styled(right.to_string(), hot_style));
            }
        }
        cursor = span_end;
    }
    line.spans = new_spans;
}

#[cfg(test)]
mod tests;
