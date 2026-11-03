use super::*;

use crate::test_support::EnvVarGuard;

use crate::test_support::env_scope_ticket;

use crate::test_support::join_env_scope;

use crate::test_support::lock_test_env;

use std::collections::HashMap;

use std::env;

use std::ffi::OsString;

use std::sync::mpsc;

use std::time::Duration;

use std::time::SystemTime;

use super::test_support::*;
use std::time::UNIX_EPOCH;

#[test]
fn requirements_check_implicit_approval_and_sandbox_defaults() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("requirements.toml");
    let mut config = Config {
        requirements_path: Some(path.display().to_string()),
        ..Config::default()
    };

    fs::write(&path, "allowed_approval_policies = ['never']\n").unwrap();
    let error = apply_requirements(&mut config).expect_err("default approval must be checked");
    assert!(error.to_string().contains("approval_policy 'on-request'"));
    config.approval_policy = Some("never".into());
    apply_requirements(&mut config).expect("explicit allowed policy");

    fs::write(&path, "allowed_sandbox_modes = ['read-only']\n").unwrap();
    let error = apply_requirements(&mut config).expect_err("default sandbox must be checked");
    assert!(error.to_string().contains("sandbox_mode 'workspace-write'"));
    config.sandbox_mode = Some("read-only".into());
    apply_requirements(&mut config).expect("explicit allowed sandbox");

    config.approval_policy = None;
    config.sandbox_mode = None;
    fs::write(
        &path,
        "allowed_approval_policies = ['ON-REQUEST']\nallowed_sandbox_modes = ['WORKSPACE-WRITE']\n",
    )
    .unwrap();
    apply_requirements(&mut config).expect("allowed implicit defaults");
    config.yolo = Some(true);
    apply_requirements(&mut config).expect("managed approvals prevent the YOLO override");
    fs::write(&path, "allowed_sandbox_modes = ['workspace-write']\n").unwrap();
    apply_requirements(&mut config).expect("managed sandbox prevents the YOLO override");
    config.approval_policy = Some("full-access".into());
    let error =
        apply_requirements(&mut config).expect_err("full access changes the sandbox default");
    assert!(
        error
            .to_string()
            .contains("sandbox_mode 'danger-full-access'")
    );
}

#[test]
fn requirements_accept_equivalent_approval_aliases() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("requirements.toml");
    let mut config = Config {
        requirements_path: Some(path.display().to_string()),
        ..Config::default()
    };
    for alias in ["untrusted", "ask", "suggest", "ON-REQUEST"] {
        fs::write(&path, format!("allowed_approval_policies = ['{alias}']\n")).unwrap();
        for policy in [None, Some("on-request"), Some("ask")] {
            config.approval_policy = policy.map(str::to_string);
            apply_requirements(&mut config).expect("equivalent approval mode is allowed");
        }
        config.approval_policy = Some("full-access".into());
        assert!(
            apply_requirements(&mut config).is_err(),
            "{alias} must not allow bypass"
        );
    }
    fs::write(&path, "allowed_approval_policies = ['unknown']\n").unwrap();
    config.approval_policy = None;
    assert!(
        apply_requirements(&mut config).is_err(),
        "unknown is not the default"
    );
    config.approval_policy = Some("other-unknown".into());
    assert!(
        apply_requirements(&mut config).is_err(),
        "unknown modes must not compare equal"
    );
    config.approval_policy = Some("UNKNOWN".into());
    apply_requirements(&mut config).expect("unparsed policies retain literal comparison");
}

#[test]
fn legacy_hosted_ollama_migration_keeps_the_existing_table_and_endpoint() {
    let _lock = lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let _overrides: Vec<_> = [
        "CODEWHALE_CONFIG_PATH",
        "DEEPSEEK_CONFIG_PATH",
        "CODEWHALE_PROVIDER",
        "DEEPSEEK_PROVIDER",
        "CODEWHALE_MODEL",
        "DEEPSEEK_MODEL",
        "DEEPSEEK_DEFAULT_TEXT_MODEL",
        "OLLAMA_MODEL",
        "OLLAMA_CLOUD_MODEL",
        "OLLAMA_BASE_URL",
        "OLLAMA_CLOUD_BASE_URL",
    ]
    .into_iter()
    .map(EnvVarGuard::remove)
    .collect();
    let path = home.path().join("config.toml");
    fs::write(&path, "provider = 'ollama'\n[providers.ollama]\nbase_url = 'https://ollama.com/v1'\nmodel = 'old-model'\n").unwrap();
    fs::write(
        home.path().join("settings.toml"),
        "default_provider = 'ollama'\n[provider_models]\nollama-cloud = 'remembered-model'\n",
    )
    .unwrap();
    let before = Config::load(None, None).unwrap();
    assert_eq!(
        before.active_provider_identity().unwrap().provider,
        ProviderKind::OllamaCloud
    );
    assert_eq!(before.default_model(), "remembered-model");
    crate::config_persistence::persist_root_bool_key(Some(&path), "allow_shell", false).unwrap();
    let after = Config::load(None, None).unwrap();
    assert_eq!(
        after.active_provider_identity().unwrap().provider,
        ProviderKind::OllamaCloud
    );
    assert_eq!(after.default_model(), "remembered-model");
    assert_eq!(after.active_route_base_url(), "https://ollama.com/v1");
    assert_eq!(
        after.active_provider_identity().unwrap().persisted_id(),
        Some("ollama")
    );
    let doc: toml::Value = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(doc["provider"].as_str(), Some("ollama"));
    assert_eq!(
        doc["providers"]["ollama"]["model"].as_str(),
        Some("remembered-model")
    );
    assert!(doc["providers"].get("ollama_cloud").is_none());
}

#[test]
fn malformed_config_error_omits_secret_contents_and_keys() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let secret = "cw-secret-tui-config-4507";
    fs::write(
        &path,
        format!("[providers.xai]\napi_key = \"{secret}\" trailing-junk\n"),
    )
    .expect("write malformed config");

    let error = Config::load(Some(path), None).expect_err("malformed config must fail");
    let diagnostic = format!("{error:#}");
    assert!(!diagnostic.contains(secret), "{diagnostic}");
    assert!(!diagnostic.contains("api_key"), "{diagnostic}");
    assert!(
        diagnostic.contains("file contents were omitted"),
        "{diagnostic}"
    );
}

#[test]
fn legacy_root_environment_replacement_requires_exact_id_and_refuses_prior_snapshot() {
    let _lock = lock_test_env();
    let _base = EnvVarGuard::set("CODEWHALE_BASE_URL", "http://127.0.0.1:18191/v1");
    let _model = EnvVarGuard::set("CODEWHALE_MODEL", "prepared-model");
    let _headers = EnvVarGuard::set("CODEWHALE_HTTP_HEADERS", "X-Prepared=one");
    let mut config = Config::from_saved_document(
        r#"provider = "custom"
base_url = "http://127.0.0.1:18190/v1"
api_key = "original-file-key"
default_text_model = "original-model"
"#,
        None,
    )
    .unwrap();
    let captured = config.active_provider_identity().unwrap();
    assert_eq!(captured.persisted_id(), None);
    apply_env_overrides(&mut config, ConfigEnvironmentPolicy::Runtime);
    let prepared = config.active_provider_identity().unwrap();
    assert_eq!(prepared.persisted_id(), Some("custom"));
    assert_eq!(config.default_model(), "prepared-model");
    assert_eq!(config.active_route_base_url(), "http://127.0.0.1:18191/v1");
    assert_eq!(
        config.http_headers().get("X-Prepared").map(String::as_str),
        Some("one")
    );
    assert!(config.verify_provider_identity(&captured).is_err());
    assert!(
        config
            .resolve_persisted_provider_identity(Some("custom"), None)
            .is_err()
    );
    assert_eq!(config.active_route_api_key().unwrap(), "");
}

#[test]
fn scoped_hosted_ollama_retains_legacy_leaf_and_refuses_replaced_endpoint() {
    let _lock = lock_test_env();
    let _env = EnvVarGuard::remove("OLLAMA_BASE_URL");
    let mut config = Config::from_saved_document(
        r#"provider = "deepseek"
[providers.ollama]
base_url = "https://ollama.com/v1"
model = "held-model"
"#,
        None,
    )
    .unwrap();
    let captured = config.resolve_provider_identity("ollama").unwrap();
    config.scope_to_provider_identity(&captured).unwrap();
    assert_eq!(config.active_provider_identity().unwrap(), captured);
    assert_eq!(config.default_model(), "held-model");
    config.providers.as_mut().unwrap().ollama.base_url = Some("http://localhost:11434/v1".into());
    assert!(config.active_provider_identity().is_err());
    assert!(config.verify_provider_identity(&captured).is_err());
    let explicit = config.resolve_provider_identity("ollama-cloud").unwrap();
    assert_eq!(explicit.persisted_id(), Some("ollama-cloud"));
    assert!(!explicit.migrated_legacy_ollama_cloud_route);
}

#[test]
fn api_provider_metadata_helpers_follow_config_provider_metadata() {
    let config = Config::default();
    let rows = codewhale_config::descriptors::provider_compatibility();
    assert_eq!(rows.len(), 53);
    for row in rows {
        if matches!(row.kind, ProviderKind::Antigravity | ProviderKind::Custom) {
            assert!(config.resolve_provider_selection_identity(row.id).is_err());
            continue;
        }
        let identity = config
            .resolve_provider_selection_identity(row.id)
            .expect("released admitted selection");
        assert_eq!(identity.provider, row.kind);
        assert_eq!(identity.key.as_str(), row.id);
        assert!(
            identity
                .compatibility()
                .is_some_and(|actual| std::ptr::eq(actual, row))
        );
        assert_eq!(
            identity.provider.provider().env_vars(),
            row.kind.provider().env_vars()
        );
    }
    let cn = config
        .resolve_provider_selection_identity("deepseek-cn")
        .unwrap();
    assert_eq!(cn.provider, ProviderKind::Deepseek);
    assert_eq!(cn.key.as_str(), "deepseek-cn");
    assert_eq!(
        cn.compatibility().unwrap().base_url,
        DEFAULT_DEEPSEEKCN_BASE_URL
    );
}

#[test]
fn retired_antigravity_is_not_selectable_but_has_an_actionable_tombstone() {
    for identity in ["antigravity", "agy"] {
        assert_eq!(ProviderKind::parse(identity), None, "{identity}");
    }
    assert!(!ProviderKind::all().contains(&ProviderKind::Antigravity));
    assert!(
        !codewhale_config::provider::providers_sorted_for_display()
            .iter()
            .map(|provider| provider.kind())
            .collect::<Vec<_>>()
            .contains(&ProviderKind::Antigravity)
    );

    for identity in ["antigravity", "agy"] {
        assert!(is_legacy_antigravity_identity(identity), "{identity}");
        let config = Config {
            provider: Some(identity.to_string()),
            ..Config::default()
        };
        let error = config
            .validate()
            .expect_err("a persisted legacy selection must fail before runtime setup")
            .to_string();
        assert!(error.contains("non-runnable"), "{identity}: {error}");
        assert!(
            error.contains("auth clear --provider antigravity"),
            "{identity}: {error}"
        );
        assert!(error.contains("provider `google`"), "{identity}: {error}");
        assert!(error.contains("GEMINI_API_KEY"), "{identity}: {error}");
    }
}

#[test]
fn retired_antigravity_env_selection_is_refused_with_the_tombstone() {
    let _guard = lock_test_env();
    let _deepseek_provider = EnvVarGuard::remove("DEEPSEEK_PROVIDER");
    for identity in ["antigravity", "agy"] {
        let _provider = EnvVarGuard::set("CODEWHALE_PROVIDER", identity);
        let mut config = Config::default();
        apply_env_overrides(&mut config, ConfigEnvironmentPolicy::Runtime);
        let error = config
            .validate()
            .expect_err("CODEWHALE_PROVIDER must not select the tombstone")
            .to_string();
        assert!(error.contains("non-runnable"), "{identity}: {error}");
        assert!(error.contains("GEMINI_API_KEY"), "{identity}: {error}");
    }
}

#[test]
fn every_api_provider_variant_resolves_base_url_without_panicking() {
    let rows = codewhale_config::descriptors::provider_compatibility();
    assert_eq!(rows.len(), 53);
    for row in rows {
        assert!(!row.base_url.is_empty(), "{} default base is empty", row.id);
    }
}

#[test]
fn provider_config_key_follows_config_provider_metadata() {
    let config = Config::default();
    for row in codewhale_config::descriptors::provider_compatibility() {
        if matches!(row.kind, ProviderKind::Antigravity | ProviderKind::Custom) {
            continue;
        }
        let identity = config.resolve_provider_selection_identity(row.id).unwrap();
        if row.id == ProviderKind::Deepseek.as_str() {
            assert!(provider_config_key(&identity).is_err());
        } else {
            assert_eq!(provider_config_key(&identity).unwrap(), row.config_key);
        }
    }
}

#[test]
fn deepseek_api_key_reads_metadata_env_vars_for_newer_providers() -> Result<()> {
    let _lock = lock_test_env();
    let _source = EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
    let cases = [
        (ProviderKind::Zai, "ZAI_API_KEY", "zai-env-key"),
        (ProviderKind::Stepfun, "STEPFUN_API_KEY", "stepfun-env-key"),
        (ProviderKind::Minimax, "MINIMAX_API_KEY", "minimax-env-key"),
        (
            ProviderKind::MinimaxAnthropic,
            "MINIMAX_API_KEY",
            "minimax-env-key",
        ),
        (
            ProviderKind::Deepinfra,
            "DEEPINFRA_API_KEY",
            "deepinfra-env-key",
        ),
        (ProviderKind::Sakana, "FUGU_API_KEY", "fugu-env-key"),
        (
            ProviderKind::Together,
            "TOGETHER_API_KEY",
            "together-env-key",
        ),
        (ProviderKind::Qianfan, "QIANFAN_API_KEY", "qianfan-env-key"),
        (
            ProviderKind::OpencodeGo,
            "OPENCODE_GO_API_KEY",
            "opencode-go-env-key",
        ),
    ];
    let _env_guards: Vec<_> = cases
        .iter()
        .map(|(_, var, value)| EnvVarGuard::set(var, value))
        .collect();

    for (provider, _, expected_key) in cases {
        let config = Config {
            provider: Some(provider.as_str().to_string()),
            ..Config::default()
        };

        assert_eq!(config.active_route_api_key()?, expected_key);
    }

    Ok(())
}

#[test]
fn goal_max_continuations_loads_from_goal_table() -> Result<()> {
    // Absent table → unlimited by default (#5052).
    let config: Config = toml::from_str("")?;
    assert_eq!(
        config.goal_max_continuations(),
        crate::goal_loop::DEFAULT_MAX_GOAL_CONTINUATIONS
    );
    assert_eq!(config.goal_max_continuations(), 0);
    assert_eq!(config.goal_continuation_delay_seconds(), 0);
    // enforce_token_budget defaults off: budgets stay advisory (#6013).
    assert!(!config.goal_enforce_token_budget());

    // Explicit backstop override.
    let config: Config = toml::from_str(
        r#"
[goal]
max_continuations = 25
continuation_delay_seconds = 300
"#,
    )?;
    assert_eq!(config.goal_max_continuations(), 25);
    assert_eq!(config.goal_continuation_delay_seconds(), 300);

    // 0 = unlimited; token/time budgets are telemetry only.
    let config: Config = toml::from_str(
        r#"
[goal]
max_continuations = 0
"#,
    )?;
    assert_eq!(config.goal_max_continuations(), 0);

    // Opt a set token budget into a hard stop (#6013).
    let config: Config = toml::from_str(
        r#"
[goal]
enforce_token_budget = true
"#,
    )?;
    assert!(config.goal_enforce_token_budget());
    assert_eq!(config.goal_continuation_delay_seconds(), 0);

    // Bound accidental giant cadences; this remains a turn loop, not a
    // replacement for durable low-frequency automations.
    let config: Config = toml::from_str(
        r#"
[goal]
continuation_delay_seconds = 999999999
"#,
    )?;
    assert_eq!(
        config.goal_continuation_delay_seconds(),
        crate::goal_loop::MAX_GOAL_CONTINUATION_DELAY_SECONDS
    );

    Ok(())
}

#[test]
fn reasoning_only_config_loads_from_table() -> Result<()> {
    // Absent table → built-in defaults.
    let config: Config = toml::from_str("")?;
    assert_eq!(
        config.reasoning_only_max_reprompts(),
        crate::config::DEFAULT_REASONING_ONLY_REPROMPTS
    );
    assert_eq!(
        config.reasoning_only_reprompt_message(),
        crate::config::DEFAULT_REASONING_ONLY_REPROMPT_MESSAGE
    );

    // Explicit max_reprompts override.
    let config: Config = toml::from_str(
        r#"
[reasoning_only]
max_reprompts = 10
"#,
    )?;
    assert_eq!(config.reasoning_only_max_reprompts(), 10);
    assert_eq!(
        config.reasoning_only_reprompt_message(),
        crate::config::DEFAULT_REASONING_ONLY_REPROMPT_MESSAGE
    );

    // Explicit reprompt_message override.
    let config: Config = toml::from_str(
        r#"
[reasoning_only]
max_reprompts = 5
reprompt_message = "tu n'as rien a dire"
"#,
    )?;
    assert_eq!(config.reasoning_only_max_reprompts(), 5);
    assert_eq!(
        config.reasoning_only_reprompt_message(),
        "tu n'as rien a dire"
    );

    // 0 = disable automatic recovery.
    let config: Config = toml::from_str(
        r#"
[reasoning_only]
max_reprompts = 0
"#,
    )?;
    assert_eq!(config.reasoning_only_max_reprompts(), 0);
    assert_eq!(
        config.reasoning_only_reprompt_message(),
        crate::config::DEFAULT_REASONING_ONLY_REPROMPT_MESSAGE
    );

    Ok(())
}

#[test]
fn modelstudio_coding_plan_mode_resolves_the_official_chat_base_url() {
    // The picker represents Coding Plan as the primary Model Studio provider
    // plus a mode, rather than switching to the legacy Coding Plan identity.
    // Keep this config-resolution seam covered: chat-route reasoning support
    // relies on receiving this exact official URL downstream.
    let config: Config = toml::from_str(
        r#"
provider = "modelstudio-token-plan"

[providers.modelstudio_token_plan]
mode = "coding-plan"
"#,
    )
    .expect("Coding Plan mode should parse");

    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::ModelstudioTokenPlan
    );
    assert_eq!(
        config.active_route_base_url(),
        DEFAULT_MODELSTUDIO_CODING_PLAN_BASE_URL
    );
}

#[test]
fn provider_context_scenario() -> Result<()> {
    // Scenario consolidation of: provider_context_window_loads_from_provider_table, provider_context_window_zero_is_invalid
    // from provider_context_window_loads_from_provider_table
    {
        let config: Config = toml::from_str(
            r#"
    provider = "openai"

    [providers.openai]
    model = "qwen3.7"
    context_window = 1000000
    "#,
        )?;

        config.validate()?;
        assert_eq!(
            config.context_window_for_provider_config(
                &config.test_identity_for_kind(ProviderKind::Openai)
            ),
            Some(1_000_000)
        );
    }
    // from provider_context_window_zero_is_invalid
    {
        let config: Config = toml::from_str(
            r#"
    [providers.openai]
    context_window = 0
    "#,
        )
        .expect("zero is syntactically valid TOML");

        let err = config
            .validate()
            .expect_err("zero context_window should be rejected");
        assert!(err.to_string().contains("providers.openai.context_window"));
    }
    Ok(())
}

#[test]
fn model_context_windows_load_isolate_and_validate() -> Result<()> {
    // A gateway fronting heterogeneous models: slash and dotted wire ids both
    // land as exact keys of the provider's own table (#6108).
    let config: Config = toml::from_str(
        r#"
provider = "command_code"

[providers.command_code]
kind = "openai-compatible"
base_url = "https://gateway.example/v1"
model = "qwen3.5-flash"
context_window = 204800

[providers.command_code.model_context_windows]
"qwen3.5-flash" = 131072
"MiniMaxAI/MiniMax-M2.5" = 1000000

[providers.openai.model_context_windows]
"qwen3.5-flash" = 64000
"#,
    )?;

    config.validate()?;

    let custom = config
        .model_context_windows_for(&config.test_identity_for_kind(ProviderKind::Custom))
        .expect("custom provider table resolves by selected provider name");
    assert_eq!(custom.get("qwen3.5-flash"), Some(&131_072));
    assert_eq!(custom.get("MiniMaxAI/MiniMax-M2.5"), Some(&1_000_000));

    // Per-provider isolation: the same wire id under another provider is a
    // different override, and untouched providers have no table at all.
    assert_eq!(
        config
            .model_context_windows_for(&config.test_identity_for_kind(ProviderKind::Openai))
            .and_then(|table| table.get("qwen3.5-flash").copied()),
        Some(64_000)
    );
    assert!(
        config
            .model_context_windows_for(&config.test_identity_for_kind(ProviderKind::Moonshot))
            .is_none()
    );

    // A zero entry fails validation with the full key path in the error.
    let zeroed: Config = toml::from_str(
        r#"
[providers.openai.model_context_windows]
"qwen3.5-flash" = 0
"#,
    )
    .expect("zero is syntactically valid TOML");
    let err = zeroed
        .validate()
        .expect_err("zero per-model context window must be rejected");
    assert!(
        err.to_string().contains("model_context_windows"),
        "unexpected error: {err}"
    );
    Ok(())
}

#[test]
fn opencode_go_context_window_zero_is_invalid() {
    let config: Config = toml::from_str(
        r#"
[providers.opencode_go]
context_window = 0
"#,
    )
    .expect("zero is syntactically valid TOML");

    let err = config
        .validate()
        .expect_err("zero OpenCode Go context_window should be rejected");
    assert!(
        err.to_string()
            .contains("providers.opencode_go.context_window")
    );
}

#[test]
fn missing_provider_api_key_message_uses_provider_metadata() -> Result<()> {
    let message = missing_provider_api_key_message(
        &Config::default().test_identity_for_kind(ProviderKind::Zai),
    )?;

    assert!(message.contains("Zhipu AI / Z.ai API key not found"));
    assert!(message.contains("https://z.ai/model-api"));
    assert!(message.contains("ZAI_API_KEY / Z_AI_API_KEY"));
    assert!(message.contains("[providers.zai] api_key"));

    Ok(())
}

#[test]
fn opencode_zen_missing_credentials_never_mentions_codex_oauth() -> Result<()> {
    let message = missing_provider_api_key_message(
        &Config::default().test_identity_for_kind(ProviderKind::OpencodeZen),
    )?;
    assert!(message.contains("OpenCode Zen API key not found"));
    assert!(message.contains("OPENCODE_ZEN_API_KEY"));
    assert!(message.contains("OPENCODE_API_KEY"));
    assert!(message.contains("[providers.opencode_zen]"));
    assert!(!message.contains("codex login"));
    assert!(!message.contains("ChatGPT"));
    assert!(!message.contains("auth.json"));
    Ok(())
}

// GHSA-72w5-pf8h-xfp4 — regression: `allow_shell` must be opt-in.
#[test]
fn allow_shell_defaults_to_false_when_unset() {
    let config = Config::default();
    assert_eq!(config.allow_shell, None, "default Config has no opt-in set");
    assert!(
        !config.allow_shell(),
        "Config::allow_shell() must default to false when no opt-in is recorded"
    );
}

// The interactive default is shell-on (approval-gated). Both interactive
// startup and the durable Agent permission baseline (app.rs) read this single
// method so the default cannot drift between launch modes; an explicit opt-out
// is still honored.
#[test]
fn interactive_allow_shell_defaults_to_true_but_honors_explicit_opt_out() {
    let default_config = Config::default();
    assert!(
        default_config.interactive_allow_shell(),
        "interactive Agent sessions expose shell by default so approvals can gate commands"
    );

    let opted_out = Config {
        allow_shell: Some(false),
        ..Config::default()
    };
    assert!(
        !opted_out.interactive_allow_shell(),
        "explicit allow_shell = false still hides shell in interactive sessions"
    );

    let opted_in = Config {
        allow_shell: Some(true),
        ..Config::default()
    };
    assert!(opted_in.interactive_allow_shell());
}

#[test]
fn prompt_suggestion_scenario() {
    // Scenario consolidation of: prompt_suggestion_defaults_to_false, prompt_suggestion_enabled_when_set_true
    // from prompt_suggestion_defaults_to_false
    {
        let config = Config::default();
        assert_eq!(
            config.prompt_suggestion, None,
            "default Config must not opt in"
        );
        assert!(
            !config.prompt_suggestion_enabled(),
            "prompt_suggestion must be opt-in (default off)"
        );
    }
    // from prompt_suggestion_enabled_when_set_true
    {
        let config = Config {
            prompt_suggestion: Some(true),
            ..Default::default()
        };
        assert!(config.prompt_suggestion_enabled());
    }
}

#[test]
fn config_loads_sibling_permissions_into_exec_policy_engine() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("config.toml");
    fs::write(&config_path, "model = \"deepseek-v4-pro\"\n").expect("write config");
    fs::write(
        dir.path().join(codewhale_config::PERMISSIONS_FILE_NAME),
        r#"
[[rules]]
tool = "exec_shell"
command = "cargo test"
"#,
    )
    .expect("write permissions");

    let config = Config::load(Some(config_path), None).expect("load config");
    let decision = config
        .exec_policy_engine
        .check(codewhale_execpolicy::ExecPolicyContext {
            command: "cargo test --workspace",
            cwd: dir.path().to_string_lossy().as_ref(),
            tool: Some("exec_shell"),
            path: None,
            ask_for_approval: codewhale_execpolicy::AskForApproval::OnFailure,
            sandbox_mode: None,
        })
        .expect("check permission");

    assert!(decision.allow);
    assert!(decision.requires_approval);
    assert_eq!(
        decision.matched_rule.as_deref(),
        Some("tool=exec_shell command=cargo test")
    );
}

#[test]
fn config_loads_sibling_permissions_when_config_file_is_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("config.toml");
    fs::write(
        dir.path().join(codewhale_config::PERMISSIONS_FILE_NAME),
        r#"
[[rules]]
tool = "exec_shell"
command = "npm test"
"#,
    )
    .expect("write permissions");

    let config = Config::load(Some(config_path), None).expect("load config");
    let decision = config
        .exec_policy_engine
        .check(codewhale_execpolicy::ExecPolicyContext {
            command: "npm test -- --runInBand",
            cwd: dir.path().to_string_lossy().as_ref(),
            tool: Some("exec_shell"),
            path: None,
            ask_for_approval: codewhale_execpolicy::AskForApproval::OnFailure,
            sandbox_mode: None,
        })
        .expect("check permission");

    assert!(decision.requires_approval);
    assert_eq!(
        decision.matched_rule.as_deref(),
        Some("tool=exec_shell command=npm test")
    );
}

#[test]
fn warns_when_allow_shell_nested_under_general_section() {
    // #2589: the reporter's config nested top-level keys under sections that
    // do not exist, so they were silently dropped and shell tools vanished.
    let raw = "[general]\nallow_shell = true\n\n[sandbox]\nsandbox_mode = \"danger-full-access\"\n";
    let warning =
        warn_on_misplaced_top_level_keys(raw).expect("misplaced keys should produce a warning");
    assert!(warning.contains("general.allow_shell"));
    assert!(warning.contains("sandbox.sandbox_mode"));
    assert!(warning.contains("#2589"));

    // Correctly placed top-level keys produce no warning.
    let ok = "allow_shell = true\nsandbox_mode = \"danger-full-access\"\n";
    assert!(warn_on_misplaced_top_level_keys(ok).is_none());

    // A parsed config from the correct placement actually enables shell.
    let parsed: ConfigFile = toml::from_str(ok).expect("parse top-level config");
    assert!(parsed.base.allow_shell());
}

#[test]
fn sandbox_network_access_parses_and_defaults_to_restricted() {
    // Absent key: restricted. Editing the workspace does not imply egress.
    let parsed: ConfigFile =
        toml::from_str("sandbox_mode = \"workspace-write\"\n").expect("parse without the key");
    assert_eq!(parsed.base.sandbox_network_access, None);

    let parsed: ConfigFile =
        toml::from_str("sandbox_network_access = true\n").expect("parse snake_case");
    assert_eq!(parsed.base.sandbox_network_access, Some(true));

    let parsed: ConfigFile =
        toml::from_str("sandboxNetworkAccess = true\n").expect("parse camelCase alias");
    assert_eq!(parsed.base.sandbox_network_access, Some(true));

    let parsed: ConfigFile =
        toml::from_str("sandbox_network_access = false\n").expect("parse explicit false");
    assert_eq!(parsed.base.sandbox_network_access, Some(false));
}

#[test]
fn load_honors_codewhale_home_for_primary_config_path() -> Result<()> {
    let _lock = lock_test_env();
    let dir = tempfile::tempdir()?;
    let codewhale_home = dir.path().join("isolated-codewhale");
    fs::create_dir_all(&codewhale_home)?;
    fs::write(codewhale_home.join("config.toml"), "provider = \"zai\"\n")?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _codewhale_config = EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
    let _deepseek_config = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");

    let expected = codewhale_home.join("config.toml");
    assert_eq!(default_config_path()?, expected);
    let config = Config::load(None, None)?;

    assert_eq!(config.provider.as_deref(), Some("zai"));
    Ok(())
}

#[test]
fn load_accepts_dispatcher_written_camel_case_config_shape() -> Result<()> {
    let _lock = lock_test_env();
    let dir = tempfile::tempdir()?;
    let codewhale_home = dir.path().join("isolated-codewhale");
    fs::create_dir_all(&codewhale_home)?;
    fs::write(
        codewhale_home.join("config.toml"),
        r#"
provider = "zai"
fallbackProviders = []
apiKey = "deepseek-test-key"
defaultTextModel = "deepseek-v4-pro"
authMode = "api_key"

[providers.zai]
apiKey = "zai-test-key"
authMode = "api_key"

[providers.zai.httpHeaders]

[providers.xiaomiMimo]
baseUrl = "https://token-plan-sgp.xiaomimimo.com/v1"

[features.enabled]
shell_tool = true
subagents = true
web_search = true
"#,
    )?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _codewhale_config = EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
    let _deepseek_config = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");

    let config = Config::load(None, None)?;

    assert_eq!(config.provider.as_deref(), Some("zai"));
    assert_eq!(config.deepseek_table_api_key(), Some("deepseek-test-key"));
    assert_eq!(
        config.default_text_model.as_deref(),
        Some("deepseek-v4-pro")
    );
    assert_eq!(config.auth_mode.as_deref(), Some("api_key"));
    let providers = config.providers.as_ref().expect("provider table");
    assert_eq!(providers.zai.api_key.as_deref(), Some("zai-test-key"));
    assert_eq!(providers.zai.auth_mode.as_deref(), Some("api_key"));
    assert_eq!(
        providers.xiaomi_mimo.base_url.as_deref(),
        Some("https://token-plan-sgp.xiaomimimo.com/v1")
    );
    let features = config.features();
    assert!(features.enabled(crate::features::Feature::ShellTool));
    assert!(features.enabled(crate::features::Feature::Subagents));
    assert!(features.enabled(crate::features::Feature::WebSearch));
    Ok(())
}

#[test]
fn tui_config_parses_lifecycle_outbox_table() {
    let raw = r#"
[lifecycle_outbox]
path = "~/.codewhale/notifications/outbox.jsonl"
webhook_url = "https://example.com/hooks/codewhale"
webhook_token = "secret-token"
"#;
    let parsed: ConfigFile = toml::from_str(raw).expect("parse lifecycle_outbox config");

    let outbox = parsed
        .base
        .lifecycle_outbox
        .expect("lifecycle_outbox table should parse");
    assert_eq!(
        outbox.path,
        Some(PathBuf::from("~/.codewhale/notifications/outbox.jsonl"))
    );
    assert_eq!(
        outbox.webhook_url.as_deref(),
        Some("https://example.com/hooks/codewhale")
    );
    assert_eq!(outbox.webhook_token.as_deref(), Some("secret-token"));

    // Off by default: a config without the table leaves the feature off.
    let absent: ConfigFile =
        toml::from_str("model = \"demo\"").expect("parse config without outbox table");
    assert!(absent.base.lifecycle_outbox.is_none());
}

/// `tui.posture_bar` / `tui.metrics_line` (#5950): absent means full — an
/// older `config.toml` keeps loading unchanged — and each key takes one of
/// the three presets.
#[test]
fn tui_config_parses_bottom_chrome_row_presets() {
    let raw = r#"
[tui]
posture_bar = "compact"
metrics_line = "hidden"
"#;
    let parsed: ConfigFile = toml::from_str(raw).expect("parse row presets");
    let tui = parsed.base.tui.expect("tui table");
    assert_eq!(tui.posture_bar, Some(ChromeRowPreset::Compact));
    assert_eq!(tui.metrics_line, Some(ChromeRowPreset::Hidden));

    let absent: ConfigFile = toml::from_str("[tui]\nmouse_capture = true\n").expect("old file");
    let tui = absent.base.tui.expect("tui table");
    assert_eq!(tui.posture_bar, None);
    assert_eq!(tui.metrics_line, None);
    assert_eq!(
        tui.posture_bar.unwrap_or_default(),
        ChromeRowPreset::Full,
        "absent means the full row"
    );

    let bad: Result<ConfigFile, _> = toml::from_str("[tui]\nposture_bar = \"tiny\"\n");
    assert!(
        bad.is_err(),
        "a preset this build does not know is refused, not guessed"
    );

    for (setting, preset) in [
        ("full", ChromeRowPreset::Full),
        ("compact", ChromeRowPreset::Compact),
        ("hidden", ChromeRowPreset::Hidden),
    ] {
        assert_eq!(ChromeRowPreset::from_setting(setting), Some(preset));
        assert_eq!(
            ChromeRowPreset::from_setting(&setting.to_uppercase()),
            Some(preset)
        );
        assert_eq!(preset.as_setting(), setting);
    }
    assert_eq!(ChromeRowPreset::from_setting("tiny"), None);
}

#[test]
fn tui_config_parses_control_socket_table() {
    let raw = r#"
[control_socket]
enabled = true
"#;
    let parsed: ConfigFile = toml::from_str(raw).expect("parse control_socket config");

    let socket = parsed
        .base
        .control_socket
        .expect("control_socket table should parse");
    assert!(socket.enabled);

    // Off by default: a config without the table leaves the feature off.
    let absent: ConfigFile =
        toml::from_str("model = \"demo\"").expect("parse config without control_socket table");
    assert!(absent.base.control_socket.is_none());

    // An empty table stays off.
    let empty: ConfigFile =
        toml::from_str("[control_socket]").expect("parse empty control_socket table");
    assert!(
        !empty
            .base
            .control_socket
            .expect("table should parse")
            .enabled,
        "empty table must leave the socket off"
    );
}

#[test]
fn tui_config_parses_hotbar_bindings() {
    let raw = r#"
[[hotbar]]
slot = 1
label = "Plan"
action = "mode.plan"

[[hotbar]]
slot = 2
action = "session.compact"
"#;
    let parsed: ConfigFile = toml::from_str(raw).expect("parse hotbar config");

    let resolved = parsed
        .base
        .resolve_hotbar_bindings(&["mode.plan", "session.compact"]);

    assert_eq!(resolved.warnings, Vec::new());
    assert_eq!(
        resolved
            .bindings
            .iter()
            .map(|binding| (
                binding.slot,
                binding.action.as_str(),
                binding.label.as_deref()
            ))
            .collect::<Vec<_>>(),
        vec![(1, "mode.plan", Some("Plan")), (2, "session.compact", None),]
    );
}

#[test]
fn tui_config_empty_hotbar_array_disables_defaults() {
    let parsed: ConfigFile = toml::from_str("hotbar = []\n").expect("parse empty hotbar");

    let resolved = parsed
        .base
        .resolve_hotbar_bindings(&["mode.plan", "session.compact"]);

    assert_eq!(resolved.warnings, Vec::new());
    assert_eq!(resolved.bindings, Vec::new());
}

#[test]
fn profile_hotbar_override_replaces_entire_user_list() {
    let mut profiles = HashMap::new();
    profiles.insert(
        "compact".to_string(),
        Config {
            hotbar: Some(vec![codewhale_config::HotbarBindingToml {
                slot: 2,
                action: "session.compact".to_string(),
                label: Some("Compact".to_string()),
            }]),
            ..Config::default()
        },
    );
    let config = ConfigFile {
        base: Box::new(Config {
            hotbar: Some(vec![codewhale_config::HotbarBindingToml {
                slot: 1,
                action: "mode.plan".to_string(),
                label: Some("Plan".to_string()),
            }]),
            ..Config::default()
        }),
        profiles: Some(profiles),
        legacy_root: Default::default(),
    };

    let merged = apply_profile(config, Some("compact")).expect("profile");

    assert_eq!(
        merged.hotbar,
        Some(vec![codewhale_config::HotbarBindingToml {
            slot: 2,
            action: "session.compact".to_string(),
            label: Some("Compact".to_string()),
        }])
    );
}

#[test]
fn profile_without_scenario() {
    // Scenario consolidation of: profile_without_hotbar_keeps_base_hotbar, profile_without_context_does_not_disable_base_context
    // from profile_without_hotbar_keeps_base_hotbar
    {
        let mut profiles = HashMap::new();
        profiles.insert("work".to_string(), Config::default());
        let config = ConfigFile {
            base: Box::new(Config {
                hotbar: Some(vec![codewhale_config::HotbarBindingToml {
                    slot: 1,
                    action: "mode.plan".to_string(),
                    label: None,
                }]),
                ..Config::default()
            }),
            profiles: Some(profiles),
            legacy_root: Default::default(),
        };

        let merged = apply_profile(config, Some("work")).expect("profile");

        assert_eq!(
            merged.hotbar,
            Some(vec![codewhale_config::HotbarBindingToml {
                slot: 1,
                action: "mode.plan".to_string(),
                label: None,
            }])
        );
    }
    // from profile_without_context_does_not_disable_base_context
    {
        let mut profiles = HashMap::new();
        profiles.insert("work".to_string(), Config::default());
        let config = ConfigFile {
            base: Box::new(Config {
                context: ContextConfig {
                    project_pack: Some(true),
                },
                ..Default::default()
            }),
            profiles: Some(profiles),
            legacy_root: Default::default(),
        };

        let merged = apply_profile(config, Some("work")).expect("profile");
        assert_eq!(merged.context.project_pack, Some(true));
    }
}

#[test]
fn update_config_scenario() {
    // Scenario consolidation of: update_config_defaults_to_enabled_without_uri, update_config_deserializes_disable_and_custom_uri
    // from update_config_defaults_to_enabled_without_uri
    {
        let config = Config::default();
        assert_eq!(config.update, None);
        assert_eq!(config.update_config(), UpdateConfig::default());
        assert!(config.update_config().check_for_updates);
        assert_eq!(config.update_config().update_uri(), None);
    }
    // from update_config_deserializes_disable_and_custom_uri
    {
        let config: Config = toml::from_str(
            r#"
            [update]
            check_for_updates = false
            update_uri = "https://mirror.example/releases/latest"
            "#,
        )
        .expect("update config");

        let update = config.update_config();
        assert!(!update.check_for_updates);
        assert_eq!(
            update.update_uri(),
            Some("https://mirror.example/releases/latest")
        );
    }
}

#[test]
fn verifier_config_parses_and_merges_overrides() {
    let config: Config = toml::from_str(
        r#"
        [verifier]
        enabled = true
        "#,
    )
    .expect("parse verifier config");

    let verifier = config.verifier.expect("verifier table");
    assert!(verifier.enabled);

    let merged = merge_config(
        Config {
            verifier: Some(codewhale_config::VerifierConfigToml { enabled: false }),
            ..Config::default()
        },
        Config {
            verifier: Some(codewhale_config::VerifierConfigToml { enabled: true }),
            ..Config::default()
        },
    );

    assert!(merged.verifier.expect("merged verifier").enabled);
}

#[test]
fn workflow_config_defaults_when_omitted_and_overrides_round_trip() {
    // #4128: omitted `[workflow]` resolves through the accessor to product
    // defaults; explicit overrides load and survive serialize → parse.
    let omitted: Config = toml::from_str("").expect("empty config");
    assert!(omitted.workflow.is_none());
    assert_eq!(
        omitted.workflow_config(),
        codewhale_config::WorkflowConfigToml::default()
    );

    let config: Config = toml::from_str(
        r#"
        [workflow]
        automatic = false
        auto_start_read_only = false
        require_approval_for_writes = true
        max_children = 32
        max_depth = 1
        default_token_budget = 90000
        "#,
    )
    .expect("parse workflow config");

    let workflow = config.workflow.clone().expect("workflow table");
    assert!(!workflow.automatic);
    assert!(!workflow.auto_start_read_only);
    assert!(workflow.require_approval_for_writes);
    assert_eq!(workflow.max_children, 32);
    assert_eq!(workflow.max_depth, 1);
    assert_eq!(workflow.default_token_budget, 90_000);
    assert_eq!(config.workflow_config(), workflow);

    let serialized = toml::to_string_pretty(&workflow).expect("serialize workflow");
    let round_tripped: codewhale_config::WorkflowConfigToml =
        toml::from_str(&serialized).expect("round-trip parse");
    assert_eq!(round_tripped, workflow);

    // Profile/project overlays replace the whole table when present.
    let merged = merge_config(
        Config {
            workflow: Some(codewhale_config::WorkflowConfigToml::default()),
            ..Config::default()
        },
        Config {
            workflow: Some(workflow.clone()),
            ..Config::default()
        },
    );
    assert_eq!(merged.workflow_config(), workflow);
}

#[test]
fn window_title_config_parses_and_overlays() {
    // `title` is a plain optional root key: absent config → `None`.
    let omitted: Config = toml::from_str("").expect("empty config");
    assert_eq!(omitted.title, None);

    let config: Config = toml::from_str(r#"title = "workspace-x""#).expect("parse title config");
    assert_eq!(config.title.as_deref(), Some("workspace-x"));

    // Overlay wins over the base when both define a title.
    let merged = merge_config(
        Config {
            title: Some("base-title".to_string()),
            ..Config::default()
        },
        Config {
            title: Some("override-title".to_string()),
            ..Config::default()
        },
    );
    assert_eq!(merged.title.as_deref(), Some("override-title"));

    // Base title survives an overlay that does not mention it.
    let merged = merge_config(
        Config {
            title: Some("base-title".to_string()),
            ..Config::default()
        },
        Config::default(),
    );
    assert_eq!(merged.title.as_deref(), Some("base-title"));
}

/// Run `body` with the three Tavily-resolution signals set exactly as given,
/// restoring the ambient values afterwards. A `Default` pin (and the
/// Firecrawl China-switch hint) means all three are absent, not just the
/// legacy `DEEPSEEK_SEARCH_PROVIDER` alias.
fn with_search_resolution_env<R>(set: &[(&str, &str)], body: impl FnOnce() -> R) -> R {
    let _guard = lock_test_env();
    let keys = [
        "CODEWHALE_SEARCH_PROVIDER",
        "DEEPSEEK_SEARCH_PROVIDER",
        "TAVILY_API_KEY",
    ];
    let previous: Vec<Option<OsString>> = keys.iter().map(env::var_os).collect();
    for key in keys {
        unsafe { env::remove_var(key) };
    }
    for (key, value) in set {
        unsafe { env::set_var(key, value) };
    }
    let output = body();
    for (key, value) in keys.iter().zip(previous) {
        unsafe { EnvGuard::restore_var(key, value) };
    }
    output
}

#[test]
fn pinned_search_provider_wins_over_provider_native_search() {
    let parse = |toml_text: &str| -> Config { toml::from_str(toml_text).expect("search config") };
    with_search_resolution_env(&[], || {
        // Nothing configured: native search keeps leading where offered.
        assert_eq!(Config::default().search_native(), None);
        // A pinned provider wins unless native is explicitly re-enabled.
        assert_eq!(
            parse("[search]\nprovider = \"tavily\"\n").search_native(),
            Some(false)
        );
        assert_eq!(
            parse("[search]\nprovider = \"tavily\"\nnative = true\n").search_native(),
            Some(true)
        );
        assert_eq!(
            parse("[search]\nnative = false\n").search_native(),
            Some(false)
        );
    });
    with_search_resolution_env(&[("CODEWHALE_SEARCH_PROVIDER", "searxng")], || {
        assert_eq!(Config::default().search_native(), Some(false));
    });
    with_search_resolution_env(&[("TAVILY_API_KEY", "tvly-test")], || {
        assert_eq!(Config::default().search_native(), Some(false));
    });
}

#[test]
fn search_provider_scenario() {
    // Scenario consolidation of: search_provider_defaults_to_firecrawl, search_provider_resolution_reports_default_source, search_provider_resolution_reports_config_source, search_provider_resolution_reports_env_override_source, search_provider_env_override_accepts_baidu, search_provider_resolution_ignores_invalid_env_override
    // from search_provider_defaults_to_firecrawl
    {
        assert_eq!(SearchProvider::default(), SearchProvider::Firecrawl);
        assert_eq!(
            SearchProvider::parse("fire-crawl"),
            Some(SearchProvider::Firecrawl)
        );
        assert_eq!(SearchProvider::Firecrawl.as_str(), "firecrawl");
    }
    // from search_provider_resolution_reports_default_source
    // (empty config, no env)
    with_search_resolution_env(&[], || {
        let config = Config::default();
        let resolution = config.search_provider_resolution();

        assert_eq!(resolution.provider, SearchProvider::Firecrawl);
        assert_eq!(resolution.source, SearchProviderSource::Default);
        // Autodetect never writes a provider into the config view.
        assert_eq!(
            config.search.as_ref().and_then(|search| search.provider),
            None
        );
    });
    // `TAVILY_API_KEY=tvly-test`, provider unset
    with_search_resolution_env(&[("TAVILY_API_KEY", "tvly-test")], || {
        let config = Config::default();
        let resolution = config.search_provider_resolution();

        assert_eq!(resolution.provider, SearchProvider::Tavily);
        assert_eq!(resolution.source, SearchProviderSource::TavilyKey);
        assert_eq!(resolution.source.as_str(), "tavily key");
        assert_eq!(resolution.provider.as_str(), "tavily");
        // Autodetect is runtime-only: `config.search.provider` stays unset.
        assert_eq!(
            config.search.as_ref().and_then(|search| search.provider),
            None
        );
    });
    // `TAVILY_API_KEY=not-a-tvly-prefix`, provider unset — a dedicated env key
    // is never prefix-checked.
    with_search_resolution_env(&[("TAVILY_API_KEY", "not-a-tvly-prefix")], || {
        let resolution = Config::default().search_provider_resolution();

        assert_eq!(resolution.provider, SearchProvider::Tavily);
        assert_eq!(resolution.source, SearchProviderSource::TavilyKey);
    });
    // `[search] api_key = "tvly-test"`, provider unset — also the shape left by
    // `CODEWHALE_SEARCH_API_KEY=tvly-test` after `apply_env_overrides`.
    with_search_resolution_env(&[], || {
        let config: Config = toml::from_str(
            r#"
            [search]
            api_key = "tvly-test"
            "#,
        )
        .expect("search config");
        let resolution = config.search_provider_resolution();

        assert_eq!(resolution.provider, SearchProvider::Tavily);
        assert_eq!(resolution.source, SearchProviderSource::TavilyKey);
        assert_eq!(
            config.search.as_ref().and_then(|search| search.provider),
            None
        );
    });
    // A non-`tvly-` generic key never autodetects. Covers
    // `CODEWHALE_SEARCH_API_KEY=doctor-offline-search-sentinel` and any
    // Firecrawl `fc-` generic value.
    with_search_resolution_env(&[], || {
        let config: Config = toml::from_str(
            r#"
            [search]
            api_key = "doctor-offline-search-sentinel"
            "#,
        )
        .expect("search config");
        let resolution = config.search_provider_resolution();

        assert_eq!(resolution.provider, SearchProvider::Firecrawl);
        assert_eq!(resolution.source, SearchProviderSource::Default);
    });
    // Dedicated provider keys that are not the Tavily signal leave the
    // Firecrawl default in place.
    with_search_resolution_env(
        &[
            ("FIRECRAWL_API_KEY", "fc-test"),
            ("SOFYA_API_KEY", "ay_live_test"),
        ],
        || {
            let resolution = Config::default().search_provider_resolution();

            assert_eq!(resolution.provider, SearchProvider::Firecrawl);
            assert_eq!(resolution.source, SearchProviderSource::Default);
        },
    );
    // Explicit Firecrawl wins over a Tavily key.
    with_search_resolution_env(&[("TAVILY_API_KEY", "tvly-test")], || {
        let config: Config = toml::from_str(
            r#"
            [search]
            provider = "firecrawl"
            "#,
        )
        .expect("search config");
        let resolution = config.search_provider_resolution();

        assert_eq!(resolution.provider, SearchProvider::Firecrawl);
        assert_eq!(resolution.source, SearchProviderSource::Config);
    });
    // An env override outranks both the config pin and the Tavily signal.
    with_search_resolution_env(
        &[
            ("CODEWHALE_SEARCH_PROVIDER", "firecrawl"),
            ("TAVILY_API_KEY", "tvly-test"),
        ],
        || {
            let resolution = Config::default().search_provider_resolution();

            assert_eq!(resolution.provider, SearchProvider::Firecrawl);
            assert_eq!(resolution.source, SearchProviderSource::EnvOverride);
        },
    );

    // from search_provider_resolution_reports_config_source
    with_search_resolution_env(&[], || {
        let config: Config = toml::from_str(
            r#"
            [search]
            provider = "tavily"
            "#,
        )
        .expect("search config");

        let resolution = config.search_provider_resolution();

        assert_eq!(resolution.provider, SearchProvider::Tavily);
        assert_eq!(resolution.source, SearchProviderSource::Config);
    });
    // from search_provider_resolution_reports_env_override_source
    with_search_resolution_env(&[("CODEWHALE_SEARCH_PROVIDER", "bocha")], || {
        let config: Config = toml::from_str(
            r#"
            [search]
            provider = "duckduckgo"
            "#,
        )
        .expect("search config");

        let resolution = config.search_provider_resolution();

        assert_eq!(resolution.provider, SearchProvider::Bocha);
        assert_eq!(resolution.source, SearchProviderSource::EnvOverride);
    });
    // The legacy alias still resolves the same way.
    with_search_resolution_env(&[("DEEPSEEK_SEARCH_PROVIDER", "bocha")], || {
        let config: Config = toml::from_str(
            r#"
            [search]
            provider = "duckduckgo"
            "#,
        )
        .expect("search config");

        let resolution = config.search_provider_resolution();

        assert_eq!(resolution.provider, SearchProvider::Bocha);
        assert_eq!(resolution.source, SearchProviderSource::EnvOverride);
    });
    // from search_provider_env_override_accepts_baidu
    with_search_resolution_env(&[("CODEWHALE_SEARCH_PROVIDER", "baidu")], || {
        let config: Config = toml::from_str(
            r#"
            [search]
            provider = "duckduckgo"
            "#,
        )
        .expect("search config");

        let resolution = config.search_provider_resolution();

        assert_eq!(resolution.provider, SearchProvider::Baidu);
        assert_eq!(resolution.source, SearchProviderSource::EnvOverride);
    });
    // from search_provider_resolution_ignores_invalid_env_override
    with_search_resolution_env(&[("CODEWHALE_SEARCH_PROVIDER", "not-a-provider")], || {
        let config: Config = toml::from_str(
            r#"
            [search]
            provider = "tavily"
            "#,
        )
        .expect("search config");

        let resolution = config.search_provider_resolution();

        assert_eq!(resolution.provider, SearchProvider::Tavily);
        assert_eq!(resolution.source, SearchProviderSource::Config);
    });
}

#[test]
fn tools_always_load_parses_and_trims_names() {
    let parsed: ConfigFile = toml::from_str(
        r#"
        [tools]
        always_load = ["git_show", " notify ", ""]
        "#,
    )
    .expect("tools config");

    let names = parsed.base.tools_always_load();

    assert!(names.contains("git_show"));
    assert!(names.contains("notify"));
    assert!(!names.contains(""));
}

#[test]
fn user_input_timeout_defaults_disabled_and_clamps() {
    let parsed: ConfigFile = toml::from_str("").expect("empty config");
    assert_eq!(parsed.base.user_input_timeout(), None);

    let parsed: ConfigFile = toml::from_str(
        r#"
        [tools]
        user_input_timeout_seconds = 900
        "#,
    )
    .expect("tools config");
    assert_eq!(
        parsed.base.user_input_timeout(),
        Some(std::time::Duration::from_secs(900))
    );

    // An explicit 0 is the documented "wait forever" value, preserved as
    // zero rather than defaulted or clamped away.
    let parsed: ConfigFile = toml::from_str(
        r#"
        [tools]
        user_input_timeout_seconds = 0
        "#,
    )
    .expect("tools config");
    assert_eq!(
        parsed.base.user_input_timeout(),
        Some(std::time::Duration::ZERO)
    );

    let parsed: ConfigFile = toml::from_str(
        r#"
        [tools]
        user_input_timeout_seconds = 999999
        "#,
    )
    .expect("tools config");
    assert_eq!(
        parsed.base.user_input_timeout(),
        Some(std::time::Duration::from_secs(86_400))
    );
}

#[test]
fn approval_timeout_defaults_unbounded_and_clamps() {
    let parsed: ConfigFile = toml::from_str("").expect("empty config");
    assert_eq!(parsed.base.approval_timeout(), None);

    // The card stays unbounded when only presentation is configured.
    let parsed: ConfigFile = toml::from_str(
        r#"
        [approval]
        default_selection = "allow_once"
        "#,
    )
    .expect("approval config");
    assert_eq!(parsed.base.approval_timeout(), None);

    let parsed: ConfigFile = toml::from_str(
        r#"
        [approval]
        timeout_seconds = 300
        "#,
    )
    .expect("approval config");
    assert_eq!(
        parsed.base.approval_timeout(),
        Some(std::time::Duration::from_secs(300))
    );

    // An explicit 0 follows the repo's "wait forever" convention (#6101).
    let parsed: ConfigFile = toml::from_str(
        r#"
        [approval]
        timeout_seconds = 0
        "#,
    )
    .expect("approval config");
    assert_eq!(parsed.base.approval_timeout(), None);

    let parsed: ConfigFile = toml::from_str(
        r#"
        [approval]
        timeout_seconds = 999999
        "#,
    )
    .expect("approval config");
    assert_eq!(
        parsed.base.approval_timeout(),
        Some(std::time::Duration::from_secs(86_400))
    );
}

#[test]
fn explicit_duckduckgo_search_provider_is_preserved() {
    let config: Config = toml::from_str(
        r#"
        [search]
        provider = "duckduckgo"
        "#,
    )
    .expect("search config");

    assert_eq!(
        config.search.and_then(|search| search.provider),
        Some(SearchProvider::DuckDuckGo)
    );
}

#[test]
fn search_config_preserves_custom_base_url() {
    let config: Config = toml::from_str(
        r#"
        [search]
        provider = "duckduckgo"
        base_url = "https://search.internal.example/html/"
        "#,
    )
    .expect("search config");

    let search = config.search.expect("search table");
    assert_eq!(search.provider, Some(SearchProvider::DuckDuckGo));
    assert_eq!(
        search.base_url.as_deref(),
        Some("https://search.internal.example/html/")
    );
}

#[test]
fn explicit_searxng_search_provider_is_preserved() {
    let config: Config = toml::from_str(
        r#"
        [search]
        provider = "searxng"
        base_url = "https://search.internal.example/"
        "#,
    )
    .expect("search config");

    let search = config.search.expect("search table");
    assert_eq!(search.provider, Some(SearchProvider::Searxng));
    assert_eq!(
        search.base_url.as_deref(),
        Some("https://search.internal.example/")
    );
}

#[test]
fn searxng_search_provider_aliases_parse_and_round_trip() {
    assert_eq!(
        SearchProvider::parse("searxng"),
        Some(SearchProvider::Searxng)
    );
    assert_eq!(
        SearchProvider::parse("searx-ng"),
        Some(SearchProvider::Searxng)
    );
    assert_eq!(
        SearchProvider::parse("searx_ng"),
        Some(SearchProvider::Searxng)
    );
    assert_eq!(
        SearchProvider::parse("searx"),
        Some(SearchProvider::Searxng)
    );
    assert_eq!(SearchProvider::Searxng.as_str(), "searxng");
}

#[test]
fn explicit_baidu_search_provider_is_preserved() {
    let config: Config = toml::from_str(
        r#"
        [search]
        provider = "baidu"
        "#,
    )
    .expect("search config");

    assert_eq!(
        config.search.and_then(|search| search.provider),
        Some(SearchProvider::Baidu)
    );
}

#[test]
fn baidu_search_provider_aliases_parse() {
    assert_eq!(SearchProvider::parse("baidu"), Some(SearchProvider::Baidu));
    assert_eq!(
        SearchProvider::parse("baidu-search"),
        Some(SearchProvider::Baidu)
    );
    assert_eq!(
        SearchProvider::parse("baidu_ai_search"),
        Some(SearchProvider::Baidu)
    );
}

#[test]
fn volcengine_search_provider_aliases_parse_and_deserialize() {
    assert_eq!(
        SearchProvider::parse("volcengine"),
        Some(SearchProvider::Volcengine)
    );
    assert_eq!(
        SearchProvider::parse("volcengine-ark"),
        Some(SearchProvider::Volcengine)
    );

    let config: Config = toml::from_str(
        r#"
        [search]
        provider = "volcengine-ark"
        "#,
    )
    .expect("volcengine search config");

    assert_eq!(
        config.search.and_then(|search| search.provider),
        Some(SearchProvider::Volcengine)
    );
}

#[test]
fn explicit_sofya_search_provider_is_preserved() {
    let config: Config = toml::from_str(
        r#"
        [search]
        provider = "sofya"
        "#,
    )
    .expect("sofya search config");

    assert_eq!(
        config.search.and_then(|search| search.provider),
        Some(SearchProvider::Sofya)
    );
}

#[test]
fn sofya_search_provider_parses_and_round_trips() {
    assert_eq!(SearchProvider::parse("sofya"), Some(SearchProvider::Sofya));
    assert_eq!(SearchProvider::parse("Sofya"), Some(SearchProvider::Sofya));
    assert_eq!(SearchProvider::Sofya.as_str(), "sofya");
}

#[test]
fn explicit_serply_search_provider_is_preserved() {
    let config: Config = toml::from_str(
        r#"
        [search]
        provider = "serply"
        "#,
    )
    .expect("serply search config");

    assert_eq!(
        config.search.and_then(|search| search.provider),
        Some(SearchProvider::Serply)
    );
}

#[test]
fn serply_search_provider_parses_and_round_trips() {
    assert_eq!(
        SearchProvider::parse("serply"),
        Some(SearchProvider::Serply)
    );
    assert_eq!(
        SearchProvider::parse("Serply"),
        Some(SearchProvider::Serply)
    );
    assert_eq!(SearchProvider::Serply.as_str(), "serply");
}

#[test]
fn live_search_provider_update_preserves_environment_precedence() {
    let _guard = lock_test_env();
    let previous_codewhale = env::var_os("CODEWHALE_SEARCH_PROVIDER");
    let previous_deepseek = env::var_os("DEEPSEEK_SEARCH_PROVIDER");
    unsafe {
        env::set_var("CODEWHALE_SEARCH_PROVIDER", "bocha");
        env::remove_var("DEEPSEEK_SEARCH_PROVIDER");
    }
    let mut config = Config::default();

    let effective = config.set_search_provider(SearchProvider::DuckDuckGo);

    unsafe {
        EnvGuard::restore_var("CODEWHALE_SEARCH_PROVIDER", previous_codewhale);
        EnvGuard::restore_var("DEEPSEEK_SEARCH_PROVIDER", previous_deepseek);
    }
    assert_eq!(effective, SearchProvider::Bocha);
    assert_eq!(
        config.search.and_then(|search| search.provider),
        Some(SearchProvider::DuckDuckGo),
        "the requested config value should still be retained beneath the override"
    );
}

#[test]
fn notification_defaults_and_live_updates_share_one_consistent_model() {
    let mut notifications = NotificationsConfig::default();
    assert_eq!(notifications.threshold_secs, 30);
    assert_eq!(notifications.completion_sound, CompletionSound::Off);
    assert_eq!(
        Config::default().notifications_config().threshold_secs,
        notifications.threshold_secs
    );
    assert_eq!(
        Config::default().notifications_config().completion_sound,
        CompletionSound::Off,
        "a fresh install must never opt itself into an audible completion cue"
    );

    notifications
        .apply_update(NotificationConfigUpdate::Method(NotificationMethod::Osc9))
        .unwrap();
    notifications
        .apply_update(NotificationConfigUpdate::Quiet(true))
        .unwrap();

    assert_eq!(notifications.method, NotificationMethod::Osc9);
    assert!(notifications.quiet);
    assert_eq!(
        notifications.threshold_secs, 30,
        "field deltas must preserve both defaults and earlier live edits"
    );
}

#[test]
fn notification_condition_accepts_background_only_policy() {
    let config: Config = toml::from_str(
        r#"
        [tui]
        notification_condition = "unfocused"
        "#,
    )
    .expect("unfocused attention policy should parse");

    assert_eq!(
        config.tui.and_then(|tui| tui.notification_condition),
        Some(NotificationCondition::Unfocused)
    );
}

#[test]
fn apply_env_scenario() {
    // Scenario consolidation of: apply_env_overrides_sets_search_api_key, apply_env_overrides_sets_search_base_url
    // from apply_env_overrides_sets_search_api_key
    {
        let _guard = lock_test_env();
        let prev = env::var_os("DEEPSEEK_SEARCH_API_KEY");
        unsafe { env::set_var("DEEPSEEK_SEARCH_API_KEY", "search-env-key") };
        let mut config = Config::default();

        apply_env_overrides(&mut config, ConfigEnvironmentPolicy::Runtime);

        unsafe { EnvGuard::restore_var("DEEPSEEK_SEARCH_API_KEY", prev) };
        assert_eq!(
            config.search.and_then(|search| search.api_key),
            Some("search-env-key".to_string())
        );
    }
    // from apply_env_overrides_sets_search_base_url
    {
        let _guard = lock_test_env();
        let prev_codewhale = env::var_os("CODEWHALE_SEARCH_BASE_URL");
        let prev_deepseek = env::var_os("DEEPSEEK_SEARCH_BASE_URL");
        unsafe {
            env::remove_var("CODEWHALE_SEARCH_BASE_URL");
            env::set_var(
                "DEEPSEEK_SEARCH_BASE_URL",
                "https://search.internal.example/html/",
            )
        };
        let mut config = Config::default();

        apply_env_overrides(&mut config, ConfigEnvironmentPolicy::Runtime);

        unsafe {
            EnvGuard::restore_var("CODEWHALE_SEARCH_BASE_URL", prev_codewhale);
            EnvGuard::restore_var("DEEPSEEK_SEARCH_BASE_URL", prev_deepseek);
        }
        assert_eq!(
            config.search.and_then(|search| search.base_url),
            Some("https://search.internal.example/html/".to_string())
        );
    }
}

#[test]
fn apply_env_overrides_yolo_prefers_codewhale_and_keeps_deepseek_alias() {
    let _guard = lock_test_env();
    let codewhale_prev = env::var_os("CODEWHALE_YOLO");
    let deepseek_prev = env::var_os("DEEPSEEK_YOLO");
    let mut config = Config::default();

    // Only the canonical name is set.
    unsafe {
        env::set_var("CODEWHALE_YOLO", "true");
        env::remove_var("DEEPSEEK_YOLO");
    }
    apply_env_overrides(&mut config, ConfigEnvironmentPolicy::Runtime);
    assert_eq!(
        config.yolo,
        Some(true),
        "CODEWHALE_YOLO=true must enable the yolo posture"
    );

    // Only the deprecated alias is set: it must keep working through 0.9.x.
    unsafe {
        env::remove_var("CODEWHALE_YOLO");
        env::set_var("DEEPSEEK_YOLO", "true");
    }
    apply_env_overrides(&mut config, ConfigEnvironmentPolicy::Runtime);
    assert_eq!(
        config.yolo,
        Some(true),
        "DEEPSEEK_YOLO remains a read-only deprecated alias until 0.10 (#5443)"
    );

    // Both set: the canonical name wins.
    unsafe { env::set_var("CODEWHALE_YOLO", "false") };
    apply_env_overrides(&mut config, ConfigEnvironmentPolicy::Runtime);
    assert_eq!(
        config.yolo,
        Some(false),
        "CODEWHALE_YOLO must win over the deprecated DEEPSEEK_YOLO alias"
    );

    unsafe {
        EnvGuard::restore_var("CODEWHALE_YOLO", codewhale_prev);
        EnvGuard::restore_var("DEEPSEEK_YOLO", deepseek_prev);
    }
}

#[test]
fn structural_config_load_keeps_safe_environment_overrides_but_omits_secret_values() {
    let _guard = lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let config_path = temp.path().join("config.toml");
    fs::write(&config_path, "").expect("empty config");
    let _home = EnvVarGuard::set("CODEWHALE_HOME", temp.path().join("home"));
    let _profile = EnvVarGuard::remove("CODEWHALE_PROFILE");
    let _legacy_profile = EnvVarGuard::remove("DEEPSEEK_PROFILE");
    let _managed = EnvVarGuard::remove("CODEWHALE_MANAGED_CONFIG_PATH");
    let _legacy_managed = EnvVarGuard::remove("DEEPSEEK_MANAGED_CONFIG_PATH");
    let _requirements = EnvVarGuard::remove("CODEWHALE_REQUIREMENTS_PATH");
    let _legacy_requirements = EnvVarGuard::remove("DEEPSEEK_REQUIREMENTS_PATH");
    let _headers = EnvVarGuard::set(
        "CODEWHALE_HTTP_HEADERS",
        "Authorization=structural-header-secret",
    );
    let _legacy_headers = EnvVarGuard::set(
        "DEEPSEEK_HTTP_HEADERS",
        "Authorization=legacy-structural-header-secret",
    );
    let _sandbox_key = EnvVarGuard::set("CODEWHALE_SANDBOX_API_KEY", "structural-sandbox-secret");
    let _legacy_sandbox_key = EnvVarGuard::set(
        "DEEPSEEK_SANDBOX_API_KEY",
        "legacy-structural-sandbox-secret",
    );
    let _search_key = EnvVarGuard::set("CODEWHALE_SEARCH_API_KEY", "structural-search-secret");
    let _legacy_search_key =
        EnvVarGuard::set("DEEPSEEK_SEARCH_API_KEY", "legacy-structural-search-secret");
    let _base_url = EnvVarGuard::set("CODEWHALE_BASE_URL", "https://safe.example:8443/v1");
    let _allow_shell = EnvVarGuard::set("CODEWHALE_ALLOW_SHELL", "false");

    let runtime = Config::load(Some(config_path.clone()), None).expect("runtime config");
    assert!(runtime.http_headers.is_some());
    assert_eq!(
        runtime.sandbox_api_key.as_deref(),
        Some("structural-sandbox-secret")
    );
    assert_eq!(
        runtime
            .search
            .as_ref()
            .and_then(|search| search.api_key.as_deref()),
        Some("structural-search-secret")
    );

    let structural = Config::load_structural(Some(config_path), None).expect("structural config");
    assert!(structural.http_headers.is_none());
    assert!(structural.sandbox_api_key.is_none());
    assert!(
        structural
            .search
            .as_ref()
            .and_then(|search| search.api_key.as_deref())
            .is_none()
    );
    assert_eq!(
        structural.deepseek_table_base_url(),
        Some("https://safe.example:8443/v1")
    );
    assert_eq!(structural.allow_shell, Some(false));

    let rendered = format!("{structural:?}");
    for sentinel in [
        "structural-header-secret",
        "legacy-structural-header-secret",
        "structural-sandbox-secret",
        "legacy-structural-sandbox-secret",
        "structural-search-secret",
        "legacy-structural-search-secret",
    ] {
        assert!(
            !rendered.contains(sentinel),
            "structural config retained {sentinel}"
        );
    }
}

#[test]
fn codewhale_search_base_url_env_wins_over_legacy_alias() {
    let _guard = lock_test_env();
    let prev_codewhale = env::var_os("CODEWHALE_SEARCH_BASE_URL");
    let prev_deepseek = env::var_os("DEEPSEEK_SEARCH_BASE_URL");
    unsafe {
        env::set_var(
            "CODEWHALE_SEARCH_BASE_URL",
            "https://codewhale-search.example/html/",
        );
        env::set_var(
            "DEEPSEEK_SEARCH_BASE_URL",
            "https://legacy-search.example/html/",
        );
    }
    let mut config = Config::default();

    apply_env_overrides(&mut config, ConfigEnvironmentPolicy::Runtime);

    unsafe {
        EnvGuard::restore_var("CODEWHALE_SEARCH_BASE_URL", prev_codewhale);
        EnvGuard::restore_var("DEEPSEEK_SEARCH_BASE_URL", prev_deepseek);
    }
    assert_eq!(
        config.search.and_then(|search| search.base_url),
        Some("https://codewhale-search.example/html/".to_string())
    );
}

#[test]
fn codewhale_prefer_bwrap_env_wins_over_legacy_alias() {
    let _guard = lock_test_env();
    let _primary = EnvVarGuard::set("CODEWHALE_PREFER_BWRAP", "false");
    let _legacy = EnvVarGuard::set("DEEPSEEK_PREFER_BWRAP", "true");
    let mut config = Config {
        prefer_bwrap: Some(true),
        ..Config::default()
    };

    apply_env_overrides(&mut config, ConfigEnvironmentPolicy::Runtime);

    assert_eq!(config.prefer_bwrap, Some(false));
}

#[test]
fn legacy_prefer_bwrap_env_remains_a_compatible_alias() {
    let _guard = lock_test_env();
    let _primary = EnvVarGuard::remove("CODEWHALE_PREFER_BWRAP");
    let _legacy = EnvVarGuard::set("DEEPSEEK_PREFER_BWRAP", "true");
    let mut config = Config::default();

    apply_env_overrides(&mut config, ConfigEnvironmentPolicy::Runtime);

    assert_eq!(config.prefer_bwrap, Some(true));
}

#[test]
fn prefers_bwrap_defaults_on_and_honors_explicit_opt_out() {
    assert!(Config::default().prefers_bwrap());
    assert!(
        Config {
            prefer_bwrap: Some(true),
            ..Config::default()
        }
        .prefers_bwrap()
    );
    assert!(
        !Config {
            prefer_bwrap: Some(false),
            ..Config::default()
        }
        .prefers_bwrap()
    );
}

#[test]
fn max_subagents_scenario() {
    // Scenario consolidation of: max_subagents_defaults_to_default_limit, max_subagents_clamps_subagents_max_concurrent
    // from max_subagents_defaults_to_default_limit
    {
        assert_eq!(Config::default().max_subagents(), DEFAULT_MAX_SUBAGENTS);
        assert_eq!(DEFAULT_MAX_SUBAGENTS, 64);
    }
    // from max_subagents_clamps_subagents_max_concurrent
    {
        let low = Config {
            subagents: Some(SubagentsConfig {
                max_concurrent: Some(0),
                ..SubagentsConfig::default()
            }),
            ..Config::default()
        };
        assert_eq!(low.max_subagents(), 1);

        let high = Config {
            subagents: Some(SubagentsConfig {
                max_concurrent: Some(MAX_SUBAGENTS + 10),
                ..SubagentsConfig::default()
            }),
            ..Config::default()
        };
        assert_eq!(high.max_subagents(), MAX_SUBAGENTS);
    }
}

#[test]
fn launch_concurrency_defaults_and_clamps_to_max_subagents() {
    // Unset launch_concurrency now defaults to the full resolved cap.
    assert_eq!(
        Config::default().launch_concurrency(),
        Config::default().max_subagents()
    );

    let mut config = Config {
        subagents: Some(SubagentsConfig {
            launch_concurrency: Some(50),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(config.launch_concurrency(), 50);

    config.subagents = Some(SubagentsConfig {
        launch_concurrency: Some(DEFAULT_MAX_SUBAGENTS + 10),
        ..SubagentsConfig::default()
    });
    assert_eq!(config.launch_concurrency(), config.max_subagents());

    config.subagents = Some(SubagentsConfig {
        launch_concurrency: Some(0),
        ..SubagentsConfig::default()
    });
    assert_eq!(config.launch_concurrency(), 1);

    config.subagents = Some(SubagentsConfig {
        launch_concurrency: Some(2),
        ..SubagentsConfig::default()
    });
    assert_eq!(config.launch_concurrency(), 2);
}

#[test]
fn subagent_budget_defaults_read_the_subagents_table() {
    // #5324: per-child step/wall-time defaults are operator config
    // (`[subagents]`), not per-call schema fields. `0` means unset (keep the
    // role / 1800s defaults).
    let cfg: SubagentsConfig =
        toml::from_str("default_max_steps = 90\ndefault_wall_time_secs = 600")
            .expect("parse [subagents] budget keys");
    assert_eq!(cfg.default_max_steps, Some(90));
    assert_eq!(cfg.default_wall_time_secs, Some(600));

    let mut config = Config {
        subagents: Some(SubagentsConfig {
            default_max_steps: Some(240),
            default_wall_time_secs: Some(2700),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(config.subagent_default_max_steps(), Some(240));
    assert_eq!(config.subagent_default_wall_time_secs(), Some(2700));

    config.subagents = Some(SubagentsConfig {
        default_max_steps: Some(0),
        default_wall_time_secs: Some(0),
        ..SubagentsConfig::default()
    });
    assert_eq!(config.subagent_default_max_steps(), None);
    assert_eq!(config.subagent_default_wall_time_secs(), None);

    assert_eq!(Config::default().subagent_default_max_steps(), None);
    assert_eq!(Config::default().subagent_default_wall_time_secs(), None);
}

#[test]
fn launch_concurrency_scenario() {
    // Scenario consolidation of: launch_concurrency_honors_deprecated_interactive_max_launch_alias, launch_concurrency_new_key_wins_over_deprecated_alias
    // from launch_concurrency_honors_deprecated_interactive_max_launch_alias
    {
        // The old TOML key `interactive_max_launch` still deserializes, via
        // #[serde(rename)], into the hidden legacy field, and the resolver
        // honors it when the new key is unset.
        let cfg: SubagentsConfig =
            toml::from_str("interactive_max_launch = 5").expect("parse legacy key");
        assert_eq!(cfg.interactive_max_launch_legacy, Some(5));
        assert_eq!(cfg.launch_concurrency, None);

        let config = Config {
            subagents: Some(cfg),
            ..Config::default()
        };
        assert_eq!(config.launch_concurrency(), 5);
    }
    // from launch_concurrency_new_key_wins_over_deprecated_alias
    {
        // When both keys are present the new `launch_concurrency` wins
        // deterministically, regardless of document order.
        let cfg: SubagentsConfig =
            toml::from_str("launch_concurrency = 3\ninteractive_max_launch = 7")
                .expect("parse both keys");
        assert_eq!(cfg.launch_concurrency, Some(3));
        assert_eq!(cfg.interactive_max_launch_legacy, Some(7));

        let config = Config {
            subagents: Some(cfg),
            ..Config::default()
        };
        assert_eq!(config.launch_concurrency(), 3);
    }
}

#[test]
fn fleet_role_model_keys_accept_canonical_and_legacy_names() {
    let canonical: Config = toml::from_str(
        r#"
[subagents]
scout_model = "scout-model"
planner_model = "planner-model"
reviewer_model = "reviewer-model"
"#,
    )
    .expect("parse canonical Fleet role keys");
    let overrides = canonical.subagent_model_overrides();
    assert_eq!(
        overrides.get("scout").map(|pin| pin.model.as_str()),
        Some("scout-model")
    );
    assert_eq!(
        overrides.get("planner").map(|pin| pin.model.as_str()),
        Some("planner-model")
    );
    assert_eq!(
        overrides.get("reviewer").map(|pin| pin.model.as_str()),
        Some("reviewer-model")
    );

    let legacy: Config = toml::from_str(
        r#"
[subagents]
explorer_model = "legacy-scout"
awaiter_model = "legacy-planner"
review_model = "legacy-reviewer"
"#,
    )
    .expect("parse v0.9.x role aliases");
    let overrides = legacy.subagent_model_overrides();
    assert_eq!(
        overrides.get("scout").map(|pin| pin.model.as_str()),
        Some("legacy-scout")
    );
    assert_eq!(
        overrides.get("planner").map(|pin| pin.model.as_str()),
        Some("legacy-planner")
    );
    assert_eq!(
        overrides.get("reviewer").map(|pin| pin.model.as_str()),
        Some("legacy-reviewer")
    );
}

#[test]
fn structured_role_pins_merge_into_legacy_override_authority() {
    let config: Config = toml::from_str(
        r#"
[subagents]
reviewer_model = "scalar-reviewer"
worker_model = "scalar-worker"
scout_model = "scalar-scout"
[subagents.models]
reviewer = "map-reviewer"
[subagents.roles.REVIEW]
model = "alias-reviewer"
[subagents.roles.reviewer]
model = "canonical-reviewer"
[subagents.roles.scout]
model = "structured-scout"
[subagents.roles.default]
model = "all-role-fallback"
[subagents.roles.custom]
model = "  "
"#,
    )
    .expect("parse structured and legacy inputs together");
    let overrides = config.subagent_model_overrides();
    assert_eq!(overrides["reviewer"].model, "canonical-reviewer");
    assert_eq!(overrides["explore"].model, "structured-scout");
    assert_eq!(overrides["general"].model, "scalar-worker");
    assert_eq!(overrides["default"].model, "all-role-fallback");
    assert_eq!(
        overrides["custom"].model, "",
        "blank explicit pin reaches admission validation"
    );
}

#[test]
fn structured_role_declarations_preserve_provider_identity_and_legacy_wire_ids() {
    let config: Config = toml::from_str(
        r#"
[subagents]
worker_model = "deepseek/deepseek-v4-pro"
[subagents.models]
reviewer = "deepseek/deepseek-v4-flash"
[subagents.roles.advisor]
model = "ReviewerRoute/vendor/model-id"
[subagents.roles.implement]
model = "openrouter/deepseek/deepseek-v4-pro"
"#,
    )
    .unwrap();
    let pins = config.subagent_model_overrides();
    for (role, model) in [
        ("general", "deepseek/deepseek-v4-pro"),
        ("reviewer", "deepseek/deepseek-v4-flash"),
    ] {
        assert_eq!(
            pins[role].provider, None,
            "legacy ids remain provider-owned"
        );
        assert_eq!(pins[role].model, model);
    }
    assert_eq!(pins["advisor"].provider.as_deref(), Some("ReviewerRoute"));
    assert_eq!(pins["advisor"].model, "vendor/model-id");
    assert_eq!(pins["implement"].provider.as_deref(), Some("openrouter"));
    assert_eq!(pins["implement"].model, "deepseek/deepseek-v4-pro");
}

#[test]
fn structured_role_pins_require_a_typed_model_field() {
    for input in [
        "[subagents.roles.reviewer]\n",
        "[subagents.roles.reviewer]\nmodel = 42\n",
        "[subagents.roles.reviewer]\nmodel = \"model\"\nprovider = \"other\"\n",
    ] {
        assert!(
            toml::from_str::<Config>(input).is_err(),
            "invalid role pin accepted: {input}"
        );
    }
}

#[test]
fn subagent_admission_limit_defaults_and_clamps() {
    assert_eq!(
        Config::default().max_admitted_subagents(),
        MAX_SUBAGENT_ADMISSION
    );

    let configured = Config {
        subagents: Some(SubagentsConfig {
            max_concurrent: Some(4),
            max_admitted: Some(80),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(configured.max_subagents(), 4);
    assert_eq!(configured.max_admitted_subagents(), 80);

    let low = Config {
        subagents: Some(SubagentsConfig {
            max_concurrent: Some(4),
            max_admitted: Some(1),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(low.max_admitted_subagents(), 4);

    let high = Config {
        subagents: Some(SubagentsConfig {
            max_admitted: Some(MAX_SUBAGENT_ADMISSION + 1),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(high.max_admitted_subagents(), MAX_SUBAGENT_ADMISSION);

    let alias_cfg: SubagentsConfig =
        toml::from_str("admission_limit = 80").expect("parse admission alias");
    assert_eq!(alias_cfg.max_admitted, Some(80));
}

#[test]
fn provider_subagent_profiles_override_global_limits_with_aliases() {
    let config: Config = toml::from_str(
        r#"
provider = "zai"

[subagents]
max_concurrent = 20
launch_concurrency = 20
max_admitted = 200
max_depth = 6
api_timeout_secs = 900
heartbeat_timeout_secs = 1200

[subagents.providers.glm]
max_concurrent = 4
launch_concurrency = 3
max_admitted = 12
max_depth = 2
api_timeout_secs = 180
heartbeat_timeout_secs = 240
"#,
    )
    .expect("parse provider subagent profile");

    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Zai
    );
    assert_eq!(config.max_subagents(), 20);
    assert_eq!(
        config.max_subagents_for_provider(&config.test_identity_for_kind(ProviderKind::Zai)),
        4
    );
    assert_eq!(
        config.launch_concurrency_for_provider(&config.test_identity_for_kind(ProviderKind::Zai)),
        3
    );
    assert_eq!(
        config
            .max_admitted_subagents_for_provider(&config.test_identity_for_kind(ProviderKind::Zai)),
        12
    );
    assert_eq!(
        config.subagent_max_spawn_depth_for_provider(
            &config.test_identity_for_kind(ProviderKind::Zai)
        ),
        2
    );
    assert_eq!(
        config.subagent_api_timeout_secs_for_provider(
            &config.test_identity_for_kind(ProviderKind::Zai)
        ),
        180
    );
    // The explicit 240s provider override sits below the 300s tool timeout,
    // and a heartbeat under the tool timeout kills children mid-legitimate-
    // tool (activity is only recorded at step boundaries). The tool-timeout
    // floor lifts the resolved value above the override
    // (2026-08-04 sub-agent hunt, finding 4).
    assert_eq!(
        config.subagent_heartbeat_timeout_secs_for_provider(
            &config.test_identity_for_kind(ProviderKind::Zai)
        ),
        DEFAULT_SUBAGENT_TOOL_TIMEOUT_SECS + 30
    );
}

#[test]
fn provider_request_concurrency_defaults_to_zai_and_can_be_overridden() {
    let default_zai: Config = toml::from_str(
        r#"
provider = "zai"
"#,
    )
    .expect("parse zai provider config");
    assert_eq!(
        default_zai
            .provider_max_concurrency(&default_zai.test_identity_for_kind(ProviderKind::Zai)),
        Some(DEFAULT_ZAI_PROVIDER_MAX_CONCURRENCY)
    );
    assert_eq!(
        default_zai
            .provider_max_concurrency(&default_zai.test_identity_for_kind(ProviderKind::Deepseek)),
        None
    );

    let configured: Config = toml::from_str(
        r#"
provider = "zai"

[providers.zhipu]
max-concurrency = 10
"#,
    )
    .expect("parse zhipu concurrency alias");
    assert_eq!(
        configured.provider_max_concurrency(&configured.test_identity_for_kind(ProviderKind::Zai)),
        Some(10)
    );

    let disabled: Config = toml::from_str(
        r#"
provider = "zai"

[providers.zai]
maxConcurrency = 0
"#,
    )
    .expect("parse disabled concurrency cap");
    assert_eq!(
        disabled.provider_max_concurrency(&disabled.test_identity_for_kind(ProviderKind::Zai)),
        None
    );

    let clamped: Config = toml::from_str(
        r#"
[providers.openai]
concurrency = 999
"#,
    )
    .expect("parse openai concurrency alias");
    assert_eq!(
        clamped.provider_max_concurrency(&clamped.test_identity_for_kind(ProviderKind::Openai)),
        Some(MAX_PROVIDER_REQUEST_CONCURRENCY)
    );
}

#[test]
fn provider_subagent_profiles_inherit_and_clamp_against_provider_max() {
    let config: Config = toml::from_str(
        r#"
[subagents]
max_concurrent = 12
launch_concurrency = 8
max_depth = 5
api_timeout_secs = 300

[subagents.providers.deepseek_api]
max_concurrent = 30
launch_concurrency = 30
max_admitted = 1

[subagents.providers.anthropic]
enabled = false
"#,
    )
    .expect("parse inherited provider subagent profile");

    assert_eq!(
        config.max_subagents_for_provider(&config.test_identity_for_kind(ProviderKind::Deepseek)),
        30
    );
    assert_eq!(
        config.launch_concurrency_for_provider(
            &config.test_identity_for_kind(ProviderKind::Deepseek)
        ),
        30
    );
    assert_eq!(
        config.max_admitted_subagents_for_provider(
            &config.test_identity_for_kind(ProviderKind::Deepseek)
        ),
        30
    );
    assert_eq!(
        config.subagent_max_spawn_depth_for_provider(
            &config.test_identity_for_kind(ProviderKind::Deepseek)
        ),
        5
    );
    assert_eq!(
        config.subagent_api_timeout_secs_for_provider(
            &config.test_identity_for_kind(ProviderKind::Deepseek)
        ),
        300
    );
    assert!(
        config
            .subagents_enabled_for_provider(&config.test_identity_for_kind(ProviderKind::Deepseek))
    );
    assert!(
        !config.subagents_enabled_for_provider(
            &config.test_identity_for_kind(ProviderKind::Anthropic)
        )
    );
}

#[test]
fn subagents_max_concurrent_overrides_top_level_cap() {
    let config = Config {
        max_subagents: Some(3),
        subagents: Some(SubagentsConfig {
            max_concurrent: Some(12),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };

    assert_eq!(config.max_subagents(), 12);
}

#[test]
fn subagents_enabled_reports_disable_precedence() {
    assert!(Config::default().subagents_enabled());

    let mut feature_disabled = Config::default();
    feature_disabled
        .set_feature("subagents", false)
        .expect("known feature");
    assert!(!feature_disabled.subagents_enabled());
    assert_eq!(
        feature_disabled.subagents_disabled_reason(),
        Some("features.subagents=false")
    );

    let explicit_disabled = Config {
        subagents: Some(SubagentsConfig {
            enabled: Some(false),
            max_concurrent: Some(0),
            max_depth: Some(0),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert!(!explicit_disabled.subagents_enabled());
    assert_eq!(
        explicit_disabled.subagents_disabled_reason(),
        Some("subagents.enabled=false")
    );

    let zero_concurrency = Config {
        subagents: Some(SubagentsConfig {
            enabled: Some(true),
            max_concurrent: Some(0),
            max_depth: Some(1),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(
        zero_concurrency.subagents_disabled_reason(),
        Some("subagents.max_concurrent=0")
    );

    let zero_depth = Config {
        subagents: Some(SubagentsConfig {
            enabled: Some(true),
            max_concurrent: Some(1),
            max_depth: Some(0),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(
        zero_depth.subagents_disabled_reason(),
        Some("subagents.max_depth=0")
    );
}

#[test]
fn subagent_max_spawn_depth_defaults_allows_zero_and_preserves_owner_choice() {
    assert_eq!(
        Config::default().subagent_max_spawn_depth(),
        codewhale_protocol::fleet::DEFAULT_SPAWN_DEPTH
    );

    let disabled = Config {
        subagents: Some(SubagentsConfig {
            max_depth: Some(0),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(disabled.subagent_max_spawn_depth(), 0);

    let high = Config {
        subagents: Some(SubagentsConfig {
            max_depth: Some(99),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(high.subagent_max_spawn_depth(), 99);
}

#[test]
fn subagent_api_timeout_defaults_and_clamps() {
    assert_eq!(
        Config::default().subagent_api_timeout_secs(),
        DEFAULT_SUBAGENT_API_TIMEOUT_SECS
    );
    assert_eq!(DEFAULT_SUBAGENT_API_TIMEOUT_SECS, 600);

    let zero = Config {
        subagents: Some(SubagentsConfig {
            api_timeout_secs: Some(0),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(
        zero.subagent_api_timeout_secs(),
        DEFAULT_SUBAGENT_API_TIMEOUT_SECS
    );

    let explicit_min = Config {
        subagents: Some(SubagentsConfig {
            api_timeout_secs: Some(MIN_SUBAGENT_API_TIMEOUT_SECS),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(explicit_min.subagent_api_timeout_secs(), 1);

    let explicit_max = Config {
        subagents: Some(SubagentsConfig {
            api_timeout_secs: Some(3600),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(explicit_max.subagent_api_timeout_secs(), 3600);

    let beyond_max = Config {
        subagents: Some(SubagentsConfig {
            api_timeout_secs: Some(3601),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(beyond_max.subagent_api_timeout_secs(), 3600);

    let high = Config {
        subagents: Some(SubagentsConfig {
            api_timeout_secs: Some(MAX_SUBAGENT_API_TIMEOUT_SECS + 60),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(
        high.subagent_api_timeout_secs(),
        MAX_SUBAGENT_API_TIMEOUT_SECS
    );
}

#[test]
fn subagent_heartbeat_timeout_defaults_clamps_and_respects_api_timeout() {
    // With the 600s default API timeout, the heartbeat floor (api + 30s)
    // lifts the resolved default above the raw 300s constant. The tool
    // timeout floor (tool + 30s) also participates but sits below the API
    // floor at default settings.
    let resolved_default = DEFAULT_SUBAGENT_HEARTBEAT_TIMEOUT_SECS
        .max(DEFAULT_SUBAGENT_API_TIMEOUT_SECS + 30)
        .max(DEFAULT_SUBAGENT_TOOL_TIMEOUT_SECS + 30);
    assert_eq!(
        Config::default().subagent_heartbeat_timeout_secs(),
        resolved_default
    );

    let zero = Config {
        subagents: Some(SubagentsConfig {
            heartbeat_timeout_secs: Some(0),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(zero.subagent_heartbeat_timeout_secs(), resolved_default);

    // With a tiny API timeout the tool-timeout floor dominates: a single
    // tool execution can run the full tool timeout without touching the
    // heartbeat, so cleanup must not fire before tool_timeout + 30s even
    // though the API floor alone would be 31s (2026-08-04 sub-agent hunt,
    // finding 4 — this case resolved to 31 before the fix, which let
    // cleanup kill a child mid-legitimate-tool).
    let low = Config {
        subagents: Some(SubagentsConfig {
            api_timeout_secs: Some(1),
            heartbeat_timeout_secs: Some(1),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(
        low.subagent_heartbeat_timeout_secs(),
        DEFAULT_SUBAGENT_TOOL_TIMEOUT_SECS + 30
    );

    let follows_long_api_timeout = Config {
        subagents: Some(SubagentsConfig {
            api_timeout_secs: Some(900),
            heartbeat_timeout_secs: Some(300),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(
        follows_long_api_timeout.subagent_heartbeat_timeout_secs(),
        930
    );

    let high = Config {
        subagents: Some(SubagentsConfig {
            heartbeat_timeout_secs: Some(MAX_SUBAGENT_HEARTBEAT_TIMEOUT_SECS + 60),
            ..SubagentsConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(
        high.subagent_heartbeat_timeout_secs(),
        MAX_SUBAGENT_HEARTBEAT_TIMEOUT_SECS
    );
}

#[test]
fn subagent_heartbeat_floor_never_drops_below_tool_timeout_plus_margin() {
    // The safety property behind finding 4: for EVERY accepted combination of
    // `[subagents] api_timeout_secs` and `heartbeat_timeout_secs`, the
    // resolved heartbeat timeout stays above the tool timeout, because a tool
    // running up to `tool_timeout` produces no heartbeat activity. Corners
    // cover the smallest legal API timeout and heartbeat against both the
    // global and provider-specific resolvers.
    let corners: [Option<u64>; 4] = [
        Some(MIN_SUBAGENT_API_TIMEOUT_SECS),
        Some(1),
        None,
        Some(MAX_SUBAGENT_API_TIMEOUT_SECS),
    ];
    let heartbeats: [Option<u64>; 4] = [
        Some(MIN_SUBAGENT_HEARTBEAT_TIMEOUT_SECS),
        Some(1),
        None,
        Some(0),
    ];
    for api in corners {
        for heartbeat in heartbeats {
            let cfg = Config {
                subagents: Some(SubagentsConfig {
                    api_timeout_secs: api,
                    heartbeat_timeout_secs: heartbeat,
                    ..SubagentsConfig::default()
                }),
                ..Config::default()
            };
            let floor = DEFAULT_SUBAGENT_TOOL_TIMEOUT_SECS + 30;
            assert!(
                cfg.subagent_heartbeat_timeout_secs() >= floor,
                "global resolver: api={api:?} heartbeat={heartbeat:?} resolved {} < {floor}",
                cfg.subagent_heartbeat_timeout_secs()
            );
            assert!(
                cfg.subagent_heartbeat_timeout_secs_for_provider(
                    &cfg.test_identity_for_kind(ProviderKind::Deepseek)
                ) >= floor,
                "provider resolver: api={api:?} heartbeat={heartbeat:?} resolved {} < {floor}",
                cfg.subagent_heartbeat_timeout_secs_for_provider(
                    &cfg.test_identity_for_kind(ProviderKind::Deepseek)
                )
            );
        }
    }
}

#[test]
fn tui_stream_chunk_timeout_defaults_env_and_clamps() {
    let _lock = lock_test_env();
    let previous = env::var_os(STREAM_CHUNK_TIMEOUT_ENV);
    unsafe {
        env::remove_var(STREAM_CHUNK_TIMEOUT_ENV);
    }

    assert_eq!(
        Config::default().stream_chunk_timeout_secs(),
        DEFAULT_STREAM_CHUNK_TIMEOUT_SECS
    );

    let zero = Config {
        tui: Some(TuiConfig {
            stream_chunk_timeout_secs: Some(0),
            max_model_steps: None,
            turn_wall_clock_secs: None,
            stream_max_content_mb: None,
            stream_max_duration_secs: None,
            ..TuiConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(
        zero.stream_chunk_timeout_secs(),
        DEFAULT_STREAM_CHUNK_TIMEOUT_SECS
    );

    let explicit_min = Config {
        tui: Some(TuiConfig {
            stream_chunk_timeout_secs: Some(MIN_STREAM_CHUNK_TIMEOUT_SECS),
            max_model_steps: None,
            turn_wall_clock_secs: None,
            stream_max_content_mb: None,
            stream_max_duration_secs: None,
            ..TuiConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(
        explicit_min.stream_chunk_timeout_secs(),
        MIN_STREAM_CHUNK_TIMEOUT_SECS
    );

    let high = Config {
        tui: Some(TuiConfig {
            stream_chunk_timeout_secs: Some(MAX_STREAM_CHUNK_TIMEOUT_SECS + 1),
            max_model_steps: None,
            turn_wall_clock_secs: None,
            stream_max_content_mb: None,
            stream_max_duration_secs: None,
            ..TuiConfig::default()
        }),
        ..Config::default()
    };
    assert_eq!(
        high.stream_chunk_timeout_secs(),
        MAX_STREAM_CHUNK_TIMEOUT_SECS
    );

    unsafe {
        env::set_var(STREAM_CHUNK_TIMEOUT_ENV, "123");
    }
    assert_eq!(Config::default().stream_chunk_timeout_secs(), 123);

    unsafe {
        env::set_var(STREAM_CHUNK_TIMEOUT_ENV, "0");
    }
    assert_eq!(
        Config::default().stream_chunk_timeout_secs(),
        DEFAULT_STREAM_CHUNK_TIMEOUT_SECS
    );

    unsafe {
        match previous {
            Some(value) => env::set_var(STREAM_CHUNK_TIMEOUT_ENV, value),
            None => env::remove_var(STREAM_CHUNK_TIMEOUT_ENV),
        }
    }
}

#[test]
fn save_api_key_writes_config_file_under_cfg_test() -> Result<()> {
    // `save_api_key` writes to the shared user config file. This
    // pins the boring v0.8.8 setup path and avoids platform
    // credential prompts during onboarding.
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let saved = save_api_key("test-key")?;
    let expected = temp_root.join(".deepseek").join("config.toml");
    assert_eq!(saved, SavedCredential::ConfigFile(expected.clone()));
    assert_eq!(saved.describe(), expected.display().to_string());

    let contents = fs::read_to_string(&expected)?;
    assert!(contents.contains("api_key = \""));

    #[cfg(unix)]
    {
        assert_eq!(fs::metadata(&expected)?.permissions().mode() & 0o777, 0o600);
        let parent = expected.parent().expect("config has parent dir");
        assert_eq!(fs::metadata(parent)?.permissions().mode() & 0o077, 0);

        fs::set_permissions(&expected, fs::Permissions::from_mode(0o644))?;
        save_api_key("second-test-key")?;
        assert_eq!(fs::metadata(&expected)?.permissions().mode() & 0o777, 0o600);
    }
    Ok(())
}

#[test]
fn policy_control_waits_for_foreign_test_env_overrides_to_restore() {
    // This is a deadlock ceiling, not a latency requirement. In the full
    // library suite, hundreds of environment-sensitive tests can acquire the
    // process-wide lock before this deliberately blocked reader resumes.
    const FULL_SUITE_SCHEDULING_TIMEOUT: Duration = Duration::from_secs(30);

    let temp = tempfile::tempdir().expect("tempdir");
    let config_path = temp.path().join("config.toml");
    let workspace = temp.path().join("workspace");
    let managed_config_path = temp.path().join("missing-managed.toml");
    let requirements_path = temp.path().join("missing-requirements.toml");
    let (started_tx, started_rx) = mpsc::channel();
    let (tx, rx) = mpsc::channel();

    let reader = {
        let lock = lock_test_env();
        let shell_override = EnvVarGuard::set("CODEWHALE_ALLOW_SHELL", "false");
        let approval_override = EnvVarGuard::set("DEEPSEEK_APPROVAL_POLICY", "never");
        let reader = std::thread::spawn(move || {
            started_tx.send(()).expect("signal policy read start");
            let config = Config {
                managed_config_path: Some(managed_config_path.display().to_string()),
                requirements_path: Some(requirements_path.display().to_string()),
                ..Config::default()
            };
            tx.send((
                config.allow_shell_control(Some(&config_path), None, &workspace),
                config.approval_policy_control(Some(&config_path), None, &workspace),
            ))
            .expect("send policy controls");
        });

        started_rx
            .recv_timeout(FULL_SUITE_SCHEDULING_TIMEOUT)
            .expect("reader reached policy read");
        assert!(
            rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "a foreign reader observed another test's temporary policy overrides"
        );
        drop(approval_override);
        drop(shell_override);
        drop(lock);
        reader
    };

    let (shell, approval) = rx
        .recv_timeout(FULL_SUITE_SCHEDULING_TIMEOUT)
        .expect("reader resumed after policy overrides were restored");
    reader.join().expect("reader thread");
    assert_eq!(shell, ShellAccessControl::Unset);
    assert_eq!(approval, ApprovalPolicyControl::Unset);
}

/// #6516: the dispatcher exports only `CODEWHALE_*`, so the settings editor
/// must name whichever variable actually owns the posture instead of a
/// hard-coded `DEEPSEEK_*` twin the process no longer sets.
#[test]
fn environment_policy_labels_name_the_variable_that_is_set() {
    let _lock = lock_test_env();
    let _legacy_approval = EnvVarGuard::remove("DEEPSEEK_APPROVAL_POLICY");
    let _approval = EnvVarGuard::set("CODEWHALE_APPROVAL_POLICY", "never");
    let _shell = EnvVarGuard::remove("CODEWHALE_ALLOW_SHELL");
    let _legacy_shell = EnvVarGuard::set("DEEPSEEK_ALLOW_SHELL", "false");
    assert_eq!(
        ApprovalPolicyControl::Environment.label(),
        "CODEWHALE_APPROVAL_POLICY"
    );
    assert_eq!(
        ShellAccessControl::Environment.label(),
        "DEEPSEEK_ALLOW_SHELL"
    );

    let _both_shell = EnvVarGuard::set("CODEWHALE_ALLOW_SHELL", "true");
    assert_eq!(
        ShellAccessControl::Environment.label(),
        "CODEWHALE_ALLOW_SHELL",
        "the CODEWHALE_* name wins when both are set, matching the readers"
    );
}

#[test]
fn base_url_reads_wait_for_foreign_test_env_overrides_to_restore() {
    let (started_tx, started_rx) = mpsc::channel();
    let (tx, rx) = mpsc::channel();

    let reader = {
        let lock = lock_test_env();
        let expected_after_restore = env_base_url_override();
        let override_guard =
            EnvVarGuard::set("CODEWHALE_BASE_URL", "https://temporary.test.invalid/v1");
        let reader = std::thread::spawn(move || {
            started_tx.send(()).expect("signal base URL read start");
            tx.send(env_base_url_override())
                .expect("send resolved base URL override");
        });

        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("reader reached base URL read");
        assert!(
            rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "a foreign reader observed another test's temporary base URL override"
        );
        drop(override_guard);
        drop(lock);
        (reader, expected_after_restore)
    };

    let observed = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("reader resumed after base URL override was restored");
    reader.0.join().expect("reader thread");
    assert_eq!(observed, reader.1);
}

#[test]
fn save_api_scenario() -> Result<()> {
    // Scenario consolidation of: save_api_key_onboarding_routes_openrouter_key_to_provider_table, save_api_key_rejects_empty_input, save_api_key_for_openai_codex_refuses_config_storage
    // from save_api_key_onboarding_routes_openrouter_key_to_provider_table
    {
        let _lock = lock_test_env();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_root = env::temp_dir().join(format!(
            "codewhale-tui-onboarding-provider-{}-{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&temp_root)?;
        let _guard = EnvGuard::new(&temp_root);

        let path = save_api_key_for(ProviderKind::Openrouter, "onboarding-openrouter-key")?;
        let contents = fs::read_to_string(&path)?;
        assert!(
            contents.contains("openrouter"),
            "expected OpenRouter provider table, got: {contents}"
        );
        assert!(contents.contains("onboarding-openrouter-key"));
    }
    // from save_api_key_rejects_empty_input
    {
        let _lock = lock_test_env();
        let err = save_api_key("   ").expect_err("empty should bail");
        assert!(
            err.to_string().contains("empty"),
            "expected error to mention empty, got: {err}"
        );
    }
    // from save_api_key_for_openai_codex_refuses_config_storage
    {
        let err = save_api_key_for(ProviderKind::OpenaiCodex, "codex-token")
            .expect_err("Codex OAuth tokens must not be persisted as provider API keys");

        let message = err.to_string();
        assert!(message.contains("Sign in with ChatGPT"), "{message}");
        assert!(message.contains("codewhale auth chatgpt"), "{message}");
    }
    Ok(())
}

#[test]
fn ensure_config_file_exists_creates_first_run_template() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-first-run-config-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let created = ensure_config_file_exists(None)?.expect("should create config");
    let content = fs::read_to_string(&created)?;

    assert_eq!(created, temp_root.join(".deepseek").join("config.toml"));
    assert!(content.contains(&format!("default_text_model = \"{DEFAULT_TEXT_MODEL}\"")));
    assert!(content.contains("reasoning_effort = \"auto\""));
    // Shift+Tab cycles the permission posture; effort moved to Ctrl+T.
    assert!(content.contains("# Ctrl+T in the TUI"));
    assert!(!content.contains("Shift+Tab"));
    assert!(!content.contains("api_key ="));
    assert!(ensure_config_file_exists(None)?.is_none());
    Ok(())
}

#[test]
fn workspace_trust_round_trips_through_global_config() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-workspace-trust-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);
    let workspace = temp_root.join("project");
    fs::create_dir_all(&workspace)?;

    assert!(!is_workspace_trusted(&workspace));
    let saved = save_workspace_trust(&workspace)?;

    assert_eq!(saved, temp_root.join(".deepseek").join("config.toml"));
    assert!(is_workspace_trusted(&workspace));
    assert!(!crate::config::needs_trust(&workspace));
    assert!(
        !workspace.join(".deepseek").exists(),
        "trust persistence must not create a project-local .deepseek directory"
    );

    let parsed: toml::Value = toml::from_str(&fs::read_to_string(saved)?)?;
    assert_eq!(
        workspace_trust_level_from_doc(&parsed, &workspace),
        Some("trusted")
    );
    Ok(())
}

#[test]
fn workspace_trust_reads_existing_projects_table() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-existing-project-trust-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);
    let workspace = temp_root.join("project");
    fs::create_dir_all(&workspace)?;
    let config_path = temp_root.join(".deepseek").join("config.toml");
    fs::create_dir_all(config_path.parent().unwrap())?;
    fs::write(
        &config_path,
        format!(
            "[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
            workspace_config_key(&workspace)
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        ),
    )?;

    assert!(is_workspace_trusted(&workspace));
    assert!(!crate::config::needs_trust(&workspace));
    Ok(())
}

#[test]
fn saved_credential_describe_returns_config_file_path() {
    let cf = SavedCredential::ConfigFile(PathBuf::from("/tmp/x.toml"));
    assert_eq!(cf.describe(), "/tmp/x.toml");
}

/// The durable-store outcome makes it explicit that config contains metadata,
/// not a second plaintext credential copy.
#[test]
fn saved_credential_describe_lists_both_targets_for_keyring_and_config() {
    let dual = SavedCredential::KeyringAndConfigFile {
        backend: "system keyring".to_string(),
        path: PathBuf::from("/tmp/x.toml"),
    };
    assert_eq!(
        dual.describe(),
        "secret store (system keyring); credential-free config metadata in /tmp/x.toml"
    );
}

#[test]
fn save_deepseek_key_uses_isolated_file_store_without_plaintext_config() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let codewhale_home = temp_root.path().join("codewhale-home");
    let config_path = codewhale_home.join("config.toml");
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");

    let saved = save_api_key("deepseek-test-credential")?;
    assert!(matches!(
        saved,
        SavedCredential::KeyringAndConfigFile { .. }
    ));

    let config = fs::read_to_string(&config_path)?;
    assert!(!config.contains("deepseek-test-credential"), "{config}");
    assert!(
        !config
            .lines()
            .any(|line| line.trim_start().starts_with("api_key ="))
    );
    let parsed: toml::Value = toml::from_str(&config)?;
    assert_eq!(
        parsed["providers"]["deepseek"]["auth_mode"].as_str(),
        Some("api_key")
    );
    // Saving a key leaves the model to the provider default.
    assert!(!config.contains("default_text_model"), "{config}");
    assert_eq!(
        codewhale_secrets::Secrets::auto_detect().get("deepseek")?,
        Some("deepseek-test-credential".to_string())
    );
    Ok(())
}

/// #5196: logout must remove the durable credential, not just the config
/// file entry. After save + `clear_api_key()`, the whole read chain —
/// secret-store slot first, config file second — must find nothing.
#[test]
fn full_logout_clears_secret_store_slot_and_config_document() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    // Canonicalize: the xAI credential walker opens each path component with
    // O_NOFOLLOW, so the lexical `/var` symlink in macOS tempdirs fails.
    let temp_root = temp_root.path().canonicalize()?;
    let _guard = EnvGuard::new(&temp_root);
    let codewhale_home = temp_root.join("codewhale-home");
    let config_path = codewhale_home.join("config.toml");
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");

    let saved = save_api_key("logout-credential")?;
    assert!(matches!(
        saved,
        SavedCredential::KeyringAndConfigFile { .. }
    ));
    assert_eq!(
        codewhale_secrets::Secrets::auto_detect().get("deepseek")?,
        Some("logout-credential".to_string())
    );

    clear_api_key()?;

    assert_eq!(
        codewhale_secrets::Secrets::auto_detect().get("deepseek")?,
        None,
        "logout must delete the durable secret-store slot"
    );
    assert_eq!(
        provider_secret_store_api_key(
            &Config::default(),
            &(Config::default()).test_identity_for_kind(ProviderKind::Deepseek)
        ),
        None,
        "the read chain must not find a cleared credential"
    );
    let config = fs::read_to_string(&config_path)?;
    assert!(!config.contains("logout-credential"), "{config}");
    assert!(
        !config
            .lines()
            .any(|line| line.trim_start().starts_with("api_key =")),
        "{config}"
    );
    Ok(())
}

/// #5196: the single-provider clear used by TUI `/logout` must delete that
/// provider's secret-store slot as well as its config-file entry.
#[test]
fn single_provider_logout_clears_secret_store_slot() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let temp_root = temp_root.path().canonicalize()?;
    let _guard = EnvGuard::new(&temp_root);
    let codewhale_home = temp_root.join("codewhale-home");
    let config_path = codewhale_home.join("config.toml");
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");

    save_api_key_for(ProviderKind::Openrouter, "openrouter-logout-credential")?;
    assert_eq!(
        codewhale_secrets::Secrets::auto_detect().get("openrouter")?,
        Some("openrouter-logout-credential".to_string())
    );

    clear_active_provider_api_key("openrouter")?;

    assert_eq!(
        codewhale_secrets::Secrets::auto_detect().get("openrouter")?,
        None,
        "single-provider logout must delete the durable secret-store slot"
    );
    assert_eq!(
        provider_secret_store_api_key(
            &Config::default(),
            &(Config::default()).test_identity_for_kind(ProviderKind::Openrouter)
        ),
        None,
        "the read chain must not find a cleared credential"
    );
    Ok(())
}

fn inject_plaintext_openrouter_key(config_path: &std::path::Path) -> Result<()> {
    let contents = fs::read_to_string(config_path)?;
    anyhow::ensure!(
        contents.contains("[providers.openrouter]"),
        "save must have created the provider table: {contents}"
    );
    fs::write(
        config_path,
        contents.replace(
            "[providers.openrouter]",
            "[providers.openrouter]\napi_key = \"logout-lock-plaintext\"",
        ),
    )?;
    Ok(())
}

struct ReleaseOnDrop(Option<mpsc::Sender<()>>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

/// Logout used to mutate the config document and (for `/logout`) the durable
/// slot with no write lock held, so a save racing a logout on one slot could
/// leave the store and the config file disagreeing. Both logout paths now
/// hold that provider's lock across the whole sequence.
#[test]
fn single_provider_logout_holds_the_slot_write_lock_across_config_and_store() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let temp_root = temp_root.path().canonicalize()?;
    let _guard = EnvGuard::new(&temp_root);
    let codewhale_home = temp_root.join("codewhale-home");
    let config_path = codewhale_home.join("config.toml");
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");

    save_api_key_for(ProviderKind::Openrouter, "openrouter-lock-credential")?;
    inject_plaintext_openrouter_key(&config_path)?;

    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        crate::credentials::store::with_provider_write_lock("openrouter", || {
            let _ = held_tx.send(());
            let _ = release_rx.recv();
        });
    });
    held_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("holder acquired the slot lock");
    let release = ReleaseOnDrop(Some(release_tx));

    let ticket = env_scope_ticket();
    let (done_tx, done_rx) = mpsc::channel();
    let logout = std::thread::spawn(move || {
        let _membership = join_env_scope(ticket);
        done_tx
            .send(clear_active_provider_api_key("openrouter"))
            .expect("send logout result");
    });

    let deadline = std::time::Instant::now() + Duration::from_millis(400);
    while std::time::Instant::now() < deadline {
        assert!(
            matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "single-provider logout finished while the slot write lock was held"
        );
        let config = fs::read_to_string(&config_path)?;
        assert!(
            config.contains("logout-lock-plaintext"),
            "logout must not mutate the config document before it holds the slot lock: {config}"
        );
        assert_eq!(
            codewhale_secrets::Secrets::auto_detect().get("openrouter")?,
            Some("openrouter-lock-credential".to_string()),
            "logout must not delete the slot while the write lock is held"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    drop(release);
    holder.join().expect("holder thread");
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("logout finished")?;
    logout.join().expect("logout thread");

    assert_eq!(
        codewhale_secrets::Secrets::auto_detect().get("openrouter")?,
        None,
        "logout must delete the durable slot once the lock is released"
    );
    let config = fs::read_to_string(&config_path)?;
    assert!(
        !config.contains("logout-lock-plaintext"),
        "logout must strip the injected plaintext once the lock is released: {config}"
    );
    Ok(())
}

#[test]
fn full_logout_holds_every_slot_write_lock_across_config_and_store() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let temp_root = temp_root.path().canonicalize()?;
    let _guard = EnvGuard::new(&temp_root);
    let codewhale_home = temp_root.join("codewhale-home");
    let config_path = codewhale_home.join("config.toml");
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");

    save_api_key_for(ProviderKind::Openrouter, "openrouter-lock-credential")?;
    inject_plaintext_openrouter_key(&config_path)?;

    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        crate::credentials::store::with_provider_write_lock("openrouter", || {
            let _ = held_tx.send(());
            let _ = release_rx.recv();
        });
    });
    held_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("holder acquired the slot lock");
    let release = ReleaseOnDrop(Some(release_tx));

    let ticket = env_scope_ticket();
    let (done_tx, done_rx) = mpsc::channel();
    let logout = std::thread::spawn(move || {
        let _membership = join_env_scope(ticket);
        done_tx.send(clear_api_key()).expect("send logout result");
    });

    let deadline = std::time::Instant::now() + Duration::from_millis(400);
    while std::time::Instant::now() < deadline {
        assert!(
            matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "full logout finished while a slot write lock was held"
        );
        let config = fs::read_to_string(&config_path)?;
        assert!(
            config.contains("logout-lock-plaintext"),
            "full logout must not mutate the config document before it holds the slot locks: {config}"
        );
        assert_eq!(
            codewhale_secrets::Secrets::auto_detect().get("openrouter")?,
            Some("openrouter-lock-credential".to_string()),
            "full logout must not delete a slot while that slot's write lock is held"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    drop(release);
    holder.join().expect("holder thread");
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("logout finished")?;
    logout.join().expect("logout thread");

    assert_eq!(
        codewhale_secrets::Secrets::auto_detect().get("openrouter")?,
        None,
        "full logout must delete the durable slot once the lock is released"
    );
    let config = fs::read_to_string(&config_path)?;
    assert!(
        !config.contains("logout-lock-plaintext"),
        "full logout must strip the injected plaintext once the lock is released: {config}"
    );
    Ok(())
}

/// #5194: when both a config-file api_key and the provider's secret-store
/// slot hold a credential, the shadowing warning names both sources, says
/// which won, and hands over the resolve command.
#[test]
fn config_api_key_shadow_warning_names_sources_winner_and_resolution() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let codewhale_home = temp_root.path().join("codewhale-home");
    let config_path = codewhale_home.join("config.toml");
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    codewhale_secrets::Secrets::auto_detect().set("openrouter", "store-key")?;

    let mut config = Config::default();
    config
        .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Openrouter))
        .unwrap()
        .api_key = Some("plaintext-config-key".to_string());

    let warning = config_api_key_shadow_warning(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Openrouter),
        "`providers.openrouter` api_key",
    )
    .expect("a live secret-store slot shadowed by a config key must warn");
    assert!(
        warning.contains("`providers.openrouter` api_key"),
        "warning must name the config-file source: {warning}"
    );
    assert!(
        warning.contains("secret-store slot \"openrouter\""),
        "warning must name the secret-store source: {warning}"
    );
    assert!(
        warning.contains("the config-file key won"),
        "warning must say which source won: {warning}"
    );
    assert!(
        warning.contains("codewhale auth set --provider openrouter"),
        "warning must name the resolve command: {warning}"
    );
    Ok(())
}

/// #5194: no secret-store credential, no shadow, no warning.
#[test]
fn config_api_key_shadow_warning_stays_quiet_without_a_store_slot() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let codewhale_home = temp_root.path().join("codewhale-home");
    let config_path = codewhale_home.join("config.toml");
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");

    let mut config = Config::default();
    config
        .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Openrouter))
        .unwrap()
        .api_key = Some("plaintext-config-key".to_string());

    assert_eq!(
        config_api_key_shadow_warning(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openrouter),
            "`providers.openrouter` api_key"
        ),
        None,
        "a config key with no secret-store slot behind it is not a shadow"
    );
    Ok(())
}

#[test]
fn whitespace_codewhale_home_never_opens_ambient_file_secret_store() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let ambient_home = temp_root.path().join("ambient-home");
    let config_path = temp_root.path().join("isolated-config.toml");
    fs::create_dir_all(&ambient_home)?;
    let _home = EnvVarGuard::set("HOME", &ambient_home);
    let _userprofile = EnvVarGuard::set("USERPROFILE", &ambient_home);
    let _codewhale_home_unset = EnvVarGuard::remove("CODEWHALE_HOME");
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", &config_path);
    let ambient_store = codewhale_secrets::Secrets::file_backed();
    ambient_store.set("deepseek", "ambient-secret-sentinel")?;
    let ambient_secret_path = ambient_home
        .join(".codewhale")
        .join("secrets")
        .join("secrets.json");
    let before = fs::read(&ambient_secret_path)?;
    let _whitespace_home = EnvVarGuard::set("CODEWHALE_HOME", " \t ");
    let resolved_config_path = codewhale_config::resolve_config_path(None)?;

    let read = provider_secret_store_api_key(
        &Config::default(),
        &(Config::default()).test_identity_for_kind(ProviderKind::Deepseek),
    );
    let saved = save_api_key("replacement-secret-sentinel")?;
    let after = fs::read(&ambient_secret_path)?;

    assert_eq!(
        read, None,
        "whitespace must not opt tests into ambient reads"
    );
    assert_eq!(saved, SavedCredential::ConfigFile(resolved_config_path));
    assert_eq!(after, before, "ambient file secret store was modified");
    Ok(())
}

#[test]
fn save_non_deepseek_key_uses_isolated_file_store_without_plaintext_config() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let codewhale_home = temp_root.path().join("codewhale-home");
    let config_path = codewhale_home.join("config.toml");
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");

    save_api_key_for(ProviderKind::Openrouter, "openrouter-test-credential")?;

    let config = fs::read_to_string(&config_path)?;
    assert!(!config.contains("openrouter-test-credential"), "{config}");
    let parsed: toml::Value = toml::from_str(&config)?;
    let openrouter = parsed
        .get("providers")
        .and_then(|providers| providers.get("openrouter"))
        .expect("openrouter metadata table");
    assert!(openrouter.get("api_key").is_none());
    assert_eq!(
        openrouter.get("auth_mode").and_then(toml::Value::as_str),
        Some("api_key")
    );
    assert_eq!(
        codewhale_secrets::Secrets::auto_detect().get("openrouter")?,
        Some("openrouter-test-credential".to_string())
    );
    Ok(())
}

#[test]
fn provider_api_key_config_failure_restores_secret_and_keeps_external_route() -> Result<()> {
    let _lock = lock_test_env();
    for prior in [None, Some("prior-xai-secret")] {
        let temp_root = tempfile::tempdir()?;
        let _guard = EnvGuard::new(temp_root.path());
        let codewhale_home = temp_root.path().canonicalize()?.join("codewhale-home");
        fs::create_dir_all(&codewhale_home)?;
        let config_path = codewhale_home.join("config.toml");
        fs::create_dir(&config_path)?;
        let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
        let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", &config_path);
        let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let generation = "xai-auth-0123456789abcdef0123456789abcdef.json";
        codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
            store.write(generation, b"prior-owned-epoch", false)
        })?;
        let secrets = codewhale_secrets::Secrets::auto_detect();
        if let Some(prior) = prior {
            secrets.set("xai", prior)?;
        }
        let external_path = temp_root.path().join("external-grok.json");
        let route_config = Config {
            provider: Some(ProviderKind::Xai.as_str().to_string()),
            providers: Some(ProvidersConfig {
                xai: ProviderConfig {
                    auth_mode: Some("oauth".to_string()),
                    oauth_credential_generation: Some(generation.to_string()),
                    external_credentials: Some(
                        codewhale_config::ExternalCredentialConsentToml::read_only(
                            codewhale_config::ProviderKind::Xai,
                            codewhale_config::ExternalCredentialSource::GrokCli,
                            external_path,
                        ),
                    ),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            }),
            ..Config::default()
        };
        let identity = ProviderIdentity {
            provider: ProviderKind::Xai,
            key: ProviderKind::Xai.as_str().into(),
            exact_id: Some(ProviderKind::Xai.as_str().into()),
            migrated_legacy_ollama_cloud_route: false,
            legacy_root_custom_generation: None,
        };
        let error = save_api_key_for_identity(&identity, &route_config, "new-xai-secret")
            .expect_err("config directory must reject metadata mutation");
        assert!(error.to_string().contains("config"), "{error:#}");
        assert_eq!(secrets.get("xai")?, prior.map(str::to_string));
        let xai = route_config
            .provider_config_for(&route_config.test_identity_for_kind(ProviderKind::Xai))
            .expect("unchanged live route");
        assert_eq!(xai.auth_mode.as_deref(), Some("oauth"));
        assert!(xai.external_credentials.is_some());
        assert!(config_path.is_dir());
        assert_eq!(
            fs::read(codewhale_home.join("credentials").join(generation))?,
            b"prior-owned-epoch",
            "failed API-key mode switch must restore the prior OAuth epoch"
        );
    }
    Ok(())
}

#[test]
fn root_api_key_config_failure_restores_absent_and_existing_secret_state() -> Result<()> {
    let _lock = lock_test_env();
    for prior in [None, Some("prior-deepseek-secret")] {
        let temp_root = tempfile::tempdir()?;
        let _guard = EnvGuard::new(temp_root.path());
        let codewhale_home = temp_root.path().join("codewhale-home");
        fs::create_dir_all(&codewhale_home)?;
        let config_path = codewhale_home.join("config.toml");
        fs::create_dir(&config_path)?;
        let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
        let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", &config_path);
        let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let secrets = codewhale_secrets::Secrets::auto_detect();
        if let Some(prior) = prior {
            secrets.set("deepseek", prior)?;
        }

        let error = save_api_key("new-deepseek-secret")
            .expect_err("config directory must reject root metadata mutation");
        assert!(error.to_string().contains("config"), "{error:#}");
        assert_eq!(secrets.get("deepseek")?, prior.map(str::to_string));
        assert!(config_path.is_dir());
    }
    Ok(())
}

#[test]
fn save_key_refuses_plaintext_config_when_isolated_file_store_is_unwritable() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let codewhale_home = temp_root.path().join("codewhale-home");
    let config_path = codewhale_home.join("config.toml");
    fs::create_dir_all(codewhale_home.join("secrets"))?;
    fs::write(
        codewhale_home.join("secrets/secrets.json"),
        "not valid json",
    )?;
    #[cfg(unix)]
    fs::set_permissions(
        codewhale_home.join("secrets/secrets.json"),
        fs::Permissions::from_mode(0o600),
    )?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let resolved_config_path = codewhale_config::resolve_config_path(None)?;

    let error = save_api_key("fallback-test-credential")
        .expect_err("secret-store failure must not downgrade to plaintext");
    let message = format!("{error:#}");
    assert!(message.contains("Secret storage"), "{message}");
    assert!(message.contains("Refusing"), "{message}");
    assert!(
        message.contains(&codewhale_config::quote_os_path(&resolved_config_path)),
        "{message}"
    );
    assert!(
        !resolved_config_path.exists(),
        "plaintext config must stay untouched"
    );
    Ok(())
}

#[test]
fn provider_key_refuses_plaintext_config_when_secret_store_snapshot_fails() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let codewhale_home = temp_root.path().join("codewhale-home");
    let config_path = codewhale_home.join("config.toml");
    fs::create_dir_all(codewhale_home.join("secrets"))?;
    fs::write(
        codewhale_home.join("secrets/secrets.json"),
        "not valid json",
    )?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", &config_path);
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let resolved_config_path = codewhale_config::resolve_config_path(None)?;
    let identity = ProviderIdentity {
        provider: ProviderKind::Openrouter,
        key: ProviderKind::Openrouter.as_str().into(),
        exact_id: Some(ProviderKind::Openrouter.as_str().into()),
        migrated_legacy_ollama_cloud_route: false,
        legacy_root_custom_generation: None,
    };

    let error = save_api_key_for_identity(&identity, &Config::default(), "provider-fallback-key")
        .expect_err("provider key must not downgrade to plaintext");
    let message = format!("{error:#}");
    assert!(message.contains("snapshot"), "{message}");
    assert!(
        message.contains(&codewhale_config::quote_os_path(&resolved_config_path)),
        "{message}"
    );
    assert!(
        !resolved_config_path.exists(),
        "plaintext config must stay untouched"
    );
    Ok(())
}

#[test]
fn relative_codewhale_home_key_save_creates_no_workspace_state() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let relative_home = PathBuf::from(format!(
        ".codewhale-relative-home-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    assert!(!relative_home.exists());
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &relative_home);
    let _config_path = EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
    let _legacy_config_path = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");

    let error = save_api_key("never-persisted")
        .expect_err("relative CODEWHALE_HOME must fail before persistence");
    let message = format!("{error:#}");
    assert!(message.contains("CODEWHALE_HOME"), "{message}");
    assert!(message.contains("absolute"), "{message}");
    assert!(
        !relative_home.exists(),
        "relative home must not create workspace state"
    );
    Ok(())
}

#[test]
fn deepseek_dispatcher_env_key_overrides_config_key() -> Result<()> {
    let _lock = lock_test_env();
    let prev_source = std::env::var_os("DEEPSEEK_API_KEY_SOURCE");
    unsafe {
        std::env::set_var("DEEPSEEK_API_KEY", "ark-dispatcher-key");
        std::env::set_var("DEEPSEEK_API_KEY_SOURCE", "cli");
    }
    let config = Config {
        ..Default::default()
    }
    .with_legacy_root(Some("saved-deepseek-key".to_string()), None);

    assert_eq!(config.active_route_api_key()?, "ark-dispatcher-key");

    unsafe {
        std::env::remove_var("DEEPSEEK_API_KEY");
        match prev_source {
            Some(value) => std::env::set_var("DEEPSEEK_API_KEY_SOURCE", value),
            None => std::env::remove_var("DEEPSEEK_API_KEY_SOURCE"),
        }
    }
    Ok(())
}

/// Regression for #343: clear_api_key strips both the root `api_key`
/// and any nested `[providers.<name>].api_key` lines from config.toml
/// so a stale credential can't shadow a fresh login.
#[test]
fn clear_api_key_strips_root_and_provider_scoped_keys() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-clear-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let temp_root = temp_root.canonicalize()?;
    let _guard = EnvGuard::new(&temp_root);

    let config_dir = temp_root.join(".deepseek");
    fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("config.toml");
    fs::write(
        &config_path,
        r#"api_key = "old-root-key"
default_text_model = "deepseek-v4-flash"

[providers.deepseek]
api_key = "old-provider-key"
base_url = "https://api.deepseek.com"

[providers.openrouter]
api_key = "old-openrouter-key"
"#,
    )?;

    clear_api_key()?;

    let after = fs::read_to_string(&config_path)?;
    assert!(
        !after.contains("old-root-key"),
        "root api_key must be stripped: {after}"
    );
    assert!(
        !after.contains("old-provider-key"),
        "provider-scoped codewhale key must be stripped: {after}"
    );
    assert!(
        !after.contains("old-openrouter-key"),
        "provider-scoped openrouter key must be stripped: {after}"
    );
    // Non-credential lines must survive.
    assert!(after.contains("default_text_model"));
    assert!(after.contains("base_url"));
    Ok(())
}

/// Finding #20 golden: a comment that merely mentions `api_key` used to
/// defeat the insert (the old `existing.contains("api_key")` scan treated it
/// as an existing assignment and never wrote the key). The TOML-aware path
/// must insert the real key and keep the comment.
#[test]
fn save_api_key_inserts_key_when_only_a_comment_mentions_it() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-api-key-comment-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    fs::create_dir_all(config_path.parent().unwrap())?;
    fs::write(
        &config_path,
        "# api_key = \"sk-placeholder\" (uncomment to set manually)\n\
         default_text_model = \"deepseek-v4-flash\"\n",
    )?;

    save_api_key("fresh-key")?;

    let after = fs::read_to_string(&config_path)?;
    assert!(
        after.contains("# api_key = \"sk-placeholder\""),
        "comment must survive: {after}"
    );
    assert!(
        after.contains("default_text_model = \"deepseek-v4-flash\""),
        "unrelated key must survive: {after}"
    );
    let parsed: toml::Value = toml::from_str(&after)?;
    assert_eq!(
        parsed["providers"]["deepseek"]
            .get("api_key")
            .and_then(toml::Value::as_str),
        Some("fresh-key"),
        "real key must be inserted despite the comment: {after}"
    );
    assert!(parsed.get("api_key").is_none(), "{after}");
    Ok(())
}

/// Replacing an existing root api_key must keep surrounding comments,
/// including the trailing comment on the api_key line itself.
#[test]
fn save_api_key_replaces_existing_key_preserving_comments() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-api-key-replace-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    fs::create_dir_all(config_path.parent().unwrap())?;
    fs::write(
        &config_path,
        r#"# top note
api_key = "old-key" # keep secret
model = "deepseek-v4-pro"

# provider note
[providers.openrouter]
base_url = "https://openrouter.ai/api/v1"
"#,
    )?;

    save_api_key("new-key")?;

    // The key moved into `[providers.deepseek]` with its comment (#6394).
    let after = fs::read_to_string(&config_path)?;
    assert!(
        after.contains("api_key = \"new-key\" # keep secret"),
        "value must be replaced in place with its comment: {after}"
    );
    let parsed: toml::Table = toml::from_str(&after)?;
    assert_eq!(
        parsed["providers"]["deepseek"]["api_key"].as_str(),
        Some("new-key")
    );
    assert!(!after.contains("old-key"), "{after}");
    assert!(after.contains("# top note"), "{after}");
    assert!(after.contains("# provider note"), "{after}");
    Ok(())
}

/// Provider-scoped key saves used to round-trip through `toml::Value`
/// pretty-printing, which dropped every comment in the file.
#[test]
fn save_api_key_for_preserves_comments_in_provider_tables() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-provider-key-comments-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    fs::create_dir_all(config_path.parent().unwrap())?;
    fs::write(
        &config_path,
        r#"# root note
model = "deepseek-v4-pro"

# openrouter note
[providers.openrouter]
base_url = "https://openrouter.ai/api/v1" # pinned
"#,
    )?;

    save_api_key_for(ProviderKind::Openrouter, "or-key")?;

    let after = fs::read_to_string(&config_path)?;
    assert!(after.contains("# root note"), "{after}");
    assert!(after.contains("# openrouter note"), "{after}");
    assert!(
        after.contains("base_url = \"https://openrouter.ai/api/v1\" # pinned"),
        "inline comment must survive: {after}"
    );
    let parsed: toml::Value = toml::from_str(&after)?;
    assert_eq!(
        parsed
            .get("providers")
            .and_then(|providers| providers.get("openrouter"))
            .and_then(|entry| entry.get("api_key"))
            .and_then(toml::Value::as_str),
        Some("or-key"),
        "{after}"
    );
    Ok(())
}

/// Clearing credentials must not disturb comments, `api_key_env`, or
/// provider tables with quoted names.
#[test]
fn clear_api_key_preserves_comments_and_unrelated_keys() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-clear-comments-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let temp_root = temp_root.canonicalize()?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    fs::create_dir_all(config_path.parent().unwrap())?;
    fs::write(
        &config_path,
        r#"# root note
api_key = "old-root-key"
api_key_env = "MY_KEY_ENV"
model = "deepseek-v4-pro"

# provider note
[providers."quoted.provider"]
api_key = "old-quoted-key"
base_url = "https://quoted.example/v1"
"#,
    )?;

    clear_api_key()?;

    let after = fs::read_to_string(&config_path)?;
    assert!(!after.contains("old-root-key"), "{after}");
    assert!(
        !after.contains("old-quoted-key"),
        "quoted provider table key must be stripped: {after}"
    );
    assert!(
        after.contains("api_key_env = \"MY_KEY_ENV\""),
        "api_key_env must not be stripped: {after}"
    );
    assert!(after.contains("# root note"), "{after}");
    assert!(after.contains("# provider note"), "{after}");
    assert!(after.contains("model = \"deepseek-v4-pro\""), "{after}");
    assert!(
        after.contains("base_url = \"https://quoted.example/v1\""),
        "{after}"
    );
    Ok(())
}

/// The old line matcher compared against the literal `[providers.<name>]`
/// header, so a quoted header (`[providers."openrouter"]`) was never
/// matched and the key survived a targeted clear.
#[test]
fn clear_active_provider_api_key_handles_quoted_table_headers() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-clear-quoted-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    fs::create_dir_all(config_path.parent().unwrap())?;
    fs::write(
        &config_path,
        r#"api_key = "root-key"

[providers."openrouter"]
api_key = "old-openrouter-key"
base_url = "https://openrouter.ai/api/v1"
"#,
    )?;

    clear_active_provider_api_key("openrouter")?;

    let after = fs::read_to_string(&config_path)?;
    assert!(
        !after.contains("old-openrouter-key"),
        "quoted provider header must be matched: {after}"
    );
    assert!(
        after.contains("api_key = \"root-key\""),
        "root key belongs to deepseek and must survive: {after}"
    );
    assert!(
        after.contains("base_url = \"https://openrouter.ai/api/v1\""),
        "{after}"
    );
    Ok(())
}

#[test]
fn clear_active_provider_api_key_clears_deepseek_cn_root_scope() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-clear-deepseek-cn-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);
    let config_path = temp_root.join(".deepseek").join("config.toml");
    fs::create_dir_all(config_path.parent().unwrap())?;
    fs::write(
        &config_path,
        r#"provider = "deepseek-cn"
api_key = "deepseek-cn-root-key"

[providers.deepseek-cn]
api_key = "deepseek-cn-table-key"

[providers.openrouter]
api_key = "unrelated-key"
"#,
    )?;

    clear_active_provider_api_key("deepseek-cn")?;

    let after = fs::read_to_string(&config_path)?;
    assert!(!after.contains("deepseek-cn-root-key"), "{after}");
    assert!(!after.contains("deepseek-cn-table-key"), "{after}");
    assert!(after.contains("unrelated-key"), "{after}");
    Ok(())
}

#[test]
fn clear_active_provider_api_key_deepseek_cn_keeps_deepseek_own_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-clear-deepseek-cn-keeps-intl-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);
    let config_path = temp_root.join(".deepseek").join("config.toml");
    fs::create_dir_all(config_path.parent().unwrap())?;

    // Each identity holds its own key: signing CN out leaves DeepSeek's.
    fs::write(
        &config_path,
        r#"provider = "deepseek-cn"

[providers.deepseek]
api_key = "sk-intl"

[providers.deepseek_cn]
api_key = "sk-cn"
"#,
    )?;
    clear_active_provider_api_key("deepseek-cn")?;
    let after = fs::read_to_string(&config_path)?;
    assert!(!after.contains("sk-cn"), "{after}");
    assert!(after.contains("sk-intl"), "{after}");

    // CN without a key of its own reads DeepSeek's, so that shared key goes.
    fs::write(
        &config_path,
        r#"provider = "deepseek-cn"

[providers.deepseek]
api_key = "sk-shared"
"#,
    )?;
    clear_active_provider_api_key("deepseek-cn")?;
    let after = fs::read_to_string(&config_path)?;
    assert!(!after.contains("sk-shared"), "{after}");

    // A top-level key this write moves beside CN's own key was the shared
    // one; a DeepSeek key that only merged with it was DeepSeek's own too.
    fs::write(
        &config_path,
        r#"provider = "deepseek-cn"
api_key = "sk-root"

[providers.deepseek]
api_key = "sk-root"

[providers.deepseek_cn]
api_key = "sk-cn"
"#,
    )?;
    clear_active_provider_api_key("deepseek-cn")?;
    let after = fs::read_to_string(&config_path)?;
    assert!(!after.contains("sk-cn"), "{after}");
    assert!(after.contains("[providers.deepseek]"), "{after}");
    assert!(after.contains("sk-root"), "{after}");
    Ok(())
}

#[test]
fn clear_active_provider_api_key_distinguishes_literal_and_named_custom_routes() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-clear-custom-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);
    let config_path = temp_root.join(".deepseek").join("config.toml");
    fs::create_dir_all(config_path.parent().unwrap())?;
    let contents = r#"provider = "custom"
api_key = "legacy-root-key"
base_url = "http://127.0.0.1:1234/v1"
default_text_model = "legacy-model"

[providers.lm-studio]
kind = "openai-compatible"
api_key = "named-route-key"
base_url = "http://127.0.0.1:5678/v1"
model = "named-model"
"#;
    fs::write(&config_path, contents)?;

    clear_active_provider_api_key("custom")?;

    let after_literal = fs::read_to_string(&config_path)?;
    assert!(
        !after_literal.contains("legacy-root-key"),
        "{after_literal}"
    );
    assert!(after_literal.contains("named-route-key"), "{after_literal}");

    fs::write(&config_path, contents)?;
    clear_active_provider_api_key("lm-studio")?;

    let after_named = fs::read_to_string(&config_path)?;
    assert!(after_named.contains("legacy-root-key"), "{after_named}");
    assert!(!after_named.contains("named-route-key"), "{after_named}");
    Ok(())
}

#[test]
fn clear_active_provider_api_key_prefers_exact_custom_table_over_legacy_root() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-clear-exact-custom-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);
    let config_path = temp_root.join(".deepseek").join("config.toml");
    fs::create_dir_all(config_path.parent().unwrap())?;
    fs::write(
        &config_path,
        r#"provider = "custom"
api_key = "legacy-root-key"
base_url = "http://127.0.0.1:1234/v1"
default_text_model = "legacy-model"

[providers.custom]
kind = "openai-compatible"
api_key = "exact-table-key"
base_url = "http://127.0.0.1:5678/v1"
model = "exact-model"
"#,
    )?;

    clear_active_provider_api_key("custom")?;

    let after = fs::read_to_string(&config_path)?;
    assert!(after.contains("legacy-root-key"), "{after}");
    assert!(!after.contains("exact-table-key"), "{after}");
    assert!(after.contains("[providers.custom]"), "{after}");
    Ok(())
}

/// Finding #19: workspace-trust saves used to round-trip through
/// `toml::to_string_pretty`, destroying comments in the whole file.
#[test]
fn save_workspace_trust_preserves_comments() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-trust-comments-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);
    let workspace = temp_root.join("project");
    fs::create_dir_all(&workspace)?;

    let config_path = temp_root.join(".deepseek").join("config.toml");
    fs::create_dir_all(config_path.parent().unwrap())?;
    fs::write(
        &config_path,
        r#"# top note
model = "deepseek-v4-pro"

# projects note
[projects."/existing/workspace"]
trust_level = "trusted" # granted earlier
"#,
    )?;

    save_workspace_trust(&workspace)?;

    let after = fs::read_to_string(&config_path)?;
    assert!(after.contains("# top note"), "{after}");
    assert!(after.contains("# projects note"), "{after}");
    assert!(after.contains("# granted earlier"), "{after}");
    assert!(
        after.contains("[projects.\"/existing/workspace\"]"),
        "existing project entry must survive: {after}"
    );
    assert!(is_workspace_trusted(&workspace));
    Ok(())
}

/// Regression for #343: explicit in-memory `api_key` (non-empty,
/// non-sentinel) wins over env/config so a freshly-typed onboarding
/// key takes effect immediately.
#[test]
fn deepseek_api_key_prefers_explicit_in_memory_override() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-override-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config = Config {
        ..Config::default()
    }
    .with_legacy_root(Some("freshly-typed-key".to_string()), None);
    let resolved = config
        .active_route_api_key()
        .expect("explicit override must resolve");
    assert_eq!(resolved, "freshly-typed-key");
    Ok(())
}

#[test]
fn deepseek_api_key_prefers_saved_config_over_stale_env() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-config-over-env-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    unsafe {
        env::set_var("DEEPSEEK_API_KEY", "stale-env-key");
    }
    let config = Config {
        ..Config::default()
    }
    .with_legacy_root(Some("fresh-config-key".to_string()), None);
    assert_eq!(config.active_route_api_key()?, "fresh-config-key");
    unsafe {
        env::remove_var("DEEPSEEK_API_KEY");
    }
    Ok(())
}

#[test]
fn env_base_url_provenance_tracks_the_route_across_provider_switches() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    fs::write(
        &config_path,
        r#"provider = "openrouter"

[providers.openrouter]
api_key = "stale-openrouter-file-key"
model = "openrouter-model"

[providers.openai]
api_key = "file-openai-key"
base_url = "https://file-openai-gateway.example.test/v1"
model = "private-openai-model"

[providers.anthropic]
api_key = "stale-anthropic-file-key"
model = "claude-sonnet-5"
"#,
    )?;
    let _base = EnvVarGuard::set("CODEWHALE_BASE_URL", "https://env-gateway.example.test/v1");
    let mut config = Config::load(Some(config_path), None)?;
    assert!(config.active_route_api_key().is_err());

    config.provider = Some("openai".to_string());
    assert_eq!(
        config.active_route_base_url(),
        "https://file-openai-gateway.example.test/v1"
    );
    assert_eq!(config.active_route_api_key()?, "file-openai-key");

    // Anthropic was never the route the environment addressed. Under the
    // endpoint-ownership receipt the generic override does not follow a
    // re-pointed config onto another vendor's route — that is the same
    // mechanism a pinned cross-provider child is resolved through, and it must
    // not be able to dispatch Anthropic traffic at the DeepSeek session's
    // gateway. Anthropic therefore resolves its own canonical endpoint, and
    // because it is no longer on an env-selected host its file-owned key is a
    // legitimate route-bound credential rather than one following a foreign
    // host.
    config.provider = Some("anthropic".to_string());
    assert_eq!(config.active_route_base_url(), DEFAULT_ANTHROPIC_BASE_URL);
    assert_ne!(
        config.active_route_base_url(),
        "https://env-gateway.example.test/v1"
    );
    assert_eq!(config.active_route_api_key()?, "stale-anthropic-file-key");

    config.provider = Some("openrouter".to_string());
    assert!(
        config.active_route_api_key().is_err(),
        "switching away and back must retain the env ownership receipt for that route"
    );
    Ok(())
}

#[test]
fn active_provider_detects_env_only_api_key() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root =
        env::temp_dir().join(format!("codewhale-tui-env-only-key-{}", std::process::id()));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    unsafe {
        env::set_var("DEEPSEEK_API_KEY", "env-only-key");
    }
    let mut config = Config::default();
    assert!(active_provider_has_env_api_key(&config));
    assert!(!active_provider_has_config_api_key(&config));
    assert!(active_provider_uses_env_only_api_key(&config));

    config.set_legacy_root(Some("config-key".to_string()), None);
    assert!(active_provider_has_config_api_key(&config));
    assert!(!active_provider_uses_env_only_api_key(&config));

    unsafe {
        env::remove_var("DEEPSEEK_API_KEY");
    }
    Ok(())
}

#[test]
fn deepseek_api_key_ignores_sentinel_placeholder() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-sentinel-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config = Config {
        ..Config::default()
    }
    .with_legacy_root(Some(API_KEYRING_SENTINEL.to_string()), None);
    // Sentinel must not be treated as a real key — the resolver should
    // fall through to env / config-provider and ultimately bail out
    // with a "key not found" error.
    let _err = config
        .active_route_api_key()
        .expect_err("sentinel placeholder must not satisfy the API key check");
    Ok(())
}

#[test]
fn default_user_paths_use_codewhale_home_for_fresh_installs() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-fresh-home-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // EnvGuard pins DEEPSEEK_CONFIG_PATH for older tests; this test wants
    // the no-explicit-path startup behavior.
    unsafe {
        env::remove_var("DEEPSEEK_CONFIG_PATH");
    }

    let config = Config::default();
    assert_eq!(
        default_config_path().unwrap(),
        temp_root.join(".codewhale").join("config.toml")
    );
    assert_eq!(
        config.mcp_config_path(),
        temp_root.join(".codewhale").join("mcp.json")
    );
    assert_eq!(
        config.notes_path(),
        temp_root.join(".codewhale").join("notes.txt")
    );
    assert_eq!(
        config.memory_path(),
        temp_root.join(".codewhale").join("memory.md")
    );
    assert_eq!(
        config.skills_dir(),
        temp_root.join(".codewhale").join("skills")
    );

    Ok(())
}

#[test]
fn default_user_paths_preserve_existing_legacy_files() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-legacy-home-test-{}-{}",
        std::process::id(),
        nanos
    ));
    let legacy_home = temp_root.join(".deepseek");
    fs::create_dir_all(&legacy_home)?;
    for name in ["config.toml", "mcp.json", "notes.txt", "memory.md"] {
        fs::write(legacy_home.join(name), "")?;
    }
    fs::create_dir_all(legacy_home.join("skills"))?;
    let _guard = EnvGuard::new(&temp_root);

    unsafe {
        env::remove_var("DEEPSEEK_CONFIG_PATH");
    }

    let config = Config::default();
    assert_eq!(
        default_config_path().unwrap(),
        legacy_home.join("config.toml")
    );
    assert_eq!(config.mcp_config_path(), legacy_home.join("mcp.json"));
    assert_eq!(config.notes_path(), legacy_home.join("notes.txt"));
    assert_eq!(config.memory_path(), legacy_home.join("memory.md"));
    assert_eq!(config.skills_dir(), legacy_home.join("skills"));

    Ok(())
}

#[test]
fn explicit_codewhale_home_isolates_all_config_owned_user_paths() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let ambient_home = temp_root.path().join("ambient-home");
    let explicit_home = temp_root.path().join("explicit-home");
    let ambient_legacy = ambient_home.join(".deepseek");
    fs::create_dir_all(ambient_legacy.join("skills"))?;
    for name in ["mcp.json", "notes.txt", "memory.md"] {
        fs::write(ambient_legacy.join(name), "legacy")?;
    }
    let _home = EnvVarGuard::set("HOME", &ambient_home);
    let _userprofile = EnvVarGuard::set("USERPROFILE", &ambient_home);
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &explicit_home);

    assert_eq!(default_skills_dir(), Some(explicit_home.join("skills")));
    assert_eq!(
        default_mcp_config_path(),
        Some(explicit_home.join("mcp.json"))
    );
    assert_eq!(default_notes_path(), Some(explicit_home.join("notes.txt")));
    assert_eq!(default_memory_path(), Some(explicit_home.join("memory.md")));
    Ok(())
}

#[test]
fn relative_mcp_config_path_falls_back_to_user_global_config() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let explicit_home = temp_root.path().join("codewhale-home");
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &explicit_home);
    let config = Config {
        mcp_config_path: Some("relative/mcp.json".to_string()),
        ..Config::default()
    };

    assert_eq!(config.mcp_config_path(), explicit_home.join("mcp.json"));
    Ok(())
}

#[test]
fn absolute_mcp_config_path_remains_an_explicit_override() -> Result<()> {
    let temp_root = tempfile::tempdir()?;
    let explicit = temp_root.path().join("custom-mcp.json");
    let config = Config {
        mcp_config_path: Some(explicit.display().to_string()),
        ..Config::default()
    };

    assert_eq!(config.mcp_config_path(), explicit);
    Ok(())
}

#[test]
fn whitespace_codewhale_home_keeps_ambient_legacy_config_path_fallbacks() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let ambient_home = temp_root.path().join("ambient-home");
    let ambient_legacy = ambient_home.join(".deepseek");
    fs::create_dir_all(ambient_legacy.join("skills"))?;
    for name in ["mcp.json", "notes.txt", "memory.md"] {
        fs::write(ambient_legacy.join(name), "legacy")?;
    }
    let _home = EnvVarGuard::set("HOME", &ambient_home);
    let _userprofile = EnvVarGuard::set("USERPROFILE", &ambient_home);
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", " \t ");

    assert_eq!(default_skills_dir(), Some(ambient_legacy.join("skills")));
    assert_eq!(
        default_mcp_config_path(),
        Some(ambient_legacy.join("mcp.json"))
    );
    assert_eq!(default_notes_path(), Some(ambient_legacy.join("notes.txt")));
    assert_eq!(
        default_memory_path(),
        Some(ambient_legacy.join("memory.md"))
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn non_unicode_codewhale_home_is_preserved_by_config_owned_user_paths() -> Result<()> {
    use std::os::unix::ffi::OsStringExt;

    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let explicit_home = temp_root
        .path()
        .join(OsString::from_vec(b"codewhale-\xff-home".to_vec()));
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &explicit_home);

    assert_eq!(default_skills_dir(), Some(explicit_home.join("skills")));
    assert_eq!(
        default_mcp_config_path(),
        Some(explicit_home.join("mcp.json"))
    );
    assert_eq!(default_notes_path(), Some(explicit_home.join("notes.txt")));
    assert_eq!(default_memory_path(), Some(explicit_home.join("memory.md")));
    Ok(())
}

#[test]
fn codewhale_config_path_env_wins_over_legacy_env() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-config-env-test-{}-{}",
        std::process::id(),
        nanos
    ));
    let preferred = temp_root.join("preferred.toml");
    let legacy = temp_root.join("legacy.toml");
    let _codewhale_config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", &preferred);
    let _legacy_config = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &legacy);

    assert_eq!(env_config_path().unwrap().unwrap(), preferred);

    Ok(())
}

#[test]
fn test_tilde_expansion_in_paths() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-tilde-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config = Config {
        skills_dir: Some("~/.deepseek/skills".to_string()),
        ..Default::default()
    };
    let expected_skills = temp_root.join(".deepseek").join("skills");
    let actual_skills = config.skills_dir();
    assert_eq!(
        actual_skills.components().collect::<Vec<_>>(),
        expected_skills.components().collect::<Vec<_>>()
    );

    Ok(())
}

#[test]
fn skills_scan_codewhale_only_defaults_false_and_parses_true() -> Result<()> {
    assert!(!Config::default().skills_config().scan_codewhale_only());

    let config: Config = toml::from_str(
        r#"
[skills]
scan_codewhale_only = true
listing_max_description_chars = 1024
listing_budget_fraction = 0.01
"#,
    )?;

    assert!(config.skills_config().scan_codewhale_only());
    assert_eq!(
        config.skills_config().listing_max_description_chars,
        Some(1024)
    );
    assert_eq!(config.skills_config().listing_budget_fraction, Some(0.01));
    config.validate()?;
    for value in [
        "listing_max_description_chars = 0",
        "listing_max_description_chars = 40001",
        "listing_budget_fraction = 0",
        "listing_budget_fraction = -0.1",
        "listing_budget_fraction = 1.1",
        "listing_budget_fraction = nan",
        "listing_budget_fraction = inf",
    ] {
        let invalid: Config = toml::from_str(&format!("[skills]\n{value}"))?;
        assert!(invalid.validate().is_err(), "accepted {value}");
    }

    for (mode, writes) in [("auto", false), ("all", true), ("none", false)] {
        let config: Config = toml::from_str(&format!(
            "skills_dir = '/discovery-only'\n[skills.bundled]\ninstall = '{mode}'\nselected = ['documents']\ndisabled = ['debug']\nexplicit_only = ['pdf']"
        ))?;
        assert_eq!(config.bundled_skills_install_dir().is_some(), writes);
        if writes {
            assert_eq!(config.bundled_skills_install_dir(), owned_skills_dir());
        }
        assert_eq!(
            config.skills_config().bundled.selected,
            Some(vec!["documents".into()])
        );
    }
    assert!(toml::from_str::<Config>("[skills.bundled]\ninstall = 'invalid'").is_err());
    Ok(())
}

#[test]
fn test_load_uses_tilde_expanded_deepseek_config_path() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-load-tilde-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".custom-deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(&config_path, "api_key = \"test-key\"\n")?;

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_CONFIG_PATH", "~/.custom-deepseek/config.toml");
    }

    let config = Config::load(None, None)?;
    assert_eq!(config.deepseek_table_api_key(), Some("test-key"));
    Ok(())
}

#[test]
fn missing_env_config_path_does_not_fall_back_to_a_different_home_file() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-load-fallback-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let home_config = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&home_config)?;
    fs::write(&home_config, "api_key = \"home-key\"\n")?;

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var(
            "DEEPSEEK_CONFIG_PATH",
            temp_root.join("missing-config.toml").as_os_str(),
        );
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config.deepseek_table_api_key(),
        None,
        "reads must honor the same missing env target that writes will create"
    );
    Ok(())
}

#[test]
fn save_then_load_uses_the_same_missing_absolute_env_config_path() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let config_path = temp_root.path().join("nested/config.toml");
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", &config_path);
    let _legacy_config_path = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
    let identity = ProviderIdentity {
        provider: ProviderKind::Openrouter,
        key: ProviderKind::Openrouter.as_str().into(),
        exact_id: Some(ProviderKind::Openrouter.as_str().into()),
        migrated_legacy_ollama_cloud_route: false,
        legacy_root_custom_generation: None,
    };

    let written =
        save_provider_model_for_identity(&identity, &Config::default(), "round-trip-model")?;
    assert_eq!(written, config_path);
    let loaded = Config::load(None, None)?;
    assert_eq!(
        loaded
            .provider_config_for(&loaded.test_identity_for_kind(ProviderKind::Openrouter))
            .and_then(|provider| provider.model.as_deref()),
        Some("round-trip-model")
    );
    Ok(())
}

#[test]
fn relative_config_env_is_a_load_error() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", ".codewhale/config.toml");
    let _legacy_config_path = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");

    let error = Config::load(None, None).expect_err("relative config path must fail closed");
    let message = format!("{error:#}");
    assert!(message.contains("CODEWHALE_CONFIG_PATH"), "{message}");
    assert!(message.contains("absolute"), "{message}");

    for error in [
        default_config_path().expect_err("default path must preserve the override error"),
        resolve_load_config_path(None)
            .expect_err("load-path helper must preserve the override error"),
        ensure_config_file_exists(None)
            .expect_err("first-run config creation must preserve the override error"),
    ] {
        let message = format!("{error:#}");
        assert!(message.contains("CODEWHALE_CONFIG_PATH"), "{message}");
        assert!(message.contains("absolute"), "{message}");
        assert!(!message.contains("home directory not found"), "{message}");
    }
    let error = env_config_path().expect_err("env helper must preserve the override error");
    let message = error.to_string();
    assert!(message.contains("CODEWHALE_CONFIG_PATH"), "{message}");
    assert!(workspace_trust_config_candidate_paths().is_empty());
    Ok(())
}

#[test]
fn test_nonexistent_profile_error() {
    let mut profiles = HashMap::new();
    profiles.insert("work".to_string(), Config::default());
    let config = ConfigFile {
        base: Box::default(),
        profiles: Some(profiles),
        legacy_root: Default::default(),
    };

    let err = apply_profile(config, Some("nonexistent")).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("Profile 'nonexistent' not found"));
    assert!(message.contains("Available profiles"));
    assert!(message.contains("work"));
}

/// #6362: `ConfigFile` keeps its base `Config` boxed. Parsing a document
/// through the profile path used to carry the multi-kilobyte struct by value
/// through the `toml::de` and `apply_profile` frames, which overflowed the
/// 2 MiB stack libtest gives every test thread in debug builds and aborted
/// the whole lib suite. Pin that budget explicitly: CI exports a larger
/// `RUST_MIN_STACK`, so without this thread the regression would be masked.
/// A regression here aborts the process with "has overflowed its stack",
/// which is the reported symptom, not a panic.
#[test]
fn profile_document_parses_within_the_default_test_thread_stack() {
    const DEFAULT_TEST_THREAD_STACK: usize = 2 * 1024 * 1024;
    let document = r#"
provider = "deepseek"
approval_policy = "on-request"

[tui]
theme = "underwater"

[profiles.work]
approval_policy = "never"
"#;
    let handle = std::thread::Builder::new()
        .name("config-default-test-stack".into())
        .stack_size(DEFAULT_TEST_THREAD_STACK)
        .spawn(move || {
            let config =
                Config::from_saved_document(document, Some("work")).expect("profile parses");
            assert_eq!(config.approval_policy.as_deref(), Some("never"));
            let base = Config::from_saved_document(document, None).expect("base parses");
            assert_eq!(base.approval_policy.as_deref(), Some("on-request"));
        })
        .expect("spawn a 2 MiB test thread");
    handle
        .join()
        .expect("config parsing must fit the default test thread stack");
}

#[test]
fn test_profile_with_no_profiles_section() {
    let config = ConfigFile {
        base: Box::default(),
        profiles: None,
        legacy_root: Default::default(),
    };

    let err = apply_profile(config, Some("missing")).unwrap_err();
    assert!(err.to_string().contains("Available profiles: none"));
}

#[test]
fn test_save_api_key_doesnt_match_similar_keys() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-api-key-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        "api_key_backup = \"old\"\napi_key = \"current\"\n",
    )?;
    let resolved_config_path = codewhale_config::resolve_config_path(None)?;

    let saved = save_api_key("new-key")?;
    assert_eq!(saved, SavedCredential::ConfigFile(resolved_config_path));

    let contents = fs::read_to_string(&config_path)?;
    assert!(contents.contains("api_key_backup = \"old\""));
    assert!(contents.contains("api_key = \""));
    Ok(())
}

#[test]
fn test_empty_top_level_api_key_counts_as_absent() {
    // An empty legacy top-level key is dropped when the file is parsed
    // (#6394); it never reaches a route as an empty credential.
    let config = Config {
        ..Default::default()
    }
    .with_legacy_root(Some("   ".to_string()), None);
    assert!(config.validate().is_ok());
    assert_eq!(config.deepseek_table_api_key(), None);
}

#[test]
fn test_missing_api_key_allowed() -> Result<()> {
    let config = Config::default();
    config.validate()?;
    Ok(())
}

#[test]
fn apply_env_overrides_ignores_empty_api_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-empty-key-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Simulate a fresh user who copied .env.example to .env without
    // filling in DEEPSEEK_API_KEY: dotenv loads it as the empty string.
    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_API_KEY", "");
    }

    let mut config = Config {
        ..Default::default()
    }
    .with_legacy_root(Some("from-config-file".to_string()), None);
    apply_env_overrides(&mut config, ConfigEnvironmentPolicy::Runtime);

    assert_eq!(config.deepseek_table_api_key(), Some("from-config-file"));
    config.validate()?;
    Ok(())
}

#[test]
fn apply_env_overrides_does_not_copy_api_key_into_config() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-env-key-not-config-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    unsafe {
        env::set_var("DEEPSEEK_API_KEY", "env-key");
    }
    let mut config = Config::default();
    apply_env_overrides(&mut config, ConfigEnvironmentPolicy::Runtime);

    assert_eq!(config.deepseek_table_api_key(), None);
    assert_eq!(config.active_route_api_key()?, "env-key");
    unsafe {
        env::remove_var("DEEPSEEK_API_KEY");
    }
    Ok(())
}

#[test]
fn normalize_model_name_preserves_v_series_snapshots() {
    // v4 canonical forms still resolve
    assert_eq!(
        normalize_model_name("deepseek-v4-pro").as_deref(),
        Some("deepseek-v4-pro")
    );
    assert_eq!(
        normalize_model_name("deepseek-v4pro").as_deref(),
        Some("deepseek-v4-pro")
    );
    assert_eq!(
        normalize_model_name("pro").as_deref(),
        Some("deepseek-v4-pro")
    );
    assert_eq!(
        normalize_model_name("flash").as_deref(),
        Some("deepseek-flash")
    );
    for alias in ["flash-vision", "deepseek-v4flashvisionexp"] {
        assert_eq!(canonical_model_name(alias), Some("deepseek-flash"));
        assert_eq!(
            normalize_model_name(alias).as_deref(),
            Some("deepseek-flash")
        );
        assert_eq!(
            normalize_model_name_for_provider(ProviderKind::Deepseek, alias).as_deref(),
            Some("deepseek-flash")
        );
        assert!(validate_route(ProviderKind::Deepseek, alias).is_ok());
    }
    // v-series dated snapshots pass through unchanged
    assert_eq!(
        normalize_model_name("deepseek-v4-flash-20260423").as_deref(),
        Some("deepseek-v4-flash-20260423")
    );
    // future v-series identities pass through
    assert_eq!(
        normalize_model_name("deepseek-v5-pro-20270101").as_deref(),
        Some("deepseek-v5-pro-20270101")
    );
    // legacy names pass through unchanged — server decides
    assert_eq!(
        normalize_model_name("deepseek-chat").as_deref(),
        Some("deepseek-chat")
    );
    // cross-provider names still normalize
    assert_eq!(
        normalize_model_name("deepseek-ai/deepseek-v4-pro").as_deref(),
        Some("deepseek-ai/deepseek-v4-pro")
    );
    // preserve exact case for providers that require case-sensitive model IDs
    assert_eq!(
        normalize_model_name("DeepSeek-V4-Pro").as_deref(),
        Some("DeepSeek-V4-Pro")
    );
    assert_eq!(
        normalize_model_name("deepseek-ai/DeepSeek-V4-Pro").as_deref(),
        Some("deepseek-ai/DeepSeek-V4-Pro")
    );
}

#[test]
fn normalize_model_scenario() {
    // Scenario consolidation of: normalize_model_for_provider_keeps_provider_remaps_when_case_is_preserved, normalize_model_name_for_provider_maps_moonshot_aliases, normalize_model_name_for_provider_maps_minimax_direct_aliases, normalize_model_name_for_provider_maps_arcee_direct_aliases, normalize_model_name_rejects_invalid_or_non_deepseek_ids, normalize_model_name_accepts_provider_prefixed_deepseek_ids
    // from normalize_model_for_provider_keeps_provider_remaps_when_case_is_preserved
    {
        assert_eq!(
            normalize_model_for_provider(ProviderKind::Deepseek, "DeepSeek-V4-Pro").as_deref(),
            Some("DeepSeek-V4-Pro")
        );
        assert_eq!(
            normalize_model_for_provider(ProviderKind::NvidiaNim, "DeepSeek-V4-Pro").as_deref(),
            Some(DEFAULT_NVIDIA_NIM_MODEL)
        );
    }
    // from normalize_model_name_for_provider_maps_moonshot_aliases
    {
        for (alias, expected) in [
            ("kimi", DEFAULT_MOONSHOT_MODEL),
            ("kimi-k2.7", DEFAULT_MOONSHOT_MODEL),
            ("kimi-k2.7-code", DEFAULT_MOONSHOT_MODEL),
            ("kimi-code", DEFAULT_MOONSHOT_MODEL),
            ("kimi-k2.6", MOONSHOT_KIMI_K2_6_MODEL),
        ] {
            assert_eq!(
                normalize_model_name_for_provider(ProviderKind::Moonshot, alias).as_deref(),
                Some(expected)
            );
        }
    }
    // from normalize_model_name_for_provider_maps_minimax_direct_aliases
    {
        for (alias, expected) in [
            ("minimax", DEFAULT_MINIMAX_MODEL),
            ("minimax-m3", DEFAULT_MINIMAX_MODEL),
            ("minimax-m2.7", MINIMAX_M2_7_MODEL),
            ("minimax-m2-7-highspeed", MINIMAX_M2_7_HIGHSPEED_MODEL),
            ("minimax-m2.5", MINIMAX_M2_5_MODEL),
            ("minimax-m2-5-highspeed", MINIMAX_M2_5_HIGHSPEED_MODEL),
            ("minimax-m2.1", MINIMAX_M2_1_MODEL),
            ("minimax-m2-1-highspeed", MINIMAX_M2_1_HIGHSPEED_MODEL),
            ("minimax-m2", MINIMAX_M2_MODEL),
        ] {
            assert_eq!(
                normalize_model_name_for_provider(ProviderKind::Minimax, alias).as_deref(),
                Some(expected)
            );
        }
    }
    // from normalize_model_name_for_provider_maps_arcee_direct_aliases
    {
        for (alias, expected) in [
            ("trinity", DEFAULT_ARCEE_MODEL),
            ("arcee-trinity", DEFAULT_ARCEE_MODEL),
            ("trinity-large-thinking", DEFAULT_ARCEE_MODEL),
            ("arcee-trinity-large-thinking", DEFAULT_ARCEE_MODEL),
            ("arcee-trinity-mini", ARCEE_TRINITY_MINI_MODEL),
            ("trinity-mini", ARCEE_TRINITY_MINI_MODEL),
            (
                "arcee-trinity-large-preview",
                ARCEE_TRINITY_LARGE_PREVIEW_MODEL,
            ),
            ("TRINITY_LARGE_PREVIEW", ARCEE_TRINITY_LARGE_PREVIEW_MODEL),
        ] {
            assert_eq!(
                normalize_model_name_for_provider(ProviderKind::Arcee, alias).as_deref(),
                Some(expected)
            );
        }
    }
    // from normalize_model_name_rejects_invalid_or_non_deepseek_ids
    {
        assert!(normalize_model_name("qwen3-coder").is_none());
        assert!(normalize_model_name("codewhale v4").is_none());
        assert!(normalize_model_name("").is_none());
    }
    // from normalize_model_name_accepts_provider_prefixed_deepseek_ids
    {
        assert_eq!(
            normalize_model_name("accounts/fireworks/models/deepseek-v4-flash").as_deref(),
            Some("accounts/fireworks/models/deepseek-v4-flash")
        );
        assert_eq!(
            normalize_model_name("provider/deepseek-ai/deepseek-v4-pro").as_deref(),
            Some("provider/deepseek-ai/deepseek-v4-pro")
        );
    }
}

#[test]
fn normalize_model_name_for_provider_canonicalizes_deepseek_api_variants() {
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::Deepseek, "deepseek-ai/DeepSeek-V4-Pro")
            .as_deref(),
        Some("deepseek-v4-pro")
    );
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::Deepseek, "deepseek/deepseek-v4-flash")
            .as_deref(),
        Some("deepseek-v4-flash")
    );

    for provider in [
        ProviderKind::Deepseek,
        ProviderKind::Deepseek,
        ProviderKind::DeepseekAnthropic,
    ] {
        for alias in ["deepseek-chat", "deepseek-reasoner"] {
            assert_eq!(
                canonical_model_id_for_provider(provider, alias).as_deref(),
                Some(DEEPSEEK_ALIAS_REPLACEMENT),
                "{provider:?} must retire {alias} before the wire boundary"
            );
            assert_eq!(
                normalize_model_name_for_provider(provider, alias).as_deref(),
                Some(DEEPSEEK_ALIAS_REPLACEMENT),
                "{provider:?} config normalization must retire {alias}"
            );
        }
    }
}

#[test]
fn migrated_deepseek_alias_receipt_is_runtime_only_and_defaults_empty() {
    assert!(Config::default().migrated_deepseek_model_alias.is_none());

    let config: Config = toml::from_str(
        r#"
default_text_model = "deepseek-v4-flash"
migrated_deepseek_model_alias = "deepseek-chat"
"#,
    )
    .expect("deserialize config");
    assert!(config.migrated_deepseek_model_alias.is_none());
}

#[test]
fn retired_deepseek_aliases_keep_mode_intent_unless_effort_is_explicit() {
    // Model normalization reads the process-global model override. Without the
    // shared lock, env-focused config tests can replace these fixture aliases.
    let _lock = lock_test_env();
    for (alias, expected_effort) in [("deepseek-chat", "off"), ("deepseek-reasoner", "high")] {
        for key in ["deepseek", "deepseek-cn", "deepseek-anthropic"] {
            let mut config = Config {
                provider: Some(key.to_string()),
                default_text_model: Some(alias.to_string()),
                ..Default::default()
            };
            normalize_model_config(&mut config);

            assert_eq!(
                config.default_text_model.as_deref(),
                Some(DEEPSEEK_ALIAS_REPLACEMENT)
            );
            assert_eq!(config.reasoning_effort.as_deref(), Some(expected_effort));
        }
    }

    let mut explicit = Config {
        provider: Some("deepseek".to_string()),
        default_text_model: Some("deepseek-chat".to_string()),
        reasoning_effort: Some("max".to_string()),
        ..Default::default()
    };
    normalize_model_config(&mut explicit);
    assert_eq!(
        explicit.default_text_model.as_deref(),
        Some(DEEPSEEK_ALIAS_REPLACEMENT)
    );
    assert_eq!(explicit.reasoning_effort.as_deref(), Some("max"));

    let mut provider_scoped = Config {
        provider: Some("deepseek-anthropic".to_string()),
        providers: Some(ProvidersConfig {
            deepseek_anthropic: ProviderConfig {
                model: Some("deepseek-reasoner".to_string()),
                ..Default::default()
            },
            ..Default::default()
        }),
        ..Default::default()
    };
    normalize_model_config(&mut provider_scoped);
    assert_eq!(
        provider_scoped
            .provider_config_for(
                &provider_scoped.test_identity_for_kind(ProviderKind::DeepseekAnthropic)
            )
            .and_then(|entry| entry.model.as_deref()),
        Some(DEEPSEEK_ALIAS_REPLACEMENT)
    );
    assert_eq!(provider_scoped.reasoning_effort.as_deref(), Some("high"));

    let mut custom_endpoint = Config {
        provider: Some("deepseek".to_string()),
        default_text_model: Some("deepseek-chat".to_string()),
        ..Default::default()
    }
    .with_legacy_root(None, Some("https://gateway.example/v1".to_string()));
    normalize_model_config(&mut custom_endpoint);
    assert_eq!(
        custom_endpoint.default_text_model.as_deref(),
        Some("deepseek-chat")
    );
    assert_eq!(custom_endpoint.reasoning_effort, None);
}

#[test]
fn retired_deepseek_aliases_do_not_escape_provider_owned_namespaces() {
    for provider in [
        ProviderKind::NvidiaNim,
        ProviderKind::Openrouter,
        ProviderKind::WanjieArk,
        ProviderKind::Custom,
    ] {
        for alias in ["deepseek-chat", "deepseek-reasoner"] {
            assert_eq!(
                canonical_model_id_for_provider(provider, alias).as_deref(),
                Some(alias),
                "{provider:?} owns the meaning of {alias}"
            );
        }
    }
}

#[test]
fn openrouter_hunyuan_aliases_resolve_to_hy3_preview() {
    for alias in [
        "tencent/hy3-preview",
        "hy3-preview",
        "hy3",
        "hunyuan",
        "tencent-hunyuan",
        "hunyuan-hy3",
    ] {
        assert_eq!(
            canonical_model_id_for_provider(ProviderKind::Openrouter, alias).as_deref(),
            Some(OPENROUTER_TENCENT_HY3_PREVIEW_MODEL),
            "{alias} should select the public Hy3 preview id"
        );
    }
    assert_ne!(
        canonical_model_id_for_provider(ProviderKind::Openrouter, "hy4").as_deref(),
        Some(OPENROUTER_TENCENT_HY3_PREVIEW_MODEL),
        "hy4 is not a released public model id"
    );
}

#[test]
fn deepseek_default_model_canonicalizes_provider_prefixed_ids() {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir().unwrap();
    let _guard = EnvGuard::new(temp_root.path());

    let config = Config {
        provider: Some("deepseek".to_string()),
        default_text_model: Some(DEFAULT_OPENROUTER_MODEL.to_string()),
        ..Default::default()
    };
    // The prefixed id canonicalizes to the deepseek-native PRO spelling; it is
    // not the default constant (that is deepseek-flash), it is that model.
    assert_eq!(config.default_model(), "deepseek-v4-pro");

    let config = Config {
        provider: Some("deepseek".to_string()),
        providers: Some(ProvidersConfig {
            deepseek: ProviderConfig {
                model: Some(DEFAULT_OPENROUTER_MODEL.to_string()),
                ..Default::default()
            },
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(config.default_model(), "deepseek-v4-pro");
}

#[test]
fn requested_model_for_provider_is_permissive_off_deepseek() {
    // #3018: the provider API is the authority for non-DeepSeek routes.
    assert_eq!(
        requested_model_for_provider(ProviderKind::Moonshot, "kimi-k2.5").as_deref(),
        Some("kimi-k2.5")
    );
    assert_eq!(
        requested_model_for_provider(ProviderKind::Ollama, "qwen3:32b").as_deref(),
        Some("qwen3:32b")
    );
    // The official DeepSeek API stays strict.
    assert!(requested_model_for_provider(ProviderKind::Deepseek, "kimi-k2.5").is_none());
    assert_eq!(
        requested_model_for_provider(ProviderKind::Deepseek, "deepseek-v4-pro").as_deref(),
        Some("deepseek-v4-pro")
    );
}

#[test]
fn validate_route_rejects_mismatched_provider_model_tuple() {
    // #3227: the exact contamination — Z.ai provider paired with a
    // DeepSeek model — is rejected locally with a diagnostic that names
    // the incompatible pair, before any network call.
    let err = validate_route(ProviderKind::Zai, "deepseek-v4-pro")
        .expect_err("zai + deepseek model must be rejected");
    assert!(err.contains("deepseek-v4-pro"), "names the model: {err}");
    assert!(err.contains("zai"), "names the provider: {err}");

    // A DeepSeek-native provider rejects a non-DeepSeek model id.
    let err = validate_route(ProviderKind::Deepseek, "GLM-5.2")
        .expect_err("deepseek + GLM must be rejected");
    assert!(err.contains("GLM-5.2"), "names the model: {err}");

    // Coherent routes pass.
    assert!(validate_route(ProviderKind::Zai, "GLM-5.2").is_ok());
    assert!(validate_route(ProviderKind::Deepseek, "deepseek-v4-pro").is_ok());
    // `auto` is always acceptable; the per-turn router resolves it.
    assert!(validate_route(ProviderKind::Zai, "auto").is_ok());
    // Pass-through / aggregator providers stay permissive — the upstream
    // API remains the authority for them.
    assert!(validate_route(ProviderKind::Openai, "deepseek-v4-pro").is_ok());
    assert!(validate_route(ProviderKind::Openai, "qwen-plus").is_ok());
    assert!(validate_route(ProviderKind::Openrouter, "deepseek-v4-pro").is_ok());
    assert!(validate_route(ProviderKind::NvidiaNim, "deepseek-v4-pro").is_ok());
    assert!(validate_route(ProviderKind::Together, DEFAULT_TOGETHER_MODEL).is_ok());
    assert!(validate_route(ProviderKind::Together, DEFAULT_TOGETHER_FLASH_MODEL).is_ok());
    assert!(validate_route(ProviderKind::Together, "deepseek-v4-pro").is_ok());

    // Sakana AI (Fugu) is a native provider — DeepSeek ids must not cross-wire.
    let err = validate_route(ProviderKind::Sakana, "deepseek-v4-flash")
        .expect_err("sakana + deepseek flash must be rejected");
    assert!(err.contains("deepseek-v4-flash"), "names the model: {err}");
    assert!(err.contains("sakana"), "names the provider: {err}");
    assert!(validate_route(ProviderKind::Sakana, DEFAULT_SAKANA_MODEL).is_ok());
}

#[test]
fn wire_model_for_provider_matches_active_provider_shape() {
    assert_eq!(
        wire_model_for_provider(ProviderKind::Deepseek, DEFAULT_OPENROUTER_MODEL),
        "deepseek-v4-pro"
    );
    assert_eq!(
        wire_model_for_provider(ProviderKind::Openrouter, "deepseek-v4-pro"),
        DEFAULT_OPENROUTER_MODEL
    );
    assert_eq!(
        wire_model_for_provider(ProviderKind::NvidiaNim, "deepseek-v4-pro"),
        DEFAULT_NVIDIA_NIM_MODEL
    );
    assert_eq!(
        wire_model_for_provider(ProviderKind::Together, "deepseek-v4-pro"),
        DEFAULT_TOGETHER_MODEL
    );
    assert_eq!(
        wire_model_for_provider(ProviderKind::Together, "deepseek-v4-flash"),
        DEFAULT_TOGETHER_FLASH_MODEL
    );
    assert_eq!(
        wire_model_for_provider(ProviderKind::Together, "thinkingmachines/inkling"),
        TOGETHER_INKLING_MODEL
    );
    assert_eq!(
        wire_model_for_provider(ProviderKind::Together, "inkling"),
        TOGETHER_INKLING_MODEL
    );
    assert_eq!(
        wire_model_for_provider(ProviderKind::Together, "together-inkling"),
        TOGETHER_INKLING_MODEL
    );
    assert_eq!(
        wire_model_for_provider(ProviderKind::Openai, DEFAULT_OPENROUTER_MODEL),
        DEFAULT_OPENROUTER_MODEL
    );
    assert_eq!(
        wire_model_for_provider(ProviderKind::Openrouter, OPENROUTER_MINIMAX_M3_MODEL),
        OPENROUTER_MINIMAX_M3_MODEL
    );
    assert_eq!(
        wire_model_for_provider(ProviderKind::SiliconflowCN, DEFAULT_SILICONFLOW_MODEL),
        DEFAULT_SILICONFLOW_MODEL
    );
    assert_eq!(
        wire_model_for_provider(ProviderKind::SiliconflowCN, "deepseek-v4-pro"),
        DEFAULT_SILICONFLOW_MODEL
    );
}

#[test]
fn wire_model_route_retires_aliases_only_on_official_deepseek_endpoints() {
    for (provider, base_url) in [
        (ProviderKind::Deepseek, "https://api.deepseek.com"),
        (ProviderKind::Deepseek, "https://api.deepseek.com/v1"),
        (ProviderKind::Deepseek, "https://api.deepseek.com/beta"),
        (
            ProviderKind::DeepseekAnthropic,
            "https://api.deepseek.com/anthropic",
        ),
        (
            ProviderKind::DeepseekAnthropic,
            "https://api.deepseek.com/anthropic/v1/",
        ),
    ] {
        for alias in ["deepseek-chat", "deepseek-reasoner"] {
            assert_eq!(
                wire_model_for_provider_route(provider, base_url, alias),
                DEEPSEEK_ALIAS_REPLACEMENT,
                "{provider:?} {base_url} must not send {alias}"
            );
        }
    }

    for (provider, base_url, alias) in [
        (
            ProviderKind::Deepseek,
            "https://gateway.example/v1",
            "deepseek-chat",
        ),
        (
            ProviderKind::DeepseekAnthropic,
            "https://messages.example/v1",
            "deepseek-reasoner",
        ),
        (
            ProviderKind::WanjieArk,
            DEFAULT_WANJIE_ARK_BASE_URL,
            "deepseek-reasoner",
        ),
        (
            ProviderKind::NvidiaNim,
            DEFAULT_NVIDIA_NIM_BASE_URL,
            "deepseek-reasoner",
        ),
    ] {
        assert_eq!(
            wire_model_for_provider_route(provider, base_url, alias),
            alias,
            "{provider:?} owns the meaning of {alias}"
        );
    }
}

#[test]
fn normalize_model_name_for_provider_keeps_provider_specific_ids() {
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::NvidiaNim, "deepseek-v4-pro").as_deref(),
        Some(DEFAULT_NVIDIA_NIM_MODEL)
    );
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::Openrouter, "deepseek-v4-flash").as_deref(),
        Some(DEFAULT_OPENROUTER_FLASH_MODEL)
    );
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::Siliconflow, "deepseek-v4-pro").as_deref(),
        Some(DEFAULT_SILICONFLOW_MODEL)
    );
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::Siliconflow, "deepseek-reasoner")
            .as_deref(),
        Some(DEFAULT_SILICONFLOW_MODEL)
    );
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::Siliconflow, "deepseek-r1").as_deref(),
        Some(DEFAULT_SILICONFLOW_MODEL)
    );
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::SiliconflowCN, "deepseek-reasoner")
            .as_deref(),
        Some(DEFAULT_SILICONFLOW_MODEL)
    );
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::Siliconflow, "deepseek-chat").as_deref(),
        Some(DEFAULT_SILICONFLOW_FLASH_MODEL)
    );
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::SiliconflowCN, "deepseek-chat").as_deref(),
        Some(DEFAULT_SILICONFLOW_FLASH_MODEL)
    );
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::Siliconflow, "deepseek-v3").as_deref(),
        Some(DEFAULT_SILICONFLOW_FLASH_MODEL)
    );
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::Siliconflow, "deepseek-v3.2").as_deref(),
        Some("deepseek-v3.2")
    );
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::Together, "deepseek-v4-pro").as_deref(),
        Some(DEFAULT_TOGETHER_MODEL)
    );
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::Together, "deepseek-chat").as_deref(),
        Some(DEFAULT_TOGETHER_FLASH_MODEL)
    );
}

#[test]
fn normalize_model_name_for_provider_maps_recent_openrouter_aliases() {
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
    ] {
        assert_eq!(
            normalize_model_name_for_provider(ProviderKind::Openrouter, alias).as_deref(),
            Some(expected)
        );
    }
}

#[test]
fn normalize_xiaomi_mimo_aliases_for_provider() {
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::XiaomiMimo, "omni").as_deref(),
        Some("mimo-v2.6-flash")
    );
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::XiaomiMimo, "tts").as_deref(),
        Some("mimo-v2.5-tts")
    );
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::XiaomiMimo, "voice-design").as_deref(),
        Some("mimo-v2.5-tts-voicedesign")
    );
    assert_eq!(
        wire_model_for_provider(ProviderKind::XiaomiMimo, "voiceclone"),
        "mimo-v2.5-tts-voiceclone"
    );
}

#[test]
fn model_completion_scenario() {
    // Scenario consolidation of: model_completion_names_for_xiaomi_mimo_include_chat_models, model_completion_names_for_deepseek_api_are_deduplicated_bare_ids, model_completion_names_for_openai_are_native_to_its_default_endpoint, model_completion_names_for_atlascloud_keep_its_provider_owned_default, model_completion_names_for_together_include_provider_owned_models, model_completion_names_for_wanjie_keep_legacy_default_and_v4_ids, model_completion_names_for_ollama_do_not_promote_static_remote_models, model_completion_names_for_openrouter_include_recent_large_models
    // from model_completion_names_for_xiaomi_mimo_include_chat_models
    {
        let models = model_completion_names_for_provider(ProviderKind::XiaomiMimo);
        for expected in ["mimo-v2.6-pro", "mimo-v2.6-flash"] {
            assert!(models.contains(&expected), "missing {expected}");
        }
        for deprecated in ["mimo-v2-pro", "mimo-v2-omni", "mimo-v2-flash"] {
            assert!(
                !models.contains(&deprecated),
                "{deprecated} is deprecated and should not be promoted"
            );
        }
        for speech_model in [
            "mimo-v2.5-tts",
            "mimo-v2.5-tts-voicedesign",
            "mimo-v2.5-tts-voiceclone",
            "mimo-v2-tts",
        ] {
            assert!(
                !models.contains(&speech_model),
                "{speech_model} belongs in speech/TTS selection, not /model"
            );
        }
    }
    // from model_completion_names_for_deepseek_api_are_deduplicated_bare_ids
    {
        assert_eq!(
            model_completion_names_for_provider(ProviderKind::Deepseek),
            vec![
                "deepseek-v4-pro",
                "deepseek-flash",
                "deepseek-v4-flash",
                "deepseek-v4-flash-vision-exp"
            ]
        );
    }
    // from model_completion_names_for_openai_are_native_to_its_default_endpoint
    {
        let models = model_completion_names_for_provider(ProviderKind::Openai);

        assert_eq!(models.first().copied(), Some("gpt-5.6"));
        assert!(models.iter().all(|model| !model.contains("deepseek")));
    }
    // from model_completion_names_for_atlascloud_keep_its_provider_owned_default
    {
        assert_eq!(
            model_completion_names_for_provider(ProviderKind::Atlascloud),
            vec![DEFAULT_ATLASCLOUD_MODEL]
        );
    }
    // from model_completion_names_for_together_include_provider_owned_models
    {
        assert_eq!(
            model_completion_names_for_provider(ProviderKind::Together),
            vec![DEFAULT_TOGETHER_MODEL, DEFAULT_TOGETHER_FLASH_MODEL]
        );
    }
    // from model_completion_names_for_wanjie_keep_legacy_default_and_v4_ids
    {
        let models = model_completion_names_for_provider(ProviderKind::WanjieArk);

        assert_eq!(models.first().copied(), Some(DEFAULT_WANJIE_ARK_MODEL));
        assert!(models.contains(&"deepseek-v4-pro"));
        assert!(models.contains(&"deepseek-v4-flash"));
    }
    // from model_completion_names_for_ollama_do_not_promote_static_remote_models
    {
        let models = model_completion_names_for_provider(ProviderKind::Ollama);

        assert!(models.is_empty());
    }
    // from model_completion_names_for_openrouter_include_recent_large_models
    {
        let models = model_completion_names_for_provider(ProviderKind::Openrouter);

        for expected in [
            DEFAULT_OPENROUTER_MODEL,
            DEFAULT_OPENROUTER_FLASH_MODEL,
            OPENROUTER_ARCEE_TRINITY_LARGE_THINKING_MODEL,
            OPENROUTER_XIAOMI_MIMO_V2_5_PRO_MODEL,
            OPENROUTER_MINIMAX_M3_MODEL,
            OPENROUTER_MINIMAX_M2_7_MODEL,
            OPENROUTER_QWEN_3_6_FLASH_MODEL,
            OPENROUTER_QWEN_3_6_35B_A3B_MODEL,
            OPENROUTER_QWEN_3_6_MAX_PREVIEW_MODEL,
            OPENROUTER_QWEN_3_6_27B_MODEL,
            OPENROUTER_QWEN_3_6_PLUS_MODEL,
            OPENROUTER_GLM_5_1_MODEL,
            OPENROUTER_GLM_5_2_MODEL,
            OPENROUTER_GEMMA_4_31B_MODEL,
        ] {
            assert!(models.contains(&expected), "missing {expected}");
        }
    }
}

#[test]
fn model_completion_scenario_2() {
    // Scenario consolidation of: model_completion_names_for_moonshot_uses_latest_platform_model, model_completion_names_for_zai_lists_default_5_1_and_turbo, model_completion_names_for_minimax_include_direct_chat_models, model_completion_names_for_minimax_anthropic_include_target_models, model_completion_names_for_sakana_include_fugu_models
    // from model_completion_names_for_moonshot_uses_latest_platform_model
    {
        let models = model_completion_names_for_provider(ProviderKind::Moonshot);

        assert_eq!(models.first().copied(), Some(DEFAULT_MOONSHOT_MODEL));
        // `kimi-k3` is served by this provider's default (direct platform) route
        // and must be offerable — a dogfood user on v0.9.1 could not find it.
        assert!(models.contains(&MOONSHOT_KIMI_K3_MODEL), "{models:?}");
        // The Kimi Code coding-plan ids belong to api.kimi.com/coding/v1, which
        // this base-URL-less list cannot express. Offering them here would
        // advertise a pairing `validate_kimi_code_api_model_id` rejects.
        assert!(!models.contains(&KIMI_CODE_K3_MODEL), "{models:?}");
        assert!(!models.contains(&DEFAULT_KIMI_CODE_MODEL), "{models:?}");
        for model in &models {
            let config = Config {
                provider: Some(ProviderKind::Moonshot.as_str().to_string()),
                default_text_model: Some((*model).to_string()),
                ..Default::default()
            };
            config
                .validate()
                .expect("every advertised Moonshot model must be valid on its default route");
        }
    }
    // from model_completion_names_for_zai_lists_default_5_1_and_turbo
    {
        let models = model_completion_names_for_provider(ProviderKind::Zai);

        // GLM-5.3 is the default and must be first; GLM-5.2 and GLM-5.1 stay
        // available, and GLM-5-Turbo is the faster sub-agent sibling.
        assert_eq!(models.first().copied(), Some(DEFAULT_ZAI_MODEL));
        assert_eq!(DEFAULT_ZAI_MODEL, ZAI_GLM_5_3_MODEL);
        assert!(models.contains(&ZAI_GLM_5_1_MODEL));
        assert!(models.contains(&ZAI_GLM_5_3_FLASH_MODEL));
        assert!(models.contains(&ZAI_GLM_5_TURBO_MODEL));
        // GLM-5.2 is still offered alongside the others but no longer takes the
        // default slot; explicit 5.2 routes are untouched.
        assert!(models.contains(&ZAI_GLM_5_2_MODEL));
        assert_ne!(models.first().copied(), Some(ZAI_GLM_5_2_MODEL));
        // No accidental duplicate entries.
        let mut sorted = models.to_vec();
        sorted.sort_unstable();
        let mut deduped = sorted.clone();
        deduped.dedup();
        assert_eq!(sorted, deduped);
    }
    // from model_completion_names_for_minimax_include_direct_chat_models
    {
        let models = model_completion_names_for_provider(ProviderKind::Minimax);

        for expected in [
            DEFAULT_MINIMAX_MODEL,
            MINIMAX_M2_1_MODEL,
            MINIMAX_M2_1_HIGHSPEED_MODEL,
            MINIMAX_M2_MODEL,
        ] {
            assert!(models.contains(&expected), "missing {expected}");
        }
        assert!(
            !models.contains(&OPENROUTER_MINIMAX_M3_MODEL),
            "direct MiniMax picker must not expose OpenRouter namespaced IDs"
        );
    }
    // from model_completion_names_for_minimax_anthropic_include_target_models
    {
        let models = model_completion_names_for_provider(ProviderKind::MinimaxAnthropic);

        assert!(models.contains(&DEFAULT_MINIMAX_MODEL));
        assert!(models.contains(&MINIMAX_M2_1_MODEL));
    }
    // from model_completion_names_for_sakana_include_fugu_models
    {
        assert_eq!(
            model_completion_names_for_provider(ProviderKind::Sakana),
            vec![DEFAULT_SAKANA_MODEL, SAKANA_FUGU_ULTRA_MODEL]
        );
    }
}

#[test]
fn normalize_model_name_for_zai_canonicalizes_current_glm_models() {
    for (alias, expected) in [
        ("glm-5.1", ZAI_GLM_5_1_MODEL),
        ("glm-5-1", ZAI_GLM_5_1_MODEL),
        ("glm-5.2", ZAI_GLM_5_2_MODEL),
        ("zai-glm-5-2", ZAI_GLM_5_2_MODEL),
        ("glm-5.3", DEFAULT_ZAI_MODEL),
        ("glm-5-3", ZAI_GLM_5_3_MODEL),
        ("zai-glm-5-3", ZAI_GLM_5_3_MODEL),
        ("glm-5.3-flash", ZAI_GLM_5_3_FLASH_MODEL),
        ("glm-5-3-flash", ZAI_GLM_5_3_FLASH_MODEL),
        ("zai-glm-5.3-flash", ZAI_GLM_5_3_FLASH_MODEL),
        ("glm-5-turbo", ZAI_GLM_5_TURBO_MODEL),
        ("zai-glm-5-turbo", ZAI_GLM_5_TURBO_MODEL),
    ] {
        assert_eq!(
            normalize_model_name_for_provider(ProviderKind::Zai, alias).as_deref(),
            Some(expected)
        );
    }
    // The 5.1-era bug shape: an alias silently resolving to the provider
    // default. Now that GLM-5.3 is the default, GLM-5.2 must keep its own id.
    assert_ne!(ZAI_GLM_5_2_MODEL, DEFAULT_ZAI_MODEL);
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::Zai, "glm-5.2").as_deref(),
        Some(ZAI_GLM_5_2_MODEL)
    );
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::Zai, "glm-next-preview").as_deref(),
        Some("glm-next-preview")
    );
}

#[test]
fn opencode_go_config_uses_documented_model_protocols() -> Result<()> {
    let _lock = lock_test_env();
    let _api_key = EnvVarGuard::remove("OPENCODE_GO_API_KEY");
    let _base_url = EnvVarGuard::remove("OPENCODE_GO_BASE_URL");
    let _model = EnvVarGuard::remove("OPENCODE_GO_MODEL");

    let config: Config = toml::from_str(
        r#"
provider = "opencode_go"

[providers.opencode_go]
api_key = "go-config-key"
model = "opencode-go/glm-5.2"
"#,
    )?;

    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::OpencodeGo
    );
    assert_eq!(config.active_route_base_url(), DEFAULT_OPENCODE_GO_BASE_URL);
    assert_eq!(config.default_model(), "glm-5.2");
    assert_eq!(config.active_route_api_key()?, "go-config-key");
    assert_eq!(
        wire_model_for_provider(ProviderKind::OpencodeGo, "opencode-go/mimo-v2.5-pro"),
        "mimo-v2.5-pro"
    );
    assert_eq!(
        model_completion_names_for_provider(ProviderKind::OpencodeGo),
        opencode_go_models()
    );
    for chat_model in opencode_go_models() {
        assert_eq!(
            canonical_model_id_for_provider(ProviderKind::OpencodeGo, chat_model).as_deref(),
            Some(chat_model)
        );
        assert!(validate_route(ProviderKind::OpencodeGo, chat_model).is_ok());
    }
    for messages_only in ["claude-unproven", "gpt-unlisted"] {
        assert!(
            !model_completion_names_for_provider(ProviderKind::OpencodeGo).contains(&messages_only),
            "{messages_only} has no documented Go protocol and must not be advertised"
        );
        assert!(
            canonical_model_id_for_provider(ProviderKind::OpencodeGo, messages_only).is_none(),
            "{messages_only} must not pass the explicit selector gate"
        );
        assert!(
            requested_model_for_provider(ProviderKind::OpencodeGo, messages_only).is_none(),
            "{messages_only} must not pass the runtime request gate"
        );
        assert!(validate_route(ProviderKind::OpencodeGo, messages_only).is_err());
        // Never substitute a different model. Keep the caller's spelling so
        // validate_route / the route resolver can reject by name. A base URL
        // override still cannot grant an unknown ID a protocol.
        assert_eq!(
            wire_model_for_provider(ProviderKind::OpencodeGo, messages_only),
            messages_only,
            "must not silently rewrite {messages_only} to the Chat default"
        );
        assert_eq!(
            wire_model_for_provider_route(
                ProviderKind::OpencodeGo,
                "https://go-gateway.example/v1",
                messages_only,
            ),
            messages_only,
            "a base URL override must not rewrite or re-admit {messages_only}"
        );
    }

    Ok(())
}

/// #6516: the removed seam-manager keys and `tui.terminal_probe_timeout_ms`
/// are no longer fields. A config that still carries them must keep loading,
/// with the live `[context] project_pack` key intact.
#[test]
fn removed_context_seam_and_terminal_probe_keys_still_load() -> Result<()> {
    let parsed: ConfigFile = toml::from_str(
        r#"
        [tui]
        terminal_probe_timeout_ms = 500

        [context]
        enabled = true
        project_pack = true
        verbatim_window_turns = 4
        l1_threshold = 1
        l2_threshold = 2
        l3_threshold = 3
        seam_model = "deepseek-v4-flash"
        "#,
    )?;

    assert_eq!(parsed.base.context.project_pack, Some(true));
    // #6516: the retired keys still load but are no longer typed fields, so
    // nothing can read or merge them again. The derived Debug lists every
    // field, which makes a reintroduced field visible here.
    let context = format!("{:?}", parsed.base.context);
    for retired in [
        "enabled",
        "verbatim_window_turns",
        "l1_threshold",
        "l2_threshold",
        "l3_threshold",
        "seam_model",
    ] {
        assert!(!context.contains(retired), "{retired} in {context}");
    }
    let tui = format!("{:?}", parsed.base.tui.expect("[tui] table loads"));
    assert!(!tui.contains("terminal_probe_timeout_ms"), "{tui}");
    Ok(())
}

#[test]
fn profile_skills_config_merges_individual_fields() {
    let mut profiles = HashMap::new();
    profiles.insert(
        "strict".to_string(),
        Config {
            skills: Some(SkillsConfig {
                scan_codewhale_only: Some(true),
                listing_max_description_chars: Some(80),
                flat_workspace_root: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    let config = ConfigFile {
        base: Box::new(Config {
            skills: Some(SkillsConfig {
                registry_url: Some("https://registry.example/skills.json".to_string()),
                max_install_size_bytes: Some(1234),
                listing_max_description_chars: Some(1024),
                listing_budget_fraction: Some(0.01),
                flat_workspace_root: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }),
        profiles: Some(profiles),
        legacy_root: Default::default(),
    };

    let merged = apply_profile(config, Some("strict")).expect("profile");
    let skills = merged.skills.expect("merged skills config");
    assert_eq!(
        skills.registry_url.as_deref(),
        Some("https://registry.example/skills.json")
    );
    assert_eq!(skills.max_install_size_bytes, Some(1234));
    assert_eq!(skills.scan_codewhale_only, Some(true));
    assert_eq!(skills.flat_workspace_root, Some(false));
    assert_eq!(skills.listing_max_description_chars, Some(80));
    assert_eq!(skills.listing_budget_fraction, Some(0.01));
    let base: Config = toml::from_str("[skills.bundled]\ninstall='all'\nselected=['debug']\ndisabled=['pdf']\nexplicit_only=['debug']").unwrap();
    let override_cfg: Config =
        toml::from_str("[skills.bundled]\nselected=[]\nexplicit_only=[]").unwrap();
    let merged = merge_skills_config(base.skills, override_cfg.skills)
        .unwrap()
        .bundled;
    assert_eq!(merged.install, Some(BundledSkillsInstall::All));
    assert_eq!(merged.selected, Some(vec![]));
    assert_eq!(merged.explicit_only, Some(vec![]));
    assert_eq!(merged.disabled, Some(vec!["pdf".into()]));
}

#[test]
fn removed_context_per_model_table_is_ignored_for_compatibility() -> Result<()> {
    let parsed: ConfigFile = toml::from_str(
        r#"
        [context]
        project_pack = true

        [context.per_model.deepseek-v4-pro]
        l1_threshold = 111
        l2_threshold = 222
        l3_threshold = 333
        "#,
    )?;

    assert_eq!(parsed.base.context.project_pack, Some(true));
    Ok(())
}

#[test]
fn project_context_pack_defaults_off_and_can_be_enabled() {
    // #4781: project context pack is opt-in (large pretty-printed tree).
    let mut config = Config::default();
    assert!(!config.project_context_pack_enabled());

    config.context.project_pack = Some(true);
    assert!(config.project_context_pack_enabled());

    config.context.project_pack = Some(false);
    assert!(!config.project_context_pack_enabled());
}

#[test]
fn validate_accepts_scenario() -> Result<()> {
    // Scenario consolidation of: validate_accepts_future_deepseek_model_id, validate_accepts_auto_default_text_model
    // from validate_accepts_future_deepseek_model_id
    {
        let config = Config {
            default_text_model: Some("deepseek-v4".to_string()),
            ..Default::default()
        };
        config.validate()?;
    }
    // from validate_accepts_auto_default_text_model
    {
        let config = Config {
            default_text_model: Some("auto".to_string()),
            ..Default::default()
        };
        config.validate()?;
        assert_eq!(config.default_model(), "auto");
    }
    Ok(())
}

#[test]
fn deepseek_provider_defaults_to_beta_endpoint() {
    let config = Config::default();

    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Deepseek
    );
    assert_eq!(config.active_route_base_url(), DEFAULT_DEEPSEEK_BASE_URL);
}

#[test]
fn explicit_deepseek_base_url_overrides_beta_default() {
    let config = Config {
        ..Default::default()
    }
    .with_legacy_root(None, Some("https://api.deepseek.com".to_string()));

    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Deepseek
    );
    assert_eq!(config.active_route_base_url(), "https://api.deepseek.com");
}

#[test]
fn deepseek_model_env_overrides_default_text_model() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-model-env-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_MODEL", "deepseek-v4-flash-20260423");
    }

    let config = Config::load(None, None)?;
    // v-series snapshots pass through unchanged — no alias folding
    assert_eq!(
        config.default_text_model.as_deref(),
        Some("deepseek-v4-flash-20260423")
    );
    Ok(())
}

#[test]
fn retired_deepseek_aliases_from_env_are_migrated_before_runtime() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _managed_config = crate::test_support::EnvVarGuard::set(
        "DEEPSEEK_MANAGED_CONFIG_PATH",
        temp_root.path().join("missing-managed.toml"),
    );

    for (provider, alias, expected_effort) in [
        ("deepseek", "deepseek-chat", "off"),
        ("deepseek-cn", "deepseek-reasoner", "high"),
        ("deepseek-anthropic", "deepseek-chat", "off"),
    ] {
        let _guard = EnvGuard::new(temp_root.path());
        // Safety: test-only environment mutation guarded by a global mutex.
        unsafe {
            env::set_var("CODEWHALE_PROVIDER", provider);
            env::set_var("CODEWHALE_MODEL", alias);
        }

        // Pass the isolated path explicitly: the process-wide default config
        // path is cached by earlier tests and can otherwise point back at the
        // developer's real provider-scoped model.
        let config = Config::load(
            Some(temp_root.path().join("isolated-alias-config.toml")),
            None,
        )?;
        assert_eq!(
            config.default_model(),
            DEEPSEEK_ALIAS_REPLACEMENT,
            "provider={provider} resolved={:?} root_model={:?} scoped_model={:?}",
            config.active_provider_identity().unwrap().provider,
            config.default_text_model,
            config
                .provider_config_for(&config.active_provider_identity().unwrap())
                .and_then(|entry| entry.model.as_deref())
        );
        assert_eq!(config.reasoning_effort(), Some(expected_effort));
        let deprecation = config
            .active_deepseek_alias_deprecation()
            .expect("loaded config should retain the alias migration receipt");
        assert_eq!(deprecation.alias, alias);
        assert_eq!(deprecation.replacement, DEEPSEEK_ALIAS_REPLACEMENT);
    }

    Ok(())
}

#[test]
fn http_headers_load_from_root_config() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-http-headers-root-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"
api_key = "test-key"
http_headers = { "X-Model-Provider-Id" = "tongyi" }
"#,
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config
            .http_headers()
            .get("X-Model-Provider-Id")
            .map(String::as_str),
        Some("tongyi")
    );
    Ok(())
}

#[test]
fn provider_http_headers_extend_and_override_root_config() {
    let mut providers = ProvidersConfig::default();
    providers.deepseek.http_headers = Some(HashMap::from([
        ("X-Model-Provider-Id".to_string(), "tongyi".to_string()),
        ("X-Shared".to_string(), "provider".to_string()),
    ]));
    let config = Config {
        http_headers: Some(HashMap::from([
            ("X-Root".to_string(), "root".to_string()),
            ("X-Shared".to_string(), "root".to_string()),
        ])),
        providers: Some(providers),
        ..Default::default()
    };

    let headers = config.http_headers();
    assert_eq!(
        headers.get("X-Model-Provider-Id").map(String::as_str),
        Some("tongyi")
    );
    assert_eq!(headers.get("X-Root").map(String::as_str), Some("root"));
    assert_eq!(
        headers.get("X-Shared").map(String::as_str),
        Some("provider")
    );
}

#[test]
fn http_headers_env_overrides_config() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-http-headers-env-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"
api_key = "test-key"
http_headers = { "X-Model-Provider-Id" = "from-file" }
"#,
    )?;
    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_HTTP_HEADERS", "X-Model-Provider-Id=from-env");
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config
            .http_headers()
            .get("X-Model-Provider-Id")
            .map(String::as_str),
        Some("from-env")
    );
    Ok(())
}

#[test]
fn nvidia_nim_scenario() -> Result<()> {
    // Scenario consolidation of: nvidia_nim_provider_uses_nim_defaults, nvidia_nim_provider_normalizes_deepseek_v4_flash_alias, nvidia_nim_env_accepts_short_nim_base_url_alias
    // from nvidia_nim_provider_uses_nim_defaults
    {
        let config = Config {
            provider: Some("nvidia-nim".to_string()),
            ..Default::default()
        };

        config.validate()?;
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::NvidiaNim
        );
        assert_eq!(config.default_model(), DEFAULT_NVIDIA_NIM_MODEL);
        assert_eq!(config.active_route_base_url(), DEFAULT_NVIDIA_NIM_BASE_URL);
    }
    // from nvidia_nim_provider_normalizes_deepseek_v4_flash_alias
    {
        let _lock = lock_test_env();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_root = env::temp_dir().join(format!(
            "codewhale-tui-nim-flash-model-alias-test-{}-{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&temp_root)?;
        let _guard = EnvGuard::new(&temp_root);

        let config = Config {
            provider: Some("nvidia-nim".to_string()),
            default_text_model: Some("deepseek-v4-flash".to_string()),
            ..Default::default()
        };

        config.validate()?;
        assert_eq!(config.default_model(), DEFAULT_NVIDIA_NIM_FLASH_MODEL);
    }
    // from nvidia_nim_env_accepts_short_nim_base_url_alias
    {
        let _lock = lock_test_env();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_root = env::temp_dir().join(format!(
            "codewhale-tui-nim-base-url-alias-test-{}-{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&temp_root)?;
        let _guard = EnvGuard::new(&temp_root);

        // Safety: test-only environment mutation guarded by a global mutex.
        unsafe {
            env::set_var("DEEPSEEK_PROVIDER", "nvidia-nim");
            env::set_var("NIM_BASE_URL", "https://short-nim.example/v1");
        }

        let config = Config::load(None, None)?;
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::NvidiaNim
        );
        assert_eq!(
            config.active_route_base_url(),
            "https://short-nim.example/v1"
        );
    }
    Ok(())
}

#[test]
fn nvidia_nim_provider_normalizes_deepseek_v4_pro_alias() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-nim-model-alias-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        "provider = \"nvidia-nim\"\ndefault_text_model = \"deepseek-v4-pro\"\napi_key = \"nim-key\"\n",
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::NvidiaNim
    );
    assert_eq!(
        config.default_text_model.as_deref(),
        Some(DEFAULT_NVIDIA_NIM_MODEL)
    );
    Ok(())
}

#[test]
fn vendor_locked_providers_reject_foreign_root_default_model() {
    let _lock = lock_test_env();
    for (provider, expected) in [
        ("xai", DEFAULT_XAI_MODEL),
        ("openai", DEFAULT_OPENAI_MODEL),
        ("moonshot", DEFAULT_MOONSHOT_MODEL),
        ("mistral", DEFAULT_MISTRAL_MODEL),
    ] {
        let config = Config {
            provider: Some(provider.to_string()),
            default_text_model: Some("deepseek-v4-pro".to_string()),
            ..Default::default()
        };
        assert_eq!(
            config.default_model(),
            expected,
            "a root DeepSeek default must not leak onto the official {provider} endpoint"
        );
    }
}

#[test]
fn mistral_model_env_overrides_vendor_default() {
    let _lock = lock_test_env();
    let _model = EnvVarGuard::set("MISTRAL_MODEL", "mistral-medium-latest");
    let _generic_model = EnvVarGuard::remove("CODEWHALE_MODEL");
    let _legacy_model = EnvVarGuard::remove("DEEPSEEK_MODEL");
    let mut config = Config {
        provider: Some("mistral".to_string()),
        ..Config::default()
    };

    apply_env_overrides(&mut config, ConfigEnvironmentPolicy::Runtime);

    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Mistral
    );
    assert_eq!(config.default_model(), "mistral-medium-latest");
}

#[test]
fn codewhale_model_precedes_mistral_model() {
    let _lock = lock_test_env();
    let _provider_model = EnvVarGuard::set("MISTRAL_MODEL", "mistral-small-latest");
    let _generic_model = EnvVarGuard::set("CODEWHALE_MODEL", "mistral-medium-latest");
    let _legacy_model = EnvVarGuard::remove("DEEPSEEK_MODEL");
    let mut config = Config {
        provider: Some("mistral".to_string()),
        ..Config::default()
    };

    apply_env_overrides(&mut config, ConfigEnvironmentPolicy::Runtime);

    assert_eq!(config.default_model(), "mistral-medium-latest");
}

#[test]
fn xai_custom_endpoint_keeps_root_default_model_pass_through() {
    let _lock = lock_test_env();
    let mut providers = ProvidersConfig::default();
    providers.xai.base_url = Some("https://proxy.example.test/v1".to_string());
    let config = Config {
        provider: Some("xai".to_string()),
        default_text_model: Some("deepseek-v4-pro".to_string()),
        providers: Some(providers),
        ..Default::default()
    };
    assert_eq!(
        config.default_model(),
        "deepseek-v4-pro",
        "custom compatible endpoints may serve any model id"
    );
}

#[test]
fn xai_explicit_provider_model_is_honored_over_vendor_default() {
    let _lock = lock_test_env();
    let mut providers = ProvidersConfig::default();
    providers.xai.model = Some("grok-4.5-mini".to_string());
    let config = Config {
        provider: Some("xai".to_string()),
        default_text_model: Some("deepseek-v4-pro".to_string()),
        providers: Some(providers),
        ..Default::default()
    };
    assert_eq!(config.default_model(), "grok-4.5-mini");
}

#[test]
fn nvidia_nim_env_overrides_provider_and_credentials() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-nim-env-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "nvidia-nim");
        env::set_var("NVIDIA_API_KEY", "nim-env-key");
        env::set_var("NVIDIA_NIM_MODEL", "deepseek-ai/deepseek-v4-pro");
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::NvidiaNim
    );
    assert_eq!(config.active_route_api_key()?, "nim-env-key");
    assert_eq!(config.default_model(), DEFAULT_NVIDIA_NIM_MODEL);
    Ok(())
}

#[test]
fn nvidia_nim_env_accepts_facade_base_url_forwarding() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-nim-forwarded-base-url-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "nvidia-nim");
        env::set_var("DEEPSEEK_BASE_URL", "https://forwarded-nim.example/v1");
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::NvidiaNim
    );
    assert_eq!(
        config.active_route_base_url(),
        "https://forwarded-nim.example/v1"
    );
    Ok(())
}

#[test]
fn openai_provider_uses_openai_compatible_defaults() -> Result<()> {
    let config = Config {
        provider: Some("openai".to_string()),
        ..Default::default()
    };

    config.validate()?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Openai
    );
    assert_eq!(config.default_model(), "gpt-5.6");
    assert_eq!(config.active_route_base_url(), "https://api.openai.com/v1");
    assert_eq!(
        codewhale_config::provider::provider_for_kind(codewhale_config::ProviderKind::Openai)
            .default_model(),
        DEFAULT_OPENAI_MODEL
    );
    Ok(())
}

#[test]
fn openai_codex_default_model_falls_back_to_codex_model() {
    // The Codex Responses backend only accepts its own model family, and a
    // global `default_text_model` is validated to DeepSeek IDs (or "auto"),
    // so with the Codex provider it must resolve to the Codex default
    // instead of leaking a DeepSeek id the backend rejects.
    // Isolate from any ambient developer Codex roster: the seed fallback is
    // asserted here; the fresh-roster preference (#5034) is covered by
    // codex_switch_without_saved_model_prefers_fresh_roster_head.
    let _lock = lock_test_env();
    let empty_codex_home = tempfile::tempdir().expect("empty codex home");
    let _codex_home = EnvVarGuard::set("CODEX_HOME", empty_codex_home.path());
    let with_deepseek_default = Config {
        provider: Some("openai-codex".to_string()),
        default_text_model: Some(DEFAULT_TEXT_MODEL.to_string()),
        ..Default::default()
    };
    assert_eq!(
        with_deepseek_default
            .active_provider_identity()
            .unwrap()
            .provider,
        ProviderKind::OpenaiCodex
    );
    assert_eq!(
        with_deepseek_default.default_model(),
        DEFAULT_OPENAI_CODEX_MODEL
    );

    // No global default resolves the same way.
    let bare = Config {
        provider: Some("openai-codex".to_string()),
        ..Default::default()
    };
    assert_eq!(bare.default_model(), DEFAULT_OPENAI_CODEX_MODEL);

    // An explicit provider-scoped model still wins over the fallback.
    let mut providers = ProvidersConfig::default();
    providers.openai_codex.model = Some("gpt-5.5-codex-preview".to_string());
    let pinned = Config {
        provider: Some("openai-codex".to_string()),
        default_text_model: Some(DEFAULT_TEXT_MODEL.to_string()),
        providers: Some(providers),
        ..Default::default()
    };
    assert_eq!(pinned.default_model(), "gpt-5.5-codex-preview");
}

#[test]
fn direct_provider_ignores_foreign_deepseek_root_default_model() {
    let _lock = lock_test_env();

    let config = Config {
        provider: Some("zai".to_string()),
        default_text_model: Some(DEFAULT_TEXT_MODEL.to_string()),
        ..Default::default()
    };

    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Zai
    );
    assert_eq!(config.default_model(), DEFAULT_ZAI_MODEL);
}

#[test]
fn insecure_skip_scenario() {
    // Scenario consolidation of: insecure_skip_tls_verify_is_scoped_to_active_provider, insecure_skip_tls_verify_reads_active_provider_table
    // from insecure_skip_tls_verify_is_scoped_to_active_provider
    {
        let mut providers = ProvidersConfig::default();
        providers.deepseek.insecure_skip_tls_verify = Some(true);
        providers.openai.insecure_skip_tls_verify = Some(false);
        let config = Config {
            provider: Some("openai".to_string()),
            providers: Some(providers),
            ..Default::default()
        };

        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::Openai
        );
        assert!(!config.insecure_skip_tls_verify());
    }
    // from insecure_skip_tls_verify_reads_active_provider_table
    {
        let mut providers = ProvidersConfig::default();
        providers.openai.insecure_skip_tls_verify = Some(true);
        let config = Config {
            provider: Some("openai".to_string()),
            providers: Some(providers),
            ..Default::default()
        };

        assert!(config.insecure_skip_tls_verify());
    }
}

#[test]
fn xiaomi_mimo_scenario() -> Result<()> {
    // Scenario consolidation of: xiaomi_mimo_provider_uses_documented_defaults, xiaomi_mimo_provider_honours_root_default_model_and_base_url, xiaomi_mimo_provider_drops_stale_deepseek_root_default_model, xiaomi_mimo_token_plan_mode_accepts_region_aliases, xiaomi_mimo_unknown_mode_stays_on_token_plan_endpoint
    // from xiaomi_mimo_provider_uses_documented_defaults
    {
        let _lock = lock_test_env();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_root = env::temp_dir().join(format!(
            "codewhale-tui-xiaomi-mimo-defaults-{}-{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&temp_root)?;
        let _guard = EnvGuard::new(&temp_root);

        let config = Config {
            provider: Some("xiaomi-mimo".to_string()),
            ..Default::default()
        };

        config.validate()?;
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::XiaomiMimo
        );
        assert_eq!(config.default_model(), DEFAULT_XIAOMI_MIMO_MODEL);
        assert_eq!(config.active_route_base_url(), DEFAULT_XIAOMI_MIMO_BASE_URL);
    }
    // from xiaomi_mimo_provider_honours_root_default_model_and_base_url
    {
        let config = Config {
            provider: Some("xiaomi-mimo".to_string()),
            default_text_model: Some("mimo-v2.5".to_string()),
            ..Default::default()
        }
        .with_legacy_root(
            None,
            Some("https://token-plan-cn.xiaomimimo.com/v1".to_string()),
        );

        config.validate()?;
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::XiaomiMimo
        );
        assert_eq!(config.default_model(), "mimo-v2.6-flash");
        assert_eq!(
            config.active_route_base_url(),
            "https://token-plan-cn.xiaomimimo.com/v1"
        );
    }
    // from xiaomi_mimo_provider_drops_stale_deepseek_root_default_model
    {
        // A leftover DeepSeek id after a provider switch must not be forwarded to
        // Xiaomi. Fall back to the MiMo seed default instead of substituting a
        // different *configured* model.
        let config = Config {
            provider: Some("xiaomi-mimo".to_string()),
            default_text_model: Some(DEFAULT_OPENROUTER_MODEL.to_string()),
            ..Default::default()
        };

        config.validate()?;
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::XiaomiMimo
        );
        assert_eq!(config.default_model(), DEFAULT_XIAOMI_MIMO_MODEL);
    }
    // from xiaomi_mimo_token_plan_mode_accepts_region_aliases
    {
        let config: Config = toml::from_str(
            r#"
    provider = "mimo"

    [providers.mimo]
    mode = "token-plan-ams"
    "#,
        )?;

        config.validate()?;
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::XiaomiMimo
        );
        assert_eq!(
            config.active_route_base_url(),
            XIAOMI_MIMO_TOKEN_PLAN_AMS_BASE_URL
        );
    }
    // from xiaomi_mimo_unknown_mode_stays_on_token_plan_endpoint
    {
        let config: Config = toml::from_str(
            r#"
    provider = "mimo"

    [providers.mimo]
    mode = "token-plan-usa"
    "#,
        )?;

        config.validate()?;
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::XiaomiMimo
        );
        assert_eq!(config.active_route_base_url(), DEFAULT_XIAOMI_MIMO_BASE_URL);
    }
    Ok(())
}

#[test]
fn only_the_identity_that_owns_the_legacy_root_may_inherit_it() -> Result<()> {
    // The legacy root `base_url` is the DeepSeek field. A foreign value must
    // never become another route's endpoint: the custom-endpoint guard withholds
    // that route's credential from the foreign host, so the route answers with
    // the *other* vendor's unauthenticated 401 and the user reads it as a bad
    // key. The literal `provider = "custom"` shape is deliberately excluded —
    // it owns the root by design — and named custom providers read their own
    // `[providers.<name>]` table.
    let foreign = "https://api.deepseek.com";
    for name in [
        "deepseek",
        "deepseek-cn",
        "xiaomi-mimo",
        "openai-codex",
        "nvidia-nim",
    ] {
        let Some(provider) = ProviderKind::parse(name) else {
            continue;
        };
        // Parsed, not constructed: the legacy top-level key only exists in a
        // file, and parsing moves it into its owner's table (#6394).
        let config = legacy_config(&format!(
            "provider = \"{name}\"\nbase_url = \"{foreign}\"\n"
        ));
        config.validate()?;
        let resolved = config.active_route_base_url();
        if matches!(provider, ProviderKind::Deepseek) {
            assert_eq!(resolved, foreign, "{name} owns the legacy root");
        } else {
            assert!(
                !resolved.contains("api.deepseek.com"),
                "{name} inherited a foreign legacy root: {resolved}"
            );
        }
    }
    Ok(())
}

#[test]
fn xiaomi_mimo_ignores_an_unrelated_legacy_root_base_url() -> Result<()> {
    // The legacy root `base_url` is a DeepSeek field. Inheriting it aimed MiMo
    // traffic — and the MiMo key probe — at api.deepseek.com, whose
    // unauthenticated 401 ("Authentication Fails (governor)") then read as a
    // rejected MiMo key. Only MiMo's own hosts may be inherited.
    let config =
        legacy_config("provider = \"xiaomi-mimo\"\nbase_url = \"https://api.deepseek.com\"\n");

    config.validate()?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::XiaomiMimo
    );
    assert_eq!(config.active_route_base_url(), DEFAULT_XIAOMI_MIMO_BASE_URL);
    assert_eq!(
        config.base_url_for_route(&config.test_identity_for_kind(ProviderKind::Deepseek)),
        "https://api.deepseek.com"
    );

    // A MiMo host on the same legacy field moves to `[providers.xiaomi_mimo]`.
    let config = legacy_config(
        "provider = \"xiaomi-mimo\"\nbase_url = \"https://token-plan-ams.xiaomimimo.com/v1\"\n",
    );

    config.validate()?;
    assert_eq!(
        config.active_route_base_url(),
        XIAOMI_MIMO_TOKEN_PLAN_AMS_BASE_URL
    );
    Ok(())
}

#[test]
fn openai_codex_provider_ignores_legacy_root_base_url() -> Result<()> {
    // `base_url` is the legacy DeepSeek setting in a normal multi-provider
    // config. Switching to Codex must not inherit it and make the official
    // CLI OAuth login ineligible.
    let config = legacy_config(
        "provider = \"openai-codex\"\nbase_url = \"https://api.deepseek.com\"\ndefault_text_model = \"gpt-5.5\"\n",
    );

    config.validate()?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::OpenaiCodex
    );
    assert_eq!(config.default_model(), "gpt-5.5");
    assert_eq!(
        config.active_route_base_url(),
        DEFAULT_OPENAI_CODEX_BASE_URL
    );
    assert!(
        !config.provider_uses_custom_endpoint(
            &config.test_identity_for_kind(ProviderKind::OpenaiCodex)
        )
    );
    Ok(())
}

#[test]
fn xiaomi_provider_alias_table_maps_to_mimo_config() -> Result<()> {
    let config: Config = toml::from_str(
        r#"
provider = "xiaomi-mimo"
default_text_model = "deepseek/deepseek-v4-pro"

[providers.xiaomi]
api_key = "mimo-table-key"
base_url = "https://token-plan-sgp.xiaomimimo.com/v1"
model = "mimo-v2.6-pro"
"#,
    )?;

    config.validate()?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::XiaomiMimo
    );
    assert_eq!(config.active_route_api_key()?, "mimo-table-key");
    assert_eq!(
        config.active_route_base_url(),
        "https://token-plan-sgp.xiaomimimo.com/v1"
    );
    assert_eq!(config.default_model(), DEFAULT_XIAOMI_MIMO_MODEL);
    Ok(())
}

#[test]
fn xiaomi_token_plan_key_rewrites_saved_pay_as_you_go_base_url() -> Result<()> {
    let config: Config = toml::from_str(
        r#"
provider = "xiaomi-mimo"

[providers.xiaomi_mimo]
api_key = "tp-test-token-plan-key"
base_url = "https://api.xiaomimimo.com/v1"
model = "mimo-v2.6-pro"
"#,
    )?;

    config.validate()?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::XiaomiMimo
    );
    assert_eq!(config.active_route_base_url(), DEFAULT_XIAOMI_MIMO_BASE_URL);
    assert_eq!(config.default_model(), DEFAULT_XIAOMI_MIMO_MODEL);
    Ok(())
}

#[test]
fn xiaomi_mimo_env_token_plan_mode_uses_token_plan_key_and_endpoint() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-xiaomi-mimo-token-plan-env-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "xiaomi-mimo");
        env::set_var("XIAOMI_MIMO_MODE", "token-plan-cn");
        env::set_var("XIAOMI_MIMO_TOKEN_PLAN_API_KEY", "tp-env-key");
        env::set_var("XIAOMI_MIMO_API_KEY", "sk-env-key");
        env::set_var("XIAOMI_MIMO_MODEL", "voiceclone");
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::XiaomiMimo
    );
    assert_eq!(config.active_route_api_key()?, "tp-env-key");
    assert_eq!(
        config.active_route_base_url(),
        XIAOMI_MIMO_TOKEN_PLAN_CN_BASE_URL
    );
    assert_eq!(config.default_model(), "voiceclone");
    Ok(())
}

#[test]
fn xiaomi_mimo_env_pay_as_you_go_mode_prefers_standard_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-xiaomi-mimo-payg-env-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "xiaomi-mimo");
        env::set_var("XIAOMI_MIMO_MODE", "pay-as-you-go");
        env::set_var("XIAOMI_MIMO_TOKEN_PLAN_API_KEY", "tp-env-key");
        env::set_var("XIAOMI_MIMO_API_KEY", "sk-env-key");
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::XiaomiMimo
    );
    assert_eq!(config.active_route_api_key()?, "sk-env-key");
    assert_eq!(
        config.active_route_base_url(),
        XIAOMI_MIMO_PAY_AS_YOU_GO_BASE_URL
    );
    Ok(())
}

#[test]
fn atlascloud_provider_uses_documented_defaults() -> Result<()> {
    let config = Config {
        provider: Some("atlascloud".to_string()),
        ..Default::default()
    };

    config.validate()?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Atlascloud
    );
    assert_eq!(config.default_model(), DEFAULT_ATLASCLOUD_MODEL);
    assert_eq!(config.active_route_base_url(), DEFAULT_ATLASCLOUD_BASE_URL);
    Ok(())
}

#[test]
fn atlascloud_env_overrides_provider_base_url_and_model() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-atlascloud-env-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "atlascloud");
        env::set_var("ATLASCLOUD_API_KEY", "atlascloud-env-key");
        env::set_var("ATLASCLOUD_BASE_URL", "https://api.atlascloud.ai/v1");
        env::set_var("ATLASCLOUD_MODEL", "deepseek-ai/deepseek-v4-flash");
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Atlascloud
    );
    assert_eq!(config.active_route_api_key()?, "atlascloud-env-key");
    assert_eq!(
        config.active_route_base_url(),
        "https://api.atlascloud.ai/v1"
    );
    assert_eq!(config.default_model(), "deepseek-ai/deepseek-v4-flash");
    Ok(())
}

#[test]
fn wanjie_ark_provider_uses_documented_defaults() -> Result<()> {
    let config = Config {
        provider: Some("wanjie-ark".to_string()),
        ..Default::default()
    };

    config.validate()?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::WanjieArk
    );
    assert_eq!(config.default_model(), DEFAULT_WANJIE_ARK_MODEL);
    assert_eq!(config.active_route_base_url(), DEFAULT_WANJIE_ARK_BASE_URL);
    Ok(())
}

#[test]
fn wanjie_ark_provider_accepts_custom_model_and_table_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-wanjie-table-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "wanjie-ark"

[providers.wanjie_ark]
api_key = "wanjie-table-key"
base_url = "https://maas-openapi.wanjiedata.com/api/v1"
model = "account-model-id"
"#,
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::WanjieArk
    );
    assert_eq!(config.active_route_api_key()?, "wanjie-table-key");
    assert_eq!(
        config.active_route_base_url(),
        "https://maas-openapi.wanjiedata.com/api/v1"
    );
    assert_eq!(config.default_model(), "account-model-id");
    Ok(())
}

#[test]
fn openai_provider_accepts_custom_model_and_base_url() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-openai-table-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "openai"

[providers.openai]
api_key = "openai-table-key"
base_url = "https://openai-compatible.example/api/coding/paas/v4"
model = "glm-5"
"#,
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Openai
    );
    assert_eq!(config.active_route_api_key()?, "openai-table-key");
    assert_eq!(
        config.active_route_base_url(),
        "https://openai-compatible.example/api/coding/paas/v4"
    );
    assert_eq!(config.default_model(), "glm-5");
    Ok(())
}

#[test]
fn openai_provider_accepts_dashscope_bailian_fixture() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-dashscope-openai-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "openai"

[providers.openai]
api_key = "dashscope-table-key"
base_url = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"
model = "qwen-plus"
"#,
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Openai
    );
    assert_eq!(config.active_route_api_key()?, "dashscope-table-key");
    assert_eq!(
        config.active_route_base_url(),
        "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"
    );
    assert_eq!(config.default_model(), "qwen-plus");
    Ok(())
}

#[test]
fn qianfan_provider_accepts_custom_model_and_base_url() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-qianfan-provider-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "qianfan"

[providers.qianfan]
api_key = "qianfan-table-key"
base_url = "https://qianfan.baidubce.com/v2"
model = "custom-qianfan-service-id"
"#,
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Qianfan
    );
    assert_eq!(config.active_route_api_key()?, "qianfan-table-key");
    assert_eq!(
        config.active_route_base_url(),
        "https://qianfan.baidubce.com/v2"
    );
    assert_eq!(config.default_model(), "custom-qianfan-service-id");
    Ok(())
}

#[test]
fn provider_config_loads_reasoning_stream_style() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-reasoning-style-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "openai"

[providers.openai]
api_key = "openai-table-key"
base_url = "https://openai-compatible.example/v1"
model = "custom-reasoner"
reasoning_stream_style = "inline_tags"
"#,
    )?;

    let config = Config::load(None, None)?;
    let openai = config
        .provider_config_for(&config.test_identity_for_kind(ProviderKind::Openai))
        .expect("openai provider config");
    assert_eq!(
        openai.reasoning_stream_style.as_deref(),
        Some("inline_tags")
    );
    Ok(())
}

// Regression for issue #1714: `codewhale --provider openai --model
// MiniMax-M2.7` forwards the choice via DEEPSEEK_MODEL (never
// OPENAI_MODEL) and uses the DEFAULT base_url. The explicit custom model
// must pass through verbatim instead of silently becoming a
// DeepSeek/provider default.
#[test]
fn deepseek_model_env_passes_custom_model_through_for_non_deepseek_providers() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-1714-passthrough-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;

    // (a) provider=openai + model="MiniMax-M2.7" via env, NO OPENAI_MODEL,
    // DEFAULT base_url.
    {
        let _guard = EnvGuard::new(&temp_root);
        // Safety: test-only environment mutation guarded by a global mutex.
        unsafe {
            env::set_var("DEEPSEEK_PROVIDER", "openai");
            env::set_var("OPENAI_API_KEY", "openai-env-key");
            env::set_var("DEEPSEEK_MODEL", "MiniMax-M2.7");
        }

        let config = Config::load(None, None)?;
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::Openai
        );
        assert_eq!(config.active_route_base_url(), DEFAULT_OPENAI_BASE_URL);
        assert_eq!(config.default_model(), "MiniMax-M2.7");
    }

    // (b) a non-passthrough provider (novita) with an unknown custom model
    // and the DEFAULT base_url must also be preserved verbatim — never
    // rewritten to DEFAULT_NOVITA_MODEL.
    {
        let _guard = EnvGuard::new(&temp_root);
        // Safety: test-only environment mutation guarded by a global mutex.
        unsafe {
            env::set_var("DEEPSEEK_PROVIDER", "novita");
            env::set_var("NOVITA_API_KEY", "novita-env-key");
            env::set_var("DEEPSEEK_MODEL", "MiniMax-M2.7");
        }

        let config = Config::load(None, None)?;
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::Novita
        );
        assert_eq!(config.active_route_base_url(), DEFAULT_NOVITA_BASE_URL);
        assert_ne!(config.default_model(), DEFAULT_NOVITA_MODEL);
        assert_eq!(config.default_model(), "MiniMax-M2.7");
    }

    Ok(())
}

#[test]
fn openrouter_provider_uses_canonical_defaults() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-or-defaults-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config = Config {
        provider: Some("openrouter".to_string()),
        ..Default::default()
    };
    config.validate()?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Openrouter
    );
    assert_eq!(config.default_model(), DEFAULT_OPENROUTER_MODEL);
    assert_eq!(config.active_route_base_url(), DEFAULT_OPENROUTER_BASE_URL);
    Ok(())
}

#[test]
fn novita_provider_uses_canonical_defaults() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-novita-defaults-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config = Config {
        provider: Some("novita".to_string()),
        ..Default::default()
    };
    config.validate()?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Novita
    );
    assert_eq!(config.default_model(), DEFAULT_NOVITA_MODEL);
    assert_eq!(config.active_route_base_url(), DEFAULT_NOVITA_BASE_URL);
    Ok(())
}

#[test]
fn fireworks_provider_uses_canonical_defaults() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-fireworks-defaults-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config = Config {
        provider: Some("fireworks".to_string()),
        ..Default::default()
    };
    config.validate()?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Fireworks
    );
    assert_eq!(config.default_model(), DEFAULT_FIREWORKS_MODEL);
    assert_eq!(config.active_route_base_url(), DEFAULT_FIREWORKS_BASE_URL);
    Ok(())
}

#[test]
fn fireworks_flash_alias_is_not_mapped_to_undocumented_model() -> Result<()> {
    let config = Config {
        provider: Some("fireworks".to_string()),
        default_text_model: Some("deepseek-v4-flash".to_string()),
        ..Default::default()
    };

    config.validate()?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Fireworks
    );
    assert_eq!(config.default_model(), "deepseek-v4-flash");
    Ok(())
}

#[test]
fn volcengine_provider_requires_api_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-volcengine-auth-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config = Config {
        provider: Some("volcengine".to_string()),
        ..Default::default()
    };

    config.validate()?;
    let err = config.active_route_api_key().expect_err("missing key");
    assert!(err.to_string().contains("Volcengine Ark API key not found"));
    Ok(())
}

#[test]
fn siliconflow_provider_uses_canonical_defaults() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-siliconflow-defaults-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config = Config {
        provider: Some("siliconflow".to_string()),
        ..Default::default()
    };
    config.validate()?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Siliconflow
    );
    assert_eq!(config.default_model(), DEFAULT_SILICONFLOW_MODEL);
    assert_eq!(config.active_route_base_url(), DEFAULT_SILICONFLOW_BASE_URL);
    assert_eq!(
        model_completion_names_for_provider(ProviderKind::Siliconflow),
        vec![DEFAULT_SILICONFLOW_MODEL, DEFAULT_SILICONFLOW_FLASH_MODEL]
    );
    Ok(())
}

#[test]
fn ollama_cloud_env_precedence_is_cloud_name_then_official_name() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    // Safety: test-only environment mutation guarded by a global mutex and
    // restored by EnvGuard.
    unsafe {
        env::set_var("OLLAMA_CLOUD_API_KEY", "cloud-specific-key");
        env::set_var("OLLAMA_API_KEY", "official-fallback-key");
    }
    let config = Config {
        provider: Some("ollama-cloud".to_string()),
        ..Config::default()
    };

    assert_eq!(config.active_route_api_key()?, "cloud-specific-key");
    // Safety: same serialized test and EnvGuard restore the prior value.
    unsafe { env::remove_var("OLLAMA_CLOUD_API_KEY") };
    assert_eq!(config.active_route_api_key()?, "official-fallback-key");
    Ok(())
}

#[test]
fn migrated_ollama_cloud_scope_preserves_legacy_table_and_slot_read_only() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let codewhale_home = temp_root.path().join("isolated-codewhale");
    fs::create_dir_all(&codewhale_home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    codewhale_secrets::Secrets::auto_detect().set("ollama", "legacy-cloud-key")?;

    let config = Config {
        provider: Some("deepseek".to_string()),
        providers: Some(ProvidersConfig {
            ollama: ProviderConfig {
                base_url: Some(codewhale_config::provider::OLLAMA_CLOUD_BASE_URL.to_string()),
                model: Some("legacy-cloud-model".to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        }),
        ..Config::default()
    };
    let identity = config
        .resolve_provider_identity("ollama")
        .expect("legacy identity migrates");
    assert_eq!(identity.provider, ProviderKind::OllamaCloud);
    assert_eq!(identity.key.as_str(), "ollama-cloud");
    assert!(identity.migrated_legacy_ollama_cloud_route);

    let mut scoped = config.clone();
    scoped.scope_to_provider_identity(&identity).unwrap();
    assert_eq!(
        scoped.active_provider_identity().unwrap().provider,
        ProviderKind::OllamaCloud
    );
    assert_eq!(
        scoped.active_route_base_url(),
        codewhale_config::provider::OLLAMA_CLOUD_BASE_URL
    );
    assert_eq!(scoped.default_model(), "legacy-cloud-model");
    assert_eq!(scoped.active_route_api_key()?, "legacy-cloud-key");
    assert!(
        scoped
            .providers
            .as_ref()
            .expect("providers")
            .ollama_cloud
            .api_key
            .is_none(),
        "migration must not copy secret material into the new config table"
    );
    assert_eq!(
        codewhale_secrets::Secrets::auto_detect().get("ollama-cloud")?,
        None,
        "migration must not copy the legacy secret into the new slot"
    );
    Ok(())
}

#[test]
fn ollama_model_is_passed_through_verbatim() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-ollama-model-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "ollama"

[providers.ollama]
base_url = "http://127.0.0.1:11434/v1"
model = "qwen2.5-coder:7b"
"#,
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Ollama
    );
    assert_eq!(config.default_model(), "qwen2.5-coder:7b");
    assert_eq!(config.active_route_base_url(), "http://127.0.0.1:11434/v1");
    Ok(())
}

#[test]
fn deepseek_base_url_env_scopes_to_self_hosted_providers() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-self-hosted-base-url-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "ollama");
        env::set_var("DEEPSEEK_BASE_URL", "http://ollama.remote:11434/v1");
    }
    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Ollama
    );
    assert_eq!(
        config.active_route_base_url(),
        "http://ollama.remote:11434/v1"
    );

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "vllm");
        env::set_var("DEEPSEEK_BASE_URL", "http://vllm.remote:8000/v1");
    }
    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Vllm
    );
    assert_eq!(config.active_route_base_url(), "http://vllm.remote:8000/v1");
    Ok(())
}

#[test]
fn vllm_env_resolves_reported_lan_http_endpoint_and_model() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-vllm-lan-http-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "vllm");
        env::set_var("VLLM_BASE_URL", "http://192.168.0.110:8000/v1");
        env::set_var("DEEPSEEK_MODEL", "deepseek-v4-flash");
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Vllm
    );
    assert_eq!(
        config.active_route_base_url(),
        "http://192.168.0.110:8000/v1"
    );
    assert_eq!(config.default_model(), "deepseek-v4-flash");
    Ok(())
}

#[test]
fn ollama_env_overrides_base_url_and_model() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-ollama-env-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "ollama-local");
        env::set_var("OLLAMA_BASE_URL", "http://ollama.example/v1");
        env::set_var("OLLAMA_MODEL", "deepseek-coder-v2:16b");
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Ollama
    );
    assert_eq!(config.active_route_base_url(), "http://ollama.example/v1");
    assert_eq!(config.default_model(), "deepseek-coder-v2:16b");
    Ok(())
}

#[test]
fn openrouter_env_api_key_resolves_via_deepseek_api_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-or-env-key-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "openrouter");
        env::set_var("OPENROUTER_API_KEY", "or-env-key");
        env::set_var("OPENROUTER_MODEL", "deepseek-v4-flash");
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Openrouter
    );
    assert_eq!(config.active_route_api_key()?, "or-env-key");
    assert_eq!(config.default_model(), DEFAULT_OPENROUTER_FLASH_MODEL);
    Ok(())
}

#[test]
fn novita_env_api_key_resolves_via_deepseek_api_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-novita-env-key-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "novita");
        env::set_var("NOVITA_API_KEY", "novita-env-key");
        env::set_var("NOVITA_MODEL", "deepseek-v4-flash");
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Novita
    );
    assert_eq!(config.active_route_api_key()?, "novita-env-key");
    assert_eq!(config.default_model(), DEFAULT_NOVITA_FLASH_MODEL);
    Ok(())
}

#[test]
fn fireworks_env_overrides_key_and_model() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-fireworks-env-key-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "fireworks");
        env::set_var("FIREWORKS_API_KEY", "fw-env-key");
        env::set_var(
            "FIREWORKS_MODEL",
            "accounts/fireworks/models/account-specific-model",
        );
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Fireworks
    );
    assert_eq!(config.active_route_api_key()?, "fw-env-key");
    assert_eq!(
        config.default_model(),
        "accounts/fireworks/models/account-specific-model"
    );
    Ok(())
}

#[test]
fn arcee_provider_uses_direct_defaults() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-arcee-defaults-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "arcee");
        env::set_var("ARCEE_API_KEY", "arcee-env-key");
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Arcee
    );
    assert_eq!(config.active_route_api_key()?, "arcee-env-key");
    assert_eq!(config.active_route_base_url(), DEFAULT_ARCEE_BASE_URL);
    assert_eq!(config.default_model(), DEFAULT_ARCEE_MODEL);
    Ok(())
}

#[test]
fn arcee_provider_table_configures_direct_route() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-arcee-table-test-{}-{}",
        std::process::id(),
        nanos
    ));
    let config_dir = temp_root.join(".deepseek");
    fs::create_dir_all(&config_dir)?;
    let _guard = EnvGuard::new(&temp_root);
    fs::write(
        config_dir.join("config.toml"),
        r#"
provider = "arcee"

[providers.arcee]
api_key = "arcee-file-key"
base_url = "https://api.arcee.ai/api/v1"
model = "arcee-trinity-large-preview"
"#,
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Arcee
    );
    assert_eq!(config.active_route_api_key()?, "arcee-file-key");
    assert_eq!(config.active_route_base_url(), DEFAULT_ARCEE_BASE_URL);
    assert_eq!(config.default_model(), ARCEE_TRINITY_LARGE_PREVIEW_MODEL);
    Ok(())
}

#[test]
fn siliconflow_cn_base_url_env_normalizes_model_aliases() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-siliconflow-cn-env-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "siliconflow-CN");
        env::set_var("SILICONFLOW_API_KEY", "sf-env-key");
        env::set_var("SILICONFLOW_BASE_URL", "https://api.siliconflow.cn/v1");
        env::set_var("SILICONFLOW_MODEL", "deepseek-reasoner");
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::SiliconflowCN
    );
    assert_eq!(config.active_route_api_key()?, "sf-env-key");
    assert_eq!(
        config.active_route_base_url(),
        "https://api.siliconflow.cn/v1"
    );
    assert_eq!(config.default_model(), DEFAULT_SILICONFLOW_MODEL);
    Ok(())
}

#[test]
fn openrouter_base_url_env_overrides_default() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-or-base-url-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "openrouter");
        env::set_var("OPENROUTER_BASE_URL", "https://or-mirror.example/v1");
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Openrouter
    );
    assert_eq!(
        config.active_route_base_url(),
        "https://or-mirror.example/v1"
    );
    Ok(())
}

#[test]
fn openrouter_reads_provider_table_from_config_file() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-or-table-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "openrouter"

[providers.openrouter]
api_key = "or-table-key"
base_url = "https://or-table.example/v1"
"#,
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Openrouter
    );
    assert_eq!(config.active_route_api_key()?, "or-table-key");
    assert_eq!(
        config.active_route_base_url(),
        "https://or-table.example/v1"
    );
    Ok(())
}

#[test]
fn siliconflow_reads_provider_table_from_config_file() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-siliconflow-table-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "siliconflow"

[providers.siliconflow]
api_key = "sf-table-key"
model = "deepseek-v4-flash"
"#,
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Siliconflow
    );
    assert_eq!(config.active_route_api_key()?, "sf-table-key");
    assert_eq!(config.active_route_base_url(), DEFAULT_SILICONFLOW_BASE_URL);
    assert_eq!(config.default_model(), DEFAULT_SILICONFLOW_FLASH_MODEL);
    Ok(())
}

#[test]
fn siliconflow_cn_preserves_reported_deepseek_prefixed_v4_route() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-siliconflow-cn-v4-report-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "siliconflow-CN"

[providers.siliconflow-CN]
api_key = "sf-cn-table-key"
base_url = "https://api.siliconflow.cn/v1"
model = "deepseek-ai/DeepSeek-V4-Pro"
"#,
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::SiliconflowCN
    );
    assert_ne!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Deepseek
    );
    assert_eq!(config.active_route_api_key()?, "sf-cn-table-key");
    assert_eq!(
        config.active_route_base_url(),
        DEFAULT_SILICONFLOW_CN_BASE_URL
    );
    assert_eq!(config.default_model(), DEFAULT_SILICONFLOW_MODEL);
    assert_eq!(
        wire_model_for_provider(
            config.active_provider_identity().unwrap().provider,
            &config.default_model()
        ),
        DEFAULT_SILICONFLOW_MODEL
    );
    Ok(())
}

#[test]
fn siliconflow_cn_falls_back_to_shared_siliconflow_table_when_unset() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-siliconflow-cn-fallback-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "siliconflow-CN"

[providers.siliconflow]
api_key = "sf-shared-key"
base_url = "https://api.siliconflow.com/v1"
model = "deepseek-chat"

[providers.siliconflow_cn]
base_url = "https://api.siliconflow.cn/v1"
"#,
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::SiliconflowCN
    );
    assert_eq!(config.active_route_api_key()?, "sf-shared-key");
    assert_eq!(
        config.active_route_base_url(),
        DEFAULT_SILICONFLOW_CN_BASE_URL
    );
    assert_eq!(config.default_model(), DEFAULT_SILICONFLOW_FLASH_MODEL);
    assert!(active_provider_has_config_api_key(&config));
    Ok(())
}

#[test]
fn siliconflow_cn_env_overrides_write_cn_table_only() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-siliconflow-cn-env-table-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "siliconflow-CN"

[providers.siliconflow]
api_key = "sf-shared-key"
base_url = "https://api.siliconflow.com/v1"
model = "deepseek-reasoner"
"#,
    )?;
    unsafe {
        env::set_var("SILICONFLOW_BASE_URL", "https://api.siliconflow.cn/v1");
        env::set_var("SILICONFLOW_MODEL", "deepseek-chat");
    }

    let config = Config::load(None, None)?;
    let providers = config.providers.as_ref().expect("providers");
    assert_eq!(
        providers.siliconflow.base_url.as_deref(),
        Some(DEFAULT_SILICONFLOW_BASE_URL)
    );
    assert_eq!(
        providers.siliconflow.model.as_deref(),
        Some(DEFAULT_SILICONFLOW_MODEL)
    );
    assert_eq!(
        providers.siliconflow_cn.base_url.as_deref(),
        Some(DEFAULT_SILICONFLOW_CN_BASE_URL)
    );
    assert_eq!(
        providers.siliconflow_cn.model.as_deref(),
        Some(DEFAULT_SILICONFLOW_FLASH_MODEL)
    );
    assert_eq!(config.active_route_api_key()?, "sf-shared-key");
    assert_eq!(config.default_model(), DEFAULT_SILICONFLOW_FLASH_MODEL);
    Ok(())
}

#[test]
fn openrouter_custom_base_url_preserves_provider_model() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-or-custom-model-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "openrouter"

[providers.openrouter]
api_key = "or-table-key"
base_url = "https://gateway.example.com/v1"
model = "DeepSeek-V4-Pro"
"#,
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Openrouter
    );
    assert_eq!(config.active_route_api_key()?, "or-table-key");
    assert_eq!(
        config.active_route_base_url(),
        "https://gateway.example.com/v1"
    );
    assert_eq!(config.default_model(), "DeepSeek-V4-Pro");
    Ok(())
}

#[test]
fn novita_reads_provider_table_from_config_file() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-novita-table-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "novita"

[providers.novita]
api_key = "novita-table-key"
"#,
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Novita
    );
    assert_eq!(config.active_route_api_key()?, "novita-table-key");
    assert_eq!(config.active_route_base_url(), DEFAULT_NOVITA_BASE_URL);
    Ok(())
}

#[test]
fn moonshot_credential_help_keeps_direct_and_kimi_code_routes_distinct() {
    let direct =
        credential_help_for_provider_route(ProviderKind::Moonshot, DEFAULT_MOONSHOT_BASE_URL);
    assert_eq!(
        direct.credential_url,
        Some("https://platform.kimi.ai/console/api-keys")
    );
    assert_eq!(
        direct.docs_url,
        Some("https://platform.kimi.ai/docs/overview")
    );

    let kimi_code =
        credential_help_for_provider_route(ProviderKind::Moonshot, DEFAULT_KIMI_CODE_BASE_URL);
    assert_eq!(
        kimi_code.credential_url,
        Some(KIMI_CODE_MEMBERSHIP_PLAN_CONSOLE_URL)
    );
    assert_eq!(kimi_code.docs_url, None);
    assert!(kimi_code.guidance.contains("membership-plan API key"));
    assert!(
        kimi_code
            .guidance
            .contains("does not import Kimi CLI credentials")
    );
}

#[test]
fn codex_read_only_consent_reads_exact_file_without_mutation() -> Result<()> {
    let _lock = lock_test_env();
    let temp = tempfile::tempdir()?;
    let temp_root = temp.path().canonicalize()?;
    let auth_path = temp_root.join("codex-auth.json");
    let token = crate::test_support::future_test_jwt("codex");
    let raw = serde_json::to_string_pretty(&json!({
        "tokens": {
            "access_token": token.clone(),
            "account_id": "acct-read-only",
            "refresh_token": "must-never-be-used",
            "future_field": ["preserve"]
        },
        "future_top_level": true
    }))?;
    fs::write(&auth_path, &raw)?;
    let ambient_decoy = temp_root.join("ambient-decoy.json");
    let ambient_decoy_raw = r#"{"tokens":{"access_token":"must-not-be-read"}}"#;
    fs::write(&ambient_decoy, ambient_decoy_raw)?;
    let _auth_path = EnvVarGuard::set("OPENAI_CODEX_AUTH_FILE", &ambient_decoy);
    let _access = EnvVarGuard::remove("OPENAI_CODEX_ACCESS_TOKEN");
    let _legacy_access = EnvVarGuard::remove("CODEX_ACCESS_TOKEN");
    let config = Config {
        provider: Some(ProviderKind::OpenaiCodex.as_str().to_string()),
        providers: Some(ProvidersConfig {
            openai_codex: ProviderConfig {
                auth_mode: Some("oauth".to_string()),
                external_credentials: Some(
                    codewhale_config::ExternalCredentialConsentToml::read_only(
                        codewhale_config::ProviderKind::OpenaiCodex,
                        codewhale_config::ExternalCredentialSource::CodexCli,
                        auth_path.clone(),
                    ),
                ),
                ..Default::default()
            },
            ..Default::default()
        }),
        ..Default::default()
    };

    let mut inactive = config.clone();
    inactive.provider = Some(ProviderKind::Deepseek.as_str().to_string());
    crate::external_credentials::reset_side_effect_trap();
    assert!(inactive.external_credential_read_consent_configured(
        &inactive.test_identity_for_kind(ProviderKind::OpenaiCodex),
        codewhale_config::ExternalCredentialSource::CodexCli,
    ));
    let dormant_error = inactive
        .external_credential_read_grant(
            &inactive.test_identity_for_kind(ProviderKind::OpenaiCodex),
            codewhale_config::ExternalCredentialSource::CodexCli,
            &ambient_decoy,
        )
        .expect_err("inactive providers cannot mint external read capabilities");
    assert!(dormant_error.to_string().contains("explicitly selected"));
    assert_eq!(
        crate::external_credentials::side_effect_trap_counts(),
        (0, 0)
    );

    let active_grant = config.external_credential_read_grant(
        &config.test_identity_for_kind(ProviderKind::OpenaiCodex),
        codewhale_config::ExternalCredentialSource::CodexCli,
        &ambient_decoy,
    )?;
    assert_eq!(
        active_grant.path(),
        auth_path,
        "the selected route remains pinned to the persisted consent path"
    );

    crate::external_credentials::reset_side_effect_trap();
    assert!(config.active_route_api_key().is_err());
    assert_eq!(
        crate::external_credentials::side_effect_trap_counts(),
        (0, 0)
    );
    assert_eq!(fs::read_to_string(&auth_path)?, raw);
    assert_eq!(fs::read_to_string(&ambient_decoy)?, ambient_decoy_raw);

    drop(_access);
    let _process_access = EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", "process-token");
    crate::external_credentials::reset_side_effect_trap();
    assert!(config.active_route_api_key().is_err());
    assert_eq!(config.codex_account_id(), None);
    assert_eq!(
        crate::external_credentials::side_effect_trap_counts(),
        (0, 0),
        "external tokens must never substitute for Codewhale-issued plan access"
    );
    Ok(())
}

#[test]
fn moonshot_kimi_code_missing_key_reports_membership_plan_console() -> Result<()> {
    let _lock = lock_test_env();
    let temp = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp.path());
    let config = Config {
        provider: Some(ProviderKind::Moonshot.as_str().to_string()),
        providers: Some(ProvidersConfig {
            moonshot: ProviderConfig {
                base_url: Some(DEFAULT_KIMI_CODE_BASE_URL.to_string()),
                model: Some(KIMI_CODE_K3_MODEL.to_string()),
                ..Default::default()
            },
            ..Default::default()
        }),
        ..Default::default()
    };

    let error = config
        .active_route_api_key()
        .expect_err("Kimi Code route needs a membership-plan API key");
    let message = error.to_string();
    assert!(
        message.contains(KIMI_CODE_MEMBERSHIP_PLAN_CONSOLE_URL),
        "{message}"
    );
    assert!(message.contains("api.kimi.com/coding/v1"), "{message}");
    assert!(
        message.contains("does not import Kimi CLI credentials"),
        "{message}"
    );
    assert!(!message.contains("https://platform.kimi.ai/console/api-keys"));
    Ok(())
}

#[test]
fn moonshot_kimi_code_saved_claude_k3_1m_alias_fails_with_api_model_guidance() -> Result<()> {
    let _lock = lock_test_env();
    let temp = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp.path());
    let config_path = temp.path().join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "moonshot"

[providers.moonshot]
api_key = "kimi-code-key"
base_url = "https://api.kimi.com/coding/v1"
model = "k3[1m]"
"#,
    )?;

    let error = Config::load(None, None)
        .expect_err("saved Claude Code context hints must not become API model ids");
    let message = error.to_string();
    assert!(message.contains("model = \"k3\""), "{message}");
    assert!(message.contains("context_window = 1048576"), "{message}");
    assert!(message.contains("plan includes 1M context"), "{message}");
    assert!(message.contains("262144 safe default"), "{message}");
    Ok(())
}

/// Regression for issue #2160: a stale root `default_text_model` carried
/// over from a DeepSeek setup must not steer the Kimi Code endpoint to
/// `deepseek-v4-pro`. The user-facing trigger here is the legacy
/// `DEEPSEEK_PROVIDER` env var (still produced by the `codewhale
/// --provider moonshot` dispatcher for compat); the test also has a
/// `CODEWHALE_PROVIDER` twin below for the public env path.
#[test]
fn moonshot_kimi_code_model_overrides_root_deepseek_default() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-kimi-code-root-model-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "deepseek"
default_text_model = "deepseek-v4-pro"

[providers.moonshot]
api_key = "kimi-code-key"
base_url = "https://api.kimi.com/coding/v1"
"#,
    )?;
    // Safety: test-only env mutation guarded by lock_test_env().
    unsafe { env::set_var("DEEPSEEK_PROVIDER", "moonshot") };

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Moonshot
    );
    assert_eq!(config.active_route_base_url(), DEFAULT_KIMI_CODE_BASE_URL);
    assert_eq!(config.default_model(), DEFAULT_KIMI_CODE_MODEL);
    Ok(())
}

/// Same regression as above, but driven by the public `CODEWHALE_PROVIDER`
/// env var. Documents the recommended user-facing setup path: never
/// `DEEPSEEK_PROVIDER=moonshot`, always `CODEWHALE_PROVIDER=moonshot`
/// (or `codewhale --provider moonshot`, which also resolves through
/// this code path internally).
#[test]
fn moonshot_kimi_code_model_resolves_via_codewhale_provider_env() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-kimi-code-cw-env-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "deepseek"
default_text_model = "deepseek-v4-pro"

[providers.moonshot]
api_key = "kimi-code-key"
base_url = "https://api.kimi.com/coding/v1"
"#,
    )?;
    // Safety: test-only env mutation guarded by lock_test_env().
    unsafe { env::set_var("CODEWHALE_PROVIDER", "moonshot") };

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Moonshot
    );
    assert_eq!(config.active_route_base_url(), DEFAULT_KIMI_CODE_BASE_URL);
    assert_eq!(config.default_model(), DEFAULT_KIMI_CODE_MODEL);
    Ok(())
}

/// `CODEWHALE_PROVIDER` wins when both it and the legacy
/// `DEEPSEEK_PROVIDER` are set, so a user adding the new alias to their
/// shell isn't surprised by a stale legacy export.
#[test]
fn codewhale_provider_env_takes_precedence_over_deepseek_provider() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-cw-vs-ds-provider-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(&config_path, "provider = \"deepseek\"\n")?;
    // Safety: test-only env mutation guarded by lock_test_env().
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "moonshot");
        env::set_var("DEEPSEEK_PROVIDER", "openrouter");
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Moonshot
    );
    Ok(())
}

/// Moonshot Platform path: when [providers.moonshot] is empty (or
/// missing) and no Kimi Code endpoint is configured, the resolver
/// defaults to the Moonshot Platform base URL and the latest Kimi platform
/// model. This is the "I have a Moonshot Platform API key, not a
/// Kimi Code plan key" path.
#[test]
fn moonshot_platform_defaults_to_kimi_k27_code() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-moonshot-platform-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "moonshot"

[providers.moonshot]
api_key = "moonshot-platform-key"
"#,
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Moonshot
    );
    assert_eq!(config.active_route_base_url(), DEFAULT_MOONSHOT_BASE_URL);
    assert_eq!(config.default_model(), DEFAULT_MOONSHOT_MODEL);
    assert_eq!(config.active_route_api_key()?, "moonshot-platform-key");
    Ok(())
}

#[test]
fn xai_invalid_owned_generation_blocks_external_and_uses_api_key_fallback() -> Result<()> {
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let root = root.path().canonicalize()?;
    let external_path = root.join("grok-auth.json");
    let external_raw = r#"{"token":"external-owner-bytes-must-not-be-read"}"#;
    fs::write(&external_path, external_raw)?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", &root);
    let _auth_path = EnvVarGuard::set("GROK_AUTH_PATH", &external_path);
    let _xai_key = EnvVarGuard::remove("XAI_API_KEY");
    let _xai_base_url = EnvVarGuard::remove("XAI_BASE_URL");

    let mut providers = ProvidersConfig::default();
    providers.xai.api_key = Some("fake-xai-cfg-key".to_string());
    providers.xai.auth_mode = Some("oauth".to_string());
    providers.xai.oauth_credential_generation = Some("../unsafe.json".to_string());
    providers.xai.external_credentials =
        Some(codewhale_config::ExternalCredentialConsentToml::read_only(
            codewhale_config::ProviderKind::Xai,
            codewhale_config::ExternalCredentialSource::GrokCli,
            external_path.clone(),
        ));
    let config = Config {
        provider: Some(ProviderKind::Xai.as_str().to_string()),
        providers: Some(providers),
        ..Config::default()
    };

    crate::external_credentials::reset_side_effect_trap();
    assert!(
        !crate::oauth::credentials_present(crate::oauth::OAuthProvider::Xai, &config),
        "an invalid owned generation pointer must not resolve external OAuth"
    );
    assert_eq!(config.active_route_api_key()?, "fake-xai-cfg-key");
    assert_eq!(
        crate::external_credentials::side_effect_trap_counts(),
        (0, 0),
        "an unusable owned generation must not access the external Grok CLI"
    );
    assert_eq!(fs::read_to_string(external_path)?, external_raw);
    Ok(())
}

/// #6528 — a key pasted with a BOM, zero-width characters, NBSP or internal
/// whitespace is saved clean, and an ambient env key is normalized and named
/// as the source the runtime resolver used.
#[test]
fn pasted_key_is_saved_clean_and_env_source_is_named() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-pasted-key-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);
    let config_path = temp_root.join(".deepseek").join("config.toml");
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path.as_os_str());
    let _secret_backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "local");

    let path = save_api_key_for(
        ProviderKind::Openrouter,
        "\u{feff}or-\u{200b}pasted\u{a0}key \u{2060}\n",
    )?;
    let parsed: toml::Value = toml::from_str(&fs::read_to_string(&path)?)?;
    assert_eq!(
        parsed
            .get("providers")
            .and_then(|p| p.get("openrouter"))
            .and_then(|t| t.get("api_key"))
            .and_then(toml::Value::as_str),
        Some("or-pastedkey")
    );

    let _env_key = EnvVarGuard::set("OPENROUTER_API_KEY", "\u{feff}or-env\u{200d}key\u{a0}");
    let config = Config {
        provider: Some("openrouter".to_string()),
        ..Config::default()
    };
    let (key, source) = config.active_route_api_key_with_source()?;
    assert_eq!(key, "or-envkey");
    assert_eq!(source, "env var OPENROUTER_API_KEY");
    let _ = fs::remove_dir_all(&temp_root);
    Ok(())
}

#[test]
fn save_api_key_for_openrouter_writes_provider_table() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-save-key-or-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);
    let config_path = temp_root.join(".deepseek").join("config.toml");
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path.as_os_str());
    let _secret_backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "local");
    let resolved_config_path = codewhale_config::resolve_config_path(None)?;

    let path = save_api_key_for(ProviderKind::Openrouter, "or-saved-key")?;
    assert_eq!(path, resolved_config_path);
    let contents = fs::read_to_string(&path)?;
    let parsed: toml::Value = toml::from_str(&contents)?;
    assert_eq!(
        parsed
            .get("providers")
            .and_then(|p| p.get("openrouter"))
            .and_then(|t| t.get("api_key"))
            .and_then(toml::Value::as_str),
        Some("or-saved-key")
    );
    // Re-saving must not duplicate or wipe sibling tables.
    let novita_path = save_api_key_for(ProviderKind::Novita, "novita-saved-key")?;
    assert_eq!(novita_path.canonicalize()?, path.canonicalize()?);
    let contents = fs::read_to_string(&path)?;
    let parsed: toml::Value = toml::from_str(&contents)?;
    assert_eq!(
        parsed
            .get("providers")
            .and_then(|p| p.get("openrouter"))
            .and_then(|t| t.get("api_key"))
            .and_then(toml::Value::as_str),
        Some("or-saved-key")
    );
    assert_eq!(
        parsed
            .get("providers")
            .and_then(|p| p.get("novita"))
            .and_then(|t| t.get("api_key"))
            .and_then(toml::Value::as_str),
        Some("novita-saved-key")
    );
    for (provider, key) in [
        (ProviderKind::Openai, "openai-saved-key"),
        (ProviderKind::WanjieArk, "wanjie-saved-key"),
        (ProviderKind::Fireworks, "fireworks-saved-key"),
        (ProviderKind::XiaomiMimo, "mimo-saved-key"),
        (ProviderKind::Siliconflow, "sf-saved-key"),
        (ProviderKind::Sglang, "sglang-saved-key"),
    ] {
        assert_eq!(
            save_api_key_for(provider, key)?.canonicalize()?,
            path.canonicalize()?
        );
    }
    let contents = fs::read_to_string(&path)?;
    let parsed: toml::Value = toml::from_str(&contents)?;
    assert_eq!(
        parsed
            .get("providers")
            .and_then(|p| p.get("openai"))
            .and_then(|t| t.get("api_key"))
            .and_then(toml::Value::as_str),
        Some("openai-saved-key")
    );
    assert_eq!(
        parsed
            .get("providers")
            .and_then(|p| p.get("wanjie_ark"))
            .and_then(|t| t.get("api_key"))
            .and_then(toml::Value::as_str),
        Some("wanjie-saved-key")
    );
    assert_eq!(
        parsed
            .get("providers")
            .and_then(|p| p.get("fireworks"))
            .and_then(|t| t.get("api_key"))
            .and_then(toml::Value::as_str),
        Some("fireworks-saved-key")
    );
    assert_eq!(
        parsed
            .get("providers")
            .and_then(|p| p.get("xiaomi_mimo"))
            .and_then(|t| t.get("api_key"))
            .and_then(toml::Value::as_str),
        Some("mimo-saved-key")
    );
    assert_eq!(
        parsed
            .get("providers")
            .and_then(|p| p.get("siliconflow"))
            .and_then(|t| t.get("api_key"))
            .and_then(toml::Value::as_str),
        Some("sf-saved-key")
    );
    assert_eq!(
        parsed
            .get("providers")
            .and_then(|p| p.get("sglang"))
            .and_then(|t| t.get("api_key"))
            .and_then(toml::Value::as_str),
        Some("sglang-saved-key")
    );
    save_api_key_for(ProviderKind::SiliconflowCN, "sf-cn-saved-key")?;
    let contents = fs::read_to_string(&path)?;
    let parsed: toml::Value = toml::from_str(&contents)?;
    assert_eq!(
        parsed
            .get("providers")
            .and_then(|p| p.get("siliconflow_cn"))
            .and_then(|t| t.get("api_key"))
            .and_then(toml::Value::as_str),
        Some("sf-cn-saved-key")
    );
    assert_eq!(
        parsed
            .get("providers")
            .and_then(|p| p.get("siliconflow"))
            .and_then(|t| t.get("api_key"))
            .and_then(toml::Value::as_str),
        Some("sf-saved-key")
    );
    Ok(())
}

#[test]
fn save_api_key_for_deepseek_cn_uses_root_deepseek_storage() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-save-key-cn-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);
    let config_path = temp_root.join(".deepseek").join("config.toml");
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path.as_os_str());
    let _secret_backend = EnvVarGuard::set("DEEPSEEK_SECRET_BACKEND", "local");

    let path = save_api_key_for(ProviderKind::Deepseek, "cn-saved-key")?;
    assert_eq!(path, config_path);
    let contents = fs::read_to_string(&path)?;
    let parsed: toml::Value = toml::from_str(&contents)?;

    // DeepSeek-CN reads `[providers.deepseek]`, which replaced the shared
    // top-level key (#6394).
    assert_eq!(
        parsed["providers"]["deepseek"]
            .get("api_key")
            .and_then(toml::Value::as_str),
        Some("cn-saved-key")
    );
    assert!(parsed.get("api_key").is_none(), "{contents}");
    Ok(())
}

#[test]
fn nvidia_nim_reads_facade_provider_table() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-nim-provider-table-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "nvidia-nim"
default_text_model = "deepseek-v4-flash"

[providers.nvidia_nim]
api_key = "nim-table-key"
base_url = "https://nim-table.example/v1"
model = "deepseek-v4-pro"
"#,
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::NvidiaNim
    );
    assert_eq!(config.active_route_api_key()?, "nim-table-key");
    assert_eq!(
        config.active_route_base_url(),
        "https://nim-table.example/v1"
    );
    // Custom base URL preserves the user-specified model name; normalisation
    // is skipped because the gateway expects the model name as-provided.
    assert_eq!(config.default_model(), "deepseek-v4-pro");
    Ok(())
}

#[test]
fn nvidia_nim_provider_table_key_overrides_root_deepseek_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-nim-root-key-precedence-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"api_key = "codewhale-root-key"
provider = "nvidia-nim"

[providers.nvidia_nim]
api_key = "nim-table-key"
base_url = "https://integrate.api.nvidia.com/v1"
model = "deepseek-ai/deepseek-v4-pro"
"#,
    )?;

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::NvidiaNim
    );
    assert_eq!(config.active_route_api_key()?, "nim-table-key");
    Ok(())
}

#[test]
fn invalid_provider_scenario() {
    // Scenario consolidation of: invalid_provider_auth_source_is_not_explicit_configuration, invalid_provider_error_lists_huggingface
    // from invalid_provider_auth_source_is_not_explicit_configuration
    {
        let entry = ProviderConfig {
            auth: Some(codewhale_config::ProviderAuthSourceToml {
                source: codewhale_config::AuthSourceKind::Command,
                command: Vec::new(),
                timeout_ms: None,
                secret_id: None,
            }),
            ..ProviderConfig::default()
        };

        assert!(!provider_config_is_explicit(&entry));
    }
    // from invalid_provider_error_lists_huggingface
    {
        let config = Config {
            provider: Some("not-a-provider".to_string()),
            ..Default::default()
        };
        let err = config.validate().expect_err("unknown provider should fail");
        let message = err.to_string();
        assert!(message.contains("Invalid provider 'not-a-provider'"));
        assert!(message.contains("huggingface"));
    }
}

#[test]
fn openrouter_nemotron_ultra_aliases_resolve_to_live_id() {
    assert_eq!(
        OPENROUTER_NEMOTRON_3_ULTRA_MODEL,
        "nvidia/nemotron-3-ultra-550b-a55b"
    );
    assert_ne!(OPENROUTER_NEMOTRON_3_ULTRA_MODEL, "nvidia/nemotron-3-ultra");

    for alias in [
        "nemotron-3-ultra",
        "nvidia/nemotron-3-ultra",
        "nvidia-nemotron-3-ultra",
    ] {
        assert_eq!(
            normalize_model_name_for_provider(ProviderKind::Openrouter, alias).as_deref(),
            Some(OPENROUTER_NEMOTRON_3_ULTRA_MODEL)
        );
    }
}

#[test]
fn status_item_balance_available_for_prepaid_providers() {
    assert!(StatusItem::Balance.is_available_for(ProviderKind::Deepseek));
    assert!(StatusItem::Balance.is_available_for(ProviderKind::Deepseek));
    assert!(StatusItem::Balance.is_available_for(ProviderKind::Openrouter));
    assert!(StatusItem::Balance.is_available_for(ProviderKind::Siliconflow));
    assert!(StatusItem::Balance.is_available_for(ProviderKind::SiliconflowCN));
    // Invoice-only, local, and unimplemented prepaid vendors stay hidden.
    assert!(!StatusItem::Balance.is_available_for(ProviderKind::Novita));
    assert!(!StatusItem::Balance.is_available_for(ProviderKind::NvidiaNim));
    assert!(!StatusItem::Balance.is_available_for(ProviderKind::Fireworks));
    assert!(!StatusItem::Balance.is_available_for(ProviderKind::Sglang));
    assert!(!StatusItem::Balance.is_available_for(ProviderKind::Vllm));
    assert!(!StatusItem::Balance.is_available_for(ProviderKind::Ollama));
    assert!(!StatusItem::Balance.is_available_for(ProviderKind::Openai));
    assert!(!StatusItem::Balance.is_available_for(ProviderKind::Atlascloud));
    // Other StatusItem variants should be available everywhere.
    assert!(StatusItem::Mode.is_available_for(ProviderKind::Ollama));
}

#[test]
fn status_items_scenario() {
    // Scenario consolidation of: status_items_deser_ignores_unknown_variants, status_items_deser_allows_missing_field
    // from status_items_deser_ignores_unknown_variants
    {
        // Simulate a stable build reading config written by a dev build that
        // knows about items the stable build doesn't (e.g. "balance" or a
        // future "cost_saving" chip).
        let toml_str = r#"
            alternate_screen = "auto"
            status_items = ["mode", "model", "unknown_future_item", "cost", "another_unknown", "cache"]
        "#;
        let tui: TuiConfig = toml::from_str(toml_str).expect("should parse without error");
        let items = tui.status_items.expect("status_items should be Some");
        assert_eq!(items.len(), 4, "unknown items should be silently dropped");
        assert_eq!(items[0], StatusItem::Mode);
        assert_eq!(items[1], StatusItem::Model);
        assert_eq!(items[2], StatusItem::Cost);
        assert_eq!(items[3], StatusItem::Cache);
    }
    // A config written before #5950 retired the inert items keeps loading:
    // the retired keys are skipped, the live ones survive in order.
    {
        let toml_str = r#"
            status_items = ["mode", "status", "model", "agents", "rate_limit", "tokens"]
        "#;
        let tui: TuiConfig = toml::from_str(toml_str).expect("legacy items should parse");
        let items = tui.status_items.expect("status_items should be Some");
        assert_eq!(
            items,
            vec![StatusItem::Mode, StatusItem::Model, StatusItem::Tokens],
            "retired keys should drop out without failing the whole file"
        );
    }
    // #6112 revived `git_branch` and added `workspace`: both parse again and
    // round-trip through their canonical keys.
    {
        let toml_str = r#"
            status_items = ["workspace", "git_branch"]
        "#;
        let tui: TuiConfig = toml::from_str(toml_str).expect("revived items should parse");
        let items = tui.status_items.expect("status_items should be Some");
        assert_eq!(items, vec![StatusItem::Workspace, StatusItem::GitBranch]);
        assert_eq!(StatusItem::Workspace.key(), "workspace");
        assert_eq!(StatusItem::GitBranch.key(), "git_branch");
    }
    {
        let tui: TuiConfig =
            toml::from_str(r#"status_items = ["ttft", "output_rate", "session_metrics"]"#)
                .expect("new and legacy metrics keys should parse");
        assert_eq!(
            tui.status_items,
            Some(vec![
                StatusItem::Ttft,
                StatusItem::OutputRate,
                StatusItem::SessionMetrics
            ])
        );
        assert_eq!(StatusItem::Ttft.key(), "ttft");
        assert_eq!(StatusItem::OutputRate.key(), "output_rate");
    }
    // from status_items_deser_allows_missing_field
    {
        let toml_str = r#"
            locale = "zh-Hans"
            mouse_capture = false
        "#;
        let tui: TuiConfig = toml::from_str(toml_str).expect("missing status_items should parse");
        assert_eq!(tui.status_items, None);
    }
}

#[test]
fn transcript_prose_scenario() -> Result<()> {
    // Scenario consolidation of: transcript_prose_measure_loads_and_resolves, transcript_prose_measure_rejects_negative_with_clear_error, transcript_prose_measure_rejects_non_integers_with_clear_error
    // from transcript_prose_measure_loads_and_resolves
    {
        // #5436: absent = full width; 0 also means full width; a positive
        // integer caps prose wrap at that many columns.
        let absent: Config = toml::from_str("provider = \"openai\"\n")?;
        absent.validate()?;
        assert_eq!(absent.prose_measure(), None);

        let zero: Config = toml::from_str(
            "
    [transcript]
    prose_measure = 0
    ",
        )?;
        zero.validate()?;
        assert_eq!(zero.prose_measure(), None, "0 must mean full width");

        let capped: Config = toml::from_str(
            "
    [transcript]
    prose_measure = 120
    ",
        )?;
        capped.validate()?;
        assert_eq!(capped.prose_measure(), Some(120));
    }
    // from transcript_prose_measure_rejects_negative_with_clear_error
    {
        let config: Config = toml::from_str(
            "
    [transcript]
    prose_measure = -5
    ",
        )
        .expect("negative integers must parse so validate can name the key");

        let error = config
            .validate()
            .expect_err("negative prose_measure should be rejected");
        let message = error.to_string();
        assert!(
            message.contains("transcript.prose_measure"),
            "error should name the key: {message}"
        );
        assert!(
            message.contains("-5"),
            "error should echo the value: {message}"
        );
        assert!(
            message.contains("positive whole number"),
            "error should say what is expected: {message}"
        );
    }
    // from transcript_prose_measure_rejects_non_integers_with_clear_error
    {
        for raw in ["\"fill\"", "12.5", "true"] {
            let config: Config = toml::from_str(&format!(
                "
    [transcript]
    prose_measure = {raw}
    "
            ))
            .unwrap_or_else(|_| panic!("{raw} must parse so validate can name the key"));

            let error = config
                .validate()
                .expect_err(&format!("{raw} should be rejected"));
            let message = error.to_string();
            assert!(
                message.contains("transcript.prose_measure"),
                "error should name the key for {raw}: {message}"
            );
            assert!(
                message.contains("positive whole number"),
                "error should say what is expected for {raw}: {message}"
            );
        }
    }
    Ok(())
}

#[test]
fn huggingface_provider_scenario() -> Result<()> {
    // Scenario consolidation of: huggingface_provider_aliases_parse, huggingface_provider_uses_direct_defaults
    // from huggingface_provider_aliases_parse
    {
        for alias in ["huggingface", "hugging-face", "hugging_face", "hf"] {
            assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::Huggingface));
        }
    }
    // from huggingface_provider_uses_direct_defaults
    {
        let _lock = lock_test_env();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_root = env::temp_dir().join(format!(
            "codewhale-tui-huggingface-defaults-test-{}-{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&temp_root)?;
        let _guard = EnvGuard::new(&temp_root);

        unsafe {
            env::set_var("CODEWHALE_PROVIDER", "huggingface");
            env::set_var("HUGGINGFACE_API_KEY", "hf-env-key");
        }

        let config = Config::load(None, None)?;
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::Huggingface
        );
        assert_eq!(config.active_route_api_key()?, "hf-env-key");
        assert_eq!(config.active_route_base_url(), DEFAULT_HUGGINGFACE_BASE_URL);
        assert_eq!(config.default_model(), DEFAULT_HUGGINGFACE_MODEL);
    }
    Ok(())
}

#[test]
fn modelscope_provider_scenario() -> Result<()> {
    // from modelscope_provider_aliases_parse
    {
        for alias in ["modelscope", "modelscope-cn"] {
            assert_eq!(ProviderKind::parse(alias), Some(ProviderKind::Modelscope));
        }
    }
    // from modelscope_provider_uses_direct_defaults
    {
        let _lock = lock_test_env();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_root = env::temp_dir().join(format!(
            "codewhale-tui-modelscope-defaults-test-{}-{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&temp_root)?;
        let _guard = EnvGuard::new(&temp_root);

        unsafe {
            env::set_var("CODEWHALE_PROVIDER", "modelscope");
            env::set_var("MODELSCOPE_API_KEY", "ms-env-key");
        }

        let config = Config::load(None, None)?;
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::Modelscope
        );
        assert_eq!(config.active_route_api_key()?, "ms-env-key");
        assert_eq!(config.active_route_base_url(), DEFAULT_MODELSCOPE_BASE_URL);
        assert_eq!(config.default_model(), DEFAULT_MODELSCOPE_MODEL);
    }
    Ok(())
}

#[test]
fn huggingface_hf_token_env_api_key_resolves() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-huggingface-hf-token-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "huggingface");
        env::set_var("HF_TOKEN", "hf-token-value");
    }

    let config = Config::load(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Huggingface
    );
    assert_eq!(config.active_route_api_key()?, "hf-token-value");
    Ok(())
}

#[test]
fn huggingface_missing_key_error_mentions_env_fallbacks() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-huggingface-missing-key-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config = Config {
        provider: Some("huggingface".to_string()),
        ..Default::default()
    };

    config.validate()?;
    let err = config.active_route_api_key().expect_err("missing key");
    let message = err.to_string();
    assert!(message.contains("Hugging Face API key not found"));
    assert!(message.contains("https://huggingface.co/settings/tokens"));
    assert!(message.contains("HUGGINGFACE_API_KEY"));
    assert!(message.contains("HF_TOKEN"));
    Ok(())
}

#[test]
fn notifications_parse_scenario() {
    // Scenario consolidation of: notifications_parse_custom_completion_sound_file, notifications_parse_quiet_and_event_categories
    // from notifications_parse_custom_completion_sound_file
    {
        let config: Config = toml::from_str(
            r#"
            [notifications]
            completion_sound = "file"
            sound_file = "E:\\google\\downloads\\xm4114.wav"
            "#,
        )
        .expect("custom completion sound config should parse");

        let notifications = config.notifications_config();
        assert_eq!(notifications.completion_sound, CompletionSound::File);
        assert_eq!(
            notifications.sound_file.as_deref(),
            Some(std::path::Path::new("E:\\google\\downloads\\xm4114.wav"))
        );
    }
    // from notifications_parse_quiet_and_event_categories
    {
        let config: Config = toml::from_str(
            r#"
            [notifications]
            quiet = true

            [notifications.events]
            approval-needed = false
            model-notify = false
            "#,
        )
        .expect("quiet + events config should parse");

        let notifications = config.notifications_config();
        assert!(notifications.quiet);
        let events = notifications.events;
        assert!(!events.approval_needed);
        assert!(!events.model_notify);
        // Unlisted categories keep their enabled default.
        assert!(events.turn_complete);
        assert!(events.subagent_terminal);
        assert!(events.input_needed);
        assert!(events.elevation_needed);
    }
}

#[test]
fn notifications_parse_event_sound_table() {
    let config: Config = toml::from_str(
        r#"
        [notifications.event_sound]
        enabled = true
        events = ["turn-complete", "bogus-event", "approval-needed"]
        min_interval_ms = 500
        quiet = true
        "#,
    )
    .expect("event sound config should parse");

    let notifications = config.notifications_config();
    assert_eq!(
        notifications.event_sound,
        EventSoundConfig {
            enabled: true,
            events: vec![
                "turn-complete".to_string(),
                "bogus-event".to_string(),
                "approval-needed".to_string(),
            ],
            min_interval_ms: 500,
            quiet: true,
        }
    );
}

#[test]
fn notifications_event_sound_defaults_when_table_absent() {
    let config: Config = toml::from_str("[notifications]\nmethod = \"off\"\n")
        .expect("bare notifications table should parse");

    let event_sound = config.notifications_config().event_sound;
    assert_eq!(event_sound, EventSoundConfig::default());
    assert!(!event_sound.enabled);
    assert_eq!(
        event_sound.events,
        vec!["turn-complete".to_string(), "approval-needed".to_string()]
    );
    assert_eq!(event_sound.min_interval_ms, 2000);
    assert!(!event_sound.quiet);
}

#[test]
fn notifications_quiet_and_events_default_off_and_all_enabled() {
    let config: Config = toml::from_str("[notifications]\nmethod = \"auto\"\n")
        .expect("bare notifications table should parse");

    let notifications = config.notifications_config();
    assert!(!notifications.quiet);
    assert_eq!(notifications.events, NotificationEventsConfig::default());
    assert!(notifications.events.turn_complete);
    assert!(notifications.events.model_notify);
}

// === #1519 custom OpenAI-compatible provider slice ===

#[test]
fn custom_provider_flatten_map_parses_alongside_named_provider() {
    // A custom `[providers.my_thing]` table lands in the flatten map while a
    // built-in `[providers.openai]` table still binds its named field.
    let config: Config = toml::from_str(
        r#"
provider = "my_thing"

[providers.openai]
api_key = "openai-key"

[providers.my_thing]
kind = "openai-compatible"
base_url = "https://api.example.com/v1"
model = "custom-model-v1"
api_key_env = "EXAMPLE_API_KEY"
"#,
    )
    .expect("config with a custom provider table should parse");

    let providers = config.providers.as_ref().expect("providers table present");
    // Built-in named field still works.
    assert_eq!(providers.openai.api_key.as_deref(), Some("openai-key"));
    // The custom entry is captured by name in the flatten map.
    let custom = providers
        .custom_provider_config("my_thing")
        .expect("custom entry parsed into flatten map");
    assert_eq!(custom.kind.as_deref(), Some("openai-compatible"));
    assert_eq!(
        custom.base_url.as_deref(),
        Some("https://api.example.com/v1")
    );
    assert_eq!(custom.model.as_deref(), Some("custom-model-v1"));
    assert_eq!(custom.api_key_env.as_deref(), Some("EXAMPLE_API_KEY"));
    assert!(custom.is_openai_compatible_custom());
    // A built-in provider name never leaks into the custom map.
    assert!(providers.custom_provider_config("openai").is_none());
}

#[test]
fn api_provider_returns_custom_for_custom_name_and_deepseek_for_junk() {
    // Names a real custom table → Custom (the #1519 silent-misroute fix).
    let mut custom = HashMap::new();
    custom.insert(
        "my_thing".to_string(),
        ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("https://api.example.com/v1".to_string()),
            ..Default::default()
        },
    );
    let config = Config {
        provider: Some("my_thing".to_string()),
        providers: Some(ProvidersConfig {
            custom,
            ..Default::default()
        }),
        ..Config::default()
    };
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Custom
    );
    config
        .validate()
        .expect("named custom providers should pass config validation");

    // An unknown selection has no admitted route; it cannot borrow DeepSeek.
    let junk = Config {
        provider: Some("totally-not-a-provider".to_string()),
        ..Config::default()
    };
    assert!(
        junk.active_provider_identity().is_err(),
        "unknown selections cannot fall back to DeepSeek"
    );
    assert!(
        junk.validate().is_err(),
        "invalid provider names should still fail validation"
    );
}

#[test]
fn custom_provider_scenario() {
    // Scenario consolidation of: custom_provider_kind_only_accepts_openai_compatible, custom_provider_base_url_and_model_resolve_from_named_table
    // from custom_provider_kind_only_accepts_openai_compatible
    {
        let ok = ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            ..Default::default()
        };
        assert!(ok.is_openai_compatible_custom());

        // Underscore spelling and case are tolerated.
        let underscore = ProviderConfig {
            kind: Some("OpenAI_Compatible".to_string()),
            ..Default::default()
        };
        assert!(underscore.is_openai_compatible_custom());

        // Any other declared wire format is rejected (callers error on these).
        let other = ProviderConfig {
            kind: Some("anthropic-messages".to_string()),
            ..Default::default()
        };
        assert!(!other.is_openai_compatible_custom());

        // Built-in providers leave `kind` unset.
        assert!(!ProviderConfig::default().is_openai_compatible_custom());
    }
    // from custom_provider_base_url_and_model_resolve_from_named_table
    {
        let mut custom = HashMap::new();
        custom.insert(
            "my_thing".to_string(),
            ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some("https://api.example.com/v1".to_string()),
                model: Some("custom-model-v1".to_string()),
                ..Default::default()
            },
        );
        let config = Config {
            provider: Some("my_thing".to_string()),
            providers: Some(ProvidersConfig {
                custom,
                ..Default::default()
            }),
            ..Config::default()
        };

        // Resolution reads the named table, not a DeepSeek default.
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::Custom
        );
        assert_eq!(config.active_route_base_url(), "https://api.example.com/v1");
        assert_eq!(config.default_model(), "custom-model-v1");
    }
}

#[test]
fn session_provider_identity_preserves_exact_named_custom_key() {
    let config = session_custom_provider_config(
        "lm-studio",
        "openai-compatible",
        "http://127.0.0.1:1234/v1",
    );

    assert_eq!(
        config.active_provider_identity().unwrap().key.as_str(),
        "lm-studio"
    );
    assert_eq!(
        config
            .resolve_provider_identity("lm-studio")
            .expect("exact custom identity"),
        ProviderIdentity {
            provider: ProviderKind::Custom,
            key: "lm-studio".into(),
            exact_id: Some("lm-studio".into()),
            migrated_legacy_ollama_cloud_route: false,
            legacy_root_custom_generation: None,
        }
    );
    assert_eq!(
        config
            .resolve_provider_identity("openrouter")
            .expect("built-in identity"),
        ProviderIdentity {
            provider: ProviderKind::Openrouter,
            key: "openrouter".into(),
            exact_id: Some("openrouter".into()),
            migrated_legacy_ollama_cloud_route: false,
            legacy_root_custom_generation: None,
        }
    );
    let migrated = config
        .resolve_provider_identity("custom")
        .expect("released generic custom record migrates to sole live named route");
    assert_eq!(
        migrated,
        ProviderIdentity {
            provider: ProviderKind::Custom,
            key: "lm-studio".into(),
            exact_id: Some("lm-studio".into()),
            migrated_legacy_ollama_cloud_route: false,
            legacy_root_custom_generation: None,
        }
    );
}

#[test]
fn persisted_legacy_ollama_cloud_receipts_upgrade_only_on_exact_live_route() {
    let exact = Config {
        provider: Some("ollama".to_string()),
        providers: Some(ProvidersConfig {
            ollama: ProviderConfig {
                base_url: Some(codewhale_config::provider::OLLAMA_CLOUD_BASE_URL.to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        }),
        ..Config::default()
    };
    for provider_id in [None, Some("ollama")] {
        let identity = exact
            .resolve_persisted_provider_identity(Some("ollama"), provider_id)
            .expect("exact released tuple migrates");
        assert_eq!(identity.provider, ProviderKind::OllamaCloud);
        assert_eq!(identity.key.as_str(), "ollama-cloud");
        assert_eq!(identity.persisted_id(), Some("ollama"));
        assert!(identity.migrated_legacy_ollama_cloud_route);
    }

    let neighbor = Config {
        provider: Some("ollama".to_string()),
        providers: Some(ProvidersConfig {
            ollama: ProviderConfig {
                base_url: Some("https://ollama.com/v1/preview".to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        }),
        ..Config::default()
    };
    let identity = neighbor
        .resolve_persisted_provider_identity(Some("ollama"), Some("ollama"))
        .expect("neighbor remains local/custom ollama identity");
    assert_eq!(identity.provider, ProviderKind::Ollama);
    assert_eq!(identity.key.as_str(), "ollama");

    let explicit = Config {
        provider: Some("ollama-cloud".to_string()),
        ..Config::default()
    };
    let identity = explicit
        .resolve_persisted_provider_identity(Some("ollama-cloud"), Some("ollama-cloud"))
        .expect("new receipt remains first-class cloud identity");
    assert_eq!(identity.provider, ProviderKind::OllamaCloud);
    assert_eq!(identity.key.as_str(), "ollama-cloud");
    assert_eq!(identity.persisted_id(), Some("ollama-cloud"));
    assert!(!identity.migrated_legacy_ollama_cloud_route);

    let coexisting = Config {
        provider: Some("ollama".to_string()),
        providers: Some(ProvidersConfig {
            ollama: ProviderConfig {
                base_url: Some(codewhale_config::provider::OLLAMA_CLOUD_BASE_URL.to_string()),
                ..ProviderConfig::default()
            },
            ollama_cloud: ProviderConfig {
                base_url: Some(codewhale_config::provider::OLLAMA_CLOUD_BASE_URL.to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        }),
        ..Config::default()
    };
    let explicit = coexisting
        .resolve_persisted_provider_identity(Some("ollama-cloud"), Some("ollama-cloud"))
        .expect("explicit receipt stays on the first-class route");
    assert!(!explicit.migrated_legacy_ollama_cloud_route);
    assert_eq!(explicit.persisted_id(), Some("ollama-cloud"));

    let mut explicit_live = coexisting.clone();
    explicit_live.provider = Some("ollama-cloud".to_string());
    let migrated = explicit_live
        .resolve_persisted_provider_identity(Some("ollama-cloud"), Some("ollama"))
        .expect("legacy receipt retains its source route after a live provider switch");
    assert!(migrated.migrated_legacy_ollama_cloud_route);
    assert_eq!(migrated.persisted_id(), Some("ollama"));
}

#[test]
fn persisted_custom_fields_require_root_provenance_when_exact_id_is_absent() {
    let table_only = Config::from_saved_document(
        r#"provider = "custom"
        [providers.custom]
        kind = "openai-compatible"
        base_url = "http://127.0.0.1:1234/v1"
        model = "local-model"
    "#,
        None,
    )
    .unwrap();
    assert!(
        table_only
            .resolve_persisted_provider_identity(Some("custom"), None)
            .is_err()
    );
    let exact = table_only
        .resolve_persisted_provider_identity(Some("custom"), Some("custom"))
        .unwrap();
    assert_eq!(exact.persisted_id(), Some("custom"));

    let root_only = Config::from_saved_document(
        r#"provider = "custom"
        base_url = "http://127.0.0.1:18180/v1"
        default_text_model = "legacy-root-model"
    "#,
        None,
    )
    .unwrap();
    let root = root_only
        .resolve_persisted_provider_identity(Some("custom"), None)
        .unwrap();
    assert_eq!(root.provider, ProviderKind::Custom);
    assert_eq!(root.key.as_str(), "custom");
    assert_eq!(root.persisted_id(), None);
    assert_eq!(root_only.active_provider_identity().unwrap(), root);
    assert_eq!(
        root_only.base_url_for_route(&root),
        "http://127.0.0.1:18180/v1"
    );
    assert_eq!(
        root_only
            .resolve_persisted_provider_identity(Some("custom"), Some("custom"))
            .unwrap()
            .persisted_id(),
        Some("custom")
    );
    let mut scoped = root_only.clone();
    scoped.scope_to_provider_identity(&root).unwrap();
    assert_eq!(
        scoped.base_url_for_route(&root),
        "http://127.0.0.1:18180/v1"
    );
    assert!(format!("{root:?}").contains("<redacted>"));
    assert!(!format!("{root:?}").contains("18180"));

    // A pre-existing exact table owns its own endpoint. The conflicting root
    // endpoint is routed to DeepSeek by #6394 and cannot authorize id-less custom.
    let coexist = Config::from_saved_document(
        r#"provider = "custom"
        base_url = "http://127.0.0.1:18180/v1"
        [providers.custom]
        kind = "openai-compatible"
        base_url = "http://127.0.0.1:1234/v1"
        model = "local-model"
    "#,
        None,
    )
    .unwrap();
    assert!(
        coexist
            .resolve_persisted_provider_identity(Some("custom"), None)
            .is_err()
    );
    let exact = coexist
        .resolve_persisted_provider_identity(Some("custom"), Some("custom"))
        .unwrap();
    assert_eq!(
        coexist.base_url_for_route(&exact),
        "http://127.0.0.1:1234/v1"
    );
    assert_eq!(
        coexist.deepseek_table_base_url(),
        Some("http://127.0.0.1:18180/v1")
    );
}

#[test]
fn persisted_empty_custom_id_never_authorizes_a_route() {
    let config = Config::from_saved_document(
        r#"provider = "custom"
        base_url = "http://127.0.0.1:18180/v1"
        default_text_model = "legacy-root-model"
    "#,
        None,
    )
    .unwrap();
    for malformed_id in ["", "   "] {
        let error = config
            .resolve_persisted_provider_identity(Some("custom"), Some(malformed_id))
            .unwrap_err();
        assert!(error.contains("empty exact provider id"), "{error}");
        assert!(error.contains("will not guess or fall back"), "{error}");
    }
    assert_eq!(
        config
            .resolve_persisted_provider_identity(Some("custom"), None)
            .unwrap()
            .persisted_id(),
        None
    );
    assert_eq!(
        config
            .resolve_persisted_provider_identity(Some("custom"), Some("custom"))
            .unwrap()
            .persisted_id(),
        Some("custom")
    );
}

#[test]
fn persisted_provider_pair_never_collapses_builtin_into_same_key_custom_route() {
    let config =
        session_custom_provider_config("openai", "openai-compatible", "http://127.0.0.1:1234/v1");
    assert_eq!(
        config
            .resolve_provider_identity("openai")
            .expect("raw exact identity intentionally prefers custom"),
        ProviderIdentity {
            provider: ProviderKind::Custom,
            key: "openai".into(),
            exact_id: Some("openai".into()),
            migrated_legacy_ollama_cloud_route: false,
            legacy_root_custom_generation: None,
        }
    );

    for provider_id in [None, Some("openai")] {
        let error = config
            .resolve_persisted_provider_identity(Some("openai"), provider_id)
            .expect_err("built-in record must not be captured by the custom table");
        assert!(error.contains("requires built-in 'openai'"), "{error}");
        assert!(error.contains("shadows"), "{error}");
        assert!(error.contains("will not guess or fall back"), "{error}");
    }

    let exact_custom = config
        .resolve_persisted_provider_identity(Some("custom"), Some("openai"))
        .expect("custom kind plus exact id intentionally selects the table");
    assert_eq!(exact_custom.provider, ProviderKind::Custom);
    assert_eq!(exact_custom.key.as_str(), "openai");

    let mismatch = config
        .resolve_persisted_provider_identity(Some("openrouter"), Some("openai"))
        .expect_err("mismatched built-in kind/id pair must fail closed");
    assert!(mismatch.contains("mismatched fields"), "{mismatch}");
}

#[test]
fn legacy_literal_custom_identity_resolves_to_its_migrated_table() {
    let _lock = lock_test_env();
    let _source = EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
    let _cli_key = EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
    let legacy = Config {
        provider: Some("custom".to_string()),
        default_text_model: Some("local-legacy-model".to_string()),
        ..Config::default()
    }
    .with_legacy_root(
        Some("legacy-root-key".to_string()),
        Some("http://127.0.0.1:1234/v1".to_string()),
    );

    assert_eq!(
        legacy
            .resolve_provider_identity("custom")
            .expect("unchanged legacy root route"),
        // The top-level route became the exact `[providers.custom]` table
        // when parsed (#6394).
        ProviderIdentity {
            provider: ProviderKind::Custom,
            key: "custom".into(),
            exact_id: Some("custom".into()),
            migrated_legacy_ollama_cloud_route: false,
            legacy_root_custom_generation: None,
        }
    );
    assert_eq!(legacy.active_route_base_url(), "http://127.0.0.1:1234/v1");
    assert_eq!(legacy.default_model(), "local-legacy-model");
    assert_eq!(legacy.active_route_api_key().unwrap(), "legacy-root-key");

    let mut named = session_custom_provider_config(
        "lm-studio",
        "openai-compatible",
        "https://api.example.com/v1",
    );
    named.set_legacy_root(Some("must-not-leak-to-named-route".to_string()), None);
    let named_key_error = named
        .active_route_api_key()
        .expect_err("root legacy key must never authorize a named custom route")
        .to_string();
    assert!(named_key_error.contains("lm-studio"), "{named_key_error}");
    assert!(!named_key_error.contains("must-not-leak"));

    let mut ambiguous_named = named.clone();
    ambiguous_named
        .providers
        .as_mut()
        .expect("providers")
        .custom
        .insert(
            "vllm-local".to_string(),
            ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some("http://127.0.0.1:8000/v1".to_string()),
                model: Some("other-local-model".to_string()),
                ..ProviderConfig::default()
            },
        );
    let ambiguous_named_error = ambiguous_named
        .resolve_provider_identity("custom")
        .expect_err("generic released record cannot choose between named routes");
    assert!(
        ambiguous_named_error.contains("valid named routes: 2"),
        "{ambiguous_named_error}"
    );
    assert!(ambiguous_named_error.contains("will not guess or fall back"));

    let removed_named = legacy
        .resolve_provider_identity("lm-studio")
        .expect_err("a removed named route must not fall back to legacy custom");
    assert!(removed_named.contains("[providers.lm-studio]"));
    assert!(removed_named.contains("will not fall back"));
}

#[test]
fn legacy_literal_custom_env_overrides_keep_the_literal_route() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-legacy-custom-env-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);
    let _source = EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
    let _cli_key = EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "custom"
api_key = "legacy-root-key"
base_url = "http://127.0.0.1:18184/v1"
default_text_model = "legacy-model"
"#,
    )?;
    // Safety: test-only env mutation guarded by lock_test_env().
    unsafe {
        env::set_var("CODEWHALE_BASE_URL", "http://127.0.0.1:18185/v1");
        env::set_var("CODEWHALE_MODEL", "env-legacy-model");
        env::set_var("DEEPSEEK_HTTP_HEADERS", "X-Legacy-Route=kept");
    }

    let config = Config::load(None, None)?;

    // The top-level route moved into `[providers.custom]` when parsed (#6394).
    assert!(config.selects_literal_custom_provider());
    assert!(
        config
            .providers
            .as_ref()
            .is_some_and(|providers| providers.custom.contains_key("custom"))
    );
    assert_eq!(config.active_route_base_url(), "http://127.0.0.1:18185/v1");
    assert_eq!(config.default_model(), "env-legacy-model");
    assert_eq!(
        config.active_route_api_key()?,
        "",
        "an env-selected keyless loopback route must not inherit the file-owned root key"
    );
    assert!(!active_provider_has_config_api_key(&config));
    assert_eq!(
        config
            .http_headers()
            .get("X-Legacy-Route")
            .map(String::as_str),
        Some("kept")
    );
    for _ in 0..2 {
        assert_eq!(
            config
                .resolve_provider_identity("custom")
                .expect("legacy route remains repeatedly resolvable")
                .key
                .as_str(),
            "custom"
        );
    }
    Ok(())
}

#[test]
fn session_provider_identity_fails_closed_for_removed_or_invalid_custom_table() {
    let removed = Config::default();
    let missing = removed
        .resolve_provider_identity("lm-studio")
        .expect_err("removed provider must fail closed");
    assert!(missing.contains("[providers.lm-studio]"));
    assert!(missing.contains("will not fall back"));

    let invalid_kind = session_custom_provider_config(
        "lm-studio",
        "anthropic-messages",
        "http://127.0.0.1:1234/v1",
    );
    let kind_error = invalid_kind
        .resolve_provider_identity("lm-studio")
        .expect_err("unsupported custom wire kind must fail closed");
    assert!(kind_error.contains("kind = \"openai-compatible\""));

    let invalid_url =
        session_custom_provider_config("lm-studio", "openai-compatible", "not a provider URL");
    let url_error = invalid_url
        .resolve_provider_identity("lm-studio")
        .expect_err("invalid custom URL must fail closed");
    assert!(url_error.contains("base_url"));
    assert!(url_error.contains("will not fall back"));
}

#[test]
fn picker_consent_persists_only_confirmed_exact_scope_and_revoke_is_one_step() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let config_path = dir.path().join("config.toml");
    let external_path = dir.path().join("codex-auth.json");
    std::fs::write(
        &config_path,
        "# preserve operator comment\n[providers.openai_codex]\nmodel = \"gpt-5-codex\" # preserve model\n",
    )
    .expect("seed config");
    // #5772: persistence now reads and validates the exact confirmed file
    // before any consent record is written, so the fixture must hold a live
    // token (far-future JWT exp) for the grant to be saved at all.
    std::fs::write(
        &external_path,
        "{\"tokens\":{\"access_token\":\"header.eyJleHAiOjk5OTk5OTk5OTl9.sig\"}}",
    )
    .expect("seed external credential");
    // The secure adapter rejects symlinked path components; macOS `/var` is
    // one, so the confirmed path must be the canonical one.
    let external_path = external_path
        .canonicalize()
        .expect("canonical external credential path");
    let mut live = Config {
        provider: Some(ProviderKind::OpenaiCodex.as_str().to_string()),
        ..Config::default()
    };

    crate::external_credentials::reset_side_effect_trap();
    {
        let captured_fixture_identity = (live).test_identity_for_kind(ProviderKind::OpenaiCodex);
        persist_external_credential_consent_for_at(
            Some(&config_path),
            &mut live,
            &captured_fixture_identity,
            codewhale_config::ProviderKind::OpenaiCodex,
            codewhale_config::ExternalCredentialSource::CodexCli,
            &external_path,
        )
    }
    .expect("persist confirmed consent");
    let saved = std::fs::read_to_string(&config_path).expect("saved config");
    assert!(saved.contains("# preserve operator comment"));
    assert!(saved.contains("model = \"gpt-5-codex\" # preserve model"));
    assert!(saved.contains("access = \"read_only\""));
    assert!(saved.contains("source = \"codex_cli\""));
    assert!(saved.contains(&external_path.display().to_string()));
    let consent = live
        .provider_config_for(&live.test_identity_for_kind(ProviderKind::OpenaiCodex))
        .and_then(|entry| entry.external_credentials.as_ref())
        .expect("live consent");
    assert_eq!(consent.path, external_path);
    assert_eq!(
        crate::external_credentials::complete_side_effect_trap_counts(),
        (1, 1, 0, 0, 0),
        "grant persistence performs exactly the confirmed read (one secure open, one \
         bounded read) and never writes, refreshes, or reaches the network"
    );

    crate::external_credentials::reset_side_effect_trap();
    {
        let captured_fixture_identity = (live).test_identity_for_kind(ProviderKind::OpenaiCodex);
        revoke_external_credential_consent_for_at(
            Some(&config_path),
            &mut live,
            &captured_fixture_identity,
        )
    }
    .expect("one-step revoke");
    let revoked = std::fs::read_to_string(&config_path).expect("revoked config");
    assert!(!revoked.contains("external_credentials"));
    assert!(
        live.provider_config_for(&live.test_identity_for_kind(ProviderKind::OpenaiCodex))
            .and_then(|entry| entry.external_credentials.as_ref())
            .is_none()
    );
    assert_eq!(
        crate::external_credentials::complete_side_effect_trap_counts(),
        (0, 0, 0, 0, 0)
    );
}

/// Every provider must accept the model ids it advertises for itself.
///
/// Regression for #4829: `validate()` checked `default_text_model` against the
/// DeepSeek-only normalizer, so a config our own setup wizard writes
/// (`provider = "zai"`, `default_text_model = "GLM-5.2"`) was rejected on every
/// startup — the CLI could not launch at all. This asserts the equal-treatment
/// contract from CLAUDE.md: no provider's own models are second-class.
#[test]
fn validate_accepts_every_providers_own_advertised_models() {
    for &provider in ProviderKind::all() {
        for model in model_completion_names_for_provider(provider) {
            let config = Config {
                provider: Some(provider.as_str().to_string()),
                default_text_model: Some(model.to_string()),
                ..Default::default()
            };
            assert!(
                config.validate().is_ok(),
                "provider {} rejected its own advertised model {model}: {:?}",
                provider.as_str(),
                config.validate().unwrap_err().to_string(),
            );
        }
    }
}

/// The exact config that bricked the CLI in the field.
#[test]
fn validate_accepts_zai_glm_model_from_setup_wizard() {
    let config = Config {
        provider: Some("zai".to_string()),
        default_text_model: Some(DEFAULT_ZAI_MODEL.to_string()),
        ..Default::default()
    };
    config
        .validate()
        .expect("setup-wizard zai/GLM config must validate");
}

/// The official DeepSeek gate is the one legitimate per-family rejection and
/// must survive the fix — this is what keeps the validation meaningful.
#[test]
fn validate_still_rejects_unknown_model_on_official_deepseek() {
    let config = Config {
        provider: Some("deepseek".to_string()),
        default_text_model: Some("definitely-not-a-deepseek-model".to_string()),
        ..Default::default()
    };
    let err = config
        .validate()
        .expect_err("official DeepSeek must reject foreign ids")
        .to_string();
    assert!(
        err.contains("definitely-not-a-deepseek-model") && err.contains("deepseek"),
        "error should name the model and the active provider, got: {err}"
    );
}

#[test]
fn validate_checks_the_selected_provider_model_before_the_root_fallback() {
    for (model, root, valid) in [
        ("deepseek-v4-pro", "old-provider-model", true),
        ("auto", "old-provider-model", true),
        ("not-a-deepseek-model", "deepseek-v4-pro", false),
        ("", "deepseek-v4-pro", false),
        ("   ", "deepseek-v4-pro", false),
    ] {
        let mut config = Config {
            provider: Some("deepseek".to_string()),
            default_text_model: Some(root.to_string()),
            ..Default::default()
        };
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Deepseek))
            .unwrap()
            .model = Some(model.to_string());
        let result = config.validate();
        assert_eq!(
            result.is_ok(),
            valid,
            "selected {model:?}, root {root:?}: {result:?}"
        );
        if valid {
            assert_eq!(config.default_model(), model);
        }
        assert_eq!(config.default_text_model.as_deref(), Some(root));
    }

    // Exact route declarations remain first-class; a declaration for another
    // provider cannot make an unknown model valid on this route.
    let mut declared: Config = toml::from_str(
        r#"provider = "deepseek"
default_text_model = "old-provider-model"
[providers.deepseek]
base_url = "https://api.deepseek.com"
model = "account-model"
[[custom_models]]
provider = "deepseek"
base_url = "https://api.deepseek.com"
id = "account-model"
"#,
    )
    .unwrap();
    declared.validate().expect("matching declared model");
    assert_eq!(declared.default_model(), "account-model");
    declared.custom_models.as_mut().unwrap()[0].provider = "zai".to_string();
    assert!(declared.validate().is_err());
}

#[test]
fn native_memory_scenario() {
    // Scenario consolidation of: native_memory_backend_owns_explicit_path, native_memory_path_honours_an_already_native_setting
    // from native_memory_backend_owns_explicit_path
    {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy-memory.md");
        let config = Config {
            memory_path: Some(legacy.to_string_lossy().into_owned()),
            memory: Some(MemoryConfig {
                backend: Some(MemoryBackend::Native),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(config.memory_backend(), MemoryBackend::Native);
        assert!(config.memory_enabled());
        assert_eq!(
            config.memory_path(),
            tmp.path().join("memory/global/MEMORY.md")
        );
    }
    // from native_memory_path_honours_an_already_native_setting
    {
        // Pointing `memory_path` at a native store is the obvious reading of the
        // name; it used to nest a second store inside and write to the wrong file.
        let mut config = Config::default();
        config.memory = Some(crate::config::MemoryConfig {
            enabled: Some(true),
            ..Default::default()
        });
        config.memory_path = Some("/tmp/cw-test/memory/global/MEMORY.md".to_string());
        assert_eq!(
            config.memory_path(),
            std::path::PathBuf::from("/tmp/cw-test/memory/global/MEMORY.md")
        );

        // A legacy single-file setting still anchors the store beside it.
        config.memory_path = Some("/tmp/cw-test/memory.md".to_string());
        assert_eq!(
            config.memory_path(),
            std::path::PathBuf::from("/tmp/cw-test/memory/global/MEMORY.md")
        );
    }
}

/// Pins the v0.9.4 memory consolidation: with `[memory] enabled = true`
/// (no explicit backend), the resolved memory path is always the native
/// `memory/global/MEMORY.md` layout, so `NativeMemoryStore::from_global_path`
/// accepts it and the deleted legacy single-file branch can never be taken.
#[test]
fn enabled_memory_always_resolves_to_native_store_path() {
    let tmp = tempfile::tempdir().unwrap();
    let config: Config = toml::from_str(
        r#"
        [memory]
        enabled = true
        "#,
    )
    .expect("parse enabled memory config");
    assert_eq!(config.memory_backend(), MemoryBackend::Native);
    let path = config.memory_path();
    assert!(
        path.ends_with("memory/global/MEMORY.md"),
        "enabled memory must resolve to the native layout, got {}",
        path.display()
    );
    assert!(
        crate::native_memory::NativeMemoryStore::from_global_path(&path).is_some(),
        "native store must accept the resolved memory path"
    );

    // Even an explicitly configured single-file path is re-rooted into the
    // native layout under the native backend.
    let custom = Config {
        memory_path: Some(tmp.path().join("memory.md").to_string_lossy().into_owned()),
        memory: Some(MemoryConfig {
            enabled: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };
    let custom_path = custom.memory_path();
    assert!(
        crate::native_memory::NativeMemoryStore::from_global_path(&custom_path).is_some(),
        "custom memory paths are re-rooted into the native layout, got {}",
        custom_path.display()
    );
}

/// v0.9.1 kimi-k3 dogfood report: a dogfood user ran `codewhale --provider moonshot --model kimi-k3`
/// and the session kept reporting `kimi-k2.7-code`. The `--model` flag reaches
/// this binary as `CODEWHALE_MODEL`, so the route it produces is asserted here
/// end to end: the effective model, the endpoint, and the id that goes on the
/// wire must all be the one the user named.
#[test]
fn cli_model_flag_selects_kimi_k3_on_the_moonshot_platform_route() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-kimi-k3-cli-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // `EnvGuard` points DEEPSEEK_CONFIG_PATH at `<home>/.deepseek/config.toml`.
    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        "provider = \"moonshot\"\n\n[providers.moonshot]\napi_key = \"k\"\n",
    )?;
    // Safety: test-only env mutation guarded by lock_test_env().
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "moonshot");
        env::set_var("CODEWHALE_MODEL", MOONSHOT_KIMI_K3_MODEL);
    }

    let config = Config::load(None, None)?;

    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Moonshot
    );
    assert_eq!(config.default_model(), MOONSHOT_KIMI_K3_MODEL);
    assert_eq!(config.active_route_base_url(), DEFAULT_MOONSHOT_BASE_URL);
    assert_eq!(
        wire_model_for_provider_route(
            ProviderKind::Moonshot,
            &config.active_route_base_url(),
            &config.default_model(),
        ),
        MOONSHOT_KIMI_K3_MODEL,
        "the id the user named must be the id on the wire"
    );
    assert_eq!(
        explicit_launch_model_override().as_deref(),
        Some(MOONSHOT_KIMI_K3_MODEL),
        "an explicit --model must remain recognizable as an explicit request"
    );
    assert_eq!(
        moonshot_k3_route_display_name(&config.active_route_base_url(), &config.default_model()),
        Some("Moonshot direct / kimi-k3")
    );
    Ok(())
}

#[test]
fn env_owned_deepseek_root_base_url_does_not_reach_the_deepseek_cn_sibling() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let _managed = no_managed_config(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    fs::write(&config_path, "provider = \"deepseek\"\n")?;
    let _base = EnvVarGuard::set("CODEWHALE_BASE_URL", "https://env-gateway.example.test/v1");

    let config = Config::load(Some(config_path), None)?;

    // The env override owns the route it was addressed to.
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Deepseek
    );
    assert_eq!(
        config.active_route_base_url(),
        "https://env-gateway.example.test/v1"
    );
    assert!(
        config
            .provider_uses_custom_endpoint(&config.test_identity_for_kind(ProviderKind::Deepseek))
    );

    // The sibling identity shares the same legacy root field but is a
    // different route: it must fall through to its own canonical endpoint.
    assert_eq!(
        config.base_url_for_route(
            &config
                .resolve_provider_selection_identity("deepseek-cn")
                .unwrap()
        ),
        DEFAULT_DEEPSEEKCN_BASE_URL
    );
    assert!(
        !config.provider_uses_custom_endpoint(
            &config
                .resolve_provider_selection_identity("deepseek-cn")
                .unwrap()
        )
    );
    assert!(
        !config.model_ids_pass_through_for_provider(
            &config
                .resolve_provider_selection_identity("deepseek-cn")
                .unwrap()
        )
    );
    Ok(())
}

#[test]
fn env_owned_deepseek_cn_root_base_url_does_not_reach_the_deepseek_sibling() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let _managed = no_managed_config(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    fs::write(&config_path, "provider = \"deepseek-cn\"\n")?;
    let _base = EnvVarGuard::set(
        "DEEPSEEK_BASE_URL",
        "https://cn-env-gateway.example.test/v1",
    );

    let config = Config::load(Some(config_path), None)?;

    assert_eq!(
        config.active_provider_identity().unwrap().key.as_str(),
        "deepseek-cn"
    );
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Deepseek
    );
    assert_eq!(
        config.active_route_base_url(),
        "https://cn-env-gateway.example.test/v1"
    );
    assert!(
        config.provider_uses_custom_endpoint(
            &config
                .resolve_provider_selection_identity("deepseek-cn")
                .unwrap()
        )
    );

    assert_eq!(
        config.base_url_for_route(
            &config
                .resolve_provider_selection_identity("deepseek")
                .unwrap()
        ),
        DEFAULT_DEEPSEEK_BASE_URL
    );
    assert!(
        !config.provider_uses_custom_endpoint(
            &config
                .resolve_provider_selection_identity("deepseek")
                .unwrap()
        )
    );
    Ok(())
}

#[test]
fn file_owned_legacy_root_base_url_stays_shared_by_both_deepseek_identities() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let _managed = no_managed_config(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    fs::write(
        &config_path,
        "provider = \"deepseek\"\nbase_url = \"https://file-gateway.example.test/v1\"\n",
    )?;

    let config = Config::load(Some(config_path), None)?;

    // No environment write, so the value is the user's own. Parsing moved it
    // to `[providers.deepseek]` (#6394), which DeepSeek-CN also reads, so
    // both identities keep the endpoint exactly as they always have.
    for key in ["deepseek", "deepseek-cn"] {
        let provider = config.resolve_provider_selection_identity(key).unwrap();
        assert_eq!(
            config.base_url_for_route(&provider),
            "https://file-gateway.example.test/v1",
            "{provider:?} must keep the file-owned legacy root endpoint"
        );
        assert!(config.provider_uses_custom_endpoint(&provider));
    }
    Ok(())
}

#[test]
fn managed_overlay_keeps_pinned_children_off_the_ambient_generic_host() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    let managed_path = temp_root.path().join("managed.toml");
    fs::write(&config_path, "provider = \"deepseek\"\n")?;
    fs::write(
        &managed_path,
        "provider = \"openrouter\"\n\n[providers.openrouter]\nbase_url = \"https://managed-gateway.example.test/v1\"\n",
    )?;
    let _managed = EnvVarGuard::set("DEEPSEEK_MANAGED_CONFIG_PATH", &managed_path);
    let _base = EnvVarGuard::set("CODEWHALE_BASE_URL", "https://env-gateway.example.test/v1");

    let config = Config::load(Some(config_path), None)?;

    // Managed routing is authoritative for the active route.
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Openrouter
    );
    assert_eq!(
        config.active_route_base_url(),
        "https://managed-gateway.example.test/v1"
    );

    // The receipt must say "nobody owns the generic override" rather than
    // being cleared: a cleared receipt reads as "never met the environment
    // layer" and re-enables the generic fallback for every pinned child.
    assert_eq!(config.base_url_env_receipt, BaseUrlEnvReceipt::NoOwner);
    for provider in [
        ProviderKind::Moonshot,
        ProviderKind::Zai,
        ProviderKind::Minimax,
        ProviderKind::Deepseek,
        ProviderKind::Deepseek,
    ] {
        assert_eq!(
            config.base_url_for_route(&config.test_identity_for_kind(provider)),
            provider.provider().default_base_url(),
            "{provider:?} must not borrow the ambient generic host under managed routing"
        );
        assert!(!config.provider_uses_custom_endpoint(&config.test_identity_for_kind(provider)));
    }
    Ok(())
}

#[test]
fn named_custom_children_resolve_by_identity_not_by_the_active_custom_route() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let _managed = no_managed_config(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    fs::write(
        &config_path,
        r#"provider = "acme"

[providers.acme]
kind = "openai-compatible"
base_url = "https://acme.example.test/v1"
model = "acme-1"

[providers.beta]
kind = "openai-compatible"
base_url = "https://beta.example.test/v1"
model = "beta-1"
"#,
    )?;

    let config = Config::load(Some(config_path), None)?;

    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Custom
    );
    assert_eq!(
        config.active_route_base_url(),
        "https://acme.example.test/v1"
    );
    // A pinned child of the other named custom table resolves its own host.
    assert_eq!(
        config.base_url_for_route(&config.resolve_provider_pin_identity("beta").unwrap()),
        "https://beta.example.test/v1"
    );
    assert!(config.custom_identity_is_resolvable("beta"));
    Ok(())
}

#[test]
fn missing_custom_identity_fails_closed_instead_of_reading_the_active_custom() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let _managed = no_managed_config(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    fs::write(
        &config_path,
        r#"provider = "acme"

[providers.acme]
kind = "openai-compatible"
base_url = "https://acme.example.test/v1"
model = "acme-1"
"#,
    )?;

    let config = Config::load(Some(config_path), None)?;
    for identity in ["ghost", "", "   ", "custom"] {
        assert!(
            config.resolve_provider_pin_identity(identity).is_err(),
            "{identity:?}"
        );
        assert!(!config.custom_identity_is_resolvable(identity));
    }
    Ok(())
}

#[test]
fn legacy_literal_custom_root_endpoint_belongs_only_to_the_literal_identity() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let _managed = no_managed_config(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    fs::write(
        &config_path,
        r#"provider = "custom"
base_url = "https://legacy-root.example.test/v1"
default_text_model = "legacy-1"
"#,
    )?;

    let config = Config::load(Some(config_path), None)?;

    assert!(config.selects_literal_custom_provider());
    assert_eq!(
        config.base_url_for_route(&config.active_provider_identity().unwrap()),
        "https://legacy-root.example.test/v1"
    );
    assert!(config.resolve_provider_pin_identity("acme").is_err());
    Ok(())
}

/// The bare `k3` id belongs to the Kimi Code coding-plan endpoint. A config
/// that selects it there must resolve, and must be labelled as the membership
/// product rather than the direct platform one (v0.9.1 kimi-k3 dogfood report).
#[test]
fn config_selects_bare_k3_on_the_kimi_code_route() {
    let config = Config {
        provider: Some("moonshot".to_string()),
        providers: Some(ProvidersConfig {
            moonshot: ProviderConfig {
                api_key: Some("k".to_string()),
                base_url: Some(DEFAULT_KIMI_CODE_BASE_URL.to_string()),
                model: Some(KIMI_CODE_K3_MODEL.to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        }),
        ..Config::default()
    };

    assert_eq!(config.default_model(), KIMI_CODE_K3_MODEL);
    assert_eq!(
        wire_model_for_provider_route(
            ProviderKind::Moonshot,
            &config.active_route_base_url(),
            &config.default_model(),
        ),
        KIMI_CODE_K3_MODEL
    );
    assert_eq!(
        moonshot_k3_route_display_name(&config.active_route_base_url(), &config.default_model()),
        Some("Kimi Code membership / k3")
    );
}

/// Neither K3 id may be silently served by the other product's endpoint. The
/// two are different plans with different context windows, so an unservable
/// pairing has to fail loudly and name both routes (v0.9.1 kimi-k3 dogfood report).
#[test]
fn k3_and_kimi_k3_never_cross_products_and_fail_visibly() {
    let crossed = validate_kimi_code_api_model_id(
        ProviderKind::Moonshot,
        DEFAULT_KIMI_CODE_BASE_URL,
        MOONSHOT_KIMI_K3_MODEL,
    )
    .expect_err("kimi-k3 is not a Kimi Code model id");
    assert!(crossed.contains("api.kimi.com/coding/v1"), "{crossed}");
    assert!(crossed.contains("api.moonshot.ai/v1"), "{crossed}");
    assert!(crossed.contains(KIMI_CODE_K3_MODEL), "{crossed}");

    let reversed = validate_kimi_code_api_model_id(
        ProviderKind::Moonshot,
        DEFAULT_MOONSHOT_BASE_URL,
        KIMI_CODE_K3_MODEL,
    )
    .expect_err("bare k3 is not a direct-platform model id");
    assert!(reversed.contains("api.moonshot.ai/v1"), "{reversed}");
    assert!(reversed.contains("api.kimi.com/coding/v1"), "{reversed}");
    assert!(reversed.contains(MOONSHOT_KIMI_K3_MODEL), "{reversed}");

    // The exact-route predicates stay disjoint.
    assert!(is_exact_direct_moonshot_k3_route(
        ProviderKind::Moonshot,
        DEFAULT_MOONSHOT_BASE_URL,
        MOONSHOT_KIMI_K3_MODEL
    ));
    assert!(!is_exact_kimi_code_k3_route(
        ProviderKind::Moonshot,
        DEFAULT_MOONSHOT_BASE_URL,
        MOONSHOT_KIMI_K3_MODEL
    ));
    assert!(is_exact_kimi_code_k3_route(
        ProviderKind::Moonshot,
        DEFAULT_KIMI_CODE_BASE_URL,
        KIMI_CODE_K3_MODEL
    ));
    assert!(is_exact_kimi_code_k3_route(
        ProviderKind::Moonshot,
        DEFAULT_KIMI_CODE_BASE_URL,
        KIMI_CODE_K3_256K_MODEL
    ));
    assert!(!is_exact_kimi_code_bare_k3_route(
        ProviderKind::Moonshot,
        DEFAULT_KIMI_CODE_BASE_URL,
        KIMI_CODE_K3_256K_MODEL
    ));
    assert!(!is_exact_direct_moonshot_k3_route(
        ProviderKind::Moonshot,
        DEFAULT_KIMI_CODE_BASE_URL,
        KIMI_CODE_K3_MODEL
    ));
}

#[test]
fn unknown_models_pass_through_on_canonical_moonshot_endpoints() {
    for base_url in [DEFAULT_KIMI_CODE_BASE_URL, DEFAULT_MOONSHOT_BASE_URL] {
        validate_kimi_code_api_model_id(ProviderKind::Moonshot, base_url, "future-kimi-model")
            .expect("unknown model IDs remain provider-owned");
    }
}

/// Preserving the user's spelling must not turn alias resolution
/// case-sensitive: normalize for comparison, never mutate what is stored or
/// sent. A first-party/catalog route still canonicalizes to its documented id.
#[test]
fn model_alias_matching_stays_case_insensitive() {
    // Mixed-case aliases still resolve to the provider's documented wire id.
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::NvidiaNim, "DeepSeek-V4-Pro").as_deref(),
        Some(DEFAULT_NVIDIA_NIM_MODEL)
    );
    assert_eq!(
        normalize_model_name_for_provider(ProviderKind::Openrouter, "DeepSeek-V4-Flash").as_deref(),
        Some(DEFAULT_OPENROUTER_FLASH_MODEL)
    );
    assert_eq!(
        canonical_model_id_for_provider(ProviderKind::Zai, "GLM-5.1").as_deref(),
        canonical_model_id_for_provider(ProviderKind::Zai, "glm-5.1").as_deref()
    );

    // A first-party DeepSeek route still migrates the retired aliases,
    // whatever case they are typed in.
    for alias in ["deepseek-chat", "DeepSeek-Chat", "DEEPSEEK-REASONER"] {
        assert_eq!(
            wire_model_for_provider_route(
                ProviderKind::Deepseek,
                "https://api.deepseek.com",
                alias
            ),
            DEEPSEEK_ALIAS_REPLACEMENT,
            "first-party DeepSeek must canonicalize {alias}"
        );
    }

    // The same alias on a custom endpoint keeps the user's exact spelling:
    // that endpoint owns both the id and its meaning.
    assert_eq!(
        wire_model_for_provider_route(
            ProviderKind::Deepseek,
            "https://llm.corp.internal/v1",
            "DeepSeek-Chat"
        ),
        "DeepSeek-Chat"
    );
}

/// The remembered `/model` pick outranks `config.toml` on the next launch. It
/// must not silently restyle the configured id: two spellings of one model are
/// the config file's call, a genuinely different model stays the memory's.
#[test]
fn remembered_model_pick_defers_to_the_configured_spelling() {
    assert_eq!(
        prefer_configured_model_spelling("DeepSeek-V4-Flash", "deepseek-v4-flash".to_string()),
        "DeepSeek-V4-Flash",
        "a case-only disagreement belongs to config.toml"
    );
    assert_eq!(
        prefer_configured_model_spelling("  DeepSeek-V4-Flash  ", "DEEPSEEK-V4-FLASH".to_string()),
        "DeepSeek-V4-Flash"
    );
    assert_eq!(
        prefer_configured_model_spelling("DeepSeek-V4-Flash", "deepseek-v4-pro".to_string()),
        "deepseek-v4-pro",
        "a different model is still a real remembered selection"
    );
    assert_eq!(
        prefer_configured_model_spelling("DeepSeek-V4-Flash", "auto".to_string()),
        "auto"
    );
    assert_eq!(
        prefer_configured_model_spelling("DeepSeek-V4-Flash", "DeepSeek-V4-Flash".to_string()),
        "DeepSeek-V4-Flash"
    );
}

/// #5956: `[compaction]` is absent by default, and absent must mean the
/// historical hard-coded 20 000-token verbatim retention budget with no
/// operator prompt suffix.
#[test]
fn compaction_tuning_defaults_to_todays_behavior() {
    let config = Config::default();
    assert_eq!(
        config.compaction_retained_user_message_tokens(),
        DEFAULT_COMPACTION_RETAINED_USER_MESSAGE_TOKENS
    );
    assert_eq!(config.compaction_retained_user_message_tokens(), 20_000);
    assert_eq!(config.compaction_summary_instructions(), None);
}

/// #5956: an explicit budget is honored inside the clamp and pulled back to
/// the nearest bound outside it, so neither a useless floor nor a
/// re-triggering ceiling can be configured.
#[test]
fn compaction_retained_user_message_tokens_clamps_to_the_documented_range() {
    let parse = |body: &str| toml::from_str::<Config>(body).expect("config parses");

    let in_range = parse("[compaction]\nretained_user_message_tokens = 60000\n");
    assert_eq!(in_range.compaction_retained_user_message_tokens(), 60_000);

    let too_small = parse("[compaction]\nretained_user_message_tokens = 1\n");
    assert_eq!(
        too_small.compaction_retained_user_message_tokens(),
        MIN_COMPACTION_RETAINED_USER_MESSAGE_TOKENS
    );

    let too_large = parse("[compaction]\nretained_user_message_tokens = 9000000\n");
    assert_eq!(
        too_large.compaction_retained_user_message_tokens(),
        MAX_COMPACTION_RETAINED_USER_MESSAGE_TOKENS
    );

    // The pre-#5956 issue text spelled the key `retained_user_message_max_tokens`;
    // it stays accepted as an alias so early adopters' files keep working.
    let alias = parse("[compaction]\nretained_user_message_max_tokens = 40000\n");
    assert_eq!(alias.compaction_retained_user_message_tokens(), 40_000);
}

/// #5956: a blank or whitespace-only key must read as unset, not as an empty
/// delimited section appended to every summarizer prompt.
#[test]
fn compaction_summary_instructions_trims_and_treats_blank_as_unset() {
    let configured: Config = toml::from_str(
        "[compaction]\nsummary_instructions = \"  Always list exact file:line.  \"\n",
    )
    .expect("config parses");
    assert_eq!(
        configured.compaction_summary_instructions().as_deref(),
        Some("Always list exact file:line.")
    );

    let blank: Config = toml::from_str("[compaction]\nsummary_instructions = \"   \\n \"\n")
        .expect("config parses");
    assert_eq!(blank.compaction_summary_instructions(), None);
}

/// The Codewhale route must attach its account key on every origin it is
/// allowed to reach, from every documented source.
///
/// Regression: `CODEWHALE_API_BASE` is the route's *own* documented override,
/// but it made the endpoint look "custom", which suppressed the saved and
/// exported key; the loopback arm below then returned an empty key and the
/// request went out with no `Authorization` header at all.
#[test]
fn codewhale_route_resolves_its_key_from_every_documented_source() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let home = temp_root.path().join("codewhale-home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");

    let route = |base: Option<&str>| Config {
        provider: Some("codewhale".to_string()),
        providers: Some(ProvidersConfig {
            codewhale: ProviderConfig {
                base_url: base.map(str::to_string),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        }),
        ..Config::default()
    };

    // Both the default origin and a loopback origin the operator declared in
    // CODEWHALE_API_BASE are the route's own authenticated endpoint.
    for declared in [None, Some("http://127.0.0.1:8901/v1")] {
        let _base = declared.map(|value| EnvVarGuard::set("CODEWHALE_API_BASE", value));
        {
            let _env = EnvVarGuard::set("CODEWHALE_API_KEY", "cwc_key_from_env");
            assert_eq!(
                route(None).active_route_api_key()?,
                "cwc_key_from_env",
                "env key, declared base {declared:?}"
            );
        }

        // The slot `codewhale account api-keys create --use` writes.
        let secrets = codewhale_secrets::Secrets::auto_detect();
        secrets.set("codewhale", "cwc_key_from_slot")?;
        assert_eq!(
            route(None).active_route_api_key()?,
            "cwc_key_from_slot",
            "secret store, declared base {declared:?}"
        );

        let mut configured = route(None);
        if let Some(providers) = configured.providers.as_mut() {
            providers.codewhale.api_key = Some("cwc_key_from_config".to_string());
        }
        assert_eq!(
            configured.active_route_api_key()?,
            "cwc_key_from_config",
            "config api_key, declared base {declared:?}"
        );

        let mut bound = route(None);
        if let Some(providers) = bound.providers.as_mut() {
            providers.codewhale.api_key_env = Some("MY_CODEWHALE_KEY".to_string());
        }
        let _bound_env = EnvVarGuard::set("MY_CODEWHALE_KEY", "cwc_key_from_api_key_env");
        assert_eq!(
            bound.active_route_api_key()?,
            "cwc_key_from_api_key_env",
            "api_key_env, declared base {declared:?}"
        );

        secrets.delete("codewhale")?;
    }
    Ok(())
}

/// Widening the official-endpoint family must not widen where the key goes.
///
/// A base URL the operator did not declare in `CODEWHALE_API_BASE` is still a
/// foreign host: the ambient account key must not follow it.
#[test]
fn codewhale_ambient_key_never_follows_an_undeclared_host() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let home = temp_root.path().join("codewhale-home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _env = EnvVarGuard::set("CODEWHALE_API_KEY", "cwc_key_must_not_travel");
    codewhale_secrets::Secrets::auto_detect().set("codewhale", "cwc_key_saved_must_not_travel")?;
    // The operator declared one origin; the config table names a different one.
    let _base = EnvVarGuard::set("CODEWHALE_API_BASE", "http://127.0.0.1:8901/v1");

    let config = Config {
        provider: Some("codewhale".to_string()),
        providers: Some(ProvidersConfig {
            codewhale: ProviderConfig {
                base_url: Some("https://not-declared.example/v1".to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        }),
        ..Config::default()
    };
    let error = config
        .active_route_api_key()
        .expect_err("an undeclared host must not inherit the account key");
    let text = error.to_string();
    assert!(!text.contains("cwc_key_must_not_travel"), "{text}");
    assert!(!text.contains("cwc_key_saved_must_not_travel"), "{text}");
    Ok(())
}

/// A missing Codewhale key must fail before any request, naming the exact fix.
///
/// Regression: the loopback branch treated this route as a keyless local
/// runtime and returned an empty key, so the request was dispatched with no
/// credential and the service answered 401 instead of the CLI answering first.
#[test]
fn codewhale_route_without_a_key_fails_before_any_request() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let home = temp_root.path().join("codewhale-home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _base = EnvVarGuard::set("CODEWHALE_API_BASE", "http://127.0.0.1:8901/v1");

    let config = Config {
        provider: Some("codewhale".to_string()),
        ..Config::default()
    };
    let error = config
        .active_route_api_key()
        .expect_err("a keyless Codewhale route must not dispatch");
    let text = error.to_string();
    assert!(text.contains("CODEWHALE_API_KEY"), "{text}");
    assert!(
        text.contains("codewhale account api-keys create --name <name> --scope models:infer --use"),
        "{text}"
    );
    assert!(
        text.contains("https://app.codewhale.net/settings?section=api"),
        "{text}"
    );
    // The only `cwc_key_` in the message is the placeholder in the export
    // example, never a resolved value.
    assert_eq!(text.matches("cwc_key_").count(), 1, "{text}");
    assert!(text.contains("cwc_key_..."), "{text}");
    Ok(())
}

#[test]
fn openrouter_vendor_profile_override_clear_and_provider_boundary() {
    let base: Config = toml::from_str(
        r#"
provider = "openrouter"
[providers.openrouter]
vendor = "deepinfra/turbo"
"#,
    )
    .unwrap();
    let unrelated: Config = toml::from_str(
        r#"
[providers.openrouter]
model = "deepseek/deepseek-v4-pro"
"#,
    )
    .unwrap();
    let retained = merge_config(base.clone(), unrelated);
    assert_eq!(
        retained.openrouter_vendor().unwrap().as_deref(),
        Some("deepinfra/turbo")
    );
    let clear: Config = toml::from_str("[providers.openrouter]\nvendor = \"\"\n").unwrap();
    assert_eq!(
        merge_config(base.clone(), clear)
            .openrouter_vendor()
            .unwrap(),
        None
    );
    let mut switched = base;
    switched.provider = Some("openai".into());
    assert_eq!(switched.openrouter_vendor().unwrap(), None);
    switched
        .provider_config_for_mut(&switched.test_identity_for_kind(ProviderKind::Openai))
        .unwrap()
        .vendor = Some("deepinfra".into());
    assert!(switched.openrouter_vendor().is_err());
}

#[test]
fn notifications_saved_profile_edit_roundtrips_the_actual_tui_loader() {
    let _guard = crate::test_support::lock_test_env();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    std::fs::write(&path, "[notifications]\nmethod = \"kitty\"\nsound = \"bell\"\n[profiles.work.notifications]\nquiet = true\nsound = \"off\"\n[profiles.inherited]\n").unwrap();
    let edit = NotificationConfigUpdate::Sound(Some(CompletionSound::Whale));
    edit.persist_for_profile(&path, Some("work")).unwrap();
    let work = Config::load(Some(path.clone()), Some("work"))
        .unwrap()
        .notifications_config();
    assert_eq!(work.sound, Some(CompletionSound::Whale));
    assert!(work.quiet);
    let root = Config::load(Some(path.clone()), None)
        .unwrap()
        .notifications_config();
    assert_eq!(root.sound, Some(CompletionSound::Bell));
    NotificationConfigUpdate::Quiet(true)
        .persist_for_profile(&path, Some("inherited"))
        .unwrap();
    let inherited = Config::load(Some(path.clone()), Some("inherited"))
        .unwrap()
        .notifications_config();
    assert!(inherited.quiet);
    assert_eq!(inherited.method, NotificationMethod::Kitty);
    let raw: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(raw["profiles"]["inherited"].get("notifications").is_none());
    let before = std::fs::read(&path).unwrap();
    assert!(edit.persist_for_profile(&path, Some("missing")).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
#[ignore = "requires CODEWHALE_TEST_NOTIFICATION_CLI pointing at the built CLI"]
fn notifications_real_cli_file_is_consumed_by_actual_tui_loader() {
    let _guard = crate::test_support::lock_test_env();
    let binary = std::env::var_os("CODEWHALE_TEST_NOTIFICATION_CLI").expect("supply built CLI");
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    for (key, value) in [
        ("sound", "whale"),
        ("quiet", "true"),
        ("events.approval-needed", "false"),
        ("condition", "always"),
        ("threshold_secs", "17"),
        ("event_sound.events", r#"["model-notify", "input-needed"]"#),
    ] {
        let result = std::process::Command::new(&binary)
            .env_clear()
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .env("CODEWHALE_HOME", temp.path().join("state"))
            .env("CODEWHALE_SECRET_BACKEND", "file")
            .current_dir(temp.path())
            .arg("--config")
            .arg(&path)
            .args(["config", "set", &format!("notifications.{key}"), value])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{key}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let loaded = Config::load(Some(path), None)
        .unwrap()
        .notifications_config();
    assert_eq!(loaded.sound, Some(CompletionSound::Whale));
    assert!(loaded.quiet);
    assert!(!loaded.events.approval_needed);
    assert_eq!(loaded.condition, Some(NotificationCondition::Always));
    assert_eq!(loaded.threshold_secs, 17);
    assert_eq!(loaded.event_sound.events, ["model-notify", "input-needed"]);
}

/// Parse a config layer the way a file is loaded, so legacy top-level keys
/// go through the #6394 canonicalizer.
fn legacy_config(body: &str) -> Config {
    crate::config::parse_config_base(body).expect("legacy config parses")
}

#[test]
fn official_chatgpt_api_on_legacy_root_belongs_to_chatgpt_in_both_crates() -> Result<()> {
    // The config crate always let Codex read an official Codex endpoint from
    // the top level; the TUI refused it. One rule now: it is Codex's (#6394).
    let body = "provider = \"openai-codex\"\nbase_url = \"https://api.openai.com/v1\"\n";
    let config = legacy_config(body);
    assert_eq!(
        config
            .provider_config_for(&config.test_identity_for_kind(ProviderKind::OpenaiCodex))
            .and_then(|entry| entry.base_url.as_deref()),
        Some("https://api.openai.com/v1")
    );
    let store = codewhale_config::parse_config_toml(body)?;
    assert_eq!(
        store.providers.openai_codex.base_url.as_deref(),
        Some("https://api.openai.com/v1")
    );
    assert!(store.providers.deepseek.base_url.is_none());
    Ok(())
}

// ── #6394: the TUI reads legacy top-level keys by the same rule ───────────

fn legacy_fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../config/tests/fixtures/legacy_root")
        .join(name);
    fs::read_to_string(&path).expect("legacy fixture")
}

fn migrated_fixture(body: &str) -> String {
    codewhale_config::legacy_root::migrated_document_text(body).unwrap_or_else(|| body.to_string())
}

#[test]
fn legacy_upgrade_fixtures_resolve_the_same_route_before_and_after_migrate() -> Result<()> {
    let _lock = lock_test_env();
    let _source = EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
    let _cli_key = EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
    for (fixture, profile, provider, base_url, key) in [
        (
            "v0_10_0_example.toml",
            None,
            ProviderKind::Deepseek,
            "https://api.deepseek.com/beta",
            Some("YOUR_DEEPSEEK_API_KEY"),
        ),
        (
            // The shipped profile's key finally reaches NIM.
            "v0_10_0_example.toml",
            Some("nvidia-nim"),
            ProviderKind::NvidiaNim,
            "https://integrate.api.nvidia.com/v1",
            Some("YOUR_NVIDIA_API_KEY"),
        ),
        (
            "v0_9_9_auth_set.toml",
            None,
            ProviderKind::Deepseek,
            DEFAULT_DEEPSEEK_BASE_URL,
            Some("sk-legacy-auth-set"),
        ),
        (
            "config_base_url_save.toml",
            None,
            ProviderKind::Deepseek,
            "https://proxy.example.test/v1",
            Some("sk-proxy-key"),
        ),
        (
            "literal_custom.toml",
            None,
            ProviderKind::Custom,
            "http://127.0.0.1:18181/v1",
            Some("sk-literal-custom"),
        ),
        (
            "url_guessed_nim.toml",
            None,
            ProviderKind::NvidiaNim,
            "https://integrate.api.nvidia.com/v1",
            None,
        ),
    ] {
        let body = legacy_fixture(fixture);
        for (stage, text) in [("before", body.clone()), ("after", migrated_fixture(&body))] {
            let config = Config::from_saved_document(&text, profile)?;
            let label = format!("{fixture} {profile:?} {stage}");
            assert_eq!(
                config.active_provider_identity().unwrap().provider,
                provider,
                "{label}"
            );
            assert_eq!(
                config.active_route_base_url().trim_end_matches('/'),
                base_url.trim_end_matches('/'),
                "{label}"
            );
            let configured_key = config.provider_route_string_with_deepseek_fallback(
                &config.test_identity_for_kind(provider),
                |entry| entry.api_key.clone(),
            );
            assert_eq!(configured_key.as_deref(), key, "{label}");
        }
    }
    Ok(())
}

#[test]
fn a_profile_endpoint_now_overrides_the_base_deepseek_table() -> Result<()> {
    // v0.10 let the base `[providers.deepseek] base_url` silently win over a
    // profile's top-level `base_url`; the profile's value now lands in the
    // profile's own table and overrides it (#6394).
    let body = r#"
[providers.deepseek]
base_url = "https://api.deepseek.com/beta"

[profiles.work]
base_url = "https://work.example.test/v1"
"#;
    let work = Config::from_saved_document(body, Some("work"))?;
    assert_eq!(work.active_route_base_url(), "https://work.example.test/v1");
    let base = Config::from_saved_document(body, None)?;
    assert_eq!(
        base.active_route_base_url(),
        "https://api.deepseek.com/beta"
    );
    Ok(())
}

#[test]
fn conflicting_top_level_key_wins_in_memory_like_before() -> Result<()> {
    let _lock = lock_test_env();
    let _source = EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
    let _cli_key = EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
    let config = Config::from_saved_document(
        r#"
api_key = "sk-top-level"
base_url = "https://top.example.test/v1"

[providers.deepseek]
api_key = "sk-table"
base_url = "https://table.example.test/v1"
"#,
        None,
    )?;
    // The key the TUI always sent, and the endpoint both crates always used.
    assert_eq!(config.deepseek_table_api_key(), Some("sk-top-level"));
    assert_eq!(
        config.active_route_base_url(),
        "https://table.example.test/v1"
    );
    assert_eq!(config.legacy_root.unresolved_conflicts().count(), 2);
    Ok(())
}

#[test]
fn deepseek_cn_shares_the_deepseek_endpoint_and_key_but_not_its_model() -> Result<()> {
    let config = Config::from_saved_document(
        r#"
provider = "deepseek-cn"

[providers.deepseek]
api_key = "sk-shared"
base_url = "https://gateway.example.test/v1"
model = "deepseek-only-model"
"#,
        None,
    )?;
    assert_eq!(
        config.active_provider_identity().unwrap().key.as_str(),
        "deepseek-cn"
    );
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Deepseek
    );
    assert_eq!(
        config.active_route_base_url(),
        "https://gateway.example.test/v1"
    );
    assert_eq!(
        config
            .provider_route_string_with_deepseek_fallback(
                &config.active_provider_identity().unwrap(),
                |entry| { entry.api_key.clone() }
            )
            .as_deref(),
        Some("sk-shared")
    );
    assert_eq!(
        config.provider_config_string_with_runtime_fallback(
            &config.active_provider_identity().unwrap(),
            |entry| { entry.model.clone() }
        ),
        None,
        "the model is not shared"
    );
    Ok(())
}

#[test]
fn vision_keeps_the_top_level_key_it_inherited_and_nothing_more() -> Result<()> {
    let legacy = Config::from_saved_document(
        "api_key = \"sk-root\"\n[vision_model]\nmodel = \"vision-x\"\nbase_url = \"https://vision.example.test/v1\"\n",
        None,
    )?;
    assert_eq!(
        legacy
            .vision_model_config()
            .and_then(|vision| vision.api_key)
            .as_deref(),
        Some("sk-root")
    );
    // A new-shape DeepSeek key is not handed to the vision host.
    let current = Config::from_saved_document(
        "[providers.deepseek]\napi_key = \"sk-deepseek\"\n[vision_model]\nmodel = \"vision-x\"\nbase_url = \"https://vision.example.test/v1\"\n",
        None,
    )?;
    assert_eq!(
        current
            .vision_model_config()
            .and_then(|vision| vision.api_key),
        None
    );
    Ok(())
}

#[test]
fn a_mimo_host_on_the_top_level_moves_to_mimo_and_a_deepseek_host_does_not() -> Result<()> {
    let mimo = Config::from_saved_document(
        "provider = \"xiaomi-mimo\"\nbase_url = \"https://api.xiaomimimo.com/v1\"\n",
        None,
    )?;
    assert_eq!(
        mimo.provider_config_for(&mimo.test_identity_for_kind(ProviderKind::XiaomiMimo))
            .and_then(|entry| entry.base_url.as_deref()),
        Some("https://api.xiaomimimo.com/v1")
    );
    let deepseek_host = Config::from_saved_document(
        "provider = \"xiaomi-mimo\"\nbase_url = \"https://api.deepseek.com\"\n",
        None,
    )?;
    assert_eq!(
        deepseek_host.active_route_base_url(),
        DEFAULT_XIAOMI_MIMO_BASE_URL
    );
    Ok(())
}

#[test]
fn no_parse_leaves_a_top_level_key_behind() -> Result<()> {
    let body = legacy_fixture("v0_10_0_example.toml");
    let parsed = parse_config_file(&body)?;
    assert!(!parsed.legacy_root.is_empty());
    let (text, _) = codewhale_config::legacy_root::canonicalize_text(&body)?;
    let table: toml::Table = toml::from_str(&text)?;
    assert!(!codewhale_config::legacy_root::has_legacy_root_keys(&table));
    Ok(())
}

#[test]
fn config_set_provider_typo_reuses_the_invalid_provider_wording() {
    let config = Config::default();
    let error = config
        .resolve_provider_selection_identity("deepsek")
        .expect_err("a typo is not a provider");
    assert!(
        error.starts_with("Invalid provider 'deepsek': expected deepseek"),
        "{error}"
    );
    assert!(!error.contains("saved session"), "{error}");
    // The resume path keeps its own saved-session wording.
    let resume = config
        .resolve_provider_pin_identity("deepsek")
        .expect_err("missing custom route");
    assert!(resume.starts_with("saved session requires"), "{resume}");
    assert_eq!(
        config
            .resolve_provider_selection_identity("deepseek")
            .expect("built-in provider")
            .provider,
        ProviderKind::Deepseek
    );
}

#[test]
fn deepseek_missing_key_message_keeps_commands_copyable_and_harness_advice_conditional() {
    let plain = deepseek_missing_key_message();
    assert!(
        plain.contains("\n  codewhale auth set --provider deepseek\n"),
        "{plain}"
    );
    assert!(
        plain.contains("If you already use DeepSeek Harness, grant read-only access:"),
        "{plain}"
    );
    assert!(
        plain.ends_with("\n  codewhale auth external-consent --provider deepseek --mode read-only"),
        "{plain}"
    );
}

#[test]
fn invalid_value_fixes_are_commands_that_work_and_name_overriding_layers() {
    let error = Config {
        tui: Some(TuiConfig {
            alternate_screen: Some("sometimes".to_string()),
            ..TuiConfig::default()
        }),
        ..Config::default()
    }
    .validate()
    .expect_err("unknown alternate_screen");
    let diagnostic = SafeConfigDiagnostic::find_in(&error).expect("safe diagnostic");
    // `config set` refuses dotted keys, so the fix edits the table instead.
    assert_eq!(
        diagnostic.fix(),
        Some(
            "set alternate_screen = \"auto\" in the [tui] table of config.toml (if a profile or managed config sets it, correct it there)"
        )
    );

    let error = Config {
        sandbox_mode: Some("bogus".to_string()),
        ..Config::default()
    }
    .validate()
    .expect_err("unknown sandbox_mode");
    let fix = SafeConfigDiagnostic::find_in(&error)
        .and_then(SafeConfigDiagnostic::fix)
        .expect("fix");
    assert!(
        fix.starts_with("codewhale config set sandbox_mode workspace-write (if CODEWHALE_SANDBOX_MODE, a profile, or managed config sets it"),
        "{fix}"
    );
}

#[test]
fn missing_profile_diagnostic_does_not_show_the_requested_name() {
    let mut profiles = HashMap::new();
    profiles.insert("work".to_string(), Config::default());
    let config = ConfigFile {
        base: Box::default(),
        profiles: Some(profiles),
        legacy_root: Default::default(),
    };
    let error = apply_profile(config, Some("sk-pasted-token")).expect_err("no such profile");
    let diagnostic = SafeConfigDiagnostic::find_in(&error).expect("safe diagnostic");
    let shown = diagnostic.display_message();
    assert_eq!(
        shown,
        "Profile not found (name not shown). Available profiles: work"
    );
    // The local error keeps the typed name for the person at the terminal.
    assert!(error.to_string().contains("sk-pasted-token"), "{error}");
}

/// #6700: `[tui].force_http1 = true` pins HTTP/1.1 without the env var.
/// (The env-only path is covered by `force_http1_scenario` under its lock.)
#[test]
fn tui_force_http1_key_pins_http1() {
    let config: Config = toml::from_str("[tui]\nforce_http1 = true\n").expect("config parses");
    assert!(config.force_http1());
}

#[test]
fn canonical_stream_config_preserves_legacy_resolution_and_precedence() {
    let _lock = lock_test_env();
    let _pin = EnvVarGuard::remove("CODEWHALE_FORCE_HTTP1");
    let _legacy_pin = EnvVarGuard::remove("DEEPSEEK_FORCE_HTTP1");
    let legacy = "[tui]\nstream_open_timeout_secs=80\nstream_chunk_timeout_secs=120\nforce_http1=true\nstream_max_resumes=4\nstream_max_transparent_retries=3\nstream_max_errors=7\nstream_max_duration_secs=900\nstream_max_content_mb=12\nconnect_timeout_secs=60\n";
    let canonical = "[stream]\nopen_timeout_secs=80\nchunk_timeout_secs=120\nforce_http1=true\nmax_resumes=4\nmax_transparent_retries=3\nmax_stream_errors=7\nmax_duration_secs=900\nmax_content_mb=12\nconnect_timeout_secs=60\n";
    let parse = |body: &str| toml::from_str::<Config>(body).unwrap();
    assert_eq!(
        toml::Value::try_from(parse(legacy).resolved_stream_settings()).unwrap(),
        toml::Value::try_from(parse(canonical).resolved_stream_settings()).unwrap(),
    );
    let mixed = parse(&format!(
        "{legacy}\n[stream]\nmax_resumes=0\nforce_http1=false\n"
    ));
    assert_eq!(mixed.stream_retry_limits().max_resumes, 0);
    assert_eq!(mixed.stream_retry_limits().max_errors, 7);
    assert_eq!(mixed.stream_open_timeout(), Duration::from_secs(80));
    assert!(!mixed.force_http1());
    let _pin = EnvVarGuard::set("CODEWHALE_FORCE_HTTP1", "1");
    assert!(
        mixed.force_http1(),
        "the existing environment pin still wins"
    );
}

#[test]
fn canonical_stream_config_env_clamps_and_keepalive_disable() {
    let _lock = lock_test_env();
    let _open = EnvVarGuard::set("CODEWHALE_STREAM_OPEN_TIMEOUT_SECS", "70");
    let _idle = EnvVarGuard::set("CODEWHALE_STREAM_IDLE_TIMEOUT_SECS", "90");
    let config: Config = toml::from_str("[stream]\nopen_timeout_secs=0\nchunk_timeout_secs=0\nmax_resumes=99\nmax_transparent_retries=99\nmax_stream_errors=0\nmax_duration_secs=999999\nmax_content_mb=999999\nconnect_timeout_secs=999999\ntcp_keepalive_secs=0\nhttp2_keep_alive_interval_secs=0\nhttp2_keep_alive_timeout_secs=0\n").unwrap();
    assert_eq!(config.stream_open_timeout(), Duration::from_secs(70));
    assert_eq!(
        config.stream_chunk_timeout_secs(),
        900,
        "zero retains idle default semantics"
    );
    assert_eq!(config.stream_retry_limits().max_resumes, 10);
    assert_eq!(config.stream_retry_limits().max_transparent_retries, 10);
    assert_eq!(config.stream_retry_limits().max_errors, 5);
    assert_eq!(config.stream_max_duration(), Duration::from_secs(86400));
    assert_eq!(config.stream_max_content_bytes(), 512 * 1024 * 1024);
    assert_eq!(config.connect_timeout(), Duration::from_secs(300));
    assert_eq!(config.tcp_keepalive(), None);
    assert_eq!(config.http2_keep_alive_interval(), None);
    assert_eq!(config.http2_keep_alive_timeout(), Duration::from_secs(20));
    let positive: Config = toml::from_str("[stream]\nopen_timeout_secs=12\nchunk_timeout_secs=14\ntcp_keepalive_secs=9000\nhttp2_keep_alive_interval_secs=9000\nhttp2_keep_alive_timeout_secs=9000\n").unwrap();
    assert_eq!(positive.stream_open_timeout(), Duration::from_secs(12));
    assert_eq!(positive.stream_chunk_timeout_secs(), 14);
    assert_eq!(positive.tcp_keepalive(), Some(Duration::from_secs(3600)));
    assert_eq!(
        positive.http2_keep_alive_interval(),
        Some(Duration::from_secs(3600))
    );
    assert_eq!(
        positive.http2_keep_alive_timeout(),
        Duration::from_secs(3600)
    );
    for invalid in [
        "open_timeout_secs=-1",
        "max_resumes=4294967296",
        "force_http1='yes'",
        "open_timout_secs=45",
    ] {
        assert!(
            toml::from_str::<Config>(&format!("[stream]\n{invalid}\n")).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn canonical_stream_config_profile_merges_independent_keys() {
    let config = parse_config_file("[stream]\nmax_resumes=7\ntcp_keepalive_secs=41\n[profiles.mobile.stream]\nopen_timeout_secs=180\n").unwrap();
    let resolved = apply_profile(config, Some("mobile")).unwrap();
    assert_eq!(resolved.stream_retry_limits().max_resumes, 7);
    assert_eq!(resolved.tcp_keepalive(), Some(Duration::from_secs(41)));
    assert_eq!(resolved.stream_open_timeout(), Duration::from_secs(180));
}

#[test]
fn migrated_root_custom_provenance_cannot_bless_replaced_added_or_profile_tables() {
    let root = Config::from_saved_document(
        r#"provider = "custom"
        base_url = "http://127.0.0.1:18180/v1"
        api_key = "fixture-key"
        default_text_model = "legacy-model"
    "#,
        None,
    )
    .unwrap();
    let captured = root.active_provider_identity().unwrap();
    for change in ["endpoint", "credential", "model", "replacement", "removed"] {
        let mut changed = root.clone();
        let tables = &mut changed.providers.as_mut().unwrap().custom;
        match change {
            "endpoint" => {
                tables.get_mut("custom").unwrap().base_url =
                    Some("http://127.0.0.1:19191/v1".into())
            }
            "credential" => {
                tables.get_mut("custom").unwrap().api_key = Some("replacement-key".into())
            }
            "model" => tables.get_mut("custom").unwrap().model = Some("replacement-model".into()),
            "replacement" => {
                tables.insert(
                    "custom".into(),
                    ProviderConfig {
                        kind: Some("openai-compatible".into()),
                        base_url: Some("http://127.0.0.1:19191/v1".into()),
                        ..ProviderConfig::default()
                    },
                );
            }
            "removed" => {
                tables.remove("custom");
            }
            _ => unreachable!(),
        }
        assert!(
            changed
                .resolve_persisted_provider_identity(Some("custom"), None)
                .is_err(),
            "{change}"
        );
        assert!(
            changed.verify_provider_identity(&captured).is_err(),
            "{change}"
        );
        assert!(changed.provider_config_for(&captured).is_none(), "{change}");
        assert_eq!(changed.base_url_for_route(&captured), "", "{change}");
    }
    let added = Config {
        provider: Some("custom".into()),
        legacy_root: root.legacy_root.clone(), // diagnostic notes are not authority.
        providers: root.providers.clone(),
        ..Config::default()
    };
    assert!(
        added
            .resolve_persisted_provider_identity(Some("custom"), None)
            .is_err()
    );
    let profile = Config::from_saved_document(
        r#"provider = "deepseek"
        [profiles.local]
        provider = "custom"
        base_url = "http://127.0.0.1:18180/v1"
        default_text_model = "profile-model"
    "#,
        Some("local"),
    )
    .unwrap();
    assert!(
        profile
            .resolve_persisted_provider_identity(Some("custom"), None)
            .is_err()
    );
    assert_eq!(
        profile
            .resolve_persisted_provider_identity(Some("custom"), Some("custom"))
            .unwrap()
            .persisted_id(),
        Some("custom")
    );
    let dropped = Config::from_saved_document(
        r#"provider = "custom"
        base_url = ""
        [providers.custom]
        kind = "openai-compatible"
        base_url = "http://127.0.0.1:18180/v1"
    "#,
        None,
    )
    .unwrap();
    assert!(
        dropped
            .resolve_persisted_provider_identity(Some("custom"), None)
            .is_err()
    );
}

#[test]
fn custom_id_case_is_not_migration_or_builtin_authority() {
    let config = Config::from_saved_document(
        r#"provider = "CustomA"
        [providers.CustomA]
        kind = "openai-compatible"
        base_url = "http://127.0.0.1:18180/v1"
        [providers.customa]
        kind = "openai-compatible"
        base_url = "http://127.0.0.1:19191/v1"
    "#,
        None,
    )
    .unwrap();
    assert!(
        config
            .resolve_persisted_provider_identity(Some("custom"), None)
            .is_err()
    );
    let a = config
        .resolve_persisted_provider_identity(Some("custom"), Some("CustomA"))
        .unwrap();
    let b = config
        .resolve_persisted_provider_identity(Some("custom"), Some("customa"))
        .unwrap();
    assert_ne!(a, b);
    assert_eq!(a.key.as_str(), "CustomA");
    assert_eq!(b.key.as_str(), "customa");
    assert_ne!(config.base_url_for_route(&a), config.base_url_for_route(&b));
}

#[test]
fn legacy_root_model_preparation_remints_only_after_verified_model_selection() {
    let mut config = Config::from_saved_document(
        r#"provider = "custom"
        base_url = "http://127.0.0.1:18180/v1"
        default_text_model = "original-model"
    "#,
        None,
    )
    .unwrap();
    let original = config.active_provider_identity().unwrap();
    config
        .set_provider_model_override(&original, Some("selected-model".into()))
        .unwrap();
    assert!(config.verify_provider_identity(&original).is_err());
    let prepared = config.active_provider_identity().unwrap();
    assert!(prepared.persisted_id().is_none());
    assert_ne!(prepared, original);
    assert_eq!(
        config
            .provider_config_for(&prepared)
            .unwrap()
            .model
            .as_deref(),
        Some("selected-model")
    );
    config
        .set_provider_base_url_override(&prepared, Some("http://127.0.0.1:19191/v1".into()))
        .unwrap();
    assert!(
        config
            .resolve_persisted_provider_identity(Some("custom"), None)
            .is_err()
    );
    assert_eq!(
        config.active_provider_identity().unwrap().persisted_id(),
        Some("custom")
    );
}
