use codewhale_config::ProviderKind;
use codewhale_engine::route_preferences::persist_provider_selection;

use codewhale_runtime::config_persistence::*;

use codewhale_runtime::config_persistence::test_support::*;
use std::fs;

#[test]
fn route_migration_is_atomic_preserves_conflicts_and_ignores_scoped_settings() {
    use crate::test_support::{EnvVarGuard, lock_test_env};
    let _lock = lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let _scope = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", project.path().join("config.toml"));
    let path = home.path().join("config.toml");
    let source = "# keep this comment\nprovider = 'deepseek'\n[providers.zai]\nmodel = 'GLM-5.2'\n[providers.TeamA]\nkind = 'openai-compatible'\nbase_url = 'https://upper.example.test/v1'\nmodel = 'Old-Upper'\n[providers.teama]\nkind = 'openai-compatible'\nbase_url = 'https://lower.example.test/v1'\nmodel = 'Old-Lower'\n";
    let legacy = "default_provider = 'zai'\n[provider_models]\nzai = 'GLM-5.3'\nTeamA = 'New-Upper'\nteama = 'New-Lower'\n";
    fs::write(&path, source).unwrap();
    fs::write(home.path().join("settings.toml"), legacy).unwrap();
    fs::write(
        project.path().join("settings.toml"),
        "default_provider = 'openai'\n[provider_models]\nzai = 'wrong-scoped-model'\n",
    )
    .unwrap();

    let admitted_source = format!(
        "{source}\n[providers.Missing]\nkind = 'openai-compatible'\nbase_url = 'https://missing.example.test/v1'\n"
    );
    let captured_missing = codewhale_runtime::config::parse_config_base(&admitted_source)
        .unwrap()
        .resolve_provider_pin_identity("Missing")
        .unwrap();
    let error = persist_provider_selection(Some(&path), &captured_missing, Some("new-model"));
    assert!(error.is_err());
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        source,
        "failed save must not commit migration separately"
    );

    persist_provider_selection(
        Some(&path),
        &selection_identity(&path, "deepseek"),
        Some("deepseek-v4-pro"),
    )
    .unwrap();
    let body = fs::read_to_string(&path).unwrap();
    let doc: toml::Value = toml::from_str(&body).unwrap();
    assert!(body.contains("# keep this comment"));
    assert_eq!(doc["route_preferences_version"].as_integer(), Some(1));
    assert_eq!(doc["provider"].as_str(), Some("deepseek"));
    assert_eq!(
        doc["providers"]["deepseek"]["model"].as_str(),
        Some("deepseek-v4-pro")
    );
    assert_eq!(doc["providers"]["zai"]["model"].as_str(), Some("GLM-5.3"));
    assert_eq!(
        doc["providers"]["TeamA"]["model"].as_str(),
        Some("New-Upper")
    );
    assert_eq!(
        doc["providers"]["teama"]["model"].as_str(),
        Some("New-Lower")
    );
    assert_eq!(
        doc["providers"]["TeamA"]["base_url"].as_str(),
        Some("https://upper.example.test/v1")
    );
    assert_eq!(
        doc["route_preferences_migration"]["previous_models"]["zai"].as_str(),
        Some("GLM-5.2")
    );
    assert_eq!(
        fs::read_to_string(home.path().join("settings.toml")).unwrap(),
        legacy
    );

    persist_provider_model_key(Some(&path), &selection_identity(&path, "zai"), "GLM-5.1").unwrap();
    let doc: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(doc["providers"]["zai"]["model"].as_str(), Some("GLM-5.1"));
    assert_eq!(
        doc["route_preferences_migration"]["previous_models"]["zai"].as_str(),
        Some("GLM-5.2")
    );
}

