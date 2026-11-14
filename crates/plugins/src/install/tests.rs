use super::*;
// `scan_tarball` lives in the sibling stage-reader module and is not needed
// by the verbs, so it is not in `super`'s namespace.
use super::tarball::scan_tarball;
use codewhale_skill_install::bounded_tar::test_archives;

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
        default: codewhale_net::network_policy::DecisionToml::Allow,
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

const MANIFEST: &[u8] = b"schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\n";
const SMALL_CAP: u64 = 64 * 1024;
const HUGE_BODY: u64 = 16 * 1024 * 1024;

fn assert_oversized(err: &anyhow::Error) {
    assert!(
        matches!(
            err.downcast_ref::<PluginInstallError>(),
            Some(PluginInstallError::OversizedBundle { .. })
        ),
        "got: {err:#}"
    );
}

#[test]
fn scan_counts_the_size_a_pax_record_gives_an_entry() {
    let bytes = test_archives::pax_size_record_archive(("plugin.toml", MANIFEST), HUGE_BODY);
    assert_oversized(&scan_tarball(&bytes, SMALL_CAP).unwrap_err());
    let tmp = tempfile::tempdir().unwrap();
    assert_oversized(
        &tarball::stage_tarball(&bytes, tmp.path(), SMALL_CAP, None)
            .map(|staged| staged.name)
            .unwrap_err(),
    );
}

#[test]
fn scan_and_extract_bound_the_decoded_stream_for_extension_entries() {
    for kind in [tar::EntryType::GNULongName, tar::EntryType::XHeader] {
        let bytes =
            test_archives::extension_body_archive(("plugin.toml", MANIFEST), kind, HUGE_BODY);
        assert_oversized(&scan_tarball(&bytes, SMALL_CAP).unwrap_err());
        let tmp = tempfile::tempdir().unwrap();
        assert_oversized(
            &tarball::stage_tarball(&bytes, tmp.path(), SMALL_CAP, None)
                .map(|staged| staged.name)
                .unwrap_err(),
        );
    }
}

fn assert_name_too_long(err: &anyhow::Error) {
    assert!(
        matches!(
            err.downcast_ref::<PluginInstallError>(),
            Some(PluginInstallError::EntryNameTooLong { .. })
        ),
        "got: {err:#}"
    );
}

#[test]
fn entry_names_longer_than_the_bound_are_rejected_before_any_copy() {
    use codewhale_skill_install::bounded_tar::MAX_ENTRY_NAME_BYTES;
    let archives = [
        test_archives::long_name_archive(("plugin.toml", MANIFEST), MAX_ENTRY_NAME_BYTES + 1),
        test_archives::long_name_archive(("plugin.toml", MANIFEST), 100 * 1024),
        test_archives::long_link_target_archive(
            ("plugin.toml", MANIFEST),
            MAX_ENTRY_NAME_BYTES + 1,
        ),
    ];
    for bytes in &archives {
        assert_name_too_long(&scan_tarball(bytes, SMALL_CAP).unwrap_err());
        let tmp = tempfile::tempdir().unwrap();
        assert_name_too_long(
            &tarball::stage_tarball(bytes, tmp.path(), SMALL_CAP, None)
                .map(|staged| staged.name)
                .unwrap_err(),
        );
    }
    let at_bound =
        test_archives::long_name_archive(("plugin.toml", MANIFEST), MAX_ENTRY_NAME_BYTES);
    assert!(scan_tarball(&at_bound, SMALL_CAP).is_ok());
}

