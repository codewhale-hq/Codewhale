//! Public-host observations captured before FEAT-027 production migration.
//! Fixtures stay outside the portable command closure.
use crate::commands::{CommandResult, execute};
use crate::tui::app::{App, AppAction, TuiOptions};
use codewhale_config::ProviderKind;
use codewhale_localization::Locale;
use codewhale_runtime::config::Config;
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;

fn app(temp: &TempDir) -> App {
    let options = TuiOptions {
        skills_dir: temp.path().join("skills"),
        memory_path: temp.path().join("memory.md"),
        notes_path: temp.path().join("notes.txt"),
        mcp_config_path: temp.path().join("mcp.json"),
        ..crate::tui::test_support::test_tui_options(temp.path())
    };
    let mut app = App::new(options, &Config::default());
    app.config_path = Some(temp.path().join("config.toml"));
    app.ui_locale = Locale::En;
    app.api_provider = ProviderKind::Deepseek;
    app.sandbox_backend = None;
    app
}

fn normalize(message: &str, temp: &TempDir) -> String {
    let mut text = message.to_string();
    // The permission path is canonicalised by the config layer
    // (`normalize_config_file_path`): `/private/var/...` on macOS and
    // `\\?\C:\...` with the long name on Windows. Replace the canonical form
    // first — substituting the raw workspace prefix inside it would leave a
    // stray `/private` behind — then the raw form used by the fixture.
    let canonical = temp
        .path()
        .canonicalize()
        .unwrap_or_else(|_| temp.path().to_path_buf());
    for workspace in [canonical.as_path(), temp.path()] {
        text = text.replace(
            &codewhale_config::quote_os_path(&workspace.join("permissions.toml")),
            "\"<WORKSPACE>/permissions.toml\"",
        );
        text = text.replace(
            &codewhale_util::utils::display_path(workspace),
            "<WORKSPACE>",
        );
        if let Some(workspace) = workspace.to_str() {
            text = text.replace(workspace, "<WORKSPACE>");
        }
    }
    if let Some(audit) = codewhale_runtime::audit::audit_log_path() {
        text = text.replace(
            &codewhale_config::quote_os_path(&audit),
            "\"<TEST_AUDIT_LOG>\"",
        );
    }
    if let Some(home) = codewhale_paths::user_home() {
        text = text.replace(home.to_str().unwrap(), "<TEST_HOME>");
    }
    // Catalog freshness depends on the bundled snapshot's age against the
    // wall clock, so it cannot be part of a stable baseline.
    while let Some(start) = text.find(" · Catalog: ") {
        let Some(end) = text[start..].find(" · Cloud facts:") else {
            break;
        };
        text.replace_range(start..start + end, "");
    }
    text
}

fn observed(result: &CommandResult, temp: &TempDir) -> Value {
    json!({"message": result.message.as_deref().map(|m| normalize(m, temp)),
        "error": result.is_error, "action": format!("{:?}", result.action)})
}

fn golden(name: &str, actual: Value) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/commands/contract/fixtures/config_policy")
        .join(format!("{name}.json"));
    let expected: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(actual, expected, "public command baseline {name}");
}

#[test]
fn permissions_public_routes_errors_and_transaction_match_baseline() {
    let _env = crate::test_support::lock_test_env();
    let temp = TempDir::new().unwrap();
    let mut app = app(&temp);
    let path = temp.path().join("permissions.toml");
    let mut results = Vec::new();
    results.push(observed(&execute("/permissions", &mut app), &temp));
    fs::write(&path, "").unwrap();
    results.push(observed(&execute("/permissions status", &mut app), &temp));
    let original = "# preserve this header\n[[rules]]\ntool = \"exec_shell\"\ncommand = \"cargo test\"\ncommand_exact = true\naction = \"allow\"\n\n[[rules]]\ntool = \"edit_file\"\npath = \"src/lib.rs\"\naction = \"deny\"\n";
    fs::write(&path, original).unwrap();
    let listing = execute("/permissions list", &mut app);
    results.push(observed(&listing, &temp));
    for name in ["/permission-rules", "/permission_rules"] {
        assert_eq!(
            observed(&execute(name, &mut app), &temp),
            observed(&listing, &temp)
        );
    }
    for token in [
        "ask-rules",
        "ask_rules",
        "askrules",
        "rules",
        "permission-rules",
        "permission_rules",
        "permissions",
    ] {
        assert_eq!(
            observed(&execute(&format!("/config {token} list"), &mut app), &temp),
            observed(&listing, &temp)
        );
    }
    for args in [
        "unknown",
        "remove",
        "remove bad",
        "remove 0",
        "remove 99",
        "remove 1 extra",
        "remove 1 --bad token",
    ] {
        let result = execute(&format!("/permissions {args}"), &mut app);
        assert!(result.is_error);
        assert!(result.action.is_none());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        results.push(observed(&result, &temp));
    }
    let preview = execute("/permissions remove 1", &mut app);
    assert!(!preview.is_error);
    assert!(preview.action.is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    let command = preview
        .message
        .as_deref()
        .unwrap()
        .split('`')
        .find(|s| s.starts_with("/permissions remove 1 --confirm "))
        .unwrap()
        .to_string();
    let token = command.split_whitespace().last().unwrap();
    let mut preview_observed = observed(&preview, &temp);
    preview_observed["message"] = json!(
        normalize(preview.message.as_ref().unwrap(), &temp).replace(token, "<CONFIRM_TOKEN>")
    );
    results.push(preview_observed);
    let changed = format!("{original}\n# changed since preview\n");
    fs::write(&path, &changed).unwrap();
    let stale = execute(&command, &mut app);
    assert!(stale.is_error);
    assert!(stale.action.is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);
    results.push(observed(&stale, &temp));
    let preview = execute("/permissions remove 1", &mut app);
    let command = preview
        .message
        .as_deref()
        .unwrap()
        .split('`')
        .find(|s| s.starts_with("/permissions remove 1 --confirm "))
        .unwrap()
        .to_string();
    let removed = execute(&command, &mut app);
    assert!(!removed.is_error);
    assert_eq!(removed.action, Some(AppAction::PermissionRulesChanged));
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("# preserve this header"));
    let parsed: codewhale_config::PermissionsToml = toml::from_str(&persisted).unwrap();
    assert_eq!(parsed.rules.len(), 1);
    assert_eq!(parsed.rules[0].tool, "edit_file");
    results.push(observed(&removed, &temp));
    golden("permissions", json!(results));
}

#[test]
fn status_public_output_and_read_only_state_match_baseline() {
    let _env = crate::test_support::lock_test_env();
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("AGENTS.md"), "fixture instructions").unwrap();
    let mut app = app(&temp);
    app.current_session_id = Some("baseline-{model}-session".into());
    app.model = "baseline-{provider}-model".into();
    app.turn_counter = 4;
    app.session.total_tokens = 1234;
    app.session.last_prompt_tokens = Some(100);
    app.session.last_completion_tokens = Some(25);
    let before = (
        app.model.clone(),
        app.current_session_id.clone(),
        app.turn_counter,
        app.session.total_tokens,
    );
    let first = execute("/status", &mut app);
    assert!(!first.is_error);
    assert!(first.action.is_none());
    let second = execute("/status ignored-arguments", &mut app);
    assert_eq!(observed(&first, &temp), observed(&second, &temp));
    assert_eq!(
        before,
        (
            app.model.clone(),
            app.current_session_id.clone(),
            app.turn_counter,
            app.session.total_tokens
        )
    );
    assert!(!temp.path().join("config.toml").exists());
    golden("status", observed(&first, &temp));
}
