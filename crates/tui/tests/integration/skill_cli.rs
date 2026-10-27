//! Integration tests for the community-skill CLI flows (#140).
//!
//! Keep this file name free of `install`: Windows can treat unmanifested test
//! binaries with installer-like names as elevation-required programs.
//!
//! These tests exercise the full validation pipeline against a tiny in-process
//! HTTP server, so the network gate, download cap, tarball validation, atomic
//! rename, and `.installed-from` marker all run end-to-end through the public
//! `codewhale-skill-install` surface (its `test-support` feature is a tui
//! dev-dependency only).

use std::io::Write;
use std::path::Path;

use flate2::Compression;
use flate2::write::GzEncoder;
use tempfile::TempDir;
use tiny_http::{Method, Response, Server};

use codewhale_net::network_policy::{DecisionToml, NetworkPolicy};
use codewhale_skill_install::install::{InstallOutcome, InstallSource, UpdateResult};

/// Construct a gzipped tarball from `(path, body)` pairs. Permissions are set
/// to 0o644 so umask differences across platforms don't perturb the bytes.
fn make_tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = tar::Builder::new(&mut gz);
        for (path, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, *body)
                .expect("append_data");
        }
        builder.finish().expect("finish tar");
    }
    gz.finish().expect("finish gz")
}

fn skill_md(name: &str, description: &str) -> Vec<u8> {
    format!(
        "---\nname: {name}\ndescription: {description}\n---\n# {name}\n\nThis is a test skill.\n"
    )
    .into_bytes()
}

fn allow_all_policy() -> NetworkPolicy {
    NetworkPolicy {
        default: DecisionToml::Allow,
        allow: Vec::new(),
        deny: Vec::new(),
        proxy: Vec::new(),
        proxy_fake_ip_cidrs: Vec::new(),
        audit: false,
    }
}

fn deny_all_policy() -> NetworkPolicy {
    NetworkPolicy {
        default: DecisionToml::Deny,
        allow: Vec::new(),
        deny: Vec::new(),
        proxy: Vec::new(),
        proxy_fake_ip_cidrs: Vec::new(),
        audit: false,
    }
}

fn prompt_all_policy() -> NetworkPolicy {
    NetworkPolicy {
        default: DecisionToml::Prompt,
        allow: Vec::new(),
        deny: Vec::new(),
        proxy: Vec::new(),
        proxy_fake_ip_cidrs: Vec::new(),
        audit: false,
    }
}

/// Spawn a tiny HTTP server that serves `bytes` at any path with 200 OK and
/// returns the bound URL. The server replies to *every* request (we re-use it
/// across multiple installs in the same test).
fn spawn_tarball_server(
    bytes: Vec<u8>,
) -> (
    String,
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<()>,
) {
    let server = Server::http("127.0.0.1:0").expect("bind ephemeral port");
    let url = format!(
        "http://{}/skill.tar.gz",
        server.server_addr().to_ip().expect("ip addr")
    );
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        loop {
            // Poll-style with a small recv timeout so we can break out cleanly.
            match server.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(Some(req)) => {
                    if req.method() != &Method::Get {
                        continue;
                    }
                    let response = Response::from_data(bytes.clone());
                    let _ = req.respond(response);
                }
                Ok(None) => {}
                Err(_) => break,
            }
            if shutdown_rx.try_recv().is_ok() {
                break;
            }
        }
    });
    (url, shutdown_tx, handle)
}

fn shutdown(tx: std::sync::mpsc::Sender<()>, handle: std::thread::JoinHandle<()>) {
    let _ = tx.send(());
    let _ = handle.join();
}

