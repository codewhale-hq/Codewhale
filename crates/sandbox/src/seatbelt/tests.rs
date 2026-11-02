use super::*;

/// #4085: an inherited read-write extension is necessary but never sufficient
/// for WorkspaceWrite. Every extension-backed write must also match one
/// approved root and retain that root's protected-subpath exclusions.
#[test]
fn file_provider_extensions_intersect_each_workspace_root_and_exclusions() {
    assert!(
        is_available(),
        "UNRUN: macOS sandbox-exec is unavailable; generated policy was not parsed"
    );

    let fixture = tempfile::tempdir().expect("create policy fixture");
    let workspace = fixture.path().join("workspace");
    let additional_root = fixture.path().join("additional-root");
    std::fs::create_dir_all(workspace.join(".codewhale")).expect("create protected workspace path");
    std::fs::create_dir_all(additional_root.join(".deepseek"))
        .expect("create protected additional-root path");

    let policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![additional_root],
        network_access: false,
        exclude_tmpdir: true,
        exclude_slash_tmp: true,
    };
    let workspace_write = generate_policy(&policy, &workspace, &[]);
    let read_only = generate_policy(&SandboxPolicy::read_only(), &workspace, &[]);

    for policy in [&workspace_write, &read_only] {
        assert!(policy.contains("(version 1)"));
        assert!(policy.contains("(deny default)"));
        assert!(policy.contains("(allow file-read*)"));
        assert!(policy.contains(r#"(allow file-read* (extension "com.apple.app-sandbox.read"))"#));
        assert!(
            policy.contains(r#"(allow file-read* (extension "com.apple.app-sandbox.read-write"))"#)
        );
    }
    assert!(!workspace_write.contains("network-outbound"));

    for root_index in 0..=1 {
        let expected = format!(
            r#"(require-all (extension "com.apple.app-sandbox.read-write") (subpath (param "WRITABLE_ROOT_{root_index}")) (require-not (subpath (param "WRITABLE_ROOT_{root_index}_RO_0")))"#
        );
        assert!(
            workspace_write.contains(&expected),
            "extension write must retain root {root_index} and its exclusion:\n{workspace_write}"
        );
    }
    let read_write_extension = r#"(extension "com.apple.app-sandbox.read-write")"#;
    assert_eq!(
        workspace_write.matches(read_write_extension).count(),
        3,
        "one read rule plus exactly one root-scoped write rule per approved root"
    );
    assert!(workspace_write.contains("file-write*"));
    assert!(!workspace_write.lines().any(|line| {
        line.trim() == r#"(allow file-write* (extension "com.apple.app-sandbox.read-write"))"#
    }));
    assert!(!read_only.contains(r#"file-write* (extension "com.apple.app-sandbox.read-write")"#));
    assert_eq!(read_only.matches(read_write_extension).count(), 1);
    assert!(!read_only.contains("WRITABLE_ROOT"));

    let args = create_seatbelt_args(vec!["/usr/bin/true".to_string()], &policy, &workspace, &[]);
    let output = Command::new(SANDBOX_EXEC_PATH)
        .args(args)
        .current_dir(&workspace)
        .output()
        .expect("parse generated policy with sandbox-exec");
    assert!(
        output.status.success(),
        "sandbox-exec rejected generated intersection policy: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Hermetic command/policy-shape coverage for the six operations reported in
/// #4085. Each operation gets a fresh fixture so an early failure cannot hide
/// later results. This is not physical File Provider acceptance.
#[test]
fn file_provider_synthetic_operations_are_independent_under_seatbelt() {
    assert!(
        is_available(),
        "UNRUN: macOS sandbox-exec is unavailable; no synthetic operation evidence collected"
    );

    #[derive(Clone, Copy, Debug)]
    enum Operation {
        Mkdir,
        Write,
        Read,
        Grep,
        DeleteFile,
        DeleteDirectory,
    }

    let mut failures = Vec::new();
    for operation in [
        Operation::Mkdir,
        Operation::Write,
        Operation::Read,
        Operation::Grep,
        Operation::DeleteFile,
        Operation::DeleteDirectory,
    ] {
        let fixture = tempfile::tempdir().expect("create independent operation fixture");
        let workspace = fixture
            .path()
            .join("Library/CloudStorage/TestProvider/Workspace");
        std::fs::create_dir_all(&workspace).expect("create synthetic CloudStorage workspace");
        let target = workspace.join("target");
        let source = workspace.join("source");

        let command = match operation {
            Operation::Mkdir => vec![
                "/bin/mkdir".to_string(),
                target.to_string_lossy().into_owned(),
            ],
            Operation::Write => {
                std::fs::write(&source, b"file-provider\n").expect("seed copy source");
                vec![
                    "/bin/cp".to_string(),
                    source.to_string_lossy().into_owned(),
                    target.to_string_lossy().into_owned(),
                ]
            }
            Operation::Read => {
                std::fs::write(&target, b"file-provider\n").expect("seed read target");
                vec![
                    "/bin/cat".to_string(),
                    target.to_string_lossy().into_owned(),
                ]
            }
            Operation::Grep => {
                std::fs::write(&target, b"file-provider\n").expect("seed grep target");
                vec![
                    "/usr/bin/grep".to_string(),
                    "-q".to_string(),
                    "file-provider".to_string(),
                    target.to_string_lossy().into_owned(),
                ]
            }
            Operation::DeleteFile => {
                std::fs::write(&target, b"file-provider\n").expect("seed deletion target");
                vec!["/bin/rm".to_string(), target.to_string_lossy().into_owned()]
            }
            Operation::DeleteDirectory => {
                std::fs::create_dir(&target).expect("seed directory deletion target");
                vec![
                    "/bin/rmdir".to_string(),
                    target.to_string_lossy().into_owned(),
                ]
            }
        };

        let args = create_seatbelt_args(command, &SandboxPolicy::default(), &workspace, &[]);
        let output = Command::new(SANDBOX_EXEC_PATH)
            .args(args)
            .current_dir(&workspace)
            .output()
            .expect("execute sandboxed operation");

        let effect_matches = match operation {
            Operation::Mkdir => target.is_dir(),
            Operation::Write => {
                matches!(std::fs::read(&target), Ok(bytes) if bytes == b"file-provider\n")
            }
            Operation::Read => output.stdout == b"file-provider\n",
            Operation::Grep => true,
            Operation::DeleteFile | Operation::DeleteDirectory => !target.exists(),
        };
        if !output.status.success() || !effect_matches {
            failures.push(format!(
                "{operation:?}: status={:?}, stderr={}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "independent sandbox operations failed:\n{}",
        failures.join("\n")
    );
}

/// S1 (#5568): the opt-in read deny-list must actually stop reads that the
/// full-disk-read posture would otherwise allow — live, with sandbox-exec —
/// and an empty list must leave the generated profile byte-identical.
#[test]
fn denied_read_subpaths_block_reads_under_every_sandboxed_posture() {
    assert!(
        is_available(),
        "UNRUN: macOS sandbox-exec is unavailable; no deny-list evidence collected"
    );

    let secret_dir = tempfile::tempdir().expect("secret dir");
    let secret_file = secret_dir.path().join("id_ed25519");
    std::fs::write(&secret_file, "PRIVATE KEY MATERIAL").expect("write secret");
    let workspace = tempfile::tempdir().expect("workspace");
    let read_cmd = vec![
        "/bin/cat".to_string(),
        secret_file.to_string_lossy().into_owned(),
    ];

    for policy in [SandboxPolicy::read_only(), SandboxPolicy::default()] {
        // Without the deny-list the read succeeds (full-disk read posture).
        let open_args = create_seatbelt_args(read_cmd.clone(), &policy, workspace.path(), &[]);
        let open = Command::new(SANDBOX_EXEC_PATH)
            .args(open_args)
            .current_dir(workspace.path())
            .output()
            .expect("run un-denied read");
        assert!(
            open.status.success(),
            "baseline read should pass under {policy:?}: {}",
            String::from_utf8_lossy(&open.stderr)
        );

        // With the parent directory denied, the same read must fail. The
        // rule set comes from the manager's setter, which canonicalizes:
        // macOS tempdirs live behind the /var -> /private/var symlink, and
        // Seatbelt matches the kernel-resolved path, so a literal-only rule
        // silently never fires (caught live by this very test).
        let mut manager = crate::SandboxManager::new();
        manager.set_denied_read_subpaths(vec![secret_dir.path().to_path_buf()]);
        let denied_args = create_seatbelt_args(
            read_cmd.clone(),
            &policy,
            workspace.path(),
            manager.denied_read_subpaths_for_test(),
        );
        let denied = Command::new(SANDBOX_EXEC_PATH)
            .args(denied_args)
            .current_dir(workspace.path())
            .output()
            .expect("run denied read");
        assert!(
            !denied.status.success(),
            "deny-listed read must fail under {policy:?}, stdout: {}",
            String::from_utf8_lossy(&denied.stdout)
        );
    }
}
