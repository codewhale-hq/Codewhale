//! Real portable handlers exercised with deterministic semantic facets.
use super::*;
use codewhale_command_contract::config_policy::*;
use codewhale_command_contract::facets::CommandPresentationContext;
use codewhale_command_contract::handler::{CommandCapabilities as Caps, CommandContexts};
use codewhale_command_contract::outcome::ConfigPolicyAction;
use codewhale_command_contract::types::{CommandApprovalMode, CommandCurrency, CommandMode};
use std::cell::Cell;

struct English {
    fail_key: Option<&'static str>,
}
impl CommandPresentationContext for English {
    fn translate(&self, key: &str, replacements: &[(&str, &str)]) -> Result<String, String> {
        if self.fail_key == Some(key) {
            return Err("invalid translation replacement contract".into());
        }
        let catalog: serde_json::Value =
            serde_json::from_str(include_str!("../../../../../localization/locales/en.json"))
                .unwrap();
        let id: String = key
            .split('_')
            .map(|word| {
                let mut chars = word.chars();
                chars
                    .next()
                    .unwrap()
                    .to_uppercase()
                    .chain(chars)
                    .collect::<String>()
            })
            .collect();
        let template = catalog[&id]
            .as_str()
            .ok_or_else(|| format!("unknown test key {key}"))?;
        let names: Vec<_> = replacements
            .iter()
            .map(|(name, value)| (format!("{{{name}}}"), *value))
            .collect();
        Ok(interpolate(
            template,
            &names
                .iter()
                .map(|(name, value)| (name.as_str(), *value))
                .collect::<Vec<_>>(),
        ))
    }
}
struct Permissions {
    view: PermissionsView,
    reads: Cell<usize>,
    removals: Vec<(usize, String)>,
    failure: bool,
}
impl Permissions {
    fn new() -> Self {
        Self {
            view: PermissionsView {
                path: "permissions.toml".into(),
                file_state: CommandPermissionsFileState::Present,
                rules: vec![PermissionRule {
                    action: CommandPermissionAction::Allow,
                    tool: "exec_{tool}\n".into(),
                    command: Some("echo {command}".into()),
                    command_exact: true,
                    path: None,
                    workspace: None,
                    applies_here: true,
                    removal_token: "opaque".into(),
                }],
                approval_mode: CommandApprovalMode::Suggest,
                audit_path: None,
            },
            reads: Cell::new(0),
            removals: vec![],
            failure: false,
        }
    }
}
impl CommandPermissionsContext for Permissions {
    fn snapshot(&self) -> Result<PermissionsView, String> {
        self.reads.set(self.reads.get() + 1);
        if self.failure {
            Err("permission read failed".into())
        } else {
            Ok(self.view.clone())
        }
    }
    fn remove_rule(&mut self, index: usize, token: &str) -> Result<RemovedPermissionRule, String> {
        self.removals.push((index, token.into()));
        if self.failure || index != 0 || token != "opaque" {
            return Err("stale token".into());
        }
        let rule = self.view.rules.remove(index);
        Ok(RemovedPermissionRule {
            action: rule.action,
            tool: rule.tool,
        })
    }
}
fn permission(p: &mut Permissions, arg: Option<&str>) -> CommandResult {
    permissions::execute(
        CommandContexts::empty()
            .with_permissions(p)
            .with_presentation(&mut English { fail_key: None }),
        arg,
    )
}
fn status_view() -> ConfigStatusView {
    ConfigStatusView {
        version: "fixture".into(),
        provider: "provider-{model}".into(),
        model: "model-{provider}".into(),
        reasoning: "max".into(),
        workspace: "/workspace".into(),
        home: None,
        project_docs: vec![],
        mode: CommandMode::Agent,
        approval_mode: CommandApprovalMode::Suggest,
        trusted: false,
        allow_shell: false,
        safety: StatusSafety::ReadOnly { enforced: true },
        mcp_configured_count: 2,
        model_pin_drift: None,
        fleet_drift: None,
        snapshot_notice: None,
        context_used: 25,
        context_window: 100,
        context_source: StatusContextSource::Catalog,
        window_override: Some(StatusWindowOverride::Provider("custom".into())),
        catalog: StatusCatalog {
            freshness: StatusCatalogFreshness::Bundled,
            offering_count: 0,
            fetched_at: None,
            last_error: None,
        },
        cloud_facts: codewhale_protocol::cloud_facts::CloudFactsState::Off,
        observed_at: 3600,
        session_id: None,
        history_count: 3,
        message_count: 4,
        input_tokens: 5,
        output_tokens: 6,
        total_tokens: 11,
        cache_hit_tokens: 0,
        cache_miss_tokens: 0,
        cost: 0.00001,
        currency: CommandCurrency::Usd,
        cost_label: None,
        metrics: StatusMetrics::default(),
        ascii_safe: false,
        tool_outputs: StatusToolOutputs::default(),
    }
}
struct Status {
    view: ConfigStatusView,
    reads: Cell<usize>,
}
impl CommandConfigStatusContext for Status {
    fn snapshot(&self) -> ConfigStatusView {
        self.reads.set(self.reads.get() + 1);
        self.view.clone()
    }
}
fn report(view: ConfigStatusView) -> String {
    let expected = view.clone();
    let mut s = Status {
        view,
        reads: Cell::new(0),
    };
    let result = status::execute(
        CommandContexts::empty()
            .with_config_status(&mut s)
            .with_presentation(&mut English { fail_key: None }),
        Some("ignored"),
    );
    assert!(!result.is_error, "{result:?}");
    assert!(result.action.is_none());
    assert_eq!(s.reads.get(), 1);
    assert_eq!(s.view, expected);
    result.message.unwrap()
}
#[test]
fn inventory_metadata_and_authority_are_the_actual_two_entry_slice() {
    let entries = portable_handlers();
    assert_eq!(
        entries
            .iter()
            .map(|(info, _)| info.name)
            .collect::<Vec<_>>(),
        ["permissions", "status"]
    );
    assert_eq!(
        entries[0].0.aliases,
        ["permission-rules", "permission_rules"]
    );
    assert_eq!(
        entries[0].0.usage,
        "/permissions [list|remove <rule-number> [--confirm <token>]]"
    );
    assert_eq!(entries[1].0.usage, "/status");
    for ((_, handler), caps) in entries.into_iter().zip([
        Caps::PERMISSIONS | Caps::PRESENTATION,
        Caps::CONFIG_STATUS | Caps::PRESENTATION,
    ]) {
        let CommandHandler::Contextual {
            capabilities,
            handler,
        } = handler
        else {
            panic!("contextual handler required")
        };
        assert_eq!(capabilities, caps);
        let result = handler(CommandContexts::empty(), None);
        assert!(result.is_error);
        assert!(result.action.is_none());
    }
}
#[test]
fn missing_presentation_rejects_before_any_observation_or_mutation() {
    let mut p = Permissions::new();
    assert!(
        permissions::execute(
            CommandContexts::empty().with_permissions(&mut p),
            Some("remove 1 --confirm opaque")
        )
        .is_error
    );
    assert_eq!(p.reads.get(), 0);
    assert!(p.removals.is_empty());
    let mut s = Status {
        view: status_view(),
        reads: Cell::new(0),
    };
    assert!(status::execute(CommandContexts::empty().with_config_status(&mut s), None).is_error);
    assert_eq!(s.reads.get(), 0);
    let mut m = English { fail_key: None };
    assert!(
        permissions::execute(CommandContexts::empty().with_presentation(&mut m), None).is_error
    );
    assert!(status::execute(CommandContexts::empty().with_presentation(&mut m), None).is_error);
}
#[test]
fn translation_failure_precedes_even_confirmed_permission_write() {
    let mut p = Permissions::new();
    let mut m = English {
        fail_key: Some("permissions_removed"),
    };
    let result = permissions::execute(
        CommandContexts::empty()
            .with_permissions(&mut p)
            .with_presentation(&mut m),
        Some("remove 1 --confirm opaque"),
    );
    assert!(result.is_error);
    assert!(result.action.is_none());
    assert!(p.removals.is_empty());
    assert_eq!(p.reads.get(), 0);
}
#[test]
fn permission_grammar_rejects_without_touching_host_and_preview_is_read_only() {
    let mut p = Permissions::new();
    for args in [
        "unknown",
        "remove",
        "remove NaN",
        "remove 0",
        "remove 1 extra",
        "remove 1 --bad opaque",
    ] {
        let result = permission(&mut p, Some(args));
        assert!(result.is_error, "{args}");
        assert!(result.action.is_none());
    }
    assert_eq!(p.reads.get(), 0);
    assert!(p.removals.is_empty());
    let preview = permission(&mut p, Some(" ReMoVe 1 "));
    assert!(!preview.is_error);
    assert!(preview.action.is_none());
    let text = preview.message.unwrap();
    assert!(text.contains("/permissions remove 1 --confirm opaque"));
    assert!(text.contains("echo {command}"));
    assert_eq!(p.reads.get(), 1);
    assert!(p.removals.is_empty());
    assert_eq!(p.view.rules.len(), 1);
}
#[test]
fn confirmation_preserves_opaque_values_and_emits_one_shared_action() {
    let mut p = Permissions::new();
    let result = permission(&mut p, Some("remove 1 --CONFIRM opaque"));
    assert!(!result.is_error);
    assert_eq!(
        result.action,
        Some(ConfigPolicyAction::PermissionRulesChanged)
    );
    assert!(result.message.unwrap().contains("exec_{tool}\\n"));
    assert_eq!(p.removals, [(0, "opaque".into())]);
    assert_eq!(p.reads.get(), 0);
    assert!(p.view.rules.is_empty());
}
#[test]
fn read_and_stale_removal_failures_do_not_emit_actions() {
    let mut p = Permissions::new();
    p.failure = true;
    let read = permission(&mut p, None);
    assert!(read.is_error);
    assert!(read.message.unwrap().contains("permission read failed"));
    assert!(read.action.is_none());
    let remove = permission(&mut p, Some("remove 1 --confirm opaque"));
    assert!(remove.is_error);
    assert!(remove.message.unwrap().contains("stale token"));
    assert!(remove.action.is_none());
    assert_eq!(p.view.rules.len(), 1);
}
#[test]
fn status_renders_typed_observations_once_and_keeps_runtime_braces() {
    let text = report(status_view());
    assert!(text.starts_with("codewhale fixture\n\n"));
    assert!(text.contains("provider-{model} · model-{provider} · reasoning max"));
    assert!(text.contains("25.0% used (25 / 100 tokens)"));
    assert!(text.contains("5 in · 6 out · 11 total · cache not reported"));
    assert!(text.contains("<$0.0001"));
    assert!(text.contains("[providers.custom] context_window in config.toml"));
    assert!(!text.contains("Catalog:"));
    assert!(!text.contains("Session metrics:"));
    assert!(text.contains("3 cells · 4 API messages"));
}
#[test]
fn status_optional_drift_notice_catalog_metrics_and_output_keep_order() {
    let mut view = status_view();
    view.model_pin_drift = Some("raw-{model}".into());
    view.fleet_drift = Some(StatusFleetDrift {
        name: "fleet-{ids}".into(),
        ids: vec!["operator".into(), "worker".into()],
    });
    view.snapshot_notice = Some(StatusSnapshotNotice {
        workspace: "place-{limit}".into(),
        scope: StatusSnapshotScope::WorkspaceTooLarge,
        limit: "2 GB".into(),
    });
    view.catalog = StatusCatalog {
        freshness: StatusCatalogFreshness::Failed,
        offering_count: 8,
        fetched_at: Some(0),
        last_error: Some("offline".into()),
    };
    view.context_used = 200;
    view.window_override = None;
    view.cache_hit_tokens = 7;
    view.cache_miss_tokens = 8;
    view.metrics.turns = 1;
    view.metrics.steps = 2;
    view.tool_outputs.artifact_count = 2;
    view.tool_outputs.artifact_bytes = 1025;
    let text = report(view);
    assert!(text.contains("raw-{model}"));
    assert!(text.contains("fleet-{ids}"));
    assert!(text.contains("operator, worker"));
    assert!(text.contains("place-{limit}"));
    assert!(text.contains("[snapshots] max_workspace_gb"));
    assert!(text.contains("100.0% used"));
    assert!(!text.contains("Window override:"));
    assert!(text.contains("models.dev refresh failed · 8 offerings · fetched 1h ago (offline)"));
    assert!(text.contains("1 turn · 2 steps"));
    assert!(text.contains("2 KB"));
    assert!(text.find("Fleet:").unwrap() < text.find("Context window:").unwrap());
}
#[test]
fn status_translation_failure_never_observes_host() {
    let mut s = Status {
        view: status_view(),
        reads: Cell::new(0),
    };
    let mut m = English {
        fail_key: Some("status_route_summary"),
    };
    let result = status::execute(
        CommandContexts::empty()
            .with_config_status(&mut s)
            .with_presentation(&mut m),
        None,
    );
    assert!(result.is_error);
    assert!(result.action.is_none());
    assert_eq!(s.reads.get(), 0);
}

