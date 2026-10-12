//! Public registry and optional-observation regressions for the policy slice.
use crate::commands::traits::CommandGroup;
use codewhale_command_contract::handler::{CommandCapabilities as Caps, CommandHandler};

#[test]
fn config_policy_keeps_host_registry_order_metadata_and_exact_capabilities() {
    let group = crate::commands::groups::config::ConfigCommands;
    assert_eq!(
        group
            .commands()
            .iter()
            .map(|command| command.info().name)
            .collect::<Vec<_>>(),
        [
            "config",
            "import-claude",
            "permissions",
            "login",
            "connect",
            "disconnect",
            "auth",
            "workbar",
            "pet",
            "settings",
            "status",
            "statusline",
            "mode",
            "fullscreen",
            "inline",
            "theme",
            "verbose",
            "trust",
            "logout"
        ]
    );
    for (info, handler) in crate::commands::groups::config::policy::portable_handlers() {
        let CommandHandler::Contextual {
            capabilities: expected,
            ..
        } = handler
        else {
            panic!("portable contextual handler")
        };
        assert_eq!(
            expected,
            if info.name == "permissions" {
                Caps::PERMISSIONS | Caps::PRESENTATION
            } else {
                Caps::CONFIG_STATUS | Caps::PRESENTATION
            }
        );
        for name in std::iter::once(info.name).chain(info.aliases.iter().copied()) {
            let registered = crate::commands::registry().get(name).unwrap();
            assert_eq!(registered.info().name, info.name);
            assert_eq!(registered.info().aliases, info.aliases);
            assert_eq!(registered.info().usage, info.usage);
            assert_eq!(
                Some(registered.info().description_id),
                crate::commands::contract::key_to_message_id(info.description_key)
            );
            let CommandHandler::Contextual { capabilities, .. } =
                registered.contextual_handler().unwrap()
            else {
                panic!("host contextual handler")
            };
            assert_eq!(capabilities, expected);
        }
    }
}

#[test]
fn config_policy_status_survives_unreadable_optional_config_without_rewriting_it() {
    let _env = crate::test_support::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let mut app = crate::test_support::test_app_with_options(
        crate::test_support::test_tui_options(temp.path()),
    );
    app.ui_locale = codewhale_localization::Locale::En;
    let path = temp.path().join("config.toml");
    let original = "provider = [malformed optional config with {braces}\n";
    std::fs::write(&path, original).unwrap();
    app.config_path = Some(path.clone());
    app.model = "retain-this-pin".into();
    app.current_session_id = Some("retain-this-session".into());
    for _ in 0..2 {
        let result = crate::commands::execute("/status", &mut app);
        assert!(!result.is_error, "{result:?}");
        assert!(result.action.is_none());
        let text = result.message.unwrap();
        assert!(text.contains("retain-this-pin"));
        assert!(text.contains("retain-this-session"));
        assert!(!text.contains("malformed optional config"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }
    assert_eq!(app.model, "retain-this-pin");
    assert_eq!(
        app.current_session_id.as_deref(),
        Some("retain-this-session")
    );
}

#[test]
fn config_policy_status_reports_saved_fleet_drift_without_rewriting_pins_or_files() {
    use crate::fleet::store::{FleetFile, FleetOperator, FleetScope, save_fleet, set_selected};
    let _env = crate::test_support::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let mut app = crate::test_support::test_app_with_options(
        crate::test_support::test_tui_options(temp.path()),
    );
    app.ui_locale = codewhale_localization::Locale::En;
    app.model = "retained-session-pin".into();
    let config = temp.path().join("config.toml");
    std::fs::write(&config, "provider = \"deepseek\"\n").unwrap();
    app.config_path = Some(config.clone());
    let mut fleet: FleetFile = toml::from_str(
        r#"schema = "fleet"
        schema_revision = 2
        name = "policy-{model}-fleet"
        [[members]]
        id = "worker"
        role = "builder"
        [[members]]
        id = "inherited"
        role = "scout"
        "#,
    )
    .unwrap();
    let file = save_fleet(&fleet, FleetScope::Workspace, temp.path()).unwrap();
    let selected = set_selected(&fleet.name, FleetScope::Workspace, temp.path()).unwrap();
    let selection_before = std::fs::read(&selected).unwrap();
    let config_before = std::fs::read(&config).unwrap();
    let observe = |app: &mut crate::tui::app::App| {
        app.command_contexts()
            .contexts(Caps::CONFIG_STATUS)
            .into_parts()
            .config_status
            .unwrap()
            .snapshot()
            .fleet_drift
    };
    assert!(
        observe(&mut app).is_none(),
        "inherited routes are not drift"
    );
    fleet.operator = Some(FleetOperator {
        provider: "removed-policy-provider".into(),
        model: "operator-pin".into(),
        reasoning: None,
    });
    fleet.members[0].provider = Some("removed-policy-provider".into());
    fleet.members[0].model = Some("worker-pin".into());
    save_fleet(&fleet, FleetScope::Workspace, temp.path()).unwrap();
    let original = std::fs::read(&file).unwrap();
    for _ in 0..2 {
        let drift = observe(&mut app).expect("saved operator/member pins drifted");
        assert_eq!(drift.name, "policy-{model}-fleet");
        assert_eq!(drift.ids, ["operator", "worker"]);
        let result = crate::commands::execute("/status", &mut app);
        assert!(!result.is_error);
        assert!(result.action.is_none());
        let text = result.message.unwrap();
        assert!(text.contains("policy-{model}-fleet"), "{text}");
        assert!(text.contains("operator, worker"), "{text}");
        assert_eq!(std::fs::read(&file).unwrap(), original);
        assert_eq!(std::fs::read(&selected).unwrap(), selection_before);
        assert_eq!(std::fs::read(&config).unwrap(), config_before);
        assert_eq!(app.model, "retained-session-pin");
    }
    std::fs::write(&file, "malformed optional fleet [[[").unwrap();
    assert!(observe(&mut app).is_none());
    let result = crate::commands::execute("/status", &mut app);
    assert!(!result.is_error);
    assert!(result.action.is_none());
    assert!(!result.message.unwrap().contains("Fleet:"));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "malformed optional fleet [[["
    );
    assert_eq!(std::fs::read(&selected).unwrap(), selection_before);
    assert_eq!(app.model, "retained-session-pin");
}
