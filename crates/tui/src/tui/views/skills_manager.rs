//! Dedicated `/skills manage` manager — audit inventory + mutation actions.
//!
//! This view never writes files. Keys emit [`ViewEvent::SkillMutationRequested`];
//! the host runs [`codewhale_skills::mutation`] and rebuilds the view.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

use super::{
    ActionHint, EmptyState, ListDetailLayout, ModalKind, ModalView, ViewAction, ViewEvent,
    render_modal_footer, render_underwater_surface, truncate_view_text,
};
use crate::tui::app::App;
use codewhale_palette as palette;
use codewhale_skills::audit::{
    AuditedSkill, AuditedSkillId, DigestState, IntegrityState, ParserState, PrecedenceState,
    ProvenanceState, SkillActionKind, SkillAuditMode, SkillAuditSnapshot, SkillSourceKind,
    TrustState, expand_owned_scan_to_compatible, scan_with_configured,
};
use codewhale_skills::mutation::{ConflictPolicy, SkillMutationRequest, SkillTargetScope};
use codewhale_skills::roots::SkillRootKind;
use codewhale_tui_kit::menu_style;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagerMode {
    OwnedOnly,
    Compatible,
}

impl ManagerMode {
    fn audit_mode(self) -> SkillAuditMode {
        match self {
            Self::OwnedOnly => SkillAuditMode::OwnedOnly,
            Self::Compatible => SkillAuditMode::Compatible,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::OwnedOnly => "owned",
            Self::Compatible => "compatible",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingConfirm {
    Remove {
        skill_id: AuditedSkillId,
        digest: Option<String>,
    },
    ImportReplace {
        skill_id: AuditedSkillId,
        digest: String,
    },
}

pub struct SkillsManagerView {
    mode: ManagerMode,
    skills: Vec<AuditedSkill>,
    selected: usize,
    detail_scroll: usize,
    import_scope: SkillTargetScope,
    pending: Option<PendingConfirm>,
    status: Option<String>,
}

impl SkillsManagerView {
    #[must_use]
    pub fn new(app: &App) -> Self {
        Self::from_scan(
            app,
            ManagerMode::OwnedOnly,
            SkillTargetScope::Global,
            None,
            None,
        )
    }

    #[must_use]
    pub fn rebuild_preserving(
        app: &App,
        previous: &Self,
        status: Option<String>,
        focus: Option<&AuditedSkillId>,
    ) -> Self {
        let mut view = Self::from_scan(
            app,
            previous.mode,
            previous.import_scope,
            status,
            focus.or_else(|| previous.selected_skill().map(|s| &s.id)),
        );
        view.detail_scroll = 0;
        view
    }

    fn from_scan(
        app: &App,
        mode: ManagerMode,
        import_scope: SkillTargetScope,
        status: Option<String>,
        focus: Option<&AuditedSkillId>,
    ) -> Self {
        let snap = scan_snapshot(app, mode);
        let mut view = Self {
            mode,
            skills: snap.skills,
            selected: 0,
            detail_scroll: 0,
            import_scope,
            pending: None,
            status,
        };
        if let Some(id) = focus
            && let Some(idx) = view.skills.iter().position(|s| &s.id == id)
        {
            view.selected = idx;
        }
        view.clamp_selection();
        view
    }

    fn selected_skill(&self) -> Option<&AuditedSkill> {
        self.skills.get(self.selected)
    }

    fn clamp_selection(&mut self) {
        if self.skills.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = self.selected.min(self.skills.len() - 1);
    }

    fn move_sel(&mut self, delta: isize) {
        if self.skills.is_empty() {
            return;
        }
        let len = self.skills.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len) as usize;
        self.selected = next;
        self.detail_scroll = 0;
        self.pending = None;
    }

    fn toggle_mode(&mut self, app: &App) {
        let next_mode = match self.mode {
            ManagerMode::OwnedOnly => ManagerMode::Compatible,
            ManagerMode::Compatible => ManagerMode::OwnedOnly,
        };
        let focus = self.selected_skill().map(|s| s.id.clone());
        let home = codewhale_paths::user_home();
        let snap = if self.mode == ManagerMode::OwnedOnly {
            expand_owned_scan_to_compatible(
                &app.workspace,
                home.as_deref(),
                Some(&app.skills_dir),
                &self.skills,
                None,
            )
        } else {
            scan_with_configured(
                &app.workspace,
                home.as_deref(),
                Some(&app.skills_dir),
                next_mode.audit_mode(),
                None,
            )
        };
        self.mode = next_mode;
        self.skills = snap.skills;
        self.pending = None;
        self.detail_scroll = 0;
        self.status = Some(format!("Scan mode: {}", self.mode.label()));
        if let Some(id) = focus {
            if let Some(idx) = self.skills.iter().position(|s| s.id == id) {
                self.selected = idx;
            } else {
                self.selected = 0;
            }
        } else {
            self.selected = 0;
        }
        self.clamp_selection();
    }

    fn cycle_import_scope(&mut self) {
        self.import_scope = match self.import_scope {
            SkillTargetScope::Global => SkillTargetScope::Project,
            SkillTargetScope::Project => SkillTargetScope::Global,
        };
        self.status = Some(format!("Import target: {}", scope_label(self.import_scope)));
    }

    fn emit_action(&mut self, kind: SkillActionKind) -> ViewAction {
        let Some(skill) = self.selected_skill().cloned() else {
            return ViewAction::None;
        };
        if !skill.available_actions.contains(&kind) {
            self.status = Some(format!(
                "{} is not available for '{}'",
                action_label(kind),
                skill.name
            ));
            return ViewAction::None;
        }

        match kind {
            SkillActionKind::Remove => {
                let digest = match &skill.digest {
                    DigestState::Known(d) => Some(d.clone()),
                    DigestState::Unknown(_) => None,
                };
                self.pending = Some(PendingConfirm::Remove {
                    skill_id: skill.id.clone(),
                    digest,
                });
                self.status = Some(format!(
                    "Remove '{}'? Press Enter to confirm, Esc to cancel.",
                    skill.name
                ));
                ViewAction::None
            }
            SkillActionKind::Import => {
                let DigestState::Known(digest) = &skill.digest else {
                    self.status = Some("Import requires a known package digest".into());
                    return ViewAction::None;
                };
                let want_kind = match self.import_scope {
                    SkillTargetScope::Project => SkillRootKind::CodeWhaleProject,
                    SkillTargetScope::Global => SkillRootKind::CodeWhaleGlobal,
                };
                // Only treat same-scope owned peers as replace conflicts — the
                // mutation controller replaces inside `import_scope` alone.
                let owned_conflict = self.skills.iter().any(|peer| {
                    peer.id.canonical_name == skill.id.canonical_name
                        && peer.root.is_writable_owned()
                        && peer.root.kind == want_kind
                        && peer.id != skill.id
                        && match &peer.digest {
                            DigestState::Known(other) => other != digest,
                            DigestState::Unknown(_) => true,
                        }
                });
                if owned_conflict {
                    self.pending = Some(PendingConfirm::ImportReplace {
                        skill_id: skill.id.clone(),
                        digest: digest.clone(),
                    });
                    self.status = Some(format!(
                        "'{}' conflicts with {} owned copy. Enter = replace, Esc = cancel.",
                        skill.name,
                        scope_label(self.import_scope)
                    ));
                    return ViewAction::None;
                }
                ViewAction::Emit(ViewEvent::SkillMutationRequested {
                    request: SkillMutationRequest::ImportExternal {
                        source_id: skill.id.clone(),
                        expected_digest: digest.clone(),
                        target: self.import_scope,
                        conflict_policy: ConflictPolicy::Reject,
                    },
                })
            }
            SkillActionKind::Update => ViewAction::Emit(ViewEvent::SkillMutationRequested {
                request: SkillMutationRequest::Update {
                    skill_id: skill.id.clone(),
                    expected_digest: known_digest(&skill),
                },
            }),
            SkillActionKind::Trust => {
                let Some(digest) = known_digest(&skill) else {
                    self.status = Some("Trust requires a known package digest".into());
                    return ViewAction::None;
                };
                ViewAction::Emit(ViewEvent::SkillMutationRequested {
                    request: SkillMutationRequest::Trust {
                        skill_id: skill.id.clone(),
                        expected_digest: digest,
                    },
                })
            }
            SkillActionKind::Install => {
                self.status = Some(
                    "Install from registry: /skill install [--project|--global] <spec>".into(),
                );
                ViewAction::None
            }
        }
    }

    fn confirm_pending(&mut self) -> ViewAction {
        match self.pending.take() {
            Some(PendingConfirm::Remove { skill_id, digest }) => {
                ViewAction::Emit(ViewEvent::SkillMutationRequested {
                    request: SkillMutationRequest::Remove {
                        skill_id,
                        expected_digest: digest,
                    },
                })
            }
            Some(PendingConfirm::ImportReplace { skill_id, digest }) => {
                ViewAction::Emit(ViewEvent::SkillMutationRequested {
                    request: SkillMutationRequest::ImportExternal {
                        source_id: skill_id,
                        expected_digest: digest,
                        target: self.import_scope,
                        conflict_policy: ConflictPolicy::ReplaceConfirmed,
                    },
                })
            }
            None => {
                // Primary action for selection.
                let Some(skill) = self.selected_skill() else {
                    return ViewAction::None;
                };
                let Some(kind) = skill.available_actions.first().copied() else {
                    self.status = Some(format!("No actions available for '{}'", skill.name));
                    return ViewAction::None;
                };
                self.emit_action(kind)
            }
        }
    }

    fn footer_hints(&self) -> Vec<ActionHint> {
        if self.pending.is_some() {
            return vec![
                ActionHint::new("Enter", "confirm"),
                ActionHint::new("Esc", "cancel"),
            ];
        }
        let mut hints = vec![
            ActionHint::new("↑/↓", "move"),
            ActionHint::new("Enter", "action"),
            ActionHint::new("c", "scan mode"),
            ActionHint::new("s", "import scope"),
            ActionHint::new("Esc", "close"),
        ];
        if let Some(skill) = self.selected_skill() {
            for action in &skill.available_actions {
                match action {
                    SkillActionKind::Import => hints.insert(2, ActionHint::new("i", "import")),
                    SkillActionKind::Update => hints.insert(2, ActionHint::new("u", "update")),
                    SkillActionKind::Remove => hints.insert(2, ActionHint::new("r", "remove")),
                    SkillActionKind::Trust => hints.insert(2, ActionHint::new("t", "trust")),
                    SkillActionKind::Install => {}
                }
            }
        }
        hints
    }

    fn render_list(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .title(Line::from(Span::styled(
                format!(" Skills ({}) ", self.skills.len()),
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD),
            )))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette::BORDER_COLOR))
            .style(Style::default().bg(palette::WHALE_BG));
        let inner = block.inner(area);
        block.render(area, buf);

