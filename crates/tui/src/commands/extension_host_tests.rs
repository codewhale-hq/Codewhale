//! Extension slash commands through the real command table and the TUI's
//! dispatch.
//!
//! These tests drive `crate::commands` (the user registry, `execute`, the
//! built-in table) and `crate::tui::app::App`, so they live on the commands
//! side of the runtime/UI boundary: the extension host is a runtime module and
//! its own tests may not depend on either (`scripts/check-command-crate-boundaries.py`).
//! They run the real Node host over the same fixtures the host's own tests use
//! and, where the table matters, install the real built-in command catalog
//! ([`CommandTable`]) in place of the host tests' stub.

use std::path::PathBuf;
use std::sync::Arc;

use super::CommandTable;
use codewhale_engine::command_catalog::CatalogGuard;
use codewhale_engine::extension_host::protocol::{RegisterKind, RegisterParams, RegisterSpecWire};
use codewhale_engine::extension_host::registry::{OwnerRegistry, OwnerState};
use codewhale_engine::extension_host::test_support::{
    FixturePlugins, fake_authority, installed, node_for_tests, plugin_id,
};
use codewhale_engine::extension_host::tier::HostTier;
use codewhale_plugins::activation::TestPolicyGuard;

/// The real catalog answers to every built-in name, every alias and the fixed
/// mode aliases the dispatcher handles ahead of the registry, and an extension
/// command cannot take any of them.
#[test]
fn the_real_built_in_catalog_refuses_every_name_alias_and_mode_alias() {
    use codewhale_engine::command_catalog::CommandCatalog;
    let catalog = CommandTable;
    let rows = catalog.builtins();
    let row = |name: &str| {
        rows.iter()
            .find(|row| row.name == name)
            .expect("real command row")
    };
    let model = row("model");
    assert_eq!(
        model.discovery,
        codewhale_command_contract::CommandDiscovery::Primary
    );
    assert!(!model.unlisted);
    assert!(!model.usage.is_empty());
    assert!(model.takes_arguments);
    let profile = row("profile");
    assert!(profile.requires_argument);
    assert!(profile.requires_required_argument);
    assert!(profile.composer_wants_trailing_space);
    assert!(!profile.palette_runs_directly);
    assert!(row("lane").unlisted);
    assert!(row("goal").subcommands.iter().any(|verb| verb == "blocked"));
    let mut owned = vec!["jihua".to_string(), "zidong".to_string()];
    for info in crate::commands::command_infos() {
        owned.push(info.name.to_string());
        owned.extend(info.aliases.iter().map(|alias| (*alias).to_string()));
    }
    assert!(
        crate::commands::command_infos()
            .iter()
            .any(|info| !info.aliases.is_empty()),
        "some built-in has an alias"
    );
    for name in &owned {
        assert!(catalog.answers_to(name), "/{name} is built in");
    }
    assert!(!catalog.answers_to("ext-echo"));

    let _catalog = CatalogGuard::install(Arc::new(CommandTable));
    let mut registry = OwnerRegistry::new();
    let owner = registry
        .begin_owner(
            HostTier::Plugin,
            "a",
            "a",
            Some(fake_authority("a")),
            "hash-a",
        )
        .unwrap();
    let register = |registry: &mut OwnerRegistry, name: &str| {
        registry.register(&RegisterParams {
            scope: None,
            owner: owner.clone(),
            kind: RegisterKind::Command,
            spec: RegisterSpecWire {
                name: name.to_string(),
                description: "d".to_string(),
                input_schema: None,
                argument_hint: None,
            },
        })
    };
    for name in ["help", "trust", "model", "jihua", "zidong"]
        .into_iter()
        .map(str::to_string)
        .chain(owned)
    {
        let in_grammar = name.starts_with(|c: char| c.is_ascii_lowercase())
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
        if in_grammar {
            let handle = register(&mut registry, &name).unwrap();
            let _ = registry.mark_active(&owner);
            let command = registry
                .live_command(handle, "a", owner.generation)
                .unwrap();
            assert_ne!(command.name, name);
            assert!(!catalog.answers_to(&command.name));
            registry.unregister(&owner, handle);
        } else {
            assert!(
                register(&mut registry, &name)
                    .unwrap_err()
                    .contains("invalid")
            );
        }
    }
    register(&mut registry, "ext-not-built-in").unwrap();
}

