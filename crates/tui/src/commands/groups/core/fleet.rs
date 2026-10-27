//! `/fleet` command — the agent fleet behind the session.
//!
//! Fleet = who is working right now. Bare `/fleet` (and `/fleet roster`)
//! opens the familiar roster surface for the selected team; `/fleet setup`
//! opens the authoring wizard. `/fleet teams` (other aliases: `fleets`,
//! `saved`, `manage`) opens the named-team picker for switching between
//! saved configurations — never the primary face.
//! `/fleet list|status|interrupt|resume` are control-plane verbs that run
//! against the **durable** workspace ledger through the shared contract in
//! `codewhale-lane`, exactly as `codewhale fleet …` does (#1888, #4022).
//! #5888: the usage line presents only the prioritized core verbs — open the
//! team, author it, switch saved teams, watch session workers, ask for help.
//! Model-route verbs, saved-route verbs, and the durable control plane stay
//! dispatchable and live one level deeper, documented in `/fleet help`,
//! instead of all fourteen competing in one usage string.
//!
//! `/fleet status` used to show the current TUI session's sub-agents. That was
//! a different thing wearing the same name: session sub-agents are not the
//! durable fleet ledger, and a run started by `codewhale fleet run` never
//! appeared. The session view is still reachable as `/fleet workers` (and
//! `/subagents`), now labelled as what it is.

use codewhale_lane::control::operations_for_domain;
use codewhale_lane::{ControlDomain, ControlOperation, ControlSurface};

use crate::commands::traits::{CommandInfo, RegisterCommand};
use crate::fleet::control::execute_fleet_control;
use crate::tui::app::{App, AppAction};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_runtime::config::Config;

use super::CommandResult;

/// The verbs `/fleet` advertises up front (#5888): open the team, author it,
/// switch saved teams, watch current-session workers, ask for the rest.
/// Every other verb stays dispatchable; it is documented one level deeper in
/// `/fleet help` instead of competing for attention in the usage line.
const PRIMARY_VERBS: [&str; 5] = ["members", "setup", "teams", "workers", "help"];

const PRIMARY_USAGE: &str = "/fleet [members|setup|teams|workers|help]";

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "fleet",
    aliases: &[],
    usage: PRIMARY_USAGE,
    description_id: MessageId::CmdFleetDescription,
};

pub(in crate::commands) struct FleetCmd;

fn help_text() -> String {
    let mut out = format!(
        "Usage: {PRIMARY_USAGE}\n\n\
         The fleet is who is working right now. The primary verbs cover the daily loop:\n\
         /fleet (or /fleet members) opens the roster — each member's role, model, and access; \
         Enter on a member row opens that member's editor. /fleet setup opens the authoring \
         wizard. /fleet teams (or fleets/saved/manage) switches between named saved teams. \
         /fleet workers (and /subagents) shows sub-agents in the current TUI session only, \
         which is a different set: it does not include durable fleet runs.\n\n\
         Advanced — team model routes (still typed, one level deeper):\n\
         /fleet models lists the selected team's models. \
         /fleet add <provider> <model> [role…] and /fleet remove <provider> <model> edit them.\n\n\
         Advanced — saved routes:\n\
         /fleet save persists the current session route into the selected team's operator. \
         /fleet save-as saves it as a new team.\n\n\
         Durable runs — these act on the durable .codewhale/fleet.jsonl ledger for this \
         workspace, the same records `codewhale fleet` reads and writes. the ledger file, \
         saved rosters, and config tables keep the Fleet name:\n",
    );
    for descriptor in operations_for_domain(ControlDomain::Fleet) {
        out.push_str(&format!(
            "\n  {:<30} {:<6} {}\n      CLI: {}\n",
            descriptor.slash_invocation(),
            descriptor.authority.as_str(),
            descriptor.summary,
            descriptor.cli_invocation
        ));
    }
    out
}

/// Split `"<verb> <rest>"` into the verb and its raw target tail.
fn split_verb(arg: Option<&str>) -> Option<(&str, Option<&str>)> {
    let rest = arg.map(str::trim).filter(|value| !value.is_empty())?;
    Some(match rest.split_once(char::is_whitespace) {
        Some((verb, tail)) => (verb, Some(tail.trim())),
        None => (rest, None),
    })
}

