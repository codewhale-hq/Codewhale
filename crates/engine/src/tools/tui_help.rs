//! On-demand command and keybinding reference: `tui_help`.
//!
//! Every field is read back out of the registries the human-facing help
//! renders from — `command_catalog::CommandCatalog`, the user-command registry, and
//! `keybinding_table::KEYBINDINGS` — so the model-facing reference cannot
//! drift from `/help` and the help overlay.

use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::command_catalog::{BuiltinCommand, CommandCatalog};
use crate::keybinding_table::KEYBINDINGS;
use codewhale_localization::{Locale, tr};

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

/// Per-section cap. The command catalog is well over a hundred entries; an
/// unscoped dump would cost more context than any answer it contains.
const MAX_COMMANDS: usize = 12;
const MAX_KEYBINDINGS: usize = 12;

/// Tool for looking up slash commands and keybindings.
pub struct TuiHelpTool;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CommandEntry {
    command: String,
    /// Omitted when the usage string carries nothing beyond the command name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    usage: Option<String>,
    description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    aliases: Vec<String>,
    source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeybindingEntry {
    chord: String,
    description: String,
    section: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HelpOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    commands: Vec<CommandEntry>,
    keybindings: Vec<KeybindingEntry>,
    /// Matches dropped by the cap, so the model narrows the query instead of
    /// assuming it saw everything.
    #[serde(default, skip_serializing_if = "is_zero")]
    omitted_commands: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    omitted_keybindings: usize,
    note: String,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[async_trait]
impl ToolSpec for TuiHelpTool {
    fn name(&self) -> &'static str {
        "tui_help"
    }

    fn description(&self) -> &'static str {
        "Look up Codewhale slash commands and keyboard shortcuts. Pass a query to scope the answer; omit it for a compact cheatsheet."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Topic, command name, or keyword to scope the answer (for example \"mode\", \"compact\", \"Ctrl+R\"). Omit for a compact cheatsheet."
                }
            },
            "required": [],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let query = match input.get("query") {
            None | Some(Value::Null) => None,
            Some(Value::String(query)) => Some(query.clone()),
            Some(_) => return Err(ToolError::invalid_input("`query` must be a string")),
        };

