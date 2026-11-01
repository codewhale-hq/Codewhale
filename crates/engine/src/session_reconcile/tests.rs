use super::*;
use crate::runtime_threads::{
    RuntimeProcessOwnerLock, RuntimeThreadStore, manager::CreateThreadRequest,
    manager::RuntimeThreadManager, manager::RuntimeThreadManagerConfig,
};
use crate::session_manager::create_saved_session_with_id_and_mode;
use crate::test_support::{EnvVarGuard, lock_test_env};
use codewhale_models::{ContentBlock, Message, Role};
use codewhale_runtime::config::Config;

fn text(role: Role, text: &str) -> Message {
    Message {
        role,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    }
}

fn fixture_config() -> Config {
    let mut config = Config::default().with_legacy_root(
        Some("local-reconcile-fixture".into()),
        Some("http://127.0.0.1:1/v1".into()),
    );
    config.set_feature("mcp", false).unwrap();
    config.set_feature("subagents", false).unwrap();
    config
}

/// An isolated Codewhale home whose sessions directory is the configured
/// one, so store confinement holds for fixtures created under it.
struct Fixture {
    sessions: SessionManager,
    home: tempfile::TempDir,
    _runtime: EnvVarGuard,
    _legacy_runtime: EnvVarGuard,
    _tasks: EnvVarGuard,
    _home: EnvVarGuard,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("home");
        let guard = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let runtime = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let legacy_runtime = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        let tasks = EnvVarGuard::remove("CODEWHALE_TASKS_DIR");
        let sessions = SessionManager::default_location().expect("sessions");
        Self {
            sessions,
            home,
            _runtime: runtime,
            _legacy_runtime: legacy_runtime,
            _tasks: tasks,
            _home: guard,
        }
    }

    fn dir(&self) -> PathBuf {
        self.sessions.sessions_dir().to_path_buf()
    }

    /// An opened, empty store at `sessions/<owner>/runtime`.
    fn empty_store(&self, owner: &str) -> PathBuf {
        let path = self.dir().join(owner).join("runtime");
        RuntimeThreadStore::open(path.clone()).expect("open store");
        path
    }

    fn document(&self, id: &str, store: Option<&Path>) -> SavedSession {
        let mut session = create_saved_session_with_id_and_mode(
            id.to_string(),
            &[text(Role::User, "hello"), text(Role::Assistant, "hi")],
            "deepseek-v4-pro",
            self.home.path(),
            0,
            None,
            None,
        );
        session.metadata.runtime_store =
            store.map(|store| RuntimeStoreBinding::for_store_dir(store).expect("binding"));
        self.sessions.save_session(&session).expect("save");
        session
    }

    /// A store holding one task-bound thread with a two-message turn, and no
    /// document bound to it. Returns the store and the thread id.
    async fn store_with_thread(&self, owner: &str) -> (PathBuf, String) {
        let path = self.dir().join(owner).join("runtime");
        let manager = RuntimeThreadManager::open(
            fixture_config(),
            self.home.path().to_path_buf(),
            RuntimeThreadManagerConfig {
                data_dir: path.clone(),
                task_data_dir: self.home.path().join("tasks"),
                sessions_dir: None,
                max_active_threads: 2,
            },
        )
        .expect("manager");
        let thread = manager
            .create_thread(CreateThreadRequest {
                task_id: Some("task_orphaned".into()),
                workspace: Some(self.home.path().to_path_buf()),
                ..CreateThreadRequest::default()
            })
            .await
            .expect("thread");
        manager
            .seed_thread_from_messages(
                &thread.id,
                &[
                    text(Role::User, "rebuild the parser"),
                    text(Role::Assistant, "parser rebuilt"),
                ],
            )
            .await
            .expect("seed");
        drop(manager);
        (path, thread.id)
    }

    fn run(&self) -> ReconcileSummary {
        self.run_with(ReconcileOptions {
            artifact_idle: Duration::ZERO,
            ..ReconcileOptions::default()
        })
    }

    fn run_with(&self, options: ReconcileOptions) -> ReconcileSummary {
        reconcile(&self.sessions, &options).expect("reconcile")
    }

    fn set_aside_root(&self) -> PathBuf {
        self.dir().join(SET_ASIDE_DIR)
    }

    fn receipts(&self) -> String {
        fs::read_to_string(self.dir().join(RECONCILE_DIR).join(RECEIPTS_FILE)).unwrap_or_default()
    }
}

