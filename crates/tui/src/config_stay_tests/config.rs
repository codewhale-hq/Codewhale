use codewhale_config::provider_preserves_custom_base_url_model;

use std::collections::HashMap;

use std::fs;

use std::time::SystemTime;

use anyhow::Result;

use serde_json::json;

#[cfg(test)]
use codewhale_config::API_KEYRING_SENTINEL;

use codewhale_config::ConfigApiKeyValueKind;

use codewhale_config::classify_config_api_key_value;

use codewhale_config::ProviderKind;

use codewhale_config::catalog::reviewed::constants::*;

use codewhale_config::descriptors::defaults::*;

#[cfg(test)]
use codewhale_config::notifications::CompletionSound;

use codewhale_config::notifications::NotificationCondition;
use codewhale_runtime::config::*;

use crate::provider_readiness::has_api_key;

use crate::provider_readiness::has_api_key_for;

use crate::provider_lake::ProviderCapability;

use crate::provider_lake::RequestPayloadMode;

use crate::provider_lake::provider_capability;

use crate::test_support::EnvVarGuard;

use crate::test_support::lock_test_env;

use std::env;

use codewhale_runtime::config::test_support::*;
use std::time::UNIX_EPOCH;

#[test]
fn remembered_deepseek_cn_and_layered_root_models_keep_their_precedence() {
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
    ]
    .into_iter()
    .map(EnvVarGuard::remove)
    .collect();
    let path = home.path().join("config.toml");
    fs::write(
        &path,
        "provider = 'deepseek-cn'\ndefault_text_model = 'deepseek-v4-flash'\n",
    )
    .unwrap();
    fs::write(
        home.path().join("settings.toml"),
        "default_model = 'deepseek-v4-pro'\n",
    )
    .unwrap();
    assert_eq!(
        crate::plugins::providers::load_config(None, None)
            .unwrap()
            .default_model(),
        "deepseek-v4-pro"
    );
    codewhale_runtime::config_persistence::persist_root_bool_key(Some(&path), "allow_shell", false)
        .unwrap();
    assert_eq!(
        crate::plugins::providers::load_config(None, None)
            .unwrap()
            .default_model(),
        "deepseek-v4-pro"
    );

    fs::write(&path, "route_preferences_version = 1\nprovider = 'deepseek'\n[providers.deepseek]\nmodel = 'deepseek-v4-flash'\n[profiles.pro]\ndefault_text_model = 'deepseek-v4-pro'\n").unwrap();
    let profile = crate::plugins::providers::load_config(None, Some("pro")).unwrap();
    assert_eq!(profile.default_model(), "deepseek-v4-pro");
    assert_eq!(
        crate::route_runtime::resolve_runtime_route(&profile, ProviderKind::Deepseek, None)
            .unwrap()
            .model,
        "deepseek-v4-pro"
    );

    let managed = home.path().join("managed.toml");
    fs::write(&managed, "default_text_model = 'deepseek-v4-pro'\n").unwrap();
    let source = format!(
        "route_preferences_version = 1\nprovider = 'deepseek'\nmanaged_config_path = '{}'\n[providers.deepseek]\nmodel = 'deepseek-v4-flash'\n",
        managed.display()
    );
    fs::write(&path, source).unwrap();
    assert_eq!(
        crate::plugins::providers::load_config(None, None)
            .unwrap()
            .default_model(),
        "deepseek-v4-pro"
    );
}

#[test]
fn loaded_startup_selection_is_shared_and_does_not_rewrite_saved_files() {
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
    ]
    .into_iter()
    .map(EnvVarGuard::remove)
    .collect();
    let path = home.path().join("config.toml");
    let source = "provider = 'zai'\n[providers.zai]\nmodel = 'GLM-5.2'\n";
    let saved = "[provider_models]\nzai = 'GLM-5.3'\n";
    fs::write(&path, source).unwrap();
    fs::write(home.path().join("settings.toml"), saved).unwrap();
    let mut config = crate::plugins::providers::load_config(None, None).unwrap();
    assert_eq!(config.default_model(), "GLM-5.3");
    assert_eq!(
        crate::model_inventory::provider_default_model(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Zai)
        ),
        "GLM-5.3"
    );
    let route =
        crate::route_runtime::resolve_runtime_route(&config, ProviderKind::Zai, None).unwrap();
    assert_eq!(
        route.model, "GLM-5.3",
        "an omitted new-thread model uses the same selection"
    );
    let explicit =
        crate::route_runtime::resolve_runtime_route(&config, ProviderKind::Zai, Some("GLM-5.2"))
            .unwrap();
    assert_eq!(explicit.model, "GLM-5.2");
    let other = {
        let __cw_config_init_0_0 = Some(HashMap::from([("zai".into(), "GLM-5.1".into())]));
        let mut __cw_config_init_0_value: codewhale_runtime::settings::Settings =
            Default::default();
        __cw_config_init_0_value.provider_models = __cw_config_init_0_0;
        __cw_config_init_0_value
    };
    assert!(
        !config.apply_saved_selection(&other),
        "loaded route snapshots never reread preferences"
    );
    assert_eq!(config.default_model(), "GLM-5.3");
    assert_eq!(fs::read_to_string(path).unwrap(), source);
    assert_eq!(
        fs::read_to_string(home.path().join("settings.toml")).unwrap(),
        saved
    );
}

#[test]
fn startup_memory_yields_to_explicit_launch_scoped_config_and_profile() {
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
    ]
    .into_iter()
    .map(EnvVarGuard::remove)
    .collect();
    let source = "provider = 'zai'\n[providers.zai]\nmodel = 'GLM-5.2'\n[profiles.pinned.providers.zai]\nmodel = 'GLM-5.1'\n";
    fs::write(home.path().join("config.toml"), source).unwrap();
    fs::write(
        home.path().join("settings.toml"),
        "[provider_models]\nzai = 'GLM-5.3'\n",
    )
    .unwrap();
    {
        let _explicit = EnvVarGuard::set("CODEWHALE_MODEL", "GLM-5.2");
        let config = crate::plugins::providers::load_config(None, None).unwrap();
        assert_eq!(config.default_model(), "GLM-5.2");
        assert_eq!(
            crate::route_runtime::resolve_runtime_route(&config, ProviderKind::Zai, None)
                .unwrap()
                .model,
            "GLM-5.2"
        );
    }
    let profile = crate::plugins::providers::load_config(None, Some("pinned")).unwrap();
    assert_eq!(profile.default_model(), "GLM-5.1");
    assert!(!profile.remembered_selection_is_applicable());
    let project = home.path().join("project");
    fs::create_dir(&project).unwrap();
    let path = project.join("config.toml");
    fs::write(&path, source).unwrap();
    let config = crate::plugins::providers::load_config(Some(path), None).unwrap();
    assert_eq!(config.default_model(), "GLM-5.2");
    assert!(!config.remembered_selection_is_applicable());
}

#[test]
fn startup_memory_preserves_named_and_custom_endpoint_wire_ids() {
    let _lock = lock_test_env();
    let settings = {
        let __cw_config_init_1_0 = Some("TeamA".into());
        let __cw_config_init_1_1 = Some(HashMap::from([
            ("TeamA".into(), "memory-upper".into()),
            ("teama".into(), "memory-lower".into()),
            ("zai".into(), "GLM-5.3".into()),
        ]));
        let mut __cw_config_init_1_value: codewhale_runtime::settings::Settings =
            Default::default();
        __cw_config_init_1_value.default_provider = __cw_config_init_1_0;
        __cw_config_init_1_value.provider_models = __cw_config_init_1_1;
        __cw_config_init_1_value
    };
    let mut config: Config = toml::from_str("provider = 'teama'\n[providers.TeamA]\nkind = 'openai-compatible'\nbase_url = 'https://upper.example.test/v1'\nmodel = 'Wire-Upper'\n[providers.teama]\nkind = 'openai-compatible'\nbase_url = 'https://lower.example.test/v1'\nmodel = 'wire-lower'\n[providers.zai]\nbase_url = 'https://proxy.example.test/v1'\nmodel = 'Exact-Zai-ID'\n").unwrap();
    let _provider = EnvVarGuard::remove("CODEWHALE_PROVIDER");
    let _legacy_provider = EnvVarGuard::remove("DEEPSEEK_PROVIDER");
    assert!(config.apply_saved_selection(&settings));
    assert_eq!(config.provider.as_deref(), Some("TeamA"));
    let identity = config.active_provider_identity().unwrap();
    let route =
        crate::route_runtime::resolve_runtime_route_for_identity(&config, &identity, None).unwrap();
    assert_eq!(route.model, "memory-upper");
    assert_eq!(
        route.candidate.endpoint().base_url,
        "https://upper.example.test/v1"
    );
    let lower = config.resolve_provider_identity("teama").unwrap();
    assert_eq!(
        crate::route_runtime::resolve_runtime_route_for_identity(&config, &lower, None)
            .unwrap()
            .model,
        "memory-lower"
    );
    assert_eq!(
        config
            .provider_config_for(&config.test_identity_for_kind(ProviderKind::Zai))
            .unwrap()
            .model
            .as_deref(),
        Some("GLM-5.3")
    );
}

#[test]
fn named_custom_table_wire_responses_reaches_the_runtime_route() {
    let _lock = lock_test_env();
    let _provider = EnvVarGuard::remove("CODEWHALE_PROVIDER");
    let _legacy_provider = EnvVarGuard::remove("DEEPSEEK_PROVIDER");
    let config: Config = toml::from_str(
        "provider = 'relay'\n[providers.relay]\nkind = 'openai-compatible'\nbase_url = 'https://relay.example.test/v1'\nmodel = 'gpt-5.5'\nwire = 'responses'\n",
    )
    .unwrap();
    let identity = config.active_provider_identity().unwrap();
    let route = crate::route_runtime::resolve_runtime_route_for_identity(&config, &identity, None)
        .expect("custom table runtime route");
    assert_eq!(route.identity.provider, ProviderKind::Custom);
    assert_eq!(
        route.candidate.protocol(),
        codewhale_config::provider::WireFormat::Responses,
        "the table's wire dialect must reach the minted candidate instead of \
         Custom's static Chat policy"
    );
    assert_eq!(
        route.candidate.endpoint().base_url,
        "https://relay.example.test/v1"
    );

    // A table without a wire dialect keeps the backward-compatible static
    // Chat policy, and an unrecognized dialect degrades to it deliberately.
    let config: Config = toml::from_str(
        "provider = 'relay'\n[providers.relay]\nkind = 'openai-compatible'\nbase_url = 'https://relay.example.test/v1'\nmodel = 'gpt-5.5'\nwire = 'respones'\n",
    )
    .unwrap();
    let identity = config.active_provider_identity().unwrap();
    let route = crate::route_runtime::resolve_runtime_route_for_identity(&config, &identity, None)
        .expect("typo'd table still resolves");
    assert_eq!(
        route.candidate.protocol(),
        codewhale_config::provider::WireFormat::ChatCompletions,
        "an unrecognized dialect must degrade to the static policy"
    );
}

#[test]
fn provider_environment_model_outranks_startup_memory() {
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
    ]
    .into_iter()
    .map(EnvVarGuard::remove)
    .collect();
    fs::write(
        home.path().join("config.toml"),
        "provider = 'openai'\n[providers.openai]\nmodel = 'gpt-5.6-sol'\n",
    )
    .unwrap();
    fs::write(
        home.path().join("settings.toml"),
        "[provider_models]\nopenai = 'gpt-5.6-terra'\n",
    )
    .unwrap();
    let _model = EnvVarGuard::set("OPENAI_MODEL", "gpt-5.6-luna");
    let config = crate::plugins::providers::load_config(None, None).unwrap();
    assert_eq!(config.default_model(), "gpt-5.6-luna");
    assert_eq!(
        crate::route_runtime::resolve_runtime_route(&config, ProviderKind::Openai, None)
            .unwrap()
            .model,
        "gpt-5.6-luna"
    );
}

#[test]
fn retired_antigravity_credentials_are_never_read_and_no_client_is_built() {
    let _guard = lock_test_env();
    // The retired private credential plane: neither variable is consulted.
    let _api_key = EnvVarGuard::set("ANTIGRAVITY_API_KEY", "must-never-be-read");
    let _adc = EnvVarGuard::set("AGY_ADC_AUTH", "must-never-be-read");
    assert!(ProviderKind::Antigravity.provider().env_vars().is_empty());
    assert_eq!(
        ProviderKind::parse_config_identity("antigravity"),
        Some(ProviderKind::Antigravity)
    );
    for identity in ["antigravity", "agy"] {
        let config = {
            let __cw_config_init_2_0 = Some(identity.to_string());
            let mut __cw_config_init_2_value: Config = Config::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value
        };
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::Antigravity,
            "{identity}"
        );
    }
    let mut config = {
        let __cw_config_init_3_0 = Some("antigravity".to_string());
        let mut __cw_config_init_3_value: Config = Config::default();
        __cw_config_init_3_value.provider = __cw_config_init_3_0;
        __cw_config_init_3_value
    };
    let providers = config.providers.get_or_insert_with(Default::default);
    providers.antigravity.api_key = Some("legacy-literal-left-behind".to_string());

    let captured = config.active_provider_identity().unwrap();
    assert_eq!(
        crate::provider_readiness::credential_state_for_provider(&config, &captured),
        crate::provider_readiness::CredentialState::Legacy
    );
    assert!(
        crate::model_inventory::ModelInventory::from_config(&config)
            .unwrap()
            .candidates
            .iter()
            .all(|candidate| candidate.provider != ProviderKind::Antigravity)
    );

    // No transport can be constructed for the tombstone; the refusal happens
    // before any network or credential I/O.
    let error = crate::client::CodewhaleClient::new(&config)
        .err()
        .expect("constructing a client for the tombstone must fail")
        .to_string();
    assert!(error.contains("non-runnable"), "{error}");
    assert!(error.contains("GEMINI_API_KEY"), "{error}");
}

#[test]
fn auto_review_config_builds_runtime_policy() -> Result<()> {
    let config: Config = toml::from_str(
        r#"
[auto_review]
natural_language_guidance = "retired compatibility key"

[[auto_review.block]]
id = "block-shell"
action_kind = "shell"
reason = "shell requires maintainer review"

[[auto_review.allow]]
id = "allow-read-file"
tool = "read_file"
reason = "read_file is allowed"
"#,
    )?;
    config.validate()?;

    let policy = crate::core::authority::auto_review::AutoReviewPolicy::from_config(&config);
    let shell_context = crate::core::authority::auto_review::AutoReviewContext::from_tool_call(
        "exec_shell",
        &serde_json::json!({"command": "cargo test"}),
        crate::core::authority::auto_review::RunOrigin::Interactive,
        codewhale_execpolicy::ApprovalMode::Auto,
        true,
        None,
    );
    let shell_decision = policy.evaluate(&shell_context);
    assert_eq!(
        shell_decision.action,
        crate::core::authority::auto_review::AutoReviewAction::Block
    );
    assert_eq!(shell_decision.rule_id.as_deref(), Some("block-shell"));

    let read_context = crate::core::authority::auto_review::AutoReviewContext::from_tool_call(
        "read_file",
        &serde_json::json!({"path": "README.md"}),
        crate::core::authority::auto_review::RunOrigin::Interactive,
        codewhale_execpolicy::ApprovalMode::Auto,
        true,
        None,
    );
    let read_decision = policy.evaluate(&read_context);
    assert_eq!(
        read_decision.action,
        crate::core::authority::auto_review::AutoReviewAction::Allow
    );
    assert_eq!(read_decision.rule_id.as_deref(), Some("allow-read-file"));

    Ok(())
}

#[test]
fn auto_review_scenario() -> Result<()> {
    // Scenario consolidation of: auto_review_profile_overrides_base_policy, auto_review_text_contains_fails_closed_instead_of_broadening_a_rule, auto_review_legacy_allow_kind_fails_closed_instead_of_widening, auto_review_config_rejects_invalid_rule_shapes
    // from auto_review_profile_overrides_base_policy
    {
        let merged = Config::from_saved_document(
            r#"
    [[auto_review.block]]
    action_kind = "shell"

    [[profiles.strict.auto_review.block]]
    action_kind = "network"
    "#,
            Some("strict"),
        )?;
        let policy = crate::core::authority::auto_review::AutoReviewPolicy::from_config(&merged);

        assert_eq!(policy.block_rules.len(), 1);
        assert_eq!(
            policy.block_rules[0].action_kind,
            Some(crate::core::authority::auto_review::ToolActionKind::External)
        );
    }
    // from auto_review_text_contains_fails_closed_instead_of_broadening_a_rule
    {
        let error = toml::from_str::<Config>(
            r#"
    [[auto_review.allow]]
    tool = "exec_shell"
    text_contains = "run tests"
    "#,
        )
        .expect("shape parses")
        .validate()
        .expect_err("retired user-intent matcher must not disappear");

        assert!(
            error
                .to_string()
                .contains("user-intent matching was retired")
        );
    }
    // from auto_review_legacy_allow_kind_fails_closed_instead_of_widening
    {
        let error = toml::from_str::<Config>(
            r#"
    [[auto_review.allow]]
    action_kind = "git"
    "#,
        )
        .expect("shape parses")
        .validate()
        .expect_err("narrow legacy allow kind must not widen to external");

        assert!(error.to_string().contains("cannot safely widen"));
    }
    // from auto_review_config_rejects_invalid_rule_shapes
    {
        let invalid_kind: Config = toml::from_str(
            r#"
    [[auto_review.block]]
    action_kind = "teleport"
    "#,
        )
        .expect("parse config");
        let err = invalid_kind.validate().expect_err("invalid kind");
        assert!(
            err.to_string()
                .contains("Invalid auto_review.block[0].action_kind")
        );

        let global_allow: Config = toml::from_str(
            r#"
    [[auto_review.allow]]
    reason = "too broad"
    "#,
        )
        .expect("parse config");
        let err = global_allow.validate().expect_err("missing matcher");
        assert!(err.to_string().contains("set at least one of tool"));
    }
    Ok(())
}

#[test]
fn network_policy_toml_maps_proxy_hosts_to_runtime_policy() {
    let policy: codewhale_config::NetworkPolicyToml = toml::from_str(
        r#"
        default = "allow"
        proxy = ["github.com", ".githubusercontent.com"]
        proxy_fake_ip_cidrs = ["198.18.0.0/15"]
        "#,
    )
    .expect("network policy toml");

    let runtime = codewhale_net::network_policy::NetworkPolicy::from(policy);

    assert_eq!(runtime.proxy, ["github.com", ".githubusercontent.com"]);
    assert_eq!(runtime.proxy_fake_ip_cidrs, ["198.18.0.0/15"]);
    assert!(runtime.trusts_proxy_fakeip_host("github.com"));
    assert!(runtime.trusts_proxy_fakeip_host("raw.githubusercontent.com"));
}

#[test]
fn user_input_limits_default_when_tools_table_is_absent() {
    let parsed = Config::from_saved_document("", None).expect("empty config");
    let limits = crate::tools::user_input::UserInputLimits::from_config(&parsed);
    assert_eq!(limits.max_questions, 6);
    assert_eq!(limits.max_options, 4);
}

