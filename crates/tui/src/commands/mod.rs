//! Slash command registry and dispatch system
//!
//! This module provides a modular command system inspired by Codex-rs.
//! Commands are organized by category and dispatched through a central strategy
//! registry. Built-in handlers live in group-owned areas under [`groups`]; this
//! module keeps registry construction, user-command precedence, and the
//! fall-through behaviour.

mod config_policy_host;
mod contract;
pub mod discovery;
mod groups;

// FEAT-025 host services for the session-export slice: the shared recovery
// writer and the protected export-destination resolver/writer. Declared at the
// `commands` root so they stay outside `groups/session`, which FEAT-043 moves
// to `codewhale-commands`.
mod session_export_host;
pub mod traits;
pub mod user_commands;
pub mod user_registry;

#[cfg(test)]
#[path = "epic_dispatch_acceptance.rs"]
mod epic_dispatch_acceptance;

// Extension slash commands through the real command table and `App` dispatch;
// they cannot live in `extension_host`, a runtime module that may not depend
// on this one.
#[cfg(test)]
mod extension_host_tests;

#[cfg(test)]
#[path = "epic_discovery_acceptance.rs"]
mod epic_discovery_acceptance;

// TUI-hosted session acceptance and persistence regressions deliberately stay
// outside `groups/session`, which FEAT-043 moves to `codewhale-commands`.
#[cfg(all(test, feature = "long-running-tests"))]
mod session_acceptance;
#[cfg(test)]
mod session_control_regression_tests;
#[cfg(test)]
mod session_export_regression_tests;
#[cfg(test)]
mod session_structcopy_host_tests;
#[cfg(test)]
mod session_structcopy_regression_tests;
// FEAT-025 Phase 5: public command-surface parity lives at the `commands` root
// for the same extraction reason as the host regressions above.
#[cfg(test)]
mod session_export_surface_tests;
// FEAT-025 audit hardening: shared host-bound test support for both export
// test suites (timestamp normalisation and the exhaustive envelope check).
#[cfg(test)]
mod session_export_test_support;
#[cfg(test)]
mod session_lifecycle_regression_tests;

use std::sync::OnceLock;

pub(crate) use groups::config::config::{set_workspace_trust, trust_change_note};

/// Stage a rollback of the last exchange for the UI to apply, or `None` when
/// there is no user message to roll back. Nothing is mutated here.
pub(crate) fn staged_conversation_undo(
    app: &mut crate::tui::app::App,
) -> Option<codewhale_command_contract::facets::SessionSyncPayload> {
    let undone = contract::debug_operations::undo_conversation_for_engine(app);
    (undone.removed > 0).then_some(undone.sync)
}
pub use traits::CommandInfo;

// Long-standing public paths that predate the group layout.
/// `/fleet add` and the picker's ⇧F share these gates; the UI applies them
/// against the live `Config`.
pub(crate) use groups::core::fleet::{fleet_catalog_rejection, fleet_provider_rejection};
pub(crate) use groups::memory::{notes_path, read_notes};
pub use groups::project::share;

// Voice capture plumbing shared with the hotbar and the UI event loop.
pub use groups::core::voice;

#[cfg(test)]
mod debug_diagnostics_baseline_tests;
// Host fixtures for the eight diagnostics commands live outside the movable
// debug group; CW-SLICE selects them together with the frozen baseline tests.
#[cfg(test)]
mod debug_diagnostics_host_tests;
#[cfg(test)]
mod debug_diagnostics_regression_tests;
#[cfg(test)]
mod debug_diagnostics_surface_tests;
#[cfg(test)]
mod debug_diagnostics_test_support;

#[cfg(test)]
mod debug_change_host_tests;
mod debug_group;
#[cfg(test)]
mod debug_mutation_host_tests;
mod session_group;

use crate::tui::app::{App, AppAction};
use codewhale_config::AppMode;

/// Shared result shape; host actions remain consumed by the existing event loop.
pub type CommandResult = codewhale_command_contract::outcome::CommandResult<AppAction>;

static REGISTRY: OnceLock<traits::CommandRegistry> = OnceLock::new();

fn build_registry() -> traits::CommandRegistry {
    let mut registry = traits::CommandRegistry::empty();
    for &group in groups::all_command_groups() {
        registry.register_group(group);
    }
    #[cfg(test)]
    {
        registry.register_test_only(feat015_ctx_command());
    }
    registry
}

/// FEAT-015 test-only contextual command (D6).
///
/// Registered into the global registry only in test builds; the production
/// registry is untouched. The command implements the portable contract
/// `RegisterCommand` shape, and its handler cannot name concrete `App`; the
/// TUI bridge resolves metadata and dispatches it through public `execute()`.
#[cfg(test)]
struct Feat015TestCommand;

#[cfg(test)]
impl codewhale_command_contract::metadata::RegisterCommand<CommandResult> for Feat015TestCommand {
    fn info() -> &'static codewhale_command_contract::metadata::CommandInfo {
        static INFO: codewhale_command_contract::metadata::CommandInfo =
            codewhale_command_contract::metadata::CommandInfo {
                name: "feat015ctx",
                aliases: &[],
                usage: "/feat015ctx",
                description_key: "cmd_workspace_description",
            };
        &INFO
    }

    fn handler() -> codewhale_command_contract::handler::CommandHandler<CommandResult> {
        codewhale_command_contract::handler::CommandHandler::Contextual {
            capabilities: codewhale_command_contract::handler::CommandCapabilities::WORKSPACE
                .union(codewhale_command_contract::handler::CommandCapabilities::MODE_POLICY)
                .union(codewhale_command_contract::handler::CommandCapabilities::COST),
            handler: feat015_contextual,
        }
    }
}

/// Test-only contextual handler: reads workspace, mode, and currency facets
/// through the envelope and returns the host-selected result type. It has no
/// concrete `App` parameter or TUI state in its input surface.
#[cfg(test)]
fn feat015_contextual(
    contexts: codewhale_command_contract::handler::CommandContexts<'_>,
    arg: Option<&str>,
) -> CommandResult {
    use codewhale_command_contract::handler::ContextParts;
    let parts: ContextParts<'_> = contexts.into_parts();
    let Some(workspace) = parts.workspace else {
        return CommandResult::error("Command capability unavailable: workspace");
    };
    let Some(mode_policy) = parts.mode_policy else {
        return CommandResult::error("Command capability unavailable: mode-policy");
    };
    let Some(cost) = parts.cost else {
        return CommandResult::error("Command capability unavailable: cost");
    };
    let workspace = workspace.workspace();
    let mode = mode_policy.mode();
    let currency = cost.display_currency();
    let normalized = arg.unwrap_or("");
    CommandResult::message(format!(
        "feat015ctx workspace={} mode={:?} currency={:?} arg={}",
        workspace.display(),
        mode,
        currency,
        normalized
    ))
}

#[cfg(test)]
static FEAT015_CTX: OnceLock<&'static traits::ContextualCommand> = OnceLock::new();

#[cfg(test)]
fn feat015_ctx_command() -> &'static traits::ContextualCommand {
    FEAT015_CTX.get_or_init(|| {
        Box::leak(Box::new(
            traits::ContextualCommand::from_contract::<Feat015TestCommand>()
                .expect("FEAT-015 portable registration must bridge into the TUI registry"),
        ))
    })
}

pub fn registry() -> &'static traits::CommandRegistry {
    REGISTRY.get_or_init(build_registry)
}

/// The built-in command table as the extension host asks about it: the one
/// read-only question "does a built-in command answer to this name?". The
/// composition root installs it at startup (`lib.rs`), so the runtime-side host
/// never depends on this module.
pub(crate) struct BuiltinCommandNames;

impl crate::extension_host::command::BuiltinCommandCatalog for BuiltinCommandNames {
    fn answers_to(&self, name: &str) -> bool {
        // `jihua` and `zidong` are mode aliases the dispatcher answers ahead
        // of the registry.
        matches!(name, "jihua" | "zidong") || registry().get(name).is_some()
    }
}

pub fn command_infos() -> Vec<&'static CommandInfo> {
    registry().infos()
}

pub fn get_command_info(name: &str) -> Option<&'static CommandInfo> {
    registry().get_info(name)
}

/// Execute a slash command with its captured active configuration.
pub fn execute_with_config(
    cmd: &str,
    app: &mut App,
    config: &crate::config::Config,
) -> CommandResult {
    execute_in_context(cmd, app, Some(config))
}

/// Legacy fixture entry; it cannot authorize a model route change.
#[cfg(test)]
pub fn execute(cmd: &str, app: &mut App) -> CommandResult {
    execute_in_context(cmd, app, None)
}

fn execute_in_context(
    cmd: &str,
    app: &mut App,
    config: Option<&crate::config::Config>,
) -> CommandResult {
    // Keep the command's raw remainder available for commands whose payload is
    // byte-sensitive. Most slash commands intentionally receive a normalized
    // argument below; `/preview-request --prompt`, however, must describe the
    // exact prompt the send path would receive, including trailing whitespace
    // and newlines.
    let dispatch_input = cmd.trim_start();
    let command_token_end = dispatch_input
        .find(char::is_whitespace)
        .unwrap_or(dispatch_input.len());
    let raw_remainder = &dispatch_input[command_token_end..];
    let trimmed = cmd.trim();

    // `$skillname` is a backward-compatible alias for `/skill skillname`.
    // Resolve it early so skills can be loaded with the `$` prefix.
    if let Some(skill_input) = trimmed.strip_prefix('$') {
        let skill_input = skill_input.trim_start();
        if skill_input.is_empty() {
            return CommandResult::error(
                "Type a skill name after $. For example: $getting-started",
            );
        }
        let parts: Vec<&str> = skill_input.splitn(2, char::is_whitespace).collect();
        let skill_name = parts.first().copied().unwrap_or("");
        let arg = parts
            .get(1)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty());
        if let Some(result) = groups::skills::run_skill_by_name(app, skill_name, arg) {
            return result;
        }
        return CommandResult::error(format!(
            "Unknown skill: ${skill_name}. Type /skills to see installed skills."
        ));
    }

    let parts: Vec<&str> = trimmed.splitn(2, char::is_whitespace).collect();
    let command = parts
        .first()
        .copied()
        .unwrap_or_default()
        .trim_start_matches('/')
        .to_ascii_lowercase();
    let arg = parts
        .get(1)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());

    // Check user-defined commands FIRST so they can override built-ins.
    // Workspace (repository) commands load only in a trusted workspace and
    // never under a protected built-in such as /trust or /undo — the
    // registry drops those at load.
    if let Some(result) = user_registry::try_dispatch(app, trimmed) {
        return result;
    }

    // Permanent backward-compatible mode aliases. They select a fixed mode
    // rather than the canonical `/mode` behavior, so they still dispatch
    // before registry lookup. Ordinary compatibility aliases belong in their
    // command's `CommandInfo` metadata.
    match command.as_str() {
        "jihua" => {
            return groups::config::dispatch(app, "jihua", arg).unwrap_or_else(|| {
                CommandResult::error("The /jihua alias could not be dispatched.")
            });
        }
        "zidong" => {
            return groups::config::dispatch(app, "zidong", arg).unwrap_or_else(|| {
                CommandResult::error("The /zidong alias could not be dispatched.")
            });
        }
        _ => {}
    }

    if let Some(command_object) = registry().get(command.as_str()) {
        let command_arg = if command_object.info().name == "preview-request" {
            Some(raw_remainder)
        } else {
            arg
        };
        // FEAT-015 dual-path seam (D2): a migrated entry with a
        // capability-scoped handler receives the envelope built from `app`;
        // everything else keeps the legacy `execute(app, args)` path. The
        // envelope is populated only with the capabilities the registration
        // declared (FEAT-019 D1/D3); production groups such as utility and
        // memory dispatch through this contextual branch.
        if let Some(handler) = command_object.contextual_handler() {
            return match handler {
                codewhale_command_contract::handler::CommandHandler::Pure(pure_fn) => {
                    pure_fn(command_arg)
                }
                codewhale_command_contract::handler::CommandHandler::Contextual {
                    capabilities,
                    handler: contextual,
                } => {
                    let mut bundle = app.command_contexts_with_config(config);
                    contextual(bundle.contexts(capabilities), command_arg)
                }
            };
        }
        return command_object.execute(app, command_arg);
    }

    match command.as_str() {
        // Permanent legacy migration hints. These are deliberately excluded
        // from registry/autocomplete and only appear when users type old names.
        "set" => CommandResult::error(
            "The /set command was retired. Use /config to edit settings and /settings to inspect current values.",
        ),
        "deepseek" => CommandResult::error(
            "The /deepseek command was renamed. Use /links (aliases: /dashboard, /api).",
        ),
        "doctor" => CommandResult::error(
            "The /doctor command is a CLI diagnostic. Run `codewhale doctor` or `codewhale doctor --json`; use `/setup` in the TUI for readiness and verification.",
        ),

        _ => {
            // Third source: skills (lowest precedence after native and user-config).
            // Try to run a skill whose name matches the command.
            if let Some(result) = groups::skills::run_skill_by_name(app, command.as_str(), arg) {
                return result;
            }
            let suggestions = user_registry::with_registry_for_app(app, |user_commands| {
                suggest_command_names(command.as_str(), 3, user_commands)
            });
            if suggestions.is_empty() {
                CommandResult::error(format!(
                    "Unknown command: /{command}. Type /help for available commands."
                ))
            } else {
                let list = suggestions
                    .into_iter()
                    .map(|name| format!("/{name}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                CommandResult::error(format!(
                    "Unknown command: /{command}. Did you mean: {list}? Type /help for available commands."
                ))
            }
        }
    }
}