        if self.skills.is_empty() {
            EmptyState::new(
                "No skills in this scan",
                "Press c to include compatible roots, or /skill install <spec>.",
            )
            .render(inner, buf);
            return;
        }

        let visible = usize::from(inner.height).max(1);
        let offset = self.selected.saturating_add(1).saturating_sub(visible);
        let end = (offset + visible).min(self.skills.len());

        for (row, idx) in (offset..end).enumerate() {
            let skill = &self.skills[idx];
            let y = inner.y + row as u16;
            if y >= inner.y + inner.height {
                break;
            }
            let selected = idx == self.selected;
            let style = if selected {
                menu_style::selected_row_style()
            } else {
                Style::default().fg(palette::TEXT_PRIMARY)
            };
            let mark = codewhale_tui_kit::glyphs::selection_marker(selected);
            let line = format!(
                "{mark} {:<6} {}  {}  {}",
                skill_tier_label(skill),
                truncate_view_text(&skill.name, 15),
                precedence_label(&skill.precedence),
                source_label(skill.source_kind),
            );
            buf.set_stringn(inner.x, y, line, usize::from(inner.width), style);
        }
    }

    fn render_detail(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .title(Line::from(Span::styled(
                " Details ",
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD),
            )))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette::BORDER_COLOR))
            .style(Style::default().bg(palette::WHALE_BG));
        let inner = block.inner(area);
        block.render(area, buf);

        let Some(skill) = self.selected_skill() else {
            EmptyState::new(
                "Nothing selected",
                "Install or import a skill to get started.",
            )
            .render(inner, buf);
            return;
        };

        let mut lines = vec![
            Line::from(Span::styled(
                skill.name.clone(),
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            kv_line("Path", &skill.safe_display_path),
            kv_line("Tier", skill_tier_label(skill)),
            kv_line("Source", source_label(skill.source_kind)),
            kv_line("Status", &precedence_label(&skill.precedence)),
            kv_line("Integrity", integrity_label(&skill.integrity)),
            kv_line("Trust", trust_label(&skill.trust)),
            kv_line("Parser", &parser_label(&skill.parser)),
            kv_line("Digest", &digest_label(&skill.digest)),
            kv_line("Provenance", &provenance_label(&skill.provenance)),
            kv_line("Readiness", "unknown"),
            kv_line("Import to", scope_label(self.import_scope)),
            kv_line("Scan", self.mode.label()),
        ];

        if let Some(desc) = skill.description.as_deref() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Description",
                Style::default().fg(palette::TEXT_MUTED),
            )));
            lines.push(Line::from(truncate_view_text(desc, 240)));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Actions",
            Style::default().fg(palette::TEXT_MUTED),
        )));
        if skill.available_actions.is_empty() {
            lines.push(Line::from(Span::styled(
                "(none — read-only or not managed)",
                Style::default().fg(palette::TEXT_MUTED),
            )));
        } else {
            let labels: Vec<&str> = skill
                .available_actions
                .iter()
                .map(|a| action_label(*a))
                .collect();
            lines.push(Line::from(labels.join(", ")));
        }

        if !skill.warnings.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Warnings",
                Style::default().fg(palette::STATUS_WARNING),
            )));
            for w in &skill.warnings {
                match w {
                    codewhale_skills::audit::SkillAuditWarning::Message(msg) => {
                        lines.push(Line::from(format!("• {msg}")));
                    }
                }
            }
        }

        if let Some(status) = &self.status {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                status.clone(),
                Style::default().fg(palette::WHALE_ACTION),
            )));
        }

        let scroll = self.detail_scroll.min(lines.len().saturating_sub(1));
        let visible: Vec<Line> = lines.into_iter().skip(scroll).collect();
        Paragraph::new(visible)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(palette::TEXT_PRIMARY))
            .render(inner, buf);
    }
}

