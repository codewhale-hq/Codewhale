use super::*;
// `scan_tarball` lives in the sibling stage-reader module and is not needed
// by the verbs, so it is not in `super`'s namespace.
use super::tarball::scan_tarball;

#[path = "tests/place_recovery.rs"]
mod place_recovery;

fn write_bundle(root: &Path, dir: &str, name: &str) -> PathBuf {
    let bundle = root.join(dir);
    fs::create_dir_all(&bundle).unwrap();
    fs::write(
        bundle.join("plugin.toml"),
        format!("schema_version = 1\n[plugin]\nname = {name:?}\nversion = \"1.0.0\"\n"),
    )
    .unwrap();
    bundle
}

fn tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for (path, body) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, *body).unwrap();
    }
    let encoder = builder.into_inner().unwrap();
    encoder.finish().unwrap()
}

#[cfg(unix)]
#[test]
fn remote_plugin_stage_preserves_only_owner_executable_intent() {
    use std::os::unix::fs::PermissionsExt as _;

    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for (path, body, mode) in [
        (
            "repo/plugin.toml",
            &b"schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\n"[..],
            0o644,
        ),
        ("repo/bin/server", &b"#!/bin/sh\nexit 0\n"[..], 0o6755),
        ("repo/data.txt", &b"data"[..], 0o666),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        builder.append_data(&mut header, path, body).unwrap();
    }
    let bytes = builder.into_inner().unwrap().finish().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_tarball(&bytes, tmp.path(), DEFAULT_MAX_SIZE_BYTES, None).unwrap();
    for (path, expected) in [("bin/server", 0o700), ("data.txt", 0o600)] {
        assert_eq!(
            fs::metadata(staged.staged_path.join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            expected,
            "{path}"
        );
    }
}

fn symlink_tarball(link_path: &str, target: &str, manifest: &str) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    let body = b"schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, manifest, &body[..])
        .unwrap();
    let mut link_header = tar::Header::new_gnu();
    link_header.set_entry_type(tar::EntryType::Symlink);
    link_header.set_size(0);
    link_header.set_mode(0o777);
    link_header.set_cksum();
    builder
        .append_link(&mut link_header, link_path, target)
        .unwrap();
    let encoder = builder.into_inner().unwrap();
    encoder.finish().unwrap()
}

/// Emit one raw ustar file entry with an arbitrary (possibly hostile) name.
/// `tar::Builder` refuses `..` and absolute paths on write, so adversarial
/// archives have to be assembled byte-by-byte.
fn raw_tar_file_entry(name: &[u8], body: &[u8]) -> Vec<u8> {
    let mut header = [0_u8; 512];
    header[..name.len()].copy_from_slice(name);
    header[100..108].copy_from_slice(b"0000644\0");
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    let size = format!("{:011o}\0", body.len());
    header[124..136].copy_from_slice(size.as_bytes());
    header[136..148].copy_from_slice(b"00000000000\0");
    header[148..156].copy_from_slice(b"        ");
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let checksum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
    let checksum = format!("{checksum:06o}\0 ");
    header[148..156].copy_from_slice(checksum.as_bytes());
    let mut out = header.to_vec();
    out.extend_from_slice(body);
    let padding = (512 - body.len() % 512) % 512;
    out.extend(std::iter::repeat_n(0, padding));
    out
}

fn raw_tarball(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
    use std::io::Write as _;

    let mut tar_bytes = Vec::new();
    for (name, body) in entries {
        tar_bytes.extend(raw_tar_file_entry(name, body));
    }
    tar_bytes.extend(std::iter::repeat_n(0, 1024));
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar_bytes).unwrap();
    encoder.finish().unwrap()
}

fn allow_all() -> NetworkPolicy {
    NetworkPolicy {
        default: crate::network_policy::DecisionToml::Allow,
        ..Default::default()
    }
}

fn no_conflict() -> impl Fn(&str) -> Option<String> {
    |_| None
}

// ── scan/extract rules ────────────────────────────────────────────────

#[test]
fn scan_rejects_path_traversal() {
    let bytes = raw_tarball(&[(
        b"repo-main/../evil/plugin.toml",
        b"schema_version = 1\n[plugin]\nname = \"evil\"\n",
    )]);
    let err = scan_tarball(&bytes, DEFAULT_MAX_SIZE_BYTES).unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<PluginInstallError>(),
            Some(PluginInstallError::PathTraversal(_))
        ),
        "got: {err:#}"
    );
}

#[test]
fn scan_rejects_absolute_paths() {
    let bytes = raw_tarball(&[(
        b"/tmp/evil/plugin.toml",
        b"schema_version = 1\n[plugin]\nname = \"evil\"\n",
    )]);
    assert!(scan_tarball(&bytes, DEFAULT_MAX_SIZE_BYTES).is_err());
}

#[test]
fn scan_enforces_size_cap() {
    let body = vec![b'x'; 1024];
    let bytes = tarball(&[
        (
            "repo-main/plugin.toml",
            b"schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\n",
        ),
        ("repo-main/blob.bin", &body),
    ]);
    let err = scan_tarball(&bytes, 512).unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<PluginInstallError>(),
            Some(PluginInstallError::OversizedBundle { .. })
        ),
        "got: {err:#}"
    );
}