#[test]
fn scan_bounds_the_decoded_stream_for_many_empty_entries() {
    let bytes = test_archives::empty_entries_archive(("plugin.toml", MANIFEST), 600);
    assert_oversized(&scan_tarball(&bytes, SMALL_CAP).unwrap_err());
    let few = test_archives::empty_entries_archive(("plugin.toml", MANIFEST), 20);
    assert!(scan_tarball(&few, SMALL_CAP).is_ok());
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
    let expected = crate::manifest::PluginManifest::validate_from_path(&source.join("plugin.toml"))
        .unwrap()
        .content_hash;
    fs::write(source.join("README.md"), "changed after review\n").unwrap();
    let actual = crate::manifest::PluginManifest::validate_from_path(&source.join("plugin.toml"))
        .unwrap()
        .content_hash;

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

    let mismatch = error.downcast_ref::<PluginContentMismatch>().unwrap();
    assert_eq!(mismatch.expected, expected);
    assert_eq!(mismatch.actual, actual);
    assert_eq!(
        error.to_string(),
        format!(
            "plugin source changed after review: expected content hash {expected}, copied bytes hash is {actual}; nothing was installed"
        )
    );
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
    let validated = crate::manifest::PluginManifest::validate_from_path(
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
    let validated = crate::manifest::PluginManifest::validate_from_path(
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
    let validated = crate::manifest::PluginManifest::validate_from_path(
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
    let validated = crate::manifest::PluginManifest::validate_from_path(&path).unwrap();
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
    let changed = crate::manifest::PluginManifest::validate_from_path(&path).unwrap();
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
    let validated = crate::manifest::PluginManifest::validate_from_path(
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
        crate::manifest::PluginManifest::validate_from_path(
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
        crate::manifest::PluginManifest::validate_from_path(
            &root.join(".claude-plugin/plugin.json")
        )
        .is_err()
    );
}

#[test]
fn dsh_admission_validates_engines_peer_ranges_and_platforms_and_admits_cjs_and_node_modules() {
    let tmp = tempfile::tempdir().unwrap();
    let patch_body = "- insert:\n  - id: entry\n    name: ./lib/index.cjs\n";

    let make_bundle = |dir_name: &str, manifest: serde_json::Value| -> PathBuf {
        let dir = tmp.path().join(dir_name);
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(dir.join("package.json"), manifest.to_string()).unwrap();
        fs::write(dir.join("cordis.patch.yml"), patch_body).unwrap();
        fs::write(
            dir.join("lib/index.cjs"),
            "module.exports = { apply() {} };\n",
        )
        .unwrap();
        dir
    };

    let bad_engines = make_bundle(
        "bad-engines",
        serde_json::json!({
            "name": "@demo/bad-engines",
            "version": "1.0.0",
            "engines": { "dsh": ">=0.3.0" },
            "dsh": { "bundle": { "patch": "./cordis.patch.yml" } }
        }),
    );
    let err = dsh::convert_package(&bad_engines, &tmp.path().join("out-bad-engines"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("incompatible-version"), "{err}");
    assert!(err.contains(">=0.3.0"), "{err}");
    assert!(err.contains("0.2.1-rc.5"), "{err}");

    let bad_peer = make_bundle(
        "bad-peer",
        serde_json::json!({
            "name": "@demo/bad-peer",
            "version": "1.0.0",
            "peerDependencies": { "@deepseek-ai/dsh-tools": "^1.0.0" },
            "dsh": { "bundle": { "patch": "./cordis.patch.yml" } }
        }),
    );
    let err = dsh::convert_package(&bad_peer, &tmp.path().join("out-bad-peer"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("incompatible-version"), "{err}");
    assert!(err.contains("@deepseek-ai/dsh-tools"), "{err}");
    assert!(err.contains("^1.0.0"), "{err}");

    let other_platform = if cfg!(target_os = "windows") {
        "darwin"
    } else {
        "win32"
    };
    let bad_platform = make_bundle(
        "bad-platform",
        serde_json::json!({
            "name": "@demo/bad-platform",
            "version": "1.0.0",
            "engines": { "dsh": "^0.2.1-rc.5" },
            "dsh": {
                "bundle": {
                    "patch": "./cordis.patch.yml",
                    "platforms": [other_platform]
                }
            }
        }),
    );
    let err = dsh::convert_package(&bad_platform, &tmp.path().join("out-bad-platform"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("unsupported-platform"), "{err}");
    assert!(err.contains(other_platform), "{err}");

    let host_platform = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else {
        "linux"
    };
    for (idx, range) in ["^0.2.1-rc.5", "^0.2.2-rc.1", ">=0.2.0-rc.1 <0.3.0-0"]
        .iter()
        .enumerate()
    {
        let bundle = make_bundle(
            &format!("valid-{idx}"),
            serde_json::json!({
                "name": format!("@demo/valid-{idx}"),
                "version": "1.0.0",
                "main": "lib/index.cjs",
                "engines": { "dsh": range },
                "dsh": {
                    "bundle": {
                        "patch": "./cordis.patch.yml",
                        "platforms": [host_platform]
                    }
                }
            }),
        );
        let dep_dir = bundle.join("node_modules/fflate");
        fs::create_dir_all(&dep_dir).unwrap();
        fs::write(
            dep_dir.join("package.json"),
            r#"{"name":"fflate","version":"0.8.2","main":"lib/index.cjs"}"#,
        )
        .unwrap();
        fs::create_dir_all(dep_dir.join("lib")).unwrap();
        fs::write(dep_dir.join("lib/index.cjs"), "module.exports = {};\n").unwrap();
        let pkg = dsh::Package::open(&bundle).unwrap();
        assert_eq!(
            dsh::presets::contained_module(&pkg, "fflate").unwrap(),
            dep_dir.join("lib/index.cjs").canonicalize().unwrap()
        );
        let out = tmp.path().join(format!("out-valid-{idx}"));
        let converted = dsh::convert_package(&bundle, &out).unwrap();
        assert!(converted.requires_native);
        assert!(
            out.join("source/node_modules/fflate/lib/index.cjs")
                .is_file()
        );
        let validated =
            crate::manifest::PluginManifest::validate_from_path(&out.join("plugin.json")).unwrap();
        assert!(validated.applicable);
    }

    let addon_bundle = make_bundle(
        "addon-bundle",
        serde_json::json!({
            "name": "@demo/addon-bundle",
            "version": "1.0.0",
            "engines": { "dsh": "^0.2.1-rc.5" },
            "dsh": { "bundle": { "patch": "./cordis.patch.yml" } }
        }),
    );
    fs::create_dir_all(addon_bundle.join("node_modules/native-dep")).unwrap();
    fs::write(
        addon_bundle.join("node_modules/native-dep/binding.node"),
        b"ELF",
    )
    .unwrap();
    let err = dsh::convert_package(&addon_bundle, &tmp.path().join("out-addon"))
        .unwrap_err()
        .to_string();
    assert!(err.contains(".node"), "{err}");
}

#[test]
fn dsh_native_source_admits_a_top_level_gitignore_and_still_refuses_other_hidden_files() {
    let tmp = tempfile::tempdir().unwrap();
    let make = |name: &str| -> PathBuf {
        let dir = tmp.path().join(name);
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(
            dir.join("package.json"),
            serde_json::json!({
                "name": format!("@demo/{name}"),
                "version": "1.0.0",
                "main": "lib/index.cjs",
                "dsh": { "bundle": { "patch": "./cordis.patch.yml" } }
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            dir.join("cordis.patch.yml"),
            "- insert:\n  - id: entry\n    name: ./lib/index.cjs\n",
        )
        .unwrap();
        fs::write(
            dir.join("lib/index.cjs"),
            "module.exports = { apply() {} };\n",
        )
        .unwrap();
        dir
    };

    let with_gitignore = make("with-gitignore");
    fs::write(with_gitignore.join(".gitignore"), "node_modules/\n").unwrap();
    let out = tmp.path().join("out-with-gitignore");
    let converted = dsh::convert_package(&with_gitignore, &out).unwrap();
    assert!(converted.requires_native);
    assert!(out.join("source/.gitignore").is_file());

    for (name, relative) in [
        ("nested-gitignore", "lib/.gitignore"),
        ("other-hidden", ".prettierrc"),
        ("env-file", ".env"),
        ("npmrc", ".npmrc"),
        ("git-dir", ".git/config"),
    ] {
        let bundle = make(name);
        let path = bundle.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "x\n").unwrap();
        let err = dsh::convert_package(&bundle, &tmp.path().join(format!("out-{name}")))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("credential files or repository metadata"),
            "{name}: {err}"
        );
    }

    #[cfg(unix)]
    {
        let linked = make("linked-gitignore");
        fs::write(tmp.path().join("outside-ignore"), "secret\n").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("outside-ignore"), linked.join(".gitignore"))
            .unwrap();
        let err = dsh::convert_package(&linked, &tmp.path().join("out-linked"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("links or reparse points"), "{err}");
    }
}

#[test]
fn dsh_client_closure_is_retained_hashed_and_refuses_ambient_dependencies() {
    let tmp = tempfile::tempdir().unwrap();
    let make = |name: &str, client: &str| -> PathBuf {
        let dir = tmp.path().join(name);
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(
            dir.join("package.json"),
            serde_json::json!({
                "name": format!("@demo/{name}"),
                "version": "1.0.0",
                "main": "lib/index.cjs",
                "exports": { "./client": { "default": "./lib/client.js" } },
                "dsh": {
                    "bundle": { "patch": "./cordis.patch.yml" },
                    "client": { "inject": ["@deepseek-ai/dsh-client-ui-primitives"], "platform": "web" }
                }
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            dir.join("cordis.patch.yml"),
            "- insert:\n  - id: entry\n    name: ./lib/index.cjs\n",
        )
        .unwrap();
        fs::write(
            dir.join("lib/index.cjs"),
            "module.exports = { apply() {} };\n",
        )
        .unwrap();
        fs::write(dir.join("lib/client.js"), client).unwrap();
        fs::write(dir.join("lib/util.js"), "exports.v = 1;\n").unwrap();
        dir
    };

    let good = make(
        "good",
        "const r = require(\"react\");\nconst p = require('@deepseek-ai/dsh-client-ui-primitives');\nconst u = require(\"./util\");\n",
    );
    let converted = dsh::convert_package(&good, &tmp.path().join("out-good")).unwrap();
    assert_eq!(converted.client_closures.len(), 1);
    let closure = &converted.client_closures[0];
    assert_eq!(closure.entry, "source/lib/client.js");
    assert!(closure.files.contains_key("source/lib/util.js"));
    assert_eq!(closure.host_provided.len(), 2);
    let installed = fs::read(tmp.path().join("out-good/source/lib/util.js")).unwrap();
    assert_eq!(
        closure.files["source/lib/util.js"],
        codewhale_skill_install::install::sha256_hex(&installed)
    );
    assert!(
        converted
            .outcomes
            .iter()
            .any(|o| o.kind == "client" && o.outcome == "retained")
    );
    assert!(!converted.outcomes.iter().any(|o| o.needs_manual_port()));

    for (label, source) in [
        ("ambient", "require(\"left-pad\");\n"),
        ("builtin", "require(\"node:fs\");\n"),
        ("computed", "const n = \"x\"; require(n);\n"),
        ("concat", "const l = \"en\"; require(\"./locale/\" + l);\n"),
        ("concat-import", "const l = \"en\"; import(\"./a\" + l);\n"),
        ("escape", "require(\"../../outside.js\");\n"),
    ] {
        let bad = make(label, source);
        let converted =
            dsh::convert_package(&bad, &tmp.path().join(format!("out-{label}"))).unwrap();
        assert!(converted.client_closures.is_empty(), "{label}");
        assert!(
            converted
                .outcomes
                .iter()
                .any(|o| o.kind == "client" && o.outcome == "skipped"),
            "{label}"
        );
        assert!(
            converted.requires_native,
            "{label}: server half still imports"
        );
    }
}

fn write_dsh_base_package(root: &Path, package_json: serde_json::Value, patch: &str) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("package.json"),
        serde_json::to_string_pretty(&package_json).unwrap(),
    )
    .unwrap();
    fs::write(root.join("cordis.patch.yml"), patch).unwrap();
    root.to_path_buf()
}

fn minimal_dsh_patch_with_skill() -> &'static str {
    "- insert:\n  - id: guide-skills\n    name: '@deepseek-ai/dsh-skill-filesystem'\n    config:\n      customSkillDirs:\n        - custom-skills\n"
}

fn write_minimal_skill(dir: &Path, name: &str) {
    fs::create_dir_all(dir.join(name)).unwrap();
    fs::write(
        dir.join(name).join("SKILL.md"),
        format!("---\nname: {name}\ndescription: demo skill\n---\nbody\n"),
    )
    .unwrap();
}

#[tokio::test]
async fn local_install_of_folder_with_both_files_takes_dsh_and_merges_commands_agents_hooks() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let pkg = write_dsh_base_package(
        &tmp.path().join("combo-pkg"),
        serde_json::json!({
            "name": "@acme/combo-widget",
            "version": "1.2.0",
            "description": "DSH description wins",
            "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
        }),
        minimal_dsh_patch_with_skill(),
    );
    write_minimal_skill(&pkg.join("custom-skills"), "guide");
    fs::create_dir_all(pkg.join("commands")).unwrap();
    fs::write(pkg.join("commands/hello.md"), "# Hello\nRun hello.\n").unwrap();
    fs::create_dir_all(pkg.join("agents")).unwrap();
    fs::write(
        pkg.join("agents/reviewer.toml"),
        "id = \"reviewer\"\n[role]\nname = \"reviewer\"\ndescription = \"Reviews\"\nprompt = \"Review.\"\n",
    )
    .unwrap();
    fs::create_dir_all(pkg.join("hooks")).unwrap();
    fs::write(
        pkg.join("hooks/hooks.toml"),
        "enabled = true\n[[hooks]]\nname = \"check\"\nevent = \"turn_end\"\ncommand = \"true\"\n",
    )
    .unwrap();
    fs::write(
        pkg.join("plugin.json"),
        serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "combo-widget",
            "version": "1.2.0",
            "extensions": {
                "net.codewhale": {
                    "commands": {"path": "commands"},
                    "agents": {"path": "agents"},
                    "hooks": {"path": "hooks"}
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let bare_source = PluginInstallSource::parse(pkg.to_str().unwrap()).unwrap();
    assert_eq!(bare_source, PluginInstallSource::Dsh(pkg.clone()));
    let path_prefixed =
        PluginInstallSource::parse(&format!("path:{}", pkg.to_str().unwrap())).unwrap();
    assert_eq!(path_prefixed, PluginInstallSource::Dsh(pkg.clone()));

    let outcome = install(
        bare_source,
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap();
    let PluginInstallOutcome::Installed(installed) = outcome else {
        panic!("expected merged install");
    };
    assert_eq!(installed.name, "combo-widget");
    assert!(installed.path.join("commands/hello.md").is_file());
    assert!(installed.path.join("agents/reviewer.toml").is_file());
    assert!(installed.path.join("hooks/hooks.toml").is_file());
    assert!(installed.path.join("skills/guide/SKILL.md").is_file());
    assert!(installed.path.join("CONVERSION.md").is_file());
    assert!(installed.path.join("CONVERSION.json").is_file());

    let validated =
        crate::manifest::PluginManifest::validate_from_path(&installed.path.join("plugin.json"))
            .unwrap();
    assert_eq!(validated.inventory.skills, 1);
    assert_eq!(validated.inventory.commands, 1);
    assert_eq!(validated.inventory.agents, 1);
    assert_eq!(validated.inventory.hooks, 1);

    let config = crate::discovery::DiscoveryConfig {
        workspace: tmp.path().join("workspace"),
        user_plugins_dir: plugins.clone(),
        workspace_plugins_dir: tmp.path().join("workspace/.codewhale/plugins"),
        builtin_plugin_dirs: Vec::new(),
        state_path: tmp.path().join("state.json"),
    };
    let registry = crate::discovery::discover_with_config(&config);
    let loaded = registry.get("combo-widget").unwrap();
    assert!(!loaded.trusted());
    assert!(!loaded.enabled);
    assert_eq!(loaded.inventory.commands, 1);
    assert_eq!(loaded.inventory.agents, 1);
    assert_eq!(loaded.inventory.hooks, 1);
}

#[test]
fn plugin_json_beside_dsh_package_keeps_dsh_name_and_version_and_refuses_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let make_pair = |tag: &str, pkg_ver: Option<&str>, pj: serde_json::Value| {
        let mut package = serde_json::json!({
            "name": "@org/my-tool",
            "description": "From package.json",
            "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
        });
        if let Some(ver) = pkg_ver {
            package["version"] = serde_json::json!(ver);
        }
        let dir = write_dsh_base_package(
            &tmp.path().join(tag),
            package,
            minimal_dsh_patch_with_skill(),
        );
        write_minimal_skill(&dir.join("custom-skills"), "guide");
        fs::create_dir_all(dir.join("commands")).unwrap();
        fs::write(dir.join("commands/ping.md"), "ping\n").unwrap();
        fs::write(
            dir.join("plugin.json"),
            serde_json::to_string_pretty(&pj).unwrap(),
        )
        .unwrap();
        dir
    };

    let wrong_name = make_pair(
        "wrong-name",
        Some("1.0.0"),
        serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "other-name",
            "version": "1.0.0",
            "extensions": {"net.codewhale": {"commands": {"path": "commands"}}}
        }),
    );
    let err = dsh::convert_package(&wrong_name, &tmp.path().join("out-wrong-name"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("plugin.json"), "{err}");
    assert!(err.contains("package.json"), "{err}");
    assert!(err.contains("my-tool"), "{err}");

    let wrong_ver = make_pair(
        "wrong-ver",
        Some("1.0.0"),
        serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "my-tool",
            "version": "2.0.0",
            "extensions": {"net.codewhale": {"commands": {"path": "commands"}}}
        }),
    );
    let err = dsh::convert_package(&wrong_ver, &tmp.path().join("out-wrong-ver"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("plugin.json"), "{err}");
    assert!(err.contains("package.json"), "{err}");
    assert!(err.contains("2.0.0"), "{err}");

    let meta_ignored = make_pair(
        "meta-ignored",
        Some("1.0.0"),
        serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "my-tool",
            "description": "From plugin.json (ignored)",
            "author": {"name": "Someone"},
            "homepage": "https://example.invalid",
            "repository": "https://example.invalid/repo",
            "license": "MIT",
            "keywords": ["alpha"],
            "extensions": {
                "net.codewhale": {"commands": {"path": "commands"}},
                "com.example.vendor": {"ignored": true}
            }
        }),
    );
    let out_ignored = tmp.path().join("out-meta-ignored");
    let converted = dsh::convert_package(&meta_ignored, &out_ignored).unwrap();
    assert!(
        converted
            .diagnostics
            .iter()
            .any(|line| line.contains("plugin.json")
                && line.contains("package.json")
                && line.contains("description")
                && line.contains("author")),
        "{:?}",
        converted.diagnostics
    );
    let emitted: serde_json::Value =
        serde_json::from_slice(&fs::read(out_ignored.join("plugin.json")).unwrap()).unwrap();
    assert_eq!(emitted["name"], "my-tool");
    assert_eq!(emitted["version"], "1.0.0");
    assert_eq!(emitted["description"], "From package.json");
    assert!(emitted.get("author").is_none());
    assert!(emitted["extensions"].get("com.example.vendor").is_none());

    let pkg_no_ver = make_pair(
        "pkg-no-ver",
        None,
        serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "my-tool",
            "version": "9.9.9",
            "extensions": {"net.codewhale": {"commands": {"path": "commands"}}}
        }),
    );
    let out_no_ver = tmp.path().join("out-no-ver");
    dsh::convert_package(&pkg_no_ver, &out_no_ver).unwrap();
    let emitted_no_ver: serde_json::Value =
        serde_json::from_slice(&fs::read(out_no_ver.join("plugin.json")).unwrap()).unwrap();
    assert!(emitted_no_ver.get("version").is_none());
}

#[test]
fn plugin_json_beside_dsh_package_may_not_declare_skills_or_connectors() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = |tag: &str, ext: serde_json::Value| {
        let dir = write_dsh_base_package(
            &tmp.path().join(tag),
            serde_json::json!({
                "name": "tool-pkg",
                "version": "1.0.0",
                "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
            }),
            minimal_dsh_patch_with_skill(),
        );
        write_minimal_skill(&dir.join("custom-skills"), "guide");
        fs::create_dir_all(dir.join("commands")).unwrap();
        fs::write(dir.join("commands/ping.md"), "ping\n").unwrap();
        fs::write(
            dir.join("plugin.json"),
            serde_json::json!({
                "$schema": "https://agent-plugins.org/schemas/plugin.json",
                "name": "tool-pkg",
                "version": "1.0.0",
                "extensions": {"net.codewhale": ext}
            })
            .to_string(),
        )
        .unwrap();
        dir
    };

    for (key, val) in [
        ("skills", serde_json::json!({"path": "custom-skills"})),
        ("native", serde_json::json!({"path": "index.mjs"})),
        ("lsp", serde_json::json!({"path": "lsp"})),
        (
            "providers",
            serde_json::json!({"acme": {"base_url": "https://example.invalid"}}),
        ),
        (
            "capabilities",
            serde_json::json!({"network_hosts": ["api.example.com"]}),
        ),
        ("when", serde_json::json!({"binaries": ["node"]})),
        ("display_name", serde_json::json!("Pretty")),
        ("icon", serde_json::json!("icon.svg")),
    ] {
        let pkg = setup(&format!("forbid-{key}"), serde_json::json!({key: val}));
        let err = dsh::convert_package(&pkg, &tmp.path().join(format!("out-{key}")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("plugin.json"), "{key}: {err}");
        assert!(err.contains("package.json"), "{key}: {err}");
        assert!(err.contains(key), "{key}: {err}");
    }

    let with_mcp_json = setup(
        "sibling-mcp-json",
        serde_json::json!({"commands": {"path": "commands"}}),
    );
    fs::write(
        with_mcp_json.join("mcp.json"),
        r#"{"mcpServers":{"s":{"type":"http","url":"https://example.invalid"}}}"#,
    )
    .unwrap();
    let err = dsh::convert_package(&with_mcp_json, &tmp.path().join("out-mcp-json"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("plugin.json"), "{err}");
    assert!(err.contains("package.json"), "{err}");
    assert!(err.contains("mcp.json"), "{err}");

    let with_unpatched_skills = setup(
        "unpatched-skills-dir",
        serde_json::json!({"commands": {"path": "commands"}}),
    );
    write_minimal_skill(&with_unpatched_skills.join("skills"), "auto-picked");
    let err = dsh::convert_package(
        &with_unpatched_skills,
        &tmp.path().join("out-unpatched-skills"),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("plugin.json"), "{err}");
    assert!(err.contains("package.json"), "{err}");
    assert!(err.contains("skills/"), "{err}");

    let patched_root_skills = write_dsh_base_package(
        &tmp.path().join("patched-root-skills"),
        serde_json::json!({
            "name": "tool-pkg",
            "version": "1.0.0",
            "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
        }),
        "- insert:\n  - id: root-skills\n    name: '@deepseek-ai/dsh-skill-filesystem'\n    config:\n      customSkillDirs:\n        - skills\n",
    );
    write_minimal_skill(&patched_root_skills.join("skills"), "patched");
    fs::create_dir_all(patched_root_skills.join("commands")).unwrap();
    fs::write(patched_root_skills.join("commands/ping.md"), "ping\n").unwrap();
    fs::write(
        patched_root_skills.join("plugin.json"),
        serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "tool-pkg",
            "version": "1.0.0",
            "extensions": {"net.codewhale": {"commands": {"path": "commands"}}}
        })
        .to_string(),
    )
    .unwrap();
    let converted = dsh::convert_package(
        &patched_root_skills,
        &tmp.path().join("out-patched-root-skills"),
    )
    .unwrap();
    assert_eq!(converted.skills, ["patched"]);
    assert_eq!(converted.commands, ["commands/ping.md"]);
}

#[test]
fn hooks_from_plugin_json_and_hook_bridge_patch_row_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    for (label, bridge_pkg) in [
        ("claude", "@deepseek-ai/dsh-hooks-claude-code"),
        ("codex", "@deepseek-ai/dsh-hooks-codex"),
    ] {
        let dir = write_dsh_base_package(
            &tmp.path().join(format!("dup-hooks-{label}")),
            serde_json::json!({
                "name": "hook-clash",
                "version": "1.0.0",
                "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
            }),
            &format!(
                "- insert:\n  - id: bridge-hooks\n    name: '{bridge_pkg}'\n    config:\n      configPath: bridge-hooks.json\n"
            ),
        );
        fs::write(dir.join("bridge-hooks.json"), "{}").unwrap();
        fs::create_dir_all(dir.join("hooks")).unwrap();
        fs::write(
            dir.join("hooks/hooks.toml"),
            "enabled = true\n[[hooks]]\nname = \"cw\"\nevent = \"turn_end\"\ncommand = \"true\"\n",
        )
        .unwrap();
        fs::write(
            dir.join("plugin.json"),
            serde_json::json!({
                "$schema": "https://agent-plugins.org/schemas/plugin.json",
                "name": "hook-clash",
                "version": "1.0.0",
                "extensions": {"net.codewhale": {"hooks": {"path": "hooks"}}}
            })
            .to_string(),
        )
        .unwrap();

        let err = dsh::convert_package(&dir, &tmp.path().join(format!("out-{label}")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("package.json"), "{label}: {err}");
        assert!(err.contains("plugin.json"), "{label}: {err}");
        assert!(err.contains("hooks"), "{label}: {err}");
    }
}

#[test]
fn other_manifest_spellings_beside_dsh_package_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    for spelling in [
        "kimi.plugin.json",
        "plugin.toml",
        ".claude-plugin/plugin.json",
        ".plugin/plugin.json",
        ".github/plugin/plugin.json",
    ] {
        let dir = write_dsh_base_package(
            &tmp.path().join(spelling.replace(['/', '.'], "_")),
            serde_json::json!({
                "name": "other-manifest",
                "version": "1.0.0",
                "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
            }),
            minimal_dsh_patch_with_skill(),
        );
        write_minimal_skill(&dir.join("custom-skills"), "guide");
        let target = dir.join(spelling);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(
            &target,
            if spelling.ends_with(".toml") {
                "schema_version = 1\n[plugin]\nname = \"other-manifest\"\nversion = \"1.0.0\"\n"
            } else {
                r#"{"name":"other-manifest"}"#
            },
        )
        .unwrap();

        assert_eq!(
            PluginInstallSource::parse(dir.to_str().unwrap()).unwrap(),
            PluginInstallSource::Dsh(dir.clone()),
            "{spelling} must still route to DSH"
        );
        let err = dsh::convert_package(
            &dir,
            &tmp.path()
                .join(format!("out-{}", spelling.replace(['/', '.'], "_"))),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("package.json"), "{spelling}: {err}");
        assert!(err.contains(spelling), "{spelling}: {err}");
    }

    let non_object_bundle = write_dsh_base_package(
        &tmp.path().join("non-object-dsh-bundle"),
        serde_json::json!({
            "name": "non-object",
            "version": "1.0.0",
            "dsh": {"bundle": "not-an-object"}
        }),
        "[]\n",
    );
    fs::write(
        non_object_bundle.join("plugin.json"),
        r#"{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"non-object"}"#,
    )
    .unwrap();
    assert_eq!(
        PluginInstallSource::parse(non_object_bundle.to_str().unwrap()).unwrap(),
        PluginInstallSource::Dsh(non_object_bundle.clone())
    );
    assert!(dsh::convert_package(&non_object_bundle, &tmp.path().join("out-non-obj")).is_err());
}

