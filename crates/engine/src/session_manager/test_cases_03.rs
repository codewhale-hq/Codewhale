

    #[test]
    fn title_derivation_skips_current_and_legacy_runtime_provenance() {
        let tmp = tempdir().expect("tempdir");
        for leading_metadata in [false, true] {
            let mut runtime = make_test_message("user", "Internal diagnostic update");
            let metadata = ContentBlock::Text {
                text: "<turn_meta>\nInput provenance: runtime (non-authoritative)\n</turn_meta>"
                    .to_string(),
                cache_control: None,
            };
            if leading_metadata {
                runtime.content.insert(0, metadata);
            } else {
                runtime.content.push(metadata);
            }
            let messages = vec![
                runtime,
                make_test_message("user", "Fix the diagnostic display"),
            ];
            let session = create_saved_session(&messages, "test-model", tmp.path(), 0, None);
            assert_eq!(session.metadata.title, "Fix the diagnostic display");
            let imported = SavedSession::import_foreign(
                container_with(messages, tmp.path()),
                tmp.path().to_path_buf(),
                "test-model".to_string(),
            )
            .expect("import succeeds");
            assert_eq!(imported.metadata.title, "Fix the diagnostic display");
        }
    }

    #[test]
    fn title_derivation_keeps_user_authored_runtime_example() {
        let tmp = tempdir().expect("tempdir");
        let messages = vec![make_test_message(
            "user",
            "<codewhale:runtime_event> example",
        )];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 0, None);
        assert_eq!(session.metadata.title, "<codewhale:runtime_event> example");
    }

    /// The exact bytes `SessionManager::load_session_metadata` reads.
    fn session_bytes(session: &SavedSession, stored_title: &str) -> Vec<u8> {
        let mut stale = session.clone();
        stale.metadata.title = stored_title.to_string();
        serde_json::to_vec(&stale).expect("serialize session")
    }

    fn loaded_title(session: &SavedSession, stored_title: &str) -> String {
        let buf = session_bytes(session, stored_title);
        let mut metadata = extract_top_level_metadata(&buf).expect("metadata extractable");
        assert_eq!(metadata.title, stored_title);
        apply_legacy_title_recovery(&mut metadata, &buf);
        metadata.title
    }

    /// What the superseded derivation stored for an Operate-contract session:
    /// the first line of the engine envelope, cut at 50 characters.
    fn legacy_operate_title() -> String {
        let message = crate::runtime_handoff::operate_contract_runtime_message();
        let ContentBlock::Text { text, .. } = &message.content[0] else {
            panic!("operate contract opens with text");
        };
        truncate_title(text, 50)
    }

    #[test]
    fn legacy_runtime_titles_recover_the_real_user_prompt() {
        let tmp = tempdir().expect("tempdir");
        let messages = vec![
            crate::runtime_handoff::operate_contract_runtime_message(),
            make_test_message("user", "Fix the diagnostic display"),
        ];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 0, None);
        let stored = legacy_operate_title();
        assert!(
            stored.starts_with("<codewhale:runtime_event kind="),
            "{stored:?}",
        );
        assert_eq!(
            loaded_title(&session, &stored),
            "Fix the diagnostic display"
        );
    }

    #[test]
    fn legacy_recovery_leaves_renames_and_user_authored_titles_alone() {
        let tmp = tempdir().expect("tempdir");
        let messages = vec![
            crate::runtime_handoff::operate_contract_runtime_message(),
            make_test_message("user", "Fix the diagnostic display"),
        ];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 0, None);
        // Renames win, including ones that open with `<` and so pay for the
        // message scan: provenance is proven, never guessed from the shape.
        for rename in [
            "Operate contract",
            "<my own angle-bracket title>",
            "<codewhale:runtime_event kind=\"operate_contract\" but renamed by me",
        ] {
            assert_eq!(loaded_title(&session, rename), rename);
        }
    }

    #[test]
    fn a_user_who_types_an_attributed_envelope_keeps_their_title() {
        // The one case the earlier substring rule got wrong. The engine's
        // envelope carries a runtime provenance line; a person's message does
        // not, and the existing classifier is what tells them apart — so this
        // title is theirs and survives.
        let tmp = tempdir().expect("tempdir");
        let typed = "<codewhale:runtime_event kind=\"operate_contract\" visibility=\"internal\">";
        let messages = vec![make_test_message("user", typed)];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 0, None);
        let stored = truncate_title(typed, 50);
        assert_eq!(session.metadata.title, stored);
        assert_eq!(loaded_title(&session, &stored), stored);
    }

    #[test]
    fn legacy_recovery_names_a_runtime_only_session_by_the_default() {
        // Nothing but runtime traffic: there is no user prompt to recover, and
        // the array ended inside the read, so the neutral default is provable.
        let tmp = tempdir().expect("tempdir");
        let messages = vec![crate::runtime_handoff::operate_contract_runtime_message()];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 0, None);
        assert_eq!(
            loaded_title(&session, &legacy_operate_title()),
            DEFAULT_SESSION_TITLE
        );
    }

    #[test]
    fn legacy_recovery_keeps_the_stored_title_when_the_read_was_truncated() {
        // A prefix cut before the user's turn must not be read as "this
        // conversation has no prompt".
        let tmp = tempdir().expect("tempdir");
        let messages = vec![
            crate::runtime_handoff::operate_contract_runtime_message(),
            make_test_message("user", "Fix the diagnostic display"),
        ];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 0, None);
        let stored = legacy_operate_title();
        let full = session_bytes(&session, &stored);
        let messages_at = full
            .windows(10)
            .position(|w| w == b"\"messages\"")
            .expect("messages key present");
        let cut = &full[..messages_at + 40];
        let mut metadata = extract_top_level_metadata(cut).expect("metadata precedes messages");
        apply_legacy_title_recovery(&mut metadata, cut);
        assert_eq!(metadata.title, stored, "a truncated read must not rename");
    }

    #[test]
    fn ordinary_titles_never_pay_for_the_message_scan() {
        // #337's bounded read is the reason `list_sessions` is cheap. The `<`
        // gate is a cost filter only; the rename decision is the provenance
        // check above.
        let tmp = tempdir().expect("tempdir");
        let messages = vec![make_test_message("user", "Fix the diagnostic display")];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 0, None);
        let mut metadata = session.metadata.clone();
        assert!(!metadata.title.starts_with('<'));
        apply_legacy_title_recovery(&mut metadata, &[]);
        assert_eq!(metadata.title, "Fix the diagnostic display");
    }

    #[test]
    fn leading_messages_stop_at_the_edge_of_a_truncated_prefix() {
        let tmp = tempdir().expect("tempdir");
        let messages = vec![
            make_test_message("user", "first"),
            make_test_message("assistant", "second"),
            make_test_message("user", "third"),
        ];
        let session = create_saved_session(&messages, "test-model", tmp.path(), 0, None);
        let buf = serde_json::to_vec(&session).expect("serialize");
        let (all, complete) = extract_leading_messages(&buf, 24);
        assert!(complete, "a whole file ends its messages array");
        assert_eq!(all.len(), 3);

        let (capped, complete) = extract_leading_messages(&buf, 2);
        assert_eq!(capped.len(), 2);
        assert!(!complete, "a capped scan has not seen the array end");

        // The file midpoint depends on metadata path lengths and can already
        // follow the messages array. Cut inside the third message instead.
        let marker = b"\"third\"";
        let cut = buf
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("third message is serialized")
            + marker.len() / 2;
        let (partial, complete) = extract_leading_messages(&buf[..cut], 24);
        assert!(!complete);
        assert_eq!(partial.len(), 2, "the cut message must remain absent");
    }

    #[test]
    fn title_derivation_keeps_the_first_image_only_user_boundary() {
        let tmp = tempdir().expect("tempdir");
        for with_metadata in [false, true] {
            let mut first = Message {
                role: Role::User,
                content: vec![ContentBlock::ImageUrl {
                    image_url: codewhale_models::ImageUrlContent {
                        url: "data:image/png;base64,AAAA".to_string(),
                    },
                }],
            };
            if with_metadata {
                first.content.push(ContentBlock::Text {
                    text: "<turn_meta>\nSession mode: Work\n</turn_meta>".to_string(),
                    cache_control: None,
                });
            }
            let messages = vec![first, make_test_message("user", "A later request")];
            assert_eq!(conversation_title_prompt(&messages), None);
            let session = create_saved_session(&messages, "test-model", tmp.path(), 0, None);
            assert_eq!(session.metadata.title, DEFAULT_SESSION_TITLE);
            let imported = SavedSession::import_foreign(
                container_with(messages, tmp.path()),
                tmp.path().to_path_buf(),
                "test-model".to_string(),
            )
            .expect("import succeeds");
            assert_eq!(imported.metadata.title, DEFAULT_SESSION_TITLE);
        }
    }

    #[test]
    fn strip_thinking_tags_removes_common_inline_blocks() {
        let text = "Before <think>private</think> middle <reasoning>hidden</reasoning> after";
        let cleaned = strip_thinking_tags(text);
        assert_eq!(cleaned, "Before  middle  after");
        assert_eq!(strip_thinking_tags("plain answer"), "plain answer");
    }

    #[test]
    fn test_format_age() {
        let now = Utc::now();
        assert_eq!(format_age(&now), "just now");

        let hour_ago = now - chrono::Duration::hours(2);
        assert_eq!(format_age(&hour_ago), "2h ago");

        let day_ago = now - chrono::Duration::days(3);
        assert_eq!(format_age(&day_ago), "3d ago");
    }

    #[test]
    fn session_titles_never_keep_terminal_controls_or_bidi_format_chars() {
        let raw = "Ev\u{1b}]0;PWNED\u{7}il\u{202e}R\u{200b}Z\u{9d}0;X\u{9c}After\u{2066}B\u{2069} 会議 🐳";
        assert_eq!(
            sanitize_session_title(raw),
            "Ev]0;PWNEDilRZ0;XAfterB 会議 🐳"
        );
        // Every rename surface goes through normalize_session_title.
        assert_eq!(
            normalize_session_title(raw).unwrap(),
            "Ev]0;PWNEDilRZ0;XAfterB 会議 🐳"
        );
        // A title that is nothing but controls is an empty title.
        assert!(normalize_session_title("\u{1b}\u{7}\u{200b}").is_err());
        // The listing line re-sanitizes titles saved before this policy.
        assert_eq!(truncate_title(raw, 40), "Ev]0;PWNEDilRZ0;XAfterB 会議 🐳");
    }

    #[test]
    fn format_session_line_includes_absolute_updated_timestamp() {
        let mut session = create_saved_session(
            &[make_test_message("user", "Find Friday work")],
            "test-model",
            Path::new("/tmp/project"),
            100,
            None,
        );
        session.metadata.updated_at = DateTime::parse_from_rfc3339("2026-06-01T12:34:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);

        let line = format_session_line(&session.metadata);

        assert!(
            line.contains("2026-06-01 12:34 UTC"),
            "session list should include an absolute timestamp, got {line:?}"
        );
    }

    #[test]
    fn test_update_session() {
        let tmp = tempdir().expect("tempdir");

        let messages = vec![make_test_message("user", "Hello")];
        let mut session = create_saved_session(&messages, "test-model", tmp.path(), 50, None);
        session.turn_outcomes.push(SavedTurnOutcome::failed("shared failure", 1));

        let new_messages = vec![
            make_test_message("user", "Hello"),
            make_test_message("assistant", "Hi!"),
        ];

        let mut updated = update_session(session, &new_messages, 100, None);
        assert_eq!(updated.messages.len(), 2);
        assert_eq!(updated.metadata.total_tokens, 100);
        assert_eq!(updated.turn_outcomes.len(), 1);
        let original_leaf = updated.leaf_id.clone().unwrap();
        updated.turn_outcomes.push(SavedTurnOutcome::failed("old branch failure", 2));
        let alternate = vec![
            make_test_message("user", "Hello"),
            make_test_message("assistant", "Different answer"),
        ];
        let mut updated = update_session(updated, &alternate, 110, None);
        assert_eq!(updated.turn_outcomes.len(), 1);
        assert_eq!(updated.turn_outcomes[0].error, "shared failure");
        updated.turn_outcomes.push(SavedTurnOutcome::failed("alternate failure", 2));
        updated.journal_branch_to(&original_leaf).expect("switch sibling branch");
        assert_eq!(updated.messages, new_messages);
        assert_eq!(updated.turn_outcomes.len(), 1);
        assert_eq!(updated.turn_outcomes[0].error, "shared failure");
    }

    #[test]
    fn save_load_round_trip_preserves_all_messages_for_cache_fidelity() {
        #[derive(serde::Deserialize)]
        struct LegacySession {
            messages: Vec<Message>,
        }

        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        // Covers the old 500-message cap boundary and well beyond.
        for count in [0, 1, 500, 501, 600, 1000] {
            let original: Vec<_> = (0..count)
                .map(|i| {
                    make_test_message(
                        if i % 2 == 0 { "user" } else { "assistant" },
                        &format!("round-trip message {i}"),
                    )
                })
                .collect();

            let mut session = create_saved_session(&original, "test-model", tmp.path(), 0, None);
            let expected_journal = session.journal.clone();
            session.compact_for_persistence_queue();
            let path = manager.save_session(&session).expect("save");
            let legacy: LegacySession =
                serde_json::from_slice(&fs::read(path).expect("read")).expect("legacy reader");
            let loaded = manager.load_session(&session.metadata.id).expect("load");

            assert_eq!(
                legacy.messages, original,
                "legacy messages for count={count}"
            );
            assert_eq!(
                loaded.journal, expected_journal,
                "journal for count={count}"
            );
            assert_eq!(
                loaded.messages.len(),
                count,
                "count preserved for count={count}"
            );
            assert_eq!(
                loaded.messages, original,
                "every message byte-identical after round-trip for count={count}"
            );
        }
    }

    #[test]
    fn test_checkpoint_round_trip_and_clear() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let messages = vec![make_test_message("user", "checkpoint me")];
        let mut session = create_saved_session(&messages, "test-model", tmp.path(), 12, None);
        session.work_state = Some(SessionWorkState {
            todos: crate::tools::todo::TodoListSnapshot {
                items: vec![crate::tools::todo::TodoItem {
                    id: 1,
                    content: "verify checkpoint durability".to_string(),
                    status: crate::tools::todo::TodoStatus::InProgress,
                }],
                completion_pct: 0,
                in_progress_id: Some(1),
            },
            ..SessionWorkState::default()
        });
        let expected_messages = session.messages.clone();
        let expected_journal = session.journal.clone();
        session.compact_for_persistence_queue();

        let path = manager.save_checkpoint(&session).expect("save checkpoint");
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(format!("{}.json", session.metadata.id).as_str()),
            "checkpoint file must be keyed by session id"
        );
        let loaded = manager
            .load_session_checkpoint(&session.metadata.id)
            .expect("load checkpoint")
            .expect("checkpoint exists");
        assert_eq!(loaded.metadata.id, session.metadata.id);
        assert_eq!(loaded.messages, expected_messages);
        assert_eq!(loaded.journal, expected_journal);
        assert_eq!(
            loaded.work_state, session.work_state,
            "work state must survive the checkpoint round trip"
        );

        manager
            .clear_session_checkpoint(&session.metadata.id)
            .expect("clear checkpoint");
        assert!(
            manager
                .load_session_checkpoint(&session.metadata.id)
                .expect("load checkpoint")
                .is_none()
        );
    }

    #[test]
    fn graph_backed_work_state_remains_readable_by_legacy_shape() {
        #[derive(serde::Deserialize)]
        struct LegacyWorkState {
            #[serde(default)]
            todos: crate::tools::todo::TodoListSnapshot,
            #[serde(default)]
            plan: crate::tools::plan::PlanSnapshot,
        }

        let fixture = include_bytes!("../../tests/fixtures/work_graph_session_v1_reader.json");
        let current: SavedSession = serde_json::from_slice(fixture).expect("current reader");
        let state = current.work_state.expect("fixture Work state");
        let legacy: LegacyWorkState = serde_json::from_value(
            serde_json::from_slice::<serde_json::Value>(fixture)
                .expect("fixture JSON")["work_state"]
                .clone(),
        )
        .expect("v1 reader ignores graph");
        assert_eq!(legacy.todos, state.todos);
        assert_eq!(legacy.plan, state.plan);
        let graph = state.graph.expect("fixture graph");
        crate::work_graph::validate(&graph).expect("valid fixture graph");
        assert_eq!(crate::work_graph::project_todos(&graph), state.todos);
        assert_eq!(crate::work_graph::project_plan(&graph), state.plan);
    }

    #[test]
    fn first_graph_write_archives_exact_legacy_session_once() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let mut session = create_saved_session(
            &[make_test_message("user", "archive before import")],
            "test-model",
            tmp.path(),
            0,
            None,
        );
        let plan = crate::tools::plan::PlanSnapshot {
            items: vec![crate::tools::plan::PlanItemArg {
                step: "Import".to_string(),
                status: crate::tools::plan::StepStatus::Pending,
            }],
            ..crate::tools::plan::PlanSnapshot::default()
        };
        let todos = crate::tools::todo::TodoListSnapshot::default();
        session.work_state = Some(SessionWorkState {
            graph: None,
            todos: todos.clone(),
            plan: plan.clone(),
        });
        let path = manager.save_session(&session).expect("save legacy session");
        let legacy_bytes = fs::read(&path).expect("read legacy bytes");

        let graph = crate::work_graph::import_legacy(&session.metadata.id, &plan, &todos)
            .expect("import graph");
        session.work_state = Some(SessionWorkState {
            graph: Some(graph),
            todos,
            plan,
        });
        manager.save_session(&session).expect("first graph write");
        let archive = manager
            .sessions_dir
            .join(WORK_GRAPH_IMPORT_ARCHIVE_DIR)
            .join(path.file_name().expect("session filename"));
        assert_eq!(fs::read(&archive).expect("archive exists"), legacy_bytes);

        session.metadata.title = "later graph write".to_string();
        manager.save_session(&session).expect("second graph write");
        assert_eq!(
            fs::read(&archive).expect("archive still exists"),
            legacy_bytes,
            "later graph writes must not replace the pre-import receipt"
        );
    }

    #[test]
    fn checkpoints_are_independent_per_session() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let first = create_saved_session(
            &[make_test_message("user", "session one")],
            "test-model",
            tmp.path(),
            0,
            None,
        );
        let second = create_saved_session(
            &[make_test_message("user", "session two")],
            "test-model",
            tmp.path(),
            0,
            None,
        );

        manager.save_checkpoint(&first).expect("save first");
        manager.save_checkpoint(&second).expect("save second");
        manager
            .clear_session_checkpoint(&first.metadata.id)
            .expect("clear first");

        assert!(
            manager
                .load_session_checkpoint(&first.metadata.id)
                .expect("load first")
                .is_none(),
            "clearing one session must remove only that session's file"
        );
        let survivor = manager
            .load_session_checkpoint(&second.metadata.id)
            .expect("load second")
            .expect("second checkpoint survives");
        assert_eq!(survivor.metadata.id, second.metadata.id);
    }

    #[test]
    fn list_checkpoints_includes_legacy_slot_and_skips_offline_queue() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let session = create_saved_session(
            &[make_test_message("user", "list me")],
            "test-model",
            tmp.path(),
            0,
            None,
        );
        manager.save_checkpoint(&session).expect("save checkpoint");
        let checkpoints = tmp.path().join("sessions").join("checkpoints");
        fs::write(checkpoints.join("latest.json"), "{}").expect("write legacy slot");
        fs::write(checkpoints.join("offline_queue.json"), "{}").expect("write legacy queue");
        fs::write(
            checkpoints.join(format!("{}.offline_queue.json", session.metadata.id)),
            "{}",
        )
        .expect("write per-session queue");

        let refs = manager.list_checkpoints().expect("list checkpoints");
        assert_eq!(refs.len(), 2, "offline queue must not be a candidate");
        assert!(
            refs.iter()
                .any(|r| r.source == CheckpointSource::Session(session.metadata.id.clone()))
        );
        assert!(refs.iter().any(|r| r.source == CheckpointSource::Legacy));
        let (sessions, recovery, warning) =
            manager.list_sessions_with_recovery().expect("recovery list");
        assert!(warning.is_some());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session.metadata.id);
        assert!(recovery.contains(&session.metadata.id));
        assert!(
            manager
                .load_recovery_checkpoint(&session.metadata.id)
                .unwrap()
                .is_some()
        );
        let mut collision = session.clone();
        collision.metadata.id = format!("{}-other", session.metadata.id);
        manager.save_checkpoint(&collision).expect("save colliding checkpoint");
        assert_eq!(
            manager
                .resolve_recoverable_session_id_prefix(&session.metadata.id)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        manager.clear_session_checkpoint(&collision.metadata.id).unwrap();
        let (restored, lease) = manager
            .attach_session_by_prefix(&session.metadata.id[..8])
            .expect("attach checkpoint-only session");
        assert_eq!(restored.session.metadata.id, session.metadata.id);
        drop(lease);
        let mut legacy = session.clone();
        legacy.metadata.id = "legacy-only".to_string();
        let damaged = manager.save_checkpoint(&legacy).expect("save checkpoint");
        fs::write(&damaged, b"{").expect("damage checkpoint");
        fs::write(
            checkpoints.join("latest.json"),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .expect("write legacy checkpoint");
        let (sessions, recovery, warning) =
            manager.list_sessions_with_recovery().expect("recovery list");
        assert!(warning.is_some());
        assert!(sessions.iter().any(|entry| entry.id == "legacy-only"));
        assert!(recovery.contains("legacy-only"));
        assert_eq!(
            manager.load_recovery_checkpoint("legacy-only").unwrap().unwrap().metadata.id,
            "legacy-only"
        );
        assert!(manager.attach_session("legacy-only").is_err());
        assert_eq!(fs::read(&damaged).unwrap(), b"{");
        assert!(manager.load_session("legacy-only").is_err());
        assert!(manager.attach_session("legacy-only").is_err());
        assert_eq!(fs::read(&damaged).unwrap(), b"{");
    }

    /// A session owned by a *prior* process instance with a crash-recovery
    /// checkpoint on disk: the foreign boot-owner stamp keeps
    /// `session_from_prior_instance` true (the save keeps the original
    /// owner), and the checkpoint is the durable interrupted sign.
    fn write_prior_interrupted_session(
        manager: &SessionManager,
        id: &str,
        workspace: &Path,
    ) -> SavedSession {
        let mut session = create_saved_session(
            &[make_test_message("user", "still working")],
            "test-model",
            workspace,
            0,
            None,
        );
        session.metadata.id = id.to_string();
        session.metadata.title = format!("prior-{id}");
        manager
            .record_session_boot_owner(id, "boot_other_instance")
            .expect("stamp foreign owner");
        manager.save_session(&session).expect("save session");
        manager.save_checkpoint(&session).expect("save checkpoint");
        session
    }

    #[test]
    fn interrupted_workspace_session_returns_newest_prior_checkpoint() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace).expect("workspace");

        write_prior_interrupted_session(&manager, "sess-old", &workspace);
        // Distinct checkpoint mtimes make newest-first deterministic.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_prior_interrupted_session(&manager, "sess-new", &workspace);

        // A checkpointed session this instance created is current work, not
        // prior work: `save_session` stamps the current boot id, and the
        // checkpoint write keeps it because the record already exists.
        let own = create_saved_session(
            &[make_test_message("user", "mine")],
            "test-model",
            &workspace,
            0,
            None,
        );
        manager.save_session(&own).expect("save own");
        manager.save_checkpoint(&own).expect("checkpoint own");

        // A checkpointed session in another workspace stays invisible.
        let other_workspace = tmp.path().join("other-ws");
        fs::create_dir_all(&other_workspace).expect("other workspace");
        write_prior_interrupted_session(&manager, "sess-elsewhere", &other_workspace);

        assert_eq!(
            manager
                .interrupted_workspace_session(&workspace, Some(own.metadata.id.as_str()))
                .map(|meta| meta.id),
            Some("sess-new".to_string())
        );
        // Excluding the newest surfaces the next interrupted session.
        assert_eq!(
            manager
                .interrupted_workspace_session(&workspace, Some("sess-new"))
                .map(|meta| meta.id),
            Some("sess-old".to_string())
        );
        assert_eq!(
            manager
                .interrupted_workspace_session(&other_workspace, None)
                .map(|meta| meta.id),
            Some("sess-elsewhere".to_string())
        );
    }

    fn hold_live_lease(manager: &SessionManager, id: &str) -> fs::File {
        manager.hold_live_lease_elsewhere(id)
    }

    #[test]
    fn interrupted_workspace_session_skips_a_session_live_elsewhere() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace).expect("workspace");
        write_prior_interrupted_session(&manager, "sess-crashed", &workspace);
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_prior_interrupted_session(&manager, "sess-running", &workspace);

        let lease = hold_live_lease(&manager, "sess-running");
        assert_eq!(
            manager
                .interrupted_workspace_session(&workspace, None)
                .map(|meta| meta.id),
            Some("sess-crashed".to_string()),
            "a checkpoint another terminal is refreshing is not a crash"
        );
        drop(lease);
        assert_eq!(
            manager
                .interrupted_workspace_session(&workspace, None)
                .map(|meta| meta.id),
            Some("sess-running".to_string())
        );
    }

    /// Attaching a session that crashed mid-turn opens the interrupted turn
    /// from its crash checkpoint; a stale checkpoint never replaces a newer
    /// saved document.
    #[test]
    fn attach_promotes_the_sessions_newer_crash_checkpoint() {
        let _env = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace).expect("workspace");
        let user = |text: &str| Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        };
        let saved = create_saved_session(&[user("first")], "test-model", &workspace, 0, None);
        let id = saved.metadata.id.clone();
        manager.save_session(&saved).expect("save session");
        let mut interrupted = saved.clone();
        interrupted.messages.push(user("in-flight turn"));
        interrupted.metadata.updated_at = saved.metadata.updated_at + chrono::Duration::seconds(5);
        manager
            .save_checkpoint(&interrupted)
            .expect("save checkpoint");
        let (listed, recovery, warning) =
            manager.list_sessions_with_recovery().expect("list recovery");
        assert!(warning.is_none());
        assert_eq!(listed.iter().filter(|session| session.id == id).count(), 1);
        assert!(recovery.contains(&id));
        let foreign = tmp.path().join("another-workspace");
        assert!(manager.attach_session_in_workspace(&id, Some(&foreign)).is_err());
        assert!(manager.load_session_checkpoint(&id).unwrap().is_some());

        let (recovery, lease) = manager.attach_session(&id).expect("attach");
        assert_eq!(
            recovery.session.messages.len(),
            2,
            "the interrupted turn is part of the attached session"
        );
        assert!(
            manager
                .load_session_checkpoint(&id)
                .expect("load checkpoint")
                .is_none(),
            "the recovered checkpoint is consumed"
        );
        drop(lease);

        let mut stale = saved.clone();
        stale.metadata.updated_at = saved.metadata.updated_at - chrono::Duration::seconds(5);
        manager
            .save_checkpoint(&stale)
            .expect("save stale checkpoint");
        let (recovery, _lease) = manager.attach_session(&id).expect("attach again");
        assert_eq!(
            recovery.session.messages.len(),
            2,
            "a stale checkpoint never replaces the newer document"
        );
    }

    /// Attaching to a session another process has open is refused, naming
    /// it, rather than giving the document a second autosaving writer.
    #[test]
    fn attach_refuses_a_session_open_in_another_process() {
        let _env = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace).expect("workspace");
        write_prior_interrupted_session(&manager, "sess-open", &workspace);

        let lease = hold_live_lease(&manager, "sess-open");
        let error = manager
            .reserve_session_for_attach("sess-open")
            .expect_err("a session open elsewhere is refused");
        assert_eq!(error.kind(), io::ErrorKind::ResourceBusy);
        assert!(error.to_string().contains("sess-open"), "{error}");
        assert!(!is_live_session("sess-open"), "nothing was claimed");

        drop(lease);
        let (_, reserved) = manager
            .attach_session("sess-open")
            .expect("attach once the other window has closed");
        assert!(
            !try_lock_elsewhere(&manager, "sess-open"),
            "the attach holds the lease from the start"
        );
        reserved.commit();
        assert!(is_live_session("sess-open"));
        assert!(!try_lock_elsewhere(&manager, "sess-open"));
        set_live_session(None);
    }

    /// Switching from session A to B must not give up A until B is applied:
    /// a reservation that is dropped (B failed to load or apply) leaves A
    /// claimed and leased, and B free.
    #[test]
    fn a_failed_attach_keeps_the_current_sessions_lease() {
        let _env = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace).expect("workspace");
        write_prior_interrupted_session(&manager, "sess-a", &workspace);
        write_prior_interrupted_session(&manager, "sess-b", &workspace);
        manager.claim_live_session("sess-a");
        assert!(!try_lock_elsewhere(&manager, "sess-a"), "A is leased");

        let reserved = manager
            .reserve_session_for_attach("sess-b")
            .expect("reserve B");
        assert!(is_live_session("sess-a"), "A stays claimed while B loads");
        assert!(!try_lock_elsewhere(&manager, "sess-a"), "and leased");
        drop(reserved);
        assert!(is_live_session("sess-a"));
        assert!(!try_lock_elsewhere(&manager, "sess-a"), "A is still leased");
        assert!(try_lock_elsewhere(&manager, "sess-b"), "B was released");

        // A B whose document cannot be loaded is refused the same way.
        let b_path = manager.validated_session_path("sess-b").expect("path");
        fs::write(&b_path, "{ not json").expect("corrupt B");
        manager
            .attach_session("sess-b")
            .expect_err("a corrupt B fails to attach");
        assert!(is_live_session("sess-a"));
        assert!(!try_lock_elsewhere(&manager, "sess-a"), "A is still leased");
        assert!(try_lock_elsewhere(&manager, "sess-b"), "B was released");
        set_live_session(None);
    }

    /// `/load` takes the same contract as `resume`: a managed record and a
    /// foreign file naming a session open in another window are both
    /// refused, and neither is claimed.
    #[test]
    fn load_of_a_session_open_elsewhere_is_refused_for_managed_and_foreign_files() {
        let _env = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace).expect("workspace");
        let session = write_prior_interrupted_session(&manager, "sess-load", &workspace);
        let managed = manager.validated_session_path("sess-load").expect("path");
        let foreign = tmp.path().join("exported.json");
        fs::write(&foreign, serde_json::to_vec(&session).expect("json")).expect("export");

        let lease = hold_live_lease(&manager, "sess-load");
        for path in [&managed, &foreign] {
            let error = manager
                .attach_session_file(session.clone(), path)
                .expect_err("a session open elsewhere is not loaded");
            assert_eq!(
                error.kind(),
                io::ErrorKind::ResourceBusy,
                "{}",
                path.display()
            );
        }
        assert!(!is_live_session("sess-load"), "nothing was claimed");

        drop(lease);
        for path in [&managed, &foreign] {
            let (loaded, reserved) = manager
                .attach_session_file(session.clone(), path)
                .expect("loads once the other window has closed");
            assert_eq!(loaded.metadata.id, "sess-load");
            assert!(
                !try_lock_elsewhere(&manager, "sess-load"),
                "the load holds the lease"
            );
            drop(reserved);
        }
        set_live_session(None);
    }

    /// A lease that cannot be taken is an error, not an unguarded attach.
    #[cfg(unix)]
    #[test]
    fn attach_fails_closed_when_the_lease_cannot_be_taken() {
        let _env = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace).expect("workspace");
        write_prior_interrupted_session(&manager, "sess-x", &workspace);
        // The lease directory is a regular file: no lock file can be opened.
        let _ = fs::remove_dir_all(sessions_dir.join(LATE_USAGE_DIR));
        fs::write(sessions_dir.join(LATE_USAGE_DIR), "").expect("block lease dir");

        let error = manager
            .attach_session("sess-x")
            .expect_err("no lease, no attach");
        assert!(error.to_string().contains("live lease"), "{error}");
        assert!(!is_live_session("sess-x"), "nothing was claimed");
    }

    /// A liveness probe from another process briefly holds the lease lock. A
    /// claim that lost that race once must take the lease on a later save,
    /// not run unleased for the whole session.
    #[test]
    fn a_claim_that_lost_a_probe_race_takes_the_lease_later() {
        let _env = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");

        let probe = hold_live_lease(&manager, "sess-a");
        manager.claim_live_session("sess-a");
        drop(probe);
        assert!(
            try_lock_elsewhere(&manager, "sess-a"),
            "the first claim lost"
        );

        manager.claim_live_session("sess-a");
        assert!(
            !try_lock_elsewhere(&manager, "sess-a"),
            "the next claim takes the lease"
        );
        set_live_session(None);
    }

    /// Whether another open file description can lock `id`'s lease now.
    fn try_lock_elsewhere(manager: &SessionManager, id: &str) -> bool {
        let lease = open_private_read_file(&manager.live_lease_path(id, false).expect("path"))
            .expect("lease file");
        codewhale_util::lock::try_lock_file_exclusive(&lease).expect("lock")
    }

    #[test]
    fn interrupted_workspace_session_ignores_settled_sessions() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace).expect("workspace");

        // Prior-instance session that settled cleanly: no checkpoint, so it
        // is not interrupted even though it is prior work.
        manager
            .record_session_boot_owner("sess-done", "boot_other_instance")
            .expect("stamp foreign owner");
        write_session_record(&manager, "sess-done", &workspace, Utc::now());

        assert!(
            manager
                .interrupted_workspace_session(&workspace, None)
                .is_none()
        );

        // Excluding the only interrupted session leaves nothing to report.
        write_prior_interrupted_session(&manager, "sess-prior", &workspace);
        assert!(
            manager
                .interrupted_workspace_session(&workspace, Some("sess-prior"))
                .is_none()
        );
    }

    #[test]
    fn session_recovery_hint_names_the_interrupted_prior_session() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
        let _codewhale_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.join("codewhale"));
        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace).expect("workspace");

        let manager = SessionManager::default_location().expect("default manager");
        assert!(
            session_recovery_hint(&workspace, None).is_none(),
            "clean store must leave the prompt untouched"
        );

        write_prior_interrupted_session(&manager, "sess-prior", &workspace);
        let hint = session_recovery_hint(&workspace, Some("sess-live"))
            .expect("hint for interrupted prior session");
        assert!(hint.contains("prior-sess-prior"), "{hint}");
        assert!(hint.contains("session_search"), "{hint}");
        assert!(hint.contains("/resume"), "{hint}");

        // The live session's own checkpoint is never reported as prior work.
        assert!(session_recovery_hint(&workspace, Some("sess-prior")).is_none());
    }

    #[test]
    fn workspace_scope_matches_subdirectories_in_same_git_checkout() {
        let tmp = tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        let nested = repo.join("crates").join("tui");
        fs::create_dir_all(&nested).expect("mkdir nested");
        fs::write(repo.join(".git"), "gitdir: .git/worktrees/repo").expect("write git marker");

        assert!(workspace_scope_matches(&repo, &nested));
    }

    #[test]
    fn workspace_scope_rejects_sibling_git_checkouts() {
        let tmp = tempdir().expect("tempdir");
        let first = tmp.path().join("repo-a");
        let second = tmp.path().join("repo-b");
        fs::create_dir_all(&first).expect("mkdir first");
        fs::create_dir_all(&second).expect("mkdir second");
        fs::write(first.join(".git"), "gitdir: .git/worktrees/a").expect("write first marker");
        fs::write(second.join(".git"), "gitdir: .git/worktrees/b").expect("write second marker");

        assert!(!workspace_scope_matches(&first, &second));
    }

    #[test]
    fn test_offline_queue_round_trip_and_clear() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");

        let state = OfflineQueueState {
            messages: vec![QueuedSessionMessage {
                display: "queued message".to_string(),
                skill_instruction: Some("Use skill".to_string()),
                skill_provenance: None,
            }],
            draft: Some(QueuedSessionMessage {
                display: "draft message".to_string(),
                skill_instruction: None,
                skill_provenance: None,
            }),
            ..OfflineQueueState::default()
        };

        manager
            .save_offline_queue_state(&state, Some("test-session"))
            .expect("save queue state");
        let loaded = manager
            .load_offline_queue_state("test-session")
            .expect("load queue state")
            .expect("queue state exists");
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.messages[0].display, "queued message");
        assert!(loaded.draft.is_some());

        manager
            .clear_offline_queue_state_for("test-session")
            .expect("clear queue state");
        assert!(
            manager
                .load_offline_queue_state("test-session")
                .expect("load queue state")
                .is_none()
        );

        // A queue with no owning session has nowhere to be restored to, so it
        // is refused rather than written where another session would find it.
        let unowned = manager.save_offline_queue_state(&state, None);
        assert!(unowned.is_err(), "unowned queue must not be parked");
    }

    fn parked(text: &str) -> OfflineQueueState {
        OfflineQueueState {
            messages: vec![QueuedSessionMessage {
                display: text.to_string(),
                skill_instruction: None,
                skill_provenance: None,
            }],
            ..OfflineQueueState::default()
        }
    }

    #[test]
    fn offline_queues_are_keyed_per_session() {
        // Replaces the #487 single-slot test, which pinned the shared
        // `checkpoints/offline_queue.json`: two concurrent Codewhale
        // instances raced on it and the loser's unsent text was destroyed.
        // Queues are keyed per session for the same reason checkpoints are.
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");

        manager
            .save_offline_queue_state(&parked("A text"), Some("session-A"))
            .expect("park A");
        manager
            .save_offline_queue_state(&parked("B text"), Some("session-B"))
            .expect("park B");

        let a = manager
            .load_offline_queue_state("session-A")
            .expect("load A")
            .expect("A still parked");
        assert_eq!(a.messages[0].display, "A text");
        assert_eq!(a.session_id.as_deref(), Some("session-A"));
        let b = manager
            .load_offline_queue_state("session-B")
            .expect("load B")
            .expect("B still parked");
        assert_eq!(b.messages[0].display, "B text");

        // Clearing one session's queue leaves the other's alone.
        manager
            .clear_offline_queue_state_for("session-A")
            .expect("clear A");
        assert!(
            manager
                .load_offline_queue_state("session-A")
                .expect("load A")
                .is_none()
        );
        assert!(
            manager
                .load_offline_queue_state("session-B")
                .expect("load B")
                .is_some(),
            "clearing one session must never delete another's unsent text"
        );

        // A session with nothing parked reads back nothing — it can never
        // inherit, or destroy, a sibling's queue.
        assert!(
            manager
                .load_offline_queue_state("session-C")
                .expect("load C")
                .is_none()
        );
    }

    #[test]
    fn legacy_global_queue_is_adopted_only_by_its_own_session() {
        let tmp = tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("new");
        let checkpoints = sessions_dir.join("checkpoints");
        fs::create_dir_all(&checkpoints).expect("create checkpoints dir");
        let legacy = checkpoints.join("offline_queue.json");
        let mut state = parked("text from the old global queue");
        state.session_id = Some("session-A".to_string());
        fs::write(
            &legacy,
            serde_json::to_string_pretty(&state).expect("serialize"),
        )
        .expect("write legacy queue");

        // A different session must not inherit it, and must not delete it.
        assert!(
            manager
                .load_offline_queue_state("session-B")
                .expect("load B")
                .is_none()
        );
        assert!(legacy.exists(), "another session's text must survive");

        // Its own session adopts it, and the global file is retired only
        // after the per-session copy is durably written.
        let adopted = manager
            .load_offline_queue_state("session-A")
            .expect("load A")
            .expect("adopted");
        assert_eq!(
            adopted.messages[0].display,
            "text from the old global queue"
        );
        assert!(!legacy.exists(), "adopted legacy queue is retired");
        assert!(
            checkpoints.join("session-A.offline_queue.json").exists(),
            "adoption writes the per-session file"
        );
        let again = manager
            .load_offline_queue_state("session-A")
            .expect("reload A")
            .expect("still parked");
        assert_eq!(again.messages[0].display, "text from the old global queue");
    }

    #[test]
    fn test_session_context_references_round_trip() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let mut session = create_saved_session(
            &[make_test_message("user", "read @src/main.rs")],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
        );
        session.context_references.push(SessionContextReference {
            message_index: 0,
            reference: ContextReference {
                kind: ContextReferenceKind::File,
                source: ContextReferenceSource::AtMention,
                badge: "file".to_string(),
                label: "src/main.rs".to_string(),
                target: tmp.path().join("src/main.rs").display().to_string(),
                included: true,
                expanded: true,
                detail: Some("included".to_string()),
            },
        });

        let path = manager.save_session(&session).expect("save session");
        let loaded = manager
            .load_session(&session.metadata.id)
            .expect("load session");
        assert!(path.exists());
        assert_eq!(loaded.context_references, session.context_references);
    }

    #[test]
    fn test_checkpoint_rejects_newer_schema() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let checkpoints = tmp.path().join("sessions").join("checkpoints");
        fs::create_dir_all(&checkpoints).expect("create checkpoints dir");
        let path = checkpoints.join("latest.json");
        fs::write(
            &path,
            r#"{
                "schema_version": 999,
                "metadata": {
                    "id": "sid",
                    "title": "bad",
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-01T00:00:00Z",
                    "message_count": 0,
                    "total_tokens": 0,
                    "model": "m",
                    "workspace": "/tmp",
                    "mode": null
                },
                "messages": [],
                "system_prompt": null
            }"#,
        )
        .expect("write checkpoint");

        let err = manager
            .load_legacy_checkpoint()
            .expect_err("should reject schema");
        assert!(err.to_string().contains("newer than supported"));

        // The same guard applies to per-session checkpoint files.
        fs::rename(&path, checkpoints.join("sid.json")).expect("rename to per-session file");
        let err = manager
            .load_session_checkpoint("sid")
            .expect_err("should reject schema");
        assert!(err.to_string().contains("newer than supported"));
    }

    #[test]
    fn test_load_session_rejects_newer_schema() {
        let tmp = tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("new");

        let id = "future-session";
        let path = sessions_dir.join(format!("{id}.json"));
        fs::write(
            &path,
            r#"{
                "schema_version": 999,
                "metadata": {
                    "id": "future-session",
                    "title": "future",
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-01T00:00:00Z",
                    "message_count": 0,
                    "total_tokens": 0,
                    "model": "m",
                    "workspace": "/tmp",
                    "mode": null
                },
                "messages": [],
                "system_prompt": null
            }"#,
        )
        .expect("write session");

        let err = manager.load_session(id).expect_err("should reject schema");
        assert!(
            err.to_string().contains("newer than supported"),
            "unexpected error: {err}"
        );
    }

    /// Regression for #337: metadata extraction skips the (potentially
    /// huge) `messages` array — it must succeed even when the messages
    /// array is megabytes long, and it must NOT confuse a `"metadata"`
    /// substring inside a message body for the real top-level key.
    #[test]
    fn extract_top_level_metadata_skips_huge_messages_array() {
        // Build a session JSON with a large `messages` payload that
        // contains the literal string `"metadata"` in a user message —
        // a naive `find("\"metadata\"")` would mis-target this.
        let big_text = format!(
            r#"this message references "metadata" inside it, repeated:{}"#,
            "x".repeat(20_000)
        );
        let json = format!(
            r#"{{
                "schema_version": 1,
                "metadata": {{
                    "id": "abc-123",
                    "title": "Real Session",
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-02T00:00:00Z",
                    "message_count": 12,
                    "total_tokens": 4096,
                    "model": "deepseek-v4-flash",
                    "workspace": "/tmp"
                }},
                "messages": [
                    {{ "role": "user", "content": [ {{ "Text": {{ "text": {big_text:?} }} }} ] }}
                ]
            }}"#
        );

        let extracted =
            extract_top_level_metadata(json.as_bytes()).expect("metadata extractable from prefix");
        assert_eq!(extracted.id, "abc-123");
        assert_eq!(extracted.title, "Real Session");
        assert_eq!(extracted.message_count, 12);
        assert_eq!(extracted.total_tokens, 4096);
    }

    /// A 64 KB read prefix routinely ends inside a multi-byte character in a
    /// CJK or emoji transcript. The metadata ahead of the cut is intact and
    /// must still be read from the prefix, not force a full-file read.
    #[test]
    fn extract_top_level_metadata_survives_a_prefix_cut_inside_a_character() {
        let json = format!(
            r#"{{"schema_version":1,"metadata":{{"id":"cjk-1","title":"会话","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z","message_count":3,"total_tokens":10,"model":"m","workspace":"/tmp"}},"messages":[{{"role":"user","content":"{}"}}]}}"#,
            "中文".repeat(100)
        );
        let bytes = json.as_bytes();
        let body_start = json.find("中文").expect("body");
        // One byte into a three-byte character.
        let cut = &bytes[..body_start + 1];
        assert!(
            std::str::from_utf8(cut).is_err(),
            "the cut splits a character"
        );

        let extracted = extract_top_level_metadata(cut).expect("metadata from the cut prefix");
        assert_eq!(extracted.id, "cjk-1");
        assert_eq!(extracted.title, "会话");
    }

    /// Only a cut at the end of the prefix is trimmed. An invalid byte ahead
    /// of it is a damaged file, left to the full read to report.
    #[test]
    fn extract_top_level_metadata_rejects_invalid_utf8_inside_the_prefix() {
        let json = r#"{"schema_version":1,"metadata":{"id":"bad-1","title":"t","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z","message_count":3,"total_tokens":10,"model":"m","workspace":"/tmp"},"messages":[{"role":"user","content":"AB"}]}"#;
        let mut bytes = json.as_bytes().to_vec();
        let body = json.find("AB").expect("body");
        bytes[body] = 0xFF;

        assert!(extract_top_level_metadata(&bytes).is_none());
    }

    #[test]
    fn extract_top_level_metadata_handles_braces_inside_strings() {
        // A title containing `{` and `}` inside the metadata block must
        // not throw off the brace counter.
        let json = r#"{
            "metadata": {
                "id": "x",
                "title": "weird { title } with braces",
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z",
                "message_count": 0,
                "total_tokens": 0,
                "model": "m",
                "workspace": "/tmp"
            },
            "messages": []
        }"#;
        let extracted = extract_top_level_metadata(json.as_bytes())
            .expect("brace-in-string survives the scanner");
        assert_eq!(extracted.title, "weird { title } with braces");
    }

    #[test]
    fn saved_session_deserializes_without_artifacts_as_empty_registry() {
        let json = r#"{
            "schema_version": 1,
            "metadata": {
                "id": "legacy-session",
                "title": "legacy",
                "created_at": "2026-05-08T00:00:00Z",
                "updated_at": "2026-05-08T00:00:00Z",
                "message_count": 0,
                "total_tokens": 0,
                "model": "deepseek-v4-pro",
                "workspace": "/tmp"
            },
            "messages": [],
            "system_prompt": null
        }"#;

        let session: SavedSession = serde_json::from_str(json).expect("legacy session loads");
        assert!(session.artifacts.is_empty());
        assert!(session.last_auto_route.is_none());
        assert!(session.metadata.parent_session_id.is_none());
        assert!(session.metadata.forked_from_message_count.is_none());
    }

    #[test]
    fn fork_lineage_metadata_round_trips_and_formats() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let parent = create_saved_session(
            &[
                make_test_message("user", "try approach A"),
                make_test_message("assistant", "A looks viable"),
            ],
            "deepseek-v4-pro",
            Path::new("/tmp"),
            42,
            None,
        );
        let mut forked = create_saved_session(
            &parent.messages,
            &parent.metadata.model,
            &parent.metadata.workspace,
            parent.metadata.total_tokens,
            None,
        );
        forked.metadata.mark_forked_from(&parent.metadata);

        manager.save_session(&forked).expect("save fork");
        let loaded = manager
            .load_session(&forked.metadata.id)
            .expect("load fork");

        assert_eq!(
            loaded.metadata.parent_session_id.as_deref(),
            Some(parent.metadata.id.as_str())
        );
        assert_eq!(loaded.metadata.forked_from_message_count, Some(2));
        let line = format_session_line(&loaded.metadata);
        assert!(line.contains("fork"));
        assert!(!line.contains(parent.metadata.id.as_str()));
    }

    #[test]
    fn save_and_load_session_preserves_artifact_metadata() {
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
        let mut session = create_saved_session(
            &[make_test_message("user", "run tests")],
            "deepseek-v4-pro",
            Path::new("/tmp"),
            0,
            None,
        );
        session.artifacts.push(codewhale_runtime::artifacts::ArtifactRecord {
            id: "art_call_big".to_string(),
            kind: codewhale_runtime::artifacts::ArtifactKind::ToolOutput,
            session_id: session.metadata.id.clone(),
            tool_call_id: "call-big".to_string(),
            tool_name: "exec_shell".to_string(),
            created_at: Utc::now(),
            byte_size: 512_000,
            preview: "cargo test output".to_string(),
            storage_path: PathBuf::from("/tmp/tool_outputs/call-big.txt"),
        });

        manager.save_session(&session).expect("save");
        let loaded = manager.load_session(&session.metadata.id).expect("load");

        assert_eq!(loaded.artifacts, session.artifacts);
    }

    // ---- #406 prune_sessions_older_than ----
    //
    // The helper is a building block for the auto-archive design: it
    // removes session files older than a threshold while leaving fresh
    // ones (and the checkpoint directory) alone. Tests cover the empty
    // case, the all-fresh case, the all-stale case, and the mixed case.

    fn write_session_with_updated_at(
        manager: &SessionManager,
        id: &str,
        updated_at: DateTime<Utc>,
    ) {
        // Build a minimal SavedSession by hand so the test isn't tied
        // to whatever the helper functions emit; we just need a
        // metadata block whose `updated_at` matches the requested
        // value.
        write_session_record(manager, id, Path::new("/tmp"), updated_at);
    }

    #[test]
    fn retention_archives_past_the_cap_and_never_unlinks_transcripts() {
        // #6136: the cap retires transcripts into the archive; it must not
        // delete what the user never asked to delete.
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let mut ids = Vec::new();
        for index in 0..(MAX_SESSIONS + 3) {
            let id = Uuid::new_v4().to_string();
            write_session_with_updated_at(
                &manager,
                &id,
                Utc::now() - chrono::Duration::minutes((MAX_SESSIONS + 3 - index) as i64),
            );
            ids.push(id);
        }
        manager.cleanup_old_sessions().expect("retention");

        let listed = manager.list_sessions().expect("sessions");
        assert_eq!(listed.len(), MAX_SESSIONS + 3, "nothing is unlinked");
        let archived: Vec<&str> = listed
            .iter()
            .filter(|session| session.archived)
            .map(|session| session.id.as_str())
            .collect();
        assert_eq!(
            archived.len(),
            3,
            "exactly the overflow is archived: {archived:?}"
        );
        for id in &ids[..3] {
            assert!(archived.contains(&id.as_str()), "{id} must be archived");
            assert!(
                manager.validated_session_path(id).expect("path").exists(),
                "the transcript file survives retention"
            );
        }
        for id in &ids[3..] {
            assert!(
                !archived.contains(&id.as_str()),
                "{id} is inside the cap and must stay active"
            );
        }
        for index in 0..(MAX_SESSIONS + 3) {
            let session = create_saved_session_with_mode(
                &[make_test_message("user", &format!("exec run {index}"))],
                "test-model",
                tmp.path(),
                0,
                None,
                Some("exec"),
            );
            manager.save_session(&session).expect("save exec session");
        }
        let listed = manager.list_sessions().expect("sessions after exec runs");
        assert_eq!(listed.len(), 2 * (MAX_SESSIONS + 3));
        for is_exec in [false, true] {
            assert_eq!(
                listed.iter().filter(|session| !session.archived && (session.mode.as_deref() == Some("exec")) == is_exec).count(),
                MAX_SESSIONS,
            );
        }
    }

    #[test]
    fn empty_stubs_are_capped_apart_and_never_evict_transcripts() {
        // #6137: auto-created "New Session" stubs must not occupy (or evict
        // from) the transcript cap.
        let tmp = tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        for index in 0..MAX_SESSIONS {
            write_session_with_updated_at(
                &manager,
                &Uuid::new_v4().to_string(),
                Utc::now() - chrono::Duration::minutes((MAX_SESSIONS + 20 - index) as i64),
            );
        }
        let mut stub_ids = Vec::new();
        for index in 0..(MAX_EMPTY_SESSION_STUBS + 4) {
            let id = Uuid::new_v4().to_string();
            write_empty_session_record(
                &manager,
                &id,
                Path::new("/tmp"),
                Utc::now()
                    - chrono::Duration::minutes((MAX_EMPTY_SESSION_STUBS + 4 - index) as i64),
            );
            stub_ids.push(id);
        }
        manager.cleanup_old_sessions().expect("retention");

        let listed = manager.list_sessions().expect("sessions");
        assert_eq!(
            listed
                .iter()
                .filter(|session| !is_empty_auto_created_session(session) && !session.archived)
                .count(),
            MAX_SESSIONS,
            "stubs never push a transcript out of the cap"
        );
        assert!(
            listed
                .iter()
                .filter(|session| !is_empty_auto_created_session(session))
                .all(|session| !session.archived),
            "no transcript is archived while only stubs are over their cap"
        );
        assert_eq!(
            listed
                .iter()
                .filter(|session| is_empty_auto_created_session(session))
                .count(),
            MAX_EMPTY_SESSION_STUBS,
            "stub retention keeps only the newest stubs"
        );
        for id in &stub_ids[..4] {
            assert!(
                !manager.validated_session_path(id).expect("path").exists(),
                "{id} is an old stub and must be removed"
            );
        }
        for id in &stub_ids[4..] {
            assert!(
                manager.validated_session_path(id).expect("path").exists(),
                "{id} is among the newest stubs and must stay"
            );
        }
    }
