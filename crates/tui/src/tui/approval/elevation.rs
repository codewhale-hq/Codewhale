//! Sandbox-elevation request policy and modal state.
//!
//! Elevation remains a distinct post-denial authority boundary: this module
//! owns the retry policy options, request construction, interaction state, and
//! typed decision event without changing the initial approval flow.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::sandbox::SandboxPolicy;
use crate::tui::shell_key_routing::{self, Focus, ShellBindingId};
use crate::tui::views::{ModalKind, ModalView, ViewAction, ViewEvent};
use crate::tui::widgets::{ElevationWidget, Renderable};
use codewhale_localization::Locale;

/// Options for elevating sandbox permissions after a denial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElevationOption {
    /// Add network access to the sandbox policy.
    WithNetwork,
    /// Add write access to specific paths.
    WithWriteAccess(Vec<PathBuf>),
    /// Remove sandbox restrictions entirely (dangerous).
    FullAccess,
    /// Abort the tool execution.
    Abort,
}

impl ElevationOption {
    /// Get the display label for this option.
    #[cfg(test)]
    pub fn label(&self) -> &'static str {
        match self {
            ElevationOption::WithNetwork => "Allow outbound network",
            ElevationOption::WithWriteAccess(_) => "Allow extra write access",
            ElevationOption::FullAccess => "Full access (filesystem + network)",
            ElevationOption::Abort => "Abort",
        }
    }

    /// Get a short description.
    #[cfg(test)]
    pub fn description(&self) -> &'static str {
        match self {
            ElevationOption::WithNetwork => "Retry with outbound network (downloads and HTTP)",
            ElevationOption::WithWriteAccess(_) => "Retry with a wider writable scope",
            ElevationOption::FullAccess => {
                "Retry without sandbox limits; grants unrestricted filesystem and network access"
            }
            ElevationOption::Abort => "Skip only this tool call",
        }
    }

    /// Convert to a sandbox policy.
    pub fn to_policy(&self, base_cwd: &Path) -> SandboxPolicy {
        match self {
            ElevationOption::WithNetwork => SandboxPolicy::workspace_with_network(),
            ElevationOption::WithWriteAccess(paths) => {
                let mut roots = paths.clone();
                roots.push(base_cwd.to_path_buf());
                SandboxPolicy::workspace_with_roots(roots, false)
            }
            ElevationOption::FullAccess => SandboxPolicy::DangerFullAccess,
            ElevationOption::Abort => SandboxPolicy::default(), // Won't be used
        }
    }
}

/// Request for user decision after a sandbox denial.
#[derive(Debug, Clone)]
pub struct ElevationRequest {
    /// The tool ID that was blocked.
    pub tool_id: String,
    /// The tool name.
    pub tool_name: String,
    /// The command that was blocked (if shell).
    pub command: Option<String>,
    /// The reason for denial (from sandbox).
    pub denial_reason: String,
    /// Available elevation options.
    pub options: Vec<ElevationOption>,
}

impl ElevationRequest {
    /// Create a new elevation request for a shell command.
    pub fn for_shell(
        tool_id: &str,
        command: &str,
        denial_reason: &str,
        blocked_network: bool,
        blocked_write: bool,
    ) -> Self {
        let mut options = Vec::new();

        if blocked_network {
            options.push(ElevationOption::WithNetwork);
        }
        if blocked_write {
            options.push(ElevationOption::WithWriteAccess(vec![]));
        }
        options.push(ElevationOption::FullAccess);
        options.push(ElevationOption::Abort);

        Self {
            tool_id: tool_id.to_string(),
            tool_name: "exec_shell".to_string(),
            command: Some(command.to_string()),
            denial_reason: denial_reason.to_string(),
            options,
        }
    }

    /// Create a generic elevation request.
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn generic(tool_id: &str, tool_name: &str, denial_reason: &str) -> Self {
        Self {
            tool_id: tool_id.to_string(),
            tool_name: tool_name.to_string(),
            command: None,
            denial_reason: denial_reason.to_string(),
            options: vec![
                ElevationOption::WithNetwork,
                ElevationOption::FullAccess,
                ElevationOption::Abort,
            ],
        }
    }
}

/// Elevation overlay state managed by the modal view stack.
#[derive(Debug, Clone)]
pub struct ElevationView {
    request: ElevationRequest,
    pub(super) selected: usize,
    locale: Locale,
    row_hitboxes: RefCell<Vec<Rect>>,
}

impl ElevationView {
    pub fn new(request: ElevationRequest, locale: Locale) -> Self {
        // Every request offers Abort. A reflexive Enter must never broaden
        // access before the person deliberately navigates to another option.
        let selected = request
            .options
            .iter()
            .position(|option| matches!(option, ElevationOption::Abort))
            .expect("elevation requests offer Abort");
        Self {
            request,
            selected,
            locale,
            row_hitboxes: RefCell::new(Vec::new()),
        }
    }

    fn select_prev(&mut self) {
        self.selected =
            crate::tui::list_nav::wrap_index(self.selected, self.request.options.len(), -1);
    }

    fn select_next(&mut self) {
        self.selected =
            crate::tui::list_nav::wrap_index(self.selected, self.request.options.len(), 1);
    }

    fn current_option(&self) -> &ElevationOption {
        &self.request.options[self.selected]
    }

    fn emit_decision(&self, option: ElevationOption) -> ViewAction {
        ViewAction::EmitAndClose(ViewEvent::ElevationDecision {
            tool_id: self.request.tool_id.clone(),
            tool_name: self.request.tool_name.clone(),
            option,
        })
    }

    /// Get the request for rendering.
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn request(&self) -> &ElevationRequest {
        &self.request
    }

    /// Get the currently selected index.
    #[expect(dead_code)]
    pub fn selected(&self) -> usize {
        self.selected
    }
}

impl ModalView for ElevationView {
    fn kind(&self) -> ModalKind {
        ModalKind::Elevation
    }

    fn tool_decision_request_id(&self) -> Option<&str> {
        Some(&self.request.tool_id)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        if key.kind != KeyEventKind::Press {
            return ViewAction::None;
        }
        // Ordinary text may have been intended for the composer. Even
        // selecting on a letter would make that text followed by Enter grant
        // access, so only deliberate navigation changes the selected option.
        match shell_key_routing::route(Focus::Modal(ModalKind::Elevation), &key) {
            Some(ShellBindingId::ElevationUp) => {
                self.select_prev();
                ViewAction::None
            }
            Some(ShellBindingId::ElevationDown) => {
                self.select_next();
                ViewAction::None
            }
            Some(ShellBindingId::ElevationConfirm) => {
                self.emit_decision(self.current_option().clone())
            }
            Some(ShellBindingId::ElevationAbort) => self.emit_decision(ElevationOption::Abort),
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.select_prev();
                ViewAction::None
            }
            MouseEventKind::ScrollDown => {
                self.select_next();
                ViewAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let clicked = self.row_hitboxes.borrow().iter().position(|rect| {
                    rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                });
                if let Some(index) = clicked {
                    return self.emit_decision(self.request.options[index].clone());
                }
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn render(&self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        let elevation_widget = ElevationWidget::new_with_hitboxes(
            &self.request,
            self.selected,
            self.locale,
            &self.row_hitboxes,
        );
        elevation_widget.render(area, buf);
    }
}
