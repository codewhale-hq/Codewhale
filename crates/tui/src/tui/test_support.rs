use std::path::{Path, PathBuf};

// ── Shared App/TuiOptions fixtures (#3923) ──────────────────────────────
//
// Before this module owned them, `create_test_app` was copy-pasted across 28
// test modules, each spelling out the full `TuiOptions` literal — 87 literals
// in all. The copies had drifted: different modules pinned different locales,
// currencies, and onboarding flags without anyone having chosen that, which is
// the non-hermeticity behind the intermittent `config_command_allow_shell_*`
// failures. Adding a `TuiOptions` field meant editing up to 87 sites.
//
// Express intentional differences by mutating the returned value at the call
// site, so the difference is visible as a deliberate line of test code rather
// than hidden inside another near-identical literal.

/// Default `TuiOptions` for tests, pinned to the deepseek-v4-pro fixture route.
pub(crate) fn test_tui_options(workspace: impl AsRef<Path>) -> crate::tui::app::TuiOptions {
    let workspace = workspace.as_ref().to_path_buf();
    crate::tui::app::TuiOptions {
        model: "deepseek-v4-pro".to_string(),
        workspace,
        config_path: None,
        config_profile: None,
        allow_shell: false,
        screen_mode: crate::tui::app::ScreenMode::Fullscreen,
        use_mouse_capture: false,
        mouse_capture_preference: false,
        use_bracketed_paste: true,
        max_subagents: 1,
        skills_dir: PathBuf::from("."),
        memory_path: PathBuf::from("memory.md"),
        notes_path: PathBuf::from("notes.txt"),
        mcp_config_path: PathBuf::from("mcp.json"),
        use_memory: false,
        // Majority-of-fixtures defaults, measured across the 89 literals this
        // helper replaced. Modules that need the other value say so explicitly.
        start_in_agent_mode: false,
        skip_onboarding: true,
        yolo: false,
        resume_session_id: None,
        initial_input: None,
        startup_notice: None,
    }
}

/// Build an `App` whose observable state does not depend on the developer's
/// machine.
///
/// `App::new` consults real persisted settings (provider/model maps,
/// auto-model, route limits, locale, currency), so an un-pinned fixture
/// computes against whatever the developer last configured. Every pin below
/// exists because some test was observed to depend on it. This fixture models
/// a session after the user has chosen a Startup action; direct `App::new`
/// tests remain the clean-launch authority.
pub(crate) fn test_app_with_options(options: crate::tui::app::TuiOptions) -> crate::tui::app::App {
    let config = codewhale_runtime::config::Config::default();
    let mut app = crate::tui::app::App::new(options, &config);

    // Shared behavior tests operate on the live session surface. Do not make
    // the production startup conditional for them: clean launches are covered
    // by direct `App::new` tests that retain the Tideline Startup Hero.
    app.launch.visible = false;

    // Deterministic presentation regardless of host locale.
    app.cost_currency = codewhale_config::pricing::CostCurrency::Usd;
    app.ui_locale = codewhale_localization::Locale::En;
    // Transcript tests must not depend on a concurrently swapped settings
    // home. Tests for hidden reasoning opt out explicitly.
    app.show_thinking = true;
    // Pin the route identity: without this, a machine with customized
    // settings computes context-window assertions against a different model
    // than the requested deepseek-v4-pro.
    app.set_provider_identity(codewhale_config::ProviderKind::Deepseek, "deepseek");
    app.billing_presentation = crate::route_billing::BillingPresentation::Metered;
    app.model = "deepseek-v4-pro".to_string();
    app.auto_model = false;
    app.last_effective_model = None;
    app.active_route_limits = None;
    app.active_context_window_override = None;
    // Fixtures replace `app.workspace` freely. Do not retain `App::new`'s real
    // process cwd as a second discovery root: parallel tests and a large
    // developer checkout can otherwise consume the bounded mention index
    // before the fixture workspace is scanned.
    app.composer.mention_cwd = None;
    // `App::new` derives onboarding state from the real `~/.codewhale`, and a
    // pending step makes `ui::frame::render` take its onboarding early return
    // before it assigns `last_prompt_area` or any other chrome geometry. CI
    // has no such state, so a layout test written against that machine passes
    // there and fails on any developer box mid-onboarding — for no product
    // reason. Shared fixtures render the ordinary session surface; onboarding
    // has its own tests that set this state deliberately.
    app.onboarding = crate::tui::app::OnboardingState::None;
    app
}
