//! Right-click context menu for mouse-captured TUI sessions.
//!
//! v0.9.1: elevated, lightly rounded surface with leading glyphs, section
//! grouping, right-aligned key-hint chips, hover-follow, and a primary action
//! focused by default. Reduced motion opens instantly (no appear frames).

use std::cell::Cell;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Widget},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::ocean;
use crate::tui::views::{ContextMenuAction, ModalKind, ModalView, ViewAction, ViewEvent};
use codewhale_palette as palette;
use codewhale_tui_kit::menu_style;

#[derive(Debug, Clone)]
pub struct ContextMenuEntry {
    pub label: String,
    pub description: String,
    pub action: ContextMenuAction,
    /// Leading glyph / icon (e.g. "⎘", "↗", "⌥").
    pub glyph: String,
    /// Right-aligned key that runs this entry (e.g. "y", "1"). Every hint
    /// shown is live: `handle_key` runs the entry whose hint matches.
    pub hint: String,
    /// When true, starts a new visual section above this entry.
    pub section_start: bool,
    /// Primary (most likely) action — focused by default and accent-styled.
    pub primary: bool,
    /// Destructive entries are confirmed in place: the first activation arms
    /// the row and swaps its label for this text, the second runs it.
    pub confirm_label: Option<String>,
}

impl ContextMenuEntry {
    pub fn new(
        label: impl Into<String>,
        description: impl Into<String>,
        action: ContextMenuAction,
    ) -> Self {
        Self {
            label: label.into(),
            description: description.into(),
            action,
            glyph: String::new(),
            hint: String::new(),
            section_start: false,
            primary: false,
            confirm_label: None,
        }
    }

    #[must_use]
    pub fn with_glyph(mut self, glyph: impl Into<String>) -> Self {
        self.glyph = glyph.into();
        self
    }

    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    #[must_use]
    pub fn section_start(mut self) -> Self {
        self.section_start = true;
        self
    }

    #[must_use]
    pub fn primary(mut self) -> Self {
        self.primary = true;
        self
    }

    /// Require a second activation. `armed_label` replaces the row label
    /// while the entry waits for it, and says what confirms it ("Stop agent:
    /// Enter or click again to confirm").
    #[must_use]
    /// A `{label}` in `armed_label` becomes this row's label (without a
    /// trailing `…`), so the armed row still names what it will do.
    pub fn confirm(mut self, armed_label: impl Into<String>) -> Self {
        let label = self.label.trim_end_matches('…').trim_end();
        self.confirm_label = Some(armed_label.into().replace("{label}", label));
        self
    }
}

/// Keys the menu itself uses; never handed out as entry hints.
const RESERVED_KEYS: [char; 3] = ['j', 'k', 'q'];
/// Letters handed to entries past the ninth, after digits run out.
const BACKFILL_LETTERS: &str = "abcdfghilmnorstuvwxz";
/// A second click on an armed row sooner than this is the tail of a
/// double-click, not a decision, so it does not confirm. Same window the
/// composer uses to tell a double-click from two clicks.
const CONFIRM_CLICK_GUARD: std::time::Duration =
    std::time::Duration::from_millis(crate::tui::mouse_ui::DOUBLE_CLICK_MS);

/// One painted row of the menu body, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuRow {
    Title,
    Divider,
    Entry(usize),
    MoreAbove(usize),
    MoreBelow(usize),
    Detail,
}

pub struct ContextMenuView {
    entries: Vec<ContextMenuEntry>,
    selected: usize,
    /// Entry waiting for its confirming second activation.
    armed: Option<usize>,
    /// When `armed` was set, so a double-click cannot arm and confirm a
    /// destructive row in one gesture.
    armed_at: Option<Instant>,
    /// First entry drawn when the menu is taller than the screen. Kept in a
    /// `Cell` because render (`&self`) is where the height is known.
    scroll: Cell<usize>,
    column: u16,
    row: u16,
    last_rect: Cell<Option<Rect>>,
    title: String,
    opened_at: Instant,
    reduced_motion: bool,
}

