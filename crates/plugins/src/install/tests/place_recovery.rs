use super::*;
use crate::install::place::{finalize_install, restore_backup};
use crate::install::stage::stage_local_copy;

fn incumbent(plugins: &Path) -> PathBuf {
    let target = write_bundle(plugins, "demo", "demo");
    fs::write(target.join("data.txt"), b"old payload").unwrap();
    fs::write(target.join(INSTALLED_FROM_MARKER), b"old provenance").unwrap();
    target
}

fn prepared_update(root: &Path, plugins: &Path) -> super::super::stage::StagedPlugin {
    let source = write_bundle(root, "source", "demo");
    fs::write(source.join("data.txt"), b"new payload").unwrap();
    stage_local_copy(&source, plugins, DEFAULT_MAX_SIZE_BYTES).unwrap()
}

#[test]
fn update_preserves_a_legitimate_dot_bak_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let target = incumbent(&plugins);
    let other = plugins.join("demo.bak");
    fs::create_dir(&other).unwrap();
    let manifest = serde_json::json!({
        "$schema": crate::agent_plugin::PLUGIN_SCHEMA_URL,
        "name": "demo.bak", "version": "1.0.0"
    });
    fs::write(other.join("plugin.json"), manifest.to_string()).unwrap();
    fs::write(other.join("canary.txt"), b"unrelated installed plugin").unwrap();
    let before = crate::manifest::PluginManifest::validate_from_path(&other.join("plugin.json"))
        .unwrap()
        .content_hash;
    let staged = prepared_update(tmp.path(), &plugins);
    let outcome =
        finalize_install(staged, "path:/controlled/source", None, "", &plugins, true).unwrap();
    assert!(matches!(outcome, PluginInstallOutcome::Installed(_)));
    assert_eq!(fs::read(target.join("data.txt")).unwrap(), b"new payload");
    assert_eq!(
        fs::read(other.join("canary.txt")).unwrap(),
        b"unrelated installed plugin"
    );
    let after = crate::manifest::PluginManifest::validate_from_path(&other.join("plugin.json"))
        .unwrap()
        .content_hash;
    assert_eq!(
        before, after,
        "unrelated valid bundle must be byte-identical"
    );
    assert_eq!(
        fs::read_dir(&plugins).unwrap().count(),
        2,
        "successful publication leaves no backup container"
    );
}

#[test]
fn failed_publication_restores_the_original_bytes_and_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let target = incumbent(&plugins);
    let staged = prepared_update(tmp.path(), &plugins);
    fs::remove_dir_all(&staged.staged_path).unwrap();
    let error =
        finalize_install(staged, "path:/controlled/source", None, "", &plugins, true).unwrap_err();
    assert!(format!("{error:#}").contains("previous plugin restored"));
    assert_eq!(fs::read(target.join("data.txt")).unwrap(), b"old payload");
    assert_eq!(
        fs::read(target.join(INSTALLED_FROM_MARKER)).unwrap(),
        b"old provenance"
    );
    assert_eq!(fs::read_dir(&plugins).unwrap().count(), 1);
}

#[test]
fn failed_restore_retains_the_old_copy_and_never_replaces_an_empty_contender() {
    let tmp = tempfile::tempdir().unwrap();
    let container = tempfile::Builder::new()
        .prefix(".plugin-backup-")
        .rand_bytes(16)
        .tempdir_in(tmp.path())
        .unwrap()
        .keep();
    let old = write_bundle(&container, "bundle", "demo");
    fs::write(old.join("data.txt"), b"recoverable old payload").unwrap();
    fs::write(old.join(INSTALLED_FROM_MARKER), b"recoverable old marker").unwrap();
    let contender = tmp.path().join("demo");
    fs::create_dir(&contender).unwrap();
    let error = restore_backup(
        &container,
        &contender,
        anyhow::anyhow!("publication refused"),
    );
    let message = format!("{error:#}");
    assert!(message.contains("automatic restoration failed"));
    assert!(message.contains(&old.display().to_string()));
    assert!(message.contains("current destination") && message.contains("was not replaced"));
    assert_eq!(
        fs::read(old.join("data.txt")).unwrap(),
        b"recoverable old payload"
    );
    assert_eq!(
        fs::read(old.join(INSTALLED_FROM_MARKER)).unwrap(),
        b"recoverable old marker"
    );
    assert!(contender.is_dir());
    assert_eq!(
        fs::read_dir(&contender).unwrap().count(),
        0,
        "atomic no-replace must preserve even an empty contender directory"
    );
}

#[test]
fn marker_failure_retains_prior_and_current_publications_for_recovery() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let target = incumbent(&plugins);
    let staged = prepared_update(tmp.path(), &plugins);
    // A deterministic real filesystem refusal after rename: a directory
    // cannot be overwritten by the provenance marker's fs::write.
    fs::create_dir(staged.staged_path.join(INSTALLED_FROM_MARKER)).unwrap();
    let error =
        finalize_install(staged, "path:/controlled/source", None, "", &plugins, true).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("automatic restoration refused"));
    assert!(message.contains("publication identity is not pinned"));
    assert!(!message.contains("previous plugin restored"));
    assert_eq!(fs::read(target.join("data.txt")).unwrap(), b"new payload");
    assert!(target.join(INSTALLED_FROM_MARKER).is_dir());
    let containers = fs::read_dir(&plugins)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".plugin-backup-")
        })
        .collect::<Vec<_>>();
    assert_eq!(containers.len(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&containers[0]).unwrap().permissions().mode() & 0o077,
            0,
            "recovery container must be owner-only at creation"
        );
    }
    let retained = containers[0].join("bundle");
    assert!(message.contains(&retained.display().to_string()));
    assert_eq!(fs::read(retained.join("data.txt")).unwrap(), b"old payload");
    assert_eq!(
        fs::read(retained.join(INSTALLED_FROM_MARKER)).unwrap(),
        b"old provenance"
    );
}
