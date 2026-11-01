use super::*;
use crate::test_support::{EnvVarGuard, lock_test_env};
use crate::tui::clipboard::{ClipboardHandler, PastedImage};
use crate::tui::history::{GenericToolCell, HistoryCell, ToolCell, ToolStatus};
use crate::tui::motion::MotionMode;
use codewhale_config::ProviderKind;
use codewhale_engine::tools::plan::{PlanItemArg, StepStatus, UpdatePlanArgs};
use codewhale_engine::tools::todo::TodoStatus;
use codewhale_models::Usage;
use codewhale_runtime::config::{Config, ProviderConfig, ProvidersConfig};
use codewhale_runtime::settings::Settings;

fn test_options(yolo: bool) -> TuiOptions {
    TuiOptions {
        model: "test-model".to_string(),
        allow_shell: yolo,
        // Keep unit tests independent from the developer's saved
        // `default_mode` setting.
        start_in_agent_mode: true,
        skip_onboarding: false,
        yolo,
        ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
    }
}

#[test]
fn truncating_api_messages_keeps_preserved_stamps_in_place() {
    let mut app = App::new(test_options(false), &Config::default());
    let message = |text: &str| Message {
        role: codewhale_models::Role::User,
        content: vec![codewhale_models::ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    };
    let first = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
    let third = first + chrono::Duration::minutes(2);
    app.api_messages =
        std::sync::Arc::new(vec![message("first"), message("second"), message("third")]);
    app.api_message_stamps = vec![first, first + chrono::Duration::minutes(1), third];
    app.truncate_api_messages(2);
    assert_eq!(app.api_messages.len(), 2);
    assert_eq!(app.api_message_stamps.len(), 2);
    app.truncate_api_messages(1);
    assert_eq!(app.api_messages.len(), 1);
    assert_eq!(app.api_message_stamps, vec![first]);
}

#[test]
fn set_api_messages_installs_the_shared_snapshot_without_copying() {
    let mut app = App::new(test_options(false), &Config::default());
    let snapshot = Arc::new(vec![Message {
        role: codewhale_models::Role::User,
        content: vec![codewhale_models::ContentBlock::Text {
            text: "hello".to_string(),
            cache_control: None,
        }],
    }]);
    app.set_api_messages(Arc::clone(&snapshot));
    assert!(Arc::ptr_eq(&app.api_messages, &snapshot));
    // Mutating the mirror detaches; the engine snapshot is untouched.
    app.push_api_message(Message {
        role: codewhale_models::Role::Assistant,
        content: vec![],
    });
    assert_eq!(snapshot.len(), 1);
    assert_eq!(app.api_messages.len(), 2);
    app.session_turn_outcomes = vec![
        codewhale_engine::session_manager::SavedTurnOutcome::failed("shared failure", 1),
        codewhale_engine::session_manager::SavedTurnOutcome::failed("replaced failure", 2),
    ];
    let replacement = Arc::new(vec![snapshot[0].clone(), snapshot[0].clone()]);
    app.set_api_messages(Arc::clone(&replacement));
    assert!(Arc::ptr_eq(&app.api_messages, &replacement));
    assert_eq!(app.session_turn_outcomes.len(), 1);
    assert_eq!(app.session_turn_outcomes[0].error, "shared failure");
}

#[test]
fn app_motion_policy_and_transcript_bridge_cover_every_settings_mode() {
    let mut app = App::new(test_options(false), &Config::default());
    app.constrained_frame_rate = false;

    for (low_motion, fancy_animations, expected_mode, static_status) in [
        (false, true, MotionMode::Full, false),
        (true, true, MotionMode::Reduced, true),
        (false, false, MotionMode::Still, true),
        // The explicit accessibility preference wins when both switches are off.
        (true, false, MotionMode::Reduced, true),
    ] {
        app.low_motion = low_motion;
        app.fancy_animations = fancy_animations;

        assert_eq!(app.motion_policy().mode(), expected_mode);
        assert_eq!(app.effective_low_motion_for_status(), static_status);
        let options = app.transcript_render_options();
        assert_eq!(options.low_motion, static_status);
        assert_eq!(options.motion_mode, expected_mode);
    }
}

#[cfg(unix)]
fn create_dir_symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_dir_symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[test]
fn feature_intro_scenario() {
    // Scenario consolidation of: feature_intro_is_silent_while_onboarding_is_in_progress, feature_intro_is_silent_when_auth_setup_is_incomplete
    // from feature_intro_is_silent_while_onboarding_is_in_progress
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.onboarding = OnboardingState::Welcome;
        let before = app.history.len();
        app.maybe_show_feature_intro();
        assert_eq!(
            app.history.len(),
            before,
            "must not nudge while onboarding is in progress"
        );
    }
    // from feature_intro_is_silent_when_auth_setup_is_incomplete
    {
        // --skip-onboarding with no provider key must not claim setup is ready (#3985).
        let mut app = App::new(test_options(false), &Config::default());
        app.onboarding = OnboardingState::None;
        app.onboarding_needs_api_key = true;
        let before = app.history.len();
        app.maybe_show_feature_intro();
        assert_eq!(
            app.history.len(),
            before,
            "must not show 'setup is ready' when API key / auth is missing"
        );
    }
}

#[test]
fn feature_intro_shows_once_persists_then_is_idempotent() {
    let _env_lock = lock_test_env();
    let tmp = std::env::temp_dir().join(format!("cw-feature-intro-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let config_path = tmp.join("config.toml");
    let _env = EnvVarGuard::set(
        "DEEPSEEK_CONFIG_PATH",
        config_path.to_string_lossy().as_ref(),
    );
    let _ = std::fs::remove_file(tmp.join("settings.toml"));

    let mut app = App::new(test_options(false), &Config::default());
    app.onboarding = OnboardingState::None;
    // Isolated config has no key; pin readiness so the ready-tip path is exercised.
    app.onboarding_needs_api_key = false;
    let before = app.history.len();

    app.maybe_show_feature_intro();
    assert_eq!(app.history.len(), before, "intro must not hide empty state");
    assert!(
        app.status_message
            .as_deref()
            .is_some_and(|message| message.contains("fleet") && message.contains("/fleet setup"))
    );

    // Persisted flag now set → a second call is a no-op.
    assert!(
        Settings::load()
            .expect("settings should load")
            .feature_intro_shown,
        "feature_intro_shown should be persisted"
    );
    app.maybe_show_feature_intro();
    assert_eq!(
        app.history.len(),
        before,
        "intro must not repeat once the flag is persisted"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn initial_input_scenario() {
    // Scenario consolidation of: initial_input_prefill_waits_for_manual_submit, initial_input_submit_marks_startup_dispatch
    // from initial_input_prefill_waits_for_manual_submit
    {
        let mut options = test_options(false);
        options.initial_input = Some(InitialInput::Prefill("review this PR".to_string()));

        let app = App::new(options, &Config::default());

        assert!(
            !app.launch.visible,
            "an intentional prefilled prompt must enter the live composer instead of the startup hero"
        );
        assert_eq!(app.input, "review this PR");
        assert_eq!(app.cursor_position, "review this PR".chars().count());
        assert!(!app.auto_submit_initial_input);
    }
    // from initial_input_submit_marks_startup_dispatch
    {
        let mut options = test_options(false);
        options.initial_input = Some(InitialInput::Submit(
            "阅读项目 and wait for instructions".to_string(),
        ));

        let app = App::new(options, &Config::default());

        assert!(
            !app.launch.visible,
            "an intentional submitted prompt must bypass the startup hero"
        );
        assert_eq!(app.input, "阅读项目 and wait for instructions");
        assert_eq!(
            app.cursor_position,
            "阅读项目 and wait for instructions".chars().count()
        );
        assert!(app.auto_submit_initial_input);
    }
}

#[test]
fn clean_launch_keeps_startup_hero_despite_a_startup_notice() {
    let _env_lock = lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    let _config_env = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    std::fs::write(tmp.path().join("settings.toml"), "launch_screen = false\n")
        .expect("legacy settings");
    let mut options = test_options(false);
    options.startup_notice =
        Some("Provider route changed; inspect the route before sending".into());

    let app = App::new(options, &Config::default());

    assert!(
        app.launch.visible,
        "a fresh interactive launch must keep the Tideline startup hero visible; a notice is not an intentional resume or prompt"
    );
}

#[test]
fn explicit_resume_bypasses_startup_hero() {
    let _env_lock = lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    let _config_env = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let mut options = test_options(false);
    options.resume_session_id = Some("explicit-resume".into());

    let app = App::new(options, &Config::default());

    assert!(
        !app.launch.visible,
        "an explicit resume must preserve the existing session path"
    );
}

#[test]
fn remote_control_initial_input_bypasses_startup_hero() {
    let _env_lock = lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    let _config_env = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let mut options = test_options(false);
    options.initial_input = Some(InitialInput::RemoteControl);

    let app = App::new(options, &Config::default());

    assert!(
        !app.launch.visible,
        "an intentional remote-control launch must preserve its existing direct-session path"
    );
}

#[test]
fn composer_arrows_scenario() {
    // Scenario consolidation of: composer_arrows_scroll_default_is_true_without_mouse_capture, composer_arrows_scroll_default_is_false_with_mouse_capture_on_non_windows, composer_arrows_scroll_default_is_false_with_mouse_capture_on_windows, composer_arrows_scroll_default_is_true_without_mouse_capture_on_windows
    // from composer_arrows_scroll_default_is_true_without_mouse_capture
    {
        assert!(default_composer_arrows_scroll_for_platform(false, false));
    }
    // from composer_arrows_scroll_default_is_false_with_mouse_capture_on_non_windows
    {
        assert!(!default_composer_arrows_scroll_for_platform(true, false));
    }
    // from composer_arrows_scroll_default_is_false_with_mouse_capture_on_windows
    {
        assert!(!default_composer_arrows_scroll_for_platform(true, true));
    }
    // from composer_arrows_scroll_default_is_true_without_mouse_capture_on_windows
    {
        assert!(default_composer_arrows_scroll_for_platform(false, true));
    }
}

#[test]
fn move_cursor_scenario() {
    // Scenario consolidation of: move_cursor_line_start_multiline, move_cursor_line_start_singleline, move_cursor_line_end_multiline, move_cursor_line_end_at_newline_stays_at_line_end, move_cursor_line_end_last_line, move_cursor_line_start_already_at_start
    // from move_cursor_line_start_multiline
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "abc\ndef\nghi".to_string();
        app.cursor_position = "abc\ndef\nghi".chars().count(); // absolute end
        app.move_cursor_line_start();
        assert_eq!(app.cursor_position, "abc\ndef\n".len()); // start of "ghi"
    }
    // from move_cursor_line_start_singleline
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "hello".to_string();
        app.cursor_position = 3;
        app.move_cursor_line_start();
        assert_eq!(app.cursor_position, 0);
    }
    // from move_cursor_line_end_multiline
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "abc\ndef\nghi".to_string();
        app.cursor_position = 0; // start of first line
        app.move_cursor_line_end();
        assert_eq!(app.cursor_position, "abc".len()); // before first '\n'
    }
    // from move_cursor_line_end_at_newline_stays_at_line_end
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "abc\ndef\nghi".to_string();
        app.cursor_position = "abc".len(); // on the '\n'
        app.move_cursor_line_end();
        assert_eq!(app.cursor_position, "abc".len()); // stays at line end
    }
    // from move_cursor_line_end_last_line
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "abc\ndef".to_string();
        app.cursor_position = "abc\n".len(); // start of last line
        app.move_cursor_line_end();
        assert_eq!(app.cursor_position, "abc\ndef".chars().count()); // absolute end
    }
    // from move_cursor_line_start_already_at_start
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "abc\ndef".to_string();
        app.cursor_position = "abc\n".len(); // start of second line
        app.move_cursor_line_start();
        assert_eq!(app.cursor_position, "abc\n".len()); // unchanged
    }
}

#[test]
fn test_trust_mode_follows_yolo_on_startup() {
    let _env_lock = lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    let _config_env = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let mut options = test_options(true);
    options.config_path = Some(config_path);
    let app = App::new(options, &Config::default());
    assert!(app.trust_mode);
}

#[test]
fn reasoning_effort_display_label_keeps_codex_top_tiers_distinct() {
    assert_eq!(
        ReasoningEffort::Off.display_label_for_provider(ProviderKind::OpenaiCodex),
        "low"
    );
    assert_eq!(
        ReasoningEffort::Medium.display_label_for_provider(ProviderKind::OpenaiCodex),
        "medium"
    );
    // The roster publishes xhigh, max and ultra as separate rungs, so the
    // label must not collapse them onto the old ceiling.
    assert_eq!(
        ReasoningEffort::XHigh.display_label_for_provider(ProviderKind::OpenaiCodex),
        "xhigh"
    );
    assert_eq!(
        ReasoningEffort::Max.display_label_for_provider(ProviderKind::OpenaiCodex),
        "max"
    );
    assert_eq!(
        ReasoningEffort::Ultra.display_label_for_provider(ProviderKind::OpenaiCodex),
        "ultra"
    );
    assert_eq!(
        ReasoningEffort::Max.display_label_for_provider(ProviderKind::Deepseek),
        "max"
    );
    assert_eq!(
        ReasoningEffort::High.display_label_for_provider(ProviderKind::OpenaiCodex),
        "high"
    );

    let mut app = App::new(test_options(false), &Config::default());
    app.api_provider = ProviderKind::OpenaiCodex;
    app.reasoning_effort = ReasoningEffort::Max;
    app.auto_model = false;
    assert_eq!(app.reasoning_effort_display_label(), "max");

    app.reasoning_effort = ReasoningEffort::Auto;
    app.last_effective_reasoning_effort =
        Some(EffectiveReasoningEffort::Tier(ReasoningEffort::Max));
    assert_eq!(app.reasoning_effort_display_label(), "auto→max");
}

#[test]
fn fixed_auto_reasoning_label_preserves_untiered_effective_receipt() {
    let mut app = App::new(test_options(false), &Config::default());
    app.api_provider = ProviderKind::Zai;
    app.auto_model = false;
    app.model = codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_TURBO_MODEL.to_string();
    app.active_route_base_url =
        codewhale_config::descriptors::defaults::DEFAULT_ZAI_BASE_URL.to_string();
    app.reasoning_effort = ReasoningEffort::Auto;
    app.last_effective_reasoning_effort =
        Some(EffectiveReasoningEffort::ThinkingEnabledGranularityUnavailable);

    assert_eq!(
        app.reasoning_effort_display_label(),
        "auto→thinking enabled; granularity unavailable"
    );
}

#[test]
fn cache_replay_keeps_untiered_reasoning_enabled() {
    let mut app = App::new(test_options(false), &Config::default());
    app.api_provider = ProviderKind::Zai;
    app.auto_model = false;
    app.model = codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_TURBO_MODEL.to_string();
    app.reasoning_effort = ReasoningEffort::Auto;
    app.last_effective_reasoning_effort =
        Some(EffectiveReasoningEffort::ThinkingEnabledGranularityUnavailable);

    assert_eq!(
        app.reasoning_effort_api_value_for_replay(
            ProviderKind::Zai,
            ProviderKind::Zai.as_str(),
            codewhale_config::descriptors::defaults::DEFAULT_ZAI_BASE_URL,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_TURBO_MODEL,
        ),
        Some("high")
    );

    app.api_provider = ProviderKind::Minimax;
    app.model = codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_MODEL.to_string();
    assert_eq!(
        app.reasoning_effort_api_value_for_replay(
            ProviderKind::Minimax,
            ProviderKind::Minimax.as_str(),
            codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_BASE_URL,
            codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_MODEL,
        ),
        Some("high")
    );

    app.last_effective_reasoning_effort = Some(EffectiveReasoningEffort::Unavailable);
    assert_eq!(
        app.reasoning_effort_api_value_for_replay(
            ProviderKind::Zai,
            ProviderKind::Zai.as_str(),
            codewhale_config::descriptors::defaults::DEFAULT_ZAI_BASE_URL,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_TURBO_MODEL,
        ),
        None
    );
}

#[test]
fn cache_replay_normalizes_reasoning_against_the_concrete_auto_route() {
    let mut app = App::new(test_options(false), &Config::default());
    app.api_provider = ProviderKind::Deepseek;
    app.model = "auto".to_string();
    app.auto_model = true;

    app.reasoning_effort = ReasoningEffort::Off;
    assert_eq!(
        app.reasoning_effort_api_value_for_replay(
            ProviderKind::OpenaiCodex,
            ProviderKind::OpenaiCodex.as_str(),
            codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_BASE_URL,
            codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL,
        ),
        Some("low"),
        "Codex must apply its Off-to-Low floor even when DeepSeek is configured"
    );

    app.reasoning_effort = ReasoningEffort::Medium;
    assert_eq!(
        app.reasoning_effort_api_value_for_replay(
            ProviderKind::Moonshot,
            ProviderKind::Moonshot.as_str(),
            codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL,
            codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL,
        ),
        Some("medium"),
        "Kimi Code K3 must retain its exact-route Medium tier"
    );
}

#[test]
fn cache_replay_target_uses_the_last_completed_auto_route() {
    let mut app = App::new(test_options(false), &Config::default());
    app.model = "auto".to_string();
    app.auto_model = true;
    app.last_effective_provider = Some(ProviderKind::OpenaiCodex);
    app.last_effective_provider_identity = Some(ProviderKind::OpenaiCodex.as_str().to_string());
    app.last_effective_model =
        Some(codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL.to_string());
    app.session.last_base_url =
        Some(codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_BASE_URL.to_string());
    app.push_turn_cache_record(TurnCacheRecord {
        provider: Some(ProviderKind::OpenaiCodex),
        provider_identity: Some(ProviderKind::OpenaiCodex.as_str().to_string()),
        model: Some(
            codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL.to_string(),
        ),
        auto_model: true,
        input_tokens: 1,
        output_tokens: 1,
        cache_hit_tokens: None,
        cache_miss_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        cost_audit: None,
        reasoning_replay_tokens: None,
        recorded_at: std::time::Instant::now(),
    });

    let target = app
        .cache_replay_target()
        .expect("completed Auto route must be replayable");

    assert_eq!(target.provider, ProviderKind::OpenaiCodex);
    assert_eq!(target.provider_identity, ProviderKind::OpenaiCodex.as_str());
    assert_eq!(
        target.provider_id.as_deref(),
        Some(ProviderKind::OpenaiCodex.as_str())
    );
    assert_eq!(
        target.model,
        codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL
    );
    assert_eq!(
        target.base_url.as_deref(),
        Some(codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_BASE_URL)
    );

    // A restored Auto session has no turn ring or raw endpoint. Once warmup
    // safely re-resolves that route, its exact key becomes sufficient
    // endpoint evidence for a following inspect.
    app.session.turn_cache_history.clear();
    app.session.last_base_url = None;
    app.session.last_warmup_key = Some(CacheWarmupKey {
        provider: ProviderKind::OpenaiCodex.as_str().to_string(),
        model: codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL.to_string(),
        base_url: codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_BASE_URL
            .to_string(),
        static_prefix_hash: "static".to_string(),
        tool_catalog_hash: "tools".to_string(),
        project_pack_hash: "project".to_string(),
        skills_hash: "skills".to_string(),
    });
    assert_eq!(
        app.cache_replay_target()
            .and_then(|target| target.base_url)
            .as_deref(),
        Some(codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_BASE_URL)
    );
}

#[test]
fn auto_reasoning_change_invalidates_the_previous_route_and_receipt() {
    let mut app = App::new(test_options(false), &Config::default());
    app.api_provider = ProviderKind::Deepseek;
    app.model = "auto".to_string();
    app.auto_model = true;
    app.reasoning_effort = ReasoningEffort::Low;
    app.reasoning_effort_preference = Some(ReasoningEffort::Low);
    app.last_effective_provider = Some(ProviderKind::OpenaiCodex);
    app.last_effective_provider_identity = Some(ProviderKind::OpenaiCodex.as_str().to_string());
    app.last_effective_model =
        Some(codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL.to_string());
    app.last_auto_route_receipt = Some(codewhale_engine::model_routing::AutoRouteReceipt {
        tier: codewhale_engine::model_routing::AutoRouteTier::Strong,
        pair: codewhale_engine::model_routing::AutoRoutePair {
            strong: codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL.to_string(),
            fast: None,
        },
        scope: codewhale_engine::model_routing::AutoRouteScope::ResolvedProvider,
        data_path: codewhale_engine::model_routing::AutoRouteDataPath::LocalHeuristic,
        reason: codewhale_engine::model_routing::AutoRouteReason::LocalFallback(
            codewhale_engine::model_routing::AutoRouteHeuristicReason::DeclaredDefault,
        ),
        decision: None,
        router_failure: None,
    });
    app.last_effective_reasoning_effort =
        Some(EffectiveReasoningEffort::Tier(ReasoningEffort::Max));

    assert!(
        app.cache_replay_target().is_some(),
        "the completed route is replayable before its classifier input changes"
    );

    app.cycle_effort();

    assert_eq!(app.reasoning_effort, ReasoningEffort::Medium);
    assert_eq!(
        app.status_message.as_deref(),
        Some("Reasoning effort: med"),
        "the change must describe the new unresolved request, not the old Codex receipt"
    );
    assert_eq!(app.last_effective_reasoning_effort, None);
    assert_eq!(app.last_effective_provider, None);
    assert_eq!(app.last_effective_provider_identity, None);
    assert_eq!(app.last_effective_model, None);
    assert_eq!(app.last_auto_route_receipt, None);
    assert!(
        app.cache_replay_target().is_none(),
        "cache replay must wait for a route accepted under the new reasoning request"
    );

    let work = app
        .work_state_snapshot()
        .expect("Work snapshot")
        .expect("effort activity creates graph state");
    let codewhale_engine::work_graph::WorkActivityEvent::ReasoningEffortChanged {
        effective, ..
    } = work
        .graph
        .expect("Work Graph")
        .activities
        .last()
        .cloned()
        .expect("effort activity");
    assert_eq!(
        effective,
        codewhale_models::reasoning_effort_tier::ReasoningEffortTier::Medium,
        "the activity receipt must not reuse the previous turn's effective tier"
    );
}

#[test]
fn thinking_is_locked_but_mode_moves_while_a_turn_is_running() {
    let mut app = App::new(test_options(false), &Config::default());
    app.mode = AppMode::Agent;
    app.reasoning_effort = ReasoningEffort::Max;
    app.is_loading = true;

    let before_effort = app.reasoning_effort;
    app.cycle_effort();
    assert_eq!(
        app.reasoning_effort, before_effort,
        "thinking must not change while busy"
    );
    assert!(
        app.status_message
            .as_deref()
            .unwrap_or_default()
            .contains("locked"),
        "expected a 'locked' status message, got {:?}",
        app.status_message
    );

    app.cycle_mode();
    assert_ne!(app.mode, AppMode::Agent, "mode changes mid-turn");
}

#[test]
fn cycle_effort_updates_effort_status_and_compaction() {
    // Ctrl+T parity with the hotbar's `reasoning.cycle` action: cycling the
    // effort must surface a status message and refresh the compaction budget,
    // not just silently flip the setting.
    let mut app = App::new(test_options(false), &Config::default());
    app.api_provider = ProviderKind::Deepseek;
    app.auto_model = false;
    app.reasoning_effort = ReasoningEffort::Off;
    // Sentinel so the test can observe update_model_compaction_budget().
    app.compact_threshold = 0;

    app.cycle_effort();

    assert_eq!(app.reasoning_effort, ReasoningEffort::Low);
    assert_eq!(app.reasoning_effort_preference, Some(ReasoningEffort::Low));
    assert_eq!(
        app.status_message.as_deref(),
        Some("Reasoning effort: low"),
        "Ctrl+T must give visible feedback like the hotbar action"
    );
    assert_ne!(
        app.compact_threshold, 0,
        "cycling effort must refresh the compaction budget"
    );
    assert!(app.needs_redraw);

    let work = app
        .work_state_snapshot()
        .expect("Work snapshot")
        .expect("effort activity creates graph state");
    let graph = work.graph.expect("Work Graph");
    let activity = graph.activities.last().expect("effort activity");
    match activity {
        codewhale_engine::work_graph::WorkActivityEvent::ReasoningEffortChanged {
            requested,
            effective,
            provider_kind,
            provider,
            operation,
            ..
        } => {
            assert_eq!(
                *requested,
                codewhale_models::reasoning_effort_tier::ReasoningEffortTier::Low
            );
            assert_eq!(
                *effective,
                codewhale_models::reasoning_effort_tier::ReasoningEffortTier::Low
            );
            assert_eq!(*provider_kind, Some(ProviderKind::Deepseek));
            assert_eq!(provider, "deepseek");
            assert!(operation.is_none());
        }
    }
    let wire = serde_json::to_value(activity).expect("serialize activity");
    assert_eq!(wire["kind"], "reasoning_effort_changed");
    assert!(
        wire.get("text").is_none(),
        "activity must not carry reasoning text"
    );
}

#[test]
fn glm_5_turbo_records_enabled_with_granularity_unavailable() {
    let mut app = App::new(test_options(false), &Config::default());
    app.set_provider_identity_record(
        codewhale_runtime::config::Config::default()
            .resolve_provider_identity(ProviderKind::Zai.as_str())
            .expect("captured fixture provider"),
    );
    app.auto_model = false;
    app.active_route_base_url =
        codewhale_config::descriptors::defaults::DEFAULT_ZAI_BASE_URL.to_string();
    app.model = codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_TURBO_MODEL.to_string();
    app.reasoning_effort = ReasoningEffort::High;

    app.cycle_effort();

    assert_eq!(app.reasoning_effort, ReasoningEffort::Max);
    assert_eq!(
        app.status_message.as_deref(),
        Some("Reasoning effort: max→thinking enabled; granularity unavailable")
    );
    assert_eq!(
        app.reasoning_effort_display_label(),
        "max→thinking enabled; granularity unavailable"
    );
    let work = app
        .work_state_snapshot()
        .expect("Work snapshot")
        .expect("effort activity creates graph state");
    let activity = work
        .graph
        .expect("Work Graph")
        .activities
        .last()
        .cloned()
        .expect("effort activity");
    let codewhale_engine::work_graph::WorkActivityEvent::ReasoningEffortChanged {
        requested,
        effective,
        provider,
        ..
    } = &activity;
    assert_eq!(
        *requested,
        codewhale_models::reasoning_effort_tier::ReasoningEffortTier::Max
    );
    assert_eq!(
        *effective,
        codewhale_models::reasoning_effort_tier::ReasoningEffortTier::ThinkingEnabledGranularityUnavailable
    );
    assert_eq!(provider, "zai");
    assert_eq!(
        serde_json::to_value(activity).expect("serialize activity")["effective"],
        "thinking_enabled_granularity_unavailable"
    );
}

#[test]
fn glm_5_1_records_enabled_with_granularity_unavailable() {
    let mut app = App::new(test_options(false), &Config::default());
    app.set_provider_identity_record(
        codewhale_runtime::config::Config::default()
            .resolve_provider_identity(ProviderKind::Zai.as_str())
            .expect("captured fixture provider"),
    );
    app.auto_model = false;
    app.active_route_base_url =
        codewhale_config::descriptors::defaults::DEFAULT_ZAI_BASE_URL.to_string();
    app.model = codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_1_MODEL.to_string();
    app.reasoning_effort = ReasoningEffort::High;

    app.cycle_effort();

    assert_eq!(
        app.reasoning_effort_display_label(),
        "max→thinking enabled; granularity unavailable"
    );
    let work = app
        .work_state_snapshot()
        .expect("Work snapshot")
        .expect("effort activity creates graph state");
    let codewhale_engine::work_graph::WorkActivityEvent::ReasoningEffortChanged {
        effective, ..
    } = work
        .graph
        .expect("Work Graph")
        .activities
        .last()
        .cloned()
        .expect("effort activity");
    assert_eq!(
        effective,
        codewhale_models::reasoning_effort_tier::ReasoningEffortTier::ThinkingEnabledGranularityUnavailable
    );
}

#[test]
fn unknown_model_on_exact_zai_endpoint_records_effective_unavailable() {
    let mut app = App::new(test_options(false), &Config::default());
    app.set_provider_identity_record(
        codewhale_runtime::config::Config::default()
            .resolve_provider_identity(ProviderKind::Zai.as_str())
            .expect("captured fixture provider"),
    );
    app.auto_model = false;
    app.active_route_base_url =
        codewhale_config::descriptors::defaults::DEFAULT_ZAI_BASE_URL.to_string();
    app.model = "glm-future-unknown".to_string();
    app.reasoning_effort = ReasoningEffort::High;

    app.cycle_effort();

    assert_eq!(
        app.reasoning_effort_display_label(),
        "max→effective unavailable"
    );
    let work = app
        .work_state_snapshot()
        .expect("Work snapshot")
        .expect("effort activity creates graph state");
    let codewhale_engine::work_graph::WorkActivityEvent::ReasoningEffortChanged {
        effective, ..
    } = work
        .graph
        .expect("Work Graph")
        .activities
        .last()
        .cloned()
        .expect("effort activity");
    assert_eq!(
        effective,
        codewhale_models::reasoning_effort_tier::ReasoningEffortTier::Unavailable
    );
}

#[test]
fn compatible_zai_gateway_records_effective_unavailable() {
    let mut app = App::new(test_options(false), &Config::default());
    app.set_provider_identity_record(
        codewhale_runtime::config::Config::default()
            .resolve_provider_identity(ProviderKind::Zai.as_str())
            .expect("captured fixture provider"),
    );
    app.auto_model = false;
    app.active_route_base_url = "https://gateway.example/v1".to_string();
    app.model = codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL.to_string();
    app.reasoning_effort = ReasoningEffort::High;

    app.cycle_effort();

    assert_eq!(app.reasoning_effort, ReasoningEffort::Max);
    assert_eq!(
        app.status_message.as_deref(),
        Some("Reasoning effort: max→effective unavailable")
    );
    assert_eq!(
        app.reasoning_effort_display_label(),
        "max→effective unavailable"
    );
    let work = app
        .work_state_snapshot()
        .expect("Work snapshot")
        .expect("effort activity creates graph state");
    let activity = work
        .graph
        .expect("Work Graph")
        .activities
        .last()
        .cloned()
        .expect("effort activity");
    let codewhale_engine::work_graph::WorkActivityEvent::ReasoningEffortChanged {
        requested,
        effective,
        provider,
        ..
    } = &activity;
    assert_eq!(
        *requested,
        codewhale_models::reasoning_effort_tier::ReasoningEffortTier::Max
    );
    assert_eq!(
        *effective,
        codewhale_models::reasoning_effort_tier::ReasoningEffortTier::Unavailable
    );
    assert_eq!(provider, "zai");
    assert_eq!(
        serde_json::to_value(activity).expect("serialize activity")["effective"],
        "unavailable"
    );
}

#[test]
fn minimax_m3_high_and_max_receipts_do_not_claim_tier_granularity() {
    for (previous, requested, label) in [
        (ReasoningEffort::Off, ReasoningEffort::Auto, "auto"),
        (ReasoningEffort::Auto, ReasoningEffort::Off, "off"),
    ] {
        let mut app = App::new(test_options(false), &Config::default());
        app.set_provider_identity_record(
            codewhale_runtime::config::Config::default()
                .resolve_provider_identity(ProviderKind::Minimax.as_str())
                .expect("captured fixture provider"),
        );
        app.auto_model = false;
        app.active_route_base_url =
            codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_BASE_URL.to_string();
        app.model = codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_MODEL.to_string();
        app.reasoning_effort = previous;

        app.cycle_effort();

        assert_eq!(app.reasoning_effort, requested);
        assert_eq!(app.reasoning_effort_display_label(), label);
        let work = app
            .work_state_snapshot()
            .expect("Work snapshot")
            .expect("effort activity creates graph state");
        let activity = work
            .graph
            .expect("Work Graph")
            .activities
            .last()
            .cloned()
            .expect("effort activity");
        let codewhale_engine::work_graph::WorkActivityEvent::ReasoningEffortChanged {
            effective,
            endpoint_identity,
            model,
            ..
        } = activity;
        assert_eq!(
            effective,
            if requested == ReasoningEffort::Auto {
                codewhale_models::reasoning_effort_tier::ReasoningEffortTier::Auto
            } else {
                codewhale_models::reasoning_effort_tier::ReasoningEffortTier::Off
            }
        );
        assert_eq!(
            endpoint_identity.as_deref(),
            Some(codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_BASE_URL)
        );
        assert_eq!(
            model.as_deref(),
            Some(codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_MODEL)
        );
    }
}

#[test]
fn minimax_anthropic_m3_high_and_max_receipts_match_adaptive_wire_truth() {
    for (previous, requested, label) in [
        (ReasoningEffort::Off, ReasoningEffort::Auto, "auto"),
        (ReasoningEffort::Auto, ReasoningEffort::Off, "off"),
    ] {
        let mut app = App::new(test_options(false), &Config::default());
        app.set_provider_identity_record(
            codewhale_runtime::config::Config::default()
                .resolve_provider_identity(ProviderKind::MinimaxAnthropic.as_str())
                .expect("captured fixture provider"),
        );
        app.auto_model = false;
        app.active_route_base_url =
            codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_ANTHROPIC_BASE_URL.to_string();
        app.model = codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_MODEL.to_string();
        app.reasoning_effort = previous;

        app.cycle_effort();

        assert_eq!(app.reasoning_effort, requested);
        assert_eq!(app.reasoning_effort_display_label(), label);
        let work = app
            .work_state_snapshot()
            .expect("Work snapshot")
            .expect("effort activity creates graph state");
        let activity = work
            .graph
            .expect("Work Graph")
            .activities
            .last()
            .cloned()
            .expect("effort activity");
        let codewhale_engine::work_graph::WorkActivityEvent::ReasoningEffortChanged {
            effective,
            provider_kind,
            provider,
            endpoint_identity,
            model,
            ..
        } = activity;
        assert_eq!(
            effective,
            if requested == ReasoningEffort::Auto {
                codewhale_models::reasoning_effort_tier::ReasoningEffortTier::Auto
            } else {
                codewhale_models::reasoning_effort_tier::ReasoningEffortTier::Off
            }
        );
        assert_eq!(provider_kind, Some(ProviderKind::MinimaxAnthropic));
        assert_eq!(provider, "minimax-anthropic");
        assert_eq!(
            endpoint_identity.as_deref(),
            Some(codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_ANTHROPIC_BASE_URL)
        );
        assert_eq!(
            model.as_deref(),
            Some(codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_MODEL)
        );
    }
}

#[test]
fn named_custom_route_displays_and_persists_effective_unavailable() {
    let mut app = App::new(test_options(false), &Config::default());
    app.set_provider_identity(ProviderKind::Custom, "my-gateway");
    app.auto_model = false;
    app.active_route_base_url = "https://gateway.example/v1?api_key=must-not-persist".to_string();
    app.model = "vendor-model-x".to_string();
    app.reasoning_effort = ReasoningEffort::High;

    app.cycle_effort();

    assert_eq!(app.reasoning_effort, ReasoningEffort::Max);
    assert_eq!(
        app.reasoning_effort_display_label(),
        "max→effective unavailable"
    );
    let work = app
        .work_state_snapshot()
        .expect("Work snapshot")
        .expect("unknown route activity creates valid graph state");
    let activity = work
        .graph
        .expect("Work Graph")
        .activities
        .last()
        .cloned()
        .expect("effort activity");
    let codewhale_engine::work_graph::WorkActivityEvent::ReasoningEffortChanged {
        effective,
        provider_kind,
        provider,
        endpoint_identity,
        model,
        ..
    } = activity;
    assert_eq!(
        effective,
        codewhale_models::reasoning_effort_tier::ReasoningEffortTier::Unavailable
    );
    assert_eq!(provider_kind, Some(ProviderKind::Custom));
    assert_eq!(provider, "my-gateway");
    let endpoint = endpoint_identity.expect("redacted endpoint provenance");
    assert!(endpoint.contains("gateway.example"), "{endpoint}");
    assert!(!endpoint.contains("must-not-persist"), "{endpoint}");
    assert_eq!(model.as_deref(), Some("vendor-model-x"));
}

