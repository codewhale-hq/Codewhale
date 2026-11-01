use super::*;
use crate::env_overrides::EnvRuntimeOverrides;
use crate::provider_config_keys::{
    CUSTOM_PROVIDER_FIELD_HINT, ProviderConfigField, get_provider_config_value,
    set_provider_config_value, unset_provider_config_value,
};
use anyhow::Result;
use codewhale_execpolicy::PermissionAction;
use std::env;
use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[test]
fn network_policy_toml_deserializes_proxy_hosts() {
    let policy: NetworkPolicyToml = toml::from_str(
        r#"
        default = "allow"
        proxy = ["github.com", ".githubusercontent.com"]
        proxy_fake_ip_cidrs = ["198.18.0.0/15"]
        "#,
    )
    .expect("network policy toml");

    assert_eq!(policy.default, "allow");
    assert_eq!(policy.proxy, ["github.com", ".githubusercontent.com"]);
    assert_eq!(policy.proxy_fake_ip_cidrs, ["198.18.0.0/15"]);
    assert!(policy.audit);
}

#[test]
fn retired_verifier_verdict_policy_is_accepted_and_dropped_on_load() {
    // `verdict_policy` was a single-value setting retired with the
    // verifier-preview schema cleanup. Old configs that still carry it must
    // keep loading; the key is read-only dropped (never written back).
    let config: ConfigToml = toml::from_str(
        r#"
        [verifier]
        enabled = true
        verdict_policy = "hunt"

        [verifier.unknown_extra]
        key = "value"
        "#,
    )
    .expect("a legacy verifier table must still parse");

    let verifier = config.verifier.expect("verifier table");
    assert!(verifier.enabled);
}

#[test]
fn lifecycle_outbox_toml_is_off_by_default_and_parses_when_configured() {
    // Unset = feature OFF: the table is absent and the field is None.
    let absent: ConfigToml = toml::from_str("model = \"demo\"\n").expect("minimal config");
    assert!(
        absent.lifecycle_outbox.is_none(),
        "unset [lifecycle_outbox] must leave the feature off"
    );

    // An empty table is also off: no path means no outbox file.
    let empty: ConfigToml =
        toml::from_str("[lifecycle_outbox]\n").expect("empty lifecycle_outbox table");
    let outbox = empty.lifecycle_outbox.expect("table should parse");
    assert!(outbox.path.is_none());
    assert!(outbox.webhook_url.is_none());
    assert!(outbox.webhook_token.is_none());

    // Full configuration: path plus optional webhook url and token.
    let full: ConfigToml = toml::from_str(
        r#"
        [lifecycle_outbox]
        path = "~/.codewhale/notifications/outbox.jsonl"
        webhook_url = "https://example.com/hooks/codewhale"
        webhook_token = "secret-token"
        "#,
    )
    .expect("full lifecycle_outbox table");
    let outbox = full.lifecycle_outbox.expect("table should parse");
    assert_eq!(
        outbox.path,
        Some(PathBuf::from("~/.codewhale/notifications/outbox.jsonl"))
    );
    assert_eq!(
        outbox.webhook_url.as_deref(),
        Some("https://example.com/hooks/codewhale")
    );
    assert_eq!(outbox.webhook_token.as_deref(), Some("secret-token"));
}

#[test]
fn lifecycle_outbox_toml_webhook_is_optional() {
    // `path` alone enables the file outbox without any webhook.
    let file_only: ConfigToml = toml::from_str(
        r#"
        [lifecycle_outbox]
        path = "/tmp/outbox.jsonl"
        "#,
    )
    .expect("file-only lifecycle_outbox table");
    let outbox = file_only.lifecycle_outbox.expect("table should parse");
    assert_eq!(outbox.path, Some(PathBuf::from("/tmp/outbox.jsonl")));
    assert!(outbox.webhook_url.is_none());

    // A webhook url without a path does not enable a file outbox; the
    // consumer decides whether webhook-only delivery is meaningful, but the
    // parse must stay lossless either way.
    let webhook_only: ConfigToml = toml::from_str(
        r#"
        [lifecycle_outbox]
        webhook_url = "https://example.com/hooks/codewhale"
        "#,
    )
    .expect("webhook-only lifecycle_outbox table");
    let outbox = webhook_only.lifecycle_outbox.expect("table should parse");
    assert!(outbox.path.is_none());
    assert_eq!(
        outbox.webhook_url.as_deref(),
        Some("https://example.com/hooks/codewhale")
    );
    assert!(outbox.webhook_token.is_none());
}

#[test]
fn control_socket_toml_is_off_by_default_and_parses_when_enabled() {
    // Unset = feature OFF: the table is absent and the field is None.
    let absent: ConfigToml = toml::from_str("model = \"demo\"\n").expect("minimal config");
    assert!(
        absent.control_socket.is_none(),
        "unset [control_socket] must leave the feature off"
    );

    // An empty table is also off: enabled defaults to false.
    let empty: ConfigToml =
        toml::from_str("[control_socket]\n").expect("empty control_socket table");
    let socket = empty.control_socket.expect("table should parse");
    assert!(!socket.enabled, "empty table must leave the socket off");

    // Explicit enable.
    let enabled: ConfigToml = toml::from_str(
        r#"
        [control_socket]
        enabled = true
        "#,
    )
    .expect("enabled control_socket table");
    assert!(
        enabled.control_socket.expect("table should parse").enabled,
        "enabled = true must turn the socket on"
    );

    // Explicit disable stays off.
    let disabled: ConfigToml = toml::from_str(
        r#"
        [control_socket]
        enabled = false
        "#,
    )
    .expect("disabled control_socket table");
    assert!(
        !disabled.control_socket.expect("table should parse").enabled,
        "enabled = false must keep the socket off"
    );
}

#[test]
fn permissions_toml_deserializes_typed_ask_rules() {
    let permissions: PermissionsToml = toml::from_str(
        r#"
        [[rules]]
        tool = "exec_shell"
        command = "cargo test"

        [[rules]]
        tool = "read_file"
        path = "secrets/api_key.txt"
        "#,
    )
    .expect("permissions toml");

    assert_eq!(
        permissions.rules,
        vec![
            ToolAskRule::exec_shell("cargo test"),
            ToolAskRule::file_path("read_file", "secrets/api_key.txt"),
        ]
    );
}

#[test]
fn permissions_toml_rejects_unknown_decision_field() {
    // `decision` is NOT a valid field — `deny_unknown_fields` still active.
    let err = toml::from_str::<PermissionsToml>(
        r#"
        [[rules]]
        tool = "exec_shell"
        decision = "allow"
        command = "cargo test"
        "#,
    )
    .expect_err("permissions.toml should reject unknown 'decision' field");

    assert!(err.message().contains("unknown field"));
}

#[test]
fn permissions_toml_deserializes_action_deny_and_allow() {
    let permissions: PermissionsToml = toml::from_str(
        r#"
        [[rules]]
        tool = "exec_shell"
        command = "sed"
        action = "deny"

        [[rules]]
        tool = "exec_shell"
        command = "git status"
        action = "allow"

        [[rules]]
        tool = "exec_shell"
        command = "cargo test"
        "#,
    )
    .expect("permissions toml with actions");

    assert_eq!(permissions.rules.len(), 3);
    assert_eq!(
        permissions.rules[0].action,
        codewhale_execpolicy::PermissionAction::Deny
    );
    assert_eq!(
        permissions.rules[1].action,
        codewhale_execpolicy::PermissionAction::Allow
    );
    assert_eq!(
        permissions.rules[2].action,
        codewhale_execpolicy::PermissionAction::Ask
    ); // default
}

#[test]
fn permissions_ruleset_populates_denied_and_trusted_prefixes() {
    let permissions: PermissionsToml = toml::from_str(
        r#"
        [[rules]]
        tool = "exec_shell"
        command = "sed"
        action = "deny"

        [[rules]]
        tool = "exec_shell"
        command = "awk"
        action = "deny"

        [[rules]]
        tool = "exec_shell"
        command = "git status"
        action = "allow"

        [[rules]]
        tool = "exec_shell"
        command = "cargo test"
        action = "ask"
        "#,
    )
    .unwrap();

    let ruleset = permissions.ruleset();

    // All four rules kept as ask_rules for path-based / tool-only matching
    assert_eq!(ruleset.ask_rules.len(), 4);
    // deny rules promoted to denied_prefixes
    assert!(ruleset.denied_prefixes.contains(&"sed".to_string()));
    assert!(ruleset.denied_prefixes.contains(&"awk".to_string()));
    // allow rule promoted to trusted_prefixes
    assert!(ruleset.trusted_prefixes.contains(&"git status".to_string()));
    // ask rule NOT in trusted/denied prefixes
    assert!(!ruleset.trusted_prefixes.contains(&"cargo test".to_string()));
    assert!(!ruleset.denied_prefixes.contains(&"cargo test".to_string()));
}

#[test]
fn exact_workspace_command_allow_is_not_promoted_to_global_prefix() {
    let rule =
        ToolAskRule::exec_shell("cargo test").into_exact_workspace_allow("/workspace/project");
    let permissions = PermissionsToml {
        rules: vec![rule.clone()],
    };

    let ruleset = permissions.ruleset();

    assert!(
        ruleset.trusted_prefixes.is_empty(),
        "an exact repo grant must not become a global trusted prefix"
    );
    assert_eq!(ruleset.ask_rules, vec![rule]);
}

#[test]
fn permissions_ruleset_deny_without_command_stays_in_ask_rules() {
    // Tool-only deny (no command) can't be promoted to denied_prefixes.
    let permissions: PermissionsToml = toml::from_str(
        r#"
        [[rules]]
        tool = "exec_shell"
        action = "deny"
        "#,
    )
    .unwrap();

    let ruleset = permissions.ruleset();
    assert_eq!(ruleset.ask_rules.len(), 1);
    assert_eq!(
        ruleset.ask_rules[0].action,
        codewhale_execpolicy::PermissionAction::Deny
    );
    // No command → nothing to promote to denied_prefixes
    assert!(ruleset.denied_prefixes.is_empty());
}

#[test]
fn permissions_ruleset_empty_rules_produces_empty_ruleset() {
    let permissions = PermissionsToml::default();
    let ruleset = permissions.ruleset();
    assert!(ruleset.trusted_prefixes.is_empty());
    assert!(ruleset.denied_prefixes.is_empty());
    assert!(ruleset.ask_rules.is_empty());
}

#[test]
fn permissions_ruleset_mixed_actions_all_coexist() {
    let permissions: PermissionsToml = toml::from_str(
        r#"
        [[rules]]
        tool = "exec_shell"
        command = "rm -rf"
        action = "deny"

        [[rules]]
        tool = "exec_shell"
        command = "git status"
        action = "allow"

        [[rules]]
        tool = "exec_shell"
        command = "npm test"
        action = "ask"

        [[rules]]
        tool = "read_file"
        path = "Cargo.toml"
        action = "allow"

        [[rules]]
        tool = "write_file"
        path = "src/secrets.rs"
        action = "deny"
        "#,
    )
    .unwrap();

    let ruleset = permissions.ruleset();

    // All 5 rules in ask_rules
    assert_eq!(ruleset.ask_rules.len(), 5);

    // Command-based deny → denied_prefixes
    assert!(ruleset.denied_prefixes.contains(&"rm -rf".to_string()));
    assert_eq!(ruleset.denied_prefixes.len(), 1); // only rm -rf has a command

    // Command-based allow → trusted_prefixes
    assert!(ruleset.trusted_prefixes.contains(&"git status".to_string()));
    assert_eq!(ruleset.trusted_prefixes.len(), 1); // only git status has a command

    // Path-based rules stay in ask_rules but not in prefixes
    let path_deny = ruleset
        .ask_rules
        .iter()
        .find(|r| r.path.as_deref() == Some("src/secrets.rs"))
        .unwrap();
    assert_eq!(
        path_deny.action,
        codewhale_execpolicy::PermissionAction::Deny
    );

    let path_allow = ruleset
        .ask_rules
        .iter()
        .find(|r| r.path.as_deref() == Some("Cargo.toml"))
        .unwrap();
    assert_eq!(
        path_allow.action,
        codewhale_execpolicy::PermissionAction::Allow
    );
}

#[test]
fn provider_command_auth_source_deserializes() {
    let config: ConfigToml = toml::from_str(
        r#"
        [providers.deepseek.auth]
        source = "command"
        command = ["keepassxc-cli", "show", "CodeWhale/DeepSeek", "--attribute", "password"]
        timeout_ms = 2000
        "#,
    )
    .expect("config toml");

    let auth = config
        .providers
        .deepseek
        .auth
        .expect("provider auth source");
    assert_eq!(auth.source, AuthSourceKind::Command);
    assert_eq!(auth.source_class(), "command");
    assert_eq!(auth.command[0], "keepassxc-cli");
    assert_eq!(auth.timeout_ms, Some(2000));
    auth.validate().expect("valid command auth source");
}

#[test]
fn provider_secret_auth_source_deserializes() {
    let config: ConfigToml = toml::from_str(
        r#"
        [providers.openai.auth]
        source = "secret"
        secret_id = "codewhale/openai"
        "#,
    )
    .expect("config toml");

    let auth = config.providers.openai.auth.expect("provider auth source");
    assert_eq!(auth.source, AuthSourceKind::Secret);
    assert_eq!(auth.source_class(), "secret");
    assert_eq!(auth.secret_id.as_deref(), Some("codewhale/openai"));
    auth.validate().expect("valid secret auth source");
}

#[test]
fn provider_auth_source_rejects_empty_command() {
    let config: ConfigToml = toml::from_str(
        r#"
        [providers.deepseek.auth]
        source = "command"
        command = []
        "#,
    )
    .expect("config toml");

    let auth = config
        .providers
        .deepseek
        .auth
        .expect("provider auth source");
    let err = auth.validate().expect_err("empty command must be invalid");
    assert!(err.to_string().contains("command must include"));
}

#[test]
fn hotbar_hidden_when_config_is_absent() {
    // #3807: an absent `hotbar` key resolves to no bindings, so the Hotbar is
    // hidden until the user opts in. The default slots are still available
    // explicitly via `default_hotbar_bindings_toml()` (what `/hotbar on` writes).
    let config = ConfigToml::default();

    let resolved = config.resolve_hotbar_bindings(&DEFAULT_HOTBAR_ACTIONS);

    assert_eq!(resolved.warnings, Vec::new());
    assert!(
        resolved.bindings.is_empty(),
        "fresh config must resolve to no hotbar bindings: {:?}",
        resolved.bindings
    );

    // The explicit default set still expands to the eight recommended slots.
    let explicit = ConfigToml {
        hotbar: Some(default_hotbar_bindings_toml()),
        ..ConfigToml::default()
    };
    assert_eq!(
        explicit
            .resolve_hotbar_bindings(&DEFAULT_HOTBAR_ACTIONS)
            .bindings,
        default_hotbar_bindings(),
        "an explicit default-bindings config still shows all eight slots"
    );
}

#[test]
fn hotbar_empty_array_disables_default_slots() {
    let config: ConfigToml = toml::from_str("hotbar = []\n").expect("parse empty hotbar array");

    let resolved = config.resolve_hotbar_bindings(&DEFAULT_HOTBAR_ACTIONS);

    assert_eq!(resolved.warnings, Vec::new());
    assert_eq!(resolved.bindings, Vec::new());

    let serialized = toml::to_string_pretty(&config).expect("serialize config");
    let round_tripped: ConfigToml =
        toml::from_str(&serialized).expect("deserialize serialized config");
    assert_eq!(round_tripped.hotbar, Some(Vec::new()));
}

#[test]
fn hotbar_tables_parse_and_round_trip() {
    let config: ConfigToml = toml::from_str(
        r#"
[[hotbar]]
slot = 1
label = "Plan"
action = "mode.plan"

[[hotbar]]
slot = 2
action = "session.compact"
"#,
    )
    .expect("parse hotbar tables");

    let resolved = config.resolve_hotbar_bindings(&["mode.plan", "session.compact"]);

    assert_eq!(
        resolved.bindings,
        vec![
            HotbarBinding {
                slot: 1,
                action: "mode.plan".to_string(),
                label: Some("Plan".to_string()),
            },
            HotbarBinding {
                slot: 2,
                action: "session.compact".to_string(),
                label: None,
            },
        ]
    );
    assert_eq!(resolved.warnings, Vec::new());

    let serialized = toml::to_string_pretty(&config).expect("serialize config");
    let round_tripped: ConfigToml =
        toml::from_str(&serialized).expect("deserialize serialized config");
    assert_eq!(round_tripped.hotbar, config.hotbar);
}

#[test]
fn hotbar_validation_warns_without_dropping_unknown_actions() {
    let config: ConfigToml = toml::from_str(
        r#"
[[hotbar]]
slot = 0
action = "mode.plan"

[[hotbar]]
slot = 2
action = "mode.plan"

[[hotbar]]
slot = 2
action = "custom.action"

[[hotbar]]
slot = 9
action = "mode.agent"
"#,
    )
    .expect("parse hotbar tables");

    let resolved = config.resolve_hotbar_bindings(&["mode.plan", "mode.agent"]);

    assert_eq!(
        resolved.bindings,
        vec![HotbarBinding {
            slot: 2,
            action: "custom.action".to_string(),
            label: None,
        }]
    );
    assert_eq!(
        resolved.warnings,
        vec![
            HotbarConfigWarning::SlotOutOfRange {
                slot: 0,
                action: "mode.plan".to_string(),
            },
            HotbarConfigWarning::UnknownAction {
                slot: 2,
                action: "custom.action".to_string(),
            },
            HotbarConfigWarning::DuplicateSlot {
                slot: 2,
                previous_action: "mode.plan".to_string(),
                replacement_action: "custom.action".to_string(),
            },
            HotbarConfigWarning::SlotOutOfRange {
                slot: 9,
                action: "mode.agent".to_string(),
            },
        ]
    );
    assert!(resolved.warnings[1].to_string().contains("keeping binding"));
}

#[test]
fn config_store_loads_sibling_permissions_toml() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "codewhale-permissions-schema-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("mkdir");
    let config_path = dir.join(CONFIG_FILE_NAME);
    let permissions_path = dir.join(PERMISSIONS_FILE_NAME);
    fs::write(&config_path, "model = \"deepseek-v4-flash\"\n").expect("write config");
    fs::write(
        &permissions_path,
        r#"
        [[rules]]
        tool = "exec_shell"
        command = "cargo test"

        [[rules]]
        tool = "read_file"
        path = "secrets/api_key.txt"
        "#,
    )
    .expect("write permissions");

    let store = ConfigStore::load(Some(config_path.clone())).expect("load config store");

    assert_eq!(store.config.model.as_deref(), Some("deepseek-v4-flash"));
    assert_eq!(
        store.permissions().rules.as_slice(),
        &[
            ToolAskRule::exec_shell("cargo test"),
            ToolAskRule::file_path("read_file", "secrets/api_key.txt"),
        ]
    );
    assert_eq!(
        store
            .permissions_path()
            .canonicalize()
            .expect("store perms"),
        permissions_path.canonicalize().expect("expected perms")
    );

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn config_store_loads_permissions_even_when_config_is_absent() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "codewhale-permissions-only-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("mkdir");
    let config_path = dir.join(CONFIG_FILE_NAME);
    fs::write(
        dir.join(PERMISSIONS_FILE_NAME),
        r#"
        [[rules]]
        tool = "exec_shell"
        command = "cargo check"
        "#,
    )
    .expect("write permissions");

    let store = ConfigStore::load(Some(config_path)).expect("load config store");

    assert!(store.config.model.is_none());
    assert_eq!(
        store.permissions().rules.as_slice(),
        &[ToolAskRule::exec_shell("cargo check")]
    );

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn config_store_exec_policy_engine_uses_sibling_permissions() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "codewhale-permissions-engine-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("mkdir");
    let config_path = dir.join(CONFIG_FILE_NAME);
    fs::write(&config_path, "model = \"deepseek-v4-flash\"\n").expect("write config");
    fs::write(
        dir.join(PERMISSIONS_FILE_NAME),
        r#"
        [[rules]]
        tool = "exec_shell"
        command = "cargo test"
        "#,
    )
    .expect("write permissions");

    let store = ConfigStore::load(Some(config_path)).expect("load config store");
    let decision = store
        .exec_policy_engine()
        .check(codewhale_execpolicy::ExecPolicyContext {
            command: "cargo test --workspace",
            cwd: "/workspace",
            tool: Some("exec_shell"),
            path: None,
            ask_for_approval: codewhale_execpolicy::AskForApproval::UnlessTrusted,
            sandbox_mode: Some("workspace-write"),
        })
        .expect("policy check");

    assert!(decision.allow);
    assert!(decision.requires_approval);
    assert_eq!(
        decision.matched_rule.as_deref(),
        Some("tool=exec_shell command=cargo test")
    );

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn config_store_appends_ask_rules_without_losing_comments_or_duplicates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let permissions_path = dir.path().join(PERMISSIONS_FILE_NAME);
    fs::write(&config_path, "model = \"deepseek-v4-flash\"\n").expect("write config");
    fs::write(
        &permissions_path,
        r#"# keep this permission note
[[rules]]
tool = "exec_shell"
command = "cargo check"
"#,
    )
    .expect("write permissions");

    let mut store = ConfigStore::load(Some(config_path)).expect("load config store");
    let existing = ToolAskRule::exec_shell("cargo check");
    let added_rule = ToolAskRule::file_path("read_file", "docs/README.md");
    let added = store
        .append_ask_rules(&[existing, added_rule.clone(), added_rule.clone()])
        .expect("append ask rules");

    assert_eq!(added, 1);
    assert_eq!(
        store.permissions().rules,
        vec![ToolAskRule::exec_shell("cargo check"), added_rule.clone(),]
    );
    let body = fs::read_to_string(&permissions_path).expect("read permissions");
    assert!(body.contains("# keep this permission note"));
    assert_eq!(body.matches("docs/README.md").count(), 1);
    assert!(!body.contains("decision"));

    let before_duplicate_append = body;
    assert_eq!(
        store
            .append_ask_rules(&[added_rule])
            .expect("dedupe ask rule"),
        0
    );
    assert_eq!(
        fs::read_to_string(&permissions_path).expect("read unchanged permissions"),
        before_duplicate_append
    );

    let reloaded =
        ConfigStore::load(Some(dir.path().join(CONFIG_FILE_NAME))).expect("reload config store");
    assert_eq!(reloaded.permissions(), store.permissions());
}

#[test]
fn config_store_appends_ask_rule_to_inline_rules_array() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let permissions_path = dir.path().join(PERMISSIONS_FILE_NAME);
    fs::write(
        &permissions_path,
        "# inline rules stay valid\nrules = [{ tool = \"exec_shell\", command = \"cargo check\" }]\n",
    )
    .expect("write permissions");

    let mut store = ConfigStore::load(Some(config_path)).expect("load config store");
    assert_eq!(
        store
            .append_ask_rules(&[ToolAskRule::file_path("read_file", "README.md")])
            .expect("append inline ask rule"),
        1
    );

    let body = fs::read_to_string(&permissions_path).expect("read permissions");
    assert!(body.contains("# inline rules stay valid"));
    let parsed: PermissionsToml = toml::from_str(&body).expect("parse persisted permissions");
    assert_eq!(
        parsed.rules,
        vec![
            ToolAskRule::exec_shell("cargo check"),
            ToolAskRule::file_path("read_file", "README.md"),
        ]
    );
}

#[test]
fn config_store_appends_exact_workspace_allow_rules() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let permissions_path = dir.path().join(PERMISSIONS_FILE_NAME);
    fs::write(&permissions_path, "# remembered grants stay user-owned\n")
        .expect("write permissions");
    let mut store = ConfigStore::load(Some(config_path.clone())).expect("load config store");
    let rule = ToolAskRule::exec_shell("cargo test")
        .into_exact_workspace_allow(dir.path().to_string_lossy());

    assert_eq!(
        store
            .append_allow_rules(&[rule.clone(), rule.clone()])
            .expect("append allow rule"),
        1
    );

    let body = fs::read_to_string(&permissions_path).expect("read permissions");
    assert!(body.contains("# remembered grants stay user-owned"));
    assert!(body.contains("action = \"allow\""));
    assert!(body.contains("command_exact = true"));
    // Windows paths may make toml_edit choose a different valid quoting style
    // than Rust's Debug output. Check the rendered field shape here and its
    // exact value through the typed parse below.
    assert!(body.lines().any(|line| line.starts_with("workspace = ")));
    let parsed: PermissionsToml = toml::from_str(&body).expect("parse permissions");
    assert_eq!(parsed.rules, vec![rule.clone()]);

    let exact = store
        .exec_policy_engine()
        .check(codewhale_execpolicy::ExecPolicyContext {
            command: "cargo test",
            cwd: dir.path().to_string_lossy().as_ref(),
            tool: Some("exec_shell"),
            path: None,
            ask_for_approval: codewhale_execpolicy::AskForApproval::OnRequest,
            sandbox_mode: Some("workspace-write"),
        })
        .expect("check exact grant");
    assert_eq!(
        exact.matched_action,
        Some(codewhale_execpolicy::PermissionAction::Allow)
    );
    assert!(!exact.requires_approval);

    let extra_args = store
        .exec_policy_engine()
        .check(codewhale_execpolicy::ExecPolicyContext {
            command: "cargo test --workspace",
            cwd: dir.path().to_string_lossy().as_ref(),
            tool: Some("exec_shell"),
            path: None,
            ask_for_approval: codewhale_execpolicy::AskForApproval::OnRequest,
            sandbox_mode: Some("workspace-write"),
        })
        .expect("check extra args");
    assert!(extra_args.requires_approval);

    let reloaded = ConfigStore::load(Some(config_path)).expect("reload config store");
    assert_eq!(reloaded.permissions().rules, vec![rule]);
}

#[test]
fn config_store_rejects_broad_or_unscoped_allow_rules() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let mut store = ConfigStore::load(Some(config_path)).expect("load config store");

    let mut unscoped = ToolAskRule::exec_shell("cargo test");
    unscoped.action = codewhale_execpolicy::PermissionAction::Allow;
    assert!(
        store
            .append_allow_rules(&[unscoped])
            .expect_err("unscoped allow must fail")
            .to_string()
            .contains("scoped to a workspace")
    );

    let mut prefix = ToolAskRule::exec_shell("cargo test");
    prefix.action = codewhale_execpolicy::PermissionAction::Allow;
    prefix.workspace = Some(dir.path().to_string_lossy().into_owned());
    assert!(
        store
            .append_allow_rules(&[prefix])
            .expect_err("prefix allow must fail")
            .to_string()
            .contains("exact matching")
    );

    assert!(
        store
            .append_allow_rules(&[ToolAskRule::new("exec_shell")
                .into_exact_workspace_allow(dir.path().to_string_lossy())])
            .expect_err("tool-wide allow must fail")
            .to_string()
            .contains("exact command or path")
    );
}

#[test]
fn config_store_does_not_overwrite_invalid_permissions_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let permissions_path = dir.path().join(PERMISSIONS_FILE_NAME);
    let mut store = ConfigStore::load(Some(config_path)).expect("load config store");
    let invalid = "rules = \"not-an-array\"\n";
    fs::write(&permissions_path, invalid).expect("write invalid permissions");

    let error = store
        .append_ask_rules(&[ToolAskRule::exec_shell("cargo test")])
        .expect_err("invalid permissions should fail");

    assert!(error.to_string().contains("failed to parse permissions"));
    assert_eq!(
        fs::read_to_string(&permissions_path).expect("read invalid permissions"),
        invalid
    );
    assert!(store.permissions().is_empty());
}

#[test]
fn duplicate_append_refreshes_permissions_changed_on_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let permissions_path = dir.path().join(PERMISSIONS_FILE_NAME);
    let mut store = ConfigStore::load(Some(config_path)).expect("load config store");
    fs::write(
        permissions_path,
        "[[rules]]\ntool = \"exec_shell\"\ncommand = \"cargo check\"\n",
    )
    .expect("write external permissions update");

    assert_eq!(
        store
            .append_ask_rules(&[ToolAskRule::exec_shell("cargo check")])
            .expect("dedupe external ask rule"),
        0
    );
    assert_eq!(
        store.permissions().rules,
        vec![ToolAskRule::exec_shell("cargo check")]
    );
}

#[cfg(unix)]
#[test]
fn config_store_secures_persisted_permissions_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let permissions_path = dir.path().join(PERMISSIONS_FILE_NAME);
    let mut store = ConfigStore::load(Some(config_path)).expect("load config store");

    store
        .append_ask_rules(&[ToolAskRule::exec_shell("cargo test")])
        .expect("append ask rule");

    let mode = fs::metadata(permissions_path)
        .expect("permissions metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn permission_snapshot_removes_array_table_rule_without_reformatting_neighbors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let permissions_path = dir.path().join(PERMISSIONS_FILE_NAME);
    fs::write(
        &permissions_path,
        r#"# operator-owned header
[[rules]]
tool = "exec_shell"
command = "cargo check"

# remove only this record
[[rules]]
tool = "exec_shell"
command = "cargo test"
action = "deny"

# keep this record and comment
[[rules]]
tool = "edit_file"
path = "src/lib.rs"
action = "allow"
"#,
    )
    .expect("write permissions");

    let snapshot =
        load_permissions_snapshot(Some(config_path.clone())).expect("load permission snapshot");
    assert!(snapshot.file_exists());
    assert_eq!(
        snapshot.path().canonicalize().expect("snapshot path"),
        permissions_path.canonicalize().expect("permissions path")
    );
    assert_eq!(snapshot.rules().len(), 3);
    let token = snapshot
        .removal_token(1)
        .expect("middle rule token")
        .to_string();

    let removed =
        remove_permission_rule(Some(config_path.clone()), 1, &token).expect("remove middle rule");
    assert_eq!(removed.command.as_deref(), Some("cargo test"));
    assert_eq!(removed.action, PermissionAction::Deny);

    let body = fs::read_to_string(&permissions_path).expect("read permissions");
    assert!(body.contains("# operator-owned header"));
    assert!(body.contains("# keep this record and comment"));
    assert!(!body.contains("cargo test"));
    assert!(body.contains("command = \"cargo check\""));
    assert!(body.contains("path = \"src/lib.rs\""));
    let parsed: PermissionsToml = toml::from_str(&body).expect("parse edited permissions");
    assert_eq!(
        parsed.rules,
        vec![ToolAskRule::exec_shell("cargo check"), {
            let mut rule = ToolAskRule::file_path("edit_file", "src/lib.rs");
            rule.action = PermissionAction::Allow;
            rule
        },]
    );

    let reloaded =
        load_permissions_snapshot(Some(config_path)).expect("reload permission snapshot");
    assert_eq!(reloaded.rules(), parsed.rules);
}

#[test]
fn permission_snapshot_removes_inline_array_rule() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let permissions_path = dir.path().join(PERMISSIONS_FILE_NAME);
    fs::write(
        &permissions_path,
        "# inline style stays inline\nrules = [\n  { tool = \"exec_shell\", command = \"cargo check\" },\n  { tool = \"write_file\", path = \"README.md\", action = \"allow\" },\n]\n",
    )
    .expect("write permissions");

    let snapshot =
        load_permissions_snapshot(Some(config_path.clone())).expect("load permission snapshot");
    let token = snapshot
        .removal_token(0)
        .expect("first rule token")
        .to_string();
    let removed = remove_permission_rule(Some(config_path), 0, &token).expect("remove inline rule");

    assert_eq!(removed.command.as_deref(), Some("cargo check"));
    let body = fs::read_to_string(&permissions_path).expect("read permissions");
    assert!(body.contains("# inline style stays inline"));
    assert!(body.contains("rules = ["));
    assert!(!body.contains("cargo check"));
    let parsed: PermissionsToml = toml::from_str(&body).expect("parse inline permissions");
    assert_eq!(parsed.rules.len(), 1);
    assert_eq!(parsed.rules[0].path.as_deref(), Some("README.md"));
}

#[test]
fn permission_removal_preserves_file_header_when_first_array_table_is_removed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let permissions_path = dir.path().join(PERMISSIONS_FILE_NAME);
    fs::write(
        &permissions_path,
        concat!(
            "# keep this file-level explanation\n",
            "[[rules]]\n",
            "tool = \"exec_shell\"\n",
            "command = \"cargo check\"\n\n",
            "[[rules]]\n",
            "tool = \"exec_shell\"\n",
            "command = \"cargo test\"\n",
        ),
    )
    .expect("write permissions");
    let snapshot =
        load_permissions_snapshot(Some(config_path.clone())).expect("load permission snapshot");
    let token = snapshot
        .removal_token(0)
        .expect("first rule token")
        .to_string();

    remove_permission_rule(Some(config_path), 0, &token).expect("remove first rule");

    let body = fs::read_to_string(&permissions_path).expect("read permissions");
    assert!(body.contains("# keep this file-level explanation"));
    assert!(!body.contains("cargo check"));
    assert!(body.contains("cargo test"));
}

#[test]
fn permission_removal_rejects_stale_snapshot_without_writing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let permissions_path = dir.path().join(PERMISSIONS_FILE_NAME);
    fs::write(
        &permissions_path,
        "[[rules]]\ntool = \"exec_shell\"\ncommand = \"cargo check\"\n",
    )
    .expect("write permissions");

    let snapshot =
        load_permissions_snapshot(Some(config_path.clone())).expect("load permission snapshot");
    let token = snapshot
        .removal_token(0)
        .expect("first rule token")
        .to_string();
    let changed = concat!(
        "[[rules]]\n",
        "tool = \"read_file\"\n",
        "path = \"README.md\"\n\n",
        "[[rules]]\n",
        "tool = \"exec_shell\"\n",
        "command = \"cargo check\"\n",
    );
    fs::write(&permissions_path, changed).expect("write concurrent permissions update");

    let error =
        remove_permission_rule(Some(config_path), 0, &token).expect_err("stale removal must fail");

    assert!(
        error
            .to_string()
            .contains("permissions changed after they were listed")
    );
    assert_eq!(
        fs::read_to_string(&permissions_path).expect("read unchanged permissions"),
        changed
    );
}

struct EnvGuard {
    active_route_api_key: Option<OsString>,
    active_route_base_url: Option<OsString>,
    deepseek_anthropic_base_url: Option<OsString>,
    deepseek_claude_base_url: Option<OsString>,
    deepseek_http_headers: Option<OsString>,
    deepseek_model: Option<OsString>,
    deepseek_default_text_model: Option<OsString>,
    deepseek_provider: Option<OsString>,
    deepseek_auth_mode: Option<OsString>,
    nvidia_api_key: Option<OsString>,
    nvidia_nim_api_key: Option<OsString>,
    nim_base_url: Option<OsString>,
    nvidia_base_url: Option<OsString>,
    nvidia_nim_base_url: Option<OsString>,
    openrouter_api_key: Option<OsString>,
    openrouter_base_url: Option<OsString>,
    openrouter_model: Option<OsString>,
    openmodel_api_key: Option<OsString>,
    openmodel_base_url: Option<OsString>,
    openmodel_model: Option<OsString>,
    xiaomi_mimo_token_plan_api_key: Option<OsString>,
    mimo_token_plan_api_key: Option<OsString>,
    xiaomi_mimo_api_key: Option<OsString>,
    xiaomi_api_key: Option<OsString>,
    mimo_api_key: Option<OsString>,
    xiaomi_mimo_base_url: Option<OsString>,
    mimo_base_url: Option<OsString>,
    xiaomi_mimo_model: Option<OsString>,
    mimo_model: Option<OsString>,
    xiaomi_mimo_mode: Option<OsString>,
    mimo_mode: Option<OsString>,
    wanjie_ark_api_key: Option<OsString>,
    volcengine_api_key: Option<OsString>,
    volcengine_ark_api_key: Option<OsString>,
    ark_api_key: Option<OsString>,
    volcengine_base_url: Option<OsString>,
    volcengine_ark_base_url: Option<OsString>,
    ark_base_url: Option<OsString>,
    wanjie_ark_base_url: Option<OsString>,
    wanjie_base_url: Option<OsString>,
    wanjie_maas_base_url: Option<OsString>,
    volcengine_model: Option<OsString>,
    volcengine_ark_model: Option<OsString>,
    wanjie_ark_model: Option<OsString>,
    wanjie_model: Option<OsString>,
    wanjie_maas_model: Option<OsString>,
    novita_api_key: Option<OsString>,
    novita_base_url: Option<OsString>,
    novita_model: Option<OsString>,
    fireworks_api_key: Option<OsString>,
    fireworks_base_url: Option<OsString>,
    fireworks_model: Option<OsString>,
    siliconflow_api_key: Option<OsString>,
    siliconflow_base_url: Option<OsString>,
    siliconflow_model: Option<OsString>,
    arcee_api_key: Option<OsString>,
    arcee_base_url: Option<OsString>,
    arcee_model: Option<OsString>,
    moonshot_api_key: Option<OsString>,
    moonshot_base_url: Option<OsString>,
    moonshot_model: Option<OsString>,
    kimi_api_key: Option<OsString>,
    kimi_base_url: Option<OsString>,
    kimi_model: Option<OsString>,
    kimi_model_name: Option<OsString>,
    zai_api_key: Option<OsString>,
    z_ai_api_key: Option<OsString>,
    zhipu_api_key: Option<OsString>,
    glm_api_key: Option<OsString>,
    zai_base_url: Option<OsString>,
    z_ai_base_url: Option<OsString>,
    zhipu_base_url: Option<OsString>,
    zhipuai_base_url: Option<OsString>,
    bigmodel_base_url: Option<OsString>,
    zai_model: Option<OsString>,
    z_ai_model: Option<OsString>,
    zhipu_model: Option<OsString>,
    zhipuai_model: Option<OsString>,
    bigmodel_model: Option<OsString>,
    glm_model: Option<OsString>,
    stepfun_api_key: Option<OsString>,
    step_api_key: Option<OsString>,
    stepfun_base_url: Option<OsString>,
    stepfun_model: Option<OsString>,
    minimax_api_key: Option<OsString>,
    minimax_base_url: Option<OsString>,
    minimax_anthropic_base_url: Option<OsString>,
    minimax_model: Option<OsString>,
    sakana_api_key: Option<OsString>,
    fugu_api_key: Option<OsString>,
    sakana_base_url: Option<OsString>,
    sakana_model: Option<OsString>,
    sglang_api_key: Option<OsString>,
    sglang_base_url: Option<OsString>,
    vllm_api_key: Option<OsString>,
    vllm_base_url: Option<OsString>,
    ollama_cloud_api_key: Option<OsString>,
    ollama_cloud_base_url: Option<OsString>,
    ollama_cloud_model: Option<OsString>,
    ollama_api_key: Option<OsString>,
    ollama_base_url: Option<OsString>,
    huggingface_api_key: Option<OsString>,
    huggingface_token: Option<OsString>,
    huggingface_base_url: Option<OsString>,
    hf_base_url: Option<OsString>,
    huggingface_model: Option<OsString>,
    hf_model: Option<OsString>,
    codewhale_provider: Option<OsString>,
    codewhale_model: Option<OsString>,
    codewhale_base_url: Option<OsString>,
    xai_api_key: Option<OsString>,
    xai_base_url: Option<OsString>,
    xai_model: Option<OsString>,
    mistral_api_key: Option<OsString>,
    mistral_base_url: Option<OsString>,
    mistral_model: Option<OsString>,
    telecomjs_api_key: Option<OsString>,
    telecomjs_base_url: Option<OsString>,
    telecomjs_model: Option<OsString>,
    edenai_api_key: Option<OsString>,
    edenai_base_url: Option<OsString>,
    edenai_model: Option<OsString>,
    concentrate_api_key: Option<OsString>,
    concentrate_base_url: Option<OsString>,
    concentrate_model: Option<OsString>,
    opencode_go_api_key: Option<OsString>,
    opencode_go_base_url: Option<OsString>,
    opencode_go_model: Option<OsString>,
    opencode_zen_api_key: Option<OsString>,
    opencode_api_key: Option<OsString>,
    opencode_zen_base_url: Option<OsString>,
    opencode_zen_model: Option<OsString>,
    meta_model_api_key: Option<OsString>,
    model_api_key: Option<OsString>,
    meta_model_api_base_url: Option<OsString>,
    model_api_base_url: Option<OsString>,
    meta_model_api_model: Option<OsString>,
    model_api_model: Option<OsString>,
}

impl EnvGuard {
    fn without_deepseek_runtime_overrides() -> Self {
        let guard = Self {
            active_route_api_key: env::var_os("DEEPSEEK_API_KEY"),
            active_route_base_url: env::var_os("DEEPSEEK_BASE_URL"),
            deepseek_anthropic_base_url: env::var_os("DEEPSEEK_ANTHROPIC_BASE_URL"),
            deepseek_claude_base_url: env::var_os("DEEPSEEK_CLAUDE_BASE_URL"),
            deepseek_http_headers: env::var_os("DEEPSEEK_HTTP_HEADERS"),
            deepseek_model: env::var_os("DEEPSEEK_MODEL"),
            deepseek_default_text_model: env::var_os("DEEPSEEK_DEFAULT_TEXT_MODEL"),
            deepseek_provider: env::var_os("DEEPSEEK_PROVIDER"),
            deepseek_auth_mode: env::var_os("DEEPSEEK_AUTH_MODE"),
            codewhale_provider: env::var_os("CODEWHALE_PROVIDER"),
            codewhale_model: env::var_os("CODEWHALE_MODEL"),
            codewhale_base_url: env::var_os("CODEWHALE_BASE_URL"),
            xai_api_key: env::var_os("XAI_API_KEY"),
            xai_base_url: env::var_os("XAI_BASE_URL"),
            xai_model: env::var_os("XAI_MODEL"),
            mistral_api_key: env::var_os("MISTRAL_API_KEY"),
            mistral_base_url: env::var_os("MISTRAL_BASE_URL"),
            mistral_model: env::var_os("MISTRAL_MODEL"),
            telecomjs_api_key: env::var_os("TELECOMJS_API_KEY"),
            telecomjs_base_url: env::var_os("TELECOMJS_BASE_URL"),
            telecomjs_model: env::var_os("TELECOMJS_MODEL"),
            edenai_api_key: env::var_os("EDENAI_API_KEY"),
            edenai_base_url: env::var_os("EDENAI_BASE_URL"),
            edenai_model: env::var_os("EDENAI_MODEL"),
            concentrate_api_key: env::var_os("CONCENTRATE_API_KEY"),
            concentrate_base_url: env::var_os("CONCENTRATE_BASE_URL"),
            concentrate_model: env::var_os("CONCENTRATE_MODEL"),
            opencode_go_api_key: env::var_os("OPENCODE_GO_API_KEY"),
            opencode_go_base_url: env::var_os("OPENCODE_GO_BASE_URL"),
            opencode_go_model: env::var_os("OPENCODE_GO_MODEL"),
            opencode_zen_api_key: env::var_os("OPENCODE_ZEN_API_KEY"),
            opencode_api_key: env::var_os("OPENCODE_API_KEY"),
            opencode_zen_base_url: env::var_os("OPENCODE_ZEN_BASE_URL"),
            opencode_zen_model: env::var_os("OPENCODE_ZEN_MODEL"),
            meta_model_api_key: env::var_os("META_MODEL_API_KEY"),
            model_api_key: env::var_os("MODEL_API_KEY"),
            meta_model_api_base_url: env::var_os("META_MODEL_API_BASE_URL"),
            model_api_base_url: env::var_os("MODEL_API_BASE_URL"),
            meta_model_api_model: env::var_os("META_MODEL_API_MODEL"),
            model_api_model: env::var_os("MODEL_API_MODEL"),
            nvidia_api_key: env::var_os("NVIDIA_API_KEY"),
            nvidia_nim_api_key: env::var_os("NVIDIA_NIM_API_KEY"),
            nim_base_url: env::var_os("NIM_BASE_URL"),
            nvidia_base_url: env::var_os("NVIDIA_BASE_URL"),
            nvidia_nim_base_url: env::var_os("NVIDIA_NIM_BASE_URL"),
            openrouter_api_key: env::var_os("OPENROUTER_API_KEY"),
            openrouter_base_url: env::var_os("OPENROUTER_BASE_URL"),
            openrouter_model: env::var_os("OPENROUTER_MODEL"),
            openmodel_api_key: env::var_os("OPENMODEL_API_KEY"),
            openmodel_base_url: env::var_os("OPENMODEL_BASE_URL"),
            openmodel_model: env::var_os("OPENMODEL_MODEL"),
            xiaomi_mimo_token_plan_api_key: env::var_os("XIAOMI_MIMO_TOKEN_PLAN_API_KEY"),
            mimo_token_plan_api_key: env::var_os("MIMO_TOKEN_PLAN_API_KEY"),
            xiaomi_mimo_api_key: env::var_os("XIAOMI_MIMO_API_KEY"),
            xiaomi_api_key: env::var_os("XIAOMI_API_KEY"),
            mimo_api_key: env::var_os("MIMO_API_KEY"),
            xiaomi_mimo_base_url: env::var_os("XIAOMI_MIMO_BASE_URL"),
            mimo_base_url: env::var_os("MIMO_BASE_URL"),
            xiaomi_mimo_model: env::var_os("XIAOMI_MIMO_MODEL"),
            mimo_model: env::var_os("MIMO_MODEL"),
            xiaomi_mimo_mode: env::var_os("XIAOMI_MIMO_MODE"),
            mimo_mode: env::var_os("MIMO_MODE"),
            wanjie_ark_api_key: env::var_os("WANJIE_ARK_API_KEY"),
            volcengine_api_key: env::var_os("VOLCENGINE_API_KEY"),
            volcengine_ark_api_key: env::var_os("VOLCENGINE_ARK_API_KEY"),
            ark_api_key: env::var_os("ARK_API_KEY"),
            volcengine_base_url: env::var_os("VOLCENGINE_BASE_URL"),
            volcengine_ark_base_url: env::var_os("VOLCENGINE_ARK_BASE_URL"),
            ark_base_url: env::var_os("ARK_BASE_URL"),
            wanjie_ark_base_url: env::var_os("WANJIE_ARK_BASE_URL"),
            wanjie_base_url: env::var_os("WANJIE_BASE_URL"),
            wanjie_maas_base_url: env::var_os("WANJIE_MAAS_BASE_URL"),
            volcengine_model: env::var_os("VOLCENGINE_MODEL"),
            volcengine_ark_model: env::var_os("VOLCENGINE_ARK_MODEL"),
            wanjie_ark_model: env::var_os("WANJIE_ARK_MODEL"),
            wanjie_model: env::var_os("WANJIE_MODEL"),
            wanjie_maas_model: env::var_os("WANJIE_MAAS_MODEL"),
            novita_api_key: env::var_os("NOVITA_API_KEY"),
            novita_base_url: env::var_os("NOVITA_BASE_URL"),
            novita_model: env::var_os("NOVITA_MODEL"),
            fireworks_api_key: env::var_os("FIREWORKS_API_KEY"),
            fireworks_base_url: env::var_os("FIREWORKS_BASE_URL"),
            fireworks_model: env::var_os("FIREWORKS_MODEL"),
            siliconflow_api_key: env::var_os("SILICONFLOW_API_KEY"),
            siliconflow_base_url: env::var_os("SILICONFLOW_BASE_URL"),
            siliconflow_model: env::var_os("SILICONFLOW_MODEL"),
            arcee_api_key: env::var_os("ARCEE_API_KEY"),
            arcee_base_url: env::var_os("ARCEE_BASE_URL"),
            arcee_model: env::var_os("ARCEE_MODEL"),
            moonshot_api_key: env::var_os("MOONSHOT_API_KEY"),
            moonshot_base_url: env::var_os("MOONSHOT_BASE_URL"),
            moonshot_model: env::var_os("MOONSHOT_MODEL"),
            kimi_api_key: env::var_os("KIMI_API_KEY"),
            kimi_base_url: env::var_os("KIMI_BASE_URL"),
            kimi_model: env::var_os("KIMI_MODEL"),
            kimi_model_name: env::var_os("KIMI_MODEL_NAME"),
            zai_api_key: env::var_os("ZAI_API_KEY"),
            z_ai_api_key: env::var_os("Z_AI_API_KEY"),
            zhipu_api_key: env::var_os("ZHIPU_API_KEY"),
            glm_api_key: env::var_os("GLM_API_KEY"),
            zai_base_url: env::var_os("ZAI_BASE_URL"),
            z_ai_base_url: env::var_os("Z_AI_BASE_URL"),
            zhipu_base_url: env::var_os("ZHIPU_BASE_URL"),
            zhipuai_base_url: env::var_os("ZHIPUAI_BASE_URL"),
            bigmodel_base_url: env::var_os("BIGMODEL_BASE_URL"),
            zai_model: env::var_os("ZAI_MODEL"),
            z_ai_model: env::var_os("Z_AI_MODEL"),
            zhipu_model: env::var_os("ZHIPU_MODEL"),
            zhipuai_model: env::var_os("ZHIPUAI_MODEL"),
            bigmodel_model: env::var_os("BIGMODEL_MODEL"),
            glm_model: env::var_os("GLM_MODEL"),
            stepfun_api_key: env::var_os("STEPFUN_API_KEY"),
            step_api_key: env::var_os("STEP_API_KEY"),
            stepfun_base_url: env::var_os("STEPFUN_BASE_URL"),
            stepfun_model: env::var_os("STEPFUN_MODEL"),
            minimax_api_key: env::var_os("MINIMAX_API_KEY"),
            minimax_base_url: env::var_os("MINIMAX_BASE_URL"),
            minimax_anthropic_base_url: env::var_os("MINIMAX_ANTHROPIC_BASE_URL"),
            minimax_model: env::var_os("MINIMAX_MODEL"),
            sakana_api_key: env::var_os("SAKANA_API_KEY"),
            fugu_api_key: env::var_os("FUGU_API_KEY"),
            sakana_base_url: env::var_os("SAKANA_BASE_URL"),
            sakana_model: env::var_os("SAKANA_MODEL"),
            sglang_api_key: env::var_os("SGLANG_API_KEY"),
            sglang_base_url: env::var_os("SGLANG_BASE_URL"),
            vllm_api_key: env::var_os("VLLM_API_KEY"),
            vllm_base_url: env::var_os("VLLM_BASE_URL"),
            ollama_cloud_api_key: env::var_os("OLLAMA_CLOUD_API_KEY"),
            ollama_cloud_base_url: env::var_os("OLLAMA_CLOUD_BASE_URL"),
            ollama_cloud_model: env::var_os("OLLAMA_CLOUD_MODEL"),
            ollama_api_key: env::var_os("OLLAMA_API_KEY"),
            ollama_base_url: env::var_os("OLLAMA_BASE_URL"),
            huggingface_api_key: env::var_os("HUGGINGFACE_API_KEY"),
            huggingface_token: env::var_os("HF_TOKEN"),
            huggingface_base_url: env::var_os("HUGGINGFACE_BASE_URL"),
            hf_base_url: env::var_os("HF_BASE_URL"),
            huggingface_model: env::var_os("HUGGINGFACE_MODEL"),
            hf_model: env::var_os("HF_MODEL"),
        };
        // Safety: test-only environment mutation guarded by a module mutex.
        unsafe {
            env::remove_var("DEEPSEEK_API_KEY");
            env::remove_var("DEEPSEEK_BASE_URL");
            env::remove_var("DEEPSEEK_ANTHROPIC_BASE_URL");
            env::remove_var("DEEPSEEK_CLAUDE_BASE_URL");
            env::remove_var("DEEPSEEK_HTTP_HEADERS");
            env::remove_var("DEEPSEEK_MODEL");
            env::remove_var("DEEPSEEK_DEFAULT_TEXT_MODEL");
            env::remove_var("DEEPSEEK_PROVIDER");
            env::remove_var("DEEPSEEK_AUTH_MODE");
            env::remove_var("CODEWHALE_PROVIDER");
            env::remove_var("CODEWHALE_MODEL");
            env::remove_var("CODEWHALE_BASE_URL");
            env::remove_var("XAI_API_KEY");
            env::remove_var("XAI_BASE_URL");
            env::remove_var("XAI_MODEL");
            env::remove_var("MISTRAL_API_KEY");
            env::remove_var("MISTRAL_BASE_URL");
            env::remove_var("MISTRAL_MODEL");
            env::remove_var("TELECOMJS_API_KEY");
            env::remove_var("TELECOMJS_BASE_URL");
            env::remove_var("TELECOMJS_MODEL");
            env::remove_var("EDENAI_API_KEY");
            env::remove_var("EDENAI_BASE_URL");
            env::remove_var("EDENAI_MODEL");
            env::remove_var("OPENCODE_GO_API_KEY");
            env::remove_var("OPENCODE_GO_BASE_URL");
            env::remove_var("OPENCODE_GO_MODEL");
            env::remove_var("OPENCODE_ZEN_API_KEY");
            env::remove_var("OPENCODE_API_KEY");
            env::remove_var("OPENCODE_ZEN_BASE_URL");
            env::remove_var("OPENCODE_ZEN_MODEL");
            env::remove_var("META_MODEL_API_KEY");
            env::remove_var("MODEL_API_KEY");
            env::remove_var("META_MODEL_API_BASE_URL");
            env::remove_var("MODEL_API_BASE_URL");
            env::remove_var("META_MODEL_API_MODEL");
            env::remove_var("MODEL_API_MODEL");
            env::remove_var("NVIDIA_API_KEY");
            env::remove_var("NVIDIA_NIM_API_KEY");
            env::remove_var("NIM_BASE_URL");
            env::remove_var("NVIDIA_BASE_URL");
            env::remove_var("NVIDIA_NIM_BASE_URL");
            env::remove_var("OPENROUTER_API_KEY");
            env::remove_var("OPENROUTER_BASE_URL");
            env::remove_var("OPENROUTER_MODEL");
            env::remove_var("OPENMODEL_API_KEY");
            env::remove_var("OPENMODEL_BASE_URL");
            env::remove_var("OPENMODEL_MODEL");
            env::remove_var("XIAOMI_MIMO_TOKEN_PLAN_API_KEY");
            env::remove_var("MIMO_TOKEN_PLAN_API_KEY");
            env::remove_var("XIAOMI_MIMO_API_KEY");
            env::remove_var("XIAOMI_API_KEY");
            env::remove_var("MIMO_API_KEY");
            env::remove_var("XIAOMI_MIMO_BASE_URL");
            env::remove_var("MIMO_BASE_URL");
            env::remove_var("XIAOMI_MIMO_MODEL");
            env::remove_var("MIMO_MODEL");
            env::remove_var("XIAOMI_MIMO_MODE");
            env::remove_var("MIMO_MODE");
            env::remove_var("WANJIE_ARK_API_KEY");
            env::remove_var("VOLCENGINE_API_KEY");
            env::remove_var("VOLCENGINE_ARK_API_KEY");
            env::remove_var("ARK_API_KEY");
            env::remove_var("VOLCENGINE_BASE_URL");
            env::remove_var("VOLCENGINE_ARK_BASE_URL");
            env::remove_var("ARK_BASE_URL");
            env::remove_var("WANJIE_ARK_BASE_URL");
            env::remove_var("WANJIE_BASE_URL");
            env::remove_var("WANJIE_MAAS_BASE_URL");
            env::remove_var("VOLCENGINE_MODEL");
            env::remove_var("VOLCENGINE_ARK_MODEL");
            env::remove_var("WANJIE_ARK_MODEL");
            env::remove_var("WANJIE_MODEL");
            env::remove_var("WANJIE_MAAS_MODEL");
            env::remove_var("NOVITA_API_KEY");
            env::remove_var("NOVITA_BASE_URL");
            env::remove_var("NOVITA_MODEL");
            env::remove_var("FIREWORKS_API_KEY");
            env::remove_var("FIREWORKS_BASE_URL");
            env::remove_var("FIREWORKS_MODEL");
            env::remove_var("SILICONFLOW_API_KEY");
            env::remove_var("SILICONFLOW_BASE_URL");
            env::remove_var("SILICONFLOW_MODEL");
            env::remove_var("ARCEE_API_KEY");
            env::remove_var("ARCEE_BASE_URL");
            env::remove_var("ARCEE_MODEL");
            env::remove_var("MOONSHOT_API_KEY");
            env::remove_var("MOONSHOT_BASE_URL");
            env::remove_var("MOONSHOT_MODEL");
            env::remove_var("KIMI_API_KEY");
            env::remove_var("KIMI_BASE_URL");
            env::remove_var("KIMI_MODEL");
            env::remove_var("KIMI_MODEL_NAME");
            env::remove_var("ZAI_API_KEY");
            env::remove_var("Z_AI_API_KEY");
            env::remove_var("ZHIPU_API_KEY");
            env::remove_var("GLM_API_KEY");
            env::remove_var("ZAI_BASE_URL");
            env::remove_var("Z_AI_BASE_URL");
            env::remove_var("ZHIPU_BASE_URL");
            env::remove_var("ZHIPUAI_BASE_URL");
            env::remove_var("BIGMODEL_BASE_URL");
            env::remove_var("ZAI_MODEL");
            env::remove_var("Z_AI_MODEL");
            env::remove_var("ZHIPU_MODEL");
            env::remove_var("ZHIPUAI_MODEL");
            env::remove_var("BIGMODEL_MODEL");
            env::remove_var("GLM_MODEL");
            env::remove_var("STEPFUN_API_KEY");
            env::remove_var("STEP_API_KEY");
            env::remove_var("STEPFUN_BASE_URL");
            env::remove_var("STEPFUN_MODEL");
            env::remove_var("MINIMAX_API_KEY");
            env::remove_var("MINIMAX_BASE_URL");
            env::remove_var("MINIMAX_ANTHROPIC_BASE_URL");
            env::remove_var("MINIMAX_MODEL");
            env::remove_var("SAKANA_API_KEY");
            env::remove_var("FUGU_API_KEY");
            env::remove_var("SAKANA_BASE_URL");
            env::remove_var("SAKANA_MODEL");
            env::remove_var("SGLANG_API_KEY");
            env::remove_var("SGLANG_BASE_URL");
            env::remove_var("VLLM_API_KEY");
            env::remove_var("VLLM_BASE_URL");
            env::remove_var("OLLAMA_CLOUD_API_KEY");
            env::remove_var("OLLAMA_CLOUD_BASE_URL");
            env::remove_var("OLLAMA_CLOUD_MODEL");
            env::remove_var("OLLAMA_API_KEY");
            env::remove_var("OLLAMA_BASE_URL");
            env::remove_var("HUGGINGFACE_API_KEY");
            env::remove_var("HF_TOKEN");
            env::remove_var("HUGGINGFACE_BASE_URL");
            env::remove_var("HF_BASE_URL");
            env::remove_var("HUGGINGFACE_MODEL");
            env::remove_var("HF_MODEL");
        }
        guard
    }

    unsafe fn restore_var(key: &str, value: Option<OsString>) {
        if let Some(value) = value {
            unsafe { env::set_var(key, value) };
        } else {
            unsafe { env::remove_var(key) };
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // Safety: test-only environment mutation guarded by a module mutex.
        unsafe {
            Self::restore_var("DEEPSEEK_API_KEY", self.active_route_api_key.take());
            Self::restore_var("DEEPSEEK_BASE_URL", self.active_route_base_url.take());
            Self::restore_var(
                "DEEPSEEK_ANTHROPIC_BASE_URL",
                self.deepseek_anthropic_base_url.take(),
            );
            Self::restore_var(
                "DEEPSEEK_CLAUDE_BASE_URL",
                self.deepseek_claude_base_url.take(),
            );
            Self::restore_var("DEEPSEEK_HTTP_HEADERS", self.deepseek_http_headers.take());
            Self::restore_var("DEEPSEEK_MODEL", self.deepseek_model.take());
            Self::restore_var(
                "DEEPSEEK_DEFAULT_TEXT_MODEL",
                self.deepseek_default_text_model.take(),
            );
            Self::restore_var("DEEPSEEK_PROVIDER", self.deepseek_provider.take());
            Self::restore_var("DEEPSEEK_AUTH_MODE", self.deepseek_auth_mode.take());
            Self::restore_var("CODEWHALE_PROVIDER", self.codewhale_provider.take());
            Self::restore_var("CODEWHALE_MODEL", self.codewhale_model.take());
            Self::restore_var("CODEWHALE_BASE_URL", self.codewhale_base_url.take());
            Self::restore_var("XAI_API_KEY", self.xai_api_key.take());
            Self::restore_var("XAI_BASE_URL", self.xai_base_url.take());
            Self::restore_var("XAI_MODEL", self.xai_model.take());
            Self::restore_var("MISTRAL_API_KEY", self.mistral_api_key.take());
            Self::restore_var("MISTRAL_BASE_URL", self.mistral_base_url.take());
            Self::restore_var("MISTRAL_MODEL", self.mistral_model.take());
            Self::restore_var("TELECOMJS_API_KEY", self.telecomjs_api_key.take());
            Self::restore_var("TELECOMJS_BASE_URL", self.telecomjs_base_url.take());
            Self::restore_var("TELECOMJS_MODEL", self.telecomjs_model.take());
            Self::restore_var("EDENAI_API_KEY", self.edenai_api_key.take());
            Self::restore_var("EDENAI_BASE_URL", self.edenai_base_url.take());
            Self::restore_var("EDENAI_MODEL", self.edenai_model.take());
            Self::restore_var("CONCENTRATE_API_KEY", self.concentrate_api_key.take());
            Self::restore_var("CONCENTRATE_BASE_URL", self.concentrate_base_url.take());
            Self::restore_var("CONCENTRATE_MODEL", self.concentrate_model.take());
            Self::restore_var("OPENCODE_GO_API_KEY", self.opencode_go_api_key.take());
            Self::restore_var("OPENCODE_GO_BASE_URL", self.opencode_go_base_url.take());
            Self::restore_var("OPENCODE_GO_MODEL", self.opencode_go_model.take());
            Self::restore_var("OPENCODE_ZEN_API_KEY", self.opencode_zen_api_key.take());
            Self::restore_var("OPENCODE_API_KEY", self.opencode_api_key.take());
            Self::restore_var("OPENCODE_ZEN_BASE_URL", self.opencode_zen_base_url.take());
            Self::restore_var("OPENCODE_ZEN_MODEL", self.opencode_zen_model.take());
            Self::restore_var("META_MODEL_API_KEY", self.meta_model_api_key.take());
            Self::restore_var("MODEL_API_KEY", self.model_api_key.take());
            Self::restore_var(
                "META_MODEL_API_BASE_URL",
                self.meta_model_api_base_url.take(),
            );
            Self::restore_var("MODEL_API_BASE_URL", self.model_api_base_url.take());
            Self::restore_var("META_MODEL_API_MODEL", self.meta_model_api_model.take());
            Self::restore_var("MODEL_API_MODEL", self.model_api_model.take());
            Self::restore_var("NVIDIA_API_KEY", self.nvidia_api_key.take());
            Self::restore_var("NVIDIA_NIM_API_KEY", self.nvidia_nim_api_key.take());
            Self::restore_var("NIM_BASE_URL", self.nim_base_url.take());
            Self::restore_var("NVIDIA_BASE_URL", self.nvidia_base_url.take());
            Self::restore_var("NVIDIA_NIM_BASE_URL", self.nvidia_nim_base_url.take());
            Self::restore_var("OPENROUTER_API_KEY", self.openrouter_api_key.take());
            Self::restore_var("OPENROUTER_BASE_URL", self.openrouter_base_url.take());
            Self::restore_var("OPENROUTER_MODEL", self.openrouter_model.take());
            Self::restore_var("OPENMODEL_API_KEY", self.openmodel_api_key.take());
            Self::restore_var("OPENMODEL_BASE_URL", self.openmodel_base_url.take());
            Self::restore_var("OPENMODEL_MODEL", self.openmodel_model.take());
            Self::restore_var(
                "XIAOMI_MIMO_TOKEN_PLAN_API_KEY",
                self.xiaomi_mimo_token_plan_api_key.take(),
            );
            Self::restore_var(
                "MIMO_TOKEN_PLAN_API_KEY",
                self.mimo_token_plan_api_key.take(),
            );
            Self::restore_var("XIAOMI_MIMO_API_KEY", self.xiaomi_mimo_api_key.take());
            Self::restore_var("XIAOMI_API_KEY", self.xiaomi_api_key.take());
            Self::restore_var("MIMO_API_KEY", self.mimo_api_key.take());
            Self::restore_var("XIAOMI_MIMO_BASE_URL", self.xiaomi_mimo_base_url.take());
            Self::restore_var("MIMO_BASE_URL", self.mimo_base_url.take());
            Self::restore_var("XIAOMI_MIMO_MODEL", self.xiaomi_mimo_model.take());
            Self::restore_var("MIMO_MODEL", self.mimo_model.take());
            Self::restore_var("XIAOMI_MIMO_MODE", self.xiaomi_mimo_mode.take());
            Self::restore_var("MIMO_MODE", self.mimo_mode.take());
            Self::restore_var("WANJIE_ARK_API_KEY", self.wanjie_ark_api_key.take());
            Self::restore_var("VOLCENGINE_API_KEY", self.volcengine_api_key.take());
            Self::restore_var("VOLCENGINE_ARK_API_KEY", self.volcengine_ark_api_key.take());
            Self::restore_var("ARK_API_KEY", self.ark_api_key.take());
            Self::restore_var("VOLCENGINE_BASE_URL", self.volcengine_base_url.take());
            Self::restore_var(
                "VOLCENGINE_ARK_BASE_URL",
                self.volcengine_ark_base_url.take(),
            );
            Self::restore_var("ARK_BASE_URL", self.ark_base_url.take());
            Self::restore_var("WANJIE_ARK_BASE_URL", self.wanjie_ark_base_url.take());
            Self::restore_var("WANJIE_BASE_URL", self.wanjie_base_url.take());
            Self::restore_var("WANJIE_MAAS_BASE_URL", self.wanjie_maas_base_url.take());
            Self::restore_var("VOLCENGINE_MODEL", self.volcengine_model.take());
            Self::restore_var("VOLCENGINE_ARK_MODEL", self.volcengine_ark_model.take());
            Self::restore_var("WANJIE_ARK_MODEL", self.wanjie_ark_model.take());
            Self::restore_var("WANJIE_MODEL", self.wanjie_model.take());
            Self::restore_var("WANJIE_MAAS_MODEL", self.wanjie_maas_model.take());
            Self::restore_var("NOVITA_API_KEY", self.novita_api_key.take());
            Self::restore_var("NOVITA_BASE_URL", self.novita_base_url.take());
            Self::restore_var("NOVITA_MODEL", self.novita_model.take());
            Self::restore_var("FIREWORKS_API_KEY", self.fireworks_api_key.take());
            Self::restore_var("FIREWORKS_BASE_URL", self.fireworks_base_url.take());
            Self::restore_var("FIREWORKS_MODEL", self.fireworks_model.take());
            Self::restore_var("SILICONFLOW_API_KEY", self.siliconflow_api_key.take());
            Self::restore_var("SILICONFLOW_BASE_URL", self.siliconflow_base_url.take());
            Self::restore_var("SILICONFLOW_MODEL", self.siliconflow_model.take());
            Self::restore_var("ARCEE_API_KEY", self.arcee_api_key.take());
            Self::restore_var("ARCEE_BASE_URL", self.arcee_base_url.take());
            Self::restore_var("ARCEE_MODEL", self.arcee_model.take());
            Self::restore_var("MOONSHOT_API_KEY", self.moonshot_api_key.take());
            Self::restore_var("MOONSHOT_BASE_URL", self.moonshot_base_url.take());
            Self::restore_var("MOONSHOT_MODEL", self.moonshot_model.take());
            Self::restore_var("KIMI_API_KEY", self.kimi_api_key.take());
            Self::restore_var("KIMI_BASE_URL", self.kimi_base_url.take());
            Self::restore_var("KIMI_MODEL", self.kimi_model.take());
            Self::restore_var("KIMI_MODEL_NAME", self.kimi_model_name.take());
            Self::restore_var("ZAI_API_KEY", self.zai_api_key.take());
            Self::restore_var("Z_AI_API_KEY", self.z_ai_api_key.take());
            Self::restore_var("ZHIPU_API_KEY", self.zhipu_api_key.take());
            Self::restore_var("GLM_API_KEY", self.glm_api_key.take());
            Self::restore_var("ZAI_BASE_URL", self.zai_base_url.take());
            Self::restore_var("Z_AI_BASE_URL", self.z_ai_base_url.take());
            Self::restore_var("ZHIPU_BASE_URL", self.zhipu_base_url.take());
            Self::restore_var("ZHIPUAI_BASE_URL", self.zhipuai_base_url.take());
            Self::restore_var("BIGMODEL_BASE_URL", self.bigmodel_base_url.take());
            Self::restore_var("ZAI_MODEL", self.zai_model.take());
            Self::restore_var("Z_AI_MODEL", self.z_ai_model.take());
            Self::restore_var("ZHIPU_MODEL", self.zhipu_model.take());
            Self::restore_var("ZHIPUAI_MODEL", self.zhipuai_model.take());
            Self::restore_var("BIGMODEL_MODEL", self.bigmodel_model.take());
            Self::restore_var("GLM_MODEL", self.glm_model.take());
            Self::restore_var("STEPFUN_API_KEY", self.stepfun_api_key.take());
            Self::restore_var("STEP_API_KEY", self.step_api_key.take());
            Self::restore_var("STEPFUN_BASE_URL", self.stepfun_base_url.take());
            Self::restore_var("STEPFUN_MODEL", self.stepfun_model.take());
            Self::restore_var("MINIMAX_API_KEY", self.minimax_api_key.take());
            Self::restore_var("MINIMAX_BASE_URL", self.minimax_base_url.take());
            Self::restore_var(
                "MINIMAX_ANTHROPIC_BASE_URL",
                self.minimax_anthropic_base_url.take(),
            );
            Self::restore_var("MINIMAX_MODEL", self.minimax_model.take());
            Self::restore_var("SAKANA_API_KEY", self.sakana_api_key.take());
            Self::restore_var("FUGU_API_KEY", self.fugu_api_key.take());
            Self::restore_var("SAKANA_BASE_URL", self.sakana_base_url.take());
            Self::restore_var("SAKANA_MODEL", self.sakana_model.take());
            Self::restore_var("SGLANG_API_KEY", self.sglang_api_key.take());
            Self::restore_var("SGLANG_BASE_URL", self.sglang_base_url.take());
            Self::restore_var("VLLM_API_KEY", self.vllm_api_key.take());
            Self::restore_var("VLLM_BASE_URL", self.vllm_base_url.take());
            Self::restore_var("OLLAMA_CLOUD_API_KEY", self.ollama_cloud_api_key.take());
            Self::restore_var("OLLAMA_CLOUD_BASE_URL", self.ollama_cloud_base_url.take());
            Self::restore_var("OLLAMA_CLOUD_MODEL", self.ollama_cloud_model.take());
            Self::restore_var("OLLAMA_API_KEY", self.ollama_api_key.take());
            Self::restore_var("OLLAMA_BASE_URL", self.ollama_base_url.take());
            Self::restore_var("HUGGINGFACE_API_KEY", self.huggingface_api_key.take());
            Self::restore_var("HF_TOKEN", self.huggingface_token.take());
            Self::restore_var("HUGGINGFACE_BASE_URL", self.huggingface_base_url.take());
            Self::restore_var("HF_BASE_URL", self.hf_base_url.take());
            Self::restore_var("HUGGINGFACE_MODEL", self.huggingface_model.take());
            Self::restore_var("HF_MODEL", self.hf_model.take());
        }
    }
}

struct RecordingSecretsStore {
    gets: Mutex<Vec<String>>,
    sets: Mutex<Vec<String>>,
    deletes: Mutex<Vec<String>>,
    value: Option<String>,
    values: std::collections::HashMap<String, String>,
}

impl RecordingSecretsStore {
    fn with_value(value: &str) -> Self {
        Self {
            gets: Mutex::new(Vec::new()),
            sets: Mutex::new(Vec::new()),
            deletes: Mutex::new(Vec::new()),
            value: Some(value.to_string()),
            values: std::collections::HashMap::new(),
        }
    }

    fn with_entries(entries: &[(&str, &str)]) -> Self {
        Self {
            gets: Mutex::new(Vec::new()),
            sets: Mutex::new(Vec::new()),
            deletes: Mutex::new(Vec::new()),
            value: None,
            values: entries
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        }
    }

    fn empty() -> Self {
        Self::with_entries(&[])
    }
}

impl codewhale_secrets::KeyringStore for RecordingSecretsStore {
    fn get(&self, key: &str) -> Result<Option<String>, codewhale_secrets::SecretsError> {
        self.gets.lock().unwrap().push(key.to_string());
        Ok(self.values.get(key).cloned().or_else(|| self.value.clone()))
    }

    fn set(&self, key: &str, _value: &str) -> Result<(), codewhale_secrets::SecretsError> {
        self.sets.lock().unwrap().push(key.to_string());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), codewhale_secrets::SecretsError> {
        self.deletes.lock().unwrap().push(key.to_string());
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "recording"
    }
}

#[test]
fn root_deepseek_fields_are_runtime_fallbacks() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // The legacy top-level shape (#6394): parsing moves both keys into
    // `[providers.deepseek]`, so the runtime resolves them from there.
    let config = crate::parse_config_toml(
        "api_key = \"root-key\"\nbase_url = \"https://api.deepseek.com\"\ndefault_text_model = \"deepseek-v4-pro\"\n",
    )
    .expect("legacy config parses");
    assert_eq!(
        config.providers.deepseek.api_key.as_deref(),
        Some("root-key")
    );
    assert!(!config.extras.contains_key("api_key") && !config.extras.contains_key("base_url"));

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Deepseek);
    assert_eq!(resolved.api_key.as_deref(), Some("root-key"));
    assert_eq!(resolved.base_url, "https://api.deepseek.com");
    assert_eq!(resolved.model, "deepseek-v4-pro");
}

#[test]
fn deepseek_runtime_defaults_to_beta_endpoint() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml::default();

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Deepseek);
    assert_eq!(resolved.base_url, DEFAULT_DEEPSEEK_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_DEEPSEEK_MODEL);
}

#[test]
fn conflicting_top_level_and_table_values_resolve_like_the_tui() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Both shapes at once (#6394): the table's endpoint wins, and the
    // top-level key wins because that is the key the TUI always sent. The
    // dispatcher used to report the table key instead.
    let config = crate::parse_config_toml(
        r#"
api_key = "root-key"
base_url = "https://api.deepseek.com"
default_text_model = "deepseek-v4-pro"

[providers.deepseek]
api_key = "provider-key"
base_url = "https://gateway.example/v1"
model = "deepseek-v4-flash"
"#,
    )
    .expect("legacy config parses");

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.api_key.as_deref(), Some("root-key"));
    assert_eq!(resolved.base_url, "https://gateway.example/v1");
    assert_eq!(resolved.model, "deepseek-v4-flash");
}

#[test]
fn provider_http_headers_override_root_headers() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        default_text_model: Some("deepseek-v4-pro".to_string()),
        ..ConfigToml::default()
    };
    config.providers.deepseek.api_key = Some("provider-key".to_string());
    config.providers.deepseek.base_url = Some("https://gateway.example/v1".to_string());
    config.providers.deepseek.model = Some("deepseek-v4-flash".to_string());
    config
        .http_headers
        .insert("X-Shared".to_string(), "root".to_string());
    config
        .providers
        .deepseek
        .http_headers
        .insert("X-Model-Provider-Id".to_string(), "tongyi".to_string());
    config
        .providers
        .deepseek
        .http_headers
        .insert("X-Shared".to_string(), "provider".to_string());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.api_key.as_deref(), Some("provider-key"));
    assert_eq!(resolved.base_url, "https://gateway.example/v1");
    assert_eq!(resolved.model, "deepseek-v4-flash");
    assert_eq!(
        resolved
            .http_headers
            .get("X-Model-Provider-Id")
            .map(String::as_str),
        Some("tongyi")
    );
    assert_eq!(
        resolved.http_headers.get("X-Shared").map(String::as_str),
        Some("provider")
    );
}

#[test]
fn insecure_skip_tls_verify_resolves_only_for_active_provider() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        provider: ProviderKind::Openai,
        ..ConfigToml::default()
    };
    config.providers.deepseek.insecure_skip_tls_verify = Some(true);

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Openai);
    assert!(!resolved.insecure_skip_tls_verify);

    config.providers.openai.insecure_skip_tls_verify = Some(true);
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Openai);
    assert!(resolved.insecure_skip_tls_verify);
}

/// #5991: `allow_insecure_http` is a per-provider key again — parsed from the
/// `[providers.<name>]` table, settable/unsettable through `config set`, and
/// listed in the custom-provider field hint. It must stay independent of
/// `insecure_skip_tls_verify` (which only relaxes TLS verification).
#[test]
fn allow_insecure_http_round_trips_and_stays_distinct_from_tls_verify() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    let raw = r#"
provider = "openai"
[providers.openai]
base_url = "http://192.168.0.110:8000/v1"
allow_insecure_http = true
insecure_skip_tls_verify = false
"#;
    let config: ConfigToml = toml::from_str(raw).expect("parses");
    assert_eq!(config.providers.openai.allow_insecure_http, Some(true));
    assert_eq!(
        config.providers.openai.insecure_skip_tls_verify,
        Some(false)
    );

    // `config set providers.openai.allow_insecure_http true` lands in the
    // same field and `config get` reads it back.
    let mut set_target = ConfigToml::default();
    set_provider_config_value(
        &mut set_target,
        ProviderKind::Openai,
        ProviderConfigField::AllowInsecureHttp,
        "true",
    )
    .expect("set accepts the key");
    assert_eq!(set_target.providers.openai.allow_insecure_http, Some(true));
    assert!(
        set_target
            .providers
            .openai
            .insecure_skip_tls_verify
            .is_none()
    );
    assert_eq!(
        get_provider_config_value(
            &set_target.providers.openai,
            ProviderConfigField::AllowInsecureHttp
        ),
        Some("true".to_string())
    );

    unset_provider_config_value(
        &mut set_target,
        ProviderKind::Openai,
        ProviderConfigField::AllowInsecureHttp,
    );
    assert_eq!(set_target.providers.openai.allow_insecure_http, None);

    assert!(CUSTOM_PROVIDER_FIELD_HINT.contains("allow_insecure_http"));
}

#[test]
fn openai_provider_accepts_dashscope_bailian_base_url_and_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        provider: ProviderKind::Openai,
        ..ConfigToml::default()
    };
    config.providers.openai.api_key = Some("dashscope-table-key".to_string());
    config.providers.openai.base_url =
        Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1".to_string());
    config.providers.openai.model = Some("qwen-plus".to_string());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Openai);
    assert_eq!(resolved.api_key.as_deref(), Some("dashscope-table-key"));
    assert_eq!(
        resolved.base_url,
        "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"
    );
    assert_eq!(resolved.model, "qwen-plus");
}

#[test]
fn http_headers_env_overrides_config() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml::default();
    config
        .http_headers
        .insert("X-Model-Provider-Id".to_string(), "from-file".to_string());
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("DEEPSEEK_HTTP_HEADERS", "X-Model-Provider-Id=from-env");
    }

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(
        resolved
            .http_headers
            .get("X-Model-Provider-Id")
            .map(String::as_str),
        Some("from-env")
    );
}

#[test]
fn yolo_env_var_prefers_codewhale_and_keeps_deepseek_alias() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let codewhale_prev = env::var_os("CODEWHALE_YOLO");
    let deepseek_prev = env::var_os("DEEPSEEK_YOLO");
    let config = ConfigToml::default();

    // Only the canonical name is set.
    unsafe {
        env::set_var("CODEWHALE_YOLO", "true");
        env::remove_var("DEEPSEEK_YOLO");
    }
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(
        resolved.yolo,
        Some(true),
        "CODEWHALE_YOLO=true must enable the yolo posture"
    );

    // Only the deprecated alias is set: it must keep working through 0.9.x.
    unsafe {
        env::remove_var("CODEWHALE_YOLO");
        env::set_var("DEEPSEEK_YOLO", "true");
    }
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(
        resolved.yolo,
        Some(true),
        "DEEPSEEK_YOLO remains a read-only deprecated alias until 0.10 (#5443)"
    );

    // Both set: the canonical name wins.
    unsafe { env::set_var("CODEWHALE_YOLO", "false") };
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(
        resolved.yolo,
        Some(false),
        "CODEWHALE_YOLO must win over the deprecated DEEPSEEK_YOLO alias"
    );

    unsafe {
        EnvGuard::restore_var("CODEWHALE_YOLO", codewhale_prev);
        EnvGuard::restore_var("DEEPSEEK_YOLO", deepseek_prev);
    }
}

#[test]
fn nvidia_nim_provider_defaults_to_catalog_endpoint_and_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::NvidiaNim,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::NvidiaNim);
    assert_eq!(resolved.base_url, DEFAULT_NVIDIA_NIM_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_NVIDIA_NIM_MODEL);
}

#[test]
fn nvidia_nim_provider_uses_provider_specific_credentials() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        provider: ProviderKind::NvidiaNim,
        ..ConfigToml::default()
    };
    config.providers.nvidia_nim.api_key = Some("nim-key".to_string());
    config.providers.nvidia_nim.base_url = Some("https://nim.example/v1".to_string());
    config.providers.nvidia_nim.model = Some("deepseek-ai/deepseek-v4-pro".to_string());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::NvidiaNim);
    assert_eq!(resolved.api_key.as_deref(), Some("nim-key"));
    assert_eq!(resolved.base_url, "https://nim.example/v1");
    assert_eq!(resolved.model, "deepseek-ai/deepseek-v4-pro");
}

#[test]
fn multiword_provider_sections_accept_the_kebab_canonical_id() {
    // The canonical provider ids are kebab (`nvidia-nim`, `wanjie-ark`,
    // `xiaomi-mimo`) everywhere users see them; before 2026-08-04 the TOML
    // fields carried only the snake_case name, so a `[providers.nvidia-nim]`
    // section parsed into nothing and the override was silently lost.
    let toml_src = "\
[providers.nvidia-nim]
model = \"nim-kebab-model\"

[providers.wanjie-ark]
model = \"wanjie-kebab-model\"

[providers.xiaomi-mimo]
model = \"mimo-kebab-model\"
";
    let parsed: ConfigToml = toml::from_str(toml_src).expect("kebab provider sections parse");
    assert_eq!(
        parsed.providers.nvidia_nim.model.as_deref(),
        Some("nim-kebab-model")
    );
    assert_eq!(
        parsed.providers.wanjie_ark.model.as_deref(),
        Some("wanjie-kebab-model")
    );
    assert_eq!(
        parsed.providers.xiaomi_mimo.model.as_deref(),
        Some("mimo-kebab-model")
    );
}

#[test]
fn nvidia_nim_provider_normalizes_flash_aliases() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let cli = CliRuntimeOverrides {
        provider: Some(ProviderKind::NvidiaNim),
        model: Some("deepseek-v4-flash".to_string()),
        ..CliRuntimeOverrides::default()
    };

    let resolved = ConfigToml::default().resolve_runtime_options(&cli);

    assert_eq!(resolved.provider, ProviderKind::NvidiaNim);
    assert_eq!(resolved.model, DEFAULT_NVIDIA_NIM_FLASH_MODEL);
}

#[test]
fn nvidia_nim_custom_env_url_does_not_inherit_ambient_credentials() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "nvidia-nim");
        env::set_var("NVIDIA_API_KEY", "nim-env-key");
        env::set_var("NVIDIA_NIM_BASE_URL", "https://nim-env.example/v1");
    }

    let config = ConfigToml::default();
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::NvidiaNim);
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
    assert_eq!(resolved.base_url, "https://nim-env.example/v1");
    assert_eq!(resolved.model, DEFAULT_NVIDIA_NIM_MODEL);
}

#[test]
fn nvidia_nim_provider_accepts_short_nim_base_url_alias() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "nvidia-nim");
        env::set_var("NVIDIA_API_KEY", "nim-env-key");
        env::set_var("NIM_BASE_URL", "https://short-nim.example/v1");
    }

    let config = ConfigToml::default();
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::NvidiaNim);
    assert_eq!(resolved.base_url, "https://short-nim.example/v1");
}

#[test]
fn nvidia_nim_provider_does_not_fallback_to_deepseek_api_key_env() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "nvidia-nim");
        env::set_var("DEEPSEEK_API_KEY", "deepseek-compat-key");
    }

    let config = ConfigToml::default();
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::NvidiaNim);
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
}

#[test]
fn list_values_redacts_root_api_key() {
    let config = crate::parse_config_toml("api_key = \"sk-deepseek-secret\"\n").unwrap();

    let values = config.list_values();

    assert!(!values.contains_key("api_key"));
    assert_eq!(
        values.get("providers.deepseek.api_key").map(String::as_str),
        Some("sk-d***cret")
    );
}

#[test]
fn list_values_fully_redacts_short_api_key() {
    let mut config = ConfigToml::default();
    config.providers.deepseek.api_key = Some("short-key".to_string());

    let values = config.list_values();

    assert_eq!(
        values.get("providers.deepseek.api_key").map(String::as_str),
        Some("********")
    );
}

#[test]
fn redacted_toml_value_keeps_shape_but_not_secret_bytes() {
    let mut config = ConfigToml {
        model: Some("deepseek-v4-pro".to_string()),
        ..ConfigToml::default()
    };
    config.providers.deepseek.api_key = Some("sk-deepseek-secret-value".to_string());
    config.providers.openrouter.api_key = Some("openrouter-secret-value".to_string());

    let value = config.redacted_toml_value();
    let table = value.as_table().expect("dump renders a table");
    let api_key = table["providers"]["deepseek"]
        .get("api_key")
        .and_then(toml::Value::as_str)
        .expect("api_key keeps its slot");
    assert!(!api_key.contains("secret"), "{api_key}");
    let rendered = toml::to_string_pretty(&value).expect("dump serializes");
    assert!(!rendered.contains("secret-value"), "{rendered}");
    assert!(rendered.contains("deepseek-v4-pro"), "{rendered}");
}

#[test]
fn get_display_value_redacts_sensitive_keys() {
    let mut config = ConfigToml::default();
    config.providers.deepseek.api_key = Some("sk-deepseek-secret".to_string());
    config.providers.openrouter.api_key = Some("openrouter-secret-value".to_string());
    config.model = Some("deepseek-v4-pro".to_string());

    // `api_key` at the top level names the active provider's table (#6394).
    assert_eq!(
        config.get_display_value("api_key").as_deref(),
        Some("sk-d***cret")
    );
    assert_eq!(
        config
            .get_display_value("providers.openrouter.api_key")
            .as_deref(),
        Some("open***alue")
    );
    assert_eq!(
        config.get_display_value("model").as_deref(),
        Some("deepseek-v4-pro")
    );
}

#[test]
fn stream_chunk_timeout_display_defaults_to_900_for_flat_key() {
    let config = ConfigToml::default();

    assert_eq!(
        config
            .get_display_value("stream_chunk_timeout_secs")
            .as_deref(),
        Some("900")
    );
}

#[test]
fn stream_chunk_timeout_display_reads_tui_table_for_flat_key() {
    let config: ConfigToml = toml::from_str(
        r#"
        [tui]
        stream_chunk_timeout_secs = 1200
        "#,
    )
    .expect("config toml");

    assert_eq!(
        config
            .get_display_value("stream_chunk_timeout_secs")
            .as_deref(),
        Some("1200")
    );
}

#[test]
fn stream_chunk_timeout_display_supports_dotted_tui_key() {
    let config: ConfigToml = toml::from_str(
        r#"
        [tui]
        stream_chunk_timeout_secs = 1200
        "#,
    )
    .expect("config toml");

    assert_eq!(
        config
            .get_display_value("tui.stream_chunk_timeout_secs")
            .as_deref(),
        Some("1200")
    );
}

#[test]
fn stream_chunk_timeout_display_zero_maps_to_default_and_clamps() {
    let zero: ConfigToml = toml::from_str(
        r#"
        [tui]
        stream_chunk_timeout_secs = 0
        "#,
    )
    .expect("zero config toml");
    assert_eq!(
        zero.get_display_value("stream_chunk_timeout_secs")
            .as_deref(),
        Some("900")
    );

    let high: ConfigToml = toml::from_str(
        r#"
        [tui]
        stream_chunk_timeout_secs = 9999
        "#,
    )
    .expect("high config toml");
    assert_eq!(
        high.get_display_value("stream_chunk_timeout_secs")
            .as_deref(),
        Some("3600")
    );
}

#[test]
fn config_display_redacts_nested_extra_secrets() {
    let mut config = ConfigToml::default();
    let mut profile = toml::map::Map::new();
    profile.insert(
        "chatgpt_access_token".to_string(),
        toml::Value::String("raw-chatgpt-access-token-value".to_string()),
    );
    profile.insert(
        "safe_label".to_string(),
        toml::Value::String("visible".to_string()),
    );

    let mut nested = toml::map::Map::new();
    nested.insert(
        "refresh_token".to_string(),
        toml::Value::String("raw-refresh-token-value".to_string()),
    );
    nested.insert("expires_at".to_string(), toml::Value::Integer(1234));
    profile.insert("session".to_string(), toml::Value::Table(nested));

    config
        .extras
        .insert("extras".to_string(), toml::Value::Table(profile));

    let listed = config.list_values();
    let rendered = listed.get("extras").expect("extras are listed");

    assert!(rendered.contains("chatgpt_access_token"));
    assert!(rendered.contains("refresh_token"));
    assert!(rendered.contains("safe_label = \"visible\""));
    assert!(!rendered.contains("raw-chatgpt-access-token-value"));
    assert!(!rendered.contains("raw-refresh-token-value"));

    let display = config
        .get_display_value("extras")
        .expect("extras display value");
    assert!(!display.contains("raw-chatgpt-access-token-value"));
    assert!(!display.contains("raw-refresh-token-value"));
}

#[test]
fn config_display_redacts_sensitive_extra_leaf_keys_and_headers() {
    let mut config = ConfigToml::default();
    config.extras.insert(
        "chatgpt_access_token".to_string(),
        toml::Value::String("raw-chatgpt-token-value".to_string()),
    );
    config.http_headers.insert(
        "Authorization".to_string(),
        "Bearer raw-header-token".to_string(),
    );
    config
        .http_headers
        .insert("X-Test".to_string(), "ok".to_string());

    assert_eq!(
        config.get_display_value("chatgpt_access_token").as_deref(),
        Some("\"raw-***alue\"")
    );

    let headers = config
        .list_values()
        .get("http_headers")
        .expect("headers are listed")
        .clone();
    assert!(headers.contains("Authorization=Bear***oken"));
    assert!(headers.contains("X-Test=ok"));
    assert!(!headers.contains("raw-header-token"));
}

#[test]
fn hook_sinks_config_uses_separate_table_from_lifecycle_hooks() -> Result<()> {
    let raw = r#"
[hooks]
enabled = true
default_timeout_secs = 20

[[hooks.hooks]]
event = "message_submit"
command = "echo ok"

[hook_sinks]
unix_socket_path = "/tmp/cw-hooks.sock"
"#;

    let config: ConfigToml = toml::from_str(raw)?;

    assert_eq!(
        config.get_value("hook_sinks.unix_socket_path").as_deref(),
        Some("/tmp/cw-hooks.sock")
    );
    assert!(
        config.extras.contains_key("hooks"),
        "legacy lifecycle hooks table must remain an opaque extra"
    );

    let serialized = toml::to_string_pretty(&config)?;
    let round_tripped: ConfigToml = toml::from_str(&serialized)?;
    let hooks = round_tripped
        .extras
        .get("hooks")
        .and_then(toml::Value::as_table)
        .expect("hooks table preserved");

    assert_eq!(
        hooks.get("enabled").and_then(toml::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        hooks
            .get("default_timeout_secs")
            .and_then(toml::Value::as_integer),
        Some(20)
    );
    assert!(
        hooks.get("hooks").and_then(toml::Value::as_array).is_some(),
        "nested lifecycle hooks array must survive config rewrites"
    );
    assert_eq!(
        round_tripped
            .get_value("hook_sinks.unix_socket_path")
            .as_deref(),
        Some("/tmp/cw-hooks.sock")
    );

    Ok(())
}

#[test]
fn hook_sinks_unix_socket_path_round_trips_through_key_value_api() -> Result<()> {
    let mut config = ConfigToml::default();

    config.set_value("hook_sinks.unix_socket_path", "/tmp/cw-events.sock")?;

    assert_eq!(
        config.get_value("hook_sinks.unix_socket_path").as_deref(),
        Some("/tmp/cw-events.sock")
    );
    assert_eq!(
        config
            .list_values()
            .get("hook_sinks.unix_socket_path")
            .map(String::as_str),
        Some("/tmp/cw-events.sock")
    );

    config.unset_value("hook_sinks.unix_socket_path")?;
    assert_eq!(config.get_value("hook_sinks.unix_socket_path"), None);

    Ok(())
}

/// End-to-end smoke for the preferred Kimi Code setup path:
///   1. Start from a fresh root config that uses DeepSeek defaults.
///   2. Mutate it through the same key-value setters the
///      `codewhale config set providers.moonshot.*` CLI invokes.
///   3. Switch the active provider through `CODEWHALE_PROVIDER` —
///      the public env alias — without ever touching the legacy
///      `DEEPSEEK_PROVIDER` name.
///   4. Resolve the runtime and confirm the doctor/runtime values.
///
/// No real API key is required; the `api_key` here is just a
/// non-empty placeholder.
#[test]
fn moonshot_kimi_code_smoke_config_set_then_resolve() -> Result<()> {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    let mut config = ConfigToml {
        provider: ProviderKind::Deepseek,
        default_text_model: Some("deepseek-v4-pro".to_string()),
        ..ConfigToml::default()
    };

    // Same key paths a user would run via `codewhale config set`.
    config.set_value("providers.moonshot.api_key", "kimi-code-key-placeholder")?;
    config.set_value("providers.moonshot.auth_mode", "api_key")?;
    config.set_value("providers.moonshot.base_url", DEFAULT_KIMI_CODE_BASE_URL)?;
    config.set_value("providers.moonshot.model", DEFAULT_KIMI_CODE_MODEL)?;

    // Public env alias for the active-provider switch.
    // Safety: test-only env mutation guarded by env_lock().
    unsafe { env::set_var("CODEWHALE_PROVIDER", "moonshot") };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Moonshot);
    assert_eq!(resolved.base_url, DEFAULT_KIMI_CODE_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_KIMI_CODE_MODEL);
    assert_eq!(resolved.auth_mode.as_deref(), Some("api_key"));
    assert_eq!(
        resolved.api_key.as_deref(),
        Some("kimi-code-key-placeholder")
    );
    assert_eq!(
        resolved.api_key_source,
        Some(RuntimeApiKeySource::ConfigFile)
    );
    Ok(())
}

#[test]
fn moonshot_provider_config_values_round_trip() -> Result<()> {
    let mut config = ConfigToml::default();

    config.set_value("providers.moonshot.api_key", "moonshot-secret-value")?;
    config.set_value("providers.moonshot.base_url", DEFAULT_KIMI_CODE_BASE_URL)?;
    config.set_value("providers.moonshot.model", DEFAULT_KIMI_CODE_MODEL)?;
    config.set_value("providers.moonshot.auth_mode", "api_key")?;
    config.set_value("providers.moonshot.http_headers", "X-Test=ok")?;

    assert_eq!(
        config
            .get_display_value("providers.moonshot.api_key")
            .as_deref(),
        Some("moon***alue")
    );
    assert_eq!(
        config.get_value("providers.moonshot.base_url").as_deref(),
        Some(DEFAULT_KIMI_CODE_BASE_URL)
    );
    assert_eq!(
        config.get_value("providers.moonshot.model").as_deref(),
        Some(DEFAULT_KIMI_CODE_MODEL)
    );
    assert_eq!(
        config.get_value("providers.moonshot.auth_mode").as_deref(),
        Some("api_key")
    );
    assert_eq!(
        config
            .list_values()
            .get("providers.moonshot.api_key")
            .map(String::as_str),
        Some("moon***alue")
    );

    config.unset_value("providers.moonshot.auth_mode")?;
    config.unset_value("providers.moonshot.base_url")?;
    config.unset_value("providers.moonshot.model")?;

    assert_eq!(config.get_value("providers.moonshot.auth_mode"), None);
    assert_eq!(config.get_value("providers.moonshot.base_url"), None);
    assert_eq!(config.get_value("providers.moonshot.model"), None);
    Ok(())
}

#[test]
fn custom_provider_config_set_get_unset_round_trip() -> Result<()> {
    let mut config = ConfigToml::default();

    // The owner flow from #5174: build an Alibaba Model Studio custom
    // provider entirely through `config set providers.<id>.<field>`.
    config.set_value("providers.alibaba_studio.kind", "openai-compatible")?;
    config.set_value(
        "providers.alibaba_studio.base_url",
        "https://coding.dashscope.aliyuncs.com/v1",
    )?;
    config.set_value("providers.alibaba_studio.model", "qwen3-coder-plus")?;
    config.set_value("providers.alibaba_studio.api_key", "sk-studio-secret")?;
    config.set_value("providers.alibaba_studio.context_window", "256000")?;

    assert_eq!(
        config.get_value("providers.alibaba_studio.kind").as_deref(),
        Some("openai-compatible")
    );
    assert_eq!(
        config
            .get_value("providers.alibaba_studio.base_url")
            .as_deref(),
        Some("https://coding.dashscope.aliyuncs.com/v1")
    );
    assert_eq!(
        config
            .get_value("providers.alibaba_studio.model")
            .as_deref(),
        Some("qwen3-coder-plus")
    );
    assert_eq!(
        config
            .get_value("providers.alibaba_studio.context_window")
            .as_deref(),
        Some("256000")
    );
    assert_eq!(
        config
            .get_display_value("providers.alibaba_studio.api_key")
            .as_deref(),
        Some("********")
    );

    // The set must land in a real `[providers.alibaba_studio]` table, never
    // in a literal top-level extras key (#5167).
    let serialized = toml::to_string(&config)?;
    assert!(
        serialized.contains("[providers.alibaba_studio]"),
        "custom provider legs must serialize as a providers table, got:\n{serialized}"
    );
    assert!(
        config
            .extras
            .keys()
            .all(|key| !key.starts_with("providers.")),
        "no literal 'providers.*' extras key may round-trip: {:?}",
        config.extras.keys().collect::<Vec<_>>()
    );

    // The table must satisfy the runtime binding contract for named custom
    // providers.
    config.set_value("provider", "alibaba_studio")?;
    assert_eq!(config.provider_id(), "alibaba_studio");

    config.unset_value("providers.alibaba_studio.api_key")?;
    assert_eq!(config.get_value("providers.alibaba_studio.api_key"), None);
    assert_eq!(
        config
            .get_value("providers.alibaba_studio.model")
            .as_deref(),
        Some("qwen3-coder-plus")
    );
    Ok(())
}

#[test]
fn custom_provider_set_rejects_unknown_field_with_corrective_error() {
    let mut config = ConfigToml::default();

    let err = config
        .set_value("providers.alibaba_studio.bogus", "x")
        .expect_err("unknown custom provider fields must not fall into extras");
    let message = format!("{err:#}");
    assert!(
        message.contains("bogus") && message.contains("base_url"),
        "error must name the bad leg and the valid shape, got: {message}"
    );
    assert!(config.providers.extras.is_empty());
}

#[test]
fn model_context_windows_set_get_unset_round_trip() -> Result<()> {
    let mut config = ConfigToml::default();

    // Built-in provider, bare wire id — plus slash and dotted spellings,
    // which stay intact because the model leg is the whole remainder.
    config.set_value("providers.moonshot.model_context_windows.k3", "262144")?;
    config.set_value(
        "providers.moonshot.model_context_windows.MiniMaxAI/MiniMax-M2.5",
        "204800",
    )?;
    config.set_value(
        "providers.openrouter.model_context_windows.qwen3.5-flash",
        "131072",
    )?;

    assert_eq!(
        config
            .get_value("providers.moonshot.model_context_windows.k3")
            .as_deref(),
        Some("262144")
    );
    assert_eq!(
        config
            .get_value("providers.moonshot.model_context_windows.MiniMaxAI/MiniMax-M2.5")
            .as_deref(),
        Some("204800")
    );
    assert_eq!(
        config
            .get_value("providers.openrouter.model_context_windows.qwen3.5-flash")
            .as_deref(),
        Some("131072")
    );

    // Per-provider isolation: another provider's table never answers.
    assert_eq!(
        config.get_value("providers.openai.model_context_windows.k3"),
        None
    );

    // The typed field round-trips through TOML as a real subtable.
    let serialized = toml::to_string(&config)?;
    assert!(
        serialized.contains("model_context_windows"),
        "per-model windows must serialize as a providers subtable, got:\n{serialized}"
    );
    let reparsed: ConfigToml = toml::from_str(&serialized)?;
    assert_eq!(
        reparsed
            .get_value("providers.moonshot.model_context_windows.k3")
            .as_deref(),
        Some("262144")
    );

    config.unset_value("providers.moonshot.model_context_windows.k3")?;
    assert_eq!(
        config.get_value("providers.moonshot.model_context_windows.k3"),
        None
    );
    assert_eq!(
        config
            .get_value("providers.moonshot.model_context_windows.MiniMaxAI/MiniMax-M2.5")
            .as_deref(),
        Some("204800")
    );
    Ok(())
}

#[test]
fn model_context_windows_custom_gateway_round_trip() -> Result<()> {
    let mut config = ConfigToml::default();

    config.set_value("providers.command_code.kind", "openai-compatible")?;
    config.set_value(
        "providers.command_code.base_url",
        "https://gateway.example/v1",
    )?;
    config.set_value(
        "providers.command_code.model_context_windows.google/gemini-3.1-flash-lite",
        "1000000",
    )?;

    assert_eq!(
        config
            .get_value("providers.command_code.model_context_windows.google/gemini-3.1-flash-lite")
            .as_deref(),
        Some("1000000")
    );

    let serialized = toml::to_string(&config)?;
    assert!(
        serialized.contains("[providers.command_code.model_context_windows]")
            || serialized.contains("model_context_windows"),
        "custom provider windows must serialize under the providers table, got:\n{serialized}"
    );

    config
        .unset_value("providers.command_code.model_context_windows.google/gemini-3.1-flash-lite")?;
    assert_eq!(
        config
            .get_value("providers.command_code.model_context_windows.google/gemini-3.1-flash-lite"),
        None
    );
    Ok(())
}

#[test]
fn model_context_windows_rejects_zero_and_bad_model_ids() {
    let mut config = ConfigToml::default();

    let err = config
        .set_value("providers.moonshot.model_context_windows.k3", "0")
        .expect_err("zero per-model window must be rejected");
    assert!(
        format!("{err:#}").contains("greater than 0"),
        "unexpected error: {err:#}"
    );

    let err = config
        .set_value("providers.moonshot.model_context_windows.auto", "204800")
        .expect_err("`auto` is a selector, not a wire model id");
    assert!(
        format!("{err:#}").contains("model_context_windows"),
        "unexpected error: {err:#}"
    );

    let err = config
        .set_value(
            "providers.moonshot.model_context_windows.bad model",
            "204800",
        )
        .expect_err("whitespace model ids must be rejected");
    assert!(
        format!("{err:#}").contains("model_context_windows"),
        "unexpected error: {err:#}"
    );

    // Rejected writes leave no residue on the typed field.
    assert!(
        config
            .providers
            .for_provider(ProviderKind::Moonshot)
            .model_context_windows
            .is_empty()
    );
}

#[test]
fn builtin_provider_set_rejects_unknown_field_with_corrective_error() {
    let mut config = ConfigToml::default();

    let err = config
        .set_value("providers.deepseek.bogus", "x")
        .expect_err("unknown built-in provider fields must not fall into extras");
    let message = format!("{err:#}");
    assert!(
        message.contains("bogus") && message.contains("deepseek"),
        "error must name the bad leg and provider, got: {message}"
    );
    assert!(config.providers.extras.is_empty());
}

#[test]
fn siliconflow_cn_provider_config_values_round_trip() -> Result<()> {
    let mut config = ConfigToml::default();

    config.set_value("providers.siliconflow_cn.api_key", "sf-cn-secret-value")?;
    config.set_value(
        "providers.siliconflow_cn.base_url",
        DEFAULT_SILICONFLOW_CN_BASE_URL,
    )?;
    config.set_value("providers.siliconflow_cn.model", DEFAULT_SILICONFLOW_MODEL)?;
    config.set_value("providers.siliconflow_cn.http_headers", "X-Test=ok")?;

    assert_eq!(
        config
            .get_display_value("providers.siliconflow_cn.api_key")
            .as_deref(),
        Some("sf-c***alue")
    );
    assert_eq!(
        config
            .get_value("providers.siliconflow_cn.base_url")
            .as_deref(),
        Some(DEFAULT_SILICONFLOW_CN_BASE_URL)
    );
    assert_eq!(
        config
            .get_value("providers.siliconflow_cn.model")
            .as_deref(),
        Some(DEFAULT_SILICONFLOW_MODEL)
    );
    assert_eq!(
        config
            .list_values()
            .get("providers.siliconflow_cn.api_key")
            .map(String::as_str),
        Some("sf-c***alue")
    );

    config.unset_value("providers.siliconflow_cn.api_key")?;
    config.unset_value("providers.siliconflow_cn.base_url")?;
    config.unset_value("providers.siliconflow_cn.model")?;
    config.unset_value("providers.siliconflow_cn.http_headers")?;

    assert_eq!(config.get_value("providers.siliconflow_cn.api_key"), None);
    assert_eq!(config.get_value("providers.siliconflow_cn.base_url"), None);
    assert_eq!(config.get_value("providers.siliconflow_cn.model"), None);
    assert_eq!(
        config.get_value("providers.siliconflow_cn.http_headers"),
        None
    );
    Ok(())
}

#[test]
fn volcengine_provider_config_values_round_trip() -> Result<()> {
    let mut config = ConfigToml::default();

    config.set_value("providers.volcengine.api_key", "volcengine-secret-value")?;
    config.set_value("providers.volcengine.base_url", DEFAULT_VOLCENGINE_BASE_URL)?;
    config.set_value("providers.volcengine.model", DEFAULT_VOLCENGINE_MODEL)?;
    config.set_value("providers.volcengine.http_headers", "X-Test=ok")?;

    assert_eq!(
        config
            .get_display_value("providers.volcengine.api_key")
            .as_deref(),
        Some("volc***alue")
    );
    assert_eq!(
        config.get_value("providers.volcengine.base_url").as_deref(),
        Some(DEFAULT_VOLCENGINE_BASE_URL)
    );
    assert_eq!(
        config.get_value("providers.volcengine.model").as_deref(),
        Some(DEFAULT_VOLCENGINE_MODEL)
    );
    assert_eq!(
        config
            .get_value("providers.volcengine.http_headers")
            .as_deref(),
        Some("X-Test=ok")
    );
    assert_eq!(
        config
            .list_values()
            .get("providers.volcengine.http_headers")
            .map(String::as_str),
        Some("X-Test=ok")
    );

    config.unset_value("providers.volcengine.http_headers")?;
    assert_eq!(config.get_value("providers.volcengine.http_headers"), None);
    Ok(())
}

#[test]
fn provider_key_value_api_covers_all_provider_metadata_entries() -> Result<()> {
    for provider in ProviderKind::ALL {
        let table = provider.provider().provider_config_key();
        let mut config = ConfigToml::default();
        let api_key = format!("secret-value-for-{table}-123456");
        let api_key_path = format!("providers.{table}.api_key");
        let base_url_path = format!("providers.{table}.base_url");
        let model_path = format!("providers.{table}.model");
        let context_window_path = format!("providers.{table}.context_window");
        let headers_path = format!("providers.{table}.http_headers");
        let mode_path = format!("providers.{table}.mode");
        let auth_mode_path = format!("providers.{table}.auth_mode");
        let insecure_path = format!("providers.{table}.insecure_skip_tls_verify");
        let path_suffix_path = format!("providers.{table}.path_suffix");

        config.set_value(&api_key_path, &api_key)?;
        config.set_value(&base_url_path, "https://gateway.example/v1")?;
        config.set_value(&model_path, "provider-test-model")?;
        config.set_value(&context_window_path, "1000000")?;
        config.set_value(&headers_path, "X-Test=ok")?;
        config.set_value(&mode_path, "concise")?;
        config.set_value(&auth_mode_path, "api_key")?;
        config.set_value(&insecure_path, "true")?;
        config.set_value(&path_suffix_path, "/chat/completions")?;

        assert_eq!(
            config.get_value(&api_key_path).as_deref(),
            Some(api_key.as_str())
        );
        assert_eq!(
            config.get_value(&base_url_path).as_deref(),
            Some("https://gateway.example/v1")
        );
        assert_eq!(
            config.get_value(&model_path).as_deref(),
            Some("provider-test-model")
        );
        assert_eq!(
            config.get_value(&context_window_path).as_deref(),
            Some("1000000")
        );
        assert_eq!(
            config.get_value(&headers_path).as_deref(),
            Some("X-Test=ok")
        );
        assert_eq!(config.get_value(&mode_path).as_deref(), Some("concise"));
        assert_eq!(
            config.get_value(&auth_mode_path).as_deref(),
            Some("api_key")
        );
        assert_eq!(config.get_value(&insecure_path).as_deref(), Some("true"));
        assert_eq!(
            config.get_value(&path_suffix_path).as_deref(),
            Some("/chat/completions")
        );

        let listed = config.list_values();
        let listed_api_key = listed
            .get(&api_key_path)
            .expect("provider API key is listed");
        assert!(listed_api_key.contains("***"));
        assert_ne!(listed_api_key, &api_key);
        assert_eq!(
            listed.get(&headers_path).map(String::as_str),
            Some("X-Test=ok")
        );
        assert_eq!(
            listed.get(&context_window_path).map(String::as_str),
            Some("1000000")
        );
        assert_eq!(listed.get(&insecure_path).map(String::as_str), Some("true"));

        config.unset_value(&api_key_path)?;
        config.unset_value(&base_url_path)?;
        config.unset_value(&model_path)?;
        config.unset_value(&context_window_path)?;
        config.unset_value(&headers_path)?;
        config.unset_value(&mode_path)?;
        config.unset_value(&auth_mode_path)?;
        config.unset_value(&insecure_path)?;
        config.unset_value(&path_suffix_path)?;

        assert_eq!(config.get_value(&api_key_path), None);
        assert_eq!(config.get_value(&base_url_path), None);
        assert_eq!(config.get_value(&model_path), None);
        assert_eq!(config.get_value(&context_window_path), None);
        assert_eq!(config.get_value(&headers_path), None);
        assert_eq!(config.get_value(&mode_path), None);
        assert_eq!(config.get_value(&auth_mode_path), None);
        assert_eq!(config.get_value(&insecure_path), None);
        assert_eq!(config.get_value(&path_suffix_path), None);

        if provider == ProviderKind::Deepseek {
            assert_eq!(config.default_text_model, None);
            assert!(config.http_headers.is_empty());
        }
    }

    Ok(())
}

#[test]
fn provider_context_window_rejects_zero() {
    let mut config = ConfigToml::default();
    let err = config
        .set_value("providers.openai.context_window", "0")
        .expect_err("zero context window should be rejected");

    assert!(err.to_string().contains("greater than 0"));
}

#[test]
fn list_values_redacts_unicode_api_key_without_byte_slicing() {
    let mut config = ConfigToml::default();
    config.providers.deepseek.api_key = Some("密钥密钥密钥密钥123456789".to_string());

    let values = config.list_values();

    assert_eq!(
        values.get("providers.deepseek.api_key").map(String::as_str),
        Some("密钥密钥***6789")
    );
}

#[test]
fn app_homes_prefer_home_env_before_platform_home_fallback() {
    let _lock = env_lock();
    let home =
        std::env::temp_dir().join(format!("codewhale-config-home-env-{}", std::process::id()));
    let userprofile = std::env::temp_dir().join(format!(
        "codewhale-config-userprofile-{}",
        std::process::id()
    ));
    let _env = StateEnvRestore {
        home: env::var_os("HOME"),
        userprofile: env::var_os("USERPROFILE"),
        codewhale_home: env::var_os("CODEWHALE_HOME"),
    };
    // Safety: test-only environment mutation is serialized by env_lock().
    unsafe {
        env::set_var("HOME", &home);
        env::set_var("USERPROFILE", &userprofile);
        env::remove_var("CODEWHALE_HOME");
    }

    assert_eq!(
        codewhale_home().expect("codewhale home"),
        home.join(CODEWHALE_APP_DIR)
    );
    assert_eq!(
        legacy_deepseek_home().expect("legacy home"),
        home.join(LEGACY_APP_DIR)
    );

    let explicit = std::env::temp_dir().join(format!(
        "codewhale-config-explicit-home-{}",
        std::process::id()
    ));
    // Safety: test-only environment mutation is serialized by env_lock().
    unsafe {
        env::set_var("CODEWHALE_HOME", &explicit);
    }
    assert_eq!(codewhale_home().expect("explicit home"), explicit);
}

#[test]
fn relative_codewhale_home_is_a_hard_error() {
    let _lock = env_lock();
    let _env = StateEnvRestore {
        home: env::var_os("HOME"),
        userprofile: env::var_os("USERPROFILE"),
        codewhale_home: env::var_os("CODEWHALE_HOME"),
    };
    // Safety: test-only environment mutation is serialized by env_lock().
    unsafe {
        env::set_var("CODEWHALE_HOME", ".codewhale");
    }

    let error = codewhale_home().expect_err("relative global home must fail closed");
    let message = format!("{error:#}");
    assert!(message.contains("CODEWHALE_HOME"), "{message}");
    assert!(message.contains("absolute"), "{message}");
}

/// Audit R04-05: a relative `HOME` must not relocate global state into the
/// working directory.
#[test]
fn relative_user_home_is_a_hard_error() {
    let _lock = env_lock();
    let _env = StateEnvRestore {
        home: env::var_os("HOME"),
        userprofile: env::var_os("USERPROFILE"),
        codewhale_home: env::var_os("CODEWHALE_HOME"),
    };
    // Safety: test-only environment mutation is serialized by env_lock().
    unsafe {
        env::set_var("HOME", "relative-home");
        env::remove_var("USERPROFILE");
        env::remove_var("CODEWHALE_HOME");
    }

    assert_eq!(codewhale_paths::user_home(), None);
    let error = codewhale_home().expect_err("relative HOME must fail closed");
    let message = format!("{error:#}");
    assert!(message.contains("HOME"), "{message}");
    assert!(message.contains("absolute"), "{message}");
}

#[test]
fn migrate_config_reports_copied_legacy_path() {
    let _lock = env_lock();
    struct LegacyConfigGuard {
        path: PathBuf,
        original: Option<Vec<u8>>,
    }

    impl LegacyConfigGuard {
        fn install(path: PathBuf, contents: &[u8]) -> Self {
            let original = fs::read(&path).ok();
            fs::create_dir_all(path.parent().expect("legacy config parent")).expect("legacy dir");
            fs::write(&path, contents).expect("legacy config");
            Self { path, original }
        }
    }

    impl Drop for LegacyConfigGuard {
        fn drop(&mut self) {
            if let Some(original) = self.original.take() {
                let _ = fs::write(&self.path, original);
            } else {
                let _ = fs::remove_file(&self.path);
                if let Some(parent) = self.path.parent() {
                    let _ = fs::remove_dir(parent);
                }
            }
        }
    }

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let home = std::env::temp_dir().join(format!(
        "codewhale-config-migration-{}-{unique}",
        std::process::id()
    ));
    let legacy_dir = home.join(LEGACY_APP_DIR);
    let primary_dir = home.join(CODEWHALE_APP_DIR);
    let legacy_config = legacy_dir.join(CONFIG_FILE_NAME);
    let _legacy = LegacyConfigGuard::install(legacy_config.clone(), b"provider = \"deepseek\"\n");

    let _env = StateEnvRestore {
        home: env::var_os("HOME"),
        userprofile: env::var_os("USERPROFILE"),
        codewhale_home: env::var_os("CODEWHALE_HOME"),
    };
    // Safety: test-only environment mutation is serialized by env_lock().
    unsafe {
        env::set_var("HOME", &home);
        env::set_var("USERPROFILE", &home);
        env::remove_var("CODEWHALE_HOME");
    }

    let migration = migrate_config_if_needed()
        .expect("migration")
        .expect("legacy config should be copied");

    assert_eq!(migration.legacy_path, legacy_config);
    assert_eq!(migration.primary_path, primary_dir.join(CONFIG_FILE_NAME));
    let notice = migration.user_notice();
    assert!(notice.contains(&legacy_dir.join(CONFIG_FILE_NAME).display().to_string()));
    assert!(notice.contains(&primary_dir.join(CONFIG_FILE_NAME).display().to_string()));
    assert!(notice.contains(".codewhale path for future edits"));
    assert!(notice.contains(".deepseek file remains only as a compatibility fallback"));
    assert_eq!(
        fs::read_to_string(primary_dir.join(CONFIG_FILE_NAME)).expect("primary config"),
        "provider = \"deepseek\"\n"
    );

    let _ = fs::remove_dir_all(home);
}

#[cfg(unix)]
#[test]
fn migrate_config_refuses_a_dangling_link_at_the_primary_path() {
    let _lock = env_lock();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let home = std::env::temp_dir().join(format!(
        "codewhale-config-migration-link-{}-{unique}",
        std::process::id()
    ));
    let legacy_config = home.join(LEGACY_APP_DIR).join(CONFIG_FILE_NAME);
    fs::create_dir_all(legacy_config.parent().expect("legacy parent")).expect("legacy dir");
    fs::write(&legacy_config, b"provider = \"deepseek\"\n").expect("legacy config");
    let primary = home.join(CODEWHALE_APP_DIR).join(CONFIG_FILE_NAME);
    fs::create_dir_all(primary.parent().expect("primary parent")).expect("primary dir");
    let target = home.join("elsewhere").join("created-through-the-link");
    fs::create_dir_all(target.parent().expect("target parent")).expect("target dir");
    std::os::unix::fs::symlink(&target, &primary).expect("plant dangling link");

    let _env = StateEnvRestore {
        home: env::var_os("HOME"),
        userprofile: env::var_os("USERPROFILE"),
        codewhale_home: env::var_os("CODEWHALE_HOME"),
    };
    // Safety: test-only environment mutation is serialized by env_lock().
    unsafe {
        env::set_var("HOME", &home);
        env::set_var("USERPROFILE", &home);
        env::remove_var("CODEWHALE_HOME");
    }

    let error = migrate_config_if_needed().expect_err("a linked primary path is refused");
    assert!(format!("{error:#}").contains("symlink"), "{error:#}");
    assert!(!target.exists(), "the link target must not be created");
    assert!(
        fs::symlink_metadata(&primary)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the planted link is left as it was"
    );

    let _ = fs::remove_dir_all(home);
}

#[test]
fn explicit_codewhale_home_bypasses_legacy_config_fallback_and_migration() {
    let _lock = env_lock();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let home = std::env::temp_dir().join(format!(
        "codewhale-config-explicit-isolation-{}-{unique}",
        std::process::id()
    ));
    let legacy_config = home.join(LEGACY_APP_DIR).join(CONFIG_FILE_NAME);
    fs::create_dir_all(legacy_config.parent().expect("legacy config parent")).expect("legacy dir");
    fs::write(&legacy_config, b"provider = \"deepseek\"\n").expect("legacy config");

    let explicit_home = home.join("isolated-codewhale");
    let _env = StateEnvRestore {
        home: env::var_os("HOME"),
        userprofile: env::var_os("USERPROFILE"),
        codewhale_home: env::var_os("CODEWHALE_HOME"),
    };
    // Safety: test-only environment mutation is serialized by env_lock().
    unsafe {
        env::set_var("HOME", &home);
        env::set_var("USERPROFILE", &home);
        env::set_var("CODEWHALE_HOME", &explicit_home);
    }

    assert_eq!(
        default_config_path().expect("default config path"),
        explicit_home.join(CONFIG_FILE_NAME),
        "explicit CODEWHALE_HOME must not read ambient legacy config"
    );
    assert!(
        migrate_config_if_needed()
            .expect("migration check")
            .is_none(),
        "explicit CODEWHALE_HOME must not migrate ambient legacy config"
    );
    assert!(
        !explicit_home.join(CONFIG_FILE_NAME).exists(),
        "legacy config must not be copied into explicit CODEWHALE_HOME"
    );

    let _ = fs::remove_dir_all(home);
}

// ── ensure_state_dir legacy migration (#3240) ───────────────────────

/// Saves and restores the env vars that the state-resolvers read.
struct StateEnvRestore {
    home: Option<OsString>,
    userprofile: Option<OsString>,
    codewhale_home: Option<OsString>,
}

impl Drop for StateEnvRestore {
    fn drop(&mut self) {
        // Safety: test-only environment mutation is serialized by env_lock().
        unsafe {
            match self.home.take() {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }
            match self.userprofile.take() {
                Some(value) => env::set_var("USERPROFILE", value),
                None => env::remove_var("USERPROFILE"),
            }
            match self.codewhale_home.take() {
                Some(value) => env::set_var("CODEWHALE_HOME", value),
                None => env::remove_var("CODEWHALE_HOME"),
            }
        }
    }
}

/// Points `HOME`/`USERPROFILE` at a fresh temp tree and clears
/// `CODEWHALE_HOME` so `codewhale_home()` -> `<home>/.codewhale` and
/// `legacy_deepseek_home()` -> `<home>/.deepseek`. Env is restored on drop.
struct StateDirEnv {
    home: PathBuf,
    _restore: StateEnvRestore,
}

impl StateDirEnv {
    fn install(unique: u128) -> Self {
        let home = std::env::temp_dir().join(format!(
            "codewhale-state-migration-{}-{unique}",
            std::process::id()
        ));
        let restore = StateEnvRestore {
            home: env::var_os("HOME"),
            userprofile: env::var_os("USERPROFILE"),
            codewhale_home: env::var_os("CODEWHALE_HOME"),
        };
        // Safety: test-only environment mutation is serialized by env_lock().
        unsafe {
            env::set_var("HOME", &home);
            env::set_var("USERPROFILE", &home);
            env::remove_var("CODEWHALE_HOME");
        }
        Self {
            home,
            _restore: restore,
        }
    }
    fn legacy(&self, sub: &str) -> PathBuf {
        self.home.join(LEGACY_APP_DIR).join(sub)
    }
    fn primary(&self, sub: &str) -> PathBuf {
        self.home.join(CODEWHALE_APP_DIR).join(sub)
    }
}

#[test]
fn ensure_state_dir_relocates_legacy_subdir_on_first_write() {
    let _lock = env_lock();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let state_env = StateDirEnv::install(unique);
    // Seed a legacy subdir; primary must not exist yet.
    fs::create_dir_all(state_env.legacy("slop_ledger")).expect("legacy dir");
    fs::write(
        state_env.legacy("slop_ledger").join("slop_ledger.json"),
        b"legacy",
    )
    .expect("legacy file");
    assert!(!state_env.primary("slop_ledger").exists());

    let (dir, migration) =
        ensure_state_dir_with_migration("slop_ledger").expect("ensure_state_dir");
    assert_eq!(dir, state_env.primary("slop_ledger"));
    let migration = migration.expect("legacy migration should be reported");
    assert_eq!(migration.kind, StateMigrationKind::Relocated);
    assert_eq!(migration.subdir, "slop_ledger");
    assert_eq!(migration.legacy_path, state_env.legacy("slop_ledger"));
    assert_eq!(migration.primary_path, state_env.primary("slop_ledger"));
    // Legacy contents relocated into primary.
    assert_eq!(
        fs::read_to_string(state_env.primary("slop_ledger").join("slop_ledger.json"))
            .expect("migrated file"),
        "legacy"
    );
    // The legacy subdir was relocated (moved), so .deepseek stops growing.
    assert!(
        !state_env.legacy("slop_ledger").exists(),
        "legacy subdir should be removed after relocation"
    );
    // Idempotent: a second call is a no-op now that primary exists.
    let (_, repeated_migration) =
        ensure_state_dir_with_migration("slop_ledger").expect("idempotent ensure");
    assert!(repeated_migration.is_none());
    let _ = fs::remove_dir_all(&state_env.home);
}

#[test]
fn state_migration_notice_explains_preserved_data_and_canonical_root() {
    let migration = StateMigration {
        subdir: "sessions".to_string(),
        legacy_path: PathBuf::from("/home/alice/.deepseek/sessions"),
        primary_path: PathBuf::from("/home/alice/.codewhale/sessions"),
        kind: StateMigrationKind::Relocated,
    };

    let notice = migration.user_notice();

    assert!(notice.contains("Codewhale migrated legacy state"));
    assert!(notice.contains("/home/alice/.deepseek/sessions"));
    assert!(notice.contains("/home/alice/.codewhale/sessions"));
    assert!(notice.contains("Your data was preserved"));
    assert!(notice.contains("Use .codewhale as the canonical state location"));
    assert!(notice.contains("remove the legacy .deepseek tree"));
}

#[test]
fn copied_state_migration_notice_says_legacy_copy_remains() {
    let migration = StateMigration {
        subdir: "catalog".to_string(),
        legacy_path: PathBuf::from("/home/alice/.deepseek/catalog"),
        primary_path: PathBuf::from("/home/alice/.codewhale/catalog"),
        kind: StateMigrationKind::Copied,
    };

    let notice = migration.user_notice();

    assert!(notice.contains("copied"));
    assert!(notice.contains("legacy .deepseek copy was left in place"));
}

#[test]
fn ensure_state_dir_writes_to_primary_when_both_exist() {
    let _lock = env_lock();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let state_env = StateDirEnv::install(unique);
    // Migrated user: primary already exists; a legacy orphan also remains.
    fs::create_dir_all(state_env.primary("sessions")).expect("primary dir");
    fs::write(state_env.primary("sessions").join("a.json"), b"primary").expect("primary file");
    fs::create_dir_all(state_env.legacy("sessions")).expect("legacy dir");
    fs::write(state_env.legacy("sessions").join("old.json"), b"legacy").expect("legacy file");

    let (dir, migration) = ensure_state_dir_with_migration("sessions").expect("ensure_state_dir");
    assert_eq!(dir, state_env.primary("sessions"));
    assert!(
        migration.is_none(),
        "existing primary must not emit a migration event"
    );
    // Primary untouched; legacy orphan left as-is (not migrated, not deleted).
    assert_eq!(
        fs::read_to_string(state_env.primary("sessions").join("a.json")).expect("primary"),
        "primary"
    );
    assert!(
        state_env.legacy("sessions").exists(),
        "existing legacy orphan must not be deleted when primary exists"
    );
    let _ = fs::remove_dir_all(&state_env.home);
}

#[test]
fn resolve_state_dir_still_finds_legacy_for_backfill() {
    let _lock = env_lock();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let state_env = StateDirEnv::install(unique);
    // Only legacy exists -> read resolver returns legacy (backfill).
    fs::create_dir_all(state_env.legacy("catalog")).expect("legacy dir");
    assert_eq!(
        resolve_state_dir("catalog").expect("resolve"),
        state_env.legacy("catalog")
    );
    // After the primary is created (e.g. via a write), the read resolver
    // returns primary — legacy is reachable only while primary is absent.
    ensure_state_dir("catalog").expect("ensure");
    assert_eq!(
        resolve_state_dir("catalog").expect("resolve after migrate"),
        state_env.primary("catalog")
    );
    let _ = fs::remove_dir_all(&state_env.home);
}

#[test]
fn explicit_codewhale_home_bypasses_legacy_state_fallback_and_migration() {
    let _lock = env_lock();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let state_env = StateDirEnv::install(unique);
    let explicit_home = state_env.home.join("isolated-codewhale");
    // Safety: test-only environment mutation is serialized by env_lock().
    unsafe {
        env::set_var("CODEWHALE_HOME", &explicit_home);
    }
    fs::create_dir_all(state_env.legacy("catalog")).expect("legacy dir");
    fs::write(state_env.legacy("catalog").join("legacy.json"), b"legacy").expect("legacy file");

    let primary = explicit_home.join("catalog");
    assert_eq!(
        resolve_state_dir("catalog").expect("resolve"),
        primary,
        "explicit CODEWHALE_HOME must not read ambient legacy state"
    );

    let ensured = ensure_state_dir("catalog").expect("ensure");
    assert_eq!(ensured, primary);
    assert!(
        state_env.legacy("catalog").join("legacy.json").exists(),
        "explicit CODEWHALE_HOME must not migrate ambient legacy state"
    );
    assert!(
        !primary.join("legacy.json").exists(),
        "legacy contents must not be copied into an explicit CODEWHALE_HOME"
    );
    let _ = fs::remove_dir_all(&state_env.home);
}

#[test]
fn state_resolvers_reject_path_traversal_subdirs() {
    // Defense against path injection (#3240 hardening): the public state
    // resolvers must refuse subdirs that could escape the state root.
    for bad in ["..", "../secret", "/etc", "a/../../b"] {
        let err = ensure_state_dir(bad)
            .err()
            .unwrap_or_else(|| panic!("expected {bad:?} to be rejected"));
        assert!(
            format!("{err:#}").contains("state subdir"),
            "expected rejection of {bad:?}, got {err:#}"
        );
        assert!(
            resolve_state_dir(bad).is_err(),
            "read resolver must also reject {bad:?}"
        );
    }
    // Safe values are accepted (including the root sentinel ".").
    assert!(ensure_safe_state_subdir(".").is_ok());
    assert!(ensure_safe_state_subdir("sessions").is_ok());
    assert!(ensure_safe_state_subdir("a/b").is_ok());
    assert!(ensure_safe_state_subdir("").is_err());
}

#[test]
fn project_state_resolvers_reject_path_traversal_subdirs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");

    for bad in ["..", "../secret", "/etc", "a/../../b"] {
        let err = resolve_project_state_dir(&workspace, bad)
            .err()
            .unwrap_or_else(|| panic!("expected {bad:?} to be rejected"));
        assert!(
            format!("{err:#}").contains("state subdir"),
            "expected rejection of {bad:?}, got {err:#}"
        );
        assert!(
            ensure_project_state_dir(&workspace, bad).is_err(),
            "write resolver must also reject {bad:?}"
        );
    }

    let canonical_workspace = workspace.canonicalize().expect("canonical workspace");
    let safe = resolve_project_state_dir(&workspace, "notes.md")
        .expect("safe project state subdir should resolve")
        .1;
    assert_eq!(
        safe,
        canonical_workspace.join(LEGACY_APP_DIR).join("notes.md")
    );
    let created =
        ensure_project_state_dir(&workspace, "a/b").expect("safe nested project state dir");
    assert_eq!(
        created,
        canonical_workspace.join(CODEWHALE_APP_DIR).join("a/b")
    );
}

#[test]
fn project_state_resolvers_reject_workspace_traversal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let bad_workspace = workspace.join("..").join("outside");

    let err = resolve_project_state_dir(&bad_workspace, "notes.md")
        .expect_err("workspace traversal should fail");
    assert!(format!("{err:#}").contains("project workspace path"));
    assert!(ensure_project_state_dir(&bad_workspace, "state").is_err());
}

#[test]
fn normalize_config_file_path_rejects_traversal() {
    let err = normalize_config_file_path(PathBuf::from("../config.toml"))
        .expect_err("traversal path should fail");
    assert!(format!("{err:#}").contains("cannot contain '..'"));
}

#[test]
fn config_store_save_revalidates_path_before_parent_creation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outside_dir = dir.path().join("outside");
    let traversal_path = dir
        .path()
        .join("allowed")
        .join("..")
        .join("outside")
        .join(CONFIG_FILE_NAME);
    let mut store = ConfigStore {
        path: traversal_path,
        config: ConfigToml::default(),
        permissions: PermissionsToml::default(),
        original_raw: None,
        legacy_root: Default::default(),
    };

    let err = store
        .save()
        .expect_err("save should reject traversal before creating parents");

    assert!(format!("{err:#}").contains("cannot contain '..'"));
    assert!(
        !outside_dir.exists(),
        "save must not create directories from an unvalidated path"
    );
}

#[test]
fn resolve_config_path_rejects_relative_env_path_before_cwd_resolution() {
    let _lock = env_lock();
    struct ConfigPathEnvGuard {
        codewhale: Option<OsString>,
        deepseek: Option<OsString>,
    }
    impl Drop for ConfigPathEnvGuard {
        fn drop(&mut self) {
            // Safety: test-only environment mutation is serialized by env_lock().
            unsafe {
                match self.codewhale.as_ref() {
                    Some(value) => env::set_var("CODEWHALE_CONFIG_PATH", value),
                    None => env::remove_var("CODEWHALE_CONFIG_PATH"),
                }
                match self.deepseek.as_ref() {
                    Some(value) => env::set_var("DEEPSEEK_CONFIG_PATH", value),
                    None => env::remove_var("DEEPSEEK_CONFIG_PATH"),
                }
            }
        }
    }
    let _guard = ConfigPathEnvGuard {
        codewhale: env::var_os("CODEWHALE_CONFIG_PATH"),
        deepseek: env::var_os("DEEPSEEK_CONFIG_PATH"),
    };

    // Safety: test-only environment mutation is serialized by env_lock().
    unsafe {
        env::set_var("CODEWHALE_CONFIG_PATH", "../config.toml");
        env::remove_var("DEEPSEEK_CONFIG_PATH");
    }

    let err = resolve_config_path(None).expect_err("relative env path should fail");
    let message = format!("{err:#}");
    assert!(message.contains("CODEWHALE_CONFIG_PATH"), "{message}");
    assert!(message.contains("absolute"), "{message}");
}

#[cfg(unix)]
#[test]
fn normalize_config_file_path_rejects_symlink_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("target.toml");
    let link = dir.path().join(CONFIG_FILE_NAME);
    fs::write(&target, "model = \"deepseek-v4-flash\"\n").expect("write target");
    std::os::unix::fs::symlink(&target, &link).expect("symlink config");

    let err = normalize_config_file_path(link).expect_err("symlink config should fail");
    assert!(format!("{err:#}").contains("must not be a symlink"));
}

#[cfg(unix)]
#[test]
fn load_project_config_rejects_symlinked_primary_config() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let primary_dir = workspace.path().join(CODEWHALE_APP_DIR);
    let legacy_dir = workspace.path().join(LEGACY_APP_DIR);
    fs::create_dir_all(&primary_dir).expect("mkdir primary");
    fs::create_dir_all(&legacy_dir).expect("mkdir legacy");
    let outside_config = outside.path().join(CONFIG_FILE_NAME);
    fs::write(&outside_config, "model = \"outside-model\"\n").expect("write outside config");
    fs::write(
        legacy_dir.join(CONFIG_FILE_NAME),
        "model = \"legacy-model\"\n",
    )
    .expect("write legacy config");
    std::os::unix::fs::symlink(&outside_config, primary_dir.join(CONFIG_FILE_NAME))
        .expect("symlink project config");

    let loaded = load_project_config(workspace.path());

    assert!(
        loaded.is_none(),
        "symlinked primary project config should stop the project overlay"
    );
}

#[test]
fn load_project_config_keeps_unknown_provider_names_strict() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let config_dir = workspace.path().join(CODEWHALE_APP_DIR);
    fs::create_dir_all(&config_dir).expect("mkdir project config");
    fs::write(
        config_dir.join(CONFIG_FILE_NAME),
        r#"provider = "acme_zen_gateway"
model = "must-not-apply"

[providers.acme_zen_gateway]
kind = "openai-compatible"
base_url = "https://acme.example/v1"
"#,
    )
    .expect("write project config");

    assert!(
        load_project_config(workspace.path()).is_none(),
        "project overlays must not gain named-provider authority"
    );
    // ...and the reason is available, not just the absence.
    let (path, reason) = load_project_config_outcome(workspace.path())
        .invalid()
        .map(|(path, reason)| (path.to_path_buf(), reason.to_string()))
        .expect("unknown provider must report why the config was rejected");
    assert!(path.ends_with(CONFIG_FILE_NAME), "{path:?}");
    assert!(reason.contains("acme_zen_gateway"), "{reason}");
}

#[test]
fn project_config_unknown_provider_reason_never_echoes_a_pasted_key() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let config_dir = workspace.path().join(CODEWHALE_APP_DIR);
    fs::create_dir_all(&config_dir).expect("mkdir project config");
    let key = ["sk-live-", "Z7qX4mNb2Vc9Lk3PwR8t"].concat();
    fs::write(
        config_dir.join(CONFIG_FILE_NAME),
        format!("provider = \"{key}\"\n"),
    )
    .expect("write project config");

    let reason = load_project_config_outcome(workspace.path())
        .invalid()
        .map(|(_, reason)| reason.to_string())
        .expect("unknown provider is rejected");
    assert!(reason.starts_with("unknown provider"), "{reason}");
    assert!(!reason.contains(&key), "{reason}");
}

#[test]
fn malformed_project_config_is_distinguishable_from_a_missing_one() {
    // #4733: the loader returned `None` for both cases. A project config can
    // only *tighten* approval/sandbox posture, so reporting a broken file as
    // "no project config" silently drops those restrictions and falls back to
    // the user's more permissive baseline.
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    assert!(
        matches!(
            load_project_config_outcome(workspace.path()),
            ProjectConfigOutcome::Missing
        ),
        "a workspace with no project config must report Missing"
    );

    let config_dir = workspace.path().join(CODEWHALE_APP_DIR);
    fs::create_dir_all(&config_dir).expect("mkdir project config");
    fs::write(
        config_dir.join(CONFIG_FILE_NAME),
        "approval_policy = \"unless-trusted\"\nthis is not valid toml\n",
    )
    .expect("write malformed project config");

    let outcome = load_project_config_outcome(workspace.path());
    let (path, reason) = outcome
        .invalid()
        .expect("a malformed project config must not read as Missing");
    assert!(path.ends_with(CONFIG_FILE_NAME), "{path:?}");
    assert!(!reason.is_empty(), "the rejection must carry a reason");
    // The reason must not leak the file's contents — config files hold
    // credentials.
    assert!(
        !reason.contains("approval_policy"),
        "reason must not echo file contents: {reason}"
    );

    // The lossy convenience wrapper still behaves as before for callers that
    // only want the config.
    assert!(load_project_config(workspace.path()).is_none());
}

#[test]
fn well_formed_project_config_still_loads() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let config_dir = workspace.path().join(CODEWHALE_APP_DIR);
    fs::create_dir_all(&config_dir).expect("mkdir project config");
    fs::write(
        config_dir.join(CONFIG_FILE_NAME),
        "approval_policy = \"unless-trusted\"\n",
    )
    .expect("write project config");

    let outcome = load_project_config_outcome(workspace.path());
    assert!(outcome.invalid().is_none(), "{outcome:?}");
    assert_eq!(
        outcome
            .into_config()
            .and_then(|config| config.approval_policy)
            .as_deref(),
        Some("unless-trusted"),
    );
}

#[cfg(unix)]
#[test]
fn load_sibling_permissions_rejects_symlink_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let outside = dir.path().join("outside-permissions.toml");
    let permissions_link = dir.path().join(PERMISSIONS_FILE_NAME);
    fs::write(&config_path, "model = \"deepseek-v4-flash\"\n").expect("write config");
    fs::write(&outside, "").expect("write outside permissions");
    std::os::unix::fs::symlink(&outside, &permissions_link).expect("symlink permissions");

    let err = load_sibling_permissions(&config_path).expect_err("symlink permissions should fail");
    assert!(format!("{err:#}").contains("must not be a symlink"));
}

#[cfg(unix)]
#[test]
fn append_ask_rules_rejects_symlinked_permissions_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let outside = dir.path().join("outside-permissions.toml");
    let permissions_link = dir.path().join(PERMISSIONS_FILE_NAME);
    fs::write(&config_path, "model = \"deepseek-v4-flash\"\n").expect("write config");
    fs::write(&outside, "").expect("write outside permissions");
    let mut store = ConfigStore::load(Some(config_path)).expect("load store before link");
    std::os::unix::fs::symlink(&outside, &permissions_link).expect("symlink permissions");

    let err = store
        .append_ask_rules(&[ToolAskRule::exec_shell("cargo test")])
        .expect_err("symlink permissions should fail");

    assert!(format!("{err:#}").contains("must not be a symlink"));
    assert_eq!(
        fs::read_to_string(&outside).expect("read outside permissions"),
        ""
    );
}

#[cfg(unix)]
#[test]
fn remove_permission_rule_rejects_symlinked_permissions_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let outside = dir.path().join("outside-permissions.toml");
    let permissions_path = dir.path().join(PERMISSIONS_FILE_NAME);
    fs::write(
        &permissions_path,
        "[[rules]]\ntool = \"exec_shell\"\ncommand = \"cargo test\"\n",
    )
    .expect("write permissions");
    let snapshot =
        load_permissions_snapshot(Some(config_path.clone())).expect("load permission snapshot");
    let token = snapshot
        .removal_token(0)
        .expect("first rule token")
        .to_string();
    fs::rename(&permissions_path, &outside).expect("move permissions outside");
    std::os::unix::fs::symlink(&outside, &permissions_path).expect("symlink permissions");

    let err = remove_permission_rule(Some(config_path), 0, &token)
        .expect_err("symlink permissions should fail");

    assert!(format!("{err:#}").contains("must not be a symlink"));
    assert!(
        fs::read_to_string(&outside)
            .expect("read outside permissions")
            .contains("cargo test")
    );
}

#[cfg(unix)]
#[test]
fn write_config_backup_rejects_symlink_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let outside = dir.path().join("outside-backup.toml");
    let backup_link = config_backup_path(&config_path);
    fs::write(&config_path, "model = \"deepseek-v4-flash\"\n").expect("write config");
    fs::write(&outside, "").expect("write outside backup");
    std::os::unix::fs::symlink(&outside, &backup_link).expect("symlink backup");

    let err = write_one_time_config_backup(&config_path).expect_err("symlink backup should fail");
    assert!(format!("{err:#}").contains("must not be a symlink"));
}

#[cfg(unix)]
#[test]
fn save_clamps_existing_config_permissions() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "deepseek-config-perms-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(CONFIG_FILE_NAME);
    fs::write(&path, "api_key = \"old\"\n").expect("seed config");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod seed");

    let mut store = ConfigStore {
        path: path.clone(),
        config: {
            let mut config = ConfigToml::default();
            config.providers.deepseek.api_key = Some("new-secret".to_string());
            config
        },
        permissions: PermissionsToml::default(),
        original_raw: Some("api_key = \"old\"\n".to_string()),
        legacy_root: Default::default(),
    };
    store.save().expect("save");

    let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn config_store_save_skips_identical_serialized_body() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "codewhale-config-noop-save-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(CONFIG_FILE_NAME);
    let config = ConfigToml {
        model: Some("deepseek-v4-flash".to_string()),
        ..ConfigToml::default()
    };
    let body = toml::to_string_pretty(&config).expect("serialize");
    fs::write(&path, &body).expect("seed config");
    #[cfg(unix)]
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).expect("chmod seed");

    let mut store = ConfigStore {
        path: path.clone(),
        config,
        permissions: PermissionsToml::default(),
        original_raw: Some(body.clone()),
        legacy_root: Default::default(),
    };
    store.save().expect("identical save should not rewrite");

    #[cfg(unix)]
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod restore");
    assert_eq!(fs::read_to_string(&path).expect("read config"), body);
    assert!(
        !config_backup_path(&path).exists(),
        "no-op save must not create a migration backup"
    );

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn config_store_save_creates_one_time_backup_before_changed_write() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "codewhale-config-backup-save-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(CONFIG_FILE_NAME);
    let original = "model = \"deepseek-v4-flash\"\n";
    fs::write(&path, original).expect("seed config");

    let mut store = ConfigStore {
        path: path.clone(),
        config: ConfigToml {
            model: Some("deepseek-v4-pro".to_string()),
            ..ConfigToml::default()
        },
        permissions: PermissionsToml::default(),
        original_raw: Some(original.to_string()),
        legacy_root: Default::default(),
    };
    store.save().expect("changed save");

    let backup_path = config_backup_path(&path);
    assert_eq!(
        fs::read_to_string(&backup_path).expect("read backup"),
        original
    );
    let updated = fs::read_to_string(&path).expect("read updated config");
    assert!(updated.contains("model = \"deepseek-v4-pro\""));

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn config_backup_strips_plaintext_api_keys_but_preserves_non_secret_auth_metadata() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(CONFIG_FILE_NAME);
    let original = r#"api_key = "root-test-credential"
default_text_model = "deepseek-v4-pro"

[providers.openrouter]
api_key = "provider-test-credential"
api_key_env = "OPENROUTER_API_KEY"
auth_mode = "api_key"
"#;
    fs::write(&path, original).expect("seed config");

    let mut store = ConfigStore::load(Some(path.clone())).expect("load config");
    store.config.default_text_model = Some("deepseek-v4-flash".to_string());
    store.save().expect("changed save");

    let backup = fs::read_to_string(config_backup_path(&path)).expect("read backup");
    assert!(!backup.contains("root-test-credential"), "{backup}");
    assert!(!backup.contains("provider-test-credential"), "{backup}");
    assert!(
        !backup
            .lines()
            .any(|line| line.trim_start().starts_with("api_key ="))
    );
    assert!(backup.contains("api_key_env = \"OPENROUTER_API_KEY\""));
    assert!(backup.contains("auth_mode = \"api_key\""));
    assert!(backup.contains("default_text_model = \"deepseek-v4-pro\""));
}

#[test]
fn config_backup_scrub_repairs_an_existing_plaintext_backup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(CONFIG_FILE_NAME);
    fs::write(&path, "model = \"new-model\"\n").expect("seed config");
    let backup_path = config_backup_path(&path);
    fs::write(
        &backup_path,
        "api_key = \"old-test-credential\"\nmodel = \"old-model\"\n",
    )
    .expect("seed backup");

    scrub_plaintext_api_keys_from_config_backup(&path).expect("scrub backup");

    let backup = fs::read_to_string(backup_path).expect("read backup");
    assert!(!backup.contains("old-test-credential"), "{backup}");
    assert!(!backup.contains("api_key"), "{backup}");
    assert!(backup.contains("model = \"old-model\""));
}

#[test]
fn config_store_save_preserves_comments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let original = "# my model\nmodel = \"deepseek-v4-flash\"\n# end comment\n";
    fs::write(&config_path, original).expect("write config");

    let mut store = ConfigStore::load(Some(config_path.clone())).expect("load config store");
    store.config.model = Some("deepseek-v4-pro".to_string());
    store.save().expect("save");

    let body = fs::read_to_string(&config_path).expect("read config");
    assert!(body.contains("# my model"), "prefix comment preserved");
    assert!(body.contains("# end comment"), "suffix comment preserved");
    assert!(body.contains("model = \"deepseek-v4-pro\""));
}

#[test]
fn config_store_save_preserves_disabled_keys() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    fs::write(
        &config_path,
        "# my note\nmodel = \"deepseek-v4-flash\"\n# base_url = \"http://localhost:11434/v1\"\n",
    )
    .expect("write config");

    let mut store = ConfigStore::load(Some(config_path.clone())).expect("load config store");
    store.config.model = Some("deepseek-v4-pro".to_string());
    store.save().expect("save");

    let body = fs::read_to_string(&config_path).expect("read config");
    assert!(
        body.contains("# base_url = \"http://localhost:11434/v1\""),
        "disabled key preserved as comment"
    );
    assert!(body.contains("model = \"deepseek-v4-pro\""));
}

#[test]
fn config_store_save_preserves_comments_with_other_keys() {
    // Realistic scenario: user already has api_key + model, adds a comment,
    // then changes model via `codewhale config set model`.
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    fs::write(
        &config_path,
        "# my deepseek key\napi_key = \"sk-1234\"\n\n# my current model\nmodel = \"deepseek-v4-flash\"\n",
    )
    .expect("write config");

    let mut store = ConfigStore::load(Some(config_path.clone())).expect("load config store");
    store.config.model = Some("deepseek-v4-pro".to_string());
    store.save().expect("save");

    let body = fs::read_to_string(&config_path).expect("read config");
    assert!(body.contains("# my deepseek key"), "api_key comment lost");
    assert!(body.contains("# my current model"), "model comment lost");
    assert!(
        body.contains("model = \"deepseek-v4-pro\""),
        "new model not written"
    );
    assert!(body.contains("api_key = \"sk-1234\""), "api_key lost");
}

#[test]
fn setup_transaction_applies_config_store_body_preserving_comments() {
    // #3410: the comment-preserving ConfigStore write must compose with
    // SetupTransaction so a setup step can update config.toml atomically
    // alongside sibling setup files.
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let state_path = dir.path().join(crate::setup_state::SETUP_STATE_FILE_NAME);
    fs::write(
        &config_path,
        "# my model\nmodel = \"deepseek-v4-flash\"\n# end comment\n",
    )
    .expect("write config");

    let mut store = ConfigStore::load(Some(config_path.clone())).expect("load config store");
    store.config.model = Some("deepseek-v4-pro".to_string());

    let mut transaction = persistence::SetupTransaction::new();
    transaction.stage(
        &config_path,
        store.rendered_body().expect("rendered body").into_bytes(),
    );
    transaction
        .stage_json(&state_path, &SetupState::default())
        .expect("stage setup state");
    transaction.commit().expect("commit");

    let body = fs::read_to_string(&config_path).expect("read config");
    assert!(body.contains("# my model"), "prefix comment preserved");
    assert!(body.contains("# end comment"), "suffix comment preserved");
    assert!(body.contains("model = \"deepseek-v4-pro\""));
    assert!(state_path.exists(), "sibling setup state written");
}

#[test]
fn setup_transaction_rolls_back_config_store_body_on_sibling_failure() {
    // #3410 rollback expectation: when a sibling stage fails to apply, the
    // already-written config.toml is restored byte-for-byte, comments and
    // all — no half-applied setup.
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let original = "# my model\nmodel = \"deepseek-v4-flash\"\n# end comment\n";
    fs::write(&config_path, original).expect("write config");
    // A parent that is a regular file makes the second stage unwritable.
    let blocker = dir.path().join("blocker");
    fs::write(&blocker, b"file, not a directory").expect("write blocker");

    let mut store = ConfigStore::load(Some(config_path.clone())).expect("load config store");
    store.config.model = Some("deepseek-v4-pro".to_string());

    let mut transaction = persistence::SetupTransaction::new();
    transaction.stage(
        &config_path,
        store.rendered_body().expect("rendered body").into_bytes(),
    );
    transaction.stage(blocker.join("nested.json"), b"{}".to_vec());
    transaction
        .commit()
        .expect_err("commit must fail on unwritable sibling");

    let body = fs::read_to_string(&config_path).expect("read config");
    assert_eq!(body, original, "config restored byte-for-byte on rollback");
}

#[test]
fn config_store_load_fails_on_malformed_config_without_touching_file() {
    // #3410 malformed-config posture: repair is explicit, never implicit.
    // Loading a malformed config surfaces a parse error naming the path and
    // leaves the file bytes untouched for the user (or doctor) to repair.
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    let secret = "cw-secret-store-load-4507";
    let malformed =
        format!("# half-edited config\n[providers.xai]\napi_key = \"{secret}\" trailing-junk\n");
    fs::write(&config_path, &malformed).expect("write config");

    let err = ConfigStore::load(Some(config_path.clone())).expect_err("malformed must not parse");
    let diagnostic = format!("{err:#}");

    assert!(
        diagnostic.contains("failed to parse config"),
        "error should name the parse failure: {err:#}"
    );
    assert!(!diagnostic.contains(secret), "{diagnostic}");
    assert!(!diagnostic.contains("api_key"), "{diagnostic}");
    assert!(diagnostic.contains("file contents were omitted"));
    let body = fs::read_to_string(&config_path).expect("read config");
    assert_eq!(body, malformed, "malformed config left untouched");
}

#[test]
fn env_api_key_lookup_reads_exactly_each_providers_env_vars() {
    // `auth print-api-key` and the dispatcher resolve through this lookup; it
    // used to read a second hand-kept table in the secrets crate that had no
    // entry for mistral, minimax, zai, orcarouter, google, stepfun, qianfan,
    // or the Anthropic-dialect routes, so their exported keys were ignored.
    let _lock = env_lock();
    let all_vars: std::collections::BTreeSet<&str> = crate::provider::all_providers()
        .iter()
        .flat_map(|provider| provider.env_vars().iter().copied())
        .collect();
    let saved: Vec<_> = all_vars
        .iter()
        .map(|var| (*var, std::env::var_os(var)))
        .collect();
    for var in &all_vars {
        unsafe { std::env::remove_var(var) };
    }
    for provider in crate::provider::all_providers() {
        let kind = provider.kind();
        let mut reads = Vec::new();
        for var in &all_vars {
            unsafe { std::env::set_var(var, "probe-key") };
            if super::env_api_key_for_provider(kind).as_deref() == Some("probe-key") {
                reads.push(*var);
            }
            unsafe { std::env::remove_var(var) };
        }
        // Xiaomi MiMo's token-plan variables are read only by the mode-aware
        // lookup, never by this generic fallback.
        let mut declared = if kind == ProviderKind::XiaomiMimo {
            super::XIAOMI_MIMO_STANDARD_ENV_VARS.to_vec()
        } else {
            provider.env_vars().to_vec()
        };
        declared.sort_unstable();
        assert_eq!(reads, declared, "{}", kind.as_str());
    }
    for (var, value) in saved {
        if let Some(value) = value {
            unsafe { std::env::set_var(var, value) };
        }
    }
}

#[test]
fn toml_errors_name_line_and_column_but_never_the_value() {
    // A type error's message quotes the string (`invalid type: string "…"`)
    // and a syntax error's snippet quotes the whole line; both can carry a
    // credential, so every layer reports only where the error is.
    let canary = "LEAKCANARY0123456789";
    let secret = format!("sk-live-{canary}");
    let cases = [
        (
            format!("model = \"x\"\ntelemetry = \"{secret}\"\n"),
            "line 2, column 13, in `telemetry`",
        ),
        (
            format!("model = \"x\"\n\n[providers.xai]\napi_key = \"{secret}\" junk\n"),
            "line 4, column",
        ),
    ];
    for (body, location) in &cases {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join(CONFIG_FILE_NAME);
        fs::write(&config_path, body).expect("write config");
        let error = format!(
            "{:#}",
            ConfigStore::load(Some(config_path)).expect_err("invalid config")
        );
        assert!(!error.contains(canary), "{error}");
        assert!(error.contains(location), "{error}");

        let workspace = tempfile::tempdir().expect("workspace");
        let project_dir = workspace.path().join(CODEWHALE_APP_DIR);
        fs::create_dir_all(&project_dir).expect("project dir");
        fs::write(project_dir.join(CONFIG_FILE_NAME), body).expect("write project config");
        let outcome = load_project_config_outcome(workspace.path());
        let (_, reason) = outcome.invalid().expect("invalid project config");
        assert!(!reason.contains(canary), "{reason}");
        assert!(reason.contains(location), "{reason}");
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    fs::write(&config_path, "model = \"x\"\n").expect("write config");
    fs::write(
        dir.path().join(PERMISSIONS_FILE_NAME),
        format!("[[rules]]\ntool = \"{secret}\" junk\n"),
    )
    .expect("write permissions");
    let error = format!(
        "{:#}",
        load_permissions_snapshot(Some(config_path)).expect_err("invalid permissions")
    );
    assert!(!error.contains(canary), "{error}");
    assert!(error.contains("line 2, column"), "{error}");
}

#[test]
fn config_store_rendered_body_preserves_comments_at_legacy_deepseek_path() {
    // #3410 legacy case: a config still living under `.deepseek/` keeps its
    // comments when written back through a transaction at the same path.
    let dir = tempfile::tempdir().expect("tempdir");
    let legacy_dir = dir.path().join(".deepseek");
    fs::create_dir_all(&legacy_dir).expect("legacy dir");
    let config_path = legacy_dir.join(CONFIG_FILE_NAME);
    fs::write(
        &config_path,
        "# legacy home config\nmodel = \"deepseek-v4-flash\"\n",
    )
    .expect("write config");

    let mut store = ConfigStore::load(Some(config_path.clone())).expect("load config store");
    store.config.model = Some("deepseek-v4-pro".to_string());

    let mut transaction = persistence::SetupTransaction::new();
    transaction.stage(
        &config_path,
        store.rendered_body().expect("rendered body").into_bytes(),
    );
    transaction.commit().expect("commit");

    let body = fs::read_to_string(&config_path).expect("read config");
    assert!(body.contains("# legacy home config"), "comment preserved");
    assert!(body.contains("model = \"deepseek-v4-pro\""));
}

#[test]
fn merge_and_preserve_comments_returns_err_on_invalid_serialized() {
    let secret = "sentinel";
    let err = merge_and_preserve_comments(
        &format!("api_key = \"{secret}\" trailing-junk\n"),
        "model = 1\n",
    )
    .expect_err("invalid serialized should fail");
    let diagnostic = format!("{err:#}");
    assert!(
        diagnostic.contains("failed to parse serialized"),
        "unexpected error: {err:#}"
    );
    assert!(!diagnostic.contains(secret), "{diagnostic}");
    assert!(!diagnostic.contains("api_key"), "{diagnostic}");
}

#[test]
fn merge_and_preserve_comments_returns_err_on_invalid_original() {
    let secret = "cw-secret-original-4507";
    let err = merge_and_preserve_comments(
        "model = 1\n",
        &format!("api_key = \"{secret}\" trailing-junk\n"),
    )
    .expect_err("invalid original should fail");
    let diagnostic = format!("{err:#}");
    assert!(
        diagnostic.contains("failed to parse original"),
        "unexpected error: {err:#}"
    );
    assert!(!diagnostic.contains(secret), "{diagnostic}");
    assert!(!diagnostic.contains("api_key"), "{diagnostic}");
}

#[test]
fn config_store_save_rejects_a_stale_or_corrupt_original_snapshot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    // A full typed writer must never overwrite bytes that differ from its
    // original snapshot, even when that snapshot is corrupt.
    fs::write(&config_path, "model = \"deepseek-v4-flash\"\n").expect("write config");

    // Bypass ConfigStore::load to inject a deliberately broken original_raw.
    let mut store = ConfigStore {
        path: config_path.clone(),
        config: ConfigToml {
            model: Some("deepseek-v4-pro".to_string()),
            ..ConfigToml::default()
        },
        permissions: PermissionsToml::default(),
        original_raw: Some("{ broken".to_string()),
        legacy_root: Default::default(),
    };
    let error = store
        .save()
        .expect_err("stale original bytes must fail instead of overwriting");
    assert!(
        error.to_string().contains("reload") && error.to_string().contains("retry"),
        "{error:#}"
    );

    let body = fs::read_to_string(&config_path).expect("read config");
    assert_eq!(body, "model = \"deepseek-v4-flash\"\n");
}

#[test]
fn sequential_config_stores_cannot_resurrect_a_concurrent_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    fs::write(
        &config_path,
        "model = \"before\"\n[providers.xai.external_credentials]\naccess = \"read_only\"\nprovider = \"xai\"\nsource = \"grok_cli\"\npath = \"/external/auth.json\"\nconsent_version = 1\n",
    )
    .expect("seed config");
    let mut stale = ConfigStore::load(Some(config_path.clone())).expect("load stale store");

    mutate_config_document(&config_path, |document| {
        unset_config_document_value(document, &["providers", "xai", "external_credentials"])?;
        set_config_document_value(document, &["tui", "low_motion"], true)
    })
    .expect("concurrent targeted update");
    stale.config.model = Some("stale-writer".to_string());
    let error = stale
        .save()
        .expect_err("stale typed snapshot must not overwrite revocation");
    assert!(error.to_string().contains("config changed"), "{error:#}");

    let saved = fs::read_to_string(config_path).expect("read final config");
    assert!(!saved.contains("external_credentials"), "{saved}");
    assert!(saved.contains("low_motion = true"), "{saved}");
    assert!(!saved.contains("stale-writer"), "{saved}");
}

#[test]
fn provider_kind_parses_openrouter_and_novita_aliases() {
    assert_eq!(
        ProviderKind::parse("openrouter"),
        Some(ProviderKind::Openrouter)
    );
    assert_eq!(
        ProviderKind::parse("OPEN_ROUTER"),
        Some(ProviderKind::Openrouter)
    );
    assert_eq!(
        ProviderKind::parse("xiaomi-mimo"),
        Some(ProviderKind::XiaomiMimo)
    );
    assert_eq!(
        ProviderKind::parse("xiaomi"),
        Some(ProviderKind::XiaomiMimo)
    );
    assert_eq!(ProviderKind::parse("novita"), Some(ProviderKind::Novita));
    assert_eq!(ProviderKind::parse("Novita"), Some(ProviderKind::Novita));
    assert_eq!(
        ProviderKind::parse("fireworks-ai"),
        Some(ProviderKind::Fireworks)
    );
    assert_eq!(
        ProviderKind::parse("silicon-flow"),
        Some(ProviderKind::Siliconflow)
    );
    assert_eq!(
        ProviderKind::parse("silicon_flow"),
        Some(ProviderKind::Siliconflow)
    );
    assert_eq!(ProviderKind::parse("kimi"), Some(ProviderKind::Moonshot));
    assert_eq!(
        ProviderKind::parse("moonshot-ai"),
        Some(ProviderKind::Moonshot)
    );
    assert_eq!(ProviderKind::parse("sg-lang"), Some(ProviderKind::Sglang));
    assert_eq!(ProviderKind::parse("v-llm"), Some(ProviderKind::Vllm));
    assert_eq!(ProviderKind::parse("vllm"), Some(ProviderKind::Vllm));
    assert_eq!(ProviderKind::parse("ollama"), Some(ProviderKind::Ollama));
    assert_eq!(
        ProviderKind::parse("ollama-local"),
        Some(ProviderKind::Ollama)
    );
    for alias in ["ollama-cloud", "ollama_cloud"] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::OllamaCloud));
        let parsed: ConfigToml =
            toml::from_str(&format!("provider = \"{alias}\"")).expect("ollama cloud alias");
        assert_eq!(parsed.provider, ProviderKind::OllamaCloud);
    }
    assert_eq!(
        ProviderKind::parse("wanjie-ark"),
        Some(ProviderKind::WanjieArk)
    );
    assert_eq!(
        ProviderKind::parse("ark_wanjie"),
        Some(ProviderKind::WanjieArk)
    );
    for alias in ["huggingface", "hugging-face", "hugging_face", "hf"] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::Huggingface));

        let parsed: ConfigToml =
            toml::from_str(&format!("provider = \"{alias}\"")).expect("huggingface alias");
        assert_eq!(parsed.provider, ProviderKind::Huggingface);
    }

    for alias in ["modelscope", "modelscope-cn"] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::Modelscope));

        let parsed: ConfigToml =
            toml::from_str(&format!("provider = \"{alias}\"")).expect("modelscope alias");
        assert_eq!(parsed.provider, ProviderKind::Modelscope);
    }

    for alias in ["deepinfra", "deep-infra", "deep_infra"] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::Deepinfra));

        let parsed: ConfigToml =
            toml::from_str(&format!("provider = \"{alias}\"")).expect("deepinfra alias");
        assert_eq!(parsed.provider, ProviderKind::Deepinfra);
    }

    for alias in ["sakana", "sakana-ai", "sakana_ai", "fugu"] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::Sakana));

        let parsed: ConfigToml =
            toml::from_str(&format!("provider = \"{alias}\"")).expect("sakana alias");
        assert_eq!(parsed.provider, ProviderKind::Sakana);
    }

    for alias in [
        "mistral",
        "mistral-ai",
        "mistral_ai",
        "mistralai",
        "la-plateforme",
        "la_plateforme",
    ] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::Mistral));

        let parsed: ConfigToml =
            toml::from_str(&format!("provider = \"{alias}\"")).expect("mistral alias");
        assert_eq!(parsed.provider, ProviderKind::Mistral);
    }

    for alias in ["qianfan", "baidu-qianfan", "baidu_qianfan", "baidu"] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::Qianfan));

        let parsed: ConfigToml =
            toml::from_str(&format!("provider = \"{alias}\"")).expect("qianfan alias");
        assert_eq!(parsed.provider, ProviderKind::Qianfan);
    }

    let parsed: ConfigToml =
        toml::from_str("provider = \"ark-wanjie\"").expect("wanjie provider alias");
    assert_eq!(parsed.provider, ProviderKind::WanjieArk);

    let parsed: ConfigToml =
        toml::from_str("provider = \"silicon-flow\"").expect("siliconflow provider alias");
    assert_eq!(parsed.provider, ProviderKind::Siliconflow);
}

/// Models.dev publishes provider ids that do not always match CodeWhale's
/// canonical id (`fireworks-ai`, `togetherai`, `novita-ai`, `moonshotai`).
/// These MUST normalize onto the right [`ProviderKind`] via
/// [`ProviderKind::parse`], which is the seam `ModelReferenceCard::from_offering`
/// uses to label a live-catalog row's provider kind. A miss here means
/// Fireworks/Together/Novita/Moonshot models from the live Models.dev catalog
/// land under an `unknown` kind (Refs #4186).
#[test]
fn provider_kind_normalizes_models_dev_provider_ids() {
    let cases = [
        ("fireworks-ai", ProviderKind::Fireworks),
        ("togetherai", ProviderKind::Together),
        ("together-ai", ProviderKind::Together),
        ("together_ai", ProviderKind::Together),
        ("novita-ai", ProviderKind::Novita),
        ("novita_ai", ProviderKind::Novita),
        // Live Models.dev key for Moonshot/Kimi (verified 2026-07-08).
        ("moonshotai", ProviderKind::Moonshot),
        ("moonshot-ai", ProviderKind::Moonshot),
        ("moonshot_ai", ProviderKind::Moonshot),
        ("nvidia", ProviderKind::NvidiaNim),
        ("xiaomi", ProviderKind::XiaomiMimo),
        ("deepinfra", ProviderKind::Deepinfra),
        ("siliconflow", ProviderKind::Siliconflow),
        // Models.dev spells the China endpoint `siliconflow-cn`; CodeWhale's
        // canonical id is `siliconflow-CN` and `parse` is case-insensitive.
        ("siliconflow-cn", ProviderKind::SiliconflowCN),
        ("openrouter", ProviderKind::Openrouter),
        ("longcat", ProviderKind::LongCat),
        ("xai", ProviderKind::Xai),
        ("x-ai", ProviderKind::Xai),
        ("x_ai", ProviderKind::Xai),
        ("grok", ProviderKind::Xai),
    ];
    for (models_dev_id, expected) in cases {
        assert_eq!(
            ProviderKind::parse(models_dev_id),
            Some(expected),
            "Models.dev id {models_dev_id:?} must normalize onto {expected:?}"
        );
    }

    // The separator-free Models.dev ids must also deserialize from config TOML,
    // so a `provider = "togetherai"` / `"novita-ai"` / `"moonshotai"` line
    // resolves identically.
    for (alias, expected) in [
        ("togetherai", ProviderKind::Together),
        ("novita-ai", ProviderKind::Novita),
        ("fireworks-ai", ProviderKind::Fireworks),
        ("moonshotai", ProviderKind::Moonshot),
        ("grok", ProviderKind::Xai),
    ] {
        let parsed: ConfigToml =
            toml::from_str(&format!("provider = \"{alias}\"")).expect("models.dev id alias");
        assert_eq!(parsed.provider, expected, "toml provider = {alias:?}");
    }
}

/// Pin the Fireworks and Together transport metadata against the real provider
/// APIs: the OpenAI-compatible base URL and the canonical API-key env var. These
/// are the two primary providers this audit targets, so a regression to a wrong
/// base URL or env var name fails here.
#[test]
fn fireworks_and_together_base_url_and_auth_metadata() {
    let fireworks = provider::provider_for_kind(ProviderKind::Fireworks);
    assert_eq!(fireworks.id(), "fireworks");
    assert_eq!(
        fireworks.default_base_url(),
        "https://api.fireworks.ai/inference/v1"
    );
    assert_eq!(fireworks.default_base_url(), DEFAULT_FIREWORKS_BASE_URL);
    assert_eq!(fireworks.env_vars(), &["FIREWORKS_API_KEY"]);
    // Fireworks wire model ids are namespaced `accounts/fireworks/models/<name>`.
    assert!(
        fireworks
            .default_model()
            .starts_with("accounts/fireworks/models/"),
        "Fireworks default model must use the accounts/fireworks/models/ prefix, got {:?}",
        fireworks.default_model()
    );

    let together = provider::provider_for_kind(ProviderKind::Together);
    assert_eq!(together.id(), "together");
    assert_eq!(together.default_base_url(), "https://api.together.xyz/v1");
    assert_eq!(together.default_base_url(), DEFAULT_TOGETHER_BASE_URL);
    assert_eq!(together.env_vars(), &["TOGETHER_API_KEY"]);
    // Together wire model ids are `<org>/<Model>` (a slash-namespaced id).
    assert!(
        together.default_model().contains('/'),
        "Together default model must be an <org>/<Model> id, got {:?}",
        together.default_model()
    );

    // The env-based key resolver (secrets crate) must recognize both providers
    // by canonical id; a shell-exported key would otherwise be ignored.
    let _lock = env_lock();
    unsafe {
        std::env::set_var("FIREWORKS_API_KEY", "fw-test-key");
        std::env::set_var("TOGETHER_API_KEY", "tg-test-key");
    }
    assert_eq!(
        super::env_api_key_for_provider(ProviderKind::Fireworks).as_deref(),
        Some("fw-test-key")
    );
    assert_eq!(
        super::env_api_key_for_provider(ProviderKind::Together).as_deref(),
        Some("tg-test-key")
    );
    unsafe {
        std::env::remove_var("FIREWORKS_API_KEY");
        std::env::remove_var("TOGETHER_API_KEY");
    }
}

#[test]
fn unknown_provider_error_lists_huggingface() {
    let mut config = ConfigToml::default();
    let err = config
        .set_value("provider", "not-a-provider")
        .expect_err("unknown provider should fail");
    let message = err.to_string();
    assert!(message.contains("unknown provider 'not-a-provider'"));
    assert!(message.contains("huggingface"));
}

#[test]
fn config_store_preserves_named_custom_provider_identity_across_typed_dispatch_reads() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        r#"# written by the TUI custom-provider flow
provider = "acme_zen_gateway"

[providers.acme_zen_gateway]
kind = "openai-compatible"
base_url = "https://acme.example/v1"
model = "deepseek-v4-flash-free"
api_key_env = "ACME_ZEN_GATEWAY_API_KEY"
"#,
    )
    .expect("custom provider fixture");

    let mut store = ConfigStore::load(Some(path.clone())).expect("dispatcher config should load");
    assert_eq!(store.config.provider, ProviderKind::Custom);
    assert_eq!(store.config.provider_id(), "acme_zen_gateway");
    assert_eq!(
        store.config.get_value("provider").as_deref(),
        Some("acme_zen_gateway")
    );
    assert_eq!(
        store
            .config
            .list_values()
            .get("provider")
            .map(String::as_str),
        Some("acme_zen_gateway")
    );

    let resolved = store
        .config
        .resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.provider, ProviderKind::Custom);
    assert_eq!(resolved.provider_source, ProviderSource::Config);
    assert_eq!(resolved.base_url, "https://acme.example/v1");
    assert_eq!(resolved.model, "deepseek-v4-flash-free");

    store
        .config
        .set_value("telemetry", "false")
        .expect("unrelated typed mutation");
    let rendered = store
        .rendered_body()
        .expect("render custom provider config");
    assert!(
        rendered.contains("provider = \"acme_zen_gateway\""),
        "{rendered}"
    );
    assert!(!rendered.contains("provider = \"custom\""), "{rendered}");
    assert!(!rendered.contains("[providers.custom]"), "{rendered}");

    store.save().expect("save custom provider config");
    let reloaded = ConfigStore::load(Some(path)).expect("reload custom provider config");
    assert_eq!(reloaded.config.provider_id(), "acme_zen_gateway");
}

#[test]
fn config_store_preserves_builtin_shadowing_custom_and_regional_selectors() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    for (selector, kind, table) in [
        (
            "OpenAI",
            ProviderKind::Custom,
            "[providers.OpenAI]\nkind = 'openai-compatible'\nbase_url = 'https://gateway.example/v1'\nmodel = 'Exact-Model'\n",
        ),
        (
            "deepseek-cn",
            ProviderKind::Deepseek,
            "[providers.deepseek_cn]\nbase_url = 'https://api.deepseek.cn'\nmodel = 'deepseek-v4-flash'\n",
        ),
    ] {
        fs::write(&path, format!("provider = '{selector}'\n{table}")).unwrap();
        let mut store = ConfigStore::load(Some(path.clone())).unwrap();
        assert_eq!(store.config.provider, kind);
        assert_eq!(store.config.provider_id(), selector);
        if kind == ProviderKind::Custom {
            let route = store
                .config
                .resolve_runtime_options(&CliRuntimeOverrides::default());
            assert_eq!(route.base_url, "https://gateway.example/v1");
            assert_eq!(route.model, "Exact-Model");
        }
        store.config.set_value("verbosity", "concise").unwrap();
        store.save().unwrap();
        let saved: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["provider"].as_str(), Some(selector));
        let mut reloaded = ConfigStore::load(Some(path.clone())).unwrap();
        assert_eq!(reloaded.config.provider, kind);
        assert_eq!(reloaded.config.provider_id(), selector);
        reloaded.config.set_value("provider", selector).unwrap();
        assert_eq!(reloaded.config.provider, kind);
        assert_eq!(reloaded.config.provider_id(), selector);
        // Direct typed callers that change kind cannot retain an old alias.
        reloaded.config.provider = if kind == ProviderKind::Custom {
            ProviderKind::Openai
        } else {
            ProviderKind::Custom
        };
        assert_eq!(
            reloaded.config.provider_id(),
            reloaded.config.provider.as_str()
        );
        assert!(reloaded.config.named_custom_provider_id().is_none());
        reloaded.save().unwrap();
        let rebound = ConfigStore::load(Some(path.clone())).unwrap();
        assert_eq!(rebound.config.provider, reloaded.config.provider);
        reloaded.config.provider = ProviderKind::Zai;
        assert_eq!(reloaded.config.provider_id(), "zai");
    }
}

#[test]
fn named_custom_root_provider_requires_a_matching_openai_compatible_table() {
    for body in [
        "provider = \"acme_zen_gateway\"\n",
        r#"provider = "acme_zen_gateway"

[providers.acme_zen_gateway]
kind = "anthropic-messages"
base_url = "https://acme.example/v1"
"#,
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(&path, body).expect("invalid custom provider fixture");
        let err = ConfigStore::load(Some(path)).expect_err("invalid custom route should fail");
        let message = format!("{err:#}");
        assert!(message.contains("acme_zen_gateway"), "{message}");
        assert!(message.contains("openai-compatible") || message.contains("matching"));
    }
}

#[test]
fn kindless_table_mirroring_a_builtin_alias_keeps_the_builtin_route() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    // A kindless `[providers.deepseek-cn]` table merely mirrors the regional
    // selector spelling. It is not an openai-compatible custom provider, so it
    // must stay inert: the selector still binds the built-in DeepSeek kind and
    // the load must not fail.
    fs::write(
        &path,
        "provider = 'deepseek-cn'\n[providers.deepseek-cn]\nmodel = 'deepseek-v4-flash'\n",
    )
    .expect("kindless alias fixture");
    let mut store = ConfigStore::load(Some(path.clone())).expect("kindless alias table must load");
    assert_eq!(store.config.provider, ProviderKind::Deepseek);
    assert_eq!(store.config.provider_id(), "deepseek-cn");
    assert!(store.config.named_custom_provider_id().is_none());
    // An unrelated typed save leaves the inert extras table untouched.
    store.config.set_value("verbosity", "concise").unwrap();
    store.save().unwrap();
    let saved: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["provider"].as_str(), Some("deepseek-cn"));
    assert_eq!(
        saved["providers"]["deepseek-cn"]["model"].as_str(),
        Some("deepseek-v4-flash")
    );
    let reloaded = ConfigStore::load(Some(path.clone())).expect("reload kindless alias config");
    assert_eq!(reloaded.config.provider, ProviderKind::Deepseek);
    assert_eq!(reloaded.config.provider_id(), "deepseek-cn");

    // A table that does validate as openai-compatible still takes precedence
    // over the built-in alias.
    fs::write(
        &path,
        "provider = 'deepseek-cn'\n[providers.deepseek-cn]\nkind = 'openai-compatible'\nbase_url = 'https://gateway.example/v1'\nmodel = 'Exact-CN'\n",
    )
    .expect("valid custom table fixture");
    let store = ConfigStore::load(Some(path)).expect("valid custom table takes precedence");
    assert_eq!(store.config.provider, ProviderKind::Custom);
    assert_eq!(store.config.provider_id(), "deepseek-cn");
    assert_eq!(store.config.named_custom_provider_id(), Some("deepseek-cn"));
}

#[test]
fn invalid_custom_kind_never_falls_back_to_a_builtin_alias() {
    for kind in ["'unsupported'", "42", "false"] {
        let mut config: ConfigToml = toml::from_str(&format!(
            "provider = 'deepseek-cn'\n[providers.deepseek-cn]\nkind = {kind}\n"
        ))
        .unwrap();
        assert!(config.bind_persisted_provider_id("deepseek-cn").is_err());
    }
}

#[test]
fn provider_kind_accepts_legacy_deepseek_cn_aliases() {
    for alias in [
        "deepseek-cn",
        "deepseek_china",
        "deepseekcn",
        "deepseek-china",
    ] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::Deepseek));

        let parsed: ConfigToml =
            toml::from_str(&format!("provider = \"{alias}\"")).expect("legacy provider alias");
        assert_eq!(parsed.provider, ProviderKind::Deepseek);
    }
}

#[test]
fn deepseek_anthropic_aliases_collapse_onto_primary_with_wire_toggle() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Dialect is not a catalog identity — aliases resolve to DeepSeek primary.
    for alias in [
        "deepseek-anthropic",
        "deepseek_anthropic",
        "deepseek-claude",
        "deepseek_claude",
    ] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::Deepseek));
    }

    let provider = provider::resolve_provider("deepseek-anthropic")
        .expect("deepseek anthropic alias resolves to primary");
    assert_eq!(provider.kind(), ProviderKind::Deepseek);
    assert_eq!(provider.id(), "deepseek");

    // wire=anthropic selects the Messages endpoint without a second provider.
    let config: ConfigToml = toml::from_str(
        r#"
provider = "deepseek"

[providers.deepseek]
wire = "anthropic"
"#,
    )
    .expect("deepseek wire config");
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.provider, ProviderKind::Deepseek);
    assert_eq!(resolved.base_url, DEFAULT_DEEPSEEK_ANTHROPIC_BASE_URL);
    // Legacy serde kind still resolves the anthropic endpoint.
    let legacy = ConfigToml {
        provider: ProviderKind::DeepseekAnthropic,
        ..ConfigToml::default()
    };
    let legacy_resolved = legacy.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(
        legacy_resolved.base_url,
        DEFAULT_DEEPSEEK_ANTHROPIC_BASE_URL
    );
}

#[test]
fn openmodel_route_defaults_to_messages_endpoint() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    for alias in ["openmodel", "open-model", "open_model"] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::Openmodel));

        let parsed: ConfigToml =
            toml::from_str(&format!("provider = \"{alias}\"")).expect("openmodel alias");
        assert_eq!(parsed.provider, ProviderKind::Openmodel);
    }

    let provider = provider::resolve_provider("openmodel").expect("openmodel metadata resolves");
    assert_eq!(provider.kind(), ProviderKind::Openmodel);
    assert_eq!(provider.provider_config_key(), "openmodel");
    assert_eq!(provider.default_model(), DEFAULT_OPENMODEL_MODEL);
    assert_eq!(provider.default_base_url(), DEFAULT_OPENMODEL_BASE_URL);
    assert_eq!(provider.env_vars(), &["OPENMODEL_API_KEY"]);
    assert_eq!(
        provider.wire_policy().fixed(),
        Some(provider::WireFormat::AnthropicMessages)
    );

    let config = ConfigToml {
        provider: ProviderKind::Openmodel,
        ..ConfigToml::default()
    };
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Openmodel);
    assert_eq!(resolved.base_url, DEFAULT_OPENMODEL_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_OPENMODEL_MODEL);

    unsafe {
        std::env::set_var("OPENMODEL_BASE_URL", "https://gateway.example.test");
        std::env::set_var("OPENMODEL_MODEL", "claude-sonnet-4-20250514");
    }
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.base_url, "https://gateway.example.test");
    assert_eq!(resolved.model, "claude-sonnet-4-20250514");
    unsafe {
        std::env::remove_var("OPENMODEL_BASE_URL");
        std::env::remove_var("OPENMODEL_MODEL");
    }
}

#[test]
fn xai_api_key_provider_resolves_defaults_and_scopes_env_credentials() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    let config = ConfigToml {
        provider: ProviderKind::Xai,
        ..ConfigToml::default()
    };
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Xai);
    assert_eq!(resolved.base_url, DEFAULT_XAI_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_XAI_MODEL);
    assert_eq!(resolved.api_key, None);

    unsafe {
        std::env::set_var("XAI_API_KEY", "xai-env-key");
        std::env::set_var("XAI_BASE_URL", "https://xai-gateway.example/v1");
        std::env::set_var("XAI_MODEL", "grok-4.3");
    }

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
    assert_eq!(resolved.base_url, "https://xai-gateway.example/v1");
    assert_eq!(resolved.model, "grok-4.3");
}

#[test]
fn mistral_provider_resolves_defaults_and_metadata() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    let metadata = provider::resolve_provider("mistral").expect("mistral provider metadata");
    assert_eq!(metadata.id(), "mistral");
    assert_eq!(metadata.kind(), ProviderKind::Mistral);
    assert_eq!(metadata.display_name(), "Mistral AI");
    assert_eq!(metadata.provider_config_key(), "mistral");
    assert_eq!(metadata.default_base_url(), "https://api.mistral.ai/v1");
    assert_eq!(metadata.default_model(), "mistral-code-latest");
    assert_eq!(metadata.env_vars(), &["MISTRAL_API_KEY"]);
    assert_eq!(
        metadata.wire_policy().fixed(),
        Some(provider::WireFormat::ChatCompletions)
    );

    let help = metadata.credential_help();
    assert_eq!(
        help.acquisition,
        provider::CredentialAcquisition::ApiKey,
        "Mistral is a hosted API-key provider"
    );
    assert_eq!(
        help.credential_url,
        Some("https://console.mistral.ai/api-keys")
    );

    let config: ConfigToml = toml::from_str(
        r#"
provider = "mistral-ai"

[providers.mistral]
api_key = "mistral-config-key"
model = "mistral-large-latest"
"#,
    )
    .expect("mistral provider table");
    assert_eq!(config.provider, ProviderKind::Mistral);
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.provider, ProviderKind::Mistral);
    assert_eq!(resolved.base_url, "https://api.mistral.ai/v1");
    assert_eq!(resolved.model, "mistral-large-latest");
    assert_eq!(resolved.api_key.as_deref(), Some("mistral-config-key"));

    unsafe {
        std::env::set_var("MISTRAL_BASE_URL", "https://api.eu.mistral.ai/v1");
        std::env::set_var("MISTRAL_MODEL", "mistral-medium-latest");
    }
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.base_url, "https://api.eu.mistral.ai/v1");
    assert_eq!(resolved.model, "mistral-medium-latest");
}

#[test]
fn opencode_go_resolves_model_aware_route() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    for alias in ["opencode-go", "opencode_go", "opencodego"] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::OpencodeGo));

        let parsed: ConfigToml =
            toml::from_str(&format!("provider = \"{alias}\"")).expect("OpenCode Go alias");
        assert_eq!(parsed.provider, ProviderKind::OpencodeGo);
    }

    let metadata = provider::resolve_provider("opencode_go").expect("provider metadata");
    assert_eq!(metadata.id(), "opencode-go");
    assert_eq!(metadata.display_name(), "OpenCode Go");
    assert_eq!(metadata.provider_config_key(), "opencode_go");
    assert_eq!(metadata.default_base_url(), DEFAULT_OPENCODE_GO_BASE_URL);
    assert_eq!(metadata.default_model(), DEFAULT_OPENCODE_GO_MODEL);
    assert_eq!(metadata.env_vars(), &["OPENCODE_GO_API_KEY"]);
    assert_eq!(metadata.wire_policy(), provider::WirePolicy::ModelAware);

    let config: ConfigToml = toml::from_str(
        r#"
provider = "opencode-go"

[providers.opencode_go]
api_key = "go-config-key"
model = "opencode-go/glm-5.2"
"#,
    )
    .expect("OpenCode Go provider table");
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.provider, ProviderKind::OpencodeGo);
    assert_eq!(resolved.base_url, DEFAULT_OPENCODE_GO_BASE_URL);
    assert_eq!(resolved.model, OPENCODE_GO_GLM_5_2_MODEL);
    assert_eq!(resolved.api_key.as_deref(), Some("go-config-key"));
    assert_eq!(
        resolved.api_key_source,
        Some(RuntimeApiKeySource::ConfigFile)
    );

    // Provider-specific environment overrides remain available, but model ids
    // stay inside the documented protocol roster.
    unsafe {
        std::env::set_var("OPENCODE_GO_API_KEY", "go-env-key");
        std::env::set_var("OPENCODE_GO_MODEL", "opencode-go/mimo-v2.5-pro");
    }
    assert_eq!(
        super::env_api_key_for_provider(ProviderKind::OpencodeGo).as_deref(),
        Some("go-env-key")
    );
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.base_url, DEFAULT_OPENCODE_GO_BASE_URL);
    assert_eq!(resolved.model, OPENCODE_GO_MIMO_V2_5_PRO_MODEL);

    for model in [OPENCODE_GO_GROK_4_5_MODEL, OPENCODE_GO_KIMI_K3_MODEL] {
        assert_eq!(opencode_go_model_id(model), Some(model));
        assert_eq!(
            opencode_go_model_id(&format!("opencode-go/{model}")),
            Some(model)
        );
    }

    // A custom endpoint preserves the configured Messages model; no fallback.
    unsafe {
        std::env::set_var("OPENCODE_GO_BASE_URL", "https://go-gateway.example/v1");
        std::env::set_var("OPENCODE_GO_MODEL", "minimax-m3");
    }
    assert_eq!(opencode_go_model_id("minimax-m3"), Some("minimax-m3"));
    assert_eq!(opencode_go_model_id("qwen3.7-max"), Some("qwen3.7-max"));
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.base_url, "https://go-gateway.example/v1");
    assert_eq!(resolved.model, "minimax-m3");
    assert_eq!(resolved.api_key, None);
}

#[test]
fn telecomjs_resolves_key_scoped_chat_completions_route() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    for alias in [
        "telecomjs",
        "telecom-js",
        "telecom_js",
        "telecomjs-cn",
        "tokenhub",
    ] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::Telecomjs));

        let parsed: ConfigToml =
            toml::from_str(&format!("provider = \"{alias}\"")).expect("TelecomJS alias");
        assert_eq!(parsed.provider, ProviderKind::Telecomjs);
    }

    let metadata = provider::resolve_provider("tokenhub").expect("provider metadata");
    assert_eq!(metadata.id(), "telecomjs");
    assert_eq!(metadata.display_name(), "TelecomJS TokenHub");
    assert_eq!(metadata.provider_config_key(), "telecomjs");
    assert_eq!(metadata.default_base_url(), DEFAULT_TELECOMJS_BASE_URL);
    assert_eq!(metadata.default_model(), DEFAULT_TELECOMJS_MODEL);
    assert_eq!(metadata.env_vars(), &["TELECOMJS_API_KEY"]);
    assert_eq!(
        metadata.wire_policy(),
        provider::WirePolicy::Fixed(provider::WireFormat::ChatCompletions)
    );

    let config: ConfigToml = toml::from_str(
        r#"
provider = "telecomjs"

[providers.telecomjs]
api_key = "telecom-config-key"
model = "glm-5.2"
"#,
    )
    .expect("TelecomJS provider table");
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.provider, ProviderKind::Telecomjs);
    assert_eq!(resolved.base_url, DEFAULT_TELECOMJS_BASE_URL);
    assert_eq!(resolved.model, "glm-5.2");
    assert_eq!(resolved.api_key.as_deref(), Some("telecom-config-key"));
    assert_eq!(
        resolved.api_key_source,
        Some(RuntimeApiKeySource::ConfigFile)
    );

    unsafe {
        std::env::set_var("TELECOMJS_API_KEY", "telecom-env-key");
        std::env::set_var("TELECOMJS_MODEL", "kimi-k2.5");
    }
    assert_eq!(
        super::env_api_key_for_provider(ProviderKind::Telecomjs).as_deref(),
        Some("telecom-env-key")
    );

    let env_config = ConfigToml {
        provider: ProviderKind::Telecomjs,
        ..ConfigToml::default()
    };
    let resolved = env_config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.base_url, DEFAULT_TELECOMJS_BASE_URL);
    assert_eq!(resolved.model, "kimi-k2.5");
    assert_eq!(resolved.api_key.as_deref(), Some("telecom-env-key"));
    assert_eq!(resolved.api_key_source, Some(RuntimeApiKeySource::Env));
}

#[test]
fn edenai_resolves_named_chat_gateway_and_environment_overrides() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    for alias in ["edenai", "eden-ai", "eden_ai"] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::Edenai));
        let parsed: ConfigToml =
            toml::from_str(&format!("provider = \"{alias}\"")).expect("Eden AI alias");
        assert_eq!(parsed.provider, ProviderKind::Edenai);
    }

    let metadata = provider::resolve_provider("eden-ai").expect("Eden AI metadata");
    assert_eq!(metadata.id(), "edenai");
    assert_eq!(metadata.display_name(), "Eden AI");
    assert_eq!(metadata.provider_config_key(), "edenai");
    assert_eq!(metadata.default_base_url(), DEFAULT_EDENAI_BASE_URL);
    assert_eq!(metadata.default_model(), DEFAULT_EDENAI_MODEL);
    assert_eq!(metadata.env_vars(), &["EDENAI_API_KEY"]);
    assert_eq!(
        metadata.wire_policy(),
        provider::WirePolicy::Fixed(provider::WireFormat::ChatCompletions)
    );

    let config: ConfigToml = toml::from_str(
        r#"
provider = "edenai"

[providers.edenai]
api_key = "eden-config-key"
model = "anthropic/claude-sonnet-4-5"
"#,
    )
    .expect("Eden AI provider table");
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.provider, ProviderKind::Edenai);
    assert_eq!(resolved.base_url, DEFAULT_EDENAI_BASE_URL);
    assert_eq!(resolved.model, "anthropic/claude-sonnet-4-5");
    assert_eq!(resolved.api_key.as_deref(), Some("eden-config-key"));
    assert_eq!(
        resolved.api_key_source,
        Some(RuntimeApiKeySource::ConfigFile)
    );

    unsafe {
        std::env::set_var("EDENAI_API_KEY", "eden-env-key");
        std::env::set_var("EDENAI_BASE_URL", "https://api.eu.edenai.run/v3");
        std::env::set_var("EDENAI_MODEL", "deepseek/deepseek-v4-flash");
    }
    let env_config = ConfigToml {
        provider: ProviderKind::Edenai,
        ..ConfigToml::default()
    };
    let resolved = env_config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.base_url, "https://api.eu.edenai.run/v3");
    assert_eq!(resolved.model, "deepseek/deepseek-v4-flash");
    assert_eq!(resolved.api_key.as_deref(), Some("eden-env-key"));
    assert_eq!(resolved.api_key_source, Some(RuntimeApiKeySource::Env));
}

#[test]
fn zenmux_resolves_named_chat_gateway_and_environment_overrides() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    for alias in ["zenmux", "zen-mux", "zen_mux"] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::Zenmux));
        let parsed: ConfigToml =
            toml::from_str(&format!("provider = \"{alias}\"")).expect("ZenMux alias");
        assert_eq!(parsed.provider, ProviderKind::Zenmux);
    }

    let metadata = provider::resolve_provider("zen-mux").expect("ZenMux metadata");
    assert_eq!(metadata.id(), "zenmux");
    assert_eq!(metadata.display_name(), "ZenMux");
    assert_eq!(metadata.provider_config_key(), "zenmux");
    assert_eq!(metadata.default_base_url(), DEFAULT_ZENMUX_BASE_URL);
    assert_eq!(metadata.default_model(), DEFAULT_ZENMUX_MODEL);
    assert_eq!(metadata.env_vars(), &["ZENMUX_API_KEY"]);
    assert_eq!(
        metadata.wire_policy(),
        provider::WirePolicy::Fixed(provider::WireFormat::ChatCompletions)
    );

    let config: ConfigToml = toml::from_str(
        r#"
provider = "zenmux"

[providers.zenmux]
api_key = "zen-config-key"
model = "z-ai/glm-5.3"
"#,
    )
    .expect("ZenMux provider table");
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.provider, ProviderKind::Zenmux);
    assert_eq!(resolved.base_url, DEFAULT_ZENMUX_BASE_URL);
    assert_eq!(resolved.model, "z-ai/glm-5.3");
    assert_eq!(resolved.api_key.as_deref(), Some("zen-config-key"));
    assert_eq!(
        resolved.api_key_source,
        Some(RuntimeApiKeySource::ConfigFile)
    );

    unsafe {
        std::env::set_var("ZENMUX_API_KEY", "zen-env-key");
        std::env::set_var("ZENMUX_BASE_URL", "https://zenmux.ai/api/v1");
        std::env::set_var("ZENMUX_MODEL", "deepseek/deepseek-v4.1-flash");
    }
    let env_config = ConfigToml {
        provider: ProviderKind::Zenmux,
        ..ConfigToml::default()
    };
    let resolved = env_config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.base_url, "https://zenmux.ai/api/v1");
    assert_eq!(resolved.model, "deepseek/deepseek-v4.1-flash");
    assert_eq!(resolved.api_key.as_deref(), Some("zen-env-key"));
    assert_eq!(resolved.api_key_source, Some(RuntimeApiKeySource::Env));
}

/// CSDN 星图 (Starmap) is an OpenAI-compatible hosted platform whose default
/// route is the Coding Plan model `glm_for_coding`: aliases collapse onto one
/// catalog identity, the metadata names the official base URL and a distinct
/// `CSDN_API_KEY` slot, the wire policy is fixed on Chat Completions, and
/// env/config overrides resolve exactly like every other provider table.
#[test]
fn csdn_resolves_named_chat_provider_and_environment_overrides() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    for alias in [
        "csdn",
        "csdn-ai",
        "csdn_ai",
        "csdn-coding-plan",
        "csdn_coding_plan",
        "starmap",
    ] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::Csdn));
        let parsed: ConfigToml =
            toml::from_str(&format!("provider = \"{alias}\"")).expect("CSDN alias");
        assert_eq!(parsed.provider, ProviderKind::Csdn);
    }

    let metadata = provider::resolve_provider("starmap").expect("CSDN metadata");
    assert_eq!(metadata.id(), "csdn");
    assert_eq!(metadata.display_name(), "CSDN");
    assert_eq!(metadata.provider_config_key(), "csdn");
    assert_eq!(metadata.default_base_url(), DEFAULT_CSDN_BASE_URL);
    assert_eq!(metadata.default_model(), DEFAULT_CSDN_MODEL);
    assert_eq!(metadata.env_vars(), &["CSDN_API_KEY"]);
    assert_eq!(
        metadata.wire_policy(),
        provider::WirePolicy::Fixed(provider::WireFormat::ChatCompletions)
    );

    let config: ConfigToml = toml::from_str(
        r#"
provider = "csdn"

[providers.csdn]
api_key = "csdn-config-key"
model = "glm_for_coding"
"#,
    )
    .expect("CSDN provider table");
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.provider, ProviderKind::Csdn);
    assert_eq!(resolved.base_url, DEFAULT_CSDN_BASE_URL);
    assert_eq!(resolved.model, "glm_for_coding");
    assert_eq!(resolved.api_key.as_deref(), Some("csdn-config-key"));
    assert_eq!(
        resolved.api_key_source,
        Some(RuntimeApiKeySource::ConfigFile)
    );

    unsafe {
        std::env::set_var("CSDN_API_KEY", "csdn-env-key");
        std::env::set_var("CSDN_BASE_URL", "https://ai.csdn.net/api/model/v1");
        std::env::set_var("CSDN_MODEL", "deepseek-v3.2");
    }
    let env_config = ConfigToml {
        provider: ProviderKind::Csdn,
        ..ConfigToml::default()
    };
    let resolved = env_config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.base_url, "https://ai.csdn.net/api/model/v1");
    assert_eq!(resolved.model, "deepseek-v3.2");
    assert_eq!(resolved.api_key.as_deref(), Some("csdn-env-key"));
    assert_eq!(resolved.api_key_source, Some(RuntimeApiKeySource::Env));
}

/// Concentrate is an opt-in, BYOK, Responses-wire gateway: aliases collapse
/// onto one catalog identity, the metadata names the official base URL and a
/// distinct `CONCENTRATE_API_KEY` slot, the wire policy is fixed on the
/// Responses API, and env/config overrides resolve exactly like every other
/// provider table.
#[test]
fn concentrate_resolves_named_responses_gateway_and_environment_overrides() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    for alias in [
        "concentrate",
        "concentrate-ai",
        "concentrate_ai",
        "concentrateai",
    ] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::Concentrate));
        let parsed: ConfigToml =
            toml::from_str(&format!("provider = \"{alias}\"")).expect("Concentrate alias");
        assert_eq!(parsed.provider, ProviderKind::Concentrate);
    }

    let metadata = provider::resolve_provider("concentrate-ai").expect("Concentrate metadata");
    assert_eq!(metadata.id(), "concentrate");
    assert_eq!(metadata.display_name(), "Concentrate");
    assert_eq!(metadata.provider_config_key(), "concentrate");
    assert_eq!(metadata.default_base_url(), "https://api.concentrate.ai/v1");
    assert_eq!(metadata.default_base_url(), DEFAULT_CONCENTRATE_BASE_URL);
    assert_eq!(metadata.default_model(), DEFAULT_CONCENTRATE_MODEL);
    assert_eq!(metadata.env_vars(), &["CONCENTRATE_API_KEY"]);
    assert_eq!(
        metadata.wire_policy(),
        provider::WirePolicy::Fixed(provider::WireFormat::Responses)
    );
    assert_eq!(
        ProviderKind::Concentrate.secret_store_slot(),
        "concentrate",
        "the gateway key never shares a slot with another provider"
    );
    let help = metadata.credential_help();
    assert_eq!(help.acquisition, provider::CredentialAcquisition::ApiKey);
    assert_eq!(
        help.docs_url,
        Some("https://concentrate.ai/docs/api-reference/introduction")
    );

    let config: ConfigToml = toml::from_str(
        r#"
provider = "concentrate"

[providers.concentrate]
api_key = "concentrate-config-key"
model = "openai/gpt-5.6-sol"
"#,
    )
    .expect("Concentrate provider table");
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.provider, ProviderKind::Concentrate);
    assert_eq!(resolved.base_url, DEFAULT_CONCENTRATE_BASE_URL);
    let alias_table: ConfigToml = toml::from_str(
        r#"
provider = "concentrate"

[providers.concentrateai]
api_key = "concentrate-alias-table-key"
model = "openai/gpt-5.6-sol"
"#,
    )
    .expect("[providers.concentrateai] must deserialize onto the concentrate table");
    let alias_resolved = alias_table.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(alias_resolved.provider, ProviderKind::Concentrate);
    assert_eq!(
        alias_resolved.api_key.as_deref(),
        Some("concentrate-alias-table-key"),
        "[providers.concentrateai] must not be silently ignored"
    );
    assert_eq!(
        resolved.model, "openai/gpt-5.6-sol",
        "provider/model ids pass through verbatim"
    );
    assert_eq!(resolved.api_key.as_deref(), Some("concentrate-config-key"));
    assert_eq!(
        resolved.api_key_source,
        Some(RuntimeApiKeySource::ConfigFile)
    );

    unsafe {
        std::env::set_var("CONCENTRATE_API_KEY", "concentrate-env-key");
        std::env::set_var("CONCENTRATE_MODEL", "claude-fable-5");
    }
    let env_config = ConfigToml {
        provider: ProviderKind::Concentrate,
        ..ConfigToml::default()
    };
    let resolved = env_config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.base_url, DEFAULT_CONCENTRATE_BASE_URL);
    assert_eq!(resolved.model, "claude-fable-5");
    assert_eq!(resolved.api_key.as_deref(), Some("concentrate-env-key"));
    assert_eq!(resolved.api_key_source, Some(RuntimeApiKeySource::Env));

    // Credential scope: a saved or environment Concentrate key is bound to the
    // official gateway. Pointing the same identity at any other base URL is
    // an arbitrary endpoint, and the key must NOT follow it (a local stub or
    // proxy gets the key only when the user writes both into the config).
    unsafe {
        std::env::set_var("CONCENTRATE_BASE_URL", "http://127.0.0.1:8790/v1");
    }
    let resolved = env_config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.base_url, "http://127.0.0.1:8790/v1");
    assert_eq!(
        resolved.api_key, None,
        "an env key must never be sent to a non-official Concentrate base URL"
    );
    unsafe {
        std::env::remove_var("CONCENTRATE_BASE_URL");
        std::env::remove_var("CONCENTRATE_MODEL");
        std::env::remove_var("CONCENTRATE_API_KEY");
    }
    let custom: ConfigToml = toml::from_str(
        r#"
provider = "concentrate"

[providers.concentrate]
base_url = "http://127.0.0.1:8790/v1"
api_key = "stub-key"
model = "concentrate/auto"
"#,
    )
    .expect("Concentrate custom endpoint table");
    let resolved = custom.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.base_url, "http://127.0.0.1:8790/v1");
    assert_eq!(resolved.api_key.as_deref(), Some("stub-key"));
    assert_eq!(
        resolved.api_key_source,
        Some(RuntimeApiKeySource::ConfigFile)
    );
    assert_eq!(resolved.model, "concentrate/auto");
}

#[test]
fn opencode_zen_configures_model_aware_provider_with_catalog_proof() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    for alias in [
        "opencode-zen",
        "opencode_zen",
        "opencodezen",
        "zen",
        "opencode",
    ] {
        assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::OpencodeZen));
    }

    let metadata = provider::resolve_provider("opencode_zen").expect("Zen provider metadata");
    assert_eq!(metadata.id(), "opencode-zen");
    assert_eq!(metadata.display_name(), "OpenCode Zen");
    assert_eq!(metadata.provider_config_key(), "opencode_zen");
    assert_eq!(metadata.default_base_url(), DEFAULT_OPENCODE_ZEN_BASE_URL);
    assert_eq!(metadata.default_model(), DEFAULT_OPENCODE_ZEN_MODEL);
    assert_eq!(
        metadata.env_vars(),
        &["OPENCODE_ZEN_API_KEY", "OPENCODE_API_KEY"]
    );
    assert_eq!(metadata.wire_policy(), provider::WirePolicy::ModelAware);

    let config: ConfigToml = toml::from_str(
        r#"
provider = "opencode-zen"

[providers.opencode_zen]
api_key = "zen-config-key"
base_url = "https://zen-gateway.example/v1"
model = "gpt-5.5"
"#,
    )
    .expect("OpenCode Zen provider table");
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.provider, ProviderKind::OpencodeZen);
    assert_eq!(resolved.base_url, "https://zen-gateway.example/v1");
    assert_eq!(resolved.model, "gpt-5.5");
    assert_eq!(resolved.api_key.as_deref(), Some("zen-config-key"));

    let resolver = crate::route::RouteResolver::new();
    let route = resolver
        .resolve(&crate::route::RouteRequest {
            explicit_provider: Some(ProviderKind::OpencodeZen),
            model_selector: Some(crate::route::LogicalModelRef::from("gpt-5.5")),
            saved_provider_model: None,
            base_url_override: None,
            limit_overrides: Vec::new(),
            wire_override: None,
        })
        .expect("documented Zen model must resolve");
    assert_eq!(route.protocol(), crate::route::RequestProtocol::Responses);
    assert_eq!(route.endpoint().endpoint_key, "responses");
}

#[test]
fn meta_model_api_scopes_both_documented_key_names_to_official_endpoint() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    let config = ConfigToml {
        provider: ProviderKind::Meta,
        ..ConfigToml::default()
    };
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Meta);
    assert_eq!(resolved.base_url, DEFAULT_META_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_META_MODEL);
    assert_eq!(resolved.api_key, None);

    unsafe {
        std::env::set_var("MODEL_API_KEY", "meta-official-key");
        std::env::set_var("MODEL_API_BASE_URL", "https://meta-gateway.example/v1");
        std::env::set_var("MODEL_API_MODEL", "muse-spark-canary");
    }
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
    assert_eq!(resolved.base_url, "https://meta-gateway.example/v1");
    assert_eq!(resolved.model, "muse-spark-canary");

    unsafe {
        std::env::set_var("META_MODEL_API_KEY", "meta-models-dev-key");
        std::env::set_var("META_MODEL_API_BASE_URL", "https://meta-primary.example/v1");
        std::env::set_var("META_MODEL_API_MODEL", "muse-spark-1.1");
    }
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
    assert_eq!(resolved.base_url, "https://meta-primary.example/v1");
    assert_eq!(resolved.model, "muse-spark-1.1");
}

#[test]
fn provider_metadata_registry_covers_every_provider_kind_once() {
    let providers = provider::all_providers();
    // Full registry keeps legacy dialect/plan kinds for provider_for_kind.
    assert_eq!(providers.len(), 52);
    // Catalog surface is one identity per vendor (no dual-wire / plan rows),
    // and never a retired tombstone: Antigravity stays in the full registry
    // so old config parses and can be cleared, but it left `ALL` when it
    // stopped being selectable (PRD §4.4 PROD-002).
    assert_eq!(ProviderKind::ALL.len(), 46);
    assert!(
        !ProviderKind::ALL.contains(&ProviderKind::Antigravity),
        "a tombstone must never be offered as a selectable provider"
    );
    assert!(ProviderKind::ALL.len() < providers.len());

    let mut ids = std::collections::BTreeSet::new();
    for provider in providers {
        assert!(ids.insert(provider.id()), "duplicate provider id");
        assert_eq!(provider.id(), provider.kind().as_str());
        assert_eq!(provider.kind().provider().id(), provider.id());
    }
    // Catalog entries are a subset of the full registry.
    for kind in ProviderKind::ALL {
        assert!(
            providers.iter().any(|p| p.kind() == kind),
            "catalog kind {kind:?} missing from full registry"
        );
    }
}

#[test]
fn provider_metadata_lookup_does_not_fall_back_to_deepseek() {
    assert!(provider::lookup_provider("not-a-provider").is_none());
    assert!(provider::resolve_provider("not-a-provider").is_none());
    assert!(provider::lookup_provider("deepseek-cn").is_none());
    assert_eq!(
        provider::resolve_provider("deepseek-cn")
            .expect("legacy alias resolves")
            .kind(),
        ProviderKind::Deepseek
    );
}

#[test]
fn provider_metadata_preserves_alias_and_config_key_semantics() {
    assert_eq!(
        provider::resolve_provider("open_router")
            .expect("openrouter alias")
            .kind(),
        ProviderKind::Openrouter
    );
    assert_eq!(
        provider::resolve_provider("xiaomi")
            .expect("xiaomi alias")
            .kind(),
        ProviderKind::XiaomiMimo
    );
    assert_eq!(
        provider::resolve_provider("kimi")
            .expect("kimi alias")
            .kind(),
        ProviderKind::Moonshot
    );
    assert_eq!(
        provider::resolve_provider("hf")
            .expect("huggingface alias")
            .kind(),
        ProviderKind::Huggingface
    );
    assert_eq!(
        provider::resolve_provider("grok")
            .expect("xAI grok alias")
            .kind(),
        ProviderKind::Xai
    );
    assert_eq!(
        provider::resolve_provider("muse-spark")
            .expect("Meta Muse Spark alias")
            .kind(),
        ProviderKind::Meta
    );

    let siliconflow_cn =
        provider::resolve_provider("siliconflow-cn").expect("siliconflow-cn alias resolves");
    assert_eq!(siliconflow_cn.kind(), ProviderKind::SiliconflowCN);
    assert_eq!(siliconflow_cn.id(), "siliconflow-CN");
    assert_eq!(siliconflow_cn.provider_config_key(), "siliconflow_cn");

    let config = ProvidersToml::default();
    let shared_table = config.for_provider(ProviderKind::SiliconflowCN);
    assert!(!std::ptr::eq(
        shared_table,
        config.for_provider(ProviderKind::Siliconflow)
    ));
}

#[test]
fn provider_metadata_defaults_match_runtime_helpers() {
    for kind in ProviderKind::ALL {
        let provider = kind.provider();
        assert_eq!(provider.default_model(), default_model_for_provider(kind));
        assert_eq!(
            provider.default_base_url(),
            default_base_url_for_provider(kind)
        );
        assert!(!provider.display_name().trim().is_empty());
        // The dynamic custom provider (#1519) intentionally declares no
        // built-in auth env var: the key env var name is supplied per entry via
        // `[providers.<name>] api_key_env = "..."`. Every built-in provider
        // still must declare at least one.
        if kind != ProviderKind::Custom {
            assert!(!provider.env_vars().is_empty());
        }
        // OpenAI Codex (ChatGPT) speaks the Responses API; DeepSeek,
        // OpenCode Zen, OpenCode Go, and the Codewhale API select a protocol per exact
        // model offering; Anthropic
        // and the Anthropic-compatible routes speak native Messages; every
        // other built-in provider is OpenAI-compatible Chat Completions.
        let expected_wire = match kind {
            ProviderKind::Deepseek
            | ProviderKind::OpencodeZen
            | ProviderKind::OpencodeGo
            | ProviderKind::Codewhale => None,
            ProviderKind::OpenaiCodex | ProviderKind::Concentrate => {
                Some(provider::WireFormat::Responses)
            }
            ProviderKind::Anthropic
            | ProviderKind::DeepseekAnthropic
            | ProviderKind::MinimaxAnthropic
            | ProviderKind::Openmodel
            | ProviderKind::ModelstudioTokenPlanAnthropic
            | ProviderKind::ModelstudioCodingPlanAnthropic => {
                Some(provider::WireFormat::AnthropicMessages)
            }
            _ => Some(provider::WireFormat::ChatCompletions),
        };
        assert_eq!(provider.wire_policy().fixed(), expected_wire);
    }
}

#[test]
fn openrouter_provider_defaults_to_canonical_endpoint_and_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::Openrouter,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Openrouter);
    assert_eq!(resolved.base_url, DEFAULT_OPENROUTER_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_OPENROUTER_MODEL);
}

#[test]
fn orcarouter_provider_defaults_to_canonical_endpoint_and_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::Orcarouter,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Orcarouter);
    assert_eq!(resolved.base_url, DEFAULT_ORCAROUTER_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_ORCAROUTER_MODEL);
}

#[test]
fn orcarouter_provider_normalizes_deepseek_aliases() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::Orcarouter,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides {
        model: Some("deepseek-v4-flash".to_string()),
        ..CliRuntimeOverrides::default()
    });

    assert_eq!(resolved.provider, ProviderKind::Orcarouter);
    assert_eq!(resolved.model, DEFAULT_ORCAROUTER_FLASH_MODEL);
}

#[test]
fn xiaomi_mimo_provider_defaults_to_canonical_endpoint_and_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::XiaomiMimo,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::XiaomiMimo);
    assert_eq!(resolved.base_url, DEFAULT_XIAOMI_MIMO_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_XIAOMI_MIMO_MODEL);
}

#[test]
fn xiaomi_provider_alias_table_maps_to_mimo_runtime_config() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config: ConfigToml = toml::from_str(
        r#"
provider = "xiaomi-mimo"
default_text_model = "deepseek/deepseek-v4-pro"

[providers.xiaomi]
api_key = "mimo-table-key"
base_url = "https://token-plan-sgp.xiaomimimo.com/v1"
model = "mimo-v2.5-pro"
"#,
    )
    .expect("xiaomi provider alias config");

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::XiaomiMimo);
    assert_eq!(resolved.api_key.as_deref(), Some("mimo-table-key"));
    assert_eq!(
        resolved.base_url,
        "https://token-plan-sgp.xiaomimimo.com/v1"
    );
    assert_eq!(resolved.model, DEFAULT_XIAOMI_MIMO_MODEL);
}

#[test]
fn xiaomi_token_plan_key_rewrites_saved_pay_as_you_go_base_url() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config: ConfigToml = toml::from_str(
        r#"
provider = "xiaomi-mimo"

[providers.xiaomi_mimo]
api_key = "tp-test-token-plan-key"
base_url = "https://api.xiaomimimo.com/v1"
model = "mimo-v2.5-pro"
"#,
    )
    .expect("xiaomi token-plan config");

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::XiaomiMimo);
    assert_eq!(resolved.base_url, DEFAULT_XIAOMI_MIMO_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_XIAOMI_MIMO_MODEL);
}

#[test]
fn xiaomi_mimo_token_plan_mode_accepts_region_aliases() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config: ConfigToml = toml::from_str(
        r#"
provider = "mimo"

[providers.mimo]
mode = "token-plan-ams"
"#,
    )
    .expect("xiaomi token-plan region config");

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::XiaomiMimo);
    assert_eq!(resolved.base_url, XIAOMI_MIMO_TOKEN_PLAN_AMS_BASE_URL);
}

#[test]
fn xiaomi_mimo_unknown_mode_stays_on_token_plan_endpoint() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config: ConfigToml = toml::from_str(
        r#"
provider = "mimo"

[providers.mimo]
mode = "token-plan-usa"
"#,
    )
    .expect("xiaomi token-plan unknown mode config");

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::XiaomiMimo);
    assert_eq!(resolved.base_url, DEFAULT_XIAOMI_MIMO_BASE_URL);
}

#[test]
fn xiaomi_mimo_aliases_resolve_to_canonical_models() {
    assert_eq!(
        normalize_model_for_provider(ProviderKind::XiaomiMimo, "omni"),
        "mimo-v2.6-flash"
    );
    assert_eq!(
        normalize_model_for_provider(ProviderKind::XiaomiMimo, "pro-ultraspeed"),
        "mimo-v2.5-pro-ultraspeed"
    );
    assert_eq!(
        normalize_model_for_provider(ProviderKind::XiaomiMimo, "tts"),
        "mimo-v2.5-tts"
    );
    assert_eq!(
        normalize_model_for_provider(ProviderKind::XiaomiMimo, "voice-design"),
        "mimo-v2.5-tts-voicedesign"
    );
    assert_eq!(
        normalize_model_for_provider(ProviderKind::XiaomiMimo, "voiceclone"),
        "mimo-v2.5-tts-voiceclone"
    );
    assert_eq!(
        normalize_model_for_provider(ProviderKind::XiaomiMimo, "custom-mimo-model"),
        "custom-mimo-model"
    );
}

#[test]
fn zai_aliases_resolve_to_canonical_models() {
    // GLM-5.3 is the default; the glm-5.1 alias must still resolve to 5.1
    // (not to the default), and GLM-5-Turbo resolves to its own id.
    assert_eq!(
        normalize_model_for_provider(ProviderKind::Zai, "glm-5.1"),
        ZAI_GLM_5_1_MODEL
    );
    assert_eq!(DEFAULT_ZAI_MODEL, "GLM-5.3");
    assert_eq!(DEFAULT_ZAI_MODEL, ZAI_GLM_5_3_MODEL);
    for alias in ["glm-5.3", "glm-5-3", "zai-glm-5.3", "zai-glm-5-3"] {
        assert_eq!(
            normalize_model_for_provider(ProviderKind::Zai, alias),
            ZAI_GLM_5_3_MODEL,
            "{alias} must canonicalize to GLM-5.3"
        );
    }
    // GLM-5.2 is a peer, no longer the default: an explicit 5.2 selection
    // must keep its own id and must never fold into DEFAULT_ZAI_MODEL.
    for alias in ["glm-5.2", "glm-5-2", "zai-glm-5.2", "zai-glm-5-2"] {
        assert_eq!(
            normalize_model_for_provider(ProviderKind::Zai, alias),
            ZAI_GLM_5_2_MODEL,
            "{alias} must canonicalize to GLM-5.2"
        );
        assert_ne!(
            normalize_model_for_provider(ProviderKind::Zai, alias),
            DEFAULT_ZAI_MODEL,
            "{alias} must not resolve to the Z.ai default"
        );
    }
    for alias in [
        "glm-5.3-flash",
        "glm-5-3-flash",
        "zai-glm-5.3-flash",
        "GLM-5.3-Flash",
    ] {
        assert_eq!(
            normalize_model_for_provider(ProviderKind::Zai, alias),
            ZAI_GLM_5_3_FLASH_MODEL,
            "{alias} must canonicalize to GLM-5.3-Flash"
        );
        assert_ne!(
            normalize_model_for_provider(ProviderKind::Zai, alias),
            ZAI_GLM_5_3_MODEL,
            "{alias} must not collapse onto GLM-5.3"
        );
    }
    assert_eq!(
        normalize_model_for_provider(ProviderKind::Zai, "glm-5-turbo"),
        ZAI_GLM_5_TURBO_MODEL
    );
    assert_eq!(
        normalize_model_for_provider(ProviderKind::Zai, "custom-glm-preview"),
        "custom-glm-preview"
    );
}

#[test]
fn zhipu_aliases_fold_into_zai_provider() {
    // Zhipu AI and Z.ai are the same vendor; `zhipu`/`zhipuai`/`bigmodel`
    // resolve to the single Zai provider rather than a separate one.
    assert_eq!(ProviderKind::parse("zhipu"), Some(ProviderKind::Zai));
    assert_eq!(ProviderKind::parse("zhipuai"), Some(ProviderKind::Zai));
    assert_eq!(ProviderKind::parse("bigmodel"), Some(ProviderKind::Zai));
    assert_eq!(ProviderKind::parse("big-model"), Some(ProviderKind::Zai));

    // A `[providers.zhipu]` table (BigModel China endpoint) merges into the Zai
    // provider config through the serde alias.
    let parsed: ConfigToml = toml::from_str(
        r#"
        [providers.zhipu]
        api_key = "$ZHIPU_API_KEY"
        base_url = "https://open.bigmodel.cn/api/paas/v4/"
        model = "glm-5-2"
        "#,
    )
    .expect("zhipu provider table parses");

    let provider = parsed.providers.for_provider(ProviderKind::Zai);
    assert_eq!(provider.api_key.as_deref(), Some("$ZHIPU_API_KEY"));
    assert_eq!(
        provider.base_url.as_deref(),
        Some("https://open.bigmodel.cn/api/paas/v4/")
    );
    assert_eq!(provider.model.as_deref(), Some("glm-5-2"));

    // GLM aliases canonicalize under the Zai umbrella, to their own ids.
    assert_eq!(
        normalize_model_for_provider(ProviderKind::Zai, "glm-5-2"),
        ZAI_GLM_5_2_MODEL
    );
}

#[test]
fn stepfun_step_plan_hosts_are_official_so_the_catalog_is_not_withheld() {
    // A Step Plan subscriber's base URL is StepFun's own documented host.
    // Treating it as custom made `catalog_models_for_route` return nothing,
    // which the picker showed as `0 bundled`: no model list, and a guessed
    // context window instead of the 1M `step-5-preview` actually has.
    for official in [
        "https://api.stepfun.ai/v1",
        "https://api.stepfun.ai/step_plan/v1",
        "https://api.stepfun.com/v1",
        "https://api.stepfun.com/step_plan/v1",
        "https://api.stepfun.ai/step_plan/v1/",
    ] {
        assert!(
            provider_base_url_is_official(ProviderKind::Stepfun, official),
            "{official} is a StepFun-owned endpoint"
        );
    }
    // A host StepFun does not own stays custom: this predicate also scopes
    // credentials, so it must not widen to arbitrary look-alikes.
    for foreign in [
        "https://api.stepfun.evil.com/v1",
        "https://api.deepseek.com",
        "https://stepfun.ai.attacker.test/step_plan/v1",
    ] {
        assert!(
            !provider_base_url_is_official(ProviderKind::Stepfun, foreign),
            "{foreign} must stay custom and keyless"
        );
    }
}

#[test]
fn zai_official_endpoint_family_includes_zhipu_general_api() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    for base_url in [
        "https://api.z.ai/api/coding/paas/v4",
        "https://api.z.ai/api/paas/v4/",
        "https://open.bigmodel.cn/api/paas/v4",
    ] {
        assert!(provider_base_url_is_official(ProviderKind::Zai, base_url));
        assert!(!provider_preserves_custom_base_url_model(
            ProviderKind::Zai,
            base_url
        ));
    }
    assert!(!provider_base_url_is_official(
        ProviderKind::Zai,
        "https://open.bigmodel.cn/api/paas/v4/preview"
    ));
}

#[test]
fn novita_provider_defaults_to_canonical_endpoint_and_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::Novita,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Novita);
    assert_eq!(resolved.base_url, DEFAULT_NOVITA_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_NOVITA_MODEL);
}

#[test]
fn fireworks_provider_defaults_to_canonical_endpoint_and_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::Fireworks,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Fireworks);
    assert_eq!(resolved.base_url, DEFAULT_FIREWORKS_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_FIREWORKS_MODEL);
}

#[test]
fn siliconflow_provider_defaults_to_canonical_endpoint_and_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::Siliconflow,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Siliconflow);
    assert_eq!(resolved.base_url, DEFAULT_SILICONFLOW_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_SILICONFLOW_MODEL);
}

#[test]
fn siliconflow_cn_config_falls_back_to_shared_table_when_unset() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        provider: ProviderKind::SiliconflowCN,
        ..ConfigToml::default()
    };
    config.providers.siliconflow.api_key = Some("sf-shared-key".to_string());
    config.providers.siliconflow.base_url = Some(DEFAULT_SILICONFLOW_BASE_URL.to_string());
    config.providers.siliconflow.model = Some("deepseek-chat".to_string());
    config.providers.siliconflow_cn.base_url = Some(DEFAULT_SILICONFLOW_CN_BASE_URL.to_string());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::SiliconflowCN);
    assert_eq!(resolved.api_key.as_deref(), Some("sf-shared-key"));
    assert_eq!(resolved.base_url, DEFAULT_SILICONFLOW_CN_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_SILICONFLOW_FLASH_MODEL);
}

#[test]
fn siliconflow_cn_first_class_config_preserves_provider_scoped_route() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        provider: ProviderKind::SiliconflowCN,
        ..ConfigToml::default()
    };
    config.providers.siliconflow_cn.api_key = Some("sf-cn-file-key".to_string());
    config.providers.siliconflow_cn.base_url = Some(DEFAULT_SILICONFLOW_CN_BASE_URL.to_string());
    config.providers.siliconflow_cn.model = Some(DEFAULT_SILICONFLOW_MODEL.to_string());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::SiliconflowCN);
    assert_eq!(resolved.api_key.as_deref(), Some("sf-cn-file-key"));
    assert_eq!(resolved.base_url, DEFAULT_SILICONFLOW_CN_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_SILICONFLOW_MODEL);
}

#[test]
fn moonshot_provider_defaults_to_kimi_k27_code() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::Moonshot,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Moonshot);
    assert_eq!(resolved.base_url, DEFAULT_MOONSHOT_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_MOONSHOT_MODEL);
}

#[test]
fn zai_stepfun_minimax_and_sakana_default_to_first_party_routes() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    for (provider, expected_base_url, expected_model) in [
        (ProviderKind::Zai, DEFAULT_ZAI_BASE_URL, DEFAULT_ZAI_MODEL),
        (
            ProviderKind::Stepfun,
            DEFAULT_STEPFUN_BASE_URL,
            DEFAULT_STEPFUN_MODEL,
        ),
        (
            ProviderKind::Minimax,
            DEFAULT_MINIMAX_BASE_URL,
            DEFAULT_MINIMAX_MODEL,
        ),
        (
            ProviderKind::MinimaxAnthropic,
            DEFAULT_MINIMAX_ANTHROPIC_BASE_URL,
            DEFAULT_MINIMAX_MODEL,
        ),
        (
            ProviderKind::Sakana,
            DEFAULT_SAKANA_BASE_URL,
            DEFAULT_SAKANA_MODEL,
        ),
    ] {
        let config = ConfigToml {
            provider,
            ..ConfigToml::default()
        };
        let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

        assert_eq!(resolved.provider, provider);
        assert_eq!(resolved.base_url, expected_base_url);
        assert_eq!(resolved.model, expected_model);
    }
}

#[test]
fn qianfan_provider_defaults_to_openai_compatible_endpoint_and_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::Qianfan,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Qianfan);
    assert_eq!(resolved.base_url, DEFAULT_QIANFAN_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_QIANFAN_MODEL);
}

#[test]
fn qianfan_provider_preserves_configured_base_url_and_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        provider: ProviderKind::Qianfan,
        ..ConfigToml::default()
    };
    config.providers.qianfan.api_key = Some("qianfan-table-key".to_string());
    config.providers.qianfan.base_url = Some("https://qianfan.baidubce.com/v2".to_string());
    config.providers.qianfan.model = Some("custom-qianfan-service-id".to_string());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Qianfan);
    assert_eq!(resolved.api_key.as_deref(), Some("qianfan-table-key"));
    assert_eq!(resolved.base_url, "https://qianfan.baidubce.com/v2");
    assert_eq!(resolved.model, "custom-qianfan-service-id");
}

#[test]
fn first_party_provider_env_model_overrides_pass_through() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "minimax");
        env::set_var("MINIMAX_MODEL", "MiniMax-M2.7-highspeed");
        env::set_var("MINIMAX_BASE_URL", "https://minimax.example/v1");
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Minimax);
    assert_eq!(resolved.base_url, "https://minimax.example/v1");
    assert_eq!(resolved.model, "MiniMax-M2.7-highspeed");
}

#[test]
fn minimax_env_model_override_canonicalizes_known_aliases() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "minimax");
        env::set_var("MINIMAX_MODEL", "minimax-m2-5-highspeed");
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Minimax);
    assert_eq!(resolved.model, "MiniMax-M2.5-highspeed");
}

#[test]
fn minimax_wire_anthropic_selects_messages_endpoint() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // minimax-anthropic is an alias of MiniMax; dialect is wire config.
    assert_eq!(
        ProviderKind::parse("minimax-anthropic"),
        Some(ProviderKind::Minimax)
    );

    let config: ConfigToml = toml::from_str(
        r#"
provider = "minimax"

[providers.minimax]
wire = "anthropic"
model = "MiniMax-M2.7"
"#,
    )
    .expect("minimax wire config");
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Minimax);
    assert_eq!(resolved.base_url, DEFAULT_MINIMAX_ANTHROPIC_BASE_URL);
    assert_eq!(resolved.model, "MiniMax-M2.7");
}

#[test]
fn parse_config_identity_preserves_legacy_table_kinds() {
    // Legacy dual-wire spellings name the user's own [providers.*] table;
    // config-table identity must not collapse them onto the vendor primary
    // the way catalog `parse` does.
    assert_eq!(
        ProviderKind::parse_config_identity("deepseek-anthropic"),
        Some(ProviderKind::DeepseekAnthropic)
    );
    assert_eq!(
        ProviderKind::parse_config_identity("deepseek_anthropic"),
        Some(ProviderKind::DeepseekAnthropic)
    );
    assert_eq!(
        ProviderKind::parse_config_identity("minimax-anthropic"),
        Some(ProviderKind::MinimaxAnthropic)
    );
    assert_eq!(
        ProviderKind::parse_config_identity("MINIMAX-ANTHROPIC"),
        Some(ProviderKind::MinimaxAnthropic)
    );
    // Primary spellings and aliases resolve exactly as catalog `parse`.
    assert_eq!(
        ProviderKind::parse_config_identity("deepseek-cn"),
        Some(ProviderKind::Deepseek)
    );
    assert_eq!(
        ProviderKind::parse_config_identity("minimax"),
        Some(ProviderKind::Minimax)
    );
    assert_eq!(ProviderKind::parse_config_identity("nope"), None);
}

#[test]
fn legacy_dual_wire_toml_table_supplies_credentials_and_endpoint() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // TOML serde keeps the legacy kind, so the user's own named table is
    // read for credentials and the Messages endpoint is selected.
    let config: ConfigToml = toml::from_str(
        r#"
provider = "deepseek-anthropic"

[providers.deepseek-anthropic]
api_key = "sk-legacy-deepseek-table"
"#,
    )
    .expect("legacy deepseek-anthropic config");
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::DeepseekAnthropic);
    assert_eq!(
        resolved.api_key.as_deref(),
        Some("sk-legacy-deepseek-table")
    );
    assert_eq!(
        resolved.api_key_source,
        Some(RuntimeApiKeySource::ConfigFile)
    );
    assert_eq!(resolved.base_url, DEFAULT_DEEPSEEK_ANTHROPIC_BASE_URL);
}

#[test]
fn legacy_dual_wire_env_provider_preserves_named_table_credentials() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // CODEWHALE_PROVIDER must resolve legacy spellings to the same kind TOML
    // serde produces; catalog collapse would orphan the user's own table and
    // drop the configured key.
    for (slug, kind, table, base_url) in [
        (
            "deepseek-anthropic",
            ProviderKind::DeepseekAnthropic,
            "deepseek-anthropic",
            DEFAULT_DEEPSEEK_ANTHROPIC_BASE_URL,
        ),
        (
            "minimax-anthropic",
            ProviderKind::MinimaxAnthropic,
            "minimax-anthropic",
            DEFAULT_MINIMAX_ANTHROPIC_BASE_URL,
        ),
        (
            "minimax_anthropic",
            ProviderKind::MinimaxAnthropic,
            "minimax-anthropic",
            DEFAULT_MINIMAX_ANTHROPIC_BASE_URL,
        ),
    ] {
        unsafe {
            env::set_var("CODEWHALE_PROVIDER", slug);
        }
        let config: ConfigToml = toml::from_str(&format!(
            "[providers.{table}]\napi_key = \"sk-legacy-{table}\"\n"
        ))
        .expect("legacy table config");
        let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

        assert_eq!(resolved.provider, kind, "env slug {slug}");
        assert_eq!(
            resolved.api_key.as_deref(),
            Some(format!("sk-legacy-{table}").as_str()),
            "env slug {slug} must read the user's own named table"
        );
        assert_eq!(
            resolved.api_key_source,
            Some(RuntimeApiKeySource::ConfigFile)
        );
        assert_eq!(resolved.base_url, base_url, "env slug {slug}");
        unsafe {
            env::remove_var("CODEWHALE_PROVIDER");
        }
    }
}

#[test]
fn config_set_legacy_dual_wire_slug_keeps_named_table_identity() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // `config set provider <legacy-slug>` must keep the table-owning kind —
    // collapsing to the primary would rewrite the config to a provider whose
    // table the user never configured.
    let mut config = ConfigToml::default();
    config
        .set_value("provider", "minimax-anthropic")
        .expect("legacy slug provider set");
    assert_eq!(config.provider, ProviderKind::MinimaxAnthropic);

    // Flat `providers.<legacy>.<field>` keys target the legacy table, not a
    // synthetic custom provider.
    config
        .set_value("providers.minimax_anthropic.api_key", "sk-minimax-legacy")
        .expect("legacy table api_key set");
    assert_eq!(
        config.providers.minimax_anthropic.api_key.as_deref(),
        Some("sk-minimax-legacy")
    );
    assert!(config.providers.extras.is_empty());
    assert_eq!(
        config
            .get_value("providers.minimax_anthropic.api_key")
            .as_deref(),
        Some("sk-minimax-legacy")
    );
    config
        .unset_value("providers.minimax_anthropic.api_key")
        .expect("legacy table api_key unset");
    assert_eq!(config.providers.minimax_anthropic.api_key, None);
}

#[test]
fn sakana_env_overrides_resolve_fugu_route() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "sakana");
        env::set_var("SAKANA_BASE_URL", "https://sakana.example/v1");
        env::set_var("SAKANA_MODEL", "fugu-ultra-20260615");
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Sakana);
    assert_eq!(resolved.base_url, "https://sakana.example/v1");
    assert_eq!(resolved.model, "fugu-ultra-20260615");
}

#[test]
fn moonshot_provider_preserves_explicit_kimi_k26() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        provider: ProviderKind::Moonshot,
        ..ConfigToml::default()
    };
    config.providers.moonshot.model = Some("kimi-k2.6".to_string());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Moonshot);
    assert_eq!(resolved.model, MOONSHOT_KIMI_K2_6_MODEL);
}

#[test]
fn moonshot_legacy_kimi_import_uses_kimi_code_endpoint_and_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        provider: ProviderKind::Moonshot,
        ..ConfigToml::default()
    };
    config.providers.moonshot.auth_mode = Some("kimi_oauth".to_string());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Moonshot);
    assert_eq!(resolved.auth_mode.as_deref(), Some("kimi_oauth"));
    assert_eq!(resolved.base_url, DEFAULT_KIMI_CODE_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_KIMI_CODE_MODEL);
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
}

#[test]
fn moonshot_kimi_code_api_key_endpoint_defaults_to_kimi_for_coding() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        provider: ProviderKind::Moonshot,
        ..ConfigToml::default()
    };
    config.providers.moonshot.api_key = Some("kimi-code-key".to_string());
    config.providers.moonshot.base_url = Some(DEFAULT_KIMI_CODE_BASE_URL.to_string());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Moonshot);
    assert_eq!(resolved.auth_mode, None);
    assert_eq!(resolved.base_url, DEFAULT_KIMI_CODE_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_KIMI_CODE_MODEL);
    assert_eq!(resolved.api_key.as_deref(), Some("kimi-code-key"));
    assert_eq!(
        resolved.api_key_source,
        Some(RuntimeApiKeySource::ConfigFile)
    );
}

/// `CODEWHALE_PROVIDER` is the user-facing env alias for switching the
/// active provider. It must be honored by the runtime resolver and win
/// over a root `provider = "deepseek"` config entry.
#[test]
fn codewhale_provider_env_switches_active_provider() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only env mutation guarded by env_lock().
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "moonshot");
    }
    let mut config = ConfigToml {
        provider: ProviderKind::Deepseek,
        ..ConfigToml::default()
    };
    config.providers.moonshot.api_key = Some("kimi-code-key".to_string());
    config.providers.moonshot.base_url = Some(DEFAULT_KIMI_CODE_BASE_URL.to_string());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Moonshot);
    assert_eq!(
        resolved.provider_source,
        ProviderSource::Env("CODEWHALE_PROVIDER")
    );
    assert_eq!(resolved.base_url, DEFAULT_KIMI_CODE_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_KIMI_CODE_MODEL);
    assert_eq!(resolved.api_key.as_deref(), Some("kimi-code-key"));
}

/// When both `CODEWHALE_PROVIDER` and the legacy `DEEPSEEK_PROVIDER`
/// are set, the public alias wins — a user adopting `CODEWHALE_*` in a
/// fresh shell config is not tripped up by a stale legacy export still
/// living in their dotfiles.
#[test]
fn codewhale_provider_env_wins_over_deepseek_provider_env() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only env mutation guarded by env_lock().
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "moonshot");
        env::set_var("DEEPSEEK_PROVIDER", "openrouter");
    }
    let config = ConfigToml {
        provider: ProviderKind::Deepseek,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Moonshot);
    assert_eq!(
        resolved.provider_source,
        ProviderSource::Env("CODEWHALE_PROVIDER")
    );
}

#[test]
fn legacy_deepseek_provider_env_records_provider_source() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only env mutation guarded by env_lock().
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "openrouter");
    }
    let config = ConfigToml {
        provider: ProviderKind::Deepseek,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Openrouter);
    assert_eq!(
        resolved.provider_source,
        ProviderSource::Env("DEEPSEEK_PROVIDER")
    );
}

#[test]
fn cli_provider_records_provider_source() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only env mutation guarded by env_lock().
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "moonshot");
    }
    let cli = CliRuntimeOverrides {
        provider: Some(ProviderKind::Openai),
        ..CliRuntimeOverrides::default()
    };
    let config = ConfigToml {
        provider: ProviderKind::Deepseek,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&cli);

    assert_eq!(resolved.provider, ProviderKind::Openai);
    assert_eq!(resolved.provider_source, ProviderSource::Cli);
}

#[test]
fn config_provider_records_provider_source() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::Moonshot,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Moonshot);
    assert_eq!(resolved.provider_source, ProviderSource::Config);
}

/// `CODEWHALE_MODEL` is the user-facing env alias for picking a model
/// against the active provider. It must be honored by the runtime
/// resolver in place of `DEEPSEEK_MODEL`.
#[test]
fn codewhale_model_env_alias_overrides_default_for_active_provider() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only env mutation guarded by env_lock().
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "moonshot");
        env::set_var("CODEWHALE_MODEL", "custom-kimi-test-model");
    }
    let config = ConfigToml::default();

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Moonshot);
    assert_eq!(resolved.model, "custom-kimi-test-model");
}

#[test]
fn blank_codewhale_model_env_alias_does_not_override_default_for_active_provider() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only env mutation guarded by env_lock().
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "moonshot");
        env::set_var("CODEWHALE_MODEL", "   ");
    }
    let config = ConfigToml::default();

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Moonshot);
    assert_eq!(resolved.model, DEFAULT_MOONSHOT_MODEL);
}

#[test]
fn deepseek_default_text_model_legacy_alias_still_overrides_active_provider_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only env mutation guarded by env_lock().
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "moonshot");
        env::set_var("DEEPSEEK_DEFAULT_TEXT_MODEL", "legacy-env-model");
    }
    let config = ConfigToml::default();

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Moonshot);
    assert_eq!(resolved.model, "legacy-env-model");
}

#[test]
fn wanjie_ark_provider_defaults_to_openai_compatible_endpoint_and_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::WanjieArk,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::WanjieArk);
    assert_eq!(resolved.base_url, DEFAULT_WANJIE_ARK_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_WANJIE_ARK_MODEL);
}

#[test]
fn sglang_provider_defaults_to_local_endpoint_and_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::Sglang,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Sglang);
    assert_eq!(resolved.base_url, DEFAULT_SGLANG_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_SGLANG_MODEL);
}

#[test]
fn vllm_provider_defaults_to_local_endpoint_and_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::Vllm,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Vllm);
    assert_eq!(resolved.base_url, DEFAULT_VLLM_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_VLLM_MODEL);
}

#[test]
fn ollama_provider_defaults_to_local_endpoint_and_unresolved_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::Ollama,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Ollama);
    assert_eq!(resolved.base_url, DEFAULT_OLLAMA_BASE_URL);
    // Pre-refresh window: do not invent a hosted DeepSeek id. A live
    // `GET /v1/models` (same tags as `/api/tags`) must replace this marker.
    assert_eq!(resolved.model, "unknown");
    assert_eq!(resolved.model, DEFAULT_OLLAMA_MODEL);
    assert_eq!(resolved.api_key, None);
}

#[test]
fn ollama_cloud_endpoint_is_official_but_neighboring_routes_are_custom() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    assert!(provider_base_url_is_official(
        ProviderKind::Ollama,
        DEFAULT_OLLAMA_BASE_URL
    ));
    for base_url in [
        provider::OLLAMA_CLOUD_BASE_URL,
        "https://ollama.com/v1/",
        "  HTTPS://OLLAMA.COM/v1/  ",
    ] {
        for provider in [ProviderKind::Ollama, ProviderKind::OllamaCloud] {
            assert!(provider_base_url_is_official(provider, base_url));
            assert!(!provider_preserves_custom_base_url_model(
                provider, base_url
            ));
        }
    }

    for base_url in [
        "http://ollama.com/v1",
        "https://ollama.com/api",
        "https://ollama.com/v1/preview",
        "https://ollama.com.evil.example/v1",
        "https://ollama-gateway.example/v1",
    ] {
        for provider in [ProviderKind::Ollama, ProviderKind::OllamaCloud] {
            assert!(!provider_base_url_is_official(provider, base_url));
            assert!(provider_preserves_custom_base_url_model(provider, base_url));
        }
    }
}

#[test]
fn explicit_ollama_cloud_defaults_to_hosted_route_and_is_not_keyless() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::OllamaCloud,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::OllamaCloud);
    assert_eq!(resolved.base_url, DEFAULT_OLLAMA_CLOUD_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_OLLAMA_CLOUD_MODEL);
    assert_eq!(resolved.api_key, None);
}

#[test]
fn ollama_cloud_preserves_provider_authoritative_model_ids() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    let mut configured = ConfigToml {
        provider: ProviderKind::OllamaCloud,
        ..ConfigToml::default()
    };
    configured.providers.ollama_cloud.model = Some("vendor/model:tag".to_string());
    let resolved = configured.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.model, "vendor/model:tag");

    let persisted_root = ConfigToml {
        provider: ProviderKind::OllamaCloud,
        default_text_model: Some("deepseek-v4-flash:0731".to_string()),
        ..ConfigToml::default()
    };
    let resolved = persisted_root.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.model, "deepseek-v4-flash:0731");
}

#[test]
fn ollama_cloud_env_prefers_pi_compatible_name_then_official_name() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "ollama-cloud");
        env::set_var("OLLAMA_CLOUD_API_KEY", "pi-compatible-key");
        env::set_var("OLLAMA_API_KEY", "official-fallback-key");
    }

    let preferred = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(preferred.provider, ProviderKind::OllamaCloud);
    assert_eq!(preferred.api_key.as_deref(), Some("pi-compatible-key"));
    assert_eq!(preferred.api_key_source, Some(RuntimeApiKeySource::Env));

    // Safety: same serialized test restores both values through EnvGuard.
    unsafe { env::remove_var("OLLAMA_CLOUD_API_KEY") };
    let fallback = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(fallback.api_key.as_deref(), Some("official-fallback-key"));
    assert_eq!(fallback.api_key_source, Some(RuntimeApiKeySource::Env));
}

#[test]
fn local_ollama_never_consumes_the_cloud_specific_environment_key() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe { env::set_var("OLLAMA_CLOUD_API_KEY", "must-not-reach-local-ollama") };
    let config = ConfigToml {
        provider: ProviderKind::Ollama,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Ollama);
    assert_eq!(resolved.base_url, DEFAULT_OLLAMA_BASE_URL);
    assert_eq!(resolved.api_key, None);
}

#[test]
fn exact_legacy_ollama_cloud_tuple_migrates_in_memory_without_writes() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let store = Arc::new(RecordingSecretsStore::with_entries(&[(
        "ollama",
        "legacy-cloud-key",
    )]));
    let secrets = Secrets::new(store.clone());
    let mut config = ConfigToml {
        provider: ProviderKind::Ollama,
        ..ConfigToml::default()
    };
    config.providers.ollama.base_url = Some(provider::OLLAMA_CLOUD_BASE_URL.to_string());
    config.providers.ollama.model = Some("legacy-cloud-model".to_string());
    let before = toml::to_string(&config).expect("serialize pre-migration config");

    let resolved =
        config.resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);

    assert_eq!(resolved.provider, ProviderKind::OllamaCloud);
    assert_eq!(resolved.base_url, provider::OLLAMA_CLOUD_BASE_URL);
    assert_eq!(resolved.model, "legacy-cloud-model");
    assert_eq!(resolved.api_key.as_deref(), Some("legacy-cloud-key"));
    assert_eq!(resolved.api_key_source, Some(RuntimeApiKeySource::Keyring));
    assert_eq!(
        store.gets.lock().unwrap().as_slice(),
        ["ollama-cloud", "ollama"]
    );
    assert!(store.sets.lock().unwrap().is_empty());
    assert!(store.deletes.lock().unwrap().is_empty());
    assert_eq!(
        toml::to_string(&config).expect("serialize post-migration config"),
        before,
        "runtime migration must not rewrite the parsed config"
    );
}

#[test]
fn explicit_ollama_cloud_uses_only_its_new_secret_slot() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let store = Arc::new(RecordingSecretsStore::with_entries(&[
        ("ollama-cloud", "cloud-key"),
        ("ollama", "must-not-be-consumed"),
    ]));
    let secrets = Secrets::new(store.clone());
    let mut config = ConfigToml {
        provider: ProviderKind::OllamaCloud,
        ..ConfigToml::default()
    };
    config.providers.ollama.base_url = Some(provider::OLLAMA_CLOUD_BASE_URL.to_string());

    let resolved =
        config.resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);

    assert_eq!(resolved.provider, ProviderKind::OllamaCloud);
    assert_eq!(resolved.api_key.as_deref(), Some("cloud-key"));
    assert_eq!(store.gets.lock().unwrap().as_slice(), ["ollama-cloud"]);
    assert!(store.sets.lock().unwrap().is_empty());
    assert!(store.deletes.lock().unwrap().is_empty());
}

#[test]
fn explicit_ollama_cloud_never_falls_back_to_local_secret_slot() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let store = Arc::new(RecordingSecretsStore::with_entries(&[(
        "ollama",
        "must-not-be-consumed",
    )]));
    let secrets = Secrets::new(store.clone());
    let config = ConfigToml {
        provider: ProviderKind::OllamaCloud,
        ..ConfigToml::default()
    };

    let resolved =
        config.resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);

    assert_eq!(resolved.api_key, None);
    assert_eq!(store.gets.lock().unwrap().as_slice(), ["ollama-cloud"]);
}

#[test]
fn neighboring_legacy_ollama_routes_do_not_migrate_or_probe_cloud_secrets() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    for base_url in [
        "http://ollama.com/v1",
        "https://ollama.com/api",
        "https://ollama.com/v1/preview",
        "https://ollama.com.evil.example/v1",
        "https://ollama-gateway.example/v1",
    ] {
        let store = Arc::new(RecordingSecretsStore::with_entries(&[
            ("ollama-cloud", "cloud-key"),
            ("ollama", "legacy-key"),
        ]));
        let secrets = Secrets::new(store.clone());
        let mut config = ConfigToml {
            provider: ProviderKind::Ollama,
            ..ConfigToml::default()
        };
        config.providers.ollama.base_url = Some(base_url.to_string());

        let resolved =
            config.resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);

        assert_eq!(resolved.provider, ProviderKind::Ollama, "{base_url}");
        assert_eq!(resolved.api_key, None, "{base_url}");
        assert!(store.gets.lock().unwrap().is_empty(), "{base_url}");
    }
}

#[test]
fn self_hosted_providers_do_not_probe_secret_store_by_default() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let store = Arc::new(RecordingSecretsStore::with_value("secret-store-key"));
    let secrets = Secrets::new(store.clone());

    for provider in [
        ProviderKind::Sglang,
        ProviderKind::Vllm,
        ProviderKind::Ollama,
    ] {
        let config = ConfigToml {
            provider,
            ..ConfigToml::default()
        };

        let resolved =
            config.resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);

        assert_eq!(resolved.provider, provider);
        assert_eq!(resolved.api_key, None);
    }

    assert!(
        store.gets.lock().unwrap().is_empty(),
        "self-hosted providers should not read the secret store by default"
    );
}

#[test]
fn self_hosted_api_key_auth_can_use_secret_store_when_requested() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let store = Arc::new(RecordingSecretsStore::with_value("secret-store-key"));
    let secrets = Secrets::new(store.clone());
    let config = ConfigToml {
        provider: ProviderKind::Ollama,
        auth_mode: Some("api_key".to_string()),
        ..ConfigToml::default()
    };

    let resolved =
        config.resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);

    assert_eq!(resolved.api_key.as_deref(), Some("secret-store-key"));
    assert_eq!(store.gets.lock().unwrap().as_slice(), ["ollama"]);
}

#[test]
fn moonshot_api_key_mode_can_use_secret_store_by_default() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let store = Arc::new(RecordingSecretsStore::with_value("secret-store-key"));
    let secrets = Secrets::new(store.clone());
    let config = ConfigToml {
        provider: ProviderKind::Moonshot,
        ..ConfigToml::default()
    };

    let resolved =
        config.resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);

    assert_eq!(resolved.api_key.as_deref(), Some("secret-store-key"));
    assert_eq!(resolved.api_key_source, Some(RuntimeApiKeySource::Keyring));
    assert_eq!(store.gets.lock().unwrap().as_slice(), ["moonshot"]);
}

#[test]
fn modelstudio_variants_resolve_one_shared_secret_store_slot() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let store = Arc::new(RecordingSecretsStore::with_value("secret-store-key"));
    let secrets = Secrets::new(store.clone());

    // One Alibaba Cloud Model Studio account authenticates every plan/dialect
    // variant, so all four resolve the family's single canonical slot.
    for provider in [
        ProviderKind::ModelstudioTokenPlan,
        ProviderKind::ModelstudioTokenPlanAnthropic,
        ProviderKind::ModelstudioCodingPlan,
        ProviderKind::ModelstudioCodingPlanAnthropic,
    ] {
        let config = ConfigToml {
            provider,
            ..ConfigToml::default()
        };

        let resolved =
            config.resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);

        assert_eq!(resolved.provider, provider);
        assert_eq!(resolved.api_key.as_deref(), Some("secret-store-key"));
        assert_eq!(resolved.api_key_source, Some(RuntimeApiKeySource::Keyring));
    }

    assert_eq!(
        store.gets.lock().unwrap().as_slice(),
        [
            "modelstudio-token-plan",
            "modelstudio-token-plan",
            "modelstudio-token-plan",
            "modelstudio-token-plan"
        ]
    );
}

#[test]
fn loopback_custom_deepseek_base_url_does_not_probe_secret_store_by_default() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let store = Arc::new(RecordingSecretsStore::with_value("stale-deepseek-key"));
    let secrets = Secrets::new(store.clone());
    let mut config = ConfigToml::default();
    config.providers.deepseek.base_url = Some("http://127.0.0.1:8000/v1".to_string());

    let resolved =
        config.resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);

    assert_eq!(resolved.provider, ProviderKind::Deepseek);
    assert_eq!(resolved.base_url, "http://127.0.0.1:8000/v1");
    assert_eq!(resolved.api_key, None);
    assert!(
        store.gets.lock().unwrap().is_empty(),
        "loopback custom endpoints should not read macOS Keychain or any secret store"
    );
}

#[test]
fn remote_custom_provider_endpoint_does_not_reuse_ambient_official_credentials() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only env mutation guarded by env_lock().
    unsafe { env::set_var("OPENROUTER_API_KEY", "ambient-official-key") };
    let store = Arc::new(RecordingSecretsStore::with_value("saved-official-key"));
    let secrets = Secrets::new(store.clone());
    let mut config = ConfigToml {
        provider: ProviderKind::Openrouter,
        ..ConfigToml::default()
    };
    config.providers.openrouter.base_url = Some("https://gateway.example/v1".to_string());

    let resolved =
        config.resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);

    assert_eq!(resolved.base_url, "https://gateway.example/v1");
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
    assert!(
        store.gets.lock().unwrap().is_empty(),
        "a custom endpoint must not read the provider's global secret slot"
    );
}

#[test]
fn env_custom_provider_endpoint_does_not_reuse_ambient_official_credentials() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // A provider-specific base URL and API key are independent ambient
    // exports; the resolver cannot safely assume the key belongs to a remote
    // custom gateway. Custom routes must bind their credential explicitly.
    // Safety: test-only env mutation guarded by env_lock().
    unsafe {
        env::set_var("OPENROUTER_BASE_URL", "https://gateway.example/v1");
        env::set_var("OPENROUTER_API_KEY", "ambient-official-key");
    }
    let store = Arc::new(RecordingSecretsStore::with_value("saved-official-key"));
    let secrets = Secrets::new(store.clone());
    let config = ConfigToml {
        provider: ProviderKind::Openrouter,
        ..ConfigToml::default()
    };

    let resolved =
        config.resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);

    assert_eq!(resolved.base_url, "https://gateway.example/v1");
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
    assert!(
        store.gets.lock().unwrap().is_empty(),
        "an env-supplied custom endpoint must not read the provider's global secret slot"
    );
}

#[test]
fn remote_custom_provider_endpoint_accepts_only_explicitly_bound_credentials() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only env mutation guarded by env_lock().
    unsafe { env::set_var("OPENROUTER_API_KEY", "ambient-official-key") };
    let store = Arc::new(RecordingSecretsStore::with_value("saved-official-key"));
    let secrets = Secrets::new(store.clone());
    let mut config = ConfigToml {
        provider: ProviderKind::Openrouter,
        ..ConfigToml::default()
    };
    config.providers.openrouter.base_url = Some("https://gateway.example/v1".to_string());
    config.providers.openrouter.api_key = Some("route-bound-key".to_string());

    let resolved =
        config.resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);

    assert_eq!(resolved.api_key.as_deref(), Some("route-bound-key"));
    assert_eq!(
        resolved.api_key_source,
        Some(RuntimeApiKeySource::ConfigFile)
    );
    assert!(store.gets.lock().unwrap().is_empty());
}

#[test]
fn auth_mode_none_suppresses_every_runtime_credential_source() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only env mutation guarded by env_lock().
    unsafe { env::set_var("OPENROUTER_API_KEY", "ambient-key") };
    let store = Arc::new(RecordingSecretsStore::with_value("saved-key"));
    let secrets = Secrets::new(store.clone());
    let mut config = ConfigToml {
        provider: ProviderKind::Openrouter,
        auth_mode: Some("none".to_string()),
        ..ConfigToml::default()
    };
    config.providers.openrouter.api_key = Some("configured-key".to_string());
    config.http_headers.insert(
        "aUtHoRiZaTiOn".to_string(),
        "Bearer configured-header-secret".to_string(),
    );
    config.providers.openrouter.http_headers.insert(
        "X-API-Key".to_string(),
        "configured-x-api-key-secret".to_string(),
    );
    config.providers.openrouter.http_headers.insert(
        "Api-Key".to_string(),
        "configured-api-key-secret".to_string(),
    );
    config.providers.openrouter.http_headers.extend([
        (
            "Proxy-Authorization".to_string(),
            "Basic configured-proxy-secret".to_string(),
        ),
        (
            "X-Auth-Token".to_string(),
            "configured-auth-token".to_string(),
        ),
        (
            "X-Access-Token".to_string(),
            "configured-access-token".to_string(),
        ),
        (
            "X-Goog-Api-Key".to_string(),
            "configured-google-key".to_string(),
        ),
        ("Cookie".to_string(), "session=secret".to_string()),
    ]);
    config
        .providers
        .openrouter
        .http_headers
        .insert("X-Route-Metadata".to_string(), "safe".to_string());
    let cli = CliRuntimeOverrides {
        api_key: Some("explicit-cli-key".to_string()),
        ..CliRuntimeOverrides::default()
    };

    let resolved = config.resolve_runtime_options_with_secrets(&cli, &secrets);

    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
    for name in [
        "authorization",
        "x-api-key",
        "api-key",
        "proxy-authorization",
        "x-auth-token",
        "x-access-token",
        "x-goog-api-key",
        "cookie",
    ] {
        assert!(
            !resolved
                .http_headers
                .keys()
                .any(|candidate| candidate.eq_ignore_ascii_case(name)),
            "disabled auth leaked {name}: {:?}",
            resolved.http_headers
        );
    }
    assert_eq!(
        resolved
            .http_headers
            .get("X-Route-Metadata")
            .map(String::as_str),
        Some("safe")
    );
    assert!(
        store.gets.lock().unwrap().is_empty(),
        "disabled auth must not probe any durable credential source"
    );
}

#[test]
fn deepseek_official_endpoint_family_shares_canonical_model_namespace() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    for base_url in [
        "https://api.deepseek.com",
        "https://api.deepseek.com/v1/",
        "https://api.deepseek.com/beta",
    ] {
        assert!(provider_base_url_is_official(
            ProviderKind::Deepseek,
            base_url
        ));
        assert!(!provider_preserves_custom_base_url_model(
            ProviderKind::Deepseek,
            base_url
        ));
    }
    assert!(!provider_base_url_is_official(
        ProviderKind::Deepseek,
        "https://api.deepseek.com.evil.example/v1"
    ));
}

#[test]
fn ollama_provider_preserves_model_tags() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let cli = CliRuntimeOverrides {
        provider: Some(ProviderKind::Ollama),
        model: Some("deepseek-coder-v2:16b".to_string()),
        ..CliRuntimeOverrides::default()
    };

    let resolved = ConfigToml::default().resolve_runtime_options(&cli);

    assert_eq!(resolved.provider, ProviderKind::Ollama);
    assert_eq!(resolved.model, "deepseek-coder-v2:16b");
}

#[test]
fn ollama_custom_remote_does_not_inherit_ambient_or_saved_official_key() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "ollama-local");
        env::set_var("OLLAMA_BASE_URL", "http://ollama.example/v1");
        env::set_var("OLLAMA_API_KEY", "ollama-env-key");
    }

    let store = Arc::new(RecordingSecretsStore::with_value("ollama-saved-key"));
    let secrets = Secrets::new(store.clone());

    let resolved = ConfigToml::default()
        .resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);

    assert_eq!(resolved.provider, ProviderKind::Ollama);
    assert_eq!(resolved.base_url, "http://ollama.example/v1");
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
    assert!(
        store.gets.lock().unwrap().is_empty(),
        "a custom Ollama endpoint must not read the official ollama secret slot"
    );
}

#[test]
fn openrouter_env_overrides_key_and_model_when_config_missing() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "openrouter");
        env::set_var("OPENROUTER_API_KEY", "or-env-key");
        env::set_var("OPENROUTER_MODEL", "deepseek-v4-flash");
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Openrouter);
    assert_eq!(resolved.api_key.as_deref(), Some("or-env-key"));
    assert_eq!(resolved.base_url, DEFAULT_OPENROUTER_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_OPENROUTER_FLASH_MODEL);
}

#[test]
fn xiaomi_mimo_custom_env_url_does_not_inherit_ambient_key() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "xiaomi-mimo");
        env::set_var("MIMO_API_KEY", "mimo-env-key");
        env::set_var("MIMO_BASE_URL", "https://mimo-gateway.example/v1");
        env::set_var("MIMO_MODEL", "mimo-v2.5");
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::XiaomiMimo);
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
    assert_eq!(resolved.base_url, "https://mimo-gateway.example/v1");
    assert_eq!(resolved.model, "mimo-v2.5");
}

#[test]
fn xiaomi_mimo_env_token_plan_mode_uses_token_plan_key_and_endpoint() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "xiaomi-mimo");
        env::set_var("XIAOMI_MIMO_MODE", "token-plan-cn");
        env::set_var("XIAOMI_MIMO_TOKEN_PLAN_API_KEY", "tp-env-key");
        env::set_var("XIAOMI_MIMO_API_KEY", "sk-env-key");
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::XiaomiMimo);
    assert_eq!(resolved.api_key.as_deref(), Some("tp-env-key"));
    assert_eq!(resolved.api_key_source, Some(RuntimeApiKeySource::Env));
    assert_eq!(resolved.base_url, XIAOMI_MIMO_TOKEN_PLAN_CN_BASE_URL);
}

#[test]
fn xiaomi_mimo_env_pay_as_you_go_mode_prefers_standard_key() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "xiaomi-mimo");
        env::set_var("XIAOMI_MIMO_MODE", "pay-as-you-go");
        env::set_var("XIAOMI_MIMO_TOKEN_PLAN_API_KEY", "tp-env-key");
        env::set_var("XIAOMI_MIMO_API_KEY", "sk-env-key");
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::XiaomiMimo);
    assert_eq!(resolved.api_key.as_deref(), Some("sk-env-key"));
    assert_eq!(resolved.api_key_source, Some(RuntimeApiKeySource::Env));
    assert_eq!(resolved.base_url, XIAOMI_MIMO_PAY_AS_YOU_GO_BASE_URL);
}

#[test]
fn xiaomi_mimo_pay_as_you_go_mode_never_falls_back_to_a_token_plan_key() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "xiaomi-mimo");
        env::set_var("XIAOMI_MIMO_MODE", "pay-as-you-go");
        env::set_var("MIMO_TOKEN_PLAN_API_KEY", "tp-env-key");
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::XiaomiMimo);
    assert_eq!(resolved.base_url, XIAOMI_MIMO_PAY_AS_YOU_GO_BASE_URL);
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
}

#[test]
fn novita_env_overrides_key_and_model_when_config_missing() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "novita");
        env::set_var("NOVITA_API_KEY", "novita-env-key");
        env::set_var("NOVITA_MODEL", "deepseek-v4-flash");
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Novita);
    assert_eq!(resolved.api_key.as_deref(), Some("novita-env-key"));
    assert_eq!(resolved.base_url, DEFAULT_NOVITA_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_NOVITA_FLASH_MODEL);
}

#[test]
fn fireworks_env_overrides_key_and_model_when_config_missing() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "fireworks");
        env::set_var("FIREWORKS_API_KEY", "fw-env-key");
        env::set_var(
            "FIREWORKS_MODEL",
            "accounts/fireworks/models/account-specific-model",
        );
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Fireworks);
    assert_eq!(resolved.api_key.as_deref(), Some("fw-env-key"));
    assert_eq!(resolved.base_url, DEFAULT_FIREWORKS_BASE_URL);
    assert_eq!(
        resolved.model,
        "accounts/fireworks/models/account-specific-model"
    );
}

#[test]
fn siliconflow_custom_env_url_does_not_inherit_ambient_key() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "siliconflow");
        env::set_var("SILICONFLOW_API_KEY", "sf-env-key");
        env::set_var("SILICONFLOW_BASE_URL", "https://sf-mirror.example/v1");
        env::set_var("SILICONFLOW_MODEL", "deepseek-v4-flash");
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Siliconflow);
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
    assert_eq!(resolved.base_url, "https://sf-mirror.example/v1");
    assert_eq!(resolved.model, "deepseek-v4-flash");
}

#[test]
fn arcee_provider_defaults_to_direct_api_endpoint_and_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::Arcee,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Arcee);
    assert_eq!(resolved.base_url, DEFAULT_ARCEE_BASE_URL);
    assert_eq!(resolved.model, DEFAULT_ARCEE_MODEL);
}

#[test]
fn arcee_custom_env_url_does_not_inherit_ambient_key() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "arcee");
        env::set_var("ARCEE_API_KEY", "arcee-env-key");
        env::set_var("ARCEE_BASE_URL", "https://arcee-mirror.example/api/v1");
        env::set_var("ARCEE_MODEL", "trinity-large-preview");
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Arcee);
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
    assert_eq!(resolved.base_url, "https://arcee-mirror.example/api/v1");
    assert_eq!(resolved.model, "trinity-large-preview");
}

#[test]
fn arcee_provider_config_overrides_runtime_defaults() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        provider: ProviderKind::Arcee,
        ..ConfigToml::default()
    };
    config.providers.arcee.api_key = Some("arcee-file-key".to_string());
    config.providers.arcee.base_url = Some(DEFAULT_ARCEE_BASE_URL.to_string());
    config.providers.arcee.model = Some("arcee-trinity-large-preview".to_string());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Arcee);
    assert_eq!(resolved.api_key.as_deref(), Some("arcee-file-key"));
    assert_eq!(resolved.base_url, DEFAULT_ARCEE_BASE_URL);
    assert_eq!(resolved.model, ARCEE_TRINITY_LARGE_PREVIEW_MODEL);
}

#[test]
fn huggingface_custom_env_url_does_not_inherit_documented_key_names() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "hf");
        env::set_var("HUGGINGFACE_API_KEY", "hf-full-key");
        env::set_var("HF_TOKEN", "hf-token-fallback");
        env::set_var("HUGGINGFACE_BASE_URL", "https://hf-full.example/v1");
        env::set_var("HF_BASE_URL", "https://hf-short.example/v1");
        env::set_var("HUGGINGFACE_MODEL", "org/full-model");
        env::set_var("HF_MODEL", "org/short-model");
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Huggingface);
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
    assert_eq!(resolved.base_url, "https://hf-full.example/v1");
    assert_eq!(resolved.model, "org/full-model");
}

#[test]
fn huggingface_short_custom_env_url_does_not_inherit_ambient_token() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "huggingface");
        env::set_var("HF_TOKEN", "hf-token-fallback");
        env::set_var("HF_BASE_URL", "https://hf-short.example/v1");
        env::set_var("HF_MODEL", "org/short-model");
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Huggingface);
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
    assert_eq!(resolved.base_url, "https://hf-short.example/v1");
    assert_eq!(resolved.model, "org/short-model");
}

#[test]
fn huggingface_token_fallback_resolves_when_primary_api_key_is_blank() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "huggingface");
        env::set_var("HUGGINGFACE_API_KEY", " ");
        env::set_var("HF_TOKEN", "hf-token-fallback");
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Huggingface);
    assert_eq!(resolved.api_key.as_deref(), Some("hf-token-fallback"));
}

#[test]
fn siliconflow_cn_base_url_env_normalizes_model_aliases() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "siliconflow");
        env::set_var("SILICONFLOW_API_KEY", "sf-env-key");
        env::set_var("SILICONFLOW_BASE_URL", "https://api.siliconflow.cn/v1");
    }

    for (alias, expected) in [
        ("deepseek-v4-flash", DEFAULT_SILICONFLOW_FLASH_MODEL),
        ("deepseek-reasoner", DEFAULT_SILICONFLOW_MODEL),
    ] {
        // Safety: test-only environment mutation guarded by a module mutex.
        unsafe {
            env::set_var("SILICONFLOW_MODEL", alias);
        }

        let resolved =
            ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

        assert_eq!(resolved.provider, ProviderKind::Siliconflow);
        assert_eq!(resolved.base_url, "https://api.siliconflow.cn/v1");
        assert_eq!(resolved.model, expected);
    }
}

#[test]
fn wanjie_ark_custom_env_url_does_not_inherit_ambient_key() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "wanjie-ark");
        env::set_var("WANJIE_ARK_API_KEY", "wanjie-env-key");
        env::set_var("WANJIE_ARK_BASE_URL", "https://wanjie.example/api/v1");
        env::set_var("WANJIE_ARK_MODEL", "account-model-id");
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::WanjieArk);
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
    assert_eq!(resolved.base_url, "https://wanjie.example/api/v1");
    assert_eq!(resolved.model, "account-model-id");
}

#[test]
fn volcengine_custom_env_alias_url_does_not_inherit_ambient_key() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: test-only environment mutation guarded by a module mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "volcengine");
        env::set_var("ARK_API_KEY", "volcengine-env-key");
        env::set_var("ARK_BASE_URL", "https://volcengine.example/api/coding/v3");
        env::set_var("VOLCENGINE_ARK_MODEL", "DeepSeek-V4-Flash");
    }

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Volcengine);
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.api_key_source, None);
    assert_eq!(
        resolved.base_url,
        "https://volcengine.example/api/coding/v3"
    );
    assert_eq!(resolved.model, "DeepSeek-V4-Flash");
}

#[test]
fn openrouter_provider_normalizes_flash_aliases() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let cli = CliRuntimeOverrides {
        provider: Some(ProviderKind::Openrouter),
        model: Some("deepseek-v4-flash".to_string()),
        ..CliRuntimeOverrides::default()
    };

    let resolved = ConfigToml::default().resolve_runtime_options(&cli);

    assert_eq!(resolved.provider, ProviderKind::Openrouter);
    assert_eq!(resolved.model, DEFAULT_OPENROUTER_FLASH_MODEL);
}

#[test]
fn qwen3_6_plus_resolves_to_canonical_on_openrouter() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::Openrouter,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides {
        model: Some("qwen3.6-plus".to_string()),
        ..CliRuntimeOverrides::default()
    });

    assert_eq!(resolved.provider, ProviderKind::Openrouter);
    assert_eq!(resolved.model, OPENROUTER_QWEN_3_6_PLUS_MODEL);
}

#[test]
fn qwen3_6_plus_alias_qwen_dash_resolves() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::Openrouter,
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides {
        model: Some("qwen-3.6-plus".to_string()),
        ..CliRuntimeOverrides::default()
    });

    assert_eq!(resolved.model, OPENROUTER_QWEN_3_6_PLUS_MODEL);
}

#[test]
fn openrouter_provider_normalizes_recent_large_model_aliases() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    for (alias, expected) in [
        (
            "trinity-large-thinking",
            OPENROUTER_ARCEE_TRINITY_LARGE_THINKING_MODEL,
        ),
        ("qwen3.6-flash", OPENROUTER_QWEN_3_6_FLASH_MODEL),
        ("qwen3.6-35b-a3b", OPENROUTER_QWEN_3_6_35B_A3B_MODEL),
        ("qwen3.6-max-preview", OPENROUTER_QWEN_3_6_MAX_PREVIEW_MODEL),
        ("qwen3.6-plus", OPENROUTER_QWEN_3_6_PLUS_MODEL),
        ("qwen3.7-plus", OPENROUTER_QWEN_3_7_PLUS_MODEL),
        ("qwen-3.7-plus", OPENROUTER_QWEN_3_7_PLUS_MODEL),
        ("mimo-v2.5-pro", OPENROUTER_XIAOMI_MIMO_V2_5_PRO_MODEL),
        ("kimi-k2.7-code", OPENROUTER_KIMI_K2_7_CODE_MODEL),
        ("kimi", OPENROUTER_KIMI_K2_7_CODE_MODEL),
        ("kimi-k2.6", OPENROUTER_KIMI_K2_6_MODEL),
        ("minimax-m3", OPENROUTER_MINIMAX_M3_MODEL),
        ("minimax-2.7", OPENROUTER_MINIMAX_M2_7_MODEL),
        ("gemma-4-31b-it", OPENROUTER_GEMMA_4_31B_MODEL),
        ("glm-5.1", OPENROUTER_GLM_5_1_MODEL),
        ("glm-5.2", OPENROUTER_GLM_5_2_MODEL),
        ("glm-5.3", OPENROUTER_GLM_5_3_MODEL),
        ("glm-5.3-flash", OPENROUTER_GLM_5_3_FLASH_MODEL),
    ] {
        let cli = CliRuntimeOverrides {
            provider: Some(ProviderKind::Openrouter),
            model: Some(alias.to_string()),
            ..CliRuntimeOverrides::default()
        };

        let resolved = ConfigToml::default().resolve_runtime_options(&cli);

        assert_eq!(resolved.provider, ProviderKind::Openrouter);
        assert_eq!(resolved.model, expected);
    }
}

#[test]
fn novita_provider_normalizes_flash_aliases() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let cli = CliRuntimeOverrides {
        provider: Some(ProviderKind::Novita),
        model: Some("deepseek-v4-flash".to_string()),
        ..CliRuntimeOverrides::default()
    };

    let resolved = ConfigToml::default().resolve_runtime_options(&cli);

    assert_eq!(resolved.provider, ProviderKind::Novita);
    assert_eq!(resolved.model, DEFAULT_NOVITA_FLASH_MODEL);
}

#[test]
fn siliconflow_provider_normalizes_flash_aliases() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let cli = CliRuntimeOverrides {
        provider: Some(ProviderKind::Siliconflow),
        model: Some("deepseek-v4-flash".to_string()),
        ..CliRuntimeOverrides::default()
    };

    let resolved = ConfigToml::default().resolve_runtime_options(&cli);

    assert_eq!(resolved.provider, ProviderKind::Siliconflow);
    assert_eq!(resolved.model, DEFAULT_SILICONFLOW_FLASH_MODEL);
}

#[test]
fn siliconflow_provider_normalizes_reasoning_aliases_to_pro() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    for alias in ["deepseek-reasoner", "deepseek-r1"] {
        let cli = CliRuntimeOverrides {
            provider: Some(ProviderKind::Siliconflow),
            model: Some(alias.to_string()),
            ..CliRuntimeOverrides::default()
        };

        let resolved = ConfigToml::default().resolve_runtime_options(&cli);

        assert_eq!(resolved.provider, ProviderKind::Siliconflow);
        assert_eq!(resolved.model, DEFAULT_SILICONFLOW_MODEL);
    }
}

#[test]
fn siliconflow_provider_preserves_deepseek_v3_2_alias() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let cli = CliRuntimeOverrides {
        provider: Some(ProviderKind::Siliconflow),
        model: Some("deepseek-v3.2".to_string()),
        ..CliRuntimeOverrides::default()
    };

    let resolved = ConfigToml::default().resolve_runtime_options(&cli);

    assert_eq!(resolved.provider, ProviderKind::Siliconflow);
    assert_eq!(resolved.model, "deepseek-v3.2");
}

#[test]
fn sglang_provider_normalizes_flash_aliases() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let cli = CliRuntimeOverrides {
        provider: Some(ProviderKind::Sglang),
        model: Some("deepseek-v4-flash".to_string()),
        ..CliRuntimeOverrides::default()
    };

    let resolved = ConfigToml::default().resolve_runtime_options(&cli);

    assert_eq!(resolved.provider, ProviderKind::Sglang);
    assert_eq!(resolved.model, DEFAULT_SGLANG_FLASH_MODEL);
}

#[test]
fn vllm_provider_normalizes_flash_aliases() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let cli = CliRuntimeOverrides {
        provider: Some(ProviderKind::Vllm),
        model: Some("deepseek-v4-flash".to_string()),
        ..CliRuntimeOverrides::default()
    };

    let resolved = ConfigToml::default().resolve_runtime_options(&cli);

    assert_eq!(resolved.provider, ProviderKind::Vllm);
    assert_eq!(resolved.model, DEFAULT_VLLM_FLASH_MODEL);
}

#[test]
fn openrouter_provider_specific_config_overrides_env() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        provider: ProviderKind::Openrouter,
        ..ConfigToml::default()
    };
    config.providers.openrouter.api_key = Some("file-key".to_string());
    config.providers.openrouter.base_url = Some("https://or-mirror.example/v1".to_string());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.api_key.as_deref(), Some("file-key"));
    assert_eq!(resolved.base_url, "https://or-mirror.example/v1");
}

#[test]
fn openrouter_custom_base_url_preserves_provider_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        provider: ProviderKind::Openrouter,
        ..ConfigToml::default()
    };
    config.providers.openrouter.base_url = Some("https://gateway.example.com/v1".to_string());
    config.providers.openrouter.model = Some("DeepSeek-V4-Pro".to_string());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Openrouter);
    assert_eq!(resolved.base_url, "https://gateway.example.com/v1");
    assert_eq!(resolved.model, "DeepSeek-V4-Pro");
}

#[test]
fn openai_compatible_tokenhub_route_preserves_provider_scope() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        provider: ProviderKind::Openai,
        ..ConfigToml::default()
    };
    config.providers.openai.api_key = Some("tokenhub-file-key".to_string());
    config.providers.openai.base_url = Some("https://tokenhub.tencentmaas.com/v1".to_string());
    config.providers.openai.model = Some("deepseek-ai/DeepSeek-V4-Pro".to_string());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Openai);
    assert_eq!(resolved.api_key.as_deref(), Some("tokenhub-file-key"));
    assert_eq!(resolved.base_url, "https://tokenhub.tencentmaas.com/v1");
    assert_eq!(resolved.model, "deepseek-ai/DeepSeek-V4-Pro");
}

#[test]
fn openrouter_compatible_base_url_preserves_namespaced_wire_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        provider: ProviderKind::Openrouter,
        ..ConfigToml::default()
    };
    config.providers.openrouter.base_url = Some("https://openrouter-compatible.example/v1".into());
    config.providers.openrouter.model = Some("deepseek/deepseek-v4-pro".into());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Openrouter);
    assert_eq!(resolved.provider_source, ProviderSource::Config);
    assert_eq!(
        resolved.base_url,
        "https://openrouter-compatible.example/v1"
    );
    assert_eq!(resolved.model, "deepseek/deepseek-v4-pro");
}

#[test]
fn fireworks_custom_base_url_preserves_provider_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        provider: ProviderKind::Fireworks,
        ..ConfigToml::default()
    };
    config.providers.fireworks.base_url = Some("https://my-gateway.example/v1".to_string());
    config.providers.fireworks.model = Some("DeepSeek-V4-Pro".to_string());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Fireworks);
    assert_eq!(resolved.base_url, "https://my-gateway.example/v1");
    // Custom base URL skips provider-specific model prefixing.
    assert_eq!(resolved.model, "DeepSeek-V4-Pro");
}

#[test]
fn siliconflow_custom_base_url_preserves_provider_model() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml {
        provider: ProviderKind::Siliconflow,
        ..ConfigToml::default()
    };
    config.providers.siliconflow.base_url = Some("https://my-gateway.example/v1".to_string());
    config.providers.siliconflow.model = Some("DeepSeek-V4-Pro".to_string());

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::Siliconflow);
    assert_eq!(resolved.base_url, "https://my-gateway.example/v1");
    assert_eq!(resolved.model, "DeepSeek-V4-Pro");
}

#[test]
fn sentinel_config_values_fall_through_without_becoming_runtime_keys() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    for sentinel in [API_KEYRING_SENTINEL, "  __KEYRING__  "] {
        let store = Arc::new(RecordingSecretsStore::with_value("stored-key"));
        let secrets = Secrets::new(store.clone());
        let mut official = ConfigToml::default();
        official.providers.deepseek.api_key = Some(sentinel.to_string());
        let resolved = official
            .resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);
        assert_eq!(resolved.api_key.as_deref(), Some("stored-key"));
        assert_eq!(resolved.api_key_source, Some(RuntimeApiKeySource::Keyring));

        let custom_store = Arc::new(RecordingSecretsStore::with_value("must-not-be-read"));
        let custom_secrets = Secrets::new(custom_store.clone());
        let mut custom = ConfigToml {
            provider: ProviderKind::Openrouter,
            ..ConfigToml::default()
        };
        custom.providers.openrouter.base_url = Some("https://gateway.example.test/v1".to_string());
        custom.providers.openrouter.api_key = Some(sentinel.to_string());
        let resolved = custom
            .resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &custom_secrets);
        assert_eq!(resolved.api_key, None);
        assert_eq!(resolved.api_key_source, None);
        assert!(custom_store.gets.lock().unwrap().is_empty());

        let empty_store = Arc::new(RecordingSecretsStore::empty());
        let empty_secrets = Secrets::new(empty_store);
        let mut xiaomi = ConfigToml {
            provider: ProviderKind::XiaomiMimo,
            ..ConfigToml::default()
        };
        xiaomi.providers.xiaomi_mimo.api_key = Some(sentinel.to_string());
        let resolved = xiaomi
            .resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &empty_secrets);
        assert_eq!(resolved.base_url, DEFAULT_XIAOMI_MIMO_BASE_URL);
        assert_eq!(resolved.api_key, None);
        assert_eq!(resolved.api_key_source, None);
    }
}

#[test]
fn config_file_resolves_above_env_and_keyring() {
    use codewhale_secrets::KeyringStore;
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: env mutation guarded by env_lock().
    unsafe { std::env::set_var("DEEPSEEK_API_KEY", "env-key") };

    let store = std::sync::Arc::new(codewhale_secrets::InMemoryKeyringStore::new());
    store.set("deepseek", "ring-key").unwrap();
    let secrets = Secrets::new(store);

    let mut config = ConfigToml::default();
    config.providers.deepseek.api_key = Some("file-key".to_string());

    let resolved =
        config.resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);
    assert_eq!(resolved.api_key.as_deref(), Some("file-key"));
    assert_eq!(
        resolved.api_key_source,
        Some(RuntimeApiKeySource::ConfigFile)
    );

    // Safety: env mutation guarded by env_lock().
    unsafe { std::env::remove_var("DEEPSEEK_API_KEY") };
}

#[test]
fn env_resolves_when_config_file_and_keyring_empty() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: env mutation guarded by env_lock().
    unsafe { std::env::set_var("DEEPSEEK_API_KEY", "env-key") };

    let secrets = Secrets::new(std::sync::Arc::new(
        codewhale_secrets::InMemoryKeyringStore::new(),
    ));
    let config = ConfigToml::default();

    let resolved =
        config.resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);
    assert_eq!(resolved.api_key.as_deref(), Some("env-key"));
    assert_eq!(resolved.api_key_source, Some(RuntimeApiKeySource::Env));

    // Safety: env mutation guarded by env_lock().
    unsafe { std::env::remove_var("DEEPSEEK_API_KEY") };
}

#[test]
fn config_file_resolves_when_keyring_and_env_empty() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    let secrets = Secrets::new(std::sync::Arc::new(
        codewhale_secrets::InMemoryKeyringStore::new(),
    ));
    let mut config = ConfigToml::default();
    config.providers.deepseek.api_key = Some("file-key".to_string());

    let resolved =
        config.resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);
    assert_eq!(resolved.api_key.as_deref(), Some("file-key"));
    assert_eq!(
        resolved.api_key_source,
        Some(RuntimeApiKeySource::ConfigFile)
    );
}

#[test]
fn keyring_resolves_when_config_file_empty_even_if_env_is_set() {
    use codewhale_secrets::KeyringStore;
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    // Safety: env mutation guarded by env_lock().
    unsafe { std::env::set_var("DEEPSEEK_API_KEY", "stale-env-key") };

    let store = std::sync::Arc::new(codewhale_secrets::InMemoryKeyringStore::new());
    store.set("deepseek", "ring-key").unwrap();
    let secrets = Secrets::new(store);

    let resolved = ConfigToml::default()
        .resolve_runtime_options_with_secrets(&CliRuntimeOverrides::default(), &secrets);
    assert_eq!(resolved.api_key.as_deref(), Some("ring-key"));
    assert_eq!(resolved.api_key_source, Some(RuntimeApiKeySource::Keyring));

    // Safety: env mutation guarded by env_lock().
    unsafe { std::env::remove_var("DEEPSEEK_API_KEY") };
}

#[test]
fn cli_flag_still_overrides_keyring() {
    use codewhale_secrets::KeyringStore;
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();

    let store = std::sync::Arc::new(codewhale_secrets::InMemoryKeyringStore::new());
    store.set("deepseek", "ring-key").unwrap();
    let secrets = Secrets::new(store);

    let cli = CliRuntimeOverrides {
        api_key: Some("cli-key".to_string()),
        ..CliRuntimeOverrides::default()
    };
    let resolved = ConfigToml::default().resolve_runtime_options_with_secrets(&cli, &secrets);
    assert_eq!(resolved.api_key.as_deref(), Some("cli-key"));
    assert_eq!(resolved.api_key_source, Some(RuntimeApiKeySource::Cli));
}

#[test]
fn provider_chain_initial_current_is_active() {
    let chain = ProviderChain::new(
        ProviderKind::NvidiaNim,
        &[ProviderKind::Deepseek, ProviderKind::Openrouter],
    );

    assert_eq!(chain.current(), ProviderKind::NvidiaNim);
    assert_eq!(chain.position(), 0);
    assert_eq!(
        chain.providers(),
        &[
            ProviderKind::NvidiaNim,
            ProviderKind::Deepseek,
            ProviderKind::Openrouter,
        ]
    );
    assert!(!chain.is_fallback_active());
}

#[test]
fn provider_chain_advance_switches_to_fallback() {
    let mut chain = ProviderChain::new(
        ProviderKind::NvidiaNim,
        &[ProviderKind::Deepseek, ProviderKind::Openrouter],
    );

    assert!(chain.has_next());
    assert_eq!(chain.advance(), Some(ProviderKind::Deepseek));
    assert_eq!(chain.current(), ProviderKind::Deepseek);
    assert!(chain.is_fallback_active());
}

#[test]
fn provider_chain_exhausts_returns_none() {
    let mut chain = ProviderChain::new(ProviderKind::Deepseek, &[ProviderKind::Openrouter]);

    assert_eq!(chain.advance(), Some(ProviderKind::Openrouter));
    assert!(!chain.has_next());
    assert_eq!(chain.advance(), None);
}

#[test]
fn provider_chain_skips_duplicates() {
    let chain = ProviderChain::new(
        ProviderKind::Deepseek,
        &[
            ProviderKind::Deepseek,
            ProviderKind::NvidiaNim,
            ProviderKind::Deepseek,
        ],
    );

    assert_eq!(
        chain.providers(),
        &[ProviderKind::Deepseek, ProviderKind::NvidiaNim]
    );
}

#[test]
fn provider_chain_remaining_counts_current_and_untried_entries() {
    let mut chain = ProviderChain::new(
        ProviderKind::Deepseek,
        &[ProviderKind::NvidiaNim, ProviderKind::Openrouter],
    );

    assert_eq!(chain.remaining(), 3);
    assert_eq!(chain.advance(), Some(ProviderKind::NvidiaNim));
    assert_eq!(chain.remaining(), 2);
}

#[test]
fn config_toml_parses_fallback_providers() {
    let config: ConfigToml = toml::from_str(
        r#"
provider = "nvidia-nim"
fallback_providers = ["deepseek", "openrouter"]
"#,
    )
    .expect("fallback providers config");

    assert_eq!(config.provider, ProviderKind::NvidiaNim);
    assert_eq!(
        config.fallback_providers,
        [ProviderKind::Deepseek, ProviderKind::Openrouter]
    );
}

#[test]
fn empty_fallback_providers_do_not_serialize() {
    let serialized = toml::to_string_pretty(&ConfigToml::default()).expect("config serializes");

    assert!(!serialized.contains("fallback_providers"));
}

#[test]
fn empty_provider_header_tables_do_not_survive_round_trip() {
    let polluted = r#"
[http_headers]

[providers.anthropic.http_headers]
" " = "ignored"
"X-Blank" = "   "

[providers.openrouter.http_headers]

[providers.xai]
model = "   "
"#;
    let config: ConfigToml = toml::from_str(polluted).expect("polluted config parses");
    let serialized = toml::to_string_pretty(&config).expect("config serializes");

    assert!(
        !serialized.contains("[http_headers]"),
        "empty root headers must not be serialized:\n{serialized}"
    );
    assert!(
        !serialized.contains("[providers.anthropic"),
        "empty Anthropic provider state must not be serialized:\n{serialized}"
    );
    assert!(
        !serialized.contains("[providers.openrouter"),
        "empty OpenRouter provider state must not be serialized:\n{serialized}"
    );
    assert!(
        !serialized.contains("[providers.xai"),
        "blank provider fields must not be serialized:\n{serialized}"
    );

    let round_tripped: ConfigToml = toml::from_str(&serialized).expect("canonical config parses");
    assert!(round_tripped.http_headers.is_empty());
    assert!(round_tripped.providers.anthropic.http_headers.is_empty());
    assert!(round_tripped.providers.openrouter.http_headers.is_empty());
}

#[test]
fn workflow_config_defaults_match_product_surface() {
    // #4128 / Section 2.11: omitted `[workflow]` keys resolve to the
    // documented product defaults so launch/approval/persist share one model.
    let defaults = WorkflowConfigToml::default();
    assert!(defaults.automatic);
    assert!(defaults.auto_start_read_only);
    assert!(defaults.require_approval_for_writes);
    assert_eq!(defaults.max_children, 0);
    assert_eq!(defaults.max_concurrent, 16);
    assert_eq!(defaults.max_depth, 0);
    // 0 = no shared cap; budgets are opt-in, matching the parent turn loop's
    // advisory policy (#6189).
    assert_eq!(defaults.default_token_budget, 0);
}

#[test]
fn workflow_config_absent_table_stays_none_empty_table_fills_defaults() {
    let absent: ConfigToml = toml::from_str("").expect("empty config parses");
    assert!(absent.workflow.is_none());

    let empty_table: ConfigToml = toml::from_str(
        r#"
[workflow]
"#,
    )
    .expect("empty workflow table should parse");
    assert_eq!(
        empty_table.workflow.expect("workflow table present"),
        WorkflowConfigToml::default()
    );
}

#[test]
fn workflow_config_partial_override_and_round_trip() {
    let config: ConfigToml = toml::from_str(
        r#"
[workflow]
automatic = false
max_children = 16
default_token_budget = 50000
"#,
    )
    .expect("workflow overrides should parse");

    let workflow = config.workflow.expect("workflow table");
    assert!(!workflow.automatic);
    assert_eq!(workflow.max_children, 16);
    assert_eq!(workflow.default_token_budget, 50_000);
    // Unset keys keep product defaults.
    assert!(workflow.auto_start_read_only);
    assert!(workflow.require_approval_for_writes);
    assert_eq!(workflow.max_concurrent, 16);
    assert_eq!(workflow.max_depth, 0);

    let serialized = toml::to_string_pretty(&workflow).expect("workflow serializes");
    let round_tripped: WorkflowConfigToml =
        toml::from_str(&serialized).expect("serialized workflow parses");
    assert_eq!(round_tripped, workflow);
}

#[test]
fn fleet_exec_config_default_matches_subagent_depth() {
    // Fleet workers and standalone sub-agents share one recursion axis:
    // the fleet default equals DEFAULT_SPAWN_DEPTH without a nesting cap.
    assert_eq!(
        FleetExecConfig::default().max_spawn_depth,
        codewhale_protocol::fleet::DEFAULT_SPAWN_DEPTH
    );
    assert_eq!(FleetExecConfig::default().max_spawn_depth, u32::MAX);
}

#[test]
fn fleet_exec_model_turns_are_unbounded_by_default() {
    assert_eq!(FleetExecConfig::default().max_turns, 0);
}

#[test]
fn fleet_exec_config_parses_max_spawn_depth() {
    let config: ConfigToml = toml::from_str(
        r#"
[fleet.exec]
max_spawn_depth = 2
"#,
    )
    .expect("fleet exec config should parse");

    assert_eq!(config.fleet.expect("fleet config").exec.max_spawn_depth, 2);
}

/// Retired tables/keys are ignored, never a parse failure: a pre-0.9.14
/// config with inline `[fleets.*]`, legacy trust keys, or dead workflow
/// knobs must still load (named fleets live in `fleets/*.toml` files).
#[test]
fn retired_inline_fleet_and_workflow_keys_are_ignored_not_rejected() {
    let config: ConfigToml = toml::from_str(
        r#"
[fleet]
default_trust_level = "local"

[fleets.alice-team]
operator = "alice"
default_trust_level = "local"

[workflow]
auto_start_child_limit = 4
max_parallel_writes_without_worktree = 1
persist_completed_activity = false
"#,
    )
    .expect("retired keys must still parse");
    assert!(config.fleet.is_some());
    assert!(config.workflow.is_some());
}

#[test]
fn fleet_profile_defaults_round_trip_through_config() {
    let config: ConfigToml = toml::from_str(
        r#"
[fleet.profiles.default]
"#,
    )
    .expect("fleet profile config should parse");

    let profile = config
        .fleet
        .expect("fleet config")
        .profiles
        .get("default")
        .expect("default profile")
        .clone();

    assert_eq!(profile, FleetProfile::default());
    assert!(!profile.permissions.allow_shell);
    assert!(!profile.permissions.trust);
    assert!(profile.permissions.approval_required);

    let serialized = toml::to_string_pretty(&profile).expect("profile serializes");
    let round_tripped: FleetProfile =
        toml::from_str(&serialized).expect("serialized profile parses");
    assert_eq!(round_tripped, profile);
}

#[test]
fn fleet_profile_explicit_config_parses_legacy_permissions_as_ignored_input() {
    let config: ConfigToml = toml::from_str(
        r#"
[fleet.profiles.verifier]
slot = "verifier"
loadout = "review"
model = "deepseek-v4-pro"

[fleet.profiles.verifier.role]
name = "verifier"
description = "Read-only verification worker"
instructions = "Check the patch and report evidence."

[fleet.profiles.verifier.permissions]
allow_shell = false
trust = false
approval_required = true

[fleet.profiles.verifier.delegation]
max_spawn_depth = 0
concurrency = 3
"#,
    )
    .expect("fleet profile config should parse");

    let profile = config
        .fleet
        .expect("fleet config")
        .profiles
        .get("verifier")
        .expect("verifier profile")
        .clone();

    assert_eq!(profile.slot, FleetSlot::Verifier);
    assert_eq!(profile.role.name, "verifier");
    assert_eq!(
        profile.role.description.as_deref(),
        Some("Read-only verification worker")
    );
    assert_eq!(
        profile.role.instructions.as_deref(),
        Some("Check the patch and report evidence.")
    );
    // "review" was a retired decorative tier: it parses as Custom and keeps
    // the same auto routing it always had.
    assert_eq!(profile.loadout, FleetLoadout::Custom("review".to_string()));
    assert_eq!(profile.model.as_deref(), Some("deepseek-v4-pro"));
    assert!(!profile.permissions.allow_shell);
    assert!(!profile.permissions.trust);
    assert!(profile.permissions.approval_required);
    assert_eq!(profile.delegation.max_spawn_depth, Some(0));
    assert_eq!(profile.delegation.max_concurrency, Some(3));
    let serialized = toml::to_string_pretty(&profile).expect("profile serializes");
    assert!(!serialized.contains("permissions"));
}

#[test]
fn fleet_loadout_accepts_default_model_classes() {
    assert_eq!(FleetLoadout::from_name("fast"), FleetLoadout::Fast);
    assert_eq!(FleetLoadout::from_name("inherit"), FleetLoadout::Inherit);
    assert_eq!(FleetLoadout::from_name(""), FleetLoadout::Inherit);
    assert_eq!(FleetLoadout::Fast.as_str(), "fast");
    // Retired tiers stay parseable as Custom so old configs keep loading
    // with identical (auto) routing.
    assert_eq!(
        FleetLoadout::from_name("strong"),
        FleetLoadout::Custom("strong".to_string())
    );
    assert_eq!(
        FleetLoadout::from_name("tool-heavy"),
        FleetLoadout::Custom("tool-heavy".to_string())
    );
    assert_eq!(
        FleetLoadout::Custom("strong".to_string()).as_str(),
        "strong"
    );
}

#[test]
fn fallback_providers_do_not_change_runtime_resolution() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let config = ConfigToml {
        provider: ProviderKind::NvidiaNim,
        fallback_providers: vec![ProviderKind::Deepseek],
        ..ConfigToml::default()
    };

    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    assert_eq!(resolved.provider, ProviderKind::NvidiaNim);
}

#[test]
fn test_verbosity_resolution() {
    let _lock = env_lock();
    // Test TOML parsing
    let toml_str = r#"
        verbosity = "concise"
    "#;
    let config: ConfigToml = toml::from_str(toml_str).unwrap();
    assert_eq!(config.verbosity, Some("concise".to_string()));

    // Test Env overrides
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    unsafe {
        std::env::set_var("CODEWHALE_VERBOSITY", "normal");
    }
    let env_overrides = EnvRuntimeOverrides::load();
    assert_eq!(env_overrides.verbosity, Some("normal".to_string()));
    unsafe {
        std::env::remove_var("CODEWHALE_VERBOSITY");
    }

    // Test fallback to DEEPSEEK_VERBOSITY
    unsafe {
        std::env::set_var("DEEPSEEK_VERBOSITY", "concise");
    }
    let env_overrides = EnvRuntimeOverrides::load();
    assert_eq!(env_overrides.verbosity, Some("concise".to_string()));
    unsafe {
        std::env::remove_var("DEEPSEEK_VERBOSITY");
    }
}

/// Save and restore the telemetry env vars around a test that mutates them.
///
/// Held together with [`env_lock`]: the process environment is global, so
/// every telemetry test serialises on the same mutex the provider tests use.
struct TelemetryEnvGuard {
    codewhale: Option<OsString>,
    deepseek: Option<OsString>,
    floor: Option<OsString>,
    codewhale_endpoint: Option<OsString>,
    deepseek_endpoint: Option<OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl TelemetryEnvGuard {
    fn take() -> Self {
        let lock = env_lock();
        let guard = Self {
            codewhale: env::var_os("CODEWHALE_TELEMETRY"),
            deepseek: env::var_os("DEEPSEEK_TELEMETRY"),
            floor: env::var_os(TELEMETRY_FLOOR_ENV),
            // The endpoint variables are cleared too. Resolution now has a
            // shipped default, so an ambient endpoint in the developer's or
            // CI's environment is the difference between pinning the default
            // and pinning whatever that machine happened to export.
            codewhale_endpoint: env::var_os("CODEWHALE_TELEMETRY_ENDPOINT"),
            deepseek_endpoint: env::var_os("DEEPSEEK_TELEMETRY_ENDPOINT"),
            _lock: lock,
        };
        // Safety: test-only environment mutation guarded by the module mutex.
        unsafe {
            env::remove_var("CODEWHALE_TELEMETRY");
            env::remove_var("DEEPSEEK_TELEMETRY");
            env::remove_var(TELEMETRY_FLOOR_ENV);
            env::remove_var("CODEWHALE_TELEMETRY_ENDPOINT");
            env::remove_var("DEEPSEEK_TELEMETRY_ENDPOINT");
        }
        guard
    }

    fn set(&self, value: &str) {
        // Safety: test-only environment mutation guarded by the module mutex.
        unsafe {
            env::set_var("CODEWHALE_TELEMETRY", value);
        }
    }

    fn set_endpoint(&self, value: &str) {
        // Safety: test-only environment mutation guarded by the module mutex.
        unsafe {
            env::set_var("CODEWHALE_TELEMETRY_ENDPOINT", value);
        }
    }

    fn set_floor(&self, value: &str) {
        // Safety: test-only environment mutation guarded by the module mutex.
        unsafe {
            env::set_var(TELEMETRY_FLOOR_ENV, value);
        }
    }

    fn clear(&self) {
        // Safety: test-only environment mutation guarded by the module mutex.
        unsafe {
            env::remove_var("CODEWHALE_TELEMETRY");
            env::remove_var("DEEPSEEK_TELEMETRY");
            env::remove_var(TELEMETRY_FLOOR_ENV);
        }
    }
}

impl Drop for TelemetryEnvGuard {
    fn drop(&mut self) {
        // Safety: test-only environment mutation guarded by the module mutex.
        unsafe {
            match self.codewhale.take() {
                Some(value) => env::set_var("CODEWHALE_TELEMETRY", value),
                None => env::remove_var("CODEWHALE_TELEMETRY"),
            }
            match self.deepseek.take() {
                Some(value) => env::set_var("DEEPSEEK_TELEMETRY", value),
                None => env::remove_var("DEEPSEEK_TELEMETRY"),
            }
            match self.floor.take() {
                Some(value) => env::set_var(TELEMETRY_FLOOR_ENV, value),
                None => env::remove_var(TELEMETRY_FLOOR_ENV),
            }
            match self.codewhale_endpoint.take() {
                Some(value) => env::set_var("CODEWHALE_TELEMETRY_ENDPOINT", value),
                None => env::remove_var("CODEWHALE_TELEMETRY_ENDPOINT"),
            }
            match self.deepseek_endpoint.take() {
                Some(value) => env::set_var("DEEPSEEK_TELEMETRY_ENDPOINT", value),
                None => env::remove_var("DEEPSEEK_TELEMETRY_ENDPOINT"),
            }
        }
    }
}

#[test]
fn env_telemetry_off_is_a_floor_over_cli_on() {
    let guard = TelemetryEnvGuard::take();
    guard.set("0");

    let config = ConfigToml {
        telemetry: Some(true),
        ..ConfigToml::default()
    };
    let cli = CliRuntimeOverrides {
        telemetry: Some(true),
        ..CliRuntimeOverrides::default()
    };

    let resolved = config.resolve_runtime_options(&cli);

    // `--telemetry true` must not be able to climb back over an explicit
    // `CODEWHALE_TELEMETRY=0`. Off is a floor, not one more precedence rung.
    assert!(!resolved.telemetry);
    // …but the environment is a run-scoped switch, not a revocation. See
    // `a_run_scoped_off_is_a_kill_switch_and_not_a_revocation`.
    assert!(!resolved.telemetry_explicit_off);
}

#[test]
fn persisted_telemetry_off_is_a_floor_over_cli_on() {
    // Regression: `--telemetry true` used to beat `telemetry = false` in the
    // config file, and the dispatcher then forwarded the resolved `true` as
    // `CODEWHALE_TELEMETRY=true`, which also outranked the child's own copy of
    // that file. Any wrapper script, alias, or agent harness passing the flag
    // silently re-enabled a user who had turned telemetry off through the one
    // switch the first-run notice advertises as permanent.
    let guard = TelemetryEnvGuard::take();

    let config = ConfigToml {
        telemetry: Some(false),
        ..ConfigToml::default()
    };
    let cli = CliRuntimeOverrides {
        telemetry: Some(true),
        ..CliRuntimeOverrides::default()
    };

    let resolved = config.resolve_runtime_options(&cli);
    assert!(!resolved.telemetry);
    // And it stays an answer, so the run re-asserts the tombstone.
    assert!(resolved.telemetry_explicit_off);

    // An environment "on" loses to it as well: re-enabling is writing the
    // durable register the off was written in.
    guard.set("1");
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert!(!resolved.telemetry);
    assert!(resolved.telemetry_explicit_off);
}

#[test]
fn a_run_scoped_off_is_a_kill_switch_and_not_a_revocation() {
    // Regression: an explicit `CODEWHALE_TELEMETRY=0` marked the run as an
    // *answer*, so the telemetry crate took its destructive opt-out branch —
    // deleting the install id and truncating the user's own dry-run records —
    // on a recipe the runtime docs prescribe for one command. Worse, the
    // dispatcher forwards a resolved `false` on every ordinary run, so the
    // shipped default was indistinguishable from a revocation.
    let guard = TelemetryEnvGuard::take();

    for value in ["0", "false", "off", "disabled", "no"] {
        guard.set(value);
        let config = ConfigToml {
            telemetry: Some(true),
            ..ConfigToml::default()
        };
        let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
        assert!(!resolved.telemetry, "{value} must stop the run");
        assert!(
            !resolved.telemetry_explicit_off,
            "{value} must not read as a revocation"
        );
    }

    // The same for the per-run flag, with the environment back to silent.
    guard.clear();
    let cli = CliRuntimeOverrides {
        telemetry: Some(false),
        ..CliRuntimeOverrides::default()
    };
    let resolved = ConfigToml {
        telemetry: Some(true),
        ..ConfigToml::default()
    }
    .resolve_runtime_options(&cli);
    assert!(!resolved.telemetry);
    assert!(!resolved.telemetry_explicit_off);
}

/// #5441: the resolved consent must name its source, because "telemetry: on"
/// with no provenance hides the one default users most need to see.
#[test]
fn telemetry_consent_names_its_source() {
    let guard = TelemetryEnvGuard::take();

    // Nobody said anything: on, by default.
    let (on, source) = resolved_telemetry_consent(None);
    assert!(on);
    assert_eq!(source, TelemetrySource::Default);

    // The config file owns the answer.
    let (on, source) = resolved_telemetry_consent(Some(true));
    assert!(on);
    assert_eq!(source, TelemetrySource::Config);

    // A persisted off is a floor and is named as the decision.
    let (on, source) = resolved_telemetry_consent(Some(false));
    assert!(!on);
    assert_eq!(source, TelemetrySource::Config);

    // An explicit environment "on" loses to the persisted off: re-enabling
    // is writing the durable register the off was written in.
    guard.set("1");
    let (on, source) = resolved_telemetry_consent(Some(false));
    assert!(!on);
    assert_eq!(source, TelemetrySource::Config);

    // An environment kill switch decides and is named.
    guard.set("0");
    let (on, source) = resolved_telemetry_consent(Some(true));
    assert!(!on);
    assert_eq!(source, TelemetrySource::Env);

    // An unreadable environment value is a kill switch, never "on".
    guard.set("yes-please");
    let (on, source) = resolved_telemetry_consent(Some(true));
    assert!(!on);
    assert_eq!(source, TelemetrySource::Env);

    // A clean environment "on" with nothing in the file is env-owned.
    guard.set("1");
    let (on, source) = resolved_telemetry_consent(None);
    assert!(on);
    assert_eq!(source, TelemetrySource::Env);
}

#[test]
fn resolved_runtime_options_mints_a_route_candidate() {
    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());
    let route = resolved
        .route
        .as_ref()
        .expect("RouteResolver is the runtime path");
    assert_eq!(route.provider_kind(), resolved.provider);
    assert_eq!(route.endpoint().base_url, resolved.base_url);
}

/// #5441: the runtime receipt carries the same source the surfaces print.
#[test]
fn resolved_runtime_options_reports_telemetry_source() {
    let guard = TelemetryEnvGuard::take();

    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());
    assert!(resolved.telemetry);
    assert_eq!(resolved.telemetry_source, TelemetrySource::Default);

    // The CLI flag owns the answer for this run, off or on.
    let cli = CliRuntimeOverrides {
        telemetry: Some(false),
        ..CliRuntimeOverrides::default()
    };
    let resolved = ConfigToml::default().resolve_runtime_options(&cli);
    assert!(!resolved.telemetry);
    assert_eq!(resolved.telemetry_source, TelemetrySource::Cli);

    // A kill switch still beats `--telemetry true`, and the source says so.
    guard.set("0");
    let cli = CliRuntimeOverrides {
        telemetry: Some(true),
        ..CliRuntimeOverrides::default()
    };
    let resolved = ConfigToml::default().resolve_runtime_options(&cli);
    assert!(!resolved.telemetry);
    assert_eq!(resolved.telemetry_source, TelemetrySource::Env);
}

/// #5441: `config get telemetry` reports the resolved consent with its
/// source instead of "key not found" on a machine whose batches ship.
#[test]
fn config_display_for_telemetry_reports_resolved_consent_with_source() {
    let guard = TelemetryEnvGuard::take();

    let config = ConfigToml::default();
    assert_eq!(
        config.get_display_value("telemetry").as_deref(),
        Some("on (default)")
    );

    let config = ConfigToml {
        telemetry: Some(false),
        ..ConfigToml::default()
    };
    assert_eq!(
        config.get_display_value("telemetry").as_deref(),
        Some("off (config)")
    );

    guard.set("0");
    let config = ConfigToml {
        telemetry: Some(true),
        ..ConfigToml::default()
    };
    assert_eq!(
        config.get_display_value("telemetry").as_deref(),
        Some("off (env)")
    );
}

#[test]
fn the_dispatcher_states_the_floor_rather_than_letting_the_child_infer_it() {
    // The child cannot tell an operator's declared kill switch from the
    // shipped default: both arrive as `CODEWHALE_TELEMETRY=false`. So the
    // dispatcher states it, and the statement outranks the inference — which
    // is what lets the first-run notice refuse to ask under a real floor while
    // still asking on an ordinary first run.
    let guard = TelemetryEnvGuard::take();
    assert!(!telemetry_floor_in_force());

    guard.set("0");
    assert!(telemetry_floor_in_force());

    // A forwarded resolved `false` with the dispatcher saying "no floor" is
    // the ordinary first run.
    guard.set("false");
    guard.set_floor("0");
    assert!(!telemetry_floor_in_force());

    // A declared floor holds even where the value alone would not show it.
    guard.set("true");
    guard.set_floor("1");
    assert!(telemetry_floor_in_force());
    let resolved = ConfigToml {
        telemetry: Some(true),
        ..ConfigToml::default()
    }
    .resolve_runtime_options(&CliRuntimeOverrides::default());
    assert!(!resolved.telemetry);

    // An unreadable value is a floor: a typo in a kill switch never resolves
    // to "on".
    guard.clear();
    guard.set("maybe");
    assert!(telemetry_floor_in_force());
}

#[test]
fn unparseable_telemetry_env_fails_closed() {
    let guard = TelemetryEnvGuard::take();
    guard.set("maybe");

    let config = ConfigToml {
        telemetry: Some(true),
        ..ConfigToml::default()
    };
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());

    // A typo must not fall through to the config file's `true`.
    assert!(!resolved.telemetry);
    // …but it is also not the user answering "no", so it is not an explicit
    // opt-out either.
    assert!(!resolved.telemetry_explicit_off);
}

#[test]
fn telemetry_env_invalid_is_recorded_rather_than_swallowed() {
    let guard = TelemetryEnvGuard::take();
    guard.set("sure why not");

    let env = EnvRuntimeOverrides::load();
    assert_eq!(env.telemetry, None);
    assert!(env.telemetry_env_invalid);

    guard.set("disabled");
    let env = EnvRuntimeOverrides::load();
    assert_eq!(env.telemetry, Some(false));
    assert!(!env.telemetry_env_invalid);

    guard.set("enabled");
    let env = EnvRuntimeOverrides::load();
    assert_eq!(env.telemetry, Some(true));
    assert!(!env.telemetry_env_invalid);
}

#[test]
fn telemetry_explicit_off_distinguishes_an_answer_from_the_default() {
    let _guard = TelemetryEnvGuard::take();

    // Nobody said anything: default on, with no explicit opt-out.
    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());
    assert!(resolved.telemetry);
    assert!(!resolved.telemetry_explicit_off);

    // The config file says no.
    let config = ConfigToml {
        telemetry: Some(false),
        ..ConfigToml::default()
    };
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert!(!resolved.telemetry);
    assert!(resolved.telemetry_explicit_off);

    // A CLI `--telemetry true` does not override the file's `false`: the
    // persistent switch is a floor, and the answer stands.
    let cli = CliRuntimeOverrides {
        telemetry: Some(true),
        ..CliRuntimeOverrides::default()
    };
    let resolved = config.resolve_runtime_options(&cli);
    assert!(!resolved.telemetry);
    assert!(resolved.telemetry_explicit_off);

    // A CLI `--telemetry false` stops the run without being an answer: it is
    // scoped to the run, and a run-scoped switch must not delete state.
    let cli = CliRuntimeOverrides {
        telemetry: Some(false),
        ..CliRuntimeOverrides::default()
    };
    let resolved = ConfigToml::default().resolve_runtime_options(&cli);
    assert!(!resolved.telemetry);
    assert!(!resolved.telemetry_explicit_off);
}

/// The shipped default endpoint, pinned by value.
///
/// A default nobody asserts is a default that drifts, and this one decides
/// which host an enabled session contacts. The literal is repeated here on
/// purpose: comparing the constant to itself would pass against any edit.
#[test]
fn an_unconfigured_endpoint_resolves_to_the_shipped_default() {
    let _guard = TelemetryEnvGuard::take();

    assert_eq!(
        DEFAULT_TELEMETRY_ENDPOINT,
        "https://telemetry.codewhale.net/v1/telemetry"
    );
    let resolved = ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(
        resolved.telemetry_endpoint.as_deref(),
        Some(DEFAULT_TELEMETRY_ENDPOINT),
        "an unconfigured endpoint must resolve to the shipped default"
    );

    // The shipped usage preference is on without an acceptance prerequisite.
    assert!(resolved.telemetry);
    assert!(!resolved.telemetry_explicit_off);
}

/// A user-set endpoint beats the shipped default, from either source, and an
/// explicitly *empty* one is the dry-run sink rather than a missing value.
#[test]
fn a_configured_endpoint_beats_the_shipped_default() {
    let guard = TelemetryEnvGuard::take();

    let config = ConfigToml {
        telemetry_endpoint: Some("https://collector.internal/v1/batch".to_string()),
        ..ConfigToml::default()
    };
    assert_eq!(
        config
            .resolve_runtime_options(&CliRuntimeOverrides::default())
            .telemetry_endpoint
            .as_deref(),
        Some("https://collector.internal/v1/batch"),
        "the config file must outrank the shipped default"
    );

    // The environment outranks the file, which outranks the default.
    guard.set_endpoint("https://collector.env.internal/v1/batch");
    assert_eq!(
        config
            .resolve_runtime_options(&CliRuntimeOverrides::default())
            .telemetry_endpoint
            .as_deref(),
        Some("https://collector.env.internal/v1/batch")
    );
    assert_eq!(
        ConfigToml::default()
            .resolve_runtime_options(&CliRuntimeOverrides::default())
            .telemetry_endpoint
            .as_deref(),
        Some("https://collector.env.internal/v1/batch")
    );
}

/// An empty endpoint means "contact nobody", not "use the default".
///
/// `None` at the resolved layer is what `codewhale-telemetry`'s client reads as
/// the dry-run sink: batches are serialized exactly as a server would see them
/// and appended to `dryrun.jsonl`, and no HTTP client is constructed. With a
/// shipped default in place that path is only reachable through an explicit
/// empty value, so both ways of writing one are pinned here.
#[test]
fn an_empty_endpoint_keeps_the_dry_run_sink_reachable() {
    let guard = TelemetryEnvGuard::take();

    let config = ConfigToml {
        telemetry_endpoint: Some(String::new()),
        ..ConfigToml::default()
    };
    assert_eq!(
        config
            .resolve_runtime_options(&CliRuntimeOverrides::default())
            .telemetry_endpoint,
        None,
        "`telemetry_endpoint = \"\"` must resolve to the dry-run sink"
    );

    // Whitespace is the same statement typed less carefully.
    let config = ConfigToml {
        telemetry_endpoint: Some("   ".to_string()),
        ..ConfigToml::default()
    };
    assert_eq!(
        config
            .resolve_runtime_options(&CliRuntimeOverrides::default())
            .telemetry_endpoint,
        None
    );

    // An emptied environment variable says it too, and it says it over a
    // config file that names a real host — otherwise the documented one-shot
    // `CODEWHALE_TELEMETRY_ENDPOINT= codewhale …` would silently keep sending.
    let config = ConfigToml {
        telemetry_endpoint: Some("https://collector.internal/v1/batch".to_string()),
        ..ConfigToml::default()
    };
    guard.set_endpoint("");
    assert_eq!(
        config
            .resolve_runtime_options(&CliRuntimeOverrides::default())
            .telemetry_endpoint,
        None
    );
    assert_eq!(
        ConfigToml::default()
            .resolve_runtime_options(&CliRuntimeOverrides::default())
            .telemetry_endpoint,
        None,
        "an emptied environment variable must not fall through to the default"
    );
}

#[test]
fn telemetry_endpoint_round_trips_through_all_four_verbs() {
    let mut config = ConfigToml::default();
    assert_eq!(config.get_value("telemetry_endpoint"), None);
    assert!(!config.list_values().contains_key("telemetry_endpoint"));

    config
        .set_value("telemetry_endpoint", "https://collector.example/ingest")
        .expect("set telemetry_endpoint");
    assert_eq!(
        config.get_value("telemetry_endpoint").as_deref(),
        Some("https://collector.example/ingest")
    );
    assert_eq!(
        config
            .list_values()
            .get("telemetry_endpoint")
            .map(String::as_str),
        Some("https://collector.example/ingest")
    );

    // Scheme rules belong at send time, not at set time: staging a value the
    // client will later refuse must still be possible.
    config
        .set_value("telemetry_endpoint", "http://collector.example/ingest")
        .expect("staging an http endpoint is not a set-time error");
    assert_eq!(
        config.get_value("telemetry_endpoint").as_deref(),
        Some("http://collector.example/ingest")
    );

    config
        .unset_value("telemetry_endpoint")
        .expect("unset telemetry_endpoint");
    assert_eq!(config.get_value("telemetry_endpoint"), None);
    assert!(!config.list_values().contains_key("telemetry_endpoint"));

    // The key must land as a typed field, never in `extras` — an extras key
    // would serialize after the section tables and break the file.
    assert!(!config.extras.contains_key("telemetry_endpoint"));
}

#[test]
fn telemetry_endpoint_stays_a_scalar_sibling_of_telemetry() {
    // `telemetry` is a root scalar and every section table is declared after
    // it, so the endpoint must serialize as a scalar too. A `[telemetry]`
    // table would be a hard parse failure on load, and a scalar emitted after
    // a table is a TOML `ValueAfterTable` error on save.
    let config = ConfigToml {
        telemetry: Some(true),
        telemetry_endpoint: Some("https://collector.example/ingest".to_string()),
        ..ConfigToml::default()
    };
    let rendered = toml::to_string_pretty(&config).expect("serialize config");
    assert!(
        rendered.contains("telemetry_endpoint = \"https://collector.example/ingest\""),
        "{rendered}"
    );
    assert!(!rendered.contains("[telemetry]"), "{rendered}");

    let endpoint_at = rendered
        .find("telemetry_endpoint =")
        .expect("endpoint present");
    if let Some(first_table_at) = rendered.find("\n[") {
        assert!(
            endpoint_at < first_table_at,
            "telemetry_endpoint must precede every section table:\n{rendered}"
        );
    }

    let round_tripped: ConfigToml = toml::from_str(&rendered).expect("round trips");
    assert_eq!(
        round_tripped.telemetry_endpoint.as_deref(),
        Some("https://collector.example/ingest")
    );
    assert_eq!(round_tripped.telemetry, Some(true));
}

#[test]
fn a_telemetry_table_is_a_hard_load_failure_and_stays_unbuildable() {
    // Documents *why* the endpoint is a sibling scalar rather than
    // `[telemetry] endpoint = …`: `telemetry` is already `Option<bool>`, so a
    // table of that name cannot deserialize at all.
    let err = toml::from_str::<ConfigToml>("[telemetry]\nenabled = true\n")
        .expect_err("a [telemetry] table must not deserialize");
    let _ = err;
}

#[test]
fn telemetry_notice_is_owed_until_it_is_answered_out_loud() {
    let mut state = SetupState::default();

    // A fresh record owes the notice, and "owed" is not an answer either way.
    assert!(state.needs_telemetry_notice(TELEMETRY_NOTICE_VERSION));
    assert!(!state.telemetry_accepted(TELEMETRY_NOTICE_VERSION));
    assert!(!state.telemetry_declined(TELEMETRY_NOTICE_VERSION));

    state.record_telemetry_notice(TELEMETRY_NOTICE_VERSION, false);
    assert!(!state.needs_telemetry_notice(TELEMETRY_NOTICE_VERSION));
    assert!(!state.telemetry_accepted(TELEMETRY_NOTICE_VERSION));
    assert!(state.telemetry_declined(TELEMETRY_NOTICE_VERSION));

    state.record_telemetry_notice(TELEMETRY_NOTICE_VERSION, true);
    assert!(state.telemetry_accepted(TELEMETRY_NOTICE_VERSION));
    assert!(!state.telemetry_declined(TELEMETRY_NOTICE_VERSION));

    // A decision recorded against different notice content is stale: the
    // notice is owed again. The stale `true` still means the user did not opt
    // out; this helper only describes whether the current wording was shown.
    state.record_telemetry_notice("0", true);
    assert!(state.needs_telemetry_notice(TELEMETRY_NOTICE_VERSION));
    assert!(!state.telemetry_accepted(TELEMETRY_NOTICE_VERSION));
    assert!(!state.telemetry_declined(TELEMETRY_NOTICE_VERSION));
}

#[test]
fn deferring_the_constitution_checkpoint_does_not_answer_the_telemetry_notice() {
    // `complete_constitution_checkpoint(_, Deferred)` persists a completed
    // checkpoint without showing the user anything — reached from the
    // skip-onboarding path. The telemetry notice must not mirror that.
    let mut state = SetupState::default();
    state.complete_constitution_checkpoint("0.9.4", ConstitutionChoice::Deferred);

    assert_eq!(
        state.constitution_checkpoint_completed_for.as_deref(),
        Some("0.9.4")
    );
    assert_eq!(state.telemetry_notice_decided_for, None);
    assert!(state.needs_telemetry_notice(TELEMETRY_NOTICE_VERSION));
    assert!(!state.telemetry_opt_in);
}

#[test]
fn inherited_setup_state_never_carries_a_telemetry_decision() {
    // Upgrading users with no `setup_state.json` get a derived record. It must
    // not manufacture an answer they never gave.
    let state = SetupState::derive_inherited(&InheritedConfigFacts {
        has_provider_route: true,
        has_credentials_or_local_runtime: true,
        trust_chosen: true,
        language: Some("en".to_string()),
        ..InheritedConfigFacts::default()
    });
    assert_eq!(state.telemetry_notice_decided_for, None);
    assert!(!state.telemetry_opt_in);
    assert!(state.needs_telemetry_notice(TELEMETRY_NOTICE_VERSION));
}

#[test]
fn telemetry_notice_fields_round_trip_and_stay_absent_when_unanswered() {
    let mut state = SetupState::default();
    let rendered = serde_json::to_string(&state).expect("serialize setup state");
    assert!(
        !rendered.contains("telemetry_notice_decided_for"),
        "an unanswered notice must not write a field: {rendered}"
    );
    assert!(
        !rendered.contains("telemetry_opt_in"),
        "the default false compatibility field must not be serialized: {rendered}"
    );

    state.record_telemetry_notice(TELEMETRY_NOTICE_VERSION, true);
    let rendered = serde_json::to_string(&state).expect("serialize setup state");
    let round_tripped: SetupState = serde_json::from_str(&rendered).expect("round trips");
    assert_eq!(round_tripped, state);

    // Records written before these fields existed load as "notice owed".
    let legacy: SetupState =
        serde_json::from_str(r#"{"schema_version":1}"#).expect("legacy record loads");
    assert_eq!(legacy.telemetry_notice_decided_for, None);
    assert!(!legacy.telemetry_opt_in);
}

#[test]
fn telemetry_disclosure_records_presentation_without_acceptance_or_erasing_old_declines() {
    for version in [None, Some("1"), Some("4")] {
        for enabled in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("setup_state.json");
            let mut state = SetupState {
                constitution_preview_version: 12,
                ..Default::default()
            };
            if let Some(version) = version {
                state.record_telemetry_notice(version, enabled);
            }
            state.save_to(&path).unwrap();
            SetupState::update_at(&path, |latest| {
                latest.record_telemetry_notice_shown(TELEMETRY_NOTICE_VERSION);
            })
            .unwrap();
            let shown = SetupState::load_from(&path).unwrap();
            assert!(!shown.needs_telemetry_notice(TELEMETRY_NOTICE_VERSION));
            assert!(!shown.telemetry_accepted(TELEMETRY_NOTICE_VERSION));
            assert!(!shown.telemetry_declined(TELEMETRY_NOTICE_VERSION));
            assert_eq!(shown.telemetry_opted_out(), version.is_some() && !enabled);
            assert_eq!(
                shown.telemetry_notice_decided_for,
                state.telemetry_notice_decided_for
            );
            assert_eq!(shown.telemetry_opt_in, state.telemetry_opt_in);
            assert_eq!(shown.constitution_preview_version, 12);
        }
    }
}

#[test]
fn telemetry_metadata_update_refuses_corrupt_or_busy_state_and_reloads_the_saved_decline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("setup_state.json");
    std::fs::write(&path, "not-json").unwrap();
    assert!(SetupState::update_at(&path, |_| {}).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "not-json");
    SetupState::default().save_to(&path).unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.with_extension("telemetry.lock"))
        .unwrap();
    let mut lock = fd_lock::RwLock::new(file);
    let guard = lock.write().unwrap();
    assert!(
        SetupState::update_at(&path, |state| {
            state.record_telemetry_notice_shown(TELEMETRY_NOTICE_VERSION);
        })
        .is_err(),
        "display bookkeeping must never block startup"
    );
    drop(guard);
    SetupState::update_at(&path, |state| {
        state.record_telemetry_notice("4", false);
    })
    .unwrap();
    SetupState::update_at(&path, |state| {
        state.record_telemetry_notice_shown(TELEMETRY_NOTICE_VERSION);
    })
    .unwrap();
    assert!(SetupState::load_from(&path).unwrap().telemetry_opted_out());
}

#[test]
fn openrouter_vendor_config_round_trip_and_trust_boundary() -> Result<()> {
    let key = "providers.openrouter.vendor";
    let mut config = ConfigToml::default();
    config.set_value(key, "deepinfra/turbo")?;
    let serialized = toml::to_string(&config)?;
    let mut reloaded: ConfigToml = toml::from_str(&serialized)?;
    assert_eq!(reloaded.get_value(key).as_deref(), Some("deepinfra/turbo"));
    assert_eq!(
        reloaded.list_values().get(key).map(String::as_str),
        Some("deepinfra/turbo")
    );
    assert_eq!(reloaded.get_value(key).as_deref(), Some("deepinfra/turbo"));
    for invalid in ["deep infra", "deepinfra\n/turbo", " deepinfra"] {
        assert!(reloaded.set_value(key, invalid).is_err());
    }
    assert!(
        reloaded
            .set_value("providers.openai.vendor", "deepinfra")
            .is_err()
    );
    assert!(
        reloaded
            .set_value("providers.my-gateway.vendor", "deepinfra")
            .is_err()
    );
    reloaded.set_value(key, "")?;
    let cleared: ConfigToml = toml::from_str(&toml::to_string(&reloaded)?)?;
    assert_eq!(cleared.get_value(key).as_deref(), Some(""));
    reloaded.unset_value(key)?;
    assert_eq!(reloaded.get_value(key), None);
    Ok(())
}

#[test]
fn notifications_nested_edits_keep_toml_types_siblings_and_future_fields() {
    let mut config: ConfigToml = toml::from_str(
        r#"
[notifications]
quiet = false
future_delivery = "keep"
[notifications.events]
input-needed = false
"#,
    )
    .unwrap();
    config.set_value("notifications.quiet", "true").unwrap();
    config
        .set_value("notifications.threshold_secs", "42")
        .unwrap();
    config
        .set_value("notifications.events.approval-needed", "false")
        .unwrap();
    config
        .set_value(
            "notifications.event_sound.events",
            r#"["input-needed", "model-notify"]"#,
        )
        .unwrap();
    config.set_value("notifications.sound", "whale").unwrap();
    let encoded = toml::to_string(&config).unwrap();
    let raw: toml::Value = toml::from_str(&encoded).unwrap();
    let notifications = &raw["notifications"];
    assert_eq!(notifications["quiet"].as_bool(), Some(true));
    assert_eq!(notifications["threshold_secs"].as_integer(), Some(42));
    assert_eq!(
        notifications["events"]["approval-needed"].as_bool(),
        Some(false)
    );
    assert_eq!(
        notifications["events"]["input-needed"].as_bool(),
        Some(false)
    );
    assert_eq!(notifications["future_delivery"].as_str(), Some("keep"));
    assert_eq!(
        notifications["event_sound"]["events"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(!raw.as_table().unwrap().contains_key("notifications.quiet"));
    assert_eq!(
        config.get_display_value("notifications.sound").as_deref(),
        Some("whale")
    );
}

#[test]
fn notifications_invalid_edits_are_atomic_even_with_public_update_values() {
    use notifications::{NotificationConfigUpdate as Update, NotificationSetting as Key};
    let mut config = ConfigToml::default();
    config.set_value("notifications.quiet", "true").unwrap();
    let before = toml::to_string(&config).unwrap();
    for (key, value) in [
        ("notifications", "false"),
        ("notifications.quiet", "maybe"),
        ("notifications.threshold_secs", "18446744073709551615"),
        ("notifications.event_sound.events", r#"["bogus"]"#),
        ("notifications.events.unknown", "true"),
        ("notifications.sound_file", ""),
    ] {
        assert!(config.set_value(key, value).is_err(), "{key}");
        assert_eq!(toml::to_string(&config).unwrap(), before);
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, &before).unwrap();
    for update in [
        Update::ThresholdSecs(u64::MAX),
        Update::SoundFile(PathBuf::new()),
        Update::EventSoundEvents(vec!["bogus".into()]),
    ] {
        let mut live = notifications::NotificationsConfig::default();
        let prior = live.clone();
        assert!(update.persist(&path).is_err());
        assert!(live.apply_update(update).is_err());
        assert_eq!(live, prior);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }
    assert!(
        notifications::edit_extras(
            &mut config.extras,
            Key::Quiet,
            Some(toml::Value::String("true".into()))
        )
        .is_err()
    );
    assert_eq!(toml::to_string(&config).unwrap(), before);
}

#[test]
fn notifications_targeted_persistence_preserves_comments_and_leaf_unset() {
    use notifications::{NotificationConfigUpdate as Update, NotificationSetting as Key};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "# operator note\n\"notifications.quiet\" = \"false\"\n[notifications]\nquiet = false # retained\nfuture = 7\n[notifications.events]\ninput-needed = false\n").unwrap();
    Update::parse(Key::Quiet, "true")
        .unwrap()
        .persist(&path)
        .unwrap();
    Update::parse(Key::Sound, "whale")
        .unwrap()
        .persist(&path)
        .unwrap();
    Key::Quiet.unset(&path).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("# operator note"));
    let raw: toml::Value = toml::from_str(&saved).unwrap();
    assert!(!raw.as_table().unwrap().contains_key("notifications.quiet"));
    assert!(raw["notifications"].get("quiet").is_none());
    assert_eq!(raw["notifications"]["sound"].as_str(), Some("whale"));
    assert_eq!(raw["notifications"]["future"].as_integer(), Some(7));
    assert_eq!(
        raw["notifications"]["events"]["input-needed"].as_bool(),
        Some(false)
    );
}

#[test]
fn notifications_legacy_condition_is_fallback_and_explicit_sound_off_wins() {
    let mut config: ConfigToml = toml::from_str(
        "[tui]\nnotification_condition = \"never\"\n[notifications]\ncompletion_sound = \"bell\"\n",
    )
    .unwrap();
    assert_eq!(
        config.get_value("notifications.condition").as_deref(),
        Some("never")
    );
    assert_eq!(
        config.get_value("notifications.sound").as_deref(),
        Some("legacy")
    );
    config
        .set_value("notifications.condition", "always")
        .unwrap();
    config.set_value("notifications.sound", "off").unwrap();
    assert_eq!(
        config.get_value("notifications.condition").as_deref(),
        Some("always")
    );
    config.unset_value("notifications.condition").unwrap();
    assert_eq!(
        config.get_value("notifications.condition").as_deref(),
        Some("never")
    );
    assert_eq!(
        config.get_value("notifications.sound").as_deref(),
        Some("off")
    );
}

#[test]
fn notifications_path_whitespace_and_quotes_round_trip_without_reparsing() {
    let mut config = ConfigToml::default();
    for path in [
        " sound with spaces.wav ",
        "\"quoted-name.wav",
        "folder/normal.wav",
    ] {
        let raw = toml::Value::String(path.into()).to_string();
        config.set_value("notifications.sound_file", &raw).unwrap();
        assert_eq!(
            config.get_value("notifications.sound_file").as_deref(),
            Some(path)
        );
    }
}

#[test]
fn notifications_malformed_parent_and_unknown_root_cannot_erase_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let original = "[notifications]\nevents = false\nquiet = true\n";
    std::fs::write(&path, original).unwrap();
    let update = notifications::NotificationConfigUpdate::parse(
        notifications::NotificationSetting::Event(notifications::NotificationEvent::InputNeeded),
        "false",
    )
    .unwrap();
    assert!(update.persist(&path).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let mut config: ConfigToml = toml::from_str(original).unwrap();
    assert!(config.unset_value("notifications").is_err());
    let before = toml::to_string(&config).unwrap();
    assert!(
        config
            .set_value("notifications.events.input-needed", "false")
            .is_err()
    );
    assert_eq!(toml::to_string(&config).unwrap(), before);
}

#[test]
fn config_table_and_nested_reads_share_redacted_document() {
    let config: ConfigToml = toml::from_str(
        r#"
[tools]
user_input_timeout_seconds = 7
[hooks]
enabled = true
[credentials.service]
value = "fixture-secret-never-display"
[providers.openai]
api_key = "fixture-provider-secret"
"#,
    )
    .unwrap();
    assert!(
        config
            .get_value("tools")
            .unwrap()
            .contains("user_input_timeout_seconds = 7")
    );
    assert_eq!(
        config
            .get_value("tools.user_input_timeout_seconds")
            .as_deref(),
        Some("7")
    );
    assert_eq!(
        config.get_display_value("hooks.enabled").as_deref(),
        Some("true")
    );
    for key in [
        "credentials",
        "credentials.service",
        "credentials.service.value",
        "providers",
    ] {
        let shown = config.get_display_value(key).expect(key);
        assert!(!shown.contains("fixture-secret-never-display"), "{key}");
        assert!(!shown.contains("fixture-provider-secret"), "{key}");
    }
}

#[test]
fn unsupported_nested_config_write_fails_without_mutation() {
    let mut config: ConfigToml =
        toml::from_str("[tools]\nuser_input_timeout_seconds = 7\n").unwrap();
    let before = toml::to_string(&config).unwrap();
    let err = config
        .set_value("tools.user_input_timeout_seconds", "0")
        .unwrap_err();
    assert!(err.to_string().contains("[tools]"));
    assert!(err.to_string().contains("user_input_timeout_seconds"));
    assert_eq!(toml::to_string(&config).unwrap(), before);
}

/// #6516: `output_mode` had no reader and was removed from the typed schema. A
/// config that still carries it must keep loading, and a typed save must keep
/// the user's line rather than silently dropping it or failing on it.
#[test]
fn retired_output_mode_key_still_loads_and_survives_a_typed_save() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join(CONFIG_FILE_NAME);
    fs::write(
        &config_path,
        "output_mode = \"plain\"\nverbosity = \"quiet\"\n\n[providers.deepseek]\nmodel = \"deepseek-v4-flash\"\n",
    )
    .expect("write config");

    let mut store = ConfigStore::load(Some(config_path.clone())).expect("load config store");
    assert_eq!(store.config.verbosity.as_deref(), Some("quiet"));
    assert_eq!(
        store.config.extras.get("output_mode"),
        Some(&toml::Value::String("plain".to_string()))
    );

    store.config.verbosity = Some("concise".to_string());
    store.save().expect("typed save");
    let saved = fs::read_to_string(&config_path).expect("read saved config");
    let reparsed: ConfigToml = toml::from_str(&saved).expect("saved config parses");
    assert_eq!(reparsed.verbosity.as_deref(), Some("concise"));
    assert!(saved.contains("output_mode = \"plain\""), "{saved}");
}

#[test]
fn closed_choice_writes_validate_before_mutation_and_redact_pasted_credentials() {
    let mut config = ConfigToml::default();
    let token = ["sk-live-", "Z7qX4mNb2Vc9Lk3PwR8t"].concat();
    for key in ["approval_policy", "sandbox_mode", "verbosity"] {
        for choice in config_toml_choices(key).unwrap() {
            let value = format!(" {} ", choice.to_ascii_uppercase());
            config
                .set_value(key, &value)
                .expect("reader accepts normalized choice");
            assert_eq!(config.get_value(key).as_deref(), Some(value.as_str()));
        }
        let before = toml::to_string(&config).unwrap();
        for value in ["misspelled-choice", token.as_str()] {
            let error = config.set_value(key, value).expect_err("invalid choice");
            let message = error.to_string();
            assert!(message.contains(key), "{message}");
            assert!(message.contains("fix: codewhale config set"), "{message}");
            assert!(!message.contains(&token), "{message}");
            assert!(message.contains(if value == token { "[redacted]" } else { value }));
            assert_eq!(
                toml::to_string(&config).unwrap(),
                before,
                "{key} changed on refusal"
            );
        }
    }
}

#[test]
fn declared_setting_writes_keep_schema_type_and_refuse_bad_values() {
    let mut config = ConfigToml::default();
    config.set_value("allow_shell", "off").unwrap();
    assert_eq!(config.extras["allow_shell"], toml::Value::Boolean(false));
    config.set_value("max_history", " 250 ").unwrap();
    assert_eq!(config.extras["max_history"], toml::Value::Integer(250));
    config
        .set_value("auto_compact_threshold_percent", "72.5")
        .unwrap();
    assert_eq!(
        config.extras["auto_compact_threshold_percent"],
        toml::Value::Float(72.5)
    );
    config.set_value("tool_collapse", "Expanded").unwrap();
    assert_eq!(
        config.extras["tool_collapse"],
        toml::Value::String("expanded".into())
    );

    let before = toml::to_string(&config).unwrap();
    for (key, value, needle) in [
        ("allow_shell", "flase", "invalid value for 'allow_shell'"),
        ("max_history", "lots", "expected an integer"),
        ("auto_compact_threshold_percent", "NaN", "expected a number"),
        (
            "tool_collapse",
            "sideways",
            "expected one of compact, expanded, calm",
        ),
    ] {
        let err = config.set_value(key, value).unwrap_err();
        assert!(format!("{err:#}").contains(needle), "{key}: {err:#}");
    }
    assert_eq!(
        toml::to_string(&config).unwrap(),
        before,
        "refusals change nothing"
    );

    // `reasoning_effort` keeps its reader's aliases, which the schema's
    // option list does not name; the TUI reader validates them.
    for alias in ["none", "mid", "maximum", "minimum"] {
        config.set_value("reasoning_effort", alias).unwrap();
        assert_eq!(
            config.extras["reasoning_effort"],
            toml::Value::String(alias.into())
        );
    }

    // Undeclared keys keep the string fallthrough; `config set` refuses the
    // ones nothing reads before calling here.
    config.set_value("skills_dir", "/tmp/skills").unwrap();
    assert_eq!(
        config.extras["skills_dir"],
        toml::Value::String("/tmp/skills".into())
    );
}

// ── #6394: legacy top-level `base_url` / `api_key` ────────────────────────

mod legacy_root_upgrade {
    use super::*;

    const V0_10_0_EXAMPLE: &str =
        include_str!("../tests/fixtures/legacy_root/v0_10_0_example.toml");
    const V0_9_9_AUTH_SET: &str =
        include_str!("../tests/fixtures/legacy_root/v0_9_9_auth_set.toml");
    const BASE_URL_SAVE: &str =
        include_str!("../tests/fixtures/legacy_root/config_base_url_save.toml");
    const LITERAL_CUSTOM: &str = include_str!("../tests/fixtures/legacy_root/literal_custom.toml");
    const URL_GUESSED_NIM: &str =
        include_str!("../tests/fixtures/legacy_root/url_guessed_nim.toml");

    const CONFLICT: &str = r#"# keep me
provider = "deepseek"
base_url = "https://root.example.test/v1"
api_key = "sk-root"
verbosity = "normal"

[providers.deepseek]
base_url = "https://table.example.test/v1"
api_key = "sk-table"
"#;

    fn resolve(config: &ConfigToml) -> ResolvedRuntimeOptions {
        config.resolve_runtime_options(&CliRuntimeOverrides::default())
    }

    fn parse(body: &str) -> ConfigToml {
        crate::parse_config_toml(body).expect("fixture parses")
    }

    fn raw_table(path: &Path) -> toml::Table {
        toml::from_str(&fs::read_to_string(path).expect("read config")).expect("config parses")
    }

    fn migrated(body: &str) -> String {
        let mut doc: toml_edit::DocumentMut = body.parse().expect("document");
        crate::legacy_root::apply_to_document(&mut doc, None);
        doc.to_string()
    }

    /// Each fixture is real writer output from an older release. It resolves
    /// the same provider, endpoint and key before and after the file moves.
    #[test]
    fn upgrade_fixtures_resolve_the_same_before_and_after_migrate() {
        let _lock = env_lock();
        let _env = EnvGuard::without_deepseek_runtime_overrides();
        for (name, body, provider, base_url, api_key) in [
            (
                "v0.10.0 example",
                V0_10_0_EXAMPLE,
                ProviderKind::Deepseek,
                "https://api.deepseek.com/beta",
                Some("YOUR_DEEPSEEK_API_KEY"),
            ),
            (
                "v0.9.9 auth set",
                V0_9_9_AUTH_SET,
                ProviderKind::Deepseek,
                DEFAULT_DEEPSEEK_BASE_URL,
                Some("sk-legacy-auth-set"),
            ),
            (
                "/config base_url --save",
                BASE_URL_SAVE,
                ProviderKind::Deepseek,
                "https://proxy.example.test/v1",
                Some("sk-proxy-key"),
            ),
            (
                "literal custom",
                LITERAL_CUSTOM,
                ProviderKind::Custom,
                "http://127.0.0.1:18181/v1",
                Some("sk-literal-custom"),
            ),
            (
                "URL-guessed NIM",
                URL_GUESSED_NIM,
                ProviderKind::NvidiaNim,
                "https://integrate.api.nvidia.com/v1",
                // The top-level key was never NIM's (the TUI only sent it to
                // DeepSeek); it stays DeepSeek's.
                None,
            ),
        ] {
            let before = parse(body);
            for key in ["base_url", "api_key", "baseUrl", "apiKey"] {
                assert!(
                    !before.extras.contains_key(key),
                    "{name}: {key} leaked into extras"
                );
            }
            let after_body = migrated(body);
            let after = parse(&after_body);
            assert!(
                !crate::legacy_root::has_legacy_root_keys(
                    &toml::from_str(&after_body).expect("migrated file parses")
                ),
                "{name}: legacy keys left after migrate"
            );
            for config in [&before, &after] {
                let resolved = resolve(config);
                assert_eq!(resolved.provider, provider, "{name}");
                assert_eq!(resolved.base_url.trim_end_matches('/'), base_url, "{name}");
                assert_eq!(resolved.api_key.as_deref(), api_key, "{name}");
            }
        }
    }

    #[test]
    fn canonicalizing_keeps_value_types_in_unrelated_keys() {
        let config = parse(
            "base_url = \"https://proxy.example.test/v1\"\nstarted_at = 1979-05-27T07:32:00Z\n",
        );
        assert!(
            matches!(
                config.extras.get("started_at"),
                Some(toml::Value::Datetime(_))
            ),
            "{:?}",
            config.extras
        );
    }

    #[test]
    fn the_literal_custom_route_keeps_its_model() {
        let _lock = env_lock();
        let _env = EnvGuard::without_deepseek_runtime_overrides();
        assert_eq!(resolve(&parse(LITERAL_CUSTOM)).model, "my-local-model");
    }

    #[test]
    fn the_shipped_nim_profile_key_now_reaches_nim() {
        let root = {
            let mut table: toml::Table = toml::from_str(V0_10_0_EXAMPLE).unwrap();
            crate::legacy_root::apply_to_table(&mut table);
            table
        };
        let profile = root["profiles"]["nvidia-nim"].as_table().unwrap();
        assert_eq!(
            profile["providers"]["nvidia_nim"]["api_key"].as_str(),
            Some("YOUR_NVIDIA_API_KEY")
        );
        assert_eq!(
            root["profiles"]["work"]["providers"]["deepseek"]["base_url"].as_str(),
            Some("https://api.deepseek.com/beta")
        );
    }

    #[test]
    fn migrate_keeps_comments_and_puts_simple_keys_before_tables() {
        let after = migrated(V0_9_9_AUTH_SET);
        assert!(after.starts_with("# codewhale Configuration\n"), "{after}");
        assert!(after.contains("# Thinking mode"), "{after}");
        assert!(after.contains("reasoning_effort = \"max\""), "{after}");
        let table_at = after.find("[providers.deepseek]").expect("table written");
        let scalar_at = after.find("reasoning_effort").unwrap();
        assert!(scalar_at < table_at, "{after}");
        assert!(parse(&after).providers.deepseek.api_key.is_some());
    }

    #[test]
    fn a_save_moves_the_keys_once_with_a_credential_free_backup_and_one_notice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CONFIG_FILE_NAME);
        fs::write(&path, V0_9_9_AUTH_SET).unwrap();

        let mut store = ConfigStore::load(Some(path.clone())).unwrap();
        assert!(store.legacy_root_migration().has_pending_moves());
        store.config.set_value("verbosity", "normal").unwrap();
        store.save().unwrap();

        let raw = raw_table(&path);
        assert!(!crate::legacy_root::has_legacy_root_keys(&raw));
        assert_eq!(
            raw["providers"]["deepseek"]["api_key"].as_str(),
            Some("sk-legacy-auth-set")
        );
        let backup = crate::legacy_root_backup_path(&path).unwrap();
        let backup_body = fs::read_to_string(&backup).expect("backup written");
        assert!(
            backup_body.contains("# codewhale Configuration"),
            "{backup_body}"
        );
        assert!(
            !backup_body.contains("sk-legacy-auth-set"),
            "backup is credential-free"
        );
        let backup_path = backup.display().to_string();
        let ours: Vec<String> = crate::legacy_root::take_notices()
            .into_iter()
            .filter(|notice| notice.contains(&backup_path))
            .collect();
        assert_eq!(ours.len(), 1, "{ours:?}");
        assert!(ours[0].contains("[providers.deepseek]"), "{ours:?}");

        // A second save changes nothing and says nothing.
        let body = fs::read_to_string(&path).unwrap();
        let mut store = ConfigStore::load(Some(path.clone())).unwrap();
        assert!(store.legacy_root_migration().is_empty());
        store.save().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), body);
        assert!(
            !crate::legacy_root::take_notices()
                .iter()
                .any(|notice| notice.contains(&backup_path))
        );
    }

    #[test]
    fn a_conflict_survives_an_unrelated_save_and_ends_on_an_explicit_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CONFIG_FILE_NAME);
        fs::write(&path, CONFLICT).unwrap();

        let mut store = ConfigStore::load(Some(path.clone())).unwrap();
        assert_eq!(
            store.legacy_root_migration().unresolved_conflicts().count(),
            2
        );
        store.config.set_value("verbosity", "normal").unwrap();
        store.save().unwrap();
        let raw = raw_table(&path);
        assert_eq!(
            raw["base_url"].as_str(),
            Some("https://root.example.test/v1")
        );
        assert_eq!(raw["api_key"].as_str(), Some("sk-root"));
        assert_eq!(
            raw["providers"]["deepseek"]["base_url"].as_str(),
            Some("https://table.example.test/v1")
        );
        assert_eq!(
            raw["providers"]["deepseek"]["api_key"].as_str(),
            Some("sk-table")
        );
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .starts_with("# keep me\n")
        );

        // `auth set` style: the user writes the key; that ends the key
        // conflict and leaves the endpoint pair alone.
        let mut store = ConfigStore::load(Some(path.clone())).unwrap();
        store
            .config
            .set_value("providers.deepseek.api_key", "sk-chosen")
            .unwrap();
        store.save().unwrap();
        let raw = raw_table(&path);
        assert!(raw.get("api_key").is_none());
        assert_eq!(
            raw["providers"]["deepseek"]["api_key"].as_str(),
            Some("sk-chosen")
        );
        assert_eq!(
            raw["base_url"].as_str(),
            Some("https://root.example.test/v1")
        );
    }

    #[test]
    fn a_targeted_write_moves_keys_and_keeps_conflicts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CONFIG_FILE_NAME);
        fs::write(&path, CONFLICT.replace("verbosity", "log_level")).unwrap();
        crate::mutate_config_document(&path, |doc| {
            crate::set_config_document_value(doc, &["verbosity"], "high")
        })
        .unwrap();
        let raw = raw_table(&path);
        assert_eq!(
            raw["base_url"].as_str(),
            Some("https://root.example.test/v1")
        );
        assert_eq!(raw["api_key"].as_str(), Some("sk-root"));

        fs::write(&path, BASE_URL_SAVE).unwrap();
        crate::mutate_config_document(&path, |doc| {
            crate::set_config_document_value(doc, &["verbosity"], "high")
        })
        .unwrap();
        let raw = raw_table(&path);
        assert!(!crate::legacy_root::has_legacy_root_keys(&raw));
        assert_eq!(
            raw["providers"]["deepseek"]["base_url"].as_str(),
            Some("https://proxy.example.test/v1")
        );
        assert_eq!(
            raw["providers"]["openrouter"]["api_key"].as_str(),
            Some("sk-or-other-vendor")
        );
    }

    #[test]
    fn config_migrate_dry_run_writes_nothing_and_a_second_run_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CONFIG_FILE_NAME);
        fs::write(&path, CONFLICT).unwrap();

        let preview = crate::preview_legacy_root_config(&path, None).unwrap();
        assert_eq!(preview.unresolved_conflicts().count(), 2);
        assert_eq!(fs::read_to_string(&path).unwrap(), CONFLICT);

        // No `--prefer`: a conflict-only file is left exactly as it is.
        let (receipt, backup) = crate::migrate_legacy_root_config(&path, None).unwrap();
        assert!(!receipt.changes_file());
        assert!(backup.is_none());
        assert_eq!(fs::read_to_string(&path).unwrap(), CONFLICT);

        let (receipt, backup) = crate::migrate_legacy_root_config(
            &path,
            Some(crate::legacy_root::LegacyRootPrefer::Table),
        )
        .unwrap();
        assert!(receipt.changes_file());
        let backup = backup.expect("backup before resolving");
        assert!(!fs::read_to_string(backup).unwrap().contains("sk-"));
        let raw = raw_table(&path);
        assert!(!crate::legacy_root::has_legacy_root_keys(&raw));
        assert_eq!(
            raw["providers"]["deepseek"]["api_key"].as_str(),
            Some("sk-table")
        );

        let (receipt, _) = crate::migrate_legacy_root_config(&path, None).unwrap();
        assert!(receipt.is_empty());
    }

    #[test]
    fn config_set_base_url_writes_the_active_providers_table() {
        let mut config = parse("provider = \"openai\"\n");
        config
            .set_value("base_url", "https://gateway.example.test/v1")
            .unwrap();
        assert_eq!(
            config.providers.openai.base_url.as_deref(),
            Some("https://gateway.example.test/v1")
        );
        assert!(!config.extras.contains_key("base_url"));
        assert_eq!(
            config.get_value("base_url").as_deref(),
            Some("https://gateway.example.test/v1")
        );
        assert_eq!(
            config.root_alias_key("base_url").as_deref(),
            Some("providers.openai.base_url")
        );
        config.unset_value("base_url").unwrap();
        assert!(config.providers.openai.base_url.is_none());
    }

    #[test]
    fn provider_field_writers_no_longer_write_top_level_twins() {
        let mut config = ConfigToml::default();
        config
            .set_value(
                "providers.deepseek.base_url",
                "https://proxy.example.test/v1",
            )
            .unwrap();
        config
            .set_value("providers.deepseek.api_key", "sk-deepseek")
            .unwrap();
        let rendered = toml::to_string(&config).unwrap();
        let table: toml::Table = toml::from_str(&rendered).unwrap();
        assert!(
            !crate::legacy_root::has_legacy_root_keys(&table),
            "{rendered}"
        );
    }

    #[test]
    fn project_config_base_url_never_reaches_the_runtime() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join(CODEWHALE_APP_DIR);
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join(CONFIG_FILE_NAME),
            "approval_policy = \"never\"\nbase_url = \"https://attacker.example.test/v1\"\n",
        )
        .unwrap();
        let project_config = crate::load_project_config(dir.path()).expect("project config");
        assert!(!project_config.extras.contains_key("base_url"));
        // Project config is read for approval/sandbox posture only; its moved
        // endpoint sits in a table nothing merges into the user's routing.
        assert_eq!(project_config.approval_policy.as_deref(), Some("never"));
    }
}

#[test]
fn typed_save_round_trips_every_builtin_provider_selector() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(CONFIG_FILE_NAME);
    let mismatched: Vec<String> = provider::all_providers()
        .iter()
        .map(|entry| entry.kind())
        .filter_map(|kind| {
            let serialized = toml::Value::try_from(kind).expect("serialize provider kind");
            (serialized.as_str() != Some(kind.as_str()))
                .then(|| format!("{} -> {serialized}", kind.as_str()))
        })
        .collect();
    assert!(
        mismatched.is_empty(),
        "serde spelling must be the canonical id: {mismatched:?}"
    );
    for entry in provider::all_providers() {
        let kind = entry.kind();
        fs::write(&path, format!("provider = \"{}\"\n", entry.id())).expect("write config");
        let Ok(mut store) = ConfigStore::load(Some(path.clone())) else {
            // A retired tombstone may refuse to load; it must not be written.
            continue;
        };
        store
            .config
            .set_value("verbosity", "concise")
            .expect("set verbosity");
        store.save().expect("typed save");
        let reloaded = ConfigStore::load(Some(path.clone()))
            .unwrap_or_else(|err| panic!("reload after saving {}: {err:#}", entry.id()));
        assert_eq!(reloaded.config.provider, kind, "{}", entry.id());
    }
}

#[test]
fn legacy_siliconflow_cn_spelling_loads_and_is_repaired_on_save() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(CONFIG_FILE_NAME);
    fs::write(&path, "provider = \"siliconflow-c-n\"\n").expect("write config");
    let mut store = ConfigStore::load(Some(path.clone())).expect("load legacy spelling");
    assert_eq!(store.config.provider, ProviderKind::SiliconflowCN);
    store
        .config
        .set_value("verbosity", "concise")
        .expect("set verbosity");
    store.save().expect("typed save");
    let body = fs::read_to_string(&path).expect("read config");
    assert!(body.contains("provider = \"siliconflow-CN\""), "{body}");
}

#[test]
fn typed_save_keeps_a_providers_section_holding_only_a_legacy_kind_table() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(CONFIG_FILE_NAME);
    for entry in provider::all_providers() {
        let key = entry.provider_config_key();
        fs::write(&path, format!("[providers.{key}]\nmodel = \"m-x\"\n")).expect("write config");
        let Ok(mut store) = ConfigStore::load(Some(path.clone())) else {
            continue;
        };
        store
            .config
            .set_value("verbosity", "concise")
            .expect("set verbosity");
        store.save().expect("typed save");
        let body = fs::read_to_string(&path).expect("read config");
        assert!(
            body.contains("m-x"),
            "[providers.{key}] was dropped by a typed save:\n{body}"
        );
    }
}

#[test]
fn typed_save_keeps_runtime_owned_keys_in_typed_sub_tables() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(CONFIG_FILE_NAME);
    fs::write(
        &path,
        "[snapshots]\nmax_workspace_gb = 8\n\n[skills]\nscan_codewhale_only = true\n\n\
         [network]\nfuture_network_key = \"kept\"\n\n[lsp]\nfuture_lsp_key = 3\n",
    )
    .expect("write config");
    let mut store = ConfigStore::load(Some(path.clone())).expect("load config");
    store
        .config
        .set_value("verbosity", "concise")
        .expect("set verbosity");
    store.save().expect("typed save");
    let saved: toml::Table = toml::from_str(&fs::read_to_string(&path).expect("read config"))
        .expect("parse saved config");
    assert_eq!(
        saved["snapshots"].get("max_workspace_gb"),
        Some(&toml::Value::Integer(8))
    );
    assert_eq!(
        saved["skills"].get("scan_codewhale_only"),
        Some(&toml::Value::Boolean(true))
    );
    assert_eq!(
        saved["network"].get("future_network_key"),
        Some(&toml::Value::String("kept".to_string()))
    );
    assert_eq!(
        saved["lsp"].get("future_lsp_key"),
        Some(&toml::Value::Integer(3))
    );
}

const NAMED_CUSTOM_WITH_LEGACY_CUSTOM: &str = r#"provider = "acme"

[providers.acme]
kind = "openai-compatible"
base_url = "https://acme.example/v1"
model = "acme-model"
context_window = CONTEXT_WINDOW

[providers.custom]
base_url = "https://legacy-custom.example/v1"
model = "legacy-model"
api_key = "legacy-custom-key-1234567890"
"#;

#[test]
fn named_custom_provider_with_invalid_table_is_rejected_on_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(CONFIG_FILE_NAME);
    fs::write(
        &path,
        NAMED_CUSTOM_WITH_LEGACY_CUSTOM.replace("CONTEXT_WINDOW", "\"big-secret-value\""),
    )
    .expect("write config");
    let err = ConfigStore::load(Some(path)).expect_err("invalid named table must not load");
    let message = format!("{err:#}");
    assert!(message.contains("[providers.acme]"), "{message}");
    assert!(!message.contains("big-secret-value"), "{message}");
}

#[test]
fn named_custom_provider_never_resolves_to_legacy_custom_table() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(CONFIG_FILE_NAME);
    fs::write(
        &path,
        NAMED_CUSTOM_WITH_LEGACY_CUSTOM.replace("CONTEXT_WINDOW", "128000"),
    )
    .expect("write config");
    let mut store = ConfigStore::load(Some(path)).expect("load valid named table");
    let resolved = store
        .config
        .resolve_runtime_options(&CliRuntimeOverrides::default());
    assert_eq!(resolved.base_url, "https://acme.example/v1");

    // The table turns invalid after binding: resolution fails closed instead
    // of reading `[providers.custom]`.
    store
        .config
        .providers
        .extras
        .get_mut("acme")
        .and_then(toml::Value::as_table_mut)
        .expect("acme table")
        .insert("context_window".to_string(), toml::Value::from("big"));
    let resolved = store
        .config
        .resolve_runtime_options(&CliRuntimeOverrides::default());
    assert!(
        !resolved.base_url.contains("legacy-custom"),
        "{}",
        resolved.base_url
    );
    assert_ne!(resolved.model, "legacy-model");
    assert_ne!(
        resolved.api_key.as_deref(),
        Some("legacy-custom-key-1234567890")
    );
}

#[test]
fn deepseek_scoped_headers_and_model_stay_out_of_root_keys() -> Result<()> {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let mut config = ConfigToml::default();
    config.set_value("providers.deepseek.http_headers", "X-Gateway-Key=ds-only")?;
    config.set_value("providers.deepseek.model", "deepseek-v4-pro")?;
    assert!(config.http_headers.is_empty());
    assert_eq!(config.default_text_model, None);

    config.set_value("provider", "openrouter")?;
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    assert!(
        !resolved.http_headers.contains_key("X-Gateway-Key"),
        "{:?}",
        resolved.http_headers
    );

    // Root values the user set apart survive unsetting the provider leg,
    // while a mirrored copy left by an earlier release is cleared with it.
    config.set_value("http_headers", "X-Everywhere=root")?;
    config.set_value("default_text_model", "root-model")?;
    config.unset_value("providers.deepseek.http_headers")?;
    config.unset_value("providers.deepseek.model")?;
    assert_eq!(
        config.http_headers.get("X-Everywhere").map(String::as_str),
        Some("root")
    );
    assert_eq!(config.default_text_model.as_deref(), Some("root-model"));

    config.set_value("providers.deepseek.http_headers", "X-Everywhere=root")?;
    config.unset_value("providers.deepseek.http_headers")?;
    assert!(config.http_headers.is_empty());
    Ok(())
}

#[test]
fn key_and_cookie_names_are_classified_as_sensitive() {
    for name in [
        "Ocp-Apim-Subscription-Key",
        "Cookie",
        "Set-Cookie",
        "secret_key",
        "access_key",
        "private_key",
        "providers.acme.secret_key",
        "X-Api-Key",
        "api_key",
    ] {
        assert!(is_sensitive_config_key(name), "{name}");
        assert!(is_upstream_auth_header(name), "{name}");
    }
    for name in [
        "api_key_env",
        "public_key",
        "base_url",
        "model",
        "X-Model-Provider-Id",
    ] {
        assert!(!is_sensitive_config_key(name), "{name}");
    }

    let mut config = ConfigToml::default();
    config.http_headers.insert(
        "Ocp-Apim-Subscription-Key".to_string(),
        "apim-value-0123456789abcdef".to_string(),
    );
    let listed = config.list_values();
    let shown = listed.get("http_headers").expect("headers listed");
    assert!(!shown.contains("apim-value-0123456789abcdef"), "{shown}");
}

#[test]
fn config_backup_strips_every_credential_named_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(CONFIG_FILE_NAME);
    let original = r#"chatgpt_access_token = "root-access-token-value"
model = "deepseek-v4-pro"

[lifecycle_outbox]
path = "/tmp/outbox.jsonl"
webhook_token = "webhook-token-value"

[providers.openrouter]
api_key = "provider-api-key-value"
auth_mode = "api_key"

[providers.openrouter.http_headers]
Authorization = "Bearer header-bearer-value"
X-Title = "kept-title"

[future_section]
profiles = [{ secret_key = "inline-array-secret-value", label = "kept-label" }]
"#;
    fs::write(&path, original).expect("seed config");

    let mut store = ConfigStore::load(Some(path.clone())).expect("load config");
    store.config.model = Some("deepseek-v4-flash".to_string());
    store.save().expect("changed save");

    let backup = fs::read_to_string(config_backup_path(&path)).expect("read backup");
    for secret in [
        "root-access-token-value",
        "webhook-token-value",
        "provider-api-key-value",
        "header-bearer-value",
        "inline-array-secret-value",
    ] {
        assert!(
            !backup.contains(secret),
            "{secret} left in backup:\n{backup}"
        );
    }
    for kept in [
        "auth_mode = \"api_key\"",
        "kept-title",
        "kept-label",
        "model = \"deepseek-v4-pro\"",
    ] {
        assert!(
            backup.contains(kept),
            "{kept} missing from backup:\n{backup}"
        );
    }
}

#[test]
fn blank_or_unrecognized_env_overrides_do_not_shadow_config_or_legacy_vars() {
    let _lock = env_lock();
    let _env = EnvGuard::without_deepseek_runtime_overrides();
    let names = [
        "CODEWHALE_AUTH_MODE",
        "DEEPSEEK_AUTH_MODE",
        "CODEWHALE_SANDBOX_MODE",
        "DEEPSEEK_SANDBOX_MODE",
    ];
    let saved: Vec<(&str, Option<OsString>)> = names
        .iter()
        .map(|name| (*name, env::var_os(name)))
        .collect();
    // SAFETY: env mutation is serialized by `env_lock` and restored below.
    unsafe {
        env::set_var("CODEWHALE_AUTH_MODE", "");
        env::remove_var("DEEPSEEK_AUTH_MODE");
        env::set_var("CODEWHALE_SANDBOX_MODE", " ");
        env::set_var("DEEPSEEK_SANDBOX_MODE", "workspace-write");
        env::set_var("CODEWHALE_PROVIDER", "openroutr");
        env::set_var("DEEPSEEK_PROVIDER", "openrouter");
    }

    let mut config = ConfigToml {
        auth_mode: Some("api_key".to_string()),
        ..ConfigToml::default()
    };
    config.provider = ProviderKind::Deepseek;
    let resolved = config.resolve_runtime_options(&CliRuntimeOverrides::default());
    let env_overrides = EnvRuntimeOverrides::load();

    unsafe {
        for (name, value) in saved {
            match value {
                Some(value) => env::set_var(name, value),
                None => env::remove_var(name),
            }
        }
    }

    assert_eq!(resolved.auth_mode.as_deref(), Some("api_key"));
    assert_eq!(
        env_overrides.sandbox_mode.as_deref(),
        Some("workspace-write")
    );
    assert_eq!(resolved.provider, ProviderKind::Openrouter);
    assert!(matches!(
        resolved.provider_source,
        ProviderSource::Env("DEEPSEEK_PROVIDER")
    ));
}

#[cfg(unix)]
#[test]
fn failed_legacy_state_copy_leaves_no_partial_primary() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let legacy = dir.path().join("legacy-sessions");
    let primary = dir.path().join("primary").join("sessions");
    fs::create_dir_all(legacy.join("nested")).expect("legacy dir");
    fs::create_dir_all(primary.parent().expect("parent")).expect("primary root");
    fs::write(legacy.join("a.json"), b"a").expect("file a");
    let unreadable = legacy.join("nested").join("b.json");
    fs::write(&unreadable, b"b").expect("file b");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).expect("chmod");
    let readable_anyway = fs::read(&unreadable).is_ok();

    let result = copy_dir_into_place(&legacy, &primary);
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).expect("chmod back");
    if readable_anyway {
        // Running as root: permissions cannot force the failure.
        return;
    }
    assert!(result.is_err());
    assert!(
        !primary.exists(),
        "a failed copy must not create the primary"
    );
    assert_eq!(
        fs::read_dir(primary.parent().unwrap()).unwrap().count(),
        0,
        "a failed copy removes only its owned staging directory"
    );

    copy_dir_into_place(&legacy, &primary).expect("retry succeeds");
    assert_eq!(fs::read(primary.join("a.json")).expect("a"), b"a");
    assert_eq!(
        fs::read(primary.join("nested").join("b.json")).expect("b"),
        b"b"
    );
}

#[test]
fn legacy_state_copy_preserves_another_attempts_staging_directory() {
    let root = tempfile::tempdir().expect("tempdir");
    let legacy = root.path().join("legacy");
    let primary = root.path().join("sessions");
    let other_staging = root.path().join(".sessions.migrating");
    fs::create_dir_all(&legacy).expect("legacy");
    fs::create_dir_all(&other_staging).expect("other attempt");
    fs::write(legacy.join("session.json"), b"source").expect("source");
    fs::write(other_staging.join("in-flight.json"), b"other attempt").expect("marker");

    copy_dir_into_place(&legacy, &primary).expect("copy");
    assert_eq!(fs::read(primary.join("session.json")).unwrap(), b"source");
    assert_eq!(
        fs::read(other_staging.join("in-flight.json")).unwrap(),
        b"other attempt"
    );
    assert!(legacy.join("session.json").exists());
    let siblings: Vec<_> = fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(siblings.len(), 3, "only this attempt's staging is removed");
}

#[test]
fn legacy_state_copy_preserves_a_completed_primary() {
    let root = tempfile::tempdir().expect("tempdir");
    let legacy = root.path().join("legacy");
    let primary = root.path().join("sessions");
    fs::create_dir_all(&legacy).expect("legacy");
    fs::create_dir_all(&primary).expect("completed primary");
    fs::write(legacy.join("session.json"), b"legacy").expect("source");
    fs::write(primary.join("session.json"), b"newer").expect("primary");

    assert!(copy_dir_into_place(&legacy, &primary).is_err());
    assert_eq!(fs::read(primary.join("session.json")).unwrap(), b"newer");
    assert_eq!(fs::read(legacy.join("session.json")).unwrap(), b"legacy");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
}

#[cfg(unix)]
#[test]
fn ensure_state_dir_keeps_legacy_authoritative_until_migration_succeeds() {
    use std::os::unix::fs::PermissionsExt;
    let _lock = env_lock();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let state_env = StateDirEnv::install(unique);
    fs::create_dir_all(state_env.legacy("sessions")).expect("legacy dir");
    fs::write(state_env.legacy("sessions").join("old.json"), b"legacy").expect("legacy file");
    let root = state_env.home.join(CODEWHALE_APP_DIR);
    fs::create_dir_all(&root).expect("codewhale root");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).expect("chmod root");
    let root_writable = fs::write(root.join("probe"), b"").is_ok();

    let first = ensure_state_dir_with_migration("sessions");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("chmod back");
    if !root_writable {
        let (dir, migration) = first.expect("ensure_state_dir falls back to legacy");
        assert_eq!(dir, state_env.legacy("sessions"));
        assert!(migration.is_none());
        assert!(!state_env.primary("sessions").exists());
        assert_eq!(
            resolve_state_dir("sessions").expect("resolve"),
            state_env.legacy("sessions")
        );

        // Once the primary root is writable again the migration retries.
        let (dir, migration) = ensure_state_dir_with_migration("sessions").expect("retry");
        assert_eq!(dir, state_env.primary("sessions"));
        assert!(migration.is_some());
        assert_eq!(
            fs::read(state_env.primary("sessions").join("old.json")).expect("migrated"),
            b"legacy"
        );
    }
    let _ = fs::remove_dir_all(&state_env.home);
}

#[test]
fn stream_settings_are_typed_nested_and_fail_without_mutation() {
    let mut config = ConfigToml::default();
    for (key, value) in [
        ("stream.open_timeout_secs", "120"),
        ("stream.force_http1", "on"),
        ("stream.tcp_keepalive_secs", "0"),
    ] {
        config.set_value(key, value).unwrap();
    }
    assert_eq!(
        config.extras["stream"]["open_timeout_secs"].as_integer(),
        Some(120)
    );
    assert_eq!(config.extras["stream"]["force_http1"].as_bool(), Some(true));
    let before = toml::to_string(&config).unwrap();
    for (key, value) in [
        ("stream.max_resumes", "-1"),
        ("stream.max_resumes", "4294967296"),
        ("stream.force_http1", "flase"),
        ("stream.tcp_keepalive_secs", "1.5"),
        ("stream.open_timout_secs", "45"),
    ] {
        assert!(config.set_value(key, value).is_err(), "{key}");
        assert_eq!(toml::to_string(&config).unwrap(), before);
    }
    let mut reloaded: ConfigToml = toml::from_str(&before).unwrap();
    reloaded.unset_value("stream.force_http1").unwrap();
    assert!(reloaded.extras["stream"].get("force_http1").is_none());
    assert_eq!(
        reloaded.extras["stream"]["open_timeout_secs"].as_integer(),
        Some(120)
    );
    assert_eq!(
        reloaded.extras["stream"]["tcp_keepalive_secs"].as_integer(),
        Some(0)
    );
}

#[test]
fn config_backup_shared_vocabulary_preserves_safe_values_and_comments_on_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(CONFIG_FILE_NAME);
    let canonical = "model = \"current-model\"\n";
    fs::write(&path, canonical).expect("canonical config");
    let raw = r#"# ordinary header stays
# privateKey = "comment-s10-synthetic"
model = "fixture-model" # clientSecret=inline-s10-synthetic
max_tokens = 4096 # ordinary inline stays
public_key = "public-value"
endpoint_key = "endpoint-label"
base_url = "https://api.example.com"
# ordinary URL https://api.example.com
[providers.fixture] # cookie=table-inline-s10-synthetic
privateKey = "private-s10-synthetic"
clientSecret = "client-s10-synthetic"
api_key_env = "SYNTHETIC_KEY_ENV"
auth_mode = "api_key"
headers = { "Set-Cookie" = "cookie-s10-synthetic", "X-Trace" = "kept-trace" }
[future_section]
profiles = [{ accessToken = "access-s10-synthetic", label = "kept-profile" }]
args = ["--flag", "clientSecret=array-s10-synthetic", "https://api.example.com"]
webhook_url = "https://hooks.example.com/?sas=url-s10-synthetic"
# trailing ordinary comment stays
"#;
    let backup_path = config_backup_path(&path);
    fs::write(&backup_path, raw).expect("seed historical backup");
    scrub_plaintext_api_keys_from_config_backup(&path).expect("scrub persisted backup");
    let backup = fs::read_to_string(&backup_path).expect("persisted backup");
    for marker in [
        "comment-s10-synthetic",
        "inline-s10-synthetic",
        "table-inline-s10-synthetic",
        "private-s10-synthetic",
        "client-s10-synthetic",
        "cookie-s10-synthetic",
        "access-s10-synthetic",
        "array-s10-synthetic",
        "url-s10-synthetic",
    ] {
        assert!(
            !backup.contains(marker),
            "credential survived in persisted backup"
        );
    }
    for kept in [
        "ordinary header stays",
        "ordinary inline stays",
        "trailing ordinary comment stays",
        "max_tokens = 4096",
        "public-value",
        "endpoint-label",
        "https://api.example.com",
        "ordinary URL",
        "SYNTHETIC_KEY_ENV",
        "auth_mode",
        "kept-trace",
        "kept-profile",
        "--flag",
    ] {
        assert!(backup.contains(kept), "ordinary backup data lost: {kept}");
    }
    backup
        .parse::<toml_edit::DocumentMut>()
        .expect("still valid TOML");
    assert_eq!(fs::read_to_string(&path).unwrap(), canonical);
}

#[test]
fn config_dump_shared_vocabulary_redacts_camel_case() {
    for key in [
        "accessToken",
        "clientSecret",
        "privateKey",
        "refreshToken",
        "Set-Cookie",
        "sas",
        "Ocp-Apim-Subscription-Key",
    ] {
        assert!(is_sensitive_config_key(key), "{key}");
        assert!(
            is_sensitive_config_key(&format!("providers.fixture.{key}")),
            "{key}"
        );
    }
    for key in [
        "max_tokens",
        "token_budget",
        "api_key_source",
        "authMode",
        "publicKey",
        "endpoint_key",
    ] {
        assert!(!is_sensitive_config_key(key), "{key}");
    }
    let value: toml::Value = toml::from_str(
        r#"privateKey = "private-s10-synthetic"
clientSecret = "client-s10-synthetic"
model = "fixture-model"
"#,
    )
    .unwrap();
    let shown = redact_toml_value_for_display("providers.fixture", &value);
    assert!(!shown.contains("private-s10-synthetic"));
    assert!(!shown.contains("client-s10-synthetic"));
    assert!(shown.contains("fixture-model"));
}
