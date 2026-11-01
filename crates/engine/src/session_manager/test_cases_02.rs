

    fn write_session_record(
        manager: &SessionManager,
        id: &str,
        workspace: &Path,
        updated_at: DateTime<Utc>,
    ) {
        let session = SavedSession {
            schema_version: CURRENT_SESSION_SCHEMA_VERSION,
            messages: vec![make_test_message("user", "hi")],
            metadata: SessionMetadata {
                id: id.to_string(),
                title: format!("session-{id}"),
                created_at: updated_at,
                updated_at,
                message_count: 1,
                total_tokens: 0,
                model: "deepseek-v4-flash".to_string(),
                model_provider: "deepseek".to_string(),
                model_provider_id: None,
                workspace: workspace.to_path_buf(),
                mode: None,
                cost: SessionCostSnapshot::default(),
                parent_session_id: None,
                forked_from_message_count: None,
                runtime_store: None,
                cumulative_turn_secs: 0,
                archived: false,
                spawn_depth: 0,
            },
            journal: None,
            leaf_id: None,
            system_prompt: None,
            context_references: Vec::new(),
            artifacts: Vec::new(),
            approval_receipts: Vec::new(),
            work_state: None,
            window_title: None,
            composer_color: None,
            last_auto_route: None,
            turn_outcomes: Vec::new(),
        };
        manager.save_session(&session).expect("save");
    }

    fn write_empty_session_record(
        manager: &SessionManager,
        id: &str,
        workspace: &Path,
        updated_at: DateTime<Utc>,
    ) {
        let session = SavedSession {
            schema_version: CURRENT_SESSION_SCHEMA_VERSION,
            messages: Vec::new(),
            metadata: SessionMetadata {
                id: id.to_string(),
                title: DEFAULT_SESSION_TITLE.to_string(),
                created_at: updated_at,
                updated_at,
                message_count: 0,
                total_tokens: 0,
                model: "deepseek-v4-pro".to_string(),
                model_provider: "deepseek".to_string(),
                model_provider_id: None,
                workspace: workspace.to_path_buf(),
                mode: Some("yolo".to_string()),
                cost: SessionCostSnapshot::default(),
                parent_session_id: None,
                forked_from_message_count: None,
                runtime_store: None,
                cumulative_turn_secs: 0,
                archived: false,
                spawn_depth: 0,
            },
            journal: None,
            leaf_id: None,
            system_prompt: None,
            context_references: Vec::new(),
            artifacts: Vec::new(),
            approval_receipts: Vec::new(),
            work_state: None,
            window_title: None,
            composer_color: None,
            last_auto_route: None,
            turn_outcomes: Vec::new(),
        };
        manager.save_session(&session).expect("save empty");
    }

    // === session retention and independent runtime data ===

    #[test]
    fn cleanup_preserves_artifacts_without_a_session_snapshot() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().to_path_buf()).expect("manager");
        let workspace = tmp.path().join("ws");

        let orphan = "11111111-1111-4111-8111-111111111111";
        let live = "22222222-2222-4222-8222-222222222222";
        for id in [orphan, live] {
            let artifacts = tmp.path().join(id).join("artifacts");
            fs::create_dir_all(&artifacts).expect("artifact dir");
            fs::write(artifacts.join("art_evidence.txt"), b"stdout").expect("artifact");
        }
        // Only `live` still has a session document.
        write_session_record(&manager, live, &workspace, Utc::now());

        manager.cleanup_old_sessions().expect("cleanup");

        assert!(
            tmp.path()
                .join(orphan)
                .join("artifacts/art_evidence.txt")
                .exists(),
            "an absent snapshot does not authorize deleting independent evidence"
        );
        assert!(
            tmp.path().join(live).join("artifacts").exists(),
            "a directory whose session still exists must be left alone"
        );
    }

    #[tokio::test]
    async fn cleanup_in_another_process_preserves_runtime_without_a_snapshot() {
        const PROBE: &str = "CODEWHALE_RUNTIME_RETENTION_PROBE";
        if let Some(directory) = std::env::var_os(PROBE) {
            let manager = SessionManager::new(PathBuf::from(directory)).expect("child manager");
            manager.cleanup_old_sessions().expect("child retention");
            return;
        }
        let tmp = tempdir().expect("tempdir");
        let sessions = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions.clone()).expect("manager");
        let runtime = sessions.join("44444444-4444-4444-8444-444444444444/runtime");
        let store = crate::runtime_threads::RuntimeThreadStore::open(runtime.clone())
            .expect("live automation store");
        let first = store
            .append_event(
                "thread_probe",
                None,
                None,
                "probe",
                serde_json::json!({"step": 1}),
            )
            .await
            .expect("first durable event");
        let run_cleanup = || {
            let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
                .args([
                    "--exact",
                    "session_manager::tests::cleanup_in_another_process_preserves_runtime_without_a_snapshot",
                    "--nocapture",
                ])
                .env(PROBE, &sessions)
                .output()
                .expect("independent cleanup process");
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        };
        assert!(
            manager
                .list_sessions()
                .expect("no interactive snapshot")
                .is_empty()
        );
        run_cleanup();
        assert_eq!(
            store.current_seq().await.expect("live cursor survives"),
            first.seq
        );
        drop(store);
        // A closed store can still own resumable events. It is not garbage
        // simply because no process or interactive transcript claims it.
        run_cleanup();
        let reopened = crate::runtime_threads::RuntimeThreadStore::open(runtime)
            .expect("reopen preserved automation store");
        assert_eq!(
            reopened.current_seq().await.expect("recovered cursor"),
            first.seq
        );
        let next = reopened
            .append_event(
                "thread_probe",
                None,
                None,
                "probe",
                serde_json::json!({"step": 2}),
            )
            .await
            .expect("continue recovered event sequence");
        assert_eq!(next.seq, first.seq + 1);
    }

    #[test]
    fn a_crashed_sessions_evidence_survives_even_without_its_document() {
        // Recovery reads exactly this: a checkpoint with no session document.
        // Reclaiming its evidence would delete what recovery needs.
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().to_path_buf()).expect("manager");
        let crashed = "33333333-3333-4333-8333-333333333333";

        fs::create_dir_all(tmp.path().join(crashed).join("artifacts")).expect("artifacts");
        let checkpoints = tmp.path().join("checkpoints");
        fs::create_dir_all(&checkpoints).expect("checkpoints dir");
        fs::write(checkpoints.join(format!("{crashed}.json")), b"{}").expect("checkpoint");

        manager.cleanup_old_sessions().expect("cleanup");

        assert!(
            tmp.path().join(crashed).exists(),
            "a crashed session's evidence must outlive its missing document"
        );
    }

    #[test]
    fn reclamation_never_touches_bookkeeping_directories() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().to_path_buf()).expect("manager");
        // `checkpoints` is not a session id and must survive being empty.
        let checkpoints = tmp.path().join("checkpoints");
        fs::create_dir_all(&checkpoints).expect("checkpoints dir");
        let not_a_session = tmp.path().join("some-user-folder");
        fs::create_dir_all(&not_a_session).expect("user dir");

        manager.cleanup_old_sessions().expect("cleanup");

        assert!(checkpoints.exists(), "checkpoints/ is not a session dir");
        assert!(
            not_a_session.exists(),
            "a name that is not a valid session id is not ours to remove"
        );
    }

    #[test]
    fn save_and_resume_reconstructs_closed_and_interrupted_approvals() {
        let tmp = tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let session = create_saved_session(
            &[make_test_message("user", "approval recovery")],
            "test-model",
            tmp.path(),
            0,
            None,
        );
        let session_id = session.metadata.id.clone();
        let store = ApprovalReceiptStore::new(sessions_dir);
        store
            .append(
                &session_id,
                &ApprovalReceipt::asked("tool-complete", "exec_shell"),
            )
            .expect("persist completed ask");
        store
            .append(
                &session_id,
                &ApprovalReceipt::decided("tool-complete", ApprovalOutcome::Denied),
            )
            .expect("persist completed decision");
        store
            .append(
                &session_id,
                &ApprovalReceipt::asked("tool-interrupted", "write_file"),
            )
            .expect("persist interrupted ask");

        manager.save_session(&session).expect("save session");
        let resumed = manager
            .load_session_snapshot(&session_id)
            .expect("resume session");
        let replay = ApprovalReplay::from_receipts(&resumed.approval_receipts)
            .expect("replay resumed approval evidence");

        assert_eq!(resumed.messages, session.messages);
        assert_eq!(replay.completed.len(), 1);
        assert_eq!(replay.completed[0].outcome, ApprovalOutcome::Denied);
        assert_eq!(replay.unmatched_asks.len(), 1);
        assert!(matches!(
            &replay.unmatched_asks[0],
            ApprovalReceipt::Asked { tool_call_id, .. } if tool_call_id == "tool-interrupted"
        ));
        assert_eq!(
            manager
                .replay_approvals(&session_id)
                .expect("replay canonical sidecar"),
            replay
        );
    }

    #[test]
    fn saved_history_keeps_execution_identity_in_snapshots_and_journal() {
        let tmp = tempdir().unwrap();
        let manager = SessionManager::new(tmp.path().join("sessions")).unwrap();
        let mut messages = vec![make_test_message("user", "inspect")];
        for execution in ["execution-a", "execution-b"] {
            messages.push(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "reused-provider-id".to_string(),
                    execution_id: Some(execution.to_string()),
                    name: "read".to_string(),
                    input: serde_json::json!({"path": "README.md"}),
                    caller: Some(codewhale_models::ToolCaller {
                        caller_type: "code_execution".to_string(),
                        tool_id: Some("provider-parent".to_string()),
                    }),
                    thought_signature: Some("provider-signature".to_string()),
                }],
            });
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "reused-provider-id".to_string(),
                    execution_id: Some(execution.to_string()),
                    content: format!("result for {execution}"),
                    is_error: None,
                    content_blocks: None,
                }],
            });
        }
        let session = create_saved_session(&messages, "test-model", tmp.path(), 0, None);
        manager.save_session(&session).unwrap();
        manager.save_checkpoint(&session).unwrap();
        let snapshot = manager.load_session_snapshot(&session.metadata.id).unwrap();
        let checkpoint = manager
            .load_session_checkpoint(&session.metadata.id)
            .unwrap()
            .unwrap();
        for restored in [snapshot, checkpoint] {
            assert_eq!(restored.messages, messages);
            assert_eq!(
                restored.journal.as_ref().unwrap().active_messages(false),
                messages
            );
            let first = restored.messages[1].content[0].tool_call_key();
            let second = restored.messages[3].content[0].tool_call_key();
            assert_ne!(first, second);
            assert_eq!(first, restored.messages[2].content[0].tool_call_key());
            assert_eq!(second, restored.messages[4].content[0].tool_call_key());
        }
    }

    #[test]
    fn approval_hydration_distinguishes_missing_and_present_logs_on_disk() {
        let ask = ApprovalReceipt::asked("receipt-hydration", "exec_shell");
        let decision = ApprovalReceipt::decided("receipt-hydration", ApprovalOutcome::ApprovedOnce);
        let embedded = vec![ask.clone(), decision];
        let mut prefix_with_torn_decision = serde_json::to_vec(&ask).unwrap();
        prefix_with_torn_decision.extend_from_slice(b"\n{\"phase\":\"decided\"");
        let cases = [
            ("missing", None, embedded.clone()),
            ("empty", Some(Vec::new()), Vec::new()),
            (
                "torn-first",
                Some(b"{\"phase\":\"asked\"".to_vec()),
                Vec::new(),
            ),
            (
                "asked-prefix",
                Some(prefix_with_torn_decision),
                vec![ask.clone()],
            ),
        ];
        for with_lock in [false, true] {
            for (name, log_bytes, expected) in &cases {
                let tmp = tempdir().unwrap();
                let sessions_dir = tmp.path().join("sessions");
                let manager = SessionManager::new(sessions_dir.clone()).unwrap();
                let mut session = create_saved_session(
                    &[make_test_message("user", "receipt recovery")],
                    "test-model",
                    tmp.path(),
                    0,
                    None,
                );
                session.approval_receipts = embedded.clone();
                let id = &session.metadata.id;
                // Persist legacy embedded evidence while no sidecar exists.
                let saved_path = manager.save_session(&session).unwrap();
                let checkpoint_path = manager.save_checkpoint(&session).unwrap();
                let saved_before = fs::read(&saved_path).unwrap();
                let checkpoint_before = fs::read(&checkpoint_path).unwrap();
                let store = ApprovalReceiptStore::new(sessions_dir.clone());
                let log_path = sessions_dir.join(id).join("approval_receipts.jsonl");
                if with_lock {
                    // Exercise the live-writer lock path as well as an imported
                    // log without a lock, then simulate its final on-disk bytes.
                    store.append(id, &ask).unwrap();
                }
                if let Some(bytes) = log_bytes {
                    fs::create_dir_all(log_path.parent().unwrap()).unwrap();
                    fs::write(&log_path, bytes).unwrap();
                } else if with_lock {
                    fs::remove_file(&log_path).unwrap();
                }
                let resumed = manager.load_session_snapshot(id).unwrap();
                let checkpoint = manager.load_session_checkpoint(id).unwrap().unwrap();
                for loaded in [&resumed, &checkpoint] {
                    assert_eq!(
                        &loaded.approval_receipts, expected,
                        "{name}, lock={with_lock}"
                    );
                    assert_eq!(loaded.messages, session.messages);
                    let replay = ApprovalReplay::from_receipts(&loaded.approval_receipts).unwrap();
                    assert_eq!(
                        replay.completed.len(),
                        usize::from(*name == "missing"),
                        "{name}"
                    );
                    assert_eq!(
                        replay.unmatched_asks.len(),
                        usize::from(*name == "asked-prefix"),
                        "{name}"
                    );
                }
                assert_eq!(
                    fs::read(&saved_path).unwrap(),
                    saved_before,
                    "read-only snapshot"
                );
                assert_eq!(
                    fs::read(&checkpoint_path).unwrap(),
                    checkpoint_before,
                    "read-only checkpoint"
                );
                // Saving the old in-memory snapshot must hydrate too, so stale
                // approvals cannot be reintroduced into either persisted file.
                manager.save_session(&session).unwrap();
                manager.save_checkpoint(&session).unwrap();
                for path in [&saved_path, &checkpoint_path] {
                    let saved: SavedSession =
                        serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
                    assert_eq!(
                        &saved.approval_receipts, expected,
                        "persisted {name}, lock={with_lock}"
                    );
                }
                match log_bytes {
                    Some(bytes) => assert_eq!(fs::read(&log_path).unwrap(), *bytes, "{name}"),
                    None => assert!(!log_path.exists(), "missing log must not be created"),
                }
            }
        }
    }

    #[test]
    fn approval_hydration_rejects_complete_corruption_without_replacing_saved_evidence() {
        let ask = ApprovalReceipt::asked("receipt-invalid", "exec_shell");
        let decision = ApprovalReceipt::decided("receipt-invalid", ApprovalOutcome::ApprovedOnce);
        let embedded = vec![ask.clone(), decision.clone()];
        let mut interior = b"not-json\n".to_vec();
        interior.extend_from_slice(&serde_json::to_vec(&ask).unwrap());
        interior.push(b'\n');
        let mut orphan_decision = serde_json::to_vec(&decision).unwrap();
        orphan_decision.push(b'\n');
        for bytes in [
            b"not-json\n".to_vec(),
            interior,
            b"{\"phase\":\"unknown\"}".to_vec(),
            orphan_decision,
        ] {
            let tmp = tempdir().unwrap();
            let sessions_dir = tmp.path().join("sessions");
            let manager = SessionManager::new(sessions_dir.clone()).unwrap();
            let mut session = create_saved_session(
                &[make_test_message("user", "invalid receipt recovery")],
                "test-model",
                tmp.path(),
                0,
                None,
            );
            session.approval_receipts = embedded.clone();
            let id = &session.metadata.id;
            let saved_path = manager.save_session(&session).unwrap();
            let checkpoint_path = manager.save_checkpoint(&session).unwrap();
            let saved_before = fs::read(&saved_path).unwrap();
            let checkpoint_before = fs::read(&checkpoint_path).unwrap();
            ApprovalReceiptStore::new(sessions_dir.clone())
                .append(id, &ask)
                .unwrap();
            let log_path = sessions_dir.join(id).join("approval_receipts.jsonl");
            fs::write(&log_path, &bytes).unwrap();
            assert_eq!(
                manager.load_session_snapshot(id).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(
                manager.load_session_checkpoint(id).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(
                manager.save_session(&session).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(
                manager.save_checkpoint(&session).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(fs::read(&saved_path).unwrap(), saved_before);
            assert_eq!(fs::read(&checkpoint_path).unwrap(), checkpoint_before);
            assert_eq!(fs::read(&log_path).unwrap(), bytes);
        }
    }

    #[test]
    fn session_boot_owner_stamps_only_the_creating_instance() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().to_path_buf()).expect("manager");
        let workspace = tmp.path().join("ws");

        // A record this instance creates is stamped with this boot id and is
        // therefore not prior-instance work.
        write_session_record(&manager, "mine", &workspace, Utc::now());
        assert_eq!(
            manager.session_boot_owner("mine").as_deref(),
            Some(current_session_boot_id())
        );
        assert!(!manager.session_from_prior_instance("mine"));

        // An id with no durable record at all is this instance's own
        // not-yet-persisted session.
        assert!(!manager.session_from_prior_instance("unsaved"));

        // A record stamped by another boot id stays owned by that instance,
        // even after this instance re-serializes it (crash recovery must not
        // re-badge restored work as ours).
        manager
            .record_session_boot_owner("theirs", "boot_other_instance")
            .expect("stamp");
        write_session_record(&manager, "theirs", &workspace, Utc::now());
        assert_eq!(
            manager.session_boot_owner("theirs").as_deref(),
            Some("boot_other_instance")
        );
        assert!(manager.session_from_prior_instance("theirs"));

        // A legacy record with no marker is classified as prior-instance
        // work, and a later re-save keeps it unclaimed.
        write_session_record(&manager, "legacy", &workspace, Utc::now());
        manager.clear_session_boot_owner("legacy");
        assert!(manager.session_from_prior_instance("legacy"));
        write_session_record(&manager, "legacy", &workspace, Utc::now());
        assert!(manager.session_from_prior_instance("legacy"));

        // Deleting the record drops its marker.
        manager.delete_session("theirs").expect("delete");
        assert_eq!(manager.session_boot_owner("theirs"), None);
    }

    #[test]
    fn session_boot_owner_sidecar_never_lists_as_a_session() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().to_path_buf()).expect("manager");
        write_session_record(&manager, "real", &tmp.path().join("ws"), Utc::now());
        assert!(manager.session_boot_owners_path().exists());
        let listed = manager.list_sessions().expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "real");
        // The reserved stem cannot be claimed as a session id either.
        assert!(manager.load_session("session_boot_owners").is_err());
    }

    #[test]
    fn test_session_manager_new() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        assert!(tmp.path().join("sessions").exists());
        let _ = manager;
    }

    #[test]
    fn test_save_and_load_session() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");

        let messages = vec![
            make_test_message("user", "Hello!"),
            make_test_message("assistant", "Hi there!"),
        ];

        let session = create_saved_session(&messages, "test-model", tmp.path(), 100, None);
        let session_id = session.metadata.id.clone();

        manager.save_session(&session).expect("save");

        let loaded = manager.load_session(&session_id).expect("load");
        assert_eq!(loaded.metadata.id, session_id);
        assert_eq!(loaded.messages.len(), 2);
    }


    #[test]
    fn runtime_snapshot_load_preserves_in_flight_tool_call() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                execution_id: None,
                id: "call-in-flight".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "README.md"}),
                caller: None,
                thought_signature: None,
            }],
        }];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 0, None);
        let session_id = session.metadata.id.clone();
        manager.save_session(&session).expect("save");

        let loaded = manager
            .load_session_snapshot(&session_id)
            .expect("snapshot load");

        assert_eq!(loaded.messages, messages);
        assert_eq!(loaded.metadata.message_count, 1);
        assert!(!loaded.messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::ToolResult { content, .. }
                        if content.contains("crashed_and_repaired")
                )
            })
        }));
    }

    #[test]
    fn explicit_session_recovery_is_reported_and_idempotent_after_save() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                execution_id: None,
                id: "call-crashed".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "README.md"}),
                caller: None,
                thought_signature: None,
            }],
        }];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 0, None);
        let session_id = session.metadata.id.clone();
        manager.save_session(&session).expect("save");

        let recovered = manager
            .recover_session_for_resume(&session_id)
            .expect("recover");
        assert!(recovered.changed);
        assert_eq!(recovered.repaired_call_count, 1);
        assert_eq!(recovered.duplicate_result_count, 0);
        assert_eq!(recovered.orphan_result_count, 0);
        manager
            .save_session(&recovered.session)
            .expect("persist recovery");

        let second = manager
            .recover_session_for_resume(&session_id)
            .expect("recover twice");
        assert!(!second.changed);
        assert_eq!(second.repaired_call_count, 0);
        assert_eq!(second.session.messages, recovered.session.messages);
    }

    #[test]
    fn resume_session_persists_repair_once() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                execution_id: None,
                id: "call-crashed".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "README.md"}),
                caller: None,
                thought_signature: None,
            }],
        }];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 0, None);
        let session_id = session.metadata.id.clone();
        manager.save_session(&session).expect("save");

        let first = manager.resume_session(&session_id).expect("first resume");
        assert!(first.changed);
        assert_eq!(first.repaired_call_count, 1);

        // The repair is already durable: a second resume finds a clean record
        // instead of re-running and re-logging the same repair on every load.
        let second = manager.resume_session(&session_id).expect("second resume");
        assert!(!second.changed);
        assert_eq!(second.repaired_call_count, 0);
        assert_eq!(second.session.messages, first.session.messages);
    }

    #[test]
    fn load_session_repairs_dangling_tool_call_with_visible_receipt() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                execution_id: None,
                id: "call-crashed".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "README.md"}),
                caller: None,
                thought_signature: None,
            }],
        }];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 0, None);
        let session_id = session.metadata.id.clone();
        manager.save_session(&session).expect("save");

        let loaded = manager.load_session(&session_id).expect("load");

        assert_eq!(loaded.metadata.message_count, loaded.messages.len());
        assert!(loaded.messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error: Some(true),
                        ..
                    } if tool_use_id == "call-crashed" && content.contains("crashed_and_repaired")
                )
            })
        }));
        assert_eq!(
            loaded.journal.as_ref().map(SessionJournal::to_messages),
            Some(loaded.messages.clone()),
            "the append-only journal must follow the repaired active branch"
        );
        assert!(loaded.messages.iter().any(|message| {
            (message.role == "assistant"
                || message.role == codewhale_models::INTERRUPTED_ASSISTANT_ROLE)
                && message.content.iter().any(|block| {
                    matches!(
                        block,
                        ContentBlock::Text { text, .. }
                            if text.contains("[tool_history_repair]")
                    )
                })
        }));
    }


    #[test]
    fn save_session_preserves_large_tool_outputs_for_cache_fidelity() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let raw = "RAW_SESSION_SENTINEL\n".repeat(2_000);
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    execution_id: None,
                    id: "call-big".to_string(),
                    name: "exec_shell".to_string(),
                    input: serde_json::json!({"command": "cargo test -p codewhale-tui"}),
                    caller: None,
                    thought_signature: None,
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call-big".to_string(),
                    content: raw.clone(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];
        let mut session = create_saved_session(&messages, "test-model", tmp.path(), 100, None);
        session.artifacts.push(codewhale_runtime::artifacts::ArtifactRecord {
            id: "art_call-big".to_string(),
            kind: codewhale_runtime::artifacts::ArtifactKind::ToolOutput,
            session_id: session.metadata.id.clone(),
            tool_call_id: "call-big".to_string(),
            tool_name: "exec_shell".to_string(),
            created_at: Utc::now(),
            byte_size: raw.len() as u64,
            preview: "checking crate ... error[E0425]".to_string(),
            storage_path: PathBuf::from("artifacts/art_call-big.txt"),
        });

        let path = manager.save_session(&session).expect("save");
        let persisted_json = fs::read_to_string(path).expect("read persisted session");
        // Raw output is preserved in-session so resume can hit the LLM cache.
        assert!(persisted_json.contains("RAW_SESSION_SENTINEL"));

        let loaded = manager.load_session(&session.metadata.id).expect("load");
        let ContentBlock::ToolResult { content, .. } = &loaded.messages[1].content[0] else {
            panic!("expected loaded tool result");
        };
        // Loaded session retains the original output for cache fidelity.
        assert!(content.contains("RAW_SESSION_SENTINEL"));
        assert!(!content.contains("[TOOL_OUTPUT_RECEIPT]"));
    }

    #[test]
    fn load_session_preserves_legacy_large_tool_outputs_for_cache_fidelity() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let raw = "RAW_LEGACY_RESUME_SENTINEL\n".repeat(2_000);
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    execution_id: None,
                    id: "call-legacy".to_string(),
                    name: "exec_shell".to_string(),
                    input: serde_json::json!({"command": "cargo check"}),
                    caller: None,
                    thought_signature: None,
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call-legacy".to_string(),
                    content: raw.clone(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];
        let mut session = create_saved_session(&messages, "test-model", tmp.path(), 100, None);
        session.artifacts.push(codewhale_runtime::artifacts::ArtifactRecord {
            id: "art_call-legacy".to_string(),
            kind: codewhale_runtime::artifacts::ArtifactKind::ToolOutput,
            session_id: session.metadata.id.clone(),
            tool_call_id: "call-legacy".to_string(),
            tool_name: "exec_shell".to_string(),
            created_at: Utc::now(),
            byte_size: raw.len() as u64,
            preview: "cargo check output".to_string(),
            storage_path: PathBuf::from("artifacts/art_call-legacy.txt"),
        });
        let path = manager
            .validated_session_path(&session.metadata.id)
            .expect("path");
        fs::write(
            &path,
            serde_json::to_string_pretty(&session).expect("serialize legacy session"),
        )
        .expect("write legacy raw session");
        assert!(
            fs::read_to_string(&path)
                .expect("read legacy raw")
                .contains("RAW_LEGACY_RESUME_SENTINEL")
        );

        let loaded = manager.load_session(&session.metadata.id).expect("load");
        let ContentBlock::ToolResult { content, .. } = &loaded.messages[1].content[0] else {
            panic!("expected loaded tool result");
        };
        // Loaded session preserves original output so resume can hit the LLM cache.
        assert!(content.contains("RAW_LEGACY_RESUME_SENTINEL"));
        assert!(!content.contains("[TOOL_OUTPUT_RECEIPT]"));
    }

    #[test]
    fn test_list_sessions() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");

        // Create a few sessions
        for i in 0..3 {
            let messages = vec![make_test_message("user", &format!("Session {i}"))];
            let session = create_saved_session(&messages, "test-model", tmp.path(), 100, None);
            manager.save_session(&session).expect("save");
        }

        let sessions = manager.list_sessions().expect("list");
        assert_eq!(sessions.len(), 3);
    }

    #[test]
    fn default_manager_copies_legacy_sessions_when_primary_already_exists() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
        let _codewhale_home = crate::test_support::EnvVarGuard::remove("CODEWHALE_HOME");

        let primary_sessions = home.join(".codewhale").join("sessions");
        let legacy_sessions = home.join(".deepseek").join("sessions");
        fs::create_dir_all(&primary_sessions).expect("primary sessions");
        fs::create_dir_all(&legacy_sessions).expect("legacy sessions");
        fs::create_dir_all(legacy_sessions.join("checkpoints")).expect("legacy checkpoints");
        fs::write(
            legacy_sessions.join("checkpoints").join("latest.json"),
            "{}",
        )
        .expect("legacy checkpoint");

        let mut legacy_session = create_saved_session(
            &[make_test_message("user", "find my old session")],
            "test-model",
            tmp.path(),
            100,
            None,
        );
        legacy_session.metadata.id = "legacy-visible".to_string();
        legacy_session.metadata.title = "session from legacy home".to_string();
        fs::write(
            legacy_sessions.join("legacy-visible.json"),
            serde_json::to_string_pretty(&legacy_session).expect("serialize legacy session"),
        )
        .expect("write legacy session");

        let manager = SessionManager::default_location().expect("default manager");
        assert_eq!(manager.sessions_dir(), primary_sessions.as_path());
        assert!(primary_sessions.join("legacy-visible.json").exists());
        assert!(!primary_sessions.join("checkpoints").exists());
        assert!(legacy_sessions.join("legacy-visible.json").exists());

        let sessions = manager.list_sessions().expect("list");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "legacy-visible");
    }

    #[test]
    fn legacy_session_copy_never_overwrites_primary_session() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
        let _codewhale_home = crate::test_support::EnvVarGuard::remove("CODEWHALE_HOME");

        let primary_sessions = home.join(".codewhale").join("sessions");
        let legacy_sessions = home.join(".deepseek").join("sessions");
        fs::create_dir_all(&primary_sessions).expect("primary sessions");
        fs::create_dir_all(&legacy_sessions).expect("legacy sessions");

        let primary_path = primary_sessions.join("same-id.json");
        fs::write(&primary_path, "primary data wins").expect("write primary session");
        fs::write(
            legacy_sessions.join("same-id.json"),
            "legacy data must not overwrite",
        )
        .expect("write legacy session");

        let dir = default_sessions_dir().expect("default session dir");
        assert_eq!(dir, primary_sessions);
        assert_eq!(
            fs::read_to_string(primary_path).expect("read primary session"),
            "primary data wins"
        );
    }

    #[test]
    fn explicit_codewhale_home_disables_legacy_session_copy() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let explicit_home = tmp.path().join("explicit-codewhale");
        let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
        let _codewhale_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &explicit_home);

        let legacy_sessions = home.join(".deepseek").join("sessions");
        fs::create_dir_all(&legacy_sessions).expect("legacy sessions");
        fs::write(legacy_sessions.join("legacy-visible.json"), "{}").expect("write legacy session");

        let dir = default_sessions_dir().expect("default session dir");
        assert_eq!(dir, explicit_home.join("sessions"));
        assert!(!dir.join("legacy-visible.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_codewhale_home_is_still_an_explicit_session_boundary() {
        use std::os::unix::ffi::OsStringExt;

        let _lock = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let explicit_home = tmp.path().join(std::ffi::OsString::from_vec(
            b"codewhale-\xff-home".to_vec(),
        ));
        let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
        let _codewhale_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &explicit_home);

        let legacy_sessions = home.join(".deepseek").join("sessions");
        fs::create_dir_all(&legacy_sessions).expect("legacy sessions");
        fs::write(legacy_sessions.join("ambient.json"), "ambient").expect("ambient legacy session");
        let safe_primary = tmp.path().join("safe-primary");
        fs::create_dir_all(&safe_primary).expect("safe primary");

        assert_eq!(
            merge_missing_legacy_session_entries(&safe_primary).expect("merge decision"),
            0
        );
        assert!(!safe_primary.join("ambient.json").exists());
    }

    #[test]
    fn latest_session_for_workspace_ignores_newer_other_directory() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let workspace_a = tmp.path().join("aa").join("aaa");
        let workspace_b = tmp.path().join("bb").join("bbb");
        fs::create_dir_all(&workspace_a).expect("mkdir workspace a");
        fs::create_dir_all(&workspace_b).expect("mkdir workspace b");
        fs::create_dir_all(tmp.path().join(".git")).expect("mkdir invalid git boundary");

        write_session_record(
            &manager,
            "current-workspace",
            &workspace_a,
            Utc::now() - chrono::Duration::minutes(10),
        );
        write_session_record(&manager, "other-workspace", &workspace_b, Utc::now());

        let global = manager
            .list_sessions()
            .expect("list")
            .into_iter()
            .next()
            .expect("global latest");
        assert_eq!(global.id, "other-workspace");

        let scoped = manager
            .get_latest_session_for_workspace(&workspace_a)
            .expect("latest for workspace")
            .expect("scoped latest");
        assert_eq!(scoped.id, "current-workspace");
    }

    #[test]
    fn latest_session_for_workspace_ignores_invalid_parent_git_marker() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let workspace_a = tmp.path().join("aa").join("aaa");
        let workspace_b = tmp.path().join("bb").join("bbb");
        fs::create_dir_all(&workspace_a).expect("mkdir workspace a");
        fs::create_dir_all(&workspace_b).expect("mkdir workspace b");
        fs::create_dir_all(tmp.path().join(".git")).expect("mkdir invalid git marker");

        write_session_record(
            &manager,
            "current-workspace",
            &workspace_a,
            Utc::now() - chrono::Duration::minutes(10),
        );
        write_session_record(&manager, "other-workspace", &workspace_b, Utc::now());

        let scoped = manager
            .get_latest_session_for_workspace(&workspace_a)
            .expect("latest for workspace")
            .expect("scoped latest");
        assert_eq!(scoped.id, "current-workspace");
    }

    #[test]
    fn latest_session_for_workspace_matches_same_git_repository() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let repo = tmp.path().join("repo");
        let repo_app = repo.join("apps").join("client");
        let repo_crate = repo.join("crates").join("server");
        let other_repo = tmp.path().join("other").join("project");
        fs::create_dir_all(repo.join(".git")).expect("mkdir .git");
        fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/main\n").expect("write HEAD");
        fs::create_dir_all(&repo_app).expect("mkdir repo app");
        fs::create_dir_all(&repo_crate).expect("mkdir repo crate");
        fs::create_dir_all(&other_repo).expect("mkdir other repo");

        write_session_record(
            &manager,
            "same-repo",
            &repo_app,
            Utc::now() - chrono::Duration::minutes(5),
        );
        write_session_record(&manager, "other-repo", &other_repo, Utc::now());

        let scoped = manager
            .get_latest_session_for_workspace(&repo_crate)
            .expect("latest for workspace")
            .expect("same repo latest");
        assert_eq!(scoped.id, "same-repo");
    }

    #[test]
    fn latest_session_for_workspace_skips_empty_auto_created_session() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let workspace = tmp.path().join("repo");
        fs::create_dir_all(&workspace).expect("mkdir workspace");

        write_session_record(
            &manager,
            "interrupted-user-turn",
            &workspace,
            Utc::now() - chrono::Duration::minutes(5),
        );
        write_empty_session_record(&manager, "empty-auto-shell", &workspace, Utc::now());

        let global = manager
            .list_sessions()
            .expect("list")
            .into_iter()
            .next()
            .expect("global latest");
        assert_eq!(global.id, "empty-auto-shell");

        let scoped = manager
            .get_latest_session_for_workspace(&workspace)
            .expect("latest for workspace")
            .expect("scoped latest");
        assert_eq!(scoped.id, "interrupted-user-turn");
    }

    #[test]
    fn test_load_by_prefix() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");

        let messages = vec![make_test_message("user", "Test session")];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 100, None);
        let prefix = truncate_id(&session.metadata.id).to_string();
        manager.save_session(&session).expect("save");

        let loaded = manager.load_session_by_prefix(&prefix).expect("load");
        assert_eq!(loaded.messages.len(), 1);
    }

    #[test]
    fn test_delete_session() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");

        let messages = vec![make_test_message("user", "To be deleted")];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 100, None);
        let session_id = session.metadata.id.clone();

        manager.save_session(&session).expect("save");
        assert!(manager.load_session(&session_id).is_ok());

        manager.delete_session(&session_id).expect("delete");
        assert!(manager.load_session(&session_id).is_err());
    }

    #[test]
    fn delete_session_removes_artifact_directory() {
        let tmp = tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("new");

        let session = create_saved_session(
            &[make_test_message("user", "artifact session")],
            "test-model",
            tmp.path(),
            100,
            None,
        );
        let session_id = session.metadata.id.clone();
        let artifact_dir = sessions_dir.join(&session_id).join("artifacts");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(artifact_dir.join("art_call.txt"), "raw output").expect("artifact file");

        manager.save_session(&session).expect("save");
        manager.delete_session(&session_id).expect("delete");

        assert!(!sessions_dir.join(format!("{session_id}.json")).exists());
        assert!(!sessions_dir.join(&session_id).exists());
    }

    #[test]
    fn delete_session_removes_its_work_graph_import_archive() {
        let tmp = tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("new");
        let session = create_saved_session(
            &[make_test_message("user", "archived transcript")],
            "test-model",
            tmp.path(),
            0,
            None,
        );
        let session_id = session.metadata.id.clone();
        let path = manager.save_session(&session).expect("save");
        let archive_dir = sessions_dir.join(WORK_GRAPH_IMPORT_ARCHIVE_DIR);
        fs::create_dir_all(&archive_dir).expect("archive dir");
        let archive = archive_dir.join(path.file_name().expect("file name"));
        fs::copy(&path, &archive).expect("archive copy");
        let other = archive_dir.join("other-session.json");
        fs::write(&other, "{}").expect("other archive");

        manager.delete_session(&session_id).expect("delete");

        assert!(
            !archive.exists(),
            "the deleted transcript's copy is removed"
        );
        assert!(other.exists(), "other sessions' copies are untouched");
    }

    /// Retention keeps a session's crash-recovery checkpoint but retires the
    /// ordinary snapshot, and the pre-import copy is an ordinary snapshot.
    #[test]
    fn retention_removes_the_import_archive_copy_of_a_recoverable_session() {
        let tmp = tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("new");
        let session = create_saved_session(
            &[make_test_message("user", "archived transcript")],
            "test-model",
            tmp.path(),
            0,
            None,
        );
        let session_id = session.metadata.id.clone();
        let path = manager.save_session(&session).expect("save");
        manager.save_checkpoint(&session).expect("checkpoint");
        let archive_dir = sessions_dir.join(WORK_GRAPH_IMPORT_ARCHIVE_DIR);
        fs::create_dir_all(&archive_dir).expect("archive dir");
        let archive = archive_dir.join(path.file_name().expect("file name"));
        fs::copy(&path, &archive).expect("archive copy");

        manager
            .remove_session(&session_id, SessionRemoval::Retention)
            .expect("retention");

        assert!(!path.exists(), "the ordinary snapshot is retired");
        assert!(!archive.exists(), "so is its pre-import copy");
        assert!(
            manager
                .load_session_checkpoint(&session_id)
                .expect("checkpoint")
                .is_some(),
            "crash recovery is kept"
        );
    }

    /// Deletion does not follow a linked archive directory to a same-named
    /// file somewhere else.
    #[cfg(unix)]
    #[test]
    fn delete_does_not_follow_a_linked_import_archive() {
        let tmp = tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("new");
        let session = create_saved_session(
            &[make_test_message("user", "transcript")],
            "test-model",
            tmp.path(),
            0,
            None,
        );
        let session_id = session.metadata.id.clone();
        let path = manager.save_session(&session).expect("save");
        let elsewhere = tmp.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).expect("elsewhere");
        let unrelated = elsewhere.join(path.file_name().expect("file name"));
        fs::write(&unrelated, "{}").expect("unrelated file");
        std::os::unix::fs::symlink(&elsewhere, sessions_dir.join(WORK_GRAPH_IMPORT_ARCHIVE_DIR))
            .expect("link");

        manager.delete_session(&session_id).expect("delete");

        assert!(!path.exists());
        assert!(unrelated.exists(), "a file behind the link is untouched");
    }

    #[test]
    fn a_session_file_naming_another_session_is_refused() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let session = create_saved_session(&[], "test-model", tmp.path(), 0, None);
        manager.save_session(&session).expect("save");
        let id = session.metadata.id.clone();
        let source = manager.validated_session_path(&id).expect("path");
        std::fs::copy(&source, source.with_file_name("impostor.json")).expect("copy");

        assert_eq!(manager.load_session(&id).expect("own id").metadata.id, id);
        // The stray copy does not make the real session ambiguous.
        assert_eq!(
            manager
                .load_session_by_prefix(&id)
                .expect("resume by id with a copy present")
                .metadata
                .id,
            id
        );
        let err = manager
            .load_session("impostor")
            .expect_err("mismatched id must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn test_session_id_rejects_invalid_characters() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");

        let err = manager
            .load_session("../outside")
            .expect_err("invalid id should fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

        let err = manager
            .delete_session("sess bad")
            .expect_err("invalid id should fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn test_session_manager_rejects_relative_traversal_dir() {
        let err = SessionManager::new(PathBuf::from("../sessions"))
            .expect_err("relative traversal directory should fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn test_truncate_title() {
        assert_eq!(truncate_title("Short", 50), "Short");
        assert_eq!(
            truncate_title("This is a very long title that should be truncated", 20),
            "This is a very lo..."
        );
        assert_eq!(truncate_title("Line 1\nLine 2", 50), "Line 1");
    }

    #[test]
    fn extract_user_prompt_strips_turn_meta_prefix() {
        assert_eq!(
            extract_user_prompt("<turn_meta>{\"cache\":\"x\"}</turn_meta>\nReal prompt"),
            "Real prompt"
        );
        assert_eq!(extract_user_prompt("  Real prompt"), "Real prompt");
        assert_eq!(
            extract_user_prompt("<turn_meta>{\"unterminated\":true}\nReal prompt"),
            "{\"unterminated\":true}\nReal prompt"
        );
    }

    #[test]
    fn create_saved_session_uses_prompt_after_turn_meta_for_title() {
        let tmp = tempdir().expect("tempdir");
        let messages = vec![make_test_message(
            "user",
            "<turn_meta>{\"cache\":\"x\"}</turn_meta>\nFix the session picker history pane",
        )];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 100, None);
        assert_eq!(
            session.metadata.title,
            "Fix the session picker history pane"
        );
    }

    #[test]
    fn create_saved_session_skips_runtime_handoffs_when_deriving_title() {
        let tmp = tempdir().expect("tempdir");
        // Operate/automation sessions start with runtime-owned control traffic
        // as the first `user` message. The auto-title must come from the real
        // prompt that follows, never from the internal envelope.
        let messages = vec![
            crate::runtime_handoff::operate_contract_runtime_message(),
            make_test_message("user", "Ship the session-title fix"),
        ];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 100, None);
        assert_eq!(session.metadata.title, "Ship the session-title fix");
        assert!(
            !session.metadata.title.contains("codewhale:runtime"),
            "internal envelope leaked into the session title: {}",
            session.metadata.title
        );
    }

    #[test]
    fn create_saved_session_with_only_runtime_traffic_keeps_placeholder_title() {
        let tmp = tempdir().expect("tempdir");
        let waiting = crate::runtime_handoff::waiting_for_subagents_runtime_message(2);
        let restored =
            crate::runtime_handoff::project_owned_messages_for_restore(vec![waiting.clone()])
                .into_iter()
                .next()
                .expect("restore projection yields one message");
        // Runtime handoffs must stay out of the auto-title. Exercise the
        // Operate contract, a waiting/restored
        // sub-agent checkpoint, and a background shell completion.
        let messages = vec![
            crate::runtime_handoff::operate_contract_runtime_message(),
            waiting,
            restored,
            crate::runtime_handoff::shell_completion_runtime_message(&[]),
        ];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 100, None);
        assert_eq!(session.metadata.title, DEFAULT_SESSION_TITLE);
        assert!(
            !session.metadata.title.contains("codewhale:runtime"),
            "internal envelope leaked into the session title: {}",
            session.metadata.title
        );
    }

    #[test]
    fn import_foreign_derives_title_from_the_first_real_user_message() {
        let tmp = tempdir().expect("tempdir");
        // Importing a session whose transcript opens with the Operate contract
        // (the shape this bug produced on export) must not re-derive the
        // envelope as the imported title.
        let container = container_with(
            vec![
                crate::runtime_handoff::operate_contract_runtime_message(),
                make_test_message("user", "Fix the session picker"),
            ],
            tmp.path(),
        );
        let imported = crate::session_manager::SavedSession::import_foreign(
            container,
            tmp.path().to_path_buf(),
            "test-model".to_string(),
        )
        .expect("import succeeds");
        assert_eq!(imported.metadata.title, "Fix the session picker");
        assert!(
            !imported.metadata.title.contains("codewhale:runtime"),
            "internal envelope leaked into the imported session title: {}",
            imported.metadata.title
        );
    }

    #[test]
    fn import_foreign_keeps_placeholder_when_only_runtime_traffic() {
        let tmp = tempdir().expect("tempdir");
        let container = container_with(
            vec![crate::runtime_handoff::operate_contract_runtime_message()],
            tmp.path(),
        );
        let imported = crate::session_manager::SavedSession::import_foreign(
            container,
            tmp.path().to_path_buf(),
            "test-model".to_string(),
        )
        .expect("import succeeds");
        assert_eq!(imported.metadata.title, DEFAULT_SESSION_TITLE);
    }