fn scan_snapshot(app: &App, mode: ManagerMode) -> SkillAuditSnapshot {
    scan_with_configured(
        &app.workspace,
        codewhale_paths::user_home().as_deref(),
        Some(&app.skills_dir),
        mode.audit_mode(),
        None,
    )
}

fn known_digest(skill: &AuditedSkill) -> Option<String> {
    match &skill.digest {
        DigestState::Known(d) => Some(d.clone()),
        DigestState::Unknown(_) => None,
    }
}

fn scope_label(scope: SkillTargetScope) -> &'static str {
    match scope {
        SkillTargetScope::Project => "project",
        SkillTargetScope::Global => "global",
    }
}

fn action_label(kind: SkillActionKind) -> &'static str {
    match kind {
        SkillActionKind::Install => "install",
        SkillActionKind::Import => "import",
        SkillActionKind::Update => "update",
        SkillActionKind::Remove => "remove",
        SkillActionKind::Trust => "trust",
    }
}

fn source_label(kind: SkillSourceKind) -> &'static str {
    match kind {
        SkillSourceKind::CodeWhaleManaged => "managed",
        SkillSourceKind::CodeWhaleManual => "manual",
        SkillSourceKind::CompatibleExternal => "external",
        SkillSourceKind::BuiltIn => "built-in",
        SkillSourceKind::ReviewedPluginSnapshot => "plugin",
        SkillSourceKind::RegistryCache => "cache",
    }
}

