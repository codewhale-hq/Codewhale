//! Remaining legacy core commands: clear, exit, model and related interactions.

use crate::config::{ProviderKind, normalize_custom_model_id, normalize_model_name_for_provider};
#[cfg(test)]
use crate::reasoning_preference::ReasoningEffort;
use crate::tui::app::{App, AppAction};
use crate::tui::views::{ModalKind, SubAgentsView, subagent_view_agents};
#[cfg(test)]
use codewhale_localization::Locale;
use codewhale_localization::{MessageId, tr};

use super::CommandResult;

/// Clear conversation history
pub fn clear(app: &mut App) -> CommandResult {
    if app.session_transition_blocked() {
        // The same gate the lifecycle commands consult, so the refusal owes
        // the same explanation. The sentence stays localized; a blocker row
        // is data (id, status, elapsed, summary) and is not translated
        // anywhere else either.
        let mut message = tr(app.ui_locale, MessageId::ClearConversationBusy).to_string();
        for blocker in app.session_transition_blockers() {
            message.push_str("\n  • ");
            message.push_str(&blocker);
        }
        return CommandResult::error(message);
    }
    let new_id = uuid::Uuid::new_v4().to_string();
    let queue_transition = match crate::tui::ui::prepare_offline_queue_transition(app, &new_id) {
        Ok(transition) => transition,
        Err(error) => return CommandResult::error(error),
    };
    if !reset_conversation_state(app) {
        return CommandResult::error(
            tr(app.ui_locale, MessageId::ClearConversationBusy).to_string(),
        );
    }
    // The App owns the session id: it keys the turn-start crash checkpoint
    // and every autosave, so mint the next id here (as `/new` does) rather
    // than letting the engine generate one the App only learns about from
    // `SessionUpdated`. Two ids for one conversation orphan the checkpoint.
    crate::tui::ui::install_offline_queue_transition(app, queue_transition);
    app.current_session_id = Some(new_id.clone());
    app.current_session_metadata = None;
    app.session_title = None;
    app.window_title = None;
    let locale = app.ui_locale;
    let message = tr(locale, MessageId::ClearConversation).to_string();
    CommandResult::with_message_and_action(
        message,
        AppAction::SyncSession {
            session_id: Some(new_id),
            messages: Vec::new(),
            system_prompt: None,
            model: app.model.clone(),
            workspace: app.workspace.clone(),
            mode: app.mode,
        },
    )
}

/// Reset the active conversation without choosing the next session id.
pub(crate) fn reset_conversation_state(app: &mut App) -> bool {
    // Work state is the only contended portion. Acquire and clear it first so
    // `/clear` and `/new` are all-or-nothing rather than losing conversation
    // state while leaving an old To-do attached to the next session.
    if !app.clear_todos() {
        return false;
    }
    // Explicit reset discards the queue it was invoked on. Capture its owner
    // before `/clear` or `/new` installs the next session id.
    if let Some(lease) = app.offline_queue_lease.clone() {
        crate::tui::persistence_actor::persist(
            crate::tui::persistence_actor::PersistRequest::ClearOfflineQueue { lease },
        );
    }
    // Atomically retire background accounting before zeroing the session.
    // Late reports retain the old scope token and are discarded instead of
    // appearing in the new conversation.
    let _settled_old_cost_scope = crate::cost_status::close_current_scope();
    app.clear_history();
    app.mark_history_updated();
    app.clear_api_messages();
    app.system_prompt = None;
    app.viewport.transcript_selection.clear();
    app.queued_messages.clear();
    app.held_restored_queue = 0;
    app.queued_draft = None;
    app.session.total_tokens = 0;
    app.session.total_conversation_tokens = 0;
    app.last_billed_input_tokens = None;
    app.session.reset_token_breakdown();
    app.session.session_cost = 0.0;
    app.session.session_cost_cny = 0.0;
    app.session.subagent_cost = 0.0;
    app.session.subagent_cost_cny = 0.0;
    app.session.subagent_usage_sources.clear();
    app.session.displayed_cost_high_water = 0.0;
    app.session.displayed_cost_high_water_cny = 0.0;
    app.reset_cost_coverage();
    app.tool_log.clear();
    app.tool_cells.clear();
    app.tool_details_by_cell.clear();
    app.exploring_entries.clear();
    app.ignored_tool_calls.clear();
    app.pending_tool_uses.clear();
    app.last_exec_wait_command = None;
    app.session.last_prompt_tokens = None;
    app.session.last_completion_tokens = None;
    app.session.last_prompt_cache_hit_tokens = None;
    app.session.last_prompt_cache_miss_tokens = None;
    app.session.last_reasoning_replay_tokens = None;
    app.session.turn_cache_history.clear();
    app.session.last_cache_inspection = None;
    app.session.last_warmup_key = None;
    app.session.last_tool_catalog = None;
    app.session.last_base_url = None;
    // A fresh conversation inherits neither this one's denials nor its
    // "approve for session" grants (UX-8).
    app.approval_session_denied.clear();
    app.approval_session_approved.clear();
    // Nor this one's child-agent approval cards, footer rows, or web-mirror
    // copies: the reset finalizes those children engine-side.
    crate::tui::pending_requests::clear_all(app);
    true
}

