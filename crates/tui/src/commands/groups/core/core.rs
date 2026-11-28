use crate::tui::app::AppAction;
use crate::tui::views::ModalKind;
use codewhale_config::ProviderKind;
use codewhale_engine::reasoning_preference::ReasoningEffort;

use super::CommandResult;

#[cfg(test)]
mod tests {
    use super::*;
    fn model(app: &mut App, arg: Option<&str>) -> CommandResult {
        crate::commands::execute(&format!("/model {}", arg.unwrap_or_default()), app)
    }
    use crate::tui::app::{App, TuiOptions, TurnCacheRecord};
    use crate::tui::history::HistoryCell;
    use codewhale_engine::client::PromptInspection;
    use codewhale_localization::Locale;
    use codewhale_models::Message;
    use codewhale_models::Role;
    use codewhale_runtime::config::Config;
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
        app.api_provider = codewhale_config::ProviderKind::Deepseek;
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
                "用法：/profile <name>",
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
    fn effort_alias_rejects_unknown_input_without_committing_an_effort() {
        let mut app = create_test_app();
        let before = app.reasoning_effort;
        let invalid = crate::commands::execute("/thinking imaginary-tier", &mut app);
        assert!(invalid.is_error);
        assert!(invalid.message.unwrap().contains("Usage: /effort"));
        assert_eq!(app.reasoning_effort, before);
        let receipt = crate::commands::execute("/effort high", &mut app);
        assert!(!receipt.is_error);
        assert_eq!(receipt.message.as_deref(), Some("Effort set to high"));
        assert_eq!(app.reasoning_effort, ReasoningEffort::High);
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
            .push(codewhale_runtime::artifacts::ArtifactRecord {
                id: "art_call_big".to_string(),
                kind: codewhale_runtime::artifacts::ArtifactKind::ToolOutput,
                session_id: "existing-session".to_string(),
                tool_call_id: "call-big".to_string(),
                tool_name: "exec_shell".to_string(),
                created_at: chrono::Utc::now(),
                byte_size: 128,
                preview: "tool output".to_string(),
                storage_path: PathBuf::from("/tmp/tool_outputs/call-big.txt"),
            });

        let result = crate::commands::execute("/clear", &mut app);
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
        assert!(app.session_write_receipt.is_some());
        assert!(app.session_write_owner.is_some());
        assert_eq!(
            app.offline_queue_lease.as_ref().unwrap().session_id(),
            next_id
        );
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

        let result = crate::commands::execute("/clear", &mut app);

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

        let result = crate::commands::execute("/clear", &mut app);

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
        app.session.subagent_usage_sources.insert(
            codewhale_engine::cost_status::usage_source_fingerprint("response-test"),
        );
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

        crate::commands::execute("/clear", &mut app);

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
        let result = crate::commands::execute("/exit", &mut create_test_app());
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
        app.set_provider_identity(codewhale_config::ProviderKind::Moonshot, "moonshot");
        app.model_ids_passthrough = true;
        app.active_route_base_url =
            codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL.to_string();
        app.active_context_window_override = None;

        let result = model(
            &mut app,
            Some(codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL),
        );

        assert!(
            !result.is_error,
            "Kimi Code K3 route should resolve: {result:?}"
        );
        assert_eq!(
            app.model,
            codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL
        );
        assert_eq!(
            app.active_route_limits
                .and_then(|limits| limits.context_tokens),
            Some(u64::from(
                codewhale_models::KIMI_CODE_K3_CONTEXT_WINDOW_TOKENS
            ))
        );

        // Switching to the direct platform endpoint requires the direct model
        // id (`kimi-k3`); bare `k3` is fail-closed (#4687).
        app.active_route_base_url =
            codewhale_config::descriptors::defaults::DEFAULT_MOONSHOT_BASE_URL.to_string();
        let rejected = model(
            &mut app,
            Some(codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL),
        );
        assert!(
            rejected.is_error,
            "bare k3 on direct Moonshot must fail closed: {rejected:?}"
        );

        let direct = model(
            &mut app,
            Some(codewhale_config::catalog::reviewed::constants::MOONSHOT_KIMI_K3_MODEL),
        );
        assert!(
            !direct.is_error,
            "direct Moonshot kimi-k3 remains valid: {direct:?}"
        );
        assert_ne!(
            app.active_route_limits
                .and_then(|limits| limits.context_tokens),
            Some(u64::from(
                codewhale_models::KIMI_CODE_K3_CONTEXT_WINDOW_TOKENS
            ))
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
        let settings = codewhale_runtime::settings::Settings::load().expect("load settings");
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
        let startup_config =
            codewhale_runtime::config::home_config_path().expect("isolated startup config");
        let startup_before = std::fs::read(&startup_config).ok();
        {
            let seed = {
                let __cw_config_init_0_0 = Some("deepseek".to_string());
                let mut __cw_config_init_0_value: codewhale_runtime::settings::Settings =
                    Default::default();
                __cw_config_init_0_value.default_provider = __cw_config_init_0_0;
                __cw_config_init_0_value
            };
            seed.save().expect("seed settings");
        }
        let mut app = create_test_app();
        app.set_provider_identity_record(
            codewhale_runtime::config::Config::default()
                .resolve_provider_identity(codewhale_config::ProviderKind::Zai.as_str())
                .expect("captured fixture provider"),
        );
        app.model_ids_passthrough = false;
        app.model = codewhale_config::descriptors::defaults::DEFAULT_ZAI_MODEL.to_string();
        app.auto_model = false;

        let result = model(&mut app, Some("GLM-5.2"));
        assert!(result.message.is_some(), "expected a model-changed message");
        assert!(!result.is_error, "GLM-5.2 is valid on Z.ai");

        let settings = codewhale_runtime::settings::Settings::load().expect("load settings");
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
            codewhale_runtime::config::Config::default()
                .resolve_provider_identity(codewhale_config::ProviderKind::Zai.as_str())
                .expect("captured fixture provider"),
        );
        app.model_ids_passthrough = false;
        app.model = codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL.to_string();
        app.auto_model = false;

        let result = model(&mut app, Some("glm-5.3"));

        assert!(!result.is_error, "GLM-5.3 is valid on Z.ai: {result:?}");
        assert_eq!(
            app.model,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_3_MODEL
        );
        assert_eq!(
            app.provider_models.get("zai").map(String::as_str),
            Some(codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_3_MODEL)
        );
        assert_eq!(
            app.pending_route_save
                .as_ref()
                .map(|pending| pending.model.as_str()),
            Some(codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_3_MODEL)
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
        app_a.api_provider = codewhale_config::ProviderKind::Zai;
        app_a.model_ids_passthrough = false;
        app_a.model = codewhale_config::descriptors::defaults::DEFAULT_ZAI_MODEL.to_string();
        app_a.auto_model = false;
        let result_a = model(&mut app_a, Some("GLM-5.2"));
        assert!(!result_a.is_error, "GLM-5.2 is valid on Z.ai");
        assert_eq!(app_a.api_provider, codewhale_config::ProviderKind::Zai);
        assert_eq!(app_a.model, "GLM-5.2");

        // Terminal B: DeepSeek / deepseek-v4-flash.
        let mut app_b = create_test_app();
        app_b.api_provider = codewhale_config::ProviderKind::Deepseek;
        app_b.model_ids_passthrough = false;
        app_b.model = "deepseek-v4-pro".to_string();
        app_b.auto_model = false;
        let result_b = model(&mut app_b, Some("deepseek-v4-flash"));
        assert!(!result_b.is_error, "deepseek-v4-flash is valid on DeepSeek");

        // B's route is a coherent DeepSeek route — never Z.ai + a DeepSeek model.
        assert_eq!(app_b.api_provider, codewhale_config::ProviderKind::Deepseek);
        assert_eq!(app_b.model, "deepseek-v4-flash");

        // A is untouched by B's selection — still Z.ai / GLM.
        assert_eq!(app_a.api_provider, codewhale_config::ProviderKind::Zai);
        assert_eq!(app_a.model, "GLM-5.2");

        // Shared settings: NOTHING was written by either `/model` — both
        // changes are session-local with a pending save decision, and the
        // global default provider was never flipped.
        let settings = codewhale_runtime::settings::Settings::load().expect("load settings");
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
        app.api_provider = codewhale_config::ProviderKind::Zai;
        app.model_ids_passthrough = false;
        app.model = codewhale_config::descriptors::defaults::DEFAULT_ZAI_MODEL.to_string();
        app.auto_model = false;
        app.provider_models.clear();

        let result = model(&mut app, Some("deepseek-v4-pro"));

        assert!(result.is_error, "expected a local rejection");
        let msg = result.message.expect("error message");
        assert!(msg.contains("deepseek-v4-pro"), "names the model: {msg}");
        assert!(msg.contains("zai"), "names the provider: {msg}");
        // The session route is unchanged — still Z.ai / GLM.
        assert_eq!(app.api_provider, codewhale_config::ProviderKind::Zai);
        assert_eq!(
            app.model,
            codewhale_config::descriptors::defaults::DEFAULT_ZAI_MODEL
        );
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
        app.api_provider = codewhale_config::ProviderKind::Openai;
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
        app.api_provider = codewhale_config::ProviderKind::Deepseek;
        app.provider_models
            .insert("moonshot".to_string(), "kimi-k2.6".to_string());

        let result = model(&mut app, Some("kimi-k2.6"));

        let message = result.message.expect("invalid model message");
        assert!(message.contains("Invalid model"));
        assert!(message.contains("active provider"));
        assert!(result.action.is_none());
        assert_eq!(app.api_provider, codewhale_config::ProviderKind::Deepseek);
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
        let result = crate::commands::execute("/models", &mut app);
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
        let result = crate::commands::execute("/subagents", &mut app);
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