fn skill_tier_label(skill: &AuditedSkill) -> &'static str {
    if skill.source_kind != SkillSourceKind::BuiltIn {
        return "custom";
    }
    codewhale_skills::bundled_skill_tier(&skill.name)
        .map_or("custom", codewhale_skills::BundledSkillTier::label)
}

fn precedence_label(state: &PrecedenceState) -> String {
    match state {
        PrecedenceState::Active => "active".into(),
        PrecedenceState::ShadowedBy(id) => format!("shadowed:{}", id.canonical_name),
        PrecedenceState::InactiveSource => "inactive".into(),
        PrecedenceState::Unknown => "unknown".into(),
    }
}

fn integrity_label(state: &IntegrityState) -> &'static str {
    match state {
        IntegrityState::Healthy => "healthy",
        IntegrityState::LocalContentDrift => "drift",
        IntegrityState::BrokenManagedInstall => "broken",
        IntegrityState::LegacyMetadataUnknown => "legacy",
        IntegrityState::Unknown => "unknown",
    }
}

fn trust_label(state: &TrustState) -> &'static str {
    match state {
        TrustState::TrustedForDigest(_) => "trusted",
        TrustState::TrustStale => "stale",
        TrustState::LegacyAdvisory => "legacy",
        TrustState::Untrusted => "untrusted",
        TrustState::NotApplicable => "n/a",
        TrustState::Unknown => "unknown",
    }
}