#[tokio::test]
async fn install_happy_path_writes_skill_and_marker() {
    let tarball = make_tarball(&[
        (
            "test-skill-main/SKILL.md",
            &skill_md("test-skill", "Test skill"),
        ),
        ("test-skill-main/notes.txt", b"hello world"),
    ]);
    let (url, tx, handle) = spawn_tarball_server(tarball);

    let tmp = TempDir::new().unwrap();
    let policy = allow_all_policy();

    let outcome = codewhale_skill_install::install::install(
        InstallSource::DirectUrl(url),
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
        false,
    )
    .await
    .expect("install ok");

    let installed = match outcome {
        InstallOutcome::Installed(s) => s,
        other => panic!("expected Installed, got {other:?}"),
    };
    assert_eq!(installed.name, "test-skill");

    let installed_dir = tmp.path().join("test-skill");
    assert!(installed_dir.is_dir(), "skill dir created");
    assert!(installed_dir.join("SKILL.md").is_file(), "SKILL.md present");
    assert!(
        installed_dir.join("notes.txt").is_file(),
        "extra file present"
    );
    assert!(
        installed_dir
            .join(codewhale_skill_install::install::INSTALLED_FROM_MARKER)
            .is_file(),
        ".installed-from marker present"
    );
    let marker = std::fs::read_to_string(
        installed_dir.join(codewhale_skill_install::install::INSTALLED_FROM_MARKER),
    )
    .expect("read marker");
    assert!(
        marker.contains("\"schema_version\": 2") || marker.contains("\"schema_version\":2"),
        "install must write metadata v2: {marker}"
    );
    assert!(
        marker.contains("content_digest"),
        "install must bind content_digest: {marker}"
    );

    shutdown(tx, handle);
}

#[tokio::test]
async fn install_rejects_path_traversal() {
    // `tar::Builder::append_data` rejects `..` itself, so we craft the bad
    // entry by writing the raw header bytes via `append`.
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = tar::Builder::new(&mut gz);
        let body = skill_md("test-skill", "T");
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(body.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_cksum();
        builder
            .append_data(&mut hdr, "test-skill-main/SKILL.md", body.as_slice())
            .unwrap();

        // Path-traversal entry. The `tar` crate's `set_path` rejects `..`
        // itself, so we patch the raw 100-byte name field in the header.
        let evil_body: &[u8] = b"not gonna happen";
        let mut evil_hdr = tar::Header::new_gnu();
        evil_hdr.set_size(evil_body.len() as u64);
        evil_hdr.set_mode(0o644);
        // Write a name with a `..` directly into the legacy "name" field.
        let bytes = evil_hdr.as_old_mut();
        let evil_name = b"../etc/passwd";
        bytes.name[..evil_name.len()].copy_from_slice(evil_name);
        evil_hdr.set_cksum();
        builder.append(&evil_hdr, evil_body).unwrap();
        builder.finish().unwrap();
    }
    let tarball = gz.finish().unwrap();
    let (url, tx, handle) = spawn_tarball_server(tarball);

    let tmp = TempDir::new().unwrap();
    let policy = allow_all_policy();
    let err = codewhale_skill_install::install::install(
        InstallSource::DirectUrl(url),
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
        false,
    )
    .await
    .expect_err("path traversal must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("escapes destination"),
        "expected path-traversal error, got: {msg}"
    );

    shutdown(tx, handle);
}

#[tokio::test]
async fn install_rejects_oversized_tarball() {
    let big = vec![b'a'; 256 * 1024]; // 256 KiB per file
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    entries.push((
        "test-skill-main/SKILL.md".to_string(),
        skill_md("test-skill", "T"),
    ));
    for i in 0..50 {
        entries.push((format!("test-skill-main/big-{i}.bin"), big.clone()));
    }
    let entry_refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_slice()))
        .collect();
    let tarball = make_tarball(&entry_refs);
    let (url, tx, handle) = spawn_tarball_server(tarball);

    let tmp = TempDir::new().unwrap();
    let policy = allow_all_policy();
    let small_cap = 1024 * 1024;
    let err = codewhale_skill_install::install::install(
        InstallSource::DirectUrl(url),
        tmp.path(),
        small_cap,
        &policy,
        false,
    )
    .await
    .expect_err("oversized must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("too large") || msg.contains("exceed"),
        "expected size cap error, got: {msg}"
    );

    shutdown(tx, handle);
}

#[tokio::test]
async fn install_rejects_missing_skill_md() {
    let tarball = make_tarball(&[("repo-main/README.md", b"not a skill")]);
    let (url, tx, handle) = spawn_tarball_server(tarball);

    let tmp = TempDir::new().unwrap();
    let policy = allow_all_policy();
    let err = codewhale_skill_install::install::install(
        InstallSource::DirectUrl(url),
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
        false,
    )
    .await
    .expect_err("missing SKILL.md must be rejected");
    assert!(format!("{err:#}").contains("missing SKILL.md"), "{err:#}");

    shutdown(tx, handle);
}