/// Commands registered by real plugins in a real host: what the user
/// registry loads, what dispatch returns, and what running each one gives.
#[tokio::test]
async fn extension_commands_run_end_to_end_through_the_user_command_registry() {
    use crate::tui::app::{App, AppAction, TuiOptions};
    use codewhale_engine::extension_host::command::CommandOutcome;

    let Some(node) = node_for_tests("extension_commands_run_end_to_end") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["ext-commands"]).await;
    let _catalog = CatalogGuard::install(Arc::new(CommandTable));
    let manager = fixture.manager(node);
    let _manager =
        codewhale_engine::extension_host::TestManagerGuard::install(Arc::clone(&manager));
    let engine = manager.attach(fixture.registry());
    // Nothing is visible before the host has registered anything.
    assert!(manager.live_command_names().is_empty());
    engine.sync().await.unwrap();
    let mut live = manager.live_command_names();
    live.sort();
    assert_eq!(
        live,
        [
            "ext-ansi",
            "ext-ask",
            "ext-dsh",
            "ext-echo",
            "ext-fail",
            "ext-slow",
            "ext-throw"
        ]
    );
    // Commands are not tools.
    assert!(manager.live_tool_names().is_empty());
    assert!(installed(&engine, fixture.workspace()).is_empty());

    // The user registry loads them for this workspace (and no other).
    let hint = |name: &str| {
        crate::commands::user_registry::with_registry_for_plugins(
            engine.plugin_view().as_ref(),
            |registry| {
                registry
                    .get(name)
                    .map(|command| (command.argument_hint.clone(), command.takes_arguments()))
            },
        )
    };
    assert_eq!(hint("ext-echo"), Some((Some("<text>".to_string()), true)));
    assert_eq!(hint("ext-ask"), Some((Some("<topic>".to_string()), true)));
    assert_eq!(hint("ext-fail"), Some((None, false)));
    let other = fixture.workspace().join("elsewhere");
    assert!(
        crate::commands::user_registry::with_registry_for_workspace(Some(&other), |registry| {
            registry.get("ext-echo").is_none()
        }),
        "another workspace never sees this workspace's extension commands"
    );
    // Discovery lists them.
    let described = crate::commands::user_registry::with_registry_for_plugins(
        engine.plugin_view().as_ref(),
        |registry| {
            registry
                .iter()
                .filter(|command| command.extension.is_some())
                .count()
        },
    );
    assert_eq!(described, 7);

    // Dispatch is the ordinary slash-command path, and returns the action
    // the UI loop runs; the arguments arrive trimmed.
    let mut app = App::new(
        TuiOptions {
            workspace: fixture.workspace().to_path_buf(),
            ..crate::tui::test_support::test_tui_options(fixture.workspace())
        },
        &codewhale_runtime::config::Config::default(),
    );
    let help = crate::commands::execute("/help ext-echo", &mut app);
    assert!(
        help.message
            .as_deref()
            .is_some_and(|text| text.contains("Echo the arguments.")),
        "{help:?}"
    );
    let dispatched = |app: &mut App, input: &str| match crate::commands::execute(input, app).action
    {
        Some(AppAction::RunExtensionCommand {
            command,
            name,
            input,
        }) => (command, name, input),
        other => panic!("{input}: expected an extension command action, got {other:?}"),
    };
    let (echo, name, args) = dispatched(&mut app, "/ext-echo   hello   there ");
    assert_eq!(
        (name.as_str(), args.as_str()),
        ("ext-echo", "hello   there")
    );
    assert_eq!(echo.origin, "extension:ext-commands");
    assert_eq!(
        codewhale_engine::extension_host::run_command(&echo, &args, None).await,
        Ok(CommandOutcome::Show {
            text: "echo: hello   there".to_string()
        })
    );
    let (ask, _, args) = dispatched(&mut app, "/EXT-ASK tokens");
    assert_eq!(
        codewhale_engine::extension_host::run_command(&ask, &args, None).await,
        Ok(CommandOutcome::Submit {
            prompt: "Summarize: tokens".to_string(),
            note: Some("Asking the model.".to_string())
        })
    );
    let (fail, _, args) = dispatched(&mut app, "/ext-fail");
    assert_eq!(
        codewhale_engine::extension_host::run_command(&fail, &args, None).await,
        Err("unknown topic".to_string())
    );
    let (thrown, _, args) = dispatched(&mut app, "/ext-throw");
    let error = codewhale_engine::extension_host::run_command(&thrown, &args, None)
        .await
        .unwrap_err();
    assert!(
        error.starts_with("failed:") && error.contains("boom"),
        "{error}"
    );
    // Escape sequences never reach the transcript.
    let (ansi, _, args) = dispatched(&mut app, "/ext-ansi");
    assert_eq!(
        codewhale_engine::extension_host::run_command(&ansi, &args, None).await,
        Ok(CommandOutcome::Show {
            text: "plain red end".to_string()
        })
    );
    // The DSH-shaped `rawInput` keeps its leading separator.
    let (dsh, _, args) = dispatched(&mut app, "/ext-dsh a b");
    assert_eq!(
        codewhale_engine::extension_host::run_command(&dsh, &args, None).await,
        Ok(CommandOutcome::Show {
            text: "\" a b\"".to_string()
        })
    );

    // Disabling the plugin removes every command at once: the registry stops
    // listing them, and the reference a user (or palette) still holds fails
    // closed instead of reaching the host.
    engine.set_plugins(fixture.disable("ext-commands"));
    engine.sync().await.unwrap();
    assert!(manager.live_command_names().is_empty());
    assert_eq!(hint("ext-echo"), None);
    let error = codewhale_engine::extension_host::run_command(&echo, "x", None)
        .await
        .unwrap_err();
    assert!(
        (error.contains("no longer registered") || error.contains("no longer selected")),
        "{error}"
    );
    manager.shutdown().await;
}

