use super::*;
// `fs` was a cfg(test) import on the parent, and `Path` is now only used by
// the legacy/render seams. Both belong here.
use crate::tui::app::{App, TuiOptions};
use codewhale_localization::Locale;
use codewhale_runtime::config::Config;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

#[test]
fn extension_owner_report_escapes_every_plugin_controlled_field() {
    struct Presentation(Locale);
    impl CommandPresentationContext for Presentation {
        fn translate(&self, key: &str, replacements: &[(&str, &str)]) -> Result<String, String> {
            let id = crate::commands::contract::key_to_plugin_message_id(key).unwrap();
            let mut output = codewhale_localization::tr(self.0, id).to_string();
            for (name, value) in replacements {
                output = output.replace(&format!("{{{name}}}"), value);
            }
            Ok(output)
        }
    }
    let mut output = String::new();
    append_host_owner_report(
        &Presentation(Locale::En),
        &mut output,
        &codewhale_engine::extension_host::OwnerReport {
            state: Some(
                codewhale_engine::extension_host::registry::OwnerState::Failed(
                    "\u{1b}[31m<script>".into(),
                ),
            ),
            tools: vec!["[tool](https://example.invalid)".into()],
            diagnostics: vec!["\n# approved\u{202e}".into()],
        },
    );
    assert!(output.contains("Extension host:"));
    for value in [
        "\u{1b}[31m<script>",
        "[tool](https://example.invalid)",
        "\n# approved\u{202e}",
    ] {
        assert!(output.contains(&escape_review_text(value, ReviewTextContext::Prose)));
        assert!(!output.contains(value));
    }
    let mut localized = String::new();
    append_host_owner_report(
        &Presentation(Locale::ZhHans),
        &mut localized,
        &codewhale_engine::extension_host::OwnerReport {
            state: None,
            tools: vec![],
            diagnostics: vec![],
        },
    );
    assert_eq!(
        localized,
        "\n扩展宿主：\n  状态：未激活\n  活动工具（0）：—"
    );
}

#[test]
fn plugin_config_summary_lists_keys_never_values_and_escapes_them() {
    let mut output = String::new();
    append_plugin_config_summary(
        &mut output,
        "greeter",
        &Ok(vec!["greeting".to_string(), "\u{1b}[31mkey".to_string()]),
    );
    assert!(output.contains("[plugins.\"greeter\".config]"), "{output}");
    assert!(output.contains("values not shown"));
    assert!(output.contains("greeting"));
    assert!(output.contains(&escape_review_text(
        "\u{1b}[31mkey",
        ReviewTextContext::Prose
    )));
    assert!(!output.contains('\u{1b}'));

    let mut refused = String::new();
    append_plugin_config_summary(
        &mut refused,
        "greeter",
        &Err("config is 20000 bytes\n# approved".to_string()),
    );
    assert!(refused.contains("is refused: "), "{refused}");
    assert!(!refused.contains("\n# approved"), "{refused}");
}

fn create_test_app(root: &Path) -> (App, TempDir) {
    let temp = TempDir::new().expect("tempdir");
    let config_path = temp.path().join("config.toml");
    let tools_dir = root.join("tools");
    fs::create_dir_all(&tools_dir).unwrap();
    fs::write(
        &config_path,
        format!(
            "[tools]\nplugin_dir = {}\n",
            toml::Value::String(tools_dir.to_string_lossy().to_string())
        ),
    )
    .unwrap();
    let options = TuiOptions {
        config_path: Some(config_path),
        skills_dir: temp.path().join("skills"),
        memory_path: temp.path().join("memory.md"),
        notes_path: temp.path().join("notes.txt"),
        mcp_config_path: temp.path().join("mcp.json"),
        ..crate::tui::test_support::test_tui_options(root)
    };
    let config = {
        let __cw_config_init_0_0 = Some(codewhale_runtime::config::ToolsConfig {
            plugin_dir: Some(tools_dir.to_string_lossy().into_owned()),
            ..Default::default()
        });
        let mut __cw_config_init_0_value: Config = Default::default();
        __cw_config_init_0_value.tools = __cw_config_init_0_0;
        __cw_config_init_0_value
    };
    let discovery = codewhale_plugins::PluginDiscoveryContext::capture_pre_dotenv();
    let registry = discovery.registry_for_workspace(root);
    let mut app = App::new_with_plugin_registry(options, &config, registry);
    app.ui_locale = Locale::En;
    (app, temp)
}

