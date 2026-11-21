use super::*;
use codewhale_engine::automation_manager::{
    AutomationManager, AutomationStatus, CreateAutomationRequest, run_now_shared,
};
use codewhale_engine::runtime_threads::{
    manager::RuntimeThreadManager, manager::RuntimeThreadManagerConfig,
};
use codewhale_engine::task_manager::{TaskManager, TaskManagerConfig};

fn fixture_config() -> Config {
    let mut config = Config::default().with_legacy_root(
        Some("local-runtime-binding-fixture".into()),
        Some("http://127.0.0.1:1/v1".into()),
    );
    config.set_feature("mcp", false).unwrap();
    config.set_feature("subagents", false).unwrap();
    config
}

#[tokio::test]
async fn runtime_store_binding_persists_on_exit_without_a_model_turn() -> anyhow::Result<()> {
    // The body is built on the heap (`boxed_phase`) rather than inline: a
    // switch can now adopt a Runtime store, which grew the load future past
    // what an inline body holds on libtest's test-thread stack.
    crate::test_support::boxed_phase(
        runtime_store_binding_persists_on_exit_without_a_model_turn_body,
    )
    .await
}

async fn runtime_store_binding_persists_on_exit_without_a_model_turn_body() -> anyhow::Result<()> {
    let _environment = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
    let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
    let explicit_store = crate::test_support::EnvVarGuard::set(
        "CODEWHALE_RUNTIME_DIR",
        root.path().join("original-runtime"),
    );
    let mut config = fixture_config();
    let sessions = SessionManager::default_location()?;
    let original = codewhale_engine::session_manager::create_saved_session_with_id_and_mode(
        "legacy-conversation".into(),
        &[text_message("user", "retain this earlier conversation")],
        "deepseek-v4-pro",
        root.path(),
        0,
        None,
        None,
    );
    assert!(original.metadata.runtime_store.is_none());
    sessions.save_session(&original)?;
    let checkpoint = sessions.save_checkpoint(&original)?;
    let checkpoint_before = std::fs::read(&checkpoint)?;
    let original = sessions.load_session_snapshot(&original.metadata.id)?;
    let mut app = Box::new(create_test_app());
    apply_loaded_session_with_goal(&mut app, &mut config, original.clone(), None)
        .map_err(anyhow::Error::msg)?;
    let task_config = TaskManagerConfig::from_runtime(&config, root.path().into(), None, Some(1));
    // Each `TaskManager::start` is a large state machine of its own (the launch
    // path documents ~1.1 MiB for these). Boxed separately so the two do not
    // both occupy this body's frame at once.
    let tasks = {
        let task_config = task_config.clone();
        let config = config.clone();
        let plugin_registry = app.plugin_registry.clone();
        let session_id = original.metadata.id.clone();
        crate::test_support::boxed_phase(move || async move {
            TaskManager::start(task_config, config, plugin_registry, &session_id, None).await
        })
        .await?
    };
    let binding = tasks
        .session_store_binding()
        .expect("attached Runtime store");
    app.runtime_services.task_manager = Some(tasks.clone());
    let (handle, actor) =
        persistence_actor::spawn_persistence_actor(SessionManager::default_location()?);
    // Match clean exit ordering: no Engine turn, checkpoint or snapshot has
    // been queued by this host before its TaskManager stops.
    tasks.shutdown_and_wait().await?;
    assert!(
        super::super::event_loop::persist_settled_session_on_shutdown(&mut app, &handle)
            .map_err(anyhow::Error::msg)?
    );
    assert!(handle.try_send(PersistRequest::Shutdown));
    actor.await?;
    let saved = sessions.load_session(&original.metadata.id)?;
    assert_eq!(saved.metadata.runtime_store.as_ref(), Some(&binding));
    assert_eq!(saved.metadata.title, original.metadata.title);
    assert_eq!(saved.messages, original.messages);
    assert!(
        sessions
            .load_session_checkpoint(&original.metadata.id)?
            .is_some()
    );
    assert_eq!(std::fs::read(checkpoint)?, checkpoint_before);
    drop(app);
    drop(tasks);
    drop(explicit_store);
    // Ordinary resume now reopens the same authority without an env override.
    let resumed = {
        let saved_id = saved.metadata.id.clone();
        let saved_binding = saved.metadata.runtime_store.clone();
        let plugin_root = root.path().to_path_buf();
        crate::test_support::boxed_phase(move || async move {
            TaskManager::start(
                task_config,
                config,
                Arc::new(codewhale_plugins::PluginRegistry::empty(&plugin_root)),
                &saved_id,
                saved_binding.as_ref(),
            )
            .await
        })
        .await?
    };
    assert_eq!(resumed.execution_scope(), binding.execution_scope);
    resumed.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn runtime_store_binding_exit_preserves_inflight_recovery() -> anyhow::Result<()> {
    let _environment = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let sessions = SessionManager::default_location()?;
    // U02-09: a locally cancelled turn still owes its terminal event.
    for (loading, dispatch, cancelled) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let original = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            root.path(),
            0,
            None,
            None,
        );
        let path = sessions.save_session(&original)?;
        let checkpoint = sessions.save_checkpoint(&original)?;
        let saved_before = std::fs::read(&path)?;
        let checkpoint_before = std::fs::read(&checkpoint)?;
        let mut app = Box::new(create_test_app());
        app.current_session_id = Some(original.metadata.id.clone());
        app.is_loading = loading;
        app.dispatch_in_flight = dispatch;
        app.suppress_stream_events_until_turn_complete = cancelled;
        let (handle, actor) =
            persistence_actor::spawn_persistence_actor(SessionManager::default_location()?);
        assert!(
            !super::super::event_loop::persist_settled_session_on_shutdown(&mut app, &handle)
                .map_err(anyhow::Error::msg)?
        );
        assert!(handle.try_send(PersistRequest::Shutdown));
        actor.await?;
        assert_eq!(std::fs::read(path)?, saved_before);
        assert_eq!(std::fs::read(checkpoint)?, checkpoint_before);
    }
    Ok(())
}

