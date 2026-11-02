use super::*;

fn bundle(root: &Path, directory: &str, name: &str) -> PathBuf {
    let path = root.join(directory);
    fs::create_dir_all(&path).unwrap();
    fs::write(
        path.join("plugin.toml"),
        format!("schema_version = 1\n[plugin]\nname = {name:?}\nversion = \"1.0.0\"\n"),
    )
    .unwrap();
    path
}

fn config(root: &Path) -> DiscoveryConfig {
    DiscoveryConfig {
        workspace: root.join("project"),
        user_plugins_dir: root.join("plugins"),
        workspace_plugins_dir: root.join("project/.codewhale/plugins"),
        builtin_plugin_dirs: vec![],
        state_path: root.join("state.json"),
    }
}

#[test]
fn private_staging_copies_cannot_shadow_the_installed_incumbent() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let installed = bundle(&cfg.user_plugins_dir, "demo", "demo");
    let operation = format!(".staging-{}", uuid::Uuid::new_v4().simple());
    let staged = bundle(&cfg.user_plugins_dir, &operation, "demo");
    fs::write(staged.join("canary.txt"), b"private staged payload").unwrap();
    let registry = discover_with_config(&cfg);
    assert_eq!(registry.len(), 1);
    assert_eq!(
        registry.get("demo").unwrap().canonical_root,
        installed.canonicalize().unwrap()
    );
    assert!(
        !registry
            .diagnostics()
            .iter()
            .any(|d| d.code == "name-conflict")
    );
    assert_eq!(
        fs::read(staged.join("canary.txt")).unwrap(),
        b"private staged payload"
    );
    assert!(!cfg.state_path.exists(), "discovery remains read-only");
}

#[test]
fn reserved_backup_containers_are_hidden_but_human_bundle_paths_remain_visible() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    fs::create_dir_all(&cfg.user_plugins_dir).unwrap();
    let backup = tempfile::Builder::new()
        .prefix(".plugin-backup-")
        .rand_bytes(16)
        .tempdir_in(&cfg.user_plugins_dir)
        .unwrap();
    // Even a manifest accidentally written directly in private recovery
    // storage must not turn it into a discoverable public bundle.
    bundle(backup.path(), "", "private-backup");
    bundle(
        &cfg.user_plugins_dir,
        ".staging-not-a-uuid",
        "human-staging",
    );
    bundle(
        &cfg.user_plugins_dir,
        ".plugin-backup-short",
        "human-backup",
    );
    let dotted = cfg.user_plugins_dir.join("demo.bak");
    fs::create_dir(&dotted).unwrap();
    fs::write(
        dotted.join("plugin.json"),
        serde_json::json!({
            "$schema": crate::agent_plugin::PLUGIN_SCHEMA_URL,
            "name": "demo.bak", "version": "1.0.0"
        })
        .to_string(),
    )
    .unwrap();
    let registry = discover_with_config(&cfg);
    assert!(registry.get("private-backup").is_none());
    assert!(registry.get("human-staging").is_some());
    assert!(registry.get("human-backup").is_some());
    assert_eq!(
        registry.get("demo.bak").unwrap().canonical_root,
        dotted.canonicalize().unwrap()
    );
    assert_eq!(registry.len(), 3);
    assert!(backup.path().join("plugin.toml").is_file());
    assert!(!cfg.state_path.exists());
}
