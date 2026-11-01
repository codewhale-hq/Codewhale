//! Command palette modal for quick command/skill insertion.
//!
//! Product job (#4276): **find and run one action** — not a dense manual.
//! Help owns concepts; Config owns settings; Fleet owns worker readiness.

use std::cell::{Cell, RefCell};
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, Widget},
};

use crate::commands;
use crate::tui::views::{
    ActionHint, CommandPaletteAction, ModalKind, ModalView, ViewAction, ViewEvent,
    centered_modal_area, render_modal_footer, render_modal_surface,
};
use codewhale_engine::tools::spec::ApprovalRequirement;
use codewhale_engine::tools::spec::ToolCapability;
use codewhale_engine::tools::{ToolContext, ToolRegistryBuilder};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_skills as skills;
use codewhale_tui_kit::menu_style;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PaletteSection {
    Action,
    Command,
    Skill,
    Tool,
    Mcp,
}

#[derive(Debug, Clone)]
pub struct CommandPaletteEntry {
    section: PaletteSection,
    pub label: String,
    pub description: String,
    pub command: String,
    pub action: CommandPaletteAction,
    show_on_empty_query: bool,
}

#[cfg(test)]
impl CommandPaletteEntry {
    #[must_use]
    pub fn section(&self) -> PaletteSection {
        self.section
    }
}

pub struct CommandPaletteView {
    locale: Locale,
    entries: Vec<CommandPaletteEntry>,
    filtered: Vec<usize>,
    query: String,
    selected: usize,
    /// Entry rows from the most recent render. Keeping the absolute filtered
    /// index here makes mouse activation use the exact same action as Enter.
    row_hitboxes: RefCell<Vec<(Rect, usize)>>,
    /// Absolute filtered index under the pointer, tinted with the shared
    /// hover style. Hover never moves the keyboard selection.
    hovered: Cell<Option<usize>>,
}

#[cfg(test)]
pub fn build_entries(
    locale: Locale,
    skills_dir: &Path,
    skills_config: &codewhale_runtime::config::SkillsConfig,
    workspace: &Path,
    mcp_config_path: &Path,
    mcp_snapshot: Option<&codewhale_engine::mcp::McpManagerSnapshot>,
) -> Vec<CommandPaletteEntry> {
    build_entries_with_plugins(
        locale,
        skills_dir,
        skills_config,
        workspace,
        mcp_config_path,
        mcp_snapshot,
        &codewhale_plugins::PluginRegistry::empty(workspace),
    )
}

pub fn build_entries_with_plugins(
    locale: Locale,
    skills_dir: &Path,
    skills_config: &codewhale_runtime::config::SkillsConfig,
    workspace: &Path,
    mcp_config_path: &Path,
    mcp_snapshot: Option<&codewhale_engine::mcp::McpManagerSnapshot>,
    plugins: &codewhale_plugins::PluginRegistry,
) -> Vec<CommandPaletteEntry> {
    let mut entries = Vec::new();
    commands::user_registry::with_registry_for_plugins(plugins, |user_registry| {
        let all_user_commands = user_registry.iter().collect::<Vec<_>>();
        for command in commands::command_infos() {
            if command.is_unlisted() {
                continue;
            }
            if commands::discovery::user_command_shadows_builtin_canonical(
                command,
                &all_user_commands,
            ) {
                continue;
            }
            let mut description =
                palette_description_for_unshadowed_aliases(command, locale, &all_user_commands);
            if command.requires_argument() {
                description.push_str("  ");
                description.push_str(command.usage);
            }
            let action = if command.palette_runs_directly() {
                CommandPaletteAction::ExecuteCommand {
                    command: format!("/{}", command.name),
                }
            } else {
                CommandPaletteAction::InsertText {
                    text: command.palette_command(),
                }
            };
            entries.push(CommandPaletteEntry {
                section: PaletteSection::Command,
                label: format!("/{}", command.name),
                description,
                command: command.palette_command(),
                action,
                show_on_empty_query: command.show_in_empty_discovery(),
            });
        }

        for command in user_registry.iter().filter(|command| !command.hidden) {
            let mut description = command
                .description
                .clone()
                .unwrap_or_else(|| "User-defined command".to_string());
            if let Some(hint) = command.display_usage() {
                description.push_str("  ");
                description.push_str(hint);
            }
            let slash_command = format!("/{}", command.name);
            let action = if command.takes_arguments() {
                CommandPaletteAction::InsertText {
                    text: format!("{slash_command} "),
                }
            } else {
                CommandPaletteAction::ExecuteCommand {
                    command: slash_command.clone(),
                }
            };
            entries.push(CommandPaletteEntry {
                section: PaletteSection::Command,
                label: slash_command.clone(),
                description,
                command: slash_command,
                action,
                show_on_empty_query: true,
            });
        }
    });

    let skills =
        skills::discover_for_workspace(workspace, Some(skills_dir), skills_config, Some(plugins))
            .into_enabled();
    for skill in skills
        .list()
        .iter()
        .filter(|skill| skill.invocation.user_invocable())
    {
        entries.push(CommandPaletteEntry {
            section: PaletteSection::Skill,
            label: format!("${}", skill.name),
            description: skill.user_menu_description(),
            command: format!("${}", skill.name),
            action: CommandPaletteAction::ExecuteCommand {
                command: format!("${}", skill.name),
            },
            show_on_empty_query: true,
        });
    }

    let context = ToolContext::new(workspace);
    let registry = ToolRegistryBuilder::new()
        .with_file_tools()
        .with_search_tools()
        .with_shell_tools()
        .with_web_tools()
        .with_git_tools()
        .with_user_input_tool(codewhale_engine::tools::user_input::UserInputLimits::default())
        .with_patch_tools()
        .with_note_tool()
        .with_diagnostics_tool()
        .with_project_tools()
        .with_test_runner_tool()
        .build(context);

    let mut tool_entries = registry
        .all()
        .into_iter()
        .filter_map(|tool| {
            let name = tool.name().to_string();
            let capabilities = tool.capabilities();

            let mut tags = Vec::new();
            if tool.is_read_only() {
                tags.push("read-only");
            }
            if capabilities.contains(&ToolCapability::WritesFiles) {
                tags.push("writes");
            }
            if capabilities.contains(&ToolCapability::ExecutesCode) {
                tags.push("shell");
            }
            if capabilities.contains(&ToolCapability::Network) {
                tags.push("network");
            }
            if tool.supports_parallel() {
                tags.push("parallel");
            }
            match tool.approval_requirement() {
                ApprovalRequirement::Required => tags.push("requires approval"),
                ApprovalRequirement::Suggest => tags.push("suggest approval"),
                ApprovalRequirement::Auto => {}
            }

            let mut description = tool.description().to_string();
            if !tags.is_empty() {
                description.push_str(" [");
                description.push_str(&tags.join(", "));
                description.push(']');
            }

            if name.trim().is_empty() {
                return None;
            }
            Some(CommandPaletteEntry {
                section: PaletteSection::Tool,
                label: format!("tool:{name}"),
                description: description.clone(),
                command: name,
                action: CommandPaletteAction::OpenTextPager {
                    title: format!("Tool: {}", tool.name()),
                    content: format_tool_details(tool.name(), tool.description(), &tags),
                },
                show_on_empty_query: true,
            })
        })
        .collect::<Vec<_>>();
    tool_entries.sort_by(|a, b| a.label.cmp(&b.label));
    entries.extend(tool_entries);

    entries.extend(build_mcp_entries(
        workspace,
        mcp_config_path,
        mcp_snapshot,
        plugins,
    ));

    entries.sort_by(|a, b| a.label.cmp(&b.label));
    entries.sort_by_key(|entry| entry.section);
    entries
}

fn palette_description_for_unshadowed_aliases(
    command: &commands::CommandInfo,
    locale: Locale,
    all_user_commands: &[&commands::user_registry::UserCommandMetadata],
) -> String {
    let desc = command.description_for(locale);
    let aliases = commands::discovery::unshadowed_builtin_aliases(command, all_user_commands);
    if aliases.len() == command.aliases.len() {
        return command.palette_description_for(locale);
    }
    if aliases.is_empty() {
        desc.to_string()
    } else {
        format!("{}  aliases: {}", desc, aliases.join(", "))
    }
}

