//! Host-reported pending input above the composer.
//!
//! The host owns the queue, the turn and every mutation. This component owns
//! only what a person can see: caller items with an explicit status word and
//! mark, attached context with an explicit state, a compact queue summary and
//! the action metadata a host may dispatch. The kit never advances a status,
//! so nothing here can claim an item was sent, delivered or verified.

use std::borrow::Cow;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use unicode_segmentation::UnicodeSegmentation;

use crate::{Paint, Role, Theme, glyphs, text};

/// What the host reports about one pending item. The word and the mark are
/// the only claims; the kit never moves an item between states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PendingInputStatus {
    /// Typed input captured for a later turn.
    Queued,
    /// A note the host will deliver into the running turn.
    Steering,
    /// The person is editing this item now.
    Editing,
    /// The host held this item.
    Paused,
    /// The host reports delivery is already under way.
    InFlight,
}

const ACTIONS_SEND_EDIT_DROP: [PendingInputAction; 3] = [
    PendingInputAction::SendNow,
    PendingInputAction::Edit,
    PendingInputAction::Drop,
];
const ACTIONS_EDIT_DROP: [PendingInputAction; 2] =
    [PendingInputAction::Edit, PendingInputAction::Drop];
const ACTIONS_SEND_DROP: [PendingInputAction; 2] =
    [PendingInputAction::SendNow, PendingInputAction::Drop];
const ACTIONS_NONE: [PendingInputAction; 0] = [];

impl PendingInputStatus {
    pub const ALL: [Self; 5] = [
        Self::Queued,
        Self::Steering,
        Self::Editing,
        Self::Paused,
        Self::InFlight,
    ];

    /// The word shown for this status.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Steering => "steering",
            Self::Editing => "editing",
            Self::Paused => "paused",
            Self::InFlight => "in flight",
        }
    }

    #[must_use]
    pub const fn role(self) -> Role {
        match self {
            Self::Queued => Role::Accent,
            Self::Steering => Role::Live,
            Self::Editing => Role::Attention,
            Self::Paused => Role::Attention,
            Self::InFlight => Role::Muted,
        }
    }

    /// The mark for this terminal; `theme.ascii()` never widens the line.
    #[must_use]
    pub fn glyph(self, theme: &Theme) -> &'static str {
        let ascii = theme.ascii();
        match self {
            Self::Queued => {
                if ascii {
                    "o"
                } else {
                    glyphs::AVAILABLE
                }
            }
            Self::Steering => {
                if ascii {
                    "+"
                } else {
                    "↳"
                }
            }
            Self::Editing => {
                if ascii {
                    "e"
                } else {
                    "✎"
                }
            }
            Self::Paused => {
                if ascii {
                    "="
                } else {
                    glyphs::PAUSED
                }
            }
            Self::InFlight => {
                if ascii {
                    ">"
                } else {
                    glyphs::CURRENT
                }
            }
        }
    }

    /// The actions a host may dispatch for an item in this status. The kit
    /// emits them as metadata only; it never performs one.
    #[must_use]
    pub const fn actions(self) -> &'static [PendingInputAction] {
        match self {
            Self::Queued | Self::Paused => &ACTIONS_SEND_EDIT_DROP,
            Self::Steering => &ACTIONS_EDIT_DROP,
            Self::Editing => &ACTIONS_SEND_DROP,
            Self::InFlight => &ACTIONS_NONE,
        }
    }
}

/// One action a host may offer beside a pending item. The kit paints the
/// word and hands the choice back; mutation stays with the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PendingInputAction {
    SendNow,
    Edit,
    Drop,
}

impl PendingInputAction {
    pub const ALL: [Self; 3] = [Self::SendNow, Self::Edit, Self::Drop];

    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::SendNow => "Send now",
            Self::Edit => "Edit",
            Self::Drop => "Drop",
        }
    }

    #[must_use]
    pub const fn role(self) -> Role {
        match self {
            Self::SendNow => Role::Accent,
            Self::Edit => Role::Foreground,
            Self::Drop => Role::Danger,
        }
    }
}