#[test]
fn plain_dsh_package_with_no_plugin_json_keeps_behavior_with_skills_dir_or_mcp_json() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = write_dsh_base_package(
        &tmp.path().join("plain-dsh"),
        serde_json::json!({
            "name": "plain-dsh",
            "version": "1.0.0",
            "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
        }),
        minimal_dsh_patch_with_skill(),
    );
    write_minimal_skill(&dir.join("custom-skills"), "guide");
    write_minimal_skill(&dir.join("skills"), "unreferenced");
    fs::write(dir.join("mcp.json"), r#"{"mcpServers":{}}"#).unwrap();

    let out = tmp.path().join("out-plain-dsh");
    let converted = dsh::convert_package(&dir, &out).unwrap();
    assert_eq!(converted.skills, ["guide"]);
    assert!(converted.commands.is_empty());
    assert!(converted.agents.is_empty());
    assert!(converted.hooks.is_empty());
    assert!(!out.join("skills/unreferenced").exists());
    assert!(!out.join("mcp.json").exists());

    let emitted_manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(out.join("plugin.json")).unwrap()).unwrap();
    let net_codewhale = emitted_manifest
        .get("extensions")
        .and_then(|ext| ext.get("net.codewhale"));
    for forbidden in ["commands", "agents", "hooks"] {
        assert!(
            net_codewhale.and_then(|obj| obj.get(forbidden)).is_none(),
            "{forbidden} must be omitted from plugin.json"
        );
    }
    let conversion_json: serde_json::Value =
        serde_json::from_slice(&fs::read(out.join("CONVERSION.json")).unwrap()).unwrap();
    for key in ["commands", "agents", "hooks"] {
        assert!(conversion_json.get(key).is_none());
        assert!(conversion_json["counts"].get(key).is_none());
    }
    let conversion_md = fs::read_to_string(out.join("CONVERSION.md")).unwrap();
    assert!(!conversion_md.contains("Merged from sibling plugin.json"));
}

