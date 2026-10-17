use crate::commands::CommandResult;
use crate::config::Config;
use crate::tui::app::{App, AppAction, TuiOptions};
use crate::tui::views::ModalKind;
use codewhale_config::AppMode;
use codewhale_localization::Locale;
use std::path::PathBuf;
use tempfile::tempdir;

fn help(app: &mut App, topic: Option<&str>) -> CommandResult {
    let input = topic
        .map(|topic| format!("/help {topic}"))
        .unwrap_or_else(|| "/help".into());
    crate::commands::execute(&input, app)
}

fn codewhale_links(app: &mut App) -> CommandResult {
    crate::commands::execute("/links", app)
}

fn home_dashboard(app: &mut App) -> CommandResult {
    crate::commands::execute("/overview", app)
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
fn help_topic_resolves_a_discovered_skill_and_both_shapes() {
    // #3912: `/help <skill>` said "Unknown command" for a skill that
    // executes and autocompletes.
    let mut app = create_test_app();
    app.cached_skills = vec![(
        "codereview".to_string(),
        "Review a diff for defects".to_string(),
    )];

    for topic in [
        "codereview",
        "CodeReview",
        "$codereview",
        "/skill codereview",
    ] {
        let result = help(&mut app, Some(topic));
        let message = result
            .message
            .as_deref()
            .unwrap_or_else(|| panic!("{topic} should resolve to skill help"));
        assert!(message.contains("Review a diff for defects"), "{message}");
        assert!(message.contains("$codereview"), "{message}");
        assert!(message.contains("/skill codereview"), "{message}");
    }
}

#[test]
fn help_topic_still_errors_for_an_unknown_name() {
    let mut app = create_test_app();
    app.cached_skills = vec![("codereview".to_string(), "Review a diff".to_string())];
    let result = help(&mut app, Some("definitely-not-a-thing"));
    assert!(result.is_error, "unknown topics must still be an error");
    assert!(
        !result
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("Review a diff"),
        "the skill fallback must not match an unrelated topic"
    );
}

#[test]
fn test_help_unknown_command() {
    let mut app = create_test_app();
    let result = help(&mut app, Some("nonexistent"));
    assert!(result.message.is_some());
    assert!(result.message.unwrap().contains("Unknown command"));
    assert!(result.action.is_none());
}

#[test]
fn test_help_known_command() {
    let mut app = create_test_app();
    let result = help(&mut app, Some("clear"));
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("clear"));
    assert!(msg.contains("Clear conversation history"));
    assert!(msg.contains("Usage: /clear"));
}

#[test]
fn test_help_config_topic_uses_interactive_editor_text() {
    let mut app = create_test_app();
    let result = help(&mut app, Some("config"));
    let msg = result.message.expect("help topic should return message");
    assert!(msg.contains("config"));
    assert!(msg.contains("Inspect and change settings"));
    assert!(msg.contains("Usage: /config"));
    assert!(msg.contains("context_window = 262144"));
    assert!(msg.contains("/config context_window"));
    assert!(msg.contains("/config search.provider"));
    assert!(msg.contains("/config prompt_suggestion"));
    assert!(msg.contains("/config notifications"));
}

