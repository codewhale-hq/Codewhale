use super::manager::*;
use super::*;
use crate::core::engine::EngineConfig;
use crate::core::engine::{MockApprovalEvent, mock_engine_handle};
use crate::core::events::{Event as EngineEvent, TurnOutcomeStatus};
use crate::core::ops::{Op, TurnSpec};
use crate::route_budget::known_route_limits;
use crate::tools::subagent::SubAgentStatus;
use codewhale_execpolicy::ApprovalMode;
use codewhale_models::{SystemPrompt, ToolCaller};
use codewhale_protocol::agent_mail::{
    AGENT_MAIL_EVENT_DELIVERED, AGENT_MAIL_EVENT_DELIVERING, AGENT_MAIL_EVENT_QUEUED,
    AgentMailSendRequest,
};
use codewhale_protocol::agent_mail::{AgentMailDeliveryMode, AgentMailSender, AgentMailStatus};
use codewhale_protocol::runtime::{
    DynamicToolCallContent, DynamicToolCallParams, DynamicToolCallResult,
};
use codewhale_runtime::config::Config;
use codewhale_runtime::runtime_policy::RuntimePolicyProjection;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;
use uuid::Uuid;

mod recovery {
    use super::*;
    use crate::core::engine::Engine;
    use crate::image_attach::test_support::runtime_image_fixture;
    use crate::llm_client::mock::{MockLlmClient, canned};

    fn config() -> Config {
        let mut config = {
            let __cw_config_init_0_0 = Some("recovery-fixture".into());
            let __cw_config_init_0_1 = Some(codewhale_runtime::config::ProvidersConfig {
                custom: HashMap::from([(
                    "recovery-fixture".into(),
                    codewhale_runtime::config::ProviderConfig {
                        kind: Some("openai-compatible".into()),
                        base_url: Some("http://127.0.0.1:18181/v1".into()),
                        model: Some("fixture-model".into()),
                        api_key: Some("local-test-key".into()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            });
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        };
        config.set_feature("mcp", false).unwrap();
        config.set_feature("subagents", false).unwrap();
        config
    }

    async fn close_engines(manager: &RuntimeThreadManager) -> Result<()> {
        let handles = manager
            .active
            .lock()
            .await
            .engines
            .drain()
            .map(|(_, state)| state.engine)
            .collect::<Vec<_>>();
        for handle in handles {
            handle.send(Op::Shutdown).await?;
            tokio::time::timeout(Duration::from_secs(10), async {
                while handle.rx_event.write().await.recv().await.is_some() {}
            })
            .await?;
        }
        Ok(())
    }

    async fn history_case(restart: bool) -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let workspace = dir.path().join("workspace");
        fs::create_dir(&workspace)?;
        let runtime_dir = dir.path().join("runtime");
        let manager_cfg = RuntimeThreadManagerConfig {
            mcp_auth_context: Default::default(),
            max_active_threads: 1,
            ..test_manager_config(runtime_dir)
        };
        let mut manager =
            RuntimeThreadManager::open(config(), workspace.clone(), manager_cfg.clone())?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let image_url = format!(
            "data:image/png;base64,{}",
            runtime_image_fixture(17).data_base64
        );
        let old: Vec<Message> = serde_json::from_value(json!([
            {"role":"user","content":[{"type":"text","text":"OLD"},{"type":"image_url","image_url":{"url":image_url}}]},
            {"role":"assistant","content":[{"type":"thinking","thinking":"prior reasoning","signature":"fixture-signed-thinking"},{"type":"tool_use","id":"fixture-call","name":"read_file","input":{"path":"fixture.txt"},"thought_signature":"fixture-tool-signature"}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"fixture-call","content":"old result"}]},
            {"role":"assistant","content":[{"type":"text","text":"OLD ANSWER"}]}
        ]))?;
        manager.seed_thread_from_messages(&thread.id, &old).await?;
        let mut saved = crate::session_manager::create_saved_session_with_id_and_mode(
            Uuid::new_v4().to_string(),
            &old,
            &thread.model,
            &workspace,
            0,
            None,
            Some("agent"),
        );
        saved.metadata.set_model_provider_route(
            thread.model_provider.as_deref().unwrap(),
            thread.model_provider_id.as_deref(),
        );
        let sessions = crate::session_manager::SessionManager::new(
            crate::session_manager::default_sessions_dir()?,
        )?;
        {
            let _admission = manager.session_checkpoint_guard().await;
            sessions.save_session(&saved)?;
            manager
                .set_thread_session_checkpoint(&thread.id, &saved)
                .await?;
        }
        let original_engine = manager.get_engine(&thread.id).await?;
        assert_eq!(original_engine.get_session_snapshot().await?.messages, old);
        // Simulate a later durable turn using the real store writer. The saved
        // full-fidelity snapshot deliberately remains at the OLD boundary.
        let tail: Vec<Message> = serde_json::from_value(json!([
            {"role":"user","content":[{"type":"text","text":"NEW"}]},
            {"role":"assistant","content":[{"type":"text","text":"NEW ANSWER"}]}
        ]))?;
        manager.seed_thread_from_messages(&thread.id, &tail).await?;
        assert_eq!(sessions.load_session(&saved.metadata.id)?.messages, old);
        if restart {
            close_engines(&manager).await?;
            drop(original_engine);
            drop(manager);
            manager = RuntimeThreadManager::open(config(), workspace, manager_cfg)?;
        } else {
            let other = manager
                .create_thread(CreateThreadRequest::default())
                .await?;
            manager
                .get_engine(&other.id)
                .await?
                .get_session_snapshot()
                .await?;
            assert!(!manager.active.lock().await.engines.contains_key(&thread.id));
        }
        let restored = manager
            .get_engine(&thread.id)
            .await?
            .get_session_snapshot()
            .await?
            .messages;
        let mut expected = old;
        expected.extend(tail);
        assert_eq!(
            restored, expected,
            "snapshot plus durable tail must be exact, including raw signatures/media"
        );
        let detail = manager.get_thread_detail(&thread.id).await?;
        assert!(
            detail
                .items
                .iter()
                .any(|item| item.detail.as_deref() == Some("NEW"))
        );
        close_engines(&manager).await?;
        Ok(())
    }

    #[tokio::test]
    async fn linked_saved_session_replays_runtime_tail_after_eviction() -> Result<()> {
        history_case(false).await
    }

    #[tokio::test]
    async fn linked_saved_session_replays_runtime_tail_after_restart() -> Result<()> {
        history_case(true).await
    }

    /// #6659: an unbound thread (no saved session) keeps one engine session
    /// id — its own thread id — across its first spawn, an LRU eviction and a
    /// Runtime restart. That id keys the thread's spill directory
    /// (`sessions/<id>/artifacts/`), its workspace snapshot tags and its shell
    /// jobs, so none of them scatter per spawn.
    #[tokio::test]
    async fn unbound_thread_keeps_one_engine_session_id_across_respawns() -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let workspace = dir.path().join("workspace");
        fs::create_dir(&workspace)?;
        let manager_cfg = RuntimeThreadManagerConfig {
            mcp_auth_context: Default::default(),
            max_active_threads: 1,
            ..test_manager_config(dir.path().join("runtime"))
        };
        let mut manager =
            RuntimeThreadManager::open(config(), workspace.clone(), manager_cfg.clone())?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        assert_eq!(thread.session_id, None, "the thread is unbound");
        let history: Vec<Message> = serde_json::from_value(json!([
            {"role":"user","content":[{"type":"text","text":"hi"}]},
            {"role":"assistant","content":[{"type":"text","text":"hello"}]}
        ]))?;
        manager
            .seed_thread_from_messages(&thread.id, &history)
            .await?;

        let engine_session_id = |manager: &RuntimeThreadManager| {
            let manager = manager.clone();
            let thread_id = thread.id.clone();
            async move {
                anyhow::Ok(
                    manager
                        .get_engine(&thread_id)
                        .await?
                        .get_session_snapshot()
                        .await?
                        .session_id,
                )
            }
        };

        let first = engine_session_id(&manager).await?;
        assert_eq!(first, thread.id);

        // Loading another thread's engine evicts this one.
        let other = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        manager.get_engine(&other.id).await?;
        assert!(!manager.active.lock().await.engines.contains_key(&thread.id));
        let after_eviction = engine_session_id(&manager).await?;
        assert_eq!(after_eviction, first, "respawn after eviction");

        close_engines(&manager).await?;
        drop(manager);
        manager = RuntimeThreadManager::open(config(), workspace, manager_cfg)?;
        let after_restart = engine_session_id(&manager).await?;
        assert_eq!(after_restart, first, "respawn after restart");
        assert_eq!(
            manager
                .get_thread_detail(&thread.id)
                .await?
                .thread
                .session_id,
            None,
            "the engine identity does not bind the thread to a saved session"
        );
        close_engines(&manager).await?;
        Ok(())
    }

    #[tokio::test]
    async fn saved_checkpoint_validation_and_forks_preserve_history() -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let sessions_dir = dir.path().join("owner-sessions");
        let mut manager_config = test_manager_config(dir.path().join("runtime"));
        manager_config.sessions_dir = Some(sessions_dir.clone());
        let manager =
            RuntimeThreadManager::open(config(), dir.path().to_path_buf(), manager_config)?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let image_url = format!(
            "data:image/png;base64,{}",
            runtime_image_fixture(33).data_base64
        );
        let messages: Vec<Message> = serde_json::from_value(json!([
            {"role":"user","content":[{"type":"text","text":"keep"},{"type":"image_url","image_url":{"url":image_url}}]},
            {"role":"assistant","content":[{"type":"thinking","thinking":"kept reasoning","signature":"kept signature"},{"type":"text","text":"kept answer"}]},
            {"role":"user","content":[{"type":"text","text":"drop"}]},
            {"role":"assistant","content":[{"type":"text","text":"dropped answer"}]}
        ]))?;
        manager
            .seed_thread_from_messages(&thread.id, &messages)
            .await?;
        let saved = crate::session_manager::create_saved_session_with_id_and_mode(
            Uuid::new_v4().to_string(),
            &messages,
            &thread.model,
            dir.path(),
            0,
            None,
            Some("agent"),
        );
        let sessions = crate::session_manager::SessionManager::new(sessions_dir.clone())?;
        assert_eq!(manager.sessions_dir(), sessions.sessions_dir());
        let _admission = manager.session_checkpoint_guard().await;
        sessions.save_session(&saved)?;
        manager
            .set_thread_session_checkpoint(&thread.id, &saved)
            .await?;
        drop(_admission);
        let _relocated_home = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_HOME",
            dir.path().join("another-home"),
        );
        let foreign_sessions = dir.path().join("foreign-sessions");
        assert!(
            manager
                .fork_thread_in_sessions_dir(&thread.id, &foreign_sessions)
                .await
                .is_err()
        );
        assert!(!foreign_sessions.exists());
        let thread = manager.get_thread(&thread.id).await?;
        let fork = manager.fork_thread(&thread.id).await?;
        assert_eq!(manager.restore_thread_messages(&fork)?, messages);
        // A fork owns its own session document. It used to inherit the
        // source's, which pointed two threads at one file: whichever side
        // saved last left the other's checkpoint describing bytes that were
        // gone, and hydrating that thread then failed outright.
        let fork_session_id = fork
            .session_id
            .clone()
            .context("fork should own a session document")?;
        assert_ne!(fork_session_id, saved.metadata.id);
        let fork_session = sessions.load_session(&fork_session_id)?;
        assert_eq!(fork_session.messages, messages);
        assert_eq!(
            fork_session.metadata.parent_session_id.as_deref(),
            Some(saved.metadata.id.as_str())
        );
        assert_eq!(
            fork_session.metadata.forked_from_message_count,
            Some(messages.len())
        );

        let (backtrack, _, _, _) = manager.fork_at_user_message(&thread.id, 0).await?;
        assert_eq!(manager.restore_thread_messages(&backtrack)?, messages[..2]);
        let backtrack_session_id = backtrack
            .session_id
            .clone()
            .context("a backtracking fork should own a session document")?;
        assert_ne!(backtrack_session_id, saved.metadata.id);
        assert_eq!(
            sessions.load_session(&backtrack_session_id)?.messages,
            messages[..2].to_vec()
        );
        let (empty, _, _, _) = manager.fork_at_user_message(&thread.id, 1).await?;
        assert!(manager.restore_thread_messages(&empty)?.is_empty());
        assert_eq!(manager.restore_thread_messages(&thread)?, messages);
        let mut legacy = thread.clone();
        legacy.saved_session_checkpoint = None;
        assert_eq!(manager.restore_thread_messages(&legacy)?, messages);
        let tail = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "new tail".into(),
                cache_control: None,
            }],
        }];
        manager.seed_thread_from_messages(&thread.id, &tail).await?;
        let mut expected = messages.clone();
        expected.extend(tail);
        assert_eq!(manager.restore_thread_messages(&legacy)?, expected);
        let mut missing = thread.clone();
        missing
            .saved_session_checkpoint
            .as_mut()
            .unwrap()
            .covered_turn_id = Some("turn_missing".into());
        assert!(
            manager
                .restore_thread_messages(&missing)
                .unwrap_err()
                .to_string()
                .contains("checkpoint turn turn_missing is missing")
        );
        let mut changed = saved.clone();
        changed.messages[0] = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "conflicting history".into(),
                cache_control: None,
            }],
        };
        sessions.save_session(&changed)?;
        // A rewritten prefix is real divergence. The thread no longer strands
        // on it (#6144): the dead binding is dropped, kept in a receipt, and
        // the thread hydrates from its own turns.
        let from_turns = manager.restore_thread_messages(&thread)?;
        assert_eq!(
            from_turns.len(),
            manager
                .reconstruct_messages_from_turns(&manager.store.list_turns_for_thread(&thread.id)?)?
                .len()
        );
        let stored = manager.store.load_thread(&thread.id)?;
        assert_eq!(stored.session_id, None);
        assert_eq!(stored.saved_session_checkpoint, None);
        let receipts = std::fs::read_to_string(
            dir.path()
                .join("runtime")
                .join(crate::session_reconcile::THREAD_UNBIND_RECEIPTS_FILE),
        )?;
        assert!(receipts.contains("thread_unbound"), "{receipts}");
        assert!(receipts.contains(&saved.metadata.id), "{receipts}");
        // The source saving new content is precisely what used to break the
        // fork: both threads named this file, so the source's save rewrote the
        // bytes under the fork's checkpoint.
        assert_eq!(manager.restore_thread_messages(&fork)?, messages);
        // A legacy link that no longer matches loads from turns, too; it only
        // unbinds when the stored record still carries that same link.
        assert!(manager.restore_thread_messages(&legacy).is_ok());
        sessions.save_session(&saved)?;
        assert_eq!(manager.restore_thread_messages(&thread)?, expected);
        Ok(())
    }

    fn msg(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![ContentBlock::Text {
                text: text.into(),
                cache_control: None,
            }],
        }
    }

    /// #6144 P4: the checkpoint covers a prefix. The document's own
    /// conversation appending to it (a TUI autosave of the same id, a later
    /// PUT) used to strand the thread with "changed after this thread's
    /// checkpoint"; now the thread keeps exactly its prefix plus its own turns.
    /// A legacy whole-document checkpoint is migrated on its first read.
    #[tokio::test]
    async fn appended_document_keeps_the_thread_on_its_checkpoint_prefix() -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let manager = RuntimeThreadManager::open(
            config(),
            dir.path().to_path_buf(),
            test_manager_config(dir.path().join("runtime")),
        )?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let prefix = vec![
            msg(Role::User, "first"),
            msg(Role::Assistant, "first answer"),
        ];
        manager
            .seed_thread_from_messages(&thread.id, &prefix)
            .await?;
        let sessions = crate::session_manager::SessionManager::new(
            crate::session_manager::default_sessions_dir()?,
        )?;
        let mut document = crate::session_manager::create_saved_session_with_id_and_mode(
            Uuid::new_v4().to_string(),
            &prefix,
            &thread.model,
            dir.path(),
            0,
            None,
            Some("agent"),
        );
        sessions.save_session(&document)?;
        {
            let _admission = manager.session_checkpoint_guard().await;
            manager
                .set_thread_session_checkpoint(&thread.id, &document)
                .await?;
        }
        let bound = manager.get_thread(&thread.id).await?;
        assert_eq!(
            bound
                .saved_session_checkpoint
                .as_ref()
                .and_then(|checkpoint| checkpoint.messages_len),
            Some(2)
        );

        // The document's conversation moves on; so does the thread.
        document.messages.extend([
            msg(Role::User, "elsewhere"),
            msg(Role::Assistant, "elsewhere answer"),
        ]);
        sessions.save_session(&document)?;
        let tail = vec![msg(Role::User, "thread tail")];
        manager.seed_thread_from_messages(&thread.id, &tail).await?;
        let mut expected = prefix.clone();
        expected.extend(tail);
        assert_eq!(manager.restore_thread_messages(&bound)?, expected);

        // A checkpoint written before `messages_len` fingerprints the whole
        // document as it was at bind time; the prefix search finds it and the
        // length is written back.
        let mut legacy = manager.store.load_thread(&thread.id)?;
        legacy
            .saved_session_checkpoint
            .as_mut()
            .expect("checkpoint")
            .messages_len = None;
        manager.store.save_thread(&legacy)?;
        assert_eq!(manager.restore_thread_messages(&legacy)?, expected);
        assert_eq!(
            manager
                .store
                .load_thread(&thread.id)?
                .saved_session_checkpoint
                .and_then(|checkpoint| checkpoint.messages_len),
            Some(2)
        );

        // P5: the document is deleted. The thread unbinds and loads from turns.
        sessions.delete_session(&document.metadata.id)?;
        let orphaned = manager.store.load_thread(&thread.id)?;
        let from_turns = manager.restore_thread_messages(&orphaned)?;
        assert_eq!(from_turns.len(), 3, "{from_turns:?}");
        assert_eq!(manager.store.load_thread(&thread.id)?.session_id, None);
        Ok(())
    }

    /// The legacy prefix search must agree with `serde_json`'s own encoding
    /// of a slice, for every prefix length.
    #[test]
    fn checkpoint_prefix_search_matches_slice_fingerprints() -> Result<()> {
        let messages = vec![
            msg(Role::User, "a"),
            msg(Role::Assistant, "b \"quoted\" \u{1F40B}"),
            msg(Role::User, "c"),
        ];
        for len in 0..=messages.len() {
            let checkpoint = SavedSessionCheckpoint {
                covered_turn_id: None,
                messages_sha256: session_messages_sha256(&messages[..len])?,
                messages_len: None,
                retained_messages: None,
            };
            assert_eq!(checkpoint_prefix_len(&checkpoint, &messages)?, Some(len));
            let exact = SavedSessionCheckpoint {
                messages_len: Some(len),
                ..checkpoint
            };
            assert_eq!(checkpoint_prefix_len(&exact, &messages)?, Some(len));
        }
        let diverged = SavedSessionCheckpoint {
            covered_turn_id: None,
            messages_sha256: session_messages_sha256(&[msg(Role::User, "other")])?,
            messages_len: Some(1),
            retained_messages: None,
        };
        assert_eq!(checkpoint_prefix_len(&diverged, &messages)?, None);
        Ok(())
    }

    /// #6144 P9: a Runtime thread's engine writes under the id its export will
    /// use, not a fresh random id per load that no document ever names.
    #[tokio::test]
    async fn runtime_thread_engine_uses_the_thread_session_id() -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let manager = RuntimeThreadManager::open(
            config(),
            dir.path().to_path_buf(),
            test_manager_config(dir.path().join("runtime")),
        )?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let engine = manager.get_engine(&thread.id).await?;
        let snapshot = engine.get_session_snapshot().await?;
        assert_eq!(snapshot.session_id, thread_session_id(&thread.id));
        assert_eq!(thread_session_id(&thread.id), thread_session_id(&thread.id));
        assert_ne!(
            thread_session_id(&thread.id),
            thread_session_id("thr_other")
        );
        close_engines(&manager).await?;
        Ok(())
    }

    /// #6144 P7: one corrupt thread record no longer fails the whole rail.
    #[tokio::test]
    async fn one_corrupt_thread_record_does_not_fail_the_listing() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let manager = RuntimeThreadManager::open(
            config(),
            dir.path().to_path_buf(),
            test_manager_config(dir.path().join("runtime")),
        )?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        std::fs::write(
            dir.path()
                .join("runtime")
                .join("threads")
                .join("thr_torn.json"),
            b"{\"id\": ",
        )?;
        let (threads, skipped) = manager.store.list_threads_lenient()?;
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, thread.id);
        assert_eq!(skipped.len(), 1);
        assert_eq!(manager.store.list_threads()?.len(), 1);
        Ok(())
    }

    /// A thread can carry `session_id` without a checkpoint: the binding
    /// outlived the field that records where its prefix ends (45 of 124 threads
    /// in one real store). Such a thread resolves its prefix by walking turn
    /// projections instead, so a fork of it has to learn the same boundary. A
    /// fork that stored "the prefix covers no turns" hydrated as the prefix
    /// *plus* the turns the prefix already contains — its history, twice.
    #[tokio::test]
    async fn fork_of_a_legacy_session_link_keeps_one_copy_of_the_history() -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let manager = RuntimeThreadManager::open(
            config(),
            dir.path().to_path_buf(),
            test_manager_config(dir.path().join("runtime")),
        )?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let messages: Vec<Message> = serde_json::from_value(json!([
            {"role":"user","content":[{"type":"text","text":"first"}]},
            {"role":"assistant","content":[{"type":"text","text":"first answer"}]},
            {"role":"user","content":[{"type":"text","text":"second"}]},
            {"role":"assistant","content":[{"type":"text","text":"second answer"}]}
        ]))?;
        manager
            .seed_thread_from_messages(&thread.id, &messages)
            .await?;
        let saved = crate::session_manager::create_saved_session_with_id_and_mode(
            Uuid::new_v4().to_string(),
            &messages,
            &thread.model,
            dir.path(),
            0,
            None,
            Some("agent"),
        );
        let sessions = crate::session_manager::SessionManager::new(
            crate::session_manager::default_sessions_dir()?,
        )?;
        sessions.save_session(&saved)?;
        // Bind it the way the older writer did: id only, no cursor.
        let mut linked = manager.get_thread(&thread.id).await?;
        linked.session_id = Some(saved.metadata.id.clone());
        linked.saved_session_checkpoint = None;
        manager.store.save_thread(&linked)?;

        let fork = manager.fork_thread(&thread.id).await?;
        assert_ne!(
            fork.session_id.as_deref(),
            Some(saved.metadata.id.as_str()),
            "the fork must own its own document"
        );
        assert_eq!(manager.restore_thread_messages(&fork)?, messages);
        let (backtrack, _, _, _) = manager.fork_at_user_message(&thread.id, 0).await?;
        assert_eq!(manager.restore_thread_messages(&backtrack)?, messages[..2]);
        Ok(())
    }

    /// A backtracking fork is prepared before the caller's file undo runs and
    /// published only after it succeeds. Its session document must be written
    /// at publish time: writing it while preparing left an unreferenced copy
    /// of the conversation behind every time the undo was refused (an
    /// untrusted thread) or failed, which is the orphaned-document shape
    /// #6406 exists to stop producing.
    #[tokio::test]
    async fn abandoned_backtrack_fork_leaves_no_session_document() -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let manager = RuntimeThreadManager::open(
            config(),
            dir.path().to_path_buf(),
            test_manager_config(dir.path().join("runtime")),
        )?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let messages: Vec<Message> = serde_json::from_value(json!([
            {"role":"user","content":[{"type":"text","text":"first"}]},
            {"role":"assistant","content":[{"type":"text","text":"first answer"}]},
            {"role":"user","content":[{"type":"text","text":"second"}]},
            {"role":"assistant","content":[{"type":"text","text":"second answer"}]}
        ]))?;
        manager
            .seed_thread_from_messages(&thread.id, &messages)
            .await?;
        let saved = crate::session_manager::create_saved_session_with_id_and_mode(
            Uuid::new_v4().to_string(),
            &messages,
            &thread.model,
            dir.path(),
            0,
            None,
            Some("agent"),
        );
        let sessions_dir = crate::session_manager::default_sessions_dir()?;
        let sessions = crate::session_manager::SessionManager::new(sessions_dir.clone())?;
        {
            let _admission = manager.session_checkpoint_guard().await;
            sessions.save_session(&saved)?;
            manager
                .set_thread_session_checkpoint(&thread.id, &saved)
                .await?;
        }
        let documents = || -> Result<usize> { Ok(std::fs::read_dir(&sessions_dir)?.count()) };
        let before = documents()?;

        let abandoned = manager.prepare_fork_at_user_message(&thread.id, 0).await?;
        drop(abandoned);
        assert_eq!(
            documents()?,
            before,
            "an abandoned fork must not write a session document"
        );

        let prepared = manager.prepare_fork_at_user_message(&thread.id, 0).await?;
        let (fork, _, _, _) = manager.publish_prepared_fork(prepared).await?;
        let fork_session_id = fork
            .session_id
            .clone()
            .context("a published fork should own a session document")?;
        assert_ne!(fork_session_id, saved.metadata.id);
        assert!(documents()? > before);
        sessions.load_session(&fork_session_id)?;
        assert_eq!(manager.restore_thread_messages(&fork)?, messages[..2]);
        Ok(())
    }

    /// A backtrack over a turn that used a file tool must still find the
    /// boundary it keeps.
    ///
    /// The saved transcript is the model-visible history, so it carries the
    /// per-turn `<turn_meta>` preamble (rebuilt from runtime facts, never
    /// recorded on an item) and tool results as the route's compaction left
    /// them (items keep the raw output). Neither is reproducible from the
    /// records, so a two-turn conversation whose first turn wrote a file could
    /// not be cut at all: undo answered "Cannot identify an exact saved-history
    /// boundary for this backtrack" and left the turn in place — reported from
    /// the VS Code client on engine 0.10.0, where reverting one file of the
    /// same conversation worked.
    #[tokio::test]
    async fn backtrack_over_a_file_tool_turn_finds_the_saved_history_boundary() -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let manager = RuntimeThreadManager::open(
            config(),
            dir.path().to_path_buf(),
            test_manager_config(dir.path().join("runtime")),
        )?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let base = Utc::now();
        let at = |seconds: i64| Some(base + chrono::Duration::seconds(seconds));

        // A file tool records itself as a `FileChange` item that carries the
        // call identity — the same shape the transcript's tool_use/tool_result
        // pair came from.
        fn tool_metadata(call: &str, name: &str, input: &str) -> Value {
            json!({
                "tool_use_id": call,
                "tool_name": name,
                "tool_input": input,
                "tool_result_for": call,
                "is_error": false,
            })
        }
        let write_call = "call_write_test";
        let bash_call = "call_bash_test";
        let write_item = |id: &str, turn: &str, order: i64, path: &str, call: &str| {
            let mut item = sample_item(turn, id, TurnItemLifecycleStatus::Completed);
            item.kind = TurnItemKind::FileChange;
            item.started_at = at(order);
            item.ended_at = item.started_at;
            item.detail = Some(format!("Successfully wrote 5 bytes to {path}"));
            item.metadata = Some(tool_metadata(
                call,
                "write",
                &format!("{{\"path\":\"{path}\",\"content\":\"x\\n\"}}"),
            ));
            item
        };
        let text_item = |id: &str, turn: &str, order: i64, kind: TurnItemKind, text: &str| {
            let mut item = sample_item(turn, id, TurnItemLifecycleStatus::Completed);
            item.kind = kind;
            item.started_at = at(order);
            item.ended_at = item.started_at;
            item.summary = text.to_string();
            item.detail = Some(text.to_string());
            item
        };

        let turn_one = "turn_writes_file";
        let turn_two = "turn_after";
        let items = vec![
            text_item(
                "item_u1",
                turn_one,
                0,
                TurnItemKind::UserMessage,
                "write test.txt",
            ),
            text_item(
                "item_r1",
                turn_one,
                1,
                TurnItemKind::AgentReasoning,
                "I will write it.",
            ),
            write_item("item_f1", turn_one, 2, "test.txt", write_call),
            {
                let mut item = sample_item(turn_one, "item_t1", TurnItemLifecycleStatus::Completed);
                item.kind = TurnItemKind::ToolCall;
                item.started_at = at(3);
                item.ended_at = item.started_at;
                // The raw output keeps the newline the route's compaction trims
                // before the model ever sees it.
                item.detail = Some("test.txt\n".to_string());
                item.metadata = Some(tool_metadata(
                    bash_call,
                    "bash",
                    "{\"command\":\"ls -l test.txt\"}",
                ));
                item
            },
            text_item(
                "item_a1",
                turn_one,
                4,
                TurnItemKind::AgentMessage,
                "created test.txt",
            ),
            text_item(
                "item_u2",
                turn_two,
                5,
                TurnItemKind::UserMessage,
                "write test2.txt",
            ),
            write_item("item_f2", turn_two, 6, "test2.txt", "call_write_second"),
            text_item(
                "item_a2",
                turn_two,
                7,
                TurnItemKind::AgentMessage,
                "created test2.txt",
            ),
        ];
        for item in &items {
            manager.store.save_item(item)?;
        }
        for (turn_id, order) in [(turn_one, 0), (turn_two, 10)] {
            let mut turn = sample_turn(&thread.id, turn_id, RuntimeTurnStatus::Completed);
            turn.started_at = at(order);
            turn.ended_at = turn.started_at;
            manager.store.save_turn(&turn)?;
        }
        let mut stored = manager.get_thread(&thread.id).await?;
        stored.latest_turn_id = Some(turn_two.to_string());
        manager.store.save_thread(&stored)?;

        // The model-visible transcript: prompts (each with the per-turn
        // preamble), the write pair the items cannot reproduce, and the bash
        // result as the route left it.
        let transcript: Vec<Message> = serde_json::from_value(json!([
            {"role":"user","content":[
                {"type":"text","text":"write test.txt"},
                {"type":"text","text":"<turn_meta>\nCurrent local date: 2026-09-24\n</turn_meta>"}]},
            {"role":"assistant","content":[
                {"type":"thinking","thinking":"I will write it."},
                {"type":"tool_use","id":write_call,"name":"write","input":{"path":"test.txt","content":"x\n"}}]},
            {"role":"user","content":[
                {"type":"tool_result","tool_use_id":write_call,"content":"Successfully wrote 5 bytes to test.txt"}]},
            {"role":"assistant","content":[
                {"type":"thinking","thinking":"verify"},
                {"type":"tool_use","id":bash_call,"name":"bash","input":{"command":"ls -l test.txt"}}]},
            {"role":"user","content":[
                {"type":"tool_result","tool_use_id":bash_call,"content":"test.txt"}]},
            {"role":"assistant","content":[
                {"type":"text","text":"created test.txt"}]},
            {"role":"user","content":[
                {"type":"text","text":"write test2.txt"},
                {"type":"text","text":"<turn_meta>\nCurrent local date: 2026-09-24\n</turn_meta>"}]},
            {"role":"assistant","content":[
                {"type":"text","text":"created test2.txt"}]}
        ]))?;
        let saved = crate::session_manager::create_saved_session_with_id_and_mode(
            Uuid::new_v4().to_string(),
            &transcript,
            &thread.model,
            dir.path(),
            0,
            None,
            Some("agent"),
        );
        let sessions = crate::session_manager::SessionManager::new(
            crate::session_manager::default_sessions_dir()?,
        )?;
        {
            let _admission = manager.session_checkpoint_guard().await;
            sessions.save_session(&saved)?;
            manager
                .set_thread_session_checkpoint(&thread.id, &saved)
                .await?;
        }

        let prepared = manager.prepare_fork_at_user_message(&thread.id, 0).await?;
        let (_, prefix, _) = &prepared.own_session;
        assert_eq!(
            prefix.as_slice(),
            &transcript[..6],
            "the fork keeps exactly the messages before the undone turn"
        );
        let (fork, _, _, _) = manager.publish_prepared_fork(prepared).await?;
        assert_eq!(manager.restore_thread_messages(&fork)?, transcript[..6]);

        // Undoing the second turn back (`depth` 1) keeps nothing: the first
        // turn's own prompt is the anchor, so the boundary is the first message.
        let first_turn = manager.prepare_fork_at_user_message(&thread.id, 1).await?;
        let (_, prefix, _) = &first_turn.own_session;
        assert!(prefix.is_empty(), "undoing the first turn keeps no history");
        drop(first_turn); // An abandoned preparation releases its captured source lease.

        // A transcript that no longer lines up with the turn records must refuse
        // rather than cut where it cannot account for the history: the undone
        // turn's prompt is the anchor here, not a coincidence to match.
        let mut drifted = transcript.clone();
        drifted[6] = serde_json::from_value(json!({
            "role":"user","content":[
                {"type":"text","text":"a prompt no turn records"},
                {"type":"text","text":"<turn_meta>\nCurrent local date: 2026-09-24\n</turn_meta>"}]}))?;
        let drifted_session = crate::session_manager::create_saved_session_with_id_and_mode(
            saved.metadata.id.clone(),
            &drifted,
            &thread.model,
            dir.path(),
            0,
            None,
            Some("agent"),
        );
        {
            let _admission = manager.session_checkpoint_guard().await;
            sessions.save_session(&drifted_session)?;
            manager
                .set_thread_session_checkpoint(&thread.id, &drifted_session)
                .await?;
        }
        let error = match manager.prepare_fork_at_user_message(&thread.id, 0).await {
            Ok(_) => panic!("a drifted transcript must not be cut"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("Cannot identify an exact saved-history boundary"),
            "the refusal names the boundary it could not find: {error}"
        );
        Ok(())
    }

    /// A fork is refused for the history it carries, not for the shape it is
    /// stored in.
    ///
    /// A saved document repeats its own conversation while it carries a
    /// journal: `messages` and the journal's active branch hold the same
    /// turns, so its file is up to twice the history it holds. The reader that
    /// opens such a document already knows this — an oversized file is judged
    /// by `SavedSession::transport_form` — but the fork writer judged the
    /// stored shape against the same 8 MiB bound. A conversation whose file
    /// passed 8 MiB could therefore be opened, saved and continued for several
    /// turns, yet never forked, and so never undone, retried or branched;
    /// reported from the VS Code client on a 9.8 MiB conversation
    /// (`/undo` answered "full history exceeds byte transport bound") whose
    /// carried form was 5.2 MiB.
    #[tokio::test]
    async fn a_fork_measures_the_history_it_carries_not_the_shape_it_stores() -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let manager = RuntimeThreadManager::open(
            config(),
            dir.path().to_path_buf(),
            test_manager_config(dir.path().join("runtime")),
        )?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let base = Utc::now();
        let at = |seconds: i64| Some(base + chrono::Duration::seconds(seconds));

        // One tool result big enough that the document's two copies of it
        // (messages and journal) cross the bound while either copy alone does
        // not. The margin is deliberate: it must stay over the bound on the
        // stored side and under it on the carried side.
        let bulk = "x".repeat(4 * 1024 * 1024 + 512 * 1024);
        let bash_call = "call_bash_bulk";
        let turn_one = "turn_bulk_output";
        let turn_two = "turn_after_bulk";

        let text_item = |id: &str, turn: &str, order: i64, kind: TurnItemKind, text: &str| {
            let mut item = sample_item(turn, id, TurnItemLifecycleStatus::Completed);
            item.kind = kind;
            item.started_at = at(order);
            item.ended_at = item.started_at;
            item.summary = text.to_string();
            item.detail = Some(text.to_string());
            item
        };
        let mut bulk_call = sample_item(turn_one, "item_bulk", TurnItemLifecycleStatus::Completed);
        bulk_call.kind = TurnItemKind::ToolCall;
        bulk_call.started_at = at(1);
        bulk_call.ended_at = bulk_call.started_at;
        bulk_call.summary = "bash: bulk output".to_string();
        bulk_call.detail = Some(bulk.clone());
        bulk_call.metadata = Some(json!({
            "tool_use_id": bash_call,
            "tool_name": "bash",
            "tool_input": "{\"command\":\"cat big.txt\"}",
            "tool_result_for": bash_call,
            "is_error": false,
        }));

        let items = vec![
            text_item(
                "item_u1",
                turn_one,
                0,
                TurnItemKind::UserMessage,
                "read big.txt",
            ),
            bulk_call,
            text_item(
                "item_a1",
                turn_one,
                2,
                TurnItemKind::AgentMessage,
                "that is a lot of text",
            ),
            text_item(
                "item_u2",
                turn_two,
                3,
                TurnItemKind::UserMessage,
                "now undo that",
            ),
            text_item("item_a2", turn_two, 4, TurnItemKind::AgentMessage, "done"),
        ];
        for item in &items {
            manager.store.save_item(item)?;
        }
        for (turn_id, order) in [(turn_one, 0), (turn_two, 10)] {
            let mut turn = sample_turn(&thread.id, turn_id, RuntimeTurnStatus::Completed);
            turn.started_at = at(order);
            turn.ended_at = turn.started_at;
            manager.store.save_turn(&turn)?;
        }
        let mut stored = manager.get_thread(&thread.id).await?;
        stored.latest_turn_id = Some(turn_two.to_string());
        manager.store.save_thread(&stored)?;

        let transcript: Vec<Message> = serde_json::from_value(json!([
            {"role":"user","content":[
                {"type":"text","text":"read big.txt"},
                {"type":"text","text":"<turn_meta>\nCurrent local date: 2026-09-24\n</turn_meta>"}]},
            {"role":"assistant","content":[
                {"type":"tool_use","id":bash_call,"name":"bash","input":{"command":"cat big.txt"}}]},
            {"role":"user","content":[
                {"type":"tool_result","tool_use_id":bash_call,"content":bulk}]},
            {"role":"assistant","content":[
                {"type":"text","text":"that is a lot of text"}]},
            {"role":"user","content":[
                {"type":"text","text":"now undo that"},
                {"type":"text","text":"<turn_meta>\nCurrent local date: 2026-09-24\n</turn_meta>"}]},
            {"role":"assistant","content":[
                {"type":"text","text":"done"}]}
        ]))?;
        let saved = crate::session_manager::create_saved_session_with_id_and_mode(
            Uuid::new_v4().to_string(),
            &transcript,
            &thread.model,
            dir.path(),
            0,
            None,
            Some("agent"),
        );
        let sessions = crate::session_manager::SessionManager::new(
            crate::session_manager::default_sessions_dir()?,
        )?;
        {
            let _admission = manager.session_checkpoint_guard().await;
            sessions.save_session(&saved)?;
            manager
                .set_thread_session_checkpoint(&thread.id, &saved)
                .await?;
        }

        // The fixture has to be in the failing regime, or the test proves
        // nothing: the file on disk is over the bound, the history it carries
        // is not.
        let bound = codewhale_protocol::MAX_CANONICAL_HISTORY_BYTES;
        let written = std::fs::metadata(
            crate::session_manager::default_sessions_dir()?
                .join(format!("{}.json", saved.metadata.id)),
        )?
        .len();
        let carried = serde_json::to_vec(&saved.transport_form())?.len();
        assert!(
            written > bound as u64,
            "the stored document must exceed the bound: {written} bytes"
        );
        assert!(
            carried <= bound,
            "the carried history must fit the bound: {carried} bytes"
        );

        let prepared = manager.prepare_fork_at_user_message(&thread.id, 0).await?;
        let (_, prefix, _) = &prepared.own_session;
        assert_eq!(
            prefix.as_slice(),
            &transcript[..4],
            "the fork keeps exactly the messages before the undone turn"
        );
        let (fork, _, _, _) = manager.publish_prepared_fork(prepared).await?;
        assert_eq!(manager.restore_thread_messages(&fork)?, transcript[..4]);
        Ok(())
    }

    /// A fork's checkpoint has to describe the document as a reader sees it.
    ///
    /// A saved session is repaired on the way out, and `resume_session` writes
    /// that repair back: tool calls and results are re-paired, and a prefix
    /// written from turn records can hold a shape the repair changes. A
    /// fingerprint taken over the written shape then describes bytes no reader
    /// sees — `thread_holding_session` rejects the fork's own thread, resuming
    /// the session starts a second one, and that second thread's save leaves
    /// the first stale, so the branch's next message fails with "Saved session
    /// … changed after this thread's checkpoint".
    #[tokio::test]
    async fn a_fork_fingerprints_the_document_its_readers_see() -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let manager = RuntimeThreadManager::open(
            config(),
            dir.path().to_path_buf(),
            test_manager_config(dir.path().join("runtime")),
        )?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let base = Utc::now();
        let at = |seconds: i64| Some(base + chrono::Duration::seconds(seconds));

        fn tool_metadata(call: &str, name: &str, input: &str) -> Value {
            json!({
                "tool_use_id": call,
                "tool_name": name,
                "tool_input": input,
                "tool_result_for": call,
                "is_error": false,
            })
        }
        let write_call = "call_write_fingerprint";
        let bash_call = "call_bash_fingerprint";
        let turn_one = "turn_fingerprint_one";
        let turn_two = "turn_fingerprint_two";

        let text_item = |id: &str, turn: &str, order: i64, kind: TurnItemKind, text: &str| {
            let mut item = sample_item(turn, id, TurnItemLifecycleStatus::Completed);
            item.kind = kind;
            item.started_at = at(order);
            item.ended_at = item.started_at;
            item.summary = text.to_string();
            item.detail = Some(text.to_string());
            item
        };
        // Two tool calls in one turn: the shapes a reconstruction and the
        // session repair do not have to agree on.
        let mut write_item = sample_item(turn_one, "item_f1", TurnItemLifecycleStatus::Completed);
        write_item.kind = TurnItemKind::FileChange;
        write_item.started_at = at(2);
        write_item.ended_at = write_item.started_at;
        write_item.detail = Some("Successfully wrote 5 bytes to test.txt".to_string());
        write_item.metadata = Some(tool_metadata(
            write_call,
            "write",
            "{\"path\":\"test.txt\",\"content\":\"x\\n\"}",
        ));
        let mut bash_item = sample_item(turn_one, "item_t1", TurnItemLifecycleStatus::Completed);
        bash_item.kind = TurnItemKind::ToolCall;
        bash_item.started_at = at(3);
        bash_item.ended_at = bash_item.started_at;
        bash_item.detail = Some("test.txt\n".to_string());
        bash_item.metadata = Some(tool_metadata(
            bash_call,
            "bash",
            "{\"command\":\"ls -l test.txt\"}",
        ));
        // A third call whose result never arrived — an interrupted turn. The
        // records keep the call, and the session repair pairs it on the way
        // out, so what a reader sees is not what was written.
        let mut interrupted = sample_item(turn_one, "item_t2", TurnItemLifecycleStatus::Failed);
        interrupted.kind = TurnItemKind::ToolCall;
        interrupted.started_at = at(4);
        interrupted.ended_at = interrupted.started_at;
        interrupted.summary = "bash (interrupted)".to_string();
        interrupted.metadata = Some(json!({
            "tool_use_id": "call_interrupted",
            "tool_name": "bash",
            "tool_input": "{\"command\":\"sleep 30\"}",
        }));

        for item in [
            text_item(
                "item_u1",
                turn_one,
                0,
                TurnItemKind::UserMessage,
                "write test.txt",
            ),
            text_item(
                "item_r1",
                turn_one,
                1,
                TurnItemKind::AgentReasoning,
                "I will write it.",
            ),
            write_item,
            bash_item,
            interrupted,
            text_item(
                "item_a1",
                turn_one,
                5,
                TurnItemKind::AgentMessage,
                "created test.txt",
            ),
            text_item(
                "item_u2",
                turn_two,
                5,
                TurnItemKind::UserMessage,
                "now read it",
            ),
            text_item(
                "item_a2",
                turn_two,
                6,
                TurnItemKind::AgentMessage,
                "read it",
            ),
            text_item(
                "item_c1",
                turn_two,
                7,
                TurnItemKind::ContextCompaction,
                "Made room: 1383 → 21 messages",
            ),
        ] {
            manager.store.save_item(&item)?;
        }
        for (turn_id, order) in [(turn_one, 0), (turn_two, 10)] {
            let mut turn = sample_turn(&thread.id, turn_id, RuntimeTurnStatus::Completed);
            turn.started_at = at(order);
            turn.ended_at = turn.started_at;
            manager.store.save_turn(&turn)?;
        }
        let mut stored = manager.get_thread(&thread.id).await?;
        stored.latest_turn_id = Some(turn_two.to_string());
        manager.store.save_thread(&stored)?;

        // The compacted source's document: the prompts verbatim, the work
        // between them summarized away — what a branch rebuilds from the
        // records instead of slicing.
        let compacted: Vec<Message> = serde_json::from_value(json!([
            {"role":"user","content":[{"type":"text","text":"write test.txt"}]},
            {"role":"user","content":[{"type":"text","text":"now read it"}]}
        ]))?;
        let saved = crate::session_manager::create_saved_session_with_id_and_mode(
            Uuid::new_v4().to_string(),
            &compacted,
            &thread.model,
            dir.path(),
            0,
            None,
            Some("agent"),
        );
        let sessions = crate::session_manager::SessionManager::new(
            crate::session_manager::default_sessions_dir()?,
        )?;
        {
            let _admission = manager.session_checkpoint_guard().await;
            sessions.save_session(&saved)?;
            manager
                .set_thread_session_checkpoint(&thread.id, &saved)
                .await?;
        }

        // Undo the last turn: the fork keeps the first one, tool calls and all.
        let (fork, _, _, _) = manager.fork_at_user_message(&thread.id, 0).await?;
        let session_id = fork
            .session_id
            .clone()
            .context("a published fork owns a session document")?;
        let checkpoint = fork
            .saved_session_checkpoint
            .as_ref()
            .context("a published fork carries a checkpoint")?;

        // What every reader sees: `resume_session` repairs the document and
        // writes the repair back, which is the shape the fingerprint must name.
        let read_back = sessions.resume_session(&session_id)?.session;
        assert_eq!(
            checkpoint.messages_sha256,
            session_messages_sha256(&read_back.messages)?,
            "the fork's checkpoint must describe the document as readers see it: {:#?}",
            read_back.messages
        );

        // And the thread that holds the session is recognised, so opening the
        // session again reuses it instead of adding a second thread whose save
        // would leave this one stale.
        assert_eq!(
            manager
                .thread_holding_session(&session_id, &read_back)
                .map(|held| held.id),
            Some(fork.id.clone()),
            "resuming the fork's session must find the fork's own thread"
        );
        Ok(())
    }
    ///
    /// Compaction rewrites the model-visible history in place: the prompts stay
    /// A fork of a *compacted* conversation keeps the whole exchange.
    ///
    /// Compaction rewrites the model-visible history in place: the prompts stay
    /// verbatim, the answers and the tool work around them are summarized into
    /// the session's system prompt. Slicing that abbreviation into a fork's own
    /// document claims, through its checkpoint, coverage the document does not
    /// carry — a client then reads a run of prompts with the agent's answers
    /// missing, and `restore_thread_messages` cannot rebuild them either,
    /// because the checkpoint tells it the prefix covers those turns already.
    ///
    /// The records keep every item of every turn, so the fork rebuilds the kept
    /// exchanges instead of copying the summary's view of them.
    #[tokio::test]
    async fn fork_of_a_compacted_conversation_keeps_the_agent_output() -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let manager = RuntimeThreadManager::open(
            config(),
            dir.path().to_path_buf(),
            test_manager_config(dir.path().join("runtime")),
        )?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let base = Utc::now();
        let at = |seconds: i64| Some(base + chrono::Duration::seconds(seconds));

        let text_item = |id: &str, turn: &str, order: i64, kind: TurnItemKind, text: &str| {
            let mut item = sample_item(turn, id, TurnItemLifecycleStatus::Completed);
            item.kind = kind;
            item.started_at = at(order);
            item.ended_at = item.started_at;
            item.summary = text.to_string();
            item.detail = Some(text.to_string());
            item
        };

        let turn_one = "turn_before_compaction";
        let turn_two = "turn_of_compaction";
        for item in [
            text_item(
                "item_u1",
                turn_one,
                0,
                TurnItemKind::UserMessage,
                "write test.txt",
            ),
            text_item(
                "item_a1",
                turn_one,
                1,
                TurnItemKind::AgentMessage,
                "created test.txt",
            ),
            text_item(
                "item_u2",
                turn_two,
                2,
                TurnItemKind::UserMessage,
                "write test2.txt",
            ),
            text_item(
                "item_a2",
                turn_two,
                3,
                TurnItemKind::AgentMessage,
                "created test2.txt",
            ),
            text_item(
                "item_c1",
                turn_two,
                4,
                TurnItemKind::ContextCompaction,
                "Made room: 1383 → 21 messages",
            ),
        ] {
            manager.store.save_item(&item)?;
        }
        for (turn_id, order) in [(turn_one, 0), (turn_two, 10)] {
            let mut turn = sample_turn(&thread.id, turn_id, RuntimeTurnStatus::Completed);
            turn.started_at = at(order);
            turn.ended_at = turn.started_at;
            manager.store.save_turn(&turn)?;
        }
        let mut stored = manager.get_thread(&thread.id).await?;
        stored.latest_turn_id = Some(turn_two.to_string());
        manager.store.save_thread(&stored)?;

        // What the engine persisted once compaction had run: the two prompts
        // verbatim, and none of the work that happened between them. The
        // summary itself rides in the session's system prompt, which is why the
        // saved messages read as a bare list of what was asked.
        let compacted: Vec<Message> = serde_json::from_value(json!([
            {"role":"user","content":[{"type":"text","text":"write test.txt"}]},
            {"role":"user","content":[{"type":"text","text":"write test2.txt"}]}
        ]))?;
        let saved = crate::session_manager::create_saved_session_with_id_and_mode(
            Uuid::new_v4().to_string(),
            &compacted,
            &thread.model,
            dir.path(),
            0,
            None,
            Some("agent"),
        );
        let sessions = crate::session_manager::SessionManager::new(
            crate::session_manager::default_sessions_dir()?,
        )?;
        {
            let _admission = manager.session_checkpoint_guard().await;
            sessions.save_session(&saved)?;
            manager
                .set_thread_session_checkpoint(&thread.id, &saved)
                .await?;
        }

        // Branch at the last turn, the way the transcript's "continue from
        // here" row does: the whole conversation is kept, and every bit of it
        // has to survive into the fork's own document.
        let (fork, _, _, _) = manager.fork_at_user_turn(&thread.id, turn_two).await?;
        let fork_session_id = fork
            .session_id
            .clone()
            .context("a published fork owns a session document")?;
        assert_ne!(fork_session_id, saved.metadata.id);
        let document = sessions.load_session(&fork_session_id)?;
        let roles: Vec<&str> = document
            .messages
            .iter()
            .map(|message| message.role.as_str())
            .collect();
        assert_eq!(
            roles,
            ["user", "assistant", "user", "assistant"],
            "the fork's document is the exchange, not the {} saved prompts: {:#?}",
            compacted.len(),
            document.messages
        );
        let restored = manager.restore_thread_messages(&fork)?;
        assert_eq!(
            document.messages, restored,
            "the document a client reads is the conversation the engine restores"
        );
        let rendered = serde_json::to_string(&document.messages)?;
        for answer in ["created test.txt", "created test2.txt"] {
            assert!(
                rendered.contains(answer),
                "the agent's answer {answer:?} survives the fork: {:#?}",
                document.messages
            );
        }

        // A depth-relative cut of the same compacted source — the shape undo
        // and retry use — keeps the exchange it did not drop, likewise whole.
        let (undo_fork, _, _, _) = manager.fork_at_user_message(&thread.id, 0).await?;
        let undo_document = sessions.load_session(
            undo_fork
                .session_id
                .as_deref()
                .context("a published fork owns a session document")?,
        )?;
        let undo_roles: Vec<&str> = undo_document
            .messages
            .iter()
            .map(|message| message.role.as_str())
            .collect();
        assert_eq!(
            undo_roles,
            ["user", "assistant"],
            "a depth-relative cut keeps the first turn's exchange: {:#?}",
            undo_document.messages
        );
        Ok(())
    }

    /// A tool call whose outcome never reached the turn store — the tool
    /// failed, so the item carries the failure text and no result item follows
    /// — used to rebuild as a call with no answer. Two things then went wrong
    /// on a fork of that conversation: the response *after* the lost call was
    /// glued into the same assistant message (no result had flushed it), and
    /// the request-time repair answered the call by inserting a result next to
    /// a message that now held two responses. DeepSeek's Responses API reads
    /// that history as `400 No tool output found for tool call …` on every
    /// turn the fork ran, and the repaired document no longer matched the
    /// records, so retrying the fork refused with "cannot identify an exact
    /// saved-history boundary".
    #[tokio::test]
    async fn a_rebuilt_turn_answers_a_tool_call_whose_outcome_the_store_lost() -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let manager = RuntimeThreadManager::open(
            config(),
            dir.path().to_path_buf(),
            test_manager_config(dir.path().join("runtime")),
        )?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let sessions = crate::session_manager::SessionManager::new(
            crate::session_manager::default_sessions_dir()?,
        )?;
        let base = Utc::now();
        let at = |seconds: i64| Some(base + chrono::Duration::seconds(seconds));

        let text_item = |id: &str, turn: &str, order: i64, kind: TurnItemKind, text: &str| {
            let mut item = sample_item(turn, id, TurnItemLifecycleStatus::Completed);
            item.kind = kind;
            item.started_at = at(order);
            item.ended_at = item.started_at;
            item.summary = text.to_string();
            item.detail = Some(text.to_string());
            item
        };

        // Two responses in one turn: the first asked for a build that the tool
        // reported as failed without recording a result, the second answered
        // and ran the tests, which did.
        let turn_lost = "turn_lost_the_call";
        let mut lost = sample_item(turn_lost, "item_call_lost", TurnItemLifecycleStatus::Failed);
        lost.kind = TurnItemKind::ToolCall;
        lost.started_at = at(2);
        lost.ended_at = lost.started_at;
        lost.summary = "bash failed: Failed to execute tool: boom".to_string();
        lost.detail = Some("Failed to execute tool: boom".to_string());
        lost.metadata = Some(json!({
            "tool_use_id": "call_lost",
            "tool_name": "bash",
            "tool_input": "{\"command\":\"cargo build\"}",
        }));
        let mut kept = sample_item(
            turn_lost,
            "item_call_kept",
            TurnItemLifecycleStatus::Completed,
        );
        kept.kind = TurnItemKind::ToolCall;
        kept.started_at = at(5);
        kept.ended_at = kept.started_at;
        kept.summary = "bash: 12 passed".to_string();
        kept.detail = Some("12 passed".to_string());
        kept.metadata = Some(json!({
            "tool_use_id": "call_kept",
            "tool_result_for": "call_kept",
            "tool_name": "bash",
            "tool_input": "{\"command\":\"cargo test\"}",
        }));

        let turn_after = "turn_of_compaction";
        for record in [
            text_item(
                "item_u1",
                turn_lost,
                0,
                TurnItemKind::UserMessage,
                "run the build",
            ),
            text_item(
                "item_r1",
                turn_lost,
                1,
                TurnItemKind::AgentReasoning,
                "the compiler will tell us what is missing",
            ),
            lost,
            text_item(
                "item_s1",
                turn_lost,
                3,
                TurnItemKind::Status,
                "Continuing — tool results",
            ),
            text_item(
                "item_r2",
                turn_lost,
                4,
                TurnItemKind::AgentReasoning,
                "the build failed; the tests still say something",
            ),
            kept,
            text_item(
                "item_a1",
                turn_lost,
                6,
                TurnItemKind::AgentMessage,
                "the build failed, the tests passed",
            ),
            text_item(
                "item_u2",
                turn_after,
                7,
                TurnItemKind::UserMessage,
                "and now the next thing",
            ),
            text_item(
                "item_c1",
                turn_after,
                8,
                TurnItemKind::ContextCompaction,
                "Made room: 40 → 21 messages",
            ),
        ] {
            manager.store.save_item(&record)?;
        }
        for (turn_id, order) in [(turn_lost, 0), (turn_after, 10)] {
            let mut turn = sample_turn(&thread.id, turn_id, RuntimeTurnStatus::Completed);
            turn.started_at = at(order);
            turn.ended_at = turn.started_at;
            manager.store.save_turn(&turn)?;
        }
        let mut stored = manager.get_thread(&thread.id).await?;
        stored.latest_turn_id = Some(turn_after.to_string());
        manager.store.save_thread(&stored)?;

        // The engine's transcript for that conversation once compaction ran:
        // the prompts verbatim, nothing that happened between them. A cut has
        // to rebuild the exchange from the records — the path this covers.
        let compacted: Vec<Message> = serde_json::from_value(json!([
            {"role":"user","content":[{"type":"text","text":"run the build"}]},
            {"role":"user","content":[{"type":"text","text":"and now the next thing"}]}
        ]))?;
        let saved = crate::session_manager::create_saved_session_with_id_and_mode(
            Uuid::new_v4().to_string(),
            &compacted,
            &thread.model,
            dir.path(),
            0,
            None,
            Some("agent"),
        );
        {
            let _admission = manager.session_checkpoint_guard().await;
            sessions.save_session(&saved)?;
            manager
                .set_thread_session_checkpoint(&thread.id, &saved)
                .await?;
        }

        let (fork, _, _, _) = manager.fork_at_user_turn(&thread.id, turn_lost).await?;
        let fork_session_id = fork
            .session_id
            .clone()
            .context("a published fork owns a session document")?;
        let document = sessions.load_session(&fork_session_id)?;
        let shape = |messages: &[Message]| -> Vec<(String, Vec<&'static str>)> {
            messages
                .iter()
                .map(|message| {
                    let blocks = message
                        .content
                        .iter()
                        .map(|block| match block {
                            ContentBlock::Text { .. } => "text",
                            ContentBlock::Thinking { .. } => "thinking",
                            ContentBlock::ToolUse { .. } => "tool_use",
                            ContentBlock::ToolResult { .. } => "tool_result",
                            _ => "other",
                        })
                        .collect();
                    (message.role.as_str().to_string(), blocks)
                })
                .collect()
        };
        assert_eq!(
            shape(&document.messages),
            vec![
                ("user".to_string(), vec!["text"]),
                ("assistant".to_string(), vec!["thinking", "tool_use"]),
                ("user".to_string(), vec!["tool_result"]),
                ("assistant".to_string(), vec!["thinking", "tool_use"]),
                ("user".to_string(), vec!["tool_result"]),
                ("assistant".to_string(), vec!["text"]),
            ],
            "each response keeps its own call and its own answer: {:#?}",
            document.messages
        );
        let calls: Vec<&str> = document
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        let answers: Vec<(&str, &str)> = document
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => Some((tool_use_id.as_str(), content.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(calls, ["call_lost", "call_kept"]);
        assert_eq!(
            answers
                .iter()
                .map(|(tool_use_id, _)| *tool_use_id)
                .collect::<Vec<_>>(),
            ["call_lost", "call_kept"],
            "no call is left unanswered: {:#?}",
            document.messages
        );
        assert!(
            answers[0].1.contains("Failed to execute tool: boom"),
            "the lost call keeps the failure the turn recorded: {:#?}",
            answers
        );

        // Now the fork's own life: the engine loads it with the source's
        // compaction summary as its system prompt, so the synced history grows
        // the checkpoint message *before* the fork's first turn, and two turns
        // follow it. Retrying the fork — a depth-relative cut of its last turn
        // — walks a document whose middle holds a message no record produced.
        let mut extended = document.messages.clone();
        extended.push(crate::compaction::compaction_checkpoint_message(
            &SystemPrompt::Text(format!(
                "{} the work so far",
                crate::compaction::SUMMARY_HEADER
            )),
        ));
        let first_prompt = "and now?";
        let second_prompt = "keep going";
        let tail: Vec<Message> = serde_json::from_value(json!([
            {"role":"user","content":[{"type":"text","text":first_prompt}]},
            {"role":"user","content":[{"type":"text","text":second_prompt}]}
        ]))?;
        extended.extend(tail.clone());
        let mut rebound = sessions.load_session(&fork_session_id)?;
        rebound.messages = extended;
        {
            let _admission = manager.session_checkpoint_guard().await;
            sessions.save_session(&rebound)?;
            manager.seed_thread_from_messages(&fork.id, &tail).await?;
            manager
                .set_thread_session_checkpoint(&fork.id, &rebound)
                .await?;
        }

        let (undo, _, _, _) = manager.fork_at_user_message(&fork.id, 0).await?;
        let undo_document = sessions.load_session(
            undo.session_id
                .as_deref()
                .context("a published fork owns a session document")?,
        )?;
        assert_eq!(
            projected_user_texts(&undo_document.messages),
            ["run the build".to_string(), first_prompt.to_string()],
            "the cut keeps the exchange and the first post-fork turn, and reads \
             past the engine's checkpoint: {:#?}",
            undo_document.messages
        );
        assert_eq!(
            undo_document
                .messages
                .iter()
                .flat_map(|message| message.content.iter())
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            ["call_lost", "call_kept"],
            "the successor keeps the answered calls whole: {:#?}",
            undo_document.messages
        );
        Ok(())
    }

    async fn control_case(interrupt: bool, follow_up: bool) -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let workspace = dir.path().join("workspace");
        fs::create_dir(&workspace)?;
        let config = config();
        let manager = RuntimeThreadManager::open(
            config.clone(),
            workspace.clone(),
            test_manager_config(dir.path().join("runtime")),
        )?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
            "fixture response",
        )]));
        let (engine, handle) = Engine::new_with_model_client(
            EngineConfig {
                workspace,
                model: thread.model.clone(),
                features: codewhale_runtime::features::Features::default(),
                subagents_enabled: false,
                snapshots_enabled: false,
                memory_enabled: false,
                terminal_chrome_enabled: false,
                runtime_services: crate::tools::spec::RuntimeToolServices {
                    active_thread_id: Some(thread.id.clone()),
                    ..Default::default()
                },
                ..EngineConfig::default()
            },
            &config,
            mock.clone(),
        );
        manager
            .install_test_engine(&thread.id, handle.clone())
            .await?;
        let (release, begin) = oneshot::channel();
        let run = tokio::spawn(async move {
            begin.await.unwrap();
            engine.run().await;
        });
        let first = manager
            .start_turn(
                &thread.id,
                StartTurnRequest {
                    prompt: "first request".into(),
                    max_output_tokens: std::num::NonZeroU32::new(1500),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(mock.call_count(), 0);
        if interrupt {
            manager.interrupt_turn(&thread.id, &first.id).await?;
        } else {
            manager
                .steer_turn(
                    &thread.id,
                    &first.id,
                    SteerTurnRequest {
                        prompt: "NEW CORRECTION".into(),
                    },
                )
                .await?;
        }
        release.send(()).unwrap();
        let terminal = wait_for_terminal_turn(&manager, &first.id).await?;
        if interrupt {
            assert_eq!(terminal.status, RuntimeTurnStatus::Interrupted);
            assert_eq!(
                mock.call_count(),
                0,
                "pre-dispatch interrupt must prevent the first provider request"
            );
            assert!(
                manager
                    .store
                    .list_items_for_turn(&first.id)?
                    .iter()
                    .all(|item| item.kind != TurnItemKind::ToolCall)
            );
        } else {
            assert_eq!(terminal.status, RuntimeTurnStatus::Completed);
            let request = mock
                .last_request()
                .expect("real Engine reached mock provider");
            assert_eq!(
                request.max_tokens, 1500,
                "queued steer preserves the admitted allowance"
            );
            let text = serde_json::to_string(&request.messages)?;
            assert_eq!(text.matches("NEW CORRECTION").count(), 1);
        }
        assert_eq!(
            manager
                .events_since(&thread.id, None)?
                .iter()
                .filter(|event| event.event == "turn.completed"
                    && event.turn_id.as_deref() == Some(first.id.as_str()))
                .count(),
            1
        );
        if follow_up {
            handle.steer("stale correction after completion").await?;
            let second = manager
                .start_turn(
                    &thread.id,
                    StartTurnRequest {
                        prompt: "second request".into(),
                        ..Default::default()
                    },
                )
                .await?;
            assert_eq!(
                wait_for_terminal_turn(&manager, &second.id).await?.status,
                RuntimeTurnStatus::Completed
            );
            assert_eq!(mock.call_count(), 1);
            assert!(
                !serde_json::to_string(&mock.last_request().unwrap().messages)?
                    .contains("stale correction after completion")
            );
        }
        handle.send(Op::Shutdown).await?;
        tokio::time::timeout(Duration::from_secs(10), run).await??;
        Ok(())
    }

    #[tokio::test]
    async fn output_cap_survives_steps_replay_restart_and_retry_without_leaking() -> Result<()> {
        use std::num::NonZeroU32;
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let workspace = dir.path().join("workspace");
        fs::create_dir(&workspace)?;
        let config = config();
        let manager_config = test_manager_config(dir.path().join("runtime"));
        let manager =
            RuntimeThreadManager::open(config.clone(), workspace.clone(), manager_config.clone())?;
        let thread = manager
            .create_thread(CreateThreadRequest {
                allowed_tools: Some(Vec::new()),
                ..Default::default()
            })
            .await?;
        let response = |text: &str, stop: &str| {
            let mut events = canned::simple_text_turn(text);
            for event in &mut events {
                if let codewhale_models::StreamEvent::MessageDelta { delta, usage } = event {
                    delta.stop_reason = Some(stop.into());
                    *usage = Some(Usage {
                        input_tokens: 11,
                        output_tokens: 7,
                        reasoning_tokens: Some(2),
                        ..Default::default()
                    });
                }
            }
            events
        };
        let mock = Arc::new(MockLlmClient::new(vec![
            response("partial review", "max_tokens"),
            response("complete review", "end_turn"),
            response("next turn", "end_turn"),
        ]));
        let (engine, handle) = Engine::new_with_model_client(
            EngineConfig {
                workspace: workspace.clone(),
                model: thread.model.clone(),
                subagents_enabled: false,
                snapshots_enabled: false,
                memory_enabled: false,
                terminal_chrome_enabled: false,
                ..EngineConfig::default()
            },
            &config,
            mock.clone(),
        );
        manager
            .install_test_engine(&thread.id, handle.clone())
            .await?;
        let run = tokio::spawn(engine.run());
        let request = StartTurnRequest {
            prompt: "bounded review".into(),
            operation_key: Some("bounded-review".into()),
            max_output_tokens: NonZeroU32::new(1500),
            ..Default::default()
        };
        let first = manager.start_turn(&thread.id, request.clone()).await?;
        let first = wait_for_terminal_turn(&manager, &first.id).await?;
        assert_eq!(
            first.status,
            RuntimeTurnStatus::Completed,
            "{:?}",
            first.error
        );
        assert_eq!(first.max_output_tokens, NonZeroU32::new(1500));
        assert_eq!(first.schema_version, OUTPUT_LIMIT_RUNTIME_SCHEMA_VERSION);
        let calls = mock.captured_requests();
        assert_eq!(
            calls.len(),
            2,
            "truncation continues through the same turn loop"
        );
        assert!(calls.iter().all(|request| request.max_tokens == 1500));
        let events = manager.events_since(&thread.id, None)?;
        let receipts: Vec<_> = events
            .iter()
            .filter(|event| event.event == "turn.usage")
            .collect();
        assert_eq!(receipts.len(), 2);
        assert!(
            receipts
                .iter()
                .all(|event| event.payload["maxOutputTokens"] == 1500)
        );
        assert!(
            receipts
                .iter()
                .all(|event| event.payload["billed_prompt_tokens"] == 11),
            "each receipt names the prompt the provider billed: {receipts:?}"
        );
        assert!(
            receipts
                .iter()
                .all(|event| event.payload.get("cost").is_some()),
            "the live-cost slot is always emitted: {receipts:?}"
        );
        assert_eq!(
            manager.start_turn(&thread.id, request.clone()).await?.id,
            first.id
        );
        assert_eq!(mock.call_count(), 2, "exact replay cannot dispatch again");
        let mut changed = request.clone();
        changed.max_output_tokens = NonZeroU32::new(1501);
        assert!(manager.start_turn(&thread.id, changed).await.is_err());
        assert_eq!(mock.call_count(), 2);
        let second = manager
            .start_turn(
                &thread.id,
                StartTurnRequest {
                    prompt: "ordinary next turn".into(),
                    ..Default::default()
                },
            )
            .await?;
        let second = wait_for_terminal_turn(&manager, &second.id).await?;
        assert_eq!(second.status, RuntimeTurnStatus::Completed);
        assert_eq!(second.max_output_tokens, None);
        assert_eq!(second.schema_version, CURRENT_RUNTIME_SCHEMA_VERSION);
        assert!(
            mock.last_request().unwrap().max_tokens > 1500,
            "cap cannot leak to the next turn"
        );
        handle.send(Op::Shutdown).await?;
        tokio::time::timeout(Duration::from_secs(10), run).await??;
        drop(handle);
        drop(manager);
        let manager =
            RuntimeThreadManager::open(config.clone(), workspace.clone(), manager_config)?;
        assert_eq!(
            manager
                .start_turn(&thread.id, request)
                .await?
                .max_output_tokens,
            NonZeroU32::new(1500)
        );
        assert!(
            manager.active.lock().await.engines.is_empty(),
            "durable replay does not start an engine"
        );
        let (retry_thread, prompt, _, allowance) =
            manager.fork_at_user_message(&thread.id, 1).await?;
        assert_eq!(allowance, NonZeroU32::new(1500));
        let retry_mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
            "retried review",
        )]));
        let (engine, handle) = Engine::new_with_model_client(
            EngineConfig {
                workspace,
                model: retry_thread.model.clone(),
                subagents_enabled: false,
                snapshots_enabled: false,
                memory_enabled: false,
                terminal_chrome_enabled: false,
                ..EngineConfig::default()
            },
            &config,
            retry_mock.clone(),
        );
        manager
            .install_test_engine(&retry_thread.id, handle.clone())
            .await?;
        let run = tokio::spawn(engine.run());
        let retry = manager
            .start_turn(
                &retry_thread.id,
                StartTurnRequest {
                    prompt: prompt.unwrap(),
                    max_output_tokens: allowance,
                    ..Default::default()
                },
            )
            .await?;
        let retry = wait_for_terminal_turn(&manager, &retry.id).await?;
        assert_eq!(retry.status, RuntimeTurnStatus::Completed);
        assert_eq!(retry_mock.last_request().unwrap().max_tokens, 1500);
        handle.send(Op::Shutdown).await?;
        tokio::time::timeout(Duration::from_secs(10), run).await??;
        Ok(())
    }

    #[tokio::test]
    async fn output_cap_admission_cannot_raise_route_limit_and_rejects_unsupported_routes()
    -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let config = config();
        let manager = RuntimeThreadManager::open(
            config,
            dir.path().into(),
            test_manager_config(dir.path().join("runtime")),
        )?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let mut harness = install_mock_engine(&manager, &thread.id).await;
        let turn = manager
            .start_turn(
                &thread.id,
                StartTurnRequest {
                    prompt: "downward bound".into(),
                    max_output_tokens: std::num::NonZeroU32::new(u32::MAX),
                    ..Default::default()
                },
            )
            .await?;
        let Some(Op::SendMessage(TurnSpec {
            route,
            max_output_tokens,
            ..
        })) = harness.rx_op.recv().await
        else {
            panic!("SendMessage expected")
        };
        let ceiling = crate::route_budget::effective_max_output_tokens_for_route(
            route.identity.provider,
            &route.model,
            known_route_limits(route.candidate.limits()),
        );
        assert_eq!(max_output_tokens.unwrap().get(), ceiling);
        assert_eq!(turn.max_output_tokens, max_output_tokens);
        assert!(ceiling < u32::MAX);
        let mut corrupt = turn.clone();
        corrupt.max_output_tokens = None;
        assert!(
            manager.store.save_turn(&corrupt).is_err(),
            "schema4 cannot lose its admitted allowance"
        );
        corrupt = turn;
        corrupt.schema_version = CURRENT_RUNTIME_SCHEMA_VERSION;
        assert!(
            manager.store.save_turn(&corrupt).is_err(),
            "older readers must not ignore a capped record"
        );
        let auto = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let error = manager
            .start_turn(
                &auto.id,
                StartTurnRequest {
                    prompt: "reject Auto before classifier".into(),
                    model: Some("auto".into()),
                    max_output_tokens: std::num::NonZeroU32::new(1500),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("exact model"), "{error}");
        assert!(manager.store.list_turns_for_thread(&auto.id)?.is_empty());
        assert!(!manager.active.lock().await.engines.contains_key(&auto.id));
        let codex = RuntimeThreadManager::open(
            {
                let __cw_config_init_1_0 = Some("openai-codex".into());
                let __cw_config_init_1_1 = Some("gpt-5.5".into());
                let mut __cw_config_init_1_value: Config = Config::default();
                __cw_config_init_1_value.provider = __cw_config_init_1_0;
                __cw_config_init_1_value.default_text_model = __cw_config_init_1_1;
                __cw_config_init_1_value
            },
            dir.path().into(),
            test_manager_config(dir.path().join("codex")),
        )?;
        let thread = codex.create_thread(CreateThreadRequest::default()).await?;
        let error = codex
            .start_turn(
                &thread.id,
                StartTurnRequest {
                    prompt: "reject unenforceable transport".into(),
                    max_output_tokens: std::num::NonZeroU32::new(1500),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("unsupported"), "{error}");
        assert!(codex.store.list_turns_for_thread(&thread.id)?.is_empty());
        assert!(codex.active.lock().await.engines.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn queued_turn_interrupt_survives_engine_startup() -> Result<()> {
        control_case(true, false).await
    }

    #[tokio::test]
    async fn queued_turn_steer_reaches_first_request() -> Result<()> {
        control_case(false, false).await
    }

    #[tokio::test]
    async fn new_turn_does_not_inherit_previous_turn_controls() -> Result<()> {
        control_case(true, true).await
    }
}

fn test_runtime_dir() -> PathBuf {
    std::env::temp_dir().join(format!("deepseek-runtime-threads-{}", Uuid::new_v4()))
}

const EVENT_PROCESS_ROLE_ENV: &str = "CODEWHALE_TEST_EVENT_PROCESS_ROLE";
const EVENT_PROCESS_ROOT_ENV: &str = "CODEWHALE_TEST_EVENT_PROCESS_ROOT";
const EVENT_PROCESS_THREAD_ENV: &str = "CODEWHALE_TEST_EVENT_PROCESS_THREAD";
const EVENT_PROCESS_SIGNAL_ENV: &str = "CODEWHALE_TEST_EVENT_PROCESS_SIGNAL";
const EVENT_PROCESS_START_ENV: &str = "CODEWHALE_TEST_EVENT_PROCESS_START";
const EVENT_PROCESS_RELEASE_ENV: &str = "CODEWHALE_TEST_EVENT_PROCESS_RELEASE";
const EVENT_PROCESS_WORKER_ENV: &str = "CODEWHALE_TEST_EVENT_PROCESS_WORKER";
const EVENT_PROCESS_COUNT_ENV: &str = "CODEWHALE_TEST_EVENT_PROCESS_COUNT";
const EVENT_PROCESS_HELPER: &str = "runtime_threads::tests::runtime_event_process_child_helper";
// Deadlock ceiling for monitor settlement in the 10k-test lib binary. This is
// a test watchdog, not an expected runtime latency or a customer-facing SLO.
const TURN_SETTLEMENT_DEADLOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// Durable record ids carry a full UUID: records live in flat per-kind
/// directories and a save replaces by id, so a short suffix would let one
/// thread's record overwrite another's.
#[test]
fn runtime_record_ids_carry_a_full_uuid() {
    let id = runtime_record_id("thr");
    let suffix = id.strip_prefix("thr_").expect("prefix");
    assert_eq!(suffix.len(), 32, "{id}");
    assert!(suffix.bytes().all(|byte| byte.is_ascii_hexdigit()), "{id}");
    assert_eq!(validated_record_id(&id, "thread id").expect("valid"), id);
    let ids: std::collections::HashSet<String> =
        (0..10_000).map(|_| runtime_record_id("item")).collect();
    assert_eq!(ids.len(), 10_000);
}

#[test]
#[ignore = "spawned by real cross-process Runtime event tests"]
fn runtime_event_process_child_helper() {
    let Ok(role) = std::env::var(EVENT_PROCESS_ROLE_ENV) else {
        return;
    };
    let root = PathBuf::from(
        std::env::var_os(EVENT_PROCESS_ROOT_ENV).expect("event child needs a store root"),
    );
    let thread_id = std::env::var(EVENT_PROCESS_THREAD_ENV).expect("event child needs a thread id");
    let signal = PathBuf::from(
        std::env::var_os(EVENT_PROCESS_SIGNAL_ENV).expect("event child needs a signal path"),
    );
    if role == "manager-racer" {
        let start = PathBuf::from(
            std::env::var_os(EVENT_PROCESS_START_ENV).expect("manager racer needs a start barrier"),
        );
        let release = PathBuf::from(
            std::env::var_os(EVENT_PROCESS_RELEASE_ENV)
                .expect("manager racer needs a release barrier"),
        );
        std::fs::write(&signal, b"ready").expect("announce manager racer readiness");
        wait_for_runtime_event_test_file(&start, "manager racer start barrier");
        match test_manager(root) {
            Ok(_manager) => {
                std::fs::write(&signal, b"won").expect("announce manager race winner");
                wait_for_runtime_event_test_file(&release, "manager racer release barrier");
            }
            Err(error) if format!("{error:#}").contains("already active in another process") => {
                std::fs::write(&signal, b"lost").expect("announce manager race loser");
            }
            Err(error) => panic!("unexpected manager race failure: {error:#}"),
        }
        return;
    }
    let store = RuntimeThreadStore::open(root.clone()).expect("event child opens Runtime store");

    match role.as_str() {
        "manager-holder" => {
            let start = PathBuf::from(
                std::env::var_os(EVENT_PROCESS_START_ENV)
                    .expect("manager holder needs a release barrier"),
            );
            drop(store);
            let _manager = test_manager(root).expect("manager child acquires process owner lock");
            std::fs::write(&signal, b"locked").expect("announce held manager owner lock");
            wait_for_runtime_event_test_file(&start, "manager owner release barrier");
        }
        "writer" => {
            let start = PathBuf::from(
                std::env::var_os(EVENT_PROCESS_START_ENV)
                    .expect("event writer needs a start barrier"),
            );
            let worker =
                std::env::var(EVENT_PROCESS_WORKER_ENV).expect("event writer needs a worker id");
            let count = std::env::var(EVENT_PROCESS_COUNT_ENV)
                .expect("event writer needs a count")
                .parse::<u64>()
                .expect("event writer count must be numeric");
            std::fs::write(&signal, b"ready").expect("announce ready event writer");
            wait_for_runtime_event_test_file(&start, "writer start barrier");
            // The same transaction `append_event` runs, with a lock budget
            // sized for contention rather than the interactive 5 s default.
            // The claim here is that sequences never collide across
            // processes; four writers each fsyncing eight appends on a loaded
            // CI runner have starved one polling writer past 5 s (macOS and
            // Windows), which failed the run without any duplicate sequence.
            // 25 s stays inside the parent's 30 s wait, so a genuinely stuck
            // lock still fails here with its own message.
            for index in 0..count {
                store
                    .append_event_transaction(
                        thread_id.clone(),
                        None,
                        None,
                        "process.event".to_string(),
                        json!({ "worker": worker, "index": index }),
                        Duration::from_secs(25),
                    )
                    .expect("append cross-process Runtime event");
            }
        }
        "holder" => {
            store
                .with_event_transaction(Duration::from_secs(5), || {
                    std::fs::write(&signal, b"locked").expect("announce held event lock");
                    std::thread::sleep(Duration::from_secs(60));
                    Ok(())
                })
                .expect("hold Runtime event transaction");
        }
        "torn" => {
            store
                .with_event_transaction(Duration::from_secs(5), || {
                    let mut state = load_runtime_store_state(&store.state_path)?;
                    let seq = state.next_seq;
                    state.next_seq = seq.checked_add(1).context("reserve torn event sequence")?;
                    write_json_atomic(&store.state_path, &state)?;
                    let path = store.events_path(&thread_id)?;
                    let mut file =
                        open_runtime_store_file(&path, "torn Runtime event fixture", |options| {
                            options.create(true).append(true);
                        })?;
                    let record = RuntimeEventRecord {
                        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                        seq,
                        timestamp: Utc::now(),
                        thread_id: thread_id.clone(),
                        turn_id: None,
                        item_id: None,
                        event: "torn.process.event".to_string(),
                        payload: json!({}),
                    };
                    file.write_all(&serde_json::to_vec(&record)?)?;
                    file.flush()?;
                    file.sync_all()?;
                    std::fs::write(&signal, b"torn").expect("announce torn event append");
                    std::thread::sleep(Duration::from_secs(60));
                    Ok(())
                })
                .expect("hold torn Runtime event transaction");
        }
        "phantom" => {
            let release = PathBuf::from(
                std::env::var_os(EVENT_PROCESS_START_ENV)
                    .expect("phantom writer needs a rollback barrier"),
            );
            store
                .with_event_transaction(Duration::from_secs(5), || {
                    let path = store.events_path(&thread_id)?;
                    let mut append = open_runtime_store_file(
                        &path,
                        "phantom Runtime event fixture",
                        |options| {
                            options.create(true).append(true);
                        },
                    )?;
                    let rollback = open_runtime_store_file(
                        &path,
                        "phantom Runtime event rollback",
                        |options| {
                            options.write(true);
                        },
                    )?;
                    validate_same_runtime_store_file_handles(&append, &rollback, &path)?;
                    let original_len = append.metadata()?.len();
                    serde_json::to_writer(
                        &mut append,
                        &RuntimeEventRecord {
                            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                            seq: 999,
                            timestamp: Utc::now(),
                            thread_id: thread_id.clone(),
                            turn_id: None,
                            item_id: None,
                            event: "phantom.process.event".to_string(),
                            payload: json!({}),
                        },
                    )?;
                    append.write_all(b"\n")?;
                    append.flush()?;
                    std::fs::write(&signal, b"visible").expect("announce rollback candidate");
                    wait_for_runtime_event_test_file(&release, "phantom rollback barrier");
                    rollback_failed_event_append_handle(&rollback, original_len)
                })
                .expect("roll back phantom Runtime event");
        }
        other => panic!("unknown Runtime event child role {other}"),
    }
}

fn wait_for_runtime_event_test_file(path: &Path, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {label} at {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn spawn_runtime_event_child(
    role: &str,
    root: &Path,
    thread_id: &str,
    signal: &Path,
) -> RuntimeEventChildGuard {
    let mut command = std::process::Command::new(
        std::env::current_exe().expect("current test executable for Runtime event child"),
    );
    command
        .arg(EVENT_PROCESS_HELPER)
        .args(["--exact", "--ignored", "--nocapture", "--test-threads", "1"])
        .env(EVENT_PROCESS_ROLE_ENV, role)
        .env(EVENT_PROCESS_ROOT_ENV, root)
        .env(EVENT_PROCESS_THREAD_ENV, thread_id)
        .env(EVENT_PROCESS_SIGNAL_ENV, signal)
        .env(EVENT_PROCESS_START_ENV, root.join("process-writers.start"))
        .env(EVENT_PROCESS_WORKER_ENV, signal.as_os_str())
        .env(EVENT_PROCESS_COUNT_ENV, "8")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        // Disable the child's libtest capture and inherit stderr into the
        // parent's capture so a failed writer reports its cause, not just 101.
        .stderr(std::process::Stdio::inherit());
    RuntimeEventChildGuard::new(command.spawn().expect("spawn Runtime event child"))
}

fn spawn_runtime_manager_racer(
    root: &Path,
    signal: &Path,
    start: &Path,
    release: &Path,
) -> RuntimeEventChildGuard {
    let mut command = std::process::Command::new(
        std::env::current_exe().expect("current test executable for Runtime manager racer"),
    );
    command
        .arg(EVENT_PROCESS_HELPER)
        .args(["--exact", "--ignored", "--nocapture", "--test-threads", "1"])
        .env(EVENT_PROCESS_ROLE_ENV, "manager-racer")
        .env(EVENT_PROCESS_ROOT_ENV, root)
        .env(EVENT_PROCESS_THREAD_ENV, "thr_unused")
        .env(EVENT_PROCESS_SIGNAL_ENV, signal)
        .env(EVENT_PROCESS_START_ENV, start)
        .env(EVENT_PROCESS_RELEASE_ENV, release)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit());
    RuntimeEventChildGuard::new(command.spawn().expect("spawn Runtime manager racer"))
}

fn wait_for_runtime_event_test_value(path: &Path, expected: &[&str], label: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(value) = std::fs::read_to_string(path) {
            let value = value.trim().to_string();
            if expected.contains(&value.as_str()) {
                return value;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {label} at {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct RuntimeEventChildGuard(Option<std::process::Child>);

impl RuntimeEventChildGuard {
    const fn new(child: std::process::Child) -> Self {
        Self(Some(child))
    }

    fn wait_success(mut self, label: &str) {
        use wait_timeout::ChildExt as _;

        let outcome = self
            .0
            .as_mut()
            .expect("Runtime event child is present")
            .wait_timeout(Duration::from_secs(30));
        match outcome {
            Ok(Some(status)) => {
                self.0.take();
                assert!(status.success(), "{label} failed with {status}");
            }
            Ok(None) => panic!("timed out waiting for {label}"),
            Err(error) => panic!("failed waiting for {label}: {error}"),
        }
    }

    fn kill_and_reap(mut self) -> std::io::Result<std::process::ExitStatus> {
        let child = self.0.as_mut().expect("Runtime event child is present");
        let kill = child.kill();
        let status = child.wait();
        if status.is_ok() {
            self.0.take();
        }
        kill.and(status)
    }
}

impl Drop for RuntimeEventChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn test_manager_config(data_dir: PathBuf) -> RuntimeThreadManagerConfig {
    RuntimeThreadManagerConfig {
        mcp_auth_context: Default::default(),
        task_data_dir: data_dir.clone(),
        sessions_dir: None,
        data_dir,
        max_active_threads: 4,
    }
}

fn test_manager(data_dir: PathBuf) -> Result<RuntimeThreadManager> {
    RuntimeThreadManager::open(
        Config::default(),
        PathBuf::from("."),
        test_manager_config(data_dir),
    )
}

struct EventAppendFaultGuard {
    restore: Option<EventAppendTestFaultRestore>,
}

impl EventAppendFaultGuard {
    fn arm(thread_id: &str, fault: EventAppendTestFault) -> Self {
        Self::arm_repeated(thread_id, fault, 1)
    }

    fn arm_repeated(thread_id: &str, fault: EventAppendTestFault, count: usize) -> Self {
        Self {
            restore: Some(set_test_event_append_fault(thread_id, fault, count)),
        }
    }
}

impl Drop for EventAppendFaultGuard {
    fn drop(&mut self) {
        if let Some(restore) = self.restore.take() {
            restore_test_event_append_fault(restore);
        }
    }
}

fn sample_thread(thread_id: &str) -> ThreadRecord {
    let now = Utc::now();
    ThreadRecord {
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: thread_id.to_string(),
        created_at: now,
        updated_at: now,
        model: DEFAULT_TEXT_MODEL.to_string(),
        model_provider: None,
        model_provider_id: None,
        reasoning_effort: None,
        allowed_tools: None,
        workspace: PathBuf::from("."),
        mode: AppMode::Agent.as_setting().to_string(),
        permission_posture: Some("ask".to_string()),
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
        session_id: None,
        saved_session_checkpoint: None,
        last_read: None,
    }
}

fn sample_turn(thread_id: &str, turn_id: &str, status: RuntimeTurnStatus) -> TurnRecord {
    let now = Utc::now();
    TurnRecord {
        prefix_checkpoint: None,
        decision_receipts: Vec::new(),
        max_output_tokens: None,
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: turn_id.to_string(),
        thread_id: thread_id.to_string(),
        status,
        input_summary: "sample".to_string(),
        created_at: now,
        started_at: Some(now),
        ended_at: None,
        duration_ms: None,
        usage: None,
        routing_settlement: false,
        effective_route_usage: None,
        permission_posture: None,
        mode: None,
        effective_provider: None,
        effective_provider_id: None,
        effective_openrouter_vendor: None,
        effective_billing_surface: None,
        effective_endpoint_fingerprint: None,
        effective_provider_live_pricing: None,
        effective_billing_mode: None,
        effective_dispatched_at: None,
        effective_model: None,
        routed_usage: Vec::new(),
        routed_usage_drop_records: Vec::new(),
        routed_usage_source_ids: Vec::new(),
        routed_usage_dropped_records: 0,
        model_request_diagnostics: None,
        operation_activity: None,
        error: None,
        item_ids: Vec::new(),
        steer_count: 0,
        agent_mail_message_id: None,
        artifacts: Vec::new(),
        workspace: None,
        workspace_snapshots: Vec::new(),
    }
}

fn agent_mail_request(
    message_id: &str,
    source_thread_id: &str,
    destination_thread_id: &str,
    sender_identity: &str,
    summary: &str,
    delivery_mode: AgentMailDeliveryMode,
) -> AgentMailSendRequest {
    AgentMailSendRequest {
        message_id: AgentMailMessageId::parse(message_id).expect("valid mail id"),
        source_thread_id: source_thread_id.to_string(),
        destination_thread_id: destination_thread_id.to_string(),
        sender: AgentMailSender {
            identity: sender_identity.to_string(),
            display_label: "Builder A".to_string(),
        },
        summary: summary.to_string(),
        evidence: Vec::new(),
        delivery_mode,
        trigger_turn: delivery_mode == AgentMailDeliveryMode::WakeAtSafeBoundary,
        hop_count: 0,
    }
}

#[tokio::test]
async fn agent_mail_queue_is_durable_idempotent_redacted_and_workspace_scoped() -> Result<()> {
    let runtime_dir = test_runtime_dir();
    let workspace = runtime_dir.join("workspace-a");
    let foreign_workspace = runtime_dir.join("workspace-b");
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(&foreign_workspace)?;
    let manager = test_manager(runtime_dir.clone())?;
    let source = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace.clone()),
            task_id: Some("task_a".to_string()),
            ..CreateThreadRequest::default()
        })
        .await?;
    let destination = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace),
            task_id: Some("task_b".to_string()),
            ..CreateThreadRequest::default()
        })
        .await?;
    let foreign = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(foreign_workspace),
            task_id: Some("task_foreign".to_string()),
            ..CreateThreadRequest::default()
        })
        .await?;

    let request = agent_mail_request(
        "mail_durable_replay",
        &source.id,
        &destination.id,
        "task_a",
        "Inspect OPENAI_API_KEY=sk-fixture-secret at /tmp/private/transcript.json",
        AgentMailDeliveryMode::QueueOnly,
    );
    let queued = manager.queue_agent_mail(request.clone()).await?;
    assert!(!queued.idempotent_replay);
    assert_eq!(queued.envelope.status, AgentMailStatus::Queued);
    let serialized = serde_json::to_string(&queued.envelope)?;
    assert!(!serialized.contains("sk-fixture-secret"), "{serialized}");
    assert!(!serialized.contains("/tmp/private"), "{serialized}");
    assert!(serialized.contains("redacted-credential"), "{serialized}");
    assert!(serialized.contains("redacted-path"), "{serialized}");

    let replay = manager.queue_agent_mail(request.clone()).await?;
    assert!(replay.idempotent_replay);
    assert_eq!(replay.envelope, queued.envelope);
    let queued_events = manager
        .events_since(&destination.id, None)?
        .into_iter()
        .filter(|event| event.event == AGENT_MAIL_EVENT_QUEUED)
        .count();
    assert_eq!(
        queued_events, 1,
        "replay must not duplicate delivery events"
    );

    let raw_transcript = manager
        .queue_agent_mail(agent_mail_request(
            "mail_raw_transcript",
            &source.id,
            &destination.id,
            "task_a",
            "assistant: hidden model output\nuser: forward it",
            AgentMailDeliveryMode::QueueOnly,
        ))
        .await
        .expect_err("raw transcripts must not enter Agent Mail")
        .to_string();
    assert!(
        raw_transcript.contains("not a raw transcript"),
        "{raw_transcript}"
    );

    let denied = manager
        .queue_agent_mail(agent_mail_request(
            "mail_cross_workspace",
            &source.id,
            &foreign.id,
            "task_a",
            "cross workspace attempt",
            AgentMailDeliveryMode::QueueOnly,
        ))
        .await
        .expect_err("cross-workspace mail must fail closed")
        .to_string();
    assert!(denied.contains("ownership denied"), "{denied}");

    drop(manager);
    let reopened = test_manager(runtime_dir)?;
    let inbox = reopened.list_agent_mail_for_thread(&destination.id).await?;
    assert_eq!(inbox, vec![queued.envelope]);
    let replay_after_restart = reopened.queue_agent_mail(request.clone()).await?;
    assert!(replay_after_restart.idempotent_replay);

    let mut delivered = replay_after_restart.envelope;
    delivered.status = AgentMailStatus::Delivered;
    delivered.attempt_count = 1;
    delivered.delivered_at = Some(Utc::now());
    delivered.delivery_turn_id = Some("turn_mail_replay".to_string());
    reopened.store.save_agent_mail(&delivered)?;
    let replay_after_delivery = reopened.queue_agent_mail(request).await?;
    assert!(replay_after_delivery.idempotent_replay);
    assert_eq!(
        replay_after_delivery.envelope.status,
        AgentMailStatus::Delivered
    );
    let lifecycle_events = reopened.events_since(&destination.id, None)?;
    assert_eq!(
        lifecycle_events
            .iter()
            .filter(|event| event.event == AGENT_MAIL_EVENT_QUEUED)
            .count(),
        1,
        "lifecycle replay must not emit queued with a delivered envelope"
    );
    assert_eq!(
        lifecycle_events
            .iter()
            .filter(|event| event.event == AGENT_MAIL_EVENT_DELIVERED)
            .count(),
        1,
        "lifecycle replay should repair the current canonical event"
    );
    Ok(())
}

fn set_test_turn_route(
    turn: &mut TurnRecord,
    provider: ProviderKind,
    provider_identity: &str,
    model: &str,
    billing_surface: Option<&str>,
    billing_mode: crate::cost_status::RouteBillingMode,
) {
    turn.persist_effective_route(&crate::cost_status::EffectiveRouteEnvelope {
        openrouter_vendor: None,
        provider,
        provider_identity: provider_identity.to_string(),
        model: model.to_string(),
        billing_surface: billing_surface.map(str::to_string),
        endpoint_fingerprint: None,
        provider_live_pricing: None,
        billing_mode,
        dispatched_at: turn.created_at,
    });
}

#[test]
fn runtime_compaction_uses_provider_route_context() {
    let limits = codewhale_config::route::RouteLimits {
        context_tokens: Some(272_000),
        input_tokens: None,
        output_tokens: None,
        output_declared: false,
    };
    let config = runtime_compaction_config(
        &Config::default(),
        ProviderKind::OpenaiCodex,
        "gpt-5.5",
        (Some(limits), None),
        false,
        false,
        80.0,
    );

    assert!(config.enabled);
    // The threshold is 80% of the route window (the meter's denominator),
    // clamped to the spendable input ceiling; here 217_600 fits under it.
    assert_eq!(config.token_threshold, 217_600);
    assert_eq!(config.effective_context_window, Some(272_000));
}

#[test]
fn legacy_turn_record_has_no_invented_route_provenance() {
    let turn = sample_turn("thr_legacy", "turn_legacy", RuntimeTurnStatus::Completed);
    let mut value = serde_json::to_value(turn).expect("serialize turn");
    let object = value.as_object_mut().expect("turn object");
    object.remove("effective_provider");
    object.remove("effective_provider_id");
    object.remove("effective_billing_surface");
    object.remove("effective_endpoint_fingerprint");
    object.remove("effective_billing_mode");
    object.remove("effective_dispatched_at");
    object.remove("effective_model");
    object.remove("routed_usage");

    let restored: TurnRecord = serde_json::from_value(value).expect("deserialize legacy turn");
    assert_eq!(restored.effective_provider, None);
    assert_eq!(restored.effective_billing_surface, None);
    assert_eq!(restored.effective_billing_mode, None);
    assert_eq!(restored.effective_dispatched_at, None);
    assert_eq!(restored.effective_model, None);
    assert!(restored.routed_usage.is_empty());
}

#[tokio::test]
async fn explicit_local_thread_model_does_not_require_an_unrelated_default_catalog() -> Result<()> {
    let root = tempfile::tempdir()?;
    let config = {
        let __cw_config_init_2_0 = Some("ollama".into());
        let mut __cw_config_init_2_value: Config = Config::default();
        __cw_config_init_2_value.provider = __cw_config_init_2_0;
        __cw_config_init_2_value
    }
    .with_legacy_root(None, Some("http://127.0.0.1:11497/v1".into()));
    let manager = RuntimeThreadManager::open(
        config,
        root.path().to_path_buf(),
        test_manager_config(root.path().join("runtime")),
    )?;
    let error = manager
        .create_thread(CreateThreadRequest::default())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("no fresh model catalog"),
        "{error}"
    );
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: Some("chosen-local:tag".into()),
            model_provider: Some("ollama".into()),
            model_provider_id: Some("ollama".into()),
            ..CreateThreadRequest::default()
        })
        .await?;
    let saved = manager.get_thread(&thread.id).await?;
    assert_eq!(saved.model, "chosen-local:tag");
    assert_eq!(saved.model_provider.as_deref(), Some("ollama"));
    assert_eq!(saved.model_provider_id.as_deref(), Some("ollama"));
    Ok(())
}

#[tokio::test]
async fn named_custom_thread_identity_round_trips_and_fails_closed_when_removed() -> Result<()> {
    let mut custom = std::collections::HashMap::new();
    custom.insert(
        "lm-studio".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("http://127.0.0.1:1234/v1".to_string()),
            model: Some("local-default".to_string()),
            ..codewhale_runtime::config::ProviderConfig::default()
        },
    );
    let config = {
        let __cw_config_init_3_0 = Some("lm-studio".to_string());
        let __cw_config_init_3_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let mut __cw_config_init_3_value: Config = Config::default();
        __cw_config_init_3_value.provider = __cw_config_init_3_0;
        __cw_config_init_3_value.providers = __cw_config_init_3_1;
        __cw_config_init_3_value
    };
    let manager = RuntimeThreadManager::open(
        config.clone(),
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;

    let thread = manager
        .create_thread(CreateThreadRequest {
            model: Some("local-code-model".to_string()),
            model_provider: Some("lm-studio".to_string()),
            ..CreateThreadRequest::default()
        })
        .await?;
    let persisted = manager.get_thread(&thread.id).await?;
    assert_eq!(persisted.model_provider.as_deref(), Some("custom"));
    assert_eq!(persisted.model_provider_id.as_deref(), Some("lm-studio"));
    let serialized = serde_json::to_string(&persisted)?;
    assert!(serialized.contains("\"model_provider\":\"custom\""));
    assert!(serialized.contains("\"model_provider_id\":\"lm-studio\""));
    assert!(!serialized.contains("127.0.0.1:1234"));

    let route = manager.resolved_route_for_thread(&config, &persisted)?;
    assert_eq!(route.identity.provider, ProviderKind::Custom);
    assert_eq!(route.identity.key.as_str(), "lm-studio");
    assert_eq!(route.model, "local-code-model");
    assert_eq!(
        route.config.active_route_base_url(),
        "http://127.0.0.1:1234/v1"
    );

    let err = manager
        .resolved_route_for_thread(&Config::default(), &persisted)
        .expect_err("removed provider must fail closed");
    let message = err.to_string();
    assert!(message.contains("[providers.lm-studio]"), "{message}");
    assert!(message.contains("will not fall back"), "{message}");

    let mut legacy_value = serde_json::to_value(&persisted)?;
    legacy_value
        .as_object_mut()
        .expect("thread object")
        .remove("model_provider");
    legacy_value
        .as_object_mut()
        .expect("thread object")
        .remove("model_provider_id");
    let legacy: ThreadRecord = serde_json::from_value(legacy_value)?;
    assert_eq!(legacy.model_provider, None);
    Ok(())
}

#[test]
fn legacy_literal_custom_thread_resumes_on_the_migrated_table() -> Result<()> {
    let config = {
        let __cw_config_init_4_0 = Some("custom".to_string());
        let __cw_config_init_4_1 = Some("legacy-default-model".to_string());
        let mut __cw_config_init_4_value: Config = Config::default();
        __cw_config_init_4_value.provider = __cw_config_init_4_0;
        __cw_config_init_4_value.default_text_model = __cw_config_init_4_1;
        __cw_config_init_4_value
    }
    .with_legacy_root(None, Some("http://127.0.0.1:18180/v1".to_string()));
    let manager = RuntimeThreadManager::open(
        config.clone(),
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    let mut persisted = sample_thread("thr_legacy_custom");
    persisted.model = "legacy-saved-model".to_string();
    persisted.model_provider = Some("custom".to_string());
    let restored: ThreadRecord = serde_json::from_str(&serde_json::to_string(&persisted)?)?;

    let route = manager.resolved_route_for_thread(&config, &restored)?;
    assert_eq!(route.identity.provider, ProviderKind::Custom);
    assert_eq!(route.identity.key.as_str(), "custom");
    assert_eq!(route.model, "legacy-saved-model");
    assert_eq!(
        route.config.active_route_base_url(),
        "http://127.0.0.1:18180/v1"
    );
    // The top-level route is the `[providers.custom]` table since #6394.
    assert_eq!(
        route
            .config
            .resolve_provider_identity("custom")
            .map_err(anyhow::Error::msg)?,
        codewhale_runtime::config::ProviderIdentity::test_exact_identity(
            ProviderKind::Custom,
            "custom",
        )
    );
    let repeated = manager.resolved_route_for_thread(&route.config, &restored)?;
    assert_eq!(repeated.identity.key.as_str(), "custom");
    assert_eq!(repeated.model, "legacy-saved-model");
    assert_eq!(
        repeated.config.active_route_base_url(),
        "http://127.0.0.1:18180/v1"
    );

    let named_config = {
        let mut custom = std::collections::HashMap::new();
        custom.insert(
            "lm-studio".to_string(),
            codewhale_runtime::config::ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some("http://127.0.0.1:18181/v1".to_string()),
                model: Some("named-model".to_string()),
                ..codewhale_runtime::config::ProviderConfig::default()
            },
        );
        {
            let __cw_config_init_6_0 = Some("lm-studio".to_string());
            let __cw_config_init_6_1 = Some(codewhale_runtime::config::ProvidersConfig {
                custom,
                ..codewhale_runtime::config::ProvidersConfig::default()
            });
            let mut __cw_config_init_6_value: Config = Config::default();
            __cw_config_init_6_value.provider = __cw_config_init_6_0;
            __cw_config_init_6_value.providers = __cw_config_init_6_1;
            __cw_config_init_6_value
        }
    };
    let error = manager
        .resolved_route_for_thread(&named_config, &restored)
        .expect_err("id-less root record must not migrate to a named table")
        .to_string();
    assert!(error.contains("root-scope migrated `base_url`"), "{error}");
    assert!(error.contains("will not guess or fall back"), "{error}");

    Ok(())
}

#[tokio::test]
async fn captured_legacy_root_thread_and_turn_keep_absent_id_provenance() -> Result<()> {
    let config = {
        let __cw_config_init_7_0 = Some("custom".to_string());
        let __cw_config_init_7_1 = Some("legacy-root-model".to_string());
        let mut __cw_config_init_7_value: Config = Config::default();
        __cw_config_init_7_value.provider = __cw_config_init_7_0;
        __cw_config_init_7_value.default_text_model = __cw_config_init_7_1;
        __cw_config_init_7_value
    }
    .with_legacy_root(None, Some("http://127.0.0.1:18180/v1".to_string()));
    let manager = RuntimeThreadManager::open(
        config,
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: Some("legacy-root-model".to_string()),
            ..CreateThreadRequest::default()
        })
        .await?;
    // The leaf is canonicalized, while the captured released record keeps no additive ID.
    assert_eq!(thread.model_provider.as_deref(), Some("custom"));
    assert_eq!(thread.model_provider_id.as_deref(), None);

    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "keep the root route".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert_eq!(turn.effective_provider.as_deref(), Some("custom"));
    assert_eq!(turn.effective_provider_id.as_deref(), None);
    match harness.rx_op.recv().await {
        Some(Op::SendMessage(TurnSpec { route, .. })) => {
            assert_eq!(route.identity.key.as_str(), "custom");
            assert_eq!(route.identity.persisted_id(), None);
            assert_eq!(
                route.config.active_route_base_url(),
                "http://127.0.0.1:18180/v1"
            );
        }
        other => panic!("expected root custom send, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn runtime_turn_reasoning_and_tool_defaults_follow_explicit_precedence() -> Result<()> {
    let manager = RuntimeThreadManager::open(
        {
            let __cw_config_init_8_0 = Some("high".to_string());
            let mut __cw_config_init_8_value: Config = Config::default();
            __cw_config_init_8_value.reasoning_effort = __cw_config_init_8_0;
            __cw_config_init_8_value
        },
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;

    let override_thread = manager
        .create_thread(CreateThreadRequest {
            reasoning_effort: Some("mid".to_string()),
            allowed_tools: Some(vec!["read_file".to_string()]),
            ..CreateThreadRequest::default()
        })
        .await?;
    assert_eq!(
        override_thread.reasoning_effort.as_deref(),
        Some("medium"),
        "new API input is persisted in its canonical spelling"
    );
    assert_eq!(
        override_thread.allowed_tools,
        Some(vec!["read_file".to_string()])
    );
    let mut override_harness = install_mock_engine(&manager, &override_thread.id).await;
    manager
        .start_turn(
            &override_thread.id,
            StartTurnRequest {
                prompt: "turn settings win".to_string(),
                reasoning_effort: Some("off".to_string()),
                allowed_tools: Some(Vec::new()),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    match override_harness.rx_op.recv().await {
        Some(Op::SendMessage(TurnSpec {
            reasoning_effort,
            reasoning_effort_auto,
            allowed_tools,
            ..
        })) => {
            assert_eq!(reasoning_effort.as_deref(), Some("off"));
            assert!(!reasoning_effort_auto);
            assert_eq!(
                allowed_tools,
                Some(Vec::new()),
                "Some([]) must reach the engine as an empty model-visible catalog gate"
            );
        }
        other => panic!("expected turn override SendMessage op, got {other:?}"),
    }

    let thread_default = manager
        .create_thread(CreateThreadRequest {
            reasoning_effort: Some("medium".to_string()),
            allowed_tools: Some(vec!["read_file".to_string()]),
            ..CreateThreadRequest::default()
        })
        .await?;
    let mut thread_harness = install_mock_engine(&manager, &thread_default.id).await;
    manager
        .start_turn(
            &thread_default.id,
            StartTurnRequest {
                prompt: "thread settings win over config".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    match thread_harness.rx_op.recv().await {
        Some(Op::SendMessage(TurnSpec {
            reasoning_effort,
            allowed_tools,
            ..
        })) => {
            assert_eq!(
                reasoning_effort.as_deref(),
                Some("high"),
                "the thread's medium preference is normalized for the exact DeepSeek route"
            );
            assert_eq!(allowed_tools, Some(vec!["read_file".to_string()]));
        }
        other => panic!("expected thread-default SendMessage op, got {other:?}"),
    }

    let configured_default = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut config_harness = install_mock_engine(&manager, &configured_default.id).await;
    manager
        .start_turn(
            &configured_default.id,
            StartTurnRequest {
                prompt: "fall back to config".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    match config_harness.rx_op.recv().await {
        Some(Op::SendMessage(TurnSpec {
            reasoning_effort,
            allowed_tools,
            ..
        })) => {
            assert_eq!(reasoning_effort.as_deref(), Some("high"));
            assert_eq!(
                allowed_tools, None,
                "no API allowlist keeps the normal configured tool catalog"
            );
        }
        other => panic!("expected config-default SendMessage op, got {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn runtime_thread_rejects_unknown_reasoning_effort() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let error = manager
        .create_thread(CreateThreadRequest {
            reasoning_effort: Some("surprise-me".to_string()),
            ..CreateThreadRequest::default()
        })
        .await
        .expect_err("new Runtime API inputs must not silently become max")
        .to_string();
    assert!(error.contains("Unrecognized reasoning effort"), "{error}");
    assert!(manager.store.list_threads()?.is_empty());

    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let error = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "reject invalid turn setting".to_string(),
                reasoning_effort: Some("surprise-me".to_string()),
                ..StartTurnRequest::default()
            },
        )
        .await
        .expect_err("per-turn Runtime API inputs use the same strict parser")
        .to_string();
    assert!(error.contains("Unrecognized reasoning effort"), "{error}");
    assert!(manager.store.list_turns_for_thread(&thread.id)?.is_empty());
    Ok(())
}

#[test]
fn runtime_request_wire_distinguishes_missing_and_empty_tool_allowlists() -> Result<()> {
    let inherited: StartTurnRequest = serde_json::from_value(json!({ "prompt": "inherit" }))?;
    assert_eq!(inherited.operation_key, None);
    assert_eq!(inherited.reasoning_effort, None);
    assert_eq!(inherited.allowed_tools, None);

    let disabled: StartTurnRequest = serde_json::from_value(json!({
        "prompt": "no tools",
        "operation_key": "cwc-turn-42",
        "reasoning_effort": "high",
        "allowed_tools": []
    }))?;
    assert_eq!(disabled.operation_key.as_deref(), Some("cwc-turn-42"));
    assert_eq!(disabled.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(disabled.allowed_tools, Some(Vec::new()));

    let camel: StartTurnRequest = serde_json::from_value(json!({
        "prompt": "compat alias",
        "operationKey": "cwc-turn-camel"
    }))?;
    assert_eq!(camel.operation_key.as_deref(), Some("cwc-turn-camel"));
    Ok(())
}

#[test]
fn runtime_turn_operation_keys_are_bounded_scoped_and_fingerprint_execution_policy() -> Result<()> {
    assert!(validate_runtime_turn_operation_key("valid-key_42").is_ok());
    assert!(
        validate_runtime_turn_operation_key(&"k".repeat(MAX_RUNTIME_TURN_OPERATION_KEY_BYTES))
            .is_ok()
    );
    for invalid in [
        "".to_string(),
        " leading".to_string(),
        "trailing ".to_string(),
        "control\nkey".to_string(),
        "k".repeat(MAX_RUNTIME_TURN_OPERATION_KEY_BYTES + 1),
    ] {
        assert!(
            validate_runtime_turn_operation_key(&invalid).is_err(),
            "invalid operation key was accepted"
        );
    }

    let key = "same-caller-key";
    let owner_a = runtime_turn_operation_key_fingerprint("owner_a", "thr_a", key)?;
    assert_ne!(
        owner_a,
        runtime_turn_operation_key_fingerprint("owner_b", "thr_a", key)?,
        "operation keys must be Runtime-store scoped"
    );
    assert_ne!(
        owner_a,
        runtime_turn_operation_key_fingerprint("owner_a", "thr_b", key)?,
        "operation keys must be thread scoped"
    );

    let mut thread = sample_thread("thr_fingerprint");
    thread.model_provider = Some("deepseek".to_string());
    thread.model_provider_id = Some("deepseek".to_string());
    let policy = RuntimePolicyProjection::from_persisted("agent", Some("ask"), false);
    let tools = vec!["read_file".to_string()];
    let base = runtime_turn_request_fingerprint(
        &thread,
        "same prompt",
        None,
        "deepseek-v4-pro",
        Some(crate::reasoning_preference::ReasoningEffort::High),
        Some(&tools),
        policy,
        false,
        false,
        &[],
        None,
        &[],
        None,
    )?;

    let mut different_provider = thread.clone();
    different_provider.model_provider = Some("openai-codex".to_string());
    different_provider.model_provider_id = Some("openai-codex".to_string());
    let changed = [
        runtime_turn_request_fingerprint(
            &different_provider,
            "same prompt",
            None,
            "deepseek-v4-pro",
            Some(crate::reasoning_preference::ReasoningEffort::High),
            Some(&tools),
            policy,
            false,
            false,
            &[],
            None,
            &[],
            None,
        )?,
        runtime_turn_request_fingerprint(
            &thread,
            "different prompt",
            None,
            "deepseek-v4-pro",
            Some(crate::reasoning_preference::ReasoningEffort::High),
            Some(&tools),
            policy,
            false,
            false,
            &[],
            None,
            &[],
            None,
        )?,
        runtime_turn_request_fingerprint(
            &thread,
            "same prompt",
            None,
            "different-model",
            Some(crate::reasoning_preference::ReasoningEffort::High),
            Some(&tools),
            policy,
            false,
            false,
            &[],
            None,
            &[],
            None,
        )?,
        runtime_turn_request_fingerprint(
            &thread,
            "same prompt",
            None,
            "deepseek-v4-pro",
            Some(crate::reasoning_preference::ReasoningEffort::Medium),
            Some(&tools),
            policy,
            false,
            false,
            &[],
            None,
            &[],
            None,
        )?,
        runtime_turn_request_fingerprint(
            &thread,
            "same prompt",
            None,
            "deepseek-v4-pro",
            Some(crate::reasoning_preference::ReasoningEffort::High),
            Some(&[]),
            policy,
            false,
            false,
            &[],
            None,
            &[],
            None,
        )?,
        runtime_turn_request_fingerprint(
            &thread,
            "same prompt",
            None,
            "deepseek-v4-pro",
            Some(crate::reasoning_preference::ReasoningEffort::High),
            Some(&tools),
            RuntimePolicyProjection::from_persisted("operate", Some("full_access"), true),
            true,
            true,
            &[],
            None,
            &[],
            None,
        )?,
    ];
    assert!(
        changed.iter().all(|fingerprint| fingerprint != &base),
        "provider/model/prompt/reasoning/tool or permission changes must alter the fingerprint"
    );
    Ok(())
}

#[tokio::test]
async fn real_turn_client_preflight_failure_writes_no_in_progress_record() -> Result<()> {
    let mut custom = std::collections::HashMap::new();
    custom.insert(
        "preflight-failure".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("https://preflight.invalid/v1".to_string()),
            model: Some("preflight-model".to_string()),
            api_key: Some("test-key".to_string()),
            // Client construction rejects this independently of ambient auth,
            // keeping the async regression hermetic without a global env lock.
            insecure_skip_tls_verify: Some(true),
            ..codewhale_runtime::config::ProviderConfig::default()
        },
    );
    let workspace = tempfile::tempdir()?;
    let manager = RuntimeThreadManager::open(
        {
            let __cw_config_init_9_0 = Some("preflight-failure".to_string());
            let __cw_config_init_9_1 = Some(codewhale_runtime::config::ProvidersConfig {
                custom,
                ..codewhale_runtime::config::ProvidersConfig::default()
            });
            let mut __cw_config_init_9_value: Config = Config::default();
            __cw_config_init_9_value.provider = __cw_config_init_9_0;
            __cw_config_init_9_value.providers = __cw_config_init_9_1;
            __cw_config_init_9_value
        },
        workspace.path().to_path_buf(),
        test_manager_config(test_runtime_dir()),
    )?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;

    let error = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "must not become a zombie turn".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await
        .expect_err("missing credentials must fail before turn persistence")
        .to_string();

    assert!(
        error.contains("TLS certificate verification cannot be disabled"),
        "{error}"
    );
    assert!(manager.store.list_turns_for_thread(&thread.id)?.is_empty());
    assert_eq!(manager.get_thread(&thread.id).await?.latest_turn_id, None);
    Ok(())
}

#[tokio::test]
async fn closed_turn_mailbox_rolls_back_durable_records_and_active_claim() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let harness = install_mock_engine(&manager, &thread.id).await;
    let before_active = {
        let active = manager.active.lock().await;
        let state = active.engines.get(&thread.id).expect("installed engine");
        (
            state.active_turn.as_ref().map(|turn| turn.turn_id.clone()),
            state.route_identity.clone(),
            state.route_model.clone(),
            active.lru.clone(),
        )
    };
    let before_thread = serde_json::to_value(manager.get_thread(&thread.id).await?)?;
    let before_events = serde_json::to_value(manager.events_since(&thread.id, None)?)?;
    drop(harness.rx_op);

    let error = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "mailbox is already closed".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await
        .expect_err("closed mailbox must reject the turn")
        .to_string();
    assert!(error.contains("Failed to start turn"), "{error}");

    assert!(manager.store.list_turns_for_thread(&thread.id)?.is_empty());
    assert_eq!(
        serde_json::to_value(manager.get_thread(&thread.id).await?)?,
        before_thread
    );
    assert_eq!(
        serde_json::to_value(manager.events_since(&thread.id, None)?)?,
        before_events
    );
    assert_eq!(
        std::fs::read_dir(&manager.store.items_dir)?.count(),
        0,
        "failed send must remove the optimistic user item"
    );
    let after_active = {
        let active = manager.active.lock().await;
        let state = active.engines.get(&thread.id).expect("installed engine");
        (
            state.active_turn.as_ref().map(|turn| turn.turn_id.clone()),
            state.route_identity.clone(),
            state.route_model.clone(),
            active.lru.clone(),
        )
    };
    assert_eq!(after_active, before_active);
    Ok(())
}

#[tokio::test]
async fn cancellation_while_waiting_for_mailbox_capacity_claims_nothing() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;

    for _ in 0..32 {
        harness.handle.try_send(Op::ListSubAgents)?;
    }
    let start_sender_count = harness.handle.tx_op.strong_count();
    let start_manager = manager.clone();
    let start_thread_id = thread.id.clone();
    let start_task = tokio::spawn(async move {
        start_manager
            .start_turn(
                &start_thread_id,
                StartTurnRequest {
                    prompt: "cancel before mailbox capacity".to_string(),
                    ..StartTurnRequest::default()
                },
            )
            .await
    });
    wait_for_sender_strong_count(&harness.handle.tx_op, start_sender_count + 2).await?;
    assert!(
        !start_task.is_finished(),
        "start should be waiting for capacity"
    );
    assert!(manager.store.list_turns_for_thread(&thread.id)?.is_empty());
    assert_eq!(manager.get_thread(&thread.id).await?.latest_turn_id, None);
    assert_eq!(manager.active_turn_flags(&thread.id, "missing").await, None);
    start_task.abort();
    let _ = start_task.await;
    for _ in 0..32 {
        assert!(matches!(
            harness.rx_op.recv().await,
            Some(Op::ListSubAgents)
        ));
    }

    for _ in 0..32 {
        harness.handle.try_send(Op::ListSubAgents)?;
    }
    let compact_sender_count = harness.handle.tx_op.strong_count();
    let compact_manager = manager.clone();
    let compact_thread_id = thread.id.clone();
    let compact_task = tokio::spawn(async move {
        compact_manager
            .compact_thread(&compact_thread_id, CompactThreadRequest::default())
            .await
    });
    wait_for_sender_strong_count(&harness.handle.tx_op, compact_sender_count + 2).await?;
    assert!(
        !compact_task.is_finished(),
        "compaction should be waiting for capacity"
    );
    assert!(manager.store.list_turns_for_thread(&thread.id)?.is_empty());
    assert_eq!(manager.get_thread(&thread.id).await?.latest_turn_id, None);
    compact_task.abort();
    let _ = compact_task.await;
    for _ in 0..32 {
        assert!(matches!(
            harness.rx_op.recv().await,
            Some(Op::ListSubAgents)
        ));
    }
    Ok(())
}

#[tokio::test]
async fn caller_cancellation_after_engine_acceptance_keeps_owned_turn_lifecycle() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;

    // Block the first start-event append so the public future remains
    // cancellable after the operation has entered the engine mailbox.
    let mut event_lock = fd_lock::RwLock::new(manager.store.open_event_lock()?);
    let event_state_guard = event_lock.try_write()?;
    let start_manager = manager.clone();
    let thread_id = thread.id.clone();
    let start_task = tokio::spawn(async move {
        start_manager
            .start_turn(
                &thread_id,
                StartTurnRequest {
                    prompt: "the lifecycle outlives its caller".to_string(),
                    ..StartTurnRequest::default()
                },
            )
            .await
    });
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), harness.rx_op.recv()).await?,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    let turns = manager.store.list_turns_for_thread(&thread.id)?;
    assert_eq!(turns.len(), 1);
    let turn_id = turns[0].id.clone();
    assert_eq!(turns[0].status, RuntimeTurnStatus::InProgress);
    assert_eq!(turns[0].item_ids.len(), 1);
    assert_eq!(
        manager.store.load_item(&turns[0].item_ids[0])?.turn_id,
        turn_id
    );
    assert!(
        manager
            .active_turn_flags(&thread.id, &turn_id)
            .await
            .is_some()
    );

    start_task.abort();
    let _ = start_task.await;
    drop(event_state_guard);

    harness
        .tx_event
        .send(EngineEvent::MessageStarted { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageDelta {
            index: 0,
            content: "owned monitor is live".to_string(),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageComplete { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    let terminal = wait_for_terminal_turn(&manager, &turn_id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Completed);
    assert_eq!(manager.active_turn_flags(&thread.id, &turn_id).await, None);

    let lifecycle: Vec<String> = manager
        .events_since(&thread.id, None)?
        .iter()
        .filter(|event| event.turn_id.as_deref() == Some(turn_id.as_str()))
        .map(|event| event.event.clone())
        .collect();
    assert_eq!(
        &lifecycle[..3],
        &["turn.started", "item.started", "item.completed"]
    );
    assert_eq!(lifecycle.last().map(String::as_str), Some("turn.completed"));
    Ok(())
}

#[tokio::test]
async fn operation_key_replays_torn_response_survives_restart_and_rejects_mismatch() -> Result<()> {
    let runtime_dir = test_runtime_dir();
    let manager = test_manager(runtime_dir.clone())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let operation_key = "cwc-op-sk-fixture-must-not-persist".to_string();
    let request = StartTurnRequest {
        prompt: "idempotent turn payload".to_string(),
        images: Vec::new(),
        operation_key: Some(operation_key.clone()),
        reasoning_effort: Some("high".to_string()),
        allowed_tools: Some(Vec::new()),
        ..StartTurnRequest::default()
    };

    // Hold the start receipt so the engine has accepted and persisted the
    // turn while the HTTP-equivalent caller still has no response.
    let mut event_lock = fd_lock::RwLock::new(manager.store.open_event_lock()?);
    let event_state_guard = event_lock.try_write()?;
    let start_manager = manager.clone();
    let start_thread_id = thread.id.clone();
    let first_request = request.clone();
    let start_task = tokio::spawn(async move {
        start_manager
            .start_turn(&start_thread_id, first_request)
            .await
    });
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), harness.rx_op.recv()).await?,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    let turns = manager.store.list_turns_for_thread(&thread.id)?;
    assert_eq!(turns.len(), 1);
    let original = turns[0].clone();

    // Simulate a torn HTTP response. Retrying immediately returns the same
    // in-progress turn and never places a second operation in the engine.
    start_task.abort();
    let _ = start_task.await;
    let replay = tokio::time::timeout(
        Duration::from_secs(2),
        manager.start_turn(&thread.id, request.clone()),
    )
    .await??;
    assert_eq!(replay.id, original.id);
    assert!(harness.rx_op.try_recv().is_err());

    let key_fingerprint = runtime_turn_operation_key_fingerprint(
        &manager.store.owner_id,
        &thread.id,
        &operation_key,
    )?;
    let binding = manager
        .store
        .load_turn_operation_binding(&key_fingerprint)?
        .context("persisted turn operation binding")?;
    assert_eq!(binding.turn_id, original.id);
    assert_eq!(binding.thread_id, thread.id);
    assert_eq!(binding.operation_key_fingerprint, key_fingerprint);
    assert_eq!(binding.request_fingerprint.len(), 64);
    assert!(
        !serde_json::to_string(&binding)?.contains(&operation_key),
        "raw operation key entered its durable binding"
    );

    drop(event_state_guard);
    harness
        .tx_event
        .send(EngineEvent::MessageStarted { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageDelta {
            index: 0,
            content: "one execution".to_string(),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageComplete { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    let terminal = wait_for_terminal_turn(&manager, &original.id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Completed);
    assert_eq!(manager.store.list_turns_for_thread(&thread.id)?.len(), 1);

    let leaked_key_file = ignore::WalkBuilder::new(&runtime_dir)
        .hidden(false)
        .build()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .find_map(|entry| {
            fs::read_to_string(entry.path())
                .ok()
                .filter(|contents| contents.contains(&operation_key))
                .map(|_| entry.path().to_path_buf())
        });
    assert!(
        leaked_key_file.is_none(),
        "raw operation key leaked into {}",
        leaked_key_file
            .as_deref()
            .map(std::path::Path::display)
            .map(|path| path.to_string())
            .unwrap_or_default()
    );

    drop(harness);
    drop(manager);
    let reopened = test_manager(runtime_dir)?;
    let replay_after_restart = reopened.start_turn(&thread.id, request.clone()).await?;
    assert_eq!(replay_after_restart.id, original.id);
    assert_eq!(replay_after_restart.status, RuntimeTurnStatus::Completed);

    let mut mismatches = Vec::new();
    let mut changed_prompt = request.clone();
    changed_prompt.prompt = "different prompt".to_string();
    mismatches.push(changed_prompt);
    let mut changed_model = request.clone();
    changed_model.model = Some("different-model".to_string());
    mismatches.push(changed_model);
    let mut changed_reasoning = request.clone();
    changed_reasoning.reasoning_effort = Some("medium".to_string());
    mismatches.push(changed_reasoning);
    let mut changed_tools = request.clone();
    changed_tools.allowed_tools = Some(vec!["read_file".to_string()]);
    mismatches.push(changed_tools);
    let mut changed_permission = request.clone();
    changed_permission.permission_posture = Some("full_access".to_string());
    mismatches.push(changed_permission);

    for mismatch in mismatches {
        let error = reopened
            .start_turn(&thread.id, mismatch)
            .await
            .expect_err("operation-key reuse with changed execution must fail closed")
            .to_string();
        assert!(error.contains("different turn request"), "{error}");
    }
    assert_eq!(reopened.store.list_turns_for_thread(&thread.id)?.len(), 1);

    let mut oversized = request;
    oversized.operation_key = Some("k".repeat(MAX_RUNTIME_TURN_OPERATION_KEY_BYTES + 1));
    let error = reopened
        .start_turn(&thread.id, oversized)
        .await
        .expect_err("oversized operation keys must be rejected")
        .to_string();
    assert!(error.contains("cannot exceed"), "{error}");
    Ok(())
}

mod turn_operation_lookup {
    use super::*;

    fn binding(
        manager: &RuntimeThreadManager,
        thread: &ThreadRecord,
        turn: &TurnRecord,
        key: &str,
    ) -> Result<RuntimeTurnOperationBinding> {
        Ok(RuntimeTurnOperationBinding {
            schema_version: TURN_OPERATION_BINDING_SCHEMA_VERSION,
            thread_id: thread.id.clone(),
            turn_id: turn.id.clone(),
            operation_key_fingerprint: runtime_turn_operation_key_fingerprint(
                &manager.store.owner_id,
                &thread.id,
                key,
            )?,
            request_fingerprint: codewhale_runtime::hashing::sha256_hex(
                "original request configuration",
            ),
            created_at: turn.created_at,
        })
    }

    // Callers snapshot the manager-owned runtime directory. CODEWHALE_HOME is
    // process-wide, so unrelated parallel tests can write elsewhere beneath
    // the temporary root while a read-only operation is being checked.
    fn directory_bytes(
        root: &Path,
    ) -> Result<std::collections::BTreeMap<PathBuf, Option<Vec<u8>>>> {
        let mut pending = vec![root.to_path_buf()];
        let mut snapshot = std::collections::BTreeMap::new();
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                let bytes = if entry.file_type()?.is_dir() {
                    pending.push(path.clone());
                    None
                } else if entry.metadata()?.len() == 0 {
                    // Empty claim locks may be held exclusively on Windows;
                    // their length already proves their complete byte content.
                    Some(Vec::new())
                } else {
                    Some(fs::read(&path)?)
                };
                snapshot.insert(path.strip_prefix(root)?.to_path_buf(), bytes);
            }
        }
        Ok(snapshot)
    }

    #[tokio::test]
    async fn exact_lookup_is_read_only_without_an_engine_and_survives_restart() -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", temp.path());
        let runtime_dir = temp.path().join("runtime");
        let mut manager = test_manager(runtime_dir.clone())?;
        let mut thread = sample_thread("thr_lookup_exact");
        let mut turn = sample_turn(
            &thread.id,
            "turn_lookup_exact",
            RuntimeTurnStatus::Completed,
        );
        thread.latest_turn_id = Some(turn.id.clone());
        turn.input_summary = "the exact accepted fixture".into();
        turn.effective_model = Some("retired-fixture-model".into());
        turn.ended_at = Some(turn.created_at + chrono::Duration::seconds(2));
        turn.duration_ms = Some(2_000);
        let key = "lookup-fixture-key";
        let binding = binding(&manager, &thread, &turn, key)?;
        manager.store.save_thread(&thread)?;
        manager.store.save_turn(&turn)?;
        manager.store.save_turn_operation_binding(&binding)?;
        drop(
            manager
                .store
                .open_turn_operation_claim_lock(&binding.operation_key_fingerprint)?,
        );
        let owner_id = manager.store.owner_id.clone();
        let expected = serde_json::to_value(&turn)?;

        for restart in [false, true] {
            if restart {
                drop(manager);
                manager = test_manager(runtime_dir.clone())?;
            }
            assert_eq!(manager.store.owner_id, owner_id);
            assert!(manager.active.lock().await.engines.is_empty());
            let before = directory_bytes(&runtime_dir)?;
            let observed = manager
                .lookup_turn_operation(&thread.id, key)?
                .context("accepted operation must remain available")?;
            assert_eq!(serde_json::to_value(observed)?, expected);
            assert!(manager.active.lock().await.engines.is_empty());
            assert_eq!(directory_bytes(&runtime_dir)?, before);
        }
        Ok(())
    }

    #[tokio::test]
    async fn absence_and_incomplete_acceptance_never_create_or_repair_records() -> Result<()> {
        use RuntimeTurnOperationLookupError::Incomplete;
        let _env = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", temp.path());
        let runtime_dir = temp.path().join("runtime");
        let manager = test_manager(runtime_dir.clone())?;
        let thread = sample_thread("thr_lookup_incomplete");
        let turn = sample_turn(
            &thread.id,
            "turn_lookup_incomplete",
            RuntimeTurnStatus::Completed,
        );
        let key = "incomplete-fixture-key";
        let binding = binding(&manager, &thread, &turn, key)?;
        let lock_path = manager
            .store
            .turn_operation_lock_path(&binding.operation_key_fingerprint)?;
        let before = directory_bytes(&runtime_dir)?;
        assert!(manager.lookup_turn_operation(&thread.id, key)?.is_none());
        assert_eq!(directory_bytes(&runtime_dir)?, before);

        drop(
            manager
                .store
                .open_turn_operation_claim_lock(&binding.operation_key_fingerprint)?,
        );
        assert!(manager.lookup_turn_operation(&thread.id, key)?.is_none());
        fs::remove_file(&lock_path)?;
        manager.store.save_thread(&thread)?;
        manager.store.save_turn(&turn)?;
        manager.store.save_turn_operation_binding(&binding)?;
        let before = directory_bytes(&runtime_dir)?;
        assert!(matches!(
            manager.lookup_turn_operation(&thread.id, key),
            Err(Incomplete)
        ));
        assert_eq!(
            directory_bytes(&runtime_dir)?,
            before,
            "lookup cannot recreate a missing lock"
        );

        drop(
            manager
                .store
                .open_turn_operation_claim_lock(&binding.operation_key_fingerprint)?,
        );
        manager.store.remove_turn(&turn.id)?;
        let before = directory_bytes(&runtime_dir)?;
        assert!(matches!(
            manager.lookup_turn_operation(&thread.id, key),
            Err(Incomplete)
        ));
        assert_eq!(
            directory_bytes(&runtime_dir)?,
            before,
            "lookup cannot recover a torn binding"
        );

        manager.store.save_turn(&turn)?;
        let mut claim = fd_lock::RwLock::new(
            manager
                .store
                .open_turn_operation_claim_lock(&binding.operation_key_fingerprint)?,
        );
        let guard = claim.try_write()?;
        let before = directory_bytes(&runtime_dir)?;
        assert!(matches!(
            manager.lookup_turn_operation(&thread.id, key),
            Err(Incomplete)
        ));
        assert_eq!(directory_bytes(&runtime_dir)?, before);
        drop(guard);
        assert_eq!(
            manager.lookup_turn_operation(&thread.id, key)?.unwrap().id,
            turn.id
        );
        assert!(manager.active.lock().await.engines.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn mismatched_records_are_hidden_and_invalid_store_objects_are_not_absence() -> Result<()>
    {
        use RuntimeTurnOperationLookupError::Unavailable;
        let _env = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", temp.path());
        let runtime_dir = temp.path().join("runtime");
        let manager = test_manager(runtime_dir.clone())?;
        let thread = sample_thread("thr_lookup_scope");
        let turn = sample_turn(
            &thread.id,
            "turn_lookup_scope",
            RuntimeTurnStatus::Completed,
        );
        let key = "scope-fixture-key";
        let binding = binding(&manager, &thread, &turn, key)?;
        let binding_path = manager
            .store
            .turn_operation_path(&binding.operation_key_fingerprint)?;
        let lock_path = manager
            .store
            .turn_operation_lock_path(&binding.operation_key_fingerprint)?;
        let turn_path = manager.store.turn_path(&turn.id)?;
        let thread_path = manager.store.thread_path(&thread.id)?;
        drop(
            manager
                .store
                .open_turn_operation_claim_lock(&binding.operation_key_fingerprint)?,
        );

        for mismatch in [
            "binding_thread",
            "binding_hash",
            "turn_id",
            "turn_thread",
            "thread_id",
        ] {
            let mut candidate_binding = binding.clone();
            let mut candidate_turn = turn.clone();
            let mut candidate_thread = thread.clone();
            match mismatch {
                "binding_thread" => candidate_binding.thread_id = "thr_other".into(),
                "binding_hash" => {
                    candidate_binding.operation_key_fingerprint =
                        codewhale_runtime::hashing::sha256_hex("other operation")
                }
                "turn_id" => candidate_turn.id = "turn_other".into(),
                "turn_thread" => candidate_turn.thread_id = "thr_other".into(),
                "thread_id" => candidate_thread.id = "thr_other".into(),
                _ => unreachable!(),
            }
            // Keep the expected filenames: save_* would follow the mutated
            // identity and accidentally test missing files instead of mismatch.
            write_json_atomic(&binding_path, &candidate_binding)?;
            write_json_atomic(&turn_path, &candidate_turn)?;
            write_json_atomic(&thread_path, &candidate_thread)?;
            let before = directory_bytes(&runtime_dir)?;
            assert!(
                manager.lookup_turn_operation(&thread.id, key)?.is_none(),
                "{mismatch}"
            );
            assert_eq!(directory_bytes(&runtime_dir)?, before, "{mismatch}");
        }
        manager.store.save_thread(&thread)?;
        manager.store.save_turn(&turn)?;
        manager.store.save_turn_operation_binding(&binding)?;
        assert!(manager.lookup_turn_operation("thr_other", key)?.is_none());

        fs::write(&binding_path, "{")?;
        assert!(matches!(
            manager.lookup_turn_operation(&thread.id, key),
            Err(Unavailable)
        ));
        fs::remove_file(&binding_path)?;
        fs::create_dir(&binding_path)?;
        assert!(matches!(
            manager.lookup_turn_operation(&thread.id, key),
            Err(Unavailable)
        ));
        fs::remove_dir(&binding_path)?;
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(temp.path().join("missing-binding"), &binding_path)?;
            assert!(matches!(
                manager.lookup_turn_operation(&thread.id, key),
                Err(Unavailable)
            ));
            fs::remove_file(&binding_path)?;
        }
        manager.store.save_turn_operation_binding(&binding)?;
        fs::remove_file(&lock_path)?;
        fs::create_dir(&lock_path)?;
        assert!(matches!(
            manager.lookup_turn_operation(&thread.id, key),
            Err(Unavailable)
        ));
        fs::remove_dir(&lock_path)?;
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(temp.path().join("missing-lock"), &lock_path)?;
            assert!(matches!(
                manager.lookup_turn_operation(&thread.id, key),
                Err(Unavailable)
            ));
            fs::remove_file(&lock_path)?;
        }
        let moved_operations = temp.path().join("unavailable-operations");
        fs::rename(&manager.store.turn_operations_dir, &moved_operations)?;
        assert!(matches!(
            manager.lookup_turn_operation(&thread.id, key),
            Err(Unavailable)
        ));
        fs::rename(&moved_operations, &manager.store.turn_operations_dir)?;
        assert!(manager.active.lock().await.engines.is_empty());
        Ok(())
    }
}

#[tokio::test]
async fn restart_removes_torn_operation_item_before_reserved_turn_retry() -> Result<()> {
    let runtime_dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(runtime_dir.clone())?;
    let thread = sample_thread("thr_torn_operation_item");
    store.save_thread(&thread)?;
    let operation_key = "relay-torn-item-operation";
    let key_fingerprint =
        runtime_turn_operation_key_fingerprint(&store.owner_id, &thread.id, operation_key)?;
    let turn_id = "turn_relay_reserved_retry";
    store.save_turn_operation_binding(&RuntimeTurnOperationBinding {
        schema_version: TURN_OPERATION_BINDING_SCHEMA_VERSION,
        thread_id: thread.id.clone(),
        turn_id: turn_id.to_string(),
        operation_key_fingerprint: key_fingerprint.clone(),
        request_fingerprint: codewhale_runtime::hashing::sha256_hex("torn-request"),
        created_at: Utc::now(),
    })?;
    let mut orphan = sample_item(
        turn_id,
        "item_torn_before_turn_commit",
        TurnItemLifecycleStatus::Completed,
    );
    orphan.kind = TurnItemKind::UserMessage;
    store.save_item(&orphan)?;
    drop(store);

    let manager = test_manager(runtime_dir)?;
    assert!(
        manager
            .store
            .load_turn_operation_binding(&key_fingerprint)?
            .is_none()
    );
    assert!(manager.store.list_items_for_turn(turn_id)?.is_empty());

    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn_with_reserved_id(
            &thread.id,
            StartTurnRequest {
                prompt: "retry after the pre-submit crash".to_string(),
                images: Vec::new(),
                operation_key: Some(operation_key.to_string()),
                ..StartTurnRequest::default()
            },
            turn_id,
        )
        .await?;
    assert_eq!(turn.id, turn_id);
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    assert_eq!(
        manager.store.list_items_for_turn(turn_id)?.len(),
        1,
        "the retry must own exactly one user item"
    );
    Ok(())
}

#[tokio::test]
async fn thread_updates_while_start_waits_for_capacity_survive_latest_turn_write() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    for _ in 0..32 {
        harness.handle.try_send(Op::ListSubAgents)?;
    }
    let sender_count = harness.handle.tx_op.strong_count();

    let start_manager = manager.clone();
    let thread_id = thread.id.clone();
    let start_task = tokio::spawn(async move {
        start_manager
            .start_turn(
                &thread_id,
                StartTurnRequest {
                    prompt: "preserve concurrent metadata".to_string(),
                    ..StartTurnRequest::default()
                },
            )
            .await
    });
    wait_for_sender_strong_count(&harness.handle.tx_op, sender_count + 2).await?;
    assert!(!start_task.is_finished());

    manager
        .update_thread(
            &thread.id,
            UpdateThreadRequest {
                title: Some("new title while queued".to_string()),
                ..UpdateThreadRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::ListSubAgents)
    ));
    let turn = tokio::time::timeout(TURN_SETTLEMENT_DEADLOCK_TIMEOUT, start_task).await???;
    let mut saw_send = false;
    for _ in 0..32 {
        if matches!(
            harness.rx_op.recv().await,
            Some(Op::SendMessage(TurnSpec { .. }))
        ) {
            saw_send = true;
            break;
        }
    }
    assert!(
        saw_send,
        "accepted send must remain behind refresh operations"
    );

    harness
        .tx_event
        .send(EngineEvent::MessageStarted { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageDelta {
            index: 0,
            content: "metadata retained".to_string(),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageComplete { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    let terminal = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Completed);
    assert_eq!(turn.item_ids.len(), 1);
    assert!(
        terminal.item_ids.contains(&turn.item_ids[0]),
        "the accepted user item must survive later assistant-item writes"
    );
    assert_eq!(
        manager.store.load_turn(&turn.id)?.item_ids,
        terminal.item_ids
    );
    let updated = manager.get_thread(&thread.id).await?;
    assert_eq!(updated.title.as_deref(), Some("new title while queued"));
    assert_eq!(updated.latest_turn_id.as_deref(), Some(turn.id.as_str()));
    Ok(())
}

#[tokio::test]
async fn execution_update_while_start_waits_rejects_stale_operation() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    for _ in 0..32 {
        harness.handle.try_send(Op::ListSubAgents)?;
    }
    let sender_count = harness.handle.tx_op.strong_count();
    let start_manager = manager.clone();
    let thread_id = thread.id.clone();
    let start_task = tokio::spawn(async move {
        start_manager
            .start_turn(
                &thread_id,
                StartTurnRequest {
                    prompt: "must not use stale mode".to_string(),
                    ..StartTurnRequest::default()
                },
            )
            .await
    });
    wait_for_sender_strong_count(&harness.handle.tx_op, sender_count + 2).await?;

    manager
        .update_thread(
            &thread.id,
            UpdateThreadRequest {
                mode: Some(AppMode::Plan.as_setting().to_string()),
                ..UpdateThreadRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::ListSubAgents)
    ));
    let error = tokio::time::timeout(Duration::from_secs(2), start_task)
        .await??
        .expect_err("stale operation must fail")
        .to_string();
    assert!(error.contains("execution settings changed"), "{error}");
    for _ in 0..31 {
        assert!(matches!(
            harness.rx_op.recv().await,
            Some(Op::ListSubAgents)
        ));
    }
    assert!(harness.rx_op.try_recv().is_err());
    assert!(manager.store.list_turns_for_thread(&thread.id)?.is_empty());
    let updated = manager.get_thread(&thread.id).await?;
    assert_eq!(updated.mode, AppMode::Plan.as_setting());
    assert_eq!(updated.latest_turn_id, None);
    assert_eq!(manager.active_turn_flags(&thread.id, "missing").await, None);
    Ok(())
}

#[tokio::test]
async fn compact_lifecycle_outlives_caller_and_preserves_concurrent_thread_updates() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    for _ in 0..32 {
        harness.handle.try_send(Op::ListSubAgents)?;
    }
    let sender_count = harness.handle.tx_op.strong_count();

    let compact_manager = manager.clone();
    let thread_id = thread.id.clone();
    let compact_task = tokio::spawn(async move {
        compact_manager
            .compact_thread(&thread_id, CompactThreadRequest::default())
            .await
    });
    wait_for_sender_strong_count(&harness.handle.tx_op, sender_count + 2).await?;
    assert!(!compact_task.is_finished());
    manager
        .update_thread(
            &thread.id,
            UpdateThreadRequest {
                title: Some("title before compact claim".to_string()),
                ..UpdateThreadRequest::default()
            },
        )
        .await?;
    // Once capacity is released, block the acknowledgement events so the
    // API future can be dropped after the engine accepted the operation.
    let mut event_lock = fd_lock::RwLock::new(manager.store.open_event_lock()?);
    let event_state_guard = event_lock.try_write()?;
    let mut saw_compact = false;
    for _ in 0..33 {
        if matches!(
            tokio::time::timeout(Duration::from_secs(2), harness.rx_op.recv()).await?,
            Some(Op::CompactContext { .. })
        ) {
            saw_compact = true;
            break;
        }
    }
    assert!(
        saw_compact,
        "manual compaction must enter the engine mailbox"
    );
    let turns = manager.store.list_turns_for_thread(&thread.id)?;
    assert_eq!(turns.len(), 1);
    let turn_id = turns[0].id.clone();
    compact_task.abort();
    let _ = compact_task.await;
    drop(event_state_guard);

    harness
        .tx_event
        .send(EngineEvent::CompactionStarted {
            id: "manual_owned".to_string(),
            auto: false,
            message: "compaction started".to_string(),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::CompactionCompleted {
            id: "manual_owned".to_string(),
            auto: false,
            message: "compaction completed".to_string(),
            messages_before: Some(4),
            messages_after: Some(2),
            summary_prompt: None,
            post_input_tokens: None,
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    loop {
        match tokio::time::timeout(Duration::from_secs(5), harness.rx_op.recv()).await? {
            Some(Op::GetSessionSnapshot { tx }) => {
                answer_session_snapshot(&tx, "settled");
                break;
            }
            Some(_) => {}
            None => bail!("engine mailbox closed before the settled-history snapshot"),
        }
    }
    let terminal = wait_for_terminal_turn(&manager, &turn_id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Completed);
    assert_eq!(manager.active_turn_flags(&thread.id, &turn_id).await, None);
    let updated = manager.get_thread(&thread.id).await?;
    assert_eq!(updated.title.as_deref(), Some("title before compact claim"));
    assert_eq!(updated.latest_turn_id.as_deref(), Some(turn_id.as_str()));
    Ok(())
}

#[tokio::test]
async fn concurrent_turn_starts_leave_one_claim_and_one_consistent_durable_turn() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let first = manager.start_turn(
        &thread.id,
        StartTurnRequest {
            prompt: "first concurrent turn".to_string(),
            ..StartTurnRequest::default()
        },
    );
    let second = manager.start_turn(
        &thread.id,
        StartTurnRequest {
            prompt: "second concurrent turn".to_string(),
            ..StartTurnRequest::default()
        },
    );

    let (first, second) = tokio::join!(first, second);
    let (turn, rejection) = match (first, second) {
        (Ok(turn), Err(error)) | (Err(error), Ok(turn)) => (turn, error),
        (first, second) => {
            panic!("expected one accepted turn and one rejection: {first:?} {second:?}")
        }
    };
    assert!(
        rejection.to_string().contains("already has an active turn"),
        "{rejection}"
    );
    let turns = manager.store.list_turns_for_thread(&thread.id)?;
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].id, turn.id);
    assert_eq!(
        manager.get_thread(&thread.id).await?.latest_turn_id,
        Some(turn.id.clone())
    );
    assert_eq!(
        manager.active_turn_flags(&thread.id, &turn.id).await,
        Some((false, false))
    );
    match harness.rx_op.recv().await {
        Some(Op::SendMessage(TurnSpec { compaction, .. })) => {
            assert_eq!(
                compaction.runtime_cost_owner.as_deref(),
                Some(turn.id.as_str())
            );
        }
        other => panic!("expected SendMessage op, got {other:?}"),
    }
    assert!(harness.rx_op.try_recv().is_err());
    Ok(())
}

#[test]
fn idless_custom_thread_refuses_conflicting_root_and_exact_table() -> Result<()> {
    let mut custom = std::collections::HashMap::new();
    custom.insert(
        "custom".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("http://127.0.0.1:18182/v1".to_string()),
            model: Some("table-model".to_string()),
            ..codewhale_runtime::config::ProviderConfig::default()
        },
    );
    let config = {
        let __cw_config_init_10_0 = Some("custom".to_string());
        let __cw_config_init_10_1 = Some("legacy-root-model".to_string());
        let __cw_config_init_10_2 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let mut __cw_config_init_10_value: Config = Config::default();
        __cw_config_init_10_value.provider = __cw_config_init_10_0;
        __cw_config_init_10_value.default_text_model = __cw_config_init_10_1;
        __cw_config_init_10_value.providers = __cw_config_init_10_2;
        __cw_config_init_10_value
    }
    .with_legacy_root(None, Some("http://127.0.0.1:18181/v1".to_string()));
    let manager = RuntimeThreadManager::open(
        config.clone(),
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    let mut legacy = sample_thread("thr_ambiguous_legacy_custom");
    legacy.model = "legacy-saved-model".to_string();
    legacy.model_provider = Some("custom".to_string());
    legacy.model_provider_id = None;

    let error = manager
        .resolved_route_for_thread(&config, &legacy)
        .expect_err("conflicting root cannot bless exact table");
    assert!(error.to_string().contains("will not guess or fall back"));

    legacy.model_provider_id = Some("custom".to_string());
    let exact = manager.resolved_route_for_thread(&config, &legacy)?;
    assert_eq!(exact.identity.provider, ProviderKind::Custom);
    assert_eq!(exact.identity.key.as_str(), "custom");
    assert_eq!(exact.identity.persisted_id(), Some("custom"));
    assert_eq!(
        exact.config.active_route_base_url(),
        "http://127.0.0.1:18182/v1"
    );
    let root_only = {
        let __cw_config_init_11_0 = Some("custom".to_string());
        let __cw_config_init_11_1 = Some("legacy-root-model".to_string());
        let mut __cw_config_init_11_value: Config = Config::default();
        __cw_config_init_11_value.provider = __cw_config_init_11_0;
        __cw_config_init_11_value.default_text_model = __cw_config_init_11_1;
        __cw_config_init_11_value
    }
    .with_legacy_root(None, Some("http://127.0.0.1:18181/v1".to_string()));
    // A top-level-only literal route became the table, so the exact thread
    // resumes on it.
    let resumed = manager.resolved_route_for_thread(&root_only, &legacy)?;
    assert_eq!(
        resumed.config.active_route_base_url(),
        "http://127.0.0.1:18181/v1"
    );
    Ok(())
}

#[tokio::test]
async fn empty_imported_custom_id_fails_closed_when_root_and_table_coexist() -> Result<()> {
    let mut custom = std::collections::HashMap::new();
    custom.insert(
        "custom".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("http://127.0.0.1:18182/v1".to_string()),
            model: Some("table-model".to_string()),
            ..codewhale_runtime::config::ProviderConfig::default()
        },
    );
    let config = {
        let __cw_config_init_12_0 = Some("custom".to_string());
        let __cw_config_init_12_1 = Some("legacy-root-model".to_string());
        let __cw_config_init_12_2 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let mut __cw_config_init_12_value: Config = Config::default();
        __cw_config_init_12_value.provider = __cw_config_init_12_0;
        __cw_config_init_12_value.default_text_model = __cw_config_init_12_1;
        __cw_config_init_12_value.providers = __cw_config_init_12_2;
        __cw_config_init_12_value
    }
    .with_legacy_root(None, Some("http://127.0.0.1:18181/v1".to_string()));
    let manager = RuntimeThreadManager::open(
        config.clone(),
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;

    let mut imported = sample_thread("thr_empty_custom_id");
    imported.model_provider = Some("custom".to_string());
    imported.model_provider_id = Some("   ".to_string());
    let error = manager
        .resolved_route_for_thread(&config, &imported)
        .expect_err("malformed imported identity must not acquire the root route")
        .to_string();
    assert!(error.contains("empty exact provider id"), "{error}");

    let before = manager.store.list_threads()?.len();
    let request_error = manager
        .create_thread(CreateThreadRequest {
            model_provider: Some("custom".to_string()),
            model_provider_id: Some(String::new()),
            ..CreateThreadRequest::default()
        })
        .await
        .expect_err("malformed create request must fail before persistence")
        .to_string();
    assert!(
        request_error.contains("empty exact provider id"),
        "{request_error}"
    );
    assert_eq!(manager.store.list_threads()?.len(), before);
    Ok(())
}

#[tokio::test]
async fn thread_records_and_create_requests_preserve_provider_kind_id_pairing() -> Result<()> {
    let mut custom = std::collections::HashMap::new();
    custom.insert(
        "openai".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("http://127.0.0.1:18183/v1".to_string()),
            model: Some("custom-openai-model".to_string()),
            ..codewhale_runtime::config::ProviderConfig::default()
        },
    );
    let config = {
        let __cw_config_init_13_0 = Some("openai".to_string());
        let __cw_config_init_13_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let mut __cw_config_init_13_value: Config = Config::default();
        __cw_config_init_13_value.provider = __cw_config_init_13_0;
        __cw_config_init_13_value.providers = __cw_config_init_13_1;
        __cw_config_init_13_value
    };
    let manager = RuntimeThreadManager::open(
        config.clone(),
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;

    for provider_id in [None, Some("openai".to_string())] {
        let mut built_in = sample_thread("thr_builtin_openai_collision");
        built_in.model_provider = Some("openai".to_string());
        built_in.model_provider_id = provider_id;
        let error = manager
            .resolved_route_for_thread(&config, &built_in)
            .expect_err("built-in thread must not route through same-key custom endpoint")
            .to_string();
        assert!(error.contains("requires built-in 'openai'"), "{error}");
        assert!(error.contains("shadows"), "{error}");
    }

    let mut exact_custom = sample_thread("thr_custom_openai_collision");
    exact_custom.model = "custom-openai-model".to_string();
    exact_custom.model_provider = Some("custom".to_string());
    exact_custom.model_provider_id = Some("openai".to_string());
    let route = manager.resolved_route_for_thread(&config, &exact_custom)?;
    assert_eq!(route.identity.provider, ProviderKind::Custom);
    assert_eq!(route.identity.key.as_str(), "openai");
    assert_eq!(
        route.config.active_route_base_url(),
        "http://127.0.0.1:18183/v1"
    );

    let mut auto_thread = exact_custom.clone();
    auto_thread.id = "thr_auto_openai_collision".to_string();
    auto_thread.model = "auto".to_string();
    manager.store.save_thread(&auto_thread)?;
    let mut restored_turn = sample_turn(
        &auto_thread.id,
        "turn_openai_collision",
        RuntimeTurnStatus::Completed,
    );
    restored_turn.effective_provider = Some("openai".to_string());
    restored_turn.effective_provider_id = None;
    restored_turn.effective_model = Some("custom-openai-model".to_string());
    manager.store.save_turn(&restored_turn)?;
    // A restored built-in pick is not the thread's saved provider, so it is
    // ignored rather than captured: the thread keeps its exact custom route.
    let ignored = manager.resolved_route_for_thread(&config, &auto_thread)?;
    assert_eq!(ignored.identity.provider, ProviderKind::Custom);
    assert_eq!(ignored.identity.key.as_str(), "openai");
    assert_eq!(
        ignored.config.active_route_base_url(),
        "http://127.0.0.1:18183/v1"
    );

    restored_turn.effective_provider = Some("custom".to_string());
    restored_turn.effective_provider_id = Some("openai".to_string());
    manager.store.save_turn(&restored_turn)?;
    let restored_custom = manager.resolved_route_for_thread(&config, &auto_thread)?;
    assert_eq!(restored_custom.identity.provider, ProviderKind::Custom);
    assert_eq!(restored_custom.identity.key.as_str(), "openai");
    assert_eq!(restored_custom.model, "custom-openai-model");

    let request_error = manager
        .create_thread(CreateThreadRequest {
            model_provider: Some("openai".to_string()),
            model_provider_id: Some("openai".to_string()),
            ..CreateThreadRequest::default()
        })
        .await
        .expect_err("built-in request must fail closed under exact custom shadow")
        .to_string();
    assert!(
        request_error.contains("requires built-in 'openai'"),
        "{request_error}"
    );

    let created = manager
        .create_thread(CreateThreadRequest {
            model_provider: Some("custom".to_string()),
            model_provider_id: Some("openai".to_string()),
            ..CreateThreadRequest::default()
        })
        .await?;
    assert_eq!(created.model_provider.as_deref(), Some("custom"));
    assert_eq!(created.model_provider_id.as_deref(), Some("openai"));
    assert_eq!(created.model, "custom-openai-model");
    Ok(())
}

#[tokio::test]
async fn config_reload_updates_next_turn_route_without_mutating_engine_route() -> Result<()> {
    let mut custom = std::collections::HashMap::new();
    custom.insert(
        "lm-studio".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("http://127.0.0.1:18181/v1".to_string()),
            model: Some("local-model".to_string()),
            api_key: Some("old-local-test-key".to_string()),
            ..codewhale_runtime::config::ProviderConfig::default()
        },
    );
    let config = {
        let __cw_config_init_14_0 = Some("lm-studio".to_string());
        let __cw_config_init_14_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let mut __cw_config_init_14_value: Config = Config::default();
        __cw_config_init_14_value.provider = __cw_config_init_14_0;
        __cw_config_init_14_value.providers = __cw_config_init_14_1;
        __cw_config_init_14_value
    };
    let manager = RuntimeThreadManager::open(
        config.clone(),
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: Some("local-model".to_string()),
            model_provider: Some("lm-studio".to_string()),
            ..CreateThreadRequest::default()
        })
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;

    let mut reloaded = config;
    let provider = reloaded
        .providers
        .as_mut()
        .and_then(|providers| providers.custom.get_mut("lm-studio"))
        .expect("named custom provider");
    provider.base_url = Some("http://127.0.0.1:18182/v1".to_string());
    provider.api_key = Some("new-local-test-key".to_string());
    manager.reload_config(reloaded).await?;

    let refreshed = manager.resolved_route_for_thread(&manager.read_config(), &thread)?;
    assert_eq!(refreshed.identity.key.as_str(), "lm-studio");
    assert_eq!(
        refreshed.config.active_route_base_url(),
        "http://127.0.0.1:18182/v1"
    );
    for _ in 0..3 {
        let op = harness.rx_op.recv().await.expect("runtime control op");
        assert!(
            matches!(
                op,
                Op::SetCompaction { .. }
                    | Op::SetStreamChunkTimeout { .. }
                    | Op::SetSubagentRuntimeConfig { .. }
            ),
            "reload must not mutate an engine provider route: {op:?}"
        );
    }
    let compact_turn = manager
        .compact_thread(
            &thread.id,
            CompactThreadRequest {
                reason: Some("verify refreshed route".to_string()),
            },
        )
        .await?;
    assert_eq!(compact_turn.effective_provider.as_deref(), Some("custom"));
    assert_eq!(
        compact_turn.effective_provider_id.as_deref(),
        Some("lm-studio")
    );
    assert_eq!(compact_turn.effective_model.as_deref(), Some("local-model"));
    match harness.rx_op.recv().await {
        Some(Op::CompactContext {
            route, compaction, ..
        }) => {
            assert_eq!(route.identity.key.as_str(), "lm-studio");
            assert_eq!(
                route.config.active_route_base_url(),
                "http://127.0.0.1:18182/v1"
            );
            assert_eq!(compaction.model, "local-model");
            assert_eq!(
                compaction.effective_context_window,
                Some(crate::route_budget::route_context_window_tokens(
                    ProviderKind::Custom,
                    "local-model",
                    crate::route_budget::known_route_limits(route.candidate.limits()),
                ))
            );
        }
        other => panic!("expected typed compact route, got {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn queued_reload_is_a_hard_boundary_for_a_concurrent_turn_start() -> Result<()> {
    let provider = |base_url: &str, key: &str| codewhale_runtime::config::ProviderConfig {
        kind: Some("openai-compatible".to_string()),
        base_url: Some(base_url.to_string()),
        model: Some("local-model".to_string()),
        api_key: Some(key.to_string()),
        ..codewhale_runtime::config::ProviderConfig::default()
    };
    let config = {
        let __cw_config_init_15_0 = Some("lm-studio".to_string());
        let __cw_config_init_15_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom: std::collections::HashMap::from([(
                "lm-studio".to_string(),
                provider("http://127.0.0.1:18181/v1", "old-local-test-key"),
            )]),
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let mut __cw_config_init_15_value: Config = Config::default();
        __cw_config_init_15_value.provider = __cw_config_init_15_0;
        __cw_config_init_15_value.providers = __cw_config_init_15_1;
        __cw_config_init_15_value
    };
    let manager = RuntimeThreadManager::open(
        config.clone(),
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: Some("local-model".to_string()),
            model_provider: Some("lm-studio".to_string()),
            ..CreateThreadRequest::default()
        })
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;

    // Hold one existing admission reader while the reload writer queues. Tokio
    // RwLock is write-preferring, so the later turn reader cannot overtake it.
    let prior_admission = manager.config_admission.read().await;
    let mut reloaded = config;
    reloaded
        .providers
        .as_mut()
        .and_then(|providers| providers.custom.get_mut("lm-studio"))
        .expect("named custom provider")
        .clone_from(&provider("http://127.0.0.1:18182/v1", "new-local-test-key"));
    let reload_manager = manager.clone();
    let mut reload = tokio::spawn(async move { reload_manager.reload_config(reloaded).await });
    tokio::task::yield_now().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut reload)
            .await
            .is_err(),
        "reload must be waiting for the prior admission reader"
    );

    let start_manager = manager.clone();
    let start_thread_id = thread.id.clone();
    let mut start = tokio::spawn(async move {
        start_manager
            .start_turn(
                &start_thread_id,
                StartTurnRequest {
                    prompt: "use the post-reload route".to_string(),
                    ..StartTurnRequest::default()
                },
            )
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut start)
            .await
            .is_err(),
        "a turn queued after the reload writer may not snapshot old config"
    );
    drop(prior_admission);
    reload.await.expect("reload task")?;
    start.await.expect("start task")?;

    loop {
        match harness.rx_op.recv().await {
            Some(Op::SendMessage(TurnSpec { route, .. })) => {
                assert_eq!(
                    route.config.active_route_base_url(),
                    "http://127.0.0.1:18182/v1"
                );
                break;
            }
            Some(
                Op::SetCompaction { .. }
                | Op::SetStreamChunkTimeout { .. }
                | Op::SetSubagentRuntimeConfig { .. },
            ) => {}
            other => panic!("expected post-reload SendMessage, got {other:?}"),
        }
    }
    Ok(())
}

#[tokio::test]
async fn queued_reload_is_a_hard_boundary_for_concurrent_compaction() -> Result<()> {
    let provider = |base_url: &str, key: &str| codewhale_runtime::config::ProviderConfig {
        kind: Some("openai-compatible".to_string()),
        base_url: Some(base_url.to_string()),
        model: Some("local-model".to_string()),
        api_key: Some(key.to_string()),
        ..codewhale_runtime::config::ProviderConfig::default()
    };
    let config = {
        let __cw_config_init_16_0 = Some("lm-studio".to_string());
        let __cw_config_init_16_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom: std::collections::HashMap::from([(
                "lm-studio".to_string(),
                provider("http://127.0.0.1:18181/v1", "old-local-test-key"),
            )]),
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let mut __cw_config_init_16_value: Config = Config::default();
        __cw_config_init_16_value.provider = __cw_config_init_16_0;
        __cw_config_init_16_value.providers = __cw_config_init_16_1;
        __cw_config_init_16_value
    };
    let manager = RuntimeThreadManager::open(
        config.clone(),
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: Some("local-model".to_string()),
            model_provider: Some("lm-studio".to_string()),
            ..CreateThreadRequest::default()
        })
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;

    let prior_admission = manager.config_admission.read().await;
    let mut reloaded = config;
    *reloaded
        .providers
        .as_mut()
        .and_then(|providers| providers.custom.get_mut("lm-studio"))
        .expect("named custom provider") =
        provider("http://127.0.0.1:18182/v1", "new-local-test-key");
    let reload_manager = manager.clone();
    let mut reload = tokio::spawn(async move { reload_manager.reload_config(reloaded).await });
    tokio::task::yield_now().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut reload)
            .await
            .is_err()
    );
    let compact_manager = manager.clone();
    let compact_thread_id = thread.id.clone();
    let mut compact = tokio::spawn(async move {
        compact_manager
            .compact_thread(
                &compact_thread_id,
                CompactThreadRequest {
                    reason: Some("post-reload compaction".to_string()),
                },
            )
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut compact)
            .await
            .is_err(),
        "compaction queued after the reload writer may not carry old config"
    );
    drop(prior_admission);
    reload.await.expect("reload task")?;
    compact.await.expect("compact task")?;

    loop {
        match harness.rx_op.recv().await {
            Some(Op::CompactContext { route, .. }) => {
                assert_eq!(
                    route.config.active_route_base_url(),
                    "http://127.0.0.1:18182/v1"
                );
                break;
            }
            Some(
                Op::SetCompaction { .. }
                | Op::SetStreamChunkTimeout { .. }
                | Op::SetSubagentRuntimeConfig { .. },
            ) => {}
            other => panic!("expected post-reload CompactContext, got {other:?}"),
        }
    }
    Ok(())
}

#[tokio::test]
async fn config_sync_reports_removed_named_custom_route_and_keeps_mailbox_clean() -> Result<()> {
    let mut custom = std::collections::HashMap::new();
    custom.insert(
        "lm-studio".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("http://127.0.0.1:18181/v1".to_string()),
            model: Some("local-model".to_string()),
            api_key: Some("local-test-key".to_string()),
            ..codewhale_runtime::config::ProviderConfig::default()
        },
    );
    let config = {
        let __cw_config_init_17_0 = Some("lm-studio".to_string());
        let __cw_config_init_17_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let mut __cw_config_init_17_value: Config = Config::default();
        __cw_config_init_17_value.provider = __cw_config_init_17_0;
        __cw_config_init_17_value.providers = __cw_config_init_17_1;
        __cw_config_init_17_value
    };
    let manager = RuntimeThreadManager::open(
        config,
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: Some("local-model".to_string()),
            model_provider: Some("lm-studio".to_string()),
            ..CreateThreadRequest::default()
        })
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;

    let err = manager
        .reload_config(Config::default())
        .await
        .expect_err("removed named custom route must fail config reload");

    let message = err.to_string();
    assert!(message.contains(&thread.id), "{message}");
    assert!(message.contains("lm-studio"), "{message}");
    assert!(harness.rx_op.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn create_thread_uses_requested_named_custom_provider_default_model() -> Result<()> {
    let mut custom = std::collections::HashMap::new();
    for (name, base_url, model) in [
        ("custom-a", "http://127.0.0.1:18181/v1", "model-a"),
        ("custom-b", "http://127.0.0.1:18182/v1", "model-b"),
    ] {
        custom.insert(
            name.to_string(),
            codewhale_runtime::config::ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some(base_url.to_string()),
                model: Some(model.to_string()),
                ..Default::default()
            },
        );
    }
    let config = {
        let __cw_config_init_18_0 = Some("custom-b".to_string());
        let __cw_config_init_18_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..Default::default()
        });
        let mut __cw_config_init_18_value: Config = Default::default();
        __cw_config_init_18_value.provider = __cw_config_init_18_0;
        __cw_config_init_18_value.providers = __cw_config_init_18_1;
        __cw_config_init_18_value
    };
    let manager = RuntimeThreadManager::open(
        config.clone(),
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;

    let thread = manager
        .create_thread(CreateThreadRequest {
            model_provider: Some("custom-a".to_string()),
            ..Default::default()
        })
        .await?;

    assert_eq!(thread.model_provider.as_deref(), Some("custom"));
    assert_eq!(thread.model_provider_id.as_deref(), Some("custom-a"));
    assert_eq!(thread.model, "model-a");
    let route = manager.resolved_route_for_thread(&config, &thread)?;
    assert_eq!(route.identity.key.as_str(), "custom-a");
    assert_eq!(
        route.config.active_route_base_url(),
        "http://127.0.0.1:18181/v1"
    );
    Ok(())
}

fn two_custom_route_config() -> Config {
    let mut custom = std::collections::HashMap::new();
    for (name, base_url, model) in [
        ("custom-a", "http://127.0.0.1:18181/v1", "model-a"),
        ("custom-b", "http://127.0.0.1:18182/v1", "model-b"),
    ] {
        custom.insert(
            name.to_string(),
            codewhale_runtime::config::ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some(base_url.to_string()),
                model: Some(model.to_string()),
                ..Default::default()
            },
        );
    }
    {
        let __cw_config_init_19_0 = Some("custom-a".to_string());
        let __cw_config_init_19_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..Default::default()
        });
        let mut __cw_config_init_19_value: Config = Default::default();
        __cw_config_init_19_value.provider = __cw_config_init_19_0;
        __cw_config_init_19_value.providers = __cw_config_init_19_1;
        __cw_config_init_19_value
    }
}

#[tokio::test]
async fn update_thread_switches_provider_and_keeps_the_loaded_engine() -> Result<()> {
    let config = two_custom_route_config();
    let manager = RuntimeThreadManager::open(
        config.clone(),
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model_provider: Some("custom-a".to_string()),
            ..Default::default()
        })
        .await?;
    assert_eq!(thread.model, "model-a");
    let mut harness = install_mock_engine(&manager, &thread.id).await;

    // Unknown and blank providers are refused and change nothing.
    for (kind, id) in [
        (Some("custom-z"), None),
        (Some(" "), None),
        (Some("custom"), Some("custom-z")),
    ] {
        let err = manager
            .update_thread(
                &thread.id,
                UpdateThreadRequest {
                    model_provider: kind.map(str::to_string),
                    model_provider_id: id.map(str::to_string),
                    ..UpdateThreadRequest::default()
                },
            )
            .await
            .expect_err("unresolvable provider must be refused");
        assert!(!err.to_string().is_empty());
    }
    let unchanged = manager.get_thread(&thread.id).await?;
    assert_eq!(unchanged.model_provider_id.as_deref(), Some("custom-a"));
    assert_eq!(unchanged.model, "model-a");

    // A route name switches like `/provider custom-b`: the thread takes that
    // provider's default model.
    let switched = manager
        .update_thread(
            &thread.id,
            UpdateThreadRequest {
                model_provider: Some("custom-b".to_string()),
                ..UpdateThreadRequest::default()
            },
        )
        .await?;
    assert_eq!(switched.model_provider.as_deref(), Some("custom"));
    assert_eq!(switched.model_provider_id.as_deref(), Some("custom-b"));
    assert_eq!(switched.model, "model-b");
    let route = manager.resolved_route_for_thread(&config, &switched)?;
    assert_eq!(route.identity.key.as_str(), "custom-b");

    // The same engine (and so the same conversation) serves the next turn,
    // now on the new route.
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "after switch".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(turn.effective_provider_id.as_deref(), Some("custom-b"));
    assert_eq!(turn.effective_model.as_deref(), Some("model-b"));
    match harness.rx_op.recv().await {
        Some(Op::SendMessage(TurnSpec { route, .. })) => {
            assert_eq!(route.identity.key.as_str(), "custom-b");
            assert_eq!(route.model, "model-b");
        }
        other => panic!("expected send on the switched route, got {other:?}"),
    }

    // The exact-id form with an explicit model, as JSON clients send it.
    let patch: UpdateThreadRequest = serde_json::from_value(serde_json::json!({
        "model_provider": "custom",
        "model_provider_id": "custom-a",
        "model": "model-a-large"
    }))?;
    let back = manager.update_thread(&thread.id, patch).await?;
    assert_eq!(back.model_provider_id.as_deref(), Some("custom-a"));
    assert_eq!(back.model, "model-a-large");
    Ok(())
}

#[tokio::test]
async fn start_turn_provider_override_routes_one_turn_only() -> Result<()> {
    let manager = RuntimeThreadManager::open(
        two_custom_route_config(),
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model_provider: Some("custom-a".to_string()),
            ..Default::default()
        })
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let request: StartTurnRequest = serde_json::from_value(serde_json::json!({
        "prompt": "one turn on B",
        "model_provider": "custom",
        "model_provider_id": "custom-b"
    }))?;
    let turn = manager.start_turn(&thread.id, request).await?;
    assert_eq!(turn.effective_provider.as_deref(), Some("custom"));
    assert_eq!(turn.effective_provider_id.as_deref(), Some("custom-b"));
    assert_eq!(turn.effective_model.as_deref(), Some("model-b"));
    match harness.rx_op.recv().await {
        Some(Op::SendMessage(TurnSpec { route, .. })) => {
            assert_eq!(route.identity.key.as_str(), "custom-b");
        }
        other => panic!("expected send on the override route, got {other:?}"),
    }
    let saved = manager.get_thread(&thread.id).await?;
    assert_eq!(saved.model_provider_id.as_deref(), Some("custom-a"));
    assert_eq!(saved.model, "model-a");

    let err = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "bad".to_string(),
                model_provider: Some("custom-z".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("unknown per-turn provider is refused");
    assert!(!err.to_string().is_empty());
    Ok(())
}

/// `two_custom_route_config` with `custom-a` removed, as a user would after
/// moving every thread to `custom-b`.
fn only_custom_b_config() -> Config {
    let mut config = two_custom_route_config();
    config.provider = Some("custom-b".to_string());
    if let Some(providers) = config.providers.as_mut() {
        providers.custom.remove("custom-a");
    }
    config
}

#[tokio::test]
async fn auto_thread_follows_a_provider_switch_even_after_the_old_provider_is_removed() -> Result<()>
{
    let config = two_custom_route_config();
    let manager = RuntimeThreadManager::open(
        config.clone(),
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: Some("auto".to_string()),
            model_provider: Some("custom-a".to_string()),
            ..Default::default()
        })
        .await?;
    // An earlier Auto turn picked a model on custom-a.
    let mut earlier = sample_turn(&thread.id, "turn_auto_on_a", RuntimeTurnStatus::Completed);
    earlier.effective_provider = Some("custom".to_string());
    earlier.effective_provider_id = Some("custom-a".to_string());
    earlier.effective_model = Some("model-a".to_string());
    manager.store.save_turn(&earlier)?;
    let before = manager.resolved_route_for_thread(&config, &thread)?;
    assert_eq!(before.identity.key.as_str(), "custom-a");
    assert_eq!(before.model, "model-a");

    let switched = manager
        .update_thread(
            &thread.id,
            UpdateThreadRequest {
                model_provider: Some("custom-b".to_string()),
                ..UpdateThreadRequest::default()
            },
        )
        .await?;
    assert_eq!(switched.model, "auto");
    let route = manager.resolved_route_for_thread(&config, &switched)?;
    assert_eq!(route.identity.key.as_str(), "custom-b");
    assert_eq!(route.model, "model-b");

    // The earlier pick names a provider that is gone; the thread still routes
    // through its saved provider instead of failing on history.
    let route = manager.resolved_route_for_thread(&only_custom_b_config(), &switched)?;
    assert_eq!(route.identity.key.as_str(), "custom-b");
    assert_eq!(route.model, "model-b");
    Ok(())
}

#[tokio::test]
async fn config_reload_accepts_removing_the_provider_an_idle_thread_switched_away_from()
-> Result<()> {
    let manager = RuntimeThreadManager::open(
        two_custom_route_config(),
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model_provider: Some("custom-a".to_string()),
            ..Default::default()
        })
        .await?;
    // The loaded engine still carries custom-a until its next turn.
    let _harness = install_mock_engine(&manager, &thread.id).await;
    manager
        .update_thread(
            &thread.id,
            UpdateThreadRequest {
                model_provider: Some("custom-b".to_string()),
                ..UpdateThreadRequest::default()
            },
        )
        .await?;
    manager.reload_config(only_custom_b_config()).await?;
    let saved = manager.get_thread(&thread.id).await?;
    let route = manager.resolved_route_for_thread(&manager.read_config(), &saved)?;
    assert_eq!(route.identity.key.as_str(), "custom-b");
    Ok(())
}

#[tokio::test]
async fn create_thread_scenario() -> Result<()> {
    // Scenario consolidation of: create_thread_uses_requested_non_current_builtin_default_model, create_thread_defaults_auto_approve_to_false
    // from create_thread_uses_requested_non_current_builtin_default_model
    {
        let config = {
            let __cw_config_init_20_0 = Some("openrouter".to_string());
            let __cw_config_init_20_1 = Some(DEFAULT_TEXT_MODEL.to_string());
            let mut __cw_config_init_20_value: Config = Default::default();
            __cw_config_init_20_value.provider = __cw_config_init_20_0;
            __cw_config_init_20_value.default_text_model = __cw_config_init_20_1;
            __cw_config_init_20_value
        };
        let manager = RuntimeThreadManager::open(
            config,
            PathBuf::from("."),
            test_manager_config(test_runtime_dir()),
        )?;

        let thread = manager
            .create_thread(CreateThreadRequest {
                model_provider: Some("zai".to_string()),
                ..Default::default()
            })
            .await?;

        assert_eq!(thread.model_provider.as_deref(), Some("zai"));
        assert_eq!(
            thread.model,
            codewhale_config::descriptors::defaults::DEFAULT_ZAI_MODEL
        );
    }
    // from create_thread_defaults_auto_approve_to_false
    {
        let manager = test_manager(test_runtime_dir())?;
        let thread = manager
            .create_thread(CreateThreadRequest {
                model: None,
                workspace: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                archived: false,
                system_prompt: None,
                task_id: None,
                ..Default::default()
            })
            .await?;

        assert!(!thread.auto_approve);
    }
    Ok(())
}

#[tokio::test]
async fn simultaneous_named_custom_auto_threads_keep_exact_routes() -> Result<()> {
    let mut custom = std::collections::HashMap::new();
    for (name, base_url, model) in [
        ("custom-a", "http://127.0.0.1:18181/v1", "model-a"),
        ("custom-b", "http://127.0.0.1:18182/v1", "model-b"),
    ] {
        custom.insert(
            name.to_string(),
            codewhale_runtime::config::ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some(base_url.to_string()),
                model: Some(model.to_string()),
                ..Default::default()
            },
        );
    }
    let manager = RuntimeThreadManager::open(
        {
            let __cw_config_init_21_0 = Some("custom-b".to_string());
            let __cw_config_init_21_1 = Some(codewhale_runtime::config::ProvidersConfig {
                custom,
                ..Default::default()
            });
            let mut __cw_config_init_21_value: Config = Default::default();
            __cw_config_init_21_value.provider = __cw_config_init_21_0;
            __cw_config_init_21_value.providers = __cw_config_init_21_1;
            __cw_config_init_21_value
        },
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    let thread_a = manager
        .create_thread(CreateThreadRequest {
            model: Some("auto".to_string()),
            model_provider: Some("custom-a".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_b = manager
        .create_thread(CreateThreadRequest {
            model: Some("auto".to_string()),
            model_provider: Some("custom-b".to_string()),
            ..Default::default()
        })
        .await?;
    let mut harness_a = install_mock_engine(&manager, &thread_a.id).await;
    let mut harness_b = install_mock_engine(&manager, &thread_b.id).await;

    let request_a = manager.start_turn(
        &thread_a.id,
        StartTurnRequest {
            prompt: "route A".to_string(),
            ..Default::default()
        },
    );
    let request_b = manager.start_turn(
        &thread_b.id,
        StartTurnRequest {
            prompt: "route B".to_string(),
            ..Default::default()
        },
    );
    let (turn_a, turn_b) = tokio::join!(request_a, request_b);
    let turn_a = turn_a?;
    let turn_b = turn_b?;

    assert_eq!(turn_a.effective_provider.as_deref(), Some("custom"));
    assert_eq!(turn_a.effective_provider_id.as_deref(), Some("custom-a"));
    assert_eq!(turn_a.effective_model.as_deref(), Some("model-a"));
    assert_eq!(turn_b.effective_provider.as_deref(), Some("custom"));
    assert_eq!(turn_b.effective_provider_id.as_deref(), Some("custom-b"));
    assert_eq!(turn_b.effective_model.as_deref(), Some("model-b"));
    match harness_a.rx_op.recv().await {
        Some(Op::SendMessage(TurnSpec { route, .. })) => {
            assert_eq!(route.identity.provider, ProviderKind::Custom);
            assert_eq!(route.identity.key.as_str(), "custom-a");
            assert_eq!(route.model, "model-a");
        }
        other => panic!("expected custom A send, got {other:?}"),
    }
    match harness_b.rx_op.recv().await {
        Some(Op::SendMessage(TurnSpec { route, .. })) => {
            assert_eq!(route.identity.provider, ProviderKind::Custom);
            assert_eq!(route.identity.key.as_str(), "custom-b");
            assert_eq!(route.model, "model-b");
        }
        other => panic!("expected custom B send, got {other:?}"),
    }
    Ok(())
}

#[test]
fn turn_record_preserves_openrouter_vendor_pin_after_reload() {
    let mut turn = sample_turn("thr_vendor", "turn_vendor", RuntimeTurnStatus::Completed);
    let route = crate::cost_status::EffectiveRouteEnvelope {
        provider: ProviderKind::Openrouter,
        provider_identity: "openrouter".to_string(),
        model: "qwen/qwen3.7-plus".to_string(),
        openrouter_vendor: Some("cerebras".to_string()),
        billing_surface: crate::pricing::billing_surface_for_route(
            ProviderKind::Openrouter,
            Some(ProviderKind::Openrouter.provider().default_base_url()),
        )
        .map(str::to_string),
        endpoint_fingerprint: None,
        provider_live_pricing: None,
        billing_mode: crate::cost_status::RouteBillingMode::Metered,
        dispatched_at: turn.created_at,
    };
    turn.persist_effective_route(&route);
    let mut serialized = serde_json::to_value(&turn).expect("serialize turn");
    assert_eq!(serialized["effective_openrouter_vendor"], "cerebras");
    let restored: TurnRecord = serde_json::from_value(serialized.clone()).expect("restore turn");
    assert_eq!(restored.effective_route_envelope(), Some(route.clone()));
    let audit = restored
        .effective_route_envelope()
        .expect("route")
        .audit(&Usage::default());
    assert_eq!(
        audit.unpriced_reason,
        Some(crate::pricing::UnpricedReason::RoutingDependentPrice)
    );

    serialized
        .as_object_mut()
        .expect("turn object")
        .remove("effective_openrouter_vendor");
    let legacy: TurnRecord = serde_json::from_value(serialized).expect("legacy turn");
    assert_eq!(
        legacy
            .effective_route_envelope()
            .expect("legacy route")
            .openrouter_vendor,
        None
    );
}

#[test]
fn turn_record_persists_billing_surface_without_raw_endpoint() {
    let mut turn = sample_turn("thr_surface", "turn_surface", RuntimeTurnStatus::Completed);
    let fingerprint = "a".repeat(64);
    turn.persist_effective_route(&crate::cost_status::EffectiveRouteEnvelope {
        openrouter_vendor: None,
        provider: ProviderKind::Stepfun,
        provider_identity: "stepfun-primary".to_string(),
        model: "step-3.7-flash".to_string(),
        billing_surface: Some(crate::pricing::STEPFUN_PAYG_BILLING_SURFACE.to_string()),
        endpoint_fingerprint: Some(fingerprint.clone()),
        provider_live_pricing: None,
        billing_mode: crate::cost_status::RouteBillingMode::Metered,
        dispatched_at: turn.created_at,
    });

    let value = serde_json::to_value(turn).expect("serialize turn");
    assert_eq!(
        value["effective_billing_surface"],
        crate::pricing::STEPFUN_PAYG_BILLING_SURFACE
    );
    assert_eq!(value["effective_billing_mode"], "metered");
    assert_eq!(value["effective_endpoint_fingerprint"], fingerprint);
    assert!(value["effective_dispatched_at"].is_string());
    assert!(value.get("base_url").is_none());
    assert!(value.get("effective_base_url").is_none());
}

#[test]
fn turn_record_round_trips_frozen_provider_live_pricing_and_drops_hostile_quotes() {
    struct ProviderCatalogReset;
    impl Drop for ProviderCatalogReset {
        fn drop(&mut self) {
            crate::provider_catalog_live::reset_cache_for_test();
            crate::provider_lake::clear_live_snapshot();
        }
    }

    let _env = crate::test_support::lock_test_env();
    let _live = crate::provider_lake::lock_live_snapshot();
    let home = tempfile::tempdir().expect("test home");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let _reset = ProviderCatalogReset;
    crate::provider_catalog_live::reset_cache_for_test();
    crate::provider_lake::clear_live_snapshot();

    let dispatched_at = Utc::now();
    let fetched_at = u64::try_from(dispatched_at.timestamp()).expect("timestamp");
    let model = "synthetic-baseten-turn-record";
    let fingerprint = codewhale_config::catalog::base_url_fingerprint(
        codewhale_config::catalog::BASETEN_BASE_URL,
    );
    let priced_delta = |input: f64, output: f64| codewhale_config::catalog::ProviderCatalogDelta {
        provider: codewhale_config::catalog::BASETEN_PROVIDER_ID.to_string(),
        base_url_fingerprint: fingerprint.clone(),
        fetched_at,
        offerings: vec![codewhale_config::catalog::CatalogOffering {
            provider: codewhale_config::catalog::BASETEN_PROVIDER_ID.to_string(),
            wire_model_id: model.to_string(),
            endpoint_key: "chat".to_string(),
            cost: Some(codewhale_config::models_dev::ModelsDevCost {
                input: Some(input),
                output: Some(output),
                cache_read: Some(0.25),
                cache_write: None,
                ..Default::default()
            }),
            ..Default::default()
        }],
    };
    crate::provider_catalog_live::record_success(priced_delta(1.25, 5.0));
    let route = crate::cost_status::EffectiveRouteEnvelope::capture(
        None,
        ProviderKind::Custom,
        codewhale_config::catalog::BASETEN_PROVIDER_ID,
        model,
        Some(codewhale_config::catalog::BASETEN_BASE_URL),
        dispatched_at,
    );
    let valid_quote = route
        .provider_live_pricing
        .clone()
        .expect("fresh Baseten scope freezes quote");

    let mut turn = sample_turn("thr_quote", "turn_quote", RuntimeTurnStatus::Completed);
    turn.persist_effective_route(&route);
    let serialized = serde_json::to_string(&turn).expect("serialize quoted turn");
    for raw_secret in [
        codewhale_config::catalog::BASETEN_BASE_URL,
        "api_key",
        "Bearer ",
    ] {
        // The assertion message must not itself log the credential fragment it
        // checks for — name the check, not the secret.
        assert!(
            !serialized.contains(raw_secret),
            "persisted turn serialization leaked a credential fragment"
        );
    }
    let restored: TurnRecord = serde_json::from_str(&serialized).expect("restore quoted turn");
    let restored_route = restored
        .effective_route_envelope()
        .expect("complete persisted route");
    assert_eq!(
        restored_route.provider_live_pricing,
        Some(valid_quote.clone())
    );

    crate::provider_catalog_live::record_success(priced_delta(19.0, 29.0));
    let usage = Usage {
        input_tokens: 1_000_000,
        ..Usage::default()
    };
    assert_eq!(
        restored_route
            .audit(&usage)
            .estimate
            .expect("frozen price survives refresh")
            .usd,
        1.25
    );

    // Model the strongest persisted attack: an adversary recomputes the
    // unkeyed integrity revision after placing credential text in the quote.
    // Structural validation, not the digest alone, must reject it.
    let secret = "Authorization: Bearer persisted-secret";
    let mut hostile_quote = valid_quote;
    hostile_quote.wire_model = secret.to_string();
    let revision_payload = serde_json::to_vec(&(
        "codewhale-provider-live-pricing-quote-v1",
        hostile_quote.provider,
        &hostile_quote.provider_identity,
        &hostile_quote.wire_model,
        &hostile_quote.endpoint_fingerprint,
        hostile_quote.catalog_fetched_at,
        &hostile_quote.currency,
        &hostile_quote.provenance,
        &hostile_quote.input_per_million,
        &hostile_quote.output_per_million,
        &hostile_quote.cache_read_per_million,
        &hostile_quote.cache_write_per_million,
    ))
    .expect("revision payload");
    hostile_quote.catalog_revision = format!(
        "sha256:{}",
        codewhale_runtime::hashing::sha256_hex(revision_payload)
    );

    let mut hostile_quote_value = serde_json::to_value(
        restored_route
            .provider_live_pricing
            .as_ref()
            .expect("valid persisted quote"),
    )
    .expect("quote JSON");
    hostile_quote_value["wire_model"] = serde_json::json!(secret);
    hostile_quote_value["catalog_revision"] = serde_json::json!(hostile_quote.catalog_revision);

    let mut hostile_turn_value = serde_json::to_value(&restored).expect("turn JSON");
    hostile_turn_value["effective_provider_live_pricing"] = hostile_quote_value.clone();
    let mut hostile_child_route = serde_json::to_value(&restored_route).expect("route JSON");
    hostile_child_route["provider_live_pricing"] = hostile_quote_value;
    hostile_turn_value["routed_usage"] = serde_json::json!([{
        "route": hostile_child_route,
        "usage": Usage::default(),
    }]);

    let sanitized: TurnRecord =
        serde_json::from_value(hostile_turn_value).expect("hostile optional quotes fail closed");
    assert!(sanitized.effective_provider_live_pricing.is_none());
    assert_eq!(sanitized.routed_usage.len(), 1);
    assert!(
        sanitized.routed_usage[0]
            .route
            .provider_live_pricing
            .is_none()
    );
    let sanitized_json = serde_json::to_string(&sanitized).expect("reserialize sanitized turn");
    assert!(!sanitized_json.contains("persisted-secret"));
    assert_eq!(
        sanitized
            .effective_route_envelope()
            .expect("route remains readable")
            .audit(&usage)
            .unpriced_reason,
        Some(crate::pricing::UnpricedReason::UnverifiedLivePricing)
    );
}

#[test]
fn serialized_turn_record_redacts_all_route_and_source_fields() {
    let mut value = serde_json::to_value(sample_turn(
        "thr_secret_route",
        "turn_secret_route",
        RuntimeTurnStatus::Completed,
    ))
    .expect("serialize clean fixture");
    value["effective_provider"] = serde_json::json!("Authorization: Bearer provider-secret");
    value["effective_provider_id"] = serde_json::json!("CUSTOM_API_KEY=sk-provider-secret");
    value["effective_openrouter_vendor"] = serde_json::json!("Authorization: Bearer vendor-secret");
    value["effective_model"] = serde_json::json!("../.ssh/model-secret");
    value["effective_billing_surface"] =
        serde_json::json!("https://alice:password@example.test/v1?token=secret#fragment");
    value["effective_endpoint_fingerprint"] = serde_json::json!("secret-endpoint");
    value["routed_usage_source_ids"] = serde_json::json!(["raw-provider-response-secret"]);
    value["routed_usage"] = serde_json::json!([{
        "route": {
            "provider": "custom",
            "provider_identity": "TOKEN=ghp_child_secret",
            "model": r"relative\secret\model",
            "billing_surface": "https://host.test/v1?key=child-secret",
            "endpoint_fingerprint": "child-endpoint-secret",
            "billing_mode": "metered",
            "dispatched_at": Utc::now(),
        },
        "usage": Usage::default(),
    }]);
    let turn: TurnRecord = serde_json::from_value(value).expect("deserialize hostile fixture");

    let serialized = serde_json::to_string(&turn).expect("serialize turn record");
    for secret in [
        "vendor-secret",
        "provider-secret",
        "sk-provider-secret",
        ".ssh",
        "alice",
        "password",
        "token=secret",
        "raw-provider-response-secret",
        "ghp_child_secret",
        "child-secret",
        "child-endpoint-secret",
    ] {
        assert!(
            !serialized.contains(secret),
            "serialized turn leaked {secret:?}: {serialized}"
        );
    }
}

#[tokio::test]
async fn aggregate_usage_keeps_codex_tokens_without_api_dollar_pricing() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let mut thread = sample_thread("thr_mixed_routes");
    thread.model = "auto".to_string();
    manager.store.save_thread(&thread)?;

    let usage = Usage {
        input_tokens: 10_000,
        output_tokens: 1_000,
        ..Usage::default()
    };
    let mut deepseek = sample_turn(&thread.id, "turn_deepseek", RuntimeTurnStatus::Completed);
    deepseek.usage = Some(usage.clone());
    set_test_turn_route(
        &mut deepseek,
        ProviderKind::Deepseek,
        ProviderKind::Deepseek.as_str(),
        "deepseek-v4-flash",
        // A real dispatch persists the classified surface. Leaving it absent
        // here would price the row at official DeepSeek rates on nothing but
        // the provider name, which is what the route audit now refuses.
        Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
        crate::cost_status::RouteBillingMode::Metered,
    );
    manager.store.save_turn(&deepseek)?;

    let mut codex = sample_turn(&thread.id, "turn_codex", RuntimeTurnStatus::Completed);
    codex.usage = Some(usage);
    set_test_turn_route(
        &mut codex,
        ProviderKind::OpenaiCodex,
        ProviderKind::OpenaiCodex.as_str(),
        "gpt-5.5",
        Some(crate::pricing::OAUTH_SUBSCRIPTION_BILLING_SURFACE),
        crate::cost_status::RouteBillingMode::Subscription,
    );
    manager.store.save_turn(&codex)?;

    let report = manager
        .aggregate_usage(None, None, UsageGroupBy::Provider)
        .await?;
    assert_eq!(report.totals.turns, 2);
    assert_eq!(report.totals.input_tokens, 20_000);
    assert!(report.totals.cost_usd > 0.0);

    let deepseek_bucket = report
        .buckets
        .iter()
        .find(|bucket| bucket.key == ProviderKind::Deepseek.as_str())
        .expect("DeepSeek bucket");
    let codex_bucket = report
        .buckets
        .iter()
        .find(|bucket| bucket.key == ProviderKind::OpenaiCodex.as_str())
        .expect("Codex bucket");
    assert!(deepseek_bucket.cost_usd > 0.0);
    assert_eq!(codex_bucket.cost_usd, 0.0);
    assert_eq!(deepseek_bucket.priced_turns, 1);
    assert_eq!(deepseek_bucket.unpriced_turns, 0);
    assert_eq!(codex_bucket.priced_turns, 0);
    assert_eq!(codex_bucket.nonmetered_turns, 1);
    assert_eq!(codex_bucket.unpriced_turns, 0);
    assert!(codex_bucket.unpriced_reasons.is_empty());
    assert_eq!(report.totals.priced_turns, 1);
    assert_eq!(report.totals.nonmetered_turns, 1);
    assert_eq!(report.totals.unpriced_turns, 0);
    assert!(report.totals.cost_complete);
    assert_eq!(codex_bucket.input_tokens, 10_000);
    assert_eq!(report.totals.cost_usd, deepseek_bucket.cost_usd);
    Ok(())
}

/// `aggregate_usage_for_thread` scopes the same recorded-time pricing the
/// global `/v1/usage` aggregate applies to exactly one thread, in both
/// published currencies: DeepSeek's first-party rows carry native CNY, so a
/// deepseek-priced turn must produce a non-zero `cost_cny` there, while a
/// different thread's spend stays out of the figure. The parent/child split
/// keeps routed-child (sub-agent) spend out of the parent figure — the split
/// session persistence writes into `session_cost_*` vs `subagent_cost_*` —
/// and CNY coverage counts turns under the provider's own CNY row.
#[tokio::test]
async fn aggregate_usage_for_thread_scopes_both_currencies_to_one_thread() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;

    let billed = sample_thread("thr_thread_usage_billed");
    manager.store.save_thread(&billed)?;
    let mut turn = sample_turn(&billed.id, "turn_billed", RuntimeTurnStatus::Completed);
    turn.usage = Some(Usage {
        input_tokens: 10_000,
        output_tokens: 1_000,
        ..Usage::default()
    });
    set_test_turn_route(
        &mut turn,
        ProviderKind::Deepseek,
        ProviderKind::Deepseek.as_str(),
        "deepseek-v4-flash",
        Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
        crate::cost_status::RouteBillingMode::Metered,
    );
    turn.routed_usage = vec![crate::cost_status::EffectiveRouteUsage {
        route: crate::cost_status::EffectiveRouteEnvelope {
            openrouter_vendor: None,
            provider: ProviderKind::Deepseek,
            provider_identity: ProviderKind::Deepseek.as_str().to_string(),
            model: "deepseek-v4-flash".to_string(),
            billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE.to_string()),
            endpoint_fingerprint: None,
            provider_live_pricing: None,
            billing_mode: crate::cost_status::RouteBillingMode::Metered,
            dispatched_at: turn.created_at,
        },
        usage: Usage {
            input_tokens: 2_000,
            output_tokens: 200,
            ..Usage::default()
        },
    }];
    manager.store.save_turn(&turn)?;

    // A second thread with its own priced turn must not leak into the
    // first thread's figure.
    let other = sample_thread("thr_thread_usage_other");
    manager.store.save_thread(&other)?;
    let mut other_turn = sample_turn(&other.id, "turn_other", RuntimeTurnStatus::Completed);
    other_turn.usage = Some(Usage {
        input_tokens: 500_000,
        output_tokens: 500_000,
        ..Usage::default()
    });
    set_test_turn_route(
        &mut other_turn,
        ProviderKind::Deepseek,
        ProviderKind::Deepseek.as_str(),
        "deepseek-v4-flash",
        Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
        crate::cost_status::RouteBillingMode::Metered,
    );
    manager.store.save_turn(&other_turn)?;

    let split = manager.aggregate_usage_for_thread(&billed.id).await?;
    // The split mirrors the persisted session fields: parent spend in
    // `parent`, routed-child (sub-agent) spend in `routed_children`.
    assert_eq!(split.parent.input_tokens, 10_000);
    assert_eq!(split.parent.output_tokens, 1_000);
    assert_eq!(split.parent.turns, 1);
    assert!(split.parent.cost_usd > 0.0);
    assert!(split.parent.cost_cny > 0.0);
    assert_eq!(split.routed_children.input_tokens, 2_000);
    assert_eq!(split.routed_children.turns, 1);
    assert!(split.routed_children.cost_usd > 0.0);
    assert!(split.routed_children.cost_cny > 0.0);

    // Native CNY row: priced in CNY without any FX projection of the USD
    // column, and the CNY coverage counters see the same money-metered
    // turns the USD counters do.
    assert_eq!(split.parent.priced_turns, 1);
    assert_eq!(split.parent.unpriced_turns, 0);
    assert_eq!(split.parent.cny_priced_turns, 1);
    assert_eq!(split.parent.cny_unpriced_turns, 0);
    assert!(split.parent.cny_unpriced_reasons.is_empty());
    assert_eq!(split.routed_children.cny_priced_turns, 1);
    assert!(split.routed_children.cny_unpriced_reasons.is_empty());

    let totals = split.combined();
    assert_eq!(totals.input_tokens, 12_000);
    assert_eq!(totals.turns, 2);
    assert_eq!(
        totals.cost_usd,
        split.parent.cost_usd + split.routed_children.cost_usd
    );
    assert!(totals.cost_cny > 0.0);
    assert_eq!(totals.priced_turns, 2);
    assert_eq!(totals.unpriced_turns, 0);
    assert_eq!(totals.cny_priced_turns, 2);
    assert!(totals.cost_complete);

    // The scoped figure agrees with the thread bucket of the global
    // aggregate, so the GUI (per-thread endpoint) and `/v1/usage` can never
    // disagree about the same thread.
    let global = manager
        .aggregate_usage(None, None, UsageGroupBy::Thread)
        .await?;
    let bucket = global
        .buckets
        .iter()
        .find(|bucket| bucket.key == billed.id)
        .expect("billed thread bucket");
    assert_eq!(bucket.cost_usd, totals.cost_usd);
    assert_eq!(bucket.cost_cny, totals.cost_cny);
    assert_eq!(bucket.cny_priced_turns, totals.cny_priced_turns);

    let missing = manager
        .aggregate_usage_for_thread("thr_does_not_exist")
        .await;
    assert!(missing.is_err());
    Ok(())
}

/// An aggregate in which nothing could be priced is unavailable, not zero.
///
/// `cost_usd` is a `f64` and an all-unknown run leaves it at `0.0`, so the
/// only thing standing between a reader and a fabricated "$0.00 spent" is that
/// the aggregate also reports zero priced turns and incomplete coverage. Those
/// qualifiers are the contract; this pins them at the aggregate level, which is
/// where per-turn honesty is most easily lost by summing.
#[tokio::test]
async fn aggregate_usage_reports_an_all_unknown_run_as_unavailable_not_zero() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = sample_thread("thr_all_unknown");
    manager.store.save_thread(&thread)?;
    let usage = Usage {
        input_tokens: 10_000,
        output_tokens: 1_000,
        ..Usage::default()
    };

    for (index, surface) in [
        // Endpoint classified but unplaceable.
        Some(crate::pricing::UNCLASSIFIED_BILLING_SURFACE),
        // No endpoint classification at all: a provider name is not evidence
        // that the official endpoint served the turn.
        None,
    ]
    .into_iter()
    .enumerate()
    {
        let mut turn = sample_turn(
            &thread.id,
            &format!("turn_unknown_{index}"),
            RuntimeTurnStatus::Completed,
        );
        turn.usage = Some(usage.clone());
        set_test_turn_route(
            &mut turn,
            ProviderKind::Openai,
            ProviderKind::Openai.as_str(),
            "gpt-5.5",
            surface,
            crate::cost_status::RouteBillingMode::Metered,
        );
        manager.store.save_turn(&turn)?;
    }

    let report = manager
        .aggregate_usage(None, None, UsageGroupBy::Thread)
        .await?;

    assert_eq!(report.totals.turns, 2);
    // Tokens are known and are reported. Only the money is unknown; an unknown
    // price must not suppress usage that the provider did report.
    assert_eq!(report.totals.input_tokens, 20_000);
    assert_eq!(report.totals.priced_turns, 0);
    assert_eq!(report.totals.unpriced_turns, 2);
    assert_eq!(report.totals.nonmetered_turns, 0);
    assert!(
        !report.totals.cost_complete,
        "an all-unknown run must never be a complete total"
    );
    assert_eq!(
        report.totals.cost_usd, 0.0,
        "no spend may be invented for an unpriced run"
    );
    assert!(
        !report.totals.unpriced_reasons.is_empty(),
        "an unpriced aggregate must say why"
    );
    assert_eq!(report.totals.cny_unpriced_turns, 2);
    assert!(
        !report.totals.cny_unpriced_reasons.is_empty(),
        "CNY coverage must travel with the money (#4318)"
    );
    for bucket in &report.buckets {
        assert_eq!(bucket.priced_turns, 0);
        assert!(
            bucket.unpriced_turns > 0,
            "a bucket cannot report a complete zero either"
        );
    }
    Ok(())
}

#[tokio::test]
async fn aggregate_usage_marks_unknown_cost_as_subtotal_and_keeps_cache_writes() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = sample_thread("thr_cost_coverage");
    manager.store.save_thread(&thread)?;
    let usage = Usage {
        input_tokens: 10_000,
        output_tokens: 1_000,
        prompt_cache_write_tokens: Some(321),
        ..Usage::default()
    };

    let mut priced = sample_turn(&thread.id, "turn_priced", RuntimeTurnStatus::Completed);
    priced.usage = Some(usage.clone());
    set_test_turn_route(
        &mut priced,
        ProviderKind::Deepseek,
        ProviderKind::Deepseek.as_str(),
        "deepseek-v4-flash",
        Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
        crate::cost_status::RouteBillingMode::Metered,
    );
    manager.store.save_turn(&priced)?;

    let mut unknown = sample_turn(&thread.id, "turn_unknown", RuntimeTurnStatus::Completed);
    unknown.usage = Some(usage);
    set_test_turn_route(
        &mut unknown,
        ProviderKind::Openai,
        ProviderKind::Openai.as_str(),
        "gpt-5.5",
        Some(crate::pricing::UNCLASSIFIED_BILLING_SURFACE),
        crate::cost_status::RouteBillingMode::Unknown,
    );
    manager.store.save_turn(&unknown)?;

    let report = manager
        .aggregate_usage(None, None, UsageGroupBy::Thread)
        .await?;
    assert!(report.totals.cost_usd > 0.0, "priced subtotal is retained");
    assert_eq!(report.totals.priced_turns, 1);
    assert_eq!(report.totals.unpriced_turns, 1);
    assert!(!report.totals.cost_complete);
    assert_eq!(report.totals.nonmetered_turns, 0);
    assert_eq!(report.totals.cache_write_tokens, 642);
    assert!(
        report
            .totals
            .unpriced_reasons
            .contains("unknown_billing_basis")
    );
    Ok(())
}

#[tokio::test]
async fn aggregate_usage_prices_slow_predispatch_turn_at_dispatch_boundary() -> Result<()> {
    // The pricing tier is chosen at the turn's *dispatch* time, not its
    // creation time. Sonnet 5's intro window used to exercise this; that
    // increase was cancelled upstream (the $2/$10 rate is now standard), so
    // the DeepSeek V4 Flash off-peak -> peak boundary at 01:00 UTC is the
    // recorded-time rate flip this test now rides: 1M cache-miss input costs
    // $0.22 off-peak and $0.44 in the peak window
    // (https://api-docs.deepseek.com/quick_start/pricing, 2026-08-17).
    let manager = test_manager(test_runtime_dir())?;
    let mut thread = sample_thread("thr_historical_pricing");
    thread.model = "deepseek-v4-flash".to_string();
    manager.store.save_thread(&thread)?;

    let usage = Usage {
        input_tokens: 1_000_000,
        output_tokens: 0,
        ..Usage::default()
    };
    for (turn_id, created_at, dispatched_at) in [
        (
            "turn_off_peak",
            "2026-08-17T00:59:58Z",
            "2026-08-17T00:59:59Z",
        ),
        (
            "turn_slow_snapshot",
            "2026-08-17T00:59:59Z",
            "2026-08-17T01:00:00Z",
        ),
    ] {
        let mut turn = sample_turn(&thread.id, turn_id, RuntimeTurnStatus::Completed);
        turn.created_at = created_at.parse().expect("recorded turn time");
        turn.started_at = Some(turn.created_at);
        turn.usage = Some(usage.clone());
        set_test_turn_route(
            &mut turn,
            ProviderKind::Deepseek,
            ProviderKind::Deepseek.as_str(),
            "deepseek-v4-flash",
            Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
            crate::cost_status::RouteBillingMode::Metered,
        );
        turn.effective_dispatched_at = Some(dispatched_at.parse().expect("dispatch time"));
        manager.store.save_turn(&turn)?;
    }

    let report = manager
        .aggregate_usage(None, None, UsageGroupBy::Model)
        .await?;

    assert_eq!(report.totals.turns, 2);
    // 0.22 (off-peak) + 0.44 (peak, priced at the dispatch minute).
    assert!(
        (report.totals.cost_usd - 0.66).abs() < 1e-9,
        "{}",
        report.totals.cost_usd
    );
    assert_eq!(report.buckets.len(), 1);
    assert!((report.buckets[0].cost_usd - 0.66).abs() < 1e-9);
    Ok(())
}

#[tokio::test]
async fn aggregate_usage_prices_only_stepfun_payg_surface() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let mut thread = sample_thread("thr_stepfun_surfaces");
    thread.model = "step-3.7-flash".to_string();
    manager.store.save_thread(&thread)?;

    let usage = Usage {
        input_tokens: 1_000_000,
        output_tokens: 500_000,
        prompt_cache_hit_tokens: Some(250_000),
        ..Usage::default()
    };
    for (turn_id, surface) in [
        (
            "turn_stepfun_payg",
            crate::pricing::STEPFUN_PAYG_BILLING_SURFACE,
        ),
        (
            "turn_stepfun_plan",
            crate::pricing::STEPFUN_PLAN_BILLING_SURFACE,
        ),
    ] {
        let mut turn = sample_turn(&thread.id, turn_id, RuntimeTurnStatus::Completed);
        turn.usage = Some(usage.clone());
        set_test_turn_route(
            &mut turn,
            ProviderKind::Stepfun,
            ProviderKind::Stepfun.as_str(),
            "step-3.7-flash",
            Some(surface),
            if surface == crate::pricing::STEPFUN_PLAN_BILLING_SURFACE {
                crate::cost_status::RouteBillingMode::Subscription
            } else {
                crate::cost_status::RouteBillingMode::Metered
            },
        );
        manager.store.save_turn(&turn)?;
    }

    let report = manager
        .aggregate_usage(None, None, UsageGroupBy::Provider)
        .await?;
    assert_eq!(report.totals.turns, 2);
    assert!((report.totals.cost_usd - 0.735).abs() < 1e-12);
    Ok(())
}

#[tokio::test]
async fn aggregate_usage_includes_exclusive_child_calls_and_zero_usage_receipts() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = sample_thread("thr_child_usage");
    manager.store.save_thread(&thread)?;

    let mut turn = sample_turn(&thread.id, "turn_parent", RuntimeTurnStatus::Completed);
    turn.usage = Some(Usage {
        input_tokens: 100,
        output_tokens: 20,
        prompt_cache_hit_tokens: Some(40),
        prompt_cache_miss_tokens: Some(50),
        prompt_cache_write_tokens: Some(10),
        reasoning_tokens: Some(5),
        ..Usage::default()
    });
    set_test_turn_route(
        &mut turn,
        ProviderKind::Deepseek,
        "deepseek",
        "deepseek-v4-flash",
        Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
        crate::cost_status::RouteBillingMode::Metered,
    );
    turn.routed_usage
        .push(crate::cost_status::EffectiveRouteUsage {
            route: crate::cost_status::EffectiveRouteEnvelope {
                openrouter_vendor: None,
                provider: ProviderKind::Deepseek,
                // A distinct actual model keeps the child's route receipt
                // separate without treating its role as a provider identity.
                provider_identity: "deepseek".to_string(),
                model: "deepseek-v4-pro".to_string(),
                // A child call carries the surface its own dispatch classified;
                // the route audit will not price a metered route without one.
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE.to_string()),
                endpoint_fingerprint: None,
                provider_live_pricing: None,
                billing_mode: crate::cost_status::RouteBillingMode::Metered,
                dispatched_at: turn.created_at,
            },
            usage: Usage {
                input_tokens: 30,
                output_tokens: 7,
                prompt_cache_hit_tokens: Some(10),
                prompt_cache_miss_tokens: Some(20),
                reasoning_tokens: Some(6),
                ..Usage::default()
            },
        });
    turn.routed_usage
        .push(crate::cost_status::EffectiveRouteUsage {
            route: crate::cost_status::EffectiveRouteEnvelope {
                openrouter_vendor: None,
                provider: ProviderKind::OpenaiCodex,
                provider_identity: "openai-codex".to_string(),
                model: "gpt-5.5".to_string(),
                billing_surface: Some(
                    crate::pricing::OAUTH_SUBSCRIPTION_BILLING_SURFACE.to_string(),
                ),
                endpoint_fingerprint: None,
                provider_live_pricing: None,
                billing_mode: crate::cost_status::RouteBillingMode::Subscription,
                dispatched_at: turn.created_at,
            },
            usage: Usage::default(),
        });
    manager.store.save_turn(&turn)?;

    let report = manager
        .aggregate_usage(None, None, UsageGroupBy::Thread)
        .await?;
    assert_eq!(report.totals.turns, 3, "parent plus two child calls");
    assert_eq!(report.totals.input_tokens, 70);
    assert_eq!(report.totals.cached_tokens, 50);
    assert_eq!(report.totals.cache_write_tokens, 10);
    assert_eq!(report.totals.output_tokens, 27);
    assert_eq!(report.totals.reasoning_tokens, 11);
    assert_eq!(report.totals.priced_turns, 2);
    assert_eq!(report.totals.nonmetered_turns, 1);
    assert_eq!(report.totals.unpriced_turns, 0);
    assert!(report.totals.cost_complete);
    assert_eq!(report.totals.route_receipts.len(), 3);
    Ok(())
}

#[tokio::test]
async fn aggregate_usage_filters_each_call_by_its_dispatch_timestamp() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = sample_thread("thr_dispatch_window");
    manager.store.save_thread(&thread)?;
    let window = Utc::now();
    let mut turn = sample_turn(
        &thread.id,
        "turn_dispatch_window",
        RuntimeTurnStatus::Completed,
    );
    turn.created_at = window - chrono::Duration::days(2);
    turn.usage = Some(Usage {
        input_tokens: 100,
        ..Usage::default()
    });
    set_test_turn_route(
        &mut turn,
        ProviderKind::Deepseek,
        "deepseek",
        "deepseek-v4-flash",
        Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
        crate::cost_status::RouteBillingMode::Metered,
    );
    turn.routed_usage
        .push(crate::cost_status::EffectiveRouteUsage {
            route: crate::cost_status::EffectiveRouteEnvelope {
                openrouter_vendor: None,
                provider: ProviderKind::Deepseek,
                provider_identity: "deepseek".to_string(),
                model: "deepseek-v4-flash".to_string(),
                billing_surface: None,
                endpoint_fingerprint: None,
                provider_live_pricing: None,
                billing_mode: crate::cost_status::RouteBillingMode::Metered,
                dispatched_at: window,
            },
            usage: Usage {
                input_tokens: 33,
                ..Usage::default()
            },
        });
    manager.store.save_turn(&turn)?;

    let report = manager
        .aggregate_usage(
            Some(window - chrono::Duration::minutes(1)),
            Some(window + chrono::Duration::minutes(1)),
            UsageGroupBy::Thread,
        )
        .await?;
    assert_eq!(report.totals.turns, 1);
    assert_eq!(report.totals.input_tokens, 33);
    Ok(())
}

#[tokio::test]
async fn aggregate_usage_marks_bounded_journal_truncation_incomplete() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = sample_thread("thr_truncated_usage");
    manager.store.save_thread(&thread)?;
    let mut turn = sample_turn(
        &thread.id,
        "turn_truncated_usage",
        RuntimeTurnStatus::Completed,
    );
    turn.routed_usage_dropped_records = 2;
    manager.store.save_turn(&turn)?;

    let report = manager
        .aggregate_usage(None, None, UsageGroupBy::Thread)
        .await?;
    assert_eq!(report.totals.dropped_usage_records, 2);
    assert_eq!(report.totals.unpriced_turns, 2);
    assert_eq!(report.totals.cny_unpriced_turns, 2);
    assert_eq!(report.totals.turns, 2);
    assert!(!report.totals.cost_complete);
    assert!(
        report
            .totals
            .unpriced_reasons
            .contains("runtime_usage_journal_truncated")
    );
    assert!(
        report
            .totals
            .cny_unpriced_reasons
            .contains("runtime_usage_journal_truncated")
    );
    let bucket = report.buckets.first().expect("thread usage bucket");
    assert_eq!(bucket.dropped_usage_records, 2);
    assert_eq!(bucket.unpriced_turns, 2);
    assert_eq!(bucket.cny_unpriced_turns, 2);
    assert!(
        bucket
            .cny_unpriced_reasons
            .contains("runtime_usage_journal_truncated")
    );
    Ok(())
}

#[test]
fn runtime_usage_sink_survives_parent_terminal_and_restart_exactly_once() -> Result<()> {
    let _cost_scope = crate::cost_status::test_scope();
    let data_dir = test_runtime_dir();
    let manager = test_manager(data_dir.clone())?;
    let thread = sample_thread("thr_compaction_sink");
    manager.store.save_thread(&thread)?;
    let turn = sample_turn(
        &thread.id,
        "turn_compaction_sink",
        RuntimeTurnStatus::InProgress,
    );
    manager.store.save_turn(&turn)?;
    manager.register_runtime_usage_sink(&turn.id);
    let lease = crate::cost_status::acquire_runtime_usage_lease(&turn.id)
        .expect("active runtime owner lease");
    crate::cost_status::finish_runtime_usage_owner(&turn.id);

    let route = crate::cost_status::EffectiveRouteEnvelope::capture(
        None,
        ProviderKind::Deepseek,
        "deepseek",
        "deepseek-v4-flash",
        Some(ProviderKind::Deepseek.provider().default_base_url()),
        Utc::now(),
    );
    crate::cost_status::report_effective_route_for_runtime(
        crate::cost_status::scope_token(),
        Some(&turn.id),
        "compaction:response:test",
        &route,
        &Usage {
            input_tokens: 17,
            output_tokens: 3,
            ..Usage::default()
        },
    );
    crate::cost_status::report_effective_route_for_runtime(
        crate::cost_status::scope_token(),
        Some(&turn.id),
        "compaction:response:test",
        &route,
        &Usage {
            input_tokens: 17,
            output_tokens: 3,
            ..Usage::default()
        },
    );
    drop(lease);
    drop(manager);

    let reopened = test_manager(data_dir.clone())?;
    reopened.register_runtime_usage_sink(&turn.id);
    crate::cost_status::report_effective_route_for_runtime(
        crate::cost_status::scope_token(),
        Some(&turn.id),
        "compaction:response:test",
        &route,
        &Usage {
            input_tokens: 17,
            output_tokens: 3,
            ..Usage::default()
        },
    );
    crate::cost_status::finish_runtime_usage_owner(&turn.id);
    drop(reopened);
    let reopened = test_manager(data_dir)?;
    let persisted = reopened.store.load_turn(&turn.id)?;
    assert_eq!(persisted.routed_usage.len(), 1);
    assert_eq!(persisted.routed_usage[0].usage.input_tokens, 17);
    assert!(
        crate::cost_status::take_runtime_usage(&turn.id)
            .records
            .is_empty(),
        "successful synchronous persistence must not duplicate into fallback"
    );
    assert!(
        crate::cost_status::drain().is_empty(),
        "runtime-owned usage must not duplicate into the TUI pool"
    );
    Ok(())
}

#[test]
fn routed_usage_append_is_bounded_and_idempotent_for_every_delivery_path() {
    let mut turn = sample_turn(
        "thr_all_paths",
        "turn_all_paths",
        RuntimeTurnStatus::InProgress,
    );
    let route = crate::cost_status::EffectiveRouteEnvelope::capture(
        None,
        ProviderKind::Deepseek,
        "deepseek-primary",
        "deepseek-v4-flash",
        Some(ProviderKind::Deepseek.provider().default_base_url()),
        Utc::now(),
    );
    for index in 0..90 {
        let path = match index % 3 {
            0 => "sink",
            1 => "mailbox",
            _ => "fallback",
        };
        let changed = append_routed_usage_record(
            &mut turn,
            &format!("{path}:response:{index}"),
            crate::cost_status::EffectiveRouteUsage {
                route: route.clone(),
                usage: Usage {
                    input_tokens: index + 1,
                    ..Usage::default()
                },
            },
        );
        assert_eq!(
            changed,
            index <= u32::try_from(MAX_ROUTED_USAGE_RECORDS_PER_TURN).unwrap()
        );
    }

    assert_eq!(turn.routed_usage.len(), MAX_ROUTED_USAGE_RECORDS_PER_TURN);
    assert_eq!(turn.routed_usage_dropped_records, 1);
    assert_eq!(
        turn.routed_usage_source_ids.len(),
        MAX_ROUTED_USAGE_RECORDS_PER_TURN
    );
    let before = turn.clone();
    assert!(!append_routed_usage_record(
        &mut turn,
        "sink:response:0",
        crate::cost_status::EffectiveRouteUsage {
            route,
            usage: Usage {
                input_tokens: 999,
                ..Usage::default()
            },
        },
    ));
    assert_eq!(turn.routed_usage, before.routed_usage);
    assert_eq!(
        turn.routed_usage_dropped_records,
        before.routed_usage_dropped_records
    );
}

#[tokio::test]
async fn raw_metadata_missing_usage_deduplicates_direct_drop_and_legacy_totals() -> Result<()> {
    let _cost_scope = crate::cost_status::test_scope();
    for mode in [
        RouteBillingMode::Metered,
        RouteBillingMode::Subscription,
        RouteBillingMode::Local,
    ] {
        let manager = test_manager(test_runtime_dir())?;
        let thread = sample_thread("thr_raw_missing");
        manager.store.save_thread(&thread)?;
        let mut turn = sample_turn(&thread.id, "turn_raw_missing", RuntimeTurnStatus::Completed);
        let mut route = EffectiveRouteEnvelope::capture(
            None,
            ProviderKind::Deepseek,
            "deepseek",
            "deepseek-v4-flash",
            Some(ProviderKind::Deepseek.provider().default_base_url()),
            Utc::now(),
        );
        route.billing_mode = mode;
        let source_id = "raw-metadata-missing-response";
        let metadata = json!({ "child_usage_records": [{ "source_id": source_id, "route": route, "usage": Usage::default() }] });
        let batch = crate::cost_status::child_usage_records_from_metadata(&metadata)
            .expect("legacy raw metadata");
        assert_eq!(batch.records.len(), 1);
        append_initial_routed_usage_to_turn(&mut turn, &batch);
        manager.store.save_turn(&turn)?;
        manager.register_runtime_usage_sink(&turn.id);
        crate::cost_status::report_unreceipted_provider_success(
            crate::cost_status::scope_token(),
            Some(&turn.id),
            &format!("routed:{}", routed_usage_source_fingerprint(source_id)),
            &route,
        );
        crate::cost_status::finish_runtime_usage_owner(&turn.id);
        let persisted = manager.store.load_turn(&turn.id)?;
        assert!(persisted.routed_usage.is_empty());
        assert_eq!(persisted.routed_usage_drop_records.len(), 1);
        assert_eq!(persisted.routed_usage_source_ids.len(), 1);
        assert_eq!(persisted.routed_usage_dropped_records, 0);

        for legacy in [false, true] {
            if legacy {
                // Old disk rows remain readable without a rewriting migration.
                let mut legacy_turn = persisted.clone();
                legacy_turn.routed_usage_drop_records.clear();
                legacy_turn.routed_usage.push(EffectiveRouteUsage {
                    route: route.clone(),
                    usage: Usage::default(),
                });
                manager.store.save_turn(&legacy_turn)?;
            }
            let totals = manager
                .aggregate_usage(None, None, UsageGroupBy::Thread)
                .await?
                .totals;
            let split = manager.aggregate_usage_for_thread(&thread.id).await?;
            for totals in [&totals, &split.routed_children] {
                assert_eq!(totals.dropped_usage_records, 1);
                assert_eq!(totals.turns, 1);
                assert_eq!(totals.priced_turns, 0);
                assert_eq!(totals.cny_priced_turns, 0);
                assert_eq!(
                    totals.unpriced_turns,
                    u64::from(mode == RouteBillingMode::Metered)
                );
                assert_eq!(
                    totals.nonmetered_turns,
                    u64::from(mode != RouteBillingMode::Metered)
                );
                assert_eq!(totals.cost_complete, mode != RouteBillingMode::Metered);
                assert!(
                    totals
                        .route_receipts
                        .iter()
                        .all(|receipt| receipt.contains("usage=missing"))
                );
            }
        }
    }
    Ok(())
}

#[test]
fn routed_usage_rejects_new_records_when_legacy_source_ledger_already_exceeds_bound() {
    let mut turn = sample_turn(
        "thr_legacy_ledger",
        "turn_legacy_ledger",
        RuntimeTurnStatus::Completed,
    );
    turn.routed_usage_source_ids = (0..=MAX_ROUTED_USAGE_RECORDS_PER_TURN)
        .map(|index| routed_usage_source_fingerprint(&format!("legacy-response-{index}")))
        .collect();
    let route = crate::cost_status::EffectiveRouteEnvelope::capture(
        None,
        ProviderKind::Deepseek,
        "deepseek",
        "deepseek-v4-flash",
        Some(ProviderKind::Deepseek.provider().default_base_url()),
        Utc::now(),
    );
    let record = EffectiveRouteUsage {
        route: route.clone(),
        usage: Usage {
            input_tokens: 1,
            ..Usage::default()
        },
    };
    turn.routed_usage = vec![record.clone(); MAX_ROUTED_USAGE_RECORDS_PER_TURN];
    let source_count = turn.routed_usage_source_ids.len();
    assert!(append_routed_usage_record(
        &mut turn,
        "new-response",
        record
    ));
    assert!(!append_routed_usage_drop_record(
        &mut turn,
        RuntimeUsageDropRecord {
            reason: crate::cost_status::RuntimeUsageMissingReason::default(),
            source_id: "another-new-response".to_string(),
            route,
        }
    ));
    assert_eq!(turn.routed_usage.len(), MAX_ROUTED_USAGE_RECORDS_PER_TURN);
    assert_eq!(turn.routed_usage_source_ids.len(), source_count);
    assert!(turn.routed_usage_drop_records.is_empty());
    assert_eq!(turn.routed_usage_dropped_records, 1);
}

#[tokio::test]
async fn aggregate_usage_fails_closed_for_legacy_reconstructed_route() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = sample_thread("thr_legacy_cost");
    manager.store.save_thread(&thread)?;
    let mut turn = sample_turn(&thread.id, "turn_legacy_cost", RuntimeTurnStatus::Completed);
    turn.usage = Some(Usage {
        input_tokens: 1_000,
        output_tokens: 10,
        ..Usage::default()
    });
    // These mutable-era fields are intentionally insufficient: no exact
    // identity, billing mode, or dispatch timestamp was persisted.
    turn.effective_provider = Some(ProviderKind::Deepseek.as_str().to_string());
    turn.effective_model = Some("deepseek-v4-flash".to_string());
    manager.store.save_turn(&turn)?;

    let report = manager
        .aggregate_usage(None, None, UsageGroupBy::Thread)
        .await?;
    assert_eq!(report.totals.cost_usd, 0.0);
    assert_eq!(report.totals.unpriced_turns, 1);
    assert!(
        report
            .totals
            .unpriced_reasons
            .contains("unknown_provider_route")
    );
    assert!(report.totals.route_receipts.is_empty());
    Ok(())
}

#[test]
fn runtime_usage_accumulators_saturate_tokens_and_currency() {
    let mut total = f64::MAX;
    saturating_add_cost_amount(&mut total, f64::MAX);
    assert_eq!(total, f64::MAX);

    let thread = sample_thread("thr_saturating");
    let turn = sample_turn(&thread.id, "turn_saturating", RuntimeTurnStatus::Completed);
    let mut totals = UsageTotals {
        input_tokens: u64::MAX,
        output_tokens: u64::MAX,
        cached_tokens: u64::MAX,
        reasoning_tokens: u64::MAX,
        reasoning_replay_tokens: u64::MAX,
        cache_write_tokens: u64::MAX,
        ..UsageTotals::default()
    };
    let mut buckets = std::collections::BTreeMap::new();
    accumulate_runtime_usage_record(
        &mut totals,
        &mut buckets,
        UsageGroupBy::Thread,
        None,
        &Usage {
            input_tokens: 1,
            output_tokens: 1,
            prompt_cache_hit_tokens: Some(1),
            prompt_cache_write_tokens: Some(1),
            reasoning_tokens: Some(1),
            reasoning_replay_tokens: Some(1),
            ..Usage::default()
        },
        &turn,
        &thread,
    );
    assert_eq!(totals.input_tokens, u64::MAX);
    assert_eq!(totals.output_tokens, u64::MAX);
    assert_eq!(totals.cached_tokens, u64::MAX);
    assert_eq!(totals.reasoning_tokens, u64::MAX);
    assert_eq!(totals.reasoning_replay_tokens, u64::MAX);
    assert_eq!(totals.cache_write_tokens, u64::MAX);
}

fn sample_item(turn_id: &str, item_id: &str, status: TurnItemLifecycleStatus) -> TurnItemRecord {
    TurnItemRecord {
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: item_id.to_string(),
        turn_id: turn_id.to_string(),
        kind: TurnItemKind::Status,
        status,
        summary: "sample item".to_string(),
        detail: None,
        metadata: None,
        artifact_refs: Vec::new(),
        artifacts: Vec::new(),
        started_at: Some(Utc::now()),
        ended_at: None,
    }
}

async fn wait_for_sender_strong_count<T>(
    sender: &tokio::sync::mpsc::Sender<T>,
    minimum: usize,
) -> Result<()> {
    tokio::time::timeout(TURN_SETTLEMENT_DEADLOCK_TIMEOUT, async {
        while sender.strong_count() < minimum {
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| anyhow!("Timed out waiting for mailbox reservation"))?;
    Ok(())
}

async fn wait_for_terminal_turn(
    manager: &RuntimeThreadManager,
    turn_id: &str,
) -> Result<TurnRecord> {
    let mut event_rx = manager.subscribe_events();
    // Every caller waits for the same durable monitor boundary. Keep its
    // deadlock watchdog here so individual fixtures cannot turn scheduler or
    // filesystem contention into an accidental two-second performance gate.
    let deadline = Instant::now() + TURN_SETTLEMENT_DEADLOCK_TIMEOUT;
    loop {
        let turn = manager.store.load_turn(turn_id)?;
        let terminal = matches!(
            turn.status,
            RuntimeTurnStatus::Completed
                | RuntimeTurnStatus::Failed
                | RuntimeTurnStatus::Interrupted
                | RuntimeTurnStatus::Canceled
        );
        let timed_out = Instant::now() >= deadline;
        if terminal || timed_out {
            let receipt_is_durable =
                manager
                    .events_since(&turn.thread_id, None)?
                    .iter()
                    .any(|event| {
                        event.event == "turn.completed" && event.turn_id.as_deref() == Some(turn_id)
                    });
            let active_claim_present = manager
                .active_turn_flags(&turn.thread_id, turn_id)
                .await
                .is_some();
            if terminal && receipt_is_durable && !active_claim_present {
                return Ok(turn);
            }
            if timed_out {
                bail!(
                    "Timed out waiting for turn {turn_id}: status={:?}, \
                     completion_receipt_present={receipt_is_durable}, \
                     active_claim_present={active_claim_present}",
                    turn.status
                );
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            continue;
        }
        tokio::select! {
            // Recheck durable state on a lifecycle event; the terminal record,
            // completion receipt and released claim remain the success gate.
            _ = event_rx.recv() => {}
            _ = sleep(remaining.min(Duration::from_millis(20))) => {}
        }
    }
}

#[test]
fn store_load_scenario() {
    // Scenario consolidation of: store_load_thread_rejects_newer_schema_version, store_load_turn_rejects_newer_schema_version, store_load_item_rejects_newer_schema_version
    // from store_load_thread_rejects_newer_schema_version
    {
        let dir = test_runtime_dir();
        let store = RuntimeThreadStore::open(dir.clone()).expect("open store");

        // Construct a thread record persisted with a future schema version.
        let mut thread = sample_thread("thr_future");
        thread.schema_version = MAX_SUPPORTED_RUNTIME_SCHEMA_VERSION + 1;

        // Bypass save_thread (which would respect our local schema_version)
        // by writing the JSON directly so we can simulate a future writer.
        let path = store.threads_dir.join(format!("{}.json", thread.id));
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdirs");
        let payload = serde_json::to_string(&thread).expect("serialize thread");
        std::fs::write(&path, payload).expect("write thread");

        let err = store
            .load_thread(&thread.id)
            .expect_err("load_thread must reject newer schema");
        let msg = format!("{err:#}");
        assert!(msg.contains("newer than supported"), "got: {msg}");

        // Cleanup so we don't leak across tests.
        let _ = std::fs::remove_dir_all(dir);
    }
    // from store_load_turn_rejects_newer_schema_version
    {
        let dir = test_runtime_dir();
        let store = RuntimeThreadStore::open(dir.clone()).expect("open store");

        let mut turn = sample_turn("thr_t", "trn_future", RuntimeTurnStatus::InProgress);
        turn.schema_version = PREFIX_CHECKPOINT_RUNTIME_SCHEMA_VERSION + 1;

        let path = store.turns_dir.join(format!("{}.json", turn.id));
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdirs");
        std::fs::write(&path, serde_json::to_string(&turn).expect("serialize turn"))
            .expect("write turn");

        let err = store
            .load_turn(&turn.id)
            .expect_err("load_turn must reject newer schema");
        assert!(
            format!("{err:#}").contains("newer than supported"),
            "got: {err:#}"
        );

        let _ = std::fs::remove_dir_all(dir);
    }
    // from store_load_item_rejects_newer_schema_version
    {
        let dir = test_runtime_dir();
        let store = RuntimeThreadStore::open(dir.clone()).expect("open store");

        let mut item = sample_item("trn_t", "itm_future", TurnItemLifecycleStatus::InProgress);
        item.schema_version = MAX_SUPPORTED_RUNTIME_SCHEMA_VERSION + 1;

        let path = store.items_dir.join(format!("{}.json", item.id));
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdirs");
        std::fs::write(&path, serde_json::to_string(&item).expect("serialize item"))
            .expect("write item");

        let err = store
            .load_item(&item.id)
            .expect_err("load_item must reject newer schema");
        assert!(
            format!("{err:#}").contains("newer than supported"),
            "got: {err:#}"
        );

        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn store_restart_discards_only_operation_bindings_that_never_acquired_a_turn() -> Result<()> {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone())?;
    let key_fingerprint = codewhale_runtime::hashing::sha256_hex("scoped-key");
    let binding = RuntimeTurnOperationBinding {
        schema_version: TURN_OPERATION_BINDING_SCHEMA_VERSION,
        thread_id: "thr_torn_binding".to_string(),
        turn_id: "turn_never_persisted".to_string(),
        operation_key_fingerprint: key_fingerprint.clone(),
        request_fingerprint: codewhale_runtime::hashing::sha256_hex("request"),
        created_at: Utc::now(),
    };
    store.save_turn_operation_binding(&binding)?;
    assert!(
        store
            .load_turn_operation_binding(&key_fingerprint)?
            .is_some()
    );

    drop(store);
    let reopened = RuntimeThreadStore::open(dir)?;
    assert!(
        reopened
            .load_turn_operation_binding(&key_fingerprint)?
            .is_none(),
        "a binding written before its turn is safe to discard because engine submission is later"
    );
    Ok(())
}

#[test]
fn runtime_event_sequences_serialize_across_real_processes() -> Result<()> {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone())?;
    let thread_id = "thr_process_writers";
    let start = dir.join("process-writers.start");
    let writer_count = 4_u64;
    let events_per_writer = 8_u64;
    let mut children = Vec::new();

    for worker in 0..writer_count {
        let signal = dir.join(format!("process-writer-{worker}.ready"));
        children.push((
            spawn_runtime_event_child("writer", &dir, thread_id, &signal),
            signal,
        ));
    }
    for (_, signal) in &children {
        wait_for_runtime_event_test_file(signal, "event writer readiness");
    }
    // The children already opened their stores before this barrier. The old
    // store-local sequence cache therefore allocated duplicates deterministically.
    std::fs::write(&start, b"go")?;
    for (child, _) in children {
        child.wait_success("cross-process event writer");
    }

    let events = store.events_since(thread_id, None)?;
    let expected = writer_count * events_per_writer;
    assert_eq!(u64::try_from(events.len())?, expected);
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        (1..=expected).collect::<Vec<_>>()
    );
    let identities = events
        .iter()
        .map(|event| {
            (
                event.payload["worker"].as_str().unwrap().to_string(),
                event.payload["index"].as_u64().unwrap(),
            )
        })
        .collect::<HashSet<_>>();
    assert_eq!(u64::try_from(identities.len())?, expected);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    assert_eq!(runtime.block_on(store.current_seq())?, expected);
    assert_eq!(
        load_runtime_store_state(&store.state_path)?.next_seq,
        expected + 1
    );

    std::fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn runtime_process_owner_lock_copy_helps_the_user_recover() {
    assert!(
        RUNTIME_PROCESS_OWNER_LOCK_HELD.contains("already active in another process"),
        "{RUNTIME_PROCESS_OWNER_LOCK_HELD}"
    );
    assert!(
        RUNTIME_PROCESS_OWNER_LOCK_HELD.contains("Close the other Codewhale session"),
        "{RUNTIME_PROCESS_OWNER_LOCK_HELD}"
    );
    assert!(
        RUNTIME_PROCESS_OWNER_LOCK_HELD.contains("CODEWHALE_RUNTIME_DIR"),
        "{RUNTIME_PROCESS_OWNER_LOCK_HELD}"
    );
    assert!(
        !RUNTIME_PROCESS_OWNER_LOCK_HELD.contains("thread store"),
        "{RUNTIME_PROCESS_OWNER_LOCK_HELD}"
    );
}

#[test]
fn runtime_manager_store_has_one_lifetime_process_owner() -> Result<()> {
    let dir = test_runtime_dir();
    let signal = dir.join("manager-owner.ready");
    let release = dir.join("process-writers.start");
    let child = spawn_runtime_event_child("manager-holder", &dir, "thr_unused", &signal);
    wait_for_runtime_event_test_file(&signal, "manager process owner readiness");

    let error = match test_manager(dir.clone()) {
        Ok(_) => panic!("a second process must not open the same Runtime manager store"),
        Err(error) => error,
    };
    let message = format!("{error:#}");
    assert!(
        message.contains("already active in another process"),
        "unexpected owner-lock error: {error:#}"
    );
    assert!(
        message.contains("CODEWHALE_RUNTIME_DIR"),
        "owner-lock error should name the override for a shared root: {error:#}"
    );
    assert!(
        message.contains("Close the other Codewhale session"),
        "owner-lock error should name the recovery action: {error:#}"
    );
    assert!(
        !message.contains("thread store"),
        "owner-lock error should not leak the store implementation: {error:#}"
    );

    let distinct_dir = test_runtime_dir();
    let distinct = test_manager(distinct_dir)?;
    drop(distinct);

    std::fs::write(release, b"release")?;
    child.wait_success("Runtime manager owner holder");
    let reopened = test_manager(dir)?;
    drop(reopened);
    Ok(())
}

#[test]
fn session_scoped_scenario() -> Result<()> {
    // Scenario consolidation of: session_scoped_runtime_default_lives_under_the_session_directory, session_scoped_runtime_roots_do_not_share_the_process_owner_lock
    // from session_scoped_runtime_default_lives_under_the_session_directory
    {
        let _lock = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir().expect("temp home");
        let home = temp.path().join("cw-home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
        let _runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");

        let cfg = RuntimeThreadManagerConfig::for_session(home.join("tasks"), "sess-1");
        assert_eq!(
            cfg.data_dir,
            home.join("sessions").join("sess-1").join("runtime")
        );
    }
    // from session_scoped_runtime_roots_do_not_share_the_process_owner_lock
    {
        let _lock = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("cw-home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
        let _runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        let tasks = home.join("tasks");

        let first = RuntimeThreadManager::open(
            Config::default(),
            PathBuf::from("."),
            RuntimeThreadManagerConfig::for_session(tasks.clone(), "session-a"),
        )?;
        let second = RuntimeThreadManager::open(
            Config::default(),
            PathBuf::from("."),
            RuntimeThreadManagerConfig::for_session(tasks, "session-b"),
        )?;
        drop(first);
        drop(second);
    }
    Ok(())
}

#[test]
fn explicit_runtime_dir_override_beats_session_scope() {
    let _lock = crate::test_support::lock_test_env();
    let temp = tempfile::tempdir().expect("temp home");
    let home = temp.path().join("cw-home");
    let override_dir = temp.path().join("shared-runtime");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
    let _runtime = crate::test_support::EnvVarGuard::set("CODEWHALE_RUNTIME_DIR", &override_dir);
    let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");

    let cfg = RuntimeThreadManagerConfig::for_session(home.join("tasks"), "sess-1");
    assert_eq!(cfg.data_dir, override_dir);
}

#[test]
fn fresh_runtime_manager_store_race_has_exactly_one_process_owner() -> Result<()> {
    let control = test_runtime_dir();
    std::fs::create_dir_all(&control)?;
    let store_root = control.join("fresh-runtime-store");
    let start = control.join("manager-race.start");
    let release = control.join("manager-race.release");
    let signal_a = control.join("manager-race-a.signal");
    let signal_b = control.join("manager-race-b.signal");
    assert!(!store_root.exists(), "race must begin from an absent store");

    let child_a = spawn_runtime_manager_racer(&store_root, &signal_a, &start, &release);
    let child_b = spawn_runtime_manager_racer(&store_root, &signal_b, &start, &release);
    wait_for_runtime_event_test_value(&signal_a, &["ready"], "manager racer A readiness");
    wait_for_runtime_event_test_value(&signal_b, &["ready"], "manager racer B readiness");
    assert!(
        !store_root.exists(),
        "neither child may initialize owner-derived state before the common barrier"
    );

    std::fs::write(&start, b"go")?;
    let mut outcomes = vec![
        wait_for_runtime_event_test_value(&signal_a, &["won", "lost"], "manager racer A outcome"),
        wait_for_runtime_event_test_value(&signal_b, &["won", "lost"], "manager racer B outcome"),
    ];
    outcomes.sort();
    assert_eq!(outcomes, vec!["lost", "won"]);

    let owner_path = store_root.join(AGENT_MAIL_OWNER_FILE);
    let owner: RuntimeStoreOwner = serde_json::from_str(&read_store_file(&owner_path)?)?;
    validated_record_id(&owner.owner_id, "Runtime owner id")?;
    assert!(owner.owner_id.starts_with("owner_"));

    std::fs::write(&release, b"release")?;
    child_a.wait_success("fresh Runtime manager racer A");
    child_b.wait_success("fresh Runtime manager racer B");
    let reopened = test_manager(store_root)?;
    assert_eq!(reopened.test_store().owner_id, owner.owner_id);
    drop(reopened);
    std::fs::remove_dir_all(control)?;
    Ok(())
}

#[test]
fn events_from_offset_does_not_reread_earlier_records() -> Result<()> {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone())?;
    let thread_id = "thr_cursor";
    for index in 0..3 {
        store.append_event_transaction(
            thread_id.to_string(),
            None,
            None,
            "cursor.event".to_string(),
            json!({ "index": index }),
            Duration::from_secs(1),
        )?;
    }

    let (first, cursor) = store.events_from_offset(thread_id, 0, None)?;
    assert_eq!(first.len(), 3);
    assert!(cursor > 0);
    let (empty, same_cursor) = store.events_from_offset(thread_id, cursor, None)?;
    assert!(empty.is_empty());
    assert_eq!(same_cursor, cursor);

    store.append_event_transaction(
        thread_id.to_string(),
        None,
        None,
        "cursor.event".to_string(),
        json!({ "index": 3 }),
        Duration::from_secs(1),
    )?;
    let (tail, next_cursor) = store.events_from_offset(thread_id, cursor, None)?;
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].payload["index"], 3);
    assert!(next_cursor > cursor);

    let (limited, limited_cursor) = store.events_from_offset(thread_id, 0, Some(2))?;
    assert_eq!(limited.len(), 2);
    assert!(limited_cursor < next_cursor);
    let (rest, _) = store.events_from_offset(thread_id, limited_cursor, None)?;
    assert_eq!(rest.len(), 2);

    std::fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn runtime_event_lock_timeout_is_non_mutating_and_holder_death_releases_lock() -> Result<()> {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone())?;
    let thread_id = "thr_process_holder";
    let signal = dir.join("process-holder.ready");
    let child = spawn_runtime_event_child("holder", &dir, thread_id, &signal);
    wait_for_runtime_event_test_file(&signal, "held Runtime event lock");

    let state_before = std::fs::read(&store.state_path)?;
    let event_path = store.events_path(thread_id)?;
    let started = Instant::now();
    let error = store
        .append_event_transaction(
            thread_id.to_string(),
            None,
            None,
            "must.timeout".to_string(),
            json!({}),
            Duration::from_millis(75),
        )
        .expect_err("contended Runtime event transaction must time out");
    let elapsed = started.elapsed();
    assert!(error.downcast_ref::<RuntimeEventLockTimeout>().is_some());
    assert!(
        elapsed >= Duration::from_millis(75),
        "returned too early: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "timeout was unbounded: {elapsed:?}"
    );
    assert_eq!(std::fs::read(&store.state_path)?, state_before);
    assert!(!event_path.exists());

    let status = child.kill_and_reap()?;
    assert!(
        !status.success(),
        "killed lock holder unexpectedly succeeded"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let committed = runtime.block_on(store.append_event(
        thread_id,
        None,
        None,
        "after.holder.death",
        json!({}),
    ))?;
    assert_eq!(committed.seq, 1);

    std::fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn killed_torn_append_is_repaired_and_preserves_reserved_gap() -> Result<()> {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone())?;
    let thread_id = "thr_process_torn";
    let signal = dir.join("process-torn.ready");
    let child = spawn_runtime_event_child("torn", &dir, thread_id, &signal);
    wait_for_runtime_event_test_file(&signal, "durable torn Runtime event tail");
    let status = child.kill_and_reap()?;
    assert!(
        !status.success(),
        "killed torn writer unexpectedly succeeded"
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let committed = runtime.block_on(store.append_event(
        thread_id,
        None,
        None,
        "after.torn.death",
        json!({}),
    ))?;
    assert_eq!(committed.seq, 2);
    assert_eq!(runtime.block_on(store.current_seq())?, 2);
    let events = store.events_since(thread_id, None)?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].seq, 2);
    let raw = std::fs::read(store.events_path(thread_id)?)?;
    assert_eq!(raw.last(), Some(&b'\n'));
    for line in raw
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        serde_json::from_slice::<RuntimeEventRecord>(line)?;
    }

    std::fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn event_reader_never_observes_cross_process_rollback_candidate() -> Result<()> {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone())?;
    let thread_id = "thr_process_phantom";
    let signal = dir.join("process-phantom.ready");
    let child = spawn_runtime_event_child("phantom", &dir, thread_id, &signal);
    wait_for_runtime_event_test_file(&signal, "visible rollback candidate");

    // Keep the candidate present until the real reader has attempted the
    // transaction. A relative sleep in the writer can expire before a loaded
    // parent is scheduled and never exercise a contended read at all.
    let error = store
        .events_since(thread_id, None)
        .expect_err("reader must not expose a candidate while its writer holds the lock");
    assert!(
        error.downcast_ref::<RuntimeEventLockTimeout>().is_some(),
        "reader failed for an unexpected reason: {error:#}"
    );
    std::fs::write(dir.join("process-writers.start"), b"rollback")?;
    child.wait_success("phantom rollback child");
    assert!(store.events_since(thread_id, None)?.is_empty());

    std::fs::remove_dir_all(dir)?;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn event_history_lock_wait_does_not_block_tokio_worker() -> Result<()> {
    let dir = test_runtime_dir();
    let manager = test_manager(dir.clone())?;
    let thread_id = "thr_async_history_wait";
    let signal = dir.join("async-history-holder.ready");
    let child = spawn_runtime_event_child("holder", &dir, thread_id, &signal);
    wait_for_runtime_event_test_file(&signal, "async history lock holder");

    let history = manager.events_since_async(thread_id, None);
    tokio::pin!(history);
    tokio::select! {
        result = &mut history => panic!("history lock wait returned early: {result:?}"),
        () = sleep(Duration::from_millis(50)) => {}
    }
    let status = child.kill_and_reap()?;
    assert!(!status.success(), "killed history lock holder succeeded");
    assert!(history.await?.is_empty());

    std::fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn runtime_event_sequence_exhaustion_fails_before_mutation() -> Result<()> {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone())?;
    store.with_event_transaction(Duration::from_secs(1), || {
        write_json_atomic(
            &store.state_path,
            &RuntimeStoreState {
                schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                next_seq: u64::MAX,
            },
        )
    })?;
    let state_before = std::fs::read(&store.state_path)?;
    let thread_id = "thr_sequence_exhausted";
    let error = store
        .append_event_transaction(
            thread_id.to_string(),
            None,
            None,
            "must.not.append".to_string(),
            json!({}),
            Duration::from_secs(1),
        )
        .expect_err("u64 sequence exhaustion must fail closed");
    assert!(
        format!("{error:#}").contains("Runtime event sequence exhausted"),
        "unexpected exhaustion error: {error:#}"
    );
    assert_eq!(std::fs::read(&store.state_path)?, state_before);
    assert!(!store.events_path(thread_id)?.exists());

    std::fs::remove_dir_all(dir)?;
    Ok(())
}

#[tokio::test]
async fn store_open_truncates_only_torn_final_event_record_and_preserves_sequence_gap() {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone()).expect("open store");
    let first = store
        .append_event("thr_torn_tail", None, None, "first", json!({ "value": 1 }))
        .await
        .expect("append first event");
    let torn = store
        .append_event("thr_torn_tail", None, None, "torn", json!({ "value": 2 }))
        .await
        .expect("append event to tear");
    let path = store.events_path("thr_torn_tail").expect("event path");
    let original_len = std::fs::metadata(&path).expect("event metadata").len();
    assert!(original_len > 16);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open event log for simulated crash")
        .set_len(original_len - 16)
        .expect("tear final event record");
    drop(store);

    let reopened = RuntimeThreadStore::open(dir.clone()).expect("repair torn event tail");
    let replay = reopened
        .events_since("thr_torn_tail", None)
        .expect("replay repaired event log");
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].seq, first.seq);

    let after_repair = reopened
        .append_event(
            "thr_torn_tail",
            None,
            None,
            "after_repair",
            json!({ "value": 3 }),
        )
        .await
        .expect("append after repair");
    assert_eq!(after_repair.seq, torn.seq.saturating_add(1));
    assert_eq!(
        reopened
            .events_since("thr_torn_tail", None)
            .expect("replay repaired and appended events")
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![first.seq, after_repair.seq]
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn store_open_treats_valid_json_without_newline_as_uncommitted_append() {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone()).expect("open store");
    let committed = store
        .append_event(
            "thr_missing_commit_marker",
            None,
            None,
            "committed",
            json!({ "value": 1 }),
        )
        .await
        .expect("append committed event");
    let uncommitted = store
        .append_event(
            "thr_missing_commit_marker",
            None,
            None,
            "missing_marker",
            json!({ "value": 2 }),
        )
        .await
        .expect("append event whose commit marker will be removed");
    let path = store
        .events_path("thr_missing_commit_marker")
        .expect("event path");
    let encoded = std::fs::read(&path).expect("read event log");
    assert_eq!(encoded.last(), Some(&b'\n'));
    let without_marker = &encoded[..encoded.len() - 1];
    let final_json = without_marker
        .rsplit(|byte| *byte == b'\n')
        .next()
        .expect("final JSON record");
    serde_json::from_slice::<RuntimeEventRecord>(final_json)
        .expect("the unterminated tail must otherwise be valid JSON");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open event log for simulated crash")
        .set_len(u64::try_from(without_marker.len()).expect("event log length fits u64"))
        .expect("remove only the newline commit marker");
    drop(store);

    let reopened = RuntimeThreadStore::open(dir.clone()).expect("repair uncommitted event tail");
    let replay = reopened
        .events_since("thr_missing_commit_marker", None)
        .expect("replay repaired event log");
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].seq, committed.seq);

    let after_repair = reopened
        .append_event(
            "thr_missing_commit_marker",
            None,
            None,
            "after_repair",
            json!({ "value": 3 }),
        )
        .await
        .expect("append after repair");
    assert_eq!(after_repair.seq, uncommitted.seq.saturating_add(1));

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn store_open_does_not_discard_newline_terminated_malformed_event() {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone()).expect("open store");
    store
        .append_event("thr_bad_tail", None, None, "valid", json!({}))
        .await
        .expect("append valid event");
    let path = store.events_path("thr_bad_tail").expect("event path");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open event log");
    std::io::Write::write_all(&mut file, b"{malformed-but-terminated}\n")
        .expect("append malformed event");
    std::io::Write::flush(&mut file).expect("flush malformed event");
    drop(file);
    drop(store);

    let reopened = RuntimeThreadStore::open(dir.clone()).expect("open terminated event log");
    let error = reopened
        .events_since("thr_bad_tail", None)
        .expect_err("terminated malformed event must fail closed");
    assert!(
        format!("{error:#}").contains("Failed to parse event line"),
        "unexpected replay error: {error:#}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn event_replay_is_bounded_and_tail_cursor_skips_only_omitted_history() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread_id = "thr_bounded_replay";
    let path = manager.store.events_path(thread_id)?;
    let mut encoded = Vec::new();
    for seq in 1_u64..=600 {
        let event = RuntimeEventRecord {
            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
            seq,
            timestamp: Utc::now(),
            thread_id: thread_id.to_string(),
            turn_id: None,
            item_id: None,
            event: "test.event".to_string(),
            payload: json!({ "seq": seq }),
        };
        serde_json::to_writer(&mut encoded, &event)?;
        encoded.push(b'\n');
    }
    std::fs::write(path, encoded)?;

    let mut full = manager.replay_events(thread_id, None, None).await?;
    assert_eq!(full.base_seq, 0);
    let mut full_sequences = Vec::new();
    while let Some(batch) = full.batches.recv().await {
        let batch = batch.map_err(anyhow::Error::msg)?;
        assert!(
            batch.len() <= RUNTIME_EVENT_REPLAY_BATCH_SIZE,
            "replay batch exceeded its memory bound"
        );
        full_sequences.extend(batch.into_iter().map(|event| event.seq));
    }
    assert_eq!(full_sequences, (1_u64..=600).collect::<Vec<_>>());

    let mut tail = manager.replay_events(thread_id, None, Some(10)).await?;
    assert_eq!(tail.base_seq, 590);
    let mut tail_sequences = Vec::new();
    while let Some(batch) = tail.batches.recv().await {
        tail_sequences.extend(
            batch
                .map_err(anyhow::Error::msg)?
                .into_iter()
                .map(|event| event.seq),
        );
    }
    assert_eq!(tail_sequences, (591_u64..=600).collect::<Vec<_>>());

    let mut empty_tail = manager.replay_events(thread_id, None, Some(0)).await?;
    assert_eq!(empty_tail.base_seq, 600);
    assert!(empty_tail.batches.recv().await.is_none());
    assert!(
        manager
            .replay_events(
                thread_id,
                None,
                Some(MAX_RUNTIME_EVENT_REPLAY_TAIL.saturating_add(1)),
            )
            .await
            .is_err()
    );
    Ok(())
}

#[test]
fn replay_worker_panic_is_reported_not_treated_as_complete() {
    let panicked = || -> std::thread::Result<Result<()>> {
        std::panic::catch_unwind(|| -> Result<()> { panic!("replay parser bug") })
    };

    // Before the base cursor: the request itself fails (HTTP 500).
    let (base_tx, mut base_rx) = oneshot::channel();
    let (batch_tx, mut batch_rx) = mpsc::channel(2);
    let mut base_tx = Some(base_tx);
    RuntimeThreadStore::route_replay_outcome(panicked(), &mut base_tx, &batch_tx);
    drop(batch_tx);
    let error = base_rx
        .try_recv()
        .expect("base cursor waiter must hear the panic")
        .expect_err("a panic is not a cursor");
    assert!(
        error.contains("replay worker panicked: replay parser bug"),
        "{error}"
    );
    assert!(
        batch_rx.try_recv().is_err(),
        "pre-cursor failures belong to the request, not the stream"
    );

    // After the base cursor: the open stream gets an `Err` batch before the
    // channel closes, so it can never mistake the crash for complete history.
    let (batch_tx, mut batch_rx) = mpsc::channel(2);
    let mut base_tx = None;
    RuntimeThreadStore::route_replay_outcome(panicked(), &mut base_tx, &batch_tx);
    drop(batch_tx);
    let error = batch_rx
        .try_recv()
        .expect("stream must receive the failure")
        .expect_err("a panic is not a batch");
    assert!(error.contains("replay worker panicked"), "{error}");
    assert!(matches!(
        batch_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));

    // A clean finish sends nothing: the closed channel alone means complete.
    let (batch_tx, mut batch_rx) = mpsc::channel(2);
    RuntimeThreadStore::route_replay_outcome(Ok(Ok(())), &mut None, &batch_tx);
    drop(batch_tx);
    assert!(matches!(
        batch_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}

#[tokio::test]
async fn event_reader_ignores_an_unterminated_live_append_tail() -> Result<()> {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone())?;
    let committed = store
        .append_event("thr_live_tail", None, None, "committed", json!({}))
        .await?;
    let path = store.events_path("thr_live_tail")?;
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    std::io::Write::write_all(&mut file, br#"{"schema_version":2,"seq":999"#)?;
    std::io::Write::flush(&mut file)?;

    let replay = store.events_since("thr_live_tail", None)?;
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].seq, committed.seq);
    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

#[cfg(unix)]
#[test]
fn store_open_rejects_symlinked_state_file() {
    let dir = test_runtime_dir();
    std::fs::create_dir_all(&dir).expect("mkdir runtime dir");
    let target = dir.join("outside-state.json");
    let link = dir.join("state.json");
    std::fs::write(
        &target,
        serde_json::to_string(&RuntimeStoreState::default()).unwrap(),
    )
    .expect("write target");
    std::os::unix::fs::symlink(&target, &link).expect("symlink state");

    let err = RuntimeThreadStore::open(dir.clone()).expect_err("symlink state should fail");
    assert!(format!("{err:#}").contains("must not be a symlink"));

    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn event_append_rollback_handle_rejects_a_swapped_symlink_target() -> Result<()> {
    let dir = test_runtime_dir();
    std::fs::create_dir_all(&dir)?;
    let outside = dir.join("outside.jsonl");
    let events_path = dir.join("events.jsonl");
    std::fs::write(&outside, b"must-remain-intact\n")?;
    std::os::unix::fs::symlink(&outside, &events_path)?;

    let error = open_runtime_store_file(&events_path, "Runtime event rollback", |options| {
        options.write(true);
    })
    .expect_err("rollback followed a swapped symlink target");
    assert!(format!("{error:#}").contains("must not be a symlink"));
    assert_eq!(std::fs::read(&outside)?, b"must-remain-intact\n");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn store_open_rejects_root_traversal() {
    let dir = test_runtime_dir();
    let bad_root = dir.join("runtime").join("..").join("outside");

    let err = RuntimeThreadStore::open(bad_root).expect_err("traversal root should fail");
    assert!(format!("{err:#}").contains("cannot contain '..'"));

    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn store_open_rejects_symlinked_store_directory() {
    let dir = test_runtime_dir();
    std::fs::create_dir_all(&dir).expect("mkdir runtime dir");
    let outside = dir.join("outside-items");
    let link = dir.join("items");
    std::fs::create_dir_all(&outside).expect("mkdir outside");
    std::os::unix::fs::symlink(&outside, &link).expect("symlink items dir");

    let err = RuntimeThreadStore::open(dir.clone()).expect_err("symlink items dir should fail");
    assert!(
        format!("{err:#}").contains("directory must not be a symlink"),
        "got: {err:#}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn store_list_items_rejects_symlinked_item_file() {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone()).expect("open store");
    let item = sample_item("turn_link", "item_link", TurnItemLifecycleStatus::Completed);
    let target = dir.join("outside-item.json");
    let link = store.items_dir.join(format!("{}.json", item.id));
    std::fs::write(&target, serde_json::to_string(&item).unwrap()).expect("write target");
    std::os::unix::fs::symlink(&target, &link).expect("symlink item");

    let err = store
        .list_items_for_turn(&item.turn_id)
        .expect_err("symlink item should fail");
    assert!(format!("{err:#}").contains("must not be a symlink"));

    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn store_list_items_rejects_swapped_symlinked_store_directory() {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone()).expect("open store");
    let outside = dir.join("outside-items");
    std::fs::create_dir_all(&outside).expect("mkdir outside");
    std::fs::remove_dir_all(&store.items_dir).expect("remove items dir");
    std::os::unix::fs::symlink(&outside, &store.items_dir).expect("symlink items dir");

    let err = store
        .list_items_for_turn("turn_link")
        .expect_err("swapped symlink items dir should fail");
    assert!(
        format!("{err:#}").contains("directory must not be a symlink"),
        "got: {err:#}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn store_load_thread_defaults_missing_additive_runtime_defaults() {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone()).expect("open store");
    let thread = sample_thread("thr_legacy_session");
    let path = store.threads_dir.join(format!("{}.json", thread.id));
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdirs");
    let mut payload = serde_json::to_value(&thread).expect("serialize thread");
    payload
        .as_object_mut()
        .expect("thread object")
        .remove("session_id");
    payload
        .as_object_mut()
        .expect("thread object")
        .remove("reasoning_effort");
    payload
        .as_object_mut()
        .expect("thread object")
        .remove("allowed_tools");
    std::fs::write(
        &path,
        serde_json::to_string(&payload).expect("encode thread"),
    )
    .expect("write thread");

    let loaded = store
        .load_thread(&thread.id)
        .expect("legacy thread should load");
    assert_eq!(loaded.session_id, None);
    assert_eq!(loaded.reasoning_effort, None);
    assert_eq!(loaded.allowed_tools, None);

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn seed_thread_keeps_tool_results_on_preceding_turn() -> Result<()> {
    let dir = test_runtime_dir();
    let manager = test_manager(dir.clone())?;
    let thread = sample_thread("thr_seed_blocks");
    manager.store.save_thread(&thread)?;
    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "check the files".to_string(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "need a tool".to_string(),
                    signature: Some("sig-1".to_string()),
                    state: None,
                    redacted_data: None,
                },
                ContentBlock::ToolUse {
                    execution_id: None,
                    id: "tool-1".to_string(),
                    name: "shell".to_string(),
                    input: json!({ "cmd": "one" }),
                    caller: None,
                    thought_signature: None,
                },
                ContentBlock::ToolUse {
                    execution_id: None,
                    id: "tool-2".to_string(),
                    name: "shell".to_string(),
                    input: json!({ "cmd": "two" }),
                    caller: None,
                    thought_signature: None,
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "tool-1".to_string(),
                content: "one".to_string(),
                is_error: None,
                content_blocks: Some(vec![json!({
                    "type": "text",
                    "text": "structured one"
                })]),
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "tool-2".to_string(),
                content: "two".to_string(),
                is_error: Some(true),
                content_blocks: None,
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "done".to_string(),
                cache_control: None,
            }],
        },
    ];

    manager
        .seed_thread_from_messages(&thread.id, &messages)
        .await?;
    let turns = manager.store.list_turns_for_thread(&thread.id)?;
    assert_eq!(turns.len(), 1);

    let restored = manager.reconstruct_messages_from_turns(&turns)?;
    let roles = restored
        .iter()
        .map(|message| message.role.as_str())
        .collect::<Vec<_>>();
    assert_eq!(roles, vec!["user", "assistant", "user", "assistant"]);
    assert_eq!(restored[2].content.len(), 2);

    match &restored[2].content[0] {
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            content_blocks,
            ..
        } => {
            assert_eq!(tool_use_id, "tool-1");
            assert_eq!(content, "one");
            assert_eq!(*is_error, None);
            assert_eq!(
                content_blocks
                    .as_ref()
                    .and_then(|blocks| blocks[0].get("text")),
                Some(&json!("structured one"))
            );
        }
        other => panic!("expected first tool result, got {other:?}"),
    }
    match &restored[2].content[1] {
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            content_blocks,
            ..
        } => {
            assert_eq!(tool_use_id, "tool-2");
            assert_eq!(content, "two");
            assert_eq!(*is_error, Some(true));
            assert!(content_blocks.is_none());
        }
        other => panic!("expected second tool result, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

/// A resumed session is seeded with one `Utc::now()` shared by every turn and
/// every item. `list_turns_for_thread` sorts on `created_at` and
/// `list_items_for_turn` sorts on `started_at`, so both sorts were a single
/// tie and the surviving order was whatever `read_dir` returned. That order
/// reaches users: `get_thread_detail` feeds the web dashboard's transcript,
/// and `fork_thread`/`fork_at_user_message` rebuild `item_ids` from it, which
/// makes the scramble durable. Seeded records must carry a total order.
#[tokio::test]
async fn seeded_session_records_carry_a_total_order() -> Result<()> {
    let dir = test_runtime_dir();
    let manager = test_manager(dir.clone())?;
    let thread = sample_thread("thr_seed_order");
    manager.store.save_thread(&thread)?;
    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "first question".to_string(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "consider".to_string(),
                    signature: None,
                    state: None,
                    redacted_data: None,
                },
                ContentBlock::ToolUse {
                    execution_id: None,
                    id: "tool-1".to_string(),
                    name: "shell".to_string(),
                    input: json!({ "cmd": "ls" }),
                    caller: None,
                    thought_signature: None,
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "tool-1".to_string(),
                content: "listing".to_string(),
                is_error: None,
                content_blocks: None,
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "first answer".to_string(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "second question".to_string(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "second answer".to_string(),
                cache_control: None,
            }],
        },
    ];

    manager
        .seed_thread_from_messages(&thread.id, &messages)
        .await?;

    let turns = manager.store.list_turns_for_thread(&thread.id)?;
    assert_eq!(turns.len(), 2);
    assert!(
        turns[0].created_at < turns[1].created_at,
        "seeded turns must not tie on the only key list_turns_for_thread sorts by: {:?} vs {:?}",
        turns[0].created_at,
        turns[1].created_at
    );
    assert_eq!(turns[0].input_summary, "first question");
    assert_eq!(turns[1].input_summary, "second question");

    for turn in &turns {
        let items = manager.store.list_items_for_turn(&turn.id)?;
        for pair in items.windows(2) {
            assert!(
                pair[0].started_at < pair[1].started_at,
                "seeded items must not tie on the only key list_items_for_turn sorts by"
            );
        }
        let scanned = items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
        assert_eq!(
            scanned, turn.item_ids,
            "the store scan must reproduce the order the turn recorded"
        );
    }

    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

#[test]
fn current_runtime_schema_version_is_two_on_v066() {
    // Locks the bump in (issue #124). Bump deliberately when persisted
    // shape changes.
    assert_eq!(CURRENT_RUNTIME_SCHEMA_VERSION, 2);
}

#[test]
fn store_rejects_path_like_record_ids() {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone()).expect("open store");

    let err = store
        .load_thread("../outside")
        .expect_err("path traversal id should fail");
    assert!(
        format!("{err:#}").contains("unsupported characters"),
        "got: {err:#}"
    );

    let mut thread = sample_thread("thr_bad/id");
    let err = store
        .save_thread(&thread)
        .expect_err("path separator id should fail");
    assert!(
        format!("{err:#}").contains("unsupported characters"),
        "got: {err:#}"
    );

    thread.id = " thr_bad".to_string();
    let err = store
        .save_thread(&thread)
        .expect_err("whitespace id should fail");
    assert!(format!("{err:#}").contains("whitespace"), "got: {err:#}");

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn enforce_lru_capacity_does_not_loop_when_all_threads_are_active() {
    let mut active = ActiveThreads::default();
    let mut pending_commands = HashSet::new();
    let harness_a = mock_engine_handle();
    let harness_b = mock_engine_handle();

    active.engines.insert(
        "thr_a".to_string(),
        ActiveThreadState {
            engine: harness_a.handle,
            active_turn: Some(ActiveTurnState {
                narrowing: crate::core::engine::TurnNarrowing::Inherit,
                goal_progress: None,
                goal_id: None,
                turn_id: "turn_a".to_string(),
                interrupt_requested: false,
                compaction_id: None,
            }),
            route_identity: codewhale_runtime::config::ProviderIdentity::test_exact_identity(
                ProviderKind::Deepseek,
                "deepseek",
            ),
            route_model: DEFAULT_TEXT_MODEL.to_string(),
            hook_executor: None,
            client_preflight_required: false,
        },
    );
    active.engines.insert(
        "thr_b".to_string(),
        ActiveThreadState {
            engine: harness_b.handle,
            active_turn: Some(ActiveTurnState {
                narrowing: crate::core::engine::TurnNarrowing::Inherit,
                goal_progress: None,
                goal_id: None,
                turn_id: "turn_b".to_string(),
                interrupt_requested: false,
                compaction_id: None,
            }),
            route_identity: codewhale_runtime::config::ProviderIdentity::test_exact_identity(
                ProviderKind::Deepseek,
                "deepseek",
            ),
            route_model: DEFAULT_TEXT_MODEL.to_string(),
            hook_executor: None,
            client_preflight_required: false,
        },
    );
    active.lru.push_back("thr_a".to_string());
    active.lru.push_back("thr_b".to_string());

    let evicted = enforce_lru_capacity(&mut active, 2, &pending_commands);
    assert!(evicted.is_empty(), "no idle threads should be evicted");
    assert_eq!(active.engines.len(), 2);
    assert_eq!(active.lru.len(), 2);
    active.engines.get_mut("thr_a").unwrap().active_turn = None;
    let authority = active.engines["thr_a"].engine.subagent_manager();
    let child = {
        let mut workers = authority.try_write().unwrap();
        let child = workers.insert_test_running_agent("lru_child", &PathBuf::from("."));
        workers.assign_test_session_owner(&child, "thr_a");
        child
    };
    assert!(enforce_lru_capacity(&mut active, 2, &pending_commands).is_empty());
    let _ = authority
        .try_write()
        .unwrap()
        .cancel_agent_for_session("thr_a", &child);
    pending_commands.insert("thr_a".to_string());
    assert!(enforce_lru_capacity(&mut active, 2, &pending_commands).is_empty());
    assert!(active.engines.contains_key("thr_a"));
    pending_commands.clear();
    let evicted = enforce_lru_capacity(&mut active, 2, &pending_commands);
    assert_eq!(evicted.len(), 1);
    assert!(!active.engines.contains_key("thr_a"));
    assert!(active.engines.contains_key("thr_b"));
}

#[test]
fn approval_decision_scenario() {
    // Scenario consolidation of: approval_decision_keeps_trust_mode_out_of_tool_approval, approval_decision_requires_auto_approve_and_trust_for_full_access
    // from approval_decision_keeps_trust_mode_out_of_tool_approval
    {
        assert!(matches!(
            RuntimeThreadManager::approval_decision(false, false, false),
            RuntimeApprovalDecision::DenyTool
        ));
        assert!(matches!(
            RuntimeThreadManager::approval_decision(false, true, false),
            RuntimeApprovalDecision::DenyTool
        ));
        assert!(matches!(
            RuntimeThreadManager::approval_decision(true, false, false),
            RuntimeApprovalDecision::ApproveTool
        ));
        assert!(matches!(
            RuntimeThreadManager::approval_decision(true, false, true),
            RuntimeApprovalDecision::DenyTool
        ));
        assert!(matches!(
            RuntimeThreadManager::approval_decision(true, true, true),
            RuntimeApprovalDecision::RetryWithFullAccess
        ));
    }
    // from approval_decision_requires_auto_approve_and_trust_for_full_access
    {
        assert_eq!(
            RuntimeThreadManager::approval_decision(false, false, false),
            RuntimeApprovalDecision::DenyTool
        );
        assert_eq!(
            RuntimeThreadManager::approval_decision(false, true, false),
            RuntimeApprovalDecision::DenyTool
        );
        assert_eq!(
            RuntimeThreadManager::approval_decision(true, false, false),
            RuntimeApprovalDecision::ApproveTool
        );
        assert_eq!(
            RuntimeThreadManager::approval_decision(true, false, true),
            RuntimeApprovalDecision::DenyTool
        );
        assert_eq!(
            RuntimeThreadManager::approval_decision(true, true, true),
            RuntimeApprovalDecision::RetryWithFullAccess
        );
    }
}

#[test]
fn open_recovers_queued_and_in_progress_turns() -> Result<()> {
    let runtime_dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(runtime_dir.clone())?;
    let thread = sample_thread("thr_recover");
    store.save_thread(&thread)?;

    let mut queued_turn = sample_turn(&thread.id, "turn_queued", RuntimeTurnStatus::Queued);
    let mut in_progress_turn =
        sample_turn(&thread.id, "turn_running", RuntimeTurnStatus::InProgress);
    let completed_turn = sample_turn(&thread.id, "turn_done", RuntimeTurnStatus::Completed);

    let queued_item = sample_item(
        &queued_turn.id,
        "item_queued",
        TurnItemLifecycleStatus::Queued,
    );
    let in_progress_item = sample_item(
        &in_progress_turn.id,
        "item_running",
        TurnItemLifecycleStatus::InProgress,
    );
    let completed_item = sample_item(
        &completed_turn.id,
        "item_done",
        TurnItemLifecycleStatus::Completed,
    );

    queued_turn.item_ids = vec![queued_item.id.clone()];
    in_progress_turn.item_ids = vec![in_progress_item.id.clone()];

    store.save_item(&queued_item)?;
    store.save_item(&in_progress_item)?;
    store.save_item(&completed_item)?;
    store.save_turn(&queued_turn)?;
    store.save_turn(&in_progress_turn)?;
    store.save_turn(&completed_turn)?;

    let manager = test_manager(runtime_dir)?;

    let queued_turn = manager.store.load_turn(&queued_turn.id)?;
    assert_eq!(queued_turn.status, RuntimeTurnStatus::Interrupted);
    assert_eq!(queued_turn.error.as_deref(), Some(RUNTIME_RESTART_REASON));
    assert!(queued_turn.ended_at.is_some());
    assert!(queued_turn.duration_ms.is_some());

    let in_progress_turn = manager.store.load_turn(&in_progress_turn.id)?;
    assert_eq!(in_progress_turn.status, RuntimeTurnStatus::Interrupted);
    assert_eq!(
        in_progress_turn.error.as_deref(),
        Some(RUNTIME_RESTART_REASON)
    );
    assert!(in_progress_turn.ended_at.is_some());
    assert!(in_progress_turn.duration_ms.is_some());

    let completed_turn = manager.store.load_turn(&completed_turn.id)?;
    assert_eq!(completed_turn.status, RuntimeTurnStatus::Completed);
    assert!(completed_turn.error.is_none());

    let queued_item = manager.store.load_item("item_queued")?;
    assert_eq!(queued_item.status, TurnItemLifecycleStatus::Interrupted);
    assert!(queued_item.ended_at.is_some());

    let in_progress_item = manager.store.load_item("item_running")?;
    assert_eq!(
        in_progress_item.status,
        TurnItemLifecycleStatus::Interrupted
    );
    assert!(in_progress_item.ended_at.is_some());

    let completed_item = manager.store.load_item("item_done")?;
    assert_eq!(completed_item.status, TurnItemLifecycleStatus::Completed);

    Ok(())
}

#[tokio::test]
async fn runtime_chat_event_read_flushes_restart_terminal_receipt_once() -> Result<()> {
    let runtime_dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(runtime_dir.clone())?;
    let thread = sample_thread("thr_relay_restart_receipt");
    let turn = sample_turn(
        &thread.id,
        "turn_relay_restart_receipt",
        RuntimeTurnStatus::InProgress,
    );
    store.save_thread(&thread)?;
    store.save_turn(&turn)?;
    drop(store);

    let manager = test_manager(runtime_dir)?;
    let events = manager.events_since_async(&thread.id, None).await?;
    let completed = events
        .iter()
        .filter(|event| {
            event.event == "turn.completed" && event.turn_id.as_deref() == Some(turn.id.as_str())
        })
        .collect::<Vec<_>>();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].payload["recovered"], true);
    assert_eq!(completed[0].payload["turn"]["status"], "interrupted");

    let replay = manager.events_since_async(&thread.id, None).await?;
    assert_eq!(
        replay
            .iter()
            .filter(|event| {
                event.event == "turn.completed"
                    && event.turn_id.as_deref() == Some(turn.id.as_str())
            })
            .count(),
        1,
        "restart recovery must append one stable terminal receipt"
    );
    Ok(())
}

#[tokio::test]
async fn restart_resumes_item_normalization_after_parent_was_already_interrupted() -> Result<()> {
    let runtime_dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(runtime_dir.clone())?;
    let mut thread = sample_thread("thr_second_crash_recovery");
    thread.latest_turn_id = Some("turn_stale_pointer".to_string());
    let mut turn = sample_turn(
        &thread.id,
        "turn_second_crash_recovery",
        RuntimeTurnStatus::Interrupted,
    );
    turn.error = Some(RUNTIME_RESTART_REASON.to_string());
    turn.ended_at = Some(Utc::now());
    let item = sample_item(
        &turn.id,
        "item_second_crash_recovery",
        TurnItemLifecycleStatus::InProgress,
    );
    turn.item_ids = vec![item.id.clone()];
    store.save_thread(&thread)?;
    store.save_item(&item)?;
    store.save_turn(&turn)?;
    drop(store);

    let manager = test_manager(runtime_dir.clone())?;
    assert_eq!(
        manager.store.load_thread(&thread.id)?.latest_turn_id,
        Some(turn.id.clone()),
        "recovery must repair a turn committed before its thread pointer"
    );
    assert_eq!(
        manager.store.load_item(&item.id)?.status,
        TurnItemLifecycleStatus::Interrupted,
        "a second startup must finish item normalization even when the parent was committed first"
    );
    let first = manager.events_since_async(&thread.id, None).await?;
    assert_eq!(
        first
            .iter()
            .filter(|event| {
                event.event == "turn.completed"
                    && event.turn_id.as_deref() == Some(turn.id.as_str())
            })
            .count(),
        1
    );
    drop(manager);

    let reopened = test_manager(runtime_dir)?;
    assert_eq!(
        reopened.store.load_item(&item.id)?.status,
        TurnItemLifecycleStatus::Interrupted
    );
    let replay = reopened.events_since_async(&thread.id, None).await?;
    assert_eq!(
        replay
            .iter()
            .filter(|event| {
                event.event == "turn.completed"
                    && event.turn_id.as_deref() == Some(turn.id.as_str())
            })
            .count(),
        1,
        "another reopen must not duplicate the recovered terminal receipt"
    );
    Ok(())
}

#[tokio::test]
async fn thread_lifecycle_persists_across_restart() -> Result<()> {
    let runtime_dir = test_runtime_dir();
    let manager = test_manager(runtime_dir.clone())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    tokio::spawn(async move {
        if matches!(rx_op.recv().await, Some(Op::SendMessage(TurnSpec { .. }))) {
            let _ = tx_event
                .send(EngineEvent::TurnStarted {
                    turn_id: "engine_turn_1".to_string(),
                    created_at: chrono::Utc::now(),
                    route: None,
                    submission_id: None,
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::MessageStarted { index: 0 })
                .await;
            let _ = tx_event
                .send(EngineEvent::MessageDelta {
                    index: 0,
                    content: "mock response".to_string(),
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::MessageComplete { index: 0 })
                .await;
            let _ = tx_event
                .send(EngineEvent::TurnComplete {
                    turn_id: None,
                    submission_id: None,
                    compaction_id: None,
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 12,
                        ..Usage::default()
                    },
                    parent_route_usage: Usage {
                        input_tokens: 10,
                        output_tokens: 12,
                        ..Usage::default()
                    },
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Completed,
                    error: None,
                    tool_catalog: None,
                    base_url: None,
                })
                .await;
        }
    });

    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "first prompt".to_string(),
                input_summary: None,
                model: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                ..Default::default()
            },
        )
        .await?;
    let completed = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(completed.status, RuntimeTurnStatus::Completed);

    drop(manager);

    let reopened = test_manager(runtime_dir)?;
    let detail = reopened.get_thread_detail(&thread.id).await?;
    assert_eq!(detail.thread.id, thread.id);
    assert_eq!(detail.turns.len(), 1);
    assert!(detail.latest_seq >= 1);
    assert!(!detail.items.is_empty());
    let events = reopened.events_since(&thread.id, None)?;
    assert!(
        events.iter().any(|ev| ev.event == "turn.completed"),
        "expected turn.completed event after restart"
    );
    Ok(())
}

#[tokio::test]
async fn initial_classifier_usage_is_persisted_before_terminal_and_merged_exactly_once()
-> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "persist classifier receipt".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));

    let classifier_usage = Usage {
        input_tokens: 7,
        output_tokens: 5,
        ..Usage::default()
    };
    let classifier_route = crate::cost_status::EffectiveRouteEnvelope::capture(
        None,
        ProviderKind::Openai,
        "openai",
        "classifier-model",
        Some(ProviderKind::Openai.provider().default_base_url()),
        Utc::now(),
    );
    let classifier_batch = crate::cost_status::RuntimeUsageBatch {
        decisions: Vec::new(),
        records: vec![crate::cost_status::RuntimeUsageRecord {
            source_id: "auto-router:runtime-fixture".to_string(),
            usage: crate::cost_status::EffectiveRouteUsage {
                route: classifier_route.clone(),
                usage: classifier_usage.clone(),
            },
        }],
        drop_records: vec![crate::cost_status::RuntimeUsageDropRecord {
            reason: crate::cost_status::RuntimeUsageMissingReason::default(),
            source_id: "auto-router:runtime-missing-usage".to_string(),
            route: classifier_route,
        }],
        // One exact drop plus two residual/unidentifiable gaps.
        dropped_records: 3,
    };
    {
        let _turn_mutation = manager.store.turn_mutation.lock();
        let mut reserved = manager.store.load_turn(&turn.id)?;
        append_initial_routed_usage_to_turn(&mut reserved, &classifier_batch);
        append_initial_routed_usage_to_turn(&mut reserved, &classifier_batch);
        manager.store.save_turn(&reserved)?;
    }
    let preterminal = manager.store.load_turn(&turn.id)?;
    assert_eq!(preterminal.routed_usage.len(), 1);
    assert_eq!(preterminal.routed_usage_drop_records.len(), 1);
    assert_eq!(preterminal.routed_usage_drop_records[0].source_id.len(), 64);
    assert!(
        !preterminal.routed_usage_drop_records[0]
            .source_id
            .contains("runtime-missing-usage")
    );
    assert_eq!(preterminal.routed_usage[0].usage, classifier_usage);
    assert_eq!(preterminal.routed_usage_source_ids.len(), 2);
    assert!(
        preterminal
            .routed_usage_source_ids
            .contains(&preterminal.routed_usage_drop_records[0].source_id),
        "the persisted exact-drop receipt must retain the same durable source fingerprint as the dedupe ledger"
    );
    assert_eq!(
        preterminal.routed_usage_dropped_records, 0,
        "TurnComplete, not pre-persistence, owns classifier drop coverage"
    );
    {
        let _turn_mutation = manager.store.turn_mutation.lock();
        let mut replayed = manager.store.load_turn(&turn.id)?;
        append_initial_routed_usage_to_turn(&mut replayed, &classifier_batch);
        manager.store.save_turn(&replayed)?;
    }
    let replayed = manager.store.load_turn(&turn.id)?;
    assert_eq!(replayed.routed_usage.len(), 1);
    assert_eq!(replayed.routed_usage_drop_records.len(), 1);
    assert_eq!(replayed.routed_usage_source_ids.len(), 2);

    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "engine_classifier_receipt".to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage {
                input_tokens: 18,
                output_tokens: 8,
                ..Usage::default()
            },
            parent_route_usage: Usage {
                input_tokens: 11,
                output_tokens: 3,
                ..Usage::default()
            },
            routed_usage_dropped_records: classifier_batch.dropped_records.saturating_sub(
                u64::try_from(classifier_batch.drop_records.len()).unwrap_or(u64::MAX),
            ),
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;

    let completed = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(completed.routed_usage.len(), 1);
    assert_eq!(completed.routed_usage_drop_records.len(), 1);
    assert_eq!(completed.routed_usage_source_ids.len(), 2);
    assert_eq!(completed.routed_usage[0].usage, classifier_usage);
    assert_eq!(completed.routed_usage_dropped_records, 2);
    assert_eq!(
        completed.usage,
        Some(Usage {
            input_tokens: 18,
            output_tokens: 8,
            ..Usage::default()
        })
    );
    assert_eq!(
        completed.effective_route_usage,
        Some(Usage {
            input_tokens: 11,
            output_tokens: 3,
            ..Usage::default()
        })
    );
    let aggregate = manager
        .aggregate_usage(None, None, UsageGroupBy::Thread)
        .await?;
    assert_eq!(aggregate.totals.dropped_usage_records, 3);
    assert!(
        aggregate.totals.route_receipts.iter().any(
            |receipt| receipt.contains("classifier-model") && receipt.contains("usage=missing")
        )
    );
    Ok(())
}

/// One completed auxiliary provider call, on its own frozen route, as the
/// classifier hands it to `start_turn` before the parent route is resolved.
fn classifier_settlement_batch(
    identity: &str,
    model: &str,
    source_prefix: &str,
    dropped_records: u64,
) -> crate::cost_status::RuntimeUsageBatch {
    let mut route = crate::cost_status::EffectiveRouteEnvelope::capture(
        None,
        ProviderKind::Custom,
        identity,
        model,
        Some(ProviderKind::Openrouter.provider().default_base_url()),
        Utc::now(),
    );
    route.billing_mode = crate::cost_status::RouteBillingMode::Subscription;
    crate::cost_status::RuntimeUsageBatch {
        decisions: Vec::new(),
        records: vec![crate::cost_status::RuntimeUsageRecord {
            source_id: format!("auto-router:{source_prefix}-usage"),
            usage: crate::cost_status::EffectiveRouteUsage {
                route: route.clone(),
                usage: Usage {
                    input_tokens: 9,
                    output_tokens: 4,
                    ..Usage::default()
                },
            },
        }],
        drop_records: vec![crate::cost_status::RuntimeUsageDropRecord {
            reason: crate::cost_status::RuntimeUsageMissingReason::default(),
            source_id: format!("auto-router:{source_prefix}-drop"),
            route,
        }],
        dropped_records,
    }
}

/// A classifier call that completed before the parent route failed must land
/// in the Runtime store, not in ownerless in-process accounting: headless
/// Runtime/API execution has no foreground session draining that pool, so a
/// process loss erased real provider spend. Settling the same completed call
/// again must not charge a second time, and the failed parent route must never
/// become the billing route.
#[tokio::test]
async fn failed_runtime_parent_route_resolve_and_preflight_settle_batch_once_without_repricing()
-> Result<()> {
    let _cost_scope = crate::cost_status::test_scope();
    let scope = crate::cost_status::scope_token();
    let runtime_dir = test_runtime_dir();
    let manager = test_manager(runtime_dir.clone())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    // One exact missing-usage route plus two residual coverage gaps.
    let batch = classifier_settlement_batch(
        "classifier-openrouter",
        "classifier/frozen-route",
        "runtime-parent-failure",
        3,
    );
    let frozen_batch = batch.clone();

    for error_message in [
        "selected parent provider/model is invalid",
        "selected parent route failed preflight",
    ] {
        let error = {
            let _settlement = InitialRoutedUsageSettlementGuard::new(
                manager.store.clone(),
                &thread.id,
                scope,
                &batch,
            );
            anyhow::anyhow!(error_message)
        };
        assert_eq!(
            error.to_string(),
            error_message,
            "settlement must preserve the original route failure"
        );
    }

    assert_eq!(
        batch, frozen_batch,
        "settlement must not coerce classifier receipts onto the failed parent provider/model"
    );

    let turns = manager.store.list_turns_for_thread(&thread.id)?;
    assert_eq!(
        turns.len(),
        1,
        "settling the same completed call twice must reuse one durable record: {turns:?}"
    );
    let settled = &turns[0];
    assert_eq!(settled.status, RuntimeTurnStatus::Failed);
    assert_eq!(settled.usage, None);
    assert_eq!(settled.effective_route_usage, None);
    assert_eq!(settled.effective_provider, None);
    assert_eq!(settled.effective_model, None);
    assert!(
        settled.effective_route_envelope().is_none(),
        "the parent route never dispatched and must never be charged"
    );
    assert_eq!(settled.routed_usage.len(), 1);
    assert_eq!(
        settled.routed_usage[0].route.model,
        "classifier/frozen-route"
    );
    assert_eq!(
        settled.routed_usage[0].usage,
        Usage {
            input_tokens: 9,
            output_tokens: 4,
            ..Usage::default()
        }
    );
    assert_eq!(settled.routed_usage_drop_records.len(), 1);
    assert_eq!(settled.routed_usage_drop_records[0].source_id.len(), 64);
    assert_eq!(settled.routed_usage_source_ids.len(), 2);
    assert_eq!(
        settled.routed_usage_dropped_records, 2,
        "no engine TurnComplete will ever arrive, so this record owns its residual gap"
    );
    assert!(
        crate::cost_status::drain().is_empty(),
        "runtime spend must not be charged to whichever session happens to be live"
    );

    // Process loss: a fresh manager over the same store still prices the call
    // exactly once, under the classifier's own frozen route.
    drop(manager);
    let reopened = test_manager(runtime_dir)?;
    let aggregate = reopened
        .aggregate_usage(None, None, UsageGroupBy::Provider)
        .await?;
    assert_eq!(aggregate.totals.input_tokens, 9);
    assert_eq!(aggregate.totals.output_tokens, 4);
    assert_eq!(aggregate.totals.dropped_usage_records, 3);
    assert_eq!(
        aggregate
            .buckets
            .iter()
            .map(|bucket| bucket.key.as_str())
            .collect::<Vec<_>>(),
        vec!["classifier-openrouter", "unknown-truncated"],
        "exact receipts bill the classifier identity; only the residual gap is unattributed"
    );
    assert!(aggregate.totals.route_receipts.iter().any(|receipt| {
        receipt.contains("identity=classifier-openrouter")
            && receipt.contains("model=classifier/frozen-route")
    }));
    assert!(
        aggregate
            .totals
            .route_receipts
            .iter()
            .all(|receipt| !receipt.contains("selected_parent")),
        "the failed parent route must never become the classifier billing route"
    );

    // Restart recovery treats the record as the terminal turn it is: it emits
    // the one missing receipt and re-prices nothing.
    let recovered = reopened.get_thread(&thread.id).await?;
    assert!(
        recovered.latest_turn_id.is_none(),
        "accounting-only settlement must not invent an accepted parent turn on restart"
    );
    assert_eq!(
        serde_json::to_value(
            reopened
                .aggregate_usage(None, None, UsageGroupBy::Provider)
                .await?
                .totals
        )?,
        serde_json::to_value(aggregate.totals)?,
        "flushing the recovery receipt must not count the call again"
    );
    Ok(())
}

#[tokio::test]
async fn routing_settlement_preserves_latest_accepted_turn_after_restart_and_fork() -> Result<()> {
    let runtime_dir = test_runtime_dir();
    let manager = test_manager(runtime_dir.clone())?;
    let mut thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut accepted = sample_turn(&thread.id, "turn_accepted", RuntimeTurnStatus::Completed);
    accepted.created_at = Utc::now() - chrono::Duration::minutes(1);
    manager.store.save_turn(&accepted)?;
    thread.latest_turn_id = Some(accepted.id.clone());
    manager.store.save_thread(&thread)?;
    let batch = classifier_settlement_batch("classifier", "classifier/model", "newer", 1);
    let settlement_id = settle_unaccepted_routed_usage(&manager.store, &thread.id, &batch)?;
    assert!(manager.store.load_turn(&settlement_id)?.routing_settlement);
    drop(manager);

    let reopened = test_manager(runtime_dir)?;
    assert_eq!(
        reopened
            .get_thread(&thread.id)
            .await?
            .latest_turn_id
            .as_deref(),
        Some(accepted.id.as_str()),
    );
    assert_eq!(reopened.store.list_turns_for_thread(&thread.id)?.len(), 2);
    let forked = reopened.fork_thread(&thread.id).await?;
    let latest_id = forked
        .latest_turn_id
        .context("fork retains its accepted turn")?;
    assert!(!reopened.store.load_turn(&latest_id)?.routing_settlement);
    Ok(())
}

/// `/new` and session load close the cost scope captured at dispatch, and
/// ownerless in-process settlement rejects a stale scope outright — which is
/// how a real classifier call vanished. The durable record must not depend on
/// the scope, and must not charge the replacement scope either.
#[tokio::test]
async fn unaccepted_classifier_settlement_survives_a_closed_cost_scope() -> Result<()> {
    let _cost_scope = crate::cost_status::test_scope();
    let dispatch_scope = crate::cost_status::scope_token();
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let batch = classifier_settlement_batch(
        "stale-scope-openrouter",
        "classifier/stale-scope",
        "stale-scope",
        1,
    );

    let _closed = crate::cost_status::close_current_scope();
    assert_ne!(dispatch_scope, crate::cost_status::scope_token());
    drop(InitialRoutedUsageSettlementGuard::new(
        manager.store.clone(),
        &thread.id,
        dispatch_scope,
        &batch,
    ));

    let turns = manager.store.list_turns_for_thread(&thread.id)?;
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].routed_usage.len(), 1);
    assert_eq!(
        turns[0].routed_usage[0].route.model,
        "classifier/stale-scope"
    );
    assert_eq!(turns[0].routed_usage_drop_records.len(), 1);
    assert_eq!(turns[0].routed_usage_dropped_records, 0);
    assert!(
        crate::cost_status::drain().is_empty(),
        "the replacement scope must not inherit the closed scope's spend"
    );
    Ok(())
}

/// An exact operation-key retry replays the original turn, but its own
/// classifier call really happened. It settles into its own record, and the
/// replayed turn stays exactly as its owner left it.
#[tokio::test]
async fn raced_runtime_operation_replay_settles_second_classifier_without_mutating_original_turn()
-> Result<()> {
    let _cost_scope = crate::cost_status::test_scope();
    let scope = crate::cost_status::scope_token();
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let original_turn = sample_turn(
        &thread.id,
        "turn_original_operation",
        RuntimeTurnStatus::Completed,
    );
    manager.store.save_turn(&original_turn)?;
    let second_classifier_batch = classifier_settlement_batch(
        "second-classifier-openrouter",
        "classifier/second-actual-call",
        "raced-replay-second-call",
        1,
    );

    let replayed_turn = {
        let _settlement = InitialRoutedUsageSettlementGuard::new(
            manager.store.clone(),
            &thread.id,
            scope,
            &second_classifier_batch,
        );
        manager.store.load_turn(&original_turn.id)?
    };
    assert_eq!(replayed_turn.id, original_turn.id);
    assert_eq!(replayed_turn.status, original_turn.status);
    assert!(replayed_turn.routed_usage.is_empty());
    assert!(replayed_turn.routed_usage_drop_records.is_empty());

    let persisted_original = manager.store.load_turn(&original_turn.id)?;
    assert!(persisted_original.routed_usage.is_empty());
    assert!(persisted_original.routed_usage_drop_records.is_empty());
    assert_eq!(persisted_original.routed_usage_dropped_records, 0);

    let settled = manager
        .store
        .list_turns_for_thread(&thread.id)?
        .into_iter()
        .filter(|turn| turn.id != original_turn.id)
        .collect::<Vec<_>>();
    assert_eq!(settled.len(), 1);
    assert_eq!(settled[0].routed_usage.len(), 1);
    assert_eq!(
        settled[0].routed_usage[0].route.model,
        "classifier/second-actual-call"
    );
    assert_eq!(settled[0].routed_usage_drop_records.len(), 1);
    Ok(())
}

/// Auto routing that reported nothing has nothing to settle. Minting a record
/// anyway would put a phantom turn on every thread whose parent route failed.
#[tokio::test]
async fn pre_turn_settlement_with_nothing_to_keep_writes_no_record() -> Result<()> {
    let _cost_scope = crate::cost_status::test_scope();
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    drop(InitialRoutedUsageSettlementGuard::new(
        manager.store.clone(),
        &thread.id,
        crate::cost_status::scope_token(),
        &crate::cost_status::RuntimeUsageBatch::default(),
    ));
    assert!(
        manager.store.list_turns_for_thread(&thread.id)?.is_empty(),
        "a classifier that reported nothing must not mint a phantom turn"
    );
    Ok(())
}

#[tokio::test]
async fn terminal_settlement_preserves_late_sink_receipts_during_pending_request_cleanup()
-> Result<()> {
    let _cost_scope = crate::cost_status::test_scope();
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "settle late child accounting".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    let lease = crate::cost_status::acquire_runtime_usage_lease(&turn.id)
        .context("admitted turn installs a persistent sink")?;
    manager.register_pending_user_input(
        &thread.id,
        PendingUserInputRequest {
            id: "input_late_settlement".to_string(),
            turn_id: turn.id.clone(),
            request: crate::tools::user_input::UserInputRequest {
                require_typed_answers: false,
                questions: Vec::new(),
            },
        },
    );

    // Stop the real cancellation append after it claims the pending input.
    // This is the await at which the former monitor retained a stale turn.
    let emit_guard = manager.event_emit.lock().await;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage {
                input_tokens: 8,
                output_tokens: 5,
                ..Usage::default()
            },
            parent_route_usage: Usage {
                input_tokens: 8,
                output_tokens: 5,
                ..Usage::default()
            },
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Failed,
            error: Some("provider failed".to_string()),
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    tokio::time::timeout(TURN_SETTLEMENT_DEADLOCK_TIMEOUT, async {
        loop {
            if manager
                .pending_user_inputs
                .lock()
                .get(&(thread.id.clone(), "input_late_settlement".to_string()))
                .is_some_and(|entry| entry.settling)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("terminal cleanup did not reach its blocked append")?;
    let route = crate::cost_status::EffectiveRouteEnvelope::capture(
        None,
        ProviderKind::Deepseek,
        "deepseek",
        "deepseek-v4-flash",
        Some(ProviderKind::Deepseek.provider().default_base_url()),
        Utc::now(),
    );
    crate::cost_status::report_effective_route_for_runtime(
        crate::cost_status::scope_token(),
        Some(&turn.id),
        "late-success-during-cleanup",
        &route,
        &Usage {
            input_tokens: 27,
            output_tokens: 4,
            ..Usage::default()
        },
    );
    crate::cost_status::report_unreceipted_provider_success(
        crate::cost_status::scope_token(),
        Some(&turn.id),
        "late-missing-during-cleanup",
        &route,
    );
    let persisted_during_cleanup = manager.store.load_turn(&turn.id)?;
    assert_eq!(persisted_during_cleanup.routed_usage.len(), 1);
    assert_eq!(persisted_during_cleanup.routed_usage_drop_records.len(), 1);
    assert_eq!(
        persisted_during_cleanup.status,
        RuntimeTurnStatus::InProgress
    );
    drop(emit_guard);

    let completed = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(completed.status, RuntimeTurnStatus::Failed);
    assert_eq!(
        completed.routed_usage,
        persisted_during_cleanup.routed_usage
    );
    assert_eq!(
        completed.routed_usage_drop_records,
        persisted_during_cleanup.routed_usage_drop_records
    );
    assert_eq!(completed.routed_usage_source_ids.len(), 2);
    assert_eq!(
        completed.usage.as_ref().map(|usage| usage.input_tokens),
        Some(8)
    );
    let totals = manager
        .aggregate_usage(None, None, UsageGroupBy::Thread)
        .await?
        .totals;
    assert_eq!(totals.input_tokens, 35);
    assert_eq!(totals.output_tokens, 9);
    assert_eq!(totals.dropped_usage_records, 1);
    drop(lease);
    Ok(())
}

#[tokio::test]
async fn monitor_deduplicates_sink_and_metadata_and_persists_metadata_only_missing_usage()
-> Result<()> {
    let _cost_scope = crate::cost_status::test_scope();
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "account for every completed child response".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    let mut batch =
        classifier_settlement_batch("child-route", "child/model", "sink-and-metadata", 1);
    crate::cost_status::report_runtime_usage_batch(
        crate::cost_status::scope_token(),
        Some(&turn.id),
        &batch,
    );
    batch
        .drop_records
        .push(crate::cost_status::RuntimeUsageDropRecord {
            reason: crate::cost_status::RuntimeUsageMissingReason::default(),
            source_id: "metadata-only-missing-response".to_string(),
            route: batch.records[0].usage.route.clone(),
        });
    batch.dropped_records = 2;
    let mut metadata = json!({});
    crate::cost_status::attach_child_usage_batch_metadata(&mut metadata, &batch);
    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "engine_metadata_receipt".to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    for _ in 0..2 {
        harness
            .tx_event
            .send(EngineEvent::ToolCallComplete {
                model_call: None,
                id: "tool-child-usage".to_string(),
                name: "rlm".to_string(),
                result: Ok(
                    crate::tools::spec::ToolResult::success("done").with_metadata(metadata.clone())
                ),
            })
            .await?;
    }
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: batch.records[0].usage.usage.clone(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    let completed = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(
        completed.routed_usage.len(),
        1,
        "metadata and direct sink describe one normal call"
    );
    assert_eq!(
        completed.routed_usage_drop_records.len(),
        2,
        "retain both missing-usage calls exactly once"
    );
    assert_eq!(completed.routed_usage_source_ids.len(), 3);
    assert_eq!(
        completed.routed_usage_dropped_records, 0,
        "all missing calls have exact identities"
    );
    let totals = manager
        .aggregate_usage(None, None, UsageGroupBy::Thread)
        .await?
        .totals;
    assert_eq!(totals.input_tokens, 9);
    assert_eq!(totals.output_tokens, 4);
    assert_eq!(totals.dropped_usage_records, 2);
    Ok(())
}

#[tokio::test]
async fn monitor_separates_lifecycle_start_from_billing_dispatch_and_child_usage() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "capture immutable route".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));

    let started_at = Utc::now() - chrono::Duration::minutes(5);
    let dispatched_at = Utc::now() - chrono::Duration::seconds(2);
    let endpoint_fingerprint = "d".repeat(64);
    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "engine_route_receipt".to_string(),
            created_at: started_at,
            route: None,
            submission_id: None,
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::RouteDispatched {
            turn_id: "engine_route_receipt".to_string(),
            route: crate::core::events::TurnRoute {
                provider: ProviderKind::Stepfun,
                provider_identity: "stepfun".to_string(),
                model: "step-3.7-flash".to_string(),
                auto_model: false,
                receipt: None,
                billing: Some(crate::core::events::RouteBillingEnvelope {
                    openrouter_vendor: None,
                    billing_surface: Some(crate::pricing::STEPFUN_PAYG_BILLING_SURFACE.to_string()),
                    endpoint_fingerprint: Some(endpoint_fingerprint.clone()),
                    provider_live_pricing: None,
                    billing_mode: crate::cost_status::RouteBillingMode::Metered,
                    dispatched_at,
                }),
                base_url: ProviderKind::Stepfun
                    .provider()
                    .default_base_url()
                    .to_string(),
                billing_product: crate::route_billing::RouteProduct::Unproven,
            },
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::SubAgentMailbox {
            owner_session_id: thread.id.clone(),
            turn_id: "engine_route_receipt".to_string(),
            seq: 77,
            message: crate::tools::subagent::MailboxMessage::TokenUsage {
                agent_id: "agent_child".to_string(),
                source_id: "response-child".to_string(),
                route: Box::new(crate::cost_status::EffectiveRouteEnvelope {
                    openrouter_vendor: None,
                    provider: ProviderKind::OpenaiCodex,
                    provider_identity: "openai-codex".to_string(),
                    model: "gpt-5.5".to_string(),
                    billing_surface: Some(
                        crate::pricing::OAUTH_SUBSCRIPTION_BILLING_SURFACE.to_string(),
                    ),
                    endpoint_fingerprint: None,
                    provider_live_pricing: None,
                    billing_mode: crate::cost_status::RouteBillingMode::Subscription,
                    dispatched_at,
                }),
                usage: Usage {
                    input_tokens: 3,
                    output_tokens: 2,
                    reasoning_tokens: Some(2),
                    ..Usage::default()
                },
            },
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ToolCallStarted {
            model_call: Some(crate::core::events::ModelToolCall {
                provider_id: "tool-routed-coverage".to_string(),
                caller: None,
                thought_signature: None,
            }),
            id: "tool-routed-coverage".to_string(),
            name: "rlm".to_string(),
            input: json!({"action": "eval"}),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ToolCallComplete {
            model_call: None,
            id: "tool-routed-coverage".to_string(),
            name: "rlm".to_string(),
            result: Ok(
                crate::tools::spec::ToolResult::success("covered").with_metadata(json!({
                    "child_usage_records": [],
                    "child_usage_dropped_records": 2,
                })),
            ),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 4,
                reasoning_replay_tokens: Some(6),
                server_tool_use: Some(codewhale_models::ServerToolUsage {
                    code_execution_requests: Some(2),
                    tool_search_requests: Some(3),
                }),
                ..Usage::default()
            },
            parent_route_usage: Usage {
                input_tokens: 10,
                output_tokens: 4,
                reasoning_replay_tokens: Some(6),
                server_tool_use: Some(codewhale_models::ServerToolUsage {
                    code_execution_requests: Some(2),
                    tool_search_requests: Some(3),
                }),
                ..Usage::default()
            },
            // The engine already folded the synchronous tool metadata into
            // this authoritative turn count. Runtime must not persist the
            // ToolCallComplete copy and then add it again here.
            routed_usage_dropped_records: 2,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            // Completion-time endpoint evidence must be ignored.
            base_url: Some("https://api.stepfun.com/v1/coding".to_string()),
        })
        .await?;

    let completed = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(completed.effective_provider.as_deref(), Some("stepfun"));
    assert_eq!(completed.effective_provider_id.as_deref(), Some("stepfun"));
    assert_eq!(
        completed.effective_billing_surface.as_deref(),
        Some(crate::pricing::STEPFUN_PAYG_BILLING_SURFACE)
    );
    assert_eq!(
        completed.effective_endpoint_fingerprint.as_deref(),
        Some(endpoint_fingerprint.as_str())
    );
    assert_eq!(
        completed.effective_billing_mode,
        Some(crate::cost_status::RouteBillingMode::Metered)
    );
    assert_eq!(completed.effective_dispatched_at, Some(dispatched_at));
    assert_eq!(completed.started_at, Some(started_at));
    assert_eq!(completed.routed_usage.len(), 1);
    assert_eq!(completed.routed_usage_dropped_records, 2);
    assert_eq!(completed.routed_usage[0].usage.reasoning_tokens, Some(2));
    let persisted_usage = completed.usage.expect("parent usage");
    assert_eq!(persisted_usage.reasoning_replay_tokens, Some(6));
    assert_eq!(
        persisted_usage.server_tool_use,
        Some(codewhale_models::ServerToolUsage {
            code_execution_requests: Some(2),
            tool_search_requests: Some(3),
        })
    );

    // A new engine mailbox starts at the same sequence number. Runtime
    // dedupe is scoped by the engine turn identity, so the second receipt is
    // persisted exactly once instead of colliding with the prior turn.
    let second = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "capture second mailbox".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    let second_engine_turn = "engine_route_receipt_second";
    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: second_engine_turn.to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::SubAgentMailbox {
            owner_session_id: thread.id.clone(),
            turn_id: second_engine_turn.to_string(),
            seq: 77,
            message: crate::tools::subagent::MailboxMessage::TokenUsage {
                agent_id: "agent-child-second".to_string(),
                source_id: "response-child-second".to_string(),
                route: Box::new(crate::cost_status::EffectiveRouteEnvelope {
                    openrouter_vendor: None,
                    provider: ProviderKind::OpenaiCodex,
                    provider_identity: "openai-codex".to_string(),
                    model: "gpt-5.5".to_string(),
                    billing_surface: Some(
                        crate::pricing::OAUTH_SUBSCRIPTION_BILLING_SURFACE.to_string(),
                    ),
                    endpoint_fingerprint: None,
                    provider_live_pricing: None,
                    billing_mode: crate::cost_status::RouteBillingMode::Subscription,
                    dispatched_at: Utc::now(),
                }),
                usage: Usage {
                    input_tokens: 5,
                    output_tokens: 1,
                    ..Usage::default()
                },
            },
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    let second = wait_for_terminal_turn(&manager, &second.id).await?;
    assert_eq!(second.routed_usage.len(), 1);
    assert_eq!(second.routed_usage[0].usage.input_tokens, 5);
    Ok(())
}

#[tokio::test]
async fn monitor_persists_request_snapshots_and_matching_terminal_diagnostics() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let first = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "request diagnostics fixture".to_string(),
                max_output_tokens: std::num::NonZeroU32::new(256),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));

    let engine_turn_id = "engine_request_diagnostics_first";
    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: engine_turn_id.to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ToolCallHeartbeat)
        .await?;
    for (span_id, activity_kind, action_id) in [
        (
            "outer-private-call",
            codewhale_protocol::engine_owner::OwnerActivityKind::Executing,
            "execute_tools",
        ),
        (
            "nested-private-call",
            codewhale_protocol::engine_owner::OwnerActivityKind::Reading,
            "read_file",
        ),
    ] {
        harness
            .tx_event
            .send(EngineEvent::OperationActivityStarted {
                span_id: span_id.to_string(),
                activity_kind,
                action_id: Some(action_id.to_string()),
            })
            .await?;
    }
    harness
        .tx_event
        .send(EngineEvent::ToolCallHeartbeat)
        .await?;
    let active_detail = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let detail = manager.get_thread_detail(&thread.id).await?;
            if detail.turns.iter().any(|turn| {
                turn.operation_activity
                    .as_ref()
                    .is_some_and(|activity| activity.active.len() == 2)
            }) {
                break Ok::<_, anyhow::Error>(detail);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    let active = active_detail
        .turns
        .iter()
        .find(|turn| turn.id == first.id)
        .unwrap()
        .operation_activity
        .as_ref()
        .unwrap();
    assert!(!active.overflowed);
    assert_eq!(active.active[1].action_id.as_deref(), Some("read_file"));
    assert!(
        active
            .active
            .iter()
            .all(|span| span.span_id.starts_with("operation:")
                && span.span_id.len() == 74
                && span.started_at <= active.observed_at)
    );
    assert!(!serde_json::to_string(active)?.contains("private-call"));
    assert!(
        manager
            .events_since(&thread.id, Some(active_detail.latest_seq))?
            .iter()
            .all(|event| event.event != "operation.activity_started")
    );
    for (span_id, activity_kind, action_id) in [
        (
            "nested-private-call",
            codewhale_protocol::engine_owner::OwnerActivityKind::Reading,
            "read_file",
        ),
        (
            "outer-private-call",
            codewhale_protocol::engine_owner::OwnerActivityKind::Executing,
            "execute_tools",
        ),
    ] {
        harness
            .tx_event
            .send(EngineEvent::OperationActivityCompleted {
                span_id: span_id.to_string(),
                activity_kind,
                action_id: Some(action_id.to_string()),
                outcome: codewhale_protocol::engine_owner::OwnerOperationOutcome::Succeeded,
            })
            .await?;
    }
    harness
        .tx_event
        .send(EngineEvent::OperationActivityStarted {
            span_id: "terminal-interruption".to_string(),
            activity_kind: codewhale_protocol::engine_owner::OwnerActivityKind::Tool,
            action_id: Some("mcp_example_probe".to_string()),
        })
        .await?;
    let mut tool = catalog_tool("mcp_computer_get_app_state");
    tool.description = "Inspect app; api_key=sk-fixture-private-value".to_string();
    let pre_request = crate::tool_inspection::ToolInspectionSnapshot::from_prepared_request(
        engine_turn_id,
        0,
        Some(&[tool]),
    );
    let request_digest = pre_request.active_tool_catalog_sha256.clone();
    harness
        .tx_event
        .send(EngineEvent::ToolRequestSnapshot {
            snapshot: pre_request,
        })
        .await?;
    for status in ["preparing", "waiting", "streaming", "settling"] {
        harness.tx_event.send(EngineEvent::status(status)).await?;
    }
    harness
        .tx_event
        .send(EngineEvent::MessageStarted { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageComplete { index: 0 })
        .await?;
    let mut foreign_terminal =
        crate::tool_inspection::ToolInspectionSnapshot::from_prepared_request(
            "engine_request_diagnostics_foreign",
            1,
            None,
        );
    foreign_terminal.terminal = Some(crate::tool_inspection::TurnStopDiagnostics {
        model_requests_started: 99,
        ..Default::default()
    });
    harness
        .tx_event
        .send(EngineEvent::ToolRequestSnapshot {
            snapshot: foreign_terminal,
        })
        .await?;
    let mut terminal_request =
        crate::tool_inspection::ToolInspectionSnapshot::from_prepared_request(
            engine_turn_id,
            1,
            None,
        );
    terminal_request.terminal = Some(crate::tool_inspection::TurnStopDiagnostics {
        model_requests_started: 2,
        transport_retries: 3,
        transparent_stream_retries: 1,
        stream_resumes: 1,
        reasoning_only_reprompts: 2,
        empty_stop_retries: 1,
        ..Default::default()
    });
    harness
        .tx_event
        .send(EngineEvent::ToolRequestSnapshot {
            snapshot: terminal_request,
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;

    let completed = wait_for_terminal_turn(&manager, &first.id).await?;
    let operation_activity = completed
        .operation_activity
        .as_ref()
        .expect("admitted operation snapshot");
    assert!(operation_activity.active.is_empty());
    assert_eq!(
        operation_activity
            .last_completed
            .as_ref()
            .unwrap()
            .action_id
            .as_deref(),
        Some("execute_tools")
    );
    let operations = manager
        .events_since(&thread.id, None)?
        .into_iter()
        .filter(|event| event.event.starts_with("operation."))
        .collect::<Vec<_>>();
    assert_eq!(
        operations
            .iter()
            .map(|event| event.event.as_str())
            .collect::<Vec<_>>(),
        [
            "operation.activity_started",
            "operation.activity_started",
            "operation.heartbeat",
            "operation.activity_completed",
            "operation.activity_completed",
            "operation.activity_started"
        ]
    );
    assert!(operations.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    assert_eq!(
        operations[0].payload["span_id"],
        operations[4].payload["span_id"]
    );
    assert_eq!(
        operations[1].payload["span_id"],
        operations[3].payload["span_id"]
    );
    assert_eq!(operations[3].payload["outcome"], "succeeded");
    assert!(
        operations
            .iter()
            .all(|event| event.turn_id.as_deref() == Some(first.id.as_str())
                && event.payload["observed_at"].is_string())
    );
    assert!(operations[3].seq > active_detail.latest_seq);
    assert_eq!(
        completed.model_request_diagnostics,
        Some(RuntimeTurnRequestDiagnostics {
            stop_reason: None,
            model_requests_started: 2,
            transport_retries: 3,
            transparent_stream_retries: 1,
            stream_resumes: 1,
            reasoning_only_reprompts: 2,
            empty_stop_retries: 1,
        }),
        "terminal model-client facts must be kept distinct from status items"
    );
    assert_eq!(
        completed.schema_version, OUTPUT_LIMIT_RUNTIME_SCHEMA_VERSION,
        "optional diagnostics must preserve the existing v4 output-limit schema"
    );
    let status_items = manager
        .store
        .list_items_for_turn(&first.id)?
        .into_iter()
        .filter(|item| item.kind == TurnItemKind::Status)
        .count();
    assert_eq!(status_items, 4);
    let snapshots = manager
        .events_since(&thread.id, None)?
        .into_iter()
        .filter(|event| event.event == "model.tools.snapshot")
        .collect::<Vec<_>>();
    assert_eq!(snapshots.len(), 2, "foreign-turn snapshots must be ignored");
    for event in &snapshots {
        assert_eq!(event.turn_id.as_deref(), Some(first.id.as_str()));
        assert_eq!(event.payload["projection_redacted"], true);
        assert_eq!(
            event.payload["snapshot"]["delivery_status"],
            "unknown (capture does not prove provider delivery)"
        );
        assert!(
            !event
                .payload
                .to_string()
                .contains("sk-fixture-private-value")
        );
    }
    let prepared = &snapshots[0].payload["snapshot"];
    assert_eq!(
        prepared["tools"][0]["name"]["value"],
        "mcp_computer_get_app_state"
    );
    assert_eq!(
        prepared["active_tool_catalog_sha256"].as_str(),
        request_digest.as_deref()
    );
    assert!(prepared["terminal"].is_null());
    assert_eq!(
        snapshots[1].payload["snapshot"]["terminal"]["model_requests_started"],
        2
    );

    let completion = manager
        .events_since(&thread.id, None)?
        .into_iter()
        .find(|event| {
            event.event == "turn.completed" && event.turn_id.as_deref() == Some(first.id.as_str())
        })
        .expect("first turn completion receipt");
    assert_eq!(
        completion
            .payload
            .pointer("/turn/modelRequestDiagnostics/modelRequestsStarted")
            .and_then(Value::as_u64),
        Some(2),
        "the existing completion event must carry the same turn's persisted receipt"
    );

    let second = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "pre-request snapshot fixture".to_string(),
                max_output_tokens: std::num::NonZeroU32::new(256),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    let second_engine_turn_id = "engine_request_diagnostics_second";
    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: second_engine_turn_id.to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    let pre_request = crate::tool_inspection::ToolInspectionSnapshot::from_prepared_request(
        second_engine_turn_id,
        0,
        None,
    );
    harness
        .tx_event
        .send(EngineEvent::ToolRequestSnapshot {
            snapshot: pre_request,
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageStarted { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageComplete { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    let second = wait_for_terminal_turn(&manager, &second.id).await?;
    assert!(
        second.model_request_diagnostics.is_none(),
        "a pre-request snapshot must not look like a delivered model call or inherit the prior turn"
    );
    assert_eq!(second.schema_version, OUTPUT_LIMIT_RUNTIME_SCHEMA_VERSION);
    let second_snapshots = manager
        .events_since(&thread.id, None)?
        .into_iter()
        .filter(|event| {
            event.event == "model.tools.snapshot"
                && event.turn_id.as_deref() == Some(second.id.as_str())
        })
        .collect::<Vec<_>>();
    assert_eq!(second_snapshots.len(), 1);
    assert_eq!(
        second_snapshots[0].payload["snapshot"]["tools_field_present"],
        false
    );
    assert!(second_snapshots[0].payload["snapshot"]["terminal"].is_null());

    assert!(
        serde_json::to_value(&second)?
            .get("modelRequestDiagnostics")
            .is_none(),
        "a v4 turn without a terminal snapshot must retain the legacy optional shape"
    );
    Ok(())
}

#[tokio::test]
async fn completed_turn_without_engine_output_fails() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    tokio::spawn(async move {
        if matches!(rx_op.recv().await, Some(Op::SendMessage(TurnSpec { .. }))) {
            let _ = tx_event
                .send(EngineEvent::TurnStarted {
                    turn_id: "engine_empty_turn".to_string(),
                    created_at: chrono::Utc::now(),
                    route: None,
                    submission_id: None,
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::TurnComplete {
                    turn_id: None,
                    submission_id: None,
                    compaction_id: None,
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 0,
                        ..Usage::default()
                    },
                    parent_route_usage: Usage {
                        input_tokens: 10,
                        output_tokens: 0,
                        ..Usage::default()
                    },
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Completed,
                    error: None,
                    tool_catalog: None,
                    base_url: None,
                })
                .await;
        }
    });

    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "empty turn".to_string(),
                input_summary: None,
                model: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                ..Default::default()
            },
        )
        .await?;

    let failed = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(failed.status, RuntimeTurnStatus::Failed);
    assert_eq!(failed.error.as_deref(), Some(EMPTY_TURN_REASON));

    let events = manager.events_since(&thread.id, None)?;
    assert!(events.iter().any(|ev| {
        ev.event == "item.failed"
            && ev
                .payload
                .get("item")
                .and_then(|item| item.get("kind"))
                .and_then(Value::as_str)
                == Some("error")
    }));
    assert!(events.iter().any(|ev| {
        ev.event == "turn.completed"
            && ev
                .payload
                .get("turn")
                .and_then(|turn| turn.get("status"))
                .and_then(Value::as_str)
                == Some("failed")
    }));
    Ok(())
}

#[tokio::test]
async fn worker_lifecycle_receipts_preserve_owner_outcome_and_durable_replay() -> Result<()> {
    use crate::core::events::AgentProgressEventMeta;
    use crate::tools::subagent::{
        AgentWorkerSpec, AgentWorkerStatus, AgentWorkerToolProfile, SubAgentManager,
    };
    use codewhale_agent_contract::role::FleetRole;
    use codewhale_agent_contract::worker_profile::WorkerRuntimeProfile;

    let runtime_dir = test_runtime_dir();
    let manager = test_manager(runtime_dir.clone())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let foreign = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = mock_engine_handle();
    harness
        .handle
        .set_subagent_manager(Arc::new(tokio::sync::RwLock::new(SubAgentManager::new(
            thread.workspace.clone(),
            1,
        ))));
    manager
        .install_test_engine(&thread.id, harness.handle.clone())
        .await?;
    let worker_manager = harness.handle.subagent_manager();
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    let thread_id = thread.id.clone();
    let foreign_id = foreign.id.clone();
    let (registered_tx, registered_rx) = oneshot::channel::<()>();
    let (late_tx, late_rx) = oneshot::channel::<()>();
    let mut events_rx = manager.subscribe_events();
    let completed = EngineEvent::AgentComplete {
        display_name: Some("audit docs".into()),
        owner_session_id: thread.id.clone(),
        id: "worker_completed".into(),
        result: "Completed successfully".into(),
        outcome: Some(SubAgentStatus::Completed),
        parent_run_id: Some("parent".into()),
        spawn_depth: Some(2),
        continuable: Some(false),
        usage: None,
    };
    tokio::spawn(async move {
        if !matches!(rx_op.recv().await, Some(Op::SendMessage(TurnSpec { .. }))) {
            return;
        }
        if registered_rx.await.is_err() {
            return;
        }
        let _ = tx_event
            .send(EngineEvent::TurnStarted {
                turn_id: "engine_worker_lifecycle".into(),
                created_at: Utc::now(),
                route: None,
                submission_id: None,
            })
            .await;
        for owner in [&thread_id, &foreign_id] {
            let _ = tx_event
                .send(EngineEvent::AgentSpawned {
                    display_name: None,
                    owner_session_id: owner.to_string(),
                    id: "worker_spawn".into(),
                    prompt: "private prompt".into(),
                    worker_status: Some(AgentWorkerStatus::Queued),
                    parent_run_id: Some("parent".into()),
                    spawn_depth: 2,
                    model: "fixture".into(),
                    route_source: None,
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::AgentProgress {
                    owner_session_id: owner.to_string(),
                    id: "worker_spawn".into(),
                    status: "private progress".into(),
                    activity: AgentProgressEventMeta::new(AgentWorkerStatus::RunningTool)
                        .with_step(3)
                        .with_tool("private_tool"),
                    parent_run_id: Some("parent".into()),
                    spawn_depth: 2,
                })
                .await;
        }
        let _ = tx_event
            .send(EngineEvent::TurnComplete {
                turn_id: None,
                submission_id: None,
                compaction_id: None,
                usage: Usage::default(),
                parent_route_usage: Usage::default(),
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await;
        if late_rx.await.is_err() {
            return;
        }
        let _ = tx_event.send(completed.clone()).await;
        let _ = tx_event.send(completed.clone()).await;
        if !matches!(rx_op.recv().await, Some(Op::SendMessage(TurnSpec { .. }))) {
            return;
        }
        let _ = tx_event
            .send(EngineEvent::TurnStarted {
                turn_id: "engine_next_parent".into(),
                created_at: Utc::now(),
                route: None,
                submission_id: None,
            })
            .await;
        let _ = tx_event
            .send(EngineEvent::MessageStarted { index: 0 })
            .await;
        let _ = tx_event
            .send(EngineEvent::MessageDelta {
                index: 0,
                content: "Second parent response".into(),
            })
            .await;
        let _ = tx_event
            .send(EngineEvent::MessageComplete { index: 0 })
            .await;

        for owner in [&thread_id, &foreign_id] {
            for (id, outcome) in [
                (
                    "worker_failed",
                    Some(SubAgentStatus::Failed("private failure".into())),
                ),
                (
                    "worker_interrupted",
                    Some(SubAgentStatus::Interrupted("private reason".into())),
                ),
                ("worker_cancelled", Some(SubAgentStatus::Cancelled)),
                ("worker_budget", Some(SubAgentStatus::BudgetExhausted)),
                ("worker_legacy", None),
                ("worker_unknown_origin", Some(SubAgentStatus::Completed)),
            ] {
                let _ = tx_event
                    .send(EngineEvent::AgentComplete {
                        display_name: None,
                        owner_session_id: owner.to_string(),
                        id: id.into(),
                        result: "Completed successfully".into(),
                        continuable: Some(id == "worker_interrupted"),
                        outcome,
                        parent_run_id: Some("parent".into()),
                        spawn_depth: Some(2),
                        usage: None,
                    })
                    .await;
            }
        }
        let _ = tx_event.send(completed).await;
        let _ = tx_event
            .send(EngineEvent::TurnComplete {
                turn_id: None,
                submission_id: None,
                compaction_id: None,
                usage: Usage::default(),
                parent_route_usage: Usage::default(),
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await;
    });
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "worker lifecycle fixture".into(),
                ..Default::default()
            },
        )
        .await?;
    {
        let mut workers = worker_manager.write().await;
        for id in [
            "worker_spawn",
            "worker_completed",
            "worker_failed",
            "worker_interrupted",
            "worker_cancelled",
            "worker_budget",
            "worker_legacy",
            "worker_unknown_origin",
        ] {
            workers.register_worker_for_session_with_origin(
                AgentWorkerSpec {
                    worker_id: id.into(),
                    run_id: id.into(),
                    parent_run_id: Some("parent".into()),
                    workflow_run_id: None,
                    session_name: None,
                    objective: "fixture".into(),
                    role: None,
                    agent_type: FleetRole::Verifier,
                    model: "fixture".into(),
                    workspace: if id == "worker_completed" {
                        thread.workspace.join("isolated-child")
                    } else {
                        thread.workspace.clone()
                    },
                    git_branch: None,
                    context_mode: "fresh".into(),
                    fork_context: false,
                    tool_profile: AgentWorkerToolProfile::Inherited,
                    runtime_profile: WorkerRuntimeProfile::default(),
                    max_steps: 2,
                    spawn_depth: 2,
                    max_spawn_depth: 3,
                    child_route: None,
                    launch_manifest: None,
                },
                &thread.id,
                None,
                (id != "worker_unknown_origin").then(|| turn.id.clone()),
            );
        }
    }
    registered_tx
        .send(())
        .map_err(|_| anyhow!("fixture registration receiver closed"))?;
    let original = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(original.status, RuntimeTurnStatus::Completed);
    assert!(
        manager.active.lock().await.engines[&thread.id]
            .active_turn
            .is_none()
    );
    late_tx
        .send(())
        .map_err(|_| anyhow!("fixture completion receiver closed"))?;
    tokio::time::timeout(TURN_SETTLEMENT_DEADLOCK_TIMEOUT, async {
        loop {
            let event = events_rx.recv().await?;
            if event.event == "agent.completed" && event.payload["agent_id"] == "worker_completed" {
                anyhow::ensure!(
                    event.turn_id.as_deref() == Some(turn.id.as_str()),
                    "idle completion changed parent"
                );
                return Ok::<_, anyhow::Error>(());
            }
        }
    })
    .await??;
    let next = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "another parent".into(),
                ..Default::default()
            },
        )
        .await?;
    let _ = wait_for_terminal_turn(&manager, &next.id).await?;
    let events = manager.events_since(&thread.id, None)?;
    let workers: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(
                event.event.as_str(),
                "agent.spawned" | "agent.progress" | "agent.completed"
            )
        })
        .collect();
    assert_eq!(
        workers.len(),
        8,
        "foreign, unknown-origin and duplicate envelopes must not enter this journal"
    );
    assert!(
        workers
            .iter()
            .all(|event| event.turn_id.as_deref() == Some(turn.id.as_str()))
    );
    assert_eq!(workers[0].payload["worker_status"], "queued");
    assert_eq!(workers[1].payload["worker_status"], "running_tool");
    assert_eq!(workers[1].payload["step"], 3);
    assert!(workers[1].payload.get("tool_name").is_none());
    for (event, expected) in workers[2..].iter().zip([
        Some("completed"),
        Some("failed"),
        Some("interrupted"),
        Some("cancelled"),
        Some("budget_exhausted"),
        None,
    ]) {
        assert_eq!(event.payload["worker_status"].as_str(), expected);
        assert_eq!(event.payload["parent_run_id"], "parent");
        assert_eq!(event.payload["spawn_depth"], 2);
        assert_eq!(
            event.payload["continuable"],
            expected == Some("interrupted")
        );
    }
    assert!(
        workers[7].payload["item"]["summary"]
            .as_str()
            .unwrap()
            .contains("outcome unconfirmed")
    );
    assert_eq!(workers[2].payload["agent_name"], "audit docs");
    assert!(
        workers[2].payload["item"]["summary"]
            .as_str()
            .unwrap()
            .starts_with("Sub-agent audit docs completed")
    );
    let original = manager.store.load_turn(&turn.id)?;
    assert_eq!(original.status, RuntimeTurnStatus::Completed);
    assert!(
        workers
            .iter()
            .all(|event| original.item_ids.contains(event.item_id.as_ref().unwrap()))
    );
    let current = manager.store.load_turn(&next.id)?;
    assert!(
        workers
            .iter()
            .all(|event| !current.item_ids.contains(event.item_id.as_ref().unwrap()))
    );
    assert_eq!(
        manager
            .store
            .load_thread(&thread.id)?
            .latest_turn_id
            .as_deref(),
        Some(next.id.as_str())
    );
    let notices = manager.list_notices(&thread.id);
    assert!(
        notices
            .iter()
            .any(|notice| notice.detail == "audit docs finished"),
        "{notices:?}"
    );
    assert!(
        notices
            .iter()
            .any(|notice| notice.detail == "worker_failed failed"),
        "{notices:?}"
    );
    assert!(
        manager
            .events_since(&foreign.id, None)?
            .iter()
            .all(|event| !event.event.starts_with("agent."))
    );
    let reopened = RuntimeThreadStore::open(runtime_dir)?;
    let replay = reopened.events_since(&thread.id, None)?;
    assert_eq!(
        serde_json::to_value(&replay)?,
        serde_json::to_value(&events)?
    );
    assert!(
        reopened
            .events_since(&thread.id, events.last().map(|event| event.seq))?
            .is_empty()
    );
    Ok(())
}
#[tokio::test]
async fn preturn_control_status_does_not_make_empty_turn_succeed() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    let thread_id = thread.id.clone();
    tokio::spawn(async move {
        if matches!(rx_op.recv().await, Some(Op::SendMessage(TurnSpec { .. }))) {
            let _ = tx_event
                .send(EngineEvent::AgentComplete {
                    display_name: None,
                    owner_session_id: thread_id,
                    id: "stale_agent".to_string(),
                    result: "stale completion".to_string(),
                    outcome: None,
                    parent_run_id: None,
                    spawn_depth: None,
                    continuable: None,
                    usage: None,
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::status("Compaction settings updated"))
                .await;
            let _ = tx_event
                .send(EngineEvent::TurnStarted {
                    turn_id: "engine_empty_after_control_status".to_string(),
                    created_at: chrono::Utc::now(),
                    route: None,
                    submission_id: None,
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::TurnComplete {
                    turn_id: None,
                    submission_id: None,
                    compaction_id: None,
                    usage: Usage::default(),
                    parent_route_usage: Usage::default(),
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Completed,
                    error: None,
                    tool_catalog: None,
                    base_url: None,
                })
                .await;
        }
    });

    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "empty after setup".to_string(),
                ..Default::default()
            },
        )
        .await?;
    let terminal = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Failed);
    assert_eq!(terminal.error.as_deref(), Some(EMPTY_TURN_REASON));
    assert!(
        manager
            .store
            .list_items_for_turn(&turn.id)?
            .iter()
            .all(|item| {
                item.summary != "Compaction settings updated"
                    && !item.summary.contains("stale_agent")
            })
    );
    Ok(())
}

#[tokio::test]
async fn engine_error_remains_failed_after_nominal_turn_complete() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    tokio::spawn(async move {
        if matches!(rx_op.recv().await, Some(Op::SendMessage(TurnSpec { .. }))) {
            let _ = tx_event
                .send(EngineEvent::TurnStarted {
                    turn_id: "engine_error_then_complete".to_string(),
                    created_at: chrono::Utc::now(),
                    route: None,
                    submission_id: None,
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::error(
                    codewhale_error_taxonomy::ErrorEnvelope::fatal("provider exploded"),
                ))
                .await;
            let _ = tx_event
                .send(EngineEvent::TurnComplete {
                    turn_id: None,
                    submission_id: None,
                    compaction_id: None,
                    usage: Usage::default(),
                    parent_route_usage: Usage::default(),
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Completed,
                    error: None,
                    tool_catalog: None,
                    base_url: None,
                })
                .await;
        }
    });

    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "surface the failure".to_string(),
                ..Default::default()
            },
        )
        .await?;
    let terminal = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Failed);
    assert_eq!(terminal.error.as_deref(), Some("provider exploded"));
    Ok(())
}

#[tokio::test]
async fn update_thread_workspace_persists_event_and_evicts_idle_engine() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let old_workspace = std::env::temp_dir().join("codewhale-runtime-old-workspace");
    let new_workspace = std::env::temp_dir().join("codewhale-runtime-new-workspace");
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: Some(old_workspace.clone()),
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;

    let updated = manager
        .update_thread(
            &thread.id,
            UpdateThreadRequest {
                workspace: Some(new_workspace.clone()),
                ..UpdateThreadRequest::default()
            },
        )
        .await?;

    assert_eq!(updated.workspace, new_workspace);
    assert_eq!(
        manager.store.load_thread(&thread.id)?.workspace,
        new_workspace
    );
    {
        let active = manager.active.lock().await;
        assert!(
            !active.engines.contains_key(&thread.id),
            "workspace changes must evict the stale cached engine"
        );
        assert!(!active.lru.iter().any(|id| id == &thread.id));
    }

    match tokio::time::timeout(Duration::from_secs(1), rx_op.recv()).await {
        Ok(Some(Op::Shutdown)) => {}
        other => panic!("expected cached engine shutdown, got {other:?}"),
    }

    let events = manager.events_since(&thread.id, None)?;
    let event = events
        .iter()
        .rev()
        .find(|event| event.event == "thread.updated")
        .expect("thread.updated event");
    let workspace_value = serde_json::to_value(&updated.workspace)?;
    assert_eq!(
        event
            .payload
            .get("changes")
            .and_then(|changes| changes.get("workspace")),
        Some(&workspace_value)
    );
    Ok(())
}

#[tokio::test]
async fn update_thread_workspace_rejects_empty_path() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let err = manager
        .update_thread(
            &thread.id,
            UpdateThreadRequest {
                workspace: Some(PathBuf::new()),
                ..UpdateThreadRequest::default()
            },
        )
        .await
        .expect_err("empty workspace must be rejected");
    assert!(format!("{err:#}").contains("workspace must not be empty"));
    Ok(())
}

/// Restore admission rejects any active turn whose workspace overlaps the
/// one about to be mutated: the same tree, a nested checkout, or a parent.
/// Unrelated workspaces are unaffected, and the thread-scoped guard reads the
/// record under admission.
#[tokio::test]
async fn workspace_restore_guard_rejects_overlapping_active_turns() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let root = std::env::temp_dir().join(format!("codewhale-restore-guard-{}", Uuid::new_v4()));
    let busy = root.join("busy");
    let nested = busy.join("nested");
    let idle = root.join("idle");
    std::fs::create_dir_all(&nested)?;
    std::fs::create_dir_all(&idle)?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(busy.clone()),
            ..Default::default()
        })
        .await?;

    // Nothing is running: both guards are granted and released.
    drop(manager.workspace_restore_guard(&busy).await?);
    let (guard, record) = manager.thread_restore_guard(&thread.id).await?;
    assert_eq!(record.id, thread.id);
    drop(guard);

    let _harness = install_mock_engine(&manager, &thread.id).await;
    {
        let mut active = manager.active.lock().await;
        let state = active.engines.get_mut(&thread.id).expect("mock engine");
        state.active_turn = Some(ActiveTurnState {
            narrowing: crate::core::engine::TurnNarrowing::Inherit,
            goal_progress: None,
            goal_id: None,
            turn_id: "turn_live".to_string(),
            interrupt_requested: false,
            compaction_id: None,
        });
    }
    for workspace in [&busy, &nested, &root] {
        let err = manager
            .workspace_restore_guard(workspace)
            .await
            .expect_err("overlapping active turn must be rejected");
        assert!(
            format!("{err:#}").contains("already has an active turn"),
            "{}: {err:#}",
            workspace.display()
        );
    }
    let err = manager
        .thread_restore_guard(&thread.id)
        .await
        .expect_err("the busy thread itself must be rejected");
    assert!(format!("{err:#}").contains("already has an active turn"));
    // An unrelated workspace is not blocked by someone else's turn.
    drop(manager.workspace_restore_guard(&idle).await?);
    let err = manager
        .thread_restore_guard("thr_missing")
        .await
        .expect_err("unknown thread");
    assert!(format!("{err:#}").contains("Thread not found"));

    {
        let mut active = manager.active.lock().await;
        active
            .engines
            .get_mut(&thread.id)
            .expect("mock engine")
            .active_turn = None;
    }
    drop(manager.workspace_restore_guard(&busy).await?);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[tokio::test]
async fn update_thread_workspace_rejects_active_turn() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let old_workspace = std::env::temp_dir().join("codewhale-runtime-active-old");
    let new_workspace = std::env::temp_dir().join("codewhale-runtime-active-new");
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: Some(old_workspace.clone()),
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    {
        let mut active = manager.active.lock().await;
        let state = active.engines.get_mut(&thread.id).expect("mock engine");
        state.active_turn = Some(ActiveTurnState {
            narrowing: crate::core::engine::TurnNarrowing::Inherit,
            goal_progress: None,
            goal_id: None,
            turn_id: "turn_live".to_string(),
            interrupt_requested: false,
            compaction_id: None,
        });
    }

    let err = manager
        .update_thread(
            &thread.id,
            UpdateThreadRequest {
                workspace: Some(new_workspace),
                ..UpdateThreadRequest::default()
            },
        )
        .await
        .expect_err("workspace update during active turn must fail");

    assert!(format!("{err:#}").contains("active turn"));
    assert_eq!(
        manager.store.load_thread(&thread.id)?.workspace,
        old_workspace
    );
    {
        let active = manager.active.lock().await;
        assert!(
            active.engines.contains_key(&thread.id),
            "active engine should stay cached after rejected update"
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), rx_op.recv())
            .await
            .is_err(),
        "rejected workspace update must not shut down the active engine"
    );
    Ok(())
}

#[tokio::test]
async fn start_turn_passes_effective_auto_approve_to_engine() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: Some(false),
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;

    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "override approval".to_string(),
                input_summary: None,
                model: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: Some(true),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(turn.permission_posture.as_deref(), Some("full_access"));

    match rx_op.recv().await {
        Some(Op::SendMessage(TurnSpec {
            auto_approve,
            approval_mode,
            ..
        })) => {
            assert!(auto_approve);
            assert_eq!(approval_mode, ApprovalMode::Bypass);
        }
        other => panic!("expected SendMessage op, got {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn start_turn_can_override_thread_auto_approve_to_false() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: Some(true),
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;

    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "disable approval".to_string(),
                input_summary: None,
                model: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: Some(false),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(turn.permission_posture.as_deref(), Some("ask"));

    match rx_op.recv().await {
        Some(Op::SendMessage(TurnSpec {
            auto_approve,
            approval_mode,
            ..
        })) => {
            assert!(!auto_approve);
            assert_eq!(approval_mode, ApprovalMode::Suggest);
        }
        other => panic!("expected SendMessage op, got {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn start_turn_enforces_and_records_auto_review_without_legacy_bypass() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            permission_posture: Some("ask".to_string()),
            ..Default::default()
        })
        .await?;
    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;

    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "review autonomously".to_string(),
                permission_posture: Some("auto-review".to_string()),
                ..Default::default()
            },
        )
        .await?;

    assert_eq!(turn.permission_posture.as_deref(), Some("auto_review"));
    assert_eq!(
        manager
            .store
            .load_turn(&turn.id)?
            .permission_posture
            .as_deref(),
        Some("auto_review")
    );
    match rx_op.recv().await {
        Some(Op::SendMessage(TurnSpec {
            auto_approve,
            approval_mode,
            ..
        })) => {
            assert!(!auto_approve);
            assert_eq!(approval_mode, ApprovalMode::Auto);
        }
        other => panic!("expected SendMessage op, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn active_turn_permission_posture_switches_use_the_engine_live_authority() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            permission_posture: Some("ask".to_string()),
            ..Default::default()
        })
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "keep working while permissions change".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec {
            approval_mode: ApprovalMode::Suggest,
            ..
        }))
    ));

    for (requested, canonical, expected_auto, expected_approval) in [
        ("auto-review", "auto_review", false, ApprovalMode::Auto),
        ("full-access", "full_access", true, ApprovalMode::Bypass),
        ("ask", "ask", false, ApprovalMode::Suggest),
    ] {
        let updated = manager
            .update_thread(
                &thread.id,
                UpdateThreadRequest {
                    permission_posture: Some(requested.to_string()),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated.permission_posture.as_deref(), Some(canonical));
        assert_eq!(updated.auto_approve, expected_auto);

        match harness.rx_op.recv().await {
            Some(Op::ChangeMode {
                auto_approve,
                approval_mode,
                ..
            }) => {
                assert_eq!(auto_approve, expected_auto, "{requested}");
                assert_eq!(approval_mode, expected_approval, "{requested}");
            }
            other => panic!("expected ChangeMode for {requested}, got {other:?}"),
        }

        let authority = {
            let active = manager.active.lock().await;
            active
                .engines
                .get(&thread.id)
                .expect("active engine")
                .engine
                .runtime_permission_authority()
        };
        assert_eq!(authority.auto_approve, expected_auto, "{requested}");
        assert_eq!(authority.approval_mode, expected_approval, "{requested}");
        assert_eq!(
            manager.active_turn_flags(&thread.id, &turn.id).await,
            Some((expected_auto, false)),
            "{requested} must replace the running turn's authority immediately"
        );
    }

    Ok(())
}

#[tokio::test]
async fn compact_thread_preserves_thread_auto_approve_policy() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: Some(false),
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;

    let turn = manager
        .compact_thread(&thread.id, CompactThreadRequest::default())
        .await?;

    match rx_op.recv().await {
        Some(Op::CompactContext { compaction, .. }) => {
            assert_eq!(
                compaction.runtime_cost_owner.as_deref(),
                Some(turn.id.as_str())
            );
        }
        other => panic!("expected CompactContext op, got {other:?}"),
    }
    assert_eq!(
        manager.active_turn_flags(&thread.id, &turn.id).await,
        Some((false, false))
    );

    Ok(())
}

#[tokio::test]
async fn compact_interrupt_persists_canceled_item_for_the_exact_request() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;

    let turn = manager
        .compact_thread(&thread.id, CompactThreadRequest::default())
        .await?;
    let compaction_id = match harness.rx_op.recv().await {
        Some(Op::CompactContext { id, .. }) => id,
        other => panic!("expected CompactContext op, got {other:?}"),
    };

    manager.interrupt_turn(&thread.id, &turn.id).await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::CancelCompaction { id }) if id == compaction_id
    ));

    harness
        .tx_event
        .send(EngineEvent::CompactionStarted {
            id: compaction_id.clone(),
            auto: false,
            message: "compaction started".to_string(),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::CompactionCancelled {
            id: compaction_id,
            auto: false,
            message: "compaction canceled".to_string(),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Interrupted,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;

    let terminal = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Interrupted);
    let items = manager.store.list_items_for_turn(&turn.id)?;
    assert!(items.iter().any(|item| {
        item.kind == TurnItemKind::ContextCompaction
            && item.status == TurnItemLifecycleStatus::Canceled
    }));
    let events = manager.events_since(&thread.id, None)?;
    assert!(events.iter().any(|event| {
        event.event == "item.canceled"
            && event
                .payload
                .get("item")
                .and_then(|item| item.get("status"))
                .and_then(Value::as_str)
                == Some("canceled")
    }));
    Ok(())
}

#[tokio::test]
async fn closed_compaction_mailbox_rolls_back_durable_records_and_active_claim() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let harness = install_mock_engine(&manager, &thread.id).await;
    let before_active = {
        let active = manager.active.lock().await;
        let state = active.engines.get(&thread.id).expect("installed engine");
        (
            state.active_turn.as_ref().map(|turn| turn.turn_id.clone()),
            state.route_identity.clone(),
            state.route_model.clone(),
            active.lru.clone(),
        )
    };
    let before_thread = serde_json::to_value(manager.get_thread(&thread.id).await?)?;
    let before_events = serde_json::to_value(manager.events_since(&thread.id, None)?)?;
    drop(harness.rx_op);

    let error = manager
        .compact_thread(&thread.id, CompactThreadRequest::default())
        .await
        .expect_err("closed mailbox must reject compaction")
        .to_string();
    assert!(error.contains("Failed to trigger compaction"), "{error}");

    assert!(manager.store.list_turns_for_thread(&thread.id)?.is_empty());
    assert_eq!(
        serde_json::to_value(manager.get_thread(&thread.id).await?)?,
        before_thread
    );
    assert_eq!(
        serde_json::to_value(manager.events_since(&thread.id, None)?)?,
        before_events
    );
    let after_active = {
        let active = manager.active.lock().await;
        let state = active.engines.get(&thread.id).expect("installed engine");
        (
            state.active_turn.as_ref().map(|turn| turn.turn_id.clone()),
            state.route_identity.clone(),
            state.route_model.clone(),
            active.lru.clone(),
        )
    };
    assert_eq!(after_active, before_active);
    Ok(())
}

#[tokio::test]
async fn compact_thread_receipt_keeps_exact_named_custom_identity() -> Result<()> {
    let mut custom = std::collections::HashMap::new();
    custom.insert(
        "lm-studio".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("http://127.0.0.1:1234/v1".to_string()),
            model: Some("local-code-model".to_string()),
            ..Default::default()
        },
    );
    let manager = RuntimeThreadManager::open(
        {
            let __cw_config_init_24_0 = Some("lm-studio".to_string());
            let __cw_config_init_24_1 = Some(codewhale_runtime::config::ProvidersConfig {
                custom,
                ..Default::default()
            });
            let mut __cw_config_init_24_value: Config = Default::default();
            __cw_config_init_24_value.provider = __cw_config_init_24_0;
            __cw_config_init_24_value.providers = __cw_config_init_24_1;
            __cw_config_init_24_value
        },
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;

    let turn = manager
        .compact_thread(&thread.id, CompactThreadRequest::default())
        .await?;

    assert!(matches!(
        rx_op.recv().await,
        Some(Op::CompactContext { .. })
    ));
    assert_eq!(turn.effective_provider.as_deref(), Some("custom"));
    assert_eq!(turn.effective_provider_id.as_deref(), Some("lm-studio"));
    Ok(())
}

#[tokio::test]
async fn compact_thread_with_real_engine_reaches_terminal_status() -> Result<()> {
    // A real engine takes the workspace coordination lock exclusively, and
    // nextest runs tests in parallel processes with this crate as their working
    // directory. A `"."` workspace would therefore be the crate directory
    // itself, and two exclusive holders of it cannot both start.
    let workspace = tempfile::tempdir()?;
    let manager = RuntimeThreadManager::open(
        {
            // This test intentionally crosses the real-engine boundary. Give
            // client preflight a hermetic credential and closed-loopback URL;
            // the assertion permits the resulting terminal failure.
            Config::default()
        }
        .with_legacy_root(
            Some("runtime-thread-test-key".to_string()),
            Some("http://127.0.0.1:1/v1".to_string()),
        ),
        workspace.path().to_path_buf(),
        test_manager_config(test_runtime_dir()),
    )?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let turn = manager
        .compact_thread(&thread.id, CompactThreadRequest::default())
        .await?;
    let terminal = wait_for_terminal_turn(&manager, &turn.id).await?;

    assert!(matches!(
        terminal.status,
        RuntimeTurnStatus::Completed | RuntimeTurnStatus::Failed
    ));
    assert!(
        terminal.ended_at.is_some(),
        "manual compaction should reach a terminal turn state"
    );
    assert_eq!(manager.active_turn_flags(&thread.id, &turn.id).await, None);

    let expected_status = match terminal.status {
        RuntimeTurnStatus::Completed => "completed",
        RuntimeTurnStatus::Failed => "failed",
        other => panic!("unexpected non-terminal compaction status: {other:?}"),
    };
    let events = manager.events_since(&thread.id, None)?;
    assert!(events.iter().any(|ev| {
        ev.event == "turn.completed"
            && ev
                .payload
                .get("turn")
                .and_then(|turn| turn.get("status"))
                .and_then(Value::as_str)
                == Some(expected_status)
    }));
    Ok(())
}

#[tokio::test]
async fn multi_turn_continuity_same_thread() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    tokio::spawn(async move {
        let mut turn_index = 0u8;
        while let Some(op) = rx_op.recv().await {
            if !matches!(op, Op::SendMessage(TurnSpec { .. })) {
                continue;
            }
            turn_index = turn_index.saturating_add(1);
            let _ = tx_event
                .send(EngineEvent::TurnStarted {
                    turn_id: format!("engine_turn_{turn_index}"),
                    created_at: chrono::Utc::now(),
                    route: None,
                    submission_id: None,
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::MessageStarted { index: 0 })
                .await;
            let _ = tx_event
                .send(EngineEvent::MessageDelta {
                    index: 0,
                    content: format!("reply {turn_index}"),
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::MessageComplete { index: 0 })
                .await;
            let _ = tx_event
                .send(EngineEvent::TurnComplete {
                    turn_id: None,
                    submission_id: None,
                    compaction_id: None,
                    usage: Usage {
                        input_tokens: 5,
                        output_tokens: 5,
                        ..Usage::default()
                    },
                    parent_route_usage: Usage {
                        input_tokens: 5,
                        output_tokens: 5,
                        ..Usage::default()
                    },
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Completed,
                    error: None,
                    tool_catalog: None,
                    base_url: None,
                })
                .await;
            if turn_index >= 2 {
                break;
            }
        }
    });

    let turn_1 = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "first".to_string(),
                input_summary: None,
                model: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                ..Default::default()
            },
        )
        .await?;
    let turn_1 = wait_for_terminal_turn(&manager, &turn_1.id).await?;
    assert_eq!(turn_1.status, RuntimeTurnStatus::Completed);

    let turn_2 = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "second".to_string(),
                input_summary: None,
                model: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                ..Default::default()
            },
        )
        .await?;
    let turn_2 = wait_for_terminal_turn(&manager, &turn_2.id).await?;
    assert_eq!(turn_2.status, RuntimeTurnStatus::Completed);

    let detail = manager.get_thread_detail(&thread.id).await?;
    assert_eq!(
        detail.thread.latest_turn_id.as_deref(),
        Some(turn_2.id.as_str())
    );
    assert_eq!(detail.turns.len(), 2);
    assert!(detail.items.iter().any(|item| {
        item.kind == TurnItemKind::UserMessage && item.detail.as_deref() == Some("first")
    }));
    assert!(detail.items.iter().any(|item| {
        item.kind == TurnItemKind::UserMessage && item.detail.as_deref() == Some("second")
    }));

    let events = manager.events_since(&thread.id, None)?;
    let started = events
        .iter()
        .filter(|ev| ev.event == "turn.started")
        .count();
    let completed = events
        .iter()
        .filter(|ev| ev.event == "turn.completed")
        .count();
    assert_eq!(started, 2);
    assert_eq!(completed, 2);
    Ok(())
}

/// A minimal tool definition for `TurnComplete.tool_catalog` receipts in
/// mock-engine goal tests.
fn catalog_tool(name: &str) -> codewhale_core::request::Tool {
    codewhale_core::request::Tool {
        tool_type: None,
        name: name.to_string(),
        description: String::new(),
        input_schema: serde_json::json!({ "type": "object" }),
        allowed_callers: None,
        defer_loading: None,
        input_examples: None,
        strict: None,
        cache_control: None,
    }
}

fn active_test_goal(thread_id: &str, goal_id: &str) -> codewhale_protocol::ThreadGoal {
    codewhale_protocol::ThreadGoal {
        thread_id: thread_id.to_string(),
        goal_id: goal_id.to_string(),
        objective: "ship the goal loop".to_string(),
        status: codewhale_protocol::ThreadGoalStatus::Active,
        token_budget: None,
        tokens_used: 0,
        time_used_seconds: 0,
        continuation_count: 0,
        last_gap_fingerprint: None,
        repeated_gap_count: 0,
        last_gap_pass: None,
        pause_reason: None,
        created_at: 0,
        updated_at: 0,
    }
}

async fn wait_for_goal_status(
    manager: &RuntimeThreadManager,
    thread_id: &str,
    expected: codewhale_protocol::ThreadGoalStatus,
    timeout: Duration,
) -> Result<codewhale_protocol::ThreadGoal> {
    let deadline = Instant::now() + timeout;
    loop {
        let goal = manager
            .store
            .load_goal(thread_id)?
            .ok_or_else(|| anyhow::anyhow!("goal record missing for {thread_id}"))?;
        if goal.status == expected {
            return Ok(goal);
        }
        if Instant::now() > deadline {
            anyhow::bail!("goal did not reach {expected:?} in time: {goal:?}");
        }
        sleep(Duration::from_millis(20)).await;
    }
}

/// Wait until the thread has `expected` terminal turns in the durable store.
async fn wait_for_terminal_turn_count(
    manager: &RuntimeThreadManager,
    thread_id: &str,
    expected: usize,
    timeout: Duration,
) -> Result<Vec<TurnRecord>> {
    let deadline = Instant::now() + timeout;
    loop {
        let turns = manager.store.list_turns_for_thread(thread_id)?;
        let terminal = turns
            .iter()
            .filter(|turn| {
                matches!(
                    turn.status,
                    RuntimeTurnStatus::Completed
                        | RuntimeTurnStatus::Failed
                        | RuntimeTurnStatus::Interrupted
                        | RuntimeTurnStatus::Canceled
                )
            })
            .count();
        if terminal == expected {
            return Ok(turns);
        }
        if Instant::now() > deadline {
            anyhow::bail!("expected {expected} terminal turns, saw {terminal}: {turns:?}");
        }
        sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn host_goal_loop_kickoff_arms_one_continuation_and_parks_at_engine_cap() -> Result<()> {
    let config = {
        let __cw_config_init_26_0 = Some(codewhale_runtime::config::GoalConfig {
            max_continuations: Some(2),
            continuation_delay_seconds: None,
            max_steps: None,
            enforce_token_budget: None,
        });
        let mut __cw_config_init_26_value: Config = Config::default();
        __cw_config_init_26_value.goal = __cw_config_init_26_0;
        __cw_config_init_26_value
    };
    let manager = RuntimeThreadManager::open(
        config,
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    manager
        .store
        .save_goal(&active_test_goal(&thread.id, "goal_cap"))?;

    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    let passes = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0));
    let counter = passes.clone();
    tokio::spawn(async move {
        while let Some(op) = rx_op.recv().await {
            if !matches!(op, Op::SendMessage(TurnSpec { .. })) {
                continue;
            }
            let pass = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            // Mirror the real engine: emit_goal_updated lands right before
            // TurnComplete, so settlement sees the final snapshot.
            let snapshot = crate::tools::goal::GoalSnapshot {
                objective: Some("ship the goal loop".to_string()),
                status: "active".to_string(),
                // The kickoff runs pass 0; the armed continuation is pass 2,
                // already at the configured cap.
                continuation_count: if pass == 1 { 0 } else { 2 },
                ..Default::default()
            };
            let _ = tx_event.send(EngineEvent::GoalUpdated { snapshot }).await;
            let _ = tx_event
                .send(EngineEvent::TurnStarted {
                    turn_id: format!("engine_goal_{pass}"),
                    created_at: chrono::Utc::now(),
                    route: None,
                    submission_id: None,
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::MessageStarted { index: 0 })
                .await;
            let _ = tx_event
                .send(EngineEvent::MessageDelta {
                    index: 0,
                    content: format!("pass {pass}"),
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::MessageComplete { index: 0 })
                .await;
            let _ = tx_event
                .send(EngineEvent::TurnComplete {
                    turn_id: None,
                    submission_id: None,
                    compaction_id: None,
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 10,
                        ..Usage::default()
                    },
                    parent_route_usage: Usage {
                        input_tokens: 10,
                        output_tokens: 10,
                        ..Usage::default()
                    },
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Completed,
                    error: None,
                    tool_catalog: Some(vec![catalog_tool("update_goal")]),
                    base_url: None,
                })
                .await;
        }
    });

    manager.activate_thread_goal(&thread.id).await?;

    // Kickoff plus exactly one armed continuation, then the engine cap
    // parks the goal instead of arming a third pass.
    let turns =
        wait_for_terminal_turn_count(&manager, &thread.id, 2, TURN_SETTLEMENT_DEADLOCK_TIMEOUT)
            .await?;
    assert_eq!(turns.len(), 2);
    assert!(
        turns
            .iter()
            .all(|turn| turn.status == RuntimeTurnStatus::Completed)
    );
    assert_eq!(turns[0].input_summary, "goal kickoff");
    assert_eq!(turns[1].input_summary, "goal continuation pass #1");

    let goal = wait_for_goal_status(
        &manager,
        &thread.id,
        codewhale_protocol::ThreadGoalStatus::Paused,
        TURN_SETTLEMENT_DEADLOCK_TIMEOUT,
    )
    .await?;
    // Snapshot authority: the record keeps the engine's counter (2), and
    // both turns' usage was accrued.
    assert_eq!(goal.continuation_count, 2);
    assert_eq!(goal.tokens_used, 40);

    // Grace window for an (incorrect) third pass with delay 0.
    sleep(Duration::from_millis(400)).await;
    assert_eq!(
        passes.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "no pass may be armed after the engine cap parked the goal"
    );

    // Transcripts must read the kickoff as a kickoff, not "pass #0".
    let detail = manager.get_thread_detail(&thread.id).await?;
    assert!(detail.items.iter().any(|item| {
        item.kind == TurnItemKind::UserMessage
            && item.detail.as_deref() == Some("Goal kickoff (host-driven)")
    }));
    assert!(detail.items.iter().any(|item| {
        item.kind == TurnItemKind::UserMessage
            && item.detail.as_deref() == Some("Goal continuation pass #1 (host-driven)")
    }));
    Ok(())
}

#[tokio::test]
async fn host_goal_loop_skips_rearm_without_update_goal_and_after_failed_pass() -> Result<()> {
    // Scenario A: a completed pass whose catalog lacks `update_goal` has no
    // terminal path, so the host must not arm another pass.
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    manager
        .store
        .save_goal(&active_test_goal(&thread.id, "goal_no_update"))?;
    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    let passes = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0));
    let counter = passes.clone();
    tokio::spawn(async move {
        while let Some(op) = rx_op.recv().await {
            if !matches!(op, Op::SendMessage(TurnSpec { .. })) {
                continue;
            }
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let snapshot = crate::tools::goal::GoalSnapshot {
                objective: Some("ship the goal loop".to_string()),
                status: "active".to_string(),
                ..Default::default()
            };
            let _ = tx_event.send(EngineEvent::GoalUpdated { snapshot }).await;
            let _ = tx_event
                .send(EngineEvent::TurnStarted {
                    turn_id: "engine_goal_no_update".to_string(),
                    created_at: chrono::Utc::now(),
                    route: None,
                    submission_id: None,
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::MessageStarted { index: 0 })
                .await;
            let _ = tx_event
                .send(EngineEvent::MessageDelta {
                    index: 0,
                    content: "work without the goal tool".to_string(),
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::MessageComplete { index: 0 })
                .await;
            let _ = tx_event
                .send(EngineEvent::TurnComplete {
                    turn_id: None,
                    submission_id: None,
                    compaction_id: None,
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 10,
                        ..Usage::default()
                    },
                    parent_route_usage: Usage {
                        input_tokens: 10,
                        output_tokens: 10,
                        ..Usage::default()
                    },
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Completed,
                    error: None,
                    tool_catalog: Some(vec![catalog_tool("read")]),
                    base_url: None,
                })
                .await;
        }
    });
    manager.activate_thread_goal(&thread.id).await?;
    wait_for_terminal_turn_count(&manager, &thread.id, 1, TURN_SETTLEMENT_DEADLOCK_TIMEOUT).await?;
    // Settlement must have accrued usage before deciding; once visible, the
    // arming decision has been made.
    let deadline = Instant::now() + TURN_SETTLEMENT_DEADLOCK_TIMEOUT;
    let goal = loop {
        let goal = manager
            .store
            .load_goal(&thread.id)?
            .ok_or_else(|| anyhow::anyhow!("goal record missing"))?;
        if goal.tokens_used >= 20 || Instant::now() > deadline {
            break goal;
        }
        sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(goal.status, codewhale_protocol::ThreadGoalStatus::Active);
    sleep(Duration::from_millis(400)).await;
    assert_eq!(
        passes.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "no pass may be armed without update_goal in the catalog"
    );

    // Scenario B: a failed kickoff pass leaves the goal Active without
    // arming anything.
    let failed_thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    manager
        .store
        .save_goal(&active_test_goal(&failed_thread.id, "goal_failed_pass"))?;
    let failed_harness = install_mock_engine(&manager, &failed_thread.id).await;
    let mut failed_rx_op = failed_harness.rx_op;
    let failed_tx_event = failed_harness.tx_event;
    let failed_passes = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0));
    let failed_counter = failed_passes.clone();
    tokio::spawn(async move {
        while let Some(op) = failed_rx_op.recv().await {
            if !matches!(op, Op::SendMessage(TurnSpec { .. })) {
                continue;
            }
            failed_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let snapshot = crate::tools::goal::GoalSnapshot {
                objective: Some("ship the goal loop".to_string()),
                status: "active".to_string(),
                ..Default::default()
            };
            let _ = failed_tx_event
                .send(EngineEvent::GoalUpdated { snapshot })
                .await;
            let _ = failed_tx_event
                .send(EngineEvent::TurnStarted {
                    turn_id: "engine_goal_failed".to_string(),
                    created_at: chrono::Utc::now(),
                    route: None,
                    submission_id: None,
                })
                .await;
            let _ = failed_tx_event
                .send(EngineEvent::TurnComplete {
                    turn_id: None,
                    submission_id: None,
                    compaction_id: None,
                    usage: Usage {
                        input_tokens: 5,
                        output_tokens: 5,
                        ..Usage::default()
                    },
                    parent_route_usage: Usage {
                        input_tokens: 5,
                        output_tokens: 5,
                        ..Usage::default()
                    },
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Failed,
                    error: Some("provider exploded".to_string()),
                    tool_catalog: Some(vec![catalog_tool("update_goal")]),
                    base_url: None,
                })
                .await;
        }
    });
    manager.activate_thread_goal(&failed_thread.id).await?;
    let failed_turns = wait_for_terminal_turn_count(
        &manager,
        &failed_thread.id,
        1,
        TURN_SETTLEMENT_DEADLOCK_TIMEOUT,
    )
    .await?;
    assert_eq!(failed_turns[0].status, RuntimeTurnStatus::Failed);
    sleep(Duration::from_millis(400)).await;
    assert_eq!(
        failed_passes.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a failed pass must not arm a continuation"
    );
    let failed_goal = manager
        .store
        .load_goal(&failed_thread.id)?
        .ok_or_else(|| anyhow::anyhow!("failed-pass goal record missing"))?;
    assert_eq!(
        failed_goal.status,
        codewhale_protocol::ThreadGoalStatus::Active
    );
    Ok(())
}

#[tokio::test]
async fn host_goal_loop_mirrors_terminal_snapshot_and_does_not_rearm() -> Result<()> {
    // A completed pass whose engine snapshot says `complete` (or `blocked`)
    // is mirrored into the durable record and arms nothing more: a restarted
    // host must not resume a goal the verifier already closed.
    let manager = test_manager(test_runtime_dir())?;
    for (goal_id, engine_status, expected) in [
        (
            "goal_complete",
            "complete",
            codewhale_protocol::ThreadGoalStatus::Complete,
        ),
        (
            "goal_blocked",
            "blocked",
            codewhale_protocol::ThreadGoalStatus::Blocked,
        ),
    ] {
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        manager
            .store
            .save_goal(&active_test_goal(&thread.id, goal_id))?;
        let harness = install_mock_engine(&manager, &thread.id).await;
        let mut rx_op = harness.rx_op;
        let tx_event = harness.tx_event;
        let passes = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0));
        let counter = passes.clone();
        let status = engine_status.to_string();
        tokio::spawn(async move {
            while let Some(op) = rx_op.recv().await {
                if !matches!(op, Op::SendMessage(TurnSpec { .. })) {
                    continue;
                }
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let snapshot = crate::tools::goal::GoalSnapshot {
                    objective: Some("ship the goal loop".to_string()),
                    status: status.clone(),
                    continuation_count: 1,
                    ..Default::default()
                };
                let _ = tx_event.send(EngineEvent::GoalUpdated { snapshot }).await;
                let _ = tx_event
                    .send(EngineEvent::TurnStarted {
                        turn_id: format!("engine_{status}"),
                        created_at: chrono::Utc::now(),
                        route: None,
                        submission_id: None,
                    })
                    .await;
                let _ = tx_event
                    .send(EngineEvent::TurnComplete {
                        turn_id: None,
                        submission_id: None,
                        compaction_id: None,
                        usage: Usage {
                            input_tokens: 7,
                            output_tokens: 3,
                            ..Usage::default()
                        },
                        parent_route_usage: Usage {
                            input_tokens: 7,
                            output_tokens: 3,
                            ..Usage::default()
                        },
                        routed_usage_dropped_records: 0,
                        status: TurnOutcomeStatus::Completed,
                        error: None,
                        tool_catalog: Some(vec![catalog_tool("update_goal")]),
                        base_url: None,
                    })
                    .await;
            }
        });
        manager.activate_thread_goal(&thread.id).await?;
        // The mid-turn checkpoint publishes the terminal status before
        // settlement accrues turn usage, so wait for both the terminal turn
        // and the settled counters rather than asserting on the first
        // status sighting.
        wait_for_terminal_turn_count(&manager, &thread.id, 1, TURN_SETTLEMENT_DEADLOCK_TIMEOUT)
            .await?;
        let deadline = Instant::now() + TURN_SETTLEMENT_DEADLOCK_TIMEOUT;
        let goal = loop {
            let goal = manager
                .store
                .load_goal(&thread.id)?
                .ok_or_else(|| anyhow::anyhow!("goal record missing for {}", thread.id))?;
            if goal.status == expected && goal.tokens_used == 10 {
                break goal;
            }
            if Instant::now() > deadline {
                bail!("goal settlement did not land in time: {goal:?}");
            }
            sleep(Duration::from_millis(20)).await;
        };
        // Usage and the engine's continuation counter landed on the record.
        assert_eq!(goal.tokens_used, 10);
        assert_eq!(goal.continuation_count, 1);
        sleep(Duration::from_millis(400)).await;
        assert_eq!(
            passes.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a terminal snapshot must not arm another pass ({engine_status})"
        );
        let turns = manager.store.list_turns_for_thread(&thread.id)?;
        assert_eq!(
            turns.len(),
            1,
            "exactly the kickoff turn ran ({engine_status})"
        );
    }
    Ok(())
}

#[tokio::test]
async fn model_created_goal_persists_through_adopted_revision() -> Result<()> {
    // A turn admitted with no host goal can still create one through the
    // model's create_goal tool. The monitor must persist that creation and
    // adopt its revision, so settlement accrues usage and the next admission
    // restores the goal instead of clearing it.
    let manager = RuntimeThreadManager::open(
        {
            let __cw_config_init_27_0 = Some(codewhale_runtime::config::GoalConfig {
                max_continuations: None,
                continuation_delay_seconds: Some(3600),
                max_steps: None,
                enforce_token_budget: None,
            });
            let mut __cw_config_init_27_value: Config = Config::default();
            __cw_config_init_27_value.goal = __cw_config_init_27_0;
            __cw_config_init_27_value
        },
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    tokio::spawn(async move {
        while let Some(op) = rx_op.recv().await {
            if !matches!(op, Op::SendMessage(TurnSpec { .. })) {
                continue;
            }
            let snapshot = crate::tools::goal::GoalSnapshot {
                goal_id: Some("model_created_goal".to_string()),
                objective: Some("model-built objective".to_string()),
                status: "active".to_string(),
                ..Default::default()
            };
            let _ = tx_event.send(EngineEvent::GoalUpdated { snapshot }).await;
            let _ = tx_event
                .send(EngineEvent::TurnStarted {
                    turn_id: "engine_model_created".to_string(),
                    created_at: chrono::Utc::now(),
                    route: None,
                    submission_id: None,
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::TurnComplete {
                    turn_id: None,
                    submission_id: None,
                    compaction_id: None,
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 10,
                        ..Usage::default()
                    },
                    parent_route_usage: Usage {
                        input_tokens: 10,
                        output_tokens: 10,
                        ..Usage::default()
                    },
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Completed,
                    error: None,
                    tool_catalog: Some(vec![catalog_tool("update_goal")]),
                    base_url: None,
                })
                .await;
        }
    });
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "build me a goal".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    wait_for_terminal_turn(&manager, &turn.id).await?;
    let deadline = Instant::now() + TURN_SETTLEMENT_DEADLOCK_TIMEOUT;
    let goal = loop {
        match manager.store.load_goal(&thread.id)? {
            Some(goal) if goal.tokens_used == 20 => break goal,
            _ if Instant::now() > deadline => {
                bail!("model-created goal was not persisted and settled")
            }
            _ => sleep(Duration::from_millis(20)).await,
        }
    };
    assert_eq!(goal.goal_id, "model_created_goal");
    assert_eq!(goal.objective, "model-built objective");
    assert_eq!(goal.status, codewhale_protocol::ThreadGoalStatus::Active);
    Ok(())
}

#[tokio::test]
async fn model_created_goal_never_overwrites_concurrent_explicit_goal() -> Result<()> {
    // The turn was admitted with no goal; an explicit PUT lands while it runs.
    // The model's create_goal snapshot must not clobber that newer revision,
    // and terminal settlement must stay fenced off it.
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    let store = manager.store.clone();
    let thread_id = thread.id.clone();
    tokio::spawn(async move {
        while let Some(op) = rx_op.recv().await {
            if !matches!(op, Op::SendMessage(TurnSpec { .. })) {
                continue;
            }
            // The explicit revision arrives after admission but before the
            // model's create_goal receipt.
            let _ = store.save_goal(&active_test_goal(&thread_id, "goal_explicit"));
            let snapshot = crate::tools::goal::GoalSnapshot {
                goal_id: Some("model_created_goal".to_string()),
                objective: Some("model-built objective".to_string()),
                status: "active".to_string(),
                ..Default::default()
            };
            let _ = tx_event.send(EngineEvent::GoalUpdated { snapshot }).await;
            let _ = tx_event
                .send(EngineEvent::TurnStarted {
                    turn_id: "engine_concurrent".to_string(),
                    created_at: chrono::Utc::now(),
                    route: None,
                    submission_id: None,
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::TurnComplete {
                    turn_id: None,
                    submission_id: None,
                    compaction_id: None,
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 10,
                        ..Usage::default()
                    },
                    parent_route_usage: Usage {
                        input_tokens: 10,
                        output_tokens: 10,
                        ..Usage::default()
                    },
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Completed,
                    error: None,
                    tool_catalog: Some(vec![catalog_tool("update_goal")]),
                    base_url: None,
                })
                .await;
        }
    });
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "build me a goal".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    wait_for_terminal_turn(&manager, &turn.id).await?;
    sleep(Duration::from_millis(200)).await;
    let goal = manager
        .store
        .load_goal(&thread.id)?
        .ok_or_else(|| anyhow::anyhow!("explicit goal vanished"))?;
    assert_eq!(goal.goal_id, "goal_explicit");
    assert_eq!(goal.objective, "ship the goal loop");
    assert_eq!(
        goal.tokens_used, 0,
        "settlement must not accrue the no-goal turn onto the explicit revision"
    );
    Ok(())
}

#[tokio::test]
async fn live_goal_progress_is_revision_fenced_and_settled_once() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut goal = active_test_goal(&thread.id, "goal_live");
    goal.tokens_used = 100;
    manager.store.save_goal(&goal)?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    manager.activate_thread_goal(&thread.id).await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(_))
    ));
    let mut progress = crate::tools::goal::GoalSnapshot::from_thread_goal(&goal);
    progress.tokens_used = 120;
    progress.continuation_count = 1;
    harness
        .tx_event
        .send(EngineEvent::GoalUpdated { snapshot: progress })
        .await?;
    tokio::time::timeout(TURN_SETTLEMENT_DEADLOCK_TIMEOUT, async {
        loop {
            if manager
                .get_goal(&thread.id)
                .await?
                .is_some_and(|goal| goal.tokens_used == 120)
                && manager.events_since(&thread.id, None)?.iter().any(|event| {
                    event.event == "thread_goal_updated"
                        && event.payload["goal"]["tokens_used"] == 120
                })
            {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    assert_eq!(
        manager.store.load_goal(&thread.id)?.unwrap().tokens_used,
        100
    );
    assert!(
        manager
            .events_since(&thread.id, None)?
            .iter()
            .any(|event| event.event == "thread_goal_updated"
                && event.payload["goal"]["tokens_used"] == 120)
    );
    let replacement = active_test_goal(&thread.id, "goal_new");
    manager.store.save_goal(&replacement)?;
    assert_eq!(manager.get_goal(&thread.id).await?.unwrap().tokens_used, 0);
    manager.remove_goal(&thread.id).await?;
    assert!(manager.get_goal(&thread.id).await?.is_none());
    manager.store.save_goal(&goal)?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 10,
                ..Usage::default()
            },
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: Some(vec![]),
            base_url: None,
        })
        .await?;
    wait_for_terminal_turn_count(&manager, &thread.id, 1, TURN_SETTLEMENT_DEADLOCK_TIMEOUT).await?;
    tokio::time::timeout(TURN_SETTLEMENT_DEADLOCK_TIMEOUT, async {
        loop {
            if manager.store.load_goal(&thread.id)?.unwrap().tokens_used == 120 {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    assert_eq!(
        manager.get_goal(&thread.id).await?.unwrap().tokens_used,
        120
    );

    // A previous turn's projection must never leak onto a replacement with
    // the same objective or recreate a deleted goal.
    let replacement = active_test_goal(&thread.id, "goal_new");
    manager.store.save_goal(&replacement)?;
    assert_eq!(manager.get_goal(&thread.id).await?.unwrap().tokens_used, 0);
    manager.remove_goal(&thread.id).await?;
    assert!(manager.get_goal(&thread.id).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn startup_discards_an_unpublished_partial_seed_and_keeps_a_committed_one() -> Result<()> {
    let runtime_dir = test_runtime_dir();
    let manager = test_manager(runtime_dir.clone())?;
    let messages: Vec<Message> = serde_json::from_value(json!([
        {"role":"user","content":[{"type":"text","text":"SEEDED"}]},
        {"role":"assistant","content":[{"type":"text","text":"SEEDED ANSWER"}]}
    ]))?;
    let mut fixtures = Vec::new();
    for _ in 0..2 {
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        manager
            .seed_thread_from_messages(&thread.id, &messages)
            .await?;
        assert!(
            !manager.store.seed_journal_path(&thread.id)?.exists(),
            "a committed seed leaves no journal"
        );
        let turns = manager.store.list_turns_for_thread(&thread.id)?;
        assert_eq!(turns.len(), 1);
        let items = manager.store.list_items_for_turn(&turns[0].id)?;
        assert_eq!(items.len(), 2);
        // Fault fixture: the journal the seed wrote before its first record.
        manager.store.save_seed_journal(&SeedJournal {
            thread_id: thread.id.clone(),
            previous_latest_turn_id: None,
            turn_ids: turns.iter().map(|turn| turn.id.clone()).collect(),
            item_ids: items.iter().map(|item| item.id.clone()).collect(),
        })?;
        fixtures.push((thread.id, turns, items));
    }
    // The first thread crashed before its commit record: every turn and item
    // is on disk, but the thread pointer never advanced.
    let (partial_id, partial_turns, partial_items) = &fixtures[0];
    let mut uncommitted = manager.store.load_thread(partial_id)?;
    uncommitted.latest_turn_id = None;
    manager.store.save_thread(&uncommitted)?;
    drop(manager);

    let reopened = test_manager(runtime_dir)?;
    assert!(
        reopened.store.list_turns_for_thread(partial_id)?.is_empty(),
        "an unpublished partial seed must not be restored as history"
    );
    for item in partial_items {
        assert!(!reopened.store.item_path(&item.id)?.exists());
    }
    assert!(!reopened.store.turn_path(&partial_turns[0].id)?.exists());
    assert_eq!(reopened.store.load_thread(partial_id)?.latest_turn_id, None);
    assert!(!reopened.store.seed_journal_path(partial_id)?.exists());

    // The second crashed after its commit, before removing the journal.
    let (committed_id, committed_turns, _) = &fixtures[1];
    assert_eq!(
        reopened.store.list_turns_for_thread(committed_id)?.len(),
        1,
        "a committed seed survives recovery"
    );
    assert_eq!(
        reopened.store.load_thread(committed_id)?.latest_turn_id,
        Some(committed_turns[0].id.clone())
    );
    assert!(!reopened.store.seed_journal_path(committed_id)?.exists());
    Ok(())
}

/// The next turn a thread admits after its seed, as it lands on disk,
/// without running one.
fn turn_after_seed(seeded: &TurnRecord, id: &str) -> TurnRecord {
    let mut later = seeded.clone();
    later.id = id.to_string();
    later.created_at = seeded.created_at + chrono::Duration::seconds(1);
    later.item_ids = Vec::new();
    later
}

/// One uncertain seed journal quarantines only its own thread: the store
/// opens, that thread's journal and records are left byte-for-byte for
/// repair (no partial seed is published or deleted), and every other thread
/// still recovers.
#[tokio::test]
async fn startup_quarantines_only_the_thread_whose_seed_journal_is_uncertain() -> Result<()> {
    for failure in [
        "unreadable-journal",
        "wrong-thread",
        "unreadable-thread",
        "wrong-commit",
        "seed-record-missing-beneath-later-turn",
    ] {
        let runtime_dir = test_runtime_dir();
        let manager = test_manager(runtime_dir.clone())?;
        let messages: Vec<Message> = serde_json::from_value(json!([
            {"role":"user","content":[{"type":"text","text":"seed recovery canary"}]},
            {"role":"assistant","content":[{"type":"text","text":"canary answer"}]}
        ]))?;
        // A healthy neighbour whose pointer recovery must still recompute.
        let healthy = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        manager
            .seed_thread_from_messages(&healthy.id, &messages)
            .await?;
        let healthy_turn = manager.store.list_turns_for_thread(&healthy.id)?[0]
            .id
            .clone();
        let mut stale = manager.store.load_thread(&healthy.id)?;
        stale.latest_turn_id = None;
        manager.store.save_thread(&stale)?;

        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        manager
            .seed_thread_from_messages(&thread.id, &messages)
            .await?;
        let turns = manager.store.list_turns_for_thread(&thread.id)?;
        let mut items = manager.store.list_items_for_turn(&turns[0].id)?;
        let mut uncommitted = manager.store.load_thread(&thread.id)?;
        uncommitted.latest_turn_id = None;
        manager.store.save_thread(&uncommitted)?;
        let journal = SeedJournal {
            thread_id: thread.id.clone(),
            previous_latest_turn_id: None,
            turn_ids: turns.iter().map(|turn| turn.id.clone()).collect(),
            item_ids: items.iter().map(|item| item.id.clone()).collect(),
        };
        manager.store.save_seed_journal(&journal)?;
        let journal_path = manager.store.seed_journal_path(&thread.id)?;
        let thread_path = manager.store.thread_path(&thread.id)?;
        let mut paths = vec![journal_path.clone(), thread_path.clone()];
        match failure {
            "unreadable-journal" => fs::write(&journal_path, b"{truncated seed intent")?,
            "wrong-thread" => {
                let mut wrong = serde_json::to_value(&journal)?;
                wrong["thread_id"] = json!("another-thread");
                fs::write(&journal_path, serde_json::to_vec(&wrong)?)?;
            }
            "unreadable-thread" => fs::write(&thread_path, b"{truncated thread")?,
            "wrong-commit" => {
                uncommitted.latest_turn_id = Some("unrelated-turn".into());
                manager.store.save_thread(&uncommitted)?;
            }
            "seed-record-missing-beneath-later-turn" => {
                // The pointer moved past the seed, but the seed is no longer
                // whole: nothing proves it committed.
                let later = turn_after_seed(&turns[0], "turn_after_seed");
                manager.store.save_turn(&later)?;
                paths.push(manager.store.turn_path(&later.id)?);
                uncommitted.latest_turn_id = Some(later.id);
                manager.store.save_thread(&uncommitted)?;
                let missing = items.pop().expect("seeded item");
                fs::remove_file(manager.store.item_path(&missing.id)?)?;
            }
            _ => unreachable!(),
        }
        for turn in &turns {
            paths.push(manager.store.turn_path(&turn.id)?);
        }
        for item in &items {
            paths.push(manager.store.item_path(&item.id)?);
        }
        let original = paths
            .iter()
            .map(fs::read)
            .collect::<std::io::Result<Vec<_>>>()?;
        drop(manager);

        let reopened = test_manager(runtime_dir)
            .with_context(|| format!("{failure}: one uncertain journal must not fail the store"))?;
        for (path, bytes) in paths.iter().zip(original) {
            assert_eq!(
                fs::read(path)?,
                bytes,
                "{failure}: retain {}",
                path.display()
            );
        }
        assert_eq!(
            reopened.store.load_thread(&healthy.id)?.latest_turn_id,
            Some(healthy_turn),
            "{failure}: every other thread still recovers"
        );
    }
    Ok(())
}

/// The seed committed, removing its journal failed, and the conversation
/// went on. The stale journal is settled as committed: removed, with the
/// whole seed kept as history beneath the later turn.
#[tokio::test]
async fn startup_keeps_a_committed_seed_whose_journal_outlived_later_turns() -> Result<()> {
    let runtime_dir = test_runtime_dir();
    let manager = test_manager(runtime_dir.clone())?;
    let messages: Vec<Message> = serde_json::from_value(json!([
        {"role":"user","content":[{"type":"text","text":"SEEDED"}]},
        {"role":"assistant","content":[{"type":"text","text":"SEEDED ANSWER"}]}
    ]))?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    manager
        .seed_thread_from_messages(&thread.id, &messages)
        .await?;
    let turns = manager.store.list_turns_for_thread(&thread.id)?;
    let items = manager.store.list_items_for_turn(&turns[0].id)?;
    manager.store.save_seed_journal(&SeedJournal {
        thread_id: thread.id.clone(),
        previous_latest_turn_id: None,
        turn_ids: turns.iter().map(|turn| turn.id.clone()).collect(),
        item_ids: items.iter().map(|item| item.id.clone()).collect(),
    })?;
    let later = turn_after_seed(&turns[0], "turn_after_seed");
    manager.store.save_turn(&later)?;
    let mut moved_on = manager.store.load_thread(&thread.id)?;
    moved_on.latest_turn_id = Some(later.id.clone());
    manager.store.save_thread(&moved_on)?;
    drop(manager);

    let reopened = test_manager(runtime_dir)?;
    assert!(
        !reopened.store.seed_journal_path(&thread.id)?.exists(),
        "a committed seed's stale journal is removed"
    );
    assert_eq!(
        reopened.store.list_turns_for_thread(&thread.id)?.len(),
        2,
        "the seed stays history beneath the later turn"
    );
    for item in &items {
        assert!(reopened.store.item_path(&item.id)?.exists());
    }
    assert_eq!(
        reopened.store.load_thread(&thread.id)?.latest_turn_id,
        Some(later.id)
    );
    Ok(())
}

/// Missing records are not uncertainty: a journal whose thread record is
/// gone (and some of whose own records already are) is an unpublished seed,
/// and its remaining records are discarded.
#[tokio::test]
async fn startup_discards_a_seed_whose_thread_and_records_are_already_gone() -> Result<()> {
    let runtime_dir = test_runtime_dir();
    let manager = test_manager(runtime_dir.clone())?;
    let messages: Vec<Message> = serde_json::from_value(json!([
        {"role":"user","content":[{"type":"text","text":"orphaned seed"}]},
        {"role":"assistant","content":[{"type":"text","text":"orphaned answer"}]}
    ]))?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    manager
        .seed_thread_from_messages(&thread.id, &messages)
        .await?;
    let turns = manager.store.list_turns_for_thread(&thread.id)?;
    let items = manager.store.list_items_for_turn(&turns[0].id)?;
    manager.store.save_seed_journal(&SeedJournal {
        thread_id: thread.id.clone(),
        previous_latest_turn_id: None,
        turn_ids: turns.iter().map(|turn| turn.id.clone()).collect(),
        item_ids: items.iter().map(|item| item.id.clone()).collect(),
    })?;
    fs::remove_file(manager.store.thread_path(&thread.id)?)?;
    fs::remove_file(manager.store.item_path(&items[0].id)?)?;
    drop(manager);

    let reopened = test_manager(runtime_dir)?;
    assert!(!reopened.store.seed_journal_path(&thread.id)?.exists());
    assert!(!reopened.store.turn_path(&turns[0].id)?.exists());
    for item in &items {
        assert!(!reopened.store.item_path(&item.id)?.exists());
    }
    Ok(())
}

#[tokio::test]
async fn seed_cleanup_validates_every_record_before_removing_anything() -> Result<()> {
    for foreign in ["turn", "item"] {
        let manager = test_manager(test_runtime_dir())?;
        let messages: Vec<Message> = serde_json::from_value(json!([
            {"role":"user","content":[{"type":"text","text":"keep both histories"}]}
        ]))?;
        let mut histories = Vec::new();
        for _ in 0..2 {
            let thread = manager
                .create_thread(CreateThreadRequest::default())
                .await?;
            manager
                .seed_thread_from_messages(&thread.id, &messages)
                .await?;
            let turns = manager.store.list_turns_for_thread(&thread.id)?;
            let items = manager.store.list_items_for_turn(&turns[0].id)?;
            histories.push((thread, turns, items));
        }
        let (thread, turns, items) = &histories[0];
        let mut journal = SeedJournal {
            thread_id: thread.id.clone(),
            previous_latest_turn_id: None,
            turn_ids: turns.iter().map(|turn| turn.id.clone()).collect(),
            item_ids: items.iter().map(|item| item.id.clone()).collect(),
        };
        if foreign == "turn" {
            journal.turn_ids.push(histories[1].1[0].id.clone());
        } else {
            journal.item_ids.push(histories[1].2[0].id.clone());
        }
        manager.store.save_seed_journal(&journal)?;
        let mut paths = vec![manager.store.seed_journal_path(&thread.id)?];
        for (thread, turns, items) in &histories {
            paths.push(manager.store.thread_path(&thread.id)?);
            for turn in turns {
                paths.push(manager.store.turn_path(&turn.id)?);
            }
            for item in items {
                paths.push(manager.store.item_path(&item.id)?);
            }
        }
        let original = paths
            .iter()
            .map(fs::read)
            .collect::<std::io::Result<Vec<_>>>()?;
        let error = manager
            .store
            .discard_seed(&journal)
            .expect_err("foreign history is not cleanup ownership");
        assert!(format!("{error:#}").contains("outside"), "{error:#}");
        for (path, bytes) in paths.iter().zip(original) {
            assert_eq!(
                fs::read(path)?,
                bytes,
                "validate the whole set before removing {}",
                path.display()
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn transition_goal_status_commits_only_the_read_revision() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    manager
        .store
        .save_goal(&active_test_goal(&thread.id, "goal_a"))?;

    let completed = manager
        .transition_goal_status(
            &thread.id,
            "goal_a",
            codewhale_protocol::ThreadGoalStatus::Active,
            codewhale_protocol::ThreadGoalStatus::Complete,
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("current revision must commit"))?;
    assert_eq!(
        completed.status,
        codewhale_protocol::ThreadGoalStatus::Complete
    );

    // A Blocked transition decided from the earlier Active read races the
    // Complete above; the terminal state must survive it.
    let raced = manager
        .transition_goal_status(
            &thread.id,
            "goal_a",
            codewhale_protocol::ThreadGoalStatus::Active,
            codewhale_protocol::ThreadGoalStatus::Blocked,
        )
        .await?;
    assert!(raced.is_none(), "a stale status read must not commit");
    assert_eq!(
        manager
            .store
            .load_goal(&thread.id)?
            .ok_or_else(|| anyhow::anyhow!("goal record missing"))?
            .status,
        codewhale_protocol::ThreadGoalStatus::Complete
    );

    // A concurrent replacement makes the previously read revision stale.
    manager
        .store
        .save_goal(&active_test_goal(&thread.id, "goal_b"))?;
    let stale = manager
        .transition_goal_status(
            &thread.id,
            "goal_a",
            codewhale_protocol::ThreadGoalStatus::Active,
            codewhale_protocol::ThreadGoalStatus::Blocked,
        )
        .await?;
    assert!(stale.is_none(), "stale revision must not commit");
    let goal = manager
        .store
        .load_goal(&thread.id)?
        .ok_or_else(|| anyhow::anyhow!("goal record missing"))?;
    assert_eq!(goal.goal_id, "goal_b");
    assert_eq!(goal.status, codewhale_protocol::ThreadGoalStatus::Active);
    Ok(())
}

#[test]
fn corrupt_active_exhausted_stall_window_loads_paused() -> Result<()> {
    // The engine pauses NoProgress in the same locked mutation that fills the
    // stall window, so a persisted Active record at the ceiling is corrupt;
    // loads must restore it paused instead of re-arming spent passes.
    let manager = test_manager(test_runtime_dir())?;
    let thread_id = "thread_corrupt_stall";
    let mut goal = active_test_goal(thread_id, "goal_corrupt");
    goal.repeated_gap_count = codewhale_protocol::MAX_REPEATED_GAP_COUNT;
    goal.last_gap_fingerprint = Some("a".repeat(64));
    goal.last_gap_pass = Some(3);
    goal.continuation_count = 3;
    manager.store.save_goal(&goal)?;

    let loaded = manager
        .store
        .load_goal(thread_id)?
        .ok_or_else(|| anyhow::anyhow!("goal record missing"))?;
    assert_eq!(loaded.status, codewhale_protocol::ThreadGoalStatus::Paused);
    assert_eq!(
        loaded.pause_reason,
        Some(codewhale_protocol::GoalPauseReason::NoProgress)
    );
    // The stall history itself is preserved for inspection; only the
    // impossible Active projection is healed.
    assert_eq!(
        loaded.repeated_gap_count,
        codewhale_protocol::MAX_REPEATED_GAP_COUNT
    );
    Ok(())
}

#[tokio::test]
async fn get_thread_detail_batches_items_by_turn_without_losing_order() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let base = Utc::now();
    let mut first_turn = sample_turn(
        &thread.id,
        "turn_detail_batch_first",
        RuntimeTurnStatus::Completed,
    );
    first_turn.created_at = base;
    let mut second_turn = sample_turn(
        &thread.id,
        "turn_detail_batch_second",
        RuntimeTurnStatus::Completed,
    );
    second_turn.created_at = base + chrono::Duration::seconds(1);
    manager.store.save_turn(&first_turn)?;
    manager.store.save_turn(&second_turn)?;

    let mut first_late = sample_item(
        &first_turn.id,
        "item_detail_first_late",
        TurnItemLifecycleStatus::Completed,
    );
    first_late.started_at = Some(base + chrono::Duration::seconds(5));
    let mut first_early = sample_item(
        &first_turn.id,
        "item_detail_first_early",
        TurnItemLifecycleStatus::Completed,
    );
    first_early.started_at = Some(base + chrono::Duration::seconds(1));
    let mut second_item = sample_item(
        &second_turn.id,
        "item_detail_second",
        TurnItemLifecycleStatus::Completed,
    );
    second_item.started_at = Some(base + chrono::Duration::seconds(2));
    let unrelated = sample_item(
        "turn_detail_batch_unrelated",
        "item_detail_unrelated",
        TurnItemLifecycleStatus::Completed,
    );

    manager.store.save_item(&first_late)?;
    manager.store.save_item(&second_item)?;
    manager.store.save_item(&unrelated)?;
    manager.store.save_item(&first_early)?;

    let detail = manager.get_thread_detail(&thread.id).await?;
    let item_ids: Vec<&str> = detail.items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(
        item_ids,
        vec![
            "item_detail_first_early",
            "item_detail_first_late",
            "item_detail_second"
        ]
    );
    Ok(())
}

#[tokio::test]
async fn interrupt_turn_marks_interrupted_after_cleanup() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    let cancel_token = harness.cancel_token;
    let cleanup_delay = Duration::from_millis(140);
    tokio::spawn(async move {
        if matches!(rx_op.recv().await, Some(Op::SendMessage(TurnSpec { .. }))) {
            let _ = tx_event
                .send(EngineEvent::TurnStarted {
                    turn_id: "engine_turn_interrupt".to_string(),
                    created_at: chrono::Utc::now(),
                    route: None,
                    submission_id: None,
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::MessageStarted { index: 0 })
                .await;
            let _ = tx_event
                .send(EngineEvent::MessageDelta {
                    index: 0,
                    content: "partial".to_string(),
                })
                .await;
            cancel_token.cancelled().await;
            sleep(cleanup_delay).await;
        }
    });

    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "interrupt me".to_string(),
                input_summary: None,
                model: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                ..Default::default()
            },
        )
        .await?;

    sleep(Duration::from_millis(20)).await;
    let interrupted_at = Instant::now();
    let interrupt_result = manager.interrupt_turn(&thread.id, &turn.id).await?;
    assert_eq!(interrupt_result.status, RuntimeTurnStatus::InProgress);

    let final_turn = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(final_turn.status, RuntimeTurnStatus::Interrupted);
    assert!(
        interrupted_at.elapsed() >= cleanup_delay,
        "turn transitioned before cleanup finished"
    );

    let events = manager.events_since(&thread.id, None)?;
    let interrupt_seq = events
        .iter()
        .find(|ev| ev.event == "turn.interrupt_requested")
        .map(|ev| ev.seq)
        .context("missing turn.interrupt_requested event")?;
    let completed = events
        .iter()
        .find(|ev| ev.event == "turn.completed")
        .context("missing turn.completed event")?;
    assert!(completed.seq > interrupt_seq);
    assert_eq!(
        completed
            .payload
            .get("turn")
            .and_then(|turn| turn.get("status"))
            .and_then(Value::as_str),
        Some("interrupted")
    );
    Ok(())
}

#[tokio::test]
async fn approval_required_with_stale_active_turn_is_denied() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: Some(true),
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "needs approval".to_string(),
                input_summary: None,
                model: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: Some(true),
                ..Default::default()
            },
        )
        .await?;

    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    let active_turn = {
        let mut active = manager.active.lock().await;
        let state = active
            .engines
            .get_mut(&thread.id)
            .context("missing active thread state")?;
        state.active_turn.take().context("missing active turn")?
    };

    harness
        .tx_event
        .send(EngineEvent::ApprovalRequired {
            approval_key: "test_key".to_string(),
            approval_grouping_key: "test_key".to_string(),
            id: "tool_stale".to_string(),
            tool_name: "exec_command".to_string(),
            description: "stale approval".to_string(),
            input: serde_json::json!({}),
            intent_summary: None,
            approval_force_prompt: false,
        })
        .await?;

    assert_eq!(
        harness.recv_approval_event().await,
        Some(MockApprovalEvent::Denied {
            id: "tool_stale".to_string(),
        })
    );

    manager
        .active
        .lock()
        .await
        .engines
        .get_mut(&thread.id)
        .context("missing active thread state")?
        .active_turn = Some(active_turn);

    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage {
                input_tokens: 0,
                output_tokens: 0,
                ..Usage::default()
            },
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;

    let terminal = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Failed);
    assert_eq!(terminal.error.as_deref(), Some(EMPTY_TURN_REASON));
    Ok(())
}

/// Readiness ceiling for an approval to be persisted, registered, or resolved.
/// Every wait under it ends as soon as its condition holds, so it only bounds
/// how long a stuck case takes to fail; a loaded shared-process `cargo test`
/// run overran the earlier 2s ceiling (#6698).
const APPROVAL_READINESS_TIMEOUT: Duration = Duration::from_secs(10);

/// Reads the approval identity exactly as an external client does: off the
/// `approval.required` event for `raw_call_id`, returning the opaque ID that
/// client must echo back. Also pins the two properties every caller below
/// depends on — the opaque ID is never the provider's call ID, and the legacy
/// `id` field aliases the opaque ID rather than leaking the raw one.
async fn await_approval_identity(
    manager: &RuntimeThreadManager,
    thread_id: &str,
    raw_call_id: &str,
) -> Result<String> {
    let deadline = Instant::now() + APPROVAL_READINESS_TIMEOUT;
    loop {
        let found = manager
            .events_since(thread_id, None)?
            .into_iter()
            .rev()
            .find(|event| {
                event.event == "approval.required"
                    && event.payload.get("tool_call_id").and_then(Value::as_str)
                        == Some(raw_call_id)
            });
        if let Some(event) = found {
            let approval_id = event
                .payload
                .get("approval_id")
                .and_then(Value::as_str)
                .context("approval.required must carry an approval_id")?
                .to_string();
            assert_eq!(
                event.payload.get("id").and_then(Value::as_str),
                Some(approval_id.as_str()),
                "the legacy `id` field must alias the opaque approval id"
            );
            assert_ne!(
                approval_id, raw_call_id,
                "the approval capability must not be the provider's tool-call id"
            );
            assert!(
                approval_id.starts_with("approval_"),
                "unexpected approval id shape: {approval_id}"
            );
            return Ok(approval_id);
        }
        if Instant::now() >= deadline {
            bail!("no approval.required event for tool call '{raw_call_id}' on '{thread_id}'");
        }
        sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn approval_required_awaits_external_decision_allow() -> Result<()> {
    for (call_id, tool_name, input) in [
        (
            "tool_external_allow",
            "exec_command",
            json!({"command":"echo fixture","metadata":{"values":[1,true,null]}}),
        ),
        (
            "agent:worker_fixture:approval:00000000-0000-4000-8000-000000000001",
            "write",
            json!({"path":"proof/shared.txt","content":"fixture\n"}),
        ),
        ("tool_external_null", "fixture_null", Value::Null),
    ] {
        let manager = test_manager(test_runtime_dir())?;
        let thread = manager
            .create_thread(CreateThreadRequest {
                model: None,
                workspace: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                archived: false,
                system_prompt: None,
                task_id: None,
                ..Default::default()
            })
            .await?;

        let mut harness = install_mock_engine(&manager, &thread.id).await;
        let _turn = manager
            .start_turn(
                &thread.id,
                StartTurnRequest {
                    prompt: "needs approval".to_string(),
                    input_summary: None,
                    model: None,
                    mode: None,
                    allow_shell: None,
                    trust_mode: None,
                    auto_approve: None,
                    ..Default::default()
                },
            )
            .await?;
        assert!(matches!(
            harness.rx_op.recv().await,
            Some(Op::SendMessage(TurnSpec { .. }))
        ));

        harness
            .tx_event
            .send(EngineEvent::ApprovalRequired {
                approval_key: "key1".to_string(),
                approval_grouping_key: "key1".to_string(),
                id: call_id.to_string(),
                tool_name: tool_name.to_string(),
                description: "external allow".to_string(),
                input: input.clone(),
                intent_summary: Some("I will update the config file.".to_string()),
                approval_force_prompt: false,
            })
            .await?;

        let deadline = Instant::now() + APPROVAL_READINESS_TIMEOUT;
        while Instant::now() < deadline && manager.pending_approvals_count() == 0 {
            sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(manager.pending_approvals_count(), 1);

        let approval_id = await_approval_identity(&manager, &thread.id, call_id).await?;

        // The snapshot a reconnecting client resumes from carries the same opaque
        // capability as the event, so both routes to the ID agree.
        let detail = manager.get_thread_detail(&thread.id).await?;
        assert_eq!(detail.pending_approvals.len(), 1);
        assert_eq!(detail.pending_approvals[0].id, approval_id);
        assert_eq!(detail.pending_approvals[0].turn_id, _turn.id);
        assert_eq!(detail.pending_approvals[0].tool_name, tool_name);
        // The snapshot keeps the raw correlator so a client that reloaded past the
        // event can still attach this prompt to the tool row it gates.
        assert_eq!(
            detail.pending_approvals[0].tool_call_id.as_deref(),
            Some(call_id)
        );
        assert_eq!(detail.pending_approvals[0].input.as_ref(), Some(&input));
        let serialized = serde_json::to_value(&detail.pending_approvals[0])?;
        assert_eq!(serialized.get("input"), Some(&input));
        let reloaded: PendingApprovalRequest = serde_json::from_value(serialized.clone())?;
        assert_eq!(reloaded.input, Some(input.clone()));
        let mut legacy = serialized;
        legacy
            .as_object_mut()
            .expect("approval object")
            .remove("input");
        let legacy: PendingApprovalRequest = serde_json::from_value(legacy)?;
        assert_eq!(legacy.input, None);
        assert!(serde_json::to_value(legacy)?.get("input").is_none());
        assert_eq!(detail.pending_user_inputs.len(), 0);

        let events = manager.events_since(&thread.id, None)?;
        let approval_event = events
            .iter()
            .rev()
            .find(|event| event.event == "approval.required")
            .context("missing approval.required event")?;
        assert_eq!(approval_event.payload.get("input"), Some(&input));
        assert_eq!(
            approval_event
                .payload
                .get("intent_summary")
                .and_then(Value::as_str),
            Some("I will update the config file.")
        );

        assert!(!manager.deliver_external_approval(
            call_id,
            ExternalApprovalDecision::Allow { remember: false },
        ));
        assert_eq!(manager.pending_approvals_count(), 1);

        assert!(manager.deliver_external_approval(
            &approval_id,
            ExternalApprovalDecision::Allow { remember: false },
        ));
        assert_eq!(
            harness.recv_approval_event().await,
            Some(MockApprovalEvent::Approved {
                id: call_id.to_string(),
            })
        );
        assert_eq!(manager.pending_approvals_count(), 0);
        assert!(
            manager
                .get_thread_detail(&thread.id)
                .await?
                .pending_approvals
                .is_empty()
        );
        assert!(
            manager.events_since(&thread.id, None)?.iter().any(|event| {
                event.event == "approval.decided"
                    && event.payload.get("approval_id").and_then(Value::as_str)
                        == Some(approval_id.as_str())
                    && event.payload.get("tool_call_id").and_then(Value::as_str) == Some(call_id)
                    && event.payload.get("decision").and_then(Value::as_str) == Some("allow")
            }),
            "approval.decided must correlate the opaque id with the tool call it settled"
        );

        harness
            .tx_event
            .send(EngineEvent::TurnComplete {
                turn_id: None,
                submission_id: None,
                compaction_id: None,
                usage: Usage::default(),
                parent_route_usage: Usage::default(),
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await?;
    }
    Ok(())
}

#[tokio::test]
async fn user_input_snapshot_survives_reload_and_clears_after_submission() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "needs a choice".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));

    harness
        .tx_event
        .send(EngineEvent::UserInputRequired {
            id: "input_reload".to_string(),
            request: crate::tools::user_input::UserInputRequest {
                require_typed_answers: true,
                questions: vec![crate::tools::user_input::UserInputQuestion {
                    detail: Some("Review this plan".into()),
                    intent: Some(crate::tools::user_input::UserInputIntent::PlanReview {
                        approve: "Yes".into(),
                        call_id: Some("plan-call".into()),
                    }),
                    header: "Continue".to_string(),
                    id: "continue".to_string(),
                    question: "Continue with the check?".to_string(),
                    options: vec![
                        crate::tools::user_input::UserInputOption {
                            label: "Yes".to_string(),
                            description: "Continue now".to_string(),
                        },
                        crate::tools::user_input::UserInputOption {
                            label: "No".to_string(),
                            description: "Stop here".to_string(),
                        },
                    ],
                    allow_free_text: false,
                    multi_select: false,
                }],
            },
        })
        .await?;

    let deadline = Instant::now() + Duration::from_secs(2);
    let detail = loop {
        let detail = manager.get_thread_detail(&thread.id).await?;
        if !detail.pending_user_inputs.is_empty() {
            break detail;
        }
        if Instant::now() >= deadline {
            bail!("pending user input did not reach the canonical snapshot");
        }
        sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(detail.pending_approvals.len(), 0);
    assert_eq!(detail.pending_user_inputs.len(), 1);
    assert_eq!(detail.pending_user_inputs[0].id, "input_reload");
    assert_eq!(detail.pending_user_inputs[0].turn_id, turn.id);
    assert_eq!(
        detail.pending_user_inputs[0].request.questions[0].question,
        "Continue with the check?"
    );

    assert!(detail.pending_user_inputs[0].request.require_typed_answers);
    assert_eq!(
        detail.pending_user_inputs[0].request.questions[0]
            .detail
            .as_deref(),
        Some("Review this plan")
    );
    let events_before = manager.events_since(&thread.id, None)?.len();
    for answer in [
        json!({"id":"continue","label":"Yes","value":"Yes"}),
        json!({"kind":"choice","id":"continue","label":"Yes","value":"forged"}),
        json!({"kind":"custom","id":"continue","label":"","value":"forbidden custom"}),
        json!({"kind":"skipped","id":"continue","label":"Yes","value":""}),
        json!({"kind":"choice","id":"other-question","label":"Yes","value":"Yes"}),
    ] {
        let response = serde_json::from_value(json!({"answers":[answer]}))?;
        assert!(
            manager
                .submit_user_input(&thread.id, "input_reload", response)
                .await
                .is_err()
        );
        assert_eq!(manager.events_since(&thread.id, None)?.len(), events_before);
        assert!(
            !manager
                .pending_user_inputs
                .lock()
                .get(&(thread.id.clone(), "input_reload".into()))
                .unwrap()
                .settling
        );
    }
    assert!(
        tokio::time::timeout(
            Duration::from_millis(25),
            harness.recv_user_input_submission()
        )
        .await
        .is_err()
    );

    let (submitted, delivered) = tokio::join!(
        manager.submit_user_input(
            &thread.id,
            "input_reload",
            crate::tools::user_input::UserInputResponse {
                answers: vec![crate::tools::user_input::UserInputAnswer {
                    kind: Some(crate::tools::user_input::UserInputAnswerKind::Choice),
                    id: "continue".to_string(),
                    label: "Yes".to_string(),
                    value: "Yes".to_string(),
                }],
            },
        ),
        harness.recv_user_input_submission(),
    );
    submitted?;
    match delivered {
        Some((id, response)) => {
            assert_eq!(id, "input_reload");
            assert_eq!(response.answers[0].id, "continue");
        }
        other => panic!("expected submitted user input, got {other:?}"),
    }
    assert!(
        manager
            .get_thread_detail(&thread.id)
            .await?
            .pending_user_inputs
            .is_empty()
    );
    assert!(manager.events_since(&thread.id, None)?.iter().any(|event| {
        event.event == "user_input.answered"
            && event.payload.get("input_id").and_then(Value::as_str) == Some("input_reload")
    }));

    harness
        .tx_event
        .send(EngineEvent::UserInputClosed {
            id: "input_reload".into(),
            outcome: crate::core::events::UserInputClosedOutcome::Answered,
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(
        manager
            .events_since(&thread.id, None)?
            .iter()
            .filter(|event| event.event == "user_input.answered")
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn unknown_user_input_id_is_not_delivered_to_engine() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let delivered = manager
        .submit_user_input(
            &thread.id,
            "input_missing",
            crate::tools::user_input::UserInputResponse {
                answers: vec![crate::tools::user_input::UserInputAnswer {
                    kind: None,
                    id: "choice".to_string(),
                    label: "Missing".to_string(),
                    value: "must-not-enter-engine-mailbox".to_string(),
                }],
            },
        )
        .await?;
    assert!(!delivered);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(25),
            harness.recv_user_input_submission()
        )
        .await
        .is_err(),
        "unknown request entered the engine mailbox"
    );
    assert!(manager.events_since(&thread.id, None)?.iter().all(|event| {
        !matches!(
            event.event.as_str(),
            "user_input.answered" | "user_input.canceled"
        )
    }));
    manager.register_pending_user_input_for_thread_for_test(&thread.id, "idle-question");
    harness
        .tx_event
        .send(EngineEvent::UserInputClosed {
            id: "idle-question".into(),
            outcome: crate::core::events::UserInputClosedOutcome::Unavailable,
        })
        .await?;
    tokio::time::timeout(Duration::from_secs(2), async {
        while !manager.pending_requests_for_thread(&thread.id).1.is_empty() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("idle event pump did not close the question")?;
    assert!(
        !manager
            .submit_user_input(
                &thread.id,
                "idle-question",
                crate::tools::user_input::UserInputResponse {
                    answers: Vec::new()
                }
            )
            .await?
    );
    assert!(
        manager
            .events_since(&thread.id, None)?
            .iter()
            .any(|event| event.event == "user_input.canceled"
                && event.payload["outcome"] == "unavailable")
    );
    Ok(())
}

#[tokio::test]
async fn user_input_tool_timeout_and_cancel_clear_snapshot_before_turn_end() -> Result<()> {
    for (error, closed) in [
        (crate::tools::spec::ToolError::Timeout { seconds: 1 }, None),
        (
            crate::tools::spec::ToolError::cancelled("turn canceled"),
            None,
        ),
        (
            crate::tools::spec::ToolError::Timeout { seconds: 1 },
            Some(crate::core::events::UserInputClosedOutcome::TimedOut),
        ),
        (
            crate::tools::spec::ToolError::cancelled("turn canceled"),
            Some(crate::core::events::UserInputClosedOutcome::Cancelled),
        ),
        (
            crate::tools::spec::ToolError::cancelled("unavailable"),
            Some(crate::core::events::UserInputClosedOutcome::Unavailable),
        ),
    ] {
        let input_id = if closed.is_some() {
            "nested-question"
        } else {
            "input-timeout"
        };
        let manager = test_manager(test_runtime_dir())?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let mut harness = install_mock_engine(&manager, &thread.id).await;
        let turn = manager
            .start_turn(
                &thread.id,
                StartTurnRequest {
                    prompt: "ask before continuing work".into(),
                    ..StartTurnRequest::default()
                },
            )
            .await?;
        assert!(matches!(
            harness.rx_op.recv().await,
            Some(Op::SendMessage(_))
        ));
        harness
            .tx_event
            .send(EngineEvent::ToolCallStarted {
                id: "input-timeout".into(),
                model_call: None,
                name: "request_user_input".into(),
                input: json!({"questions": []}),
            })
            .await?;
        harness
            .tx_event
            .send(EngineEvent::UserInputRequired {
                id: input_id.into(),
                request: crate::tools::user_input::UserInputRequest {
                    require_typed_answers: false,
                    questions: Vec::new(),
                },
            })
            .await?;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if manager
                    .get_thread_detail(&thread.id)
                    .await?
                    .pending_user_inputs
                    .len()
                    == 1
                {
                    return Ok::<_, anyhow::Error>(());
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("question was not registered")??;

        if let Some(outcome) = closed {
            harness
                .tx_event
                .send(EngineEvent::UserInputClosed {
                    id: input_id.into(),
                    outcome,
                })
                .await?;
        } else {
            harness
                .tx_event
                .send(EngineEvent::ToolCallComplete {
                    id: "input-timeout".into(),
                    model_call: None,
                    name: "request_user_input".into(),
                    result: Err(error),
                })
                .await?;
        }
        tokio::time::timeout(TURN_SETTLEMENT_DEADLOCK_TIMEOUT, async {
            loop {
                let detail = manager.get_thread_detail(&thread.id).await?;
                let events = manager.events_since(&thread.id, None)?;
                if detail.pending_user_inputs.is_empty()
                    && events.iter().any(|event| {
                        event.event == "user_input.canceled"
                            && event.turn_id.as_deref() == Some(turn.id.as_str())
                            && event.payload.get("input_id").and_then(Value::as_str)
                                == Some(input_id)
                            && if let Some(outcome) = closed {
                                event.payload.get("outcome")
                                    == Some(&serde_json::to_value(outcome).unwrap())
                            } else {
                                event.payload.get("terminal").and_then(Value::as_bool)
                                    == Some(false)
                            }
                    })
                {
                    assert!(!events.iter().any(|event| event.event == "turn.completed"));
                    return Ok::<_, anyhow::Error>(());
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("completed tool left its question pending")??;
        assert_eq!(
            manager.store.load_turn(&turn.id)?.status,
            RuntimeTurnStatus::InProgress
        );
        assert!(
            !manager
                .submit_user_input(
                    &thread.id,
                    input_id,
                    crate::tools::user_input::UserInputResponse {
                        answers: Vec::new()
                    },
                )
                .await?,
            "an expired question accepted a late answer"
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(25),
                harness.recv_user_input_cancellation()
            )
            .await
            .is_err(),
            "a completed waiter must not receive another cancellation"
        );
        harness
            .tx_event
            .send(EngineEvent::TurnComplete {
                turn_id: None,
                submission_id: None,
                compaction_id: None,
                usage: Usage::default(),
                parent_route_usage: Usage::default(),
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await?;
        wait_for_terminal_turn(&manager, &turn.id).await?;
    }
    Ok(())
}

#[tokio::test]
async fn user_input_tool_settlement_preserves_other_request_ids_turns_and_retryable_receipts()
-> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    for (id, turn_id) in [
        ("inner-question", "turn-question"),
        ("new-question", "turn-new"),
    ] {
        manager.register_pending_user_input(
            &thread.id,
            PendingUserInputRequest {
                id: id.into(),
                turn_id: turn_id.into(),
                request: crate::tools::user_input::UserInputRequest {
                    require_typed_answers: false,
                    questions: Vec::new(),
                },
            },
        );
    }
    // Completing an enclosing execute_tools call is not the inner question's
    // terminal fact. Neither may a delayed prior-turn event own a reused id.
    manager
        .settle_user_input_for_completed_tool(&thread.id, "turn-question", "outer-call")
        .await?;
    manager
        .user_input_settlement(&thread.id)
        .close(
            "inner-question",
            Some("turn-old"),
            crate::core::events::UserInputClosedOutcome::Cancelled,
        )
        .await?;
    assert_eq!(manager.pending_requests_for_thread(&thread.id).1.len(), 2);
    assert!(
        manager
            .events_since(&thread.id, None)?
            .iter()
            .all(|event| event.event != "user_input.canceled")
    );

    let fault = EventAppendFaultGuard::arm(&thread.id, EventAppendTestFault::AfterSync);
    let error = manager
        .user_input_settlement(&thread.id)
        .close(
            "inner-question",
            Some("turn-question"),
            crate::core::events::UserInputClosedOutcome::Cancelled,
        )
        .await
        .expect_err("injected append failure must retain the request");
    drop(fault);
    assert!(format!("{error:#}").contains("rolled back"));
    assert_eq!(manager.pending_requests_for_thread(&thread.id).1.len(), 2);
    manager
        .user_input_settlement(&thread.id)
        .close(
            "inner-question",
            Some("turn-question"),
            crate::core::events::UserInputClosedOutcome::Cancelled,
        )
        .await?;
    let pending = manager.pending_requests_for_thread(&thread.id).1;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, "new-question");
    // Duplicate completion is idempotent and never manufactures a receipt.
    manager
        .user_input_settlement(&thread.id)
        .close(
            "inner-question",
            Some("turn-question"),
            crate::core::events::UserInputClosedOutcome::Cancelled,
        )
        .await?;
    assert_eq!(
        manager
            .events_since(&thread.id, None)?
            .iter()
            .filter(|event| event.event == "user_input.canceled")
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn user_input_tool_settlement_waits_for_answer_receipt_and_recovers_failed_claim()
-> Result<()> {
    for answered in [false, true] {
        let manager = test_manager(test_runtime_dir())?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        manager.register_pending_user_input(
            &thread.id,
            PendingUserInputRequest {
                id: "input-race".into(),
                turn_id: "turn-race".into(),
                request: crate::tools::user_input::UserInputRequest {
                    require_typed_answers: false,
                    questions: Vec::new(),
                },
            },
        );
        let PendingUserInputClaim::Claimed(request) = manager
            .user_input_settlement(&thread.id)
            .claim_pending_user_input(&thread.id, "input-race", None, None)?
        else {
            bail!("answer did not own the request");
        };
        let settlement =
            manager.settle_user_input_for_completed_tool(&thread.id, "turn-race", "input-race");
        tokio::pin!(settlement);
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut settlement)
                .await
                .is_err(),
            "tool completion must wait for the answer receipt's owner"
        );
        if answered {
            manager
                .user_input_settlement(&thread.id)
                .settle_claimed_user_input(
                    &thread.id,
                    None,
                    request,
                    UserInputTerminalOutcome::Answered(
                        crate::tools::user_input::UserInputResponse {
                            answers: Vec::new(),
                        },
                    ),
                )
                .await?;
        } else {
            // The API worker restores its claim after a retryable append
            // error. The already-completed waiter still needs retirement.
            manager
                .user_input_settlement(&thread.id)
                .restore_pending_user_input_claim(&thread.id, &request);
        }
        tokio::time::timeout(Duration::from_secs(2), &mut settlement)
            .await
            .context("tool settlement did not follow the answer receipt")??;
        assert!(manager.pending_requests_for_thread(&thread.id).1.is_empty());
        let events = manager.events_since(&thread.id, None)?;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event == "user_input.canceled")
                .count(),
            usize::from(!answered)
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event == "user_input.answered")
                .count(),
            usize::from(answered)
        );
    }
    Ok(())
}

#[tokio::test]
async fn user_input_rejected_answer_or_cancel_restores_exact_question_for_retry() -> Result<()> {
    for cancel in [false, true] {
        let manager = test_manager(test_runtime_dir())?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let mut harness = install_mock_engine(&manager, &thread.id).await;
        manager.register_pending_user_input(
            &thread.id,
            PendingUserInputRequest {
                id: "input-rejected".into(),
                turn_id: "turn-rejected".into(),
                request: crate::tools::user_input::UserInputRequest {
                    require_typed_answers: false,
                    questions: Vec::new(),
                },
            },
        );
        let delivery = async {
            if cancel {
                manager
                    .cancel_user_input(&thread.id, "input-rejected")
                    .await
            } else {
                manager
                    .submit_user_input(
                        &thread.id,
                        "input-rejected",
                        crate::tools::user_input::UserInputResponse {
                            answers: Vec::new(),
                        },
                    )
                    .await
            }
        };
        let (result, rejected) = tokio::join!(delivery, harness.reject_user_input_decision());
        assert!(
            result.is_err(),
            "Engine rejection cannot report delivery success"
        );
        assert_eq!(rejected.as_deref(), Some("input-rejected"));
        let pending = manager.pending_requests_for_thread(&thread.id).1;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "input-rejected");
        assert!(manager.events_since(&thread.id, None)?.iter().any(|event| {
            event.event == "user_input.required" && event.payload.get("delivery_error").is_some()
        }));
        let (accepted, consumed) = tokio::join!(
            manager.submit_user_input(
                &thread.id,
                "input-rejected",
                crate::tools::user_input::UserInputResponse {
                    answers: Vec::new()
                }
            ),
            harness.recv_user_input_submission(),
        );
        assert!(accepted?);
        assert!(consumed.is_some());
        assert!(manager.pending_requests_for_thread(&thread.id).1.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn user_input_terminal_tool_result_does_not_resurrect_rejected_inflight_answer() -> Result<()>
{
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    manager.register_pending_user_input(
        &thread.id,
        PendingUserInputRequest {
            id: "input-expired-race".into(),
            turn_id: "turn-expired-race".into(),
            request: crate::tools::user_input::UserInputRequest {
                require_typed_answers: false,
                questions: Vec::new(),
            },
        },
    );
    let terminal = async {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if manager
                    .pending_user_inputs
                    .lock()
                    .get(&(thread.id.clone(), "input-expired-race".into()))
                    .is_some_and(|entry| entry.settling)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await?;
        manager
            .settle_user_input_for_completed_tool(
                &thread.id,
                "turn-expired-race",
                "input-expired-race",
            )
            .await
    };
    let (submitted, completed, rejected) = tokio::join!(
        manager.submit_user_input(
            &thread.id,
            "input-expired-race",
            crate::tools::user_input::UserInputResponse {
                answers: Vec::new()
            }
        ),
        terminal,
        harness.reject_user_input_decision(),
    );
    assert!(submitted.is_err());
    completed?;
    assert_eq!(rejected.as_deref(), Some("input-expired-race"));
    assert!(manager.pending_requests_for_thread(&thread.id).1.is_empty());
    let events = manager.events_since(&thread.id, None)?;
    let last = events
        .iter()
        .rfind(|event| event.event.starts_with("user_input."))
        .unwrap();
    assert_eq!(last.event, "user_input.canceled");
    assert!(
        !manager
            .submit_user_input(
                &thread.id,
                "input-expired-race",
                crate::tools::user_input::UserInputResponse {
                    answers: Vec::new()
                }
            )
            .await?
    );
    Ok(())
}

#[tokio::test]
async fn user_input_receipt_append_failure_restores_request_without_delivery() -> Result<()> {
    const SECRET: &str = "answer-only-for-engine-after-retry";
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    manager.register_pending_user_input(
        &thread.id,
        PendingUserInputRequest {
            id: "input_retry".to_string(),
            turn_id: "turn_retry".to_string(),
            request: crate::tools::user_input::UserInputRequest {
                require_typed_answers: false,
                questions: vec![crate::tools::user_input::UserInputQuestion {
                    detail: None,
                    intent: None,
                    header: "Choice".into(),
                    id: "choice".into(),
                    question: "Continue?".into(),
                    options: Vec::new(),
                    allow_free_text: true,
                    multi_select: false,
                }],
            },
        },
    );
    let response = || crate::tools::user_input::UserInputResponse {
        answers: vec![crate::tools::user_input::UserInputAnswer {
            kind: None,
            id: "choice".to_string(),
            label: "Retry".to_string(),
            value: SECRET.to_string(),
        }],
    };

    let fault_guard = EventAppendFaultGuard::arm(&thread.id, EventAppendTestFault::AfterSync);
    let error = manager
        .submit_user_input(&thread.id, "input_retry", response())
        .await
        .expect_err("injected receipt append unexpectedly succeeded");
    drop(fault_guard);
    assert!(format!("{error:#}").contains("rolled back"));
    assert_eq!(
        manager
            .get_thread_detail(&thread.id)
            .await?
            .pending_user_inputs
            .len(),
        1,
        "retry-safe append failure removed the authoritative prompt"
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(25),
            harness.recv_user_input_submission()
        )
        .await
        .is_err(),
        "answer reached the engine before its receipt was durable"
    );

    let (submitted, delivered) = tokio::join!(
        manager.submit_user_input(&thread.id, "input_retry", response()),
        harness.recv_user_input_submission(),
    );
    assert!(submitted?, "restored request was not retryable");
    let (_, delivered) = delivered.context("retried answer was not consumed")?;
    assert_eq!(delivered.answers[0].value, SECRET);
    let events = manager.events_since(&thread.id, None)?;
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event == "user_input.answered")
            .count(),
        1
    );
    assert!(!serde_json::to_string(&events)?.contains(SECRET));
    Ok(())
}

#[tokio::test]
async fn user_input_settlement_outlives_canceled_api_future() -> Result<()> {
    const SECRET: &str = "answer-survives-request-disconnect";
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    manager.register_pending_user_input(
        &thread.id,
        PendingUserInputRequest {
            id: "input_detached".to_string(),
            turn_id: "turn_detached".to_string(),
            request: crate::tools::user_input::UserInputRequest {
                require_typed_answers: false,
                questions: vec![crate::tools::user_input::UserInputQuestion {
                    detail: None,
                    intent: None,
                    header: "Choice".into(),
                    id: "choice".into(),
                    question: "Continue?".into(),
                    options: Vec::new(),
                    allow_free_text: true,
                    multi_select: false,
                }],
            },
        },
    );

    let emit_guard = manager.event_emit.lock().await;
    let submit_manager = manager.clone();
    let thread_id = thread.id.clone();
    let submission = tokio::spawn(async move {
        submit_manager
            .submit_user_input(
                &thread_id,
                "input_detached",
                crate::tools::user_input::UserInputResponse {
                    answers: vec![crate::tools::user_input::UserInputAnswer {
                        kind: None,
                        id: "choice".to_string(),
                        label: "Continue".to_string(),
                        value: SECRET.to_string(),
                    }],
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if manager
                .pending_user_inputs
                .lock()
                .get(&(thread.id.clone(), "input_detached".to_string()))
                .is_some_and(|entry| entry.settling)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("submission did not claim the pending request")?;
    submission.abort();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(25),
            harness.recv_user_input_submission()
        )
        .await
        .is_err(),
        "answer reached the engine before its receipt append was released"
    );
    drop(emit_guard);

    let (_, delivered) =
        tokio::time::timeout(Duration::from_secs(2), harness.recv_user_input_submission())
            .await
            .context("detached settlement did not reach the engine")?
            .context("detached settlement was canceled")?;
    assert_eq!(delivered.answers[0].value, SECRET);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if manager.pending_requests_for_thread(&thread.id).1.is_empty() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("accepted detached reply did not retire its request")?;
    let detail = manager.get_thread_detail(&thread.id).await?;
    assert!(detail.pending_user_inputs.is_empty());
    let events = manager.events_since(&thread.id, None)?;
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event == "user_input.answered")
            .count(),
        1
    );
    assert!(!serde_json::to_string(&events)?.contains(SECRET));
    Ok(())
}

#[tokio::test]
async fn terminal_user_input_cancellation_is_durable_before_engine_delivery() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    manager.register_pending_user_input(
        &thread.id,
        PendingUserInputRequest {
            id: "input_terminal_order".to_string(),
            turn_id: "turn_terminal_order".to_string(),
            request: crate::tools::user_input::UserInputRequest {
                require_typed_answers: false,
                questions: Vec::new(),
            },
        },
    );

    let emit_guard = manager.event_emit.lock().await;
    let engine = harness.handle.clone();
    let settle_manager = manager.clone();
    let thread_id = thread.id.clone();
    let settlement = tokio::spawn(async move {
        settle_manager
            .settle_user_inputs_for_terminal_turn(&thread_id, "turn_terminal_order", Some(engine))
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if manager
                .pending_user_inputs
                .lock()
                .get(&(thread.id.clone(), "input_terminal_order".to_string()))
                .is_some_and(|entry| entry.settling)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("terminal cancellation did not claim the request")?;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(25),
            harness.recv_user_input_submission()
        )
        .await
        .is_err(),
        "terminal cancellation reached the engine before durable append"
    );
    drop(emit_guard);
    let (settled, delivered) = tokio::join!(settlement, harness.recv_user_input_cancellation());
    settled.context("terminal settlement task panicked")??;
    assert_eq!(delivered.as_deref(), Some("input_terminal_order"));
    let events = manager.events_since(&thread.id, None)?;
    let canceled = events
        .iter()
        .find(|event| {
            event.event == "user_input.canceled"
                && event.payload.get("input_id").and_then(Value::as_str)
                    == Some("input_terminal_order")
        })
        .context("missing terminal cancellation receipt")?;
    assert_eq!(canceled.payload["terminal"], true);
    assert!(
        manager
            .get_thread_detail(&thread.id)
            .await?
            .pending_user_inputs
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn thread_detail_cursor_precedes_projection_reads_at_terminal_boundary() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "complete while the snapshot is paused".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));

    let (hook_tx, mut hook_rx) = mpsc::unbounded_channel();
    manager.set_snapshot_test_hook(hook_tx);
    let snapshot_manager = manager.clone();
    let snapshot_thread_id = thread.id.clone();
    let snapshot_task = tokio::spawn(async move {
        snapshot_manager
            .get_thread_detail(&snapshot_thread_id)
            .await
    });

    let point = tokio::time::timeout(Duration::from_secs(2), hook_rx.recv())
        .await
        .context("snapshot did not capture its replay cursor")?
        .context("snapshot test hook closed")?;
    assert_eq!(point.thread_id, thread.id);

    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "snapshot_terminal".to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageStarted { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageComplete { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;

    assert!(
        tokio::time::timeout(Duration::from_millis(100), async {
            loop {
                if manager
                    .events_since(&thread.id, Some(point.latest_seq))?
                    .iter()
                    .any(|event| {
                        event.turn_id.as_deref() == Some(&turn.id)
                            && event.event == "turn.completed"
                    })
                {
                    break Ok::<_, anyhow::Error>(());
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_err(),
        "terminal publication crossed a snapshot projection boundary"
    );
    point
        .resume
        .send(())
        .map_err(|_| anyhow!("snapshot dropped its resume barrier"))?;

    let detail = snapshot_task.await.context("snapshot task panicked")??;
    assert_eq!(detail.latest_seq, point.latest_seq);
    assert_eq!(
        detail
            .turns
            .iter()
            .find(|record| record.id == turn.id)
            .map(|record| record.status),
        Some(RuntimeTurnStatus::InProgress),
        "the paused snapshot must retain the pre-terminal projection"
    );

    let completed_event = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(event) = manager
                .events_since(&thread.id, Some(detail.latest_seq))?
                .into_iter()
                .find(|event| {
                    event.turn_id.as_deref() == Some(&turn.id) && event.event == "turn.completed"
                })
            {
                break Ok::<_, anyhow::Error>(event);
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("terminal event did not follow the released snapshot boundary")??;
    assert!(completed_event.seq > detail.latest_seq);
    assert!(
        manager
            .events_since(&thread.id, Some(detail.latest_seq))?
            .iter()
            .any(|event| event.seq == completed_event.seq),
        "the same terminal transition must remain replayable from the snapshot cursor"
    );
    Ok(())
}

#[tokio::test]
async fn thread_detail_does_not_reenter_recovery_while_projection_is_locked() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let turn_id = "turn_recovery_queued_during_snapshot";
    let mut turn = sample_turn(&thread.id, turn_id, RuntimeTurnStatus::Completed);
    turn.ended_at = Some(Utc::now());
    turn.duration_ms = Some(1);
    manager.store.save_turn(&turn)?;
    {
        let _thread_mutation = manager.store.thread_mutation.lock();
        let mut persisted_thread = manager.store.load_thread(&thread.id)?;
        persisted_thread.latest_turn_id = Some(turn_id.to_string());
        manager.store.save_thread(&persisted_thread)?;
    }

    let (hook_tx, mut hook_rx) = mpsc::unbounded_channel();
    manager.set_snapshot_test_hook(hook_tx);
    let snapshot_manager = manager.clone();
    let snapshot_thread_id = thread.id.clone();
    let snapshot_task = tokio::spawn(async move {
        snapshot_manager
            .get_thread_detail(&snapshot_thread_id)
            .await
    });
    let point = tokio::time::timeout(Duration::from_secs(2), hook_rx.recv())
        .await
        .context("snapshot did not acquire its projection boundary")?
        .context("snapshot test hook closed")?;

    // Queue after get_thread_detail's initial recovery flush, while its
    // projection lock is held. A nested get_thread call would see this receipt
    // and deadlock trying to reacquire that same lock.
    manager.queue_recovery_receipt(RecoveredTurnReceipt {
        turn: turn.clone(),
        unresolved_dynamic_tools: Vec::new(),
    });
    point
        .resume
        .send(())
        .map_err(|_| anyhow!("snapshot dropped its resume barrier"))?;
    let detail = tokio::time::timeout(Duration::from_secs(2), snapshot_task)
        .await
        .context("snapshot re-entered recovery while holding its projection lock")?
        .context("snapshot task panicked")??;
    assert_eq!(detail.thread.id, thread.id);
    assert_eq!(detail.latest_seq, point.latest_seq);
    assert!(manager.recovery_receipts.lock().contains_key(&thread.id));

    // The next top-level observation flushes the receipt after the prior
    // snapshot has released its projection boundary.
    manager.get_thread(&thread.id).await?;
    let completed = manager
        .events_since(&thread.id, None)?
        .into_iter()
        .filter(|event| {
            event.event == "turn.completed" && event.turn_id.as_deref() == Some(turn_id)
        })
        .collect::<Vec<_>>();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].payload["recovered"], true);
    assert!(!manager.recovery_receipts.lock().contains_key(&thread.id));
    Ok(())
}

#[tokio::test]
async fn thread_detail_materializes_stream_prefixes_before_their_delta_cursor() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "snapshot both streamed prefixes".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));

    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "delta_snapshot".to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageStarted { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageDelta {
            index: 0,
            content: "durable message prefix".to_string(),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ThinkingStarted { index: 1 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ThinkingDelta {
            index: 1,
            content: "durable reasoning prefix".to_string(),
        })
        .await?;

    let deltas = tokio::time::timeout(TURN_SETTLEMENT_DEADLOCK_TIMEOUT, async {
        loop {
            let deltas = manager
                .events_since(&thread.id, None)?
                .into_iter()
                .filter(|event| {
                    event.turn_id.as_deref() == Some(&turn.id) && event.event == "item.delta"
                })
                .collect::<Vec<_>>();
            if deltas.len() == 2 {
                break Ok::<_, anyhow::Error>(deltas);
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("stream deltas were not durably sequenced")??;

    let detail = manager.get_thread_detail(&thread.id).await?;
    let latest_delta_seq = deltas.iter().map(|event| event.seq).max().unwrap_or(0);
    assert!(detail.latest_seq >= latest_delta_seq);
    let message = detail
        .items
        .iter()
        .find(|item| item.kind == TurnItemKind::AgentMessage)
        .context("snapshot omitted the streaming message item")?;
    let reasoning = detail
        .items
        .iter()
        .find(|item| item.kind == TurnItemKind::AgentReasoning)
        .context("snapshot omitted the streaming reasoning item")?;
    assert_eq!(message.status, TurnItemLifecycleStatus::InProgress);
    assert_eq!(message.detail.as_deref(), Some("durable message prefix"));
    assert_eq!(reasoning.status, TurnItemLifecycleStatus::InProgress);
    assert_eq!(
        reasoning.detail.as_deref(),
        Some("durable reasoning prefix")
    );
    assert_eq!(
        manager.store.load_item(&message.id)?.detail,
        message.detail,
        "message prefix must already be on disk before its delta cursor"
    );
    assert_eq!(
        manager.store.load_item(&reasoning.id)?.detail,
        reasoning.detail,
        "reasoning prefix must already be on disk before its delta cursor"
    );
    assert!(
        manager
            .events_since(&thread.id, Some(detail.latest_seq))?
            .iter()
            .all(|event| event.event != "item.delta"),
        "the snapshot itself must carry every delta at or before latest_seq"
    );

    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Interrupted,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    Ok(())
}

/// A reasoning segment whose stream died before its own stop frame must not
/// outlive the turn that streamed it.
///
/// A stream-resume retry — system sleep, a mid-stream network drop — discards
/// the fragment and re-issues the request without ever emitting
/// `ThinkingComplete`; the engine parks the block itself when the next one
/// starts (`flush_thinking_block`). The projection has to close the superseded
/// item at that same boundary. Overwriting the pointer instead left the record
/// `in_progress` inside a turn that went on to complete, which made every later
/// session save of that thread fail with a 409 naming an "active turn" that did
/// not exist.
#[tokio::test]
async fn superseded_reasoning_segment_closes_when_the_retried_stream_starts() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "retry the request after the stream dies".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));

    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "retried_stream".to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    // First attempt: reasoning streams, then the connection dies before the
    // block's stop frame arrives.
    harness
        .tx_event
        .send(EngineEvent::ThinkingStarted { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ThinkingDelta {
            index: 0,
            content: "first fragment".to_string(),
        })
        .await?;
    // The re-issued request streams a fresh block.
    harness
        .tx_event
        .send(EngineEvent::ThinkingStarted { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ThinkingDelta {
            index: 0,
            content: "second fragment".to_string(),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ThinkingComplete { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageStarted { index: 1 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageDelta {
            index: 1,
            content: "the answer".to_string(),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageComplete { index: 1 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;

    let terminal = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Completed);

    // Read the records from the store, not through `get_thread_detail`: the
    // projection repairs terminal-turn leftovers on read, and this test is
    // about the close happening at the boundary that causes it.
    let stored = manager.store.load_turn(&turn.id)?;
    let items = stored
        .item_ids
        .iter()
        .map(|item_id| manager.store.load_item(item_id))
        .collect::<Result<Vec<_>>>()?;
    let reasoning = items
        .iter()
        .filter(|item| item.kind == TurnItemKind::AgentReasoning)
        .collect::<Vec<_>>();
    assert_eq!(
        reasoning.len(),
        2,
        "each streamed segment keeps its own record: {items:#?}"
    );
    assert_eq!(reasoning[0].detail.as_deref(), Some("first fragment"));
    for item in &reasoning {
        assert_eq!(
            item.status,
            TurnItemLifecycleStatus::Completed,
            "a segment the retry superseded still ended: {item:#?}"
        );
        assert!(item.ended_at.is_some(), "{item:#?}");
    }
    assert!(
        items
            .iter()
            .all(|item| item.status == TurnItemLifecycleStatus::Completed),
        "no item may outlive a completed turn: {items:#?}"
    );

    // The close is published too, so a live client stops rendering the
    // superseded segment as still running.
    let closed = manager
        .events_since(&thread.id, None)?
        .into_iter()
        .filter(|event| {
            event.event == "item.completed"
                && event.item_id.as_deref() == Some(reasoning[0].id.as_str())
        })
        .count();
    assert_eq!(closed, 1, "the superseded segment is announced once");
    Ok(())
}

/// A tool call whose completion frame never arrived must not survive the turn
/// that started it.
///
/// Nothing in the monitor's own pointers holds a tool item: it lives only in
/// the turn's item list, so an interrupt or a stream that dies between the call
/// and its result used to leave the record `in_progress` forever. The terminal
/// sweep is what closes it, and it has to run before the parent turn is
/// written.
#[tokio::test]
async fn turn_end_closes_a_tool_call_that_never_completed() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "start a tool call that never answers".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));

    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "unanswered_tool_call".to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ToolCallStarted {
            id: "call-never-completed".to_string(),
            model_call: None,
            name: "bash".to_string(),
            input: json!({ "command": "true" }),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;

    let terminal = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Completed);

    let stored = manager.store.load_turn(&turn.id)?;
    let tool_item = stored
        .item_ids
        .iter()
        .map(|item_id| manager.store.load_item(item_id))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .find(|item| item.kind == TurnItemKind::ToolCall)
        .context("the tool call item")?;
    assert_eq!(
        tool_item.status,
        TurnItemLifecycleStatus::Completed,
        "a call left open by the turn's own terminal boundary: {tool_item:#?}"
    );
    assert!(tool_item.ended_at.is_some(), "{tool_item:#?}");
    Ok(())
}

/// A store an older build wrote — a terminal turn still listing an item as
/// live — is repaired where the projection already holds it.
///
/// The repair must close the leftovers to the outcome the turn itself recorded
/// and leave a live turn alone, because a live turn's monitor is still writing
/// those records.
#[tokio::test]
async fn thread_detail_closes_items_a_terminal_turn_still_lists_as_live() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let terminal_turn = {
        let mut turn = sample_turn(&thread.id, "turn_stale_item", RuntimeTurnStatus::Completed);
        turn.ended_at = Some(turn.created_at + chrono::Duration::seconds(1));
        let mut item = sample_item(
            &turn.id,
            "item_stale_reasoning",
            TurnItemLifecycleStatus::InProgress,
        );
        item.kind = TurnItemKind::AgentReasoning;
        item.detail = Some("half a thought".to_string());
        manager.store.save_item(&item)?;
        turn.item_ids = vec![item.id.clone()];
        manager.store.save_turn(&turn)?;
        turn
    };
    let live_turn = {
        let mut turn = sample_turn(&thread.id, "turn_live_item", RuntimeTurnStatus::InProgress);
        let mut item = sample_item(
            &turn.id,
            "item_live_reasoning",
            TurnItemLifecycleStatus::InProgress,
        );
        item.kind = TurnItemKind::AgentReasoning;
        manager.store.save_item(&item)?;
        turn.item_ids = vec![item.id.clone()];
        manager.store.save_turn(&turn)?;
        turn
    };
    let mut record = manager.store.load_thread(&thread.id)?;
    record.latest_turn_id = Some(live_turn.id.clone());
    manager.store.save_thread(&record)?;

    let detail = manager.get_thread_detail(&thread.id).await?;
    let repaired = detail
        .items
        .iter()
        .find(|item| item.id == "item_stale_reasoning")
        .context("the stale item")?;
    assert_eq!(repaired.status, TurnItemLifecycleStatus::Completed);
    assert_eq!(
        repaired.ended_at, terminal_turn.ended_at,
        "the turn's own end is the honest end for a segment it delivered"
    );
    assert_eq!(
        manager.store.load_item("item_stale_reasoning")?.status,
        TurnItemLifecycleStatus::Completed,
        "the repair is persisted, not only projected"
    );

    let still_live = detail
        .items
        .iter()
        .find(|item| item.id == "item_live_reasoning")
        .context("the live item")?;
    assert_eq!(
        still_live.status,
        TurnItemLifecycleStatus::InProgress,
        "a live turn's records belong to its monitor"
    );

    // Idempotent: a second read finds nothing left to do.
    let again = manager.get_thread_detail(&thread.id).await?;
    assert_eq!(
        again
            .items
            .iter()
            .find(|item| item.id == "item_stale_reasoning")
            .map(|item| item.status),
        Some(TurnItemLifecycleStatus::Completed)
    );
    Ok(())
}

#[tokio::test]
async fn thread_detail_delta_boundary_is_replay_idempotent() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "pause a snapshot across streamed deltas".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));

    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "delta_boundary".to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageStarted { index: 0 })
        .await?;

    let started = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(event) = manager
                .events_since(&thread.id, None)?
                .into_iter()
                .find(|event| {
                    event.turn_id.as_deref() == Some(&turn.id)
                        && event.event == "item.started"
                        && event.payload.pointer("/item/kind").and_then(Value::as_str)
                            == Some("agent_message")
                })
            {
                break Ok::<_, anyhow::Error>(event);
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("message item did not start")??;
    let item_id = started.item_id.context("started event omitted item id")?;

    let (hook_tx, mut hook_rx) = mpsc::unbounded_channel();
    manager.set_snapshot_test_hook(hook_tx);
    let snapshot_manager = manager.clone();
    let snapshot_thread_id = thread.id.clone();
    let snapshot_task = tokio::spawn(async move {
        snapshot_manager
            .get_thread_detail(&snapshot_thread_id)
            .await
    });
    let point = tokio::time::timeout(Duration::from_secs(2), hook_rx.recv())
        .await
        .context("snapshot did not capture its replay cursor")?
        .context("snapshot test hook closed")?;
    assert!(point.latest_seq >= started.seq);

    for content in ["A", "B"] {
        harness
            .tx_event
            .send(EngineEvent::MessageDelta {
                index: 0,
                content: content.to_string(),
            })
            .await?;
    }
    sleep(STREAM_DELTA_BATCH_MAX_LATENCY + Duration::from_millis(50)).await;
    assert!(
        manager
            .events_since(&thread.id, Some(point.latest_seq))?
            .iter()
            .all(|event| event.event != "item.delta"),
        "a delta must not publish while the snapshot holds its projection boundary"
    );

    point
        .resume
        .send(())
        .map_err(|_| anyhow!("snapshot dropped its resume barrier"))?;
    let detail = snapshot_task.await.context("snapshot task panicked")??;
    let snapshotted = detail
        .items
        .iter()
        .find(|item| item.id == item_id)
        .context("snapshot omitted the streaming item")?;
    assert_eq!(snapshotted.detail.as_deref(), Some(""));

    let delta = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(event) = manager
                .events_since(&thread.id, Some(detail.latest_seq))?
                .into_iter()
                .find(|event| {
                    event.item_id.as_deref() == Some(&item_id) && event.event == "item.delta"
                })
            {
                break Ok::<_, anyhow::Error>(event);
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("batched delta did not publish after snapshot release")??;
    assert_eq!(
        delta.payload.get("delta").and_then(Value::as_str),
        Some("AB")
    );
    assert_eq!(
        manager.store.load_item(&item_id)?.detail.as_deref(),
        Some("AB")
    );

    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Interrupted,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn terminal_turn_cancels_pending_user_input_and_clears_snapshot() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "needs input before completion".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    harness
        .tx_event
        .send(EngineEvent::UserInputRequired {
            id: "input_terminal".to_string(),
            request: crate::tools::user_input::UserInputRequest {
                require_typed_answers: false,
                questions: vec![crate::tools::user_input::UserInputQuestion {
                    detail: None,
                    intent: None,
                    header: "Continue".to_string(),
                    id: "continue".to_string(),
                    question: "Continue?".to_string(),
                    options: vec![crate::tools::user_input::UserInputOption {
                        label: "Yes".to_string(),
                        description: "Continue now".to_string(),
                    }],
                    allow_free_text: false,
                    multi_select: false,
                }],
            },
        })
        .await?;

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if !manager
            .get_thread_detail(&thread.id)
            .await?
            .pending_user_inputs
            .is_empty()
        {
            break;
        }
        if Instant::now() >= deadline {
            bail!("pending user input did not reach the canonical snapshot");
        }
        sleep(Duration::from_millis(20)).await;
    }

    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    let canceled = tokio::time::timeout(
        Duration::from_secs(2),
        harness.recv_user_input_cancellation(),
    )
    .await
    .expect("terminal user-input cancellation timed out");
    assert_eq!(canceled.as_deref(), Some("input_terminal"));

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let detail = manager.get_thread_detail(&thread.id).await?;
        if detail.pending_user_inputs.is_empty()
            && manager.events_since(&thread.id, None)?.iter().any(|event| {
                event.event == "user_input.canceled"
                    && event.turn_id.as_deref() == Some(turn.id.as_str())
                    && event.payload.get("input_id").and_then(Value::as_str)
                        == Some("input_terminal")
                    && event.payload.get("terminal").and_then(Value::as_bool) == Some(true)
            })
        {
            break;
        }
        if Instant::now() >= deadline {
            bail!(
                "terminal user input was not cleared from the snapshot with a cancellation event"
            );
        }
        sleep(Duration::from_millis(20)).await;
    }
    Ok(())
}

#[tokio::test]
async fn interrupted_turn_cancels_pending_user_input_and_clears_snapshot() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "needs input before interruption".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    harness
        .tx_event
        .send(EngineEvent::UserInputRequired {
            id: "input_interrupt".to_string(),
            request: crate::tools::user_input::UserInputRequest {
                require_typed_answers: false,
                questions: vec![crate::tools::user_input::UserInputQuestion {
                    detail: None,
                    intent: None,
                    header: "Continue".to_string(),
                    id: "continue".to_string(),
                    question: "Continue?".to_string(),
                    options: vec![crate::tools::user_input::UserInputOption {
                        label: "Yes".to_string(),
                        description: "Continue now".to_string(),
                    }],
                    allow_free_text: false,
                    multi_select: false,
                }],
            },
        })
        .await?;

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if !manager
            .get_thread_detail(&thread.id)
            .await?
            .pending_user_inputs
            .is_empty()
        {
            break;
        }
        if Instant::now() >= deadline {
            bail!("pending user input did not reach the canonical snapshot");
        }
        sleep(Duration::from_millis(20)).await;
    }

    // The user pressed Esc: the engine lands the interrupt as a terminal
    // turn outcome, not a failure. An unanswered prompt must not outlive it.
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Interrupted,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    let canceled = tokio::time::timeout(
        Duration::from_secs(2),
        harness.recv_user_input_cancellation(),
    )
    .await
    .expect("interrupted user-input cancellation timed out");
    assert_eq!(canceled.as_deref(), Some("input_interrupt"));

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let detail = manager.get_thread_detail(&thread.id).await?;
        if detail.pending_user_inputs.is_empty()
            && manager.events_since(&thread.id, None)?.iter().any(|event| {
                event.event == "user_input.canceled"
                    && event.turn_id.as_deref() == Some(turn.id.as_str())
                    && event.payload.get("input_id").and_then(Value::as_str)
                        == Some("input_interrupt")
                    && event.payload.get("terminal").and_then(Value::as_bool) == Some(true)
            })
        {
            break;
        }
        if Instant::now() >= deadline {
            bail!(
                "interrupted user input was not cleared from the snapshot with a cancellation event"
            );
        }
        sleep(Duration::from_millis(20)).await;
    }
    Ok(())
}

#[tokio::test]
async fn dynamic_tool_result_settles_snapshot_and_emits_one_safe_resolution() -> Result<()> {
    use crate::tools::spec::DynamicToolExecutor;

    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "run an external lookup".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "dynamic_result".to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;

    const RESULT_SECRET: &str = "dynamic-result-secret";
    let executor = manager.clone();
    let executor_thread_id = thread.id.clone();
    let execution = tokio::spawn(async move {
        DynamicToolExecutor::execute_dynamic_tool(
            &executor,
            Some(executor_thread_id),
            Some("bench".to_string()),
            "lookup".to_string(),
            json!({ "record_id": "record-7" }),
        )
        .await
    });

    let pending = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let detail = manager.get_thread_detail(&thread.id).await?;
            if let Some(call) = detail.pending_dynamic_tool_calls.first() {
                break Ok::<_, anyhow::Error>((detail.latest_seq, call.clone()));
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("dynamic tool call did not reach the canonical snapshot")??;
    let (snapshot_seq, call) = pending;
    assert_eq!(call.thread_id, thread.id);
    assert_eq!(call.turn_id, turn.id);
    assert_eq!(call.namespace.as_deref(), Some("bench"));
    assert_eq!(call.tool, "lookup");
    assert_eq!(call.arguments["record_id"], "record-7");
    let requested = manager
        .events_since(&thread.id, None)?
        .into_iter()
        .find(|event| {
            event.event == "tool_call.requested"
                && event.payload.get("call_id").and_then(Value::as_str)
                    == Some(call.call_id.as_str())
        })
        .context("dynamic tool request was not durable")?;
    assert!(requested.seq <= snapshot_seq);
    assert!(
        manager
            .events_since(&thread.id, Some(snapshot_seq))?
            .iter()
            .all(|event| event.event != "tool_call.requested"),
        "the pending call must be recoverable from the snapshot once replay starts at latest_seq"
    );

    assert!(
        manager
            .deliver_dynamic_tool_result(
                &thread.id,
                &turn.id,
                &call.call_id,
                DynamicToolCallResult {
                    success: true,
                    content: vec![DynamicToolCallContent::InputText {
                        text: RESULT_SECRET.to_string(),
                    }],
                },
            )
            .await?
    );
    let result = execution.await.context("dynamic tool task panicked")??;
    assert_eq!(
        result.content, RESULT_SECRET,
        "the model-facing result changed"
    );
    assert!(
        manager
            .get_thread_detail(&thread.id)
            .await?
            .pending_dynamic_tool_calls
            .is_empty()
    );
    let resolved = manager
        .events_since(&thread.id, None)?
        .into_iter()
        .filter(|event| {
            event.event == "tool_call.resolved"
                && event.payload.get("call_id").and_then(Value::as_str)
                    == Some(call.call_id.as_str())
        })
        .collect::<Vec<_>>();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["status"], "resolved");
    assert_eq!(resolved[0].payload["success"], true);
    assert!(
        !serde_json::to_string(&resolved)?.contains(RESULT_SECRET),
        "terminal dynamic-tool lifecycle must not echo result content"
    );
    assert!(
        !manager
            .deliver_dynamic_tool_result(
                &thread.id,
                &turn.id,
                &call.call_id,
                DynamicToolCallResult {
                    success: true,
                    content: Vec::new(),
                },
            )
            .await?,
        "a duplicate result must not settle or emit twice"
    );
    assert_eq!(
        manager
            .events_since(&thread.id, None)?
            .iter()
            .filter(|event| event.event == "tool_call.resolved")
            .count(),
        1
    );

    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn dynamic_tool_result_receipt_outlives_canceled_delivery_future() -> Result<()> {
    use crate::tools::spec::DynamicToolExecutor;

    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "settle a result after its HTTP future disappears".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "dynamic_detached_settlement".to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;

    const RESULT_SECRET: &str = "detached-dynamic-result-secret";
    let executor = manager.clone();
    let executor_thread_id = thread.id.clone();
    let execution = tokio::spawn(async move {
        DynamicToolExecutor::execute_dynamic_tool(
            &executor,
            Some(executor_thread_id),
            Some("bench".to_string()),
            "detached_lookup".to_string(),
            json!({ "record_id": "record-detached" }),
        )
        .await
    });

    let call = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(call) = manager
                .get_thread_detail(&thread.id)
                .await?
                .pending_dynamic_tool_calls
                .first()
                .cloned()
            {
                break Ok::<_, anyhow::Error>(call);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("dynamic tool call did not become pending")??;

    // Stall terminal publication, submit the result, and wait until that path
    // owns the call. Canceling the API future from this point must not cancel
    // its detached receipt task or wake the model before the receipt is durable.
    let emit_guard = manager.event_emit.lock().await;
    let delivery_manager = manager.clone();
    let delivery_thread_id = thread.id.clone();
    let delivery_turn_id = turn.id.clone();
    let delivery_call_id = call.call_id.clone();
    let delivery = tokio::spawn(async move {
        delivery_manager
            .deliver_dynamic_tool_result(
                &delivery_thread_id,
                &delivery_turn_id,
                &delivery_call_id,
                DynamicToolCallResult {
                    success: true,
                    content: vec![DynamicToolCallContent::InputText {
                        text: RESULT_SECRET.to_string(),
                    }],
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let settling = manager
                .pending_dynamic_tools
                .lock()
                .get(&call.call_id)
                .is_some_and(|entry| entry.sender.is_none());
            if settling {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("result delivery never claimed the pending call")?;
    assert!(
        !execution.is_finished(),
        "the model consumed the result before its terminal receipt could commit"
    );
    assert!(
        !manager
            .deliver_dynamic_tool_result(
                &thread.id,
                &turn.id,
                &call.call_id,
                DynamicToolCallResult {
                    success: false,
                    content: Vec::new(),
                },
            )
            .await?,
        "a duplicate result stole a call whose terminal receipt was settling"
    );
    delivery.abort();
    assert!(
        delivery
            .await
            .expect_err("delivery API future must be canceled")
            .is_cancelled()
    );
    assert!(
        !execution.is_finished(),
        "canceling the delivery future woke the model without a receipt"
    );

    drop(emit_guard);
    let model_result = tokio::time::timeout(Duration::from_secs(2), execution)
        .await
        .context("model did not receive the result after terminal publication")?
        .context("dynamic tool task panicked")??;
    assert_eq!(model_result.content, RESULT_SECRET);

    let detail = manager.get_thread_detail(&thread.id).await?;
    assert!(detail.pending_dynamic_tool_calls.is_empty());
    let terminal = manager
        .events_since(&thread.id, None)?
        .into_iter()
        .filter(|event| {
            matches!(
                event.event.as_str(),
                "tool_call.resolved" | "tool_call.timeout" | "tool_call.canceled"
            ) && event.payload.get("call_id").and_then(Value::as_str) == Some(call.call_id.as_str())
        })
        .collect::<Vec<_>>();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].event, "tool_call.resolved");
    assert_eq!(terminal[0].payload["success"], true);
    assert!(
        !serde_json::to_string(&terminal)?.contains(RESULT_SECRET),
        "the terminal receipt exposed result content"
    );
    assert!(
        !manager
            .deliver_dynamic_tool_result(
                &thread.id,
                &turn.id,
                &call.call_id,
                DynamicToolCallResult {
                    success: true,
                    content: Vec::new(),
                },
            )
            .await?,
        "a duplicate retry settled an already terminal call"
    );

    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn dynamic_tool_result_acceptance_survives_receiver_close_before_append() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let receiver = manager.register_pending_dynamic_tool_for_test(
        &thread.id,
        "turn_closed_receiver",
        "call_closed_receiver",
    )?;
    let emit_guard = manager.event_emit.lock().await;
    let delivery_manager = manager.clone();
    let delivery_thread_id = thread.id.clone();
    let delivery = tokio::spawn(async move {
        delivery_manager
            .deliver_dynamic_tool_result(
                &delivery_thread_id,
                "turn_closed_receiver",
                "call_closed_receiver",
                DynamicToolCallResult {
                    success: true,
                    content: vec![DynamicToolCallContent::InputText {
                        text: "closed-receiver-secret".to_string(),
                    }],
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if manager
                .pending_dynamic_tools
                .lock()
                .get("call_closed_receiver")
                .is_some_and(|entry| entry.sender.is_none())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("result did not claim the call before append")?;
    drop(receiver);
    drop(emit_guard);

    assert!(
        delivery.await.context("delivery task panicked")??,
        "durably accepted result was reported as missing after receiver close"
    );
    assert!(
        manager
            .get_thread_detail(&thread.id)
            .await?
            .pending_dynamic_tool_calls
            .is_empty()
    );
    let terminal = manager
        .events_since(&thread.id, None)?
        .into_iter()
        .filter(|event| {
            event.payload.get("call_id").and_then(Value::as_str) == Some("call_closed_receiver")
                && matches!(
                    event.event.as_str(),
                    "tool_call.resolved" | "tool_call.timeout" | "tool_call.canceled"
                )
        })
        .collect::<Vec<_>>();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].event, "tool_call.resolved");
    assert_eq!(terminal[0].payload["result_accepted"], true);
    assert!(
        !serde_json::to_string(&terminal)?.contains("closed-receiver-secret"),
        "closed-receiver acceptance exposed result content"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn dynamic_tool_receipt_append_failure_rolls_back_for_retry() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut receiver = manager.register_pending_dynamic_tool_for_test(
        &thread.id,
        "turn_retry_after_append_failure",
        "call_retry_after_append_failure",
    )?;
    let claim = match manager.claim_pending_dynamic_tool(
        &thread.id,
        "turn_retry_after_append_failure",
        "call_retry_after_append_failure",
    ) {
        PendingDynamicToolClaim::Claimed(claim) => claim,
        PendingDynamicToolClaim::Settling(_)
        | PendingDynamicToolClaim::Indeterminate
        | PendingDynamicToolClaim::Missing => {
            bail!("failed to claim the append-failure fixture")
        }
    };

    // Replace this throwaway thread's event log with a symlink. Runtime store
    // hardening rejects the append deterministically, exercising settlement
    // rollback without relying on platform permission behavior.
    let events_path = manager.store.events_path(&thread.id)?;
    let backup_path = events_path.with_extension("jsonl.append-failure-backup");
    std::fs::rename(&events_path, &backup_path)?;
    std::os::unix::fs::symlink(&backup_path, &events_path)?;
    let ack = manager.spawn_dynamic_tool_settlement(
        claim,
        DynamicToolTerminalOutcome::Resolved(DynamicToolCallResult {
            success: true,
            content: vec![DynamicToolCallContent::InputText {
                text: "discarded-before-retry".to_string(),
            }],
        }),
    );
    let failed = RuntimeThreadManager::await_dynamic_tool_settlement(ack).await;
    std::fs::remove_file(&events_path)?;
    std::fs::rename(&backup_path, &events_path)?;
    assert!(
        failed.is_err(),
        "symlinked event append unexpectedly succeeded"
    );

    {
        let pending = manager.pending_dynamic_tools.lock();
        let entry = pending
            .get("call_retry_after_append_failure")
            .context("failed receipt append stranded or removed the pending call")?;
        assert!(
            entry
                .sender
                .as_ref()
                .is_some_and(|sender| !sender.is_closed()),
            "failed receipt append left a Settling entry without its sender"
        );
    }
    assert_eq!(
        manager
            .get_thread_detail(&thread.id)
            .await?
            .pending_dynamic_tool_calls
            .len(),
        1,
        "rollback removed the snapshot-authoritative pending request"
    );

    assert!(
        manager
            .deliver_dynamic_tool_result(
                &thread.id,
                "turn_retry_after_append_failure",
                "call_retry_after_append_failure",
                DynamicToolCallResult {
                    success: true,
                    content: vec![DynamicToolCallContent::InputText {
                        text: "retry-result".to_string(),
                    }],
                },
            )
            .await?,
        "retry did not settle the restored call"
    );
    let delivered = tokio::time::timeout(Duration::from_secs(2), &mut receiver)
        .await
        .context("restored receiver was not woken by retry")??;
    assert_eq!(
        delivered.content,
        vec![DynamicToolCallContent::InputText {
            text: "retry-result".to_string(),
        }]
    );
    let resolved = manager
        .events_since(&thread.id, None)?
        .into_iter()
        .filter(|event| {
            event.event == "tool_call.resolved"
                && event.payload.get("call_id").and_then(Value::as_str)
                    == Some("call_retry_after_append_failure")
        })
        .collect::<Vec<_>>();
    assert_eq!(resolved.len(), 1);
    assert!(
        !serde_json::to_string(&resolved)?.contains("retry-result"),
        "retried terminal receipt exposed result content"
    );
    Ok(())
}

#[tokio::test]
async fn dynamic_tool_post_write_failures_rollback_without_duplicate_receipts() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;

    for (index, fault) in [
        EventAppendTestFault::AfterFlush,
        EventAppendTestFault::AfterSync,
    ]
    .into_iter()
    .enumerate()
    {
        let turn_id = format!("turn_post_write_{index}");
        let call_id = format!("call_post_write_{index}");
        let result_text = format!("post-write-result-{index}");
        let mut receiver =
            manager.register_pending_dynamic_tool_for_test(&thread.id, &turn_id, &call_id)?;
        let fault_guard = EventAppendFaultGuard::arm(&thread.id, fault);
        let error = manager
            .deliver_dynamic_tool_result(
                &thread.id,
                &turn_id,
                &call_id,
                DynamicToolCallResult {
                    success: true,
                    content: vec![DynamicToolCallContent::InputText {
                        text: result_text.clone(),
                    }],
                },
            )
            .await
            .expect_err("injected post-write failure unexpectedly settled");
        drop(fault_guard);
        assert!(
            error.to_string().contains("rolled back"),
            "post-write failure was not classified retry-safe: {error}"
        );

        let failed_snapshot = manager.get_thread_detail(&thread.id).await?;
        assert!(
            failed_snapshot
                .pending_dynamic_tool_calls
                .iter()
                .any(|call| call.call_id == call_id),
            "rolled-back call disappeared from the canonical snapshot"
        );
        assert!(
            manager
                .events_since(&thread.id, Some(failed_snapshot.latest_seq))?
                .is_empty(),
            "failed append left a replay-visible terminal suffix"
        );
        assert!(
            manager.events_since(&thread.id, None)?.iter().all(|event| {
                event.payload.get("call_id").and_then(Value::as_str) != Some(call_id.as_str())
            }),
            "failed append left a visible terminal record before retry"
        );

        assert!(
            manager
                .deliver_dynamic_tool_result(
                    &thread.id,
                    &turn_id,
                    &call_id,
                    DynamicToolCallResult {
                        success: true,
                        content: vec![DynamicToolCallContent::InputText {
                            text: result_text.clone(),
                        }],
                    },
                )
                .await?,
            "retry did not durably accept the rolled-back result"
        );
        let delivered = tokio::time::timeout(Duration::from_secs(2), &mut receiver)
            .await
            .context("retried result did not reach its model receiver")??;
        assert_eq!(
            delivered.content,
            vec![DynamicToolCallContent::InputText {
                text: result_text.clone(),
            }]
        );

        let replay = manager.events_since(&thread.id, Some(failed_snapshot.latest_seq))?;
        let terminal = replay
            .iter()
            .filter(|event| {
                event.event == "tool_call.resolved"
                    && event.payload.get("call_id").and_then(Value::as_str)
                        == Some(call_id.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(terminal.len(), 1);
        assert!(terminal[0].seq > failed_snapshot.latest_seq);
        assert_eq!(terminal[0].payload["result_accepted"], true);
        assert!(
            !serde_json::to_string(&terminal)?.contains(&result_text),
            "retried terminal receipt exposed result content"
        );
        let settled_snapshot = manager.get_thread_detail(&thread.id).await?;
        assert!(
            settled_snapshot
                .pending_dynamic_tool_calls
                .iter()
                .all(|call| call.call_id != call_id)
        );
        assert!(settled_snapshot.latest_seq >= terminal[0].seq);
    }
    Ok(())
}

#[tokio::test]
async fn restart_recovers_terminal_turn_after_dynamic_receipt_append_failure() -> Result<()> {
    let data_dir = test_runtime_dir();
    let manager = test_manager(data_dir.clone())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let turn_id = "turn_terminal_recovery";
    let call_id = "call_terminal_recovery";
    let mut turn = sample_turn(&thread.id, turn_id, RuntimeTurnStatus::Completed);
    turn.ended_at = Some(Utc::now());
    turn.duration_ms = Some(1);
    manager.store.save_turn(&turn)?;
    {
        let _thread_mutation = manager.store.thread_mutation.lock();
        let mut persisted_thread = manager.store.load_thread(&thread.id)?;
        persisted_thread.latest_turn_id = Some(turn_id.to_string());
        manager.store.save_thread(&persisted_thread)?;
    }
    let params = DynamicToolCallParams {
        thread_id: thread.id.clone(),
        turn_id: turn_id.to_string(),
        call_id: call_id.to_string(),
        namespace: Some("recovery".to_string()),
        tool: "recover_lookup".to_string(),
        arguments: json!({ "record": "recovery-only" }),
    };
    manager
        .emit_event_for_test(
            &thread.id,
            Some(turn_id),
            "tool_call.requested",
            json!(&params),
        )
        .await?;
    let receiver = manager.register_pending_dynamic_tool(params)?;

    let fault_guard = EventAppendFaultGuard::arm(&thread.id, EventAppendTestFault::AfterSync);
    let error = manager
        .deliver_dynamic_tool_result(
            &thread.id,
            turn_id,
            call_id,
            DynamicToolCallResult {
                success: true,
                content: vec![DynamicToolCallContent::InputText {
                    text: "never-committed-result".to_string(),
                }],
            },
        )
        .await
        .expect_err("injected terminal receipt append unexpectedly succeeded");
    drop(fault_guard);
    assert!(error.to_string().contains("rolled back"));
    assert!(manager.events_since(&thread.id, None)?.iter().all(|event| {
        event.event != "turn.completed"
            && !matches!(
                event.event.as_str(),
                "tool_call.resolved" | "tool_call.canceled" | "tool_call.timeout"
            )
    }));
    drop(receiver);
    drop(manager);

    let recovered = test_manager(data_dir.clone())?;
    // Opening is synchronous; the first async observation flushes queued
    // recovery receipts in terminal-call-before-turn order.
    let recovered_turn = recovered.get_thread(&thread.id).await?;
    assert_eq!(recovered_turn.latest_turn_id.as_deref(), Some(turn_id));
    let events = recovered.events_since(&thread.id, None)?;
    let canceled = events
        .iter()
        .filter(|event| {
            event.event == "tool_call.canceled"
                && event.payload.get("call_id").and_then(Value::as_str) == Some(call_id)
        })
        .collect::<Vec<_>>();
    let completed = events
        .iter()
        .filter(|event| {
            event.event == "turn.completed" && event.turn_id.as_deref() == Some(turn_id)
        })
        .collect::<Vec<_>>();
    assert_eq!(canceled.len(), 1);
    assert_eq!(canceled[0].payload["reason"], "process_restart");
    assert_eq!(canceled[0].payload["recovered"], true);
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].payload["recovered"], true);
    assert!(canceled[0].seq < completed[0].seq);
    assert_eq!(
        completed[0]
            .payload
            .pointer("/turn/status")
            .and_then(Value::as_str),
        Some("completed")
    );

    // Re-observation and a second manager restart both remain idempotent.
    recovered.get_thread(&thread.id).await?;
    assert_eq!(
        recovered
            .events_since(&thread.id, None)?
            .iter()
            .filter(|event| {
                event.event == "turn.completed" && event.turn_id.as_deref() == Some(turn_id)
            })
            .count(),
        1
    );
    drop(recovered);
    let reopened = test_manager(data_dir)?;
    reopened.get_thread(&thread.id).await?;
    let reopened_events = reopened.events_since(&thread.id, None)?;
    assert_eq!(
        reopened_events
            .iter()
            .filter(|event| {
                event.event == "tool_call.canceled"
                    && event.payload.get("call_id").and_then(Value::as_str) == Some(call_id)
            })
            .count(),
        1
    );
    assert_eq!(
        reopened_events
            .iter()
            .filter(|event| {
                event.event == "turn.completed" && event.turn_id.as_deref() == Some(turn_id)
            })
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn restart_reconciles_unresolved_dynamic_call_after_existing_turn_completion() -> Result<()> {
    let data_dir = test_runtime_dir();
    let manager = test_manager(data_dir.clone())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let turn_id = "turn_legacy_completed_request";
    let call_id = "call_legacy_completed_request";
    let mut turn = sample_turn(&thread.id, turn_id, RuntimeTurnStatus::Completed);
    turn.ended_at = Some(Utc::now());
    turn.duration_ms = Some(1);
    manager.store.save_turn(&turn)?;
    let params = DynamicToolCallParams {
        thread_id: thread.id.clone(),
        turn_id: turn_id.to_string(),
        call_id: call_id.to_string(),
        namespace: Some("legacy".to_string()),
        tool: "legacy_lookup".to_string(),
        arguments: json!({ "record": "persisted-before-terminal-receipts" }),
    };
    manager
        .emit_event_for_test(
            &thread.id,
            Some(turn_id),
            "tool_call.requested",
            json!(&params),
        )
        .await?;
    manager
        .emit_event_for_test(
            &thread.id,
            Some(turn_id),
            "turn.completed",
            json!({ "turn": &turn }),
        )
        .await?;
    drop(manager);

    let recovered = test_manager(data_dir)?;
    recovered.get_thread(&thread.id).await?;
    let events = recovered.events_since(&thread.id, None)?;
    let terminal_calls = events
        .iter()
        .filter(|event| {
            event.turn_id.as_deref() == Some(turn_id)
                && event.payload.get("call_id").and_then(Value::as_str) == Some(call_id)
                && matches!(
                    event.event.as_str(),
                    "tool_call.resolved" | "tool_call.canceled" | "tool_call.timeout"
                )
        })
        .collect::<Vec<_>>();
    assert_eq!(terminal_calls.len(), 1);
    assert_eq!(terminal_calls[0].event, "tool_call.canceled");
    assert_eq!(terminal_calls[0].payload["reason"], "process_restart");
    assert_eq!(terminal_calls[0].payload["recovered"], true);
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                event.event == "turn.completed" && event.turn_id.as_deref() == Some(turn_id)
            })
            .count(),
        1,
        "recovery duplicated an already durable turn completion"
    );
    Ok(())
}

#[tokio::test]
async fn concurrent_recovery_dedupe_scans_emit_each_terminal_receipt_once() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let turn_id = "turn_concurrent_recovery_dedupe";
    let call_id = "call_concurrent_recovery_dedupe";
    let mut turn = sample_turn(&thread.id, turn_id, RuntimeTurnStatus::Completed);
    turn.ended_at = Some(Utc::now());
    let params = DynamicToolCallParams {
        thread_id: thread.id.clone(),
        turn_id: turn_id.to_string(),
        call_id: call_id.to_string(),
        namespace: Some("recovery".to_string()),
        tool: "dedupe_lookup".to_string(),
        arguments: json!({ "record": "same-terminal-receipt" }),
    };

    let (first_call, second_call) = tokio::join!(
        manager.emit_recovered_dynamic_cancellation_if_missing(&params),
        manager.emit_recovered_dynamic_cancellation_if_missing(&params),
    );
    assert_eq!(
        usize::from(first_call?) + usize::from(second_call?),
        1,
        "the event_emit boundary must linearize dynamic-terminal dedupe"
    );

    let (first_turn, second_turn) = tokio::join!(
        manager.emit_turn_completed_if_missing(&turn, true),
        manager.emit_turn_completed_if_missing(&turn, true),
    );
    assert_eq!(
        usize::from(first_turn?) + usize::from(second_turn?),
        1,
        "the event_emit boundary must linearize turn-completion dedupe"
    );

    let events = manager.events_since(&thread.id, None)?;
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                event.event == "tool_call.canceled"
                    && event.turn_id.as_deref() == Some(turn_id)
                    && event.payload.get("call_id").and_then(Value::as_str) == Some(call_id)
            })
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                event.event == "turn.completed" && event.turn_id.as_deref() == Some(turn_id)
            })
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn consecutive_dynamic_receipt_failures_queue_in_process_recovery() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let turn_id = "turn_consecutive_receipt_failures";
    let call_id = "call_consecutive_receipt_failures";
    let turn = sample_turn(&thread.id, turn_id, RuntimeTurnStatus::InProgress);
    manager.store.save_turn(&turn)?;
    {
        let _thread_mutation = manager.store.thread_mutation.lock();
        let mut persisted_thread = manager.store.load_thread(&thread.id)?;
        persisted_thread.latest_turn_id = Some(turn_id.to_string());
        manager.store.save_thread(&persisted_thread)?;
    }
    let params = DynamicToolCallParams {
        thread_id: thread.id.clone(),
        turn_id: turn_id.to_string(),
        call_id: call_id.to_string(),
        namespace: Some("recovery".to_string()),
        tool: "retry_lookup".to_string(),
        arguments: json!({ "record": "in-process" }),
    };
    let requested = manager
        .emit_event_for_test(
            &thread.id,
            Some(turn_id),
            "tool_call.requested",
            json!(&params),
        )
        .await?;
    let mut receiver = manager.register_pending_dynamic_tool(params)?;

    // The submitted result and the monitor's terminal cancellation both fail
    // after fsync, with each JSONL line transactionally removed. The monitor
    // must retain an in-process recovery path instead of evicting the engine
    // with an Awaiting call and no future owner.
    let fault_guard =
        EventAppendFaultGuard::arm_repeated(&thread.id, EventAppendTestFault::AfterSync, 2);
    let result_error = manager
        .deliver_dynamic_tool_result(
            &thread.id,
            turn_id,
            call_id,
            DynamicToolCallResult {
                success: true,
                content: vec![DynamicToolCallContent::InputText {
                    text: "rolled-back-result".to_string(),
                }],
            },
        )
        .await
        .expect_err("first injected receipt failure unexpectedly succeeded");
    assert!(result_error.to_string().contains("rolled back"));
    manager
        .settle_claimed_turn_failure(&thread.id, turn_id, "forced monitor failure")
        .await;
    drop(fault_guard);

    assert!(
        manager
            .recovery_receipts
            .lock()
            .get(&thread.id)
            .is_some_and(|receipts| receipts.iter().any(|receipt| receipt.turn.id == turn_id)),
        "second retry-safe failure did not queue in-process recovery"
    );
    assert_eq!(manager.pending_dynamic_tools_count(), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut receiver)
            .await
            .is_err(),
        "failed cancellation unexpectedly closed the model receiver"
    );
    assert!(manager.events_since(&thread.id, None)?.iter().all(|event| {
        event.event != "turn.completed"
            && !matches!(
                event.event.as_str(),
                "tool_call.resolved" | "tool_call.canceled" | "tool_call.timeout"
            )
    }));

    // The next async observation owns the queued retry. It durably cancels
    // the call before publishing exactly one recovered turn completion.
    manager.get_thread(&thread.id).await?;
    let closed = tokio::time::timeout(Duration::from_secs(2), &mut receiver)
        .await
        .context("recovery did not wake the model receiver")?;
    assert!(closed.is_err(), "terminal recovery delivered a tool result");
    let events = manager.events_since(&thread.id, None)?;
    let canceled = events
        .iter()
        .filter(|event| {
            event.event == "tool_call.canceled"
                && event.payload.get("call_id").and_then(Value::as_str) == Some(call_id)
        })
        .collect::<Vec<_>>();
    let completed = events
        .iter()
        .filter(|event| {
            event.event == "turn.completed" && event.turn_id.as_deref() == Some(turn_id)
        })
        .collect::<Vec<_>>();
    assert_eq!(canceled.len(), 1);
    assert_eq!(canceled[0].payload["reason"], "turn_terminal");
    assert_eq!(canceled[0].payload["terminal"], true);
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].payload["recovered"], true);
    assert!(canceled[0].seq < completed[0].seq);
    assert!(
        canceled[0].seq > requested.seq.saturating_add(1),
        "rolled-back append sequence values were unexpectedly reused"
    );
    assert_eq!(manager.pending_dynamic_tools_count(), 0);
    assert!(!manager.recovery_receipts.lock().contains_key(&thread.id));

    manager.get_thread(&thread.id).await?;
    let replay = manager.events_since(&thread.id, None)?;
    assert_eq!(
        replay
            .iter()
            .filter(|event| {
                event.event == "tool_call.canceled"
                    && event.payload.get("call_id").and_then(Value::as_str) == Some(call_id)
            })
            .count(),
        1
    );
    assert_eq!(
        replay
            .iter()
            .filter(|event| {
                event.event == "turn.completed" && event.turn_id.as_deref() == Some(turn_id)
            })
            .count(),
        1
    );
    Ok(())
}

#[test]
fn pending_dynamic_tool_registry_rejects_duplicates_and_is_bounded() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let mut receivers = Vec::with_capacity(MAX_PENDING_DYNAMIC_TOOL_CALLS);
    receivers.push(manager.register_pending_dynamic_tool_for_test(
        "thread-bound",
        "turn-bound",
        "call-0",
    )?);
    assert!(
        manager
            .register_pending_dynamic_tool_for_test("thread-bound", "turn-bound", "call-0",)
            .is_err(),
        "duplicate call IDs must not replace an existing result channel"
    );

    for index in 1..MAX_PENDING_DYNAMIC_TOOL_CALLS {
        receivers.push(manager.register_pending_dynamic_tool_for_test(
            "thread-bound",
            "turn-bound",
            &format!("call-{index}"),
        )?);
    }
    assert_eq!(
        manager.pending_dynamic_tools_count(),
        MAX_PENDING_DYNAMIC_TOOL_CALLS
    );
    let error = manager
        .register_pending_dynamic_tool_for_test("thread-bound", "turn-bound", "call-over-limit")
        .expect_err("pending dynamic tool registry exceeded its hard limit");
    assert!(
        error
            .to_string()
            .contains("pending dynamic tool call limit")
    );
    Ok(())
}

#[tokio::test]
async fn dynamic_tool_timeout_clears_snapshot_and_emits_once() -> Result<()> {
    use crate::tools::spec::{DynamicToolExecutor, ToolError};

    let manager = test_manager(test_runtime_dir())?;
    manager.set_test_dynamic_tool_result_timeout_ms(25);
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "let an external lookup time out".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "dynamic_timeout".to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;

    let error = DynamicToolExecutor::execute_dynamic_tool(
        &manager,
        Some(thread.id.clone()),
        None,
        "slow_lookup".to_string(),
        json!({ "marker": "request-only" }),
    )
    .await
    .expect_err("dynamic tool unexpectedly resolved");
    assert!(matches!(error, ToolError::Timeout { .. }));
    assert!(
        manager
            .get_thread_detail(&thread.id)
            .await?
            .pending_dynamic_tool_calls
            .is_empty()
    );
    let timeout_events = manager
        .events_since(&thread.id, None)?
        .into_iter()
        .filter(|event| event.event == "tool_call.timeout")
        .collect::<Vec<_>>();
    assert_eq!(timeout_events.len(), 1);
    assert_eq!(timeout_events[0].turn_id.as_deref(), Some(turn.id.as_str()));
    assert_eq!(timeout_events[0].payload["status"], "timeout");
    assert_eq!(timeout_events[0].payload["timeout_secs"], 0);
    assert!(timeout_events[0].payload.get("arguments").is_none());

    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    Ok(())
}

#[test]
fn dynamic_tool_result_backstop_matches_mcp_tools_call_budget() {
    // The dynamic-result wait is a backstop for client-executed tools, not a
    // cap on the tool itself: the MCP `tools/call` budget legitimately allows
    // 1800s executions, so the backstop must not fire earlier than that.
    assert_eq!(
        DYNAMIC_TOOL_RESULT_TIMEOUT,
        Duration::from_secs(codewhale_mcp::config::McpTimeouts::default().execute_timeout),
        "dynamic tool-result backstop must not undercut the MCP tools/call budget"
    );
    assert_eq!(DYNAMIC_TOOL_RESULT_TIMEOUT, Duration::from_secs(1800));
}

#[tokio::test]
async fn terminal_turn_cancels_pending_dynamic_tool_exactly_once() -> Result<()> {
    use crate::tools::spec::DynamicToolExecutor;

    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "cancel an external lookup with the turn".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "dynamic_cancel".to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;

    let executor = manager.clone();
    let executor_thread_id = thread.id.clone();
    let execution = tokio::spawn(async move {
        DynamicToolExecutor::execute_dynamic_tool(
            &executor,
            Some(executor_thread_id),
            None,
            "cancel_lookup".to_string(),
            json!({ "id": "pending" }),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !manager
                .get_thread_detail(&thread.id)
                .await?
                .pending_dynamic_tool_calls
                .is_empty()
            {
                break Ok::<_, anyhow::Error>(());
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("dynamic call did not become pending")??;

    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Interrupted,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    execution
        .await
        .context("dynamic tool task panicked")?
        .expect_err("terminal turn unexpectedly resolved the dynamic tool");

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let detail = manager.get_thread_detail(&thread.id).await?;
            let canceled = manager
                .events_since(&thread.id, None)?
                .into_iter()
                .filter(|event| event.event == "tool_call.canceled")
                .collect::<Vec<_>>();
            if detail.pending_dynamic_tool_calls.is_empty() && canceled.len() == 1 {
                assert_eq!(canceled[0].payload["status"], "canceled");
                assert_eq!(canceled[0].payload["terminal"], true);
                break Ok::<_, anyhow::Error>(());
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("terminal dynamic call did not disappear exactly once")??;
    assert_eq!(
        turn.id,
        manager
            .get_thread(&thread.id)
            .await?
            .latest_turn_id
            .unwrap()
    );
    Ok(())
}

/// DESKTOP-QA-20260923 bug 1: the engine's 60s "Still waiting for tool
/// approval" heartbeat is queued while the relay is parked on the external
/// decision. It must never be sequenced after `approval.decided`, where it
/// reads as a live claim that the (already answered) call is still waiting.
#[tokio::test]
async fn engine_withdrawal_retires_approval_and_unblocks_queued_events() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "run extension".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "engine_withdrawal".to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    let request = |id: &str| EngineEvent::ApprovalRequired {
        id: id.to_string(),
        tool_name: "exec_shell".to_string(),
        description: "extension call".to_string(),
        input: json!({"command": "echo example"}),
        approval_key: format!("extension-key:{id}"),
        approval_grouping_key: "extension-group".to_string(),
        intent_summary: None,
        approval_force_prompt: true,
    };
    harness.tx_event.send(request("ext-1.1")).await?;
    let withdrawn_id = await_approval_identity(&manager, &thread.id, "ext-1.1").await?;

    harness.tx_event.send(EngineEvent::UserInputRequired {
        id: "nested-during-approval".into(),
        request: serde_json::from_value(json!({"require_typed_answers":true,"questions":[{"id":"note","question":"Any note?","allow_free_text":true}]}))?,
    }).await?;
    harness
        .tx_event
        .send(EngineEvent::UserInputClosed {
            id: "nested-during-approval".into(),
            outcome: crate::core::events::UserInputClosedOutcome::TimedOut,
        })
        .await?;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if manager.events_since(&thread.id, None)?.iter().any(|event| {
                event.event == "user_input.canceled"
                    && event.payload["input_id"] == "nested-during-approval"
            }) {
                return Ok::<_, anyhow::Error>(());
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("closure parked behind an unrelated approval")??;
    let waiting = manager.get_thread_detail(&thread.id).await?;
    assert!(waiting.pending_user_inputs.is_empty());
    assert_eq!(waiting.pending_approvals.len(), 1);
    assert!(
        !manager
            .submit_user_input(
                &thread.id,
                "nested-during-approval",
                crate::tools::user_input::UserInputResponse {
                    answers: Vec::new()
                }
            )
            .await?
    );

    // An unrelated event ahead of withdrawal must not park the event pump.
    harness
        .tx_event
        .send(EngineEvent::MessageStarted { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::MessageDelta {
            index: 0,
            content: "continued".into(),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ApprovalWithdrawn {
            id: "ext-1.1".into(),
        })
        .await?;
    harness.tx_event.send(request("ext-1.2")).await?;
    let next_id = await_approval_identity(&manager, &thread.id, "ext-1.2").await?;
    assert_ne!(withdrawn_id, next_id);
    assert!(
        !manager.deliver_external_approval(
            &withdrawn_id,
            ExternalApprovalDecision::Allow { remember: true },
        ),
        "withdrawal removes the capability before any late allow can create a grant"
    );
    let detail = manager.get_thread_detail(&thread.id).await?;
    assert_eq!(detail.pending_approvals.len(), 1);
    assert_eq!(detail.pending_approvals[0].id, next_id);
    assert!(detail.approval_grants.is_empty());
    assert!(
        manager.deliver_external_approval(
            &next_id,
            ExternalApprovalDecision::Deny { remember: false }
        )
    );
    assert_eq!(
        harness.recv_approval_event().await,
        Some(MockApprovalEvent::Denied {
            id: "ext-1.2".into()
        }),
        "withdrawal itself never dispatches a fabricated user decision"
    );
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    wait_for_terminal_turn(&manager, &turn.id).await?;
    assert!(manager.events_since(&thread.id, None)?.iter().any(|event| {
        event.event == "approval.decided"
            && event.payload["approval_id"] == withdrawn_id
            && event.payload["tool_call_id"] == "ext-1.1"
            && event.payload["cancelled"] == true
            && event.payload["remember"] == false
    }));
    assert_eq!(manager.pending_approvals_count(), 0);
    Ok(())
}

#[tokio::test]
async fn approval_wait_heartbeat_is_never_sequenced_after_the_decision() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;

    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "needs approval".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));

    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "engine_turn_wait".to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    // The Responses client joins call and item ids with `|`.
    let call_id = "call_00_wait|fc_99765c30";
    harness
        .tx_event
        .send(EngineEvent::ApprovalRequired {
            approval_key: "wait-key".to_string(),
            approval_grouping_key: "wait-key".to_string(),
            id: call_id.to_string(),
            tool_name: "run_verifiers".to_string(),
            description: "verifiers".to_string(),
            input: serde_json::json!({}),
            intent_summary: None,
            approval_force_prompt: false,
        })
        .await?;
    let approval_id = await_approval_identity(&manager, &thread.id, call_id).await?;

    // The engine's heartbeat fires while the card is still unanswered.
    harness
        .tx_event
        .send(EngineEvent::Status {
            message: format!(
                "Still waiting for tool approval on `{call_id}` after 60s — the turn is parked here until it is answered"
            ),
        })
        .await?;
    sleep(Duration::from_millis(100)).await;

    assert!(manager.deliver_external_approval(
        &approval_id,
        ExternalApprovalDecision::Allow { remember: false },
    ));
    assert_eq!(
        harness.recv_approval_event().await,
        Some(MockApprovalEvent::Approved {
            id: call_id.to_string(),
        })
    );

    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    wait_for_terminal_turn(&manager, &turn.id).await?;

    let events = manager.events_since(&thread.id, None)?;
    let decided_at = events
        .iter()
        .position(|event| event.event == "approval.decided")
        .context("approval.decided was not emitted")?;
    let stale: Vec<_> = events[decided_at..]
        .iter()
        .filter(|event| {
            event.payload["item"]["detail"]
                .as_str()
                .is_some_and(|detail| detail.starts_with("Still waiting for tool approval"))
        })
        .map(|event| (event.seq, event.payload["item"]["detail"].clone()))
        .collect();
    assert!(
        stale.is_empty(),
        "approval-wait heartbeat sequenced after approval.decided: {stale:?}"
    );
    Ok(())
}

#[tokio::test]
async fn approval_required_external_deny_is_denied() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let _turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "needs approval".to_string(),
                input_summary: None,
                model: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));

    harness
        .tx_event
        .send(EngineEvent::ApprovalRequired {
            approval_key: "key2".to_string(),
            approval_grouping_key: "key2".to_string(),
            id: "tool_external_deny".to_string(),
            tool_name: "exec_command".to_string(),
            description: "external deny".to_string(),
            input: serde_json::json!({}),
            intent_summary: None,
            approval_force_prompt: false,
        })
        .await?;

    let deadline = Instant::now() + APPROVAL_READINESS_TIMEOUT;
    while Instant::now() < deadline && manager.pending_approvals_count() == 0 {
        sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(manager.pending_approvals_count(), 1);

    let approval_id = await_approval_identity(&manager, &thread.id, "tool_external_deny").await?;
    assert!(manager.deliver_external_approval(
        &approval_id,
        ExternalApprovalDecision::Deny { remember: false },
    ));
    assert_eq!(
        harness.recv_approval_event().await,
        Some(MockApprovalEvent::Denied {
            id: "tool_external_deny".to_string(),
        })
    );

    // A settled capability is spent: replaying it neither re-decides nor
    // reaches the engine a second time.
    assert!(!manager.deliver_external_approval(
        &approval_id,
        ExternalApprovalDecision::Allow { remember: true },
    ));
    assert_eq!(manager.pending_approvals_count(), 0);
    assert!(
        !manager.store.load_thread(&thread.id)?.auto_approve,
        "a replayed decision must not apply its remember flag"
    );

    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    Ok(())
}

/// Providers restart their tool-call ID counters per response, so two threads
/// can hold gated calls whose raw IDs are byte-equal. While the waiter map was
/// keyed by that raw ID, the second registration silently evicted the first:
/// one thread's approval could authorize the other thread's tool call, and the
/// evicted thread waited out its timeout for a decision that had been made.
#[tokio::test]
async fn identical_raw_tool_call_ids_on_two_threads_stay_independently_gated() -> Result<()> {
    const RAW_CALL_ID: &str = "call_1";
    let manager = test_manager(test_runtime_dir())?;

    let mut gated = Vec::new();
    let mut inputs = std::collections::HashMap::new();
    for prompt in ["first gated turn", "second gated turn"] {
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let mut harness = install_mock_engine(&manager, &thread.id).await;
        manager
            .start_turn(
                &thread.id,
                StartTurnRequest {
                    prompt: prompt.to_string(),
                    ..Default::default()
                },
            )
            .await?;
        assert!(matches!(
            harness.rx_op.recv().await,
            Some(Op::SendMessage(TurnSpec { .. }))
        ));
        let input =
            json!({"command":"echo fixture","owner_prompt":prompt,"nested":{"values":[null,true]}});
        harness
            .tx_event
            .send(EngineEvent::ApprovalRequired {
                approval_key: RAW_CALL_ID.to_string(),
                approval_grouping_key: RAW_CALL_ID.to_string(),
                id: RAW_CALL_ID.to_string(),
                tool_name: "exec_command".to_string(),
                description: format!("gated {prompt}"),
                input: input.clone(),
                intent_summary: None,
                approval_force_prompt: false,
            })
            .await?;
        let approval_id = await_approval_identity(&manager, &thread.id, RAW_CALL_ID).await?;
        inputs.insert(thread.id.clone(), input);
        gated.push((thread, harness, approval_id));
    }

    // Neither registration evicted the other, and each thread's snapshot shows
    // only its own capability.
    assert_eq!(manager.pending_approvals_count(), 2);
    assert_ne!(
        gated[0].2, gated[1].2,
        "each registration must mint its own capability"
    );
    for (thread, _, approval_id) in &gated {
        let pending = manager
            .get_thread_detail(&thread.id)
            .await?
            .pending_approvals;
        assert_eq!(pending.len(), 1);
        assert_eq!(&pending[0].id, approval_id);
        // Both snapshots correlate to the same raw call ID — that is the
        // provider behavior this fix survives — while the capability that
        // settles each one is distinct.
        assert_eq!(pending[0].tool_call_id.as_deref(), Some(RAW_CALL_ID));
        assert_eq!(pending[0].input.as_ref(), inputs.get(&thread.id));
        let reloaded: PendingApprovalRequest =
            serde_json::from_value(serde_json::to_value(&pending[0])?)?;
        assert_eq!(reloaded.input.as_ref(), inputs.get(&thread.id));
    }

    // The shared correlator is visible to both clients and settles neither.
    assert!(!manager.deliver_external_approval(
        RAW_CALL_ID,
        ExternalApprovalDecision::Allow { remember: false },
    ));
    assert_eq!(manager.pending_approvals_count(), 2);

    let mut gated = gated.into_iter();
    let (thread_a, mut harness_a, approval_a) = gated.next().context("first gated thread")?;
    let (thread_b, mut harness_b, approval_b) = gated.next().context("second gated thread")?;

    // Deciding the first settles the first, addressed to the engine by the raw
    // call ID it gated on.
    assert!(manager.deliver_external_approval(
        &approval_a,
        ExternalApprovalDecision::Allow { remember: false },
    ));
    assert_eq!(
        harness_a.recv_approval_event().await,
        Some(MockApprovalEvent::Approved {
            id: RAW_CALL_ID.to_string(),
        })
    );

    // ...and cross-authorizes nothing: the second thread is still waiting.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), harness_b.recv_approval_event())
            .await
            .is_err(),
        "an approval for one thread must not decide another thread's identical call id"
    );
    assert_eq!(manager.pending_approvals_count(), 1);
    let still_pending = manager
        .get_thread_detail(&thread_b.id)
        .await?
        .pending_approvals;
    assert_eq!(still_pending.len(), 1);
    assert_eq!(still_pending[0].id, approval_b);
    assert_eq!(still_pending[0].input.as_ref(), inputs.get(&thread_b.id));
    assert!(
        manager
            .get_thread_detail(&thread_a.id)
            .await?
            .pending_approvals
            .is_empty()
    );

    assert!(manager.deliver_external_approval(
        &approval_b,
        ExternalApprovalDecision::Deny { remember: false },
    ));
    assert_eq!(
        harness_b.recv_approval_event().await,
        Some(MockApprovalEvent::Denied {
            id: RAW_CALL_ID.to_string(),
        })
    );
    assert_eq!(manager.pending_approvals_count(), 0);

    for harness in [&harness_a, &harness_b] {
        harness
            .tx_event
            .send(EngineEvent::TurnComplete {
                turn_id: None,
                submission_id: None,
                compaction_id: None,
                usage: Usage::default(),
                parent_route_usage: Usage::default(),
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await?;
    }
    Ok(())
}

/// Drive Runtime's actual event monitor and inspect the Engine inbox, rather
/// than restating the pure resolver's truth table (#6471).
#[tokio::test]
async fn runtime_approval_disposition_auto_paths_obey_force_and_posture() -> Result<()> {
    use crate::approval_log::ApprovalDecider;

    for (posture, forced, remembered, approved, denied_posture) in [
        ("full-access", false, false, true, None),
        ("full-access", false, true, true, None),
        (
            "full-access",
            true,
            false,
            false,
            Some("full_access_policy_hold"),
        ),
        (
            "full-access",
            true,
            true,
            false,
            Some("full_access_policy_hold"),
        ),
        ("never", false, false, false, Some("never")),
        ("never", false, true, false, Some("never")),
        ("ask", false, true, true, None),
    ] {
        let manager = test_manager(test_runtime_dir())?;
        let thread = manager
            .create_thread(CreateThreadRequest {
                permission_posture: Some(
                    if posture == "never" { "ask" } else { posture }.to_string(),
                ),
                ..Default::default()
            })
            .await?;
        let mut harness = install_mock_engine(&manager, &thread.id).await;
        let turn = manager
            .start_turn(
                &thread.id,
                StartTurnRequest {
                    prompt: "honor the exact approval disposition".to_string(),
                    ..Default::default()
                },
            )
            .await?;
        assert!(matches!(
            harness.rx_op.recv().await,
            Some(Op::SendMessage(TurnSpec { .. }))
        ));
        // Never is an internal live authority, not a public Runtime posture.
        if posture == "never" {
            harness.handle.publish_turn_authority(
                AppMode::Agent,
                true,
                false,
                false,
                ApprovalMode::Never,
                None,
            );
        }
        let grant = if remembered {
            Some(
                manager
                    .add_session_grant(&thread.id, &turn.id, "bash", "policy-group", "s")
                    .await
                    .context("fixture conversation grant")?,
            )
        } else {
            None
        };
        let input = json!({"approval_key":"extcall:ext:claimed","command":"echo fixture","nested":{"values":[null,true,3]}});
        harness
            .tx_event
            .send(EngineEvent::ApprovalRequired {
                approval_key: "policy-key".to_string(),
                approval_grouping_key: "policy-group".to_string(),
                id: "policy-call".to_string(),
                tool_name: "bash".to_string(),
                // Text and input cannot impersonate a Rust-minted origin key.
                description: "Requested by extension:claimed".to_string(),
                input: input.clone(),
                intent_summary: None,
                approval_force_prompt: forced,
            })
            .await?;
        let decision =
            tokio::time::timeout(APPROVAL_READINESS_TIMEOUT, harness.recv_approval_decision())
                .await
                .context("automatic disposition must not open a modal")?;
        let expected = if approved {
            MockApprovalEvent::Approved {
                id: "policy-call".to_string(),
            }
        } else {
            MockApprovalEvent::Denied {
                id: "policy-call".to_string(),
            }
        };
        let by = if approved && posture == "ask" {
            ApprovalDecider::SessionRule
        } else {
            ApprovalDecider::Posture
        };
        assert_eq!(
            decision,
            Some((expected, Some(by))),
            "{posture}/{forced}/{remembered}"
        );
        assert_eq!(manager.pending_approvals_count(), 0);
        let detail = manager.get_thread_detail(&thread.id).await?;
        assert!(detail.pending_approvals.is_empty());
        assert_eq!(detail.approval_grants.len(), usize::from(remembered));
        let required = manager
            .events_since(&thread.id, None)?
            .into_iter()
            .find(|event| event.event == "approval.required");
        if approved {
            let required = required.context("automatic approval request receipt")?;
            assert_eq!(required.payload.get("input"), Some(&input));
            assert_eq!(required.payload["tool_call_id"], "policy-call");
        } else {
            assert!(required.is_none());
        }
        let decided = manager
            .events_since(&thread.id, None)?
            .into_iter()
            .find(|event| {
                event.event == "approval.decided" && event.payload["tool_call_id"] == "policy-call"
            })
            .context("automatic decision receipt")?;
        assert_eq!(
            decided.payload["decision"],
            if approved { "allow" } else { "deny" }
        );
        assert_eq!(decided.payload["auto"], true);
        assert_eq!(decided.payload["remember"], false);
        if let Some(posture) = denied_posture {
            assert_eq!(decided.payload["posture"], posture);
        }
        if approved && posture == "ask" {
            assert_eq!(decided.payload["grant_id"], grant.unwrap().grant_id);
        } else {
            assert!(
                decided
                    .payload
                    .get("grant_id")
                    .is_none_or(serde_json::Value::is_null)
            );
        }
        let approval_id = decided.payload["approval_id"]
            .as_str()
            .context("opaque decision identity")?;
        assert!(approval_id.starts_with("approval_"));
        assert_ne!(approval_id, "policy-call");
        for id in ["policy-call", approval_id] {
            assert!(!manager.deliver_external_approval(
                id,
                ExternalApprovalDecision::Allow { remember: false },
            ));
        }
        harness
            .tx_event
            .send(EngineEvent::TurnComplete {
                turn_id: None,
                submission_id: None,
                compaction_id: None,
                usage: Usage::default(),
                parent_route_usage: Usage::default(),
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await?;
        assert_eq!(
            wait_for_terminal_turn(&manager, &turn.id).await?.status,
            RuntimeTurnStatus::Completed,
        );
    }
    Ok(())
}

#[tokio::test]
async fn runtime_approval_disposition_forced_requests_require_human() -> Result<()> {
    use crate::approval_log::ApprovalDecider;

    for (posture, tool_name, extension_origin) in [
        ("full-access", "bash", true),
        ("full-access", "web_search", true),
        ("ask", "bash", false),
    ] {
        let manager = test_manager(test_runtime_dir())?;
        let thread = manager
            .create_thread(CreateThreadRequest {
                permission_posture: Some(posture.to_string()),
                ..Default::default()
            })
            .await?;
        let mut harness = install_mock_engine(&manager, &thread.id).await;
        let turn = manager
            .start_turn(
                &thread.id,
                StartTurnRequest {
                    prompt: "wait for the exact human decision".to_string(),
                    ..Default::default()
                },
            )
            .await?;
        assert!(matches!(
            harness.rx_op.recv().await,
            Some(Op::SendMessage(TurnSpec { .. }))
        ));
        let input = json!({"command": "echo fixture", "query": "fixture"});
        let (key, group) = if extension_origin {
            crate::tools::approval_cache::extension_origin_approval_keys(
                "ext:fixture@reviewed-hash",
                None,
                tool_name,
                &input,
            )
        } else {
            crate::tools::approval_cache::approval_keys_for_call(None, tool_name, &input)
        };
        let grant = manager
            .add_session_grant(&thread.id, &turn.id, tool_name, &group.0, "s")
            .await
            .context("fixture conversation grant")?;
        harness
            .tx_event
            .send(EngineEvent::ApprovalRequired {
                approval_key: key.0,
                approval_grouping_key: group.0,
                id: "human-call".to_string(),
                tool_name: tool_name.to_string(),
                description: "exact human approval required".to_string(),
                input,
                intent_summary: None,
                approval_force_prompt: true,
            })
            .await?;
        let approval_id = await_approval_identity(&manager, &thread.id, "human-call").await?;
        let detail = manager.get_thread_detail(&thread.id).await?;
        assert_eq!(detail.pending_approvals.len(), 1, "{posture}/{tool_name}");
        assert_eq!(detail.pending_approvals[0].id, approval_id);
        assert_eq!(detail.pending_approvals[0].turn_id, turn.id);
        assert_eq!(
            detail.pending_approvals[0].tool_call_id.as_deref(),
            Some("human-call")
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), harness.recv_approval_decision())
                .await
                .is_err(),
            "neither Full Access nor a remembered grant may answer: {posture}/{tool_name}"
        );
        assert!(!manager.deliver_external_approval(
            "human-call",
            ExternalApprovalDecision::Allow { remember: false },
        ));
        assert!(manager.deliver_external_approval(
            &approval_id,
            ExternalApprovalDecision::Allow { remember: false },
        ));
        assert_eq!(
            tokio::time::timeout(APPROVAL_READINESS_TIMEOUT, harness.recv_approval_decision())
                .await?,
            Some((
                MockApprovalEvent::Approved {
                    id: "human-call".to_string()
                },
                Some(ApprovalDecider::User),
            )),
        );
        assert_eq!(manager.pending_approvals_count(), 0);
        assert!(!manager.deliver_external_approval(
            &approval_id,
            ExternalApprovalDecision::Allow { remember: false },
        ));
        assert_eq!(
            manager.approval_grants_for_thread(&thread.id)[0].grant_id,
            grant.grant_id
        );
        let decided = manager
            .events_since(&thread.id, None)?
            .into_iter()
            .find(|event| {
                event.event == "approval.decided" && event.payload["approval_id"] == approval_id
            })
            .context("human decision receipt")?;
        assert_eq!(decided.payload["decision"], "allow");
        assert_ne!(
            decided.payload.get("auto").and_then(Value::as_bool),
            Some(true)
        );
        harness
            .tx_event
            .send(EngineEvent::TurnComplete {
                turn_id: None,
                submission_id: None,
                compaction_id: None,
                usage: Usage::default(),
                parent_route_usage: Usage::default(),
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await?;
        assert_eq!(
            wait_for_terminal_turn(&manager, &turn.id).await?.status,
            RuntimeTurnStatus::Completed,
        );
    }
    Ok(())
}

#[tokio::test]
async fn auto_review_force_prompt_is_denied_without_opening_a_modal() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            permission_posture: Some("auto-review".to_string()),
            ..Default::default()
        })
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "review the gated action".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec {
            approval_mode: ApprovalMode::Auto,
            ..
        }))
    ));

    harness
        .tx_event
        .send(EngineEvent::ApprovalRequired {
            approval_key: "auto_hold".to_string(),
            approval_grouping_key: "auto_hold".to_string(),
            id: "tool_auto_hold".to_string(),
            tool_name: "exec_command".to_string(),
            description: "policy hold under auto review".to_string(),
            input: serde_json::json!({}),
            intent_summary: None,
            approval_force_prompt: true,
        })
        .await?;

    let decision = tokio::time::timeout(Duration::from_secs(2), harness.recv_approval_event())
        .await
        .context("Auto-Review hold should resolve without a modal")?;
    assert_eq!(
        decision,
        Some(MockApprovalEvent::Denied {
            id: "tool_auto_hold".to_string(),
        })
    );
    assert_eq!(manager.pending_approvals_count(), 0);
    let decided = manager
        .events_since(&thread.id, None)?
        .into_iter()
        .find(|event| {
            event.event == "approval.decided"
                && event.payload.get("posture").and_then(Value::as_str) == Some("auto_review")
        })
        .context("auto-review hold must emit approval.decided")?;
    assert_eq!(
        decided.payload.get("tool_call_id").and_then(Value::as_str),
        Some("tool_auto_hold")
    );
    // Even on a path with no waiter, `approval_id` stays a runtime-minted
    // value so a client never learns a raw call ID through that field.
    let approval_id = decided
        .payload
        .get("approval_id")
        .and_then(Value::as_str)
        .context("missing approval_id")?;
    assert_ne!(approval_id, "tool_auto_hold");
    assert!(approval_id.starts_with("approval_"));
    assert!(!manager.deliver_external_approval(
        approval_id,
        ExternalApprovalDecision::Allow { remember: false },
    ));

    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    let terminal = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Completed);
    Ok(())
}

#[tokio::test]
async fn approval_interrupt_revokes_waiter_and_rejects_late_actions() -> Result<()> {
    // Also cover an allow already queued when Stop wins, before the monitor
    // resumes: it must not dispatch or persist remember=true.
    for queued_allow in [false, true] {
        let manager = test_manager(test_runtime_dir())?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let mut harness = install_mock_engine(&manager, &thread.id).await;
        let turn = manager
            .start_turn(
                &thread.id,
                StartTurnRequest {
                    prompt: "cancel the pending write".to_string(),
                    ..Default::default()
                },
            )
            .await?;
        assert!(matches!(
            harness.rx_op.recv().await,
            Some(Op::SendMessage(_))
        ));
        let event = |id: &str| EngineEvent::ApprovalRequired {
            approval_key: id.to_string(),
            approval_grouping_key: id.to_string(),
            id: id.to_string(),
            tool_name: "computer_set_value".to_string(),
            description: "pending write".to_string(),
            input: json!({}),
            intent_summary: None,
            approval_force_prompt: false,
        };
        harness.tx_event.send(event("pending_write")).await?;
        let approval = await_approval_identity(&manager, &thread.id, "pending_write").await?;
        let (other_approval, mut other_rx) =
            manager.register_pending_approval_for_thread_for_test("other-thread", "other-call");

        if queued_allow {
            assert!(manager.deliver_external_approval(
                &approval,
                ExternalApprovalDecision::Allow { remember: true },
            ));
        }
        manager.interrupt_turn(&thread.id, &turn.id).await?;
        assert!(!manager.deliver_external_approval(
            &approval,
            ExternalApprovalDecision::Allow { remember: true },
        ));
        assert!(
            manager
                .get_thread_detail(&thread.id)
                .await?
                .pending_approvals
                .is_empty()
        );
        assert_eq!(
            manager.pending_approvals_count(),
            1,
            "other task remains gated"
        );
        assert!(matches!(
            other_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        let decision = tokio::time::timeout(Duration::from_secs(2), harness.recv_approval_event())
            .await
            .context("Stop must wake the approval monitor")?;
        assert_eq!(
            decision,
            Some(MockApprovalEvent::Denied {
                id: "pending_write".to_string()
            })
        );
        assert!(!manager.store.load_thread(&thread.id)?.auto_approve);
        assert!(
            manager.events_since(&thread.id, None)?.iter().any(|event| {
                event.event == "approval.decided"
                    && event.payload["approval_id"] == approval
                    && event.payload["decision"] == "deny"
                    && event.payload["cancelled"] == true
            }),
            "cancelled approval must clear the client's pending UI"
        );

        harness.tx_event.send(event("queued_after_stop")).await?;
        let decision = tokio::time::timeout(Duration::from_secs(2), harness.recv_approval_event())
            .await
            .context("late approval event must fail closed")?;
        assert_eq!(
            decision,
            Some(MockApprovalEvent::Denied {
                id: "queued_after_stop".to_string()
            })
        );
        assert!(
            manager
                .get_thread_detail(&thread.id)
                .await?
                .pending_approvals
                .is_empty()
        );
        assert!(
            !manager.events_since(&thread.id, None)?.iter().any(|event| {
                event.event == "approval.required"
                    && event.payload["tool_call_id"] == "queued_after_stop"
            })
        );
        harness
            .tx_event
            .send(EngineEvent::TurnComplete {
                turn_id: None,
                submission_id: None,
                compaction_id: None,
                usage: Usage::default(),
                parent_route_usage: Usage::default(),
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Interrupted,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await?;
        assert_eq!(
            wait_for_terminal_turn(&manager, &turn.id).await?.status,
            RuntimeTurnStatus::Interrupted
        );
        assert!(manager.deliver_external_approval(
            &other_approval,
            ExternalApprovalDecision::Deny { remember: false },
        ));
        assert!(matches!(
            other_rx.await?,
            ExternalApprovalDecision::Deny { .. }
        ));
        manager
            .start_turn(
                &thread.id,
                StartTurnRequest {
                    prompt: "new turn after stop".to_string(),
                    ..Default::default()
                },
            )
            .await?;
        assert!(matches!(
            harness.rx_op.recv().await,
            Some(Op::SendMessage(_))
        ));
    }
    Ok(())
}

#[tokio::test]
async fn approval_timeout_denies_clears_ui_and_next_turn_can_start() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    manager.set_test_approval_decision_timeout_ms(25);
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "needs approval".to_string(),
                input_summary: None,
                model: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));

    harness
        .tx_event
        .send(EngineEvent::ApprovalRequired {
            approval_key: "timeout_key".to_string(),
            approval_grouping_key: "timeout_key".to_string(),
            id: "tool_timeout".to_string(),
            tool_name: "exec_command".to_string(),
            description: "external timeout".to_string(),
            input: serde_json::json!({}),
            intent_summary: None,
            approval_force_prompt: false,
        })
        .await?;

    let decision = tokio::time::timeout(APPROVAL_READINESS_TIMEOUT, harness.recv_approval_event())
        .await
        .context("approval timeout should resolve the engine's wait")?;
    // The engine hears a timeout, not the user's denial, so the model and the
    // approval receipt both say "timed out" and the budget slot is refunded.
    assert_eq!(
        decision,
        Some(MockApprovalEvent::TimedOut {
            id: "tool_timeout".to_string(),
        })
    );
    assert_eq!(manager.pending_approvals_count(), 0);

    let approval_id = await_approval_identity(&manager, &thread.id, "tool_timeout").await?;
    let events = manager.events_since(&thread.id, None)?;
    assert!(
        events.iter().any(|event| {
            event.event == "approval.timeout"
                && event.payload.get("approval_id").and_then(Value::as_str)
                    == Some(approval_id.as_str())
                && event.payload.get("tool_call_id").and_then(Value::as_str) == Some("tool_timeout")
        }),
        "timeout event should be persisted against the same opaque id it opened with"
    );
    assert!(
        events.iter().any(|event| {
            event.event == "approval.decided"
                && event.payload.get("approval_id").and_then(Value::as_str)
                    == Some(approval_id.as_str())
                && event.payload.get("tool_call_id").and_then(Value::as_str) == Some("tool_timeout")
                && event.payload.get("decision").and_then(Value::as_str) == Some("deny")
                && event.payload.get("timeout").and_then(Value::as_bool) == Some(true)
        }),
        "timeout should also emit approval.decided so clients can clear pending UI"
    );
    // A decision arriving after the timeout cancelled the waiter must not
    // resurrect it — the engine already has its denial.
    assert!(!manager.deliver_external_approval(
        &approval_id,
        ExternalApprovalDecision::Allow { remember: false },
    ));

    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    let terminal = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Completed);

    let _next = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "after timeout".to_string(),
                input_summary: None,
                model: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                ..Default::default()
            },
        )
        .await?;
    assert!(
        matches!(
            harness.rx_op.recv().await,
            Some(Op::SendMessage(TurnSpec { .. }))
        ),
        "thread should accept a fresh turn after approval timeout cleanup"
    );

    Ok(())
}

#[tokio::test]
async fn thinking_delta_emits_agent_reasoning_item() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: Some(true),
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let mut event_rx = manager.subscribe_events();
    let _turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "show your thinking".to_string(),
                input_summary: None,
                model: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: Some(true),
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));

    harness
        .tx_event
        .send(EngineEvent::ThinkingStarted { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ThinkingDelta {
            index: 0,
            content: "Let me reason about this.".to_string(),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ThinkingComplete { index: 0 })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;

    // A busy or constrained runner can be quiet for more than one 200 ms poll
    // even though the engine is still making progress. Keep polling until the
    // actual deadline instead of treating the first quiet interval as failure.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut delta_seen = false;
    let mut completed_seen = false;
    while Instant::now() < deadline && (!delta_seen || !completed_seen) {
        match tokio::time::timeout(Duration::from_millis(200), event_rx.recv()).await {
            Ok(Ok(record)) => {
                if record.event == "item.delta"
                    && record.payload.get("kind").and_then(|v| v.as_str())
                        == Some("agent_reasoning")
                {
                    delta_seen = true;
                    assert_eq!(
                        record.payload.get("delta").and_then(|v| v.as_str()),
                        Some("Let me reason about this.")
                    );
                }
                if record.event == "item.completed"
                    && record
                        .payload
                        .get("item")
                        .and_then(|v| v.get("kind"))
                        .and_then(|v| v.as_str())
                        == Some("agent_reasoning")
                {
                    completed_seen = true;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }
    assert!(delta_seen, "expected item.delta with kind=agent_reasoning");
    assert!(
        completed_seen,
        "expected item.completed for the reasoning item"
    );
    Ok(())
}

#[tokio::test]
async fn deliver_external_approval_for_unknown_id_returns_false() {
    let manager = test_manager(test_runtime_dir()).expect("manager");
    assert!(!manager.deliver_external_approval(
        "no_such_approval",
        ExternalApprovalDecision::Allow { remember: false },
    ));
    assert_eq!(manager.pending_approvals_count(), 0);
}

/// E1/E2 regression: "allow for this conversation" on one call is a grant for
/// that tool and argument class. It never changes the thread's posture (which
/// used to flip to Full Access and publish a mid-turn posture change that
/// failed the approved call), it answers the queued same-class call without a
/// prompt, it cancels nothing, a different class still prompts, and it can be
/// revoked.
#[tokio::test]
async fn approval_remember_grants_tool_class_without_changing_posture() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let posture_before = manager.store.load_thread(&thread.id)?.permission_posture;
    assert!(!manager.store.load_thread(&thread.id)?.auto_approve);

    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "compare espresso machines".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));

    let search = |id: &str, q: &str| {
        let input = json!({ "search_query": [{ "q": q }] });
        EngineEvent::ApprovalRequired {
            approval_key: crate::tools::approval_cache::build_approval_key("web.run", &input).0,
            approval_grouping_key: crate::tools::approval_cache::build_approval_grouping_key(
                "web.run", &input,
            )
            .0,
            id: id.to_string(),
            tool_name: "web.run".to_string(),
            description: "Browse the web".to_string(),
            input,
            intent_summary: None,
            approval_force_prompt: false,
        }
    };
    // Three gated calls queued behind one prompt: two searches, one write.
    harness
        .tx_event
        .send(search("call_search_1", "espresso"))
        .await?;
    harness
        .tx_event
        .send(search("call_search_2", "grinders"))
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ApprovalRequired {
            approval_key: "write-key".to_string(),
            approval_grouping_key: "write-group".to_string(),
            id: "call_write".to_string(),
            tool_name: "write_file".to_string(),
            description: "write".to_string(),
            input: json!({ "path": thread.workspace.join("espresso.md") }),
            intent_summary: None,
            approval_force_prompt: false,
        })
        .await?;

    let approval_id = await_approval_identity(&manager, &thread.id, "call_search_1").await?;
    let pending = manager
        .get_thread_detail(&thread.id)
        .await?
        .pending_approvals;
    assert_eq!(
        pending[0].summary.as_deref(),
        Some("Search the web for 'espresso'"),
        "the approval carries a model-independent summary (E6)"
    );
    assert!(manager.deliver_external_approval(
        &approval_id,
        ExternalApprovalDecision::Allow { remember: true },
    ));
    assert_eq!(
        harness.recv_approval_event().await,
        Some(MockApprovalEvent::Approved {
            id: "call_search_1".to_string()
        })
    );
    // The queued same-class search is answered by the grant, not cancelled.
    assert_eq!(
        harness.recv_approval_event().await,
        Some(MockApprovalEvent::Approved {
            id: "call_search_2".to_string()
        })
    );
    // A different tool still asks.
    let write_approval = await_approval_identity(&manager, &thread.id, "call_write").await?;
    let detail = manager.get_thread_detail(&thread.id).await?;
    assert_eq!(detail.pending_approvals.len(), 1);
    assert_eq!(
        detail.pending_approvals[0].summary.as_deref(),
        Some("Write espresso.md"),
        "paths are workspace-relative"
    );

    // Posture is untouched everywhere: record, live engine, mailbox.
    let record = manager.store.load_thread(&thread.id)?;
    assert!(
        !record.auto_approve,
        "a session grant must not enable auto-approve"
    );
    assert_eq!(record.permission_posture, posture_before);
    assert_eq!(
        manager.active_turn_flags(&thread.id, &turn.id).await,
        Some((false, false))
    );
    assert!(
        harness.rx_op.try_recv().is_err(),
        "approving must not queue a posture change"
    );

    // The grant is named on the event stream and in the snapshot.
    assert_eq!(detail.approval_grants.len(), 1);
    let grant = detail.approval_grants[0].clone();
    assert_eq!(grant.tool_name, "web.run");
    assert_eq!(grant.summary, "Search the web for 'espresso'");
    let events = manager.events_since(&thread.id, None)?;
    assert!(events.iter().any(|event| {
        event.event == "approval.grant_added"
            && event.payload["grant"]["grant_id"] == grant.grant_id
    }));
    assert!(events.iter().any(|event| {
        event.event == "approval.decided"
            && event.payload["tool_call_id"] == "call_search_2"
            && event.payload["grant_id"] == grant.grant_id
    }));

    assert!(manager.deliver_external_approval(
        &write_approval,
        ExternalApprovalDecision::Deny { remember: false },
    ));
    let _ = harness.recv_approval_event().await;

    // Revoked, the next search prompts again.
    assert!(
        manager
            .revoke_approval_grant(&thread.id, &grant.grant_id)
            .await?
    );
    assert!(
        !manager
            .revoke_approval_grant(&thread.id, &grant.grant_id)
            .await?
    );
    harness
        .tx_event
        .send(search("call_search_3", "tampers"))
        .await?;
    await_approval_identity(&manager, &thread.id, "call_search_3").await?;
    assert!(
        manager
            .get_thread_detail(&thread.id)
            .await?
            .approval_grants
            .is_empty()
    );

    manager.interrupt_turn(&thread.id, &turn.id).await?;
    Ok(())
}

#[tokio::test]
async fn archiving_or_deleting_a_thread_ends_its_session_grants() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let archived = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let kept = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let grant = manager
        .add_session_grant(
            &archived.id,
            "turn_1",
            "web.run",
            "web:web.run:search_query",
            "Search the web for 'espresso'",
        )
        .await
        .expect("a live thread records the grant");
    manager
        .add_session_grant(
            &kept.id,
            "turn_1",
            "web.run",
            "web:web.run:search_query",
            "s",
        )
        .await
        .expect("a live thread records the grant");

    // A title edit leaves grants alone.
    manager
        .update_thread(
            &archived.id,
            UpdateThreadRequest {
                title: Some("renamed".to_string()),
                ..UpdateThreadRequest::default()
            },
        )
        .await?;
    assert_eq!(manager.approval_grants_for_thread(&archived.id).len(), 1);

    manager
        .update_thread(
            &archived.id,
            UpdateThreadRequest {
                archived: Some(true),
                ..UpdateThreadRequest::default()
            },
        )
        .await?;
    assert!(manager.approval_grants_for_thread(&archived.id).is_empty());
    let events = manager.events_since(&archived.id, None)?;
    assert!(events.iter().any(|event| {
        event.event == "approval.grant_revoked"
            && event.payload["grant"]["grant_id"] == grant.grant_id
    }));
    // Archiving has no quiescence gate: a prompt raised before the archive
    // can be answered "allow for this conversation" after it. That answer
    // must not plant a grant that survives the archive.
    assert!(
        manager
            .add_session_grant(
                &archived.id,
                "turn_1",
                "web.run",
                "web:web.run:search_query",
                "late remember",
            )
            .await
            .is_none(),
        "an archived thread records no new grant"
    );
    assert!(manager.approval_grants_for_thread(&archived.id).is_empty());
    // Unarchiving does not bring the grant back.
    manager
        .update_thread(
            &archived.id,
            UpdateThreadRequest {
                archived: Some(false),
                ..UpdateThreadRequest::default()
            },
        )
        .await?;
    assert!(manager.approval_grants_for_thread(&archived.id).is_empty());
    // Another thread's grants are untouched.
    assert_eq!(manager.approval_grants_for_thread(&kept.id).len(), 1);

    // Deleting a thread drops its grants with it.
    manager.discard_empty_thread(&kept.id).await?;
    assert!(manager.approval_grants_for_thread(&kept.id).is_empty());
    Ok(())
}

#[tokio::test]
async fn elevation_required_with_stale_active_turn_is_denied() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: Some(true),
            auto_approve: Some(true),
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "needs elevation".to_string(),
                input_summary: None,
                model: None,
                mode: None,
                allow_shell: None,
                trust_mode: Some(true),
                auto_approve: Some(true),
                ..Default::default()
            },
        )
        .await?;

    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    let active_turn = {
        let mut active = manager.active.lock().await;
        let state = active
            .engines
            .get_mut(&thread.id)
            .context("missing active thread state")?;
        state.active_turn.take().context("missing active turn")?
    };

    harness
        .tx_event
        .send(EngineEvent::ElevationRequired {
            tool_id: "tool_stale_elevated".to_string(),
            tool_name: "exec_command".to_string(),
            command: None,
            denial_reason: "sandbox denied".to_string(),
            blocked_network: false,
            blocked_write: false,
        })
        .await?;

    assert_eq!(
        harness.recv_approval_event().await,
        Some(MockApprovalEvent::Denied {
            id: "tool_stale_elevated".to_string(),
        })
    );

    manager
        .active
        .lock()
        .await
        .engines
        .get_mut(&thread.id)
        .context("missing active thread state")?
        .active_turn = Some(active_turn);

    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage {
                input_tokens: 0,
                output_tokens: 0,
                ..Usage::default()
            },
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;

    let terminal = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Failed);
    assert_eq!(terminal.error.as_deref(), Some(EMPTY_TURN_REASON));
    Ok(())
}

#[tokio::test]
async fn steer_turn_on_active_turn_records_item_and_event() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    let mut rx_steer = harness.rx_steer;
    let tx_event = harness.tx_event;
    let (steer_seen_tx, steer_seen_rx) = oneshot::channel::<String>();
    tokio::spawn(async move {
        if matches!(rx_op.recv().await, Some(Op::SendMessage(TurnSpec { .. }))) {
            let _ = tx_event
                .send(EngineEvent::TurnStarted {
                    turn_id: "engine_turn_steer".to_string(),
                    created_at: chrono::Utc::now(),
                    route: None,
                    submission_id: None,
                })
                .await;
            if let Some(steer) = rx_steer.recv().await {
                // Model an engine that commits the steer into its record;
                // `commit()` is what reports acceptance back to `steer_turn`.
                let _ = steer_seen_tx.send(steer.into_pending().commit());
            }
            let _ = tx_event
                .send(EngineEvent::MessageStarted { index: 0 })
                .await;
            let _ = tx_event
                .send(EngineEvent::MessageDelta {
                    index: 0,
                    content: "steered response".to_string(),
                })
                .await;
            let _ = tx_event
                .send(EngineEvent::MessageComplete { index: 0 })
                .await;
            let _ = tx_event
                .send(EngineEvent::TurnComplete {
                    turn_id: None,
                    submission_id: None,
                    compaction_id: None,
                    usage: Usage {
                        input_tokens: 8,
                        output_tokens: 9,
                        ..Usage::default()
                    },
                    parent_route_usage: Usage {
                        input_tokens: 8,
                        output_tokens: 9,
                        ..Usage::default()
                    },
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Completed,
                    error: None,
                    tool_catalog: None,
                    base_url: None,
                })
                .await;
        }
    });

    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "initial".to_string(),
                input_summary: None,
                model: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                ..Default::default()
            },
        )
        .await?;

    let steer_text = "add bullet list".to_string();
    let steered_turn = manager
        .steer_turn(
            &thread.id,
            &turn.id,
            SteerTurnRequest {
                prompt: steer_text.clone(),
            },
        )
        .await?;
    assert!(matches!(steered_turn.steer_count, 0 | 1));
    let observed_steer = steer_seen_rx
        .await
        .context("driver did not receive steer")?;
    assert_eq!(observed_steer, steer_text);

    tokio::time::timeout(TURN_SETTLEMENT_DEADLOCK_TIMEOUT, async {
        loop {
            let events = manager.events_since(&thread.id, None)?;
            let delivered = events.iter().any(|event| {
                event.turn_id.as_deref() == Some(turn.id.as_str()) && event.event == "turn.steered"
            });
            let completed = events.iter().any(|event| {
                event.turn_id.as_deref() == Some(turn.id.as_str())
                    && event.event == "item.completed"
                    && event.payload["item"]["detail"] == steer_text
            });
            if delivered && completed {
                return Ok::<_, anyhow::Error>(());
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .context("steer delivery did not settle")??;
    assert_eq!(manager.store.load_turn(&turn.id)?.steer_count, 1);

    let final_turn = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(final_turn.status, RuntimeTurnStatus::Completed);
    assert_eq!(final_turn.steer_count, 1);

    let events = manager.events_since(&thread.id, None)?;
    assert!(events.iter().any(|ev| ev.event == "turn.steered"));
    assert!(events.iter().any(|ev| {
        ev.event == "item.completed"
            && ev
                .payload
                .get("item")
                .and_then(|item| item.get("detail"))
                .and_then(Value::as_str)
                == Some("add bullet list")
    }));
    Ok(())
}

#[tokio::test]
async fn steer_receipts_outlive_caller_cancellation_after_engine_acceptance() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    let mut rx_steer = harness.rx_steer;
    let tx_event = harness.tx_event;

    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "initial".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));

    // Hold publication after durable persistence and mailbox acceptance so the
    // API future can be cancelled while the detached receipt task is pending.
    let emit_guard = manager.event_emit.lock().await;
    let steer_manager = manager.clone();
    let thread_id = thread.id.clone();
    let turn_id = turn.id.clone();
    let steer_task = tokio::spawn(async move {
        steer_manager
            .steer_turn(
                &thread_id,
                &turn_id,
                SteerTurnRequest {
                    prompt: "keep the accepted steer".to_string(),
                },
            )
            .await
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), rx_steer.recv())
            .await?
            .map(|steer| steer.into_pending().commit()),
        Some("keep the accepted steer".to_string())
    );
    steer_task.abort();
    assert!(
        steer_task
            .await
            .expect_err("caller task must be cancelled")
            .is_cancelled()
    );
    drop(emit_guard);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let events = manager.events_since(&thread.id, None)?;
        let steered = events.iter().any(|event| event.event == "turn.steered");
        let completed = events.iter().any(|event| {
            event.event == "item.completed"
                && event
                    .payload
                    .get("item")
                    .and_then(|item| item.get("detail"))
                    .and_then(Value::as_str)
                    == Some("keep the accepted steer")
        });
        if steered && completed {
            break;
        }
        if Instant::now() >= deadline {
            bail!("detached steer receipts were not persisted after caller cancellation");
        }
        tokio::task::yield_now().await;
    }

    let persisted_turn = manager.store.load_turn(&turn.id)?;
    assert_eq!(persisted_turn.steer_count, 1);
    let items = manager.store.list_items_for_turn(&turn.id)?;
    let steer_item = items
        .iter()
        .find(|item| item.detail.as_deref() == Some("keep the accepted steer"))
        .context("accepted steer item must remain durable")?;
    assert!(persisted_turn.item_ids.contains(&steer_item.id));

    tx_event
        .send(EngineEvent::MessageStarted { index: 0 })
        .await?;
    tx_event
        .send(EngineEvent::MessageDelta {
            index: 0,
            content: "accepted steer completed".to_string(),
        })
        .await?;
    tx_event
        .send(EngineEvent::MessageComplete { index: 0 })
        .await?;
    tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    let terminal = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Completed);
    Ok(())
}

#[tokio::test]
async fn steer_dropped_by_the_engine_is_reported_as_undelivered() -> Result<()> {
    // #6276: the channel accepting the text is not the model seeing it. An
    // engine that takes a steer and never commits it — the turn moved on, was
    // interrupted, or failed — must not produce a delivery receipt.
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    let mut rx_steer = harness.rx_steer;
    let tx_event = harness.tx_event;

    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "initial".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));

    // The engine receives the steer and drops it without committing, exactly
    // as `next_turn_steer` does for a turn that has already moved on.
    let drop_driver = tokio::spawn(async move {
        let steer = rx_steer.recv().await;
        drop(steer);
        rx_steer
    });

    let error = manager
        .steer_turn(
            &thread.id,
            &turn.id,
            SteerTurnRequest {
                prompt: "never reaches the model".to_string(),
            },
        )
        .await
        .expect_err("a dropped steer must not report delivery");
    assert!(
        error.to_string().contains("moved on before the steer"),
        "drop error must name the cause, got: {error}"
    );
    let _rx_steer = drop_driver.await?;

    // The durable record agrees: canceled item, no delivery count.
    let items = manager.store.list_items_for_turn(&turn.id)?;
    let steer_item = items
        .iter()
        .find(|item| item.detail.as_deref() == Some("never reaches the model"))
        .context("the attempted steer must stay on the record")?;
    assert_eq!(steer_item.status, TurnItemLifecycleStatus::Canceled);
    assert_eq!(manager.store.load_turn(&turn.id)?.steer_count, 0);

    let events = manager.events_since(&thread.id, None)?;
    let dropped = events
        .iter()
        .find(|ev| ev.event == "turn.steer_dropped")
        .context("a dropped steer must be announced so a client can requeue")?;
    assert_eq!(
        dropped.payload.get("input").and_then(Value::as_str),
        Some("never reaches the model")
    );
    assert!(
        !events.iter().any(|ev| ev.event == "turn.steered"),
        "a dropped steer must not emit turn.steered"
    );
    assert!(
        !events.iter().any(|ev| {
            ev.event == "item.completed"
                && ev
                    .payload
                    .get("item")
                    .and_then(|item| item.get("detail"))
                    .and_then(Value::as_str)
                    == Some("never reaches the model")
        }),
        "a dropped steer must not emit item.completed"
    );

    tx_event
        .send(EngineEvent::MessageStarted { index: 0 })
        .await?;
    tx_event
        .send(EngineEvent::MessageDelta {
            index: 0,
            content: "unsteered response".to_string(),
        })
        .await?;
    tx_event
        .send(EngineEvent::MessageComplete { index: 0 })
        .await?;
    tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    let terminal = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Completed);
    Ok(())
}

#[tokio::test]
async fn steer_rejects_a_terminal_durable_turn_without_dispatch_or_item() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    let mut rx_steer = harness.rx_steer;
    let tx_event = harness.tx_event;

    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "initial".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    let original_item_ids = turn.item_ids.clone();
    {
        let _turn_mutation = manager.store.turn_mutation.lock();
        let mut terminal = manager.store.load_turn(&turn.id)?;
        terminal.status = RuntimeTurnStatus::Completed;
        terminal.ended_at = Some(Utc::now());
        manager.store.save_turn(&terminal)?;
    }

    let error = manager
        .steer_turn(
            &thread.id,
            &turn.id,
            SteerTurnRequest {
                prompt: "must be rejected".to_string(),
            },
        )
        .await
        .expect_err("terminal turn must reject steering");
    assert!(error.to_string().contains("no longer in progress"));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), rx_steer.recv())
            .await
            .is_err(),
        "rejected terminal steer must not reach the engine"
    );
    let persisted = manager.store.load_turn(&turn.id)?;
    assert_eq!(persisted.steer_count, 0);
    assert_eq!(persisted.item_ids, original_item_ids);
    assert_eq!(manager.store.list_items_for_turn(&turn.id)?.len(), 1);

    // Restore the synthetic record and let the real monitor settle normally.
    {
        let _turn_mutation = manager.store.turn_mutation.lock();
        let mut active = manager.store.load_turn(&turn.id)?;
        active.status = RuntimeTurnStatus::InProgress;
        active.ended_at = None;
        manager.store.save_turn(&active)?;
    }
    tx_event
        .send(EngineEvent::MessageStarted { index: 0 })
        .await?;
    tx_event
        .send(EngineEvent::MessageDelta {
            index: 0,
            content: "terminal rejection test completed".to_string(),
        })
        .await?;
    tx_event
        .send(EngineEvent::MessageComplete { index: 0 })
        .await?;
    tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    let terminal = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Completed);
    Ok(())
}

#[tokio::test]
async fn concurrent_event_publication_keeps_live_and_durable_sequence_order() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut live_rx = manager.subscribe_events();

    let mut emitters = Vec::new();
    for index in 0..24_u64 {
        let emitter = manager.clone();
        let thread_id = thread.id.clone();
        emitters.push(tokio::spawn(async move {
            emitter
                .emit_event(
                    &thread_id,
                    None,
                    None,
                    "test.concurrent",
                    json!({ "index": index }),
                )
                .await
        }));
    }
    for emitter in emitters {
        emitter.await??;
    }

    let mut live = Vec::new();
    for _ in 0..24 {
        live.push(tokio::time::timeout(Duration::from_secs(2), live_rx.recv()).await??);
    }
    assert!(live.windows(2).all(|pair| pair[0].seq < pair[1].seq));

    let durable: Vec<_> = manager
        .events_since(&thread.id, None)?
        .into_iter()
        .filter(|event| event.event == "test.concurrent")
        .collect();
    assert_eq!(durable.len(), 24);
    assert_eq!(
        live.iter()
            .map(|event| (event.seq, event.payload.clone()))
            .collect::<Vec<_>>(),
        durable
            .iter()
            .map(|event| (event.seq, event.payload.clone()))
            .collect::<Vec<_>>(),
        "broadcast order must exactly match append order"
    );
    Ok(())
}

#[tokio::test]
async fn closed_engine_event_stream_fails_turn_items_and_evicts_engine() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;

    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "engine stream will close".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    drop(tx_event);

    let terminal = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Failed);
    let terminal_error = terminal.error.as_deref().unwrap_or_default();
    assert!(
        terminal.error.as_deref().is_some_and(|error| {
            error.contains("Failed to monitor") || error.contains("without producing any output")
        }),
        "unexpected terminal error: {terminal_error:?}"
    );
    assert!(
        manager
            .store
            .list_items_for_turn(&turn.id)?
            .iter()
            .all(|item| !matches!(
                item.status,
                TurnItemLifecycleStatus::Queued | TurnItemLifecycleStatus::InProgress
            ))
    );
    let deadline = Instant::now() + TURN_SETTLEMENT_DEADLOCK_TIMEOUT;
    loop {
        if !manager.active.lock().await.engines.contains_key(&thread.id) {
            break;
        }
        if Instant::now() >= deadline {
            bail!("failed engine was not evicted");
        }
        tokio::task::yield_now().await;
    }
    assert!(matches!(rx_op.recv().await, Some(Op::Shutdown)));
    Ok(())
}

#[tokio::test]
async fn failed_turn_cancels_pending_user_input_and_clears_snapshot() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "input required, then the engine stream closes".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    harness
        .tx_event
        .send(EngineEvent::UserInputRequired {
            id: "input_failed_turn".to_string(),
            request: crate::tools::user_input::UserInputRequest {
                require_typed_answers: false,
                questions: vec![crate::tools::user_input::UserInputQuestion {
                    detail: None,
                    intent: None,
                    header: "Continue".to_string(),
                    id: "continue".to_string(),
                    question: "Continue?".to_string(),
                    options: vec![crate::tools::user_input::UserInputOption {
                        label: "Yes".to_string(),
                        description: "Continue now".to_string(),
                    }],
                    allow_free_text: false,
                    multi_select: false,
                }],
            },
        })
        .await?;

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if !manager
            .get_thread_detail(&thread.id)
            .await?
            .pending_user_inputs
            .is_empty()
        {
            break;
        }
        if Instant::now() >= deadline {
            bail!("pending user input did not reach the canonical snapshot");
        }
        sleep(Duration::from_millis(20)).await;
    }

    // Fail the turn mid-prompt: closing the event stream settles the turn as
    // failed through the monitor-failure path.
    harness.close_event_stream();

    let canceled = tokio::time::timeout(
        Duration::from_secs(2),
        harness.recv_user_input_cancellation(),
    )
    .await
    .expect("failure-path user-input cancellation timed out");
    assert_eq!(canceled.as_deref(), Some("input_failed_turn"));

    let terminal = wait_for_terminal_turn(&manager, &turn.id).await?;
    assert_eq!(terminal.status, RuntimeTurnStatus::Failed);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let detail = manager.get_thread_detail(&thread.id).await?;
        if detail.pending_user_inputs.is_empty()
            && manager.events_since(&thread.id, None)?.iter().any(|event| {
                event.event == "user_input.canceled"
                    && event.turn_id.as_deref() == Some(turn.id.as_str())
                    && event.payload.get("input_id").and_then(Value::as_str)
                        == Some("input_failed_turn")
                    && event.payload.get("terminal").and_then(Value::as_bool) == Some(true)
            })
        {
            break;
        }
        if Instant::now() >= deadline {
            bail!("failed turn left a stale pending user input in the snapshot");
        }
        sleep(Duration::from_millis(20)).await;
    }
    Ok(())
}

fn answer_session_snapshot(
    tx: &Arc<
        std::sync::Mutex<Option<tokio::sync::oneshot::Sender<crate::core::ops::SessionSnapshot>>>,
    >,
    text: &str,
) {
    let messages: Vec<Message> = serde_json::from_value(json!([
        {"role": "user", "content": [{"type": "text", "text": text}]}
    ]))
    .expect("snapshot message");
    if let Some(tx) = tx.lock().expect("snapshot sender").take() {
        let _ = tx.send(crate::core::ops::SessionSnapshot {
            session_id: String::new(),
            messages,
            total_tokens: 0,
            model: String::new(),
            model_provider: String::new(),
            model_provider_id: None,
            workspace: PathBuf::new(),
            system_prompt: None,
            instruction_assembly: None,
            mode: String::new(),
        });
    }
}

#[tokio::test]
async fn compaction_lifecycle_emits_item_events_with_compaction_counts() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;

    let harness = install_mock_engine(&manager, &thread.id).await;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    let checkpoint_thread = thread.id.clone();
    let checkpoint_workspace = thread.workspace.clone();
    tokio::spawn(async move {
        let mut op_count = 0usize;
        let mut snapshots = 0usize;
        while let Some(op) = rx_op.recv().await {
            match op {
                Op::GetSessionSnapshot { tx } => {
                    snapshots = snapshots.saturating_add(1);
                    answer_session_snapshot(
                        &tx,
                        if snapshots == 1 {
                            "auto settled"
                        } else {
                            "manual settled"
                        },
                    );
                }
                Op::SendMessage(TurnSpec { .. }) => {
                    op_count = op_count.saturating_add(1);
                    let _ = tx_event
                        .send(EngineEvent::TurnStarted {
                            turn_id: "engine_turn_auto".to_string(),
                            created_at: chrono::Utc::now(),
                            route: None,
                            submission_id: None,
                        })
                        .await;
                    for (agent_id, withdrawn, accepted) in [
                        (Some("child".to_string()), false, false),
                        (None, true, false),
                        (None, false, true),
                        (None, false, true),
                    ] {
                        let messages: Vec<Message> = serde_json::from_value(json!([
                            {"role":"user","content":[{"type":"text","text":"auto checkpoint prefix"}]}
                        ])).unwrap();
                        let snapshot = Arc::new(crate::core::ops::SessionSnapshot {
                            session_id: checkpoint_thread.clone(),
                            messages: messages.clone(),
                            total_tokens: 0,
                            model: String::new(),
                            model_provider: String::new(),
                            model_provider_id: None,
                            workspace: checkpoint_workspace.clone(),
                            system_prompt: None,
                            instruction_assembly: None,
                            mode: "agent".into(),
                        });
                        let (reply, received) = oneshot::channel();
                        let withdraw = CancellationToken::new();
                        if withdrawn {
                            withdraw.cancel();
                        }
                        let request = crate::core::events::SessionCheckpointRequest::new(
                            crate::core::events::SessionCheckpointCaller {
                                session_id: checkpoint_thread.clone(), agent_id, turn_id: "engine_turn_auto".into(),
                                call_id: "mock_checkpoint_call".into(), workspace: checkpoint_workspace.clone(),
                            }, snapshot, session_messages_sha256(&messages).unwrap(),
                            crate::core::events::SessionCheckpointProjection::RuntimeReceiptRedacted, withdraw, reply,
                        );
                        tx_event
                            .send(EngineEvent::SessionUpdated {
                                checkpoint: Some(request),
                                session_id: checkpoint_thread.clone(),
                                messages: Arc::new(messages),
                                system_prompt: None,
                                instruction_assembly: None,
                                model: String::new(),
                                workspace: checkpoint_workspace.clone(),
                            })
                            .await
                            .unwrap();
                        let outcome = tokio::time::timeout(Duration::from_secs(5), received)
                            .await
                            .expect("checkpoint must not call back into the Engine mailbox")
                            .expect("checkpoint writer must settle its sender");
                        if accepted {
                            assert!(matches!(outcome, Ok(true)));
                        } else {
                            assert!(outcome.is_err());
                        }
                    }
                    let _ = tx_event
                        .send(EngineEvent::CompactionStarted {
                            id: "auto_compact_1".to_string(),
                            auto: true,
                            message: "auto compact begin".to_string(),
                        })
                        .await;
                    let _ = tx_event
                        .send(EngineEvent::CompactionCompleted {
                            id: "auto_compact_1".to_string(),
                            auto: true,
                            message: "auto compact done".to_string(),
                            messages_before: Some(7),
                            messages_after: Some(3),
                            summary_prompt: None,
                            post_input_tokens: None,
                        })
                        .await;
                    let _ = tx_event
                        .send(EngineEvent::TurnComplete {
                            turn_id: None,
                            submission_id: None,
                            compaction_id: None,
                            usage: Usage {
                                input_tokens: 3,
                                output_tokens: 3,
                                ..Usage::default()
                            },
                            parent_route_usage: Usage {
                                input_tokens: 3,
                                output_tokens: 3,
                                ..Usage::default()
                            },
                            routed_usage_dropped_records: 0,
                            status: TurnOutcomeStatus::Completed,
                            error: None,
                            tool_catalog: None,
                            base_url: None,
                        })
                        .await;
                }
                Op::CompactContext { .. } => {
                    op_count = op_count.saturating_add(1);
                    let _ = tx_event
                        .send(EngineEvent::CompactionStarted {
                            id: "manual_compact_1".to_string(),
                            auto: false,
                            message: "manual compact begin".to_string(),
                        })
                        .await;
                    let _ = tx_event
                        .send(EngineEvent::CompactionCompleted {
                            id: "manual_compact_1".to_string(),
                            auto: false,
                            message: "manual compact done".to_string(),
                            messages_before: Some(5),
                            messages_after: Some(2),
                            summary_prompt: Some(
                                "## 📋 Conversation Summary (Auto-Generated)\n\nkey facts."
                                    .to_string(),
                            ),
                            post_input_tokens: None,
                        })
                        .await;
                    let _ = tx_event
                        .send(EngineEvent::TurnComplete {
                            turn_id: None,
                            submission_id: None,
                            compaction_id: None,
                            usage: Usage {
                                input_tokens: 1,
                                output_tokens: 1,
                                ..Usage::default()
                            },
                            parent_route_usage: Usage {
                                input_tokens: 1,
                                output_tokens: 1,
                                ..Usage::default()
                            },
                            routed_usage_dropped_records: 0,
                            status: TurnOutcomeStatus::Completed,
                            error: None,
                            tool_catalog: None,
                            base_url: None,
                        })
                        .await;
                }
                _ => {}
            }
            if op_count >= 2 && snapshots >= 2 {
                break;
            }
        }
    });

    let auto_turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "trigger auto".to_string(),
                input_summary: None,
                model: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                ..Default::default()
            },
        )
        .await?;
    let auto_turn = wait_for_terminal_turn(&manager, &auto_turn.id).await?;
    assert_eq!(auto_turn.status, RuntimeTurnStatus::Completed);

    let manual_turn = manager
        .compact_thread(
            &thread.id,
            CompactThreadRequest {
                reason: Some("manual request".to_string()),
            },
        )
        .await?;
    let manual_turn = wait_for_terminal_turn(&manager, &manual_turn.id).await?;
    assert_eq!(manual_turn.status, RuntimeTurnStatus::Completed);

    let events = manager.events_since(&thread.id, None)?;
    assert!(events.iter().any(|ev| {
        ev.event == "item.started"
            && ev
                .payload
                .get("item")
                .and_then(|item| item.get("kind"))
                .and_then(Value::as_str)
                == Some("context_compaction")
            && ev.payload.get("auto").and_then(Value::as_bool) == Some(true)
    }));
    assert!(events.iter().any(|ev| {
        ev.event == "item.completed"
            && ev
                .payload
                .get("item")
                .and_then(|item| item.get("kind"))
                .and_then(Value::as_str)
                == Some("context_compaction")
            && ev.payload.get("auto").and_then(Value::as_bool) == Some(true)
            && ev.payload.get("messages_before").and_then(Value::as_u64) == Some(7)
            && ev.payload.get("messages_after").and_then(Value::as_u64) == Some(3)
    }));
    assert!(events.iter().any(|ev| {
        ev.event == "item.completed"
            && ev
                .payload
                .get("item")
                .and_then(|item| item.get("kind"))
                .and_then(Value::as_str)
                == Some("context_compaction")
            && ev.payload.get("auto").and_then(Value::as_bool) == Some(false)
            && ev.payload.get("messages_before").and_then(Value::as_u64) == Some(5)
            && ev.payload.get("messages_after").and_then(Value::as_u64) == Some(2)
    }));

    // Each compacting turn stores the history the engine settled on; a
    // reload restores the newest one instead of replaying the turns it
    // replaced, and the thread record carries no summary of its own.
    assert!(
        manager
            .store
            .list_turns_for_thread(&thread.id)?
            .iter()
            .any(|turn| turn.prefix_checkpoint.is_some())
    );
    let record = manager.get_thread(&thread.id).await?;
    assert_eq!(record.system_prompt, None);
    let restored = manager.restore_thread_messages(&record)?;
    let expected: Vec<Message> = serde_json::from_value(json!([
        {"role": "user", "content": [{"type": "text", "text": "manual settled"}]}
    ]))?;
    assert_eq!(restored, expected);
    Ok(())
}

#[test]
fn summarize_text_truncates() {
    let out = summarize_text("abcdefghijklmnopqrstuvwxyz", 10);
    assert_eq!(out, "abcdefg...");
}

#[test]
fn opening_manager_recovers_stale_queued_and_in_progress_work() -> Result<()> {
    let data_dir = test_runtime_dir();
    let manager = test_manager(data_dir.clone())?;
    let started_at = Utc::now() - chrono::Duration::seconds(5);
    let created_at = started_at - chrono::Duration::seconds(1);

    let thread = ThreadRecord {
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: "thr_restart".to_string(),
        created_at,
        updated_at: created_at,
        model: DEFAULT_TEXT_MODEL.to_string(),
        model_provider: None,
        model_provider_id: None,
        reasoning_effort: None,
        allowed_tools: None,
        workspace: PathBuf::from("."),
        mode: "agent".to_string(),
        permission_posture: None,
        allow_shell: false,
        trust_mode: false,
        auto_approve: false,
        latest_turn_id: Some("turn_in_progress".to_string()),
        latest_response_bookmark: None,
        archived: false,
        system_prompt: None,
        task_id: None,
        parent_thread_id: None,
        title: None,
        session_id: None,
        saved_session_checkpoint: None,
        last_read: None,
    };
    manager.store.save_thread(&thread)?;

    let completed_item = TurnItemRecord {
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: "item_completed".to_string(),
        turn_id: "turn_in_progress".to_string(),
        kind: TurnItemKind::Status,
        status: TurnItemLifecycleStatus::Completed,
        summary: "done".to_string(),
        detail: None,
        metadata: None,
        artifact_refs: Vec::new(),
        artifacts: Vec::new(),
        started_at: Some(started_at),
        ended_at: Some(started_at + chrono::Duration::seconds(1)),
    };
    let in_progress_item = TurnItemRecord {
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: "item_in_progress".to_string(),
        turn_id: "turn_in_progress".to_string(),
        kind: TurnItemKind::ToolCall,
        status: TurnItemLifecycleStatus::InProgress,
        summary: "running".to_string(),
        detail: None,
        metadata: None,
        artifact_refs: Vec::new(),
        artifacts: Vec::new(),
        started_at: Some(started_at),
        ended_at: None,
    };
    let queued_item = TurnItemRecord {
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: "item_queued".to_string(),
        turn_id: "turn_queued".to_string(),
        kind: TurnItemKind::ToolCall,
        status: TurnItemLifecycleStatus::Queued,
        summary: "queued".to_string(),
        detail: None,
        metadata: None,
        artifact_refs: Vec::new(),
        artifacts: Vec::new(),
        started_at: None,
        ended_at: None,
    };
    manager.store.save_item(&completed_item)?;
    manager.store.save_item(&in_progress_item)?;
    manager.store.save_item(&queued_item)?;

    manager.store.save_turn(&TurnRecord {
        prefix_checkpoint: None,
        decision_receipts: Vec::new(),
        max_output_tokens: None,
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: "turn_in_progress".to_string(),
        thread_id: thread.id.clone(),
        status: RuntimeTurnStatus::InProgress,
        input_summary: "hello".to_string(),
        created_at,
        started_at: Some(started_at),
        ended_at: None,
        duration_ms: None,
        usage: None,
        routing_settlement: false,
        effective_route_usage: None,
        permission_posture: None,
        mode: None,
        effective_provider: None,
        effective_provider_id: None,
        effective_openrouter_vendor: None,
        effective_billing_surface: None,
        effective_endpoint_fingerprint: None,
        effective_provider_live_pricing: None,
        effective_billing_mode: None,
        effective_dispatched_at: None,
        effective_model: None,
        routed_usage: Vec::new(),
        routed_usage_drop_records: Vec::new(),
        routed_usage_source_ids: Vec::new(),
        routed_usage_dropped_records: 0,
        model_request_diagnostics: None,
        operation_activity: None,
        error: None,
        item_ids: vec![completed_item.id.clone(), in_progress_item.id.clone()],
        steer_count: 0,
        agent_mail_message_id: None,
        artifacts: Vec::new(),
        workspace: None,
        workspace_snapshots: Vec::new(),
    })?;
    manager.store.save_turn(&TurnRecord {
        prefix_checkpoint: None,
        decision_receipts: Vec::new(),
        max_output_tokens: None,
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: "turn_queued".to_string(),
        thread_id: thread.id.clone(),
        status: RuntimeTurnStatus::Queued,
        input_summary: "later".to_string(),
        created_at,
        started_at: None,
        ended_at: None,
        duration_ms: None,
        usage: None,
        routing_settlement: false,
        effective_route_usage: None,
        permission_posture: None,
        mode: None,
        effective_provider: None,
        effective_provider_id: None,
        effective_openrouter_vendor: None,
        effective_billing_surface: None,
        effective_endpoint_fingerprint: None,
        effective_provider_live_pricing: None,
        effective_billing_mode: None,
        effective_dispatched_at: None,
        effective_model: None,
        routed_usage: Vec::new(),
        routed_usage_drop_records: Vec::new(),
        routed_usage_source_ids: Vec::new(),
        routed_usage_dropped_records: 0,
        model_request_diagnostics: None,
        operation_activity: None,
        error: None,
        item_ids: vec![queued_item.id.clone()],
        steer_count: 0,
        agent_mail_message_id: None,
        artifacts: Vec::new(),
        workspace: None,
        workspace_snapshots: Vec::new(),
    })?;
    drop(manager);

    let recovered = test_manager(data_dir)?;

    let recovered_thread = recovered.store.load_thread(&thread.id)?;
    assert!(recovered_thread.updated_at >= thread.updated_at);

    let recovered_in_progress_turn = recovered.store.load_turn("turn_in_progress")?;
    assert_eq!(
        recovered_in_progress_turn.status,
        RuntimeTurnStatus::Interrupted
    );
    assert_eq!(
        recovered_in_progress_turn.error.as_deref(),
        Some(RUNTIME_RESTART_REASON)
    );
    assert!(recovered_in_progress_turn.ended_at.is_some());
    assert!(
        recovered_in_progress_turn
            .duration_ms
            .is_some_and(|duration| duration >= 5_000)
    );

    let recovered_queued_turn = recovered.store.load_turn("turn_queued")?;
    assert_eq!(recovered_queued_turn.status, RuntimeTurnStatus::Interrupted);
    assert_eq!(
        recovered_queued_turn.error.as_deref(),
        Some(RUNTIME_RESTART_REASON)
    );
    assert!(recovered_queued_turn.ended_at.is_some());
    assert_eq!(recovered_queued_turn.duration_ms, None);

    assert_eq!(
        recovered.store.load_item(&completed_item.id)?.status,
        TurnItemLifecycleStatus::Completed
    );
    let recovered_in_progress_item = recovered.store.load_item(&in_progress_item.id)?;
    assert_eq!(
        recovered_in_progress_item.status,
        TurnItemLifecycleStatus::Interrupted
    );
    assert!(recovered_in_progress_item.ended_at.is_some());

    let recovered_queued_item = recovered.store.load_item(&queued_item.id)?;
    assert_eq!(
        recovered_queued_item.status,
        TurnItemLifecycleStatus::Interrupted
    );
    assert!(recovered_queued_item.ended_at.is_some());

    Ok(())
}

#[test]
fn parse_mode_scenario() {
    // Scenario consolidation of: parse_mode_defaults_to_agent, parse_mode_opt_resolves_explicit_tokens_and_aliases, parse_mode_opt_rejects_prompt_fragments, parse_mode_wrapper_defaults_and_resolves_numeric_aliases
    // from parse_mode_defaults_to_agent
    {
        assert_eq!(parse_mode("unknown"), AppMode::Agent);
        assert_eq!(parse_mode("plan"), AppMode::Plan);
    }
    // from parse_mode_opt_resolves_explicit_tokens_and_aliases
    {
        assert_eq!(parse_mode_opt("agent"), Some(AppMode::Agent));
        assert_eq!(parse_mode_opt("1"), Some(AppMode::Agent));
        assert_eq!(parse_mode_opt("plan"), Some(AppMode::Plan));
        assert_eq!(parse_mode_opt("2"), Some(AppMode::Plan));
        assert_eq!(parse_mode_opt("auto"), Some(AppMode::Agent));
        assert_eq!(parse_mode_opt("operate"), Some(AppMode::Operate));
        assert_eq!(parse_mode_opt("3"), Some(AppMode::Operate));
        // Legacy YOLO spellings resolve to Act; the posture travels separately.
        assert_eq!(parse_mode_opt("yolo"), Some(AppMode::Agent));
        assert_eq!(parse_mode_opt("4"), Some(AppMode::Agent));
        assert_eq!(parse_mode_opt(" PLAN "), Some(AppMode::Plan));
    }
    // from parse_mode_opt_rejects_prompt_fragments
    {
        for input in [
            "plan a trip to Tokyo",
            "switch the agent on",
            "enter yolo mode",
            "agent of chaos",
            "mode",
        ] {
            assert_eq!(parse_mode_opt(input), None);
        }
    }
    // from parse_mode_wrapper_defaults_and_resolves_numeric_aliases
    {
        assert_eq!(parse_mode("plan a trip to Tokyo"), AppMode::Agent);
        assert_eq!(parse_mode("auto"), AppMode::Agent);
        assert_eq!(parse_mode("1"), AppMode::Agent);
        assert_eq!(parse_mode("2"), AppMode::Plan);
        assert_eq!(parse_mode("3"), AppMode::Operate);
        assert_eq!(parse_mode("4"), AppMode::Agent);
    }
}

#[test]
fn mode_only_override_preserves_legacy_full_access_posture() -> Result<()> {
    let mut thread = sample_thread("thr_legacy_full_access");
    thread.permission_posture = None;
    thread.auto_approve = true;

    let policy = runtime_policy_with_overrides(&thread, Some("plan"), None, None)?;
    assert_eq!(policy.mode, AppMode::Plan);
    assert_eq!(policy.permission_wire(), "full_access");

    let ask = runtime_policy_with_overrides(&thread, Some("act"), None, Some(false))?;
    assert_eq!(ask.permission_wire(), "ask");
    Ok(())
}

/// Helper for the `fork_at_user_message` tests: write a sequence of
/// (user, assistant) turns under the given thread id. Each turn gets
/// one UserMessage item carrying `user_text` in `detail` plus one
/// AgentMessage item. Turn `created_at` is monotonically increasing
/// so the chronological sort in `list_turns_for_thread` is stable.
fn seed_turns_with_user_messages(
    manager: &RuntimeThreadManager,
    thread_id: &str,
    user_texts: &[&str],
) -> Result<Vec<String>> {
    let mut turn_ids = Vec::new();
    let base = Utc::now();
    for (offset, text) in user_texts.iter().enumerate() {
        let created_at = base + chrono::Duration::milliseconds(offset as i64);
        let turn_id = format!("turn_test_{offset}");
        let user_item_id = format!("item_user_{offset}");
        let asst_item_id = format!("item_asst_{offset}");
        manager.store.save_item(&TurnItemRecord {
            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
            id: user_item_id.clone(),
            turn_id: turn_id.clone(),
            kind: TurnItemKind::UserMessage,
            status: TurnItemLifecycleStatus::Completed,
            summary: (*text).to_string(),
            detail: Some((*text).to_string()),
            metadata: None,
            artifact_refs: Vec::new(),
            artifacts: Vec::new(),
            started_at: Some(created_at),
            ended_at: Some(created_at),
        })?;
        manager.store.save_item(&TurnItemRecord {
            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
            id: asst_item_id.clone(),
            turn_id: turn_id.clone(),
            kind: TurnItemKind::AgentMessage,
            status: TurnItemLifecycleStatus::Completed,
            summary: format!("reply {offset}"),
            detail: Some(format!("reply {offset}")),
            metadata: None,
            artifact_refs: Vec::new(),
            artifacts: Vec::new(),
            started_at: Some(created_at),
            ended_at: Some(created_at),
        })?;
        manager.store.save_turn(&TurnRecord {
            prefix_checkpoint: None,
            decision_receipts: Vec::new(),
            max_output_tokens: None,
            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
            id: turn_id.clone(),
            thread_id: thread_id.to_string(),
            status: RuntimeTurnStatus::Completed,
            input_summary: (*text).to_string(),
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at),
            duration_ms: Some(0),
            usage: None,
            routing_settlement: false,
            effective_route_usage: None,
            permission_posture: None,
            mode: None,
            effective_provider: None,
            effective_provider_id: None,
            effective_openrouter_vendor: None,
            effective_billing_surface: None,
            effective_endpoint_fingerprint: None,
            effective_provider_live_pricing: None,
            effective_billing_mode: None,
            effective_dispatched_at: None,
            effective_model: None,
            routed_usage: Vec::new(),
            routed_usage_drop_records: Vec::new(),
            routed_usage_source_ids: Vec::new(),
            routed_usage_dropped_records: 0,
            model_request_diagnostics: None,
            operation_activity: None,
            error: None,
            item_ids: vec![user_item_id, asst_item_id],
            steer_count: 0,
            agent_mail_message_id: None,
            artifacts: Vec::new(),
            workspace: None,
            workspace_snapshots: Vec::new(),
        })?;
        turn_ids.push(turn_id);
    }
    Ok(turn_ids)
}

#[tokio::test]
async fn fork_at_user_message_drops_tail_and_returns_user_text() -> Result<()> {
    // Seed three completed user/assistant turns. Backtracking with
    // depth=0 should drop only the most recent turn ("third") and
    // hand back its original text so the caller can refill the
    // composer.
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;
    seed_turns_with_user_messages(&manager, &thread.id, &["first", "second", "third"])?;

    let (forked, original_text, _, _) = manager.fork_at_user_message(&thread.id, 0).await?;
    assert_eq!(original_text.as_deref(), Some("third"));
    assert_ne!(forked.id, thread.id);

    let forked_turns = manager.store.list_turns_for_thread(&forked.id)?;
    assert_eq!(
        forked_turns.len(),
        2,
        "depth=0 should drop the most recent turn"
    );
    let summaries: Vec<&str> = forked_turns
        .iter()
        .map(|t| t.input_summary.as_str())
        .collect();
    assert_eq!(summaries, vec!["first", "second"]);
    Ok(())
}

#[tokio::test]
async fn fork_at_user_message_depth_one_drops_two_turns() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;
    seed_turns_with_user_messages(&manager, &thread.id, &["a", "b", "c", "d"])?;

    let (forked, original_text, _, _) = manager.fork_at_user_message(&thread.id, 1).await?;
    assert_eq!(original_text.as_deref(), Some("c"));
    let forked_turns = manager.store.list_turns_for_thread(&forked.id)?;
    let summaries: Vec<&str> = forked_turns
        .iter()
        .map(|t| t.input_summary.as_str())
        .collect();
    assert_eq!(summaries, vec!["a", "b"]);
    Ok(())
}

#[tokio::test]
async fn fork_at_user_turn_leaves_a_running_turn_alone() -> Result<()> {
    // A conversation can be branched while it is working: a person watching a
    // turn go the wrong way should not have to stop it (and lose what it has
    // already done) before taking a different path. Two things must hold —
    // the fork is cut at the named turn, and the running turn is untouched and
    // still running in the thread it belongs to.
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;
    let turn_ids =
        seed_turns_with_user_messages(&manager, &thread.id, &["first", "second", "third"])?;

    // The newest turn is still in flight: its record exists, its answer does not.
    let mut running = manager
        .store
        .list_turns_for_thread(&thread.id)?
        .into_iter()
        .find(|turn| turn.id == turn_ids[2])
        .context("seeded third turn")?;
    running.status = RuntimeTurnStatus::InProgress;
    running.ended_at = None;
    running.usage = None;
    let running_item_ids = running.item_ids.clone();
    manager.store.save_turn(&running)?;
    let source_before = manager.get_thread(&thread.id).await?;

    let (forked, original_text, _, _) = manager.fork_at_user_turn(&thread.id, &turn_ids[1]).await?;

    // The branch keeps the turn it names, and hands back the question the
    // running turn is answering — that is what a person wants next.
    assert_eq!(original_text.as_deref(), Some("third"));
    let forked_turns = manager.store.list_turns_for_thread(&forked.id)?;
    let summaries: Vec<&str> = forked_turns
        .iter()
        .map(|turn| turn.input_summary.as_str())
        .collect();
    assert_eq!(summaries, vec!["first", "second"]);

    // The source thread is exactly as it was, including the turn in flight:
    // forking never interrupts one, it only copies a prefix.
    let source_turns = manager.store.list_turns_for_thread(&thread.id)?;
    assert_eq!(source_turns.len(), 3, "the source keeps every turn");
    let still_running = source_turns
        .iter()
        .find(|turn| turn.id == turn_ids[2])
        .context("the running turn is still there")?;
    assert_eq!(still_running.status, RuntimeTurnStatus::InProgress);
    assert_eq!(still_running.item_ids, running_item_ids);
    assert_eq!(
        manager.get_thread(&thread.id).await?,
        source_before,
        "the source thread's own record is not rewritten by a fork"
    );
    Ok(())
}

#[tokio::test]
async fn fork_at_user_turn_receipt_skips_a_compaction_turn_after_the_anchor() -> Result<()> {
    // A manual `/compact` is a turn of its own whose only item is the
    // compaction: no prompt. Branching at the turn before it must still drop
    // it (the fork keeps only the anchor and what precedes it), but the
    // receipt belongs to the next *user* turn — the question that was asked
    // next — not to the prompt-less compaction that happens to sit between.
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;
    let turn_ids =
        seed_turns_with_user_messages(&manager, &thread.id, &["first", "second", "third"])?;
    let seeded = manager.store.list_turns_for_thread(&thread.id)?;
    let anchor = seeded
        .iter()
        .find(|turn| turn.id == turn_ids[1])
        .context("seeded second turn")?;

    // The compaction turn lands between the anchor and the next user turn.
    let compaction_at = anchor.created_at + chrono::Duration::microseconds(500);
    let compaction_item_id = "item_compaction_between".to_string();
    manager.store.save_item(&TurnItemRecord {
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: compaction_item_id.clone(),
        turn_id: "turn_compaction_between".to_string(),
        kind: TurnItemKind::ContextCompaction,
        status: TurnItemLifecycleStatus::Completed,
        summary: "Context compacted".to_string(),
        detail: Some("summary of first and second".to_string()),
        metadata: None,
        artifact_refs: Vec::new(),
        artifacts: Vec::new(),
        started_at: Some(compaction_at),
        ended_at: Some(compaction_at),
    })?;
    let mut compaction_turn = anchor.clone();
    compaction_turn.id = "turn_compaction_between".to_string();
    compaction_turn.input_summary = "Context compacted".to_string();
    compaction_turn.created_at = compaction_at;
    compaction_turn.started_at = Some(compaction_at);
    compaction_turn.ended_at = Some(compaction_at);
    compaction_turn.item_ids = vec![compaction_item_id];
    manager.store.save_turn(&compaction_turn)?;

    // The next user turn carries its own allowance, which travels with it.
    let mut next_user_turn = seeded
        .iter()
        .find(|turn| turn.id == turn_ids[2])
        .context("seeded third turn")?
        .clone();
    next_user_turn.max_output_tokens = std::num::NonZeroU32::new(4096);
    next_user_turn.schema_version = OUTPUT_LIMIT_RUNTIME_SCHEMA_VERSION;
    manager.store.save_turn(&next_user_turn)?;

    let order: Vec<String> = manager
        .store
        .list_turns_for_thread(&thread.id)?
        .into_iter()
        .map(|turn| turn.id)
        .collect();
    assert_eq!(
        order,
        vec![
            turn_ids[0].clone(),
            turn_ids[1].clone(),
            "turn_compaction_between".to_string(),
            turn_ids[2].clone(),
        ],
        "the compaction turn sits right after the anchor"
    );

    let (forked, original_text, original_images, max_output_tokens) =
        manager.fork_at_user_turn(&thread.id, &turn_ids[1]).await?;

    assert_eq!(original_text.as_deref(), Some("third"));
    assert!(original_images.is_empty());
    assert_eq!(max_output_tokens, std::num::NonZeroU32::new(4096));
    let summaries: Vec<String> = manager
        .store
        .list_turns_for_thread(&forked.id)?
        .into_iter()
        .map(|turn| turn.input_summary)
        .collect();
    assert_eq!(
        summaries,
        vec!["first".to_string(), "second".to_string()],
        "the fork keeps the anchor and drops the compaction after it"
    );
    Ok(())
}

#[tokio::test]
async fn fork_at_user_message_out_of_range_errors() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;
    seed_turns_with_user_messages(&manager, &thread.id, &["only"])?;

    let err = manager.fork_at_user_message(&thread.id, 5).await.err();
    assert!(err.is_some(), "depth past the end should bail out");
    Ok(())
}

#[tokio::test]
async fn fork_at_user_message_does_not_mutate_source() -> Result<()> {
    // The source thread must be untouched: turns still present, items
    // still present, latest_turn_id still pointing at the original
    // tail. Backtrack creates a sibling, never edits in place.
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;
    let turn_ids = seed_turns_with_user_messages(&manager, &thread.id, &["x", "y", "z"])?;

    let _ = manager.fork_at_user_message(&thread.id, 0).await?;

    let source_turns = manager.store.list_turns_for_thread(&thread.id)?;
    assert_eq!(
        source_turns.len(),
        3,
        "source thread must still hold every turn after fork"
    );
    for tid in &turn_ids {
        assert!(
            manager.store.load_turn(tid).is_ok(),
            "turn {tid} must remain on disk"
        );
    }
    Ok(())
}

/// Release acceptance: the full two-task Agent Mail matrix in one run, with
/// evidence written to disk. Run explicitly:
///   CODEWHALE_MAIL_ACCEPTANCE_DIR=/Volumes/VIXinSSD/CW/artifacts/... \
///     cargo test -p codewhale-tui --lib agent_mail_release_acceptance -- --ignored
#[tokio::test]
#[ignore = "release acceptance: run explicitly with CODEWHALE_MAIL_ACCEPTANCE_DIR"]
async fn agent_mail_release_acceptance_two_task_matrix() -> Result<()> {
    use codewhale_protocol::agent_mail::AGENT_MAIL_EVENT_READ;
    use std::time::Instant as StdInstant;

    let runtime_dir = test_runtime_dir();
    let workspace = runtime_dir.join("workspace-shared");
    let foreign_workspace = runtime_dir.join("workspace-foreign");
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(&foreign_workspace)?;
    let artifacts = std::env::var("CODEWHALE_MAIL_ACCEPTANCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| runtime_dir.join("acceptance-artifacts"));
    fs::create_dir_all(&artifacts)?;

    let manager = test_manager(runtime_dir.clone())?;
    let task_a = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace.clone()),
            task_id: Some("task_a".to_string()),
            ..CreateThreadRequest::default()
        })
        .await?;
    let task_b = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace),
            task_id: Some("task_b".to_string()),
            ..CreateThreadRequest::default()
        })
        .await?;
    let task_foreign = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(foreign_workspace),
            task_id: Some("task_foreign".to_string()),
            ..CreateThreadRequest::default()
        })
        .await?;
    let harness_b = install_mock_engine(&manager, &task_b.id).await;

    let credential_summary = "Handoff: the staging token OPENAI_API_KEY=[REDACTED-VALUE] \
        must rotate; scratch DB dump lives under /tmp/private/staging.sql. \
        Verify the deploy checklist before the next release cut.";

    // (1) Queue-only mail A -> B; B persists it without sampling (no turn).
    let m1 = manager
        .queue_agent_mail(agent_mail_request(
            "mail_accept_m1",
            &task_a.id,
            &task_b.id,
            "task_a",
            credential_summary,
            AgentMailDeliveryMode::QueueOnly,
        ))
        .await?;
    assert!(!m1.idempotent_replay);
    assert_eq!(m1.envelope.status, AgentMailStatus::Queued);
    let serialized = serde_json::to_string(&m1.envelope)?;
    assert!(!serialized.contains("sk-fixture-secret"), "{serialized}");
    assert!(!serialized.contains("/tmp/private"), "{serialized}");
    assert!(serialized.contains("redacted-credential"), "{serialized}");
    assert!(serialized.contains("redacted-path"), "{serialized}");
    assert!(
        manager.store.list_turns_for_thread(&task_b.id)?.is_empty(),
        "queue-only mail must not sample the destination"
    );

    // (2) Replaying the same envelope is idempotent — no duplicate event.
    let m1_replay = manager
        .queue_agent_mail(agent_mail_request(
            "mail_accept_m1",
            &task_a.id,
            &task_b.id,
            "task_a",
            credential_summary,
            AgentMailDeliveryMode::QueueOnly,
        ))
        .await?;
    assert!(m1_replay.idempotent_replay);
    assert_eq!(m1_replay.envelope, m1.envelope);
    assert_eq!(
        mail_event_count(&manager, &task_b.id, AGENT_MAIL_EVENT_QUEUED),
        1
    );

    // (3) Unauthorized cross-workspace delivery fails closed.
    let denied = manager
        .queue_agent_mail(agent_mail_request(
            "mail_accept_foreign",
            &task_a.id,
            &task_foreign.id,
            "task_a",
            "cross workspace attempt",
            AgentMailDeliveryMode::QueueOnly,
        ))
        .await
        .expect_err("cross-workspace mail must fail closed")
        .to_string();
    assert!(denied.contains("ownership denied"), "{denied}");
    assert!(
        manager
            .list_agent_mail_for_thread(&task_foreign.id)
            .await?
            .is_empty()
    );

    // (4) A real terminal turn on B is the safe boundary: the wake envelope
    // delivers exactly once; the queue-only envelope stays queued.
    let m2 = manager
        .queue_agent_mail(agent_mail_request(
            "mail_accept_m2",
            &task_a.id,
            &task_b.id,
            "task_a",
            "Wake: the fixture suite now passes; land the follow-up step.",
            AgentMailDeliveryMode::WakeAtSafeBoundary,
        ))
        .await?;
    let explicit_turn = manager
        .start_turn(
            &task_b.id,
            StartTurnRequest {
                prompt: "ordinary work turn on task B".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    harness_b
        .tx_event
        .send(EngineEvent::MessageComplete { index: 0 })
        .await?;
    harness_b
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    let explicit_terminal = wait_for_terminal_turn(&manager, &explicit_turn.id).await?;
    assert_eq!(explicit_terminal.status, RuntimeTurnStatus::Completed);

    let deadline = StdInstant::now() + Duration::from_secs(10);
    let (m2_delivered, wake_turns) = loop {
        let inbox = manager.list_agent_mail_for_thread(&task_b.id).await?;
        let envelope = inbox
            .iter()
            .find(|mail| mail.message_id.as_str() == "mail_accept_m2")
            .expect("wake envelope present");
        let turns = manager
            .store
            .list_turns_for_thread(&task_b.id)?
            .into_iter()
            .filter(|turn| turn.agent_mail_message_id.as_deref() == Some("mail_accept_m2"))
            .count();
        if envelope.status == AgentMailStatus::Delivered && turns == 1 {
            break (envelope.clone(), turns);
        }
        if StdInstant::now() > deadline {
            anyhow::bail!(
                "wake delivery did not settle: status={:?} turns={turns}",
                envelope.status
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        wake_turns, 1,
        "wake mail must produce exactly one follow-up"
    );
    assert!(m2_delivered.delivery_turn_id.is_some());
    assert_eq!(
        mail_event_count(&manager, &task_b.id, AGENT_MAIL_EVENT_DELIVERING),
        1
    );
    assert_eq!(
        mail_event_count(&manager, &task_b.id, AGENT_MAIL_EVENT_DELIVERED),
        1
    );
    let inbox_after_boundary = manager.list_agent_mail_for_thread(&task_b.id).await?;
    let m1_after = inbox_after_boundary
        .iter()
        .find(|mail| mail.message_id.as_str() == "mail_accept_m1")
        .expect("queue-only envelope present");
    assert_eq!(
        m1_after.status,
        AgentMailStatus::Queued,
        "queue-only mail must wait for an explicit delivery request"
    );

    // (5) Read transition publishes exactly one READ event.
    let m2_read = manager
        .mark_agent_mail_read(&task_b.id, &m2.envelope.message_id)
        .await?;
    assert_eq!(m2_read.status, AgentMailStatus::Read);
    assert!(m2_read.read_at.is_some());
    assert_eq!(
        mail_event_count(&manager, &task_b.id, AGENT_MAIL_EVENT_READ),
        1
    );

    // (6) Queued mail survives a full runtime restart; replay stays idempotent.
    let m3 = manager
        .queue_agent_mail(agent_mail_request(
            "mail_accept_m3",
            &task_a.id,
            &task_b.id,
            "task_a",
            "Post-restart handoff: re-run the acceptance matrix.",
            AgentMailDeliveryMode::QueueOnly,
        ))
        .await?;
    drop(manager);
    let reopened = test_manager(runtime_dir.clone())?;
    let inbox_restarted = reopened.list_agent_mail_for_thread(&task_b.id).await?;
    assert!(
        inbox_restarted
            .iter()
            .any(|mail| mail.message_id == m3.envelope.message_id
                && mail.status == AgentMailStatus::Queued)
    );
    assert!(
        inbox_restarted
            .iter()
            .any(|mail| mail.message_id == m2.envelope.message_id
                && mail.status == AgentMailStatus::Read)
    );
    let m1_restart_replay = reopened
        .queue_agent_mail(agent_mail_request(
            "mail_accept_m1",
            &task_a.id,
            &task_b.id,
            "task_a",
            credential_summary,
            AgentMailDeliveryMode::QueueOnly,
        ))
        .await?;
    assert!(m1_restart_replay.idempotent_replay);
    // one queued event per envelope (m1, m2, m3) — replay added none
    assert_eq!(
        mail_event_count(&reopened, &task_b.id, AGENT_MAIL_EVENT_QUEUED),
        3
    );

    // (7) Evidence: sanitized envelopes, persisted events, turn receipts,
    // restart state, and a SHA-256 manifest.
    let envelopes = reopened.list_agent_mail_for_thread(&task_b.id).await?;
    let envelope_dump = serde_json::to_string_pretty(&envelopes)?;
    assert!(!envelope_dump.contains("sk-fixture-secret"));
    assert!(!envelope_dump.contains("/tmp/private"));
    fs::write(artifacts.join("envelopes-b.sanitized.json"), &envelope_dump)?;

    let events = reopened.events_since(&task_b.id, None)?;
    let mut event_lines = String::new();
    for event in &events {
        event_lines.push_str(&serde_json::to_string(event)?);
        event_lines.push('\n');
    }
    assert!(!event_lines.contains("sk-fixture-secret"));
    fs::write(artifacts.join("events-b.jsonl"), &event_lines)?;

    let turns_b = reopened.store.list_turns_for_thread(&task_b.id)?;
    let turn_receipts: Vec<_> = turns_b
        .iter()
        .map(|turn| {
            serde_json::json!({
                "turn_id": turn.id,
                "status": turn.status,
                "agent_mail_message_id": turn.agent_mail_message_id,
                "input_summary": turn.input_summary,
                "created_at": turn.created_at,
            })
        })
        .collect();
    let stub_request_counts = serde_json::json!({
        "task_b_explicit_user_turns": 1,
        "task_b_agent_mail_follow_up_turns": 1,
        "task_b_turns_total": turn_receipts.len(),
        "task_a_provider_requests": 0,
        "note": "mock engine channel; queue-only mail sampled zero turns",
    });
    fs::write(
        artifacts.join("turns-b.json"),
        serde_json::to_string_pretty(&turn_receipts)?,
    )?;
    fs::write(
        artifacts.join("stub-request-counts.json"),
        serde_json::to_string_pretty(&stub_request_counts)?,
    )?;
    fs::write(
        artifacts.join("restart-inbox-b.json"),
        serde_json::to_string_pretty(&inbox_restarted)?,
    )?;
    fs::write(
        artifacts.join("boundary-receipt.md"),
        format!(
            "# Agent Mail acceptance boundary receipt\n\n\
             explicit turn {} -> {:?}\n\
             wake envelope {} -> {:?} (delivery_turn_id={:?})\n\
             queue-only envelope {} stayed {:?} across the boundary\n",
            explicit_terminal.id,
            explicit_terminal.status,
            m2.envelope.message_id,
            m2_delivered.status,
            m2_delivered.delivery_turn_id,
            m1.envelope.message_id,
            m1_after.status,
        ),
    )?;

    let mut manifest = serde_json::Map::new();
    for name in [
        "envelopes-b.sanitized.json",
        "events-b.jsonl",
        "turns-b.json",
        "stub-request-counts.json",
        "restart-inbox-b.json",
        "boundary-receipt.md",
    ] {
        let bytes = fs::read(artifacts.join(name))?;
        manifest.insert(
            name.to_string(),
            serde_json::json!({
                "sha256": codewhale_runtime::hashing::sha256_hex(&bytes),
                "bytes": bytes.len(),
            }),
        );
    }
    fs::write(
        artifacts.join("manifest.json"),
        serde_json::to_string_pretty(&serde_json::Value::Object(manifest))?,
    )?;
    Ok(())
}

fn mail_event_count(manager: &RuntimeThreadManager, thread_id: &str, event_name: &str) -> usize {
    manager
        .events_since(thread_id, None)
        .expect("events readable")
        .into_iter()
        .filter(|event| event.event == event_name)
        .count()
}

/// Regression for #5823: after a Runtime restart, history is rebuilt from the
/// persisted turn-item snapshots. The live engine path stores the tool-call
/// identity in item metadata and overwrites `detail` with the tool output at
/// completion, so the rebuild must restore a paired, non-empty
/// tool_call/tool_result from metadata instead of an empty shell.
#[test]
fn restart_rebuild_restores_tool_call_identity_from_persisted_items() -> Result<()> {
    let dir = test_runtime_dir();
    let manager = test_manager(dir.clone())?;
    let thread = sample_thread("thr_rebuild_5823");
    manager.store.save_thread(&thread)?;

    let now = Utc::now();
    let user_item = TurnItemRecord {
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: "item_u5823".to_string(),
        turn_id: "turn_5823".to_string(),
        kind: TurnItemKind::UserMessage,
        status: TurnItemLifecycleStatus::Completed,
        summary: "read the readme".to_string(),
        detail: Some("read the readme".to_string()),
        metadata: None,
        artifact_refs: Vec::new(),
        artifacts: Vec::new(),
        started_at: Some(now),
        ended_at: Some(now),
    };
    // Completed live tool call: identity captured at `ToolCallStarted`, result
    // markers merged at `ToolCallComplete`, `detail` holding the tool output.
    let call_item = TurnItemRecord {
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: "item_t5823".to_string(),
        turn_id: "turn_5823".to_string(),
        kind: TurnItemKind::ToolCall,
        status: TurnItemLifecycleStatus::Completed,
        summary: "read: readme body".to_string(),
        detail: Some("# README body".to_string()),
        metadata: Some(json!({
            "tool_use_id": "call_00_abc",
            "tool_name": "read",
            "tool_input": r#"{"path":"README.md"}"#,
            "tool_result_for": "call_00_abc",
            "is_error": false,
        })),
        artifact_refs: Vec::new(),
        artifacts: Vec::new(),
        started_at: Some(now),
        ended_at: Some(now),
    };
    manager.store.save_item(&user_item)?;
    manager.store.save_item(&call_item)?;
    manager.store.save_turn(&TurnRecord {
        prefix_checkpoint: None,
        decision_receipts: Vec::new(),
        max_output_tokens: None,
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: "turn_5823".to_string(),
        thread_id: thread.id.clone(),
        status: RuntimeTurnStatus::Completed,
        input_summary: "read the readme".to_string(),
        created_at: now,
        started_at: Some(now),
        ended_at: Some(now),
        duration_ms: None,
        usage: None,
        routing_settlement: false,
        effective_route_usage: None,
        permission_posture: None,
        mode: None,
        effective_provider: None,
        effective_provider_id: None,
        effective_openrouter_vendor: None,
        effective_billing_surface: None,
        effective_endpoint_fingerprint: None,
        effective_provider_live_pricing: None,
        effective_billing_mode: None,
        effective_dispatched_at: None,
        effective_model: None,
        routed_usage: Vec::new(),
        routed_usage_drop_records: Vec::new(),
        routed_usage_source_ids: Vec::new(),
        routed_usage_dropped_records: 0,
        model_request_diagnostics: None,
        operation_activity: None,
        error: None,
        item_ids: vec![user_item.id.clone(), call_item.id.clone()],
        steer_count: 0,
        agent_mail_message_id: None,
        artifacts: Vec::new(),
        workspace: None,
        workspace_snapshots: Vec::new(),
    })?;

    let turns = manager.store.list_turns_for_thread(&thread.id)?;
    let messages = manager.reconstruct_messages_from_turns(&turns)?;
    let roles: Vec<&str> = messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, vec!["user", "assistant", "user"]);

    match &messages[1].content[0] {
        ContentBlock::ToolUse {
            id, name, input, ..
        } => {
            assert_eq!(id, "call_00_abc");
            assert_eq!(name, "read");
            // This exact string is the provider `tool_calls[].function.arguments`
            // value; the old bug replayed "null" with empty id/name.
            assert_eq!(
                serde_json::to_string(input).unwrap(),
                r#"{"path":"README.md"}"#
            );
        }
        other => panic!("expected restored tool call, got {other:?}"),
    }
    match &messages[2].content[0] {
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            ..
        } => {
            assert_eq!(tool_use_id, "call_00_abc");
            assert_eq!(content, "# README body");
        }
        other => panic!("expected restored tool result, got {other:?}"),
    }

    let mut checkpoint_turn = sample_turn(&thread.id, "turn_prefix", RuntimeTurnStatus::InProgress);
    checkpoint_turn.schema_version = PREFIX_CHECKPOINT_RUNTIME_SCHEMA_VERSION;
    checkpoint_turn.max_output_tokens = std::num::NonZeroU32::new(1500);
    let mut checkpoint_user = user_item.clone();
    checkpoint_user.id = "item_prefix_user".into();
    checkpoint_user.turn_id = checkpoint_turn.id.clone();
    checkpoint_user.detail = Some("checkpoint input".into());
    checkpoint_user.summary = "checkpoint input".into();
    let mut first = call_item.clone();
    first.id = "item_prefix_first".into();
    first.turn_id = checkpoint_turn.id.clone();
    first.status = TurnItemLifecycleStatus::InProgress;
    first.detail = Some(r#"{"path":"first","token":"fixture-secret"}"#.into());
    first.metadata = Some(
        json!({"tool_use_id":"execution_first", "execution_id":"execution_first",
        "provider_tool_use_id":"provider_first", "tool_name":"read", "tool_input":r#"{"path":"first","token":"fixture-secret"}"#}),
    );
    let mut closed = first.clone();
    closed.id = "item_prefix_closed".into();
    closed.status = TurnItemLifecycleStatus::Completed;
    closed.detail = Some("raw durable result".into());
    closed.metadata = Some(
        json!({"tool_use_id":"execution_closed", "execution_id":"execution_closed",
        "provider_tool_use_id":"provider_closed", "tool_name":"read", "tool_input":r#"{"path":"closed"}"#,
        "tool_result_for":"execution_closed"}),
    );
    checkpoint_turn.item_ids = vec![
        checkpoint_user.id.clone(),
        closed.id.clone(),
        first.id.clone(),
    ];
    manager.store.save_item(&checkpoint_user)?;
    manager.store.save_item(&closed)?;
    manager.store.save_item(&first)?;
    let mut prefix = messages.clone();
    prefix.extend(serde_json::from_value::<Vec<Message>>(json!([
        {"role":"user", "content":[{"type":"text","text":"<turn_meta>canonical</turn_meta>checkpoint input"}]},
        {"role":"assistant", "content":[{"type":"tool_use","execution_id":"execution_closed","id":"provider_closed","name":"read","input":{"path":"closed"}}]},
        {"role":"user", "content":[{"type":"tool_result","execution_id":"execution_closed","tool_use_id":"provider_closed","content":"compacted result\n[hook context] retained canonical detail"}]},
        {"role":"assistant", "content":[
            {"type":"thinking","thinking":"canonical thought","signature":"retained-signature"},
            {"type":"tool_use","execution_id":"execution_first","id":"provider_first","name":"read","input":{"path":"first","token":"fixture-secret"}},
            {"type":"tool_use","execution_id":"execution_second","id":"provider_second","name":"read","input":{"path":"second"}}
        ]}
    ]))?);
    let source_sha256 = session_messages_sha256(&prefix)?;
    let tool_declarations = checkpoint_tool_declarations(&prefix)?;
    for message in &mut prefix {
        crate::core::engine::redact_tool_blocks(message, |text| {
            text.replace("fixture-secret", "[REDACTED]")
        });
    }
    let reference = manager.store.save_settled_history(&prefix)?;
    checkpoint_turn.prefix_checkpoint = Some(RuntimePrefixCheckpoint {
        checkpoint_id: "checkpoint_fixture".into(),
        session_id: thread.id.clone(),
        agent_id: None,
        runtime_turn_id: checkpoint_turn.id.clone(),
        engine_turn_id: "engine_prefix".into(),
        call_id: "invocation_prefix".into(),
        owner_id: manager.store.owner_id.clone(),
        source_sha256,
        persisted_sha256: reference["sha256"].as_str().unwrap().into(),
        messages_len: prefix.len(),
        projection: "runtime_receipt_redacted".into(),
        item_ids: checkpoint_turn.item_ids.clone(),
        tool_declarations,
        pending_items: Vec::new(),
        covered_items: HashMap::from([
            (
                checkpoint_user.id.clone(),
                checkpoint_item_digest(&checkpoint_user)?,
            ),
            (closed.id.clone(), checkpoint_item_digest(&closed)?),
        ]),
    });
    manager.store.save_turn(&checkpoint_turn)?;
    assert!(
        serde_json::to_value(&checkpoint_turn)?
            .get("prefix_checkpoint")
            .is_none()
    );
    assert!(
        manager
            .store
            .load_turn(&checkpoint_turn.id)?
            .prefix_checkpoint
            .is_some()
    );
    assert_eq!(manager.restore_thread_messages(&thread)?, prefix);
    checkpoint_turn.prefix_checkpoint.as_mut().unwrap().owner_id = "foreign_owner".into();
    manager.store.save_turn(&checkpoint_turn)?;
    assert!(manager.restore_thread_messages(&thread).is_err());
    checkpoint_turn.prefix_checkpoint.as_mut().unwrap().owner_id = manager.store.owner_id.clone();
    manager.store.save_turn(&checkpoint_turn)?;

    first.status = TurnItemLifecycleStatus::Completed;
    first.detail = Some("first result".into());
    first.metadata.as_mut().unwrap()["tool_result_for"] = json!("execution_first");
    manager.store.save_item(&first)?;
    let mut second = first.clone();
    second.id = "item_prefix_second".into();
    second.status = TurnItemLifecycleStatus::Interrupted;
    second.detail = Some(r#"{"path":"second"}"#.into());
    second.metadata = Some(
        json!({"tool_use_id":"execution_second", "execution_id":"execution_second",
        "provider_tool_use_id":"provider_second", "tool_name":"read", "tool_input":r#"{"path":"second"}"#}),
    );
    let mut later = checkpoint_user.clone();
    later.id = "item_prefix_later".into();
    later.kind = TurnItemKind::AgentMessage;
    later.detail = Some("later assistant".into());
    later.summary = "later assistant".into();
    manager.store.save_item(&second)?;
    manager.store.save_item(&later)?;
    checkpoint_turn
        .item_ids
        .extend([second.id.clone(), later.id.clone()]);
    checkpoint_turn.status = RuntimeTurnStatus::Interrupted;
    manager.store.save_turn(&checkpoint_turn)?;
    let mut expected = prefix.clone();
    expected.extend(serde_json::from_value::<Vec<Message>>(json!([
        {"role":"user","content":[
            {"type":"tool_result","execution_id":"execution_first","tool_use_id":"provider_first","content":"first result"},
            {"type":"tool_result","execution_id":"execution_second","tool_use_id":"provider_second",
                "content":codewhale_runtime::tool_history_repair::CRASH_REPAIR_CONTENT,"is_error":true}
        ]},
        {"role":"assistant","content":[{"type":"text","text":"later assistant"}]}
    ]))?);
    assert_eq!(manager.restore_thread_messages(&thread)?, expected);
    let turn = manager.store.load_turn(&checkpoint_turn.id)?;
    let items = manager.prepared_items_for_turns(std::slice::from_ref(&turn))?;
    assert_eq!(
        manager.history_from_records(std::slice::from_ref(&turn), &items)?,
        expected
    );
    first.metadata.as_mut().unwrap()["tool_input"] = json!(r#"{"path":"changed"}"#);
    manager.store.save_item(&first)?;
    assert!(manager.restore_thread_messages(&thread).is_err());
    first.metadata.as_mut().unwrap()["tool_input"] =
        json!(r#"{"path":"first","token":"fixture-secret"}"#);
    manager.store.save_item(&first)?;
    let history_path = manager
        .store
        .settled_history_path(reference["sha256"].as_str().unwrap())?;
    fs::write(&history_path, b"corrupt")?;
    assert!(manager.restore_thread_messages(&thread).is_err());
    manager.store.save_settled_history(&prefix)?;
    fs::remove_file(manager.store.item_path(&checkpoint_user.id)?)?;
    assert!(manager.restore_thread_messages(&thread).is_err());
    manager.store.save_item(&checkpoint_user)?;
    fs::remove_file(manager.store.item_path(&second.id)?)?;
    checkpoint_turn.item_ids.retain(|id| id != &second.id);
    manager.store.save_turn(&checkpoint_turn)?;
    assert_eq!(manager.restore_thread_messages(&thread)?, expected);
    drop(manager);
    let manager = test_manager(dir.clone())?;
    assert_eq!(manager.restore_thread_messages(&thread)?, expected);

    let mut compacted = sample_turn(
        &thread.id,
        "turn_after_prefix",
        RuntimeTurnStatus::Completed,
    );
    compacted.created_at = checkpoint_turn.created_at + chrono::Duration::seconds(1);
    let compacted_history: Vec<Message> = serde_json::from_value(json!([
        {"role":"user","content":[{"type":"text","text":"newer canonical compaction"}]}
    ]))?;
    let compacted_reference = manager.store.save_settled_history(&compacted_history)?;
    let mut compaction_item = later.clone();
    compaction_item.id = "item_after_prefix_compaction".into();
    compaction_item.turn_id = compacted.id.clone();
    compaction_item.kind = TurnItemKind::ContextCompaction;
    compaction_item.metadata = Some(json!({"settled_history":compacted_reference}));
    compacted.item_ids = vec![compaction_item.id.clone()];
    manager.store.save_item(&compaction_item)?;
    manager.store.save_turn(&compacted)?;
    fs::write(&history_path, b"superseded old prefix")?;
    assert_eq!(manager.restore_thread_messages(&thread)?, compacted_history);

    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

/// A tool call snapshot persisted at `ToolCallStarted` but never completed
/// (restart mid-tool) still carries its identity and must rebuild the call
/// with non-empty id/name/arguments instead of an empty shell.
#[test]
fn restart_rebuild_keeps_in_flight_tool_call_identity() -> Result<()> {
    let dir = test_runtime_dir();
    let manager = test_manager(dir.clone())?;
    let thread = sample_thread("thr_rebuild_5823_inflight");
    manager.store.save_thread(&thread)?;

    let now = Utc::now();
    let call_item = TurnItemRecord {
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: "item_inflight5823".to_string(),
        turn_id: "turn_5823_inflight".to_string(),
        kind: TurnItemKind::ToolCall,
        status: TurnItemLifecycleStatus::InProgress,
        summary: "read started".to_string(),
        detail: Some(r#"{"path":"README.md"}"#.to_string()),
        metadata: Some(json!({
            "tool_use_id": "call_00_def",
            "tool_name": "read",
            "tool_input": r#"{"path":"README.md"}"#,
        })),
        artifact_refs: Vec::new(),
        artifacts: Vec::new(),
        started_at: Some(now),
        ended_at: None,
    };
    manager.store.save_item(&call_item)?;
    manager.store.save_turn(&TurnRecord {
        prefix_checkpoint: None,
        decision_receipts: Vec::new(),
        max_output_tokens: None,
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: "turn_5823_inflight".to_string(),
        thread_id: thread.id.clone(),
        status: RuntimeTurnStatus::Failed,
        input_summary: "read the readme".to_string(),
        created_at: now,
        started_at: Some(now),
        ended_at: Some(now),
        duration_ms: None,
        usage: None,
        routing_settlement: false,
        effective_route_usage: None,
        permission_posture: None,
        mode: None,
        effective_provider: None,
        effective_provider_id: None,
        effective_openrouter_vendor: None,
        effective_billing_surface: None,
        effective_endpoint_fingerprint: None,
        effective_provider_live_pricing: None,
        effective_billing_mode: None,
        effective_dispatched_at: None,
        effective_model: None,
        routed_usage: Vec::new(),
        routed_usage_drop_records: Vec::new(),
        routed_usage_source_ids: Vec::new(),
        routed_usage_dropped_records: 0,
        model_request_diagnostics: None,
        operation_activity: None,
        error: None,
        item_ids: vec![call_item.id.clone()],
        steer_count: 0,
        agent_mail_message_id: None,
        artifacts: Vec::new(),
        workspace: None,
        workspace_snapshots: Vec::new(),
    })?;

    let turns = manager.store.list_turns_for_thread(&thread.id)?;
    let messages = manager.reconstruct_messages_from_turns(&turns)?;
    match &messages[0].content[0] {
        ContentBlock::ToolUse {
            id, name, input, ..
        } => {
            assert_eq!(id, "call_00_def");
            assert_eq!(name, "read");
            assert_eq!(
                serde_json::to_string(input).unwrap(),
                r#"{"path":"README.md"}"#
            );
        }
        other => panic!("expected in-flight tool call identity, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

/// A steer the engine never committed is recorded `canceled`/`queued` exactly
/// because the model never saw it. Rebuilding history must honour that: #6276
/// made the receipt honest, and replaying the text here would put it back into
/// the context the receipt says it never reached.
#[test]
fn restart_rebuild_skips_steers_the_engine_never_delivered() -> Result<()> {
    let dir = test_runtime_dir();
    let manager = test_manager(dir.clone())?;
    let thread = sample_thread("thr_rebuild_6276");
    manager.store.save_thread(&thread)?;

    let now = Utc::now();
    let turn_id = "turn_6276_rebuild".to_string();
    let item = |id: &str, text: &str, status: TurnItemLifecycleStatus| TurnItemRecord {
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: id.to_string(),
        turn_id: turn_id.clone(),
        kind: TurnItemKind::UserMessage,
        status,
        summary: text.to_string(),
        detail: Some(text.to_string()),
        metadata: None,
        artifact_refs: Vec::new(),
        artifacts: Vec::new(),
        started_at: Some(now),
        ended_at: Some(now),
    };
    let delivered = item("item_6276_ok", "hello", TurnItemLifecycleStatus::Completed);
    let dropped = item(
        "item_6276_drop",
        "never seen by the model",
        TurnItemLifecycleStatus::Canceled,
    );
    let pending = item(
        "item_6276_queue",
        "not settled yet",
        TurnItemLifecycleStatus::Queued,
    );
    manager.store.save_item(&delivered)?;
    manager.store.save_item(&dropped)?;
    manager.store.save_item(&pending)?;
    manager.store.save_turn(&TurnRecord {
        prefix_checkpoint: None,
        decision_receipts: Vec::new(),
        max_output_tokens: None,
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: turn_id.clone(),
        thread_id: thread.id.clone(),
        status: RuntimeTurnStatus::Completed,
        input_summary: "hello".to_string(),
        created_at: now,
        started_at: Some(now),
        ended_at: Some(now),
        duration_ms: None,
        usage: None,
        routing_settlement: false,
        effective_route_usage: None,
        permission_posture: None,
        mode: None,
        effective_provider: None,
        effective_provider_id: None,
        effective_openrouter_vendor: None,
        effective_billing_surface: None,
        effective_endpoint_fingerprint: None,
        effective_provider_live_pricing: None,
        effective_billing_mode: None,
        effective_dispatched_at: None,
        effective_model: None,
        routed_usage: Vec::new(),
        routed_usage_drop_records: Vec::new(),
        routed_usage_source_ids: Vec::new(),
        routed_usage_dropped_records: 0,
        model_request_diagnostics: None,
        operation_activity: None,
        error: None,
        item_ids: vec![delivered.id.clone(), dropped.id.clone(), pending.id.clone()],
        steer_count: 0,
        agent_mail_message_id: None,
        artifacts: Vec::new(),
        workspace: None,
        workspace_snapshots: Vec::new(),
    })?;

    let turns = manager.store.list_turns_for_thread(&thread.id)?;
    let messages = manager.reconstruct_messages_from_turns(&turns)?;
    let replayed: Vec<String> = messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        replayed,
        vec!["hello".to_string()],
        "only the delivered user message may be rebuilt"
    );

    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

/// Snapshots persisted before tool identity was durable (metadata absent)
/// must contribute nothing to rebuilt history: an empty id/name tool_call
/// with `arguments: "null"` is rejected by strict OpenAI-compatible
/// endpoints, so degrading silently is safer than replaying the shell.
#[test]
fn restart_rebuild_skips_legacy_tool_items_without_identity() -> Result<()> {
    let dir = test_runtime_dir();
    let manager = test_manager(dir.clone())?;
    let thread = sample_thread("thr_rebuild_5823_legacy");
    manager.store.save_thread(&thread)?;

    let now = Utc::now();
    let user_item = TurnItemRecord {
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: "item_legacy_u".to_string(),
        turn_id: "turn_5823_legacy".to_string(),
        kind: TurnItemKind::UserMessage,
        status: TurnItemLifecycleStatus::Completed,
        summary: "hello".to_string(),
        detail: Some("hello".to_string()),
        metadata: None,
        artifact_refs: Vec::new(),
        artifacts: Vec::new(),
        started_at: Some(now),
        ended_at: Some(now),
    };
    let legacy_tool_item = TurnItemRecord {
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: "item_legacy_t".to_string(),
        turn_id: "turn_5823_legacy".to_string(),
        kind: TurnItemKind::ToolCall,
        status: TurnItemLifecycleStatus::Completed,
        summary: "read: old output".to_string(),
        detail: Some("old output".to_string()),
        metadata: None,
        artifact_refs: Vec::new(),
        artifacts: Vec::new(),
        started_at: Some(now),
        ended_at: Some(now),
    };
    manager.store.save_item(&user_item)?;
    manager.store.save_item(&legacy_tool_item)?;
    manager.store.save_turn(&TurnRecord {
        prefix_checkpoint: None,
        decision_receipts: Vec::new(),
        max_output_tokens: None,
        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
        id: "turn_5823_legacy".to_string(),
        thread_id: thread.id.clone(),
        status: RuntimeTurnStatus::Completed,
        input_summary: "hello".to_string(),
        created_at: now,
        started_at: Some(now),
        ended_at: Some(now),
        duration_ms: None,
        usage: None,
        routing_settlement: false,
        effective_route_usage: None,
        permission_posture: None,
        mode: None,
        effective_provider: None,
        effective_provider_id: None,
        effective_openrouter_vendor: None,
        effective_billing_surface: None,
        effective_endpoint_fingerprint: None,
        effective_provider_live_pricing: None,
        effective_billing_mode: None,
        effective_dispatched_at: None,
        effective_model: None,
        routed_usage: Vec::new(),
        routed_usage_drop_records: Vec::new(),
        routed_usage_source_ids: Vec::new(),
        routed_usage_dropped_records: 0,
        model_request_diagnostics: None,
        operation_activity: None,
        error: None,
        item_ids: vec![user_item.id.clone(), legacy_tool_item.id.clone()],
        steer_count: 0,
        agent_mail_message_id: None,
        artifacts: Vec::new(),
        workspace: None,
        workspace_snapshots: Vec::new(),
    })?;

    let turns = manager.store.list_turns_for_thread(&thread.id)?;
    let messages = manager.reconstruct_messages_from_turns(&turns)?;
    let roles: Vec<&str> = messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, vec!["user"]);
    for message in &messages {
        for block in &message.content {
            assert!(
                !matches!(
                    block,
                    ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. }
                ),
                "legacy snapshot must not rebuild a tool block, got {block:?}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

#[test]
fn store_load_failures_carry_typed_record_context() -> Result<()> {
    // #5931: a monitor recognizes a store fault by type, not by message
    // text, and the file it names is the one on disk.
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone())?;

    let error = store
        .load_turn("turn_missing")
        .expect_err("missing turn loaded");
    let failure = RuntimeStoreRecordFailure::from_error(&error).expect("typed read context");
    assert_eq!(failure.operation, RuntimeStoreOperation::Read);
    assert_eq!(failure.record_kind, RuntimeStoreRecordKind::Turn);
    assert_eq!(failure.record_id, "turn_missing");
    assert_eq!(failure.path, store.turn_path("turn_missing")?);
    assert!(
        error.to_string().starts_with("Failed to read turn "),
        "{error}"
    );

    let item_path = store.item_path("item_garbage")?;
    std::fs::write(&item_path, "{ not json")?;
    let error = store
        .load_item("item_garbage")
        .expect_err("garbage item loaded");
    let failure = RuntimeStoreRecordFailure::from_error(&error).expect("typed parse context");
    assert_eq!(failure.operation, RuntimeStoreOperation::Parse);
    assert_eq!(failure.record_kind, RuntimeStoreRecordKind::Item);
    assert_eq!(failure.path, item_path);
    let notice = failure.notice(&error, false);
    let shown_path = item_path.display().to_string();
    assert!(notice.message.contains(&shown_path), "{}", notice.message);
    assert!(
        notice
            .next_action
            .starts_with(&format!("Move {shown_path} aside")),
        "{}",
        notice.next_action
    );
    assert!(!notice.reason.is_empty());
    assert!(!notice.terminal);

    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

#[tokio::test]
async fn settle_claimed_turn_failure_publishes_store_failure_for_unreadable_turn() -> Result<()> {
    // #5931: "Failed to load turn after monitor failure" was a log line; the
    // operator now gets an event naming the file, and drivers learn the turn
    // can never reach `turn.completed`.
    let dir = test_runtime_dir();
    let manager = test_manager(dir.clone())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let turn_id = "turn_unreadable_record";
    let turn_path = manager.store.turn_path(turn_id)?;
    std::fs::write(&turn_path, "{ not a turn")?;

    manager
        .settle_claimed_turn_failure(&thread.id, turn_id, "forced monitor failure")
        .await;

    let events = manager.events_since(&thread.id, None)?;
    let notice = events
        .iter()
        .find(|event| event.event == RUNTIME_STORE_FAILURE_EVENT)
        .expect("store failure event was published");
    assert_eq!(notice.turn_id.as_deref(), Some(turn_id));
    let payload: RuntimeStoreFailureNotice = serde_json::from_value(notice.payload.clone())?;
    assert_eq!(payload.failure.operation, RuntimeStoreOperation::Parse);
    assert_eq!(payload.failure.record_kind, RuntimeStoreRecordKind::Turn);
    assert_eq!(payload.failure.record_id, turn_id);
    assert_eq!(payload.failure.path, turn_path);
    assert!(
        payload.terminal,
        "an unreadable turn record can never terminalize"
    );
    let shown_path = turn_path.display().to_string();
    assert!(payload.message.contains(&shown_path), "{}", payload.message);
    assert!(
        payload.next_action.contains("aside"),
        "{}",
        payload.next_action
    );
    assert!(
        events.iter().all(|event| event.event != "turn.completed"),
        "no terminal receipt can follow an unreadable turn record"
    );

    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

#[tokio::test]
async fn report_store_failure_names_the_item_file_without_terminalizing() -> Result<()> {
    // The monitor's "Failed to read item …" path: the notice carries the
    // item id and file, and a fault without typed context stays a log line.
    let dir = test_runtime_dir();
    let manager = test_manager(dir.clone())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let item_id = "item_unreadable";
    let item_path = manager.store.item_path(item_id)?;
    std::fs::write(&item_path, "{ not an item")?;
    let error = manager
        .store
        .load_item(item_id)
        .expect_err("garbage item loaded");

    let published = manager
        .report_store_failure(
            &thread.id,
            Some("turn_monitor"),
            &format!("Failed to monitor turn: {error}"),
            &error,
            false,
        )
        .await
        .expect("a typed store fault publishes");
    assert_eq!(published.event, RUNTIME_STORE_FAILURE_EVENT);
    assert_eq!(published.turn_id.as_deref(), Some("turn_monitor"));
    assert_eq!(published.item_id.as_deref(), Some(item_id));
    let payload: RuntimeStoreFailureNotice = serde_json::from_value(published.payload)?;
    assert!(!payload.terminal);
    assert_eq!(payload.failure.path, item_path);
    assert_eq!(payload.failure.record_kind, RuntimeStoreRecordKind::Item);

    let plain = anyhow::anyhow!("plain failure");
    assert!(
        manager
            .report_store_failure(&thread.id, None, "plain", &plain, false)
            .await
            .is_none()
    );
    assert_eq!(
        manager
            .events_since(&thread.id, None)?
            .iter()
            .filter(|event| event.event == RUNTIME_STORE_FAILURE_EVENT)
            .count(),
        1
    );

    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

mod task_ownership;

mod runtime_image_inputs {
    use super::*;
    use crate::core::engine::Engine;
    use crate::image_attach::test_support::runtime_image_fixture;
    use crate::llm_client::mock::{MockLlmClient, canned};

    fn config() -> Config {
        let mut config = {
            let __cw_config_init_28_0 = Some("deepseek".into());
            let __cw_config_init_28_1 = Some("deepseek-v4-flash-vision-exp".into());
            let __cw_config_init_28_2 = true;
            let mut __cw_config_init_28_value: Config = Config::default();
            __cw_config_init_28_value.provider = __cw_config_init_28_0;
            __cw_config_init_28_value.default_text_model = __cw_config_init_28_1;
            __cw_config_init_28_value.runtime_chat_isolated = __cw_config_init_28_2;
            __cw_config_init_28_value
        }
        .with_legacy_root(Some("synthetic-image-fixture-key".into()), None);
        config
            .set_provider_model_override(
                &config.test_identity_for_kind(ProviderKind::Deepseek),
                Some("deepseek-v4-flash-vision-exp".into()),
            )
            .unwrap();
        config.set_feature("mcp", false).unwrap();
        config.set_feature("subagents", false).unwrap();
        config
    }

    #[tokio::test]
    async fn runtime_image_admission_rejects_auto_unknown_and_malformed_without_engine()
    -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let manager = RuntimeThreadManager::open(
            config(),
            dir.path().to_path_buf(),
            test_manager_config(dir.path().join("runtime")),
        )?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let good = runtime_image_fixture(17);
        for (prompt, model, image) in [
            ("look", "auto", good.clone()),
            ("look", "deepseek-v4-pro", good.clone()),
            ("look", "unknown-image-fixture", good.clone()),
            ("", "deepseek-v4-flash-vision-exp", good.clone()),
            (
                "look",
                "deepseek-v4-flash-vision-exp",
                codewhale_protocol::runtime::RuntimeImageInput {
                    mime: "image/jpeg".into(),
                    ..good
                },
            ),
        ] {
            assert!(
                manager
                    .start_turn(
                        &thread.id,
                        StartTurnRequest {
                            prompt: prompt.into(),
                            model: Some(model.into()),
                            images: vec![image],
                            operation_key: Some("image-rejected".into()),
                            ..Default::default()
                        }
                    )
                    .await
                    .is_err()
            );
            assert!(
                manager.active.lock().await.engines.is_empty(),
                "invalid image must fail before Engine load/classifier"
            );
            assert!(manager.store.list_turns_for_thread(&thread.id)?.is_empty());
        }
        Ok(())
    }

    #[tokio::test]
    async fn runtime_image_engine_replay_restart_fork_and_retry_keep_exact_order() -> Result<()> {
        image_engine_replay_restart_fork_and_retry_keep_exact_order(None).await
    }

    #[tokio::test]
    async fn output_cap_image_engine_replay_restart_fork_and_retry_keep_exact_order() -> Result<()>
    {
        image_engine_replay_restart_fork_and_retry_keep_exact_order(std::num::NonZeroU32::new(1500))
            .await
    }

    async fn image_engine_replay_restart_fork_and_retry_keep_exact_order(
        max_output_tokens: Option<std::num::NonZeroU32>,
    ) -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let config = config();
        let manager_config = test_manager_config(dir.path().join("runtime"));
        let manager = RuntimeThreadManager::open(
            config.clone(),
            dir.path().to_path_buf(),
            manager_config.clone(),
        )?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let images = vec![runtime_image_fixture(17), runtime_image_fixture(33)];
        let mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
            "fixture image response",
        )]));
        let (engine, handle) = Engine::new_with_model_client(
            EngineConfig {
                workspace: dir.path().to_path_buf(),
                model: thread.model.clone(),
                subagents_enabled: false,
                snapshots_enabled: false,
                memory_enabled: false,
                terminal_chrome_enabled: false,
                runtime_services: crate::tools::spec::RuntimeToolServices {
                    active_thread_id: Some(thread.id.clone()),
                    ..Default::default()
                },
                ..EngineConfig::default()
            },
            &config,
            mock.clone(),
        );
        manager
            .install_test_engine(&thread.id, handle.clone())
            .await?;
        let run = tokio::spawn(engine.run());
        let request = StartTurnRequest {
            max_output_tokens,
            prompt: "compare the two images\n[Attached image: /private/host-only.png]".into(),
            images: images.clone(),
            operation_key: Some("image-exact-replay".into()),
            ..Default::default()
        };
        let turn = manager.start_turn(&thread.id, request.clone()).await?;
        assert_eq!(
            wait_for_terminal_turn(&manager, &turn.id).await?.status,
            RuntimeTurnStatus::Completed
        );
        assert_eq!(turn.max_output_tokens, max_output_tokens);
        assert_eq!(
            turn.schema_version,
            if max_output_tokens.is_some() {
                OUTPUT_LIMIT_RUNTIME_SCHEMA_VERSION
            } else {
                IMAGE_RUNTIME_SCHEMA_VERSION
            }
        );
        assert_eq!(mock.call_count(), 1);
        let provider_request = mock.last_request().context("mock request")?;
        if let Some(max_output_tokens) = max_output_tokens {
            assert_eq!(provider_request.max_tokens, max_output_tokens.get());
        }
        let sent: Vec<_> = provider_request
            .messages
            .iter()
            .flat_map(|m| m.content.clone())
            .collect();
        assert_eq!(
            crate::image_attach::runtime_images_from_blocks(&sent)?,
            images
        );
        assert!(!serde_json::to_string(&provider_request.messages)?.contains("host-only.png"));
        assert_eq!(
            manager.start_turn(&thread.id, request.clone()).await?.id,
            turn.id
        );
        let mut changed = request.clone();
        changed.images.reverse();
        assert!(manager.start_turn(&thread.id, changed).await.is_err());
        assert_eq!(mock.call_count(), 1);
        let item = manager
            .store
            .list_items_for_turn(&turn.id)?
            .into_iter()
            .find(|i| i.kind == TurnItemKind::UserMessage)
            .unwrap();
        assert_eq!(item.schema_version, IMAGE_RUNTIME_SCHEMA_VERSION);
        assert!(
            item.schema_version > 2,
            "pre-image readers reject this schema"
        );
        assert_eq!(
            manager.get_thread(&thread.id).await?.schema_version,
            turn.schema_version
        );
        handle.send(Op::Shutdown).await?;
        tokio::time::timeout(Duration::from_secs(10), run).await??;
        drop(handle);
        drop(manager);
        let reopened =
            RuntimeThreadManager::open(config.clone(), dir.path().to_path_buf(), manager_config)?;
        let replayed = reopened.start_turn(&thread.id, request).await?;
        assert_eq!(replayed.id, turn.id);
        assert_eq!(replayed.max_output_tokens, max_output_tokens);
        assert_eq!(replayed.schema_version, turn.schema_version);
        assert!(
            reopened.active.lock().await.engines.is_empty(),
            "exact replay needs no provider or Engine"
        );
        let stored_thread = reopened.get_thread(&thread.id).await?;
        let messages = reopened.restore_thread_messages(&stored_thread)?;
        assert_eq!(
            crate::image_attach::runtime_images_from_blocks(
                &messages
                    .into_iter()
                    .flat_map(|m| m.content)
                    .collect::<Vec<_>>()
            )?,
            images
        );
        let fork = reopened.fork_thread(&thread.id).await?;
        let forked_turns = reopened.store.list_turns_for_thread(&fork.id)?;
        assert_eq!(forked_turns.len(), 1);
        assert_eq!(forked_turns[0].max_output_tokens, max_output_tokens);
        assert_eq!(forked_turns[0].schema_version, turn.schema_version);
        let messages = reopened.restore_thread_messages(&fork)?;
        assert_eq!(
            crate::image_attach::runtime_images_from_blocks(
                &messages
                    .into_iter()
                    .flat_map(|m| m.content)
                    .collect::<Vec<_>>()
            )?,
            images
        );
        let (_, original_text, retry_images, retry_output_cap) =
            reopened.fork_at_user_message(&thread.id, 0).await?;
        assert!(original_text.unwrap().starts_with("compare the two images"));
        assert_eq!(retry_images, images);
        assert_eq!(retry_output_cap, max_output_tokens);
        let followup_mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
            "remembered screenshots",
        )]));
        let (engine, handle) = Engine::new_with_model_client(
            EngineConfig {
                workspace: dir.path().to_path_buf(),
                model: thread.model.clone(),
                subagents_enabled: false,
                snapshots_enabled: false,
                memory_enabled: false,
                terminal_chrome_enabled: false,
                runtime_services: crate::tools::spec::RuntimeToolServices {
                    active_thread_id: Some(thread.id.clone()),
                    ..Default::default()
                },
                ..Default::default()
            },
            &config,
            followup_mock.clone(),
        );
        reopened
            .install_test_engine(&thread.id, handle.clone())
            .await?;
        let run = tokio::spawn(engine.run());
        handle
            .send(Op::SyncSession {
                session_id: stored_thread.session_id.clone(),
                messages: reopened.restore_thread_messages(&stored_thread)?,
                system_prompt: None,
                system_prompt_override: false,
                model: stored_thread.model.clone(),
                workspace: stored_thread.workspace.clone(),
                mode: AppMode::Agent,
            })
            .await?;
        let followup = reopened
            .start_turn(
                &thread.id,
                StartTurnRequest {
                    prompt: "What differed between those screenshots?".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(
            wait_for_terminal_turn(&reopened, &followup.id)
                .await?
                .status,
            RuntimeTurnStatus::Completed
        );
        assert_eq!(
            followup.max_output_tokens, None,
            "new turns do not inherit the old allowance"
        );
        let outbound = followup_mock
            .last_request()
            .context("restored follow-up request")?;
        assert_eq!(
            crate::image_attach::runtime_images_from_blocks(
                &outbound
                    .messages
                    .iter()
                    .flat_map(|m| m.content.clone())
                    .collect::<Vec<_>>()
            )?,
            images
        );
        assert!(!serde_json::to_string(&outbound.messages)?.contains("host-only.png"));
        handle.send(Op::Shutdown).await?;
        tokio::time::timeout(Duration::from_secs(10), run).await??;
        Ok(())
    }

    #[tokio::test]
    async fn runtime_image_seed_roundtrips_user_block_order_and_future_schema_refuses() -> Result<()>
    {
        let dir = tempfile::tempdir()?;
        let manager = test_manager(dir.path().join("runtime"))?;
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let mut content = crate::image_attach::prepare_runtime_images(&[
            runtime_image_fixture(8),
            runtime_image_fixture(9),
        ])?;
        content.insert(
            1,
            ContentBlock::Text {
                text: "between images".into(),
                cache_control: None,
            },
        );
        let original = vec![Message {
            role: Role::User,
            content,
        }];
        manager
            .seed_thread_from_messages(&thread.id, &original)
            .await?;
        let updated = manager.get_thread(&thread.id).await?;
        assert_eq!(manager.restore_thread_messages(&updated)?, original);
        let turn = manager.store.list_turns_for_thread(&thread.id)?.remove(0);
        assert_eq!(turn.schema_version, IMAGE_RUNTIME_SCHEMA_VERSION);
        let mut item = manager.store.list_items_for_turn(&turn.id)?.remove(0);
        item.schema_version = MAX_SUPPORTED_RUNTIME_SCHEMA_VERSION + 1;
        manager.store.save_item(&item)?;
        assert!(
            manager
                .store
                .load_item(&item.id)
                .unwrap_err()
                .to_string()
                .contains("newer than supported")
        );
        Ok(())
    }

    #[tokio::test]
    async fn runtime_image_durable_corruption_and_bad_import_fail_closed() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let manager = test_manager(dir.path().join("runtime"))?;
        let thread = manager.create_thread(Default::default()).await?;
        let mut valid = crate::image_attach::prepare_runtime_images(&[runtime_image_fixture(4)])?;
        valid.insert(
            0,
            ContentBlock::Text {
                text: "keep my prompt".into(),
                cache_control: None,
            },
        );
        manager
            .seed_thread_from_messages(
                &thread.id,
                &[Message {
                    role: Role::User,
                    content: valid.clone(),
                }],
            )
            .await?;
        let turn = manager.store.list_turns_for_thread(&thread.id)?.remove(0);
        let original = manager.store.list_items_for_turn(&turn.id)?.remove(0);
        let mut bad_url = valid.clone();
        if let ContentBlock::ImageUrl { image_url } = &mut bad_url[1] {
            image_url.url = "https://host.invalid/private-image".into();
        }
        let mut truncated = valid.clone();
        if let ContentBlock::ImageUrl { image_url } = &mut truncated[1] {
            image_url.url = "data:image/png;base64,iVBORw0KGgo=".into();
        }
        for content in [
            vec![],
            valid[..1].to_vec(),
            valid[1..].to_vec(),
            bad_url,
            truncated,
        ] {
            for schema in [
                IMAGE_RUNTIME_SCHEMA_VERSION,
                OUTPUT_LIMIT_RUNTIME_SCHEMA_VERSION,
            ] {
                let mut damaged = original.clone();
                damaged.set_image_content(content.clone());
                damaged.schema_version = schema;
                manager.store.save_item(&damaged)?;
                assert!(manager.store.load_item(&damaged.id).is_err());
                assert!(manager.store.list_items_for_turn(&turn.id).is_err());
                assert!(
                    manager
                        .restore_thread_messages(&manager.get_thread(&thread.id).await?)
                        .is_err()
                );
            }
        }
        // Legacy records never give this additive metadata field new authority.
        let mut legacy = original.clone();
        legacy.schema_version = 2;
        legacy.metadata = Some(json!({"runtime_image_content": []}));
        manager.store.save_item(&legacy)?;
        assert_eq!(
            manager.store.load_item(&legacy.id)?.user_content()?,
            valid[..1]
        );
        let empty = manager.create_thread(Default::default()).await?;
        let mut invalid = valid.clone();
        if let ContentBlock::ImageUrl { image_url } = &mut invalid[1] {
            image_url.url = "file:///private/never-read".into();
        }
        assert!(
            manager
                .seed_thread_from_messages(
                    &empty.id,
                    &[
                        Message {
                            role: Role::User,
                            content: valid
                        },
                        Message {
                            role: Role::User,
                            content: invalid
                        },
                    ]
                )
                .await
                .is_err()
        );
        assert!(manager.store.list_turns_for_thread(&empty.id)?.is_empty());
        assert!(
            manager
                .get_thread(&empty.id)
                .await?
                .latest_turn_id
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn runtime_image_stored_retry_keeps_five_mib_bytes_without_relaxing_route_policy()
    -> Result<()> {
        let dir = tempfile::tempdir()?;
        let config = config();
        let manager = RuntimeThreadManager::open(
            config,
            dir.path().to_path_buf(),
            test_manager_config(dir.path().join("runtime")),
        )?;
        let cases = [
            vec![
                crate::image_attach::test_support::runtime_image_fixture_bytes(4 * 1024 * 1024 + 1),
            ],
            vec![
                crate::image_attach::test_support::runtime_image_fixture_bytes(3 * 1024 * 1024);
                2
            ],
            vec![runtime_image_fixture(5); 11],
        ];
        for expected in cases {
            let thread = manager.create_thread(Default::default()).await?;
            let mut content = crate::image_attach::prepare_stored_images(&expected)?;
            content.insert(
                0,
                ContentBlock::Text {
                    text: "stored screenshot".into(),
                    cache_control: None,
                },
            );
            manager
                .seed_thread_from_messages(
                    &thread.id,
                    &[Message {
                        role: Role::User,
                        content,
                    }],
                )
                .await?;
            let (fork, prompt, images, _) = manager.fork_at_user_message(&thread.id, 0).await?;
            assert_eq!(images, expected);
            let request = StartTurnRequest {
                prompt: prompt.unwrap(),
                images,
                ..Default::default()
            };
            assert!(manager.start_turn(&fork.id, request.clone()).await.is_err());
            let mut unsupported = request.clone();
            unsupported.model = Some("deepseek-v4-pro".into());
            assert!(
                manager
                    .start_turn_from_stored_images(&fork.id, unsupported)
                    .await
                    .is_err()
            );
            assert!(!manager.active.lock().await.engines.contains_key(&fork.id));
            let mut harness = install_mock_engine(&manager, &fork.id).await;
            let turn = manager
                .start_turn_from_stored_images(&fork.id, request.clone())
                .await?;
            let Some(Op::SendMessage(TurnSpec { images, .. })) = harness.rx_op.recv().await else {
                bail!("expected stored image retry");
            };
            assert_eq!(images, request.images);
            assert_eq!(turn.schema_version, IMAGE_RUNTIME_SCHEMA_VERSION);
            // The next fork shares this workspace. Finish the mock normally
            // and observe its durable settlement before restoring that case.
            for event in [
                EngineEvent::TurnStarted {
                    turn_id: turn.id.clone(),
                    created_at: Utc::now(),
                    route: None,
                    submission_id: None,
                },
                EngineEvent::MessageStarted { index: 0 },
                EngineEvent::MessageDelta {
                    index: 0,
                    content: "stored image fixture response".into(),
                },
                EngineEvent::MessageComplete { index: 0 },
                EngineEvent::TurnComplete {
                    turn_id: None,
                    submission_id: None,
                    compaction_id: None,
                    usage: Usage::default(),
                    parent_route_usage: Usage::default(),
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Completed,
                    error: None,
                    tool_catalog: None,
                    base_url: None,
                },
            ] {
                harness.tx_event.send(event).await?;
            }
            assert_eq!(
                wait_for_terminal_turn(&manager, &turn.id).await?.status,
                RuntimeTurnStatus::Completed
            );
        }
        Ok(())
    }
}

#[test]
fn runtime_image_native_fingerprint_keeps_legacy_text_and_binds_bytes() -> Result<()> {
    let thread = sample_thread("thr_image_legacy");
    let policy = RuntimePolicyProjection::from_persisted("agent", Some("ask"), false);
    let fingerprint = |images: &[codewhale_protocol::runtime::RuntimeImageInput]| {
        runtime_turn_request_fingerprint(
            &thread,
            "hello",
            None,
            "fixture-model",
            None,
            None,
            policy,
            false,
            false,
            &[],
            None,
            images,
            None,
        )
    };
    // SHA-256 of the canonical committed version-1 text request fields.
    assert_eq!(
        fingerprint(&[])?,
        "4dd89138dfd3bfdcffbfa4fed71664e98f23e40a8b46ee6a4829871d94103d67"
    );
    let one = crate::image_attach::test_support::runtime_image_fixture(1);
    let two = crate::image_attach::test_support::runtime_image_fixture(2);
    assert_ne!(
        fingerprint(std::slice::from_ref(&one))?,
        fingerprint(std::slice::from_ref(&two))?
    );
    assert_ne!(
        fingerprint(&[one.clone(), two.clone()])?,
        fingerprint(&[two, one])?
    );
    let absent: StartTurnRequest = serde_json::from_value(json!({"prompt":"hello"}))?;
    let empty: StartTurnRequest = serde_json::from_value(json!({"prompt":"hello","images":[]}))?;
    assert_eq!(serde_json::to_value(absent)?, serde_json::to_value(empty)?);
    Ok(())
}

#[test]
fn output_cap_wire_requires_positive_integer_and_preserves_legacy_absence() -> Result<()> {
    for invalid in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!("1500"),
        json!(4_294_967_296u64),
        json!(true),
    ] {
        let input = json!({"prompt":"review", "maxOutputTokens":invalid});
        assert!(
            serde_json::from_value::<StartTurnRequest>(input.clone()).is_err(),
            "{input}"
        );
        assert!(serde_json::from_value::<codewhale_protocol::PromptRequest>(input).is_err());
    }
    let absent: StartTurnRequest = serde_json::from_value(json!({"prompt":"review"}))?;
    assert!(
        serde_json::to_value(absent)?
            .get("maxOutputTokens")
            .is_none()
    );
    let valid: StartTurnRequest =
        serde_json::from_value(json!({"prompt":"review","maxOutputTokens":1500}))?;
    assert_eq!(valid.max_output_tokens.unwrap().get(), 1500);
    Ok(())
}

/// Notice projection (#6180): engine events raise watchable notices with
/// thread/turn identity; elevation clears when its tool call completes and
/// the rest clear on ack.
#[tokio::test]
async fn notices_raise_from_engine_events_and_clear_on_settle_or_ack() -> Result<()> {
    use crate::tools::subagent::{AgentWorkerSpec, AgentWorkerToolProfile, SubAgentManager};
    use codewhale_agent_contract::role::FleetRole;
    use codewhale_agent_contract::worker_profile::WorkerRuntimeProfile;

    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            model: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: Some(true),
            auto_approve: Some(true),
            archived: false,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await?;
    let mut harness = mock_engine_handle();
    harness
        .handle
        .set_subagent_manager(Arc::new(tokio::sync::RwLock::new(SubAgentManager::new(
            thread.workspace.clone(),
            1,
        ))));
    manager
        .install_test_engine(&thread.id, harness.handle.clone())
        .await?;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "raise notices".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    harness
        .handle
        .subagent_manager()
        .write()
        .await
        .register_worker_for_session_with_origin(
            AgentWorkerSpec {
                worker_id: "agent_done".into(),
                run_id: "agent_done".into(),
                parent_run_id: None,
                workflow_run_id: None,
                session_name: None,
                objective: "fixture".into(),
                role: None,
                agent_type: FleetRole::Verifier,
                model: "fixture".into(),
                workspace: thread.workspace.clone(),
                git_branch: None,
                context_mode: "fresh".into(),
                fork_context: false,
                tool_profile: AgentWorkerToolProfile::Inherited,
                runtime_profile: WorkerRuntimeProfile::default(),
                max_steps: 2,
                spawn_depth: 1,
                max_spawn_depth: 3,
                child_route: None,
                launch_manifest: None,
            },
            &thread.id,
            None,
            Some(turn.id.clone()),
        );
    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: turn.id.clone(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;

    // Event projection runs on the engine task; wait (bounded) for each
    // notice rather than sleeping a fixed span.
    async fn wait_for_notices(
        manager: &RuntimeThreadManager,
        thread_id: &str,
        count: usize,
    ) -> Vec<ActiveNotice> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let notices = manager.list_notices(thread_id);
            if notices.len() == count || tokio::time::Instant::now() >= deadline {
                return notices;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    harness
        .tx_event
        .send(EngineEvent::AgentComplete {
            display_name: None,
            owner_session_id: thread.id.clone(),
            id: "agent_done".to_string(),
            result: "did the thing".to_string(),
            outcome: Some(crate::tools::subagent::SubAgentStatus::Completed),
            parent_run_id: None,
            spawn_depth: None,
            continuable: None,
            usage: Some(crate::tools::subagent::AgentRunUsage {
                status: "completed".to_string(),
                input_tokens: Some(800),
                output_tokens: Some(200),
                total_tokens: Some(1000),
                cost_microusd: Some(50),
                note: String::new(),
            }),
        })
        .await?;
    let notices = wait_for_notices(&manager, &thread.id, 1).await;
    assert_eq!(notices.len(), 1, "agent completion raises: {notices:?}");
    // #6315: the completion receipt's usage persists on the event payload
    // so metrics can answer where the child tokens went.
    let events = manager.events_since(&thread.id, None)?;
    let completed = events
        .iter()
        .find(|event| event.event == "agent.completed")
        .expect("agent.completed persisted");
    assert_eq!(completed.payload["usage"]["total_tokens"], 1000);
    assert_eq!(completed.payload["usage"]["cost_microusd"], 50);

    harness
        .tx_event
        .send(EngineEvent::ToolCallComplete {
            model_call: None,
            id: "tool_notify_1".to_string(),
            name: "notify".to_string(),
            result: Ok(crate::tools::spec::ToolResult::success("pinged")),
        })
        .await?;
    let notices = wait_for_notices(&manager, &thread.id, 2).await;
    assert_eq!(notices.len(), 2, "model notify raises: {notices:?}");

    harness
        .tx_event
        .send(EngineEvent::ElevationRequired {
            tool_id: "tool_needs_elev".to_string(),
            tool_name: "exec_command".to_string(),
            command: None,
            denial_reason: "sandbox denied".to_string(),
            blocked_network: false,
            blocked_write: false,
        })
        .await?;
    assert!(matches!(
        harness.recv_approval_event().await,
        Some(crate::core::engine::MockApprovalEvent::RetryWithPolicy { .. })
    ));
    let notices = wait_for_notices(&manager, &thread.id, 3).await;
    let kinds: Vec<&str> = notices.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(notices.len(), 3, "all three kinds raise: {kinds:?}");
    assert!(kinds.contains(&"elevation-needed"));
    assert!(kinds.contains(&"subagent-terminal"));
    assert!(kinds.contains(&"model-notify"));
    assert!(notices.iter().all(|n| n.turn_id == turn.id));
    let notify_id = notices
        .iter()
        .find(|n| n.kind == "model-notify")
        .map(|n| n.id.clone())
        .context("missing model-notify")?;

    // The elevation question ends when its tool call completes.
    harness
        .tx_event
        .send(EngineEvent::ToolCallComplete {
            model_call: None,
            id: "tool_needs_elev".to_string(),
            name: "exec_command".to_string(),
            result: Ok(crate::tools::spec::ToolResult::success("elevated ok")),
        })
        .await?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let notices = manager.list_notices(&thread.id);
        if notices.len() == 2 || tokio::time::Instant::now() >= deadline {
            assert_eq!(notices.len(), 2, "elevation clears on settle");
            assert!(notices.iter().all(|n| n.kind != "elevation-needed"));
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Terminal/notify kinds clear on ack; unknown acks report false.
    assert!(manager.ack_notice(&thread.id, &notify_id));
    assert!(!manager.ack_notice(&thread.id, &notify_id));
    assert!(!manager.ack_notice(&thread.id, "notice_nope"));
    assert_eq!(manager.list_notices(&thread.id).len(), 1);

    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn shell_opt_in_is_idle_only_and_preserves_ask() -> Result<()> {
    let _env = crate::test_support::lock_test_env();
    let _shell_env = [
        crate::test_support::EnvVarGuard::remove("CODEWHALE_ALLOW_SHELL"),
        crate::test_support::EnvVarGuard::remove("DEEPSEEK_ALLOW_SHELL"),
    ];
    let dir = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let workspace = dir.path().join("workspace");
    fs::create_dir(&workspace)?;
    let config_path = dir.path().join("config.toml");
    fs::write(&config_path, "allow_shell = false\n")?;
    let manager = test_manager(dir.path().join("runtime"))?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace),
            allow_shell: Some(false),
            permission_posture: Some("ask".into()),
            ..Default::default()
        })
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    manager
        .active
        .lock()
        .await
        .engines
        .get_mut(&thread.id)
        .unwrap()
        .active_turn = Some(ActiveTurnState {
        narrowing: crate::core::engine::TurnNarrowing::Inherit,
        goal_progress: None,
        goal_id: None,
        turn_id: "turn_shell_guard".into(),
        interrupt_requested: false,
        compaction_id: None,
    });
    let error = manager
        .update_thread_with_shell_policy(
            &thread.id,
            UpdateThreadRequest {
                allow_shell: Some(true),
                title: Some("must not persist".into()),
                ..Default::default()
            },
            Some(&config_path),
            None,
        )
        .await
        .expect_err("active turn must reject opt-in");
    assert!(error.to_string().contains("already has an active turn"));
    let unchanged = manager.store.load_thread(&thread.id)?;
    assert!(!unchanged.allow_shell);
    assert_eq!(unchanged.title, thread.title);
    assert_eq!(unchanged.updated_at, thread.updated_at);
    assert!(harness.rx_op.try_recv().is_err());
    manager
        .active
        .lock()
        .await
        .engines
        .get_mut(&thread.id)
        .unwrap()
        .active_turn = None;
    let enabled = manager
        .update_thread_with_shell_policy(
            &thread.id,
            UpdateThreadRequest {
                allow_shell: Some(true),
                ..Default::default()
            },
            Some(&config_path),
            None,
        )
        .await?;
    assert!(enabled.allow_shell);
    assert_eq!(enabled.permission_posture.as_deref(), Some("ask"));
    assert!(!enabled.auto_approve && !enabled.trust_mode);
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::ChangeMode {
            allow_shell: true,
            auto_approve: false,
            approval_mode: ApprovalMode::Suggest,
            ..
        })
    ));
    manager
        .active
        .lock()
        .await
        .engines
        .get_mut(&thread.id)
        .unwrap()
        .active_turn = Some(ActiveTurnState {
        narrowing: crate::core::engine::TurnNarrowing::Inherit,
        goal_progress: None,
        goal_id: None,
        turn_id: "turn_revoke".into(),
        interrupt_requested: false,
        compaction_id: None,
    });
    let disabled = manager
        .update_thread_with_shell_policy(
            &thread.id,
            UpdateThreadRequest {
                allow_shell: Some(false),
                ..Default::default()
            },
            Some(&config_path),
            None,
        )
        .await?;
    assert!(
        !disabled.allow_shell,
        "tightening remains available during a turn"
    );
    Ok(())
}

#[tokio::test]
async fn shell_policy_uses_explicit_profile_and_actual_thread_workspace() -> Result<()> {
    let _env = crate::test_support::lock_test_env();
    let _shell_env = [
        crate::test_support::EnvVarGuard::remove("CODEWHALE_ALLOW_SHELL"),
        crate::test_support::EnvVarGuard::remove("DEEPSEEK_ALLOW_SHELL"),
    ];
    let dir = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let workspace = dir.path().join("thread-workspace");
    fs::create_dir(&workspace)?;
    let config_path = dir.path().join("explicit.toml");
    fs::write(
        &config_path,
        "allow_shell = true\n[profiles.restricted]\nallow_shell = false\n",
    )?;
    let manager = test_manager(dir.path().join("runtime"))?;
    manager.config.write().allow_shell = Some(false);
    let thread = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace.clone()),
            allow_shell: Some(false),
            ..Default::default()
        })
        .await?;
    let error = manager
        .update_thread_with_shell_policy(
            &thread.id,
            UpdateThreadRequest {
                allow_shell: Some(true),
                ..Default::default()
            },
            Some(&config_path),
            Some("restricted"),
        )
        .await
        .expect_err("profile denial must win");
    assert!(error.to_string().contains("active config profile"));
    assert!(!manager.store.load_thread(&thread.id)?.allow_shell);
    // This same policy check is used by direct job launch, so historical
    // opt-ins cannot bypass a subsequently controlled denial.
    assert!(
        manager
            .validate_shell_access_policy(&workspace, Some(&config_path), Some("restricted"))
            .await
            .is_err()
    );
    manager.config.write().allow_shell = Some(true);
    let project = workspace.join(codewhale_config::CODEWHALE_APP_DIR);
    fs::create_dir(&project)?;
    fs::write(project.join("config.toml"), "allow_shell = false\n")?;
    let error = manager
        .validate_shell_access_policy(&workspace, Some(&config_path), None)
        .await
        .expect_err("thread folder beats host merged true");
    assert!(error.to_string().contains("project configuration"));
    let managed = dir.path().join("managed.toml");
    fs::write(&managed, "allow_shell = false\n")?;
    manager.config.write().managed_config_path = Some(managed.to_string_lossy().into_owned());
    manager.config.write().allow_shell = Some(false);
    assert!(
        manager
            .validate_shell_access_policy(dir.path(), Some(&config_path), None)
            .await
            .unwrap_err()
            .to_string()
            .contains("managed configuration")
    );
    // Ambiguous/unreadable managed authority fails closed even if a stale
    // effective snapshot says true.
    fs::write(&managed, "[broken")?;
    manager.config.write().allow_shell = Some(true);
    assert!(
        manager
            .validate_shell_access_policy(dir.path(), Some(&config_path), None)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn unset_thread_shell_takes_the_interactive_default_unless_policy_denies() -> Result<()> {
    let _env = crate::test_support::lock_test_env();
    let _shell_env = [
        crate::test_support::EnvVarGuard::remove("CODEWHALE_ALLOW_SHELL"),
        crate::test_support::EnvVarGuard::remove("DEEPSEEK_ALLOW_SHELL"),
    ];
    let dir = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let workspace = dir.path().join("thread-workspace");
    fs::create_dir(&workspace)?;
    let config_path = dir.path().join("config.toml");
    fs::write(&config_path, "[profiles.restricted]\nallow_shell = false\n")?;
    let manager = test_manager(dir.path().join("runtime"))?;
    manager.config.write().allow_shell = None;

    // Unset: an app-created conversation gets approval-gated shell.
    let open = manager
        .create_thread_with_shell_policy(
            CreateThreadRequest {
                workspace: Some(workspace.clone()),
                ..Default::default()
            },
            Some(&config_path),
            None,
        )
        .await?;
    assert!(
        open.allow_shell,
        "unset allow_shell takes the interactive default"
    );

    // An explicit value is still honored exactly.
    let explicit = manager
        .create_thread_with_shell_policy(
            CreateThreadRequest {
                workspace: Some(workspace.clone()),
                allow_shell: Some(false),
                ..Default::default()
            },
            Some(&config_path),
            None,
        )
        .await?;
    assert!(!explicit.allow_shell);

    // A profile-sourced setting wins over the default at creation. The host's
    // merged snapshot is left unset, so `validate_shell_access_policy` reads a
    // profile, environment or managed source as a denial whatever value that
    // source sets; this pins the gate, not the value read from the profile.
    let profile_denied = manager
        .create_thread_with_shell_policy(
            CreateThreadRequest {
                workspace: Some(workspace.clone()),
                ..Default::default()
            },
            Some(&config_path),
            Some("restricted"),
        )
        .await?;
    assert!(!profile_denied.allow_shell);

    // A managed source likewise denies while the host's merged snapshot is
    // unset. (With the host snapshot at `Some(true)` it would be allowed;
    // that is main's existing behavior and not what this test pins.)
    let managed = dir.path().join("managed.toml");
    fs::write(&managed, "allow_shell = false\n")?;
    manager.config.write().managed_config_path = Some(managed.to_string_lossy().into_owned());
    let managed_denied = manager
        .create_thread_with_shell_policy(
            CreateThreadRequest {
                workspace: Some(workspace.clone()),
                ..Default::default()
            },
            Some(&config_path),
            None,
        )
        .await?;
    assert!(!managed_denied.allow_shell);
    manager.config.write().managed_config_path = None;

    // A project-local `allow_shell = false` in the thread's own folder wins
    // even when the host's merged config would allow shell.
    let project = workspace.join(codewhale_config::CODEWHALE_APP_DIR);
    fs::create_dir(&project)?;
    fs::write(project.join("config.toml"), "allow_shell = false\n")?;
    let project_denied = manager
        .create_thread_with_shell_policy(
            CreateThreadRequest {
                workspace: Some(workspace.clone()),
                ..Default::default()
            },
            Some(&config_path),
            None,
        )
        .await?;
    assert!(!project_denied.allow_shell);

    // An explicit opt-in runs the same check and is refused, exactly as a
    // PATCH opt-in is, instead of bypassing the project restriction.
    let refused = manager
        .create_thread_with_shell_policy(
            CreateThreadRequest {
                workspace: Some(workspace.clone()),
                allow_shell: Some(true),
                ..Default::default()
            },
            Some(&config_path),
            None,
        )
        .await
        .expect_err("explicit allow_shell=true must not bypass a project restriction");
    assert!(
        refused
            .to_string()
            .contains("shell commands are restricted"),
        "unexpected error: {refused}"
    );
    Ok(())
}

/// The thread summary's preview is read through `newest_message_text_by_turn`,
/// so pin the selection rules it depends on: non-message items never win, an
/// empty trailing message is skipped rather than reported, and a missing
/// `started_at` reads as newest because that is what `sort_turn_items_by_start`
/// does when it substitutes `Utc::now()`.
#[test]
fn newest_message_text_by_turn_picks_the_latest_message_ignoring_non_messages() {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone()).expect("open store");
    let turn_id = "trn_preview".to_string();
    let at = |seconds: i64| Some(Utc::now() - chrono::Duration::seconds(seconds));

    let message = |item_id: &str, text: &str, started_at| {
        let mut item = sample_item(&turn_id, item_id, TurnItemLifecycleStatus::Completed);
        item.kind = TurnItemKind::AgentMessage;
        item.summary = text.to_string();
        item.detail = Some(text.to_string());
        item.started_at = started_at;
        item
    };

    // A late tool call is not a candidate; a later empty message is skipped.
    let mut tool = sample_item(&turn_id, "itm_tool", TurnItemLifecycleStatus::Completed);
    tool.kind = TurnItemKind::ToolCall;
    tool.started_at = at(5);

    for item in [
        message("itm_old", "oldest", at(30)),
        tool,
        message("itm_mid", "newer", at(20)),
        message("itm_blank", "   ", at(10)),
        message("itm_undated", "undated", None),
    ] {
        store.save_item(&item).expect("save item");
    }

    let found = store
        .newest_message_text_by_turn(std::slice::from_ref(&turn_id))
        .expect("scan item store");
    assert_eq!(
        found.get(&turn_id).map(String::as_str),
        Some("undated"),
        "expected the newest non-empty message; got {found:?}"
    );

    // A turn with no message at all has no preview text, so the caller falls
    // back to an older turn rather than reporting an empty row.
    let empty = store
        .newest_message_text_by_turn(&["trn_absent".to_string()])
        .expect("scan item store");
    assert!(empty.is_empty(), "expected no preview text; got {empty:?}");

    let _ = std::fs::remove_dir_all(dir);
}

/// The thread rail's preview is read from each row's own newest turn.
///
/// The batch read this replaced walked the whole items directory to answer the
/// same question, which on a 658MB / 58k-item store cost ~1.4s on every
/// summary. The row still has to carry the newest message the turn appended —
/// not the first one it wrote, and not a trailing status or tool record.
#[tokio::test]
async fn thread_list_facts_reads_each_preview_from_its_own_turn() -> Result<()> {
    let dir = test_runtime_dir();
    let manager = test_manager(dir.clone())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(dir.clone()),
            ..CreateThreadRequest::default()
        })
        .await?;

    let message = |turn_id: &str, item_id: &str, kind: TurnItemKind, text: &str| {
        let mut item = sample_item(turn_id, item_id, TurnItemLifecycleStatus::Completed);
        item.kind = kind;
        item.summary = text.to_string();
        item.detail = Some(text.to_string());
        item
    };

    let older_at = Utc::now() - chrono::Duration::seconds(60);
    let mut older = sample_turn(&thread.id, "trn_older", RuntimeTurnStatus::Completed);
    older.created_at = older_at;
    older.started_at = Some(older_at);
    let newer_at = Utc::now();
    let mut newer = sample_turn(&thread.id, "trn_newer", RuntimeTurnStatus::Completed);
    newer.created_at = newer_at;
    newer.started_at = Some(newer_at);

    // The older turn's reply must not win, and the newer turn's own prompt has
    // to lose to its reply: both are earlier in the turn's append order.
    let older_items = [
        message(
            "trn_older",
            "itm_older_prompt",
            TurnItemKind::UserMessage,
            "older prompt",
        ),
        message(
            "trn_older",
            "itm_older_reply",
            TurnItemKind::AgentMessage,
            "older reply",
        ),
    ];
    let mut newer_items = [
        message(
            "trn_newer",
            "itm_newer_prompt",
            TurnItemKind::UserMessage,
            "newest prompt",
        ),
        message(
            "trn_newer",
            "itm_newer_reply",
            TurnItemKind::AgentMessage,
            "newest reply",
        ),
        // A trailing blank message and a trailing non-message both have to be
        // stepped over: the walk ends on the reply above, not on either of
        // these, and not on the three records after them.
        message(
            "trn_newer",
            "itm_newer_blank",
            TurnItemKind::AgentMessage,
            "   ",
        ),
        sample_item(
            "trn_newer",
            "itm_newer_tool",
            TurnItemLifecycleStatus::Completed,
        ),
        sample_item(
            "trn_newer",
            "itm_newer_status",
            TurnItemLifecycleStatus::Completed,
        ),
    ];
    newer_items.iter_mut().for_each(|item| {
        item.started_at = Some(newer_at);
    });
    older.item_ids = older_items.iter().map(|item| item.id.clone()).collect();
    newer.item_ids = newer_items.iter().map(|item| item.id.clone()).collect();

    for item in older_items.iter().chain(newer_items.iter()) {
        manager.store.save_item(item)?;
    }
    manager.store.save_turn(&older)?;
    manager.store.save_turn(&newer)?;

    // A second thread whose items the walk must never need. If the rail went
    // back to scanning the items directory, the read count below would cover
    // these too.
    let decoy = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut decoy_turn = sample_turn(&decoy.id, "trn_decoy", RuntimeTurnStatus::Completed);
    decoy_turn.item_ids = vec!["itm_decoy".to_string()];
    manager.store.save_item(&message(
        "trn_decoy",
        "itm_decoy",
        TurnItemKind::AgentMessage,
        "decoy reply",
    ))?;
    manager.store.save_turn(&decoy_turn)?;

    manager.reset_whole_store_scan_file_reads();
    let facts = manager
        .thread_list_facts(std::slice::from_ref(&thread.id))
        .await?;
    let (turn_reads, item_reads) = manager.whole_store_scan_file_reads();

    assert_eq!(
        item_reads, 0,
        "a thread list must not walk the items directory to fill a preview"
    );
    assert!(
        turn_reads >= 2,
        "every turn record is still read: {turn_reads}"
    );
    assert_eq!(
        facts[&thread.id].preview.as_deref(),
        Some("newest reply"),
        "expected the last message the newest turn appended"
    );

    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

/// A newest turn that holds no message at all falls back to an older turn,
/// exactly as the batch read did: the row would otherwise lose its preview.
#[tokio::test]
async fn thread_list_facts_falls_back_to_an_older_turn_for_a_preview() -> Result<()> {
    let dir = test_runtime_dir();
    let manager = test_manager(dir.clone())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(dir.clone()),
            ..CreateThreadRequest::default()
        })
        .await?;

    let older_at = Utc::now() - chrono::Duration::seconds(60);
    let mut older = sample_turn(&thread.id, "trn_older", RuntimeTurnStatus::Completed);
    older.created_at = older_at;
    older.started_at = Some(older_at);
    let mut reply = sample_item("trn_older", "itm_reply", TurnItemLifecycleStatus::Completed);
    reply.kind = TurnItemKind::AgentMessage;
    reply.summary = "older reply".to_string();
    reply.detail = Some("older reply".to_string());
    older.item_ids = vec![reply.id.clone()];

    // The newest turn is a status-only record: a routing settlement, or a turn
    // whose items never carried a message.
    let newer_at = Utc::now();
    let mut newer = sample_turn(&thread.id, "trn_newer", RuntimeTurnStatus::Completed);
    newer.created_at = newer_at;
    newer.started_at = Some(newer_at);
    newer.item_ids = vec!["itm_status".to_string()];

    manager.store.save_item(&reply)?;
    manager.store.save_item(&sample_item(
        "trn_newer",
        "itm_status",
        TurnItemLifecycleStatus::Completed,
    ))?;
    manager.store.save_turn(&older)?;
    manager.store.save_turn(&newer)?;

    manager.reset_whole_store_scan_file_reads();
    let facts = manager
        .thread_list_facts(std::slice::from_ref(&thread.id))
        .await?;
    let (_, item_reads) = manager.whole_store_scan_file_reads();

    assert_eq!(
        item_reads, 0,
        "the fallback still reads turns, not the store"
    );
    assert_eq!(facts[&thread.id].preview.as_deref(), Some("older reply"));

    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

/// A turn written before `item_ids` existed has no item list to read by, so the
/// summary still reaches it through the directory scan that answered every turn
/// before this change — one pass for the page, and the thread's walk resumes
/// from the turn it stopped on.
#[tokio::test]
async fn thread_list_facts_still_answers_a_turn_without_item_ids() -> Result<()> {
    let dir = test_runtime_dir();
    let manager = test_manager(dir.clone())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(dir.clone()),
            ..CreateThreadRequest::default()
        })
        .await?;

    let mut turn = sample_turn(&thread.id, "trn_legacy", RuntimeTurnStatus::Completed);
    turn.item_ids = Vec::new();
    let mut reply = sample_item(
        "trn_legacy",
        "itm_legacy_reply",
        TurnItemLifecycleStatus::Completed,
    );
    reply.kind = TurnItemKind::AgentMessage;
    reply.summary = "legacy reply".to_string();
    reply.detail = Some("legacy reply".to_string());
    manager.store.save_item(&reply)?;
    manager.store.save_turn(&turn)?;

    manager.reset_whole_store_scan_file_reads();
    let facts = manager
        .thread_list_facts(std::slice::from_ref(&thread.id))
        .await?;
    let (_, item_reads) = manager.whole_store_scan_file_reads();

    assert!(
        item_reads > 0,
        "a turn with no item list can only be found by scanning"
    );
    assert_eq!(facts[&thread.id].preview.as_deref(), Some("legacy reply"));

    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

/// Opening threads costs one items-directory read per store, not one per open,
/// and the later opens still answer with the same items.
#[test]
fn list_items_for_turns_map_reads_the_items_directory_once_per_store() {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone()).expect("open store");

    let mut saved = Vec::new();
    for turn in 0..3 {
        for position in 0..4 {
            let id = format!("itm_{turn}_{position}");
            saved.push(id.clone());
            store
                .save_item(&sample_item(
                    &format!("trn_{turn}"),
                    &id,
                    TurnItemLifecycleStatus::Completed,
                ))
                .expect("save item");
        }
    }
    let turn_ids: Vec<String> = (0..3).map(|turn| format!("trn_{turn}")).collect();
    let read_items = |map: &HashMap<String, Vec<TurnItemRecord>>| {
        let mut ids: Vec<String> = map.values().flatten().map(|item| item.id.clone()).collect();
        ids.sort();
        ids
    };

    store
        .item_dir_files_read
        .store(0, std::sync::atomic::Ordering::SeqCst);
    let first = store
        .list_items_for_turns_map(&turn_ids)
        .expect("first read");
    assert_eq!(
        store
            .item_dir_files_read
            .load(std::sync::atomic::Ordering::SeqCst),
        saved.len() as u64,
        "the first read of a store has to find its items the only way there is"
    );

    store
        .item_dir_files_read
        .store(0, std::sync::atomic::Ordering::SeqCst);
    let second = store
        .list_items_for_turns_map(&turn_ids)
        .expect("second read");
    assert_eq!(
        store
            .item_dir_files_read
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a later read must come off the index, not the directory"
    );

    let ids = read_items(&first);
    assert_eq!(ids, saved);
    assert_eq!(read_items(&second), saved);

    let _ = std::fs::remove_dir_all(dir);
}

/// What the server's startup warm-up buys: the first thread a client opens no
/// longer pays the whole-directory read.
#[test]
fn a_warmed_store_opens_a_thread_without_reading_the_items_directory() {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone()).expect("open store");
    let turn_id = "trn_warm".to_string();
    store
        .save_item(&sample_item(
            &turn_id,
            "itm_warm",
            TurnItemLifecycleStatus::Completed,
        ))
        .expect("save item");

    store.ensure_item_index().expect("warm the index");
    store
        .item_dir_files_read
        .store(0, std::sync::atomic::Ordering::SeqCst);

    let items = store
        .list_items_for_turns_map(std::slice::from_ref(&turn_id))
        .expect("read a thread's items");
    assert_eq!(items[&turn_id].len(), 1);
    assert_eq!(
        store
            .item_dir_files_read
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a warmed store must answer from the index alone"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// The index is exact, not a snapshot: an item the store holds is returned
/// whether or not the turn ever registered it, and whether it was written
/// before or after the index was built.
///
/// An item that lands after its turn settled is deliberately never pushed into
/// that turn's `item_ids` (`attach_item_to_turn`), and the rebuild paths read it
/// out of the directory anyway. A read that trusted `item_ids` would lose it —
/// which is the whole reason the index exists rather than a narrower lookup.
#[test]
fn item_reads_include_items_a_turn_never_registered() {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone()).expect("open store");
    let turn_id = "trn_unregistered".to_string();

    // The first item is what a read finds before the index exists.
    store
        .save_item(&sample_item(
            &turn_id,
            "itm_before_index",
            TurnItemLifecycleStatus::Completed,
        ))
        .expect("save item");
    let seeded = store
        .list_items_for_turns_map(std::slice::from_ref(&turn_id))
        .expect("seed the index");
    assert_eq!(seeded[&turn_id].len(), 1);

    // And this one is written after it, registered by nothing.
    store
        .save_item(&sample_item(
            &turn_id,
            "itm_after_index",
            TurnItemLifecycleStatus::Completed,
        ))
        .expect("save late item");
    let read = store
        .list_items_for_turns_map(std::slice::from_ref(&turn_id))
        .expect("read after the write");
    let mut ids: Vec<String> = read[&turn_id].iter().map(|item| item.id.clone()).collect();
    ids.sort();
    assert_eq!(ids, vec!["itm_after_index", "itm_before_index"]);

    let _ = std::fs::remove_dir_all(dir);
}

/// A write that races the directory read is recorded, and the published map
/// drains that record once — not twice, even though the read also saw the file
/// on disk.
///
/// Holding the seed guard is what a running read does, so the write below takes
/// the recorded branch rather than the map's. Without it the branch is only
/// reachable by timing, and a duplicate would show up as the same item twice in
/// a thread's transcript.
#[test]
fn an_item_written_while_the_index_is_being_built_is_published_once() {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone()).expect("open store");
    let turn_id = "trn_racing_write".to_string();

    let seed = store.item_index_seed.lock();
    store
        .save_item(&sample_item(
            &turn_id,
            "itm_racing_write",
            TurnItemLifecycleStatus::Completed,
        ))
        .expect("save an item while a read is running");
    drop(seed);

    let read = store
        .list_items_for_turns_map(std::slice::from_ref(&turn_id))
        .expect("read after the write");
    let ids: Vec<&str> = read[&turn_id].iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, vec!["itm_racing_write"]);

    let _ = std::fs::remove_dir_all(dir);
}

/// The runtime saves one item id several times (in progress, then completed or
/// failed). Once the index is warm, every one of those saves reaches the
/// published map, and the item must still come back once, not once per save.
#[test]
fn an_item_saved_again_after_the_index_is_warm_is_listed_once() {
    let dir = test_runtime_dir();
    let store = RuntimeThreadStore::open(dir.clone()).expect("open store");
    let turn_id = "trn_resaved".to_string();

    store.ensure_item_index().expect("warm the index");
    for status in [
        TurnItemLifecycleStatus::InProgress,
        TurnItemLifecycleStatus::Completed,
    ] {
        store
            .save_item(&sample_item(&turn_id, "itm_resaved", status))
            .expect("save item");
    }
    // A batch write of the same id takes the same path.
    let again = sample_item(&turn_id, "itm_resaved", TurnItemLifecycleStatus::Completed);
    store.save_items_batch(&[&again]).expect("batch save item");

    let read = store
        .list_items_for_turns_map(std::slice::from_ref(&turn_id))
        .expect("read the turn");
    let items = &read[&turn_id];
    assert_eq!(items.len(), 1, "one item id, one item");
    assert_eq!(items[0].status, TurnItemLifecycleStatus::Completed);

    let _ = std::fs::remove_dir_all(dir);
}

/// A turn can still name an item whose file was removed. The thread list skips
/// that id and keeps looking, instead of failing the whole summary page.
#[tokio::test]
async fn thread_list_facts_skips_an_item_file_that_is_gone() -> Result<()> {
    let dir = test_runtime_dir();
    let manager = test_manager(dir.clone())?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(dir.clone()),
            ..CreateThreadRequest::default()
        })
        .await?;

    let mut turn = sample_turn(&thread.id, "trn_gone", RuntimeTurnStatus::Completed);
    let mut reply = sample_item("trn_gone", "itm_reply", TurnItemLifecycleStatus::Completed);
    reply.kind = TurnItemKind::AgentMessage;
    reply.summary = "kept reply".to_string();
    reply.detail = Some("kept reply".to_string());
    turn.item_ids = vec![reply.id.clone(), "itm_removed".to_string()];
    manager.store.save_item(&reply)?;
    manager.store.save_turn(&turn)?;

    let facts = manager
        .thread_list_facts(std::slice::from_ref(&thread.id))
        .await?;
    assert_eq!(facts[&thread.id].preview.as_deref(), Some("kept reply"));

    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

/// The summary route settles recovered turns through
/// `flush_recovery_receipts` now that its rows no longer go through
/// `get_thread_detail`. That settled state is what the page's attention count
/// reports, so the flush must drain every listed thread, not just the first.
#[tokio::test]
async fn flush_recovery_receipts_drains_every_listed_thread() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let mut thread_ids = Vec::new();
    for index in 0..2 {
        let thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        let turn = sample_turn(
            &thread.id,
            &format!("turn_flush_{index}"),
            RuntimeTurnStatus::InProgress,
        );
        manager.store.save_turn(&turn)?;
        manager.queue_recovery_receipt(RecoveredTurnReceipt {
            turn,
            unresolved_dynamic_tools: Vec::new(),
        });
        thread_ids.push(thread.id);
    }
    assert_eq!(
        manager.recovery_receipts.lock().len(),
        2,
        "both threads should start with a queued receipt"
    );

    manager.flush_recovery_receipts(&thread_ids).await?;

    let remaining = manager
        .recovery_receipts
        .lock()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        remaining.is_empty(),
        "the flush must settle every listed thread; still queued: {remaining:?}"
    );

    Ok(())
}

#[tokio::test]
async fn engine_plumbing_items_are_tagged_internal_and_retry_hints_are_dropped() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "plumbing visibility fixture".to_string(),
                ..StartTurnRequest::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "engine_plumbing_visibility".to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    for status in [
        "Executing tools sequentially (writes, approvals, or non-parallel tools detected)",
        "Loaded deferred tool 'load_skill'. Retry the call with its visible schema.",
        "Reconnecting…",
    ] {
        harness.tx_event.send(EngineEvent::status(status)).await?;
    }
    harness
        .tx_event
        .send(EngineEvent::ToolCallStarted {
            model_call: Some(crate::core::events::ModelToolCall {
                provider_id: "tool-search-1".to_string(),
                caller: None,
                thought_signature: None,
            }),
            id: "tool-search-1".to_string(),
            name: "tool_search".to_string(),
            input: json!({"query": "skill"}),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ToolCallComplete {
            model_call: None,
            id: "tool-search-1".to_string(),
            name: "tool_search".to_string(),
            result: Ok(crate::tools::spec::ToolResult::success("found load_skill")),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ToolCallStarted {
            model_call: Some(crate::core::events::ModelToolCall {
                provider_id: "hydrate-1".to_string(),
                caller: None,
                thought_signature: None,
            }),
            id: "hydrate-1".to_string(),
            name: "load_skill".to_string(),
            input: json!({}),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ToolCallComplete {
            model_call: None,
            id: "hydrate-1".to_string(),
            name: "load_skill".to_string(),
            result: Ok(
                crate::tools::spec::ToolResult::success("schema loaded").with_metadata(json!({
                    "deferred_tool_loaded": true,
                    "executed": false,
                })),
            ),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    wait_for_terminal_turn(&manager, &turn.id).await?;

    let items = manager.store.list_items_for_turn(&turn.id)?;
    let visibility = |item: &TurnItemRecord| {
        item.metadata
            .as_ref()
            .and_then(|meta| meta.get("visibility"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let statuses = items
        .iter()
        .filter(|item| item.kind == TurnItemKind::Status)
        .map(|item| (item.summary.clone(), visibility(item)))
        .collect::<Vec<_>>();
    assert_eq!(
        statuses,
        vec![
            (
                "Executing tools sequentially (writes, approvals, or non-parallel tools detected)"
                    .to_string(),
                Some("internal".to_string()),
            ),
            ("Reconnecting…".to_string(), None),
        ],
        "the model-facing retry hint must not become a user item"
    );
    for tool in ["tool_search", "load_skill"] {
        let item = items
            .iter()
            .find(|item| {
                item.metadata
                    .as_ref()
                    .and_then(|meta| meta.get("tool_name"))
                    .and_then(Value::as_str)
                    == Some(tool)
            })
            .unwrap_or_else(|| panic!("{tool} item persisted"));
        assert_eq!(visibility(item).as_deref(), Some("internal"), "{tool}");
    }
    Ok(())
}

/// #6418: a refused session switch must say *which* guard refused. One case
/// per `StoreAdoptionRefusal` variant, each built from a real store so the
/// layout under test is the product's.
mod adoption_refusal {
    use super::*;
    use crate::automation_manager::{AutomationManager, AutomationStatus, CreateAutomationRequest};

    /// Fields drop in declaration order: the env guards restore before the
    /// env lock releases.
    struct Fixture {
        _home: crate::test_support::EnvVarGuard,
        _runtime: crate::test_support::EnvVarGuard,
        _legacy: crate::test_support::EnvVarGuard,
        root: tempfile::TempDir,
        _env: crate::test_support::TestEnvLock,
    }

    fn fixture() -> Result<Fixture> {
        let env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir()?;
        let home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        Ok(Fixture {
            root,
            _env: env,
            _home: home,
            _runtime: runtime,
            _legacy: legacy,
        })
    }

    fn opened_binding(
        fixture: &Fixture,
        session: &str,
        scope: &str,
    ) -> Result<RuntimeStoreBinding> {
        let data_dir = fixture
            .root
            .path()
            .join("sessions")
            .join(session)
            .join("runtime");
        drop(RuntimeThreadStore::open(data_dir.clone())?);
        Ok(RuntimeStoreBinding {
            data_dir,
            execution_scope: scope.to_string(),
        })
    }

    #[test]
    fn empty_unheld_store_has_no_refusal() -> Result<()> {
        let fixture = fixture()?;
        let binding = opened_binding(&fixture, "adoptable", &"0".repeat(64))?;
        assert_eq!(binding.adoption_refusal()?, None);
        assert!(binding.is_adoptable_empty_store()?);
        Ok(())
    }

    #[test]
    fn store_outside_the_sessions_dir_is_unconfined() -> Result<()> {
        let fixture = fixture()?;
        let data_dir = fixture.root.path().join("elsewhere").join("runtime");
        drop(RuntimeThreadStore::open(data_dir.clone())?);
        let binding = RuntimeStoreBinding {
            data_dir,
            execution_scope: "0".repeat(64),
        };
        assert_eq!(
            binding.adoption_refusal()?,
            Some(StoreAdoptionRefusal::Unconfined)
        );
        assert!(!binding.is_adoptable_empty_store()?);
        Ok(())
    }

    /// A confined path that does not exist reaches `NotADirectory`; a file
    /// in its place never gets that far — confinement rejects any
    /// non-directory on the way down as an `Err`, which is still a refusal.
    #[test]
    fn missing_confined_store_is_not_a_directory() -> Result<()> {
        let fixture = fixture()?;
        let session_dir = fixture.root.path().join("sessions").join("absent");
        std::fs::create_dir_all(&session_dir)?;
        let binding = RuntimeStoreBinding {
            data_dir: session_dir.join("runtime"),
            execution_scope: "0".repeat(64),
        };
        assert_eq!(
            binding.adoption_refusal()?,
            Some(StoreAdoptionRefusal::NotADirectory)
        );
        assert!(!binding.is_adoptable_empty_store()?);

        std::fs::write(&binding.data_dir, "not a store")?;
        assert!(
            binding.adoption_refusal().is_err(),
            "a file in the store's place fails closed"
        );
        Ok(())
    }

    #[test]
    fn live_owner_lock_is_held_by_live_process() -> Result<()> {
        let fixture = fixture()?;
        let binding = opened_binding(&fixture, "held", &"0".repeat(64))?;
        let held = RuntimeProcessOwnerLock::acquire(&binding.data_dir)?;
        assert_eq!(
            binding.adoption_refusal()?,
            Some(StoreAdoptionRefusal::HeldByLiveProcess)
        );
        assert!(!binding.is_adoptable_empty_store()?);
        drop(held);
        assert_eq!(binding.adoption_refusal()?, None);
        Ok(())
    }

    #[test]
    fn each_work_dir_is_named_as_durable_work() -> Result<()> {
        let fixture = fixture()?;
        let binding = opened_binding(&fixture, "busy", &"0".repeat(64))?;
        for dir in RUNTIME_STORE_WORK_DIRS {
            let marker = binding.data_dir.join(dir).join("work.json");
            std::fs::write(&marker, "{}")?;
            assert_eq!(
                binding.adoption_refusal()?,
                Some(StoreAdoptionRefusal::HasDurableWork { dir }),
                "{dir} holds work"
            );
            assert!(!binding.is_adoptable_empty_store()?);
            std::fs::remove_file(&marker)?;
        }
        // Events appended and since pruned still count, reported as `events`.
        let state_path = binding.data_dir.join("state.json");
        let mut state: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&state_path)?)?;
        state["next_seq"] = serde_json::json!(5);
        std::fs::write(&state_path, serde_json::to_vec(&state)?)?;
        assert_eq!(
            binding.adoption_refusal()?,
            Some(StoreAdoptionRefusal::HasDurableWork { dir: "events" })
        );
        Ok(())
    }

    #[test]
    fn automation_pinned_to_the_scope_refuses() -> Result<()> {
        let fixture = fixture()?;
        let scope = "ab".repeat(32);
        let binding = opened_binding(&fixture, "pinned", &scope)?;
        let automations = AutomationManager::open(fixture.root.path().join("automations"))?;
        let created = automations.create_automation(CreateAutomationRequest {
            destination_thread_id: None,
            name: "scope fixture".into(),
            prompt: "local fixture only".into(),
            rrule: "FREQ=HOURLY;INTERVAL=1".into(),
            cwds: vec![fixture.root.path().into()],
            model: None,
            model_provider: None,
            model_provider_id: None,
            mode: None,
            allow_shell: Some(false),
            trust_mode: Some(false),
            auto_approve: Some(false),
            delivery_mode: None,
            status: Some(AutomationStatus::Paused),
        })?;
        automations.edit_automation(&created.id, |record| {
            let mut record =
                record.ok_or_else(|| anyhow::anyhow!("fresh automation must exist"))?;
            record.execution_scope = Some(scope.clone());
            Ok(Some(record))
        })?;
        assert_eq!(
            binding.adoption_refusal()?,
            Some(StoreAdoptionRefusal::ScopePinnedAutomation)
        );
        assert!(!binding.is_adoptable_empty_store()?);
        Ok(())
    }

    fn set_aside_destination(fixture: &Fixture, name: &str) -> std::path::PathBuf {
        fixture
            .root
            .path()
            .join("sessions")
            .join(".set-aside")
            .join("run")
            .join(name)
    }

    /// #6144: a set-aside holds the store's lock for the whole move and
    /// never unlinks the lock file. An opener that takes the store's lock in
    /// the gap after the held lock file has been moved out gets a fresh lock
    /// on a fresh store at the old path, and it is that store's only owner:
    /// a second opener is refused, and the moved store's lock is not held.
    #[test]
    fn opener_in_the_set_aside_gap_is_the_only_owner() -> Result<()> {
        let fixture = fixture()?;
        let binding = opened_binding(&fixture, "gap", &"0".repeat(64))?;
        let source = binding.data_dir.clone();
        let destination = set_aside_destination(&fixture, "gap");
        let held = binding.try_hold()?.expect("unheld store");

        let gap_owner = std::rc::Rc::new(std::cell::RefCell::new(None));
        let slot = gap_owner.clone();
        let gap_source = source.clone();
        set_owner_lock_test_hook(OwnerLockTestPoint::LockFileMoved, move || {
            *slot.borrow_mut() = Some(RuntimeProcessOwnerLock::acquire(&gap_source));
        });
        held.move_to(&destination)?;
        let gap_owner = gap_owner
            .borrow_mut()
            .take()
            .expect("the gap hook ran")
            .expect("the gap opener takes the lock at the old path");

        assert!(destination.join("state.json").is_file(), "store data moved");
        assert!(
            destination.join(RUNTIME_PROCESS_OWNER_LOCK_FILE).is_file(),
            "the held lock file moved with the store instead of being unlinked"
        );
        assert!(
            source.join(RUNTIME_PROCESS_OWNER_LOCK_FILE).is_file(),
            "the gap opener's store is left in place"
        );

        let second = RuntimeProcessOwnerLock::acquire(&source);
        assert!(
            second.is_err(),
            "a second opener must not also own the store"
        );
        assert!(binding.try_hold()?.is_none());
        assert!(binding.has_live_holder()?);
        assert!(
            RuntimeProcessOwnerLock::try_acquire_file(
                &destination.join(RUNTIME_PROCESS_OWNER_LOCK_FILE),
                false,
            )?
            .is_some(),
            "the moved store's lock is released, not held by the gap opener"
        );

        drop(gap_owner);
        assert!(RuntimeProcessOwnerLock::acquire(&source).is_ok());
        Ok(())
    }

    /// #6144: an opener that opened the lock file before a set-aside moved
    /// it, and locked it after, holds the moved file. It must notice and
    /// lock the store at its own path instead, or a later opener would
    /// create a fresh lock file there and both would own that store.
    #[test]
    fn opener_that_locks_a_moved_lock_file_reopens_its_path() -> Result<()> {
        let fixture = fixture()?;
        let binding = opened_binding(&fixture, "raced", &"0".repeat(64))?;
        let source = binding.data_dir.clone();
        let destination = set_aside_destination(&fixture, "raced");

        let mover_binding = binding.clone();
        let mover_destination = destination.clone();
        set_owner_lock_test_hook(OwnerLockTestPoint::LockFileOpened, move || {
            mover_binding
                .try_hold()
                .expect("hold")
                .expect("the opener has not locked yet")
                .move_to(&mover_destination)
                .expect("set aside while the opener sits between open and lock");
        });
        let owner = RuntimeProcessOwnerLock::acquire(&source)?;
        assert!(destination.join("state.json").is_file(), "the move ran");

        assert!(
            RuntimeProcessOwnerLock::acquire(&source).is_err(),
            "the opener owns the store at its path, so a second opener is refused"
        );
        assert!(binding.has_live_holder()?);
        assert!(
            RuntimeProcessOwnerLock::try_acquire_file(
                &destination.join(RUNTIME_PROCESS_OWNER_LOCK_FILE),
                false,
            )?
            .is_some(),
            "the opener let go of the moved store's lock"
        );
        drop(owner);
        Ok(())
    }
}

/// The exact-prefix search a fork's alignment runs, in one pass.
///
/// It replaces a rebuild-per-prefix search that was quadratic in the
/// transcript (measured at ~9 s on a 5 MB session), so what it must keep is
/// the *answer*: the first prefix whose projection is the kept one, and no
/// match at all when the transcript drifted.
#[test]
fn exact_prefix_boundary_finds_the_first_matching_prefix() {
    let user = |text: &str| Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    };
    let messages = vec![user("first"), user("second"), user("third")];
    let projection = |messages: &[Message]| session_recovery_projection(messages);

    // A boundary in the middle: the kept projection names the first two turns.
    assert_eq!(
        exact_prefix_boundary(&messages, &projection(&messages[..2])),
        Some(2)
    );
    // Every message, and none of them.
    assert_eq!(
        exact_prefix_boundary(&messages, &projection(&messages)),
        Some(3)
    );
    assert_eq!(exact_prefix_boundary(&messages, &[]), Some(0));
    // A prefix the transcript cannot produce is no match, not a neighbour.
    assert_eq!(
        exact_prefix_boundary(&messages, &projection(&[user("other")])),
        None
    );
    // A projection that runs past the end of the transcript is no match either.
    let mut longer = projection(&messages);
    longer.push(serde_json::json!(["user", "beyond"]));
    assert_eq!(exact_prefix_boundary(&messages, &longer), None);
}

#[test]
fn saved_history_boundary_refuses_until_every_kept_prompt_is_seen() {
    let user = |text: &str| Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    };
    let kept = vec!["first".to_string(), "second".to_string()];

    // The kept prompts in order, then the undone turn's prompt: cut there.
    let messages = vec![user("first"), user("second"), user("undo me")];
    assert_eq!(saved_history_boundary(&messages, &kept, "undo me"), Some(2));

    // "second" never appears: matching "undo me" early would drop a kept turn.
    let drifted = vec![user("first"), user("undo me"), user("second")];
    assert_eq!(saved_history_boundary(&drifted, &kept, "undo me"), None);

    // A repeated prompt stays aligned with the records.
    let repeated = vec![user("same"), user("same")];
    assert_eq!(
        saved_history_boundary(&repeated, &["same".to_string()], "same"),
        Some(1)
    );
}

/// The engine installs a compaction checkpoint into a synced history, and no
/// turn record reproduces one: the compaction ran in the engine, between
/// records. A fork of a compacted source keeps the summary in its system
/// prompt, so the engine writes that checkpoint into the fork's own document
/// on its first load — and `/undo`, retry and fork-again then run these walks
/// over it. It is the engine's message, not a turn's prompt, so it must not
/// read as a kept prompt the records never saw.
#[test]
fn a_compaction_checkpoint_is_not_a_kept_prompt() {
    let user = |text: &str| Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    };
    let checkpoint = crate::compaction::compaction_checkpoint_message(&SystemPrompt::Text(
        format!("{} the work so far", crate::compaction::SUMMARY_HEADER),
    ));
    assert!(
        codewhale_models::turn_classify::is_wire_compaction_checkpoint_message(&checkpoint),
        "the fixture is the engine's own checkpoint message"
    );
    let messages = vec![user("first"), checkpoint, user("undo me")];
    assert_eq!(
        saved_history_boundary(&messages, &["first".to_string()], "undo me"),
        Some(2)
    );
    assert_eq!(
        exact_prefix_boundary(
            &messages,
            &session_recovery_projection(&[user("first"), user("undo me")])
        ),
        Some(3)
    );
}

/// The rest of the engine's own traffic, by the rule the checkpoint test above
/// states: none of it is recorded on a turn, so none of it may read as a kept
/// prompt.
///
/// A conversation that received an MCP-server notice, a deferred-toolbox
/// snapshot, or a screenshot could not be cut at all: `/undo` answered
/// "Cannot identify an exact saved-history boundary for this backtrack" before
/// it touched a file, so the failed turns a person wanted gone stayed in the
/// transcript. Both the notice and the toolbox snapshot carry the runtime
/// provenance line inside their `<turn_meta>`; a person's own prompt carries
/// none, which is what keeps this recognition from swallowing a real turn.
#[test]
fn runtime_traffic_and_attachment_tags_are_not_kept_prompts() {
    let text = |text: &str| ContentBlock::Text {
        text: text.to_string(),
        cache_control: None,
    };
    let prompt_meta = || text("<turn_meta>\nCurrent local date: 2026-09-24\n</turn_meta>");
    let runtime_meta =
        || text("<turn_meta>\nInput provenance: runtime (non-authoritative)\n</turn_meta>");
    let user = |content: Vec<ContentBlock>| Message {
        role: Role::User,
        content,
    };
    let assistant = |text_: &str| Message {
        role: Role::Assistant,
        content: vec![text(text_)],
    };
    let attached = "second\n[Attached image: /tmp/shot.png]";
    let messages = vec![
        user(vec![text("first"), prompt_meta()]),
        // The MCP-server notice the engine puts at the head of a session.
        user(vec![
            text(
                "<codewhale:runtime_event kind=\"mcp_configured_servers\" visibility=\"internal\">\n- chrome-devtools\n</codewhale:runtime_event>",
            ),
            runtime_meta(),
        ]),
        assistant("ok"),
        // A prompt that carried a screenshot: the engine brackets the image
        // with path tags, and the record keeps only the naming block.
        user(vec![
            text(attached),
            text("<image path=\"/tmp/shot.png\">"),
            ContentBlock::ImageUrl {
                image_url: codewhale_models::ImageUrlContent {
                    url: "data:image/png;base64,iVBORw0KGgo=".to_string(),
                },
            },
            text("</image>"),
            prompt_meta(),
        ]),
        // The deferred-toolbox snapshot the engine injects when the tool set
        // changes; it sits under a full per-turn preamble, not a bare one.
        user(vec![
            text("{\"type\":\"current_deferred_toolbox\",\"active_deferred_tools\":[]}"),
            text(
                "<turn_meta>\nCurrent local date: 2026-09-24\nInput provenance: runtime (non-authoritative)\n## Repo Working Set\nTop-level dirs: src\n</turn_meta>",
            ),
        ]),
        assistant("ok2"),
        user(vec![text("undo me"), prompt_meta()]),
    ];

    assert!(
        projected_user_text(&messages[1]).is_none(),
        "the MCP notice is not a prompt"
    );
    assert!(
        projected_user_text(&messages[4]).is_none(),
        "the toolbox snapshot is not a prompt"
    );
    assert_eq!(
        projected_user_text(&messages[3]).as_deref(),
        Some(attached),
        "a screenshot's prompt is the block that named it, not its tags"
    );

    // Both walks the fork runs land where the records say they must. The
    // kept history is every message but the last, so the exact prefix ends
    // one past the end of the transcript's kept region.
    let kept = vec!["first".to_string(), attached.to_string()];
    assert_eq!(saved_history_boundary(&messages, &kept, "undo me"), Some(6));
    assert_eq!(
        exact_prefix_boundary(
            &messages,
            &session_recovery_projection(&[
                user(vec![text("first"), prompt_meta()]),
                assistant("ok"),
                user(vec![text(attached)]),
                assistant("ok2"),
            ])
        ),
        Some(messages.len() - 1),
        "the exact prefix ends just past the last kept message"
    );
}

/// The decision a rebuild makes about a call the turn store left unanswered:
/// which outcome the item itself holds, and which calls it must leave alone.
#[test]
fn a_lost_call_is_answered_only_where_its_own_outcome_is_known() {
    let item = |status: TurnItemLifecycleStatus, detail: Option<&str>| {
        let mut item = sample_item("turn_lost", "item_call", status);
        item.kind = TurnItemKind::ToolCall;
        item.summary = "bash failed: no output".to_string();
        item.detail = detail.map(str::to_string);
        item
    };
    let none = HashSet::new();
    let identity = |provider_id| RuntimeToolIdentity {
        execution_id: None,
        provider_id,
    };

    // A failure keeps the text the tool reported, and falls back to the
    // item's summary when the record holds no detail of its own.
    assert_eq!(
        unanswered_call_result(
            &item(
                TurnItemLifecycleStatus::Failed,
                Some("Failed to execute tool: boom")
            ),
            identity("call-1"),
            "bash",
            &none
        ),
        Some("Failed to execute tool: boom".to_string())
    );
    assert_eq!(
        unanswered_call_result(
            &item(TurnItemLifecycleStatus::Failed, None),
            identity("call-1"),
            "bash",
            &none
        ),
        Some("bash failed: no output".to_string())
    );

    // A call the process never finished says so in the repository's one
    // spelling, whichever way it was left behind.
    for status in [
        TurnItemLifecycleStatus::Interrupted,
        TurnItemLifecycleStatus::Canceled,
    ] {
        assert_eq!(
            unanswered_call_result(&item(status, None), identity("call-1"), "bash", &none),
            Some(codewhale_runtime::tool_history_repair::CRASH_REPAIR_CONTENT.to_string()),
            "{status:?} is terminal: no result is coming"
        );
    }

    // The engine is still waiting for a running call; a completed one already
    // has its result; a queued one was never delivered. A snapshot without a
    // call identity is not rebuilt at all, so answering it would leave the
    // result on its own.
    for status in [
        TurnItemLifecycleStatus::InProgress,
        TurnItemLifecycleStatus::Queued,
        TurnItemLifecycleStatus::Completed,
    ] {
        assert_eq!(
            unanswered_call_result(&item(status, None), identity("call-1"), "bash", &none),
            None,
            "{status:?} is not a lost outcome"
        );
    }
    assert_eq!(
        unanswered_call_result(
            &item(TurnItemLifecycleStatus::Failed, None),
            identity("call-1"),
            "",
            &none
        ),
        None,
        "a call the rebuild would not emit is not answered"
    );
    assert_eq!(
        unanswered_call_result(
            &item(TurnItemLifecycleStatus::Failed, None),
            identity(""),
            "bash",
            &none
        ),
        None
    );

    // A result the turn records on its own item is that call's answer.
    let recorded = [identity("call-1").key()].into_iter().collect();
    assert_eq!(
        unanswered_call_result(
            &item(TurnItemLifecycleStatus::Failed, None),
            identity("call-1"),
            "bash",
            &recorded
        ),
        None
    );
}

/// #6522 review: `/resume`, `/load` and launch warm the canonical sessions
/// root on a blocking thread, so the confinement predicate that follows on
/// the UI runtime is served from the cache instead of resolving a path.
#[cfg(unix)]
#[tokio::test]
async fn canonical_sessions_root_is_resolved_off_the_ui_runtime_and_cached() -> Result<()> {
    let _env = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let real_home = root.path().join("real-home");
    std::fs::create_dir_all(&real_home)?;
    let home = root.path().join("linked-home");
    std::os::unix::fs::symlink(&real_home, &home)?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
    let sessions = codewhale_config::resolve_state_dir("sessions")?;
    std::fs::create_dir_all(&sessions)?;

    assert_eq!(cached_canonical_sessions_root(&sessions), None);
    prepare_canonical_sessions_root().await;
    let cached = cached_canonical_sessions_root(&sessions).expect("warmed");
    assert_eq!(cached, sessions.canonicalize()?);
    assert_ne!(cached, sessions, "the fixture spells the root two ways");
    Ok(())
}

/// B1: a credential a tool printed is masked in the durable tool item (and
/// the event log built from it) on Runtime API threads, as it is in the
/// engine transcript.
#[tokio::test]
async fn runtime_receipts_mask_configured_secrets() -> Result<()> {
    const TOKEN: &str = "sk-ant-oat01-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcdefghij";
    const EXACT: &str = "abcdefghijklmnopqrst";
    let manager = test_manager(test_runtime_dir())?;
    manager.config.write().sandbox_api_key = Some(EXACT.to_string());
    let thread = manager
        .create_thread(CreateThreadRequest {
            trust_mode: Some(true),
            auto_approve: Some(true),
            ..Default::default()
        })
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "print the auth file".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: turn.id.clone(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ToolCallStarted {
            model_call: Some(crate::core::events::ModelToolCall {
                provider_id: "tool-cat-auth".to_string(),
                caller: None,
                thought_signature: None,
            }),
            id: "tool-cat-auth".to_string(),
            name: "exec_command".to_string(),
            input: json!({"cmd": "cat auth.json"}),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ToolCallComplete {
            model_call: None,
            id: "tool-cat-auth".to_string(),
            name: "exec_command".to_string(),
            // Real `exec_shell` metadata shape: the summaries carry the
            // first stdout line, here compact JSON as `jq -c` prints it.
            result: Ok(crate::tools::spec::ToolResult::success(format!(
                "{{\n  \"access_token\": \"{TOKEN}\",\n  \"note\": \"keep me {EXACT}\"\n}}\n"
            ))
            .with_metadata(json!({
                "exit_code": 0,
                "summary": format!("{{\"tokens\":{{\"access_token\":\"{TOKEN}\"}}}}"),
                "stdout_summary": format!("{{\"tokens\":{{\"access_token\":\"{TOKEN}\"}}}}"),
                "stderr_summary": format!("export OPENAI_API_KEY={TOKEN}"),
                "stdout_len": 120,
                "nested": {"output": [EXACT]},
                "api_key": "tiny",
            }))),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ToolCallStarted {
            model_call: Some(crate::core::events::ModelToolCall {
                provider_id: "tool-error".into(),
                caller: None,
                thought_signature: None,
            }),
            id: "tool-error".into(),
            name: "exec_command".into(),
            input: json!({"cmd": "false"}),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ToolCallComplete {
            model_call: None,
            id: "tool-error".into(),
            name: "exec_command".into(),
            result: Err(crate::tools::spec::ToolError::execution_failed(format!(
                "failed with {EXACT}"
            ))),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    wait_for_terminal_turn(&manager, &turn.id).await?;

    let items = manager.store.list_items_for_turn(&turn.id)?;
    let tool_item = items
        .iter()
        .find(|item| {
            item.detail
                .as_deref()
                .is_some_and(|d| d.contains("keep me"))
        })
        .context("the tool item keeps its ordinary output")?;
    let stored = serde_json::to_string(tool_item)?;
    assert!(!stored.contains(TOKEN), "{stored}");
    assert!(
        !serde_json::to_string(&items)?.contains(EXACT),
        "exact configured secret survived in a receipt"
    );
    let metadata = tool_item.metadata.as_ref().context("tool metadata kept")?;
    assert_eq!(metadata["stdout_len"], 120, "ordinary metadata survives");
    assert_eq!(metadata["exit_code"], 0);
    assert_eq!(metadata["api_key"], codewhale_config::persistence::REDACTED);
    let events = serde_json::to_string(&manager.events_since(&thread.id, None)?)?;
    assert!(!events.contains(TOKEN), "the event log holds no live token");
    assert!(
        !events.contains(EXACT),
        "the event log holds no exact configured secret"
    );
    Ok(())
}

/// B3: Runtime API approvals (GPUI, web) follow the one approval clock,
/// `[approval] timeout_seconds`. Unset, nothing denies on the user's behalf;
/// `[tools] user_input_timeout_seconds` no longer bounds approvals.
#[test]
fn runtime_approvals_wait_indefinitely_unless_approval_timeout_is_set() -> Result<()> {
    let manager = test_manager(test_runtime_dir())?;
    assert_eq!(manager.approval_decision_timeout(), None);

    let mut config = {
        let __cw_config_init_29_0 = Some(codewhale_runtime::config::ToolsConfig {
            user_input_timeout_seconds: Some(300),
            ..Default::default()
        });
        let mut __cw_config_init_29_value: Config = Config::default();
        __cw_config_init_29_value.tools = __cw_config_init_29_0;
        __cw_config_init_29_value
    };
    let manager = RuntimeThreadManager::open(
        config.clone(),
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    assert_eq!(
        manager.approval_decision_timeout(),
        None,
        "the question clock does not bound approvals"
    );

    config.approval = Some(codewhale_runtime::config::ApprovalConfig {
        timeout_seconds: Some(45),
        ..Default::default()
    });
    let manager = RuntimeThreadManager::open(
        config,
        PathBuf::from("."),
        test_manager_config(test_runtime_dir()),
    )?;
    assert_eq!(
        manager.approval_decision_timeout(),
        Some(Duration::from_secs(45))
    );
    Ok(())
}

/// B4: a Runtime thread's tool completion fires `tool_call_after`, and a
/// failed call also fires `on_error`, through the thread's executor.
#[cfg(unix)]
#[tokio::test]
async fn runtime_tool_completion_fires_after_and_error_hooks() -> Result<()> {
    use codewhale_config::hooks::{Hook, HookEvent, HooksConfig};
    let dir = tempfile::tempdir()?;
    let log = dir.path().join("hooks.log");
    let append = |label: &str| {
        format!(
            "printf '%s %s %s\\n' {label} \"$CODEWHALE_TOOL_CALL_ID\" \"$DEEPSEEK_TOOL_SUCCESS\" >> {}",
            log.display()
        )
    };
    let manager = test_manager(test_runtime_dir())?;
    let config = {
        let __cw_config_init_30_0 = Some(HooksConfig {
            hooks: vec![
                Hook::new(HookEvent::ToolCallAfter, &append("after")),
                Hook::new(HookEvent::OnError, &append("error")),
            ],
            enabled: true,
            ..HooksConfig::default()
        });
        let mut __cw_config_init_30_value: Config = Config::default();
        __cw_config_init_30_value.hooks = __cw_config_init_30_0;
        __cw_config_init_30_value
    };
    let hooks = manager.hook_executor_for_workspace(&config, dir.path(), None);
    fire_runtime_tool_completion_hooks(
        &hooks,
        "thr_1",
        "turn-1",
        "call-ok",
        "exec_command",
        None,
        &Ok(crate::tools::spec::ToolResult::success("fine")),
    );
    fire_runtime_tool_completion_hooks(
        &hooks,
        "thr_1",
        "turn-1",
        "call-bad",
        "exec_command",
        None,
        &Ok(crate::tools::spec::ToolResult::error("exit 1")),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let text = loop {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.lines().count() >= 3 || Instant::now() >= deadline {
            break text;
        }
        sleep(Duration::from_millis(20)).await;
    };
    let mut lines: Vec<&str> = text.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        vec![
            "after call-bad false",
            "after call-ok true",
            "error call-bad false"
        ],
        "{text}"
    );
    Ok(())
}

/// A completed file-tool call carries typed artifact refs on its durable item
/// and on the live `item.completed` payload; the legacy `artifact_refs`
/// projection holds only the workspace paths, and a spill yields a
/// tool-output ref.
#[tokio::test]
async fn tool_completion_items_carry_typed_artifact_refs() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path().join("workspace");
    fs::create_dir(&workspace)?;
    let manager = test_manager(dir.path().join("runtime"))?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace.clone()),
            auto_approve: Some(true),
            trust_mode: Some(true),
            ..Default::default()
        })
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "write things".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    let digest = codewhale_runtime::hashing::sha256_hex(b"# out\n");
    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: turn.id.clone(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    for (call, name) in [("call_write", "apply_patch"), ("call_shell", "exec_shell")] {
        harness
            .tx_event
            .send(EngineEvent::ToolCallStarted {
                model_call: Some(crate::core::events::ModelToolCall {
                    provider_id: call.to_string(),
                    caller: None,
                    thought_signature: None,
                }),
                id: call.to_string(),
                name: name.to_string(),
                input: json!({}),
            })
            .await?;
    }
    harness
        .tx_event
        .send(EngineEvent::ToolCallComplete {
            model_call: None,
            id: "call_write".to_string(),
            name: "apply_patch".to_string(),
            result: Ok(
                crate::tools::spec::ToolResult::success("ok").with_metadata(json!({
                    "mutation": {
                        "files": [
                            { "path": "out.md", "outcome": "created", "size": 6, "sha256": digest },
                            { "path": "old.md", "outcome": "deleted" },
                            { "path": "../escape.md", "outcome": "created" }
                        ],
                        "renames": []
                    }
                })),
            ),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::ToolCallComplete {
            model_call: None,
            id: "call_shell".to_string(),
            name: "exec_shell".to_string(),
            // A failed call's large output spills too.
            result: Ok(
                crate::tools::spec::ToolResult::error("boom").with_metadata(json!({
                    "artifact_id": "art_call_shell",
                    "artifact_session_id": "engine-session",
                    "artifact_relative_path": "artifacts/art_call_shell.txt",
                    "artifact_byte_size": 6,
                    "artifact_digest": digest,
                })),
            ),
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    let turn = wait_for_terminal_turn(&manager, &turn.id).await?;

    let items = turn
        .item_ids
        .iter()
        .map(|id| manager.store.load_item(id))
        .collect::<Result<Vec<_>>>()?;
    let write = items
        .iter()
        .find(|item| {
            item.metadata
                .as_ref()
                .is_some_and(|m| m["tool_use_id"] == "call_write")
        })
        .expect("write item");
    assert_eq!(
        write
            .artifacts
            .iter()
            .map(|r| (r.path.as_str(), r.change))
            .collect::<Vec<_>>(),
        [
            ("out.md", Some(turn_artifacts::FileChangeKind::Created)),
            ("old.md", Some(turn_artifacts::FileChangeKind::Deleted)),
        ]
    );
    assert_eq!(
        write.artifacts[0].revision.as_deref(),
        Some(digest.as_str())
    );
    assert_eq!(
        write.artifacts[0].item_id.as_deref(),
        Some(write.id.as_str())
    );
    assert_eq!(write.artifact_refs, vec![PathBuf::from("out.md")]);

    let shell = items
        .iter()
        .find(|item| {
            item.metadata
                .as_ref()
                .is_some_and(|m| m["tool_use_id"] == "call_shell")
        })
        .expect("shell item");
    assert_eq!(shell.status, TurnItemLifecycleStatus::Failed);
    assert_eq!(shell.artifacts.len(), 1);
    assert_eq!(
        shell.artifacts[0].kind,
        turn_artifacts::TurnArtifactKind::ToolOutput
    );
    assert_eq!(
        shell.artifacts[0].session_id.as_deref(),
        Some("engine-session")
    );
    assert!(
        shell.artifact_refs.is_empty(),
        "spill paths are not workspace paths"
    );

    // The live item events carry the same refs.
    let events = manager.events_since(&thread.id, None)?;
    let live = events
        .iter()
        .find(|event| {
            event.event == "item.completed" && event.item_id.as_deref() == Some(write.id.as_str())
        })
        .expect("write item.completed");
    assert_eq!(live.payload["item"]["artifacts"][0]["path"], "out.md");
    assert_eq!(live.payload["item"]["artifact_refs"], json!(["out.md"]));
    Ok(())
}

struct WorkspaceTurnFixture {
    _env: crate::test_support::TestEnvLock,
    _home: crate::test_support::EnvVarGuard,
    dir: tempfile::TempDir,
    workspace: PathBuf,
    manager: RuntimeThreadManager,
    thread: ThreadRecord,
}

async fn workspace_turn_fixture() -> Result<WorkspaceTurnFixture> {
    let env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir()?;
    let home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path().join("home"));
    let workspace = dir.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("README.md"), "fixture\n")?;
    let manager = test_manager(dir.path().join("runtime"))?;
    let thread = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace.clone()),
            auto_approve: Some(true),
            trust_mode: Some(true),
            ..Default::default()
        })
        .await?;
    Ok(WorkspaceTurnFixture {
        _env: env,
        _home: home,
        dir,
        workspace,
        manager,
        thread,
    })
}

/// Take one real restore point in the fixture workspace, tagged the way a
/// Runtime engine tags it (the thread's own id, #6621).
fn fixture_restore_point(
    fixture: &WorkspaceTurnFixture,
    kind: codewhale_snapshot::WorkspaceSnapshotKind,
    label: &str,
    tool_call_id: Option<&str>,
) -> Result<codewhale_snapshot::WorkspaceSnapshotRef> {
    let (taken, _) = crate::core::turn::restore_point_snapshot(
        &fixture.workspace,
        label,
        0,
        Some(&fixture.thread.id),
        None,
    )
    .ok_or_else(|| anyhow!("snapshot {label} failed"))?;
    Ok(codewhale_snapshot::WorkspaceSnapshotRef::new(
        kind,
        &taken,
        &fixture.thread.id,
        tool_call_id,
    ))
}

/// Which restore points the scripted engine reports for the turn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScriptedSnapshots {
    /// `pre_turn`, `tool` (before `call_patch`) and `post_turn`.
    All,
    /// Only `pre_turn`: the closing snapshot failed and reported nothing.
    PreTurnOnly,
    /// None: snapshots off, gated, or failed.
    None,
}

/// Drive one scripted turn: a file-tool write with its receipt, a shell-style
/// write with none, the engine's restore-point receipts (all before
/// TurnComplete, as a recording host gets them) and TurnComplete. Returns the
/// terminal turn and the receipts sent.
async fn run_workspace_turn(
    fixture: &WorkspaceTurnFixture,
    scripted: ScriptedSnapshots,
) -> Result<(TurnRecord, Vec<codewhale_snapshot::WorkspaceSnapshotRef>)> {
    use codewhale_snapshot::WorkspaceSnapshotKind;
    let manager = &fixture.manager;
    let mut harness = install_mock_engine(manager, &fixture.thread.id).await;
    let turn = manager
        .start_turn(
            &fixture.thread.id,
            StartTurnRequest {
                prompt: "build the page".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(
        harness.rx_op.recv().await,
        Some(Op::SendMessage(TurnSpec { .. }))
    ));
    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "engine-turn".to_string(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    let mut sent = Vec::new();
    if scripted != ScriptedSnapshots::None {
        let pre =
            fixture_restore_point(fixture, WorkspaceSnapshotKind::PreTurn, "pre-turn:1", None)?;
        harness
            .tx_event
            .send(EngineEvent::WorkspaceSnapshotTaken {
                snapshot: pre.clone(),
            })
            .await?;
        sent.push(pre);
    }
    harness
        .tx_event
        .send(EngineEvent::ToolCallStarted {
            model_call: Some(crate::core::events::ModelToolCall {
                provider_id: "call_patch".to_string(),
                caller: None,
                thought_signature: None,
            }),
            id: "call_patch".to_string(),
            name: "apply_patch".to_string(),
            input: json!({}),
        })
        .await?;
    if scripted == ScriptedSnapshots::All {
        let tool = fixture_restore_point(
            fixture,
            WorkspaceSnapshotKind::Tool,
            "tool:call_patch",
            Some("call_patch"),
        )?;
        harness
            .tx_event
            .send(EngineEvent::WorkspaceSnapshotTaken {
                snapshot: tool.clone(),
            })
            .await?;
        sent.push(tool);
    }
    fs::write(fixture.workspace.join("notes.md"), "# notes\n")?;
    harness
        .tx_event
        .send(EngineEvent::ToolCallComplete {
            model_call: None,
            id: "call_patch".to_string(),
            name: "apply_patch".to_string(),
            result: Ok(
                crate::tools::spec::ToolResult::success("ok").with_metadata(json!({
                    "mutation": { "files": [{
                        "path": "notes.md", "outcome": "created", "size": 8,
                        "sha256": codewhale_runtime::hashing::sha256_hex(b"# notes\n"),
                    }], "renames": [] }
                })),
            ),
        })
        .await?;
    // A shell command's write: no receipt reaches the runtime.
    fs::write(fixture.workspace.join("out.md"), "shell output\n")?;
    if scripted == ScriptedSnapshots::All {
        let post = fixture_restore_point(
            fixture,
            WorkspaceSnapshotKind::PostTurn,
            "post-turn:1",
            None,
        )?;
        harness
            .tx_event
            .send(EngineEvent::WorkspaceSnapshotTaken {
                snapshot: post.clone(),
            })
            .await?;
        sent.push(post);
    }
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    Ok((wait_for_terminal_turn(manager, &turn.id).await?, sent))
}

async fn wait_for_turn_workspace(
    manager: &RuntimeThreadManager,
    turn_id: &str,
    state: TurnWorkspaceState,
) -> Result<TurnRecord> {
    let deadline = Instant::now() + TURN_SETTLEMENT_DEADLOCK_TIMEOUT;
    loop {
        let turn = manager.store.load_turn(turn_id)?;
        if turn.workspace.as_ref().map(|w| w.state) == Some(state) {
            return Ok(turn);
        }
        if Instant::now() >= deadline {
            bail!(
                "turn {turn_id} never reached {state:?}: {:?}",
                turn.workspace
            );
        }
        sleep(Duration::from_millis(10)).await;
    }
}

/// The workspace delta between the pre/post-turn restore points recorded on
/// the turn settles into the turn aggregate: a shell-written file appears
/// with its revision next to the tool-written one, every restore point a ref
/// advertises is one recorded on the thread's own turn (#6621),
/// `turn.artifacts` is published, and the list route serves it only for the
/// owning thread.
#[tokio::test]
async fn turn_workspace_delta_settles_shell_writes_into_the_aggregate() -> Result<()> {
    let fixture = workspace_turn_fixture().await?;
    let (turn, receipts) = run_workspace_turn(&fixture, ScriptedSnapshots::All).await?;
    let [pre, tool, post] = receipts.as_slice() else {
        bail!("three receipts: {receipts:?}");
    };
    // The receipts are the turn's own restore points.
    assert_eq!(turn.workspace_snapshots, receipts);

    let turn =
        wait_for_turn_workspace(&fixture.manager, &turn.id, TurnWorkspaceState::Settled).await?;
    let workspace = turn.workspace.clone().unwrap();
    assert_eq!(
        workspace.pre_turn_snapshot_id.as_deref(),
        Some(pre.tree_id.as_str())
    );
    assert_eq!(
        workspace.post_turn_snapshot_id.as_deref(),
        Some(post.tree_id.as_str())
    );
    let shell = turn
        .artifacts
        .iter()
        .find(|r| r.path == "out.md")
        .expect("shell-written file is reported");
    assert_eq!(
        shell.source,
        turn_artifacts::TurnArtifactSource::WorkspaceChangedDuringTurn
    );
    assert_eq!(shell.change, Some(turn_artifacts::FileChangeKind::Created));
    assert_eq!(
        shell.revision.as_deref(),
        Some(codewhale_runtime::hashing::sha256_hex(b"shell output\n").as_str())
    );
    assert_eq!(shell.size, Some(13));
    // The thread owns the turn's pre-turn restore point, so file-revert
    // accepts it for a shell-written file, bound session or not.
    assert_eq!(
        shell.restore_snapshot_id.as_deref(),
        Some(pre.tree_id.as_str())
    );
    let notes = turn
        .artifacts
        .iter()
        .find(|r| r.path == "notes.md")
        .unwrap();
    assert_eq!(
        notes.source,
        turn_artifacts::TurnArtifactSource::ToolMutation
    );
    assert_eq!(notes.tool_call_id.as_deref(), Some("call_patch"));
    assert_eq!(
        notes.restore_snapshot_id.as_deref(),
        Some(pre.tree_id.as_str())
    );
    // The item names its own call's `tool` restore point.
    let patch_item = turn
        .item_ids
        .iter()
        .map(|id| fixture.manager.store.load_item(id))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .find(|item| !item.artifacts.is_empty())
        .expect("patch item");
    assert_eq!(
        patch_item.artifacts[0].restore_snapshot_id.as_deref(),
        Some(tool.tree_id.as_str())
    );

    // The settled turn is saved before `turn.artifacts` is emitted, so the
    // event can trail the state this test just observed.
    let deadline = Instant::now() + TURN_SETTLEMENT_DEADLOCK_TIMEOUT;
    let events = loop {
        let events = fixture.manager.events_since(&fixture.thread.id, None)?;
        if events.iter().any(|event| event.event == "turn.artifacts") {
            break events;
        }
        if Instant::now() >= deadline {
            bail!("turn.artifacts was never published");
        }
        sleep(Duration::from_millis(10)).await;
    };
    let published = events
        .iter()
        .find(|event| event.event == "turn.artifacts")
        .expect("turn.artifacts published");
    assert_eq!(published.turn_id.as_deref(), Some(turn.id.as_str()));
    assert_eq!(published.payload["workspace"]["state"], "settled");
    let completed = events
        .iter()
        .find(|event| event.event == "turn.completed")
        .expect("turn.completed");
    assert_eq!(completed.payload["turn"]["workspace"]["state"], "pending");

    let view = fixture
        .manager
        .turn_artifacts(&fixture.thread.id, &turn.id)
        .await?
        .expect("owning thread sees the turn");
    assert_eq!(view.artifacts, turn.artifacts);
    let other = fixture
        .manager
        .create_thread(CreateThreadRequest {
            workspace: Some(fixture.workspace.clone()),
            ..Default::default()
        })
        .await?;
    assert!(
        fixture
            .manager
            .turn_artifacts(&other.id, &turn.id)
            .await?
            .is_none(),
        "a turn is served only through its own thread"
    );
    assert!(
        fixture
            .manager
            .turn_artifacts(&fixture.thread.id, "turn_missing")
            .await?
            .is_none()
    );
    Ok(())
}

/// Snapshots off: the turn still lists what its tool receipts recorded, and
/// says plainly why there is no workspace delta. A turn that recorded no
/// restore point with snapshots on says it captured none; one whose closing
/// snapshot failed keeps its pre-turn restore point and says so.
#[tokio::test]
async fn a_turn_without_its_snapshot_pair_keeps_the_item_aggregate_with_a_reason() -> Result<()> {
    let fixture = workspace_turn_fixture().await?;
    let (turn, _) = run_workspace_turn(&fixture, ScriptedSnapshots::None).await?;
    let workspace = turn.workspace.clone().expect("workspace accounting");
    assert_eq!(workspace.state, TurnWorkspaceState::Unavailable);
    assert_eq!(workspace.reason, Some(TurnWorkspaceReason::NotCaptured));
    assert_eq!(
        turn.artifacts
            .iter()
            .map(|r| r.path.as_str())
            .collect::<Vec<_>>(),
        ["notes.md"]
    );
    assert_eq!(turn.artifacts[0].restore_snapshot_id, None);

    let (turn, receipts) = run_workspace_turn(&fixture, ScriptedSnapshots::PreTurnOnly).await?;
    let workspace = turn.workspace.clone().expect("workspace accounting");
    assert_eq!(workspace.state, TurnWorkspaceState::Unavailable);
    assert_eq!(workspace.reason, Some(TurnWorkspaceReason::SnapshotFailed));
    assert_eq!(
        workspace.pre_turn_snapshot_id.as_deref(),
        Some(receipts[0].tree_id.as_str())
    );

    fixture.manager.config.write().snapshots = Some(codewhale_runtime::config::SnapshotsConfig {
        enabled: false,
        ..Default::default()
    });
    let (turn, _) = run_workspace_turn(&fixture, ScriptedSnapshots::None).await?;
    let workspace = turn.workspace.clone().expect("workspace accounting");
    assert_eq!(
        workspace.reason,
        Some(TurnWorkspaceReason::SnapshotsDisabled)
    );
    assert_eq!(turn.artifacts.len(), 1);
    Ok(())
}

/// A turn left `pending` by a stopped Runtime is reconciled on startup to
/// `runtime_restarted`, keeping its item-derived refs, never re-guessed.
#[tokio::test]
async fn a_pending_turn_workspace_is_reconciled_on_restart() -> Result<()> {
    let fixture = workspace_turn_fixture().await?;
    let mut turn = sample_turn(
        &fixture.thread.id,
        "turn_pending_ws",
        RuntimeTurnStatus::Completed,
    );
    turn.ended_at = Some(Utc::now());
    let pre = "0123456789abcdef0123456789abcdef01234567".to_string();
    turn.workspace = Some(TurnWorkspaceArtifacts::pending(pre.clone()));
    turn.artifacts = vec![TurnArtifactRef {
        id: "art_call".into(),
        kind: TurnArtifactKind::ToolOutput,
        path: "artifacts/art_call.txt".into(),
        change: None,
        previous_path: None,
        size: Some(1),
        revision: None,
        content_type: None,
        session_id: Some("s".into()),
        item_id: None,
        tool_call_id: None,
        tool_name: None,
        source: turn_artifacts::TurnArtifactSource::ToolOutputSpill,
        restore_snapshot_id: None,
        recorded_at: Utc::now(),
    }];
    fixture.manager.store.save_turn(&turn)?;
    let data_dir = fixture.dir.path().join("runtime");
    let WorkspaceTurnFixture {
        _env,
        _home,
        dir,
        manager,
        ..
    } = fixture;
    drop(manager);
    let reopened = test_manager(data_dir)?;
    let turn = reopened.store.load_turn("turn_pending_ws")?;
    let workspace = turn.workspace.expect("workspace kept");
    assert_eq!(workspace.state, TurnWorkspaceState::Unavailable);
    assert_eq!(
        workspace.reason,
        Some(TurnWorkspaceReason::RuntimeRestarted)
    );
    assert_eq!(
        workspace.pre_turn_snapshot_id.as_deref(),
        Some(pre.as_str())
    );
    assert_eq!(turn.artifacts.len(), 1);
    drop(dir);
    Ok(())
}
/// #6582: a Runtime API `bash` completion hands the command's exit code and
/// status to `tool_call_after` and `on_error`, as the TUI does. A nonzero
/// exit is a `ToolError`. A foreground wait that expires is not: the command
/// moves to the background and stays running, so `on_error` does not fire.
#[cfg(unix)]
#[tokio::test]
async fn runtime_shell_completion_delivers_exit_code_and_status_to_hooks() -> Result<()> {
    use crate::tools::spec::ToolSpec;
    use codewhale_config::hooks::{Hook, HookEvent, HooksConfig};
    let dir = tempfile::tempdir()?;
    let after_log = dir.path().join("after.log");
    let error_log = dir.path().join("error.log");
    let script = |path: &std::path::Path| {
        format!(
            "printf '%s %s %s %s\\n' \"$CODEWHALE_TOOL_CALL_ID\" \"${{DEEPSEEK_TOOL_EXIT_CODE-unset}}\" \"${{DEEPSEEK_TOOL_STATUS-unset}}\" \"$DEEPSEEK_TOOL_SUCCESS\" >> {}",
            path.display()
        )
    };
    let manager = test_manager(test_runtime_dir())?;
    let config = {
        let __cw_config_init_31_0 = Some(HooksConfig {
            hooks: vec![
                Hook::new(HookEvent::ToolCallAfter, &script(&after_log)),
                Hook::new(HookEvent::OnError, &script(&error_log)),
            ],
            enabled: true,
            ..HooksConfig::default()
        });
        let mut __cw_config_init_31_value: Config = Config::default();
        __cw_config_init_31_value.hooks = __cw_config_init_31_0;
        __cw_config_init_31_value
    };
    let hooks = manager.hook_executor_for_workspace(&config, dir.path(), None);
    let context = crate::tools::spec::ToolContext::new(dir.path());
    let cases = [
        ("call-exit-0", json!({"command": "exit 0"})),
        ("call-exit-1", json!({"command": "exit 1"})),
        (
            "call-exit-127",
            json!({"command": "codewhale-no-such-command-6582"}),
        ),
        (
            "call-timeout",
            json!({"command": "sleep 5", "timeout": 0.2}),
        ),
    ];
    for (id, input) in cases {
        let result = crate::tools::shell::LowercaseBashTool
            .execute(input.clone(), &context)
            .await;
        fire_runtime_tool_completion_hooks(
            &hooks,
            "thr_1",
            "turn-1",
            id,
            "bash",
            Some(&input),
            &result,
        );
    }
    let read_lines = |path: &std::path::Path| {
        let mut lines = std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        lines.sort();
        lines
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let (after, errors) = loop {
        let after = read_lines(&after_log);
        let errors = read_lines(&error_log);
        if (after.len() >= 4 && errors.len() >= 2) || Instant::now() >= deadline {
            break (after, errors);
        }
        sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        after,
        vec![
            "call-exit-0 0 completed true",
            "call-exit-1 1 failed false",
            "call-exit-127 127 failed false",
            "call-timeout unset running true",
        ]
    );
    assert_eq!(
        errors,
        vec![
            "call-exit-1 1 failed false",
            "call-exit-127 127 failed false",
        ]
    );
    Ok(())
}

mod execution_identity {
    use super::*;
    use crate::core::events::ModelToolCall;

    #[tokio::test]
    async fn seed_restart_snapshot_import_and_fork_preserve_execution_pairs() -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let runtime = dir.path().join("runtime");
        let manager = test_manager(runtime.clone())?;
        let thread = manager.create_thread(Default::default()).await?;
        let original: Vec<Message> = serde_json::from_value(json!([
            {"role":"user","content":[{"type":"text","text":"retain both executions"}]},
            {"role":"assistant","content":[{"type":"tool_use","id":"wire-reused","execution_id":"host-one","name":"read_file","input":{"path":"one"},"caller":{"type":"subagent","tool_id":"parent-wire"},"thought_signature":"signed-one"}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"wire-reused","execution_id":"host-one","content":"one","content_blocks":[{"type":"text","text":"rich-one"}]}]},
            {"role":"assistant","content":[{"type":"tool_use","id":"wire-reused","execution_id":"host-two","name":"read_file","input":{"path":"two"}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"wire-reused","execution_id":"host-two","content":"two"}]}
        ]))?;
        manager
            .seed_thread_from_messages(&thread.id, &original)
            .await?;
        drop(manager);
        let manager = test_manager(runtime)?;
        let reopened = manager.get_thread(&thread.id).await?;
        assert_eq!(manager.restore_thread_messages(&reopened)?, original);
        let detail = manager.get_thread_detail(&thread.id).await?;
        let calls: Vec<_> = detail
            .items
            .iter()
            .filter_map(|item| item.metadata.as_ref())
            .filter(|meta| meta.get("tool_use_id").is_some())
            .collect();
        assert_eq!(calls.len(), 2);
        assert_ne!(calls[0]["tool_use_id"], calls[1]["tool_use_id"]);
        assert!(
            calls
                .iter()
                .all(|meta| meta["provider_tool_use_id"] == "wire-reused")
        );

        // The existing snapshot/journal and foreign-import owner carry the
        // metadata; importing does not create authority or a fresh execution.
        let saved = crate::session_manager::create_saved_session_with_id_and_mode(
            Uuid::new_v4().to_string(),
            &original,
            &thread.model,
            &thread.workspace,
            0,
            None,
            Some("agent"),
        );
        let sessions = crate::session_manager::SessionManager::new(
            crate::session_manager::default_sessions_dir()?,
        )?;
        sessions.save_session(&saved)?;
        let loaded = sessions.load_session(&saved.metadata.id)?;
        assert_eq!(loaded.messages, original);
        let imported = crate::session_manager::SavedSession::import_foreign(
            loaded.export_container("identity-fixture"),
            thread.workspace.clone(),
            thread.model.clone(),
        )
        .map_err(anyhow::Error::msg)?;
        assert_eq!(imported.messages, original);
        assert!(imported.approval_receipts.is_empty());
        manager
            .set_thread_session_checkpoint(&thread.id, &loaded)
            .await?;
        let fork = manager.fork_thread(&thread.id).await?;
        assert_eq!(manager.restore_thread_messages(&fork)?, original);
        assert_eq!(
            manager
                .get_thread_detail(&fork.id)
                .await?
                .pending_approvals
                .len(),
            0
        );
        let mut other_identity = original.clone();
        if let ContentBlock::ToolUse { execution_id, .. } = &mut other_identity[1].content[0] {
            *execution_id = Some("host-other".into());
        }
        assert_ne!(
            session_recovery_projection(&original),
            session_recovery_projection(&other_identity)
        );
        if let ContentBlock::ToolUse { execution_id, .. } = &mut other_identity[1].content[0] {
            *execution_id = Some(String::new());
        }
        let before = serde_json::to_value(manager.get_thread_detail(&thread.id).await?)?;
        assert!(
            manager
                .seed_thread_from_messages(&thread.id, &other_identity)
                .await
                .is_err()
        );
        assert_eq!(
            serde_json::to_value(manager.get_thread_detail(&thread.id).await?)?,
            before,
            "reject the whole malformed import before writing any turn or item"
        );
        let mut wrong_provider = original.clone();
        if let ContentBlock::ToolResult { tool_use_id, .. } = &mut wrong_provider[4].content[0] {
            *tool_use_id = "different-wire".into();
        }
        assert!(
            manager
                .seed_thread_from_messages(&thread.id, &wrong_provider)
                .await
                .unwrap_err()
                .to_string()
                .contains("inconsistent provider identity")
        );
        assert_eq!(
            serde_json::to_value(manager.get_thread_detail(&thread.id).await?)?,
            before
        );
        // Also refuse already-corrupted separate result records on a real
        // item-only rebuild; never return a repaired call plus an orphan result.
        let turns = manager.store.list_turns_for_thread(&thread.id)?;
        let mut result = manager
            .store
            .list_items_for_turn(&turns[0].id)?
            .into_iter()
            .find(|item| {
                item.metadata
                    .as_ref()
                    .is_some_and(|meta| meta["tool_result_for"] == "host-two")
            })
            .context("second result")?;
        result.metadata.as_mut().unwrap()["provider_tool_use_id"] = json!("different-wire");
        manager.store.save_item(&result)?;
        assert!(
            manager
                .reconstruct_messages_from_turns(&turns)
                .unwrap_err()
                .to_string()
                .contains("inconsistent provider identity")
        );
        Ok(())
    }

    #[tokio::test]
    async fn monitor_preserves_admitted_correlation_and_redacts_private_answers() -> Result<()> {
        let manager = test_manager(test_runtime_dir())?;
        let thread = manager.create_thread(Default::default()).await?;
        let mut harness = install_mock_engine(&manager, &thread.id).await;
        let turn = manager
            .start_turn(
                &thread.id,
                StartTurnRequest {
                    prompt: "project exact tool events".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert!(matches!(
            harness.rx_op.recv().await,
            Some(Op::SendMessage(_))
        ));
        harness
            .tx_event
            .send(EngineEvent::TurnStarted {
                turn_id: turn.id.clone(),
                created_at: Utc::now(),
                route: None,
                submission_id: None,
            })
            .await?;
        for (id, name, model, fails) in [
            ("host-one", "read_file", true, false),
            ("host-two", "exec_shell", true, false),
            ("host-three", "write_file", true, true),
            ("host-local", "read_file", false, false),
            ("host-private", REQUEST_USER_INPUT_TOOL_NAME, true, false),
        ] {
            let caller = (id == "host-one").then(|| ToolCaller {
                caller_type: "subagent".into(),
                tool_id: Some("parent-wire".into()),
            });
            let model_call = model.then(|| ModelToolCall {
                provider_id: "wire-reused".into(),
                caller,
                thought_signature: (id == "host-one").then(|| "trusted-signature".into()),
            });
            harness
                .tx_event
                .send(EngineEvent::ToolCallStarted {
                    id: id.into(),
                    name: name.into(),
                    input: json!({"source":id}),
                    model_call,
                })
                .await?;
            let result = if fails {
                Err(crate::tools::spec::ToolError::execution_failed(
                    "interrupted fixture",
                ))
            } else {
                Ok(crate::tools::spec::ToolResult::success(if id == "host-private" {
                    "private-answer-do-not-persist"
                } else { "output" }).with_metadata(json!({
                    "tool_use_id":"forged", "tool_result_for":"forged", "tool_name":"forged",
                    "tool_input":"forged", "execution_id":"forged", "provider_tool_use_id":"forged",
                    "tool_caller":{"type":"forged"}, "tool_thought_signature":"forged",
                    "visibility":"forged", "ordinary_result_metadata":42,
                    "content_blocks":[{"type":"text","text":"rich-output"}]
                })))
            };
            harness
                .tx_event
                .send(EngineEvent::ToolCallComplete {
                    id: id.into(),
                    name: name.into(),
                    result,
                    // Completion cannot replace the admitted start's identity.
                    model_call: Some(ModelToolCall {
                        provider_id: "forged-completion".into(),
                        caller: None,
                        thought_signature: None,
                    }),
                })
                .await?;
        }
        harness
            .tx_event
            .send(EngineEvent::TurnComplete {
                turn_id: None,
                submission_id: None,
                compaction_id: None,
                usage: Usage::default(),
                parent_route_usage: Usage::default(),
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await?;
        wait_for_terminal_turn(&manager, &turn.id).await?;
        let detail = manager.get_thread_detail(&thread.id).await?;
        let bytes = serde_json::to_string(&detail.items)?;
        assert!(!bytes.contains("private-answer-do-not-persist"));
        assert!(!bytes.contains("forged"));
        let second = detail
            .items
            .iter()
            .find(|item| {
                item.metadata
                    .as_ref()
                    .is_some_and(|meta| meta["tool_use_id"] == "host-two")
            })
            .context("second call")?;
        assert_eq!(second.kind, TurnItemKind::CommandExecution);
        let meta = second.metadata.as_ref().unwrap();
        assert!(meta.get("tool_caller").is_none());
        assert!(meta.get("tool_thought_signature").is_none());
        assert!(meta.get("visibility").is_none());
        assert_eq!(meta["ordinary_result_metadata"], 42);
        let messages = manager.reconstruct_messages_from_turns(&detail.turns)?;
        let uses: Vec<_> = messages
            .iter()
            .flat_map(|m| &m.content)
            .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
            .collect();
        let results: Vec<_> = messages
            .iter()
            .flat_map(|m| &m.content)
            .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
            .collect();
        assert_eq!(
            uses.len(),
            3,
            "non-model and redacted private items are not replayed"
        );
        assert_eq!(
            results.len(),
            3,
            "an earlier same-wire result cannot hide a failed later call"
        );
        for (index, (call, result)) in uses.iter().zip(results.iter()).enumerate() {
            let expected = format!("host-{}", ["one", "two", "three"][index]);
            assert_eq!(
                call.tool_call_key(),
                Some(ToolCallKey::Execution(&expected))
            );
            assert_eq!(call.tool_call_key(), result.tool_call_key());
            match call {
                ContentBlock::ToolUse {
                    id,
                    caller,
                    thought_signature,
                    ..
                } => {
                    assert_eq!(id, "wire-reused");
                    assert_eq!(caller.is_some(), index == 0);
                    assert_eq!(thought_signature.is_some(), index == 0);
                }
                _ => unreachable!(),
            }
        }
        assert!(matches!(
            results[2],
            ContentBlock::ToolResult {
                is_error: Some(true),
                ..
            }
        ));
        assert!(
            !serde_json::to_string(&manager.events_since(&thread.id, None)?)?
                .contains("private-answer-do-not-persist")
        );
        Ok(())
    }

    #[test]
    fn explicit_and_legacy_result_domains_cannot_settle_each_other() -> Result<()> {
        let mut item = sample_item(
            "turn_identity",
            "call",
            TurnItemLifecycleStatus::Interrupted,
        );
        item.kind = TurnItemKind::ToolCall;
        let new = RuntimeToolIdentity {
            execution_id: Some("same-text"),
            provider_id: "wire",
        };
        let legacy = RuntimeToolIdentity {
            execution_id: None,
            provider_id: "same-text",
        };
        for (call, result) in [
            (new, legacy),
            (legacy, new),
            (
                new,
                RuntimeToolIdentity {
                    execution_id: Some("same-text"),
                    provider_id: "different-wire",
                },
            ),
        ] {
            assert!(
                unanswered_call_result(
                    &item,
                    call,
                    "read_file",
                    &[result.key()].into_iter().collect()
                )
                .is_some()
            );
        }
        assert!(
            unanswered_call_result(&item, new, "read_file", &[new.key()].into_iter().collect())
                .is_none()
        );
        for meta in [
            json!({"tool_use_id":"same-text","execution_id":"","provider_tool_use_id":"wire"}),
            json!({"tool_use_id":"other","execution_id":"same-text","provider_tool_use_id":"wire"}),
            json!({"tool_use_id":"same-text","execution_id":"same-text","provider_tool_use_id":false}),
        ] {
            assert!(RuntimeToolIdentity::read(Some(&meta), "tool_use_id").is_err());
        }
        let non_model = json!({"tool_use_id":"same-text","execution_id":"same-text"});
        assert!(RuntimeToolIdentity::read(Some(&non_model), "tool_use_id")?.is_none());
        let old = json!({"tool_use_id":"same-text"});
        assert_eq!(
            RuntimeToolIdentity::read(Some(&old), "tool_use_id")?
                .unwrap()
                .key(),
            legacy.key()
        );
        Ok(())
    }

    /// #6803: a tool that fails — a `read` of a missing file — is answered
    /// with an error result the model receives. That answer must live in the
    /// record itself (`tool_result_for` and `is_error` on the failed item, as a
    /// completed call records its own), so a runtime restarted on the same
    /// home rebuilds the call together with its result rather than a
    /// `function_call` the provider rejects with `No tool output found`.
    #[tokio::test]
    async fn a_failed_tool_call_records_its_result_and_replays_after_restart() -> Result<()> {
        let runtime = test_runtime_dir();
        let manager = test_manager(runtime.clone())?;
        let thread = manager.create_thread(Default::default()).await?;
        let mut harness = install_mock_engine(&manager, &thread.id).await;
        let turn = manager
            .start_turn(
                &thread.id,
                StartTurnRequest {
                    prompt: "read a file that is not there".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert!(matches!(
            harness.rx_op.recv().await,
            Some(Op::SendMessage(_))
        ));
        harness
            .tx_event
            .send(EngineEvent::TurnStarted {
                turn_id: turn.id.clone(),
                created_at: Utc::now(),
                route: None,
                submission_id: None,
            })
            .await?;
        harness
            .tx_event
            .send(EngineEvent::ToolCallStarted {
                id: "host-read".into(),
                name: "read".into(),
                input: json!({"path": "missing.txt"}),
                model_call: Some(ModelToolCall {
                    provider_id: "call_00_missing".into(),
                    caller: None,
                    thought_signature: None,
                }),
            })
            .await?;
        harness
            .tx_event
            .send(EngineEvent::ToolCallComplete {
                id: "host-read".into(),
                name: "read".into(),
                result: Err(crate::tools::spec::ToolError::execution_failed(
                    "missing.txt: No such file or directory",
                )),
                model_call: None,
            })
            .await?;
        harness
            .tx_event
            .send(EngineEvent::TurnComplete {
                turn_id: None,
                submission_id: None,
                compaction_id: None,
                usage: Usage::default(),
                parent_route_usage: Usage::default(),
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await?;
        wait_for_terminal_turn(&manager, &turn.id).await?;

        let detail = manager.get_thread_detail(&thread.id).await?;
        let failed = detail
            .items
            .iter()
            .find(|item| {
                item.metadata
                    .as_ref()
                    .is_some_and(|meta| meta["tool_use_id"] == "host-read")
            })
            .context("failed call")?;
        assert_eq!(failed.status, TurnItemLifecycleStatus::Failed);
        let meta = failed.metadata.as_ref().unwrap();
        assert_eq!(meta["tool_result_for"], "host-read");
        assert_eq!(meta["is_error"], true);
        assert_eq!(meta["provider_tool_use_id"], "call_00_missing");

        // Restart on the same home: the next request is rebuilt from the
        // persisted records alone.
        drop(harness);
        drop(manager);
        let manager = test_manager(runtime)?;
        let reopened = manager.get_thread(&thread.id).await?;
        let messages = manager.restore_thread_messages(&reopened)?;
        let blocks: Vec<&ContentBlock> = messages.iter().flat_map(|m| &m.content).collect();
        let calls: Vec<&ContentBlock> = blocks
            .iter()
            .copied()
            .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
            .collect();
        let results: Vec<&ContentBlock> = blocks
            .iter()
            .copied()
            .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
            .collect();
        assert_eq!(calls.len(), 1, "the failed call is replayed");
        assert_eq!(results.len(), 1, "and answered exactly once");
        assert_eq!(calls[0].tool_call_key(), results[0].tool_call_key());
        let ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            ..
        } = results[0]
        else {
            unreachable!()
        };
        assert_eq!(tool_use_id, "call_00_missing");
        assert_eq!(*is_error, Some(true));
        assert!(content.contains("No such file or directory"), "{content}");
        Ok(())
    }
}

#[tokio::test]
async fn decision_receipt_lease_persists_terminal_turn_and_replays_exactly_once() -> Result<()> {
    let _cost_scope = crate::cost_status::test_scope();
    let directory = test_runtime_dir();
    let manager = test_manager(directory.clone())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let turn = sample_turn(
        &thread.id,
        "decision-terminal-origin",
        RuntimeTurnStatus::Completed,
    );
    manager.store.save_turn(&turn)?;
    manager.register_runtime_usage_sink(&turn.id);
    let lease = crate::cost_status::acquire_runtime_usage_lease(&turn.id).expect("origin lease");
    crate::cost_status::finish_runtime_usage_owner(&turn.id);
    let receipt = crate::cost_status::decision_receipt_fixture("raw-runtime-decision-id");
    let batch = crate::cost_status::RuntimeUsageBatch {
        decisions: vec![receipt.clone()],
        ..Default::default()
    };
    for _ in 0..2 {
        crate::cost_status::report_runtime_usage_batch(
            crate::cost_status::scope_token(),
            Some(&turn.id),
            &batch,
        );
    }
    drop(lease);
    drop(manager);
    let restarted = test_manager(directory)?;
    let reloaded = restarted.store.load_turn(&turn.id)?;
    assert_eq!(reloaded.status, RuntimeTurnStatus::Completed);
    assert_eq!(reloaded.decision_receipts, vec![receipt.sanitized()]);
    assert_eq!(
        reloaded.decision_receipts[0]
            .evidence
            .provider_reported_cost_usd
            .as_deref(),
        Some("0.000012054")
    );
    let aggregate = restarted
        .aggregate_usage(None, None, UsageGroupBy::Thread)
        .await?;
    assert!(
        aggregate
            .totals
            .route_receipts
            .iter()
            .any(|r| r.contains("0.000012054")),
        "existing cost diagnostics expose retained provider evidence"
    );
    assert!(
        reloaded.routed_usage.is_empty(),
        "diagnostic receipt alone must not mint a second token charge"
    );
    assert_eq!(
        unaccepted_routed_usage_turn_id(&thread.id, &batch),
        unaccepted_routed_usage_turn_id(&thread.id, &batch)
    );
    assert!(!serde_json::to_string(&reloaded)?.contains("raw-runtime-decision-id"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn control_owner_capture_uses_held_lease_and_new_generation_after_restart() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("runtime");
    let manager = test_manager(path.clone())?;
    let (binding, generation) = manager.capture_control_owner()?;
    assert_eq!(binding, manager.session_store_binding());
    assert!(!generation.is_empty());
    assert_eq!(manager.capture_control_owner()?.1, generation);
    let lock = binding.data_dir.join(RUNTIME_PROCESS_OWNER_LOCK_FILE);
    let retained = binding.data_dir.join("retained-owner.lock");
    fs::rename(&lock, &retained)?;
    fs::write(&lock, b"replacement")?;
    assert!(manager.capture_control_owner().is_err());
    fs::remove_file(&lock)?;
    fs::rename(&retained, &lock)?;
    drop(manager);
    let restarted = test_manager(path)?;
    assert_ne!(restarted.capture_control_owner()?.1, generation);
    assert_eq!(restarted.capture_control_owner()?.0, binding);
    Ok(())
}

#[test]
fn ordinary_terminal_request_facts_omit_private_acp_stop_projection() {
    let facts = RuntimeTurnRequestDiagnostics {
        stop_reason: None,
        model_requests_started: 2,
        transport_retries: 0,
        transparent_stream_retries: 1,
        stream_resumes: 0,
        reasoning_only_reprompts: 0,
        empty_stop_retries: 0,
    };
    let json = serde_json::to_value(facts).unwrap();
    assert!(json.get("stopReason").is_none());
    assert_eq!(
        serde_json::from_value::<RuntimeTurnRequestDiagnostics>(json).unwrap(),
        facts
    );
}
#[test]
fn acp_typed_stop_projection_preserves_every_core_reason() {
    use crate::tool_inspection::{TurnStopDiagnostics, TurnStopReason};
    for (reason, expected) in [
        (
            TurnStopReason::ProviderNoToolCall,
            RuntimeTurnStopReason::ProviderNoToolCall,
        ),
        (
            TurnStopReason::ProviderToolCallMissing,
            RuntimeTurnStopReason::ProviderToolCallMissing,
        ),
        (
            TurnStopReason::StepBudgetExhausted,
            RuntimeTurnStopReason::StepBudgetExhausted,
        ),
        (
            TurnStopReason::NoProgress,
            RuntimeTurnStopReason::NoProgress,
        ),
        (
            TurnStopReason::Interrupted,
            RuntimeTurnStopReason::Interrupted,
        ),
        (TurnStopReason::Failed, RuntimeTurnStopReason::Failed),
    ] {
        let facts = RuntimeTurnRequestDiagnostics::from(&TurnStopDiagnostics {
            reason: Some(reason),
            ..Default::default()
        });
        assert_eq!(facts.stop_reason, Some(expected));
    }
}

#[test]
fn durable_turn_unknown_drop_promotes_exact_source_once_without_erasing_other_gap() {
    let mut turn = sample_turn("thread", "turn", RuntimeTurnStatus::Completed);
    let route = EffectiveRouteEnvelope::capture(
        None,
        ProviderKind::Deepseek,
        "deepseek",
        "deepseek-v4-flash",
        Some("https://api.deepseek.com/v1"),
        Utc::now(),
    );
    for source in ["late-response", "still-unknown"] {
        assert!(append_routed_usage_drop_record(
            &mut turn,
            RuntimeUsageDropRecord {
                reason: crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown,
                source_id: source.into(),
                route: route.clone(),
            }
        ));
    }
    let mut restored: TurnRecord =
        serde_json::from_slice(&serde_json::to_vec(&turn).unwrap()).unwrap();
    assert!(
        restored
            .routed_usage_drop_records
            .iter()
            .all(|record| record.reason
                == crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown)
    );
    let usage = Usage {
        input_tokens: 17,
        output_tokens: 3,
        ..Default::default()
    };
    let mut changed = route.clone();
    changed.model = "different-model".into();
    assert!(!append_routed_usage_record(
        &mut restored,
        "late-response",
        EffectiveRouteUsage {
            route: changed,
            usage: usage.clone()
        }
    ));
    assert!(append_routed_usage_record(
        &mut restored,
        "late-response",
        EffectiveRouteUsage {
            route: route.clone(),
            usage: usage.clone()
        }
    ));
    assert!(!append_routed_usage_record(
        &mut restored,
        "late-response",
        EffectiveRouteUsage { route, usage }
    ));
    assert_eq!(restored.routed_usage.len(), 1);
    assert_eq!(restored.routed_usage_drop_records.len(), 1);
    assert_eq!(restored.routed_usage_source_ids.len(), 2);
    assert_eq!(
        restored.routed_usage_drop_records[0].source_id,
        routed_usage_source_fingerprint("still-unknown")
    );
}

#[test]
fn runtime_transport_retry_counts_default_old_bytes_and_preserve_new_receipts() {
    let old = serde_json::json!({"modelRequestsStarted":2,"transparentStreamRetries":1,"streamResumes":0});
    let facts: RuntimeTurnRequestDiagnostics = serde_json::from_value(old).unwrap();
    assert_eq!(facts.transport_retries, 0);
    assert_eq!(facts.reasoning_only_reprompts, 0);
    assert_eq!(facts.empty_stop_retries, 0);
    let facts = RuntimeTurnRequestDiagnostics::from(&crate::tool_inspection::TurnStopDiagnostics {
        model_requests_started: 2,
        transport_retries: 3,
        reasoning_only_reprompts: 2,
        empty_stop_retries: 1,
        ..Default::default()
    });
    assert_eq!(facts.transport_retries, 3);
    assert_eq!(facts.reasoning_only_reprompts, 2);
    assert_eq!(facts.empty_stop_retries, 1);
    let encoded = serde_json::to_value(facts).unwrap();
    assert_eq!(encoded["transportRetries"], 3);
    assert_eq!(encoded["reasoningOnlyReprompts"], 2);
    assert_eq!(encoded["emptyStopRetries"], 1);
    assert_eq!(
        serde_json::from_value::<RuntimeTurnRequestDiagnostics>(encoded).unwrap(),
        facts
    );
}

#[tokio::test]
async fn profile_constitution_runtime_admission_binds_the_complete_snapshot_to_replay() -> Result<()>
{
    use codewhale_config::user_constitution::{ProfileConstitution, ProfileConstitutionSnapshot};
    let manager = test_manager(test_runtime_dir())?;
    let thread = manager
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = install_mock_engine(&manager, &thread.id).await;
    let request = StartTurnRequest {
        prompt: "Use this profile".into(),
        operation_key: Some("constitution-once".into()),
        profile_constitution: Some(ProfileConstitutionSnapshot {
            account_id: "acct_fixture".into(),
            revision: 4,
            constitution: ProfileConstitution {
                notes: "Exact preference".into(),
                ..Default::default()
            },
        }),
        ..Default::default()
    };
    let turn = manager.start_turn(&thread.id, request.clone()).await?;
    let Some(Op::SendMessage(spec)) = harness.rx_op.recv().await else {
        bail!("missing Engine operation");
    };
    assert_eq!(spec.profile_constitution, request.profile_constitution);
    assert_eq!(
        manager.start_turn(&thread.id, request.clone()).await?.id,
        turn.id
    );
    let mut changed = request.clone();
    changed
        .profile_constitution
        .as_mut()
        .unwrap()
        .constitution
        .notes = "New preference".into();
    assert!(manager.start_turn(&thread.id, changed).await.is_err());
    let mut changed = request.clone();
    changed.profile_constitution = None;
    assert!(manager.start_turn(&thread.id, changed).await.is_err());
    assert!(harness.rx_op.try_recv().is_err());
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    wait_for_terminal_turn(&manager, &turn.id).await?;
    Ok(())
}
