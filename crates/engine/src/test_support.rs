//! Shared test-only helpers.

#[cfg(test)]
use std::ffi::OsString;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::OnceLock;
#[cfg(all(unix, test))]
use std::time::UNIX_EPOCH;

pub use codewhale_runtime::test_support::*;

/// `CODEWHALE_CONFORMANCE_UPDATE=1` rewrites goldens from the current source.
pub const UPDATE_ENV: &str = "CODEWHALE_CONFORMANCE_UPDATE";

pub fn update_mode() -> bool {
    let requested = std::env::var(UPDATE_ENV).is_ok_and(|value| value == "1");
    if requested && std::env::var_os("CI").is_some_and(|value| !value.is_empty()) {
        panic!("{UPDATE_ENV}=1 is refused under CI: goldens are reviewed source, not build output");
    }
    requested
}

/// Compare `actual` with the golden at `path`; in update mode write it
/// instead. Returns a failure description rather than panicking so a family
/// can report every drifted case in one run.
pub fn check_golden(path: &Path, actual: &str) -> Result<(), String> {
    let update = update_mode();
    let expected = std::fs::read_to_string(path).ok();
    if expected.as_deref() == Some(actual) {
        return Ok(());
    }
    if update {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create golden dir");
        }
        std::fs::write(path, actual).expect("write golden");
        eprintln!("conformance: rewrote {}", path.display());
        return Ok(());
    }
    let Some(expected) = expected else {
        return Err(format!(
            "missing golden {}; review the output and record it with {UPDATE_ENV}=1",
            path.display()
        ));
    };
    Err(format!(
        "golden drift at {}\n{}\nIf this change is intended, re-record with {UPDATE_ENV}=1 and review the diff.",
        path.display(),
        first_difference(&expected, actual)
    ))
}

fn first_difference(expected: &str, actual: &str) -> String {
    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();
    let index = expected_lines
        .iter()
        .zip(&actual_lines)
        .position(|(left, right)| left != right)
        .unwrap_or_else(|| expected_lines.len().min(actual_lines.len()));
    let character = match (expected_lines.get(index), actual_lines.get(index)) {
        (Some(left), Some(right)) => left
            .chars()
            .zip(right.chars())
            .take_while(|(left, right)| left == right)
            .count(),
        _ => 0,
    };
    let start = character.saturating_sub(40);
    let show = |lines: &[&str]| {
        lines.get(index).map_or_else(
            || "<end of file>".to_string(),
            |line| {
                let window: String = line.chars().skip(start).take(601).collect();
                format!(
                    "{}{}",
                    if start == 0 { "" } else { "…" },
                    truncate(&window, 600)
                )
            },
        )
    };
    format!(
        "first difference at line {}, character {} (expected {} lines, got {}):\n  expected: {}\n  actual:   {}",
        index + 1,
        character + 1,
        expected_lines.len(),
        actual_lines.len(),
        show(&expected_lines),
        show(&actual_lines)
    )
}

fn truncate(line: &str, max: usize) -> String {
    if line.chars().count() <= max {
        return line.to_string();
    }
    let head: String = line.chars().take(max).collect();
    format!("{head}…")
}

/// A fixture workspace an OS sandbox can still see (#6305).
///
/// The Linux bwrap wrapper mounts a fresh `--tmpfs /tmp` before it binds the
/// policy's writable roots (`sandbox/bwrap.rs`), and an enforced read-only
/// command has no writable roots at all — nothing re-exposes the host `/tmp`.
/// A fixture rooted there is shadowed inside the sandbox, so the trailing
/// `--chdir <workspace>` lands on a path that no longer exists and bwrap exits
/// with `Can't chdir to /tmp/.tmpXXXXXX`. The tmpfs is deliberate isolation and
/// a security boundary, so the fixture moves instead of the mount.
///
/// Known limitations: this relocates only the directory the sandboxed command
/// chdirs into. It does not make anything else under the host `/tmp` reachable
/// from inside the sandbox, and it says nothing about `/dev` or `/proc`, which
/// bwrap also replaces. Use it for the sandbox probes; plain `tempfile::tempdir`
/// stays correct everywhere else.
// Every caller is `#[cfg(unix)]` (the bwrap probes); on Windows these would
// be dead code and CI builds tests with `-Dwarnings`.
#[cfg(unix)]
pub fn sandbox_visible_tempdir() -> tempfile::TempDir {
    let root = sandbox_visible_fixture_root();
    tempfile::tempdir_in(root).unwrap_or_else(|error| {
        panic!(
            "failed to create a sandbox-visible fixture workspace in {}: {error}",
            root.display()
        )
    })
}

