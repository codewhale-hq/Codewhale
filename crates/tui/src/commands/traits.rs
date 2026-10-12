//! Command traits and registry support.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::tui::app::App;
use codewhale_localization::{Locale, MessageId, tr};

use super::CommandResult;

#[derive(Debug, Clone, Copy)]
pub struct CommandInfo {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub usage: &'static str,
    pub description_id: MessageId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandDiscovery {
    Primary,
    Advanced,
    Compatibility,
}

pub(crate) const ADVANCED_DISCOVERY_COMMANDS: &[&str] = &[
    "anchor",
    "balance",
    "cache",
    "change",
    "context",
    "diff",
    "edit",
    "hf",
    "lsp",
    "modeldb",
    "models",
    "network",
    "plugin",
    "preview-request",
    "profile",
    "purge",
    "relay",
    "rename",
    "rlm",
    "settings",
    "share",
    "workbar",
    "status",
    "system",
    "theme",
    "tools",
    "trust",
    "verbose",
];

pub(crate) const COMPATIBILITY_DISCOVERY_COMMANDS: &[&str] = &["subagents"];

/// Commands that exist and run, but are not advertised anywhere the operator
/// browses: not in slash completion, not in `/help`, not in the palette.
///
/// This is for surfaces that are real but not ready to be recommended. Typing
/// the command still works, the `codewhale <verb>` CLI is untouched, and the
/// hotbar still registers a binding for it — `codewhale-lane` resolves its
/// control-plane actions through that registry, so it is a substrate rather
/// than a place to browse. The entry here only stops the product from
/// *teaching* a route it is not standing behind yet. Founder, 2026-09-03:
/// "remove /lane".
pub(crate) const UNLISTED_COMMANDS: &[&str] = &["lane"];

/// Small, task-oriented starting set for a bare `/` in the composer.
///
/// The full command catalog remains searchable through `/help`, the command
/// palette, and by typing any command prefix. `agents` is the preferred alias
/// for the compatibility-owned `subagents` command.
pub(crate) const BARE_SLASH_DISCOVERY_COMMANDS: &[&str] =
    &["help", "setup", "model", "settings", "resume", "rc"];

#[must_use]
pub(crate) fn bare_slash_discovery_rank(name: &str) -> Option<usize> {
    BARE_SLASH_DISCOVERY_COMMANDS
        .iter()
        .position(|entry| *entry == name)
}

/// Built-in commands that the palette pastes into the composer instead of
/// executing, even though they have no *required* argument.
///
/// Prefer keeping this empty. Every name here must be a registered canonical
/// command name — see `palette_paste_only_names_are_registered` in the
/// command palette tests.
pub(crate) const PALETTE_PASTE_ONLY: &[&str] = &[];

impl CommandDiscovery {
    pub fn show_at_root(self) -> bool {
        matches!(self, CommandDiscovery::Primary)
    }
}

/// Bare words in a usage line that stand for a value the operator supplies
/// rather than a literal token they type. `/workspace [path|worktrees]` reads
/// the same to a parser either way, so the metavariables are named once here
/// instead of being re-encoded as a second usage source next to the registry.
const USAGE_METAVARIABLES: &[&str] = &[
    "args", "command", "days", "dir", "key", "message", "model", "name", "path", "prompt", "query",
    "text", "url", "value",
];

/// Literal subcommands declared by a `usage` line, in declaration order.
///
/// The registry's `usage` strings are the only place argument shapes are
/// written down, so this reads them instead of adding a parallel
/// `subcommands` field that would drift. It is deliberately conservative:
/// anything that does not look like a typed word — `<placeholder>`, `--flag`,
/// `snake_case` metavariables, `path/to/file` — is skipped, so an unfamiliar
/// usage shape yields no hint rather than a wrong one.
#[must_use]
pub fn usage_subcommands(usage: &str) -> Vec<&str> {
    let Some((_, rest)) = usage.trim().split_once(char::is_whitespace) else {
        return Vec::new();
    };
    // Only the first argument group names subcommands. A later group is a
    // modifier on whichever one was chosen (`/workbar […] [--save]`).
    let group = first_top_level_group(rest.trim_start());
    let mut subcommands: Vec<&str> = Vec::new();
    for alternative in split_top_level_alternatives(strip_one_bracket(group)) {
        let Some(token) = leading_literal_token(alternative) else {
            continue;
        };
        if !subcommands.contains(&token) {
            subcommands.push(token);
        }
    }
    subcommands
}

/// The first whitespace-separated chunk of `rest`, counting `[` and `<` so a
/// group containing spaces (`[open <id>|prune <days>]`) stays whole.
fn first_top_level_group(rest: &str) -> &str {
    let mut depth = 0usize;
    for (idx, ch) in rest.char_indices() {
        match ch {
            '[' | '<' => depth += 1,
            ']' | '>' => depth = depth.saturating_sub(1),
            _ if ch.is_whitespace() && depth == 0 => return &rest[..idx],
            _ => {}
        }
    }
    rest
}

/// Remove one enclosing `[…]`, or one enclosing `<…>` that fences a choice.
///
/// `<entry_id>` is a value and stays wrapped so it is skipped later;
/// `<turn <n>|plan>` is a required choice between literal verbs and is opened.
fn strip_one_bracket(group: &str) -> &str {
    let (open, close) = match group.chars().next() {
        Some('[') => ('[', ']'),
        Some('<') => ('<', '>'),
        _ => return group,
    };
    if !group.ends_with(close) {
        return group;
    }
    let inner = &group[open.len_utf8()..group.len() - close.len_utf8()];
    if !brackets_balanced(inner) {
        return group;
    }
    if open == '<' && split_top_level_alternatives(inner).len() < 2 {
        return group;
    }
    inner
}

fn brackets_balanced(text: &str) -> bool {
    let mut depth = 0isize;
    for ch in text.chars() {
        match ch {
            '[' | '<' => depth += 1,
            ']' | '>' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return false;
        }
    }
    depth == 0
}

/// Split on `|` that is not nested inside a `[…]` or `<…>`.
fn split_top_level_alternatives(group: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (idx, ch) in group.char_indices() {
        match ch {
            '[' | '<' => depth += 1,
            ']' | '>' => depth = depth.saturating_sub(1),
            '|' if depth == 0 => {
                parts.push(group[start..idx].trim());
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(group[start..].trim());
    parts.retain(|part| !part.is_empty());
    parts
}

/// The literal verb an alternative starts with, or `None` when it starts with
/// a placeholder, a flag, or a metavariable.
fn leading_literal_token(alternative: &str) -> Option<&str> {
    let end = alternative
        .find(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-')
        .unwrap_or(alternative.len());
    let token = &alternative[..end];
    // A word only counts when it is spelled the way a typed verb is spelled:
    // starting lowercase (or a digit, for `/mode [work|plan|1|2|3]`) and made
    // of ASCII word characters. `N`, `entry_id`, `--force` and
    // `path/to/export.json` are all values, not verbs.
    let mut chars = token.chars();
    let first = chars.next()?;
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return None;
    }
    let trailing = &alternative[end..];
    if trailing
        .chars()
        .next()
        .is_some_and(|ch| matches!(ch, '_' | '/' | '.'))
    {
        return None;
    }
    if USAGE_METAVARIABLES.contains(&token) {
        return None;
    }
    Some(token)
}

impl CommandInfo {
    /// Literal subcommands this command's `usage` line declares.
    ///
    /// Used by the slash menu to list `/workspace worktrees` and friends
    /// once the command name has been typed.
    #[must_use]
    pub fn subcommands(&self) -> Vec<&'static str> {
        usage_subcommands(self.usage)
    }

    pub fn requires_argument(&self) -> bool {
        self.usage.contains('<') || self.usage.contains('[')
    }

    pub fn requires_required_argument(&self) -> bool {
        let mut optional_depth = 0usize;
        for ch in self.usage.chars() {
            match ch {
                '[' => optional_depth += 1,
                ']' => optional_depth = optional_depth.saturating_sub(1),
                '<' if optional_depth == 0 => return true,
                _ => {}
            }
        }
        false
    }

    /// Whether the slash menu / composer should leave a trailing space so the
    /// user can type arguments immediately. `/change` is bare-useful (opens
    /// the latest changelog) even though its usage documents an optional
    /// version, so it is the only historical carve-out.
    pub fn composer_wants_trailing_space(&self) -> bool {
        self.name != "change" && self.requires_argument()
    }

    /// Whether the command palette should run this command immediately when
    /// selected, instead of pasting it into the composer.
    ///
    /// Default: run anything that does not require a mandatory positional
    /// argument (including optional-arg commands that open a picker when bare).
    /// [`PALETTE_PASTE_ONLY`] is the explicit opt-out for side-effectful or
    /// multi-step no-arg commands that should still paste for confirmation.
    pub fn palette_runs_directly(&self) -> bool {
        if self.requires_required_argument() {
            return false;
        }
        !PALETTE_PASTE_ONLY.contains(&self.name)
    }

    pub fn palette_command(&self) -> String {
        if self.requires_argument() {
            format!("/{} ", self.name)
        } else {
            format!("/{}", self.name)
        }
    }

    pub fn description_for(&self, locale: Locale) -> Cow<'static, str> {
        tr(locale, self.description_id)
    }

    pub fn palette_description_for(&self, locale: Locale) -> String {
        let desc = self.description_for(locale);
        if self.aliases.is_empty() {
            desc.to_string()
        } else {
            format!("{}  aliases: {}", desc, self.aliases.join(", "))
        }
    }

    pub fn discovery(&self) -> CommandDiscovery {
        if COMPATIBILITY_DISCOVERY_COMMANDS.contains(&self.name) {
            CommandDiscovery::Compatibility
        } else if ADVANCED_DISCOVERY_COMMANDS.contains(&self.name) {
            CommandDiscovery::Advanced
        } else {
            CommandDiscovery::Primary
        }
    }

    pub fn show_in_empty_discovery(&self) -> bool {
        self.discovery().show_at_root()
    }

    /// Whether this command may appear in slash completion at all.
    ///
    /// Always: the menu is how the command surface is discovered, so hiding
    /// commands from it makes them unfindable. Founder live-test: "I like how
    /// we prioritize the slash thing but it should still be able to find all
    /// of them." A bare `/` used to return only
    /// [`BARE_SLASH_DISCOVERY_COMMANDS`], which is a *ranking* concern — the
    /// menu already sorts those six to the top and the popup scrolls around
    /// the selection, so the short list is preserved as the head of a
    /// complete one rather than as the whole of a truncated one.
    pub fn show_in_slash_completion(&self, _prefix: &str) -> bool {
        !self.is_unlisted()
    }

    /// Whether this command is deliberately not advertised — see
    /// [`UNLISTED_COMMANDS`]. It still runs when typed.
    #[must_use]
    pub fn is_unlisted(&self) -> bool {
        UNLISTED_COMMANDS
            .iter()
            .any(|name| self.name == *name || self.aliases.contains(name))
    }
}

pub trait Command: Send + Sync {
    fn info(&self) -> &'static CommandInfo;
    fn execute(&self, app: &mut App, args: Option<&str>) -> CommandResult;

    /// FEAT-015 dual-path seam: if the entry carries a capability-scoped
    /// handler, the dispatcher builds the envelope from `app` and calls it
    /// here; otherwise the legacy `execute(app, args)` path is used. The
    /// default keeps every existing entry legacy (D2).
    fn contextual_handler(
        &self,
    ) -> Option<codewhale_command_contract::handler::CommandHandler<CommandResult>> {
        None
    }
}

pub trait CommandGroup: Send + Sync {
    fn commands(&self) -> &'static [Box<dyn Command>];
}

pub(crate) type CommandHandler = fn(&mut App, Option<&str>) -> CommandResult;

/// Trait implemented by focused built-in command modules.
///
/// A command module owns its metadata and exposes a static execution function
/// that the group registry can wire into [`FunctionCommand`].
pub trait RegisterCommand {
    fn info() -> &'static CommandInfo;
    fn execute(app: &mut App, arg: Option<&str>) -> CommandResult;
}

pub(crate) struct FunctionCommand {
    info: &'static CommandInfo,
    handler: CommandHandler,
}

impl FunctionCommand {
    pub(crate) const fn new(info: &'static CommandInfo, handler: CommandHandler) -> Self {
        Self { info, handler }
    }
}

impl Command for FunctionCommand {
    fn info(&self) -> &'static CommandInfo {
        self.info
    }

    fn execute(&self, app: &mut App, args: Option<&str>) -> CommandResult {
        (self.handler)(app, args)
    }
}

/// A registry entry that carries an optional capability-scoped handler.
///
/// FEAT-015's dual-path seam (D2): migrated registrations may supply a
/// `CommandHandler<CommandResult>` (App-free; built from `CommandContexts`),
/// while unmigrated registrations keep the legacy `execute(app, args)` path.
/// This entry type is App-free — only the dispatcher in `commands/mod.rs`
/// touches `App` when it builds the envelope from the bundle.
///
/// FEAT-015 ships no production contextual registration, so in production
/// builds this type is only referenced through the trait; the test fixture
/// (D6) constructs it under `#[cfg(test)]`. The allow is removed once a
/// production group migrates (FEAT-018+).
pub(crate) struct ContextualCommand {
    info: &'static CommandInfo,
    handler: Option<codewhale_command_contract::handler::CommandHandler<CommandResult>>,
    legacy: Option<CommandHandler>,
}

impl ContextualCommand {
    pub(crate) const fn contextual(
        info: &'static CommandInfo,
        handler: codewhale_command_contract::handler::CommandHandler<CommandResult>,
    ) -> Self {
        Self {
            info,
            handler: Some(handler),
            legacy: None,
        }
    }