fn build_mcp_entries(
    workspace: &Path,
    mcp_config_path: &Path,
    mcp_snapshot: Option<&codewhale_engine::mcp::McpManagerSnapshot>,
    plugins: &codewhale_plugins::PluginRegistry,
) -> Vec<CommandPaletteEntry> {
    let owned_snapshot = if mcp_snapshot.is_none() {
        codewhale_engine::mcp::manager_snapshot_from_config_with_workspace_and_plugins(
            mcp_config_path,
            workspace,
            false,
            plugins,
        )
        .ok()
    } else {
        None
    };
    let snapshot = mcp_snapshot.or(owned_snapshot.as_ref());
    let mut entries = vec![CommandPaletteEntry {
        section: PaletteSection::Mcp,
        label: "mcp:manager".to_string(),
        description: format!("Open MCP manager ({})", mcp_config_path.display()),
        command: "/mcp".to_string(),
        action: CommandPaletteAction::ExecuteCommand {
            command: "/mcp".to_string(),
        },
        show_on_empty_query: true,
    }];

    let Some(snapshot) = snapshot else {
        return entries;
    };

    for server in &snapshot.servers {
        let state = if server.enabled {
            if server.connected {
                "connected"
            } else if server.error.is_some() {
                "failed"
            } else {
                "enabled"
            }
        } else {
            "disabled"
        };
        entries.push(CommandPaletteEntry {
            section: PaletteSection::Mcp,
            label: format!("mcp:{}", server.name),
            description: format!(
                "{} {} [{}] tools={} resources={} prompts={}",
                server.transport,
                codewhale_engine::mcp::mcp_display_target(
                    &server.transport,
                    &server.command_or_url
                ),
                state,
                server.tools.len(),
                server.resources.len(),
                server.prompts.len()
            ),
            command: format!("/mcp show {}", server.name),
            action: CommandPaletteAction::OpenTextPager {
                title: format!("MCP Server: {}", server.name),
                content: format_mcp_server_details(snapshot, server),
            },
            show_on_empty_query: true,
        });

        for tool in &server.tools {
            entries.push(CommandPaletteEntry {
                section: PaletteSection::Mcp,
                label: format!("mcp:{}:tool:{}", server.name, tool.name),
                description: format!(
                    "{}{}",
                    tool.model_name,
                    tool.description
                        .as_ref()
                        .map_or(String::new(), |desc| format!(" - {desc}"))
                ),
                command: tool.model_name.clone(),
                action: CommandPaletteAction::OpenTextPager {
                    title: format!("MCP Tool: {}", tool.model_name),
                    content: format!(
                        "Server: {}\nRuntime name: {}\nKind: tool\n\n{}",
                        server.name,
                        tool.model_name,
                        tool.description.as_deref().unwrap_or("(no description)")
                    ),
                },
                show_on_empty_query: true,
            });
            // Add a "use" entry that inserts the tool's model_name into the input
            // so users can quickly reference the tool in their message to the AI.
            if !tool.model_name.trim().is_empty() {
                entries.push(CommandPaletteEntry {
                    section: PaletteSection::Mcp,
                    label: format!("mcp:{}:tool:{} > use", server.name, tool.name),
                    description: format!(
                        "Insert {} into input — type args then send{}",
                        tool.model_name,
                        tool.description
                            .as_ref()
                            .map_or(String::new(), |desc| format!(" ({desc})"))
                    ),
                    command: tool.model_name.clone(),
                    action: CommandPaletteAction::InsertText {
                        text: tool.model_name.clone(),
                    },
                    show_on_empty_query: true,
                });
            }
        }

        for resource in &server.resources {
            entries.push(CommandPaletteEntry {
                section: PaletteSection::Mcp,
                label: format!("mcp:{}:resource:{}", server.name, resource.name),
                description: resource
                    .description
                    .clone()
                    .unwrap_or_else(|| "MCP resource".to_string()),
                command: resource.name.clone(),
                action: CommandPaletteAction::OpenTextPager {
                    title: format!("MCP Resource: {}", resource.name),
                    content: format!(
                        "Server: {}\nResource: {}\nModel helper: list_mcp_resources / read_mcp_resource",
                        server.name, resource.name
                    ),
                },
                show_on_empty_query: true,
            });
        }

        for prompt in &server.prompts {
            entries.push(CommandPaletteEntry {
                section: PaletteSection::Mcp,
                label: format!("mcp:{}:prompt:{}", server.name, prompt.name),
                description: format!(
                    "{}{}",
                    prompt.model_name,
                    prompt
                        .description
                        .as_ref()
                        .map_or(String::new(), |desc| format!(" - {desc}"))
                ),
                command: prompt.model_name.clone(),
                action: CommandPaletteAction::OpenTextPager {
                    title: format!("MCP Prompt: {}", prompt.model_name),
                    content: format!(
                        "Server: {}\nRuntime name: {}\nKind: prompt",
                        server.name, prompt.model_name
                    ),
                },
                show_on_empty_query: true,
            });
        }
    }

    entries
}

fn format_mcp_server_details(
    snapshot: &codewhale_engine::mcp::McpManagerSnapshot,
    server: &codewhale_engine::mcp::McpServerSnapshot,
) -> String {
    let mut lines = vec![
        format!("Config: {}", snapshot.config_path.display()),
        format!("Server: {}", server.name),
        format!("Enabled: {}", server.enabled),
        format!("Connected: {}", server.connected),
        format!("Transport: {}", server.transport),
        format!(
            "Target: {}",
            codewhale_engine::mcp::mcp_display_target(&server.transport, &server.command_or_url)
        ),
        format!(
            "Timeouts: connect={}s execute={}s read={}s",
            server.connect_timeout, server.execute_timeout, server.read_timeout
        ),
    ];
    if let Some(error) = server.error.as_ref() {
        lines.push(format!("Error: {error}"));
    }
    lines.push(String::new());
    lines.push(format!("Tools ({})", server.tools.len()));
    for tool in &server.tools {
        lines.push(format!("  - {}", tool.model_name));
    }
    lines.push(format!("Resources ({})", server.resources.len()));
    for resource in &server.resources {
        lines.push(format!("  - {}", resource.name));
    }
    lines.push(format!("Prompts ({})", server.prompts.len()));
    for prompt in &server.prompts {
        lines.push(format!("  - {}", prompt.model_name));
    }
    lines.join("\n")
}

fn modal_block() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette::BORDER_COLOR))
        .style(Style::default().bg(palette::WHALE_BG))
        .padding(Padding::uniform(1))
}

fn parse_section_term(term: &str) -> Option<(PaletteSection, String)> {
    let (section, query) = term.split_once(':')?;

    if section.is_empty() || query.is_empty() {
        return None;
    }

    let query = query.to_ascii_lowercase();
    let section = match section {
        "a" | "action" | "actions" => PaletteSection::Action,
        "c" | "cmd" | "command" | "commands" => PaletteSection::Command,
        "s" | "skill" | "skills" => PaletteSection::Skill,
        "t" | "tool" | "tools" => PaletteSection::Tool,
        "m" | "mcp" => PaletteSection::Mcp,
        _ => return None,
    };

    Some((section, query))
}

fn section_tag(section: PaletteSection) -> &'static str {
    match section {
        PaletteSection::Action => "action",
        PaletteSection::Command => "command",
        PaletteSection::Skill => "skill",
        PaletteSection::Tool => "tool",
        PaletteSection::Mcp => "mcp",
    }
}

fn section_rank(section: PaletteSection) -> usize {
    match section {
        PaletteSection::Action => 0,
        PaletteSection::Command => 1,
        PaletteSection::Skill => 2,
        PaletteSection::Tool => 3,
        PaletteSection::Mcp => 4,
    }
}

fn format_tool_details(name: &str, description: &str, tags: &[&str]) -> String {
    let mut lines = vec![
        format!("Tool: {name}"),
        String::new(),
        description.to_string(),
    ];
    if !tags.is_empty() {
        lines.push(String::new());
        lines.push(format!("Capabilities: {}", tags.join(", ")));
    }
    lines.push(String::new());
    lines.push(
        "Use slash commands and skills here for direct actions; use tool entries to inspect what the agent can call."
            .to_string(),
    );
    lines.join("\n")
}

fn term_score(term: &str, label: &str, description: &str, command: &str, haystack: &str) -> usize {
    if term.is_empty() {
        return 0;
    }

    if label == term || command == term || description == term {
        return 0;
    }

    if label.starts_with(term) {
        return 8;
    }

    if command.starts_with(term) {
        return 16;
    }

    if description.contains(term) {
        return 64;
    }

    if label.contains(term) {
        return 32;
    }

    if command.contains(term) {
        return 48;
    }

    if haystack.contains(term) {
        return 96;
    }

    128
}

fn entry_match_score(entry: &CommandPaletteEntry, terms: &[&str]) -> Option<usize> {
    if terms.is_empty() {
        return Some(0);
    }

    let section = section_tag(entry.section);
    let label = entry.label.to_ascii_lowercase();
    let description = entry.description.to_ascii_lowercase();
    let command = entry.command.to_ascii_lowercase();
    let entry_text = format!("{section} {label} {description} {command}");

    let mut total_score = 0usize;

    for term in terms {
        if let Some((required_section, scoped_query)) = parse_section_term(term) {
            if entry.section != required_section {
                return None;
            }
            if !entry_text.contains(&scoped_query) {
                return None;
            }
            total_score += term_score(&scoped_query, &label, &description, &command, &entry_text);
            continue;
        }

        if !entry_text.contains(term) {
            return None;
        }
        total_score += term_score(term, &label, &description, &command, &entry_text);
    }

    Some(total_score)
}

/// Number of rendered rows the entry loop consumes for the window
/// `sections[start..end]`: one row per entry, plus one section-label row each
/// time the section changes, plus a separator blank before every section group
/// after the first.
fn rendered_entry_rows(sections: &[PaletteSection], start: usize, end: usize) -> usize {
    let end = end.min(sections.len());
    if start >= end {
        return 0;
    }
    let mut rows = 0usize;
    let mut active: Option<PaletteSection> = None;
    for (slot, sec) in sections[start..end].iter().enumerate() {
        if active != Some(*sec) {
            if slot > 0 {
                rows += 1; // separator blank
            }
            rows += 1; // section label
            active = Some(*sec);
        }
        rows += 1; // the entry itself
    }
    rows
}

