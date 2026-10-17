//! FEAT-029: public command-surface and localization parity for the
//! `debug::diagnostics` slice.
//!
//! The host regressions prove handler/rendering parity. This module proves the
//! *observable command surface* is unchanged: registry metadata (name, aliases,
//! usage), registry position, the `description_key` -> catalog bridge, palette
//! and discovery classification, and canonical-name/alias dispatch equivalence
//! through the public `execute` seam.
//!
//! It lives at the `commands` root for the same FEAT-045 extraction reason as
//! the regression suite. The metadata fixture was captured from the
//! untouched implementation and compared byte-for-byte.

use codewhale_localization::{Locale, MessageId};

use crate::commands::debug_diagnostics_test_support::{DiagnosticsHarness, assert_fixture};
use crate::commands::{CommandResult, execute};
use crate::config::ProviderKind;
use crate::tui::app::AppAction;

/// The eight declared diagnostics commands in registry order, with their exact
/// aliases and catalog description ids.
const DIAGNOSTICS: &[(&str, &[&str], MessageId)] = &[
    ("tokens", &[], MessageId::CmdTokensDescription),
    ("cost", &[], MessageId::CmdCostDescription),
    ("balance", &[], MessageId::CmdBalanceDescription),
    ("cache", &[], MessageId::CmdCacheDescription),
    (
        "preview-request",
        &["dryrun", "preview_request"],
        MessageId::CmdPreviewRequestDescription,
    ),
    ("tools", &["tool-studio"], MessageId::CmdToolsDescription),
    ("system", &["xitong"], MessageId::CmdSystemDescription),
    ("context", &["ctx"], MessageId::CmdContextDescription),
];

fn info(name: &str) -> &'static crate::commands::CommandInfo {
    crate::commands::get_command_info(name)
        .unwrap_or_else(|| panic!("/{name} must remain registered"))
}

/// Freeze the complete registry metadata surface (name, aliases, usage,
/// English description, palette text, discovery flags) for all eight commands.
#[test]
fn diagnostics_surface_metadata_matches_baseline() {
    let _harness = DiagnosticsHarness::new();
    let mut out = String::new();
    for (name, _, _) in DIAGNOSTICS {
        let info = info(name);
        out.push_str(&format!("name: {}\n", info.name));
        out.push_str(&format!("aliases: {:?}\n", info.aliases));
        out.push_str(&format!("usage: {}\n", info.usage));
        out.push_str(&format!(
            "description_en: {}\n",
            info.description_for(Locale::En)
        ));
        out.push_str(&format!(
            "palette_en: {}\n",
            info.palette_description_for(Locale::En)
        ));
        out.push_str(&format!(
            "requires_argument: {}\n",
            info.requires_argument()
        ));
        out.push_str(&format!("unlisted: {}\n", info.is_unlisted()));
        out.push_str(&format!(
            "show_in_empty_discovery: {}\n",
            info.show_in_empty_discovery()
        ));
        out.push_str("---\n");
    }
    assert_fixture("surface_metadata.txt", &out);
}

/// Every canonical name and alias resolves to the same registry entry, and the
/// `description_key` bridge maps to the expected catalog `MessageId`.
#[test]
fn diagnostics_names_aliases_and_description_bridge_are_exact() {
    let registry = crate::commands::registry();
    for (name, aliases, description_id) in DIAGNOSTICS {
        let canonical = info(name);
        assert_eq!(canonical.name, *name, "canonical name");
        assert_eq!(canonical.aliases, *aliases, "/{name} alias list");
        assert_eq!(
            canonical.description_id, *description_id,
            "/{name} catalog bridge"
        );
        assert!(
            !canonical.description_for(Locale::En).trim().is_empty(),
            "/{name} must resolve an English description"
        );

        let entry = registry
            .get(name)
            .unwrap_or_else(|| panic!("/{name} must be registered"));
        assert_eq!(entry.info().name, *name);
        for alias in *aliases {
            let via_alias = registry
                .get(alias)
                .unwrap_or_else(|| panic!("/{alias} must resolve"));
            assert_eq!(
                via_alias.info().name,
                *name,
                "/{alias} must resolve to /{name}"
            );
            assert_eq!(via_alias.info().usage, canonical.usage, "/{alias} usage");
        }
    }
}

