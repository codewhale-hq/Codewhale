//! Searchable help overlay for `Alt+?`, `F1`, and `Ctrl+/`.
//!
//! Renders two stacked sections — *Slash commands* and *Keybindings* — with
//! a live substring filter applied as the user types in the search box. The
//! entry point decides which section comes first: `/help` and context-menu
//! Help lead with commands, while keyboard shortcuts lead with the key
//! reference that the footer promises. The command list is sourced from
//! [`crate::commands::command_infos()`] and the keybinding list from
//! [`crate::keybinding_table::KEYBINDINGS`] so neither can drift from the
//! wired-up handlers.
//!
//! Keys: any printable character extends the filter, `Backspace` (or `Ctrl+H`)
//! shrinks it,
//! `↑`/`↓` (or `Ctrl+P`/`Ctrl+N`) move the selection, `PgUp`/`PgDn` jump by
//! ten rows, `Home`/`End` jump to ends, and `Esc` closes. Pressing `?` again
//! at the call-site (`tui::ui`) also toggles the overlay closed.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashSet;
#[cfg(test)]
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::commands;
use crate::keybinding_table::KEYBINDINGS;
use crate::tui::menu_style;
use crate::tui::views::{
    ActionHint, ModalKind, ModalView, ViewAction, render_modal_footer, render_panel_scroll_rail,
    render_underwater_surface,
};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;

/// Two top-level sections rendered in the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelpSection {
    Command,
    UserCommand,
    Skill,
    Keybinding,
}

impl HelpSection {
    fn label(self, locale: Locale) -> Cow<'static, str> {
        match self {
            Self::Command => tr(locale, MessageId::HelpSlashCommands),
            Self::UserCommand => tr(locale, MessageId::HelpUserCommands),
            Self::Skill => tr(locale, MessageId::HelpSkills),
            Self::Keybinding => tr(locale, MessageId::HelpKeybindings),
        }
    }
}

/// Which reference surface owns the first visible section when Help opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpOrdering {
    /// `/help` and context-menu Help are command discovery surfaces.
    CommandsFirst,
    /// F1 and its Ctrl+/ and Alt+? fallbacks open the keyboard reference.
    KeybindingsFirst,
}

impl HelpOrdering {
    fn section_rank(self, section: HelpSection) -> u8 {
        // User commands and skills sit with the built-in commands: they are
        // the same kind of thing to the user (#3912), so a keyboard-reference
        // open still sorts every command surface below the chords.
        match (self, section) {
            (Self::CommandsFirst, HelpSection::Command) => 0,
            (Self::CommandsFirst, HelpSection::UserCommand) => 1,
            (Self::CommandsFirst, HelpSection::Skill) => 2,
            (Self::CommandsFirst, HelpSection::Keybinding) => 3,
            (Self::KeybindingsFirst, HelpSection::Keybinding) => 0,
            (Self::KeybindingsFirst, HelpSection::Command) => 1,
            (Self::KeybindingsFirst, HelpSection::UserCommand) => 2,
            (Self::KeybindingsFirst, HelpSection::Skill) => 3,
        }
    }
}

#[derive(Debug, Clone)]
struct HelpEntry {
    section: HelpSection,
    /// Sort-within-section key — keybinding entries reuse their declared
    /// section's rank so the help overlay groups Navigation, Editing, … in
    /// the same order as `keybinding_table`.
    sub_rank: u8,
    label: String,
    description: String,
    /// The command's argument shape, when it has one worth stating — the
    /// registry `usage` string for built-ins, the front-matter usage for
    /// workspace commands. `None` for rows that are already the whole shape
    /// (`/copy`, `$skill`, a keybinding chord).
    usage: Option<String>,
    /// Lowercased haystack used for substring matching; pre-built so each
    /// keystroke does not re-allocate per entry.
    haystack: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HelpRenderRow {
    Group {
        key: String,
        label: String,
        count: usize,
        collapsed: bool,
    },
    Entry {
        slot: usize,
        entry_idx: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HelpHit {
    Group(String),
    Entry(usize),
}

pub struct HelpView {
    locale: Locale,
    ordering: HelpOrdering,
    entries: Vec<HelpEntry>,
    /// Indices into `entries`, in display order, after filtering.
    filtered: Vec<usize>,
    query: String,
    /// Keyboard focus covers both group headers and entry rows. `selected`
    /// remains the last focused entry slot so entry-oriented actions and
    /// tests keep a stable target while a header owns focus.
    focus: Option<HelpHit>,
    selected: usize,
    collapsed: HashSet<String>,
    row_hitboxes: RefCell<Vec<(Rect, HelpHit)>>,
}

impl Default for HelpView {
    fn default() -> Self {
        Self::new()
    }
}

impl HelpView {
    pub fn new() -> Self {
        Self::new_for_locale(Locale::En)
    }

    pub fn new_for_locale(locale: Locale) -> Self {
        Self::new_with_ordering(locale, HelpOrdering::CommandsFirst)
    }

    /// Discoverability index over every user-invocable surface (#3912):
    /// built-ins, workspace commands, and discovered skills. `skills` comes
    /// from `App::cached_skills`; pass `&[]` only where none are discovered.
    #[cfg(test)]
    pub fn new_for_workspace(
        locale: Locale,
        workspace: &Path,
        skills: &[(String, String)],
    ) -> Self {
        commands::user_registry::with_registry_for_workspace(Some(workspace), |registry| {
            Self::new_with_registry(locale, HelpOrdering::CommandsFirst, registry, skills)
        })
    }

    pub fn new_for_app(app: &crate::tui::app::App, shortcuts: bool) -> Self {
        commands::user_registry::with_registry_for_app(app, |registry| {
            Self::new_with_registry(
                app.ui_locale,
                if shortcuts {
                    HelpOrdering::KeybindingsFirst
                } else {
                    HelpOrdering::CommandsFirst
                },
                registry,
                &app.cached_skills,
            )
        })
    }
    fn new_with_ordering(locale: Locale, ordering: HelpOrdering) -> Self {
        let registry = commands::user_registry::UserCommandRegistry::new();
        Self::new_with_registry(locale, ordering, &registry, &[])
    }

    fn new_with_registry(
        locale: Locale,
        ordering: HelpOrdering,
        registry: &commands::user_registry::UserCommandRegistry,
        skills: &[(String, String)],
    ) -> Self {
        let entries = build_entries(locale, registry, skills);
        let mut view = Self {
            locale,
            ordering,
            entries,
            filtered: Vec::new(),
            query: String::new(),
            focus: None,
            selected: 0,
            collapsed: default_collapsed(ordering),
            row_hitboxes: RefCell::new(Vec::new()),
        };
        view.refilter();
        view
    }

    /// Start with every Help/shortcuts group expanded. Default is the
    /// Grok-like folded long tail; `/config help_expand_groups true` opts in.
    #[must_use]
    pub fn with_groups_expanded(mut self, expand: bool) -> Self {
        if expand {
            self.collapsed.clear();
            self.clamp_focus_to_visible();
        }
        self
    }

    fn tr(&self, id: MessageId) -> Cow<'static, str> {
        tr(self.locale, id)
    }

    fn refilter(&mut self) {
        // Substring matching is intentional — fuzzy matchers can hide the
        // exact-prefix hit a user is typing toward, which is the wrong
        // failure mode for a *help* surface. We split on whitespace so
        // multi-term queries (`apply mode`) act as an AND.
        let query = self.query.trim().to_ascii_lowercase();
        let terms: Vec<&str> = query
            .split_whitespace()
            .filter(|term| !term.is_empty())
            .collect();

        let mut filtered: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| terms.iter().all(|term| entry.haystack.contains(term)))
            .map(|(idx, _)| idx)
            .collect();

        filtered.sort_by_key(|idx| {
            let entry = &self.entries[*idx];
            (
                self.ordering.section_rank(entry.section),
                entry.sub_rank,
                entry.label.clone(),
            )
        });
        self.filtered = filtered;
        self.clamp_focus_to_visible();
    }

    fn clamp_focus_to_visible(&mut self) {
        let visible = self.visible_entry_slots();
        if !visible.is_empty() && !visible.contains(&self.selected) {
            self.selected = visible[0];
        }
        let focusable = self.focusable_rows();
        if !self
            .focus
            .as_ref()
            .is_some_and(|focus| focusable.contains(focus))
        {
            // Prefer the first entry over the group header above it. A header
            // has no description, and the detail row under the filter reads
            // the focused entry — so falling back to a header left that row
            // blank exactly when Help opens and while a query is being typed.
            self.focus = focusable
                .iter()
                .find(|hit| matches!(hit, HelpHit::Entry(_)))
                .or_else(|| focusable.first())
                .cloned();
        }
    }

    fn visible_entry_slots(&self) -> Vec<usize> {
        self.filtered
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(slot, entry_idx)| {
                let key = group_key(&self.entries[entry_idx]);
                if self.group_is_collapsed(&key) {
                    None
                } else {
                    Some(slot)
                }
            })
            .collect()
    }

    fn group_is_collapsed(&self, key: &str) -> bool {
        self.query.trim().is_empty() && self.collapsed.contains(key)
    }

