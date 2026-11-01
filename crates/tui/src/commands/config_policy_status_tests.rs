//! Existing public status regressions, moved out of the portable closure.
use crate::commands::CommandResult;
use crate::tui::app::App;
use codewhale_execpolicy::ApprovalMode;
use codewhale_localization::{Locale, MessageId, tr};
fn status(app: &mut App) -> CommandResult {
    crate::commands::config_policy_host::status(app, None)
}
fn format_status(app: &mut App) -> String {
    status(app).message.expect("status report")
}

mod tests {
    use codewhale_models::Role;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;
    use crate::tui::app::TuiOptions;
    use crate::tui::history::HistoryCell;
    use codewhale_config::AppMode;
    use codewhale_config::ProviderKind;
    use codewhale_models::{ContentBlock, Message};
    use codewhale_runtime::config::Config;

    #[test]
    fn status_keeps_current_session_snapshot_remedy_after_notice_delivery() {
        let _env = crate::test_support::lock_test_env();
        let root = TempDir::new().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let _user_home = crate::test_support::EnvVarGuard::set("HOME", root.path());
        let _user_profile = crate::test_support::EnvVarGuard::set("USERPROFILE", root.path());
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("large.txt"), vec![b'x'; 4096]).unwrap();
        let mut app = create_test_app(workspace.clone());
        app.current_session_id = Some("session-a".into());
        assert!(
            codewhale_engine::core::turn::pre_turn_snapshot(
                &workspace,
                1,
                1024,
                None,
                Some("session-a")
            )
            .is_none()
        );
        assert_eq!(
            codewhale_engine::core::turn::take_snapshots_disabled_notices(
                &workspace,
                Some("session-a")
            )
            .len(),
            1
        );
        for _ in 0..2 {
            let report = status(&mut app).message.unwrap();
            assert!(report.contains("Snapshots and /undo are off"), "{report}");
            assert!(report.contains("snapshot-eligible content"), "{report}");
            // Stated once, not doubled by a raw reason plus a template.
            assert_eq!(
                report
                    .matches(codewhale_command_contract::config_policy::SNAPSHOTS_CAP_CONFIG_KEY)
                    .count(),
                1,
                "{report}"
            );
        }
        app.current_session_id = Some("session-b".into());
        assert!(
            !status(&mut app)
                .message
                .unwrap()
                .contains("Snapshots and /undo are off")
        );
        app.current_session_id = Some("session-a".into());
        assert!(
            codewhale_engine::core::turn::pre_turn_snapshot(
                &workspace,
                2,
                0,
                None,
                Some("session-a")
            )
            .is_some()
        );
        assert!(
            !status(&mut app)
                .message
                .unwrap()
                .contains("Snapshots and /undo are off")
        );
    }

    #[test]
    fn status_warns_when_the_session_pin_left_a_fresh_roster_and_keeps_it() {
        // #6035: warning only. The pin is never rewritten, and a route with
        // no fresh roster proves nothing, so it stays silent.
        let _env = crate::test_support::lock_test_env();
        let _live = codewhale_engine::provider_lake::lock_live_snapshot();
        let root = TempDir::new().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let _user_home = crate::test_support::EnvVarGuard::set("HOME", root.path());
        let _user_profile = crate::test_support::EnvVarGuard::set("USERPROFILE", root.path());
        codewhale_engine::provider_catalog_live::reset_cache_for_test();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = create_test_app(workspace);
        app.auto_model = false;
        app.model = "deepseek-v4-flash".to_string();
        let notice = "is not in deepseek's current model list";
        assert!(
            !status(&mut app).message.unwrap().contains(notice),
            "no fresh roster, no claim"
        );

        let config = codewhale_plugins::providers::load_config(
            app.config_path.clone(),
            app.config_profile.as_deref(),
        )
        .unwrap_or_default();
        let base_url = config.base_url_for_route(
            &config
                .resolve_provider_selection_identity("deepseek")
                .unwrap(),
        );
        let fingerprint = codewhale_config::catalog::base_url_fingerprint(&base_url);
        let fetched_at = codewhale_config::catalog::now_unix();
        codewhale_engine::provider_catalog_live::record_success(
            codewhale_config::catalog::ProviderCatalogDelta {
                provider: "deepseek".to_string(),
                base_url_fingerprint: fingerprint.clone(),
                fetched_at,
                offerings: vec![codewhale_config::catalog::CatalogOffering {
                    provider: "deepseek".to_string(),
                    wire_model_id: "deepseek-flash".to_string(),
                    endpoint_key: "chat".to_string(),
                    source: codewhale_config::catalog::CatalogSource::Live {
                        base_url_fingerprint: fingerprint,
                        fetched_at,
                    },
                    ..Default::default()
                }],
            },
        );

        let report = status(&mut app).message.unwrap();
        assert!(report.contains(notice), "{report}");
        assert!(report.contains("deepseek-v4-flash"), "{report}");
        assert_eq!(app.model, "deepseek-v4-flash", "the pin is left unchanged");

        app.model = "deepseek-flash".to_string();
        assert!(!status(&mut app).message.unwrap().contains(notice));
        app.model = "deepseek-v4-flash".to_string();
        app.auto_model = true;
        assert!(!status(&mut app).message.unwrap().contains(notice));
        codewhale_engine::provider_catalog_live::reset_cache_for_test();
    }

    fn create_test_app(workspace: PathBuf) -> App {
        let options = TuiOptions {
            skills_dir: PathBuf::from("/tmp/test-skills"),
            ..crate::tui::test_support::test_tui_options(workspace)
        };
        let mut app = App::new(options, &Config::default());
        app.api_provider = ProviderKind::Deepseek;
        app
    }

    #[test]
    fn status_report_includes_runtime_fields() {
        let tmpdir = TempDir::new().expect("temp dir");
        std::fs::write(tmpdir.path().join("AGENTS.md"), "# Instructions").expect("write docs");
        let mut app = create_test_app(tmpdir.path().to_path_buf());
        app.current_session_id = Some("session-123".to_string());
        app.session.total_tokens = 1234;
        app.session.last_prompt_tokens = Some(100);
        app.session.last_completion_tokens = Some(25);
        app.session.last_prompt_cache_hit_tokens = Some(70);
        app.session.last_prompt_cache_miss_tokens = Some(30);
        app.api_messages_mut().push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_string(),
                cache_control: None,
            }],
        });
        app.history.push(HistoryCell::User {
            content: "hello".to_string(),
        });

        let result = status(&mut app);
        let msg = result.message.expect("status message");
        assert!(msg.starts_with(&format!("codewhale {}", env!("CARGO_PKG_VERSION"))));
        assert!(msg.contains("Route:"));
        assert!(msg.contains("Directory:"));
        assert!(msg.contains("AGENTS.md"));
        assert!(msg.contains("Mode:"));
        assert!(msg.contains("approvals"));
        assert!(msg.contains("Session:"));
        assert!(msg.contains("session-123"));
        assert!(msg.contains("Context window:"));
        assert!(msg.contains("Tool outputs:"));
        assert!(msg.contains("Session tokens:"));
        assert!(msg.contains("/tokens"));
        assert!(msg.contains("/statusline"));
    }

    /// Every row has to earn its place in a 24-row terminal. The report used
    /// to run 31 lines, so at 80x24 — where the transcript viewport is 18
    /// rows — a user who typed `/status` landed on the *tail*: the version,
    /// route, directory, mode and sandbox rows had already scrolled off, and
    /// what remained on screen was five "not reported" rows and a `$0.0000`.
    ///
    /// A fresh session is 18 rows, not 17: `Window override:` is present
    /// unless the value is already configured. That matches the viewport
    /// height, so the title still scrolls off once `/status` occupies a
    /// history cell.
    #[test]
    fn status_report_fits_a_short_terminal() {
        let tmpdir = TempDir::new().expect("temp dir");
        let mut app = create_test_app(tmpdir.path().to_path_buf());
        let msg = status(&mut app).message.expect("status message");
        let rows = msg.lines().count();
        assert!(
            msg.contains("Window override:"),
            "fresh session keeps the override row: {msg}"
        );
        assert_eq!(
            rows, 18,
            "fresh session is 18 rows with Window override present, got {rows} rows:\n{msg}"
        );
        let source = msg
            .lines()
            .find(|line| line.contains("Window source:"))
            .unwrap();
        assert!(
            source.chars().count() <= 80,
            "fresh source provenance must not wrap: {source}"
        );
    }

    /// `Rate limits:` was a `push_row` of a string literal — it could never
    /// report anything but "not available from provider telemetry". A row
    /// that cannot say anything cannot inform, and it cost a row on every
    /// terminal forever.
    #[test]
    fn status_report_drops_the_row_that_could_never_say_anything() {
        let tmpdir = TempDir::new().expect("temp dir");
        let mut app = create_test_app(tmpdir.path().to_path_buf());
        let msg = status(&mut app).message.expect("status message");
        assert!(!msg.contains("Rate limits"), "{msg}");
        assert!(
            !msg.contains("not available from provider telemetry"),
            "{msg}"
        );
    }

    /// The per-turn ledger is `/tokens`' whole subject and `/status` printed
    /// six rows of it. Shedding the field beats printing it at the same
    /// weight as the sandbox policy — but only if the report says where it
    /// went, and only if the two facts that live nowhere else (the
    /// cumulative in/out split and the cumulative cache totals) survive.
    #[test]
    fn status_report_sheds_the_per_turn_ledger_and_names_where_it_went() {
        let tmpdir = TempDir::new().expect("temp dir");
        let mut app = create_test_app(tmpdir.path().to_path_buf());
        app.session.total_input_tokens = 900;
        app.session.total_output_tokens = 120;
        app.session.total_tokens = 1020;
        app.session.total_cache_hit_tokens = 700;
        app.session.total_cache_miss_tokens = 200;
        app.session.last_prompt_tokens = Some(100);

        let msg = status(&mut app).message.expect("status message");

        for shed in [
            "Last API input:",
            "Last API output:",
            "Cache hit/miss:",
            "Session input:",
            "Session output:",
            "Total tokens:",
            "Session cache:",
        ] {
            assert!(
                !msg.contains(shed),
                "{shed} should be shed, not printed: {msg}"
            );
        }
        assert!(msg.contains("Per-turn tokens: /tokens"), "{msg}");
        // The footer-item *keys* were a full-width row of internal config
        // names; `/statusline` is the surface that owns them.
        assert!(!msg.contains("reasoning_replay"), "{msg}");
        assert!(!msg.contains("git_branch"), "{msg}");
        assert!(msg.contains("Footer items: /statusline"), "{msg}");

        let row = msg
            .lines()
            .find(|line| line.trim_start().starts_with("Session tokens:"))
            .expect("session tokens row");
        assert!(row.contains("900 in"), "{row}");
        assert!(row.contains("120 out"), "{row}");
        assert!(row.contains("1020 total"), "{row}");
        assert!(row.contains("cache 700 hit / 200 miss"), "{row}");
    }

    /// Provider, model and effort are one fact — which route this turn goes
    /// to — and the header rail already renders them as one dotted lockup.
    #[test]
    fn status_report_states_the_route_the_way_the_header_does() {
        let tmpdir = TempDir::new().expect("temp dir");
        let mut app = create_test_app(tmpdir.path().to_path_buf());
        let msg = status(&mut app).message.expect("status message");
        assert!(!msg.contains("Provider:"), "{msg}");
        assert!(!msg.contains("Model:"), "{msg}");
        let row = msg
            .lines()
            .find(|line| line.trim_start().starts_with("Route:"))
            .expect("route row");
        assert!(row.contains(" · "), "route must read as a lockup: {row}");
        assert!(row.contains("reasoning"), "{row}");
    }

    /// #5134: the number alone sends users to the issue tracker. `/status` has
    /// to name the provenance and the key that changes it, and it must name the
    /// table the user is actually on — not a generic placeholder. The two are
    /// separate facts, so the key gets its own aligned row instead of a
    /// parenthesis that pushed the provenance off the end of an 80-column line.
    #[test]
    fn status_report_names_context_window_source_and_override_key() {
        let tmpdir = TempDir::new().expect("temp dir");
        let mut app = create_test_app(tmpdir.path().to_path_buf());
        app.set_provider_identity_record(
            codewhale_runtime::config::Config::default()
                .resolve_provider_identity(ProviderKind::Moonshot.as_str())
                .expect("captured fixture provider"),
        );

        let msg = status(&mut app).message.expect("status message");

        let source_row = msg
            .lines()
            .find(|line| line.trim_start().starts_with("Window source:"))
            .expect("window source row");
        assert!(
            !source_row.contains("context_window"),
            "the provenance row states the provenance only: {source_row}"
        );
        // A labelled row, not an indented continuation: the transcript cell
        // strips leading whitespace, so an aligned continuation line rendered
        // flush against the label column and read as a field of its own with
        // the label missing.
        let override_row = msg
            .lines()
            .find(|line| line.trim_start().starts_with("Window override:"))
            .expect("window override row");
        assert!(
            override_row.contains("[providers.moonshot] context_window in config.toml"),
            "{override_row}"
        );

        // A user override reads as a statement of fact, not as advice to set
        // something that is already set.
        app.active_context_window =
            Some(codewhale_engine::route_runtime::ContextWindowResolution {
                tokens: 262_144,
                source: codewhale_engine::route_runtime::ContextWindowSource::Configured,
            });
        app.active_context_window_source =
            codewhale_engine::route_runtime::ContextWindowSource::Configured;
        let msg = status(&mut app).message.expect("status message");
        let row = msg
            .lines()
            .find(|line| line.trim_start().starts_with("Window source:"))
            .expect("window source row");
        assert!(row.contains("configured"), "{row}");
        assert!(!msg.contains("Window override:"), "{msg}");
    }

    #[test]
    fn status_report_keeps_exact_named_custom_provider() {
        let tmpdir = TempDir::new().expect("temp dir");
        let mut app = create_test_app(tmpdir.path().to_path_buf());
        app.set_provider_identity(ProviderKind::Custom, "lm-studio");

        let msg = status(&mut app).message.expect("status message");

        let route_row = msg
            .lines()
            .find(|line| line.trim_start().starts_with("Route:"))
            .expect("route row");
        assert!(route_row.contains("lm-studio"), "{route_row}");
        assert!(!route_row.contains("custom"), "{route_row}");
        assert!(
            msg.contains("[providers.lm-studio] context_window in config.toml"),
            "the override must name the captured route table: {msg}"
        );
    }

    #[test]
    fn status_report_interpolation_preserves_braces_in_runtime_values() {
        let tmpdir = TempDir::new().expect("temp dir");
        let mut app = create_test_app(tmpdir.path().to_path_buf());
        app.set_provider_identity(ProviderKind::Custom, "acme-{model}");
        app.model = "vision-{reasoning}".to_string();
        app.current_session_id = Some("session-{cells}-{messages}".to_string());

        let msg = format_status(&mut app);
        let route_row = msg
            .lines()
            .find(|line| line.trim_start().starts_with("Route:"))
            .expect("route row");
        assert!(
            route_row.contains("acme-{model} · vision-{reasoning} ·"),
            "{route_row}"
        );
        let session_row = msg
            .lines()
            .find(|line| line.trim_start().starts_with("Session:"))
            .expect("session row");
        assert!(
            session_row.contains("session-{cells}-{messages}"),
            "{session_row}"
        );
    }

    #[test]
    fn status_report_surfaces_effective_safety_policy() {
        let tmpdir = TempDir::new().expect("temp dir");
        let mut app = create_test_app(tmpdir.path().to_path_buf());
        // `/status` is honest about enforcement: on a platform with no OS
        // sandbox (e.g. Windows) it reports "<policy> requested, not enforced"
        // instead of the enforced string. The test must hold on both, so it
        // branches on the same signal `safety_summary` uses (`sandbox_backend`).
        let unenforced = app.sandbox_backend.is_none();

        app.mode = AppMode::Agent;
        let agent = format_status(&mut app);
        assert!(agent.contains("Safety:"));
        if unenforced {
            assert!(agent.contains("workspace-write requested, not enforced"));
        } else {
            // workspace-write no longer implies egress; /status must say so.
            assert!(agent.contains("sandbox workspace-write, network off"));
        }

        app.approval_mode = ApprovalMode::Bypass;
        let full_access = format_status(&mut app);
        assert!(full_access.contains("sandbox disabled, network unrestricted"));

        app.configured_sandbox_mode = Some("workspace-write".to_string());
        let clamped = format_status(&mut app);
        if unenforced {
            assert!(clamped.contains("workspace-write requested, not enforced"));
        } else {
            // Clamping full access down to workspace-write lands on the same
            // restricted posture an ordinary Agent turn gets.
            assert!(clamped.contains("sandbox workspace-write, network off"));
        }

        // The explicit opt-in is the only thing that flips the reported label.
        app.configured_sandbox_network = Some(true);
        let networked = format_status(&mut app);
        if unenforced {
            assert!(networked.contains("workspace-write requested, not enforced"));
        } else {
            assert!(networked.contains("sandbox workspace-write, network on"));
        }
        app.configured_sandbox_network = None;

        app.mode = AppMode::Plan;
        let plan = format_status(&mut app);
        if unenforced {
            assert!(plan.contains("read-only requested, not enforced"));
        } else {
            assert!(plan.contains("sandbox read-only, network off"));
        }

        app.configured_sandbox_mode = None;
        app.mode = AppMode::Agent;
        let yolo = format_status(&mut app);
        assert!(yolo.contains("sandbox disabled, network unrestricted"));
    }

    #[test]
    fn status_report_surfaces_large_tool_output_pressure() {
        let tmpdir = TempDir::new().expect("temp dir");
        let mut app = create_test_app(tmpdir.path().to_path_buf());
        let raw = "RAW_STATUS_PRESSURE\n".repeat(2_000);
        app.api_messages_mut().push(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "call-big".to_string(),
                content: raw,
                is_error: None,
                content_blocks: None,
            }],
        });
        app.session_artifacts
            .push(codewhale_runtime::artifacts::ArtifactRecord {
                id: "art_call-big".to_string(),
                kind: codewhale_runtime::artifacts::ArtifactKind::ToolOutput,
                session_id: "session-123".to_string(),
                tool_call_id: "call-big".to_string(),
                tool_name: "exec_shell".to_string(),
                created_at: chrono::Utc::now(),
                byte_size: 24_000,
                preview: "large output".to_string(),
                storage_path: PathBuf::from("artifacts/art_call-big.txt"),
            });

        let result = status(&mut app);
        let msg = result.message.expect("status message");

        assert!(msg.contains("Tool outputs:"));
        assert!(msg.contains("raw over cap"));
        assert!(msg.contains("context pressure"));
        assert!(msg.contains("artifact"));
    }

    #[test]
    fn status_report_localizes_the_complete_japanese_surface() {
        let tmpdir = TempDir::new().expect("temp dir");
        let mut app = create_test_app(tmpdir.path().to_path_buf());
        app.ui_locale = Locale::Ja;
        app.approval_mode = ApprovalMode::Bypass;
        // The installed window carries its own source and outranks the bare
        // field, so a fixture that only sets the source keeps whatever window
        // the test app resolved and the status reports that source instead.
        app.active_context_window =
            Some(codewhale_engine::route_runtime::ContextWindowResolution {
                tokens: 200_000,
                source: codewhale_engine::route_runtime::ContextWindowSource::ProviderReported,
            });
        app.active_context_window_source =
            codewhale_engine::route_runtime::ContextWindowSource::ProviderReported;

        let msg = format_status(&mut app);

        for id in [
            MessageId::StatusLabelRoute,
            MessageId::StatusLabelDirectory,
            MessageId::StatusLabelProjectDocs,
            MessageId::StatusLabelMode,
            MessageId::StatusLabelSafety,
            MessageId::StatusLabelContextWindow,
            MessageId::StatusLabelWindowSource,
            MessageId::StatusLabelWindowOverride,
            MessageId::StatusLabelSession,
            MessageId::StatusLabelSessionTokens,
            MessageId::StatusLabelSessionCost,
            MessageId::StatusLabelToolOutputs,
            MessageId::StatusProjectDocsNone,
            MessageId::StatusContextSourceProviderReported,
            MessageId::StatusSessionNotSaved,
            MessageId::StatusToolNone,
            MessageId::StatusSafetyDisabled,
        ] {
            let japanese = tr(Locale::Ja, id);
            assert_ne!(japanese, tr(Locale::En, id), "{id:?} copied English");
            assert!(msg.contains(japanese.as_ref()), "missing {id:?}: {msg}");
        }

        for english in [
            "Route:",
            "Directory:",
            "Project docs:",
            "Mode:",
            "Safety:",
            "Context window:",
            "Window source:",
            "Window override:",
            "Session:",
            "Session tokens:",
            "Session cost:",
            "Tool outputs:",
            "reasoning ",
            "no project docs",
            "not saved yet",
            "no large outputs tracked",
            "Per-turn tokens:",
        ] {
            assert!(
                !msg.contains(english),
                "English leaked as {english:?}: {msg}"
            );
        }

        // Protocol/config identities and commands remain literal inside the
        // translated prose.
        for literal in [
            "deepseek",
            "context_window",
            "config.toml",
            "/tokens",
            "/statusline",
        ] {
            assert!(msg.contains(literal), "missing literal {literal:?}: {msg}");
        }
    }

    #[test]
    fn project_docs_reports_missing_docs() {
        let tmpdir = TempDir::new().expect("temp dir");
        let mut app = create_test_app(tmpdir.path().to_path_buf());
        assert_eq!(
            format_status(&mut app)
                .lines()
                .find(|line| line.trim_start().starts_with("Project docs:")),
            Some("  Project docs:    no project docs")
        );
    }
}