#[test]
fn moving_root_default_off_the_root_does_not_resurrect_a_shadowed_legacy_model() {
    use crate::test_support::{EnvVarGuard, lock_test_env};
    let _lock = lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let _model_guard = ModelEnvGuard::new();
    let path = home.path().join("config.toml");
    // `default_text_model` shadows the legacy root `model` on every route.
    // Moving the alias onto openai's leaf must not un-shadow the legacy
    // value for the pass-through incoming route.
    fs::write(
            &path,
            "route_preferences_version = 1\nprovider = 'openai'\nmodel = 'deepseek-v4-flash'\ndefault_text_model = 'gpui-fixture'\n",
        )
        .unwrap();
    persist_provider_selection(Some(&path), &selection_identity(&path, "openrouter"), None)
        .unwrap();
    let doc: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!(doc.get("default_text_model").is_none(), "{doc:?}");
    assert!(doc.get("model").is_none(), "{doc:?}");
    assert_eq!(
        doc["providers"]["openai"]["model"].as_str(),
        Some("gpui-fixture")
    );
    let switched = codewhale_plugins::providers::load_config(Some(path.clone()), None).unwrap();
    assert_eq!(
        switched.active_provider_identity().unwrap().provider,
        ProviderKind::Openrouter
    );
    // OpenRouter normalizes the stale value to `deepseek/deepseek-v4-flash`.
    assert!(
        !switched.default_model().contains("deepseek-v4-flash"),
        "{}",
        switched.default_model()
    );
    assert_ne!(switched.default_model(), "gpui-fixture");

    persist_provider_selection(Some(&path), &selection_identity(&path, "openai"), None).unwrap();
    let returned = codewhale_plugins::providers::load_config(Some(path.clone()), None).unwrap();
    assert_eq!(
        returned.active_provider_identity().unwrap().provider,
        ProviderKind::Openai
    );
    assert_eq!(returned.default_model(), "gpui-fixture");
}

#[test]
fn switching_provider_away_and_back_preserves_every_route_selection() {
    use crate::test_support::{EnvVarGuard, lock_test_env};
    let _lock = lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let _model_guard = ModelEnvGuard::new();
    let path = home.path().join("config.toml");
    for root_key in ["default_text_model", "model"] {
        for existing in [None, Some("GLM-4.5-Air")] {
            let mut source = format!(
                "route_preferences_version = 1\nprovider = 'zai'\n{root_key} = 'GLM-4.6'\n"
            );
            if let Some(model) = existing {
                source.push_str(&format!("[providers.zai]\nmodel = '{model}'\n"));
            }
            fs::write(&path, source).unwrap();
            persist_provider_selection(
                Some(&path),
                &selection_identity(&path, "deepseek"),
                Some("deepseek-v4-pro"),
            )
            .unwrap();
            let doc: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            // The outgoing route was resolving the root alias only when it
            // had no leaf; then the choice moves onto that leaf. Otherwise
            // the alias is inert saved state and stays. Neither case may
            // delete it.
            if existing.is_none() {
                assert!(doc.get(root_key).is_none(), "{doc:?}");
                assert_eq!(
                    doc["providers"]["zai"]["model"].as_str(),
                    Some("GLM-4.6"),
                    "the outgoing route's choice must follow it onto its leaf"
                );
            } else {
                assert_eq!(
                    doc[root_key].as_str(),
                    Some("GLM-4.6"),
                    "an inert root fallback must survive the switch"
                );
            }
            assert_eq!(
                doc["providers"]["deepseek"]["model"].as_str(),
                Some("deepseek-v4-pro")
            );
            let restored = codewhale_plugins::providers::load_config(Some(path.clone()), None)
                .expect("saved provider switch must remain loadable");
            assert_eq!(
                restored.active_provider_identity().unwrap().provider,
                ProviderKind::Deepseek
            );
            assert_eq!(restored.default_model(), "deepseek-v4-pro");

            // Switching back must find the choice this route had, whether
            // it was stored on its own leaf or in the root fallback.
            persist_provider_selection(Some(&path), &selection_identity(&path, "zai"), None)
                .unwrap();
            let returned = codewhale_plugins::providers::load_config(Some(path.clone()), None)
                .expect("switching back must remain loadable");
            assert_eq!(
                returned.active_provider_identity().unwrap().provider,
                ProviderKind::Zai
            );
            assert_eq!(
                returned.default_model(),
                existing.unwrap_or("GLM-4.6"),
                "switching away and back must restore the outgoing selection"
            );
        }
    }
}