/// One queued, steering, editing, paused or in-flight item.
///
/// `content` is display text; it may hold newlines and controls, and the
/// component sanitizes and flattens it. `id` is the stable caller key used
/// for selection and for `actions_for`.
#[derive(Clone, Debug)]
pub struct PendingInputItem<'a> {
    pub id: Cow<'a, str>,
    pub label: Cow<'a, str>,
    pub content: Cow<'a, str>,
    pub status: PendingInputStatus,
    pub status_word: Option<Cow<'a, str>>,
}

impl<'a> PendingInputItem<'a> {
    #[must_use]
    pub fn new(
        id: impl Into<Cow<'a, str>>,
        content: impl Into<Cow<'a, str>>,
        status: PendingInputStatus,
    ) -> Self {
        Self {
            id: id.into(),
            label: Cow::Borrowed(""),
            content: content.into(),
            status,
            status_word: None,
        }
    }

    #[must_use]
    pub fn label(mut self, label: impl Into<Cow<'a, str>>) -> Self {
        self.label = label.into();
        self
    }

    /// A caller-localized status word, replacing the default one.
    #[must_use]
    pub fn status_word(mut self, word: impl Into<Cow<'a, str>>) -> Self {
        self.status_word = Some(word.into());
        self
    }

    #[must_use]
    pub fn word(&self) -> &str {
        self.status_word
            .as_deref()
            .unwrap_or_else(|| self.status.word())
    }

    #[must_use]
    pub fn actions(&self) -> &'static [PendingInputAction] {
        self.status.actions()
    }
}

/// What the host reports about one attached context item. `Unconfirmed` is
/// deliberately not `Included`: unconfirmed context is never shown as sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContextPreviewState {
    /// Known to be included with the message.
    Included,
    /// Attached, but not confirmed by the host.
    Unconfirmed,
    /// Attached and offered for removal.
    Removable,
}

impl ContextPreviewState {
    pub const ALL: [Self; 3] = [Self::Included, Self::Unconfirmed, Self::Removable];

    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Included => "included",
            Self::Unconfirmed => "unconfirmed",
            Self::Removable => "removable",
        }
    }

    #[must_use]
    pub const fn role(self) -> Role {
        match self {
            Self::Included => Role::Live,
            Self::Unconfirmed => Role::Attention,
            Self::Removable => Role::Muted,
        }
    }

    #[must_use]
    pub fn glyph(self, theme: &Theme) -> &'static str {
        let ascii = theme.ascii();
        match self {
            Self::Included => {
                if ascii {
                    "+"
                } else {
                    glyphs::DONE
                }
            }
            Self::Unconfirmed => {
                if ascii {
                    "?"
                } else {
                    glyphs::ATTENTION
                }
            }
            Self::Removable => {
                if ascii {
                    "-"
                } else {
                    glyphs::NEUTRAL
                }
            }
        }
    }
}

/// One attached context item, with its own reported state.
#[derive(Clone, Debug)]
pub struct ContextPreviewItem<'a> {
    pub id: Cow<'a, str>,
    pub label: Cow<'a, str>,
    pub detail: Option<Cow<'a, str>>,
    pub state: ContextPreviewState,
}

impl<'a> ContextPreviewItem<'a> {
    #[must_use]
    pub fn new(
        id: impl Into<Cow<'a, str>>,
        label: impl Into<Cow<'a, str>>,
        state: ContextPreviewState,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            detail: None,
            state,
        }
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<Cow<'a, str>>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn word(&self) -> &str {
        self.state.word()
    }
}

#[derive(Clone)]
struct PaintedRow {
    line: Line<'static>,
    selected: bool,
}

// One geometry and paint authority for the generic list and native card.
struct PendingPlan {
    width: u16,
    max_rows: u16,
    rows: Vec<PaintedRow>,
    one_row: Option<PaintedRow>,
    tail: Option<PendingTail>,
}