#[test]
fn test_help_links_topic_shows_aliases() {
    let mut app = create_test_app();
    let result = help(&mut app, Some("links"));
    let msg = result.message.expect("help topic should return message");
    assert!(msg.contains("links"));
    assert!(msg.contains("Show Codewhale, community, and provider links"));
    assert!(msg.contains("Usage: /links"));
    assert!(msg.contains("Aliases: dashboard, api"));

    let root = tempdir().unwrap();
    crate::test_support::trust_workspace(root.path());
    let directory = root.path().join(".codewhale/commands");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("custom-links.md"), "---\ndescription: User link helper\nusage: /custom-links <target>\naliases: api\n---\nuser link $ARGUMENTS").unwrap();
    let mut app = crate::tui::test_support::test_app_with_options(
        crate::tui::test_support::test_tui_options(root.path()),
    );
    app.ui_locale = Locale::En;
    app.cached_skills = vec![
        ("custom-links".into(), "Wrong skill fallback".into()),
        ("links".into(), "Wrong builtin fallback".into()),
    ];
    crate::commands::user_registry::reload(Some(root.path()));
    for topic in ["custom-links", "api"] {
        let result = help(&mut app, Some(topic));
        assert!(!result.is_error);
        let text = result.message.unwrap();
        assert!(text.starts_with("custom-links\n\n"));
        assert!(text.contains("User link helper"));
        assert!(text.contains("Usage: /custom-links <target>"));
        assert!(text.contains("Aliases: api"), "{text}");
        assert!(!text.contains("Wrong skill fallback"));
    }
    let text = help(&mut app, Some("links")).message.unwrap();
    assert!(text.starts_with("links\n\n"));
    assert!(text.contains("Aliases: dashboard, lianjie"));
    assert!(!text.contains("Aliases: dashboard, api"));
    assert!(!text.contains("Wrong builtin fallback"));
}

#[test]
fn test_help_memory_topic_shows_usage_and_description() {
    let mut app = create_test_app();
    let result = help(&mut app, Some("memory"));
    let msg = result.message.expect("help topic should return message");
    assert!(msg.contains("memory"));
    assert!(msg.contains("persistent structured user memory"));
    assert!(msg.contains(
        "Usage: /memory [status|path|search|get|remember|import|export|reindex|clear|help]"
    ));
}

#[test]
fn test_help_pushes_overlay() {
    let mut app = create_test_app();
    assert_ne!(app.view_stack.top_kind(), Some(ModalKind::Help));
    let result = help(&mut app, None);
    assert_eq!(result.message, None);
    assert_eq!(result.action, None);
    assert_eq!(app.view_stack.top_kind(), Some(ModalKind::Help));
}

#[test]
fn test_help_does_not_duplicate_overlay() {
    let mut app = create_test_app();
    assert!(app.view_stack.is_empty());
    help(&mut app, None);
    let initial_kind = app.view_stack.top_kind();
    help(&mut app, None);
    assert_eq!(app.view_stack.top_kind(), initial_kind);
    assert!(app.view_stack.pop().is_some());
    assert!(
        app.view_stack.is_empty(),
        "one dismissal must remove the only help modal"
    );
}

#[test]
fn test_codewhale_links() {
    let mut app = create_test_app();
    let result = codewhale_links(&mut app);
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("Codewhale & community"));
    assert!(msg.contains("https://codewhale.net/en/docs"));
    assert!(msg.contains("https://codewhale.net/en/community"));
    assert!(msg.contains("https://github.com/codewhale-hq/CodeWhale"));
    assert!(msg.contains("https://app.codewhale.net"));
    assert!(msg.contains("separate sign-in"));
    assert!(msg.contains("not connected to the current local session"));
    assert!(msg.contains("Provider Links"));
    assert!(msg.contains("DeepSeek (deepseek) <- current"));
    assert!(msg.contains("https://platform.deepseek.com/api_keys"));
    assert!(msg.contains("Xiaomi MiMo (xiaomi-mimo)"));
    assert!(msg.contains("https://platform.xiaomimimo.com/token-plan"));
    assert!(msg.contains("Moonshot/Kimi (moonshot)"));
    assert!(msg.contains("https://platform.kimi.ai/console/api-keys"));
    assert!(msg.contains("https://platform.kimi.ai/docs/overview"));
    assert!(msg.contains("https://api.kimi.com/coding/v1"));
    assert!(msg.contains("https://www.kimi.com/code/console"));
    assert!(msg.contains("never imports Kimi CLI credentials"));
    assert!(msg.contains("https://console.openmodel.ai/"));
    assert!(msg.contains("https://docs.openmodel.ai/en/docs/getting-started/authentication"));
    assert!(msg.contains("https://console.sakana.ai/api-keys"));
    assert!(msg.contains("https://console.sakana.ai/get-started"));
    assert!(msg.contains("Baidu Qianfan (qianfan)"));
    assert!(msg.contains("https://cloud.baidu.com/doc/qianfan/index.html"));
    assert!(msg.contains("Local Ollama is keyless by default"));
    assert!(msg.contains("codewhale auth chatgpt"));
    assert!(
        !msg.contains("codex login"),
        "official sign-in must not direct users to external CLI credentials"
    );
    assert!(msg.contains("no canonical vendor credential page exists"));
    assert!(msg.contains("OPENAI_API_KEY"));
    assert!(msg.contains("XIAOMI_MIMO_TOKEN_PLAN_API_KEY"));
    assert!(!msg.contains("https://codewhale.dev/docs/providers"));
    assert!(result.action.is_none());
}