#[test]
fn user_input_limits_read_from_tools_table_and_clamp() {
    let parsed = Config::from_saved_document(
        r#"
        [tools]
        user_input_max_questions = 9
        user_input_max_options = 6
        "#,
        None,
    )
    .expect("tools config");
    let limits = crate::tools::user_input::UserInputLimits::from_config(&parsed);
    assert_eq!(limits.max_questions, 9);
    assert_eq!(limits.max_options, 6);

    let clamped = Config::from_saved_document(
        r#"
        [tools]
        user_input_max_questions = 50
        user_input_max_options = 0
        "#,
        None,
    )
    .expect("tools config");
    let clamped = crate::tools::user_input::UserInputLimits::from_config(&clamped);
    assert_eq!(clamped.max_questions, 10);
    assert_eq!(clamped.max_options, 2);
}

#[test]
fn goal_max_steps_default_and_zero_are_uncapped() {
    for source in ["", "[goal]", "[goal]\nmax_steps = 0\n"] {
        let parsed = Config::from_saved_document(source, None).expect("goal config");
        let resolved = parsed.goal_max_steps();
        assert_eq!(resolved, u32::MAX, "{source:?}");

        // Hosts pass this resolved value directly to the goal turn. It must
        // remain uncapped beyond the old 1,000-step ceiling and at saturation.
        let mut turn = crate::core::turn::TurnContext::with_budget_source(
            resolved,
            crate::core::turn::StepBudgetSource::Goal,
        );
        assert_eq!(turn.step_limit(), None);
        turn.step = 1_000;
        assert!(turn.next_step(), "{source:?}");
        assert!(!turn.at_max_steps(), "{source:?}");
        turn.step = u32::MAX;
        assert!(turn.next_step(), "{source:?}");
        assert!(!turn.at_max_steps(), "{source:?}");
    }
}

#[test]
fn goal_max_steps_preserves_positive_limits() {
    for (configured, expected) in [(1, 1), (50, 50), (100_000, 100_000), (500_000, 500_000)] {
        let source = format!("[goal]\nmax_steps = {configured}\n");
        let parsed = Config::from_saved_document(&source, None).expect("goal config");
        let resolved = parsed.goal_max_steps();
        assert_eq!(resolved, expected, "{source:?}");

        let mut turn = crate::core::turn::TurnContext::with_budget_source(
            resolved,
            crate::core::turn::StepBudgetSource::Goal,
        );
        assert_eq!(turn.step_limit(), Some(expected));
        turn.step = expected;
        assert!(turn.at_max_steps(), "{source:?}");
        assert!(!turn.next_step(), "{source:?}");
    }
}

#[test]
fn has_api_key_detects_in_memory_override_and_env_var() -> Result<()> {
    // Pins the v0.8.8 contract: `has_api_key` covers the prompt-free
    // sources used by `Config::active_route_api_key` (in-memory override,
    // env var, config-file slot).
    let _lock = lock_test_env();
    // Explicit in-memory key wins over every other source per
    // `Config::active_route_api_key`'s "Path 0" override.
    let cfg = <Config as Default>::default()
        .with_legacy_root(Some("sk-in-memory-override".to_string()), None);
    assert!(
        has_api_key(&cfg),
        "in-memory override must be detected as a usable key"
    );

    // Env var path.
    let env_cfg = Config::default();
    unsafe {
        std::env::set_var("DEEPSEEK_API_KEY", "env-key");
    }
    assert!(
        has_api_key(&env_cfg),
        "env-var key must be detected even with empty config"
    );
    unsafe {
        std::env::remove_var("DEEPSEEK_API_KEY");
    }
    Ok(())
}

#[test]
fn provider_neutral_scenario() -> Result<()> {
    // Scenario consolidation of: provider_neutral_cli_key_wins_after_profile_provider_switch, provider_neutral_cli_key_requires_dispatcher_source_marker
    // from provider_neutral_cli_key_wins_after_profile_provider_switch
    {
        let _lock = lock_test_env();
        let _source = EnvVarGuard::set(codewhale_config::CLI_API_KEY_SOURCE_ENV, "cli");
        let _cli_key = EnvVarGuard::set(codewhale_config::CLI_API_KEY_ENV, "explicit-profile-key");
        let _anthropic_env = EnvVarGuard::remove("ANTHROPIC_API_KEY");
        let mut providers = ProvidersConfig::default();
        providers.anthropic.api_key = Some("saved-anthropic-key".to_string());
        let config = {
            let __cw_config_init_5_0 = Some("anthropic".to_string());
            let __cw_config_init_5_1 = Some(providers);
            let mut __cw_config_init_5_value: Config = Default::default();
            __cw_config_init_5_value.provider = __cw_config_init_5_0;
            __cw_config_init_5_value.providers = __cw_config_init_5_1;
            __cw_config_init_5_value
        };

        assert_eq!(config.active_route_api_key()?, "explicit-profile-key");
        assert!(has_api_key(&config));
        assert!(active_provider_has_env_api_key(&config));
    }
    // from provider_neutral_cli_key_requires_dispatcher_source_marker
    {
        let _lock = lock_test_env();
        let _source = EnvVarGuard::remove(codewhale_config::CLI_API_KEY_SOURCE_ENV);
        let _legacy_source = EnvVarGuard::remove(codewhale_config::LEGACY_CLI_API_KEY_SOURCE_ENV);
        let _cli_key = EnvVarGuard::set(codewhale_config::CLI_API_KEY_ENV, "untrusted-generic-key");
        let _anthropic_env = EnvVarGuard::remove("ANTHROPIC_API_KEY");
        let mut providers = ProvidersConfig::default();
        providers.anthropic.api_key = Some("saved-anthropic-key".to_string());
        let config = {
            let __cw_config_init_6_0 = Some("anthropic".to_string());
            let __cw_config_init_6_1 = Some(providers);
            let mut __cw_config_init_6_value: Config = Default::default();
            __cw_config_init_6_value.provider = __cw_config_init_6_0;
            __cw_config_init_6_value.providers = __cw_config_init_6_1;
            __cw_config_init_6_value
        };

        assert_eq!(config.active_route_api_key()?, "saved-anthropic-key");
    }
    Ok(())
}

fn config_with_provider_scoped_key(provider: &str, api_key: &str) -> Config {
    let mut providers = ProvidersConfig::default();
    match provider {
        "deepseek" | "deepseek-cn" => {
            providers.deepseek.api_key = Some(api_key.to_string());
        }
        "nvidia-nim" => {
            providers.nvidia_nim.api_key = Some(api_key.to_string());
        }
        "openai" => {
            providers.openai.api_key = Some(api_key.to_string());
        }
        "wanjie-ark" => {
            providers.wanjie_ark.api_key = Some(api_key.to_string());
        }
        "openrouter" => {
            providers.openrouter.api_key = Some(api_key.to_string());
        }
        "novita" => {
            providers.novita.api_key = Some(api_key.to_string());
        }
        "fireworks" => {
            providers.fireworks.api_key = Some(api_key.to_string());
        }
        "siliconflow" => {
            providers.siliconflow.api_key = Some(api_key.to_string());
        }
        "sglang" => {
            providers.sglang.api_key = Some(api_key.to_string());
        }
        "vllm" => {
            providers.vllm.api_key = Some(api_key.to_string());
        }
        "ollama" => {
            providers.ollama.api_key = Some(api_key.to_string());
        }
        "huggingface" => {
            providers.huggingface.api_key = Some(api_key.to_string());
        }
        "qianfan" => {
            providers.qianfan.api_key = Some(api_key.to_string());
        }
        _ => panic!("unexpected provider {provider}"),
    }

    {
        let __cw_config_init_7_0 = Some(provider.to_string());
        let __cw_config_init_7_1 = Some(providers);
        let mut __cw_config_init_7_value: Config = Config::default();
        __cw_config_init_7_value.provider = __cw_config_init_7_0;
        __cw_config_init_7_value.providers = __cw_config_init_7_1;
        __cw_config_init_7_value
    }
}

#[test]
fn has_api_scenario() -> Result<()> {
    // Scenario consolidation of: has_api_key_uses_active_provider_scoped_config_key, has_api_key_uses_root_config_key_for_deepseek_variants, has_api_key_for_uses_deepseek_cn_provider_table, has_api_key_for_uses_root_config_key_for_deepseek_variants
    // from has_api_key_uses_active_provider_scoped_config_key
    {
        // `has_api_key` intentionally consults live endpoint env overrides. Keep
        // this config-only assertion out of the windows where another test owns a
        // process-global custom endpoint.
        let _lock = lock_test_env();
        for provider in [
            "openai",
            "wanjie-ark",
            "openrouter",
            "novita",
            "fireworks",
            "siliconflow",
            "qianfan",
        ] {
            let config = config_with_provider_scoped_key(provider, "provider-config-key");

            assert!(
                has_api_key(&config),
                "active provider config key must satisfy onboarding auth check for {provider}"
            );
        }
    }
    // from has_api_key_uses_root_config_key_for_deepseek_variants
    {
        // A concurrent CODEWHALE_BASE_URL override deliberately unbinds the saved
        // root key from the active endpoint. Serialize this assertion with the
        // tests that install those process-global overrides.
        let _lock = lock_test_env();
        for provider in ["deepseek", "deepseek-cn"] {
            let config = {
                let __cw_config_init_8_0 = Some(provider.to_string());
                let mut __cw_config_init_8_value: Config = Config::default();
                __cw_config_init_8_value.provider = __cw_config_init_8_0;
                __cw_config_init_8_value
            }
            .with_legacy_root(Some("root-config-key".to_string()), None);

            assert!(
                has_api_key(&config),
                "root config api_key must satisfy onboarding auth check for {provider}"
            );
        }
    }
    // from has_api_key_for_uses_deepseek_cn_provider_table
    {
        let _lock = lock_test_env();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_root = env::temp_dir().join(format!(
            "codewhale-tui-has-key-cn-{}-{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&temp_root)?;
        let _guard = EnvGuard::new(&temp_root);

        let mut providers = ProvidersConfig::default();
        providers.deepseek_cn.api_key = Some("cn-file-key".to_string());
        let config = {
            let __cw_config_init_9_0 = Some(providers);
            let mut __cw_config_init_9_value: Config = Config::default();
            __cw_config_init_9_value.providers = __cw_config_init_9_0;
            __cw_config_init_9_value
        };

        assert!(has_api_key_for(
            &config,
            &(config)
                .resolve_provider_selection_identity("deepseek-cn")
                .unwrap()
        ));
    }
    // from has_api_key_for_uses_root_config_key_for_deepseek_variants
    {
        let _lock = lock_test_env();
        let config = Config::default().with_legacy_root(Some("root-config-key".to_string()), None);

        assert!(has_api_key_for(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Deepseek)
        ));
        assert!(has_api_key_for(
            &config,
            &(config)
                .resolve_provider_selection_identity("deepseek-cn")
                .unwrap()
        ));
    }
    Ok(())
}

#[test]
fn has_api_key_uses_active_provider_env_key() -> Result<()> {
    let _lock = lock_test_env();
    for (provider, env_var) in [
        ("openai", "OPENAI_API_KEY"),
        ("wanjie-ark", "WANJIE_ARK_API_KEY"),
        ("openrouter", "OPENROUTER_API_KEY"),
        ("novita", "NOVITA_API_KEY"),
        ("fireworks", "FIREWORKS_API_KEY"),
        ("siliconflow", "SILICONFLOW_API_KEY"),
        ("qianfan", "QIANFAN_API_KEY"),
    ] {
        unsafe {
            std::env::set_var(env_var, "provider-env-key");
        }

        let config = {
            let __cw_config_init_11_0 = Some(provider.to_string());
            let mut __cw_config_init_11_value: Config = Config::default();
            __cw_config_init_11_value.provider = __cw_config_init_11_0;
            __cw_config_init_11_value
        };

        assert!(
            has_api_key(&config),
            "active provider env key must satisfy onboarding auth check for {provider}"
        );

        unsafe {
            std::env::remove_var(env_var);
        }
    }
    Ok(())
}

#[test]
fn standalone_tui_reads_saved_secret_before_ambient_env() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let codewhale_home = temp_root.path().join("isolated-codewhale");
    fs::create_dir_all(&codewhale_home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _ambient_key = EnvVarGuard::set("DEEPSEEK_API_KEY", "stale-env-key");

    let secrets = codewhale_secrets::Secrets::auto_detect();
    secrets.set("deepseek", "saved-secret-key")?;

    let config = Config::default();
    assert_eq!(config.active_route_api_key()?, "saved-secret-key");
    assert!(has_api_key(&config));
    assert!(active_provider_has_config_api_key(&config));

    let configured = Config::default().with_legacy_root(Some("fresh-config-key".to_string()), None);
    assert_eq!(configured.active_route_api_key()?, "fresh-config-key");
    Ok(())
}

#[test]
fn authenticated_local_provider_reads_saved_secret() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let codewhale_home = temp_root.path().join("isolated-codewhale");
    fs::create_dir_all(&codewhale_home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _ambient_key = EnvVarGuard::remove("VLLM_API_KEY");

    codewhale_secrets::Secrets::auto_detect().set("vllm", "saved-local-secret")?;

    let mut providers = ProvidersConfig::default();
    providers.vllm.base_url = Some("http://127.0.0.1:8000/v1".to_string());
    providers.vllm.auth_mode = Some("api_key".to_string());
    let config = {
        let __cw_config_init_13_0 = Some("vllm".to_string());
        let __cw_config_init_13_1 = Some(providers);
        let mut __cw_config_init_13_value: Config = Config::default();
        __cw_config_init_13_value.provider = __cw_config_init_13_0;
        __cw_config_init_13_value.providers = __cw_config_init_13_1;
        __cw_config_init_13_value
    };

    assert_eq!(config.active_route_api_key()?, "saved-local-secret");
    assert!(has_api_key(&config));
    assert!(active_provider_has_config_api_key(&config));
    Ok(())
}

#[test]
fn named_custom_provider_never_reuses_generic_custom_secret() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let codewhale_home = temp_root.path().join("isolated-codewhale");
    fs::create_dir_all(&codewhale_home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");

    codewhale_secrets::Secrets::auto_detect().set("custom", "endpoint-a-secret")?;

    let mut providers = ProvidersConfig::default();
    providers.custom.insert(
        "endpoint-b".to_string(),
        ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("https://endpoint-b.example.test/v1".to_string()),
            model: Some("endpoint-b-model".to_string()),
            auth_mode: Some("api_key".to_string()),
            ..ProviderConfig::default()
        },
    );
    let config = {
        let __cw_config_init_14_0 = Some("endpoint-b".to_string());
        let __cw_config_init_14_1 = Some(providers);
        let mut __cw_config_init_14_value: Config = Config::default();
        __cw_config_init_14_value.provider = __cw_config_init_14_0;
        __cw_config_init_14_value.providers = __cw_config_init_14_1;
        __cw_config_init_14_value
    };

    assert!(config.should_skip_secret_store_for_provider(
        &config.test_identity_for_kind(ProviderKind::Custom)
    ));
    assert!(
        provider_secret_store_api_key(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Custom)
        )
        .is_none()
    );
    assert!(config.active_route_api_key().is_err());
    assert!(!has_api_key(&config));
    assert!(!active_provider_has_config_api_key(&config));
    Ok(())
}

#[test]
fn built_in_provider_custom_endpoint_never_reuses_global_credentials() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let codewhale_home = temp_root.path().join("isolated-codewhale");
    fs::create_dir_all(&codewhale_home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _ambient = EnvVarGuard::set("OPENROUTER_API_KEY", "ambient-official-key");
    codewhale_secrets::Secrets::auto_detect().set("openrouter", "saved-official-key")?;

    let mut providers = ProvidersConfig::default();
    providers.openrouter.base_url = Some("https://gateway.example.test/v1".to_string());
    let config = {
        let __cw_config_init_15_0 = Some("openrouter".to_string());
        let __cw_config_init_15_1 = Some(providers);
        let mut __cw_config_init_15_value: Config = Config::default();
        __cw_config_init_15_value.provider = __cw_config_init_15_0;
        __cw_config_init_15_value.providers = __cw_config_init_15_1;
        __cw_config_init_15_value
    };

    assert!(
        config.provider_uses_custom_endpoint(
            &config.test_identity_for_kind(ProviderKind::Openrouter)
        )
    );
    assert!(config.should_skip_secret_store_for_provider(
        &config.test_identity_for_kind(ProviderKind::Openrouter)
    ));
    assert!(config.active_route_api_key().is_err());
    assert!(!has_api_key(&config));
    assert!(!active_provider_has_config_api_key(&config));
    assert!(!active_provider_has_env_api_key(&config));
    Ok(())
}

#[test]
fn custom_endpoint_accepts_route_bound_api_key_env_and_reports_ready() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let _route_key = EnvVarGuard::set("MY_GATEWAY_ROUTE_KEY", "route-bound-key");
    let _ambient = EnvVarGuard::set("OPENROUTER_API_KEY", "ambient-official-key");

    let mut providers = ProvidersConfig::default();
    providers.openrouter.base_url = Some("https://gateway.example.test/v1".to_string());
    providers.openrouter.api_key_env = Some("MY_GATEWAY_ROUTE_KEY".to_string());
    let config = {
        let __cw_config_init_16_0 = Some("openrouter".to_string());
        let __cw_config_init_16_1 = Some(providers);
        let mut __cw_config_init_16_value: Config = Config::default();
        __cw_config_init_16_value.provider = __cw_config_init_16_0;
        __cw_config_init_16_value.providers = __cw_config_init_16_1;
        __cw_config_init_16_value
    };

    assert_eq!(config.active_route_api_key()?, "route-bound-key");
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Openrouter)
    ));
    assert!(active_provider_has_env_api_key(&config));
    assert!(active_provider_uses_env_only_api_key(&config));
    Ok(())
}

#[test]
fn env_selected_custom_endpoints_do_not_rebind_file_credentials() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    let _route_key = EnvVarGuard::set("MY_GATEWAY_ROUTE_KEY", "route-bound-file-key");
    let _source = EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
    let _cli_key = EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");

    fs::write(
        &config_path,
        r#"api_key = "saved-root-deepseek-key"
default_text_model = "deepseek-chat"
"#,
    )?;
    {
        let _generic_base = EnvVarGuard::set(
            "CODEWHALE_BASE_URL",
            "https://generic-gateway.example.test/v1",
        );
        let config = crate::plugins::providers::load_config(Some(config_path.clone()), None)?;
        assert!(
            config.provider_uses_custom_endpoint(
                &config.test_identity_for_kind(ProviderKind::Deepseek)
            )
        );
        assert!(config.active_route_api_key().is_err());
        assert!(!active_provider_has_config_api_key(&config));
        assert!(!active_provider_has_env_api_key(&config));
        assert!(!has_api_key_for(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Deepseek)
        ));
    }

    fs::write(
        &config_path,
        r#"provider = "openrouter"

[providers.openrouter]
api_key = "saved-openrouter-route-key"
api_key_env = "MY_GATEWAY_ROUTE_KEY"
model = "openai/gpt-5"
"#,
    )?;
    for (env_name, endpoint) in [
        (
            "CODEWHALE_BASE_URL",
            "https://generic-openrouter-gateway.example.test/v1",
        ),
        (
            "OPENROUTER_BASE_URL",
            "https://provider-openrouter-gateway.example.test/v1",
        ),
    ] {
        let _base = EnvVarGuard::set(env_name, endpoint);
        let config = crate::plugins::providers::load_config(Some(config_path.clone()), None)?;
        assert_eq!(config.active_route_base_url(), endpoint);
        assert!(config.provider_uses_custom_endpoint(
            &config.test_identity_for_kind(ProviderKind::Openrouter)
        ));
        assert!(config.active_route_api_key().is_err());
        assert!(!active_provider_has_config_api_key(&config));
        assert!(!active_provider_has_env_api_key(&config));
        assert!(!has_api_key_for(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openrouter)
        ));
    }

    Ok(())
}