    fn toggle_group(&mut self, key: &str) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.to_string());
        }
        self.focus = Some(HelpHit::Group(key.to_string()));
        self.clamp_focus_to_visible();
    }

    fn move_selection(&mut self, delta: isize) {
        // #4755: help list wraps at both ends (same as other modal lists).
        // Group headers participate so a keyboard user can open the same
        // default-collapsed rows as a mouse user.
        let focusable = self.focusable_rows();
        if focusable.is_empty() {
            return;
        }
        let pos = focusable
            .iter()
            .position(|candidate| self.focus.as_ref() == Some(candidate))
            .unwrap_or(0);
        let next = crate::tui::list_nav::wrap_index(pos, focusable.len(), delta);
        self.set_focus(focusable[next].clone());
    }

    fn move_selection_wrapping(&mut self, delta: isize) {
        self.move_selection(delta);
    }

    fn render_rows(&self) -> Vec<HelpRenderRow> {
        let mut rows = Vec::new();
        let mut active_group: Option<String> = None;

        for (slot, entry_idx) in self.filtered.iter().copied().enumerate() {
            let entry = &self.entries[entry_idx];
            let key = group_key(entry);
            if active_group.as_deref() != Some(key.as_str()) {
                let count = self
                    .filtered
                    .iter()
                    .filter(|idx| group_key(&self.entries[**idx]) == key)
                    .count();
                let collapsed = self.group_is_collapsed(&key);
                rows.push(HelpRenderRow::Group {
                    key: key.clone(),
                    label: group_label(entry, self.locale),
                    count,
                    collapsed,
                });
                active_group = Some(key.clone());
            }
            if self.group_is_collapsed(&key) {
                continue;
            }
            rows.push(HelpRenderRow::Entry { slot, entry_idx });
        }

        rows
    }

    /// Width of the label column for each group, measured from the labels
    /// that group actually contains.
    ///
    /// The column used to be a flat 28 columns at every terminal size. At 60
    /// columns that spent 28 of ~53 on a gutter — `/advisor` is eight cells
    /// wide, so twenty blank columns sat between every command and a
    /// description that had been cut to 21. Sizing per group keeps the block
    /// under each header reading as one table while handing the slack back to
    /// the descriptions; it is stable while scrolling because it does not
    /// depend on which rows are on screen.
    fn label_widths(&self, cap: usize) -> std::collections::HashMap<String, usize> {
        let mut widths: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for entry_idx in self.filtered.iter().copied() {
            let entry = &self.entries[entry_idx];
            let width = entry.label.width().min(cap);
            let slot = widths.entry(group_key(entry)).or_default();
            *slot = (*slot).max(width);
        }
        widths
    }

    /// What the focused row could not say for itself: the command's argument
    /// shape, and its description when the row had to shed one.
    ///
    /// `/help` used to show `label + description` and keep `usage` in the
    /// search haystack alone, so `/workspace [path|worktrees]` read as
    /// `/workspace` and the worktree manager behind it was invisible (#5952).
    /// The usage line is new information, so it is printed whenever the
    /// registry has one; the description is only repeated when the row shed
    /// it, because printing the same sentence twice on one screen is the
    /// duplication this slot was built to avoid. The row stays reserved
    /// either way so the list does not jump as focus moves.
    fn focused_entry_detail(
        &self,
        inner_width: usize,
        label_cap: usize,
        label_widths: &std::collections::HashMap<String, usize>,
    ) -> Option<String> {
        let HelpHit::Entry(slot) = self.focus.as_ref()? else {
            return None;
        };
        let entry_idx = *self.filtered.get(*slot)?;
        let entry = &self.entries[entry_idx];
        let label_width = label_widths
            .get(&group_key(entry))
            .copied()
            .unwrap_or(label_cap);
        let inline_capacity = inner_width.saturating_sub(label_width + 4);
        let inline = shed_to_width(&entry.description, inline_capacity);
        let full = shed_to_width(&entry.description, inner_width);
        let repaired = (full != inline && !full.is_empty()).then(|| full.to_string());
        match (entry.usage.as_deref(), repaired) {
            (None, repaired) => repaired,
            (Some(usage), None) => Some(shed_to_width(usage, inner_width).to_string()),
            (Some(usage), Some(description)) => {
                // They join at a joint `shed_to_width` already sheds on, and
                // the description leads: repairing the shed is what this slot
                // was built for, and a panel too narrow to hold both must not
                // spend itself on the argument shape and drop the sentence
                // the row could not print.
                let joined = format!("{description} — {usage}");
                Some(shed_to_width(&joined, inner_width).to_string())
            }
        }
    }

    fn focusable_rows(&self) -> Vec<HelpHit> {
        self.render_rows()
            .into_iter()
            .map(|row| match row {
                HelpRenderRow::Group { key, .. } => HelpHit::Group(key),
                HelpRenderRow::Entry { slot, .. } => HelpHit::Entry(slot),
            })
            .collect()
    }

    fn set_focus(&mut self, focus: HelpHit) {
        if let HelpHit::Entry(slot) = focus {
            self.selected = slot;
            self.focus = Some(HelpHit::Entry(slot));
        } else {
            self.focus = Some(focus);
        }
    }

    fn focused_group_key(&self) -> Option<String> {
        match self.focus.as_ref()? {
            HelpHit::Group(key) => Some(key.clone()),
            HelpHit::Entry(slot) => self
                .filtered
                .get(*slot)
                .map(|entry_idx| group_key(&self.entries[*entry_idx])),
        }
    }

    fn selected_render_row(rows: &[HelpRenderRow], focus: Option<&HelpHit>) -> usize {
        rows.iter()
            .position(|row| match (row, focus) {
                (HelpRenderRow::Group { key, .. }, Some(HelpHit::Group(focused))) => key == focused,
                (HelpRenderRow::Entry { slot, .. }, Some(HelpHit::Entry(focused))) => {
                    slot == focused
                }
                _ => false,
            })
            .unwrap_or(0)
    }

    fn visible_row_start(
        rows: &[HelpRenderRow],
        focus: Option<&HelpHit>,
        visible_budget: usize,
    ) -> usize {
        if rows.len() <= visible_budget {
            return 0;
        }

        let selected_row = Self::selected_render_row(rows, focus);
        let half = visible_budget / 2;
        if selected_row <= half {
            0
        } else if selected_row + half >= rows.len() {
            rows.len().saturating_sub(visible_budget)
        } else {
            selected_row.saturating_sub(half)
        }
    }
}

