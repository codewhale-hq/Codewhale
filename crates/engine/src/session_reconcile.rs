//! Repair of the saved-session store (#6144).
//!
//! The session document is the one authority for a conversation. Two links
//! hang off it, each with exactly one home: document → Runtime store in
//! `metadata.runtime_store`, and Runtime thread → document in
//! `ThreadRecord.session_id` plus its checkpoint. A directory's *name* is not
//! ownership: a host store sits under whichever id the process booted with,
//! and every conversation that process saved binds it.
//!
//! The write paths now keep those links consistent (stores are retired where
//! they are abandoned, export is idempotent, deleting a document unbinds its
//! threads, checkpoints cover a prefix). What they cannot prevent — a crash
//! or kill between two writes, and everything earlier builds left behind — is
//! repaired here, on load:
//!
//! - **R1** an unreadable document is set aside (a newer-schema one is left in
//!   place and reported);
//! - **R2/R5** a store no document binds, holding no work, is set aside while
//!   its process-owner lock is held, so no process can open it mid-move; a
//!   store another process holds is skipped and counted as in use;
//! - **R3** a store no document binds that holds threads is re-indexed: one
//!   "Recovered:" document per thread, bound to the store, with the thread
//!   bound back to it;
//! - **R4** a thread bound to a document that no longer exists is unbound
//!   (the old binding goes in the receipts) and loads from its own turns;
//! - **R6** a directory with no document, no store and only artifacts or
//!   approval receipts is set aside when no document, thread or derived
//!   thread id names it and it has been untouched for a day.
//!
//! Repair only ever *moves* things, into `sessions/.set-aside/<run>/`, with a
//! `MANIFEST.jsonl` naming each original path; it never unlinks. Every action
//! appends a line to `sessions/.reconcile/receipts.jsonl` (a dropped thread
//! binding is recorded in its store's `session-unbind-receipts.jsonl`), and
//! the run's summary is kept in `sessions/.reconcile/last.json` for
//! `codewhale doctor`.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::runtime_threads::{RuntimeStoreBinding, ThreadRecord};
use crate::session_manager::{SavedSession, SessionManager, is_runtime_store_dir_name};

pub const SET_ASIDE_DIR: &str = ".set-aside";
const RECONCILE_DIR: &str = ".reconcile";
const RECONCILE_LOCK_FILE: &str = ".reconcile.lock";
const RECEIPTS_FILE: &str = "receipts.jsonl";
const LAST_RUN_FILE: &str = "last.json";
const MANIFEST_FILE: &str = "MANIFEST.jsonl";
const LATE_USAGE_DIR: &str = ".late-usage";
const CHECKPOINTS_DIR: &str = "checkpoints";
const SESSION_BOOT_OWNERS_FILE: &str = "session_boot_owners.json";
/// Upper bound on actions per run; the rest waits for the next launch.
pub const DEFAULT_ACTION_LIMIT: usize = 500;
/// A document-less artifact directory has no lock to prove nobody is still
/// writing it (an older build's engine, say), so it must also be idle.
const ARTIFACT_DIR_IDLE: Duration = Duration::from_secs(24 * 60 * 60);

/// What one reconcile run did (or, dry, would do).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileSummary {
    pub ran_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub dry_run: bool,
    /// Another process was already reconciling; nothing was examined.
    #[serde(default)]
    pub skipped_concurrent: bool,
    /// R1: unreadable documents set aside.
    #[serde(default)]
    pub documents_set_aside: usize,
    /// Documents written by a newer build, left in place.
    #[serde(default)]
    pub documents_newer_schema: usize,
    /// R2/R5: unbound, empty Runtime stores set aside.
    #[serde(default)]
    pub stores_set_aside: usize,
    /// Unbound stores that hold work with no thread to re-index; kept.
    #[serde(default)]
    pub stores_kept_with_work: usize,
    /// Unbound stores a live process holds; left for a later run.
    #[serde(default)]
    pub stores_in_use: usize,
    /// R3: "Recovered:" documents written for threads in unbound stores.
    #[serde(default)]
    pub sessions_recovered: usize,
    /// R4: threads whose document no longer exists, unbound.
    #[serde(default)]
    pub threads_unbound: usize,
    /// R6: document-less artifact directories set aside.
    #[serde(default)]
    pub artifact_dirs_set_aside: usize,
    /// Document-less directories kept because something names them, they
    /// are recent, or they hold something this repair does not recognise.
    #[serde(default)]
    pub artifact_dirs_kept: usize,
    /// The per-run action limit stopped this run; the next one continues.
    #[serde(default)]
    pub limit_reached: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_aside_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