#[test]
fn file_bound_custom_endpoints_keep_route_credentials() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    let _route_key = EnvVarGuard::set("MY_GATEWAY_ROUTE_KEY", "file-env-route-key");

    fs::write(
        &config_path,
        r#"api_key = "file-root-key"
base_url = "https://file-deepseek-gateway.example.test/v1"
default_text_model = "private-deepseek-model"
"#,
    )?;
    let root = crate::plugins::providers::load_config(Some(config_path.clone()), None)?;
    assert_eq!(root.active_route_api_key()?, "file-root-key");
    assert!(active_provider_has_config_api_key(&root));
    assert!(has_api_key_for(
        &root,
        &(root).test_identity_for_kind(ProviderKind::Deepseek)
    ));

    fs::write(
        &config_path,
        r#"provider = "openrouter"

[providers.openrouter]
api_key = "file-provider-key"
base_url = "https://file-openrouter-gateway.example.test/v1"
model = "private-openrouter-model"
"#,
    )?;
    let provider_key = crate::plugins::providers::load_config(Some(config_path.clone()), None)?;
    assert_eq!(provider_key.active_route_api_key()?, "file-provider-key");
    assert!(active_provider_has_config_api_key(&provider_key));
    assert!(has_api_key_for(
        &provider_key,
        &(provider_key).test_identity_for_kind(ProviderKind::Openrouter)
    ));

    fs::write(
        &config_path,
        r#"provider = "openrouter"

[providers.openrouter]
api_key_env = "MY_GATEWAY_ROUTE_KEY"
base_url = "https://file-openrouter-gateway.example.test/v1"
model = "private-openrouter-model"
"#,
    )?;
    let route_env = crate::plugins::providers::load_config(Some(config_path), None)?;
    assert_eq!(route_env.active_route_api_key()?, "file-env-route-key");
    assert!(active_provider_has_env_api_key(&route_env));
    assert!(has_api_key_for(
        &route_env,
        &(route_env).test_identity_for_kind(ProviderKind::Openrouter)
    ));
    Ok(())
}

/// A session-scoped `[providers.*]` fixture with credentials but no endpoints,
/// so every base URL in these tests comes from the resolver rather than a file.
const CROSS_PROVIDER_ROUTE_FIXTURE: &str = r#"api_key = "session-deepseek-key"
default_text_model = "deepseek-chat"

[providers.moonshot]
api_key = "moonshot-route-key"

[providers.zai]
api_key = "zai-route-key"

[providers.minimax]
api_key = "minimax-route-key"
"#;

#[test]
fn generic_base_url_override_never_reaches_pinned_child_routes() -> Result<()> {
    // #4093-class routing truth: every cross-provider seam (pinned subagent /
    // fleet child, per-turn auto-router, tool routing, picker preview) clones
    // the session config and re-points `provider`. The generic endpoint
    // override belongs to the DeepSeek session that set it and must never
    // follow a child to another vendor's route.
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    fs::write(&config_path, CROSS_PROVIDER_ROUTE_FIXTURE)?;

    for env_name in ["CODEWHALE_BASE_URL", "DEEPSEEK_BASE_URL"] {
        let session_host = "https://session-gateway.example.test/v1";
        let _base = EnvVarGuard::set(env_name, session_host);
        let config = crate::plugins::providers::load_config(Some(config_path.clone()), None)?;

        // Documented behavior for the active DeepSeek route is unchanged.
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::Deepseek
        );
        assert_eq!(config.active_route_base_url(), session_host);
        assert!(
            config.provider_uses_custom_endpoint(
                &config.test_identity_for_kind(ProviderKind::Deepseek)
            )
        );

        for (provider, expected) in [
            (ProviderKind::Moonshot, DEFAULT_MOONSHOT_BASE_URL),
            (ProviderKind::Zai, DEFAULT_ZAI_BASE_URL),
            (ProviderKind::Minimax, DEFAULT_MINIMAX_BASE_URL),
        ] {
            assert_eq!(
                config.base_url_for_route(&config.test_identity_for_kind(provider)),
                expected,
                "{env_name}: {provider:?} must resolve from its own identity table"
            );
            assert!(
                !config.provider_uses_custom_endpoint(&config.test_identity_for_kind(provider)),
                "{env_name}: {provider:?} is on its canonical host, not a custom one"
            );

            let route = crate::route_runtime::resolve_runtime_route(&config, provider, None)
                .unwrap_or_else(|err| panic!("{env_name}: {provider:?} child route: {err}"));
            // The scoped config and the executable candidate must agree, and
            // neither may name the session host.
            assert_eq!(route.config.active_route_base_url(), expected);
            assert_eq!(route.candidate.endpoint().base_url, expected);
            assert_ne!(route.candidate.endpoint().base_url, session_host);
        }

        assert!(
            config
                .resolve_provider_selection_identity("custom")
                .is_err()
        );
    }

    Ok(())
}

#[test]
fn provider_scoped_base_url_env_applies_only_to_its_own_route() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    fs::write(&config_path, CROSS_PROVIDER_ROUTE_FIXTURE)?;

    let moonshot_host = "https://moonshot-gateway.example.test/v1";
    let _moonshot = EnvVarGuard::set("MOONSHOT_BASE_URL", moonshot_host);

    // Without a generic override the active DeepSeek route keeps its default:
    // a provider-scoped variable names exactly one provider.
    let config = crate::plugins::providers::load_config(Some(config_path.clone()), None)?;
    assert_eq!(config.active_route_base_url(), DEFAULT_DEEPSEEK_BASE_URL);
    assert_eq!(
        config.base_url_for_route(&config.test_identity_for_kind(ProviderKind::Zai)),
        DEFAULT_ZAI_BASE_URL
    );
    assert_eq!(
        config.base_url_for_route(&config.test_identity_for_kind(ProviderKind::Moonshot)),
        moonshot_host
    );
    let route = crate::route_runtime::resolve_runtime_route(&config, ProviderKind::Moonshot, None)
        .expect("Moonshot child route");
    assert_eq!(route.candidate.endpoint().base_url, moonshot_host);

    // With both set, each override stays on its own route.
    let session_host = "https://session-gateway.example.test/v1";
    let _base = EnvVarGuard::set("CODEWHALE_BASE_URL", session_host);
    let config = crate::plugins::providers::load_config(Some(config_path), None)?;
    assert_eq!(config.active_route_base_url(), session_host);
    assert_eq!(
        config.base_url_for_route(&config.test_identity_for_kind(ProviderKind::Moonshot)),
        moonshot_host
    );
    assert_eq!(
        config.base_url_for_route(&config.test_identity_for_kind(ProviderKind::Zai)),
        DEFAULT_ZAI_BASE_URL
    );
    let route = crate::route_runtime::resolve_runtime_route(&config, ProviderKind::Moonshot, None)
        .expect("Moonshot child route");
    assert_eq!(route.config.active_route_base_url(), moonshot_host);
    assert_eq!(route.candidate.endpoint().base_url, moonshot_host);
    assert_ne!(route.candidate.endpoint().base_url, session_host);

    Ok(())
}

#[test]
fn source_marked_cli_key_can_follow_cli_forwarded_custom_base_url() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    fs::write(
        &config_path,
        r#"api_key = "saved-root-key"
base_url = "https://api.deepseek.com/v1"
default_text_model = "deepseek-chat"
"#,
    )?;
    let _base = EnvVarGuard::set(
        "DEEPSEEK_BASE_URL",
        "https://explicit-cli-gateway.example.test/v1",
    );
    let _source = EnvVarGuard::set(codewhale_config::CLI_API_KEY_SOURCE_ENV, "cli");
    let _cli_key = EnvVarGuard::set(codewhale_config::CLI_API_KEY_ENV, "explicit-cli-key");

    let config = crate::plugins::providers::load_config(Some(config_path), None)?;
    assert_eq!(config.active_route_api_key()?, "explicit-cli-key");
    assert!(!active_provider_has_config_api_key(&config));
    assert!(active_provider_has_env_api_key(&config));
    assert!(active_provider_uses_env_only_api_key(&config));
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Deepseek)
    ));
    Ok(())
}

#[test]
fn managed_file_endpoint_replaces_lower_env_provenance() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    let managed_path = temp_root.path().join("managed.toml");
    fs::write(
        &managed_path,
        r#"[providers.openrouter]
api_key = "managed-route-key"
base_url = "https://managed-gateway.example.test/v1"
model = "managed-model"
"#,
    )?;
    fs::write(
        &config_path,
        format!(
            "provider = \"openrouter\"\nmanaged_config_path = {:?}\n",
            managed_path.display().to_string()
        ),
    )?;
    let _base = EnvVarGuard::set(
        "CODEWHALE_BASE_URL",
        "https://lower-env-gateway.example.test/v1",
    );

    let config = crate::plugins::providers::load_config(Some(config_path), None)?;
    assert_eq!(
        config.active_route_base_url(),
        "https://managed-gateway.example.test/v1"
    );
    assert_eq!(config.active_route_api_key()?, "managed-route-key");
    assert!(active_provider_has_config_api_key(&config));
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Openrouter)
    ));
    Ok(())
}

#[test]
fn managed_config_cannot_grant_external_credential_consent() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    let managed_path = temp_root.path().join("managed.toml");
    let external_path = temp_root.path().join("codex-auth.json");
    let external_raw = r#"{"tokens":{"access_token":"must-never-be-read"}}"#;
    fs::write(&external_path, external_raw)?;
    fs::write(
        &managed_path,
        format!(
            r#"[providers.openai_codex.external_credentials]
access = "read_only"
provider = "openai-codex"
source = "codex_cli"
path = {:?}
consent_version = 1
"#,
            external_path.display().to_string()
        ),
    )?;
    fs::write(
        &config_path,
        format!(
            "provider = \"openai-codex\"\nmanaged_config_path = {:?}\n",
            managed_path.display().to_string()
        ),
    )?;

    codewhale_runtime::external_credentials::reset_side_effect_trap();
    let config = crate::plugins::providers::load_config(Some(config_path), None)?;
    assert!(
        config
            .provider_config_for(&config.test_identity_for_kind(ProviderKind::OpenaiCodex))
            .and_then(|provider| provider.external_credentials.as_ref())
            .is_none()
    );
    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OpenaiCodex)
    ));
    assert_eq!(
        codewhale_runtime::external_credentials::side_effect_trap_counts(),
        (0, 0),
        "managed config must not consent to user-owned external credentials"
    );
    assert_eq!(fs::read_to_string(external_path)?, external_raw);
    Ok(())
}

#[test]
fn managed_disabled_external_policy_tightens_lower_user_consent() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    let managed_path = temp_root.path().join("managed.toml");
    let external_path = temp_root.path().join("codex-auth.json");
    fs::write(
        &external_path,
        r#"{"tokens":{"access_token":"must-not-read"}}"#,
    )?;
    fs::write(
        &managed_path,
        format!(
            r#"[providers.openai_codex.external_credentials]
access = "disabled"
provider = "openai-codex"
source = "codex_cli"
path = {:?}
consent_version = 1
"#,
            external_path.display().to_string()
        ),
    )?;
    fs::write(
        &config_path,
        format!(
            r#"provider = "openai-codex"
managed_config_path = {:?}

[providers.openai_codex]
auth_mode = "oauth"

[providers.openai_codex.external_credentials]
access = "read_only"
provider = "openai-codex"
source = "codex_cli"
path = {:?}
consent_version = 1
"#,
            managed_path.display().to_string(),
            external_path.display().to_string(),
        ),
    )?;
    let _auth_path = EnvVarGuard::set("OPENAI_CODEX_AUTH_FILE", &external_path);
    codewhale_runtime::external_credentials::reset_side_effect_trap();
    let config = crate::plugins::providers::load_config(Some(config_path), None)?;
    let effective = config
        .provider_config_for(&config.test_identity_for_kind(ProviderKind::OpenaiCodex))
        .and_then(|provider| provider.external_credentials.as_ref())
        .expect("managed disabled tombstone");
    assert_eq!(
        effective.access,
        codewhale_config::ExternalCredentialAccess::Disabled
    );
    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OpenaiCodex)
    ));
    assert_eq!(
        codewhale_runtime::external_credentials::complete_side_effect_trap_counts(),
        (0, 0, 0, 0, 0),
        "managed deny must suppress lower consent before every side effect"
    );
    Ok(())
}

#[test]
fn named_custom_api_key_env_satisfies_runtime_and_onboarding_readiness() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let _route_key = EnvVarGuard::set("NAMED_CUSTOM_ROUTE_KEY", "named-route-key");

    let mut providers = ProvidersConfig::default();
    providers.custom.insert(
        "acme".to_string(),
        ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("https://acme.example.test/v1".to_string()),
            model: Some("acme-model".to_string()),
            api_key_env: Some("NAMED_CUSTOM_ROUTE_KEY".to_string()),
            ..ProviderConfig::default()
        },
    );
    let config = {
        let __cw_config_init_17_0 = Some("acme".to_string());
        let __cw_config_init_17_1 = Some(providers);
        let mut __cw_config_init_17_value: Config = Config::default();
        __cw_config_init_17_value.provider = __cw_config_init_17_0;
        __cw_config_init_17_value.providers = __cw_config_init_17_1;
        __cw_config_init_17_value
    };

    assert_eq!(config.active_route_api_key()?, "named-route-key");
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Custom)
    ));
    assert!(has_api_key(&config));
    Ok(())
}

#[test]
fn auth_mode_none_suppresses_config_env_secret_and_oauth_credentials() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let codewhale_home = temp_root.path().join("isolated-codewhale");
    fs::create_dir_all(&codewhale_home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _ambient = EnvVarGuard::set("XAI_API_KEY", "ambient-xai-key");
    let oauth_path = temp_root.path().join("grok-auth.json");
    fs::write(&oauth_path, r#"{"access_token":"oauth-token"}"#)?;
    let _oauth_path = EnvVarGuard::set("GROK_AUTH_PATH", oauth_path.as_os_str());
    codewhale_secrets::Secrets::auto_detect().set("xai", "saved-xai-key")?;

    let mut providers = ProvidersConfig::default();
    providers.xai.auth_mode = Some("none".to_string());
    providers.xai.api_key = Some("configured-xai-key".to_string());
    providers.xai.http_headers = Some(HashMap::from([
        ("X-API-Key".to_string(), "configured-x-key".to_string()),
        ("Api-Key".to_string(), "configured-key".to_string()),
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
        ("X-Route-Metadata".to_string(), "safe".to_string()),
    ]));
    let config = {
        let __cw_config_init_18_0 = Some("xai".to_string());
        let __cw_config_init_18_1 = Some(HashMap::from([(
            "aUtHoRiZaTiOn".to_string(),
            "Bearer configured-secret".to_string(),
        )]));
        let __cw_config_init_18_2 = Some(providers);
        let mut __cw_config_init_18_value: Config = Config::default();
        __cw_config_init_18_value.provider = __cw_config_init_18_0;
        __cw_config_init_18_value.http_headers = __cw_config_init_18_1;
        __cw_config_init_18_value.providers = __cw_config_init_18_2;
        __cw_config_init_18_value
    };

    assert_eq!(config.active_route_api_key()?, "");
    assert!(
        has_api_key(&config),
        "no-auth routes are ready without a key"
    );
    assert!(!active_provider_has_config_api_key(&config));
    assert!(!active_provider_has_env_api_key(&config));
    let headers = config.http_headers();
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
            !headers
                .keys()
                .any(|candidate| candidate.eq_ignore_ascii_case(name)),
            "disabled auth leaked {name}: {headers:?}"
        );
    }
    assert_eq!(
        headers.get("X-Route-Metadata").map(String::as_str),
        Some("safe")
    );
    Ok(())
}

#[test]
fn provider_sentinel_falls_through_to_route_env_then_fixture_store() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let codewhale_home = temp_root.path().join("isolated-codewhale");
    fs::create_dir_all(&codewhale_home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let config_path = temp_root.path().join("config.toml");
    let secrets = codewhale_secrets::Secrets::auto_detect();

    for sentinel in [API_KEYRING_SENTINEL, "  __KEYRING__  "] {
        fs::write(
            &config_path,
            format!("provider = \"openai\"\n\n[providers.openai]\napi_key = {sentinel:?}\n"),
        )?;
        let config = crate::plugins::providers::load_config(Some(config_path.clone()), None)?;
        assert_eq!(
            config
                .provider_config()
                .and_then(|entry| entry.api_key.as_deref())
                .map(classify_config_api_key_value),
            Some(ConfigApiKeyValueKind::SecretStoreSentinel)
        );
        assert!(!active_provider_has_config_api_key(&config));
        assert!(!has_api_key_for(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openai)
        ));

        secrets.set("openai", "FIXTURE-STORED-KEY")?;
        assert_eq!(
            config.active_route_api_key()?,
            "FIXTURE-STORED-KEY",
            "{sentinel:?} must fall through to the allowed fixture store"
        );
        assert!(active_provider_has_config_api_key(&config));
        assert!(has_api_key_for(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openai)
        ));
        secrets.delete("openai")?;

        let _route_env = EnvVarGuard::set("OFFICIAL_SENTINEL_ROUTE_KEY", "FIXTURE-ENV-KEY");
        fs::write(
            &config_path,
            format!(
                "provider = \"openai\"\n\n[providers.openai]\napi_key = {sentinel:?}\napi_key_env = \"OFFICIAL_SENTINEL_ROUTE_KEY\"\n"
            ),
        )?;
        let config = crate::plugins::providers::load_config(Some(config_path.clone()), None)?;
        assert_eq!(
            config.active_route_api_key()?,
            "FIXTURE-ENV-KEY",
            "route-bound api_key_env must outrank the store after {sentinel:?}"
        );
        assert!(!active_provider_has_config_api_key(&config));
        assert!(active_provider_has_env_api_key(&config));

        fs::write(&config_path, format!("api_key = {sentinel:?}\n"))?;
        let root = crate::plugins::providers::load_config(Some(config_path.clone()), None)?;
        assert!(!active_provider_has_config_api_key(&root));
        assert!(!has_api_key_for(
            &root,
            &(root).test_identity_for_kind(ProviderKind::Deepseek)
        ));
        secrets.set("deepseek", "FIXTURE-DEEPSEEK-STORED-KEY")?;
        assert_eq!(
            root.active_route_api_key()?,
            "FIXTURE-DEEPSEEK-STORED-KEY",
            "root {sentinel:?} must also fall through to the allowed fixture store"
        );
        assert!(active_provider_has_config_api_key(&root));
        secrets.delete("deepseek")?;
    }
    Ok(())
}