/// Built-in names remain protected and plugin commands remain scoped;
/// a built-in collision is available under its plugin-specific name.
#[tokio::test]
async fn extension_commands_never_shadow_built_ins_or_other_plugins() {
    let Some(node) = node_for_tests("extension_commands_never_shadow") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&[
        "ext-commands",
        "commands-clash-builtin",
        "commands-clash-plugin",
    ])
    .await;
    let _catalog = CatalogGuard::install(Arc::new(CommandTable));
    let manager = fixture.manager(node);
    let _manager =
        codewhale_engine::extension_host::TestManagerGuard::install(Arc::clone(&manager));
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let state = |name: &str| manager.owner_state(&plugin_id(&fixture, name)).unwrap();
    assert_eq!(state("commands-clash-builtin"), OwnerState::Active);
    assert!(
        manager
            .live_command_names()
            .contains(&"plugin-commands-clash-builtin-help".to_string())
    );
    // Distinct entry scopes may register the same spelling. The selected
    // caller refuses ambiguity instead of choosing a global union winner.
    assert_eq!(state("ext-commands"), OwnerState::Active);
    assert_eq!(state("commands-clash-plugin"), OwnerState::Active);
    assert_eq!(
        manager
            .live_command_names()
            .iter()
            .filter(|name| *name == "ext-echo")
            .count(),
        2
    );
    let selected = manager.commands_for_plugins(engine.plugin_view().as_ref());
    let alias = selected
        .iter()
        .find(|entry| entry.registration.name == "plugin-commands-clash-builtin-help")
        .unwrap();
    assert_eq!(
        codewhale_engine::extension_host::run_command(&alias.reference(), "", None).await,
        Ok(
            codewhale_engine::extension_host::command::CommandOutcome::Show {
                text: "shadowed".into()
            }
        )
    );
    assert!(
        !selected
            .iter()
            .any(|entry| entry.registration.name == "ext-echo")
    );
    assert!(!manager.live_command_names().contains(&"help".to_string()));
    let diagnostics = manager.diagnostics().join("\n");
    assert!(
        diagnostics.contains("/plugin-commands-clash-builtin-help"),
        "{diagnostics}"
    );
    manager.shutdown().await;

    // The user registry is the last line: a command already defined by a
    // markdown source, or by a built-in, wins the spelling and the extension
    // one is left out with a load error naming why.
    let mut registry = crate::commands::user_registry::UserCommandRegistry::from_loaded(vec![(
        "ext-echo".to_string(),
        "markdown wins".to_string(),
    )]);
    let entry = |name: &str| codewhale_engine::extension_host::command::ExtensionCommandEntry {
        selection: None,
        registration: codewhale_engine::extension_host::registry::CommandRegistration {
            scope: None,
            handle: 1,
            owner: codewhale_config::OwnerRef {
                plugin_id: "p".into(),
                generation: 1,
                owner_token: "t".into(),
            },
            tier: HostTier::Plugin,
            plugin_name: "p".into(),
            content_hash: "h".into(),
            name: name.to_string(),
            declared_name: name.to_string(),
            description: "d".into(),
            argument_hint: None,
        },
        authority: fake_authority("p"),
        workspace: PathBuf::from("/w"),
    };
    registry.load_extension_commands(vec![entry("ext-echo"), entry("help")]);
    let errors: Vec<String> = registry
        .load_errors()
        .iter()
        .map(|error| error.message.clone())
        .collect();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("'/ext-echo' collides with another command")),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("'/help' collides with a built-in command")),
        "{errors:?}"
    );
    assert!(registry.get("ext-echo").unwrap().extension.is_none());
}