#[test]
fn a_bare_switch_relocates_a_root_alias_the_incoming_route_cannot_serve() {
    use crate::test_support::{EnvVarGuard, lock_test_env};
    let _lock = lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let _model_guard = ModelEnvGuard::new();
    let path = home.path().join("config.toml");
    // No model argument, and the outgoing route keeps its only model in the
    // root fallback. Official DeepSeek cannot serve that id, so leaving it
    // behind would commit a switch whose config no longer loads.
    fs::write(
            &path,
            "route_preferences_version = 1\nprovider = 'volcengine'\ndefault_text_model = 'ark-private-id'\n",
        )
        .unwrap();
    persist_provider_selection(Some(&path), &selection_identity(&path, "deepseek"), None).unwrap();

    let doc: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        doc.get("default_text_model").is_none(),
        "an alias the incoming route cannot serve must not be left behind"
    );
    assert_eq!(
        doc["providers"]["volcengine"]["model"].as_str(),
        Some("ark-private-id"),
        "the displaced choice moves onto its own route's leaf, it is not dropped"
    );
    codewhale_plugins::providers::load_config(Some(path.clone()), None)
        .expect("a bare provider switch must remain loadable");

    persist_provider_selection(Some(&path), &selection_identity(&path, "volcengine"), None)
        .unwrap();
    let returned = codewhale_plugins::providers::load_config(Some(path.clone()), None)
        .expect("switching back must remain loadable");
    assert_eq!(
        returned.active_provider_identity().unwrap().provider,
        ProviderKind::Volcengine
    );
    assert_eq!(returned.default_model(), "ark-private-id");
}

#[test]
fn bare_switch_from_legacy_custom_commits_a_loadable_config() {
    use crate::test_support::{EnvVarGuard, lock_test_env};
    let _lock = lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let _model_guard = ModelEnvGuard::new();
    let path = home.path().join("config.toml");
    let original = "route_preferences_version = 1\nprovider = 'custom'\nbase_url = 'https://proxy.example.test/v1'\ndefault_text_model = 'proxy-wire-id'\n[providers.deepseek]\nbase_url = 'https://api.deepseek.com/beta'\n";
    fs::write(&path, original).unwrap();
    codewhale_plugins::providers::load_config(Some(path.clone()), None).unwrap();
    // The write moves the top-level route into `[providers.custom]`,
    // model included (#6394), so the custom model is no longer the
    // switch's only copy and a bare switch commits a loadable config.
    persist_provider_selection(Some(&path), &selection_identity(&path, "deepseek"), None).unwrap();
    let switched = codewhale_plugins::providers::load_config(Some(path.clone()), None)
        .expect("a bare provider switch must remain loadable");
    assert_eq!(
        switched.active_provider_identity().unwrap().provider,
        ProviderKind::Deepseek
    );
    let doc: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        doc["providers"]["custom"]["model"].as_str(),
        Some("proxy-wire-id")
    );
    assert_eq!(
        doc["providers"]["custom"]["base_url"].as_str(),
        Some("https://proxy.example.test/v1")
    );
}

