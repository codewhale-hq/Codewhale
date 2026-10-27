//! Concrete policy observations and atomic permission edits. Replaces the host reads
//! formerly embedded in config/permissions.rs and config/status.rs.
use super::{SharedCommandHost, to_command_approval, to_command_currency, to_command_mode};
use crate::tui::app::App;
use codewhale_command_contract::config_policy::*;

pub(super) struct PermissionsAdapter<'a> {
    pub(super) host: SharedCommandHost<'a>,
}
pub(super) struct ConfigStatusAdapter<'a> {
    pub(super) host: SharedCommandHost<'a>,
}

fn action(value: codewhale_execpolicy::PermissionAction) -> CommandPermissionAction {
    match value {
        codewhale_execpolicy::PermissionAction::Allow => CommandPermissionAction::Allow,
        codewhale_execpolicy::PermissionAction::Ask => CommandPermissionAction::Ask,
        codewhale_execpolicy::PermissionAction::Deny => CommandPermissionAction::Deny,
    }
}
impl CommandPermissionsContext for PermissionsAdapter<'_> {
    fn snapshot(&self) -> Result<PermissionsView, String> {
        let app = self.host.app.borrow();
        let snapshot = codewhale_config::load_permissions_snapshot(app.config_path.clone())
            .map_err(|error| format!("{error:#}"))?;
        let rules = snapshot
            .rules()
            .iter()
            .enumerate()
            .map(|(index, rule)| PermissionRule {
                action: action(rule.action),
                tool: rule.tool.clone(),
                command: rule.command.clone(),
                command_exact: rule.command_exact,
                path: rule.path.clone(),
                workspace: rule.workspace.clone(),
                applies_here: rule_applies_in_workspace(rule, &app.workspace),
                removal_token: snapshot
                    .removal_token(index)
                    .expect("snapshot carries one token per rule")
                    .to_owned(),
            })
            .collect();
        Ok(PermissionsView {
            path: snapshot.path().to_path_buf(),
            rules,
            file_state: match snapshot.file_state() {
                codewhale_config::PermissionsFileState::Missing => {
                    CommandPermissionsFileState::Missing
                }
                codewhale_config::PermissionsFileState::Empty => CommandPermissionsFileState::Empty,
                codewhale_config::PermissionsFileState::Present => {
                    CommandPermissionsFileState::Present
                }
            },
            approval_mode: to_command_approval(app.approval_mode),
            audit_path: codewhale_runtime::audit::audit_log_path(),
        })
    }
    fn remove_rule(
        &mut self,
        index: usize,
        expected_token: &str,
    ) -> Result<RemovedPermissionRule, String> {
        let path = self.host.app.borrow().config_path.clone();
        codewhale_config::remove_permission_rule(path, index, expected_token)
            .map(|rule| RemovedPermissionRule {
                action: action(rule.action),
                tool: rule.tool,
            })
            .map_err(|error| format!("{error:#}"))
    }
}
impl CommandConfigStatusContext for ConfigStatusAdapter<'_> {
    fn snapshot(&self) -> ConfigStatusView {
        project_status(&self.host.app.borrow())
    }
}

