use super::*;
use serde_json::json;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use tempfile::tempdir;

#[test]
fn test_tool_result_success() {
    let result = ToolResult::success("hello");
    assert!(result.success);
    assert_eq!(result.content, "hello");
    assert!(result.metadata.is_none());
}

#[test]
fn test_tool_result_error() {
    let result = ToolResult::error("something failed");
    assert!(!result.success);
    assert_eq!(result.content, "something failed");
}

#[test]
fn test_tool_result_json() {
    let data = json!({"key": "value"});
    let result = ToolResult::json(&data).unwrap();
    assert!(result.success);
    assert!(result.content.contains("key"));
}

#[test]
fn test_tool_result_with_metadata() {
    let result = ToolResult::success("content").with_metadata(json!({"extra": true}));
    assert!(result.metadata.is_some());
}

#[test]
fn test_tool_context_resolve_path_relative() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    // Create a test file
    let test_file = tmp.path().join("test.txt");
    std::fs::write(&test_file, "test").expect("write");

    let resolved = ctx.resolve_path("test.txt").expect("resolve");
    assert!(resolved.ends_with("test.txt"));
}

#[test]
fn test_tool_context_resolve_path_escape() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    // Try to escape workspace
    let result = ctx.resolve_path("/etc/passwd");
    assert!(result.is_err());
}

#[test]
fn test_tool_context_resolve_path_parent_traversal() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    let result = ctx.resolve_path("../escape.txt");
    assert!(result.is_err());
}

#[test]
fn test_tool_context_resolve_path_normalizes_parent() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    let result = ctx.resolve_path("new/../safe.txt");
    assert!(result.is_ok());
}

#[test]
fn test_tool_context_trust_mode() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf()).with_trust_mode(true);

    // In trust mode, absolute paths should work
    let result = ctx.resolve_path("/tmp");
    assert!(result.is_ok());
}

#[test]
fn tool_context_keeps_execution_state_grouped_and_value_cloned() {
    let mut context = ToolContext::new(".");
    context.auto_approve = true;
    context.state_namespace = "session-a".to_string();

    assert!(context.execution.auto_approve);
    assert_eq!(context.execution.state_namespace, "session-a");

    let mut cloned = context.clone();
    cloned.state_namespace = "session-b".to_string();
    assert_eq!(context.state_namespace, "session-a");
    assert_eq!(cloned.execution.state_namespace, "session-b");
}

#[test]
fn legacy_auto_approve_bit_folds_into_the_context_posture() {
    let plain = ToolContext::new(".");
    assert_eq!(
        plain.approval_mode,
        codewhale_execpolicy::ApprovalMode::Suggest,
        "a context with no authority runs under Ask"
    );

    let yolo = ToolContext::with_auto_approve(".", false, "notes.md", true);
    assert!(yolo.auto_approve);
    assert_eq!(
        yolo.approval_mode,
        codewhale_execpolicy::ApprovalMode::Bypass,
        "a set auto-approve bit is the Full Access posture"
    );
}

#[test]
fn tool_context_top_level_stays_slim_as_services_grow() {
    assert!(
        std::mem::size_of::<ToolContext>()
            <= std::mem::size_of::<PathBuf>() + 2 * std::mem::size_of::<usize>(),
        "ToolContext should contain only the workspace and boxed execution group"
    );
}

/// Issue #29: paths under a user-trusted external directory resolve
/// successfully even though they fall outside the workspace, while
/// untrusted external paths still error with `PathEscape`.
#[test]
fn test_tool_context_trusted_external_path_allows_escape() {
    let workspace = tempdir().expect("workspace tempdir");
    let trusted_root = tempdir().expect("trusted tempdir");
    let trusted_file = trusted_root.path().join("notes.md");
    std::fs::write(&trusted_file, "shared notes").unwrap();

    let ctx = ToolContext::new(workspace.path().to_path_buf()).with_trusted_external_paths(vec![
        trusted_root
            .path()
            .canonicalize()
            .unwrap_or_else(|_| trusted_root.path().to_path_buf()),
    ]);

    let resolved = ctx
        .resolve_path(trusted_file.to_str().unwrap())
        .expect("trusted path should resolve");
    assert!(resolved.ends_with("notes.md"));

    // Path outside workspace AND outside the trust list should still fail.
    let other = tempdir().expect("untrusted tempdir");
    let other_file = other.path().join("secret.md");
    std::fs::write(&other_file, "x").unwrap();
    let err = ctx
        .resolve_path(other_file.to_str().unwrap())
        .expect_err("untrusted path must error");
    assert!(matches!(err, ToolError::PathEscape { .. }));
}