fn fleet_models_text(app: &App) -> String {
    use crate::fleet::members::fleet_models;
    let locale = app.ui_locale;
    let models = match fleet_models(&app.workspace) {
        Ok(models) if models.is_empty() => {
            return tr(locale, MessageId::FleetModelsEmpty).into_owned();
        }
        Ok(models) => models,
        // A selected fleet that cannot be read is named, never shown as
        // "the session model only".
        Err(error) => {
            return tr(locale, MessageId::FleetModelsBroken).replace("{error}", &error.to_string());
        }
    };
    let mut lines = vec![
        tr(locale, MessageId::FleetModelsHeader)
            .replace("{fleet}", &models[0].fleet)
            .replace("{count}", &models.len().to_string()),
    ];
    for member in &models {
        let provider = codewhale_config::ProviderKind::parse(&member.provider);
        let facts = provider
            .and_then(|p| crate::provider_lake::catalog_offering_for_model(p, &member.model))
            .map(|row| {
                let mut parts = Vec::new();
                if let Some(cost) = row.cost.as_ref()
                    && let (Some(input), Some(output)) = (cost.input, cost.output)
                {
                    parts.push(
                        tr(locale, MessageId::FleetModelsFactPrice)
                            .replace("{input}", &format!("{input:.2}"))
                            .replace("{output}", &format!("{output:.2}")),
                    );
                }
                if let Some(limit) = row.limit.as_ref()
                    && let Some(context) = limit.context
                {
                    parts.push(
                        tr(locale, MessageId::FleetModelsFactContext)
                            .replace("{context}", &(context / 1000).to_string()),
                    );
                }
                if row.tool_call == Some(true) {
                    parts.push(tr(locale, MessageId::FleetModelsFactTools).into_owned());
                }
                parts.join(" · ")
            })
            .filter(|facts| !facts.is_empty())
            .map(|facts| format!(" · {facts}"))
            .unwrap_or_default();
        lines.push(format!(
            "  {}/{} · {}{facts}",
            member.provider,
            member.model,
            member.roles_label()
        ));
    }
    lines.push(tr(locale, MessageId::FleetModelsFooter).into_owned());
    lines.join("\n")
}

/// Whether `provider_id` names a provider the user has configured — active
/// route, explicit `[providers.<id>]` table, or usable credentials — in the
/// **live** `config`, the same source the `/provider` and `/model` pickers
/// consult. A startup snapshot goes stale after an in-session provider change.
fn provider_id_is_configured(app: &App, config: &Config, provider_id: &str) -> bool {
    let Some(active) = app
        .provider_identity
        .as_ref()
        .filter(|identity| config.verify_provider_identity(identity).is_ok())
    else {
        return false;
    };
    let Ok(identity) = config.resolve_provider_selection_identity(provider_id) else {
        return false;
    };
    crate::provider_readiness::provider_is_configured_for_active(config, &identity, active)
}

/// The localized reason `provider` may not enter the team, or `None` when
/// it is configured. Shared by `/fleet add` and the picker's ⇧F so a locked
/// or unauthenticated provider row is refused on both surfaces alike.
#[must_use]
pub(crate) fn fleet_provider_rejection(
    app: &App,
    config: &Config,
    provider: &str,
) -> Option<String> {
    (!provider_id_is_configured(app, config, provider)).then(|| {
        tr(app.ui_locale, MessageId::FleetAddProviderUnconfigured).replace("{provider}", provider)
    })
}

/// The localized reason a typed `/fleet add` route is refused when a known
/// provider's catalog is non-empty and does not list `model`. The picker
/// skips this check: its rows already come from the catalog or a live list.
#[must_use]
pub(crate) fn fleet_catalog_rejection(
    locale: Locale,
    provider: &str,
    model: &str,
) -> Option<String> {
    let known = codewhale_config::ProviderKind::parse(provider)?;
    let served = crate::provider_lake::all_catalog_models_for_provider(known);
    (!served.is_empty() && !served.iter().any(|id| id.eq_ignore_ascii_case(model))).then(|| {
        tr(locale, MessageId::FleetAddModelNotServed)
            .replace("{provider}", provider)
            .replace("{model}", model)
    })
}

/// `/fleet add <provider> <model> [role…]`: parse here; the write happens in
/// the UI's `AppAction::FleetAddModel` arm, which holds the live config for
/// provider validation and the engine handle for the roster refresh.
fn fleet_add(app: &App, target: Option<&str>) -> CommandResult {
    let mut words = target.unwrap_or_default().split_whitespace();
    let (Some(provider), Some(model)) = (words.next(), words.next()) else {
        return CommandResult::error(tr(app.ui_locale, MessageId::FleetAddUsage));
    };
    CommandResult::action(AppAction::FleetAddModel {
        provider: provider.to_string(),
        model: model.to_string(),
        roles: words.map(str::to_string).collect(),
    })
}