fn write_bundle(root: &Path) {
    let bundle = root.join(".codewhale/plugins/demo");
    fs::create_dir_all(bundle.join("skills/hello")).unwrap();
    fs::write(
        bundle.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\ndescription = \"Import spreadsheet data safely\"\n[skills]\npath = \"skills\"\n",
    )
    .unwrap();
    fs::write(
        bundle.join("skills/hello/SKILL.md"),
        "---\nname: hello\ndescription: hello\n---\nbody\n",
    )
    .unwrap();
}

fn write_mcp_review_bundle(root: &Path) {
    let bundle = root.join(".codewhale/plugins/review-mcp");
    fs::create_dir_all(&bundle).unwrap();
    fs::write(bundle.join("server.js"), "// reviewed entrypoint\n").unwrap();
    fs::write(
        bundle.join("plugin.toml"),
        r#"schema_version = 1
[plugin]
name = "review-mcp"
version = "1.0.0"

[mcp_servers.local]
command = "node"
args = ["server.js", "--mode=worker", "-e", "console.log('ready')"]

[mcp_servers.local.env]
PLUGIN_TOKEN = "${PLUGIN_TOKEN_SOURCE}"

[mcp_servers.remote]
url = "https://example.invalid/mcp"
bearer_token_env_var = "REMOTE_TOKEN"

[mcp_servers.remote.env_headers]
X_Api_Key = "REMOTE_API_KEY"

[capabilities]
network_hosts = ["example.invalid"]
"#,
    )
    .unwrap();
}

#[test]
fn bare_plugin_command_opens_unified_extensions_modal() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
    let (mut app, _temp) = create_test_app(root.path());

    let result = plugins_with_kimi_home_override(&mut app, None, None);

    assert!(matches!(
        result.action,
        Some(AppAction::OpenExtensions {
            tab: crate::tui::views::extensions::ExtensionsTab::Plugins
        })
    ));
    assert!(result.message.is_none());
}

#[test]
fn list_show_validate_are_read_only_and_label_legacy_tools() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let codewhale_home = root.path().join("home");
    // A configured user has a home; that is the state in which the built-in
    // bundle is materialized and listed.
    fs::create_dir_all(&codewhale_home).unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    write_bundle(root.path());
    let (mut app, _temp) = create_test_app(root.path());
    fs::write(
        root.path().join("tools/greet.sh"),
        "# name: greet\n# description: hello\n",
    )
    .unwrap();
    // The app already resolved the legacy tools path during startup.
    // Read-only plugin commands must not reopen a credential-bearing
    // config file merely to inventory those tools.
    fs::write(
        app.config_path.as_ref().unwrap(),
        "api_key = [\"must-not-be-re-read\"\n",
    )
    .unwrap();
    let state_path = codewhale_home.join("plugins/state.json");

    for arg in [Some("list"), Some("show demo"), Some("validate")] {
        let result = plugins_with_kimi_home_override(&mut app, arg, None);
        assert!(!result.is_error, "{:?}", result.message);
        assert!(!state_path.exists(), "read-only command wrote plugin state");
    }
    let list = plugins_with_kimi_home_override(&mut app, Some("list"), None)
        .message
        .unwrap();
    assert!(list.contains("Plugin bundles (1)"), "{list}");
    assert!(list.contains("workspace · not-reviewed"), "{list}");
    assert!(list.contains("disabled"));
    assert!(list.contains("Legacy plugin tools (1)"));
}

#[test]
fn list_preserves_the_one_shot_on_disk_reload_nudge() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
    let (mut app, _temp) = create_test_app(root.path());

    // Mutate the on-disk catalog after discovery. Listing must report the
    // current-main nudge without rediscovering or changing trust state.
    write_bundle(root.path());
    let first = plugins_with_kimi_home_override(&mut app, Some("list"), None)
        .message
        .unwrap();
    assert!(
        first.contains(codewhale_plugins::PLUGIN_RELOAD_NUDGE),
        "{first}"
    );

    let second = plugins_with_kimi_home_override(&mut app, Some("list"), None)
        .message
        .unwrap();
    assert!(
        !second.contains(codewhale_plugins::PLUGIN_RELOAD_NUDGE),
        "nudge must appear once per catalog stamp: {second}"
    );
}

#[test]
fn suggest_ranks_installed_plugins_without_trusting_or_enabling_them() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let codewhale_home = root.path().join("home");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    write_bundle(root.path());
    let (mut app, _temp) = create_test_app(root.path());

    for arg in ["suggest", "suggest go"] {
        let result = plugins_with_kimi_home_override(&mut app, Some(arg), None);
        assert!(
            result.is_error,
            "expected usage error for {arg}: {result:?}"
        );
    }

    let result =
        plugins_with_kimi_home_override(&mut app, Some("suggest spreadsheet import"), None);
    assert!(!result.is_error, "{result:?}");
    let message = result.message.expect("suggestion message");
    assert!(message.contains("Suggested plugins"), "{message}");
    assert!(message.contains("demo — disabled"), "{message}");
    assert!(message.contains("Why:"), "{message}");
    assert!(message.contains("/plugin trust demo"), "{message}");
    assert!(
        message.contains("spreadsheet") || message.contains("import"),
        "{message}"
    );
    assert!(message.contains("Nothing was installed, trusted, or enabled."));
    assert!(!codewhale_home.join("plugins/state.json").exists());
    let plugin = app.plugin_registry.get("demo").expect("demo plugin");
    assert!(!plugin.enabled && !plugin.trusted());
}