/// #6362: this test used to be one async body. Every debug-build temporary
/// of that body — the boxed `App` returns, the cloned `Config`s, two session
/// snapshots, the task-manager futures — got its own slot in a single poll
/// frame, which alone measured 1.1 MiB on the 2 MiB stack libtest gives a
/// test thread (gdb frame attribution, 2026-09-20). The phases below are
/// built and boxed through `boxed_phase`, so each phase's temporaries die
/// with its own poll frame and the outer body holds pointers; inline
/// `async {}` phases measured 806 KiB of never-reused slots on the outer
/// frame and still overflowed. The test pins the default budget explicitly
/// instead of inheriting CI's 16 MiB `RUST_MIN_STACK`, which is what masked
/// the overflow.
#[test]
fn runtime_store_binding_survives_launch_snapshot_and_resume() -> anyhow::Result<()> {
    use crate::test_support::boxed_phase;

    crate::test_support::block_on_default_test_stack(|| async {
        let _environment = crate::test_support::lock_test_env();
        let root = tempfile::tempdir()?;
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let _runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        let config = boxed_phase(|| async { Box::new(fixture_config()) }).await;
        let sessions = SessionManager::default_location()?;
        let task_config =
            TaskManagerConfig::from_runtime(&config, root.path().into(), None, Some(1));
        let (root, config, task_config, sessions) =
            (&root, config.as_ref(), &task_config, &sessions);

        // Phase 1: launch over a saved conversation and bind its Runtime store.
        let (initial_id, saved_id, binding, automation) = boxed_phase(move || async move {
            let mut app = Box::new(create_test_app());
            app.workspace = root.path().into();
            let initial_id = super::super::event_loop::ensure_runtime_session_id(&mut app);
            let tasks = TaskManager::start(
                task_config.clone(),
                config.clone(),
                app.plugin_registry.clone(),
                &initial_id,
                None,
            )
            .await?;
            app.runtime_services.task_manager = Some(tasks.clone());
            // A saved initial conversation may later be deleted while another
            // launch still refers to its Runtime store.
            let initial = build_session_snapshot(&mut app, sessions).map_err(anyhow::Error::msg)?;
            sessions.save_session(&initial)?;
            let launch = begin_launch_session(&mut app, None);
            assert!(!launch.is_error, "{:?}", launch.message);
            assert_ne!(app.current_session_id.as_deref(), Some(initial_id.as_str()));
            let saved = build_session_snapshot(&mut app, sessions).map_err(anyhow::Error::msg)?;
            let binding = saved
                .metadata
                .runtime_store
                .clone()
                .expect("attached host binding");
            assert_eq!(binding.execution_scope, tasks.execution_scope());
            sessions.save_session(&saved)?;
            let mut automations = AutomationManager::open(root.path().join("automations"))?;
            automations.bind_task_manager(&tasks)?;
            let automation = automations.create_automation(CreateAutomationRequest {
                destination_thread_id: None,
                name: "resumed ownership fixture".into(),
                prompt: "local fixture only".into(),
                rrule: "FREQ=HOURLY;INTERVAL=1".into(),
                cwds: vec![root.path().into()],
                model: None,
                model_provider: None,
                model_provider_id: None,
                mode: None,
                allow_shell: Some(false),
                trust_mode: Some(false),
                auto_approve: Some(false),
                delivery_mode: None,
                status: Some(AutomationStatus::Paused),
                disallowed_tools: Vec::new(),
            })?;
            tasks.shutdown_and_wait().await?;
            drop(app);
            drop(tasks);
            drop(automations);
            Ok::<_, anyhow::Error>((initial_id, saved.metadata.id.clone(), binding, automation))
        })
        .await?;
        sessions.delete_session(&initial_id)?;
        assert!(
            binding.data_dir.is_dir(),
            "transcript deletion cannot erase Runtime authority"
        );
        let loaded = sessions.load_session(&saved_id)?;
        assert_eq!(loaded.metadata.runtime_store.as_ref(), Some(&binding));
        let mut resumed_config = boxed_phase(|| async { Box::new(config.clone()) }).await;
        let (loaded, automation) = (&loaded, &automation);

        // Phase 2: resume with the binding and run the automation for real.
        {
            let resumed_config = resumed_config.as_mut();
            boxed_phase(move || async move {
                let mut resumed = Box::new(create_test_app());
                apply_loaded_session_with_goal(&mut resumed, resumed_config, loaded.clone(), None)
                    .map_err(anyhow::Error::msg)?;
                let tasks = TaskManager::start(
                    task_config.clone(),
                    config.clone(),
                    resumed.plugin_registry.clone(),
                    &loaded.metadata.id,
                    loaded.metadata.runtime_store.as_ref(),
                )
                .await?;
                assert_eq!(
                    tasks.execution_scope(),
                    automation.execution_scope.as_deref().unwrap()
                );
                resumed.runtime_services.task_manager = Some(tasks.clone());
                let automations = Arc::new(tokio::sync::Mutex::new(AutomationManager::open(
                    root.path().join("automations"),
                )?));
                // The real Run-now admission must now create its durable
                // receipt. The configured endpoint is closed loopback and no
                // shell/tool is authorized.
                let run = run_now_shared(&automations, &automation.id, &tasks).await?;
                assert!(run.task_id.is_some(), "{run:?}");
                assert_eq!(
                    automations
                        .lock()
                        .await
                        .list_runs(&automation.id, None)?
                        .len(),
                    1
                );
                assert_eq!(
                    automations
                        .lock()
                        .await
                        .get_automation(&automation.id)?
                        .execution_scope,
                    automation.execution_scope
                );
                tasks.shutdown_and_wait().await?;
                drop(resumed);
                drop(tasks);
                Ok::<_, anyhow::Error>(())
            })
            .await?;
        }

        // Phase 3: reproduce the old resume path — deriving a store from the
        // saved conversation id without its binding opens a foreign scope and
        // cannot run.
        // Keep this start future out of the outer poll frame too. Its Config
        // temporaries otherwise stay on the stack underneath every boxed phase.
        let foreign = boxed_phase(move || async move {
            TaskManager::start(
                task_config.clone(),
                config.clone(),
                Arc::new(codewhale_plugins::PluginRegistry::empty(root.path())),
                &loaded.metadata.id,
                None,
            )
            .await
        })
        .await?;
        let foreign = &foreign;
        boxed_phase(move || async move {
            let foreign_automations = Arc::new(tokio::sync::Mutex::new(AutomationManager::open(
                root.path().join("automations"),
            )?));
            let definition_path = root
                .path()
                .join("automations/automations")
                .join(format!("{}.json", automation.id));
            let before_foreign_run = std::fs::read(&definition_path)?;
            let error = run_now_shared(&foreign_automations, &automation.id, foreign)
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("another Runtime execution scope"),
                "{error:#}"
            );
            assert_eq!(
                foreign_automations
                    .lock()
                    .await
                    .list_runs(&automation.id, None)?
                    .len(),
                1
            );
            assert_eq!(std::fs::read(definition_path)?, before_foreign_run);
            Ok::<_, anyhow::Error>(())
        })
        .await?;

        // Phase 4: a host that already owns a foreign scope refuses the switch
        // and keeps its pending input.
        {
            let resumed_config = resumed_config.as_mut();
            boxed_phase(move || async move {
                let mut other_app = Box::new(create_test_app());
                other_app.runtime_services.task_manager = Some(foreign.clone());
                other_app.input = "preserve pending input".into();
                let old_id = other_app.current_session_id.clone();
                // The refusal lives on the switch entry point now, not on the
                // load function behind it: adoption is the switch's job.
                let next_config = resumed_config.clone();
                let error = apply_loaded_session_config_snapshot(
                    &mut other_app,
                    resumed_config,
                    loaded.clone(),
                    next_config,
                    false,
                )
                .await
                .err()
                .expect("a foreign execution scope must refuse");
                // The refusal must name the route that actually works. "Resume
                // it in a new Codewhale process" was true but unactionable:
                // starting a new process and then picking the session from
                // `/resume` returns here, because that is this same switch
                // path (#6207, #6225).
                assert!(
                    error.contains("codewhale resume"),
                    "the refusal must point at the direct-open path: {error}"
                );
                assert!(
                    error.contains(&loaded.metadata.id),
                    "the refusal must name the session to open: {error}"
                );
                assert_eq!(other_app.current_session_id, old_id);
                assert_eq!(other_app.input, "preserve pending input");
                Ok::<_, anyhow::Error>(())
            })
            .await?;
        }
        foreign.shutdown_and_wait().await?;
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn runtime_store_binding_retention_does_not_follow_session_directory_symlinks() -> anyhow::Result<()>
{
    let _environment = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let sessions = SessionManager::default_location()?;
    let saved = codewhale_engine::session_manager::create_saved_session_with_id_and_mode(
        "linked-session".into(),
        &[],
        "fixture",
        root.path(),
        0,
        None,
        None,
    );
    sessions.save_session(&saved)?;
    let target = root.path().join("unrelated-directory");
    std::fs::create_dir_all(&target)?;
    std::fs::write(target.join("keep.txt"), "preserve user data")?;
    let link = root.path().join("sessions/linked-session");
    std::os::unix::fs::symlink(&target, &link)?;
    sessions.delete_session("linked-session")?;
    assert!(!link.exists());
    assert_eq!(
        std::fs::read_to_string(target.join("keep.txt"))?,
        "preserve user data"
    );
    Ok(())
}

#[test]
fn runtime_store_binding_rejects_foreign_missing_or_overridden_store() -> anyhow::Result<()> {
    let _environment = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
    let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
    let config = fixture_config();
    let cfg = RuntimeThreadManagerConfig::for_session(root.path().join("tasks"), "original");
    let runtime = RuntimeThreadManager::open(config.clone(), root.path().into(), cfg.clone())?;
    let binding = runtime.session_store_binding();
    drop(runtime);
    let state_path = binding.data_dir.join("state.json");
    let before = std::fs::read(&state_path)?;
    let mut wrong = binding.clone();
    wrong.execution_scope = "0".repeat(64);
    let open = |binding: &codewhale_engine::runtime_threads::RuntimeStoreBinding| {
        RuntimeThreadManager::open_for_session(
            config.clone(),
            root.path().into(),
            cfg.clone(),
            Arc::new(codewhale_plugins::PluginRegistry::empty(root.path())),
            Some(binding),
        )
    };
    assert!(
        open(&wrong)
            .err()
            .unwrap()
            .to_string()
            .contains("ownership does not match")
    );
    assert_eq!(std::fs::read(&state_path)?, before);
    wrong.data_dir = root.path().join("missing-store");
    assert!(open(&wrong).is_err());
    assert!(
        !wrong.data_dir.exists(),
        "saved binding cannot create a replacement authority"
    );
    let _override = crate::test_support::EnvVarGuard::set(
        "CODEWHALE_RUNTIME_DIR",
        root.path().join("foreign-override"),
    );
    assert!(
        open(&binding)
            .err()
            .unwrap()
            .to_string()
            .contains("override conflicts")
    );
    Ok(())
}

#[test]
fn missing_runtime_store_recovers_without_reusing_authority_or_resurrecting_stale_binding()
-> anyhow::Result<()> {
    let _environment = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
    let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
    let sessions = SessionManager::default_location()?;
    let mut saved = codewhale_engine::session_manager::create_saved_session_with_id_and_mode(
        "interrupted".into(),
        &[text_message("user", "retain my work")],
        "deepseek-v4-pro",
        root.path(),
        0,
        None,
        None,
    );
    let missing = codewhale_engine::runtime_threads::RuntimeStoreBinding {
        data_dir: root.path().join("sessions/previous/runtime"),
        execution_scope: "0".repeat(64),
    };
    saved.metadata.runtime_store = Some(missing.clone());
    sessions.save_session(&saved)?;
    let stale = saved.clone();
    let manager = RuntimeThreadManager::open_for_session(
        fixture_config(),
        root.path().into(),
        RuntimeThreadManagerConfig::for_session(root.path().join("tasks"), "interrupted"),
        Arc::new(codewhale_plugins::PluginRegistry::empty(root.path())),
        Some(&missing),
    )?;
    let recovered = manager.session_store_binding();
    assert_ne!(recovered.execution_scope, missing.execution_scope);
    assert_ne!(recovered.data_dir, missing.data_dir);
    assert!(!missing.data_dir.exists());
    assert!(
        RuntimeThreadManager::open_for_session(
            fixture_config(),
            root.path().into(),
            RuntimeThreadManagerConfig::for_session(root.path().join("tasks"), "interrupted"),
            Arc::new(codewhale_plugins::PluginRegistry::empty(root.path())),
            Some(&missing),
        )
        .is_err(),
        "concurrent recovery cannot mint a second owner"
    );
    // Losing the process before the repaired binding is saved must leave a
    // retryable, session-scoped store, not an orphan or a second authority.
    drop(manager);
    let manager = RuntimeThreadManager::open_for_session(
        fixture_config(),
        root.path().into(),
        RuntimeThreadManagerConfig::for_session(root.path().join("tasks"), "interrupted"),
        Arc::new(codewhale_plugins::PluginRegistry::empty(root.path())),
        Some(&missing),
    )?;
    assert_eq!(manager.session_store_binding(), recovered);
    let other_recovery = RuntimeThreadManager::open_for_session(
        fixture_config(),
        root.path().into(),
        RuntimeThreadManagerConfig::for_session(root.path().join("tasks"), "other-interrupted"),
        Arc::new(codewhale_plugins::PluginRegistry::empty(root.path())),
        Some(&missing),
    )?;
    assert_ne!(
        other_recovery.session_store_binding().data_dir,
        recovered.data_dir
    );
    assert_ne!(
        other_recovery.session_store_binding().execution_scope,
        recovered.execution_scope
    );
    saved.metadata.runtime_store = Some(recovered.clone());
    sessions.save_session(&saved)?;
    sessions.save_session(&stale)?;
    sessions.save_checkpoint(&stale)?;
    let durable = sessions.load_session("interrupted")?;
    assert_eq!(durable.metadata.runtime_store, Some(recovered.clone()));
    assert_eq!(durable.messages, stale.messages);
    let competing = RuntimeThreadManager::open_for_session(
        fixture_config(),
        root.path().into(),
        RuntimeThreadManagerConfig::for_session(root.path().join("tasks"), "competing"),
        Arc::new(codewhale_plugins::PluginRegistry::empty(root.path())),
        None,
    )?;
    let mut competing_snapshot = stale.clone();
    competing_snapshot.metadata.runtime_store = Some(competing.session_store_binding());
    assert!(sessions.save_session(&competing_snapshot).is_err());
    assert!(sessions.save_checkpoint(&competing_snapshot).is_err());
    assert_eq!(
        sessions.load_session("interrupted")?.metadata.runtime_store,
        Some(recovered),
        "another valid owner cannot overwrite the completed recovery"
    );
    Ok(())
}

#[test]
fn picker_recovers_missing_store_into_the_idle_host_and_persists_before_returning()
-> anyhow::Result<()> {
    // Run on the default 2 MiB test stack with the body boxed: a switch can
    // now adopt a Runtime store, which grew `apply_loaded_session_with_goal`'s
    // future past what an inline `#[tokio::test]` body holds. `boxed_phase`
    // keeps those temporaries in the future's own poll frame, and
    // `block_on_default_test_stack` pins the budget libtest really gives.
    crate::test_support::block_on_default_test_stack(|| {
        crate::test_support::boxed_phase(
            picker_recovers_missing_store_into_the_idle_host_and_persists_before_returning_body,
        )
    })
}

async fn picker_recovers_missing_store_into_the_idle_host_and_persists_before_returning_body()
-> anyhow::Result<()> {
    let _environment = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
    let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
    let sessions = SessionManager::default_location()?;
    let mut config = fixture_config();
    let mut saved = codewhale_engine::session_manager::create_saved_session_with_id_and_mode(
        "picker-interrupted".into(),
        &[text_message("user", "retain my work")],
        "deepseek-v4-pro",
        root.path(),
        0,
        None,
        None,
    );
    saved.metadata.runtime_store = Some(codewhale_engine::runtime_threads::RuntimeStoreBinding {
        data_dir: root.path().join("sessions/previous/runtime"),
        execution_scope: "0".repeat(64),
    });
    sessions.save_session(&saved)?;
    let saved = sessions.load_session_snapshot("picker-interrupted")?;
    let mut app = Box::new(create_test_app());
    let tasks = TaskManager::start(
        TaskManagerConfig::from_runtime(&config, root.path().into(), None, Some(1)),
        config.clone(),
        app.plugin_registry.clone(),
        "picker-current",
        None,
    )
    .await?;
    let binding = tasks.session_store_binding().expect("current host");
    app.runtime_services.task_manager = Some(tasks.clone());
    app.current_session_id = Some("picker-current".into());
    app.api_messages_mut()
        .push(text_message("user", "current conversation"));
    let current_messages = app.api_messages.clone();
    let plan_state = app.plan_state.clone();
    let held = plan_state
        .try_lock()
        .expect("hold Work state during recovery");
    assert!(apply_loaded_session_with_goal(&mut app, &mut config, saved.clone(), None).is_err());
    assert_eq!(app.current_session_id.as_deref(), Some("picker-current"));
    assert_eq!(app.api_messages, current_messages);
    assert_eq!(
        sessions
            .load_session("picker-interrupted")?
            .metadata
            .runtime_store
            .as_ref(),
        Some(&binding),
        "binding repair survives a contended UI restore"
    );
    drop(held);
    let stale_error = apply_loaded_session_with_goal(&mut app, &mut config, saved.clone(), None)
        .expect_err("the repaired binding invalidates its original picker source");
    assert!(
        stale_error.contains("changed after its captured source"),
        "{stale_error}"
    );
    assert_eq!(app.current_session_id.as_deref(), Some("picker-current"));
    assert_eq!(app.api_messages, current_messages);
    let recovered = sessions.load_session_snapshot("picker-interrupted")?;
    apply_loaded_session_with_goal(&mut app, &mut config, recovered, None)
        .map_err(anyhow::Error::msg)?;
    assert_eq!(
        app.current_session_id.as_deref(),
        Some("picker-interrupted")
    );
    let durable = sessions.load_session("picker-interrupted")?;
    assert_eq!(durable.metadata.runtime_store.as_ref(), Some(&binding));
    assert_eq!(durable.messages, saved.messages);
    assert_eq!(
        app.current_session_metadata.as_ref().unwrap().runtime_store,
        Some(binding)
    );
    sessions.save_session(&saved)?;
    assert_eq!(
        sessions
            .load_session("picker-interrupted")?
            .metadata
            .runtime_store,
        durable.metadata.runtime_store
    );
    tasks.shutdown_and_wait().await?;
    Ok(())
}

/// #6207: a store that exists but is empty, unheld, and scope-free holds
/// nothing a session switch could abandon. A force-quit leaves exactly that
/// shape — the directory is on disk, ownerless, with zero events — and
/// refusing it left the session unopenable while protecting nothing.
#[test]
fn adoptable_empty_store_reports_nothing_to_abandon() -> anyhow::Result<()> {
    let _environment = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
    let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");

    let store_dir = root.path().join("sessions/interrupted/runtime");
    // Open a real store, so the layout under test is the product's rather than
    // the test's idea of it.
    drop(codewhale_engine::runtime_threads::RuntimeThreadStore::open(
        store_dir.clone(),
    )?);

    let binding = codewhale_engine::runtime_threads::RuntimeStoreBinding {
        data_dir: store_dir.clone(),
        execution_scope: "0".repeat(64),
    };
    assert!(
        !binding.is_missing_session_store()?,
        "the store exists, so the old predicate cannot recover it"
    );
    assert_eq!(
        binding.adoption_refusal()?,
        None,
        "a freshly opened store holds nothing to abandon"
    );
    assert!(
        !binding.has_live_holder()?,
        "nobody holds the freshly opened store"
    );
    assert!(
        !binding.has_scope_pinned_automation()?,
        "no automations exist under the fixture home"
    );
    assert!(
        binding.is_adoptable_empty_store()?,
        "empty, unheld, scope-free: adoptable"
    );

    // Each work directory must be load-bearing on its own. If `open` gains a
    // directory that RUNTIME_STORE_WORK_DIRS misses, this is the assertion that
    // notices, instead of the miss silently widening what a switch will adopt.
    for dir in [
        "threads",
        "turns",
        "items",
        "events",
        "goals",
        "agent-mail",
        "turn-operations",
    ] {
        let marker = store_dir.join(dir).join("work.json");
        std::fs::write(&marker, "{}")?;
        assert_eq!(
            binding.adoption_refusal()?,
            Some(codewhale_engine::runtime_threads::StoreAdoptionRefusal::HasDurableWork { dir }),
            "{dir} holds work; the store must not be adopted"
        );
        assert!(
            !binding.is_adoptable_empty_store()?,
            "{dir} blocks the adopt"
        );
        std::fs::remove_file(&marker)?;
    }
    assert!(
        binding.is_adoptable_empty_store()?,
        "markers removed: adoptable again"
    );
    Ok(())
}

/// #6418: the binding a live host records is its store's canonical root,
/// while the configured sessions root is spelled lexically. When those differ
/// (a Windows verbatim prefix or short name, a symlinked home on Unix), the
/// hand-built bindings above still pass but every *recorded* binding read as
/// unconfined, so an empty, unheld store could never be adopted in-session.
#[test]
fn recorded_binding_is_confined_under_a_non_canonical_home() -> anyhow::Result<()> {
    let _environment = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let real_home = root.path().join("real-home");
    std::fs::create_dir_all(&real_home)?;
    #[cfg(unix)]
    let home = {
        let linked = root.path().join("linked-home");
        std::os::unix::fs::symlink(&real_home, &linked)?;
        linked
    };
    // Windows needs no fixture: canonical paths there carry `\\?\`.
    #[cfg(not(unix))]
    let home = real_home.clone();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
    let _runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
    let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");

    let runtime = RuntimeThreadManager::open(
        fixture_config(),
        home.clone(),
        RuntimeThreadManagerConfig::for_session(home.join("tasks"), "previous"),
    )?;
    let binding = runtime.session_store_binding();
    drop(runtime);

    #[cfg(unix)]
    assert!(
        !binding.data_dir.starts_with(&home),
        "fixture must record a binding spelled differently from the home"
    );
    assert!(
        !binding.is_missing_session_store()?,
        "the recorded store exists"
    );
    assert!(
        binding.is_adoptable_empty_store()?,
        "a recorded, empty, unheld store is confined and adoptable"
    );

    // Confinement still refuses a store outside the sessions root.
    let outside = codewhale_engine::runtime_threads::RuntimeStoreBinding {
        data_dir: root.path().join("elsewhere/previous/runtime"),
        execution_scope: binding.execution_scope.clone(),
    };
    std::fs::create_dir_all(&outside.data_dir)?;
    assert!(!outside.is_adoptable_empty_store()?);
    Ok(())
}

/// #6207: the race that reverted the first fix — a live foreign TaskManager
/// holds the store while its disk state is still empty, so emptiness alone
/// cannot tell abandonment from a holder that has not flushed yet. The
/// process-owner lock is held for the manager's lifetime, which is what
/// distinguishes the two.
#[test]
fn adoptable_empty_store_refuses_a_live_holder() -> anyhow::Result<()> {
    let _environment = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
    let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");

    let store_dir = root.path().join("sessions/held/runtime");
    drop(codewhale_engine::runtime_threads::RuntimeThreadStore::open(
        store_dir.clone(),
    )?);
    let binding = codewhale_engine::runtime_threads::RuntimeStoreBinding {
        data_dir: store_dir.clone(),
        execution_scope: "0".repeat(64),
    };
    assert!(binding.is_adoptable_empty_store()?);

    let _held = codewhale_engine::runtime_threads::RuntimeProcessOwnerLock::acquire(&store_dir)?;
    assert!(
        binding.has_live_holder()?,
        "the held owner lock reads as held"
    );
    assert!(
        !binding.is_adoptable_empty_store()?,
        "a held store must refuse even while its disk state is empty"
    );
    drop(_held);
    assert!(
        !binding.has_live_holder()?,
        "releasing the lock releases the hold"
    );
    assert!(
        binding.is_adoptable_empty_store()?,
        "unheld again: adoptable"
    );
    Ok(())
}

/// #6207: scope-pinned automations are recorded outside the store
/// directories, so an otherwise empty store with one bound to its scope
/// still refuses — adopting it would orphan their scheduled work.
#[test]
fn adoptable_empty_store_refuses_a_scope_pinned_automation() -> anyhow::Result<()> {
    let _environment = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
    let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");

    let store_dir = root.path().join("sessions/pinned/runtime");
    drop(codewhale_engine::runtime_threads::RuntimeThreadStore::open(
        store_dir.clone(),
    )?);
    let scope = "ab".repeat(32);
    let binding = codewhale_engine::runtime_threads::RuntimeStoreBinding {
        data_dir: store_dir.clone(),
        execution_scope: scope.clone(),
    };
    assert!(binding.is_adoptable_empty_store()?);

    let automations = AutomationManager::open(root.path().join("automations"))?;
    let created = automations.create_automation(CreateAutomationRequest {
        destination_thread_id: None,
        name: "scope fixture".into(),
        prompt: "local fixture only".into(),
        rrule: "FREQ=HOURLY;INTERVAL=1".into(),
        cwds: vec![root.path().into()],
        model: None,
        model_provider: None,
        model_provider_id: None,
        mode: None,
        allow_shell: Some(false),
        trust_mode: Some(false),
        auto_approve: Some(false),
        delivery_mode: None,
        status: Some(AutomationStatus::Paused),
        disallowed_tools: Vec::new(),
    })?;
    automations.edit_automation(&created.id, |record| {
        let mut record = record.ok_or_else(|| anyhow::anyhow!("fresh automation must exist"))?;
        record.execution_scope = Some(scope.clone());
        Ok(Some(record))
    })?;
    assert!(
        binding.has_scope_pinned_automation()?,
        "the bound automation is visible from the binding's scope"
    );
    assert!(
        !binding.is_adoptable_empty_store()?,
        "a scope-pinned automation blocks the adopt"
    );
    Ok(())
}

/// #6207 end to end: the picker adopts an existing-but-empty unheld store
/// into the idle host and persists the repaired binding, the way the
/// missing-store path already does.
#[test]
fn picker_adopts_existing_empty_unheld_store() -> anyhow::Result<()> {
    // Run on the default 2 MiB test stack with the body boxed: a switch can
    // now adopt a Runtime store, which grew `apply_loaded_session_with_goal`'s
    // future past what an inline `#[tokio::test]` body holds. `boxed_phase`
    // keeps those temporaries in the future's own poll frame, and
    // `block_on_default_test_stack` pins the budget libtest really gives.
    crate::test_support::block_on_default_test_stack(|| {
        crate::test_support::boxed_phase(picker_adopts_existing_empty_unheld_store_body)
    })
}

async fn picker_adopts_existing_empty_unheld_store_body() -> anyhow::Result<()> {
    let _environment = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
    let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
    let sessions = SessionManager::default_location()?;
    let mut config = fixture_config();

    let store_dir = root.path().join("sessions/previous/runtime");
    drop(codewhale_engine::runtime_threads::RuntimeThreadStore::open(
        store_dir.clone(),
    )?);
    let mut saved = codewhale_engine::session_manager::create_saved_session_with_id_and_mode(
        "picker-adoptable".into(),
        &[text_message("user", "retain my work")],
        "deepseek-v4-pro",
        root.path(),
        0,
        None,
        None,
    );
    saved.metadata.runtime_store = Some(codewhale_engine::runtime_threads::RuntimeStoreBinding {
        data_dir: store_dir,
        execution_scope: "0".repeat(64),
    });
    sessions.save_session(&saved)?;
    let saved = sessions.load_session_snapshot("picker-adoptable")?;

    let mut app = Box::new(create_test_app());
    let tasks = TaskManager::start(
        TaskManagerConfig::from_runtime(&config, root.path().into(), None, Some(1)),
        config.clone(),
        app.plugin_registry.clone(),
        "picker-current",
        None,
    )
    .await?;
    let binding = tasks.session_store_binding().expect("current host");
    app.runtime_services.task_manager = Some(tasks.clone());
    app.current_session_id = Some("picker-current".into());

    apply_loaded_session_with_goal(&mut app, &mut config, saved.clone(), None)
        .map_err(anyhow::Error::msg)?;
    assert_eq!(app.current_session_id.as_deref(), Some("picker-adoptable"));
    let durable = sessions.load_session("picker-adoptable")?;
    assert_eq!(durable.metadata.runtime_store.as_ref(), Some(&binding));
    assert_eq!(durable.messages, saved.messages);
    // #6144 P1a: the store the conversation left is set aside where it was
    // abandoned, not left on disk with nothing pointing at it. The switch
    // hands that off the UI runtime. Moving the store precedes writing its
    // manifest, so wait for both rather than treating the move as completion.
    let abandoned = root.path().join("sessions/previous/runtime");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let set_aside = loop {
        let manifest = std::fs::read_dir(root.path().join("sessions/.set-aside"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|run| {
                std::fs::read_to_string(run.path().join("MANIFEST.jsonl")).unwrap_or_default()
            })
            .collect::<String>();
        if (!abandoned.exists() && manifest.contains("previous"))
            || std::time::Instant::now() >= deadline
        {
            break manifest;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert!(!abandoned.exists());
    assert!(set_aside.contains("previous"), "{set_aside}");
    tasks.shutdown_and_wait().await?;
    Ok(())
}

/// #6144 P1a: an abandoned store another document still binds is kept.
/// Drive on the default 2 MiB test stack, not the caller's.
///
/// `apply_loaded_session_with_goal` grew past what this test's former
/// `#[tokio::test]` body could hold once a switch could adopt a store: the
/// load call, the task manager it rebinds and the session snapshots all share
/// one poll frame. `block_on_default_test_stack` pins the budget libtest
/// actually gives a test thread, so this fails here rather than in CI's
/// 16 MiB `RUST_MIN_STACK`.
#[test]
fn picker_adoption_keeps_a_store_another_document_binds() -> anyhow::Result<()> {
    // `boxed_phase` builds the body future on the heap, so its temporaries
    // live in that future's own poll frame instead of being inlined into the
    // thread's stack frame.
    crate::test_support::block_on_default_test_stack(|| {
        crate::test_support::boxed_phase(picker_adoption_keeps_a_store_another_document_binds_body)
    })
}

async fn picker_adoption_keeps_a_store_another_document_binds_body() -> anyhow::Result<()> {
    let _environment = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
    let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
    let sessions = SessionManager::default_location()?;
    let mut config = fixture_config();

    let store_dir = root.path().join("sessions/previous/runtime");
    drop(codewhale_engine::runtime_threads::RuntimeThreadStore::open(
        store_dir.clone(),
    )?);
    let binding =
        codewhale_engine::runtime_threads::RuntimeStoreBinding::for_store_dir(&store_dir)?;
    let mut saved = codewhale_engine::session_manager::create_saved_session_with_id_and_mode(
        "picker-adoptable".into(),
        &[text_message("user", "retain my work")],
        "deepseek-v4-pro",
        root.path(),
        0,
        None,
        None,
    );
    saved.metadata.runtime_store = Some(binding.clone());
    sessions.save_session(&saved)?;
    let saved = sessions.load_session_snapshot("picker-adoptable")?;
    let mut sibling = codewhale_engine::session_manager::create_saved_session_with_id_and_mode(
        "same-host-sibling".into(),
        &[text_message("user", "saved in the same host")],
        "deepseek-v4-pro",
        root.path(),
        0,
        None,
        None,
    );
    sibling.metadata.runtime_store = Some(binding);
    sessions.save_session(&sibling)?;

    let mut app = Box::new(create_test_app());
    let tasks = TaskManager::start(
        TaskManagerConfig::from_runtime(&config, root.path().into(), None, Some(1)),
        config.clone(),
        app.plugin_registry.clone(),
        "picker-current",
        None,
    )
    .await?;
    app.runtime_services.task_manager = Some(tasks.clone());
    app.current_session_id = Some("picker-current".into());
    // Boxed like the other phases here: libtest gives a test thread a 2 MiB
    // stack, and the load future's state machine outgrew it once the switch
    // gained store adoption (`LoadedSessionOutcome` and its checks).
    crate::test_support::boxed_phase(move || async move {
        apply_loaded_session_with_goal(&mut app, &mut config, saved, None)
            .map_err(anyhow::Error::msg)?;
        // Give the background retirement time to (wrongly) act.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(store_dir.is_dir(), "the sibling still binds it");
        Ok::<_, anyhow::Error>(())
    })
    .await?;
    tasks.shutdown_and_wait().await?;
    Ok(())
}

/// A store that holds only finished history is no longer a reason to refuse a
/// switch.
///
/// The rule this replaces refused any target whose `threads/` directory held a
/// record, so a conversation that had ever run under the Runtime could not be
/// opened from inside a running session even though nothing in it was pending.
/// The switch does not abandon the target — that store stays on disk for
/// whichever process opens it next — so the only question worth asking is
/// whether the store being *left* still needs this process.
#[test]
fn switch_adopts_a_store_holding_finished_history() -> anyhow::Result<()> {
    // Run on the default 2 MiB test stack with the body boxed: a switch can
    // now adopt a Runtime store, which grew `apply_loaded_session_with_goal`'s
    // future past what an inline `#[tokio::test]` body holds. `boxed_phase`
    // keeps those temporaries in the future's own poll frame, and
    // `block_on_default_test_stack` pins the budget libtest really gives.
    crate::test_support::block_on_default_test_stack(|| {
        crate::test_support::boxed_phase(switch_adopts_a_store_holding_finished_history_body)
    })
}

async fn switch_adopts_a_store_holding_finished_history_body() -> anyhow::Result<()> {
    let _environment = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
    let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
    let sessions = SessionManager::default_location()?;
    let mut config = fixture_config();

    // A populated target store: one thread record, which is what
    // `first_durable_work_dir` reports as `HasDurableWork { dir: "threads" }`.
    let target_store = root.path().join("sessions/populated/runtime");
    {
        let store = crate::runtime_threads::RuntimeThreadStore::open(target_store.clone())?;
        let thread = fixture_thread("thr_finished", config.default_model(), root.path());
        store.save_thread(&thread)?;
    }
    let mut saved = crate::session_manager::create_saved_session_with_id_and_mode(
        "populated-target".into(),
        &[text_message("user", "long conversation")],
        "deepseek-v4-pro",
        root.path(),
        0,
        None,
        None,
    );
    // The real binding, not a fabricated scope: adoption validates the store's
    // own owner file, and a made-up scope is the *stale* case (recovered onto
    // the current host instead).
    saved.metadata.runtime_store = Some(
        crate::runtime_threads::RuntimeStoreBinding::for_store_dir(&target_store)?,
    );
    sessions.save_session(&saved)?;

    let mut app = Box::new(create_test_app());
    // Boxed: `TaskManager::start` is a large state machine and would otherwise
    // share this body's frame with the load-and-adopt call below.
    let task_config = TaskManagerConfig::from_runtime(&config, root.path().into(), None, Some(1));
    let start_config = config.clone();
    let plugin_registry = app.plugin_registry.clone();
    let tasks = crate::test_support::boxed_phase(move || async move {
        TaskManager::start(
            task_config,
            start_config,
            plugin_registry,
            "switch-current",
            None,
        )
        .await
    })
    .await?;
    app.runtime_services.task_manager = Some(tasks.clone());
    app.current_session_id = Some("switch-current".into());

    // Drive the real entry point, not the load function behind it: adoption is
    // what the switch does, so the assertion belongs on the host's owner.
    let expected_store = target_store.clone();
    let next_config = config.clone();
    apply_loaded_session_config_snapshot(&mut app, &mut config, saved, next_config, false)
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(app.current_session_id.as_deref(), Some("populated-target"));
    // The host must now own the target store. Compare canonical paths: the
    // adopted binding is the store's own canonical root, while the fixture
    // path may be the /var symlink.
    let held = app
        .runtime_services
        .task_manager
        .as_ref()
        .expect("host keeps a manager")
        .session_store_binding()
        .expect("host holds a store");
    let held_dir = held
        .data_dir
        .canonicalize()
        .unwrap_or_else(|_| held.data_dir.clone());
    let expected_dir = expected_store
        .canonicalize()
        .unwrap_or_else(|_| expected_store.clone());
    assert_eq!(held_dir, expected_dir, "the host adopted the target store");
    assert!(target_store.is_dir(), "the target store is never discarded");
    if let Some(tasks) = app.runtime_services.task_manager.clone() {
        tasks.shutdown_and_wait().await?;
    }
    Ok(())
}

/// A refused switch leaves the live session, its workspace and its manager
/// exactly as they were.
///
/// This is the case the review named: adoption used to run *after* the load had
/// already replaced the session id and metadata, and it took the manager out of
/// the App before trying to open the target. A refusal could therefore strand a
/// host with no manager while the UI showed the new conversation. Adoption is
/// now preflight, so a refusal costs nothing.
#[test]
fn refused_switch_keeps_the_live_session_and_its_manager() -> anyhow::Result<()> {
    // Run on the default 2 MiB test stack with the body boxed: a switch can
    // now adopt a Runtime store, which grew `apply_loaded_session_with_goal`'s
    // future past what an inline `#[tokio::test]` body holds. `boxed_phase`
    // keeps those temporaries in the future's own poll frame, and
    // `block_on_default_test_stack` pins the budget libtest really gives.
    crate::test_support::block_on_default_test_stack(|| {
        crate::test_support::boxed_phase(refused_switch_keeps_the_live_session_and_its_manager_body)
    })
}

async fn refused_switch_keeps_the_live_session_and_its_manager_body() -> anyhow::Result<()> {
    let _environment = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
    let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
    let sessions = SessionManager::default_location()?;
    let mut config = fixture_config();

    let target_store = root.path().join("sessions/target/runtime");
    {
        let store = crate::runtime_threads::RuntimeThreadStore::open(target_store.clone())?;
        store.save_thread(&fixture_thread(
            "thr_target",
            config.default_model(),
            root.path(),
        ))?;
    }
    let mut saved = crate::session_manager::create_saved_session_with_id_and_mode(
        "refused-target".into(),
        &[text_message("user", "elsewhere")],
        "deepseek-v4-pro",
        root.path(),
        0,
        None,
        None,
    );
    saved.metadata.runtime_store = Some(
        crate::runtime_threads::RuntimeStoreBinding::for_store_dir(&target_store)?,
    );
    sessions.save_session(&saved)?;

    let mut app = Box::new(create_test_app());
    // Boxed: `TaskManager::start` is a large state machine, and it would share
    // this body's frame with the load call below.
    let task_config = TaskManagerConfig::from_runtime(&config, root.path().into(), None, Some(1));
    let start_config = config.clone();
    let plugin_registry = app.plugin_registry.clone();
    let tasks = crate::test_support::boxed_phase(move || async move {
        TaskManager::start(
            task_config,
            start_config,
            plugin_registry,
            "waiting-current",
            None,
        )
        .await
    })
    .await?;
    app.runtime_services.task_manager = Some(tasks.clone());
    app.current_session_id = Some("waiting-current".into());
    app.input = "keep my draft".into();

    // Refuse the switch the way a live turn does: `session_transition_blocked`
    // is consulted before anything is released, so the failure must cost
    // nothing. A request waiting on this process is the other refusal
    // (`switch_refuses_while_requests_are_waiting`'s predecessor did not exist;
    // the in-memory half is covered by `has_unsettled_requests`).
    app.runtime_turn_status = Some("in_progress".to_string());
    let current = tasks.session_store_binding().expect("host holds a store");
    let next_config = config.clone();
    let (app, error) = crate::test_support::boxed_phase(move || async move {
        let result =
            apply_loaded_session_config_snapshot(&mut app, &mut config, saved, next_config, false)
                .await;
        (app, result)
    })
    .await;
    let error = error.expect_err("a store with an active turn must not be released");
    assert!(
        error.contains("runtime work is active"),
        "the refusal must name the active work: {error}"
    );
    // Nothing about the live session moved.
    assert_eq!(app.current_session_id.as_deref(), Some("waiting-current"));
    assert_eq!(app.input, "keep my draft");
    let held = app
        .runtime_services
        .task_manager
        .as_ref()
        .expect("the manager must survive a refused switch")
        .session_store_binding()
        .expect("host holds a store");
    let held_dir = held
        .data_dir
        .canonicalize()
        .unwrap_or_else(|_| held.data_dir.clone());
    let expected_dir = current
        .data_dir
        .canonicalize()
        .unwrap_or_else(|_| current.data_dir.clone());
    assert_eq!(held_dir, expected_dir, "the host kept its own store");
    tasks.shutdown_and_wait().await?;
    Ok(())
}

/// A minimal durable thread record: enough for `first_durable_work_dir` to see
/// the `threads/` directory as non-empty, which is the whole condition under
/// test. Field values are otherwise inert.
fn fixture_thread(
    id: &str,
    model: String,
    workspace: &std::path::Path,
) -> crate::runtime_threads::ThreadRecord {
    crate::runtime_threads::ThreadRecord {
        schema_version: 2,
        id: id.to_string(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        model: model.to_string(),
        model_provider: None,
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
        title: None,
        parent_thread_id: None,
        last_read: None,
        session_id: None,
        saved_session_checkpoint: None,
        executor: None,
        environment: None,
        agent_preset: None,
    }
}