impl ReconcileSummary {
    fn actions(&self) -> usize {
        self.documents_set_aside
            + self.stores_set_aside
            + self.sessions_recovered
            + self.threads_unbound
            + self.artifact_dirs_set_aside
    }

    /// Whether this run changed anything on disk.
    #[must_use]
    pub fn changed(&self) -> bool {
        !self.dry_run && self.actions() > 0
    }

    /// One line for the first frame, only when something changed.
    #[must_use]
    pub fn notice(&self) -> Option<String> {
        if !self.changed() {
            return None;
        }
        let mut parts = Vec::new();
        if self.sessions_recovered > 0 {
            parts.push(format!("{} recovered", self.sessions_recovered));
        }
        if self.threads_unbound > 0 {
            parts.push(format!("{} threads re-linked", self.threads_unbound));
        }
        let set_aside =
            self.stores_set_aside + self.artifact_dirs_set_aside + self.documents_set_aside;
        if set_aside > 0 {
            parts.push(format!("{set_aside} unused items set aside"));
        }
        let mut line = format!("Sessions repaired: {}", parts.join(", "));
        if let Some(path) = &self.set_aside_path {
            line.push_str(&format!(" → {}", path.display()));
        }
        Some(line)
    }

    /// The `codewhale doctor` detail for this run.
    #[must_use]
    pub fn doctor_detail(&self) -> String {
        let when = self
            .ran_at
            .map_or_else(|| "never".to_string(), |at| at.to_rfc3339());
        let mut line = format!(
            "last repair {when}: {} recovered, {} threads unbound, {} stores + {} artifact dirs + {} unreadable documents set aside, {} newer-schema documents left in place, {} unbound stores in use by another process, {} kept with work",
            self.sessions_recovered,
            self.threads_unbound,
            self.stores_set_aside,
            self.artifact_dirs_set_aside,
            self.documents_set_aside,
            self.documents_newer_schema,
            self.stores_in_use,
            self.stores_kept_with_work,
        );
        if self.limit_reached {
            line.push_str("; more remains for the next run");
        }
        if let Some(path) = &self.set_aside_path {
            line.push_str(&format!("; set aside under {}", path.display()));
        }
        if !self.errors.is_empty() {
            line.push_str(&format!("; {} errors", self.errors.len()));
        }
        line
    }
}

#[derive(Debug, Clone)]
pub struct ReconcileOptions {
    /// Examine and count, but move and write nothing.
    pub dry_run: bool,
    pub limit: usize,
    /// A document being resumed right now; never touched.
    pub skip_session: Option<String>,
    /// How long a document-less artifact directory must sit untouched.
    pub artifact_idle: Duration,
}

impl Default for ReconcileOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            limit: DEFAULT_ACTION_LIMIT,
            skip_session: None,
            artifact_idle: ARTIFACT_DIR_IDLE,
        }
    }
}

/// The summary of the last completed (non-dry) run, if any.
#[must_use]
pub fn last_run(sessions_dir: &Path) -> Option<ReconcileSummary> {
    let raw = fs::read(sessions_dir.join(RECONCILE_DIR).join(LAST_RUN_FILE)).ok()?;
    serde_json::from_slice(&raw).ok()
}

static PENDING_NOTICE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The first-frame notice a background run left, taken once.
pub fn take_pending_notice() -> Option<String> {
    PENDING_NOTICE.lock().ok()?.take()
}