#[test]
#[cfg(unix)]
fn test_tool_context_follow_symlinks_allows_nonexistent_path_under_workspace_symlink() {
    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    std::fs::create_dir_all(outside.join("target")).expect("mkdir outside target");
    symlink(outside.join("target"), workspace.join("linked")).expect("symlink");

    let ctx = ToolContext::new(workspace).with_follow_symlinks(true);
    let resolved = ctx
        .resolve_path("linked/new.txt")
        .expect("path under workspace symlink should resolve");

    let expected = outside
        .join("target")
        .canonicalize()
        .expect("canonical target")
        .join("new.txt");
    assert_eq!(resolved, normalize_path(&expected));
}

#[test]
#[cfg(unix)]
fn test_tool_context_default_mode_rejects_nonexistent_path_under_workspace_symlink() {
    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    std::fs::create_dir_all(outside.join("target")).expect("mkdir outside target");
    symlink(outside.join("target"), workspace.join("linked")).expect("symlink");

    let ctx = ToolContext::new(workspace);
    let err = ctx
        .resolve_path("linked/new.txt")
        .expect_err("default mode should still reject workspace symlink escapes");

    assert!(matches!(err, ToolError::PathEscape { .. }));
}

fn scoped_authority(roots: &[&str], files: &[&str]) -> ToolAuthorityEnvelope {
    ToolAuthorityEnvelope {
        schema_version: 1,
        owner: "fleet-worker-1".to_string(),
        authority: ToolMutationAuthority::ScopedWrite,
        network_access: None,
        shell: ToolShellAuthority::None,
        verification: ToolVerificationAuthority::None,
        writable_roots: roots.iter().map(|value| (*value).to_string()).collect(),
        writable_files: files.iter().map(|value| (*value).to_string()).collect(),
        coordination_contracts: Vec::new(),
    }
    .normalized()
    .expect("valid test authority")
}

#[test]
fn tool_authority_allows_normal_nonexistent_children_only_inside_scope() {
    let tmp = tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join("src")).expect("src");
    let context = ToolContext::new(tmp.path().to_path_buf());
    let authority = scoped_authority(&["src"], &[]);

    assert!(
        authority
            .permits_mutation_path(&context, "src/new/nested.rs")
            .expect("normal nonexistent child")
    );
    assert!(
        !authority
            .permits_mutation_path(&context, "docs/outside.md")
            .expect("ordinary out-of-scope path")
    );
}

#[cfg(unix)]
#[test]
fn tool_authority_rejects_exact_file_symlink_aliases() {
    let tmp = tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join("src")).expect("src");
    std::fs::create_dir(tmp.path().join("other")).expect("other");
    std::fs::write(tmp.path().join("other/target.rs"), "outside scope\n").expect("target");
    symlink("../other/target.rs", tmp.path().join("src/alias.rs")).expect("alias");
    let context = ToolContext::new(tmp.path().to_path_buf());
    let authority = scoped_authority(&[], &["src/alias.rs"]);

    let error = authority
        .permits_mutation_path(&context, "src/alias.rs")
        .expect_err("an exact-file claim must not authorize a symlink target")
        .to_string();
    assert!(error.contains("must not traverse symlinks"), "{error}");
}

#[cfg(unix)]
#[test]
fn tool_authority_rejects_claimed_root_and_child_symlink_aliases() {
    let tmp = tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join("real")).expect("real");
    symlink("real", tmp.path().join("linked")).expect("linked root");
    let context = ToolContext::new(tmp.path().to_path_buf());
    let claimed_alias = scoped_authority(&["linked"], &[]);
    let claimed_real = scoped_authority(&["real"], &[]);

    for (authority, path) in [
        (&claimed_alias, "linked/new.rs"),
        (&claimed_real, "linked/new.rs"),
    ] {
        let error = authority
            .permits_mutation_path(&context, path)
            .expect_err("symlinked roots and mutation paths must fail closed")
            .to_string();
        assert!(error.contains("must not traverse symlinks"), "{error}");
    }
}