struct PendingTail {
    hint: String,
    omitted: String,
    hint_style: Style,
    omitted_style: Style,
}

impl PendingPlan {
    fn new(width: u16, max_rows: u16) -> Self {
        Self {
            width,
            max_rows,
            rows: Vec::new(),
            one_row: None,
            tail: None,
        }
    }
    fn push(&mut self, line: Line<'static>) {
        self.rows.push(PaintedRow {
            line,
            selected: false,
        });
    }
    fn height(&self) -> u16 {
        if self.width < 4 {
            return 0;
        }
        u16::try_from(
            self.rows
                .len()
                .saturating_add(usize::from(self.tail.is_some())),
        )
        .unwrap_or(u16::MAX)
        .min(self.max_rows)
    }
    fn shown(&self, height: u16) -> Vec<PaintedRow> {
        let height = height.min(self.max_rows);
        if self.width < 4 || height == 0 || self.rows.is_empty() {
            return Vec::new();
        }
        if height == 1
            && let Some(row) = &self.one_row
        {
            return vec![row.clone()];
        }
        let keep = usize::from(height).saturating_sub(usize::from(self.tail.is_some()));
        let mut out: Vec<_> = self.rows.iter().take(keep).cloned().collect();
        if let Some(tail) = &self.tail {
            let hidden = self.rows.len().saturating_sub(keep);
            let mut spans = Vec::new();
            if hidden > 0 {
                spans.push(Span::styled(
                    format!("+{hidden} {}  ", tail.omitted),
                    tail.omitted_style,
                ));
            }
            spans.push(Span::styled(tail.hint.clone(), tail.hint_style));
            out.push(PaintedRow {
                line: Line::from(spans),
                selected: false,
            });
        }
        out
    }
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        if area.is_empty() {
            return;
        }
        let shown = self.shown(area.height);
        for (index, painted) in shown.iter().enumerate() {
            let rect = Rect::new(
                area.x,
                area.y
                    .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                area.width,
                1,
            );
            if painted.selected {
                buf.set_style(rect, theme.bg(Role::Selected));
            }
        }
        // Keep native Ratatui glyph painting, including caller-owned styles
        // on wide-character continuation cells, in one shared paint path.
        Paragraph::new(shown.into_iter().map(|row| row.line).collect::<Vec<_>>()).render(area, buf);
    }
}

/// Localized copy for the native composer preview. Templates use `{count}`
/// and `{number}`; key labels are resolved by the host before being supplied.
#[derive(Clone, Debug)]
pub struct PendingCardWords<'a> {
    pub context_header: Cow<'a, str>,
    pub inputs_header: Cow<'a, str>,
    pub sending_prefix: Cow<'a, str>,
    pub editing_prefix: Cow<'a, str>,
    pub editing_restore: Cow<'a, str>,
    pub queued_prefix: Cow<'a, str>,
    pub queued_one_prefix: Cow<'a, str>,
    pub queued_many_prefix: Cow<'a, str>,
    pub queued_controls: Cow<'a, str>,
    pub compact_controls: Cow<'a, str>,
    pub removable: Cow<'a, str>,
    pub selected_remove: Cow<'a, str>,
}

impl Default for PendingCardWords<'_> {
    fn default() -> Self {
        Self {
            context_header: "Context for next send".into(),
            inputs_header: "Pending inputs".into(),
            sending_prefix: "Sending into this turn: ".into(),
            editing_prefix: "Editing follow-up: ".into(),
            editing_restore: "Esc restores the queued follow-up".into(),
            queued_prefix: "Queued follow-up #{number}: ".into(),
            queued_one_prefix: "Queued #1: ".into(),
            queued_many_prefix: "Queued {count}; next: ".into(),
            queued_controls: "Enter send now · ↑ edit".into(),
            compact_controls: "Enter send now · ↑ edit · /queue drop 1".into(),
            removable: "removable".into(),
            selected_remove: "Backspace/Delete removes".into(),
        }
    }
}