fn manifests(root: &Path) -> String {
    let mut out = String::new();
    if let Ok(runs) = fs::read_dir(root) {
        for run in runs.flatten() {
            out.push_str(&fs::read_to_string(run.path().join(MANIFEST_FILE)).unwrap_or_default());
        }
    }
    out
}

/// The measured shape (#6144): documents are bound to a store under
/// *another* id's directory, and that directory has no document of its own.
/// Only the store nothing binds may move.
/// The reconcile lock sidecar is owner-only, even when an older release left
/// it world-readable.
#[cfg(unix)]
#[test]
fn reconcile_lock_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;
    let _lock = lock_test_env();
    let fixture = Fixture::new();
    let lock = fixture.dir().join(RECONCILE_LOCK_FILE);
    let mode = || fs::metadata(&lock).unwrap().permissions().mode() & 0o777;

    fixture.run();
    assert_eq!(mode(), 0o600);

    fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
    fixture.run();
    assert_eq!(mode(), 0o600);
}

#[test]
fn reconcile_sets_aside_only_stores_no_document_binds() {
    let _env = lock_test_env();
    let fx = Fixture::new();
    let host_store = fx.empty_store("host-anchor");
    fx.document("conversation-a", Some(&host_store));
    fx.document("conversation-b", Some(&host_store));
    let orphan = fx.empty_store("crashed-anchor");

    let summary = fx.run();

    assert_eq!(summary.stores_set_aside, 1, "{summary:?}");
    assert!(host_store.is_dir(), "a bound store must never move");
    assert!(!orphan.exists());
    assert!(
        !fx.dir().join("crashed-anchor").exists(),
        "the emptied directory goes"
    );
    let manifest = manifests(&fx.set_aside_root());
    assert!(manifest.contains("crashed-anchor"), "{manifest}");
    assert!(fx.receipts().contains("store_set_aside"));
    assert_eq!(summary.notice().is_some(), summary.changed());
    assert_eq!(last_run(&fx.dir()), Some(summary));

    // Idempotent: nothing left to do.
    let again = fx.run();
    assert!(!again.changed(), "{again:?}");
}

/// A store whose owner lock is held is in use by a live process: skipped and
/// reported, never moved. Holding that lock through the move is also what
/// makes a concurrent opener fail its lock instead of racing the move.
#[test]
fn reconcile_skips_a_store_a_live_process_holds() {
    let _env = lock_test_env();
    let fx = Fixture::new();
    let held = fx.empty_store("live-host");
    let lock = RuntimeProcessOwnerLock::acquire(&held).expect("hold");

    let summary = fx.run();
    assert_eq!(summary.stores_set_aside, 0);
    assert_eq!(summary.stores_in_use, 1);
    assert!(held.is_dir());

    drop(lock);
    assert_eq!(fx.run().stores_set_aside, 1);
}

/// R3: a store no document binds but that holds a conversation is
/// re-indexed, not set aside, and a second run is a no-op.
#[tokio::test]
async fn reconcile_reindexes_threads_in_an_unbound_store() {
    let _env = lock_test_env();
    let fx = Fixture::new();
    let (store, thread_id) = fx.store_with_thread("worker-anchor").await;

    let summary = fx.run();
    assert_eq!(summary.sessions_recovered, 1, "{summary:?}");
    assert_eq!(summary.stores_set_aside, 0);
    assert!(store.is_dir());

    let id = crate::runtime_threads::thread_session_id(&thread_id);
    let recovered = fx.sessions.load_session(&id).expect("recovered document");
    assert!(recovered.metadata.title.starts_with("Recovered: "));
    assert_eq!(
        recovered.messages,
        vec![
            text(Role::User, "rebuild the parser"),
            text(Role::Assistant, "parser rebuilt")
        ]
    );
    let binding = recovered
        .metadata
        .runtime_store
        .expect("bound to its store");
    assert_eq!(binding.data_dir, store.canonicalize().unwrap());
    let thread = RuntimeThreadStore::open(store.clone())
        .unwrap()
        .load_thread(&thread_id)
        .unwrap();
    assert_eq!(thread.session_id.as_deref(), Some(id.as_str()));
    assert_eq!(
        thread
            .saved_session_checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.messages_len),
        Some(2)
    );

    let again = fx.run();
    assert_eq!(again.sessions_recovered, 0, "{again:?}");
    assert_eq!(again.stores_set_aside, 0);
}

