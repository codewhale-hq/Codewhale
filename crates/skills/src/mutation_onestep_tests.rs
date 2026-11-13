use super::*;
use codewhale_net::network_policy::NetworkPolicy;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

struct Fixture {
    _tmp: TempDir,
    workspace: PathBuf,
    home: PathBuf,
    network: NetworkPolicy,
}

impl Fixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&home).unwrap();
        Self {
            _tmp: tmp,
            workspace,
            home,
            network: NetworkPolicy {
                allow: vec!["127.0.0.1".into()],
                ..NetworkPolicy::default()
            },
        }
    }

    fn ctx(&self) -> MutationContext<'_> {
        MutationContext {
            workspace: &self.workspace,
            home: Some(&self.home),
            configured_skills_dir: None,
            network: &self.network,
            max_size: install::DEFAULT_MAX_SIZE_BYTES,
            registry_url: install::DEFAULT_REGISTRY_URL,
        }
    }

    fn source(&self, name: &str, body: &str) -> PathBuf {
        let dir = self.workspace.join("src").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: d\n---\n{body}\n"),
        )
        .unwrap();
        dir
    }

    fn skills_dir(&self, target: SkillTargetScope) -> PathBuf {
        match target {
            SkillTargetScope::Project => self.workspace.join(".codewhale").join("skills"),
            SkillTargetScope::Global => self.home.join(".codewhale").join("skills"),
        }
    }
}

async fn run(
    fx: &Fixture,
    initiator: Option<SkillInitiator>,
    request: SkillOneStepRequest,
) -> Result<SkillOneStepReceipt> {
    execute_one_step(request, initiator, &fx.ctx()).await
}

fn install_spec(spec: &str, target: SkillTargetScope) -> SkillOneStepRequest {
    SkillOneStepRequest::Install {
        spec: spec.to_string(),
        target,
    }
}

fn path_spec(dir: &Path) -> String {
    format!("path:{}", dir.display())
}

#[test]
fn local_path_spec_table() {
    for local in [
        "path:foo",
        "./foo",
        "../foo",
        "/abs/foo",
        "~/foo",
        "~",
        ".",
        "..",
        "C:\\skills\\foo",
        "c:/skills/foo",
        "D:foo",
        "\\\\server\\share\\foo",
        "\\foo",
        ".\\foo",
        "..\\foo",
        "dir\\foo",
    ] {
        assert!(local_path_spec(local).is_some(), "{local}");
    }
    for remote in [
        "review-pr",
        "github:o/r",
        "https://example.com/a.tar.gz",
        "http://127.0.0.1:1/a.tar.gz",
        "my.skill",
    ] {
        assert!(local_path_spec(remote).is_none(), "{remote}");
    }
    assert_eq!(local_path_spec("path:  x y "), Some("x y"));
}

#[tokio::test]
async fn folder_install_update_and_no_change() {
    let fx = Fixture::new();
    let src = fx.source("folder-skill", "one");
    let spec = path_spec(&src);

    let first = run(&fx, None, install_spec(&spec, SkillTargetScope::Project))
        .await
        .unwrap();
    assert_eq!(first.outcome, SkillMutationOutcome::Installed);
    assert!(
        first
            .summary
            .starts_with("Installed skill folder-skill (project) from path:")
    );

    let again = run(&fx, None, install_spec(&spec, SkillTargetScope::Project))
        .await
        .unwrap();
    assert_eq!(again.outcome, SkillMutationOutcome::NoChange);

    fs::write(
        src.join("SKILL.md"),
        "---\nname: folder-skill\ndescription: d\n---\ntwo\n",
    )
    .unwrap();
    let updated = run(
        &fx,
        None,
        install_spec("./src/folder-skill", SkillTargetScope::Project),
    )
    .await
    .unwrap();
    assert_eq!(updated.outcome, SkillMutationOutcome::Updated);
    let dest = fx
        .skills_dir(SkillTargetScope::Project)
        .join("folder-skill");
    assert!(
        fs::read_to_string(dest.join("SKILL.md"))
            .unwrap()
            .contains("two")
    );
    let marker = read_installed_marker(&dest).unwrap().unwrap();
    assert!(marker.spec.starts_with("path:"));
    assert!(!dest.with_extension("bak").exists());
}