#[test]
fn custom_route_sentinel_is_never_a_key_and_requires_a_route_binding() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let config_path = temp_root.path().join("config.toml");

    for sentinel in [API_KEYRING_SENTINEL, "  __KEYRING__  "] {
        fs::write(
            &config_path,
            format!(
                "provider = \"acme\"\n\n[providers.acme]\nkind = \"openai-compatible\"\nbase_url = \"https://acme.example.test/v1\"\nmodel = \"acme-model\"\napi_key = {sentinel:?}\n"
            ),
        )?;
        let config = crate::plugins::providers::load_config(Some(config_path.clone()), None)?;
        assert!(config.should_skip_secret_store_for_provider(
            &config.test_identity_for_kind(ProviderKind::Custom)
        ));
        let error = config
            .active_route_api_key()
            .expect_err("named custom sentinel must not become a bearer key");
        assert!(error.to_string().contains("must be bound explicitly"));
        assert!(!active_provider_has_config_api_key(&config));
        assert!(!has_api_key_for(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Custom)
        ));

        let _route_env = EnvVarGuard::set("CUSTOM_SENTINEL_ROUTE_KEY", "FIXTURE-CUSTOM-ENV-KEY");
        fs::write(
            &config_path,
            format!(
                "provider = \"acme\"\n\n[providers.acme]\nkind = \"openai-compatible\"\nbase_url = \"https://acme.example.test/v1\"\nmodel = \"acme-model\"\napi_key = {sentinel:?}\napi_key_env = \"CUSTOM_SENTINEL_ROUTE_KEY\"\n"
            ),
        )?;
        let config = crate::plugins::providers::load_config(Some(config_path.clone()), None)?;
        assert_eq!(config.active_route_api_key()?, "FIXTURE-CUSTOM-ENV-KEY");
        assert!(!active_provider_has_config_api_key(&config));
        assert!(active_provider_has_env_api_key(&config));
    }

    fs::write(
        &config_path,
        format!(
            "provider = \"openrouter\"\n\n[providers.openrouter]\nbase_url = \"https://gateway.example.test/v1\"\napi_key = {API_KEYRING_SENTINEL:?}\n"
        ),
    )?;
    let custom_endpoint = crate::plugins::providers::load_config(Some(config_path), None)?;
    assert!(custom_endpoint.should_skip_secret_store_for_provider(
        &custom_endpoint.test_identity_for_kind(ProviderKind::Openrouter)
    ));
    assert!(custom_endpoint.active_route_api_key().is_err());
    assert!(!active_provider_has_config_api_key(&custom_endpoint));
    assert!(!has_api_key_for(
        &custom_endpoint,
        &(custom_endpoint).test_identity_for_kind(ProviderKind::Openrouter)
    ));
    Ok(())
}

#[test]
fn loopback_deepseek_base_url_runs_without_api_key() -> Result<()> {
    let _lock = lock_test_env();
    let config = <Config as Default>::default()
        .with_legacy_root(None, Some("http://127.0.0.1:8000/v1".to_string()));

    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Deepseek
    );
    assert!(has_api_key(&config));
    assert_eq!(config.active_route_api_key()?, "");
    Ok(())
}

#[test]
fn xiaomi_mimo_custom_env_url_does_not_inherit_ambient_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-xiaomi-mimo-env-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "mimo");
        env::set_var("MIMO_API_KEY", "mimo-env-key");
        env::set_var("MIMO_BASE_URL", "https://mimo-gateway.example/v1");
        env::set_var("MIMO_MODEL", "mimo-v2.5");
    }

    let config = crate::plugins::providers::load_config(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::XiaomiMimo
    );
    let error = config
        .active_route_api_key()
        .expect_err("ambient key must not follow a custom endpoint");
    assert!(error.to_string().contains("must be bound explicitly"));
    assert!(!has_api_key(&config));
    assert_eq!(
        config.active_route_base_url(),
        "https://mimo-gateway.example/v1"
    );
    assert_eq!(config.default_model(), "mimo-v2.5");
    Ok(())
}

#[test]
fn wanjie_ark_custom_env_url_does_not_inherit_ambient_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-wanjie-env-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "ark-wanjie");
        env::set_var("WANJIE_ARK_API_KEY", "wanjie-env-key");
        env::set_var("WANJIE_ARK_BASE_URL", "https://wanjie.example/api/v1");
        env::set_var("WANJIE_ARK_MODEL", "wanjie-model-id");
    }

    let config = crate::plugins::providers::load_config(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::WanjieArk
    );
    let error = config
        .active_route_api_key()
        .expect_err("ambient key must not follow a custom endpoint");
    assert!(error.to_string().contains("must be bound explicitly"));
    assert!(!has_api_key(&config));
    assert_eq!(
        config.active_route_base_url(),
        "https://wanjie.example/api/v1"
    );
    assert_eq!(config.default_model(), "wanjie-model-id");
    Ok(())
}

#[test]
fn openai_custom_env_url_does_not_inherit_ambient_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-openai-env-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "openai");
        env::set_var("OPENAI_API_KEY", "openai-env-key");
        env::set_var("OPENAI_BASE_URL", "https://openai-compatible.example/v4");
        env::set_var("OPENAI_MODEL", "glm-5");
    }

    let config = crate::plugins::providers::load_config(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Openai
    );
    let error = config
        .active_route_api_key()
        .expect_err("ambient key must not follow a custom endpoint");
    assert!(error.to_string().contains("must be bound explicitly"));
    assert!(!has_api_key(&config));
    assert_eq!(
        config.active_route_base_url(),
        "https://openai-compatible.example/v4"
    );
    assert_eq!(config.default_model(), "glm-5");
    Ok(())
}

#[test]
fn openai_facade_custom_url_does_not_inherit_ambient_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-openai-forwarded-base-url-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "openai");
        env::set_var("OPENAI_API_KEY", "forwarded-openai-key");
        env::set_var("DEEPSEEK_BASE_URL", "https://forwarded-openai.example/v4");
        env::set_var("DEEPSEEK_MODEL", "glm-5");
    }

    let config = crate::plugins::providers::load_config(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Openai
    );
    let error = config
        .active_route_api_key()
        .expect_err("ambient key must not follow a custom endpoint");
    assert!(error.to_string().contains("must be bound explicitly"));
    assert!(!has_api_key(&config));
    assert_eq!(
        config.active_route_base_url(),
        "https://forwarded-openai.example/v4"
    );
    assert_eq!(config.default_model(), "glm-5");
    Ok(())
}

#[test]
fn volcengine_custom_env_url_does_not_inherit_ambient_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-volcengine-env-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("DEEPSEEK_PROVIDER", "volcengine");
        env::set_var("ARK_API_KEY", "volc-env-key");
        env::set_var("VOLCENGINE_ARK_BASE_URL", "https://volc.example/v1");
        env::set_var("VOLCENGINE_ARK_MODEL", "DeepSeek-V4-Flash");
    }

    let config = crate::plugins::providers::load_config(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Volcengine
    );
    let error = config
        .active_route_api_key()
        .expect_err("ambient key must not follow a custom endpoint");
    assert!(error.to_string().contains("must be bound explicitly"));
    assert!(!has_api_key(&config));
    assert_eq!(config.active_route_base_url(), "https://volc.example/v1");
    assert_eq!(config.default_model(), "DeepSeek-V4-Flash");
    Ok(())
}

#[test]
fn sglang_provider_works_without_api_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-sglang-defaults-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config = {
        let __cw_config_init_20_0 = Some("sglang".to_string());
        let mut __cw_config_init_20_value: Config = Default::default();
        __cw_config_init_20_value.provider = __cw_config_init_20_0;
        __cw_config_init_20_value
    };
    config.validate()?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Sglang
    );
    assert_eq!(config.default_model(), DEFAULT_SGLANG_MODEL);
    assert_eq!(config.active_route_base_url(), DEFAULT_SGLANG_BASE_URL);
    assert_eq!(config.active_route_api_key()?, "");
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Sglang)
    ));
    Ok(())
}

#[test]
fn ollama_provider_uses_local_defaults_without_api_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-ollama-defaults-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config = {
        let __cw_config_init_21_0 = Some("ollama".to_string());
        let mut __cw_config_init_21_value: Config = Default::default();
        __cw_config_init_21_value.provider = __cw_config_init_21_0;
        __cw_config_init_21_value
    };
    config.validate()?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Ollama
    );
    assert_eq!(config.default_model(), DEFAULT_OLLAMA_MODEL);
    assert_eq!(config.active_route_base_url(), DEFAULT_OLLAMA_BASE_URL);
    assert_eq!(config.active_route_api_key()?, "");
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Ollama)
    ));
    Ok(())
}

#[test]
fn ollama_cloud_resolves_env_key_and_is_not_keyless() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-ollama-cloud-env-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);
    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe { env::set_var("OLLAMA_API_KEY", "ollama-cloud-env-key") };

    let config = {
        let __cw_config_init_22_0 = Some("ollama".to_string());
        let __cw_config_init_22_1 = Some(ProvidersConfig {
            ollama: ProviderConfig {
                base_url: Some("https://ollama.com/v1/".to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_22_value: Config = Config::default();
        __cw_config_init_22_value.provider = __cw_config_init_22_0;
        __cw_config_init_22_value.providers = __cw_config_init_22_1;
        __cw_config_init_22_value
    };

    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::OllamaCloud
    );
    assert!(!provider_route_is_keyless_self_hosted(
        ProviderKind::OllamaCloud,
        &config.active_route_base_url()
    ));
    assert_eq!(config.active_route_api_key()?, "ollama-cloud-env-key");
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OllamaCloud)
    ));
    Ok(())
}

#[test]
fn ollama_cloud_resolves_saved_provider_key() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let codewhale_home = temp_root.path().join("isolated-codewhale");
    fs::create_dir_all(&codewhale_home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");

    codewhale_secrets::Secrets::auto_detect().set("ollama", "ollama-cloud-saved-key")?;
    let config = {
        let __cw_config_init_23_0 = Some("ollama".to_string());
        let __cw_config_init_23_1 = Some(ProvidersConfig {
            ollama: ProviderConfig {
                base_url: Some(codewhale_config::provider::OLLAMA_CLOUD_BASE_URL.to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_23_value: Config = Config::default();
        __cw_config_init_23_value.provider = __cw_config_init_23_0;
        __cw_config_init_23_value.providers = __cw_config_init_23_1;
        __cw_config_init_23_value
    };

    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::OllamaCloud
    );
    assert_eq!(config.active_route_api_key()?, "ollama-cloud-saved-key");
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OllamaCloud)
    ));
    Ok(())
}

#[test]
fn explicit_ollama_cloud_uses_new_secret_slot_without_local_fallback() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let codewhale_home = temp_root.path().join("isolated-codewhale");
    fs::create_dir_all(&codewhale_home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");

    let secrets = codewhale_secrets::Secrets::auto_detect();
    secrets.set("ollama", "must-not-be-consumed")?;
    let config = {
        let __cw_config_init_24_0 = Some("ollama-cloud".to_string());
        let __cw_config_init_24_1 = Some(ProvidersConfig {
            ollama: ProviderConfig {
                base_url: Some(codewhale_config::provider::OLLAMA_CLOUD_BASE_URL.to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_24_value: Config = Config::default();
        __cw_config_init_24_value.provider = __cw_config_init_24_0;
        __cw_config_init_24_value.providers = __cw_config_init_24_1;
        __cw_config_init_24_value
    };

    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::OllamaCloud
    );
    let identity = config
        .resolve_provider_identity("ollama-cloud")
        .expect("explicit Cloud identity");
    assert!(!identity.migrated_legacy_ollama_cloud_route());
    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OllamaCloud)
    ));
    assert!(config.active_route_api_key().is_err());

    secrets.set("ollama-cloud", "cloud-slot-key")?;
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OllamaCloud)
    ));
    assert_eq!(config.active_route_api_key()?, "cloud-slot-key");
    Ok(())
}

#[test]
fn ollama_cloud_without_key_fails_with_cloud_guidance() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-ollama-cloud-missing-key-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config = {
        let __cw_config_init_25_0 = Some("ollama".to_string());
        let __cw_config_init_25_1 = Some(ProvidersConfig {
            ollama: ProviderConfig {
                base_url: Some("https://ollama.com/v1".to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_25_value: Config = Config::default();
        __cw_config_init_25_value.provider = __cw_config_init_25_0;
        __cw_config_init_25_value.providers = __cw_config_init_25_1;
        __cw_config_init_25_value
    };

    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::OllamaCloud
    );
    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OllamaCloud)
    ));
    let error = config
        .active_route_api_key()
        .expect_err("Ollama Cloud must require an API key");
    let message = error.to_string();
    assert!(message.contains("Ollama Cloud API key not found"));
    assert!(message.contains("https://ollama.com/settings/keys"));
    assert!(message.contains("OLLAMA_CLOUD_API_KEY / OLLAMA_API_KEY"));
    Ok(())
}

#[test]
fn ollama_custom_remote_does_not_inherit_cloud_env_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-ollama-custom-remote-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe { env::set_var("OLLAMA_API_KEY", "must-not-cross-routes") };
    codewhale_secrets::Secrets::auto_detect().set("ollama", "must-not-cross-routes-either")?;

    let config = {
        let __cw_config_init_26_0 = Some("ollama".to_string());
        let __cw_config_init_26_1 = Some(ProvidersConfig {
            ollama: ProviderConfig {
                base_url: Some("https://ollama-gateway.example/v1".to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_26_value: Config = Config::default();
        __cw_config_init_26_value.provider = __cw_config_init_26_0;
        __cw_config_init_26_value.providers = __cw_config_init_26_1;
        __cw_config_init_26_value
    };

    assert!(
        config.provider_uses_custom_endpoint(&config.test_identity_for_kind(ProviderKind::Ollama))
    );
    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Ollama)
    ));
    let error = config
        .active_route_api_key()
        .expect_err("custom remote must bind its credential explicitly");
    assert!(
        error
            .to_string()
            .contains("Custom endpoint credentials for ollama must be bound explicitly")
    );
    Ok(())
}

#[test]
fn siliconflow_custom_env_url_does_not_inherit_ambient_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-siliconflow-env-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "siliconflow");
        env::set_var("SILICONFLOW_API_KEY", "sf-env-key");
        env::set_var("SILICONFLOW_BASE_URL", "https://sf-mirror.example/v1");
        env::set_var("SILICONFLOW_MODEL", "deepseek-v4-flash");
    }

    let config = crate::plugins::providers::load_config(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Siliconflow
    );
    let error = config
        .active_route_api_key()
        .expect_err("ambient key must not follow a custom endpoint");
    assert!(error.to_string().contains("must be bound explicitly"));
    assert!(!has_api_key(&config));
    assert_eq!(
        config.active_route_base_url(),
        "https://sf-mirror.example/v1"
    );
    assert_eq!(config.default_model(), "deepseek-v4-flash");
    Ok(())
}

#[test]
fn arcee_custom_env_url_does_not_inherit_ambient_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-arcee-env-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "arcee");
        env::set_var("ARCEE_API_KEY", "arcee-env-key");
        env::set_var("ARCEE_BASE_URL", "https://arcee-mirror.example/api/v1");
        env::set_var("ARCEE_MODEL", "arcee-trinity-large-preview");
    }

    let config = crate::plugins::providers::load_config(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Arcee
    );
    let error = config
        .active_route_api_key()
        .expect_err("ambient key must not follow a custom endpoint");
    assert!(error.to_string().contains("must be bound explicitly"));
    assert!(!has_api_key(&config));
    assert_eq!(
        config.active_route_base_url(),
        "https://arcee-mirror.example/api/v1"
    );
    assert_eq!(config.default_model(), "arcee-trinity-large-preview");
    Ok(())
}

#[test]
fn siliconflow_cn_reads_hyphenated_provider_table_from_config_file() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-siliconflow-cn-table-{}-{}",
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
model = "deepseek-reasoner"
"#,
    )?;

    let config = crate::plugins::providers::load_config(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::SiliconflowCN
    );
    assert_eq!(config.active_route_api_key()?, "sf-cn-table-key");
    assert_eq!(
        config.active_route_base_url(),
        DEFAULT_SILICONFLOW_CN_BASE_URL
    );
    assert_eq!(config.default_model(), DEFAULT_SILICONFLOW_MODEL);
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::SiliconflowCN)
    ));
    Ok(())
}

#[test]
fn moonshot_kimi_import_is_api_key_only_and_never_reads_external_credentials() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-kimi-code-oauth-key-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let kimi_code_home = temp_root.join(".kimi-code");
    let credential_dir = kimi_code_home.join("credentials");
    fs::create_dir_all(&credential_dir)?;
    unsafe { env::set_var("KIMI_CODE_HOME", &kimi_code_home) };

    let credential = json!({
        "access_token": "must-never-be-read",
        "refresh_token": "must-never-be-used",
        "expires_at": SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_secs_f64()
            + 3600.0,
        "scope": "openid profile email",
        "token_type": "Bearer",
    });
    let credential_path = credential_dir.join("kimi-code.json");
    let credential_raw = serde_json::to_string(&credential)?;
    fs::write(&credential_path, &credential_raw)?;

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"provider = "moonshot"

[providers.moonshot]
auth_mode = "kimi_oauth"
api_key = "stale-api-key"
"#,
    )?;

    let config = crate::plugins::providers::load_config(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Moonshot
    );
    assert_eq!(config.active_route_base_url(), DEFAULT_KIMI_CODE_BASE_URL);
    assert_eq!(config.default_model(), DEFAULT_KIMI_CODE_MODEL);
    let error = config
        .active_route_api_key()
        .expect_err("Kimi external OAuth credentials are never imported");
    assert!(error.to_string().contains("does not impersonate"));
    assert!(
        error
            .to_string()
            .contains(codewhale_config::provider::KIMI_CODE_MEMBERSHIP_PLAN_CONSOLE_URL)
    );
    assert!(
        !error
            .to_string()
            .contains("https://platform.kimi.ai/console/api-keys")
    );
    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Moonshot)
    ));
    assert_eq!(
        fs::read_to_string(credential_path)?,
        credential_raw,
        "Codewhale must never read, refresh, or rewrite Kimi CLI credentials"
    );
    Ok(())
}