/// Inclusion, removability and keyboard selection are independent host facts.
/// None of them advances a queue or claims delivery.
#[derive(Clone, Debug)]
pub struct PendingCardContext<'a> {
    pub kind: Cow<'a, str>,
    pub label: Cow<'a, str>,
    pub detail: Option<Cow<'a, str>>,
    pub included: bool,
    pub removable: bool,
    pub selected: bool,
}

/// The native card's five concrete style slots. Defaults come from `Theme`;
/// a host retaining its palette grammar may provide these exact styles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingCardStyles {
    pub input: Style,
    pub warning: Style,
    pub context_muted: Style,
    pub context_label: Style,
    pub selected: Style,
}

impl PendingCardStyles {
    #[must_use]
    pub fn for_theme(theme: &Theme) -> Self {
        Self {
            input: theme.fg(Role::Dim).add_modifier(Modifier::DIM),
            warning: theme.fg(Role::Attention),
            context_muted: theme.fg(Role::Muted),
            context_label: theme.fg(Role::Foreground),
            selected: theme.fg(Role::Foreground).patch(theme.bg(Role::Selected)),
        }
    }
}

/// Native pending input above a composer, over caller-owned facts and copy.
/// The same measured row plan as `PendingInputPreview` owns height, clipping,
/// the one-row action fallback and painting. There is no queue or request state.
#[derive(Clone, Debug)]
pub struct PendingCard<'a> {
    pub context: Vec<PendingCardContext<'a>>,
    pub sending: Vec<Cow<'a, str>>,
    pub queued: Vec<Cow<'a, str>>,
    pub editing: Option<Cow<'a, str>>,
    /// Already-localized notices that precede context and input, including
    /// hidden child requests. A notice always outranks a queue action fallback.
    pub priority_rows: Vec<Cow<'a, str>>,
    pub words: PendingCardWords<'a>,
    pub styles: Option<PendingCardStyles>,
}

impl<'a> PendingCard<'a> {
    #[must_use]
    pub fn new(words: PendingCardWords<'a>) -> Self {
        Self {
            context: Vec::new(),
            sending: Vec::new(),
            queued: Vec::new(),
            editing: None,
            priority_rows: Vec::new(),
            words,
            styles: None,
        }
    }