    /// Bridge one portable contract registration into the TUI-owned registry.
    ///
    /// The command supplies only contract metadata and an App-free handler;
    /// the TUI resolves the localization key and owns the resulting registry
    /// entry. This is the dependency inversion later command crates reuse.
    pub(crate) fn from_contract<C>() -> Result<Self, String>
    where
        C: codewhale_command_contract::metadata::RegisterCommand<CommandResult>,
    {
        let portable = C::info();
        let description_id = super::contract::key_to_message_id(portable.description_key)
            .ok_or_else(|| {
                format!(
                    "unknown command description key {:?} for /{}",
                    portable.description_key, portable.name
                )
            })?;
        let info = Box::leak(Box::new(CommandInfo {
            name: portable.name,
            aliases: portable.aliases,
            usage: portable.usage,
            description_id,
        }));
        Ok(Self::contextual(info, C::handler()))
    }
}
impl Command for ContextualCommand {
    fn info(&self) -> &'static CommandInfo {
        self.info
    }

    fn execute(&self, app: &mut App, args: Option<&str>) -> CommandResult {
        match self.legacy {
            Some(legacy) => legacy(app, args),
            None => CommandResult::error("command has no executable handler"),
        }
    }

    fn contextual_handler(
        &self,
    ) -> Option<codewhale_command_contract::handler::CommandHandler<CommandResult>> {
        self.handler.clone()
    }
}
pub struct CommandRegistry {
    commands: Vec<&'static dyn Command>,
    name_to_index: HashMap<&'static str, usize>,
}

