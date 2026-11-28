#[cfg(test)]
mod tests {
    use crate::commands::fleet_host::{fleet_catalog_rejection, fleet_provider_rejection};
    use crate::tui::app::{App, AppAction, TuiOptions};
    use codewhale_lane::control::operations_for_domain;
    use codewhale_lane::{ControlDomain, ControlSurface};
    use codewhale_localization::Locale;
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
        let added =
            crate::commands::execute("/fleet add openrouter z-ai/glm-5.3-flash explore", &mut app);
        assert_eq!(
            added.action,
            Some(AppAction::FleetAddModel {
                provider: "openrouter".to_string(),
                model: "z-ai/glm-5.3-flash".to_string(),
                roles: vec!["explore".to_string()],
            })
        );
        assert!(!added.is_error && added.message.is_none(), "{added:?}");

        let removed =
            crate::commands::execute("/fleet remove openrouter z-ai/glm-5.3-flash", &mut app);
        assert_eq!(
            removed.action,
            Some(AppAction::FleetRemoveModel {
                provider: "openrouter".to_string(),
                model: "z-ai/glm-5.3-flash".to_string(),
            })
        );

        for arg in ["add openrouter", "add", "remove openrouter", "remove"] {
            let usage = crate::commands::execute(&format!("/fleet {arg}"), &mut app);
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

        let empty = crate::commands::execute("/fleet models", &mut app);
        assert!(
            empty
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("session model only"),
            "got: {empty:?}"
        );

        codewhale_engine::fleet::members::add_fleet_model(
            &workspace,
            "openrouter",
            "z-ai/glm-5.3-flash",
            &["explore".to_string()],
        )
        .expect("add");
        let listed = crate::commands::execute("/fleet models", &mut app)
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

        let selected = codewhale_engine::fleet::store::resolve_selected_fleet(&workspace)
            .expect("ok")
            .expect("selected");
        std::fs::remove_file(&selected.path).expect("remove");
        let broken = crate::commands::execute("/fleet models", &mut app)
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

        let result = crate::commands::execute("/fleet", &mut app);

        assert_eq!(result.action, Some(AppAction::OpenFleetRoster));
        assert!(result.message.is_none());
    }

    #[test]
    fn fleet_saved_fleet_verbs_open_the_named_fleet_list() {
        for arg in ["teams", "team", "fleets", "saved", "manage"] {
            let mut app = test_app();

            let result = crate::commands::execute(&format!("/fleet {arg}"), &mut app);

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
        let retired_verb = crate::commands::execute("/fleet pods", &mut app);
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

            let result = crate::commands::execute(&format!("/fleet {arg}"), &mut app);

            assert_eq!(result.action, Some(AppAction::OpenFleetRoster), "{arg}");
            assert!(result.message.is_none(), "{arg}");
        }
    }

    #[test]
    fn fleet_setup_args_open_setup_wizard() {
        for arg in ["setup", "edit", "new"] {
            let mut app = test_app();

            let result = crate::commands::execute(&format!("/fleet {arg}"), &mut app);

            assert_eq!(result.action, Some(AppAction::OpenFleetSetup), "{arg}");
            assert!(result.message.is_none(), "{arg}");
        }
    }

    #[test]
    fn fleet_workers_arg_opens_the_session_subagent_view() {
        for arg in ["workers", "worker", "agents", "subagents"] {
            let mut app = test_app();

            let result = crate::commands::execute(&format!("/fleet {arg}"), &mut app);

            assert_eq!(result.action, Some(AppAction::ListSubAgents), "{arg}");
            assert!(result.message.is_none(), "{arg}");
        }
    }

    #[test]
    fn fleet_status_reads_the_durable_ledger_not_session_subagents() {
        let workspace = tempfile::tempdir().unwrap();
        let mut app = app_in(workspace.path().to_path_buf());

        let result = crate::commands::execute("/fleet status", &mut app);

        assert_eq!(
            result.action, None,
            "/fleet status must not open the session sub-agent view"
        );
        let message = result.message.as_deref().unwrap_or_default();
        assert!(message.contains("fleet.status"), "got: {message}");
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
            let result = crate::commands::execute(&format!("/fleet {arg}"), &mut app);
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

        let result = crate::commands::execute("/fleet help", &mut app);

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

        let result = crate::commands::execute("/fleet bogus", &mut app);

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
        let info = crate::commands::get_command_info("fleet").expect("fleet info");
        assert_eq!(info.name, "fleet");
        assert!(info.aliases.is_empty());
        assert!(info.usage.contains("teams"));
        assert!(info.usage.contains("workers"));
        for advanced in ["save-as", "interrupt", "resume", "models", "status"] {
            assert!(
                !info.usage.contains(advanced),
                "usage must not advertise {advanced}"
            );
        }
        assert!(!info.usage.contains("pods"));
    }

    #[test]
    fn fleet_dispatches_and_retired_pod_does_not() {
        let mut app = test_app();
        let result = crate::commands::execute("/fleet", &mut app);
        assert_eq!(result.action, Some(AppAction::OpenFleetRoster));
        assert!(!result.is_error);

        assert!(crate::commands::get_command_info("pod").is_none());
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
        let info = crate::commands::get_command_info("fleet").expect("fleet info");
        let mut app = test_app();
        let help = crate::commands::execute("/fleet help", &mut app)
            .message
            .unwrap_or_default();
        for descriptor in operations_for_domain(ControlDomain::Fleet) {
            assert_eq!(descriptor.slash_command, info.name);
            assert_eq!(descriptor.hotbar_action_id(), "slash.fleet");
            assert!(
                help.contains(&format!("/fleet {}", descriptor.verb)),
                "/fleet help must document {} now that usage does not",
                descriptor.verb
            );
            assert!(descriptor.offers(ControlSurface::Cli));
        }
        assert!(
            !info.requires_required_argument(),
            "/fleet must stay directly runnable from the palette and hotbar"
        );
    }

    #[test]
    fn fleet_usage_presents_a_prioritized_core_and_help_keeps_the_rest_reachable() {
        let info = crate::commands::get_command_info("fleet").expect("fleet info");
        let verbs: Vec<&str> = info
            .usage
            .trim_start_matches("/fleet [")
            .trim_end_matches(']')
            .split('|')
            .collect();
        assert_eq!(
            verbs,
            ["members", "setup", "teams", "workers", "help"],
            "the usage line is exactly the primary set"
        );

        let mut app = test_app();
        let help = crate::commands::execute("/fleet help", &mut app)
            .message
            .unwrap_or_default();
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
