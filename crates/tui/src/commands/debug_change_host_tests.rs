//! Preserved changelog parsing and host parity tests, outside the portable group.

use crate::commands::CommandResult;
use crate::tui::app::AppAction;
use codewhale_commands::debug::change::*;
use codewhale_localization::{MessageId, tr};
const CODEWHALE_CHANGELOG: &str = include_str!("../../CHANGELOG.md");
fn change(app: &mut App, version: Option<&str>) -> CommandResult {
    crate::commands::execute(
        &version.map_or_else(|| "/change".to_string(), |arg| format!("/change {arg}")),
        app,
    )
}
use crate::test_support::{EnvVarGuard, lock_test_env};
use crate::tui::app::{App, TuiOptions};
use codewhale_localization::Locale;
use codewhale_runtime::config::Config;
fn make_app(tmpdir: &tempfile::TempDir, locale: Locale, has_api_key: bool) -> App {
    let mut config = Config::default();
    if has_api_key {
        config.set_legacy_root(Some("test-key".to_string()), None);
    }
    let mut app = App::new(
        TuiOptions {
            skills_dir: tmpdir.path().join("skills"),
            memory_path: tmpdir.path().join("memory.md"),
            notes_path: tmpdir.path().join("notes.txt"),
            mcp_config_path: tmpdir.path().join("mcp.json"),
            ..crate::tui::test_support::test_tui_options(tmpdir.path())
        },
        &config,
    );
    app.ui_locale = locale;
    app.api_provider = codewhale_config::ProviderKind::Deepseek;
    app.model_ids_passthrough = false;
    app.onboarding_needs_api_key = !has_api_key;
    app
}

#[test]
fn extract_latest_section_finds_first_version() {
    let content = "\n\
## [0.8.26] - 2026-05-09\n\
\n\
A security + polish release.\n\
\n\
### Fixed\n\
\n\
- Fixed something\n\
\n\
## [0.8.25] - 2026-05-09\n\
\n\
A stabilization release.\n";
    let section = extract_latest_changelog_section(content).expect("should find a section");
    assert!(section.contains("0.8.26"));
    assert!(section.contains("Fixed something"));
    assert!(!section.contains("0.8.25"));
}

#[test]
fn extract_latest_section_handles_0_8_29_style_fixture() {
    let content = "\n\
# Changelog\n\
\n\
## [0.8.29] - 2026-05-11\n\
\n\
Release candidate polish.\n\
\n\
### Added\n\
- New note-management command.\n\
\n\
## [0.8.28] - 2026-05-10\n\
\n\
Previous release.\n";
    let section = extract_latest_changelog_section(content).expect("should find a section");
    assert!(section.contains("0.8.29"));
    assert!(section.contains("2026-05-11"));
    assert!(section.contains("New note-management command"));
    assert!(!section.contains("0.8.28"));
}

#[test]
fn extract_latest_section_returns_none_for_empty_content() {
    assert!(extract_latest_changelog_section("").is_none());
}

#[test]
fn extract_latest_section_returns_none_for_no_version_headers() {
    let content = "# Just a heading\n\nSome text\n";
    assert!(extract_latest_changelog_section(content).is_none());
}

#[test]
fn extract_latest_section_handles_single_version() {
    let content = "\n## [0.8.26] - 2026-05-09\n\nOnly one version.\n";
    let section = extract_latest_changelog_section(content).expect("should find a section");
    assert!(section.contains("0.8.26"));
    assert!(section.contains("Only one version"));
}

#[test]
fn extract_latest_section_handles_subheadings() {
    let content = "\n\
## [0.8.26] - 2026-05-09\n\
\n\
### Added\n\
- New feature A\n\
\n\
### Fixed\n\
- Fixed bug B\n\
\n\
## [0.8.25] - 2026-05-09\n\
";
    let section = extract_latest_changelog_section(content).expect("should find a section");
    assert!(section.contains("New feature A"));
    assert!(section.contains("Fixed bug B"));
    assert!(!section.contains("0.8.25"));
}