impl CommandRegistry {
    pub fn empty() -> Self {
        Self {
            commands: Vec::new(),
            name_to_index: HashMap::new(),
        }
    }

    pub fn register(&mut self, command: &'static dyn Command) {
        let index = self.commands.len();
        let info = command.info();
        self.name_to_index.insert(info.name, index);
        for alias in info.aliases {
            self.name_to_index.insert(alias, index);
        }
        self.commands.push(command);
    }

    pub fn register_group(&mut self, group: &dyn CommandGroup) {
        for command in group.commands() {
            self.register(command.as_ref());
        }
    }

    /// FEAT-015: register a test-only contextual command under `#[cfg(test)]`.
    /// The production registry is untouched (D6); the fixture dispatches
    /// through the public `execute()` to prove the seam.
    #[cfg(test)]
    pub(crate) fn register_test_only(&mut self, command: &'static dyn Command) {
        self.register(command);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Command> {
        let name = name.strip_prefix('/').unwrap_or(name);
        self.name_to_index
            .get(name)
            .and_then(|index| self.commands.get(*index))
            .copied()
    }

    pub fn get_info(&self, name: &str) -> Option<&'static CommandInfo> {
        self.get(name).map(Command::info)
    }

    /// FEAT-015: whether the named entry has a capability-scoped handler.
    /// Used by test assertions under `#[cfg(test)]`; production builds have
    /// no contextual entries, so the method is dead there until a group
    /// migrates (FEAT-018+).
    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) fn has_contextual_handler(&self, name: &str) -> bool {
        self.get(name)
            .is_some_and(|command| command.contextual_handler().is_some())
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn Command> {
        self.commands.iter().copied()
    }

    pub fn infos(&self) -> Vec<&'static CommandInfo> {
        self.iter().map(Command::info).collect()
    }
}