/// Exit the application
pub fn exit() -> CommandResult {
    CommandResult::action(AppAction::Quit)
}

/// Switch or view current model. With no argument, open the two-pane
/// picker (Pro/Flash + thinking effort) per #39 — gives users a discoverable
/// way to flip both knobs without memorising the docs.
pub fn model(app: &mut App, model_name: Option<&str>) -> CommandResult {
    // `/model router …` is `/router …` (#6525): one Router setup view.
    if let Some(name) = model_name.map(str::trim) {
        let (head, rest) = name.split_once(char::is_whitespace).unwrap_or((name, ""));
        if head.eq_ignore_ascii_case("router") {
            return super::router::router_command(Some(rest));
        }
    }
    if model_name.is_some_and(|name| name.eq_ignore_ascii_case("save-default")) {
        // Explicit persistence of the pending session route as the startup
        // default — only an explicit command can write settings after an
        // in-session route change.
        let message = app.apply_route_save_choice(
            crate::tui::views::route_save_prompt::RouteSaveChoice::SaveAsDefault,
        );
        return CommandResult::message(message);
    }
    if let Some(name) = model_name {
        // Manual Models.dev catalog refresh (#4187). Dispatched async so the
        // TUI event loop is not blocked; failure keeps prior/bundled rows.
        if name.trim().eq_ignore_ascii_case("refresh") {
            return CommandResult::action(AppAction::RefreshModelsDevCatalog);
        }
        if name.trim().eq_ignore_ascii_case("auto") {
            let old_model = app.model_display_label();
            let model_changed = !app.auto_model || app.model != "auto";
            app.set_model_selection("auto".to_string());
            app.active_route_limits = app.context_window_override_limits();
            app.update_model_compaction_budget();
            if model_changed {
                app.clear_model_scoped_telemetry();
            } else {
                app.session.last_prompt_tokens = None;
                app.session.last_completion_tokens = None;
            }
            let provider_identity = app.provider_identity_for_persistence().to_string();
            app.provider_models
                .insert(provider_identity.clone(), "auto".to_string());
            // Temporary by default; the route-save prompt owns persistence.
            app.note_session_route_change(&provider_identity, "auto");
            let mut message = tr(app.ui_locale, MessageId::ModelChanged)
                .replace("{old}", &old_model)
                .replace("{new}", "auto");
            message.push_str(&tr(app.ui_locale, MessageId::ModelChangedSessionNote));
            return CommandResult::with_message_and_action(
                message,
                AppAction::UpdateCompaction(app.compaction_config()),
            );
        }
        let declared = app.api_provider != ProviderKind::OpenaiCodex
            && codewhale_config::catalog::configured::validate_configured_models(
                &app.configured_models,
            )
            .is_ok()
            && app.configured_models.iter().any(|model| {
                model.id == name
                    && model.matches_route(
                        app.provider_identity_for_persistence(),
                        &app.active_route_base_url,
                    )
            });
        let model_id = if declared {
            name.to_string()
        } else if app.accepts_custom_model_ids() {
            let Some(model_id) = normalize_custom_model_id(name) else {
                return CommandResult::error(format!(
                    "Invalid model '{name}'. Expected a non-empty model ID."
                ));
            };
            model_id
        } else {
            let Some(model_id) = normalize_model_name_for_provider(app.api_provider, name) else {
                return CommandResult::error(format!(
                    "Invalid model '{name}'. Expected auto or a model for the active provider ({}).",
                    app.api_provider.as_str()
                ));
            };
            model_id
        };
        let strict_direct_custom_endpoint = app.accepts_custom_model_ids()
            && matches!(app.api_provider, ProviderKind::Deepseek | ProviderKind::Zai);
        let route_resolution = if declared {
            match crate::route_runtime::resolve_declared_model_candidate(
                app.api_provider,
                app.provider_identity_for_persistence(),
                &model_id,
                &app.active_route_base_url,
                app.active_context_window_override,
                app.active_model_context_windows.as_ref(),
                &app.configured_models,
            ) {
                Ok(resolution) => Some(resolution),
                Err(reason) => return CommandResult::error(reason),
            }
        } else if strict_direct_custom_endpoint {
            None
        } else {
            // `/model` normally resolves against the active provider's
            // catalog-default endpoint so it retains the existing local
            // provider/model validation. The one endpoint-sensitive model
            // selection is Kimi Code's bare `k3`: resolve it against the
            // committed Kimi Code route rather than silently falling back to
            // Moonshot's direct API route. Do not pass an unrelated stale
            // endpoint through here; doing so would turn a foreign provider
            // model into an apparent custom-route selection.
            let route_base_url = crate::config::is_exact_kimi_code_k3_route(
                app.api_provider,
                &app.active_route_base_url,
                &model_id,
            )
            .then(|| app.active_route_base_url.clone());
            match crate::route_runtime::resolve_route_candidate_with_context_metadata(
                app.api_provider,
                Some(&model_id),
                None,
                route_base_url,
                app.active_context_window_override,
                app.active_model_context_windows.as_ref(),
                None,
            ) {
                Ok(resolution) => Some(resolution),
                Err(reason) => return CommandResult::error(reason),
            }
        };
        let old_model = app.model_display_label();
        let model_changed = app.auto_model || app.model != model_id;
        app.set_model_selection(model_id.clone());
        if let Some(resolution) = route_resolution {
            app.set_active_route_resolution(
                resolution.candidate.endpoint().base_url.clone(),
                resolution.candidate.limits(),
                resolution.context_window.source,
            );
        } else {
            app.active_route_limits = app.context_window_override_limits();
            app.active_context_window_source = app
                .configured_context_window_for(&app.model)
                .map(|resolution| resolution.source)
                .unwrap_or(crate::route_runtime::ContextWindowSource::Fallback);
        }
        app.update_model_compaction_budget();
        if model_changed {
            app.clear_model_scoped_telemetry();
        } else {
            app.session.last_prompt_tokens = None;
            app.session.last_completion_tokens = None;
        }
        let provider_identity = app.provider_identity_for_persistence().to_string();
        app.provider_models
            .insert(provider_identity.clone(), model_id.clone());
        app.note_route_used(&provider_identity, &model_id);
        // Route changes are temporary by default: nothing is written here.
        // The route-save prompt offers the explicit persistence choices.
        app.note_session_route_change(&provider_identity, &model_id);
        let mut message = tr(app.ui_locale, MessageId::ModelChanged)
            .replace("{old}", &old_model)
            .replace("{new}", &model_id);
        message.push_str(&tr(app.ui_locale, MessageId::ModelChangedSessionNote));
        CommandResult::with_message_and_action(
            message,
            AppAction::UpdateCompaction(app.compaction_config()),
        )
    } else {
        CommandResult::action(AppAction::OpenModelPicker)
    }
}

