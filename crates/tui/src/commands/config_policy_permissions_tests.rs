//! Existing permission host regressions, moved out of the portable closure.
use crate::commands::config_policy_host::permissions as permissions_command;
use crate::commands::contract::config_policy::rule_applies_in_workspace;
use crate::tui::app::{App, AppAction};
use codewhale_config::ToolAskRule;
use codewhale_localization::{MessageId, tr};

mod tests {
    use std::fs;

    use crate::tui::app::TuiOptions;
    use codewhale_localization::Locale;

    use super::*;

    fn test_app(config_path: std::path::PathBuf, workspace: std::path::PathBuf) -> App {
        let config = codewhale_runtime::config::Config::default();
        let mut app = App::new(
            TuiOptions {
                workspace,
                ..crate::tui::test_support::test_tui_options(std::path::PathBuf::from("."))
            },
            &config,
        );
        app.config_path = Some(config_path);
        app.ui_locale = Locale::En;
        app
    }

    #[test]
    fn list_shows_source_scope_matcher_and_workspace_applicability() {
        let dir = tempfile::tempdir().expect("tempdir");
        let other = tempfile::tempdir().expect("other tempdir");
        let config_path = dir.path().join("config.toml");
        let permissions_path = dir.path().join("permissions.toml");
        fs::write(
            &permissions_path,
            format!(
                r#"
[[rules]]
tool = "exec_shell"
command = "cargo test"
command_exact = true
workspace = {workspace:?}
action = "allow"

[[rules]]
tool = "edit_file"
path = "src/lib.rs"
workspace = {other:?}
"#,
                workspace = dir.path().to_string_lossy(),
                other = other.path().to_string_lossy(),
            ),
        )
        .expect("write permissions");
        let displayed_permissions_path =
            codewhale_config::resolve_permissions_path(Some(config_path.clone()))
                .expect("resolve permissions path");
        let mut app = test_app(config_path, dir.path().to_path_buf());

        let result = permissions_command(&mut app, Some("list"));
        let message = result.message.expect("list message");

        assert!(!result.is_error);
        assert!(message.contains(&codewhale_config::quote_os_path(
            &displayed_permissions_path
        )));
        assert!(message.contains("#1 | allow | exec_shell"));
        assert!(message.contains("exact command `cargo test`"));
        assert!(message.contains("active in this workspace"));
        assert!(message.contains("#2 | ask | edit_file"));
        assert!(message.contains("exact normalized path `src/lib.rs`"));
        assert!(message.contains("not active in this workspace"));
    }

    #[test]
    fn list_preserves_missing_empty_and_malformed_diagnostics() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        let permissions_path = dir.path().join("permissions.toml");
        let displayed_permissions_path =
            codewhale_config::resolve_permissions_path(Some(config_path.clone()))
                .expect("resolve permissions path");
        let mut app = test_app(config_path, dir.path().to_path_buf());

        let missing = permissions_command(&mut app, None);
        let missing_message = missing.message.expect("missing message");
        assert!(!missing.is_error);
        assert!(missing_message.contains("File status: missing"));
        assert!(missing_message.contains("Rule count: 0"));

        fs::write(&permissions_path, "").expect("write empty permissions");
        let empty = permissions_command(&mut app, Some("status"));
        let empty_message = empty.message.expect("empty message");
        assert!(!empty.is_error);
        assert!(empty_message.contains("File status: empty"));