#[test]
fn scan_requires_exactly_one_plugin_manifest_root() {
    let zero = tarball(&[("repo-main/README.md", b"no manifest here")]);
    let err = scan_tarball(&zero, DEFAULT_MAX_SIZE_BYTES).unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<PluginInstallError>(),
            Some(PluginInstallError::PluginTomlRoots(0))
        ),
        "got: {err:#}"
    );

    let manifest = b"schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\n";
    let two = tarball(&[
        ("repo-main/plugin.toml", manifest),
        ("repo-main/examples/other/plugin.toml", manifest),
    ]);
    let err = scan_tarball(&two, DEFAULT_MAX_SIZE_BYTES).unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<PluginInstallError>(),
            Some(PluginInstallError::PluginTomlRoots(2))
        ),
        "got: {err:#}"
    );
}

#[test]
fn stage_tarball_accepts_a_kimi_manifest_root() {
    let manifest = br#"{
      "name": "kimi-archive",
      "version": "1.0.0",
      "description": "Kimi archive fixture"
    }"#;
    let bytes = tarball(&[("repo-main/kimi.plugin.json", manifest)]);
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");

    let staged = stage_tarball(&bytes, &plugins, DEFAULT_MAX_SIZE_BYTES, None).unwrap();

    assert_eq!(staged.name, "kimi-archive");
    assert!(staged.staged_path.join("kimi.plugin.json").is_file());
}

#[test]
fn extract_rejects_symlinks_inside_the_bundle_subtree() {
    let bytes = symlink_tarball(
        "repo-main/evil-link",
        "/etc/passwd",
        "repo-main/plugin.toml",
    );
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let err = stage_tarball(&bytes, &plugins, DEFAULT_MAX_SIZE_BYTES, None).unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<PluginInstallError>(),
            Some(PluginInstallError::SymlinkRejected)
        ),
        "got: {err:#}"
    );
    assert!(fs::read_dir(&plugins).unwrap().next().is_none());
}

#[test]
fn extract_ignores_entries_outside_the_bundle_subtree() {
    let manifest = b"schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\n";
    let bytes = tarball(&[
        ("repo-main/bundles/demo/plugin.toml", manifest),
        (
            "repo-main/bundles/demo/skills/a/SKILL.md",
            b"---\nname: a\ndescription: a\n---\n",
        ),
        ("repo-main/other/plugin.toml.bak", b"ignored"),
        ("repo-main/README.md", b"repo docs stay behind"),
    ]);
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let staged = stage_tarball(&bytes, &plugins, DEFAULT_MAX_SIZE_BYTES, None).unwrap();
    assert_eq!(staged.name, "demo");
    assert!(staged.staged_path.join("plugin.toml").exists());
    assert!(staged.staged_path.join("skills/a/SKILL.md").exists());
    assert!(!staged.staged_path.join("README.md").exists());
    assert!(!staged.staged_path.join("other").exists());
    fs::remove_dir_all(&staged.staged_path).unwrap();
}