/// Reconcile the default sessions store on a blocking worker. Called once a
/// host holds its own Runtime store, so that store and any store the launch
/// just opened read as in use rather than as candidates.
pub fn spawn_background_reconcile(skip_session: Option<String>) {
    tokio::task::spawn_blocking(move || {
        let Ok(manager) = SessionManager::default_location() else {
            return;
        };
        let options = ReconcileOptions {
            skip_session,
            ..ReconcileOptions::default()
        };
        match reconcile(&manager, &options) {
            Ok(summary) => {
                if let Some(notice) = summary.notice()
                    && let Ok(mut pending) = PENDING_NOTICE.lock()
                {
                    *pending = Some(notice);
                }
            }
            Err(error) => tracing::warn!(%error, "session store repair did not run"),
        }
    });
}

/// Run one bounded reconcile pass over `sessions`' store.
pub fn reconcile(
    sessions: &SessionManager,
    options: &ReconcileOptions,
) -> io::Result<ReconcileSummary> {
    let sessions_dir = sessions.sessions_dir().to_path_buf();
    let mut summary = ReconcileSummary {
        ran_at: Some(Utc::now()),
        dry_run: options.dry_run,
        ..ReconcileSummary::default()
    };
    let lock_file =
        crate::session_manager::open_private_lock_file(&sessions_dir.join(RECONCILE_LOCK_FILE))?;
    if !codewhale_util::lock::try_lock_file_exclusive(&lock_file)? {
        summary.skipped_concurrent = true;
        return Ok(summary);
    }
    let mut run = Run {
        sessions,
        canonical_root: sessions_dir
            .canonicalize()
            .unwrap_or_else(|_| sessions_dir.clone()),
        sessions_dir,
        options,
        set_aside: SetAside::default(),
        summary,
    };
    run.execute();
    let summary = run.summary;
    if !options.dry_run {
        let dir = sessions.sessions_dir().join(RECONCILE_DIR);
        fs::create_dir_all(&dir)?;
        let bytes = serde_json::to_vec_pretty(&summary).map_err(io::Error::other)?;
        codewhale_util::utils::write_atomic(&dir.join(LAST_RUN_FILE), &bytes)?;
    }
    drop(lock_file);
    Ok(summary)
}

/// Receipts of thread bindings dropped in a store, kept in that store's root
/// (outside its work directories, so they never make it look busy).
pub const THREAD_UNBIND_RECEIPTS_FILE: &str = "session-unbind-receipts.jsonl";

/// Record the binding a thread is losing, next to the thread itself: the
/// store is the one place every writer that unbinds (the thread's own host,
/// the Runtime API, reconcile) already knows.
pub fn record_thread_unbound(store_dir: &Path, thread: &ThreadRecord, reason: &str) {
    let receipt = json!({
        "action": "thread_unbound",
        "thread_id": thread.id,
        "session_id": thread.session_id,
        "saved_session_checkpoint": thread.saved_session_checkpoint,
        "reason": reason,
        "at": Utc::now(),
    });
    let result = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(store_dir.join(THREAD_UNBIND_RECEIPTS_FILE))
        .and_then(|mut file| {
            let mut line = serde_json::to_vec(&receipt).map_err(io::Error::other)?;
            line.push(b'\n');
            file.write_all(&line)
        });
    if let Err(error) = result {
        tracing::warn!(%error, thread_id = %thread.id, "thread unbind receipt was not written");
    }
}

/// Set aside `store` when no document or checkpoint binds it, no process
/// holds it, and it holds no work. Returns where it went. Used where a store
/// is abandoned — a switch that rebinds its conversation elsewhere, a delete
/// — so the abandonment does not wait for the next reconcile.
pub fn retire_unbound_store(
    sessions: &SessionManager,
    store: &Path,
    reason: &str,
) -> Option<PathBuf> {
    if !store.is_dir() {
        return None;
    }
    let references = collect_document_references(sessions.sessions_dir(), false);
    if references.binds(store) {
        return None;
    }
    let binding = RuntimeStoreBinding::for_store_dir(store).ok()?;
    let held = binding.try_hold().ok().flatten()?;
    if !matches!(held.keep_reason(), Ok(None)) {
        return None;
    }
    let canonical_root = sessions
        .sessions_dir()
        .canonicalize()
        .unwrap_or_else(|_| sessions.sessions_dir().to_path_buf());
    let mut set_aside = SetAside::default();
    let original = held.binding().data_dir.clone();
    let target = set_aside
        .target_for(sessions.sessions_dir(), &canonical_root, &original)
        .ok()?;
    if let Err(error) = held.move_to(&target) {
        tracing::debug!(store = %original.display(), %error, "released store was not set aside");
        return None;
    }
    set_aside.manifest(&original, &target, reason, json!({"kind": "runtime_store"}));
    append_receipt(
        sessions.sessions_dir(),
        json!({"action": "store_set_aside", "path": original, "moved_to": target, "reason": reason}),
    );
    remove_if_empty(original.parent());
    Some(target)
}

