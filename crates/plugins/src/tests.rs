use std::fs;
use std::path::{Path, PathBuf};

use super::activation::PluginActivationCapability;
use super::discovery::{DiscoveryConfig, discover_with_config};
use super::managed_policy::{
    MANAGED_POLICY_FILE_NAME, MANAGED_POLICY_PATH_ENV, MANAGED_POLICY_SCHEMA_VERSION,
    ManagedPluginPolicy,
};
use super::manifest::PluginCompatibility;
use super::types::{PluginDiagnosticLevel, PluginTrustStatus};

fn config(root: &Path) -> DiscoveryConfig {
    DiscoveryConfig {
        workspace: root.join("project"),
        user_plugins_dir: root.join("user"),
        workspace_plugins_dir: root.join("workspace"),
        builtin_plugin_dirs: Vec::new(),
        state_path: root.join("state/plugin-state.json"),
    }
}

fn write_plugin(config: &DiscoveryConfig, extra: &str) -> PathBuf {
    write_named_plugin(config, "demo", extra)
}

fn write_named_plugin(config: &DiscoveryConfig, name: &str, extra: &str) -> PathBuf {
    let plugin = config.user_plugins_dir.join(name);
    fs::create_dir_all(&plugin).unwrap();
    fs::write(
        plugin.join("plugin.toml"),
        format!("schema_version = 1\n[plugin]\nname = {name:?}\nversion = \"1.0.0\"\n{extra}"),
    )
    .unwrap();
    plugin
}

#[test]
fn on_disk_catalog_change_nudges_reload_once_until_rediscover() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "");

    let registry = discover_with_config(&config);
    assert!(
        !registry.on_disk_catalog_changed(),
        "fresh discovery must match the on-disk stamp"
    );

    let mut last_nudged = None;
    assert!(
        crate::plugin_reload_nudge(&registry, &mut last_nudged).is_none(),
        "unchanged catalog must not nudge"
    );

    write_named_plugin(&config, "extra", "");
    assert!(
        registry.on_disk_catalog_changed(),
        "a new bundle directory is a catalog change"
    );
    assert_eq!(
        crate::plugin_reload_nudge(&registry, &mut last_nudged),
        Some(crate::PLUGIN_RELOAD_NUDGE)
    );
    assert!(
        crate::plugin_reload_nudge(&registry, &mut last_nudged).is_none(),
        "the same unseen catalog must nudge only once"
    );

    let reloaded = registry.rediscover_for_workspace(&config.workspace);
    assert!(!reloaded.on_disk_catalog_changed());
    assert!(crate::plugin_reload_nudge(&reloaded, &mut last_nudged).is_none());
}