#[test]
fn nested_tool_authority_may_only_narrow_the_outer_cap() {
    let tmp = tempdir().expect("tempdir");
    let outer = scoped_authority(&["src"], &["Cargo.toml"]);
    let narrower = scoped_authority(&["src/parser"], &[]);
    let expansion = scoped_authority(&["docs"], &[]);
    ToolContext::new(tmp.path().to_path_buf())
        .with_tool_authority(outer.clone())
        .unwrap()
        .with_tool_authority(narrower)
        .expect("nested scope may narrow");
    let error = ToolContext::new(tmp.path().to_path_buf())
        .with_tool_authority(outer.clone())
        .unwrap()
        .with_tool_authority(expansion)
        .err()
        .expect("nested scope expansion must fail closed");
    assert!(error.contains("cannot expand"), "{error}");

    let read_only = ToolAuthorityEnvelope {
        schema_version: 1,
        owner: "read-only-child".to_string(),
        authority: ToolMutationAuthority::ReadOnly,
        network_access: None,
        shell: ToolShellAuthority::None,
        verification: ToolVerificationAuthority::None,
        writable_roots: Vec::new(),
        writable_files: Vec::new(),
        coordination_contracts: Vec::new(),
    };
    ToolContext::new(tmp.path().to_path_buf())
        .with_tool_authority(outer.clone())
        .unwrap()
        .with_tool_authority(read_only)
        .expect("read-only always narrows a write cap");

    let shell_expansion = ToolAuthorityEnvelope {
        schema_version: 1,
        owner: "shell-expansion".to_string(),
        authority: ToolMutationAuthority::ReadOnly,
        network_access: None,
        shell: ToolShellAuthority::ReadOnly,
        verification: ToolVerificationAuthority::None,
        writable_roots: Vec::new(),
        writable_files: Vec::new(),
        coordination_contracts: Vec::new(),
    };
    ToolContext::new(tmp.path().to_path_buf())
        .with_tool_authority(outer)
        .unwrap()
        .with_tool_authority(shell_expansion)
        .err()
        .expect("nested authority cannot add a shell cap the outer process lacks");
}

#[test]
fn legacy_v1_authority_envelopes_default_to_shell_none() {
    let authority = ToolAuthorityEnvelope::from_json(
        r#"{"schema_version":1,"owner":"legacy-worker","authority":"read_only"}"#,
    )
    .expect("pre-shell v1 envelope remains readable");
    assert_eq!(authority.shell, ToolShellAuthority::None);
    assert_eq!(authority.verification, ToolVerificationAuthority::None);
}

#[test]
fn headless_fleet_registers_bash_only_when_the_clamped_ceiling_keeps_it() {
    assert!(fleet_exec_shell_enabled(
        true,
        ToolShellAuthority::ReadOnly,
        None
    ));
    assert!(!fleet_exec_shell_enabled(
        true,
        ToolShellAuthority::ReadOnly,
        Some(&["ba*".into()])
    ));
    assert!(!fleet_exec_shell_enabled(
        true,
        ToolShellAuthority::None,
        None
    ));
}

#[test]
fn bounded_verification_is_typed_and_cannot_smuggle_bash_authority() {
    let bounded = ToolAuthorityEnvelope::from_json(
        r#"{"schema_version":1,"owner":"verifier","authority":"read_only","verification":"bounded"}"#,
    )
    .expect("bounded verifier authority");
    assert_eq!(bounded.verification, ToolVerificationAuthority::Bounded);

    let widened = ToolAuthorityEnvelope {
        shell: ToolShellAuthority::ReadOnly,
        ..bounded
    };
    assert!(
        widened.normalized().is_err(),
        "bounded verification and Bash authority are separate, non-composable caps"
    );
}

#[test]
fn read_only_machine_authority_clamps_live_shell_policy() {
    let tmp = tempdir().expect("tempdir");
    let read_only = ToolAuthorityEnvelope {
        schema_version: 1,
        owner: "scout".to_string(),
        authority: ToolMutationAuthority::ReadOnly,
        network_access: Some(true),
        shell: ToolShellAuthority::ReadOnly,
        verification: ToolVerificationAuthority::None,
        writable_roots: Vec::new(),
        writable_files: Vec::new(),
        coordination_contracts: Vec::new(),
    };
    let mut context = ToolContext::new(tmp.path().to_path_buf())
        .with_tool_authority(read_only)
        .expect("read-only authority");

    assert_eq!(context.shell_policy, ShellPolicy::ReadOnly);
    context.set_shell_policy(ShellPolicy::Full);
    assert_eq!(
        context.shell_policy,
        ShellPolicy::ReadOnly,
        "a live mode refresh must not widen the process authority cap"
    );

    let scoped = ToolContext::new(tmp.path().to_path_buf())
        .with_tool_authority(scoped_authority(&["src"], &[]))
        .expect("scoped authority")
        .with_shell_policy(ShellPolicy::Full);
    assert_eq!(scoped.shell_policy, ShellPolicy::None);
}