        fs::write(
            &permissions_path,
            "[[rules]]\ntool = \"do-not-echo-this\"\ncommand = ",
        )
        .expect("write malformed permissions");
        let malformed = permissions_command(&mut app, Some("list"));
        let malformed_message = malformed.message.expect("malformed message");
        assert!(malformed.is_error);
        assert!(malformed_message.contains("Could not read or change permission rules"));
        assert!(malformed_message.contains(&codewhale_config::quote_os_path(
            &displayed_permissions_path
        )));
        assert!(malformed_message.contains("file contents were omitted"));
        assert!(!malformed_message.contains("do-not-echo-this"));
    }

    #[test]
    fn remove_requires_preview_token_then_emits_live_reload_action() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        let permissions_path = dir.path().join("permissions.toml");
        let original = "[[rules]]\ntool = \"exec_shell\"\ncommand = \"cargo test\"\n";
        fs::write(&permissions_path, original).expect("write permissions");
        let mut app = test_app(config_path, dir.path().to_path_buf());

        let preview = permissions_command(&mut app, Some("remove 1"));
        let preview_message = preview.message.expect("preview message");
        assert!(!preview.is_error);
        assert_eq!(
            fs::read_to_string(&permissions_path).expect("read previewed permissions"),
            original
        );
        let confirm_command = preview_message
            .split('`')
            .find(|part| part.starts_with("/permissions remove 1 --confirm "))
            .expect("confirmation command");
        let confirm_arg = confirm_command
            .strip_prefix("/permissions ")
            .expect("command prefix");

        let confirmed = permissions_command(&mut app, Some(confirm_arg));

        assert!(!confirmed.is_error);
        assert_eq!(confirmed.action, Some(AppAction::PermissionRulesChanged));
        let persisted = fs::read_to_string(&permissions_path).expect("read edited permissions");
        let parsed: codewhale_config::PermissionsToml =
            toml::from_str(&persisted).expect("parse edited permissions");
        assert!(parsed.rules.is_empty());
    }

    #[test]
    fn legacy_config_ask_rules_entry_uses_the_permissions_editor_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        fs::write(
            dir.path().join("permissions.toml"),
            "[[rules]]\ntool = \"exec_shell\"\ncommand = \"cargo test\"\n",
        )
        .expect("write permissions");
        let mut app = test_app(config_path, dir.path().to_path_buf());

        let result = {
            let contract = crate::commands::groups::config::config::config(
                app.command_contexts_with_config(None)
                    .contexts(crate::commands::groups::config::config::CONFIG_CAPS),
                Some("ask-rules list"),
            );
            crate::commands::CommandResult {
                message: contract.message,
                action: contract
                    .action
                    .map(crate::commands::config_host::config_action_to_app_action),
                is_error: contract.is_error,
            }
        };
        let message = result.message.expect("compatibility list message");

        assert!(!result.is_error);
        assert!(message.contains("Permission rules"));
        assert!(message.contains("#1 | ask | exec_shell"));
    }

    #[test]
    fn permissions_command_is_registered_with_compatibility_aliases() {
        let info = crate::commands::get_command_info("permissions").expect("permissions command");

        assert_eq!(info.name, "permissions");
        assert!(info.aliases.contains(&"permission-rules"));
        assert!(info.usage.contains("remove <rule-number>"));
    }

    #[test]
    fn invalid_workspace_scopes_never_appear_active() {
        let mut rule = ToolAskRule::exec_shell("cargo test");
        rule.workspace = Some("../not-an-absolute-scope".to_string());

        assert!(!rule_applies_in_workspace(
            &rule,
            std::path::Path::new("also-relative")
        ));
    }

    #[test]
    fn permission_messages_keep_placeholder_parity_across_complete_locales() {
        let ids = [
            MessageId::PermissionsListHeader,
            MessageId::PermissionsRuleEntry,
            MessageId::PermissionsMatchExactCommand,
            MessageId::PermissionsMatchCommandPrefix,
            MessageId::PermissionsMatchExactPath,
            MessageId::PermissionsScopeRepo,
            MessageId::PermissionsRemovePreview,
            MessageId::PermissionsRemoved,
            MessageId::PermissionsRuleNotFound,
            MessageId::PermissionsOperationFailed,
        ];
        for id in ids {
            let english = placeholders(&tr(Locale::En, id));
            for locale in Locale::shipped_complete() {
                assert_eq!(
                    placeholders(&tr(*locale, id)),
                    english,
                    "{} {id:?} placeholder drift",
                    locale.tag()
                );
            }
        }
    }

    fn placeholders(message: &str) -> std::collections::BTreeSet<String> {
        message
            .split('{')
            .skip(1)
            .filter_map(|suffix| suffix.split_once('}').map(|(name, _)| name.to_string()))
            .collect()
    }
}