fn project_status(app: &App) -> ConfigStatusView {
    let config = crate::plugins::providers::load_config(
        app.config_path.clone(),
        app.config_profile.as_deref(),
    )
    .ok();
    let catalog = crate::models_dev_live::status();
    let metrics = crate::tui::session_metrics::snapshot_from_app(app);
    let outputs =
        crate::tool_output_receipts::tool_output_status(&app.api_messages, &app.session_artifacts);
    let context_window = crate::route_budget::route_context_window_tokens(
        app.api_provider,
        app.effective_model_for_budget(),
        app.active_route_limits,
    );
    let context_used = crate::compaction::estimate_input_tokens_conservative(
        &app.api_messages,
        app.system_prompt.as_ref(),
    )
    .max(codewhale_util::utils::estimate_message_chars(&app.api_messages) / 4);
    let context_source = match app.active_context_window_source {
        crate::route_runtime::ContextWindowSource::Configured => StatusContextSource::Configured,
        crate::route_runtime::ContextWindowSource::UserDeclared => {
            StatusContextSource::UserDeclared
        }
        crate::route_runtime::ContextWindowSource::ConfiguredModel => {
            StatusContextSource::ConfiguredModel
        }
        crate::route_runtime::ContextWindowSource::ProviderReported => {
            StatusContextSource::ProviderReported
        }
        crate::route_runtime::ContextWindowSource::StaticKimiCodeSafeFloor => {
            StatusContextSource::StaticKimiCodeSafeFloor
        }
        crate::route_runtime::ContextWindowSource::Catalog => StatusContextSource::Catalog,
        crate::route_runtime::ContextWindowSource::NameSuffixHint => {
            StatusContextSource::NameSuffixHint
        }
        crate::route_runtime::ContextWindowSource::Fallback => StatusContextSource::Fallback,
    };
    let window_override = if matches!(
        context_source,
        StatusContextSource::Configured | StatusContextSource::ConfiguredModel
    ) {
        None
    } else {
        Some(
            app.provider_identity
                .as_ref()
                .and_then(|identity| identity.config_table_key().ok())
                .map_or(StatusWindowOverride::ActiveProvider, |table| {
                    StatusWindowOverride::Provider(table.to_owned())
                }),
        )
    };
    ConfigStatusView {
        version: env!("CARGO_PKG_VERSION").into(),
        provider: app.provider_identity_for_persistence().into(),
        model: app.model_display_label(),
        reasoning: app.reasoning_effort_display_label(),
        workspace: app.workspace.clone(),
        home: codewhale_paths::user_home(),
        project_docs: ["AGENTS.md", "CLAUDE.md"]
            .into_iter()
            .filter(|name| app.workspace.join(name).is_file())
            .map(str::to_owned)
            .collect(),
        mode: to_command_mode(app.mode),
        approval_mode: to_command_approval(app.approval_mode),
        trusted: app.trust_mode,
        allow_shell: app.allow_shell,
        safety: safety(app),
        mcp_configured_count: app.mcp_configured_count,
        model_pin_drift: config
            .as_ref()
            .and_then(|config| model_pin_drift(app, config)),
        fleet_drift: config.as_ref().and_then(|config| fleet_drift(app, config)),
        snapshot_notice: crate::core::turn::snapshots_disabled_status(
            &app.workspace,
            app.current_session_id.as_deref(),
        ),
        context_used,
        context_window,
        context_source,
        window_override,
        catalog: StatusCatalog {
            freshness: match catalog.freshness {
                crate::models_dev_live::ModelsDevFreshness::Bundled => {
                    StatusCatalogFreshness::Bundled
                }
                crate::models_dev_live::ModelsDevFreshness::Live => StatusCatalogFreshness::Live,
                crate::models_dev_live::ModelsDevFreshness::Stale => StatusCatalogFreshness::Stale,
                crate::models_dev_live::ModelsDevFreshness::Failed => {
                    StatusCatalogFreshness::Failed
                }
            },
            offering_count: catalog.offering_count,
            fetched_at: catalog.fetched_at,
            last_error: catalog.last_error,
        },
        cloud_facts: codewhale_cloud_facts::status().state,
        observed_at: codewhale_config::catalog::now_unix(),
        session_id: app.current_session_id.clone(),
        history_count: app.history.len(),
        message_count: app.api_messages.len(),
        input_tokens: app.session.displayed_total_input_tokens(),
        output_tokens: app.session.displayed_total_output_tokens(),
        total_tokens: app.session.displayed_total_tokens(),
        cache_hit_tokens: app.session.displayed_total_cache_hit_tokens(),
        cache_miss_tokens: app.session.displayed_total_cache_miss_tokens(),
        cost: app.session_cost_for_currency(app.cost_currency),
        currency: to_command_currency(app.cost_display_currency(app.cost_currency)),
        cost_label: match app.cumulative_usage_chip() {
            chip @ crate::route_billing::UsageChip::PricedSubtotal { .. } => {
                crate::route_billing::format_usage_chip(&chip, app.ui_locale)
            }
            crate::route_billing::UsageChip::Unknown(_) => Some(
                codewhale_localization::tr(
                    app.ui_locale,
                    codewhale_localization::MessageId::CmdCostUnknownValue,
                )
                .into_owned(),
            ),
            _ => None,
        },
        metrics,
        ascii_safe: crate::tui::color_compat::ascii_safe_enabled(),
        tool_outputs: outputs,
    }
}

pub(crate) fn safety(app: &App) -> StatusSafety {
    let policy = crate::core::authority::sandbox_policy_for_turn(
        app.mode,
        app.approval_mode,
        app.configured_sandbox_mode.as_deref(),
        &app.workspace,
        crate::core::authority::SandboxNetworkAccess::from_config(app.configured_sandbox_network),
    );
    let enforced = app.sandbox_backend.is_some();
    match policy {
        codewhale_sandbox::SandboxPolicy::ReadOnly { .. } => StatusSafety::ReadOnly { enforced },
        codewhale_sandbox::SandboxPolicy::WorkspaceWrite { network_access, .. } => {
            StatusSafety::WorkspaceWrite {
                enforced,
                network_access,
            }
        }
        codewhale_sandbox::SandboxPolicy::DangerFullAccess => StatusSafety::FullAccess {
            no_new_privs: codewhale_sandbox::process_hardening::no_new_privs_active(),
        },
        codewhale_sandbox::SandboxPolicy::ExternalSandbox { .. } => StatusSafety::External,
    }
}

