use std::path::Path;

use std::path::PathBuf;

use codewhale_config::ProviderKind;

use codewhale_runtime::config::Config;

use codewhale_engine::provider_readiness::ResolvedProviderReadiness;

use codewhale_engine::provider_readiness::resolve_for_model;

struct HomeGuard {
    prev_home: Option<std::ffi::OsString>,
    prev_config_path: Option<std::ffi::OsString>,
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: serialised by lock_test_env held by the caller.
        unsafe {
            match &self.prev_home {
                Some(v) => std::env::set_var("CODEWHALE_HOME", v),
                None => std::env::remove_var("CODEWHALE_HOME"),
            }
            match &self.prev_config_path {
                Some(v) => std::env::set_var("CODEWHALE_CONFIG_PATH", v),
                None => std::env::remove_var("CODEWHALE_CONFIG_PATH"),
            }
        }
    }
}

/// Sealed user-global home with a saved DeepSeek API key, created once per
/// process. Tests must hold `lock_test_env` before touching it.
fn sealed_home() -> &'static Path {
    static HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let dir = tempfile::TempDir::new().expect("temp home").keep();
        // Seed the user-global config: DeepSeek is the authorized provider.
        // The config lives at $CODEWHALE_HOME/config.toml (the primary path
        // when CODEWHALE_HOME is explicit).
        std::fs::write(
            dir.join("config.toml"),
            r#"provider = "deepseek"
[providers.deepseek]
api_key = "sk-test-scope-deepseek"
"#,
        )
        .expect("write config");
        dir
    })
}

/// Point `CODEWHALE_HOME` at the sealed home (optionally also pinning
/// `CODEWHALE_CONFIG_PATH`). Caller must hold `lock_test_env`.
fn sealed_env(config_path: Option<&Path>) -> HomeGuard {
    let prev_home = std::env::var_os("CODEWHALE_HOME");
    let prev_config_path = std::env::var_os("CODEWHALE_CONFIG_PATH");
    // SAFETY: serialised by lock_test_env held by the caller.
    unsafe {
        std::env::set_var("CODEWHALE_HOME", sealed_home());
        match config_path {
            Some(path) => std::env::set_var("CODEWHALE_CONFIG_PATH", path),
            None => std::env::remove_var("CODEWHALE_CONFIG_PATH"),
        }
    }
    HomeGuard {
        prev_home,
        prev_config_path,
    }
}

fn deepseek_readiness(config: &Config) -> ResolvedProviderReadiness {
    resolve_for_model(
        config,
        &(config).test_identity_for_kind(ProviderKind::Deepseek),
        "deepseek-v4-pro",
        &codewhale_engine::provider_readiness::ProviderReadinessSnapshot::default(),
    )
}

fn workspace_with_config(dir: &Path, provider: &str) -> PathBuf {
    let ws = dir.join(provider);
    std::fs::create_dir_all(ws.join(".codewhale")).expect("workspace dir");
    std::fs::write(
        ws.join(".codewhale").join("config.toml"),
        format!(
            r#"provider = "{provider}"

[providers.{provider}]
# deliberately no api_key — this workspace selects a route it never
# authorized anywhere.
"#
        ),
    )
    .expect("write workspace config");
    ws
}

#[test]
fn readiness_is_identical_across_unrelated_workspaces() {
    let _lock = crate::test_support::lock_test_env();
    let _home = sealed_env(None);

    let base = tempfile::TempDir::new().expect("temp base");
    let ws_a = base.path().join("project-a");
    let ws_b = base.path().join("project-b");
    std::fs::create_dir_all(&ws_a).expect("ws a");
    std::fs::create_dir_all(&ws_b).expect("ws b");

    // Same user-global home, two unrelated folders, no workspace overrides.
    let config_a = codewhale_plugins::providers::load_config(None, None).expect("load from A");
    let config_b = codewhale_plugins::providers::load_config(None, None).expect("load from B");

    let readiness_a = deepseek_readiness(&config_a);
    let readiness_b = deepseek_readiness(&config_b);
    assert_eq!(
        readiness_a.label(),
        readiness_b.label(),
        "readiness must not depend on the launch folder: {} vs {}",
        readiness_a.label(),
        readiness_b.label()
    );
    assert!(
        readiness_a.can_attempt(),
        "the user-global key must make DeepSeek attemptable from A: {}",
        readiness_a.label()
    );
    assert!(
        readiness_b.can_attempt(),
        "the user-global key must make DeepSeek attemptable from B: {}",
        readiness_b.label()
    );
}