#[test]
fn trust_and_enablement_are_separate_atomic_state_transitions() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "");

    let mut registry = discover_with_config(&config);
    assert!(registry.enable("demo").is_err());
    assert!(!config.state_path.exists());

    registry.trust("demo").unwrap();
    assert!(registry.get("demo").unwrap().trusted());
    assert!(!registry.get("demo").unwrap().enabled);
    registry.enable("demo").unwrap();
    assert!(registry.is_active("demo"));
    registry.revoke_trust("demo").unwrap();
    assert!(registry.get("demo").unwrap().enabled);
    registry.trust("demo").unwrap();
    assert!(registry.get("demo").unwrap().trusted());
    assert!(
        !registry.get("demo").unwrap().enabled,
        "trust must never reuse an old enablement bit"
    );
    assert!(!registry.is_active("demo"));
    registry.enable("demo").unwrap();
    assert!(registry.is_active("demo"));

    let raw = fs::read_to_string(&config.state_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["schema_version"], 1);
    let receipt = parsed["plugins"]
        .as_object()
        .and_then(|plugins| plugins.values().next())
        .and_then(|plugin| plugin.get("trust"))
        .expect("trust receipt");
    assert!(receipt["content_hash"].as_str().is_some());
    assert!(receipt["capability_hash"].as_str().is_some());
    assert_eq!(receipt["reviewed_capabilities"]["skills"], 0);
    assert!(receipt["reviewed_at"].as_str().is_some());
    let history = parsed["plugins"]
        .as_object()
        .and_then(|plugins| plugins.values().next())
        .and_then(|plugin| plugin["review_history"].as_array())
        .expect("review history");
    assert_eq!(history.len(), 2);
    assert_eq!(history[1]["content_hash"], receipt["content_hash"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(config.state_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&config.state_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let entries = fs::read_dir(config.state_path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(entries.iter().any(|name| name == "plugin-state.json"));
    assert!(entries.iter().any(|name| name == "plugin-state.json.lock"));
    assert!(entries.iter().any(|name| name == ".runtime"));
    assert!(
        entries.iter().all(|name| !name.contains(".tmp")),
        "atomic persistence must not strand temp files: {entries:?}"
    );
}

#[test]
fn native_review_mutation_finishes_then_rechecks_current_default_receipt() {
    let _policy = super::activation::TestPolicyGuard::extension_host(true);
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let plugin = config.user_plugins_dir.join("native-preset");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(
        plugin.join("plugin.json"),
        r#"{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"native-preset","version":"1.0.0","extensions":{"net.codewhale":{"native":{"paths":["index.mjs"]}}}}"#,
    )
    .unwrap();
    fs::write(
        plugin.join("index.mjs"),
        "// codewhale-native-preset-v1 {\"id\":\"reviewer\",\"trust\":\"user\",\"is_default\":true}\nexport function apply() {}\n",
    )
    .unwrap();
    let mut registry = discover_with_config(&config);
    registry.trust("native-preset").unwrap();
    registry.enable("native-preset").unwrap();
    let selected = registry.selected_native_entries().to_vec();
    assert_eq!(selected.len(), 1);
    let fresh = discover_with_config(&config);
    assert_eq!(fresh.selected_native_entries(), selected);
    assert!(fresh.with_native_preset(selected[0].clone()).is_ok());

    registry.disable("native-preset").unwrap();
    assert!(registry.with_native_preset(selected[0].clone()).is_err());
    assert!(!discover_with_config(&config).is_active("native-preset"));
    registry.enable("native-preset").unwrap();
    assert!(registry.with_native_preset(selected[0].clone()).is_ok());
    registry.revoke_trust("native-preset").unwrap();
    assert!(registry.with_native_preset(selected[0].clone()).is_err());
    assert!(!discover_with_config(&config).is_active("native-preset"));
}

#[test]
fn declarative_runtime_sources_survive_restart_only_from_the_staged_snapshot() {
    let fixture = super::test_fixture::DeclarativePluginFixture::new();
    let plugin = fixture.registry.get("runtime-demo").expect("plugin");
    let staged_root = plugin.staged_root.as_deref().expect("staged root");
    for capability in [
        PluginActivationCapability::Commands,
        PluginActivationCapability::Agents,
        PluginActivationCapability::Hooks,
    ] {
        let (sources, errors) =
            super::runtime::active_component_sources(&fixture.registry, capability);
        assert!(errors.is_empty(), "{capability:?}: {errors:?}");
        assert_eq!(sources.len(), 1, "{capability:?}");
        assert!(sources[0].path.starts_with(staged_root), "{capability:?}");
        assert!(
            !sources[0].path.starts_with(&plugin.canonical_root),
            "{capability:?} must never execute from mutable source"
        );
    }
}

#[test]
fn content_change_invalidates_trust_without_changing_capabilities() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let plugin = write_plugin(&config, "\n[skills]\npath = \"skills\"\n");
    fs::create_dir_all(plugin.join("skills/demo")).unwrap();
    fs::write(
        plugin.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: first\n---\nbody\n",
    )
    .unwrap();

    let mut first = discover_with_config(&config);
    first.trust("demo").unwrap();
    first.enable("demo").unwrap();
    assert!(first.is_active("demo"));

    fs::write(
        plugin.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: changed\n---\nbody\n",
    )
    .unwrap();
    let second = discover_with_config(&config);
    let plugin = second.get("demo").unwrap();
    assert!(plugin.enabled, "enablement is independent from trust");
    assert_eq!(plugin.trust_status, PluginTrustStatus::ContentChanged);
    assert!(!plugin.active());
}

#[test]
fn aba_source_skill_body_is_replaced_by_the_staged_snapshot_before_activation() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let plugin = write_plugin(&config, "\n[skills]\npath = \"skills\"\n");
    let skill_path = plugin.join("skills/demo/SKILL.md");
    fs::create_dir_all(skill_path.parent().unwrap()).unwrap();
    fs::write(
        &skill_path,
        "---\nname: demo\ndescription: stable\n---\nbody A\n",
    )
    .unwrap();

    // Capture authority A, parse a transient B body, then restore source A.
    // This deterministically models the old discovery A -> B -> A race.
    let mut authority_a = discover_with_config(&config);
    fs::write(
        &skill_path,
        "---\nname: demo\ndescription: transient\n---\nbody B\n",
    )
    .unwrap();
    let transient_b = discover_with_config(&config)
        .get("demo")
        .unwrap()
        .skill_snapshots
        .clone();
    fs::write(
        &skill_path,
        "---\nname: demo\ndescription: stable\n---\nbody A\n",
    )
    .unwrap();
    authority_a.replace_skill_snapshots_for_test("demo", transient_b);
    assert!(
        authority_a.get("demo").unwrap().skill_snapshots[0]
            .body
            .contains("body B")
    );

    authority_a.trust("demo").unwrap();
    authority_a.enable("demo").unwrap();
    let active = authority_a.get("demo").unwrap();
    assert!(active.active());
    assert!(active.skill_snapshots[0].body.contains("body A"));
    assert!(!active.skill_snapshots[0].body.contains("body B"));
    let staged_bytes = fs::read(&active.skill_snapshots[0].path).unwrap();
    let mut digest = sha2::Sha256::new();
    use sha2::Digest as _;
    digest.update(b"codewhale-plugin-file-bytes-v1\0");
    digest.update(staged_bytes);
    let staged_hash = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(active.skill_snapshots[0].source_hash, staged_hash);
    assert!(
        active.skill_snapshots[0]
            .path
            .starts_with(active.staged_root.as_ref().unwrap()),
        "active Skill paths must point into the Codewhale-owned staged tree"
    );
}

#[test]
fn capability_escalation_invalidates_trust_and_stays_inactive() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let plugin = write_plugin(&config, "");

    let mut first = discover_with_config(&config);
    first.trust("demo").unwrap();
    first.enable("demo").unwrap();

    fs::create_dir_all(plugin.join("hooks")).unwrap();
    fs::write(
        plugin.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\n[hooks]\npath = \"hooks\"\n",
    )
    .unwrap();
    let second = discover_with_config(&config);
    let plugin = second.get("demo").unwrap();
    assert_eq!(plugin.trust_status, PluginTrustStatus::CapabilitiesChanged);
    assert!(plugin.enabled);
    assert!(!plugin.active());
}

#[test]
fn malformed_state_is_fail_closed_and_never_overwritten() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "");
    fs::create_dir_all(config.state_path.parent().unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(
            config.state_path.parent().unwrap(),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    fs::write(&config.state_path, "{ malformed").unwrap();

    let mut registry = discover_with_config(&config);
    assert!(registry.state_error().is_some());
    assert!(!registry.get("demo").unwrap().enabled);
    assert!(!registry.get("demo").unwrap().trusted());
    assert!(registry.trust("demo").is_err());
    assert_eq!(
        fs::read_to_string(&config.state_path).unwrap(),
        "{ malformed"
    );
}

#[test]
fn atomic_write_failure_does_not_mutate_live_enablement() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "");

    let mut registry = discover_with_config(&config);
    registry.trust("demo").unwrap();
    fs::remove_file(&config.state_path).unwrap();
    fs::create_dir(&config.state_path).unwrap();

    assert!(registry.enable("demo").is_err());
    let plugin = registry.get("demo").unwrap();
    assert!(plugin.trusted());
    assert!(!plugin.enabled);
    assert!(!plugin.active());
}

#[test]
fn revoking_trust_does_not_rewrite_enablement() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "");

    let mut registry = discover_with_config(&config);
    registry.trust("demo").unwrap();
    registry.enable("demo").unwrap();
    registry.revoke_trust("demo").unwrap();

    let plugin = registry.get("demo").unwrap();
    assert!(plugin.enabled);
    assert!(!plugin.trusted());
    assert!(!plugin.active());
}

#[test]
fn all_unsupported_bundles_can_be_reviewed_but_not_enabled() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let plugin = write_plugin(&config, "\n[lsp]\npath = \"lsp\"\n");
    fs::create_dir_all(plugin.join("lsp")).unwrap();

    let mut registry = discover_with_config(&config);
    let plugin = registry.get("demo").unwrap();
    assert_eq!(plugin.compatibility(), PluginCompatibility::Unsupported);
    assert!(!plugin.inventory.has_supported_components());
    registry.trust("demo").unwrap();
    let error = registry.enable("demo").unwrap_err();
    assert!(
        error.contains("no supported declarative components"),
        "all-unsupported enable must name the missing supported surfaces: {error}"
    );
    assert!(error.contains("lsp"), "{error}");
    assert!(!registry.is_active("demo"));
    let plugin = registry.get("demo").unwrap();
    assert!(plugin.trusted());
    assert!(!plugin.enabled);
    assert_eq!(plugin.state_label(), "disabled");
    assert_eq!(plugin.compatibility(), PluginCompatibility::Unsupported);
}