#[test]
fn custom_routes_named_with_builtin_slugs_retain_custom_kind_and_fail_closed() {
    for identity in ["openai", "zai"] {
        let mut app = App::new(test_options(false), &Config::default());
        app.set_provider_identity(ProviderKind::Custom, identity);
        app.auto_model = false;
        app.active_route_base_url = "https://gateway.example/v1".to_string();
        app.model = "vendor-model-x".to_string();
        app.reasoning_effort = ReasoningEffort::High;

        app.cycle_effort();

        assert_eq!(
            app.reasoning_effort_display_label(),
            "max→effective unavailable"
        );
        let work = app
            .work_state_snapshot()
            .expect("Work snapshot")
            .expect("effort activity creates graph state");
        let activity = work
            .graph
            .expect("Work Graph")
            .activities
            .last()
            .cloned()
            .expect("effort activity");
        let codewhale_engine::work_graph::WorkActivityEvent::ReasoningEffortChanged {
            effective,
            provider_kind,
            provider,
            ..
        } = activity;
        assert_eq!(
            effective,
            codewhale_models::reasoning_effort_tier::ReasoningEffortTier::Unavailable
        );
        assert_eq!(provider_kind, Some(ProviderKind::Custom));
        assert_eq!(provider, identity);
    }
}

#[test]
fn zai_gateway_off_and_high_receipts_remain_unavailable() {
    for (previous, requested, label) in [
        (ReasoningEffort::High, ReasoningEffort::Max, "max"),
        (ReasoningEffort::Max, ReasoningEffort::Auto, "auto"),
    ] {
        let mut app = App::new(test_options(false), &Config::default());
        app.set_provider_identity_record(
            codewhale_runtime::config::Config::default()
                .resolve_provider_identity(ProviderKind::Zai.as_str())
                .expect("captured fixture provider"),
        );
        app.auto_model = false;
        app.active_route_base_url = "https://gateway.example/v1".to_string();
        app.model = codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL.to_string();
        app.reasoning_effort = previous;

        app.cycle_effort();

        assert_eq!(app.reasoning_effort, requested);
        assert_eq!(
            app.reasoning_effort_display_label(),
            format!("{label}→effective unavailable")
        );
    }
}

#[test]
fn kimi_code_high_and_max_work_receipts_preserve_exact_tiers() {
    for (previous, requested) in [
        (ReasoningEffort::Auto, ReasoningEffort::Low),
        (ReasoningEffort::High, ReasoningEffort::Max),
    ] {
        let mut app = App::new(test_options(false), &Config::default());
        app.set_provider_identity_record(
            codewhale_runtime::config::Config::default()
                .resolve_provider_identity(ProviderKind::Moonshot.as_str())
                .expect("captured fixture provider"),
        );
        app.auto_model = false;
        app.active_route_base_url =
            codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL.to_string();
        app.model = codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL.to_string();
        app.reasoning_effort = previous;

        app.cycle_effort();

        let work = app
            .work_state_snapshot()
            .expect("Work snapshot")
            .expect("effort activity creates graph state");
        let activity = work
            .graph
            .expect("Work Graph")
            .activities
            .last()
            .cloned()
            .unwrap();
        let codewhale_engine::work_graph::WorkActivityEvent::ReasoningEffortChanged {
            effective,
            endpoint_identity,
            model,
            ..
        } = activity;
        assert_eq!(effective, requested.into());
        assert_eq!(
            endpoint_identity.as_deref(),
            Some(codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL)
        );
        assert_eq!(
            model.as_deref(),
            Some(codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL)
        );
    }
}

#[test]
fn active_turn_zai_receipt_overrides_all_mutable_parallel_route_metadata() {
    let mut app = App::new(test_options(false), &Config::default());
    app.api_provider = ProviderKind::Deepseek;
    app.active_route_base_url =
        codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL.to_string();
    app.model = "deepseek-chat".to_string();
    app.reasoning_effort = ReasoningEffort::High;
    app.active_turn = Some(ActiveTurnMetadata {
        turn_id: "turn-zai-receipt".to_string(),
        created_at: chrono::Utc::now(),
        route: Some(codewhale_engine::core::events::TurnRoute {
            provider: ProviderKind::Zai,
            provider_identity: "openai".to_string(),
            model: "mutable-wrong-model".to_string(),
            auto_model: false,
            receipt: Some(codewhale_engine::route_receipt::TurnRouteReceipt::new(
                ProviderKind::Zai,
                "zai",
                codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_TURBO_MODEL,
                codewhale_config::descriptors::defaults::DEFAULT_ZAI_BASE_URL,
                "test-secret-never-persisted",
            )),
            billing: Some(codewhale_engine::core::events::RouteBillingEnvelope {
                openrouter_vendor: None,
                billing_surface: None,
                endpoint_fingerprint: None,
                provider_live_pricing: None,
                billing_mode: codewhale_engine::cost_status::RouteBillingMode::Unknown,
                dispatched_at: chrono::Utc::now(),
            }),
            base_url: codewhale_config::descriptors::defaults::DEFAULT_ZAI_BASE_URL.to_string(),
            billing_product: codewhale_engine::route_billing::RouteProduct::Unproven,
        }),
        auto_route_receipt: None,
        suggestion_authority: None,
    });

    assert_eq!(
        app.reasoning_effort_display_label(),
        "high→thinking enabled; granularity unavailable"
    );

    app.apply_reasoning_effort_cycle();
    let work = app
        .work_state_snapshot()
        .expect("Work snapshot")
        .expect("effort activity creates graph state");
    let activity = work
        .graph
        .expect("Work Graph")
        .activities
        .last()
        .cloned()
        .expect("effort activity");
    let codewhale_engine::work_graph::WorkActivityEvent::ReasoningEffortChanged {
        provider_kind,
        provider,
        endpoint_identity,
        model,
        ..
    } = activity;
    assert_eq!(provider_kind, Some(ProviderKind::Zai));
    assert_eq!(provider, "zai");
    assert_eq!(
        endpoint_identity.as_deref(),
        Some(codewhale_config::descriptors::defaults::DEFAULT_ZAI_BASE_URL)
    );
    assert_eq!(
        model.as_deref(),
        Some(codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_TURBO_MODEL)
    );
}

#[test]
fn pending_zai_route_without_endpoint_receipt_is_effective_unavailable() {
    let mut app = App::new(test_options(false), &Config::default());
    app.api_provider = ProviderKind::Deepseek;
    app.auto_model = false;
    app.reasoning_effort = ReasoningEffort::Max;
    app.pending_turn_route = Some((
        ProviderKind::Zai,
        codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL.to_string(),
        true,
    ));

    assert_eq!(
        app.reasoning_effort_display_label(),
        "max→effective unavailable"
    );
}

#[test]
fn reasoning_effort_scenario() {
    // Scenario consolidation of: reasoning_effort_display_receipts_route_normalization, reasoning_effort_api_values_are_provider_aware_for_codex
    // from reasoning_effort_display_receipts_route_normalization
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.api_provider = ProviderKind::Moonshot;
        app.auto_model = false;
        app.reasoning_effort = ReasoningEffort::Low;
        app.active_route_base_url =
            codewhale_config::descriptors::defaults::DEFAULT_MOONSHOT_BASE_URL.to_string();
        app.model = "kimi-k2.5".to_string();

        assert_eq!(app.reasoning_effort_display_label(), "low→high");

        app.active_route_base_url =
            codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL.to_string();
        app.model = "k3".to_string();
        assert_eq!(app.reasoning_effort_display_label(), "low");

        app.reasoning_effort = ReasoningEffort::Off;
        assert_eq!(app.reasoning_effort_display_label(), "off→low");
    }
    // from reasoning_effort_api_values_are_provider_aware_for_codex
    {
        assert_eq!(
            ReasoningEffort::Off.normalize_for_provider(ProviderKind::OpenaiCodex),
            ReasoningEffort::Low
        );
        assert_eq!(
            ReasoningEffort::Auto.normalize_for_provider(ProviderKind::OpenaiCodex),
            ReasoningEffort::Medium
        );
        // Codex sends the rung the operator picked: the roster offers xhigh,
        // max and ultra as separate efforts per model.
        assert_eq!(
            ReasoningEffort::XHigh.api_value_for_provider(ProviderKind::OpenaiCodex),
            Some("xhigh")
        );
        assert_eq!(
            ReasoningEffort::Max.api_value_for_provider(ProviderKind::OpenaiCodex),
            Some("max")
        );
        assert_eq!(
            ReasoningEffort::Ultra.api_value_for_provider(ProviderKind::OpenaiCodex),
            Some("ultra")
        );
        assert_eq!(
            ReasoningEffort::Off.api_value_for_provider(ProviderKind::OpenaiCodex),
            Some("low")
        );
        assert_eq!(
            ReasoningEffort::Max.api_value_for_provider(ProviderKind::Deepseek),
            Some("max")
        );
        assert_eq!(
            ReasoningEffort::from_setting("ultracode"),
            ReasoningEffort::Ultra
        );
    }
}

#[test]
fn ollama_cloud_normal_turns_preserve_the_documented_reasoning_ladder() {
    let base_url = codewhale_config::descriptors::defaults::DEFAULT_OLLAMA_CLOUD_BASE_URL;
    let model = codewhale_config::descriptors::defaults::DEFAULT_OLLAMA_CLOUD_MODEL;
    for effort in [
        ReasoningEffort::Off,
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::Max,
    ] {
        assert_eq!(
            effort.normalize_for_route(ProviderKind::OllamaCloud, base_url, model),
            effort,
            "{effort:?} must remain distinct on Ollama's documented Cloud ladder"
        );
    }
    assert_eq!(
        ReasoningEffort::Minimal.normalize_for_route(ProviderKind::OllamaCloud, base_url, model,),
        ReasoningEffort::Low
    );
    assert_eq!(
        ReasoningEffort::XHigh.normalize_for_route(ProviderKind::OllamaCloud, base_url, model),
        ReasoningEffort::Max
    );
}

#[test]
fn reasoning_effort_uses_one_strict_alias_table_and_legacy_fallback() {
    for raw in ["off", "none", "disabled", "false"] {
        assert_eq!(ReasoningEffort::parse_strict(raw), Ok(ReasoningEffort::Off));
    }
    for raw in ["low", "minimum", "light"] {
        assert_eq!(ReasoningEffort::parse_strict(raw), Ok(ReasoningEffort::Low));
    }
    // `minimal` is its own rung: `parse_strict(as_setting(Minimal))` must not
    // lose the variant by collapsing it onto `Low` (Slice 4, D3).
    assert_eq!(
        ReasoningEffort::parse_strict("minimal"),
        Ok(ReasoningEffort::Minimal)
    );
    for raw in ["medium", "mid"] {
        assert_eq!(
            ReasoningEffort::parse_strict(raw),
            Ok(ReasoningEffort::Medium)
        );
    }
    assert_eq!(
        ReasoningEffort::parse_strict("xhigh"),
        Ok(ReasoningEffort::XHigh)
    );
    for raw in ["ultra", "ultracode"] {
        assert_eq!(
            ReasoningEffort::parse_strict(raw),
            Ok(ReasoningEffort::Ultra)
        );
    }
    for raw in ["max", "maximum"] {
        assert_eq!(ReasoningEffort::parse_strict(raw), Ok(ReasoningEffort::Max));
    }
    assert!(ReasoningEffort::parse_strict("surprise").is_err());
    assert_eq!(
        ReasoningEffort::from_setting("surprise"),
        ReasoningEffort::Auto
    );
}

#[test]
fn reasoning_effort_normalizes_each_exact_k3_route_without_neighbor_leakage() {
    let kimi_base = codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL;
    let moonshot_base = codewhale_config::descriptors::defaults::DEFAULT_MOONSHOT_BASE_URL;
    assert_eq!(
        ReasoningEffort::Off.normalize_for_route(ProviderKind::Moonshot, kimi_base, "k3"),
        ReasoningEffort::Low,
        "membership K3 stays on K3 by mapping off to its lowest thinking tier"
    );
    assert_eq!(
        ReasoningEffort::Auto.normalize_for_route(ProviderKind::Moonshot, kimi_base, "k3"),
        ReasoningEffort::Auto,
        "route normalization preserves the Auto sentinel until dispatch selects a concrete tier"
    );
    assert_eq!(
        ReasoningEffort::Low.normalize_for_route(ProviderKind::Moonshot, kimi_base, "k3"),
        ReasoningEffort::Low
    );
    assert_eq!(
        ReasoningEffort::Medium.normalize_for_route(ProviderKind::Moonshot, kimi_base, "k3"),
        ReasoningEffort::Medium
    );
    assert_eq!(
        ReasoningEffort::Low.normalize_for_route(ProviderKind::Moonshot, moonshot_base, "k3"),
        ReasoningEffort::High
    );
    assert_eq!(
        ReasoningEffort::Medium.normalize_for_route(
            ProviderKind::Moonshot,
            kimi_base,
            "kimi-for-coding",
        ),
        ReasoningEffort::High
    );

    assert_eq!(
        ReasoningEffort::Off.normalize_for_route(
            ProviderKind::Moonshot,
            moonshot_base,
            codewhale_config::catalog::reviewed::constants::MOONSHOT_KIMI_K3_MODEL,
        ),
        ReasoningEffort::Low,
        "direct K3 is always-thinking, so off becomes its lowest supported tier"
    );
    assert_eq!(
        ReasoningEffort::Low.normalize_for_route(
            ProviderKind::Moonshot,
            moonshot_base,
            codewhale_config::catalog::reviewed::constants::MOONSHOT_KIMI_K3_MODEL,
        ),
        ReasoningEffort::Low
    );
    assert_eq!(
        ReasoningEffort::Medium.normalize_for_route(
            ProviderKind::Moonshot,
            moonshot_base,
            codewhale_config::catalog::reviewed::constants::MOONSHOT_KIMI_K3_MODEL,
        ),
        ReasoningEffort::High
    );
    assert_eq!(
        ReasoningEffort::Off.normalize_for_route(
            ProviderKind::Moonshot,
            "https://proxy.example/v1",
            codewhale_config::catalog::reviewed::constants::MOONSHOT_KIMI_K3_MODEL,
        ),
        ReasoningEffort::Off,
        "a neighboring gateway must not inherit direct-platform always-thinking semantics"
    );
}

#[test]
fn picker_uses_scenario() {
    // Scenario consolidation of: picker_uses_catalog_reasoning_efforts_for_grok_46, picker_uses_catalog_reasoning_efforts_for_grok_45
    // from picker_uses_catalog_reasoning_efforts_for_grok_46
    {
        let labels: Vec<&str> = crate::tui::model_picker::picker_efforts_for_route(
            ProviderKind::Xai,
            ProviderKind::Xai.provider().default_base_url(),
            codewhale_config::catalog::reviewed::constants::XAI_GROK_4_6_MODEL,
            false,
        )
        .iter()
        .map(|effort| effort.as_setting())
        .collect();
        assert_eq!(labels, vec!["auto", "low", "medium", "high", "xhigh"]);
    }
    // from picker_uses_catalog_reasoning_efforts_for_grok_45
    {
        let labels: Vec<&str> = crate::tui::model_picker::picker_efforts_for_route(
            ProviderKind::Xai,
            ProviderKind::Xai.provider().default_base_url(),
            codewhale_config::catalog::reviewed::constants::XAI_GROK_4_5_MODEL,
            false,
        )
        .iter()
        .map(|effort| effort.as_setting())
        .collect();
        assert_eq!(labels, vec!["auto", "low", "medium", "high"]);
    }
}

#[test]
fn reasoning_effort_preserves_grok_46_ladder_only_on_exact_xai_route() {
    let xai = codewhale_config::descriptors::defaults::DEFAULT_XAI_BASE_URL;
    let model = codewhale_config::catalog::reviewed::constants::XAI_GROK_4_6_MODEL;
    for (requested, expected) in [
        (ReasoningEffort::Off, ReasoningEffort::High),
        (ReasoningEffort::Low, ReasoningEffort::Low),
        (ReasoningEffort::Medium, ReasoningEffort::Medium),
        (ReasoningEffort::High, ReasoningEffort::High),
        (ReasoningEffort::XHigh, ReasoningEffort::XHigh),
        (ReasoningEffort::Max, ReasoningEffort::XHigh),
        (ReasoningEffort::Ultra, ReasoningEffort::XHigh),
        (ReasoningEffort::Auto, ReasoningEffort::Auto),
    ] {
        assert_eq!(
            requested.normalize_for_route(ProviderKind::Xai, xai, model),
            expected,
            "{requested:?}"
        );
    }
    assert_eq!(
        ReasoningEffort::Medium.normalize_for_route(
            ProviderKind::Xai,
            "https://gateway.example/v1",
            model,
        ),
        ReasoningEffort::High,
        "a custom endpoint must not inherit the official catalog effort ladder"
    );
    let custom = "https://gateway.example/v1";
    assert!(ReasoningEffort::offering_for_reasoning(ProviderKind::Xai, custom, model).is_none());
    assert!(ReasoningEffort::catalog_effort_values(ProviderKind::Xai, custom, model).is_none());
    assert_eq!(
        crate::tui::model_picker::picker_efforts_for_route(ProviderKind::Xai, custom, model, false),
        vec![
            ReasoningEffort::Auto,
            ReasoningEffort::Off,
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ]
    );
}

fn xai_grok_46_startup_config() -> Config {
    {
        let __cw_config_init_0_0 = Some("xai".to_string());
        let __cw_config_init_0_1 = Some(ProvidersConfig {
            xai: ProviderConfig {
                api_key: Some("xai-startup-test-key".to_string()),
                base_url: Some(
                    codewhale_config::descriptors::defaults::DEFAULT_XAI_BASE_URL.to_string(),
                ),
                model: Some(
                    codewhale_config::catalog::reviewed::constants::XAI_GROK_4_6_MODEL.to_string(),
                ),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_0_value: Config = Config::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value.providers = __cw_config_init_0_1;
        __cw_config_init_0_value
    }
}

fn xai_grok_46_startup_app(config: &Config) -> App {
    let mut options = test_options(false);
    options.model = codewhale_config::catalog::reviewed::constants::XAI_GROK_4_6_MODEL.to_string();
    App::new(options, config)
}

#[test]
fn app_new_scenario() {
    // Scenario consolidation of: app_new_uses_grok_46_official_high_when_effort_is_unset, app_new_maps_persisted_grok_46_off_to_high_and_max_to_xhigh, app_new_defaults_auto_compact_on_for_256k_class_models_when_unset, app_new_defaults_auto_compact_on_for_v4_class_models_when_unset, app_new_respects_explicit_auto_compact_false_for_256k_class_models, app_new_respects_explicit_auto_compact_false_for_v4_class_models, app_new_with_explicit_api_key_does_not_trigger_onboarding, app_new_respects_allow_shell_option_when_not_yolo
    // from app_new_uses_grok_46_official_high_when_effort_is_unset
    {
        let _lock = lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
        let config = xai_grok_46_startup_config();
        let app = xai_grok_46_startup_app(&config);

        assert_eq!(app.api_provider, ProviderKind::Xai);
        assert_eq!(
            app.model,
            codewhale_config::catalog::reviewed::constants::XAI_GROK_4_6_MODEL
        );
        assert_eq!(
            app.active_route_base_url,
            codewhale_config::descriptors::defaults::DEFAULT_XAI_BASE_URL
        );
        assert_eq!(app.reasoning_effort, ReasoningEffort::High);
        assert_eq!(app.reasoning_effort_display_label(), "high");
    }
    // from app_new_maps_persisted_grok_46_off_to_high_and_max_to_xhigh
    {
        let _lock = lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
        let config = xai_grok_46_startup_config();

        for (raw, expected, display) in [
            ("off", ReasoningEffort::High, "high"),
            ("max", ReasoningEffort::XHigh, "xhigh"),
            ("auto", ReasoningEffort::Auto, "auto"),
        ] {
            std::fs::write(
                tmp.path().join("settings.toml"),
                format!("reasoning_effort = \"{raw}\"\n"),
            )
            .expect("settings");

            let app = xai_grok_46_startup_app(&config);
            assert_eq!(app.reasoning_effort, expected, "raw setting {raw}");
            assert_eq!(app.reasoning_effort_display_label(), display);
        }
    }
    // from app_new_defaults_auto_compact_on_for_256k_class_models_when_unset
    {
        let _lock = lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);

        let mut options = test_options(false);
        options.model = "trinity-large-thinking".to_string();
        let app = App::new(options, &Config::default());

        assert!(app.auto_compact);
        assert!(!app.auto_compact_user_configured);
        assert_eq!(app.auto_compact_threshold_percent, 80.0);
        assert_eq!(app.compact_threshold, 195_584);
    }
    // from app_new_defaults_auto_compact_on_for_v4_class_models_when_unset
    {
        let _lock = lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);

        let mut options = test_options(false);
        options.model = "deepseek-v4-pro".to_string();
        let app = App::new(options, &Config::default());

        assert!(app.auto_compact);
        assert!(!app.auto_compact_user_configured);
        assert_eq!(app.auto_compact_threshold_percent, 80.0);
        assert_eq!(app.compact_threshold, 800_000);
    }
    // from app_new_respects_explicit_auto_compact_false_for_256k_class_models
    {
        let _lock = lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        std::fs::write(tmp.path().join("settings.toml"), "auto_compact = false\n")
            .expect("settings");
        let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);

        let mut options = test_options(false);
        options.model = "trinity-large-thinking".to_string();
        let app = App::new(options, &Config::default());

        assert!(!app.auto_compact);
        assert!(app.auto_compact_user_configured);
        assert_eq!(app.compact_threshold, 195_584);
    }
    // from app_new_respects_explicit_auto_compact_false_for_v4_class_models
    {
        let _lock = lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        std::fs::write(tmp.path().join("settings.toml"), "auto_compact = false\n")
            .expect("settings");
        let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);

        let mut options = test_options(false);
        options.model = "deepseek-v4-pro".to_string();
        let app = App::new(options, &Config::default());

        assert!(!app.auto_compact);
        assert!(app.auto_compact_user_configured);
        assert_eq!(app.compact_threshold, 800_000);
    }
    // from app_new_with_explicit_api_key_does_not_trigger_onboarding
    {
        let _lock = lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
        let _provider_env = EnvVarGuard::remove("CODEWHALE_PROVIDER");
        let _legacy_provider_env = EnvVarGuard::remove("DEEPSEEK_PROVIDER");

        let config =
            Config::default().with_legacy_root(Some("sk-test-onboarding-key".to_string()), None);
        let app = App::new(test_options(false), &config);
        assert!(
            !app.onboarding_needs_api_key,
            "explicit config.api_key must satisfy the onboarding check"
        );
    }
    // from app_new_respects_allow_shell_option_when_not_yolo
    {
        let mut options = test_options(false);
        options.allow_shell = false;
        options.start_in_agent_mode = true; // avoid coupling to settings.default_mode
        let app = App::new(options, &Config::default());
        assert!(!app.allow_shell);
    }
}

#[test]
fn cycle_effort_scenario() {
    // Scenario consolidation of: cycle_effort_walks_grok_46_official_ladder, cycle_effort_walks_grok_45_official_ladder_without_xhigh
    // from cycle_effort_walks_grok_46_official_ladder
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.set_provider_identity_record(
            codewhale_runtime::config::Config::default()
                .resolve_provider_identity(ProviderKind::Xai.as_str())
                .expect("captured fixture provider"),
        );
        app.auto_model = false;
        app.active_route_base_url =
            codewhale_config::descriptors::defaults::DEFAULT_XAI_BASE_URL.to_string();
        app.model = codewhale_config::catalog::reviewed::constants::XAI_GROK_4_6_MODEL.to_string();
        app.reasoning_effort = ReasoningEffort::High;

        let expected = [
            ReasoningEffort::XHigh,
            ReasoningEffort::Auto,
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
        ];
        for next in expected {
            app.cycle_effort();
            assert_eq!(app.reasoning_effort, next, "next {:?}", next);
        }
    }
    // from cycle_effort_walks_grok_45_official_ladder_without_xhigh
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.set_provider_identity_record(
            codewhale_runtime::config::Config::default()
                .resolve_provider_identity(ProviderKind::Xai.as_str())
                .expect("captured fixture provider"),
        );
        app.auto_model = false;
        app.active_route_base_url =
            codewhale_config::descriptors::defaults::DEFAULT_XAI_BASE_URL.to_string();
        app.model = codewhale_config::catalog::reviewed::constants::XAI_GROK_4_5_MODEL.to_string();
        app.reasoning_effort = ReasoningEffort::High;

        app.cycle_effort();
        assert_eq!(app.reasoning_effort, ReasoningEffort::Auto);
        app.cycle_effort();
        assert_eq!(app.reasoning_effort, ReasoningEffort::Low);
        app.cycle_effort();
        assert_eq!(app.reasoning_effort, ReasoningEffort::Medium);
        app.cycle_effort();
        assert_eq!(app.reasoning_effort, ReasoningEffort::High);
    }
}

#[test]
fn set_model_selection_normalizes_codex_fixed_model_effort() {
    let mut app = App::new(test_options(false), &Config::default());
    app.api_provider = ProviderKind::OpenaiCodex;
    app.reasoning_effort = ReasoningEffort::Off;
    app.reasoning_effort_preference = Some(ReasoningEffort::Off);

    app.set_model_selection("gpt-5.5-codex".to_string());

    assert_eq!(app.reasoning_effort, ReasoningEffort::Low);
    assert_eq!(app.reasoning_effort_preference, Some(ReasoningEffort::Off));
    assert!(!app.auto_model);
    assert_eq!(app.reasoning_effort_display_label(), "low");
}

#[test]
fn auto_model_selection_preserves_only_explicit_reasoning_effort() {
    let mut app = App::new(test_options(false), &Config::default());
    app.reasoning_effort = ReasoningEffort::Max;
    app.reasoning_effort_preference = None;

    app.set_model_selection("auto".to_string());

    assert!(app.auto_model);
    assert_eq!(app.reasoning_effort, ReasoningEffort::Auto);
    assert_eq!(app.reasoning_effort_preference, None);

    for (provider, requested, normalized) in [
        (
            ProviderKind::Deepseek,
            ReasoningEffort::Low,
            ReasoningEffort::High,
        ),
        (
            ProviderKind::OpenaiCodex,
            ReasoningEffort::Off,
            ReasoningEffort::Low,
        ),
    ] {
        app.api_provider = provider;
        app.auto_model = false;
        app.model = "fixed-model".to_string();
        app.reasoning_effort = normalized;
        app.reasoning_effort_preference = Some(requested);

        app.set_model_selection("auto".to_string());

        assert_eq!(app.reasoning_effort, requested, "{provider:?}");
        assert_eq!(
            app.reasoning_effort_preference,
            Some(requested),
            "{provider:?}"
        );
    }
}