        let catalog = crate::command_catalog::installed()
            .ok_or_else(|| ToolError::execution_failed("command catalog is not installed"))?;
        let workspace = context.workspace.clone();
        #[cfg(any(test, feature = "test-support"))]
        let env_ticket = crate::test_support::env_scope_ticket();
        let help = tokio::task::spawn_blocking(move || {
            #[cfg(any(test, feature = "test-support"))]
            let _membership = crate::test_support::join_env_scope(env_ticket);
            build_help(catalog.as_ref(), ui_locale(), &workspace, query.as_deref())
        })
        .await
        .map_err(|error| ToolError::execution_failed(error.to_string()))?;
        ToolResult::json(&help).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

// === Helpers ===

/// The reference is rendered from the same localized strings the overlay uses,
/// so it follows the configured UI locale rather than the model's language.
/// `load_read_only` keeps the lookup free of the legacy-settings migration
/// write that `Settings::load` performs.
fn ui_locale() -> Locale {
    codewhale_localization::resolve_locale(
        &codewhale_runtime::settings::Settings::load_read_only()
            .unwrap_or_default()
            .locale,
    )
}

fn build_help(
    catalog: &dyn CommandCatalog,
    locale: Locale,
    workspace: &Path,
    query: Option<&str>,
) -> HelpOutput {
    let Some(query) = query.map(str::trim).filter(|query| !query.is_empty()) else {
        return cheatsheet(catalog, locale, workspace);
    };
    let needle = query.to_lowercase();

    let mut commands: Vec<(u8, CommandEntry)> = all_commands(catalog, locale, workspace)
        .into_iter()
        .filter_map(|entry| entry.rank(&needle).map(|rank| (rank, entry)))
        .collect();
    commands.sort_by_key(|(rank, _)| *rank);

    let mut keybindings: Vec<(u8, KeybindingEntry)> = all_keybindings(locale)
        .into_iter()
        .filter_map(|entry| entry.rank(&needle).map(|rank| (rank, entry)))
        .collect();
    keybindings.sort_by_key(|(rank, _)| *rank);

    let note = if commands.is_empty() && keybindings.is_empty() {
        format!(
            "No command or keybinding matches '{query}'. Call tui_help with no query for the cheatsheet."
        )
    } else {
        format!("Commands and keybindings matching '{query}'.")
    };

    HelpOutput {
        query: Some(query.to_string()),
        omitted_commands: commands.len().saturating_sub(MAX_COMMANDS),
        omitted_keybindings: keybindings.len().saturating_sub(MAX_KEYBINDINGS),
        commands: take_entries(commands, MAX_COMMANDS),
        keybindings: take_entries(keybindings, MAX_KEYBINDINGS),
        note,
    }
}

/// Unscoped answer: the commands the help overlay shows at its root, capped
/// like every other answer.
fn cheatsheet(catalog: &dyn CommandCatalog, locale: Locale, workspace: &Path) -> HelpOutput {
    // User commands lead: they are workspace-specific and unguessable, where a
    // built-in is stable enough to be worth querying by name.
    let mut entries = user_commands(catalog, workspace);
    entries.extend(
        catalog
            .builtins()
            .into_iter()
            .filter(|info| info.show_in_empty_discovery)
            .map(|info| builtin_entry(&info, locale)),
    );
    let keybindings = all_keybindings(locale);

    HelpOutput {
        query: None,
        omitted_commands: entries.len().saturating_sub(MAX_COMMANDS),
        omitted_keybindings: keybindings.len().saturating_sub(MAX_KEYBINDINGS),
        commands: entries.into_iter().take(MAX_COMMANDS).collect(),
        keybindings: keybindings.into_iter().take(MAX_KEYBINDINGS).collect(),
        note: "Compact cheatsheet. Pass a query to reach advanced commands and the rest of the keybindings.".to_string(),
    }
}

fn take_entries<T>(ranked: Vec<(u8, T)>, limit: usize) -> Vec<T> {
    ranked
        .into_iter()
        .take(limit)
        .map(|(_, entry)| entry)
        .collect()
}

fn builtin_entry(info: &BuiltinCommand, locale: Locale) -> CommandEntry {
    let command = format!("/{}", info.name);
    CommandEntry {
        usage: Some(info.usage.trim().to_string()).filter(|usage| *usage != command),
        command,
        description: tr(locale, info.description_id).into_owned(),
        aliases: info.aliases.iter().map(|alias| alias.to_string()).collect(),
        source: "builtin".to_string(),
    }
}

fn all_commands(
    catalog: &dyn CommandCatalog,
    locale: Locale,
    workspace: &Path,
) -> Vec<CommandEntry> {
    let mut entries: Vec<CommandEntry> = catalog
        .builtins()
        .into_iter()
        // Unlisted commands still run when typed; help does not teach them.
        .filter(|info| !info.unlisted)
        .map(|info| builtin_entry(&info, locale))
        .collect();
    entries.extend(user_commands(catalog, workspace));
    entries
}

/// The user registry is keyed by a hash map, so sort for a stable answer.
fn user_commands(catalog: &dyn CommandCatalog, workspace: &Path) -> Vec<CommandEntry> {
    let mut entries: Vec<CommandEntry> = catalog
        .user_commands(workspace)
        .into_iter()
        .filter(|metadata| !metadata.hidden)
        .map(|metadata| CommandEntry {
            command: format!("/{}", metadata.name),
            usage: metadata.usage,
            description: metadata.description.unwrap_or_default(),
            aliases: metadata.aliases,
            source: "user".to_string(),
        })
        .collect();
    entries.sort_by(|a, b| a.command.cmp(&b.command));
    entries
}

fn all_keybindings(locale: Locale) -> Vec<KeybindingEntry> {
    KEYBINDINGS
        .iter()
        .map(|binding| KeybindingEntry {
            chord: binding.chord.to_string(),
            description: tr(locale, binding.description_id).into_owned(),
            section: binding.section.label(locale).into_owned(),
        })
        .collect()
}

impl CommandEntry {
    /// Relevance rank, lowest first; `None` when the entry does not match.
    fn rank(&self, needle: &str) -> Option<u8> {
        let name = self.command.trim_start_matches('/').to_lowercase();
        if name == needle
            || self
                .aliases
                .iter()
                .any(|alias| alias.to_lowercase() == needle)
        {
            return Some(0);
        }
        if name.starts_with(needle) {
            return Some(1);
        }
        if name.contains(needle) {
            return Some(2);
        }
        if self.description.to_lowercase().contains(needle)
            || self
                .usage
                .as_deref()
                .is_some_and(|usage| usage.to_lowercase().contains(needle))
        {
            return Some(3);
        }
        None
    }
}

impl KeybindingEntry {
    fn rank(&self, needle: &str) -> Option<u8> {
        if self.chord.to_lowercase().contains(needle) {
            return Some(0);
        }
        if self.description.to_lowercase().contains(needle)
            || self.section.to_lowercase().contains(needle)
        {
            return Some(1);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    struct Catalog;
    impl CommandCatalog for Catalog {
        fn answers_to(&self, _: &str) -> bool {
            false
        }
        fn builtins(&self) -> Vec<BuiltinCommand> {
            ["mode".to_string(), "compact".to_string()]
                .into_iter()
                .chain((0..13).map(|index| format!("edge{index}")))
                .map(|name| BuiltinCommand {
                    usage: format!("/{name} [value]"),
                    name,
                    aliases: Vec::new(),
                    description_id: codewhale_localization::MessageId::CmdModeDescription,
                    subcommands: Vec::new(),
                    takes_arguments: true,
                    requires_argument: true,
                    requires_required_argument: false,
                    composer_wants_trailing_space: true,
                    palette_runs_directly: false,
                    show_in_empty_discovery: true,
                    discovery: codewhale_command_contract::CommandDiscovery::Primary,
                    unlisted: false,
                })
                .collect()
        }
        fn user_commands(&self, _: &Path) -> Vec<crate::command_catalog::UserCommand> {
            Vec::new()
        }
    }

    #[test]
    fn scoped_query_answers_from_the_command_and_keybinding_registries() {
        let tmp = tempdir().expect("tempdir");
        let help = build_help(&Catalog, Locale::En, tmp.path(), Some("mode"));

        let mode = help
            .commands
            .iter()
            .find(|entry| entry.command == "/mode")
            .unwrap_or_else(|| panic!("no /mode in {:?}", help.commands));
        assert_eq!(mode.usage.as_deref(), Some("/mode [value]"));
        assert_eq!(
            mode.description,
            tr(
                Locale::En,
                codewhale_localization::MessageId::CmdModeDescription
            )
        );

        assert!(
            help.keybindings
                .iter()
                .all(|entry| KEYBINDINGS.iter().any(|kb| kb.chord == entry.chord)),
            "chords must come from the keybinding catalog: {:?}",
            help.keybindings
        );
        assert!(
            !help.keybindings.is_empty(),
            "the Modes keybinding section should match 'mode'"
        );
    }

    #[test]
    fn exact_command_name_outranks_incidental_mentions() {
        let tmp = tempdir().expect("tempdir");
        let help = build_help(&Catalog, Locale::En, tmp.path(), Some("compact"));

        assert_eq!(
            help.commands.first().map(|entry| entry.command.as_str()),
            Some("/compact")
        );
    }

    #[test]
    fn unmatched_query_returns_nothing_rather_than_everything() {
        let tmp = tempdir().expect("tempdir");
        let help = build_help(&Catalog, Locale::En, tmp.path(), Some("zzzz-no-such-topic"));

        assert!(help.commands.is_empty());
        assert!(help.keybindings.is_empty());
        assert!(help.note.starts_with("No command or keybinding matches"));
    }

    #[test]
    fn output_stays_bounded_and_reports_what_it_dropped() {
        let tmp = tempdir().expect("tempdir");
        // A query every entry matches on the description axis would otherwise
        // dump the whole catalog.
        for query in [None, Some("e")] {
            let help = build_help(&Catalog, Locale::En, tmp.path(), query);
            assert!(help.commands.len() <= MAX_COMMANDS, "{query:?}");
            assert!(help.keybindings.len() <= MAX_KEYBINDINGS, "{query:?}");
        }

        let broad = build_help(&Catalog, Locale::En, tmp.path(), Some("e"));
        assert!(
            broad.omitted_commands > 0,
            "a catalog-wide query must report the dropped matches"
        );
    }

    #[test]
    fn blank_query_falls_back_to_the_cheatsheet() {
        let tmp = tempdir().expect("tempdir");
        let help = build_help(&Catalog, Locale::En, tmp.path(), Some("   "));

        assert!(help.query.is_none());
        assert!(!help.commands.is_empty());
    }

    #[tokio::test]
    async fn execute_returns_json_and_rejects_non_string_queries() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path());
        let _catalog = crate::command_catalog::CatalogGuard::install(std::sync::Arc::new(Catalog));

        let result = TuiHelpTool
            .execute(json!({ "query": "mode" }), &ctx)
            .await
            .expect("execute");
        assert!(result.success);
        let parsed: HelpOutput =
            serde_json::from_str(&result.content).expect("tool result should be json");
        assert_eq!(parsed.query.as_deref(), Some("mode"));

        let error = TuiHelpTool
            .execute(json!({ "query": 7 }), &ctx)
            .await
            .expect_err("non-string query");
        assert!(error.to_string().contains("`query` must be a string"));
    }
}