/// Compute the `[start, end)` window of filtered entries to render so that the
/// selected entry is always visible and the rendered rows — entries plus the
/// per-section labels and separators inserted between them — fit within
/// `available` rows.
///
/// The previous logic sized the window purely by entry count (`popup_height -
/// 7`) while the same fixed-height area also held the header, section labels,
/// and separators. Those uncounted rows pushed the selection past the bottom
/// clip line, so it vanished and the list appeared frozen until the index
/// finally exceeded the (overlarge) entry budget (#2590).
fn visible_entry_window(
    sections: &[PaletteSection],
    selected: usize,
    available: usize,
) -> (usize, usize) {
    let total = sections.len();
    if total == 0 || available == 0 {
        return (0, 0);
    }
    let selected = selected.min(total - 1);
    // Always include the selected row, then greedily grow downward and upward
    // while the fully-rendered window still fits. Growth only ever adds rows,
    // so the greedy expansion terminates at the largest fitting window.
    let mut start = selected;
    let mut end = selected + 1;
    loop {
        let mut progressed = false;
        if end < total && rendered_entry_rows(sections, start, end + 1) <= available {
            end += 1;
            progressed = true;
        }
        if start > 0 && rendered_entry_rows(sections, start - 1, end) <= available {
            start -= 1;
            progressed = true;
        }
        if !progressed {
            break;
        }
    }
    (start, end)
}

impl CommandPaletteView {
    #[cfg(test)]
    pub fn new(entries: Vec<CommandPaletteEntry>) -> Self {
        Self::new_for_locale(Locale::En, entries)
    }

    pub fn new_for_locale(locale: Locale, entries: Vec<CommandPaletteEntry>) -> Self {
        let mut view = Self {
            locale,
            entries,
            filtered: Vec::new(),
            query: String::new(),
            selected: 0,
            row_hitboxes: RefCell::new(Vec::new()),
            hovered: Cell::new(None),
        };
        view.refilter();
        view
    }

    fn refilter(&mut self) {
        let query = self.query.trim().to_ascii_lowercase();
        let terms: Vec<&str> = query
            .split_whitespace()
            .filter(|term| !term.is_empty())
            .collect();

        let mut filtered = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(idx, entry)| {
                if terms.is_empty() && !entry.show_on_empty_query {
                    return None;
                }
                entry_match_score(entry, &terms).map(|score| (idx, score))
            })
            .collect::<Vec<_>>();

        filtered.sort_by_key(|(idx, score)| {
            let entry = &self.entries[*idx];
            (section_rank(entry.section), *score, &entry.label)
        });
        // Follow the highlighted entry across the refilter instead of leaving a
        // raw index pointing into a freshly re-sorted list. Every keystroke
        // refilters, so a clamp alone silently slides the highlight onto an
        // unrelated row — and Enter runs whatever it landed on. `filtered` holds
        // indices into the stable `entries`, so the entry is its own identity.
        let keep = self.filtered.get(self.selected).copied();
        self.filtered = filtered.into_iter().map(|(idx, _)| idx).collect();
        self.selected = keep
            .and_then(|entry| self.filtered.iter().position(|idx| *idx == entry))
            .unwrap_or(0);
        self.hovered.set(None);
    }

    fn scope_hint_lines() -> Line<'static> {
        let hint = "scope: c:cmd · s:skill · t:tool · m:mcp";
        Line::from(Span::styled(
            hint,
            Style::default()
                .fg(palette::TEXT_DIM)
                .add_modifier(Modifier::ITALIC),
        ))
    }

    fn format_section_label(section: PaletteSection, count: usize) -> Line<'static> {
        let title = match section {
            PaletteSection::Action => "Actions",
            PaletteSection::Command => "Commands",
            PaletteSection::Skill => "Skills",
            PaletteSection::Tool => "Tools",
            PaletteSection::Mcp => "MCP",
        };
        Line::from(vec![Span::styled(
            format!("  {title} ({count})  "),
            Style::default()
                .fg(palette::WHALE_ACTION)
                .add_modifier(Modifier::BOLD),
        )])
    }

    fn move_selection(&mut self, delta: isize) {
        self.selected =
            codewhale_tui_kit::list_nav::wrap_index(self.selected, self.filtered.len(), delta);
        self.hovered.set(None);
    }

    fn selected_entry(&self) -> Option<&CommandPaletteEntry> {
        self.filtered
            .get(self.selected)
            .and_then(|idx| self.entries.get(*idx))
    }
}