#[test]
fn app_new_normalizes_saved_codex_reasoning_effort() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let _token = EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", "test-codex-startup-token");
    let config = {
        let __cw_config_init_2_0 = Some("openai-codex".to_string());
        let __cw_config_init_2_1 = Some(ProvidersConfig {
            openai_codex: ProviderConfig {
                model: Some(
                    codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL.to_string(),
                ),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_2_value: Config = Config::default();
        __cw_config_init_2_value.provider = __cw_config_init_2_0;
        __cw_config_init_2_value.providers = __cw_config_init_2_1;
        __cw_config_init_2_value
    };

    for (raw, expected, display) in [
        ("off", ReasoningEffort::Low, "low"),
        ("auto", ReasoningEffort::Medium, "medium"),
        ("max", ReasoningEffort::Max, "max"),
        ("xhigh", ReasoningEffort::XHigh, "xhigh"),
        ("ultra", ReasoningEffort::Ultra, "ultra"),
    ] {
        std::fs::write(
            tmp.path().join("settings.toml"),
            format!("reasoning_effort = \"{raw}\"\n"),
        )
        .expect("settings");

        let app = App::new(test_options(false), &config);

        assert_eq!(app.api_provider, ProviderKind::OpenaiCodex);
        assert_eq!(app.reasoning_effort, expected, "raw setting {raw}");
        assert_eq!(
            app.reasoning_effort_preference,
            Some(ReasoningEffort::from_setting(raw)),
            "raw setting {raw}"
        );
        assert_eq!(app.reasoning_effort_display_label(), display);
    }
}

#[test]
fn app_new_exposes_direct_moonshot_k3_off_as_effective_low() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    std::fs::write(
        tmp.path().join("settings.toml"),
        "reasoning_effort = \"off\"\n",
    )
    .expect("settings");
    let config = {
        let __cw_config_init_3_0 = Some("moonshot".to_string());
        let __cw_config_init_3_1 = Some(ProvidersConfig {
            moonshot: ProviderConfig {
                api_key: Some("moonshot-startup-test-key".to_string()),
                base_url: Some(
                    codewhale_config::descriptors::defaults::DEFAULT_MOONSHOT_BASE_URL.to_string(),
                ),
                model: Some(
                    codewhale_config::catalog::reviewed::constants::MOONSHOT_KIMI_K3_MODEL
                        .to_string(),
                ),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_3_value: Config = Config::default();
        __cw_config_init_3_value.provider = __cw_config_init_3_0;
        __cw_config_init_3_value.providers = __cw_config_init_3_1;
        __cw_config_init_3_value
    };

    let mut options = test_options(false);
    options.model =
        codewhale_config::catalog::reviewed::constants::MOONSHOT_KIMI_K3_MODEL.to_string();
    let app = App::new(options, &config);

    assert_eq!(app.api_provider, ProviderKind::Moonshot);
    assert_eq!(
        app.model,
        codewhale_config::catalog::reviewed::constants::MOONSHOT_KIMI_K3_MODEL
    );
    assert_eq!(
        app.active_route_base_url,
        codewhale_config::descriptors::defaults::DEFAULT_MOONSHOT_BASE_URL
    );
    assert_eq!(app.reasoning_effort, ReasoningEffort::Low);
    assert_eq!(app.reasoning_effort_display_label(), "low");
}

#[test]
fn codex_startup_threads_fresh_roster_context_into_active_route_limits() {
    let _lock = lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let canonical_home = tmp
        .path()
        .canonicalize()
        .expect("canonical private fixture home");
    let config_path = tmp.path().join("config.toml");
    let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let _home = EnvVarGuard::set("CODEWHALE_HOME", &canonical_home);
    let mut config = {
        let __cw_config_init_4_0 = Some("openai-codex".to_string());
        let __cw_config_init_4_1 = Some(ProvidersConfig {
            openai_codex: ProviderConfig {
                model: Some(
                    codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL.to_string(),
                ),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_4_value: Config = Config::default();
        __cw_config_init_4_value.provider = __cw_config_init_4_0;
        __cw_config_init_4_value.providers = __cw_config_init_4_1;
        __cw_config_init_4_value
    };

    codewhale_runtime::oauth::install_test_chatgpt_registration(&mut config)
        .expect("owned ChatGPT registration");
    codewhale_runtime::codex_model_roster::install_test_chatgpt_roster_with_metadata(
        &config,
        vec![codewhale_runtime::codex_model_roster::CodexModelMetadata {
            id: codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL.to_string(),
            display_name: None,
            context_window: Some(128_000),
            reasoning: Some(true),
            efforts: vec!["high".to_string()],
        }],
    )
    .expect("account-scoped roster");

    let mut options = test_options(false);
    options.model = codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL.to_string();
    let app = App::new(options, &config);

    assert_eq!(app.api_provider, ProviderKind::OpenaiCodex);
    assert_eq!(
        app.active_route_limits
            .and_then(|limits| limits.context_tokens),
        Some(128_000)
    );
    assert_eq!(
        codewhale_engine::route_budget::route_context_window_tokens(
            app.api_provider,
            &app.model,
            app.active_route_limits,
        ),
        128_000
    );
}

#[test]
fn settings_default_provider_auth_check_uses_provider_scoped_key() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    std::fs::write(
        tmp.path().join("settings.toml"),
        "default_provider = \"openai\"\n",
    )
    .expect("settings");
    let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let _deepseek_key = EnvVarGuard::remove("DEEPSEEK_API_KEY");
    let _openai_key = EnvVarGuard::remove("OPENAI_API_KEY");

    let config = {
        let __cw_config_init_5_0 = Some(ProvidersConfig {
            openai: ProviderConfig {
                api_key: Some("openai-config-key".to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_5_value: Config = Config::default();
        __cw_config_init_5_value.providers = __cw_config_init_5_0;
        __cw_config_init_5_value
    };

    let app = App::new(test_options(false), &config);

    assert_eq!(app.api_provider, ProviderKind::Openai);
    assert!(
        !app.onboarding_needs_api_key,
        "OpenAI provider config key should satisfy startup auth without a DeepSeek key"
    );
    assert!(!app.api_key_env_only);
}

#[test]
fn saved_startup_provider_overrides_config_file_provider() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    std::fs::write(
        tmp.path().join("settings.toml"),
        "default_provider = \"deepseek\"\ndefault_model = \"deepseek-v4-pro\"\n",
    )
    .expect("settings");
    let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);

    let config = {
        let __cw_config_init_6_0 = Some("xiaomi-mimo".to_string());
        let __cw_config_init_6_1 = Some(ProvidersConfig {
            deepseek: ProviderConfig {
                api_key: Some("deepseek-config-key".to_string()),
                model: Some("deepseek-v4-pro".to_string()),
                ..ProviderConfig::default()
            },
            xiaomi_mimo: ProviderConfig {
                api_key: Some("mimo-config-key".to_string()),
                model: Some("mimo-v2.5-pro".to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_6_value: Config = Config::default();
        __cw_config_init_6_value.provider = __cw_config_init_6_0;
        __cw_config_init_6_value.providers = __cw_config_init_6_1;
        __cw_config_init_6_value
    };

    let mut options = test_options(false);
    options.model = "mimo-v2.5-pro".to_string();
    let app = App::new(options, &config);

    assert_eq!(app.api_provider, ProviderKind::Deepseek);
    assert_eq!(app.model, "deepseek-v4-pro");
    assert!(
        !app.onboarding_needs_api_key,
        "the saved startup provider's config key should satisfy startup auth"
    );
}

#[test]
fn selected_fleet_operator_outranks_remembered_startup_route_and_reasoning() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    std::fs::write(
        tmp.path().join("settings.toml"),
        r#"default_provider = "openrouter"
reasoning_effort = "off"

[provider_models]
deepseek = "deepseek-v4-pro"
openrouter = "openai/gpt-5"
"#,
    )
    .expect("settings");
    let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);

    let config = {
        let __cw_config_init_7_0 = Some("deepseek".to_string());
        let __cw_config_init_7_1 = Some("high".to_string());
        let __cw_config_init_7_2 = true;
        let __cw_config_init_7_3 = true;
        let __cw_config_init_7_4 = Some(ProvidersConfig {
            deepseek: ProviderConfig {
                api_key: Some("deepseek-config-key".to_string()),
                model: Some("deepseek-v4-flash-vision-exp".to_string()),
                ..ProviderConfig::default()
            },
            openrouter: ProviderConfig {
                api_key: Some("openrouter-config-key".to_string()),
                model: Some("openai/gpt-5".to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_7_value: Config = Config::default();
        __cw_config_init_7_value.provider = __cw_config_init_7_0;
        __cw_config_init_7_value.reasoning_effort = __cw_config_init_7_1;
        __cw_config_init_7_value.fleet_operator_route_applied = __cw_config_init_7_2;
        __cw_config_init_7_value.fleet_operator_reasoning_applied = __cw_config_init_7_3;
        __cw_config_init_7_value.providers = __cw_config_init_7_4;
        __cw_config_init_7_value
    };

    let mut options = test_options(false);
    options.model = "deepseek-v4-flash-vision-exp".to_string();
    let app = App::new(options, &config);

    assert_eq!(app.api_provider, ProviderKind::Deepseek);
    assert_eq!(app.model, "deepseek-v4-flash-vision-exp");
    assert_eq!(app.reasoning_effort, ReasoningEffort::High);
    assert_eq!(app.reasoning_effort_preference, Some(ReasoningEffort::High));
}

#[test]
fn explicit_launch_provider_overrides_saved_startup_provider() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    std::fs::write(
        tmp.path().join("settings.toml"),
        "default_provider = \"deepseek\"\ndefault_model = \"deepseek-v4-pro\"\n",
    )
    .expect("settings");
    let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let _provider = EnvVarGuard::set("CODEWHALE_PROVIDER", "xiaomi-mimo");

    let config = {
        let __cw_config_init_8_0 = Some("xiaomi-mimo".to_string());
        let __cw_config_init_8_1 = Some(ProvidersConfig {
            deepseek: ProviderConfig {
                api_key: Some("deepseek-config-key".to_string()),
                model: Some("deepseek-v4-pro".to_string()),
                ..ProviderConfig::default()
            },
            xiaomi_mimo: ProviderConfig {
                api_key: Some("mimo-config-key".to_string()),
                model: Some("mimo-v2.5-pro".to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_8_value: Config = Config::default();
        __cw_config_init_8_value.provider = __cw_config_init_8_0;
        __cw_config_init_8_value.providers = __cw_config_init_8_1;
        __cw_config_init_8_value
    };

    let mut options = test_options(false);
    options.model = "mimo-v2.5-pro".to_string();
    let app = App::new(options, &config);

    assert_eq!(app.api_provider, ProviderKind::XiaomiMimo);
    assert_eq!(app.model, "mimo-v2.5-pro");
}

#[test]
fn pending_turn_cost_moves_displayed_total_mid_turn() {
    let mut app = App::new(test_options(false), &Config::default());

    // Two model calls land per-step receipts while the turn is still running:
    // the displayed total must move now, not at TurnComplete (#5578).
    app.accrue_pending_turn_cost_estimate(CostEstimate::usd_only(0.06));
    app.accrue_pending_turn_cost_estimate(CostEstimate::usd_only(0.04));
    assert_eq!(
        app.displayed_session_cost_for_currency(CostCurrency::Usd),
        0.1
    );
    assert_eq!(app.session_cost_for_currency(CostCurrency::Usd), 0.1);

    // TurnComplete: provisional hands off to the authoritative cumulative
    // price. A slightly lower settled figure must not make the display
    // reverse (#244), and nothing may count twice.
    app.clear_pending_turn_cost();
    app.accrue_session_cost_estimate(CostEstimate::usd_only(0.09));
    assert_eq!(app.session.session_cost, 0.09);
    assert_eq!(
        app.displayed_session_cost_for_currency(CostCurrency::Usd),
        0.1
    );
}

#[test]
fn pending_turn_usage_moves_token_surfaces_without_double_counting() {
    let mut app = App::new(test_options(false), &Config::default());
    let usage = Usage {
        input_tokens: 100,
        output_tokens: 20,
        prompt_cache_hit_tokens: Some(60),
        prompt_cache_miss_tokens: Some(40),
        prompt_cache_write_tokens: Some(5),
        ..Usage::default()
    };

    app.session.accrue_pending_turn_usage(&usage);
    assert_eq!(app.session.displayed_total_tokens(), 120);
    assert_eq!(app.session.displayed_total_input_tokens(), 100);
    assert_eq!(app.session.displayed_total_output_tokens(), 20);
    assert_eq!(app.session.displayed_total_cache_hit_tokens(), 60);
    assert_eq!(app.session.displayed_total_cache_miss_tokens(), 35);
    assert_eq!(app.session.displayed_total_cache_write_tokens(), 5);

    app.session.clear_pending_turn_usage();
    app.session.total_tokens = 120;
    app.session.total_input_tokens = 100;
    app.session.total_output_tokens = 20;
    app.session.total_cache_hit_tokens = 60;
    app.session.total_cache_miss_tokens = 35;
    app.session.total_cache_write_tokens = 5;
    assert_eq!(app.session.displayed_total_tokens(), 120);
    assert_eq!(app.session.displayed_total_cache_write_tokens(), 5);
}

#[test]
fn context_pressure_toast_kind_is_not_inferred_from_display_text() {
    let mut app = App::new(test_options(false), &Config::default());
    app.sticky_status = Some(StatusToast::new(
        "Context high: 90%",
        StatusToastLevel::Warning,
        None,
    ));
    assert!(!app.dismiss_context_pressure_warning());
    assert!(app.sticky_status.is_some());

    app.sticky_status = Some(StatusToast::context_pressure(
        "localized pressure warning",
        codewhale_runtime::context_budget::PressureLevel::High,
    ));
    assert!(app.dismiss_context_pressure_warning());
    assert!(app.sticky_status.is_none());
}

#[test]
fn critical_context_pressure_remains_visible_over_transient_info_toasts() {
    let mut app = App::new(test_options(false), &Config::default());
    app.sticky_status = Some(StatusToast::context_pressure(
        "Context critical: 95%",
        codewhale_runtime::context_budget::PressureLevel::Critical,
    ));
    app.push_status_toast("Saved", StatusToastLevel::Info, None);

    assert_eq!(
        app.active_status_toast(crate::tui::underwater::ShellPhase::Working)
            .map(|toast| toast.text),
        Some("Context critical: 95%".to_string())
    );
}

#[test]
fn cny_display_scenario() {
    // Scenario consolidation of: cny_display_falls_back_to_usd_for_usd_only_costs, cny_display_keeps_cny_when_costs_have_cny_rates, cny_display_does_not_fall_back_to_an_unproven_usd_total
    // from cny_display_falls_back_to_usd_for_usd_only_costs
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.cost_currency = CostCurrency::Cny;
        app.accrue_session_cost_estimate(CostEstimate::usd_only(0.42));
        app.session.cost_priced_turns = 1;

        let displayed = app.displayed_session_cost_for_currency(CostCurrency::Cny);

        assert_eq!(displayed, 0.42);
        assert_eq!(app.session_cost_for_currency(CostCurrency::Cny), 0.42);
        assert_eq!(app.format_cost_amount(displayed), "$0.42");
    }
    // from cny_display_keeps_cny_when_costs_have_cny_rates
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.cost_currency = CostCurrency::Cny;
        app.accrue_session_cost_estimate(CostEstimate {
            usd: 0.42,
            cny: 2.5,
        });
        app.session.cost_priced_turns = 1;
        app.session.cost_cny_priced_turns = 1;

        let displayed = app.displayed_session_cost_for_currency(CostCurrency::Cny);

        assert_eq!(displayed, 2.5);
        assert_eq!(app.format_cost_amount(displayed), "¥2.50");
    }
    // from cny_display_does_not_fall_back_to_an_unproven_usd_total
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.cost_currency = CostCurrency::Cny;
        app.accrue_session_cost_estimate(CostEstimate::usd_only(0.42));

        assert_eq!(
            app.cost_display_currency(CostCurrency::Cny),
            CostCurrency::Cny
        );
        assert_eq!(
            app.displayed_session_cost_for_currency(CostCurrency::Cny),
            0.0
        );
    }
}

#[test]
fn subscription_route_hides_stale_session_dollars_in_footer() {
    let mut app = App::new(test_options(false), &Config::default());
    app.accrue_session_cost_estimate(CostEstimate::usd_only(12.34));
    app.billing_presentation =
        codewhale_engine::route_billing::BillingPresentation::Subscription("Codex OAuth quota");
    // Stale unaudited dollars must never render on a plan route; the usage
    // chip carries the plan-aware line instead of money or silence.
    let chip = app.cumulative_usage_chip();
    assert!(
        !matches!(chip, codewhale_engine::route_billing::UsageChip::Money(_)),
        "{chip:?}"
    );
    let rendered = codewhale_engine::route_billing::format_usage_chip(
        &chip,
        codewhale_localization::Locale::En,
    )
    .unwrap_or_default();
    assert!(!rendered.contains('$'), "{rendered}");
    assert!(rendered.contains("Codex OAuth quota"), "{rendered}");
}

#[test]
fn provider_switch_keeps_audited_cumulative_spend_visible() {
    let mut app = App::new(test_options(false), &Config::default());
    let usage = codewhale_models::Usage {
        input_tokens: 10_000,
        output_tokens: 1_000,
        ..Default::default()
    };
    let priced = codewhale_engine::pricing::audit_turn_cost_for_provider_at(
        ProviderKind::Deepseek,
        "deepseek-v4-flash",
        &usage,
        chrono::Utc::now(),
    );
    app.record_turn_cost_audit(&priced);
    app.accrue_session_cost_estimate(priced.estimate.expect("priced"));

    app.api_provider = ProviderKind::OpenaiCodex;
    app.model = "gpt-5.5".to_string();
    app.billing_presentation =
        codewhale_engine::route_billing::BillingPresentation::Subscription("Codex OAuth quota");
    assert!(matches!(
        app.cumulative_usage_chip(),
        codewhale_engine::route_billing::UsageChip::Money(_)
    ));
    assert!(
        codewhale_engine::route_billing::format_usage_chip(
            &app.cumulative_usage_chip(),
            app.ui_locale
        )
        .is_some_and(|label| !label.is_empty())
    );

    let unknown = codewhale_engine::pricing::audit_turn_cost_for_route_at(
        ProviderKind::Openai,
        "gpt-5.5",
        Some(codewhale_engine::pricing::UNCLASSIFIED_BILLING_SURFACE),
        &usage,
        chrono::Utc::now(),
    );
    app.record_turn_cost_audit(&unknown);
    assert!(matches!(
        app.cumulative_usage_chip(),
        codewhale_engine::route_billing::UsageChip::PricedSubtotal { legacy: false, .. }
    ));
}

#[test]
fn slash_command_classifier_treats_absolute_path_as_message() {
    assert!(looks_like_slash_command_input("/"));
    assert!(looks_like_slash_command_input("/help"));
    assert!(looks_like_slash_command_input("/model deepseek-v4-pro"));
    assert!(!looks_like_slash_command_input("/ hello"));
    assert!(!looks_like_slash_command_input("  / hello"));
    assert!(!looks_like_slash_command_input(
        "/usr/lib/x86_64-linux-gnu/ 是标准路径吗？"
    ));
}

#[test]
fn bang_shell_scenario() {
    // Scenario consolidation of: bang_shell_prefix_parses_compact_and_spaced_forms, bang_shell_prefix_rejects_empty_command
    // from bang_shell_prefix_parses_compact_and_spaced_forms
    {
        assert_eq!(shell_command_from_bang_input("!pwd"), Ok(Some("pwd")));
        assert_eq!(shell_command_from_bang_input("! pwd"), Ok(Some("pwd")));
        assert_eq!(
            shell_command_from_bang_input("  !  cargo test -p codewhale-tui sidebar"),
            Ok(Some("cargo test -p codewhale-tui sidebar"))
        );
        assert_eq!(shell_command_from_bang_input("normal message"), Ok(None));
    }
    // from bang_shell_prefix_rejects_empty_command
    {
        assert_eq!(
            shell_command_from_bang_input("!"),
            Err("Usage: ! <shell command>")
        );
        assert_eq!(
            shell_command_from_bang_input("!   "),
            Err("Usage: ! <shell command>")
        );
    }
}

#[test]
fn stop_word_matching_requires_one_token() {
    let words = vec!["stop".to_string(), "wait".to_string(), "pause".to_string()];
    assert_eq!(is_stop_word("STOP", &words).as_deref(), Some("stop"));
    assert_eq!(is_stop_word("+ stop", &words).as_deref(), Some("stop"));
    assert_eq!(is_stop_word("!wait", &words).as_deref(), Some("wait"));
    assert_eq!(is_stop_word("pause.", &words).as_deref(), Some("pause"));
    assert!(is_stop_word("please stop", &words).is_none());
    assert!(is_stop_word("don't stop", &words).is_none());
}

#[test]
fn submit_input_records_absolute_slash_path_as_message_history() {
    let mut app = App::new(test_options(false), &Config::default());
    let input = "/usr/lib/x86_64-linux-gnu/ 是标准路径吗？";
    app.input = input.to_string();
    app.cursor_position = input.chars().count();

    let submitted = app.submit_input().expect("expected submitted input");

    assert_eq!(submitted, input);
    assert_eq!(app.input_history.last().map(String::as_str), Some(input));
}

#[test]
fn submit_input_recalls_slash_commands_and_persists_them_for_the_next_session() {
    let _env_lock = lock_test_env();
    let home = tempfile::tempdir().expect("isolated home");
    let _home = EnvVarGuard::set("HOME", home.path());
    let _profile = EnvVarGuard::set("USERPROFILE", home.path());
    let _state = EnvVarGuard::set("CODEWHALE_HOME", home.path().join(".codewhale"));
    let mut app = App::new(test_options(false), &Config::default());
    app.input_history.clear();
    for input in ["/theme", "/theme", "/theme", "/compact", "/compact"] {
        app.input = input.to_string();
        app.cursor_position = input.chars().count();
        assert_eq!(app.submit_input().as_deref(), Some(input));
    }
    assert_eq!(app.input_history, ["/theme", "/compact"]);
    app.history_up();
    assert_eq!(app.input, "/compact");
    app.history_up();
    assert_eq!(app.input, "/theme");

    crate::composer_history::flush_history_writer_for_tests(std::time::Duration::from_secs(5));
    let mut resumed = App::new(test_options(false), &Config::default());
    resumed.history_up();
    assert_eq!(resumed.input, "/compact");
    resumed.history_up();
    assert_eq!(resumed.input, "/theme");
}

#[test]
fn restore_last_scenario() {
    // Scenario consolidation of: restore_last_submitted_prompt_rehydrates_empty_composer, restore_last_submitted_prompt_preserves_existing_draft, restore_last_cleared_input_restores_saved_draft, restore_last_cleared_input_does_nothing_when_composer_not_empty
    // from restore_last_submitted_prompt_rehydrates_empty_composer
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.last_submitted_prompt = Some("fix the typo\nand retry".to_string());

        assert!(app.restore_last_submitted_prompt_if_empty());

        assert_eq!(app.input, "fix the typo\nand retry");
        assert_eq!(app.cursor_position, app.input.chars().count());
        assert!(app.needs_redraw);
    }
    // from restore_last_submitted_prompt_preserves_existing_draft
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.last_submitted_prompt = Some("previous prompt".to_string());
        app.input = "new draft".to_string();
        app.cursor_position = app.input.chars().count();

        assert!(!app.restore_last_submitted_prompt_if_empty());

        assert_eq!(app.input, "new draft");
        assert_eq!(app.cursor_position, "new draft".chars().count());
    }
    // from restore_last_cleared_input_restores_saved_draft
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "previous".to_string();
        app.cursor_position = 8;
        app.clear_input_recoverable();
        assert!(app.input.is_empty());

        let restored = app.restore_last_cleared_input_if_empty();
        assert!(restored);
        assert_eq!(app.input, "previous");
        assert!(app.clear_undo_buffer.is_none());
    }
    // from restore_last_cleared_input_does_nothing_when_composer_not_empty
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.clear_undo_buffer = Some("old".to_string());
        app.input = "current".to_string();
        assert!(!app.restore_last_cleared_input_if_empty());
    }
}

#[test]
fn composer_strips_scenario() {
    // Scenario consolidation of: composer_strips_raw_sgr_mouse_report_when_mouse_capture_is_enabled, composer_strips_corrupted_mouse_report_burst, composer_strips_raw_sgr_mouse_report_when_mouse_capture_is_disabled, composer_strips_tail_only_mouse_report_burst_when_mouse_capture_is_disabled, composer_strips_osc8_hyperlink_fragment, composer_strips_closing_osc8_fragment, composer_strips_kitty_keyboard_protocol_fragment, composer_strips_dec_private_mode_set_reset_fragments, composer_strips_mixed_control_sequence_burst
    // from composer_strips_raw_sgr_mouse_report_when_mouse_capture_is_enabled
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.use_mouse_capture = true;

        app.insert_str("[<35;44;18M");

        assert_eq!(app.input, "");
        assert_eq!(app.cursor_position, 0);
    }
    // from composer_strips_corrupted_mouse_report_burst
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.use_mouse_capture = true;
        app.insert_str("draft ");
        let leaked = "43;19M[<35;44;18M[<35;45;18M5;46;18M;48;18M";

        app.insert_str(leaked);

        assert_eq!(app.input, "draft ");
        assert_eq!(app.cursor_position, "draft ".chars().count());
    }
    // from composer_strips_raw_sgr_mouse_report_when_mouse_capture_is_disabled
    {
        let mut app = App::new(test_options(false), &Config::default());

        app.insert_str("[<35;44;18M");

        assert_eq!(app.input, "");
        assert_eq!(app.cursor_position, 0);
    }
    // from composer_strips_tail_only_mouse_report_burst_when_mouse_capture_is_disabled
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.insert_str("draft ");

        app.insert_str(";76;20M35;74;22M35;73;23M");

        assert_eq!(app.input, "draft ");
        assert_eq!(app.cursor_position, "draft ".chars().count());
    }
    // from composer_strips_osc8_hyperlink_fragment
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.use_mouse_capture = true;
        app.insert_str("draft ");

        // OSC 8 prefix with URL body but no terminator delivered yet —
        // exactly what crossterm hands us if its event reader is
        // interrupted mid-sequence and the leading ESC is consumed by the
        // parser before the rest gets reclassified as Char(c).
        app.insert_str("]8;;https://example.com");

        assert_eq!(app.input, "draft ");
        assert_eq!(app.cursor_position, "draft ".chars().count());
    }
    // from composer_strips_closing_osc8_fragment
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.use_mouse_capture = true;
        app.insert_str("hello ");

        // The closing wrapper `]8;;` (with a stray ST `\\` from a
        // chopped escape) can arrive on its own when the parser ate
        // the start of the sequence in a previous read but caught the
        // tail as keystrokes.
        app.insert_str("]8;;\\");

        assert_eq!(app.input, "hello ");
        assert_eq!(app.cursor_position, "hello ".chars().count());
    }
    // from composer_strips_kitty_keyboard_protocol_fragment
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.use_mouse_capture = true;
        app.insert_str("ready ");

        // Kitty keyboard protocol responses look like `\x1b[?1u`,
        // `\x1b[>1u`, `\x1b[<1u`, or `\x1b[?u`. With the ESC consumed,
        // the tail shape is `[?…u`, `[>…u`, or `[<…u`.
        app.insert_str("[?1u[>1u[<1u[?u");

        assert_eq!(app.input, "ready ");
        assert_eq!(app.cursor_position, "ready ".chars().count());
    }
    // from composer_strips_dec_private_mode_set_reset_fragments
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.use_mouse_capture = true;
        app.insert_str("ok ");

        // Regression for #2592: DEC private mode set/reset chatter ends in
        // `h`/`l`, not `u`, so the `u`-only terminator used to leak the
        // leading `[`. Bracketed paste, mouse capture, focus reporting, and
        // synchronized output all leak during dense streaming.
        app.insert_str("[?2004h[?2004l[?1000h[?1004h[?2026h[?25l");

        assert_eq!(app.input, "ok ");
        assert_eq!(app.cursor_position, "ok ".chars().count());
    }
    // from composer_strips_mixed_control_sequence_burst
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.use_mouse_capture = true;
        app.insert_str("hi");

        // Mixed dense burst combining all three fragment families
        // described in #1915.
        app.insert_str("[<35;44;18M]8;;https://example.com[?1u");

        assert_eq!(app.input, "hi");
        assert_eq!(app.cursor_position, 2);
    }
}

#[test]
fn composer_preserves_scenario() {
    // Scenario consolidation of: composer_preserves_draft_suffix_when_stripping_mouse_report, composer_preserves_numeric_draft_when_stripping_mouse_report
    // from composer_preserves_draft_suffix_when_stripping_mouse_report
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.use_mouse_capture = true;
        app.insert_str("commit -m");

        app.insert_str("[<65;44;18M");

        assert_eq!(app.input, "commit -m");
        assert_eq!(app.cursor_position, "commit -m".chars().count());
    }
    // from composer_preserves_numeric_draft_when_stripping_mouse_report
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.use_mouse_capture = true;
        app.insert_str("123");

        app.insert_str("[<65;44;18M");

        assert_eq!(app.input, "123");
        assert_eq!(app.cursor_position, 3);
    }
}

#[test]
fn composer_keeps_scenario() {
    // Scenario consolidation of: composer_keeps_coordinate_like_text_when_mouse_capture_is_disabled, composer_keeps_normal_bracket_text_with_mouse_capture_enabled, composer_keeps_coordinate_like_text_with_mouse_capture_enabled, composer_keeps_bracket_question_word_text, composer_keeps_legitimate_url_text_with_mouse_capture_enabled, composer_keeps_legitimate_bracket_question_text, composer_keeps_legitimate_closing_bracket_digit_text
    // from composer_keeps_coordinate_like_text_when_mouse_capture_is_disabled
    {
        let mut app = App::new(test_options(false), &Config::default());

        app.insert_str("Size 12;34M");

        assert_eq!(app.input, "Size 12;34M");
        assert_eq!(app.cursor_position, "Size 12;34M".chars().count());
    }
    // from composer_keeps_normal_bracket_text_with_mouse_capture_enabled
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.use_mouse_capture = true;

        app.insert_str("Use [<tag>] normally");

        assert_eq!(app.input, "Use [<tag>] normally");
    }
    // from composer_keeps_coordinate_like_text_with_mouse_capture_enabled
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.use_mouse_capture = true;

        app.insert_str("Size 12;34M");

        assert_eq!(app.input, "Size 12;34M");
    }
    // from composer_keeps_bracket_question_word_text
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.use_mouse_capture = true;

        // The `h`/`l` terminator only counts after a numeric parameter, so
        // ordinary prose where a letter follows `[?` directly is preserved.
        app.insert_str("[?help] and [?later]");

        assert_eq!(app.input, "[?help] and [?later]");
    }
    // from composer_keeps_legitimate_url_text_with_mouse_capture_enabled
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.use_mouse_capture = true;

        // URLs typed by the user must survive the filter — only
        // recognized control-sequence shapes are stripped.
        app.insert_str("see https://example.com/path?a=1&b=2 for info");

        assert_eq!(app.input, "see https://example.com/path?a=1&b=2 for info");
    }
    // from composer_keeps_legitimate_bracket_question_text
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.use_mouse_capture = true;

        // Text that uses brackets, question marks, and lowercase `u` —
        // shapes that overlap Kitty fragments — must not be eaten.
        app.insert_str("[is this ok?] sure");

        assert_eq!(app.input, "[is this ok?] sure");
    }
    // from composer_keeps_legitimate_closing_bracket_digit_text
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.use_mouse_capture = true;

        // Plain `]8` followed by spaces and words must survive — only
        // the OSC 8 shape `]8;` (with the mandatory `;` separator)
        // should be treated as a fragment.
        app.insert_str("array[]8 elements");

        assert_eq!(app.input, "array[]8 elements");
    }
}

// === Bug #1915: broader terminal control-sequence fragments leaking
// into the composer during dense streaming output. The narrow SGR
// mouse-report filter installed in e63a4ba4a covers `[<…M` style
// bursts, but not OSC 8 hyperlink fragments (`]8;;http…`) or Kitty
// keyboard protocol responses (`[?u`, `[>1u`). These can arrive when
// crossterm's event reader is mid-sequence and the unparsed tail is
// delivered as individual Char(c) keystrokes that land in the input.

// initial_onboarding_state tests
// These pin the logic that decides whether the TUI shows the
// first missing decision or goes straight to the chat view. Getting this
// wrong either locks first-run users out of provider setup or nags returning
// users whose configuration is already usable.

#[test]
fn skip_onboarding_suppresses_all_onboarding_states() {
    assert_eq!(
        initial_onboarding_state(true, false, true, true, true),
        OnboardingState::None
    );
    assert_eq!(
        initial_onboarding_state(true, true, true, true, true),
        OnboardingState::None
    );
}

#[test]
fn fully_configured_returning_user_skips_onboarding() {
    assert_eq!(
        initial_onboarding_state(false, true, false, false, false),
        OnboardingState::None
    );
}

#[test]
fn returning_user_missing_api_key_starts_at_composer() {
    assert_eq!(
        initial_onboarding_state(false, true, false, true, false),
        OnboardingState::None
    );
    assert_eq!(
        initial_onboarding_state(false, true, false, true, true),
        OnboardingState::None
    );
}

#[test]
fn first_run_user_without_a_key_starts_at_composer() {
    assert_eq!(
        initial_onboarding_state(false, false, true, true, true),
        OnboardingState::None
    );
    assert_eq!(
        initial_onboarding_state(false, false, false, true, true),
        OnboardingState::None
    );
}

#[test]
fn first_run_user_with_a_key_starts_at_composer() {
    assert_eq!(
        initial_onboarding_state(false, false, false, false, true),
        OnboardingState::None
    );
    assert_eq!(
        initial_onboarding_state(false, false, false, false, false),
        OnboardingState::None
    );
}

#[test]
fn onboarding_workspace_trust_gate_only_fires_for_onboarded_user() {
    assert!(onboarding_is_workspace_trust_gate(false, true, false, true));
    assert!(!onboarding_is_workspace_trust_gate(true, true, false, true));
    assert!(!onboarding_is_workspace_trust_gate(false, true, true, true));
    assert!(!onboarding_is_workspace_trust_gate(
        false, false, false, true
    ));
}

#[test]
fn onboarded_user_still_gets_workspace_trust_prompt_when_needed() {
    assert_eq!(
        initial_onboarding_state(false, true, false, false, true),
        OnboardingState::TrustDirectory
    );
}

// App::new tests: missing key is detected

#[test]
fn app_new_detects_missing_api_key_with_default_config() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let _provider_env = EnvVarGuard::remove("CODEWHALE_PROVIDER");
    let _legacy_provider_env = EnvVarGuard::remove("DEEPSEEK_PROVIDER");
    let _api_key_envs: Vec<_> = [
        "DEEPSEEK_API_KEY",
        "NVIDIA_API_KEY",
        "NVIDIA_NIM_API_KEY",
        "OPENAI_API_KEY",
        "ATLASCLOUD_API_KEY",
        "WANJIE_ARK_API_KEY",
        "WANJIE_API_KEY",
        "WANJIE_MAAS_API_KEY",
        "OPENROUTER_API_KEY",
        "NOVITA_API_KEY",
        "FIREWORKS_API_KEY",
        "SILICONFLOW_API_KEY",
        "MOONSHOT_API_KEY",
        "KIMI_API_KEY",
        "SGLANG_API_KEY",
        "VLLM_API_KEY",
        "OLLAMA_API_KEY",
    ]
    .into_iter()
    .map(EnvVarGuard::remove)
    .collect();

    // Config::default() carries no api_key, and this test isolates process
    // env/settings so previous tests or developer shells cannot satisfy it.
    let app = App::new(test_options(false), &Config::default());
    assert!(
        app.onboarding_needs_api_key,
        "default config (no key) must set onboarding_needs_api_key"
    );
}

#[test]
fn first_run_app_without_a_key_opens_provider_setup() {
    let _lock = lock_test_env();
    let home = tempfile::TempDir::new().expect("isolated first-run home");
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path().to_string_lossy().as_ref());
    let config_path = home.path().join("config.toml");
    let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let _provider_env = EnvVarGuard::remove("CODEWHALE_PROVIDER");
    let _legacy_provider_env = EnvVarGuard::remove("DEEPSEEK_PROVIDER");
    let _api_key_envs: Vec<_> = [
        "DEEPSEEK_API_KEY",
        "NVIDIA_API_KEY",
        "NVIDIA_NIM_API_KEY",
        "OPENAI_API_KEY",
        "ATLASCLOUD_API_KEY",
        "WANJIE_ARK_API_KEY",
        "WANJIE_API_KEY",
        "WANJIE_MAAS_API_KEY",
        "OPENROUTER_API_KEY",
        "NOVITA_API_KEY",
        "FIREWORKS_API_KEY",
        "SILICONFLOW_API_KEY",
        "MOONSHOT_API_KEY",
        "KIMI_API_KEY",
        "SGLANG_API_KEY",
        "VLLM_API_KEY",
        "OLLAMA_API_KEY",
    ]
    .into_iter()
    .map(EnvVarGuard::remove)
    .collect();

    let app = App::new(test_options(false), &Config::default());
    // CW-7167: a keyless launch opens the composer; the provider is asked for
    // on the first send.
    assert_eq!(app.onboarding, OnboardingState::None);
    assert!(app.onboarding_needs_api_key);
    assert!(app.onboarding_missing_key_recovery);
    // A new user has no saved route, so the picker opens on the provider
    // list, not on the built-in default's missing key.
    assert!(!app.onboarding_recovers_configured_route());
    // Language is asked in /setup, so the launch screen is not "2/3".
    assert!(!app.onboarding_had_language_step);
}

#[test]
fn new_caches_workspace_skills_for_slash_menu() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    let skill_dir = workspace.join(".agents").join("skills").join("local-skill");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    crate::test_support::trust_workspace(&workspace);
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: local-skill\ndescription: Local workspace skill\n---\nUse the local skill.\n",
    )
    .expect("skill file");

    let mut options = test_options(false);
    options.workspace = workspace.clone();
    options.skills_dir = tmp.path().join("global-skills");
    let app = App::new(options, &Config::default());

    assert_eq!(app.skills_dir, workspace.join(".agents").join("skills"));
    assert!(app.cached_skills.iter().any(|(name, description)| {
        name == "local-skill" && description == "Local workspace skill"
    }));
}

#[test]
fn cached_skills_merges_across_candidate_directories() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    crate::test_support::trust_workspace(&workspace);

    // Higher-precedence directory contains a stale empty dir for `foo`
    // (no SKILL.md). This used to shadow the real definition further
    // down the candidate list when the cache only scanned a single dir.
    std::fs::create_dir_all(workspace.join(".agents").join("skills").join("foo"))
        .expect("stale empty dir");

    // Lower-precedence directory has the real skill.
    let real_dir = workspace.join(".claude").join("skills").join("foo");
    std::fs::create_dir_all(&real_dir).expect("real skill dir");
    std::fs::write(
        real_dir.join("SKILL.md"),
        "---\nname: foo\ndescription: Real foo skill\n---\nbody\n",
    )
    .expect("skill file");

    let mut options = test_options(false);
    options.workspace = workspace.clone();
    options.skills_dir = tmp.path().join("global-skills");
    let app = App::new(options, &Config::default());

    assert!(
        app.cached_skills
            .iter()
            .any(|(name, description)| name == "foo" && description == "Real foo skill"),
        "cached_skills should fall through to lower-precedence dir when higher-precedence one has an empty stub: {:?}",
        app.cached_skills,
    );
}

#[test]
fn cached_skills_respect_codewhale_only_scan_config() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    crate::test_support::trust_workspace(&workspace);

    let claude_dir = workspace
        .join(".claude")
        .join("skills")
        .join("claude-skill");
    std::fs::create_dir_all(&claude_dir).expect("claude skill dir");
    std::fs::write(
        claude_dir.join("SKILL.md"),
        "---\nname: claude-skill\ndescription: Claude skill\n---\nbody\n",
    )
    .expect("write claude skill");

    let codewhale_dir = workspace
        .join(".codewhale")
        .join("skills")
        .join("codewhale-skill");
    std::fs::create_dir_all(&codewhale_dir).expect("codewhale skill dir");
    std::fs::write(
        codewhale_dir.join("SKILL.md"),
        "---\nname: codewhale-skill\ndescription: CodeWhale skill\n---\nbody\n",
    )
    .expect("write codewhale skill");

    let mut options = test_options(false);
    options.workspace = workspace.clone();
    options.skills_dir = tmp.path().join("global-skills");
    let app = App::new(options, &{
        let __cw_config_init_9_0 = Some(codewhale_runtime::config::SkillsConfig {
            scan_codewhale_only: Some(true),
            ..Default::default()
        });
        let mut __cw_config_init_9_value: Config = Default::default();
        __cw_config_init_9_value.skills = __cw_config_init_9_0;
        __cw_config_init_9_value
    });

    assert_eq!(app.skills_dir, workspace.join(".codewhale").join("skills"));
    assert!(
        app.cached_skills
            .iter()
            .any(|(name, _)| name == "codewhale-skill"),
        "CodeWhale skill should be cached: {:?}",
        app.cached_skills
    );
    assert!(
        !app.cached_skills
            .iter()
            .any(|(name, _)| name == "claude-skill"),
        "strict scan should not cache Claude skills: {:?}",
        app.cached_skills
    );
}

#[test]
fn resolve_skills_dir_requires_codewhale_skills_to_be_directory() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(workspace.join(".codewhale")).expect("codewhale dir");
    std::fs::write(
        workspace.join(".codewhale").join("skills"),
        "not a directory",
    )
    .expect("skills file");

    let global_skills_dir = tmp.path().join("global-skills");
    let config = {
        let __cw_config_init_10_0 = Some(codewhale_runtime::config::SkillsConfig {
            scan_codewhale_only: Some(true),
            ..Default::default()
        });
        let mut __cw_config_init_10_value: Config = Default::default();
        __cw_config_init_10_value.skills = __cw_config_init_10_0;
        __cw_config_init_10_value
    };

    let resolved = resolve_skills_dir(&workspace, &global_skills_dir, &config);

    assert_eq!(resolved, global_skills_dir);
}

#[test]
fn resolve_skills_dir_ignores_untrusted_workspace_skills() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let global_skills_dir = tmp.path().join("global-skills");
    for relative in [".agents/skills", "skills"] {
        let workspace = tmp
            .path()
            .join(format!("ws-{}", relative.replace('/', "-")));
        let local_skills = workspace.join(relative);
        std::fs::create_dir_all(&local_skills).expect("skills dir");
        let config = {
            let __cw_config_init_11_0 = Some(global_skills_dir.to_string_lossy().into_owned());
            let __cw_config_init_11_1 = Some(codewhale_runtime::config::SkillsConfig {
                flat_workspace_root: Some(true),
                ..Default::default()
            });
            let mut __cw_config_init_11_value: Config = Default::default();
            __cw_config_init_11_value.skills_dir = __cw_config_init_11_0;
            __cw_config_init_11_value.skills = __cw_config_init_11_1;
            __cw_config_init_11_value
        };

        assert_eq!(
            resolve_skills_dir(&workspace, &global_skills_dir, &config),
            global_skills_dir,
            "untrusted {relative} must not be resolved as the skills dir"
        );
        crate::test_support::trust_workspace(&workspace);
        assert_eq!(
            resolve_skills_dir(&workspace, &global_skills_dir, &config),
            local_skills,
            "trusted {relative} resolves as before"
        );
    }
}

#[test]
fn cached_skills_include_configured_directory() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let workspace = tmp.path().join("workspace");

    let configured_dir = tmp.path().join("configured-skills");
    let configured_skill_dir = configured_dir.join("configured-skill");
    std::fs::create_dir_all(&configured_skill_dir).expect("configured skill dir");
    std::fs::write(
        configured_skill_dir.join("SKILL.md"),
        "---\nname: configured-skill\ndescription: Configured skill\n---\nbody\n",
    )
    .expect("write configured skill");

    let mut options = test_options(false);
    options.workspace = workspace.clone();
    options.skills_dir = configured_dir.clone();
    let config = {
        let __cw_config_init_12_0 = Some(configured_dir.to_string_lossy().into_owned());
        let mut __cw_config_init_12_value: Config = Default::default();
        __cw_config_init_12_value.skills_dir = __cw_config_init_12_0;
        __cw_config_init_12_value
    };
    let app = App::new(options, &config);

    assert!(
        app.cached_skills
            .iter()
            .any(|(name, description)| name == "configured-skill"
                && description == "Configured skill"),
        "configured skill dir should be merged: {:?}",
        app.cached_skills
    );
}