fn fleet_remove(app: &App, target: Option<&str>) -> CommandResult {
    let mut words = target.unwrap_or_default().split_whitespace();
    let (Some(provider), Some(model)) = (words.next(), words.next()) else {
        return CommandResult::error(tr(app.ui_locale, MessageId::FleetRemoveUsage));
    };
    CommandResult::action(AppAction::FleetRemoveModel {
        provider: provider.to_string(),
        model: model.to_string(),
    })
}

fn run_control(app: &App, operation: ControlOperation, target: Option<&str>) -> CommandResult {
    let receipt = execute_fleet_control(ControlSurface::Slash, &app.workspace, operation, target);
    let rendered = receipt.render();
    if receipt.is_error() {
        CommandResult::error(rendered)
    } else {
        CommandResult::message(rendered)
    }
}

impl RegisterCommand for FleetCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn execute(app: &mut App, arg: Option<&str>) -> CommandResult {
        let Some((verb, target)) = split_verb(arg) else {
            // Primary face: the familiar roster for the selected team.
            // Named-team switching lives under /fleet teams — never between
            // the operator and their team.
            return CommandResult::action(AppAction::OpenFleetRoster);
        };
        match verb {
            "save" | "update" => {
                // Explicit persistence of the pending session route into the
                // selected fleet's operator. Only an explicit command can
                // write a saved fleet after an in-session route change.
                let message = app.apply_route_save_choice(
                    crate::tui::views::route_save_prompt::RouteSaveChoice::UpdateFleet,
                );
                return CommandResult::message(message);
            }
            "save-as" | "saveas" => {
                let message = app.apply_route_save_choice(
                    crate::tui::views::route_save_prompt::RouteSaveChoice::SaveAsNewFleet,
                );
                return CommandResult::message(message);
            }
            _ => {}
        }
        match verb {
            // The team as models (design §10 F1): what the person added,
            // provider-exact, with the roles each model fills.
            "models" | "model" => CommandResult::message(fleet_models_text(app)),
            "add" => fleet_add(app, target),
            "remove" | "rm" | "drop" => fleet_remove(app, target),
            "members" | "member" | "roster" | "roles" | "role" | "profiles" | "profile" => {
                CommandResult::action(AppAction::OpenFleetRoster)
            }
            "setup" | "edit" | "new" => CommandResult::action(AppAction::OpenFleetSetup),
            // Named saved teams — secondary surface for multi-team pick/switch.
            // Deliberately not "list": that verb is the durable ledger (#4022).
            // "fleets" stays as a compatibility spelling for the wire name.
            "teams" | "team" | "fleets" | "saved" | "manage" => {
                CommandResult::action(AppAction::OpenFleetList)
            }
            // The current-session sub-agent projection, named for what it is.
            "workers" | "worker" | "agents" | "subagents" => super::core::subagents(app),
            "help" | "?" => CommandResult::message(help_text()),
            other => match ControlOperation::parse_verb(ControlDomain::Fleet, other) {
                Some(operation) => run_control(app, operation, target),
                None => CommandResult::error(format!(
                    "Unknown /fleet target '{other}'. Use {} — or /fleet help for the full \
                     verb list.",
                    PRIMARY_VERBS[..PRIMARY_VERBS.len() - 1].join(", ")
                )),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiOptions;
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    fn test_app() -> App {
        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        App::new(options, &Config::default())
    }

    fn app_in(workspace: PathBuf) -> App {
        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(workspace.clone())
        };
        let mut app = App::new(options, &Config::default());
        app.workspace = workspace;
        app
    }

    fn isolated_workspace() -> (tempfile::TempDir, crate::test_support::EnvVarGuard, PathBuf) {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let guard = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.as_os_str());
        let workspace = temp.path().join("repo");
        std::fs::create_dir_all(&workspace).expect("workspace");
        (temp, guard, workspace)
    }

