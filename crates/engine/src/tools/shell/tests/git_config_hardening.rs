use super::*;

#[cfg(unix)]
struct RepoFixture {
    _root: tempfile::TempDir,
    repo: std::path::PathBuf,
    secret_path: std::path::PathBuf,
    canary_path: std::path::PathBuf,
    secret_value: &'static str,
}

#[cfg(unix)]
impl RepoFixture {
    fn new(config_snippet: &str, attributes: Option<&str>, add_signed_commit: bool) -> Self {
        let root = crate::test_support::sandbox_visible_tempdir();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let secret_path = root.path().join("secret.txt");
        let canary_path = root.path().join("ran.canary");
        let secret_value = "GIT-HARDENING-CANARY-9f2a";
        std::fs::write(&secret_path, format!("{secret_value}\n")).unwrap();

        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .status()
                .expect("run git");
            assert!(status.success(), "git {args:?} failed: {status}");
        };

        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.invalid"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        if let Some(attrs) = attributes {
            std::fs::write(repo.join(".gitattributes"), attrs).unwrap();
        }
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);

        if add_signed_commit {
            let tree = String::from_utf8(
                std::process::Command::new("git")
                    .args(["rev-parse", "HEAD^{tree}"])
                    .current_dir(&repo)
                    .output()
                    .expect("rev-parse")
                    .stdout,
            )
            .unwrap();
            let commit_obj = format!(
                "tree {}\nauthor Test <test@example.invalid> 1700000000 +0000\ncommitter Test <test@example.invalid> 1700000000 +0000\ngpgsig -----BEGIN PGP SIGNATURE-----\n \n iQEzBAABCAAdFiEE\n -----END PGP SIGNATURE-----\n\nsigned commit\n",
                tree.trim()
            );
            let mut hash_child = std::process::Command::new("git")
                .args(["hash-object", "-t", "commit", "-w", "--stdin"])
                .current_dir(&repo)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("hash-object");
            std::io::Write::write_all(hash_child.stdin.as_mut().unwrap(), commit_obj.as_bytes())
                .unwrap();
            let output = hash_child.wait_with_output().unwrap();
            assert!(output.status.success());
            let commit_id = String::from_utf8(output.stdout).unwrap();
            git(&["update-ref", "HEAD", commit_id.trim()]);
        }

        std::fs::write(repo.join("a.txt"), "hello\nworld\n").unwrap();

        let rendered = config_snippet
            .replace("{{SECRET}}", &secret_path.display().to_string())
            .replace("{{CANARY}}", &canary_path.display().to_string());
        let mut config = std::fs::read_to_string(repo.join(".git/config")).unwrap();
        config.push('\n');
        config.push_str(&rendered);
        config.push('\n');
        std::fs::write(repo.join(".git/config"), config).unwrap();

        Self {
            _root: root,
            repo,
            secret_path,
            canary_path,
            secret_value,
        }
    }

    fn write_helper_script(&self) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let script = self._root.path().join("helper.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho RAN > {}\ncat {}\nexit 0\n",
                shell_words::quote(&self.canary_path.display().to_string()),
                shell_words::quote(&self.secret_path.display().to_string()),
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        script
    }

    fn assert_inert(&self, output: &str, label: &str) {
        assert!(
            !output.contains(self.secret_value),
            "{label}: repository helper leaked secret into output: {output}"
        );
        assert!(
            !self.canary_path.exists(),
            "{label}: repository helper executed and created canary file"
        );
    }
}

#[cfg(unix)]
fn full_interactive_context(repo: &std::path::Path) -> ToolContext {
    let mut context = ToolContext::new(repo);
    context.auto_approve = false;
    context.shell_policy = ShellPolicy::Full;
    context
}

#[cfg(unix)]
fn native_readonly_context(repo: &std::path::Path) -> Option<ToolContext> {
    let mut context = ToolContext::new(repo);
    context.auto_approve = false;
    context.shell_policy = ShellPolicy::Full;
    #[cfg(target_os = "linux")]
    context.shell_manager.lock().unwrap().set_prefer_bwrap(true);
    if !context
        .shell_manager
        .lock()
        .unwrap()
        .configured_sandbox_type()
        .is_some_and(is_native_readonly_sandbox)
    {
        return None;
    }
    Some(context)
}