#[test]
fn an_unnamed_custom_route_keeps_its_root_model_across_a_switch() {
    use crate::test_support::{EnvVarGuard, lock_test_env};
    let _lock = lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let _model_guard = ModelEnvGuard::new();
    let path = home.path().join("config.toml");
    // The one-way root migration copies the model to the exact custom
    // leaf. A switch with its own model retains the historical root alias
    // too; both choices survive without granting missing-id table authority.
    fs::write(
            &path,
            "route_preferences_version = 1\nprovider = 'custom'\nbase_url = 'https://proxy.example.test/v1'\ndefault_text_model = 'proxy-wire-id'\n",
        )
        .unwrap();
    persist_provider_selection(
        Some(&path),
        &selection_identity(&path, "deepseek"),
        Some("deepseek-v4-pro"),
    )
    .unwrap();
    let doc: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        doc["default_text_model"].as_str(),
        Some("proxy-wire-id"),
        "the custom route's only saved model must survive the switch"
    );
    let restored = codewhale_plugins::providers::load_config(Some(path.clone()), None)
        .expect("the switched config must remain loadable");
    assert_eq!(restored.default_model(), "deepseek-v4-pro");

    // Nothing can re-select an unnamed custom route by identity once the
    // selector names another provider, so restoring it is a `provider`
    // edit. The model has to still be there when it happens.
    let returning = fs::read_to_string(&path)
        .unwrap()
        .replace("provider = 'deepseek'", "provider = 'custom'")
        .replace("provider = \"deepseek\"", "provider = 'custom'");
    fs::write(&path, returning).unwrap();
    let returned = codewhale_plugins::providers::load_config(Some(path.clone()), None)
        .expect("returning to the custom route must remain loadable");
    assert_eq!(returned.default_model(), "proxy-wire-id");
}

#[test]
fn canonical_model_writer_targets_the_literal_custom_table_and_exact_named_ids() {
    use crate::test_support::{EnvVarGuard, lock_test_env};
    let _lock = lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let path = home.path().join("config.toml");
    fs::write(&path, "route_preferences_version = 1\nprovider = 'custom'\nbase_url = 'https://legacy.example.test/v1'\ndefault_text_model = 'old-wire-id'\n").unwrap();
    persist_provider_selection(
        Some(&path),
        &selection_identity(&path, "custom"),
        Some("Exact-New-ID"),
    )
    .unwrap();
    // The literal route is the `[providers.custom]` table since #6394.
    let doc: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        doc["providers"]["custom"]["model"].as_str(),
        Some("Exact-New-ID")
    );
    assert!(doc.get("base_url").is_none());

    fs::write(&path, "route_preferences_version = 1\nprovider = 'Team.A'\n[providers.'Team.A']\nkind = 'openai-compatible'\nbase_url = 'https://named.example.test/v1'\nmodel = 'old'\n").unwrap();
    persist_provider_selection(
        Some(&path),
        &selection_identity(&path, "Team.A"),
        Some("Exact-Named-ID"),
    )
    .unwrap();
    let doc: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        doc["providers"]["Team.A"]["model"].as_str(),
        Some("Exact-Named-ID")
    );
    let admitted_lower = codewhale_runtime::config::parse_config_base(
            "provider = 'team.a'\n[providers.'team.a']\nkind = 'openai-compatible'\nbase_url = 'https://lower.example.test/v1'\n"
        ).unwrap().resolve_provider_pin_identity("team.a").unwrap();
    let before_wrong_case = fs::read_to_string(&path).unwrap();
    assert!(persist_provider_selection(Some(&path), &admitted_lower, Some("wrong")).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), before_wrong_case);
    persist_provider_model_key(
        Some(&path),
        &selection_identity(&path, "deepseek-cn"),
        "deepseek-v4-pro",
    )
    .unwrap();
    let doc: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        doc["providers"]["deepseek_cn"]["model"].as_str(),
        Some("deepseek-v4-pro")
    );
}