    #[test]
    fn fleet_add_and_remove_parse_into_actions_the_ui_applies_with_the_live_config() {
        let mut app = test_app();
        let added = FleetCmd::execute(&mut app, Some("add openrouter z-ai/glm-5.3-flash explore"));
        assert_eq!(
            added.action,
            Some(AppAction::FleetAddModel {
                provider: "openrouter".to_string(),
                model: "z-ai/glm-5.3-flash".to_string(),
                roles: vec!["explore".to_string()],
            })
        );
        assert!(!added.is_error && added.message.is_none(), "{added:?}");

        let removed = FleetCmd::execute(&mut app, Some("remove openrouter z-ai/glm-5.3-flash"));
        assert_eq!(
            removed.action,
            Some(AppAction::FleetRemoveModel {
                provider: "openrouter".to_string(),
                model: "z-ai/glm-5.3-flash".to_string(),
            })
        );

        for arg in ["add openrouter", "add", "remove openrouter", "remove"] {
            let usage = FleetCmd::execute(&mut app, Some(arg));
            assert!(usage.is_error && usage.action.is_none(), "{arg}: {usage:?}");
            assert!(
                usage
                    .message
                    .as_deref()
                    .unwrap_or_default()
                    .contains("Usage: /fleet"),
                "{arg}: {usage:?}"
            );
        }
    }

    #[test]
    fn fleet_models_lists_the_selected_fleet_and_names_a_broken_selection() {
        let _lock = crate::test_support::lock_test_env();
        let (_temp, _home, workspace) = isolated_workspace();
        let mut app = app_in(workspace.clone());

        let empty = FleetCmd::execute(&mut app, Some("models"));
        assert!(
            empty
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("session model only"),
            "got: {empty:?}"
        );

        crate::fleet::members::add_fleet_model(
            &workspace,
            "openrouter",
            "z-ai/glm-5.3-flash",
            &["explore".to_string()],
        )
        .expect("add");
        let listed = FleetCmd::execute(&mut app, Some("models"))
            .message
            .unwrap_or_default();
        assert!(
            listed.contains("openrouter/z-ai/glm-5.3-flash · explore"),
            "got: {listed}"
        );
        assert!(
            listed.starts_with("Your team `My fleet` (1 models)"),
            "got: {listed}"
        );

        // A selected team whose file is gone is a broken selection, and
        // `/fleet models` says so instead of "session model only".
        let selected = crate::fleet::store::resolve_selected_fleet(&workspace)
            .expect("ok")
            .expect("selected");
        std::fs::remove_file(&selected.path).expect("remove");
        let broken = FleetCmd::execute(&mut app, Some("models"))
            .message
            .unwrap_or_default();
        assert!(broken.contains("could not be loaded"), "got: {broken}");
        assert!(!broken.contains("session model only"), "got: {broken}");
    }

    #[test]
    fn fleet_add_rejections_come_from_the_live_config_and_the_catalog() {
        let app = test_app();
        let mut live = Config::default();
        assert!(
            fleet_provider_rejection(&app, &live, "unknown-provider")
                .is_some_and(|reason| reason.contains("not a configured provider"))
        );
        assert!(fleet_provider_rejection(&app, &live, "openrouter").is_some());
        // An in-session credential change is visible without restarting.
        live.provider_config_for_mut(
            &live.test_identity_for_kind(codewhale_config::ProviderKind::Openrouter),
        )
        .unwrap()
        .api_key = Some("test-key".to_string());
        assert_eq!(fleet_provider_rejection(&app, &live, "openrouter"), None);

        assert!(
            fleet_catalog_rejection(Locale::En, "anthropic", "not-a-real-model")
                .is_some_and(|reason| reason.contains("does not serve"))
        );
        assert_eq!(
            fleet_catalog_rejection(Locale::En, "some-custom-provider", "anything"),
            None
        );
    }

    #[test]
    fn fleet_command_opens_roster_view() {
        let mut app = test_app();

        let result = FleetCmd::execute(&mut app, None);

        assert_eq!(result.action, Some(AppAction::OpenFleetRoster));
        assert!(result.message.is_none());
    }

    #[test]
    fn fleet_saved_fleet_verbs_open_the_named_fleet_list() {
        for arg in ["teams", "team", "fleets", "saved", "manage"] {
            let mut app = test_app();

            let result = FleetCmd::execute(&mut app, Some(arg));

            assert_eq!(result.action, Some(AppAction::OpenFleetList), "{arg}");
            assert!(result.message.is_none(), "{arg}");
        }
    }