#[test]
fn cached_skills_preserve_configured_directory_in_codewhale_only_scan() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    crate::test_support::trust_workspace(&workspace);

    let codewhale_skill_dir = workspace
        .join(".codewhale")
        .join("skills")
        .join("workspace-codewhale");
    std::fs::create_dir_all(&codewhale_skill_dir).expect("workspace codewhale skill dir");
    std::fs::write(
        codewhale_skill_dir.join("SKILL.md"),
        "---\nname: workspace-codewhale\ndescription: Workspace CodeWhale skill\n---\nbody\n",
    )
    .expect("write workspace codewhale skill");

    let configured_dir = tmp.path().join("configured-skills");
    let configured_skill_dir = configured_dir.join("configured-skill");
    std::fs::create_dir_all(&configured_skill_dir).expect("configured skill dir");
    std::fs::write(
        configured_skill_dir.join("SKILL.md"),
        "---\nname: configured-skill\ndescription: Configured skill\n---\nbody\n",
    )
    .expect("write configured skill");

    let mut options = test_options(false);
    options.workspace = workspace.clone();
    options.skills_dir = configured_dir.clone();
    let config = {
        let __cw_config_init_13_0 = Some(configured_dir.to_string_lossy().into_owned());
        let __cw_config_init_13_1 = Some(codewhale_runtime::config::SkillsConfig {
            scan_codewhale_only: Some(true),
            ..Default::default()
        });
        let mut __cw_config_init_13_value: Config = Default::default();
        __cw_config_init_13_value.skills_dir = __cw_config_init_13_0;
        __cw_config_init_13_value.skills = __cw_config_init_13_1;
        __cw_config_init_13_value
    };
    let app = App::new(options, &config);

    assert_eq!(app.skills_dir, configured_dir);
    assert!(
        app.cached_skills
            .iter()
            .any(|(name, _)| name == "workspace-codewhale"),
        "workspace CodeWhale skill should still be cached: {:?}",
        app.cached_skills
    );
    assert!(
        app.cached_skills
            .iter()
            .any(|(name, _)| name == "configured-skill"),
        "explicit configured skills_dir should still be cached: {:?}",
        app.cached_skills
    );
}

#[test]
fn cached_skills_reject_codewhale_only_workspace_symlink_escape() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    let escape_target = tmp.path().join("escape-target");
    let escaped_skill_dir = escape_target.join("escaped-skill");
    std::fs::create_dir_all(workspace.join(".codewhale")).expect("codewhale dir");
    std::fs::create_dir_all(&escaped_skill_dir).expect("escaped skill dir");
    std::fs::write(
        escaped_skill_dir.join("SKILL.md"),
        "---\nname: escaped-skill\ndescription: Escaped skill\n---\nbody\n",
    )
    .expect("write escaped skill");

    let link_path = workspace.join(".codewhale").join("skills");
    if create_dir_symlink(&escape_target, &link_path).is_err() {
        return;
    }

    let global_skills_dir = tmp.path().join("global-skills");
    let mut options = test_options(false);
    options.workspace = workspace.clone();
    options.skills_dir = global_skills_dir.clone();
    let config = {
        let __cw_config_init_14_0 = Some(codewhale_runtime::config::SkillsConfig {
            scan_codewhale_only: Some(true),
            ..Default::default()
        });
        let mut __cw_config_init_14_value: Config = Default::default();
        __cw_config_init_14_value.skills = __cw_config_init_14_0;
        __cw_config_init_14_value
    };
    let app = App::new(options, &config);

    assert_eq!(app.skills_dir, global_skills_dir);
    assert!(
        !app.cached_skills
            .iter()
            .any(|(name, _)| name == "escaped-skill"),
        "strict app cache must not follow escaped workspace CodeWhale symlinks: {:?}",
        app.cached_skills
    );
}

#[test]
fn paste_defers_oversized_text_consolidation_until_submit() {
    // (#3263): a large paste stays inline so the user can still edit it.
    // At submit time, the inline text is replaced by the @mention so the
    // model reads the full content from the paste file instead of receiving
    // it twice.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut opts = test_options(false);
    opts.workspace = tmp.path().to_path_buf();
    let mut app = App::new(opts, &Config::default());
    let full_content = "y".repeat(MAX_SUBMITTED_INPUT_CHARS + 256);

    app.insert_paste_text(&full_content);

    assert_eq!(app.input, full_content);
    assert_eq!(app.cursor_position, app.input.chars().count());
    let pastes_dir = tmp.path().join(".codewhale/pastes");
    assert!(
        !pastes_dir.exists() || std::fs::read_dir(&pastes_dir).unwrap().next().is_none(),
        "paste file should not be written before submit"
    );
    assert!(
        app.status_toasts
            .iter()
            .all(|toast| !toast.text.contains("backed up")),
        "backup toast should not appear before submit"
    );

    let submitted = app.submit_input().expect("expected submitted input");
    // The submission is an attachment card, never a bare path: a size
    // header, the @-mention that attaches the file for the model, and a
    // bounded preview of the pasted content.
    assert!(
        submitted.starts_with("[Pasted content attached · "),
        "submission must open with the attachment header, got: {}",
        &submitted[..submitted.len().min(80)]
    );
    assert!(
        submitted.contains("\n@.codewhale/pastes/paste-"),
        "the @-mention must survive verbatim for file-mention resolution"
    );
    assert!(
        submitted.contains("--- preview ---\nyyy"),
        "a bounded preview of the pasted content must be visible"
    );
    let mention_line = submitted
        .lines()
        .find(|line| line.starts_with("@.codewhale/pastes/"))
        .expect("mention line");
    let mention = &mention_line[1..]; // strip leading '@'
    assert!(mention.ends_with(".md"), "expected .md extension");
    let abs = tmp.path().join(mention);
    assert!(abs.is_file(), "paste file must exist at {abs:?}");
    let written = std::fs::read_to_string(&abs).expect("read");
    assert_eq!(written, full_content);
    assert!(
        app.status_toasts
            .iter()
            .any(|toast| toast.text.contains("backed up")),
        "expected backup toast after submit"
    );
}

#[test]
fn oversized_paste_submission_never_renders_as_a_bare_path() {
    // The reported incident: a large paste became a transcript row that
    // showed only `@.codewhale/pastes/paste-….md` — a mysterious path where
    // the user's message should be. The submission must carry a visible
    // size header and content preview around the mention so the user can
    // always see what they sent.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut opts = test_options(false);
    opts.workspace = tmp.path().to_path_buf();
    let mut app = App::new(opts, &Config::default());
    let full_content = format!(
        "IMPORTANT INSTRUCTIONS\n{}",
        "x".repeat(MAX_SUBMITTED_INPUT_CHARS + 10)
    );

    app.insert_paste_text(&full_content);
    let submitted = app.submit_input().expect("expected submitted input");

    assert_ne!(submitted, submitted.lines().nth(1).expect("mention line"));
    assert!(
        submitted.contains("IMPORTANT INSTRUCTIONS"),
        "the preview must surface the pasted content's first line"
    );
    assert!(
        submitted.contains(&format!("· {} chars]", full_content.chars().count())),
        "the header must state the full pasted size"
    );
    // The full oversized content must NOT be inlined — the file is the
    // single source of truth for the model.
    assert!(
        !submitted.contains(&"x".repeat(MAX_SUBMITTED_INPUT_CHARS)),
        "the inline copy must stay bounded; the @-mention attaches the file"
    );
}

#[test]
fn paste_under_threshold_does_not_consolidate() {
    // Negative path: a small paste must NOT spawn a paste file. The
    // input stays inline so the user can edit it freely.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut opts = test_options(false);
    opts.workspace = tmp.path().to_path_buf();
    let mut app = App::new(opts, &Config::default());
    let small = "hello world\nthis is fine".to_string();

    app.insert_paste_text(&small);

    assert_eq!(app.input, small);
    assert!(!app.input.starts_with("@.codewhale/pastes/"));
    // No paste file gets written for under-cap pastes.
    let pastes_dir = tmp.path().join(".codewhale/pastes");
    assert!(
        !pastes_dir.exists() || std::fs::read_dir(&pastes_dir).unwrap().next().is_none(),
        "no paste file should be written for under-cap content"
    );
}

#[test]
fn large_multiline_paste_preserves_exact_bytes_through_submit() {
    // #4719: large multi-line pastes must not byte-corrupt before submission.
    // Real dogfood saw paths like `codewhale-v091-exact-88a158-ci` arrive as
    // `work-88a158-ci` — assert exact fidelity for a representative payload.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut opts = test_options(false);
    opts.workspace = tmp.path().to_path_buf();
    let mut app = App::new(opts, &Config::default());

    let payload = format!(
        "Mission path: /Volumes/VIXinSSD/CW/worktrees/codewhale-v091-exact-88a158-ci\n\
         SHA: 0dfe9170a10e081fe48b23239f22d33260f4fa24\n\
         Branch: codex/v091-local-candidate-20260722\n\
         Paths that must not truncate: codewhale-v091-exact-88a158-ci worktrees/codewhale-v091-exact-88a158-ci\n\
         Mixed punctuation: a;b:c[m]<n> digits 0123456789 and hyphens-ok\n\
         Unicode: 你好世界 café — keep every codepoint.\n\
         {}",
        "line-body-".repeat(200)
    );
    // Stay under MAX_SUBMITTED_INPUT_CHARS so submit returns the inline text
    // (no @paste consolidation) and we can compare exact bytes.
    assert!(
        payload.chars().count() < MAX_SUBMITTED_INPUT_CHARS,
        "fixture must stay under submit consolidation threshold"
    );

    app.insert_paste_text(&payload);
    assert_eq!(
        app.input, payload,
        "composer input must equal pasted payload exactly"
    );

    let submitted = app.submit_input().expect("submit");
    assert_eq!(
        submitted, payload,
        "submitted bytes must equal pasted payload exactly"
    );
}

#[test]
fn submit_input_consolidates_oversized_input_into_paste_file() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut opts = test_options(false);
    opts.workspace = tmp.path().to_path_buf();
    let mut app = App::new(opts, &Config::default());
    let full_content = "x".repeat(MAX_SUBMITTED_INPUT_CHARS + 128);
    app.input = full_content.clone();
    app.cursor_position = app.input.chars().count();

    let submitted = app.submit_input().expect("expected submitted input");

    // The submitted text is an attachment card: size header, the @-mention
    // that attaches the file for the model, and a bounded preview (#3263
    // follow-up: never a bare path).
    assert!(
        submitted.starts_with("[Pasted content attached · "),
        "submission must open with the attachment header, got: {}",
        &submitted[..submitted.len().min(80)]
    );
    let mention_line = submitted
        .lines()
        .find(|line| line.starts_with("@.codewhale/pastes/paste-"))
        .expect("mention line");
    assert!(
        mention_line.ends_with(".md"),
        "expected .md extension, got: {mention_line}"
    );

    // The paste file must exist on disk with the full original content.
    let mention = &mention_line[1..]; // strip leading '@'
    let abs_path = tmp.path().join(mention);
    assert!(abs_path.is_file(), "paste file must exist at {abs_path:?}");
    let written = std::fs::read_to_string(&abs_path).expect("read paste file");
    assert_eq!(written, full_content);

    // A status toast should have been pushed.
    assert!(
        app.status_toasts
            .iter()
            .any(|toast| toast.text.contains("backed up")),
        "expected backup toast, got: {:?}",
        app.status_toasts
            .iter()
            .map(|t| &t.text)
            .collect::<Vec<_>>()
    );

    // The composer must be clear after submit.
    assert!(app.input.is_empty());
}

#[test]
fn submit_input_holds_oversized_input_when_paste_file_cannot_be_written() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // `.codewhale` is a file, so `.codewhale/pastes` cannot be created.
    std::fs::write(tmp.path().join(".codewhale"), "not a dir").expect("seed file");
    let mut opts = test_options(false);
    opts.workspace = tmp.path().to_path_buf();
    let mut app = App::new(opts, &Config::default());
    let full_content = "y".repeat(MAX_SUBMITTED_INPUT_CHARS + 128);
    app.input = full_content.clone();
    app.cursor_position = app.input.chars().count();

    assert_eq!(
        app.submit_input(),
        None,
        "a truncated prompt must not be sent"
    );
    assert_eq!(
        app.input, full_content,
        "the full text stays in the composer"
    );
    // The toast is short enough to survive the footer's clause-shedding...
    assert!(
        app.status_toasts
            .iter()
            .any(|toast| toast.text.starts_with("Not sent")
                && toast.text.contains("paste file not saved")),
        "expected a short not-sent toast"
    );
    // ...and the actionable reason, with the write error, is in the transcript.
    assert!(
        app.history.iter().any(|cell| matches!(
            cell,
            HistoryCell::System { content }
                if content.starts_with("Not sent")
                    && content.contains("shorten it")
                    && !content.contains("{error}")
        )),
        "expected the full reason in the transcript"
    );
}

#[test]
fn app_starts_without_seeded_transcript_messages() {
    let app = App::new(test_options(false), &Config::default());
    assert!(app.history.is_empty());
    assert_eq!(app.history_version, 0);
}

#[test]
fn clear_todos_resets_todos_list() {
    let mut app = App::new(test_options(false), &Config::default());

    // Seed some todos.
    {
        let mut todos = app.todos.try_lock().expect("todos lock");
        todos.add("buy milk".to_string(), TodoStatus::Pending);
        todos.add("write code".to_string(), TodoStatus::InProgress);
        assert_eq!(todos.snapshot().items.len(), 2);
    }

    assert!(app.clear_todos());

    let todos = app.todos.try_lock().expect("todos lock");
    assert!(todos.snapshot().items.is_empty());
}

#[test]
fn clear_todos_resets_plan_state() {
    let mut app = App::new(test_options(false), &Config::default());

    {
        let mut plan = app
            .plan_state
            .try_lock()
            .expect("plan lock should be available");
        plan.update(UpdatePlanArgs {
            explanation: Some("test plan".to_string()),
            plan: vec![PlanItemArg {
                step: "step 1".to_string(),
                status: StepStatus::InProgress,
            }],
            ..UpdatePlanArgs::default()
        });
        assert!(!plan.snapshot().is_empty());
    }

    assert!(app.clear_todos());

    let plan = app
        .plan_state
        .try_lock()
        .expect("plan lock should be available");
    assert!(plan.snapshot().is_empty());
}

#[test]
fn work_state_snapshot_round_trips_todos_and_plan() {
    let app = App::new(test_options(false), &Config::default());
    {
        let mut todos = app.todos.try_lock().expect("todos lock");
        todos.add("inspect".to_string(), TodoStatus::Completed);
        todos.add("patch".to_string(), TodoStatus::InProgress);
    }
    {
        let mut plan = app.plan_state.try_lock().expect("plan lock");
        plan.update(UpdatePlanArgs {
            objective: Some("Keep Work durable".to_string()),
            plan: vec![PlanItemArg {
                step: "verify".to_string(),
                status: StepStatus::InProgress,
            }],
            ..UpdatePlanArgs::default()
        });
    }
    let state = app
        .work_state_snapshot()
        .expect("snapshot locks")
        .expect("non-empty state");

    let mut restored = App::new(test_options(false), &Config::default());
    let restored_workspace = restored.workspace.clone();
    restored
        .restore_work_state("restored-session", &restored_workspace, Some(&state))
        .expect("restore Work state");
    assert_eq!(
        restored.work_state_snapshot().expect("snapshot"),
        Some(state)
    );
}

#[test]
fn work_restore_reconciles_fleet_from_the_restored_workspace() {
    let restored_workspace = tempfile::tempdir().expect("restored workspace");
    let ledger = codewhale_engine::fleet::ledger::FleetLedger::open(restored_workspace.path())
        .expect("open restored Fleet ledger");
    ledger
        .enqueue(codewhale_protocol::fleet::FleetInboxEntry {
            run_id: codewhale_protocol::fleet::FleetRunId::from("run-restore"),
            task_id: "task-restore".to_string(),
            priority: 0,
            enqueued_at: "2026-07-18T00:00:00Z".to_string(),
            lease_deadline: None,
            attempts: 0,
        })
        .expect("enqueue restored Fleet task");

    let source = codewhale_engine::work_graph::new_shared_work_runtime(
        codewhale_engine::tools::todo::new_shared_todo_list(),
        codewhale_engine::tools::plan::new_shared_plan_state(),
    );
    source
        .register_operation(
            "restored-session",
            codewhale_engine::work_graph::OperationIntent::new(
                "fleet:run-restore/task-restore",
                "restored Fleet task",
                true,
                "fleet",
                "restore-test",
            ),
        )
        .expect("register Fleet binding");
    let captured = source
        .capture(Some("restored-session"))
        .expect("capture source Work state")
        .expect("non-empty source Work state");
    let state = codewhale_engine::session_manager::SessionWorkState {
        graph: Some(captured.graph),
        todos: captured.todos,
        plan: captured.plan,
    };

    let mut app = App::new(test_options(false), &Config::default());
    assert_ne!(app.workspace, restored_workspace.path());
    app.restore_work_state("restored-session", restored_workspace.path(), Some(&state))
        .expect("restore Work state from target workspace");
    let graph = app
        .runtime_services
        .work
        .as_ref()
        .expect("Work runtime")
        .capture(Some("restored-session"))
        .expect("capture restored Work state")
        .expect("restored graph")
        .graph;
    let operation = graph
        .nodes
        .iter()
        .find(|node| {
            node.binding
                .as_ref()
                .is_some_and(|binding| binding.external == "fleet:run-restore/task-restore")
        })
        .expect("restored Fleet operation");
    assert_eq!(
        operation.state,
        codewhale_engine::work_graph::NodeState::Initializing,
        "the target workspace ledger must outrank the app's previous workspace"
    );
}

#[test]
fn failed_workspace_owner_reconcile_leaves_previous_work_state_intact() {
    let restored_workspace = tempfile::tempdir().expect("restored workspace");
    let ledger = codewhale_engine::fleet::ledger::FleetLedger::open(restored_workspace.path())
        .expect("open restored Fleet ledger");
    ledger
        .enqueue(codewhale_protocol::fleet::FleetInboxEntry {
            run_id: codewhale_protocol::fleet::FleetRunId::from("run-regress"),
            task_id: "task-regress".to_string(),
            priority: 0,
            enqueued_at: "2026-07-18T00:00:00Z".to_string(),
            lease_deadline: None,
            attempts: 0,
        })
        .expect("enqueue older Fleet owner state");

    let incoming = codewhale_engine::work_graph::new_shared_work_runtime(
        codewhale_engine::tools::todo::new_shared_todo_list(),
        codewhale_engine::tools::plan::new_shared_plan_state(),
    );
    incoming
        .register_operation(
            "incoming-session",
            codewhale_engine::work_graph::OperationIntent::new(
                "fleet:run-regress/task-regress",
                "newer saved Fleet task",
                true,
                "fleet",
                "regression-test",
            ),
        )
        .expect("register incoming Fleet binding");
    incoming
        .reconcile_operation(
            "incoming-session",
            codewhale_engine::work_graph::OperationOwnerSnapshot::new(
                "fleet:run-regress/task-regress",
                codewhale_engine::work_graph::OwnerState::Running,
                2,
                2,
            ),
        )
        .expect("record newer saved owner sequence");
    let incoming = incoming
        .capture(Some("incoming-session"))
        .expect("capture incoming state")
        .expect("incoming graph");
    let incoming = codewhale_engine::session_manager::SessionWorkState {
        graph: Some(incoming.graph),
        todos: incoming.todos,
        plan: incoming.plan,
    };

    let mut app = App::new(test_options(false), &Config::default());
    let work = app
        .runtime_services
        .work
        .as_ref()
        .expect("Work runtime")
        .clone();
    work.register_operation(
        "previous-session",
        codewhale_engine::work_graph::OperationIntent::new(
            "shell:shell_previous",
            "previous operation",
            false,
            "exec_shell",
            "previous-test",
        ),
    )
    .expect("register previous state");
    let before = work
        .capture(Some("previous-session"))
        .expect("capture previous state")
        .expect("previous graph");

    let error = app
        .restore_work_state(
            "incoming-session",
            restored_workspace.path(),
            Some(&incoming),
        )
        .expect_err("owner sequence regression must fail closed");
    assert!(error.contains("sequence regressed"), "{error}");
    assert_eq!(
        work.capture(Some("previous-session"))
            .expect("capture state after failed restore")
            .expect("previous graph remains"),
        before,
        "failed restore must not replace any part of the previous Work state"
    );
}

#[test]
fn entering_operate_preserves_user_rail_panel() {
    let mut app = App::new(test_options(false), &Config::default());
    app.work_surface.panel = crate::tui::work_surface::RailPanel::Agents;

    assert!(app.set_mode(AppMode::Operate));
    assert_eq!(
        app.work_surface.panel,
        crate::tui::work_surface::RailPanel::Agents
    );
}

#[test]
fn test_cycle_scenario() {
    // Scenario consolidation of: test_cycle_mode_transitions, test_cycle_mode_reverse_transitions
    // from test_cycle_mode_transitions
    {
        let mut app = App::new(test_options(false), &Config::default());
        let initial_mode = app.mode;
        app.cycle_mode();
        // Mode should have changed
        assert_ne!(app.mode, initial_mode);
    }
    // from test_cycle_mode_reverse_transitions
    {
        let mut app = App::new(test_options(false), &Config::default());

        app.mode = AppMode::Plan;
        app.cycle_mode_reverse();
        assert_eq!(app.mode, AppMode::Operate);

        app.mode = AppMode::Operate;
        app.cycle_mode_reverse();
        assert_eq!(app.mode, AppMode::Agent);

        app.mode = AppMode::Agent;
        app.cycle_mode_reverse();
        assert_eq!(app.mode, AppMode::Plan);
    }
}

#[test]
fn effective_route_display_tracks_inflight_and_last_auto_provider() {
    let mut app = App::new(test_options(false), &Config::default());
    app.auto_model = true;
    app.pending_turn_route = Some((ProviderKind::Zai, "glm-5.2".to_string(), true));
    assert_eq!(
        app.effective_route_display(),
        (ProviderKind::Zai, "glm-5.2".to_string())
    );

    app.pending_turn_route = None;
    app.last_effective_provider = Some(ProviderKind::Xai);
    app.last_effective_model = Some("grok-4.5".to_string());
    assert_eq!(
        app.effective_route_display(),
        (ProviderKind::Xai, "grok-4.5".to_string())
    );
}

#[test]
fn test_mode_scenario() {
    // Scenario consolidation of: test_mode_switch_does_not_emit_redundant_toast, test_mode_switch_toasts_do_not_disrupt_non_mode_toasts
    // from test_mode_switch_does_not_emit_redundant_toast
    {
        let mut app = App::new(test_options(false), &Config::default());
        let first_mode = app.mode.next();
        let second_mode = first_mode.next();

        app.set_mode(first_mode);
        app.sync_status_message_to_toasts();
        assert!(app.status_toasts.is_empty());

        app.set_mode(second_mode);
        app.sync_status_message_to_toasts();
        assert!(app.status_toasts.is_empty());
    }
    // from test_mode_switch_toasts_do_not_disrupt_non_mode_toasts
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.yolo_compat_notified = true;
        app.status_message = Some("Task queued".to_string());
        app.sync_status_message_to_toasts();

        app.set_mode(AppMode::Agent);
        app.sync_status_message_to_toasts();
        app.set_mode_yolo_compat();
        app.sync_status_message_to_toasts();

        assert_eq!(app.status_toasts.len(), 1);
        assert!(
            app.status_toasts
                .iter()
                .any(|toast| toast.text == "Task queued")
        );
    }
}

#[test]
fn test_clear_input() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "test input".to_string();
    app.cursor_position = app.input.len();
    app.clear_input();
    assert!(app.input.is_empty());
    assert_eq!(app.cursor_position, 0);
}

#[test]
fn test_queue_message() {
    let mut app = App::new(test_options(false), &Config::default());
    app.queue_message(QueuedMessage::new("test message".to_string(), None));
    assert_eq!(app.queued_message_count(), 1);
    assert!(app.queued_messages.front().is_some());
}

#[test]
fn test_remove_scenario() {
    // Scenario consolidation of: test_remove_queued_message, test_remove_queued_message_invalid_index
    // from test_remove_queued_message
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.queue_message(QueuedMessage::new("first".to_string(), None));
        app.queue_message(QueuedMessage::new("second".to_string(), None));

        // Remove first (index 0)
        let removed = app.remove_queued_message(0);
        assert!(removed.is_some());
        assert_eq!(app.queued_message_count(), 1);

        // Remove second (now at index 0)
        let removed = app.remove_queued_message(0);
        assert!(removed.is_some());
        assert_eq!(app.queued_message_count(), 0);
    }
    // from test_remove_queued_message_invalid_index
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.queue_message(QueuedMessage::new("test".to_string(), None));

        // Try to remove non-existent index
        let removed = app.remove_queued_message(100);
        assert!(removed.is_none());
    }
}

#[test]
fn test_set_mode_updates_state() {
    let mut app = App::new(test_options(false), &Config::default());
    app.yolo_compat_notified = true;
    app.set_mode(AppMode::Plan);
    // The deprecated YOLO alias lands in Act (M6 back-compat shim).
    app.set_mode_yolo_compat();
    assert_eq!(app.mode, AppMode::Agent);
    assert!(app.yolo);
    // YOLO compat shim should enable trust, shell, and bypass approvals.
    assert!(app.trust_mode);
    assert!(app.allow_shell);
    assert_eq!(app.approval_mode, ApprovalMode::Bypass);
}

#[test]
fn set_mode_scenario() {
    // Scenario consolidation of: set_mode_yolo_restores_previous_policies_on_exit, set_mode_plan_restores_previous_approval_on_agent_exit, set_mode_plan_to_yolo_keeps_yolo_permissions_and_restores_agent_baseline
    // from set_mode_yolo_restores_previous_policies_on_exit
    {
        let mut options = test_options(false);
        options.allow_shell = false;
        options.start_in_agent_mode = true; // avoid coupling to settings.default_mode
        let mut app = App::new(options, &Config::default());
        app.allow_shell = false;
        app.trust_mode = false;
        app.approval_mode = ApprovalMode::Never;
        app.yolo_compat_notified = true;

        app.set_mode_yolo_compat();
        assert!(app.allow_shell);
        assert!(app.trust_mode);
        assert_eq!(app.approval_mode, ApprovalMode::Bypass);

        app.set_mode(AppMode::Agent);
        assert!(!app.allow_shell);
        assert!(!app.trust_mode);
        assert_eq!(app.approval_mode, ApprovalMode::Never);
    }
    // from set_mode_plan_restores_previous_approval_on_agent_exit
    {
        let config = {
            let __cw_config_init_15_0 = Some("never".to_string());
            let mut __cw_config_init_15_value: Config = Default::default();
            __cw_config_init_15_value.approval_policy = __cw_config_init_15_0;
            __cw_config_init_15_value
        };
        let mut options = test_options(false);
        options.start_in_agent_mode = true; // avoid coupling to settings.default_mode
        let mut app = App::new(options, &config);
        assert_eq!(app.mode, AppMode::Agent);
        assert_eq!(app.approval_mode, ApprovalMode::Never);

        app.set_mode(AppMode::Plan);
        app.approval_mode = ApprovalMode::Suggest;

        app.set_mode(AppMode::Agent);
        assert_eq!(app.mode, AppMode::Agent);
        assert_eq!(app.approval_mode, ApprovalMode::Never);
    }
    // from set_mode_plan_to_yolo_keeps_yolo_permissions_and_restores_agent_baseline
    {
        let mut options = test_options(false);
        options.allow_shell = false;
        options.start_in_agent_mode = true; // avoid coupling to settings.default_mode
        let mut app = App::new(options, &Config::default());
        app.allow_shell = false;
        app.trust_mode = false;
        app.approval_mode = ApprovalMode::Never;
        app.yolo_compat_notified = true;

        app.set_mode(AppMode::Plan);
        app.approval_mode = ApprovalMode::Suggest;

        app.set_mode_yolo_compat();
        assert_eq!(app.mode, AppMode::Agent);
        assert!(app.allow_shell);
        assert!(app.trust_mode);
        assert_eq!(app.approval_mode, ApprovalMode::Bypass);

        app.set_mode(AppMode::Agent);
        assert_eq!(app.mode, AppMode::Agent);
        assert!(!app.allow_shell);
        assert!(!app.trust_mode);
        assert_eq!(app.approval_mode, ApprovalMode::Never);
    }
}

#[test]
fn base_policy_for_mode_projects_the_mode_permission_table() {
    // Pure projection of (mode, prefs) — the single source of truth for #3386.
    let prefs = ModeSessionPrefs {
        agent_allow_shell: true,
        agent_trust_mode: true,
        agent_approval_mode: ApprovalMode::Never,
    };

    // Plan: read-only, no shell, no trust, Suggest — and it never inherits the
    // (here elevated) Agent baseline.
    let plan = base_policy_for_mode(AppMode::Plan, &prefs);
    assert_eq!(plan.mode, AppMode::Plan);
    assert!(!plan.allow_shell);
    assert!(!plan.trust_mode);
    assert_eq!(plan.approval_mode, ApprovalMode::Suggest);

    // Agent: exactly the durable baseline.
    let agent = base_policy_for_mode(AppMode::Agent, &prefs);
    assert_eq!(agent.mode, AppMode::Agent);
    assert!(agent.allow_shell);
    assert!(agent.trust_mode);
    assert_eq!(agent.approval_mode, ApprovalMode::Never);

    // Operate uses the Agent baseline.
    let operate = base_policy_for_mode(AppMode::Operate, &prefs);
    assert_eq!(operate.mode, AppMode::Operate);
    assert_eq!(operate.allow_shell, agent.allow_shell);
    assert_eq!(operate.trust_mode, agent.trust_mode);
    assert_eq!(operate.approval_mode, ApprovalMode::Never);

    // Full Access is represented by the Bypass posture, not a mode row or a
    // separate auto-approve field (#3736).

    // A minimal Agent baseline projects through Agent unchanged.
    let minimal = ModeSessionPrefs {
        agent_allow_shell: false,
        agent_trust_mode: false,
        agent_approval_mode: ApprovalMode::Suggest,
    };
    let agent_min = base_policy_for_mode(AppMode::Agent, &minimal);
    assert!(!agent_min.allow_shell);
    assert!(!agent_min.trust_mode);
    assert_eq!(agent_min.approval_mode, ApprovalMode::Suggest);
    let operate_min = base_policy_for_mode(AppMode::Operate, &minimal);
    assert!(!operate_min.allow_shell);
    assert!(!operate_min.trust_mode);
    assert_eq!(operate_min.approval_mode, ApprovalMode::Suggest);
}

#[test]
fn cycle_approval_scenario() {
    // Scenario consolidation of: cycle_approval_posture_cycles_suggest_auto_bypass, cycle_approval_posture_emits_rebinding_notice_once
    // from cycle_approval_posture_cycles_suggest_auto_bypass
    {
        let _env_lock = lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        let _config_env = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
        let mut options = test_options(false);
        options.start_in_agent_mode = true;
        options.config_path = Some(config_path);
        let mut app = App::new(options, &Config::default());
        app.approval_mode = ApprovalMode::Suggest;

        assert!(app.cycle_approval_posture());
        assert_eq!(app.approval_mode, ApprovalMode::Auto);

        assert!(app.cycle_approval_posture());
        assert_eq!(app.approval_mode, ApprovalMode::Bypass);

        assert!(app.cycle_approval_posture());
        assert_eq!(app.approval_mode, ApprovalMode::Suggest);
        let persisted =
            std::fs::read_to_string(tmp.path().join("settings.toml")).expect("settings");
        assert!(persisted.contains("permission_posture = \"ask\""));
    }
    // from cycle_approval_posture_emits_rebinding_notice_once
    {
        let _env_lock = lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        let _config_env = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
        let mut options = test_options(false);
        options.start_in_agent_mode = true;
        options.config_path = Some(config_path);
        let mut app = App::new(options, &Config::default());

        assert!(app.cycle_approval_posture());
        let notices = app
            .status_toasts
            .iter()
            .filter(|toast| toast.text.contains("moved to Ctrl+T"))
            .count();
        assert_eq!(notices, 1, "first cycle posts the rebinding notice");

        assert!(app.cycle_approval_posture());
        let notices = app
            .status_toasts
            .iter()
            .filter(|toast| toast.text.contains("moved to Ctrl+T"))
            .count();
        assert_eq!(notices, 1, "notice is one-shot per session");
    }
}

/// Tab cycles the mode, Shift+Tab cycles the permission posture. They are two
/// independent axes, and Plan used to veto the second key — which welded them
/// together on the keyboard: Shift+Tab silently did nothing in Plan.
///
/// Allowing it weakens nothing. Plan's read-only guarantee is mode-derived:
/// `authority` maps `(Plan, _, Bypass)` to `SandboxPolicy::ReadOnly` and
/// `tool_catalog` gates every write tool on `mode != AppMode::Plan`. So the
/// cycle moves the durable Act/Operate baseline while the *live* Plan policy
/// stays `Suggest`, and the new posture lands when the mode leaves Plan.
#[test]
fn plan_permission_cycle_moves_the_baseline_and_leaves_plan_read_only() {
    let _env_lock = lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    let _config_env = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let mut options = test_options(false);
    options.config_path = Some(config_path);
    let mut app = App::new(options, &Config::default());
    app.set_agent_approval_posture(ApprovalMode::Auto);
    app.set_mode(AppMode::Plan);

    assert!(
        app.cycle_approval_posture(),
        "Shift+Tab must still change permissions while in Plan"
    );
    assert_eq!(
        app.mode_prefs.agent_approval_mode,
        ApprovalMode::Bypass,
        "the durable baseline advances Auto -> Full Access"
    );
    assert_eq!(
        app.approval_mode,
        ApprovalMode::Suggest,
        "Plan's live policy is untouched: it stays read-only and asks"
    );
    assert_eq!(
        app.mode,
        AppMode::Plan,
        "changing permissions must not move the mode"
    );
    assert!(
        app.status_toasts
            .iter()
            .any(|toast| toast.text.contains("applies in Work and Operate")),
        "the receipt must say when the new posture starts applying"
    );

    let persisted = std::fs::read_to_string(tmp.path().join("settings.toml")).expect("settings");
    assert!(
        persisted.contains("permission_posture = \"full-access\""),
        "the posture is durable, not dropped because Plan was active: {persisted}"
    );

    app.set_mode(AppMode::Operate);
    assert_eq!(
        app.approval_mode,
        ApprovalMode::Bypass,
        "leaving Plan projects the posture chosen while in Plan"
    );
}

#[test]
fn busy_permission_cycle_moves_the_live_posture() {
    let _env_lock = lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    let _config_env = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let mut options = test_options(false);
    options.config_path = Some(config_path);
    let mut app = App::new(options, &Config::default());
    let before = app.approval_mode;
    app.is_loading = true;

    assert!(app.cycle_approval_posture());
    assert_ne!(app.mode_prefs.agent_approval_mode, before);
    assert!(
        !app.status_message
            .as_deref()
            .is_some_and(|message| message.contains("locked"))
    );
}

#[test]
fn permission_postures_persist_across_restart() {
    let _env_lock = lock_test_env();
    for (cycles, expected) in [
        (1, ApprovalMode::Auto),
        (2, ApprovalMode::Bypass),
        (3, ApprovalMode::Suggest),
    ] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let config_env = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &path);
        let mut options = test_options(false);
        options.start_in_agent_mode = true;
        options.config_path = Some(path.clone());
        let mut app = App::new(options.clone(), &Config::default());
        for _ in 0..cycles {
            assert!(app.cycle_approval_posture());
        }
        assert_eq!(app.approval_mode, expected);
        assert_eq!(app.trust_mode, expected == ApprovalMode::Bypass);

        let restarted = App::new(options, &Config::default());
        assert_eq!(restarted.approval_mode, expected);
        assert_eq!(restarted.mode_prefs.agent_approval_mode, expected);
        assert_eq!(restarted.trust_mode, expected == ApprovalMode::Bypass);
        drop(config_env);
    }
}