// ── local copy rules ──────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_from_local_path_copies_and_marks_the_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let source = write_bundle(tmp.path(), "src/demo", "demo");
    fs::create_dir_all(source.join("skills/hello")).unwrap();
    fs::write(
        source.join("skills/hello/SKILL.md"),
        "---\nname: hello\ndescription: hi\n---\nbody\n",
    )
    .unwrap();

    let outcome = install(
        PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap();
    let PluginInstallOutcome::Installed(installed) = outcome else {
        panic!("expected install to succeed");
    };
    assert_eq!(installed.name, "demo");
    assert_eq!(installed.path, plugins.join("demo"));
    assert!(installed.path.join("plugin.toml").exists());
    assert!(installed.path.join("skills/hello/SKILL.md").exists());
    let marker: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(installed.path.join(INSTALLED_FROM_MARKER)).unwrap(),
    )
    .unwrap();
    assert!(marker["spec"].as_str().unwrap().starts_with("path:"));
    // Local copies must not inherit a stale provenance marker.
    assert_ne!(marker["spec"].as_str().unwrap(), "path:");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exact_local_install_rejects_changed_bytes_before_placement() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let source = write_bundle(tmp.path(), "src/exact-demo", "exact-demo");
    let expected =
        crate::plugins::manifest::PluginManifest::validate_from_path(&source.join("plugin.toml"))
            .unwrap()
            .content_hash;
    fs::write(source.join("README.md"), "changed after review\n").unwrap();

    let error = install_with_expected_content_hash(
        PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        &no_conflict(),
        &expected,
    )
    .await
    .unwrap_err();

    assert!(format!("{error:#}").contains("source changed after review"));
    assert!(!plugins.join("exact-demo").exists());
    assert!(
        fs::read_dir(&plugins).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".staging-")),
        "hash mismatch must clean its private staging directory"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_imports_official_kimi_datasource_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let source = tmp.path().join("src/kimi-datasource");
    fs::create_dir_all(source.join("bin")).unwrap();
    fs::write(
        source.join("kimi.plugin.json"),
        r#"{
          "name": "kimi-datasource",
          "version": "3.3.0",
          "description": "Kimi datasource",
          "mcpServers": {
            "data": {
              "command": "node",
              "args": ["./bin/kimi-datasource.mjs"],
              "cwd": "./"
            }
          },
          "interface": {
            "displayName": "Kimi Datasource",
            "shortDescription": "Data tools",
            "developerName": "Moonshot AI"
          }
        }"#,
    )
    .unwrap();
    fs::write(
        source.join("SKILL.md"),
        "---\nname: kimi-datasource\ndescription: data\n---\n",
    )
    .unwrap();
    fs::write(source.join("bin/kimi-datasource.mjs"), "// fixture\n").unwrap();

    let outcome = install(
        PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap();
    let PluginInstallOutcome::Installed(installed) = outcome else {
        panic!("expected Kimi plugin install to succeed");
    };
    assert_eq!(installed.name, "kimi-datasource");
    assert!(installed.path.join("kimi.plugin.json").exists());
    let validated = crate::plugins::manifest::PluginManifest::validate_from_path(
        &installed.path.join("kimi.plugin.json"),
    )
    .unwrap();
    assert_eq!(validated.inventory.skills, 1);
    assert_eq!(validated.inventory.mcp_servers, 1);
    assert_eq!(validated.inventory.stdio_mcp_servers, 1);
    assert_eq!(
        validated.manifest.plugin.display_name.as_deref(),
        Some("Kimi Datasource")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_imports_managed_kimi_cu_shape_with_platform_and_tool_filters() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let source = tmp.path().join("src/kimi-cu");
    fs::create_dir_all(source.join("skills/kimi-cu")).unwrap();
    fs::create_dir_all(source.join("bin")).unwrap();
    fs::write(
        source.join("kimi.plugin.json"),
        r#"{
          "name": "kimi-cu",
          "version": "0.5.4",
          "description": "Computer-use wiring",
          "license": "Proprietary",
          "skills": "./skills/",
          "mcpServers": {
            "kimi-cu": {
              "command": "sh",
              "args": ["./bin/kimi-cu-mcp"],
              "cwd": "./",
              "enabledTools": ["computer_get_state", "computer_click"]
            }
          },
          "interface": {
            "displayName": "Kimi Computer Use",
            "shortDescription": "Computer control",
            "longDescription": "Requires the external Kimi runtime and permissions.",
            "developerName": "Moonshot AI",
            "iconUrl": "https://example.invalid/kimi-cu.png",
            "category": "Developer Tools",
            "hostKind": "local",
            "platforms": ["macos"],
            "mcpOverrides": {
              "mac": {
                "displayName": "Kimi Computer Use for macOS",
                "iconUrl": "https://example.invalid/kimi-cu-mac.png"
              }
            }
          }
        }"#,
    )
    .unwrap();
    fs::write(
        source.join("skills/kimi-cu/SKILL.md"),
        "---\nname: kimi-cu\ndescription: computer use\n---\n",
    )
    .unwrap();
    fs::write(source.join("bin/kimi-cu-mcp"), "#!/bin/sh\n").unwrap();

    let outcome = install(
        PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap();
    let PluginInstallOutcome::Installed(installed) = outcome else {
        panic!("expected managed Kimi CU plugin install to succeed");
    };
    let validated = crate::plugins::manifest::PluginManifest::validate_from_path(
        &installed.path.join("kimi.plugin.json"),
    )
    .unwrap();
    assert_eq!(validated.inventory.skills, 1);
    assert_eq!(validated.inventory.mcp_servers, 1);
    assert_eq!(validated.inventory.stdio_mcp_servers, 1);
    assert_eq!(
        validated.manifest.plugin.license.as_deref(),
        Some("Proprietary")
    );
    assert_eq!(
        validated
            .manifest
            .when
            .as_ref()
            .and_then(|when| when.os.as_ref()),
        Some(&vec!["macos".to_string()])
    );
    assert_eq!(validated.applicable, cfg!(target_os = "macos"));
    let server = validated
        .manifest
        .mcp_servers
        .as_ref()
        .and_then(|servers| servers.get("kimi-cu"))
        .unwrap();
    assert_eq!(
        server.enabled_tools,
        ["computer_get_state", "computer_click"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_imports_official_kimi_webbridge_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let source = tmp.path().join("src/kimi-webbridge");
    fs::create_dir_all(source.join("skills/kimi-webbridge")).unwrap();
    fs::write(
        source.join("kimi.plugin.json"),
        r#"{
          "$schema": "https://kimi.com/schemas/kimi.plugin.schema.json",
          "name": "kimi-webbridge",
          "version": "1.11.3",
          "description": "Control the real browser",
          "keywords": ["browser", "automation"],
          "author": "Moonshot AI",
          "license": "Proprietary",
          "skills": "./skills/",
          "interface": {
            "displayName": "Kimi WebBridge",
            "shortDescription": "Browser control",
            "longDescription": "Requires the local daemon and browser extension.",
            "developerName": "Moonshot AI",
            "websiteURL": "https://www.kimi.com/features/webbridge"
          }
        }"#,
    )
    .unwrap();
    fs::write(
        source.join("skills/kimi-webbridge/SKILL.md"),
        "---\nname: kimi-webbridge\ndescription: browser\n---\n",
    )
    .unwrap();

    let outcome = install(
        PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap();
    let PluginInstallOutcome::Installed(installed) = outcome else {
        panic!("expected Kimi plugin install to succeed");
    };
    let validated = crate::plugins::manifest::PluginManifest::validate_from_path(
        &installed.path.join("kimi.plugin.json"),
    )
    .unwrap();
    assert_eq!(validated.inventory.skills, 1);
    assert_eq!(validated.inventory.mcp_servers, 0);
    assert_eq!(
        validated.manifest.plugin.author.as_deref(),
        Some("Moonshot AI")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kimi_import_rejects_unsupported_runtime_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let source = tmp.path().join("src/kimi-hooks");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("kimi.plugin.json"),
        r#"{
          "name": "kimi-hooks",
          "version": "1.0.0",
          "hooks": [{"event":"PreToolUse","command":"node ./hook.mjs"}]
        }"#,
    )
    .unwrap();

    let error = install(
        PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("unknown field `hooks`"),
        "unsupported Kimi runtime fields must fail closed: {error:#}"
    );
    assert!(!plugins.join("kimi-hooks").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kimi_import_rejects_unknown_mcp_executable_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let source = tmp.path().join("src/kimi-unknown-exec");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("kimi.plugin.json"),
        r#"{
          "name": "kimi-unknown-exec",
          "version": "1.0.0",
          "mcpServers": {
            "unknown": {
              "command": "node",
              "args": ["server.mjs"],
              "postInstallCommand": "node install.mjs"
            }
          }
        }"#,
    )
    .unwrap();

    let error = install(
        PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("unknown field `postInstallCommand`"),
        "unknown Kimi executable fields must fail closed: {error:#}"
    );
    assert!(!plugins.join("kimi-unknown-exec").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_refuses_to_overwrite_a_hand_placed_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    write_bundle(&plugins, "demo", "demo");
    let source = write_bundle(tmp.path(), "src/demo", "demo");

    let err = install(
        PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<PluginInstallError>(),
            Some(PluginInstallError::NotInstalledHere(_))
        ),
        "hand-placed bundle must be protected, got: {err:#}"
    );
    assert!(
        !plugins.join("demo/skills").exists(),
        "no partial overwrite"
    );

    // A bundle that *was* installed here gets the AlreadyInstalled hint.
    fs::write(plugins.join("demo").join(INSTALLED_FROM_MARKER), "{}").unwrap();
    let err = install(
        PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<PluginInstallError>(),
            Some(PluginInstallError::AlreadyInstalled(_))
        ),
        "got: {err:#}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_install_rejects_symlinks_in_the_source() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let source = write_bundle(tmp.path(), "src/demo", "demo");
    std::os::unix::fs::symlink("/etc/passwd", source.join("linked")).unwrap();

    let err = install(
        PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap_err();
    // The bundle validator rejects symlinked content before any copy runs.
    assert!(format!("{err:#}").contains("symbolic link"), "got: {err:#}");
    assert!(!plugins.join("demo").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_refuses_sources_inside_the_plugins_root() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let nested = write_bundle(&plugins, "demo", "demo");
    let err = install(
        PluginInstallSource::parse(nested.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("inside the user plugins directory"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_enforces_the_name_conflict_hook() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let source = write_bundle(tmp.path(), "src/demo", "demo");
    let err = install(
        PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &|name| Some(format!("name '{name}' is shadowed by a builtin bundle")),
    )
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("shadowed by a builtin bundle"));
    assert!(err.downcast_ref::<PluginNameConflict>().is_some());
    assert!(!plugins.join("demo").exists());
    // The staging dir must be cleaned up on the conflict path.
    assert!(
        !fs::read_dir(&plugins)
            .map(|mut entries| entries.any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".staging-")))
            .unwrap_or(false)
    );
}

// ── update / uninstall ────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_refuses_missing_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    write_bundle(&plugins, "hand", "hand");
    let err = update("hand", &plugins, DEFAULT_MAX_SIZE_BYTES, &allow_all())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<PluginInstallError>(),
            Some(PluginInstallError::NotInstalledHere(_))
        ),
        "got: {err:#}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_update_reuses_staging_and_preserves_the_installed_bundle_on_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let source = write_bundle(tmp.path(), "src/demo", "demo");
    fs::write(source.join("data.txt"), b"v1").unwrap();
    install(
        PluginInstallSource::LocalPath(source.clone()),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap();
    let target = plugins.join("demo");
    let original_marker = fs::read(target.join(INSTALLED_FROM_MARKER)).unwrap();
    assert!(matches!(
        update("demo", &plugins, DEFAULT_MAX_SIZE_BYTES, &allow_all())
            .await
            .unwrap(),
        PluginUpdateResult::NoChange
    ));
    assert_eq!(
        fs::read(target.join(INSTALLED_FROM_MARKER)).unwrap(),
        original_marker
    );

    for altered in [Some(b"altered".as_slice()), None] {
        if let Some(bytes) = altered {
            fs::write(target.join("data.txt"), bytes).unwrap();
        } else {
            fs::remove_file(target.join("data.txt")).unwrap();
        }
        assert!(matches!(
            update("demo", &plugins, DEFAULT_MAX_SIZE_BYTES, &allow_all())
                .await
                .unwrap(),
            PluginUpdateResult::Updated(_)
        ));
        assert_eq!(fs::read(target.join("data.txt")).unwrap(), b"v1");
        assert_eq!(
            fs::read(target.join(INSTALLED_FROM_MARKER)).unwrap(),
            original_marker
        );
    }

    fs::write(source.join("data.txt"), b"v2").unwrap();
    let PluginUpdateResult::Updated(updated) =
        update("demo", &plugins, DEFAULT_MAX_SIZE_BYTES, &allow_all())
            .await
            .unwrap()
    else {
        panic!("changed local source must update through the existing installer");
    };
    assert_eq!(fs::read(updated.path.join("data.txt")).unwrap(), b"v2");
    let updated_marker = fs::read(target.join(INSTALLED_FROM_MARKER)).unwrap();
    assert_ne!(updated_marker, original_marker);
    assert!(matches!(
        update("demo", &plugins, DEFAULT_MAX_SIZE_BYTES, &allow_all())
            .await
            .unwrap(),
        PluginUpdateResult::NoChange
    ));

    let manifest = fs::read(source.join("plugin.toml")).unwrap();
    fs::write(
        source.join("plugin.toml"),
        b"schema_version = 1\n[plugin]\nname = \"other\"\nversion = \"2.0.0\"\n",
    )
    .unwrap();
    let error = update("demo", &plugins, DEFAULT_MAX_SIZE_BYTES, &allow_all())
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<PluginNameConflict>().is_some());
    assert!(!plugins.join("other").exists());
    fs::write(source.join("plugin.toml"), b"malformed = [").unwrap();
    assert!(
        update("demo", &plugins, DEFAULT_MAX_SIZE_BYTES, &allow_all())
            .await
            .is_err()
    );
    fs::write(source.join("plugin.toml"), manifest).unwrap();
    fs::remove_dir_all(&source).unwrap();
    assert!(
        update("demo", &plugins, DEFAULT_MAX_SIZE_BYTES, &allow_all())
            .await
            .is_err()
    );
    assert_eq!(fs::read(target.join("data.txt")).unwrap(), b"v2");
    assert_eq!(
        fs::read(target.join(INSTALLED_FROM_MARKER)).unwrap(),
        updated_marker
    );
    assert_eq!(
        fs::read_dir(&plugins).unwrap().count(),
        1,
        "no staging or backup residue"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uninstall_requires_the_marker_and_removes_the_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let source = write_bundle(tmp.path(), "src/demo", "demo");
    install(
        PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap();

    uninstall("demo", &plugins).unwrap();
    assert!(!plugins.join("demo").exists());

    write_bundle(&plugins, "hand", "hand");
    let err = uninstall("hand", &plugins).unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<PluginInstallError>(),
            Some(PluginInstallError::NotInstalledHere(_))
        ),
        "got: {err:#}"
    );
    assert!(plugins.join("hand").exists(), "hand-placed bundle survives");
    assert!(uninstall("missing", &plugins).is_err());
}

#[cfg(unix)]
#[test]
fn uninstall_rejects_symlink_targets_escaping_the_plugins_root() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&plugins).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join(INSTALLED_FROM_MARKER), "{}").unwrap();
    std::os::unix::fs::symlink(&outside, plugins.join("linked")).unwrap();

    let err = uninstall("linked", &plugins).unwrap_err();
    assert!(format!("{err:#}").contains("escapes plugins directory"));
    assert!(outside.exists());
}