/// [`retire_unbound_store`] off the calling thread when a Tokio runtime is
/// running (a session switch runs on the UI runtime and must not wait on a
/// scan of every document), inline otherwise.
pub fn retire_unbound_store_in_background(
    sessions: SessionManager,
    store: PathBuf,
    reason: &'static str,
) {
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => {
            runtime.spawn_blocking(move || retire_unbound_store(&sessions, &store, reason));
        }
        Err(_) => {
            retire_unbound_store(&sessions, &store, reason);
        }
    }
}

fn append_receipt(sessions_dir: &Path, mut receipt: Value) {
    if let Value::Object(map) = &mut receipt {
        map.insert("at".into(), json!(Utc::now()));
    }
    let dir = sessions_dir.join(RECONCILE_DIR);
    let result = fs::create_dir_all(&dir).and_then(|()| {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(RECEIPTS_FILE))?;
        let mut line = serde_json::to_vec(&receipt).map_err(io::Error::other)?;
        line.push(b'\n');
        file.write_all(&line)
    });
    if let Err(error) = result {
        tracing::warn!(%error, "session repair receipt was not written");
    }
}

fn remove_if_empty(dir: Option<&Path>) {
    if let Some(dir) = dir
        && fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_none())
    {
        let _ = fs::remove_dir(dir);
    }
}

/// One `.set-aside/<run>/` directory, created on first use.
#[derive(Default)]
struct SetAside {
    run_dir: Option<PathBuf>,
}

impl SetAside {
    fn target_for(
        &mut self,
        sessions_dir: &Path,
        canonical_root: &Path,
        original: &Path,
    ) -> io::Result<PathBuf> {
        let run_dir = match &self.run_dir {
            Some(dir) => dir.clone(),
            None => {
                let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
                let suffix = &uuid::Uuid::new_v4().simple().to_string()[..6];
                let dir = sessions_dir
                    .join(SET_ASIDE_DIR)
                    .join(format!("{stamp}-{suffix}"));
                fs::create_dir_all(&dir)?;
                self.run_dir = Some(dir.clone());
                dir
            }
        };
        let relative = original
            .strip_prefix(canonical_root)
            .or_else(|_| original.strip_prefix(sessions_dir))
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| {
                original
                    .file_name()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("unnamed"))
            });
        Ok(run_dir.join(relative))
    }

    fn manifest(&self, original: &Path, moved_to: &Path, reason: &str, detail: Value) {
        let Some(run_dir) = &self.run_dir else {
            return;
        };
        let line = json!({
            "original": original,
            "moved_to": moved_to,
            "reason": reason,
            "detail": detail,
            "at": Utc::now(),
        });
        let result = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(run_dir.join(MANIFEST_FILE))
            .and_then(|mut file| {
                let mut bytes = serde_json::to_vec(&line).map_err(io::Error::other)?;
                bytes.push(b'\n');
                file.write_all(&bytes)
            });
        if let Err(error) = result {
            tracing::warn!(%error, "set-aside manifest entry was not written");
        }
    }
}

/// Everything that names a session id or a store.
#[derive(Default)]
struct References {
    /// Ids with a readable document or a crash-recovery checkpoint.
    documents: HashSet<String>,
    /// Stores some document or checkpoint binds (canonical where possible).
    bound_stores: HashSet<PathBuf>,
    /// Ids named by a Runtime thread, or derived from one.
    thread_named: HashSet<String>,
    /// Every uuid-shaped token found in any document's text.
    document_tokens: HashSet<String>,
    /// Document texts, kept only to answer non-uuid ids (rare).
    document_texts: Vec<String>,
}

impl References {
    fn binds(&self, store: &Path) -> bool {
        self.bound_stores.contains(&canonical(store)) || self.bound_stores.contains(store)
    }

