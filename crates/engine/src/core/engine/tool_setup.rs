//! Per-turn tool registry setup.
//!
//! This keeps mode/feature-specific registry construction out of the send path.

use super::*;
use crate::core::authority::shell_policy_for_mode;
use crate::tools::AgentToolSurfaceOptions;
use codewhale_agent_contract::worker_profile::ShellPolicy;

fn should_register_remember_tool(memory_enabled: bool) -> bool {
    memory_enabled
}

impl Engine {
    pub(super) fn agent_tool_surface_options(
        &self,
        shell_policy: ShellPolicy,
    ) -> AgentToolSurfaceOptions {
        let mut options = AgentToolSurfaceOptions::new(shell_policy);
        options.apply_patch_enabled = self.config.features.enabled(Feature::ApplyPatch);
        options.web_search_enabled = self.config.features.enabled(Feature::WebSearch);
        options.memory_tool_enabled = should_register_remember_tool(self.config.memory_enabled);
        options.vision_config = if self.config.features.enabled(Feature::VisionModel) {
            self.config.vision_config.clone()
        } else {
            None
        };
        options.speech_output_dir = self.config.speech_output_dir.clone();
        options.goal_state = Some(self.config.goal_state.clone());
        options.verify_tool_enabled = self.config.features.enabled(Feature::Verify);
        options.user_input_limits = self.config.user_input_limits;
        options.request_plugin_install_enabled = self.request_plugin_install_allowed();
        options
    }

    /// `request_plugin_install` returns a TUI slash command and is a
    /// proactive offer, so it exists only in the interactive TUI with
    /// contextual tips on (0.10.1 plugin offering policy, rules 3 and 11).
    /// Exec, ACP, and runtime-API hosts run with terminal chrome off. Applies
    /// to every mode's surface and is inherited by child agents.
    fn request_plugin_install_allowed(&self) -> bool {
        self.config.terminal_chrome_enabled
            && codewhale_runtime::settings::Settings::load_read_only()
                .map(|settings| settings.contextual_tips)
                .unwrap_or(true)
    }

    #[cfg(test)]
    pub(super) fn build_turn_tool_registry_builder(
        &self,
        mode: AppMode,
        todo_list: SharedTodoList,
        plan_state: SharedPlanState,
    ) -> ToolRegistryBuilder {
        self.build_turn_tool_registry_builder_for_route(
            mode,
            self.session.allow_shell,
            self.codewhale_client.clone(),
            &self.session.model,
            todo_list,
            plan_state,
        )
    }

    /// Build the registry from the route and authority already resolved for
    /// this turn. Preview calls this before either is installed on the engine,
    /// so reading `self.session` here would describe the previous turn's shell
    /// posture, client, and model.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_turn_tool_registry_builder_for_route(
        &self,
        mode: AppMode,
        allow_shell: bool,
        client: Option<CodewhaleClient>,
        model: &str,
        todo_list: SharedTodoList,
        plan_state: SharedPlanState,
    ) -> ToolRegistryBuilder {
        let shell_policy = shell_policy_for_mode(mode, allow_shell);
        if mode != AppMode::Plan {
            let mut builder = ToolRegistryBuilder::new().with_agent_runtime_surface(
                client.clone(),
                model.to_string(),
                self.agent_tool_surface_options(shell_policy),
                todo_list,
                plan_state,
            );
            if self.config.features.enabled(Feature::Mcp) {
                builder = builder.with_registry_mcp_sync_tool();
            }
            // `start_mcp_server` belongs to every executable mode. Keep its
            // handler aligned with the model catalog, which always loads the
            // tool while MCP is enabled. The former early return registered
            // it only in Plan mode, so Agent/Full Access advertised a tool
            // that could never cross the execution boundary.
            if let Some(ref pool) = self.mcp_pool {
                builder = builder
                    .with_runtime_mcp_tool(Arc::clone(pool))
                    .with_registry_mcp_start_tool(Arc::clone(pool));
            }
            return builder;
        }

        let mut builder = ToolRegistryBuilder::new()
            // Modes change authority, not the primitive tool identity.
            // Plan advertises the same file names as Work; the turn-loop
            // mode gate below the schema boundary blocks write/edit, and
            // the ToolContext carries ShellPolicy::None.
            .with_file_tools()
            // Foreground-only shell registration: never add terminal/*
            // lifecycle tools to Plan merely to keep the `bash` identity
            // stable across modes.
            .with_foreground_shell_tools()
            .with_search_tools()
            // Compaction replaces history in Plan exactly as it does in Work,
            // so the archive read tools have to exist there too or the summary
            // header would point at names this mode never advertises.
            .with_compaction_archive_tools()
            .with_git_tools()
            .with_git_history_tools()
            .with_diagnostics_tool()
            .with_read_media_tool()
            .with_skill_tools()
            .with_validation_tools()
            .with_handle_tools()
            .with_runtime_read_only_task_tools()
            .with_todo_tool(todo_list)
            .with_plan_tool(plan_state)
            .with_goal_tools(self.config.goal_state.clone());

        builder = builder
            .with_review_tool(client.clone(), model.to_string())
            .with_user_input_tool(self.config.user_input_limits);

        if self.config.features.enabled(Feature::WebSearch) {
            builder = builder.with_web_tools();
        }

        // Register the `remember` tool only when the user has opted in to
        // user-memory (#489). Without that opt-in the tool would always
        // fail; surfacing it would just waste catalog slots.
        if should_register_remember_tool(self.config.memory_enabled) {
            builder = builder.with_remember_tool();
        }

        // Register image_analyze tool when vision_model is configured and feature enabled.
        if self.config.features.enabled(Feature::VisionModel)
            && let Some(ref vision_config) = self.config.vision_config
        {
            builder = builder.with_vision_tools(vision_config.clone(), client.clone());
        }

        // Register the `notify` tool unconditionally (#1322). Interactive and
        // headless entry points install the merged notification policy before
        // tool setup, including method=off, quiet/category, and attention.
        // The tool returns a truthful suppressed/delivered receipt.
        builder = builder.with_notify_tool();

        if self.request_plugin_install_allowed() {
            builder = builder.with_request_plugin_install_tool();
        }

        // Register the `registry_sync` tool for fetching and caching
        // MCP Registry server metadata. Rides on `Feature::Mcp` — the same
        // flag that gates the rest of the MCP system (defaults to enabled;
        // opt out via `[features]` in config.toml).
        if self.config.features.enabled(Feature::Mcp) {
            builder = builder.with_registry_mcp_sync_tool();
        }

        // Register the start_mcp_server tool so LLM can dynamically start
        // MCP servers from conversation context. Only when the pool has been
        // initialized (lazy via ensure_mcp_pool).
        if let Some(ref pool) = self.mcp_pool {
            builder = builder
                .with_runtime_mcp_tool(Arc::clone(pool))
                .with_registry_mcp_start_tool(Arc::clone(pool));
        }

        builder
    }
}

#[cfg(test)]
mod tests {
    use super::should_register_remember_tool;

    #[test]
    fn remember_tool_registration_requires_memory_opt_in() {
        assert!(should_register_remember_tool(true));
        assert!(!should_register_remember_tool(false));
    }
}