/// Update a configuration value programmatically (used by interactive UI views).
pub fn set_config_value(app: &mut App, key: &str, value: &str, persist: bool) -> CommandResult {
    groups::config::config::set_config_value(app, key, value, persist)
}

/// Switch the interaction mode (plan / work / operate).
pub fn switch_mode(app: &mut App, mode: AppMode) -> String {
    groups::config::config::switch_mode(app, mode)
}

fn edit_distance(a: &str, b: &str) -> usize {
    if a == b {
        return 0;
    }
    if a.is_empty() {
        return b.chars().count();
    }
    if b.is_empty() {
        return a.chars().count();
    }

    let b_chars: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    let mut current = vec![0usize; b_chars.len() + 1];

    for (i, a_ch) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, b_ch) in b_chars.iter().enumerate() {
            let cost = if a_ch == *b_ch { 0 } else { 1 };
            let delete = previous[j + 1] + 1;
            let insert = current[j] + 1;
            let substitute = previous[j] + cost;
            current[j + 1] = delete.min(insert).min(substitute);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[b_chars.len()]
}

pub(crate) fn best_suggestion_score<'a>(
    query: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<(u8, usize)> {
    let mut best: Option<(u8, usize)> = None;
    for candidate in candidates {
        let prefix_match = candidate.starts_with(query) || query.starts_with(candidate);
        let contains_match = candidate.contains(query) || query.contains(candidate);
        let distance = edit_distance(candidate, query);
        let close_typo = distance <= 2;
        if !(prefix_match || contains_match || close_typo) {
            continue;
        }

        let rank = if prefix_match {
            0
        } else if contains_match {
            1
        } else {
            2
        };

        match best {
            Some((best_rank, best_distance))
                if rank > best_rank || (rank == best_rank && distance >= best_distance) => {}
            _ => best = Some((rank, distance)),
        }
    }
    best
}