fn parser_label(state: &ParserState) -> String {
    match state {
        ParserState::Valid => "valid".into(),
        ParserState::Warning(ws) => format!("warning({})", ws.len()),
        ParserState::Broken(msg) => format!("broken:{msg}"),
        ParserState::Oversized => "oversized".into(),
    }
}

fn digest_label(state: &DigestState) -> String {
    match state {
        DigestState::Known(d) => {
            if d.len() > 12 {
                format!("{}…", &d[..12])
            } else {
                d.clone()
            }
        }
        DigestState::Unknown(reason) => format!("unknown:{reason:?}"),
    }
}

fn provenance_label(state: &ProvenanceState) -> String {
    match state {
        ProvenanceState::Managed { spec, .. } => spec.clone().unwrap_or_else(|| "managed".into()),
        ProvenanceState::Manual => "manual".into(),
        ProvenanceState::External => "external".into(),
        ProvenanceState::BuiltIn => "built-in".into(),
        ProvenanceState::Plugin => "plugin".into(),
        ProvenanceState::Cache => "cache".into(),
        ProvenanceState::Unknown => "unknown".into(),
    }
}

fn kv_line(key: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{key:<11}"),
            Style::default().fg(palette::TEXT_MUTED),
        ),
        Span::raw(value.to_string()),
    ])
}