#[cfg(test)]
mod usage_subcommand_tests {
    use super::*;

    use crate::commands::get_command_info;

    #[test]
    fn workspace_usage_names_the_worktree_manager() {
        let workspace = get_command_info("workspace").expect("built-in workspace command");
        assert_eq!(workspace.subcommands(), vec!["worktrees"]);
    }

    /// The managers #5952 names: each one hid behind a single word, and each
    /// one now has its verbs on the menu the moment the name is typed.
    #[test]
    fn the_managers_the_issue_names_declare_their_verbs() {
        for (name, expected) in [
            ("workspace", vec!["worktrees"]),
            (
                "fleet",
                vec!["members", "setup", "teams", "workers", "help"],
            ),
            (
                "sessions",
                vec!["show", "open", "archive", "unarchive", "prune"],
            ),
            (
                "automation",
                vec!["list", "show", "print", "pause", "resume", "delete", "run"],
            ),
        ] {
            let info = get_command_info(name).expect("registered command");
            assert_eq!(info.subcommands(), expected, "/{name}");
        }
        let mcp = get_command_info("mcp").expect("registered command");
        for verb in ["init", "import", "add", "doctor", "reload"] {
            assert!(
                mcp.subcommands().contains(&verb),
                "/mcp must offer `{verb}`: {:?}",
                mcp.subcommands()
            );
        }
    }