#[cfg(unix)]
#[tokio::test]
async fn auto_approved_readonly_git_commands_block_all_repo_config_exec_vectors() {
    let diff_driver = RepoFixture::new(
        "[diff \"x\"]\n\tcommand = sh -c 'echo RAN > {{CANARY}}; cat {{SECRET}}'",
        Some("* diff=x\n"),
        false,
    );
    let ctx = full_interactive_context(&diff_driver.repo);
    for cmd in ["git diff", "git log -n 1", "git show HEAD"] {
        let input = json!({"command": cmd});
        assert_eq!(
            LowercaseBashTool.approval_requirement_for(&input),
            ApprovalRequirement::Auto,
            "{cmd}"
        );
        let result = LowercaseBashTool
            .execute(input, &ctx)
            .await
            .expect("auto-approved git read succeeds");
        diff_driver.assert_inert(&result.content, cmd);
        if cmd == "git diff" {
            assert!(result.content.contains("+world"), "{}", result.content);
        }
    }

    let textconv = RepoFixture::new(
        "[diff \"x\"]\n\ttextconv = sh -c 'echo RAN > {{CANARY}}; cat {{SECRET}}'",
        Some("* diff=x\n"),
        false,
    );
    let ctx = full_interactive_context(&textconv.repo);
    for cmd in ["git diff", "git blame a.txt"] {
        let input = json!({"command": cmd});
        assert_eq!(
            LowercaseBashTool.approval_requirement_for(&input),
            ApprovalRequirement::Auto,
            "{cmd}"
        );
        let result = LowercaseBashTool
            .execute(input, &ctx)
            .await
            .expect("textconv-hardened git read succeeds");
        textconv.assert_inert(&result.content, cmd);
    }

    let ext_diff = RepoFixture::new(
        "[diff]\n\texternal = sh -c 'echo RAN > {{CANARY}}; cat {{SECRET}}'",
        None,
        false,
    );
    let ctx = full_interactive_context(&ext_diff.repo);
    let result = LowercaseBashTool
        .execute(json!({"command": "git diff"}), &ctx)
        .await
        .expect("diff.external neutralized");
    ext_diff.assert_inert(&result.content, "diff.external");
    assert!(result.content.contains("+world"), "{}", result.content);

    let fsmonitor = RepoFixture::new(
        "[core]\n\tfsmonitor = sh -c 'echo RAN > {{CANARY}}; cat {{SECRET}}'",
        None,
        false,
    );
    let ctx = full_interactive_context(&fsmonitor.repo);
    let result = LowercaseBashTool
        .execute(json!({"command": "git status"}), &ctx)
        .await
        .expect("core.fsmonitor neutralized");
    fsmonitor.assert_inert(&result.content, "core.fsmonitor");

    let filter = RepoFixture::new(
        "[filter \"x\"]\n\tprocess = sh -c 'echo RAN > {{CANARY}}; cat {{SECRET}}'\n\tclean = sh -c 'echo RAN > {{CANARY}}; cat {{SECRET}}'",
        Some("* filter=x\n"),
        false,
    );
    let ctx = full_interactive_context(&filter.repo);
    for cmd in ["git diff", "git status"] {
        let result = LowercaseBashTool
            .execute(json!({"command": cmd}), &ctx)
            .await
            .expect("filter driver neutralized");
        filter.assert_inert(&result.content, cmd);
    }

    let gpg = RepoFixture::new("", None, true);
    let helper = gpg.write_helper_script();
    let mut cfg = std::fs::read_to_string(gpg.repo.join(".git/config")).unwrap();
    cfg.push_str(&format!(
        "\n[gpg]\n\tprogram = {}\n[log]\n\tshowSignature = true\n",
        helper.display()
    ));
    std::fs::write(gpg.repo.join(".git/config"), cfg).unwrap();
    let ctx = full_interactive_context(&gpg.repo);
    for cmd in ["git log -n 1", "git show HEAD"] {
        let input = json!({"command": cmd});
        assert_eq!(
            LowercaseBashTool.approval_requirement_for(&input),
            ApprovalRequirement::Auto,
            "{cmd}"
        );
        let result = LowercaseBashTool
            .execute(input, &ctx)
            .await
            .expect("gpg.program neutralized");
        gpg.assert_inert(&result.content, cmd);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn enforced_readonly_blocks_all_repo_config_exec_vectors() {
    let diff_driver = RepoFixture::new(
        "[diff \"x\"]\n\tcommand = sh -c 'cat {{SECRET}}'",
        Some("* diff=x\n"),
        false,
    );
    let Some(ctx) = native_readonly_context(&diff_driver.repo) else {
        return;
    };
    for cmd in [
        "git diff",
        "git status && git diff",
        "git diff | head -n 20",
        "git -C . diff",
    ] {
        let result = LowercaseBashTool
            .execute(json!({"command": cmd, "read_only": true}), &ctx)
            .await
            .expect("enforced readonly git diff succeeds");
        diff_driver.assert_inert(&result.content, cmd);
        assert!(
            result.content.contains("+world"),
            "{cmd}: {}",
            result.content
        );
    }
    let wrapped = LowercaseBashTool
        .execute(
            json!({"command": "sh -c 'git diff'", "read_only": true}),
            &ctx,
        )
        .await;
    let wrapped_text = match wrapped {
        Ok(res) => res.content,
        Err(err) => err.to_string(),
    };
    diff_driver.assert_inert(&wrapped_text, "sh -c 'git diff'");

    let textconv = RepoFixture::new(
        "[diff \"x\"]\n\ttextconv = sh -c 'cat {{SECRET}}'",
        Some("* diff=x\n"),
        false,
    );
    let ctx = native_readonly_context(&textconv.repo).unwrap();
    let result = LowercaseBashTool
        .execute(json!({"command": "git diff", "read_only": true}), &ctx)
        .await
        .expect("enforced readonly textconv neutralized");
    textconv.assert_inert(&result.content, "readonly textconv");

    let ext_diff = RepoFixture::new("[diff]\n\texternal = sh -c 'cat {{SECRET}}'", None, false);
    let ctx = native_readonly_context(&ext_diff.repo).unwrap();
    let result = LowercaseBashTool
        .execute(json!({"command": "git diff", "read_only": true}), &ctx)
        .await
        .expect("enforced readonly diff.external neutralized");
    ext_diff.assert_inert(&result.content, "readonly diff.external");
    assert!(result.content.contains("+world"), "{}", result.content);

    let fsmonitor = RepoFixture::new("[core]\n\tfsmonitor = sh -c 'cat {{SECRET}}'", None, false);
    let ctx = native_readonly_context(&fsmonitor.repo).unwrap();
    let result = LowercaseBashTool
        .execute(json!({"command": "git status", "read_only": true}), &ctx)
        .await
        .expect("enforced readonly core.fsmonitor neutralized");
    fsmonitor.assert_inert(&result.content, "readonly core.fsmonitor");

    let filter = RepoFixture::new(
        "[filter \"x\"]\n\tprocess = sh -c 'cat {{SECRET}}'",
        Some("* filter=x\n"),
        false,
    );
    let ctx = native_readonly_context(&filter.repo).unwrap();
    let result = LowercaseBashTool
        .execute(json!({"command": "git diff", "read_only": true}), &ctx)
        .await
        .expect("enforced readonly filter.process neutralized");
    filter.assert_inert(&result.content, "readonly filter.process");

    let gpg = RepoFixture::new("", None, true);
    let helper = gpg.write_helper_script();
    let mut cfg = std::fs::read_to_string(gpg.repo.join(".git/config")).unwrap();
    cfg.push_str(&format!(
        "\n[gpg]\n\tprogram = {}\n[log]\n\tshowSignature = true\n",
        helper.display()
    ));
    std::fs::write(gpg.repo.join(".git/config"), cfg).unwrap();
    let ctx = native_readonly_context(&gpg.repo).unwrap();
    let result = LowercaseBashTool
        .execute(json!({"command": "git log -n 1", "read_only": true}), &ctx)
        .await
        .expect("enforced readonly gpg.program neutralized");
    gpg.assert_inert(&result.content, "readonly gpg.program");
}