#[test]
fn process_tool_authority_inherits_into_all_context_constructors() {
    const CHILD_ENV: &str = "CODEWHALE_TEST_PROCESS_TOOL_AUTHORITY_CHILD";
    if std::env::var_os(CHILD_ENV).is_some() {
        let tmp = tempdir().expect("tempdir");
        install_process_tool_authority(ToolAuthorityEnvelope {
            schema_version: 1,
            owner: "fleet-worker-child-process".to_string(),
            authority: ToolMutationAuthority::ReadOnly,
            network_access: None,
            shell: ToolShellAuthority::ReadOnly,
            verification: ToolVerificationAuthority::None,
            writable_roots: Vec::new(),
            writable_files: Vec::new(),
            coordination_contracts: Vec::new(),
        })
        .expect("install process authority once in isolated child");
        let notes = tmp.path().join("notes.md");
        let contexts = [
            ToolContext::new(tmp.path().to_path_buf()),
            ToolContext::with_options(tmp.path().to_path_buf(), false, notes.clone()),
            ToolContext::with_auto_approve(tmp.path().to_path_buf(), false, notes, true),
        ];
        for context in contexts {
            let authority = context
                .tool_authority
                .as_ref()
                .expect("every constructor inherits process authority");
            assert_eq!(authority.owner, "fleet-worker-child-process");
            assert_eq!(authority.authority, ToolMutationAuthority::ReadOnly);
            assert_eq!(context.shell_policy, ShellPolicy::ReadOnly);
        }
        return;
    }

    let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .arg("--exact")
        .arg("tools::spec::tests::process_tool_authority_inherits_into_all_context_constructors")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .output()
        .expect("spawn isolated authority test child");
    assert!(
        output.status.success(),
        "child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_required_str() {
    let input = json!({"name": "test", "count": 42});
    assert_eq!(required_str(&input, "name").unwrap(), "test");
    assert!(required_str(&input, "missing").is_err());
    assert!(required_str(&input, "count").is_err()); // not a string
}

#[test]
fn test_optional_str() {
    let input = json!({"name": "test", "count": 7});
    assert_eq!(optional_str(&input, "name").unwrap(), Some("test"));
    assert_eq!(optional_str(&input, "missing").unwrap(), None);
    // An explicit null is the wire spelling of "absent", not a type error.
    assert_eq!(optional_str(&json!({"name": null}), "name").unwrap(), None);
    let err = optional_str(&input, "count").expect_err("a number is not a string");
    let err = err.to_string();
    assert!(
        err.contains("count") && err.contains("number") && err.contains("string"),
        "{err}"
    );
}

#[test]
fn test_required_u64() {
    let input = json!({"count": 42});
    assert_eq!(required_u64(&input, "count").unwrap(), 42);
    assert!(required_u64(&input, "missing").is_err());
}

#[test]
fn test_optional_u64() {
    let input = json!({"count": 42});
    assert_eq!(optional_u64(&input, "count", 0).unwrap(), 42);
    assert_eq!(optional_u64(&input, "missing", 100).unwrap(), 100);
    assert_eq!(
        optional_u64(&json!({"count": null}), "count", 9).unwrap(),
        9
    );
    // A stringy number keeps its default today only because the harness
    // never noticed; it must be an error instead.
    for bad in [json!("42"), json!(-1), json!(2.5), json!([42])] {
        let err = optional_u64(&json!({"count": bad}), "count", 100)
            .expect_err("a non-integer must not fall back to the default")
            .to_string();
        assert!(
            err.contains("count") && err.contains("non-negative integer"),
            "{err}"
        );
    }
}

#[test]
fn test_optional_bool() {
    let input = json!({"flag": true});
    assert!(optional_bool(&input, "flag", false).unwrap());
    assert!(!optional_bool(&input, "missing", false).unwrap());
    assert!(optional_bool(&json!({"flag": null}), "flag", true).unwrap());
    // The whole point: "true" must never become the default `false`.
    for bad in [json!("true"), json!("false"), json!(1), json!(0), json!([])] {
        let err = optional_bool(&json!({"flag": bad}), "flag", false)
            .expect_err("a non-boolean must not fall back to the default")
            .to_string();
        assert!(err.contains("flag") && err.contains("boolean"), "{err}");
    }
}

#[test]
fn test_tool_error_display() {
    let err = ToolError::missing_field("path");
    assert_eq!(
        format!("{err}"),
        "Failed to validate input: missing required field 'path'"
    );

    let err = ToolError::execution_failed("boom");
    assert_eq!(format!("{err}"), "Failed to execute tool: boom");
}

#[test]
fn test_approval_requirement_default() {
    let level = ApprovalRequirement::default();
    assert_eq!(level, ApprovalRequirement::Auto);
}

#[test]
fn test_resolve_home_path_exact_prefixes() {
    let fake_home = PathBuf::from("/fake/user/home");

    // Exact ~ and ~/ prefixes resolve
    assert_eq!(
        resolve_home_path_with("~", || Some(fake_home.clone())).unwrap(),
        Some(fake_home.clone())
    );
    assert_eq!(
        resolve_home_path_with("~/", || Some(fake_home.clone())).unwrap(),
        Some(fake_home.clone())
    );
    assert_eq!(
        resolve_home_path_with("~//", || Some(fake_home.clone())).unwrap(),
        Some(fake_home.clone())
    );
    assert_eq!(
        resolve_home_path_with("~/file.txt", || Some(fake_home.clone())).unwrap(),
        Some(fake_home.join("file.txt"))
    );
    assert_eq!(
        resolve_home_path_with("~/a/b/c.md", || Some(fake_home.clone())).unwrap(),
        Some(fake_home.join("a/b/c.md"))
    );

    #[cfg(windows)]
    {
        assert_eq!(
            resolve_home_path_with(r"~\", || Some(fake_home.clone())).unwrap(),
            Some(fake_home.clone())
        );
        assert_eq!(
            resolve_home_path_with(r"~\file.txt", || Some(fake_home.clone())).unwrap(),
            Some(fake_home.join("file.txt"))
        );
    }

    // Must NOT expand ~otheruser, shell variables, command substitutions, globs, or literals
    assert_eq!(
        resolve_home_path_with("~otheruser", || Some(fake_home.clone())).unwrap(),
        None
    );
    assert_eq!(
        resolve_home_path_with("~otheruser/file", || Some(fake_home.clone())).unwrap(),
        None
    );
    assert_eq!(
        resolve_home_path_with("./~/file", || Some(fake_home.clone())).unwrap(),
        None
    );
    assert_eq!(
        resolve_home_path_with("$HOME/file", || Some(fake_home.clone())).unwrap(),
        None
    );
    assert_eq!(
        resolve_home_path_with("`whoami`/file", || Some(fake_home.clone())).unwrap(),
        None
    );
    assert_eq!(
        resolve_home_path_with("~*", || Some(fake_home.clone())).unwrap(),
        None
    );
    assert_eq!(
        resolve_home_path_with("regular/path", || Some(fake_home.clone())).unwrap(),
        None
    );
    assert_eq!(
        resolve_home_path_with("/absolute/path", || Some(fake_home.clone())).unwrap(),
        None
    );
}

#[test]
fn test_resolve_home_path_unknown_home_fails_explicitly_without_cwd_guessing() {
    let err = resolve_home_path_with("~", || None).expect_err("unknown home must fail explicitly");
    let msg = err.to_string();
    assert!(
        msg.contains("user home directory could not be determined"),
        "error message must be explicit: {msg}"
    );

    let err = resolve_home_path_with("~/nested/file.txt", || None)
        .expect_err("unknown home must fail explicitly");
    let msg = err.to_string();
    assert!(
        msg.contains("user home directory could not be determined"),
        "error message must be explicit: {msg}"
    );
}

#[test]
fn test_tool_context_resolve_path_home_prefix_inside_workspace() {
    // The fixture lives inside the home, so the home must be the test's own.
    let _sealed = crate::test_support::SealedHome::new();
    let real_home = codewhale_paths::user_home().expect("test home must be available");
    let home_temp = tempfile::Builder::new()
        .prefix("cw_spec_test_home_")
        .tempdir_in(&real_home)
        .expect("create fixture inside test home");

    let test_file = home_temp.path().join("inside.txt");
    std::fs::write(&test_file, "inside workspace").expect("write test file");

    let rel = test_file
        .strip_prefix(&real_home)
        .expect("fixture is below test home");
    let tilde_path = format!("~/{}", rel.to_string_lossy());

    let ctx = ToolContext::new(home_temp.path().to_path_buf());
    let resolved = ctx
        .resolve_path(&tilde_path)
        .expect("home path inside workspace should resolve");

    let expected = test_file
        .canonicalize()
        .unwrap_or_else(|_| normalize_path(&test_file));
    assert_eq!(resolved, expected);
}

#[test]
fn test_tool_context_resolve_path_home_prefix_restricted_refusal() {
    let workspace = tempdir().expect("workspace tempdir");
    let ctx = ToolContext::new(workspace.path().to_path_buf());

    // A home path outside workspace without trusted external path or trust mode must be refused
    let err = ctx
        .resolve_path("~/some_untrusted_file_never_present_12345.txt")
        .expect_err("home path outside workspace must error");
    assert!(matches!(err, ToolError::PathEscape { .. }));
}

#[test]
fn test_tool_context_resolve_path_home_prefix_trusted_external_path() {
    // The fixture lives inside the home, so the home must be the test's own.
    let _sealed = crate::test_support::SealedHome::new();
    let real_home = codewhale_paths::user_home().expect("test home must be available");
    let trusted_dir = tempfile::Builder::new()
        .prefix("cw_spec_trusted_home_")
        .tempdir_in(&real_home)
        .expect("create fixture inside test home");
    let trusted_file = trusted_dir.path().join("shared.md");
    std::fs::write(&trusted_file, "shared content").expect("write trusted file");

    let rel = trusted_file
        .strip_prefix(&real_home)
        .expect("fixture is below test home");
    let tilde_path = format!("~/{}", rel.to_string_lossy());

    let workspace = tempdir().expect("workspace tempdir");
    let canonical_trusted = trusted_dir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| trusted_dir.path().to_path_buf());
    let ctx = ToolContext::new(workspace.path().to_path_buf())
        .with_trusted_external_paths(vec![canonical_trusted]);

    let resolved = ctx
        .resolve_path(&tilde_path)
        .expect("trusted external home path should resolve");
    assert_eq!(resolved, trusted_file.canonicalize().unwrap());
}

#[test]
fn test_tool_context_resolve_path_literal_tilde_in_workspace() {
    let workspace = tempdir().expect("workspace tempdir");
    let literal_tilde = workspace.path().join("~");
    std::fs::create_dir_all(&literal_tilde).expect("create literal ~ dir");
    let literal_file = literal_tilde.join("nested.txt");
    std::fs::write(&literal_file, "literal tilde data").expect("write literal");

    let ctx = ToolContext::new(workspace.path().to_path_buf());
    let resolved = ctx
        .resolve_path("./~/nested.txt")
        .expect("literal ./~/ path must resolve inside workspace");
    assert_eq!(resolved, literal_file.canonicalize().unwrap());
}

#[test]
fn test_tool_context_resolve_path_no_shell_expansion() {
    let workspace = tempdir().expect("workspace tempdir");
    let ctx = ToolContext::new(workspace.path().to_path_buf());

    // $HOME/file should NOT expand shell env var, but be treated relative to workspace
    let resolved = ctx
        .resolve_path("$HOME/file.txt")
        .expect("should treat as workspace child");
    assert!(
        resolved.starts_with(
            workspace
                .path()
                .canonicalize()
                .unwrap_or_else(|_| workspace.path().to_path_buf())
        )
    );
    assert!(resolved.to_string_lossy().contains("$HOME"));

    // ~otheruser should NOT expand other user home, but be treated relative to workspace
    let resolved_other = ctx
        .resolve_path("~otheruser/file.txt")
        .expect("should treat as workspace child");
    assert!(resolved_other.to_string_lossy().contains("~otheruser"));
}

#[test]
fn delegated_shell_is_refused_outright_when_the_session_lacks_it() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut ctx = ToolContext::new(workspace.path());

    // Leaving the flag out would fall back to the host default, which may
    // allow shell: a session without full shell must store an explicit "no".
    ctx.shell_policy = codewhale_agent_contract::worker_profile::ShellPolicy::None;
    assert_eq!(ctx.cap_delegated_authority(None, None, None).0, Some(false));
    assert_eq!(
        ctx.cap_delegated_authority(Some(true), None, None).0,
        Some(false)
    );

    ctx.shell_policy = codewhale_agent_contract::worker_profile::ShellPolicy::Full;
    assert_eq!(ctx.cap_delegated_authority(None, None, None).0, None);
    assert_eq!(
        ctx.cap_delegated_authority(Some(true), None, None).0,
        Some(true)
    );
}