/// Portable description keys resolve to the exact original catalog ids for
/// every member; metadata does not grant a runtime presentation facet.
#[test]
fn diagnostics_description_keys_resolve_to_the_original_catalog() {
    let keys = [
        ("tokens", "cmd_tokens_description"),
        ("cost", "cmd_cost_description"),
        ("balance", "cmd_balance_description"),
        ("cache", "cmd_cache_description"),
        ("preview-request", "cmd_preview_request_description"),
        ("tools", "cmd_tools_description"),
        ("system", "cmd_system_description"),
        ("context", "cmd_context_description"),
    ];
    for ((name, _, id), (expected_name, key)) in DIAGNOSTICS.iter().zip(keys) {
        assert_eq!(*name, expected_name);
        assert_eq!(super::contract::key_to_message_id(key), Some(*id));
        assert_eq!(info(name).description_id, *id);
        assert!(!info(name).description_for(Locale::Ja).is_empty());
    }
    assert_eq!(
        super::contract::key_to_message_id("cmd_not_registered_description"),
        None
    );
}

/// Runtime presentation uses the existing catalog and exact named-placeholder
/// contract for only the three commands that need it. Literal metadata is
/// separately bridged above; an unknown key or incomplete replacements fail.
#[test]
fn diagnostics_runtime_translation_uses_original_catalog_and_placeholder_contract() {
    let mut harness = DiagnosticsHarness::new();
    harness.app.ui_locale = Locale::Ja;
    let mut bundle = harness.app.command_contexts();
    let mut parts = bundle
        .contexts(codewhale_command_contract::handler::CommandCapabilities::PRESENTATION)
        .into_parts();
    let presentation = parts.presentation.as_deref_mut().unwrap();
    assert_eq!(
        presentation.translate("cmd_cost_coverage", &[("priced", "2"), ("turns", "3")]),
        Ok("対象: 課金対象ターン 3 件のうち 2 件を算定しました。".into())
    );
    assert_eq!(
        presentation.translate(
            "cmd_cache_totals",
            &[
                ("sum_in", "4"),
                ("sum_hit", "2"),
                ("sum_miss", "1"),
                ("avg", "50%")
            ],
        ),
        Ok("Σ 入力: 4   Σ ヒット: 2   Σ ミス: 1   平均ヒット率: 50%\n".into())
    );
    assert_eq!(
        presentation.translate(
            "cmd_tokens_context_with_window",
            &[("used", "6"), ("window", "12"), ("percent", "50.0")],
        ),
        Ok("~6 / 12 (50.0%)".into())
    );
    assert_eq!(
        presentation.translate("cmd_cost_coverage", &[("priced", "2")]),
        Err("invalid translation replacement contract".into())
    );
    assert_eq!(
        presentation.translate("cmd_not_registered", &[]),
        Err("unknown translation key".into())
    );
}