#[tokio::test]
async fn install_accepts_claude_compatible_skill_directory_archive() {
    let tarball = make_tarball(&[
        (
            "repo-main/.claude/skills/workflow-pack/SKILL.md",
            &skill_md("workflow-pack", "Workflow pack"),
        ),
        (
            "repo-main/.claude/skills/workflow-pack/scripts/check.sh",
            b"echo ok",
        ),
        ("repo-main/README.md", b"outside the selected skill subtree"),
    ]);
    let (url, tx, handle) = spawn_tarball_server(tarball);

    let tmp = TempDir::new().unwrap();
    let policy = allow_all_policy();
    let outcome = codewhale_skill_install::install::install(
        InstallSource::DirectUrl(url),
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
        false,
    )
    .await
    .expect("claude-compatible skill dir should install");
    let installed = match outcome {
        InstallOutcome::Installed(installed) => installed,
        other => panic!("expected Installed, got {other:?}"),
    };

    assert_eq!(installed.name, "workflow-pack");
    assert!(installed.path.join("SKILL.md").is_file());
    assert!(installed.path.join("scripts/check.sh").is_file());
    assert!(!installed.path.join("README.md").exists());

    shutdown(tx, handle);
}

#[tokio::test]
async fn install_accepts_nested_workflow_pack_skill_directory() {
    let tarball = make_tarball(&[
        (
            "repo-main/packages/superpowers/5.1.0/skills/using-superpowers/SKILL.md",
            &skill_md("using-superpowers", "Use Superpowers workflow"),
        ),
        (
            "repo-main/packages/superpowers/5.1.0/skills/using-superpowers/references/guide.md",
            b"guide",
        ),
    ]);
    let (url, tx, handle) = spawn_tarball_server(tarball);

    let tmp = TempDir::new().unwrap();
    let policy = allow_all_policy();
    let outcome = codewhale_skill_install::install::install(
        InstallSource::DirectUrl(url),
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
        false,
    )
    .await
    .expect("nested workflow-pack skill dir should install");
    let installed = match outcome {
        InstallOutcome::Installed(installed) => installed,
        other => panic!("expected Installed, got {other:?}"),
    };

    assert_eq!(installed.name, "using-superpowers");
    assert!(installed.path.join("SKILL.md").is_file());
    assert!(installed.path.join("references/guide.md").is_file());

    shutdown(tx, handle);
}

#[tokio::test]
async fn install_rejects_multi_skill_claude_plugin_archive() {
    let tarball = make_tarball(&[
        (
            "repo-main/.claude-plugin/plugin.json",
            br#"{"name":"workflow-pack","version":"1.0.0"}"#,
        ),
        (
            "repo-main/skills/plan/SKILL.md",
            &skill_md("plan", "Planning skill"),
        ),
        (
            "repo-main/skills/review/SKILL.md",
            &skill_md("review", "Review skill"),
        ),
    ]);
    let (url, tx, handle) = spawn_tarball_server(tarball);

    let tmp = TempDir::new().unwrap();
    let policy = allow_all_policy();
    let err = codewhale_skill_install::install::install(
        InstallSource::DirectUrl(url),
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
        false,
    )
    .await
    .expect_err("multi-skill Claude plugin archive should not be flattened");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("Claude Code plugin archive contains multiple SKILL.md entries"),
        "expected Claude plugin compatibility error, got: {msg}"
    );
    assert!(
        std::fs::read_dir(tmp.path()).unwrap().next().is_none(),
        "rejected plugin archive must not write an installed skill"
    );

    shutdown(tx, handle);
}

#[tokio::test]
async fn install_accepts_single_skill_subdirectory_archive() {
    let tarball = make_tarball(&[
        (
            "repo-main/my-workflow/SKILL.md",
            &skill_md("my-workflow", "Nested workflow"),
        ),
        ("repo-main/my-workflow/examples/example.md", b"example"),
        ("repo-main/README.md", b"outside the selected skill subtree"),
    ]);
    let (url, tx, handle) = spawn_tarball_server(tarball);

    let tmp = TempDir::new().unwrap();
    let policy = allow_all_policy();
    let outcome = codewhale_skill_install::install::install(
        InstallSource::DirectUrl(url),
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
        false,
    )
    .await
    .expect("single nested skill dir should install");
    let installed = match outcome {
        InstallOutcome::Installed(installed) => installed,
        other => panic!("expected Installed, got {other:?}"),
    };

    assert_eq!(installed.name, "my-workflow");
    assert!(installed.path.join("SKILL.md").is_file());
    assert!(installed.path.join("examples/example.md").is_file());
    assert!(!installed.path.join("README.md").exists());

    shutdown(tx, handle);
}