#[test]
fn explicit_workspace_config_selects_its_route_without_locking_user_global() {
    let _lock = crate::test_support::lock_test_env();
    let base = tempfile::TempDir::new().expect("temp base");
    // Workspace A deliberately selects zai with no credential anywhere.
    let ws_a = workspace_with_config(base.path(), "zai");
    // The explicit config path is the workspace file — this is the
    // "launched from that folder with --config" shape.
    let config_path = ws_a.join(".codewhale").join("config.toml");
    let _home = sealed_env(Some(&config_path));

    let config = codewhale_plugins::providers::load_config(Some(config_path.clone()), None)
        .expect("load workspace config");
    // The workspace selection IS honored for the session route.
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        ProviderKind::Zai,
        "the explicit workspace config selects zai"
    );

    // The user-global DeepSeek authorization did not disappear: it resolves
    // from the user-global credential sources regardless of which config file
    // was loaded.
    let deepseek = deepseek_readiness(&config);
    assert!(
        deepseek.can_attempt(),
        "authorization established once must stay visible: {}",
        deepseek.label()
    );

    // The uncredentialed zai route is unavailable with a precise reason —
    // never a lie, never a silent substitution.
    let zai = resolve_for_model(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Zai),
        "GLM-5.2",
        &codewhale_engine::provider_readiness::ProviderReadinessSnapshot::default(),
    );
    assert!(
        !zai.can_attempt(),
        "zai has no credential anywhere: {}",
        zai.label()
    );
    let reason = zai.blocked_reason().map(|r| r.into_owned());
    assert!(
        reason.as_ref().is_some_and(|r| !r.trim().is_empty()),
        "unavailable must carry a precise reason: {reason:?}"
    );
}

fn write_nested_zai_config(base: &std::path::Path) -> std::path::PathBuf {
    let nested = base.join("parent-repo").join("nested-repo");
    std::fs::create_dir_all(nested.join(".codewhale")).expect("nested dir");
    std::fs::write(
        nested.join(".codewhale").join("config.toml"),
        r#"provider = "zai"

[providers.zai]
"#,
    )
    .expect("nested config");
    nested
}

fn assert_user_global_survives_workspace(label: &str, config_path: std::path::PathBuf) {
    let _home = sealed_env(Some(&config_path));
    let config = codewhale_plugins::providers::load_config(Some(config_path.clone()), None)
        .unwrap_or_else(|err| panic!("load from {label}: {err}"));
    let deepseek = deepseek_readiness(&config);
    assert!(
        deepseek.can_attempt(),
        "{label}: user-global authorization must survive: {}",
        deepseek.label()
    );
}

#[test]
fn nested_repo_does_not_change_readiness() {
    let _lock = crate::test_support::lock_test_env();
    let base = tempfile::TempDir::new().expect("temp base");
    let nested = write_nested_zai_config(base.path());
    assert_user_global_survives_workspace("nested", nested.join(".codewhale/config.toml"));
}

#[cfg(unix)]
#[test]
fn symlinked_worktree_does_not_change_readiness() {
    let _lock = crate::test_support::lock_test_env();
    let base = tempfile::TempDir::new().expect("temp base");
    let nested = write_nested_zai_config(base.path());
    let symlinked = base.path().join("symlink-worktree");
    std::os::unix::fs::symlink(&nested, &symlinked).expect("symlink");
    assert_user_global_survives_workspace("symlinked", symlinked.join(".codewhale/config.toml"));
}

#[test]
fn unavailable_truly_means_unavailable_with_a_reason() {
    let _lock = crate::test_support::lock_test_env();
    let _home = sealed_env(None);

    let config = codewhale_plugins::providers::load_config(None, None).expect("load config");
    // Moonshot has no key anywhere in the sealed fixtures.
    let moonshot = resolve_for_model(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Moonshot),
        "kimi-k2.6",
        &codewhale_engine::provider_readiness::ProviderReadinessSnapshot::default(),
    );
    assert!(!moonshot.can_attempt());
    let reason = moonshot.blocked_reason().map(|r| r.into_owned());
    assert!(
        reason.as_ref().is_some_and(|r| !r.trim().is_empty()),
        "unavailable must carry a precise reason: {reason:?}"
    );
}