/// R4: a thread in a bound store whose document is gone is unbound, with
/// the old binding in the receipts.
#[tokio::test]
async fn reconcile_unbinds_threads_whose_document_is_gone() {
    let _env = lock_test_env();
    let fx = Fixture::new();
    let (store, thread_id) = fx.store_with_thread("host").await;
    fx.document("keeper", Some(&store));
    {
        let opened = RuntimeThreadStore::open(store.clone()).unwrap();
        let mut thread = opened.load_thread(&thread_id).unwrap();
        thread.session_id = Some("deleted-document".into());
        opened.save_thread(&thread).unwrap();
    }

    let summary = fx.run();
    assert_eq!(summary.threads_unbound, 1, "{summary:?}");
    let thread = RuntimeThreadStore::open(store.clone())
        .unwrap()
        .load_thread(&thread_id)
        .unwrap();
    assert_eq!(thread.session_id, None);
    let receipts = fs::read_to_string(store.join(THREAD_UNBIND_RECEIPTS_FILE)).unwrap();
    assert!(receipts.contains("thread_unbound") && receipts.contains("deleted-document"));
}

/// R4 then R3 in a store no document binds: deleting a document from the
/// session picker leaves the threads naming it bound. They are unbound (with
/// a receipt) and then recovered, instead of keeping the store forever.
#[tokio::test]
async fn reconcile_recovers_threads_in_an_unbound_store_bound_to_a_missing_document() {
    let _env = lock_test_env();
    let fx = Fixture::new();
    let (store, thread_id) = fx.store_with_thread("picker-deleted").await;
    {
        let opened = RuntimeThreadStore::open(store.clone()).unwrap();
        let mut thread = opened.load_thread(&thread_id).unwrap();
        thread.session_id = Some("deleted-from-picker".into());
        opened.save_thread(&thread).unwrap();
    }

    let summary = fx.run();
    assert_eq!(summary.threads_unbound, 1, "{summary:?}");
    assert_eq!(summary.sessions_recovered, 1, "{summary:?}");
    assert_eq!(summary.stores_kept_with_work, 0, "{summary:?}");

    let id = crate::runtime_threads::thread_session_id(&thread_id);
    let recovered = fx.sessions.load_session(&id).expect("recovered document");
    assert!(recovered.metadata.title.starts_with("Recovered: "));
    assert_eq!(
        recovered
            .metadata
            .runtime_store
            .expect("bound to its store")
            .data_dir,
        store.canonicalize().unwrap()
    );
    let thread = RuntimeThreadStore::open(store.clone())
        .unwrap()
        .load_thread(&thread_id)
        .unwrap();
    assert_eq!(thread.session_id.as_deref(), Some(id.as_str()));
    let receipts = fs::read_to_string(store.join(THREAD_UNBIND_RECEIPTS_FILE)).unwrap();
    assert!(receipts.contains("deleted-from-picker"), "{receipts}");

    let again = fx.run();
    assert!(!again.changed(), "{again:?}");
}