#[test]
fn component_paths_in_sibling_plugin_json_cannot_escape_or_follow_links() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside.md");
    fs::write(&outside, "secret\n").unwrap();

    let make = |tag: &str, commands_path: &str| {
        let dir = write_dsh_base_package(
            &tmp.path().join(tag),
            serde_json::json!({
                "name": "sec-pkg",
                "version": "1.0.0",
                "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
            }),
            minimal_dsh_patch_with_skill(),
        );
        write_minimal_skill(&dir.join("custom-skills"), "guide");
        fs::write(
            dir.join("plugin.json"),
            serde_json::json!({
                "$schema": "https://agent-plugins.org/schemas/plugin.json",
                "name": "sec-pkg",
                "version": "1.0.0",
                "extensions": {"net.codewhale": {"commands": {"path": commands_path}}}
            })
            .to_string(),
        )
        .unwrap();
        dir
    };

    for (tag, bad_path) in [
        ("parent", "../outside.md"),
        ("abs", "/etc/passwd"),
        ("win-abs", "C:\\Users\\demo\\.ssh\\id_rsa"),
        ("dot-env", ".env"),
        ("ssh", ".ssh/id_rsa"),
        ("creds", "credentials.json"),
        ("dot", "."),
    ] {
        let dir = make(tag, bad_path);
        if bad_path == ".env" || bad_path == "credentials.json" {
            fs::write(dir.join(bad_path), "SECRET=1\n").unwrap();
        }
        assert!(
            dsh::convert_package(&dir, &tmp.path().join(format!("out-{tag}"))).is_err(),
            "{tag} ({bad_path}) must refuse"
        );
    }

    let hidden_in_dir = make("hidden-in-dir", "commands");
    fs::create_dir_all(hidden_in_dir.join("commands")).unwrap();
    fs::write(hidden_in_dir.join("commands/.env"), "SECRET=1\n").unwrap();
    assert!(dsh::convert_package(&hidden_in_dir, &tmp.path().join("out-hidden-in-dir")).is_err());

    #[cfg(unix)]
    {
        let linked_cmd = make("linked-cmd", "commands");
        fs::create_dir_all(linked_cmd.join("commands")).unwrap();
        std::os::unix::fs::symlink(&outside, linked_cmd.join("commands/link.md")).unwrap();
        assert!(dsh::convert_package(&linked_cmd, &tmp.path().join("out-linked-cmd")).is_err());

        let linked_pj = write_dsh_base_package(
            &tmp.path().join("linked-pj"),
            serde_json::json!({
                "name": "sec-pkg",
                "version": "1.0.0",
                "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
            }),
            minimal_dsh_patch_with_skill(),
        );
        write_minimal_skill(&linked_pj.join("custom-skills"), "guide");
        let outside_pj = tmp.path().join("outside-plugin.json");
        fs::write(
            &outside_pj,
            r#"{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"sec-pkg"}"#,
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside_pj, linked_pj.join("plugin.json")).unwrap();
        assert!(dsh::convert_package(&linked_pj, &tmp.path().join("out-linked-pj")).is_err());

        let hardlinked = make("hardlinked", "commands");
        fs::create_dir_all(hardlinked.join("commands")).unwrap();
        fs::hard_link(&outside, hardlinked.join("commands/hard.md")).unwrap();
        assert!(dsh::convert_package(&hardlinked, &tmp.path().join("out-hardlinked")).is_err());
    }
}

