//! Local narrowing for the existing Engine. It grants no authority.
use super::*;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum EngineHostProfile {
    #[default]
    Normal,
    Acp,
    Rlm,
    Child,
}
impl EngineHostProfile {
    pub(crate) fn is_acp(self) -> bool {
        self == Self::Acp
    }
}
/// Additional local narrowing on one exact admitted control. Neither request
/// JSON nor persistent configuration can choose it. Inherit preserves the base
/// host's ceiling (including Child); Acp never replaces that base authority.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum TurnNarrowing {
    #[default]
    Inherit,
    Acp,
}
impl TurnNarrowing {
    pub(crate) fn is_acp(self) -> bool {
        self == Self::Acp
    }
    pub(crate) fn request_fingerprint(self, ordinary: String) -> String {
        match self {
            Self::Inherit => ordinary,
            Self::Acp => codewhale_runtime::hashing::sha256_hex(format!(
                "runtime-turn-narrowing\u{1f}acp\u{1f}{ordinary}"
            )),
        }
    }
}
impl Engine {
    pub(super) fn is_acp_turn(&self) -> bool {
        self.host_profile.is_acp() || self.turn_narrowing.is_acp()
    }

    pub(super) fn acp_tool_build(
        &self,
        authority: &TurnAuthority,
        route: &TurnRouteContext,
        allowed: Option<Vec<String>>,
    ) -> TurnToolBuild {
        let mut context = self.build_tool_context_for_turn(authority, route);
        let requested = self
            .api_config
            .sandbox_backend
            .as_deref()
            .is_some_and(|kind| !kind.trim().is_empty() && !kind.eq_ignore_ascii_case("none"));
        let backend = codewhale_sandbox::backend::create_backend(&self.api_config)
            .ok()
            .flatten()
            .filter(|backend| {
                backend.kind() != codewhale_sandbox::backend::SandboxKind::Unsupported
            })
            .map(Arc::from);
        let shell = authority.allow_shell
            && self.turn_acp_shell_ceiling.unwrap_or(authority.allow_shell)
            && authority.mode != AppMode::Plan
            && self.api_config.allow_shell()
            && self.config.features.enabled(Feature::ShellTool)
            && (!requested || backend.is_some());
        context = context.with_shell_policy(if shell {
            codewhale_agent_contract::worker_profile::ShellPolicy::Full
        } else {
            codewhale_agent_contract::worker_profile::ShellPolicy::None
        });
        if let Some(backend) = backend {
            context = context.with_sandbox_backend(backend);
        }
        let mut builder = ToolRegistryBuilder::new()
            .with_file_tools()
            .with_search_tools()
            .with_git_tools();
        if self.config.features.enabled(Feature::ApplyPatch) {
            builder = builder.with_patch_tools();
        }
        if shell {
            builder = builder.with_foreground_shell_tools();
        }
        let mut registry = builder.build(context);
        if let Some(overrides) = self
            .api_config
            .tools
            .as_ref()
            .and_then(|tools| tools.overrides.as_ref())
        {
            for name in overrides.keys() {
                remove_acp_overridden_builtin(&mut registry, name);
            }
        }
        let allowed = Some(
            registry
                .names()
                .into_iter()
                .filter(|name| tool_catalog::tool_allowed(allowed.as_deref(), name))
                .map(str::to_string)
                .collect(),
        );
        let catalog = self.child_host.as_ref().map_or_else(
            || registry.to_api_tools_with_cache(true),
            |child| {
                child
                    .authority
                    .tools_for_model(&registry, &child.authority.agent_type)
            },
        );
        let always_load = catalog.iter().map(|tool| tool.name.clone()).collect();
        TurnToolBuild {
            surface: ToolSurfacePolicy::new(
                registry,
                Some(catalog),
                authority.mode,
                &always_load,
                &[],
                false,
                allowed,
                self.config.disallowed_tools.clone(),
                self.config.max_tool_calls,
                tool_catalog::ToolMode::Direct,
            ),
            mcp_tool_names: Vec::new(),
            mcp: McpToolState::Disabled,
            subagent_runtime_model: None,
            mailbox: None,
            plugin_tool_names: HashSet::new(),
        }
    }
}
fn remove_acp_overridden_builtin(registry: &mut crate::tools::ToolRegistry, tool_name: &str) {
    let aliases: &[&str] = match tool_name {
        "bash" | "Bash" | "exec_shell" => &["bash", "Bash", "exec_shell"],
        "read" | "write" | "edit" | "File" | "read_file" | "write_file" | "edit_file" => &[
            "read",
            "write",
            "edit",
            "File",
            "read_file",
            "write_file",
            "edit_file",
        ],
        "apply_patch" => &["apply_patch"],
        _ => std::slice::from_ref(&tool_name),
    };
    for alias in aliases {
        registry.remove_tool(alias);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(workspace: &std::path::Path) -> (Engine, EngineHandle, TurnRouteContext) {
        let api = {
            let __cw_config_init_0_0 = Some(true);
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.allow_shell = __cw_config_init_0_0;
            __cw_config_init_0_value
        }
        .with_legacy_root(Some("local-acp-profile-fixture".into()), None);
        let (mut engine, handle) = Engine::new(
            EngineConfig {
                workspace: workspace.to_path_buf(),
                snapshots_enabled: false,
                memory_enabled: false,
                subagents_enabled: false,
                ..Default::default()
            },
            &api,
        );
        engine.config.features.disable(Feature::Mcp);
        engine.host_profile = EngineHostProfile::Acp;
        let route = TurnRouteContext {
            provider: codewhale_config::ProviderKind::Deepseek,
            model: DEFAULT_TEXT_MODEL.into(),
            capabilities: Default::default(),
            limits: None,
            client: engine.codewhale_client.clone(),
            api_config: Box::new(api),
            locale_tag: engine.config.locale_tag.clone(),
            role_models: HashMap::new(),
            auto_model: false,
            reasoning_effort: None,
            reasoning_effort_auto: false,
        };
        (engine, handle, route)
    }
    fn authority(mode: AppMode, shell: bool) -> TurnAuthority {
        TurnAuthority::from_effective_fields(mode, shell, false, false, ApprovalMode::Suggest)
    }
    #[tokio::test(flavor = "current_thread")]
    async fn acp_catalog_and_final_authority_exclude_hidden_runtime_lifecycles() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(dir.path());
        let (engine, _handle, route) = fixture(dir.path());
        let build = engine.acp_tool_build(&authority(AppMode::Agent, false), &route, None);
        assert!(build.surface.registry.get("read").is_some());
        for name in [
            "bash",
            "Bash",
            "exec_shell",
            "exec_shell_interact",
            "code_execution",
            "js_execution",
            "execute_tools",
            "tool_search",
            "Task",
            "request_user_input",
            "rlm",
        ] {
            assert!(build.surface.registry.get(name).is_none(), "{name}");
            assert!(
                !build.surface.catalog.iter().any(|tool| tool.name == name),
                "{name}"
            );
        }
        let unrelated = crate::tools::ToolRegistryBuilder::new()
            .with_foreground_shell_tools()
            .build(build.surface.registry.context().clone());
        let err = unrelated
            .execute_full("bash", serde_json::json!({"command":"printf forbidden"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied { .. }));
    }
    #[tokio::test(flavor = "current_thread")]
    async fn acp_final_cap_rejects_stateful_aliases_and_plan_writes() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(dir.path());
        let (engine, _handle, route) = fixture(dir.path());
        let build = engine.acp_tool_build(&authority(AppMode::Agent, true), &route, None);
        for input in [
            serde_json::json!({"command":"sleep 5 &"}),
            serde_json::json!({"command":"nohup true"}),
            serde_json::json!({"command":"printf x","persist":true}),
            serde_json::json!({"command":"printf x","interactive":true}),
            serde_json::json!({"action":"start","command":"printf x"}),
        ] {
            assert!(
                build
                    .surface
                    .registry
                    .execute_full("Bash", input)
                    .await
                    .is_err()
            );
        }
        let plan = engine.acp_tool_build(&authority(AppMode::Plan, true), &route, None);
        assert!(
            plan.surface
                .registry
                .execute_full("write", serde_json::json!({"path":"no.txt","content":"x"}))
                .await
                .is_err()
        );
        assert!(!dir.path().join("no.txt").exists());
        for command in [
            "printf 'a&b'",
            "printf a\\&b",
            "true && true",
            "printf x >&2",
        ] {
            assert!(
                !crate::tools::shell::foreground_command_requests_detach(command),
                "{command}"
            );
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn acp_override_removes_all_file_and_shell_compatibility_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(dir.path());
        let (mut engine, _handle, route) = fixture(dir.path());
        engine.api_config.tools = Some(codewhale_runtime::config::ToolsConfig {
            overrides: Some(HashMap::from([
                (
                    "write".into(),
                    codewhale_runtime::config::ToolOverride::Disabled,
                ),
                (
                    "exec_shell".into(),
                    codewhale_runtime::config::ToolOverride::Command {
                        command: "must-never-launch".into(),
                        args: None,
                    },
                ),
            ])),
            ..Default::default()
        });
        let build = engine.acp_tool_build(&authority(AppMode::Agent, true), &route, None);
        for name in [
            "read",
            "write",
            "edit",
            "File",
            "read_file",
            "write_file",
            "edit_file",
            "bash",
            "Bash",
            "exec_shell",
        ] {
            assert!(build.surface.registry.get(name).is_none(), "{name}");
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_engine_context_does_not_inherit_acp_narrowing_or_observations() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(dir.path());
        let (mut engine, _handle, route) = fixture(dir.path());
        engine.host_profile = EngineHostProfile::Normal;
        assert_eq!(
            engine
                .build_tool_context_for_turn(&authority(AppMode::Agent, true), &route)
                .acp_host,
            None
        );
        engine.host_profile = EngineHostProfile::Acp;
        assert_eq!(
            engine
                .build_tool_context_for_turn(&authority(AppMode::Plan, true), &route)
                .acp_host,
            Some(AppMode::Plan)
        );
        assert!(matches!(
            engine.goal_continuation_if_active(),
            GoalContinuationAction::Inactive
        ));
    }
    #[tokio::test(flavor = "current_thread")]
    async fn acp_catalogs_keep_concurrent_workspaces_and_foreground_execution_independent() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(dir.path());
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        std::fs::write(a.join("f.txt"), "workspace-a").unwrap();
        std::fs::write(b.join("f.txt"), "workspace-b").unwrap();
        let (ea, _ha, ra) = fixture(&a);
        let (eb, _hb, rb) = fixture(&b);
        let ba = ea.acp_tool_build(&authority(AppMode::Agent, true), &ra, None);
        let bb = eb.acp_tool_build(&authority(AppMode::Agent, false), &rb, None);
        let (oa, ob) = tokio::join!(
            ba.surface
                .registry
                .execute_full("read", serde_json::json!({"path":"f.txt"})),
            bb.surface
                .registry
                .execute_full("read", serde_json::json!({"path":"f.txt"}))
        );
        assert!(oa.unwrap().content.contains("workspace-a"));
        assert!(ob.unwrap().content.contains("workspace-b"));
        assert!(ba.surface.registry.get("bash").is_some());
        assert!(bb.surface.registry.get("bash").is_none());
        let output = ba
            .surface
            .registry
            .execute_full(
                "bash",
                serde_json::json!({"command":"echo acp-foreground-marker"}),
            )
            .await
            .unwrap();
        assert!(output.content.contains("acp-foreground-marker"));
    }
    #[tokio::test(flavor = "current_thread")]
    async fn requested_unavailable_external_sandbox_removes_acp_shell_without_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(dir.path());
        let (mut engine, _handle, route) = fixture(dir.path());
        engine.api_config.sandbox_backend = Some("unsupported-fixture-backend".into());
        let build = engine.acp_tool_build(&authority(AppMode::Agent, true), &route, None);
        assert!(build.surface.registry.get("bash").is_none());
        assert!(build.surface.registry.get("Bash").is_none());
        assert_eq!(
            build.surface.registry.context().shell_policy,
            codewhale_agent_contract::worker_profile::ShellPolicy::None
        );
    }
    #[test]
    fn narrowed_request_fingerprint_keeps_ordinary_historical_bytes_and_separates_acp() {
        let historical = codewhale_runtime::hashing::sha256_hex("ordinary canonical payload");
        assert_eq!(
            TurnNarrowing::Inherit.request_fingerprint(historical.clone()),
            historical
        );
        let narrowed = TurnNarrowing::Acp.request_fingerprint(historical.clone());
        assert_ne!(narrowed, historical);
        assert_eq!(narrowed, TurnNarrowing::Acp.request_fingerprint(historical));
    }
    #[tokio::test(flavor = "current_thread")]
    async fn effective_acp_catalog_keeps_normal_base_and_initial_shell_ceiling() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(dir.path());
        let (mut engine, _handle, route) = fixture(dir.path());
        engine.host_profile = EngineHostProfile::Normal;
        engine.config.max_steps = 97;
        engine.config.goal_max_steps = Some(211);
        engine.config.subagents_enabled = true;
        engine.turn_narrowing = TurnNarrowing::Acp;
        engine.turn_acp_shell_ceiling = Some(false);
        let build = engine.acp_tool_build(&authority(AppMode::Agent, true), &route, None);
        assert!(
            build.surface.registry.get("bash").is_none(),
            "later ordinary authority cannot raise the ACP client's initial terminal ceiling"
        );
        assert_eq!(engine.host_profile, EngineHostProfile::Normal);
        assert_eq!(engine.config.max_steps, 97);
        assert_eq!(engine.config.goal_max_steps, Some(211));
        assert!(engine.config.subagents_enabled);
        assert_eq!(
            engine
                .build_tool_context_for_turn(&authority(AppMode::Agent, true), &route)
                .acp_host,
            Some(AppMode::Agent)
        );
        engine.turn_narrowing = TurnNarrowing::Inherit;
        engine.turn_acp_shell_ceiling = None;
        assert_eq!(
            engine
                .build_tool_context_for_turn(&authority(AppMode::Agent, true), &route)
                .acp_host,
            None
        );
    }
    #[tokio::test(flavor = "current_thread")]
    async fn acp_defers_normal_boot_queue_without_losing_its_generation() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(dir.path());
        let (mut engine, _handle, route) = fixture(dir.path());
        engine.host_profile = EngineHostProfile::Normal;
        engine.turn_narrowing = TurnNarrowing::Acp;
        engine.mcp_boot_generation = Some(1);
        engine.mcp_boot_in_flight = true;
        engine.session.pending_prefix_change_reason = None;
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        engine.mcp_boot_rx = Some(rx);
        tx.try_send(McpBootUpdate::Progress {
            generation: 1,
            authority_errors: Arc::new(HashMap::new()),
            connection_errors: HashMap::new(),
            connecting: vec!["ordinary-pending".into()],
        })
        .unwrap();
        let build = engine.acp_tool_build(&authority(AppMode::Agent, false), &route, None);
        let mut catalog = build.surface.catalog.clone();
        let mut names = catalog.iter().map(|tool| tool.name.clone()).collect();
        engine
            .refresh_boot_mcp_catalog(&build.surface, &mut catalog, &mut names)
            .await;
        assert_eq!(engine.mcp_boot_rx.as_ref().unwrap().len(), 1);
        assert!(engine.session.pending_prefix_change_reason.is_none());
        engine.turn_narrowing = TurnNarrowing::Inherit;
        engine
            .refresh_boot_mcp_catalog(&build.surface, &mut catalog, &mut names)
            .await;
        assert_eq!(engine.mcp_boot_rx.as_ref().unwrap().len(), 0);
        assert_eq!(
            engine.session.pending_prefix_change_reason.as_deref(),
            Some("mcp-session-boot")
        );
    }
    #[tokio::test(flavor = "current_thread")]
    async fn real_engine_queued_normal_acp_normal_controls_keep_exact_fifo_profile() {
        use crate::llm_client::mock::{MockLlmClient, canned};
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(dir.path());
        let api = Config::default().with_legacy_root(Some("local-acp-queue-fixture".into()), None);
        let model = Arc::new(MockLlmClient::new(vec![
            canned::simple_text_turn("ordinary first"),
            canned::simple_text_turn("ACP middle"),
            canned::simple_text_turn("ordinary successor"),
        ]));
        let (mut engine, handle) = Engine::new_with_model_client(
            EngineConfig {
                workspace: dir.path().to_path_buf(),
                snapshots_enabled: false,
                memory_enabled: false,
                max_steps: 97,
                goal_max_steps: Some(211),
                subagents_enabled: true,
                ..Default::default()
            },
            &api,
            model.clone(),
        );
        engine.config.features.disable(Feature::Mcp);
        let message = |content: &str| {
            Op::SendMessage(TurnSpec {
                profile_constitution: None,
                max_output_tokens: None,
                content: content.into(),
                images: Vec::new(),
                mode: AppMode::Agent,
                route: Box::new(
                    resolve_runtime_route_for_identity(
                        &api,
                        &api.active_provider_identity().unwrap(),
                        Some(DEFAULT_TEXT_MODEL),
                    )
                    .unwrap(),
                ),
                compaction: Box::new(CompactionConfig::default()),
                initial_routed_usage: Box::default(),
                goal_objective: None,
                goal_token_budget: None,
                goal_status: GoalStatus::Paused,
                reasoning_effort: None,
                reasoning_effort_auto: false,
                auto_model: false,
                allow_shell: false,
                trust_mode: false,
                auto_approve: false,
                approval_mode: ApprovalMode::Suggest,
                translation_enabled: false,
                allowed_tools: None,
                dynamic_tools: Vec::new(),
                hook_executor: None,
                verbosity: None,
                provenance: UserInputProvenance::ExternalUser,
                submission_id: None,
            })
        };
        handle.send(message("ordinary first")).await.unwrap();
        handle.send_reserved_acp_op(
            handle.tx_op.clone().try_reserve_owned().unwrap(),
            message("ACP middle"),
        );
        handle.send(message("ordinary successor")).await.unwrap();
        assert_eq!(engine.host_profile, EngineHostProfile::Normal);
        assert!(
            !engine.is_acp_turn(),
            "a queued ACP operation must not narrow the current owner"
        );
        let worker = tokio::spawn(Box::pin(engine.run()));
        let mut declared_tool_changes = Vec::new();
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut completed = 0;
            while completed != 3 {
                let event = handle
                    .rx_event
                    .write()
                    .await
                    .recv()
                    .await
                    .expect("Engine event");
                match event {
                    Event::PrefixCacheChange {
                        changed: true,
                        tools_changed: true,
                        pin_reason,
                        ..
                    } => declared_tool_changes.push(pin_reason),
                    Event::TurnComplete { .. } => completed += 1,
                    _ => {}
                }
            }
        })
        .await
        .expect("three actual queued turns");
        assert_eq!(
            declared_tool_changes,
            ["change:tool_surface", "change:tool_surface"],
            "both admitted profile transitions must have attributed prefix changes"
        );
        let requests = model.captured_requests();
        assert_eq!(requests.len(), 3);
        assert_ne!(requests[0].tools, requests[1].tools);
        assert_eq!(
            requests[0].tools, requests[2].tools,
            "the exact queued ACP control must not narrow either ordinary neighbour"
        );
        handle.send(Op::Shutdown).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), worker)
            .await
            .unwrap()
            .unwrap();
    }
}
