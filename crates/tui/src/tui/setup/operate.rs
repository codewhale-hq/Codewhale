use crate::tui::app::App;
use codewhale_runtime::config::Config;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SetupOperateFacts {
    pub(super) runtime_ready: bool,
    pub(super) runtime_result: String,
    pub(super) roster_ready: bool,
    pub(super) roster_result: String,
    pub(super) concurrency_result: String,
    pub(super) result: String,
}

impl Default for SetupOperateFacts {
    fn default() -> Self {
        Self {
            runtime_ready: false,
            runtime_result: "agent runtime not loaded".to_string(),
            roster_ready: false,
            roster_result: "Fleet not loaded".to_string(),
            concurrency_result: "concurrency not loaded".to_string(),
            result: "operate readiness not loaded".to_string(),
        }
    }
}

impl SetupOperateFacts {
    pub(super) fn from_app_config(app: &App, config: &Config, provider_ready: bool) -> Self {
        let provider_identity = app.provider_identity_for_persistence();
        let identity = app
            .admitted_provider_identity()
            .ok()
            .filter(|identity| config.verify_provider_identity(identity).is_ok());
        let subagents_enabled = identity.map_or(false, |identity| {
            config.subagents_enabled_for_provider(identity)
        });
        let max_subagents =
            identity.map_or(0, |identity| config.max_subagents_for_provider(identity));
        let launch_concurrency = identity.map_or(0, |identity| {
            config.launch_concurrency_for_provider(identity)
        });
        let max_admitted = identity.map_or(0, |identity| {
            config.max_admitted_subagents_for_provider(identity)
        });
        let runtime_disabled_reason = if subagents_enabled {
            None
        } else {
            Some(
                config
                    .subagents_disabled_reason()
                    .unwrap_or("disabled for active provider"),
            )
        };
        let max_spawn_depth = identity.map_or(0, |identity| {
            config.subagent_max_spawn_depth_for_provider(identity)
        });
        let runtime_configured =
            subagents_enabled && max_subagents > 0 && launch_concurrency > 0 && max_spawn_depth > 0;
        // Conversation and read-only discovery do not require a worker. This
        // readiness fact describes whether Operate can also dispatch executable
        // work in the background and report its lifecycle accurately.
        let runtime_ready = runtime_configured && provider_ready;
        let runtime_result = if let Some(reason) = runtime_disabled_reason {
            format!("agent runtime disabled ({reason})")
        } else if runtime_ready {
            format!(
                "agent runtime ready for {}; max_subagents={}, launch_concurrency={}, admission={}, max_spawn_depth={}; background dispatch and completion receipts are available",
                provider_identity, max_subagents, launch_concurrency, max_admitted, max_spawn_depth
            )
        } else if runtime_configured {
            format!(
                "agent runtime configured for {}, but the active provider route is not ready; max_subagents={}, launch_concurrency={}, admission={}, max_spawn_depth={}",
                provider_identity, max_subagents, launch_concurrency, max_admitted, max_spawn_depth
            )
        } else {
            format!(
                "agent runtime has no launch capacity for {}; max_subagents={}, launch_concurrency={}, admission={}, max_spawn_depth={}",
                provider_identity, max_subagents, launch_concurrency, max_admitted, max_spawn_depth
            )
        };

        let roster = crate::fleet::identity::load_effective_roster(
            &config.fleet_config(),
            &app.workspace,
            Some(app.plugin_registry.as_ref()),
        );
        let roster_members = roster.members().len();
        let origin_count = |origin| {
            roster
                .members()
                .iter()
                .filter(|member| member.origin == origin)
                .count()
        };
        let config_members = origin_count(crate::fleet::roster::ProfileOrigin::Config);
        let personal_members = origin_count(crate::fleet::roster::ProfileOrigin::Personal);
        let project_members = origin_count(crate::fleet::roster::ProfileOrigin::Workspace);
        let custom_roster_members = config_members + personal_members + project_members;
        let roster_ready = roster.load_error().is_none() && roster_members > 0;
        let roster_result = if let Some(error) = roster.load_error() {
            error.to_string()
        } else if custom_roster_members > 0 {
            let origins = [
                ("config", config_members),
                ("personal", personal_members),
                ("project", project_members),
            ]
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .map(|(label, count)| format!("{label}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
            format!("{roster_members} agents (custom: {origins})")
        } else {
            format!("{roster_members} built-in agents; starter Fleet available")
        };

        let concurrency_result = format!(
            "configured launch_concurrency={launch_concurrency}; max_subagents={max_subagents}; admission={max_admitted}; plan limit not probed"
        );
        let result = format!(
            "provider={}, runtime={}, roster={}, concurrency={}",
            if provider_ready {
                "ready"
            } else {
                "needs_action"
            },
            if runtime_ready {
                "ready"
            } else {
                "needs_action"
            },
            if roster_ready {
                "ready"
            } else {
                "needs_action"
            },
            concurrency_result
        );

        Self {
            runtime_ready,
            runtime_result,
            roster_ready,
            roster_result,
            concurrency_result,
            result,
        }
    }
}