fn build_entries(
    locale: Locale,
    registry: &commands::user_registry::UserCommandRegistry,
    skills: &[(String, String)],
) -> Vec<HelpEntry> {
    let mut entries = Vec::new();

    for command in commands::command_infos() {
        if registry.get(command.name).is_some() {
            continue;
        }
        let label = format!("/{}", command.name);
        let localized = command.description_for(locale);
        let visible_aliases = command
            .aliases
            .iter()
            .copied()
            .filter(|alias| registry.get(alias).is_none())
            .filter(|alias| alias_is_listed_for(locale, alias))
            .collect::<Vec<_>>();
        // Every alias stays findable by typing it, listed or not.
        let alias_terms = command
            .aliases
            .iter()
            .map(|alias| format!("/{alias}"))
            .collect::<Vec<_>>()
            .join(" ");
        let description = if visible_aliases.is_empty() {
            localized.to_string()
        } else {
            format!(
                "{}  (aliases: {})",
                localized,
                visible_aliases
                    .iter()
                    .map(|a| format!("/{a}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let haystack = format!(
            "{} {} {} {}",
            label.to_ascii_lowercase(),
            description.to_ascii_lowercase(),
            command.usage.to_ascii_lowercase(),
            alias_terms.to_lowercase()
        );
        entries.push(HelpEntry {
            section: HelpSection::Command,
            // Curated commands first, then the catalog; alphabetical within
            // each by leaning on `label.clone()` in the final sort_by_key tuple.
            sub_rank: command_sub_rank(&label),
            usage: stated_usage(command.usage, &label),
            label,
            description,
            haystack,
        });
    }

    // Workspace commands (#3912). The registry was already consulted above to
    // suppress shadowed built-ins; until now it never contributed a row of its
    // own, so `.codewhale/commands/*.md` authors could not find their own work
    // in the surface that teaches the product. `hidden` entries stay out.
    for command in registry.iter().filter(|command| !command.hidden) {
        let label = format!("/{}", command.name);
        let description = command
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .unwrap_or_default()
            .to_string();
        let usage = command
            .display_usage()
            .map(str::to_owned)
            .unwrap_or_else(|| label.clone());
        let haystack = format!(
            "{} {} {}",
            label.to_ascii_lowercase(),
            description.to_ascii_lowercase(),
            usage.to_ascii_lowercase()
        );
        entries.push(HelpEntry {
            section: HelpSection::UserCommand,
            sub_rank: 0,
            usage: stated_usage(&usage, &label),
            label,
            description,
            haystack,
        });
    }

    // Skills dispatch as `$name` or `/skill name`; advertise the shape the
    // user actually types.
    for (name, description) in skills {
        let label = format!("${name}");
        let description = description.trim().to_string();
        let haystack = format!(
            "{} {} /skill {}",
            label.to_ascii_lowercase(),
            description.to_ascii_lowercase(),
            name.to_ascii_lowercase()
        );
        entries.push(HelpEntry {
            section: HelpSection::Skill,
            sub_rank: 0,
            usage: None,
            label,
            description,
            haystack,
        });
    }

    for binding in KEYBINDINGS {
        // macOS renders Alt chords with the Option glyph (`⌥V`), never
        // `Alt`/`Cmd` (TUI-DOG-002 acceptance).
        let mut label = crate::tui::shell_key_routing::display_chord(binding.chord).into_owned();
        // The newline row is the one chord whose availability depends on the
        // terminal rather than the platform, so it is answered here instead
        // of listing a key that may do nothing.
        if label.contains("Shift+Enter")
            && !crate::tui::composer_ui::terminal_can_report_shift_enter()
        {
            label = label.replace(" / Shift+Enter (enhanced terminals)", "");
        }
        let description = tr(locale, binding.description_id).into_owned();
        let haystack = format!(
            "{} {}",
            label.to_ascii_lowercase(),
            description.to_ascii_lowercase()
        );
        entries.push(HelpEntry {
            section: HelpSection::Keybinding,
            sub_rank: binding.section.rank(),
            usage: None,
            label,
            description,
            haystack,
        });
    }

    entries
}

/// Romanized Chinese (pinyin) command aliases. They dispatch in every locale,
/// but only the Chinese packs list them: an English reader sees `/clear`, not
/// `/clear (aliases: /qingping)`.
const ROMANIZED_ALIASES: &[&str] = &[
    "bangzhu",
    "chongmingming",
    "chongshi",
    "daili",
    "dangan",
    "daochu",
    "digui",
    "fujian",
    "gaiming",
    "gouzi",
    "jiazai",
    "jihua",
    "jineng",
    "jinengliebiao",
    "lianjie",
    "maodian",
    "moxing",
    "moxingliebiao",
    "qingchu",
    "qingping",
    "shencha",
    "shouye",
    "tuichu",
    "xinren",
    "xitong",
    "yasuo",
    "yuyin",
    "yuyincontrol",
    "yuyinsend",
    "zhinengti",
    "zhuye",
    "zidong",
    "zuoye",
];

/// Whether `/help` lists `alias` beside its command for `locale`. Chinese
/// aliases (Han script or pinyin) are listed only in the Chinese packs.
fn alias_is_listed_for(locale: Locale, alias: &str) -> bool {
    if matches!(locale, Locale::ZhHans | Locale::ZhHant) {
        return true;
    }
    let han = alias
        .chars()
        .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch));
    !han && !ROMANIZED_ALIASES.contains(&alias)
}

/// The usage line worth printing beside a row, or `None` when it only
/// restates the label.
///
/// `/copy`'s usage is `/copy`; showing it teaches nothing and costs the row
/// that `/queue [list|send <n>|…]` needs. Workspace commands declare the
/// argument shape alone (`<environment>`), so the label is prepended to make
/// the same whole line a built-in already carries.
fn stated_usage(usage: &str, label: &str) -> Option<String> {
    let usage = usage.trim();
    if usage.is_empty() || usage == label {
        return None;
    }
    Some(if usage.starts_with('/') {
        usage.to_string()
    } else {
        format!("{label} {usage}")
    })
}

/// The commands Help opens on. Everything else stays one keystroke away under
/// *All commands* — 103 rows sorted alphabetically is a catalog, not an answer,
/// and it buried the handful of commands people actually reach for.
///
/// Membership is a product judgement, not a usage metric: these are the ones a
/// session needs to steer itself. The first five agree with the composer's own
/// curated slash menu on purpose, so the two surfaces teach the same thing.
const COMMON_COMMANDS: [&str; 12] = [
    "/setup",
    "/model",
    "/settings",
    "/resume",
    "/clear",
    "/compact",
    "/context",
    "/cost",
    "/diff",
    "/mcp",
    "/theme",
    "/exit",
];

/// Sort rank that also selects the group: curated commands sort and group ahead
/// of the full catalog, because `filtered` is ordered by
/// `(section_rank, sub_rank, label)` and `group_key` reads the same field.
const COMMAND_RANK_COMMON: u8 = 0;
const COMMAND_RANK_ALL: u8 = 1;

fn command_sub_rank(label: &str) -> u8 {
    if COMMON_COMMANDS.contains(&label) {
        COMMAND_RANK_COMMON
    } else {
        COMMAND_RANK_ALL
    }
}

fn group_key(entry: &HelpEntry) -> String {
    match entry.section {
        HelpSection::Command if entry.sub_rank == COMMAND_RANK_COMMON => "cmd:common".into(),
        HelpSection::Command => "cmd:all".into(),
        HelpSection::UserCommand => "usercmd".into(),
        HelpSection::Skill => "skill".into(),
        HelpSection::Keybinding => format!("kb:{}", entry.sub_rank),
    }
}

fn group_label(entry: &HelpEntry, locale: Locale) -> String {
    match entry.section {
        HelpSection::Command if entry.sub_rank == COMMAND_RANK_COMMON => {
            tr(locale, MessageId::HelpGroupCommonCommands).into_owned()
        }
        HelpSection::Command => tr(locale, MessageId::HelpGroupAllCommands).into_owned(),
        HelpSection::Keybinding => keybinding_section_for_rank(entry.sub_rank)
            .map(|section| section.label(locale).into_owned())
            .unwrap_or_else(|| entry.section.label(locale).into_owned()),
        other => other.label(locale).into_owned(),
    }
}

fn keybinding_section_for_rank(rank: u8) -> Option<crate::keybinding_table::KeybindingSection> {
    crate::keybinding_table::KeybindingSection::ALL
        .into_iter()
        .find(|section| section.rank() == rank)
}

fn default_collapsed(ordering: HelpOrdering) -> HashSet<String> {
    use crate::keybinding_table::KeybindingSection;
    let kb_keys = KeybindingSection::ALL
        .into_iter()
        .map(|section| format!("kb:{}", section.rank()));

    match ordering {
        HelpOrdering::KeybindingsFirst => {
            // Show Navigation only — the rest is a long tail the user
            // expands or searches. Slash/skill catalogs stay folded.
            let mut set: HashSet<String> = ["cmd:common", "cmd:all", "usercmd", "skill"]
                .into_iter()
                .map(str::to_string)
                .collect();
            set.extend(kb_keys.filter(|key| key != "kb:0"));
            set
        }
        HelpOrdering::CommandsFirst => {
            // Open on the curated commands with everything else folded. The
            // catalogs are still one keystroke — or one keystroke of typing,
            // since a query ignores collapse entirely — away.
            let mut set: HashSet<String> = ["cmd:all", "usercmd", "skill"]
                .into_iter()
                .map(str::to_string)
                .collect();
            set.extend(kb_keys);
            set
        }
    }
}

/// Joints a one-line description may shed at, longest-binding first. These
/// are the marks the descriptions already use: a trailing parenthetical (the
/// alias list), a semicolon or em-dash clause, then ordinary sentence and
/// comma boundaries.
const FIELD_JOINTS: [&str; 6] = [" (", "; ", " — ", ". ", ": ", ", "];

/// Fit a description into `max_width` by shedding whole fields, never by
/// cutting one.
///
/// The overlay used to hand every label and description to a
/// `truncate_to_width` that appended `…`. In a list of two hundred rows an
/// ellipsis is the worst possible mark: it promises text the row has no way
/// to reveal, and it lands mid-token — `(aliases: /qin…` leaves an unclosed
/// parenthesis, and `deepseek-v4-…` names no model, because these strings
/// share prefixes. So the description sheds its alias parenthetical first,
/// then trailing clauses at its own joints, and finally itself. The label is
/// never shed at all: it is the string the user has to type.
fn shed_to_width(text: &str, max_width: usize) -> Cow<'_, str> {
    let trimmed = text.trim_end();
    if max_width == 0 {
        return Cow::Borrowed("");
    }
    if trimmed.width() <= max_width {
        return Cow::Borrowed(trimmed);
    }
    let mut best = "";
    let mut oversize_clause = "";
    let mut depth = 0usize;
    for (idx, ch) in trimmed.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        // Only cut where the parentheses balance. `(aliases: /image, /media)`
        // holds a `: ` and a `, ` that are joints of the alias list, not of
        // the sentence; cutting there left `(aliases: /image, /media` with the
        // parenthesis hanging open — no ellipsis, and still a broken row.
        if depth > 0 {
            continue;
        }
        let rest = &trimmed[idx..];
        if !FIELD_JOINTS.iter().any(|joint| rest.starts_with(joint)) {
            continue;
        }
        let head = trimmed[..idx].trim_end_matches([' ', ',', ';', ':', '—', '-']);
        if head.is_empty() {
            continue;
        }
        let width = head.width();
        if width <= max_width {
            if width > best.width() {
                best = head;
            }
        } else if oversize_clause.is_empty() {
            // The main clause was one column over, so the joint itself did
            // not fire. Word-shed that clause rather than the alias list
            // hanging off it — otherwise `/automation` keeps the adjectives
            // and loses `automations`. Heads grow left to right, so the first
            // oversize one is the main clause; a later, wider head is that
            // clause plus everything trailing it, which is the text this
            // branch exists to shed.
            oversize_clause = head;
        }
    }
    if best.is_empty() {
        // Roughly half of these descriptions are a single clause with no
        // joint at all — "Toggle background advisor watcher on/off for this
        // session". Shedding the whole field there left a bare `/advisor`
        // beside rows that still had text, which reads as a broken renderer
        // rather than as a decision. So the last resort is the sentence's
        // own short form: whole words, no mark, and the same text restated
        // at panel width one row up in the detail slot. What is never done
        // is append `…`, which
        // would claim text this overlay has no way to reveal.
        let source = if oversize_clause.is_empty() {
            trimmed
        } else {
            oversize_clause
        };
        shed_to_words(source, max_width)
    } else {
        Cow::Borrowed(best)
    }
}

/// Longest prefix of `text` that fits `max_width` display columns, cut on a
/// character boundary. Used when there is no word boundary to cut on.
fn widest_char_prefix(text: &str, max_width: usize) -> &str {
    let mut fitted = 0usize;
    for (idx, ch) in text.char_indices() {
        let next = idx + ch.len_utf8();
        if text[..next].width() > max_width {
            break;
        }
        fitted = next;
    }
    &text[..fitted]
}

/// Longest whole-word prefix of `text` that fits, with trailing short
/// function words dropped so the phrase does not end on `to an`.
///
/// The scan used to stop on a space, so the last word was never included
/// even when it fitted, and the two-pass short-word trim then left a simple
/// verb + modifier + noun phrase without the noun — `/automation` read
/// `Manage durable scheduled`. If that prefix lost the head noun, intervening
/// modifiers are dropped so the noun survives.
fn shed_to_words(text: &str, max_width: usize) -> Cow<'_, str> {
    let mut end = 0usize;
    for (idx, ch) in text.char_indices() {
        if ch == ' ' && text[..idx].width() <= max_width {
            end = idx;
        }
    }
    // Include the last word when the whole phrase fits. The loop above only
    // fires on spaces, so without this the head noun was always eaten.
    if text.width() <= max_width {
        end = text.len();
    }
    if end == 0 {
        // No usable space boundary. That is the normal case for Japanese,
        // Chinese and Thai, which do not delimit words with spaces at all —
        // the loop above can never fire, so this used to return "" and every
        // description in those locales rendered blank. It also happens in
        // English whenever the first space falls beyond `max_width`.
        // Fall back to the widest whole-character prefix that fits.
        return Cow::Borrowed(
            widest_char_prefix(text, max_width).trim_end_matches([' ', ',', ';', ':', '—', '-']),
        );
    }
    let mut head = &text[..end];
    // Two passes at most: enough for `to an`, not enough to eat a real word.
    for _ in 0..2 {
        let Some(cut) = head.rfind(' ') else { break };
        if head.len() - cut - 1 > 3 {
            break;
        }
        head = &head[..cut];
    }
    let head = head.trim_end_matches([' ', ',', ';', ':', '—', '-']);
    if let Some(kept) = keep_simple_head_noun(text, head, max_width) {
        return kept;
    }
    Cow::Borrowed(head)
}

fn is_short_function_word(word: &str) -> bool {
    !word.is_empty() && word.len() <= 3
}

fn is_plain_content_word(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|ch| ch.is_ascii_alphabetic() || matches!(ch, '-' | '/'))
}