/// #6555: reconcile judges seed journals exactly as Runtime startup does,
/// but never settles them. An unpublished seed's turns are not history, and
/// a thread whose journal cannot be settled is not recovered.
#[tokio::test]
async fn reconcile_does_not_recover_an_unpublished_or_unsettled_seed() {
    let _env = lock_test_env();
    for journal_state in ["uncommitted", "unreadable"] {
        let fx = Fixture::new();
        let (store, thread_id) = fx.store_with_thread("seeding-anchor").await;
        let journal_path = store.join("threads").join(format!("{thread_id}.seed"));
        {
            let opened = RuntimeThreadStore::open(store.clone()).unwrap();
            let turns = opened.list_turns_for_thread(&thread_id).unwrap();
            let items = opened.list_items_for_turn(&turns[0].id).unwrap();
            // The process stopped between the seed's records and its commit.
            let mut thread = opened.load_thread(&thread_id).unwrap();
            thread.latest_turn_id = None;
            opened.save_thread(&thread).unwrap();
            let journal = serde_json::json!({
                "thread_id": thread_id,
                "previous_latest_turn_id": null,
                "turn_ids": turns.iter().map(|turn| turn.id.clone()).collect::<Vec<_>>(),
                "item_ids": items.iter().map(|item| item.id.clone()).collect::<Vec<_>>(),
            });
            let bytes = match journal_state {
                "uncommitted" => serde_json::to_vec(&journal).unwrap(),
                _ => b"{truncated seed intent".to_vec(),
            };
            fs::write(&journal_path, bytes).unwrap();
        }

        let summary = fx.run();
        assert_eq!(
            summary.sessions_recovered, 0,
            "{journal_state}: {summary:?}"
        );
        assert_eq!(
            summary.stores_kept_with_work, 1,
            "{journal_state}: {summary:?}"
        );
        assert!(
            journal_path.is_file(),
            "{journal_state}: reconcile leaves the journal for Runtime startup"
        );
        let thread = RuntimeThreadStore::open(store.clone())
            .unwrap()
            .load_thread(&thread_id)
            .unwrap();
        assert_eq!(thread.session_id, None, "{journal_state}");
    }
}

/// R1: an unreadable document is set aside with its hash; a document from a
/// newer build is left alone.
#[test]
fn reconcile_sets_aside_unreadable_documents_and_keeps_newer_ones() {
    let _env = lock_test_env();
    let fx = Fixture::new();
    fs::write(fx.dir().join("torn.json"), b"{\"metadata\": {\"id\": ").unwrap();
    fs::write(
        fx.dir().join("future.json"),
        br#"{"schema_version": 999, "shape": "unknown"}"#,
    )
    .unwrap();

    let summary = fx.run();
    assert_eq!(summary.documents_set_aside, 1, "{summary:?}");
    assert_eq!(summary.documents_newer_schema, 1);
    assert!(!fx.dir().join("torn.json").exists());
    assert!(fx.dir().join("future.json").exists());
    let manifest = manifests(&fx.set_aside_root());
    assert!(
        manifest.contains("torn.json") && manifest.contains("sha256"),
        "{manifest}"
    );
}

/// R6: a document-less artifact directory nothing names is set aside; one a
/// document mentions, or one still being written, is kept.
#[test]
fn reconcile_sets_aside_unreferenced_artifact_directories() {
    let _env = lock_test_env();
    let fx = Fixture::new();
    let lost = "0f0f0f0f-1111-4222-8333-444444444444";
    let named = "0a0a0a0a-1111-4222-8333-555555555555";
    for id in [lost, named] {
        let artifacts = fx.dir().join(id).join("artifacts");
        fs::create_dir_all(&artifacts).unwrap();
        fs::write(artifacts.join("context-transfer-x.json"), b"[]").unwrap();
    }
    let mut mentions = create_saved_session_with_id_and_mode(
        "mentions".into(),
        &[text(Role::User, &format!("see sessions/{named}/artifacts"))],
        "deepseek-v4-pro",
        fx.home.path(),
        0,
        None,
        None,
    );
    mentions.metadata.title = "Mentions".into();
    fx.sessions.save_session(&mentions).unwrap();

    // Recent directories are kept until they have been idle long enough.
    let recent = fx.run_with(ReconcileOptions::default());
    assert_eq!(recent.artifact_dirs_set_aside, 0);

    let summary = fx.run();
    assert_eq!(summary.artifact_dirs_set_aside, 1, "{summary:?}");
    assert_eq!(summary.artifact_dirs_kept, 1);
    assert!(!fx.dir().join(lost).exists());
    assert!(fx.dir().join(named).exists());
}

