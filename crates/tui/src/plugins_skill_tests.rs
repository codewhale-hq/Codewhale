use std::fs;
use std::path::{Path, PathBuf};

use codewhale_plugins::activation::PluginActivationCapability;
use codewhale_plugins::discovery::{DiscoveryConfig, discover_with_config};
use codewhale_plugins::manifest::{PluginCompatibility, capability_hash_v1, capability_hash_v2};
use codewhale_plugins::types::{PluginDiagnosticLevel, PluginTrustStatus};

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

fn write_mixed_bundle(config: &DiscoveryConfig) -> PathBuf {
    let plugin = write_plugin(
        config,
        "\n[skills]\npath = \"skills\"\n[commands]\npath = \"commands\"\n[hooks]\npath = \"hooks\"\n[lsp]\npath = \"lsp\"\n",
    );
    fs::create_dir_all(plugin.join("skills/demo")).unwrap();
    fs::write(
        plugin.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: first\n---\nbody\n",
    )
    .unwrap();
    fs::create_dir_all(plugin.join("commands")).unwrap();
    fs::create_dir_all(plugin.join("hooks")).unwrap();
    fs::create_dir_all(plugin.join("lsp")).unwrap();
    plugin
}

fn replace_capability_hashes(value: &mut serde_json::Value, from: &str, to: &str) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if key == "capability_hash" && child.as_str() == Some(from) {
                    *child = serde_json::Value::String(to.to_string());
                } else {
                    replace_capability_hashes(child, from, to);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                replace_capability_hashes(item, from, to);
            }
        }
        _ => {}
    }
}

#[test]
fn mixed_supported_and_unsupported_components_activate_only_supported_surfaces() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_mixed_bundle(&config);

    let mut registry = discover_with_config(&config);
    let plugin = registry.get("demo").unwrap();
    assert_eq!(plugin.compatibility(), PluginCompatibility::Partial);
    assert_eq!(
        plugin.inventory.supported_labels(),
        vec!["skills", "commands", "hooks"]
    );
    assert_eq!(plugin.inventory.unsupported_labels(), vec!["lsp"]);
    assert!(
        plugin.diagnostics.iter().any(|diagnostic| {
            diagnostic.level == PluginDiagnosticLevel::Warning
                && diagnostic.code == "component-inactive"
                && diagnostic.message.contains("lsp")
        }),
        "inactive components must stay visible in diagnostics: {:?}",
        plugin.diagnostics
    );

    registry.trust("demo").unwrap();
    registry.enable("demo").unwrap();
    let plugin = registry.get("demo").unwrap();
    assert!(plugin.active());
    assert_eq!(plugin.state_label(), "active");
    assert_eq!(plugin.compatibility(), PluginCompatibility::Partial);
    assert_eq!(plugin.inventory.commands, 1);
    assert_eq!(plugin.inventory.hooks, 1);
    assert_eq!(plugin.skill_snapshots.len(), 1);
    assert_eq!(plugin.skill_snapshots[0].name, "demo");
    assert!(plugin.component_active(PluginActivationCapability::Skills));
    assert!(plugin.component_active(PluginActivationCapability::Commands));
    assert!(plugin.component_active(PluginActivationCapability::Hooks));
    assert!(!plugin.component_active(PluginActivationCapability::Lsp));

    let skills = codewhale_skills::discover_from_directories_with_plugins(
        Vec::<PathBuf>::new(),
        Some(&registry),
    );
    assert_eq!(skills.get("demo:demo").unwrap().body.trim(), "body");
}

#[test]
fn mixed_bundle_revocation_and_trust_changes_deactivate_supported_surfaces() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    let plugin = write_mixed_bundle(&config);

    let mut registry = discover_with_config(&config);
    registry.trust("demo").unwrap();
    registry.enable("demo").unwrap();
    assert!(registry.is_active("demo"));

    registry.revoke_trust("demo").unwrap();
    let revoked = registry.get("demo").unwrap();
    assert!(revoked.enabled);
    assert!(!revoked.trusted());
    assert!(!revoked.active());
    assert_eq!(revoked.compatibility(), PluginCompatibility::Partial);
    let skills = codewhale_skills::discover_from_directories_with_plugins(
        Vec::<PathBuf>::new(),
        Some(&registry),
    );
    assert!(
        skills.get("demo:demo").is_none(),
        "revoking trust must drop the supported Skill adapter"
    );

    registry.trust("demo").unwrap();
    assert!(
        !registry.is_active("demo"),
        "re-trust must not reuse the previous enablement bit"
    );
    registry.enable("demo").unwrap();
    assert!(registry.is_active("demo"));

    fs::write(
        plugin.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: changed\n---\nbody two\n",
    )
    .unwrap();
    let changed = discover_with_config(&config);
    let plugin = changed.get("demo").unwrap();
    assert_eq!(plugin.trust_status, PluginTrustStatus::ContentChanged);
    assert!(plugin.enabled);
    assert!(!plugin.active());
    let skills = codewhale_skills::discover_from_directories_with_plugins(
        Vec::<PathBuf>::new(),
        Some(&changed),
    );
    assert!(skills.get("demo:demo").is_none());
}