#[tokio::test]
async fn install_rejects_missing_required_frontmatter() {
    let tarball = make_tarball(&[("repo-main/SKILL.md", b"---\nname: test\n---\nbody\n")]);
    let (url, tx, handle) = spawn_tarball_server(tarball);

    let tmp = TempDir::new().unwrap();
    let policy = allow_all_policy();
    let err = codewhale_skill_install::install::install(
        InstallSource::DirectUrl(url),
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
        false,
    )
    .await
    .expect_err("missing description must be rejected");
    assert!(format!("{err:#}").contains("description"), "{err:#}");

    shutdown(tx, handle);
}

#[tokio::test]
async fn install_idempotent_then_uninstall_then_reinstall() {
    let tarball_bytes =
        make_tarball(&[("repo-main/SKILL.md", &skill_md("idem-skill", "Idempotent"))]);
    let (url, tx, handle) = spawn_tarball_server(tarball_bytes);

    let tmp = TempDir::new().unwrap();
    let policy = allow_all_policy();

    codewhale_skill_install::install::install(
        InstallSource::DirectUrl(url.clone()),
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
        false,
    )
    .await
    .expect("first install ok");

    // Second install with `update = false` must reject.
    let err = codewhale_skill_install::install::install(
        InstallSource::DirectUrl(url.clone()),
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
        false,
    )
    .await
    .expect_err("second install must reject");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("already installed"),
        "expected already-installed error, got: {msg}"
    );

    // Uninstall then reinstall.
    codewhale_skill_install::install::uninstall("idem-skill", tmp.path()).expect("uninstall ok");
    assert!(!tmp.path().join("idem-skill").exists());

    codewhale_skill_install::install::install(
        InstallSource::DirectUrl(url),
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
        false,
    )
    .await
    .expect("reinstall ok");

    assert!(tmp.path().join("idem-skill").join("SKILL.md").is_file());
    shutdown(tx, handle);
}

#[tokio::test]
async fn update_no_change_returns_nochange_without_overwriting() {
    let tarball_bytes =
        make_tarball(&[("repo-main/SKILL.md", &skill_md("upd-skill", "Update test"))]);
    let (url, tx, handle) = spawn_tarball_server(tarball_bytes);
    let tmp = TempDir::new().unwrap();
    let policy = allow_all_policy();

    codewhale_skill_install::install::install(
        InstallSource::DirectUrl(url.clone()),
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
        false,
    )
    .await
    .unwrap();

    // Patch the marker so update() re-fetches the same URL.
    let marker_path = tmp
        .path()
        .join("upd-skill")
        .join(codewhale_skill_install::install::INSTALLED_FROM_MARKER);
    let marker_body = std::fs::read_to_string(&marker_path).unwrap();
    let mut marker_json: serde_json::Value = serde_json::from_str(&marker_body).unwrap();
    marker_json["spec"] = serde_json::Value::String(url);
    std::fs::write(&marker_path, marker_json.to_string()).unwrap();

    // Capture mtime so we can confirm SKILL.md wasn't rewritten.
    let skill_md_path = tmp.path().join("upd-skill").join("SKILL.md");
    let mtime_before = std::fs::metadata(&skill_md_path)
        .unwrap()
        .modified()
        .unwrap();

    let result = codewhale_skill_install::install::update(
        "upd-skill",
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
    )
    .await
    .expect("update ok");
    assert!(matches!(result, UpdateResult::NoChange));

    let mtime_after = std::fs::metadata(&skill_md_path)
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(mtime_before, mtime_after, "SKILL.md must not be rewritten");
    shutdown(tx, handle);
}

#[tokio::test]
async fn install_with_deny_policy_returns_network_denied() {
    let tmp = TempDir::new().unwrap();
    let policy = deny_all_policy();
    let outcome = codewhale_skill_install::install::install(
        InstallSource::DirectUrl("https://example.invalid/skill.tar.gz".to_string()),
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
        false,
    )
    .await
    .expect("policy outcome should be Ok");
    match outcome {
        InstallOutcome::NetworkDenied(host) => {
            assert!(host.contains("example.invalid"), "got host {host}");
        }
        other => panic!("expected NetworkDenied, got {other:?}"),
    }

    // Verify the temp dir is untouched.
    assert!(
        std::fs::read_dir(tmp.path()).unwrap().next().is_none(),
        "temp dir must be untouched"
    );
}