pub(crate) fn fleet_drift(
    app: &App,
    config: &codewhale_runtime::config::Config,
) -> Option<StatusFleetDrift> {
    let selected = crate::fleet::store::selected_fleet(&app.workspace)?;
    let (fleet, _scope) = crate::fleet::store::load_fleet_at(&selected.path).ok()?;
    let active = config.active_provider_identity().ok();
    let health = crate::provider_readiness::ProviderReadinessSnapshot::default();
    let routes = crate::tui::views::fleet_setup::cross_provider_model_routes(
        config,
        active.as_ref(),
        &health,
    );
    let offered =
        |provider: &str, model: &str| routes.iter().any(|(p, m, _)| p == provider && m == model);
    let mut drifted: Vec<String> = Vec::new();
    if let Some(operator) = &fleet.operator
        && !offered(&operator.provider, &operator.model)
    {
        drifted.push("operator".to_string());
    }
    for member in &fleet.members {
        if let (Some(provider), Some(model)) = (&member.provider, &member.model)
            && !offered(provider, model)
        {
            drifted.push(member.id.clone());
        }
    }
    if drifted.is_empty() {
        return None;
    }
    Some(StatusFleetDrift {
        name: fleet.name,
        ids: drifted,
    })
}

pub(crate) fn model_pin_drift(
    app: &App,
    config: &codewhale_runtime::config::Config,
) -> Option<String> {
    if app.auto_model || app.model.trim().is_empty() {
        return None;
    }
    let provider = app.provider_identity_for_persistence();
    crate::provider_catalog_live::pin_missing_from_fresh_roster(config, provider, &app.model)
        .filter(|missing| *missing)?;
    Some(app.model.clone())
}