#[test]
fn suggest_matches_manifest_keywords_for_a_named_integration() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let codewhale_home = root.path().join("home");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    let bundle = root.path().join(".codewhale/plugins/supabase");
    fs::create_dir_all(&bundle).unwrap();
    fs::write(
        bundle.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"supabase\"\nversion = \"1.0.0\"\ndescription = \"Hosted Postgres and auth\"\nkeywords = [\"supabase\", \"postgres\"]\n",
    )
    .unwrap();
    let (mut app, _temp) = create_test_app(root.path());

    let result = plugins_with_kimi_home_override(&mut app, Some("suggest add supabase auth"), None);
    assert!(!result.is_error, "{result:?}");
    let message = result.message.expect("suggestion message");
    assert!(message.contains("supabase"), "{message}");
    assert!(message.contains("/plugin trust supabase"), "{message}");
    assert!(message.contains("Nothing was installed, trusted, or enabled."));
}

#[test]
fn trust_requires_content_and_capability_bound_review_token() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
    write_bundle(root.path());
    let (mut app, _temp) = create_test_app(root.path());
    let enable_review = plugins_with_kimi_home_override(&mut app, Some("enable demo"), None);
    assert!(!enable_review.is_error);
    assert!(
        enable_review
            .message
            .as_deref()
            .is_some_and(|message| message.contains("/plugin trust demo "))
    );
    assert!(!app.plugin_registry.get("demo").unwrap().trusted());

    let review_result = plugins_with_kimi_home_override(&mut app, Some("trust demo"), None);
    let Some(AppAction::OpenCommandReview {
        content, command, ..
    }) = review_result.action
    else {
        panic!("trust opens a confirmation control");
    };
    assert!(
        !content.contains("/plugin trust demo "),
        "the hash belongs to the control"
    );
    let review = review_result.message.unwrap();
    let confirmation = review
        .lines()
        .find(|line| line.starts_with("/plugin trust demo "))
        .unwrap();
    assert_eq!(confirmation, command);
    let token = confirmation
        .split_whitespace()
        .last()
        .expect("review confirmation token");
    let (content_digest, capability_digest) = token
        .split_once('.')
        .expect("content and capability digests");
    assert_eq!(content_digest.len(), 64);
    assert_eq!(capability_digest.len(), 64);
    assert!(content_digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(
        capability_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    assert!(!app.plugin_registry.get("demo").unwrap().trusted());

    assert!(plugins_with_kimi_home_override(&mut app, Some("trust demo wrong"), None).is_error);
    let shortened = format!(
        "trust demo {}.{}",
        &content_digest[..12],
        &capability_digest[..12]
    );
    assert!(
        plugins_with_kimi_home_override(&mut app, Some(&shortened), None).is_error,
        "the legacy 48-bit content prefix must not authorize trust"
    );
    let arg = confirmation.trim_start_matches("/plugin ");
    assert!(!plugins_with_kimi_home_override(&mut app, Some(arg), None).is_error);
    assert!(!plugins_with_kimi_home_override(&mut app, Some("enable demo"), None).is_error);
    assert!(app.plugin_registry.is_active("demo"));
    assert!(!plugins_with_kimi_home_override(&mut app, Some("disable demo"), None).is_error);
    assert!(!app.plugin_registry.is_active("demo"));
}

fn write_mixed_bundle(root: &Path) {
    let bundle = root.join(".codewhale/plugins/mixed");
    fs::create_dir_all(bundle.join("skills/hello")).unwrap();
    fs::create_dir_all(bundle.join("commands")).unwrap();
    fs::create_dir_all(bundle.join("hooks")).unwrap();
    fs::create_dir_all(bundle.join("lsp")).unwrap();
    fs::write(
        bundle.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"mixed\"\nversion = \"1.0.0\"\n[skills]\npath = \"skills\"\n[commands]\npath = \"commands\"\n[hooks]\npath = \"hooks\"\n[lsp]\npath = \"lsp\"\n",
    )
    .unwrap();
    fs::write(
        bundle.join("skills/hello/SKILL.md"),
        "---\nname: hello\ndescription: hello\n---\nbody\n",
    )
    .unwrap();
}

#[test]
fn mixed_bundle_review_and_enable_keep_supported_components_active() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
    write_mixed_bundle(root.path());
    let (mut app, _temp) = create_test_app(root.path());

    let list = plugins_with_kimi_home_override(&mut app, Some("list"), None)
        .message
        .unwrap();
    assert!(list.contains("compatibility=partial"), "{list}");
    assert!(list.contains("commands=1"), "{list}");
    assert!(list.contains("hooks=1"), "{list}");

    let show = plugins_with_kimi_home_override(&mut app, Some("show mixed"), None)
        .message
        .unwrap();
    assert!(show.contains("Compatibility: partial"), "{show}");
    assert!(show.contains("Inactive components: [lsp]"), "{show}");
    assert!(show.contains("Active components: [none]"), "{show}");

    let review = plugins_with_kimi_home_override(&mut app, Some("trust mixed"), None)
        .message
        .unwrap();
    let confirmation = review
        .lines()
        .find(|line| line.starts_with("/plugin trust mixed "))
        .unwrap();
    let arg = confirmation.trim_start_matches("/plugin ");
    assert!(!plugins_with_kimi_home_override(&mut app, Some(arg), None).is_error);
    let enabled = plugins_with_kimi_home_override(&mut app, Some("enable mixed"), None);
    assert!(!enabled.is_error, "{:?}", enabled.message);
    let message = enabled.message.unwrap();
    assert!(message.contains("Compatibility: partial"), "{message}");
    assert!(message.contains("inactive: lsp"), "{message}");
    assert!(app.plugin_registry.is_active("mixed"));
    assert_eq!(
        app.plugin_registry
            .get("mixed")
            .unwrap()
            .compatibility()
            .as_str(),
        "partial"
    );

    let show = plugins_with_kimi_home_override(&mut app, Some("show mixed"), None)
        .message
        .unwrap();
    assert!(show.contains("State: active"), "{show}");
    assert!(show.contains("Inactive components: [lsp]"), "{show}");
    assert!(
        show.contains("Active components: [skills, commands, hooks]"),
        "{show}"
    );
    assert!(show.contains("Qualified skills: [mixed:hello]"), "{show}");
}