impl ContextMenuView {
    pub fn new_with_motion(
        entries: Vec<ContextMenuEntry>,
        column: u16,
        row: u16,
        title: String,
        reduced_motion: bool,
    ) -> Self {
        // Focus the primary action by default when present.
        let selected = entries.iter().position(|e| e.primary).unwrap_or(0);
        let mut entries = entries;
        assign_hints(&mut entries);
        for entry in &mut entries {
            if entry.glyph.is_empty() {
                entry.glyph = default_glyph_for(&entry.action);
            }
        }
        Self {
            entries,
            selected,
            armed: None,
            armed_at: None,
            scroll: Cell::new(0),
            column,
            row,
            last_rect: Cell::new(None),
            title,
            opened_at: Instant::now(),
            reduced_motion,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        self.armed = None;
        self.selected =
            codewhale_tui_kit::list_nav::wrap_index(self.selected, self.entries.len(), delta);
    }

    /// Run entry `idx`, or arm it when it needs confirming. `confirm` is false
    /// for a hint key: a letter arms a destructive row but never confirms it.
    fn activate(&mut self, idx: usize, confirm: bool) -> ViewAction {
        let Some(entry) = self.entries.get(idx) else {
            return ViewAction::None;
        };
        if self.selected != idx {
            self.armed = None;
        }
        self.selected = idx;
        if entry.confirm_label.is_some() && !(confirm && self.armed == Some(idx)) {
            if self.armed != Some(idx) {
                self.armed_at = Some(Instant::now());
            }
            self.armed = Some(idx);
            return ViewAction::None;
        }
        ViewAction::EmitAndClose(ViewEvent::ContextMenuSelected {
            action: entry.action.clone(),
        })
    }

    fn label_for(&self, idx: usize) -> &str {
        let entry = &self.entries[idx];
        match (&entry.confirm_label, self.armed == Some(idx)) {
            (Some(armed), true) => armed.as_str(),
            _ => entry.label.as_str(),
        }
    }

    fn menu_width(&self, area_width: u16) -> u16 {
        let widest = self
            .entries
            .iter()
            .map(|entry| {
                let label_width = UnicodeWidthStr::width(entry.label.as_str()).max(
                    entry
                        .confirm_label
                        .as_deref()
                        .map_or(0, UnicodeWidthStr::width),
                );
                let action_width = UnicodeWidthStr::width(entry.glyph.as_str())
                    .saturating_add(label_width)
                    .saturating_add(UnicodeWidthStr::width(entry.hint.as_str()))
                    .saturating_add(7);
                let detail_width =
                    UnicodeWidthStr::width(entry.description.as_str()).saturating_add(4);
                action_width.max(detail_width)
            })
            .max()
            .unwrap_or(20)
            .max(UnicodeWidthStr::width(self.title.as_str()).saturating_add(4));
        let width = u16::try_from(widest.clamp(22, 56)).unwrap_or(56);
        width.min(area_width.max(1))
    }

    fn has_detail(&self) -> bool {
        self.entries.iter().any(|e| !e.description.is_empty())
    }

    fn visual_row_count(&self) -> usize {
        // title + entries + section dividers + a stable selected-detail row
        let dividers = self
            .entries
            .iter()
            .enumerate()
            .filter(|(idx, e)| e.section_start && *idx > 0)
            .count();
        self.entries
            .len()
            .saturating_add(usize::from(!self.title.is_empty()))
            .saturating_add(dividers)
            .saturating_add(usize::from(self.has_detail()) * 2)
    }

    fn menu_rect(&self, area: Rect) -> Rect {
        let width = self.menu_width(area.width);
        // +2: the top rail and one row of bottom padding.
        let desired_height =
            u16::try_from(self.visual_row_count().saturating_add(2)).unwrap_or(u16::MAX);
        let height = desired_height.min(area.height.max(1));
        let max_x = area.right().saturating_sub(width).max(area.x);
        let max_y = area.bottom().saturating_sub(height).max(area.y);
        let x = self.column.max(area.x).min(max_x);
        let y = self.row.max(area.y).min(max_y);
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// The body rows for a menu drawn in `rect`, starting one row below the
    /// top rail. When the entries do not fit, a window around the selection
    /// is shown with `↑ n more` / `↓ n more` rows, so every entry stays
    /// reachable and the keyboard selection is always on screen.
    fn layout(&self, rect: Rect) -> Vec<MenuRow> {
        let body = usize::from(rect.height.saturating_sub(2));
        let fixed = usize::from(!self.title.is_empty()) + usize::from(self.has_detail()) * 2;
        let budget = body.saturating_sub(fixed).max(1);
        let (start, end) = self.window(budget);

        let mut rows = Vec::new();
        if !self.title.is_empty() {
            rows.push(MenuRow::Title);
        }
        if start > 0 {
            rows.push(MenuRow::MoreAbove(start));
        }
        for idx in start..end {
            if idx > start && self.entries[idx].section_start {
                rows.push(MenuRow::Divider);
            }
            rows.push(MenuRow::Entry(idx));
        }
        if end < self.entries.len() {
            rows.push(MenuRow::MoreBelow(self.entries.len() - end));
        }
        if self.has_detail() {
            rows.push(MenuRow::Divider);
            rows.push(MenuRow::Detail);
        }
        rows
    }

    /// `[start, end)` of the entries that fit in `budget` rows with the
    /// selection visible.
    fn window(&self, budget: usize) -> (usize, usize) {
        let len = self.entries.len();
        if len == 0 {
            return (0, 0);
        }
        let fill = |start: usize| {
            let mut used = usize::from(start > 0);
            let mut idx = start;
            while idx < len {
                let cost = 1 + usize::from(idx > start && self.entries[idx].section_start);
                let more_below = usize::from(idx + 1 < len);
                if used + cost + more_below > budget {
                    break;
                }
                used += cost;
                idx += 1;
            }
            idx.max(start + 1).min(len)
        };
        let mut start = self.scroll.get().min(self.selected);
        loop {
            let end = fill(start);
            if self.selected < end || start >= self.selected {
                self.scroll.set(start);
                return (start, end);
            }
            start += 1;
        }
    }

    /// Map a mouse row to what is painted there.
    fn row_at(&self, mouse_row: u16, rect: Rect) -> Option<MenuRow> {
        let first = rect.y.saturating_add(1);
        if mouse_row < first || mouse_row >= rect.bottom() {
            return None;
        }
        self.layout(rect)
            .get(usize::from(mouse_row - first))
            .copied()
    }

    /// Map a mouse row to an entry index. Title, dividers, overflow markers
    /// and the detail row are not entries.
    fn entry_at_row(&self, mouse_row: u16, rect: Rect) -> Option<usize> {
        match self.row_at(mouse_row, rect)? {
            MenuRow::Entry(idx) => Some(idx),
            _ => None,
        }
    }

    fn inside(&self, mouse: MouseEvent) -> Option<Rect> {
        let rect = self.last_rect.get()?;
        (mouse.column >= rect.x
            && mouse.column < rect.right()
            && mouse.row >= rect.y
            && mouse.row < rect.bottom())
        .then_some(rect)
    }

    fn clicked_entry(&self, mouse: MouseEvent) -> Option<usize> {
        let rect = self.inside(mouse)?;
        self.entry_at_row(mouse.row, rect)
    }

    fn appear_progress(&self) -> f32 {
        if self.reduced_motion {
            return 1.0;
        }
        let ms = self.opened_at.elapsed().as_millis() as f32;
        // Two-frame (~80 ms) soft open.
        (ms / 80.0).clamp(0.0, 1.0)
    }
}

/// Give every entry a working key. Explicit hints are kept unless an earlier
/// entry already owns the key (two menus merged into one can both ask for
/// `y`); entries without one get `1`–`9` by position, then a spare letter.
fn assign_hints(entries: &mut [ContextMenuEntry]) {
    let mut taken: Vec<String> = RESERVED_KEYS.iter().map(char::to_string).collect();
    for entry in entries.iter_mut() {
        if entry.hint.is_empty() {
            continue;
        }
        if taken.contains(&entry.hint) {
            entry.hint.clear();
        } else {
            taken.push(entry.hint.clone());
        }
    }
    let mut spare = BACKFILL_LETTERS.chars().map(|c| c.to_string());
    for (idx, entry) in entries.iter_mut().enumerate() {
        if !entry.hint.is_empty() {
            continue;
        }
        // Digits keep meaning "the Nth row" where that row is free to take it.
        let digit = (idx < 9).then(|| (idx + 1).to_string());
        let key = match digit {
            Some(digit) if !taken.contains(&digit) => digit,
            _ => match spare.find(|key| !taken.contains(key)) {
                Some(key) => key,
                None => continue,
            },
        };
        taken.push(key.clone());
        entry.hint = key;
    }
}

impl ModalView for ContextMenuView {
    fn kind(&self) -> ModalKind {
        ModalKind::ContextMenu
    }

    /// The context menu is a small anchored popup, not a full-screen modal:
    /// scope the central backdrop to the menu itself so opening it does not
    /// blank the transcript behind it (#3868).
    fn occupied_region(&self, area: Rect) -> Rect {
        self.menu_rect(area)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ViewAction::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                ViewAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                ViewAction::None
            }
            KeyCode::Enter => self.activate(self.selected, true),
            // A hint is the bare letter: Ctrl+C or Alt+Y must not run the
            // row that happens to carry `c` or `y`.
            KeyCode::Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                let key = c.to_string();
                match self.entries.iter().position(|entry| entry.hint == key) {
                    Some(idx) => self.activate(idx, false),
                    None => ViewAction::None,
                }
            }
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        match mouse.kind {
            MouseEventKind::Moved => {
                // Hover-follow: selection tracks the pointer over rows.
                if let Some(idx) = self.clicked_entry(mouse)
                    && self.selected != idx
                {
                    self.armed = None;
                    self.selected = idx;
                }
                ViewAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(idx) = self.clicked_entry(mouse) {
                    // The second half of a double-click lands on the row the
                    // first half just armed; it keeps the row armed and
                    // waits for a deliberate click or Enter.
                    let double_click_tail = self.armed == Some(idx)
                        && self
                            .armed_at
                            .is_some_and(|at| at.elapsed() < CONFIRM_CLICK_GUARD);
                    if double_click_tail {
                        return ViewAction::None;
                    }
                    return self.activate(idx, true);
                }
                // The title, dividers, overflow markers and detail row are
                // part of the menu, not a way out of it. Only a click
                // outside dismisses.
                if self.inside(mouse).is_some() {
                    ViewAction::None
                } else {
                    ViewAction::Close
                }
            }
            MouseEventKind::Down(MouseButton::Right) => ViewAction::Close,
            MouseEventKind::ScrollUp => {
                self.move_selection(-1);
                ViewAction::None
            }
            MouseEventKind::ScrollDown => {
                self.move_selection(1);
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let menu_area = self.menu_rect(area);
        self.last_rect.set(Some(menu_area));
        Clear.render(menu_area, buf);

        let progress = self.appear_progress();
        let elevated = palette::SURFACE_ELEVATED;
        let shadow = ocean::mix_colors(elevated, palette::WHALE_BG, 0.35);
        let accent = palette::WHALE_ACTION;
        let soft_accent = ocean::mix_colors(accent, elevated, 0.72);

        // Soft depth: paint a one-cell shadow offset below/right when space allows.
        if progress >= 1.0 && menu_area.right() < area.right() {
            for y in menu_area.y..menu_area.bottom() {
                let cell = &mut buf[(menu_area.right(), y)];
                if cell.symbol() == " " || cell.symbol().is_empty() {
                    cell.set_bg(shadow);
                }
            }
        }
        if progress >= 1.0 && menu_area.bottom() < area.bottom() {
            for x in menu_area.x..menu_area.right() {
                let cell = &mut buf[(x, menu_area.bottom())];
                if cell.symbol() == " " || cell.symbol().is_empty() {
                    cell.set_bg(shadow);
                }
            }
        }

        // Fill elevated surface (borderless form).
        for y in menu_area.y..menu_area.bottom() {
            for x in menu_area.x..menu_area.right() {
                buf[(x, y)].set_bg(elevated);
            }
        }

        // Soft top accent rail (1 cell) instead of a heavy border.
        for x in menu_area.x..menu_area.right() {
            buf[(x, menu_area.y)].set_bg(soft_accent);
        }

        let inner_width = menu_area.width.saturating_sub(2) as usize;
        let muted = Style::default().fg(palette::TEXT_HINT).bg(elevated);
        let divider = Line::from(Span::styled(
            format!(" {}", "─".repeat(inner_width.min(48))),
            Style::default().fg(palette::BORDER_COLOR).bg(elevated),
        ));
        let mut lines: Vec<Line<'static>> = Vec::new();

        for row in self.layout(menu_area) {
            match row {
                MenuRow::Title => {
                    let title = trim_to_width(&self.title, inner_width);
                    lines.push(Line::from(Span::styled(
                        format!(" {title}"),
                        muted.add_modifier(Modifier::BOLD),
                    )));
                }
                MenuRow::Divider => lines.push(divider.clone()),
                MenuRow::MoreAbove(count) => {
                    lines.push(Line::from(Span::styled(format!(" ↑ {count} more"), muted)));
                }
                MenuRow::MoreBelow(count) => {
                    lines.push(Line::from(Span::styled(format!(" ↓ {count} more"), muted)));
                }
                MenuRow::Detail => {
                    let description = self
                        .entries
                        .get(self.selected)
                        .map(|entry| {
                            trim_to_width(&entry.description, inner_width.saturating_sub(2))
                        })
                        .unwrap_or_default();
                    lines.push(Line::from(Span::styled(format!(" {description}"), muted)));
                }
                MenuRow::Entry(idx) => {
                    lines.push(self.entry_line(idx, inner_width, elevated, accent));
                }
            }
        }

        let body = Rect {
            x: menu_area.x,
            y: menu_area.y.saturating_add(1),
            width: menu_area.width,
            height: menu_area.height.saturating_sub(1),
        };
        Paragraph::new(lines).render(body, buf);
    }
}

impl ContextMenuView {
    fn entry_line(
        &self,
        idx: usize,
        inner_width: usize,
        elevated: ratatui::style::Color,
        accent: ratatui::style::Color,
    ) -> Line<'static> {
        let entry = &self.entries[idx];
        let selected = idx == self.selected;
        let row_style = if selected {
            menu_style::selected_row_style()
        } else {
            let label_fg = if entry.primary {
                accent
            } else {
                palette::TEXT_SOFT
            };
            Style::default().fg(label_fg).bg(elevated)
        };
        let glyph = if entry.glyph.is_empty() {
            "·"
        } else {
            entry.glyph.as_str()
        };
        let hint = entry.hint.as_str();
        let glyph_width = UnicodeWidthStr::width(glyph);
        // Fixed glyph slot of two display columns: full-width icons (📌,
        // 2 cols) fit as-is, narrow ones (? / ↩, 1 col) get a trailing
        // space so every label starts at the same column.
        let glyph_slot = if glyph_width < 2 {
            format!("{glyph} ")
        } else {
            glyph.to_string()
        };
        let label_budget = inner_width
            .saturating_sub(glyph_width.max(2))
            .saturating_sub(UnicodeWidthStr::width(hint))
            .saturating_sub(4);
        let label = trim_to_width(self.label_for(idx), label_budget);
        let pad = label_budget.saturating_sub(UnicodeWidthStr::width(label.as_str()));
        let text = format!(" {glyph_slot} {label}{} {hint} ", " ".repeat(pad));
        let style = if self.armed == Some(idx) {
            row_style
                .fg(palette::STATUS_WARNING)
                .add_modifier(Modifier::BOLD)
        } else if !selected && entry.primary {
            row_style.add_modifier(Modifier::BOLD)
        } else {
            row_style
        };
        Line::from(Span::styled(text, style))
    }
}

fn default_glyph_for(action: &ContextMenuAction) -> String {
    // Best-effort icons from the action discriminant name.
    let name = format!("{action:?}");
    if name.contains("Copy") {
        "⎘".to_string()
    } else if name.contains("Paste") {
        "📋".to_string()
    } else if name.contains("Open") || name.contains("Edit") {
        "↗".to_string()
    } else if name.contains("Diff") || name.contains("Git") {
        "⌥".to_string()
    } else if name.contains("Select") {
        "▣".to_string()
    } else {
        "·".to_string()
    }
}

fn trim_to_width(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 3 {
        let mut out = String::new();
        let mut width = 0usize;
        for ch in text.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if width + ch_width > max_width {
                break;
            }
            out.push(ch);
            width += ch_width;
        }
        return out;
    }