/// Restore the head noun of a simple `verb modifier* noun` phrase when the
/// prefix trim dropped it. Phrases with a short function word after the verb
/// (`Toggle the background advisor for this session`) stay prefix-trimmed,
/// as do rows that carry punctuation (`(aliases: /image, /media)`).
fn keep_simple_head_noun<'a>(
    text: &'a str,
    prefix: &'a str,
    max_width: usize,
) -> Option<Cow<'a, str>> {
    let words: Vec<&str> = text.split(' ').filter(|word| !word.is_empty()).collect();
    if words.len() < 2 {
        return None;
    }
    let noun = *words.last()?;
    if is_short_function_word(noun) || prefix.ends_with(noun) {
        return None;
    }
    if !words.iter().all(|word| is_plain_content_word(word)) {
        return None;
    }
    if words[1..words.len() - 1]
        .iter()
        .any(|word| is_short_function_word(word))
    {
        return None;
    }
    if noun.width() > max_width {
        return None;
    }
    let verb = words[0];
    let modifiers = &words[1..words.len() - 1];
    for skip in 0..=modifiers.len() {
        let mut candidate = String::from(verb);
        for modifier in &modifiers[skip..] {
            candidate.push(' ');
            candidate.push_str(modifier);
        }
        candidate.push(' ');
        candidate.push_str(noun);
        if candidate.width() <= max_width {
            if text.starts_with(&candidate)
                && text
                    .as_bytes()
                    .get(candidate.len())
                    .is_none_or(|byte| *byte == b' ')
            {
                return Some(Cow::Borrowed(&text[..candidate.len()]));
            }
            return Some(Cow::Owned(candidate));
        }
    }
    Some(Cow::Borrowed(noun))
}