#[test]
fn mcp_review_discloses_host_authority_and_names_without_secret_values() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
    write_mcp_review_bundle(root.path());
    let (mut app, _temp) = create_test_app(root.path());
    let review = plugins_with_kimi_home_override(&mut app, Some("trust review-mcp"), None)
        .message
        .expect("review output");
    assert!(review.contains("mcp=2 (stdio=1 remote=1)"));
    assert!(review.contains("host-user filesystem/network authority"));
    assert!(review.contains("PLUGIN\\_TOKEN <- PLUGIN\\_TOKEN\\_SOURCE"));
    assert!(review.contains("X\\_Api\\_Key <- REMOTE\\_API\\_KEY"));
    assert!(review.contains("bearer_env=REMOTE\\_TOKEN"));
    assert!(review.contains("redirects=same-origin-only"));
    assert!(review.contains("Qualified skills: [none]"));
    assert!(review.contains("#2 value=\"--mode=worker\""));
    assert!(review.contains("#3 value=\"-e\""));
    assert!(review.contains("#4 value=\"console.log('ready')\""));
    assert!(review.contains("oauth=disabled"));
}

#[test]
fn legacy_tool_detail_remains_available_under_tools_namespace() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
    let (mut app, _temp) = create_test_app(root.path());
    fs::write(
        root.path().join("tools/greet.sh"),
        "# name: greet\n# description: Say hello\n# approval: required\n",
    )
    .unwrap();
    let result = plugins_with_kimi_home_override(&mut app, Some("tools greet"), None);
    assert!(!result.is_error);
    let message = result.message.unwrap();
    assert!(message.contains("Say hello"));
    assert!(message.contains("required"));
}

