use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Widget, Wrap},
};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::fmt;
use unicode_width::UnicodeWidthStr;

use crate::config::{ApprovalPolicyControl, Config, ProviderKind};
use crate::features::{FEATURES, Stage};
use crate::settings::Settings;
use crate::tools::UserInputResponse;
use crate::tools::subagent::{
    FleetRole, SubAgentAssignment, SubAgentResult, SubAgentStatus, localized_whale_display_names,
};
use crate::tui::app::App;
use crate::tui::approval::{ElevationOption, ReviewDecision};
use crate::tui::focus_texture::FocusTextureMode;
use crate::tui::history::{HistoryCell, SubAgentCell, summarize_tool_output};
use crate::tui::menu_style;
use crate::tui::tideline::{SettingApplySemantics, SettingAuthority, SettingFact, UiSnapshot};
use crate::tui::widgets::agent_card::AgentLifecycle;
use codewhale_localization::{
    Locale, MessageId, configured_locale_is_partial_pack, normalize_configured_locale, tr, tr_key,
};
use codewhale_palette as palette;

pub mod automations;
pub mod extensions;
pub mod fleet_detail;
pub mod fleet_list;
pub mod fleet_roster;
pub mod fleet_setup;
pub mod mode_picker;
pub mod route_save_prompt;
pub(crate) mod router_setup;
pub mod skills_manager;
pub mod status_picker;
pub mod workflows_manager;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalKind {
    PetHabitat,
    Approval,
    Elevation,
    UserInput,
    CommandPalette,
    Help,
    SubAgents,
    Pager,
    LiveTranscript,
    SessionPicker,
    Config,
    ModelPicker,
    ProviderPicker,
    ModePicker,
    FleetRoster,
    FleetSetup,
    FleetList,
    FleetDetail,
    HotbarSetup,
    SetupWizard,
    FilePicker,
    StatusPicker,
    FeedbackPicker,
    ThemePicker,
    ContextMenu,
    ContextInspector,
    SkillsManager,
    /// Unified, read-only extensions inventory. Mutations delegate to the
    /// existing Hooks / Plugins / Skills / MCP command controllers.
    Extensions,
    /// Native git worktree manager (list / create / switch / compare).
    WorktreeManager,
    /// Live workflow **run** dashboard (`/workflows`): active and retained
    /// runs from the journal, with host-side cancel.
    WorkflowsManager,
    /// The scheduled-automation room (`/automation`): every automation the
    /// person owns, with pause / resume / run / cancel / delete.
    Automations,
    /// "Resume this session?" over the launch card. Resuming replaces the
    /// whole session context, so it asks first.
    LaunchResumeConfirm,
    /// Router setup (`/router`, `/model router`): presets for `[auto.router]`.
    RouterSetup,
}

/// Clear and paint a modal popup with an opaque surface.
///
/// Older modals often called `Clear` only, which left reset-background blank
/// cells that could read as translucent on terminals with a non-default app
/// background. This helper makes the popup area explicit and keeps the small
/// shadow from inheriting stale transcript glyphs.
pub(crate) fn render_modal_surface(area: Rect, popup_area: Rect, buf: &mut Buffer) {
    let shadow_x = popup_area.x.saturating_add(1);
    let shadow_y = popup_area.y.saturating_add(1);
    let shadow_right = area.x.saturating_add(area.width);
    let shadow_bottom = area.y.saturating_add(area.height);
    let shadow_width = popup_area.width.min(shadow_right.saturating_sub(shadow_x));
    let shadow_height = popup_area
        .height
        .min(shadow_bottom.saturating_sub(shadow_y));

    if shadow_width > 0 && shadow_height > 0 {
        Block::default()
            .style(Style::default().bg(palette::SURFACE_ELEVATED))
            .render(
                Rect {
                    x: shadow_x,
                    y: shadow_y,
                    width: shadow_width,
                    height: shadow_height,
                },
                buf,
            );
    }

    Clear.render(popup_area, buf);
    Block::default()
        .style(Style::default().bg(palette::WHALE_BG))
        .render(popup_area, buf);
}

/// Paint a full-screen underwater instrument surface and return its body.
///
/// Secondary rooms use one title hairline and one bottom action rail instead
/// of a centered generic card. A one-cell outer margin is retained when the
/// terminal can afford it; compact panes use every cell.
pub(crate) fn render_underwater_surface(
    area: Rect,
    buf: &mut Buffer,
    title: impl Into<String>,
) -> Rect {
    let margin_x = u16::from(area.width >= 44);
    let margin_y = u16::from(area.height >= 24);
    let surface = Rect {
        x: area.x.saturating_add(margin_x),
        y: area.y.saturating_add(margin_y),
        width: area.width.saturating_sub(margin_x.saturating_mul(2)),
        height: area.height.saturating_sub(margin_y.saturating_mul(2)),
    };
    Clear.render(area, buf);
    Block::default()
        .style(Style::default().bg(palette::WHALE_BG))
        .render(area, buf);
    // Ratatui clips long block titles at the border edge without signalling
    // that anything is missing. Reserve the corner cells and semantic-ellipsis
    // the title so compact terminals still read as intentional instruments.
    let title_width = usize::from(surface.width.saturating_sub(4));
    let title = crate::tui::ui_text::semantic_truncate(&title.into(), title_width);
    let block = Block::default()
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(palette::WHALE_ACTION)
                .add_modifier(Modifier::BOLD),
        )))
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(palette::BORDER_COLOR))
        .style(Style::default().bg(palette::WHALE_BG))
        .padding(Padding::new(1, 1, u16::from(area.height >= 24), 0));
    let inner = block.inner(surface);
    block.render(surface, buf);
    inner
}

/// Render wrapped detail text scrolled by *rendered* rows and return the
/// largest useful scroll. Clamping by logical lines before wrapping left the
/// wrapped tail unreachable and made each step jump a whole wrapped line.
pub(crate) fn render_wrapped_detail(
    lines: Vec<Line<'static>>,
    area: Rect,
    buf: &mut Buffer,
    requested_scroll: usize,
) -> usize {
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let max_scroll = paragraph
        .line_count(area.width.max(1))
        .saturating_sub(usize::from(area.height).max(1));
    let scroll = u16::try_from(requested_scroll.min(max_scroll)).unwrap_or(u16::MAX);
    paragraph.scroll((scroll, 0)).render(area, buf);
    max_scroll
}

/// Paint a scrollbar on the exact right edge of the panel it controls and
/// return the content rect with that rail reserved. Nothing is drawn when all
/// rows fit, so narrow surfaces do not spend a column on a fictional control.
pub(crate) fn render_panel_scroll_rail(
    area: Rect,
    buf: &mut Buffer,
    total_rows: usize,
    offset: usize,
    visible_rows: usize,
    focused: bool,
) -> Rect {
    if area.width < 2 || area.height == 0 || total_rows <= visible_rows.max(1) {
        return area;
    }
    let rail_x = area.right().saturating_sub(1);
    let rail_height = usize::from(area.height);
    let visible = visible_rows.max(1).min(total_rows);
    let thumb_height = ((rail_height * visible).div_ceil(total_rows)).clamp(1, rail_height);
    let max_offset = total_rows.saturating_sub(visible);
    let travel = rail_height.saturating_sub(thumb_height);
    let thumb_top = travel
        .saturating_mul(offset.min(max_offset))
        .checked_div(max_offset)
        .unwrap_or(0);
    let thumb_color = if focused {
        palette::TEXT_MUTED
    } else {
        palette::TEXT_DIM
    };
    for local_y in 0..area.height {
        let y = area.y.saturating_add(local_y);
        let local = usize::from(local_y);
        let is_thumb = local >= thumb_top && local < thumb_top + thumb_height;
        buf[(rail_x, y)]
            .set_symbol(if is_thumb { "█" } else { "│" })
            .set_style(Style::default().fg(if is_thumb {
                thumb_color
            } else {
                palette::BORDER_COLOR
            }));
    }
    Rect {
        width: area.width.saturating_sub(1),
        ..area
    }
}

fn render_modal_backdrop(area: Rect, buf: &mut Buffer) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)]
                .set_symbol(" ")
                .set_style(Style::default().bg(palette::WHALE_BG));
        }
    }
}

/// Compute a centered, responsive popup rect for a modal.
///
/// The size starts from `preferred_*`, but is clamped so it never exceeds the
/// frame (leaving a small breathing-room margin when there is space) and never
/// drops below `min_*` unless the frame itself is smaller. Centering the result
/// inside `area` replaces the repeated, error-prone
/// `N.min(area.width.saturating_sub(..))` arithmetic scattered across modals so
/// every overlay sizes itself the same way at 80x24, 100x30, 120x32, 160x40,
/// and beyond. See #3732.
pub(crate) fn centered_modal_area(
    area: Rect,
    preferred_width: u16,
    preferred_height: u16,
    min_width: u16,
    min_height: u16,
) -> Rect {
    // Keep a 2-cell margin on each axis when the frame can spare it so the
    // backdrop stays visible around the card; otherwise fill the frame.
    let avail_width = area.width.saturating_sub(2).max(1);
    let avail_height = area.height.saturating_sub(2).max(1);
    let width = preferred_width.clamp(min_width.min(avail_width), avail_width);
    let height = preferred_height.clamp(min_height.min(avail_height), avail_height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

/// A single key/label hint shown in a modal's action footer.
///
/// Footers built from `ActionHint`s are laid out by [`action_footer_lines`],
/// which wraps to additional rows instead of letting an action run off the
/// right edge of the modal — the core overflow bug behind #3732. Use this for
/// action/navigation hints; truncate only identifiers/paths/hashes elsewhere.
pub(crate) struct ActionHint {
    key: Cow<'static, str>,
    label: Cow<'static, str>,
}

impl ActionHint {
    pub(crate) fn new(
        key: impl Into<Cow<'static, str>>,
        label: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
        }
    }

    /// Display columns this hint occupies: ` key ` (key padded by a space on
    /// each side) followed by the label.
    fn width(&self) -> usize {
        UnicodeWidthStr::width(self.key.as_ref()) + 2 + UnicodeWidthStr::width(self.label.as_ref())
    }

    fn spans(&self) -> [Span<'static>; 2] {
        [
            Span::styled(
                format!(" {} ", self.key),
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                self.label.clone().into_owned(),
                Style::default().fg(palette::TEXT_MUTED),
            ),
        ]
    }
}

/// Lay out action hints into one or more lines that each fit within `width`.
///
/// Hints are packed greedily; when the next hint would overflow the current row
/// the layout starts a new row rather than truncating. No action is ever
/// dropped or clipped (a single hint wider than `width` is emitted alone, which
/// only happens at degenerate widths below the modal minimums). This is the
/// shared replacement for the single-line `title_bottom` footers that silently
/// pushed actions off-screen.
pub(crate) fn action_footer_lines(hints: &[ActionHint], width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width);
    if hints.is_empty() || width == 0 {
        return Vec::new();
    }
    const GAP: usize = 1;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut current_width = 0usize;
    for hint in hints {
        let hint_width = hint.width();
        let needed = if current.is_empty() {
            hint_width
        } else {
            current_width + GAP + hint_width
        };
        if !current.is_empty() && needed > width {
            lines.push(Line::from(std::mem::take(&mut current)));
            current_width = 0;
        }
        if !current.is_empty() {
            current.push(Span::raw(" ".repeat(GAP)));
            current_width += GAP;
        }
        current.extend(hint.spans());
        current_width += hint_width;
    }
    if !current.is_empty() {
        lines.push(Line::from(current));
    }
    lines
}

/// Reserve `lines` worth of rows at the bottom of `inner`, paint them, and
/// return the content area that remains above. Shared by the action-hint and
/// free-text modal footers.
fn place_footer_lines(
    inner: Rect,
    buf: &mut Buffer,
    lines: Vec<Line<'static>>,
    quiet_gutter: bool,
) -> Rect {
    if lines.is_empty() || inner.height == 0 {
        return inner;
    }
    let footer_height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .min(inner.height);
    // Opted-in overlays keep one quiet row between scrollable body copy and
    // the action rail. Degenerate heights keep every row for content.
    let gutter_height = u16::from(quiet_gutter && inner.height >= footer_height.saturating_add(4));
    let footer_area = Rect {
        x: inner.x,
        y: inner.y + inner.height - footer_height,
        width: inner.width,
        height: footer_height,
    };
    Paragraph::new(lines).render(footer_area, buf);
    Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner
            .height
            .saturating_sub(footer_height.saturating_add(gutter_height)),
    }
}

/// Render a wrapping action footer anchored to the bottom of `inner` and
/// return the content area that remains above it.
///
/// Modals call this after painting their block so the footer reserves exactly
/// as many rows as it needs (bounded by the available height) and the body
/// fills the rest. Centralizing it keeps every modal's action row visible and
/// reachable at narrow widths.
pub(crate) fn render_modal_footer(inner: Rect, buf: &mut Buffer, hints: &[ActionHint]) -> Rect {
    let lines = action_footer_lines(hints, inner.width);
    place_footer_lines(inner, buf, lines, false)
}

/// Render a modal action footer with one quiet body-to-footer row when the
/// caller's responsive layout has explicitly budgeted for it.
pub(crate) fn render_modal_footer_with_gutter(
    inner: Rect,
    buf: &mut Buffer,
    hints: &[ActionHint],
) -> Rect {
    let lines = action_footer_lines(hints, inner.width);
    place_footer_lines(inner, buf, lines, true)
}

/// Word-wrap a free-form footer string into styled lines that each fit `width`.
///
/// For footers that are pre-composed prose/sentences (e.g. localized config
/// hints) rather than discrete key/label hints. Wrapping on whitespace keeps
/// every word visible instead of clipping the tail at the modal edge.
pub(crate) fn wrapped_footer_lines(text: &str, width: u16, style: Style) -> Vec<Line<'static>> {
    let width = usize::from(width);
    if text.trim().is_empty() || width == 0 {
        return Vec::new();
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for word in text.split_whitespace() {
        let word_width = UnicodeWidthStr::width(word);
        let needed = if current.is_empty() {
            word_width
        } else {
            current_width + 1 + word_width
        };
        if !current.is_empty() && needed > width {
            lines.push(Line::from(Span::styled(
                std::mem::take(&mut current),
                style,
            )));
            current_width = 0;
        }
        if !current.is_empty() {
            current.push(' ');
            current_width += 1;
        }
        current.push_str(word);
        current_width += word_width;
    }
    if !current.is_empty() {
        lines.push(Line::from(Span::styled(current, style)));
    }
    lines
}

/// Render a wrapping free-text footer anchored to the bottom of `inner` and
/// return the content area above it. The prose counterpart to
/// [`render_modal_footer`].
pub(crate) fn render_modal_text_footer(
    inner: Rect,
    buf: &mut Buffer,
    text: &str,
    style: Style,
) -> Rect {
    let lines = wrapped_footer_lines(text, inner.width, style);
    // Free-text status footers are already separated semantically from their
    // table body and can carry the last visible receipt themselves. Do not
    // spend another row here; action-rail layouts can opt into that gutter.
    place_footer_lines(inner, buf, lines, false)
}

/// Shared list/detail geometry for modal managers and pickers.
///
/// Wide modals get a stable left list and a right detail pane. Narrow modals
/// stack the list over the detail so neither side becomes unreadably thin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ListDetailLayout {
    pub(crate) list: Rect,
    pub(crate) detail: Rect,
    pub(crate) stacked: bool,
}

impl ListDetailLayout {
    #[must_use]
    pub(crate) fn split(area: Rect, min_detail_width: u16) -> Self {
        if area.width == 0 || area.height == 0 {
            return Self {
                list: area,
                detail: area,
                stacked: true,
            };
        }

        let gap = 1;
        let min_list_width = 30.min(area.width);
        let can_split = area.width >= 96
            && area
                .width
                .saturating_sub(gap)
                .saturating_sub(min_list_width)
                >= min_detail_width;
        if can_split {
            let max_list_width = area.width.saturating_sub(gap + min_detail_width);
            let preferred = area.width.saturating_mul(42) / 100;
            let list_width = preferred.clamp(min_list_width, max_list_width.min(52));
            let detail_width = area.width.saturating_sub(list_width + gap);
            return Self {
                list: Rect {
                    x: area.x,
                    y: area.y,
                    width: list_width,
                    height: area.height,
                },
                detail: Rect {
                    x: area.x + list_width + gap,
                    y: area.y,
                    width: detail_width,
                    height: area.height,
                },
                stacked: false,
            };
        }

        let gap = if area.height >= 8 { 1 } else { 0 };
        let min_detail_height = 4.min(area.height);
        let max_list_height = area.height.saturating_sub(gap + min_detail_height);
        let preferred = area.height.saturating_mul(3) / 5;
        let list_height = preferred.clamp(1, max_list_height.max(1));
        let detail_height = area.height.saturating_sub(list_height + gap);
        Self {
            list: Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: list_height,
            },
            detail: Rect {
                x: area.x,
                y: area.y + list_height + gap,
                width: area.width,
                height: detail_height,
            },
            stacked: true,
        }
    }
}

/// Plain empty-state copy for modal list/detail bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EmptyState {
    title: Cow<'static, str>,
    body: Cow<'static, str>,
    primary_action: Option<(Cow<'static, str>, Cow<'static, str>)>,
    secondary_action: Option<(Cow<'static, str>, Cow<'static, str>)>,
}

impl EmptyState {
    pub(crate) fn new(
        title: impl Into<Cow<'static, str>>,
        body: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            primary_action: None,
            secondary_action: None,
        }
    }

    #[must_use]
    pub(crate) fn primary_action(
        mut self,
        key: impl Into<Cow<'static, str>>,
        label: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.primary_action = Some((key.into(), label.into()));
        self
    }

    #[must_use]
    pub(crate) fn secondary_action(
        mut self,
        key: impl Into<Cow<'static, str>>,
        label: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.secondary_action = Some((key.into(), label.into()));
        self
    }

    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer) {
        let mut lines = vec![
            Line::from(Span::styled(
                self.title.clone().into_owned(),
                Style::default()
                    .fg(palette::TEXT_PRIMARY)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                self.body.clone().into_owned(),
                Style::default().fg(palette::TEXT_MUTED),
            )),
        ];
        if self.primary_action.is_some() || self.secondary_action.is_some() {
            lines.push(Line::from(""));
        }
        for (key, label) in [self.primary_action.as_ref(), self.secondary_action.as_ref()]
            .into_iter()
            .flatten()
        {
            let hint = ActionHint::new(key.clone(), label.clone());
            lines.push(Line::from(hint.spans().to_vec()));
        }
        Paragraph::new(lines)
            .style(Style::default().fg(palette::TEXT_PRIMARY))
            .wrap(Wrap { trim: true })
            .render(area, buf);
    }
}

#[derive(Debug, Clone)]
pub enum CommandPaletteAction {
    ExecuteCommand { command: String },
    InsertText { text: String },
    OpenTextPager { title: String, content: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextMenuAction {
    CopySelection,
    OpenSelection,
    ClearSelection,
    CopyCell {
        cell_index: usize,
    },
    OpenDetails {
        cell_index: usize,
    },
    Paste,
    OpenCommandPalette,
    OpenContextInspector,
    OpenHelp,
    /// Open a workspace file at a line in the user's editor. The path was
    /// resolved inside the workspace when the menu was built; the entry is
    /// only offered when it resolved.
    OpenFileAtLine {
        path: std::path::PathBuf,
        line: u32,
    },
    /// Hide a transcript cell. Adds the cell's index to `collapsed_cells`.
    HideCell {
        cell_index: usize,
    },
    /// Show a previously hidden cell (when right-clicking near it).
    ShowCell {
        cell_index: usize,
    },
    /// Show all currently hidden cells.
    ShowAllHidden,
    /// Run a work-surface row action — the same typed action a left click or
    /// Enter on that row runs, never a free-form command string.
    Row(crate::tui::app::SidebarRowAction),
    /// Copy a pre-resolved text payload (e.g. a sidebar row's full text)
    /// to the clipboard.
    CopyText {
        text: String,
    },
    /// Act on a row of the open Extensions panel, found again by its id.
    Extension {
        item_id: String,
        verb: ExtensionMenuVerb,
    },
    /// Pin/unpin the host terminal window (normal window ↔ always-on-top
    /// mini window). Windows only; no-op elsewhere.
    ToggleWindowPin,
}

/// What an Extensions row menu entry does to its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionMenuVerb {
    /// The row's own action — what Enter runs.
    Activate,
    /// Show the row's state, description and detail in a pager.
    Details,
    /// The row's reversible on/off switch.
    Toggle,
    /// Remove the row. The menu already asked for confirmation.
    Remove,
}

#[derive(Debug, Clone)]
pub enum ViewEvent {
    CommandPaletteSelected {
        action: CommandPaletteAction,
    },
    OpenTextPager {
        title: String,
        content: String,
    },
    ApprovalDecision {
        tool_id: String,
        tool_name: String,
        decision: ReviewDecision,
        timed_out: bool,
        /// Exact-argument fingerprint, used to scope *denials* (#1617).
        approval_key: String,
        /// Lossy / arity-aware fingerprint, used to scope *approvals*.
        approval_grouping_key: String,
        /// Permission rules to append when the decision approves.
        persistent_rules: Vec<codewhale_config::ToolAskRule>,
    },
    ElevationDecision {
        tool_id: String,
        tool_name: String,
        option: ElevationOption,
    },
    UserInputSubmitted {
        tool_id: String,
        response: UserInputResponse,
    },
    UserInputCancelled {
        tool_id: String,
    },
    ConfigUpdated {
        key: String,
        value: String,
        persist: bool,
    },
    /// The canonical `/theme` picker's selection. Preview, rollback, and
    /// persist travel in one event so the theme never changes by half.
    ThemeSelectionUpdated {
        theme: String,
        persist: bool,
    },
    SubAgentsRefresh,
    SidebarAgentCancel {
        agent_id: String,
    },
    /// An agent row activation (Work strip, sidebar dossier, `/agents`) or
    /// Alt+V from Agent Details, Enter/click on any agent row, and Enter in the
    /// `/agents` register all request the agent's transcript — since v0.9.7's
    /// "one agent, one destination" inversion that is the in-place focus.
    OpenAgentTranscript {
        agent_id: String,
    },
    /// Agent Details was popped with Esc/q/Left. The Work surface uses this
    /// to release only its detail-open owner while retaining selection.
    AgentDetailsClosed {
        agent_id: String,
    },
    /// Emitted by the file picker (`Ctrl+P`) when the user presses Enter on a
    /// candidate. The handler should insert `@<path>` at the composer's cursor
    /// position.
    FilePickerSelected {
        path: String,
    },
    SessionSelected {
        session_id: String,
    },
    SessionRenamed {
        metadata: Box<crate::session_manager::SessionMetadata>,
    },
    /// A session's archive flag was flipped (#2934 / #4397).
    ///
    /// Distinct from `SessionRenamed` so the receipt can say what actually
    /// happened; reusing rename would report "Renamed session …" for an
    /// archive, which is exactly the kind of small lie that erodes trust in
    /// every other receipt.
    SessionArchived {
        metadata: Box<crate::session_manager::SessionMetadata>,
    },
    SessionDeleted {
        session_id: String,
        title: String,
    },
    /// Emitted by the `/model` picker on Enter or Shift+D. Carries both the
    /// chosen model id and reasoning effort tier so the UI handler can update
    /// App state and forward `Op::SetModel` to the running engine.
    /// `save_as_startup_default` is true only for the explicit Shift+D action;
    /// ordinary Enter remains a session-local route change. `previous_*`
    /// fields let the handler skip work when nothing changed and craft a clear
    /// status message.
    ModelPickerApplied {
        model: String,
        identity: Option<crate::config::ProviderIdentity>,
        effort: crate::reasoning_preference::ReasoningEffort,
        previous_model: String,
        previous_effort: crate::reasoning_preference::ReasoningEffort,
        save_as_startup_default: bool,
    },
    /// Emitted by the `/model` picker on Esc so the next open can restore
    /// the browsing context — view mode and highlighted row (#4109 / #4115).
    ModelPickerDismissed {
        /// True when the dismissed view browses beyond configured providers
        /// (Catalog / Recent / Coding / Cheap / Long context).
        catalog_view: bool,
        /// Named view key (`configured`, `catalog`, `recent`, `coding`,
        /// `cheap`, `long_context`) for reopen restore (#4115).
        view: String,
        selected_row_id: Option<String>,
    },
    /// Enter on a locked (unauthenticated) model: explain why selection is
    /// blocked and open the provider auth/setup path when possible.
    /// Re-resolve readiness + rebuild catalog rows for the open model picker.
    ModelPickerRefresh,
    ModelPickerTogglePin {
        provider: crate::config::ProviderKind,
        /// Exact named route for `Custom`; built-in providers leave this unset.
        provider_id: Option<String>,
        model: String,
    },
    ModelPickerMovePin {
        provider: crate::config::ProviderKind,
        /// Exact named route for `Custom`; built-in providers leave this unset.
        provider_id: Option<String>,
        model: String,
        delta: isize,
    },
    /// `⇧F` in the picker: add the row's exact route to the team (the
    /// selected saved team), or remove it when it is already there (design §10 F1).
    ModelPickerToggleFleet {
        provider: crate::config::ProviderKind,
        /// Exact named route for `Custom`; built-in providers leave this unset.
        provider_id: Option<String>,
        model: String,
    },
    /// Enter on a Fleet editor row: open the standard `/model` picker for
    /// that row (the editor stays underneath) instead of the editor's own
    /// inline route list.
    FleetProfileRoutePickRequested {
        editor_id: uuid::Uuid,
    },
    FleetProfileRoutePicked {
        editor_id: uuid::Uuid,
        provider: crate::config::ProviderKind,
        provider_id: Option<String>,
        model: String,
        reasoning: Option<crate::reasoning_preference::ReasoningEffort>,
    },
    FleetProfileRouteCommitRequested {
        editor_id: uuid::Uuid,
    },
    FleetAssignmentPickerDismissed {
        editor_id: uuid::Uuid,
    },
    FleetRosterOpenCoordinatorRequested,
    FleetDetailRoutePickRequested {
        target: crate::tui::views::fleet_detail::FleetRouteTarget,
        editor_id: uuid::Uuid,
    },
    /// The `/model` picker, opened for a Fleet editor row, resolved a route.
    /// Carries the row's absolute route — never a diff against the session —
    /// and the host applies and saves it on the editor still on the stack.
    /// The picker's `auto` row means "inherit the session route".
    FleetRoutePicked {
        target: crate::tui::views::fleet_detail::FleetRouteTarget,
        editor_id: uuid::Uuid,
        provider: crate::config::ProviderKind,
        /// Exact named route for `Custom`; built-in providers leave this unset.
        provider_id: Option<String>,
        model: String,
        reasoning: Option<crate::reasoning_preference::ReasoningEffort>,
    },
    ModelPickerNeedsAuth {
        identity: crate::config::ProviderIdentity,
        model: String,
        reason: String,
    },
    /// Transient status toast from a modal (e.g. locked-model explanation).
    StatusMessage {
        message: String,
    },
    /// The Tideline topbar's route segment requested the normal `/provider`
    /// surface. It carries no catalog, readiness, or selected-route payload:
    /// those facts remain owned by the provider picker and its apply path.
    TopbarRoutePickerRequested,
    /// The info line's model field requested the normal `/model` surface.
    /// Same rule as the route segment: an entry point carrying no catalog.
    TopbarModelPickerRequested,
    /// Emitted by the `/provider` picker on Esc so the next open can restore
    /// the browsing context — view mode and highlighted row.
    ProviderPickerDismissed {
        catalog_view: bool,
        selected_provider_id: Option<String>,
    },
    /// Emitted by the `/provider` picker when the user selects a provider
    /// that already has credentials — the handler should perform the same
    /// switch as `AppAction::SwitchProvider`.
    ProviderPickerApplied {
        identity: crate::config::ProviderIdentity,
    },
    /// Emitted by the `/provider` picker after the user types an API key
    /// inline for a provider that lacked one. The handler validates the key
    /// live; on success it reopens the guided flow at the model-pick stage
    /// without persisting yet (#3875).
    ProviderPickerApiKeySubmitted {
        identity: crate::config::ProviderIdentity,
        api_key: String,
        /// Endpoint chosen in the wizard's billing-route stage, applied to the
        /// verification config only — nothing is written until confirm (#4526).
        base_url: Option<String>,
    },
    /// Emitted by the `/provider` guided setup confirm stage after the user
    /// accepted provider + model. The handler persists the key (and model)
    /// via the comment-preserving config path, then performs the switch.
    ProviderPickerSetupConfirmed {
        identity: crate::config::ProviderIdentity,
        api_key: String,
        model: String,
        context_window: Option<u32>,
        /// Endpoint the key was verified against, persisted to the provider's
        /// own `base_url` before the key is saved (#4526).
        base_url: Option<String>,
    },
    /// Emitted by the `/provider` picker after the custom provider form is
    /// completed. The handler persists a named OpenAI-compatible provider
    /// table and switches to it without storing raw secrets.
    ProviderPickerCustomProviderSubmitted {
        provider_id: String,
        base_url: String,
        model: Option<String>,
        api_key_env: Option<String>,
    },
    /// Emitted by provider/setup UI when xAI device-code OAuth is requested.
    ProviderPickerXaiOAuthRequested,
    ProviderPickerClaudeOAuthRequested,
    /// Emitted by provider/setup UI when native ChatGPT PKCE sign-in is requested.
    ProviderPickerChatgptOAuthRequested,
    /// Emitted by provider/setup UI when OrcaRouter OAuth 2.0 + PKCE sign-in is
    /// requested. The picker only emits this after the user chose "Connect with
    /// OrcaRouter" from the two-option OrcaRouter auth screen.
    ProviderPickerPluginOAuthRequested {
        provider: String,
    },
    ProviderPickerOrcarouterOAuthRequested,
    /// Emitted only after the picker showed owner, exact path, and the full
    /// read-only side-effect contract and the user explicitly confirmed it.
    ProviderPickerExternalConsentConfirmed {
        provider: crate::config::ProviderKind,
        consent_provider: codewhale_config::ProviderKind,
        source: codewhale_config::ExternalCredentialSource,
        path: std::path::PathBuf,
    },
    /// One-step revocation from a provider row that currently has consent.
    ProviderPickerExternalConsentRevoked {
        provider: crate::config::ProviderKind,
    },
    /// Emitted by the `/provider` picker (the `M` action) to jump straight to
    /// the `/model` picker pre-filtered to the highlighted provider (#3083).
    ProviderPickerOpenModels {
        identity: crate::config::ProviderIdentity,
    },
    /// Emitted by `/provider` `T`: probe `/models` and refresh readiness
    /// without treating a 2xx as model-ready (#5350).
    ProviderPickerTestConnection {
        identity: crate::config::ProviderIdentity,
        /// Restore Catalog vs Configured after the probe. Must not force
        /// the all-providers catalog if the user was on configured-only.
        catalog_view: bool,
    },
    /// Emitted by the `/mode` picker when the user chooses a mode.
    ModeSelected {
        mode: codewhale_config::AppMode,
    },
    /// Emitted by the `/statusline` picker every time the user toggles an
    /// item (live preview) and once more on Enter (final). The handler
    /// updates `app.status_items` immediately and persists on `final_save`
    /// so the footer animates without a write per keystroke.
    StatusItemsUpdated {
        items: Vec<crate::config::StatusItem>,
        final_save: bool,
    },
    /// Emitted by the `/hotbar` setup wizard when the user saves the draft
    /// bindings. The host updates live config state; disk persistence is
    /// handled by the follow-up persistence slice.
    HotbarSetupSaved {
        bindings: Vec<codewhale_config::HotbarBindingToml>,
    },
    /// Emitted by the constitution-first setup shell when a staged setup-state
    /// record should be committed atomically to `$CODEWHALE_HOME/setup_state.json`.
    SetupStateCommitRequested {
        state: codewhale_config::SetupState,
        message: String,
    },
    /// Emitted by the constitution-first setup shell when accepting a guided
    /// structured user-global constitution. The host commits the constitution
    /// and matching setup-state record together.
    SetupConstitutionCommitRequested {
        constitution: codewhale_config::UserConstitution,
        state: codewhale_config::SetupState,
        message: String,
    },
    /// Emitted by the setup Constitution card (`A`, provider route ready) to
    /// ask the user's first configured model to draft the constitution from
    /// the guided answers plus an optional bounded own-words note. The host
    /// performs the one-shot call, pushes the sanitized/bounded draft back into the wizard, and opens the
    /// ratification preview; on any failure it reports why and leaves the
    /// deterministic guided draft standing. Nothing is persisted by this
    /// event — saving still goes through the ratify keypress and
    /// [`SetupConstitutionCommitRequested`](Self::SetupConstitutionCommitRequested).
    SetupConstitutionModelDraftRequested {
        draft: crate::tui::setup::GuidedConstitutionDraft,
        freeform_note: Option<String>,
        locale: codewhale_localization::Locale,
    },
    /// Emitted by the fleet setup Review step (`m`) to ask the configured
    /// model to draft the agent profile the wizard describes. The host
    /// performs the one-shot call, pushes the sanitized/bounded draft back
    /// into the wizard, and opens the rendered-TOML preview; on failure it
    /// reports why and the manual authoring flow stands. Nothing is
    /// persisted by this event.
    FleetProfileModelDraftRequested {
        role: String,
        /// Target model for the worker: a concrete model id, or "inherit".
        model: String,
        /// Canonical provider id for a concrete cross-provider route pick, or
        /// `None` for `inherit` (#4093). Carried so the model-drafted profile
        /// keeps the picked provider instead of collapsing to an ambiguous,
        /// provider-scoped profile — the exact bug #4093 fixes.
        provider: Option<String>,
        /// Canonical reasoning tier selected by the wizard, or `None` for
        /// inherit (#4137). Carried with the async draft for the same reason
        /// as `provider`: the ratified profile must preserve the operator's
        /// explicit choice, not whatever the model echoed.
        reasoning_effort: Option<String>,
        locale: codewhale_localization::Locale,
    },
    /// Emitted by the `/fleet` roster view (`s` / Enter) to edit a member.
    /// The host routes a selected v2 Fleet to its exact editor and uses the
    /// legacy profile wizard only when no named Fleet is selected.
    FleetRosterOpenSetupRequested {
        /// Exact Fleet member id; roles are not unique and therefore cannot
        /// identify which row the operator selected.
        member_id: String,
    },
    /// Open the live workers tab from the unified Fleet surface.
    FleetRosterOpenWorkersRequested,

    /// The roster asks the host to open the secondary named-Fleet switcher
    /// (`/fleet fleets`; `/fleet fleets` remains compatible). Editing stays on
    /// setup; this is pick/select only.
    FleetRosterOpenFleetsRequested,

    /// The Fleet list view asks the host to open a saved Fleet's detail view.
    FleetListOpenDetailRequested {
        name: String,
        scope: crate::fleet::store::FleetScope,
    },
    /// A Fleet store mutation happened (select/save/delete/rename/copy).
    /// The message is the exact receipt; the host refreshes roster state.
    FleetStoreChanged {
        message: String,
    },
    /// Emitted by the fleet setup Review step after the user previewed a
    /// model-drafted profile and pressed the explicit ratify key. The host
    /// renders TOML deterministically from the validated draft and persists it
    /// atomically in the explicitly selected project or personal scope.
    FleetProfileDraftCommitRequested {
        draft: Box<crate::fleet::profile::FleetProfileDraft>,
        scope: crate::fleet::profile::FleetProfileScope,
    },
    /// Emitted by the Fleet setup Model step when the user selects a route that
    /// has structurally valid external-consent credentials but is not the
    /// active session provider. The host performs a route-scoped validation
    /// (minting the read capability only for this exact provider/source/path)
    /// and records the result in the session health snapshot so the same row
    /// becomes selectable on the next render. The parent session provider and
    /// model are never changed.
    FleetSetupExternalConsentActivationRequested {
        provider_id: String,
        model: String,
    },
    /// Emitted by the setup Runtime Posture card after the user has previewed
    /// and confirmed an explicit preset/config diff.
    SetupRuntimePresetApplyRequested {
        preset: crate::tui::setup::SetupRuntimePreset,
        state: codewhale_config::SetupState,
        message: String,
    },
    /// Emitted by the setup Provider/Model readiness card to hand off to the
    /// existing provider manager instead of duplicating provider auth UI.
    SetupOpenProviderRequested,
    /// Emitted by the setup Provider/Model readiness card to hand off to the
    /// existing provider-qualified model route picker.
    SetupOpenModelRequested,
    /// Emitted by the setup Operate/Fleet readiness card to hand off to the
    /// existing Fleet setup wizard without writing Fleet config itself.
    SetupOpenFleetRequested,
    /// Emitted by the setup Hotbar card to hand off to the existing Hotbar
    /// setup wizard without rewriting bindings itself.
    SetupOpenHotbarRequested,
    /// Emitted by the setup Runtime Posture card to hand off to the existing
    /// work-mode picker.
    SetupOpenModeRequested,
    /// Emitted by the setup Runtime Posture card to hand off to the existing
    /// config view for approval/sandbox/network details.
    SetupOpenConfigRequested,
    /// Emitted by the progressive setup guide to start the same account-owned
    /// web remote-control flow as `/rc`. Setup never duplicates enrollment.
    SetupOpenRemoteControlRequested,
    /// Emitted by the `/hotbar` setup wizard when the user chooses "Disable
    /// Hotbar". The host persists `hotbar = []` and hides the panel.
    HotbarDisableRequested,
    /// Emitted by the live-transcript overlay while in backtrack preview
    /// mode (#133) when the user steps the highlighted user message with
    /// Left or Right. The handler advances `app.backtrack`, refreshes the
    /// overlay's `selected_idx`, and pins scroll near the new highlight.
    BacktrackStep {
        direction: crate::tui::backtrack::Direction,
    },
    /// Emitted by the live-transcript overlay when the user presses Enter
    /// in backtrack preview mode (#133). The handler calls
    /// `app.backtrack.confirm()`, trims `app.history`/`api_messages` to
    /// the selected user message, populates the composer with the
    /// dropped user text, and closes the overlay.
    BacktrackConfirm,
    /// Emitted by the live-transcript overlay when the user presses Esc
    /// in backtrack preview mode (#133). The handler resets
    /// `app.backtrack` and closes the overlay without trimming.
    BacktrackCancel,
    ContextMenuSelected {
        action: ContextMenuAction,
    },
    /// A modal view asks for a context menu over itself (the Extensions
    /// panel's row menu). The host pushes it on top of the view.
    OpenContextMenu {
        title: String,
        entries: Vec<crate::tui::context_menu::ContextMenuEntry>,
        column: u16,
        row: u16,
    },
    /// Emitted by the pager (`c` / `y`) to copy its body to the system
    /// clipboard. The host handler writes via `app.clipboard` and surfaces a
    /// status message — modal views cannot reach `app` directly. `label` is
    /// the noun shown in the success / failure status (e.g. "Pager content").
    CopyToClipboard {
        text: String,
        label: String,
    },
    /// Emitted by the skills manager when the user confirms an install /
    /// import / update / remove / trust action. The host runs the mutation
    /// controller and rebuilds the open manager view.
    SkillMutationRequested {
        request: crate::skills::mutation::SkillMutationRequest,
    },
    /// Toggle owned-only vs compatible audit scan inside the skills manager.
    SkillsManagerToggleCompatible,
    /// The launch card's resume confirmation was accepted. The host resumes
    /// the named session through the same path the card's own Enter uses.
    LaunchResumeConfirmed {
        session_id: String,
    },
    /// A slash command an Extensions row activated in place: the panel stays
    /// open, the host runs the command through the normal command path, then
    /// hands the panel a fresh snapshot so every row re-reads live state.
    /// When `pager_title` is set, the command's text output renders in a
    /// pager stacked on the panel rather than landing in the transcript.
    ExecutePanelCommand {
        command: String,
        pager_title: Option<String>,
    },
    /// The open Extensions panel's bounded poll: the host rebuilds the read
    /// model only when the MCP snapshot generation or the initializing flag
    /// moved past what the panel's snapshot last saw.
    RefreshExtensions {
        mcp_generation: u64,
        mcp_initializing: bool,
    },
}

#[derive(Debug, Clone)]
pub enum ViewAction {
    None,
    /// The view's own state changed with no event to report (a background
    /// load landed): the host must repaint, nothing else.
    Redraw,
    Close,
    Emit(ViewEvent),
    EmitAndClose(ViewEvent),
}

pub trait ModalView: std::any::Any {
    fn kind(&self) -> ModalKind;
    fn handle_key(&mut self, key: KeyEvent) -> ViewAction;
    /// Returns `true` if the modal consumed the paste; `false` to let the
    /// host route the text elsewhere (e.g. drop it because a modal is open,
    /// or insert it into the composer when no modal wants it). The default
    /// is `false` so modals that don't care about paste don't silently
    /// swallow Cmd-V.
    fn handle_paste(&mut self, _text: &str) -> bool {
        false
    }

    fn handle_mouse(&mut self, _mouse: MouseEvent) -> ViewAction {
        ViewAction::None
    }
    fn render(&self, area: Rect, buf: &mut Buffer);
    /// The region this modal actually paints within the full frame `area`.
    ///
    /// Defaults to the whole frame, which is the legacy full-screen overlay
    /// behaviour every picker/menu still relies on. Inline modals (the
    /// approval prompt) override this to return a bottom-anchored band so the
    /// backdrop only dims their strip and the transcript above stays visible.
    /// The returned rect MUST match the region the modal renders into, or the
    /// dim and the painted content will disagree.
    fn occupied_region(&self, area: Rect) -> Rect {
        area
    }
    fn update_subagents(&mut self, _agents: &[SubAgentResult]) -> bool {
        false
    }
    fn tick(&mut self) -> ViewAction {
        ViewAction::None
    }
    /// Erased downcast hook for views that need a typed reference back from
    /// the boxed trait object (e.g. the live transcript overlay needs `&mut`
    /// access from outside the trait so it can refresh its snapshot of the
    /// app's transcript state right before render).
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;

    /// The approval tool id this view decides, when this view is an approval
    /// card. Enables identity-aware dismissal: a remote decision must close
    /// its own card, not whichever approval happens to be on top.
    fn approval_request_id(&self) -> Option<&str> {
        None
    }

    /// The tool decision this card waits on, including an elevation retry.
    /// Retirement shares identity without granting initial approval authority.
    fn tool_decision_request_id(&self) -> Option<&str> {
        self.approval_request_id()
    }

    /// The human-question tool id, kept separate from approval authority.
    fn user_input_request_id(&self) -> Option<&str> {
        None
    }
}

#[derive(Default)]
pub struct ViewStack {
    views: Vec<Box<dyn ModalView>>,
    /// Focus-context texture prototype mode (#4823). `Off` by default, which
    /// keeps the render output byte-identical to the pre-prototype path.
    focus_texture: FocusTextureMode,
    /// Theme snapshot for the texture pass, set alongside the mode each
    /// frame. `None` (e.g. tests that never opt in) disables the texture.
    focus_texture_theme: Option<codewhale_palette::UiTheme>,
    /// When the view now on top became the top view — pushed, or revealed
    /// by closing or removing the views above it. A key observed before
    /// this instant was typed at something else and must never answer an
    /// approval or elevation card that only then became visible (approvals M2).
    top_since: Option<std::time::Instant>,
}

/// What one [`ViewStack::tick`] produced: events to handle, and whether the
/// frame must be repainted.
#[derive(Debug, Default)]
pub struct ViewTick {
    pub events: Vec<ViewEvent>,
    pub redraw: bool,
}

impl ViewStack {
    pub fn new() -> Self {
        Self {
            views: Vec::new(),
            focus_texture: FocusTextureMode::Off,
            focus_texture_theme: None,
            top_since: None,
        }
    }

    /// Identity of the top view, for noticing when a different view becomes
    /// the top one.
    fn top_identity(&self) -> Option<*const ()> {
        self.views
            .last()
            .map(|view| std::ptr::from_ref::<dyn ModalView>(view.as_ref()).cast::<()>())
    }

    /// Restamp `top_since` when the top view changed since `before`.
    fn note_top_change(&mut self, before: Option<*const ()>) {
        if self.top_identity() != before {
            self.top_since = Some(std::time::Instant::now());
        }
    }

    /// Set the focus-context texture mode and theme for subsequent renders
    /// (#4823 prototype). Called once per frame from the UI render path with
    /// the parsed setting; a plain enum/theme copy, no allocation.
    pub fn set_focus_texture(&mut self, mode: FocusTextureMode, theme: codewhale_palette::UiTheme) {
        self.focus_texture = mode;
        self.focus_texture_theme = Some(theme);
    }

    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }

    pub fn top_kind(&self) -> Option<ModalKind> {
        self.views.last().map(|view| view.kind())
    }

    /// Remove the approval card deciding exactly `gate` at any depth, not
    /// only the top: a decision made elsewhere (web, phone) must retire its
    /// card even when another view sits above it. Identity-aware: it never
    /// closes an unrelated approval card.
    pub fn remove_approval_for_gate(&mut self, gate: &str) -> bool {
        let before = self.views.len();
        let top = self.top_identity();
        self.views
            .retain(|view| !crate::remote_control::view_is_approval_for_gate(view.as_ref(), gate));
        self.note_top_change(top);
        self.views.len() != before
    }

    /// Remove the initial approval or elevation retry for `id` at any depth.
    pub fn remove_tool_decision_by_id(&mut self, id: &str) -> bool {
        let before = self.views.len();
        let top = self.top_identity();
        self.views
            .retain(|view| view.tool_decision_request_id() != Some(id));
        self.note_top_change(top);
        self.views.len() != before
    }

    /// Retire one settled question at any depth without closing other views.
    pub fn remove_user_input_by_id(&mut self, id: &str) -> bool {
        let before = self.views.len();
        let top = self.top_identity();
        self.views
            .retain(|view| view.user_input_request_id() != Some(id));
        self.note_top_change(top);
        self.views.len() != before
    }

    /// Whether an initial approval card for `id` is anywhere in the stack.
    #[cfg(test)]
    pub fn contains_approval_id(&self, id: &str) -> bool {
        self.views
            .iter()
            .any(|view| view.approval_request_id() == Some(id))
    }

    pub fn contains_tool_decision_id(&self, id: &str) -> bool {
        self.views
            .iter()
            .any(|view| view.tool_decision_request_id() == Some(id))
    }

    pub fn tool_decision_request_ids(&self) -> Vec<String> {
        self.views
            .iter()
            .filter_map(|view| view.tool_decision_request_id().map(str::to_owned))
            .collect()
    }

    /// The initial approval id of the top view, kept distinct in authority tests.
    #[cfg(test)]
    pub fn top_approval_id(&self) -> Option<&str> {
        self.views
            .last()
            .and_then(|view| view.approval_request_id())
    }

    /// The decision currently shown, including an elevation retry.
    pub fn top_tool_decision_id(&self) -> Option<&str> {
        self.views
            .last()
            .and_then(|view| view.tool_decision_request_id())
    }

    /// Whether a key observed at `observed_at` predates the moment the
    /// approval or elevation card on top became visible — raised, or revealed
    /// by closing the card above it — i.e. it was typed ahead and must not
    /// answer that card. Two quick answers cannot confirm the card beneath.
    pub fn key_predates_top_approval(&self, observed_at: std::time::Instant) -> bool {
        matches!(
            self.top_kind(),
            Some(ModalKind::Approval | ModalKind::Elevation)
        ) && self
            .top_since
            .is_some_and(|top_since| observed_at < top_since)
    }

    pub fn contains_kind(&self, kind: ModalKind) -> bool {
        self.views.iter().any(|view| view.kind() == kind)
    }

    /// Close the named view and any child modal opened above it. This keeps a
    /// shell-global toggle from stacking a duplicate parent behind its picker.
    pub fn pop_through_kind(&mut self, kind: ModalKind) -> bool {
        while let Some(view) = self.pop() {
            if view.kind() == kind {
                return true;
            }
        }
        false
    }

    pub fn top_occupied_region(&self, area: Rect) -> Option<Rect> {
        self.views.last().map(|view| view.occupied_region(area))
    }

    pub fn push<V: ModalView + 'static>(&mut self, view: V) {
        let kind = view.kind();
        let top = self.top_identity();
        self.views.push(Box::new(view));
        self.note_top_change(top);
        tracing::debug!(target: "codewhale_tui::view_stack", action = "push", kind = ?kind, depth = self.views.len(), "view pushed");
    }

    /// Push an already-boxed view back onto the stack. Used by call sites
    /// that pop a view, mutate it externally, and need to restore it without
    /// the generic `push` re-boxing dance.
    pub fn push_boxed(&mut self, view: Box<dyn ModalView>) {
        let kind = view.kind();
        let top = self.top_identity();
        self.views.push(view);
        self.note_top_change(top);
        tracing::debug!(target: "codewhale_tui::view_stack", action = "push_boxed", kind = ?kind, depth = self.views.len(), "view pushed");
    }

    pub fn pop(&mut self) -> Option<Box<dyn ModalView>> {
        let top = self.top_identity();
        let popped = self.views.pop();
        self.note_top_change(top);
        if let Some(view) = popped.as_ref() {
            tracing::debug!(target: "codewhale_tui::view_stack", action = "pop", kind = ?view.kind(), depth = self.views.len(), "view popped");
        }
        popped
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        // Focus-context texture prototype (#4823): runs over the already
        // rendered background BEFORE any backdrop or view paint, so the
        // focused modal is painted afterwards at full strength and the
        // texture can never overwrite it. `Off` (the default) leaves the
        // buffer untouched, keeping output byte-identical to the
        // pre-prototype path.
        if self.focus_texture != FocusTextureMode::Off
            && let (Some(focus), Some(theme)) =
                (self.top_occupied_region(area), self.focus_texture_theme)
        {
            crate::tui::focus_texture::apply_focus_texture(
                area,
                buf,
                focus,
                &theme,
                self.focus_texture,
                crate::tui::color_compat::ascii_safe_enabled(),
            );
        }
        // Dim each view's own occupied region rather than the whole frame, so
        // an inline modal (the approval prompt) leaves the transcript above it
        // visible instead of blacking out the screen. Full-screen modals keep
        // the default `occupied_region` of the entire frame, so their backdrop
        // is unchanged.
        for view in &self.views {
            let region = view.occupied_region(area);
            crate::tui::osc8::overlay_frame_links(region, Vec::new());
            render_modal_backdrop(region, buf);
            view.render(area, buf);
        }
    }

    pub fn update_subagents(&mut self, agents: &[SubAgentResult]) -> bool {
        let mut updated = false;
        for view in &mut self.views {
            updated |= view.update_subagents(agents);
        }
        updated
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<ViewEvent> {
        let action = self
            .views
            .last_mut()
            .map(|view| view.handle_key(key))
            .unwrap_or(ViewAction::None);
        self.apply_action(action)
    }

    pub fn handle_paste(&mut self, text: &str) -> bool {
        self.views
            .last_mut()
            .map(|view| view.handle_paste(text))
            .unwrap_or(false)
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> Vec<ViewEvent> {
        let action = self
            .views
            .last_mut()
            .map(|view| view.handle_mouse(mouse))
            .unwrap_or(ViewAction::None);
        self.apply_action(action)
    }

    /// Advance the top view's timers. The host repaints when `redraw` is
    /// set — a view whose state changed on its own (a background preview
    /// landing) returns [`ViewAction::Redraw`], and any emitted event also
    /// implies a repaint. Without this, tick-driven changes stay invisible
    /// until the next key press.
    pub fn tick(&mut self) -> ViewTick {
        let action = self
            .views
            .last_mut()
            .map(|view| view.tick())
            .unwrap_or(ViewAction::None);
        let view_redraw = matches!(action, ViewAction::Redraw);
        let events = self.apply_action(action);
        ViewTick {
            redraw: view_redraw || !events.is_empty(),
            events,
        }
    }

    fn apply_action(&mut self, action: ViewAction) -> Vec<ViewEvent> {
        let mut events = Vec::new();
        let top = self.top_identity();
        match action {
            // Key and mouse paths already repaint after dispatch; `tick`
            // reads `Redraw` before calling here.
            ViewAction::None | ViewAction::Redraw => {}
            ViewAction::Close => {
                if let Some(view) = self.views.pop() {
                    tracing::debug!(target: "codewhale_tui::view_stack", action = "close", kind = ?view.kind(), depth = self.views.len(), "view closed via action");
                }
            }
            ViewAction::Emit(event) => {
                events.push(event);
            }
            ViewAction::EmitAndClose(event) => {
                events.push(event);
                if let Some(view) = self.views.pop() {
                    tracing::debug!(target: "codewhale_tui::view_stack", action = "emit_and_close", kind = ?view.kind(), depth = self.views.len(), "view closed via action");
                }
            }
        }
        self.note_top_change(top);
        events
    }

    /// Whether the Extensions panel is the top view.
    pub fn extensions_is_top(&self) -> bool {
        self.views
            .last()
            .is_some_and(|view| view.kind() == ModalKind::Extensions)
    }

    /// Run an Extensions row-menu entry against the panel, when it is on
    /// top. `None` when the panel is gone or no longer lists the row.
    pub fn extensions_menu_action(
        &mut self,
        item_id: &str,
        verb: ExtensionMenuVerb,
    ) -> Option<Vec<ViewEvent>> {
        let action = self
            .views
            .last_mut()?
            .as_any_mut()
            .downcast_mut::<extensions::ExtensionsView>()?
            .run_menu_verb(item_id, verb)?;
        Some(self.apply_action(action))
    }

    /// Whether a provider picker anywhere in the stack has been used — a key
    /// pressed or a click landed in it.
    pub fn provider_picker_interacted(&mut self) -> bool {
        self.views.iter_mut().any(|view| {
            view.as_any_mut()
                .downcast_mut::<crate::tui::provider_picker::ProviderPickerView>()
                .is_some_and(|picker| picker.interacted())
        })
    }

    /// Hand a freshly-built read model to the open Extensions panel, when it
    /// is on top. A pager or another modal stacked above it means the user is
    /// looking at something else — the rebuild is skipped and the next poll
    /// retries.
    pub fn refresh_extensions(&mut self, snapshot: extensions::ExtensionsSnapshot) {
        if let Some(view) = self.views.last_mut()
            && let Some(panel) = view
                .as_any_mut()
                .downcast_mut::<extensions::ExtensionsView>()
        {
            panel.refresh_snapshot(snapshot);
        }
    }
}

impl fmt::Debug for ViewStack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ViewStack")
            .field("len", &self.views.len())
            .field("top", &self.top_kind())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigScope {
    Session,
    Saved,
}

impl ConfigScope {
    fn label(self, locale: Locale) -> Cow<'static, str> {
        tr(
            locale,
            match self {
                ConfigScope::Session => MessageId::ConfigScopeSession,
                ConfigScope::Saved => MessageId::ConfigScopeSaved,
            },
        )
    }

    fn persist(self) -> bool {
        matches!(self, ConfigScope::Saved)
    }
}

#[derive(Debug, Clone)]
struct ConfigRow {
    key: String,
    value: String,
    editable: bool,
    scope: ConfigScope,
    /// Typed facts decided when the row is built from `App`, `Settings`, and
    /// `Config`; the shell never re-derives them from the key at render time.
    facts: ConfigRowFacts,
}

impl ConfigRow {
    fn edit_value(&self) -> &str {
        if self.key.starts_with("notifications.") {
            self.facts.effective.as_deref().unwrap_or(&self.value)
        } else {
            &self.value
        }
    }

    /// The schema declaration behind this row. `None` means the key is not
    /// declared, and the row is dropped before the view is built.
    fn schema(&self) -> Option<&'static codewhale_config::SettingDef> {
        codewhale_config::setting(&self.key)
    }

    /// The row's `ui` block. `None` means "declared, but not shown".
    fn ui(&self) -> Option<&'static codewhale_config::SettingUi> {
        self.schema().and_then(|def| def.ui.as_ref())
    }

    /// Section heading, from the schema's group id.
    fn section(&self) -> ConfigSection {
        self.ui()
            .and_then(|ui| ConfigSection::from_id(ui.group))
            .unwrap_or(ConfigSection::Experimental)
    }
}

/// What a row *is*: a persisted or session setting fact, an action that opens
/// another surface, or a read-only receipt observed from the running app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigRowKind {
    Setting,
    Action,
    Diagnostic,
}

/// Which [`UiSnapshot`] fact a row projects, when it projects one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotLane {
    Provider,
    Model,
}

/// Which durable store a saved row persists to — independent of which
/// authority currently wins the *effective* decision. An environment or
/// terminal override (NO_ANIMATIONS, a legacy console host, …) relabels the
/// row's authority without repairing or breaking its store, so store-error
/// marking must key on this field, never on `authority`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingStore {
    /// `settings.toml` (user settings).
    UserSettings,
    /// `config.toml` (workspace configuration).
    WorkspaceConfig,
    /// Session-owned, diagnostic, or otherwise not persisted from this surface.
    None,
}

impl SettingStore {
    /// The store an authority implies when a row is *built* under it. Only
    /// meaningful at construction: an override authority applied later must
    /// keep the row's original store.
    fn for_authority(authority: SettingAuthority) -> Self {
        match authority {
            SettingAuthority::UserSettings => SettingStore::UserSettings,
            SettingAuthority::WorkspaceConfiguration => SettingStore::WorkspaceConfig,
            _ => SettingStore::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfigRowFacts {
    kind: ConfigRowKind,
    authority: SettingAuthority,
    /// The store this row's saved/startup lanes come from (#5730 Windows CI:
    /// an override-authority row still has a store that can fail to load).
    store: SettingStore,
    apply: SettingApplySemantics,
    /// Value observed in force from an explicit `App` field. `None` means
    /// unobserved; it is never inferred from the persisted value.
    effective: Option<String>,
    /// Snapshot lane supplying the live fact for this row.
    snapshot: Option<SnapshotLane>,
    /// Slash command that activation runs, with the localized verb for it.
    command: Option<(&'static str, MessageId)>,
    /// The concrete environment/terminal token when `authority` is an
    /// override (`NO_ANIMATIONS`, `TERM_PROGRAM=vscode`, …).
    authority_detail: Option<&'static str>,
    /// The load error of the row's store when it could not be read; the saved
    /// and startup lanes are then unavailable, never a default in disguise.
    store_error: Option<String>,
}

impl ConfigRowFacts {
    /// A `settings.toml` value edited here and applied on save.
    fn saved_setting() -> Self {
        Self {
            kind: ConfigRowKind::Setting,
            authority: SettingAuthority::UserSettings,
            store: SettingStore::UserSettings,
            apply: SettingApplySemantics::Immediate,
            effective: None,
            snapshot: None,
            command: None,
            authority_detail: None,
            store_error: None,
        }
    }

    /// The effective value is forced by an environment or terminal override.
    fn overridden(self, environment: bool, detail: &'static str) -> Self {
        Self {
            authority: if environment {
                SettingAuthority::Environment
            } else {
                SettingAuthority::Terminal
            },
            authority_detail: Some(detail),
            ..self
        }
    }

    /// The row's store failed to load: no saved or startup value is known.
    /// The error is folded onto one line so it reads in a single lane.
    fn unavailable(self, error: &str) -> Self {
        Self {
            store_error: Some(error.split_whitespace().collect::<Vec<_>>().join(" ")),
            ..self
        }
    }

    /// A value the live session owns; its row value *is* the observed value.
    fn session_setting() -> Self {
        Self {
            authority: SettingAuthority::Session,
            store: SettingStore::None,
            apply: SettingApplySemantics::EffectiveNow,
            ..Self::saved_setting()
        }
    }

    /// A persisted setting shown but not editable from this surface.
    fn read_only_setting(authority: SettingAuthority) -> Self {
        Self {
            store: SettingStore::for_authority(authority),
            authority,
            apply: SettingApplySemantics::ReadOnly,
            ..Self::saved_setting()
        }
    }

    /// A receipt observed from the running app, never a persisted fact.
    fn diagnostic(authority: SettingAuthority) -> Self {
        Self {
            kind: ConfigRowKind::Diagnostic,
            store: SettingStore::None,
            authority,
            apply: SettingApplySemantics::ReadOnly,
            ..Self::saved_setting()
        }
    }

    /// A row whose activation opens another surface; not a persisted fact.
    fn action(command: &'static str, verb: MessageId) -> Self {
        Self {
            kind: ConfigRowKind::Action,
            authority: SettingAuthority::Session,
            store: SettingStore::None,
            apply: SettingApplySemantics::EffectiveNow,
            command: Some((command, verb)),
            ..Self::saved_setting()
        }
    }

    fn authority(self, authority: SettingAuthority) -> Self {
        // Callers that relabel a row's authority through this builder are
        // describing the row's store (e.g. a config.toml row), so the store
        // follows. An *override* never goes through this builder — it wins
        // the effective decision while the store stays what it was.
        let store = SettingStore::for_authority(authority);
        Self {
            authority,
            store,
            ..self
        }
    }

    fn apply(self, apply: SettingApplySemantics) -> Self {
        Self { apply, ..self }
    }

    fn effective(self, value: impl Into<String>) -> Self {
        Self {
            effective: Some(value.into()),
            ..self
        }
    }

    fn snapshot(self, lane: SnapshotLane) -> Self {
        Self {
            snapshot: Some(lane),
            ..self
        }
    }

    /// A setting fact whose activation opens a picker instead of an editor.
    fn opens(self, command: &'static str, verb: MessageId) -> Self {
        Self {
            command: Some((command, verb)),
            ..self
        }
    }
}

/// Editor behavior for one Settings entry. This is intentionally independent
/// from where the value is stored: category/scope describe ownership, while
/// kind determines the interaction and validation surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingKind {
    Boolean,
    Choice,
    Integer,
    Text,
    Action,
    ReadOnly,
}

#[derive(Debug, Clone)]
struct SettingMeta {
    kind: SettingKind,
    category: ConfigSection,
    choices: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct SettingsRegistry {
    provider: ProviderKind,
    base_url: String,
    model: String,
    auto_model: bool,
}

impl SettingsRegistry {
    fn new(view: &ConfigView) -> Self {
        Self {
            provider: view.api_provider,
            base_url: view.route_base_url.clone(),
            model: view.route_model.clone(),
            auto_model: view.auto_model,
        }
    }

    fn reasoning_effort_choices(&self) -> Vec<String> {
        let mut values = vec!["default".to_string()];
        for effort in crate::tui::model_picker::picker_efforts_for_route(
            self.provider,
            &self.base_url,
            &self.model,
            self.auto_model,
        ) {
            let label = if self.provider == ProviderKind::OpenaiCodex {
                effort.display_label_for_provider(self.provider)
            } else {
                effort.as_setting()
            };
            if !values.iter().any(|value| value == label) {
                values.push(label.to_string());
            }
        }
        values
    }

    fn meta(&self, row: &ConfigRow) -> SettingMeta {
        let choices = if row.key == "reasoning_effort" {
            Some(self.reasoning_effort_choices())
        } else {
            config_choice_values(&row.key)
        };
        let kind = if !row.editable {
            SettingKind::ReadOnly
        } else if row.facts.command.is_some() {
            SettingKind::Action
        } else if config_boolean_key(&row.key) {
            SettingKind::Boolean
        } else if choices.is_some() {
            SettingKind::Choice
        } else if config_integer_key(&row.key) {
            SettingKind::Integer
        } else {
            SettingKind::Text
        };
        SettingMeta {
            kind,
            category: row.section(),
            choices,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigSection {
    Provider,
    Model,
    Permissions,
    Network,
    Display,
    Composer,
    Sidebar,
    History,
    Mcp,
    Fleet,
    /// Workflow orchestration (`/workflow`). Kept out of Fleet: a Fleet is
    /// *who*, a Workflow is *what order* the work follows over it.
    Workflow,
    /// Session-scoped drivers such as `/goal`.
    Session,
    /// Explicitly legacy compatibility settings that are not a live choice —
    /// e.g. the DeepSeek-only `default_model` fallback (#4751).
    Legacy,
    Experimental,
}

/// The seven Tideline settings categories in rail order
/// (`docs/design/tideline-redesign.html`, "Settings categories").
///
/// A category is a projection over the existing [`ConfigRow`] store: rows keep
/// their fine-grained [`ConfigSection`]; the category follows the section
/// unless the row's typed facts file it elsewhere (motion keys, telemetry,
/// low-level receipts).
///
/// This enum is the single taxonomy: the `ConfigView` rail/strip and the
/// Tideline settings stage scaffold both iterate [`ConfigCategory::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigCategory {
    Appearance,
    ModelsProviders,
    Work,
    ToolsMcp,
    Trust,
    Motion,
    Advanced,
}

impl ConfigCategory {
    /// The schema tab id this category renders.
    fn id(self) -> &'static str {
        match self {
            ConfigCategory::Appearance => codewhale_config::settings_schema::TAB_APPEARANCE,
            ConfigCategory::ModelsProviders => codewhale_config::settings_schema::TAB_MODELS,
            ConfigCategory::Work => codewhale_config::settings_schema::TAB_WORK,
            ConfigCategory::ToolsMcp => codewhale_config::settings_schema::TAB_TOOLS,
            ConfigCategory::Trust => codewhale_config::settings_schema::TAB_TRUST,
            ConfigCategory::Motion => codewhale_config::settings_schema::TAB_MOTION,
            ConfigCategory::Advanced => codewhale_config::settings_schema::TAB_ADVANCED,
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|category| category.id() == id)
    }

    const ALL: [ConfigCategory; 7] = [
        ConfigCategory::Appearance,
        ConfigCategory::ModelsProviders,
        ConfigCategory::Work,
        ConfigCategory::ToolsMcp,
        ConfigCategory::Trust,
        ConfigCategory::Motion,
        ConfigCategory::Advanced,
    ];

    fn label(self, locale: Locale) -> Cow<'static, str> {
        tr(
            locale,
            match self {
                ConfigCategory::Appearance => MessageId::ConfigCategoryAppearance,
                ConfigCategory::ModelsProviders => MessageId::ConfigCategoryModelsProviders,
                ConfigCategory::Work => MessageId::ConfigCategoryWork,
                ConfigCategory::ToolsMcp => MessageId::ConfigCategoryToolsMcp,
                ConfigCategory::Trust => MessageId::ConfigCategoryTrust,
                ConfigCategory::Motion => MessageId::ConfigCategoryMotion,
                ConfigCategory::Advanced => MessageId::ConfigCategoryAdvanced,
            },
        )
    }

    /// The rail category the schema files this row under.
    fn for_row(row: &ConfigRow) -> Self {
        row.ui()
            .and_then(|ui| Self::from_id(ui.tab))
            .unwrap_or(ConfigCategory::Advanced)
    }

    fn contains(self, row: &ConfigRow) -> bool {
        Self::for_row(row) == self
    }

    fn position(self) -> usize {
        Self::ALL
            .iter()
            .position(|category| *category == self)
            .unwrap_or(0)
    }

    fn next(self) -> Self {
        Self::ALL[(self.position() + 1) % Self::ALL.len()]
    }

    fn prev(self) -> Self {
        Self::ALL[(self.position() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

impl ConfigSection {
    const ALL: [ConfigSection; 14] = [
        ConfigSection::Provider,
        ConfigSection::Model,
        ConfigSection::Permissions,
        ConfigSection::Network,
        ConfigSection::Display,
        ConfigSection::Composer,
        ConfigSection::Sidebar,
        ConfigSection::History,
        ConfigSection::Mcp,
        ConfigSection::Fleet,
        ConfigSection::Workflow,
        ConfigSection::Session,
        ConfigSection::Legacy,
        ConfigSection::Experimental,
    ];

    /// The schema group id this section heads.
    fn id(self) -> &'static str {
        match self {
            ConfigSection::Provider => "provider",
            ConfigSection::Model => "model",
            ConfigSection::Permissions => "permissions",
            ConfigSection::Network => "network",
            ConfigSection::Display => "display",
            ConfigSection::Composer => "composer",
            ConfigSection::Sidebar => "workbar",
            ConfigSection::History => "history",
            ConfigSection::Mcp => "mcp",
            ConfigSection::Fleet => "fleet",
            ConfigSection::Workflow => "workflow",
            ConfigSection::Session => "session",
            ConfigSection::Legacy => "legacy",
            ConfigSection::Experimental => "experimental",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|section| section.id() == id)
    }

    fn label(self, locale: Locale) -> Cow<'static, str> {
        tr(
            locale,
            match self {
                ConfigSection::Provider => MessageId::ConfigSectionProvider,
                ConfigSection::Model => MessageId::ConfigSectionModel,
                ConfigSection::Permissions => MessageId::ConfigSectionPermissions,
                ConfigSection::Network => MessageId::ConfigSectionNetwork,
                ConfigSection::Display => MessageId::ConfigSectionDisplay,
                ConfigSection::Composer => MessageId::ConfigSectionComposer,
                ConfigSection::Sidebar => MessageId::ConfigSectionSidebar,
                ConfigSection::History => MessageId::ConfigSectionHistory,
                ConfigSection::Mcp => MessageId::ConfigSectionMcp,
                ConfigSection::Fleet => MessageId::ConfigSectionFleet,
                ConfigSection::Workflow => MessageId::ConfigSectionWorkflow,
                ConfigSection::Session => MessageId::ConfigSectionSession,
                ConfigSection::Legacy => MessageId::ConfigSectionLegacy,
                ConfigSection::Experimental => MessageId::ConfigSectionExperimental,
            },
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigListItem {
    Section(ConfigSection),
    Row(usize),
}

/// Clickable editor controls; keyboard Enter/Esc do the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorControl {
    Apply,
    Cancel,
}

/// Clickable overflow markers of the category strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavStep {
    Previous,
    Next,
}

#[derive(Debug, Clone)]
struct ConfigEdit {
    key: String,
    original_value: String,
    buffer: Vec<char>,
    cursor: usize,
    select_all: bool,
    scope: ConfigScope,
    choices: Option<Vec<String>>,
    selected_choice: usize,
}

pub struct ConfigView {
    rows: Vec<ConfigRow>,
    selected: usize,
    scroll: usize,
    editing: Option<ConfigEdit>,
    /// The editor as it was committed, kept until the host answers: a rejected
    /// value reopens it with the typed buffer instead of dropping it (U09-m1).
    pending_commit: Option<ConfigEdit>,
    filter: String,
    status: Option<String>,
    locale: Locale,
    last_visible_rows: Cell<usize>,
    /// Selection-anchored scroll actually used by the last render; keeps the
    /// panel scroll rail truthful when the stored scroll predates a resize.
    last_render_scroll: Cell<usize>,
    /// Exact painted cells of each list row; a click selects a row only when
    /// it lands inside one of these rects.
    last_row_hitboxes: RefCell<Vec<(Rect, usize)>>,
    /// Exact painted cells of each visible editor choice.
    last_choice_hitboxes: RefCell<Vec<(Rect, usize)>>,
    /// Exact painted cells of the editor's Apply / Cancel controls.
    last_editor_controls: RefCell<Vec<(Rect, EditorControl)>>,
    /// Exact painted cells of each category in the rail or strip.
    last_rail_hitboxes: RefCell<Vec<(Rect, ConfigCategory)>>,
    /// Exact painted cells of the strip's ‹ / › overflow markers.
    last_nav_controls: RefCell<Vec<(Rect, NavStep)>>,
    last_mouse_selected: Option<usize>,
    /// Pointer hover state, repainted from the shared hover style. Hover
    /// never moves the keyboard selection; it only tints what the pointer
    /// is over so every clickable element answers visibly.
    hovered_row: Option<usize>,
    hovered_rail: Option<ConfigCategory>,
    hovered_nav: Option<NavStep>,
    hovered_editor: Option<EditorControl>,
    hovered_choice: Option<usize>,
    api_provider: ProviderKind,
    route_base_url: String,
    route_model: String,
    auto_model: bool,
    /// Selected rail category of the Tideline settings shell.
    category: ConfigCategory,
    /// Read-only session projection for the provider/model facts; the detail
    /// pane shows its lanes verbatim instead of guessing a saved default.
    snapshot: UiSnapshot,
}

const CONFIG_MIN_KEY_COLUMN_WIDTH: usize = 19;
const CONFIG_VALUE_COLUMN_WIDTH: usize = 44;
const CONFIG_MIN_VALUE_COLUMN_WIDTH: usize = 10;
const CONFIG_SCOPE_COLUMN_WIDTH: usize = 7;
const CONFIG_ROW_PREFIX_WIDTH: usize = 2;
/// The two two-column gaps painted between key, value, and affordance.
const CONFIG_COLUMN_GAPS_WIDTH: usize = 4;
/// Affordance glyph column (`[x]`, `‹ ›`, `✎`, `›`, `⊘`) plus its gap.
const CONFIG_AFFORDANCE_COLUMN_WIDTH: usize = 5;
/// List width below which the scope badge sheds in favour of the value.
const CONFIG_SCOPE_BADGE_MIN_WIDTH: usize = 60;

impl ConfigView {
    pub fn new_for_app(app: &App) -> Self {
        // A store that fails to load yields no saved facts. The defaults below
        // only shape the row list; every row backed by a failed store is
        // marked unavailable before the view is returned.
        let (settings, settings_error) = match Settings::load_persisted() {
            Ok(settings) => {
                let error = settings.load_error.clone();
                (settings, error)
            }
            Err(error) => (Settings::default(), Some(error.to_string())),
        };
        let (config, config_error) =
            match Config::load(app.config_path.clone(), app.config_profile.as_deref()) {
                Ok(config) => (config, None),
                Err(error) => (Config::default(), Some(error.to_string())),
            };
        let motion_override = crate::settings::detect_low_motion_override();
        let motion_facts = |facts: ConfigRowFacts| match motion_override {
            Some(source) => facts.overridden(source.is_environment(), source.label()),
            None => facts,
        };
        let permission_control = config.approval_policy_control(
            app.config_path.as_deref(),
            app.config_profile.as_deref(),
            &app.workspace,
        );
        let saved_permission_row = match permission_control {
            ApprovalPolicyControl::Unset => ConfigRow {
                key: "permission_posture".to_string(),
                value: settings
                    .permission_posture
                    .as_deref()
                    .unwrap_or("ask")
                    .to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ApprovalPolicyControl::RootConfig => ConfigRow {
                key: "approval_policy".to_string(),
                value: config
                    .approval_policy
                    .as_deref()
                    .unwrap_or("ask")
                    .to_string(),
                editable: permission_control.editable_root(),
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting()
                    .authority(SettingAuthority::WorkspaceConfiguration),
            },
            source => ConfigRow {
                key: "managed_approval_policy".to_string(),
                value: format!(
                    "{} · {}",
                    app.approval_mode.permission_chip_label(),
                    source.label()
                ),
                editable: false,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::read_only_setting(SettingAuthority::ManagedPolicy),
            },
        };
        let approval_session_editable = matches!(permission_control, ApprovalPolicyControl::Unset);
        let shell_control = config.allow_shell_control(
            app.config_path.as_deref(),
            app.config_profile.as_deref(),
            &app.workspace,
        );
        let shell_row = if shell_control.editable_root() {
            ConfigRow {
                key: "allow_shell".to_string(),
                value: app.allow_shell.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting()
                    .authority(SettingAuthority::WorkspaceConfiguration),
            }
        } else {
            ConfigRow {
                key: "managed_allow_shell".to_string(),
                value: format!("{} · {}", app.allow_shell, shell_control.label()),
                editable: false,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::read_only_setting(SettingAuthority::ManagedPolicy),
            }
        };
        let (active_route_provider, _) = app.effective_route_display();
        let (active_provider_identity, active_route_model) = app.effective_route_identity_display();
        let mut rows = vec![
            ConfigRow {
                key: "provider".to_string(),
                value: active_provider_identity.clone(),
                editable: true,
                scope: ConfigScope::Session,
                facts: ConfigRowFacts::session_setting()
                    .snapshot(SnapshotLane::Provider)
                    .opens("/provider", MessageId::ConfigActionOpenProvider),
            },
            ConfigRow {
                key: config_base_url_row_key(active_route_provider).to_string(),
                value: config_base_url_row_value(app),
                // An endpoint is a route receipt, not a loose global knob.
                // `/provider` owns changing the credential, model, and endpoint
                // together; this row must not pretend that editing a live
                // receipt can mutate an already-running client.
                editable: false,
                scope: ConfigScope::Session,
                facts: ConfigRowFacts::diagnostic(SettingAuthority::Session),
            },
            ConfigRow {
                key: "context_window".to_string(),
                value: config
                    .active_provider_identity()
                    .ok()
                    .filter(|identity| {
                        // Display names are labels; saved facts require the
                        // exact provider identity admitted by this App.
                        app.admitted_provider_identity()
                            .is_ok_and(|active| active == identity)
                            && identity.provider == active_route_provider
                            && (!app.auto_model
                                || app.last_effective_provider.is_none()
                                || app.last_effective_provider_identity.as_deref()
                                    == Some(identity.key.as_str()))
                    })
                    .and_then(|identity| config.context_window_for_provider_config(&identity))
                    .map_or_else(|| "(not set)".to_string(), |tokens| tokens.to_string()),
                editable: false,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::read_only_setting(SettingAuthority::WorkspaceConfiguration),
            },
            ConfigRow {
                key: "effective_context_window".to_string(),
                value: format!(
                    "{} tokens · {}",
                    crate::route_budget::route_context_window_tokens(
                        app.api_provider,
                        app.effective_model_for_budget(),
                        app.active_route_limits,
                    ),
                    app.active_context_window_source.display_label()
                ),
                editable: false,
                scope: ConfigScope::Session,
                facts: ConfigRowFacts::diagnostic(SettingAuthority::Session),
            },
            ConfigRow {
                key: "model".to_string(),
                // `·` keeps the row unambiguous when a provider display name
                // itself contains `/` (e.g. `Zhipu AI / Z.ai`).
                value: format!("{active_provider_identity} · {active_route_model}"),
                editable: true,
                scope: ConfigScope::Session,
                facts: ConfigRowFacts::session_setting()
                    .snapshot(SnapshotLane::Model)
                    .opens("/model", MessageId::ConfigActionOpenModel),
            },
            // DeepSeek-only legacy fallback: hide on non-DeepSeek providers so
            // it is not misread as an active setting (#4717). Keep the field
            // and routing behavior; surface the row only for DeepSeek routes
            // (or when an explicit value is set and the operator needs to see it).
            // Built below after provider check so non-DeepSeek menus stay clean.
            ConfigRow {
                key: "reasoning_effort".to_string(),
                value: settings.reasoning_effort.as_deref().map_or_else(
                    || tr(app.ui_locale, MessageId::ConfigDefaultReasoning).to_string(),
                    |value| {
                        crate::reasoning_preference::ReasoningEffort::from_setting_for_provider(
                            value,
                            app.api_provider,
                        )
                        .as_setting_for_provider(app.api_provider)
                        .to_string()
                    },
                ),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "approval_mode".to_string(),
                value: app.approval_mode.permission_chip_label().to_string(),
                editable: approval_session_editable,
                scope: ConfigScope::Session,
                facts: ConfigRowFacts::session_setting(),
            },
            saved_permission_row,
            ConfigRow {
                key: "default_mode".to_string(),
                value: settings.default_mode.clone(),
                editable: true,
                scope: ConfigScope::Saved,
                // The startup mode is read once when a session begins.
                facts: ConfigRowFacts::saved_setting().apply(SettingApplySemantics::NextSession),
            },
            shell_row,
            ConfigRow {
                key: "telemetry".to_string(),
                value: crate::telemetry_notice::saved_preference_enabled(&config).to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                // Telemetry is a trust decision, not a network knob.
                facts: ConfigRowFacts::saved_setting()
                    .authority(SettingAuthority::WorkspaceConfiguration),
            },
            ConfigRow {
                key: "stream_chunk_timeout_secs".to_string(),
                value: app.stream_chunk_timeout_secs.to_string(),
                editable: true,
                scope: ConfigScope::Session,
                facts: ConfigRowFacts::session_setting(),
            },
            ConfigRow {
                key: "theme".to_string(),
                value: settings.theme.clone(),
                editable: true,
                scope: ConfigScope::Saved,
                // The live theme is the one the app is painting with.
                facts: ConfigRowFacts::saved_setting().effective(app.theme_id.name()),
            },
            ConfigRow {
                key: "locale".to_string(),
                value: settings.locale.clone(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting().effective(app.ui_locale.tag()),
            },
            ConfigRow {
                key: "background_color".to_string(),
                value: settings.background_color.clone().unwrap_or_else(|| {
                    tr(app.ui_locale, MessageId::ConfigDefaultValue).to_string()
                }),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "focus_texture".to_string(),
                value: settings.focus_texture.clone(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "pet_mode".to_string(),
                value: settings.pet_mode.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting().effective(app.pet_watch.enabled.to_string()),
            },
            ConfigRow {
                key: "calm_mode".to_string(),
                value: settings.calm_mode.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "low_motion".to_string(),
                value: settings.low_motion.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                // `low_motion` always wins over fancy animations; both are
                // Motion, and their live values come from `App`, not disk.
                facts: motion_facts(
                    ConfigRowFacts::saved_setting().effective(app.low_motion.to_string()),
                ),
            },
            ConfigRow {
                key: "fancy_animations".to_string(),
                value: settings.fancy_animations.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: motion_facts(
                    ConfigRowFacts::saved_setting().effective(app.fancy_animations.to_string()),
                ),
            },
            // `launch_screen` is a retired setting: accepted on load, dropped
            // on save — no config row (main's retirement wins over the
            // branch's stale row).
            ConfigRow {
                key: "show_thinking".to_string(),
                value: settings.show_thinking.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "thinking_default_expanded".to_string(),
                value: settings.thinking_default_expanded.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "thinking_preview_lines".to_string(),
                value: settings.thinking_preview_lines.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "thinking_highlight".to_string(),
                value: settings.thinking_highlight.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "help_expand_groups".to_string(),
                value: settings.help_expand_groups.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "contextual_tips".to_string(),
                value: settings.contextual_tips.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting()
                    .effective(app.behavioral_tips.enabled().to_string()),
            },
            ConfigRow {
                key: "pin_last_prompt".to_string(),
                value: settings.pin_last_prompt.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "show_tool_details".to_string(),
                value: settings.show_tool_details.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "inline_diffs".to_string(),
                value: settings.inline_diffs.clone(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "status_indicator".to_string(),
                value: settings.status_indicator.clone(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "synchronized_output".to_string(),
                value: settings.synchronized_output.clone(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "cost_currency".to_string(),
                value: settings.cost_currency.clone(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting().effective(cost_currency_config_value(app)),
            },
            ConfigRow {
                key: "transcript_spacing".to_string(),
                value: settings.transcript_spacing.clone(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "tool_collapse".to_string(),
                value: settings.tool_collapse_mode.clone(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "composer_density".to_string(),
                value: settings.composer_density.clone(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "composer_border".to_string(),
                value: settings.composer_border.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "composer_multiline_mode".to_string(),
                value: settings.composer_multiline_mode.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "composer_vim_mode".to_string(),
                value: settings.composer_vim_mode.clone(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "bracketed_paste".to_string(),
                value: settings.bracketed_paste.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "paste_burst_detection".to_string(),
                value: settings.paste_burst_detection.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "mention_menu_limit".to_string(),
                value: settings.mention_menu_limit.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "mention_menu_behavior".to_string(),
                value: settings.mention_menu_behavior.clone(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "mention_walk_depth".to_string(),
                value: settings.mention_walk_depth.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "workspace_follow_symlinks".to_string(),
                value: settings.workspace_follow_symlinks.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                // Mention menus follow it now; engine tools read it at startup.
                facts: ConfigRowFacts::saved_setting()
                    .apply(SettingApplySemantics::UiNowEngineRestart),
            },
            ConfigRow {
                key: "work_surface_placement".to_string(),
                value: settings.work_surface_placement.clone(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "work_surface_top_height".to_string(),
                value: settings.work_surface_top_height.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "work_surface_side_width".to_string(),
                value: settings.work_surface_side_width.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "rail_panel".to_string(),
                value: settings.rail_panel.clone(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "context_panel".to_string(),
                value: settings.context_panel.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "sessions_rail".to_string(),
                value: settings.sessions_rail.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            // Read at startup by `main`, not held on `App`, so the row reflects
            // the persisted value rather than a live field (#2934).
            ConfigRow {
                key: "session_auto_resume".to_string(),
                value: settings.session_auto_resume.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting().apply(SettingApplySemantics::NextSession),
            },
            ConfigRow {
                key: "auto_compact".to_string(),
                value: settings.auto_compact.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "auto_compact_threshold_percent".to_string(),
                value: format!("{:.0}", settings.auto_compact_threshold_percent),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "max_history".to_string(),
                value: settings.max_input_history.to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::saved_setting(),
            },
            ConfigRow {
                key: "mcp_open".to_string(),
                value: "/mcp".to_string(),
                editable: true,
                scope: ConfigScope::Session,
                facts: ConfigRowFacts::action("/mcp", MessageId::ConfigActionOpenMcp),
            },
            ConfigRow {
                key: "mcp_reconnect".to_string(),
                value: "/mcp reload".to_string(),
                editable: true,
                scope: ConfigScope::Session,
                facts: ConfigRowFacts::action("/mcp reload", MessageId::ConfigActionMcpReconnect),
            },
            ConfigRow {
                key: "mcp_diagnose".to_string(),
                value: "/mcp validate".to_string(),
                editable: true,
                scope: ConfigScope::Session,
                facts: ConfigRowFacts::action("/mcp validate", MessageId::ConfigActionMcpDiagnose),
            },
            ConfigRow {
                key: "plugins_open".to_string(),
                value: "/plugin".to_string(),
                editable: true,
                scope: ConfigScope::Session,
                facts: ConfigRowFacts::action("/plugin", MessageId::ConfigActionOpenPlugins),
            },
            ConfigRow {
                key: "mcp_config_path".to_string(),
                value: app.mcp_config_path.display().to_string(),
                editable: true,
                scope: ConfigScope::Saved,
                // The live path changes on save; running servers keep their
                // old config until `/mcp reload`.
                facts: ConfigRowFacts::saved_setting()
                    .authority(SettingAuthority::WorkspaceConfiguration)
                    .apply(SettingApplySemantics::ReloadRequired),
            },
            ConfigRow {
                key: "fleet.exec.max_spawn_depth".to_string(),
                value: config
                    .fleet
                    .as_ref()
                    .map(|fleet| fleet.exec.max_spawn_depth)
                    .unwrap_or_else(|| codewhale_config::FleetExecConfig::default().max_spawn_depth)
                    .to_string(),
                editable: false,
                scope: ConfigScope::Saved,
                facts: ConfigRowFacts::read_only_setting(SettingAuthority::WorkspaceConfiguration),
            },
        ];
        // The DeepSeek-only legacy fallback stays a persisted runtime key but
        // has no settings row: it is not a live choice on any provider, and
        // a leftover value is cleared with `/set default_model` instead of
        // a Legacy table section.
        let external_status_rows = [ProviderKind::OpenaiCodex, ProviderKind::Xai]
            .into_iter()
            .filter_map(|provider| {
                config
                    .builtin_provider_identity(provider)
                    .ok()
                    .and_then(|identity| config.external_credential_consent_status(&identity))
                    .map(|status| {
                        let state = if status.route_state == "active" {
                            tr(app.ui_locale, MessageId::CtxInspActive)
                        } else {
                            tr(app.ui_locale, MessageId::ProviderExternalDormant)
                        };
                        let scope = tr(app.ui_locale, MessageId::ProviderExternalDetailScope)
                            .replace("{access}", status.access.as_str())
                            .replace("{provider}", &status.provider)
                            .replace("{source}", status.source.as_str())
                            .replace("{version}", &status.consent_version.to_string())
                            .replace("{state}", &state);
                        let owner_path = tr(app.ui_locale, MessageId::ProviderExternalOwnerPath)
                            .replace("{owner}", status.owner)
                            .replace("{path}", &codewhale_config::quote_os_path(&status.path));
                        let pinned_warning = status.ambient_path_changed.then(|| {
                            tr(app.ui_locale, MessageId::ProviderExternalPinnedPathWarning)
                                .replace("{owner}", status.owner)
                                .replace("{path}", &codewhale_config::quote_os_path(&status.path))
                        });
                        let semantics = match status.access {
                            codewhale_config::ExternalCredentialAccess::Disabled => {
                                tr(app.ui_locale, MessageId::ProviderExternalDisabledDetail)
                            }
                            codewhale_config::ExternalCredentialAccess::ReadOnly => {
                                tr(app.ui_locale, MessageId::ProviderExternalReadOnlySemantics)
                            }
                            codewhale_config::ExternalCredentialAccess::Managed => {
                                tr(app.ui_locale, MessageId::ProviderExternalManagedDetail)
                            }
                        };
                        let semantics_revoke =
                            tr(app.ui_locale, MessageId::ProviderExternalSemanticsRevoke)
                                .replace("{semantics}", &semantics)
                                .replace("{revoke}", &status.revoke_command);
                        ConfigRow {
                            key: format!("external_credentials.{}", provider.as_str()),
                            value: match pinned_warning {
                                Some(warning) => format!(
                                    "{scope} · {owner_path} · {warning} · {semantics_revoke}"
                                ),
                                None => format!("{scope} · {owner_path} · {semantics_revoke}"),
                            },
                            editable: false,
                            scope: ConfigScope::Saved,
                            facts: ConfigRowFacts::diagnostic(
                                SettingAuthority::WorkspaceConfiguration,
                            ),
                        }
                    })
            });
        rows.splice(2..2, external_status_rows);
        // An explanation route, never an editable sandbox policy. The existing
        // status report owns the observed platform/backend enforcement facts.
        rows.push(ConfigRow {
            key: "sandbox_details".into(),
            value: "/status".into(),
            editable: true,
            scope: ConfigScope::Session,
            facts: ConfigRowFacts::action("/status", MessageId::AutomationActionInspect),
        });
        rows.extend(experimental_config_rows(&config));
        rows.extend(
            codewhale_config::notifications::NotificationSetting::ALL
                .into_iter()
                .map(|setting| ConfigRow {
                    key: format!("notifications.{}", setting.key()),
                    value: config.notifications_config().display(setting),
                    editable: true,
                    scope: ConfigScope::Saved,
                    facts: ConfigRowFacts::saved_setting()
                        .authority(SettingAuthority::WorkspaceConfiguration)
                        .effective(app.notification_settings.display(setting)),
                }),
        );

        // The schema decides what is shown and in what order. A row whose key
        // carries no `ui` block is declared but not browsable (it stays
        // settable through `/set`); a row the schema does not declare at all
        // cannot be placed, labelled, or edited, so it is not painted either.
        rows.retain(|row| row.ui().is_some());
        rows.sort_by_key(|row| codewhale_config::setting_index(&row.key).unwrap_or(usize::MAX));

        // A row whose store failed to load carries the error instead of a
        // default: its value reads unavailable, it is not editable (writing
        // through an unreadable store could clobber it), and its saved and
        // startup lanes are reported as unavailable. Keyed on the row's
        // *store*, not its authority: an environment/terminal override wins
        // the effective decision without making the store any less broken
        // (caught by Windows CI, where the legacy-console probe relabels the
        // motion rows' authority and a broken store then went unreported).
        let unavailable = tr(app.ui_locale, MessageId::ConfigUnavailable).into_owned();
        for row in &mut rows {
            let error = match row.facts.store {
                SettingStore::UserSettings => settings_error.as_deref(),
                SettingStore::WorkspaceConfig => config_error.as_deref(),
                SettingStore::None => None,
            };
            if let Some(error) = error
                && row.facts.kind == ConfigRowKind::Setting
            {
                row.facts = row.facts.clone().unavailable(error);
                row.editable = false;
                row.value = unavailable.clone();
            }
        }

        let mut view = Self {
            rows,
            selected: 0,
            scroll: 0,
            editing: None,
            pending_commit: None,
            filter: String::new(),
            status: None,
            locale: app.ui_locale,
            last_visible_rows: Cell::new(0),
            last_render_scroll: Cell::new(0),
            last_row_hitboxes: RefCell::new(Vec::new()),
            last_choice_hitboxes: RefCell::new(Vec::new()),
            last_editor_controls: RefCell::new(Vec::new()),
            last_rail_hitboxes: RefCell::new(Vec::new()),
            last_nav_controls: RefCell::new(Vec::new()),
            last_mouse_selected: None,
            hovered_row: None,
            hovered_rail: None,
            hovered_nav: None,
            hovered_editor: None,
            hovered_choice: None,
            api_provider: app.api_provider,
            route_base_url: app.active_route_base_url.clone(),
            route_model: app.model.clone(),
            auto_model: app.auto_model,
            // Settings opens on Appearance (design: first rail category).
            category: ConfigCategory::Appearance,
            snapshot: UiSnapshot::from_app(app),
        };
        view.select_first_visible_row();
        view
    }

    fn tr(&self, id: MessageId) -> Cow<'static, str> {
        tr(self.locale, id)
    }

    /// Keep the user's place when the host rebuilds this view after applying
    /// a setting to the live app.
    pub(crate) fn focus_key(&mut self, key: &str) {
        if let Some(index) = self.rows.iter().position(|row| row.key == key) {
            self.category = ConfigCategory::for_row(&self.rows[index]);
            self.selected = index;
            self.last_mouse_selected = None;
            self.clear_hover();
            self.adjust_scroll(self.visible_rows_cached());
        }
    }

    /// Snapshot the active search so live config updates can rebuild the
    /// modal without making the user's filtered result set jump away.
    pub(crate) fn filter_query(&self) -> &str {
        &self.filter
    }

    /// The key whose inline editor is open, if any. Exposes the transient
    /// editing state to the host-level tests in `tui::ui::tests` that drive
    /// the real `refresh_config_view_if_open` path (#theme-nav-exit).
    ///
    /// Test-only: production code reads `editing` directly, and a non-test lib
    /// build would flag this as dead code under `-D warnings`.
    #[cfg(test)]
    pub(crate) fn editing_key(&self) -> Option<&str> {
        self.editing.as_ref().map(|edit| edit.key.as_str())
    }

    /// The highlighted choice index inside the open editor, if any.
    ///
    /// Test-only; see [`Self::editing_key`].
    #[cfg(test)]
    pub(crate) fn editing_selected_choice(&self) -> Option<usize> {
        self.editing.as_ref().map(|edit| edit.selected_choice)
    }

    /// Rebuild after the host applied a setting (`refresh_config_view_if_open`)
    /// while keeping the open editor alive.
    ///
    /// The theme editor live-previews on every highlight, and each preview is a
    /// `ConfigUpdated` that lands here again. A bare `new_for_app` dropped
    /// `editing`, so the first arrow key closed the editor, the next one fell
    /// through to the non-editing key map (where Left/Right switch category),
    /// and `selected_choice` snapped back to 0. The user saw the highlight leap
    /// away from the row they were on — the theme never moved (#theme-nav-exit).
    ///
    /// The rows themselves must come from the refreshed snapshot: a persisted
    /// commit changes the value on disk and the row has to show it. Only the
    /// transient editing state is carried over.
    pub(crate) fn rebuild_preserving(app: &App, previous: &Self, focus_key: &str) -> Self {
        let mut view = Self::new_for_app(app);
        view.restore_filter(previous.filter_query().to_string());
        view.focus_key(focus_key);
        let carried = match &previous.editing {
            // A row that vanished from the refreshed snapshot (filter, scope or
            // availability changed underneath) has no editor to belong to.
            Some(edit) => view
                .rows
                .iter()
                .position(|row| row.key == edit.key)
                .map(|index| (index, edit.clone())),
            None => None,
        };
        match carried {
            Some((index, mut edit)) => {
                // The highlight is the user's cursor, not a disk fact: keep it
                // inside the refreshed choice list instead of resetting it.
                let choices_len = edit.choices.as_ref().map_or(0, Vec::len);
                if edit.selected_choice >= choices_len {
                    edit.selected_choice = choices_len.saturating_sub(1);
                }
                view.selected = index;
                view.editing = Some(edit);
            }
            None => view.editing = None,
        }
        view
    }

    /// Reopen the editor the host just rejected, with the value the user
    /// typed, on a view rebuilt from disk truth. A row that no longer exists
    /// has nothing to reopen.
    pub(crate) fn restore_rejected_commit(&mut self, previous: &Self) {
        let Some(edit) = previous.pending_commit.clone() else {
            return;
        };
        if let Some(index) = self.rows.iter().position(|row| row.key == edit.key) {
            self.selected = index;
            self.editing = Some(edit);
        }
    }

    pub(crate) fn restore_filter(&mut self, filter: String) {
        self.update_filter(|current| *current = filter);
    }

    fn visible_rows_cached(&self) -> usize {
        let cached = self.last_visible_rows.get();
        if cached == 0 { 8 } else { cached }
    }

    /// The lowercased search terms for the current filter, computed once per
    /// interaction instead of once per row per pass (#6213 T6).
    fn filter_terms(&self) -> Vec<String> {
        self.filter
            .trim()
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    fn row_matches_filter(&self, row: &ConfigRow, terms: &[String]) -> bool {
        if terms.is_empty() {
            return true;
        }

        let meta = SettingsRegistry::new(self).meta(row);
        let section = meta.category.label(self.locale).to_lowercase();
        let section_en = meta.category.label(Locale::En).to_lowercase();
        let category = ConfigCategory::for_row(row);
        let category_label = category.label(self.locale).to_lowercase();
        let category_en = category.label(Locale::En).to_lowercase();
        let label = config_label_for_key_for_locale(self.locale, &row.key).to_lowercase();
        let key = row.key.to_lowercase();
        let raw_value = row.value.to_lowercase();
        let value = self.row_display_value(row).to_lowercase();
        let scope = row.scope.label(self.locale).to_lowercase();
        let scope_en = row.scope.label(Locale::En).to_lowercase();
        let hint = config_hint_for_key(self.locale, &row.key).to_lowercase();

        let explanation_terms = if row.key == "sandbox_details" {
            "sandbox filesystem unenforced isolation bubblewrap bwrap doctor"
        } else {
            ""
        };
        terms.iter().all(|term| {
            explanation_terms.contains(term)
                || section.contains(term)
                || section_en.contains(term)
                || category_label.contains(term)
                || category_en.contains(term)
                || label.contains(term)
                || key.contains(term)
                || raw_value.contains(term)
                || value.contains(term)
                || scope.contains(term)
                || scope_en.contains(term)
                || hint.contains(term)
        })
    }

    fn matching_row_indices(&self) -> Vec<usize> {
        let filtering = !self.filter.is_empty();
        let terms = self.filter_terms();
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(idx, row)| {
                (self.row_matches_filter(row, &terms) && (filtering || self.category.contains(row)))
                    .then_some(idx)
            })
            .collect()
    }

    fn visible_items(&self) -> Vec<ConfigListItem> {
        let mut items = Vec::new();
        let mut current_section = None;
        let filtering = !self.filter.is_empty();

        let terms = self.filter_terms();
        for (idx, row) in self.rows.iter().enumerate() {
            if !self.row_matches_filter(row, &terms) {
                continue;
            }
            // The rail category filters rows unless the user is searching.
            if !filtering && !self.category.contains(row) {
                continue;
            }

            if current_section != Some(row.section()) {
                current_section = Some(row.section());
                items.push(ConfigListItem::Section(row.section()));
            }
            items.push(ConfigListItem::Row(idx));
        }

        items
    }

    fn select_first_visible_row(&mut self) {
        if let Some(idx) = self
            .visible_items()
            .into_iter()
            .find_map(|item| match item {
                ConfigListItem::Row(i) => Some(i),
                ConfigListItem::Section(_) => None,
            })
        {
            self.selected = idx;
            self.scroll = 0;
        }
        self.last_mouse_selected = None;
        self.clear_hover();
    }

    fn key_column_width(&self) -> usize {
        self.rows
            .iter()
            .map(|row| {
                let label = config_label_for_key_for_locale(self.locale, &row.key);
                UnicodeWidthStr::width(label.as_str())
            })
            .max()
            .unwrap_or(CONFIG_MIN_KEY_COLUMN_WIDTH)
            .max(CONFIG_MIN_KEY_COLUMN_WIDTH)
    }

    fn table_column_widths(&self, content_width: usize) -> (usize, usize, usize) {
        // The affordance glyph is the interaction; the scope badge is
        // secondary and sheds first so narrow lists keep a readable value.
        let scope_width = if content_width >= CONFIG_SCOPE_BADGE_MIN_WIDTH {
            CONFIG_SCOPE_COLUMN_WIDTH
        } else {
            0
        };
        let fixed_width = CONFIG_ROW_PREFIX_WIDTH
            + CONFIG_COLUMN_GAPS_WIDTH
            + CONFIG_AFFORDANCE_COLUMN_WIDTH
            + scope_width;
        let key_value_width = content_width.saturating_sub(fixed_width);
        let desired_key_width = self.key_column_width();

        if key_value_width == 0 {
            return (0, 0, scope_width);
        }

        let minimum_key_width = CONFIG_MIN_KEY_COLUMN_WIDTH.min(key_value_width);
        let key_width = desired_key_width
            .min(key_value_width.saturating_sub(CONFIG_MIN_VALUE_COLUMN_WIDTH))
            .max(minimum_key_width);
        let value_width = key_value_width
            .saturating_sub(key_width)
            .min(CONFIG_VALUE_COLUMN_WIDTH);

        (key_width, value_width, scope_width)
    }

    fn selected_row_index(&self) -> Option<usize> {
        let selected = self.selected;
        self.matching_row_indices()
            .into_iter()
            .any(|idx| idx == selected)
            .then_some(selected)
    }

    fn selected_display_position(&self, items: &[ConfigListItem]) -> Option<usize> {
        items
            .iter()
            .position(|item| matches!(item, ConfigListItem::Row(idx) if *idx == self.selected))
    }

    fn sync_selection_to_filter(&mut self) {
        let matches = self.matching_row_indices();
        if matches.is_empty() {
            self.selected = 0;
            self.scroll = 0;
            return;
        }

        if !matches.contains(&self.selected) {
            self.selected = matches[0];
        }
    }

    /// Clear every hover tint: selection moves, filters, and scrolls can all
    /// shift painted rows out from under a stationary pointer.
    fn clear_hover(&mut self) {
        self.hovered_row = None;
        self.hovered_rail = None;
        self.hovered_nav = None;
        self.hovered_editor = None;
        self.hovered_choice = None;
    }

    /// Hover pass: tint whatever the pointer is over using the shared hover
    /// style. Hover never moves the keyboard selection and never activates.
    fn track_hover(&mut self, mouse: MouseEvent) {
        let position = Position::new(mouse.column, mouse.row);
        if self.editing.is_some() {
            self.hovered_choice = self
                .last_choice_hitboxes
                .borrow()
                .iter()
                .find_map(|(rect, choice)| rect.contains(position).then_some(*choice));
            self.hovered_editor = self
                .last_editor_controls
                .borrow()
                .iter()
                .find_map(|(rect, control)| rect.contains(position).then_some(*control));
            self.hovered_row = None;
            self.hovered_rail = None;
            self.hovered_nav = None;
            return;
        }
        self.hovered_row = self
            .last_row_hitboxes
            .borrow()
            .iter()
            .find_map(|(rect, row_idx)| rect.contains(position).then_some(*row_idx));
        self.hovered_rail = self
            .last_rail_hitboxes
            .borrow()
            .iter()
            .find_map(|(rect, category)| rect.contains(position).then_some(*category));
        self.hovered_nav = self
            .last_nav_controls
            .borrow()
            .iter()
            .find_map(|(rect, step)| rect.contains(position).then_some(*step));
        self.hovered_editor = None;
        self.hovered_choice = None;
    }

    fn update_filter(&mut self, update: impl FnOnce(&mut String)) {
        update(&mut self.filter);
        self.status = None;
        self.last_mouse_selected = None;
        self.clear_hover();
        self.sync_selection_to_filter();
        self.adjust_scroll(self.visible_rows_cached());
    }

    fn adjust_scroll(&mut self, visible_rows: usize) {
        self.sync_selection_to_filter();

        let items = self.visible_items();
        if items.is_empty() {
            self.scroll = 0;
            return;
        }

        let visible_rows = visible_rows.max(1);
        let max_scroll = items.len().saturating_sub(visible_rows);
        self.scroll = self.scroll.min(max_scroll);

        let Some(selected_pos) = self.selected_display_position(&items) else {
            self.scroll = 0;
            return;
        };

        if selected_pos < self.scroll {
            self.scroll = selected_pos;
        }

        if selected_pos >= self.scroll + visible_rows {
            self.scroll = selected_pos.saturating_sub(visible_rows.saturating_sub(1));
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let matches = self.matching_row_indices();
        if matches.is_empty() {
            return;
        }

        let current = matches
            .iter()
            .position(|idx| *idx == self.selected)
            .unwrap_or(0);
        let next = crate::tui::list_nav::wrap_index(current, matches.len(), delta);

        self.selected = matches[next];
        self.clear_hover();
        let visible_rows = self.visible_rows_cached();
        self.adjust_scroll(visible_rows);
    }

    fn toggle_selected_boolean(&self) -> Option<ViewAction> {
        let row = self.rows.get(self.selected_row_index()?)?;
        if SettingsRegistry::new(self).meta(row).kind != SettingKind::Boolean {
            return None;
        }
        let value = if canonical_config_choice(&row.key, row.edit_value()) == "true" {
            "false"
        } else {
            "true"
        };
        Some(ViewAction::Emit(ViewEvent::ConfigUpdated {
            key: row.key.clone(),
            value: value.to_string(),
            persist: row.scope.persist(),
        }))
    }

    fn open_selected_catalog_picker(&self) -> Option<ViewAction> {
        let row = self.rows.get(self.selected_row_index()?)?;
        if !row.editable {
            return None;
        }
        let (command, _) = row.facts.command?;
        if row.key == "sandbox_details" {
            return Some(ViewAction::Emit(ViewEvent::ExecutePanelCommand {
                command: command.to_string(),
                pager_title: Some(config_label_for_key_for_locale(self.locale, &row.key)),
            }));
        }
        Some(ViewAction::Emit(ViewEvent::CommandPaletteSelected {
            action: CommandPaletteAction::ExecuteCommand {
                command: command.to_string(),
            },
        }))
    }

    fn move_choice(&mut self, delta: isize) {
        let Some(edit) = self.editing.as_mut() else {
            return;
        };
        let Some(choices) = edit.choices.as_ref() else {
            return;
        };
        let max = choices.len().saturating_sub(1);
        edit.selected_choice = if delta.is_negative() {
            edit.selected_choice.saturating_sub(delta.unsigned_abs())
        } else {
            (edit.selected_choice + delta as usize).min(max)
        };
        self.hovered_choice = None;
    }

    /// Live-preview the edited choice when the edited key is the theme:
    /// highlighting a theme row applies it session-only (`persist:false`)
    /// so the surface behind the editor repaints immediately, while only
    /// Enter/Apply persists. Other keys preview nothing.
    fn preview_edited_choice(&self) -> ViewAction {
        let Some(edit) = self.editing.as_ref() else {
            return ViewAction::None;
        };
        if edit.key != "theme" {
            return ViewAction::None;
        }
        let Some(value) = edit
            .choices
            .as_ref()
            .and_then(|choices| choices.get(edit.selected_choice).cloned())
        else {
            return ViewAction::None;
        };
        ViewAction::Emit(ViewEvent::ConfigUpdated {
            key: edit.key.clone(),
            value,
            persist: false,
        })
    }

    /// Leave the editor without applying (Esc or the Cancel control). When
    /// the theme highlight moved, the live surface already previews the
    /// highlighted theme, so Esc reverts it to the exact value the editor
    /// opened with (session-only, mirroring the `/theme` picker rollback).
    fn cancel_edit(&mut self) -> ViewAction {
        let revert = self
            .editing
            .as_ref()
            .filter(|edit| edit.key == "theme")
            .and_then(|edit| {
                let highlighted = edit.choices.as_ref()?.get(edit.selected_choice)?;
                (canonical_config_choice(&edit.key, highlighted)
                    != canonical_config_choice(&edit.key, &edit.original_value))
                .then(|| ViewEvent::ConfigUpdated {
                    key: edit.key.clone(),
                    value: edit.original_value.clone(),
                    persist: false,
                })
            });
        self.editing = None;
        self.status = Some(self.tr(MessageId::ConfigEditCancelled).to_string());
        self.last_mouse_selected = None;
        self.clear_hover();
        revert.map_or(ViewAction::None, ViewAction::Emit)
    }

    /// Hover-follow for the editor's choice rows (the global hover rule):
    /// the pointer highlights the hovered row, painted with the shared
    /// selected-row style. On the theme editor it also live-previews,
    /// exactly like ↑/↓.
    fn hover_edited_choice(&mut self, mouse: MouseEvent) -> ViewAction {
        let position = Position::new(mouse.column, mouse.row);
        let hovered = self
            .last_choice_hitboxes
            .borrow()
            .iter()
            .find_map(|(rect, choice)| rect.contains(position).then_some(*choice));
        let Some(hovered) = hovered else {
            return ViewAction::None;
        };
        let changed = match self.editing.as_mut() {
            Some(edit) if edit.selected_choice != hovered => {
                edit.selected_choice = hovered;
                true
            }
            _ => false,
        };
        if changed {
            self.preview_edited_choice()
        } else {
            ViewAction::None
        }
    }

    /// Apply the editor's value (Enter or the Apply control): the selected
    /// choice, or the trimmed text buffer.
    fn commit_edit(&mut self) -> ViewAction {
        let Some(edit) = self.editing.take() else {
            return ViewAction::None;
        };
        self.pending_commit = Some(edit.clone());
        self.last_mouse_selected = None;
        self.clear_hover();
        let value = match edit.choices.as_ref() {
            Some(choices) => match choices.get(edit.selected_choice).cloned() {
                Some(value) => value,
                None => return ViewAction::None,
            },
            None => edit.buffer.iter().collect::<String>().trim().to_string(),
        };
        ViewAction::Emit(ViewEvent::ConfigUpdated {
            key: edit.key,
            value,
            persist: edit.scope.persist(),
        })
    }

    fn handle_choice_key(&mut self, key: KeyEvent) -> ViewAction {
        match key.code {
            KeyCode::Esc => self.cancel_edit(),
            KeyCode::Enter => self.commit_edit(),
            KeyCode::Up | KeyCode::Left | KeyCode::Char('k') => {
                self.move_choice(-1);
                self.preview_edited_choice()
            }
            KeyCode::Down | KeyCode::Right | KeyCode::Char('j') => {
                self.move_choice(1);
                self.preview_edited_choice()
            }
            KeyCode::PageUp => {
                self.move_choice(-5);
                self.preview_edited_choice()
            }
            KeyCode::PageDown => {
                self.move_choice(5);
                self.preview_edited_choice()
            }
            KeyCode::Home => {
                if let Some(edit) = self.editing.as_mut() {
                    edit.selected_choice = 0;
                }
                self.preview_edited_choice()
            }
            KeyCode::End => {
                if let Some(edit) = self.editing.as_mut()
                    && let Some(choices) = edit.choices.as_ref()
                {
                    edit.selected_choice = choices.len().saturating_sub(1);
                }
                self.preview_edited_choice()
            }
            KeyCode::Char(digit @ '1'..='9') => {
                if let Some(edit) = self.editing.as_mut()
                    && let Some(choices) = edit.choices.as_ref()
                {
                    let index = digit as usize - '1' as usize;
                    if index < choices.len() {
                        edit.selected_choice = index;
                    }
                }
                self.preview_edited_choice()
            }
            KeyCode::Char(' ') => {
                self.move_choice(1);
                self.preview_edited_choice()
            }
            _ => ViewAction::None,
        }
    }

    fn handle_editing_key(&mut self, key: KeyEvent) -> ViewAction {
        if self
            .editing
            .as_ref()
            .is_some_and(|edit| edit.choices.is_some())
        {
            return self.handle_choice_key(key);
        }
        match key.code {
            KeyCode::Esc => self.cancel_edit(),
            KeyCode::Enter => self.commit_edit(),
            KeyCode::Backspace => {
                if let Some(edit) = self.editing.as_mut() {
                    if edit.select_all {
                        edit.buffer.clear();
                        edit.cursor = 0;
                        edit.select_all = false;
                    } else if edit.cursor > 0 {
                        edit.cursor = edit.cursor.saturating_sub(1);
                        edit.buffer.remove(edit.cursor);
                    }
                }
                ViewAction::None
            }
            KeyCode::Delete => {
                if let Some(edit) = self.editing.as_mut() {
                    if edit.select_all {
                        edit.buffer.clear();
                        edit.cursor = 0;
                        edit.select_all = false;
                    } else if edit.cursor < edit.buffer.len() {
                        edit.buffer.remove(edit.cursor);
                    }
                }
                ViewAction::None
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(edit) = self.editing.as_mut() {
                    edit.buffer.clear();
                    edit.cursor = 0;
                    edit.select_all = false;
                }
                ViewAction::None
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(edit) = self.editing.as_mut() {
                    edit.cursor = edit.buffer.len();
                    edit.select_all = true;
                }
                ViewAction::None
            }
            KeyCode::Left => {
                if let Some(edit) = self.editing.as_mut() {
                    if edit.select_all {
                        edit.cursor = 0;
                        edit.select_all = false;
                    } else {
                        edit.cursor = edit.cursor.saturating_sub(1);
                    }
                }
                ViewAction::None
            }
            KeyCode::Right => {
                if let Some(edit) = self.editing.as_mut() {
                    if edit.select_all {
                        edit.cursor = edit.buffer.len();
                        edit.select_all = false;
                    } else {
                        edit.cursor = (edit.cursor + 1).min(edit.buffer.len());
                    }
                }
                ViewAction::None
            }
            KeyCode::Home => {
                if let Some(edit) = self.editing.as_mut() {
                    edit.cursor = 0;
                    edit.select_all = false;
                }
                ViewAction::None
            }
            KeyCode::End => {
                if let Some(edit) = self.editing.as_mut() {
                    edit.cursor = edit.buffer.len();
                    edit.select_all = false;
                }
                ViewAction::None
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && !ch.is_control() =>
            {
                if let Some(edit) = self.editing.as_mut() {
                    if edit.select_all {
                        edit.buffer.clear();
                        edit.cursor = 0;
                        edit.select_all = false;
                    }
                    edit.buffer.insert(edit.cursor, ch);
                    edit.cursor += 1;
                }
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn start_edit(&mut self) {
        let Some(row_idx) = self.selected_row_index() else {
            return;
        };
        let Some(row) = self.rows.get(row_idx) else {
            return;
        };
        let key = row.key.clone();
        let original_value = row.edit_value().to_string();
        let initial_value = match config_default_placeholder_message(&key) {
            Some(message_id)
                if original_value == tr(self.locale, message_id)
                    || original_value == tr(Locale::En, message_id) =>
            {
                String::new()
            }
            _ => original_value.clone(),
        };

        let meta = SettingsRegistry::new(self).meta(row);
        let choices = meta.choices;
        let selected_choice = choices
            .as_ref()
            .and_then(|choices| {
                let current = canonical_config_choice(&key, &initial_value);
                choices
                    .iter()
                    .position(|choice| canonical_config_choice(&key, choice) == current)
            })
            .unwrap_or(0);
        let buffer: Vec<char> = initial_value.chars().collect();
        self.last_mouse_selected = None;
        self.editing = Some(ConfigEdit {
            key,
            original_value,
            cursor: buffer.len(),
            buffer,
            select_all: true,
            scope: row.scope,
            choices,
            selected_choice,
        });
        self.status = None;
    }

    fn clear_filter(&mut self) {
        if self.filter.is_empty() {
            return;
        }

        self.update_filter(|filter| filter.clear());
    }

    fn row_display_value(&self, row: &ConfigRow) -> String {
        if row.key.starts_with("notifications.") {
            return config_choice_label(self.locale, &row.key, row.edit_value());
        }
        // The effective lane is only ever an explicit `App` observation carried
        // on the row's typed facts; a persisted value never stands in for it.
        let effective = row.facts.effective.as_deref();
        if row.key == "cost_currency"
            && row.scope == ConfigScope::Saved
            && let Some(effective_currency) = effective
        {
            let saved_cost_currency = crate::pricing::CostCurrency::from_setting(&row.value);
            let effective_cost_currency =
                crate::pricing::CostCurrency::from_setting(effective_currency);
            if saved_cost_currency != effective_cost_currency {
                return format!(
                    "{}{}",
                    row.value,
                    self.tr(MessageId::ConfigRowEffective)
                        .replace("{currency}", effective_currency)
                );
            }
        }

        let runtime_value = effective.and_then(|value| value.parse::<bool>().ok());
        if let Some(runtime_value) = runtime_value
            && row.value.parse::<bool>().ok() != Some(runtime_value)
        {
            let saved = config_choice_label(
                self.locale,
                &row.key,
                &canonical_config_choice(&row.key, &row.value),
            );
            let effective = config_choice_label(self.locale, &row.key, &runtime_value.to_string());
            return format!(
                "{}{}",
                saved,
                self.tr(MessageId::ConfigRowEffective)
                    .replace("{currency}", &effective)
            );
        }

        // Preserve the exact saved currency alias in the table (for example
        // `rmb`) while the chooser highlights its canonical `cny` option.
        if row.key == "cost_currency" {
            return row.value.clone();
        }

        if SettingsRegistry::new(self).meta(row).choices.is_some() {
            if config_default_placeholder_message(&row.key).is_some_and(|message_id| {
                row.value == tr(self.locale, message_id) || row.value == tr(Locale::En, message_id)
            }) {
                return self.tr(MessageId::ConfigValueProviderDefault).into_owned();
            }
            let canonical = canonical_config_choice(&row.key, &row.value);
            return config_choice_label(self.locale, &row.key, &canonical);
        }

        row.value.clone()
    }
}

fn config_base_url_row_key(provider: ProviderKind) -> &'static str {
    if matches!(provider, ProviderKind::Deepseek) {
        "base_url"
    } else {
        "provider_url"
    }
}

fn config_base_url_row_value(app: &App) -> String {
    app.active_route_base_url.clone()
}

fn cost_currency_config_value(app: &App) -> String {
    match app.cost_currency {
        crate::pricing::CostCurrency::Usd => "usd",
        crate::pricing::CostCurrency::Cny => "cny",
    }
    .to_string()
}

fn experimental_config_rows(config: &Config) -> Vec<ConfigRow> {
    let features = config.features();
    let configured = config.features.as_ref().map(|table| &table.entries);
    let mut rows = Vec::new();

    for spec in FEATURES
        .iter()
        .filter(|spec| matches!(spec.stage, Stage::Experimental | Stage::Beta))
    {
        let effective = features.enabled(spec.id);
        let configured_value = configured
            .and_then(|entries| entries.get(spec.key))
            .copied();
        rows.push(ConfigRow {
            key: format!("features.{}", spec.key),
            value: experimental_feature_value(
                effective,
                spec.default_enabled,
                configured_value.is_some(),
            ),
            editable: false,
            scope: ConfigScope::Saved,
            facts: ConfigRowFacts::read_only_setting(SettingAuthority::WorkspaceConfiguration),
        });
    }

    rows.push(ConfigRow {
        key: "goal_command".to_string(),
        value:
            "/goal sets session objectives with optional token budgets; state shows in Work context"
                .to_string(),
        editable: false,
        scope: ConfigScope::Saved,
        facts: ConfigRowFacts::diagnostic(SettingAuthority::Session),
    });
    rows.push(ConfigRow {
        // Workflow orchestration is its own section, not a Fleet concern.
        key: "workflow".to_string(),
        value:
            "/workflow runs scripted fan-out/fan-in operations with run cards and cancel support"
                .to_string(),
        editable: false,
        scope: ConfigScope::Saved,
        facts: ConfigRowFacts::diagnostic(SettingAuthority::Session),
    });

    rows
}

fn experimental_feature_value(effective: bool, default_enabled: bool, configured: bool) -> String {
    let state = if effective { "enabled" } else { "disabled" };
    let default_state = if default_enabled {
        "enabled"
    } else {
        "disabled"
    };
    if configured {
        format!("{state} (configured; default {default_state})")
    } else {
        format!("{state} (default {default_state})")
    }
}

/// Localized label for a setting key.
///
/// The schema names the string; the locale pack owns the text. A setting
/// declared without a label message humanizes its key, and a `features.*` key
/// wears the localized feature prefix.
fn config_label_for_key_for_locale(locale: Locale, key: &str) -> String {
    let declared = codewhale_config::setting(key)
        .and_then(|def| def.ui.as_ref())
        .map(|ui| ui.label)
        .unwrap_or("");
    if !declared.is_empty() {
        return tr_key(locale, declared).to_string();
    }
    let humanized = humanize_config_key(key.strip_prefix("features.").unwrap_or(key));
    if key.starts_with("features.") {
        tr(locale, MessageId::ConfigLabelFeaturePrefix).replace("{name}", &humanized)
    } else {
        humanized
    }
}

#[cfg(test)]
fn config_label_for_key(key: &str) -> String {
    config_label_for_key_for_locale(Locale::En, key)
}

fn humanize_config_key(key: &str) -> String {
    key.split(['.', '_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            let Some(first) = chars.next() else {
                return String::new();
            };
            let mut word = first.to_uppercase().collect::<String>();
            word.push_str(chars.as_str());
            word
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Localized description for a setting key.
///
/// Theme and locale describe themselves with their shipped value lists (value
/// lists, not prose, so they cannot go stale); every other sentence is the
/// message the schema names.
fn config_hint_for_key(locale: Locale, key: &str) -> Cow<'static, str> {
    match key {
        "theme" => {
            static THEME_HINT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
            return Cow::Borrowed(THEME_HINT.get_or_init(|| {
                codewhale_palette::SELECTABLE_THEMES
                    .iter()
                    .map(|id| id.name())
                    .collect::<Vec<_>>()
                    .join(" | ")
            }));
        }
        "locale" => {
            static LOCALE_HINT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
            return Cow::Borrowed(
                LOCALE_HINT.get_or_init(|| codewhale_localization::configured_locale_values(" | ")),
            );
        }
        _ => {}
    }
    let declared = codewhale_config::setting(key)
        .and_then(|def| def.ui.as_ref())
        .map(|ui| ui.description)
        .unwrap_or("");
    if declared.is_empty() {
        return Cow::Borrowed("");
    }
    tr_key(locale, declared)
}

fn config_default_placeholder_message(key: &str) -> Option<MessageId> {
    match key {
        "default_model" | "background_color" => Some(MessageId::ConfigDefaultValue),
        "reasoning_effort" => Some(MessageId::ConfigDefaultReasoning),
        _ => None,
    }
}

fn config_boolean_key(key: &str) -> bool {
    codewhale_config::setting(key).is_some_and(|def| def.is_bool())
}

fn config_integer_key(key: &str) -> bool {
    codewhale_config::setting(key).is_some_and(|def| def.is_int())
}

/// Selectable values for a key.
///
/// Two settings take their values from a live registry instead of the schema:
/// the shipped palettes and the shipped locale packs. `reasoning_effort` is a
/// third, and `SettingsRegistry::reasoning_effort_choices` owns it because the
/// answer depends on the active route, not just the provider. Everything else
/// is the declared value set.
fn config_choice_values(key: &str) -> Option<Vec<String>> {
    match key {
        "theme" => {
            return Some(
                codewhale_palette::SELECTABLE_THEMES
                    .iter()
                    .map(|id| id.name().to_string())
                    .collect(),
            );
        }
        "locale" => {
            let mut values = vec!["auto".to_string()];
            values.extend(
                Locale::shipped()
                    .iter()
                    .map(|locale| locale.tag().to_string()),
            );
            return Some(values);
        }
        "reasoning_effort" => {
            // Settings-canonical vocabulary; the live settings screen uses
            // `reasoning_effort_choices()` which narrows this by route/provider.
            return Some(vec![
                "default".to_string(),
                "off".to_string(),
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
                "xhigh".to_string(),
                "auto".to_string(),
                "ultra".to_string(),
                "max".to_string(),
            ]);
        }
        _ => {}
    }
    codewhale_config::setting(key)
        .and_then(|def| def.values())
        .map(|values| values.into_iter().map(str::to_string).collect())
}

fn canonical_config_choice(key: &str, value: &str) -> String {
    let normalized = value.trim().to_ascii_lowercase().replace([' ', '_'], "-");
    match key {
        key if config_boolean_key(key) => match normalized.as_str() {
            "true" | "on" | "yes" | "1" | "enabled" => "true".to_string(),
            _ => "false".to_string(),
        },
        "approval_mode" | "permission_posture" => match normalized.as_str() {
            "ask" | "suggest" | "on-request" | "untrusted" => "ask".to_string(),
            "auto" | "auto-review" => "auto-review".to_string(),
            "full" | "full-access" | "bypass" | "yolo" => "full-access".to_string(),
            _ => normalized,
        },
        "approval_policy" => match normalized.as_str() {
            "ask" | "suggest" | "on-request" | "untrusted" => "ask".to_string(),
            "auto" | "auto-review" => "auto-review".to_string(),
            "full" | "full-access" | "bypass" | "yolo" => "full-access".to_string(),
            "never" | "deny" => "never".to_string(),
            _ => normalized,
        },
        "reasoning_effort" => {
            if matches!(normalized.as_str(), "" | "(default)" | "config-default") {
                "default".to_string()
            } else if normalized == "max" && value.trim().eq_ignore_ascii_case("xhigh") {
                "xhigh".to_string()
            } else {
                normalized
            }
        }
        "cost_currency" => match normalized.as_str() {
            "rmb" | "yuan" | "cny" => "cny".to_string(),
            _ => "usd".to_string(),
        },
        "default_mode" => match normalized.as_str() {
            "plan" => "plan".to_string(),
            "operate" | "operation" | "ops" => "operate".to_string(),
            _ => "agent".to_string(),
        },
        "locale" => normalize_configured_locale(value)
            .unwrap_or(value)
            .to_string(),
        _ => normalized,
    }
}

/// Localized label for one value of a setting.
///
/// The schema declares per-value labels; a boolean with none uses the shared
/// on/off pair, and any other undeclared value shows itself.
fn config_choice_label(locale: Locale, key: &str, value: &str) -> String {
    // The runtime "default" choice for reasoning_effort is the unset sentinel;
    // keep its localized placeholder label instead of showing the raw word.
    if key == "reasoning_effort" && value == "default" {
        return tr(locale, MessageId::ConfigDefaultReasoning).into_owned();
    }
    let declared = codewhale_config::setting(key);
    let message = declared
        .and_then(|def| def.option(value))
        .map(|option| option.label)
        .filter(|label| !label.is_empty());
    let label = match message {
        Some(message) => tr_key(locale, message).into_owned(),
        None if declared.is_some_and(|def| def.is_bool()) => match value {
            "true" => tr(locale, MessageId::ConfigValueOn).into_owned(),
            "false" => tr(locale, MessageId::ConfigValueOff).into_owned(),
            other => other.to_string(),
        },
        None => value.to_string(),
    };

    if key == "locale" && configured_locale_is_partial_pack(value) {
        format!(
            "{label} ({})",
            tr(locale, MessageId::ConfigLocalePartialBadge)
        )
    } else {
        label
    }
}

/// Localized one-line detail for one value of a setting.
fn config_choice_detail(locale: Locale, key: &str, value: &str) -> Cow<'static, str> {
    if key == "locale" && configured_locale_is_partial_pack(value) {
        return tr(locale, MessageId::ConfigLocalePartialDetail);
    }
    let declared = codewhale_config::setting(key)
        .and_then(|def| def.option(value))
        .map(|option| option.description)
        .filter(|description| !description.is_empty());
    match declared {
        Some(message) => tr_key(locale, message),
        None => Cow::Borrowed(""),
    }
}

fn render_config_editor_value_line(
    edit: &ConfigEdit,
    locale: Locale,
) -> ratatui::text::Line<'static> {
    use ratatui::{
        style::Style,
        text::{Line, Span},
    };

    let mut spans = Vec::new();
    spans.push(Span::styled(
        tr(locale, MessageId::ConfigEditNewLabel),
        Style::default().fg(palette::TEXT_MUTED),
    ));

    let cursor_style = Style::default()
        .fg(palette::WHALE_BG)
        .bg(palette::WHALE_ACTION)
        .bold();
    let selected_style = Style::default()
        .fg(palette::SELECTION_TEXT)
        .bg(palette::SELECTION_BG);

    if edit.select_all && !edit.buffer.is_empty() {
        let text = edit.buffer.iter().collect::<String>();
        spans.push(Span::styled(text, selected_style));
        spans.push(Span::styled(" ", cursor_style));
        return Line::from(spans);
    }

    let before = edit.buffer.iter().take(edit.cursor).collect::<String>();
    spans.push(Span::raw(before));
    if edit.cursor < edit.buffer.len() {
        let ch = edit.buffer[edit.cursor];
        spans.push(Span::styled(ch.to_string(), cursor_style));
        let after = edit
            .buffer
            .iter()
            .skip(edit.cursor.saturating_add(1))
            .collect::<String>();
        spans.push(Span::raw(after));
    } else {
        spans.push(Span::styled(" ", cursor_style));
    }

    Line::from(spans)
}

impl ModalView for ConfigView {
    fn kind(&self) -> ModalKind {
        ModalKind::Config
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        // Any key is a state change: a following single click must select,
        // never activate, whatever the pointer touched before.
        self.last_mouse_selected = None;
        if self.editing.is_some() {
            return self.handle_editing_key(key);
        }
        // A status line ("Edit cancelled", …) is transient: navigation gives
        // the row back to its activation copy.
        self.status = None;

        match key.code {
            KeyCode::Esc => {
                if self.filter.is_empty() {
                    ViewAction::Close
                } else {
                    self.clear_filter();
                    ViewAction::None
                }
            }
            KeyCode::Tab | KeyCode::Right
                if !key.modifiers.contains(KeyModifiers::SHIFT) && self.filter.is_empty() =>
            {
                self.category = self.category.next();
                self.select_first_visible_row();
                ViewAction::None
            }
            KeyCode::BackTab | KeyCode::Tab | KeyCode::Left if self.filter.is_empty() => {
                self.category = self.category.prev();
                self.select_first_visible_row();
                ViewAction::None
            }
            KeyCode::Up => {
                self.move_selection(-1);
                ViewAction::None
            }
            KeyCode::Down => {
                self.move_selection(1);
                ViewAction::None
            }
            KeyCode::PageUp => {
                self.move_selection(-5);
                ViewAction::None
            }
            KeyCode::PageDown => {
                self.move_selection(5);
                ViewAction::None
            }
            KeyCode::Backspace => {
                if !self.filter.is_empty() {
                    self.update_filter(|filter| {
                        filter.pop();
                    });
                }
                ViewAction::None
            }
            // Ctrl+H is the legacy ASCII backspace many terminals emit.
            KeyCode::Char('h')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                if !self.filter.is_empty() {
                    self.update_filter(|filter| {
                        filter.pop();
                    });
                }
                ViewAction::None
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.clear_filter();
                ViewAction::None
            }
            KeyCode::Enter => {
                if self
                    .selected_row_index()
                    .and_then(|idx| self.rows.get(idx))
                    .is_some_and(|row| row.editable)
                {
                    if let Some(action) = self.open_selected_catalog_picker() {
                        return action;
                    }
                    if let Some(action) = self.toggle_selected_boolean() {
                        return action;
                    }
                    self.start_edit();
                }
                ViewAction::None
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && !ch.is_control() =>
            {
                self.update_filter(|filter| filter.push(ch));
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        if matches!(mouse.kind, MouseEventKind::Moved) {
            let has_choices = self
                .editing
                .as_ref()
                .is_some_and(|edit| edit.choices.is_some());
            if has_choices {
                return self.hover_edited_choice(mouse);
            }
            self.track_hover(mouse);
            return ViewAction::None;
        }
        if self.editing.is_some() {
            let has_choices = self
                .editing
                .as_ref()
                .is_some_and(|edit| edit.choices.is_some());
            match mouse.kind {
                MouseEventKind::ScrollUp if has_choices => {
                    self.move_choice(-1);
                    return self.preview_edited_choice();
                }
                MouseEventKind::ScrollDown if has_choices => {
                    self.move_choice(1);
                    return self.preview_edited_choice();
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    let position = Position::new(mouse.column, mouse.row);
                    let control = self
                        .last_editor_controls
                        .borrow()
                        .iter()
                        .find_map(|(rect, control)| rect.contains(position).then_some(*control));
                    match control {
                        Some(EditorControl::Apply) => return self.commit_edit(),
                        Some(EditorControl::Cancel) => return self.cancel_edit(),
                        None => {}
                    }
                    let choice = self
                        .last_choice_hitboxes
                        .borrow()
                        .iter()
                        .find_map(|(rect, choice)| rect.contains(position).then_some(*choice));
                    let picked = match (choice, self.editing.as_mut()) {
                        (Some(choice), Some(edit)) => {
                            edit.selected_choice = choice;
                            true
                        }
                        _ => false,
                    };
                    if picked {
                        return self.preview_edited_choice();
                    }
                }
                _ => {}
            }
            return ViewAction::None;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.move_selection(-3);
                self.last_mouse_selected = None;
                self.clear_hover();
                return ViewAction::None;
            }
            MouseEventKind::ScrollDown => {
                self.move_selection(3);
                self.last_mouse_selected = None;
                self.clear_hover();
                return ViewAction::None;
            }
            _ => {}
        }
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return ViewAction::None;
        }

        let position = Position::new(mouse.column, mouse.row);
        let clicked_category = self
            .last_rail_hitboxes
            .borrow()
            .iter()
            .find_map(|(rect, category)| rect.contains(position).then_some(*category));
        let stepped = self
            .last_nav_controls
            .borrow()
            .iter()
            .find_map(|(rect, step)| rect.contains(position).then_some(*step))
            .map(|step| match step {
                NavStep::Previous => self.category.prev(),
                NavStep::Next => self.category.next(),
            });
        if let Some(category) = clicked_category.or(stepped) {
            // A category click (or an overflow marker) is an explicit
            // navigation: it leaves search so the list shows exactly that
            // category, never a stale filtered mix.
            self.clear_filter();
            self.status = None;
            if self.category != category {
                self.category = category;
                self.select_first_visible_row();
            }
            self.last_mouse_selected = None;
            return ViewAction::None;
        }

        // Only the painted cells of a list row select it; the rail, dividers,
        // detail pane, status row, and footer never do.
        let selected = self
            .last_row_hitboxes
            .borrow()
            .iter()
            .find_map(|(rect, row_idx)| rect.contains(position).then_some(*row_idx));
        if let Some(row_idx) = selected {
            let activate = self.last_mouse_selected == Some(row_idx) && self.selected == row_idx;
            self.selected = row_idx;
            self.status = None;
            self.adjust_scroll(self.visible_rows_cached());
            self.last_mouse_selected = Some(row_idx);
            if activate && self.rows.get(row_idx).is_some_and(|row| row.editable) {
                if let Some(action) = self.open_selected_catalog_picker() {
                    return action;
                }
                if let Some(action) = self.toggle_selected_boolean() {
                    return action;
                }
                self.start_edit();
            }
        }
        ViewAction::None
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        use ratatui::{
            style::Style,
            text::{Line, Span},
            widgets::{Paragraph, Widget},
        };

        let inner =
            render_underwater_surface(area, buf, self.tr(MessageId::ConfigModalTitle).to_string());
        let (lines, footer) = if let Some(edit) = self.editing.as_ref() {
            *self.last_choice_hitboxes.borrow_mut() = Vec::new();
            *self.last_editor_controls.borrow_mut() = Vec::new();
            *self.last_rail_hitboxes.borrow_mut() = Vec::new();
            *self.last_nav_controls.borrow_mut() = Vec::new();
            let footer_text = if edit.choices.is_some() {
                if inner.width < 56 || inner.height <= 8 {
                    self.tr(MessageId::ConfigChoiceFooterCompact).to_string()
                } else {
                    self.tr(MessageId::ConfigChoiceFooter).to_string()
                }
            } else {
                self.tr(MessageId::ConfigEditFooter).to_string()
            };
            let reserved_footer_lines =
                wrapped_footer_lines(&footer_text, inner.width, Style::default()).len();
            // The clickable Apply / Cancel controls always own the last body
            // row above the footer.
            const CONTROL_ROWS: usize = 1;
            // Spacer rows are secondary chrome: give them up before the
            // editable value line falls below the wrapped footer on compact
            // terminals (#40x12).
            let body_rows =
                usize::from(inner.height).saturating_sub(reserved_footer_lines + CONTROL_ROWS);
            // The expanded header costs six rows before the options. Reserve
            // at least three choices plus their detail before adding spacers;
            // a slightly taller compact shell must not show fewer options.
            let spacious = body_rows >= if edit.choices.is_some() { 10 } else { 8 };
            let mut lines: Vec<Line> = Vec::new();
            let edit_label = config_label_for_key_for_locale(self.locale, &edit.key);
            let edit_title = if edit_label == edit.key {
                format!("{}{}", self.tr(MessageId::ConfigEditTitlePrefix), edit.key)
            } else {
                format!(
                    "{}{} [{}]",
                    self.tr(MessageId::ConfigEditTitlePrefix),
                    edit_label,
                    edit.key
                )
            };
            lines.push(Line::from(vec![Span::styled(
                edit_title,
                Style::default().fg(palette::WHALE_ACTION).bold(),
            )]));
            if spacious {
                lines.push(Line::from(""));
            }
            let muted = Style::default().fg(palette::TEXT_MUTED);
            let scope_spans = vec![
                Span::styled(self.tr(MessageId::ConfigEditScopeLabel), muted),
                Span::raw(edit.scope.label(self.locale)),
            ];
            let current_spans = vec![
                Span::styled(self.tr(MessageId::ConfigEditCurrentLabel), muted),
                Span::raw(truncate_view_text(&edit.original_value, 60)),
            ];
            if spacious {
                lines.push(Line::from(scope_spans));
                lines.push(Line::from(current_spans));
                lines.push(Line::from(""));
            } else {
                // Compact: scope and current share one row so the choices
                // and the controls both stay visible at 40x12.
                let mut merged = scope_spans;
                merged.push(Span::styled(" · ", muted));
                merged.extend(current_spans);
                lines.push(Line::from(merged));
            }
            if let Some(choices) = edit.choices.as_ref() {
                lines.push(Line::from(Span::styled(
                    self.tr(MessageId::ConfigEditChooseLabel),
                    Style::default().fg(palette::TEXT_MUTED),
                )));

                // Large catalogs (providers and themes) remain bounded by the
                // terminal. Keep the active option centered and mouse-hitbox
                // only the slice that is actually visible.
                let selected_detail = choices
                    .get(edit.selected_choice)
                    .map(|choice| config_choice_detail(self.locale, &edit.key, choice))
                    .unwrap_or_default();
                let available_rows = usize::from(inner.height)
                    .saturating_sub(reserved_footer_lines + CONTROL_ROWS + lines.len());
                // At the minimum supported height, the choices themselves are
                // the primary object. Shed the explanatory detail before any
                // option; larger surfaces keep one row for that detail.
                let detail_rows = usize::from(!selected_detail.is_empty() && available_rows > 3);
                let option_budget = available_rows.saturating_sub(detail_rows).max(1);
                let visible_options = option_budget.min(choices.len());
                let max_start = choices.len().saturating_sub(visible_options);
                let start = edit
                    .selected_choice
                    .saturating_sub(visible_options / 2)
                    .min(max_start);
                let end = (start + visible_options).min(choices.len());
                let mut hitboxes = Vec::new();

                for (choice_idx, choice) in choices.iter().enumerate().take(end).skip(start) {
                    let selected = choice_idx == edit.selected_choice;
                    let marker = crate::tui::glyphs::selection_marker(selected);
                    let label = config_choice_label(self.locale, &edit.key, choice);
                    let line_y = inner.y.saturating_add(lines.len() as u16);
                    hitboxes.push((
                        Rect {
                            x: inner.x,
                            y: line_y,
                            width: inner.width,
                            height: 1,
                        },
                        choice_idx,
                    ));
                    let mut line = Line::from(format!(
                        "  {marker} {:>2}. {}",
                        choice_idx + 1,
                        truncate_view_text(&label, usize::from(inner.width).saturating_sub(8))
                    ));
                    line.style = if selected {
                        menu_style::selected_row_style()
                    } else if self.hovered_choice == Some(choice_idx) {
                        Style::default()
                            .fg(palette::TEXT_PRIMARY)
                            .patch(crate::tui::menu_style::hovered_row_style())
                    } else {
                        Style::default().fg(palette::TEXT_PRIMARY)
                    };
                    lines.push(line);
                }
                *self.last_choice_hitboxes.borrow_mut() = hitboxes;

                if !selected_detail.is_empty()
                    && lines.len() + reserved_footer_lines + CONTROL_ROWS
                        < usize::from(inner.height)
                {
                    lines.push(Line::from(Span::styled(
                        crate::tui::ui_text::semantic_truncate(
                            selected_detail.as_ref(),
                            usize::from(inner.width),
                        ),
                        Style::default().fg(palette::TEXT_MUTED),
                    )));
                }
            } else {
                lines.push(render_config_editor_value_line(edit, self.locale));
                if spacious {
                    lines.push(Line::from(""));
                }
                let hint = config_hint_for_key(self.locale, &edit.key);
                if !hint.is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            self.tr(MessageId::ConfigEditHintLabel),
                            Style::default().fg(palette::TEXT_MUTED),
                        ),
                        Span::raw(hint),
                    ]));
                }
            }
            (lines, footer_text)
        } else {
            self.render_settings_shell(inner, buf);
            return;
        };

        // Footer wraps inside the body so its hints can never run off the modal
        // edge (#3732); the editor renders into the area above it, and the
        // Apply / Cancel controls own the last body row.
        let content = render_modal_text_footer(
            inner,
            buf,
            &footer,
            Style::default().fg(palette::TEXT_MUTED),
        );
        let body = Rect {
            height: content.height.saturating_sub(1),
            ..content
        };
        Paragraph::new(lines)
            .style(Style::default().fg(palette::TEXT_PRIMARY))
            .scroll((0, 0))
            .render(body, buf);
        if content.height > 0 {
            self.render_editor_controls(
                Rect {
                    y: content.bottom().saturating_sub(1),
                    height: 1,
                    ..content
                },
                buf,
            );
        }
    }
}

impl ConfigView {
    /// Paint `[ Apply ]  [ Cancel ]` and record their exact hitboxes.
    fn render_editor_controls(&self, row: Rect, buf: &mut Buffer) {
        use crate::tui::ui_text::text_display_width;

        let mut controls = Vec::new();
        let mut x = row.x;
        for (control, id, style) in [
            (
                EditorControl::Apply,
                MessageId::ConfigEditorApply,
                // The filled Apply control answers hover with an underline:
                // a bg tint would erase its button fill.
                if self.hovered_editor == Some(EditorControl::Apply) {
                    menu_style::selected_row_style().add_modifier(Modifier::UNDERLINED)
                } else {
                    menu_style::selected_row_style()
                },
            ),
            (
                EditorControl::Cancel,
                MessageId::ConfigEditorCancel,
                if self.hovered_editor == Some(EditorControl::Cancel) {
                    Style::default()
                        .fg(palette::TEXT_PRIMARY)
                        .add_modifier(Modifier::BOLD)
                        .patch(crate::tui::menu_style::hovered_row_style())
                } else {
                    Style::default()
                        .fg(palette::TEXT_PRIMARY)
                        .add_modifier(Modifier::BOLD)
                },
            ),
        ] {
            let label = format!("[ {} ]", self.tr(id));
            let width = u16::try_from(text_display_width(&label)).unwrap_or(u16::MAX);
            let limit = row.right().saturating_sub(x);
            if limit == 0 {
                break;
            }
            buf.set_stringn(x, row.y, &label, usize::from(limit), style);
            controls.push((
                Rect {
                    x,
                    y: row.y,
                    width: width.min(limit),
                    height: 1,
                },
                control,
            ));
            x = x.saturating_add(width).saturating_add(2);
        }
        *self.last_editor_controls.borrow_mut() = controls;
    }
}

// ---------------------------------------------------------------------------
// Tideline settings shell: category rail │ setting + action list │ detail at
// ≥100 columns; a horizontally windowed category strip over a full-width list
// below that. Every fact painted here comes from a row's typed
// `ConfigRowFacts`; nothing is re-derived from the key at render time.

/// Inner width at which the detail pane is painted beside the list and the
/// groups column appears. Below it the panel is tabs over one full-width
/// list, and the group headings inside the list carry the grouping.
const CONFIG_SHELL_DETAIL_MIN_WIDTH: u16 = 100;
/// Groups column width (the active tab's `ui.group` names).
const CONFIG_SHELL_GROUPS_WIDTH: u16 = 18;

/// Pane geometry for one render of the settings shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConfigShellPanes {
    groups: Option<Rect>,
    list: Rect,
    detail: Option<Rect>,
}

fn config_shell_panes(body: Rect, use_rail: bool) -> ConfigShellPanes {
    let rail_width = if use_rail {
        CONFIG_SHELL_GROUPS_WIDTH
    } else {
        0
    };
    let detail_width = if body.width >= CONFIG_SHELL_DETAIL_MIN_WIDTH {
        // A third of the body, floored so the list keeps a legible value
        // column at the 100-column blocker size.
        (body.width.saturating_mul(34) / 100).clamp(28, 44)
    } else {
        0
    };
    let rail_gap = u16::from(rail_width > 0);
    let detail_gap = u16::from(detail_width > 0);
    let list_width = body
        .width
        .saturating_sub(rail_width + rail_gap + detail_width + detail_gap)
        .max(1);
    let groups = (rail_width > 0).then_some(Rect {
        width: rail_width,
        ..body
    });
    let list = Rect {
        x: body.x.saturating_add(rail_width + rail_gap),
        width: list_width,
        ..body
    };
    let detail = (detail_width > 0).then_some(Rect {
        x: list.right().saturating_add(detail_gap),
        width: detail_width,
        ..body
    });
    ConfigShellPanes {
        groups,
        list,
        detail,
    }
}

fn setting_authority_label(
    locale: Locale,
    authority: SettingAuthority,
    detail: Option<&str>,
) -> Cow<'static, str> {
    let name = detail.unwrap_or_default();
    match authority {
        SettingAuthority::Environment => {
            Cow::Owned(tr(locale, MessageId::ConfigSourceEnvironment).replace("{name}", name))
        }
        SettingAuthority::Terminal => {
            Cow::Owned(tr(locale, MessageId::ConfigSourceTerminal).replace("{name}", name))
        }
        SettingAuthority::Session => tr(locale, MessageId::ConfigSourceSession),
        SettingAuthority::UserSettings => tr(locale, MessageId::ConfigSourceUserSettings),
        SettingAuthority::WorkspaceConfiguration => tr(locale, MessageId::ConfigSourceConfig),
        SettingAuthority::ManagedPolicy => tr(locale, MessageId::ConfigSourceManaged),
    }
}

fn setting_apply_label(locale: Locale, apply: SettingApplySemantics) -> Cow<'static, str> {
    tr(
        locale,
        match apply {
            SettingApplySemantics::EffectiveNow => MessageId::ConfigApplyEffectiveNow,
            SettingApplySemantics::Immediate => MessageId::ConfigApplyOnSave,
            SettingApplySemantics::NextSession => MessageId::ConfigApplyNextSession,
            SettingApplySemantics::RestartRequired => MessageId::ConfigApplyRestart,
            SettingApplySemantics::ReadOnly => MessageId::ConfigApplyReadOnly,
            SettingApplySemantics::ReloadRequired => MessageId::ConfigApplyReload,
            SettingApplySemantics::UiNowEngineRestart => MessageId::ConfigApplyUiNowEngineRestart,
        },
    )
}

fn setting_kind_label(locale: Locale, kind: SettingKind) -> Cow<'static, str> {
    tr(
        locale,
        match kind {
            SettingKind::Boolean => MessageId::ConfigKindToggle,
            SettingKind::Choice => MessageId::ConfigKindChoice,
            SettingKind::Integer => MessageId::ConfigKindNumber,
            SettingKind::Text => MessageId::ConfigKindText,
            SettingKind::Action => MessageId::ConfigKindAction,
            SettingKind::ReadOnly => MessageId::ConfigKindReadOnly,
        },
    )
}

/// Locale-neutral affordance glyph painted beside every list row so the
/// interaction (toggle / choose / edit / open / none) is visible before the
/// row is selected.
fn setting_affordance(kind: SettingKind, on: Option<bool>) -> &'static str {
    match kind {
        SettingKind::Boolean => {
            if on == Some(true) {
                "[x]"
            } else {
                "[ ]"
            }
        }
        SettingKind::Choice => "‹ ›",
        SettingKind::Integer | SettingKind::Text => "✎",
        SettingKind::Action => "›",
        SettingKind::ReadOnly => "⊘",
    }
}

/// Styles a category navigator paints with; `ConfigView` and the Tideline
/// stage scaffold each supply their own palette.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CategoryNavStyle {
    pub selected: Style,
    pub normal: Style,
    pub marker: Style,
    pub ascii_safe: bool,
}

/// Window of chips `[start, end)` that fits `width` columns while always
/// containing `selected`. Chips are separated by one column and two columns
/// are reserved on each side that hides chips, for the overflow markers.
fn category_strip_window(widths: &[usize], selected: usize, width: usize) -> (usize, usize) {
    let count = widths.len();
    let mut start = 0;
    loop {
        let mut used = if start > 0 { 2 } else { 0 };
        let mut end = start;
        while end < count {
            let separator = usize::from(end > start);
            let tail = if end + 1 < count { 2 } else { 0 };
            if end > start && used + separator + widths[end] + tail > width {
                break;
            }
            used += separator + widths[end];
            end += 1;
        }
        let end = end.max((start + 1).min(count));
        if selected < end || start + 1 >= count {
            return (start, end);
        }
        start += 1;
    }
}

/// Painted cells of a category strip: the visible chips and the ‹ / ›
/// overflow markers, which are themselves pointer targets so every category
/// is reachable by clicking alone at any width.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CategoryStripHitboxes {
    pub chips: Vec<(Rect, ConfigCategory)>,
    pub previous: Option<Rect>,
    pub next: Option<Rect>,
}

/// Paint the horizontally windowed category strip (the narrow-width
/// navigator from the design's `.settings-nav` rule) and return the painted
/// rect of every visible category and overflow marker.
///
/// `hovered` tints the chip under the pointer (and `hovered_nav` the overflow
/// marker) with the shared hover style so every strip target answers
/// visibly; hover never moves `selected`.
pub(crate) fn render_settings_category_strip(
    area: Rect,
    buf: &mut Buffer,
    selected: ConfigCategory,
    locale: Locale,
    style: CategoryNavStyle,
    hovered: Option<ConfigCategory>,
    hovered_nav: Option<NavStep>,
) -> CategoryStripHitboxes {
    use crate::tui::ui_text::{text_display_width, truncate_line_to_width};

    let mut hitboxes = CategoryStripHitboxes::default();
    if area.width < 4 || area.height == 0 {
        return hitboxes;
    }
    // At phone-width terminals, show one complete category and its position.
    // Both arrows retain the same wraparound navigation and measured targets.
    if area.width < 50 {
        let previous = Rect::new(area.x, area.y, 2, 1);
        let next = Rect::new(area.right().saturating_sub(2), area.y, 2, 1);
        let label_area = Rect::new(area.x + 2, area.y, area.width.saturating_sub(4), 1);
        let label = format!(
            "{}  {}/{}",
            selected.label(locale),
            selected.position() + 1,
            ConfigCategory::ALL.len()
        );
        for (rect, glyph, step) in [
            (
                previous,
                if style.ascii_safe { "< " } else { "‹ " },
                NavStep::Previous,
            ),
            (
                next,
                if style.ascii_safe { " >" } else { " ›" },
                NavStep::Next,
            ),
        ] {
            let marker_style = if hovered_nav == Some(step) {
                style.marker.patch(menu_style::hovered_row_style())
            } else {
                style.marker
            };
            buf.set_stringn(rect.x, rect.y, glyph, 2, marker_style);
        }
        buf.set_stringn(
            label_area.x,
            label_area.y,
            truncate_line_to_width(&label, usize::from(label_area.width)),
            usize::from(label_area.width),
            style.selected,
        );
        hitboxes.chips.push((label_area, selected));
        hitboxes.previous = Some(previous);
        hitboxes.next = Some(next);
        return hitboxes;
    }
    let labels: Vec<String> = ConfigCategory::ALL
        .iter()
        .map(|category| category.label(locale).into_owned())
        .collect();
    let widths: Vec<usize> = labels
        .iter()
        .map(|label| text_display_width(label) + 2)
        .collect();
    let (start, end) = category_strip_window(&widths, selected.position(), usize::from(area.width));
    let (prev, next) = if style.ascii_safe {
        ("< ", " >")
    } else {
        ("‹ ", " ›")
    };
    let y = area.y;
    let right = area.right();
    let mut x = area.x;
    if start > 0 {
        let prev_style = if hovered_nav == Some(NavStep::Previous) {
            style
                .marker
                .patch(crate::tui::menu_style::hovered_row_style())
        } else {
            style.marker
        };
        buf.set_stringn(x, y, prev, 2, prev_style);
        hitboxes.previous = Some(Rect {
            x,
            y,
            width: 2,
            height: 1,
        });
        x = x.saturating_add(2);
    }
    let tail_reserve: u16 = if end < labels.len() { 2 } else { 0 };
    for (index, label) in labels.iter().enumerate().take(end).skip(start) {
        let limit = right.saturating_sub(tail_reserve).saturating_sub(x);
        if limit == 0 {
            break;
        }
        let chip = format!(" {label} ");
        let painted = truncate_line_to_width(&chip, usize::from(limit));
        let painted_width = u16::try_from(text_display_width(&painted)).unwrap_or(limit);
        if painted_width == 0 {
            break;
        }
        let category = ConfigCategory::ALL[index];
        let chip_style = if category == selected {
            style.selected
        } else if hovered == Some(category) {
            style
                .normal
                .patch(crate::tui::menu_style::hovered_row_style())
        } else {
            style.normal
        };
        buf.set_stringn(x, y, &painted, usize::from(limit), chip_style);
        hitboxes.chips.push((
            Rect {
                x,
                y,
                width: painted_width,
                height: 1,
            },
            category,
        ));
        x = x.saturating_add(painted_width).saturating_add(1);
    }
    if end < labels.len() {
        let marker_x = right.saturating_sub(2);
        let next_style = if hovered_nav == Some(NavStep::Next) {
            style
                .marker
                .patch(crate::tui::menu_style::hovered_row_style())
        } else {
            style.marker
        };
        buf.set_stringn(marker_x, y, next, 2, next_style);
        hitboxes.next = Some(Rect {
            x: marker_x,
            y,
            width: 2,
            height: 1,
        });
    }
    hitboxes
}

impl ConfigView {
    /// Display label of the exact persisted value, without any effective
    /// suffix: the `saved` lane of the detail pane.
    fn saved_display_value(&self, row: &ConfigRow) -> String {
        // Preserve the exact saved currency alias (for example `rmb`).
        if row.key == "cost_currency" {
            return row.value.clone();
        }
        if SettingsRegistry::new(self).meta(row).choices.is_some() {
            if config_default_placeholder_message(&row.key).is_some_and(|message_id| {
                row.value == tr(self.locale, message_id) || row.value == tr(Locale::En, message_id)
            }) {
                return self.tr(MessageId::ConfigValueProviderDefault).into_owned();
            }
            let canonical = canonical_config_choice(&row.key, &row.value);
            return config_choice_label(self.locale, &row.key, &canonical);
        }
        row.value.clone()
    }

    /// Editor kind from the existing registry (its boolean/choice/integer
    /// tables plus the row's typed activation command).
    fn editor_kind(&self, row: &ConfigRow) -> SettingKind {
        SettingsRegistry::new(self).meta(row).kind
    }

    /// Project a setting row onto the shared Tideline fact. Action and
    /// diagnostic rows are not persisted facts and project to `None`.
    ///
    /// A lane is filled only from an explicit observation: the session
    /// snapshot, the live session value, the persisted value (saved and
    /// startup), or the `App` field carried on the row as `effective`.
    /// Nothing is inferred across lanes.
    fn setting_fact(&self, row: &ConfigRow) -> Option<SettingFact<String>> {
        if row.facts.kind != ConfigRowKind::Setting {
            return None;
        }
        let mut fact = match row.facts.snapshot {
            Some(SnapshotLane::Provider) => self.snapshot.provider.clone(),
            Some(SnapshotLane::Model) => self.snapshot.model.clone(),
            None => match row.scope {
                ConfigScope::Session => SettingFact::active_session(self.saved_display_value(row)),
                ConfigScope::Saved => {
                    // An unreadable store yields no saved or startup value.
                    let saved = row
                        .facts
                        .store_error
                        .is_none()
                        .then(|| self.saved_display_value(row));
                    let effective = row.facts.effective.as_deref().map(|value| {
                        config_choice_label(
                            self.locale,
                            &row.key,
                            &canonical_config_choice(&row.key, value),
                        )
                    });
                    SettingFact {
                        current: effective.clone(),
                        effective,
                        startup: saved.clone(),
                        saved,
                        authority: row.facts.authority,
                        apply: row.facts.apply,
                    }
                }
            },
        };
        fact.authority = row.facts.authority;
        fact.apply = row.facts.apply;
        Some(fact)
    }

    /// Verb for activating the selected row (`Enter opens…`, `Space toggles`).
    fn setting_action_label(&self, row: &ConfigRow) -> Cow<'static, str> {
        if let Some((_, verb)) = row.facts.command {
            return self.tr(verb);
        }
        self.tr(match self.editor_kind(row) {
            SettingKind::Boolean => MessageId::ConfigActionToggle,
            SettingKind::Choice => MessageId::ConfigActionChoose,
            SettingKind::Integer | SettingKind::Text => MessageId::ConfigActionEdit,
            SettingKind::Action | SettingKind::ReadOnly => MessageId::ConfigActionReadOnly,
        })
    }

    /// What activating the selected row does, spelled out: second click or
    /// Enter is the activation model, so the row says so.
    fn activation_copy(&self, row: &ConfigRow) -> String {
        if row.editable {
            format!(
                "{} {}",
                self.tr(MessageId::ConfigActivateAgain),
                self.setting_action_label(row)
            )
        } else {
            self.tr(MessageId::ConfigActionReadOnly).into_owned()
        }
    }

    fn lane_or_unobserved(&self, lane: Option<&String>) -> String {
        lane.cloned()
            .unwrap_or_else(|| self.tr(MessageId::ConfigLaneUnobserved).into_owned())
    }

    /// A persisted lane: the value, or the store's load error, or unobserved.
    fn store_lane(&self, lane: Option<&String>, store_error: Option<&str>) -> String {
        match (lane, store_error) {
            (Some(value), _) => value.clone(),
            (None, Some(error)) => self
                .tr(MessageId::ConfigLaneUnavailable)
                .replace("{error}", error),
            (None, None) => self.tr(MessageId::ConfigLaneUnobserved).into_owned(),
        }
    }

    /// The detail pane: label and key, then the typed facts for the row's
    /// kind, then the description and the activation copy.
    fn setting_detail_lines(&self, row: &ConfigRow, width: usize) -> Vec<Line<'static>> {
        use crate::tui::ui_text::semantic_truncate;

        let label = config_label_for_key_for_locale(self.locale, &row.key);
        let kind = self.editor_kind(row);
        let muted = Style::default().fg(palette::TEXT_MUTED);
        let primary = Style::default().fg(palette::TEXT_PRIMARY);
        let value_width = width.saturating_sub(10);
        let fact_line = |name: MessageId, value: &str| {
            Line::from(vec![
                Span::styled(format!("{:<10}", self.tr(name)), muted),
                Span::styled(semantic_truncate(value, value_width), primary),
            ])
        };
        let mut lines = vec![
            Line::from(Span::styled(
                semantic_truncate(&label, width),
                Style::default().fg(palette::WHALE_ACTION).bold(),
            )),
            Line::from(Span::styled(
                semantic_truncate(&row.key, width),
                Style::default().fg(palette::TEXT_DIM),
            )),
            Line::from(""),
        ];
        let source =
            setting_authority_label(self.locale, row.facts.authority, row.facts.authority_detail);
        let kind_label = setting_kind_label(self.locale, kind);
        match row.facts.kind {
            ConfigRowKind::Setting => {
                let fact = self
                    .setting_fact(row)
                    .unwrap_or_else(|| SettingFact::active_session(self.saved_display_value(row)));
                let store_error = row.facts.store_error.as_deref();
                lines.push(fact_line(
                    MessageId::ConfigFactCurrent,
                    &self.lane_or_unobserved(fact.effective.as_ref().or(fact.current.as_ref())),
                ));
                lines.push(fact_line(
                    MessageId::ConfigFactSaved,
                    &self.store_lane(fact.saved.as_ref(), store_error),
                ));
                lines.push(fact_line(
                    MessageId::ConfigFactStartup,
                    &self.store_lane(fact.startup.as_ref(), store_error),
                ));
                lines.push(fact_line(MessageId::ConfigFactSource, &source));
                lines.push(fact_line(
                    MessageId::ConfigFactScope,
                    row.scope.label(self.locale).as_ref(),
                ));
                lines.push(fact_line(
                    MessageId::ConfigFactApply,
                    &setting_apply_label(self.locale, fact.apply),
                ));
                lines.push(fact_line(MessageId::ConfigFactKind, &kind_label));
                // No existing source reports availability; say so rather
                // than implying the setting is known to be usable.
                lines.push(fact_line(
                    MessageId::ConfigFactAvailable,
                    &self.tr(MessageId::ConfigLaneUnobserved),
                ));
            }
            ConfigRowKind::Action => {
                lines.push(Line::from(Span::styled(
                    self.tr(MessageId::ConfigRowActionNote).into_owned(),
                    muted,
                )));
                if let Some((command, _)) = row.facts.command {
                    lines.push(fact_line(MessageId::ConfigFactOpens, command));
                }
                lines.push(fact_line(MessageId::ConfigFactSource, &source));
                lines.push(fact_line(MessageId::ConfigFactKind, &kind_label));
            }
            ConfigRowKind::Diagnostic => {
                lines.push(Line::from(Span::styled(
                    self.tr(MessageId::ConfigRowDiagnosticNote).into_owned(),
                    muted,
                )));
                lines.push(fact_line(MessageId::ConfigFactObserved, &row.value));
                lines.push(fact_line(MessageId::ConfigFactSource, &source));
                lines.push(fact_line(MessageId::ConfigFactKind, &kind_label));
            }
        }
        lines.push(Line::from(""));
        let hint = config_hint_for_key(self.locale, &row.key);
        let description = if hint.is_empty() {
            self.tr(MessageId::ConfigDescriptionDefault)
        } else {
            hint
        };
        lines.push(Line::from(Span::styled(description.into_owned(), muted)));
        lines.push(Line::from(Span::styled(
            self.activation_copy(row),
            Style::default().fg(palette::TEXT_HINT),
        )));
        lines
    }

    /// One-row fold of the detail for surfaces too narrow for the pane: the
    /// activation copy first, then the lanes that still fit.
    fn setting_detail_summary(&self, row: &ConfigRow) -> String {
        let activation = self.activation_copy(row);
        let label = config_label_for_key_for_locale(self.locale, &row.key);
        match row.facts.kind {
            ConfigRowKind::Setting => {
                let fact = self
                    .setting_fact(row)
                    .unwrap_or_else(|| SettingFact::active_session(self.saved_display_value(row)));
                format!(
                    "{activation} · {label}: {} {} · {} {} · {}",
                    self.tr(MessageId::ConfigFactCurrent),
                    self.lane_or_unobserved(fact.effective.as_ref().or(fact.current.as_ref())),
                    self.tr(MessageId::ConfigFactSaved),
                    self.store_lane(fact.saved.as_ref(), row.facts.store_error.as_deref()),
                    setting_apply_label(self.locale, fact.apply)
                )
            }
            ConfigRowKind::Action => {
                format!("{activation} · {}", self.tr(MessageId::ConfigRowActionNote))
            }
            ConfigRowKind::Diagnostic => {
                format!(
                    "{label}: {} · {}",
                    row.value,
                    self.tr(MessageId::ConfigRowDiagnosticNote)
                )
            }
        }
    }

    fn render_pane_divider(area: Rect, buf: &mut Buffer, x: u16) {
        if x < area.x || x >= area.right() {
            return;
        }
        for y in area.top()..area.bottom() {
            buf[(x, y)]
                .set_symbol("│")
                .set_style(Style::default().fg(palette::BORDER_COLOR));
        }
    }

    fn render_setting_detail(&self, area: Rect, buf: &mut Buffer) {
        let Some(row) = self.selected_row_index().and_then(|idx| self.rows.get(idx)) else {
            Paragraph::new(Line::from(Span::styled(
                self.tr(MessageId::ConfigNoSettings).into_owned(),
                Style::default().fg(palette::TEXT_MUTED),
            )))
            .wrap(Wrap { trim: false })
            .render(area, buf);
            return;
        };
        Paragraph::new(self.setting_detail_lines(row, usize::from(area.width)))
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    /// Paint the three-pane shell into the surface body.
    /// The active tab's groups, in schema order, with the group holding the
    /// selected row lit. It is the same projection the centre list paints as
    /// headings — at ≥100 columns it also gets a column of its own.
    fn render_group_column(&self, area: Rect, buf: &mut Buffer) {
        let selected_group = self
            .selected_row_index()
            .and_then(|idx| self.rows.get(idx))
            .map(|row| row.section());
        let mut lines: Vec<Line> = Vec::new();
        for group in self.visible_groups() {
            let selected = Some(group) == selected_group;
            let style = if selected {
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette::TEXT_MUTED)
            };
            lines.push(Line::from(vec![
                Span::styled(
                    if selected { "❯ " } else { "  " },
                    Style::default().fg(palette::WHALE_ACTION),
                ),
                Span::styled(
                    fit_config_column(
                        &group.label(self.locale),
                        usize::from(area.width).saturating_sub(2),
                    ),
                    style,
                ),
            ]));
        }
        Paragraph::new(lines).render(area, buf);
    }

    /// The groups of the active tab that actually have rows, in schema order.
    fn visible_groups(&self) -> Vec<ConfigSection> {
        let mut groups: Vec<ConfigSection> = Vec::new();
        for item in self.visible_items() {
            if let ConfigListItem::Section(section) = item
                && !groups.contains(&section)
            {
                groups.push(section);
            }
        }
        groups
    }

    /// A live preview of the footer the current values paint, drawn by the
    /// real footer renderer rather than a mock of it: the theme is the one
    /// the theme row selects, the chips are the mode and permission rows, and
    /// the depth line is this session's own context reading.
    fn render_footer_preview(&self, area: Rect, buf: &mut Buffer) {
        let label = self.tr(MessageId::ConfigPreviewLabel);
        let label_width = u16::try_from(UnicodeWidthStr::width(label.as_ref())).unwrap_or(0);
        if area.width <= label_width {
            return;
        }
        Paragraph::new(Line::from(Span::styled(
            label.into_owned(),
            Style::default().fg(palette::TEXT_HINT),
        )))
        .render(Rect { height: 1, ..area }, buf);

        let value_of = |key: &str| {
            self.rows
                .iter()
                .find(|row| row.key == key)
                .map(|row| canonical_config_choice(key, &row.value))
        };
        let theme = value_of("theme")
            .and_then(|name| {
                palette::SELECTABLE_THEMES
                    .iter()
                    .find(|id| id.name() == name)
                    .copied()
            })
            .map_or(palette::UI_THEME, palette::ThemeId::ui_theme);
        let context_percent = self.snapshot.context_budget.as_ref().map_or(0, |budget| {
            u8::try_from(budget.percent_basis_points / 100).unwrap_or(100)
        });
        let mode = value_of("default_mode").unwrap_or_else(|| "agent".to_string());
        let mode_ink = match mode.as_str() {
            "plan" => palette::ChromeInk::PolicyPlan,
            "operate" => palette::ChromeInk::PolicyOperate,
            _ => palette::ChromeInk::PolicyAct,
        };
        let permission = value_of("approval_mode")
            .or_else(|| value_of("permission_posture"))
            .unwrap_or_else(|| "ask".to_string());
        let permission_ink = match permission.as_str() {
            "auto-review" => palette::ChromeInk::PermissionAutoReview,
            "full-access" => palette::ChromeInk::PermissionFullAccess,
            _ => palette::ChromeInk::PermissionAsk,
        };
        let footer = crate::tui::phase_strip::TidelineFooter::new(
            &theme,
            (permission.as_str(), permission_ink),
        )
        .mode_chip(Some((mode.as_str(), mode_ink)))
        .context_percent(context_percent);
        crate::tui::phase_strip::render_tideline_footer(
            Rect {
                x: area.x.saturating_add(label_width),
                width: area.width - label_width,
                height: 1,
                ..area
            },
            buf,
            &footer,
        );
    }

    fn render_settings_shell(&self, inner: Rect, buf: &mut Buffer) {
        *self.last_choice_hitboxes.borrow_mut() = Vec::new();
        *self.last_editor_controls.borrow_mut() = Vec::new();
        *self.last_nav_controls.borrow_mut() = Vec::new();
        let items = self.visible_items();
        let match_count = self.matching_row_indices().len();

        let compact = inner.width < 50;
        // Keys and symbols stay legible in one row; the search field above
        // already explains typing. Shed verbose copy before editable content.
        let ascii_safe = crate::tui::color_compat::ascii_safe_enabled();
        let compact_hints = if ascii_safe {
            [
                ActionHint::new("Tab", "<>"),
                ActionHint::new("Up/Dn", ""),
                ActionHint::new("Enter", ""),
                ActionHint::new("Esc", ""),
            ]
        } else {
            [
                ActionHint::new("Tab", "⇆"),
                ActionHint::new("↑↓", ""),
                ActionHint::new("Enter", "↵"),
                ActionHint::new("Esc", "×"),
            ]
        };
        // Reserve the action footer by its actual wrapped height so no list
        // row silently falls off the bottom on compact terminals.
        let footer_height = |id: MessageId| -> usize {
            wrapped_footer_lines(&self.tr(id), inner.width, Style::default()).len()
        };
        let footer_lines = if compact {
            1
        } else if !self.filter.is_empty() {
            footer_height(MessageId::ConfigFooterFiltered)
        } else {
            footer_height(MessageId::ConfigFooterScrollable)
                .max(footer_height(MessageId::ConfigFooterDefault))
        }
        .max(1);
        let content_height = usize::from(inner.height).saturating_sub(footer_lines);

        // Header: the tab row over the search line. The tabs are always the
        // top row — a settings panel that hides which tab you are on is the
        // thing this layout exists to fix.
        const HEADER_LINES: usize = 2;
        // ≥100 columns spends 18 cells on the groups column and a detail
        // pane; below that the tab row and the in-list group headings carry
        // the same structure at 80 columns.
        let show_detail = inner.width >= CONFIG_SHELL_DETAIL_MIN_WIDTH;
        // Bottom band: the selected row's sentence, then a preview of the
        // footer these settings paint. The preview sheds first on short
        // terminals; the sentence holds while two list lines remain.
        let preview_lines = usize::from(content_height >= HEADER_LINES + 10);
        let sentence_lines = if compact {
            usize::from(content_height >= HEADER_LINES + 3)
        } else if content_height.saturating_sub(HEADER_LINES + preview_lines) >= 3 {
            // Without a detail pane the band also carries the lanes that pane
            // would have shown, on a second line so neither is truncated away.
            if show_detail {
                1
            } else if content_height >= HEADER_LINES + 9 {
                3
            } else {
                2
            }
        } else {
            usize::from(self.status.is_some())
        };
        let bottom_lines = sentence_lines + preview_lines;
        let body_height = content_height
            .saturating_sub(HEADER_LINES + bottom_lines)
            .max(1);
        self.last_visible_rows.set(body_height);
        let use_rail = show_detail && body_height >= 3;

        let clamp_height = |y: u16, wanted: usize| -> u16 {
            u16::try_from(wanted)
                .unwrap_or(u16::MAX)
                .min(inner.bottom().saturating_sub(y))
        };
        let header = Rect {
            height: clamp_height(inner.y, HEADER_LINES),
            ..inner
        };
        let body = Rect {
            y: header.bottom(),
            height: clamp_height(header.bottom(), body_height),
            ..inner
        };
        let bottom = Rect {
            y: body.bottom(),
            height: clamp_height(body.bottom(), bottom_lines),
            ..inner
        };
        let panes = config_shell_panes(body, use_rail);

        // Selection-anchored scroll: the row being manipulated always renders.
        let list_line_budget = usize::from(body.height).max(1);
        // A section caption costs itself plus a blank spacer, except at the
        // top of the window where no spacer is painted.
        let item_line_cost = |item: &ConfigListItem, first: bool| match item {
            ConfigListItem::Section(_) if first => 1usize,
            ConfigListItem::Section(_) => 2usize,
            ConfigListItem::Row(_) => 1usize,
        };
        let visible_end = |start: usize| {
            let mut used = 0usize;
            let mut end = start;
            while end < items.len() {
                let cost = item_line_cost(&items[end], end == start);
                if end > start && used.saturating_add(cost) > list_line_budget {
                    break;
                }
                used = used.saturating_add(cost);
                end += 1;
            }
            end
        };
        let mut start = self.scroll.min(items.len().saturating_sub(1));
        if let Some(selected_pos) = self.selected_display_position(&items) {
            start = start.min(selected_pos);
            while selected_pos >= visible_end(start) && start < selected_pos {
                start += 1;
            }
        }
        let end = visible_end(start);
        let scrollable = start > 0 || end < items.len();
        self.last_render_scroll.set(start);

        // Header.
        let search_value = if self.filter.is_empty() {
            self.tr(MessageId::ConfigSearchPlaceholder).to_string()
        } else {
            self.filter.clone()
        };
        let search_line = Line::from(vec![
            Span::styled(
                self.tr(MessageId::ConfigSearchLabel),
                Style::default().fg(palette::TEXT_MUTED),
            ),
            Span::raw(search_value),
            Span::styled(
                format!("  ({match_count}/{})", self.rows.len()),
                Style::default().fg(palette::TEXT_MUTED),
            ),
        ]);
        *self.last_rail_hitboxes.borrow_mut() = Vec::new();
        if header.height > 0 {
            let nav_row = Rect {
                height: 1,
                ..header
            };
            {
                let strip_style = CategoryNavStyle {
                    selected: menu_style::selected_row_style(),
                    normal: Style::default().fg(palette::TEXT_MUTED),
                    marker: Style::default().fg(palette::TEXT_HINT),
                    ascii_safe,
                };
                let strip = render_settings_category_strip(
                    nav_row,
                    buf,
                    self.category,
                    self.locale,
                    strip_style,
                    self.hovered_rail,
                    self.hovered_nav,
                );
                *self.last_rail_hitboxes.borrow_mut() = strip.chips;
                *self.last_nav_controls.borrow_mut() = strip
                    .previous
                    .map(|rect| (rect, NavStep::Previous))
                    .into_iter()
                    .chain(strip.next.map(|rect| (rect, NavStep::Next)))
                    .collect();
            }
        }
        if header.height > 1 {
            Paragraph::new(search_line).render(
                Rect {
                    y: header.y.saturating_add(1),
                    height: 1,
                    ..header
                },
                buf,
            );
        }

        // Groups column: the active tab's `ui.group` names, the one holding
        // the selected row lit.
        if let Some(column) = panes.groups {
            self.render_group_column(column, buf);
            Self::render_pane_divider(body, buf, column.right());
        }

        // List.
        let list =
            render_panel_scroll_rail(panes.list, buf, items.len(), start, list_line_budget, true);
        let (key_column_width, value_column_width, scope_column_width) =
            self.table_column_widths(usize::from(list.width));
        let mut lines: Vec<Line> = Vec::new();
        let mut row_hitboxes = Vec::new();
        for item in &items[start..end] {
            match item {
                ConfigListItem::Section(section) => {
                    if !lines.is_empty() {
                        lines.push(Line::from(""));
                    }
                    lines.push(Line::from(Span::styled(
                        format!("  {}", section.label(self.locale)),
                        Style::default()
                            .fg(palette::TEXT_HINT)
                            .bold()
                            .add_modifier(Modifier::UNDERLINED),
                    )));
                }
                ConfigListItem::Row(idx) => {
                    let Some(row) = self.rows.get(*idx) else {
                        continue;
                    };
                    let line_y = list.y.saturating_add(lines.len() as u16);
                    if line_y >= list.bottom() {
                        break;
                    }
                    row_hitboxes.push((
                        Rect {
                            x: list.x,
                            y: line_y,
                            width: list.width,
                            height: 1,
                        },
                        *idx,
                    ));
                    let selected = *idx == self.selected;
                    // Hover tints but never steals the keyboard selection.
                    let hovered = !selected && self.hovered_row == Some(*idx);
                    let style = if selected {
                        menu_style::selected_row_style()
                    } else if row.editable {
                        Style::default().fg(palette::TEXT_PRIMARY)
                    } else {
                        // Read-only rows look distinct from editable ones.
                        Style::default()
                            .fg(palette::TEXT_MUTED)
                            .add_modifier(Modifier::DIM)
                    };
                    let label = config_label_for_key_for_locale(self.locale, &row.key);
                    let (key_width, value_width) = if compact {
                        let available = usize::from(list.width).saturating_sub(
                            CONFIG_ROW_PREFIX_WIDTH
                                + CONFIG_COLUMN_GAPS_WIDTH
                                + CONFIG_AFFORDANCE_COLUMN_WIDTH,
                        );
                        let key_width = UnicodeWidthStr::width(label.as_str()).min(available / 2);
                        (key_width, available.saturating_sub(key_width))
                    } else {
                        (key_column_width, value_column_width)
                    };
                    let key = fit_config_column(&label, key_width);
                    let value = fit_config_column(&self.row_display_value(row), value_width);
                    let kind = self.editor_kind(row);
                    let on = (kind == SettingKind::Boolean)
                        .then(|| canonical_config_choice(&row.key, row.edit_value()) == "true");
                    let affordance = setting_affordance(kind, on);
                    // Action and diagnostic rows are not persisted facts, so
                    // they carry no scope badge.
                    let badge = match row.facts.kind {
                        ConfigRowKind::Setting if scope_column_width > 0 => {
                            row.scope.label(self.locale)
                        }
                        _ => Cow::Borrowed(""),
                    };
                    let rail = if selected { "❯" } else { " " };
                    let mut line = Line::from(vec![
                        Span::styled(
                            rail,
                            if selected {
                                style
                            } else {
                                Style::default().fg(palette::TEXT_DIM)
                            },
                        ),
                        Span::styled(format!("{key}  {value}  "), style),
                        Span::styled(
                            format!("{affordance:<3}  "),
                            if selected {
                                style
                            } else if row.editable {
                                Style::default().fg(palette::WHALE_ACTION)
                            } else {
                                Style::default()
                                    .fg(palette::TEXT_DIM)
                                    .add_modifier(Modifier::DIM)
                            },
                        ),
                        Span::styled(
                            badge.into_owned(),
                            if selected {
                                style
                            } else {
                                Style::default()
                                    .fg(palette::TEXT_HINT)
                                    .add_modifier(Modifier::DIM)
                            },
                        ),
                    ]);
                    if selected {
                        line.style = menu_style::selected_row_bg_style();
                    } else if hovered {
                        line.style = menu_style::hovered_row_style();
                    }
                    lines.push(line);
                }
            }
        }
        *self.last_row_hitboxes.borrow_mut() = row_hitboxes;
        if items.is_empty() {
            let message = if self.filter.is_empty() {
                self.tr(MessageId::ConfigNoSettings).to_string()
            } else {
                format!(
                    "{}\"{}\".",
                    self.tr(MessageId::ConfigNoMatchesPrefix),
                    self.filter
                )
            };
            lines.push(Line::from(Span::styled(
                message,
                Style::default().fg(palette::TEXT_MUTED),
            )));
        }
        Paragraph::new(lines)
            .style(Style::default().fg(palette::TEXT_PRIMARY))
            .render(list, buf);

        // Detail.
        if let Some(detail) = panes.detail {
            Self::render_pane_divider(body, buf, detail.x.saturating_sub(1));
            self.render_setting_detail(detail, buf);
        }

        // Status row: an explicit status wins; otherwise the selected row's
        // activation copy, with the detail facts folded in when no pane
        // shows them.
        let sentence_height = u16::try_from(sentence_lines)
            .unwrap_or(0)
            .min(bottom.height);
        if sentence_height > 0 {
            let selected_row = self.selected_row_index().and_then(|idx| self.rows.get(idx));
            // The band says what the selected setting *is*: the schema's
            // sentence, in one plain line. A live status or an active filter
            // is more urgent and takes the row while it lasts.
            let bottom_text = if let Some(status) = self.status.as_ref() {
                status.clone()
            } else if let Some(row) = selected_row.filter(|_| compact) {
                format!(
                    "{}: {}",
                    config_label_for_key_for_locale(self.locale, &row.key),
                    self.row_display_value(row)
                )
            } else if !self.filter.is_empty() {
                format!(
                    "{}: {match_count}",
                    self.tr(MessageId::ConfigFilteredSettings)
                )
            } else if let Some(row) = selected_row {
                let sentence = config_hint_for_key(self.locale, &row.key);
                if sentence.is_empty() {
                    self.activation_copy(row)
                } else {
                    sentence.into_owned()
                }
            } else {
                String::new()
            };
            let band = vec![Line::from(Span::styled(
                crate::tui::ui_text::semantic_truncate(&bottom_text, usize::from(inner.width)),
                Style::default().fg(palette::TEXT_MUTED),
            ))];
            // With a detail pane the lanes are already on screen; without one
            // the band's second line carries what that pane would have shown.
            Paragraph::new(band).render(
                Rect {
                    height: 1,
                    ..bottom
                },
                buf,
            );
            if sentence_height > 1
                && self.status.is_none()
                && self.filter.is_empty()
                && let Some(row) = selected_row
            {
                let folded = Rect {
                    y: bottom.y.saturating_add(1),
                    height: sentence_height - 1,
                    ..bottom
                };
                Paragraph::new(Line::from(Span::styled(
                    crate::tui::ui_text::semantic_truncate(
                        &self.setting_detail_summary(row),
                        usize::from(inner.width).saturating_mul(usize::from(folded.height)),
                    ),
                    Style::default().fg(palette::TEXT_HINT),
                )))
                .wrap(Wrap { trim: true })
                .render(folded, buf);
            }
        }
        if bottom.height > sentence_height {
            self.render_footer_preview(
                Rect {
                    y: bottom.y.saturating_add(sentence_height),
                    height: bottom.height - sentence_height,
                    ..bottom
                },
                buf,
            );
        }

        let footer = if !self.filter.is_empty() {
            self.tr(MessageId::ConfigFooterFiltered)
        } else if scrollable {
            self.tr(MessageId::ConfigFooterScrollable)
        } else {
            self.tr(MessageId::ConfigFooterDefault)
        };
        if compact {
            render_modal_footer(inner, buf, &compact_hints);
        } else {
            render_modal_text_footer(
                inner,
                buf,
                &footer,
                Style::default().fg(palette::TEXT_MUTED),
            );
        }
    }
}

pub mod help;

pub use help::HelpView;

pub struct SubAgentsView {
    agents: Vec<SubAgentResult>,
    scroll: usize,
    /// Index into the render-ordered agent list (`ordered` on `grouped`).
    /// Enter/click open the selected agent's transcript — the same primary
    /// destination every other agent surface resolves to (v0.9.7).
    selected: usize,
    /// Rendered agent blocks from the last frame: `(first_line, line_count,
    /// agent_id)` in render order. Interior-mutable because `render` takes
    /// `&self`; consumed by click resolution and selection scroll-follow.
    row_lines: std::cell::RefCell<Vec<(usize, usize, String)>>,
    /// Body area of the last render, for mapping click rows onto lines.
    body_area: std::cell::Cell<Rect>,
    /// Effective (clamped) scroll of the last render.
    last_render_scroll: std::cell::Cell<usize>,
    /// Visible body height of the last render.
    last_visible_lines: std::cell::Cell<usize>,
    /// Motion policy at open: the Whale Teams working wake animates only
    /// under `MotionMode::Full` (Reduced/Still hold the poster frame).
    motion: crate::tui::motion::mode::MotionMode,
    /// UI locale for the whale state words.
    locale: Locale,
    /// Wall clock anchor for the working-wake frame.
    opened_at: std::time::Instant,
    /// True when the Fleet roster is parked directly underneath on the view
    /// stack (#5954), i.e. this view was reached with `Tab`/`w` from the
    /// roster. `Esc` still pops exactly one view — the flag only decides
    /// whether the footer promises `back` or `close`, and lets `F` return to
    /// the parked roster instead of stacking a second one. Direct entry
    /// (`/fleet workers`, the Work dock) leaves it false, so `Esc` closes.
    back_to_fleet_roster: bool,
    /// Agent whose Stop is armed (addendum F4). Stopping an agent that can
    /// change files takes two presses of `X` (or `X` then `Enter`); `Esc`
    /// or moving the selection disarms it.
    armed_stop: Option<String>,
}

/// Whether stopping this agent can strand file work, so `X` asks twice: it
/// is still running and may write files (write permission or a full shell).
/// Rows without a permission snapshot (live progress rows) stop on one press.
fn subagent_stop_needs_confirm(agent: &SubAgentResult) -> bool {
    agent.status == SubAgentStatus::Running
        && agent
            .runtime_permissions
            .as_ref()
            .is_some_and(|permissions| permissions.write || permissions.shell == "full")
}

/// Build the agent rows shown by `/subagents`.
///
/// The engine manager is the durable source of truth, but live UI cards can
/// briefly be ahead of the manager-list refresh. Include those live rows so
/// the command does not say "no agents" while the footer/sidebar already show
/// active delegated work.
pub(crate) fn subagent_view_agents(
    app: &App,
    manager_agents: &[SubAgentResult],
) -> Vec<SubAgentResult> {
    let mut agents = manager_agents.to_vec();
    let manager_agent_count = agents.len();
    let mut seen: std::collections::HashSet<String> =
        agents.iter().map(|agent| agent.agent_id.clone()).collect();

    for (agent_id, progress) in &app.agent_progress {
        if seen.insert(agent_id.clone()) {
            agents.push(live_subagent_result(
                agent_id,
                FleetRole::Worker,
                SubAgentStatus::Running,
                progress,
                Some("live"),
                None, // live rows compute nickname from agent manager on render
            ));
        }
    }

    for cell in &app.history {
        match cell {
            HistoryCell::SubAgent(SubAgentCell::Delegate(card))
                if seen.insert(card.agent_id.clone()) =>
            {
                let agent_type = FleetRole::from_str(&card.agent_type).unwrap_or(FleetRole::Worker);
                let mut row = live_subagent_result(
                    &card.agent_id,
                    agent_type,
                    lifecycle_to_subagent_status(card.status, card.summary.as_deref()),
                    card.summary.as_deref().unwrap_or(card.agent_type.as_str()),
                    Some("transcript"),
                    None, // transcript-derived rows get nickname from manager on render
                );
                row.worker_status = lifecycle_worker_status(card.status);
                agents.push(row);
            }
            HistoryCell::SubAgent(SubAgentCell::Fanout(card)) => {
                for worker in &card.workers {
                    if seen.insert(worker.agent_id.clone()) {
                        let objective = format!(
                            "{} worker {}",
                            summarize_tool_output(&card.kind),
                            summarize_tool_output(&worker.worker_id)
                        );
                        let mut row = live_subagent_result(
                            &worker.agent_id,
                            FleetRole::Worker,
                            lifecycle_to_subagent_status(worker.status, None),
                            &objective,
                            Some(card.kind.as_str()),
                            None, // fanout worker rows get nickname from manager on render
                        );
                        row.worker_status = lifecycle_worker_status(worker.status);
                        agents.push(row);
                    }
                }
            }
            _ => {}
        }
    }

    let mut display_names = localized_whale_display_names(
        agents[..manager_agent_count]
            .iter()
            .map(|agent| (agent.agent_id.as_str(), agent.nickname.as_deref())),
        app.ui_locale.tag(),
    );
    for agent in &mut agents[..manager_agent_count] {
        // The row headline reads `nickname`, so the dispatch name lands there
        // when the agent has one; the generated whale names the rest (#5287).
        // The view is handed manager rows that may not be in
        // `subagent_cache` yet, so the row's own dispatch name backs up the
        // app-wide lookup.
        let own_name = agent.name.trim();
        let display_name = app
            .agent_given_name(&agent.agent_id)
            .or_else(|| {
                (!own_name.is_empty() && own_name != agent.agent_id).then(|| own_name.to_string())
            })
            .or_else(|| display_names.remove(&agent.agent_id));
        agent.nickname = display_name;
    }
    for agent in &mut agents[manager_agent_count..] {
        // Progress and transcript rows can arrive before ListSubAgents. Keep
        // their stable Agent-N placeholder until the manager snapshot supplies
        // the locale-neutral identity needed for generated whale display.
        agent.nickname = app.agent_label_map.get(&agent.agent_id).cloned();
    }

    agents
}

/// Project a transcript card's lifecycle onto the manager status vocabulary.
/// A failure or interruption keeps the reason the card recorded from its
/// terminal envelope; only a card that recorded none gets the generic line.
///
/// `SubAgentStatus` has no pending state; callers carry a pending worker as
/// `worker_status: Queued` (see [`lifecycle_worker_status`]) so it is not
/// shown as running.
fn lifecycle_to_subagent_status(status: AgentLifecycle, reason: Option<&str>) -> SubAgentStatus {
    let reason = |fallback: &str| {
        reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .unwrap_or(fallback)
            .to_string()
    };
    match status {
        AgentLifecycle::Pending | AgentLifecycle::Running => SubAgentStatus::Running,
        AgentLifecycle::Completed => SubAgentStatus::Completed,
        AgentLifecycle::Failed => SubAgentStatus::Failed(reason("failed in transcript")),
        AgentLifecycle::Cancelled => SubAgentStatus::Cancelled,
        AgentLifecycle::Interrupted => {
            SubAgentStatus::Interrupted(reason("interrupted in transcript"))
        }
    }
}

/// The worker-status detail a transcript lifecycle adds to its projected
/// manager status: a spawned but not yet started worker is `Queued`, which the
/// row label and whale state show instead of the coarse `Running`.
fn lifecycle_worker_status(
    status: AgentLifecycle,
) -> Option<crate::tools::subagent::AgentWorkerStatus> {
    (status == AgentLifecycle::Pending).then_some(crate::tools::subagent::AgentWorkerStatus::Queued)
}

fn live_subagent_result(
    agent_id: &str,
    agent_type: FleetRole,
    status: SubAgentStatus,
    objective: &str,
    role: Option<&str>,
    nickname: Option<String>,
) -> SubAgentResult {
    SubAgentResult {
        usage: None,
        name: agent_id.to_string(),
        agent_id: agent_id.to_string(),
        context_mode: "fresh".to_string(),
        fork_context: false,
        workspace: None,
        git_branch: None,
        agent_type,
        assignment: SubAgentAssignment {
            native_preset: None,
            objective: summarize_tool_output(objective),
            role: role.map(str::to_string),
        },
        model: String::new(),
        nickname,
        status,
        worker_status: None,
        runtime_permissions: None,
        parent_run_id: None,
        spawn_depth: 0,
        child_route: None,
        result: None,
        steps_taken: 0,
        checkpoint: None,
        needs_input: None,
        duration_ms: 0,
        started_at: None,
        from_prior_session: false,
        idle_ms: None,
        heartbeat_timeout_ms: None,
    }
}

impl SubAgentsView {
    pub fn new(agents: Vec<SubAgentResult>) -> Self {
        Self {
            agents,
            scroll: 0,
            selected: 0,
            row_lines: std::cell::RefCell::new(Vec::new()),
            body_area: std::cell::Cell::new(Rect::default()),
            last_render_scroll: std::cell::Cell::new(0),
            last_visible_lines: std::cell::Cell::new(0),
            motion: crate::tui::motion::mode::MotionMode::Still,
            locale: Locale::En,
            opened_at: std::time::Instant::now(),
            back_to_fleet_roster: false,
            armed_stop: None,
        }
    }

    /// Open with the app's motion policy and locale so the whale rows follow
    /// the user's reduced-motion setting and language.
    pub fn for_app(app: &App, agents: Vec<SubAgentResult>) -> Self {
        let mut view = Self::new(agents);
        view.motion = app.motion_policy().mode();
        view.locale = app.ui_locale;
        view
    }

    fn selected_agent(&self) -> Option<&SubAgentResult> {
        let id = self.ordered_agent_ids().get(self.selected).cloned()?;
        self.agents.iter().find(|agent| agent.agent_id == id)
    }

    /// `X`: stop the selected agent. One that can change files arms first and
    /// stops on the second press, so a stray key cannot end a writer.
    fn press_stop(&mut self) -> ViewAction {
        let Some(agent) = self.selected_agent() else {
            return ViewAction::None;
        };
        let agent_id = agent.agent_id.clone();
        if subagent_stop_needs_confirm(agent) && self.armed_stop.as_deref() != Some(&agent_id) {
            self.armed_stop = Some(agent_id);
            return ViewAction::None;
        }
        self.armed_stop = None;
        ViewAction::Emit(ViewEvent::SidebarAgentCancel { agent_id })
    }

    /// Mark this view as pushed on top of the Fleet roster (#5954), so the
    /// footer says `back` and `F` pops to the parked roster.
    #[must_use]
    pub fn over_fleet_roster(mut self) -> Self {
        self.back_to_fleet_roster = true;
        self
    }

    /// Footer label for `Esc`: `back` while the roster is parked underneath,
    /// `close` at the root. The hint has to name what the key actually does.
    fn esc_hint_label(&self) -> std::borrow::Cow<'static, str> {
        if self.back_to_fleet_roster {
            tr(self.locale, MessageId::SetupActionBack)
        } else {
            tr(self.locale, MessageId::SessionsActionClose)
        }
    }

    /// Working-wake frame for this render: 0 unless motion is Full.
    fn whale_frame(&self) -> usize {
        let now_ms = u64::try_from(self.opened_at.elapsed().as_millis()).unwrap_or(0);
        crate::tui::whales::working_frame(now_ms, self.motion)
    }

    /// The five status groups in render order, each sorted the way the view
    /// paints them. Selection, Enter, and click resolution all consume this
    /// so the highlighted row and the opened agent can never diverge.
    fn grouped(agents: &[SubAgentResult]) -> [Vec<&SubAgentResult>; 5] {
        let mut running = Vec::new();
        let mut completed = Vec::new();
        let mut interrupted = Vec::new();
        let mut failed = Vec::new();
        let mut cancelled = Vec::new();

        for agent in agents {
            match agent.status {
                SubAgentStatus::Running => running.push(agent),
                SubAgentStatus::Completed => completed.push(agent),
                SubAgentStatus::Interrupted(_) => interrupted.push(agent),
                SubAgentStatus::Failed(_) => failed.push(agent),
                SubAgentStatus::Cancelled => cancelled.push(agent),
                SubAgentStatus::BudgetExhausted => failed.push(agent),
            }
        }
        for group in [
            &mut running,
            &mut completed,
            &mut interrupted,
            &mut failed,
            &mut cancelled,
        ] {
            group.sort_by(|a, b| {
                agent_type_order(&a.agent_type)
                    .cmp(&agent_type_order(&b.agent_type))
                    .then_with(|| a.agent_id.cmp(&b.agent_id))
            });
        }
        [running, completed, interrupted, failed, cancelled]
    }

    fn ordered_agent_ids(&self) -> Vec<String> {
        Self::grouped(&self.agents)
            .iter()
            .flatten()
            .map(|agent| agent.agent_id.clone())
            .collect()
    }

    /// Keep the selected agent's block inside the visible body, using the
    /// last render's layout (stale by at most one frame).
    fn follow_selection(&mut self) {
        let row_lines = self.row_lines.borrow();
        let Some((first, count, _)) = row_lines.get(self.selected) else {
            return;
        };
        let visible = self.last_visible_lines.get().max(1);
        let end = first + count;
        if *first < self.scroll {
            self.scroll = *first;
        } else if end > self.scroll + visible {
            self.scroll = end.saturating_sub(visible);
        }
    }
}

impl ModalView for SubAgentsView {
    fn kind(&self) -> ModalKind {
        ModalKind::SubAgents
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        use crossterm::event::KeyCode;

        // An armed Stop (F4) owns Esc and Enter: Esc disarms without closing,
        // Enter confirms — the same two-step the Work inspector's Stop uses.
        if self.armed_stop.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.armed_stop = None;
                    return ViewAction::None;
                }
                KeyCode::Enter => return self.press_stop(),
                KeyCode::Char('x') | KeyCode::Char('X') => {}
                _ => self.armed_stop = None,
            }
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ViewAction::Close,
            // Enter opens the selected agent's transcript — the same primary
            // destination the Work strip and sidebar resolve to (v0.9.7). On
            // an empty register Enter keeps its old refresh meaning.
            KeyCode::Enter => match self.ordered_agent_ids().get(self.selected).cloned() {
                Some(agent_id) => ViewAction::Emit(ViewEvent::OpenAgentTranscript { agent_id }),
                None => ViewAction::Emit(ViewEvent::SubAgentsRefresh),
            },
            KeyCode::Char('r') | KeyCode::Char('R') => {
                ViewAction::Emit(ViewEvent::SubAgentsRefresh)
            }
            // Manage: stop the selected agent. Terminal agents ignore the
            // key; the cancel receipt names what happened either way. A
            // running agent that can change files asks twice (F4).
            KeyCode::Char('x') | KeyCode::Char('X') => self.press_stop(),
            // The roster is the same destination either way: pop back to the
            // parked one when there is one (#5954) — re-running `/fleet`
            // would stack a duplicate roster and lose its cursor.
            KeyCode::Char('f') | KeyCode::Char('F') if self.back_to_fleet_roster => {
                ViewAction::Close
            }
            KeyCode::Char('f') | KeyCode::Char('F') => {
                ViewAction::Emit(ViewEvent::CommandPaletteSelected {
                    action: CommandPaletteAction::ExecuteCommand {
                        command: "/fleet".to_string(),
                    },
                })
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                self.follow_selection();
                ViewAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = self
                    .selected
                    .saturating_add(1)
                    .min(self.agents.len().saturating_sub(1));
                self.follow_selection();
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.scroll = self.scroll.saturating_sub(3);
                ViewAction::None
            }
            MouseEventKind::ScrollDown => {
                // Clamped to the real maximum at render time.
                self.scroll = self.scroll.saturating_add(3);
                ViewAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let area = self.body_area.get();
                if mouse.column < area.x
                    || mouse.column >= area.x.saturating_add(area.width)
                    || mouse.row < area.y
                    || mouse.row >= area.y.saturating_add(area.height)
                {
                    return ViewAction::None;
                }
                let line = usize::from(mouse.row - area.y) + self.last_render_scroll.get();
                let hit = self
                    .row_lines
                    .borrow()
                    .iter()
                    .enumerate()
                    .find(|(_, (first, count, _))| line >= *first && line < first + count)
                    .map(|(index, (_, _, agent_id))| (index, agent_id.clone()));
                match hit {
                    Some((index, agent_id)) => {
                        self.selected = index;
                        // Click opens the same door Enter does.
                        ViewAction::Emit(ViewEvent::OpenAgentTranscript { agent_id })
                    }
                    None => ViewAction::None,
                }
            }
            _ => ViewAction::None,
        }
    }

    fn update_subagents(&mut self, agents: &[SubAgentResult]) -> bool {
        let selected_id = self.ordered_agent_ids().get(self.selected).cloned();
        self.agents = agents.to_vec();
        let last = self.agents.len().saturating_sub(1);
        self.scroll = self.scroll.min(last);
        self.selected = selected_id
            .and_then(|id| {
                self.ordered_agent_ids()
                    .iter()
                    .position(|candidate| candidate == &id)
            })
            .unwrap_or_else(|| self.selected.min(last));
        // An armed Stop only survives while its agent is still selected and
        // still needs the confirm (it may have finished meanwhile).
        let still_armed = self.armed_stop.as_deref().is_some_and(|armed| {
            self.selected_agent()
                .is_some_and(|agent| agent.agent_id == armed && subagent_stop_needs_confirm(agent))
        });
        if !still_armed {
            self.armed_stop = None;
        }
        true
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        Block::default()
            .style(Style::default().bg(palette::WHALE_BG))
            .render(area, buf);

        let mut lines: Vec<Line> = Vec::new();
        let mut row_lines: Vec<(usize, usize, String)> = Vec::new();
        let content_width = area.width.saturating_sub(4) as usize;

        if self.agents.is_empty() {
            lines.push(Line::from(Span::styled(
                tr(
                    self.locale,
                    MessageId::SubagentsNoCurrentSessionFleetWorkers,
                ),
                Style::default().fg(palette::TEXT_MUTED),
            )));
            lines.push(Line::from(Span::styled(
                tr(self.locale, MessageId::SubagentsEmptyGuidance),
                Style::default().fg(palette::TEXT_DIM),
            )));
        } else {
            let [running, completed, interrupted, failed, cancelled] = Self::grouped(&self.agents);
            let selected_id = self
                .ordered_agent_ids()
                .get(self.selected)
                .cloned()
                .unwrap_or_default();

            let status_summary = [
                (
                    MessageId::SubagentsStatusRunning,
                    running.len(),
                    palette::STATUS_WARNING,
                ),
                (
                    MessageId::SubagentsStatusCompleted,
                    completed.len(),
                    palette::STATUS_SUCCESS,
                ),
                (
                    MessageId::SubagentsStatusInterrupted,
                    interrupted.len(),
                    palette::STATUS_WARNING,
                ),
                (
                    MessageId::SubagentsStatusFailed,
                    failed.len(),
                    palette::WHALE_ERROR,
                ),
                (
                    MessageId::SubagentsStatusCancelled,
                    cancelled.len(),
                    palette::TEXT_MUTED,
                ),
            ];

            lines.push(Line::from(Span::styled(
                tr(
                    self.locale,
                    MessageId::SubagentsCurrentSessionFleetWorkersTitle,
                ),
                Style::default().fg(palette::WHALE_ACTION).bold(),
            )));
            lines.push(Line::from(Span::styled(
                tr(
                    self.locale,
                    MessageId::SubagentsCurrentSessionFleetWorkerRoles,
                ),
                Style::default().fg(palette::TEXT_DIM),
            )));

            let mut summary_parts = Vec::new();
            for (label_id, count, color) in status_summary {
                let label = tr(self.locale, label_id);
                let count = count.to_string();
                summary_parts.push(Line::from(Span::styled(
                    tr(self.locale, MessageId::SubagentsSummaryItem)
                        .replace("{label}", label.as_ref())
                        .replace("{count}", &count),
                    Style::default().fg(color),
                )));
            }

            let mut summary = vec![Span::styled("  ", Style::default().fg(palette::TEXT_DIM))];
            for (idx, part) in summary_parts.into_iter().enumerate() {
                if idx > 0 {
                    summary.push(Span::raw("  ·  "));
                }
                summary.extend(part);
            }
            lines.push(Line::from(summary));
            lines.push(Line::from(Span::styled(
                "",
                Style::default().fg(palette::TEXT_DIM),
            )));

            for (title_id, style, group) in [
                (
                    MessageId::SubagentsStatusRunning,
                    ratatui::style::Style::from(palette::STATUS_WARNING),
                    &running,
                ),
                (
                    MessageId::SubagentsStatusCompleted,
                    palette::STATUS_SUCCESS.into(),
                    &completed,
                ),
                (
                    MessageId::SubagentsStatusInterrupted,
                    palette::STATUS_WARNING.into(),
                    &interrupted,
                ),
                (
                    MessageId::SubagentsStatusFailed,
                    palette::WHALE_ERROR.into(),
                    &failed,
                ),
                (
                    MessageId::SubagentsStatusCancelled,
                    palette::TEXT_MUTED.into(),
                    &cancelled,
                ),
            ] {
                let title = tr(self.locale, title_id);
                append_subagent_group(
                    &mut lines,
                    &mut row_lines,
                    title.as_ref(),
                    style,
                    group,
                    content_width,
                    &selected_id,
                    WhaleRowContext {
                        locale: self.locale,
                        frame: self.whale_frame(),
                    },
                );
            }
        }

        let content = render_modal_footer(
            area,
            buf,
            &[
                ActionHint::new("Esc", self.esc_hint_label()),
                ActionHint::new("↑/↓", tr(self.locale, MessageId::CtxInspActionSelect)),
                ActionHint::new("Enter", tr(self.locale, MessageId::ExtensionsActionFocus)),
                if self.armed_stop.is_some() {
                    ActionHint::new(
                        "X/Enter",
                        tr(self.locale, MessageId::WorkSurfaceStopConfirmHint),
                    )
                } else {
                    ActionHint::new("X", tr(self.locale, MessageId::SidebarStopControl))
                },
                ActionHint::new("R", tr(self.locale, MessageId::SubagentsActionRefresh)),
                ActionHint::new("F", tr(self.locale, MessageId::SubagentsActionRosterSetup)),
            ],
        );
        let shell = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                ratatui::layout::Constraint::Length(3),
                ratatui::layout::Constraint::Min(1),
            ])
            .split(content);
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    format!("─ {} ", tr(self.locale, MessageId::FleetRosterHeaderLabel)),
                    Style::default().fg(palette::WHALE_ACTION).bold(),
                ),
                Span::styled(
                    "──────────────────────── ",
                    Style::default().fg(palette::BORDER_COLOR),
                ),
                Span::styled(
                    format!(
                        "{}  {}  ",
                        tr(self.locale, MessageId::SubagentsHeaderRoster),
                        tr(self.locale, MessageId::FleetRosterTabSetup),
                    ),
                    Style::default().fg(palette::TEXT_MUTED),
                ),
                Span::styled(
                    tr(self.locale, MessageId::FleetRosterWorkers),
                    Style::default().fg(palette::WHALE_ACTION).bold(),
                ),
                Span::styled(
                    " ─────────────────",
                    Style::default().fg(palette::BORDER_COLOR),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                format!("  {}", tr(self.locale, MessageId::SubagentsHeaderColumns)),
                Style::default().fg(palette::TEXT_MUTED),
            )),
        ])
        .render(shell[0], buf);

        let total_lines = lines.len();
        let visible_lines = usize::from(shell[1].height).max(1);
        let max_scroll = total_lines.saturating_sub(visible_lines);
        let scroll = self.scroll.min(max_scroll);

        // Cache the layout for Enter/click resolution and scroll-follow.
        self.row_lines.replace(row_lines);
        self.body_area.set(shell[1]);
        self.last_render_scroll.set(scroll);
        self.last_visible_lines.set(visible_lines);

        Paragraph::new(lines)
            .scroll((scroll as u16, 0))
            .render(shell[1], buf);
    }
}

/// Locale and working-wake frame for the whale badge on each worker row.
#[derive(Debug, Clone, Copy)]
struct WhaleRowContext {
    locale: Locale,
    frame: usize,
}

#[allow(clippy::too_many_arguments)]
fn append_subagent_group(
    lines: &mut Vec<ratatui::text::Line<'static>>,
    row_lines: &mut Vec<(usize, usize, String)>,
    title: &str,
    section_style: ratatui::style::Style,
    agents: &[&SubAgentResult],
    content_width: usize,
    selected_id: &str,
    whale: WhaleRowContext,
) {
    use ratatui::{
        style::Style,
        text::{Line, Span},
    };
    if agents.is_empty() {
        return;
    }

    lines.push(Line::from(Span::styled(
        tr(whale.locale, MessageId::SubagentsGroupHeading)
            .replace("{label}", title)
            .replace("{count}", &agents.len().to_string()),
        section_style.bold(),
    )));

    for agent in agents {
        let block_start = lines.len();
        let is_selected = agent.agent_id == selected_id;
        let id = truncate_view_text(&agent.agent_id, 11);
        let display_name = agent
            .nickname
            .as_deref()
            .map(|nick| format!("{nick:<12}"))
            .unwrap_or_else(|| format!("{id:<12}"));
        let kind = format_agent_type(whale.locale, &agent.agent_type);
        let (status, status_style, status_detail) =
            format_agent_status(whale.locale, &agent.status, agent.worker_status);

        let name_style = if is_selected {
            Style::default().fg(palette::WHALE_ACTION).bold()
        } else {
            Style::default().fg(palette::TEXT_PRIMARY)
        };
        // Whale Teams: species badge from the worker's Fleet role (or its
        // advisory role hint), then the six-state word derived from the
        // child's real status — never from elapsed time.
        let species = agent
            .assignment
            .role
            .as_deref()
            .map(crate::tui::whales::WhaleSpecies::for_role_id)
            .filter(|species| *species != crate::tui::whales::WhaleSpecies::Plain)
            .unwrap_or_else(|| crate::tui::whales::WhaleSpecies::for_fleet_role(&agent.agent_type));
        let whale_state = crate::tui::whales::WhaleState::for_subagent(agent);
        let mut row = vec![
            // The selection cursor: Enter (or a click) opens this agent's
            // transcript, matching every other agent surface.
            Span::styled(
                if is_selected { "\u{25B8} " } else { "  " },
                Style::default().fg(palette::WHALE_ACTION),
            ),
        ];
        row.extend(crate::tui::whales::badge(species, &palette::UI_THEME));
        row.push(Span::raw(" "));
        row.extend([
            Span::styled(display_name, name_style),
            Span::raw(" "),
            Span::styled(format!("{id:<11}"), Style::default().fg(palette::TEXT_DIM)),
            Span::styled(
                format!("{kind:<9}"),
                Style::default().fg(palette::TEXT_MUTED),
            ),
            Span::raw("  "),
            Span::styled(format!("{status:<10}"), status_style),
            Span::raw("  "),
            Span::styled(
                format!("{:>4}✦", agent.steps_taken),
                Style::default().fg(palette::TEXT_DIM),
            ),
            Span::raw("  "),
            Span::styled(
                format!("{:>6}ms", agent.duration_ms),
                Style::default().fg(palette::TEXT_DIM),
            ),
        ]);
        lines.push(Line::from(row));

        // The whale's own state word, paired with its glyph cue, so the row
        // says "Waiting for you" / "Blocked" in the user's language next to
        // the raw runtime status above. No caption text beyond that.
        let mut whale_line = vec![Span::raw("    ")];
        whale_line.extend(crate::tui::whales::badge_with_state_frame(
            species,
            Some(whale_state),
            whale.frame,
            &palette::UI_THEME,
            whale.locale,
        ));
        lines.push(Line::from(whale_line));

        if let Some(detail) = status_detail {
            let max_len = content_width.saturating_sub(10);
            let detail = truncate_view_text(detail, max_len);
            lines.push(Line::from(vec![
                Span::styled(
                    tr(whale.locale, MessageId::SubagentsLabelReason),
                    Style::default().fg(palette::TEXT_MUTED),
                ),
                Span::styled(detail, Style::default().fg(palette::WHALE_ERROR)),
            ]));
        }

        if let Some(role) = agent.assignment.role.as_deref() {
            let max_len = content_width.saturating_sub(14);
            let role = truncate_view_text(role, max_len);
            lines.push(Line::from(vec![
                Span::styled(
                    tr(whale.locale, MessageId::SubagentsLabelRole),
                    Style::default().fg(palette::TEXT_MUTED),
                ),
                Span::styled(role, Style::default().fg(palette::WHALE_ACTION)),
            ]));
        }

        if let Some(permissions) = agent.runtime_permissions.as_ref() {
            let network = tr(
                whale.locale,
                if permissions.network {
                    MessageId::SubagentsValueOn
                } else {
                    MessageId::SubagentsValueOff
                },
            );
            let shell = format_subagent_shell(whale.locale, &permissions.shell);
            let write = tr(
                whale.locale,
                if permissions.write {
                    MessageId::SubagentsValueOn
                } else {
                    MessageId::SubagentsValueOff
                },
            );
            let posture = tr(whale.locale, MessageId::SubagentsPostureDetails)
                .replace("{network}", network.as_ref())
                .replace("{shell}", shell.as_ref())
                .replace("{write}", write.as_ref());
            let max_len = content_width.saturating_sub(18);
            let posture = truncate_view_text(&posture, max_len);
            lines.push(Line::from(vec![
                Span::styled(
                    tr(whale.locale, MessageId::SubagentsLabelPosture),
                    Style::default().fg(palette::TEXT_MUTED),
                ),
                Span::styled(posture, Style::default().fg(palette::WHALE_ACTION)),
            ]));
        }

        if let Some(branch) = agent.git_branch.as_deref() {
            let workspace = agent
                .workspace
                .as_deref()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty());
            let branch_detail = match workspace {
                Some(workspace) => tr(whale.locale, MessageId::SubagentsBranchWithWorkspace)
                    .replace("{branch}", branch)
                    .replace("{workspace}", workspace),
                None => tr(whale.locale, MessageId::SubagentsBranch).replace("{branch}", branch),
            };
            let max_len = content_width.saturating_sub(14);
            let branch_detail = truncate_view_text(&branch_detail, max_len);
            lines.push(Line::from(vec![
                Span::styled(
                    tr(whale.locale, MessageId::SubagentsLabelGit),
                    Style::default().fg(palette::TEXT_MUTED),
                ),
                Span::styled(branch_detail, Style::default().fg(palette::WHALE_ACTION)),
            ]));
        }

        let max_len = content_width.saturating_sub(18);
        let objective = truncate_view_text(&agent.assignment.objective, max_len);
        lines.push(Line::from(vec![
            Span::styled(
                tr(whale.locale, MessageId::SubagentsLabelObjective),
                Style::default().fg(palette::TEXT_MUTED),
            ),
            Span::styled(objective, Style::default().fg(palette::TEXT_DIM)),
        ]));

        if let Some(result) = agent.result.as_ref() {
            let max_len = content_width.saturating_sub(16);
            let preview = truncate_view_text(result, max_len);
            lines.push(Line::from(vec![
                Span::styled(
                    tr(whale.locale, MessageId::SubagentsLabelResult),
                    Style::default().fg(palette::TEXT_MUTED),
                ),
                Span::styled(preview, Style::default().fg(palette::TEXT_DIM)),
            ]));
        }

        row_lines.push((
            block_start,
            lines.len() - block_start,
            agent.agent_id.clone(),
        ));
    }

    lines.push(Line::from(""));
}

fn agent_type_order(agent_type: &FleetRole) -> u8 {
    match agent_type {
        FleetRole::Worker => 0,
        FleetRole::Scout => 1,
        FleetRole::Planner => 2,
        FleetRole::Builder => 3,
        FleetRole::Verifier => 4,
        FleetRole::Reviewer => 5,
        FleetRole::Consultant => 6,
        FleetRole::Custom => 7,
    }
}

fn format_agent_type(locale: Locale, agent_type: &FleetRole) -> Cow<'static, str> {
    // `FleetRole::as_str()` is the durable runtime/schema identifier. The
    // register is a localized user surface, so map only the known roles here
    // and leave the internal identifier untouched everywhere else.
    let message_id = match agent_type {
        FleetRole::Worker => MessageId::SubagentsRoleWorker,
        FleetRole::Scout => MessageId::SubagentsRoleScout,
        FleetRole::Planner => MessageId::SubagentsRolePlanner,
        FleetRole::Builder => MessageId::SubagentsRoleBuilder,
        FleetRole::Verifier => MessageId::SubagentsRoleVerifier,
        FleetRole::Reviewer => MessageId::SubagentsRoleReviewer,
        FleetRole::Consultant => MessageId::SubagentsRoleConsultant,
        FleetRole::Custom => MessageId::SubagentsRoleCustom,
    };
    tr(locale, message_id)
}

fn format_agent_status(
    locale: Locale,
    status: &SubAgentStatus,
    worker_status: Option<crate::tools::subagent::AgentWorkerStatus>,
) -> (Cow<'static, str>, ratatui::style::Style, Option<&str>) {
    use ratatui::style::Style;

    match status {
        // A worker that has not started yet waits; it is not running.
        SubAgentStatus::Running
            if worker_status == Some(crate::tools::subagent::AgentWorkerStatus::Queued) =>
        {
            (
                tr(locale, MessageId::AutomationRunStatusQueued),
                Style::default().fg(palette::STATUS_WARNING),
                None,
            )
        }
        SubAgentStatus::Running => (
            tr(locale, MessageId::AutomationRunStatusRunning),
            Style::default().fg(palette::WHALE_ACTION),
            None,
        ),
        SubAgentStatus::Completed => (
            tr(locale, MessageId::AutomationRunStatusCompleted),
            Style::default().fg(palette::STATUS_SUCCESS),
            None,
        ),
        SubAgentStatus::Interrupted(reason) => (
            tr(locale, MessageId::SubagentsRowStatusInterrupted),
            Style::default().fg(palette::STATUS_WARNING),
            Some(reason.as_str()),
        ),
        SubAgentStatus::Cancelled => (
            tr(locale, MessageId::SubagentsRowStatusCancelled),
            Style::default().fg(palette::TEXT_MUTED),
            None,
        ),
        SubAgentStatus::BudgetExhausted => (
            tr(locale, MessageId::SubagentsRowStatusBudgetExhausted),
            Style::default().fg(palette::STATUS_WARNING),
            None,
        ),
        SubAgentStatus::Failed(reason) => (
            tr(locale, MessageId::AutomationRunStatusFailed),
            Style::default().fg(palette::WHALE_ERROR),
            Some(reason.as_str()),
        ),
    }
}

fn format_subagent_shell(locale: Locale, shell: &str) -> Cow<'static, str> {
    match shell {
        "none" => tr(locale, MessageId::SubagentsShellNone),
        "read_only" => tr(locale, MessageId::SubagentsShellReadOnly),
        "full" => tr(locale, MessageId::SubagentsShellFull),
        // A future runtime may add a posture before the TUI knows how to
        // localize it. Preserve that exact runtime value instead of guessing.
        _ => Cow::Owned(shell.to_string()),
    }
}

fn truncate_view_text(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    match text.char_indices().nth(max_chars) {
        Some((idx, _)) => text[..idx].to_string(),
        None => text.to_string(),
    }
}

fn fit_config_column(text: &str, width: usize) -> String {
    let mut fitted = crate::tui::ui_text::truncate_line_to_width(text, width);
    let padding = width.saturating_sub(crate::tui::ui_text::text_display_width(&fitted));
    fitted.push_str(&" ".repeat(padding));
    fitted
}

#[cfg(test)]
mod tests {
    use super::{
        ActionHint, ConfigCategory, ConfigListItem, ConfigRowKind, ConfigScope, ConfigView,
        EmptyState, FocusTextureMode, HelpView, ListDetailLayout, ModalKind, ModalView,
        SettingKind, SettingStore, SettingsRegistry, ViewAction, ViewEvent, ViewStack,
        action_footer_lines, canonical_config_choice, centered_modal_area, config_choice_detail,
        config_choice_label, config_choice_values, config_label_for_key,
        config_label_for_key_for_locale, render_modal_footer_with_gutter,
        render_underwater_surface, subagent_view_agents, truncate_view_text,
    };
    use crate::config::Config;
    use crate::settings::Settings;
    use crate::tools::subagent::{FleetRole, SubAgentAssignment, SubAgentResult, SubAgentStatus};
    use crate::tui::app::{App, TuiOptions};
    use crate::tui::history::{HistoryCell, SubAgentCell};
    use crate::tui::views::{CommandPaletteAction, SubAgentsView};
    use crate::tui::widgets::agent_card::{AgentLifecycle, FanoutCard};
    use codewhale_localization::{Locale, MessageId, tr, tr_key};
    use codewhale_palette as palette;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::{
        buffer::Buffer,
        layout::Rect,
        style::{Color, Style},
    };
    use std::borrow::Cow;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use unicode_width::UnicodeWidthStr;

    /// Terminal sizes the v0.8.66 modal blocker (#3732) requires every overlay
    /// to remain readable and fully operable at.
    const BLOCKER_SIZES: [(u16, u16); 4] = [(80, 24), (100, 30), (120, 32), (160, 40)];

    /// Render a modal through the `ViewStack` (so the shared opaque backdrop is
    /// painted exactly as in production) over a sentinel-filled buffer, then
    /// assert: every `required_label` is visible, no sentinel `X` survives
    /// anywhere (fully opaque), the center cell carries the modal ink, and no
    /// row overflows the frame width.
    fn assert_modal_usable_and_opaque<V: ModalView + 'static>(
        make: impl Fn() -> V,
        required_labels: &[&str],
    ) {
        for (w, h) in BLOCKER_SIZES {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            let sentinel_style = Style::default().fg(Color::Magenta).bg(Color::Green);
            for y in 0..h {
                for x in 0..w {
                    buf[(x, y)].set_symbol("X").set_style(sentinel_style);
                }
            }
            let mut stack = ViewStack::new();
            stack.push(make());
            stack.render(area, &mut buf);

            let rows: Vec<String> = (0..h)
                .map(|y| {
                    (0..w)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect();
            let text = rows.join("\n");

            for label in required_labels {
                assert!(text.contains(label), "{w}x{h}: missing '{label}'");
            }
            let unpainted = (0..h).find_map(|y| {
                (0..w).find_map(|x| {
                    let cell = &buf[(x, y)];
                    (cell.symbol() == "X" && cell.fg == Color::Magenta && cell.bg == Color::Green)
                        .then_some((x, y))
                })
            });
            assert!(
                unpainted.is_none(),
                "{w}x{h}: background bleed-through at {unpainted:?}"
            );
            assert_eq!(
                buf[(w / 2, h / 2)].bg,
                palette::WHALE_BG,
                "{w}x{h}: modal interior must be opaque"
            );
            for (y, row) in rows.iter().enumerate() {
                assert!(
                    UnicodeWidthStr::width(row.trim_end()) <= w as usize,
                    "{w}x{h}: row {y} overflows width: {row:?}"
                );
            }
        }
    }

    #[test]
    fn config_modal_is_usable_and_opaque_at_blocker_sizes() {
        let _lock = crate::test_support::lock_test_env();
        // "Search" is the hardcoded English search-row label; asserting it (plus
        // the opacity/overflow checks) proves the modal renders fully and its
        // footer wraps inside bounds rather than clipping.
        assert_modal_usable_and_opaque(|| create_config_view(Locale::En), &["Search"]);
    }

    #[test]
    fn subagents_modal_is_usable_and_opaque_at_blocker_sizes() {
        assert_modal_usable_and_opaque(
            || SubAgentsView::new(Vec::new()),
            &["close", "refresh", "setup"],
        );
    }

    /// #5954: the `Esc` hint has to name what the key does — `back` while the
    /// Fleet roster is parked underneath, `close` on direct entry.
    #[test]
    fn subagents_esc_hint_says_back_over_the_roster_and_close_at_the_root() {
        let area = Rect::new(0, 0, 160, 40);

        let direct = SubAgentsView::new(Vec::new());
        let mut direct_buf = Buffer::empty(area);
        direct.render(area, &mut direct_buf);
        let direct_text = buffer_text(&direct_buf, area);
        assert!(
            direct_text.contains("Esc close"),
            "direct entry must still promise close: {direct_text}"
        );

        let over_roster = SubAgentsView::new(Vec::new()).over_fleet_roster();
        let mut back_buf = Buffer::empty(area);
        over_roster.render(area, &mut back_buf);
        let back_text = buffer_text(&back_buf, area);
        assert!(
            back_text.contains("Esc back"),
            "over the roster the hint must promise back: {back_text}"
        );
        assert!(
            !back_text.contains("Esc close"),
            "over the roster the hint must not still promise close: {back_text}"
        );
    }

    /// #5954: workers opened directly (`/fleet workers`, the Work dock) is the
    /// root of its own stack, so `Esc` closes the window as before.
    #[test]
    fn subagents_opened_directly_closes_on_esc() {
        let mut stack = ViewStack::new();
        stack.push(SubAgentsView::new(Vec::new()));
        stack.handle_key(KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(stack.is_empty(), "direct entry must close on Esc");
    }

    /// #5954: `F` in workers is the roster door. With a roster parked below,
    /// it pops back to it instead of re-running `/fleet` and stacking a
    /// duplicate roster.
    #[test]
    fn subagents_f_pops_back_to_the_parked_roster() {
        let mut over_roster = SubAgentsView::new(Vec::new()).over_fleet_roster();
        assert!(matches!(
            over_roster.handle_key(KeyEvent::new(
                crossterm::event::KeyCode::Char('F'),
                crossterm::event::KeyModifiers::NONE,
            )),
            ViewAction::Close
        ));

        let mut direct = SubAgentsView::new(Vec::new());
        assert!(matches!(
            direct.handle_key(KeyEvent::new(
                crossterm::event::KeyCode::Char('F'),
                crossterm::event::KeyModifiers::NONE,
            )),
            ViewAction::Emit(ViewEvent::CommandPaletteSelected { .. })
        ));
    }

    #[test]
    fn subagents_modal_names_current_session_pod_workers_in_each_locale() {
        let area = Rect::new(0, 0, 160, 40);
        let app = create_test_app();

        let empty = SubAgentsView::for_app(&app, Vec::new());
        let mut empty_buf = Buffer::empty(area);
        empty.render(area, &mut empty_buf);
        let empty_text = buffer_text(&empty_buf, area);
        assert!(
            empty_text.contains("No agents in this session."),
            "{empty_text}"
        );
        assert!(
            empty_text.contains("Set up roles with /fleet."),
            "{empty_text}"
        );

        let english = SubAgentsView::for_app(
            &app,
            vec![manager_agent("agent_live", SubAgentStatus::Running)],
        );
        let mut english_buf = Buffer::empty(area);
        english.render(area, &mut english_buf);
        let english_text = buffer_text(&english_buf, area);
        assert!(
            english_text.contains("Agents in this session"),
            "{english_text}"
        );
        assert!(
            english_text.contains("Roles shown are this session's agent roles."),
            "{english_text}"
        );

        let mut zh_hans_app = create_test_app();
        zh_hans_app.ui_locale = Locale::ZhHans;
        let zh_hans = SubAgentsView::for_app(
            &zh_hans_app,
            vec![manager_agent("agent_live", SubAgentStatus::Running)],
        );
        let mut zh_hans_buf = Buffer::empty(area);
        zh_hans.render(area, &mut zh_hans_buf);
        let zh_hans_text = buffer_text(&zh_hans_buf, area);
        // Ratatui gives each CJK glyph a trailing buffer cell. Strip those
        // layout spaces before asserting the actual rendered copy.
        let zh_hans_compact = zh_hans_text
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>();
        assert_eq!(
            tr(
                Locale::ZhHans,
                MessageId::SubagentsCurrentSessionFleetWorkersTitle
            ),
            "本会话的智能体"
        );
        assert!(zh_hans_compact.contains("本会话的智能体"), "{zh_hans_text}");
        assert!(
            zh_hans_compact.contains("所示角色为本会话中智能体的角色。"),
            "{zh_hans_text}"
        );
        assert!(
            !zh_hans_text.contains("Agents in this session"),
            "{zh_hans_text}"
        );
    }

    #[test]
    fn subagents_modal_localizes_worker_rows_and_preserves_english_status_rendering() {
        let area = Rect::new(0, 0, 200, 60);
        let mut agent = manager_agent("agent_live", SubAgentStatus::Running);
        agent.agent_type = FleetRole::Builder;
        agent.assignment.role = Some("release".to_string());
        agent.assignment.objective = "verify localized row".to_string();
        agent.runtime_permissions = Some(codewhale_protocol::fleet::FleetEffectivePermissions {
            write: true,
            network: true,
            shell: "read_only".to_string(),
            tool_scope: "inherit".to_string(),
            tools: Vec::new(),
            background: false,
            max_spawn_depth: 0,
            profile_id: None,
            profile_origin: None,
            source: "test".to_string(),
        });
        agent.git_branch = Some("feature/localize".to_string());
        agent.workspace = Some(PathBuf::from("/tmp/fleet-workers"));
        agent.result = Some("all checks passed".to_string());
        let mut interrupted = manager_agent(
            "agent_interrupted",
            SubAgentStatus::Interrupted("manual review".to_string()),
        );
        interrupted.agent_type = FleetRole::Reviewer;

        let app = create_test_app();
        let english = SubAgentsView::for_app(&app, vec![agent.clone(), interrupted.clone()]);
        let mut english_buf = Buffer::empty(area);
        english.render(area, &mut english_buf);
        let english_text = buffer_text(&english_buf, area);
        for expected in [
            "Agents in this session",
            "Running: 1",
            "Completed: 0",
            "Interrupted: 1",
            "Failed: 0",
            "Cancelled: 0",
            "Running (1)",
            "implement",
            "running",
            "reason: manual review",
            "role: release",
            "access: network=on · shell=read-only · write=on",
            "git: branch feature/localize @ fleet-workers",
            "objective: verify localized row",
            "result: all checks passed",
            "live agent status · role · objective · model · elapsed",
            "close",
            "select",
            "focus",
            "stop",
            "refresh",
            "fleet/setup",
        ] {
            assert!(
                english_text.contains(expected),
                "missing {expected:?}: {english_text}"
            );
        }

        let mut zh_hans_app = create_test_app();
        zh_hans_app.ui_locale = Locale::ZhHans;
        let zh_hans = SubAgentsView::for_app(&zh_hans_app, vec![agent, interrupted]);
        let mut zh_hans_buf = Buffer::empty(area);
        zh_hans.render(area, &mut zh_hans_buf);
        let zh_hans_text = buffer_text(&zh_hans_buf, area);
        let zh_hans_compact = zh_hans_text
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>();
        for expected in [
            "本会话的智能体",
            "运行中：1",
            "已中断：1",
            "Fleet设置智能体",
            "实时智能体状态·角色·目标·模型·已用时间",
            "运行中（1）",
            "构建者",
            "原因：manualreview",
            "角色：release",
            "访问级别：网络=开·Shell=只读·写入=开",
            "Git：分支feature/localize@fleet-workers",
            "目标：verifylocalizedrow",
            "结果：allcheckspassed",
            "刷新",
            "fleet/setup",
        ] {
            assert!(
                zh_hans_compact.contains(expected),
                "missing {expected:?}: {zh_hans_text}"
            );
        }
        assert!(
            !zh_hans_text.contains("Running: 1"),
            "English status leaked into zh-Hans modal: {zh_hans_text}"
        );
    }

    /// Focus-texture prototype (#4823): with a mode forced on, a real
    /// full-screen modal must render exactly as before — the texture pass
    /// no-ops because the focus region covers (nearly) the whole frame.
    /// The default `Off` case is pinned by the existing
    /// `*_modal_is_usable_and_opaque_at_blocker_sizes` tests above: they run
    /// unmodified because `ViewStack::new()` defaults to `Off`, which leaves
    /// the buffer byte-identical to the pre-prototype render.
    #[test]
    fn focus_texture_modes_keep_fullscreen_modal_usable_and_opaque() {
        let _lock = crate::test_support::lock_test_env();
        let theme = codewhale_palette::ThemeId::Whale.ui_theme();
        for mode in [FocusTextureMode::Scrim, FocusTextureMode::Grain] {
            for (w, h) in BLOCKER_SIZES {
                let area = Rect::new(0, 0, w, h);
                let mut buf = Buffer::empty(area);
                let sentinel_style = Style::default().fg(Color::Magenta).bg(Color::Green);
                for y in 0..h {
                    for x in 0..w {
                        buf[(x, y)].set_symbol("X").set_style(sentinel_style);
                    }
                }
                let mut stack = ViewStack::new();
                stack.push(create_config_view(Locale::En));
                stack.set_focus_texture(mode, theme);
                stack.render(area, &mut buf);

                let rows: Vec<String> = (0..h)
                    .map(|y| {
                        (0..w)
                            .map(|x| buf[(x, y)].symbol().to_string())
                            .collect::<String>()
                    })
                    .collect();
                let text = rows.join("\n");

                assert!(
                    text.contains("Search"),
                    "{mode:?} {w}x{h}: missing 'Search'"
                );
                let unpainted = (0..h).find_map(|y| {
                    (0..w).find_map(|x| {
                        let cell = &buf[(x, y)];
                        (cell.symbol() == "X"
                            && cell.fg == Color::Magenta
                            && cell.bg == Color::Green)
                            .then_some((x, y))
                    })
                });
                assert!(
                    unpainted.is_none(),
                    "{mode:?} {w}x{h}: background bleed-through at {unpainted:?}"
                );
                assert_eq!(
                    buf[(w / 2, h / 2)].bg,
                    palette::WHALE_BG,
                    "{mode:?} {w}x{h}: modal interior must be opaque"
                );
            }
        }
    }

    /// The texture actually engages outside an *inline* modal's band: the
    /// approval prompt only occupies a bottom strip, so the sentinel field
    /// above it goes through the scrim/grain pass. The modal is painted
    /// after the texture, so its band stays fully opaque and its labels
    /// survive at every blocker size.
    #[test]
    fn focus_texture_modes_keep_inline_modal_usable() {
        let theme = codewhale_palette::ThemeId::Whale.ui_theme();
        for mode in [FocusTextureMode::Scrim, FocusTextureMode::Grain] {
            for (w, h) in BLOCKER_SIZES {
                let area = Rect::new(0, 0, w, h);
                let mut buf = Buffer::empty(area);
                let sentinel_style = Style::default().fg(Color::Magenta).bg(Color::Green);
                for y in 0..h {
                    for x in 0..w {
                        buf[(x, y)].set_symbol("X").set_style(sentinel_style);
                    }
                }
                let request = crate::tui::approval::ApprovalRequest::new(
                    "test-id",
                    "read_file",
                    "Read a file from disk",
                    &serde_json::json!({"path": "src/main.rs"}),
                    "tool:read_file",
                );
                let mut stack = ViewStack::new();
                stack.push(crate::tui::approval::ApprovalView::new(request));
                stack.set_focus_texture(mode, theme);
                let focus = stack
                    .top_occupied_region(area)
                    .expect("approval view on the stack");
                stack.render(area, &mut buf);

                let rows: Vec<String> = (0..h)
                    .map(|y| {
                        (0..w)
                            .map(|x| buf[(x, y)].symbol().to_string())
                            .collect::<String>()
                    })
                    .collect();
                let text = rows.join("\n");

                // The card heading is the plain summary of the call (E6,
                // mark 4), not the raw tool name.
                assert!(
                    text.contains("Do you want to proceed?") && text.contains("Read src/main.rs"),
                    "{mode:?} {w}x{h}: approval prompt must survive the texture"
                );
                // Zero sentinel bleed INSIDE the focused band: the backdrop
                // and the modal own every cell there. Outside the band the
                // texture intentionally leaves the sentinel glyphs in place
                // (Scrim only re-colors; Grain never overwrites text).
                let mut whale_bg_cells = 0_u32;
                for y in focus.top()..focus.bottom() {
                    for x in focus.left()..focus.right() {
                        let cell = &buf[(x, y)];
                        assert!(
                            !(cell.symbol() == "X"
                                && cell.fg == Color::Magenta
                                && cell.bg == Color::Green),
                            "{mode:?} {w}x{h}: sentinel bleed inside focus at ({x},{y})"
                        );
                        if cell.bg == palette::WHALE_BG {
                            whale_bg_cells += 1;
                        }
                    }
                }
                // The band keeps the opaque modal ink. (Not every cell: the
                // selected option row carries its own highlight background.)
                assert!(
                    whale_bg_cells > 0,
                    "{mode:?} {w}x{h}: modal band lost its opaque WHALE_BG surface"
                );
            }
        }
    }

    #[test]
    fn centered_modal_area_clamps_and_centers() {
        // Roomy frame: preferred size honoured, centered.
        let area = Rect::new(0, 0, 160, 40);
        let rect = centered_modal_area(area, 80, 20, 40, 10);
        assert_eq!((rect.width, rect.height), (80, 20));
        assert_eq!(rect.x, (160 - 80) / 2);
        assert_eq!(rect.y, (40 - 20) / 2);

        // Tiny frame: never exceeds the frame even below the requested minimum.
        let tiny = Rect::new(0, 0, 30, 8);
        let rect = centered_modal_area(tiny, 80, 20, 40, 10);
        assert!(rect.width <= tiny.width, "width must fit frame");
        assert!(rect.height <= tiny.height, "height must fit frame");
        assert!(rect.x + rect.width <= tiny.width);
        assert!(rect.y + rect.height <= tiny.height);
    }

    #[test]
    fn action_footer_wraps_instead_of_overflowing() {
        let hints = [
            ActionHint::new("↑↓", "move"),
            ActionHint::new("a-z", "jump"),
            ActionHint::new("Enter", "apply"),
            ActionHint::new("R", "edit key"),
            ActionHint::new("M", "models"),
            ActionHint::new("Esc", "cancel"),
        ];

        // Wide enough for a single row.
        let wide = action_footer_lines(&hints, 120);
        assert_eq!(wide.len(), 1);
        assert!(wide[0].width() <= 120);

        // Narrow forces wrapping but never truncates: every action survives and
        // no produced line exceeds the available width.
        let narrow = action_footer_lines(&hints, 28);
        assert!(narrow.len() >= 2, "narrow footer should wrap to >1 row");
        for line in &narrow {
            assert!(
                line.width() <= 28,
                "wrapped footer row overflows: {} cols",
                line.width()
            );
        }
        let joined: String = narrow
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        for label in ["move", "jump", "apply", "edit key", "models", "cancel"] {
            assert!(joined.contains(label), "footer dropped action: {label}");
        }
    }

    #[test]
    fn render_modal_footer_reserves_rows_and_returns_body() {
        let inner = Rect::new(2, 2, 40, 10);
        let mut buf = Buffer::empty(Rect::new(0, 0, 44, 14));
        let hints = [
            ActionHint::new("Enter", "save"),
            ActionHint::new("Esc", "cancel"),
        ];
        let body = render_modal_footer_with_gutter(inner, &mut buf, &hints);
        // Normal-height overlays reserve a single quiet gutter above the
        // one-row footer, so body prose never runs into the action rail.
        assert_eq!(body.y, inner.y);
        assert_eq!(body.height, inner.height - 2);
        assert_eq!(body.y + body.height, inner.y + inner.height - 2);
        let gutter_y = inner.y + inner.height - 2;
        assert!(
            (inner.x..inner.right()).all(|x| buf[(x, gutter_y)].symbol().trim().is_empty()),
            "modal footer gutter should stay visually quiet"
        );
    }

    #[test]
    fn list_detail_layout_splits_wide_and_stacks_narrow() {
        let wide = ListDetailLayout::split(Rect::new(0, 0, 120, 24), 34);
        assert!(!wide.stacked);
        assert!(wide.list.width >= 30);
        assert!(wide.detail.width >= 34);
        assert_eq!(wide.list.height, 24);
        assert_eq!(wide.detail.height, 24);
        assert!(wide.list.right() < wide.detail.left());

        let narrow = ListDetailLayout::split(Rect::new(0, 0, 80, 20), 34);
        assert!(narrow.stacked);
        assert_eq!(narrow.list.width, 80);
        assert_eq!(narrow.detail.width, 80);
        assert!(narrow.list.bottom() <= narrow.detail.top());
        assert!(narrow.list.height > 0);
    }

    #[test]
    fn empty_state_renders_copy_and_actions() {
        let area = Rect::new(0, 0, 48, 8);
        let mut buf = Buffer::empty(area);
        EmptyState::new("Nothing here", "Use search or switch categories.")
            .primary_action("/", "filter")
            .secondary_action("Esc", "cancel")
            .render(area, &mut buf);

        let text = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        for expected in ["Nothing here", "Use search", "filter", "cancel"] {
            assert!(
                text.contains(expected),
                "empty state missing {expected:?}: {text:?}"
            );
        }
    }

    struct ConfigSettingsEnvGuard {
        _config_path: crate::test_support::EnvVarGuard,
        _tmp: TempDir,
        _lock: crate::test_support::TestEnvLock,
    }

    impl ConfigSettingsEnvGuard {
        fn new(settings_toml: &str) -> Self {
            let lock = crate::test_support::lock_test_env();
            let tmp = TempDir::new().expect("settings tempdir");
            let config_path = tmp.path().join(".deepseek").join("config.toml");
            let settings_path = config_path
                .parent()
                .expect("settings parent")
                .join("settings.toml");
            std::fs::create_dir_all(config_path.parent().expect("config parent"))
                .expect("config dir");
            std::fs::write(&settings_path, settings_toml).expect("settings file");
            let config_path_guard =
                crate::test_support::EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
            Self {
                _config_path: config_path_guard,
                _tmp: tmp,
                _lock: lock,
            }
        }
    }

    fn create_test_app() -> App {
        static NEXT_CONFIG_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let config_id = NEXT_CONFIG_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let isolated_config_path = std::env::temp_dir().join(format!(
            "codewhale-config-view-test-{}-{config_id}.toml",
            std::process::id()
        ));
        let options = TuiOptions {
            // ConfigView consults the app's persisted config. Point generic
            // tests at a unique absent file so developer or concurrent test
            // settings cannot silently change which controls are editable.
            config_path: Some(isolated_config_path),
            ..crate::test_support::test_tui_options(PathBuf::from("."))
        };
        let mut app = App::new(options, &Config::default());
        app.api_provider = crate::config::ProviderKind::Deepseek;
        app
    }

    fn cost_currency_row_for_settings(
        settings_toml: &str,
    ) -> (String, String, crate::pricing::CostCurrency, Locale) {
        let _guard = ConfigSettingsEnvGuard::new(settings_toml);
        let app = create_test_app();
        let view = ConfigView::new_for_app(&app);
        let row = view
            .rows
            .iter()
            .find(|row| row.key == "cost_currency")
            .expect("cost_currency row");

        (
            row.value.clone(),
            view.row_display_value(row),
            app.cost_currency,
            app.ui_locale,
        )
    }

    fn type_filter(view: &mut ConfigView, text: &str) {
        for ch in text.chars() {
            let action = view.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
            assert!(matches!(action, ViewAction::None));
        }
    }

    fn manager_agent(id: &str, status: SubAgentStatus) -> SubAgentResult {
        SubAgentResult {
            usage: None,
            name: id.to_string(),
            agent_id: id.to_string(),
            context_mode: "fresh".to_string(),
            fork_context: false,
            workspace: None,
            git_branch: None,
            agent_type: FleetRole::Scout,
            assignment: SubAgentAssignment {
                native_preset: None,
                objective: "read the docs".to_string(),
                role: None,
            },
            model: "deepseek-v4-flash".to_string(),
            nickname: None,
            status,
            worker_status: None,
            runtime_permissions: None,
            parent_run_id: None,
            spawn_depth: 0,
            child_route: None,
            result: None,
            steps_taken: 1,
            checkpoint: None,
            needs_input: None,
            duration_ms: 10,
            started_at: None,
            from_prior_session: false,
            idle_ms: None,
            heartbeat_timeout_ms: None,
        }
    }

    fn writer_agent(id: &str) -> SubAgentResult {
        let mut agent = manager_agent(id, SubAgentStatus::Running);
        agent.runtime_permissions = Some(codewhale_protocol::fleet::FleetEffectivePermissions {
            write: true,
            network: false,
            shell: "read_only".to_string(),
            tool_scope: "inherit".to_string(),
            tools: Vec::new(),
            background: false,
            max_spawn_depth: 0,
            profile_id: None,
            profile_origin: None,
            source: "test".to_string(),
        });
        agent
    }

    fn press(view: &mut SubAgentsView, code: KeyCode) -> ViewAction {
        view.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn is_stop_of(action: &ViewAction, id: &str) -> bool {
        matches!(
            action,
            ViewAction::Emit(ViewEvent::SidebarAgentCancel { agent_id }) if agent_id == id
        )
    }

    #[test]
    fn stopping_a_writing_agent_takes_two_presses_and_esc_disarms() {
        let mut view = SubAgentsView::new(vec![writer_agent("w")]);

        // First X arms; nothing is stopped yet and the footer asks to confirm.
        assert!(matches!(
            press(&mut view, KeyCode::Char('x')),
            ViewAction::None
        ));
        let area = Rect::new(0, 0, 100, 20);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        assert!(buffer_text(&buf, area).contains("X/Enter"));

        // Esc disarms without closing the register.
        assert!(matches!(press(&mut view, KeyCode::Esc), ViewAction::None));
        assert!(view.armed_stop.is_none());

        // X, X stops; X then Enter stops too.
        assert!(matches!(
            press(&mut view, KeyCode::Char('X')),
            ViewAction::None
        ));
        assert!(is_stop_of(&press(&mut view, KeyCode::Char('X')), "w"));
        assert!(matches!(
            press(&mut view, KeyCode::Char('x')),
            ViewAction::None
        ));
        assert!(is_stop_of(&press(&mut view, KeyCode::Enter), "w"));
        assert!(view.armed_stop.is_none());
    }

    #[test]
    fn stopping_a_read_only_or_moved_selection_does_not_need_the_armed_press() {
        // A read-only running agent (no write, no full shell) stops at once.
        let mut read_only = writer_agent("r");
        if let Some(permissions) = read_only.runtime_permissions.as_mut() {
            permissions.write = false;
        }
        let mut view = SubAgentsView::new(vec![read_only]);
        assert!(is_stop_of(&press(&mut view, KeyCode::Char('x')), "r"));

        // Moving the selection disarms: the next X on the other writer arms
        // afresh instead of stopping it.
        let mut view = SubAgentsView::new(vec![writer_agent("a"), writer_agent("b")]);
        assert!(matches!(
            press(&mut view, KeyCode::Char('x')),
            ViewAction::None
        ));
        press(&mut view, KeyCode::Down);
        assert!(view.armed_stop.is_none());
        assert!(matches!(
            press(&mut view, KeyCode::Char('x')),
            ViewAction::None
        ));
        assert!(is_stop_of(&press(&mut view, KeyCode::Char('x')), "b"));

        // An armed agent that finishes before the confirm is disarmed.
        let mut view = SubAgentsView::new(vec![writer_agent("w")]);
        press(&mut view, KeyCode::Char('x'));
        let mut done = writer_agent("w");
        done.status = SubAgentStatus::Completed;
        view.update_subagents(&[done]);
        assert!(view.armed_stop.is_none());
    }

    #[test]
    fn worker_register_update_preserves_selected_agent_across_new_spawns() {
        let mut view = SubAgentsView::new(vec![manager_agent("b", SubAgentStatus::Running)]);
        view.update_subagents(&[
            manager_agent("a", SubAgentStatus::Running),
            manager_agent("b", SubAgentStatus::Running),
        ]);
        assert_eq!(view.ordered_agent_ids()[view.selected], "b");
        view.update_subagents(&[
            manager_agent("a", SubAgentStatus::Running),
            manager_agent("b", SubAgentStatus::Completed),
        ]);
        assert_eq!(view.ordered_agent_ids()[view.selected], "b");
    }

    #[test]
    fn subagent_view_agents_includes_progress_only_running_agent() {
        let mut app = create_test_app();
        app.ensure_agent_label("agent_live");
        app.agent_progress
            .insert("agent_live".to_string(), "reading code".to_string());

        let agents = subagent_view_agents(&app, &[]);

        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].agent_id, "agent_live");
        assert!(matches!(agents[0].status, SubAgentStatus::Running));
        assert_eq!(agents[0].assignment.role.as_deref(), Some("live"));
        assert!(agents[0].assignment.objective.contains("reading code"));
        assert_eq!(agents[0].nickname.as_deref(), Some("Agent 1"));
    }

    #[test]
    fn subagent_view_replaces_progress_placeholder_after_manager_snapshot() {
        let mut app = create_test_app();
        app.ui_locale = Locale::En;
        app.ensure_agent_label("agent_live");
        app.agent_progress
            .insert("agent_live".to_string(), "reading code".to_string());

        let progress_only = subagent_view_agents(&app, &[]);
        assert_eq!(progress_only[0].nickname.as_deref(), Some("Agent 1"));

        let mut manager = manager_agent("agent_live", SubAgentStatus::Running);
        manager.nickname = Some(crate::tools::subagent::whale_name_for_id_in_locale(
            "agent_live",
            "ja",
        ));
        let manager_backed = subagent_view_agents(&app, &[manager]);
        assert_eq!(
            manager_backed[0].nickname.as_deref(),
            Some(crate::tools::subagent::whale_name_for_id_in_locale("agent_live", "en").as_str())
        );
    }

    #[test]
    fn subagent_view_headlines_the_dispatch_name_over_the_whale() {
        // #5287: `/subagents` spells the identity column from `nickname`, so a
        // named dispatch lands there and only an unnamed one gets a whale.
        let mut app = create_test_app();
        app.ui_locale = Locale::En;
        let mut named = manager_agent("agent_named_lane", SubAgentStatus::Running);
        named.name = "branch-triage".to_string();
        let plain = manager_agent("agent_plain_lane", SubAgentStatus::Running);

        let agents = subagent_view_agents(&app, &[named, plain]);
        assert_eq!(agents[0].nickname.as_deref(), Some("branch-triage"));
        assert_eq!(
            agents[1].nickname.as_deref(),
            Some(
                crate::tools::subagent::whale_name_for_id_in_locale("agent_plain_lane", "en")
                    .as_str()
            )
        );
    }

    #[test]
    fn subagent_view_agents_includes_live_fanout_workers_when_cache_is_empty() {
        let mut app = create_test_app();
        let mut card = FanoutCard::new("rlm").with_workers(["chunk_1", "chunk_2"]);
        card.upsert_worker("chunk_1", AgentLifecycle::Completed);
        card.upsert_worker("chunk_2", AgentLifecycle::Running);
        app.add_message(HistoryCell::SubAgent(SubAgentCell::Fanout(card)));
        app.last_fanout_card_index = Some(app.history.len().saturating_sub(1));

        let agents = subagent_view_agents(&app, &[]);

        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].agent_id, "chunk_1");
        assert!(matches!(agents[0].status, SubAgentStatus::Completed));
        assert_eq!(agents[1].agent_id, "chunk_2");
        assert!(matches!(agents[1].status, SubAgentStatus::Running));
        assert_eq!(agents[1].assignment.role.as_deref(), Some("rlm"));
    }

    #[test]
    fn subagent_view_agents_deduplicates_manager_rows_over_live_rows() {
        let mut app = create_test_app();
        app.agent_progress
            .insert("agent_cached".to_string(), "live duplicate".to_string());
        let manager = vec![manager_agent("agent_cached", SubAgentStatus::Running)];

        let agents = subagent_view_agents(&app, &manager);

        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].agent_type, FleetRole::Scout);
        assert_eq!(agents[0].assignment.objective, "read the docs");
    }

    #[test]
    fn fleet_worker_status_view_can_jump_to_fleet_setup() {
        let mut view = SubAgentsView::new(Vec::new());

        let action = view.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));

        match action {
            ViewAction::Emit(ViewEvent::CommandPaletteSelected {
                action: CommandPaletteAction::ExecuteCommand { command },
            }) => assert_eq!(command, "/fleet"),
            other => panic!("expected /fleet jump action, got {other:?}"),
        }
    }

    /// One agent, one destination (v0.9.7): Enter on a `/agents` row opens
    /// the selected agent's transcript — the same destination the Work strip
    /// and sidebar resolve to. Selection follows render order (running before
    /// completed), and an empty register keeps Enter's refresh meaning.
    #[test]
    fn subagents_enter_opens_the_selected_agents_transcript() {
        let mut view = SubAgentsView::new(vec![
            manager_agent("agent_done", SubAgentStatus::Completed),
            manager_agent("agent_live", SubAgentStatus::Running),
        ]);

        // Render order groups running first, so the initial selection is the
        // running agent even though the completed one was pushed first.
        match view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            ViewAction::Emit(ViewEvent::OpenAgentTranscript { agent_id }) => {
                assert_eq!(agent_id, "agent_live");
            }
            other => panic!("expected transcript open, got {other:?}"),
        }

        let _ = view.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        match view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            ViewAction::Emit(ViewEvent::OpenAgentTranscript { agent_id }) => {
                assert_eq!(agent_id, "agent_done");
            }
            other => panic!("expected transcript open, got {other:?}"),
        }

        let mut empty = SubAgentsView::new(Vec::new());
        assert!(matches!(
            empty.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ViewAction::Emit(ViewEvent::SubAgentsRefresh)
        ));
    }

    /// Whale Teams rows: every worker carries its species badge and a state
    /// word derived from the real status (running → Working, completed →
    /// Resting, failed → Blocked, interrupted → Waiting for you), and the
    /// working wake holds the poster frame outside Full motion.
    #[test]
    fn subagents_rows_carry_species_badges_and_truthful_state_words() {
        let mut interrupted = manager_agent("agent_wait", SubAgentStatus::Interrupted("q".into()));
        interrupted.agent_type = FleetRole::Builder;
        let mut failed = manager_agent("agent_fail", SubAgentStatus::Failed("boom".into()));
        failed.agent_type = FleetRole::Reviewer;
        let view = SubAgentsView::new(vec![
            manager_agent("agent_done", SubAgentStatus::Completed),
            manager_agent("agent_live", SubAgentStatus::Running),
            interrupted,
            failed,
        ]);
        assert_eq!(view.whale_frame(), 0, "Still motion holds the poster frame");
        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let text = buffer_text(&buf, area);
        // Scout (manager_agent default role) → beak badge; Builder → Patch
        // bracket; Reviewer → Lantern lens.
        assert!(text.contains("◂▰ agent_live"), "{text}");
        assert!(text.contains("◂▰ · Working"), "{text}");
        assert!(text.contains("◂▰ Resting"), "{text}");
        assert!(text.contains("▰] ◆ Waiting for you"), "{text}");
        assert!(text.contains("◇▰ ▌ Blocked"), "{text}");
        assert!(
            !text.contains("Scout · research"),
            "no caption labels: {text}"
        );
        assert!(
            !text.contains("Lantern · review"),
            "no caption labels: {text}"
        );
    }

    /// A click on a rendered `/agents` row opens the clicked agent's
    /// transcript and moves the selection cursor onto it.
    #[test]
    fn subagents_click_opens_the_clicked_agents_transcript() {
        let mut view = SubAgentsView::new(vec![
            manager_agent("agent_done", SubAgentStatus::Completed),
            manager_agent("agent_live", SubAgentStatus::Running),
        ]);
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        // Resolve the completed agent's on-screen row from the recorded
        // layout, exactly as a click does in reverse.
        let (first_line, _, agent_id) = view
            .row_lines
            .borrow()
            .iter()
            .find(|(_, _, id)| id == "agent_done")
            .cloned()
            .expect("completed agent block recorded");
        assert_eq!(agent_id, "agent_done");
        let body = view.body_area.get();
        let scroll = view.last_render_scroll.get();
        let click_row = body.y + u16::try_from(first_line - scroll).expect("visible row");

        let action = view.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: body.x + 2,
            row: click_row,
            modifiers: KeyModifiers::NONE,
        });
        match action {
            ViewAction::Emit(ViewEvent::OpenAgentTranscript { agent_id }) => {
                assert_eq!(agent_id, "agent_done");
            }
            other => panic!("expected transcript open, got {other:?}"),
        }
        assert_eq!(view.ordered_agent_ids()[view.selected], "agent_done");

        // The selection cursor is visible after a re-render.
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let text = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains('\u{25B8}'),
            "selection cursor missing:\n{text}"
        );
    }

    fn visible_section_labels(view: &ConfigView) -> Vec<Cow<'static, str>> {
        view.visible_items()
            .into_iter()
            .filter_map(|item| match item {
                ConfigListItem::Section(section) => Some(section.label(view.locale)),
                ConfigListItem::Row(_) => None,
            })
            .collect()
    }

    fn create_config_view(locale: Locale) -> ConfigView {
        let mut app = create_test_app();
        app.ui_locale = locale;
        ConfigView::new_for_app(&app)
    }

    fn visible_row_keys(view: &ConfigView) -> Vec<&str> {
        view.visible_items()
            .into_iter()
            .filter_map(|item| match item {
                ConfigListItem::Row(idx) => Some(view.rows[idx].key.as_str()),
                ConfigListItem::Section(_) => None,
            })
            .collect()
    }

    #[test]
    fn truncate_view_text_handles_unicode() {
        let text = "abc😀é";
        assert_eq!(truncate_view_text(text, 0), "");
        assert_eq!(truncate_view_text(text, 1), "a");
        assert_eq!(truncate_view_text(text, 3), "abc");
        assert_eq!(truncate_view_text(text, 4), "abc😀");
        assert_eq!(truncate_view_text(text, 5), "abc😀é");
    }

    #[test]
    fn underwater_surface_ellipsizes_narrow_titles() {
        let area = Rect::new(0, 0, 24, 8);
        let mut buf = Buffer::empty(area);
        render_underwater_surface(area, &mut buf, "Help — Concepts, commands, and keybindings");
        let top = (0..area.width)
            .map(|x| buf[(x, 0)].symbol())
            .collect::<String>();
        assert!(
            top.contains('…'),
            "narrow title should signal truncation: {top}"
        );
    }

    #[test]
    fn config_view_groups_rows_by_expected_sections() {
        let view = create_config_view(Locale::En);
        assert_eq!(
            visible_section_labels(&view),
            vec!["Display"],
            "Settings opens on Appearance"
        );
    }

    #[test]
    fn config_view_includes_expected_editable_rows() {
        let app = create_test_app();
        let view = ConfigView::new_for_app(&app);
        let keys = view
            .rows
            .iter()
            .map(|row| row.key.as_str())
            .collect::<Vec<_>>();
        assert!(keys.contains(&"provider"));
        assert!(keys.contains(&"model"));
        assert!(keys.contains(&"reasoning_effort"));
        assert!(keys.contains(&"base_url"));
        assert!(keys.contains(&"external_credentials.openai-codex"));
        assert!(keys.contains(&"external_credentials.xai"));
        assert!(keys.contains(&"approval_mode"));
        assert!(keys.contains(&"permission_posture"));
        assert!(keys.contains(&"allow_shell"));
        assert!(keys.contains(&"theme"));
        assert!(keys.contains(&"locale"));
        assert!(keys.contains(&"background_color"));
        assert!(keys.contains(&"fancy_animations"));
        assert!(keys.contains(&"thinking_default_expanded"));
        assert!(keys.contains(&"synchronized_output"));
        assert!(keys.contains(&"auto_compact"));
        assert!(keys.contains(&"tool_collapse"));
        assert!(keys.contains(&"composer_border"));
        assert!(keys.contains(&"composer_multiline_mode"));
        assert!(keys.contains(&"cost_currency"));
        assert!(keys.contains(&"mcp_open"));
        assert!(keys.contains(&"mcp_reconnect"));
        assert!(keys.contains(&"mcp_diagnose"));
        assert!(keys.contains(&"plugins_open"));
        assert!(keys.contains(&"mcp_config_path"));
        assert!(keys.contains(&"fleet.exec.max_spawn_depth"));
        // Retired rows: the backends stay live (`default_model` routing,
        // the `vision_model` feature flag) or were derived receipts
        // (`fast_model`), but none keeps a table row.
        assert!(!keys.contains(&"features.vision_model"));
        assert!(!keys.contains(&"fast_model"));
        assert!(!keys.contains(&"default_model"));
        assert!(keys.contains(&"goal_command"));
        assert!(keys.contains(&"workflow"));
        assert!(!keys.contains(&"features.subagents"));
        assert!(!keys.contains(&"features.web_search"));
        assert!(!keys.contains(&"features.apply_patch"));
        assert!(!keys.contains(&"features.mcp"));
        assert!(!keys.contains(&"features.exec_policy"));
        assert!(!keys.contains(&"whaleflow"));
        // Diagnostic-only rows, managed permission rows, and live route
        // receipts are not editable; everything else outside the
        // read-only sections should be.
        const DIAGNOSTIC_ONLY: &[&str] = &[
            "context_window",
            "effective_context_window",
            "external_credentials.openai-codex",
            "external_credentials.xai",
            "base_url",
            "provider_url",
            // Sub-agent depth stays a read-only config.toml receipt in its
            // new Model home; it is edited in the fleet config, not here.
            "fleet.exec.max_spawn_depth",
        ];
        assert!(
            view.rows
                .iter()
                .filter(|row| {
                    !matches!(
                        row.section(),
                        super::ConfigSection::Experimental
                            | super::ConfigSection::Fleet
                            | super::ConfigSection::Workflow
                            | super::ConfigSection::Session
                            | super::ConfigSection::Legacy
                    ) && !DIAGNOSTIC_ONLY.contains(&row.key.as_str())
                        && !row.key.starts_with("managed_")
                })
                .all(|row| row.editable)
        );
        assert!(
            view.rows
                .iter()
                .filter(|row| {
                    matches!(
                        row.section(),
                        super::ConfigSection::Experimental
                            | super::ConfigSection::Fleet
                            | super::ConfigSection::Workflow
                            | super::ConfigSection::Session
                            | super::ConfigSection::Legacy
                    )
                })
                .all(|row| !row.editable || row.key.starts_with("notifications."))
        );
        // Route endpoint rows are provider-specific: DeepSeek routes expose
        // `base_url`, every other provider exposes `provider_url`. Whichever
        // exists must be a read-only route receipt.
        const ROUTE_RECEIPT_KEYS: &[&str] = &["base_url", "provider_url"];
        for key in DIAGNOSTIC_ONLY
            .iter()
            .filter(|key| !ROUTE_RECEIPT_KEYS.contains(key))
        {
            assert!(
                view.rows.iter().any(|row| row.key == *key && !row.editable),
                "{key} must remain diagnostic-only"
            );
        }
        let receipt_rows: Vec<_> = view
            .rows
            .iter()
            .filter(|row| ROUTE_RECEIPT_KEYS.contains(&row.key.as_str()))
            .collect();
        assert_eq!(
            receipt_rows.len(),
            1,
            "exactly one endpoint receipt row must exist for the active route"
        );
        assert!(
            !receipt_rows[0].editable,
            "endpoint receipt rows must be read-only"
        );
    }

    #[test]
    fn config_view_surfaces_structural_external_consent_without_io() {
        let _env = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir().expect("config view fixture");
        let config_path = temp.path().join("config.toml");
        let auth_path = temp.path().join("codex-auth.json");
        fs::write(&auth_path, "external-secret-must-not-be-read").expect("auth trap");
        fs::write(
            &config_path,
            format!(
                r#"provider = "openai-codex"
[providers.openai_codex]
auth_mode = "oauth"
[providers.openai_codex.external_credentials]
access = "read_only"
provider = "openai-codex"
source = "codex_cli"
path = {:?}
consent_version = 1
"#,
                auth_path.display().to_string()
            ),
        )
        .expect("config fixture");
        let ambient_path = temp.path().join("new-ambient-codex-auth.json");
        let _path = crate::test_support::EnvVarGuard::set("OPENAI_CODEX_AUTH_FILE", &ambient_path);
        let mut app = create_test_app();
        app.config_path = Some(config_path);
        crate::external_credentials::reset_side_effect_trap();
        let view = ConfigView::new_for_app(&app);
        let row = view
            .rows
            .iter()
            .find(|row| row.key == "external_credentials.openai-codex")
            .expect("structural consent row");
        assert!(row.value.contains("access=read_only"), "{}", row.value);
        assert!(row.value.contains("source=codex_cli"), "{}", row.value);
        assert!(row.value.contains("version=1"), "{}", row.value);
        assert!(row.value.contains("active"), "{}", row.value);
        assert!(row.value.contains("remains pinned"), "{}", row.value);
        assert!(
            row.value
                .contains(&codewhale_config::quote_os_path(&auth_path)),
            "{}",
            row.value
        );
        assert!(
            !row.value.contains(&ambient_path.display().to_string()),
            "{}",
            row.value
        );
        assert!(
            row.value
                .contains("external-revoke --provider openai-codex")
        );
        assert_eq!(
            crate::external_credentials::complete_side_effect_trap_counts(),
            (0, 0, 0, 0, 0)
        );
    }

    #[test]
    fn config_view_permission_row_tracks_the_controlling_saved_source() {
        let explicit_dir = TempDir::new().expect("explicit config tempdir");
        let explicit_path = explicit_dir.path().join("config.toml");
        fs::write(&explicit_path, "approval_policy = \"auto\"\n").expect("explicit config");
        let mut app = create_test_app();
        app.config_path = Some(explicit_path);

        let mut explicit = ConfigView::new_for_app(&app);
        let row = explicit
            .rows
            .iter()
            .find(|row| row.key == "approval_policy")
            .expect("explicit approval policy row");
        assert_eq!(row.value, "auto");
        assert!(row.editable);
        assert_eq!(row.scope, ConfigScope::Saved);
        assert!(
            explicit
                .rows
                .iter()
                .all(|row| row.key != "permission_posture")
        );
        explicit.focus_key("approval_policy");
        explicit.start_edit();
        let choices = explicit
            .editing
            .as_ref()
            .and_then(|edit| edit.choices.as_ref())
            .expect("approval posture choices");
        assert_eq!(
            choices,
            &vec![
                "use-tui-default".to_string(),
                "ask".to_string(),
                "auto-review".to_string(),
                "full-access".to_string(),
            ]
        );
        let area = Rect::new(0, 0, 110, 30);
        let mut buf = Buffer::empty(area);
        explicit.render(area, &mut buf);
        let dump = buffer_text(&buf, area);
        assert!(
            dump.contains("4. Full Access"),
            "root permission chooser must expose the product posture:\n{dump}"
        );
        assert!(
            !dump.contains("4. Never"),
            "root permission chooser leaked the raw fail-closed policy token:\n{dump}"
        );
        let use_tui_default = explicit
            .editing
            .as_ref()
            .and_then(|edit| edit.choices.as_ref())
            .and_then(|choices| {
                choices
                    .iter()
                    .position(|choice| choice == "use-tui-default")
            })
            .expect("TUI default choice");
        explicit
            .editing
            .as_mut()
            .expect("choice editor")
            .selected_choice = use_tui_default;
        match explicit.handle_choice_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            ViewAction::Emit(ViewEvent::ConfigUpdated {
                key,
                value,
                persist,
            }) => {
                assert_eq!(key, "approval_policy");
                assert_eq!(value, "use-tui-default");
                assert!(persist);
            }
            other => panic!("expected saved ConfigUpdated event, got {other:?}"),
        }

        let managed_dir = TempDir::new().expect("managed config tempdir");
        let requirements_path = managed_dir.path().join("requirements.toml");
        fs::write(
            &requirements_path,
            "allowed_approval_policies = [\"never\"]\n",
        )
        .expect("requirements config");
        let config_path = managed_dir.path().join("config.toml");
        let requirements_value =
            toml::Value::String(requirements_path.to_string_lossy().into_owned()).to_string();
        fs::write(
            &config_path,
            format!("approval_policy = \"never\"\nrequirements_path = {requirements_value}\n"),
        )
        .expect("managed config");
        app.config_path = Some(config_path);

        let managed = ConfigView::new_for_app(&app);
        let row = managed
            .rows
            .iter()
            .find(|row| row.key == "managed_approval_policy")
            .expect("managed approval policy row");
        assert!(!row.editable);
        assert_eq!(row.scope, ConfigScope::Saved);
        assert!(
            managed
                .rows
                .iter()
                .all(|row| row.key != "permission_posture" && row.key != "approval_policy")
        );
    }

    #[test]
    fn config_view_provider_uses_full_picker_and_preserves_custom_provider_id() {
        let dir = TempDir::new().expect("custom provider tempdir");
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
provider = "acme_ai"

[providers.acme_ai]
kind = "openai-compatible"
base_url = "https://api.example.invalid/v1"
model = "acme-model"
api_key_env = "ACME_API_KEY"
"#,
        )
        .expect("custom provider config");
        let mut app = create_test_app();
        app.config_path = Some(config_path);
        app.set_provider_identity(crate::config::ProviderKind::Custom, "acme_ai");
        let mut view = ConfigView::new_for_app(&app);
        view.focus_key("provider");

        let row = &view.rows[view.selected];
        assert_eq!(row.value, "acme_ai");
        assert_eq!(
            row.scope,
            ConfigScope::Session,
            "the provider row shows the live route identity, not saved config"
        );
        assert!(
            config_choice_values("provider").is_none(),
            "provider must not be truncated to the generic enum chooser"
        );

        match view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            ViewAction::Emit(ViewEvent::CommandPaletteSelected {
                action: CommandPaletteAction::ExecuteCommand { command },
            }) => assert_eq!(command, "/provider"),
            other => panic!("expected full provider picker command, got {other:?}"),
        }
        assert!(view.editing.is_none());
    }

    #[test]
    fn config_view_active_model_uses_picker_and_retired_rows_are_gone() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        view.focus_key("model");

        match view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            ViewAction::Emit(ViewEvent::CommandPaletteSelected {
                action: CommandPaletteAction::ExecuteCommand { command },
            }) => assert_eq!(command, "/model"),
            other => panic!("expected full model picker, got {other:?}"),
        }
        assert!(view.editing.is_none());

        // The derived fast-sibling receipt and the legacy DeepSeek fallback
        // have no rows: sibling choice happens in the /model picker and the
        // fallback stays a `/set`-only compatibility key.
        for key in ["fast_model", "default_model"] {
            assert!(
                view.rows.iter().all(|row| row.key != key),
                "{key} must have no settings row"
            );
        }
    }

    #[test]
    fn config_view_zai_model_row_has_no_derived_rows() {
        let _guard = ConfigSettingsEnvGuard::new("");
        let mut app = create_test_app();
        app.api_provider = crate::config::ProviderKind::Zai;
        app.model = crate::config::ZAI_GLM_5_2_MODEL.to_string();

        let view = ConfigView::new_for_app(&app);
        let active = view
            .rows
            .iter()
            .find(|row| row.key == "model")
            .expect("active model row");

        assert_eq!(active.value, "Zhipu AI / Z.ai · GLM-5.2");
        // Derived receipts retired: the fast sibling is named in the /model
        // picker, and the DeepSeek-only fallback never appears as a row.
        for key in ["fast_model", "default_model"] {
            assert!(
                view.rows.iter().all(|row| row.key != key),
                "{key} row must be gone for zai"
            );
        }
    }

    #[test]
    fn config_view_live_route_never_shows_stale_saved_provider() {
        // The reported defect: the saved config still said `provider =
        // "deepseek"` while the session was actually routed to Z.ai / GLM-5.3,
        // and Settings presented the stale saved value as the "Active
        // provider". Route rows must show the live route identity; saved
        // config is a startup/default fact, never the active receipt.
        let temp_root = std::env::temp_dir().join(format!(
            "codewhale-stale-saved-provider-view-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&temp_root).unwrap();
        let config_path = temp_root.join("config.toml");
        fs::write(
            &config_path,
            "provider = \"deepseek\"\nbase_url = \"https://api.deepseek.com/v1\"\n",
        )
        .unwrap();

        let mut app = create_test_app();
        app.config_path = Some(config_path.clone());
        // Live session route, exactly as a /provider switch would leave it.
        app.api_provider = crate::config::ProviderKind::Zai;
        app.model = "GLM-5.3".to_string();
        app.active_route_base_url = crate::config::DEFAULT_ZAI_BASE_URL.to_string();

        let view = ConfigView::new_for_app(&app);

        let provider_row = view
            .rows
            .iter()
            .find(|row| row.key == "provider")
            .expect("provider row");
        assert!(
            provider_row.value.contains("Z.ai"),
            "provider row must show the live route identity: {}",
            provider_row.value
        );
        assert!(
            !provider_row.value.to_lowercase().contains("deepseek"),
            "stale saved provider must not appear as the active route: {}",
            provider_row.value
        );
        assert_eq!(provider_row.scope, ConfigScope::Session);

        let model_row = view
            .rows
            .iter()
            .find(|row| row.key == "model")
            .expect("model row");
        assert_eq!(model_row.value, "Zhipu AI / Z.ai · GLM-5.3");

        let url_row = view
            .rows
            .iter()
            .find(|row| row.key == "provider_url")
            .expect("endpoint row for the live Z.ai route");
        assert_eq!(url_row.value, crate::config::DEFAULT_ZAI_BASE_URL);
        assert!(!view.rows.iter().any(|row| row.key == "base_url"));
    }

    #[test]
    fn config_view_shows_no_deepseek_fallback_row_on_any_provider() {
        let _guard = ConfigSettingsEnvGuard::new("");
        let mut app = create_test_app();
        for provider in [
            crate::config::ProviderKind::Zai,
            crate::config::ProviderKind::Xai,
            crate::config::ProviderKind::Openrouter,
            crate::config::ProviderKind::Ollama,
            crate::config::ProviderKind::Deepseek,
        ] {
            app.api_provider = provider;
            let view = ConfigView::new_for_app(&app);
            assert!(
                view.rows.iter().all(|row| row.key != "default_model"),
                "default_model must have no row for {:?}",
                provider
            );
        }
    }

    #[test]
    fn config_view_saved_deepseek_fallback_is_a_read_only_migration_input() {
        // Old fallback values still parse, but new model choices belong to
        // the canonical config selection writer.
        let _guard = ConfigSettingsEnvGuard::new("default_model = \"deepseek-v4-pro\"\n");
        let mut app = create_test_app();
        app.api_provider = crate::config::ProviderKind::Zai;

        let view = ConfigView::new_for_app(&app);
        assert!(
            view.rows.iter().all(|row| row.key != "default_model"),
            "saved legacy fallback must not surface a row"
        );
        let mut settings = Settings::default();
        let error = settings
            .set("default_model", "deepseek-v4-pro")
            .expect_err("legacy model settings must not become another writer");
        assert!(error.to_string().contains("config.toml"));
        assert!(settings.default_model.is_none());
    }

    /// Retired rows leave no section behind: sub-agent depth moved into the
    /// Model group, the legacy fallback and the vision flag lost their rows,
    /// and `/goal` + Workflow keep their own sections. Persisted keys are
    /// unchanged.
    #[test]
    fn config_view_settings_rows_land_in_truthful_sections() {
        let _guard = ConfigSettingsEnvGuard::new("default_model = \"deepseek-v4-pro\"\n");
        let mut app = create_test_app();
        app.api_provider = crate::config::ProviderKind::Zai;
        let view = ConfigView::new_for_app(&app);

        let section_of = |key: &str| {
            view.rows
                .iter()
                .find(|row| row.key == key)
                .unwrap_or_else(|| panic!("{key} row"))
                .section()
        };
        assert_eq!(
            section_of("fleet.exec.max_spawn_depth"),
            super::ConfigSection::Model
        );
        assert_eq!(section_of("goal_command"), super::ConfigSection::Session);
        assert_eq!(section_of("workflow"), super::ConfigSection::Workflow);

        // The retired rows are gone on every provider, even with a saved
        // fallback value still on disk.
        for key in ["default_model", "fast_model", "features.vision_model"] {
            assert!(
                view.rows.iter().all(|row| row.key != key),
                "{key} must have no row"
            );
        }

        // …and their sections retire with them: no Legacy, Experimental, or
        // Fleet headings may survive with zero rows behind them.
        let retired_sections = [
            super::ConfigSection::Legacy,
            super::ConfigSection::Experimental,
            super::ConfigSection::Fleet,
        ];
        for row in &view.rows {
            assert!(
                !retired_sections.contains(&row.section()),
                "{} still files under a retired section",
                row.key
            );
        }

        // Relabelling is presentation only: the persisted key and value
        // round-trip unchanged, so existing config files keep loading
        // identically.
        let depth = view
            .rows
            .iter()
            .find(|row| row.key == "fleet.exec.max_spawn_depth")
            .expect("sub-agent depth row");
        assert_eq!(depth.scope, ConfigScope::Saved);
        assert!(!depth.editable);
        assert_eq!(config_label_for_key(&depth.key), "agent depth");

        // Workflow keeps its own name and its `/workflow` wording.
        let workflow = view
            .rows
            .iter()
            .find(|row| row.section() == super::ConfigSection::Workflow)
            .expect("workflow row");
        assert_eq!(workflow.key.as_str(), "workflow");
        assert!(workflow.value.starts_with("/workflow "), "{workflow:?}");
        assert_eq!(config_label_for_key("workflow"), "Workflow");
    }

    #[test]
    fn config_view_experimental_features_leave_no_rows() {
        // The vision row retired: even a configured beta flag surfaces no
        // table row. The flag itself stays live in the feature backend,
        // diagnosed where vision runs instead of in Advanced.
        let temp_root = std::env::temp_dir().join(format!(
            "codewhale-experimental-config-view-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&temp_root).unwrap();
        let config_path = temp_root.join("config.toml");
        fs::write(
            &config_path,
            r#"
[features]
web_search = false
vision_model = true
"#,
        )
        .unwrap();

        let mut app = create_test_app();
        app.config_path = Some(config_path);
        let view = ConfigView::new_for_app(&app);

        for key in [
            "features.web_search",
            "features.vision_model",
            "features.subagents",
        ] {
            assert!(
                view.rows.iter().all(|row| row.key != key),
                "{key} must have no settings row"
            );
        }
    }

    #[test]
    fn config_view_shows_fleet_max_spawn_depth_from_config() {
        let temp_root = std::env::temp_dir().join(format!(
            "codewhale-fleet-config-view-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&temp_root).unwrap();
        let config_path = temp_root.join("config.toml");
        fs::write(
            &config_path,
            r#"
[fleet.exec]
max_spawn_depth = 2
"#,
        )
        .unwrap();

        let mut app = create_test_app();
        app.config_path = Some(config_path);
        let view = ConfigView::new_for_app(&app);

        let row = view
            .rows
            .iter()
            .find(|row| row.key == "fleet.exec.max_spawn_depth")
            .expect("fleet spawn depth row");
        assert_eq!(row.value, "2");
        assert!(!row.editable);
    }

    #[test]
    fn config_view_retired_experimental_section_stays_gone() {
        let mut view = create_config_view(Locale::En);

        // The Experimental group retired with the vision row: the flag stays
        // live in the backend, but no section or row answers to it anymore.
        view.update_filter(|filter| filter.push_str("experimental"));
        assert!(visible_section_labels(&view).is_empty());
        assert!(visible_row_keys(&view).is_empty());

        view.clear_filter();
        type_filter(&mut view, "feature vision");
        assert!(visible_section_labels(&view).is_empty());
        assert!(visible_row_keys(&view).is_empty());

        view.clear_filter();
        type_filter(&mut view, "goal");
        assert_eq!(visible_section_labels(&view), vec!["Session"]);
        assert_eq!(visible_row_keys(&view), vec!["goal_command"]);

        // The `workflow` row keeps its key and its name; #4751 only moved it
        // out of Fleet into its own Workflow section.
        view.clear_filter();
        type_filter(&mut view, "workflow");
        assert_eq!(visible_section_labels(&view), vec!["Workflow"]);
        let workflow_keys = visible_row_keys(&view);
        assert_eq!(workflow_keys.first(), Some(&"workflow"));
        assert_eq!(
            workflow_keys.len(),
            1 + codewhale_config::notifications::NotificationSetting::ALL.len()
        );
        assert!(workflow_keys[1..].iter().all(|key| {
            codewhale_config::notifications::NotificationSetting::parse(key).is_some()
        }));

        view.clear_filter();
        type_filter(&mut view, "whaleflow");
        assert!(visible_row_keys(&view).is_empty());
    }

    #[test]
    fn config_view_base_url_reflects_active_route_receipt() {
        let mut app = create_test_app();
        app.active_route_base_url = "https://ui-config-view.local/v1".to_string();
        let view = ConfigView::new_for_app(&app);

        let row = view
            .rows
            .iter()
            .find(|row| row.key == "base_url")
            .expect("base_url row missing");
        assert_eq!(
            config_label_for_key(&row.key),
            "Provider API URL (DeepSeek route)"
        );
        // The endpoint row is a read-only receipt for the live route; it must
        // not re-read config files, which may describe a different saved
        // route than the one the session is actually using.
        assert_eq!(row.value, "https://ui-config-view.local/v1");
        assert!(!row.editable);
        assert_eq!(row.scope, ConfigScope::Session);
    }

    #[test]
    fn config_view_uses_provider_url_for_non_deepseek_provider() {
        let temp_root = std::env::temp_dir().join(format!(
            "codewhale-provider-url-view-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&temp_root).unwrap();
        let config_path = temp_root.join("config.toml");
        fs::write(
            &config_path,
            r#"
provider = "xiaomi-mimo"

[providers.xiaomi_mimo]
api_key = "tp-test-token-plan-key"
base_url = "https://api.xiaomimimo.com/v1"
"#,
        )
        .unwrap();

        let mut app = create_test_app();
        app.api_provider = crate::config::ProviderKind::XiaomiMimo;
        app.active_route_base_url = crate::config::DEFAULT_XIAOMI_MIMO_BASE_URL.to_string();
        app.ui_locale = Locale::Es419;
        app.config_path = Some(config_path.clone());
        let mut view = ConfigView::new_for_app(&app);

        let row = view
            .rows
            .iter()
            .find(|row| row.key == "provider_url")
            .expect("provider_url row missing");
        // The endpoint row reflects the live route identity (the default when
        // nothing overrides it), not a config-file re-read, and is a receipt.
        assert_eq!(row.value, crate::config::DEFAULT_XIAOMI_MIMO_BASE_URL);
        assert!(!row.editable);
        assert!(!view.rows.iter().any(|row| row.key == "base_url"));

        view.focus_key("provider_url");
        let hint = view
            .setting_detail_lines(&view.rows[view.selected], 200)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let es_hint = tr(Locale::Es419, MessageId::ConfigHintProviderUrl);
        assert!(hint.contains(es_hint.as_ref()), "{hint}");
        assert!(hint.contains("pago por uso"), "{hint}");
        assert!(
            !hint.contains(tr(Locale::En, MessageId::ConfigHintProviderUrl).as_ref()),
            "the Spanish settings view must not leak the English guidance: {hint}"
        );
    }

    #[test]
    fn config_view_cost_currency_shows_saved_and_effective_runtime_currency() {
        let _guard = ConfigSettingsEnvGuard::new("locale = \"zh-Hans\"\ncost_currency = \"usd\"\n");
        let app = create_test_app();
        assert_eq!(app.ui_locale, Locale::ZhHans);
        assert_eq!(app.cost_currency, crate::pricing::CostCurrency::Cny);

        let view = ConfigView::new_for_app(&app);
        let row = view
            .rows
            .iter()
            .find(|row| row.key == "cost_currency")
            .expect("cost_currency row");

        assert_eq!(row.value, "usd");
        assert_eq!(view.row_display_value(row), "usd (实际 cny)");
        assert_eq!(Settings::load().expect("settings").cost_currency, "usd");
    }

    #[test]
    fn config_view_cost_currency_aliases_matching_effective_currency_are_silent() {
        for alias in ["rmb", "yuan", "¥"] {
            let (saved_value, display_value, effective_currency, locale) =
                cost_currency_row_for_settings(&format!(
                    "locale = \"zh-Hans\"\ncost_currency = \"{alias}\"\n"
                ));

            assert_eq!(locale, Locale::ZhHans);
            assert_eq!(effective_currency, crate::pricing::CostCurrency::Cny);
            assert_eq!(saved_value, alias);
            assert_eq!(display_value, alias);
        }
    }

    #[test]
    fn config_view_cost_currency_matching_cny_setting_is_silent() {
        let (saved_value, display_value, effective_currency, locale) =
            cost_currency_row_for_settings("locale = \"zh-Hans\"\ncost_currency = \"cny\"\n");

        assert_eq!(locale, Locale::ZhHans);
        assert_eq!(effective_currency, crate::pricing::CostCurrency::Cny);
        assert_eq!(saved_value, "cny");
        assert_eq!(display_value, "cny");
    }

    #[test]
    fn config_view_cost_currency_non_zh_hans_locale_uses_saved_currency() {
        let (saved_value, display_value, effective_currency, locale) =
            cost_currency_row_for_settings("locale = \"en\"\ncost_currency = \"cny\"\n");

        assert_eq!(locale, Locale::En);
        assert_eq!(effective_currency, crate::pricing::CostCurrency::Cny);
        assert_eq!(saved_value, "cny");
        assert_eq!(display_value, "cny");
    }

    /// The panel's contract, cell-exact: tabs across the top, the groups
    /// column beside the list at 120 columns and folded into the headings at
    /// 80, the selected setting's sentence in the band, and the live footer
    /// preview under it. A visual change that cannot show as a golden diff
    /// did not happen.
    #[test]
    fn config_panel_golden_at_eighty_and_one_twenty() {
        let _guard = ConfigSettingsEnvGuard::new("theme = \"terminal\"\n");
        let app = create_test_app();
        let view = ConfigView::new_for_app(&app);
        for (width, height) in [(40u16, 12u16), (80u16, 24u16), (120u16, 32u16)] {
            let rendered = crate::tui::golden_harness::render_golden_text(width, height, |buf| {
                view.render(Rect::new(0, 0, width, height), buf);
            });
            crate::tui::golden_harness::assert_matches_golden(
                &format!("config_panel_{width}x{height}"),
                &rendered,
            );
        }
    }

    /// ASCII-safe terminals: every settings row kind (toggle, choice, number,
    /// text, action, read-only) and the panel chrome around it must narrow
    /// to ASCII through the backend's cell adapter. `✎` used to pass through
    /// untouched on every editable number/text row.
    #[test]
    fn config_panel_every_row_kind_renders_ascii_through_the_adapter() {
        let _guard = ConfigSettingsEnvGuard::new("theme = \"terminal\"\n");
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        let mut kinds_seen = Vec::new();
        for category in ConfigCategory::ALL {
            view.category = category;
            view.select_first_visible_row();
            let items = view.visible_items();
            for item in &items {
                if let ConfigListItem::Row(idx) = item {
                    let kind = view.editor_kind(&view.rows[*idx]);
                    if !kinds_seen.contains(&kind) {
                        kinds_seen.push(kind);
                    }
                }
            }
            // Tall enough that the whole category paints without scrolling.
            let height = u16::try_from(items.len() + 16).unwrap_or(u16::MAX);
            let area = Rect::new(0, 0, 120, height);
            let mut buf = Buffer::empty(area);
            view.render(area, &mut buf);
            for y in area.top()..area.bottom() {
                for x in area.left()..area.right() {
                    let mut cell = buf[(x, y)].clone();
                    crate::tui::color_compat::adapt_cell_symbol_for_ascii(&mut cell);
                    assert!(
                        cell.symbol().is_ascii(),
                        "{category:?} ({x},{y}): {:?} has no ASCII fallback",
                        buf[(x, y)].symbol()
                    );
                }
            }
        }
        for kind in [
            SettingKind::Boolean,
            SettingKind::Choice,
            SettingKind::Integer,
            SettingKind::Text,
            SettingKind::Action,
            SettingKind::ReadOnly,
        ] {
            assert!(
                kinds_seen.contains(&kind),
                "no settings row of kind {kind:?} was rendered"
            );
        }
    }

    /// Slice C: cell-exact goldens for Edit Theme with the underwater
    /// default open — title, scope/current lanes, the 14 theme rows, and
    /// the Apply/Cancel controls. Empty settings mean the editor opens on
    /// the default theme, so these goldens pin the default end to end.
    /// Re-bless with `CODEWHALE_BLESS_GOLDENS=1`.
    #[test]
    fn edit_theme_matches_goldens_at_blocker_sizes() {
        let _guard = ConfigSettingsEnvGuard::new("");
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        view.focus_key("theme");
        view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            view.editing
                .as_ref()
                .is_some_and(|edit| edit.key == "theme"),
            "Enter must open the theme editor"
        );
        for (width, height) in [(80u16, 24u16), (120u16, 32u16)] {
            let rendered = crate::tui::golden_harness::render_golden_text(width, height, |buf| {
                view.render(Rect::new(0, 0, width, height), buf);
            });
            crate::tui::golden_harness::assert_matches_golden(
                &format!("edit_theme_{width}x{height}"),
                &trim_golden_rows(&rendered),
            );
        }
    }

    /// Goldens are stored without cell padding: every row is right-trimmed
    /// and trailing empty rows are dropped, so `git diff --check` stays
    /// clean.
    fn trim_golden_rows(text: &str) -> String {
        let mut rows: Vec<&str> = text.lines().map(str::trim_end).collect();
        while rows.last().is_some_and(|row| row.is_empty()) {
            rows.pop();
        }
        let mut out = rows.join("\n");
        out.push('\n');
        out
    }

    #[test]
    fn notification_rows_keep_saved_and_live_values_distinct_after_reopening() {
        let _guard = ConfigSettingsEnvGuard::new("");
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        std::fs::write(&path, "[notifications]\nquiet = false\nsound = \"off\"\n").unwrap();
        let mut app = create_test_app();
        app.config_path = Some(path.clone());
        let mut config = Config::load(Some(path), None).unwrap();
        crate::tui::ui::apply_notification_update(
            &mut app,
            &mut config,
            crate::config::NotificationConfigUpdate::Quiet(true),
        )
        .unwrap();
        crate::tui::ui::apply_notification_update(
            &mut app,
            &mut config,
            crate::config::NotificationConfigUpdate::Sound(Some(
                crate::config::CompletionSound::Whale,
            )),
        )
        .unwrap();
        for _ in 0..2 {
            let mut view = ConfigView::new_for_app(&app);
            let row = view
                .rows
                .iter()
                .find(|row| row.key == "notifications.quiet")
                .unwrap();
            assert_eq!(row.value, "false");
            assert_eq!(row.edit_value(), "true");
            let fact = view.setting_fact(row).unwrap();
            assert_ne!(fact.saved, fact.current);
            view.focus_key("notifications.quiet");
            assert!(
                matches!(view.toggle_selected_boolean(), Some(ViewAction::Emit(ViewEvent::ConfigUpdated { value, .. })) if value == "false")
            );
            view.focus_key("notifications.sound");
            view.start_edit();
            let edit = view.editing.as_ref().unwrap();
            assert_eq!(
                edit.choices.as_ref().unwrap()[edit.selected_choice],
                "whale"
            );
        }
        app.refresh_notification_settings(&Config::default());
    }

    /// The settings screen is a projection of the schema: its rail tabs, the
    /// group headings inside them, and the row order are the schema's
    /// declaration order, not a second table's. This is the one table test
    /// that keeps the projection honest.
    #[test]
    fn settings_tree_equals_the_schema_projection() {
        let mut view = create_config_view(Locale::En);
        let built: std::collections::HashSet<&str> =
            view.rows.iter().map(|row| row.key.as_str()).collect();
        let schema_keys: std::collections::HashSet<&str> =
            codewhale_config::schema_rows().map(|def| def.key).collect();

        // Every schema row must appear in the view; `ui: None` settings stay
        // declared but off-screen, and anything the schema says is visible must
        // have a row here. A few rows are mutually exclusive: exactly one member
        // of each set is shown, chosen by which store controls the fact.
        let conditional_pairs: &[&[&str]] = &[
            &[
                "permission_posture",
                "approval_policy",
                "managed_approval_policy",
            ],
            &["allow_shell", "managed_allow_shell"],
            &["base_url", "provider_url"],
        ];
        for def in codewhale_config::schema_rows() {
            let present_in_pair = conditional_pairs
                .iter()
                .any(|pair| pair.contains(&def.key) && pair.iter().any(|key| built.contains(key)));
            // Experimental feature rows exist only when the flag is configured
            // or non-default (`experimental_feature_rows`).
            let configured_only = def.key.starts_with("features.");
            assert!(
                def.ui.is_none() || built.contains(def.key) || present_in_pair || configured_only,
                "{} is declared in schema_rows but has no ConfigRow",
                def.key
            );
        }

        // No row should exist that is not declared in the schema.
        for key in &built {
            assert!(
                schema_keys.contains(key),
                "{key} has a ConfigRow but is not declared in schema_rows"
            );
        }

        assert_eq!(
            codewhale_config::schema_tabs(),
            ConfigCategory::ALL
                .iter()
                .map(|category| category.id())
                .collect::<Vec<_>>(),
            "rail order and schema tab order have drifted apart"
        );

        let mut expected: Vec<String> = Vec::new();
        for tab in codewhale_config::schema_tabs() {
            let mut group: Option<&str> = None;
            for def in codewhale_config::schema_rows() {
                let ui = def.ui.as_ref().expect("schema_rows filters on ui");
                if ui.tab != tab || !built.contains(def.key) {
                    continue;
                }
                if group != Some(ui.group) {
                    group = Some(ui.group);
                    expected.push(format!("{tab}/{}", ui.group));
                }
                expected.push(format!("{tab}/{}/{}", ui.group, def.key));
            }
        }

        let mut actual: Vec<String> = Vec::new();
        for category in ConfigCategory::ALL {
            view.category = category;
            for item in view.visible_items() {
                match item {
                    ConfigListItem::Section(section) => {
                        actual.push(format!("{}/{}", category.id(), section.id()));
                    }
                    ConfigListItem::Row(idx) => {
                        let row = &view.rows[idx];
                        actual.push(format!(
                            "{}/{}/{}",
                            category.id(),
                            row.section().id(),
                            row.key
                        ));
                    }
                }
            }
        }

        assert_eq!(actual, expected);
    }

    /// Every persisted row must land in a store. Typed action rows only open
    /// another surface; `settings.toml` rows round-trip through `Settings`.
    /// The remaining receipts and `config.toml` keys are spelled out so a new
    /// editable row cannot quietly discard the user's edit.
    #[test]
    fn every_settings_row_reaches_a_store() {
        let _guard = crate::test_support::lock_test_env();
        // Non-action rows outside `settings.toml`: report a fact or persist
        // to config.toml through `set_config_value`.
        const NOT_SETTINGS_TOML: &[&str] = &[
            "fleet.exec.max_spawn_depth",
            "goal_command",
            "workflow",
            "mcp_open",
            "mcp_reconnect",
            "mcp_diagnose",
            "plugins_open",
            "sandbox_details",
            "mcp_config_path",
            "approval_mode",
            "permission_posture",
            "approval_policy",
            "managed_approval_policy",
            "allow_shell",
            "managed_allow_shell",
            "telemetry",
            "context_window",
            "effective_context_window",
            "fast_model",
            "features.vision_model",
            "features.subagents",
            "features.web_search",
            "features.apply_patch",
            "features.mcp",
            "features.exec_policy",
            "base_url",
            "provider_url",
            "effective_context_window",
            "external_credentials.openai-codex",
            "external_credentials.xai",
        ];

        for def in codewhale_config::schema_rows() {
            if def.ui.is_some_and(|ui| {
                ui.row == codewhale_config::settings_schema::SettingRowKind::Action
            }) {
                continue;
            }
            if let Some(setting) =
                codewhale_config::notifications::NotificationSetting::parse(def.key)
            {
                let samples = match setting {
                    codewhale_config::notifications::NotificationSetting::SoundFile => {
                        vec!["call with spaces.wav".to_string()]
                    }
                    codewhale_config::notifications::NotificationSetting::EventSoundEvents => {
                        vec![r#"["input-needed", "model-notify"]"#.to_string()]
                    }
                    _ => def
                        .values()
                        .map(|values| values.into_iter().map(str::to_string).collect())
                        .unwrap_or_else(|| vec!["37".to_string()]),
                };
                let temp = tempfile::tempdir().unwrap();
                let path = temp.path().join("config.toml");
                for sample in samples {
                    let edit =
                        crate::config::NotificationConfigUpdate::parse(setting, &sample).unwrap();
                    edit.persist(&path).unwrap();
                    let loaded = Config::load(Some(path.clone()), None)
                        .unwrap()
                        .notifications_config();
                    assert_eq!(
                        loaded.display(setting),
                        edit.display(),
                        "{} must reach the TUI config store",
                        def.key
                    );
                }
                continue;
            }
            // At least one value per row that is not the default, so a row
            // whose store silently drops writes cannot pass by looking like
            // an untouched `Settings`: bools and enums try every value, an
            // int tries the default plus one, free text tries a sentinel.
            // `config_choice_values` also covers the two registry-backed
            // strings (theme, locale), whose value set is the shipped list.
            let samples: Vec<String> = match config_choice_values(def.key) {
                Some(values) => values,
                // Validated free text: a value the store's own parser accepts.
                None if def.key == "background_color" => vec!["#1a1b26".to_string()],
                None if def.key == "default_model" => vec!["deepseek-v4-pro".to_string()],
                None if def.is_int() => {
                    let default: i64 = def.default.parse().unwrap_or(0);
                    vec![(default + 1).to_string()]
                }
                None if def.is_float() => {
                    let default: f64 = def.default.parse().unwrap_or(50.0);
                    vec![(default + 0.5).to_string()]
                }
                None => vec!["roundtrip-probe".to_string()],
            };
            assert!(
                !samples.is_empty(),
                "{} yields no sample to round-trip",
                def.key
            );
            let mut settings = Settings::default();
            let accepted = samples
                .first()
                .is_some_and(|sample| settings.set(def.key, sample).is_ok());
            if !accepted {
                assert!(
                    NOT_SETTINGS_TOML.contains(&def.key),
                    "{} shows a row that settings.toml will not take",
                    def.key
                );
                continue;
            }
            for sample in &samples {
                let mut settings = Settings::default();
                settings
                    .set(def.key, sample)
                    .unwrap_or_else(|error| panic!("{} rejects {sample}: {error}", def.key));
                let written = toml::to_string(&settings)
                    .unwrap_or_else(|error| panic!("{} will not serialize: {error}", def.key));
                let reloaded: Settings = toml::from_str(&written)
                    .unwrap_or_else(|error| panic!("{} will not reload: {error}", def.key));
                assert_eq!(
                    toml::to_string(&reloaded).expect("reloaded settings serialize"),
                    written,
                    "{} does not survive a settings.toml round trip at {sample}",
                    def.key
                );
            }
        }
    }

    /// Every field `Settings` persists to settings.toml is declared in
    /// SETTINGS_SCHEMA — a row for editable values, a hidden def for picker
    /// memory and one-way flags. A persisted field without a declaration has
    /// no kind, no provenance layer, and no resolver home.
    #[test]
    fn every_persisted_settings_field_is_declared_in_the_schema() {
        use crate::settings::PinnedModel;

        // Options serialize as absent when None; force them present so the
        // table below names every key settings.toml can hold.
        let settings = Settings {
            background_color: Some("#1a1b26".to_string()),
            default_provider: Some("deepseek".to_string()),
            default_model: Some("deepseek-v4-pro".to_string()),
            reasoning_effort: Some("medium".to_string()),
            permission_posture: Some("ask".to_string()),
            sandbox_mode: Some("read-only".to_string()),
            provider_models: Some(std::collections::HashMap::from([(
                "deepseek".to_string(),
                "deepseek-v4-pro".to_string(),
            )])),
            enabled_models: Some(std::collections::HashMap::from([(
                "deepseek".to_string(),
                vec!["deepseek-v4-pro".to_string()],
            )])),
            pinned_models: vec![PinnedModel {
                provider: "deepseek".to_string(),
                model: "deepseek-v4-pro".to_string(),
                label: None,
            }],
            behavioral_tip_impressions: std::collections::BTreeMap::from([(
                "probe".to_string(),
                1u8,
            )]),
            footer_hint_uses: std::collections::BTreeMap::from([("probe".to_string(), 1u8)]),
            ..Settings::default()
        };
        let table = toml::Value::try_from(&settings)
            .expect("settings serialize")
            .as_table()
            .expect("settings are a table")
            .clone();
        // The probe is only trustworthy if it actually names the keys whose
        // only declaration is hidden; a future `skip_serializing` would
        // silently drop a key from this table instead of failing below.
        for key in [
            "tool_collapse_mode",
            "max_input_history",
            "default_provider",
            "sandbox_mode",
            "provider_models",
            "enabled_models",
            "pinned_models",
            "feature_intro_shown",
            "yolo_deprecation_shown",
            "work_surface_bottom_migrated",
            "behavioral_tip_impressions",
            "footer_hint_uses",
        ] {
            assert!(
                table.contains_key(key),
                "probe settings undercovers settings.toml: `{key}` did not serialize"
            );
        }
        for key in table.keys() {
            assert!(
                codewhale_config::setting(key).is_some(),
                "settings.toml persists `{key}` with no SETTINGS_SCHEMA declaration"
            );
        }
    }

    /// Every message key declared by the schema must resolve to a localized
    /// string in every shipped locale. `tr_key` returns the key itself when a
    /// pack is missing the entry, so this fails fast on a stale binding.
    #[test]
    fn pet_mode_setting_shows_saved_and_live_choices_and_emits_the_existing_commit_event() {
        let _guard = ConfigSettingsEnvGuard::new("pet_mode = false\n");
        let mut app = create_test_app();
        app.pet_watch.enabled = true;
        let mut view = ConfigView::new_for_app(&app);
        let row = view
            .rows
            .iter()
            .find(|row| row.key == "pet_mode")
            .expect("pet view setting");
        assert_eq!(row.value, "false");
        assert_eq!(row.facts.effective.as_deref(), Some("true"));
        assert!(row.editable);
        assert_eq!(row.ui().unwrap().label, "ConfigLabelPetMode");
        view.focus_key("pet_mode");
        view.start_edit();
        let edit = view.editing.as_mut().expect("pet mode editor");
        edit.selected_choice = edit
            .choices
            .as_ref()
            .unwrap()
            .iter()
            .position(|value| value == "true")
            .unwrap();
        assert!(
            matches!(view.handle_choice_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), ViewAction::Emit(ViewEvent::ConfigUpdated { key, value, persist: true }) if key == "pet_mode" && value == "true")
        );
    }

    #[test]
    fn settings_schema_message_keys_are_localized() {
        let mut keys: Vec<&'static str> = Vec::new();
        for def in codewhale_config::SETTINGS_SCHEMA {
            if let Some(ui) = def.ui {
                if !ui.label.is_empty() {
                    keys.push(ui.label);
                }
                if !ui.description.is_empty() {
                    keys.push(ui.description);
                }
            }
            let options = match def.kind {
                codewhale_config::SettingKind::Bool(options) => options,
                codewhale_config::SettingKind::Enum(options) => options,
                codewhale_config::SettingKind::Int
                | codewhale_config::SettingKind::String
                | codewhale_config::SettingKind::Float => &[],
            };
            for option in options {
                if !option.label.is_empty() {
                    keys.push(option.label);
                }
                if !option.description.is_empty() {
                    keys.push(option.description);
                }
            }
        }

        for locale in Locale::shipped() {
            for key in &keys {
                let resolved = tr_key(*locale, key);
                assert_ne!(
                    resolved.as_ref(),
                    *key,
                    "{key} is not localized for {locale:?}"
                );
            }
        }
    }

    #[test]
    fn config_view_exposes_configured_and_effective_context_window() {
        let _env = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir().expect("config fixture");
        let config_path = temp.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
provider = "moonshot"
[providers.moonshot]
model = "kimi-k3"
context_window = 262144
"#,
        )
        .expect("config");
        let _config_path =
            crate::test_support::EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
        let config =
            Config::load(Some(config_path.clone()), None).expect("captured fixture config");
        let options = TuiOptions {
            config_path: Some(config_path),
            ..crate::test_support::test_tui_options(temp.path())
        };
        let mut app = App::new(options, &config);
        let identity = config
            .active_provider_identity()
            .expect("captured fixture provider");
        assert_eq!(
            config.context_window_for_provider_config(&identity),
            Some(262_144)
        );
        app.set_provider_identity_record(identity);
        app.model = "kimi-k3".to_string();
        app.active_route_limits = Some(codewhale_config::route::RouteLimits {
            context_tokens: Some(262_144),
            ..Default::default()
        });
        app.active_context_window_source = crate::route_runtime::ContextWindowSource::Configured;

        let view = ConfigView::new_for_app(&app);
        let configured = view
            .rows
            .iter()
            .find(|row| row.key == "context_window")
            .expect("configured context row");
        let effective = view
            .rows
            .iter()
            .find(|row| row.key == "effective_context_window")
            .expect("effective context row");

        assert_eq!(configured.value, "262144");
        assert_eq!(effective.value, "262144 tokens · configured");

        app.auto_model = true;
        app.last_effective_provider = Some(app.api_provider);
        app.last_effective_provider_identity = Some("other-moonshot".to_string());
        let automatic_view = ConfigView::new_for_app(&app);
        let configured = automatic_view
            .rows
            .iter()
            .find(|row| row.key == "context_window")
            .expect("automatic configured context row");
        assert_eq!(configured.value, "(not set)");
        app.pending_turn_route = Some((app.api_provider, "kimi-k3".to_string(), true));
        let pending_view = ConfigView::new_for_app(&app);
        let configured = pending_view
            .rows
            .iter()
            .find(|row| row.key == "context_window")
            .expect("pending foreign configured context row");
        assert_eq!(configured.value, "(not set)");
        app.pending_turn_route = None;
        app.auto_model = false;

        // Same provider kind is insufficient when a different owned table is
        // selected. The saved row must not borrow another table's limit.
        let saved_path = PathBuf::from(
            std::env::var_os("DEEPSEEK_CONFIG_PATH").expect("isolated saved config path"),
        );
        std::fs::write(
            &saved_path,
            r#"
provider = "custom-current"
[providers.custom-current]
kind = "openai-compatible"
base_url = "https://current.example.invalid/v1"
model = "fixture-model"
context_window = 131072
"#,
        )
        .expect("saved custom provider");
        let saved = Config::load(Some(saved_path.clone()), None).expect("saved custom config");
        let saved_identity = saved
            .active_provider_identity()
            .expect("saved admitted custom identity");
        assert_eq!(saved_identity.provider, crate::config::ProviderKind::Custom);
        assert_eq!(saved_identity.key.as_str(), "custom-current");
        assert_eq!(
            saved.context_window_for_provider_config(&saved_identity),
            Some(131_072)
        );
        let options = TuiOptions {
            config_path: Some(saved_path),
            ..crate::test_support::test_tui_options(temp.path())
        };
        let mut app = App::new(options, &saved);
        app.set_provider_identity_record(saved_identity.clone());
        let saved_view = ConfigView::new_for_app(&app);
        let configured = saved_view
            .rows
            .iter()
            .find(|row| row.key == "context_window")
            .expect("saved custom configured context row");
        assert_eq!(configured.value, "131072");
        let foreign: Config = toml::from_str(
            r#"
provider = "custom-other"
[providers.custom-other]
kind = "openai-compatible"
base_url = "https://other.example.invalid/v1"
model = "fixture-model"
context_window = 131072
"#,
        )
        .expect("foreign configured provider");
        let foreign_identity = foreign
            .active_provider_identity()
            .expect("foreign admitted provider identity");
        assert_eq!(
            foreign_identity.provider,
            crate::config::ProviderKind::Custom
        );
        assert_eq!(foreign_identity.provider, app.api_provider);
        assert_eq!(foreign_identity.key.as_str(), "custom-other");
        assert_ne!(foreign_identity.key, saved_identity.key);
        app.set_provider_identity_record(foreign_identity);
        let foreign_view = ConfigView::new_for_app(&app);
        let configured = foreign_view
            .rows
            .iter()
            .find(|row| row.key == "context_window")
            .expect("foreign configured context row");
        assert_eq!(configured.value, "(not set)");
    }

    #[test]
    fn config_view_displays_saved_codex_reasoning_effort_label() {
        let _guard = ConfigSettingsEnvGuard::new("reasoning_effort = \"max\"\n");
        let mut app = create_test_app();
        app.api_provider = crate::config::ProviderKind::OpenaiCodex;

        let view = ConfigView::new_for_app(&app);
        let row = view
            .rows
            .iter()
            .find(|row| row.key == "reasoning_effort")
            .expect("reasoning_effort row");

        assert_eq!(row.value, "max");
    }

    #[test]
    fn config_view_editing_localized_default_placeholders_starts_blank() {
        let _guard = ConfigSettingsEnvGuard::new("locale = \"zh-Hans\"\n");
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);

        for (key, message_id) in [
            ("reasoning_effort", MessageId::ConfigDefaultReasoning),
            ("background_color", MessageId::ConfigDefaultValue),
        ] {
            view.focus_key(key);
            view.start_edit();

            let edit = view.editing.as_ref().expect("editing should start");
            assert_eq!(edit.original_value, tr(Locale::ZhHans, message_id));
            assert!(
                edit.buffer.is_empty(),
                "localized default placeholder should not become edit text for {key}"
            );

            view.editing = None;
        }
    }

    #[test]
    fn config_view_filter_matches_group_and_rows() {
        let mut view = create_config_view(Locale::En);

        type_filter(&mut view, "workbar");

        assert_eq!(view.filter, "workbar");
        assert_eq!(visible_section_labels(&view), vec!["Workbar"]);
        assert_eq!(
            visible_row_keys(&view),
            vec![
                "work_surface_placement",
                "work_surface_top_height",
                "work_surface_side_width",
                "rail_panel",
            ]
        );
        assert_eq!(view.rows[view.selected].key, "work_surface_placement");
    }

    #[test]
    fn localized_config_view_filter_matches_english_section_and_scope_labels() {
        let mut view = create_config_view(Locale::PtBr);

        type_filter(&mut view, "workbar saved");

        assert_eq!(view.filter, "workbar saved");
        assert_eq!(visible_section_labels(&view), vec!["Barra lateral"]);
        assert_eq!(
            visible_row_keys(&view),
            vec![
                "work_surface_placement",
                "work_surface_top_height",
                "work_surface_side_width",
                "rail_panel",
            ]
        );
    }

    #[test]
    fn config_view_filter_accepts_unicode_case() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);

        type_filter(&mut view, "thinking");
        assert_eq!(
            visible_row_keys(&view),
            vec![
                // `reasoning_effort` joined this filter when the thinking
                // ladder gave it a config row; the schema files it under
                // Models, so it now sorts after the appearance rows.
                "show_thinking",
                "thinking_default_expanded",
                "thinking_preview_lines",
                "thinking_highlight",
                "reasoning_effort"
            ]
        );

        view.clear_filter();
        view.rows[0].value = "CAFÉ".to_string();
        type_filter(&mut view, "café");
        assert_eq!(visible_row_keys(&view), vec!["theme"]);
    }

    fn assert_config_search_owns_text(query: &str) {
        let mut view = create_config_view(Locale::En);
        // Start on an actionable boolean so a stolen Space would emit a
        // persisted update, and a stolen e would open an editor.
        view.focus_key("low_motion");
        let values = view
            .rows
            .iter()
            .map(|row| row.value.clone())
            .collect::<Vec<_>>();
        let mut stack = ViewStack::new();
        stack.push(view);
        for ch in query.chars() {
            assert!(
                stack
                    .handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
                    .is_empty(),
                "{query:?}"
            );
            assert_eq!(stack.top_kind(), Some(ModalKind::Config), "{query:?}");
        }
        let mut modal = stack.pop().unwrap();
        let view = modal.as_any_mut().downcast_mut::<ConfigView>().unwrap();
        assert_eq!(view.filter, query);
        assert!(
            view.editing.is_none(),
            "search text must not enter a settings editor"
        );
        assert_eq!(
            view.rows
                .iter()
                .map(|row| row.value.clone())
                .collect::<Vec<_>>(),
            values
        );
        assert!(matches!(
            view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ViewAction::None
        ));
        assert!(view.filter.is_empty());
        assert!(matches!(
            view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ViewAction::Close
        ));
    }

    #[test]
    fn config_search_owns_initial_q() {
        assert_config_search_owns_text("quiet");
        assert_config_search_owns_text("Queue");
    }

    #[test]
    fn config_search_owns_initial_e() {
        assert_config_search_owns_text("effort");
        assert_config_search_owns_text("Effort");
    }

    #[test]
    fn config_search_owns_initial_j() {
        assert_config_search_owns_text("json");
    }

    #[test]
    fn config_search_owns_initial_k() {
        assert_config_search_owns_text("key");
    }

    #[test]
    fn config_search_owns_initial_space() {
        assert_config_search_owns_text(" 队列é");
    }

    #[test]
    fn config_view_filter_matches_friendly_labels_and_hints() {
        let mut view = create_config_view(Locale::En);

        type_filter(&mut view, "shell access");
        assert_eq!(visible_row_keys(&view), vec!["allow_shell"]);

        view.clear_filter();
        type_filter(&mut view, "reasoning level");
        assert_eq!(visible_row_keys(&view), vec!["reasoning_effort"]);

        view.clear_filter();
        type_filter(&mut view, "fan-out/fan-in");
        assert_eq!(visible_row_keys(&view), vec!["workflow"]);
    }

    /// #5134 filed an issue to ask how to raise the context window, because
    /// the rows that answer it are keyed `context_window` and only findable by
    /// someone who already knows that name. The filter has to answer the words
    /// a user actually types.
    #[test]
    fn config_view_filter_finds_context_window_by_user_vocabulary() {
        let mut view = create_config_view(Locale::En);

        for phrase in ["context length", "context size", "max context length"] {
            view.clear_filter();
            type_filter(&mut view, phrase);
            let keys = visible_row_keys(&view);
            assert!(
                keys.contains(&"context_window"),
                "`{phrase}` must surface the context_window row: {keys:?}"
            );
            assert!(
                keys.contains(&"effective_context_window"),
                "`{phrase}` must surface the resolved window row: {keys:?}"
            );
        }

        // The adjacent knob the same user reaches for next.
        view.clear_filter();
        type_filter(&mut view, "compaction threshold");
        let keys = visible_row_keys(&view);
        assert!(
            keys.contains(&"auto_compact_threshold_percent"),
            "`compaction threshold` must surface the auto-compaction trigger: {keys:?}"
        );
    }

    #[test]
    fn config_view_renders_friendly_setting_labels() {
        let mut view = create_config_view(Locale::En);
        assert_ne!(
            config_label_for_key("show_thinking"),
            config_label_for_key("thinking_highlight"),
            "reasoning visibility and background controls need distinct labels"
        );
        view.category = ConfigCategory::ModelsProviders;
        view.select_first_visible_row();
        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);

        view.render(area, &mut buf);

        let dump = buffer_text(&buf, area);
        assert!(
            dump.contains("Active provider"),
            "missing provider label:\n{dump}"
        );
        assert!(
            dump.contains("Models & providers"),
            "missing settings rail:\n{dump}"
        );

        view.category = ConfigCategory::Trust;
        view.select_first_visible_row();
        let mut permission_buf = Buffer::empty(area);
        view.render(area, &mut permission_buf);
        let permission_dump = buffer_text(&permission_buf, area);
        assert!(
            permission_dump.contains("Shell access"),
            "missing shell label:\n{permission_dump}"
        );
    }

    #[test]
    fn localized_config_view_renders_at_narrow_width() {
        let mut app = create_test_app();
        app.ui_locale = Locale::PtBr;
        let mut view = ConfigView::new_for_app(&app);
        view.category = ConfigCategory::ModelsProviders;
        view.select_first_visible_row();
        let area = Rect::new(0, 0, 60, 18);
        let mut buf = Buffer::empty(area);

        view.render(area, &mut buf);

        let dump = buffer_text(&buf, area);
        assert!(dump.contains("Provedor"), "missing localized rows:\n{dump}");
        assert!(
            !dump.contains("MISSING"),
            "missing-key marker leaked:\n{dump}"
        );
    }

    #[test]
    fn config_view_selected_row_uses_muted_selection_highlight() {
        let mut view = create_config_view(Locale::En);
        view.selected = view
            .rows
            .iter()
            .position(|row| row.key == "theme")
            .expect("theme row");
        view.category = ConfigCategory::Appearance;
        view.adjust_scroll(8);
        let area = Rect::new(0, 0, 100, 24);
        let mut buf = Buffer::empty(area);

        view.render(area, &mut buf);

        let y = view
            .last_row_hitboxes
            .borrow()
            .iter()
            .find_map(|(rect, idx)| (*idx == view.selected).then_some(rect.y))
            .expect("selected config row should have a hitbox");
        let highlighted_cells = (area.x..area.x.saturating_add(area.width))
            .filter(|&x| {
                let cell = &buf[(x, y)];
                !cell.symbol().trim().is_empty()
                    && cell.bg == palette::SELECTION_BG
                    && cell.fg == palette::SELECTION_TEXT
            })
            .count();

        assert!(
            highlighted_cells >= 4,
            "selected config row should render readable selection text"
        );
        assert!(
            !(area.x..area.x.saturating_add(area.width))
                .any(|x| buf[(x, y)].bg == palette::WHALE_ACTION),
            "selected config row should not use the bright accent background"
        );
    }

    #[test]
    fn config_view_keeps_scope_column_aligned_for_long_keys() {
        let mut view = create_config_view(Locale::ZhHans);
        type_filter(&mut view, "composer");
        let area = Rect::new(0, 0, 100, 24);
        let mut buf = Buffer::empty(area);

        view.render(area, &mut buf);

        let dump = buffer_text(&buf, area);
        assert!(
            dump.contains("粘 贴 检 测"),
            "localized config labels should stay readable:\n{dump}"
        );
        let scope_columns = (area.y..area.y.saturating_add(area.height))
            .filter_map(|y| {
                // One dumped char per cell (wide glyphs dump as glyph +
                // continuation cell), so a char count is the cell column.
                // Every list row paints an affordance in the same column, so
                // the first affordance glyph is the shared alignment anchor.
                let line = buffer_row_text(&buf, area, y);
                if !line.contains("已 保 存") {
                    return None;
                }
                line.find(['‹', '[', '✎'])
                    .map(|byte| line[..byte].chars().count())
            })
            .collect::<Vec<_>>();
        assert!(
            scope_columns.len() >= 2,
            "expected composer config rows with scopes:\n{dump}"
        );
        assert!(
            scope_columns
                .iter()
                .all(|column| *column == scope_columns[0]),
            "scope column should stay aligned even for long keys ({scope_columns:?}):\n{dump}"
        );
    }

    #[test]
    fn config_view_filter_no_match_does_not_edit_hidden_row() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);

        type_filter(&mut view, "zzzz");
        assert!(visible_row_keys(&view).is_empty());

        let action = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(action, ViewAction::None));
        assert!(view.editing.is_none());

        let clear = view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(clear, ViewAction::None));
        assert!(view.filter.is_empty());
        assert!(!visible_row_keys(&view).is_empty());
    }

    #[test]
    fn a_rejected_setting_reopens_the_editor_with_the_typed_value() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        type_filter(&mut view, "mcp_config");
        view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        view.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_filter(&mut view, "servers.json");
        assert!(matches!(
            view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ViewAction::Emit(ViewEvent::ConfigUpdated { .. })
        ));
        assert!(view.editing.is_none());

        let accepted = ConfigView::rebuild_preserving(&app, &view, "mcp_config_path");
        assert!(
            accepted.editing.is_none(),
            "an accepted value closes the editor"
        );

        let mut rejected = ConfigView::rebuild_preserving(&app, &view, "mcp_config_path");
        rejected.restore_rejected_commit(&view);
        let edit = rejected.editing.as_ref().expect("a rejected value reopens");
        assert_eq!(edit.key.as_str(), "mcp_config_path");
        assert_eq!(edit.buffer.iter().collect::<String>(), "servers.json");
    }

    #[test]
    fn a_pending_worker_row_reads_queued_not_running() {
        use crate::tools::subagent::AgentWorkerStatus;
        assert_eq!(
            super::lifecycle_worker_status(AgentLifecycle::Pending),
            Some(AgentWorkerStatus::Queued)
        );
        assert_eq!(
            super::lifecycle_worker_status(AgentLifecycle::Running),
            None
        );
        let (queued, ..) = super::format_agent_status(
            Locale::En,
            &SubAgentStatus::Running,
            Some(AgentWorkerStatus::Queued),
        );
        let (running, ..) = super::format_agent_status(Locale::En, &SubAgentStatus::Running, None);
        assert_eq!(queued, tr(Locale::En, MessageId::AutomationRunStatusQueued));
        assert_ne!(queued, running);
    }

    #[test]
    fn config_view_can_edit_filtered_row() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);

        type_filter(&mut view, "mcp_config");
        assert_eq!(visible_row_keys(&view), vec!["mcp_config_path"]);

        let start = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(start, ViewAction::None));
        assert!(view.editing.is_some());

        let clear = view.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(matches!(clear, ViewAction::None));
        type_filter(&mut view, "servers.json");

        let submit = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match submit {
            ViewAction::Emit(ViewEvent::ConfigUpdated {
                key,
                value,
                persist,
            }) => {
                assert_eq!(key, "mcp_config_path");
                assert_eq!(value, "servers.json");
                assert!(persist);
            }
            other => panic!("expected config update emit, got {other:?}"),
        }
    }

    #[test]
    fn config_view_enter_and_ctrl_u_emit_config_updated() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        view.focus_key("background_color");

        let start = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(start, ViewAction::None));
        assert!(view.editing.is_some());

        let clear = view.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(matches!(clear, ViewAction::None));
        let cleared = view
            .editing
            .as_ref()
            .expect("editing should remain active after Ctrl+U");
        assert!(cleared.buffer.is_empty());

        for ch in "55".chars() {
            let action = view.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
            assert!(matches!(action, ViewAction::None));
        }

        let submit = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match submit {
            ViewAction::Emit(ViewEvent::ConfigUpdated {
                key,
                value,
                persist,
            }) => {
                assert_eq!(key, "background_color");
                assert_eq!(value, "55");
                assert!(persist);
            }
            other => panic!("expected config update emit, got {other:?}"),
        }
        assert!(view.editing.is_none());
    }

    #[test]
    fn config_view_boolean_rows_toggle_without_text_editing() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        view.focus_key("low_motion");
        let expected =
            if canonical_config_choice("low_motion", &view.rows[view.selected].value) == "true" {
                "false"
            } else {
                "true"
            };

        let action = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        match action {
            ViewAction::Emit(ViewEvent::ConfigUpdated {
                key,
                value,
                persist,
            }) => {
                assert_eq!(key, "low_motion");
                assert_eq!(value, expected);
                assert!(persist);
            }
            other => panic!("expected direct boolean update, got {other:?}"),
        }
        assert!(view.editing.is_none());
    }

    #[test]
    fn config_view_enum_rows_use_a_bounded_choice_list() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        view.focus_key("default_mode");

        let start = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(start, ViewAction::None));
        let edit = view.editing.as_ref().expect("choice editor");
        assert_eq!(
            edit.choices.as_deref(),
            Some(
                &[
                    "agent".to_string(),
                    "plan".to_string(),
                    "operate".to_string(),
                ][..]
            )
        );
        assert!(
            edit.choices
                .as_ref()
                .expect("startup choices")
                .iter()
                .all(|choice| choice != "yolo")
        );

        let _ = view.handle_key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE));
        let apply = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match apply {
            ViewAction::Emit(ViewEvent::ConfigUpdated {
                key,
                value,
                persist,
            }) => {
                assert_eq!(key, "default_mode");
                assert_eq!(value, "operate");
                assert!(persist);
            }
            other => panic!("expected startup choice update, got {other:?}"),
        }

        assert_eq!(
            canonical_config_choice("default_mode", "Operate"),
            "operate"
        );
        assert_eq!(
            config_choice_label(Locale::En, "default_mode", "operate"),
            "Operate"
        );
        assert!(!config_choice_detail(Locale::En, "default_mode", "operate").is_empty());
    }

    #[test]
    fn locale_choices_cover_shipped_registry_and_mark_partial_packs() {
        let choices = config_choice_values("locale").expect("locale choices");
        let expected = std::iter::once("auto".to_string())
            .chain(
                Locale::shipped()
                    .iter()
                    .map(|locale| locale.tag().to_string()),
            )
            .collect::<Vec<_>>();
        assert_eq!(
            choices, expected,
            "native locale choices must match Locale::shipped()"
        );

        let partial_badge = tr(Locale::En, MessageId::ConfigLocalePartialBadge);
        let partial_detail = tr(Locale::En, MessageId::ConfigLocalePartialDetail);
        for locale in Locale::shipped() {
            let canonical = canonical_config_choice("locale", locale.tag());
            assert_eq!(canonical, locale.tag());

            let label = config_choice_label(Locale::En, "locale", &canonical);
            assert_eq!(
                label.contains(partial_badge.as_ref()),
                locale.is_partial_pack(),
                "{} partial-pack badge drifted",
                locale.tag()
            );

            let detail = config_choice_detail(Locale::En, "locale", &canonical);
            assert_eq!(
                !detail.is_empty(),
                locale.is_partial_pack(),
                "{} partial-pack detail drifted",
                locale.tag()
            );
            if locale.is_partial_pack() {
                assert_eq!(detail, partial_detail);
            }
        }
    }

    #[test]
    fn locale_choice_editor_submits_newly_admitted_locales() {
        for tag in ["ko", "vi", "zh-Hant"] {
            let mut view = create_config_view(Locale::En);
            view.focus_key("locale");
            view.start_edit();
            let edit = view.editing.as_mut().expect("locale choice editor");
            edit.selected_choice = edit
                .choices
                .as_ref()
                .and_then(|choices| choices.iter().position(|choice| choice == tag))
                .unwrap_or_else(|| panic!("locale choices must include {tag}"));

            match view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
                ViewAction::Emit(ViewEvent::ConfigUpdated { key, value, .. }) => {
                    assert_eq!(key, "locale");
                    assert_eq!(value, tag);
                }
                other => panic!("selecting locale {tag} must submit ConfigUpdated, got {other:?}"),
            }
        }
    }

    #[test]
    fn complete_locale_shows_no_partial_badge_at_minimum_terminal_layout() {
        // zh-Hant reached full en.json parity in #5143 and no shipped pack is
        // partial anymore, so the picker must not render the partial badge.
        let mut view = create_config_view(Locale::En);
        view.focus_key("locale");
        view.start_edit();
        let edit = view.editing.as_mut().expect("locale choice editor");
        edit.selected_choice = edit
            .choices
            .as_ref()
            .and_then(|choices| choices.iter().position(|choice| choice == "zh-Hant"))
            .expect("zh-Hant choice");

        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let dump = buffer_text(&buf, area);
        assert!(
            dump.contains("zh-Hant"),
            "zh-Hant choice must render at minimum layout: {dump:?}"
        );
        assert!(
            !dump.contains("zh-Hant (partial)"),
            "zh-Hant is a complete pack and must not show the partial badge: {dump:?}"
        );
    }

    #[test]
    fn settings_registry_types_every_config_row() {
        let app = create_test_app();
        let view = ConfigView::new_for_app(&app);
        let registry = SettingsRegistry::new(&view);

        let kind_for = |key: &str| {
            let row = view
                .rows
                .iter()
                .find(|row| row.key == key)
                .unwrap_or_else(|| panic!("missing config row {key}"));
            registry.meta(row).kind
        };

        assert_eq!(kind_for("provider"), SettingKind::Action);
        assert_eq!(kind_for("model"), SettingKind::Action);
        assert_eq!(kind_for("low_motion"), SettingKind::Boolean);
        assert_eq!(kind_for("default_mode"), SettingKind::Choice);
        assert_eq!(kind_for("thinking_preview_lines"), SettingKind::Integer);
        assert_eq!(kind_for("mcp_open"), SettingKind::Action);
        assert_eq!(kind_for("mcp_reconnect"), SettingKind::Action);
        assert_eq!(kind_for("mcp_diagnose"), SettingKind::Action);
        assert_eq!(kind_for("plugins_open"), SettingKind::Action);
        assert_eq!(kind_for("mcp_config_path"), SettingKind::Text);
        assert_eq!(
            kind_for("fleet.exec.max_spawn_depth"),
            SettingKind::ReadOnly
        );

        for row in &view.rows {
            let meta = registry.meta(row);
            assert_eq!(meta.category, row.section());
            assert_eq!(
                meta.kind == SettingKind::Choice || meta.kind == SettingKind::Boolean,
                meta.choices.is_some(),
                "choice metadata drifted for {}",
                row.key
            );
        }
    }

    #[test]
    fn config_labels_are_consumed_from_complete_locale_packs() {
        for locale in Locale::shipped_complete() {
            assert_eq!(
                config_label_for_key_for_locale(*locale, "provider"),
                tr(*locale, MessageId::ConfigLabelProvider)
            );
            assert_eq!(
                config_label_for_key_for_locale(*locale, "features.mcp"),
                tr(*locale, MessageId::ConfigLabelFeaturePrefix).replace("{name}", "Mcp")
            );
        }
        assert_ne!(
            config_label_for_key_for_locale(Locale::Ja, "provider"),
            config_label_for_key_for_locale(Locale::En, "provider")
        );
    }

    #[test]
    fn model_row_hint_names_the_model_picker() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        view.focus_key("model");

        let hint = view.activation_copy(&view.rows[view.selected]);
        assert!(hint.contains("Enter opens model picker"), "{hint}");
        assert!(!hint.contains("Enter opens provider picker"), "{hint}");
        assert!(
            hint.starts_with(&en(MessageId::ConfigActivateAgain)),
            "{hint}"
        );
    }

    #[test]
    fn config_view_mouse_wheel_moves_rows_and_choice_selection() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        let first_row = view.selected;

        let _ = view.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert!(
            view.selected > first_row,
            "wheel should move the settings list"
        );

        view.focus_key("default_mode");
        view.start_edit();
        view.editing
            .as_mut()
            .expect("choice editor")
            .selected_choice = 0;
        let _ = view.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(
            view.editing
                .as_ref()
                .expect("choice editor")
                .selected_choice,
            1
        );
    }

    #[test]
    fn config_view_mouse_click_selects_row() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        view.category = ConfigCategory::ModelsProviders;
        view.select_first_visible_row();
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        let hitboxes = view.last_row_hitboxes.borrow().clone();
        let (_, row_idx) = hitboxes
            .iter()
            .find(|(_, idx)| view.rows.get(*idx).is_some_and(|row| row.key == "model"))
            .copied()
            .expect("model row should have a hitbox");
        let y = hitboxes
            .iter()
            .find_map(|(rect, idx)| (*idx == row_idx).then_some(rect.y))
            .expect("selected row should have a y coordinate");

        let action = view.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 20,
            row: y,
            modifiers: KeyModifiers::NONE,
        });

        assert!(matches!(action, ViewAction::None));
        assert_eq!(view.selected, row_idx);

        let second = view.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 20,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
        match second {
            ViewAction::Emit(ViewEvent::CommandPaletteSelected {
                action: CommandPaletteAction::ExecuteCommand { command },
            }) => assert_eq!(command, "/model"),
            other => panic!("second click should open the model picker, got {other:?}"),
        }
        assert!(view.editing.is_none());
    }

    #[test]
    fn config_view_hover_tints_without_moving_selection() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        let area = Rect::new(0, 0, 120, 32);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let selected_before = view.selected;

        // Hover a non-selected row: the tint lands, the selection holds.
        let (rect, row_idx) = view
            .last_row_hitboxes
            .borrow()
            .iter()
            .copied()
            .find(|(_, idx)| *idx != selected_before)
            .expect("a non-selected row");
        let action = view.handle_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: rect.x.saturating_add(1),
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(action, ViewAction::None));
        assert_eq!(view.hovered_row, Some(row_idx));
        assert_eq!(view.selected, selected_before);

        // Repaint: the hovered row wears the shared hover band.
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        assert_eq!(
            buf[(rect.x, rect.y)].bg,
            palette::SURFACE_ELEVATED,
            "hovered row must show the shared hover band"
        );

        // Hover a strip chip: the rail tint lands, the tab holds.
        let (chip, _) = view
            .last_rail_hitboxes
            .borrow()
            .iter()
            .copied()
            .find(|(_, category)| *category == ConfigCategory::Advanced)
            .expect("Advanced chip");
        let action = view.handle_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: chip.x.saturating_add(1),
            row: chip.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(action, ViewAction::None));
        assert_eq!(view.hovered_rail, Some(ConfigCategory::Advanced));
        assert_eq!(view.category, ConfigCategory::Appearance);

        // The search line is no target: hovering it clears every tint.
        let action = view.handle_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 5,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(action, ViewAction::None));
        assert_eq!(view.hovered_row, None);
        assert_eq!(view.hovered_rail, None);
    }

    #[test]
    fn config_view_rail_categories_are_clickable() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        assert_eq!(view.category, ConfigCategory::Appearance);
        // 120 columns: the vertical rail; 100 columns (96 inner): the strip.
        for width in [120u16, 100] {
            view.category = ConfigCategory::Appearance;
            view.select_first_visible_row();
            let area = Rect::new(0, 0, width, 30);
            let mut buf = Buffer::empty(area);
            view.render(area, &mut buf);

            let hitboxes = view.last_rail_hitboxes.borrow().clone();
            assert_eq!(hitboxes.len(), ConfigCategory::ALL.len(), "{width}");
            let (rect, category) = hitboxes
                .iter()
                .copied()
                .find(|(_, category)| *category == ConfigCategory::Advanced)
                .expect("Advanced should have a hitbox");

            let action = view.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x.saturating_add(1),
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            });
            assert!(matches!(action, ViewAction::None));
            assert_eq!(category, ConfigCategory::Advanced);
            assert_eq!(view.category, ConfigCategory::Advanced, "{width}");
            assert!(
                view.rows
                    .get(view.selected)
                    .is_some_and(|row| ConfigCategory::for_row(row) == ConfigCategory::Advanced)
            );
        }
    }

    #[test]
    fn config_categories_cover_every_row_with_the_approved_seven() {
        let app = create_test_app();
        let view = ConfigView::new_for_app(&app);
        let labels: Vec<Cow<'static, str>> = ConfigCategory::ALL
            .iter()
            .map(|category| category.label(Locale::En))
            .collect();
        assert_eq!(
            labels,
            [
                "Appearance",
                "Models & providers",
                "Work",
                "Tools & MCP",
                "Trust",
                "Motion",
                "Advanced",
            ]
        );
        let category_of = |key: &str| {
            let row = view
                .rows
                .iter()
                .find(|row| row.key == key)
                .unwrap_or_else(|| panic!("row {key}"));
            ConfigCategory::for_row(row)
        };
        assert_eq!(category_of("theme"), ConfigCategory::Appearance);
        assert_eq!(category_of("provider"), ConfigCategory::ModelsProviders);
        assert_eq!(category_of("model"), ConfigCategory::ModelsProviders);
        assert_eq!(
            category_of("reasoning_effort"),
            ConfigCategory::ModelsProviders
        );
        // Raw endpoint, credential receipt, and context diagnostic rows live
        // under Advanced so default categories read as product language.
        for key in ["base_url", "context_window", "effective_context_window"] {
            assert_eq!(category_of(key), ConfigCategory::Advanced, "{key}");
        }
        assert!(
            view.rows
                .iter()
                .filter(|row| ConfigCategory::for_row(row) == ConfigCategory::ModelsProviders)
                .all(|row| !row.key.starts_with("external_credentials.")),
            "credential receipts must not surface in Models & providers"
        );
        // Sub-agent depth moved out of the one-row Fleet tab into Models.
        assert_eq!(
            category_of("fleet.exec.max_spawn_depth"),
            ConfigCategory::ModelsProviders
        );
        assert_eq!(category_of("composer_density"), ConfigCategory::Work);
        assert_eq!(category_of("work_surface_placement"), ConfigCategory::Work);
        assert_eq!(category_of("auto_compact"), ConfigCategory::Work);
        assert_eq!(category_of("mcp_open"), ConfigCategory::ToolsMcp);
        assert_eq!(category_of("approval_mode"), ConfigCategory::Trust);
        assert_eq!(category_of("allow_shell"), ConfigCategory::Trust);
        assert_eq!(category_of("telemetry"), ConfigCategory::Trust);
        assert_eq!(category_of("low_motion"), ConfigCategory::Motion);
        assert_eq!(category_of("fancy_animations"), ConfigCategory::Motion);
        for category in ConfigCategory::ALL {
            assert!(
                view.rows.iter().any(|row| category.contains(row)),
                "{} lists no rows",
                category.label(Locale::En)
            );
        }
        for category in ConfigCategory::ALL {
            assert_eq!(category.next().prev(), category);
        }
    }

    /// English copy for an id, for asserting on rendered chrome.
    fn en(id: MessageId) -> String {
        tr(Locale::En, id).into_owned()
    }

    fn render_dump(view: &ConfigView, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        buffer_text(&buf, area)
    }

    #[test]
    fn config_shell_uses_strip_and_list_at_80x24_and_rail_list_detail_when_wide() {
        let app = create_test_app();
        let view = ConfigView::new_for_app(&app);
        assert_eq!(view.category, ConfigCategory::Appearance);

        let dump = render_dump(&view, 80, 24);
        assert!(
            dump.contains("Appearance"),
            "strip shows the active category:\n{dump}"
        );
        assert!(dump.contains("Theme"), "list missing theme row:\n{dump}");
        assert!(
            !dump.contains("▸ Appearance"),
            "no vertical rail at 80 columns:\n{dump}"
        );
        assert!(
            !dump.contains(&format!("{:<10}", en(MessageId::ConfigFactSource))),
            "detail pane must shed at 80 columns:\n{dump}"
        );
        assert!(
            dump.contains(&en(MessageId::ConfigActivateAgain)),
            "selected row spells out its activation:\n{dump}"
        );
        assert!(
            dump.contains(&en(MessageId::ConfigFactCurrent))
                && dump.contains(&en(MessageId::ConfigFactSaved)),
            "narrow status row folds the lanes:\n{dump}"
        );
        {
            let rows = view.last_row_hitboxes.borrow();
            assert!(!rows.is_empty(), "list rows must stay clickable at 80x24");
            assert!(
                rows.iter()
                    .all(|(rect, _)| rect.width > 20 && rect.height == 1),
                "row hitboxes are exact rects: {rows:?}"
            );
            let strip = view.last_rail_hitboxes.borrow();
            assert!(!strip.is_empty() && strip.len() <= 8, "{strip:?}");
            assert!(
                strip.iter().all(|(rect, _)| rect.y < rows[0].0.y),
                "strip sits above the list: {strip:?}"
            );
        }

        let dump = render_dump(&view, 120, 32);
        assert!(
            dump.contains("❯ Display"),
            "groups column lists the active tab's groups:\n{dump}"
        );
        for label in [
            &en(MessageId::ConfigFactCurrent),
            &en(MessageId::ConfigFactSaved),
            &en(MessageId::ConfigFactStartup),
            &en(MessageId::ConfigFactSource),
            &en(MessageId::ConfigFactScope),
            &en(MessageId::ConfigFactApply),
            &en(MessageId::ConfigFactKind),
            &en(MessageId::ConfigFactAvailable),
        ] {
            assert!(
                dump.contains(&format!("{label:<10}")),
                "detail pane missing {label:?}:\n{dump}"
            );
        }
        assert!(
            dump.contains(&en(MessageId::ConfigSourceUserSettings)),
            "source names the store:\n{dump}"
        );
        assert!(
            dump.contains(&en(MessageId::ConfigApplyOnSave)),
            "apply semantics:\n{dump}"
        );
        assert!(
            dump.contains(&en(MessageId::ConfigKindChoice)),
            "editor kind painted for the theme row:\n{dump}"
        );
        assert_eq!(view.last_rail_hitboxes.borrow().len(), 7);

        for (w, h) in [(0u16, 0u16), (20, 4), (44, 12), (60, 18), (300, 60)] {
            let _ = render_dump(&view, w, h);
        }
    }

    #[test]
    fn config_shell_blocker_sizes_keep_active_category_list_and_affordances() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        for (w, h) in [(40u16, 12u16), (80, 24), (100, 30), (120, 32)] {
            view.category = ConfigCategory::Appearance;
            view.select_first_visible_row();
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            view.render(area, &mut buf);
            let dump = buffer_text(&buf, area);
            assert!(dump.contains("Appearance"), "{w}x{h}:\n{dump}");
            assert!(dump.contains("Search:"), "{w}x{h} keeps search:\n{dump}");
            let rows = view.last_row_hitboxes.borrow().clone();
            assert!(!rows.is_empty(), "{w}x{h} lists rows:\n{dump}");
            // Every listed row paints its affordance glyph inside its own
            // hitbox cells (toggle / choose / edit / open / read-only).
            for (rect, idx) in rows {
                let row = &view.rows[idx];
                let kind = view.editor_kind(row);
                let line: String = (rect.x..rect.right())
                    .map(|x| buf[(x, rect.y)].symbol().to_string())
                    .collect();
                let glyph = super::setting_affordance(kind, Some(true));
                let glyph_off = super::setting_affordance(kind, Some(false));
                assert!(
                    line.contains(glyph) || line.contains(glyph_off),
                    "{w}x{h} row {} lacks its {kind:?} affordance: {line:?}",
                    row.key
                );
            }
        }
    }

    #[test]
    fn config_shell_short_heights_keep_advanced_reachable_by_keys_and_pointer() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        for (w, h) in [(40u16, 12u16), (44, 12), (60, 16)] {
            view.category = ConfigCategory::Appearance;
            view.select_first_visible_row();
            for _ in 0..6 {
                let _ = view.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
            }
            assert_eq!(view.category, ConfigCategory::Advanced, "{w}x{h}");
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            view.render(area, &mut buf);
            let dump = buffer_text(&buf, area);
            assert!(
                dump.contains("Advanced"),
                "{w}x{h} active category:\n{dump}"
            );
            let strip = view.last_rail_hitboxes.borrow().clone();
            let (advanced, _) = strip
                .iter()
                .copied()
                .find(|(_, category)| *category == ConfigCategory::Advanced)
                .unwrap_or_else(|| panic!("{w}x{h} Advanced hitbox: {dump}"));
            let cells: String = (advanced.x..advanced.right())
                .map(|x| buf[(x, advanced.y)].symbol().to_string())
                .collect();
            assert!(
                cells.contains("Advanced"),
                "{w}x{h} hitbox cells: {cells:?}"
            );
            // Pointer parity: the neighbour chip or compact Previous control
            // moves the category exactly as ← does.
            let _ = view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
            let by_key = view.category;
            let _ = view.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
            let mut buf = Buffer::empty(area);
            view.render(area, &mut buf);
            let strip = view.last_rail_hitboxes.borrow().clone();
            let motion = strip
                .iter()
                .find(|(_, category)| *category == by_key)
                .map(|(rect, _)| *rect)
                .or_else(|| {
                    view.last_nav_controls
                        .borrow()
                        .iter()
                        .find(|(_, step)| *step == super::NavStep::Previous)
                        .map(|(rect, _)| *rect)
                })
                .unwrap_or_else(|| panic!("{w}x{h} {by_key:?} navigation target"));
            let action = view.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: motion.x,
                row: motion.y,
                modifiers: KeyModifiers::NONE,
            });
            assert!(matches!(action, ViewAction::None));
            assert_eq!(view.category, by_key, "{w}x{h} pointer parity");
        }
    }

    #[test]
    fn config_detail_never_synthesizes_current_from_saved() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        let low_motion = view
            .rows
            .iter()
            .position(|row| row.key == "low_motion")
            .expect("low_motion row");
        view.rows[low_motion].value = "false".to_string();
        view.rows[low_motion].facts.effective = Some("true".to_string());
        // Pin the authority: the host motion-override probe (a legacy console
        // host, NO_ANIMATIONS, an SSH session, …) would otherwise relabel this
        // row and make the test host-dependent. The subject here is the
        // saved/effective synthesis contract, not override detection.
        view.rows[low_motion].facts.authority = super::SettingAuthority::UserSettings;
        view.rows[low_motion].facts.authority_detail = None;
        view.category = ConfigCategory::Motion;
        view.selected = low_motion;

        let fact = view
            .setting_fact(&view.rows[low_motion])
            .expect("setting fact");
        assert_eq!(fact.effective.as_deref(), Some("On"));
        assert_eq!(fact.saved.as_deref(), Some("Off"));
        assert_eq!(fact.startup.as_deref(), Some("Off"));
        assert_eq!(fact.authority, super::SettingAuthority::UserSettings);
        assert_eq!(fact.apply, super::SettingApplySemantics::Immediate);

        let dump = render_dump(&view, 120, 32);
        let lane = |name: &str| {
            dump.lines()
                .find(|line| line.contains(&format!("{name:<10}")))
                .unwrap_or_else(|| panic!("{name} lane:\n{dump}"))
                .to_string()
        };
        assert!(lane("current").contains("On"), "{}", lane("current"));
        assert!(lane("saved").contains("Off"), "{}", lane("saved"));

        // A saved row with no App observation reports current as unobserved
        // instead of echoing the persisted value.
        let calm = view
            .rows
            .iter()
            .find(|row| row.key == "calm_mode")
            .expect("calm_mode row");
        let fact = view.setting_fact(calm).expect("setting fact");
        assert!(fact.effective.is_none() && fact.current.is_none());
        assert_eq!(
            fact.saved.as_deref(),
            Some(config_choice_label(Locale::En, "calm_mode", &calm.value).as_str())
        );
        assert!(
            view.setting_detail_summary(calm)
                .contains(&en(MessageId::ConfigLaneUnobserved))
        );

        // Theme and locale report the value the app is actually running
        // with, from App, not the persisted string.
        let theme = view
            .rows
            .iter()
            .find(|row| row.key == "theme")
            .expect("theme row");
        assert_eq!(
            theme.facts.effective.as_deref(),
            Some(app.theme_id.name()),
            "theme current lane comes from App"
        );
        let locale = view
            .rows
            .iter()
            .find(|row| row.key == "locale")
            .expect("locale row");
        assert_eq!(locale.facts.effective.as_deref(), Some(app.ui_locale.tag()));

        // Session-owned facts never claim a saved default they did not read.
        let provider = view
            .rows
            .iter()
            .find(|row| row.key == "provider")
            .expect("provider row");
        let fact = view.setting_fact(provider).expect("setting fact");
        assert_eq!(fact.authority, super::SettingAuthority::Session);
        assert_eq!(fact.effective, view.snapshot.provider.effective);
        assert!(fact.saved.is_none() && fact.startup.is_none());
    }

    #[test]
    fn config_rows_carry_truthful_apply_semantics_and_kinds() {
        use super::{ConfigRowKind, SettingApplySemantics, SettingAuthority};
        let app = create_test_app();
        let view = ConfigView::new_for_app(&app);
        let row = |key: &str| {
            view.rows
                .iter()
                .find(|row| row.key == key)
                .unwrap_or_else(|| panic!("row {key}"))
        };
        assert_eq!(
            row("default_mode").facts.apply,
            SettingApplySemantics::NextSession
        );
        assert_eq!(
            row("mcp_config_path").facts.apply,
            SettingApplySemantics::ReloadRequired
        );
        assert_eq!(row("theme").facts.apply, SettingApplySemantics::Immediate);
        for key in ["mcp_open", "mcp_reconnect", "mcp_diagnose", "plugins_open"] {
            let action = row(key);
            assert_eq!(action.facts.kind, ConfigRowKind::Action, "{key}");
            assert!(view.setting_fact(action).is_none(), "{key} is not a fact");
            assert!(
                view.setting_detail_summary(action)
                    .contains(&en(MessageId::ConfigRowActionNote)),
                "{key}"
            );
        }
        for key in ["effective_context_window", "base_url"] {
            let receipt = row(key);
            assert_eq!(receipt.facts.kind, ConfigRowKind::Diagnostic, "{key}");
            assert!(view.setting_fact(receipt).is_none(), "{key}");
        }
        assert_eq!(
            row("permission_posture").facts.authority,
            SettingAuthority::UserSettings
        );
        assert_eq!(
            row("allow_shell").facts.authority,
            SettingAuthority::WorkspaceConfiguration
        );
        assert_eq!(
            super::ConfigRowFacts::read_only_setting(SettingAuthority::ManagedPolicy).apply,
            SettingApplySemantics::ReadOnly
        );
        assert_eq!(
            row("telemetry").facts.authority,
            SettingAuthority::WorkspaceConfiguration
        );
        assert_eq!(
            super::setting_apply_label(Locale::En, SettingApplySemantics::ReloadRequired),
            en(MessageId::ConfigApplyReload)
        );
        assert_eq!(
            super::setting_apply_label(Locale::En, SettingApplySemantics::UiNowEngineRestart),
            en(MessageId::ConfigApplyUiNowEngineRestart)
        );
        assert_eq!(
            super::setting_apply_label(Locale::ZhHans, SettingApplySemantics::ReloadRequired),
            tr(Locale::ZhHans, MessageId::ConfigApplyReload)
        );
    }

    #[test]
    fn config_list_clicks_outside_row_rects_never_select_or_activate() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        let area = Rect::new(0, 0, 120, 32);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let before = view.selected;
        let rows = view.last_row_hitboxes.borrow().clone();
        let (first, _) = rows[0];
        let list_bottom = rows.iter().map(|(rect, _)| rect.bottom()).max().unwrap();
        let tabs = view.last_rail_hitboxes.borrow().clone();
        let tabs_bottom = tabs.iter().map(|(rect, _)| rect.bottom()).max().unwrap();
        assert!(tabs_bottom <= first.y, "tabs must not overlap the list");

        let click = |view: &mut ConfigView, column: u16, row: u16| {
            view.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        // Rail area on a row that is not a category, the divider between
        // rail and list, the detail pane, the status row, and the footer.
        let probes = [
            (first.x.saturating_sub(4), list_bottom.saturating_sub(1)),
            (first.x.saturating_sub(1), first.y),
            (first.right().saturating_add(3), first.y),
            (first.x.saturating_add(2), area.bottom().saturating_sub(4)),
            (first.x.saturating_add(2), area.bottom().saturating_sub(2)),
        ];
        for (column, row) in probes {
            // Twice: a second click is the activation gesture on a row.
            let _ = click(&mut view, column, row);
            let action = click(&mut view, column, row);
            assert!(matches!(action, ViewAction::None), "({column},{row})");
            assert_eq!(view.selected, before, "({column},{row}) must not select");
            assert!(view.editing.is_none(), "({column},{row}) must not activate");
        }
        // Inside the rect: selects on the first click.
        let (rect, idx) = rows[rows.len() - 1];
        let _ = click(&mut view, rect.x, rect.y);
        assert_eq!(view.selected, idx);
    }

    #[test]
    fn config_search_indexes_categories_and_category_click_clears_filter() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        type_filter(&mut view, "fleet");
        assert!(
            visible_row_keys(&view).contains(&"fleet.exec.max_spawn_depth"),
            "{:?}",
            visible_row_keys(&view)
        );
        view.clear_filter();
        type_filter(&mut view, "trust");
        let keys = visible_row_keys(&view);
        assert!(keys.contains(&"approval_mode"), "{keys:?}");
        assert!(keys.contains(&"telemetry"), "{keys:?}");

        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        // The strip windows from the active (Appearance) chip at 80 columns;
        // Trust is inside that window.
        let (rect, category) = view
            .last_rail_hitboxes
            .borrow()
            .iter()
            .copied()
            .find(|(_, category)| *category == ConfigCategory::Trust)
            .expect("Trust chip visible while filtering");
        let action = view.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(action, ViewAction::None));
        assert_eq!(category, ConfigCategory::Trust);
        assert!(view.filter.is_empty(), "category click clears the filter");
        assert_eq!(view.category, ConfigCategory::Trust);
        let keys = visible_row_keys(&view);
        assert!(keys.contains(&"approval_mode"), "{keys:?}");
        assert!(keys.contains(&"telemetry"), "{keys:?}");
        assert!(
            keys.iter().all(|key| ConfigCategory::Trust
                .contains(view.rows.iter().find(|row| row.key == *key).unwrap())),
            "only Trust rows remain after the click: {keys:?}"
        );
    }

    /// Interaction evidence at the blocker sizes: the real `ConfigView`
    /// driven by keys and pointer at 40x12, 80x24, 100x30, and 120x32. The
    /// rendered buffers are printed (`--nocapture`) as harness evidence — the
    /// real renderer into a ratatui `Buffer`, not a terminal capture.
    #[test]
    #[allow(
        clippy::print_stdout,
        reason = "prints the rendered buffers as interaction evidence under --nocapture"
    )]
    fn config_shell_interaction_evidence_at_blocker_sizes() {
        let app = create_test_app();
        let key = |view: &mut ConfigView, code: KeyCode| {
            view.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
        };
        let click = |view: &mut ConfigView, column: u16, row: u16| {
            view.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        for (w, h) in [(40u16, 12u16), (80, 24), (100, 30), (120, 32)] {
            let mut view = ConfigView::new_for_app(&app);
            let area = Rect::new(0, 0, w, h);
            let snapshot = |view: &ConfigView, step: &str| {
                let mut buf = Buffer::empty(area);
                view.render(area, &mut buf);
                let dump = buffer_text(&buf, area);
                println!("== {w}x{h} · {step} ==\n{dump}");
                dump
            };

            // Opens on Appearance with the first Appearance row selected.
            assert_eq!(view.category, ConfigCategory::Appearance);
            assert_eq!(view.rows[view.selected].key, "theme");
            let dump = snapshot(&view, "open");
            assert!(dump.contains("Appearance"), "{w}x{h}:\n{dump}");
            assert!(dump.contains("Search:"), "{w}x{h}:\n{dump}");

            // → lands on Models & providers (the one-row Fleet tab is gone;
            // sub-agent depth moved into the Model group). Focus it and check
            // the read-only config.toml posture.
            assert!(matches!(key(&mut view, KeyCode::Right), ViewAction::None));
            assert_eq!(view.category, ConfigCategory::ModelsProviders);
            view.focus_key("fleet.exec.max_spawn_depth");
            assert_eq!(view.rows[view.selected].key, "fleet.exec.max_spawn_depth");
            let dump = snapshot(&view, "after → (Models & providers)");
            assert!(dump.contains("Models & providers"), "{w}x{h}:\n{dump}");
            assert!(
                dump.contains(super::setting_affordance(SettingKind::ReadOnly, None)),
                "{w}x{h} read-only affordance:\n{dump}"
            );
            assert!(matches!(key(&mut view, KeyCode::Enter), ViewAction::None));
            assert!(view.editing.is_none(), "{w}x{h} read-only rows never edit");

            // Tab ×4 → Motion; ↓ → fancy_animations; Enter toggles it and
            // emits the persisted update without opening an editor.
            for _ in 0..4 {
                assert!(matches!(key(&mut view, KeyCode::Tab), ViewAction::None));
            }
            assert_eq!(view.category, ConfigCategory::Motion);
            assert_eq!(view.rows[view.selected].key, "low_motion");
            assert!(matches!(key(&mut view, KeyCode::Down), ViewAction::None));
            assert_eq!(view.rows[view.selected].key, "fancy_animations");
            let dump = snapshot(&view, "Motion · ↓ to fancy_animations");
            assert!(
                if w < 50 {
                    dump.contains("Enter")
                } else {
                    dump.contains(&en(MessageId::ConfigActivateAgain))
                },
                "{w}x{h} activation copy:\n{dump}"
            );
            match key(&mut view, KeyCode::Enter) {
                ViewAction::Emit(ViewEvent::ConfigUpdated { key, persist, .. }) => {
                    assert_eq!(key, "fancy_animations");
                    assert!(persist);
                }
                other => panic!("{w}x{h} Enter should toggle, got {other:?}"),
            }
            assert!(view.editing.is_none());

            // Pointer parity: click a visible non-active chip or the compact
            // Previous control, then select and activate the first row.
            let mut buf = Buffer::empty(area);
            view.render(area, &mut buf);
            let (chip, target) = view
                .last_rail_hitboxes
                .borrow()
                .iter()
                .copied()
                .find(|(_, category)| *category != ConfigCategory::Motion)
                .or_else(|| {
                    view.last_nav_controls
                        .borrow()
                        .iter()
                        .find(|(_, step)| *step == super::NavStep::Previous)
                        .map(|(rect, _)| (*rect, ConfigCategory::Trust))
                })
                .expect("another category is reachable through a painted target");
            assert!(matches!(click(&mut view, chip.x, chip.y), ViewAction::None));
            assert_eq!(view.category, target, "{w}x{h} chip click");
            let mut buf = Buffer::empty(area);
            view.render(area, &mut buf);
            let (row_rect, row_idx) = view.last_row_hitboxes.borrow()[0];
            assert!(matches!(
                click(&mut view, row_rect.x + 1, row_rect.y),
                ViewAction::None
            ));
            assert_eq!(view.selected, row_idx, "{w}x{h} first click selects");
            let dump = snapshot(
                &view,
                &format!("pointer · {} chip, row selected", target.label(Locale::En)),
            );
            assert!(dump.contains("❯"), "{w}x{h} selected row marker:\n{dump}");
            let second = click(&mut view, row_rect.x + 1, row_rect.y);
            let row = &view.rows[row_idx];
            match (row.editable, row.facts.command) {
                (false, _) => {
                    assert!(matches!(second, ViewAction::None));
                    assert!(view.editing.is_none());
                }
                (true, Some((command, _))) => match second {
                    ViewAction::Emit(ViewEvent::CommandPaletteSelected {
                        action: CommandPaletteAction::ExecuteCommand { command: emitted },
                    }) => assert_eq!(emitted, command),
                    ViewAction::Emit(ViewEvent::ExecutePanelCommand {
                        command: emitted,
                        pager_title: Some(_),
                    }) => assert_eq!(emitted, command),
                    other => panic!("{w}x{h} second click should open {command}: {other:?}"),
                },
                (true, None) => assert!(
                    view.editing.is_some() || matches!(second, ViewAction::Emit(_)),
                    "{w}x{h} second click must edit or emit: {second:?}"
                ),
            }
            if view.editing.is_some() {
                let _ = snapshot(&view, "editor after second click");
                assert!(matches!(key(&mut view, KeyCode::Esc), ViewAction::None));
                assert!(view.editing.is_none());
            }

            // Search: typing filters across categories; Esc clears; Esc again
            // closes the view.
            for ch in "telemetry".chars() {
                assert!(matches!(
                    key(&mut view, KeyCode::Char(ch)),
                    ViewAction::None
                ));
            }
            assert_eq!(visible_row_keys(&view), vec!["telemetry"]);
            let dump = snapshot(&view, "search \"telemetry\"");
            assert!(dump.contains("telemetry"), "{w}x{h}:\n{dump}");
            assert!(matches!(key(&mut view, KeyCode::Esc), ViewAction::None));
            assert!(view.filter.is_empty());
            assert!(matches!(key(&mut view, KeyCode::Esc), ViewAction::Close));
        }
    }

    /// P1.1: a store that fails to load never becomes a default labelled
    /// saved/startup. The row is unavailable, read-only, and its lanes carry
    /// the load error; App-observed lanes (low_motion) still render.
    #[test]
    fn config_rows_report_unreadable_stores_instead_of_defaults() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = TempDir::new().expect("tempdir");
        let config_path = tmp.path().join(".deepseek").join("config.toml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            config_path.parent().unwrap().join("settings.toml"),
            "theme = [broken\n",
        )
        .unwrap();
        std::fs::write(&config_path, "approval_policy = [broken\n").unwrap();
        let _guard = crate::test_support::EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
        let mut app = create_test_app();
        app.config_path = Some(config_path.clone());
        let mut view = ConfigView::new_for_app(&app);

        let theme = view.rows.iter().find(|row| row.key == "theme").unwrap();
        assert!(
            !theme.editable,
            "an unreadable store is never written through"
        );
        assert_eq!(theme.value, en(MessageId::ConfigUnavailable));
        let error = theme
            .facts
            .store_error
            .clone()
            .expect("settings load error is preserved");
        assert!(!error.is_empty());
        let fact = view.setting_fact(theme).expect("setting fact");
        assert!(fact.saved.is_none() && fact.startup.is_none(), "{fact:?}");
        assert_eq!(
            fact.effective.as_deref(),
            Some(app.theme_id.name()),
            "the live theme is still known from App"
        );
        let summary = view.setting_detail_summary(theme);
        assert!(
            summary.contains(&en(MessageId::ConfigLaneUnavailable).replace("{error}", &error)),
            "{summary}"
        );

        let telemetry = view.rows.iter().find(|row| row.key == "telemetry").unwrap();
        assert!(!telemetry.editable);
        assert!(
            telemetry.facts.store_error.is_some(),
            "config.toml error is preserved"
        );
        let low_motion = view
            .rows
            .iter()
            .find(|row| row.key == "low_motion")
            .unwrap();
        let fact = view.setting_fact(low_motion).expect("setting fact");
        assert!(
            fact.saved.is_none(),
            "no persisted lane from a broken store"
        );
        assert!(
            fact.effective.is_some(),
            "App still supplies the live value"
        );

        // Rendered: the detail pane names the failure, never a default.
        view.focus_key("theme");
        let lines = view.setting_detail_lines(&view.rows[view.selected], 400);
        let expected = en(MessageId::ConfigLaneUnavailable).replace("{error}", &error);
        // The lane is one line (the error is folded) and may be ellipsized
        // past the pane width, so its head is the stable part.
        assert!(
            !error.contains('\n'),
            "store error is folded onto one line: {error:?}"
        );
        let head: String = expected.chars().take(60).collect();
        assert!(
            lines.iter().any(|line| line.to_string().contains(&head)),
            "saved lane carries the load error: {expected}"
        );
        let dump = render_dump(&view, 120, 32);
        let head: String = en(MessageId::ConfigUnavailable).chars().take(7).collect();
        assert!(
            dump.contains(&head),
            "unavailable value painted in the list:\n{dump}"
        );
        let unavailable_prefix = en(MessageId::ConfigLaneUnavailable)
            .split("{error}")
            .next()
            .unwrap()
            .to_string();
        assert!(
            dump.contains(unavailable_prefix.trim_end()),
            "unavailable lane painted:\n{dump}"
        );
        // Session rows are untouched by store failures.
        let provider = view.rows.iter().find(|row| row.key == "provider").unwrap();
        assert!(provider.editable && provider.facts.store_error.is_none());
    }

    /// An environment/terminal override wins the effective decision but does
    /// not repair a broken store: the overridden motion row must still report
    /// the settings.toml load failure instead of synthesizing a saved lane.
    /// (Windows CI caught this through the legacy-console probe; store truth
    /// is keyed on the row's store, not its current authority.)
    #[test]
    fn overridden_motion_row_still_reports_a_broken_settings_store() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = TempDir::new().expect("tempdir");
        let config_dir = tmp.path().join(".deepseek");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("settings.toml"), "theme = [broken\n").unwrap();
        let config_path = config_dir.join("config.toml");
        std::fs::write(&config_path, "approval_policy = [broken\n").unwrap();
        let _guard = crate::test_support::EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
        let _override = crate::test_support::EnvVarGuard::set("NO_ANIMATIONS", "1");
        let app = create_test_app();
        let view = ConfigView::new_for_app(&app);

        let row = view
            .rows
            .iter()
            .find(|row| row.key == "low_motion")
            .expect("low_motion row");
        assert_eq!(row.facts.authority, super::SettingAuthority::Environment);
        assert!(
            row.facts.store_error.is_some(),
            "the override wins the effective decision; the broken store is still reported"
        );
        let fact = view.setting_fact(row).expect("setting fact");
        assert!(fact.saved.is_none() && fact.startup.is_none(), "{fact:?}");
        assert!(
            fact.effective.is_some(),
            "App still supplies the live value"
        );
    }

    /// P1.2: when a runtime overlay forces low motion, the row's source is
    /// that override, not `settings.toml`.
    #[test]
    fn motion_rows_name_the_winning_environment_override() {
        let _lock = crate::test_support::lock_test_env();
        let _no_animations = crate::test_support::EnvVarGuard::set("NO_ANIMATIONS", "1");
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        for key in ["low_motion", "fancy_animations"] {
            let row = view.rows.iter().find(|row| row.key == key).unwrap();
            assert_eq!(
                row.facts.authority,
                super::SettingAuthority::Environment,
                "{key}"
            );
            assert_eq!(row.facts.authority_detail, Some("NO_ANIMATIONS"), "{key}");
        }
        let theme = view.rows.iter().find(|row| row.key == "theme").unwrap();
        assert_eq!(theme.facts.authority, super::SettingAuthority::UserSettings);
        view.focus_key("low_motion");
        let expected = en(MessageId::ConfigSourceEnvironment).replace("{name}", "NO_ANIMATIONS");
        // The detail pane is 39 columns wide at 120x32 and ellipsizes long
        // values, so the unbounded detail lines carry the full label and the
        // rendered pane carries its visible head.
        let lines = view.setting_detail_lines(&view.rows[view.selected], 200);
        assert!(
            lines
                .iter()
                .any(|line| line.to_string().contains(&expected)),
            "detail names the override: {expected}"
        );
        let dump = render_dump(&view, 120, 32);
        let head: String = expected.chars().take(20).collect();
        assert!(dump.contains(&head), "source names the override:\n{dump}");
    }

    /// Slice C: Edit Theme live preview — highlighting a theme row emits a
    /// session-only `ConfigUpdated` (the surface repaints immediately) while
    /// only Enter/Apply persists; Esc reverts to the opening value.
    #[test]
    fn edit_theme_highlight_previews_without_persisting_and_esc_reverts() {
        let _guard = ConfigSettingsEnvGuard::new("theme = \"terminal\"\n");
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        view.focus_key("theme");
        let key = |view: &mut ConfigView, code: KeyCode| {
            view.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
        };
        assert!(matches!(key(&mut view, KeyCode::Enter), ViewAction::None));
        assert!(
            view.editing
                .as_ref()
                .is_some_and(|edit| edit.key == "theme"),
            "Enter must open the theme editor"
        );

        // ↓ highlights shoreline: preview (persist:false), editor stays open.
        match key(&mut view, KeyCode::Down) {
            ViewAction::Emit(ViewEvent::ConfigUpdated {
                key,
                value,
                persist,
            }) => {
                assert_eq!(key, "theme");
                assert_eq!(value, "shoreline");
                assert!(!persist, "highlighting must not persist");
            }
            other => panic!("highlight must preview, got {other:?}"),
        }
        assert!(view.editing.is_some(), "preview keeps the editor open");

        // Esc reverts the live surface to the opening value, session-only.
        match key(&mut view, KeyCode::Esc) {
            ViewAction::Emit(ViewEvent::ConfigUpdated {
                key,
                value,
                persist,
            }) => {
                assert_eq!(key, "theme");
                assert_eq!(value, "terminal");
                assert!(!persist, "revert must not persist");
            }
            other => panic!("esc must revert the preview, got {other:?}"),
        }
        assert!(view.editing.is_none());
    }

    /// Slice C: Enter/Apply in Edit Theme persists the highlighted theme.
    #[test]
    fn edit_theme_enter_persists_the_highlighted_theme() {
        let _guard = ConfigSettingsEnvGuard::new("theme = \"terminal\"\n");
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        view.focus_key("theme");
        let key = |view: &mut ConfigView, code: KeyCode| {
            view.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
        };
        assert!(matches!(key(&mut view, KeyCode::Enter), ViewAction::None));
        let _ = key(&mut view, KeyCode::Down);
        match key(&mut view, KeyCode::Enter) {
            ViewAction::Emit(ViewEvent::ConfigUpdated {
                key,
                value,
                persist,
            }) => {
                assert_eq!(key, "theme");
                assert_eq!(value, "shoreline");
                assert!(persist, "Apply must persist");
            }
            other => panic!("enter must persist the highlight, got {other:?}"),
        }
        assert!(view.editing.is_none());
    }

    /// Slice C: Esc without moving the highlight previews nothing and
    /// reverts nothing.
    #[test]
    fn edit_theme_esc_without_preview_is_silent() {
        let _guard = ConfigSettingsEnvGuard::new("theme = \"terminal\"\n");
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        view.focus_key("theme");
        let key = |view: &mut ConfigView, code: KeyCode| {
            view.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
        };
        assert!(matches!(key(&mut view, KeyCode::Enter), ViewAction::None));
        assert!(
            matches!(key(&mut view, KeyCode::Esc), ViewAction::None),
            "no preview happened, so there is nothing to revert"
        );
    }

    /// Slice C: live preview is theme-only — other choice editors keep
    /// their silent highlight behavior.
    #[test]
    fn edit_choice_highlight_previews_only_the_theme_key() {
        let _guard = ConfigSettingsEnvGuard::new("");
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        view.focus_key("default_mode");
        let key = |view: &mut ConfigView, code: KeyCode| {
            view.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
        };
        assert!(matches!(key(&mut view, KeyCode::Enter), ViewAction::None));
        assert!(
            view.editing
                .as_ref()
                .is_some_and(|edit| edit.key == "default_mode"),
            "Enter must open the default_mode editor"
        );
        assert!(
            matches!(key(&mut view, KeyCode::Down), ViewAction::None),
            "non-theme highlight must stay silent"
        );
        assert!(
            matches!(key(&mut view, KeyCode::Esc), ViewAction::None),
            "no preview means no revert"
        );
    }

    /// Slice C (global hover rule): hovering an Edit Theme choice row
    /// highlights it and live-previews; hovering the same row again is
    /// silent.
    #[test]
    fn edit_theme_hover_highlights_and_previews() {
        let _guard = ConfigSettingsEnvGuard::new("theme = \"terminal\"\n");
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        view.focus_key("theme");
        view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let area = Rect::new(0, 0, 120, 32);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let hover = |view: &mut ConfigView, column: u16, row: u16| {
            view.handle_mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        // Choice index 2 is shoreline (system, terminal, shoreline, …).
        let (rect, _) = view
            .last_choice_hitboxes
            .borrow()
            .iter()
            .copied()
            .find(|(_, idx)| *idx == 2)
            .expect("rendered shoreline hitbox");
        match hover(&mut view, rect.x, rect.y) {
            ViewAction::Emit(ViewEvent::ConfigUpdated {
                key,
                value,
                persist,
            }) => {
                assert_eq!(key, "theme");
                assert_eq!(value, "shoreline");
                assert!(!persist, "hover preview must not persist");
            }
            other => panic!("hover must preview, got {other:?}"),
        }
        assert!(
            matches!(hover(&mut view, rect.x, rect.y), ViewAction::None),
            "hovering the highlighted row is silent"
        );
        assert!(
            matches!(hover(&mut view, 0, 0), ViewAction::None),
            "hovering outside every row is silent"
        );
    }

    /// P1.3: at 40 columns every category is reachable with the pointer alone
    /// — visible chips are clicked directly, hidden ones through the › and ‹
    /// overflow markers, which are themselves hitboxes.
    #[test]
    fn config_strip_categories_are_reachable_by_pointer_alone_at_40_columns() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        let area = Rect::new(0, 0, 40, 12);
        let click = |view: &mut ConfigView, rect: Rect| {
            view.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            })
        };
        let mut reached = vec![view.category];
        for target in ConfigCategory::ALL {
            // Walk to `target` using only painted hitboxes.
            for _ in 0..16 {
                if view.category == target {
                    break;
                }
                let mut buf = Buffer::empty(area);
                view.render(area, &mut buf);
                let chip = view
                    .last_rail_hitboxes
                    .borrow()
                    .iter()
                    .find(|(_, category)| *category == target)
                    .map(|(rect, _)| *rect);
                let step = |step: super::NavStep| {
                    view.last_nav_controls
                        .borrow()
                        .iter()
                        .find(|(_, s)| *s == step)
                        .map(|(rect, _)| *rect)
                };
                let rect = chip
                    .or_else(|| step(super::NavStep::Next))
                    .or_else(|| step(super::NavStep::Previous))
                    .expect("a chip or an overflow marker is always clickable");
                // The marker cells are painted, not empty.
                let cells: String = (rect.x..rect.right())
                    .map(|x| buf[(x, rect.y)].symbol().to_string())
                    .collect();
                assert!(!cells.trim().is_empty(), "{target:?}: {cells:?}");
                assert!(matches!(click(&mut view, rect), ViewAction::None));
            }
            assert_eq!(view.category, target, "pointer-only path to {target:?}");
            reached.push(target);
        }
        for category in ConfigCategory::ALL {
            assert!(reached.contains(&category));
        }
        // And back to the front by pointer only: the Appearance chip once it
        // scrolls into view, ‹ until then.
        for _ in 0..16 {
            if view.category == ConfigCategory::Appearance {
                break;
            }
            let mut buf = Buffer::empty(area);
            view.render(area, &mut buf);
            let chip = view
                .last_rail_hitboxes
                .borrow()
                .iter()
                .find(|(_, category)| *category == ConfigCategory::Appearance)
                .map(|(rect, _)| *rect);
            let rect = chip
                .or_else(|| {
                    view.last_nav_controls
                        .borrow()
                        .iter()
                        .find(|(_, s)| *s == super::NavStep::Previous)
                        .map(|(rect, _)| *rect)
                })
                .expect("the Appearance chip or ‹ is painted");
            let _ = click(&mut view, rect);
        }
        assert_eq!(view.category, ConfigCategory::Appearance);
    }

    /// P1.4: choice and text editors expose clickable Apply / Cancel controls
    /// and exact choice hitboxes at 40x12 and 80x24.
    #[test]
    fn config_editors_apply_and_cancel_by_pointer_with_exact_hitboxes() {
        let app = create_test_app();
        let click = |view: &mut ConfigView, column: u16, row: u16| {
            view.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        for (w, h) in [(40u16, 12u16), (80, 24)] {
            let area = Rect::new(0, 0, w, h);
            // Choice editor.
            let mut view = ConfigView::new_for_app(&app);
            view.focus_key("default_mode");
            assert!(matches!(
                view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                ViewAction::None
            ));
            assert!(view.editing.is_some());
            let mut buf = Buffer::empty(area);
            view.render(area, &mut buf);
            let dump = buffer_text(&buf, area);
            let controls = view.last_editor_controls.borrow().clone();
            let apply = controls
                .iter()
                .find(|(_, c)| *c == super::EditorControl::Apply)
                .map(|(r, _)| *r)
                .unwrap_or_else(|| panic!("{w}x{h} Apply control:\n{dump}"));
            let cancel = controls
                .iter()
                .find(|(_, c)| *c == super::EditorControl::Cancel)
                .map(|(r, _)| *r)
                .unwrap_or_else(|| panic!("{w}x{h} Cancel control:\n{dump}"));
            for (rect, id) in [
                (apply, MessageId::ConfigEditorApply),
                (cancel, MessageId::ConfigEditorCancel),
            ] {
                let cells: String = (rect.x..rect.right())
                    .map(|x| buf[(x, rect.y)].symbol().to_string())
                    .collect();
                assert!(cells.contains(&en(id)), "{w}x{h} {cells:?}");
                assert!(rect.bottom() <= area.bottom());
            }
            assert!(
                dump.contains(&en(MessageId::ConfigEditChooseLabel)),
                "{w}x{h}:\n{dump}"
            );
            let choices = view.last_choice_hitboxes.borrow().clone();
            let (operate_rect, operate_idx) = choices
                .iter()
                .copied()
                .find(|(_, idx)| *idx == 2)
                .unwrap_or_else(|| panic!("{w}x{h} third choice painted:\n{dump}"));
            // Clicking a choice selects it; clicking beside the controls does
            // nothing; clicking Apply emits exactly that choice.
            assert!(matches!(
                click(&mut view, operate_rect.x + 2, operate_rect.y),
                ViewAction::None
            ));
            assert_eq!(view.editing.as_ref().unwrap().selected_choice, operate_idx);
            let beside = apply.right().saturating_add(1);
            if beside < cancel.x {
                assert!(matches!(
                    click(&mut view, beside, apply.y),
                    ViewAction::None
                ));
                assert!(view.editing.is_some(), "{w}x{h} gap click is inert");
            }
            match click(&mut view, apply.x + 1, apply.y) {
                ViewAction::Emit(ViewEvent::ConfigUpdated {
                    key,
                    value,
                    persist,
                }) => {
                    assert_eq!(key, "default_mode");
                    assert_eq!(value, "operate");
                    assert!(persist);
                }
                other => panic!("{w}x{h} Apply click should emit: {other:?}"),
            }
            assert!(view.editing.is_none());

            // Cancel by pointer.
            view.focus_key("default_mode");
            let _ = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            let mut buf = Buffer::empty(area);
            view.render(area, &mut buf);
            let cancel = view
                .last_editor_controls
                .borrow()
                .iter()
                .find(|(_, c)| *c == super::EditorControl::Cancel)
                .map(|(r, _)| *r)
                .unwrap();
            assert!(matches!(
                click(&mut view, cancel.x, cancel.y),
                ViewAction::None
            ));
            assert!(view.editing.is_none(), "{w}x{h} Cancel leaves the editor");
            assert_eq!(
                view.status.as_deref(),
                Some(en(MessageId::ConfigEditCancelled).as_str())
            );

            // Text editor: type, then Apply by pointer.
            view.focus_key("thinking_preview_lines");
            let _ = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            for ch in "77".chars() {
                let _ = view.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
            }
            let mut buf = Buffer::empty(area);
            view.render(area, &mut buf);
            let dump = buffer_text(&buf, area);
            let apply = view
                .last_editor_controls
                .borrow()
                .iter()
                .find(|(_, c)| *c == super::EditorControl::Apply)
                .map(|(r, _)| *r)
                .unwrap_or_else(|| panic!("{w}x{h} text editor Apply:\n{dump}"));
            assert!(
                dump.contains(&en(MessageId::ConfigEditNewLabel).trim_end().to_string()),
                "{w}x{h} value line stays visible above the controls:\n{dump}"
            );
            match click(&mut view, apply.x, apply.y) {
                ViewAction::Emit(ViewEvent::ConfigUpdated { key, value, .. }) => {
                    assert_eq!(key, "thinking_preview_lines");
                    assert_eq!(value, "77");
                }
                other => panic!("{w}x{h} text Apply should emit: {other:?}"),
            }
        }
    }

    /// P1.5: second-click activation is disarmed by every keyboard step, so no
    /// single click after navigation can mutate anything.
    #[test]
    fn config_second_click_arming_resets_on_every_keyboard_step() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        view.category = ConfigCategory::Motion;
        view.select_first_visible_row();
        let area = Rect::new(0, 0, 80, 24);
        let click_row = |view: &mut ConfigView, key: &str| -> ViewAction {
            let mut buf = Buffer::empty(area);
            view.render(area, &mut buf);
            let rect = view
                .last_row_hitboxes
                .borrow()
                .iter()
                .find(|(_, idx)| view.rows[*idx].key == key)
                .map(|(rect, _)| *rect)
                .unwrap_or_else(|| panic!("{key} row painted"));
            view.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x + 1,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            })
        };
        let key = |view: &mut ConfigView, code: KeyCode| {
            view.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
        };
        let sequences: Vec<Vec<KeyCode>> = vec![
            vec![KeyCode::Down, KeyCode::Up],
            vec![KeyCode::Right, KeyCode::Left],
            vec![KeyCode::Tab, KeyCode::BackTab],
            vec![KeyCode::Char('x'), KeyCode::Backspace],
            vec![KeyCode::PageDown, KeyCode::PageUp],
        ];
        for sequence in sequences {
            // The previous iteration's checking click left the row armed; a
            // keyboard step (like any real navigation) disarms it first.
            let _ = key(&mut view, KeyCode::Up);
            assert!(matches!(
                click_row(&mut view, "low_motion"),
                ViewAction::None
            ));
            for code in &sequence {
                let _ = key(&mut view, *code);
            }
            assert_eq!(view.category, ConfigCategory::Motion, "{sequence:?}");
            let action = click_row(&mut view, "low_motion");
            assert!(
                matches!(action, ViewAction::None),
                "{sequence:?} then one click must not mutate: {action:?}"
            );
            assert!(view.editing.is_none(), "{sequence:?}");
            assert_eq!(view.rows[view.selected].key, "low_motion");
        }
        // Control: two consecutive clicks with nothing in between activate
        // (the previous check click is disarmed by a keyboard step first).
        let _ = key(&mut view, KeyCode::Up);
        assert!(matches!(
            click_row(&mut view, "low_motion"),
            ViewAction::None
        ));
        assert!(matches!(
            click_row(&mut view, "low_motion"),
            ViewAction::Emit(ViewEvent::ConfigUpdated { .. })
        ));
        // A rebuilt focus (the host re-renders after applying) is disarmed
        // even though the emitting click left the row armed.
        view.focus_key("low_motion");
        assert!(matches!(
            click_row(&mut view, "low_motion"),
            ViewAction::None
        ));
    }

    /// P1.6: the reachable editor surface renders from the packs — search
    /// label, choose label, choice labels and details, footer, controls, and
    /// hints — with no English fallbacks in zh-Hans.
    #[test]
    fn config_editor_surface_is_localized() {
        let mut app = create_test_app();
        app.ui_locale = Locale::ZhHans;
        let mut view = ConfigView::new_for_app(&app);
        let zh = |id: MessageId| tr(Locale::ZhHans, id).into_owned();
        let spaced = |text: &str| -> String {
            text.chars()
                .map(|ch| {
                    if UnicodeWidthStr::width(ch.to_string().as_str()) > 1 {
                        format!("{ch} ")
                    } else {
                        ch.to_string()
                    }
                })
                .collect()
        };
        let dump = render_dump(&view, 80, 24);
        assert!(
            dump.contains(spaced(&zh(MessageId::ConfigSearchLabel)).trim()),
            "search label:\n{dump}"
        );
        view.focus_key("default_mode");
        let _ = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let dump = render_dump(&view, 80, 24);
        for id in [
            MessageId::ConfigEditChooseLabel,
            MessageId::ConfigChoiceModeAct,
            MessageId::ConfigChoiceModePlan,
            MessageId::ConfigChoiceModeOperate,
            MessageId::ConfigChoiceDetailModeAgent,
            MessageId::ConfigEditorApply,
            MessageId::ConfigEditorCancel,
        ] {
            let text = zh(id);
            assert!(
                dump.contains(spaced(&text).trim_end()),
                "localized {id:?} = {text}:\n{dump}"
            );
        }
        assert!(
            dump.contains(spaced(&zh(MessageId::ConfigChoiceFooter)).trim())
                || dump.contains(spaced(&zh(MessageId::ConfigChoiceFooterCompact)).trim()),
            "localized choice footer:\n{dump}"
        );
        for english in ["Choose:", "Full Access", "Apply", "Cancel", "Search:"] {
            assert!(!dump.contains(english), "English leaked: {english}\n{dump}");
        }
        let _ = view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        view.focus_key("low_motion");
        let dump = render_dump(&view, 120, 32);
        let hint = zh(MessageId::ConfigHintLowMotion);
        assert!(
            dump.contains(spaced(&hint).trim_end()),
            "localized hint in the detail pane:\n{dump}"
        );
    }

    #[test]
    fn config_shell_renders_localized_chrome_without_missing_markers() {
        let mut app = create_test_app();
        app.ui_locale = Locale::ZhHans;
        let view = ConfigView::new_for_app(&app);
        for (w, h) in [(80u16, 24u16), (120, 32)] {
            let dump = render_dump(&view, w, h);
            assert!(!dump.contains("MISSING"), "{w}x{h}:\n{dump}");
            let section = tr(Locale::ZhHans, MessageId::ConfigSectionDisplay);
            let spaced: String = section.chars().map(|ch| format!("{ch} ")).collect();
            assert!(
                dump.contains(spaced.trim_end()),
                "{w}x{h} localized section label {section}:\n{dump}"
            );
            // The new shell chrome renders from the packs too: the active
            // category chip/rail row and, when wide, the detail fact labels.
            let category = tr(Locale::ZhHans, MessageId::ConfigCategoryAppearance);
            let spaced: String = category.chars().map(|ch| format!("{ch} ")).collect();
            assert!(
                dump.contains(spaced.trim_end()),
                "{w}x{h} localized category {category}:\n{dump}"
            );
            if w >= 100 {
                for id in [MessageId::ConfigFactCurrent, MessageId::ConfigFactApply] {
                    let label = tr(Locale::ZhHans, id);
                    let spaced: String = label.chars().map(|ch| format!("{ch} ")).collect();
                    assert!(
                        dump.contains(spaced.trim_end()),
                        "{w}x{h} localized fact label {label}:\n{dump}"
                    );
                }
                let unobserved = tr(Locale::ZhHans, MessageId::ConfigLaneUnobserved);
                let spaced: String = unobserved.chars().map(|ch| format!("{ch} ")).collect();
                assert!(
                    dump.contains(spaced.trim_end()),
                    "{w}x{h} localized unobserved lane:\n{dump}"
                );
            }
            let scope = tr(Locale::ZhHans, MessageId::ConfigScopeSaved);
            let spaced: String = scope.chars().map(|ch| format!("{ch} ")).collect();
            assert!(
                dump.contains(spaced.trim_end()) || dump.contains(scope.as_ref()),
                "{w}x{h} localized scope badge {scope}:\n{dump}"
            );
        }
    }

    #[test]
    fn config_view_mcp_action_rows_run_existing_commands() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        for (key, command) in [
            ("mcp_open", "/mcp"),
            ("mcp_reconnect", "/mcp reload"),
            ("mcp_diagnose", "/mcp validate"),
            ("plugins_open", "/plugin"),
        ] {
            view.focus_key(key);
            let action = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            match action {
                ViewAction::Emit(ViewEvent::CommandPaletteSelected {
                    action: CommandPaletteAction::ExecuteCommand { command: emitted },
                }) => {
                    assert_eq!(emitted, command, "{key}");
                    assert!(!emitted.contains("/mcp auth"), "{key}");
                }
                other => panic!("{key} should run {command}, got {other:?}"),
            }
        }
    }

    #[test]
    fn config_view_bottom_hint_semantically_truncates_at_narrow_width() {
        // The dense bottom status line must truncate on a word boundary with an
        // ellipsis instead of leaving a mid-word fragment clipped by the
        // terminal (#3987).
        let mut app = create_test_app();
        app.ui_locale = Locale::En;
        let mut view = ConfigView::new_for_app(&app);
        view.status = Some(
            "CFGSTATUS persisted the configuration override to disk successfully \
             without clipping the trailing MARKEREND status text"
                .to_string(),
        );

        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        let rows: Vec<String> = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect();

        // No rendered row may overflow the available columns.
        for (idx, row) in rows.iter().enumerate() {
            assert!(
                crate::tui::ui_text::text_display_width(row) <= usize::from(area.width),
                "line {idx} overflows: {row:?}"
            );
        }

        let status_line = rows
            .iter()
            .find(|row| row.contains("CFGSTATUS"))
            .expect("bottom status hint should be rendered");
        assert!(
            status_line.contains('…'),
            "status should be truncated with an ellipsis: {status_line:?}"
        );
        assert!(
            !status_line.contains("MARKEREND"),
            "truncated status must drop trailing text: {status_line:?}"
        );
    }

    #[test]
    fn config_view_typing_replaces_on_first_char() {
        let app = create_test_app();
        let mut view = ConfigView::new_for_app(&app);
        view.focus_key("background_color");

        let _ = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let edit = view.editing.as_ref().expect("editing should be active");
        assert!(edit.select_all, "editor should start with select-all");

        let _ = view.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let edit = view.editing.as_ref().expect("editing should remain active");
        assert_eq!(edit.buffer.iter().collect::<String>(), "x");
    }

    #[test]
    fn config_view_escape_cancels_editing() {
        let mut app = create_test_app();
        app.ui_locale = Locale::En;
        let mut view = ConfigView::new_for_app(&app);
        view.focus_key("thinking_preview_lines");
        let _ = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(view.editing.is_some());

        let cancel = view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(cancel, ViewAction::None));
        assert!(view.editing.is_none());
        assert_eq!(
            view.status.as_deref(),
            Some(&*tr(Locale::En, MessageId::ConfigEditCancelled))
        );
    }

    /// A modal that doesn't override `handle_paste` must report
    /// "not consumed" so the host can fall through to the composer.
    /// Regression: views/mod.rs previously inverted the boolean, swallowing
    /// every Cmd-V while any modal was on top.
    #[test]
    fn default_modal_does_not_consume_paste() {
        let mut stack = ViewStack::new();
        stack.push(HelpView::new_for_locale(codewhale_localization::Locale::En));
        assert!(!stack.handle_paste("hello"));
        assert_eq!(stack.top_kind(), Some(ModalKind::Help));
    }

    struct BareModal;

    impl ModalView for BareModal {
        fn kind(&self) -> ModalKind {
            ModalKind::ContextMenu
        }

        fn handle_key(&mut self, _key: KeyEvent) -> ViewAction {
            ViewAction::None
        }

        fn render(&self, area: Rect, buf: &mut Buffer) {
            let x = area.x + area.width / 2;
            let y = area.y + area.height / 2;
            buf[(x, y)]
                .set_symbol("M")
                .set_style(Style::default().fg(Color::White).bg(Color::Red));
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    #[test]
    fn view_stack_paints_opaque_backdrop_before_modal() {
        let area = Rect::new(0, 0, 24, 8);
        let modal_x = area.x + area.width / 2;
        let modal_y = area.y + area.height / 2;
        let mut buf = Buffer::empty(area);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)]
                    .set_symbol("X")
                    .set_style(Style::default().fg(Color::Red).bg(Color::Blue));
            }
        }

        let mut stack = ViewStack::new();
        stack.push(BareModal);
        stack.render(area, &mut buf);

        assert_eq!(buf[(modal_x, modal_y)].symbol(), "M");
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if x == modal_x && y == modal_y {
                    continue;
                }
                let cell = &buf[(x, y)];
                assert_eq!(
                    cell.symbol(),
                    " ",
                    "stale glyph at ({x},{y}) must be cleared"
                );
                assert_eq!(
                    cell.bg,
                    palette::WHALE_BG,
                    "backdrop at ({x},{y}) must be opaque"
                );
            }
        }
    }

    #[test]
    fn view_stack_masks_links_behind_opaque_modals() {
        let area = Rect::new(0, 0, 24, 8);
        crate::tui::osc8::set_frame_links(vec![crate::tui::osc8::LinkRegion {
            row: 3,
            col_start: 2,
            col_end: 18,
            target: "https://example.invalid/under-modal".to_string(),
        }]);
        let mut stack = ViewStack::new();
        stack.push(BareModal);
        stack.render(area, &mut Buffer::empty(area));
        assert!(crate::tui::osc8::take_frame_links().is_empty());
    }

    fn buffer_text(buf: &Buffer, area: Rect) -> String {
        let mut out = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn buffer_row_text(buf: &Buffer, area: Rect, y: u16) -> String {
        (area.left()..area.right())
            .map(|x| buf[(x, y)].symbol())
            .collect()
    }

    /// 40x12 regression: the compact tier must surrender secondary chrome
    /// (in-body title, column captions, separator) before it surrenders the
    /// settings rows, and the wrapped footer height must come out of the
    /// table budget instead of silently clipping rows.
    #[test]
    fn config_compact_theme_category_and_footer_remain_legible() {
        let _guard = ConfigSettingsEnvGuard::new("theme = \"shoreline\"\n");
        let mut view = create_config_view(Locale::En);
        view.focus_key("theme");
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let dump = buffer_text(&buf, area);
        let theme_rect = view
            .last_row_hitboxes
            .borrow()
            .iter()
            .find(|(_, idx)| view.rows[*idx].key == "theme")
            .unwrap()
            .0;
        assert!(
            buffer_row_text(&buf, area, theme_rect.y).contains("shoreline"),
            "{dump}"
        );
        assert!(dump.contains("Appearance  1/7"), "{dump}");
        let footer = dump
            .lines()
            .find(|line| line.contains("Enter") && line.contains("Esc"))
            .expect("all compact action hints share one line");
        assert!(footer.contains("Tab"));
        for category in ConfigCategory::ALL {
            view.category = category;
            view.select_first_visible_row();
            view.render(area, &mut buf);
            let dump = buffer_text(&buf, area);
            assert!(
                dump.contains(category.label(Locale::En).as_ref()),
                "{category:?}: {dump}"
            );
            assert_eq!(view.last_nav_controls.borrow().len(), 2);
        }
    }

    #[test]
    fn config_sandbox_search_opens_observed_status_without_editing_policy() {
        let mut view = create_config_view(Locale::En);
        for query in [
            "sandbox",
            "filesystem",
            "unenforced",
            "bubblewrap",
            "doctor",
        ] {
            view.restore_filter(query.to_string());
            let matches = view.matching_row_indices();
            let index = *matches
                .iter()
                .find(|&&idx| view.rows[idx].key == "sandbox_details")
                .expect("sandbox explanation discoverable");
            view.selected = index;
            let row = &view.rows[index];
            assert_eq!(row.facts.kind, ConfigRowKind::Action);
            assert_eq!(row.facts.store, SettingStore::None);
            assert!(
                matches!(view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                ViewAction::Emit(ViewEvent::ExecutePanelCommand { command, pager_title: Some(_) }) if command == "/status")
            );
            assert!(view.editing.is_none());
        }
        assert!(!view.rows.iter().any(|row| row.key == "sandbox_mode"));
    }

    #[test]
    fn config_view_compact_heights_always_show_a_selectable_setting() {
        let mut view = create_config_view(Locale::En);
        for (width, height, label) in [(40u16, 12u16, "40x12"), (60, 16, "60x16")] {
            let area = Rect::new(0, 0, width, height);
            let mut buf = Buffer::empty(area);

            view.render(area, &mut buf);

            let dump = buffer_text(&buf, area);
            let (selected_y, selected_idx) = {
                let hitboxes = view.last_row_hitboxes.borrow();
                assert!(
                    !hitboxes.is_empty(),
                    "{label} should register selectable setting hitboxes:\n{dump}"
                );
                hitboxes
                    .iter()
                    .find(|(_, idx)| *idx == view.selected)
                    .copied()
                    .unwrap_or_else(|| {
                        panic!("{label} selected setting should be rendered:\n{dump}")
                    })
            };
            let row = buffer_row_text(&buf, area, selected_y.y);
            let row_label = config_label_for_key(&view.rows[selected_idx].key);
            let prefix: String = row_label.chars().take(8).collect();
            assert!(
                row.contains(&prefix),
                "{label} hitbox row should contain the selected setting ({row_label:?}); got {row:?}"
            );
            assert!(
                dump.contains("Search:"),
                "{label} should keep the search affordance:\n{dump}"
            );
        }

        // The selection anchor must hold while navigating across sections at
        // the smallest supported size.
        let area = Rect::new(0, 0, 40, 12);
        for step in 0..12 {
            view.move_selection(1);
            let mut buf = Buffer::empty(area);
            view.render(area, &mut buf);
            let rendered = view
                .last_row_hitboxes
                .borrow()
                .iter()
                .any(|(_, idx)| *idx == view.selected);
            assert!(
                rendered,
                "selected setting fell out of the 40x12 window after {} moves",
                step + 1
            );
        }
    }

    /// 40x12 regression: the edit surface must keep the editable value line
    /// (and its hint) above the wrapped footer.
    #[test]
    fn config_view_compact_edit_surface_keeps_value_line_visible() {
        let mut view = create_config_view(Locale::En);
        view.focus_key("approval_mode");
        view.start_edit();
        assert!(view.editing.is_some(), "approval_mode should be editable");
        assert_eq!(
            view.editing
                .as_ref()
                .and_then(|edit| edit.choices.as_ref())
                .expect("session permission choices"),
            &vec![
                "ask".to_string(),
                "auto-review".to_string(),
                "full-access".to_string(),
            ]
        );
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);

        view.render(area, &mut buf);

        let dump = buffer_text(&buf, area);
        assert!(
            dump.contains("Choose:") && dump.contains("Full Access"),
            "the choice list must stay visible at 40x12:\n{dump}"
        );
    }
}
