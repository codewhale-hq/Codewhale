//! Native worktree manager UI (list / create / switch / compare).
//!
//! Data lives in [`codewhale_engine::git_status`]; this module is pure presentation +
//! key handling. Never blocks the render path on git subprocesses — refresh
//! is scheduled via `git_status::refresh_if_stale` / `force_refresh` from
//! background-friendly call sites.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::tui::views::{ModalKind, ModalView, ViewAction, ViewEvent};
use codewhale_engine::git_status::{self, GitStatusSnapshot, WorktreeEntry};
use codewhale_palette as palette;
use codewhale_tui_kit::menu_style;

/// Modes inside the worktree manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    List,
    Create,
    Compare,
}

/// Native worktree manager modal.
pub struct WorktreeManagerView {
    workspace: PathBuf,
    selected: usize,
    mode: Mode,
    create_buffer: String,
    create_as_new_branch: bool,
    compare_against: Option<PathBuf>,
    status: Option<String>,
    last_snapshot: GitStatusSnapshot,
}

impl WorktreeManagerView {
    #[must_use]
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        let workspace = workspace.into();
        git_status::refresh_if_stale(&workspace);
        let last_snapshot = git_status::cached_status();
        Self {
            workspace,
            selected: 0,
            mode: Mode::List,
            create_buffer: String::new(),
            create_as_new_branch: true,
            compare_against: None,
            status: None,
            last_snapshot,
        }
    }

    fn refresh(&mut self) {
        git_status::force_refresh(&self.workspace);
        self.last_snapshot = git_status::cached_status();
        let n = self.last_snapshot.worktrees.len().max(1);
        if self.selected >= n {
            self.selected = n.saturating_sub(1);
        }
    }

    fn entries(&self) -> &[WorktreeEntry] {
        &self.last_snapshot.worktrees
    }

    fn selected_entry(&self) -> Option<&WorktreeEntry> {
        self.entries().get(self.selected)
    }

    fn is_current(workspace: &Path, entry: &WorktreeEntry) -> bool {
        same_path(workspace, &entry.path)
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    let a = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let b = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    a == b
}

fn worktree_name_error(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        Some("Enter a branch / path name")
    } else if name.starts_with('-') {
        Some("Name cannot start with '-'")
    } else if name.contains("..") {
        Some("Name cannot contain '..'")
    } else if name.contains('\\') {
        Some("Name cannot contain '\\'")
    } else if name.chars().any(char::is_control) {
        Some("Name cannot contain control characters")
    } else {
        None
    }
}

impl ModalView for WorktreeManagerView {
    fn kind(&self) -> ModalKind {
        ModalKind::WorktreeManager
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        match self.mode {
            Mode::List => self.handle_list_key(key),
            Mode::Create => self.handle_create_key(key),
            Mode::Compare => self.handle_compare_key(key),
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let popup = centered(area, 72, 18.min(area.height.saturating_sub(2)));
        Clear.render(popup, buf);
        Block::default()
            .borders(Borders::ALL)
            .title(" Worktrees ")
            .border_style(Style::default().fg(palette::BORDER_COLOR))
            .style(
                Style::default()
                    .fg(palette::TEXT_PRIMARY)
                    .bg(palette::SURFACE_ELEVATED),
            )
            .render(popup, buf);

        let inner = Rect {
            x: popup.x.saturating_add(1),
            y: popup.y.saturating_add(1),
            width: popup.width.saturating_sub(2),
            height: popup.height.saturating_sub(2),
        };
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        match self.mode {
            Mode::List => self.render_list(inner, buf),
            Mode::Create => self.render_create(inner, buf),
            Mode::Compare => self.render_compare(inner, buf),
        }
    }
}

impl WorktreeManagerView {
    fn handle_list_key(&mut self, key: KeyEvent) -> ViewAction {
        match key.code {
            KeyCode::Esc => ViewAction::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                ViewAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = self.entries().len().saturating_sub(1);
                self.selected = (self.selected + 1).min(max);
                ViewAction::None
            }
            KeyCode::Char('r')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || key.modifiers == KeyModifiers::NONE =>
            {
                self.refresh();
                self.status = Some("Refreshed worktree list".into());
                ViewAction::None
            }
            KeyCode::Char('n') => {
                self.mode = Mode::Create;
                self.create_buffer.clear();
                ViewAction::None
            }
            KeyCode::Char('d') => {
                if let Some(path) = self.selected_entry().map(|e| e.path.clone()) {
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("worktree")
                        .to_string();
                    self.compare_against = Some(path);
                    self.mode = Mode::Compare;
                    self.status = Some(format!("Diff against {name}"));
                }
                ViewAction::None
            }
            KeyCode::Enter => {
                if let Some(entry) = self.selected_entry() {
                    let path = entry.path.display().to_string();
                    if Self::is_current(&self.workspace, entry) {
                        self.status = Some("Already in this worktree".into());
                        ViewAction::None
                    } else {
                        ViewAction::Emit(ViewEvent::StatusMessage {
                            message: format!(
                                "Switch: open a new session in {path} (cwd switch is session-scoped)"
                            ),
                        })
                    }
                } else {
                    ViewAction::None
                }
            }
            _ => ViewAction::None,
        }
    }