#[test]
fn change_uses_bundled_release_notes_without_workspace_changelog() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut app = make_app(&tmp, Locale::En, false);
    let result = change(&mut app, None);
    assert!(!result.is_error);
    let msg = result.message.expect("should have a message");
    let expected = extract_latest_changelog_section(CODEWHALE_CHANGELOG)
        .expect("bundled changelog should have a release section");
    assert!(msg.contains(expected.lines().next().unwrap()));
}

#[test]
fn change_ignores_workspace_changelog() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("CHANGELOG.md"),
        "\n## [9.9.9] - 2099-01-01\n\nWorkspace changelog.\n",
    )
    .unwrap();
    let mut app = make_app(&tmp, Locale::En, false);
    let result = change(&mut app, None);
    assert!(!result.is_error);
    let msg = result.message.expect("should have a message");
    assert!(!msg.contains("9.9.9"));
    assert!(!msg.contains("Workspace changelog"));
}

#[test]
fn change_in_english_returns_message_without_action() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut app = make_app(&tmp, Locale::En, true);
    let result = change(&mut app, None);
    assert!(!result.is_error);
    let msg = result.message.expect("should have a message");
    let expected = extract_latest_changelog_section(CODEWHALE_CHANGELOG)
        .expect("bundled changelog should have a release section");
    assert!(msg.contains(expected.lines().next().unwrap()));
    assert!(
        result.action.is_none(),
        "English locale should not send translation"
    );
}

#[test]
fn change_in_non_english_also_sends_translation_action() {
    for (locale, _label) in [
        (Locale::ZhHans, "zh-Hans"),
        (Locale::Ja, "ja"),
        (Locale::PtBr, "pt-BR"),
    ] {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut app = make_app(&tmp, locale, true);
        let result = change(&mut app, None);
        assert!(!result.is_error, "Failed for locale {locale:?}");
        let msg = result.message.expect("should have a message");
        assert!(msg.contains(&*tr(locale, MessageId::CmdChangeTranslationQueued)));
        assert!(
            matches!(result.action, Some(AppAction::SendMessage(_))),
            "Non-English locale should send translation, got {:?}",
            result.action
        );
        if let Some(AppAction::SendMessage(prompt)) = &result.action {
            let expected = extract_latest_changelog_section(CODEWHALE_CHANGELOG)
                .expect("bundled changelog should have a release section");
            assert!(prompt.contains(expected.lines().next().unwrap()));
            let prev_ver = extract_previous_version_number(CODEWHALE_CHANGELOG)
                .expect("bundled changelog should have a previous release");
            assert!(
                prompt.contains(&prev_ver),
                "translation prompt should include previous-version hint: {prompt}"
            );
        }
    }
}

#[test]
fn change_in_non_english_without_api_key_uses_explicit_fallback() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _lock = lock_test_env();
    let _config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", tmp.path().join("config.toml"));
    let _deepseek_key = EnvVarGuard::remove("DEEPSEEK_API_KEY");
    let _deepseek_provider = EnvVarGuard::remove("DEEPSEEK_PROVIDER");
    let _codewhale_provider = EnvVarGuard::remove("CODEWHALE_PROVIDER");
    let mut app = make_app(&tmp, Locale::ZhHans, false);
    let result = change(&mut app, None);
    assert!(!result.is_error);
    let msg = result.message.expect("should have a message");
    assert!(msg.contains(&*tr(
        Locale::ZhHans,
        MessageId::CmdChangeTranslationUnavailable
    )));
    assert!(
        result.action.is_none(),
        "missing API key should not send translation"
    );
}