#[test]
fn provider_links_emit_urls_as_inline_code_for_narrow_transcripts() {
    let mut app = create_test_app();
    let result = codewhale_links(&mut app);
    let msg = result.message.expect("links should return a message");

    assert!(msg.contains("`https://platform.openai.com/api-keys`"));
    assert!(
        msg.contains("`https://platform.minimax.io/user-center/basic-information/interface-key`")
    );

    for line in msg.lines().filter(|line| line.contains("http")) {
        let Some(url_start) = line.find("http") else {
            continue;
        };
        assert!(
            line[..url_start].ends_with('`') && line[url_start..].contains('`'),
            "provider URL should be inline-code wrapped so narrow TUI renders do not emit oversized OSC8 link payloads: {line}"
        );
    }
}

#[test]
fn project_links_follow_the_available_public_site_locale() {
    let mut app = create_test_app();
    app.ui_locale = Locale::ZhHans;

    let msg = codewhale_links(&mut app)
        .message
        .expect("links should return a message");

    assert!(msg.contains("`https://codewhale.net/zh/docs`"));
    assert!(msg.contains("`https://codewhale.net/zh/community`"));
    assert!(msg.contains("`https://app.codewhale.net`"));
}

#[test]
fn test_home_dashboard_includes_all_sections() {
    let mut app = create_test_app();
    app.session.total_conversation_tokens = 1234;
    let result = home_dashboard(&mut app);
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("codewhale"));
    assert!(!msg.contains("codewhale Home Dashboard"));
    assert!(msg.contains("Model:"));
    assert!(msg.contains("Mode:"));
    assert!(msg.contains("Workspace:"));
    assert!(msg.contains("History:"));
    assert!(msg.contains("Tokens:"));
    assert!(msg.contains("Quick Actions"));
    assert!(msg.contains("Mode Tips"));
    assert!(result.action.is_none());
}

#[test]
fn test_home_dashboard_shows_queued_when_present() {
    let mut app = create_test_app();
    app.queued_messages
        .push_back(crate::tui::app::QueuedMessage::new(
            "test".to_string(),
            None,
        ));
    let result = home_dashboard(&mut app);
    let msg = result.message.unwrap();
    assert!(msg.contains("Queued:"));
}

#[test]
fn test_home_dashboard_mode_tips_for_each_mode() {
    let modes = [AppMode::Agent, AppMode::Plan, AppMode::Operate];
    for mode in modes {
        let mut app = create_test_app();
        app.mode = mode;
        let result = home_dashboard(&mut app);
        let msg = result.message.unwrap();
        assert!(msg.contains("Mode Tips"), "Missing tips for mode {mode:?}");
    }
}

#[test]
fn test_home_dashboard_quick_actions_reflect_links_and_config_and_hide_removed_commands() {
    let mut app = create_test_app();
    let result = home_dashboard(&mut app);
    let msg = result
        .message
        .expect("home dashboard should return message");
    assert!(msg.contains("/workspace   - Switch folders or worktrees"));
    assert!(msg.contains("/restore     - Roll files back to a turn snapshot"));
    assert!(msg.contains("/tokens      - Show session spend and context"));
    assert!(msg.contains("/links       - Codewhale, community & provider links"));
    assert!(msg.contains("/config      - Inspect and change settings"));
    assert!(
        !msg.lines()
            .any(|line| line.trim_start().starts_with("/set "))
    );
    assert!(!msg.contains("/codewhale"));
}