#[test]
fn fatal_manifest_errors_fail_closed_and_do_not_activate_mixed_looking_bundles() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let plugin = write_plugin(
        &config,
        "\n[skills]\npath = \"skills\"\n[commands]\npath = \"commands\"\nunknown_field = true\n",
    );
    fs::create_dir_all(plugin.join("skills/demo")).unwrap();
    fs::write(
        plugin.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: first\n---\nbody\n",
    )
    .unwrap();
    fs::create_dir_all(plugin.join("commands")).unwrap();

    let mut registry = discover_with_config(&config);
    assert!(registry.get("demo").is_none());
    assert!(
        registry.diagnostics().iter().any(|diagnostic| {
            diagnostic.level == PluginDiagnosticLevel::Error
                && diagnostic.code == "manifest-invalid"
        }),
        "fatal parse errors must remain registry-level errors: {:?}",
        registry.diagnostics()
    );
    assert!(registry.trust("demo").is_err());
    assert!(registry.enable("demo").is_err());
    assert!(!registry.is_active("demo"));
}

#[test]
fn stale_concurrent_registries_do_not_lose_updates() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_named_plugin(&config, "alpha", "");
    write_named_plugin(&config, "beta", "");

    let left = discover_with_config(&config);
    let right = discover_with_config(&config);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let left_barrier = std::sync::Arc::clone(&barrier);
    let left = std::thread::spawn(move || {
        let mut registry = left;
        left_barrier.wait();
        registry.trust("alpha").unwrap();
        registry.enable("alpha").unwrap();
    });
    let right = std::thread::spawn(move || {
        let mut registry = right;
        barrier.wait();
        registry.trust("beta").unwrap();
        registry.enable("beta").unwrap();
    });
    left.join().unwrap();
    right.join().unwrap();

    let fresh = discover_with_config(&config);
    assert!(fresh.is_active("alpha"));
    assert!(fresh.is_active("beta"));
}

#[test]
fn stale_enable_cannot_resurrect_revoked_trust() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "");
    let mut initial = discover_with_config(&config);
    initial.trust("demo").unwrap();
    initial.enable("demo").unwrap();

    let mut stale = discover_with_config(&config);
    let authority = stale.authority_for("demo").unwrap();
    let mut revoker = discover_with_config(&config);
    revoker.revoke_trust("demo").unwrap();
    assert!(super::registry::verify_plugin_state_authority(&authority).is_err());

    stale.enable("demo").unwrap();
    let fresh = discover_with_config(&config);
    assert!(fresh.get("demo").unwrap().enabled);
    assert!(!fresh.get("demo").unwrap().trusted());
    assert!(!fresh.is_active("demo"));
}

#[cfg(unix)]
#[test]
fn staging_is_owner_only_and_uses_the_reviewed_executable_shape() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let plugin = write_plugin(&config, "");
    let executable = plugin.join("server.sh");
    fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();

    let mut registry = discover_with_config(&config);
    registry.trust("demo").unwrap();
    registry.enable("demo").unwrap();
    let staged = registry.get("demo").unwrap().staged_root.as_ref().unwrap();
    assert_ne!(staged, &plugin);
    let state_parent = config.state_path.parent().unwrap().canonicalize().unwrap();
    let relative_stage = staged.strip_prefix(state_parent).unwrap();
    assert!(
        relative_stage.starts_with(Path::new(".runtime/v2")),
        "runtime authority must use the v2 staging domain: {}",
        staged.display()
    );
    assert_eq!(
        fs::metadata(staged).unwrap().permissions().mode() & 0o777,
        0o500
    );
    assert_eq!(
        fs::metadata(staged.join("plugin.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o400
    );
    assert_eq!(
        fs::metadata(staged.join("server.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o500
    );
}

#[cfg(unix)]
#[test]
fn discovery_does_not_rewrite_existing_state_or_lock_permissions() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "");
    fs::create_dir_all(config.state_path.parent().unwrap()).unwrap();
    fs::set_permissions(
        config.state_path.parent().unwrap(),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    fs::write(
        &config.state_path,
        "{\"schema_version\":1,\"plugins\":{}}\n",
    )
    .unwrap();
    let lock = config.state_path.with_file_name("plugin-state.json.lock");
    fs::write(&lock, b"sentinel").unwrap();
    fs::set_permissions(&config.state_path, fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o666)).unwrap();

    let state_before = fs::metadata(&config.state_path).unwrap();
    let lock_before = fs::metadata(&lock).unwrap();
    let state_body = fs::read(&config.state_path).unwrap();
    let lock_body = fs::read(&lock).unwrap();
    let registry = discover_with_config(&config);

    assert!(registry.state_error().is_none());
    assert_eq!(fs::read(&config.state_path).unwrap(), state_body);
    assert_eq!(fs::read(&lock).unwrap(), lock_body);
    assert_eq!(
        fs::metadata(&config.state_path)
            .unwrap()
            .permissions()
            .mode(),
        state_before.permissions().mode()
    );
    assert_eq!(
        fs::metadata(&lock).unwrap().permissions().mode(),
        lock_before.permissions().mode()
    );
}

#[cfg(unix)]
#[test]
fn discovery_rejects_an_insecure_state_parent_without_mutating_it() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "");
    let state_parent = config.state_path.parent().unwrap();
    fs::create_dir_all(state_parent).unwrap();
    let state_body = b"{\"schema_version\":1,\"plugins\":{}}\n";
    fs::write(&config.state_path, state_body).unwrap();
    fs::set_permissions(state_parent, fs::Permissions::from_mode(0o777)).unwrap();

    let registry = discover_with_config(&config);

    assert!(registry.state_error().is_some());
    assert_eq!(fs::read(&config.state_path).unwrap(), state_body);
    assert_eq!(
        fs::metadata(state_parent).unwrap().permissions().mode() & 0o777,
        0o777,
        "read-only discovery must not repair directory permissions"
    );
}

#[cfg(unix)]
#[test]
fn trust_rejects_an_existing_group_accessible_state_parent_without_repairing_it() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "");
    let state_parent = config.state_path.parent().unwrap();
    fs::create_dir_all(state_parent).unwrap();
    fs::set_permissions(state_parent, fs::Permissions::from_mode(0o777)).unwrap();
    let mut registry = discover_with_config(&config);
    assert!(registry.state_error().is_some());

    assert!(registry.trust("demo").is_err());

    assert_eq!(
        fs::metadata(state_parent).unwrap().permissions().mode() & 0o777,
        0o777,
        "trust must not silently repair a pre-existing unsafe authority directory"
    );
    assert!(!config.state_path.exists());
}