#[test]
fn malformed_legacy_preferences_do_not_partially_commit_a_route_save() {
    use crate::test_support::{EnvVarGuard, lock_test_env};
    let _lock = lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let path = home.path().join("config.toml");
    let source = "provider = 'zai'\n[providers.zai]\nmodel = 'GLM-5.2'\n";
    fs::write(&path, source).unwrap();
    let settings_path = home.path().join("settings.toml");
    let malformed = "default_provider = [\n";
    fs::write(&settings_path, malformed).unwrap();
    let error = persist_provider_selection(
        Some(&path),
        &selection_identity(&path, "zai"),
        Some("GLM-5.3"),
    )
    .expect_err("unreadable legacy preferences must block the entire save");
    assert!(error.to_string().contains("configuration was not changed"));
    assert_eq!(fs::read_to_string(&path).unwrap(), source);
    assert_eq!(fs::read_to_string(&settings_path).unwrap(), malformed);
}
#[test]
fn fresh_locked_root_migration_admits_only_the_captured_table_and_keeps_undo_cas() {
    let _lock = crate::test_support::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let path = home.path().join("config.toml");
    let original = "route_preferences_version = 1\nprovider = 'custom'\nbase_url = 'http://127.0.0.1:18180/v1'\ndefault_text_model = 'old-model'\n";
    fs::write(&path, original).unwrap();
    let captured = codewhale_runtime::config::parse_config_base(original)
        .unwrap()
        .active_provider_identity()
        .unwrap();
    assert!(captured.persisted_id().is_none());
    let (_, undo) =
        persist_provider_selection(Some(&path), &captured, Some("selected-model")).unwrap();
    let saved = fs::read_to_string(&path).unwrap();
    let parsed = codewhale_runtime::config::parse_config_base(&saved).unwrap();
    assert_eq!(parsed.default_model(), "selected-model");
    assert_eq!(
        parsed
            .provider_config_for(&parsed.active_provider_identity().unwrap())
            .unwrap()
            .model
            .as_deref(),
        Some("selected-model")
    );
    let newer = saved.replace("selected-model", "newer-model");
    fs::write(&path, &newer).unwrap();
    assert!(!undo.undo().unwrap());
    assert_eq!(fs::read_to_string(&path).unwrap(), newer);
}

#[test]
fn locked_root_write_refuses_table_only_profile_conflict_and_changed_generation() {
    let _lock = crate::test_support::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let path = home.path().join("config.toml");
    let root = "route_preferences_version = 1\nprovider = 'custom'\nbase_url = 'http://127.0.0.1:18180/v1'\ndefault_text_model = 'old-model'\n";
    let captured = codewhale_runtime::config::parse_config_base(root)
        .unwrap()
        .active_provider_identity()
        .unwrap();
    for source in [
        "route_preferences_version = 1\nprovider = 'custom'\n[providers.custom]\nbase_url = 'http://127.0.0.1:18180/v1'\nmodel = 'old-model'\n",
        "route_preferences_version = 1\nprovider = 'deepseek'\n[profiles.dev]\nprovider = 'custom'\nbase_url = 'http://127.0.0.1:18180/v1'\ndefault_text_model = 'old-model'\n",
        "route_preferences_version = 1\nprovider = 'custom'\nbase_url = 'http://127.0.0.1:18180/v1'\ndefault_text_model = 'old-model'\n[providers.custom]\nbase_url = 'http://127.0.0.1:19191/v1'\n",
        "route_preferences_version = 1\nprovider = 'custom'\nbase_url = 'http://127.0.0.1:18180/v1'\ndefault_text_model = 'changed-model'\n",
        "route_preferences_version = 1\nprovider = 'custom'\nbase_url = 'http://127.0.0.1:18180/v1'\ndefault_text_model = 'old-model'\napi_key = 'changed-key'\n",
    ] {
        fs::write(&path, source).unwrap();
        assert!(persist_provider_selection(Some(&path), &captured, Some("must-not-save")).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), source);
        assert!(persist_provider_model_key(Some(&path), &captured, "must-not-save").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), source);
        assert!(
            persist_route_base_url(Some(&path), &captured, "http://127.0.0.1:22222/v1").is_err()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), source);
    }
}