/// `OUT_DIR` is this crate's own build directory, so it follows the Cargo
/// target directory rather than `TMPDIR` — the one path every test binary
/// already owns and that is outside `/tmp` in every normal layout. A target
/// directory deliberately placed under `/tmp` would silently reintroduce
/// #6305, so say so instead of handing back a shadowed path.
#[cfg(unix)]
fn sandbox_visible_fixture_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = Path::new(env!("OUT_DIR")).join("sandbox-fixtures");
        assert!(
            !root.starts_with("/tmp"),
            "sandbox fixtures need a root outside /tmp, which bwrap replaces with a fresh \
             tmpfs: {} is under it. Point CARGO_TARGET_DIR somewhere else.",
            root.display()
        );
        std::fs::create_dir_all(&root).unwrap_or_else(|error| {
            panic!(
                "failed to create the sandbox fixture root {}: {error}",
                root.display()
            )
        });
        root
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn env_snapshot(keys: &[&str]) -> Vec<Option<OsString>> {
        let _lock = lock_test_env();
        keys.iter().map(std::env::var_os).collect()
    }

    #[test]
    fn sealed_home_pins_every_alias_and_restores_the_environment() {
        const KEYS: [&str; 8] = [
            "HOME",
            "USERPROFILE",
            "HOMEDRIVE",
            "HOMEPATH",
            "CODEWHALE_HOME",
            "CODEWHALE_CONFIG_PATH",
            "DEEPSEEK_CONFIG_PATH",
            "DEEPSEEK_HOME",
        ];
        let before = env_snapshot(&KEYS);
        {
            let seal = SealedHome::new();
            let now = |key: &str| std::env::var_os(key);
            assert_eq!(now("HOME").as_deref(), Some(seal.home().as_os_str()));
            assert_eq!(now("USERPROFILE").as_deref(), Some(seal.home().as_os_str()));
            assert_eq!(
                now("CODEWHALE_HOME").as_deref(),
                Some(seal.codewhale_home().as_os_str())
            );
            for alias in [
                "HOMEDRIVE",
                "HOMEPATH",
                "CODEWHALE_CONFIG_PATH",
                "DEEPSEEK_CONFIG_PATH",
                "DEEPSEEK_HOME",
            ] {
                assert_eq!(now(alias), None, "{alias} would route around the seal");
            }
            assert!(guarded_environment_provides_state_paths());
            assert!(home_is_sealed());
            // A resolver that follows the environment lands inside the seal.
            let sessions = crate::session_manager::default_sessions_dir().expect("sessions dir");
            assert_eq!(sessions, seal.codewhale_home().join("sessions"));
        }
        {
            let owned = tempfile::tempdir().expect("owned home");
            let seal = SealedHome::at(owned.path());
            assert_eq!(seal.home(), owned.path());
            assert_eq!(seal.codewhale_home(), owned.path().join(".codewhale"));
        }
        assert_eq!(env_snapshot(&KEYS), before, "the seal must restore the env");
    }

    /// The fence on every resolver a test reaches without ever thinking about
    /// the home: an unsealed test must land in the isolated root, whatever the
    /// ambient `HOME` / `CODEWHALE_HOME` say. Add a resolver here when it grows
    /// a `#[cfg(any(test, feature = "test-support"))]` fence — and fence the next one before a test finds it.
    #[test]
    fn unsealed_state_resolvers_stay_inside_the_isolated_root() {
        // Hold the barrier to keep the fixture's environment stable.
        let _lock = lock_test_env();
        assert!(!home_is_sealed());
        let root = isolated_test_state_root();
        let inside = |label: &str, path: &Path| {
            assert!(
                path.starts_with(root),
                "{label} resolved outside the isolated test root: {}",
                path.display()
            );
        };

        inside(
            "sessions",
            &crate::session_manager::default_sessions_dir().expect("sessions dir"),
        );
        inside(
            "snapshots",
            &codewhale_snapshot::snapshot_dir_for(Path::new("/nonexistent/codewhale-fence-probe"))
                .expect("snapshot dir"),
        );
        let audit = codewhale_runtime::audit::audit_log_path().expect("audit log path");
        assert!(
            audit.starts_with(std::env::temp_dir()),
            "the audit log resolved outside the temp dir: {}",
            audit.display()
        );
    }

    #[test]
    fn home_seal_is_available_only_to_its_owner_and_enrolled_workers() {
        let seal = SealedHome::new();
        let sessions = seal.codewhale_home().join("sessions");
        let ticket = env_scope_ticket().expect("seal owns an environment scope");

        std::thread::spawn(move || {
            assert!(!home_is_sealed(), "a foreign worker cannot follow the seal");
            assert!(
                crate::session_manager::default_sessions_dir()
                    .expect("isolated sessions dir")
                    .starts_with(isolated_test_state_root())
            );

            let membership = join_env_scope(Some(ticket)).expect("enroll worker");
            assert!(home_is_sealed());
            assert_eq!(
                crate::session_manager::default_sessions_dir().expect("sealed sessions dir"),
                sessions
            );
            drop(membership);
            assert!(!home_is_sealed(), "leaving the scope withdraws the seal");
        })
        .join()
        .expect("home seal worker");
    }

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
    #[cfg(unix)]
    #[test]
    fn ambient_home_survives_a_sample_of_state_touching_tests() {
        use std::os::unix::fs::PermissionsExt;

        const PROBE_ENV: &str = "CODEWHALE_TEST_AMBIENT_HOME_PROBE";
        const FILTER_ENV: &str = "CODEWHALE_TEST_AMBIENT_HOME_FILTER";
        const SAMPLE: &[&str] = &[
            "commands::contract::tests::bundle_construction_performs_no_eager_work",
            "commands::contract::tests::skill_group_snapshot_list_and_restore_roundtrip",
            "commands::debug_diagnostics_host_tests::test_context_report_subcommands_return_source_map",
            "commands::debug_mutation_host_tests::test_patch_undo_requests_session_resync_after_restore",
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

    #[test]
    fn removing_home_variables_does_not_seal_a_missing_path() {
        let _lock = lock_test_env();
        let _home = EnvVarGuard::remove("HOME");
        let _userprofile = EnvVarGuard::remove("USERPROFILE");

        assert!(
            !guarded_environment_provides_state_paths(),
            "removing HOME variables must keep state in the isolated test root"
        );
        assert_eq!(
            codewhale_runtime::config_persistence::config_toml_path(None)
                .expect("resolve isolated config path"),
            unsealed_test_state_root().join(codewhale_config::CONFIG_FILE_NAME)
        );
    }

    #[test]
    fn lock_without_sealed_paths_does_not_use_developer_config() {
        let _lock = lock_test_env();
        let path = codewhale_runtime::config_persistence::config_toml_path(None)
            .expect("resolve isolated config path");
        let root = isolated_test_state_root();
        assert!(
            path.starts_with(root),
            "holding lock_test_env without an EnvVarGuard must not read ~/.codewhale ({})",
            path.display()
        );
        assert_eq!(
            path,
            unsealed_test_state_root().join(codewhale_config::CONFIG_FILE_NAME)
        );
    }

    #[test]
    fn unguarded_state_writes_use_isolated_test_root() {
        const PROBE_ENV: &str = "CODEWHALE_TEST_STATE_ISOLATION_PROBE";
        const RECEIPT_ENV: &str = "CODEWHALE_TEST_STATE_ISOLATION_RECEIPT";

        if std::env::var_os(PROBE_ENV).is_some() {
            let config_path = codewhale_runtime::config_persistence::persist_root_bool_key(
                None,
                "allow_shell",
                true,
            )
            .expect("write isolated config");
            let direct_config_path = codewhale_runtime::config::save_workspace_trust(Path::new(
                "/tmp/codewhale-test-workspace",
            ))
            .expect("write through direct default config path");
            codewhale_runtime::settings::Settings::default()
                .save()
                .expect("write isolated settings");
            let settings_path =
                codewhale_runtime::settings::Settings::path().expect("resolve isolated settings");
            let root = isolated_test_state_root();
            assert!(config_path.starts_with(root), "{}", config_path.display());
            assert!(
                settings_path.starts_with(root),
                "{}",
                settings_path.display()
            );
            assert!(
                direct_config_path.starts_with(root),
                "{}",
                direct_config_path.display()
            );
            let receipt = std::env::var_os(RECEIPT_ENV).expect("receipt path");
            std::fs::write(
                receipt,
                format!(
                    "{}\n{}\n{}\n{}\n",
                    root.display(),
                    config_path.display(),
                    settings_path.display(),
                    direct_config_path.display()
                ),
            )
            .expect("write isolation receipt");
            return;
        }

        let sentinel = tempfile::tempdir().expect("sentinel home");
        let user_state = sentinel.path().join(".codewhale");
        std::fs::create_dir_all(&user_state).expect("create sentinel state");
        let config_path = user_state.join("config.toml");
        let settings_path = user_state.join("settings.toml");
        let config_sentinel = b"# developer config sentinel\n";
        let settings_sentinel = b"# developer settings sentinel\n";
        std::fs::write(&config_path, config_sentinel).expect("seed config");
        std::fs::write(&settings_path, settings_sentinel).expect("seed settings");
        let receipt_path = sentinel.path().join("receipt.txt");

        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .arg("--exact")
            .arg("test_support::tests::unguarded_state_writes_use_isolated_test_root")
            .arg("--test-threads=1")
            .env(PROBE_ENV, "1")
            .env(RECEIPT_ENV, &receipt_path)
            .env("HOME", sentinel.path())
            .env("USERPROFILE", sentinel.path())
            .env_remove("CODEWHALE_HOME")
            .env_remove("CODEWHALE_CONFIG_PATH")
            .env_remove("DEEPSEEK_CONFIG_PATH")
            .output()
            .expect("run isolated-state probe");
        assert!(
            output.status.success(),
            "probe failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        assert_eq!(
            std::fs::read(&config_path).expect("read config sentinel"),
            config_sentinel
        );
        assert_eq!(
            std::fs::read(&settings_path).expect("read settings sentinel"),
            settings_sentinel
        );

        let receipt = std::fs::read_to_string(&receipt_path).expect("read isolation receipt");
        let mut paths = receipt.lines().map(PathBuf::from);
        let isolated_root = paths.next().expect("root receipt");
        let written_config = paths.next().expect("config receipt");
        let written_settings = paths.next().expect("settings receipt");
        let direct_config = paths.next().expect("direct config receipt");
        assert!(!isolated_root.starts_with(sentinel.path()));
        assert!(written_config.starts_with(&isolated_root));
        assert!(written_settings.starts_with(&isolated_root));
        assert!(direct_config.starts_with(&isolated_root));
        assert!(written_config.exists());
        assert!(written_settings.exists());
    }

    #[test]
    fn config_path_read_waits_for_foreign_env_redirect_to_restore() {
        let (started_tx, started_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel();
        let redirected = std::env::temp_dir().join(format!(
            "codewhale-config-path-read-barrier-{}",
            std::process::id()
        ));

        let reader = {
            let lock = lock_test_env();
            let redirect = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &redirected);
            let reader = std::thread::spawn(move || {
                started_tx.send(()).expect("signal config path read start");
                tx.send(codewhale_runtime::config_persistence::config_toml_path(
                    None,
                ))
                .expect("send resolved config path");
            });

            started_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("reader reached config path resolution");
            assert!(
                rx.recv_timeout(Duration::from_millis(50)).is_err(),
                "a foreign reader observed the temporary config redirect"
            );
            drop(redirect);
            drop(lock);
            reader
        };

        let resolved = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("reader resumed after the redirect was restored")
            .expect("resolve config path");
        reader.join().expect("reader thread");
        assert_ne!(resolved, redirected);
    }

    #[test]
    fn settings_save_waits_for_foreign_state_io_transaction() {
        let (holder_ready_tx, holder_ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let holder = std::thread::spawn(move || {
            with_test_state_io_lock(|| {
                holder_ready_tx.send(()).expect("signal state lock held");
                release_rx.recv().expect("release state lock");
            });
        });
        holder_ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("holder acquired state I/O lock");

        let (started_tx, started_rx) = mpsc::channel();
        let (saved_tx, saved_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            started_tx.send(()).expect("signal settings save start");
            saved_tx
                .send(codewhale_runtime::settings::Settings::default().save())
                .expect("send settings save result");
        });
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("writer reached settings save");
        assert!(
            saved_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "settings save did not wait for an in-flight state transaction"
        );

        release_tx.send(()).expect("release holder");
        holder.join().expect("holder thread");
        saved_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("settings save resumed")
            .expect("settings save succeeded");
        writer.join().expect("writer thread");
    }
}
