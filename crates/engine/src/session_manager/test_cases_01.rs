
    use super::*;
    use crate::approval_log::ApprovalOutcome;
    use codewhale_models::ContentBlock;
    use codewhale_models::Role;
    use std::fs;
    use tempfile::tempdir;

    fn make_test_message(role: &str, text: &str) -> Message {
        Message {
            role: Role::from(role),
            content: vec![codewhale_models::ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    /// The journal is the session's timeline: an entry's `created_at` is when
    /// the message landed, not when a save ran. A rebuilt journal must not
    /// collapse 90 minutes of appends into the save instant — an inspector
    /// reading the file needs "this loop is 12 seconds" to be true.
    #[test]
    fn journal_entries_keep_append_stamps_across_saves() {
        let tmp = tempdir().expect("tempdir");
        let messages = vec![
            make_test_message("user", "first"),
            make_test_message("assistant", "answer"),
        ];
        let t0 = Utc::now() - chrono::Duration::minutes(90);
        let t1 = t0 + chrono::Duration::seconds(12);
        let session = create_saved_session_with_id_mode_and_stamps(
            "stamped".to_string(),
            &messages,
            &[t0, t1],
            "deepseek-v4-flash",
            tmp.path(),
            0,
            None,
            None,
        );
        let journal = session.journal.as_ref().expect("journal");
        assert_eq!(journal.entries[0].created_at, t0);
        assert_eq!(journal.entries[1].created_at, t1);
        assert_ne!(
            journal.entries[0].created_at, session.metadata.updated_at,
            "an append 90 minutes before save must not read as save time"
        );
        // Resume hands the same stamps back to the live log.
        assert_eq!(session.journal_message_stamps(), vec![t0, t1]);
        // A save with no stamps keeps the old behavior: entries collapse to
        // save time rather than inventing times.
        let before_save = Utc::now();
        let unstamped = create_saved_session_with_id_and_mode(
            "unstamped".to_string(),
            &messages,
            "deepseek-v4-flash",
            tmp.path(),
            0,
            None,
            None,
        );
        let journal = unstamped.journal.as_ref().expect("journal");
        assert!(
            journal.entries.iter().all(|entry| {
                entry.created_at >= before_save && entry.created_at <= unstamped.metadata.created_at
            }),
            "unstamped entries are created during save, before snapshot metadata"
        );
    }

    fn save_late_usage_test_session(manager: &SessionManager, id: &str) -> SavedSession {
        let session = create_saved_session_with_id_and_mode(
            id.to_string(),
            &[make_test_message("user", "recoverable transcript")],
            "deepseek-v4-flash",
            manager.sessions_dir(),
            0,
            None,
            Some("agent"),
        );
        manager.save_session(&session).expect("save session");
        session
    }

    fn late_usage_test_record(source_id: &str) -> crate::cost_status::RuntimeUsageRecord {
        crate::cost_status::RuntimeUsageRecord {
            source_id: source_id.to_string(),
            usage: crate::cost_status::EffectiveRouteUsage {
                route: crate::cost_status::EffectiveRouteEnvelope::capture(
                    None,
                    ProviderKind::Deepseek,
                    "deepseek",
                    "deepseek-v4-flash",
                    Some(codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL),
                    Utc::now(),
                ),
                usage: codewhale_models::Usage {
                    input_tokens: 1,
                    ..Default::default()
                },
            },
        }
    }

    #[test]
    fn decision_receipt_survives_restart_replay_and_session_deletion() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        save_late_usage_test_session(&manager, "decision-origin");
        let receipt = crate::cost_status::decision_receipt_fixture("raw-decision-response-id");
        manager
            .persist_late_decision_receipt("decision-origin", "origin-turn", &receipt)
            .expect("decision append");
        manager
            .persist_late_runtime_usage(
                "decision-origin",
                "origin-turn",
                &crate::cost_status::RuntimeUsageRecord {
                    source_id: receipt.source_id.clone(),
                    usage: crate::cost_status::EffectiveRouteUsage {
                        route: receipt.route.clone(),
                        usage: receipt.usage.clone().expect("usage"),
                    },
                },
            )
            .expect("token append dedupes");
        drop(manager);
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("restart");
        let retained = manager
            .decision_receipts_for_session("decision-origin")
            .expect("receipts");
        assert_eq!(retained, vec![receipt.sanitized()]);
        let mut restored = manager
            .load_session_snapshot("decision-origin")
            .expect("restore");
        assert_eq!(restored.metadata.total_tokens, 13);
        assert_eq!(
            restored.metadata.cost.unpriced_turns, 1,
            "unknown TypeSafe billing remains explicit"
        );
        assert!(
            restored
                .metadata
                .cost
                .route_receipts
                .iter()
                .any(|r| r.contains("0.000012054"))
        );
        manager.apply_late_usage_to_metadata(&mut restored.metadata);
        assert_eq!(
            restored.metadata.total_tokens, 13,
            "replay must not count tokens twice"
        );
        assert_eq!(restored.metadata.cost.unpriced_turns, 1);
        manager.save_session(&restored).expect("save overlay");
        assert_eq!(
            manager
                .load_session_snapshot("decision-origin")
                .expect("resume again")
                .metadata
                .total_tokens,
            13
        );
        let (ledger, _) = manager.late_usage_paths("decision-origin").expect("paths");
        assert!(
            !fs::read_to_string(&ledger)
                .expect("ledger")
                .contains("raw-decision-response-id")
        );
        save_late_usage_test_session(&manager, "decision-partial-origin");
        let mut partial = receipt.clone();
        partial.source_id = "partial-provider-response-id".into();
        partial.usage_complete = false;
        partial.usage.as_mut().expect("partial usage").output_tokens = 0;
        manager
            .persist_late_decision_receipt("decision-partial-origin", "partial-turn", &partial)
            .expect("partial diagnostic append");
        let partial_session = manager
            .load_session_snapshot("decision-partial-origin")
            .expect("partial replay");
        assert_eq!(
            partial_session.metadata.total_tokens, 0,
            "partial provider counters remain diagnostic rather than an authoritative subtotal"
        );
        assert_eq!(partial_session.metadata.cost.unpriced_turns, 1);
        assert!(
            partial_session
                .metadata
                .cost
                .route_receipts
                .iter()
                .any(|r| r.contains("0.000012054"))
        );
        manager.delete_session("decision-origin").expect("delete");
        assert!(
            manager
                .persist_late_decision_receipt("decision-origin", "origin-turn", &receipt)
                .expect("retired replay")
        );
        assert!(
            !ledger.exists(),
            "late evidence must never recreate a deleted origin"
        );
        let mut oversized = receipt;
        oversized.evidence.response_model = Some("x".repeat(129));
        assert!(
            manager
                .persist_late_decision_receipt("decision-origin", "origin-turn", &oversized)
                .is_err()
        );
    }

    #[test]
    fn late_usage_reads_do_not_create_accounting_storage() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let saved = save_late_usage_test_session(&manager, "no-late-usage");
        let directory = manager.sessions_dir().join(LATE_USAGE_DIR);
        let inventory = || {
            fs::read_dir(&directory)
                .expect("accounting directory")
                .map(|entry| entry.expect("entry").file_name())
                .collect::<BTreeSet<_>>()
        };
        let before = inventory();
        assert_eq!(before.len(), 1, "save creates the lifecycle lock only");
        assert_eq!(manager.list_sessions().expect("list").len(), 1);
        manager.load_session_by_prefix("no-late").expect("resume");
        manager
            .load_session_snapshot("no-late-usage")
            .expect("snapshot");
        assert_eq!(inventory(), before, "reads must not create sidecar files");

        // Imported snapshots predate lifecycle locks. Reading one must not
        // create either its missing accounting directory or a lock leaf.
        let imported = SessionManager::new(tmp.path().join("imported")).expect("imported store");
        write_atomic(
            &imported
                .validated_session_path(&saved.metadata.id)
                .expect("imported path"),
            serialize_saved_session(saved.clone())
                .expect("snapshot bytes")
                .as_bytes(),
        )
        .expect("import snapshot");
        let imported_directory = imported.sessions_dir().join(LATE_USAGE_DIR);
        imported.list_sessions().expect("imported list");
        imported
            .load_session_by_prefix("no-late")
            .expect("imported resume");
        assert!(
            !imported_directory.exists(),
            "reads must not create the sidecar directory"
        );

        fs::create_dir(&imported_directory).expect("empty accounting directory");
        imported
            .load_session_snapshot("no-late-usage")
            .expect("imported snapshot");
        assert_eq!(
            fs::read_dir(imported_directory).expect("directory").count(),
            0
        );
    }

    #[test]
    fn late_usage_projection_failure_preserves_recovery_and_is_idempotent() {
        for malformed in ["json", "oversized", "directory", "tombstone"] {
            let tmp = tempdir().expect("tempdir");
            let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
            let affected = save_late_usage_test_session(&manager, "affected-session");
            save_late_usage_test_session(&manager, "healthy-session");
            let (ledger, _) = manager
                .ensure_late_usage_paths("affected-session")
                .expect("paths");
            match malformed {
                "json" => fs::write(&ledger, b"{invalid accounting").expect("malformed ledger"),
                "oversized" => fs::File::create(&ledger)
                    .expect("file")
                    .set_len(MAX_LATE_USAGE_LEDGER_BYTES + 1)
                    .expect("oversized ledger"),
                "directory" => fs::create_dir(&ledger).expect("special ledger"),
                "tombstone" => fs::write(ledger.with_extension("deleted"), b"invalid marker")
                    .expect("malformed tombstone"),
                _ => unreachable!(),
            }

            let listed = manager
                .list_sessions()
                .expect("list survives sidecar failure");
            assert_eq!(listed.len(), 2);
            let bad = listed
                .iter()
                .find(|session| session.id == affected.metadata.id)
                .expect("affected");
            assert!(
                bad.cost
                    .unpriced_reasons
                    .contains(LATE_USAGE_UNAVAILABLE_REASON)
            );
            assert_eq!(bad.cost.unpriced_turns, 1);
            let good = manager
                .load_session_by_prefix("healthy")
                .expect("unaffected resume");
            assert_eq!(good.metadata.cost.unpriced_turns, 0);

            let mut restored = manager
                .load_session_by_prefix("affected")
                .expect("affected recovery");
            assert_eq!(restored.messages, affected.messages);
            manager.apply_late_usage_to_metadata(&mut restored.metadata);
            assert_eq!(restored.metadata.cost.unpriced_turns, 1);
            assert_eq!(restored.metadata.cost.cny_unpriced_turns, 1);
            assert_eq!(restored.metadata.cost.usage_source_fingerprints.len(), 1);
            if malformed == "tombstone" {
                assert!(
                    manager.save_session(&restored).is_err(),
                    "an invalid deletion marker must fail closed for writes"
                );
                fs::remove_file(ledger.with_extension("deleted"))
                    .expect("repair malformed deletion marker");
            }
            manager
                .save_session(&restored)
                .expect("save recovered transcript");
            let again = manager
                .load_session_snapshot("affected-session")
                .expect("repeat recovery");
            assert_eq!(again.metadata.cost.unpriced_turns, 1);
            assert_eq!(again.metadata.cost.cny_unpriced_turns, 1);
            assert_eq!(
                again.metadata.total_tokens, 0,
                "unsafe accounting must not be used"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn linked_late_usage_directory_does_not_block_transcripts_or_touch_target() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        save_late_usage_test_session(&manager, "linked-directory");
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).expect("outside directory");
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o755))
            .expect("outside permissions");
        fs::rename(
            manager.sessions_dir().join(LATE_USAGE_DIR),
            tmp.path().join("original-accounting"),
        )
        .expect("park original accounting directory");
        symlink(&outside, manager.sessions_dir().join(LATE_USAGE_DIR)).expect("linked store");
        let recovered = manager
            .load_session_snapshot("linked-directory")
            .expect("transcript");
        assert!(
            recovered
                .metadata
                .cost
                .unpriced_reasons
                .contains(LATE_USAGE_UNAVAILABLE_REASON)
        );
        assert_eq!(manager.list_sessions().expect("listing").len(), 1);
        assert!(
            manager
                .persist_late_runtime_usage(
                    "linked-directory",
                    "turn",
                    &late_usage_test_record("source")
                )
                .is_err()
        );
        assert_eq!(fs::read_dir(&outside).expect("outside contents").count(), 0);
        assert_eq!(
            fs::metadata(outside)
                .expect("outside metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }

    #[test]
    fn deleting_session_retires_late_usage_and_keeps_one_lock_inode() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        save_late_usage_test_session(&manager, "deleted-session");
        let record = late_usage_test_record("before-deletion");
        manager
            .persist_late_runtime_usage("deleted-session", "turn", &record)
            .expect("append");
        let (ledger, lock_path) = manager.late_usage_paths("deleted-session").expect("paths");
        let mut old_lock =
            fd_lock::RwLock::new(open_private_lock_file(&lock_path).expect("captured lock"));

        manager.delete_session("deleted-session").expect("delete");
        assert!(!ledger.exists());
        assert!(
            !manager
                .validated_session_path("deleted-session")
                .expect("session path")
                .exists()
        );
        assert!(SessionManager::late_usage_is_deleted(&ledger).expect("tombstone"));
        assert!(manager.load_late_usage("deleted-session").is_err());
        assert!(
            manager
                .persist_late_runtime_usage("deleted-session", "turn", &record)
                .expect("retired replay")
        );
        assert!(
            !ledger.exists(),
            "late callback must not resurrect accounting"
        );
        manager
            .delete_session("deleted-session")
            .expect("idempotent cleanup retry");

        let mut new_lock =
            fd_lock::RwLock::new(open_private_lock_file(&lock_path).expect("current lock"));
        let _held = old_lock.write().expect("old handle still owns the lock");
        assert!(
            matches!(new_lock.try_write(), Err(error) if error.kind() == io::ErrorKind::WouldBlock),
            "deletion must not replace or unlink a held lock inode"
        );
    }

    #[test]
    fn lifecycle_admission_holds_delete_lock_and_rejects_retired_origin() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        save_late_usage_test_session(&manager, "active-origin");
        let (_, lock_path) = manager.late_usage_paths("active-origin").expect("paths");
        let mut competing_lock =
            fd_lock::RwLock::new(open_private_lock_file(&lock_path).expect("competing lock"));
        assert_eq!(
            manager
                .with_live_session_origin("active-origin", || {
                    assert!(
                        matches!(competing_lock.try_write(), Err(error) if error.kind() == io::ErrorKind::WouldBlock),
                        "active acceptance must hold the deletion lock"
                    );
                    true
                })
                .expect("active acceptance"),
            Some(true)
        );
        assert_eq!(
            manager
                .with_live_session_origin("active-origin", || false)
                .expect("stale scope falls through"),
            Some(false)
        );
        drop(competing_lock.write().expect("admission releases the lock"));

        manager.delete_session("active-origin").expect("delete");
        let mut ran_after_delete = false;
        assert_eq!(
            manager
                .with_live_session_origin("active-origin", || {
                    ran_after_delete = true;
                    true
                })
                .expect("retired origin"),
            None
        );
        assert!(!ran_after_delete, "retired scopes cannot accept new usage");
    }

    #[test]
    fn deleted_session_rejects_stale_snapshot_and_checkpoint_saves() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let stale = save_late_usage_test_session(&manager, "stale-writer");
        manager.delete_session("stale-writer").expect("delete");
        for error in [
            manager.save_session(&stale).expect_err("reject stale save"),
            manager
                .save_checkpoint(&stale)
                .expect_err("reject stale checkpoint"),
        ] {
            assert_eq!(error.kind(), io::ErrorKind::NotFound);
        }
        assert!(manager.list_sessions().expect("list").is_empty());
        assert!(
            !manager
                .validated_checkpoint_path("stale-writer")
                .expect("checkpoint path")
                .exists(),
            "a retired writer must not recreate crash-recovery data"
        );
    }

    #[test]
    fn explicit_delete_removes_owned_recovery_checkpoints_only() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let retired = save_late_usage_test_session(&manager, "retired-recovery");
        let retained = save_late_usage_test_session(&manager, "retained-recovery");
        manager.save_checkpoint(&retired).expect("owned checkpoint");
        manager
            .save_checkpoint(&retained)
            .expect("other checkpoint");
        let legacy_path = manager.checkpoints_dir().join(LEGACY_CHECKPOINT_FILE);
        write_atomic(
            &legacy_path,
            serialize_saved_session(retired.clone())
                .expect("legacy bytes")
                .as_bytes(),
        )
        .expect("owned legacy checkpoint");

        assert!(manager.delete_session("retired-recovery").is_err());
        assert!(legacy_path.exists());
        fs::remove_file(&legacy_path).expect("remove legacy checkpoint");
        manager.delete_session("retired-recovery").expect("delete");
        assert!(manager.load_legacy_checkpoint().expect("legacy").is_none());
        assert!(
            manager
                .load_session_checkpoint("retired-recovery")
                .expect("owned checkpoint")
                .is_none()
        );
        let checkpoints = manager.list_checkpoints().expect("checkpoint picker");
        assert_eq!(checkpoints.len(), 1);
        assert!(matches!(
            &checkpoints[0].source,
            CheckpointSource::Session(id) if id == "retained-recovery"
        ));
        assert!(
            manager
                .load_session_checkpoint("retained-recovery")
                .expect("other recovery")
                .is_some()
        );

        // An origin can exist only as crash recovery, with no ordinary
        // snapshot. Explicit deletion must still be able to retire it.
        fs::remove_file(
            manager
                .validated_session_path("retained-recovery")
                .expect("ordinary snapshot path"),
        )
        .expect("simulate checkpoint-only origin");
        manager
            .delete_session("retained-recovery")
            .expect("delete recovery-only origin");
        assert!(
            manager
                .list_checkpoints()
                .expect("checkpoint picker")
                .is_empty()
        );
        assert!(manager.save_checkpoint(&retained).is_err());
    }

    #[test]
    fn retention_preserves_checkpoint_origin_receipts_and_evidence() {
        for retention in ["age", "size"] {
            for checkpoint_kind in ["session", "legacy"] {
                let tmp = tempdir().expect("tempdir");
                let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
                let id = "55555555-5555-4555-8555-555555555555";
                let mut old = save_late_usage_test_session(&manager, id);
                old.metadata.updated_at = Utc::now() - chrono::Duration::days(60);
                manager.save_session(&old).expect("old snapshot");
                let evidence = manager.sessions_dir().join(id).join("artifacts");
                fs::create_dir_all(&evidence).expect("recovery evidence");
                fs::write(evidence.join("receipt.txt"), b"recoverable evidence").expect("receipt");
                if checkpoint_kind == "session" {
                    manager.save_checkpoint(&old).expect("recovery checkpoint");
                } else {
                    fs::create_dir_all(manager.checkpoints_dir()).expect("checkpoints");
                    write_atomic(
                        &manager.checkpoints_dir().join(LEGACY_CHECKPOINT_FILE),
                        serialize_saved_session(old.clone())
                            .expect("legacy bytes")
                            .as_bytes(),
                    )
                    .expect("legacy recovery checkpoint");
                }
                manager
                    .persist_late_runtime_usage(id, "turn", &late_usage_test_record("before-prune"))
                    .expect("origin accounting");
                if retention == "age" {
                    assert_eq!(
                        manager
                            .prune_sessions_older_than(std::time::Duration::from_secs(24 * 3600))
                            .expect("age prune"),
                        1
                    );
                    assert!(
                        !manager
                            .validated_session_path(id)
                            .expect("snapshot path")
                            .exists(),
                        "an explicit age prune still unlinks"
                    );
                } else {
                    for index in 0..MAX_SESSIONS {
                        write_session_with_updated_at(
                            &manager,
                            &format!("fresh-{index}"),
                            Utc::now(),
                        );
                    }
                    manager.cleanup_old_sessions().expect("size cleanup");
                    let listed = manager.list_sessions().expect("sessions");
                    assert_eq!(
                        listed.len(),
                        MAX_SESSIONS + 1,
                        "the archived record stays listed outside the active cap"
                    );
                    let retained = listed
                        .iter()
                        .find(|session| session.id == id)
                        .expect("archived record remains on disk");
                    assert!(
                        retained.archived,
                        "a transcript past the cap is archived, never unlinked (#6136)"
                    );
                    assert!(
                        manager
                            .validated_session_path(id)
                            .expect("snapshot path")
                            .exists(),
                        "the transcript file survives retention"
                    );
                }
                let (ledger, _) = manager.late_usage_paths(id).expect("ledger paths");
                assert!(!SessionManager::late_usage_is_deleted(&ledger).expect("origin retained"));
                assert!(ledger.exists(), "recovery must retain accounting");
                assert!(
                    evidence.join("receipt.txt").exists(),
                    "recovery must retain evidence"
                );
                assert_eq!(
                    manager
                        .with_live_session_origin(id, || true)
                        .expect("resume admission"),
                    Some(true)
                );
                let mut recovered = if checkpoint_kind == "session" {
                    manager
                        .load_session_checkpoint(id)
                        .expect("checkpoint read")
                } else {
                    manager.load_legacy_checkpoint().expect("legacy read")
                }
                .expect("retained recovery");
                assert_eq!(
                    recovered.metadata.total_tokens, 1,
                    "checkpoint overlays exact origin usage"
                );
                recovered.metadata.updated_at = Utc::now();
                manager
                    .save_session(&recovered)
                    .expect("save resumed origin");
                assert_eq!(
                    manager
                        .load_session_snapshot(id)
                        .expect("resumed snapshot")
                        .metadata
                        .total_tokens,
                    1,
                    "replayed recovery accounting remains idempotent"
                );
            }
        }
    }

    #[test]
    fn external_writers_are_refused_while_another_process_holds_the_live_lease() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let id = "77777777-7777-4777-8777-777777777777";
        save_late_usage_test_session(&manager, id);
        let snapshot = manager.validated_session_path(id).expect("snapshot path");
        let original = fs::read(&snapshot).expect("original bytes");
        // Another process's TUI holds the lease; this process's in-memory
        // registry knows nothing about it.
        let lease = manager.hold_live_lease_elsewhere(id);
        assert!(!is_live_session(id));

        let rename = manager
            .rename_session(id, "Renamed elsewhere", SessionMutator::External)
            .expect_err("a rename would be reverted by the owner's next autosave");
        assert_eq!(rename.kind(), io::ErrorKind::ResourceBusy);
        let archive = manager
            .set_session_archived(id, true, SessionMutator::External)
            .expect_err("an archive would be reverted by the owner's next autosave");
        assert_eq!(archive.kind(), io::ErrorKind::ResourceBusy);
        assert_eq!(fs::read(&snapshot).expect("bytes"), original);

        drop(lease);
        manager
            .rename_session(id, "Renamed after close", SessionMutator::External)
            .expect("an unowned session accepts external writes");
    }

    #[test]
    fn external_mutations_keep_the_live_lease_until_the_blocked_write_finishes() {
        let _env = crate::test_support::lock_test_env();
        let mut failures = Vec::new();
        for operation in ["rename", "archive", "delete"] {
            let tmp = tempdir().expect("tempdir");
            let manager = std::sync::Arc::new(
                SessionManager::new(tmp.path().join("sessions")).expect("manager"),
            );
            let id = "88888888-8888-4888-8888-888888888888";
            save_late_usage_test_session(&manager, id);
            // Hold the actual persistence lock so the shipping mutation stays
            // between admission and commit while another surface tries attach.
            let (_, lock_path) = manager.ensure_late_usage_paths(id).expect("paths");
            let mut lock = fd_lock::RwLock::new(open_private_lock_file(&lock_path).unwrap());
            let write_guard = lock.write().expect("hold persistence lock");
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let worker_manager = manager.clone();
            let ticket = crate::test_support::env_scope_ticket();
            let worker = std::thread::spawn(move || {
                let _membership = crate::test_support::join_env_scope(ticket);
                entered_tx.send(()).unwrap();
                match operation {
                    "rename" => worker_manager
                        .rename_session(id, "Atomic rename", SessionMutator::External)
                        .map(|_| ()),
                    "archive" => worker_manager
                        .set_session_archived(id, true, SessionMutator::External)
                        .map(|_| ()),
                    "delete" => worker_manager.delete_session(id),
                    _ => unreachable!(),
                }
            });
            entered_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !manager.is_session_live_anywhere(id)
                && !worker.is_finished()
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let holds_lease = manager.is_session_live_anywhere(id);
            let attach_refused = manager
                .reserve_session_for_attach(id)
                .is_err_and(|error| error.kind() == io::ErrorKind::ResourceBusy);
            // Release and join before asserting, even in the fixes-off case;
            // no failed assertion may leave a blocked mutation behind.
            drop(write_guard);
            worker
                .join()
                .expect("mutation worker")
                .expect("mutation succeeds");
            if !holds_lease {
                failures.push(format!("{operation}: released its lease before commit"));
            }
            if !attach_refused {
                failures.push(format!("{operation}: admitted a competing session owner"));
            }
            assert!(
                !manager.is_session_live_anywhere(id),
                "{operation}: leaked its lease"
            );
        }
        assert!(failures.is_empty(), "{failures:?}");
    }

    #[cfg(unix)]
    #[test]
    fn uncertain_live_lease_refuses_external_mutations_without_changing_history() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let id = "99999999-9999-4999-8999-999999999999";
        save_late_usage_test_session(&manager, id);
        let snapshot = manager.validated_session_path(id).unwrap();
        let original = fs::read(&snapshot).unwrap();
        let target = tmp.path().join("untouched");
        fs::write(&target, b"unrelated bytes").unwrap();
        let live_path = manager.live_lease_path(id, true).unwrap();
        std::os::unix::fs::symlink(&target, &live_path).unwrap();
        assert!(
            manager.is_session_live_anywhere(id),
            "uncertain ownership is not free"
        );
        assert!(
            manager
                .rename_session(id, "Unsafe", SessionMutator::External)
                .is_err()
        );
        assert!(
            manager
                .set_session_archived(id, true, SessionMutator::External)
                .is_err()
        );
        assert!(manager.delete_session(id).is_err());
        assert_eq!(fs::read(snapshot).unwrap(), original);
        assert_eq!(fs::read(target).unwrap(), b"unrelated bytes");
    }

    #[test]
    fn retention_fails_closed_on_an_unreadable_legacy_checkpoint_origin() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let id = "66666666-6666-4666-8666-666666666666";
        let mut old = save_late_usage_test_session(&manager, id);
        old.metadata.updated_at = Utc::now() - chrono::Duration::days(60);
        manager.save_session(&old).expect("old snapshot");
        let snapshot = manager.validated_session_path(id).expect("snapshot path");
        let original = fs::read(&snapshot).expect("original snapshot bytes");
        // Fault fixture: a legacy checkpoint whose owner cannot be read. It
        // may be this session's (damaged) recovery.
        fs::create_dir_all(manager.checkpoints_dir()).expect("checkpoints");
        let legacy = manager.checkpoints_dir().join(LEGACY_CHECKPOINT_FILE);
        let corrupt: &[u8] = b"{\"messages\": [truncated";
        write_atomic(&legacy, corrupt).expect("corrupt legacy checkpoint");
        assert!(manager.legacy_checkpoint_origin().is_err());

        assert_eq!(
            manager
                .prune_sessions_older_than(std::time::Duration::from_secs(24 * 3600))
                .expect("age prune"),
            0,
            "an uncertain origin must not be pruned"
        );
        assert_eq!(
            fs::read(&snapshot).expect("snapshot survives retention"),
            original,
            "the ordinary snapshot keeps its original bytes"
        );
        assert_eq!(fs::read(&legacy).expect("legacy survives"), corrupt);
        let (ledger, _) = manager.late_usage_paths(id).expect("ledger paths");
        assert!(!SessionManager::late_usage_is_deleted(&ledger).expect("not retired"));

        assert!(manager.delete_session(id).is_err());
        assert_eq!(fs::read(&snapshot).unwrap(), original);
        assert_eq!(fs::read(&legacy).unwrap(), corrupt);
        fs::remove_file(&legacy).expect("remove damaged legacy fixture");
        manager.delete_session(id).expect("explicit delete");
        assert!(!snapshot.exists());
    }

    #[test]
    fn interrupted_session_deletion_keeps_recovery_incomplete_and_can_finish() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let saved = save_late_usage_test_session(&manager, "interrupted-delete");
        manager
            .save_checkpoint(&saved)
            .expect("checkpoint before deletion");
        write_atomic(
            &manager.checkpoints_dir().join(LEGACY_CHECKPOINT_FILE),
            serialize_saved_session(saved.clone())
                .expect("legacy bytes")
                .as_bytes(),
        )
        .expect("legacy checkpoint before deletion");
        let legacy = manager.checkpoints_dir().join(LEGACY_CHECKPOINT_FILE);
        let legacy_before = fs::read(&legacy).expect("legacy bytes");
        let (ledger, _) = manager
            .ensure_late_usage_paths("interrupted-delete")
            .expect("paths");
        write_atomic(&ledger.with_extension("deleted"), LATE_USAGE_DELETED)
            .expect("crash after tombstone");
        let recovered = manager
            .load_session_snapshot("interrupted-delete")
            .expect("transcript remains recoverable");
        assert!(
            recovered
                .metadata
                .cost
                .unpriced_reasons
                .contains(LATE_USAGE_UNAVAILABLE_REASON)
        );
        assert!(
            manager
                .load_session_checkpoint("interrupted-delete")
                .expect("checkpoint read")
                .is_none(),
            "a checkpoint retired before a crash must not be offered for recovery"
        );
        assert!(
            manager
                .load_legacy_checkpoint()
                .expect("legacy read")
                .is_none()
        );
        manager
            .delete_session("interrupted-delete")
            .expect("finish deletion");
        assert!(manager.list_sessions().expect("list").is_empty());
        assert_eq!(fs::read(&legacy).expect("legacy preserved"), legacy_before);
        assert!(manager.load_legacy_checkpoint().expect("legacy read").is_none());
    }

    #[test]
    #[ignore = "subprocess helper for the late usage deletion regression"]
    fn late_usage_callback_subprocess() {
        let directory = PathBuf::from(
            std::env::var_os("CODEWHALE_LATE_USAGE_TEST_DIR").expect("fixture directory"),
        );
        let manager = SessionManager::new(directory.join("sessions")).expect("manager");
        manager
            .persist_late_runtime_usage(
                "process-delete-race",
                "turn",
                &late_usage_test_record("first-callback"),
            )
            .expect("first callback");
        fs::write(directory.join("ready"), b"ready").expect("signal ready");
        let started = std::time::Instant::now();
        while !directory.join("continue").exists() {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(10),
                "callback gate timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            manager
                .persist_late_runtime_usage(
                    "process-delete-race",
                    "turn",
                    &late_usage_test_record("late-callback")
                )
                .expect("retired callback")
        );
    }

    #[test]
    fn late_usage_callback_in_another_process_cannot_resurrect_deleted_session() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        save_late_usage_test_session(&manager, "process-delete-race");
        let mut child =
            std::process::Command::new(std::env::current_exe().expect("test executable"))
                .args([
                    "--exact",
                    "session_manager::tests::late_usage_callback_subprocess",
                    "--ignored",
                ])
                .env("CODEWHALE_LATE_USAGE_TEST_DIR", tmp.path())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("callback process");
        let started = std::time::Instant::now();
        while !tmp.path().join("ready").exists() {
            if started.elapsed() >= std::time::Duration::from_secs(10)
                || child.try_wait().expect("child status").is_some()
            {
                let _ = child.kill();
                let _ = child.wait();
                panic!("callback process did not reach the deletion gate");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            manager
                .load_session_snapshot("process-delete-race")
                .expect("first callback persisted")
                .metadata
                .total_tokens,
            1
        );
        let deleted = manager.delete_session("process-delete-race");
        fs::write(tmp.path().join("continue"), b"continue").expect("release callback");
        let status = loop {
            if let Some(status) = child.try_wait().expect("child status") {
                break status;
            }
            if started.elapsed() >= std::time::Duration::from_secs(10) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("late callback process did not finish");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        deleted.expect("delete while callback process was pending");
        assert!(status.success(), "callback process failed");
        let (ledger, _) = manager
            .late_usage_paths("process-delete-race")
            .expect("paths");
        assert!(!ledger.exists());
        assert!(manager.list_sessions().expect("list").is_empty());
    }

    #[test]
    fn late_usage_sidecar_survives_stale_session_save_and_replays_once() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let old_id = "old-session";
        let new_id = "new-session";
        let old = create_saved_session_with_id_and_mode(
            old_id.to_string(),
            &[make_test_message("user", "old session")],
            "deepseek-v4-flash",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        let new = create_saved_session_with_id_and_mode(
            new_id.to_string(),
            &[make_test_message("user", "new session")],
            "deepseek-v4-flash",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        manager.save_session(&old).expect("save old");
        manager.save_session(&new).expect("save new");

        let priced_route = crate::cost_status::EffectiveRouteEnvelope::capture(
            None,
            ProviderKind::Deepseek,
            "deepseek",
            "deepseek-v4-flash",
            Some(codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL),
            Utc::now(),
        );
        let usage = codewhale_models::Usage {
            input_tokens: 17,
            output_tokens: 5,
            ..codewhale_models::Usage::default()
        };
        let usage_record = crate::cost_status::RuntimeUsageRecord {
            source_id: "translation:old-turn:assistant:1".to_string(),
            usage: crate::cost_status::EffectiveRouteUsage {
                route: priced_route.clone(),
                usage: usage.clone(),
            },
        };
        let missing_record = crate::cost_status::RuntimeUsageDropRecord {
            reason: crate::cost_status::RuntimeUsageMissingReason::default(),
            source_id: "advisor:old-turn:provider-response:0".to_string(),
            route: priced_route,
        };
        let mut subscription_route = missing_record.route.clone();
        subscription_route.billing_mode = crate::cost_status::RouteBillingMode::Subscription;
        let subscription_missing = crate::cost_status::RuntimeUsageDropRecord {
            reason: crate::cost_status::RuntimeUsageMissingReason::default(),
            source_id: "translation:old-turn:thinking:2".to_string(),
            route: subscription_route,
        };

        for _ in 0..2 {
            assert!(
                manager
                    .persist_late_runtime_usage(old_id, "old-turn", &usage_record)
                    .expect("persist late usage")
            );
            assert!(
                manager
                    .persist_late_runtime_drop(old_id, "old-turn", &missing_record)
                    .expect("persist missing usage")
            );
            assert!(
                manager
                    .persist_late_runtime_drop(old_id, "old-turn", &subscription_missing)
                    .expect("persist subscription missing usage")
            );
        }

        // A concurrent stale whole-session writer cannot erase the independent
        // origin ledger. Loading overlays it once by stable response identity.
        manager.save_session(&old).expect("stale old-session save");
        let first = manager.load_session_snapshot(old_id).expect("load old");
        let second = manager.load_session_snapshot(old_id).expect("replay old");
        for loaded in [&first, &second] {
            assert_eq!(loaded.metadata.total_tokens, 22);
            assert_eq!(loaded.metadata.cost.unpriced_turns, 1);
            assert_eq!(loaded.metadata.cost.cny_unpriced_turns, 1);
            assert_eq!(loaded.metadata.cost.usage_source_fingerprints.len(), 3);
            assert!(
                loaded
                    .metadata
                    .cost
                    .unpriced_reasons
                    .contains("provider_success_missing_usage")
            );
        }
        assert_eq!(first.metadata.cost.priced_turns, 1);

        let clean = manager.load_session_snapshot(new_id).expect("load new");
        assert_eq!(clean.metadata.total_tokens, 0);
        assert_eq!(clean.metadata.cost.priced_turns, 0);
        assert_eq!(clean.metadata.cost.unpriced_turns, 0);
        assert!(clean.metadata.cost.usage_source_fingerprints.is_empty());

        let ledger = fs::read_to_string(
            manager
                .sessions_dir()
                .join(LATE_USAGE_DIR)
                .join(format!("{old_id}.json")),
        )
        .expect("late ledger");
        assert!(!ledger.contains("translation:old-turn"));
        assert!(!ledger.contains(codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let ledger_dir = manager.sessions_dir().join(LATE_USAGE_DIR);
            assert_eq!(
                fs::metadata(&ledger_dir)
                    .expect("private sidecar directory")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            for path in [
                ledger_dir.join(format!("{old_id}.json")),
                ledger_dir.join(format!("{old_id}.lock")),
            ] {
                assert_eq!(
                    fs::metadata(path)
                        .expect("private sidecar metadata")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
    }

    #[test]
    fn late_usage_sidecar_has_a_bounded_fail_closed_overflow() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let session_id = "bounded-session";
        let session = create_saved_session_with_id_and_mode(
            session_id.to_string(),
            &[make_test_message("user", "bounded session")],
            "local-model",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        manager.save_session(&session).expect("save bounded");
        let mut route = crate::cost_status::EffectiveRouteEnvelope::capture(
            None,
            ProviderKind::Custom,
            "local-provider",
            "local-model",
            Some("http://127.0.0.1:11434/v1"),
            Utc::now(),
        );
        route.billing_mode = crate::cost_status::RouteBillingMode::Local;
        for index in 0..=MAX_LATE_USAGE_UNRESOLVED_RECORDS_PER_SESSION {
            manager
                .persist_late_runtime_drop(
                    session_id,
                    "bounded-turn",
                    &crate::cost_status::RuntimeUsageDropRecord {
                        reason: crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown,
                        source_id: format!("late-bounded:{index}"),
                        route: route.clone(),
                    },
                )
                .expect("bounded append");
        }

        let loaded = manager
            .load_session_snapshot(session_id)
            .expect("load bounded");
        assert_eq!(
            loaded.metadata.total_tokens,
            0
        );
        assert_eq!(loaded.metadata.cost.unpriced_turns, 1);
        assert!(
            loaded
                .metadata
                .cost
                .unpriced_reasons
                .contains("late_usage_ledger_overflow")
        );
        let ledger = manager.load_late_usage(session_id).expect("bounded ledger");
        assert_eq!(ledger.records.len(), MAX_LATE_USAGE_UNRESOLVED_RECORDS_PER_SESSION);
        assert!(ledger.overflowed);
    }

    #[cfg(unix)]
    #[test]
    fn late_usage_sidecar_rejects_linked_lock_and_ledger_leaves() {
        use std::os::unix::fs::symlink;

        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let session_id = "linked-sidecar-session";
        save_late_usage_test_session(&manager, session_id);
        save_late_usage_test_session(&manager, "unaffected-sidecar-session");
        let (ledger_path, lock_path) = manager.ensure_late_usage_paths(session_id).expect("paths");
        let route = crate::cost_status::EffectiveRouteEnvelope::capture(
            None,
            ProviderKind::Deepseek,
            "deepseek",
            "deepseek-v4-flash",
            Some(codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL),
            Utc::now(),
        );
        let record = crate::cost_status::RuntimeUsageRecord {
            source_id: "linked-sidecar-response".to_string(),
            usage: crate::cost_status::EffectiveRouteUsage {
                route,
                usage: codewhale_models::Usage {
                    input_tokens: 1,
                    ..codewhale_models::Usage::default()
                },
            },
        };

        let outside_lock = tmp.path().join("outside.lock");
        fs::write(&outside_lock, b"outside-lock").expect("outside lock");
        fs::remove_file(&lock_path).expect("replace fixture lifecycle lock");
        symlink(&outside_lock, &lock_path).expect("symlink lock");
        assert!(
            manager
                .persist_late_runtime_usage(session_id, "turn", &record)
                .is_err(),
            "a symlink lock leaf must fail closed"
        );
        assert_eq!(
            fs::read(&outside_lock).expect("outside lock unchanged"),
            b"outside-lock"
        );
        fs::remove_file(&lock_path).expect("remove lock symlink");

        fs::hard_link(&outside_lock, &lock_path).expect("hard-linked lock");
        assert!(
            manager
                .persist_late_runtime_usage(session_id, "turn", &record)
                .is_err(),
            "a multiply linked lock leaf must fail closed"
        );
        fs::remove_file(&lock_path).expect("remove hard-linked lock");

        let outside_ledger = tmp.path().join("outside.json");
        fs::write(
            &outside_ledger,
            br#"{"schema_version":1,"records":[],"overflowed":false}"#,
        )
        .expect("outside ledger");
        symlink(&outside_ledger, &ledger_path).expect("symlink ledger");
        assert!(
            manager.load_late_usage(session_id).is_err(),
            "a symlink ledger leaf must fail closed"
        );
        assert!(
            manager
                .load_session_snapshot(session_id)
                .expect("recover linked ledger transcript")
                .metadata
                .cost
                .unpriced_reasons
                .contains(LATE_USAGE_UNAVAILABLE_REASON)
        );
        fs::remove_file(&ledger_path).expect("remove ledger symlink");

        fs::hard_link(&outside_ledger, &ledger_path).expect("hard-linked ledger");
        assert!(
            manager.load_late_usage(session_id).is_err(),
            "a multiply linked ledger leaf must fail closed"
        );
        assert_eq!(
            manager
                .list_sessions()
                .expect("list linked ledger transcript")
                .len(),
            2
        );
        assert_eq!(
            manager
                .load_session_by_prefix("unaffected")
                .expect("unaffected resume")
                .metadata
                .cost
                .unpriced_turns,
            0
        );
        assert_eq!(
            fs::read(&outside_ledger).expect("outside ledger unchanged"),
            br#"{"schema_version":1,"records":[],"overflowed":false}"#
        );
    }

    fn container_with(messages: Vec<Message>, dir: &std::path::Path) -> SessionImportContainer {
        let session = create_saved_session(&messages, "test-model", dir, 100, None);
        session.export_container("test-session.json")
    }

    #[test]
    fn session_goal_sidecar_round_trips_control_state_without_model_output() {
        let tmp = tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let session_id = "11111111-2222-4333-8444-555555555555";
        let runtime = GoalSnapshot {
            goal_id: None,
            objective: Some("finish the provider migration".to_string()),
            status: "paused".to_string(),
            token_budget: Some(50_000),
            tokens_used: 12_345,
            time_used_seconds: 67,
            continuation_count: 4,
            elapsed_seconds: Some(91),
            evidence: Some("Bearer credential-shaped-model-output".to_string()),
            blocker: Some("/arbitrary/private/path".to_string()),
            pause_reason: Some(GoalPauseReason::User),
            completion_verification: None,
            advisories: Vec::new(),
            last_gap_fingerprint: None,
            repeated_gap_count: 0,
            last_gap_pass: None,
            progress: None,
        };
        let durable = SessionGoalState::from_runtime(&runtime)
            .expect("valid runtime goal")
            .expect("non-empty durable goal");

        manager
            .save_session_goal(session_id, Some(&durable))
            .expect("save goal");
        let raw = fs::read_to_string(
            sessions_dir
                .join(SESSION_GOALS_DIR)
                .join(format!("{session_id}.json")),
        )
        .expect("read goal sidecar");
        assert!(!raw.contains("credential-shaped-model-output"));
        assert!(!raw.contains("/arbitrary/private/path"));

        let reopened = SessionManager::new(sessions_dir).expect("reopen manager");
        let restored = reopened
            .load_session_goal(session_id)
            .expect("load goal")
            .expect("persisted goal");
        assert_eq!(restored, durable);
        assert_eq!(restored.to_runtime_snapshot().objective, runtime.objective);
        assert_eq!(restored.to_runtime_snapshot().status, "paused");

        reopened
            .save_session_goal(session_id, None)
            .expect("clear goal");
        assert_eq!(
            reopened.load_session_goal(session_id).expect("load clear"),
            None
        );
    }

    /// Coverage state round-trips with the money it qualifies, and a session
    /// written before coverage existed is detected as *unknown* rather than being
    /// read as a complete total covering zero turns (#4318).
    #[test]
    fn cost_snapshot_round_trips_coverage_and_detects_legacy_unknown() {
        // A pre-coverage row: real money, no coverage fields at all.
        let legacy: SessionCostSnapshot = serde_json::from_value(serde_json::json!({
            "session_cost_usd": 1.25,
            "session_cost_cny": 0.0,
            "subagent_cost_usd": 0.0,
            "subagent_cost_cny": 0.0,
            "displayed_cost_high_water_usd": 1.25,
            "displayed_cost_high_water_cny": 0.0
        }))
        .expect("legacy cost snapshot stays readable");
        assert_eq!(legacy.priced_turns, 0);
        assert_eq!(legacy.unpriced_turns, 0);
        assert!(!legacy.coverage_recorded);
        assert!(
            legacy.coverage_is_legacy_unknown(),
            "a non-zero total with no coverage evidence must not read as complete"
        );

        // An all-zero pre-coverage session is still unknown: zero may mean no
        // turns, all unpriced turns, or exact zero usage. Absence of evidence is
        // never rewritten into a complete 0/0 claim.
        let empty = SessionCostSnapshot::default();
        assert!(empty.coverage_is_legacy_unknown());

        // A coverage-aware writer that recorded zero money-metered turns is also
        // not unknown — it positively knows the answer is zero.
        let recorded_zero = SessionCostSnapshot {
            session_cost_usd: 1.25,
            coverage_recorded: true,
            ..SessionCostSnapshot::default()
        };
        assert!(!recorded_zero.coverage_is_legacy_unknown());

        // Full round-trip of every coverage field.
        let full = SessionCostSnapshot {
            session_cost_usd: 2.5,
            session_cost_cny: 3.0,
            subagent_cost_usd: 0.5,
            subagent_cost_cny: 0.25,
            displayed_cost_high_water_usd: 3.0,
            displayed_cost_high_water_cny: 3.25,
            priced_turns: 7,
            unpriced_turns: 2,
            cny_priced_turns: 1,
            cny_unpriced_turns: 8,
            unpriced_reasons: ["missing_class_price".to_string()].into(),
            cny_unpriced_reasons: ["currency_not_published".to_string()].into(),
            unpriced_classes: ["cache_write".to_string()].into(),
            pricing_provenances: ["models_dev_bundled".to_string()].into(),
            live_pricing_defects: ["live_pricing_stale".to_string()].into(),
            live_pricing_unusable_defects: ["live_pricing_scope_mismatch".to_string()].into(),
            route_receipts: ["provider=anthropic identity=- model=claude-haiku-4-5 \
                 surface=first-party-payg endpoint_fp=abc123 currency=usd"
                .to_string()]
            .into(),
            usage_source_fingerprints: ["response-fingerprint".to_string()].into(),
            missing_usage_sources: Default::default(),
            missing_usage_overflowed: false,
            coverage_recorded: true,
        };
        let json = serde_json::to_string(&full).expect("serialize");
        let back: SessionCostSnapshot = serde_json::from_str(&json).expect("round-trip");
        assert_eq!(back.priced_turns, 7);
        assert_eq!(back.unpriced_turns, 2);
        assert_eq!(back.cny_priced_turns, 1);
        assert_eq!(back.cny_unpriced_turns, 8);
        assert_eq!(back.unpriced_reasons, full.unpriced_reasons);
        assert_eq!(back.cny_unpriced_reasons, full.cny_unpriced_reasons);
        assert_eq!(back.unpriced_classes, full.unpriced_classes);
        assert_eq!(back.pricing_provenances, full.pricing_provenances);
        assert_eq!(back.live_pricing_defects, full.live_pricing_defects);
        assert_eq!(
            back.usage_source_fingerprints,
            full.usage_source_fingerprints
        );
        assert_eq!(
            back.live_pricing_unusable_defects,
            full.live_pricing_unusable_defects
        );
        assert_eq!(back.route_receipts, full.route_receipts);
        assert!(back.coverage_recorded);
        assert!(!back.coverage_is_legacy_unknown());

        // The persisted receipts carry no endpoint URL or credential.
        let lower = json.to_lowercase();
        for needle in ["http", "api_key", "authorization", "bearer", "sk-"] {
            assert!(!lower.contains(needle), "{needle} leaked into {json}");
        }
    }

    /// The USD and CNY totals a snapshot reports are projections of one
    /// dual-currency accumulation, never two independent sums that could
    /// disagree (#4939).
    ///
    /// For any turn sequence — dual-priced, USD-only, CNY-only, or garbage
    /// estimates — folding the turns jointly and projecting each currency must
    /// equal accumulating that currency on its own. This is the invariant that
    /// makes the persisted per-currency columns safe: they are written from the
    /// same joint fold, so a code path can no longer update one and forget the
    /// other. CNY is derived from provider-published CNY rows, not from an FX
    /// multiple of USD, so a USD-only turn must contribute exactly zero CNY.
    #[test]
    fn cost_snapshot_currency_totals_are_projections_of_one_accumulator() {
        use crate::pricing::CostEstimate;

        let turn_sequences: &[&[CostEstimate]] = &[
            // Dual-priced turns (DeepSeek-style routes with a published CNY row).
            &[
                CostEstimate {
                    usd: 0.01,
                    cny: 0.07,
                },
                CostEstimate {
                    usd: 0.02,
                    cny: 0.14,
                },
            ],
            // USD-only turns: CNY unpublished, so the CNY projection stays zero.
            &[
                CostEstimate {
                    usd: 0.25,
                    cny: 0.0,
                },
                CostEstimate { usd: 1.5, cny: 0.0 },
            ],
            // Mixed: one currency priced per turn, alternating.
            &[
                CostEstimate { usd: 0.5, cny: 0.0 },
                CostEstimate { usd: 0.0, cny: 3.5 },
                CostEstimate {
                    usd: 0.125,
                    cny: 0.875,
                },
            ],
            // Hostile values: sanitization must apply identically per currency.
            &[
                CostEstimate {
                    usd: f64::NAN,
                    cny: 0.25,
                },
                CostEstimate {
                    usd: 0.75,
                    cny: -1.0,
                },
                CostEstimate {
                    usd: f64::INFINITY,
                    cny: 0.25,
                },
            ],
        ];

        for turns in turn_sequences {
            // Joint fold: how the app accumulates (one accumulator, both
            // currencies advance together through the same saturating_add).
            let joint = turns.iter().fold(CostEstimate::default(), |acc, turn| {
                acc.saturating_add(*turn)
            });

            // Independent per-currency folds: what a drifted parallel
            // accumulator would compute if it only saw one currency.
            let usd_alone = turns.iter().fold(CostEstimate::default(), |acc, turn| {
                acc.saturating_add(CostEstimate {
                    usd: turn.usd,
                    cny: 0.0,
                })
            });
            let cny_alone = turns.iter().fold(CostEstimate::default(), |acc, turn| {
                acc.saturating_add(CostEstimate {
                    usd: 0.0,
                    cny: turn.cny,
                })
            });

            let snapshot = SessionCostSnapshot {
                session_cost_usd: joint.usd,
                session_cost_cny: joint.cny,
                ..SessionCostSnapshot::default()
            };
            assert_eq!(
                snapshot.total_usd(),
                usd_alone.usd,
                "USD projection drifted from independent accumulation for {turns:?}"
            );
            assert_eq!(
                snapshot.total_cny(),
                cny_alone.cny,
                "CNY projection drifted from independent accumulation for {turns:?}"
            );
            assert_eq!(snapshot.total_estimate().usd, snapshot.total_usd());
            assert_eq!(snapshot.total_estimate().cny, snapshot.total_cny());
        }

        // A USD-only session projects zero CNY — no fabricated FX conversion —
        // and the subagent column joins the same fold.
        let usd_only = SessionCostSnapshot {
            session_cost_usd: 2.5,
            subagent_cost_usd: 0.5,
            ..SessionCostSnapshot::default()
        };
        assert_eq!(usd_only.total_usd(), 3.0);
        assert_eq!(usd_only.total_cny(), 0.0);
    }
    #[test]
    fn late_usage_unresolved_overflow_retains_modern_known_receipts_once_after_restart() {
        let tmp = tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions.clone()).unwrap();
        let session = create_saved_session_with_id_and_mode(
            "overflow-origin".into(),
            &[],
            "deepseek-v4-flash",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        manager.save_session(&session).unwrap();
        let route = crate::cost_status::EffectiveRouteEnvelope::capture(
            None,
            ProviderKind::Deepseek,
            "deepseek",
            "deepseek-v4-flash",
            Some("https://api.deepseek.com/v1"),
            Utc::now(),
        );
        for index in 0..65 {
            manager
                .persist_late_runtime_drop(
                    "overflow-origin",
                    "turn",
                    &crate::cost_status::RuntimeUsageDropRecord {
                        reason:
                            crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown,
                        source_id: format!("attempt-{index}"),
                        route: route.clone(),
                    },
                )
                .unwrap();
        }
        let missing = manager.load_session_snapshot("overflow-origin").unwrap();
        assert_eq!(missing.metadata.cost.unpriced_turns, 65);
        assert_eq!(missing.metadata.cost.missing_usage_sources.len(), 64);
        manager.save_session(&missing).unwrap();
        drop(manager);
        let manager = SessionManager::new(sessions).unwrap();
        let mut changed = route.clone();
        changed.model = "different-model".into();
        let invalid = crate::cost_status::RuntimeUsageRecord {
            source_id: "attempt-0".into(),
            usage: crate::cost_status::EffectiveRouteUsage {
                route: changed,
                usage: codewhale_models::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    ..Default::default()
                },
            },
        };
        assert_eq!(
            manager
                .persist_late_runtime_usage("overflow-origin", "turn", &invalid)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        for index in 0..65 {
            let known = crate::cost_status::RuntimeUsageRecord {
                source_id: format!("attempt-{index}"),
                usage: crate::cost_status::EffectiveRouteUsage {
                    route: route.clone(),
                    usage: codewhale_models::Usage {
                        input_tokens: 1,
                        output_tokens: 1,
                        ..Default::default()
                    },
                },
            };
            for _ in 0..2 {
                assert!(
                    manager
                        .persist_late_runtime_usage("overflow-origin", "turn", &known)
                        .unwrap()
                );
            }
        }
        let known = manager.load_session_snapshot("overflow-origin").unwrap();
        assert_eq!(known.metadata.total_tokens, 130);
        assert_eq!(known.metadata.cost.priced_turns, 65);
        assert_eq!(known.metadata.cost.unpriced_turns, 1);
        assert!(known.metadata.cost.missing_usage_sources.is_empty());
        assert!(
            known
                .metadata
                .cost
                .unpriced_reasons
                .contains("late_usage_ledger_overflow")
        );
        manager.save_session(&known).unwrap();
        let replay = manager.load_session_snapshot("overflow-origin").unwrap();
        assert_eq!(replay.metadata.total_tokens, 130);
        assert_eq!(replay.metadata.cost.priced_turns, 65);
        assert_eq!(replay.metadata.cost.unpriced_turns, 1);
        let ledger = manager.load_late_usage("overflow-origin").unwrap();
        assert_eq!(ledger.records.len(), 65);
        assert!(ledger.overflowed);
        assert!(ledger.records.iter().all(|record| record.usage.is_some()));
    }

    #[test]
    fn late_known_receipt_byte_exhaustion_errors_preserves_previous_records_and_gap() {
        let tmp = tempdir().unwrap();
        let manager = SessionManager::new(tmp.path().join("sessions")).unwrap();
        let route = crate::cost_status::EffectiveRouteEnvelope::capture(
            None,
            ProviderKind::Deepseek,
            "deepseek",
            "deepseek-v4-flash",
            Some("https://api.deepseek.com/v1"),
            Utc::now(),
        );
        let record = LateUsageRecord {
            source_fingerprint: crate::cost_status::usage_source_fingerprint("retained"),
            turn_fingerprint: crate::cost_status::usage_source_fingerprint("turn"),
            route: route.clone(),
            usage: Some(codewhale_models::Usage {
                input_tokens: 1,
                ..Default::default()
            }),
            reason: crate::cost_status::RuntimeUsageMissingReason::default(),
            decision: None,
        };
        let empty_bytes = serde_json::to_vec(&LateUsageLedger::default())
            .unwrap()
            .len();
        let record_bytes = serde_json::to_vec(&record).unwrap().len() + 1;
        let count =
            (usize::try_from(MAX_LATE_USAGE_LEDGER_BYTES).unwrap() - empty_bytes) / record_bytes;
        let original = LateUsageLedger {
            records: (0..count)
                .map(|index| LateUsageRecord {
                    source_fingerprint: crate::cost_status::usage_source_fingerprint(&format!(
                        "retained-{index}"
                    )),
                    ..record.clone()
                })
                .collect(),
            ..Default::default()
        };
        let (path, _) = manager.ensure_late_usage_paths("byte-limit").unwrap();
        SessionManager::write_late_usage_ledger(&path, &original).unwrap();
        let extra = crate::cost_status::RuntimeUsageRecord {
            source_id: "must-not-be-marked-handled".into(),
            usage: crate::cost_status::EffectiveRouteUsage {
                route,
                usage: codewhale_models::Usage {
                    input_tokens: 1,
                    ..Default::default()
                },
            },
        };
        assert_eq!(
            manager
                .persist_late_runtime_usage("byte-limit", "turn", &extra)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        let retained = manager.load_late_usage("byte-limit").unwrap();
        assert_eq!(retained.records.len(), original.records.len());
        assert!(retained.overflowed);
        assert!(
            !retained
                .records
                .iter()
                .any(|record| record.source_fingerprint
                    == crate::cost_status::usage_source_fingerprint(&extra.source_id))
        );
        assert!(fs::metadata(&path).unwrap().len() <= MAX_LATE_USAGE_LEDGER_BYTES);
        // The opened-file boundary rejects oversized bytes independently of
        // the serializer; neither case silently truncates a known receipt.
        fs::write(
            &path,
            vec![b' '; usize::try_from(MAX_LATE_USAGE_LEDGER_BYTES + 1).unwrap()],
        )
        .unwrap();
        assert_eq!(
            manager.load_late_usage("byte-limit").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