impl ModalView for HelpView {
    fn kind(&self) -> ModalKind {
        ModalKind::Help
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        // Scroll clamps at the ends (keyboard Up/Down wrap); wheel-wrapping
        // reads as disorienting.
        match mouse.kind {
            MouseEventKind::ScrollUp => self.move_selection(-1),
            MouseEventKind::ScrollDown => self.move_selection(1),
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self.row_hitboxes.borrow().iter().find_map(|(rect, hit)| {
                    rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                        .then_some(hit.clone())
                });
                if let Some(hit) = hit {
                    match hit {
                        HelpHit::Group(key) => self.toggle_group(&key),
                        HelpHit::Entry(slot) => self.set_focus(HelpHit::Entry(slot)),
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
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                ViewAction::Close
            }
            KeyCode::Up => {
                self.move_selection_wrapping(-1);
                ViewAction::None
            }
            KeyCode::Down => {
                self.move_selection_wrapping(1);
                ViewAction::None
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_selection_wrapping(-1);
                ViewAction::None
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_selection_wrapping(1);
                ViewAction::None
            }
            KeyCode::PageUp => {
                self.move_selection(-10);
                ViewAction::None
            }
            KeyCode::PageDown => {
                self.move_selection(10);
                ViewAction::None
            }
            KeyCode::Home => {
                if let Some(first) = self.focusable_rows().first().cloned() {
                    self.set_focus(first);
                }
                ViewAction::None
            }
            KeyCode::End => {
                if let Some(last) = self.focusable_rows().last().cloned() {
                    self.set_focus(last);
                }
                ViewAction::None
            }
            KeyCode::Enter => {
                if let Some(HelpHit::Group(key)) = self.focus.clone() {
                    self.toggle_group(&key);
                }
                ViewAction::None
            }
            KeyCode::Right => {
                if let Some(HelpHit::Group(key)) = self.focus.clone()
                    && self.group_is_collapsed(&key)
                {
                    self.toggle_group(&key);
                }
                ViewAction::None
            }
            KeyCode::Left => {
                if let Some(key) = self.focused_group_key() {
                    match self.focus.as_ref() {
                        Some(HelpHit::Entry(_)) => self.set_focus(HelpHit::Group(key)),
                        Some(HelpHit::Group(_)) if !self.group_is_collapsed(&key) => {
                            self.collapsed.insert(key.clone());
                            self.set_focus(HelpHit::Group(key));
                            self.clamp_focus_to_visible();
                        }
                        _ => {}
                    }
                }
                ViewAction::None
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.refilter();
                ViewAction::None
            }
            // Terminals where stty erase == ^H send Ctrl+H instead of
            // Backspace (DEL). Treat it identically so the filter input
            // works across all platforms (#958).
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.pop();
                self.refilter();
                ViewAction::None
            }
            KeyCode::Char(c)
                if !c.is_control()
                    && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT) =>
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
        let inner = render_underwater_surface(
            area,
            buf,
            format!(
                "{} — {}",
                self.tr(MessageId::HelpTitle),
                self.tr(MessageId::HelpSubtitle)
            ),
        );

        // The action footer wraps inside the modal body (#3732) rather than the
        // single-line border title that silently clipped hints at narrow
        // widths; the list renders into the content area above it. Empty hint
        // keys keep the existing localized footer phrases as plain labels.
        let content = render_modal_footer(
            inner,
            buf,
            &[
                // `Type to filter` is already printed in the filter row two
                // lines above; saying it twice on one screen cost the row the
                // footer wrapped onto at 60 columns.
                ActionHint::new("", self.tr(MessageId::HelpFooterMove)),
                ActionHint::new("", self.tr(MessageId::HelpFooterJump)),
                // Directional tree controls are self-describing and avoid
                // injecting an English-only phrase into localized Help.
                ActionHint::new("←/→", ""),
                ActionHint::new("", self.tr(MessageId::HelpFooterClose)),
            ],
        );

        let mut lines: Vec<Line<'static>> = Vec::new();

        // The filter and the size of what it selected are one fact, so they
        // share one row: the count used to own a row of its own, and a blank
        // spacer owned the row under it. At 60x20 that was two of the eight
        // rows this overlay had left for content.
        let query_label = if self.query.is_empty() {
            self.tr(MessageId::HelpFilterPlaceholder).to_string()
        } else {
            format!("{}{}", self.tr(MessageId::HelpFilterPrefix), self.query)
        };
        let match_count = if self.query.is_empty() {
            format!("{} entries", self.entries.len())
        } else {
            format!("{} / {} matches", self.filtered.len(), self.entries.len())
        };
        let rows = self.render_rows();
        // Two header rows: the filter with its count, and the detail row
        // that restates the focused entry's description at panel width.
        let visible_rows = content.height.saturating_sub(2) as usize;
        let row_start = Self::visible_row_start(&rows, self.focus.as_ref(), visible_rows.max(1));
        // Reserve the rail before calculating column widths. Otherwise the
        // description column writes beneath the rail on compact terminals.
        let content = render_panel_scroll_rail(
            content,
            buf,
            rows.len(),
            row_start,
            visible_rows.max(1),
            true,
        );

        // Borders and padding eat 4 cells from each side (border 1 + padding
        // 1) × 2. The label column is measured from the labels each group
        // holds rather than fixed at 28, and the descriptions get everything
        // left over.
        let inner_width = content.width as usize;
        let label_cap = 28.min(inner_width.saturating_sub(8));
        let label_widths = self.label_widths(label_cap);

        // Measured against the rail-adjusted width so the right-aligned count
        // lands inside the list, not under the scroll rail.
        let gap = (content.width as usize)
            .saturating_sub(query_label.width() + match_count.width())
            .max(2);
        lines.push(Line::from(vec![
            Span::styled(
                query_label,
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" ".repeat(gap)),
            Span::styled(match_count, Style::default().fg(palette::TEXT_DIM)),
        ]));

        // A row cannot hold a command and a sentence at sixty columns, so the
        // list sheds descriptions there rather than cutting them. That is only
        // honest if the shed text is still reachable, so the focused entry
        // states its description here at the full width of the panel — where
        // most of them fit whole, and the rest shed at their own joints
        // instead of at a column boundary. The slot keeps its row whether or
        // not it is filled, so the list below does not jump as focus moves.
        let detail = self
            .focused_entry_detail(inner_width, label_cap, &label_widths)
            .unwrap_or_default();
        lines.push(Line::from(Span::styled(
            detail,
            Style::default().fg(palette::TEXT_MUTED),
        )));

        if self.filtered.is_empty() {
            lines.push(Line::from(Span::styled(
                self.tr(MessageId::HelpNoMatches),
                Style::default()
                    .fg(palette::TEXT_MUTED)
                    .add_modifier(Modifier::ITALIC),
            )));
        } else {
            // `content` is the body area above the wrapping footer (the block's
            // border, padding, and footer rows already removed), so budgeting
            // against its height keeps selected rows clear of the footer.
            let header_lines = lines.len();
            let visible_budget = (content.height as usize)
                .saturating_sub(header_lines)
                .max(1);

            for row in rows.iter().skip(row_start).take(visible_budget) {
                match *row {
                    HelpRenderRow::Group {
                        ref key,
                        ref label,
                        count,
                        collapsed,
                    } => {
                        let row_y = content.y.saturating_add(lines.len() as u16);
                        self.row_hitboxes.borrow_mut().push((
                            Rect::new(content.x, row_y, content.width, 1),
                            HelpHit::Group(key.clone()),
                        ));
                        // The selection cursor is `▸` and the collapsed
                        // chevron is `▸`. Printed side by side, a focused
                        // collapsed group read `▸ ▸ Slash commands (97)` —
                        // the same glyph twice for two different facts. The
                        // chevron stays, because it is this row's own state;
                        // focus is carried by the selection style, which is
                        // what carries it on every other row here.
                        let marker = if collapsed { "▸" } else { "▾" };
                        let is_focused = self.focus.as_ref() == Some(&HelpHit::Group(key.clone()));
                        let style = if is_focused {
                            menu_style::selected_row_style()
                        } else {
                            Style::default()
                                .fg(palette::WHALE_ACTION)
                                .add_modifier(Modifier::BOLD)
                        };
                        lines.push(Line::from(Span::styled(
                            format!("{marker} {label} ({count})"),
                            style,
                        )));
                    }
                    HelpRenderRow::Entry { slot, entry_idx } => {
                        let row_y = content.y.saturating_add(lines.len() as u16);
                        self.row_hitboxes.borrow_mut().push((
                            Rect::new(content.x, row_y, content.width, 1),
                            HelpHit::Entry(slot),
                        ));
                        let entry = &self.entries[entry_idx];
                        let is_selected = self.focus.as_ref() == Some(&HelpHit::Entry(slot));
                        let cursor =
                            format!("{} ", crate::tui::glyphs::selection_marker(is_selected));
                        let label_width = label_widths
                            .get(&group_key(entry))
                            .copied()
                            .unwrap_or(label_cap);
                        let pad = label_width.saturating_sub(entry.label.width());
                        let desc_capacity =
                            inner_width.saturating_sub(cursor.width() + label_width + 2);
                        let desc = shed_to_width(&entry.description, desc_capacity);
                        // The label is the string you type and the description
                        // qualifies it. They were both TEXT_PRIMARY, so the
                        // row said everything at one weight and the eye had
                        // nothing to skim down.
                        let (label_style, desc_style) = if is_selected {
                            (
                                menu_style::selected_row_style(),
                                menu_style::selected_row_style(),
                            )
                        } else {
                            (
                                Style::default().fg(palette::TEXT_PRIMARY),
                                Style::default().fg(palette::TEXT_DIM),
                            )
                        };
                        let mut spans = vec![
                            Span::styled(format!("{cursor}{}", entry.label), label_style),
                            Span::styled(" ".repeat(pad + 2), label_style),
                        ];
                        if !desc.is_empty() {
                            spans.push(Span::styled(desc.to_string(), desc_style));
                        }
                        lines.push(Line::from(spans));
                    }
                }
            }
        }

        Paragraph::new(lines).render(content, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_filter(view: &mut HelpView, text: &str) {
        for ch in text.chars() {
            view.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
    }

    fn first_filtered_section(view: &HelpView) -> HelpSection {
        view.entries[*view
            .filtered
            .first()
            .expect("help should contain at least one entry")]
        .section
    }

    #[test]
    fn empty_filter_lists_all_entries() {
        let view = HelpView::new();
        // Total = registered slash commands + catalogued keybindings.
        let expected = commands::command_infos().len() + KEYBINDINGS.len();
        assert_eq!(view.filtered.len(), expected);
        assert_eq!(view.entries.len(), expected);
    }

    #[test]
    fn entry_points_choose_the_section_they_promise() {
        let commands = HelpView::new_for_locale(Locale::En);
        assert_eq!(commands.ordering, HelpOrdering::CommandsFirst);
        assert_eq!(first_filtered_section(&commands), HelpSection::Command);

        let shortcuts = HelpView::new_with_ordering(Locale::En, HelpOrdering::KeybindingsFirst);
        assert_eq!(shortcuts.ordering, HelpOrdering::KeybindingsFirst);
        assert_eq!(first_filtered_section(&shortcuts), HelpSection::Keybinding);
    }

    #[test]
    fn workspace_commands_and_skills_are_findable_with_provenance() {
        // #3912: both surfaces executed and autocompleted but were absent
        // from the surface that teaches the product.
        let tmp = tempfile::TempDir::new().unwrap();
        crate::test_support::trust_workspace(tmp.path());
        let commands_dir = tmp.path().join(".codewhale").join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        std::fs::write(
            commands_dir.join("shipit.md"),
            "---\ndescription: Cut a release candidate\n---\nbody",
        )
        .unwrap();
        std::fs::write(
            commands_dir.join("secret.md"),
            "---\ndescription: Internal only\nhidden: true\n---\nbody",
        )
        .unwrap();

        let skills = vec![(
            "codereview".to_string(),
            "Review a diff for defects".to_string(),
        )];
        let mut view = HelpView::new_for_workspace(Locale::En, tmp.path(), &skills);

        let user = view
            .entries
            .iter()
            .find(|entry| entry.label == "/shipit")
            .expect("workspace command should be listed");
        assert_eq!(user.section, HelpSection::UserCommand);
        assert!(user.description.contains("Cut a release candidate"));

        let skill = view
            .entries
            .iter()
            .find(|entry| entry.label == "$codereview")
            .expect("discovered skill should be listed");
        assert_eq!(skill.section, HelpSection::Skill);

        assert!(
            !view.entries.iter().any(|entry| entry.label == "/secret"),
            "hidden workspace commands stay out of the overlay"
        );

        // Both are reachable through the existing substring filter.
        type_filter(&mut view, "shipit");
        assert!(
            view.filtered
                .iter()
                .any(|idx| view.entries[*idx].label == "/shipit")
        );

        let mut view = HelpView::new_for_workspace(Locale::En, tmp.path(), &skills);
        type_filter(&mut view, "review a diff");
        assert!(
            view.filtered
                .iter()
                .any(|idx| view.entries[*idx].label == "$codereview"),
            "skills are findable by their description"
        );
    }

    #[test]
    fn skill_rows_advertise_the_slash_skill_shape_too() {
        let tmp = tempfile::TempDir::new().unwrap();
        let skills = vec![("audit".to_string(), "Audit the tree".to_string())];
        let mut view = HelpView::new_for_workspace(Locale::En, tmp.path(), &skills);
        type_filter(&mut view, "/skill audit");
        assert!(
            view.filtered
                .iter()
                .any(|idx| view.entries[*idx].label == "$audit"),
            "searching the /skill form finds the skill"
        );
    }

    #[test]
    fn help_hides_builtins_with_shadowed_canonical_names() {
        let registry = commands::user_registry::UserCommandRegistry::from_loaded(vec![(
            "help".to_string(),
            "---\ndescription: Custom help workflow\n---\ncustom help".to_string(),
        )]);
        let entries = build_entries(Locale::En, &registry, &[]);

        // The built-in row is suppressed so the name is not advertised twice.
        assert!(
            !entries
                .iter()
                .any(|entry| entry.label == "/help" && entry.section == HelpSection::Command),
            "the shadowed built-in must not keep its own row"
        );
        // Since #3912 the shadowing workspace command supplies the row instead
        // of the name vanishing from help entirely.
        let user = entries
            .iter()
            .find(|entry| entry.label == "/help")
            .expect("the user command that shadows /help should be listed");
        assert_eq!(user.section, HelpSection::UserCommand);
        assert!(user.description.contains("Custom help workflow"));
    }

    #[test]
    fn substring_filter_narrows_to_command() {
        let mut view = HelpView::new();
        type_filter(&mut view, "mode [work");
        assert!(!view.filtered.is_empty());
        // Every filtered entry should genuinely contain the query in its
        // searchable haystack — no false positives slipped past.
        for idx in &view.filtered {
            assert!(
                view.entries[*idx].haystack.contains("mode [work"),
                "entry {:?} leaked through `mode [work` filter",
                view.entries[*idx]
            );
        }
        // The unified `/mode` command must surface when filtering for a
        // concrete mode value from the visible vocabulary.
        assert!(
            view.filtered
                .iter()
                .any(|idx| view.entries[*idx].label == "/mode"),
            "/mode should match the `mode [work` filter"
        );
    }

    #[test]
    fn substring_filter_finds_keybinding_by_chord() {
        let mut view = HelpView::new();
        type_filter(&mut view, "ctrl+r");
        assert!(!view.filtered.is_empty(), "Ctrl+R should match");
        assert!(
            view.filtered
                .iter()
                .any(|idx| view.entries[*idx].label.eq_ignore_ascii_case("ctrl+r")),
            "Ctrl+R chord must surface in the filtered set"
        );
    }

    #[test]
    fn multiple_terms_act_as_and() {
        let mut view = HelpView::new();
        type_filter(&mut view, "session picker");
        assert!(
            !view.filtered.is_empty(),
            "expected at least one entry mentioning both `session` and `picker`"
        );
        for idx in &view.filtered {
            let haystack = &view.entries[*idx].haystack;
            assert!(
                haystack.contains("session") && haystack.contains("picker"),
                "entry {:?} leaked through `session picker` AND filter",
                view.entries[*idx]
            );
        }
    }

    #[test]
    fn unknown_filter_yields_empty_set() {
        let mut view = HelpView::new();
        type_filter(&mut view, "zzzqqxxnope");
        assert!(view.filtered.is_empty());
        assert_eq!(view.selected, 0);
    }

    #[test]
    fn backspace_widens_match_set() {
        let mut view = HelpView::new();
        // Near-miss against the still-visible mode vocabulary so the last
        // character removes a unique miss and broadens the match set.
        type_filter(&mut view, "modez");
        let narrow = view.filtered.len();
        view.handle_key(key(KeyCode::Backspace));
        let wider = view.filtered.len();
        assert!(
            wider > narrow,
            "backspace must broaden the matching set (was {narrow}, now {wider})"
        );
    }

    #[test]
    fn ctrl_h_widens_match_set() {
        let mut view = HelpView::new();
        type_filter(&mut view, "modez");
        let narrow = view.filtered.len();
        view.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        let wider = view.filtered.len();
        assert!(
            wider > narrow,
            "Ctrl+H must behave as Backspace, broadening the matching set (was {narrow}, now {wider})"
        );
    }

    #[test]
    fn esc_closes_overlay() {
        let mut view = HelpView::new();
        let action = view.handle_key(key(KeyCode::Esc));
        assert!(matches!(action, ViewAction::Close));
    }

    #[test]
    fn ctrl_c_closes_overlay() {
        let mut view = HelpView::new();
        let action = view.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(matches!(action, ViewAction::Close));
    }

    #[test]
    fn help_search_owns_initial_q() {
        for query in ["queue", "Queue", "q 队列é"] {
            let mut stack = crate::tui::views::ViewStack::new();
            stack.push(HelpView::new());
            for ch in query.chars() {
                let modifiers = if ch.is_uppercase() {
                    KeyModifiers::SHIFT
                } else {
                    KeyModifiers::NONE
                };
                assert!(
                    stack
                        .handle_key(KeyEvent::new(KeyCode::Char(ch), modifiers))
                        .is_empty()
                );
                assert_eq!(stack.top_kind(), Some(ModalKind::Help), "{query:?}");
            }
            let mut modal = stack.pop().unwrap();
            let view = modal.as_any_mut().downcast_mut::<HelpView>().unwrap();
            assert_eq!(view.query, query);
            if query.eq_ignore_ascii_case("queue") {
                assert!(
                    view.filtered
                        .iter()
                        .any(|&i| view.entries[i].label == "/queue")
                );
            }
            view.handle_key(key(KeyCode::Backspace));
            assert_eq!(
                view.query,
                query
                    .chars()
                    .take(query.chars().count() - 1)
                    .collect::<String>()
            );
            assert!(matches!(
                view.handle_key(key(KeyCode::Esc)),
                ViewAction::Close
            ));
        }
    }

    #[test]
    fn arrow_keys_move_selection_and_wrap_edges() {
        let mut view = HelpView::new();
        let focusable = view.focusable_rows();
        assert!(
            focusable.len() >= 3,
            "need at least three visible help rows"
        );
        // Help opens on the first entry, not the header above it: the detail
        // row under the filter reads the focused entry, and a header has no
        // description to put there.
        assert_eq!(view.focus.as_ref(), Some(&focusable[1]));
        // Up returns to its group; another Up wraps to the final visible row.
        view.handle_key(key(KeyCode::Up));
        assert_eq!(view.focus.as_ref(), focusable.first());
        view.handle_key(key(KeyCode::Up));
        assert_eq!(view.focus.as_ref(), focusable.last());
        // Down from last wraps to first; End still jumps to the last visible row.
        view.handle_key(key(KeyCode::Down));
        assert_eq!(view.focus.as_ref(), focusable.first());
        view.handle_key(key(KeyCode::Down));
        assert_eq!(view.focus.as_ref(), Some(&focusable[1]));
        view.handle_key(key(KeyCode::End));
        assert_eq!(view.focus.as_ref(), focusable.last());
    }

    #[test]
    fn mouse_click_selects_visible_help_row() {
        let mut view = HelpView::new();
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let (rect, slot) = view
            .row_hitboxes
            .borrow()
            .iter()
            .find_map(|(rect, hit)| match hit {
                HelpHit::Entry(slot) => Some((*rect, *slot)),
                HelpHit::Group(_) => None,
            })
            .expect("at least one entry hitbox");

        view.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(view.selected, slot);
        assert_eq!(view.focus, Some(HelpHit::Entry(slot)));
    }

    /// `/help` used to open on all 103 slash commands sorted alphabetically —
    /// a catalog, not an answer, and it buried the handful of commands a
    /// session actually steers itself with. It opens on the curated group now,
    /// with the catalog one keystroke below it.
    #[test]
    fn help_opens_on_the_curated_commands_with_the_catalog_folded() {
        let view = HelpView::new();
        assert!(
            !view.group_is_collapsed("cmd:common"),
            "the curated commands are the point of opening Help"
        );
        assert!(
            view.group_is_collapsed("cmd:all"),
            "the full catalog stays folded until asked for"
        );

        let rows = view.render_rows();
        let entries = rows
            .iter()
            .filter(|row| matches!(row, HelpRenderRow::Entry { .. }))
            .count();
        assert!(
            entries <= COMMON_COMMANDS.len(),
            "Help opened with {entries} rows; only the curated set should be expanded: {:?}",
            rows.iter()
                .filter_map(|row| match row {
                    HelpRenderRow::Entry { entry_idx, .. } =>
                        Some(view.entries[*entry_idx].label.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        );

        // Every curated command is a real registered command, and each is on
        // screen. A typo here would silently shrink the opening view.
        // Distinct labels: a workspace command may share a built-in's name,
        // and that is a naming collision, not a missing curated entry.
        let shown: std::collections::BTreeSet<&str> = view
            .filtered
            .iter()
            .map(|idx| view.entries[*idx].label.as_str())
            .filter(|label| COMMON_COMMANDS.contains(label))
            .collect();
        assert_eq!(
            shown.len(),
            COMMON_COMMANDS.len(),
            "curated commands missing from the registry: {:?}",
            COMMON_COMMANDS
                .iter()
                .filter(|name| !shown.contains(*name))
                .collect::<Vec<_>>()
        );

        // Typing reaches the folded catalog without expanding anything by hand.
        let mut view = view;
        type_filter(&mut view, "/advisor");
        assert!(
            view.filtered
                .iter()
                .any(|idx| view.entries[*idx].label == "/advisor"),
            "a query must reach commands inside the folded catalog"
        );
    }

    /// Help opens with the full command catalog folded. Tests about layout,
    /// scrolling or a specific catalog command open it first — what they
    /// exercise is the rendering of those rows, not the default fold state,
    /// which `help_opens_on_the_curated_commands` covers on its own.
    fn view_with_catalog_open() -> HelpView {
        let mut view = HelpView::new();
        view.toggle_group("cmd:all");
        view.focus = None;
        view
    }

    #[test]
    fn visible_window_keeps_selected_entry_visible_after_scroll() {
        let mut view = view_with_catalog_open();
        let selected = view
            .filtered
            .iter()
            .position(|idx| view.entries[*idx].label == "/home")
            .expect("/home command should be present");
        view.selected = selected;
        view.focus = Some(HelpHit::Entry(selected));

        let rows = view.render_rows();
        let row_start = HelpView::visible_row_start(&rows, view.focus.as_ref(), 12);
        let visible = &rows[row_start..(row_start + 12).min(rows.len())];

        assert!(
            visible
                .iter()
                .any(|row| matches!(row, HelpRenderRow::Entry { slot, .. } if *slot == selected)),
            "selected help entry should stay in the visible render window"
        );
    }

    fn rows_at(view: &HelpView, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        (area.top()..area.bottom())
            .map(|y| {
                (area.left()..area.right())
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    /// House rule, and the thing the overlay broke worst. A trailing `…` in a
    /// list of two hundred rows promises text no keystroke can reveal, and it
    /// lands mid-token: `(aliases: /qin…` and `deepseek-v4-…` name nothing,
    /// because these strings share prefixes.
    #[test]
    fn no_row_advertises_truncation_at_any_width() {
        for width in [60u16, 80, 96, 120] {
            let view = HelpView::new();
            for row in rows_at(&view, width, 24) {
                assert!(
                    !row.contains('…'),
                    "help must shed, not truncate, at {width} columns: {row:?}"
                );
            }
        }
    }

    /// A cut inside `(aliases: /image, /media)` leaves the parenthesis hanging
    /// open — no ellipsis, and still a broken row. Joints only count where the
    /// parentheses balance.
    #[test]
    fn shedding_never_leaves_a_parenthesis_open() {
        let text = "Attach media  (aliases: /image, /media)";
        for width in 4..text.len() {
            let shed = shed_to_width(text, width);
            let opens = shed.matches('(').count();
            let closes = shed.matches(')').count();
            assert_eq!(opens, closes, "unbalanced at width {width}: {shed:?}");
        }
    }

    /// A single-clause description has no joint to shed at. Shedding the whole
    /// field left a bare label beside rows that still had text, which reads as
    /// a broken renderer; the short form stops on a whole word instead, and
    /// does not end on a dangling `to an`.
    #[test]
    fn a_jointless_description_sheds_to_whole_words() {
        let text = "Move the active branch to an existing session entry";
        let shed = shed_to_width(text, 26);
        assert!(text.starts_with(&*shed), "{shed:?}");
        assert!(!shed.is_empty());
        assert!(!shed.ends_with(" an"), "{shed:?}");
        assert!(!shed.ends_with(" to"), "{shed:?}");
        assert!(!shed.ends_with('…'), "{shed:?}");
    }

    /// The label column was a flat 28 columns at every terminal size, so at 60
    /// columns twenty blank cells sat between `/advisor` and a description cut
    /// to 21. It is measured from the labels each group holds instead — and
    /// measured in the rendered row, not just in the helper, because a helper
    /// the renderer ignores proves nothing.
    #[test]
    fn label_column_is_measured_from_the_group_not_fixed() {
        let view = view_with_catalog_open();
        let widest = view
            .entries
            .iter()
            .filter(|entry| {
                entry.section == HelpSection::Command && entry.sub_rank == COMMAND_RANK_ALL
            })
            .map(|entry| entry.label.width())
            .max()
            .expect("commands exist");
        assert!(
            widest < 28,
            "slash command labels are short; the fixture assumes it"
        );
        assert_eq!(view.label_widths(28).get("cmd:all").copied(), Some(widest));

        // Tall enough to reach past the curated group into the catalog.
        let rows = rows_at(&view, 60, 60);
        let row = rows
            .iter()
            .find(|row| row.contains("/advisor"))
            .expect("advisor row");
        let label_at = row.find("/advisor").expect("label");
        let description_at = row[label_at..]
            .find("Toggle")
            .map(|offset| label_at + offset)
            .expect("description follows the label on the same row");
        assert!(
            description_at - label_at <= widest + 2,
            "description must start right after the widest label in the group, \
             not after a flat 28-column gutter: {row:?}"
        );
    }

    /// At 60x20 the description slot is 35 columns. `/automation`'s
    /// "Manage durable scheduled automations" is 36, so the last-resort
    /// word shed printed "Manage durable scheduled" — the adjectives
    /// without the noun that says what is being managed.
    #[test]
    fn sixty_column_help_keeps_the_automation_noun() {
        let view = view_with_catalog_open();
        let rows = rows_at(&view, 60, 60);
        let row = rows
            .iter()
            .find(|row| row.contains("/automation"))
            .expect("/automation is a registered command");
        assert!(
            row.contains("automations"),
            "/automation lost the noun it manages: {row:?}"
        );
    }

    /// The selection cursor and the collapsed chevron are both `▸`. Printed
    /// side by side, a focused collapsed group read `▸ ▸ Slash commands (97)`
    /// — one glyph, twice, for two different facts.
    #[test]
    fn a_group_header_spends_one_glyph_on_one_meaning() {
        let mut view = view_with_catalog_open();
        view.toggle_group("cmd:all");
        assert_eq!(view.focus, Some(HelpHit::Group("cmd:all".to_string())));
        let rows = rows_at(&view, 96, 60);
        let header = rows
            .iter()
            .find(|row| row.contains("All commands"))
            .expect("group header row");
        assert!(!header.contains("▸ ▸"), "{header:?}");
        assert!(
            header.contains('▸'),
            "collapsed state still shown: {header:?}"
        );
    }

    /// The detail row repairs a shed; it never repeats one. On a wide terminal
    /// the inline description already fits, so the slot carries the usage
    /// line alone rather than printing the same sentence twice on one screen.
    #[test]
    fn the_detail_row_repairs_a_shed_and_never_repeats_one() {
        let mut view = view_with_catalog_open();
        let slot = view
            .filtered
            .iter()
            .position(|idx| view.entries[*idx].label == "/advisor")
            .expect("/advisor is a registered command");
        view.set_focus(HelpHit::Entry(slot));
        let entry = &view.entries[view.filtered[slot]];
        let description = entry.description.clone();

        let wide = rows_at(&view, 140, 24);
        let occurrences = wide
            .iter()
            .filter(|row| row.contains(description.trim()))
            .count();
        assert_eq!(
            occurrences, 1,
            "wide terminal must not say it twice: {wide:#?}"
        );

        let narrow = rows_at(&view, 60, 20);
        let detail_row = narrow
            .iter()
            .position(|row| row.contains("Type to filter"))
            .expect("filter row")
            + 1;
        // The scroll rail paints the last column of every row.
        let strip_rail = |row: &str| {
            row.trim_end_matches(['█', '│', '┃', ' '])
                .trim()
                .to_string()
        };
        let detail = strip_rail(narrow.get(detail_row).expect("detail row"));
        assert!(
            !detail.is_empty(),
            "narrow terminal must repair the shed: {narrow:#?}"
        );
        assert!(description.starts_with(&detail), "{detail:?}");
        let inline = strip_rail(
            narrow
                .iter()
                .find(|row| row.contains("/advisor"))
                .expect("advisor row"),
        );
        let inline_description = inline
            .split_once("/advisor")
            .map(|(_, rest)| rest.trim().to_string())
            .unwrap_or_default();
        assert!(
            detail.len() > inline_description.len(),
            "the detail row must carry more than the row could: {inline_description:?} / {detail:?}"
        );
    }

    /// #5952: the usage string lived in the search haystack alone, so
    /// `/workspace [path|worktrees]` rendered as `/workspace` and the
    /// worktree manager behind it had no way of being seen.
    #[test]
    fn the_focused_command_states_its_usage() {
        let mut view = HelpView::new();
        let slot = view
            .filtered
            .iter()
            .position(|idx| view.entries[*idx].label == "/workspace")
            .expect("/workspace is a registered command");
        view.set_focus(HelpHit::Entry(slot));

        let rows = rows_at(&view, 140, 24);
        assert!(
            rows.iter()
                .any(|row| row.contains("/workspace [path|worktrees]")),
            "the usage line must be on screen: {rows:#?}"
        );
    }

    /// A row whose usage only restates its label spends no columns saying so.
    #[test]
    fn a_command_without_arguments_states_no_usage() {
        let view = HelpView::new();
        let entry = view
            .entries
            .iter()
            .find(|entry| entry.label == "/copy")
            .expect("/copy is a registered command");
        assert_eq!(entry.usage, None);

        let workspace = view
            .entries
            .iter()
            .find(|entry| entry.label == "/workspace")
            .expect("/workspace is a registered command");
        assert_eq!(
            workspace.usage.as_deref(),
            Some("/workspace [path|worktrees]")
        );
    }

    /// At 60 columns the detail slot cannot hold both, and the description it
    /// exists to repair wins — the usage sheds at its own joint rather than
    /// pushing the sentence off the panel.
    #[test]
    fn a_narrow_panel_keeps_the_repaired_description_over_the_usage() {
        let mut view = HelpView::new();
        let slot = view
            .filtered
            .iter()
            .position(|idx| view.entries[*idx].label == "/advisor")
            .expect("/advisor is a registered command");
        view.set_focus(HelpHit::Entry(slot));
        let description = view.entries[view.filtered[slot]].description.clone();

        let narrow = rows_at(&view, 60, 20);
        let detail_row = narrow
            .iter()
            .position(|row| row.contains("Type to filter"))
            .expect("filter row")
            + 1;
        let detail = narrow
            .get(detail_row)
            .expect("detail row")
            .trim_end_matches(['█', '│', '┃', ' '])
            .trim()
            .to_string();
        assert!(
            description.starts_with(&detail) && !detail.is_empty(),
            "the narrow detail row must still be the description: {detail:?}"
        );
    }

    /// Workspace commands declare their own usage in front matter; the row
    /// reads it from the same place the built-ins read theirs.
    #[test]
    fn a_workspace_command_states_its_declared_usage() {
        let tmp = tempfile::TempDir::new().unwrap();
        crate::test_support::trust_workspace(tmp.path());
        let commands_dir = tmp.path().join(".codewhale").join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        std::fs::write(
            commands_dir.join("shipit.md"),
            "---\ndescription: Ship the branch\nargument-hint: <environment>\n---\nbody",
        )
        .unwrap();

        let view = HelpView::new_for_workspace(Locale::En, tmp.path(), &[]);
        let entry = view
            .entries
            .iter()
            .find(|entry| entry.label == "/shipit")
            .expect("workspace command row");
        assert_eq!(entry.usage.as_deref(), Some("/shipit <environment>"));
    }

    #[test]
    fn render_keeps_next_row_after_help_visible() {
        let mut view = HelpView::new();
        let help_slot = view
            .filtered
            .iter()
            .position(|idx| view.entries[*idx].label == "/help")
            .expect("/help command should be present");
        view.selected = help_slot;
        view.focus = Some(HelpHit::Entry(help_slot));
        view.handle_key(key(KeyCode::Down));
        let selected_slot = match view.focus {
            Some(HelpHit::Entry(slot)) => slot,
            ref other => panic!("expected entry focus after /help, got {other:?}"),
        };
        let selected_idx = view.filtered[selected_slot];
        let selected_label = view.entries[selected_idx].label.clone();

        let area = Rect::new(0, 0, 96, 32);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        let mut highlighted_label = false;
        for y in area.top()..area.bottom() {
            let mut row = String::new();
            let mut row_has_highlight = false;
            for x in area.left()..area.right() {
                let cell = &buf[(x, y)];
                row.push_str(cell.symbol());
                row_has_highlight |=
                    cell.bg == palette::SELECTION_BG && cell.fg == palette::SELECTION_TEXT;
            }
            if row_has_highlight && row.contains(&selected_label) {
                highlighted_label = true;
                break;
            }
        }

        assert!(
            highlighted_label,
            "selected row after /help should stay visibly highlighted"
        );
    }

    #[test]
    fn selected_help_row_uses_selection_highlight() {
        let view = HelpView::new();
        let area = Rect::new(0, 0, 96, 32);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        let mut found_highlight = false;
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let cell = &buf[(x, y)];
                if cell.bg == palette::SELECTION_BG && cell.fg == palette::SELECTION_TEXT {
                    found_highlight = true;
                    break;
                }
            }
        }

        assert!(
            found_highlight,
            "selected row should use the semantic selection highlight"
        );
    }

    #[test]
    fn render_includes_help_chrome_for_empty_filter() {
        let view = view_with_catalog_open();
        let area = Rect::new(0, 0, 96, 32);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        let dump = buffer_text(&buf, area);
        // Title border + section headings should always render.
        assert!(dump.contains("Help"), "missing help title:\n{dump}");
        assert!(
            dump.contains("Type to filter"),
            "missing filter prompt:\n{dump}"
        );
        // Help opens on the curated set with the catalog folded beneath it,
        // so both group headings are part of the chrome a user always sees.
        assert!(
            dump.contains("Common commands"),
            "missing curated-command heading:\n{dump}"
        );
        assert!(
            dump.contains("All commands"),
            "missing command-catalog heading:\n{dump}"
        );
        // Footer hint should advertise close key on the bottom border.
        assert!(
            dump.contains("Esc close"),
            "missing Esc close footer hint:\n{dump}"
        );
    }

    #[test]
    fn render_with_filter_shows_only_matching_section_and_status() {
        let mut view = HelpView::new();
        type_filter(&mut view, "mode [work");
        let area = Rect::new(0, 0, 96, 24);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        let dump = buffer_text(&buf, area);
        assert!(
            dump.contains("Filter: mode [work"),
            "filter echo missing:\n{dump}"
        );
        assert!(
            dump.contains("matches"),
            "match counter missing in dump:\n{dump}"
        );
        assert!(
            dump.contains("/mode"),
            "expected /mode command in filtered render:\n{dump}"
        );
        assert!(
            !dump.contains("/model"),
            "non-matching commands should not render under a `mode [work` filter:\n{dump}"
        );
    }

    #[test]
    fn localized_help_chrome_renders_without_missing_markers() {
        let view = HelpView::new_for_locale(Locale::ZhHans);
        let area = Rect::new(0, 0, 48, 18);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        let dump = buffer_text(&buf, area);
        assert!(
            dump.contains('帮') && dump.contains('助'),
            "missing localized title:\n{dump}"
        );
        assert!(
            !dump.contains("MISSING"),
            "missing-key marker leaked:\n{dump}"
        );
    }

    #[test]
    fn localized_help_keybinding_descriptions_use_zh_hans() {
        let registry = commands::user_registry::UserCommandRegistry::new();
        let entries = build_entries(Locale::ZhHans, &registry, &[]);
        let kb_entries: Vec<_> = entries
            .iter()
            .filter(|e| e.section == HelpSection::Keybinding)
            .collect();
        assert!(!kb_entries.is_empty(), "no keybinding entries found");

        for entry in &kb_entries {
            let group = group_label(entry, Locale::ZhHans);
            assert!(
                group
                    .chars()
                    .any(|c| { ('\u{4e00}'..='\u{9fff}').contains(&c) }),
                "keybinding group not localized: {group} ({})",
                entry.description
            );
        }
    }

    /// The four terminal sizes the v0.8.66 modal blocker (#3732) requires
    /// every overlay to remain readable and fully operable at.
    const BLOCKER_SIZES: [(u16, u16); 4] = [(80, 24), (100, 30), (120, 32), (160, 40)];

    const SHORTCUT_HELP_SIZES: [(u16, u16); 5] =
        [(40, 12), (60, 16), (80, 24), (100, 32), (140, 40)];

    #[test]
    fn shortcut_help_leads_with_keys_at_responsive_sizes() {
        use crate::tui::views::ViewStack;

        let keybindings_heading = tr(Locale::En, MessageId::HelpSectionNavigation);
        let commands_heading = tr(Locale::En, MessageId::HelpSlashCommands);

        for (w, h) in SHORTCUT_HELP_SIZES {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            for y in 0..h {
                for x in 0..w {
                    buf[(x, y)].set_symbol("§");
                }
            }

            let mut stack = ViewStack::new();
            stack.push(HelpView::new_with_ordering(
                Locale::En,
                HelpOrdering::KeybindingsFirst,
            ));
            stack.render(area, &mut buf);

            let rows: Vec<String> = (0..h)
                .map(|y| {
                    (0..w)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect();
            let text = rows.join("\n");
            let keys_at = text.find(keybindings_heading.as_ref()).unwrap_or_else(|| {
                panic!("{w}x{h}: shortcut Help hid the keybindings heading:\n{text}")
            });
            if let Some(commands_at) = text.find(commands_heading.as_ref()) {
                assert!(
                    keys_at < commands_at,
                    "{w}x{h}: shortcut Help rendered commands before keybindings:\n{text}"
                );
            }
            assert!(
                !text.contains('§'),
                "{w}x{h}: background bleed-through into shortcut Help"
            );
            assert!(
                (0..h).any(|y| {
                    (0..w).any(|x| {
                        let cell = &buf[(x, y)];
                        cell.bg == palette::SELECTION_BG && cell.fg == palette::SELECTION_TEXT
                    })
                }),
                "{w}x{h}: first keybinding row lost its selection highlight"
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
    fn help_is_usable_and_opaque_at_blocker_sizes() {
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
            stack.push(HelpView::new_for_locale(Locale::En));
            stack.render(area, &mut buf);

            let rows: Vec<String> = (0..h)
                .map(|y| {
                    (0..w)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect();
            let text = rows.join("\n");

            // `type to filter` is deliberately absent: the filter row prints
            // `Type to filter` two lines above, and at 60 columns saying it
            // twice pushed the footer onto a second row.
            for label in [
                "Type to filter",
                "Up/Down move",
                "PgUp/PgDn jump",
                "Esc close",
            ] {
                assert!(text.contains(label), "{w}x{h}: missing footer '{label}'");
            }
            assert!(
                !text.contains('X'),
                "{w}x{h}: background bleed-through into modal surface"
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
    fn shortcuts_open_folds_the_long_tail() {
        let view = HelpView::new_with_ordering(Locale::En, HelpOrdering::KeybindingsFirst);
        let rows = view.render_rows();
        let groups: Vec<&str> = rows
            .iter()
            .filter_map(|row| match row {
                HelpRenderRow::Group {
                    label, collapsed, ..
                } => Some((*collapsed, label.as_str())),
                _ => None,
            })
            .map(|(collapsed, label)| {
                if collapsed {
                    label
                } else {
                    // keep expanded groups in a second pass
                    label
                }
            })
            .collect();
        assert!(
            groups.contains(&"Navigation"),
            "shortcuts should surface Navigation: {groups:?}"
        );
        assert!(
            rows.iter().any(|row| matches!(
                row,
                HelpRenderRow::Group {
                    collapsed: false,
                    ..
                }
            )),
            "at least one group stays open"
        );
        assert!(
            rows.iter().any(|row| matches!(
                row,
                HelpRenderRow::Group {
                    collapsed: true,
                    ..
                }
            )),
            "the long tail should start collapsed"
        );
        assert!(
            !rows.iter().any(|row| matches!(
                row,
                HelpRenderRow::Entry { entry_idx, .. }
                    if view.entries[*entry_idx].section == HelpSection::Command
            )),
            "slash commands stay folded until the user expands or searches"
        );
    }

    #[test]
    fn enter_toggles_the_selected_group() {
        let mut view = HelpView::new_with_ordering(Locale::En, HelpOrdering::KeybindingsFirst);
        // Focus opens on the first entry, so step up onto its header first.
        view.handle_key(key(KeyCode::Up));
        assert!(matches!(view.focus, Some(HelpHit::Group(_))));
        let before = view.visible_entry_slots().len();
        view.handle_key(key(KeyCode::Enter));
        let after = view.visible_entry_slots().len();
        assert_ne!(
            before, after,
            "Enter should fold or unfold the selected group's members"
        );
        view.handle_key(key(KeyCode::Enter));
        assert_eq!(
            view.visible_entry_slots().len(),
            before,
            "a second Enter restores the previous fold"
        );
    }

    #[test]
    fn right_expands_and_left_collapses_a_focused_header() {
        let mut view = HelpView::new_with_ordering(Locale::En, HelpOrdering::KeybindingsFirst);
        let group_key = "cmd:all".to_string();
        assert!(view.group_is_collapsed(&group_key));
        view.focus = Some(HelpHit::Group(group_key.clone()));

        view.handle_key(key(KeyCode::Right));
        assert!(!view.group_is_collapsed(&group_key));
        assert_eq!(view.focus, Some(HelpHit::Group(group_key.clone())));

        view.handle_key(key(KeyCode::Left));
        assert!(view.group_is_collapsed(&group_key));
        assert_eq!(view.focus, Some(HelpHit::Group(group_key)));
    }

    #[test]
    fn mouse_click_on_group_header_matches_enter_toggle() {
        let mut view = HelpView::new_with_ordering(Locale::En, HelpOrdering::KeybindingsFirst);
        let area = Rect::new(0, 0, 100, 120);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let (rect, group) = view
            .row_hitboxes
            .borrow()
            .iter()
            .find_map(|(rect, hit)| match hit {
                HelpHit::Group(key) if view.group_is_collapsed(key) => Some((*rect, key.clone())),
                _ => None,
            })
            .expect("at least one collapsed group header is visible");

        view.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });

        assert!(!view.group_is_collapsed(&group));
        assert_eq!(view.focus, Some(HelpHit::Group(group)));
    }

    #[test]
    fn search_unfolds_collapsed_groups() {
        let mut view = HelpView::new_with_ordering(Locale::En, HelpOrdering::KeybindingsFirst);
        assert!(
            view.group_is_collapsed("cmd:all"),
            "slash commands start collapsed on the shortcuts surface"
        );
        type_filter(&mut view, "/mode");
        assert!(
            !view.group_is_collapsed("cmd:all"),
            "a search query must reveal matching groups"
        );
        assert!(
            view.filtered
                .iter()
                .any(|idx| view.entries[*idx].label == "/mode")
        );
    }

    #[test]
    fn help_expand_groups_starts_unfolded() {
        let view = HelpView::new_with_ordering(Locale::En, HelpOrdering::KeybindingsFirst)
            .with_groups_expanded(true);
        assert!(
            !view.group_is_collapsed("cmd:all"),
            "help_expand_groups must start with slash commands visible"
        );
        assert!(
            view.render_rows().iter().any(|row| matches!(
                row,
                HelpRenderRow::Entry { entry_idx, .. }
                    if view.entries[*entry_idx].section == HelpSection::Command
            )),
            "expanded shortcuts include slash command rows"
        );
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
}

#[cfg(test)]
mod shed_to_words_script_tests {
    use super::{shed_to_words, widest_char_prefix};
    use unicode_width::UnicodeWidthStr;

    /// Japanese, Chinese and Thai do not put spaces between words, so a
    /// word-boundary scan finds nothing and used to yield an empty string —
    /// every help description rendered blank in those locales.
    #[test]
    fn a_script_without_spaces_still_gets_a_description() {
        for text in [
            "バックグラウンドのアドバイザーを切り替える",
            "切换后台顾问",
            "切換背景顧問",
        ] {
            for width in [8usize, 12, 20, 30] {
                let shed = shed_to_words(text, width);
                assert!(
                    !shed.is_empty(),
                    "{text:?} at {width}: description rendered blank",
                );
                assert!(
                    shed.width() <= width,
                    "{text:?} at {width}: {shed:?} overflows ({} cols)",
                    shed.width(),
                );
                assert!(
                    text.starts_with(&*shed),
                    "{shed:?} is not a prefix of {text:?}"
                );
            }
        }
    }

    /// The same hole opens in English whenever the first space sits past the
    /// budget: the scan never fires and the row goes blank.
    #[test]
    fn an_overlong_first_word_sheds_to_characters_rather_than_nothing() {
        let text = "Internationalisation settings";
        let shed = shed_to_words(text, 10);
        assert!(!shed.is_empty(), "long first word rendered blank");
        assert!(shed.width() <= 10, "{shed:?}");
    }

    /// Ordinary English is unchanged: still cut on a word boundary, still
    /// drops a trailing short function word.
    #[test]
    fn english_still_sheds_on_word_boundaries() {
        let text = "Toggle the background advisor for this session";
        let shed = shed_to_words(text, 24);
        assert!(shed.width() <= 24, "{shed:?}");
        assert!(!shed.ends_with(' '), "{shed:?}");
        assert!(
            shed.split(' ').count() > 1 && text.starts_with(&*shed),
            "{shed:?} should be a whole-word prefix",
        );
    }

    #[test]
    fn widest_char_prefix_never_splits_a_character() {
        let text = "日本語テキスト";
        for width in 0..=14 {
            let prefix = widest_char_prefix(text, width);
            assert!(text.starts_with(prefix));
            assert!(prefix.width() <= width);
        }
    }

    /// The last-resort word shed always ended on a space, so the last word
    /// of a simple verb + modifier + noun phrase was dropped even when it
    /// fitted, and when it overflowed by one column the two-pass short-word
    /// trim left the adjectives without the noun they qualify.
    #[test]
    fn a_simple_noun_phrase_keeps_the_head_noun() {
        let text = "Manage durable scheduled automations";
        // 24 fits "Manage durable scheduled" exactly and not the noun.
        // 35 is the description slot at 60 columns (measured).
        // 36 is the full phrase.
        for width in [24usize, 28, 32, 35, 36] {
            let shed = shed_to_words(text, width);
            assert!(
                shed.contains("automations"),
                "width {width} dropped the head noun: {shed:?}"
            );
            assert!(
                shed.width() <= width,
                "width {width} overflowed: {shed:?} ({} cols)",
                shed.width()
            );
        }

        // Help appends `  (aliases: …)` onto the same row. The joint head is
        // one column over the 35-column slot, so last-resort word shed must
        // run on that clause, not on the alias list.
        let aliased = "Manage durable scheduled automations  (aliases: /automations, /scheduled)";
        let shed = super::shed_to_width(aliased, 35);
        assert!(
            shed.contains("automations"),
            "aliased row dropped the head noun: {shed:?}"
        );
        assert!(
            !shed.contains("aliases"),
            "aliased row kept the alias list instead of the clause: {shed:?}"
        );
        assert!(shed.width() <= 35, "{shed:?}");
    }
}