#[test]
fn legacy_trust_receipts_fail_closed_as_needs_review_under_v3() {
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
    let plugin = first.get("demo").unwrap();
    let legacy_hashes = [
        ("v1", capability_hash_v1(&plugin.inventory)),
        ("v2", capability_hash_v2(&plugin.inventory)),
    ];
    let v3_hash = plugin.capability_hash.clone();
    assert!(
        legacy_hashes.iter().all(|(_, hash)| hash != &v3_hash),
        "v3 receipts must not collide with either historical domain"
    );
    let content_hash = plugin.content_hash.clone();

    let raw = fs::read_to_string(&config.state_path).unwrap();
    for (version, legacy_hash) in legacy_hashes {
        let mut parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        replace_capability_hashes(&mut parsed, &v3_hash, &legacy_hash);
        fs::write(
            &config.state_path,
            serde_json::to_string_pretty(&parsed).unwrap(),
        )
        .unwrap();

        let restarted = discover_with_config(&config);
        let plugin = restarted.get("demo").unwrap();
        assert_eq!(plugin.content_hash, content_hash);
        assert_eq!(plugin.capability_hash, v3_hash);
        assert_eq!(plugin.trust_status, PluginTrustStatus::CapabilitiesChanged);
        assert!(plugin.enabled);
        assert!(!plugin.trusted());
        assert!(!plugin.active());
        assert!(restarted.authority_for("demo").is_some());
        let skills = codewhale_skills::discover_from_directories_with_plugins(
            Vec::<PathBuf>::new(),
            Some(&restarted),
        );
        assert!(
            skills.get("demo:demo").is_none(),
            "a {version} receipt must not activate Skills after the v3 policy binding"
        );
    }
}

#[test]
fn workspace_scoped_registries_do_not_cross_load_skills() {
    let tmp = tempfile::tempdir().unwrap();
    let left_config = config(&tmp.path().join("left"));
    let right_config = config(&tmp.path().join("right"));
    for (config, body) in [(&left_config, "left body"), (&right_config, "right body")] {
        let plugin = write_plugin(config, "\n[skills]\npath = \"skills\"\n");
        fs::create_dir_all(plugin.join("skills/only")).unwrap();
        fs::write(
            plugin.join("skills/only/SKILL.md"),
            format!("---\nname: only\ndescription: scoped\n---\n{body}\n"),
        )
        .unwrap();
    }
    let mut left = discover_with_config(&left_config);
    left.trust("demo").unwrap();
    left.enable("demo").unwrap();
    let mut right = discover_with_config(&right_config);
    right.trust("demo").unwrap();
    right.enable("demo").unwrap();

    let left_skills = codewhale_skills::discover_from_directories_with_plugins(
        Vec::<PathBuf>::new(),
        Some(&left),
    );
    let right_skills = codewhale_skills::discover_from_directories_with_plugins(
        Vec::<PathBuf>::new(),
        Some(&right),
    );
    assert_eq!(left_skills.get("demo:only").unwrap().body, "left body");
    assert_eq!(right_skills.get("demo:only").unwrap().body, "right body");
    assert_ne!(left.workspace(), right.workspace());
}