#[test]
fn shift_tab_migrates_user_root_policy_to_durable_tui_posture() {
    let _env_lock = lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    let settings_path = tmp.path().join("settings.toml");
    std::fs::write(&config_path, "# keep\napproval_policy = \"on-request\"\n")
        .expect("root config");
    std::fs::write(&settings_path, "permission_posture = \"full-access\"\n").expect("settings");
    let _config_env = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let _approval_env = EnvVarGuard::remove("DEEPSEEK_APPROVAL_POLICY");
    let config = codewhale_plugins::providers::load_config(Some(config_path.clone()), None)
        .expect("load config");
    let mut options = test_options(false);
    options.start_in_agent_mode = true;
    options.config_path = Some(config_path.clone());

    let mut app = App::new(options.clone(), &config);
    assert_eq!(app.approval_mode, ApprovalMode::Suggest);
    assert!(app.approval_policy_locked());

    assert!(app.cycle_root_approval_posture());
    assert_eq!(app.approval_mode, ApprovalMode::Auto);
    assert!(!app.approval_policy_locked());
    let saved_config = std::fs::read_to_string(&config_path).expect("saved config");
    assert!(saved_config.contains("# keep"));
    assert!(!saved_config.contains("approval_policy"));
    let saved_settings = std::fs::read_to_string(&settings_path).expect("saved settings");
    assert!(saved_settings.contains("permission_posture = \"auto-review\""));

    let restarted_config =
        codewhale_plugins::providers::load_config(Some(config_path), None).expect("reload config");
    let restarted = App::new(options, &restarted_config);
    assert_eq!(restarted.approval_mode, ApprovalMode::Auto);
    assert!(!restarted.approval_policy_locked());
}

#[test]
fn legacy_yolo_migrates_root_policy_to_agent_full_access() {
    let _env_lock = lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    let settings_path = tmp.path().join("settings.toml");
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::write(&config_path, "# keep\napproval_policy = \"on-request\"\n")
        .expect("legacy config");
    std::fs::write(&settings_path, "default_mode = \"yolo\"\n").expect("legacy settings");
    let _config_env = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let _approval_env = EnvVarGuard::remove("DEEPSEEK_APPROVAL_POLICY");
    let config = codewhale_plugins::providers::load_config(Some(config_path.clone()), None)
        .expect("load config");
    let mut options = test_options(false);
    options.start_in_agent_mode = false;
    options.workspace = workspace;
    options.config_path = Some(config_path.clone());

    let app = App::new(options.clone(), &config);

    assert_eq!(app.mode, AppMode::Agent);
    assert_eq!(app.approval_mode, ApprovalMode::Bypass);
    assert!(!app.approval_policy_locked());
    let saved_config = std::fs::read_to_string(&config_path).expect("saved config");
    assert!(saved_config.contains("# keep"));
    assert!(!saved_config.contains("approval_policy"));
    let saved_settings = std::fs::read_to_string(&settings_path).expect("saved settings");
    assert!(saved_settings.contains("default_mode = \"agent\""));
    assert!(saved_settings.contains("permission_posture = \"full-access\""));

    let restarted_config =
        codewhale_plugins::providers::load_config(Some(config_path), None).expect("reload config");
    let restarted = App::new(options, &restarted_config);
    assert_eq!(restarted.mode, AppMode::Agent);
    assert_eq!(restarted.approval_mode, ApprovalMode::Bypass);
    assert!(!restarted.approval_policy_locked());
}

#[test]
fn legacy_yolo_honors_a_missing_explicit_config_path_without_home_fallback() {
    let _env_lock = lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let home_config_dir = home.join(codewhale_config::CODEWHALE_APP_DIR);
    let override_dir = tmp.path().join("missing-override");
    let missing_override = override_dir.join("config.toml");
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&home_config_dir).expect("home config dir");
    std::fs::create_dir_all(&override_dir).expect("override dir");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let home_config = home_config_dir.join("config.toml");
    std::fs::write(
        &home_config,
        "# actual fallback\napproval_policy = \"on-request\"\n",
    )
    .expect("home config");
    let override_settings = override_dir.join("settings.toml");
    std::fs::write(&override_settings, "default_mode = \"yolo\"\n").expect("legacy settings");

    let _home = EnvVarGuard::set("HOME", &home);
    let _user_profile = EnvVarGuard::set("USERPROFILE", &home);
    let _codewhale_home = EnvVarGuard::remove("CODEWHALE_HOME");
    let _codewhale_config = EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
    let _deepseek_config = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &missing_override);
    let _approval_env = EnvVarGuard::remove("DEEPSEEK_APPROVAL_POLICY");

    let config = codewhale_plugins::providers::load_config(None, None)
        .expect("load explicit missing config");
    assert_eq!(config.approval_policy, None);
    let mut options = test_options(false);
    options.start_in_agent_mode = false;
    options.workspace = workspace;
    options.config_path = None;

    let app = App::new(options, &config);

    assert_eq!(app.mode, AppMode::Agent);
    assert_eq!(app.approval_mode, ApprovalMode::Bypass);
    assert!(!app.approval_policy_locked());
    assert!(
        !missing_override.exists(),
        "settings migration must not create an unrelated config document"
    );
    let saved_home_config = std::fs::read_to_string(&home_config).expect("untouched home config");
    assert!(saved_home_config.contains("# actual fallback"));
    assert!(saved_home_config.contains("approval_policy = \"on-request\""));
    let saved_settings =
        std::fs::read_to_string(&override_settings).expect("normalized override settings");
    assert!(saved_settings.contains("default_mode = \"agent\""));
    assert!(saved_settings.contains("permission_posture = \"full-access\""));
}

#[test]
fn managed_requirements_ignore_saved_full_access_and_lock_changes() {
    let _env_lock = lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    let requirements_path = tmp.path().join("requirements.toml");
    std::fs::write(
        tmp.path().join("settings.toml"),
        "permission_posture = \"full-access\"\n",
    )
    .expect("settings");
    std::fs::write(
        &requirements_path,
        "allowed_approval_policies = [\"on-request\"]\n",
    )
    .expect("requirements");
    let _config_env = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let config = {
        let __cw_config_init_16_0 = Some(requirements_path.to_string_lossy().into_owned());
        let mut __cw_config_init_16_value: Config = Config::default();
        __cw_config_init_16_value.requirements_path = __cw_config_init_16_0;
        __cw_config_init_16_value
    };

    let mut app = App::new(test_options(false), &config);

    assert!(app.approval_policy_locked());
    assert!(app.approval_policy_requirements_managed());
    assert_eq!(app.approval_mode, ApprovalMode::Suggest);
    assert!(!app.cycle_approval_posture());
    assert_eq!(app.approval_mode, ApprovalMode::Suggest);
    assert!(
        app.status_toasts
            .iter()
            .any(|toast| toast.text.contains("controlled"))
    );
}

#[test]
fn sandbox_requirements_prevent_full_access_overrides() {
    let _env_lock = lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    let requirements_path = tmp.path().join("requirements.toml");
    std::fs::write(
        &requirements_path,
        "allowed_sandbox_modes = [\"workspace-write\"]\n",
    )
    .expect("requirements");
    let _home = EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
    let _config_env = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let config = {
        let __cw_config_init_17_0 = Some(requirements_path.to_string_lossy().into_owned());
        let mut __cw_config_init_17_value: Config = Config::default();
        __cw_config_init_17_value.requirements_path = __cw_config_init_17_0;
        __cw_config_init_17_value
    };

    for (settings, cli_yolo) in [
        ("permission_posture = \"full-access\"\n", false),
        ("", true),
        ("default_mode = \"yolo\"\n", false),
    ] {
        std::fs::write(tmp.path().join("settings.toml"), settings).expect("settings");
        let mut options = test_options(cli_yolo);
        options.workspace = tmp.path().to_path_buf();
        let mut app = App::new(options, &config);

        assert_eq!(app.approval_mode, ApprovalMode::Suggest);
        assert!(!app.trust_mode);
        assert!(!app.yolo);
        assert!(app.approval_policy_requirements_managed());
        assert!(!app.cycle_approval_posture());
        assert_eq!(app.select_yolo_compat(), SettingSelection::Refused);
        assert!(matches!(
            codewhale_engine::core::authority::sandbox_policy_for_turn(
                app.mode,
                app.approval_mode,
                config.sandbox_mode.as_deref(),
                &app.workspace,
                codewhale_engine::core::authority::SandboxNetworkAccess::Restricted,
            ),
            codewhale_sandbox::SandboxPolicy::WorkspaceWrite { .. }
        ));
    }
}

#[test]
fn yolo_entry_points_honor_a_locked_approval_policy() {
    let _env_lock = lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let requirements_path = tmp.path().join("requirements.toml");
    std::fs::write(
        &requirements_path,
        "allowed_approval_policies = [\"on-request\"]\n",
    )
    .expect("requirements");
    let config = {
        let __cw_config_init_18_0 = Some(requirements_path.to_string_lossy().into_owned());
        let mut __cw_config_init_18_value: Config = Config::default();
        __cw_config_init_18_value.requirements_path = __cw_config_init_18_0;
        __cw_config_init_18_value
    };

    let mut options = test_options(false);
    options.yolo = true;
    options.allow_shell = false;
    let mut app = App::new(options, &config);

    assert!(app.approval_policy_locked());
    assert_eq!(app.approval_mode, ApprovalMode::Suggest);
    assert!(!app.allow_shell);
    assert!(!app.trust_mode);
    assert!(!app.yolo);

    assert_eq!(app.select_yolo_compat(), SettingSelection::Refused);
    assert_eq!(app.approval_mode, ApprovalMode::Suggest);
    assert!(!app.allow_shell);
    assert!(!app.yolo);
    assert!(
        app.status_toasts
            .iter()
            .any(|toast| toast.text.contains("controlled"))
    );

    assert!(!app.set_mode_yolo_compat());
    assert_eq!(app.approval_mode, ApprovalMode::Suggest);
    assert!(!app.allow_shell);
    assert!(!app.yolo);
}

#[test]
fn set_mode_agent_to_yolo_to_agent_restores_baseline_without_yolo_leak() {
    // Round-trip Agent -> YOLO -> Agent must not leave YOLO's elevated authority
    // (shell/trust/Auto) bleeding into the restored Agent surface (#3386).
    let mut options = test_options(false);
    options.allow_shell = false;
    options.start_in_agent_mode = true;
    let mut app = App::new(options, &Config::default());
    // User's chosen Agent surface: shell on, trust off, Suggest approvals.
    app.allow_shell = true;
    app.trust_mode = false;
    app.approval_mode = ApprovalMode::Suggest;
    app.yolo_compat_notified = true;

    app.set_mode_yolo_compat();
    assert_eq!(app.mode, AppMode::Agent);
    assert!(app.allow_shell);
    assert!(app.trust_mode);
    assert_eq!(app.approval_mode, ApprovalMode::Bypass);
    assert!(app.yolo);

    app.set_mode(AppMode::Agent);
    assert_eq!(app.mode, AppMode::Agent);
    assert!(app.allow_shell, "shell baseline preserved");
    assert!(
        !app.trust_mode,
        "YOLO trust authority must not leak into Agent"
    );
    assert_eq!(
        app.approval_mode,
        ApprovalMode::Suggest,
        "YOLO Auto approvals must not leak into Agent"
    );
    assert!(!app.yolo);
}

#[test]
fn set_mode_plan_to_yolo_to_agent_does_not_bleed_yolo_into_agent() {
    // Plan -> YOLO -> Agent: the Agent baseline captured before leaving Agent is
    // what we land on, untouched by the transient Plan or YOLO policies (#3386).
    let mut options = test_options(false);
    options.allow_shell = false;
    options.start_in_agent_mode = true;
    let mut app = App::new(options, &Config::default());
    app.allow_shell = false;
    app.trust_mode = false;
    app.approval_mode = ApprovalMode::Never;
    app.yolo_compat_notified = true;

    app.set_mode(AppMode::Plan);
    // Plan is read-only regardless of the baseline.
    assert!(!app.allow_shell);
    assert!(!app.trust_mode);
    assert_eq!(app.approval_mode, ApprovalMode::Suggest);

    app.set_mode_yolo_compat();
    assert!(app.allow_shell);
    assert!(app.trust_mode);
    assert_eq!(app.approval_mode, ApprovalMode::Bypass);

    app.set_mode(AppMode::Agent);
    assert_eq!(app.mode, AppMode::Agent);
    assert!(!app.allow_shell);
    assert!(!app.trust_mode);
    assert_eq!(app.approval_mode, ApprovalMode::Never);
}

#[test]
fn set_mode_captures_agent_edits_as_the_durable_baseline() {
    // Editing the permission surface in Agent updates the baseline that a later
    // Plan -> Agent (or YOLO -> Agent) restores to (#3386).
    let mut options = test_options(false);
    options.allow_shell = false;
    options.start_in_agent_mode = true;
    let mut app = App::new(options, &Config::default());
    assert_eq!(app.mode, AppMode::Agent);
    app.allow_shell = false;
    app.set_agent_approval_posture(ApprovalMode::Suggest);

    // Initial baseline restores to no-shell / Suggest.
    app.set_mode(AppMode::Plan);
    app.set_mode(AppMode::Agent);
    assert!(!app.allow_shell);
    assert_eq!(app.approval_mode, ApprovalMode::Suggest);

    // User now turns shell on and tightens approvals while in Agent.
    app.allow_shell = true;
    app.approval_mode = ApprovalMode::Never;

    // A Plan hop and back must restore the *edited* baseline, not the original.
    app.set_mode(AppMode::Plan);
    assert!(!app.allow_shell, "Plan is read-only");
    app.set_mode(AppMode::Agent);
    assert!(app.allow_shell, "edited shell baseline restored");
    assert_eq!(app.approval_mode, ApprovalMode::Never);
}

#[test]
fn yolo_start_with_default_config_restores_interactive_agent_shell_baseline() {
    // Isolate from the developer's live settings.toml — a saved
    // `permission_posture` (e.g. full-access) must not leak into the
    // durable baseline these assertions depend on.
    let _env_lock = lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    let _config_env = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let mut options = test_options(true);
    options.config_path = Some(config_path);
    let mut app = App::new(options, &Config::default());
    // --yolo starts in Agent mode with the full-access compat shim (M6).
    assert_eq!(app.mode, AppMode::Agent);
    assert!(app.yolo);
    assert!(app.allow_shell);
    assert!(app.trust_mode);
    assert_eq!(app.approval_mode, ApprovalMode::Bypass);

    app.set_mode(AppMode::Agent);
    assert!(
        app.allow_shell,
        "default interactive Agent baseline should expose approval-gated shell after YOLO downshift"
    );
    assert!(!app.trust_mode);
    assert_eq!(app.approval_mode, ApprovalMode::Suggest);
}

#[test]
fn leaving_yolo_after_startup_restores_baseline_policies() {
    // Isolate from the developer's live settings.toml — a saved
    // `permission_posture` (e.g. full-access) must not leak into the
    // durable baseline these assertions depend on.
    let _env_lock = lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.toml");
    let _config_env = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let config = {
        let __cw_config_init_19_0 = Some(false);
        let mut __cw_config_init_19_value: Config = Default::default();
        __cw_config_init_19_value.allow_shell = __cw_config_init_19_0;
        __cw_config_init_19_value
    };

    let mut options = test_options(true);
    options.config_path = Some(config_path);
    let mut app = App::new(options, &config);
    // --yolo starts in Agent mode with the full-access compat shim (M6).
    assert_eq!(app.mode, AppMode::Agent);
    assert!(app.yolo);
    assert!(app.allow_shell);
    assert!(app.trust_mode);
    assert_eq!(app.approval_mode, ApprovalMode::Bypass);

    app.set_mode(AppMode::Agent);
    assert!(!app.allow_shell);
    assert!(!app.trust_mode);
    assert_eq!(app.approval_mode, ApprovalMode::Suggest);
}

#[test]
fn configured_approval_policy_initializes_live_approval_mode() {
    let config = {
        let __cw_config_init_20_0 = Some("never".to_string());
        let mut __cw_config_init_20_value: Config = Default::default();
        __cw_config_init_20_value.approval_policy = __cw_config_init_20_0;
        __cw_config_init_20_value
    };
    let mut options = test_options(false);
    options.start_in_agent_mode = true;

    let app = App::new(options, &config);

    assert_eq!(app.mode, AppMode::Agent);
    assert_eq!(app.approval_mode, ApprovalMode::Never);
}

#[test]
fn test_mark_history_updated() {
    let mut app = App::new(test_options(false), &Config::default());
    let initial_version = app.history_version;
    app.mark_history_updated();
    assert!(app.history_version > initial_version);
}

#[test]
fn live_motion_invalidation_only_bumps_live_transcript_rows() {
    let mut app = App::new(test_options(false), &Config::default());
    app.history = vec![
        HistoryCell::Assistant {
            content: "settled".to_string(),
            streaming: false,
        },
        HistoryCell::Assistant {
            content: "streaming".to_string(),
            streaming: true,
        },
        HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
            name: "read_file".to_string(),
            status: ToolStatus::Running,
            input_summary: None,
            output: None,
            prompts: None,
            spillover_path: None,
            output_summary: None,
            is_diff: false,
        })),
        HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
            name: "agent".to_string(),
            status: ToolStatus::Running,
            input_summary: Some("action: spawn".to_string()),
            output: None,
            prompts: None,
            spillover_path: None,
            output_summary: None,
            is_diff: false,
        })),
        HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
            name: "read_file".to_string(),
            status: ToolStatus::Success,
            input_summary: None,
            output: Some("done".to_string()),
            prompts: None,
            spillover_path: None,
            output_summary: None,
            is_diff: false,
        })),
    ];
    app.resync_history_revisions();
    let history_before = app.history_revisions.clone();

    let active = app.active_cell.get_or_insert_with(ActiveCell::new);
    active.push_untracked(HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
        name: "web_search".to_string(),
        status: ToolStatus::Running,
        input_summary: None,
        output: None,
        prompts: None,
        spillover_path: None,
        output_summary: None,
        is_diff: false,
    })));
    let app_active_before = app.active_cell_revision;
    let cell_active_before = app.active_cell.as_ref().expect("active cell").revision();

    app.mark_live_motion_updated();

    assert_eq!(app.history_revisions[0], history_before[0]);
    assert_ne!(app.history_revisions[1], history_before[1]);
    assert_ne!(app.history_revisions[2], history_before[2]);
    assert_eq!(app.history_revisions[3], history_before[3]);
    assert_eq!(app.history_revisions[4], history_before[4]);
    assert_ne!(app.active_cell_revision, app_active_before);
    assert_ne!(
        app.active_cell.as_ref().expect("active cell").revision(),
        cell_active_before
    );

    let history_after_all_live = app.history_revisions.clone();
    let app_active_after_all_live = app.active_cell_revision;
    let cell_active_after_all_live = app.active_cell.as_ref().expect("active cell").revision();
    app.mark_live_history_motion_updated();

    assert_eq!(app.history_revisions[0], history_after_all_live[0]);
    assert_ne!(app.history_revisions[1], history_after_all_live[1]);
    assert_ne!(app.history_revisions[2], history_after_all_live[2]);
    assert_eq!(app.history_revisions[3], history_after_all_live[3]);
    assert_eq!(app.history_revisions[4], history_after_all_live[4]);
    assert_eq!(app.active_cell_revision, app_active_after_all_live);
    assert_eq!(
        app.active_cell.as_ref().expect("active cell").revision(),
        cell_active_after_all_live
    );
}

#[test]
fn expanded_tool_scenario() {
    // Scenario consolidation of: expanded_tool_runs_rebase_when_history_prefix_shifts, expanded_tool_runs_prune_when_history_is_truncated
    // from expanded_tool_runs_rebase_when_history_prefix_shifts
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.expanded_tool_runs = std::collections::HashSet::from([2usize, 6usize]);

        app.shift_history_maps_down(3);

        assert_eq!(app.expanded_tool_runs, std::collections::HashSet::from([4]));
    }
    // from expanded_tool_runs_prune_when_history_is_truncated
    {
        let mut app = App::new(test_options(false), &Config::default());
        for idx in 0..5 {
            app.add_message(HistoryCell::System {
                content: format!("cell {idx}"),
            });
        }
        app.expanded_tool_runs = std::collections::HashSet::from([1usize, 4usize]);

        app.truncate_history_to(3);

        assert_eq!(app.expanded_tool_runs, std::collections::HashSet::from([1]));
    }
}

#[test]
fn tool_run_expansion_toggle_opens_and_closes_run() {
    let mut app = App::new(test_options(false), &Config::default());
    app.tool_collapse_mode = ToolCollapseMode::Compact;
    app.tool_collapse_threshold = 3;
    for name in ["read_file", "list_dir", "web_search"] {
        app.add_message(HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
            name: name.to_string(),
            status: ToolStatus::Success,
            input_summary: None,
            output: Some("ok".to_string()),
            prompts: None,
            spillover_path: None,
            output_summary: None,
            is_diff: false,
        })));
    }

    assert!(app.toggle_tool_run_expansion_at(0));
    assert!(app.expanded_tool_runs.contains(&0));
    assert!(app.toggle_tool_run_expansion_at(2));
    assert!(!app.expanded_tool_runs.contains(&0));
    assert!(!app.toggle_tool_run_expansion_at(99));
}

#[test]
fn tool_run_expansion_toggle_handles_active_run() {
    let mut app = App::new(test_options(false), &Config::default());
    app.tool_collapse_mode = ToolCollapseMode::Compact;
    app.tool_collapse_threshold = 3;
    app.add_message(HistoryCell::User {
        content: "go".to_string(),
    });

    let active_start = app.history.len();
    let active = app.active_cell.get_or_insert_with(ActiveCell::new);
    for name in ["read_file", "list_dir", "web_search"] {
        active.push_untracked(HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
            name: name.to_string(),
            status: ToolStatus::Success,
            input_summary: None,
            output: Some("ok".to_string()),
            prompts: None,
            spillover_path: None,
            output_summary: None,
            is_diff: false,
        })));
    }

    assert!(app.toggle_tool_run_expansion_at(active_start));
    assert!(app.expanded_tool_runs.contains(&active_start));
    assert!(app.toggle_tool_run_expansion_at(active_start + 2));
    assert!(!app.expanded_tool_runs.contains(&active_start));
}

#[test]
fn test_scroll_operations() {
    let mut app = App::new(test_options(false), &Config::default());
    // Just verify scroll methods can be called without panic
    app.scroll_up(5);
    app.scroll_down(3);
}

#[test]
fn resize_preserves_scrolled_transcript_position() {
    let mut app = App::new(test_options(false), &Config::default());
    app.viewport.transcript_scroll = TranscriptScroll::at_line(42);
    app.viewport.last_transcript_top = 42;
    app.viewport.pending_scroll_delta = 5;

    app.handle_resize(120, 40);

    let meta = vec![
        TranscriptLineMeta::Spacer {
            copy_prefix_width: 0
        };
        240
    ];
    let (_, top) = app.viewport.transcript_scroll.resolve_top(&meta, 200);
    assert_eq!(top, 42);
    assert_eq!(app.viewport.pending_scroll_delta, 0);
}

#[test]
fn resize_keeps_tail_state_when_user_was_at_tail() {
    let mut app = App::new(test_options(false), &Config::default());
    app.viewport.transcript_scroll = TranscriptScroll::to_bottom();
    app.viewport.last_transcript_top = 42;

    app.handle_resize(120, 40);

    assert!(app.viewport.transcript_scroll.is_at_tail());
}

#[test]
fn resize_seeds_visible_height_for_paging_before_next_render() {
    let mut app = App::new(test_options(false), &Config::default());
    app.viewport.last_transcript_visible = 12;

    app.handle_resize(120, 40);
    assert_eq!(app.viewport.last_transcript_visible, 38);

    app.handle_resize(120, 1);
    assert_eq!(app.viewport.last_transcript_visible, 1);
}

#[test]
fn test_add_message() {
    let mut app = App::new(test_options(false), &Config::default());
    let initial_len = app.history.len();
    app.add_message(HistoryCell::User {
        content: "test".to_string(),
    });
    assert_eq!(app.history.len(), initial_len + 1);
}

#[test]
fn test_compaction_config() {
    let mut app = App::new(test_options(false), &Config::default());
    let config = app.compaction_config();
    // Config should be valid (just checking it returns something)
    let _ = config.enabled;

    app.auto_model = true;
    app.model = "auto".to_string();
    app.last_effective_model = None;
    let config = app.compaction_config();
    assert_eq!(config.model, DEFAULT_TEXT_MODEL);

    app.last_effective_model = Some("deepseek-v4-flash".to_string());
    let config = app.compaction_config();
    assert_eq!(config.model, "deepseek-v4-flash");
}

#[test]
fn test_update_model_compaction_budget() {
    let mut app = App::new(test_options(false), &Config::default());
    // Pin the inputs so the budget math is deterministic and does not
    // depend on the developer's local `auto_compact_threshold_percent`
    // setting (App::new loads real settings) or on auto-model resolution.
    app.auto_model = false;
    app.api_provider = ProviderKind::Deepseek;
    app.active_route_limits = None;
    app.active_context_window_override = None;
    // The installed window outranks the route limits, so a fixture that only
    // clears the limits keeps the window `App::new` resolved for its default
    // model and the budget is computed against the wrong one. Production
    // clears it through `set_model_selection`; mutating `app.model` directly
    // has to do the same.
    app.active_context_window = None;
    app.auto_compact_threshold_percent = 80.0;

    // A large-context model earns a proportionally larger compaction
    // budget; an unknown model falls back to the fixed default threshold.
    app.model = "deepseek-v4-pro".to_string();
    app.update_model_compaction_budget();
    let large_window_threshold = app.compact_threshold;

    app.model = "unknown-test-model".to_string();
    app.update_model_compaction_budget();
    let unknown_threshold = app.compact_threshold;

    assert!(
        unknown_threshold > 0,
        "unknown model must still get a positive budget"
    );
    assert!(
        large_window_threshold > unknown_threshold,
        "a large-context model ({large_window_threshold}) should budget more \
         than an unknown model ({unknown_threshold})"
    );
}

#[test]
fn test_input_history_navigation() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input_history.push("first".to_string());
    app.input_history.push("second".to_string());

    // Navigate up
    app.history_up();
    assert!(app.history_index.is_some());

    // Navigate down
    app.history_down();
}

#[test]
fn paste_while_navigating_history_detaches_before_down_can_discard_it() {
    // A paste (insert_str family) while a history entry is on screen must
    // detach navigation like typing does; otherwise the next Down replaces
    // the buffer and silently destroys the pasted text.
    let mut app = App::new(test_options(false), &Config::default());
    app.input_history.push("older".to_string());
    app.input_history.push("newer".to_string());
    app.input = "draft".to_string();

    app.history_up();
    assert_eq!(app.input, "newer");
    app.insert_str(" pasted");
    assert!(app.history_index.is_none());
    assert_eq!(app.input, "newer pasted");

    app.history_down();
    assert_eq!(
        app.input, "newer pasted",
        "detached edit must survive history keys"
    );
}

#[test]
fn external_edit_while_navigating_history_detaches_stale_state() {
    // Same hazard through the $EDITOR round-trip: the edited buffer replaces
    // recalled history, so the stale index, draft, selection, and attachment
    // positions must not survive it.
    let mut app = App::new(test_options(false), &Config::default());
    app.input_history.push("older".to_string());
    app.input = "draft".to_string();

    app.history_up();
    assert_eq!(app.input, "older");
    app.apply_external_edit("edited in vi".to_string());
    assert!(app.history_index.is_none());
    assert!(app.history_navigation_draft.is_none());
    assert!(app.selection_anchor.is_none());
    assert_eq!(app.input, "edited in vi");

    app.history_down();
    assert_eq!(
        app.input, "edited in vi",
        "detached edit must survive history keys"
    );
}

#[test]
fn input_history_scenario() {
    // Scenario consolidation of: input_history_down_restores_live_draft_after_accidental_up, input_history_navigation_clears_stale_selection, input_history_restores_empty_draft_at_end_of_navigation
    // from input_history_down_restores_live_draft_after_accidental_up
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input_history.push("previous prompt".to_string());
        app.input = "careful current draft".to_string();
        app.cursor_position = "careful".chars().count();

        app.history_up();
        assert_eq!(app.input, "previous prompt");

        app.history_down();
        assert_eq!(app.input, "careful current draft");
        assert_eq!(app.cursor_position, "careful".chars().count());
        assert!(app.history_index.is_none());
    }
    // from input_history_navigation_clears_stale_selection
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input_history.push("previous input".to_string());
        app.input = "hello world".to_string();
        app.cursor_position = "hello ".chars().count();
        app.selection_anchor = Some(app.input.chars().count());

        app.history_up();
        assert_eq!(app.input, "previous input");
        assert!(app.selection_anchor.is_none());

        app.insert_char('x');
        assert_eq!(app.input, "previous inputx");
    }
    // from input_history_restores_empty_draft_at_end_of_navigation
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input_history.push("previous prompt".to_string());

        app.history_up();
        assert_eq!(app.input, "previous prompt");

        app.history_down();
        assert!(app.input.is_empty());
        assert_eq!(app.cursor_position, 0);
        assert!(app.history_index.is_none());
    }
}

#[test]
fn word_cursor_helpers_move_by_whitespace_delimited_words() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "alpha beta  gamma".to_string();
    app.cursor_position = 0;

    app.move_cursor_word_forward();
    assert_eq!(app.cursor_position, "alpha ".chars().count());

    app.move_cursor_word_forward();
    assert_eq!(app.cursor_position, "alpha beta  ".chars().count());

    app.move_cursor_word_backward();
    assert_eq!(app.cursor_position, "alpha ".chars().count());
}

#[test]
fn editing_history_entry_leaves_navigation_mode() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input_history.push("previous prompt".to_string());
    app.input = "current draft".to_string();
    app.cursor_position = app.input.chars().count();

    app.history_up();
    app.insert_char('!');
    app.history_down();

    assert_eq!(app.input, "previous prompt!");
    assert!(app.history_index.is_none());
}

#[test]
fn history_search_scenario() {
    // Scenario consolidation of: history_search_filters_matches_and_skips_duplicates, history_search_matches_unicode_case_insensitively, history_search_accepts_match_without_submitting, history_search_cancel_restores_pre_search_draft
    // from history_search_filters_matches_and_skips_duplicates
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input_history.clear();
        app.input_history.push("alpha one".to_string());
        app.input_history.push("beta two".to_string());
        app.input_history.push("alpha one".to_string());
        app.draft_history.push_back("draft alpha".to_string());

        app.start_history_search();
        app.history_search_insert_str("alpha");

        assert_eq!(
            app.history_search_matches(),
            vec!["draft alpha".to_string(), "alpha one".to_string()]
        );
    }
    // from history_search_matches_unicode_case_insensitively
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input_history.clear();
        app.input_history.push("CAFÉ prompt".to_string());

        app.start_history_search();
        app.history_search_insert_str("café");

        assert_eq!(
            app.history_search_matches(),
            vec!["CAFÉ prompt".to_string()]
        );
    }
    // from history_search_accepts_match_without_submitting
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input_history.clear();
        app.input_history.push("older prompt".to_string());

        app.start_history_search();
        app.history_search_insert_str("older");

        assert!(app.accept_history_search());
        assert_eq!(app.input, "older prompt");
        assert_eq!(app.cursor_position, "older prompt".chars().count());
        assert!(app.composer_history_search.is_none());
    }
    // from history_search_cancel_restores_pre_search_draft
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input_history.clear();
        app.input = "current draft".to_string();
        app.cursor_position = 7;
        app.input_history.push("older prompt".to_string());

        app.start_history_search();
        app.history_search_insert_str("older");
        app.cancel_history_search();

        assert_eq!(app.input, "current draft");
        assert_eq!(app.cursor_position, 7);
        assert!(app.composer_history_search.is_none());
    }
}

#[test]
fn recoverable_clear_stashes_nonempty_draft() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input_history.clear();
    app.input = "recover this".to_string();
    app.cursor_position = app.input.chars().count();

    app.clear_input_recoverable();
    app.start_history_search();
    app.history_search_insert_str("recover");

    assert_eq!(
        app.history_search_matches(),
        vec!["recover this".to_string()]
    );
}

#[test]
fn clear_undo_scenario() {
    // Scenario consolidation of: clear_undo_buffer_is_set_on_clear_input_recoverable, clear_undo_buffer_is_none_when_clearing_empty_input
    // from clear_undo_buffer_is_set_on_clear_input_recoverable
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "hello".to_string();
        app.cursor_position = 5;

        app.clear_input_recoverable();

        assert!(app.input.is_empty());
        assert_eq!(app.clear_undo_buffer.as_deref(), Some("hello"));
    }
    // from clear_undo_buffer_is_none_when_clearing_empty_input
    {
        let mut app = App::new(test_options(false), &Config::default());
        assert!(app.input.is_empty());

        app.clear_input_recoverable();

        assert!(app.clear_undo_buffer.is_none());
    }
}

#[test]
fn composer_paste_flushes_pending_burst_and_normalizes_crlf() {
    let mut app = App::new(test_options(false), &Config::default());
    app.use_paste_burst_detection = true;
    let now = Instant::now();
    let key = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('x'),
        crossterm::event::KeyModifiers::NONE,
    );

    assert!(crate::tui::paste::handle_paste_burst_key(
        &mut app, &key, now
    ));
    assert!(
        app.input.is_empty(),
        "first burst char should stay buffered"
    );

    app.insert_paste_text("a\r\nb\rc");

    assert_eq!(app.input, "xa\nb\nc");
    assert_eq!(app.cursor_position, "xa\nb\nc".chars().count());
    assert!(!app.paste_burst.is_active());
}

#[test]
fn bracketed_paste_preserves_bare_carriage_return_line_breaks() {
    let mut app = App::new(test_options(false), &Config::default());

    app.insert_paste_text("alpha\r  indented\r# literal heading\r- literal list");

    assert_eq!(
        app.input,
        "alpha\n  indented\n# literal heading\n- literal list"
    );
    assert_eq!(app.cursor_position, app.input.chars().count());
}

#[test]
fn enter_during_active_paste_burst_appends_newline_to_buffer_not_submit() {
    // #1073: when chars are still being assembled into a paste burst and
    // an Enter arrives (the trailing newline of the paste), the Enter
    // must be absorbed into the burst buffer — not fired as a submit.
    let mut app = App::new(test_options(false), &Config::default());
    app.use_paste_burst_detection = true;
    let now = Instant::now();
    app.paste_burst.append_char_to_buffer('h', now);
    app.paste_burst.append_char_to_buffer('i', now);
    assert!(app.paste_burst.is_active());
    assert!(app.input.is_empty());

    let result = app.handle_composer_enter(true);

    assert!(
        result.is_none(),
        "Enter during active paste burst must not submit"
    );
    let flushed = app.paste_burst.flush_before_modified_input();
    assert_eq!(
        flushed.as_deref(),
        Some("hi\n"),
        "newline must land in the burst buffer so the next flush carries it"
    );
}

#[test]
fn enter_inside_paste_burst_window_after_flush_inserts_newline_not_submit() {
    // #1073: after a burst has flushed (text now in `input`), the
    // suppression window stays open for ~120ms. An Enter arriving in
    // that window is the trailing newline of the paste, not a user
    // submit — insert it as a literal newline into the composer.
    let mut app = App::new(test_options(false), &Config::default());
    app.use_paste_burst_detection = true;
    app.input = "hello".to_string();
    app.cursor_position = "hello".chars().count();
    let now = Instant::now();
    app.paste_burst.extend_window(now);
    assert!(!app.paste_burst.is_active());
    assert!(
        app.paste_burst.newline_should_insert_instead_of_submit(now),
        "suppression window should be open"
    );

    let result = app.handle_composer_enter(true);

    assert!(
        result.is_none(),
        "Enter inside post-flush suppression window must not submit"
    );
    assert_eq!(
        app.input, "hello\n",
        "newline must be inserted into the composer instead of firing a submit"
    );
    assert!(!app.composer_enter_would_submit(true));
    assert!(app.composer_enter_would_submit(false));
    assert_eq!(app.handle_composer_enter(false).as_deref(), Some("hello\n"));
}