// ── source parsing ────────────────────────────────────────────────────

#[test]
fn parse_routes_remote_and_local_specs() {
    assert_eq!(
        PluginInstallSource::parse("github:owner/repo").unwrap(),
        PluginInstallSource::Remote(InstallSource::GitHubRepo("owner/repo".into()))
    );
    assert_eq!(
        PluginInstallSource::parse("https://example.com/p.tar.gz").unwrap(),
        PluginInstallSource::Remote(InstallSource::DirectUrl(
            "https://example.com/p.tar.gz".into()
        ))
    );
    assert_eq!(
        PluginInstallSource::parse("./bundles/demo").unwrap(),
        PluginInstallSource::LocalPath(PathBuf::from("./bundles/demo"))
    );
    assert_eq!(
        PluginInstallSource::parse("path:/opt/demo").unwrap(),
        PluginInstallSource::LocalPath(PathBuf::from("/opt/demo"))
    );
    assert!(PluginInstallSource::parse("").is_err());
    assert!(PluginInstallSource::parse("   ").is_err());
    assert!(PluginInstallSource::parse("path:").is_err());
    assert_eq!(
        PluginInstallSource::parse("git:github.com/owner/repo").unwrap(),
        PluginInstallSource::Remote(InstallSource::GitHubRepo("owner/repo".into()))
    );
    for (spec, url) in [
        (
            "git:github.com/owner/repo@v1.2.3",
            "https://github.com/owner/repo/archive/v1.2.3.tar.gz",
        ),
        (
            "npm:@owner/plugin@1.2.3",
            "https://registry.npmjs.org/@owner/plugin/-/plugin-1.2.3.tgz",
        ),
        (
            "npm:plugin@1.2.3-alpha.1",
            "https://registry.npmjs.org/plugin/-/plugin-1.2.3-alpha.1.tgz",
        ),
    ] {
        let source = PluginInstallSource::parse(spec).unwrap();
        assert_eq!(
            source,
            PluginInstallSource::Remote(InstallSource::DirectUrl(url.into()))
        );
        assert_eq!(
            PluginInstallSource::parse(&plugin_spec_string(&source, None)).unwrap(),
            source
        );
    }
    for spec in [
        "git:gitlab.com/owner/repo",
        "git:github.com/owner/repo@../x",
        "git:github.com/owner/repo@",
        "npm:plugin",
        "npm:plugin@latest",
        "npm:plugin@^1.2.3",
        "npm:@owner/../plugin@1.2.3",
        "npm:../plugin@1.2.3",
    ] {
        assert!(PluginInstallSource::parse(spec).is_err(), "{spec}");
    }
}