pub(crate) fn rule_applies_in_workspace(
    rule: &codewhale_config::ToolAskRule,
    workspace: &std::path::Path,
) -> bool {
    let Some(rule_workspace) = rule.workspace.as_deref() else {
        return true;
    };
    let workspace = workspace.to_string_lossy();
    let Some(rule_workspace) = codewhale_execpolicy::normalize_workspace_scope(rule_workspace)
    else {
        return false;
    };
    let Some(workspace) = codewhale_execpolicy::normalize_workspace_scope(&workspace) else {
        return false;
    };
    rule_workspace == workspace
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_command_contract::handler::CommandCapabilities as Caps;
    use std::fs;
    use tempfile::TempDir;

    fn fixture(temp: &TempDir) -> App {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(temp.path()),
        );
        app.config_path = Some(temp.path().join("config.toml"));
        app.sandbox_backend = None;
        app
    }

    #[test]
    fn config_policy_host_exposes_exact_facets_without_observing_invalid_storage() {
        let temp = TempDir::new().unwrap();
        let mut app = fixture(&temp);
        fs::write(temp.path().join("permissions.toml"), "invalid [[[ ").unwrap();
        let mut bundle = app.command_contexts();
        for (caps, permission, status) in [
            (Caps::NONE, false, false),
            (Caps::PERMISSIONS | Caps::PRESENTATION, true, false),
            (Caps::CONFIG_STATUS | Caps::PRESENTATION, false, true),
        ] {
            let p = bundle.contexts(caps).into_parts();
            assert_eq!(p.permissions.is_some(), permission);
            assert_eq!(p.config_status.is_some(), status);
            assert_eq!(p.presentation.is_some(), permission || status);
            assert!(p.mode_policy.is_none() && p.workspace.is_none() && p.session.is_none());
            assert!(p.model.is_none() && p.cost.is_none() && p.system_prompt.is_none());
            assert!(p.skills.is_none() && p.media.is_none() && p.project.is_none());
            assert!(p.memory.is_none() && p.skill_group.is_none() && p.plugin.is_none());
            assert!(
                p.lifecycle.is_none()
                    && p.control.is_none()
                    && p.export.is_none()
                    && p.structcopy.is_none()
            );
            assert!(
                p.debug_receipts.is_none() && p.debug_change.is_none() && p.debug_history.is_none()
            );
            assert!(
                p.debug_diff.is_none() && p.debug_undo.is_none() && p.debug_diagnostics.is_none()
            );
            assert!(
                p.config.is_none()
                    && p.pet.is_none()
                    && p.trust.is_none()
                    && p.login.is_none()
                    && p.work_surface.is_none()
            );
        }
        assert!(
            bundle
                .contexts(Caps::PERMISSIONS)
                .into_parts()
                .permissions
                .unwrap()
                .snapshot()
                .is_err()
        );
    }

    #[test]
    fn config_policy_permission_adapter_preserves_token_atomicity_and_comment_bytes() {
        let temp = TempDir::new().unwrap();
        let mut app = fixture(&temp);
        let path = temp.path().join("permissions.toml");
        let mut bundle = app.command_contexts();
        let facet = bundle
            .contexts(Caps::PERMISSIONS)
            .into_parts()
            .permissions
            .unwrap();
        assert_eq!(
            facet.snapshot().unwrap().file_state,
            CommandPermissionsFileState::Missing
        );
        let original = "# keep\n[[rules]]\ntool = \"exec_shell\"\naction = \"allow\"\n\n[[rules]]\ntool = \"edit_file\"\naction = \"deny\"\n";
        fs::write(&path, original).unwrap();
        let before = facet.snapshot().unwrap();
        // The config layer resolves the sibling file through
        // `normalize_config_file_path`, so the reported path is canonical:
        // `/private/var/...` on macOS and `\\?\C:\...` with the long name on
        // Windows. Ask the same resolver instead of assuming the raw `TempDir`
        // string, which only matches on Linux.
        let resolved_path =
            codewhale_config::resolve_permissions_path(Some(temp.path().join("config.toml")))
                .unwrap();
        assert_eq!(before.path, resolved_path);
        assert_eq!(before.rules[0].action, CommandPermissionAction::Allow);
        assert_eq!(before.rules[1].action, CommandPermissionAction::Deny);
        assert!(before.rules.iter().all(|r| r.applies_here));
        assert!(facet.remove_rule(0, "wrong").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        let changed = format!("{original}\n# concurrent edit\n");
        fs::write(&path, &changed).unwrap();
        assert!(
            facet
                .remove_rule(0, &before.rules[0].removal_token)
                .is_err()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), changed);
        let current = facet.snapshot().unwrap();
        let removed = facet
            .remove_rule(0, &current.rules[0].removal_token)
            .unwrap();
        assert_eq!(
            removed,
            RemovedPermissionRule {
                action: CommandPermissionAction::Allow,
                tool: "exec_shell".into()
            }
        );
        let remaining = facet.snapshot().unwrap();
        assert_eq!(remaining.rules.len(), 1);
        assert_eq!(remaining.rules[0].tool, "edit_file");
        assert!(
            fs::read_to_string(path)
                .unwrap()
                .contains("# concurrent edit")
        );
    }

    #[test]
    fn config_policy_status_observes_semantic_state_without_writes() {
        let _env = crate::test_support::lock_test_env();
        let temp = TempDir::new().unwrap();
        let mut app = fixture(&temp);
        fs::write(temp.path().join("AGENTS.md"), "fixture").unwrap();
        app.model = "pinned-{model}".into();
        app.current_session_id = Some("session-{provider}".into());
        app.turn_counter = 7;
        let expected_model = app.model_display_label();
        let (a, b) = {
            let mut bundle = app.command_contexts();
            let facet = bundle
                .contexts(Caps::CONFIG_STATUS)
                .into_parts()
                .config_status
                .unwrap();
            (facet.snapshot(), facet.snapshot())
        };
        assert_eq!(a.model, expected_model);
        assert_eq!(a.workspace, temp.path());
        assert_eq!(a.project_docs, ["AGENTS.md"]);
        assert_eq!(a.session_id.as_deref(), Some("session-{provider}"));
        assert_eq!(a.metrics.turns, 7);
        assert_eq!(a.session_id, b.session_id);
        assert_eq!(a.metrics, b.metrics);
        assert!(a.context_window > 0);
        assert_eq!(app.model, "pinned-{model}");
        assert_eq!(app.turn_counter, 7);
        assert!(!temp.path().join("config.toml").exists());
        assert!(!temp.path().join("permissions.toml").exists());
    }
}

#[cfg(test)]
#[test]
fn config_policy_status_retains_notice_after_delivery_and_binds_it_to_session() {
    let _env = crate::test_support::lock_test_env();
    let root = tempfile::TempDir::new().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("large.txt"), vec![b'x'; 4096]).unwrap();
    let mut app = crate::tui::test_support::test_app_with_options(
        crate::tui::test_support::test_tui_options(&workspace),
    );
    app.current_session_id = Some("policy-session".into());
    assert!(
        crate::core::turn::pre_turn_snapshot(&workspace, 1, 1024, None, Some("policy-session"))
            .is_none()
    );
    assert_eq!(
        crate::core::turn::take_snapshots_disabled_notices(&workspace, Some("policy-session"))
            .len(),
        1
    );
    let a = project_status(&app).snapshot_notice.unwrap();
    let b = project_status(&app).snapshot_notice.unwrap();
    assert_eq!(a, b);
    assert_eq!(a.scope, StatusSnapshotScope::WorkspaceTooLarge);
    app.current_session_id = Some("other-session".into());
    assert!(project_status(&app).snapshot_notice.is_none());
    app.current_session_id = Some("policy-session".into());
    assert_eq!(project_status(&app).snapshot_notice, Some(a));
}