    #[test]
    fn a_choice_group_yields_every_literal_verb_once() {
        assert_eq!(
            usage_subcommands("/queue [list|send <n>|edit <n>|drop <n>|clear]"),
            vec!["list", "send", "edit", "drop", "clear"]
        );
        // `/mcp` repeats `import` and `add` with different tails; the menu
        // offers each verb once.
        assert_eq!(
            usage_subcommands(
                "/mcp [init|import|import approve <name>|add stdio <name>|add http <name> <url>|doctor]"
            ),
            vec!["init", "import", "add", "doctor"]
        );
    }

    #[test]
    fn a_required_choice_between_verbs_is_opened_but_a_value_is_not() {
        assert_eq!(
            usage_subcommands(
                "/structcopy <turn <n>|tool <call-id>|plan|workflow <run-id>> [stdout]"
            ),
            vec!["turn", "tool", "plan", "workflow"]
        );
        assert!(usage_subcommands("/branch <entry_id>").is_empty());
        assert!(usage_subcommands("/rename <new title>").is_empty());
    }

    #[test]
    fn a_bare_alternation_without_brackets_still_parses() {
        assert_eq!(
            usage_subcommands("/auth xai-device|chatgpt|chatgpt-revoke"),
            vec!["xai-device", "chatgpt", "chatgpt-revoke"]
        );
        assert_eq!(usage_subcommands("/turn inspect"), vec!["inspect"]);
    }

    #[test]
    fn values_flags_and_later_groups_are_not_offered_as_verbs() {
        // Metavariables spelled bare, snake_case values, paths, flags, and a
        // single uppercase placeholder are all values the operator supplies.
        assert!(usage_subcommands("/help [command]").is_empty());
        assert!(usage_subcommands("/profile <name>").is_empty());
        assert!(usage_subcommands("/save [path]").is_empty());
        assert!(usage_subcommands("/new [--force]").is_empty());
        assert!(usage_subcommands("/agent [N] <task>").is_empty());
        assert!(
            usage_subcommands("/resume [session_id|path/to/export.json]").is_empty(),
            "snake_case and path-shaped alternatives are values"
        );
        // Only the first group names subcommands; `[--save]` modifies whichever
        // placement was chosen.
        assert_eq!(
            usage_subcommands("/workbar [bottom|top|off] [--save]"),
            vec!["bottom", "top", "off"]
        );
    }

    #[test]
    fn a_command_without_arguments_declares_no_subcommands() {
        assert!(usage_subcommands("/copy").is_empty());
        assert!(usage_subcommands("").is_empty());
    }

    #[test]
    fn every_registered_usage_parses_without_panicking() {
        // The parser reads strings maintained by hand across ~120 commands;
        // an unfamiliar shape must yield no hint rather than a panic.
        for info in crate::commands::command_infos() {
            let subcommands = info.subcommands();
            for subcommand in subcommands {
                assert!(
                    info.usage.contains(subcommand),
                    "/{}: `{subcommand}` is not in `{}`",
                    info.name,
                    info.usage
                );
            }
        }
    }
}