    fn plan(&self, width: u16, theme: &Theme) -> PendingPlan {
        let mut plan = PendingPlan::new(width, u16::MAX);
        if width < 4 {
            return plan;
        }
        let styles = self
            .styles
            .unwrap_or_else(|| PendingCardStyles::for_theme(theme));
        for notice in &self.priority_rows {
            plan.push(Line::from(Span::styled(
                card_clip(&card_safe(notice), usize::from(width), theme.ascii()),
                styles.warning,
            )));
        }
        let queued_only = self.context.is_empty()
            && self.sending.is_empty()
            && self.editing.is_none()
            && !self.queued.is_empty();
        if queued_only {
            let prefix = if self.queued.len() == 1 {
                card_copy(&self.words.queued_one_prefix, theme)
            } else {
                card_copy(&self.words.queued_many_prefix, theme)
                    .replace("{count}", &self.queued.len().to_string())
            };
            let next = card_safe(&self.queued[0].replace('\n', " "));
            plan.push(Line::from(Span::styled(
                card_clip(
                    &format!("{prefix}{next}"),
                    usize::from(width),
                    theme.ascii(),
                ),
                styles.input.add_modifier(Modifier::ITALIC),
            )));
            let controls = Line::from(Span::styled(
                card_clip(
                    &card_copy(&self.words.compact_controls, theme),
                    usize::from(width),
                    theme.ascii(),
                ),
                styles.input,
            ));
            if self.priority_rows.is_empty() {
                plan.one_row = Some(PaintedRow {
                    line: controls.clone(),
                    selected: false,
                });
            }
            plan.push(controls);
            return plan;
        }
        if !self.context.is_empty() {
            plan.push(card_header(&self.words.context_header, theme));
            for item in &self.context {
                let prefix_style = if item.selected {
                    styles.selected.add_modifier(Modifier::BOLD)
                } else if item.included {
                    styles.context_muted
                } else {
                    styles.warning
                };
                let body_style = if item.selected {
                    styles.selected
                } else if item.included {
                    styles.context_label
                } else {
                    styles.context_muted
                };
                let mut body = format!("[{}] {}", card_safe(&item.kind), card_safe(&item.label));
                let separator = if theme.ascii() { " . " } else { " · " };
                if let Some(detail) = item
                    .detail
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                {
                    body.push_str(separator);
                    body.push_str(&card_safe(detail));
                }
                let action = if item.selected {
                    Some(&self.words.selected_remove)
                } else if item.removable {
                    Some(&self.words.removable)
                } else {
                    None
                };
                if let Some(action) = action {
                    body.push_str(separator);
                    body.push_str(&card_copy(action, theme));
                }
                for (index, segment) in
                    card_wrap(&body, usize::from(width.saturating_sub(4).max(1)))
                        .into_iter()
                        .enumerate()
                {
                    let prefix = if index > 0 {
                        "    "
                    } else if item.selected {
                        if theme.ascii() { "  > " } else { "  ▸ " }
                    } else if theme.ascii() {
                        "  + "
                    } else {
                        "  ↳ "
                    };
                    plan.push(Line::from(vec![
                        Span::styled(prefix, prefix_style),
                        Span::styled(segment, body_style),
                    ]));
                }
            }
        }
        let has_inputs =
            !self.sending.is_empty() || !self.queued.is_empty() || self.editing.is_some();
        if has_inputs {
            if !self.context.is_empty() {
                plan.push(Line::from(""));
            }
            plan.push(card_header(&self.words.inputs_header, theme));
            for value in &self.sending {
                card_item(
                    &mut plan,
                    value,
                    &card_copy(&self.words.sending_prefix, theme),
                    styles.input,
                    theme,
                );
            }
            if let Some(value) = &self.editing {
                card_item(
                    &mut plan,
                    value,
                    &card_copy(&self.words.editing_prefix, theme),
                    styles.input.add_modifier(Modifier::ITALIC),
                    theme,
                );
                plan.push(Line::from(Span::styled(
                    card_copy(&self.words.editing_restore, theme),
                    styles.input,
                )));
            }
            for (index, value) in self.queued.iter().enumerate() {
                let prefix = card_copy(&self.words.queued_prefix, theme)
                    .replace("{number}", &(index + 1).to_string());
                card_item(
                    &mut plan,
                    value,
                    &prefix,
                    styles.input.add_modifier(Modifier::ITALIC),
                    theme,
                );
            }
            if !self.queued.is_empty() {
                plan.push(Line::from(Span::styled(
                    card_copy(&self.words.queued_controls, theme),
                    styles.input,
                )));
            }
        }
        plan
    }

    #[must_use]
    pub fn height(&self, width: u16, theme: &Theme) -> u16 {
        self.plan(width, theme).height()
    }
}

impl Paint for PendingCard<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        self.plan(area.width, theme).paint(area, buf, theme);
    }
    fn height(&self, width: u16, theme: &Theme) -> u16 {
        self.plan(width, theme).height()
    }
}

fn card_safe(value: &str) -> String {
    // Newlines are handled by card_item. Sanitize once before both measuring
    // and painting; invisible control/bidi bytes do not create phantom rows.
    text::display_safe(value).into_owned()
}

fn card_copy(value: &str, theme: &Theme) -> String {
    let value = card_safe(value);
    if !theme.ascii() {
        return value;
    }
    value
        .chars()
        .map(|c| {
            glyphs::ascii_fallback(c.encode_utf8(&mut [0; 4]))
                .map_or_else(|| c.to_string(), str::to_owned)
        })
        .collect()
}

fn card_header(value: &str, theme: &Theme) -> Line<'static> {
    Line::from(format!(
        "{} {}",
        glyphs::pick("•", theme.ascii()),
        card_copy(value, theme)
    ))
}

