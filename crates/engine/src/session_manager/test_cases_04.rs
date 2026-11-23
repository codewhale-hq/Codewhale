

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

    #[test]
    fn saved_session_without_agent_preset_loads_and_round_trips_the_mode() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let session = create_saved_session(&[], "test-model", tmp.path(), 0, None);
        let mut raw = serde_json::to_value(&session).expect("serialize");
        assert!(raw.get("agent_preset").is_none());
        let legacy: SavedSession = serde_json::from_value(raw.clone()).expect("legacy load");
        assert_eq!(legacy.agent_preset, None);
        raw["agent_preset"] = serde_json::json!("creator");
        let saved: SavedSession = serde_json::from_value(raw).expect("mode load");
        manager.save_session(&saved).expect("save");
        let loaded = manager.load_session(&saved.metadata.id).expect("load");
        assert_eq!(loaded.agent_preset.as_deref(), Some("creator"));
    }

    #[test]
    fn a_hand_edited_agent_preset_still_loads_and_never_becomes_standard() {
        let session = create_saved_session(&[], "test-model", std::path::Path::new("."), 0, None);
        let base = serde_json::to_value(&session).expect("serialize");
        let big = "x".repeat(1_000_000);
        for (edited, expected) in [
            (serde_json::Value::Null, None),
            (serde_json::json!("creator"), Some("creator".to_string())),
            (serde_json::json!(""), Some(String::new())),
            (serde_json::json!("../x"), Some("../x".to_string())),
            (serde_json::json!(5), Some("5".to_string())),
            (serde_json::json!(true), Some("true".to_string())),
            (serde_json::json!({"a": 1}), Some("{\"a\":1}".to_string())),
            (serde_json::json!(big.clone()), Some(big)),
        ] {
            let mut raw = base.clone();
            raw["agent_preset"] = edited;
            let loaded: SavedSession = serde_json::from_value(raw).expect("loads");
            assert_eq!(loaded.agent_preset, expected);
        }
    }

    #[test]
    fn migrate_workspace_updates_session_checkpoint_and_runtime_thread() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let old_dir = tmp.path().join("Project_A");
        let new_dir = tmp.path().join("Project_B");
        std::fs::create_dir_all(&old_dir).expect("mkdir old");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");

        let mut session = create_saved_session(&[], "test-model", &old_dir, 0, None);
        let id = session.metadata.id.clone();
        let store_dir = sessions_dir.join(&id).join("runtime");
        let store = crate::runtime_threads::RuntimeThreadStore::open(store_dir.clone())
            .expect("create runtime store");
        let thread = migration_test_thread("thr_test_1", &id, &old_dir);
        let binding = crate::runtime_threads::RuntimeStoreBinding::for_store_dir(&store_dir)
            .expect("binding");
        session.metadata.runtime_store = Some(binding);
        manager.save_session(&session).expect("save session");
        manager.save_checkpoint(&session).expect("save checkpoint");
        std::fs::copy(
            manager.checkpoints_dir().join(format!("{id}.json")),
            manager.checkpoints_dir().join("latest.json"),
        )
        .expect("copy legacy checkpoint");
        let initial_obs = manager
            .with_session_write_admission(&id, || manager.observe_session_admitted(&id))
            .expect("observe pre")
            .expect("pre obs");
        let mut thread = thread;
        thread.saved_session_checkpoint = Some(crate::runtime_threads::SavedSessionCheckpoint {
            publication_observation: Some(initial_obs),
            covered_turn_id: None,
            messages_sha256: "abc".to_string(),
            messages_len: Some(0),
            retained_messages: None,
        });
        store.save_thread(&thread).expect("save thread");

        std::fs::rename(&old_dir, &new_dir).expect("rename workspace");
        assert!(
            manager
                .get_latest_session_for_workspace(&new_dir)
                .expect("query")
                .is_none()
        );

        let before_dry = migration_test_tree(&sessions_dir);
        let dry = manager
            .migrate_workspace(&WorkspaceMigrationOptions {
                old_path: old_dir.clone(),
                new_path: new_dir.clone(),
                session_id: None,
                dry_run: true,
            })
            .expect("dry run");
        assert_eq!(migration_test_tree(&sessions_dir), before_dry);
        assert_eq!(dry.migrated_sessions, vec![id.clone()]);
        assert_eq!(dry.migrated_checkpoints, vec![id.clone()]);
        assert_eq!(dry.migrated_threads, vec!["thr_test_1".to_string()]);
        assert!(
            !store_dir
                .join(crate::runtime_threads::RUNTIME_PROCESS_OWNER_LOCK_FILE)
                .exists()
        );
        assert_eq!(
            manager.load_session(&id).expect("still old").metadata.workspace,
            old_dir
        );

        let applied = manager
            .migrate_workspace(&WorkspaceMigrationOptions {
                old_path: old_dir.clone(),
                new_path: new_dir.clone(),
                session_id: None,
                dry_run: false,
            })
            .expect("apply migration");
        assert_eq!(applied.migrated_sessions, vec![id.clone()]);
        assert_eq!(applied.migrated_checkpoints, vec![id.clone()]);
        assert_eq!(applied.migrated_threads, vec!["thr_test_1".to_string()]);

        let canonical_new = std::fs::canonicalize(&new_dir).expect("canonical new");
        assert_eq!(
            manager.load_session(&id).expect("updated").metadata.workspace,
            canonical_new
        );
        assert_eq!(
            manager
                .load_recovery_checkpoint(&id)
                .expect("ckpt")
                .expect("present")
                .metadata
                .workspace,
            canonical_new
        );
        assert_eq!(
            manager
                .load_legacy_checkpoint()
                .expect("legacy ckpt")
                .expect("legacy present")
                .metadata
                .workspace,
            canonical_new
        );
        let expected_post_obs = manager
            .with_session_write_admission(&id, || manager.observe_session_admitted(&id))
            .expect("observe post")
            .expect("post obs");
        let reloaded_thread = store.load_thread("thr_test_1").expect("thread");
        assert_eq!(reloaded_thread.workspace, canonical_new);
        assert_eq!(
            reloaded_thread
                .saved_session_checkpoint
                .as_ref()
                .and_then(|cp| cp.publication_observation.as_ref()),
            Some(&expected_post_obs)
        );

        let live_lease = manager.reserve_session_for_external_write(&id).expect("lease");
        let busy = manager
            .migrate_workspace(&WorkspaceMigrationOptions {
                old_path: new_dir.clone(),
                new_path: tmp.path().join("Project_C"),
                session_id: None,
                dry_run: true,
            })
            .expect("busy skipped");
        assert_eq!(busy.busy_sessions, vec![id]);
        drop(live_lease);
    }

    #[cfg(unix)]
    #[test]
    fn migrate_workspace_heals_half_edited_session_json_and_matches_symlinks() {
        let tmp = tempdir().expect("tempdir");
        let old_dir = tmp.path().join("Project_A");
        let old_sub = old_dir.join("apps").join("api");
        let new_dir = tmp.path().join("Project_B");
        let new_sub = new_dir.join("apps").join("api");
        std::fs::create_dir_all(&old_sub).expect("mkdir old");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");

        let mut session = create_saved_session(&[], "test-model", &old_sub, 0, None);
        let id = session.metadata.id.clone();
        let store_dir = sessions_dir.join(&id).join("runtime");
        let store = crate::runtime_threads::RuntimeThreadStore::open(store_dir.clone())
            .expect("create runtime store");
        let mut thread = migration_test_thread("thr_half_1", &id, &old_sub);
        let binding = crate::runtime_threads::RuntimeStoreBinding::for_store_dir(&store_dir)
            .expect("binding");
        session.metadata.runtime_store = Some(binding);
        std::fs::rename(&old_dir, &new_dir).expect("rename workspace");
        session.metadata.workspace = new_sub.clone();
        manager.save_session(&session).expect("save half-edited session");

        let initial_obs = manager
            .with_session_write_admission(&id, || manager.observe_session_admitted(&id))
            .expect("observe pre")
            .expect("pre obs");
        thread.saved_session_checkpoint = Some(crate::runtime_threads::SavedSessionCheckpoint {
            publication_observation: Some(initial_obs.clone()),
            covered_turn_id: None,
            messages_sha256: "abc".to_string(),
            messages_len: Some(0),
            retained_messages: None,
        });
        store.save_thread(&thread).expect("save thread");

        let applied = manager
            .migrate_workspace(&WorkspaceMigrationOptions {
                old_path: old_dir.clone(),
                new_path: new_dir.clone(),
                session_id: None,
                dry_run: false,
            })
            .expect("heal thread without CODEWHALE_HOME override");
        assert!(applied.unreadable.is_empty(), "{:?}", applied.unreadable);
        assert!(applied.migrated_sessions.is_empty());
        assert_eq!(applied.migrated_threads, vec!["thr_half_1".to_string()]);
        let canonical_new_sub = std::fs::canonicalize(&new_sub).expect("canonical new sub");
        let healed_thread = store.load_thread("thr_half_1").expect("thread");
        assert_eq!(healed_thread.workspace, canonical_new_sub);
        let healed_obs = healed_thread
            .saved_session_checkpoint
            .as_ref()
            .and_then(|cp| cp.publication_observation.as_ref())
            .expect("healed publication observation");
        assert_eq!(healed_obs, &initial_obs);

        let symlink_path = tmp.path().join("Project_Link");
        std::os::unix::fs::symlink(&new_dir, &symlink_path).expect("symlink");
        assert!(paths_equivalent(&symlink_path, &new_dir));

        let real_parent = tmp.path().join("real_parent");
        let real_sub = real_parent.join("sub");
        std::fs::create_dir_all(&real_sub).expect("mkdir real_sub");
        let sym_sub = tmp.path().join("sym_sub");
        std::os::unix::fs::symlink(&real_sub, &sym_sub).expect("sym_sub");
        assert!(!paths_equivalent(
            &sym_sub.join("..").join("missing"),
            &tmp.path().join("missing")
        ));
        assert!(paths_equivalent(
            &sym_sub.join("..").join("missing"),
            &real_parent.join("missing")
        ));

        let nested_err = manager
            .migrate_workspace(&WorkspaceMigrationOptions {
                old_path: new_dir.clone(),
                new_path: new_sub,
                session_id: None,
                dry_run: true,
            })
            .expect_err("nested paths must be rejected");
        assert_eq!(nested_err.kind(), std::io::ErrorKind::InvalidInput);
    }

    fn migration_test_thread(
        thread_id: &str,
        session_id: &str,
        workspace: &Path,
    ) -> crate::runtime_threads::ThreadRecord {
        crate::runtime_threads::ThreadRecord {
            schema_version: 2,
            id: thread_id.to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            model: "test-model".to_string(),
            model_provider: Some("deepseek".to_string()),
            model_provider_id: None,
            reasoning_effort: None,
            allowed_tools: None,
            disallowed_tools: Vec::new(),
            add_dirs: Vec::new(),
            workspace: workspace.to_path_buf(),
            mode: "agent".to_string(),
            permission_posture: None,
            allow_shell: false,
            trust_mode: false,
            auto_approve: false,
            latest_turn_id: None,
            latest_response_bookmark: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            parent_thread_id: None,
            title: None,
            session_id: Some(session_id.to_string()),
            saved_session_checkpoint: None,
            last_read: None,
            executor: None,
            environment: None,
            agent_preset: None,
            whale_id: None,
        }
    }

    fn migration_test_tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut entries = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).expect("read dir").flatten() {
                let path = entry.path();
                let meta = fs::symlink_metadata(&path).expect("metadata");
                if meta.is_dir() {
                    stack.push(path.clone());
                    entries.push((path, Vec::new()));
                } else if meta.file_type().is_symlink() {
                    let target = fs::read_link(&path).expect("read link");
                    entries.push((path, target.into_os_string().into_encoded_bytes()));
                } else {
                    entries.push((path.clone(), fs::read(&path).expect("read file")));
                }
            }
        }
        entries.sort();
        entries
    }

    #[cfg(unix)]
    #[test]
    fn migrate_workspace_refuses_symlinked_tasks_runtime_and_leaves_target_untouched() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let old_dir = tmp.path().join("Project_A");
        let new_dir = tmp.path().join("Project_B");
        fs::create_dir_all(&new_dir).expect("mkdir new");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let session = create_saved_session(&[], "test-model", &old_dir, 0, None);
        let id = session.metadata.id.clone();
        manager.save_session(&session).expect("save session");

        let victim_dir = tmp.path().join("victim");
        fs::create_dir_all(victim_dir.join("threads")).expect("mkdir victim threads");
        let victim_thread = migration_test_thread("thr_victim", &id, &old_dir);
        fs::write(
            victim_dir.join("threads").join("thr_victim.json"),
            serde_json::to_vec(&victim_thread).expect("thread json"),
        )
        .expect("write victim thread");
        let tasks_dir = tmp.path().join("tasks");
        fs::create_dir_all(&tasks_dir).expect("mkdir tasks");
        std::os::unix::fs::symlink(&victim_dir, tasks_dir.join("runtime")).expect("sym tasks");
        let victim_before = migration_test_tree(&victim_dir);

        for dry_run in [true, false] {
            let report = manager
                .migrate_workspace(&WorkspaceMigrationOptions {
                    old_path: old_dir.clone(),
                    new_path: new_dir.clone(),
                    session_id: None,
                    dry_run,
                })
                .expect("migration reports instead of failing");
            assert!(report.migrated_sessions.is_empty(), "{report:?}");
            assert!(report.migrated_threads.is_empty(), "{report:?}");
            assert!(
                report
                    .unreadable
                    .iter()
                    .any(|warning| warning.contains("runtime store")),
                "{report:?}"
            );
            assert_eq!(migration_test_tree(&victim_dir), victim_before);
        }
        assert_eq!(
            manager.load_session(&id).expect("session").metadata.workspace,
            old_dir
        );

        for data_dir in [tasks_dir.join("runtime"), fs::canonicalize(&victim_dir).expect("canon")] {
            let binding = crate::runtime_threads::RuntimeStoreBinding {
                data_dir,
                execution_scope: String::new(),
            };
            assert!(binding.try_hold_for_sessions_dir(&sessions_dir).is_err());
        }
        assert_eq!(migration_test_tree(&victim_dir), victim_before);

        let peer_store = sessions_dir.join("peer_session").join("runtime");
        fs::create_dir_all(peer_store.join("threads")).expect("mkdir peer threads");
        fs::write(
            peer_store.join("threads").join("thr_peer.json"),
            serde_json::to_vec(&victim_thread).expect("peer thread json"),
        )
        .expect("write peer thread");
        let peer_before = migration_test_tree(&peer_store);
        fs::remove_file(tasks_dir.join("runtime")).expect("rm old sym");
        std::os::unix::fs::symlink(&peer_store, tasks_dir.join("runtime")).expect("sym peer");
        for dry_run in [true, false] {
            let report = manager
                .migrate_workspace(&WorkspaceMigrationOptions {
                    old_path: old_dir.clone(),
                    new_path: new_dir.clone(),
                    session_id: None,
                    dry_run,
                })
                .expect("intra-root symlink reports instead of failing");
            assert!(report.migrated_sessions.is_empty(), "{report:?}");
            assert!(report.migrated_threads.is_empty(), "{report:?}");
            assert!(
                report
                    .unreadable
                    .iter()
                    .any(|warning| warning.contains("runtime store")),
                "{report:?}"
            );
            assert_eq!(migration_test_tree(&peer_store), peer_before);
        }
    }

    #[test]
    fn migrate_workspace_leaves_stale_publication_observation_for_reopen() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let old_dir = tmp.path().join("Project_A");
        let new_dir = tmp.path().join("Project_B");
        fs::create_dir_all(&old_dir).expect("mkdir old");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");

        let mut session = create_saved_session(&[], "test-model", &old_dir, 0, None);
        let id = session.metadata.id.clone();
        let store_dir = sessions_dir.join(&id).join("runtime");
        let store = crate::runtime_threads::RuntimeThreadStore::open(store_dir.clone())
            .expect("create runtime store");
        session.metadata.runtime_store = Some(
            crate::runtime_threads::RuntimeStoreBinding::for_store_dir(&store_dir)
                .expect("binding"),
        );
        manager.save_session(&session).expect("save session");
        let stale_obs = manager
            .with_session_write_admission(&id, || manager.observe_session_admitted(&id))
            .expect("observe")
            .expect("obs");
        let mut thread = migration_test_thread("thr_stale_1", &id, &old_dir);
        thread.saved_session_checkpoint = Some(crate::runtime_threads::SavedSessionCheckpoint {
            publication_observation: Some(stale_obs.clone()),
            covered_turn_id: None,
            messages_sha256: "abc".to_string(),
            messages_len: Some(0),
            retained_messages: None,
        });
        store.save_thread(&thread).expect("save thread");

        fs::rename(&old_dir, &new_dir).expect("rename workspace");
        session.metadata.workspace = fs::canonicalize(&new_dir).expect("canonical new");
        manager.save_session(&session).expect("simulate interrupted migration");

        let applied = manager
            .migrate_workspace(&WorkspaceMigrationOptions {
                old_path: old_dir.clone(),
                new_path: new_dir.clone(),
                session_id: None,
                dry_run: false,
            })
            .expect("re-run migration");
        assert_eq!(applied.migrated_threads, vec!["thr_stale_1".to_string()]);
        assert!(
            applied
                .unreadable
                .iter()
                .any(|warning| warning.contains("thr_stale_1") && warning.contains("reopen")),
            "{:?}",
            applied.unreadable
        );
        let reloaded = store.load_thread("thr_stale_1").expect("thread");
        assert_eq!(
            reloaded.workspace,
            fs::canonicalize(&new_dir).expect("canonical new")
        );
        assert_eq!(
            reloaded
                .saved_session_checkpoint
                .as_ref()
                .and_then(|cp| cp.publication_observation.as_ref()),
            Some(&stale_obs)
        );
    }