/// The production registry now exposes exactly the declared facets, including
/// each alias. Preview stays a pure function, not a contextual empty envelope.
#[test]
fn diagnostics_registrations_expose_exact_facets_and_preview_is_pure() {
    use codewhale_command_contract::handler::{
        CommandCapabilities as Caps, CommandContexts, CommandHandler,
    };

    let mut harness = DiagnosticsHarness::new();
    for (name, aliases, _) in DIAGNOSTICS {
        let expected = match *name {
            "tokens" | "cost" | "cache" => Caps::DEBUG_DIAGNOSTICS | Caps::PRESENTATION,
            "preview-request" => Caps::NONE,
            _ => Caps::DEBUG_DIAGNOSTICS,
        };
        for spelling in std::iter::once(*name).chain(aliases.iter().copied()) {
            let registered = crate::commands::registry()
                .get(spelling)
                .expect("registered spelling");
            match registered
                .contextual_handler()
                .expect("portable registration")
            {
                CommandHandler::Pure(pure) => {
                    assert_eq!(*name, "preview-request", "only preview is pure");
                    assert_eq!(expected, Caps::NONE);
                    assert!(matches!(
                        pure(Some("json")).action,
                        Some(AppAction::PreviewOutboundRequest { json: true, .. })
                    ));
                }
                CommandHandler::Contextual {
                    capabilities,
                    handler,
                } => {
                    assert_ne!(*name, "preview-request");
                    assert_eq!(capabilities, expected, "/{spelling} declaration");
                    let result = handler(CommandContexts::empty(), None);
                    assert!(result.is_error);
                    assert_eq!(
                        result.message.as_deref(),
                        Some("Error: Command capability unavailable: debug_diagnostics")
                    );
                    assert!(result.action.is_none());

                    let mut bundle = harness.app.command_contexts();
                    let parts = bundle.contexts(capabilities).into_parts();
                    let codewhale_command_contract::handler::ContextParts {
                        session,
                        model,
                        cost,
                        mode_policy,
                        system_prompt,
                        skills,
                        workspace,
                        presentation,
                        media,
                        memory,
                        project,
                        skill_group,
                        plugin,
                        lifecycle,
                        control,
                        export,
                        structcopy,
                        debug_receipts,
                        debug_change,
                        debug_history,
                        debug_diff,
                        debug_undo,
                        debug_diagnostics,
                        permissions,
                        config_status,
                        config,
                        pet,
                        trust,
                        login,
                        work_surface,
                        navigation,
                        voice_input,
                    } = parts;
                    assert!(
                        permissions.is_none()
                            && config_status.is_none()
                            && config.is_none()
                            && pet.is_none()
                            && trust.is_none()
                            && login.is_none()
                            && work_surface.is_none()
                            && navigation.is_none()
                            && voice_input.is_none()
                    );
                    assert!(debug_diagnostics.is_some(), "/{spelling} needs diagnostics");
                    assert_eq!(
                        presentation.is_some(),
                        expected.contains(Caps::PRESENTATION),
                        "/{spelling} presentation"
                    );
                    for (facet, exposed) in [
                        ("session", session.is_some()),
                        ("model", model.is_some()),
                        ("cost", cost.is_some()),
                        ("mode_policy", mode_policy.is_some()),
                        ("system_prompt", system_prompt.is_some()),
                        ("skills", skills.is_some()),
                        ("workspace", workspace.is_some()),
                        ("media", media.is_some()),
                        ("memory", memory.is_some()),
                        ("project", project.is_some()),
                        ("skill_group", skill_group.is_some()),
                        ("plugin", plugin.is_some()),
                        ("lifecycle", lifecycle.is_some()),
                        ("control", control.is_some()),
                        ("export", export.is_some()),
                        ("structcopy", structcopy.is_some()),
                        ("debug_receipts", debug_receipts.is_some()),
                        ("debug_change", debug_change.is_some()),
                        ("debug_history", debug_history.is_some()),
                        ("debug_diff", debug_diff.is_some()),
                        ("debug_undo", debug_undo.is_some()),
                    ] {
                        assert!(!exposed, "/{spelling} must not expose {facet}");
                    }
                }
            }
        }
    }
    for name in ["receipts", "change", "edit", "diff", "undo", "retry"] {
        assert!(
            crate::commands::registry()
                .get(name)
                .unwrap()
                .contextual_handler()
                .is_some(),
            "/{name} must use its portable registration"
        );
    }
}