#[tokio::test]
async fn existing_codewhale_install_keeps_update_semantics_when_dsh_added() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let source = tmp.path().join("evolved-plugin");
    fs::create_dir_all(source.join("commands")).unwrap();
    fs::write(source.join("commands/ping.md"), "ping v1\n").unwrap();
    fs::write(
        source.join("plugin.json"),
        serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "evolved",
            "version": "1.0.0",
            "extensions": {"net.codewhale": {"commands": {"path": "commands"}}}
        })
        .to_string(),
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
        panic!("expected initial install");
    };
    let marker: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(installed.path.join(INSTALLED_FROM_MARKER)).unwrap(),
    )
    .unwrap();
    assert!(marker["spec"].as_str().unwrap().starts_with("path:"));

    fs::write(
        source.join("package.json"),
        serde_json::json!({
            "name": "evolved",
            "version": "1.0.0",
            "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
        })
        .to_string(),
    )
    .unwrap();
    fs::write(source.join("cordis.patch.yml"), "[]\n").unwrap();
    fs::write(source.join("commands/ping.md"), "ping v2\n").unwrap();

    assert_eq!(
        PluginInstallSource::parse(source.to_str().unwrap()).unwrap(),
        PluginInstallSource::Dsh(source.clone())
    );

    let updated = update("evolved", &plugins, DEFAULT_MAX_SIZE_BYTES, &allow_all())
        .await
        .unwrap();
    let PluginUpdateResult::Updated(updated_bundle) = updated else {
        panic!("expected LocalPath update");
    };
    assert_eq!(
        fs::read_to_string(updated_bundle.path.join("commands/ping.md")).unwrap(),
        "ping v2\n"
    );
    assert!(!updated_bundle.path.join("CONVERSION.json").exists());
}