fn safety_id(flag: Option<bool>) -> MessageId {
    use crate::commands::groups::config::policy::{
        policy_messages::StatusText, status::safety_disabled_message,
    };
    match safety_disabled_message(flag) {
        StatusText::StatusSafetyDisabledSetuidBlocked => {
            MessageId::StatusSafetyDisabledSetuidBlocked
        }
        StatusText::StatusSafetyDisabledSetuidAllowed => {
            MessageId::StatusSafetyDisabledSetuidAllowed
        }
        StatusText::StatusSafetyDisabled => MessageId::StatusSafetyDisabled,
        other => panic!("unexpected safety text {other:?}"),
    }
}
#[test]
fn status_safety_row_discloses_no_new_privs_flag_state_for_full_access() {
    // #5723: both flag states get a distinct, truthful row; a platform
    // without the flag keeps the plain full-access label. The live query
    // is host-dependent, so the selector is pinned directly.
    let blocked = tr(Locale::En, safety_id(Some(true)));
    assert!(
        blocked.contains("sandbox disabled, network unrestricted"),
        "{blocked}"
    );
    assert!(blocked.contains("sudo/setuid blocked"), "{blocked}");

    let relaxed = tr(Locale::En, safety_id(Some(false)));
    assert!(
        relaxed.contains("sandbox disabled, network unrestricted"),
        "{relaxed}"
    );
    assert!(relaxed.contains("sudo/setuid allowed"), "{relaxed}");

    let plain = safety_id(None);
    assert_eq!(
        tr(Locale::En, plain),
        tr(Locale::En, MessageId::StatusSafetyDisabled)
    );

    // The disclosure is real prose, so every complete pack must carry a
    // translation rather than a copy of the English string.
    for id in [safety_id(Some(true)), safety_id(Some(false))] {
        assert_ne!(tr(Locale::Ja, id), tr(Locale::En, id), "{id:?}");
    }
}