#[cfg(unix)]
#[test]
fn discovery_rejects_a_symlinked_state_parent_without_touching_its_target() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "");
    let target = tmp.path().join("state-target");
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
    let state_body = b"{\"schema_version\":1,\"plugins\":{}}\n";
    fs::write(target.join("plugin-state.json"), state_body).unwrap();
    symlink(&target, config.state_path.parent().unwrap()).unwrap();

    let mut registry = discover_with_config(&config);

    assert!(registry.state_error().is_some());
    assert!(registry.trust("demo").is_err());
    assert_eq!(
        fs::read(target.join("plugin-state.json")).unwrap(),
        state_body
    );
    assert!(
        fs::symlink_metadata(config.state_path.parent().unwrap())
            .unwrap()
            .file_type()
            .is_symlink(),
        "discovery and trust must leave the state-parent link in place"
    );
}

#[cfg(unix)]
#[test]
fn discovery_rejects_linked_state_and_lock_without_touching_targets() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    for linked_entry in ["state", "lock"] {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path());
        write_plugin(&config, "");
        fs::create_dir_all(config.state_path.parent().unwrap()).unwrap();
        fs::set_permissions(
            config.state_path.parent().unwrap(),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let target = tmp.path().join(format!("{linked_entry}-target"));
        fs::write(&target, "{\"schema_version\":1,\"plugins\":{}}\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        let target_before = fs::read(&target).unwrap();
        let target_mode = fs::metadata(&target).unwrap().permissions().mode();
        if linked_entry == "state" {
            symlink(&target, &config.state_path).unwrap();
        } else {
            fs::write(
                &config.state_path,
                "{\"schema_version\":1,\"plugins\":{}}\n",
            )
            .unwrap();
            symlink(
                &target,
                config.state_path.with_file_name("plugin-state.json.lock"),
            )
            .unwrap();
        }

        let registry = discover_with_config(&config);
        assert!(
            registry.state_error().is_some(),
            "linked {linked_entry} must fail closed"
        );
        assert_eq!(fs::read(&target).unwrap(), target_before);
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode(),
            target_mode
        );
    }
}

#[cfg(unix)]
#[test]
fn staging_rejects_root_swaps_symlinked_runtime_parents_and_hardlinks() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let plugin = write_plugin(&config, "");
    let mut swapped = discover_with_config(&config);
    let original = plugin.with_file_name("demo-original");
    fs::rename(&plugin, &original).unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(
        outside.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    symlink(&outside, &plugin).unwrap();
    assert!(swapped.trust("demo").is_err());

    fs::remove_file(&plugin).unwrap();
    fs::rename(&original, &plugin).unwrap();
    let mut parent_swap = discover_with_config(&config);
    fs::create_dir_all(config.state_path.parent().unwrap()).unwrap();
    fs::set_permissions(
        config.state_path.parent().unwrap(),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let runtime_root = config.state_path.parent().unwrap().join(".runtime");
    if runtime_root.exists() {
        fs::remove_dir_all(&runtime_root).unwrap();
    }
    let runtime_outside = tmp.path().join("runtime-outside");
    fs::create_dir(&runtime_outside).unwrap();
    symlink(&runtime_outside, &runtime_root).unwrap();
    assert!(parent_swap.trust("demo").is_err());

    fs::remove_file(&runtime_root).unwrap();
    let external_file = tmp.path().join("external.txt");
    fs::write(&external_file, "reviewed-looking content").unwrap();
    fs::hard_link(&external_file, plugin.join("hardlinked.txt")).unwrap();
    let mut hardlinked = discover_with_config(&config);
    assert!(hardlinked.trust("demo").is_err());
}

// ─────────────────────────────────────────────────────────────────────────────
// Install on-ramp integration (#5182)
// ─────────────────────────────────────────────────────────────────────────────

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn allow_all_network() -> codewhale_net::network_policy::NetworkPolicy {
    codewhale_net::network_policy::NetworkPolicy {
        default: codewhale_net::network_policy::DecisionToml::Allow,
        ..Default::default()
    }
}

fn write_install_source(root: &Path, name: &str) -> PathBuf {
    let source = root.join(format!("source/{name}"));
    fs::create_dir_all(source.join("skills/hello")).unwrap();
    fs::write(
        source.join("plugin.toml"),
        format!(
            "schema_version = 1\n[plugin]\nname = {name:?}\nversion = \"1.0.0\"\n[skills]\npath = \"skills\"\n"
        ),
    )
    .unwrap();
    fs::write(
        source.join("skills/hello/SKILL.md"),
        "---\nname: hello\ndescription: hi\n---\nbody\n",
    )
    .unwrap();
    source
}

#[test]
fn installed_bundles_land_disabled_and_untrusted_then_follow_the_trust_flow() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let source = write_install_source(tmp.path(), "demo");
    let network = allow_all_network();

    let outcome = block_on(super::install::install(
        super::install::PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        &config.user_plugins_dir,
        super::install::DEFAULT_MAX_SIZE_BYTES,
        &network,
        false,
        &|_| None,
    ))
    .unwrap();
    assert!(
        matches!(outcome, super::install::PluginInstallOutcome::Installed(_)),
        "local install must succeed"
    );
    assert!(
        config
            .user_plugins_dir
            .join("demo")
            .join(codewhale_skill_install::install::INSTALLED_FROM_MARKER)
            .exists()
    );

    // The discovery invariant: freshly installed bits are disabled + untrusted.
    let mut registry = discover_with_config(&config);
    let plugin = registry.get("demo").unwrap();
    assert!(!plugin.enabled);
    assert!(!plugin.trusted());
    assert!(registry.enable("demo").is_err());

    registry.trust("demo").unwrap();
    registry.enable("demo").unwrap();
    assert!(registry.is_active("demo"));
    assert_eq!(
        registry
            .get("demo")
            .unwrap()
            .skill_snapshots
            .first()
            .map(|snapshot| snapshot.name.as_str()),
        Some("hello")
    );
}