/// Fetch and list available models from the configured API endpoint.
pub fn models(_app: &mut App) -> CommandResult {
    CommandResult::action(AppAction::FetchModels)
}

/// List Fleet worker status from the engine.
/// Request a refresh and print the agent roster into the transcript once it
/// lands. One-shot: the flag is cleared by the event handler, so ambient
/// refreshes (sidebar polls, spawn/complete events) never spam the transcript.
pub fn subagents_roster(app: &mut App) -> CommandResult {
    app.agent_roster_print_requested = true;
    app.status_message = Some(tr(app.ui_locale, MessageId::SubagentsFetching).to_string());
    CommandResult::action(AppAction::ListSubAgents)
}

pub fn subagents(app: &mut App) -> CommandResult {
    if app.view_stack.top_kind() != Some(ModalKind::SubAgents) {
        let agents = subagent_view_agents(app, &app.subagent_cache);
        app.view_stack.push(SubAgentsView::for_app(app, agents));
    }
    app.status_message = Some(tr(app.ui_locale, MessageId::SubagentsFetching).to_string());
    CommandResult::action(AppAction::ListSubAgents)
}

/// Switch to a configured profile.
pub fn profile_switch(app: &mut App, arg: Option<&str>) -> CommandResult {
    let profile_name = match arg {
        Some(name) if !name.trim().is_empty() => name.trim().to_string(),
        _ => {
            return CommandResult::error(tr(app.ui_locale, MessageId::ProfileUsage));
        }
    };
    CommandResult::with_message_and_action(
        tr(app.ui_locale, MessageId::ProfileSwitching).replace("{name}", &profile_name),
        AppAction::SwitchProfile {
            profile: profile_name,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::PromptInspection;
    use crate::config::Config;
    use crate::tui::app::{App, TuiOptions, TurnCacheRecord};
    use crate::tui::history::HistoryCell;
    use codewhale_models::Message;
    use codewhale_models::Role;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::Instant;
    use tempfile::TempDir;

    struct SettingsPathGuard {
        _tmp: TempDir,
        previous: Option<OsString>,
        _lock: crate::test_support::TestEnvLock,
    }

    impl SettingsPathGuard {
        fn new() -> Self {
            let lock = crate::test_support::lock_test_env();
            let tmp = TempDir::new().expect("settings tempdir");
            let config_path = tmp.path().join(".deepseek").join("config.toml");
            std::fs::create_dir_all(config_path.parent().expect("config parent"))
                .expect("config dir");
            let previous = std::env::var_os("DEEPSEEK_CONFIG_PATH");
            // Safety: test-only environment mutation guarded by a global mutex.
            unsafe {
                std::env::set_var("DEEPSEEK_CONFIG_PATH", &config_path);
            }
            Self {
                _tmp: tmp,
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for SettingsPathGuard {
        fn drop(&mut self) {
            // Safety: test-only environment mutation guarded by a global mutex.
            unsafe {
                if let Some(previous) = self.previous.take() {
                    std::env::set_var("DEEPSEEK_CONFIG_PATH", previous);
                } else {
                    std::env::remove_var("DEEPSEEK_CONFIG_PATH");
                }
            }
        }
    }

    fn create_test_app() -> App {
        let options = TuiOptions {
            skills_dir: PathBuf::from("/tmp/test-skills"),
            ..crate::tui::test_support::test_tui_options(PathBuf::from("/tmp/test-workspace"))
        };
        let mut app = App::new(options, &Config::default());
        app.ui_locale = codewhale_localization::Locale::En;
        app.api_provider = crate::config::ProviderKind::Deepseek;
        app.model = "deepseek-v4-pro".to_string();
        app.auto_model = false;
        app.model_ids_passthrough = false;
        app
    }

    #[test]
    fn profile_command_localizes_replies_and_preserves_switch_action() {
        let _env = crate::test_support::lock_test_env();
        for (locale, usage, switching) in [
            (
                Locale::En,
                "Usage: /profile <name>",
                "Switching to profile 'work'...",
            ),
            (
                Locale::ZhHans,
                "用法: /profile <name>",
                "正在切换到配置文件 'work'...",
            ),
            (
                Locale::De,
                "Verwendung: /profile <name>",
                "Wechsle zum Profil 'work'...",
            ),
        ] {
            let mut app = create_test_app();
            app.ui_locale = locale;
            let previous_profile = app.config_profile.clone();

            for command in ["/profile", "/profile   "] {
                let result = crate::commands::execute(command, &mut app);
                assert!(result.is_error, "{locale:?}: {command:?}");
                assert!(result.action.is_none(), "{locale:?}: {command:?}");
                let message = result.message.expect("profile usage");
                assert!(message.contains(usage), "{locale:?}: {message}");
                assert!(message.contains("~/.codewhale/config.toml"), "{message}");
                assert!(message.contains("[profiles]"), "{message}");
            }

            let result = crate::commands::execute("/profile   work  ", &mut app);
            assert!(!result.is_error, "{locale:?}");
            assert_eq!(result.message.as_deref(), Some(switching), "{locale:?}");
            assert!(matches!(
                result.action,
                Some(AppAction::SwitchProfile { profile }) if profile == "work"
            ));
            assert_eq!(app.config_profile, previous_profile, "{locale:?}");
        }
    }

    #[test]
    fn test_clear_resets_all_state() {
        let mut app = create_test_app();
        // Set up some state
        app.history.push(HistoryCell::User {
            content: "test".to_string(),
        });
        app.api_messages_mut().push(Message {
            role: Role::User,
            content: vec![],
        });
        app.session.total_conversation_tokens = 100;
        app.tool_log.push("test".to_string());
        app.current_session_id = Some("existing-session".to_string());
        app.session_artifacts
            .push(crate::artifacts::ArtifactRecord {
                id: "art_call_big".to_string(),
                kind: crate::artifacts::ArtifactKind::ToolOutput,
                session_id: "existing-session".to_string(),
                tool_call_id: "call-big".to_string(),
                tool_name: "exec_shell".to_string(),
                created_at: chrono::Utc::now(),
                byte_size: 128,
                preview: "tool output".to_string(),
                storage_path: PathBuf::from("/tmp/tool_outputs/call-big.txt"),
            });

        let result = clear(&mut app);
        assert!(result.message.is_some());
        assert!(app.history.is_empty());
        assert!(app.api_messages.is_empty());
        assert_eq!(app.session.total_conversation_tokens, 0);
        assert!(app.tool_log.is_empty());
        assert!(app.tool_cells.is_empty());
        assert!(app.tool_details_by_cell.is_empty());
        assert!(app.session_artifacts.is_empty());
        // The App mints the next session id itself so the engine and every
        // checkpoint/autosave share one id (no orphaned checkpoint).
        let next_id = app
            .current_session_id
            .clone()
            .expect("/clear claims the next session id");
        assert_ne!(next_id, "existing-session");
        assert!(uuid::Uuid::parse_str(&next_id).is_ok(), "{next_id}");
        match result.action {
            Some(AppAction::SyncSession {
                session_id,
                messages,
                ..
            }) => {
                assert_eq!(session_id.as_deref(), Some(next_id.as_str()));
                assert!(messages.is_empty());
            }
            other => panic!("expected SyncSession, got {other:?}"),
        }
    }

    #[test]
    fn clear_is_all_or_nothing_when_work_state_is_busy() {
        let mut app = create_test_app();
        app.history.push(HistoryCell::User {
            content: "keep me".to_string(),
        });
        app.api_messages_mut().push(Message {
            role: Role::User,
            content: vec![],
        });
        app.current_session_id = Some("current-session".to_string());
        let plan_state = app.plan_state.clone();
        let _held = plan_state.try_lock().expect("hold plan lock");

        let result = clear(&mut app);

        assert!(result.is_error);
        assert!(result.action.is_none());
        assert_eq!(app.history.len(), 1);
        assert_eq!(app.api_messages.len(), 1);
        assert_eq!(app.current_session_id.as_deref(), Some("current-session"));
        assert!(result.message.as_deref().is_some_and(|message| {
            message.contains("Nothing cleared") && message.contains("busy")
        }));
    }

    #[test]
    fn clear_rejects_an_active_turn_without_mutating_session_state() {
        let mut app = create_test_app();
        app.history.push(HistoryCell::User {
            content: "keep active turn".to_string(),
        });
        app.api_messages_mut().push(Message {
            role: Role::User,
            content: vec![],
        });
        app.current_session_id = Some("active-session".to_string());
        app.is_loading = true;
        app.runtime_turn_status = Some("in_progress".to_string());

        let result = clear(&mut app);

        assert!(result.is_error);
        assert!(result.action.is_none());
        assert_eq!(app.history.len(), 1);
        assert_eq!(app.api_messages.len(), 1);
        assert_eq!(app.current_session_id.as_deref(), Some("active-session"));
    }

    #[test]
    fn clear_resets_session_telemetry() {
        let mut app = create_test_app();
        app.session.total_tokens = 234;
        app.session.total_conversation_tokens = 123;
        app.session.session_cost = 0.42;
        app.session.session_cost_cny = 3.05;
        app.session.subagent_cost = 0.11;
        app.session.subagent_cost_cny = 0.80;
        app.session
            .subagent_usage_sources
            .insert(crate::cost_status::usage_source_fingerprint(
                "response-test",
            ));
        app.session.displayed_cost_high_water = 0.53;
        app.session.displayed_cost_high_water_cny = 3.85;
        app.session.last_prompt_cache_hit_tokens = Some(70);
        app.session.last_prompt_cache_miss_tokens = Some(30);
        app.session.last_reasoning_replay_tokens = Some(12);
        app.session.total_cache_write_tokens = 99;
        app.session.last_warmup_key = None;
        app.session.last_tool_catalog = Some(Vec::new());
        app.session.last_base_url = Some("https://api.deepseek.com".to_string());
        app.session.last_cache_inspection = Some(PromptInspection {
            base_static_prefix_hash: "base".to_string(),
            full_request_prefix_hash: "full".to_string(),
            tool_catalog_hash: String::new(),
            layers: Vec::new(),
        });
        app.push_turn_cache_record(TurnCacheRecord {
            provider: None,
            provider_identity: None,
            model: None,
            auto_model: false,
            input_tokens: 100,
            output_tokens: 25,
            cache_hit_tokens: Some(70),
            cache_miss_tokens: Some(30),
            reasoning_replay_tokens: Some(12),
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost_audit: None,
            recorded_at: Instant::now(),
        });

        clear(&mut app);

        assert_eq!(app.session.total_tokens, 0);
        assert_eq!(app.session.total_conversation_tokens, 0);
        assert_eq!(app.session.session_cost, 0.0);
        assert_eq!(app.session.session_cost_cny, 0.0);
        assert_eq!(app.session.subagent_cost, 0.0);
        assert_eq!(app.session.subagent_cost_cny, 0.0);
        assert!(app.session.subagent_usage_sources.is_empty());
        assert_eq!(app.session.displayed_cost_high_water, 0.0);
        assert_eq!(app.session.displayed_cost_high_water_cny, 0.0);
        assert_eq!(app.session.last_prompt_cache_hit_tokens, None);
        assert_eq!(app.session.last_prompt_cache_miss_tokens, None);
        assert_eq!(app.session.last_reasoning_replay_tokens, None);
        assert_eq!(app.session.total_cache_write_tokens, 0);
        assert!(app.session.turn_cache_history.is_empty());
        assert_eq!(app.session.last_cache_inspection, None);
        assert_eq!(app.session.last_warmup_key, None);
        assert_eq!(app.session.last_tool_catalog, None);
        assert_eq!(app.session.last_base_url, None);
    }

    #[test]
    fn test_exit_returns_quit_action() {
        let result = exit();
        assert!(result.message.is_none());
        assert!(matches!(result.action, Some(AppAction::Quit)));
    }

    #[test]
    fn test_model_change_updates_state() {
        let _settings = SettingsPathGuard::new();
        let mut app = create_test_app();
        let old_model = app.model.clone();
        let result = model(&mut app, Some("deepseek-v4-flash"));
        assert!(result.message.is_some());
        let msg = result.message.unwrap();
        assert!(msg.contains(&old_model));
        assert!(msg.contains("deepseek-v4-flash"));
        assert!(matches!(
            result.action,
            Some(AppAction::UpdateCompaction(_))
        ));
        assert_eq!(app.model, "deepseek-v4-flash");
        assert_eq!(app.session.last_prompt_tokens, None);
        assert_eq!(app.session.last_completion_tokens, None);
    }

    #[test]
    fn model_command_preserves_active_kimi_code_endpoint_for_bare_k3() {
        let _settings = SettingsPathGuard::new();
        let mut app = create_test_app();
        app.set_provider_identity(crate::config::ProviderKind::Moonshot, "moonshot");
        app.model_ids_passthrough = true;
        app.active_route_base_url = crate::config::DEFAULT_KIMI_CODE_BASE_URL.to_string();
        app.active_context_window_override = None;

        let result = model(&mut app, Some(crate::config::KIMI_CODE_K3_MODEL));

        assert!(
            !result.is_error,
            "Kimi Code K3 route should resolve: {result:?}"
        );
        assert_eq!(app.model, crate::config::KIMI_CODE_K3_MODEL);
        assert_eq!(
            app.active_route_limits
                .and_then(|limits| limits.context_tokens),
            Some(u64::from(crate::config::KIMI_CODE_K3_CONTEXT_WINDOW_TOKENS))
        );

        // Switching to the direct platform endpoint requires the direct model
        // id (`kimi-k3`); bare `k3` is fail-closed (#4687).
        app.active_route_base_url = crate::config::DEFAULT_MOONSHOT_BASE_URL.to_string();
        let rejected = model(&mut app, Some(crate::config::KIMI_CODE_K3_MODEL));
        assert!(
            rejected.is_error,
            "bare k3 on direct Moonshot must fail closed: {rejected:?}"
        );

        let direct = model(&mut app, Some(crate::config::MOONSHOT_KIMI_K3_MODEL));
        assert!(
            !direct.is_error,
            "direct Moonshot kimi-k3 remains valid: {direct:?}"
        );
        assert_ne!(
            app.active_route_limits
                .and_then(|limits| limits.context_tokens),
            Some(u64::from(crate::config::KIMI_CODE_K3_CONTEXT_WINDOW_TOKENS))
        );
    }

    #[test]
    fn model_command_is_session_local_until_explicitly_saved() {
        let _settings = SettingsPathGuard::new();
        let mut app = create_test_app();

        let result = model(&mut app, Some("deepseek-v4-flash"));

        assert!(result.message.is_some());
        assert_eq!(
            app.provider_models.get("deepseek").map(String::as_str),
            Some("deepseek-v4-flash")
        );
        // The live session changed and the save decision is pending — the
        // route-save prompt owns persistence now.
        let pending = app.pending_route_save.as_ref().expect("pending save");
        assert_eq!(pending.provider_identity, "deepseek");
        assert_eq!(pending.model, "deepseek-v4-flash");

        // NOTHING was written to settings: no scoped model, no default
        // provider, no default model. The message says the change is
        // session-only.
        let settings = crate::settings::Settings::load().expect("load settings");
        assert_eq!(
            settings
                .provider_models
                .as_ref()
                .and_then(|models| models.get("deepseek")),
            None
        );
        assert_eq!(settings.default_provider.as_deref(), None);
        assert_eq!(settings.default_model.as_deref(), None);
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("session only"),
            "the receipt must say the change is temporary: {:?}",
            result.message
        );
    }

    #[test]
    fn model_command_does_not_mutate_shared_default_provider() {
        // Regression for #3227, strengthened: a `/model` change must not drag
        // the global `default_provider` onto it AND must not write the scoped
        // model either — the change is session-local until the user explicitly
        // saves it via the route-save prompt.
        let _settings = SettingsPathGuard::new();
        let startup_config = crate::config::home_config_path().expect("isolated startup config");
        let startup_before = std::fs::read(&startup_config).ok();
        {
            let seed = crate::settings::Settings {
                default_provider: Some("deepseek".to_string()),
                ..Default::default()
            };
            seed.save().expect("seed settings");
        }
        let mut app = create_test_app();
        app.set_provider_identity_record(
            crate::config::Config::default()
                .resolve_provider_identity(crate::config::ProviderKind::Zai.as_str())
                .expect("captured fixture provider"),
        );
        app.model_ids_passthrough = false;
        app.model = crate::config::DEFAULT_ZAI_MODEL.to_string();
        app.auto_model = false;

        let result = model(&mut app, Some("GLM-5.2"));
        assert!(result.message.is_some(), "expected a model-changed message");
        assert!(!result.is_error, "GLM-5.2 is valid on Z.ai");

        let settings = crate::settings::Settings::load().expect("load settings");
        // Neither canonical startup config nor the legacy archive changes.
        assert_eq!(std::fs::read(startup_config).ok(), startup_before);
        assert_eq!(settings.default_provider.as_deref(), Some("deepseek"));
        // No scoped entry was written either — session-local.
        assert_eq!(
            settings
                .provider_models
                .as_ref()
                .and_then(|models| models.get("zai")),
            None
        );
        // The in-memory route changed and the decision is pending.
        assert_eq!(app.model, "GLM-5.2");
        let pending = app.pending_route_save.as_ref().expect("pending save");
        assert_eq!(pending.provider_identity, "zai");
    }

    #[test]
    fn model_command_keeps_glm_53_as_its_own_wire_id() {
        let _settings = SettingsPathGuard::new();
        let mut app = create_test_app();
        app.set_provider_identity_record(
            crate::config::Config::default()
                .resolve_provider_identity(crate::config::ProviderKind::Zai.as_str())
                .expect("captured fixture provider"),
        );
        app.model_ids_passthrough = false;
        app.model = crate::config::ZAI_GLM_5_2_MODEL.to_string();
        app.auto_model = false;

        let result = model(&mut app, Some("glm-5.3"));

        assert!(!result.is_error, "GLM-5.3 is valid on Z.ai: {result:?}");
        assert_eq!(app.model, crate::config::ZAI_GLM_5_3_MODEL);
        assert_eq!(
            app.provider_models.get("zai").map(String::as_str),
            Some(crate::config::ZAI_GLM_5_3_MODEL)
        );
        assert_eq!(
            app.pending_route_save
                .as_ref()
                .map(|pending| pending.model.as_str()),
            Some(crate::config::ZAI_GLM_5_3_MODEL)
        );
    }

    #[test]
    fn two_sessions_keep_independent_provider_model_routes() {
        // #3227: two App instances sharing one settings/config path. A is on
        // Z.ai/GLM; B switches to DeepSeek and picks a DeepSeek model. B must
        // build a DeepSeek route (not Z.ai + a DeepSeek model), A must stay on
        // Z.ai/GLM, and neither session's `/model` may flip the shared global
        // default provider out from under the other.
        let _settings = SettingsPathGuard::new();

        // Terminal A: Z.ai / GLM.
        let mut app_a = create_test_app();
        app_a.api_provider = crate::config::ProviderKind::Zai;
        app_a.model_ids_passthrough = false;
        app_a.model = crate::config::DEFAULT_ZAI_MODEL.to_string();
        app_a.auto_model = false;
        let result_a = model(&mut app_a, Some("GLM-5.2"));
        assert!(!result_a.is_error, "GLM-5.2 is valid on Z.ai");
        assert_eq!(app_a.api_provider, crate::config::ProviderKind::Zai);
        assert_eq!(app_a.model, "GLM-5.2");

        // Terminal B: DeepSeek / deepseek-v4-flash.
        let mut app_b = create_test_app();
        app_b.api_provider = crate::config::ProviderKind::Deepseek;
        app_b.model_ids_passthrough = false;
        app_b.model = "deepseek-v4-pro".to_string();
        app_b.auto_model = false;
        let result_b = model(&mut app_b, Some("deepseek-v4-flash"));
        assert!(!result_b.is_error, "deepseek-v4-flash is valid on DeepSeek");

        // B's route is a coherent DeepSeek route — never Z.ai + a DeepSeek model.
        assert_eq!(app_b.api_provider, crate::config::ProviderKind::Deepseek);
        assert_eq!(app_b.model, "deepseek-v4-flash");

        // A is untouched by B's selection — still Z.ai / GLM.
        assert_eq!(app_a.api_provider, crate::config::ProviderKind::Zai);
        assert_eq!(app_a.model, "GLM-5.2");

        // Shared settings: NOTHING was written by either `/model` — both
        // changes are session-local with a pending save decision, and the
        // global default provider was never flipped.
        let settings = crate::settings::Settings::load().expect("load settings");
        assert_eq!(settings.default_provider.as_deref(), None);
        assert_eq!(settings.provider_models, None);
        // Both sessions carry their own pending save decisions.
        assert_eq!(
            app_a.pending_route_save.as_ref().map(|p| p.model.as_str()),
            Some("GLM-5.2")
        );
        assert_eq!(
            app_b.pending_route_save.as_ref().map(|p| p.model.as_str()),
            Some("deepseek-v4-flash")
        );
    }

    #[test]
    fn model_command_rejects_model_foreign_to_active_provider() {
        // #3227: a DeepSeek model id requested while the session is on Z.ai is
        // rejected locally with a precise diagnostic, before any network call.
        let _settings = SettingsPathGuard::new();
        let mut app = create_test_app();
        app.api_provider = crate::config::ProviderKind::Zai;
        app.model_ids_passthrough = false;
        app.model = crate::config::DEFAULT_ZAI_MODEL.to_string();
        app.auto_model = false;
        app.provider_models.clear();

        let result = model(&mut app, Some("deepseek-v4-pro"));

        assert!(result.is_error, "expected a local rejection");
        let msg = result.message.expect("error message");
        assert!(msg.contains("deepseek-v4-pro"), "names the model: {msg}");
        assert!(msg.contains("zai"), "names the provider: {msg}");
        // The session route is unchanged — still Z.ai / GLM.
        assert_eq!(app.api_provider, crate::config::ProviderKind::Zai);
        assert_eq!(app.model, crate::config::DEFAULT_ZAI_MODEL);
    }

    #[test]
    fn model_switch_clears_turn_cache_history() {
        let _settings = SettingsPathGuard::new();
        let mut app = create_test_app();
        // Keep the assertion independent of the developer's saved default model.
        app.auto_model = false;
        app.model = "deepseek-v4-pro".to_string();
        app.push_turn_cache_record(TurnCacheRecord {
            provider: None,
            provider_identity: None,
            model: None,
            auto_model: false,
            input_tokens: 100,
            output_tokens: 25,
            cache_hit_tokens: Some(70),
            cache_miss_tokens: Some(30),
            reasoning_replay_tokens: Some(12),
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost_audit: None,
            recorded_at: Instant::now(),
        });

        let result = model(&mut app, Some("deepseek-v4-flash"));

        assert!(result.message.is_some());
        assert!(app.session.turn_cache_history.is_empty());
    }

    #[test]
    fn model_reset_same_model_keeps_turn_cache_history() {
        let _settings = SettingsPathGuard::new();
        let mut app = create_test_app();
        app.auto_model = false;
        app.model = "deepseek-v4-pro".to_string();
        app.push_turn_cache_record(TurnCacheRecord {
            provider: None,
            provider_identity: None,
            model: None,
            auto_model: false,
            input_tokens: 100,
            output_tokens: 25,
            cache_hit_tokens: Some(70),
            cache_miss_tokens: Some(30),
            reasoning_replay_tokens: Some(12),
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost_audit: None,
            recorded_at: Instant::now(),
        });

        let result = model(&mut app, Some("deepseek-v4-pro"));

        assert!(result.message.is_some());
        assert_eq!(app.session.turn_cache_history.len(), 1);
    }

    #[test]
    fn test_model_auto_enables_auto_thinking() {
        let _settings = SettingsPathGuard::new();
        let mut app = create_test_app();
        app.reasoning_effort = ReasoningEffort::Off;
        app.reasoning_effort_preference = None;

        let result = model(&mut app, Some("auto"));

        assert!(result.message.is_some());
        assert!(app.auto_model);
        assert_eq!(app.model, "auto");
        assert_eq!(app.reasoning_effort, ReasoningEffort::Auto);
        assert!(app.last_effective_model.is_none());
        assert!(app.last_effective_reasoning_effort.is_none());
    }

    #[test]
    fn test_model_auto_preserves_raw_explicit_thinking() {
        let _settings = SettingsPathGuard::new();
        let mut app = create_test_app();
        app.api_provider = ProviderKind::OpenaiCodex;
        app.auto_model = false;
        app.reasoning_effort = ReasoningEffort::Low;
        app.reasoning_effort_preference = Some(ReasoningEffort::Off);

        let result = model(&mut app, Some("auto"));

        assert!(result.message.is_some());
        assert!(app.auto_model);
        assert_eq!(app.model, "auto");
        assert_eq!(app.reasoning_effort, ReasoningEffort::Off);
        assert_eq!(app.reasoning_effort_preference, Some(ReasoningEffort::Off));
    }

    #[test]
    fn test_model_change_accepts_future_deepseek_model() {
        let _settings = SettingsPathGuard::new();
        let mut app = create_test_app();
        let result = model(&mut app, Some("deepseek-v4"));
        assert!(result.message.is_some());
        let msg = result.message.unwrap();
        assert!(msg.contains("deepseek-v4"));
        assert_eq!(app.model, "deepseek-v4");
        assert!(matches!(
            result.action,
            Some(AppAction::UpdateCompaction(_))
        ));
    }

    #[test]
    fn test_model_change_accepts_custom_id_for_openai_compatible_provider() {
        let _settings = SettingsPathGuard::new();
        let mut app = create_test_app();
        app.api_provider = crate::config::ProviderKind::Openai;
        app.model_ids_passthrough = true;

        let result = model(&mut app, Some("opencode-go/glm-5.1"));

        assert!(result.message.is_some());
        assert_eq!(app.model, "opencode-go/glm-5.1");
        assert!(!app.auto_model);
        assert!(matches!(
            result.action,
            Some(AppAction::UpdateCompaction(_))
        ));
    }

    #[test]
    fn test_model_change_accepts_custom_id_for_custom_base_url() {
        let _settings = SettingsPathGuard::new();
        let mut app = create_test_app();
        app.model_ids_passthrough = true;

        let result = model(&mut app, Some("opencode-go/kimi-k2.6"));

        assert!(result.message.is_some());
        assert_eq!(app.model, "opencode-go/kimi-k2.6");
        assert!(matches!(
            result.action,
            Some(AppAction::UpdateCompaction(_))
        ));
    }

    #[test]
    fn test_model_change_rejects_invalid_model() {
        let mut app = create_test_app();
        let result = model(&mut app, Some("gpt-4"));
        assert!(result.message.is_some());
        let msg = result.message.unwrap();
        assert!(msg.contains("Invalid model"));
        assert!(msg.contains("active provider"));
        assert!(msg.contains("deepseek"));
        assert!(!msg.contains("Common DeepSeek models"));
        assert!(result.action.is_none());
    }

    #[test]
    fn model_command_rejects_saved_model_from_other_provider() {
        let mut app = create_test_app();
        app.api_provider = crate::config::ProviderKind::Deepseek;
        app.provider_models
            .insert("moonshot".to_string(), "kimi-k2.6".to_string());

        let result = model(&mut app, Some("kimi-k2.6"));

        let message = result.message.expect("invalid model message");
        assert!(message.contains("Invalid model"));
        assert!(message.contains("active provider"));
        assert!(result.action.is_none());
        assert_eq!(app.api_provider, crate::config::ProviderKind::Deepseek);
        assert_eq!(app.model, "deepseek-v4-pro");
    }

    #[test]
    fn test_model_without_args_opens_picker() {
        let mut app = create_test_app();
        let result = model(&mut app, None);
        assert_eq!(result.message, None);
        assert_eq!(result.action, Some(AppAction::OpenModelPicker));
    }

    #[test]
    fn test_models_triggers_fetch_action() {
        let mut app = create_test_app();
        let result = models(&mut app);
        assert!(result.message.is_none());
        assert!(matches!(result.action, Some(AppAction::FetchModels)));
    }

    #[test]
    fn model_refresh_dispatches_models_dev_catalog_action() {
        let mut app = create_test_app();
        let result = model(&mut app, Some("refresh"));
        assert!(result.message.is_none());
        assert!(matches!(
            result.action,
            Some(AppAction::RefreshModelsDevCatalog)
        ));
    }

    #[test]
    fn test_subagents_pushes_view_and_sets_status() {
        let mut app = create_test_app();
        let result = subagents(&mut app);
        assert!(result.message.is_none());
        assert!(matches!(result.action, Some(AppAction::ListSubAgents)));
        assert_eq!(app.view_stack.top_kind(), Some(ModalKind::SubAgents));
        assert_eq!(
            app.status_message,
            Some("Finding this session's agents...".to_string())
        );
    }

    #[test]
    fn provider_link_metadata_marks_custom_routes_as_configuration_owned() {
        let links =
            codewhale_config::provider::provider_for_kind(codewhale_config::ProviderKind::Custom)
                .credential_help();

        assert_eq!(
            links.acquisition,
            codewhale_config::provider::CredentialAcquisition::Configuration
        );
        assert_eq!(links.docs_url, None);
        assert_eq!(links.credential_url, None);
    }
}