/// The absorbed Enter above must not buy the window more time. Re-arming on
/// it meant a user pressing Enter to send kept extending suppression by
/// another 120ms per press, so the composer only ever grew newlines and
/// never submitted.
#[test]
fn enter_absorbed_after_flush_does_not_re_arm_the_suppression_window() {
    let mut app = App::new(test_options(false), &Config::default());
    app.use_paste_burst_detection = true;
    app.input = "hello".to_string();
    app.cursor_position = "hello".chars().count();
    let now = Instant::now();
    app.paste_burst.extend_window(now);

    assert!(
        app.handle_composer_enter(true).is_none(),
        "first Enter is absorbed as the paste's possible trailing newline"
    );
    assert_eq!(app.input, "hello\n");

    // The window must still expire relative to `now` — the moment the burst
    // last saw real input — not relative to the Enter that was absorbed.
    assert!(
        !app.paste_burst
            .newline_should_insert_instead_of_submit(now + Duration::from_millis(121)),
        "absorbing an Enter must not extend the suppression window"
    );
}

#[test]
fn enter_outside_any_paste_burst_window_submits_normally() {
    // Regression guard: the suppression must not trip when the user
    // actually wants to submit.
    let mut app = App::new(test_options(false), &Config::default());
    app.use_paste_burst_detection = true;
    app.input = "hello world".to_string();
    app.cursor_position = "hello world".chars().count();

    let result = app.handle_composer_enter(true);

    assert_eq!(
        result.as_deref(),
        Some("hello world"),
        "Enter outside any paste burst window must submit normally"
    );
    assert!(
        app.input.is_empty(),
        "submit_input should clear the composer"
    );
}

#[test]
fn enter_with_paste_burst_detection_disabled_submits_normally() {
    // When the user has explicitly turned off paste-burst detection
    // (`bracketed_paste = false` is independent, this is the
    // `paste_burst_detection` setting), the suppression must be
    // skipped — otherwise turning it off would not actually turn it
    // off.
    let mut app = App::new(test_options(false), &Config::default());
    app.use_paste_burst_detection = false;
    app.input = "ship it".to_string();
    app.cursor_position = "ship it".chars().count();
    let now = Instant::now();
    app.paste_burst.extend_window(now);

    let result = app.handle_composer_enter(true);

    assert_eq!(result.as_deref(), Some("ship it"));
}

#[test]
fn clipboard_text_paste_matches_bracketed_paste_state() {
    let text = "alpha\r\nbeta";
    let mut bracketed = App::new(test_options(false), &Config::default());
    let mut clipboard = App::new(test_options(false), &Config::default());

    bracketed.insert_paste_text(text);
    clipboard.apply_clipboard_content(ClipboardContent::Text(text.to_string()));

    assert_eq!(clipboard.input, bracketed.input);
    assert_eq!(clipboard.cursor_position, bracketed.cursor_position);
    assert_eq!(clipboard.slash_menu_hidden, bracketed.slash_menu_hidden);
    assert_eq!(clipboard.mention_menu_hidden, bracketed.mention_menu_hidden);
}

#[test]
fn ssh_direct_clipboard_paste_points_to_terminal_owned_bracketed_paste() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "keep this draft".to_string();
    app.cursor_position = app.input.chars().count();
    app.clipboard = ClipboardHandler::for_test(true, true);

    assert!(!app.paste_from_clipboard());
    assert_eq!(app.input, "keep this draft");
    let hint = app
        .status_message
        .as_deref()
        .expect("remote paste hint")
        .to_string();
    assert!(hint.contains("SSH paste uses your local terminal"));
    assert!(hint.contains("Cmd+V on macOS"));
    assert!(hint.contains("Ctrl+Shift+V on Linux/Windows"));
}

#[test]
fn clipboard_image_paste_keeps_adjacent_text_and_concise_status() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "before after".to_string();
    app.cursor_position = "before".chars().count();

    app.apply_clipboard_content(ClipboardContent::Image(PastedImage {
        path: PathBuf::from("/tmp/pasted.png"),
        width: 8,
        height: 4,
        byte_len: 2048,
    }));

    assert!(
        app.input
            .contains("before\n[Attached image: 8x4 PNG (2KB) at /tmp/pasted.png]")
    );
    assert!(app.input.contains("] after"));
    let status = app.status_message.as_deref().expect("status message");
    assert_eq!(status, "Attached image: 8x4 PNG (2KB)");
}

#[test]
fn pasted_text_and_image_placeholders_survive_history_and_queue_paths() {
    let mut app = App::new(test_options(false), &Config::default());
    app.insert_paste_text("line 1\r\nline 2");
    app.insert_media_attachment("image", Path::new("/tmp/pasted.png"), Some("8x4 PNG (2KB)"));

    let submitted = app.submit_input().expect("submitted input");
    assert!(submitted.contains("line 1\nline 2"));
    assert!(submitted.contains("[Attached image: 8x4 PNG (2KB) at /tmp/pasted.png]"));

    app.history_up();
    assert_eq!(app.input, submitted);
    assert_eq!(app.composer_attachment_count(), 1);

    app.clear_input();
    app.queue_message(QueuedMessage::new(
        submitted.clone(),
        Some("Use this skill".to_string()),
    ));
    assert!(app.pop_last_queued_into_draft());
    assert_eq!(app.input, submitted);
    assert_eq!(app.composer_attachment_count(), 1);
    assert_eq!(
        app.queued_draft
            .as_ref()
            .and_then(|draft| draft.skill_instruction.as_deref()),
        Some("Use this skill")
    );

    app.push_pending_steer(QueuedMessage::new(submitted.clone(), None));
    let steers = app.drain_pending_steers();
    assert_eq!(steers[0].display, submitted);
}

#[test]
fn selected_attachment_row_removes_placeholder_without_manual_editing() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "before".to_string();
    app.cursor_position = "before".chars().count();
    app.insert_media_attachment("image", Path::new("/tmp/pasted.png"), Some("8x4 PNG"));
    app.insert_str("after");

    app.move_cursor_start();
    assert!(app.select_previous_composer_attachment());
    assert_eq!(app.selected_composer_attachment_index(), Some(0));
    assert!(app.remove_selected_composer_attachment());

    assert!(!app.input.contains("[Attached image:"));
    assert!(app.input.contains("before"));
    assert!(app.input.contains("after"));
    assert_eq!(app.composer_attachment_count(), 0);
    assert!(app.selected_composer_attachment_index().is_none());
}

#[test]
fn kill_to_end_of_line_cuts_from_middle_of_word() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "hello world".to_string();
    app.cursor_position = 6; // before 'w'
    assert!(app.kill_to_end_of_line());
    assert_eq!(app.input, "hello ");
    assert_eq!(app.cursor_position, 6);
    assert_eq!(app.kill_buffer, "world");
}

#[test]
fn kill_at_eol_consumes_following_newline() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "line one\nline two".to_string();
    app.cursor_position = 8; // sitting on the '\n'
    assert!(app.kill_to_end_of_line());
    assert_eq!(app.input, "line oneline two");
    assert_eq!(app.cursor_position, 8);
    assert_eq!(app.kill_buffer, "\n");

    // Empty input: kill is a no-op and the buffer is untouched.
    let mut empty = App::new(test_options(false), &Config::default());
    assert!(!empty.kill_to_end_of_line());
    assert!(empty.input.is_empty());
    assert!(empty.kill_buffer.is_empty());
}

#[test]
fn yank_inserts_kill_buffer_and_preserves_it() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "abc def".to_string();
    app.cursor_position = 4; // before 'd'
    assert!(app.kill_to_end_of_line());
    assert_eq!(app.input, "abc ");
    assert_eq!(app.kill_buffer, "def");

    // Move cursor to the start and yank twice — kill_buffer must persist.
    app.cursor_position = 0;
    assert!(app.yank());
    assert!(app.yank());
    assert_eq!(app.input, "defdefabc ");
    assert_eq!(app.cursor_position, 6);
    assert_eq!(app.kill_buffer, "def");

    // Yank with empty buffer is a no-op.
    let mut empty = App::new(test_options(false), &Config::default());
    assert!(!empty.yank());
    assert!(empty.input.is_empty());
}

// ---- Issue #90: quit confirmation timeout ----

#[test]
fn quit_is_not_armed_by_default() {
    let app = App::new(test_options(false), &Config::default());
    assert!(!app.quit_is_armed());
    assert!(app.quit_armed_until.is_none());
}

#[test]
fn arm_quit_sets_two_second_window() {
    let mut app = App::new(test_options(false), &Config::default());
    app.arm_quit();
    assert!(app.quit_is_armed());
    let deadline = app.quit_armed_until.expect("deadline set");
    let remaining = deadline.saturating_duration_since(Instant::now());
    // Allow a generous margin for slow CI machines: 1.5s..=2.0s.
    assert!(
        remaining >= Duration::from_millis(1500) && remaining <= Duration::from_secs(2),
        "expected ~2s window, got {remaining:?}",
    );
    assert!(app.needs_redraw, "armed prompt should request a redraw");
}

#[test]
fn disarm_quit_scenario() {
    // Scenario consolidation of: disarm_quit_clears_the_timer, disarm_quit_when_not_armed_is_a_noop
    // from disarm_quit_clears_the_timer
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.arm_quit();
        app.needs_redraw = false;
        app.disarm_quit();
        assert!(!app.quit_is_armed());
        assert!(app.quit_armed_until.is_none());
        assert!(app.needs_redraw, "disarming should request a redraw");
    }
    // from disarm_quit_when_not_armed_is_a_noop
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.needs_redraw = false;
        app.disarm_quit();
        assert!(!app.needs_redraw, "no redraw when nothing changed");
    }
}

#[test]
fn quit_armed_scenario() {
    // Scenario consolidation of: quit_armed_expires_after_window, quit_armed_tick_is_noop_within_window
    // from quit_armed_expires_after_window
    {
        let mut app = App::new(test_options(false), &Config::default());
        // Pin the deadline in the past to simulate a stale timer.
        app.quit_armed_until = Some(Instant::now() - Duration::from_millis(10));
        assert!(
            !app.quit_is_armed(),
            "expired timer must not count as armed"
        );

        app.needs_redraw = false;
        app.tick_quit_armed();
        assert!(app.quit_armed_until.is_none(), "tick clears expired timer");
        assert!(
            app.needs_redraw,
            "expiry triggers a redraw to repaint footer"
        );
    }
    // from quit_armed_tick_is_noop_within_window
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.arm_quit();
        app.needs_redraw = false;
        app.tick_quit_armed();
        assert!(
            app.quit_is_armed(),
            "tick within window keeps the timer armed"
        );
        assert!(!app.needs_redraw, "no redraw when nothing changed");
    }
}

#[test]
fn re_arming_after_expiry_starts_a_fresh_window() {
    let mut app = App::new(test_options(false), &Config::default());
    app.quit_armed_until = Some(Instant::now() - Duration::from_secs(5));
    app.tick_quit_armed();
    assert!(app.quit_armed_until.is_none());
    app.arm_quit();
    let deadline = app.quit_armed_until.expect("re-armed");
    assert!(deadline > Instant::now(), "fresh deadline in the future");
}

// ---- Issue #208: in-flight input routing ----

#[test]
fn submit_disposition_scenario() {
    // Scenario consolidation of: submit_disposition_immediate_when_idle_and_online, submit_disposition_queue_when_busy_and_online_not_streaming, submit_disposition_queue_when_busy_and_streaming, submit_disposition_queue_when_offline_and_idle, submit_disposition_offline_busy_queues, submit_disposition_does_not_mutate_the_queue
    // from submit_disposition_immediate_when_idle_and_online
    {
        let app = App::new(test_options(false), &Config::default());
        assert!(!app.is_loading);
        assert!(!app.offline_mode);
        assert_eq!(
            app.decide_submit_disposition(),
            SubmitDisposition::Immediate
        );
    }
    // from submit_disposition_queue_when_busy_and_online_not_streaming
    {
        // Bare Enter has one stable busy-state meaning even before the provider
        // emits its first token: queue a follow-up for the next turn.
        let mut app = App::new(test_options(false), &Config::default());
        app.is_loading = true;
        app.offline_mode = false;
        // streaming_message_index is None (default) → waiting phase
        assert_eq!(app.decide_submit_disposition(), SubmitDisposition::Queue);
    }
    // from submit_disposition_queue_when_busy_and_streaming
    {
        // #382: Busy + streaming → Queue (was QueueFollowUp; now unified)
        let mut app = App::new(test_options(false), &Config::default());
        app.is_loading = true;
        app.offline_mode = false;
        app.streaming_message_index = Some(0);
        assert_eq!(app.decide_submit_disposition(), SubmitDisposition::Queue);
    }
    // from submit_disposition_queue_when_offline_and_idle
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.is_loading = false;
        app.offline_mode = true;
        assert_eq!(app.decide_submit_disposition(), SubmitDisposition::Queue);
    }
    // from submit_disposition_offline_busy_queues
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.is_loading = true;
        app.offline_mode = true;
        // Offline mode always queues, even when streaming
        app.streaming_message_index = Some(0);
        assert_eq!(app.decide_submit_disposition(), SubmitDisposition::Queue);
    }
    // from submit_disposition_does_not_mutate_the_queue
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.is_loading = true;
        app.streaming_message_index = Some(0);
        assert_eq!(app.enter_with_double_tap(), Some(SubmitDisposition::Queue));
        app.queue_message(QueuedMessage::new("older queued".to_string(), None));
        app.queue_message(QueuedMessage::new("just typed follow-up".to_string(), None));
        assert!(app.input.is_empty());
        // The event loop owns empty-Enter queue promotion. Merely asking for the
        // disposition must not mutate queue state — even when the answer is the
        // double-tap Steer.
        assert_eq!(app.enter_with_double_tap(), Some(SubmitDisposition::Steer));
        assert_eq!(app.queued_message_count(), 2);
    }
}

#[test]
fn composer_submit_state_by_chord_matrix() {
    use super::{ComposerSubmitAction, ComposerSubmitChord};

    let mut app = App::new(test_options(false), &Config::default());
    app.input = "hello".to_string();
    assert_eq!(
        app.decide_composer_submit(ComposerSubmitChord::Enter),
        ComposerSubmitAction::Submit(SubmitDisposition::Immediate)
    );
    assert_eq!(
        app.decide_composer_submit(ComposerSubmitChord::CtrlEnter),
        ComposerSubmitAction::Submit(SubmitDisposition::Immediate)
    );

    app.is_loading = true;
    assert_eq!(
        app.decide_composer_submit(ComposerSubmitChord::Enter),
        ComposerSubmitAction::Submit(SubmitDisposition::Queue)
    );
    assert_eq!(
        app.decide_composer_submit(ComposerSubmitChord::CtrlEnter),
        ComposerSubmitAction::Submit(SubmitDisposition::Steer)
    );

    app.streaming_message_index = Some(0);
    assert_eq!(
        app.decide_composer_submit(ComposerSubmitChord::Enter),
        ComposerSubmitAction::Submit(SubmitDisposition::Queue)
    );
    assert_eq!(
        app.decide_composer_submit(ComposerSubmitChord::CtrlEnter),
        ComposerSubmitAction::Submit(SubmitDisposition::Steer)
    );

    app.queue_message(QueuedMessage::new("older queued".to_string(), None));
    app.input.clear();
    assert_eq!(
        app.decide_composer_submit(ComposerSubmitChord::Enter),
        ComposerSubmitAction::SendQueuedNow
    );
    assert_eq!(
        app.decide_composer_submit(ComposerSubmitChord::CtrlEnter),
        ComposerSubmitAction::SendQueuedNow
    );

    app.input = "offline follow-up".to_string();
    app.offline_mode = true;
    assert_eq!(
        app.decide_composer_submit(ComposerSubmitChord::CtrlEnter),
        ComposerSubmitAction::Submit(SubmitDisposition::Queue)
    );
}

#[test]
fn bare_enter_scenario() {
    // Scenario consolidation of: bare_enter_while_streaming_queues_then_double_tap_steers, bare_enter_passes_through_when_idle
    // from bare_enter_while_streaming_queues_then_double_tap_steers
    {
        let mut app = App::new(test_options(false), &Config::default());
        // Busy + streaming: the first bare Enter queues and opens the window; a
        // second inside it steers (the same disposition Ctrl+Enter takes); a
        // second after the window lapses is an ordinary queue.
        app.is_loading = true;
        app.streaming_message_index = Some(0);

        let first = app.enter_with_double_tap();
        assert_eq!(first, Some(SubmitDisposition::Queue));
        assert!(app.double_tap_window_open());
        let second = app.enter_with_double_tap();
        assert_eq!(second, Some(SubmitDisposition::Steer));
        assert!(!app.double_tap_window_open(), "a steer closes the window");

        let first = app.enter_with_double_tap();
        assert_eq!(first, Some(SubmitDisposition::Queue));
        app.last_enter_instant =
            Some(std::time::Instant::now() - App::DOUBLE_TAP_WINDOW - Duration::from_millis(1));
        assert!(!app.double_tap_window_open());
        let late = app.enter_with_double_tap();
        assert_eq!(late, Some(SubmitDisposition::Queue));
    }
    // from bare_enter_passes_through_when_idle
    {
        let mut app = App::new(test_options(false), &Config::default());
        // Engine idle → Immediate every time.
        let first = app.enter_with_double_tap();
        assert_eq!(first, Some(SubmitDisposition::Immediate));
        let second = app.enter_with_double_tap();
        assert_eq!(second, Some(SubmitDisposition::Immediate));
    }
}

#[test]
fn double_tap_drains_every_queued_message_oldest_first_inside_the_window() {
    let mut app = App::new(test_options(false), &Config::default());
    app.is_loading = true;
    app.streaming_message_index = Some(0);
    app.queue_message(QueuedMessage::new("older queued".to_string(), None));
    app.queue_message(QueuedMessage::new("just typed follow-up".to_string(), None));
    assert!(
        app.take_queued_for_double_tap_steer().is_empty(),
        "no window armed"
    );
    app.arm_double_tap_window();
    let taken = app.take_queued_for_double_tap_steer();
    assert_eq!(
        taken
            .iter()
            .map(|message| message.display.as_str())
            .collect::<Vec<_>>(),
        vec!["older queued", "just typed follow-up"],
        "the window drains the whole queue in order"
    );
    assert_eq!(app.queued_message_count(), 0);
    assert!(
        app.take_queued_for_double_tap_steer().is_empty(),
        "one steer per tap"
    );
}

#[test]
fn sticky_error_ttl_is_capped_and_clears_on_composer_activity() {
    let mut app = App::new(test_options(false), &Config::default());
    app.set_sticky_status("workflow failed", StatusToastLevel::Error, None);
    let sticky = app.sticky_status.as_ref().expect("sticky error");
    assert_eq!(sticky.ttl_ms, Some(App::STICKY_ERROR_TTL_MS));
    app.insert_char('a');
    assert!(app.sticky_status.is_none());
}

#[test]
fn push_pending_steer_arms_resend_flag() {
    let mut app = App::new(test_options(false), &Config::default());
    assert!(!app.submit_pending_steers_after_interrupt);
    app.push_pending_steer(QueuedMessage::new("steer me".to_string(), None));
    assert_eq!(app.pending_steers.len(), 1);
    assert!(app.submit_pending_steers_after_interrupt);
}

#[test]
fn drain_pending_scenario() {
    // Scenario consolidation of: drain_pending_steers_clears_flag_and_returns_in_order, drain_pending_steers_when_empty_is_safe
    // from drain_pending_steers_clears_flag_and_returns_in_order
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.push_pending_steer(QueuedMessage::new("first".to_string(), None));
        app.push_pending_steer(QueuedMessage::new("second".to_string(), None));
        app.push_pending_steer(QueuedMessage::new("third".to_string(), None));

        let drained = app.drain_pending_steers();
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0].display, "first");
        assert_eq!(drained[2].display, "third");
        assert!(app.pending_steers.is_empty());
        assert!(!app.submit_pending_steers_after_interrupt);
    }
    // from drain_pending_steers_when_empty_is_safe
    {
        let mut app = App::new(test_options(false), &Config::default());
        // Flag-only set (someone armed it manually): drain still clears it.
        app.submit_pending_steers_after_interrupt = true;
        let drained = app.drain_pending_steers();
        assert!(drained.is_empty());
        assert!(!app.submit_pending_steers_after_interrupt);
    }
}

#[test]
fn double_push_pending_steer_is_idempotent_on_flag() {
    let mut app = App::new(test_options(false), &Config::default());
    app.push_pending_steer(QueuedMessage::new("a".to_string(), None));
    app.push_pending_steer(QueuedMessage::new("b".to_string(), None));
    assert!(app.submit_pending_steers_after_interrupt);
    assert_eq!(app.pending_steers.len(), 2);
}

#[test]
fn pop_last_scenario() {
    // Scenario consolidation of: pop_last_queued_into_draft_pops_back_and_arms_draft, pop_last_queued_into_draft_noop_when_composer_dirty, pop_last_queued_into_draft_noop_when_draft_already_armed, pop_last_queued_into_draft_noop_when_queue_empty
    // from pop_last_queued_into_draft_pops_back_and_arms_draft
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.queue_message(QueuedMessage::new(
            "first".to_string(),
            Some("skill-A".to_string()),
        ));
        app.queue_message(QueuedMessage::new(
            "last".to_string(),
            Some("skill-B".to_string()),
        ));

        assert!(app.pop_last_queued_into_draft());
        assert_eq!(app.input, "last");
        assert_eq!(app.cursor_position, "last".chars().count());
        assert_eq!(app.queued_messages.len(), 1);
        let draft = app.queued_draft.clone().expect("draft is set");
        assert_eq!(draft.display, "last");
        assert_eq!(draft.skill_instruction.as_deref(), Some("skill-B"));
    }
    // from pop_last_queued_into_draft_noop_when_composer_dirty
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.queue_message(QueuedMessage::new("queued".to_string(), None));
        app.input = "typing".to_string();
        app.cursor_position = char_count(&app.input);

        assert!(!app.pop_last_queued_into_draft());
        assert_eq!(app.input, "typing");
        assert_eq!(app.queued_messages.len(), 1);
        assert!(app.queued_draft.is_none());
    }
    // from pop_last_queued_into_draft_noop_when_draft_already_armed
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.queue_message(QueuedMessage::new("queued".to_string(), None));
        app.queued_draft = Some(QueuedMessage::new("editing".to_string(), None));

        assert!(!app.pop_last_queued_into_draft());
        assert_eq!(app.queued_messages.len(), 1);
        assert_eq!(
            app.queued_draft.as_ref().map(|d| d.display.as_str()),
            Some("editing")
        );
    }
    // from pop_last_queued_into_draft_noop_when_queue_empty
    {
        let mut app = App::new(test_options(false), &Config::default());
        assert!(!app.pop_last_queued_into_draft());
        assert!(app.input.is_empty());
        assert!(app.queued_draft.is_none());
    }
}

#[test]
fn cancel_queued_draft_edit_restores_original_message() {
    let mut app = App::new(test_options(false), &Config::default());
    app.queue_message(QueuedMessage::new("first".to_string(), None));
    app.queue_message(QueuedMessage::new(
        "original follow-up".to_string(),
        Some("skill".to_string()),
    ));
    assert!(app.pop_last_queued_into_draft());
    app.input = "edited but not submitted".to_string();
    app.cursor_position = char_count(&app.input);

    assert!(app.cancel_queued_draft_edit());

    assert!(app.input.is_empty());
    assert!(app.queued_draft.is_none());
    assert_eq!(app.queued_messages.len(), 2);
    let restored = app.queued_messages.back().expect("restored message");
    assert_eq!(restored.display, "original follow-up");
    assert_eq!(restored.skill_instruction.as_deref(), Some("skill"));
    assert_eq!(
        app.clear_undo_buffer.as_deref(),
        Some("edited but not submitted"),
        "the interrupted edit remains recoverable via normal draft recovery"
    );
}

#[test]
fn finalize_streaming_scenario() {
    // Scenario consolidation of: finalize_streaming_assistant_marks_existing_cell_interrupted, finalize_streaming_assistant_handles_empty_content, finalize_streaming_assistant_no_op_without_index, finalize_streaming_assistant_is_idempotent_on_double_call
    // from finalize_streaming_assistant_marks_existing_cell_interrupted
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.add_message(HistoryCell::Assistant {
            content: "partial reply so far".to_string(),
            streaming: true,
        });
        let idx = app.history.len() - 1;
        app.streaming_message_index = Some(idx);

        app.finalize_streaming_assistant_as_interrupted();

        assert!(app.streaming_message_index.is_none());
        match &app.history[idx] {
            HistoryCell::Assistant { content, streaming } => {
                assert!(content.starts_with("[interrupted]"), "got: {content}");
                assert!(content.contains("partial reply so far"));
                assert!(!*streaming);
            }
            other => panic!("expected Assistant cell, got {other:?}"),
        }
    }
    // from finalize_streaming_assistant_handles_empty_content
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.add_message(HistoryCell::Assistant {
            content: String::new(),
            streaming: true,
        });
        let idx = app.history.len() - 1;
        app.streaming_message_index = Some(idx);

        app.finalize_streaming_assistant_as_interrupted();

        match &app.history[idx] {
            HistoryCell::Assistant { content, streaming } => {
                assert_eq!(content, "[interrupted]");
                assert!(!*streaming);
            }
            other => panic!("expected Assistant cell, got {other:?}"),
        }
    }
    // from finalize_streaming_assistant_no_op_without_index
    {
        let mut app = App::new(test_options(false), &Config::default());
        // No streaming index set; should not panic and should leave history unchanged.
        let prev_len = app.history.len();
        app.finalize_streaming_assistant_as_interrupted();
        assert_eq!(app.history.len(), prev_len);
        assert!(app.streaming_message_index.is_none());
    }
    // from finalize_streaming_assistant_is_idempotent_on_double_call
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.add_message(HistoryCell::Assistant {
            content: "something".to_string(),
            streaming: true,
        });
        let idx = app.history.len() - 1;
        app.streaming_message_index = Some(idx);

        app.finalize_streaming_assistant_as_interrupted();
        // Second call without resetting state must be safe.
        app.finalize_streaming_assistant_as_interrupted();

        match &app.history[idx] {
            HistoryCell::Assistant { content, .. } => {
                // Second call still finds index None — content unchanged from first.
                assert!(content.starts_with("[interrupted] "));
                assert_eq!(content.matches("[interrupted]").count(), 1);
            }
            other => panic!("expected Assistant cell, got {other:?}"),
        }
    }
}

#[test]
fn delete_word_scenario() {
    // Scenario consolidation of: delete_word_backward_removes_previous_word_only, delete_word_backward_handles_trailing_space_and_utf8, delete_word_forward_handles_leading_space_and_utf8
    // from delete_word_backward_removes_previous_word_only
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "hello world".to_string();
        app.cursor_position = char_count(&app.input);

        app.delete_word_backward();

        assert_eq!(app.input, "hello ");
        assert_eq!(app.cursor_position, char_count("hello "));
    }
    // from delete_word_backward_handles_trailing_space_and_utf8
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "cafe 你好   ".to_string();
        app.cursor_position = char_count(&app.input);

        app.delete_word_backward();

        assert_eq!(app.input, "cafe ");
        assert_eq!(app.cursor_position, char_count("cafe "));
    }
    // from delete_word_forward_handles_leading_space_and_utf8
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "hello 你好 world".to_string();
        app.cursor_position = char_count("hello");

        app.delete_word_forward();

        assert_eq!(app.input, "hello world");
        assert_eq!(app.cursor_position, char_count("hello"));
    }
}

#[test]
fn delete_to_start_of_line_respects_multiline_cursor() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "first\nsecond line".to_string();
    app.cursor_position = char_count("first\nsecond");

    app.delete_to_start_of_line();

    assert_eq!(app.input, "first\n line");
    assert_eq!(app.cursor_position, char_count("first\n"));
}

#[test]
fn kill_and_yank_handle_multibyte_utf8() {
    let mut app = App::new(test_options(false), &Config::default());
    // "café 你好" — char_count = 7 (c,a,f,é, ,你,好); UTF-8 bytes differ.
    app.input = "café 你好".to_string();
    app.cursor_position = 5; // before '你'
    assert!(app.kill_to_end_of_line());
    assert_eq!(app.input, "café ");
    assert_eq!(app.cursor_position, 5);
    assert_eq!(app.kill_buffer, "你好");

    // Yank back at the same spot — must not panic on char boundaries.
    assert!(app.yank());
    assert_eq!(app.input, "café 你好");
    assert_eq!(app.cursor_position, 7);
}

#[test]
fn selection_range_scenario() {
    // Scenario consolidation of: selection_range_returns_none_when_no_anchor, selection_range_returns_ordered_range, selection_range_normalizes_order, selection_range_returns_none_when_anchor_equals_cursor
    // from selection_range_returns_none_when_no_anchor
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "hello world".to_string();
        app.cursor_position = 5;
        app.selection_anchor = None;
        assert!(app.selection_range().is_none());
    }
    // from selection_range_returns_ordered_range
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "hello world".to_string();
        app.cursor_position = 5;
        app.selection_anchor = Some(2);
        assert_eq!(app.selection_range(), Some((2, 5)));
    }
    // from selection_range_normalizes_order
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "hello world".to_string();
        app.cursor_position = 2;
        app.selection_anchor = Some(5);
        assert_eq!(app.selection_range(), Some((2, 5)));
    }
    // from selection_range_returns_none_when_anchor_equals_cursor
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "hello".to_string();
        app.cursor_position = 3;
        app.selection_anchor = Some(3);
        assert!(app.selection_range().is_none());
    }
}

#[test]
fn delete_selection_scenario() {
    // Scenario consolidation of: delete_selection_removes_selected_text, delete_selection_noop_when_no_selection, delete_selection_handles_cjk_and_emoji_ranges
    // from delete_selection_removes_selected_text
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "hello world".to_string();
        app.cursor_position = 5;
        app.selection_anchor = Some(2);
        assert!(app.delete_selection());
        assert_eq!(app.input, "he world");
        assert_eq!(app.cursor_position, 2);
        assert!(app.selection_anchor.is_none());
    }
    // from delete_selection_noop_when_no_selection
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "hello".to_string();
        app.cursor_position = 3;
        app.selection_anchor = None;
        assert!(!app.delete_selection());
        assert_eq!(app.input, "hello");
        assert_eq!(app.cursor_position, 3);
    }
    // from delete_selection_handles_cjk_and_emoji_ranges
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "a你👩‍👩‍👧‍👦好b".to_string();
        // Select 你 + family emoji (7 chars) + 好: chars 1..10.
        app.selection_anchor = Some(1);
        app.cursor_position = 10;
        assert_eq!(app.selected_text(), "你👩‍👩‍👧‍👦好");
        assert!(app.delete_selection());
        assert_eq!(app.input, "ab");
        assert_eq!(app.cursor_position, 1);
    }
}

#[test]
fn insert_char_replaces_selection() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "hello world".to_string();
    app.cursor_position = 5;
    app.selection_anchor = Some(2);
    app.insert_char('X');
    assert_eq!(app.input, "heX world");
    assert_eq!(app.cursor_position, 3);
    assert!(app.selection_anchor.is_none());
}

#[test]
fn delete_char_removes_selection_instead_of_single_char() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "hello world".to_string();
    app.cursor_position = 5;
    app.selection_anchor = Some(2);
    app.delete_char();
    assert_eq!(app.input, "he world");
    assert_eq!(app.cursor_position, 2);
}

#[test]
fn selected_text_returns_correct_substring() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "hello world".to_string();
    app.cursor_position = 5;
    app.selection_anchor = Some(2);
    assert_eq!(app.selected_text(), "llo");
}

#[test]
fn insert_str_replaces_selection() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "hello world".to_string();
    app.cursor_position = 5;
    app.selection_anchor = Some(2);
    app.insert_str("yo");
    assert_eq!(app.input, "heyo world");
    assert_eq!(app.cursor_position, 4);
    assert!(app.selection_anchor.is_none());
}

// === Composer real-editor contract (v0.9.1) ====================================

#[test]
fn grapheme_boundaries_snap_around_zwj_emoji_and_flags() {
    // "a👩‍👩‍👧‍👦b" — the family emoji is 7 chars (4 people + 3 ZWJ) but ONE grapheme.
    let text = "a👩‍👩‍👧‍👦b";
    let family_chars = "👩‍👩‍👧‍👦".chars().count();
    assert_eq!(family_chars, 7);
    // Stepping right from after 'a' jumps over the whole family.
    assert_eq!(next_grapheme_boundary(text, 1), 1 + family_chars);
    // Stepping left from before 'b' jumps back to just after 'a'.
    assert_eq!(prev_grapheme_boundary(text, 1 + family_chars), 1);
    // A cursor stranded mid-cluster snaps to the cluster edges.
    assert_eq!(prev_grapheme_boundary(text, 3), 1);
    assert_eq!(next_grapheme_boundary(text, 3), 1 + family_chars);

    // Flag pair: two regional-indicator chars, one grapheme.
    let flag = "🇯🇵";
    assert_eq!(flag.chars().count(), 2);
    assert_eq!(next_grapheme_boundary(flag, 0), 2);
    assert_eq!(prev_grapheme_boundary(flag, 2), 0);
}

#[test]
fn cursor_moves_by_grapheme_over_emoji_and_cjk() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "你👍🏽好".to_string(); // CJK + skin-tone emoji (2 chars) + CJK
    app.cursor_position = 0;
    app.move_cursor_right();
    assert_eq!(app.cursor_position, 1); // after 你
    app.move_cursor_right();
    assert_eq!(app.cursor_position, 3); // after 👍🏽 (base + modifier)
    app.move_cursor_right();
    assert_eq!(app.cursor_position, 4); // after 好
    app.move_cursor_right();
    assert_eq!(app.cursor_position, 4); // clamped at end
    app.move_cursor_left();
    assert_eq!(app.cursor_position, 3);
    app.move_cursor_left();
    assert_eq!(app.cursor_position, 1);
    app.move_cursor_left();
    assert_eq!(app.cursor_position, 0);
    app.move_cursor_left();
    assert_eq!(app.cursor_position, 0); // clamped at start
}

/// U01-m4: vim Normal-mode clamps, `a`, and `j`/`k` column moves land on
/// grapheme-cluster starts, never inside a combining or skin-tone sequence.
#[test]
fn vim_cursor_moves_never_split_a_grapheme_cluster() {
    let mut app = App::new(test_options(false), &Config::default());
    app.vim_enabled = true;
    // "e" + COMBINING ACUTE: two scalars, one cluster.
    app.input = "e\u{301}".to_string();
    app.cursor_position = char_count(&app.input);
    app.vim_enter_normal();
    assert_eq!(
        app.cursor_position, 0,
        "Normal mode sits on the cluster start"
    );
    app.vim_enter_append();
    assert_eq!(
        app.cursor_position, 2,
        "`a` appends after the whole cluster"
    );

    // Scalar column 1 falls inside the other line's skin-tone emoji.
    app.input = "ab\n\u{1f44d}\u{1f3fd}c".to_string();
    app.cursor_position = 1; // on `b`
    app.vim_move_down();
    assert_eq!(app.cursor_position, 3, "`j` lands on the emoji's start");
    app.input = "\u{1f44d}\u{1f3fd}c\nab".to_string();
    app.cursor_position = 5; // on `b`
    app.vim_move_up();
    assert_eq!(app.cursor_position, 0, "`k` lands on the emoji's start");
}

#[test]
fn backspace_removes_whole_emoji_cluster() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "hi👩‍👩‍👧‍👦".to_string();
    app.cursor_position = char_count(&app.input);
    app.delete_char();
    assert_eq!(app.input, "hi");
    assert_eq!(app.cursor_position, 2);
}