/// D4: `/plugin tools` names a script whose `approval: auto` was ignored and
/// shows the approval it actually runs with.
#[test]
fn legacy_tools_report_ignored_auto_approval() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
    let (mut app, _temp) = create_test_app(root.path());
    fs::write(
        root.path().join("tools/greet.sh"),
        "# name: greet\n# description: Say hello\n# approval: auto\n",
    )
    .unwrap();
    fs::write(
        root.path().join("tools/audit.sh"),
        "# name: audit\n# description: Audit\n# approval: required\n",
    )
    .unwrap();
    // The renderer escapes Markdown in plugin-controlled text.
    let warning = "[script_tool_auto_approval_ignored]: script tool 'greet': \\`approval: auto\\` is no longer supported for script tools";

    let list = plugins_with_kimi_home_override(&mut app, Some("tools"), None)
        .message
        .unwrap();
    assert!(list.contains(warning), "{list}");
    assert!(!list.contains("script tool 'audit'"), "{list}");

    let detail = plugins_with_kimi_home_override(&mut app, Some("tools greet"), None)
        .message
        .unwrap();
    assert!(detail.contains("suggest"), "{detail}");
    assert!(detail.contains(warning), "{detail}");
    let other = plugins_with_kimi_home_override(&mut app, Some("tools audit"), None)
        .message
        .unwrap();
    assert!(
        !other.contains("script_tool_auto_approval_ignored"),
        "{other}"
    );
}

#[test]
fn install_update_uninstall_verbs_validate_arguments() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
    let (mut app, _temp) = create_test_app(root.path());
    for arg in ["install", "update", "uninstall"] {
        let result = plugins_with_kimi_home_override(&mut app, Some(arg), None);
        assert!(result.is_error, "bare `{arg}` must print usage");
    }
    let invalid = plugins_with_kimi_home_override(&mut app, Some("install github:"), None);
    assert!(invalid.is_error);
    assert!(
        invalid
            .message
            .unwrap()
            .contains("Invalid plugin install source"),
        "invalid specs must be rejected before any network or disk access"
    );
}

#[test]
fn install_update_uninstall_verbs_drive_the_guided_trust_flow() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let codewhale_home = root.path().join("home");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);

    let source = root.path().join("source/installed-demo");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"installed-demo\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();

    let (mut app, _temp) = create_test_app(root.path());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let installed = plugins_with_kimi_home_override(
            &mut app,
            Some(&format!("install {}", source.display())),
            None,
        );
        assert!(!installed.is_error, "{:?}", installed.message);
        let message = installed.message.unwrap();
        assert!(message.contains("disabled and untrusted"), "{message}");
        let confirmation = message
            .lines()
            .find(|line| line.starts_with("/plugin trust installed-demo "))
            .expect("install must route into the trust review")
            .to_string();
        let plugin = app.plugin_registry.get("installed-demo").unwrap();
        assert!(!plugin.enabled && !plugin.trusted());
        assert!(
            codewhale_home
                .join("plugins/installed-demo/.installed-from")
                .exists()
        );

        // An unchanged local source succeeds without changing the review token.
        let update = plugins_with_kimi_home_override(&mut app, Some("update installed-demo"), None);
        assert!(!update.is_error, "{:?}", update.message);
        assert!(update.message.unwrap().contains("already up to date"));

        let arg = confirmation.trim_start_matches("/plugin ").to_string();
        assert!(!plugins_with_kimi_home_override(&mut app, Some(&arg), None).is_error);
        assert!(
            !plugins_with_kimi_home_override(&mut app, Some("enable installed-demo"), None)
                .is_error
        );
        assert!(app.plugin_registry.is_active("installed-demo"));

        fs::write(
            source.join("plugin.toml"),
            "schema_version = 1\n[plugin]\nname = \"installed-demo\"\nversion = \"2.0.0\"\n",
        )
        .unwrap();
        let updated =
            plugins_with_kimi_home_override(&mut app, Some("update installed-demo"), None);
        assert!(!updated.is_error, "{:?}", updated.message);
        let message = updated.message.unwrap();
        let current_confirmation = message
            .lines()
            .find(|line| line.starts_with("/plugin trust installed-demo "))
            .expect("changed local content must route into its current review");
        assert_ne!(current_confirmation, confirmation);
        assert!(!app.plugin_registry.is_active("installed-demo"));
        assert!(plugins_with_kimi_home_override(&mut app, Some(&arg), None).is_error);
        let current_arg = current_confirmation.trim_start_matches("/plugin ");
        assert!(!plugins_with_kimi_home_override(&mut app, Some(current_arg), None).is_error);
        assert!(
            !plugins_with_kimi_home_override(&mut app, Some("enable installed-demo"), None)
                .is_error
        );
        assert!(app.plugin_registry.is_active("installed-demo"));

        // Uninstall requires disabled, then removes bits and prunes state.
        let refused =
            plugins_with_kimi_home_override(&mut app, Some("uninstall installed-demo"), None);
        assert!(refused.is_error);
        assert!(codewhale_home.join("plugins/installed-demo").exists());
        assert!(
            !plugins_with_kimi_home_override(&mut app, Some("disable installed-demo"), None)
                .is_error
        );
        let removed =
            plugins_with_kimi_home_override(&mut app, Some("uninstall installed-demo"), None);
        assert!(!removed.is_error, "{:?}", removed.message);
        assert!(!codewhale_home.join("plugins/installed-demo").exists());
        assert!(app.plugin_registry.get("installed-demo").is_none());
        let raw = fs::read_to_string(codewhale_home.join("plugins/state.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(
            parsed["plugins"].as_object().unwrap().is_empty(),
            "uninstall must prune the state entry: {raw}"
        );
    });
}