#[test]
fn change_in_non_english_offline_uses_explicit_fallback() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut app = make_app(&tmp, Locale::Ja, true);
    app.offline_mode = true;
    let result = change(&mut app, None);
    assert!(!result.is_error);
    let msg = result.message.expect("should have a message");
    assert!(msg.contains(&*tr(Locale::Ja, MessageId::CmdChangeTranslationUnavailable)));
    assert!(
        result.action.is_none(),
        "offline mode should not send translation"
    );
}

#[test]
fn extract_latest_ignores_lines_before_first_version() {
    let content = "\n\
# Changelog\n\
\n\
Some intro text.\n\
\n\
## [0.8.26] - 2026-05-09\n\
\n\
Content\n\
";
    let section = extract_latest_changelog_section(content).expect("should find a section");
    assert!(section.contains("0.8.26"));
    assert!(!section.contains("Changelog"));
    assert!(!section.contains("intro text"));
}

#[test]
fn extract_latest_skips_empty_unreleased_section() {
    let content = "\n\
## [Unreleased]\n\
\n\
## [0.8.32] - 2026-05-12\n\
\n\
A release with content.\n\
\n\
### Fixed\n\
- Something fixed\n\
\n\
## [0.8.31] - 2026-05-11\n\
\n\
Previous release.\n";
    let section = extract_latest_changelog_section(content).expect("should skip Unreleased");
    assert!(section.contains("0.8.32"));
    assert!(section.contains("Something fixed"));
    assert!(!section.contains("Unreleased"));
    assert!(!section.contains("0.8.31"));
}

#[test]
fn extract_latest_skips_entirely_empty_unreleased() {
    // `## [Unreleased]` followed immediately by the next version heading.
    let content = "\n\
## [Unreleased]\n\
## [0.8.32] - 2026-05-12\n\
\n\
Content here.\n";
    let section = extract_latest_changelog_section(content).expect("should find 0.8.32");
    assert!(section.contains("0.8.32"));
    assert!(!section.contains("Unreleased"));
}

#[test]
fn extract_latest_returns_none_when_all_sections_empty() {
    let content = "\n\
## [Unreleased]\n\
## [Future]\n";
    assert!(extract_latest_changelog_section(content).is_none());
}

#[test]
fn extract_latest_skips_multiple_empty_sections() {
    let content = "\n\
## [Unreleased]\n\
\n\
## [Next]\n\
\n\
## [0.8.32] - 2026-05-12\n\
\n\
Real content.\n";
    let section = extract_latest_changelog_section(content).expect("should find 0.8.32");
    assert!(section.contains("0.8.32"));
    assert!(section.contains("Real content"));
}

#[test]
fn extract_by_version_finds_exact_version() {
    let content = "\n\
## [0.8.32] - 2026-05-12\n\
\n\
Release content.\n\
\n\
## [0.8.31] - 2026-05-11\n\
\n\
Earlier release.\n";
    let section =
        extract_changelog_section_by_version(content, "0.8.31").expect("should find 0.8.31");
    assert!(section.contains("0.8.31"));
    assert!(section.contains("Earlier release"));
    assert!(!section.contains("0.8.32"));
}

#[test]
fn extract_by_version_returns_none_for_missing_version() {
    let content = "\n\
## [0.8.32] - 2026-05-12\n\
\n\
Content.\n";
    assert!(extract_changelog_section_by_version(content, "9.9.9").is_none());
}

#[test]
fn extract_by_version_finds_version_without_date() {
    let content = "\n\
## [Unreleased]\n\
\n\
Nothing.\n";
    let section = extract_changelog_section_by_version(content, "Unreleased")
        .expect("should find Unreleased");
    assert!(section.contains("Unreleased"));
    assert!(section.contains("Nothing"));
}

#[test]
fn extract_by_version_respects_empty_sections() {
    // `## [0.8.32]` is empty, should return None for it
    let content = "\n\
## [0.8.32] - 2026-05-12\n\
## [0.8.31] - 2026-05-11\n\
\n\
Content.\n";
    assert!(extract_changelog_section_by_version(content, "0.8.32").is_none());
}