#[tokio::test]
async fn merged_bundle_review_shows_commands_agents_hooks_and_fails_closed_on_tamper() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let pkg = write_dsh_base_package(
        &tmp.path().join("review-pkg"),
        serde_json::json!({
            "name": "review-pkg",
            "version": "1.0.0",
            "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
        }),
        "[]\n",
    );
    fs::create_dir_all(pkg.join("commands")).unwrap();
    fs::write(pkg.join("commands/hello.md"), "hello v1\n").unwrap();
    fs::create_dir_all(pkg.join("agents")).unwrap();
    fs::write(
        pkg.join("agents/scout.toml"),
        "id = \"scout\"\n[role]\nname = \"scout\"\ndescription = \"Scouts\"\nprompt = \"Scout.\"\n",
    )
    .unwrap();
    fs::create_dir_all(pkg.join("hooks")).unwrap();
    fs::write(
        pkg.join("hooks/hooks.toml"),
        "enabled = true\n[[hooks]]\nname = \"guard\"\nevent = \"turn_end\"\ncommand = \"true\"\n",
    )
    .unwrap();
    fs::write(
        pkg.join("plugin.json"),
        serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "review-pkg",
            "version": "1.0.0",
            "extensions": {
                "net.codewhale": {
                    "commands": {"path": "commands"},
                    "agents": {"path": "agents"},
                    "hooks": {"path": "hooks"}
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let (conversion, reviewed_hash) = preview_dsh(&pkg).unwrap();
    assert_eq!(conversion.commands, ["commands/hello.md"]);
    assert_eq!(conversion.agents, ["agents/scout.toml"]);
    assert_eq!(conversion.hooks, ["hooks/hooks.toml"]);

    fs::write(pkg.join("commands/hello.md"), "tampered\n").unwrap();
    let err = install_with_expected_content_hash(
        PluginInstallSource::parse(pkg.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        &no_conflict(),
        &reviewed_hash,
    )
    .await
    .unwrap_err();
    assert!(err.downcast_ref::<PluginContentMismatch>().is_some());
    assert!(!plugins.join("review-pkg").exists());

    let (updated_conversion, updated_hash) = preview_dsh(&pkg).unwrap();
    assert_eq!(updated_conversion.commands, ["commands/hello.md"]);
    let outcome = install_with_expected_content_hash(
        PluginInstallSource::parse(pkg.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        &no_conflict(),
        &updated_hash,
    )
    .await
    .unwrap();
    let PluginInstallOutcome::Installed(installed) = outcome else {
        panic!("expected install");
    };
    let receipt_md = fs::read_to_string(installed.path.join("CONVERSION.md")).unwrap();
    assert!(receipt_md.contains("commands/hello.md"), "{receipt_md}");
    assert!(receipt_md.contains("agents/scout.toml"), "{receipt_md}");
    assert!(receipt_md.contains("hooks/hooks.toml"), "{receipt_md}");
    let receipt_json: serde_json::Value =
        serde_json::from_slice(&fs::read(installed.path.join("CONVERSION.json")).unwrap()).unwrap();
    assert_eq!(receipt_json["counts"]["commands"], 1);
    assert_eq!(receipt_json["counts"]["agents"], 1);
    assert_eq!(receipt_json["counts"]["hooks"], 1);

    fs::write(pkg.join("commands/hello.md"), "hello v3\n").unwrap();
    let PluginUpdateResult::Updated(refreshed) =
        update("review-pkg", &plugins, DEFAULT_MAX_SIZE_BYTES, &allow_all())
            .await
            .unwrap()
    else {
        panic!("expected DSH update");
    };
    assert_eq!(
        fs::read_to_string(refreshed.path.join("commands/hello.md")).unwrap(),
        "hello v3\n"
    );
}

#[test]
fn package_with_native_code_and_plugin_json_commands_prevents_bytes_drift_and_collisions() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = write_dsh_base_package(
        &tmp.path().join("native-plus-cmd"),
        serde_json::json!({
            "name": "@demo/native-plus-cmd",
            "version": "1.0.0",
            "main": "index.js",
            "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
        }),
        "- insert:\n  - id: native-mod\n    name: '@demo/native-plus-cmd'\n",
    );
    fs::write(pkg.join("index.js"), "export function apply() {}\n").unwrap();
    fs::create_dir_all(pkg.join("extra")).unwrap();
    fs::write(pkg.join("extra/run.md"), "Run custom command.\n").unwrap();
    fs::create_dir_all(pkg.join("commands")).unwrap();
    fs::write(pkg.join("commands/built.md"), "Built-in command.\n").unwrap();
    fs::write(
        pkg.join("plugin.json"),
        serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "native-plus-cmd",
            "version": "1.0.0",
            "extensions": {
                "net.codewhale": {
                    "commands": {"paths": ["commands/built.md", "extra/run.md"]}
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let out = tmp.path().join("out-native-plus-cmd");
    let converted = dsh::convert_package(&pkg, &out).unwrap();
    assert!(converted.requires_native);
    assert_eq!(
        converted.commands,
        ["commands/built.md", "commands/extra/run.md"]
    );
    assert_eq!(
        fs::read(out.join("source/commands/built.md")).unwrap(),
        fs::read(out.join("commands/built.md")).unwrap()
    );
    assert_eq!(
        fs::read(out.join("source/extra/run.md")).unwrap(),
        fs::read(out.join("commands/extra/run.md")).unwrap()
    );
    let validated =
        crate::manifest::PluginManifest::validate_from_path(&out.join("plugin.json")).unwrap();
    assert_eq!(validated.inventory.native, 1);
    assert_eq!(validated.inventory.commands, 2);
}

#[test]
fn case_variant_plugin_json_beside_dsh_package_is_handled_on_case_insensitive_fs() {
    let tmp = tempfile::tempdir().unwrap();
    let probe = tmp.path().join("case_probe.txt");
    fs::write(&probe, "x").unwrap();
    let case_insensitive = fs::symlink_metadata(tmp.path().join("CASE_PROBE.TXT")).is_ok();

    let pkg = write_dsh_base_package(
        &tmp.path().join("case-pkg"),
        serde_json::json!({
            "name": "case-pkg",
            "version": "1.0.0",
            "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
        }),
        minimal_dsh_patch_with_skill(),
    );
    write_minimal_skill(&pkg.join("custom-skills"), "guide");
    fs::create_dir_all(pkg.join("commands")).unwrap();
    let crlf_cmd = "Line one\r\nLine two\r\n";
    fs::write(pkg.join("commands/hello.md"), crlf_cmd).unwrap();
    let crlf_pj = "{\r\n  \"$schema\": \"https://agent-plugins.org/schemas/plugin.json\",\r\n  \"name\": \"wrong-case-pkg\",\r\n  \"extensions\": {\"net.codewhale\": {\"commands\": {\"path\": \"commands\"}}}\r\n}\r\n";
    fs::write(pkg.join("Plugin.JSON"), crlf_pj).unwrap();

    if case_insensitive {
        assert_eq!(
            PluginInstallSource::parse(pkg.to_str().unwrap()).unwrap(),
            PluginInstallSource::Dsh(pkg.clone())
        );
        let err = dsh::convert_package(&pkg, &tmp.path().join("out-case-bad"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("plugin.json"), "{err}");
        assert!(err.contains("package.json"), "{err}");

        let valid_crlf_pj = "{\r\n  \"$schema\": \"https://agent-plugins.org/schemas/plugin.json\",\r\n  \"name\": \"case-pkg\",\r\n  \"version\": \"1.0.0\",\r\n  \"extensions\": {\"net.codewhale\": {\"commands\": {\"path\": \"commands\"}}}\r\n}\r\n";
        fs::write(pkg.join("Plugin.JSON"), valid_crlf_pj).unwrap();
        let out_ok = tmp.path().join("out-case-ok");
        let converted = dsh::convert_package(&pkg, &out_ok).unwrap();
        assert_eq!(converted.commands, ["commands/hello.md"]);
        assert_eq!(
            fs::read(out_ok.join("commands/hello.md")).unwrap(),
            crlf_cmd.as_bytes()
        );
    } else {
        let converted = dsh::convert_package(&pkg, &tmp.path().join("out-case-sens")).unwrap();
        assert!(converted.commands.is_empty());
    }
}

#[test]
fn tarball_with_dsh_package_and_plugin_json_merges_and_enforces_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let dsh_pkg = serde_json::to_vec(&serde_json::json!({
        "name": "@acme/tar-combo",
        "version": "1.0.0",
        "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
    }))
    .unwrap();
    let patch = minimal_dsh_patch_with_skill().as_bytes();
    let skill = b"---\nname: guide\ndescription: demo\n---\nbody\n";
    let cmd = b"# Tar command\n";
    let valid_pj = serde_json::to_vec(&serde_json::json!({
        "$schema": "https://agent-plugins.org/schemas/plugin.json",
        "name": "tar-combo",
        "version": "1.0.0",
        "extensions": {"net.codewhale": {"commands": {"path": "commands"}}}
    }))
    .unwrap();

    let ok_tar = tarball(&[
        ("repo/package.json", &dsh_pkg),
        ("repo/cordis.patch.yml", patch),
        ("repo/custom-skills/guide/SKILL.md", skill),
        ("repo/commands/hello.md", cmd),
        ("repo/plugin.json", &valid_pj),
    ]);
    let staged = tarball::stage_tarball(&ok_tar, tmp.path(), DEFAULT_MAX_SIZE_BYTES, None).unwrap();
    assert_eq!(staged.name, "tar-combo");
    assert!(staged.staged_path.join("commands/hello.md").is_file());
    assert!(staged.staged_path.join("CONVERSION.json").is_file());
    let validated = crate::manifest::PluginManifest::validate_from_path(
        &staged.staged_path.join("plugin.json"),
    )
    .unwrap();
    assert_eq!(validated.inventory.skills, 1);
    assert_eq!(validated.inventory.commands, 1);

    let forbidden_pj = serde_json::to_vec(&serde_json::json!({
        "$schema": "https://agent-plugins.org/schemas/plugin.json",
        "name": "tar-combo",
        "version": "1.0.0",
        "extensions": {"net.codewhale": {"native": {"path": "index.mjs"}}}
    }))
    .unwrap();
    let bad_tar = tarball(&[
        ("repo/package.json", &dsh_pkg),
        ("repo/cordis.patch.yml", patch),
        ("repo/custom-skills/guide/SKILL.md", skill),
        ("repo/index.mjs", b"export function apply() {}\n"),
        ("repo/plugin.json", &forbidden_pj),
    ]);
    assert!(tarball::stage_tarball(&bad_tar, tmp.path(), DEFAULT_MAX_SIZE_BYTES, None).is_err());

    let huge_pkg_json = vec![b' '; 1_048_577];
    let oversized_pkg_tar = tarball(&[
        ("repo/package.json", &huge_pkg_json),
        ("repo/plugin.json", &valid_pj),
    ]);
    assert!(
        tarball::stage_tarball(&oversized_pkg_tar, tmp.path(), DEFAULT_MAX_SIZE_BYTES, None)
            .is_err()
    );

    let multi_tar = tarball(&[
        ("a/package.json", &dsh_pkg),
        ("a/cordis.patch.yml", patch),
        ("a/custom-skills/guide/SKILL.md", skill),
        ("b/plugin.json", &valid_pj),
        ("b/commands/hello.md", cmd),
    ]);
    assert!(tarball::stage_tarball(&multi_tar, tmp.path(), DEFAULT_MAX_SIZE_BYTES, None).is_err());
}

#[test]
fn tarball_with_dsh_package_and_other_manifest_spelling_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let dsh_pkg = serde_json::to_vec(&serde_json::json!({
        "name": "other-tar",
        "version": "1.0.0",
        "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
    }))
    .unwrap();
    let patch = minimal_dsh_patch_with_skill().as_bytes();
    let skill = b"---\nname: guide\ndescription: demo\n---\nbody\n";

    for (path, body) in [
        (
            "repo/.claude-plugin/plugin.json",
            &br#"{"name":"other-tar"}"#[..],
        ),
        ("repo/kimi.plugin.json", &br#"{"name":"other-tar"}"#[..]),
    ] {
        let archive = tarball(&[
            ("repo/package.json", &dsh_pkg),
            ("repo/cordis.patch.yml", patch),
            ("repo/custom-skills/guide/SKILL.md", skill),
            (path, body),
        ]);
        let err = tarball::stage_tarball(&archive, tmp.path(), DEFAULT_MAX_SIZE_BYTES, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("package.json"), "{path}: {err}");
    }
}

#[test]
fn native_dsh_bundle_allows_root_skills_only_when_skill_filesystem_row_is_active() {
    let tmp = tempfile::tempdir().unwrap();
    let active_pkg = write_dsh_base_package(
        &tmp.path().join("native-active-skills"),
        serde_json::json!({
            "name": "@demo/native-skills",
            "version": "1.0.0",
            "main": "index.js",
            "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
        }),
        "- insert:\n  - id: native-mod\n    name: '@demo/native-skills'\n  - id: root-skills\n    name: '@deepseek-ai/dsh-skill-filesystem'\n    config:\n      customSkillDirs: [skills]\n",
    );
    fs::write(active_pkg.join("index.js"), "export function apply() {}\n").unwrap();
    write_minimal_skill(&active_pkg.join("skills"), "guide");
    fs::create_dir_all(active_pkg.join("commands")).unwrap();
    fs::write(active_pkg.join("commands/hello.md"), "# Hello\n").unwrap();
    fs::write(
        active_pkg.join("plugin.json"),
        serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "native-skills",
            "version": "1.0.0",
            "extensions": {"net.codewhale": {"commands": {"path": "commands"}}}
        })
        .to_string(),
    )
    .unwrap();

    let converted =
        dsh::convert_package(&active_pkg, &tmp.path().join("out-native-active")).unwrap();
    assert!(converted.requires_native);
    assert_eq!(converted.commands, ["commands/hello.md"]);

    let disabled_pkg = write_dsh_base_package(
        &tmp.path().join("native-disabled-skills"),
        serde_json::json!({
            "name": "@demo/native-skills",
            "version": "1.0.0",
            "main": "index.js",
            "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
        }),
        "- insert:\n  - id: native-mod\n    name: '@demo/native-skills'\n  - id: root-skills\n    name: '@deepseek-ai/dsh-skill-filesystem'\n    disabled: true\n    config:\n      customSkillDirs: [skills]\n",
    );
    fs::write(
        disabled_pkg.join("index.js"),
        "export function apply() {}\n",
    )
    .unwrap();
    write_minimal_skill(&disabled_pkg.join("skills"), "guide");
    fs::create_dir_all(disabled_pkg.join("commands")).unwrap();
    fs::write(disabled_pkg.join("commands/hello.md"), "# Hello\n").unwrap();
    fs::write(
        disabled_pkg.join("plugin.json"),
        fs::read(active_pkg.join("plugin.json")).unwrap(),
    )
    .unwrap();

    let err = dsh::convert_package(&disabled_pkg, &tmp.path().join("out-native-disabled"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("skills/"), "{err}");

    let missing_row_pkg = write_dsh_base_package(
        &tmp.path().join("native-missing-skills"),
        serde_json::json!({
            "name": "@demo/native-skills",
            "version": "1.0.0",
            "main": "index.js",
            "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
        }),
        "- insert:\n  - id: native-mod\n    name: '@demo/native-skills'\n",
    );
    fs::write(
        missing_row_pkg.join("index.js"),
        "export function apply() {}\n",
    )
    .unwrap();
    write_minimal_skill(&missing_row_pkg.join("skills"), "guide");
    fs::create_dir_all(missing_row_pkg.join("commands")).unwrap();
    fs::write(missing_row_pkg.join("commands/hello.md"), "# Hello\n").unwrap();
    fs::write(
        missing_row_pkg.join("plugin.json"),
        fs::read(active_pkg.join("plugin.json")).unwrap(),
    )
    .unwrap();
    let err = dsh::convert_package(&missing_row_pkg, &tmp.path().join("out-native-missing"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("skills/"), "{err}");
}

#[cfg(unix)]
#[tokio::test]
async fn hardlinks_are_rejected_in_local_bundle_and_dsh_routing() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");

    let outside_cmd = tmp.path().join("outside-cmd.md");
    fs::write(&outside_cmd, "outside\n").unwrap();
    let local_bundle = tmp.path().join("local-hardlink");
    fs::create_dir_all(local_bundle.join("commands")).unwrap();
    fs::write(
        local_bundle.join("plugin.json"),
        serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "local-hardlink",
            "version": "1.0.0",
            "extensions": {"net.codewhale": {"commands": {"path": "commands"}}}
        })
        .to_string(),
    )
    .unwrap();
    fs::hard_link(&outside_cmd, local_bundle.join("commands/linked.md")).unwrap();
    let err = install(
        PluginInstallSource::parse(local_bundle.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("hard link"),
        "expected hard link refusal, got: {err:#}"
    );

    let outside_pkg_json = tmp.path().join("outside-package.json");
    fs::write(
        &outside_pkg_json,
        serde_json::json!({
            "name": "hardlink-dsh",
            "version": "1.0.0",
            "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
        })
        .to_string(),
    )
    .unwrap();
    let dsh_hardlink = tmp.path().join("dsh-hardlink");
    fs::create_dir_all(&dsh_hardlink).unwrap();
    fs::hard_link(&outside_pkg_json, dsh_hardlink.join("package.json")).unwrap();
    fs::write(
        dsh_hardlink.join("plugin.json"),
        r#"{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"hardlink-dsh","version":"1.0.0"}"#,
    )
    .unwrap();
    assert!(PluginInstallSource::parse(dsh_hardlink.to_str().unwrap()).is_err());

    let bad_pkg_json = tmp.path().join("dsh-malformed");
    fs::create_dir_all(&bad_pkg_json).unwrap();
    fs::write(bad_pkg_json.join("package.json"), "{not-json").unwrap();
    fs::write(
        bad_pkg_json.join("plugin.json"),
        r#"{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"bad-pkg","version":"1.0.0"}"#,
    )
    .unwrap();
    assert!(PluginInstallSource::parse(bad_pkg_json.to_str().unwrap()).is_err());
}

#[tokio::test]
async fn plugin_json_only_folder_still_installs_by_its_plugin_json() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");

    let pure_src = tmp.path().join("pure-plugin");
    fs::create_dir_all(pure_src.join("commands")).unwrap();
    fs::write(pure_src.join("commands/hello.md"), "# Hello\n").unwrap();
    fs::write(
        pure_src.join("plugin.json"),
        serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "pure-plugin",
            "version": "1.0.0",
            "description": "Plain plugin.json",
            "extensions": {"net.codewhale": {"commands": {"path": "commands"}}}
        })
        .to_string(),
    )
    .unwrap();

    let source_parsed = PluginInstallSource::parse(pure_src.to_str().unwrap()).unwrap();
    assert_eq!(
        source_parsed,
        PluginInstallSource::LocalPath(pure_src.clone())
    );
    let expected =
        crate::manifest::PluginManifest::validate_from_path(&pure_src.join("plugin.json")).unwrap();
    let PluginInstallOutcome::Installed(installed_pure) = install(
        source_parsed,
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap() else {
        panic!("expected pure local install");
    };
    assert!(!installed_pure.path.join("CONVERSION.json").exists());
    let actual_pure = crate::manifest::PluginManifest::validate_from_path(
        &installed_pure.path.join("plugin.json"),
    )
    .unwrap();
    assert_eq!(installed_pure.content_hash, expected.content_hash);
    assert_eq!(
        actual_pure.content_hash,
        installed_pure.installed_content_hash
    );
    assert_eq!(actual_pure.capability_hash, expected.capability_hash);

    let npm_src = tmp.path().join("npm-helper-plugin");
    fs::create_dir_all(npm_src.join("commands")).unwrap();
    fs::write(npm_src.join("commands/hello.md"), "# Hello\n").unwrap();
    fs::write(
        npm_src.join("package.json"),
        serde_json::json!({
            "name": "@demo/helper-scripts",
            "version": "9.9.9",
            "scripts": {"test": "echo ok"}
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        npm_src.join("plugin.json"),
        serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "npm-helper-plugin",
            "version": "1.0.0",
            "description": "Plain plugin.json beside npm package.json",
            "extensions": {"net.codewhale": {"commands": {"path": "commands"}}}
        })
        .to_string(),
    )
    .unwrap();
    let expected_npm =
        crate::manifest::PluginManifest::validate_from_path(&npm_src.join("plugin.json")).unwrap();
    let npm_parsed = PluginInstallSource::parse(npm_src.to_str().unwrap()).unwrap();
    assert_eq!(npm_parsed, PluginInstallSource::LocalPath(npm_src.clone()));
    let PluginInstallOutcome::Installed(installed_npm) = install(
        npm_parsed,
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        false,
        &no_conflict(),
    )
    .await
    .unwrap() else {
        panic!("expected local install with ordinary package.json");
    };
    assert!(!installed_npm.path.join("CONVERSION.json").exists());
    let actual_npm = crate::manifest::PluginManifest::validate_from_path(
        &installed_npm.path.join("plugin.json"),
    )
    .unwrap();
    assert_eq!(actual_npm.manifest.plugin.name, "npm-helper-plugin");
    assert_eq!(installed_npm.content_hash, expected_npm.content_hash);
    assert_eq!(
        actual_npm.content_hash,
        installed_npm.installed_content_hash
    );
    assert_eq!(actual_npm.capability_hash, expected_npm.capability_hash);
}

#[tokio::test]
async fn a_hook_from_plugin_json_is_in_the_review_and_the_capability_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");

    let hook_a =
        "enabled = true\n[[hooks]]\nname = \"guard\"\nevent = \"turn_end\"\ncommand = \"true\"\n";
    let hook_b =
        "enabled = true\n[[hooks]]\nname = \"alt\"\nevent = \"turn_end\"\ncommand = \"true\"\n";

    let plain = tmp.path().join("plain-hook-bundle");
    fs::create_dir_all(plain.join("hooks")).unwrap();
    fs::write(plain.join("hooks/a.toml"), hook_a).unwrap();
    fs::write(
        plain.join("plugin.json"),
        serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "hook-parity",
            "version": "1.0.0",
            "extensions": {"net.codewhale": {"hooks": {"path": "hooks/a.toml"}}}
        })
        .to_string(),
    )
    .unwrap();
    let validated_plain =
        crate::manifest::PluginManifest::validate_from_path(&plain.join("plugin.json")).unwrap();

    let dsh_pkg = write_dsh_base_package(
        &tmp.path().join("dsh-hook-bundle"),
        serde_json::json!({
            "name": "hook-parity",
            "version": " 1.0.0 ",
            "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
        }),
        "[]\n",
    );
    fs::create_dir_all(dsh_pkg.join("hooks")).unwrap();
    fs::write(dsh_pkg.join("hooks/a.toml"), hook_a).unwrap();
    fs::write(dsh_pkg.join("hooks/b.toml"), hook_b).unwrap();
    fs::create_dir_all(dsh_pkg.join("empty-commands")).unwrap();
    let sibling_pj_a = serde_json::json!({
        "$schema": "https://agent-plugins.org/schemas/plugin.json",
        "name": "hook-parity",
        "version": "1.0.0",
        "extensions": {
            "net.codewhale": {
                "commands": {"path": "empty-commands"},
                "hooks": {"path": "hooks/a.toml"}
            }
        }
    })
    .to_string();
    fs::write(dsh_pkg.join("plugin.json"), &sibling_pj_a).unwrap();

    let (previewed, reviewed_hash) = preview_dsh(&dsh_pkg).unwrap();
    assert_eq!(previewed.hooks, ["hooks/a.toml"]);
    assert!(previewed.commands.is_empty());

    let PluginInstallOutcome::Installed(installed) = install_with_expected_content_hash(
        PluginInstallSource::parse(dsh_pkg.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        &no_conflict(),
        &reviewed_hash,
    )
    .await
    .unwrap() else {
        panic!("expected install");
    };
    let validated_dsh =
        crate::manifest::PluginManifest::validate_from_path(&installed.path.join("plugin.json"))
            .unwrap();
    assert_eq!(
        serde_json::to_value(&validated_dsh.manifest.capabilities).unwrap(),
        serde_json::to_value(&validated_plain.manifest.capabilities).unwrap()
    );
    assert_eq!(
        validated_dsh.capability_hash,
        validated_plain.capability_hash
    );
    assert!(installed.path.join("hooks/a.toml").is_file());
    assert!(!installed.path.join("empty-commands").exists());

    fs::write(
        dsh_pkg.join("hooks/a.toml"),
        hook_a.replace("\"true\"", "\"false\""),
    )
    .unwrap();
    let err_hook = install_with_expected_content_hash(
        PluginInstallSource::parse(dsh_pkg.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        &no_conflict(),
        &reviewed_hash,
    )
    .await
    .unwrap_err();
    assert!(err_hook.downcast_ref::<PluginContentMismatch>().is_some());

    fs::write(dsh_pkg.join("hooks/a.toml"), hook_a).unwrap();
    let (_, clean_hash) = preview_dsh(&dsh_pkg).unwrap();
    assert_eq!(clean_hash, reviewed_hash);

    fs::write(
        dsh_pkg.join("plugin.json"),
        sibling_pj_a.replace("hooks/a.toml", "hooks/b.toml"),
    )
    .unwrap();
    let err_pj = install_with_expected_content_hash(
        PluginInstallSource::parse(dsh_pkg.to_str().unwrap()).unwrap(),
        &plugins,
        DEFAULT_MAX_SIZE_BYTES,
        &allow_all(),
        &no_conflict(),
        &reviewed_hash,
    )
    .await
    .unwrap_err();
    assert!(err_pj.downcast_ref::<PluginContentMismatch>().is_some());

    let empty_only = write_dsh_base_package(
        &tmp.path().join("empty-only-cmd"),
        serde_json::json!({
            "name": "empty-only-cmd",
            "version": "1.0.0",
            "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
        }),
        "[]\n",
    );
    fs::create_dir_all(empty_only.join("empty-commands")).unwrap();
    fs::write(
        empty_only.join("plugin.json"),
        serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "empty-only-cmd",
            "version": "1.0.0",
            "extensions": {"net.codewhale": {"commands": {"path": "empty-commands"}}}
        })
        .to_string(),
    )
    .unwrap();
    assert!(dsh::convert_package(&empty_only, &tmp.path().join("out-empty-only")).is_err());
}