impl ModalView for SkillsManagerView {
    fn kind(&self) -> ModalKind {
        ModalKind::SkillsManager
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        if self.pending.is_some() {
            return match key.code {
                KeyCode::Esc => {
                    self.pending = None;
                    self.status = Some("Cancelled.".into());
                    ViewAction::None
                }
                KeyCode::Enter => self.confirm_pending(),
                _ => ViewAction::None,
            };
        }

        match key.code {
            KeyCode::Esc => ViewAction::Close,
            KeyCode::Char('q') | KeyCode::Char('Q') if key.modifiers.is_empty() => {
                ViewAction::Close
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => {
                self.move_sel(-1);
                ViewAction::None
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => {
                self.move_sel(1);
                ViewAction::None
            }
            KeyCode::PageUp => {
                self.detail_scroll = self.detail_scroll.saturating_sub(8);
                ViewAction::None
            }
            KeyCode::PageDown => {
                self.detail_scroll = self.detail_scroll.saturating_add(8);
                ViewAction::None
            }
            KeyCode::Home => {
                self.detail_scroll = 0;
                ViewAction::None
            }
            KeyCode::Enter => self.confirm_pending(),
            KeyCode::Char('i') | KeyCode::Char('I') if key.modifiers.is_empty() => {
                self.emit_action(SkillActionKind::Import)
            }
            KeyCode::Char('u') | KeyCode::Char('U') if key.modifiers.is_empty() => {
                self.emit_action(SkillActionKind::Update)
            }
            KeyCode::Char('r') | KeyCode::Char('R') if key.modifiers.is_empty() => {
                self.emit_action(SkillActionKind::Remove)
            }
            KeyCode::Char('t') | KeyCode::Char('T') if key.modifiers.is_empty() => {
                self.emit_action(SkillActionKind::Trust)
            }
            KeyCode::Char('c') | KeyCode::Char('C') if key.modifiers.is_empty() => {
                ViewAction::Emit(ViewEvent::SkillsManagerToggleCompatible)
            }
            KeyCode::Char('s') | KeyCode::Char('S') if key.modifiers.is_empty() => {
                self.cycle_import_scope();
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let body = render_underwater_surface(area, buf, "Skills Manager");
        let hints = self.footer_hints();
        let content = render_modal_footer(body, buf, &hints);

        let header_h = 2u16.min(content.height);
        let header = Rect {
            x: content.x,
            y: content.y,
            width: content.width,
            height: header_h,
        };
        let mode_line = format!(
            "  scan={}   import-target={}   {}",
            self.mode.label(),
            scope_label(self.import_scope),
            self.status.as_deref().unwrap_or("idle")
        );
        buf.set_stringn(
            header.x,
            header.y,
            truncate_view_text(&mode_line, usize::from(header.width)),
            usize::from(header.width),
            Style::default().fg(palette::TEXT_SECONDARY),
        );

        let panel = Rect {
            x: content.x,
            y: content.y.saturating_add(header_h),
            width: content.width,
            height: content.height.saturating_sub(header_h),
        };
        let layout = ListDetailLayout::split(panel, 34);
        self.render_list(layout.list, buf);
        self.render_detail(layout.detail, buf);
    }
}

/// Apply scan-mode toggle using the live app workspace (host-driven).
pub fn apply_toggle_compatible(view: &mut SkillsManagerView, app: &App) {
    view.toggle_mode(app);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{App, TuiOptions};
    use codewhale_runtime::config::Config;
    use crossterm::event::{KeyEventKind, KeyModifiers};
    use std::fs;
    use tempfile::TempDir;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    struct IsolatedHome {
        _home: crate::test_support::EnvVarGuard,
        _userprofile: crate::test_support::EnvVarGuard,
        _codewhale_home: crate::test_support::EnvVarGuard,
        _lock: crate::test_support::TestEnvLock,
    }

    impl IsolatedHome {
        fn new(tmpdir: &TempDir) -> Self {
            let lock = crate::test_support::lock_test_env();
            let home = tmpdir.path().join("home");
            fs::create_dir_all(&home).unwrap();
            Self {
                _home: crate::test_support::EnvVarGuard::set("HOME", &home),
                _userprofile: crate::test_support::EnvVarGuard::set("USERPROFILE", &home),
                _codewhale_home: crate::test_support::EnvVarGuard::set(
                    "CODEWHALE_HOME",
                    home.join(".codewhale"),
                ),
                _lock: lock,
            }
        }
    }

    fn app_in(tmp: &TempDir) -> App {
        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace).unwrap();
        let options = TuiOptions {
            skills_dir: tmp.path().join("skills"),
            memory_path: tmp.path().join("memory.md"),
            notes_path: tmp.path().join("notes.txt"),
            mcp_config_path: tmp.path().join("mcp.json"),
            ..crate::tui::test_support::test_tui_options(workspace)
        };
        App::new(options, &Config::default())
    }

    #[test]
    fn opens_owned_scan_and_closes_on_esc() {
        let tmp = TempDir::new().unwrap();
        let _home = IsolatedHome::new(&tmp);
        let app = app_in(&tmp);
        let mut view = SkillsManagerView::new(&app);
        assert_eq!(view.mode, ManagerMode::OwnedOnly);
        assert_eq!(view.kind(), ModalKind::SkillsManager);
        assert!(matches!(
            view.handle_key(key(KeyCode::Esc)),
            ViewAction::Close
        ));
    }

    #[test]
    fn remove_requires_confirm_then_emits() {
        let tmp = TempDir::new().unwrap();
        let _home = IsolatedHome::new(&tmp);
        let workspace = tmp.path().join("ws");
        let skill_dir = workspace.join(".codewhale").join("skills").join("demo");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo\ndescription: d\n---\nbody\n",
        )
        .unwrap();
        let digest = codewhale_skills::audit::compute_package_digest(&skill_dir).unwrap();
        codewhale_skill_install::install::write_installed_from_v2(
            &skill_dir,
            "github:o/r",
            None,
            "src",
            &digest,
            "demo",
        )
        .unwrap();

        let mut app = app_in(&tmp);
        app.workspace = workspace;
        let mut view = SkillsManagerView::new(&app);
        assert!(!view.skills.is_empty());
        let action = view.handle_key(key(KeyCode::Char('r')));
        assert!(matches!(action, ViewAction::None));
        assert!(view.pending.is_some());
        let action = view.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            action,
            ViewAction::Emit(ViewEvent::SkillMutationRequested {
                request: SkillMutationRequest::Remove { .. },
            })
        ));
    }