#[test]
fn home_dashboard_localizes_in_zh_hans() {
    use codewhale_localization::Locale;
    let mut app = create_test_app();
    app.ui_locale = Locale::ZhHans;
    let result = home_dashboard(&mut app);
    let msg = result
        .message
        .expect("home dashboard should return message");
    assert!(
        msg.contains("codewhale"),
        "missing canonical product title:\n{msg}"
    );
    assert!(msg.contains("模型"), "missing zh-Hans model label:\n{msg}");
    assert!(
        msg.contains("快捷操作"),
        "missing zh-Hans quick actions:\n{msg}"
    );
    assert!(
        msg.contains("模式提示"),
        "missing zh-Hans mode tips:\n{msg}"
    );
}

#[cfg(test)]
mod home {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn home_preserves_session_draft_and_reveal_and_escape_returns() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options("."),
        );
        app.launch.dismiss();
        app.current_session_id = Some("retained-session".into());
        app.api_messages_mut().push(codewhale_models::Message {
            role: codewhale_models::Role::User,
            content: vec![codewhale_models::ContentBlock::Text {
                text: "Retain this conversation".into(),
                cache_control: None,
            }],
        });
        app.add_message(crate::tui::history::HistoryCell::System {
            content: "Retain this transcript".into(),
        });
        app.input = "unsent draft".into();
        app.cursor_position = app.input.chars().count();
        app.launch.mark_reveal_started_at = Some(std::time::Instant::now());
        let reveal = app.launch.mark_reveal_started_at;
        let messages = app.api_messages.clone();
        let history_len = app.history.len();
        let result = crate::commands::execute("/home", &mut app);
        assert!(!result.is_error, "{:?}", result.message);
        assert!(
            result.action.is_none(),
            "navigation must not sync/reset the Engine"
        );
        assert!(crate::tui::widgets::should_render_empty_state(&app));
        assert!(app.launch.return_to_session);
        assert!(!crate::tui::underwater::launch_motion_active(
            &app, false, true
        ));
        crate::tui::underwater::refresh_launch_row_hitboxes(
            &mut app,
            ratatui::layout::Rect::new(0, 0, 80, 20),
        );
        let rows = crate::tui::underwater::launch_rows_for_app(&app);
        assert_eq!(
            crate::tui::underwater::launch_row_click_action(&rows[0].id),
            crate::tui::underwater::LaunchAction::ReturnToSession,
        );
        app.launch.menu_selected = Some(0);
        assert_eq!(
            crate::tui::underwater::handle_launch_composer_key(
                &mut app,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            ),
            crate::tui::underwater::LaunchComposerKey::MenuRun,
        );
        assert_eq!(app.input, "unsent draft");
        crate::tui::underwater::handle_launch_composer_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(!app.launch.visible);
        assert!(!app.launch.return_to_session);
        assert_eq!(app.current_session_id.as_deref(), Some("retained-session"));
        assert_eq!(app.api_messages, messages);
        assert_eq!(app.history.len(), history_len);
        assert_eq!(app.input, "unsent draft");
        assert_eq!(app.launch.mark_reveal_started_at, reveal);
        assert!(!crate::tui::widgets::should_render_empty_state(&app));

        for command in ["/overview", "/stats"] {
            let result = crate::commands::execute(command, &mut app);
            assert!(!result.is_error);
            assert!(result.message.unwrap().contains("Quick Actions"));
            assert!(!app.launch.visible);
        }
    }

    #[test]
    fn home_refuses_to_cover_active_work() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options("."),
        );
        app.launch.dismiss();
        app.is_loading = true;
        let result = crate::commands::execute("/home", &mut app);
        assert!(result.is_error);
        assert!(!app.launch.visible);
    }
}

