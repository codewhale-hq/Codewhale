pub(crate) use codewhale_engine::test_support::*;

#[cfg(unix)]
mod tests {
    use std::path::Path;
    use std::time::UNIX_EPOCH;

    /// Tripwire for the leak this module exists to prevent: run a sample of
    /// the tests that once wrote `~/.codewhale` in a child process whose
    /// *ambient* `HOME` and `CODEWHALE_HOME` are a seeded, read-only sentinel,
    /// then require both that they pass and that the sentinel is byte-for-byte
    /// as it was. A test that writes the ambient home without sealing its own
    /// fails here by name, instead of silently rewriting a developer's profile.
    ///
    /// The sample covers one test per resolver class that leaked
    /// (`App::new`, command dispatch, sessions, snapshots, plugin bundles,
    /// credentials, the `/import-claude` report). To sweep a whole family,
    /// set `CODEWHALE_TEST_AMBIENT_HOME_FILTER` to a libtest filter such as
    /// `commands::`; that takes minutes, which is why it is not the default.
    #[test]
    fn ambient_home_survives_a_sample_of_state_touching_tests() {
        use std::os::unix::fs::PermissionsExt;

        const PROBE_ENV: &str = "CODEWHALE_TEST_AMBIENT_HOME_PROBE";
        const FILTER_ENV: &str = "CODEWHALE_TEST_AMBIENT_HOME_FILTER";
        const SAMPLE: &[&str] = &[
            "commands::contract::tests::bundle_construction_performs_no_eager_work",
            "commands::contract::tests::skill_group_snapshot_list_and_restore_roundtrip",
            "commands::debug_diagnostics_host_tests::test_context_report_subcommands_return_source_map",
            "commands::debug_mutation_host_tests::test_patch_undo_requests_durable_rewind_after_restore",
            "commands::groups::core::core::tests::test_clear_resets_all_state",
            "commands::groups::plugins::tests::kimi_managed_import_refuses_linked_children",
            "commands::session_lifecycle_regression_tests::fork_saves_parent_and_switches_to_child_session",
            "commands::session_lifecycle_regression_tests::test_save_creates_file_and_sets_session_id",
            "commands::tests::every_registered_command_dispatches_to_a_handler",
            "commands::tests::every_command_alias_dispatches_to_a_handler",
            "commands::tests::feat020_plugin_dispatches_through_public_seam",
        ];

        // The child runs the sample; it must not recurse into this probe.
        if std::env::var_os(PROBE_ENV).is_some() {
            return;
        }

        fn walk(dir: &Path, visit: &mut dyn FnMut(&Path, &std::fs::Metadata)) {
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .expect("read sentinel dir")
                .map(|entry| entry.expect("sentinel entry").path())
                .collect();
            entries.sort();
            for path in entries {
                let metadata = std::fs::symlink_metadata(&path).expect("sentinel metadata");
                visit(&path, &metadata);
                if metadata.is_dir() {
                    walk(&path, visit);
                }
            }
        }
        fn listing(root: &Path) -> Vec<String> {
            let mut out = Vec::new();
            walk(root, &mut |path, metadata| {
                let mtime = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |elapsed| elapsed.as_nanos());
                out.push(format!(
                    "{} dir={} len={} mtime={mtime} contents={:?}",
                    path.strip_prefix(root).unwrap_or(path).display(),
                    metadata.is_dir(),
                    metadata.len(),
                    metadata
                        .is_file()
                        .then(|| std::fs::read(path).expect("read sentinel file"))
                ));
            });
            out
        }
        fn set_readonly(root: &Path, readonly: bool) {
            let mode = |dir: bool| match (dir, readonly) {
                (true, true) => 0o500,
                (true, false) => 0o700,
                (false, true) => 0o400,
                (false, false) => 0o600,
            };
            let apply = |path: &Path, dir: bool| {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode(dir)))
                    .expect("set sentinel permissions");
            };
            walk(root, &mut |path, metadata| {
                if !metadata.file_type().is_symlink() {
                    apply(path, metadata.is_dir());
                }
            });
            apply(root, true);
        }

        let sentinel = tempfile::tempdir().expect("sentinel tempdir");
        let home = sentinel.path().join("home");
        let codewhale_home = sentinel.path().join("codewhale-home");
        // Content a leaking test would read, rewrite or migrate.
        for (relative, body) in [
            (
                "home/.claude/CLAUDE.md",
                "# someone's Claude instructions\n",
            ),
            ("home/.claude.json", "{\"mcpServers\":{}}\n"),
            ("home/.codewhale/instructions.md", "# real instructions\n"),
            ("home/.deepseek/config.toml", "model = \"keep-me\"\n"),
            ("home/.deepseek/sessions/legacy.json", "{}\n"),
            ("codewhale-home/instructions.md", "# real instructions\n"),
            ("codewhale-home/sessions/keep.json", "{}\n"),
        ] {
            let path = sentinel.path().join(relative);
            std::fs::create_dir_all(path.parent().expect("sentinel parent")).expect("seed dir");
            std::fs::write(path, body).expect("seed file");
        }
        assert!(home.is_dir() && codewhale_home.is_dir());
        let before = listing(sentinel.path());
        set_readonly(sentinel.path(), true);

        let filter = std::env::var(FILTER_ENV).ok().filter(|f| !f.is_empty());
        let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"));
        child.arg("--test-threads=4");
        match &filter {
            Some(filter) => child.arg(filter),
            None => child.arg("--exact").args(SAMPLE),
        };
        let output = child
            .env(PROBE_ENV, "1")
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("CODEWHALE_HOME", &codewhale_home)
            .env_remove("HOMEDRIVE")
            .env_remove("HOMEPATH")
            .env_remove("CODEWHALE_CONFIG_PATH")
            .env_remove("DEEPSEEK_CONFIG_PATH")
            .env_remove("DEEPSEEK_HOME")
            .output();

        // Restore write access first so a failure still cleans up after itself.
        set_readonly(sentinel.path(), false);
        let output = output.expect("run the sample under a read-only ambient home");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let report = format!("stdout:\n{stdout}\nstderr:\n{stderr}");

        assert!(
            output.status.success(),
            "a test failed under a read-only ambient home — it reaches the user's \
             state without sealing it (use SealedHome, or fence the resolver):\n{report}"
        );
        if filter.is_none() {
            assert!(
                stdout.contains(&format!("running {} tests", SAMPLE.len())),
                "the sample names drifted from real tests; update SAMPLE:\n{report}"
            );
        }
        assert_eq!(
            listing(sentinel.path()),
            before,
            "a test wrote the ambient home:\n{report}"
        );
    }
}