#[test]
fn forward_delete_removes_whole_flag_cluster() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "🇯🇵ok".to_string();
    app.cursor_position = 0;
    app.delete_char_forward();
    assert_eq!(app.input, "ok");
    assert_eq!(app.cursor_position, 0);
}

#[test]
fn backspace_deletes_cjk_per_character() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "你好".to_string();
    app.cursor_position = 2;
    app.delete_char();
    assert_eq!(app.input, "你");
    app.delete_char();
    assert_eq!(app.input, "");
}

#[test]
fn vim_x_removes_whole_grapheme_cluster() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "👍🏽a".to_string();
    app.cursor_position = 0;
    app.vim_delete_char_under_cursor();
    assert_eq!(app.input, "a");
    assert_eq!(app.cursor_position, 0);
}

#[test]
fn select_all_scenario() {
    // Scenario consolidation of: select_all_covers_whole_draft, select_all_on_empty_composer_sets_no_anchor, select_all_then_typing_replaces_everything_recoverably, select_all_then_backspace_is_recoverable_with_ctrl_z
    // from select_all_covers_whole_draft
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "hello 你好 🇯🇵".to_string();
        app.cursor_position = 3;
        app.select_all();
        assert_eq!(app.selection_anchor, Some(0));
        assert_eq!(app.cursor_position, char_count(&app.input));
        assert_eq!(app.selected_text(), "hello 你好 🇯🇵");
    }
    // from select_all_on_empty_composer_sets_no_anchor
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.select_all();
        assert!(app.selection_anchor.is_none());
        assert!(app.selection_range().is_none());
    }
    // from select_all_then_typing_replaces_everything_recoverably
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "precious draft".to_string();
        app.select_all();
        app.insert_char('x');
        assert_eq!(app.input, "x");
        assert_eq!(app.cursor_position, 1);
        // The overwritten draft is stashed like Ctrl+U would.
        assert_eq!(app.clear_undo_buffer.as_deref(), Some("precious draft"));
        assert!(app.draft_history.iter().any(|d| d == "precious draft"));
    }
    // from select_all_then_backspace_is_recoverable_with_ctrl_z
    {
        let mut app = App::new(test_options(false), &Config::default());
        app.input = "do not lose me".to_string();
        app.select_all();
        app.delete_char();
        assert_eq!(app.input, "");
        assert!(app.restore_last_cleared_input_if_empty());
        assert_eq!(app.input, "do not lose me");
        assert_eq!(app.cursor_position, char_count(&app.input));
    }
}

#[test]
fn partial_selection_delete_does_not_stash_undo_buffer() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "hello world".to_string();
    app.selection_anchor = Some(0);
    app.cursor_position = 5;
    assert!(app.delete_selection());
    assert_eq!(app.input, " world");
    assert!(app.clear_undo_buffer.is_none());
}

#[test]
fn shift_home_end_style_selection_uses_line_bounds() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "first line\nsecond line".to_string();
    // Cursor in the middle of the second line ("second ".len() == 7).
    app.cursor_position = 11 + 7;
    // Shift+Home: anchor at cursor, move to line start.
    app.selection_anchor = Some(app.cursor_position);
    app.move_cursor_line_start();
    assert_eq!(app.cursor_position, 11);
    assert_eq!(app.selected_text(), "second ");
    // Shift+End from the same anchor: move to line end.
    app.move_cursor_line_end();
    assert_eq!(app.cursor_position, char_count(&app.input));
    assert_eq!(app.selected_text(), "line");
}

#[test]
fn word_selection_extends_by_word_and_replaces_on_type() {
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "alpha beta gamma".to_string();
    app.cursor_position = 0;
    // Ctrl/Alt+Shift+Right twice: anchor once, extend word-wise.
    app.selection_anchor = Some(app.cursor_position);
    app.move_cursor_word_forward();
    app.move_cursor_word_forward();
    assert_eq!(app.selected_text(), "alpha beta ");
    app.insert_char('X');
    assert_eq!(app.input, "Xgamma");
    assert_eq!(app.cursor_position, 1);
}

// === #2574: capability-aware fallback eligibility ===============================

/// Build an `App` whose fallback chain is `[active, fallbacks...]` with each
/// provider's auth controlled via `config.providers` keys. The startup-default
/// settings home is isolated too: an intentional saved default from a previous
/// test or a developer's real profile must not replace the chain primary.
fn app_with_fallback_chain(
    active: ProviderKind,
    fallbacks: &[codewhale_config::ProviderKind],
    keyed: &[ProviderKind],
) -> App {
    let settings_home = tempfile::tempdir().expect("isolated fallback settings home");
    let _home = EnvVarGuard::set("HOME", settings_home.path());
    let _user_profile = EnvVarGuard::set("USERPROFILE", settings_home.path());
    let _codewhale_home =
        EnvVarGuard::set("CODEWHALE_HOME", settings_home.path().join(".codewhale"));
    let _deepseek_config = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
    let _codewhale_config = EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
    let mut providers = ProvidersConfig::default();
    for provider in keyed {
        let entry = ProviderConfig {
            api_key: Some(format!("test-key-{}", provider.as_str())),
            ..Default::default()
        };
        match provider {
            ProviderKind::Deepseek => providers.deepseek = entry,
            ProviderKind::Openai => providers.openai = entry,
            ProviderKind::Openrouter => providers.openrouter = entry,
            ProviderKind::Together => providers.together = entry,
            ProviderKind::Fireworks => providers.fireworks = entry,
            other => panic!("unhandled keyed provider in test helper: {other:?}"),
        }
    }

    let config = {
        let __cw_config_init_21_0 = Some(active.as_str().to_string());
        let __cw_config_init_21_1 = fallbacks.to_vec();
        let __cw_config_init_21_2 = Some(providers);
        let mut __cw_config_init_21_value: Config = Default::default();
        __cw_config_init_21_value.provider = __cw_config_init_21_0;
        __cw_config_init_21_value.fallback_providers = __cw_config_init_21_1;
        __cw_config_init_21_value.providers = __cw_config_init_21_2;
        __cw_config_init_21_value
    };

    let mut options = test_options(false);
    options.start_in_agent_mode = true;
    options.skip_onboarding = true;
    App::new(options, &config)
}

#[test]
fn advance_fallback_skips_unauthed_middle_provider_and_lands_on_next_ready() {
    let _lock = lock_test_env();
    let _openai = EnvVarGuard::remove("OPENAI_API_KEY");
    let _openrouter = EnvVarGuard::remove("OPENROUTER_API_KEY");
    let _together = EnvVarGuard::remove("TOGETHER_API_KEY");

    // Chain: Openai (active, keyed) -> Openrouter (no key) -> Together (keyed).
    let mut app = app_with_fallback_chain(
        ProviderKind::Openai,
        &[
            codewhale_config::ProviderKind::Openrouter,
            codewhale_config::ProviderKind::Together,
        ],
        &[ProviderKind::Openai, ProviderKind::Together],
    );
    assert_eq!(app.fallback_chain_position(), Some(0));

    // Openrouter is skipped (needs auth); we land on Together.
    let next = app.advance_fallback("network error");
    assert_eq!(next, Some(ProviderKind::Together));
    assert_eq!(app.api_provider, ProviderKind::Together);
    assert_eq!(app.fallback_chain_position(), Some(2));

    let reason = app.last_fallback_reason.as_deref().unwrap_or_default();
    assert!(
        reason.contains("Fell back to together"),
        "reason should name the landed provider: {reason}"
    );
    assert!(
        reason.contains("skipped openrouter: needs auth"),
        "reason should note the skipped provider: {reason}"
    );
}

#[test]
fn advance_fallback_scenario() {
    // Scenario consolidation of: advance_fallback_local_provider_is_eligible_without_a_key, advance_fallback_local_primary_may_fall_back_to_local_sibling
    // from advance_fallback_local_provider_is_eligible_without_a_key
    {
        let _lock = lock_test_env();
        let _openai = EnvVarGuard::remove("OPENAI_API_KEY");

        // Chain: Openai (active, keyed) -> Ollama (local, no key needed).
        let mut app = app_with_fallback_chain(
            ProviderKind::Openai,
            &[codewhale_config::ProviderKind::Ollama],
            &[ProviderKind::Openai],
        );

        let next = app.advance_fallback("timeout");
        assert_eq!(
            next,
            Some(ProviderKind::Ollama),
            "self-hosted providers are ready without a key"
        );
        assert_eq!(app.api_provider, ProviderKind::Ollama);
        let reason = app.last_fallback_reason.as_deref().unwrap_or_default();
        assert!(reason.contains("Fell back to ollama"), "{reason}");
        assert!(
            !reason.contains("skipped"),
            "no providers should be skipped: {reason}"
        );
    }
    // from advance_fallback_local_primary_may_fall_back_to_local_sibling
    {
        let _lock = lock_test_env();

        // Local primary (Ollama) -> local sibling (vLLM). Both are self-hosted, so
        // the local/private posture is preserved and the fallback is allowed.
        let mut app = app_with_fallback_chain(
            ProviderKind::Ollama,
            &[codewhale_config::ProviderKind::Vllm],
            &[],
        );

        let next = app.advance_fallback("local runtime unavailable");
        assert_eq!(
            next,
            Some(ProviderKind::Vllm),
            "local->local fallback stays within the private posture"
        );
        assert_eq!(app.api_provider, ProviderKind::Vllm);
        let reason = app.last_fallback_reason.as_deref().unwrap_or_default();
        assert!(reason.contains("Fell back to vllm"), "{reason}");
    }
}

#[test]
fn advance_fallback_all_unready_exhausts_with_clear_reason() {
    let _lock = lock_test_env();
    let _openai = EnvVarGuard::remove("OPENAI_API_KEY");
    let _openrouter = EnvVarGuard::remove("OPENROUTER_API_KEY");
    let _together = EnvVarGuard::remove("TOGETHER_API_KEY");

    // Chain: Openai (active, keyed) -> Openrouter (no key) -> Together (no key).
    // Every fallback entry is unready, so the chain exhausts.
    let mut app = app_with_fallback_chain(
        ProviderKind::Openai,
        &[
            codewhale_config::ProviderKind::Openrouter,
            codewhale_config::ProviderKind::Together,
        ],
        &[ProviderKind::Openai],
    );

    let next = app.advance_fallback("rate limited");
    assert_eq!(next, None, "no ready fallback remains");
    // Active provider is unchanged on exhaustion.
    assert_eq!(app.api_provider, ProviderKind::Openai);

    let reason = app.last_fallback_reason.as_deref().unwrap_or_default();
    assert!(
        reason.contains("Fallback chain exhausted"),
        "reason should state exhaustion: {reason}"
    );
    assert!(
        reason.contains("skipped openrouter: needs auth")
            && reason.contains("skipped together: needs auth"),
        "reason should note every skipped provider: {reason}"
    );
}

#[test]
fn startup_and_fallback_skip_inactive_external_only_routes_without_io() {
    let _lock = lock_test_env();
    let temp = tempfile::tempdir().expect("external fallback fixtures");
    let codex_path = temp.path().join("codex-auth.json");
    let grok_path = temp.path().join("grok-auth.json");
    let codex_raw = "inactive Codex bytes must not be read";
    let grok_raw = "inactive Grok bytes must not be read";
    std::fs::write(&codex_path, codex_raw).expect("write Codex trap");
    std::fs::write(&grok_path, grok_raw).expect("write Grok trap");
    let _home = EnvVarGuard::set("CODEWHALE_HOME", temp.path().join("owned-home"));
    let _codex_path = EnvVarGuard::set("OPENAI_CODEX_AUTH_FILE", &codex_path);
    let _grok_path = EnvVarGuard::set("GROK_AUTH_PATH", &grok_path);
    let _codex_access = EnvVarGuard::remove("OPENAI_CODEX_ACCESS_TOKEN");
    let _legacy_codex_access = EnvVarGuard::remove("CODEX_ACCESS_TOKEN");
    let _xai_key = EnvVarGuard::remove("XAI_API_KEY");
    let _cli_key = EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
    let _cli_source = EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");

    let config = {
        let __cw_config_init_22_0 = Some(ProviderKind::Deepseek.as_str().to_string());
        let __cw_config_init_22_1 = vec![
            codewhale_config::ProviderKind::OpenaiCodex,
            codewhale_config::ProviderKind::Xai,
        ];
        let __cw_config_init_22_2 = Some(ProvidersConfig {
            openai_codex: ProviderConfig {
                auth_mode: Some("oauth".to_string()),
                external_credentials: Some(
                    codewhale_config::ExternalCredentialConsentToml::read_only(
                        codewhale_config::ProviderKind::OpenaiCodex,
                        codewhale_config::ExternalCredentialSource::CodexCli,
                        codex_path.clone(),
                    ),
                ),
                ..Default::default()
            },
            xai: ProviderConfig {
                auth_mode: Some("oauth".to_string()),
                external_credentials: Some(
                    codewhale_config::ExternalCredentialConsentToml::read_only(
                        codewhale_config::ProviderKind::Xai,
                        codewhale_config::ExternalCredentialSource::GrokCli,
                        grok_path.clone(),
                    ),
                ),
                ..Default::default()
            },
            ..Default::default()
        });
        let mut __cw_config_init_22_value: Config = Default::default();
        __cw_config_init_22_value.provider = __cw_config_init_22_0;
        __cw_config_init_22_value.fallback_providers = __cw_config_init_22_1;
        __cw_config_init_22_value.providers = __cw_config_init_22_2;
        __cw_config_init_22_value
    }
    .with_legacy_root(Some("active-deepseek-key".to_string()), None);
    let mut options = test_options(false);
    options.skip_onboarding = true;

    codewhale_runtime::external_credentials::reset_side_effect_trap();
    let mut app = App::new(options, &config);
    assert_eq!(
        codewhale_runtime::external_credentials::side_effect_trap_counts(),
        (0, 0),
        "startup readiness must not inspect inactive external credentials"
    );
    assert_eq!(app.advance_fallback("active route unavailable"), None);
    assert_eq!(
        codewhale_runtime::external_credentials::side_effect_trap_counts(),
        (0, 0),
        "fallback selection must skip external-only inactive routes without inspection"
    );
    let reason = app.last_fallback_reason.as_deref().unwrap_or_default();
    assert!(
        reason.contains("skipped openai-codex: needs auth"),
        "{reason}"
    );
    assert!(reason.contains("skipped xai: needs auth"), "{reason}");
    assert_eq!(
        std::fs::read_to_string(&codex_path).expect("Codex trap unchanged"),
        codex_raw
    );
    assert_eq!(
        std::fs::read_to_string(&grok_path).expect("Grok trap unchanged"),
        grok_raw
    );
}

#[test]
fn advance_fallback_local_primary_does_not_fall_back_to_cloud() {
    let _lock = lock_test_env();
    let _openai = EnvVarGuard::remove("OPENAI_API_KEY");
    let _deepseek = EnvVarGuard::remove("DEEPSEEK_API_KEY");

    // Local primary (Ollama) -> cloud fallback (DeepSeek, fully keyed). The
    // cloud entry is policy-blocked even though it is otherwise ready, so the
    // chain exhausts rather than leaking a local/private route out to cloud.
    let mut app = app_with_fallback_chain(
        ProviderKind::Ollama,
        &[codewhale_config::ProviderKind::Deepseek],
        &[ProviderKind::Deepseek],
    );

    let next = app.advance_fallback("local runtime unavailable");
    assert_eq!(next, None, "local->cloud fallback must be blocked");
    assert_eq!(app.api_provider, ProviderKind::Ollama);

    let reason = app.last_fallback_reason.as_deref().unwrap_or_default();
    assert!(
        reason.contains("local/private policy"),
        "block reason must be visible and specific: {reason}"
    );
    assert!(
        !reason.contains("needs auth"),
        "the block is policy, not missing auth: {reason}"
    );
}

#[test]
fn advance_fallback_cloud_primary_can_hop_cloud_to_local_to_cloud() {
    let _lock = lock_test_env();
    let _openai = EnvVarGuard::remove("OPENAI_API_KEY");
    let _deepseek = EnvVarGuard::remove("DEEPSEEK_API_KEY");

    // The local/private guard is origin-based. A cloud primary may route to a
    // local fallback and then to another cloud fallback if the cloud candidate
    // is otherwise ready; only local/private primaries are blocked from leaking
    // out to cloud.
    let mut app = app_with_fallback_chain(
        ProviderKind::Openai,
        &[
            codewhale_config::ProviderKind::Ollama,
            codewhale_config::ProviderKind::Deepseek,
        ],
        &[ProviderKind::Openai, ProviderKind::Deepseek],
    );

    let local = app.advance_fallback("cloud provider timed out");
    assert_eq!(local, Some(ProviderKind::Ollama));
    assert_eq!(app.api_provider, ProviderKind::Ollama);

    let cloud = app.advance_fallback("local runtime unavailable");
    assert_eq!(cloud, Some(ProviderKind::Deepseek));
    assert_eq!(app.api_provider, ProviderKind::Deepseek);

    let reason = app.last_fallback_reason.as_deref().unwrap_or_default();
    assert!(reason.contains("Fell back to deepseek"), "{reason}");
    assert!(
        !reason.contains("local/private policy"),
        "cloud-primary chains should not trigger local/private blocking: {reason}"
    );
}

#[test]
fn status_classifier_does_not_paint_negated_success_green() {
    use super::StatusToastLevel;
    // Failures that happen to contain a success keyword ("saved", "found")
    // must not toast green (#3757 UX review).
    let (level, _, _) = App::classify_status_text("Custom provider was not saved.");
    assert_ne!(level, StatusToastLevel::Success);
    let (level, _, _) = App::classify_status_text("Queued message not found");
    assert_ne!(level, StatusToastLevel::Success);
    let (level, _, _) = App::classify_status_text("Could not enable subagents");
    assert_ne!(level, StatusToastLevel::Success);
    let (level, _, _) = App::classify_status_text("No sessions found");
    assert_ne!(level, StatusToastLevel::Success);

    // Genuine successes still classify green.
    let (level, _, _) = App::classify_status_text("Team profile saved: reviewer.toml");
    assert_eq!(level, StatusToastLevel::Success);

    // Both cancel spellings classify as Warning.
    let (level, _, _) = App::classify_status_text("Turn canceled");
    assert_eq!(level, StatusToastLevel::Warning);
    let (level, _, _) = App::classify_status_text("Turn cancelled");
    assert_eq!(level, StatusToastLevel::Warning);
}

#[test]
fn onboarding_provider_copy_is_provider_neutral_in_en() {
    use codewhale_localization::{Locale, MessageId, tr};

    let title = tr(Locale::En, MessageId::OnboardProviderTitle);
    let blurb = tr(Locale::En, MessageId::OnboardProviderBlurb);
    assert!(!title.to_ascii_lowercase().contains("deepseek"), "{title}");
    assert!(!blurb.to_ascii_lowercase().contains("deepseek"), "{blurb}");
    let choose = tr(Locale::En, MessageId::OnboardProviderChoose);
    assert!(
        !choose.to_ascii_lowercase().contains("deepseek"),
        "{choose}"
    );
}

#[test]
fn agent_current_activity_bounds_redacts_and_strips_control_sequences() {
    let secret = "sk-activity-secret-1234567890";
    let raw = format!(
        "\u{1b}[31mrunning\u{1b}[0m\napi_key={secret}\n\u{1b}]8;;https://example.invalid\u{7}details\u{1b}]8;;\u{7}\u{1}"
    );
    let activity = AgentCurrentActivity::bounded(
        AgentCurrentActivityStatus::Running,
        Some(raw.clone()),
        Some(format!("\u{1b}[33mFile.read\u{1b}[0m {secret}")),
        Some(4),
    );

    let detail = activity.detail.expect("bounded detail");
    let tool = activity.current_tool.expect("bounded tool");
    assert!(detail.contains("running"), "{detail:?}");
    assert!(detail.contains("api_key=[redacted]"), "{detail:?}");
    assert!(detail.contains("details"), "{detail:?}");
    assert!(tool.contains("File.read"), "{tool:?}");
    assert!(tool.contains("[redacted]"), "{tool:?}");
    for safe in [&detail, &tool] {
        assert!(!safe.contains(secret), "{safe:?}");
        assert!(!safe.contains('\u{1b}'), "{safe:?}");
        assert!(!safe.contains('\u{1}'), "{safe:?}");
        assert!(!safe.contains("example.invalid"), "{safe:?}");
    }
    assert_eq!(activity.step, Some(4));
    assert_eq!(
        raw.matches(secret).count(),
        1,
        "source text stays untouched"
    );
}

// ---------------------------------------------------------------------------
// Startup-default persistence (mode + thinking)
// ---------------------------------------------------------------------------
//
// Before this lane, `settings.default_mode` was written in exactly two places
// — a setup-preset apply and `/config` — so interactive mode cycling never
// persisted and Operate silently reverted to Act on restart. Reasoning effort
// persisted, but only through the model/effort picker, so Ctrl+T and the
// hotbar `reasoning.cycle` action were equally lossy.

/// Seal `HOME`/`CODEWHALE_HOME` onto a temp dir so these tests can assert the
/// real write/reload round trip without touching the developer's settings.
fn sealed_settings_home(tmp: &std::path::Path) -> Vec<EnvVarGuard> {
    vec![
        EnvVarGuard::set("HOME", tmp),
        EnvVarGuard::set("USERPROFILE", tmp),
        EnvVarGuard::set("CODEWHALE_HOME", tmp.join(".codewhale")),
        EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH"),
        EnvVarGuard::remove("CODEWHALE_CONFIG_PATH"),
    ]
}

#[test]
fn interactive_mode_cycle_persists_the_startup_default() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut app = App::new(test_options(false), &Config::default());
    app.mode = AppMode::Agent;
    app.cycle_mode();

    assert_eq!(
        app.mode,
        AppMode::Operate,
        "Act -> Operate is the Tab cycle"
    );
    let reloaded = Settings::load().expect("reload settings");
    assert_eq!(
        reloaded.default_mode, "operate",
        "the mode the user cycled into must be the startup default"
    );
    assert_eq!(
        AppMode::from_setting(&reloaded.default_mode),
        AppMode::Operate,
        "a restart must restore the last user choice"
    );
    assert!(
        app.startup_defaults.drain_failures().is_empty(),
        "a successful write must not report a failure"
    );
}

#[test]
fn explicit_mode_selection_and_hotbar_share_the_persistence_owner() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut app = App::new(test_options(false), &Config::default());
    assert_eq!(app.select_mode(AppMode::Plan), SettingSelection::Changed);
    assert_eq!(Settings::load().expect("reload").default_mode, "plan");

    // The legacy YOLO entry point installs Act, so that is what must persist —
    // "yolo" is a permission alias, never a startup mode.
    assert_eq!(app.select_yolo_compat(), SettingSelection::Changed);
    assert_eq!(Settings::load().expect("reload").default_mode, "agent");
}

#[test]
fn session_restore_and_effective_turn_paths_do_not_rewrite_the_startup_default() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut app = App::new(test_options(false), &Config::default());
    app.select_mode(AppMode::Plan);
    assert_eq!(Settings::load().expect("reload").default_mode, "plan");

    // `set_mode` is the session-only primitive used by session restore and
    // preset application. It must move the live session without claiming the
    // user picked a new startup default.
    assert!(app.set_mode(AppMode::Operate));
    assert_eq!(app.mode, AppMode::Operate);
    assert_eq!(
        Settings::load().expect("reload").default_mode,
        "plan",
        "restoring a session must not rewrite the startup default"
    );
}

#[test]
fn reselecting_restored_live_mode_updates_the_startup_default() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    Settings::transact(|settings| {
        settings.default_mode = "agent".to_string();
        Ok(())
    })
    .expect("seed startup default");
    let mut app = App::new(test_options(false), &Config::default());
    assert!(app.set_mode(AppMode::Operate), "simulate session restore");
    assert_eq!(Settings::load().expect("reload").default_mode, "agent");

    assert_eq!(
        app.select_mode(AppMode::Operate),
        SettingSelection::PersistedSame,
        "an accepted selection that did not move live mode is not a refusal"
    );
    assert_eq!(
        Settings::load().expect("reload").default_mode,
        "operate",
        "the explicit same-live selection must still become the startup default"
    );
}

#[test]
fn mode_change_while_a_turn_runs_is_live_and_persisted() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut app = App::new(test_options(false), &Config::default());
    app.select_mode(AppMode::Plan);
    app.is_loading = true;
    app.cycle_mode();
    app.startup_defaults.flush();

    assert_eq!(app.mode, AppMode::Plan.next());
    assert_eq!(
        Settings::load().expect("reload").default_mode,
        app.mode.as_setting(),
        "a mid-turn change is a real selection"
    );
}

#[test]
fn reasoning_cycle_persists_through_the_same_owner_as_the_picker() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut app = App::new(test_options(false), &Config::default());
    app.api_provider = ProviderKind::Deepseek;
    app.auto_model = false;
    app.reasoning_effort = ReasoningEffort::Off;

    // Ctrl+T and the hotbar `reasoning.cycle` action both land in
    // `apply_reasoning_effort_cycle`.
    app.apply_reasoning_effort_cycle();

    // One step up DeepSeek's ladder from Off is Low, not the old shortcut's High.
    assert_eq!(app.reasoning_effort, ReasoningEffort::Low);
    assert_eq!(
        Settings::load()
            .expect("reload settings")
            .reasoning_effort
            .as_deref(),
        Some("low"),
        "a restart must restore the last thinking choice"
    );
}

#[test]
fn failed_startup_default_write_is_reported_not_swallowed() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // A regular file where the home directory must be: every settings write
    // below it fails.
    let blocked_home = tmp.path().join("codewhale-home-file");
    std::fs::write(&blocked_home, "not a directory").expect("blocking file");
    let _home = EnvVarGuard::set("HOME", tmp.path());
    let _user_profile = EnvVarGuard::set("USERPROFILE", tmp.path());
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &blocked_home);
    let _deepseek_config = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
    let _codewhale_config = EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut app = App::new(test_options(false), &Config::default());
    assert_eq!(
        app.select_mode(AppMode::Plan),
        SettingSelection::Changed,
        "the live session still changes; only the durable write fails"
    );
    assert_eq!(app.mode, AppMode::Plan);

    app.drain_startup_default_failures();
    let toast = app
        .status_toasts
        .iter()
        .find(|toast| toast.text.contains("startup mode"))
        .expect("a failed startup-default write must surface a toast");
    assert!(
        toast.text.contains("was not saved"),
        "toast must say the write did not land, got {:?}",
        toast.text
    );
    assert!(
        !toast.text.contains(".codewhale"),
        "a failure toast must not carry the settings path, got {:?}",
        toast.text
    );
}

// ---------------------------------------------------------------------------
// Startup-default write ordering
// ---------------------------------------------------------------------------
//
// Each write is a load / modify / save transaction over one `settings.toml`.
// These tests run on a real multi-threaded runtime so the writes actually go
// through `spawn_blocking`, and assert the outcome is decided by the order the
// user acted in — not by which blocking task the scheduler happened to pick.
// `StartupDefaultsWriter::flush` is the determinism hook: it blocks until the
// queue is empty and no transaction is in flight.

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rapid_mode_selections_persist_the_last_one_not_the_last_to_finish() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut app = App::new(test_options(false), &Config::default());
    // Faster than a human can Tab, and deliberately revisiting modes so a
    // reordered transaction would land on a value that is also "plausible".
    for mode in [
        AppMode::Plan,
        AppMode::Operate,
        AppMode::Agent,
        AppMode::Plan,
        AppMode::Operate,
        AppMode::Agent,
        AppMode::Plan,
    ] {
        app.select_mode(mode);
    }
    app.startup_defaults.flush();

    assert_eq!(app.mode, AppMode::Plan);
    assert_eq!(
        Settings::load().expect("reload").default_mode,
        "plan",
        "the last selection must win, whatever order the writers ran in"
    );
    assert!(
        app.startup_defaults.drain_failures().is_empty(),
        "no write in the burst may fail"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rapid_thinking_selections_persist_the_last_one() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut app = App::new(test_options(false), &Config::default());
    app.api_provider = ProviderKind::Deepseek;
    app.auto_model = false;
    app.reasoning_effort = ReasoningEffort::Off;

    for _ in 0..6 {
        app.apply_reasoning_effort_cycle();
    }
    app.startup_defaults.flush();

    let expected = app.reasoning_effort.as_setting_for_route(
        app.api_provider,
        &app.active_route_base_url,
        &app.model,
    );
    assert_eq!(
        Settings::load()
            .expect("reload")
            .reasoning_effort
            .as_deref(),
        Some(expected),
        "the tier the session ended on must be the tier on disk"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fixed_route_thinking_cycle_persists_raw_preference() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut app = App::new(test_options(false), &Config::default());
    app.set_provider_identity_record(
        codewhale_runtime::config::Config::default()
            .resolve_provider_identity(ProviderKind::Moonshot.as_str())
            .expect("captured fixture provider"),
    );
    app.auto_model = false;
    app.active_route_base_url =
        codewhale_config::descriptors::defaults::DEFAULT_MOONSHOT_BASE_URL.to_string();
    app.model = codewhale_config::catalog::reviewed::constants::MOONSHOT_KIMI_K3_MODEL.to_string();
    app.reasoning_effort = ReasoningEffort::Max;

    app.apply_reasoning_effort_cycle();
    app.startup_defaults.flush();

    // Cycling off the top of K3's ladder wraps to Auto rather than Off now
    // that the cycle walks `picker_efforts_for_route`. What this test is
    // about is unchanged: whatever tier the cycle lands on is the raw
    // preference that has to survive a restart, not whatever the route
    // executes.
    assert_eq!(app.reasoning_effort, ReasoningEffort::Auto);
    assert_eq!(app.reasoning_effort_preference, Some(ReasoningEffort::Auto));
    assert_eq!(
        Settings::load()
            .expect("reload")
            .reasoning_effort
            .as_deref(),
        Some("auto"),
        "the raw preference the cycle landed on must survive restart"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_mode_and_thinking_writes_finish_before_a_synchronous_selection() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();
    let mut app = App::new(test_options(false), &Config::default());
    app.api_provider = ProviderKind::Deepseek;
    app.auto_model = false;
    app.reasoning_effort = ReasoningEffort::Off;
    assert_eq!(app.select_mode(AppMode::Plan), SettingSelection::Changed);
    app.apply_reasoning_effort_cycle();

    // A newer synchronous effort selection drains the older queued mode and
    // effort first, so a late background writer cannot restore the old effort.
    app.startup_defaults
        .apply_blocking(crate::tui::startup_defaults::StartupDefaults::reasoning_effort("high"))
        .expect("effort write must land");
    let saved = Settings::load_persisted().expect("reload");
    assert_eq!(saved.default_mode, "plan");
    assert_eq!(saved.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(app.select_mode(AppMode::Operate), SettingSelection::Changed);
    app.startup_defaults.flush();
    let saved = Settings::load_persisted().expect("reload");
    assert_eq!(saved.default_mode, "operate");
    assert_eq!(saved.reasoning_effort.as_deref(), Some("high"));
    assert!(app.startup_defaults.drain_failures().is_empty());
}

// ---------------------------------------------------------------------------
// Startup defaults vs. the *other* settings writers
// ---------------------------------------------------------------------------
//
// `StartupDefaultsWriter` only serializes the transactions it owns. The tests
// above prove that much. What follows is the boundary the writer cannot provide
// on its own: `settings.toml` has direct writers in the same process — most
// sharply the Shift+Tab permission posture on the same event loop — and each of
// them loads the whole file, changes some fields, and writes the whole file
// back. Two such writers that do not share a load/modify/save lock each write
// back the other's pre-image, and whichever saves last silently reverts the
// other's field. That boundary now lives in `Settings::transact`.

/// Seal the settings file onto `tmp` via the config-path override, and hand back
/// the root config path the posture writers need. Caller must already hold
/// `lock_test_env()`.
fn sealed_settings_with_root_config(
    tmp: &std::path::Path,
) -> (std::path::PathBuf, Vec<EnvVarGuard>) {
    let config_path = tmp.join("config.toml");
    let guards = vec![
        EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path),
        EnvVarGuard::remove("CODEWHALE_CONFIG_PATH"),
        EnvVarGuard::remove("DEEPSEEK_APPROVAL_POLICY"),
    ];
    (config_path, guards)
}

/// Tab (queued mode write) and Shift+Tab (synchronous posture write) hit the
/// same file through different writers. Neither may lose the other's field.
///
/// This is the concrete pair from the v0.9.1 report: mode cycling spawns a
/// background `default_mode` transaction, the very next keystroke persists
/// `permission_posture` inline, and before `Settings::transact` the two loaded
/// the same bytes — so the later save reverted whichever field the earlier one
/// had just written.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mode_and_permission_posture_writes_do_not_clobber_each_other() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (config_path, _env) = sealed_settings_with_root_config(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut options = test_options(false);
    options.start_in_agent_mode = true;
    options.config_path = Some(config_path);
    let mut app = App::new(options, &Config::default());
    app.approval_mode = ApprovalMode::Suggest;
    app.mode = AppMode::Agent;

    // Alternate the two writers faster than a human can press keys. Plan is
    // skipped because it refuses permission changes by design (#3386), so every
    // iteration below genuinely performs both writes.
    for next_mode in [
        AppMode::Operate,
        AppMode::Agent,
        AppMode::Operate,
        AppMode::Agent,
        AppMode::Operate,
    ] {
        assert_eq!(
            app.select_mode(next_mode),
            SettingSelection::Changed,
            "mode selection must change mode"
        );
        assert!(
            app.cycle_approval_posture(),
            "the posture write must succeed, or the assertion below is vacuous"
        );
    }
    app.startup_defaults.flush();

    let expected_posture = App::approval_posture_setting(app.mode_prefs.agent_approval_mode);
    let saved = Settings::load_persisted().expect("reload settings");
    assert_eq!(
        saved.default_mode, "operate",
        "the posture writer must not revert the mode the user cycled into"
    );
    assert_eq!(
        saved.permission_posture.as_deref(),
        Some(expected_posture),
        "the mode writer must not revert the posture the user cycled into"
    );
    assert!(
        app.startup_defaults.drain_failures().is_empty(),
        "no write in the burst may fail"
    );
}

/// The same boundary for the thinking write against an unrelated direct writer.
///
/// `Settings::transact` here stands in for every load/modify/save site that is
/// not the startup-defaults writer — `/set --save`, the sidebar and work-surface
/// size persists, the preset apply, the pin reorder. They all share one lock now,
/// so a queued thinking write and an unrelated key cannot revert each other.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thinking_and_an_unrelated_direct_setting_write_do_not_clobber_each_other() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut app = App::new(test_options(false), &Config::default());
    app.api_provider = ProviderKind::Deepseek;
    app.auto_model = false;
    app.reasoning_effort = ReasoningEffort::Off;

    for index in 0..6 {
        app.apply_reasoning_effort_cycle();
        // Interleaved on the same thread, exactly as the event loop would when a
        // `/set --save` or a divider drag lands between two Ctrl+T presses.
        Settings::transact(|settings| settings.set("max_history", &(100 + index).to_string()))
            .expect("the direct write must land");
    }
    app.startup_defaults.flush();

    let expected_effort = app.reasoning_effort.as_setting_for_route(
        app.api_provider,
        &app.active_route_base_url,
        &app.model,
    );
    let saved = Settings::load_persisted().expect("reload settings");
    assert_eq!(
        saved.reasoning_effort.as_deref(),
        Some(expected_effort),
        "the direct writer must not revert the thinking level"
    );
    assert_eq!(
        saved.max_input_history, 105,
        "the thinking writer must not revert the last direct write"
    );
    assert!(app.startup_defaults.drain_failures().is_empty());
}

