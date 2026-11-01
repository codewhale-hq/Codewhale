

    #[test]
    fn stub_retention_removes_legacy_named_stub_instead_of_warning_forever() {
        // Founder run 2026-09-28: `session_<timestamp>.json` stubs from early
        // builds carry a uuid id, so removal by id hit NotFound and the same
        // two stubs were re-listed and warned about on every launch.
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let legacy_id = Uuid::new_v4().to_string();
        write_empty_session_record(
            &manager,
            &legacy_id,
            Path::new("/tmp"),
            Utc::now() - chrono::Duration::days(120),
        );
        let legacy_path = manager.sessions_dir.join("session_20260526_133732.json");
        fs::rename(
            manager.validated_session_path(&legacy_id).expect("path"),
            &legacy_path,
        )
        .expect("rename to legacy name");
        for index in 0..MAX_EMPTY_SESSION_STUBS {
            write_empty_session_record(
                &manager,
                &Uuid::new_v4().to_string(),
                Path::new("/tmp"),
                Utc::now() - chrono::Duration::minutes(index as i64),
            );
        }
        // Every save runs retention, so the newer stubs above already pushed
        // the legacy one past the stub cap; run it once more explicitly.
        manager.cleanup_old_sessions().expect("retention");

        assert!(
            !legacy_path.exists(),
            "retention retires the file it listed, not `<id>.json`"
        );
        let listed = manager.list_sessions().expect("sessions");
        assert_eq!(listed.len(), MAX_EMPTY_SESSION_STUBS);
        assert!(listed.iter().all(|session| session.id != legacy_id));
    }

    #[test]
    fn stub_retention_treats_an_already_removed_stub_as_removed() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let id = Uuid::new_v4().to_string();
        let canonical = manager.validated_session_path(&id).expect("path");
        manager
            .retire_empty_stub(&canonical, &id)
            .expect("a stub gone before removal ran is already removed");
        manager
            .retire_empty_stub(&manager.sessions_dir.join("session_gone.json"), &id)
            .expect("a legacy stub gone before removal ran is already removed");
    }

    #[test]
    fn prune_sessions_older_than_returns_zero_for_empty_dir() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let pruned = manager
            .prune_sessions_older_than(std::time::Duration::from_secs(3600))
            .expect("prune");
        assert_eq!(pruned, 0);
    }

    #[test]
    fn prune_sessions_older_than_keeps_fresh_records() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        // All updated within the last hour.
        write_session_with_updated_at(
            &manager,
            "fresh-1",
            Utc::now() - chrono::Duration::minutes(30),
        );
        write_session_with_updated_at(
            &manager,
            "fresh-2",
            Utc::now() - chrono::Duration::minutes(5),
        );
        let pruned = manager
            .prune_sessions_older_than(std::time::Duration::from_secs(3600))
            .expect("prune");
        assert_eq!(pruned, 0);
        // Both files still on disk.
        assert_eq!(manager.list_sessions().expect("list").len(), 2);
    }

    #[test]
    fn prune_sessions_older_than_huge_max_age_keeps_everything() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        write_session_with_updated_at(&manager, "old", Utc::now() - chrono::Duration::days(3650));
        write_session_with_updated_at(&manager, "new", Utc::now());
        // Both overflow paths: from_std rejects u64::MAX seconds, and a
        // representable-but-enormous age underflows the DateTime subtraction.
        for max_age in [
            std::time::Duration::MAX,
            std::time::Duration::from_secs(i64::MAX as u64 / 1_000),
        ] {
            let pruned = manager
                .prune_sessions_older_than(max_age)
                .expect("huge max_age must not error or panic");
            assert_eq!(pruned, 0, "{max_age:?}");
            assert_eq!(manager.list_sessions().expect("list").len(), 2);
        }
    }

    #[test]
    fn prune_sessions_older_than_removes_stale_records() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        // Two stale records ≥7 days old.
        write_session_with_updated_at(&manager, "stale-1", Utc::now() - chrono::Duration::days(8));
        write_session_with_updated_at(&manager, "stale-2", Utc::now() - chrono::Duration::days(30));
        let pruned = manager
            .prune_sessions_older_than(std::time::Duration::from_secs(7 * 24 * 3600))
            .expect("prune");
        assert_eq!(pruned, 2);
        assert_eq!(manager.list_sessions().expect("list").len(), 0);
    }

    #[test]
    fn prune_sessions_older_than_only_removes_stale_records_in_mixed_dir() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        write_session_with_updated_at(&manager, "fresh", Utc::now() - chrono::Duration::hours(1));
        write_session_with_updated_at(&manager, "stale", Utc::now() - chrono::Duration::days(60));
        let pruned = manager
            .prune_sessions_older_than(std::time::Duration::from_secs(7 * 24 * 3600))
            .expect("prune");
        assert_eq!(pruned, 1);
        let remaining = manager.list_sessions().expect("list");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "fresh");
    }

    #[test]
    fn prune_sessions_older_than_skips_checkpoint_directory() {
        // The checkpoint subsystem owns `<sessions>/checkpoints/` —
        // prune must not walk into it. The list_sessions iterator
        // already filters to top-level `*.json` files (skipping
        // sub-directories), so this test pins that behaviour.
        let tmp = tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("new");
        let checkpoint_dir = sessions_dir.join("checkpoints");
        fs::create_dir_all(&checkpoint_dir).expect("mkdir checkpoints");
        // Drop a legacy checkpoint inside the checkpoint dir; prune should
        // leave it alone. It belongs to a readable, unrelated origin: an
        // unreadable origin makes retention fail closed instead (see
        // `retention_fails_closed_on_an_unreadable_legacy_checkpoint_origin`).
        let checkpoint_file = checkpoint_dir.join(LEGACY_CHECKPOINT_FILE);
        let unrelated = save_late_usage_test_session(&manager, "unrelated-origin");
        write_atomic(
            &checkpoint_file,
            serialize_saved_session(unrelated)
                .expect("legacy bytes")
                .as_bytes(),
        )
        .expect("write checkpoint");

        write_session_with_updated_at(&manager, "stale", Utc::now() - chrono::Duration::days(60));
        let pruned = manager
            .prune_sessions_older_than(std::time::Duration::from_secs(7 * 24 * 3600))
            .expect("prune");
        assert_eq!(pruned, 1, "the top-level stale session should be removed");
        assert!(
            checkpoint_file.exists(),
            "checkpoint file should be untouched"
        );
    }

    #[test]
    fn test_load_offline_queue_rejects_newer_schema() {
        let tmp = tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("new");
        let checkpoints = sessions_dir.join("checkpoints");
        fs::create_dir_all(&checkpoints).expect("create checkpoints dir");
        let path = checkpoints.join("session-A.offline_queue.json");
        fs::write(
            &path,
            r#"{
                "schema_version": 999,
                "messages": [],
                "draft": null
            }"#,
        )
        .expect("write queue");

        let err = manager
            .load_offline_queue_state("session-A")
            .expect_err("should reject schema");
        assert!(
            err.to_string().contains("newer than supported"),
            "unexpected error: {err}"
        );

        // An unreadable *legacy* global queue is somebody else's problem to
        // recover: it must not fail this session's boot, and must survive.
        let legacy = checkpoints.join("offline_queue.json");
        fs::write(&legacy, r#"{"schema_version": 999}"#).expect("write legacy queue");
        assert!(
            manager
                .load_offline_queue_state("session-B")
                .expect("legacy corruption must not fail the boot")
                .is_none()
        );
        assert!(legacy.exists(), "unreadable legacy queue is left in place");
    }
    #[cfg(all(unix, not(target_os = "solaris")))]
    #[test]
    fn offline_queue_lease_releases_while_an_inherited_descriptor_remains_open() {
        let directory = tempfile::tempdir().expect("queue fixture");
        let manager = SessionManager::new(directory.path().join("sessions")).expect("manager");
        let editor = manager
            .acquire_offline_queue_lease("shared-session")
            .expect("first editor");
        // dup and fork share the same open-file description. Keep it alive
        // without a timing race or forking the multithreaded test process.
        let inherited = editor._file.try_clone().expect("inherited descriptor");
        let pending_write = std::sync::Arc::clone(&editor);
        drop(editor);
        assert_eq!(
            manager
                .acquire_offline_queue_lease("shared-session")
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock,
            "pending writes retain the exclusive editor lease"
        );
        drop(pending_write);
        let next_editor = manager
            .acquire_offline_queue_lease("shared-session")
            .expect("completed editor releases even while a child retains its descriptor");
        drop(inherited);
        assert_eq!(
            manager
                .acquire_offline_queue_lease("shared-session")
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock,
            "closing the old descriptor must not release the next editor's lock"
        );
        drop(next_editor);
        assert!(
            manager
                .acquire_offline_queue_lease("shared-session")
                .is_ok()
        );
    }

    #[test]
    fn offline_queue_lease_excludes_another_process_and_releases() {
        const PROBE: &str = "CODEWHALE_QUEUE_LEASE_PROBE_DIR";
        const HELD: &str = "CODEWHALE_QUEUE_LEASE_PROBE_HELD";
        if let Some(directory) = std::env::var_os(PROBE) {
            let manager = SessionManager::new(PathBuf::from(directory)).expect("child store");
            let result = manager.acquire_offline_queue_lease("shared-session");
            if std::env::var(HELD).as_deref() == Ok("1") {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
            } else {
                assert!(result.is_ok(), "closed owner must release its kernel lock");
            }
            return;
        }
        let directory = tempfile::tempdir().expect("queue fixture");
        let sessions = directory.path().join("sessions");
        let manager = SessionManager::new(sessions.clone()).expect("parent store");
        let lease = manager
            .acquire_offline_queue_lease("shared-session")
            .expect("first editor");
        let probe = |held: bool| {
            let output = std::process::Command::new(
                std::env::current_exe().expect("test executable"),
            )
            .args([
                "--exact",
                "session_manager::tests::offline_queue_lease_excludes_another_process_and_releases",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PROBE, &sessions)
            .env(HELD, if held { "1" } else { "0" })
            .output()
            .expect("second editor process");
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        };
        probe(true);
        let _different_session = manager
            .acquire_offline_queue_lease("different-session")
            .expect("unrelated queue is available");
        drop(lease);
        probe(false);
        for invalid in ["", "../session", "nested/session"] {
            assert_eq!(
                manager
                    .acquire_offline_queue_lease(invalid)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn whitespace_home_override_is_consistent_across_tui_state_entry_points() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().expect("temporary root");
        let home = tmp.path().join("home");
        let userprofile = tmp.path().join("userprofile");
        let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
        let _userprofile = crate::test_support::EnvVarGuard::set("USERPROFILE", &userprofile);
        let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", " \t ");
        let _config_path = crate::test_support::EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
        let _legacy_config_path = crate::test_support::EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
        let primary = home.join(".codewhale");

        assert_eq!(codewhale_paths::user_home(), Some(home.clone()));
        assert_eq!(
            codewhale_runtime::config::workspace_trust_config_candidate_paths(),
            vec![
                primary.join("config.toml"),
                home.join(".deepseek").join("config.toml")
            ]
        );
        assert_eq!(
            codewhale_runtime::runtime_log::log_directory(),
            Some(primary.join("logs"))
        );
        assert_eq!(
            crate::automation_manager::default_automations_dir(),
            primary.join("automations")
        );
        assert_eq!(
            default_sessions_dir().expect("session directory"),
            primary.join("sessions")
        );
    }