#[test]
fn mutation_uninstall_requires_disabled_then_deletes_bits_and_prunes_state() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let source = write_install_source(tmp.path(), "demo");
    let network = allow_all_network();

    let mut registry = discover_with_config(&config);
    let ctx = super::mutation::PluginMutationContext {
        network: &network,
        max_size: super::install::DEFAULT_MAX_SIZE_BYTES,
    };
    let receipt = block_on(super::mutation::execute(
        super::mutation::PluginMutationRequest::Install {
            source: super::install::PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        },
        &ctx,
        &mut registry,
    ))
    .unwrap();
    assert_eq!(
        receipt.outcome,
        super::mutation::PluginMutationOutcome::Installed
    );

    let mut registry = discover_with_config(&config);
    registry.trust("demo").unwrap();
    registry.enable("demo").unwrap();

    // Enabled bundles are refused before anything is deleted.
    let refused = block_on(super::mutation::execute(
        super::mutation::PluginMutationRequest::Uninstall {
            selector: "demo".to_string(),
        },
        &ctx,
        &mut registry,
    ));
    assert!(refused.is_err(), "uninstall must require disabled");
    assert!(config.user_plugins_dir.join("demo").exists());

    registry.disable("demo").unwrap();
    let receipt = block_on(super::mutation::execute(
        super::mutation::PluginMutationRequest::Uninstall {
            selector: "demo".to_string(),
        },
        &ctx,
        &mut registry,
    ))
    .unwrap();
    assert_eq!(
        receipt.outcome,
        super::mutation::PluginMutationOutcome::Uninstalled
    );
    assert!(!config.user_plugins_dir.join("demo").exists());

    let rediscovered = discover_with_config(&config);
    assert!(rediscovered.is_empty());
    let raw = fs::read_to_string(&config.state_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(
        parsed["plugins"].as_object().unwrap().is_empty(),
        "state entry must be pruned: {raw}"
    );
}

#[test]
fn mutation_install_rejects_names_claimed_by_other_scopes() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    // A hand-placed workspace bundle already owns the name `demo`.
    let workspace_bundle = config.workspace_plugins_dir.join("demo");
    fs::create_dir_all(&workspace_bundle).unwrap();
    fs::write(
        workspace_bundle.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    let source = write_install_source(tmp.path(), "demo");
    let network = allow_all_network();

    let mut registry = discover_with_config(&config);
    let ctx = super::mutation::PluginMutationContext {
        network: &network,
        max_size: super::install::DEFAULT_MAX_SIZE_BYTES,
    };
    let err = block_on(super::mutation::execute(
        super::mutation::PluginMutationRequest::Install {
            source: super::install::PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        },
        &ctx,
        &mut registry,
    ))
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("already used by the workspace bundle"),
        "got: {err:#}"
    );
    assert!(!config.user_plugins_dir.join("demo").exists());
}

#[test]
fn mutation_update_accepts_unchanged_local_installs_and_refuses_foreign_scopes() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let source = write_install_source(tmp.path(), "demo");
    let network = allow_all_network();

    let mut registry = discover_with_config(&config);
    let ctx = super::mutation::PluginMutationContext {
        network: &network,
        max_size: super::install::DEFAULT_MAX_SIZE_BYTES,
    };
    block_on(super::mutation::execute(
        super::mutation::PluginMutationRequest::Install {
            source: super::install::PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        },
        &ctx,
        &mut registry,
    ))
    .unwrap();

    let installed = config.user_plugins_dir.join("demo");
    let before = [
        installed.join("plugin.toml"),
        installed.join("skills/hello/SKILL.md"),
        installed.join(codewhale_skill_install::install::INSTALLED_FROM_MARKER),
    ]
    .map(|path| {
        let bytes = fs::read(&path).unwrap();
        (path, bytes)
    });
    let state_before = fs::read(&config.state_path).ok();
    let mut registry = discover_with_config(&config);
    let receipt = block_on(super::mutation::execute(
        super::mutation::PluginMutationRequest::Update {
            selector: "demo".to_string(),
        },
        &ctx,
        &mut registry,
    ))
    .unwrap();
    assert_eq!(
        receipt.outcome,
        super::mutation::PluginMutationOutcome::NoChange
    );
    for (path, bytes) in before {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    assert_eq!(fs::read(&config.state_path).ok(), state_before);

    let workspace_bundle = config.workspace_plugins_dir.join("workspace-demo");
    fs::create_dir_all(&workspace_bundle).unwrap();
    let workspace_manifest =
        b"schema_version = 1\n[plugin]\nname = \"workspace-demo\"\nversion = \"1.0.0\"\n";
    fs::write(workspace_bundle.join("plugin.toml"), workspace_manifest).unwrap();
    let mut registry = discover_with_config(&config);
    let err = block_on(super::mutation::execute(
        super::mutation::PluginMutationRequest::Update {
            selector: "workspace-demo".to_string(),
        },
        &ctx,
        &mut registry,
    ))
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("only user-scope bundles"),
        "got: {err:#}"
    );
    assert!(
        format!("{err:#}").contains("workspace bundle"),
        "got: {err:#}"
    );
    assert_eq!(
        fs::read(workspace_bundle.join("plugin.toml")).unwrap(),
        workspace_manifest
    );
    assert!(!config.user_plugins_dir.join("workspace-demo").exists());

    let err = block_on(super::mutation::execute(
        super::mutation::PluginMutationRequest::Update {
            selector: "missing".to_string(),
        },
        &ctx,
        &mut registry,
    ))
    .unwrap_err();
    assert!(format!("{err:#}").contains("was not found"), "got: {err:#}");
}

// ─────────────────────────────────────────────────────────────────────────────
// Agent Plugins v1.0.0 (plugin.json / mcp.json) interop
// ─────────────────────────────────────────────────────────────────────────────

fn write_json_bundle(config: &DiscoveryConfig, dir: &str, plugin_json: &str) -> PathBuf {
    let plugin = config.user_plugins_dir.join(dir);
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("plugin.json"), plugin_json).unwrap();
    plugin
}