    fn names(&self, id: &str) -> bool {
        self.documents.contains(id)
            || self.thread_named.contains(id)
            || self.document_tokens.contains(id)
            || (!looks_like_uuid(id) && self.document_texts.iter().any(|text| text.contains(id)))
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn looks_like_uuid(id: &str) -> bool {
    id.len() == 36 && uuid::Uuid::parse_str(id).is_ok()
}

fn uuid_tokens(bytes: &[u8]) -> impl Iterator<Item = String> + '_ {
    static UUID: std::sync::LazyLock<regex::bytes::Regex> = std::sync::LazyLock::new(|| {
        regex::bytes::Regex::new(
            "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
        )
        .expect("uuid pattern")
    });
    UUID.find_iter(bytes)
        .map(|found| String::from_utf8_lossy(found.as_bytes()).to_ascii_lowercase())
}

fn document_id(path: &Path) -> Option<String> {
    if path.extension().is_none_or(|ext| ext != "json")
        || path
            .file_name()
            .is_some_and(|name| name == SESSION_BOOT_OWNERS_FILE)
    {
        return None;
    }
    let id = path.file_stem()?.to_str()?;
    codewhale_runtime::artifacts::is_valid_session_id(id).then(|| id.to_string())
}

/// Store bindings and ids of every readable document and checkpoint. With
/// `keep_text`, also collect what R6 needs to know which ids documents mention.
fn collect_document_references(sessions_dir: &Path, keep_text: bool) -> References {
    let mut references = References::default();
    let scan = |dir: &Path, references: &mut References, keep_text: bool| {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(id) = document_id(&path) else {
                continue;
            };
            let Ok(metadata) = SessionManager::load_session_metadata(&path) else {
                continue;
            };
            references.documents.insert(id);
            if let Some(binding) = metadata.runtime_store {
                references.bound_stores.insert(canonical(&binding.data_dir));
                references.bound_stores.insert(binding.data_dir);
            }
            if keep_text && let Ok(bytes) = fs::read(&path) {
                references.document_tokens.extend(uuid_tokens(&bytes));
                references
                    .document_texts
                    .push(String::from_utf8_lossy(&bytes).into_owned());
            }
        }
    };
    scan(sessions_dir, &mut references, keep_text);
    scan(
        &sessions_dir.join(CHECKPOINTS_DIR),
        &mut references,
        keep_text,
    );
    references
}

/// One store directory under a session directory.
struct StoreDir {
    owner_id: String,
    path: PathBuf,
}

struct Run<'a> {
    sessions: &'a SessionManager,
    sessions_dir: PathBuf,
    canonical_root: PathBuf,
    options: &'a ReconcileOptions,
    set_aside: SetAside,
    summary: ReconcileSummary,
}