    #[test]
    fn render_fits_narrow_terminal() {
        let tmp = TempDir::new().unwrap();
        let _home = IsolatedHome::new(&tmp);
        let app = app_in(&tmp);
        let view = SkillsManagerView::new(&app);
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let mut found = false;
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push(
                    buf.cell((x, y))
                        .map(|c| c.symbol().chars().next().unwrap_or(' '))
                        .unwrap_or(' '),
                );
            }
            if row.contains("Skills") {
                found = true;
                break;
            }
        }
        assert!(found, "expected Skills title on 80x24 surface");
    }

    #[test]
    fn selected_skill_row_uses_charter_pointer_and_selection_bg() {
        let tmp = TempDir::new().unwrap();
        let _home = IsolatedHome::new(&tmp);
        let mut app = app_in(&tmp);
        app.skills_dir = tmp.path().join("home").join(".codewhale").join("skills");
        codewhale_skills::install_system_skills(&app.skills_dir, &app.skills_config.bundled)
            .unwrap();

        let view = SkillsManagerView::new(&app);
        assert!(!view.skills.is_empty());
        let area = Rect::new(0, 0, 100, 24);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        let selected_row_painted = area
            .positions()
            .any(|position| buf[position].bg == palette::SELECTION_BG);
        assert!(
            selected_row_painted,
            "selected skill row must use the shared selection highlight"
        );
        let text = area
            .positions()
            .map(|position| buf[position].symbol())
            .collect::<String>();
        assert!(
            text.contains(codewhale_tui_kit::glyphs::SELECTION),
            "selected skill row must carry the charter pointer: {text}"
        );
    }

    #[test]
    fn manager_labels_exact_built_ins_by_tier_and_user_skills_as_custom() {
        let tmp = TempDir::new().unwrap();
        let _home = IsolatedHome::new(&tmp);
        let mut app = app_in(&tmp);
        app.skills_dir = tmp.path().join("home").join(".codewhale").join("skills");
        codewhale_skills::install_system_skills(&app.skills_dir, &app.skills_config.bundled)
            .unwrap();
        write_skill_pkg(&app.skills_dir.join("my-workflow"), "my-workflow", "mine");

        let view = SkillsManagerView::new(&app);
        let best = view
            .skills
            .iter()
            .find(|skill| skill.name == "best-of-n")
            .expect("best-of-n row");
        let pdf = view
            .skills
            .iter()
            .find(|skill| skill.name == "pdf")
            .expect("pdf row");
        let custom = view
            .skills
            .iter()
            .find(|skill| skill.name == "my-workflow")
            .expect("custom row");

        assert_eq!(skill_tier_label(best), "core");
        assert_eq!(skill_tier_label(pdf), "tools");
        assert_eq!(skill_tier_label(custom), "custom");
    }

    #[test]
    fn import_replace_confirm_is_scoped_to_import_target() {
        let tmp = TempDir::new().unwrap();
        let _home = IsolatedHome::new(&tmp);
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();

        // Project-owned conflict only — Global import must not prompt replace.
        write_skill_pkg(
            &workspace.join(".codewhale").join("skills").join("shared"),
            "shared",
            "owned-project",
        );
        write_skill_pkg(
            &workspace.join(".claude").join("skills").join("shared"),
            "shared",
            "external-different",
        );

        let mut app = app_in(&tmp);
        app.workspace = workspace;
        // Force HOME for scan so global root is the isolated home.
        let mut view = SkillsManagerView::from_scan(
            &app,
            ManagerMode::Compatible,
            SkillTargetScope::Global,
            None,
            None,
        );
        // Select the external row.
        let ext_idx = view
            .skills
            .iter()
            .position(|s| s.source_kind == SkillSourceKind::CompatibleExternal)
            .expect("external skill");
        view.selected = ext_idx;

        let action = view.handle_key(key(KeyCode::Char('i')));
        assert!(
            matches!(
                action,
                ViewAction::Emit(ViewEvent::SkillMutationRequested {
                    request: SkillMutationRequest::ImportExternal {
                        conflict_policy: ConflictPolicy::Reject,
                        ..
                    },
                })
            ),
            "global import must not treat project-owned peer as replace: {action:?}"
        );
        assert!(view.pending.is_none());

        view.import_scope = SkillTargetScope::Project;
        let action = view.handle_key(key(KeyCode::Char('i')));
        assert!(matches!(action, ViewAction::None));
        assert!(
            matches!(view.pending, Some(PendingConfirm::ImportReplace { .. })),
            "project import should confirm replace against project-owned peer"
        );
        assert!(home.exists());
    }

    #[test]
    fn compatible_scan_includes_configured_skills_dir() {
        let tmp = TempDir::new().unwrap();
        let _home = IsolatedHome::new(&tmp);
        let workspace = tmp.path().join("ws");
        let configured = tmp.path().join("custom-skills");
        write_skill_pkg(&configured.join("custom-one"), "custom-one", "from-config");

        let mut app = app_in(&tmp);
        app.workspace = workspace;
        app.skills_dir = configured;

        let view = SkillsManagerView::from_scan(
            &app,
            ManagerMode::Compatible,
            SkillTargetScope::Global,
            None,
            None,
        );
        assert!(
            view.skills.iter().any(|s| s.name == "custom-one"),
            "configured skills_dir rows must appear in compatible scan"
        );
    }

    fn write_skill_pkg(dir: &std::path::Path, name: &str, body: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: d\n---\n{body}\n"),
        )
        .unwrap();
    }
}