#[tokio::test]
async fn path_provenance_update_copies_again() {
    let fx = Fixture::new();
    let src = fx.source("redo", "one");
    run(
        &fx,
        None,
        install_spec(&path_spec(&src), SkillTargetScope::Project),
    )
    .await
    .unwrap();
    fs::write(
        src.join("SKILL.md"),
        "---\nname: redo\ndescription: d\n---\nchanged\n",
    )
    .unwrap();
    let receipt = run(
        &fx,
        Some(SkillInitiator::Person),
        SkillOneStepRequest::Mutation(SkillMutationRequest::UpdateByName {
            name: "redo".into(),
            scope: Some(SkillTargetScope::Project),
            expected_digest: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(receipt.outcome, SkillMutationOutcome::Updated);
    assert_eq!(receipt.action, SkillActionKind::Update);
    let dest = fx.skills_dir(SkillTargetScope::Project).join("redo");
    assert!(
        fs::read_to_string(dest.join("SKILL.md"))
            .unwrap()
            .contains("changed")
    );
}

#[tokio::test]
async fn install_over_unmarked_directory_is_refused() {
    let fx = Fixture::new();
    let src = fx.source("held", "new");
    let existing = fx.skills_dir(SkillTargetScope::Project).join("held");
    fs::create_dir_all(&existing).unwrap();
    fs::write(
        existing.join("SKILL.md"),
        "---\nname: held\ndescription: d\n---\nmine\n",
    )
    .unwrap();
    let err = run(
        &fx,
        None,
        install_spec(&path_spec(&src), SkillTargetScope::Project),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("not installed by Codewhale"),
        "{err}"
    );
    assert!(
        fs::read_to_string(existing.join("SKILL.md"))
            .unwrap()
            .contains("mine")
    );
}

#[tokio::test]
async fn missing_skill_md_and_file_sources_are_refused() {
    let fx = Fixture::new();
    let empty = fx.workspace.join("empty");
    fs::create_dir_all(&empty).unwrap();
    let err = run(
        &fx,
        None,
        install_spec(&path_spec(&empty), SkillTargetScope::Project),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("no SKILL.md"), "{err}");

    let archive = fx.workspace.join("pack.txt");
    fs::write(&archive, b"x").unwrap();
    let err = run(
        &fx,
        None,
        install_spec(&path_spec(&archive), SkillTargetScope::Project),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains(".tar.gz / .tgz archives"), "{err}");
    let corrupt = fx.workspace.join("corrupt.tar.gz");
    fs::write(&corrupt, b"not-gzip").unwrap();
    assert!(
        run(
            &fx,
            None,
            install_spec(&path_spec(&corrupt), SkillTargetScope::Project),
        )
        .await
        .is_err()
    );
    assert!(
        !fx.skills_dir(SkillTargetScope::Project)
            .join("empty")
            .exists()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_are_refused() {
    let fx = Fixture::new();
    let real = fx.source("linked", "body");
    let link = fx.workspace.join("link-to-skill");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let err = run(
        &fx,
        None,
        install_spec(&path_spec(&link), SkillTargetScope::Project),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("symlinked"), "{err}");

    let outside = fx.workspace.join("secret.txt");
    fs::write(&outside, "secret").unwrap();
    std::os::unix::fs::symlink(&outside, real.join("leak.txt")).unwrap();
    let err = run(
        &fx,
        None,
        install_spec(&path_spec(&real), SkillTargetScope::Project),
    )
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("not safe to copy"), "{err:#}");
    assert!(
        !fx.skills_dir(SkillTargetScope::Project)
            .join("linked")
            .exists()
    );
}

#[tokio::test]
async fn shipped_markers_are_stripped() {
    let fx = Fixture::new();
    let src = fx.source("marked", "body");
    fs::write(src.join(install::TRUSTED_MARKER), "{\"schema_version\":2}").unwrap();
    fs::write(
        src.join(install::INSTALLED_FROM_MARKER),
        "{\"spec\":\"github:evil/skill\"}",
    )
    .unwrap();
    run(
        &fx,
        None,
        install_spec(&path_spec(&src), SkillTargetScope::Project),
    )
    .await
    .unwrap();
    let dest = fx.skills_dir(SkillTargetScope::Project).join("marked");
    assert!(!dest.join(install::TRUSTED_MARKER).exists());
    let marker = read_installed_marker(&dest).unwrap().unwrap();
    assert!(marker.spec.starts_with("path:"), "{}", marker.spec);
}

#[cfg(unix)]
#[tokio::test]
async fn executable_bit_is_kept() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let src = fx.source("exec", "body");
    fs::create_dir_all(src.join("scripts")).unwrap();
    let script = src.join("scripts").join("run.sh");
    fs::write(&script, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let receipt = run(
        &fx,
        None,
        install_spec(&path_spec(&src), SkillTargetScope::Project),
    )
    .await
    .unwrap();
    assert!(
        receipt.summary.ends_with("includes 1 script"),
        "{}",
        receipt.summary
    );
    let copied = fx
        .skills_dir(SkillTargetScope::Project)
        .join("exec")
        .join("scripts")
        .join("run.sh");
    assert_eq!(
        fs::metadata(copied).unwrap().permissions().mode() & 0o111,
        0o111
    );
}

#[tokio::test]
async fn only_a_person_global_install_records_approval() {
    let fx = Fixture::new();
    let src = fx.source("approval", "body");
    let spec = path_spec(&src);

    for (initiator, target, expect_marker) in [
        (None, SkillTargetScope::Global, false),
        (Some(SkillInitiator::Agent), SkillTargetScope::Global, false),
        (
            Some(SkillInitiator::CommandLine),
            SkillTargetScope::Global,
            false,
        ),
        (
            Some(SkillInitiator::Person),
            SkillTargetScope::Project,
            false,
        ),
        (Some(SkillInitiator::Person), SkillTargetScope::Global, true),
    ] {
        let dest = fx.skills_dir(target).join("approval");
        if dest.exists() {
            fs::remove_dir_all(&dest).unwrap();
        }
        run(&fx, initiator, install_spec(&spec, target))
            .await
            .unwrap();
        let marker = dest.join(install::TRUSTED_MARKER);
        assert_eq!(marker.exists(), expect_marker, "{initiator:?} {target:?}");
        if expect_marker {
            let digest = package_digest::compute_package_digest(&dest).unwrap();
            assert!(fs::read_to_string(marker).unwrap().contains(&digest));
        }
    }
}

#[tokio::test]
async fn person_update_rebinds_trust_to_the_new_digest() {
    let fx = Fixture::new();
    let src = fx.source("rebind", "one");
    let spec = path_spec(&src);
    run(
        &fx,
        Some(SkillInitiator::Person),
        install_spec(&spec, SkillTargetScope::Global),
    )
    .await
    .unwrap();
    fs::write(
        src.join("SKILL.md"),
        "---\nname: rebind\ndescription: d\n---\ntwo\n",
    )
    .unwrap();
    run(
        &fx,
        Some(SkillInitiator::Person),
        install_spec(&spec, SkillTargetScope::Global),
    )
    .await
    .unwrap();
    let dest = fx.skills_dir(SkillTargetScope::Global).join("rebind");
    let digest = package_digest::compute_package_digest(&dest).unwrap();
    assert!(
        fs::read_to_string(dest.join(install::TRUSTED_MARKER))
            .unwrap()
            .contains(&digest)
    );
}

fn tarball(skill_name: &str, body: &str) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    let mut dir = tar::Header::new_gnu();
    dir.set_entry_type(tar::EntryType::Directory);
    dir.set_mode(0o755);
    dir.set_size(0);
    builder
        .append_data(&mut dir, "repo-main/", std::io::empty())
        .unwrap();
    let content = format!("---\nname: {skill_name}\ndescription: d\n---\n{body}\n");
    let mut file = tar::Header::new_gnu();
    file.set_mode(0o644);
    file.set_size(content.len() as u64);
    builder
        .append_data(&mut file, "repo-main/SKILL.md", content.as_bytes())
        .unwrap();
    builder.into_inner().unwrap().finish().unwrap()
}

async fn serve(body: Arc<Mutex<Vec<u8>>>) -> std::net::SocketAddr {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while let Ok(n) = socket.read(&mut buf).await {
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let payload = body.lock().unwrap().clone();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&payload).await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn remote_install_again_updates_or_reports_no_change() {
    let fx = Fixture::new();
    let body = Arc::new(Mutex::new(tarball("remote-skill", "one")));
    let addr = serve(body.clone()).await;
    let url = format!("http://{addr}/s.tar.gz");

    let first = run(
        &fx,
        Some(SkillInitiator::Person),
        install_spec(&url, SkillTargetScope::Global),
    )
    .await
    .unwrap();
    assert_eq!(first.outcome, SkillMutationOutcome::Installed);
    let dest = fx.skills_dir(SkillTargetScope::Global).join("remote-skill");
    assert!(dest.join(install::TRUSTED_MARKER).exists());

    let same = run(
        &fx,
        Some(SkillInitiator::Person),
        install_spec(&url, SkillTargetScope::Global),
    )
    .await
    .unwrap();
    assert_eq!(same.outcome, SkillMutationOutcome::NoChange);

    *body.lock().unwrap() = tarball("remote-skill", "two");
    let moved = run(
        &fx,
        Some(SkillInitiator::Person),
        install_spec(&url, SkillTargetScope::Global),
    )
    .await
    .unwrap();
    assert_eq!(moved.outcome, SkillMutationOutcome::Updated);
    assert!(
        fs::read_to_string(dest.join("SKILL.md"))
            .unwrap()
            .contains("two")
    );
    let digest = package_digest::compute_package_digest(&dest).unwrap();
    assert!(
        fs::read_to_string(dest.join(install::TRUSTED_MARKER))
            .unwrap()
            .contains(&digest)
    );
}

#[tokio::test]
async fn remote_install_over_other_source_says_it_replaces() {
    let fx = Fixture::new();
    let first = Arc::new(Mutex::new(tarball("shared-name", "one")));
    let second = Arc::new(Mutex::new(tarball("shared-name", "two")));
    let a = serve(first).await;
    let b = serve(second).await;
    let url_a = format!("http://{a}/a.tar.gz");
    let url_b = format!("http://{b}/b.tar.gz");

    run(&fx, None, install_spec(&url_a, SkillTargetScope::Project))
        .await
        .unwrap();
    let receipt = run(&fx, None, install_spec(&url_b, SkillTargetScope::Project))
        .await
        .unwrap();
    assert_eq!(receipt.outcome, SkillMutationOutcome::Updated);
    assert!(
        receipt
            .summary
            .contains(&format!("replaces the copy from {url_a}")),
        "{}",
        receipt.summary
    );
}

#[tokio::test]
async fn remote_install_over_unmarked_directory_is_refused() {
    let fx = Fixture::new();
    let body = Arc::new(Mutex::new(tarball("hand-made", "remote")));
    let addr = serve(body).await;
    let existing = fx.skills_dir(SkillTargetScope::Project).join("hand-made");
    fs::create_dir_all(&existing).unwrap();
    fs::write(
        existing.join("SKILL.md"),
        "---\nname: hand-made\ndescription: d\n---\nmine\n",
    )
    .unwrap();
    let err = run(
        &fx,
        None,
        install_spec(
            &format!("http://{addr}/x.tar.gz"),
            SkillTargetScope::Project,
        ),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("not installed here")
            || format!("{err:#}").contains("hand-made"),
        "{err:#}"
    );
    assert!(
        fs::read_to_string(existing.join("SKILL.md"))
            .unwrap()
            .contains("mine")
    );
}

#[tokio::test]
async fn remote_no_change_does_not_trust_locally_modified_content() {
    let fx = Fixture::new();
    let body = Arc::new(Mutex::new(tarball("tamper", "one")));
    let addr = serve(body).await;
    let url = format!("http://{addr}/t.tar.gz");
    run(&fx, None, install_spec(&url, SkillTargetScope::Global))
        .await
        .unwrap();
    let dest = fx.skills_dir(SkillTargetScope::Global).join("tamper");
    fs::write(dest.join("extra.sh"), "echo hi").unwrap();
    let receipt = run(
        &fx,
        Some(SkillInitiator::Person),
        install_spec(&url, SkillTargetScope::Global),
    )
    .await
    .unwrap();
    assert_eq!(receipt.outcome, SkillMutationOutcome::NoChange);
    assert!(dest.join("extra.sh").exists());
    assert!(!dest.join(install::TRUSTED_MARKER).exists());
}

type ArchiveEntry<'a> = (&'a str, tar::EntryType, u32, &'a [u8], Option<&'a str>);

fn build_archive_bytes(entries: &[ArchiveEntry<'_>]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (path, kind, mode, data, link_target) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(*kind);
        header.set_mode(*mode);
        if let Some(target) = link_target {
            header.set_size(0);
            header.set_path(path).unwrap();
            header.set_link_name(target).unwrap();
            header.set_cksum();
            builder.append(&header, std::io::empty()).unwrap();
        } else {
            let body: &[u8] = if *kind == tar::EntryType::Directory {
                &[]
            } else {
                data
            };
            header.set_size(body.len() as u64);
            let raw = path.as_bytes();
            assert!(raw.len() <= 100);
            header.as_gnu_mut().unwrap().name[..raw.len()].copy_from_slice(raw);
            header.set_cksum();
            builder.append(&header, body).unwrap();
        }
    }
    builder.into_inner().unwrap().finish().unwrap()
}

#[tokio::test]
async fn local_tar_gz_and_tgz_install_update_and_preserve_exec() {
    let fx = Fixture::new();
    let skill_one = b"---\nname: pack-skill\ndescription: d\n---\nv1\n".to_vec();
    let script = b"#!/bin/sh\necho ok\n";
    let flat_bytes = build_archive_bytes(&[
        ("SKILL.md", tar::EntryType::Regular, 0o644, &skill_one, None),
        ("scripts/", tar::EntryType::Directory, 0o755, &[], None),
        (
            "scripts/run.sh",
            tar::EntryType::Regular,
            0o755,
            script,
            None,
        ),
        (
            ".installed-from",
            tar::EntryType::Regular,
            0o644,
            br#"{"spec":"spoofed"}"#,
            None,
        ),
        (
            ".trusted",
            tar::EntryType::Regular,
            0o644,
            br#"{"digest":"spoofed"}"#,
            None,
        ),
    ]);
    let archive_path = fx.workspace.join("pack-skill.tar.gz");
    fs::write(&archive_path, &flat_bytes).unwrap();

    let first = run(
        &fx,
        None,
        install_spec("./pack-skill.tar.gz", SkillTargetScope::Project),
    )
    .await
    .unwrap();
    assert_eq!(first.outcome, SkillMutationOutcome::Installed);
    assert!(
        first
            .summary
            .starts_with("Installed skill pack-skill (project) from path:")
            && first.summary.ends_with(", on; includes 1 script"),
        "{}",
        first.summary
    );
    let dest = fx.skills_dir(SkillTargetScope::Project).join("pack-skill");
    assert!(!dest.join(install::TRUSTED_MARKER).exists());
    let marker = read_installed_marker(&dest).unwrap().unwrap();
    assert!(marker.spec.starts_with("path:") && marker.spec.ends_with("pack-skill.tar.gz"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(dest.join("scripts/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0);
    }

    let same = run(
        &fx,
        None,
        install_spec(&path_spec(&archive_path), SkillTargetScope::Project),
    )
    .await
    .unwrap();
    assert_eq!(same.outcome, SkillMutationOutcome::NoChange);

    let skill_two = b"---\nname: pack-skill\ndescription: d\n---\nv2\n".to_vec();
    let wrapped_v2 = build_archive_bytes(&[
        ("repo-main/", tar::EntryType::Directory, 0o755, &[], None),
        (
            "repo-main/SKILL.md",
            tar::EntryType::Regular,
            0o644,
            &skill_two,
            None,
        ),
        (
            "repo-main/scripts/",
            tar::EntryType::Directory,
            0o755,
            &[],
            None,
        ),
        (
            "repo-main/scripts/run.sh",
            tar::EntryType::Regular,
            0o755,
            script,
            None,
        ),
        (
            "repo-main/.TRUSTED",
            tar::EntryType::Regular,
            0o644,
            br#"{"digest":"spoofed"}"#,
            None,
        ),
    ]);
    fs::write(&archive_path, &wrapped_v2).unwrap();

    let updated = run(
        &fx,
        Some(SkillInitiator::Person),
        SkillOneStepRequest::Mutation(SkillMutationRequest::UpdateByName {
            name: "pack-skill".into(),
            scope: Some(SkillTargetScope::Project),
            expected_digest: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(updated.outcome, SkillMutationOutcome::Updated);
    assert!(
        fs::read_to_string(dest.join("SKILL.md"))
            .unwrap()
            .contains("v2")
    );
    assert!(!dest.join(".TRUSTED").exists());
    assert!(!dest.join(install::TRUSTED_MARKER).exists());

    let tgz_path = fx.home.join("wrapped.tgz");
    let tgz_skill = b"---\nname: tgz-global\ndescription: d\n---\nglobal\n".to_vec();
    let tgz_bytes = build_archive_bytes(&[
        ("wrapper/", tar::EntryType::Directory, 0o755, &[], None),
        (
            "wrapper/SKILL.md",
            tar::EntryType::Regular,
            0o644,
            &tgz_skill,
            None,
        ),
        (
            "wrapper/.installed-from",
            tar::EntryType::Regular,
            0o644,
            br#"{"spec":"spoofed"}"#,
            None,
        ),
    ]);
    fs::write(&tgz_path, &tgz_bytes).unwrap();

    let global = run(
        &fx,
        Some(SkillInitiator::Person),
        install_spec("~/wrapped.tgz", SkillTargetScope::Global),
    )
    .await
    .unwrap();
    assert_eq!(global.outcome, SkillMutationOutcome::Installed);
    let global_dest = fx.skills_dir(SkillTargetScope::Global).join("tgz-global");
    let digest = package_digest::compute_package_digest(&global_dest).unwrap();
    assert!(
        fs::read_to_string(global_dest.join(install::TRUSTED_MARKER))
            .unwrap()
            .contains(&digest)
    );
}

#[tokio::test]
async fn local_archive_security_and_invalid_inputs_are_refused() {
    let fx = Fixture::new();
    let skill_md = b"---\nname: bad\ndescription: d\n---\nbad\n";

    let symlink_bytes = build_archive_bytes(&[
        ("SKILL.md", tar::EntryType::Regular, 0o644, skill_md, None),
        (
            "leak",
            tar::EntryType::Symlink,
            0o777,
            &[],
            Some("/etc/passwd"),
        ),
    ]);
    let symlink_tar = fx.workspace.join("sym.tar.gz");
    fs::write(&symlink_tar, symlink_bytes).unwrap();
    assert!(
        run(
            &fx,
            None,
            install_spec(&path_spec(&symlink_tar), SkillTargetScope::Project),
        )
        .await
        .is_err()
    );

    let hardlink_bytes = build_archive_bytes(&[
        ("SKILL.md", tar::EntryType::Regular, 0o644, skill_md, None),
        ("hard", tar::EntryType::Link, 0o644, &[], Some("SKILL.md")),
    ]);
    let hardlink_tar = fx.workspace.join("hard.tar.gz");
    fs::write(&hardlink_tar, hardlink_bytes).unwrap();
    assert!(
        run(
            &fx,
            None,
            install_spec(&path_spec(&hardlink_tar), SkillTargetScope::Project),
        )
        .await
        .is_err()
    );

    for (file_name, entry_name) in [
        ("dotdot.tar.gz", "../escape/SKILL.md"),
        ("abs.tar.gz", "/tmp/evil/SKILL.md"),
        ("backslash.tar.gz", "dir\\SKILL.md"),
    ] {
        let bytes =
            build_archive_bytes(&[(entry_name, tar::EntryType::Regular, 0o644, skill_md, None)]);
        let path = fx.workspace.join(file_name);
        fs::write(&path, bytes).unwrap();
        assert!(
            run(
                &fx,
                None,
                install_spec(&path_spec(&path), SkillTargetScope::Project),
            )
            .await
            .is_err(),
            "{file_name}"
        );
    }

    let missing_md =
        build_archive_bytes(&[("README.md", tar::EntryType::Regular, 0o644, b"hi", None)]);
    let missing_tar = fx.workspace.join("missing.tar.gz");
    fs::write(&missing_tar, missing_md).unwrap();
    assert!(
        run(
            &fx,
            None,
            install_spec(&path_spec(&missing_tar), SkillTargetScope::Project),
        )
        .await
        .is_err()
    );

    let many = codewhale_skill_install::bounded_tar::test_archives::empty_entries_archive(
        ("SKILL.md", skill_md),
        600,
    );
    let many_tar = fx.workspace.join("many.tar.gz");
    fs::write(&many_tar, many).unwrap();
    assert!(
        run(
            &fx,
            None,
            install_spec(&path_spec(&many_tar), SkillTargetScope::Project),
        )
        .await
        .is_err()
    );

    let big_payload = vec![b'x'; 8192];
    let big_bytes = build_archive_bytes(&[
        ("SKILL.md", tar::EntryType::Regular, 0o644, skill_md, None),
        (
            "big.txt",
            tar::EntryType::Regular,
            0o644,
            &big_payload,
            None,
        ),
    ]);
    let big_tar = fx.workspace.join("big.tar.gz");
    fs::write(&big_tar, big_bytes).unwrap();
    let mut small_ctx = fx.ctx();
    small_ctx.max_size = 1024;
    assert!(
        execute_one_step(
            install_spec(&path_spec(&big_tar), SkillTargetScope::Project),
            None,
            &small_ctx,
        )
        .await
        .is_err()
    );

    let plain = fx.workspace.join("notes.txt");
    fs::write(&plain, "not an archive").unwrap();
    assert!(
        run(
            &fx,
            None,
            install_spec(&path_spec(&plain), SkillTargetScope::Project),
        )
        .await
        .is_err()
    );

    #[cfg(unix)]
    {
        let real_tar = fx.workspace.join("real.tar.gz");
        fs::write(&real_tar, tarball("linked-arc", "body")).unwrap();
        let sym_tar = fx.workspace.join("sym-link.tar.gz");
        std::os::unix::fs::symlink(&real_tar, &sym_tar).unwrap();
        assert!(
            run(
                &fx,
                None,
                install_spec(&path_spec(&sym_tar), SkillTargetScope::Project),
            )
            .await
            .is_err()
        );
    }

    assert!(
        !fx.skills_dir(SkillTargetScope::Project)
            .join("bad")
            .exists()
    );
}

#[tokio::test]
async fn skills_state_off_survives_update_and_shows_off_in_receipt() {
    let fx = Fixture::new();
    let state_path = fx.home.join(".codewhale").join("skills_state.toml");
    let mut store =
        codewhale_runtime::skill_state::SkillStateStore::load_from(state_path.clone()).unwrap();
    store.set_enabled("disabled-skill", false).unwrap();

    let body = Arc::new(Mutex::new(tarball("disabled-skill", "one")));
    let addr = serve(body.clone()).await;
    let url = format!("http://{addr}/d.tar.gz");

    let installed = run(&fx, None, install_spec(&url, SkillTargetScope::Project))
        .await
        .unwrap();
    assert!(
        installed.summary.ends_with(", off"),
        "{}",
        installed.summary
    );

    *body.lock().unwrap() = tarball("disabled-skill", "two");
    let updated = run(&fx, None, install_spec(&url, SkillTargetScope::Project))
        .await
        .unwrap();
    assert_eq!(updated.outcome, SkillMutationOutcome::Updated);
    assert!(updated.summary.ends_with(", off"), "{}", updated.summary);
    let after_store =
        codewhale_runtime::skill_state::SkillStateStore::load_from(state_path).unwrap();
    assert!(!after_store.is_enabled("disabled-skill"));
}

#[tokio::test]
async fn legacy_execute_install_remote_updates_in_place_without_auto_trust() {
    let fx = Fixture::new();
    let body = Arc::new(Mutex::new(tarball("legacy-skill", "one")));
    let addr = serve(body.clone()).await;
    let source = InstallSource::parse(&format!("http://{addr}/l.tar.gz")).unwrap();

    let first = execute(
        SkillMutationRequest::InstallRemote {
            source: source.clone(),
            target: SkillTargetScope::Global,
        },
        &fx.ctx(),
    )
    .await
    .unwrap();
    assert_eq!(first.outcome, SkillMutationOutcome::Installed);
    let dest = fx.skills_dir(SkillTargetScope::Global).join("legacy-skill");
    assert!(!dest.join(install::TRUSTED_MARKER).exists());

    let same = execute(
        SkillMutationRequest::InstallRemote {
            source: source.clone(),
            target: SkillTargetScope::Global,
        },
        &fx.ctx(),
    )
    .await
    .unwrap();
    assert_eq!(same.outcome, SkillMutationOutcome::NoChange);

    *body.lock().unwrap() = tarball("legacy-skill", "two");
    let updated = execute(
        SkillMutationRequest::InstallRemote {
            source,
            target: SkillTargetScope::Global,
        },
        &fx.ctx(),
    )
    .await
    .unwrap();
    assert_eq!(updated.outcome, SkillMutationOutcome::Updated);
    assert!(!dest.join(install::TRUSTED_MARKER).exists());
}