impl Run<'_> {
    fn budget_left(&mut self) -> bool {
        if self.summary.actions() >= self.options.limit {
            self.summary.limit_reached = true;
            return false;
        }
        true
    }

    fn error(&mut self, context: &str, error: impl std::fmt::Display) {
        self.summary.errors.push(format!("{context}: {error}"));
    }

    fn receipt(&self, receipt: Value) {
        if !self.options.dry_run {
            append_receipt(&self.sessions_dir, receipt);
        }
    }

    fn execute(&mut self) {
        self.repair_documents();
        let mut references = collect_document_references(&self.sessions_dir, true);
        let stores = self.store_dirs();
        self.collect_thread_references(&stores, &mut references);
        for store in &stores {
            if !self.budget_left() {
                return;
            }
            self.repair_store(store, &references);
        }
        self.repair_artifact_dirs(&references);
        if let Some(dir) = &self.set_aside.run_dir {
            self.summary.set_aside_path = Some(dir.clone());
        }
    }

    /// R1: set aside documents that cannot be read at all.
    fn repair_documents(&mut self) {
        let Ok(entries) = fs::read_dir(&self.sessions_dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        for path in paths {
            let Some(id) = document_id(&path) else {
                continue;
            };
            if self.options.skip_session.as_deref() == Some(id.as_str())
                || SessionManager::load_session_metadata(&path).is_ok()
            {
                continue;
            }
            match classify_unlisted_document(&path) {
                DocumentState::Readable => continue,
                DocumentState::NewerSchema => {
                    self.summary.documents_newer_schema += 1;
                    continue;
                }
                DocumentState::Unreadable(reason) => {
                    if !self.budget_left() {
                        return;
                    }
                    self.set_aside_path(&path, &format!("unreadable document: {reason}"), true);
                    self.summary.documents_set_aside += 1;
                }
            }
        }
    }

    fn store_dirs(&self) -> Vec<StoreDir> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(&self.sessions_dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let owner = entry.file_name();
            let Some(owner_id) = owner.to_str() else {
                continue;
            };
            if !codewhale_runtime::artifacts::is_valid_session_id(owner_id)
                || !entry.path().is_dir()
            {
                continue;
            }
            let Ok(children) = fs::read_dir(entry.path()) else {
                continue;
            };
            for child in children.flatten() {
                if is_runtime_store_dir_name(&child.file_name()) && child.path().is_dir() {
                    out.push(StoreDir {
                        owner_id: owner_id.to_string(),
                        path: child.path(),
                    });
                }
            }
        }
        out.sort_by(|left, right| left.path.cmp(&right.path));
        out
    }

    /// Every thread in every store (read-only; no locks) names its bound
    /// document and the id its engine writes under. Plus the host stores
    /// outside the sessions directory (`codewhale serve`, task runtimes).
    fn collect_thread_references(&self, stores: &[StoreDir], references: &mut References) {
        let mut roots: Vec<PathBuf> = stores.iter().map(|store| store.path.clone()).collect();
        let tasks = crate::task_manager::default_tasks_dir().join("runtime");
        roots.push(tasks.clone());
        if let Ok(entries) = fs::read_dir(&tasks) {
            roots.extend(entries.flatten().map(|entry| entry.path()));
        }
        for root in roots {
            let Ok(entries) = fs::read_dir(root.join("threads")) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|ext| ext != "json") {
                    continue;
                }
                if let Some(thread_id) = path.file_stem().and_then(|stem| stem.to_str()) {
                    references
                        .thread_named
                        .insert(crate::runtime_threads::thread_session_id(thread_id));
                }
                if let Ok(raw) = fs::read(&path)
                    && let Ok(value) = serde_json::from_slice::<Value>(&raw)
                    && let Some(session_id) = value.get("session_id").and_then(Value::as_str)
                {
                    references.thread_named.insert(session_id.to_string());
                }
            }
        }
    }

    fn repair_store(&mut self, store: &StoreDir, references: &References) {
        let binding = match RuntimeStoreBinding::for_store_dir(&store.path) {
            Ok(binding) => binding,
            Err(error) => return self.error(&format!("store {}", store.path.display()), error),
        };
        let held = match binding.try_hold() {
            Ok(Some(held)) => held,
            Ok(None) => {
                if !references.binds(&store.path) {
                    self.summary.stores_in_use += 1;
                }
                return;
            }
            Err(error) => return self.error(&format!("store {}", store.path.display()), error),
        };
        if self
            .options
            .skip_session
            .as_deref()
            .is_some_and(|skip| skip == store.owner_id)
        {
            return;
        }
        let exists = |id: &str| references.documents.contains(id);
        if references.binds(&store.path) {
            // R4 for a bound store nobody holds.
            if self.options.dry_run {
                return;
            }
            match held.unbind_threads_without_documents(exists) {
                Ok(count) => self.summary.threads_unbound += count,
                Err(error) => self.error(&format!("threads in {}", store.path.display()), error),
            }
            return;
        }
        match held.keep_reason() {
            Ok(None) => {
                let tombstoned = self
                    .sessions_dir
                    .join(LATE_USAGE_DIR)
                    .join(format!("{}.deleted", store.owner_id))
                    .exists();
                let reason = if tombstoned {
                    "empty Runtime store of a deleted session"
                } else {
                    "empty Runtime store no document binds"
                };
                self.summary.stores_set_aside += 1;
                if self.options.dry_run {
                    return;
                }
                let target = match self.set_aside.target_for(
                    &self.sessions_dir,
                    &self.canonical_root,
                    &held.binding().data_dir.clone(),
                ) {
                    Ok(target) => target,
                    Err(error) => {
                        self.summary.stores_set_aside -= 1;
                        return self.error("set-aside directory", error);
                    }
                };
                let original = held.binding().data_dir.clone();
                if let Err(error) = held.move_to(&target) {
                    self.summary.stores_set_aside -= 1;
                    return self.error(&format!("store {}", original.display()), error);
                }
                self.set_aside.manifest(
                    &original,
                    &target,
                    reason,
                    json!({"kind": "runtime_store"}),
                );
                self.receipt(json!({"action": "store_set_aside", "path": original, "moved_to": target, "reason": reason}));
                remove_if_empty(store.path.parent());
            }
            Ok(Some(_)) => self.recover_store(held, &store.path, exists),
            Err(error) => self.error(&format!("store {}", store.path.display()), error),
        }
    }

    /// R3: give each conversation in an unbound store a document of its own.
    fn recover_store(
        &mut self,
        held: crate::runtime_threads::HeldRuntimeStore,
        path: &Path,
        exists: impl Fn(&str) -> bool,
    ) {
        // R4 applies here too. Deleting a document from the session picker
        // does not unbind the threads naming it, so an unbound store can hold
        // threads bound to a document that is gone. Left bound, they are
        // neither loadable through that document nor recoverable below, and
        // the store is kept forever. Unbinding them (with a receipt) makes
        // them recoverable in this same pass.
        if !self.options.dry_run {
            match held.unbind_threads_without_documents(&exists) {
                Ok(count) => self.summary.threads_unbound += count,
                Err(error) => return self.error(&format!("threads in {}", path.display()), error),
            }
        }
        let threads = match held.recoverable_threads() {
            Ok(threads) => threads,
            Err(error) => return self.error(&format!("threads in {}", path.display()), error),
        };
        if threads.is_empty() {
            self.summary.stores_kept_with_work += 1;
            return;
        }
        for recovered in threads {
            if !self.budget_left() {
                return;
            }
            self.summary.sessions_recovered += 1;
            if self.options.dry_run {
                continue;
            }
            let session = match self.recovered_document(&held, &recovered, &exists) {
                Ok(session) => session,
                Err(error) => {
                    self.summary.sessions_recovered -= 1;
                    self.error(&format!("recover thread {}", recovered.thread.id), error);
                    continue;
                }
            };
            if let Err(error) = held.bind_recovered_thread(&recovered.thread.id, &session) {
                self.error(&format!("bind thread {}", recovered.thread.id), error);
                continue;
            }
            self.receipt(json!({
                "action": "session_recovered",
                "store": path,
                "thread_id": recovered.thread.id,
                "session_id": session.metadata.id,
                "title": session.metadata.title,
            }));
        }
    }

    fn recovered_document(
        &self,
        held: &crate::runtime_threads::HeldRuntimeStore,
        recovered: &crate::runtime_threads::RecoverableThread,
        exists: &impl Fn(&str) -> bool,
    ) -> anyhow::Result<SavedSession> {
        let thread = &recovered.thread;
        let id = crate::runtime_threads::thread_session_id(&thread.id);
        // A run that stopped between saving the document and binding the
        // thread left the document behind at this same id; reuse it.
        if exists(&id) || self.sessions.session_document_exists(&id) {
            return Ok(self.sessions.load_session(&id)?);
        }
        let mut session = crate::session_manager::create_saved_session_with_id_and_mode(
            id,
            &recovered.messages,
            &thread.model,
            &thread.workspace,
            0,
            thread
                .system_prompt
                .clone()
                .map(codewhale_models::SystemPrompt::Text)
                .as_ref(),
            Some(thread.mode.as_str()),
        );
        if let Some(provider) = thread.model_provider.as_deref() {
            session
                .metadata
                .set_model_provider_route(provider, thread.model_provider_id.as_deref());
        }
        let label = thread
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| session.metadata.title.clone());
        session.metadata.title = recovered_title(&label);
        session.metadata.runtime_store = Some(held.binding().clone());
        self.sessions.save_session(&session)?;
        Ok(session)
    }

    /// R6: directories with no document and no store that hold only
    /// artifacts or approval receipts nothing names.
    fn repair_artifact_dirs(&mut self, references: &References) {
        let Ok(entries) = fs::read_dir(&self.sessions_dir) else {
            return;
        };
        let mut dirs: Vec<(String, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_string();
                (codewhale_runtime::artifacts::is_valid_session_id(&name) && entry.path().is_dir())
                    .then(|| (name, entry.path()))
            })
            .collect();
        dirs.sort();
        let idle_before = SystemTime::now()
            .checked_sub(self.options.artifact_idle)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        for (id, path) in dirs {
            if references.documents.contains(&id)
                || self.options.skip_session.as_deref() == Some(id.as_str())
                || !holds_only_artifacts(&path)
            {
                continue;
            }
            if references.names(&id.to_ascii_lowercase())
                || references.names(&id)
                || newest_mtime(&path).is_none_or(|mtime| mtime > idle_before)
            {
                self.summary.artifact_dirs_kept += 1;
                continue;
            }
            if !self.budget_left() {
                return;
            }
            self.set_aside_path(
                &path,
                "artifacts of a conversation no document or thread names",
                false,
            );
            self.summary.artifact_dirs_set_aside += 1;
        }
    }

    fn set_aside_path(&mut self, path: &Path, reason: &str, hash: bool) {
        if self.options.dry_run {
            return;
        }
        let detail = if hash {
            fs::read(path).map_or(
                Value::Null,
                |bytes| json!({"sha256": file_sha256(&bytes), "bytes": bytes.len()}),
            )
        } else {
            json!({"kind": "directory"})
        };
        let target = match self
            .set_aside
            .target_for(&self.sessions_dir, &self.canonical_root, path)
        {
            Ok(target) => target,
            Err(error) => return self.error("set-aside directory", error),
        };
        if let Some(parent) = target.parent()
            && let Err(error) = fs::create_dir_all(parent)
        {
            return self.error("set-aside directory", error);
        }
        if let Err(error) = fs::rename(path, &target) {
            return self.error(&format!("set aside {}", path.display()), error);
        }
        self.set_aside.manifest(path, &target, reason, detail);
        self.receipt(
            json!({"action": "set_aside", "path": path, "moved_to": target, "reason": reason}),
        );
    }
}