#[test]
fn codex_external_credentials_are_disabled_by_default_and_managed_fails_before_io() -> Result<()> {
    let _lock = lock_test_env();
    let temp = tempfile::tempdir()?;
    let temp_root = temp.path().canonicalize()?;
    let auth_path = temp_root.join("codex-auth.json");
    let token = crate::test_support::future_test_jwt("codex");
    let raw = serde_json::to_string_pretty(&json!({
        "tokens": {
            "access_token": token.clone(),
            "account_id": "acct-must-not-be-read",
            "refresh_token": "must-never-be-used"
        },
        "unknown": {"preserve": true}
    }))?;
    fs::write(&auth_path, &raw)?;
    let ambient_decoy = temp_root.join("ambient-decoy.json");
    let ambient_decoy_raw = r#"{"tokens":{"access_token":"must-not-be-read"}}"#;
    fs::write(&ambient_decoy, ambient_decoy_raw)?;
    let _auth_path = EnvVarGuard::set("OPENAI_CODEX_AUTH_FILE", &ambient_decoy);
    let _access = EnvVarGuard::remove("OPENAI_CODEX_ACCESS_TOKEN");
    let _legacy_access = EnvVarGuard::remove("CODEX_ACCESS_TOKEN");
    let _account = EnvVarGuard::remove("OPENAI_CODEX_ACCOUNT_ID");
    let _legacy_account = EnvVarGuard::remove("CODEX_ACCOUNT_ID");

    let disabled = {
        let __cw_config_init_27_0 = Some(ProviderKind::OpenaiCodex.as_str().to_string());
        let mut __cw_config_init_27_value: Config = Default::default();
        __cw_config_init_27_value.provider = __cw_config_init_27_0;
        __cw_config_init_27_value
    };
    codewhale_runtime::external_credentials::reset_side_effect_trap();
    assert!(!has_api_key_for(
        &disabled,
        &(disabled).test_identity_for_kind(ProviderKind::OpenaiCodex)
    ));
    let error = disabled
        .active_route_api_key()
        .expect_err("external credentials default to disabled");
    assert!(error.to_string().contains("Sign in with ChatGPT"));
    assert_eq!(disabled.codex_account_id(), None);
    assert_eq!(
        codewhale_runtime::external_credentials::side_effect_trap_counts(),
        (0, 0)
    );

    let mut managed_consent = codewhale_config::ExternalCredentialConsentToml::read_only(
        codewhale_config::ProviderKind::OpenaiCodex,
        codewhale_config::ExternalCredentialSource::CodexCli,
        auth_path.clone(),
    );
    managed_consent.access = codewhale_config::ExternalCredentialAccess::Managed;
    let managed = {
        let __cw_config_init_28_0 = Some(ProviderKind::OpenaiCodex.as_str().to_string());
        let __cw_config_init_28_1 = Some(ProvidersConfig {
            openai_codex: ProviderConfig {
                auth_mode: Some("oauth".to_string()),
                external_credentials: Some(managed_consent),
                ..Default::default()
            },
            ..Default::default()
        });
        let mut __cw_config_init_28_value: Config = Default::default();
        __cw_config_init_28_value.provider = __cw_config_init_28_0;
        __cw_config_init_28_value.providers = __cw_config_init_28_1;
        __cw_config_init_28_value
    };
    codewhale_runtime::external_credentials::reset_side_effect_trap();
    assert!(!has_api_key_for(
        &managed,
        &(managed).test_identity_for_kind(ProviderKind::OpenaiCodex)
    ));
    let error = managed
        .active_route_api_key()
        .expect_err("managed access needs a preservation adapter");
    assert!(error.to_string().contains("Sign in with ChatGPT"));
    assert_eq!(
        codewhale_runtime::external_credentials::side_effect_trap_counts(),
        (0, 0)
    );
    assert_eq!(fs::read_to_string(&auth_path)?, raw);
    Ok(())
}

#[test]
fn moonshot_kimi_code_api_key_uses_coding_model() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-kimi-code-key-{}-{}",
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
api_key = "kimi-code-key"
base_url = "https://api.kimi.com/coding/v1"
"#,
    )?;

    let config = crate::plugins::providers::load_config(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Moonshot
    );
    assert_eq!(config.active_route_base_url(), DEFAULT_KIMI_CODE_BASE_URL);
    assert_eq!(config.default_model(), DEFAULT_KIMI_CODE_MODEL);
    assert_eq!(config.active_route_api_key()?, "kimi-code-key");
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Moonshot)
    ));
    Ok(())
}

/// Env-var-only path: `CODEWHALE_BASE_URL=https://api.kimi.com/coding/v1`
/// combined with `CODEWHALE_PROVIDER=moonshot` must trigger Kimi Code
/// model selection even when the TOML has no `base_url`.
#[test]
fn moonshot_kimi_code_env_base_url_selects_coding_model() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-kimi-code-env-url-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let config_path = temp_root.join(".deepseek").join("config.toml");
    ensure_parent_dir(&config_path)?;
    fs::write(
        &config_path,
        r#"[providers.moonshot]
api_key = "kimi-code-env-key"
"#,
    )?;
    // Safety: test-only env mutation guarded by lock_test_env().
    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "moonshot");
        env::set_var("CODEWHALE_BASE_URL", "https://api.kimi.com/coding/v1");
    }

    let config = crate::plugins::providers::load_config(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Moonshot
    );
    assert_eq!(config.active_route_base_url(), DEFAULT_KIMI_CODE_BASE_URL);
    assert_eq!(config.default_model(), DEFAULT_KIMI_CODE_MODEL);
    assert_eq!(config.active_route_api_key()?, "kimi-code-env-key");
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Moonshot)
    ));
    Ok(())
}

#[test]
fn has_api_key_for_detects_env_and_config_per_provider() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-has-key-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    let mut config = Config::default();
    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Openai)
    ));
    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::WanjieArk)
    ));
    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Volcengine)
    ));
    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Openrouter)
    ));
    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::XiaomiMimo)
    ));
    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Siliconflow)
    ));
    assert!(
        has_api_key_for(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Sglang)
        ),
        "SGLang is self-hosted and does not require a key by default"
    );
    assert!(
        has_api_key_for(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Vllm)
        ),
        "vLLM is self-hosted and does not require a key by default"
    );

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::set_var("OPENROUTER_API_KEY", "or-env");
        env::set_var("OPENAI_API_KEY", "openai-env");
        env::set_var("WANJIE_API_KEY", "wanjie-env");
        env::set_var("ARK_API_KEY", "volc-env");
        env::set_var("MIMO_API_KEY", "mimo-env");
        env::set_var("SILICONFLOW_API_KEY", "sf-env");
    }
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Openai)
    ));
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::WanjieArk)
    ));
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Volcengine)
    ));
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Openrouter)
    ));
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::XiaomiMimo)
    ));
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Siliconflow)
    ));
    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Novita)
    ));

    // Safety: test-only environment mutation guarded by a global mutex.
    unsafe {
        env::remove_var("OPENROUTER_API_KEY");
        env::remove_var("OPENAI_API_KEY");
        env::remove_var("WANJIE_API_KEY");
        env::remove_var("ARK_API_KEY");
        env::remove_var("MIMO_API_KEY");
        env::remove_var("SILICONFLOW_API_KEY");
    }
    let mut providers = ProvidersConfig::default();
    providers.openai.api_key = Some("file-openai".to_string());
    providers.wanjie_ark.api_key = Some("file-wanjie".to_string());
    providers.xiaomi_mimo.api_key = Some("file-mimo".to_string());
    providers.novita.api_key = Some("file-novita".to_string());
    providers.siliconflow.api_key = Some("file-siliconflow".to_string());
    config.providers = Some(providers);
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Openai)
    ));
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::WanjieArk)
    ));
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::XiaomiMimo)
    ));
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Novita)
    ));
    assert!(has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Siliconflow)
    ));
    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Openrouter)
    ));
    Ok(())
}

#[test]
fn provider_auth_source_metadata_is_not_a_runtime_credential() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let mut providers = ProvidersConfig::default();
    providers.openai.auth = Some(codewhale_config::ProviderAuthSourceToml {
        source: codewhale_config::AuthSourceKind::Command,
        command: vec!["secret-tool".to_string(), "lookup".to_string()],
        timeout_ms: Some(2000),
        secret_id: None,
    });
    let config = {
        let __cw_config_init_29_0 = Some("openai".to_string());
        let __cw_config_init_29_1 = Some(providers);
        let mut __cw_config_init_29_value: Config = Config::default();
        __cw_config_init_29_value.provider = __cw_config_init_29_0;
        __cw_config_init_29_value.providers = __cw_config_init_29_1;
        __cw_config_init_29_value
    };

    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Openai)
    ));
    assert!(config.active_route_api_key().is_err());
    Ok(())
}

#[test]
fn xai_oauth_selection_falls_back_to_explicit_api_key_without_external_io() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-xai-auth-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let auth_path = temp_root.join("auth.json");
    let _auth_path = EnvVarGuard::set("GROK_AUTH_PATH", auth_path.as_os_str());
    let _xai_key = EnvVarGuard::remove("XAI_API_KEY");

    let mut providers = ProvidersConfig::default();
    providers.xai.api_key = Some("fake-xai-cfg-key".to_string());
    providers.xai.auth_mode = Some("oauth".to_string());
    let api_key_config = {
        let __cw_config_init_30_0 = Some("xai".to_string());
        let __cw_config_init_30_1 = Some(providers);
        let mut __cw_config_init_30_value: Config = Config::default();
        __cw_config_init_30_value.provider = __cw_config_init_30_0;
        __cw_config_init_30_value.providers = __cw_config_init_30_1;
        __cw_config_init_30_value
    };
    codewhale_runtime::external_credentials::reset_side_effect_trap();
    assert!(has_api_key_for(
        &api_key_config,
        &(api_key_config).test_identity_for_kind(ProviderKind::Xai)
    ));
    assert_eq!(api_key_config.active_route_api_key()?, "fake-xai-cfg-key");
    assert_eq!(
        codewhale_runtime::external_credentials::side_effect_trap_counts(),
        (0, 0)
    );

    fs::write(&auth_path, "{}")?;
    assert!(!has_api_key_for(
        &Config::default(),
        &(Config::default()).test_identity_for_kind(ProviderKind::Xai)
    ));
    fs::remove_dir_all(temp_root)?;
    Ok(())
}

#[test]
fn modelstudio_variants_share_one_secret_slot_and_key_availability() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let codewhale_home = temp_root.path().join("codewhale-home");
    let config_path = codewhale_home.join("config.toml");
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _ms_env = EnvVarGuard::remove("MODELSTUDIO_API_KEY");
    let _dashscope_env = EnvVarGuard::remove("DASHSCOPE_API_KEY");
    let _cli_source = EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
    let _cli_key = EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");

    let variants = [
        ProviderKind::ModelstudioTokenPlan,
        ProviderKind::ModelstudioTokenPlanAnthropic,
        ProviderKind::ModelstudioCodingPlan,
        ProviderKind::ModelstudioCodingPlanAnthropic,
    ];
    for variant in variants {
        assert_eq!(
            provider_secret_store_slot(variant),
            "modelstudio-token-plan",
            "{variant:?} must share the family's one credential slot"
        );
    }

    // Saving on the Token Plan variant writes the single family slot only.
    save_api_key_for(ProviderKind::ModelstudioTokenPlan, "ms-family-key")?;
    let secrets = codewhale_secrets::Secrets::auto_detect();
    assert_eq!(
        secrets.get("modelstudio-token-plan")?,
        Some("ms-family-key".to_string())
    );
    assert_eq!(secrets.get("modelstudio-coding-plan")?, None);

    // Every variant — active or not — resolves the family key, so the picker
    // badge stops showing three bogus "missing key" rows after one save.
    let inactive_variants =
        crate::plugins::providers::load_config(Some(config_path.clone()), None)?;
    for variant in variants {
        assert_eq!(
            provider_secret_store_api_key(
                &inactive_variants,
                &(inactive_variants).test_identity_for_kind(variant)
            )
            .as_deref(),
            Some("ms-family-key"),
            "{variant:?} must read the family slot"
        );
        assert!(
            has_api_key_for(
                &inactive_variants,
                &(inactive_variants).test_identity_for_kind(variant)
            ),
            "{variant:?} key-availability badge must resolve the family key"
        );
    }

    // Saving on any sibling variant overwrites the same shared slot.
    save_api_key_for(
        ProviderKind::ModelstudioCodingPlanAnthropic,
        "ms-family-key-v2",
    )?;
    assert_eq!(
        secrets.get("modelstudio-token-plan")?,
        Some("ms-family-key-v2".to_string())
    );
    assert_eq!(secrets.get("modelstudio-coding-plan-anthropic")?, None);
    let reloaded = crate::plugins::providers::load_config(Some(config_path), None)?;
    for variant in variants {
        assert_eq!(
            provider_secret_store_api_key(&reloaded, &(reloaded).test_identity_for_kind(variant))
                .as_deref(),
            Some("ms-family-key-v2"),
            "{variant:?} must follow the family slot across saves"
        );
    }
    Ok(())
}

// ========================================================================
// Provider Capability Matrix tests
// ========================================================================