/// A DSH bundle package: one remote MCP row and one skill directory.
fn write_dsh_package(root: &Path, url: &str) -> PathBuf {
    let package = root.join("dsh-source");
    fs::create_dir_all(package.join("pack-skills/guide")).unwrap();
    fs::write(
        package.join("package.json"),
        serde_json::json!({"name": "@demo/docs-dsh", "version": "1.0.0",
                           "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}})
        .to_string(),
    )
    .unwrap();
    fs::write(
        package.join("cordis.patch.yml"),
        format!(
            "- insert:\n  - id: docs\n    name: '@deepseek-ai/dsh-mcp-client'\n    config: {{serverName: docs, transport: streamable-http, url: '{url}'}}\n  - id: skills\n    name: '@deepseek-ai/dsh-skill-filesystem'\n    config: {{customSkillDirs: [pack-skills]}}\n  - id: theme\n    name: '@deepseek-ai/dsh-client-ui-theme'\n"
        ),
    )
    .unwrap();
    fs::write(
        package.join("pack-skills/guide/SKILL.md"),
        "---\nname: guide\ndescription: Bundled guide\n---\nBody.\n",
    )
    .unwrap();
    package
}

fn allow_all_network() -> codewhale_net::network_policy::NetworkPolicy {
    codewhale_net::network_policy::NetworkPolicy {
        default: codewhale_net::network_policy::DecisionToml::Allow,
        ..Default::default()
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// DSH import is the ordinary reviewed install: the preview hash is what an
/// exact install stages, the bundle lands disabled and untrusted, update
/// re-converts the recorded package, and a changed package cannot be
/// installed against a stale review or keep its trust.
#[test]
fn dsh_packages_import_through_the_reviewed_install_and_update_flow() {
    codewhale_engine::extension_host::install_ports();
    let tmp = tempfile::tempdir().unwrap();
    let _home = codewhale_runtime::test_support::SealedHome::at(tmp.path());
    let config = config(tmp.path());
    let network = allow_all_network();
    let package = write_dsh_package(tmp.path(), "https://docs.example.invalid/mcp");
    let (conversion, reviewed) = codewhale_plugins::install::preview_dsh(&package).unwrap();
    assert_eq!(conversion.plugin_name, "docs-dsh");
    assert_eq!(conversion.remote_servers, ["docs"]);
    assert_eq!(conversion.skills, ["guide"]);
    assert!(
        conversion
            .outcomes
            .iter()
            .any(|o| o.needs_manual_port() && o.row.as_deref() == Some("theme"))
    );

    let source =
        codewhale_plugins::install::PluginInstallSource::parse(package.to_str().unwrap()).unwrap();
    assert!(
        matches!(
            source,
            codewhale_plugins::install::PluginInstallSource::Dsh(_)
        ),
        "{source:?}"
    );
    let outcome = block_on(
        codewhale_plugins::install::install_with_expected_content_hash(
            source,
            &config.user_plugins_dir,
            codewhale_plugins::install::DEFAULT_MAX_SIZE_BYTES,
            &network,
            &|_| None,
            &reviewed,
        ),
    )
    .unwrap();
    let codewhale_plugins::install::PluginInstallOutcome::Installed(installed) = outcome else {
        panic!("DSH import must install");
    };
    assert_eq!(installed.content_hash, reviewed);
    let marker = fs::read_to_string(
        config
            .user_plugins_dir
            .join("docs-dsh")
            .join(codewhale_skill_install::install::INSTALLED_FROM_MARKER),
    )
    .unwrap();
    assert!(marker.contains("dsh:"), "{marker}");

    let mut registry = discover_with_config(&config);
    let plugin = registry.get("docs-dsh").unwrap();
    assert!(!plugin.enabled && !plugin.trusted());
    registry.trust("docs-dsh").unwrap();
    registry.enable("docs-dsh").unwrap();
    assert!(registry.is_active("docs-dsh"));

    let unchanged = block_on(codewhale_plugins::install::update(
        "docs-dsh",
        &config.user_plugins_dir,
        codewhale_plugins::install::DEFAULT_MAX_SIZE_BYTES,
        &network,
    ))
    .unwrap();
    assert!(matches!(
        unchanged,
        codewhale_plugins::install::PluginUpdateResult::NoChange
    ));

    fs::remove_dir_all(&package).unwrap();
    write_dsh_package(tmp.path(), "https://docs-v2.example.invalid/mcp");
    let stale = block_on(
        codewhale_plugins::install::install_with_expected_content_hash(
            codewhale_plugins::install::PluginInstallSource::Dsh(package.clone()),
            &tmp.path().join("other-plugins"),
            codewhale_plugins::install::DEFAULT_MAX_SIZE_BYTES,
            &network,
            &|_| None,
            &reviewed,
        ),
    )
    .unwrap_err();
    assert!(
        stale.to_string().contains("changed after review"),
        "{stale:#}"
    );

    let updated = block_on(codewhale_plugins::install::update(
        "docs-dsh",
        &config.user_plugins_dir,
        codewhale_plugins::install::DEFAULT_MAX_SIZE_BYTES,
        &network,
    ))
    .unwrap();
    assert!(matches!(
        updated,
        codewhale_plugins::install::PluginUpdateResult::Updated(_)
    ));
    let registry = discover_with_config(&config);
    assert!(
        !registry.get("docs-dsh").unwrap().trusted(),
        "changed converted bytes invalidate the trust receipt"
    );
}