fn suggest_command_names(
    input: &str,
    limit: usize,
    user_commands: &user_registry::UserCommandRegistry,
) -> Vec<String> {
    let query = input.trim().to_ascii_lowercase();
    if query.is_empty() || limit == 0 {
        return Vec::new();
    }

    let mut scored: Vec<(u8, usize, String)> = Vec::new();
    for command in registry().infos() {
        // A user command can shadow a built-in canonical name or just one of
        // its aliases. Score only the built-in spellings that still dispatch
        // to the built-in so suggestions never advertise different behavior.
        if user_commands.get(command.name).is_some() {
            continue;
        }
        let candidates = std::iter::once(command.name).chain(
            command
                .aliases
                .iter()
                .copied()
                .filter(|alias| user_commands.get(alias).is_none()),
        );
        if let Some((rank, distance)) = best_suggestion_score(&query, candidates) {
            scored.push((rank, distance, command.name.to_string()));
        }
    }

    for command in user_commands.iter().filter(|command| !command.hidden) {
        let candidates = std::iter::once(command.name.as_str()).chain(
            command.aliases.iter().map(String::as_str).filter(|alias| {
                user_commands
                    .get(alias)
                    .is_some_and(|resolved| resolved.name == command.name)
            }),
        );
        if let Some((rank, distance)) = best_suggestion_score(&query, candidates) {
            scored.push((rank, distance, command.name.clone()));
        }
    }

    scored.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, name)| name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ProviderKind};
    use crate::tools::plan::{PlanItemArg, StepStatus, UpdatePlanArgs};
    use crate::tools::todo::TodoStatus;
    use crate::tui::app::{App, AppAction, TuiOptions};
    use crate::tui::work_surface::{RailPanel, WorkSurfacePlacement};
    use codewhale_localization::{Locale, MessageId};
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    fn is_palette_safe_command_name(name: &str) -> bool {
        let bytes = name.as_bytes();
        !bytes.is_empty()
            && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
            && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
            && !name.contains("--")
    }

    fn create_test_app() -> App {
        let options = TuiOptions {
            ..crate::test_support::test_tui_options(PathBuf::from("."))
        };
        App::new(options, &Config::default())
    }

    #[test]
    fn user_registry_module_is_compiled() {
        super::user_registry::reload(None);
        let registry = super::user_registry::current_registry();
        assert!(registry.is_valid());
    }

    #[test]
    fn preview_request_dispatch_preserves_prompt_edge_bytes() {
        let mut app = create_test_app();
        let result = execute("/preview-request --prompt   lead\ntrail  ", &mut app);

        assert!(!result.is_error, "{result:?}");
        assert!(matches!(
            result.action,
            Some(AppAction::PreviewOutboundRequest {
                json: false,
                base_prompt_only: false,
                hypothetical_prompt,
            }) if hypothetical_prompt.as_deref() == Some("  lead\ntrail  ")
        ));
    }

    #[test]
    fn user_command_shadows_builtin_before_group_dispatch() {
        let temp = tempdir().unwrap();
        crate::test_support::trust_workspace(temp.path());
        let commands_dir = temp.path().join(".codewhale").join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        std::fs::write(
            commands_dir.join("help.md"),
            "---\ndescription: User help\n---\nuser help $ARGUMENTS",
        )
        .unwrap();

        let mut app = crate::test_support::test_app_with_options(
            crate::test_support::test_tui_options(temp.path()),
        );
        super::user_registry::reload(Some(temp.path()));

        let result = execute("/help now", &mut app);
        assert!(!result.is_error);
        match result.action {
            Some(AppAction::SendMessage(message)) => assert_eq!(message, "user help now"),
            other => panic!("expected user command SendMessage action, got {other:?}"),
        }
    }

    #[test]
    fn removed_user_command_reloads_and_falls_back_to_builtin() {
        let temp = tempdir().unwrap();
        crate::test_support::trust_workspace(temp.path());
        let commands_dir = temp.path().join(".codewhale").join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        let command_path = commands_dir.join("help.md");
        std::fs::write(&command_path, "user help").unwrap();

        let mut app = crate::test_support::test_app_with_options(
            crate::test_support::test_tui_options(temp.path()),
        );
        super::user_registry::reload(Some(temp.path()));
        assert!(matches!(
            execute("/help config", &mut app).action,
            Some(AppAction::SendMessage(_))
        ));

        std::fs::remove_file(command_path).unwrap();
        super::user_registry::reload(Some(temp.path()));
        let result = execute("/help config", &mut app);
        assert!(!result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|message| message.contains("config")),
            "built-in /help should handle the command"
        );
        assert!(result.action.is_none());
    }

    #[test]
    fn command_registry_contains_config_and_links_but_not_set_or_deepseek() {
        assert!(command_infos().iter().any(|cmd| cmd.name == "config"));
        assert!(get_command_info("experiments").is_none());
        assert!(get_command_info("experimental").is_none());
        let rail = command_infos()
            .into_iter()
            .find(|cmd| cmd.name == "workbar")
            .expect("workbar command should exist");
        assert_eq!(rail.aliases, &["rail", "sidebar"]);
        assert_eq!(rail.description_id, MessageId::CmdSidebarDescription);
        assert!(rail.description_for(Locale::En).contains("workbar"));
        assert!(command_infos().iter().any(|cmd| cmd.name == "links"));
        let hf = command_infos()
            .into_iter()
            .find(|cmd| cmd.name == "hf")
            .expect("hf command should exist");
        assert_eq!(hf.aliases, &["huggingface"]);
        assert_eq!(hf.description_id, MessageId::CmdHfDescription);
        assert!(hf.description_for(Locale::En).contains("Hugging Face"));
        assert!(command_infos().iter().any(|cmd| cmd.name == "memory"));
        assert!(!command_infos().iter().any(|cmd| cmd.name == "set"));
        assert!(!command_infos().iter().any(|cmd| cmd.name == "deepseek"));
    }

    #[test]
    fn pet_command_is_registered_and_the_workbar_no_longer_advertises_watch() {
        let pet = command_infos()
            .into_iter()
            .find(|cmd| cmd.name == "pet")
            .expect("pet command should exist");
        assert_eq!(pet.description_id, MessageId::CmdPetDescription);
        assert!(pet.usage.starts_with("/pet"));
        let rail = command_infos()
            .into_iter()
            .find(|cmd| cmd.name == "workbar")
            .expect("workbar command should exist");
        assert!(!rail.usage.contains("watch"), "{}", rail.usage);
    }

    #[test]
    fn links_command_has_dashboard_and_api_aliases() {
        let links = command_infos()
            .into_iter()
            .find(|cmd| cmd.name == "links")
            .expect("links command should exist");
        assert_eq!(links.aliases, &["dashboard", "api", "lianjie"]);
    }

    #[test]
    fn transcript_command_is_discoverable_and_opens_live_overlay() {
        let transcript = command_infos()
            .into_iter()
            .find(|cmd| cmd.name == "transcript")
            .expect("transcript command should exist");
        assert_eq!(transcript.usage, "/transcript");
        assert!(transcript.show_in_empty_discovery());

        let mut app = create_test_app();
        let result = execute("/transcript", &mut app);
        assert!(!result.is_error);
        assert!(matches!(result.action, Some(AppAction::OpenLiveTranscript)));
    }

    #[test]
    fn hf_alias_dispatches_to_concepts_helper() {
        let mut app = create_test_app();
        let result = execute("/huggingface concepts", &mut app);
        assert!(!result.is_error);
        let message = result.message.expect("concepts message");
        assert!(message.contains("Hugging Face provider route"));
        assert!(message.contains("Hugging Face MCP"));
        assert!(message.contains("Hub workflows"));
    }

    #[test]
    fn login_slash_command_reports_status_and_key_opens_picker() {
        let mut app = create_test_app();
        let status = execute("/login status", &mut app);
        assert!(!status.is_error);
        let message = status.message.expect("login status");
        assert!(message.contains("Codewhale login"), "{message}");
        assert!(message.contains("Account:"), "{message}");
        assert!(message.contains("codewhale login"), "{message}");
        // No-brand invariant: the internal cloud-agent slot is not user
        // surface, so status never names it or teaches a set-slot command.
        assert!(!message.contains("Daytona"), "{message}");
        assert!(!message.contains("set-slot"), "{message}");

        assert_eq!(
            execute("/login", &mut app).action,
            Some(AppAction::OpenProviderPicker)
        );
        assert!(execute("/login status extra", &mut app).is_error);
        let key = execute("/login key", &mut app);
        assert!(!key.is_error);
        assert_eq!(key.action, Some(AppAction::OpenProviderPicker));

        let daytona = execute("/login daytona", &mut app);
        assert!(daytona.is_error);
        let err = daytona.message.expect("usage");
        assert!(
            err.contains("Usage: /login [status|account|key|<provider>]"),
            "{err}"
        );

        let unknown = execute("/login oauth", &mut app);
        assert!(unknown.is_error);
        let err = unknown.message.expect("usage");
        assert!(err.contains("Usage: /login"), "{err}");
    }

    #[test]
    fn xai_device_auth_slash_command_starts_login() {
        let mut app = create_test_app();
        let result = execute("/auth xai-device", &mut app);
        assert!(!result.is_error);
        assert!(matches!(
            result.action,
            Some(AppAction::StartXaiDeviceLogin)
        ));
    }

    #[test]
    fn chatgpt_auth_slash_command_starts_login() {
        let mut app = create_test_app();
        let result = execute("/auth chatgpt", &mut app);
        assert!(!result.is_error);
        assert!(matches!(
            result.action,
            Some(AppAction::StartChatgptPkceLogin)
        ));
    }

    #[test]
    fn chatgpt_revoke_slash_command_defers_to_the_event_loop() {
        // The remote revoke is a blocking round trip; the command must hand it
        // to the loop instead of doing it inline (#5784 review).
        let mut app = create_test_app();
        let result = execute("/auth chatgpt-revoke", &mut app);
        assert!(!result.is_error);
        assert!(matches!(result.action, Some(AppAction::StartChatgptRevoke)));
    }

    #[test]
    fn rlm_slash_command_routes_to_persistent_tool_instruction() {
        let mut app = create_test_app();
        let result = execute("/rlm 2 inspect this long corpus", &mut app);
        assert!(!result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or("")
                .contains("persistent working context")
        );
        let Some(AppAction::SendMessage(message)) = result.action else {
            panic!("expected SendMessage action");
        };
        assert!(message.contains("session-persistent working context"));
        assert!(message.contains("Do not use legacy `rlm` tool actions"));
    }

    /// `/kernel` was briefly introduced by an in-flight change and rejected:
    /// the persistent working context is ordinary Agent behavior, not a
    /// control surface users have to learn.
    #[test]
    fn kernel_is_not_a_command() {
        let mut app = create_test_app();
        let result = execute("/kernel inspect the fresh corpus", &mut app);
        assert!(
            result.is_error,
            "/kernel must not resolve to a registered command"
        );
    }

    #[test]
    fn agent_slash_command_routes_to_persistent_tool_instruction() {
        let mut app = create_test_app();
        let result = execute("/agent 0 inspect the parser", &mut app);
        assert!(!result.is_error);
        let Some(AppAction::SendMessage(message)) = result.action else {
            panic!("expected SendMessage action");
        };
        assert!(message.contains("`agent`"));
        assert!(message.contains("max_depth: 0"));
    }

    #[test]
    fn relay_slash_command_routes_to_session_relay_instruction() {
        let mut app = create_test_app();
        app.goal.objective = Some("Unify the work surface".to_string());
        app.goal.token_budget = Some(12_000);
        {
            let mut todos = app.todos.try_lock().expect("todo lock");
            todos.add("inspect workspace".to_string(), TodoStatus::Completed);
            todos.add("patch relay command".to_string(), TodoStatus::InProgress);
        }
        {
            let mut plan = app.plan_state.try_lock().expect("plan lock");
            plan.update(UpdatePlanArgs {
                objective: Some("Keep relays grounded".to_string()),
                explanation: Some("RLM-style strategy".to_string()),
                sources_used: vec!["transcript context".to_string()],
                critical_files: vec!["crates/tui/src/commands/mod.rs".to_string()],
                constraints: vec!["Do not invent verification".to_string()],
                verification_plan: Some("Check relay prompt assertions".to_string()),
                handoff_packet: Some("Next thread should read the To-do list".to_string()),
                plan: vec![PlanItemArg {
                    step: "keep To-do primary".to_string(),
                    status: StepStatus::InProgress,
                }],
                ..UpdatePlanArgs::default()
            });
        }

        let result = execute("/relay verify install", &mut app);
        assert!(!result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains(crate::prompts::HANDOFF_RELATIVE_PATH)
        );
        let Some(AppAction::SendMessage(message)) = result.action else {
            panic!("expected SendMessage action");
        };
        assert!(message.contains("session relay"));
        assert!(message.contains("接力"));
        // The relay is written where the next session reads it first.
        assert!(message.contains(&format!(
            "Write or update `{}`",
            crate::prompts::HANDOFF_RELATIVE_PATH
        )));
        assert!(message.contains("# Session relay"));
        assert!(message.contains("Requested relay focus: verify install"));
        assert!(message.contains("Goal objective: Unify the work surface"));
        assert!(message.contains("Goal token budget: 12000"));
        // #3983: the relay artifact shows the same bounded To-do snapshot body
        // a forked agent is handed — byte for byte.
        let expected_body = crate::todo_snapshot::todo_snapshot_body(
            &app.todos.try_lock().expect("todo lock").snapshot(),
        )
        .expect("canonical body");
        assert_eq!(
            expected_body,
            "To-do (50% settled)\n- [x] #1 inspect workspace\n- [~] #2 patch relay command"
        );
        assert!(
            message.contains(&expected_body),
            "relay must embed the canonical To-do body: {message}"
        );
        assert!(message.contains("Conversational strategy notes from update_plan"));
        assert!(message.contains("Objective: Keep relays grounded"));
        assert!(message.contains("Explanation: RLM-style strategy"));
        assert!(message.contains("Source: transcript context"));
        assert!(message.contains("Critical file: crates/tui/src/commands/mod.rs"));
        assert!(message.contains("Constraint: Do not invent verification"));
        assert!(message.contains("Verification plan: Check relay prompt assertions"));
        assert!(message.contains("Handoff packet: Next thread should read the To-do list"));
        assert!(message.contains("[in_progress] keep To-do primary"));
        assert!(
            !message.contains("Work checklist"),
            "relay copy should use To-do vocabulary: {message}"
        );
    }

    /// #3983: `update_plan` is conversational strategy, not a To-do. A session
    /// with plan state and an empty To-do has no list to hand off, and the
    /// relay artifact must not manufacture one.
    #[test]
    fn relay_does_not_present_plan_only_state_as_work_state() {
        let mut app = create_test_app();
        {
            let mut plan = app.plan_state.try_lock().expect("plan lock");
            plan.update(UpdatePlanArgs {
                objective: Some("Ship the To-do seam".to_string()),
                plan: vec![PlanItemArg {
                    step: "draft the renderer".to_string(),
                    status: StepStatus::InProgress,
                }],
                ..UpdatePlanArgs::default()
            });
        }

        let result = execute("/relay", &mut app);
        let Some(AppAction::SendMessage(message)) = result.action else {
            panic!("expected SendMessage action");
        };

        assert!(
            !message.contains("Current To-do:"),
            "plan-only state must not render as a To-do: {message}"
        );
        assert!(
            !message.contains("To-do ("),
            "plan-only state must not synthesize a To-do list: {message}"
        );
        assert!(message.contains("Conversational strategy notes from update_plan"));
    }

    /// #3983: a graph-backed update is authoritative immediately, even before
    /// the compatibility To-do projection is published to the UI.
    #[tokio::test]
    async fn relay_reads_same_turn_graph_backed_work_update() {
        use crate::tools::spec::ToolSpec as _;

        let mut app = create_test_app();
        let work =
            crate::work_graph::new_shared_work_runtime(app.todos.clone(), app.plan_state.clone());
        app.runtime_services.work = Some(work.clone());

        let mut context = crate::tools::spec::ToolContext::new(app.workspace.clone());
        context.runtime.work = Some(work);
        crate::tools::todo::TodoWriteTool::new(app.todos.clone())
            .execute(
                serde_json::json!({
                    "todos": [{"content": "relay the staged graph", "status": "in_progress"}]
                }),
                &context,
            )
            .await
            .expect("graph-backed todo_write");

        assert!(
            app.todos.lock().await.snapshot().is_empty(),
            "precondition: legacy projection has not published yet"
        );

        let result = execute("/relay", &mut app);
        let Some(AppAction::SendMessage(message)) = result.action else {
            panic!("expected SendMessage action");
        };
        assert!(
            message.contains("[~] #1 relay the staged graph"),
            "{message}"
        );
    }

    #[test]
    fn relay_command_has_bilingual_aliases() {
        let relay = command_infos()
            .into_iter()
            .find(|cmd| cmd.name == "relay")
            .expect("relay command should exist");
        assert_eq!(relay.aliases, &["batonpass", "接力"]);
        assert!(relay.description_for(Locale::ZhHans).contains("接力"));
        assert!(relay.description_for(Locale::ZhHant).contains("接力"));

        let mut app = create_test_app();
        let result = execute("/接力 next hand", &mut app);
        assert!(!result.is_error);
        let Some(AppAction::SendMessage(message)) = result.action else {
            panic!("expected SendMessage action");
        };
        assert!(message.contains("Requested relay focus: next hand"));
    }

    /// AT-008: No built-in command name or alias is registered twice,
    /// and no built-in alias collides with another command's canonical name.
    /// This test iterates every command from `command_infos()` (all 9 groups)
    /// and asserts uniqueness across the full set of names and aliases.
    #[test]
    fn command_registry_has_unique_names_and_aliases() {
        let mut names = std::collections::BTreeSet::new();
        for command in command_infos() {
            assert!(
                names.insert(command.name),
                "duplicate command name /{}",
                command.name
            );
        }

        let mut aliases = std::collections::BTreeSet::new();
        for command in command_infos() {
            for alias in command.aliases {
                assert!(
                    !names.contains(alias),
                    "alias /{alias} collides with a command name"
                );
                assert!(aliases.insert(*alias), "duplicate command alias /{alias}");
            }
        }
    }

    /// AT-009: Command ownership contract — top-level `commands/mod.rs` only
    /// registers groups (`groups::all_command_groups()`), each group owns its
    /// `commands()` list, and every command has valid metadata.
    ///
    /// Config and debug groups are documented permanent exceptions: they keep
    /// group-local `CommandInfo` statics and `dispatch()` in `mod.rs` rather
    /// than extracting every command into a focused module. This is accepted
    /// final structure per FEAT-008 §3.2.
    ///
    /// Enforcement strategy:
    /// - Exactly 9 source-verified groups (from `groups/mod.rs`)
    /// - Each group owns its commands() list
    /// - Config and debug exceptions verified within their specific groups by
    ///   identifying the group through its first command ("config" and "tokens")
    /// - Not circular: the group-iterated command count is a consistency check;
    ///   the primary enforcement is exact group count + per-group non-empty + valid metadata
    #[test]
    fn command_ownership_contract_is_enforced() {
        let groups = groups::all_command_groups();

        // AT-009 primary: exactly 9 groups matching groups/mod.rs
        assert_eq!(
            groups.len(),
            9,
            "expected exactly 9 command groups (core, session, config, debug, \
             project, skills, memory, plugins, utility), got {}",
            groups.len()
        );

        let mut total_commands = 0;
        let mut has_config = false;
        let mut has_debug = false;
        for &group in groups {
            let commands = group.commands();
            assert!(
                !commands.is_empty(),
                "each group must have at least one command"
            );
            for cmd in commands {
                let info = cmd.info();
                assert!(!info.name.is_empty(), "command name must not be empty");
                assert!(
                    is_palette_safe_command_name(info.name),
                    "/{} command names must be lowercase ASCII kebab-case",
                    info.name
                );
                let usage_prefix = format!("/{}", info.name);
                assert!(
                    info.usage.starts_with(&usage_prefix),
                    "/{} usage must start with /{{name}}, got {:?}",
                    info.name,
                    info.usage
                );
            }
            total_commands += commands.len();

            // Identify config and debug groups by their command content to
            // verify permanent-exception counts within the correct group.
            if commands.iter().any(|c| c.info().name == "config") {
                has_config = true;
                assert_eq!(
                    commands.len(),
                    17,
                    "config group (group-local metadata exception) expected \
                     exactly 17 commands, got {}",
                    commands.len()
                );
            }
            if commands.iter().any(|c| c.info().name == "tokens") {
                has_debug = true;
                assert_eq!(
                    commands.len(),
                    14,
                    "debug group (group-local metadata exception) expected \
                     exactly 14 commands, got {}",
                    commands.len()
                );
            }
        }

        // Config and debug groups must be found and verified by content identity
        assert!(
            has_config,
            "config group not found (expected first command: /config)"
        );
        assert!(
            has_debug,
            "debug group not found (expected first command: /tokens)"
        );

        // Consistency: group-iterated command count must match registry.
        // FEAT-015 registers one test-only contextual command (`/feat015ctx`)
        // under `#[cfg(test)]` to prove the dual-path seam (D6); the nine
        // production groups remain exactly 96 commands.
        let test_only_count = command_infos()
            .iter()
            .filter(|info| info.name == "feat015ctx")
            .count();
        assert_eq!(
            total_commands + test_only_count,
            command_infos().len(),
            "group-iterated command count must match registry infos count"
        );
    }

    #[test]
    fn command_groups_are_cached_once() {
        let first_groups = groups::all_command_groups();
        let second_groups = groups::all_command_groups();
        assert!(
            std::ptr::eq(first_groups.as_ptr(), second_groups.as_ptr()),
            "command group list should be cached"
        );

        for &group in first_groups {
            let first_commands = group.commands();
            let second_commands = group.commands();
            assert!(
                std::ptr::eq(first_commands.as_ptr(), second_commands.as_ptr()),
                "command list should be cached per group"
            );
        }
    }

    #[test]
    fn command_registry_metadata_is_complete_and_palette_safe() {
        for command in command_infos() {
            assert!(!command.name.is_empty(), "command name must not be empty");
            assert_eq!(
                command.name.trim(),
                command.name,
                "/{} command name must not need trimming",
                command.name
            );
            assert!(
                is_palette_safe_command_name(command.name),
                "/{} command names must stay lowercase ASCII kebab-case",
                command.name
            );

            let expected_usage_prefix = format!("/{}", command.name);
            assert!(
                command.usage.starts_with(&expected_usage_prefix),
                "/{} usage must start with its canonical slash command, got {:?}",
                command.name,
                command.usage
            );

            let description = command.description_for(Locale::En);
            assert!(
                !description.trim().is_empty(),
                "/{} must have non-empty English help text",
                command.name
            );
            // #3913: descriptions must not restate the usage field — the
            // palette and /help already append `usage` when arguments exist.
            assert!(
                !description.contains(command.usage),
                "/{} description embeds its usage string {:?}: {description:?}",
                command.name,
                command.usage
            );
            assert!(
                !description.contains(&format!("/{}", command.name)),
                "/{} description embeds slash-command syntax that usage already covers: {description:?}",
                command.name
            );
            for banned_prefix in ["Toolbox:", "Reference:"] {
                assert!(
                    !description.starts_with(banned_prefix),
                    "/{} description should not start with {banned_prefix:?}: {description:?}",
                    command.name
                );
            }

            let palette_command = command.palette_command();
            assert!(
                palette_command.starts_with(&expected_usage_prefix),
                "/{} palette command must use the canonical command, got {:?}",
                command.name,
                palette_command
            );
            assert_eq!(
                palette_command.ends_with(' '),
                command.requires_argument(),
                "/{} palette command spacing must match argument requirement",
                command.name
            );

            for &alias in command.aliases {
                assert!(
                    !alias.trim().is_empty(),
                    "/{} alias must not be empty",
                    command.name
                );
                assert_eq!(
                    alias.trim(),
                    alias,
                    "/{} alias /{alias} must not need trimming",
                    command.name
                );
                assert!(
                    !alias.starts_with('/'),
                    "/{} alias /{alias} must be stored without a slash",
                    command.name
                );
                assert!(
                    !alias.chars().any(char::is_whitespace),
                    "/{} alias /{alias} must not contain whitespace",
                    command.name
                );
                assert!(
                    !alias.chars().any(|ch| ch.is_ascii_uppercase()),
                    "/{} alias /{alias} must not contain uppercase ASCII",
                    command.name
                );
            }
        }
    }

    #[test]
    fn flagship_orchestration_and_workspace_commands_are_visible_at_the_palette_root() {
        for name in [
            "auto",
            "dispatch",
            "goal",
            "hooks",
            "tokens",
            "translate",
            "workflow",
            "workspace",
        ] {
            let info = registry()
                .get_info(name)
                .unwrap_or_else(|| panic!("/{name} must be registered"));
            assert!(
                info.show_in_empty_discovery(),
                "/{name} must appear at the palette root (#5442 / #5439)"
            );
            assert!(
                !traits::ADVANCED_DISCOVERY_COMMANDS.contains(&name),
                "/{name} must not stay on the Advanced discovery list"
            );
        }
    }

    #[test]
    fn command_discovery_tier_lists_use_canonical_registered_names() {
        for (tier_name, names) in [
            ("advanced", traits::ADVANCED_DISCOVERY_COMMANDS),
            ("compatibility", traits::COMPATIBILITY_DISCOVERY_COMMANDS),
        ] {
            for &name in names {
                let info = registry()
                    .get_info(name)
                    .unwrap_or_else(|| panic!("{tier_name} discovery entry {name:?} must resolve"));
                assert_eq!(
                    info.name, name,
                    "{tier_name} discovery entry {name:?} must be canonical, not an alias for /{}",
                    info.name
                );
            }
        }
    }

    #[test]
    fn command_info_resolves_canonical_names_and_aliases() {
        for command in command_infos() {
            for lookup in [command.name.to_string(), format!("/{}", command.name)] {
                let resolved = get_command_info(&lookup)
                    .unwrap_or_else(|| panic!("{lookup:?} should resolve to /{}", command.name));
                assert_eq!(resolved.name, command.name);
            }

            for &alias in command.aliases {
                for lookup in [alias.to_string(), format!("/{alias}")] {
                    let resolved = get_command_info(&lookup).unwrap_or_else(|| {
                        panic!("{lookup:?} should resolve to /{}", command.name)
                    });
                    assert_eq!(resolved.name, command.name);
                }
            }
        }
    }

    #[test]
    fn every_registered_command_has_a_help_topic() {
        let mut app = create_test_app();
        for command in command_infos() {
            let result = execute(&format!("/help {}", command.name), &mut app);
            assert!(
                !result.is_error,
                "/help {} returned an error: {result:?}",
                command.name
            );
            let message = result
                .message
                .unwrap_or_else(|| panic!("/help {} should return text", command.name));
            assert!(
                message.contains(command.name),
                "/help {} should mention the command name, got {message:?}",
                command.name
            );
            assert!(
                message.contains(command.usage),
                "/help {} should include usage {:?}, got {message:?}",
                command.name,
                command.usage
            );
        }
    }

    #[test]
    fn context_command_opens_inspector_and_keeps_ctx_alias() {
        let context = command_infos()
            .into_iter()
            .find(|cmd| cmd.name == "context")
            .expect("context command should exist");
        assert_eq!(context.aliases, &["ctx"]);
        assert!(context.description_for(Locale::En).contains("inspector"));

        let mut app = create_test_app();
        let result = execute("/ctx", &mut app);
        assert!(matches!(
            result.action,
            Some(AppAction::OpenContextInspector)
        ));

        let report = execute("/context report", &mut app);
        let message = report.message.expect("context report should return text");
        assert!(message.contains("Context Source Map"));
    }

    #[test]
    fn cache_inspect_dispatches_through_cache_command() {
        let mut app = create_test_app();
        let result = execute("/cache inspect", &mut app);
        let msg = result.message.expect("cache inspect should return text");
        assert!(msg.contains("Cache Inspect"));
        assert!(msg.contains("Base static prefix hash:"));
        assert!(msg.contains("Full request prefix hash:"));
        assert!(result.action.is_none());
    }

    #[test]
    fn cache_warmup_dispatches_action() {
        let mut app = create_test_app();
        let result = execute("/cache warmup", &mut app);
        assert!(result.message.is_none());
        assert!(matches!(result.action, Some(AppAction::CacheWarmup)));
    }

    #[test]
    fn execute_config_opens_config_view_action() {
        let mut app = create_test_app();
        let result = execute("/config", &mut app);
        assert!(result.message.is_none());
        assert!(matches!(result.action, Some(AppAction::OpenConfigView)));
    }

    #[test]
    fn execute_verbose_toggles_live_transcript_detail() {
        let mut app = create_test_app();
        assert!(!app.verbose_transcript);

        let result = execute("/verbose on", &mut app);
        assert!(!result.is_error);
        assert!(app.verbose_transcript);
        assert!(result.message.unwrap().contains("on"));

        let result = execute("/verbose off", &mut app);
        assert!(!result.is_error);
        assert!(!app.verbose_transcript);
        assert!(result.message.unwrap().contains("off"));
    }

    #[test]
    fn voice_send_and_voice_control_commands_toggle_state() {
        let mut app = create_test_app();
        assert!(!app.voice_send_enabled);
        assert!(!app.voice_control_enabled);

        for invocation in ["/voicesend", "/voice-send", "/yuyinsend", "/语音发送"] {
            let result = execute(invocation, &mut app);
            assert!(!result.is_error, "{invocation} should toggle cleanly");
            assert!(result.action.is_none());
            assert!(result.message.is_some());
        }
        // Four toggles land back at disabled.
        assert!(!app.voice_send_enabled);

        let result = execute("/voicecontrol", &mut app);
        assert!(!result.is_error);
        assert!(app.voice_control_enabled);
        let result = execute("/voice-control", &mut app);
        assert!(!result.is_error);
        assert!(!app.voice_control_enabled);
    }

    /// `/voice` defers the actual capture to the UI event loop via
    /// `AppAction::VoiceCapture`, so executing it never records audio.
    /// On hosts without a recorder it must fail gracefully instead.
    #[test]
    fn voice_command_toggles_on_and_off_or_fails_gracefully() {
        let mut app = create_test_app();
        let result = execute("/voice", &mut app);
        if app.voice_enabled {
            assert!(!result.is_error);
            assert!(matches!(result.action, Some(AppAction::VoiceCapture)));
            let off = execute("/voice", &mut app);
            assert!(!off.is_error);
            assert!(off.action.is_none());
            assert!(!app.voice_enabled);
        } else {
            assert!(result.is_error);
            assert!(result.action.is_none());
        }
    }

    #[test]
    fn execute_rail_sets_placement_and_reports_actual_state() {
        let mut app = create_test_app();

        let result = execute("/workbar off", &mut app);
        assert!(!result.is_error);
        assert_eq!(app.work_surface.placement, WorkSurfacePlacement::Off);
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("Workbar is off")
        );

        let result = execute("/rail right", &mut app);
        assert!(!result.is_error);
        assert_eq!(app.work_surface.placement, WorkSurfacePlacement::Right);
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("right placement")
        );

        // The /rail and /sidebar aliases drive the same workbar.
        let result = execute("/sidebar left", &mut app);
        assert!(!result.is_error);
        assert_eq!(app.work_surface.placement, WorkSurfacePlacement::Left);

        let result = execute("/rail top", &mut app);
        assert!(!result.is_error);
        assert_eq!(app.work_surface.placement, WorkSurfacePlacement::Top);

        // Bare /workbar reports the actual rendered state; it must never claim
        // visibility for a surface that cannot render.
        app.work_surface.placement = WorkSurfacePlacement::Off;
        let result = execute("/workbar", &mut app);
        assert!(!result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("Workbar is off")
        );
    }

    #[test]
    fn execute_rail_accepts_panel_targets_and_legacy_words() {
        let mut app = create_test_app();

        let result = execute("/rail agents", &mut app);
        assert!(!result.is_error);
        assert_eq!(app.work_surface.panel, RailPanel::Agents);

        let result = execute("/sidebar context", &mut app);
        assert!(!result.is_error);
        assert_eq!(app.work_surface.panel, RailPanel::Context);

        let result = execute("/rail activity", &mut app);
        assert!(!result.is_error);
        assert_eq!(
            app.work_surface.panel,
            RailPanel::Tasks,
            "activity maps onto the Tasks panel"
        );

        let result = execute("/rail pinned", &mut app);
        assert!(!result.is_error);
        assert_eq!(
            app.work_surface.panel,
            RailPanel::Tasks,
            "pinned folded into the tasks view"
        );
        let result = execute("/rail files", &mut app);
        assert!(!result.is_error);
        assert_eq!(app.work_surface.panel, RailPanel::Files);

        let result = execute("/sidebar on", &mut app);
        assert!(!result.is_error);
        assert_eq!(
            app.work_surface.placement,
            WorkSurfacePlacement::Bottom,
            "on restores the default bottom workbar (round 3)"
        );

        let result = execute("/sidebar none", &mut app);
        assert!(!result.is_error);
        assert_eq!(app.work_surface.placement, WorkSurfacePlacement::Off);
    }

    #[test]
    fn execute_rail_rejects_invalid_args() {
        let mut app = create_test_app();
        let result = execute("/rail maybe", &mut app);
        assert!(result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("Usage: /workbar")
        );
    }

    #[test]
    fn execute_links_and_aliases_return_links_message() {
        let mut app = create_test_app();
        for cmd in ["/links", "/dashboard", "/api", "/lianjie"] {
            let result = execute(cmd, &mut app);
            let msg = result.message.expect("links commands should return text");
            assert!(msg.contains("https://codewhale.net/en/docs"));
            assert!(msg.contains("https://codewhale.net/en/community"));
            assert!(msg.contains("https://github.com/codewhale-hq/CodeWhale"));
            assert!(msg.contains("https://app.codewhale.net"));
            assert!(msg.contains("separate sign-in"));
            assert!(msg.contains("not connected to the current local session"));
            assert!(msg.contains("https://platform.deepseek.com"));
            assert!(result.action.is_none());
        }
    }

    #[test]
    fn execute_workspace_alias_switches_workspace() {
        let dir = tempdir().expect("temp dir");
        let mut app = create_test_app();
        let result = execute(&format!("/cwd {}", dir.path().display()), &mut app);
        assert!(matches!(
            result.action,
            Some(AppAction::SwitchWorkspace { workspace }) if workspace == dir.path().canonicalize().unwrap()
        ));
    }

    #[test]
    fn removed_set_and_deepseek_commands_show_migration_hints() {
        let mut app = create_test_app();
        let set_result = execute("/set model deepseek-v4-pro", &mut app);
        let set_msg = set_result
            .message
            .expect("legacy command should return an error message");
        assert!(set_msg.contains("The /set command was retired"));
        assert!(set_msg.contains("/config"));
        assert!(set_msg.contains("/settings"));
        assert!(set_result.action.is_none());

        let deepseek_result = execute("/deepseek", &mut app);
        let deepseek_msg = deepseek_result
            .message
            .expect("legacy command should return an error message");
        assert!(deepseek_msg.contains("The /deepseek command was renamed"));
        assert!(deepseek_msg.contains("/links"));
        assert!(deepseek_msg.contains("/dashboard"));
        assert!(deepseek_msg.contains("/api"));
        assert!(deepseek_result.action.is_none());
    }

    /// Seals the user's home *and* points the config at the fixture's own
    /// file. Dispatching every command reaches credentials, sessions, snapshots,
    /// plugin bundles, audit logs and the `/import-claude` report — all of which
    /// resolve under the home, so pinning the config path alone (as this once
    /// did) left them on the developer's real profile.
    struct ConfigPathGuard {
        // Fields drop in order: restore the config path, then the seal.
        _config_path: crate::test_support::EnvVarGuard,
        _home: crate::test_support::SealedHome,
    }

    impl ConfigPathGuard {
        fn new(config_path: &Path) -> Self {
            let home = crate::test_support::SealedHome::new();
            let config = crate::test_support::EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", config_path);
            Self {
                _config_path: config,
                _home: home,
            }
        }
    }

    /// Build an App scoped to an isolated tempdir so dispatch-side-effects
    /// (e.g. `/init` writing AGENTS.md, explicit `/export <path>` writes, or
    /// `/logout` clearing credentials) don't pollute the repo working tree or
    /// the developer's real config when the smoke tests run.
    fn create_isolated_test_app() -> (App, tempfile::TempDir, ConfigPathGuard) {
        let tmpdir = tempfile::TempDir::new().expect("tempdir for smoke test");
        let workspace = tmpdir.path().to_path_buf();
        let config_path = workspace.join(".deepseek").join("config.toml");
        std::fs::create_dir_all(config_path.parent().expect("config parent")).expect("config dir");
        let guard = ConfigPathGuard::new(&config_path);
        // Skills live under the workspace here, so they load only once trusted.
        crate::test_support::trust_workspace(&workspace);
        let options = TuiOptions {
            config_path: Some(config_path),
            skills_dir: workspace.join("skills"),
            memory_path: workspace.join("memory.md"),
            notes_path: workspace.join("notes.txt"),
            mcp_config_path: workspace.join("mcp.json"),
            ..crate::test_support::test_tui_options(workspace.clone())
        };
        let app = App::new(options, &Config::default());
        assert!(
            app.dispatch_completion_tx.is_none(),
            "dispatch smoke fixtures must not permit native window mutations"
        );
        (app, tmpdir, guard)
    }

    /// Smoke test: every entry in `command_infos()` must dispatch to a real handler.
    /// A dispatch miss surfaces as the fall-through `Unknown command:` error
    /// message in `execute`. This catches the case where a new command is
    /// added to `command_infos()` (so it shows up in `/help` and the palette) but
    /// the matching arm in `execute` is forgotten — the user would type the
    /// command, see it autocomplete, and then get an unhelpful "did you
    /// mean" suggestion. Also catches panics in handlers because the test
    /// runner unwinds the panic and reports the offending command.
    /// `/save` still defaults its output path, while `/export` accepts a legacy
    /// direct file path. Pass explicit tempdir paths so this smoke test covers
    /// both file handlers without touching the developer's clipboard.
    fn invocation_for(command_name: &str, alias_or_name: &str, tmpdir: &std::path::Path) -> String {
        match command_name {
            "save" => format!("/{alias_or_name} {}", tmpdir.join("session.json").display()),
            "export" => format!("/{alias_or_name} {}", tmpdir.join("chat.md").display()),
            _ => format!("/{alias_or_name}"),
        }
    }

    /// `/restore` is covered by its own dedicated tests in
    /// `commands/restore.rs` that serialize on the global env mutex via
    /// `scoped_home` (snapshot repo init shells out to git, which races
    /// against parallel-running tests). Skip it here so this smoke test
    /// stays parallel-safe.
    ///
    /// `/pin` is covered on every platform. The headless fixture has no
    /// completion mailbox, so Windows rejects it before resolving or changing
    /// a host window; the former synchronous message-pump wait cannot occur.
    fn skip_in_dispatch_smoke(name: &str) -> bool {
        name == "restore"
    }

    /// Upper bound on a single command dispatch in the smoke tests.
    ///
    /// Generous next to the millisecond each handler actually takes, and far
    /// below nextest's 600 s test timeout, so a handler that blocks fails the
    /// test *by name* instead of burning a ten-minute CI slot with no
    /// attribution (#5919).
    const DISPATCH_WATCHDOG: std::time::Duration = std::time::Duration::from_secs(30);

    /// Dispatch one command under a per-command watchdog and return the
    /// handler's message.
    ///
    /// The app is built and the command executed on a dedicated thread; the
    /// test thread waits on the result with a timeout. A handler that never
    /// returns leaves its thread parked, but the test itself fails
    /// immediately, naming the invocation. A handler that panics still
    /// surfaces as that panic — the smoke tests are the repo's only
    /// panic-in-a-handler net, so the payload is resumed rather than
    /// swallowed.
    fn dispatch_under_watchdog(command_name: &str, alias_or_name: &str) -> Option<String> {
        let label = format!("/{alias_or_name}");
        let (tx, rx) = std::sync::mpsc::channel();
        let name = command_name.to_string();
        let alias = alias_or_name.to_string();
        let handle = std::thread::Builder::new()
            .name(format!("dispatch-smoke-{alias_or_name}"))
            // Command handlers are deeply recursive in debug builds; match the
            // 16 MiB the CI runner sets via RUST_MIN_STACK for the main thread.
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                let (mut app, tmpdir, _guard) = create_isolated_test_app();
                let invocation = invocation_for(&name, &alias, tmpdir.path());
                let result = execute(&invocation, &mut app);
                let _ = tx.send(result.message);
            })
            .expect("spawn dispatch smoke thread");

        let started = std::time::Instant::now();
        match rx.recv_timeout(DISPATCH_WATCHDOG) {
            Ok(message) => {
                let _ = handle.join();
                // Quiet on the common path; a handler heading for the
                // watchdog still leaves a named breadcrumb in the log.
                let elapsed = started.elapsed();
                if elapsed > std::time::Duration::from_secs(1) {
                    eprintln!("dispatch smoke: {label} took {elapsed:?}");
                }
                message
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!(
                "{label} did not return within {DISPATCH_WATCHDOG:?}: its handler blocks. \
                 Fix the handler or add it to skip_in_dispatch_smoke with a reason."
            ),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => match handle.join() {
                Ok(()) => panic!("{label} dispatch thread ended without producing a result"),
                Err(payload) => std::panic::resume_unwind(payload),
            },
        }
    }

    #[test]
    fn slash_parser_preserves_arguments_after_the_command_name() {
        let mut app = create_test_app();
        let result = execute("/agent 2 review   this   carefully", &mut app);
        assert!(!result.is_error);
        let Some(AppAction::SendMessage(message)) = result.action else {
            panic!("expected /agent to send a model instruction");
        };
        assert!(message.contains(r#"prompt: "review   this   carefully""#));
        assert!(message.contains("max_depth: 2"));

        let mut app = create_test_app();
        let result = execute("   /relay   ship   command   harness   ", &mut app);
        assert!(!result.is_error);
        let Some(AppAction::SendMessage(message)) = result.action else {
            panic!("expected /relay to send a model instruction");
        };
        assert!(message.contains("Requested relay focus: ship   command   harness"));

        let mut app = create_test_app();
        let result = execute("/rlm 3 inspect   this   corpus", &mut app);
        assert!(!result.is_error);
        let Some(AppAction::SendMessage(message)) = result.action else {
            panic!("expected /rlm to send a model instruction");
        };
        assert!(message.contains(r#"this text: "inspect   this   corpus""#));
        assert!(message.contains("session-persistent working context"));
    }

    #[test]
    fn representative_command_groups_keep_dispatch_surfaces() {
        let mut app = create_test_app();
        let help = execute("/help clear", &mut app)
            .message
            .expect("/help clear should return text");
        assert!(help.contains("clear"));
        assert!(help.contains("/clear"));

        let mut app = create_test_app();
        let result = execute("/config", &mut app);
        assert!(matches!(result.action, Some(AppAction::OpenConfigView)));

        let mut app = create_test_app();
        let result = execute("/relay command boundary", &mut app);
        assert!(!result.is_error);
        assert!(matches!(
            result.action,
            Some(AppAction::SendMessage(message))
                if message.contains("Requested relay focus: command boundary")
        ));

        let mut app = create_test_app();
        let note_help = execute("/note help", &mut app)
            .message
            .expect("/note help should return text");
        assert!(note_help.contains("Usage: /note"));

        let mut app = create_test_app();
        let result = execute("/goal ship layer 2 | budget: 100", &mut app);
        assert!(!result.is_error);
        assert!(matches!(
            result.action,
            Some(AppAction::SetGoalObjective {
                ref objective,
                token_budget: Some(100)
            }) if objective == "ship layer 2"
        ));
        // The hunt-era alias is gone: `/hunt` must not resolve anymore.
        assert!(execute("/hunt ship layer 2", &mut app).is_error);

        let (mut app, _tmpdir, _guard) = create_isolated_test_app();
        let result = execute("/skills", &mut app);
        assert!(matches!(
            result.action,
            Some(AppAction::OpenExtensions {
                tab: crate::tui::views::extensions::ExtensionsTab::Skills
            })
        ));

        let mut app = create_test_app();
        let result = execute("/task list", &mut app);
        assert!(matches!(result.action, Some(AppAction::TaskList)));

        let mut app = create_test_app();
        let tokens = execute("/tokens", &mut app)
            .message
            .expect("/tokens should return text");
        assert!(tokens.contains("deepseek-v4-pro"));
    }

    /// Smoke test: every entry in `command_infos()` must dispatch to a real handler.
    /// A dispatch miss surfaces as the fall-through `Unknown command:` error
    /// message in `execute`. This catches the case where a new command is
    /// added to `command_infos()` (so it shows up in `/help` and the palette) but
    /// the matching arm in `execute` is forgotten — the user would type the
    /// command, see it autocomplete, and then get an unhelpful "did you
    /// mean" suggestion. Also catches panics in handlers because the test
    /// runner unwinds the panic and reports the offending command.
    #[test]
    fn every_registered_command_dispatches_to_a_handler() {
        for command in command_infos() {
            if skip_in_dispatch_smoke(command.name) {
                continue;
            }
            if let Some(msg) = dispatch_under_watchdog(command.name, command.name) {
                assert!(
                    !msg.contains("Unknown command"),
                    "/{} fell through to the unknown-command branch: {msg}",
                    command.name,
                );
            }
        }
    }

    /// Same check, but for declared aliases — `/q` should not fall through
    /// just because the registry lists it as an alias of `/exit`.
    #[test]
    fn every_command_alias_dispatches_to_a_handler() {
        for command in command_infos() {
            if skip_in_dispatch_smoke(command.name) {
                continue;
            }
            for alias in command.aliases {
                if let Some(msg) = dispatch_under_watchdog(command.name, alias) {
                    assert!(
                        !msg.contains("Unknown command"),
                        "/{alias} (alias of /{}) fell through to unknown: {msg}",
                        command.name,
                    );
                }
            }
        }
    }

    #[test]
    fn balance_command_has_own_help_text() {
        let info = get_command_info("balance").expect("balance command should be registered");
        assert_eq!(info.description_id, MessageId::CmdBalanceDescription);
        assert!(
            info.description_for(Locale::En)
                .contains("provider account balance")
        );
    }

    #[test]
    fn balance_command_dispatches_live_fetch_for_prepaid_providers() {
        let mut app = create_test_app();
        for provider in [
            ProviderKind::Deepseek,
            ProviderKind::Openrouter,
            ProviderKind::Siliconflow,
        ] {
            app.api_provider = provider;
            let result = execute("/balance", &mut app);
            assert!(!result.is_error, "{provider:?}");
            assert!(
                matches!(result.action, Some(AppAction::FetchBalance)),
                "{provider:?} should dispatch a live remaining-credit fetch"
            );
        }
    }

    #[test]
    fn balance_command_reports_unsupported_provider_clearly() {
        let mut app = create_test_app();
        app.set_provider_identity(ProviderKind::Ollama, "ollama");

        let result = execute("/balance", &mut app);
        let msg = result
            .message
            .expect("unsupported providers should return a clear message");

        assert!(!result.is_error);
        assert!(msg.contains("Ollama"));
        assert!(msg.contains("not supported"));
        assert!(msg.contains("dashboard"));
    }

    #[test]
    fn unknown_command_suggests_nearest_match() {
        let mut app = create_test_app();
        let result = execute("/modle", &mut app);
        let msg = result
            .message
            .expect("unknown command should return an error message");
        assert!(msg.contains("Unknown command: /modle"));
        assert!(msg.contains("Did you mean:"));
        assert!(msg.contains("/model"));
    }

    #[test]
    fn unknown_command_without_close_match_keeps_help_guidance() {
        let mut app = create_test_app();
        let result = execute("/zzzzzz", &mut app);
        let msg = result
            .message
            .expect("unknown command should return an error message");
        assert!(msg.contains("Unknown command: /zzzzzz"));
        assert!(msg.contains("Type /help for available commands."));
    }

    #[test]
    fn dollar_skill_prefix_with_no_name_shows_usage() {
        let mut app = create_test_app();
        let result = execute("$", &mut app);
        assert!(result.is_error);
        let msg = result.message.expect("should return error message");
        assert!(msg.contains("Type a skill name after $"));
    }

    #[test]
    fn dollar_skill_prefix_unknown_skill_reports_unknown_skill() {
        let mut app = create_test_app();
        let result = execute("$definitely-not-a-real-skill-12345", &mut app);
        assert!(result.is_error);
        let msg = result.message.expect("should return error message");
        assert!(msg.contains("Unknown skill: $definitely-not-a-real-skill-12345"));
        assert!(msg.contains("/skills"));
    }

    #[test]
    fn dollar_skill_prefix_does_not_break_existing_slash_dispatch() {
        let mut app = create_test_app();
        let result = execute("/help", &mut app);
        assert!(!result.is_error);
    }

    fn write_test_skill(root: &Path, name: &str) {
        let skill_dir = root.join("skills").join(name);
        std::fs::create_dir_all(&skill_dir).expect("skill directory");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: Test {name} skill\n---\nFollow the test instructions."
            ),
        )
        .expect("skill fixture");
    }

    #[test]
    fn task_bearing_skill_invocations_send_the_task_on_the_activated_turn() {
        for invocation in ["$foo do X", "/foo do X", "/skill foo do X"] {
            let (mut app, tmpdir, _guard) = create_isolated_test_app();
            write_test_skill(tmpdir.path(), "foo");

            let result = execute(invocation, &mut app);

            assert!(!result.is_error, "{invocation}: {result:?}");
            assert!(
                result
                    .message
                    .as_deref()
                    .is_some_and(|message| message.contains("Skill 'foo' activated")),
                "{invocation}: {result:?}"
            );
            assert!(
                matches!(result.action, Some(AppAction::SendMessage(ref task)) if task == "do X"),
                "{invocation}: {result:?}"
            );
            assert!(
                app.active_skill
                    .as_deref()
                    .is_some_and(|instruction| instruction.contains("# Skill: foo")),
                "{invocation} did not arm foo for the dispatched task"
            );
        }
    }

    #[test]
    fn bare_dollar_skill_still_arms_the_next_message() {
        let (mut app, tmpdir, _guard) = create_isolated_test_app();
        write_test_skill(tmpdir.path(), "foo");

        let result = execute("$foo", &mut app);

        assert!(!result.is_error, "{result:?}");
        assert!(result.action.is_none());
        assert!(
            app.active_skill
                .as_deref()
                .is_some_and(|instruction| instruction.contains("# Skill: foo"))
        );
    }

    #[test]
    fn shorthand_can_invoke_a_skill_named_install_without_stealing_management_commands() {
        for invocation in ["$install do X", "/install do X"] {
            let (mut app, tmpdir, _guard) = create_isolated_test_app();
            write_test_skill(tmpdir.path(), "install");

            let result = execute(invocation, &mut app);

            assert!(!result.is_error, "{invocation}: {result:?}");
            assert!(
                matches!(result.action, Some(AppAction::SendMessage(ref task)) if task == "do X"),
                "{invocation}: {result:?}"
            );
            assert!(
                app.active_skill
                    .as_deref()
                    .is_some_and(|instruction| instruction.contains("# Skill: install")),
                "{invocation} did not activate the install skill"
            );
        }

        let (mut app, tmpdir, _guard) = create_isolated_test_app();
        write_test_skill(tmpdir.path(), "install");
        let result = execute("/skill install", &mut app);
        assert!(result.is_error, "management subcommand should show usage");
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|message| message.contains("/skill install"))
        );
        assert!(result.action.is_none());
        assert!(app.active_skill.is_none());
    }

    // ---------------------------------------------------------------------
    // FEAT-015: test-only contextual dispatch through the public dispatcher
    // (D6). The fixture is registered into the global registry only in test
    // builds and executes through the public `execute()`.
    // ---------------------------------------------------------------------

    #[test]
    fn feat015_contextual_command_executes_through_public_dispatcher() {
        let mut app = create_test_app();
        let result = execute("/feat015ctx hello", &mut app);
        assert!(!result.is_error, "{result:?}");
        let message = result.message.expect("message");
        assert!(message.contains("workspace="), "{message}");
        assert!(message.contains("mode="), "{message}");
        assert!(message.contains("currency="), "{message}");
        assert!(message.contains("arg=hello"), "{message}");
        assert!(result.action.is_none());
    }

    #[test]
    fn feat015_contextual_command_fails_safely_without_declared_facets() {
        let result = feat015_contextual(
            codewhale_command_contract::handler::CommandContexts::empty(),
            None,
        );
        assert!(result.is_error, "{result:?}");
        assert_eq!(
            result.message.as_deref(),
            Some("Error: Command capability unavailable: workspace")
        );
        assert!(result.action.is_none());
    }

    #[test]
    fn feat015_contextual_command_is_registered_only_in_test_builds() {
        // The fixture entry is present in the test-build registry with a
        // capability-scoped handler; production builds never see it.
        assert!(registry().has_contextual_handler("feat015ctx"));
        let info = registry().get_info("feat015ctx").expect("info");
        assert_eq!(info.name, "feat015ctx");
        assert_eq!(
            info.description_id,
            codewhale_localization::MessageId::CmdWorkspaceDescription,
            "portable description_key must bridge to the TUI localization id"
        );
    }

    #[test]
    fn feat015_unmigrated_production_entries_remain_legacy() {
        // FEAT-015 shipped no production contextual command. Later FEATs
        // register bounded portable groups/slices; every entry outside the
        // explicit list must still use the original legacy dispatcher.
        const MIGRATED_GROUPS: &[&str] = &[
            // FEAT-018 utility group.
            "attach",
            "automation",
            "dispatch",
            "jobs",
            "mcp",
            "network",
            "task",
            "update",
            // FEAT-021 project group.
            "init",
            "lsp",
            "share",
            "goal",
            // FEAT-019 memory group.
            "note",
            "memory",
            // FEAT-020 plugins group.
            "plugin",
            // FEAT-022 skills group.
            "skills",
            "skill",
            "review",
            "restore",
            // FEAT-023 session lifecycle slice.
            "branch",
            "compact",
            "fork",
            "load",
            "new",
            "purge",
            "save",
            "sessions",
            "tree",
            // FEAT-024 session control slice.
            "relay",
            "rename",
            "resume",
            "rc",
            "remote-env",
            "title",
            // FEAT-025 session export slice.
            "export",
            // FEAT-026 completes the session structural-copy slice.
            "structcopy",
            // FEAT-027 config policy/status slice; remaining config stays legacy.
            "permissions",
            "status",
            // FEAT-029 complete debug group, including receipts and mutation.
            "tokens",
            "cost",
            "receipts",
            "balance",
            "cache",
            "preview-request",
            "tools",
            "change",
            "system",
            "context",
            "edit",
            "diff",
            "undo",
            "retry",
        ];
        for info in command_infos() {
            if info.name == "feat015ctx" || MIGRATED_GROUPS.contains(&info.name) {
                continue;
            }
            assert!(
                !registry().has_contextual_handler(info.name),
                "/{} must remain on the legacy dispatch path",
                info.name
            );
        }
    }

    // ---------------------------------------------------------------------
    // FEAT-018: public pure/contextual dispatch and seven-entry inventory
    // (Task 6.2). These tests enter through the public registry/dispatch seam
    // and prove both handler variants plus all seven utility metadata records.
    // ---------------------------------------------------------------------

    #[test]
    fn feat018_all_seven_utility_entries_are_registered_with_portable_handlers() {
        for name in [
            "attach",
            "automation",
            "jobs",
            "mcp",
            "network",
            "task",
            "update",
        ] {
            let info = registry()
                .get_info(name)
                .unwrap_or_else(|| panic!("/{name} must be registered"));
            assert_eq!(info.name, name, "canonical name");
            assert!(
                registry().has_contextual_handler(name),
                "/{name} must carry a portable handler"
            );
        }
    }

    #[test]
    fn feat018_pure_utility_command_dispatches_through_public_seam() {
        let mut app = create_test_app();
        // /jobs is a Pure handler: it must execute without building an
        // envelope and return the same action as the parser.
        let result = execute("/jobs list", &mut app);
        assert!(!result.is_error, "{result:?}");
        assert!(
            matches!(
                result.action,
                Some(crate::tui::app::AppAction::ShellJob(
                    crate::tui::app::ShellJobAction::List
                ))
            ),
            "{result:?}"
        );

        // /update is Pure too; a bare check should reach the plan resolver and
        // return a message (or a safe error in a test environment), never a panic.
        let result = execute("/update", &mut app);
        assert!(result.message.is_some() || result.is_error, "{result:?}");
    }

    #[test]
    fn feat018_contextual_utility_commands_dispatch_through_public_seam() {
        let mut app = create_test_app();

        // /automation (contextual, presentation facet): list action.
        let automation = execute("/automation list", &mut app);
        assert!(
            matches!(
                automation.action,
                Some(crate::tui::app::AppAction::Automation(
                    crate::tui::app::AutomationAction::List
                ))
            ),
            "{automation:?}"
        );

        // /task (contextual, workspace facet): digest without a runtime must
        // produce the canonical no-active text.
        let task = execute("/task digest", &mut app);
        assert_eq!(
            task.message.as_deref(),
            Some("No active operations or to-do items."),
            "{task:?}"
        );

        // /mcp (contextual, presentation facet): status maps to Show action.
        let mcp = execute("/mcp status", &mut app);
        assert!(
            matches!(
                mcp.action,
                Some(crate::tui::app::AppAction::Mcp(
                    crate::tui::app::McpUiAction::Show
                ))
            ),
            "{mcp:?}"
        );

        // /attach (contextual, workspace + media facets): missing path is a
        // safe error, never a panic, and the composer is untouched.
        let attach = execute("/attach", &mut app);
        assert!(attach.is_error, "{attach:?}");
        assert!(app.input.is_empty(), "composer must stay unchanged");

        // /network (pure): list produces a message.
        let network = execute("/network list", &mut app);
        assert!(network.message.is_some() || network.is_error, "{network:?}");
    }

    // FEAT-021 project group public dispatch (Phase 6)

    #[test]
    fn feat021_project_entries_register_through_portable_bridge() {
        use codewhale_command_contract::handler::{CommandCapabilities, CommandHandler};

        for (name, expected) in [
            ("init", CommandCapabilities::WORKSPACE),
            ("lsp", CommandCapabilities::PROJECT),
            ("share", CommandCapabilities::SESSION_EXPORT),
            (
                "goal",
                CommandCapabilities::PROJECT.union(CommandCapabilities::PRESENTATION),
            ),
        ] {
            assert!(
                registry().has_contextual_handler(name),
                "/{name} must register through the portable bridge"
            );
            let handler = registry()
                .get(name)
                .expect("entry")
                .contextual_handler()
                .expect("contextual handler");
            let CommandHandler::Contextual { capabilities, .. } = handler else {
                panic!("/{name} must be contextual");
            };
            assert_eq!(capabilities, expected, "/{name} exact capability set");
        }
    }

    #[test]
    fn feat021_project_commands_dispatch_through_public_seam() {
        let mut app = create_test_app();
        app.workspace = PathBuf::from(".");

        // /init: creating message + SendMessage action.
        let init = execute("/init", &mut app);
        assert!(!init.is_error, "{init:?}");
        assert!(matches!(init.action, Some(AppAction::SendMessage(_))));

        // /lsp status reaches the adapter through the public seam.
        let lsp = execute("/lsp status", &mut app);
        assert!(!lsp.is_error, "{lsp:?}");
        let lsp_msg = lsp.message.expect("lsp message");
        assert!(
            lsp_msg.contains("LSP diagnostics are currently **"),
            "{lsp_msg}"
        );

        // /share help is a safe no-op route.
        let share = execute("/share help", &mut app);
        assert!(!share.is_error, "{share:?}");

        // /goal status without a goal prints usage (no panic).
        let goal = execute("/goal status", &mut app);
        assert!(!goal.is_error, "{goal:?}");

        // Metadata bridges to the TUI localization ids.
        for (name, id) in [
            ("init", MessageId::CmdInitDescription),
            ("lsp", MessageId::CmdLspDescription),
            ("share", MessageId::CmdShareDescription),
            ("goal", MessageId::CmdGoalDescription),
        ] {
            let info = registry().get_info(name).expect("info");
            assert_eq!(info.description_id, id, "/{name} description bridge");
        }
    }

    #[test]
    fn feat021_public_dispatch_never_panics_on_project_commands() {
        let mut app = create_test_app();
        app.workspace = PathBuf::from(".");
        for command in [
            "/init",
            "/init ",
            "/lsp",
            "/lsp status",
            "/lsp on",
            "/lsp off",
            "/lsp bogus",
            "/share",
            "/share help",
            "/share bogus",
            "/goal",
            "/goal status",
            "/goal pause",
            "/goal resume",
            "/goal done",
            "/goal bogus",
            "/goal 42",
        ] {
            let result = execute(command, &mut app);
            // Every path returns a result; none may panic.
            assert!(
                result.message.is_some() || result.action.is_some(),
                "{command}: {result:?}"
            );
        }
    }

    // ---------------------------------------------------------------------
    // FEAT-019: public memory registration/dispatch and exact capability
    // declarations (Task 6.2). Tests enter through the registry and the
    // public `execute` seam and prove the memory group's portable entries.
    // ---------------------------------------------------------------------

    /// App with an isolated temp workspace and memory enabled.
    fn memory_test_app(tmpdir: &tempfile::TempDir) -> App {
        let options = TuiOptions {
            memory_path: tmpdir.path().join("memory.md"),
            use_memory: true,
            ..crate::test_support::test_tui_options(tmpdir.path())
        };
        App::new(options, &Config::default())
    }

    #[test]
    fn feat019_memory_entries_are_registered_with_exact_capabilities() {
        for (name, expected) in [
            (
                "note",
                codewhale_command_contract::handler::CommandCapabilities::WORKSPACE,
            ),
            (
                "memory",
                codewhale_command_contract::handler::CommandCapabilities::WORKSPACE
                    .union(codewhale_command_contract::handler::CommandCapabilities::MEMORY),
            ),
        ] {
            assert!(
                registry().has_contextual_handler(name),
                "/{name} must register through the portable bridge"
            );
            let handler = registry()
                .get(name)
                .expect("entry")
                .contextual_handler()
                .expect("contextual handler");
            let codewhale_command_contract::handler::CommandHandler::Contextual {
                capabilities,
                ..
            } = handler
            else {
                panic!("/{name} must be contextual");
            };
            assert_eq!(capabilities, expected, "/{name} exact capability set");
            assert!(
                !capabilities.contains(
                    codewhale_command_contract::handler::CommandCapabilities::PRESENTATION
                ) && !capabilities
                    .contains(codewhale_command_contract::handler::CommandCapabilities::MEDIA),
                "/{name} must not declare presentation or media"
            );
        }
    }

    // ---------------------------------------------------------------------
    // FEAT-022: skills group registration + public dispatch (Task 6.2).
    // All four commands register through the portable bridge; frontier state
    // is asserted by the migration fixtures and live gate.
    // ---------------------------------------------------------------------

    /// Seals the user's home so global skill discovery stays hermetic.
    fn feat022_scoped_home(_tmp: &tempfile::TempDir) -> crate::test_support::SealedHome {
        crate::test_support::SealedHome::new()
    }

    fn feat022_test_app(tmp: &tempfile::TempDir) -> App {
        // The fixture's skills dir lives inside its workspace.
        crate::test_support::trust_workspace(tmp.path());
        let mut options = crate::test_support::test_tui_options(tmp.path());
        options.skills_dir = tmp.path().join("skills");
        crate::test_support::test_app_with_options(options)
    }

    fn feat022_write_skill(dir: &std::path::Path, name: &str) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name} skill\n---\n{name} instructions"),
        )
        .unwrap();
    }

    #[test]
    fn feat022_all_four_skills_entries_are_registered_with_portable_handlers() {
        use codewhale_command_contract::handler::{CommandCapabilities, CommandHandler};

        for (name, alias, expected) in [
            (
                "skills",
                Some("jinengliebiao"),
                CommandCapabilities::SKILL_GROUP,
            ),
            (
                "skill",
                Some("jineng"),
                CommandCapabilities::SKILL_GROUP.union(CommandCapabilities::SKILLS),
            ),
            ("review", Some("shencha"), CommandCapabilities::SKILL_GROUP),
            ("restore", None, CommandCapabilities::SKILL_GROUP),
        ] {
            let info = registry()
                .get_info(name)
                .unwrap_or_else(|| panic!("/{name} must be registered"));
            assert_eq!(info.name, name, "canonical name");
            let handler = registry()
                .get(name)
                .expect("entry")
                .contextual_handler()
                .expect("contextual handler");
            let CommandHandler::Contextual { capabilities, .. } = handler else {
                panic!("/{name} must be contextual");
            };
            assert_eq!(capabilities, expected, "/{name} exact capability set");
            if let Some(alias) = alias {
                assert!(
                    registry().get_info(alias).is_some(),
                    "/{name} alias {alias} must resolve"
                );
            }
        }
    }

    #[test]
    fn feat019_note_dispatches_through_public_seam() {
        let tmpdir = tempfile::TempDir::new().unwrap();
        let mut app = memory_test_app(&tmpdir);

        let appended = execute("/note hello from dispatch", &mut app);
        assert!(!appended.is_error, "{appended:?}");
        assert!(
            appended
                .message
                .as_deref()
                .is_some_and(|msg| msg.contains("Note appended to")),
            "{appended:?}"
        );
        let notes = tmpdir.path().join(".deepseek").join("notes.md");
        assert!(notes.exists(), "notes file written under the workspace");
        let content = std::fs::read_to_string(&notes).unwrap();
        assert!(content.contains("hello from dispatch"));

        // Metadata bridges to the TUI localization id.
        let info = registry().get_info("note").expect("note info");
        assert_eq!(
            info.description_id,
            codewhale_localization::MessageId::CmdNoteDescription
        );
    }

    #[test]
    fn feat019_memory_dispatches_through_public_seam() {
        let tmpdir = tempfile::TempDir::new().unwrap();
        let mut app = memory_test_app(&tmpdir);

        let path = execute("/memory path", &mut app);
        assert!(!path.is_error, "{path:?}");
        // The native store root is a directory; memory.md is only the legacy
        // import anchor, no longer the authoritative path.
        assert_eq!(
            path.message.as_deref(),
            Some(tmpdir.path().join("memory").to_str().unwrap())
        );

        // Native status reaches the real adapter through the public seam.
        let status = execute("/memory native status", &mut app);
        assert!(!status.is_error, "{status:?}");
        let msg = status.message.expect("status message");
        assert!(msg.contains("Native memory root:"), "{msg}");

        let info = registry().get_info("memory").expect("memory info");
        assert_eq!(
            info.description_id,
            codewhale_localization::MessageId::CmdMemoryDescription
        );
    }

    #[test]
    fn feat019_public_dispatch_never_panics_on_memory_commands() {
        let tmpdir = tempfile::TempDir::new().unwrap();
        let mut app = memory_test_app(&tmpdir);
        for command in [
            "/note",
            "/note ",
            "/memory",
            "/memory native bogus",
            "/memory wat",
        ] {
            let result = execute(command, &mut app);
            // Every path returns a result; none may panic.
            assert!(result.message.is_some(), "{command}: {result:?}");
        }
    }

    #[test]
    fn feat022_skills_commands_dispatch_through_public_seam() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _home = feat022_scoped_home(&tmp);
        let mut app = feat022_test_app(&tmp);
        std::fs::create_dir_all(tmp.path().join("skills")).unwrap();
        feat022_write_skill(&tmp.path().join("skills"), "demo");

        // Bare /skills opens Extensions; explicit manage retains the mutation surface.
        let result = execute("/skills", &mut app);
        assert!(!result.is_error, "{result:?}");
        assert!(
            matches!(
                result.action,
                Some(crate::tui::app::AppAction::OpenExtensions {
                    tab: crate::tui::views::extensions::ExtensionsTab::Skills
                })
            ),
            "{result:?}"
        );

        assert!(matches!(
            execute("/skills manage", &mut app).action,
            Some(AppAction::OpenSkillsManager)
        ));
        let mcp_info = get_command_info("mcp").expect("registered MCP command");
        assert!(mcp_info.aliases.contains(&"mcps"));
        assert_eq!(get_command_info("mcps").unwrap().name, mcp_info.name);
        for command in ["/mcp", "/mcps"] {
            assert!(
                matches!(
                    execute(command, &mut app).action,
                    Some(AppAction::OpenExtensions {
                        tab: crate::tui::views::extensions::ExtensionsTab::Mcp
                    })
                ),
                "{command}"
            );
        }

        // /skill activates the demo skill and sets active_skill.
        let result = execute("/skill demo", &mut app);
        assert!(!result.is_error, "{result:?}");
        assert!(result.message.unwrap().contains("Skill 'demo' activated."));
        assert!(app.active_skill.is_some());

        // /restore with no snapshots shows the empty message.
        let result = execute("/restore", &mut app);
        assert!(!result.is_error, "{result:?}");
        assert!(result.message.unwrap().contains("No snapshots"));

        // /review without a target prints usage.
        let result = execute("/review", &mut app);
        assert!(result.is_error, "{result:?}");
        assert!(result.message.unwrap().contains("Usage: /review"));
    }

    #[test]
    fn feat022_aliases_dispatch_through_public_seam() {
        // All four aliases (jinengliebiao, jineng, shencha) resolve through the
        // registry to the same portable handlers as the canonical names.
        let tmp = tempfile::TempDir::new().unwrap();
        let _home = feat022_scoped_home(&tmp);
        let mut app = feat022_test_app(&tmp);
        std::fs::create_dir_all(tmp.path().join("skills")).unwrap();
        feat022_write_skill(&tmp.path().join("skills"), "demo");

        let result = execute("/jinengliebiao", &mut app);
        assert!(
            matches!(
                result.action,
                Some(crate::tui::app::AppAction::OpenExtensions {
                    tab: crate::tui::views::extensions::ExtensionsTab::Skills
                })
            ),
            "{result:?}"
        );

        let result = execute("/jineng demo", &mut app);
        assert!(!result.is_error, "{result:?}");
        assert!(result.message.unwrap().contains("Skill 'demo' activated."));

        let result = execute("/shencha", &mut app);
        assert!(result.is_error, "{result:?}");
        assert!(result.message.unwrap().contains("Usage: /review"));
    }

    #[test]
    fn feat022_context_exposure_is_exact_per_d4() {
        // The test-only full envelope exposes every adapter; production
        // dispatch exposes only each handler's declared facets.
        // skills/review/restore consume only skill_group; skill also consumes
        // skills for cache refreshes.
        let tmp = tempfile::TempDir::new().unwrap();
        let _home = feat022_scoped_home(&tmp);
        let mut app = feat022_test_app(&tmp);
        let mut bundle = app.command_contexts();
        let parts = bundle.parts();
        assert!(parts.skill_group.is_some());
        assert!(parts.skills.is_some());
        // Missing-facet safety through the public seam is covered by the
        // handler-level tests; here we assert the envelope carries both.
    }

    // ---------------------------------------------------------------------
    // FEAT-020 plugins group public dispatch (Phase 6)
    // ---------------------------------------------------------------------

    /// App with an isolated temp workspace and a discovered plugin bundle.
    fn plugin_test_app(tmpdir: &tempfile::TempDir) -> App {
        // Write a minimal plugin bundle so the registry discovers real data.
        let bundle = tmpdir.path().join(".codewhale/plugins/demo");
        std::fs::create_dir_all(bundle.join("skills/hello")).unwrap();
        std::fs::write(
            bundle.join("plugin.toml"),
            "schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\ndescription = \"Import spreadsheet data safely\"\n[skills]\npath = \"skills\"\n",
        )
        .unwrap();
        std::fs::write(
            bundle.join("skills/hello/SKILL.md"),
            "---\nname: hello\ndescription: hello\n---\nbody\n",
        )
        .unwrap();
        let options = TuiOptions {
            ..crate::test_support::test_tui_options(tmpdir.path())
        };
        let mut app = App::new(options, &Config::default());
        let discovery = crate::plugins::PluginDiscoveryContext::capture_pre_dotenv();
        app.plugin_registry = discovery.registry_for_workspace(tmpdir.path());
        app
    }

    #[test]
    fn feat020_plugin_entry_is_registered_with_exact_capabilities() {
        let name = "plugin";
        assert!(
            registry().has_contextual_handler(name),
            "/{name} must register through the portable bridge"
        );
        let handler = registry()
            .get(name)
            .expect("entry")
            .contextual_handler()
            .expect("contextual handler");
        let codewhale_command_contract::handler::CommandHandler::Contextual {
            capabilities, ..
        } = handler
        else {
            panic!("/{name} must be contextual");
        };
        let expected = codewhale_command_contract::handler::CommandCapabilities::WORKSPACE
            .union(codewhale_command_contract::handler::CommandCapabilities::PRESENTATION)
            .union(codewhale_command_contract::handler::CommandCapabilities::PLUGIN);
        assert_eq!(capabilities, expected, "/{name} exact capability set");
        // Undeclared facets stay absent.
        assert!(
            !capabilities.contains(codewhale_command_contract::handler::CommandCapabilities::MEDIA)
        );
        assert!(
            !capabilities
                .contains(codewhale_command_contract::handler::CommandCapabilities::MEMORY)
        );
        assert!(
            !capabilities
                .contains(codewhale_command_contract::handler::CommandCapabilities::SKILLS)
        );
        assert!(
            !capabilities
                .contains(codewhale_command_contract::handler::CommandCapabilities::PROJECT)
        );
        assert!(
            !capabilities
                .contains(codewhale_command_contract::handler::CommandCapabilities::SKILL_GROUP)
        );
    }

    #[test]
    fn feat020_plugin_dispatches_through_public_seam() {
        let _home = crate::test_support::SealedHome::new();
        let tmpdir = tempfile::TempDir::new().unwrap();
        let mut app = plugin_test_app(&tmpdir);

        // Bare action opens the extensions view (no panic).
        let bare = execute("/plugin", &mut app);
        assert!(bare.action.is_some(), "{bare:?}");

        // List reaches the real adapter through the public seam.
        let list = execute("/plugin list", &mut app);
        assert!(!list.is_error, "{list:?}");
        let msg = list.message.expect("list message");
        assert!(msg.contains("demo"), "{msg}");

        // Metadata bridges to the TUI localization id.
        let info = registry().get_info("plugin").expect("plugin info");
        assert_eq!(
            info.description_id,
            codewhale_localization::MessageId::CmdPluginDescription
        );
    }

    #[test]
    fn feat020_public_dispatch_never_panics_on_plugin_commands() {
        let _home = crate::test_support::SealedHome::new();
        let tmpdir = tempfile::TempDir::new().unwrap();
        let mut app = plugin_test_app(&tmpdir);
        for command in [
            "/plugin",
            "/plugin ",
            "/plugin list",
            "/plugin show nope",
            "/plugin validate",
            "/plugin tools",
            "/plugin marketplace",
            "/plugin import kimi",
            "/plugin suggest",
        ] {
            let result = execute(command, &mut app);
            // Every path returns a result; none may panic.
            assert!(
                result.message.is_some() || result.action.is_some(),
                "{command}: {result:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // FEAT-023 Phase 6 (Task 6.2): the nine lifecycle registrations dispatch
    // through the public seam with exact capability declarations.
    // -----------------------------------------------------------------------

    #[test]
    fn feat023_lifecycle_entries_register_through_portable_bridge() {
        use codewhale_command_contract::handler::{CommandCapabilities, CommandHandler};

        for name in ["branch", "fork", "load", "new", "save", "sessions", "tree"] {
            assert!(
                registry().has_contextual_handler(name),
                "/{name} must register through the portable bridge"
            );
            let handler = registry()
                .get(name)
                .expect("entry")
                .contextual_handler()
                .expect("contextual handler");
            let CommandHandler::Contextual { capabilities, .. } = handler else {
                panic!("/{name} must be contextual");
            };
            assert_eq!(
                capabilities,
                CommandCapabilities::SESSION_LIFECYCLE,
                "/{name} declares lifecycle authority only"
            );
        }
        // Pure handlers register through the bridge with no host bundle.
        for name in ["compact", "purge"] {
            assert!(
                registry().has_contextual_handler(name),
                "/{name} must register through the portable bridge"
            );
            let handler = registry()
                .get(name)
                .expect("entry")
                .contextual_handler()
                .expect("pure handler");
            assert!(
                matches!(handler, CommandHandler::Pure(_)),
                "/{name} must be pure (no host context bundle)"
            );
        }
        // FEAT-026 completes the final session command adoption.
        assert!(
            registry().has_contextual_handler("structcopy"),
            "/structcopy must use the shared command boundary"
        );
    }

    // ---------------------------------------------------------------------
    // FEAT-024: session control entries register through the portable bridge
    // (D3/D6) — five declare SESSION_CONTROL only; `/remote-env` declares
    // control plus presentation; export/structcopy have independent authority.
    // ---------------------------------------------------------------------

    #[test]
    fn feat024_control_entries_register_through_portable_bridge() {
        use codewhale_command_contract::handler::{CommandCapabilities, CommandHandler};

        for name in ["relay", "rename", "resume", "rc", "title"] {
            assert!(
                registry().has_contextual_handler(name),
                "/{name} must register through the portable bridge"
            );
            let handler = registry()
                .get(name)
                .expect("entry")
                .contextual_handler()
                .expect("contextual handler");
            let CommandHandler::Contextual { capabilities, .. } = handler else {
                panic!("/{name} must be contextual");
            };
            assert_eq!(
                capabilities,
                CommandCapabilities::SESSION_CONTROL,
                "/{name} declares control authority only"
            );
        }
        let handler = registry()
            .get("remote-env")
            .expect("entry")
            .contextual_handler()
            .expect("remote-env handler");
        let CommandHandler::Contextual { capabilities, .. } = handler else {
            panic!("/remote-env must be contextual");
        };
        assert_eq!(
            capabilities,
            CommandCapabilities::SESSION_CONTROL.union(CommandCapabilities::PRESENTATION),
            "/remote-env declares control plus presentation only"
        );
        // FEAT-026 also registers structcopy through its own narrow boundary.
        assert!(
            registry().has_contextual_handler("structcopy"),
            "/structcopy must use the shared command boundary"
        );
    }

    #[test]
    fn feat023_lifecycle_commands_dispatch_through_public_seam() {
        let _home = crate::test_support::SealedHome::new();
        let mut app = create_test_app();
        app.workspace = PathBuf::from(".");

        // Pure handlers need no App machinery.
        let compact = execute("/compact the auth refactor", &mut app);
        assert_eq!(
            compact.message.as_deref(),
            Some("Making room (focus: the auth refactor)…")
        );
        assert!(matches!(
            compact.action,
            Some(AppAction::CompactContext { focus: Some(ref f) }) if f == "the auth refactor"
        ));
        let purge = execute("/purge", &mut app);
        assert_eq!(
            purge.message.as_deref(),
            Some("Agent context purge triggered...")
        );
        assert!(matches!(purge.action, Some(AppAction::PurgeContext)));

        // Contextual handler reaches the adapter through the seam; /tree on a
        // bare app reports no active session.
        let tree = execute("/tree", &mut app);
        assert!(
            tree.message
                .as_deref()
                .unwrap_or_default()
                .contains("No active session"),
            "{tree:?}"
        );

        // Subcommand routing and usage errors stay byte-exact.
        let bad = execute("/sessions teleport", &mut app);
        assert!(
            bad.message
                .as_deref()
                .unwrap_or_default()
                .contains("unknown subcommand `teleport`"),
            "{bad:?}"
        );
        let branch_usage = execute("/branch", &mut app);
        assert!(
            branch_usage
                .message
                .as_deref()
                .unwrap_or_default()
                .starts_with("Usage: /branch <entry_id>"),
            "{branch_usage:?}"
        );
    }

    #[test]
    fn feat024_control_commands_dispatch_through_public_seam() {
        let _home = crate::test_support::SealedHome::new();
        let mut app = create_test_app();
        app.workspace = PathBuf::from(".");

        // /relay composes through the control adapter and emits the bounded
        // SendMessage action; only SESSION_CONTROL is exposed.
        let relay = execute("/relay handoff notes", &mut app);
        assert_eq!(
            relay.message.as_deref(),
            Some("Preparing session relay at .codewhale/handoff.md...")
        );
        let relay_message = match relay.action {
            Some(AppAction::SendMessage(message)) => message,
            other => panic!("expected SendMessage, got {other:?}"),
        };
        assert!(relay_message.contains("Create a compact session relay (接力)"));
        assert!(relay_message.contains("- Requested relay focus: handoff notes"));

        // /rc status reaches the remote-control service through the facet.
        let rc = execute("/rc status", &mut app);
        assert_eq!(rc.message.as_deref(), Some("Remote control: off"));

        // /remote-env bare overview is localized through the presentation
        // facet with the exact source-custody boundary copy.
        let remote_env = execute("/remote-env", &mut app);
        assert!(
            remote_env
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("Hosted Work starts a new environment"),
            "{remote_env:?}"
        );

        // /rename and /title validation boundaries stay exact over the seam.
        let rename = execute("/rename", &mut app);
        assert_eq!(
            rename.message.as_deref(),
            Some("Error: Usage: /rename <new title>")
        );
        let title = execute("/title", &mut app);
        assert!(
            title
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("Window title: [unset]"),
            "{title:?}"
        );

        // Bare /resume opens the picker through the adapter.
        let resume = execute("/resume", &mut app);
        assert!(!resume.is_error);
        assert!(resume.action.is_none());
        assert!(resume.message.is_none());
    }

    // ---------------------------------------------------------------------
    // FEAT-025: session export entry registers through the portable bridge
    // (D1/D3/D5). `/export` (alias `/daochu`) declares exactly SESSION_EXPORT;
    // `/structcopy` remains a direct host handler for FEAT-026, so the root
    // `session` frontier stays pending.
    // ---------------------------------------------------------------------

    #[test]
    fn feat025_export_entry_registers_through_portable_bridge() {
        use codewhale_command_contract::handler::{CommandCapabilities, CommandHandler};

        assert!(
            registry().has_contextual_handler("export"),
            "/export must register through the portable bridge"
        );
        assert!(
            registry().has_contextual_handler("daochu"),
            "/daochu must resolve to the same portable bridge entry"
        );

        let handler = registry()
            .get("export")
            .expect("entry")
            .contextual_handler()
            .expect("contextual handler");
        let CommandHandler::Contextual { capabilities, .. } = handler else {
            panic!("/export must be contextual");
        };
        assert_eq!(
            capabilities,
            CommandCapabilities::SESSION_EXPORT,
            "/export declares export authority only"
        );

        // Least authority is catalogue-wide: no other registration may declare
        // the session-export capability.
        let export_declarers: Vec<&str> = registry()
            .iter()
            .filter(|command| {
                command
                    .contextual_handler()
                    .is_some_and(|handler| match handler {
                        CommandHandler::Contextual { capabilities, .. } => {
                            capabilities.contains(CommandCapabilities::SESSION_EXPORT)
                        }
                        CommandHandler::Pure(_) => false,
                    })
            })
            .map(|command| command.info().name)
            .collect();
        // `/share` publishes the same redacted projection `/export` renders,
        // so it holds the same authority and nothing more.
        let mut export_declarers = export_declarers;
        export_declarers.sort_unstable();
        assert_eq!(
            export_declarers,
            vec!["export", "share"],
            "only /export and /share may declare SESSION_EXPORT"
        );

        // Export has no direct host fallback. Structcopy also uses the
        // contract route after FEAT-026, with its own independent authority.
        let mut app = create_test_app();
        let legacy = registry()
            .get("export")
            .expect("entry")
            .execute(&mut app, None);
        assert_eq!(
            legacy.message.as_deref(),
            Some("Error: command has no executable handler"),
            "/export must not keep a legacy function registration"
        );
        assert!(
            registry().has_contextual_handler("structcopy"),
            "/structcopy must use the shared command boundary"
        );
    }

    #[test]
    fn feat025_export_registered_handler_fails_safely_without_authority() {
        // The dispatcher builds the envelope from the declared capabilities and
        // calls this exact handler object. A narrower envelope that omits the
        // export facet must return the safe error before parsing or performing
        // any projection, clipboard, recovery, resolution, or write operation.
        let handler = registry()
            .get("export")
            .expect("entry")
            .contextual_handler()
            .expect("contextual handler");
        let codewhale_command_contract::handler::CommandHandler::Contextual {
            handler: contextual,
            ..
        } = handler
        else {
            panic!("/export must be contextual");
        };

        for arg in [None, Some("clipboard"), Some("file out.md")] {
            let result = contextual(
                codewhale_command_contract::handler::CommandContexts::empty(),
                arg,
            );
            assert!(result.is_error, "{arg:?} must fail without authority");
            assert_eq!(
                result.message.as_deref(),
                Some("Error: Command capability unavailable: session_export"),
                "{arg:?} must keep the exact safe error"
            );
            assert!(result.action.is_none(), "{arg:?} must produce no action");
        }
    }
}

#[cfg(test)]
mod config_policy_host_tests;
#[cfg(test)]
mod config_policy_permissions_tests;
#[cfg(test)]
mod config_policy_status_tests;