#[test]
fn missing_facets_use_the_inherited_exact_error_contract() {
    let missing = permissions::execute(CommandContexts::empty(), None);
    assert_eq!(
        missing.message.as_deref(),
        Some("Error: Command capability unavailable: permissions")
    );
    let missing = status::execute(CommandContexts::empty(), None);
    assert_eq!(
        missing.message.as_deref(),
        Some("Error: Command capability unavailable: config_status")
    );
    let mut permissions = Permissions::new();
    let missing = permissions::execute(
        CommandContexts::empty().with_permissions(&mut permissions),
        None,
    );
    assert_eq!(
        missing.message.as_deref(),
        Some("Error: Command capability unavailable: presentation")
    );
    let mut status = Status {
        view: status_view(),
        reads: Cell::new(0),
    };
    let missing = status::execute(
        CommandContexts::empty().with_config_status(&mut status),
        None,
    );
    assert_eq!(
        missing.message.as_deref(),
        Some("Error: Command capability unavailable: presentation")
    );
    assert_eq!(permissions.reads.get(), 0);
    assert_eq!(status.reads.get(), 0);
}

#[cfg(unix)]
#[test]
fn permissions_quote_non_utf8_paths_without_losing_bytes_or_emitting_controls() {
    use std::os::unix::ffi::OsStringExt;
    let mut permissions = Permissions::new();
    permissions.view.path = std::ffi::OsString::from_vec(vec![b'b', b'a', b'd', 0xff, 0x1b]).into();
    let result = permission(&mut permissions, None);
    assert!(!result.is_error);
    let text = result.message.unwrap();
    assert!(text.contains("\"bad\\xff\\x1b\""), "{text}");
    assert!(!text.contains('\u{1b}'));
    assert!(permissions.removals.is_empty());
}