    fn handle_create_key(&mut self, key: KeyEvent) -> ViewAction {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::List;
                ViewAction::None
            }
            KeyCode::Backspace => {
                self.create_buffer.pop();
                ViewAction::None
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.create_as_new_branch = !self.create_as_new_branch;
                ViewAction::None
            }
            KeyCode::Enter => {
                let name = self.create_buffer.trim();
                if let Some(message) = worktree_name_error(name) {
                    self.status = Some(message.into());
                    return ViewAction::None;
                }
                let path = self
                    .workspace
                    .join(".cw-worktrees")
                    .join(name.replace('/', "-"));
                let result = git_status::create_worktree(
                    self.last_snapshot
                        .root
                        .as_deref()
                        .unwrap_or(&self.workspace),
                    &path,
                    name,
                    self.create_as_new_branch,
                );
                match result {
                    Ok(()) => {
                        self.refresh();
                        self.mode = Mode::List;
                        self.status = Some(format!("Created worktree at {}", path.display()));
                    }
                    Err(err) => {
                        self.status = Some(format!("Create failed: {err}"));
                    }
                }
                ViewAction::None
            }
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.create_buffer.push(c);
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn handle_compare_key(&mut self, key: KeyEvent) -> ViewAction {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::List;
                self.compare_against = None;
                ViewAction::None
            }
            KeyCode::Enter => {
                let Some(path) = self.compare_against.clone() else {
                    return ViewAction::None;
                };
                // Emit a status that dogfood can follow; full diff_render
                // integration uses existing /diff tooling against the path.
                ViewAction::Emit(ViewEvent::StatusMessage {
                    message: format!(
                        "Diff against {}: use /diff or context-menu Diff on a file",
                        path.display()
                    ),
                })
            }
            _ => ViewAction::None,
        }
    }

    fn render_list(&self, area: Rect, buf: &mut Buffer) {
        let mut lines: Vec<Line<'static>> = Vec::new();
        let branch = self.last_snapshot.branch.as_deref().unwrap_or("detached");
        lines.push(Line::from(vec![
            Span::styled("repo ", Style::default().fg(palette::TEXT_MUTED)),
            Span::styled(
                branch.to_string(),
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD),
            ),
            if self.last_snapshot.dirty {
                Span::styled(" *", Style::default().fg(palette::STATUS_WARNING))
            } else {
                Span::raw("")
            },
        ]));
        lines.push(Line::from(Span::styled(
            "n new · d diff against · Enter switch · r refresh · Esc close",
            Style::default().fg(palette::TEXT_HINT),
        )));
        lines.push(Line::from(""));

        if self.entries().is_empty() {
            lines.push(Line::from(Span::styled(
                "No worktrees listed (not a git repo?)",
                Style::default().fg(palette::TEXT_MUTED),
            )));
        } else {
            for (i, entry) in self.entries().iter().enumerate() {
                let selected = i == self.selected;
                let current = Self::is_current(&self.workspace, entry);
                let marker = if selected { "▸ " } else { "  " };
                let cur = if current { " · current" } else { "" };
                let locked = if entry.locked { " 🔒" } else { "" };
                let branch = entry.branch.as_deref().unwrap_or("detached");
                let path = entry
                    .path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_else(|| entry.path.to_str().unwrap_or("?"));
                let text = format!("{marker}{path} · {branch}{cur}{locked}");
                let style = if selected {
                    menu_style::selected_row_style_with_fg(palette::WHALE_ACTION)
                } else if current {
                    Style::default().fg(palette::WHALE_LIVE)
                } else {
                    Style::default().fg(palette::TEXT_PRIMARY)
                };
                // One-cell accent rail on selected row.
                let rail = if selected { "▌" } else { " " };
                lines.push(Line::from(vec![
                    Span::styled(rail, Style::default().fg(palette::WHALE_ACTION)),
                    Span::styled(
                        truncate(&text, usize::from(area.width.saturating_sub(2))),
                        style,
                    ),
                ]));
            }
        }

        if let Some(status) = &self.status {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                status.clone(),
                Style::default().fg(palette::TEXT_MUTED),
            )));
        }

        Paragraph::new(lines).render(area, buf);
    }

    fn render_create(&self, area: Rect, buf: &mut Buffer) {
        let kind = if self.create_as_new_branch {
            "new branch"
        } else {
            "existing branch"
        };
        let lines = vec![
            Line::from(Span::styled(
                "Create worktree",
                Style::default()
                    .fg(palette::TEXT_PRIMARY)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("Ctrl+B toggle · currently: {kind}"),
                Style::default().fg(palette::TEXT_HINT),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("name › ", Style::default().fg(palette::TEXT_MUTED)),
                Span::styled(
                    self.create_buffer.clone(),
                    Style::default().fg(palette::WHALE_ACTION),
                ),
                Span::styled("█", Style::default().fg(palette::TEXT_HINT)),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "Enter create · Esc back",
                Style::default().fg(palette::TEXT_HINT),
            )),
            if let Some(status) = &self.status {
                Line::from(Span::styled(
                    status.clone(),
                    Style::default().fg(palette::STATUS_WARNING),
                ))
            } else {
                Line::from("")
            },
        ];
        Paragraph::new(lines).render(area, buf);
    }

    fn render_compare(&self, area: Rect, buf: &mut Buffer) {
        let path = self
            .compare_against
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none)".into());
        let lines = vec![
            Line::from(Span::styled(
                "Diff against worktree",
                Style::default()
                    .fg(palette::TEXT_PRIMARY)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                truncate(&path, usize::from(area.width)),
                Style::default().fg(palette::WHALE_LIVE),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Enter: show diff workflow · Esc back",
                Style::default().fg(palette::TEXT_HINT),
            )),
            Line::from(Span::styled(
                "Uses existing diff_render surfaces — never blocks UI on git.",
                Style::default().fg(palette::TEXT_MUTED),
            )),
        ];
        Paragraph::new(lines).render(area, buf);
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x.saturating_add(area.width.saturating_sub(width) / 2),
        y: area
            .y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    }
}

fn truncate(text: &str, max: usize) -> String {
    if UnicodeWidthStr::width(text) <= max {
        return text.to_string();
    }
    if max <= 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut w = 0usize;
    let limit = max.saturating_sub(1);
    for ch in text.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > limit {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_constructs_from_workspace() {
        let view = WorktreeManagerView::new(std::env::temp_dir());
        assert_eq!(view.mode, Mode::List);
    }
}