#[test]
fn dsh_import_reviews_without_installing_then_installs_the_exact_bundle() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let codewhale_home = root.path().join("codewhale-home");
    let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    let package = root.path().join("dsh-package");
    fs::create_dir_all(package.join("pack-skills/guide")).unwrap();
    fs::write(
        package.join("package.json"),
        r#"{"name": "@demo/docs-dsh", "version": "2.0.0", "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}}"#,
    )
    .unwrap();
    fs::write(
        package.join("cordis.patch.yml"),
        "- insert:\n  - id: docs\n    name: '@deepseek-ai/dsh-mcp-client'\n    config: {serverName: docs, transport: streamable-http, url: 'https://docs.example.invalid/mcp'}\n  - id: skills\n    name: '@deepseek-ai/dsh-skill-filesystem'\n    config: {customSkillDirs: [pack-skills]}\n  - id: theme\n    name: '@deepseek-ai/dsh-client-ui-theme'\n",
    )
    .unwrap();
    fs::write(
        package.join("pack-skills/guide/SKILL.md"),
        "---\nname: guide\ndescription: Bundled guide\n---\nBody.\n",
    )
    .unwrap();

    let (mut app, _temp) = create_test_app(root.path());
    let help = plugins_with_kimi_home_override(&mut app, Some("help"), None)
        .message
        .unwrap();
    assert!(help.contains("/plugin import dsh <package-dir>"), "{help}");
    let review = plugins_with_kimi_home_override(
        &mut app,
        Some(&format!("import dsh {}", package.display())),
        None,
    );
    assert!(!review.is_error, "{:?}", review.message);
    let message = review.message.unwrap();
    for fact in [
        "@demo/docs-dsh@2.0.0",
        "plugin 'docs-dsh'",
        "Skills: guide",
        "Remote MCP servers: docs",
        "Network hosts it will request: docs.example.invalid",
        "theme",
        "Nothing was installed",
    ] {
        // Untrusted package text is rendered with review escaping.
        assert!(
            message.replace('\\', "").contains(fact),
            "{fact}: {message}"
        );
    }
    let approval = message
        .lines()
        .find_map(|line| line.trim().strip_prefix("/plugin "))
        .expect("review renders an exact approval command")
        .to_string();
    assert!(!codewhale_home.join("plugins/docs-dsh").exists());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let installed = plugins_with_kimi_home_override(&mut app, Some(&approval), None);
        assert!(!installed.is_error, "{:?}", installed.message);
        assert!(
            installed
                .message
                .as_deref()
                .is_some_and(|m| m.contains("disabled and untrusted"))
        );
    });
    let plugin = app.plugin_registry.get("docs-dsh").unwrap();
    assert!(!plugin.enabled && !plugin.trusted());
    assert!(
        codewhale_home
            .join("plugins/docs-dsh/CONVERSION.md")
            .is_file()
    );
}