#[test]
fn provider_capability_scenario() {
    // Scenario consolidation of: provider_capability_deepseek_v4_pro_has_1m_window_and_thinking, provider_capability_deepseek_anthropic_uses_messages_payload, provider_capability_openmodel_uses_messages_payload, provider_capability_deepseek_v4_flash_has_1m_window_and_thinking, provider_capability_deepseek_chat_alias_has_v4_flash_caps_and_metadata, provider_capability_deepseek_reasoner_alias_has_v4_flash_caps_and_metadata, provider_capability_deepseek_v4_flash_has_no_alias_deprecation, provider_capability_nvidia_nim_v4_pro_maps_correctly
    // from provider_capability_deepseek_v4_pro_has_1m_window_and_thinking
    {
        let cap = provider_capability(ProviderKind::Deepseek, "deepseek-v4-pro");
        assert_eq!(
            cap.context_window,
            codewhale_models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(cap.max_output, Some(384_000));
        assert!(cap.thinking_supported);
        assert!(cap.cache_telemetry_supported);
        assert_eq!(
            cap.request_payload_mode,
            RequestPayloadMode::ChatCompletions
        );
    }
    // from provider_capability_deepseek_anthropic_uses_messages_payload
    {
        let cap = provider_capability(
            ProviderKind::DeepseekAnthropic,
            DEFAULT_DEEPSEEK_ANTHROPIC_MODEL,
        );
        assert_eq!(
            cap.context_window,
            codewhale_models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(cap.max_output, Some(384_000));
        assert!(cap.thinking_supported);
        assert!(!cap.cache_telemetry_supported);
        assert_eq!(
            cap.request_payload_mode,
            RequestPayloadMode::AnthropicMessages
        );
        assert!(cap.alias_deprecation.is_none());
    }
    // from provider_capability_openmodel_uses_messages_payload
    {
        let cap = provider_capability(ProviderKind::Openmodel, DEFAULT_OPENMODEL_MODEL);
        assert_eq!(cap.resolved_model, DEFAULT_OPENMODEL_MODEL);
        assert_eq!(
            cap.context_window,
            codewhale_models::context_window_for_model(DEFAULT_OPENMODEL_MODEL).unwrap_or(200_000)
        );
        assert_eq!(
            cap.max_output,
            Some(
                codewhale_models::max_output_tokens_for_model(DEFAULT_OPENMODEL_MODEL)
                    .unwrap_or(64_000)
            )
        );
        assert!(!cap.cache_telemetry_supported);
        assert_eq!(
            cap.request_payload_mode,
            RequestPayloadMode::AnthropicMessages
        );
        assert!(provider_passes_model_through(ProviderKind::Openmodel));
    }
    // from provider_capability_deepseek_v4_flash_has_1m_window_and_thinking
    {
        let cap = provider_capability(ProviderKind::Deepseek, "deepseek-v4-flash");
        assert_eq!(
            cap.context_window,
            codewhale_models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(cap.max_output, Some(384_000));
        assert!(cap.thinking_supported);
        assert!(cap.cache_telemetry_supported);
    }
    // from provider_capability_deepseek_chat_alias_has_v4_flash_caps_and_metadata
    {
        let cap = provider_capability(ProviderKind::Deepseek, "deepseek-chat");
        assert_eq!(
            cap.context_window,
            codewhale_models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(cap.max_output, Some(384_000));
        assert!(cap.thinking_supported);
        assert!(cap.cache_telemetry_supported);

        let deprecation = cap
            .alias_deprecation
            .as_ref()
            .expect("alias deprecation metadata");
        assert_eq!(deprecation.alias, "deepseek-chat");
        assert_eq!(deprecation.replacement, "deepseek-flash");
        assert_eq!(deprecation.retirement_date, "2026-07-24");
        assert_eq!(deprecation.retirement_utc, "2026-07-24T15:59:00Z");
    }
    // from provider_capability_deepseek_reasoner_alias_has_v4_flash_caps_and_metadata
    {
        let cap = provider_capability(ProviderKind::Deepseek, "deepseek-reasoner");
        assert_eq!(
            cap.context_window,
            codewhale_models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(cap.max_output, Some(384_000));
        assert!(cap.thinking_supported);
        assert!(cap.cache_telemetry_supported);

        let deprecation = cap
            .alias_deprecation
            .as_ref()
            .expect("alias deprecation metadata");
        assert_eq!(deprecation.alias, "deepseek-reasoner");
        assert_eq!(deprecation.replacement, "deepseek-flash");
    }
    // from provider_capability_deepseek_v4_flash_has_no_alias_deprecation
    {
        let cap = provider_capability(ProviderKind::Deepseek, "deepseek-v4-flash");
        assert!(cap.alias_deprecation.is_none());
    }
    // from provider_capability_nvidia_nim_v4_pro_maps_correctly
    {
        let cap = provider_capability(ProviderKind::NvidiaNim, DEFAULT_NVIDIA_NIM_MODEL);
        assert_eq!(
            cap.context_window,
            codewhale_models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(cap.max_output, Some(384_000));
        assert!(cap.thinking_supported);
        assert!(cap.cache_telemetry_supported);
        assert_eq!(
            cap.request_payload_mode,
            RequestPayloadMode::ChatCompletions
        );
    }
}

#[test]
fn provider_capability_scenario_2() {
    // Scenario consolidation of: provider_capability_nvidia_nim_v4_flash_maps_correctly, provider_capability_openai_codex_uses_responses_payload, provider_capability_marks_exact_inkling_route_as_reasoning, provider_capability_xiaomi_mimo_has_thinking_no_cache, provider_capability_novita_v4_pro_has_thinking_no_cache, provider_capability_fireworks_v4_pro_has_thinking_no_cache, provider_capability_siliconflow_v4_pro_has_thinking_no_cache, provider_capability_sglang_v4_pro_has_thinking_no_cache
    // from provider_capability_nvidia_nim_v4_flash_maps_correctly
    {
        let cap = provider_capability(ProviderKind::NvidiaNim, DEFAULT_NVIDIA_NIM_FLASH_MODEL);
        assert_eq!(
            cap.context_window,
            codewhale_models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(cap.max_output, Some(384_000));
        assert!(cap.thinking_supported);
        assert!(cap.cache_telemetry_supported);
    }
    // from provider_capability_openai_codex_uses_responses_payload
    {
        let cap = provider_capability(ProviderKind::OpenaiCodex, DEFAULT_OPENAI_CODEX_MODEL);
        assert_eq!(cap.provider, ProviderKind::OpenaiCodex);
        assert_eq!(cap.resolved_model, DEFAULT_OPENAI_CODEX_MODEL);
        assert_eq!(
            cap.context_window,
            OPENAI_CODEX_EFFECTIVE_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(cap.max_output, Some(4096));
        assert!(cap.thinking_supported);
        assert!(!cap.cache_telemetry_supported);
        assert_eq!(cap.request_payload_mode, RequestPayloadMode::Responses);
    }
    // from provider_capability_marks_exact_inkling_route_as_reasoning
    {
        let cap = provider_capability(ProviderKind::Together, TOGETHER_INKLING_MODEL);
        assert!(cap.thinking_supported);
        assert_eq!(
            codewhale_models::context_window_for_model(TOGETHER_INKLING_MODEL),
            None
        );
        assert_eq!(
            codewhale_models::max_output_tokens_for_model(TOGETHER_INKLING_MODEL),
            None
        );
    }
    // from provider_capability_xiaomi_mimo_has_thinking_no_cache
    {
        let cap = provider_capability(ProviderKind::XiaomiMimo, DEFAULT_XIAOMI_MIMO_MODEL);
        assert_eq!(cap.context_window, 1_048_576);
        assert_eq!(cap.max_output, Some(131_072));
        assert!(cap.thinking_supported);
        assert!(!cap.cache_telemetry_supported);
        assert_eq!(
            cap.request_payload_mode,
            RequestPayloadMode::ChatCompletions
        );

        let omni = provider_capability(ProviderKind::XiaomiMimo, XIAOMI_MIMO_V2_6_FLASH_MODEL);
        assert_eq!(omni.context_window, 1_048_576);
        assert_eq!(omni.max_output, Some(131_072));
        assert!(omni.thinking_supported);
        assert!(!omni.cache_telemetry_supported);
    }
    // from provider_capability_novita_v4_pro_has_thinking_no_cache
    {
        let cap = provider_capability(ProviderKind::Novita, DEFAULT_NOVITA_MODEL);
        assert_eq!(
            cap.context_window,
            codewhale_models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(cap.max_output, Some(384_000));
        assert!(cap.thinking_supported);
        assert!(!cap.cache_telemetry_supported);
    }
    // from provider_capability_fireworks_v4_pro_has_thinking_no_cache
    {
        let cap = provider_capability(ProviderKind::Fireworks, DEFAULT_FIREWORKS_MODEL);
        assert_eq!(
            cap.context_window,
            codewhale_models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(cap.max_output, Some(384_000));
        assert!(cap.thinking_supported);
        assert!(!cap.cache_telemetry_supported);

        // Only the exact provider-owned bundled row supplies this ceiling.
        // Neighboring IDs and a different provider cannot inherit its limit.
        for (provider, model) in [
            (
                ProviderKind::Fireworks,
                "accounts/fireworks/models/deepseek-v4.1-flash-expires-on-0910",
            ),
            (
                ProviderKind::Fireworks,
                "accounts/fireworks/models/deepseek-v4-pro-custom",
            ),
            (ProviderKind::Deepseek, DEFAULT_FIREWORKS_MODEL),
        ] {
            assert_eq!(
                provider_capability(provider, model).max_output,
                None,
                "{model}"
            );
        }
    }
    // from provider_capability_siliconflow_v4_pro_has_thinking_no_cache
    {
        let cap = provider_capability(ProviderKind::Siliconflow, DEFAULT_SILICONFLOW_MODEL);
        assert_eq!(
            cap.context_window,
            codewhale_models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(cap.max_output, Some(384_000));
        assert!(cap.thinking_supported);
        assert!(!cap.cache_telemetry_supported);
        assert_eq!(
            cap.request_payload_mode,
            RequestPayloadMode::ChatCompletions
        );
    }
    // from provider_capability_sglang_v4_pro_has_thinking_no_cache
    {
        let cap = provider_capability(ProviderKind::Sglang, DEFAULT_SGLANG_MODEL);
        assert_eq!(
            cap.context_window,
            codewhale_models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(cap.max_output, Some(384_000));
        assert!(cap.thinking_supported);
        assert!(!cap.cache_telemetry_supported);
    }
}

#[test]
fn provider_capability_openrouter_v4_pro_has_thinking_no_cache() {
    let cap = provider_capability(ProviderKind::Openrouter, DEFAULT_OPENROUTER_MODEL);
    assert_eq!(
        cap.context_window,
        codewhale_models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
    );
    assert_eq!(cap.max_output, Some(384_000));
    assert!(cap.thinking_supported);
    // OpenRouter does not return DeepSeek prompt-cache telemetry.
    assert!(!cap.cache_telemetry_supported);
    assert_eq!(
        cap.request_payload_mode,
        RequestPayloadMode::ChatCompletions
    );
}

#[test]
fn provider_capability_openrouter_recent_large_models_are_reasoning_aware() {
    for (model, expected_window, expected_output) in [
        (
            OPENROUTER_ARCEE_TRINITY_LARGE_THINKING_MODEL,
            262_144,
            262_144,
        ),
        (OPENROUTER_QWEN_3_6_FLASH_MODEL, 1_000_000, 65_536),
        // Output caps vendor-verified at 65,536 (MODEL_PROVIDER_AUDIT A2/D-7).
        (OPENROUTER_QWEN_3_6_35B_A3B_MODEL, 262_144, 65_536),
        (OPENROUTER_QWEN_3_6_MAX_PREVIEW_MODEL, 262_144, 65_536),
        (OPENROUTER_QWEN_3_6_27B_MODEL, 262_144, 65_536),
        (OPENROUTER_QWEN_3_6_PLUS_MODEL, 1_000_000, 65_536),
        (OPENROUTER_XIAOMI_MIMO_V2_5_PRO_MODEL, 1_000_000, 131_072),
        (OPENROUTER_MINIMAX_M3_MODEL, 1_000_000, 524_288),
        (OPENROUTER_MINIMAX_M2_7_MODEL, 204_800, 131_072),
        (OPENROUTER_GLM_5_1_MODEL, 202_752, 131_072),
        (OPENROUTER_GLM_5_2_MODEL, 1_000_000, 131_072),
        (OPENROUTER_NEMOTRON_3_ULTRA_MODEL, 1_000_000, 16_384),
    ] {
        let cap = provider_capability(ProviderKind::Openrouter, model);

        assert_eq!(cap.context_window, expected_window);
        assert_eq!(cap.max_output, Some(expected_output));
        assert!(cap.thinking_supported);
        assert!(!cap.cache_telemetry_supported);
        assert_eq!(
            cap.request_payload_mode,
            RequestPayloadMode::ChatCompletions
        );
    }
}

#[test]
fn provider_capability_arcee_direct_models_use_api_docs_shape() {
    let thinking_cap = provider_capability(ProviderKind::Arcee, DEFAULT_ARCEE_MODEL);
    assert_eq!(thinking_cap.context_window, 262_144);
    assert_eq!(thinking_cap.max_output, Some(262_144));
    assert!(thinking_cap.thinking_supported);
    assert!(!thinking_cap.cache_telemetry_supported);
    assert_eq!(
        thinking_cap.request_payload_mode,
        RequestPayloadMode::ChatCompletions
    );

    let preview = provider_capability(ProviderKind::Arcee, ARCEE_TRINITY_LARGE_PREVIEW_MODEL);
    assert_eq!(preview.context_window, 262_144);
    assert_eq!(preview.max_output, None);
    assert!(!preview.thinking_supported);

    let mini = provider_capability(ProviderKind::Arcee, ARCEE_TRINITY_MINI_MODEL);
    assert_eq!(mini.context_window, 128_000);
    // Trinity Mini's upstream output limit is unknown, and ProviderCapability
    // now says so instead of fabricating a 4K request fallback.
    assert_eq!(mini.max_output, None);
    assert_eq!(
        codewhale_models::max_output_tokens_for_model(ARCEE_TRINITY_MINI_MODEL),
        None
    );
    assert!(mini.thinking_supported);
    assert!(!mini.cache_telemetry_supported);
    assert_eq!(
        mini.request_payload_mode,
        RequestPayloadMode::ChatCompletions
    );
}

#[test]
fn provider_capability_scenario_3() {
    // Scenario consolidation of: provider_capability_openai_custom_model_is_chat_completions_without_thinking, provider_capability_atlascloud_v4_model_resolves_model_metadata, provider_capability_moonshot_default_model_resolves_kimi_metadata, provider_capability_minimax_anthropic_uses_messages_shape, provider_capability_wanjie_ark_reasoner_has_thinking_no_cache, provider_capability_mistral_matches_reasoning_model_contract, provider_capability_ollama_deepseek_tag_uses_deepseek_heuristic, provider_capability_ollama_unknown_model_falls_back_to_8192
    // from provider_capability_openai_custom_model_is_chat_completions_without_thinking
    {
        let cap = provider_capability(ProviderKind::Openai, "glm-5");
        assert_eq!(
            cap.context_window,
            codewhale_models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(cap.max_output, None);
        assert!(!cap.thinking_supported);
        assert!(!cap.cache_telemetry_supported);
        assert_eq!(
            cap.request_payload_mode,
            RequestPayloadMode::ChatCompletions
        );
    }
    // from provider_capability_atlascloud_v4_model_resolves_model_metadata
    {
        // #3023: Atlascloud uses the generic model-based path, so its default
        // DeepSeek V4 model resolves the real V4 metadata instead of the old
        // hardcoded legacy floor.
        let cap = provider_capability(ProviderKind::Atlascloud, "deepseek-ai/deepseek-v4-flash");
        assert_eq!(
            cap.context_window,
            codewhale_models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(cap.max_output, Some(384_000));
        assert!(cap.thinking_supported);
        assert!(!cap.cache_telemetry_supported);
        assert_eq!(
            cap.request_payload_mode,
            RequestPayloadMode::ChatCompletions
        );
    }
    // from provider_capability_moonshot_default_model_resolves_kimi_metadata
    {
        let cap = provider_capability(ProviderKind::Moonshot, DEFAULT_MOONSHOT_MODEL);
        assert_eq!(cap.context_window, 262_144);
        assert_eq!(cap.max_output, Some(32_768));
        assert!(cap.thinking_supported);
        assert!(!cap.cache_telemetry_supported);
        assert_eq!(
            cap.request_payload_mode,
            RequestPayloadMode::ChatCompletions
        );
    }
    // from provider_capability_minimax_anthropic_uses_messages_shape
    {
        for model in [DEFAULT_MINIMAX_MODEL, MINIMAX_M2_7_MODEL] {
            let cap = provider_capability(ProviderKind::MinimaxAnthropic, model);
            assert!(cap.thinking_supported, "{model}");
            assert!(!cap.cache_telemetry_supported, "{model}");
            assert_eq!(
                cap.request_payload_mode,
                RequestPayloadMode::AnthropicMessages
            );
        }
    }
    // from provider_capability_wanjie_ark_reasoner_has_thinking_no_cache
    {
        let cap = provider_capability(ProviderKind::WanjieArk, DEFAULT_WANJIE_ARK_MODEL);
        assert_eq!(
            cap.context_window,
            codewhale_models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(cap.max_output, None);
        assert!(cap.thinking_supported);
        assert!(!cap.cache_telemetry_supported);
        assert_eq!(
            cap.request_payload_mode,
            RequestPayloadMode::ChatCompletions
        );
    }
    // from provider_capability_mistral_matches_reasoning_model_contract
    {
        for model in ["mistral-medium-latest", "mistral-small-latest"] {
            let cap = provider_capability(ProviderKind::Mistral, model);
            assert_eq!(cap.context_window, 262_144, "{model}");
            assert!(cap.thinking_supported, "{model}");
            assert_eq!(
                cap.request_payload_mode,
                RequestPayloadMode::ChatCompletions
            );
        }
        for model in ["mistral-code-latest", "mistral-large-latest"] {
            let cap = provider_capability(ProviderKind::Mistral, model);
            assert!(!cap.thinking_supported, "{model}");
        }
    }
    // Unknown local tags retain an explicitly conservative budget.
    {
        // A family name does not establish this deployment's context window.
        let cap = provider_capability(ProviderKind::Ollama, "deepseek-v3.1:671b");
        assert_eq!(cap.context_window, 8192);
        assert_eq!(
            codewhale_models::context_window_for_model("deepseek-v3.1:671b"),
            None
        );
        assert_eq!(cap.max_output, None);
        assert!(!cap.thinking_supported);
        assert!(!cap.cache_telemetry_supported);
        assert_eq!(
            cap.request_payload_mode,
            RequestPayloadMode::ChatCompletions
        );
    }
    // from provider_capability_ollama_unknown_model_falls_back_to_8192
    {
        let cap = provider_capability(ProviderKind::Ollama, "llama3.2:3b");
        assert_eq!(cap.context_window, 8192);
        assert_eq!(cap.max_output, None);
        assert!(!cap.thinking_supported);
        assert!(!cap.cache_telemetry_supported);
        assert_eq!(
            cap.request_payload_mode,
            RequestPayloadMode::ChatCompletions
        );
    }
}

#[test]
fn provider_capability_kimi_membership_ids_report_unknown_output_ceiling() {
    // The `kimi-for-coding` family is membership-only: the membership catalog
    // owns its output limits, so the static matrix must say "unknown" rather
    // than fabricating a ceiling. A placeholder here is not cosmetic — it
    // becomes a hard request clamp in `route_budget`.
    for model in ["kimi-for-coding", "kimi-for-coding-highspeed"] {
        let cap = provider_capability(ProviderKind::Moonshot, model);
        assert_eq!(cap.context_window, 262_144, "{model}");
        assert_eq!(cap.max_output, None, "{model}");
        assert!(cap.thinking_supported, "{model}");

        // Unknown is *omitted* on the wire, never serialized as a number.
        let json = serde_json::to_value(&cap).expect("capability serializes");
        assert!(
            json.get("max_output").is_none(),
            "{model}: unknown output ceiling must not be serialized: {json}"
        );
        let round_tripped: ProviderCapability =
            serde_json::from_value(json).expect("capability round-trips with an absent max_output");
        assert_eq!(round_tripped, cap, "{model}");
    }

    // The direct-platform K2.7 Code route does publish 32K, and keeps it.
    assert_eq!(
        provider_capability(ProviderKind::Moonshot, "kimi-k2.7-code").max_output,
        Some(32_768)
    );
}

#[test]
fn provider_capability_zai_defaults_to_5_3_and_tracks_5_2_5_1_and_turbo() {
    // GLM-5.3 is now the default direct Z.AI model; its limits inherit from
    // GLM-5.2 (1M context window) until Z.ai publishes distinct 5.3 numbers.
    let default = provider_capability(ProviderKind::Zai, DEFAULT_ZAI_MODEL);
    assert_eq!(default.resolved_model, DEFAULT_ZAI_MODEL);
    assert_eq!(default.resolved_model, ZAI_GLM_5_3_MODEL);
    assert_eq!(default.context_window, 1_000_000);
    assert_eq!(default.max_output, Some(131_072));
    assert!(default.thinking_supported);
    assert!(!default.cache_telemetry_supported);

    // GLM-5.2 remains available as an explicit model with its own id.
    let v52 = provider_capability(ProviderKind::Zai, ZAI_GLM_5_2_MODEL);
    assert_eq!(v52.resolved_model, ZAI_GLM_5_2_MODEL);
    assert_eq!(v52.context_window, 1_000_000);
    assert_eq!(v52.max_output, Some(131_072));
    assert!(v52.thinking_supported);

    // GLM-5.1 remains available as an explicit model (smaller window).
    let v51 = provider_capability(ProviderKind::Zai, ZAI_GLM_5_1_MODEL);
    assert_eq!(v51.resolved_model, ZAI_GLM_5_1_MODEL);
    assert_eq!(v51.context_window, 202_752);
    assert_eq!(v51.max_output, Some(131_072));
    assert!(v51.thinking_supported);

    // GLM-5.3-Flash is the published 1M multimodal sibling.
    let flash = provider_capability(ProviderKind::Zai, ZAI_GLM_5_3_FLASH_MODEL);
    assert_eq!(flash.resolved_model, ZAI_GLM_5_3_FLASH_MODEL);
    assert_eq!(flash.context_window, 1_000_000);
    assert_eq!(flash.max_output, Some(131_072));
    assert!(flash.thinking_supported);

    // GLM-5-Turbo is the faster sub-agent sibling of GLM-5.2.
    let turbo = provider_capability(ProviderKind::Zai, ZAI_GLM_5_TURBO_MODEL);
    assert_eq!(turbo.resolved_model, ZAI_GLM_5_TURBO_MODEL);
}

#[test]
fn provider_capability_minimax_direct_models_use_api_docs_shape() {
    let m3 = provider_capability(ProviderKind::Minimax, DEFAULT_MINIMAX_MODEL);
    assert_eq!(m3.context_window, 1_000_000);
    assert_eq!(m3.max_output, Some(524_288));
    assert!(m3.thinking_supported);
    assert!(!m3.cache_telemetry_supported);
    assert_eq!(m3.request_payload_mode, RequestPayloadMode::ChatCompletions);

    for model in [
        MINIMAX_M2_7_MODEL,
        MINIMAX_M2_7_HIGHSPEED_MODEL,
        MINIMAX_M2_5_MODEL,
        MINIMAX_M2_5_HIGHSPEED_MODEL,
        MINIMAX_M2_1_MODEL,
        MINIMAX_M2_1_HIGHSPEED_MODEL,
        MINIMAX_M2_MODEL,
    ] {
        let cap = provider_capability(ProviderKind::Minimax, model);
        assert_eq!(cap.context_window, 204_800, "{model}");
        assert!(cap.thinking_supported, "{model}");
        assert!(!cap.cache_telemetry_supported, "{model}");
        assert_eq!(
            cap.request_payload_mode,
            RequestPayloadMode::ChatCompletions
        );
    }
}

#[test]
fn provider_capability_scenario_4() {
    // Scenario consolidation of: provider_capability_non_v4_model_has_smaller_window, provider_capability_roundtrip_serialization
    // from provider_capability_non_v4_model_has_smaller_window
    {
        let cap = provider_capability(ProviderKind::Deepseek, "deepseek-coder");
        assert_eq!(
            cap.context_window,
            codewhale_models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(cap.max_output, None);
        assert!(!cap.thinking_supported);
    }
    // from provider_capability_roundtrip_serialization
    {
        let cap = provider_capability(ProviderKind::Deepseek, "deepseek-v4-pro");
        let json = serde_json::to_value(&cap).unwrap();
        let deserialized: ProviderCapability = serde_json::from_value(json).unwrap();
        assert_eq!(cap, deserialized);
    }
}

#[test]
fn huggingface_custom_env_urls_do_not_inherit_ambient_keys() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-huggingface-env-test-{}-{}",
        std::process::id(),
        nanos
    ));

    {
        let long_form_root = temp_root.join("long-form");
        fs::create_dir_all(&long_form_root)?;
        let _guard = EnvGuard::new(&long_form_root);

        unsafe {
            env::set_var("CODEWHALE_PROVIDER", "huggingface");
            env::set_var("HUGGINGFACE_API_KEY", "hf-env-key");
            env::set_var("HF_TOKEN", "hf-token-fallback");
            env::set_var("HUGGINGFACE_BASE_URL", "https://custom-hf.example/v1");
            env::set_var("HF_BASE_URL", "https://fallback-hf.example/v1");
            env::set_var("HUGGINGFACE_MODEL", "meta-llama/Llama-3-70B");
            env::set_var("HF_MODEL", "fallback/model");
        }

        let config = crate::plugins::providers::load_config(None, None)?;
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::Huggingface
        );
        let error = config
            .active_route_api_key()
            .expect_err("ambient key must not follow a custom endpoint");
        assert!(error.to_string().contains("must be bound explicitly"));
        assert!(!has_api_key(&config));
        assert_eq!(
            config.active_route_base_url(),
            "https://custom-hf.example/v1"
        );
        assert_eq!(config.default_model(), "meta-llama/Llama-3-70B");
    }

    {
        let short_form_root = temp_root.join("short-form");
        fs::create_dir_all(&short_form_root)?;
        let _guard = EnvGuard::new(&short_form_root);

        unsafe {
            env::set_var("CODEWHALE_PROVIDER", "huggingface");
            env::set_var("HF_TOKEN", "hf-env-key");
            env::set_var("HF_BASE_URL", "https://custom-hf.example/v1");
            env::set_var("HF_MODEL", "meta-llama/Llama-3-70B");
        }

        let config = crate::plugins::providers::load_config(None, None)?;
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::Huggingface
        );
        let error = config
            .active_route_api_key()
            .expect_err("ambient key must not follow a custom endpoint");
        assert!(error.to_string().contains("must be bound explicitly"));
        assert!(!has_api_key(&config));
        assert_eq!(
            config.active_route_base_url(),
            "https://custom-hf.example/v1"
        );
        assert_eq!(config.default_model(), "meta-llama/Llama-3-70B");
    }
    Ok(())
}

#[test]
fn huggingface_short_custom_env_url_does_not_inherit_ambient_key() -> Result<()> {
    let _lock = lock_test_env();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!(
        "codewhale-tui-huggingface-short-env-test-{}-{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&temp_root)?;
    let _guard = EnvGuard::new(&temp_root);

    unsafe {
        env::set_var("CODEWHALE_PROVIDER", "hf");
        env::set_var("HF_TOKEN", "hf-token-value");
        env::set_var("HF_BASE_URL", "https://short-hf.example/v1");
        env::set_var("HF_MODEL", "org/short-model");
    }

    let config = crate::plugins::providers::load_config(None, None)?;
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Huggingface
    );
    let error = config
        .active_route_api_key()
        .expect_err("ambient key must not follow a custom endpoint");
    assert!(error.to_string().contains("must be bound explicitly"));
    assert!(!has_api_key(&config));
    assert_eq!(
        config.active_route_base_url(),
        "https://short-hf.example/v1"
    );
    assert_eq!(config.default_model(), "org/short-model");
    Ok(())
}

#[test]
fn literal_custom_table_round_trips_as_exact_historical_route() {
    let config =
        session_custom_provider_config("custom", "openai-compatible", "http://127.0.0.1:1234/v1");

    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Custom
    );
    let identity = config
        .resolve_provider_identity("custom")
        .expect("exact [providers.custom] identity");
    assert_eq!(identity.key.as_str(), "custom");
    let route = crate::route_runtime::resolve_runtime_route(
        &config,
        identity.provider,
        Some("local-model"),
    )
    .expect("resolve exact literal table")
    .validate()
    .expect("preflight exact literal table");
    assert_eq!(route.identity.key.as_str(), "custom");
    assert_eq!(route.client.base_url(), "http://127.0.0.1:1234/v1");
    assert_eq!(
        route
            .config
            .resolve_provider_identity(route.identity.key.as_str())
            .expect("repeat exact literal table resolution"),
        identity
    );
}

#[test]
fn case_colliding_custom_table_preserves_exact_spelling_across_receipts() {
    let config =
        session_custom_provider_config("CUSTOM", "openai-compatible", "http://127.0.0.1:5678/v1");

    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Custom
    );
    assert_eq!(
        config.active_provider_identity().unwrap().key.as_str(),
        "CUSTOM"
    );
    let identity = config
        .resolve_provider_identity("CUSTOM")
        .expect("exact case-colliding custom identity");
    assert_eq!(identity.key.as_str(), "CUSTOM");
    let route = crate::route_runtime::resolve_runtime_route(
        &config,
        identity.provider,
        Some("local-model"),
    )
    .expect("resolve case-colliding custom table")
    .validate()
    .expect("preflight case-colliding custom table");
    assert_eq!(route.identity.key.as_str(), "CUSTOM");
    assert_eq!(route.client.base_url(), "http://127.0.0.1:5678/v1");
}

#[test]
fn dispatch_endpoint_and_billing_receipts_agree_for_every_resolved_route() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let _managed = no_managed_config(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    fs::write(&config_path, "provider = \"deepseek\"\n")?;
    let _base = EnvVarGuard::set("CODEWHALE_BASE_URL", "https://env-gateway.example.test/v1");

    let config = crate::plugins::providers::load_config(Some(config_path), None)?;

    // `for_route` reads the ambient config; `for_dispatched_route` reads the
    // endpoint the client is actually built from. After the resolver became
    // identity-aware these must not be able to disagree for the active route.
    let provider = config.active_provider_identity().unwrap().provider;
    let resolved = config.active_route_base_url();
    assert_eq!(
        crate::route_billing::for_route(&config, &(config).test_identity_for_kind(provider)),
        crate::route_billing::for_dispatched_route(
            &config,
            crate::route_billing::DispatchedRoute {
                provider,
                base_url: &resolved,
            },
        )
    );

    // A pinned cross-provider child bills from its own resolved endpoint,
    // which is its canonical host — not the session's env-selected gateway.
    for child in [ProviderKind::DeepseekAnthropic, ProviderKind::Moonshot] {
        let child_base = config.base_url_for_route(&config.test_identity_for_kind(child));
        assert_eq!(child_base, child.provider().default_base_url(), "{child:?}");
        assert_eq!(
            crate::route_billing::for_dispatched_route(
                &config,
                crate::route_billing::DispatchedRoute {
                    provider: child,
                    base_url: &child_base,
                },
            ),
            crate::route_billing::for_route(&config, &(config).test_identity_for_kind(child)),
            "{child:?} ambient and dispatch billing receipts must agree"
        );
    }
    Ok(())
}

#[test]
fn readiness_and_inventory_classify_the_resolved_route_not_the_session_host() -> Result<()> {
    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let _guard = EnvGuard::new(temp_root.path());
    let _managed = no_managed_config(temp_root.path());
    let config_path = temp_root.path().join("config.toml");
    fs::write(&config_path, "provider = \"deepseek\"\n")?;
    let _base = EnvVarGuard::set("CODEWHALE_BASE_URL", "http://127.0.0.1:11434/v1");

    let config = crate::plugins::providers::load_config(Some(config_path), None)?;

    // Readiness: the active route is on a local custom host and classifies as
    // keyless-local; the sibling identity is still the canonical hosted
    // endpoint and must not inherit that classification.
    assert_eq!(
        crate::provider_readiness::credential_state_for_provider(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Deepseek)
        ),
        crate::provider_readiness::CredentialState::Local
    );
    assert_ne!(
        crate::provider_readiness::credential_state_for_provider(
            &config,
            &(config)
                .resolve_provider_selection_identity("deepseek-cn")
                .unwrap()
        ),
        crate::provider_readiness::CredentialState::Local
    );

    // Inventory: the runtime route the picker/inventory reads is built by
    // re-pointing a clone of this config, so it must resolve the sibling's own
    // canonical endpoint.
    let route = crate::route_runtime::resolve_runtime_route_for_identity(
        &config,
        &config
            .resolve_provider_selection_identity("deepseek-cn")
            .unwrap(),
        None,
    )
    .expect("deepseek-cn runtime route");
    assert_eq!(
        route.candidate.endpoint().base_url,
        DEFAULT_DEEPSEEKCN_BASE_URL
    );
    assert_eq!(
        route.config.active_route_base_url(),
        DEFAULT_DEEPSEEKCN_BASE_URL
    );

    // And a canonical/default endpoint is never reported as custom.
    assert!(
        !config.provider_uses_custom_endpoint(
            &config
                .resolve_provider_selection_identity("deepseek-cn")
                .unwrap()
        )
    );
    assert!(
        config
            .provider_uses_custom_endpoint(&config.test_identity_for_kind(ProviderKind::Deepseek))
    );
    Ok(())
}

#[test]
fn configured_inactive_provider_reads_its_secret_store_key() -> Result<()> {
    let _lock = lock_test_env();
    let temp = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", temp.path());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _moonshot = EnvVarGuard::remove("MOONSHOT_API_KEY");
    let _kimi = EnvVarGuard::remove("KIMI_API_KEY");

    // The state guided setup leaves behind: auth_mode saved to config, the
    // key saved to the secret store only — and the operator then switches
    // the active provider away (#5033).
    let providers = ProvidersConfig {
        moonshot: ProviderConfig {
            auth_mode: Some("api_key".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    let config = {
        let __cw_config_init_31_0 = Some("deepseek".to_string());
        let __cw_config_init_31_1 = Some(providers);
        let mut __cw_config_init_31_value: Config = Default::default();
        __cw_config_init_31_value.provider = __cw_config_init_31_0;
        __cw_config_init_31_value.providers = __cw_config_init_31_1;
        __cw_config_init_31_value
    };

    assert!(
        !has_api_key_for(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Moonshot)
        ),
        "no stored key yet: the configured provider must still read as unconfigured"
    );

    codewhale_secrets::Secrets::auto_detect().set("moonshot", "kimi-test-credential")?;
    assert!(
        has_api_key_for(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Moonshot)
        ),
        "a configured-but-inactive provider with a stored key must read as configured (#5033)"
    );
    Ok(())
}

/// A self-hosted OpenAI-compatible gateway owns its model namespace, including
/// its casing. A WeChat-community user configured `DeepSeek-V4-Flash` against
/// their company's internal endpoint and reported the id coming back lowercase
/// — an id that endpoint does not serve. Every stage from the parsed config to
/// the resolved wire id must hand the string back byte-for-byte.
#[test]
fn custom_endpoint_model_id_survives_verbatim_through_the_route() {
    const MODEL: &str = "DeepSeek-V4-Flash";
    let shapes: [(&str, &str); 6] = [
        (
            "provider-scoped deepseek table with a custom base_url",
            r#"
provider = "deepseek"
[providers.deepseek]
base_url = "https://llm.corp.internal/v1"
api_key = "k"
model = "DeepSeek-V4-Flash"
"#,
        ),
        (
            "deepseek table with no explicit provider key",
            r#"
[providers.deepseek]
base_url = "https://llm.corp.internal/v1"
api_key = "k"
model = "DeepSeek-V4-Flash"
"#,
        ),
        (
            "root base_url with the root default_text_model",
            r#"
base_url = "https://llm.corp.internal/v1"
api_key = "k"
default_text_model = "DeepSeek-V4-Flash"
"#,
        ),
        (
            "anthropic dialect on a custom deepseek endpoint",
            r#"
provider = "deepseek"
[providers.deepseek]
base_url = "https://llm.corp.internal/anthropic"
api_key = "k"
model = "DeepSeek-V4-Flash"
wire = "anthropic"
"#,
        ),
        (
            "openai-compatible provider table",
            r#"
provider = "openai"
[providers.openai]
base_url = "https://llm.corp.internal/v1"
api_key = "k"
model = "DeepSeek-V4-Flash"
"#,
        ),
        (
            "literal custom provider on a root base_url",
            r#"
provider = "custom"
base_url = "https://llm.corp.internal/v1"
api_key = "k"
default_text_model = "DeepSeek-V4-Flash"
"#,
        ),
    ];

    for (label, body) in shapes {
        let mut config = codewhale_runtime::config::parse_config_base(body).expect("config parses");
        normalize_model_config_for_test(&mut config);
        let provider = config.active_provider_identity().unwrap().provider;
        let base_url = config.base_url_for_route(&config.test_identity_for_kind(provider));
        assert!(
            provider_preserves_custom_base_url_model(provider, &base_url),
            "{label}: {base_url} must classify as a custom endpoint"
        );

        let stored = config
            .provider_config_for(&config.test_identity_for_kind(provider))
            .and_then(|entry| entry.model.clone())
            .or_else(|| config.default_text_model.clone())
            .unwrap_or_default();
        assert_eq!(stored, MODEL, "{label}: config load rewrote the stored id");

        let resolved = config.default_model();
        assert_eq!(resolved, MODEL, "{label}: default_model() rewrote the id");

        assert_eq!(
            wire_model_for_provider_route(provider, &base_url, &resolved),
            MODEL,
            "{label}: the wire id must match the configured id byte-for-byte"
        );

        let route = crate::route_runtime::resolve_runtime_route(&config, provider, Some(&resolved))
            .unwrap_or_else(|err| panic!("{label}: route resolution failed: {err}"));
        assert_eq!(
            route.model, MODEL,
            "{label}: the resolved route rewrote the id"
        );
    }
}

/// Reproduces the report that started the credential-resolution lane: a home
/// whose secret store holds a working DeepSeek key, where the provider picker
/// reads "missing key" while a real turn from the same home resolves that key.
///
/// The asymmetry is #5033's marker gate. For a provider that is not currently
/// active, `has_api_key_for` reads the durable slot only when some sibling's
/// `[providers.<name>]` table carries the api-key auth-mode marker. The request
/// path has no such gate — it reads the slot for whatever provider is active.
/// Any home where the marker is absent but the slot is populated (a config
/// written by an older CodeWhale, a workspace config loaded in place of the
/// user-global one, a key written straight into the store) makes the two
/// surfaces contradict each other, and nothing in the old picker output let a
/// user tell which one was lying.
///
/// This test does not change that policy — the gate is what keeps catalog
/// rendering from opening a write-capable keyring for 40 providers. It pins
/// the contradiction, and pins the part that is now fixed: the row says the
/// slot was skipped, and why.
#[test]
fn picker_and_request_path_disagree_when_the_secret_slot_marker_is_missing() -> Result<()> {
    use codewhale_runtime::credentials::CredentialSource;

    let _lock = lock_test_env();
    let temp_root = tempfile::tempdir()?;
    let temp_root = temp_root.path().canonicalize()?;
    let _guard = EnvGuard::new(&temp_root);
    let codewhale_home = temp_root.join("codewhale-home");
    let config_path = codewhale_home.join("config.toml");
    let _home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path.as_os_str());
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");

    save_api_key("deepseek-working-key")?;
    assert_eq!(
        codewhale_secrets::Secrets::auto_detect().get("deepseek")?,
        Some("deepseek-working-key".to_string()),
        "precondition: the durable slot holds a usable key"
    );
    // Drop the marker the save wrote, standing in for a home whose config was
    // written by an older CodeWhale or replaced by a workspace-scoped file.
    fs::write(&config_path, "provider = \"openrouter\"\n")?;

    let active_deepseek = {
        let __cw_config_init_32_0 = Some("deepseek".to_string());
        let mut __cw_config_init_32_value: Config = Config::default();
        __cw_config_init_32_value.provider = __cw_config_init_32_0;
        __cw_config_init_32_value
    };
    assert_eq!(
        active_deepseek.active_route_api_key().ok(),
        Some("deepseek-working-key".to_string()),
        "the request path still resolves the stored key"
    );

    let viewing_config = {
        let __cw_config_init_33_0 = Some("openrouter".to_string());
        let mut __cw_config_init_33_value: Config = Config::default();
        __cw_config_init_33_value.provider = __cw_config_init_33_0;
        __cw_config_init_33_value
    };
    assert!(
        !has_api_key_for(
            &viewing_config,
            &(viewing_config).test_identity_for_kind(ProviderKind::Deepseek)
        ),
        "the reported contradiction: readiness reports no key for the same home"
    );

    let resolution = resolve_credential_source(
        &viewing_config,
        &(viewing_config).test_identity_for_kind(ProviderKind::Deepseek),
    );
    assert!(matches!(
        resolution.source,
        CredentialSource::Missing { .. }
    ));
    let checked = resolution.checked_places();
    assert!(
        checked
            .contains("secret store \"deepseek\" (not read: inactive provider, no api-key marker)"),
        "the row must explain the skip instead of implying an empty slot: {checked}"
    );
    assert!(
        resolution.source.probed().iter().any(|probe| probe
            .fix
            .as_deref()
            .is_some_and(|fix| fix.contains("codewhale auth set"))),
        "the row must offer the command that resolves it: {resolution:?}"
    );
    println!("CAPTURED checked-places: {checked}");
    println!("CAPTURED first-fix: {:?}", resolution.first_fix());
    Ok(())
}

#[test]
fn notifications_canonical_condition_overrides_and_unsets_to_legacy() {
    let config: Config = toml::from_str("[tui]\nnotification_condition = \"never\"\n[notifications]\ncondition = \"always\"\nsound = \"off\"\ncompletion_sound = \"whale\"\n").unwrap();
    assert_eq!(
        config.notifications_config().condition,
        Some(NotificationCondition::Always)
    );
    assert_eq!(
        config.notifications_config().sound,
        Some(CompletionSound::Off)
    );
    assert_eq!(
        codewhale_notify::settings_projection(&config.notifications_config())
            .unwrap()
            .1,
        std::time::Duration::ZERO
    );
}

/// #6700: `[retry].jitter`, `jitter_factor` and `respect_retry_after` reach
/// the resolved policy (and the client's `RetryConfig`) instead of being
/// dropped for compiled-in defaults.
#[test]
fn retry_policy_reads_jitter_and_retry_after_keys() {
    let parse = |body: &str| toml::from_str::<Config>(body).expect("config parses");

    let defaults = parse("[retry]\nmax_retries = 2\n").retry_policy();
    assert!(defaults.jitter);
    assert!((defaults.jitter_factor - 0.1).abs() < f64::EPSILON);
    assert!(defaults.respect_retry_after);

    let tuned =
        parse("[retry]\njitter = false\njitter_factor = 0.3\nrespect_retry_after = false\n")
            .retry_policy();
    assert!(!tuned.jitter);
    assert!((tuned.jitter_factor - 0.3).abs() < f64::EPSILON);
    assert!(!tuned.respect_retry_after);
    let client: crate::llm_client::RetryConfig = tuned.into();
    assert!(!client.jitter);
    assert!((client.jitter_factor - 0.3).abs() < f64::EPSILON);
    assert!(!client.respect_retry_after);

    let high = parse("[retry]\njitter_factor = 7.5\n").retry_policy();
    assert!((high.jitter_factor - 1.0).abs() < f64::EPSILON);
    let negative = parse("[retry]\njitter_factor = -0.5\n").retry_policy();
    assert!(negative.jitter_factor.abs() < f64::EPSILON);
}

// Exact released enum counterpart, retained only for actual serde conservation.
// Source SHA256 a56bc5bd8b93fbb465ea4db075272746c94c38bf466821e11eeda9f5105d65ef.
mod released_provider_serde {
    use super::*;
    use serde::Serialize;
    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum ApiProvider {
        Deepseek,
        DeepseekCN,
        DeepseekAnthropic,
        NvidiaNim,
        Openai,
        Atlascloud,
        WanjieArk,
        Volcengine,
        Openrouter,
        Orcarouter,
        XiaomiMimo,
        Novita,
        Fireworks,
        Siliconflow,
        SiliconflowCn,
        Arcee,
        Moonshot,
        Sglang,
        Vllm,
        Ollama,
        OllamaCloud,
        Huggingface,
        Together,
        Qianfan,
        OpenaiCodex,
        Anthropic,
        Openmodel,
        Zai,
        Stepfun,
        Minimax,
        MinimaxAnthropic,
        Deepinfra,
        Sakana,
        LongCat,
        OpencodeGo,
        OpencodeZen,
        Meta,
        Xai,
        /// Mistral AI — la Plateforme (OpenAI-compatible Chat Completions).
        Mistral,
        /// Google Gemini — official OpenAI-compatible endpoint. A distinct
        /// backend, not an OpenAI alias: thought signatures on tool calls are
        /// captured and replayed per Google's contract.
        Google,
        /// Retired Antigravity identity retained only to deserialize and clear
        /// legacy Codewhale configuration. It is never selectable or runnable.
        Antigravity,
        /// ModelScope — Alibaba's ModelScope inference API (OpenAI-compatible).
        Modelscope,
        /// Jiangsu Telecom TokenHub — OpenAI-compatible AI gateway.
        Telecomjs,
        /// Eden AI — OpenAI-compatible AI gateway (aggregator).
        Edenai,
        /// ZenMux — OpenAI-compatible AI gateway (aggregator).
        Zenmux,
        /// CSDN 星图 — OpenAI-compatible hosted platform and Coding Plan.
        Csdn,
        /// Concentrate — OpenAI Responses-compatible AI gateway (aggregator; BYOK only).
        Concentrate,
        /// Codewhale API — account-backed model access over connected provider keys.
        Codewhale,
        /// Alibaba Cloud Model Studio — Token Plan (OpenAI-compatible Chat Completions).
        ModelstudioTokenPlan,
        /// Alibaba Cloud Model Studio — Token Plan Anthropic-compatible endpoint.
        ModelstudioTokenPlanAnthropic,
        /// Alibaba Cloud Model Studio — Coding Plan (OpenAI-compatible Chat Completions).
        ModelstudioCodingPlan,
        /// Alibaba Cloud Model Studio — Coding Plan Anthropic-compatible endpoint.
        ModelstudioCodingPlanAnthropic,
        /// User-defined OpenAI-compatible endpoint (#1519).
        ///
        /// Selected when `provider = "<name>"` names a `[providers.<name>]
        /// kind="openai-compatible"` table. A single dynamic identity that maps to
        /// [`codewhale_config::ProviderKind::Custom`] and routes via the OpenAI Chat
        /// Completions wire protocol; the concrete endpoint/model/auth come from the
        /// named config table, not from this variant.
        Custom,
    }

    #[derive(Serialize)]
    struct LegacySaved {
        provider: ApiProvider,
        provider_identity: String,
        model: String,
        receipt: crate::model_routing::AutoRouteReceipt,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effective_reasoning_effort:
            Option<codewhale_models::reasoning_effort_tier::ReasoningEffortTier>,
    }
    #[derive(Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum LegacyActivity {
        ReasoningEffortChanged {
            requested: codewhale_models::reasoning_effort_tier::ReasoningEffortTier,
            effective: codewhale_models::reasoning_effort_tier::ReasoningEffortTier,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            provider_kind: Option<ApiProvider>,
            provider: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            endpoint_identity: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            model: Option<String>,
            ts: i64,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            operation: Option<crate::work_graph::WorkNodeId>,
        },
    }
    #[derive(Serialize)]
    struct LegacyPricing {
        provider: ApiProvider,
        provider_identity: String,
        wire_model: String,
        endpoint_fingerprint: String,
        catalog_fetched_at: u64,
        catalog_revision: String,
        currency: codewhale_config::pricing::Currency,
        provenance: codewhale_config::pricing::PricingProvenance,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cloud_facts: Option<crate::provider_catalog_live::CloudFactsPricingSource>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_per_million: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_per_million: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_read_per_million: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_write_per_million: Option<String>,
    }
    fn cases() -> [(ApiProvider, ProviderKind, &'static str); 53] {
        [
            (ApiProvider::Deepseek, ProviderKind::Deepseek, "deepseek"),
            (
                ApiProvider::DeepseekCN,
                ProviderKind::Deepseek,
                "deepseek-cn",
            ),
            (
                ApiProvider::DeepseekAnthropic,
                ProviderKind::DeepseekAnthropic,
                "deepseek-anthropic",
            ),
            (
                ApiProvider::NvidiaNim,
                ProviderKind::NvidiaNim,
                "nvidia-nim",
            ),
            (ApiProvider::Openai, ProviderKind::Openai, "openai"),
            (
                ApiProvider::Atlascloud,
                ProviderKind::Atlascloud,
                "atlascloud",
            ),
            (
                ApiProvider::WanjieArk,
                ProviderKind::WanjieArk,
                "wanjie-ark",
            ),
            (
                ApiProvider::Volcengine,
                ProviderKind::Volcengine,
                "volcengine",
            ),
            (
                ApiProvider::Openrouter,
                ProviderKind::Openrouter,
                "openrouter",
            ),
            (
                ApiProvider::Orcarouter,
                ProviderKind::Orcarouter,
                "orcarouter",
            ),
            (
                ApiProvider::XiaomiMimo,
                ProviderKind::XiaomiMimo,
                "xiaomi-mimo",
            ),
            (ApiProvider::Novita, ProviderKind::Novita, "novita"),
            (ApiProvider::Fireworks, ProviderKind::Fireworks, "fireworks"),
            (
                ApiProvider::Siliconflow,
                ProviderKind::Siliconflow,
                "siliconflow",
            ),
            (
                ApiProvider::SiliconflowCn,
                ProviderKind::SiliconflowCN,
                "siliconflow-CN",
            ),
            (ApiProvider::Arcee, ProviderKind::Arcee, "arcee"),
            (ApiProvider::Moonshot, ProviderKind::Moonshot, "moonshot"),
            (ApiProvider::Sglang, ProviderKind::Sglang, "sglang"),
            (ApiProvider::Vllm, ProviderKind::Vllm, "vllm"),
            (ApiProvider::Ollama, ProviderKind::Ollama, "ollama"),
            (
                ApiProvider::OllamaCloud,
                ProviderKind::OllamaCloud,
                "ollama-cloud",
            ),
            (
                ApiProvider::Huggingface,
                ProviderKind::Huggingface,
                "huggingface",
            ),
            (ApiProvider::Together, ProviderKind::Together, "together"),
            (ApiProvider::Qianfan, ProviderKind::Qianfan, "qianfan"),
            (
                ApiProvider::OpenaiCodex,
                ProviderKind::OpenaiCodex,
                "openai-codex",
            ),
            (ApiProvider::Anthropic, ProviderKind::Anthropic, "anthropic"),
            (ApiProvider::Openmodel, ProviderKind::Openmodel, "openmodel"),
            (ApiProvider::Zai, ProviderKind::Zai, "zai"),
            (ApiProvider::Stepfun, ProviderKind::Stepfun, "stepfun"),
            (ApiProvider::Minimax, ProviderKind::Minimax, "minimax"),
            (
                ApiProvider::MinimaxAnthropic,
                ProviderKind::MinimaxAnthropic,
                "minimax-anthropic",
            ),
            (ApiProvider::Deepinfra, ProviderKind::Deepinfra, "deepinfra"),
            (ApiProvider::Sakana, ProviderKind::Sakana, "sakana"),
            (ApiProvider::LongCat, ProviderKind::LongCat, "longcat"),
            (
                ApiProvider::OpencodeGo,
                ProviderKind::OpencodeGo,
                "opencode-go",
            ),
            (
                ApiProvider::OpencodeZen,
                ProviderKind::OpencodeZen,
                "opencode-zen",
            ),
            (ApiProvider::Meta, ProviderKind::Meta, "meta"),
            (ApiProvider::Xai, ProviderKind::Xai, "xai"),
            (ApiProvider::Mistral, ProviderKind::Mistral, "mistral"),
            (ApiProvider::Google, ProviderKind::Google, "google"),
            (
                ApiProvider::Antigravity,
                ProviderKind::Antigravity,
                "antigravity",
            ),
            (
                ApiProvider::Modelscope,
                ProviderKind::Modelscope,
                "modelscope",
            ),
            (ApiProvider::Telecomjs, ProviderKind::Telecomjs, "telecomjs"),
            (ApiProvider::Edenai, ProviderKind::Edenai, "edenai"),
            (ApiProvider::Zenmux, ProviderKind::Zenmux, "zenmux"),
            (ApiProvider::Csdn, ProviderKind::Csdn, "csdn"),
            (
                ApiProvider::Concentrate,
                ProviderKind::Concentrate,
                "concentrate",
            ),
            (ApiProvider::Codewhale, ProviderKind::Codewhale, "codewhale"),
            (
                ApiProvider::ModelstudioTokenPlan,
                ProviderKind::ModelstudioTokenPlan,
                "modelstudio-token-plan",
            ),
            (
                ApiProvider::ModelstudioTokenPlanAnthropic,
                ProviderKind::ModelstudioTokenPlanAnthropic,
                "modelstudio-token-plan-anthropic",
            ),
            (
                ApiProvider::ModelstudioCodingPlan,
                ProviderKind::ModelstudioCodingPlan,
                "modelstudio-coding-plan",
            ),
            (
                ApiProvider::ModelstudioCodingPlanAnthropic,
                ProviderKind::ModelstudioCodingPlanAnthropic,
                "modelstudio-coding-plan-anthropic",
            ),
            (ApiProvider::Custom, ProviderKind::Custom, "custom"),
        ]
    }
    #[test]
    fn released_all_53_provider_wrappers_keep_actual_serde_and_revision_bytes() {
        use crate::model_routing::{
            AutoRouteDataPath, AutoRouteHeuristicReason, AutoRoutePair, AutoRouteReason,
            AutoRouteReceipt, AutoRouteScope, AutoRouteTier,
        };
        use crate::work_graph::WorkActivityEvent;
        use codewhale_config::pricing::{Currency, PricingProvenance};
        use codewhale_models::reasoning_effort_tier::ReasoningEffortTier;
        let receipt = AutoRouteReceipt {
            tier: AutoRouteTier::Selected,
            pair: AutoRoutePair {
                strong: "model-A".into(),
                fast: None,
            },
            scope: AutoRouteScope::ResolvedProvider,
            data_path: AutoRouteDataPath::LocalHeuristic,
            reason: AutoRouteReason::LocalFallback(AutoRouteHeuristicReason::DeclaredDefault),
            decision: None,
            router_failure: None,
        };
        for (legacy, provider, id) in cases() {
            let old_saved = LegacySaved {
                provider: legacy,
                provider_identity: id.into(),
                model: "model-A".into(),
                receipt: receipt.clone(),
                effective_reasoning_effort: Some(ReasoningEffortTier::High),
            };
            let saved = crate::session_manager::SavedAutoRouteReceipt {
                provider,
                provider_identity: id.into(),
                model: "model-A".into(),
                receipt: receipt.clone(),
                effective_reasoning_effort: Some(ReasoningEffortTier::High),
            };
            let expected = serde_json::to_vec(&old_saved).unwrap();
            assert_eq!(serde_json::to_vec(&saved).unwrap(), expected, "saved {id}");
            let restored: crate::session_manager::SavedAutoRouteReceipt =
                serde_json::from_slice(&expected).unwrap();
            assert_eq!(restored, saved, "saved decode {id}");
            assert_eq!(serde_json::to_vec(&restored).unwrap(), expected);
            let old_activity = LegacyActivity::ReasoningEffortChanged {
                requested: ReasoningEffortTier::High,
                effective: ReasoningEffortTier::Unavailable,
                provider_kind: Some(legacy),
                provider: id.into(),
                endpoint_identity: Some("https://example.test/v1".into()),
                model: Some("model-A".into()),
                ts: 71,
                operation: None,
            };
            let activity = WorkActivityEvent::ReasoningEffortChanged {
                requested: ReasoningEffortTier::High,
                effective: ReasoningEffortTier::Unavailable,
                provider_kind: Some(provider),
                provider: id.into(),
                endpoint_identity: Some("https://example.test/v1".into()),
                model: Some("model-A".into()),
                ts: 71,
                operation: None,
            };
            let expected = serde_json::to_vec(&old_activity).unwrap();
            assert_eq!(
                serde_json::to_vec(&activity).unwrap(),
                expected,
                "activity {id}"
            );
            let restored: WorkActivityEvent = serde_json::from_slice(&expected).unwrap();
            assert_eq!(restored, activity, "activity decode {id}");
            let input = Some("1.25".to_string());
            let output = Some("2.5".to_string());
            let empty: Option<String> = None;
            let old_payload = serde_json::to_vec(&(
                "codewhale-provider-live-pricing-quote-v1",
                legacy,
                id,
                "model-A",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                73_u64,
                Currency::Usd,
                PricingProvenance::UserOverride,
                &input,
                &output,
                &empty,
                &empty,
            ))
            .unwrap();
            let revision = format!(
                "sha256:{}",
                codewhale_runtime::hashing::sha256_hex(&old_payload)
            );
            let old_quote = LegacyPricing {
                provider: legacy,
                provider_identity: id.into(),
                wire_model: "model-A".into(),
                endpoint_fingerprint:
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                catalog_fetched_at: 73,
                catalog_revision: revision.clone(),
                currency: Currency::Usd,
                provenance: PricingProvenance::UserOverride,
                cloud_facts: None,
                input_per_million: input.clone(),
                output_per_million: output.clone(),
                cache_read_per_million: None,
                cache_write_per_million: None,
            };
            let quote = crate::provider_catalog_live::ProviderLivePricingQuote {
                provider,
                provider_identity: id.into(),
                wire_model: "model-A".into(),
                endpoint_fingerprint:
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                catalog_fetched_at: 73,
                catalog_revision: revision,
                currency: Currency::Usd,
                provenance: PricingProvenance::UserOverride,
                cloud_facts: None,
                pricing_schedule: None,
                input_per_million: input,
                output_per_million: output,
                cache_read_per_million: None,
                cache_write_per_million: None,
            };
            let expected = serde_json::to_vec(&old_quote).unwrap();
            assert_eq!(
                serde_json::to_vec(&quote).unwrap(),
                expected,
                "pricing {id}"
            );
            let restored: crate::provider_catalog_live::ProviderLivePricingQuote =
                serde_json::from_slice(&expected).unwrap();
            assert_eq!(restored, quote, "pricing decode {id}");
        }
    }
    #[test]
    fn released_classifier_data_path_retains_every_source_identity_tag() {
        #[derive(Serialize)]
        #[serde(rename_all = "snake_case")]
        enum LegacyDataPath {
            Classifier {
                provider: ApiProvider,
                model: String,
            },
        }
        for (legacy, kind, id) in cases() {
            let old = LegacyDataPath::Classifier {
                provider: legacy,
                model: "router-model".into(),
            };
            let current = crate::model_routing::AutoRouteDataPath::Classifier {
                provider: id.into(),
                provider_kind: kind,
                model: "router-model".into(),
            };
            let expected = serde_json::to_vec(&old).unwrap();
            assert_eq!(serde_json::to_vec(&current).unwrap(), expected, "{id}");
            let decoded: crate::model_routing::AutoRouteDataPath =
                serde_json::from_slice(&expected).unwrap();
            assert_eq!(decoded, current, "{id}");
        }
    }

    #[test]
    fn released_provider_wrappers_do_not_infer_brand_authority_from_custom_names() {
        use crate::work_graph::WorkActivityEvent;
        use codewhale_models::reasoning_effort_tier::ReasoningEffortTier;
        for id in ["openai", "CustomA", "customa"] {
            let event = WorkActivityEvent::ReasoningEffortChanged {
                requested: ReasoningEffortTier::High,
                effective: ReasoningEffortTier::Unavailable,
                provider_kind: Some(ProviderKind::Custom),
                provider: id.into(),
                endpoint_identity: Some("https://example.test/v1".into()),
                model: Some("model".into()),
                ts: 1,
                operation: None,
            };
            let json = serde_json::to_value(&event).unwrap();
            assert_eq!(json["provider_kind"], "custom");
            assert_eq!(
                serde_json::from_value::<WorkActivityEvent>(json).unwrap(),
                event
            );
        }
        for (tag, id) in [
            ("openai", "CustomA"),
            ("deepseek_c_n", "deepseek"),
            ("custom", ""),
            ("unknown", "openai"),
        ] {
            let malformed = serde_json::json!({"kind":"reasoning_effort_changed", "requested":"high", "effective":"high",
                "provider_kind":tag,"provider":id,"endpoint_identity":"https://example.test/v1","model":"model","ts":1});
            assert!(
                serde_json::from_value::<WorkActivityEvent>(malformed).is_err(),
                "{tag}/{id}"
            );
        }
        let legacy = serde_json::json!({"kind":"reasoning_effort_changed", "requested":"high", "effective":"high", "provider":"openai", "ts":1});
        let WorkActivityEvent::ReasoningEffortChanged {
            provider_kind,
            effective,
            ..
        } = serde_json::from_value(legacy).unwrap();
        assert!(provider_kind.is_none());
        assert_eq!(effective, ReasoningEffortTier::Unavailable);
    }
    #[test]
    fn released_all_53_effective_envelopes_keep_actual_serde_bytes() {
        #[derive(Serialize)]
        struct LegacyEnvelope {
            provider: ApiProvider,
            provider_identity: String,
            model: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            openrouter_vendor: Option<String>,
            billing_surface: Option<String>,
            endpoint_fingerprint: Option<String>,
            provider_live_pricing: Option<crate::provider_catalog_live::ProviderLivePricingQuote>,
            billing_mode: crate::cost_status::RouteBillingMode,
            dispatched_at: chrono::DateTime<chrono::Utc>,
        }
        let at = chrono::DateTime::from_timestamp(73, 0).unwrap();
        for (legacy, kind, id) in cases() {
            for vendor in [None, Some("exact-upstream".to_string())] {
                let old = LegacyEnvelope {
                    provider: legacy,
                    provider_identity: id.into(),
                    model: "model-A".into(),
                    openrouter_vendor: vendor.clone(),
                    billing_surface: None,
                    endpoint_fingerprint: None,
                    provider_live_pricing: None,
                    billing_mode: crate::cost_status::RouteBillingMode::Unknown,
                    dispatched_at: at,
                };
                let current = crate::cost_status::EffectiveRouteEnvelope {
                    provider: kind,
                    provider_identity: id.into(),
                    model: "model-A".into(),
                    openrouter_vendor: vendor,
                    billing_surface: None,
                    endpoint_fingerprint: None,
                    provider_live_pricing: None,
                    billing_mode: crate::cost_status::RouteBillingMode::Unknown,
                    dispatched_at: at,
                };
                let expected = serde_json::to_vec(&old).unwrap();
                assert_eq!(serde_json::to_vec(&current).unwrap(), expected, "{id}");
                let decoded: crate::cost_status::EffectiveRouteEnvelope =
                    serde_json::from_slice(&expected).unwrap();
                assert_eq!(decoded, current, "decode {id}");
                assert_eq!(
                    serde_json::to_vec(&decoded).unwrap(),
                    expected,
                    "reencode {id}"
                );
            }
        }
    }

    #[test]
    fn china_wire_tag_config_leaf_and_route_identity_are_distinct_contracts() {
        let config = {
            let __cw_config_init_34_0 = Some("deepseek-cn".into());
            let mut __cw_config_init_34_value: Config = Default::default();
            __cw_config_init_34_value.provider = __cw_config_init_34_0;
            __cw_config_init_34_value
        };
        let identity = config.active_provider_identity().unwrap();
        assert_eq!(identity.provider, ProviderKind::Deepseek);
        assert_eq!(identity.key.as_str(), "deepseek-cn");
        assert_eq!(identity.persisted_kind(), "deepseek-cn");
        assert_eq!(identity.persisted_id(), Some("deepseek-cn"));
        assert_eq!(identity.config_table_key().unwrap(), "deepseek_cn");
        assert_eq!(provider_config_key(&identity).unwrap(), "deepseek_cn");
        assert_eq!(
            serde_json::to_string(&ApiProvider::DeepseekCN).unwrap(),
            "\"deepseek_c_n\""
        );
        assert_eq!(
            identity.compatibility().unwrap().tui_wire_tag,
            "deepseek_c_n"
        );
        assert!(
            provider_config_key(&Config::default().active_provider_identity().unwrap()).is_err()
        );
    }

    #[test]
    fn classifier_exact_custom_receipts_keep_case_and_refuse_contradictions() {
        use crate::model_routing::AutoRouteDataPath;
        for id in ["openai", "CustomA", "customa"] {
            let source = AutoRouteDataPath::Classifier {
                provider: id.into(),
                provider_kind: ProviderKind::Custom,
                model: "router-model".into(),
            };
            let value = serde_json::to_value(&source).unwrap();
            assert_eq!(value["classifier"]["provider"], "custom");
            assert_eq!(value["classifier"]["provider_id"], id);
            assert_eq!(
                serde_json::from_value::<AutoRouteDataPath>(value).unwrap(),
                source
            );
        }
        for value in [
            serde_json::json!({"classifier":{"provider":"openai","provider_id":"CustomA","model":"router-model"}}),
            serde_json::json!({"classifier":{"provider":"custom","provider_id":"","model":"router-model"}}),
            serde_json::json!({"classifier":{"provider":"unknown","model":"router-model"}}),
        ] {
            assert!(serde_json::from_value::<AutoRouteDataPath>(value).is_err());
        }
    }
}