    #[test]
    fn retired_pod_invocations_are_rejected() {
        let mut app = test_app();
        let rejected = crate::commands::execute("/pod", &mut app);
        assert!(
            rejected.is_error,
            "/pod must not dispatch, got: {rejected:?}"
        );
        assert!(
            rejected
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("Unknown command: /pod"),
            "got: {rejected:?}"
        );

        let mut app = test_app();
        let retired_verb = FleetCmd::execute(&mut app, Some("pods"));
        assert!(retired_verb.is_error);
        assert!(
            retired_verb
                .message
                .as_deref()
                .is_some_and(|message| message.contains("Unknown /fleet target 'pods'")),
            "got: {retired_verb:?}"
        );
    }

    #[test]
    fn fleet_members_and_roster_aliases_open_roster_view() {
        for arg in [
            "members", "member", "roster", "roles", "role", "profiles", "profile",
        ] {
            let mut app = test_app();

            let result = FleetCmd::execute(&mut app, Some(arg));

            assert_eq!(result.action, Some(AppAction::OpenFleetRoster), "{arg}");
            assert!(result.message.is_none(), "{arg}");
        }
    }

    #[test]
    fn fleet_setup_args_open_setup_wizard() {
        for arg in ["setup", "edit", "new"] {
            let mut app = test_app();

            let result = FleetCmd::execute(&mut app, Some(arg));

            assert_eq!(result.action, Some(AppAction::OpenFleetSetup), "{arg}");
            assert!(result.message.is_none(), "{arg}");
        }
    }

    /// #4022: the session sub-agent projection keeps its own name. It is no
    /// longer allowed to answer for the durable worker ledger.
    #[test]
    fn fleet_workers_arg_opens_the_session_subagent_view() {
        for arg in ["workers", "worker", "agents", "subagents"] {
            let mut app = test_app();

            let result = FleetCmd::execute(&mut app, Some(arg));

            assert_eq!(result.action, Some(AppAction::ListSubAgents), "{arg}");
            assert!(result.message.is_none(), "{arg}");
        }
    }

    /// #4022: `/fleet status` must read the durable ledger, not substitute the
    /// current session's sub-agents for it.
    #[test]
    fn fleet_status_reads_the_durable_ledger_not_session_subagents() {
        let workspace = tempfile::tempdir().unwrap();
        let mut app = app_in(workspace.path().to_path_buf());

        let result = FleetCmd::execute(&mut app, Some("status"));

        assert_eq!(
            result.action, None,
            "/fleet status must not open the session sub-agent view"
        );
        let message = result.message.as_deref().unwrap_or_default();
        assert!(message.contains("fleet.status"), "got: {message}");
        // This workspace has no ledger, so the truthful answer is a typed
        // unavailability — never an empty-looking "all clear".
        assert!(message.contains("no_fleet_ledger"), "got: {message}");
        assert!(
            !workspace
                .path()
                .join(".codewhale")
                .join("fleet.jsonl")
                .exists(),
            "a read verb must not create the durable ledger"
        );
    }

    #[test]
    fn fleet_control_verbs_route_through_the_shared_contract() {
        let workspace = tempfile::tempdir().unwrap();
        for (arg, expected_id) in [
            ("list", "fleet.list"),
            ("status", "fleet.status"),
            ("interrupt worker-1", "fleet.interrupt"),
            ("resume run-1", "fleet.resume"),
            ("restart worker-1", "fleet.restart"),
        ] {
            let mut app = app_in(workspace.path().to_path_buf());
            let result = FleetCmd::execute(&mut app, Some(arg));
            let message = result.message.as_deref().unwrap_or_default();
            assert!(
                message.contains(expected_id),
                "/fleet {arg} must report {expected_id}, got: {message}"
            );
            assert_eq!(result.action, None, "/fleet {arg}");
        }
    }

    #[test]
    fn fleet_help_arg_distinguishes_durable_from_session_state() {
        let mut app = test_app();

        let result = FleetCmd::execute(&mut app, Some("help"));

        assert!(!result.is_error);
        assert!(result.action.is_none());
        let message = result.message.as_deref().unwrap_or_default();
        for surface in [
            "/fleet members",
            "/fleet setup",
            "/fleet teams",
            "/fleet status",
        ] {
            assert!(message.contains(surface), "help must describe {surface}");
        }
        assert!(
            !message.contains("compatibility alias"),
            "no retired alias may be documented: {message}"
        );
        assert!(
            !message.contains("codewhale pod"),
            "no retired CLI spelling may be documented: {message}"
        );
        for truth in [
            "current TUI session",
            "codewhale fleet status",
            ".codewhale/fleet.jsonl",
        ] {
            assert!(message.contains(truth), "help must distinguish {truth}");
        }
        for descriptor in operations_for_domain(ControlDomain::Fleet) {
            assert!(
                message.contains(descriptor.cli_invocation),
                "help must name the CLI twin of {}",
                descriptor.id
            );
        }
    }