// ── remote fetch against a loopback server ────────────────────────────

/// Serve each body once, in order, over plain loopback HTTP.
fn serve_bodies(bodies: Vec<Vec<u8>>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for body in bodies {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            // Consume the request headers before responding.
            let mut request = Vec::new();
            let mut buf = [0_u8; 1024];
            loop {
                use std::io::Read as _;
                let read = stream.read(&mut buf).unwrap_or(0);
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            use std::io::Write as _;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });
    format!("http://127.0.0.1:{port}/plugin.tar.gz")
}

fn loopback_policy() -> NetworkPolicy {
    NetworkPolicy {
        allow: vec!["127.0.0.1".to_string()],
        ..Default::default()
    }
}

fn remote_bundle_bytes(name: &str, extra: &[u8]) -> Vec<u8> {
    let manifest = format!("schema_version = 1\n[plugin]\nname = {name:?}\nversion = \"1.0.0\"\n");
    tarball(&[
        ("repo-main/plugin.toml", manifest.as_bytes()),
        ("repo-main/data.txt", extra),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn marketplace_archive_selects_only_requested_bundle_and_updates_same_source() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let make_archive = |content: &[u8], name: &str| {
        tarball(&[
            (
                "repo-main/plugins/demo/plugin.toml",
                format!("schema_version = 1\n[plugin]\nname = {name:?}\nversion = \"1.0.0\"\n")
                    .as_bytes(),
            ),
            ("repo-main/plugins/demo/data.txt", content),
            (
                "repo-main/plugins/other/plugin.toml",
                b"schema_version = 1\n[plugin]\nname = \"other\"\nversion = \"1.0.0\"\n",
            ),
        ])
    };
    let v1 = make_archive(b"v1", "demo");
    let v2 = make_archive(b"v2", "demo");
    let url = format!(
        "{}#path=plugins/demo",
        serve_bodies(vec![v1.clone(), v1, v2, make_archive(b"renamed", "other")])
    );
    let outcome = install(
        PluginInstallSource::parse(&url).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &loopback_policy(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, PluginInstallOutcome::Installed(_)));
    assert!(!plugins.join("other").exists());
    assert_eq!(fs::read(plugins.join("demo/data.txt")).unwrap(), b"v1");
    assert!(matches!(
        update("demo", &plugins, DEFAULT_MAX_SIZE_BYTES, &loopback_policy())
            .await
            .unwrap(),
        PluginUpdateResult::NoChange
    ));
    assert!(matches!(
        update("demo", &plugins, DEFAULT_MAX_SIZE_BYTES, &loopback_policy())
            .await
            .unwrap(),
        PluginUpdateResult::Updated(_)
    ));
    assert_eq!(fs::read(plugins.join("demo/data.txt")).unwrap(), b"v2");
    let marker: InstalledFromMarker =
        serde_json::from_str(&fs::read_to_string(plugins.join("demo/.installed-from")).unwrap())
            .unwrap();
    assert_eq!(marker.spec, url);
    assert!(
        update("demo", &plugins, DEFAULT_MAX_SIZE_BYTES, &loopback_policy())
            .await
            .is_err()
    );
    assert_eq!(fs::read(plugins.join("demo/data.txt")).unwrap(), b"v2");
    assert!(!plugins.join("other").exists());
}

#[test]
fn archive_bundle_selector_rejects_traversal_missing_and_ambiguous_roots() {
    for selector in [
        "",
        "/plugins/demo",
        "../demo",
        "plugins/../demo",
        "plugins//demo",
        "plugins/%2e%2e",
        "plugins\\demo",
    ] {
        assert!(
            PluginInstallSource::parse(&format!(
                "https://example.test/repo.tar.gz#path={selector}"
            ))
            .is_err()
        );
    }
    let tmp = tempfile::tempdir().unwrap();
    let manifest = b"schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\n";
    let bytes = tarball(&[
        ("a/plugins/demo/plugin.toml", manifest),
        ("b/plugins/demo/plugin.toml", manifest),
    ]);
    for path in ["plugins/demo", "plugins/missing"] {
        assert!(stage_tarball(&bytes, tmp.path(), DEFAULT_MAX_SIZE_BYTES, Some(path)).is_err());
    }
    let linked = symlink_tarball(
        "repo/plugins/demo/link",
        "/tmp",
        "repo/plugins/demo/plugin.toml",
    );
    assert!(
        stage_tarball(
            &linked,
            tmp.path(),
            DEFAULT_MAX_SIZE_BYTES,
            Some("plugins/demo")
        )
        .is_err()
    );
    assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_is_a_digest_noop_until_the_upstream_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let v1 = remote_bundle_bytes("demo", b"v1");
    let v2 = remote_bundle_bytes("demo", b"v2-changed");
    // install, update (same bytes → no-op), update (new bytes → swap).
    let url = serve_bodies(vec![v1.clone(), v1.clone(), v2.clone()]);

    let outcome = install(
        PluginInstallSource::parse(&url).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &loopback_policy(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap();
    let PluginInstallOutcome::Installed(installed) = outcome else {
        panic!("expected install to succeed");
    };
    assert_eq!(installed.name, "demo");
    assert_eq!(
        fs::read(plugins.join("demo/data.txt")).unwrap(),
        b"v1".to_vec()
    );

    let no_change = update("demo", &plugins, DEFAULT_MAX_SIZE_BYTES, &loopback_policy())
        .await
        .unwrap();
    assert!(
        matches!(no_change, PluginUpdateResult::NoChange),
        "identical upstream bytes must be a digest no-op"
    );
    assert_eq!(
        fs::read(plugins.join("demo/data.txt")).unwrap(),
        b"v1".to_vec()
    );

    let changed = update("demo", &plugins, DEFAULT_MAX_SIZE_BYTES, &loopback_policy())
        .await
        .unwrap();
    let PluginUpdateResult::Updated(updated) = changed else {
        panic!("changed upstream bytes must swap the bundle");
    };
    assert_eq!(
        fs::read(updated.path.join("data.txt")).unwrap(),
        b"v2-changed".to_vec()
    );
    // The marker records the new checksum, so a following update against
    // the same bytes would be a no-op again.
    let marker: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(updated.path.join(INSTALLED_FROM_MARKER)).unwrap(),
    )
    .unwrap();
    assert_eq!(marker["source_checksum"].as_str().unwrap(), sha256_hex(&v2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_install_surfaces_policy_gates_without_touching_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");

    // Default policy prompts for unknown hosts.
    let outcome = install(
        PluginInstallSource::parse("https://plugin.example.invalid/x.tar.gz").unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &NetworkPolicy::default(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap();
    assert!(
        matches!(
            outcome,
            PluginInstallOutcome::NeedsApproval(ref host) if host == "plugin.example.invalid"
        ),
        "got: {outcome:?}"
    );

    let denied = NetworkPolicy {
        deny: vec!["plugin.example.invalid".to_string()],
        ..Default::default()
    };
    let outcome = install(
        PluginInstallSource::parse("https://plugin.example.invalid/x.tar.gz").unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &denied,
        false,
        &no_conflict(),
    )
    .await
    .unwrap();
    assert!(
        matches!(outcome, PluginInstallOutcome::NetworkDenied(_)),
        "got: {outcome:?}"
    );
    assert!(!plugins.join("demo").exists());
}

/// Same manifest/MCP shape as the official Linear and GitHub marketplace
/// bundles: metadata is nested, components and the flat MCP map are at root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_claude_bundle_keeps_root_components_and_review_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir_all(source.join(".claude-plugin")).unwrap();
    fs::create_dir_all(source.join("skills/triage")).unwrap();
    fs::write(
        source.join(".claude-plugin/plugin.json"),
        r#"{"name":"linear","description":"Issue tracking","author":{"name":"Linear"},"skills":"./skills/"}"#,
    )
    .unwrap();
    fs::write(
        source.join(".mcp.json"),
        r#"{"linear":{"type":"http","url":"https://mcp.linear.app/mcp"}}"#,
    )
    .unwrap();
    fs::write(
        source.join("skills/triage/SKILL.md"),
        "---\nname: triage\ndescription: Issue triage\n---\nRead issues.\n",
    )
    .unwrap();
    let plugins = tmp.path().join("plugins");
    let outcome = install(
        PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap();
    let PluginInstallOutcome::Installed(installed) = outcome else {
        panic!("expected install")
    };
    let path = installed.path.join(".claude-plugin/plugin.json");
    let validated = crate::plugins::manifest::PluginManifest::validate_from_path(&path).unwrap();
    assert_eq!(
        validated.canonical_root,
        installed.path.canonicalize().unwrap()
    );
    assert_eq!(validated.inventory.skills, 1);
    assert_eq!(validated.inventory.mcp_servers, 1);
    let server = &validated.manifest.mcp_servers.as_ref().unwrap()["linear"];
    assert_eq!(server.url.as_deref(), Some("https://mcp.linear.app/mcp"));
    assert!(server.command.is_none());
    assert!(
        server.transport.is_none(),
        "Streamable HTTP is the existing default HTTP transport"
    );
    fs::write(
        installed.path.join(".mcp.json"),
        r#"{"linear":{"type":"http","url":"https://changed.invalid/mcp"}}"#,
    )
    .unwrap();
    let changed = crate::plugins::manifest::PluginManifest::validate_from_path(&path).unwrap();
    assert_ne!(validated.content_hash, changed.content_hash);
    assert_ne!(validated.capability_hash, changed.capability_hash);
}

#[test]
fn claude_archive_stages_the_bundle_outside_metadata_directory() {
    let bytes = tarball(&[
        ("repo-main/.claude-plugin/plugin.json", br#"{"name":"github"}"#),
        ("repo-main/.mcp.json", br#"{"mcpServers":{"github":{"type":"http","url":"https://api.githubcopilot.com/mcp/","headers":{"Authorization":"Bearer ${GITHUB_PERSONAL_ACCESS_TOKEN}","X-Workspace":"${GITHUB_WORKSPACE}"}}}}"#),
        ("repo-main/skills/review/SKILL.md", b"---\nname: review\ndescription: Review\n---\nReview code.\n"),
    ]);
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_tarball(
        &bytes,
        &tmp.path().join("plugins"),
        DEFAULT_MAX_SIZE_BYTES,
        None,
    )
    .unwrap();
    assert_eq!(staged.name, "github");
    assert!(
        staged
            .staged_path
            .join(".claude-plugin/plugin.json")
            .is_file()
    );
    assert!(staged.staged_path.join(".mcp.json").is_file());
    assert!(staged.staged_path.join("skills/review/SKILL.md").is_file());
    let validated = crate::plugins::manifest::PluginManifest::validate_from_path(
        &staged.staged_path.join(".claude-plugin/plugin.json"),
    )
    .unwrap();
    let server = &validated.manifest.mcp_servers.as_ref().unwrap()["github"];
    assert!(server.headers.is_empty());
    assert_eq!(
        server.bearer_token_env_var.as_deref(),
        Some("GITHUB_PERSONAL_ACCESS_TOKEN")
    );
    assert_eq!(server.env_headers["X-Workspace"], "GITHUB_WORKSPACE");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_import_rejects_unsupported_components_without_partial_install() {
    for (file, body) in [
        ("hooks/hooks.json", "{}"),
        (".lsp.json", "{}"),
        (
            ".mcp.json",
            r#"{"demo":{"command":"node","unknownExecutableField":"run"}}"#,
        ),
        (
            ".mcp.json",
            r#"{"demo":{"type":"http","url":"https://example.com/mcp","headers":{"Authorization":"Bearer literal-secret"}}}"#,
        ),
        (
            ".mcp.json",
            r#"{"demo":{"type":"http","url":"https://example.com/mcp","headers":{"Authorization":"Bearer ${TOKEN}-suffix"}}}"#,
        ),
        (
            ".mcp.json",
            r#"{"demo":{"type":"http","url":"https://example.com/mcp","headers":{"Authorization":"${TOKEN} ${OTHER}"}}}"#,
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        fs::create_dir_all(source.join(".claude-plugin")).unwrap();
        fs::write(
            source.join(".claude-plugin/plugin.json"),
            r#"{"name":"demo"}"#,
        )
        .unwrap();
        fs::create_dir_all(source.join(file).parent().unwrap()).unwrap();
        fs::write(source.join(file), body).unwrap();
        let plugins = tmp.path().join("plugins");
        assert!(
            install(
                PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
                &plugins,
                DEFAULT_MAX_SIZE_BYTES,
                &allow_all(),
                false,
                &no_conflict()
            )
            .await
            .is_err(),
            "{file}"
        );
        assert!(!plugins.join("demo").exists());
    }
}

#[cfg(unix)]
#[test]
fn claude_metadata_and_mcp_links_cannot_escape_bundle_validation() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("bundle");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("plugin.json"), r#"{"name":"demo"}"#).unwrap();
    symlink(&outside, root.join(".claude-plugin")).unwrap();
    assert!(
        crate::plugins::manifest::PluginManifest::validate_from_path(
            &root.join(".claude-plugin/plugin.json")
        )
        .is_err()
    );
    fs::remove_file(root.join(".claude-plugin")).unwrap();
    fs::create_dir_all(root.join(".claude-plugin")).unwrap();
    fs::copy(
        outside.join("plugin.json"),
        root.join(".claude-plugin/plugin.json"),
    )
    .unwrap();
    fs::write(outside.join("mcp.json"), "{}").unwrap();
    symlink(outside.join("mcp.json"), root.join(".mcp.json")).unwrap();
    assert!(
        crate::plugins::manifest::PluginManifest::validate_from_path(
            &root.join(".claude-plugin/plugin.json")
        )
        .is_err()
    );
}