#[tokio::test]
async fn install_with_prompt_policy_returns_needs_approval() {
    let tmp = TempDir::new().unwrap();
    let policy = prompt_all_policy();
    let outcome = codewhale_skill_install::install::install(
        InstallSource::DirectUrl("https://example.invalid/skill.tar.gz".to_string()),
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
        false,
    )
    .await
    .expect("policy outcome should be Ok");
    match outcome {
        InstallOutcome::NeedsApproval(host) => {
            assert!(host.contains("example.invalid"), "got host {host}");
        }
        other => panic!("expected NeedsApproval, got {other:?}"),
    }
    assert!(
        std::fs::read_dir(tmp.path()).unwrap().next().is_none(),
        "temp dir must be untouched on prompt"
    );
}

#[tokio::test]
async fn install_rejects_symlink_entry() {
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = tar::Builder::new(&mut gz);

        let body = skill_md("link-skill", "x");
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(body.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_cksum();
        builder
            .append_data(&mut hdr, "repo-main/SKILL.md", body.as_slice())
            .unwrap();

        let mut link_hdr = tar::Header::new_gnu();
        link_hdr.set_entry_type(tar::EntryType::Symlink);
        link_hdr.set_size(0);
        link_hdr.set_mode(0o777);
        builder
            .append_link(&mut link_hdr, "repo-main/escape", Path::new("/etc/passwd"))
            .unwrap();
        builder.finish().unwrap();
    }
    let tarball = gz.finish().unwrap();
    let (url, tx, handle) = spawn_tarball_server(tarball);

    let tmp = TempDir::new().unwrap();
    let policy = allow_all_policy();
    let err = codewhale_skill_install::install::install(
        InstallSource::DirectUrl(url),
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
        false,
    )
    .await
    .expect_err("symlinks must be rejected");
    assert!(format!("{err:#}").contains("symlink"), "{err:#}");

    shutdown(tx, handle);
}

#[tokio::test]
async fn install_ignores_symlink_outside_selected_skill_root() {
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = tar::Builder::new(&mut gz);

        let mut link_hdr = tar::Header::new_gnu();
        link_hdr.set_entry_type(tar::EntryType::Symlink);
        link_hdr.set_size(0);
        link_hdr.set_mode(0o777);
        builder
            .append_link(&mut link_hdr, "repo-main/AGENTS.md", Path::new("CLAUDE.md"))
            .unwrap();

        let body = skill_md("nested-skill", "Nested skill");
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(body.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_cksum();
        builder
            .append_data(
                &mut hdr,
                "repo-main/skills/nested-skill/SKILL.md",
                body.as_slice(),
            )
            .unwrap();

        let notes = b"selected subtree only";
        let mut notes_hdr = tar::Header::new_gnu();
        notes_hdr.set_size(notes.len() as u64);
        notes_hdr.set_mode(0o644);
        notes_hdr.set_cksum();
        builder
            .append_data(
                &mut notes_hdr,
                "repo-main/skills/nested-skill/notes.txt",
                notes.as_slice(),
            )
            .unwrap();

        builder.finish().unwrap();
    }
    let tarball = gz.finish().unwrap();
    let (url, tx, handle) = spawn_tarball_server(tarball);

    let tmp = TempDir::new().unwrap();
    let policy = allow_all_policy();
    let outcome = codewhale_skill_install::install::install(
        InstallSource::DirectUrl(url),
        tmp.path(),
        codewhale_skill_install::install::DEFAULT_MAX_SIZE_BYTES,
        &policy,
        false,
    )
    .await
    .expect("repo-level symlink outside selected skill root should be ignored");
    let installed = match outcome {
        InstallOutcome::Installed(installed) => installed,
        other => panic!("expected Installed, got {other:?}"),
    };

    assert_eq!(installed.name, "nested-skill");
    assert!(installed.path.join("SKILL.md").exists());
    assert!(installed.path.join("notes.txt").exists());
    assert!(!installed.path.join("AGENTS.md").exists());

    shutdown(tx, handle);
}

#[test]
fn uninstall_refuses_system_skill() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("system-skill");
    std::fs::create_dir_all(&dir).unwrap();
    let mut f = std::fs::File::create(dir.join("SKILL.md")).unwrap();
    f.write_all(b"---\nname: system-skill\ndescription: x\n---\n")
        .unwrap();
    // No `.installed-from` marker — looks like a system skill.

    let err = codewhale_skill_install::install::uninstall("system-skill", tmp.path())
        .expect_err("must refuse");
    assert!(format!("{err:#}").contains("not installed via"));
    assert!(dir.exists(), "directory must be left alone");
}