#[test]
fn change_with_version_arg_shows_older_release() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut app = make_app(&tmp, Locale::En, false);
    let result = change(&mut app, Some("0.8.1"));
    // 0.8.1 is a very old release; if it exists, the result should not be an error.
    // If that exact version doesn't exist in the bundled changelog, we still
    // expect a proper error message referencing the version.
    if result.is_error {
        let msg = result.message.as_deref().unwrap_or("");
        assert!(msg.contains("0.8.1"), "error should mention version: {msg}");
    } else {
        let msg = result.message.expect("should have a message");
        assert!(msg.contains("0.8.1"));
    }
}

#[test]
fn change_with_empty_version_arg_acts_as_default() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut app = make_app(&tmp, Locale::En, false);
    let result_default = change(&mut app, None);
    assert!(!result_default.is_error);

    let mut app2 = make_app(&tmp, Locale::En, false);
    let result_empty = change(&mut app2, Some(""));
    assert!(!result_empty.is_error);

    // Both should have the same message content
    let msg_default = result_default.message.as_deref().unwrap_or("");
    let msg_empty = result_empty.message.as_deref().unwrap_or("");
    assert_eq!(msg_default, msg_empty);
}

#[test]
fn change_with_nonexistent_version_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut app = make_app(&tmp, Locale::En, false);
    let result = change(&mut app, Some("99.99.99"));
    assert!(result.is_error);
    let msg = result.message.as_deref().unwrap_or("");
    assert!(
        msg.contains("99.99.99"),
        "error should mention version: {msg}"
    );
}

#[test]
fn extract_by_version_ignores_substring_matches() {
    let content =
        "\n## [0.8.1] - 2026-01-01\n\nContent A.\n\n## [0.8.10] - 2026-01-10\n\nContent B.\n";
    let section =
        extract_changelog_section_by_version(content, "0.8.1").expect("should find 0.8.1");
    assert!(section.contains("Content A"));
    assert!(!section.contains("Content B"));
}

// --- extract_previous_version_number tests ---

#[test]
fn prev_version_finds_second_heading() {
    let content = "\n\
## [0.8.32] - 2026-05-12\n\
\n\
Release content.\n\
\n\
## [0.8.31] - 2026-05-11\n\
\n\
Earlier release.\n";
    let prev = extract_previous_version_number(content).expect("should find 0.8.31");
    assert_eq!(prev, "0.8.31");
}

#[test]
fn prev_version_skips_empty_unreleased_section() {
    let content = "\n\
## [Unreleased]\n\
\n\
## [0.8.32] - 2026-05-12\n\
\n\
Actual release.\n\
\n\
## [0.8.31] - 2026-05-11\n\
\n\
Older release.\n";
    let prev =
        extract_previous_version_number(content).expect("should skip Unreleased and find 0.8.31");
    assert_eq!(prev, "0.8.31");
}

#[test]
fn prev_version_returns_none_for_single_version() {
    let content = "\n## [0.8.32] - 2026-05-12\n\nOnly one version.\n";
    assert!(extract_previous_version_number(content).is_none());
}

#[test]
fn prev_version_returns_none_for_empty_content() {
    assert!(extract_previous_version_number("").is_none());
}

#[test]
fn prev_version_returns_none_for_no_version_headers() {
    let content = "# Just a heading\n\nNo versions here.\n";
    assert!(extract_previous_version_number(content).is_none());
}

#[test]
fn prev_version_handles_adjacent_headings() {
    let content = "\n\
## [0.8.32] - 2026-05-12\n\
\n\
Content.\n\
## [0.8.31] - 2026-05-11\n\
\n\
Older content.\n";
    let prev = extract_previous_version_number(content)
        .expect("should find 0.8.31 even with no blank line after section");
    assert_eq!(prev, "0.8.31");
}