#[test]
fn kimi_managed_import_is_read_only_until_hash_bound_approval() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let codewhale_home = root.path().join("codewhale-home");
    let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    let managed = root.path().join(".kimi-code/plugins/managed/kimi-demo");
    fs::create_dir_all(managed.join("skills/kimi-demo")).unwrap();
    fs::write(
        managed.join("kimi.plugin.json"),
        r#"{
          "name": "kimi-demo",
          "version": "1.0.0",
          "license": "Proprietary",
          "skills": "./skills/",
          "interface": {
            "displayName": "Kimi Demo",
            "hostKind": "local",
            "platforms": ["macos"]
          }
        }"#,
    )
    .unwrap();
    fs::write(
        managed.join("skills/kimi-demo/SKILL.md"),
        "---\nname: kimi-demo\ndescription: local managed fixture\n---\n",
    )
    .unwrap();

    let (mut app, _temp) = create_test_app(root.path());
    let help = plugins_with_kimi_home_override(&mut app, Some("help"), None)
        .message
        .unwrap();
    assert!(help.contains("/plugin import kimi [list]"), "{help}");
    let listed = plugins_with_kimi_home(&mut app, Some("import kimi"), root.path());
    assert!(!listed.is_error, "{:?}", listed.message);
    let message = listed.message.unwrap();
    assert!(
        message.contains("Kimi Demo") || message.contains("kimi-demo"),
        "{message}"
    );
    assert!(message.contains("license=Proprietary"), "{message}");
    assert!(message.contains("content hash:"), "{message}");
    assert!(message.contains("External Kimi apps"), "{message}");
    let approval = message
        .lines()
        .find_map(|line| line.trim().strip_prefix("approve: /plugin "))
        .expect("listing must render an exact approval command")
        .to_string();
    assert!(app.plugin_registry.get("kimi-demo").is_none());
    assert!(!codewhale_home.join("plugins/kimi-demo").exists());
    assert!(!codewhale_home.join("plugins/state.json").exists());

    // The approval token is tied to the bytes that were inspected.
    fs::write(
        managed.join("skills/kimi-demo/SKILL.md"),
        "---\nname: kimi-demo\ndescription: changed fixture\n---\n",
    )
    .unwrap();
    let changed = plugins_with_kimi_home(&mut app, Some(&approval), root.path());
    assert!(changed.is_error);
    assert!(changed.message.unwrap().contains("changed since review"));
    assert!(!codewhale_home.join("plugins/kimi-demo").exists());

    let refreshed = plugins_with_kimi_home(&mut app, Some("import kimi"), root.path())
        .message
        .unwrap();
    let approval = refreshed
        .lines()
        .find_map(|line| line.trim().strip_prefix("approve: /plugin "))
        .unwrap()
        .to_string();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let installed = plugins_with_kimi_home(&mut app, Some(&approval), root.path());
        assert!(!installed.is_error, "{:?}", installed.message);
        assert!(
            installed
                .message
                .as_deref()
                .is_some_and(|message| message.contains("disabled and untrusted"))
        );
    });
    let plugin = app.plugin_registry.get("kimi-demo").unwrap();
    assert!(!plugin.enabled && !plugin.trusted());
    assert!(
        codewhale_home
            .join("plugins/kimi-demo/kimi.plugin.json")
            .is_file()
    );
}

#[test]
fn kimi_managed_import_renders_in_the_selected_non_english_locale() {
    let root = TempDir::new().unwrap();
    let (mut app, _temp) = create_test_app(root.path());
    app.ui_locale = Locale::Es419;

    let message = plugins_with_kimi_home(&mut app, Some("import kimi"), root.path())
        .message
        .expect("localized Kimi listing");
    assert!(
        message.contains("Plugins gestionados por Kimi"),
        "{message}"
    );
    assert!(
        message.contains("No se encontraron plugins gestionados válidos"),
        "{message}"
    );
    assert!(
        !message.contains("No valid managed plugins found"),
        "{message}"
    );
}

#[cfg(unix)]
#[test]
fn kimi_managed_import_refuses_linked_children() {
    use std::os::unix::fs::symlink;

    let root = TempDir::new().unwrap();
    let managed_root = root.path().join(".kimi-code/plugins/managed");
    let outside = root.path().join("outside");
    fs::create_dir_all(&managed_root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    symlink(&outside, managed_root.join("linked-plugin")).unwrap();
    let (mut app, _temp) = create_test_app(root.path());

    let result = plugins_with_kimi_home(&mut app, Some("import kimi"), root.path());
    assert!(!result.is_error);
    let message = result.message.unwrap();
    assert!(message.contains("Rejected entries"), "{message}");
    assert!(message.contains("links and reparse points are refused"));
    assert!(!message.contains("approve: /plugin import kimi approve linked-plugin"));
}

#[test]
fn export_verb_writes_agent_plugins_bundle() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let codewhale_home = root.path().join("home");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    write_bundle(root.path());
    let (mut app, _temp) = create_test_app(root.path());

    let usage = plugins_with_kimi_home_override(&mut app, Some("export"), None);
    assert!(usage.is_error, "export without arguments is a usage error");
    let missing = plugins_with_kimi_home_override(&mut app, Some("export nope out"), None);
    assert!(missing.is_error, "exporting an unknown plugin fails");

    let result = plugins_with_kimi_home_override(&mut app, Some("export demo exported/demo"), None);
    assert!(!result.is_error, "{result:?}");
    let message = result.message.expect("export message");
    assert!(message.contains("Exported `demo`"), "{message}");
    assert!(message.contains("plugin.json"), "{message}");

    let bundle = root.path().join("exported/demo");
    let plugin_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(bundle.join("plugin.json")).unwrap()).unwrap();
    codewhale_plugins::agent_plugin::validate_plugin_json(&plugin_json).unwrap();
    assert_eq!(plugin_json["name"], "demo");
    assert_eq!(plugin_json["description"], "Import spreadsheet data safely");
    assert!(bundle.join("skills/hello/SKILL.md").is_file());
    assert!(!bundle.join("plugin.toml").exists());
    // No MCP servers declared, so no mcp.json is written.
    assert!(!bundle.join("mcp.json").exists());
    // The installed bundle keeps its legacy manifest and stays untouched.
    assert!(
        root.path()
            .join(".codewhale/plugins/demo/plugin.toml")
            .exists()
    );
}