fn card_clip(value: &str, width: usize, ascii: bool) -> String {
    if text::width(value) <= width {
        return value.to_owned();
    }
    let ellipsis = if ascii && width >= 4 {
        "..."
    } else if ascii {
        "."
    } else {
        "…"
    };
    let budget = width.saturating_sub(text::width(ellipsis));
    let mut out = String::new();
    let mut cells = 0;
    for g in value.graphemes(true) {
        let next = text::width(g);
        if cells + next > budget {
            break;
        }
        out.push_str(g);
        cells += next;
    }
    if width > 0 {
        out.push_str(ellipsis);
    }
    out
}

fn card_item(plan: &mut PendingPlan, value: &str, prefix: &str, style: Style, theme: &Theme) {
    let indent = " ".repeat(card_body_width(prefix));
    let body_width = usize::from(plan.width)
        .saturating_sub(card_body_width(prefix))
        .max(1);
    let mut produced = 0;
    for (paragraph_index, paragraph) in value.split('\n').enumerate() {
        for (index, segment) in card_wrap(&card_safe(paragraph), body_width)
            .into_iter()
            .enumerate()
        {
            if produced == 3 {
                plan.push(Line::from(Span::styled(
                    format!("{indent}{}", if theme.ascii() { "..." } else { "…" }),
                    style,
                )));
                return;
            }
            let leader = if paragraph_index == 0 && index == 0 {
                prefix
            } else {
                &indent
            };
            plan.push(Line::from(Span::styled(
                format!("{leader}{segment}"),
                style,
            )));
            produced += 1;
        }
    }
}