#[test]
fn third_party_agent_plugin_with_unknown_extension_namespace_loads_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let plugin = write_json_bundle(
        &config,
        "third-party",
        r#"{
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "third-party",
            "version": "1.4.2",
            "description": "A plugin authored for another client",
            "author": {"name": "Other Client", "email": "plugins@other.example"},
            "keywords": ["browser", "remote"],
            "extensions": {
                "com.example.client": {"anything": [1, 2, 3], "nested": {"x": true}},
                "net.codewhale": {"capabilities": {"network_hosts": ["example.com"]}}
            }
        }"#,
    );
    fs::create_dir_all(plugin.join("skills/demo")).unwrap();
    fs::write(
        plugin.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: demo skill\n---\nbody\n",
    )
    .unwrap();
    fs::create_dir_all(plugin.join("bin")).unwrap();
    fs::write(
        plugin.join("mcp.json"),
        r#"{
            "$schema": "https://agent-plugins.org/schemas/mcp.json",
            "mcpServers": {
                "local": {
                    "type": "stdio",
                    "command": "run.sh",
                    "args": ["--port", "8080"],
                    "env": {"API_KEY": "${THIRD_PARTY_API_KEY}"},
                    "cwd": "bin"
                },
                "remote": {
                    "type": "sse",
                    "url": "https://example.com/mcp",
                    "extensions": {
                        "net.codewhale": {"env_headers": {"Authorization": "THIRD_PARTY_REMOTE_TOKEN"}},
                        "com.example.client": {"polling": true}
                    }
                }
            }
        }"#,
    )
    .unwrap();

    let registry = discover_with_config(&config);
    let errors: Vec<_> = registry
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.level == super::types::PluginDiagnosticLevel::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "unexpected error diagnostics: {errors:?}"
    );

    let plugin = registry
        .get("third-party")
        .expect("third-party plugin loads");
    assert_eq!(
        plugin.manifest.plugin.author.as_deref(),
        Some("Other Client <plugins@other.example>")
    );
    assert_eq!(plugin.manifest.plugin.keywords, vec!["browser", "remote"]);
    assert_eq!(plugin.inventory.skills, 1);
    assert_eq!(plugin.inventory.mcp_servers, 2);
    assert_eq!(plugin.inventory.stdio_mcp_servers, 1);
    assert_eq!(plugin.inventory.remote_mcp_servers, 1);
    assert_eq!(plugin.skill_snapshots.len(), 1);
    assert_eq!(plugin.skill_snapshots[0].name, "demo");

    let servers = plugin.manifest.mcp_servers.as_ref().unwrap();
    assert_eq!(servers["local"].command.as_deref(), Some("run.sh"));
    assert_eq!(servers["local"].env["API_KEY"], "${THIRD_PARTY_API_KEY}");
    assert_eq!(servers["remote"].transport.as_deref(), Some("sse"));
    assert_eq!(
        servers["remote"].env_headers["Authorization"],
        "THIRD_PARTY_REMOTE_TOKEN"
    );
}

#[test]
fn discovery_prefers_plugin_json_over_legacy_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let plugin = config.user_plugins_dir.join("dual");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(
        plugin.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"toml-legacy\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    fs::write(
        plugin.join("plugin.json"),
        r#"{"$schema": "https://agent-plugins.org/schemas/plugin.json", "name": "json-native", "version": "1.0.0"}"#,
    )
    .unwrap();

    let registry = discover_with_config(&config);
    assert!(registry.get("json-native").is_some());
    assert!(registry.get("toml-legacy").is_none());
}

#[test]
fn legacy_toml_names_with_double_hyphens_still_load() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_named_plugin(&config, "a--b", "");
    let registry = discover_with_config(&config);
    assert!(
        registry.get("a--b").is_some(),
        "the legacy plugin.toml name rule keeps `--` runs readable"
    );
}

#[test]
fn plugin_json_with_reserved_mcp_env_name_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let plugin = write_json_bundle(
        &config,
        "env-bad",
        r#"{"$schema": "https://agent-plugins.org/schemas/plugin.json", "name": "env-bad", "version": "1.0.0"}"#,
    );
    fs::write(
        plugin.join("mcp.json"),
        r#"{"mcpServers": {"x": {"command": "run", "env": {"PLUGIN_ROOT": "/tmp"}}}}"#,
    )
    .unwrap();

    let registry = discover_with_config(&config);
    assert!(registry.get("env-bad").is_none());
    assert!(
        registry.diagnostics().iter().any(|diagnostic| {
            diagnostic.level == super::types::PluginDiagnosticLevel::Error
                && diagnostic.message.contains("PLUGIN_ROOT")
        }),
        "expected a PLUGIN_ROOT diagnostic, got {:?}",
        registry.diagnostics()
    );
}

#[test]
fn export_writes_spec_valid_bundle_that_rediscovers() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let plugin = write_named_plugin(
        &config,
        "demo",
        "[skills]\npath = \"skills\"\n\n[mcp_servers.local]\ncommand = \"run.sh\"\n",
    );
    fs::create_dir_all(plugin.join("skills/hello")).unwrap();
    fs::write(
        plugin.join("skills/hello/SKILL.md"),
        "---\nname: hello\ndescription: hello skill\n---\nbody\n",
    )
    .unwrap();

    let registry = discover_with_config(&config);
    let loaded = registry.get("demo").cloned().unwrap();
    let target = tmp.path().join("exported/demo-export");
    let receipt =
        super::export::export_plugin_bundle(&loaded, &target, &Default::default()).unwrap();
    assert_eq!(receipt.exported_name, "demo");
    assert_eq!(receipt.display_name, None);
    assert!(receipt.wrote_mcp_json);
    assert!(!receipt.skills_normalized);

    // The emitted documents exist, conform to the standard's shape, and the
    // legacy manifest is not carried over.
    let plugin_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(target.join("plugin.json")).unwrap()).unwrap();
    super::agent_plugin::validate_plugin_json(&plugin_json).unwrap();
    assert_eq!(plugin_json["name"], "demo");
    let mcp_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(target.join("mcp.json")).unwrap()).unwrap();
    super::agent_plugin::validate_mcp_json(&mcp_json).unwrap();
    assert_eq!(mcp_json["mcpServers"]["local"]["type"], "stdio");
    assert_eq!(mcp_json["mcpServers"]["local"]["command"], "run.sh");
    assert!(target.join("skills/hello/SKILL.md").is_file());
    assert!(!target.join("plugin.toml").exists());

    // The exported bundle is itself discoverable as an Agent Plugins bundle.
    let rediscovery = DiscoveryConfig {
        workspace: tmp.path().join("project2"),
        user_plugins_dir: tmp.path().join("exported"),
        workspace_plugins_dir: tmp.path().join("workspace2"),
        builtin_plugin_dirs: Vec::new(),
        state_path: tmp.path().join("state2/plugin-state.json"),
    };
    let registry = discover_with_config(&rediscovery);
    let exported = registry.get("demo").expect("exported bundle rediscovers");
    assert_eq!(exported.inventory.skills, 1);
    assert_eq!(exported.inventory.mcp_servers, 1);
    assert_eq!(exported.inventory.stdio_mcp_servers, 1);
    assert_eq!(exported.skill_snapshots[0].name, "hello");
}