#[test]
fn plugin_dismissals_list_and_reset_both_kinds() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let codewhale_home = root.path().join("home");
    fs::create_dir_all(&codewhale_home).unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    let (mut app, _temp) = create_test_app(root.path());
    codewhale_runtime::settings::Settings::transact_opt(|settings| {
        Ok(settings
            .dismissed_plugin_suggestions
            .insert("keptaway".to_string())
            .then_some(()))
    })
    .unwrap();
    app.plugin_cta.dismissed.insert("keptaway".to_string());
    app.plugin_cta.dismissed.insert("esconce".to_string());

    let listed = plugins_with_kimi_home_override(&mut app, Some("dismissals"), None)
        .message
        .expect("dismissal list");
    let kept = listed
        .find("keptaway")
        .unwrap_or_else(|| panic!("{listed}"));
    let session = listed.find("esconce").unwrap_or_else(|| panic!("{listed}"));
    assert!(listed.contains("Don't suggest again"), "{listed}");
    assert!(listed.contains("This session only"), "{listed}");
    assert!(kept < session, "{listed}");

    let reset = plugins_with_kimi_home_override(&mut app, Some("dismissals reset KeptAway"), None)
        .message
        .expect("reset receipt");
    assert!(reset.contains("keptaway"), "{reset}");
    assert!(
        codewhale_runtime::settings::Settings::load()
            .unwrap()
            .dismissed_plugin_suggestions
            .is_empty(),
        "reset must reach the saved choice"
    );
    assert!(!app.plugin_cta.dismissed.contains("keptaway"));
    assert!(app.plugin_cta.dismissed.contains("esconce"));

    plugins_with_kimi_home_override(&mut app, Some("dismissals reset"), None);
    assert!(app.plugin_cta.dismissed.is_empty());
    let empty = plugins_with_kimi_home_override(&mut app, Some("dismissals"), None)
        .message
        .expect("empty list");
    assert!(empty.contains("No plugins are hidden"), "{empty}");
}

#[test]
fn doctor_reports_read_only_then_fix_retires_stale_records_with_a_backup() {
    let _lock = crate::test_support::lock_test_env();
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    // The registry's own private state layout: 0700 directory, 0600 file.
    let plugins_dir = home.join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&plugins_dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let state_path = plugins_dir.join("state.json");
    let stale = serde_json::json!({
        "schema_version": 1,
        "plugins": {
            "workspace/111111111111/old-demo": {
                "generation": 4,
                "enabled": false,
                "trust": null,
                "review_history": [{
                    "content_hash": "c",
                    "capability_hash": "k",
                    "reviewed_capabilities": codewhale_plugins::manifest::PluginInventory::default(),
                    "reviewed_at": "2026-01-01T00:00:00+00:00"
                }]
            }
        }
    });
    fs::write(&state_path, serde_json::to_vec_pretty(&stale).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&state_path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
    let (mut app, _temp) = create_test_app(root.path());

    let report = plugins_with_kimi_home_override(&mut app, Some("doctor"), None);
    assert!(!report.is_error, "{report:?}");
    let text = report.message.unwrap();
    assert!(text.contains("nothing was changed"), "{text}");
    // The renderer escapes markdown, so the hyphen arrives backslashed.
    assert!(text.contains("old\\-demo"), "{text}");
    assert!(text.contains("/plugin doctor --fix"), "{text}");
    assert!(
        fs::read_to_string(&state_path)
            .unwrap()
            .contains("old-demo"),
        "the report must not write"
    );

    let fixed = plugins_with_kimi_home_override(&mut app, Some("doctor --fix"), None);
    assert!(!fixed.is_error, "{fixed:?}");
    assert!(
        fixed
            .message
            .as_deref()
            .unwrap()
            .contains("Retired 1 records")
    );
    assert!(matches!(
        fixed.action,
        Some(AppAction::PluginRegistryChanged)
    ));
    assert!(
        !fs::read_to_string(&state_path)
            .unwrap()
            .contains("old-demo")
    );
    assert!(
        fs::read_to_string(plugins_dir.join("state.json.pre-gc"))
            .unwrap()
            .contains("old-demo"),
        "the previous state is kept"
    );

    let usage = plugins_with_kimi_home_override(&mut app, Some("doctor --force"), None);
    assert!(usage.is_error);
    assert!(usage.message.unwrap().contains("Usage: /plugin doctor"));
}