fn workspace_switch(app: &mut App, arg: Option<&str>) -> CommandResult {
    let input = arg
        .map(|arg| format!("/workspace {arg}"))
        .unwrap_or_else(|| "/workspace".into());
    crate::commands::execute(&input, app)
}

#[test]
fn workspace_without_arg_shows_current_workspace() {
    let mut app = create_test_app();
    let result = workspace_switch(&mut app, None);
    let msg = result.message.expect("workspace should be shown");
    assert!(msg.contains("Current workspace:"));
    assert!(msg.contains("/tmp/test-workspace"));
    assert!(result.action.is_none());
    let home = crate::config::effective_home_dir().expect("hermetic home");
    let result = workspace_switch(&mut app, Some("~"));
    assert!(
        matches!(result.action, Some(AppAction::SwitchWorkspace { workspace }) if workspace == home.canonicalize().unwrap_or(home.clone()))
    );
    let child = tempfile::tempdir_in(&home).unwrap();
    let input = format!("~/{}", child.path().file_name().unwrap().to_str().unwrap());
    let result = workspace_switch(&mut app, Some(&input));
    assert!(
        matches!(result.action, Some(AppAction::SwitchWorkspace { workspace }) if workspace == child.path().canonicalize().unwrap())
    );
}

#[test]
fn workspace_existing_absolute_dir_returns_switch_action() {
    let mut app = create_test_app();
    let dir = tempdir().expect("temp dir");
    let result = workspace_switch(&mut app, Some(dir.path().to_str().unwrap()));
    assert!(matches!(
        result.action,
        Some(AppAction::SwitchWorkspace { workspace }) if workspace == dir.path().canonicalize().unwrap()
    ));
}

#[test]
fn workspace_relative_dir_resolves_from_current_workspace() {
    let root = tempdir().expect("temp dir");
    let child = root.path().join("child");
    std::fs::create_dir(&child).expect("child dir");
    let mut app = create_test_app();
    app.workspace = root.path().to_path_buf();

    let result = workspace_switch(&mut app, Some("child"));
    assert!(matches!(
        result.action,
        Some(AppAction::SwitchWorkspace { workspace }) if workspace == child.canonicalize().unwrap()
    ));
}

#[test]
fn workspace_rejects_missing_path() {
    let mut app = create_test_app();
    let result = workspace_switch(&mut app, Some("definitely-missing"));
    assert!(result.is_error);
    assert!(result.message.unwrap().contains("does not exist"));
}

#[test]
fn workspace_rejects_file_path() {
    let root = tempdir().expect("temp dir");
    let file = root.path().join("file.txt");
    std::fs::write(&file, "not a directory").expect("test file");
    let mut app = create_test_app();

    let result = workspace_switch(&mut app, Some(file.to_str().unwrap()));
    assert!(result.is_error);
    assert!(result.message.unwrap().contains("not a directory"));
}

#[cfg(test)]
mod anchor {
    use crate::commands::CommandResult;
    use crate::config::Config;
    use crate::tui::app::{App, TuiOptions};
    use tempfile::TempDir;

    fn anchor(app: &mut App, arg: Option<&str>) -> CommandResult {
        let input = arg
            .map(|arg| format!("/anchor {arg}"))
            .unwrap_or_else(|| "/anchor".into());
        crate::commands::execute(&input, app)
    }

    enum AnchorFault {
        Read,
        Open,
        Write,
        Replace,
    }