fn card_wrap(value: &str, width: usize) -> Vec<String> {
    if value.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut cells = 0;
    for word in value.split_inclusive(' ') {
        let next = card_body_width(word);
        if cells + next > width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            cells = 0;
        }
        if next > width {
            // Preserve a long URL/token as one clipped row. Never split it
            // into several misleading overflow fragments.
            out.push(word.trim_end().to_owned());
        } else {
            current.push_str(word);
            cells += next;
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn card_body_width(value: &str) -> usize {
    // Native composer wrapping keeps its existing terminal contract for
    // circled numbers and keycaps without a variation selector. Compact
    // summaries still use ordinary Ratatui Unicode width when truncated.
    value
        .graphemes(true)
        .map(|grapheme| {
            if grapheme.contains('\u{20e3}') {
                return 2;
            }
            if let Some(c) = grapheme.chars().next()
                && c.len_utf8() == grapheme.len()
                && matches!(c, '\u{2460}'..='\u{24ff}' | '\u{2776}'..='\u{2793}' | '\u{3248}'..='\u{324f}')
            {
                return 2;
            }
            text::width(grapheme)
        })
        .sum()
}

/// Pending items and attached context over caller-owned facts.
///
/// The component holds borrowed items and an optional selected ID. It paints
/// a compact queue summary, the context row, one row per item, and an action
/// hint built from the union of the item actions. At one row of height the
/// summary and the action words share the row so an action stays
/// discoverable. An empty preview asks for zero rows.
#[derive(Clone, Debug)]
pub struct PendingInputPreview<'a> {
    pub items: Vec<PendingInputItem<'a>>,
    pub context: Vec<ContextPreviewItem<'a>>,
    pub selected: Option<Cow<'a, str>>,
    pub context_label: Cow<'a, str>,
    pub hint: Cow<'a, str>,
    pub omitted_label: Cow<'a, str>,
    pub max_rows: u16,
}

impl<'a> PendingInputPreview<'a> {
    #[must_use]
    pub fn new(items: Vec<PendingInputItem<'a>>, context: Vec<ContextPreviewItem<'a>>) -> Self {
        Self {
            items,
            context,
            selected: None,
            context_label: Cow::Borrowed("Context"),
            hint: Cow::Borrowed(""),
            omitted_label: Cow::Borrowed("more"),
            max_rows: 8,
        }
    }

    /// Select an item by its stable caller ID.
    #[must_use]
    pub fn selected(mut self, id: impl Into<Cow<'a, str>>) -> Self {
        self.selected = Some(id.into());
        self
    }

    #[must_use]
    pub fn context_label(mut self, label: impl Into<Cow<'a, str>>) -> Self {
        self.context_label = label.into();
        self
    }

    /// Replace the action hint. Empty builds one from the item actions.
    #[must_use]
    pub fn hint(mut self, hint: impl Into<Cow<'a, str>>) -> Self {
        self.hint = hint.into();
        self
    }

    #[must_use]
    pub fn omitted_label(mut self, label: impl Into<Cow<'a, str>>) -> Self {
        self.omitted_label = label.into();
        self
    }

    #[must_use]
    pub fn max_rows(mut self, rows: u16) -> Self {
        self.max_rows = rows;
        self
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.context.is_empty()
    }

    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn selected_index(&self) -> Option<usize> {
        let selected = self.selected.as_deref()?;
        self.items.iter().position(|item| &*item.id == selected)
    }

    /// The union of the actions the items report, in `Send now`, `Edit`,
    /// `Drop` order. Metadata only; the host dispatches.
    #[must_use]
    pub fn actions(&self) -> Vec<PendingInputAction> {
        PendingInputAction::ALL
            .into_iter()
            .filter(|action| {
                self.items
                    .iter()
                    .any(|item| item.actions().contains(action))
            })
            .collect()
    }

    /// The actions for one caller ID, or `None` when no item has that ID.
    #[must_use]
    pub fn actions_for(&self, id: &str) -> Option<&'static [PendingInputAction]> {
        self.items
            .iter()
            .find(|item| &*item.id == id)
            .map(PendingInputItem::actions)
    }

    /// Rows this preview wants at `width`. Empty input asks for zero.
    #[must_use]
    pub fn height(&self, width: u16, theme: &Theme) -> u16 {
        self.plan(width, theme).height()
    }

    fn separator(theme: &Theme) -> &'static str {
        if theme.ascii() { " / " } else { " · " }
    }

    fn hint_text(&self, _theme: &Theme) -> String {
        if !self.hint.is_empty() {
            return text::display_safe(&self.hint).into_owned();
        }
        self.actions()
            .iter()
            .map(|action| action.word())
            .collect::<Vec<_>>()
            .join(" / ")
    }

    fn summary_text(&self, theme: &Theme) -> String {
        let mut parts = Vec::new();
        for status in PendingInputStatus::ALL {
            let count = self
                .items
                .iter()
                .filter(|item| item.status == status)
                .count();
            if count > 0 {
                parts.push(format!("{count} {}", status.word()));
            }
        }
        if parts.is_empty() && !self.context.is_empty() {
            parts.push(format!(
                "{} {}",
                self.context.len(),
                text::display_safe(&self.context_label)
            ));
        }
        parts.join(Self::separator(theme))
    }

    fn summary_line(&self, theme: &Theme) -> Line<'static> {
        let mut spans = Vec::new();
        let separator = Self::separator(theme);
        let mut first = true;
        for status in PendingInputStatus::ALL {
            let count = self
                .items
                .iter()
                .filter(|item| item.status == status)
                .count();
            if count == 0 {
                continue;
            }
            if !first {
                spans.push(Span::styled(separator, theme.fg(Role::BorderStrong)));
            }
            spans.push(Span::styled(
                format!("{count} {}", status.word()),
                theme.fg(status.role()),
            ));
            first = false;
        }
        if first {
            spans.push(Span::styled(
                format!(
                    "{} {}",
                    self.context.len(),
                    text::display_safe(&self.context_label)
                ),
                theme.fg(Role::Muted),
            ));
        }
        Line::from(spans)
    }

    fn context_line(&self, item: &ContextPreviewItem<'_>, theme: &Theme) -> Line<'static> {
        let mut spans = vec![
            Span::styled(item.state.glyph(theme), theme.fg(item.state.role())),
            Span::raw(" "),
            Span::styled(item.state.word(), theme.fg(item.state.role())),
            Span::raw(" / "),
            Span::styled(
                text::display_safe(&item.label).into_owned(),
                theme.fg(Role::Foreground),
            ),
        ];
        if let Some(detail) = &item.detail {
            spans.push(Span::styled(
                format!(" ({})", text::display_safe(detail)),
                theme.fg(Role::Muted),
            ));
        }
        Line::from(spans)
    }

    fn item_line(&self, index: usize, theme: &Theme) -> Line<'static> {
        let item = &self.items[index];
        let selected = self.selected.as_deref() == Some(&*item.id);
        let marker = if selected {
            if theme.ascii() { "> " } else { "▸ " }
        } else {
            "  "
        };
        let mut spans = vec![
            Span::styled(marker, theme.fg(Role::Accent)),
            Span::styled(item.status.glyph(theme), theme.fg(item.status.role())),
            Span::raw(" "),
            Span::styled(
                text::display_safe(item.word()).into_owned(),
                theme.fg(item.status.role()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(": ", theme.fg(Role::BorderStrong)),
        ];
        if !item.label.is_empty() {
            spans.push(Span::styled(
                format!("{} ", text::display_safe(&item.label)),
                theme.fg(Role::Muted),
            ));
        }
        spans.push(Span::styled(
            one_line(&item.content),
            theme.fg(Role::Foreground),
        ));
        if selected {
            let actions = item.actions();
            if !actions.is_empty() {
                spans.push(Span::raw("  "));
                for (position, action) in actions.iter().enumerate() {
                    if position > 0 {
                        spans.push(Span::styled(" / ", theme.fg(Role::BorderStrong)));
                    }
                    spans.push(Span::styled(action.word(), theme.fg(action.role())));
                }
            }
        }
        Line::from(spans)
    }

    fn single_line(&self, width: usize, theme: &Theme) -> Line<'static> {
        let ascii = theme.ascii();
        let hint = self.hint_text(theme);
        let summary = self.summary_text(theme);
        let separator = Self::separator(theme);
        let shown = if hint.is_empty() {
            text::truncate_words(&summary, width, ascii).into_owned()
        } else {
            let full = format!("{summary}{separator}{hint}");
            let short = format!(
                "{} pending{separator}{hint}",
                self.items.len() + self.context.len()
            );
            if text::width(&full) <= width {
                full
            } else if text::width(&short) <= width {
                short
            } else {
                text::truncate_words(&hint, width, ascii).into_owned()
            }
        };
        Line::from(Span::styled(shown, theme.fg(Role::Foreground)))
    }

    fn plan(&self, width: u16, theme: &Theme) -> PendingPlan {
        let mut plan = PendingPlan::new(width, self.max_rows);
        if width < 4 || self.is_empty() {
            return plan;
        }
        plan.one_row = Some(PaintedRow {
            line: self.single_line(usize::from(width), theme),
            selected: false,
        });
        plan.push(self.summary_line(theme));
        let selected = self.selected_index();
        if let Some(index) = selected {
            plan.rows.push(PaintedRow {
                line: self.item_line(index, theme),
                selected: true,
            });
        }
        for item in &self.context {
            plan.push(self.context_line(item, theme));
        }
        for (index, _) in self.items.iter().enumerate() {
            if selected != Some(index) {
                plan.push(self.item_line(index, theme));
            }
        }
        let hint = self.hint_text(theme);
        if !hint.is_empty() {
            plan.tail = Some(PendingTail {
                hint,
                omitted: text::display_safe(&self.omitted_label).into_owned(),
                hint_style: theme.fg(Role::Accent),
                omitted_style: theme.fg(Role::Muted),
            });
        }
        plan
    }
}

fn one_line(value: &str) -> String {
    text::display_safe(&value.replace(['\n', '\r', '\t'], " ")).into_owned()
}

impl Paint for PendingInputPreview<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        self.plan(area.width, theme).paint(area, buf, theme);
    }
    fn height(&self, width: u16, theme: &Theme) -> u16 {
        self.plan(width, theme).height()
    }
}