/// The per-run limit stops a run and the next continues; a second process
/// arriving while one runs does nothing.
#[test]
fn reconcile_is_bounded_and_single_flight() {
    let _env = lock_test_env();
    let fx = Fixture::new();
    fx.empty_store("orphan-one");
    fx.empty_store("orphan-two");

    let first = fx.run_with(ReconcileOptions {
        limit: 1,
        ..ReconcileOptions::default()
    });
    assert_eq!(first.stores_set_aside, 1);
    assert!(first.limit_reached);
    let second = fx.run();
    assert_eq!(second.stores_set_aside, 1);
    assert!(!second.limit_reached);

    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(fx.dir().join(RECONCILE_LOCK_FILE))
        .unwrap();
    assert!(codewhale_util::lock::try_lock_file_exclusive(&lock).unwrap());
    let skipped = fx.run();
    assert!(skipped.skipped_concurrent);
}

/// P2: deleting a document retires the store its *binding* names — not
/// `sessions/<id>/runtime` — unless another document binds it or it holds
/// work. A `runtime-recovered-*` store under the deleted id is never removed.
#[tokio::test]
async fn deleting_a_document_retires_the_store_it_released() {
    let _env = lock_test_env();
    let fx = Fixture::new();
    let shared = fx.empty_store("host-one");
    let sole = fx.empty_store("host-two");
    fx.document("keeps-shared", Some(&shared));
    fx.document("uses-shared", Some(&shared));
    fx.document("uses-sole", Some(&sole));
    let (busy, _) = fx.store_with_thread("host-three").await;
    fx.document("uses-busy", Some(&busy));
    let recovered = fx.dir().join("uses-sole").join("runtime-recovered-session");
    RuntimeThreadStore::open(recovered.clone()).unwrap();
    fx.document("bound-to-recovered", Some(&recovered));

    fx.sessions.delete_session("uses-shared").unwrap();
    fx.sessions.delete_session("uses-sole").unwrap();
    fx.sessions.delete_session("uses-busy").unwrap();

    assert!(shared.is_dir(), "another document still binds it");
    assert!(!sole.exists(), "released and empty: set aside");
    assert!(busy.is_dir(), "holds work: kept");
    assert!(recovered.is_dir(), "bound elsewhere: never deleted");
    assert!(manifests(&fx.set_aside_root()).contains("host-two"));
}

/// P8: concurrent boot-owner stamps from separate handles lose no entry.
#[test]
fn concurrent_boot_owner_stamps_keep_every_entry() {
    let _env = lock_test_env();
    let fx = Fixture::new();
    let ids: Vec<String> = (0..8).map(|index| format!("stamped-{index}")).collect();
    for id in &ids {
        fx.document(id, None);
    }
    let dir = fx.dir();
    std::thread::scope(|scope| {
        for id in &ids {
            let dir = dir.clone();
            scope.spawn(move || {
                let manager = SessionManager::new(dir).unwrap();
                manager.record_session_boot_owner(id, "boot_test").unwrap();
            });
        }
    });
    for id in &ids {
        assert_eq!(
            fx.sessions.session_boot_owner(id).as_deref(),
            Some("boot_test")
        );
    }
}

#[test]
fn summary_notice_and_doctor_line_name_what_changed() {
    let quiet = ReconcileSummary::default();
    assert_eq!(quiet.notice(), None);
    assert!(quiet.doctor_detail().contains("never"));

    let changed = ReconcileSummary {
        ran_at: Some(Utc::now()),
        sessions_recovered: 2,
        threads_unbound: 5,
        stores_set_aside: 140,
        stores_in_use: 1,
        set_aside_path: Some(PathBuf::from("/tmp/set-aside")),
        ..ReconcileSummary::default()
    };
    let notice = changed.notice().unwrap();
    assert!(notice.contains("2 recovered"), "{notice}");
    assert!(notice.contains("5 threads re-linked"), "{notice}");
    assert!(notice.contains("140 unused items set aside"), "{notice}");
    let detail = changed.doctor_detail();
    assert!(detail.contains("140 stores"), "{detail}");
    assert!(detail.contains("1 unbound stores in use"), "{detail}");

    let dry = ReconcileSummary {
        dry_run: true,
        ..changed
    };
    assert_eq!(dry.notice(), None, "a dry run changed nothing");
}