impl ModalView for CommandPaletteView {
    fn kind(&self) -> ModalKind {
        ModalKind::CommandPalette
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        match mouse.kind {
            MouseEventKind::Moved => {
                let hovered = self.row_hitboxes.borrow().iter().find_map(|(rect, index)| {
                    rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                        .then_some(*index)
                });
                self.hovered.set(hovered);
            }
            MouseEventKind::ScrollUp => self.move_selection(-1),
            MouseEventKind::ScrollDown => self.move_selection(1),
            MouseEventKind::Down(MouseButton::Left) => {
                let clicked = self.row_hitboxes.borrow().iter().find_map(|(rect, index)| {
                    rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                        .then_some(*index)
                });
                if let Some(index) = clicked {
                    if self.selected == index {
                        if let Some(entry) = self.selected_entry() {
                            return ViewAction::EmitAndClose(ViewEvent::CommandPaletteSelected {
                                action: entry.action.clone(),
                            });
                        }
                    } else {
                        self.selected = index;
                    }
                }
            }
            _ => {}
        }
        ViewAction::None
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        match key.code {
            KeyCode::Esc => ViewAction::Close,
            KeyCode::Enter => {
                if let Some(entry) = self.selected_entry() {
                    ViewAction::EmitAndClose(ViewEvent::CommandPaletteSelected {
                        action: entry.action.clone(),
                    })
                } else {
                    ViewAction::None
                }
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
                self.move_selection(-8);
                ViewAction::None
            }
            KeyCode::PageDown => {
                self.move_selection(8);
                ViewAction::None
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.refilter();
                ViewAction::None
            }
            // Ctrl+H is the legacy ASCII backspace many terminals emit.
            KeyCode::Char('h')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.query.pop();
                self.refilter();
                ViewAction::None
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.query.push(c);
                self.refilter();
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.row_hitboxes.borrow_mut().clear();
        let popup_area = centered_modal_area(area, 90, 22, 44, 8);
        let popup_width = popup_area.width;

        render_modal_surface(area, popup_area, buf);

        let title = format!(
            " {} — {} ",
            tr(self.locale, MessageId::CommandPaletteTitle),
            tr(self.locale, MessageId::CommandPaletteSubtitle)
        );
        let block = modal_block().title(Line::from(Span::styled(
            title,
            Style::default()
                .fg(palette::WHALE_ACTION)
                .add_modifier(Modifier::BOLD),
        )));
        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let content = render_modal_footer(
            inner,
            buf,
            &[
                ActionHint::new("↑/↓", "move"),
                ActionHint::new("Enter", "select"),
                ActionHint::new("Esc", "cancel"),
            ],
        );

        let mut lines = Vec::new();
        let mut entry_line_indices = Vec::new();
        let query_label = if self.query.is_empty() {
            "Type to filter".to_string()
        } else {
            format!("Filter: {}", self.query)
        };
        lines.push(Line::from(Span::styled(
            query_label,
            Style::default().fg(palette::TEXT_MUTED),
        )));
        let match_count = if self.query.is_empty() {
            format!(
                "{} shown / {} entries",
                self.filtered.len(),
                self.entries.len()
            )
        } else {
            format!("{} / {} matches", self.filtered.len(), self.entries.len())
        };
        lines.push(Line::from(Span::styled(
            match_count,
            Style::default().fg(palette::TEXT_DIM).italic(),
        )));
        lines.push(Self::scope_hint_lines());
        lines.push(Line::from(""));

        // Rows the bordered popup can show for the list, minus the header that
        // was already pushed above. The entry loop additionally emits section
        // labels and separators, so the scroll window is sized against the real
        // rendered cost rather than a flat entry count (#2590).
        let header_lines = lines.len();
        let available = (content.height as usize).saturating_sub(header_lines);
        let mut action_count = 0usize;
        let mut command_count = 0usize;
        let mut skill_count = 0usize;
        let mut tool_count = 0usize;
        let mut mcp_count = 0usize;
        for idx in &self.filtered {
            match self.entries[*idx].section {
                PaletteSection::Action => action_count += 1,
                PaletteSection::Command => command_count += 1,
                PaletteSection::Skill => skill_count += 1,
                PaletteSection::Tool => tool_count += 1,
                PaletteSection::Mcp => mcp_count += 1,
            }
        }
        if self.filtered.is_empty() {
            lines.push(Line::from(Span::styled(
                "No matches",
                Style::default().fg(palette::TEXT_MUTED).italic(),
            )));
        } else {
            let label_width = 24.min(popup_width.saturating_sub(26) as usize);
            let sections: Vec<PaletteSection> = self
                .filtered
                .iter()
                .map(|idx| self.entries[*idx].section)
                .collect();
            let (start, end) = visible_entry_window(&sections, self.selected, available);
            let mut active_section = None;
            for (slot, idx) in self.filtered[start..end].iter().enumerate() {
                let absolute = start + slot;
                let is_selected = absolute == self.selected;
                let entry = &self.entries[*idx];

                if active_section != Some(entry.section) {
                    if slot > 0 {
                        lines.push(Line::from(""));
                    }
                    let count = match entry.section {
                        PaletteSection::Action => action_count,
                        PaletteSection::Command => command_count,
                        PaletteSection::Skill => skill_count,
                        PaletteSection::Tool => tool_count,
                        PaletteSection::Mcp => mcp_count,
                    };
                    lines.push(Self::format_section_label(entry.section, count));
                    active_section = Some(entry.section);
                }

                // Hover tints but never steals the keyboard selection.
                let hovered = !is_selected && self.hovered.get() == Some(absolute);
                let style = if is_selected {
                    menu_style::selected_row_style()
                } else if hovered {
                    Style::default()
                        .fg(palette::TEXT_PRIMARY)
                        .patch(menu_style::hovered_row_style())
                } else {
                    Style::default().fg(palette::TEXT_PRIMARY)
                };

                let pointer = codewhale_tui_kit::glyphs::selection_marker(is_selected);
                // `{:<width$}` pads but never truncates, so a long label — every
                // `mcp:server:tool` row — ran past the column and pushed the
                // description off the card entirely. Truncate first, then pad, so
                // the description column stays on one axis.
                let label =
                    codewhale_tui_kit::ui_text::truncate_line_to_width(&entry.label, label_width);
                let mut line = format!("{pointer} {label:<label_width$}");
                // The rows are drawn into `content`, which is the popup less its
                // borders and padding — measuring against `popup_width` overstated
                // the room by four columns.
                let content_width = (popup_width as usize).saturating_sub(4);
                let desc_capacity = content_width.saturating_sub(label_width + 4);
                let desc = codewhale_tui_kit::ui_text::truncate_line_to_width(
                    &entry.description,
                    desc_capacity,
                );
                line.push_str("  ");
                line.push_str(&desc);
                entry_line_indices.push((lines.len(), absolute));
                lines.push(Line::from(Span::styled(line, style)));
            }
        }

        // The palette's row-budget logic intentionally treats each logical
        // line as one terminal row. Do not wrap here: wrapping a long query or
        // label would both hide later entries and make mouse hitboxes lie.
        Paragraph::new(lines).render(content, buf);
        *self.row_hitboxes.borrow_mut() = entry_line_indices
            .into_iter()
            .filter_map(|(line, index)| {
                let row = content.y.saturating_add(line as u16);
                (row < content.bottom())
                    .then_some((Rect::new(content.x, row, content.width, 1), index))
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn refilter_keeps_the_highlight_on_the_entry_the_user_was_looking_at() {
        // Every keystroke refilters and re-sorts. The index used to be clamped
        // but never re-anchored, so refining a query could slide the highlight
        // onto an unrelated row — and Enter runs whatever is highlighted.
        let entries = vec![
            palette_entry(PaletteSection::Tool, "tool:one", "alpha", "one"),
            palette_entry(PaletteSection::Tool, "tool:two", "shared", "two"),
            palette_entry(PaletteSection::Tool, "tool:three", "shared", "three"),
        ];
        let mut view = CommandPaletteView::new(entries);

        view.query = "tool".to_string();
        view.refilter();
        view.selected = view
            .filtered
            .iter()
            .position(|idx| view.entries[*idx].label == "tool:three")
            .expect("tool:three is listed");

        // Narrowing to a query `tool:three` still matches. It moves to a lower
        // index in the shorter list, which is exactly the case a clamp gets
        // wrong: the old code reset to 0 and highlighted `tool:two`.
        view.query = "shared".to_string();
        view.refilter();
        assert_eq!(
            view.selected_entry().map(|entry| entry.label.as_str()),
            Some("tool:three"),
            "the highlight jumped to another row: {:?}",
            view.selected_entry().map(|entry| entry.label.clone())
        );

        // When the highlighted entry filters out entirely, fall back to the top
        // rather than to a stale index.
        view.query = "alpha".to_string();
        view.refilter();
        assert_eq!(
            view.selected_entry().map(|entry| entry.label.as_str()),
            Some("tool:one")
        );
    }

    #[test]
    fn visible_window_keeps_selection_in_view_and_fits() {
        // Single large section, small budget: every selection must stay visible
        // and the rendered window must fit the available rows (#2590).
        let sections = vec![PaletteSection::Command; 30];
        let available = 10;
        for selected in 0..sections.len() {
            let (start, end) = visible_entry_window(&sections, selected, available);
            assert!(
                start <= selected && selected < end,
                "selected {selected} must lie within [{start}, {end})"
            );
            assert!(
                rendered_entry_rows(&sections, start, end) <= available,
                "window [{start}, {end}) must fit within {available} rows"
            );
        }
    }

    #[test]
    fn visible_window_scrolls_as_selection_advances() {
        let sections = vec![PaletteSection::Command; 30];
        let available = 8;
        let (start_near, _) = visible_entry_window(&sections, 0, available);
        assert_eq!(start_near, 0);
        // A far-down selection must advance the window start — the old code
        // left it pinned at 0 so the selection scrolled off-screen.
        let (start_far, end_far) = visible_entry_window(&sections, 25, available);
        assert!(start_far > 0, "window should scroll for a far selection");
        assert!(start_far <= 25 && 25 < end_far);
    }

    #[test]
    fn visible_window_accounts_for_section_overhead() {
        // Each entry is its own section, so each costs a label (plus a
        // separator after the first) on top of the entry row. Far fewer than
        // `available` entries fit, and the window must still respect the budget.
        let sections = vec![
            PaletteSection::Action,
            PaletteSection::Command,
            PaletteSection::Skill,
            PaletteSection::Tool,
            PaletteSection::Mcp,
        ];
        let available = 6;
        let (start, end) = visible_entry_window(&sections, 0, available);
        assert_eq!(start, 0);
        assert!(end >= 1, "at least the selected entry must render");
        assert!(rendered_entry_rows(&sections, start, end) <= available);
    }

    #[test]
    fn visible_window_handles_empty_and_zero_budget() {
        assert_eq!(visible_entry_window(&[], 0, 10), (0, 0));
        let sections = vec![PaletteSection::Command; 5];
        assert_eq!(visible_entry_window(&sections, 2, 0), (0, 0));
    }

    fn palette_entry(
        section: PaletteSection,
        label: &str,
        description: &str,
        command: &str,
    ) -> CommandPaletteEntry {
        CommandPaletteEntry {
            section,
            label: label.to_string(),
            description: description.to_string(),
            command: command.to_string(),
            action: CommandPaletteAction::InsertText {
                text: command.to_string(),
            },
            show_on_empty_query: true,
        }
    }

    fn assert_palette_search_owns_text(query: &str) {
        let entries = ["json", "key", "队列é"]
            .map(|text| palette_entry(PaletteSection::Command, text, "", text))
            .to_vec();
        let mut stack = crate::tui::views::ViewStack::new();
        stack.push(CommandPaletteView::new(entries));
        for ch in query.chars() {
            assert!(
                stack
                    .handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
                    .is_empty()
            );
            assert_eq!(stack.top_kind(), Some(ModalKind::CommandPalette));
        }
        let mut modal = stack.pop().unwrap();
        let view = modal
            .as_any_mut()
            .downcast_mut::<CommandPaletteView>()
            .unwrap();
        assert_eq!(view.query, query);
        assert_eq!(view.filtered.len(), 1);
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::PageUp,
            KeyCode::PageDown,
        ] {
            assert!(matches!(
                view.handle_key(KeyEvent::new(code, KeyModifiers::NONE)),
                ViewAction::None
            ));
            assert_eq!(view.query, query);
        }
        assert!(matches!(
            view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ViewAction::EmitAndClose(ViewEvent::CommandPaletteSelected {
                action: CommandPaletteAction::InsertText { text }
            }) if text == query
        ));
        assert!(matches!(
            view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ViewAction::Close
        ));
    }

    #[test]
    fn palette_search_owns_initial_j() {
        assert_palette_search_owns_text("json");
    }

    #[test]
    fn palette_search_owns_initial_k() {
        assert_palette_search_owns_text("key");
    }

    #[test]
    fn palette_search_owns_unicode() {
        assert_palette_search_owns_text("队列é");
    }

    #[test]
    fn command_palette_filters_with_section_shortcuts() {
        let entries = vec![
            palette_entry(PaletteSection::Command, "/mode", "mode command", "/mode"),
            palette_entry(
                PaletteSection::Skill,
                "skill:search",
                "search skill",
                "/skill search",
            ),
            palette_entry(PaletteSection::Tool, "tool:git", "git tool", "git"),
            palette_entry(
                PaletteSection::Tool,
                "tool:search",
                "search utility",
                "search",
            ),
            palette_entry(PaletteSection::Mcp, "mcp:fs", "filesystem", "mcp_fs_read"),
        ];
        let mut view = CommandPaletteView::new(entries);

        view.query = "c:mode".to_string();
        view.refilter();
        assert_eq!(view.filtered, vec![0]);

        view.query = "s:search".to_string();
        view.refilter();
        assert_eq!(view.filtered, vec![1]);

        view.query = "t:search".to_string();
        view.refilter();
        assert_eq!(view.filtered, vec![3]);

        view.query = "m:fs".to_string();
        view.refilter();
        assert_eq!(view.filtered, vec![4]);
    }

    #[test]
    fn command_palette_ranks_label_matches_before_description_matches() {
        let entries = vec![
            palette_entry(
                PaletteSection::Command,
                "/git",
                "status summary for repository",
                "git",
            ),
            palette_entry(
                PaletteSection::Command,
                "/config",
                "configure git settings",
                "config",
            ),
            palette_entry(
                PaletteSection::Command,
                "/sync",
                "sync repository state",
                "sync",
            ),
        ];
        let mut view = CommandPaletteView::new(entries);

        view.query = "git".to_string();
        view.refilter();

        assert_eq!(view.entries[view.filtered[0]].label, "/git");
        assert_eq!(view.entries[view.filtered[1]].label, "/config");
    }

    #[test]
    fn command_palette_supports_multiple_terms() {
        let entries = vec![
            palette_entry(
                PaletteSection::Command,
                "/search-code",
                "search with ripgrep",
                "search code",
            ),
            palette_entry(
                PaletteSection::Tool,
                "tool:search",
                "search web and files",
                "search",
            ),
            palette_entry(
                PaletteSection::Skill,
                "skill:search",
                "search files and docs",
                "/skill search",
            ),
        ];
        let mut view = CommandPaletteView::new(entries);

        view.query = "search code".to_string();
        view.refilter();
        assert_eq!(view.filtered.len(), 1);
        assert_eq!(view.entries[view.filtered[0]].label, "/search-code");

        view.query = "s:search".to_string();
        view.refilter();
        assert_eq!(view.filtered.len(), 1);
        assert_eq!(view.entries[view.filtered[0]].label, "skill:search");
    }

    #[test]
    fn command_palette_skills_use_workspace_and_configured_directories() {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        crate::test_support::trust_workspace(&workspace);
        let workspace_skill_dir = workspace
            .join(".agents")
            .join("skills")
            .join("workspace-skill");
        std::fs::create_dir_all(&workspace_skill_dir).expect("create workspace skill dir");
        std::fs::write(
            workspace_skill_dir.join("SKILL.md"),
            "---\nname: workspace-skill\ndescription: Workspace skill\ngithub: https://example.com\n---\nbody",
        )
        .expect("write workspace skill");

        let configured_dir = tmp.path().join("configured-skills");
        let configured_skill_dir = configured_dir.join("configured-skill");
        std::fs::create_dir_all(&configured_skill_dir).expect("create configured skill dir");
        std::fs::write(
            configured_skill_dir.join("SKILL.md"),
            "---\nname: configured-skill\ndescription: Configured skill\n---\nbody",
        )
        .expect("write configured skill");

        let entries = build_entries(
            Locale::En,
            configured_dir.as_path(),
            &Default::default(),
            workspace.as_path(),
            Path::new("mcp.json"),
            None,
        );
        let skill_labels = entries
            .iter()
            .filter(|entry| entry.section == PaletteSection::Skill)
            .map(|entry| entry.label.as_str())
            .collect::<Vec<_>>();

        assert!(skill_labels.contains(&"$workspace-skill"));
        assert!(skill_labels.contains(&"$configured-skill"));
    }

    #[test]
    fn command_palette_skills_respect_codewhale_only_scan() {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        crate::test_support::trust_workspace(&workspace);
        let claude_skill_dir = workspace
            .join(".claude")
            .join("skills")
            .join("claude-skill");
        std::fs::create_dir_all(&claude_skill_dir).expect("create claude skill dir");
        std::fs::write(
            claude_skill_dir.join("SKILL.md"),
            "---\nname: claude-skill\ndescription: Claude skill\n---\nbody",
        )
        .expect("write claude skill");
        let codewhale_skill_dir = workspace
            .join(".codewhale")
            .join("skills")
            .join("codewhale-skill");
        std::fs::create_dir_all(&codewhale_skill_dir).expect("create codewhale skill dir");
        std::fs::write(
            codewhale_skill_dir.join("SKILL.md"),
            "---\nname: codewhale-skill\ndescription: CodeWhale skill\n---\nbody",
        )
        .expect("write codewhale skill");

        let entries = build_entries(
            Locale::En,
            workspace.join(".codewhale").join("skills").as_path(),
            &codewhale_runtime::config::SkillsConfig {
                scan_codewhale_only: Some(true),
                ..Default::default()
            },
            workspace.as_path(),
            Path::new("mcp.json"),
            None,
        );
        let skill_labels: Vec<&str> = entries
            .iter()
            .filter(|entry| entry.section == PaletteSection::Skill)
            .map(|entry| entry.label.as_str())
            .collect();

        assert!(skill_labels.contains(&"$codewhale-skill"));
        assert!(!skill_labels.contains(&"$claude-skill"));
    }

    #[test]
    fn command_palette_includes_only_active_reviewed_plugin_skills() {
        let _env = crate::test_support::lock_test_env();
        let tmp = TempDir::new().expect("tempdir");
        let _home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join("home"));
        let workspace = tmp.path().join("workspace");
        let plugin_root = tmp.path().join("plugins/demo");
        std::fs::create_dir_all(plugin_root.join("skills/review")).expect("plugin Skill dir");
        std::fs::write(
            plugin_root.join("plugin.toml"),
            "schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\n[skills]\npath = \"skills\"\n",
        )
        .expect("plugin manifest");
        std::fs::write(
            plugin_root.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: reviewed plugin Skill\n---\nbody\n",
        )
        .expect("plugin Skill");
        let config = codewhale_plugins::discovery::DiscoveryConfig {
            workspace: workspace.clone(),
            user_plugins_dir: tmp.path().join("plugins"),
            workspace_plugins_dir: workspace.join(".codewhale/plugins"),
            builtin_plugin_dirs: Vec::new(),
            state_path: tmp.path().join("plugin-state/state.json"),
        };
        let mut plugins = codewhale_plugins::discovery::discover_with_config(&config);
        let entries_before = build_entries_with_plugins(
            Locale::En,
            tmp.path().join("skills").as_path(),
            &Default::default(),
            &workspace,
            Path::new("mcp.json"),
            None,
            &plugins,
        );
        assert!(
            !entries_before
                .iter()
                .any(|entry| entry.label == "$demo:review")
        );

        plugins.trust("demo").expect("trust plugin");
        plugins.enable("demo").expect("enable plugin");
        let entries_after = build_entries_with_plugins(
            Locale::En,
            tmp.path().join("skills").as_path(),
            &Default::default(),
            &workspace,
            Path::new("mcp.json"),
            None,
            &plugins,
        );
        let skill = entries_after
            .iter()
            .find(|entry| entry.label == "$demo:review")
            .expect("active reviewed plugin Skill should reach the palette");
        assert!(matches!(
            &skill.action,
            CommandPaletteAction::ExecuteCommand { command } if command == "$demo:review"
        ));
    }

    #[test]
    fn command_palette_command_entries_include_links_and_config_but_not_removed_commands() {
        let entries = build_entries(
            Locale::En,
            Path::new("."),
            &Default::default(),
            Path::new("."),
            Path::new("mcp.json"),
            None,
        );
        let command_labels = entries
            .iter()
            .filter(|entry| entry.section == PaletteSection::Command)
            .map(|entry| entry.label.as_str())
            .collect::<Vec<_>>();

        assert!(command_labels.contains(&"/config"));
        assert!(command_labels.contains(&"/links"));
        assert!(command_labels.contains(&"/voice"));
        assert!(!command_labels.contains(&"/set"));
        assert!(!command_labels.contains(&"/deepseek"));
    }

    #[test]
    fn command_palette_includes_workspace_user_commands() {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let commands_dir = workspace.join(".codewhale").join("commands");
        std::fs::create_dir_all(&commands_dir).expect("create commands dir");
        codewhale_runtime::config::save_workspace_trust(&workspace).expect("trust workspace");
        std::fs::write(
            commands_dir.join("review.md"),
            "---\ndescription: Review with context\nargument-hint: <path>\n---\nReview $ARGUMENTS",
        )
        .expect("write user command");

        let entries = build_entries(
            Locale::En,
            tmp.path().join("skills").as_path(),
            &Default::default(),
            workspace.as_path(),
            tmp.path().join("mcp.json").as_path(),
            None,
        );
        let user_entry = entries
            .iter()
            .find(|entry| entry.section == PaletteSection::Command && entry.label == "/review")
            .expect("user command should appear in command palette");

        assert!(user_entry.description.contains("Review with context"));
        assert!(user_entry.description.contains("<path>"));
        assert!(matches!(
            &user_entry.action,
            CommandPaletteAction::InsertText { text } if text == "/review "
        ));
    }

    #[test]
    fn command_palette_uses_frontmatter_name_usage_and_arguments() {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        crate::test_support::trust_workspace(&workspace);
        let commands_dir = workspace.join(".codewhale").join("commands");
        std::fs::create_dir_all(&commands_dir).expect("create commands dir");
        std::fs::write(
            commands_dir.join("workflow-file.md"),
            "---\nname: inspect\ndescription: Inspect a target\nusage: /inspect <path>\narguments: <path>\nargument-hint: <legacy>\n---\nInspect $ARGUMENTS",
        )
        .expect("write user command");

        let entries = build_entries(
            Locale::En,
            tmp.path().join("skills").as_path(),
            &Default::default(),
            workspace.as_path(),
            tmp.path().join("mcp.json").as_path(),
            None,
        );
        let user_entry = entries
            .iter()
            .find(|entry| entry.section == PaletteSection::Command && entry.label == "/inspect")
            .expect("frontmatter name should be the palette command");

        assert_eq!(user_entry.description, "Inspect a target  /inspect <path>");
        assert!(matches!(
            &user_entry.action,
            CommandPaletteAction::InsertText { text } if text == "/inspect "
        ));
        assert!(!entries.iter().any(|entry| {
            entry.section == PaletteSection::Command && entry.label == "/workflow-file"
        }));
    }

    #[test]
    fn command_palette_excludes_hidden_user_commands() {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let commands_dir = workspace.join(".codewhale").join("commands");
        std::fs::create_dir_all(&commands_dir).expect("create commands dir");
        std::fs::write(
            commands_dir.join("secret.md"),
            "---\ndescription: Internal workflow\nhidden: true\n---\nsecret",
        )
        .expect("write hidden user command");

        let entries = build_entries(
            Locale::En,
            tmp.path().join("skills").as_path(),
            &Default::default(),
            workspace.as_path(),
            tmp.path().join("mcp.json").as_path(),
            None,
        );

        assert!(
            !entries
                .iter()
                .any(|entry| entry.section == PaletteSection::Command && entry.label == "/secret")
        );
    }

    #[test]
    fn hidden_frontmatter_name_override_suppresses_shadowed_builtin() {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        crate::test_support::trust_workspace(&workspace);
        let commands_dir = workspace.join(".codewhale").join("commands");
        std::fs::create_dir_all(&commands_dir).expect("create commands dir");
        std::fs::write(
            commands_dir.join("private-help.md"),
            "---\nname: help\nhidden: true\n---\nprivate help",
        )
        .expect("write hidden user command");

        let entries = build_entries(
            Locale::En,
            tmp.path().join("skills").as_path(),
            &Default::default(),
            workspace.as_path(),
            tmp.path().join("mcp.json").as_path(),
            None,
        );

        assert!(!entries.iter().any(|entry| {
            entry.section == PaletteSection::Command
                && matches!(entry.label.as_str(), "/help" | "/private-help")
        }));
    }

    #[test]
    fn command_palette_filters_shadowed_builtin_aliases_from_description() {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        crate::test_support::trust_workspace(&workspace);
        let commands_dir = workspace.join(".codewhale").join("commands");
        std::fs::create_dir_all(&commands_dir).expect("create commands dir");
        std::fs::write(
            commands_dir.join("image-review.md"),
            "---\ndescription: Review an image\nalias: image\n---\nreview image",
        )
        .expect("write user command");

        let entries = build_entries(
            Locale::En,
            tmp.path().join("skills").as_path(),
            &Default::default(),
            workspace.as_path(),
            tmp.path().join("mcp.json").as_path(),
            None,
        );
        let attach = entries
            .iter()
            .find(|entry| entry.section == PaletteSection::Command && entry.label == "/attach")
            .expect("built-in canonical command should remain visible");

        assert!(
            !attach.description.contains("aliases: image")
                && !attach.description.contains(", image")
                && !attach.description.contains("image,"),
            "shadowed /image alias must not be advertised by /attach: {}",
            attach.description
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.section == PaletteSection::Command
                    && entry.label == "/image-review"),
            "user command that owns the /image alias should be visible"
        );
    }

    #[test]
    fn command_palette_visible_canonical_shadow_has_exactly_one_user_row() {
        // Deep-Dive Q1: a visible user command whose canonical name equals a
        // built-in must produce exactly one palette row owned by the user
        // command (its metadata and action), never the built-in row.
        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        crate::test_support::trust_workspace(&workspace);
        let commands_dir = workspace.join(".codewhale").join("commands");
        std::fs::create_dir_all(&commands_dir).expect("create commands dir");
        std::fs::write(
            commands_dir.join("my-help.md"),
            "---\nname: help\ndescription: My private help\nusage: /help <topic>\n---\nhelp $ARGUMENTS",
        )
        .expect("write user command");

        let entries = build_entries(
            Locale::En,
            tmp.path().join("skills").as_path(),
            &Default::default(),
            workspace.as_path(),
            tmp.path().join("mcp.json").as_path(),
            None,
        );
        let rows = entries
            .iter()
            .filter(|entry| entry.section == PaletteSection::Command && entry.label == "/help")
            .collect::<Vec<_>>();

        assert_eq!(rows.len(), 1, "exactly one /help row must exist");
        assert!(
            rows[0].description.contains("My private help"),
            "row must carry the user command metadata: {}",
            rows[0].description
        );
        assert!(
            rows[0].description.contains("/help <topic>"),
            "row must carry the user command usage: {}",
            rows[0].description
        );
        assert!(
            matches!(&rows[0].action, CommandPaletteAction::InsertText { text } if text == "/help "),
            "row must carry the user command action"
        );
    }

    #[test]
    fn command_palette_accepted_alias_suppresses_builtin_canonical_row() {
        // A visible user command whose accepted alias equals a built-in
        // canonical token must suppress the built-in row in the palette,
        // matching the shared alias-aware contract.
        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        crate::test_support::trust_workspace(&workspace);
        let commands_dir = workspace.join(".codewhale").join("commands");
        std::fs::create_dir_all(&commands_dir).expect("create commands dir");
        std::fs::write(
            commands_dir.join("assistant.md"),
            "---\ndescription: My assistant\nalias: help\n---\nassistant",
        )
        .expect("write user command");

        let entries = build_entries(
            Locale::En,
            tmp.path().join("skills").as_path(),
            &Default::default(),
            workspace.as_path(),
            tmp.path().join("mcp.json").as_path(),
            None,
        );

        assert!(
            !entries
                .iter()
                .any(|entry| entry.section == PaletteSection::Command && entry.label == "/help"),
            "built-in canonical row must be suppressed when a user alias claims /help"
        );
        assert!(
            entries.iter().any(
                |entry| entry.section == PaletteSection::Command && entry.label == "/assistant"
            ),
            "the user command must appear under its own canonical name"
        );
    }

    #[test]
    fn command_palette_hidden_canonical_shadow_exposes_no_discovery_row() {
        // A hidden user command claiming a built-in canonical token must not
        // surface either row: the hidden command is excluded from output while
        // still owning the token (AT-008 boundary in the palette).
        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        crate::test_support::trust_workspace(&workspace);
        let commands_dir = workspace.join(".codewhale").join("commands");
        std::fs::create_dir_all(&commands_dir).expect("create commands dir");
        std::fs::write(
            commands_dir.join("private-help.md"),
            "---\nname: help\nhidden: true\n---\nprivate help",
        )
        .expect("write hidden user command");

        let entries = build_entries(
            Locale::En,
            tmp.path().join("skills").as_path(),
            &Default::default(),
            workspace.as_path(),
            tmp.path().join("mcp.json").as_path(),
            None,
        );

        assert!(
            !entries
                .iter()
                .any(|entry| entry.section == PaletteSection::Command && entry.label == "/help"),
            "neither the hidden user command nor the shadowed built-in may appear"
        );
    }

    #[test]
    fn command_palette_alias_shadow_preserves_canonical_row_without_claimed_alias() {
        // A user command claiming only one built-in alias must leave the
        // built-in canonical row visible and remove only the claimed alias
        // from its description.
        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        crate::test_support::trust_workspace(&workspace);
        let commands_dir = workspace.join(".codewhale").join("commands");
        std::fs::create_dir_all(&commands_dir).expect("create commands dir");
        std::fs::write(
            commands_dir.join("image-review.md"),
            "---\ndescription: Review an image\nalias: image\n---\nreview image",
        )
        .expect("write user command");

        let entries = build_entries(
            Locale::En,
            tmp.path().join("skills").as_path(),
            &Default::default(),
            workspace.as_path(),
            tmp.path().join("mcp.json").as_path(),
            None,
        );
        let attach_rows = entries
            .iter()
            .filter(|entry| entry.section == PaletteSection::Command && entry.label == "/attach")
            .collect::<Vec<_>>();

        assert_eq!(
            attach_rows.len(),
            1,
            "built-in canonical row must appear once"
        );
        assert!(
            !attach_rows[0].description.contains("aliases: image")
                && !attach_rows[0].description.contains(", image")
                && !attach_rows[0].description.contains("image,"),
            "claimed /image alias must be absent from /attach description: {}",
            attach_rows[0].description
        );
    }

    #[test]
    fn command_palette_has_one_entry_for_every_registered_command() {
        let tmp = TempDir::new().expect("tempdir");
        let skills_dir = tmp.path().join("skills");
        let mcp_config_path = tmp.path().join("mcp.json");
        let entries = build_entries(
            Locale::En,
            skills_dir.as_path(),
            &Default::default(),
            tmp.path(),
            mcp_config_path.as_path(),
            None,
        );

        let command_entries = entries
            .iter()
            .filter(|entry| entry.section == PaletteSection::Command)
            .collect::<Vec<_>>();
        let user_registry = commands::user_registry::registry_for_workspace(Some(tmp.path()));
        let visible_user_commands = user_registry
            .iter()
            .filter(|command| !command.hidden)
            .count();
        let shadowed_builtins = commands::command_infos()
            .iter()
            .filter(|command| user_registry.get(command.name).is_some())
            .count();
        // Unlisted commands run when typed but are never advertised — see
        // `commands::traits::UNLISTED_COMMANDS`.
        let unlisted = commands::command_infos()
            .iter()
            .filter(|command| command.is_unlisted() && user_registry.get(command.name).is_none())
            .count();
        assert_eq!(
            command_entries.len(),
            commands::command_infos().len() - shadowed_builtins - unlisted + visible_user_commands
        );

        for command in commands::command_infos() {
            if user_registry.get(command.name).is_some() || command.is_unlisted() {
                continue;
            }
            let label = format!("/{}", command.name);
            let matching = command_entries
                .iter()
                .filter(|entry| entry.label == label)
                .collect::<Vec<_>>();
            assert_eq!(
                matching.len(),
                1,
                "expected one palette entry for /{}",
                command.name
            );

            let entry = matching[0];
            assert_eq!(entry.command, command.palette_command());
            assert!(
                entry
                    .description
                    .contains(&*command.description_for(Locale::En)),
                "/{} palette description should include command help text",
                command.name
            );
            if command.requires_argument() {
                assert!(
                    entry.description.contains(command.usage),
                    "/{} palette description should include usage {:?}",
                    command.name,
                    command.usage
                );
            }
        }
    }

    #[test]
    fn command_palette_hides_toolbox_commands_until_searched() {
        let entries = build_entries(
            Locale::En,
            Path::new("."),
            &Default::default(),
            Path::new("."),
            Path::new("mcp.json"),
            None,
        );
        let mut view = CommandPaletteView::new(entries);
        let root_labels = view
            .filtered
            .iter()
            .map(|idx| view.entries[*idx].label.as_str())
            .collect::<Vec<_>>();

        assert!(root_labels.contains(&"/provider"));
        assert!(root_labels.contains(&"/model"));
        assert!(root_labels.contains(&"/fleet"));
        assert!(!root_labels.contains(&"/pod"));
        assert!(root_labels.contains(&"/config"));
        assert!(root_labels.contains(&"/statusline"));
        assert!(!root_labels.contains(&"/rlm"));
        assert!(!root_labels.contains(&"/modeldb"));
        assert!(!root_labels.contains(&"/models"));
        assert!(!root_labels.contains(&"/subagents"));

        view.query = "rlm".to_string();
        view.refilter();
        assert!(
            view.filtered
                .iter()
                .any(|idx| view.entries[*idx].label == "/rlm"),
            "advanced /rlm should still be searchable"
        );
    }

    #[test]
    fn command_palette_runs_model_command_to_open_picker() {
        let entries = build_entries(
            Locale::En,
            Path::new("."),
            &Default::default(),
            Path::new("."),
            Path::new("mcp.json"),
            None,
        );
        let model = entries
            .iter()
            .find(|entry| entry.section == PaletteSection::Command && entry.label == "/model")
            .expect("model command entry");

        assert_eq!(model.command, "/model ");
        assert!(matches!(
            &model.action,
            CommandPaletteAction::ExecuteCommand { command } if command == "/model"
        ));
    }

    #[test]
    fn command_palette_runs_change_without_requiring_version() {
        let entries = build_entries(
            Locale::En,
            Path::new("."),
            &Default::default(),
            Path::new("."),
            Path::new("mcp.json"),
            None,
        );
        let change = entries
            .iter()
            .find(|entry| entry.section == PaletteSection::Command && entry.label == "/change")
            .expect("change command entry");

        assert!(matches!(
            &change.action,
            CommandPaletteAction::ExecuteCommand { command } if command == "/change"
        ));
    }

    #[test]
    fn palette_paste_only_names_are_registered_canonical_commands() {
        let registered: std::collections::HashSet<&str> = commands::command_infos()
            .iter()
            .map(|info| info.name)
            .collect();
        for name in commands::traits::PALETTE_PASTE_ONLY {
            assert!(
                registered.contains(name),
                "PALETTE_PASTE_ONLY entry `{name}` is not a registered command"
            );
        }
    }

    #[test]
    fn command_palette_direct_execute_follows_command_metadata() {
        let tmp = TempDir::new().expect("tempdir");
        let skills_dir = tmp.path().join("skills");
        let mcp_config_path = tmp.path().join("mcp.json");
        let entries = build_entries(
            Locale::En,
            skills_dir.as_path(),
            &Default::default(),
            tmp.path(),
            mcp_config_path.as_path(),
            None,
        );
        let user_registry = commands::user_registry::registry_for_workspace(Some(tmp.path()));

        for command in commands::command_infos() {
            if user_registry.get(command.name).is_some() || command.is_unlisted() {
                continue;
            }
            let label = format!("/{}", command.name);
            let entry = entries
                .iter()
                .find(|entry| entry.section == PaletteSection::Command && entry.label == label)
                .unwrap_or_else(|| panic!("missing palette entry for {label}"));

            if command.palette_runs_directly() {
                assert!(
                    matches!(
                        &entry.action,
                        CommandPaletteAction::ExecuteCommand { command: c }
                            if c == &format!("/{}", command.name)
                    ),
                    "/{} should execute directly from the palette (no required arg)",
                    command.name
                );
            } else {
                assert!(
                    matches!(
                        &entry.action,
                        CommandPaletteAction::InsertText { text }
                            if text == &command.palette_command()
                    ),
                    "/{} should paste for required arguments (usage: {})",
                    command.name,
                    command.usage
                );
            }
        }

        // Drift traps from #3911: no-arg rows must not silently paste, and
        // dead mode-arg names must never reappear as palette allowlist entries.
        for no_arg in [
            "cost",
            "diff",
            "edit",
            "purge",
            "setup",
            "hotbar",
            "translate",
        ] {
            let label = format!("/{no_arg}");
            let entry = entries
                .iter()
                .find(|entry| entry.section == PaletteSection::Command && entry.label == label)
                .unwrap_or_else(|| panic!("missing palette entry for {label}"));
            assert!(
                matches!(&entry.action, CommandPaletteAction::ExecuteCommand { .. }),
                "/{no_arg} is no-arg and must run directly"
            );
        }
        for required in ["rename", "attach", "profile", "review"] {
            let label = format!("/{required}");
            let entry = entries
                .iter()
                .find(|entry| entry.section == PaletteSection::Command && entry.label == label)
                .unwrap_or_else(|| panic!("missing palette entry for {label}"));
            assert!(
                matches!(&entry.action, CommandPaletteAction::InsertText { .. }),
                "/{required} requires an argument and must paste"
            );
        }
    }

    #[test]
    fn command_palette_includes_mcp_discovery_and_failed_servers() {
        let snapshot = codewhale_engine::mcp::McpManagerSnapshot {
            config_path: Path::new("mcp.json").to_path_buf(),
            config_exists: true,
            reload_required: false,
            servers: vec![
                codewhale_engine::mcp::McpServerSnapshot {
                    name: "fs".to_string(),
                    enabled: true,
                    required: false,
                    transport: "stdio".to_string(),
                    command_or_url: "node server.js".to_string(),
                    connect_timeout: 10,
                    execute_timeout: 60,
                    read_timeout: 120,
                    connected: true,
                    error: None,
                    auth_required: false,
                    capability_metadata:
                        codewhale_engine::mcp::McpServerCapabilityMetadata::LegacyFallback,
                    tools: vec![codewhale_engine::mcp::McpDiscoveredItem {
                        name: "read".to_string(),
                        model_name: "mcp_fs_read".to_string(),
                        description: Some("Read files".to_string()),
                    }],
                    resources: Vec::new(),
                    prompts: Vec::new(),
                },
                codewhale_engine::mcp::McpServerSnapshot {
                    name: "broken".to_string(),
                    enabled: true,
                    required: false,
                    transport: "http/sse".to_string(),
                    command_or_url: "https://example.invalid/mcp".to_string(),
                    connect_timeout: 10,
                    execute_timeout: 60,
                    read_timeout: 120,
                    connected: false,
                    error: Some("connect failed".to_string()),
                    auth_required: false,
                    capability_metadata:
                        codewhale_engine::mcp::McpServerCapabilityMetadata::NotObserved,
                    tools: Vec::new(),
                    resources: Vec::new(),
                    prompts: Vec::new(),
                },
            ],
        };
        let entries = build_entries(
            Locale::En,
            Path::new("."),
            &Default::default(),
            Path::new("."),
            Path::new("mcp.json"),
            Some(&snapshot),
        );

        assert!(entries.iter().any(|entry| entry.label == "mcp:manager"));
        assert!(entries.iter().any(|entry| entry.command == "mcp_fs_read"));
        let failed = entries
            .iter()
            .find(|entry| entry.label == "mcp:broken")
            .expect("failed server visible");
        assert!(failed.description.contains("failed"));

        // Verify the "use" insert entry for MCP tools
        let use_entry = entries
            .iter()
            .find(|entry| entry.label == "mcp:fs:tool:read > use")
            .expect("MCP tool use entry should exist");
        assert!(matches!(
            &use_entry.action,
            CommandPaletteAction::InsertText { text } if text == "mcp_fs_read"
        ));
        assert_eq!(use_entry.command, "mcp_fs_read");
    }

    #[test]
    fn command_palette_marks_disabled_servers_visibly() {
        // The healthy/failed cases are covered above; disabled was the
        // remaining gap from #197's acceptance list. Disabled servers must
        // appear in the palette with a `[disabled]` state tag so users can
        // see them without opening the MCP manager.
        let snapshot = codewhale_engine::mcp::McpManagerSnapshot {
            config_path: Path::new("mcp.json").to_path_buf(),
            config_exists: true,
            reload_required: false,
            servers: vec![codewhale_engine::mcp::McpServerSnapshot {
                name: "muted".to_string(),
                enabled: false,
                required: false,
                transport: "stdio".to_string(),
                command_or_url: "node disabled.js".to_string(),
                connect_timeout: 10,
                execute_timeout: 60,
                read_timeout: 120,
                connected: false,
                error: None,
                auth_required: false,
                capability_metadata:
                    codewhale_engine::mcp::McpServerCapabilityMetadata::NotObserved,
                tools: Vec::new(),
                resources: Vec::new(),
                prompts: Vec::new(),
            }],
        };
        let entries = build_entries(
            Locale::En,
            Path::new("."),
            &Default::default(),
            Path::new("."),
            Path::new("mcp.json"),
            Some(&snapshot),
        );

        let muted = entries
            .iter()
            .find(|entry| entry.label == "mcp:muted")
            .expect("disabled server should still appear in the palette");
        assert!(
            muted.description.contains("[disabled]"),
            "expected `[disabled]` state tag in description, got: {}",
            muted.description
        );
    }

    #[test]
    fn command_palette_emits_actions_not_raw_insertions() {
        let entries = vec![CommandPaletteEntry {
            section: PaletteSection::Command,
            label: "/config".to_string(),
            description: "open config".to_string(),
            command: "/config".to_string(),
            action: CommandPaletteAction::ExecuteCommand {
                command: "/config".to_string(),
            },
            show_on_empty_query: true,
        }];
        let mut view = CommandPaletteView::new(entries);

        let action = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert!(matches!(
            action,
            ViewAction::EmitAndClose(ViewEvent::CommandPaletteSelected {
                action: CommandPaletteAction::ExecuteCommand { .. }
            })
        ));
    }

    #[test]
    fn command_palette_mouse_runs_the_same_skill_action_as_enter() {
        let entries = vec![
            palette_entry(PaletteSection::Command, "/config", "open config", "/config"),
            CommandPaletteEntry {
                section: PaletteSection::Skill,
                label: "$plugin:review".to_string(),
                description: "review from an enabled plugin".to_string(),
                command: "$plugin:review".to_string(),
                action: CommandPaletteAction::ExecuteCommand {
                    command: "$plugin:review".to_string(),
                },
                show_on_empty_query: true,
            },
        ];
        let mut keyboard = CommandPaletteView::new(entries.clone());
        keyboard.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()));
        let keyboard_action =
            keyboard.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        let mut mouse = CommandPaletteView::new(entries);
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        mouse.render(area, &mut buf);
        let (rect, _) = mouse
            .row_hitboxes
            .borrow()
            .iter()
            .find(|(_, index)| *index == 1)
            .copied()
            .expect("plugin Skill row should have a mouse hitbox");
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        };
        assert!(matches!(mouse.handle_mouse(click), ViewAction::None));
        let mouse_action = mouse.handle_mouse(click);

        for action in [keyboard_action, mouse_action] {
            assert!(matches!(
                action,
                ViewAction::EmitAndClose(ViewEvent::CommandPaletteSelected {
                    action: CommandPaletteAction::ExecuteCommand { command }
                }) if command == "$plugin:review"
            ));
        }
    }

    #[test]
    fn command_palette_hover_tints_entry_without_moving_selection() {
        let mut view = sample_palette_view();
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        assert_eq!(view.selected, 0);
        let (rect, _) = view
            .row_hitboxes
            .borrow()
            .iter()
            .find(|(_, index)| *index == 1)
            .copied()
            .expect("second entry should have a mouse hitbox");
        let hover = MouseEvent {
            kind: MouseEventKind::Moved,
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        };
        assert!(matches!(view.handle_mouse(hover), ViewAction::None));
        assert_eq!(view.hovered.get(), Some(1));
        assert_eq!(view.selected, 0);

        let mut hovered_buf = Buffer::empty(area);
        view.render(area, &mut hovered_buf);
        assert_eq!(
            hovered_buf[(rect.x, rect.y)].bg,
            codewhale_palette::SURFACE_ELEVATED,
            "hovered palette entry must show the shared hover band"
        );
    }

    /// The four terminal sizes the v0.8.66 modal blocker (#3732) requires every
    /// overlay to remain readable and fully operable at.
    const BLOCKER_SIZES: [(u16, u16); 4] = [(80, 24), (100, 30), (120, 32), (160, 40)];

    fn sample_palette_view() -> CommandPaletteView {
        let entries = vec![
            palette_entry(PaletteSection::Command, "/config", "open config", "/config"),
            palette_entry(PaletteSection::Command, "/model", "choose model", "/model"),
            palette_entry(PaletteSection::Skill, "$search", "search skill", "$search"),
            palette_entry(PaletteSection::Tool, "tool:git", "git tool", "git"),
            palette_entry(PaletteSection::Mcp, "mcp:fs", "filesystem", "mcp_fs_read"),
        ];
        CommandPaletteView::new(entries)
    }

    #[test]
    fn command_palette_is_usable_and_opaque_at_blocker_sizes() {
        use crate::tui::views::ViewStack;
        for (w, h) in BLOCKER_SIZES {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            for y in 0..h {
                for x in 0..w {
                    buf[(x, y)].set_symbol("X");
                }
            }
            let mut stack = ViewStack::new();
            stack.push(sample_palette_view());
            stack.render(area, &mut buf);

            let rows: Vec<String> = (0..h)
                .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
                .collect();
            let text = rows.join("\n");

            // Footer keeps every action.
            assert!(text.contains("move"), "{w}x{h}: missing 'move' hint");
            assert!(text.contains("select"), "{w}x{h}: missing 'select' hint");
            assert!(text.contains("cancel"), "{w}x{h}: missing 'cancel' hint");

            // The selected row carries the charter pointer glyph.
            assert!(
                text.contains(codewhale_tui_kit::glyphs::SELECTION),
                "{w}x{h}: selected row missing charter pointer"
            );

            // Header stays compact: scope help is a single line, with no
            // multi-line "Try:" example block crowding out entries.
            assert!(text.contains("Type to filter"), "{w}x{h}: missing prompt");
            assert!(
                !text.contains("Try:"),
                "{w}x{h}: scope example block should stay collapsed"
            );

            // Composited frame is fully opaque.
            assert!(!text.contains('X'), "{w}x{h}: background bleed-through");
            assert_eq!(
                buf[(w / 2, h / 2)].bg,
                palette::WHALE_BG,
                "{w}x{h}: modal interior must be opaque"
            );

            // No horizontal overflow.
            for (y, row) in rows.iter().enumerate() {
                assert!(
                    UnicodeWidthStr::width(row.trim_end()) <= w as usize,
                    "{w}x{h}: row {y} overflows width: {row:?}"
                );
            }
        }
    }

    #[test]
    fn command_palette_selected_row_uses_shared_selection_style_at_blocker_sizes() {
        use crate::tui::views::ViewStack;
        for (w, h) in BLOCKER_SIZES {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            let mut stack = ViewStack::new();
            stack.push(sample_palette_view());
            stack.render(area, &mut buf);

            // The first entry ("/config") is selected by default; find its row.
            let selected_y = (0..h)
                .find(|&y| {
                    let row: String = (0..w).map(|x| buf[(x, y)].symbol()).collect();
                    row.contains("/config")
                })
                .unwrap_or_else(|| panic!("{w}x{h}: selected entry should render"));
            let selected_cells = (0..w)
                .filter(|&x| {
                    let cell = &buf[(x, selected_y)];
                    !cell.symbol().trim().is_empty()
                        && cell.bg == palette::SELECTION_BG
                        && cell.fg == palette::SELECTION_TEXT
                })
                .count();
            assert!(
                selected_cells >= "/config".len(),
                "{w}x{h}: selected row must render with the shared selection style \
                 (palette::SELECTION_TEXT on palette::SELECTION_BG)"
            );
        }
    }
}