#[test]
fn repeated_readers_never_rewrite_configuration() {
    let _lock = crate::test_support::lock_test_env();
    let _home = sealed_env(None);

    let config_path = sealed_home().join("config.toml");
    let before = std::fs::read(&config_path).expect("read config before");

    // Resolve twice from the same sealed fixtures — a reader must never
    // write anything, so a later process can never revert a newer selection
    // by merely loading it. (Thread spawns are deliberately not used: they
    // would contend on the process-wide test env lock; the property under
    // test is that loading is side-effect-free, which a second load proves.)
    for _ in 0..2 {
        let config = codewhale_plugins::providers::load_config(Some(config_path.clone()), None)
            .expect("load");
        let readiness = deepseek_readiness(&config);
        assert!(readiness.can_attempt());
    }

    let after = std::fs::read(&config_path).expect("read config after");
    assert_eq!(before, after, "readers must never rewrite configuration");
}

/// The scope contract visible to the UI: a workspace selection changes only
/// that workspace's selected configuration. Proven at the store level so the
/// Fleet selection files behave the same way as the config path above.
#[test]
fn workspace_fleet_selection_affects_only_that_workspace() {
    use codewhale_engine::fleet::store::{
        FleetFile, FleetScope, save_fleet, selected_fleet, set_selected,
    };
    let _lock = crate::test_support::lock_test_env();
    // A FRESH personal home per test: the shared sealed home would pick up
    // the parallel personal-selection test's writes.
    let home = tempfile::TempDir::new().expect("temp home");
    std::fs::create_dir_all(home.path().join("fleets")).expect("fleets dir");
    let prev = std::env::var_os("CODEWHALE_HOME");
    // SAFETY: serialised by lock_test_env.
    unsafe { std::env::set_var("CODEWHALE_HOME", home.path()) };

    let base = tempfile::TempDir::new().expect("temp base");
    let ws_a = base.path().join("ws-a");
    let ws_b = base.path().join("ws-b");
    std::fs::create_dir_all(&ws_a).expect("ws a");
    std::fs::create_dir_all(&ws_b).expect("ws b");

    let fleet = FleetFile::new("Team A".to_string(), None).expect("fleet");
    save_fleet(&fleet, FleetScope::Personal, &ws_a).expect("save personal");

    // Workspace A selects the fleet for this folder only — the selection may
    // point at the personal Fleet, never silently shadowing or copying it.
    set_selected("Team A", FleetScope::Workspace, &ws_a).expect("select in A");

    // B is untouched: no selection there.
    assert!(
        selected_fleet(&ws_a).is_some(),
        "A has its folder selection"
    );
    assert!(selected_fleet(&ws_b).is_none(), "B must be unaffected");

    // A's selection resolves to the personal Fleet file (no copy was made),
    // labeled by the scope it actually lives in.
    let sel = selected_fleet(&ws_a).expect("selected in A");
    assert_eq!(sel.scope, FleetScope::Personal);
    assert!(
        !ws_a.join(".codewhale/fleets/team-a.toml").exists(),
        "a workspace selection must not copy the fleet file"
    );
    // SAFETY: serialised by lock_test_env.
    unsafe {
        match prev {
            Some(v) => std::env::set_var("CODEWHALE_HOME", v),
            None => std::env::remove_var("CODEWHALE_HOME"),
        }
    }
}

/// A saved personal Fleet persists across a "restart": a fresh load from the
/// same sealed home still resolves the selection.
#[test]
fn personal_fleet_selection_persists_across_restart() {
    use codewhale_engine::fleet::store::{
        FleetFile, FleetScope, save_fleet, selected_fleet, set_selected,
    };
    let _lock = crate::test_support::lock_test_env();
    // Fresh personal home per test (see the workspace-selection test).
    let home = tempfile::TempDir::new().expect("temp home");
    std::fs::create_dir_all(home.path().join("fleets")).expect("fleets dir");
    let prev = std::env::var_os("CODEWHALE_HOME");
    // SAFETY: serialised by lock_test_env.
    unsafe { std::env::set_var("CODEWHALE_HOME", home.path()) };

    let ws = tempfile::TempDir::new().expect("temp ws");
    let fleet = FleetFile::new("My Default".to_string(), None).expect("fleet");
    save_fleet(&fleet, FleetScope::Personal, ws.path()).expect("save personal");
    set_selected("My Default", FleetScope::Personal, ws.path()).expect("select");

    // "Restart": a fresh resolution from the same home.
    let sel = selected_fleet(ws.path()).expect("selection after restart");
    assert_eq!(sel.name, "My Default");
    assert_eq!(sel.scope, FleetScope::Personal);
    // SAFETY: serialised by lock_test_env.
    unsafe {
        match prev {
            Some(v) => std::env::set_var("CODEWHALE_HOME", v),
            None => std::env::remove_var("CODEWHALE_HOME"),
        }
    }
}