    let limit = max_width.saturating_sub(3);
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > limit {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_entry_is_selected_by_default() {
        let entries = vec![
            ContextMenuEntry::new("Copy", "", ContextMenuAction::CopySelection),
            ContextMenuEntry::new("Paste", "", ContextMenuAction::Paste).primary(),
        ];
        let view = ContextMenuView::new_with_motion(entries, 0, 0, "menu".into(), false);
        assert_eq!(view.selected, 1);
    }

    #[test]
    fn reduced_motion_opens_instantly() {
        let view = ContextMenuView::new_with_motion(
            vec![ContextMenuEntry::new(
                "Copy",
                "",
                ContextMenuAction::CopySelection,
            )],
            0,
            0,
            "menu".into(),
            true,
        );
        assert!((view.appear_progress() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn hover_rows_match_titled_menu_entries_and_dividers() {
        let entries = vec![
            ContextMenuEntry::new("Copy", "", ContextMenuAction::CopySelection),
            ContextMenuEntry::new("Paste", "", ContextMenuAction::Paste).section_start(),
            ContextMenuEntry::new("Help", "", ContextMenuAction::OpenHelp).primary(),
        ];
        let mut view = ContextMenuView::new_with_motion(entries, 0, 0, "Actions".into(), false);
        let rect = Rect::new(10, 5, 30, 10);
        view.last_rect.set(Some(rect));
        let moved = |row| MouseEvent {
            kind: MouseEventKind::Moved,
            column: rect.x.saturating_add(1),
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };

        // The title and section divider are not selectable.
        view.handle_mouse(moved(rect.y.saturating_add(1)));
        assert_eq!(view.selected, 2);
        view.handle_mouse(moved(rect.y.saturating_add(2)));
        assert_eq!(view.selected, 0);
        view.handle_mouse(moved(rect.y.saturating_add(3)));
        assert_eq!(view.selected, 0);

        view.handle_mouse(moved(rect.y.saturating_add(4)));
        assert_eq!(view.selected, 1);
        view.handle_mouse(moved(rect.y.saturating_add(5)));
        assert_eq!(view.selected, 2);
    }

    #[test]
    fn untitled_menu_entries_start_on_first_body_row() {
        let view = ContextMenuView::new_with_motion(
            vec![ContextMenuEntry::new(
                "Copy",
                "",
                ContextMenuAction::CopySelection,
            )],
            0,
            0,
            String::new(),
            false,
        );
        let rect = Rect::new(10, 5, 30, 5);

        assert_eq!(view.entry_at_row(rect.y.saturating_add(1), rect), Some(0));
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }
    }

    fn emitted(action: ViewAction) -> Option<ContextMenuAction> {
        match action {
            ViewAction::EmitAndClose(ViewEvent::ContextMenuSelected { action }) => Some(action),
            _ => None,
        }
    }

    fn cell(index: usize) -> ContextMenuAction {
        ContextMenuAction::HideCell { cell_index: index }
    }

    /// T3: the letters drawn in the menu used to be decoration — only digits,
    /// Enter, j/k, q and Esc were handled. Every hint shown now runs its row.
    #[test]
    fn letter_hints_run_their_entry() {
        let entries = vec![
            ContextMenuEntry::new("Paste", "", ContextMenuAction::Paste).with_hint("p"),
            ContextMenuEntry::new("Copy", "", ContextMenuAction::CopySelection).with_hint("y"),
            ContextMenuEntry::new("Help", "", ContextMenuAction::OpenHelp),
        ];
        let mut view = ContextMenuView::new_with_motion(entries, 0, 0, String::new(), true);

        assert_eq!(
            emitted(view.handle_key(key(KeyCode::Char('y')))),
            Some(ContextMenuAction::CopySelection)
        );
        assert_eq!(
            emitted(view.handle_key(key(KeyCode::Char('p')))),
            Some(ContextMenuAction::Paste)
        );
        // The third row has no letter of its own and gets its position.
        assert_eq!(view.entries[2].hint, "3");
        assert_eq!(
            emitted(view.handle_key(key(KeyCode::Char('3')))),
            Some(ContextMenuAction::OpenHelp)
        );
        assert!(matches!(
            view.handle_key(key(KeyCode::Char('x'))),
            ViewAction::None
        ));
        // A modified letter is a different shortcut, not the row's hint.
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            assert!(matches!(
                view.handle_key(KeyEvent::new(KeyCode::Char('y'), modifiers)),
                ViewAction::None
            ));
        }
    }

    /// Entries past the ninth had no key at all. Each now gets a spare
    /// letter that is shown and works; j, k and q stay navigation.
    #[test]
    fn entries_past_nine_get_a_working_key() {
        let entries = (0..12)
            .map(|index| ContextMenuEntry::new(format!("row {index}"), "", cell(index)))
            .collect();
        let mut view = ContextMenuView::new_with_motion(entries, 0, 0, String::new(), true);

        let hints: Vec<&str> = view.entries.iter().map(|e| e.hint.as_str()).collect();
        assert_eq!(&hints[..9], ["1", "2", "3", "4", "5", "6", "7", "8", "9"]);
        for hint in &hints[9..] {
            assert!(!hint.is_empty(), "every row has a key: {hints:?}");
            assert!(!["j", "k", "q"].contains(hint), "{hints:?}");
        }
        let tenth = view.entries[9].hint.chars().next().expect("tenth hint");
        assert_eq!(
            emitted(view.handle_key(key(KeyCode::Char(tenth)))),
            Some(cell(9))
        );
    }

    /// Two builders asking for the same letter (a row's Copy and Copy
    /// selection both want `y`) must not leave a hint that runs the wrong row.
    #[test]
    fn a_duplicate_hint_goes_to_the_first_entry_only() {
        let entries = vec![
            ContextMenuEntry::new("Copy row", "", ContextMenuAction::Paste).with_hint("y"),
            ContextMenuEntry::new("Copy selection", "", ContextMenuAction::CopySelection)
                .with_hint("y"),
        ];
        let view = ContextMenuView::new_with_motion(entries, 0, 0, String::new(), true);
        assert_eq!(view.entries[0].hint, "y");
        assert_eq!(view.entries[1].hint, "2");
    }

    /// T9: the title, dividers and detail row are part of the menu. Clicking
    /// them used to close it; only a click outside the menu dismisses now.
    #[test]
    fn clicks_on_title_divider_and_detail_are_inert() {
        let entries = vec![
            ContextMenuEntry::new("Copy", "copy it", ContextMenuAction::CopySelection),
            ContextMenuEntry::new("Paste", "paste it", ContextMenuAction::Paste).section_start(),
        ];
        let mut view = ContextMenuView::new_with_motion(entries, 0, 0, "Title".into(), true);
        let rect = Rect::new(10, 5, 30, 10);
        view.last_rect.set(Some(rect));
        let rows = view.layout(rect);
        assert_eq!(
            rows,
            vec![
                MenuRow::Title,
                MenuRow::Entry(0),
                MenuRow::Divider,
                MenuRow::Entry(1),
                MenuRow::Divider,
                MenuRow::Detail,
            ]
        );
        for (offset, row) in rows.iter().enumerate() {
            if matches!(row, MenuRow::Entry(_)) {
                continue;
            }
            let y = rect.y + 1 + u16::try_from(offset).unwrap();
            assert!(
                matches!(view.handle_mouse(click(rect.x + 2, y)), ViewAction::None),
                "{row:?} must not close the menu"
            );
        }
        // The rail and the padding row are inside the menu too.
        assert!(matches!(
            view.handle_mouse(click(rect.x + 2, rect.y)),
            ViewAction::None
        ));
        assert!(matches!(
            view.handle_mouse(click(rect.x + 2, rect.bottom() - 1)),
            ViewAction::None
        ));
        assert!(matches!(
            view.handle_mouse(click(rect.right() + 1, rect.y)),
            ViewAction::Close
        ));
        assert_eq!(
            emitted(view.handle_mouse(click(rect.x + 2, rect.y + 4))),
            Some(ContextMenuAction::Paste)
        );
    }

    /// Destructive rows are confirmed in place: the first Enter or click arms
    /// the row, the second runs it, and moving away disarms. A letter can
    /// arm a row but never confirms it.
    #[test]
    fn a_confirm_entry_needs_a_second_activation() {
        let entries = vec![
            ContextMenuEntry::new("Copy", "", ContextMenuAction::CopySelection),
            ContextMenuEntry::new("Stop agent…", "", ContextMenuAction::Paste)
                .confirm("{label}: Enter or click again to confirm")
                .with_hint("s"),
        ];
        let mut view = ContextMenuView::new_with_motion(entries, 0, 0, String::new(), true);

        assert!(matches!(
            view.handle_key(key(KeyCode::Char('s'))),
            ViewAction::None
        ));
        assert_eq!(view.armed, Some(1));
        assert_eq!(
            view.label_for(1),
            "Stop agent: Enter or click again to confirm",
            "the armed row names its action and what confirms it"
        );
        assert!(
            matches!(view.handle_key(key(KeyCode::Char('s'))), ViewAction::None),
            "a letter never confirms"
        );
        assert_eq!(
            emitted(view.handle_key(key(KeyCode::Enter))),
            Some(ContextMenuAction::Paste)
        );

        let entries = vec![
            ContextMenuEntry::new("Copy", "", ContextMenuAction::CopySelection),
            ContextMenuEntry::new("Stop", "", ContextMenuAction::Paste).confirm("again"),
        ];
        let mut view = ContextMenuView::new_with_motion(entries, 0, 0, String::new(), true);
        view.handle_key(key(KeyCode::Down));
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter)),
            ViewAction::None
        ));
        view.handle_key(key(KeyCode::Up));
        view.handle_key(key(KeyCode::Down));
        assert_eq!(view.armed, None, "moving the selection disarms");
        assert_eq!(view.label_for(1), "Stop");
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter)),
            ViewAction::None
        ));
    }

    /// A double-click on a destructive row used to arm it and confirm it in
    /// one gesture. The second click of a double-click is now ignored; a
    /// later, separate click (or Enter) still confirms.
    #[test]
    fn a_double_click_does_not_confirm_a_destructive_row() {
        let entries = vec![
            ContextMenuEntry::new("Copy", "", ContextMenuAction::CopySelection),
            ContextMenuEntry::new("Stop agent…", "", ContextMenuAction::Paste)
                .confirm("{label}: Enter or click again to confirm"),
        ];
        let mut view = ContextMenuView::new_with_motion(entries, 0, 0, String::new(), true);
        let rect = Rect::new(0, 0, 40, 6);
        view.last_rect.set(Some(rect));
        let stop_row = rect.y + 2;
        assert_eq!(view.entry_at_row(stop_row, rect), Some(1));

        assert!(matches!(
            view.handle_mouse(click(2, stop_row)),
            ViewAction::None
        ));
        assert_eq!(view.armed, Some(1));
        assert!(
            matches!(view.handle_mouse(click(2, stop_row)), ViewAction::None),
            "the second click of a double-click must not confirm"
        );
        assert_eq!(view.armed, Some(1), "the row stays armed");

        // A deliberate click after the double-click window confirms.
        view.armed_at = Instant::now().checked_sub(CONFIRM_CLICK_GUARD * 2);
        assert_eq!(
            emitted(view.handle_mouse(click(2, stop_row))),
            Some(ContextMenuAction::Paste)
        );

        // Enter confirms at once: a key press is never a double-click.
        let entries = vec![
            ContextMenuEntry::new("Stop", "", ContextMenuAction::Paste)
                .confirm("{label}: Enter or click again to confirm"),
        ];
        let mut view = ContextMenuView::new_with_motion(entries, 0, 0, String::new(), true);
        view.last_rect.set(Some(rect));
        view.handle_mouse(click(2, rect.y + 1));
        assert_eq!(view.armed, Some(0));
        assert_eq!(
            emitted(view.handle_key(key(KeyCode::Enter))),
            Some(ContextMenuAction::Paste)
        );
    }

    /// T9: a menu taller than the screen used to be cut off. It now shows a
    /// window with `n more` markers that follows the keyboard selection.
    #[test]
    fn a_tall_menu_scrolls_to_keep_the_selection_visible() {
        let entries = (0..20)
            .map(|index| ContextMenuEntry::new(format!("row {index}"), "", cell(index)))
            .collect();
        let mut view = ContextMenuView::new_with_motion(entries, 0, 0, String::new(), true);
        let rect = Rect::new(0, 0, 30, 8);
        let rows = view.layout(rect);
        assert_eq!(rows.first(), Some(&MenuRow::Entry(0)));
        assert!(
            matches!(rows.last(), Some(MenuRow::MoreBelow(_))),
            "{rows:?}"
        );
        assert!(rows.len() <= 6, "the body fits the rect: {rows:?}");

        // Up from the first row wraps to the last one, which must be drawn.
        view.handle_key(key(KeyCode::Up));
        let rows = view.layout(rect);
        assert!(rows.contains(&MenuRow::Entry(19)), "{rows:?}");
        assert!(
            matches!(rows.first(), Some(MenuRow::MoreAbove(_))),
            "{rows:?}"
        );
        let y = 1 + u16::try_from(
            rows.iter()
                .position(|row| *row == MenuRow::Entry(19))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(view.entry_at_row(y, rect), Some(19));
    }
}