/// Last write wins across *both* kinds of writer, and only for its own field.
///
/// The startup-default writer decides ordering among its own queued
/// transactions; `Settings::transact` decides atomicity against everything else.
/// Together the final file must be the last value the user chose for every field
/// they touched — not a mixture that depends on which blocking task the
/// scheduler picked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rapid_mixed_writes_settle_on_the_last_value_for_every_field() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (config_path, _env) = sealed_settings_with_root_config(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut options = test_options(false);
    options.start_in_agent_mode = true;
    options.config_path = Some(config_path);
    let mut app = App::new(options, &Config::default());
    app.api_provider = ProviderKind::Deepseek;
    app.auto_model = false;
    app.reasoning_effort = ReasoningEffort::Off;
    app.approval_mode = ApprovalMode::Suggest;
    app.mode = AppMode::Agent;

    for index in 0..5 {
        // Queued (background) writers.
        assert_eq!(
            app.select_mode(if index % 2 == 0 {
                AppMode::Operate
            } else {
                AppMode::Agent
            }),
            SettingSelection::Changed
        );
        app.apply_reasoning_effort_cycle();
        // Synchronous direct writers.
        assert!(app.cycle_approval_posture());
        Settings::transact(|settings| settings.set("max_history", &(200 + index).to_string()))
            .expect("the direct write must land");
    }
    // A synchronous mode selection must land after every queued writer.
    app.startup_defaults
        .apply_blocking(crate::tui::startup_defaults::StartupDefaults::mode(
            app.mode,
        ))
        .expect("mode write must land");
    app.startup_defaults.flush();

    let expected_effort = app.reasoning_effort.as_setting_for_route(
        app.api_provider,
        &app.active_route_base_url,
        &app.model,
    );
    let expected_posture = App::approval_posture_setting(app.mode_prefs.agent_approval_mode);
    let saved = Settings::load_persisted().expect("reload settings");
    assert_eq!(saved.default_mode, app.mode.as_setting());
    assert_eq!(saved.reasoning_effort.as_deref(), Some(expected_effort));
    assert_eq!(saved.permission_posture.as_deref(), Some(expected_posture));
    assert_eq!(saved.max_input_history, 204);
    assert!(app.startup_defaults.drain_failures().is_empty());
}

/// A test that never sealed its environment must not be able to write, and must
/// not pay for another test's sealed scope.
///
/// Almost every `App` test cycles modes without sealing `HOME`. Those calls have
/// to be inert: not "usually inert because no other test happens to have opted
/// in", but inert by construction, because the alternative is rewriting the
/// developer's real `~/.codewhale/settings.toml` during `cargo test`.
#[test]
fn mode_cycling_in_an_unsealed_test_writes_nothing() {
    let mut app = App::new(test_options(false), &Config::default());
    app.mode = AppMode::Agent;
    assert_eq!(
        app.select_mode(AppMode::Operate),
        SettingSelection::Changed,
        "the live session must still change"
    );
    assert_eq!(app.mode, AppMode::Operate);
    assert_eq!(
        app.startup_defaults.pending_len(),
        0,
        "an unsealed test must enqueue nothing a later sealed drain could inherit"
    );
    assert!(
        app.startup_defaults.drain_failures().is_empty(),
        "a skipped test write is not a user-visible failure"
    );
}

// ---------------------------------------------------------------------------
// The live-route turn lock reaches the slash surfaces (#2982)
// ---------------------------------------------------------------------------
//
// The lock used to live only in the selectors — Tab, Ctrl+T, the pickers, the
// hotbar. `/set` and `/config <key> <value>` reached the same live route through
// a different door, and both are reachable mid-turn: the composer accepts
// Shift+Enter and the slash menu while `is_loading`. So during a running turn a
// slash command could swap the model, thinking level, or provider out from
// under the engine *and* persist it. The refusal now sits in one place, above
// every disk write and every `App` mutation.

/// Every live-route key and alias, exercised through the same entry point the
/// slash commands use. Live state, persisted state, the startup-default queue,
/// and setup progress must all be exactly where they started.
#[test]
fn slash_config_and_set_refuse_every_live_route_key_while_a_turn_runs() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    Settings::transact(|settings| {
        settings.default_mode = "plan".to_string();
        settings.default_model = Some("deepseek-chat".to_string());
        settings.reasoning_effort = Some("off".to_string());
        Ok(())
    })
    .expect("seed the persisted route");
    let before = Settings::load_persisted().expect("read the seeded settings");

    let mut app = App::new(test_options(false), &Config::default());
    app.api_provider = ProviderKind::Deepseek;
    app.auto_model = false;
    app.set_model_selection("deepseek-chat".to_string());
    app.reasoning_effort = ReasoningEffort::Off;
    let _ = app.set_mode(AppMode::Plan);
    app.is_loading = true;

    let live_mode = app.mode;
    let live_model = app.model.clone();
    let live_effort = app.reasoning_effort;
    let live_provider = app.api_provider;

    // Both `--save` and session-only forms: the refusal is above the branch
    // that decides whether to persist, so neither may get through.
    for persist in [true, false] {
        for (key, value) in [
            ("model", "deepseek-v4-pro"),
            ("default_model", "deepseek-v4-pro"),
            ("reasoning_effort", "high"),
            ("effort", "high"),
            ("provider", "openai"),
        ] {
            let result = crate::commands::set_config_value(&mut app, key, value, persist);
            assert!(
                result.is_error,
                "/set {key} {value} (persist={persist}) must be refused mid-turn"
            );
            let message = result.message.unwrap_or_default();
            assert!(
                message.contains("locked while a turn is running"),
                "the refusal must say why, got {message:?}"
            );
        }
    }

    assert_eq!(app.mode, live_mode, "live mode must not move");
    assert_eq!(app.model, live_model, "the live route model must not move");
    assert_eq!(
        app.reasoning_effort, live_effort,
        "the live thinking tier must not move"
    );
    assert_eq!(
        app.api_provider, live_provider,
        "the live provider must not move"
    );

    let after = Settings::load_persisted().expect("reload settings");
    assert_eq!(after.default_mode, before.default_mode);
    assert_eq!(after.default_model, before.default_model);
    assert_eq!(after.reasoning_effort, before.reasoning_effort);
    assert_eq!(after.provider_models, before.provider_models);

    assert_eq!(
        app.startup_defaults.pending_len(),
        0,
        "a refused command must not queue a startup-default write"
    );
    app.startup_defaults.flush();
    assert!(
        app.startup_defaults.drain_failures().is_empty(),
        "a refusal is not a write failure"
    );
    assert_eq!(
        Settings::load_persisted()
            .expect("reload after flush")
            .default_mode,
        before.default_mode,
        "nothing may land after the queue is drained either"
    );
    assert!(
        !codewhale_config::SetupState::path()
            .expect("setup state path")
            .exists(),
        "a refused route change must not record provider/model setup progress"
    );
}

/// `default_mode` is a restart default that `set_config_value` deliberately does
/// not apply to the live session, so the turn lock must leave it alone. Locking
/// it would refuse a key that cannot affect the running turn.
#[test]
fn restart_only_default_mode_is_still_settable_while_a_turn_runs() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut app = App::new(test_options(false), &Config::default());
    let _ = app.set_mode(AppMode::Plan);
    app.is_loading = true;

    let result = crate::commands::set_config_value(&mut app, "default_mode", "operate", true);
    assert!(
        !result.is_error,
        "default_mode is restart-only, got {:?}",
        result.message
    );
    assert_eq!(
        Settings::load_persisted().expect("reload").default_mode,
        "operate"
    );
    assert_eq!(
        app.mode,
        AppMode::Plan,
        "a restart default must not move the live session"
    );
}

// ---------------------------------------------------------------------------
// Shutdown
// ---------------------------------------------------------------------------

/// The last thing a user does before quitting is very often the selection they
/// most want to keep. Those writes are queued off the event loop on purpose, so
/// without an explicit join at shutdown the process can exit with the newest
/// selection still sitting in the queue.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_flushes_the_last_selection_and_returns_late_failures() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut app = App::new(test_options(false), &Config::default());
    // Deliberately *not* flushed and never drained by an event-loop iteration:
    // this is the "Tab, then immediately quit" shape.
    assert_eq!(app.select_mode(AppMode::Operate), SettingSelection::Changed);

    let failures = app.startup_defaults.shutdown();
    assert!(failures.is_empty(), "the write must land, not fail");
    assert_eq!(
        Settings::load_persisted().expect("reload").default_mode,
        "operate",
        "the last immediate selection must be on disk after shutdown"
    );
}

/// A write that fails after the final redraw cannot be toasted — the toast
/// surface will never be painted again. `shutdown` therefore *returns* the
/// failures so the caller can print them on the restored terminal, and the
/// message it produces is localized and path-free.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_startup_default_failure_is_returned_not_only_logged() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // A regular file where the home directory must be: every settings write
    // below it fails.
    let blocked_home = tmp.path().join("codewhale-home-file");
    std::fs::write(&blocked_home, "not a directory").expect("blocking file");
    let _home = EnvVarGuard::set("HOME", tmp.path());
    let _user_profile = EnvVarGuard::set("USERPROFILE", tmp.path());
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &blocked_home);
    let _deepseek_config = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
    let _codewhale_config = EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut app = App::new(test_options(false), &Config::default());
    assert_eq!(app.select_mode(AppMode::Operate), SettingSelection::Changed);

    let failures = app.startup_defaults.shutdown();
    let failure = failures
        .first()
        .expect("a failed write must be reported at shutdown, not swallowed");
    assert_eq!(
        failure.subjects,
        vec![crate::tui::startup_defaults::StartupDefaultSubject::Mode]
    );

    let message = app.startup_default_failure_message(failure);
    assert!(
        message.contains("startup mode") && message.contains("was not saved"),
        "the shutdown notice must name what was lost, got {message:?}"
    );
    assert!(
        !message.contains(".codewhale") && !message.contains(tmp.path().to_str().unwrap()),
        "the shutdown notice must not print the settings path, got {message:?}"
    );
}

// ---------------------------------------------------------------------------
// Selector truth: refusal, live change, and persisted-same are three outcomes
// ---------------------------------------------------------------------------
//
// `select_mode` used to return a bool. A refusal and an accepted same-live
// selection both came back `false`, so `/mode`, the Alt+A/P/Y shortcuts, and the
// hotbar mode rows all reported "Already in X mode." for both — including for
// the case that had just rewritten the startup default.

/// The three outcomes are distinguishable, and only a live change is a live
/// change.
#[test]
fn mode_selection_reports_refusal_change_and_persisted_same_distinctly() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut app = App::new(test_options(false), &Config::default());
    let _ = app.set_mode(AppMode::Agent);

    assert_eq!(app.select_mode(AppMode::Operate), SettingSelection::Changed);
    assert!(SettingSelection::Changed.changed_live_state());
    assert!(SettingSelection::Changed.accepted());

    assert_eq!(
        app.select_mode(AppMode::Operate),
        SettingSelection::PersistedSame
    );
    assert!(
        !SettingSelection::PersistedSame.changed_live_state(),
        "a persisted-same selection must not resync the engine"
    );
    assert!(
        SettingSelection::PersistedSame.accepted(),
        "a persisted-same selection did write the startup default"
    );

    app.is_loading = true;
    assert_eq!(app.select_mode(AppMode::Plan), SettingSelection::Changed);
    assert!(!SettingSelection::Refused.accepted());
    assert_eq!(app.mode, AppMode::Plan, "mode is not turn-locked");
}

/// Every accepted same-live selection shows a saved receipt, and a mid-turn
/// switch reads as a switch — the two must not read the same.
#[test]
fn slash_mode_distinguishes_a_saved_startup_default_from_a_switch() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut app = App::new(test_options(false), &Config::default());
    let _ = app.set_mode(AppMode::Operate);
    Settings::transact(|settings| {
        settings.default_mode = "agent".to_string();
        Ok(())
    })
    .expect("seed a startup default that disagrees with the live mode");

    // Same live mode, different startup default: `/mode operate` is a real save.
    let receipt = crate::commands::switch_mode(&mut app, AppMode::Operate);
    assert!(
        receipt.contains("saved as startup default"),
        "the save must be reported, got {receipt:?}"
    );
    app.startup_defaults.flush();
    assert_eq!(
        Settings::load_persisted().expect("reload").default_mode,
        "operate"
    );

    app.is_loading = true;
    let switched = crate::commands::switch_mode(&mut app, AppMode::Plan);
    assert!(
        switched.starts_with("Switched to"),
        "mode is not turn-locked, got {switched:?}"
    );
    assert_ne!(switched, receipt);
}

/// The hotbar mode rows share the receipt: dispatching a row for the live mode
/// is `Handled` (no engine resync) but still tells the user it saved.
#[test]
fn hotbar_mode_row_for_the_live_mode_still_shows_the_saved_receipt() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

    let mut app = App::new(test_options(false), &Config::default());
    let _ = app.set_mode(AppMode::Plan);
    let outcome = app.select_mode(AppMode::Plan);
    app.report_mode_selection(AppMode::Plan, outcome);

    assert_eq!(outcome, SettingSelection::PersistedSame);
    assert!(
        app.status_message
            .as_deref()
            .is_some_and(|message| message.contains("saved as startup default")),
        "got {:?}",
        app.status_message
    );
    app.startup_defaults.flush();
    assert_eq!(
        Settings::load_persisted().expect("reload").default_mode,
        "plan"
    );
}

/// v0.9.1 kimi-k3 dogfood report: `settings.toml`'s `[provider_models]` is a memory of the last
/// `/model` pick, so it must not override a model the user named for *this*
/// launch. A dogfood user ran `codewhale --provider moonshot --model kimi-k3`
/// and the session header kept showing the remembered `kimi-k2.7-code` while
/// `doctor` reported `kimi-k3`; header and route have to agree.
#[test]
fn an_explicit_launch_model_outranks_the_remembered_provider_model() {
    let _lock = lock_test_env();
    let temp = tempfile::tempdir().expect("sealed state root");
    let _home = EnvVarGuard::set("CODEWHALE_HOME", temp.path());
    let config_path = temp.path().join("config.toml");
    std::fs::write(
        &config_path,
        "provider = \"moonshot\"\n\n[providers.moonshot]\napi_key = \"k\"\nmodel = \"kimi-k3\"\n",
    )
    .expect("seed config");
    std::fs::write(
        temp.path().join("settings.toml"),
        "[provider_models]\nmoonshot = \"kimi-k2.7-code\"\n",
    )
    .expect("seed settings");
    let _config_path_guard = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_path);
    let _codewhale_config_path = EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");

    // Without an explicit request this launch, the remembered pick still wins:
    // that stickiness is what `/model` exists for.
    let _no_flag = EnvVarGuard::remove("CODEWHALE_MODEL");
    let _no_legacy_flag = EnvVarGuard::remove("DEEPSEEK_MODEL");
    let config = codewhale_plugins::providers::load_config(Some(config_path.clone()), None)
        .expect("load sealed config");
    let remembered = App::new(
        TuiOptions {
            model: config.default_model(),
            ..test_options(false)
        },
        &config,
    );
    assert_eq!(
        remembered.model, "kimi-k2.7-code",
        "the remembered /model pick remains the default when nothing was named"
    );

    // `--model` reaches this binary as CODEWHALE_MODEL. It must win.
    let _model_flag = EnvVarGuard::set("CODEWHALE_MODEL", "kimi-k3");
    let config = codewhale_plugins::providers::load_config(Some(config_path), None)
        .expect("load explicit launch snapshot");
    let requested = App::new(
        TuiOptions {
            model: config.default_model(),
            ..test_options(false)
        },
        &config,
    );
    assert_eq!(
        requested.model, "kimi-k3",
        "an explicit --model must never be silently replaced by session memory"
    );
}

#[test]
fn ambient_clock_advances_by_clamped_steps() {
    let mut app = App::new(test_options(false), &Config::default());
    // First sample establishes the baseline without advancing.
    assert_eq!(app.sample_ambient_clock_ms(), 0);
    // Simulate a long gap between draws (a burst of stream work): the clock
    // may advance by at most one clamped step, so positions derived from it
    // cannot teleport across the gap.
    app.ambient_clock_sampled_at = Some(Instant::now() - Duration::from_secs(9));
    let advanced = app.sample_ambient_clock_ms();
    assert!(
        advanced <= App::AMBIENT_MAX_STEP_MS,
        "a 9s draw gap must clamp to one step, got {advanced}ms"
    );
}

#[test]
fn ambient_idle_settles_after_grace_and_wakes_on_activity() {
    let mut app = App::new(test_options(false), &Config::default());
    let start = Instant::now();
    // Fresh idle: not yet settled, anchor recorded.
    assert!(!app.ambient_idle_settled(false, start));
    // Still inside the grace window.
    assert!(!app.ambient_idle_settled(
        false,
        start + Duration::from_millis(App::AMBIENT_IDLE_SETTLE_MS - 500)
    ));
    // Past the grace window: the aquarium is still.
    assert!(app.ambient_idle_settled(
        false,
        start + Duration::from_millis(App::AMBIENT_IDLE_SETTLE_MS + 500)
    ));
    // Any live activity clears the anchor and wakes the scene…
    assert!(!app.ambient_idle_settled(true, start + Duration::from_secs(60)));
    // …and idleness afterwards restarts the full grace period.
    assert!(!app.ambient_idle_settled(false, start + Duration::from_secs(61)));
}

#[test]
fn launch_onboarding_scenario() {
    // Scenario consolidation of: launch_onboarding_skips_picker_when_xai_oauth_needs_reauth, launch_onboarding_opens_picker_for_generic_missing_key, launch_onboarding_clean_when_onboarded_with_key, launch_onboarding_starts_first_run_at_composer
    // from launch_onboarding_skips_picker_when_xai_oauth_needs_reauth
    {
        // #5032: an onboarded user whose active xAI OAuth credential is missing
        // must NOT be sent back to the generic provider picker every launch.
        let (onboarding, recovery) = launch_onboarding_decision(
            false, // skip_onboarding
            true,  // was_onboarded
            false, // needs_language
            true,  // needs_api_key
            false, // needs_workspace_trust
            true,  // xai_oauth_needs_reauth
        );
        assert_eq!(onboarding, OnboardingState::None);
        assert!(!recovery);
    }
    // from launch_onboarding_opens_picker_for_generic_missing_key
    {
        // A generic missing key (not the xAI-OAuth re-auth case) starts at the
        // composer with missing-key recovery armed for the first send.
        let (onboarding, recovery) =
            launch_onboarding_decision(false, true, false, true, false, false);
        assert_eq!(onboarding, OnboardingState::None);
        assert!(recovery);
    }
    // from launch_onboarding_clean_when_onboarded_with_key
    {
        let (onboarding, recovery) =
            launch_onboarding_decision(false, true, false, false, false, false);
        assert_eq!(onboarding, OnboardingState::None);
        assert!(!recovery);
    }
    // from launch_onboarding_starts_first_run_at_composer
    {
        // xAI OAuth re-auth never reopens the generic picker, first run or not.
        let (onboarding, recovery) =
            launch_onboarding_decision(false, false, false, true, false, true);
        assert_eq!(onboarding, OnboardingState::None);
        assert!(!recovery);

        let (keyless, recovery) = launch_onboarding_decision(false, false, true, true, true, false);
        assert_eq!(keyless, OnboardingState::None);
        assert!(recovery);

        // A first run with a key starts at the composer.
        let (trust, _) = launch_onboarding_decision(false, false, false, false, true, false);
        assert_eq!(trust, OnboardingState::None);

        let (ready, _) = launch_onboarding_decision(false, false, false, false, false, false);
        assert_eq!(ready, OnboardingState::None);
    }
}

/// Memory note M3: a resume owns the loaded session, so its journal and
/// history move into the App instead of being cloned beside a copy that is
/// dropped right after. The journal's entry buffer is the *same allocation*
/// afterwards, and the result matches the borrowing path exactly.
#[test]
fn owned_restore_moves_the_journal_and_matches_the_borrowing_restore() {
    let message = |text: &str| Message {
        role: codewhale_models::Role::User,
        content: vec![codewhale_models::ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    };
    let messages = vec![message("first"), message("second")];
    let t0 = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
    let t1 = t0 + chrono::Duration::seconds(12);
    let saved = codewhale_engine::session_manager::create_saved_session_with_id_mode_and_stamps(
        "owned-restore".to_string(),
        &messages,
        &[t0, t1],
        "test-model",
        Path::new("."),
        0,
        None,
        None,
    );

    let mut borrowed = App::new(test_options(false), &Config::default());
    borrowed.restore_api_messages(
        codewhale_engine::runtime_handoff::project_owned_messages_for_restore(
            saved.messages.clone(),
        ),
        &saved,
    );

    let mut owned_session = saved.clone();
    let entries_buffer = owned_session
        .journal
        .as_ref()
        .expect("journal")
        .entries
        .as_ptr();
    let mut owned = App::new(test_options(false), &Config::default());
    owned.restore_api_messages_from_owned(&mut owned_session);

    assert_eq!(
        owned.session_journal.entries.as_ptr(),
        entries_buffer,
        "the journal must be moved into the App, not cloned"
    );
    assert!(owned_session.journal.is_none());
    assert!(owned_session.messages.is_empty());
    assert_eq!(
        owned.session_journal.entries,
        borrowed.session_journal.entries
    );
    assert_eq!(owned.api_messages, borrowed.api_messages);
    assert_eq!(owned.api_message_stamps, vec![t0, t1]);
    assert_eq!(owned.api_message_stamps, borrowed.api_message_stamps);

    // A legacy session without a journal rebuilds it from the history, on
    // both paths alike.
    let mut legacy = saved.clone();
    legacy.journal = None;
    let mut legacy_borrowed = App::new(test_options(false), &Config::default());
    legacy_borrowed.restore_api_messages(
        codewhale_engine::runtime_handoff::project_owned_messages_for_restore(
            legacy.messages.clone(),
        ),
        &legacy,
    );
    let mut legacy_owned = App::new(test_options(false), &Config::default());
    legacy_owned.restore_api_messages_from_owned(&mut legacy);
    assert_eq!(legacy_owned.api_messages, legacy_borrowed.api_messages);
    assert_eq!(
        legacy_owned.session_journal.entries.len(),
        legacy_borrowed.session_journal.entries.len()
    );
    assert_eq!(legacy_owned.api_message_stamps.len(), 2);
}

/// Walk Ctrl+T for two full laps on a concrete route and assert that every
/// press changes the effective tier — the value `/status`, the effort status
/// line, and Work receipts report — not merely the requested label.
fn assert_every_ctrl_t_press_changes_the_effective_tier(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) -> Vec<ReasoningEffort> {
    let mut app = App::new(test_options(false), &Config::default());
    app.set_provider_identity_record(
        codewhale_runtime::config::Config::default()
            .resolve_provider_identity(provider.as_str())
            .expect("captured fixture provider"),
    );
    app.auto_model = false;
    app.active_route_base_url = base_url.to_string();
    app.model = model.to_string();
    app.reasoning_effort = ReasoningEffort::Auto;
    let ladder =
        crate::tui::model_picker::picker_efforts_for_route(provider, base_url, model, false);
    let mut effective = app.effective_reasoning_effort_for_active_route(app.reasoning_effort);
    let mut walked = Vec::new();
    // Two full laps: the report was about presses after the first lap.
    for press in 0..ladder.len() * 2 {
        assert_eq!(app.cycle_effort(), SettingSelection::Changed);
        let next = app.effective_reasoning_effort_for_active_route(app.reasoning_effort);
        assert_ne!(
            next, effective,
            "{model}: press {press} ({:?}) left the effective tier at {effective:?}",
            app.reasoning_effort
        );
        effective = next;
        walked.push(app.reasoning_effort);
    }
    let mut expected = ladder.clone();
    expected.rotate_left(1);
    expected.extend(expected.clone());
    assert_eq!(walked, expected, "{model} walks the picker ladder");
    ladder
}

#[test]
fn every_ctrl_t_press_changes_the_effective_thinking_tier() {
    // #6650: Ctrl+T walked rungs that resolved to the tier already in effect,
    // so presses looked dead.
    let _catalog = codewhale_engine::provider_lake::lock_live_snapshot();
    codewhale_engine::provider_lake::clear_live_snapshot();
    for (provider, base_url, model) in [
        (
            ProviderKind::Deepseek,
            codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL,
            "deepseek-v4.1-flash",
        ),
        (
            ProviderKind::Deepseek,
            codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL,
            "deepseek-v4.1",
        ),
        (
            ProviderKind::Moonshot,
            codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL,
            codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL,
        ),
        (
            ProviderKind::Xai,
            codewhale_config::descriptors::defaults::DEFAULT_XAI_BASE_URL,
            codewhale_config::catalog::reviewed::constants::XAI_GROK_4_6_MODEL,
        ),
    ] {
        assert_every_ctrl_t_press_changes_the_effective_tier(provider, base_url, model);
    }
}

#[test]
fn ctrl_t_skips_catalog_rungs_that_resolve_to_an_offered_tier() {
    // A catalog can publish effort spellings the route collapses: DeepSeek
    // sends `medium`/`xhigh` as `high`, and Z.ai GLM-5.2 sends `low`/`medium`
    // as `high`. Each Ctrl+T press must still reach a new effective tier.
    use ReasoningEffort::{Auto, High, Low, Max, Off};
    let _catalog = codewhale_engine::provider_lake::lock_live_snapshot();
    codewhale_engine::provider_lake::clear_live_snapshot();
    let fetched_at = u64::try_from(chrono::Utc::now().timestamp()).expect("timestamp");
    let offering = |provider: ProviderKind, model: &str, values: &[&str]| {
        codewhale_config::catalog::CatalogOffering {
            provider: provider.as_str().to_string(),
            wire_model_id: model.to_string(),
            endpoint_key: "chat".to_string(),
            reasoning_options: vec![serde_json::json!({ "type": "effort", "values": values })],
            source: codewhale_config::catalog::CatalogSource::ModelsDevLive { fetched_at },
            ..Default::default()
        }
    };
    codewhale_engine::provider_lake::set_live_snapshot(
        codewhale_config::catalog::CatalogSnapshot {
            offerings: vec![
                offering(
                    ProviderKind::Deepseek,
                    "deepseek-v4.1-flash",
                    &["low", "medium", "high", "xhigh", "max"],
                ),
                offering(
                    ProviderKind::Zai,
                    codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
                    &["off", "low", "medium", "high", "max"],
                ),
            ],
        },
        codewhale_engine::provider_lake::LiveSource::ModelsDev,
    );

    let deepseek = assert_every_ctrl_t_press_changes_the_effective_tier(
        ProviderKind::Deepseek,
        codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL,
        "deepseek-v4.1-flash",
    );
    let zai = assert_every_ctrl_t_press_changes_the_effective_tier(
        ProviderKind::Zai,
        codewhale_config::descriptors::defaults::DEFAULT_ZAI_BASE_URL,
        codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
    );
    codewhale_engine::provider_lake::clear_live_snapshot();

    assert_eq!(deepseek, vec![Auto, Low, High, Max]);
    assert_eq!(zai, vec![Auto, Off, High, Max]);
}

#[test]
fn ctrl_t_moves_past_a_persisted_alias_the_ladder_dropped() {
    // DeepSeek has no `medium`; it resolves to `high`, so the next press must
    // reach `max` rather than re-select `high`.
    let _catalog = codewhale_engine::provider_lake::lock_live_snapshot();
    let mut app = App::new(test_options(false), &Config::default());
    app.api_provider = ProviderKind::Deepseek;
    app.auto_model = false;
    app.active_route_base_url =
        codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL.to_string();
    app.model = "deepseek-v4.1-flash".to_string();
    app.reasoning_effort = ReasoningEffort::Medium;

    app.cycle_effort();

    assert_eq!(app.reasoning_effort, ReasoningEffort::Max);
}

#[test]
fn skills_cache_hides_model_only_and_preserves_argument_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    crate::test_support::trust_workspace(&workspace);
    let root = workspace.join(".codewhale/skills");
    for (name, policy) in [
        ("model", "user-invocable: false"),
        (
            "user",
            "disable-model-invocation: true\nargument-hint: '[path]'",
        ),
        (
            "disabled",
            "disable-model-invocation: true\nuser-invocable: false",
        ),
    ] {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: routing\n{policy}\n---\nbody"),
        )
        .unwrap();
    }
    let mut options = test_options(false);
    options.workspace = workspace;
    options.skills_dir = root;
    let app = App::new(options, &Config::default());
    assert!(
        app.cached_skills
            .iter()
            .all(|(name, _)| name != "model" && name != "disabled")
    );
    assert!(
        app.cached_skills
            .iter()
            .any(|(name, description)| name == "user" && description.contains("[path]"))
    );
}

#[cfg(unix)]
#[test]
fn oversized_paste_is_not_written_through_a_linked_pastes_directory() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let outside = tempfile::TempDir::new().expect("outside");
    std::os::unix::fs::symlink(outside.path(), tmp.path().join(".codewhale")).expect("link");
    let mut opts = test_options(false);
    opts.workspace = tmp.path().to_path_buf();
    let mut app = App::new(opts, &Config::default());
    app.insert_paste_text(&"y".repeat(MAX_SUBMITTED_INPUT_CHARS + 256));

    let _ = app.submit_input();

    assert!(
        std::fs::read_dir(outside.path()).unwrap().next().is_none(),
        "the pasted text must not land outside the workspace"
    );
}

#[cfg(not(windows))]
#[test]
fn dropped_screenshot_path_becomes_an_image_attachment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shot = dir.path().join("Screenshot 2026-10-04 at 22.25.47.png");
    std::fs::write(&shot, codewhale_engine::image_attach::test_support::PNG_1X1).expect("fixture");
    // Terminal.app / iTerm2 deliver a drop as a shell-escaped paste.
    let dropped = shot.display().to_string().replace(' ', "\\ ");
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "what is wrong here?".to_string();
    app.cursor_position = app.input.chars().count();

    app.insert_paste_text(&dropped);

    let line = format!("[Attached image: {}]", shot.display());
    assert!(app.input.contains(&line), "{}", app.input);
    assert!(!app.input.contains("\\ "), "{}", app.input);
    assert_eq!(app.composer_attachment_count(), 1);
    assert_eq!(
        app.status_message.as_deref(),
        Some(format!("Attached image: {}", shot.display()).as_str())
    );
    let expanded = codewhale_engine::image_attach::expand_attachment_blocks(&app.input);
    assert!(expanded.notices.is_empty(), "{expanded:?}");
    assert!(
        expanded
            .blocks
            .iter()
            .any(|block| matches!(block, codewhale_models::ContentBlock::ImageUrl { .. }))
    );
}

#[test]
fn image_path_pasted_on_a_command_line_stays_literal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shot = dir.path().join("shot.png");
    std::fs::write(&shot, codewhale_engine::image_attach::test_support::PNG_1X1).expect("fixture");
    let mut app = App::new(test_options(false), &Config::default());
    app.input = "/rename ".to_string();
    app.cursor_position = app.input.chars().count();

    app.insert_paste_text(&shot.display().to_string());

    assert_eq!(app.input, format!("/rename {}", shot.display()));
    assert_eq!(app.composer_attachment_count(), 0);
}

#[cfg(not(windows))]
#[test]
fn a_typed_drop_with_the_question_on_its_line_still_sends_the_image() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shot = dir.path().join("Screenshot 2026-10-04 at 22.25.47.png");
    std::fs::write(&shot, codewhale_engine::image_attach::test_support::PNG_1X1).expect("fixture");
    let mut app = App::new(test_options(false), &Config::default());
    // Arrived as keystrokes, so the paste-time check never saw it.
    app.input = format!(
        "{}  why does it show jobs 2?",
        shot.display().to_string().replace(' ', "\\ ")
    );
    app.cursor_position = app.input.chars().count();

    let submitted = app.submit_input().expect("submitted");

    assert!(
        submitted.starts_with(&format!("[Attached image: {}]\n", shot.display())),
        "{submitted}"
    );
    assert!(
        submitted.ends_with("why does it show jobs 2?"),
        "{submitted}"
    );
    let expanded = codewhale_engine::image_attach::expand_attachment_blocks(&submitted);
    assert!(expanded.notices.is_empty(), "{expanded:?}");
    assert!(
        expanded
            .blocks
            .iter()
            .any(|block| matches!(block, codewhale_models::ContentBlock::ImageUrl { .. }))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pet_view_choice_restores_without_launch_overlay_and_preserves_other_settings() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().unwrap();
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();
    let path = tmp.path().join(".codewhale/settings.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "# keep this comment\nlow_motion = true\ncontextual_tips = false\n",
    )
    .unwrap();
    let mut app = App::new(test_options(false), &Config::default());
    assert!(
        !app.pet_watch.enabled,
        "old settings keep their existing view"
    );
    app.pet_watch.detach_for_test();
    app.input = "unfinished draft".into();
    assert!(
        !crate::commands::execute_with_config("/pet on", &mut app, &Config::default()).is_error
    );
    assert!(app.pet_watch.enabled);
    assert!(app.view_stack.is_empty());
    assert_eq!(app.input, "unfinished draft");
    app.startup_defaults.flush();
    let restored = App::new(test_options(false), &Config::default());
    assert!(restored.pet_watch.enabled);
    assert!(
        !restored.launch.visible,
        "the welcome overlay must not hide the remembered view"
    );
    assert!(restored.low_motion);
    assert!(restored.view_stack.is_empty());
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("# keep this comment"));
    assert!(!Settings::load_persisted().unwrap().contextual_tips);

    // Both public selectors enqueue in user-action order; quit immediately
    // after the last choice and prove the existing shutdown join persists it.
    for value in [false, true, false, true] {
        app.pet_watch.detach_for_test();
        let result = crate::commands::execute_with_config(
            &format!(
                "/config pet_mode {} --save",
                if value { "true" } else { "false" }
            ),
            &mut app,
            &Config::default(),
        );
        assert!(!result.is_error);
    }
    assert!(
        !crate::commands::execute_with_config("/pet off", &mut app, &Config::default()).is_error
    );
    assert!(app.startup_defaults.shutdown().is_empty());
    assert!(!Settings::load_persisted().unwrap().pet_mode);
    assert!(
        !App::new(test_options(false), &Config::default())
            .pet_watch
            .enabled
    );

    // Session overrides and rejected values must not change the saved choice.
    app.pet_watch.detach_for_test();
    assert!(
        !crate::commands::execute_with_config(
            "/config pet_mode true",
            &mut app,
            &Config::default()
        )
        .is_error
    );
    assert!(
        crate::commands::execute_with_config(
            "/config pet_mode banana --save",
            &mut app,
            &Config::default()
        )
        .is_error
    );
    assert!(app.pet_watch.enabled);
    app.startup_defaults.flush();
    assert!(!Settings::load_persisted().unwrap().pet_mode);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pet_view_save_failure_keeps_the_live_view_and_reports_without_overwriting_bad_settings() {
    let _lock = lock_test_env();
    let tmp = tempfile::TempDir::new().unwrap();
    let _env = sealed_settings_home(tmp.path());
    let _writes = crate::tui::startup_defaults::allow_writes_in_tests();
    let path = tmp.path().join(".codewhale/settings.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let bad = "# preserve for repair\npet_mode = [broken";
    std::fs::write(&path, bad).unwrap();
    let mut app = App::new(test_options(false), &Config::default());
    app.pet_watch.detach_for_test();
    app.ui_locale = codewhale_localization::Locale::Fr;
    assert!(
        !crate::commands::execute_with_config("/pet on", &mut app, &Config::default()).is_error
    );
    assert!(app.pet_watch.enabled);
    app.startup_defaults.flush();
    let failures = app.startup_defaults.drain_failures();
    assert_eq!(failures.len(), 1);
    assert_eq!(
        failures[0].subjects,
        vec![crate::tui::startup_defaults::StartupDefaultSubject::PetMode]
    );
    let message = app.startup_default_failure_message(&failures[0]);
    assert!(message.contains(app.tr(MessageId::ConfigLabelPetMode).as_ref()));
    assert!(!message.contains(tmp.path().to_str().unwrap()));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), bad);
    assert!(
        app.pet_watch.enabled,
        "a failed save does not undo the live view"
    );
}