#[test]
fn export_slugifies_legacy_names_and_collision_is_an_error() {
    // Two legacy plugins whose names collide once slugified: exporting the
    // `a--b` bundle must fail, not silently rename.
    let tmp = tempfile::tempdir().unwrap();
    let pair_config = config(tmp.path());
    write_named_plugin(&pair_config, "a--b", "");
    write_named_plugin(&pair_config, "a-b", "");
    let registry = discover_with_config(&pair_config);
    let loaded = registry.get("a--b").cloned().unwrap();
    let existing: std::collections::BTreeSet<String> = registry
        .list()
        .iter()
        .map(|plugin| plugin.name().to_string())
        .filter(|name| name != "a--b")
        .collect();
    let target = tmp.path().join("out");
    let error = super::export::export_plugin_bundle(&loaded, &target, &existing).unwrap_err();
    assert!(error.contains("collides"), "{error}");
    assert!(
        !target.exists(),
        "a failed export leaves no directory behind"
    );

    // Without the collision, the export slugifies and preserves the original
    // name as the display name.
    let tmp = tempfile::tempdir().unwrap();
    let solo_config = config(tmp.path());
    write_named_plugin(&solo_config, "a--b", "");
    let registry = discover_with_config(&solo_config);
    let loaded = registry.get("a--b").cloned().unwrap();
    let target = tmp.path().join("out");
    let receipt =
        super::export::export_plugin_bundle(&loaded, &target, &Default::default()).unwrap();
    assert_eq!(receipt.exported_name, "a-b");
    assert_eq!(receipt.display_name.as_deref(), Some("a--b"));
    let plugin_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(target.join("plugin.json")).unwrap()).unwrap();
    super::agent_plugin::validate_plugin_json(&plugin_json).unwrap();
    assert_eq!(plugin_json["name"], "a-b");
    assert_eq!(
        plugin_json["extensions"]["net.codewhale"]["display_name"],
        "a--b"
    );
}

#[test]
fn export_moves_custom_skills_layout_to_the_standard_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let plugin = config.user_plugins_dir.join("custom-skills");
    fs::create_dir_all(plugin.join("prompts/my-skill")).unwrap();
    fs::write(
        plugin.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"custom-skills\"\nversion = \"1.0.0\"\n[skills]\npath = \"prompts\"\n",
    )
    .unwrap();
    fs::write(
        plugin.join("prompts/my-skill/SKILL.md"),
        "---\nname: my-skill\ndescription: custom layout\n---\nbody\n",
    )
    .unwrap();

    let registry = discover_with_config(&config);
    let loaded = registry.get("custom-skills").cloned().unwrap();
    assert_eq!(loaded.skill_snapshots.len(), 1);

    let target = tmp.path().join("exported/custom-skills");
    let receipt =
        super::export::export_plugin_bundle(&loaded, &target, &Default::default()).unwrap();
    assert!(receipt.skills_normalized);
    assert!(target.join("skills/my-skill/SKILL.md").is_file());
    assert!(!target.join("prompts").exists());
    let plugin_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(target.join("plugin.json")).unwrap()).unwrap();
    super::agent_plugin::validate_plugin_json(&plugin_json).unwrap();
    assert!(
        plugin_json.get("extensions").is_none(),
        "a standard-layout bundle emits no Codewhale extension at all: {plugin_json}"
    );

    let rediscovery = DiscoveryConfig {
        workspace: tmp.path().join("project2"),
        user_plugins_dir: tmp.path().join("exported"),
        workspace_plugins_dir: tmp.path().join("workspace2"),
        builtin_plugin_dirs: Vec::new(),
        state_path: tmp.path().join("state2/plugin-state.json"),
    };
    let registry = discover_with_config(&rediscovery);
    let exported = registry
        .get("custom-skills")
        .expect("normalized bundle rediscovers");
    assert_eq!(exported.inventory.skills, 1);
    assert_eq!(exported.skill_snapshots[0].name, "my-skill");
}

#[test]
fn export_skills_normalization_collision_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let plugin = config.user_plugins_dir.join("colliding-skills");
    fs::create_dir_all(plugin.join("skills/dup")).unwrap();
    fs::create_dir_all(plugin.join("prompts/dup")).unwrap();
    fs::write(
        plugin.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"colliding-skills\"\nversion = \"1.0.0\"\n[skills]\npath = \"skills\"\npaths = [\"prompts\"]\n",
    )
    .unwrap();
    fs::write(
        plugin.join("skills/dup/SKILL.md"),
        "---\nname: first\ndescription: one\n---\nbody\n",
    )
    .unwrap();
    fs::write(
        plugin.join("prompts/dup/SKILL.md"),
        "---\nname: second\ndescription: two\n---\nbody\n",
    )
    .unwrap();

    let registry = discover_with_config(&config);
    let loaded = registry.get("colliding-skills").cloned().unwrap();
    assert_eq!(loaded.skill_snapshots.len(), 2);
    let target = tmp.path().join("out");
    let error =
        super::export::export_plugin_bundle(&loaded, &target, &Default::default()).unwrap_err();
    assert!(error.contains("collision"), "{error}");
    assert!(
        !target.exists(),
        "a failed export removes the directory it created"
    );
}

fn policy_path(config: &DiscoveryConfig) -> PathBuf {
    config
        .state_path
        .parent()
        .expect("state path has a parent")
        .join(MANAGED_POLICY_FILE_NAME)
}

fn write_policy(path: &Path, policy: &ManagedPluginPolicy) {
    fs::create_dir_all(path.parent().expect("policy path has a parent")).unwrap();
    fs::write(path, serde_json::to_string_pretty(policy).unwrap()).unwrap();
}

fn restrictive_policy() -> ManagedPluginPolicy {
    ManagedPluginPolicy {
        schema_version: MANAGED_POLICY_SCHEMA_VERSION,
        allowed_plugins: Default::default(),
        allow_unlisted: false,
    }
}

#[test]
fn absent_managed_policy_preserves_today_enablement() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "");

    let mut registry = discover_with_config(&config);
    assert!(registry.managed_policy_error().is_none());
    assert_eq!(
        registry.managed_policy_path(),
        Some(policy_path(&config).as_path())
    );
    assert!(
        !registry
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "policy-invalid"),
        "no policy file means no policy diagnostic"
    );

    registry.trust("demo").unwrap();
    registry.enable("demo").unwrap();
    assert!(registry.is_active("demo"));

    let reloaded = discover_with_config(&config);
    assert!(
        reloaded.is_active("demo"),
        "absent policy must keep today's restart behaviour"
    );
}

#[test]
fn managed_policy_refuses_unlisted_enable_with_named_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "");
    write_named_plugin(&config, "extra", "");

    // Review first: trust hardens the state directory, and the policy file
    // lands next to it afterwards, the way an organization delivery would.
    let mut registry = discover_with_config(&config);
    registry.trust("demo").unwrap();
    registry.trust("extra").unwrap();
    let extra_id = registry.get("extra").expect("extra plugin").id.clone();

    write_policy(
        &policy_path(&config),
        &ManagedPluginPolicy {
            schema_version: MANAGED_POLICY_SCHEMA_VERSION,
            allowed_plugins: [extra_id].into_iter().collect(),
            allow_unlisted: false,
        },
    );

    let mut registry = discover_with_config(&config);

    let error = registry.enable("demo").unwrap_err();
    assert!(
        error.contains("managed plugin policy"),
        "refusal must name the policy: {error}"
    );
    assert!(
        error.contains("not on the plugin allowlist"),
        "refusal must say why: {error}"
    );
    assert!(!registry.is_enabled("demo"));
    assert!(!registry.is_active("demo"));

    registry.enable("extra").unwrap();
    assert!(registry.is_active("extra"));

    // Flipping the document to allow unlisted plugins applies to the live
    // registry: `enable` re-reads the policy instead of trusting its snapshot.
    write_policy(
        &policy_path(&config),
        &ManagedPluginPolicy {
            schema_version: MANAGED_POLICY_SCHEMA_VERSION,
            allowed_plugins: Default::default(),
            allow_unlisted: true,
        },
    );
    registry.enable("demo").unwrap();
    assert!(registry.is_active("demo"));
}