enum DocumentState {
    Readable,
    NewerSchema,
    Unreadable(String),
}

/// A document `list_sessions` could not list: newer, readable after all, or
/// genuinely unreadable.
fn classify_unlisted_document(path: &Path) -> DocumentState {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => return DocumentState::Unreadable(error.to_string()),
    };
    if let Ok(value) = serde_json::from_slice::<Value>(&bytes)
        && value
            .get("schema_version")
            .and_then(Value::as_u64)
            .is_some_and(|version| {
                version > u64::from(crate::session_manager::CURRENT_SESSION_SCHEMA_VERSION)
            })
    {
        return DocumentState::NewerSchema;
    }
    match serde_json::from_slice::<SavedSession>(&bytes) {
        Ok(_) => DocumentState::Readable,
        Err(error) => DocumentState::Unreadable(error.to_string()),
    }
}

fn holds_only_artifacts(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    let mut any = false;
    for entry in entries.flatten() {
        any = true;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return false;
        };
        if !matches!(
            name,
            "artifacts" | "approval_receipts.jsonl" | "approval_receipts.lock"
        ) {
            return false;
        }
    }
    any
}

fn newest_mtime(path: &Path) -> Option<SystemTime> {
    let metadata = fs::symlink_metadata(path).ok()?;
    let mut newest = metadata.modified().ok()?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path).ok()?.flatten() {
            if let Some(child) = newest_mtime(&entry.path()) {
                newest = newest.max(child);
            }
        }
    }
    Some(newest)
}

fn recovered_title(label: &str) -> String {
    let title =
        crate::session_manager::sanitize_session_title(&format!("Recovered: {}", label.trim()));
    title
        .chars()
        .take(crate::session_manager::MAX_SESSION_TITLE_CHARS)
        .collect()
}

/// SHA-256 of a file's bytes, for the manifest.
fn file_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
#[path = "session_reconcile/tests.rs"]
mod tests;