#[test]
fn prev_version_skips_multiple_empty_sections() {
    let content = "\n\
## [Unreleased]\n\
\n\
## [Future]\n\
\n\
## [0.8.32] - 2026-05-12\n\
\n\
Real release.\n\
\n\
## [0.8.31] - 2026-05-11\n\
\n\
Older release.\n";
    let prev = extract_previous_version_number(content)
        .expect("should skip Unreleased and Future, find 0.8.31");
    assert_eq!(prev, "0.8.31");
}

#[test]
fn prev_version_after_explicit_version_finds_next_older_release() {
    let content = "\n\
## [0.8.32] - 2026-05-12\n\
\n\
Current release.\n\
\n\
## [0.8.31] - 2026-05-11\n\
\n\
Requested release.\n\
\n\
## [0.8.30] - 2026-05-10\n\
\n\
Older release.\n";
    let prev = extract_previous_version_number_after_version(content, "0.8.31")
        .expect("should find 0.8.30");
    assert_eq!(prev, "0.8.30");
}

#[test]
fn prev_version_after_explicit_version_skips_empty_sections() {
    let content = "\n\
## [0.8.32] - 2026-05-12\n\
\n\
Current release.\n\
\n\
## [0.8.31] - 2026-05-11\n\
\n\
Requested release.\n\
\n\
## [Future]\n\
\n\
## [0.8.30] - 2026-05-10\n\
\n\
Older release.\n";
    let prev = extract_previous_version_number_after_version(content, "0.8.31")
        .expect("should skip Future and find 0.8.30");
    assert_eq!(prev, "0.8.30");
}

// --- change() output hint tests ---

#[test]
fn change_without_args_includes_previous_version_hint() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut app = make_app(&tmp, Locale::En, false);
    let result = change(&mut app, None);
    assert!(!result.is_error);
    let msg = result.message.expect("should have a message");
    // The previous version hint should be part of the output.
    // We can't assert an exact version number since the changelog changes,
    // but the hint message key should appear.
    assert!(
        msg.contains("Previous version:") || msg.contains("run `/change"),
        "expected previous-version hint in output, got: {msg}"
    );
}

#[test]
fn change_with_explicit_version_includes_previous_hint() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut app = make_app(&tmp, Locale::En, false);
    // Derive versions from the bundled changelog: it only embeds a recent
    // slice of releases, so hardcoded versions would age out of it.
    let explicit = extract_previous_version_number(CODEWHALE_CHANGELOG)
        .expect("bundled changelog should have a previous release");
    let expected_prev =
        extract_previous_version_number_after_version(CODEWHALE_CHANGELOG, &explicit)
            .expect("bundled changelog should have at least three releases");
    let result = change(&mut app, Some(&explicit));
    assert!(!result.is_error);
    let msg = result.message.as_deref().unwrap_or("");
    assert!(
        msg.contains("Previous version:") && msg.contains(&expected_prev),
        "explicit version should show previous-version hint: {msg}"
    );
}

#[test]
fn change_hint_uses_localized_template() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut app = make_app(&tmp, Locale::ZhHans, true);
    let result = change(&mut app, None);
    assert!(!result.is_error);
    let msg = result.message.expect("should have a message");
    // zh-Hans template: "上一个版本:"
    assert!(
        msg.contains("上一个版本"),
        "zh-Hans output should contain localized hint: {msg}"
    );
}

#[test]
fn change_hint_in_japanese() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut app = make_app(&tmp, Locale::Ja, true);
    let result = change(&mut app, None);
    assert!(!result.is_error);
    let msg = result.message.expect("should have a message");
    assert!(
        msg.contains("前のバージョン"),
        "ja output should contain localized hint: {msg}"
    );
}

#[test]
fn change_hint_in_portuguese() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut app = make_app(&tmp, Locale::PtBr, true);
    let result = change(&mut app, None);
    assert!(!result.is_error);
    let msg = result.message.expect("should have a message");
    assert!(
        msg.contains("Versão anterior"),
        "pt-BR output should contain localized hint: {msg}"
    );
}