#[test]
fn plugin_enabled_before_policy_arrived_does_not_survive_rediscovery() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "");

    let mut registry = discover_with_config(&config);
    registry.trust("demo").unwrap();
    registry.enable("demo").unwrap();
    assert!(registry.is_active("demo"));

    let authority = registry.authority_for("demo").unwrap();
    assert!(super::registry::verify_plugin_state_authority(&authority).is_ok());
    let serialized = serde_json::to_value(&authority).unwrap();
    assert!(serialized.get("managed_policy_source").is_none());
    let unbound: codewhale_config::PluginAuthority = serde_json::from_value(serialized).unwrap();
    assert!(unbound.managed_policy_source.is_none());
    assert!(super::registry::verify_plugin_state_authority(&unbound).is_err());

    write_policy(&policy_path(&config), &restrictive_policy());
    assert!(
        super::registry::verify_plugin_state_authority(&authority)
            .unwrap_err()
            .contains("managed plugin policy")
    );
    let mut redirected = authority.clone();
    redirected.managed_policy_path = Some(tmp.path().join("absent-policy.json"));
    assert!(
        super::registry::verify_plugin_state_authority(&redirected)
            .unwrap_err()
            .contains("trusted managed policy source")
    );

    let reloaded = discover_with_config(&config);
    assert!(
        !reloaded.is_enabled("demo"),
        "a forbidden plugin must not come back enabled"
    );
    assert!(!reloaded.is_active("demo"));
}

#[test]
fn hand_edited_enabled_state_does_not_defeat_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "");

    let mut registry = discover_with_config(&config);
    registry.trust("demo").unwrap();
    let id = registry
        .get("demo")
        .expect("demo plugin")
        .id
        .as_str()
        .to_string();

    // Forge the exact attack: flip the persisted bit by hand.
    let raw = fs::read_to_string(&config.state_path).unwrap();
    let mut state: serde_json::Value = serde_json::from_str(&raw).unwrap();
    state["plugins"][id.as_str()]["enabled"] = serde_json::Value::Bool(true);
    fs::write(
        &config.state_path,
        serde_json::to_string_pretty(&state).unwrap(),
    )
    .unwrap();

    // Control: without a policy the forged bit comes back enabled and active.
    let unpoliced = discover_with_config(&config);
    assert!(unpoliced.is_enabled("demo"));
    assert!(unpoliced.is_active("demo"));

    write_policy(&policy_path(&config), &restrictive_policy());

    let policed = discover_with_config(&config);
    assert!(
        !policed.is_enabled("demo"),
        "hand-edited enabled:true must not survive a forbidding policy"
    );
    assert!(!policed.is_active("demo"));
}

#[test]
fn malformed_managed_policy_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "");

    let mut registry = discover_with_config(&config);
    registry.trust("demo").unwrap();
    registry.enable("demo").unwrap();
    assert!(registry.is_active("demo"));

    let path = policy_path(&config);
    fs::create_dir_all(path.parent().unwrap()).unwrap();

    fs::write(&path, "{ malformed").unwrap();
    let mut registry = discover_with_config(&config);
    assert!(
        registry
            .managed_policy_error()
            .is_some_and(|error| error.contains("failed to parse")),
        "garbage policy must report a clear load error"
    );
    assert!(
        registry
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "policy-invalid"),
        "garbage policy must leave a registry diagnostic"
    );
    assert!(
        !registry.is_enabled("demo"),
        "a malformed policy must not leave the plugin enabled"
    );
    assert!(!registry.is_active("demo"));
    let error = registry.enable("demo").unwrap_err();
    assert!(
        error.contains("Managed plugin policy"),
        "fail-closed refusal must name the policy: {error}"
    );

    fs::write(
        &path,
        r#"{"schema_version":999,"allowed_plugins":[],"allow_unlisted":false}"#,
    )
    .unwrap();
    let registry = discover_with_config(&config);
    assert!(
        registry
            .managed_policy_error()
            .is_some_and(|error| error.contains("unsupported managed plugin policy schema")),
        "wrong-schema policy must fail closed with a clear error"
    );
    assert!(!registry.is_enabled("demo"));

    fs::write(
        &path,
        r#"{"schema_version":1,"allowed_plugins":[],"allow_unlisted":false,"surprise":true}"#,
    )
    .unwrap();
    let registry = discover_with_config(&config);
    assert!(
        registry.managed_policy_error().is_some(),
        "unknown policy fields must fail closed like unknown state fields"
    );
    assert!(!registry.is_enabled("demo"));
}

#[test]
fn managed_policy_path_env_override_is_honored() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "");

    let custom = tmp.path().join("custom-policy.json");
    write_policy(&custom, &restrictive_policy());
    assert!(
        !policy_path(&config).exists(),
        "the default sibling must stay absent so the override is proven"
    );

    let environment = super::context::HostEnvironment::from_entries([(
        std::ffi::OsString::from(MANAGED_POLICY_PATH_ENV),
        custom.as_os_str().to_os_string(),
    )]);
    let context =
        super::context::PluginDiscoveryContext::from_config_and_environment(&config, environment);
    let mut registry = super::discovery::discover_with_context(&config, context);
    assert_eq!(
        registry.managed_policy_path(),
        Some(custom.as_path()),
        "the env override must win over the default sibling path"
    );

    registry.trust("demo").unwrap();
    let error = registry.enable("demo").unwrap_err();
    assert!(
        error.contains("managed plugin policy"),
        "override policy must refuse with the named reason: {error}"
    );
    assert!(!registry.is_enabled("demo"));
    let relative = PathBuf::from("relative-managed-policy.json");
    let launch_directory = std::env::current_dir().unwrap();
    let environment = super::context::HostEnvironment::from_entries([(
        std::ffi::OsString::from(MANAGED_POLICY_PATH_ENV),
        relative.as_os_str().to_os_string(),
    )]);
    let context =
        super::context::PluginDiscoveryContext::from_config_and_environment(&config, environment);
    assert_eq!(
        context.managed_policy_source().unwrap(),
        launch_directory.join(relative)
    );
    assert!(context.managed_policy_source().unwrap().is_absolute());
}