/// Registry position inside the debug group is preserved: the eight
/// diagnostics commands stay in their original relative order and the five
/// The other debug commands remain registered around these diagnostics.
#[test]
fn diagnostics_registry_position_matches_baseline() {
    let names: Vec<&str> = crate::commands::command_infos()
        .iter()
        .map(|info| info.name)
        .collect();
    let position = |name: &str| {
        names
            .iter()
            .position(|candidate| *candidate == name)
            .unwrap_or_else(|| panic!("/{name} must be registered; found {names:?}"))
    };

    let order = [
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
    for pair in order.windows(2) {
        assert!(
            position(pair[0]) < position(pair[1]),
            "/{} must stay before /{}",
            pair[0],
            pair[1]
        );
    }
}

fn render(result: &CommandResult) -> String {
    let mut out = String::new();
    out.push_str(&format!("is_error: {}\n", result.is_error));
    match &result.message {
        Some(message) => out.push_str(&format!("message:\n{message}\n")),
        None => out.push_str("message: <none>\n"),
    }
    match &result.action {
        Some(action) => out.push_str(&format!("action: {action:?}\n")),
        None => out.push_str("action: <none>\n"),
    }
    out
}

/// Canonical names and every compatibility alias dispatch to byte-identical
/// results through the public seam.
#[test]
fn public_dispatch_canonical_and_alias_are_byte_equivalent() {
    let preview_cases = [
        "/preview-request json",
        "/preview-request json --prompt keep  the bytes",
        "/preview-request base-prompt",
    ];
    for command in preview_cases {
        let canonical = {
            let mut canonical_harness = DiagnosticsHarness::new();
            execute(command, &mut canonical_harness.app)
        };
        for alias in ["dryrun", "preview_request"] {
            let aliased = command.replacen("preview-request", alias, 1);
            let mut alias_harness = DiagnosticsHarness::new();
            let aliased_result = execute(&aliased, &mut alias_harness.app);
            assert_eq!(
                render(&canonical),
                render(&aliased_result),
                "{aliased} must match {command}"
            );
        }
    }

    // /tools and /tool-studio share the prepared-snapshot payload.
    let canonical = {
        let mut canonical_harness = DiagnosticsHarness::new();
        canonical_harness.app.session.last_tool_request_snapshot = Some(snapshot());
        execute("/tools json", &mut canonical_harness.app)
    };
    let mut alias_harness = DiagnosticsHarness::new();
    alias_harness.app.session.last_tool_request_snapshot = Some(snapshot());
    let aliased = execute("/tool-studio json", &mut alias_harness.app);
    assert_eq!(render(&canonical), render(&aliased));

    // /system and /xitong share the exact message.
    let canonical = {
        let mut canonical_harness = DiagnosticsHarness::new();
        canonical_harness.app.system_prompt =
            Some(codewhale_models::SystemPrompt::Text("aliased".to_string()));
        execute("/system", &mut canonical_harness.app)
    };
    let mut alias_harness = DiagnosticsHarness::new();
    alias_harness.app.system_prompt =
        Some(codewhale_models::SystemPrompt::Text("aliased".to_string()));
    let aliased = execute("/xitong", &mut alias_harness.app);
    assert_eq!(render(&canonical), render(&aliased));

    // /context and /ctx share the bare inspector action.
    let canonical = {
        let mut canonical_harness = DiagnosticsHarness::new();
        execute("/context", &mut canonical_harness.app)
    };
    let mut alias_harness = DiagnosticsHarness::new();
    let aliased = execute("/ctx", &mut alias_harness.app);
    assert!(matches!(
        canonical.action,
        Some(AppAction::OpenContextInspector)
    ));
    assert_eq!(render(&canonical), render(&aliased));
}

/// `/preview-request` remains a pure action-only leaf through the public
/// dispatch path; no prompt, route or provider state is read.
#[test]
fn preview_request_remains_the_pure_registration() {
    // The public dispatcher still uses the staged function wrapper until
    // Phase 6 wires the portable registrations. Even here preview creates no
    // facet bundle and parses the action without provider/session state.
    let mut harness = DiagnosticsHarness::new();
    harness.app.api_provider = ProviderKind::Ollama;
    let result = execute("/preview-request", &mut harness.app);
    assert!(matches!(
        result.action,
        Some(AppAction::PreviewOutboundRequest { .. })
    ));
}

fn snapshot() -> crate::tool_inspection::ToolInspectionSnapshot {
    crate::tool_inspection::ToolInspectionSnapshot::from_prepared_request(
        "turn-1",
        2,
        Some(&[tool("read_file"), tool("write_file")]),
    )
}

fn tool(name: &str) -> codewhale_models::Tool {
    codewhale_models::Tool {
        tool_type: Some("function".to_string()),
        name: name.to_string(),
        description: format!("{name} test tool"),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}}
        }),
        allowed_callers: None,
        defer_loading: Some(false),
        input_examples: None,
        strict: Some(true),
        cache_control: None,
    }
}