    impl codewhale_command_contract::facets::CommandWorkspaceContext for AnchorFault {
        fn workspace(&self) -> std::path::PathBuf {
            unreachable!("unexpected workspace observation")
        }
        fn work_state_snapshot(&self) -> Result<Option<String>, String> {
            unreachable!("unexpected work observation")
        }
        fn operation_digest(&mut self) -> Result<String, String> {
            unreachable!("unexpected work observation")
        }
        fn effective_home_dir(&self) -> Option<std::path::PathBuf> {
            unreachable!("unexpected home observation")
        }
        fn inspect_workspace_target(
            &self,
            _: &std::path::Path,
        ) -> codewhale_command_contract::navigation::WorkspaceTarget {
            unreachable!("unexpected path inspection")
        }
        fn read_anchor_document(&self) -> Result<Option<String>, String> {
            match self {
                Self::Read => Err("read denied".into()),
                Self::Replace => Ok(Some("first\n---\nsecond".into())),
                _ => unreachable!("append must not read the document"),
            }
        }
        fn append_anchor_bytes(
            &mut self,
            parts: &[&[u8]],
        ) -> Result<std::path::PathBuf, codewhale_command_contract::navigation::AnchorWriteError>
        {
            use codewhale_command_contract::navigation::AnchorWriteError;
            let expected: &[&[u8]] = &[b"\n---\n", b"entry", b"\n"];
            assert_eq!(parts, expected);
            match self {
                Self::Open => Err(AnchorWriteError::Open("open denied".into())),
                Self::Write => Err(AnchorWriteError::Write("partial write".into())),
                _ => unreachable!("unexpected append"),
            }
        }
        fn replace_anchor_document(&mut self, bytes: &[u8]) -> Result<(), String> {
            assert!(matches!(self, Self::Replace));
            assert_eq!(bytes, b"second");
            Err("replace denied".into())
        }
    }

    fn fault_result(fault: &mut AnchorFault, arg: &str) -> CommandResult {
        use codewhale_command_contract::handler::{CommandContexts, CommandHandler};
        let CommandHandler::Contextual { handler, .. } = crate::commands::registry()
            .get("anchor")
            .unwrap()
            .contextual_handler()
            .unwrap()
        else {
            panic!("anchor must be contextual");
        };
        handler(CommandContexts::empty().with_workspace(fault), Some(arg))
    }

    fn create_test_app_with_tmpdir(tmpdir: &TempDir) -> App {
        let options = TuiOptions {
            skills_dir: tmpdir.path().join("skills"),
            memory_path: tmpdir.path().join("memory.md"),
            notes_path: tmpdir.path().join("notes.txt"),
            mcp_config_path: tmpdir.path().join("mcp.json"),
            ..crate::tui::test_support::test_tui_options(tmpdir.path())
        };
        App::new(options, &Config::default())
    }