    #[test]
    fn fleet_unknown_arg_reports_error() {
        let mut app = test_app();

        let result = FleetCmd::execute(&mut app, Some("bogus"));

        assert!(result.is_error);
        assert!(result.action.is_none());
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|message| message.contains("Unknown /fleet target 'bogus'"))
        );
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|message| message.contains("Use members, setup, teams"))
        );
    }

    #[test]
    fn fleet_aliases_are_registered_on_command_info() {
        assert_eq!(FleetCmd::info().name, "fleet");
        assert!(FleetCmd::info().aliases.is_empty());
        assert!(FleetCmd::info().usage.contains("teams"));
        assert!(FleetCmd::info().usage.contains("workers"));
        // #5888: the usage line is the prioritized core; advanced verbs stay
        // dispatchable but are documented in /fleet help, not here.
        for advanced in ["save-as", "interrupt", "resume", "models", "status"] {
            assert!(
                !FleetCmd::info().usage.contains(advanced),
                "usage must not advertise {advanced}"
            );
        }
        assert!(!FleetCmd::info().usage.contains("pods"));
    }

    #[test]
    fn fleet_dispatches_and_retired_pod_does_not() {
        let mut app = test_app();
        let result = crate::commands::execute("/fleet", &mut app);
        assert_eq!(result.action, Some(AppAction::OpenFleetRoster));
        assert!(!result.is_error);

        assert!(crate::commands::get_command_info("pod").is_none());
        // Model B (2026-09-03): the live collective is the fleet; there is
        // no separate `/team` command.
        assert!(crate::commands::get_command_info("team").is_none());

        let workspace = tempfile::tempdir().expect("workspace");
        let mut fleet_app = app_in(workspace.path().to_path_buf());
        let mut retired_app = app_in(workspace.path().to_path_buf());
        let fleet_status = crate::commands::execute("/fleet status", &mut fleet_app);
        let retired_status = crate::commands::execute("/pod status", &mut retired_app);
        assert!(retired_status.is_error);
        assert_ne!(fleet_status.message, retired_status.message);
    }

    #[test]
    fn slash_command_and_cli_agree_on_fleet_verb_ids() {
        for descriptor in operations_for_domain(ControlDomain::Fleet) {
            assert_eq!(descriptor.slash_command, COMMAND_INFO.name);
            assert_eq!(descriptor.hotbar_action_id(), "slash.fleet");
            // #5888: usage presents only the prioritized core; every durable
            // control verb is documented one level deeper in /fleet help.
            assert!(
                help_text().contains(&format!("/fleet {}", descriptor.verb)),
                "/fleet help must document {} now that usage does not",
                descriptor.verb
            );
            assert!(descriptor.offers(ControlSurface::Cli));
        }
        assert!(
            !COMMAND_INFO.requires_required_argument(),
            "/fleet must stay directly runnable from the palette and hotbar"
        );
    }

    /// #5888: the primary surface is a small, prioritized set, and every
    /// advanced verb remains named by `/fleet help` — nothing is
    /// unreachable, it just lives one level deeper.
    #[test]
    fn fleet_usage_presents_a_prioritized_core_and_help_keeps_the_rest_reachable() {
        let verbs: Vec<&str> = FleetCmd::info()
            .usage
            .trim_start_matches("/fleet [")
            .trim_end_matches(']')
            .split('|')
            .collect();
        assert_eq!(
            verbs, PRIMARY_VERBS,
            "the usage line is exactly the primary set"
        );

        let help = help_text();
        for advanced in [
            "/fleet models",
            "/fleet add",
            "/fleet remove",
            "/fleet save",
            "/fleet save-as",
        ] {
            assert!(help.contains(advanced), "help must name {advanced}");
        }
        for descriptor in operations_for_domain(ControlDomain::Fleet) {
            assert!(
                help.contains(&format!("/fleet {}", descriptor.verb)),
                "help must keep {} reachable",
                descriptor.verb
            );
        }
    }
}