    #[cfg(unix)]
    #[test]
    fn confined_anchors_refuse_linked_files_and_directories() {
        use std::os::unix::fs::symlink;

        for directory in [".codewhale", ".deepseek"] {
            for linked_directory in [false, true] {
                let workspace = TempDir::new().unwrap();
                let outside = TempDir::new().unwrap();
                let mut app = create_test_app_with_tmpdir(&workspace);
                let original = "first anchor\n---\nsecond anchor";
                let target = outside.path().join("anchors.md");
                std::fs::write(&target, original).unwrap();
                let parent = workspace.path().join(directory);
                if linked_directory {
                    symlink(outside.path(), &parent).unwrap();
                } else {
                    std::fs::create_dir_all(&parent).unwrap();
                    symlink(&target, parent.join("anchors.md")).unwrap();
                }
                for command in ["list", "next anchor", "remove 1"] {
                    assert!(anchor(&mut app, Some(command)).is_error);
                    assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
                }
                // Exercise the truncate path independently of the read refusal.
                {
                    let mut bundle = app.command_contexts();
                    let parts = bundle
                        .contexts(
                            codewhale_command_contract::handler::CommandCapabilities::WORKSPACE,
                        )
                        .into_parts();
                    assert!(
                        parts
                            .workspace
                            .unwrap()
                            .replace_anchor_document(&[])
                            .is_err()
                    );
                }
                assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn confined_anchors_refuse_dangling_links() {
        for directory in [".codewhale", ".deepseek"] {
            let workspace = TempDir::new().unwrap();
            let outside = TempDir::new().unwrap();
            let mut app = create_test_app_with_tmpdir(&workspace);
            let target = outside.path().join("missing.md");
            let parent = workspace.path().join(directory);
            std::fs::create_dir_all(&parent).unwrap();
            std::os::unix::fs::symlink(&target, parent.join("anchors.md")).unwrap();
            assert!(anchor(&mut app, Some("list")).is_error);
            assert!(anchor(&mut app, Some("next anchor")).is_error);
            {
                let mut bundle = app.command_contexts();
                let parts = bundle
                    .contexts(codewhale_command_contract::handler::CommandCapabilities::WORKSPACE)
                    .into_parts();
                assert!(
                    parts
                        .workspace
                        .unwrap()
                        .replace_anchor_document(&[])
                        .is_err()
                );
            }
            assert!(!target.exists());
        }
    }

    #[test]
    fn test_anchor_without_content_returns_error() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let result = anchor(&mut app, None);
        assert!(result.is_error);
        assert!(result.message.unwrap().contains("Usage:"));
    }

    #[test]
    fn test_anchor_with_empty_content_returns_error() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let result = anchor(&mut app, Some("   "));
        assert!(result.is_error);
        assert!(result.message.unwrap().contains("Usage:"));
    }

    #[test]
    fn test_anchor_add() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let result = anchor(&mut app, Some("API status field is unreliable"));
        assert!(!result.is_error);
        assert!(result.message.unwrap().contains("Anchor pinned"));

        let path = tmpdir.path().join(".deepseek").join("anchors.md");
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("API status field is unreliable"));
        for (mut fault, expected) in [
            (
                AnchorFault::Open,
                "Error: Failed to open anchors file: open denied",
            ),
            (
                AnchorFault::Write,
                "Error: Failed to write anchor: partial write",
            ),
        ] {
            let result = fault_result(&mut fault, "entry");
            assert!(result.is_error && result.action.is_none());
            assert_eq!(result.message.as_deref(), Some(expected));
        }
    }

    #[test]
    fn test_anchor_list_empty() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let result = anchor(&mut app, Some("list"));
        assert!(!result.is_error);
        assert!(result.message.unwrap().contains("No anchors set"));
        let result = fault_result(&mut AnchorFault::Read, "list");
        assert!(result.is_error && result.action.is_none());
        assert_eq!(
            result.message.as_deref(),
            Some("Error: Failed to read anchors file: read denied")
        );
    }

    #[test]
    fn test_anchor_list_with_items() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        anchor(&mut app, Some("First anchor"));
        anchor(&mut app, Some("Second anchor"));

        let result = anchor(&mut app, Some("list"));
        let msg = result.message.unwrap();
        assert!(msg.contains("2 total"));
        assert!(msg.contains("1. First anchor"));
        assert!(msg.contains("2. Second anchor"));
    }

    #[test]
    fn test_anchor_remove() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        anchor(&mut app, Some("First anchor"));
        anchor(&mut app, Some("Second anchor"));

        let result = anchor(&mut app, Some("remove 1"));
        assert!(!result.is_error);
        assert!(result.message.unwrap().contains("Removed anchor #1"));

        let result = anchor(&mut app, Some("list"));
        let msg = result.message.unwrap();
        assert!(msg.contains("1 total"));
        assert!(msg.contains("Second anchor"));
        assert!(!msg.contains("First anchor"));
        let result = fault_result(&mut AnchorFault::Replace, "remove 1");
        assert!(result.is_error && result.action.is_none());
        assert_eq!(
            result.message.as_deref(),
            Some("Error: Failed to write anchors file: replace denied")
        );
    }

    #[test]
    fn test_anchor_remove_invalid_index() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        anchor(&mut app, Some("Only anchor"));

        let result = anchor(&mut app, Some("remove 5"));
        assert!(result.is_error);
        assert!(result.message.unwrap().contains("does not exist"));
    }

    #[test]
    fn test_anchor_remove_non_numeric() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let result = anchor(&mut app, Some("remove abc"));
        assert!(result.is_error);
        assert!(result.message.unwrap().contains("Invalid index"));
    }
}
