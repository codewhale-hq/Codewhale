//! Historical thread recovery, canonical owner receipts and metadata.
//!
//! [`StateStore`] retains legacy conversation graphs as read-only archives.
//! A bounded absent-only restore can admit a complete historical graph in one
//! SQLite transaction; live transcript writes belong to Runtime's Engine and
//! SessionManager. Canonical aliases and durable operations use this same
//! SQLite connection. Thread metadata, dynamic tools and the session
//! index retain their existing ownership.

mod runtime_aliases;

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

/// Serializes all `session_index.jsonl` read/append/compact/rename operations so
/// concurrent `StateStore` clones cannot interleave an append with a compaction
/// rename and silently drop entries.
static SESSION_INDEX_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

use anyhow::{Context, Result};
use chrono::Utc;
use codewhale_paths::{CODEWHALE_APP_DIR, LEGACY_APP_DIR, codewhale_home_override};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// Re-export protocol's ThreadStatus so callers in the state crate and
// external consumers (e.g. core) can reference a single canonical definition.
pub use codewhale_protocol::ThreadStatus;

/// Indicates how a session was initiated.
///
/// Serialized as lowercase snake_case strings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    /// Started by a user interacting with the CLI.
    Interactive,
    /// Resumed from a previously persisted session.
    Resume,
    /// Created by forking an existing conversation at a specific message.
    Fork,
    /// Initiated programmatically via the API.
    Api,
    /// Source is unknown or unspecified.
    Unknown,
}

/// Metadata for a persisted conversation thread.
///
/// Each thread represents a single conversation session and stores its
/// configuration, git context, and current status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadMetadata {
    /// Unique identifier for this thread.
    pub id: String,
    /// Optional filesystem path to the rollout (JSONL transcript) file.
    pub rollout_path: Option<PathBuf>,
    /// Short preview or summary of the thread content.
    pub preview: String,
    /// Whether this thread is ephemeral (not persisted long-term).
    pub ephemeral: bool,
    /// Identifier of the model provider used for this thread (e.g. `"openai"`).
    pub model_provider: String,
    /// Unix timestamp (seconds) when the thread was created.
    pub created_at: i64,
    /// Unix timestamp (seconds) of the most recent update to the thread.
    pub updated_at: i64,
    /// Current lifecycle status of the thread.
    pub status: ThreadStatus,
    /// Optional filesystem path associated with the thread working context.
    pub path: Option<PathBuf>,
    /// Working directory that was active when the thread was created.
    pub cwd: PathBuf,
    /// Version of the CLI that created this thread.
    pub cli_version: String,
    /// How this session was initiated.
    pub source: SessionSource,
    /// User-assigned display name for the thread.
    pub name: Option<String>,
    /// Serialized sandbox policy applied to this thread, if any.
    pub sandbox_policy: Option<String>,
    /// Approval mode configured for tool calls in this thread.
    pub approval_mode: Option<String>,
    /// Whether the thread has been archived.
    pub archived: bool,
    /// Unix timestamp (seconds) when the thread was archived, or `None` if not archived.
    pub archived_at: Option<i64>,
    /// Git commit SHA of the working tree when the thread was created.
    pub git_sha: Option<String>,
    /// Git branch checked out when the thread was created.
    pub git_branch: Option<String>,
    /// URL of the git remote origin, if available.
    pub git_origin_url: Option<String>,
    /// Memory mode configured for this thread (e.g. `"local"`, `"remote"`).
    pub memory_mode: Option<String>,
    /// ID of the current leaf message in the conversation tree.
    pub current_leaf_id: Option<i64>,
}

/// A complete historical SQLite thread, restored once into an absent target.
///
/// This is recovery/import data, never a live transcript writer. Entry IDs,
/// timestamps, all branches and the exact selected leaf are retained.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyThreadArchive {
    pub thread: ThreadMetadata,
    pub messages: Vec<MessageRecord>,
    #[serde(default)]
    pub goal: Option<ThreadGoalRecord>,
    #[serde(default)]
    pub checkpoints: Vec<CheckpointRecord>,
}

/// A dynamically registered tool associated with a thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicToolRecord {
    /// Ordinal position of this tool in the thread tool list.
    pub position: i64,
    /// Unique name identifying the tool.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: Option<String>,
    /// JSON Schema describing the tool input parameters.
    pub input_schema: Value,
}

pub use codewhale_protocol::MessageRecord;

/// Private historical database fixture for State unit tests.
#[cfg(test)]
#[derive(Debug, Clone)]
struct NewMessage {
    /// Role of the message sender (e.g. `"user"`, `"history"`).
    pub role: String,
    /// Text content of the message.
    pub content: String,
    /// Optional structured item payload.
    pub item: Option<Value>,
}

/// A named checkpoint capturing the state of a thread at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRecord {
    /// ID of the thread this checkpoint belongs to.
    pub thread_id: String,
    /// Unique identifier for this checkpoint within its thread.
    pub checkpoint_id: String,
    /// Serialized state snapshot stored as a JSON value.
    pub state: Value,
    /// Unix timestamp (seconds) when the checkpoint was created or last updated.
    pub created_at: i64,
}

/// Persisted lifecycle status for a thread goal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadGoalStatus {
    /// Goal is active and should continue receiving work.
    Active,
    /// Goal is paused by the user.
    Paused,
    /// Goal is blocked and cannot make meaningful progress.
    Blocked,
    /// Goal stopped because account/service usage limits were reached.
    UsageLimited,
    /// Goal stopped because its explicit token budget was reached.
    BudgetLimited,
    /// Goal has been completed.
    Complete,
}

/// Persisted goal state attached to a thread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadGoalRecord {
    /// Thread this goal belongs to.
    pub thread_id: String,
    /// Stable identifier for this goal revision.
    pub goal_id: String,
    /// User-visible objective.
    pub objective: String,
    /// Current lifecycle status.
    pub status: ThreadGoalStatus,
    /// Optional token budget requested by the user.
    pub token_budget: Option<i64>,
    /// Tokens consumed while pursuing the goal.
    pub tokens_used: i64,
    /// Elapsed wall-clock work time in seconds.
    pub time_used_seconds: i64,
    /// Durable continuation passes dispatched for this objective.
    pub continuation_count: i64,
    /// Unix timestamp (seconds) when the goal was created.
    pub created_at: i64,
    /// Unix timestamp (seconds) when the goal was last updated.
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_gap_fingerprint: Option<String>,
    #[serde(default)]
    pub repeated_gap_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_gap_pass: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_reason: Option<codewhale_protocol::GoalPauseReason>,
}

/// Filters for listing conversation threads.
#[derive(Debug, Clone)]
pub struct ThreadListFilters {
    /// Whether to include archived threads in the results.
    pub include_archived: bool,
    /// Maximum number of threads to return. Defaults to 50.
    pub limit: Option<usize>,
}

impl Default for ThreadListFilters {
    fn default() -> Self {
        Self {
            include_archived: false,
            limit: Some(50),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionIndexEntry {
    thread_id: String,
    thread_name: Option<String>,
    updated_at: i64,
    rollout_path: Option<PathBuf>,
}

/// Rewrite the session index once the append-only log grows large enough that
/// full-file scans become costly. Lookups already dedupe by thread id, so
/// compaction keeps only the latest entry per thread.
fn session_index_compact_line_threshold() -> usize {
    if cfg!(test) { 5 } else { 5_000 }
}

/// The `user_version` the last step of `StateStore::migrate_schema` writes.
const SCHEMA_VERSION: u32 = 7;

/// Persistent storage for conversation threads, messages, and checkpoints.
///
/// Backed by a SQLite database and an append-only JSONL session index file.
/// The database schema is automatically initialized and migrated on [`open`](Self::open).
#[derive(Debug, Clone)]
pub struct StateStore {
    db_path: PathBuf,
    session_index_path: PathBuf,
    // Single long-lived connection shared by all clones. SQLite pragmas are
    // per-connection, so opening once in `open` and applying them there keeps
    // every operation consistent without re-opening the database per call.
    conn: Arc<Mutex<Connection>>,
}

impl StateStore {
    /// Open (or create) a state store at the given database path.
    ///
    /// If `path` is `None`, the default location (`~/.codewhale/state.db`, with
    /// `~/.deepseek/state.db` as a legacy fallback) is used.
    /// The database schema is created automatically if it does not exist.
    pub fn open(path: Option<PathBuf>) -> Result<Self> {
        let db_path = match path {
            Some(path) => path,
            None => {
                let default = default_state_db_path();
                anyhow::ensure!(
                    default.is_absolute(),
                    "could not resolve an absolute home directory for the state database; set CODEWHALE_HOME to an absolute path"
                );
                default
            }
        };
        let session_index_path = db_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("session_index.jsonl");
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create state directory {}", parent.display())
            })?;
        }
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open state db {}", db_path.display()))?;
        Self::configure_connection(&conn, &db_path)?;
        Self::init_schema(&conn)?;
        Ok(Self {
            db_path,
            session_index_path,
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Apply connection-level SQLite settings that must hold for every open.
    ///
    /// Enables WAL so readers and writers from concurrent CodeWhale processes
    /// do not block each other as aggressively as the default rollback journal,
    /// and sets a multi-second busy timeout so a second process retries on
    /// `SQLITE_BUSY` instead of failing immediately (issue #4734).
    fn configure_connection(conn: &Connection, db_path: &Path) -> Result<()> {
        // Install our wait policy before touching database-level settings or
        // schema. Connection::open may currently provide a dependency default,
        // but StateStore must not rely on that incidental behavior.
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .with_context(|| format!("failed to set busy_timeout for {}", db_path.display()))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .with_context(|| format!("failed to enable foreign keys for {}", db_path.display()))?;

        // WAL persists in the database header, so established stores need no
        // write-like journal transition on every process start. Fresh or
        // explicitly downgraded stores still transition once, and we verify
        // SQLite accepted the requested mode instead of silently retaining the
        // previous one (for example on an unsupported VFS).
        let journal_mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .with_context(|| format!("failed to read journal mode for {}", db_path.display()))?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            let configured_mode: String = conn
                .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
                .with_context(|| format!("failed to enable WAL for {}", db_path.display()))?;
            if !configured_mode.eq_ignore_ascii_case("wal") {
                anyhow::bail!(
                    "failed to enable WAL for {}: SQLite retained journal mode {configured_mode}",
                    db_path.display()
                );
            }
        }
        Ok(())
    }

    /// Returns the filesystem path of the underlying SQLite database.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        // Poisoning means a panic mid-operation; any open transaction was
        // rolled back when it dropped, but surface the condition rather than
        // silently continuing on a connection whose state we can't vouch for.
        self.conn
            .lock()
            .map_err(|_| anyhow::anyhow!("state db connection mutex poisoned"))
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        let user_version: u32 = conn.query_row("PRAGMA user_version;", [], |row| row.get(0))?;
        if user_version >= SCHEMA_VERSION {
            return Ok(());
        }
        // Take the write lock *before* reading the version or probing
        // columns. Two processes opening a fresh database used to both read
        // v0 and "column missing" outside any transaction; the loser then
        // waited out the winner's commit and re-ran its `ADD COLUMN`, failing
        // the open with "duplicate column name". Under `BEGIN IMMEDIATE` the
        // loser waits (busy_timeout) and then decides from the committed
        // schema. The version read above is only a fast path for the
        // already-current database, which never takes the write lock.
        conn.execute_batch("BEGIN IMMEDIATE;")
            .context("failed to lock the state db for its schema migration")?;
        let migrated = Self::migrate_schema(conn).and_then(|()| {
            conn.execute_batch("COMMIT;")
                .context("failed to commit the state db schema migration")
        });
        if migrated.is_err() {
            let _ = conn.execute_batch("ROLLBACK;");
        }
        migrated
    }

    /// Every schema step, run inside the one transaction [`Self::init_schema`]
    /// holds; the steps themselves never begin or commit.
    fn migrate_schema(conn: &Connection) -> Result<()> {
        let mut user_version: u32 = conn.query_row("PRAGMA user_version;", [], |row| row.get(0))?;
        if user_version == 0 {
            // Guard each ALTER: a database restored with a v0 header (or
            // stamped by a racing process that crashed before setting
            // user_version) can already carry these columns, and an
            // unguarded ADD COLUMN aborts the whole open with
            // "duplicate column name".
            let add_parent_entry_id = if column_exists(conn, "messages", "parent_entry_id")? {
                ""
            } else {
                "ALTER TABLE messages ADD COLUMN parent_entry_id INTEGER NULL;"
            };
            let add_current_leaf_id = if column_exists(conn, "threads", "current_leaf_id")? {
                ""
            } else {
                "ALTER TABLE threads ADD COLUMN current_leaf_id INTEGER NULL;"
            };
            conn.execute_batch(&format!(
                r#"
                CREATE TABLE IF NOT EXISTS threads (
                    id TEXT PRIMARY KEY,
                    rollout_path TEXT,
                    preview TEXT NOT NULL,
                    ephemeral INTEGER NOT NULL,
                    model_provider TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL,
                    status TEXT NOT NULL,
                    path TEXT,
                    cwd TEXT NOT NULL,
                    cli_version TEXT NOT NULL,
                    source TEXT NOT NULL,
                    title TEXT,
                    sandbox_policy TEXT,
                    approval_mode TEXT,
                    archived INTEGER NOT NULL DEFAULT 0,
                    archived_at INTEGER,
                    git_sha TEXT,
                    git_branch TEXT,
                    git_origin_url TEXT,
                    memory_mode TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_threads_updated_at ON threads(updated_at DESC);
                CREATE INDEX IF NOT EXISTS idx_threads_archived_at ON threads(archived_at DESC);
                CREATE INDEX IF NOT EXISTS idx_threads_archived_updated ON threads(archived, updated_at DESC);

                CREATE TABLE IF NOT EXISTS thread_dynamic_tools (
                    thread_id TEXT NOT NULL,
                    position INTEGER NOT NULL,
                    name TEXT NOT NULL,
                    description TEXT,
                    input_schema TEXT NOT NULL,
                    PRIMARY KEY (thread_id, position),
                    FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
                );

                CREATE TABLE IF NOT EXISTS messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    thread_id TEXT NOT NULL,
                    role TEXT NOT NULL,
                    content TEXT NOT NULL,
                    item_json TEXT,
                    created_at INTEGER NOT NULL,
                    FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_messages_thread_created_at ON messages(thread_id, created_at ASC);

                CREATE TABLE IF NOT EXISTS checkpoints (
                    thread_id TEXT NOT NULL,
                    checkpoint_id TEXT NOT NULL,
                    state_json TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY(thread_id, checkpoint_id),
                    FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_checkpoints_thread_created_at ON checkpoints(thread_id, created_at DESC);

                CREATE TABLE IF NOT EXISTS jobs (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    status TEXT NOT NULL,
                    progress INTEGER,
                    detail TEXT,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_jobs_updated_at ON jobs(updated_at DESC);

                -- Add parent_entry_id column, and set to last message before current message
                {add_parent_entry_id}
                UPDATE messages
                    SET parent_entry_id = (
                        SELECT m2.id
                        FROM messages m2
                        WHERE m2.thread_id = messages.thread_id
                            AND (
                                m2.created_at < messages.created_at
                                OR (
                                    m2.created_at = messages.created_at
                                    AND m2.id < messages.id
                                )
                            )
                        ORDER BY m2.created_at DESC, m2.id DESC
                        LIMIT 1
                    );
                CREATE INDEX IF NOT EXISTS idx_messages_parent_entry_id ON messages(parent_entry_id);

                -- Add current_leaf_id column, and set to last message in thread
                {add_current_leaf_id}
                UPDATE threads
                    SET current_leaf_id = (
                        SELECT m.id
                        FROM messages m
                        WHERE m.thread_id = threads.id
                        ORDER BY m.id DESC
                        LIMIT 1
                    );

                PRAGMA user_version = 1;
                "#
            ))
            .context("failed to initialize thread schema")?;
            user_version = 1;
        }
        if user_version < 2 {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS workflow_runs (
                    id TEXT PRIMARY KEY,
                    workflow_id TEXT NOT NULL,
                    goal TEXT NOT NULL,
                    status TEXT NOT NULL,
                    input_hash TEXT,
                    started_at INTEGER NOT NULL,
                    completed_at INTEGER,
                    metadata_json TEXT NOT NULL DEFAULT '{}'
                );
                CREATE INDEX IF NOT EXISTS idx_workflow_runs_status_started_at
                    ON workflow_runs(status, started_at DESC);
                CREATE INDEX IF NOT EXISTS idx_workflow_runs_workflow_started_at
                    ON workflow_runs(workflow_id, started_at DESC);

                CREATE TABLE IF NOT EXISTS branch_runs (
                    id TEXT PRIMARY KEY,
                    workflow_run_id TEXT NOT NULL,
                    branch_id TEXT NOT NULL,
                    node_id TEXT NOT NULL,
                    status TEXT NOT NULL,
                    started_at INTEGER NOT NULL,
                    completed_at INTEGER,
                    result_json TEXT NOT NULL DEFAULT '{}',
                    FOREIGN KEY(workflow_run_id) REFERENCES workflow_runs(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_branch_runs_workflow_run_id
                    ON branch_runs(workflow_run_id);
                CREATE INDEX IF NOT EXISTS idx_branch_runs_branch_id
                    ON branch_runs(branch_id);

                CREATE TABLE IF NOT EXISTS leaf_runs (
                    id TEXT PRIMARY KEY,
                    workflow_run_id TEXT NOT NULL,
                    branch_run_id TEXT,
                    leaf_id TEXT NOT NULL,
                    task_id TEXT NOT NULL,
                    input_hash TEXT,
                    status TEXT NOT NULL,
                    output_json TEXT NOT NULL DEFAULT '{}',
                    artifacts_json TEXT NOT NULL DEFAULT '[]',
                    started_at INTEGER NOT NULL,
                    completed_at INTEGER,
                    FOREIGN KEY(workflow_run_id) REFERENCES workflow_runs(id) ON DELETE CASCADE,
                    FOREIGN KEY(branch_run_id) REFERENCES branch_runs(id) ON DELETE SET NULL
                );
                CREATE INDEX IF NOT EXISTS idx_leaf_runs_workflow_run_id
                    ON leaf_runs(workflow_run_id);
                CREATE INDEX IF NOT EXISTS idx_leaf_runs_replay_lookup
                    ON leaf_runs(workflow_run_id, leaf_id, input_hash);

                CREATE TABLE IF NOT EXISTS control_node_runs (
                    id TEXT PRIMARY KEY,
                    workflow_run_id TEXT NOT NULL,
                    node_id TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    status TEXT NOT NULL,
                    selected_children_json TEXT NOT NULL DEFAULT '[]',
                    result_json TEXT NOT NULL DEFAULT '{}',
                    started_at INTEGER NOT NULL,
                    completed_at INTEGER,
                    FOREIGN KEY(workflow_run_id) REFERENCES workflow_runs(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_control_node_runs_workflow_run_id
                    ON control_node_runs(workflow_run_id);
                CREATE INDEX IF NOT EXISTS idx_control_node_runs_node_id
                    ON control_node_runs(node_id);

                CREATE TABLE IF NOT EXISTS teacher_candidates (
                    id TEXT PRIMARY KEY,
                    workflow_run_id TEXT NOT NULL,
                    control_node_run_id TEXT NOT NULL,
                    candidate_id TEXT NOT NULL,
                    branch_run_id TEXT,
                    score REAL,
                    passed INTEGER,
                    rationale_json TEXT NOT NULL DEFAULT '{}',
                    created_at INTEGER NOT NULL,
                    FOREIGN KEY(workflow_run_id) REFERENCES workflow_runs(id) ON DELETE CASCADE,
                    FOREIGN KEY(control_node_run_id) REFERENCES control_node_runs(id) ON DELETE CASCADE,
                    FOREIGN KEY(branch_run_id) REFERENCES branch_runs(id) ON DELETE SET NULL
                );
                CREATE INDEX IF NOT EXISTS idx_teacher_candidates_workflow_run_id
                    ON teacher_candidates(workflow_run_id);
                CREATE INDEX IF NOT EXISTS idx_teacher_candidates_control_node_run_id
                    ON teacher_candidates(control_node_run_id);

                PRAGMA user_version = 2;
                "#,
            )
            .context("failed to initialize workflow trace schema")?;
            user_version = 2;
        }
        if user_version < 3 {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS thread_goals (
                    thread_id TEXT PRIMARY KEY NOT NULL,
                    goal_id TEXT NOT NULL,
                    objective TEXT NOT NULL,
                    status TEXT NOT NULL CHECK(status IN (
                        'active',
                        'paused',
                        'blocked',
                        'usage_limited',
                        'budget_limited',
                        'complete'
                    )),
                    token_budget INTEGER,
                    tokens_used INTEGER NOT NULL DEFAULT 0,
                    time_used_seconds INTEGER NOT NULL DEFAULT 0,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL,
                    FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
                );

                PRAGMA user_version = 3;
                "#,
            )
            .context("failed to initialize thread goal schema")?;
            user_version = 3;
        }
        if user_version < 4 {
            // Same restore/race guard as the v0 block: the column may
            // already exist even though the header predates version 4.
            let add_continuation_count = if column_exists(
                conn,
                "thread_goals",
                "continuation_count",
            )? {
                ""
            } else {
                "ALTER TABLE thread_goals\n                    ADD COLUMN continuation_count INTEGER NOT NULL DEFAULT 0;"
            };
            conn.execute_batch(&format!(
                r#"
                {add_continuation_count}

                PRAGMA user_version = 4;
                "#
            ))
            .context("failed to initialize thread goal continuation schema")?;
            user_version = 4;
        }
        if user_version < 5 {
            let mut additions = String::new();
            for (column, definition) in [
                ("last_gap_fingerprint", "TEXT"),
                ("repeated_gap_count", "INTEGER NOT NULL DEFAULT 0"),
                ("last_gap_pass", "INTEGER"),
                ("pause_reason", "TEXT"),
            ] {
                if !column_exists(conn, "thread_goals", column)? {
                    additions.push_str(&format!(
                        "ALTER TABLE thread_goals ADD COLUMN {column} {definition};\n"
                    ));
                }
            }
            conn.execute_batch(&format!("{additions} PRAGMA user_version = 5;"))
                .context("failed to initialize durable goal stall schema")?;
            user_version = 5;
        }
        if user_version < 6 {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS thread_runtime_links (
                    thread_id TEXT PRIMARY KEY NOT NULL,
                    runtime_thread_id TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
                );

                PRAGMA user_version = 6;
                "#,
            )
            .context("failed to initialize thread runtime link schema")?;
            user_version = 6;
        }
        if user_version < 7 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS state_store_identity (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), identity TEXT NOT NULL);
                 INSERT OR IGNORE INTO state_store_identity VALUES(1, lower(hex(randomblob(32))));
                 CREATE TABLE IF NOT EXISTS thread_runtime_receipts (thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE, receipt_json TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS thread_runtime_operations (operation_key TEXT PRIMARY KEY, request_digest TEXT NOT NULL, receipt_json TEXT NOT NULL);
                 PRAGMA user_version = 7;",
            ).context("failed to initialize bound canonical Runtime receipts")?;
        }
        Ok(())
    }

    /// The runtime (turn engine) thread a client-facing thread runs on, if
    /// one was linked. Links outlive the app-server process, so a thread
    /// keeps its conversation across a daemon restart.
    pub fn get_runtime_thread_link(&self, thread_id: &str) -> Result<Option<String>> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT runtime_thread_id FROM thread_runtime_links WHERE thread_id = ?1",
            params![thread_id],
            |row| row.get(0),
        )
        .optional()
        .with_context(|| format!("failed to read runtime link for thread {thread_id}"))
    }

    /// Record the runtime thread for `thread_id`. The thread must exist; the
    /// link is removed with it.
    #[cfg(test)]
    fn set_runtime_thread_link(&self, thread_id: &str, runtime_thread_id: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let bound: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM thread_runtime_receipts WHERE thread_id = ?1)",
            params![thread_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            !bound,
            "bound canonical aliases cannot be changed by the legacy link writer"
        );
        tx.execute(
            r#"
            INSERT INTO thread_runtime_links(thread_id, runtime_thread_id, created_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(thread_id) DO UPDATE SET
                runtime_thread_id = excluded.runtime_thread_id,
                created_at = excluded.created_at
            "#,
            params![thread_id, runtime_thread_id, Utc::now().timestamp()],
        )
        .with_context(|| format!("failed to save runtime link for thread {thread_id}"))?;
        tx.commit()?;
        Ok(())
    }

    /// Insert or update thread metadata.
    ///
    /// This does **not** update legacy history or its selected leaf. Restoring an
    /// old transcript requires an absent target and a complete immutable archive.
    pub fn upsert_thread(&self, thread: &ThreadMetadata) -> Result<()> {
        let conn = self.conn()?;
        write_thread_metadata_on(&conn, thread)?;

        self.append_thread_name(
            &thread.id,
            thread.name.clone(),
            thread.updated_at,
            thread.rollout_path.clone(),
        )?;
        Ok(())
    }

    /// Retrieve a single thread by its ID.
    ///
    /// Returns `None` if no thread with the given ID exists.
    pub fn get_thread(&self, id: &str) -> Result<Option<ThreadMetadata>> {
        let conn = self.conn()?;
        conn.query_row(
            r#"
            SELECT id, rollout_path, preview, ephemeral, model_provider, created_at, updated_at, status, path, cwd,
                   cli_version, source, title, sandbox_policy, approval_mode, archived, archived_at,
                   git_sha, git_branch, git_origin_url, memory_mode, current_leaf_id
            FROM threads
            WHERE id = ?1
            "#,
            params![id],
            row_to_thread,
        )
        .optional()
        .context("failed to read thread")
    }

    /// List threads ordered by most recently updated.
    ///
    /// Use [`ThreadListFilters`] to control whether archived threads are included
    /// and the maximum number of results returned.
    pub fn list_threads(&self, filters: ThreadListFilters) -> Result<Vec<ThreadMetadata>> {
        let conn = self.conn()?;
        let sql = if filters.include_archived {
            "SELECT id, rollout_path, preview, ephemeral, model_provider, created_at, updated_at, status, path, cwd, cli_version, source, title, sandbox_policy, approval_mode, archived, archived_at, git_sha, git_branch, git_origin_url, memory_mode, current_leaf_id FROM threads ORDER BY updated_at DESC LIMIT ?1"
        } else {
            "SELECT id, rollout_path, preview, ephemeral, model_provider, created_at, updated_at, status, path, cwd, cli_version, source, title, sandbox_policy, approval_mode, archived, archived_at, git_sha, git_branch, git_origin_url, memory_mode, current_leaf_id FROM threads WHERE archived = 0 ORDER BY updated_at DESC LIMIT ?1"
        };

        let mut stmt = conn.prepare(sql).context("failed to prepare list query")?;
        let limit = i64::try_from(filters.limit.unwrap_or(50)).unwrap_or(50);
        let mut rows = stmt
            .query(params![limit])
            .context("failed to query threads")?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().context("failed to iterate thread rows")? {
            out.push(row_to_thread(row)?);
        }
        Ok(out)
    }

    /// Archive a thread, setting its status to [`ThreadStatus::Archived`] and
    /// recording the current timestamp.
    pub fn mark_archived(&self, id: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE threads SET archived = 1, archived_at = ?2, status = ?3 WHERE id = ?1",
            params![
                id,
                Utc::now().timestamp(),
                thread_status_to_str(&ThreadStatus::Archived)
            ],
        )
        .context("failed to archive thread")?;
        Ok(())
    }

    /// Unarchive a thread, removing the archived flag and clearing `archived_at`.
    pub fn mark_unarchived(&self, id: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE threads SET archived = 0, archived_at = NULL, status = CASE WHEN status = ?2 THEN ?3 ELSE status END WHERE id = ?1",
            params![
                id,
                thread_status_to_str(&ThreadStatus::Archived),
                thread_status_to_str(&ThreadStatus::Idle),
            ],
        )
        .context("failed to unarchive thread")?;
        Ok(())
    }

    /// Permanently delete a thread and all of its associated data
    /// (messages, checkpoints, dynamic tools) via cascading foreign keys, and
    /// drop it from the session index.
    ///
    /// The index is append-only and name lookups read it without the
    /// database, so a deleted thread left there kept answering for its name —
    /// shadowing a live thread of the same name when it was newer.
    pub fn delete_thread(&self, id: &str) -> Result<()> {
        {
            let conn = self.conn()?;
            conn.execute("DELETE FROM threads WHERE id = ?1", params![id])
                .context("failed to delete thread")?;
        }
        let _guard = SESSION_INDEX_LOCK.lock().unwrap();
        self.with_session_index_lock(|| {
            let mut latest = self.session_index_map()?;
            if latest.remove(id).is_some() {
                self.rewrite_session_index_locked(&latest)?;
            }
            Ok(())
        })
    }

    /// Insert or replace the persisted goal for a thread.
    #[cfg(test)]
    fn upsert_thread_goal(&self, goal: &ThreadGoalRecord) -> Result<()> {
        let conn = self.conn()?;
        write_thread_goal_on(&conn, goal)
    }

    /// Accrue additional token and wall-clock usage onto a thread's persisted goal.
    ///
    /// This is the durable, additive accounting path for the persistent goal loop: it
    /// increments `tokens_used` and `time_used_seconds` in a single atomic SQL `UPDATE`
    /// (`col = col + ?`) so concurrent accruals do not race a read-modify-write. The
    /// goal's `updated_at` is advanced to the larger of its current value and `now`,
    /// keeping the timestamp monotonic even if a stale `now` is supplied.
    ///
    /// `token_delta` and `time_delta_seconds` are added on the database side; callers
    /// should pass non-negative deltas (negative values are accepted and will decrement,
    /// which is intentionally left to the caller's discretion).
    ///
    /// Returns the updated [`ThreadGoalRecord`], or `Ok(None)` if the thread has no
    /// persisted goal. Unlike the historical fixture upsert this never
    /// creates a goal row; it only accumulates onto an existing one.
    #[cfg(test)]
    fn record_thread_goal_usage(
        &self,
        thread_id: &str,
        token_delta: i64,
        time_delta_seconds: i64,
        now: i64,
    ) -> Result<Option<ThreadGoalRecord>> {
        let conn = self.conn()?;
        let changed = conn
            .execute(
                r#"
                UPDATE thread_goals
                SET tokens_used = tokens_used + ?2,
                    time_used_seconds = time_used_seconds + ?3,
                    updated_at = MAX(updated_at, ?4)
                WHERE thread_id = ?1
                "#,
                params![thread_id, token_delta, time_delta_seconds, now],
            )
            .context("failed to record thread goal usage")?;
        if changed == 0 {
            return Ok(None);
        }
        Self::read_thread_goal(&conn, thread_id)
    }

    /// Increment the durable cross-turn continuation counter for a thread goal.
    ///
    /// The older TUI continuation guard is scoped to one engine turn. This
    /// counter is intentionally persisted so a resumed goal loop can feed
    /// `goal_loop::decide_continuation` with the true cross-turn count.
    #[cfg(test)]
    fn record_thread_goal_continuation(
        &self,
        thread_id: &str,
        now: i64,
    ) -> Result<Option<ThreadGoalRecord>> {
        let conn = self.conn()?;
        let changed = conn
            .execute(
                r#"
                UPDATE thread_goals
                SET continuation_count = continuation_count + 1,
                    updated_at = MAX(updated_at, ?2)
                WHERE thread_id = ?1
                "#,
                params![thread_id, now],
            )
            .context("failed to record thread goal continuation")?;
        if changed == 0 {
            return Ok(None);
        }
        Self::read_thread_goal(&conn, thread_id)
    }

    /// Retrieve the persisted goal for a thread.
    pub fn get_thread_goal(&self, thread_id: &str) -> Result<Option<ThreadGoalRecord>> {
        let conn = self.conn()?;
        Self::read_thread_goal(&conn, thread_id)
    }

    /// Read a goal on an already-held connection. The `record_*` mutators call
    /// this instead of the public goal reader, which would re-lock the
    /// connection mutex and self-deadlock.
    fn read_thread_goal(conn: &Connection, thread_id: &str) -> Result<Option<ThreadGoalRecord>> {
        let mut goal = Self::read_thread_goal_snapshot(conn, thread_id)?;
        if let Some(goal) = goal.as_mut() {
            goal.normalize_restored_stall_state();
        }
        Ok(goal)
    }

    /// Immutable migration/CAS snapshot keeps the original persisted status.
    /// Restore normalization belongs to the reader/target, never source CAS.
    fn read_thread_goal_snapshot(
        conn: &Connection,
        thread_id: &str,
    ) -> Result<Option<ThreadGoalRecord>> {
        conn.query_row(
            r#"
            SELECT thread_id, goal_id, objective, status, token_budget, tokens_used,
                   time_used_seconds, continuation_count, created_at, updated_at,
                   last_gap_fingerprint, repeated_gap_count, last_gap_pass, pause_reason
            FROM thread_goals
            WHERE thread_id = ?1
            "#,
            params![thread_id],
            row_to_thread_goal,
        )
        .optional()
        .context("failed to read thread goal")
    }

    /// Delete the persisted goal for a thread.
    #[cfg(test)]
    fn delete_thread_goal(&self, thread_id: &str) -> Result<bool> {
        let conn = self.conn()?;
        let changed = conn
            .execute(
                "DELETE FROM thread_goals WHERE thread_id = ?1",
                params![thread_id],
            )
            .context("failed to delete thread goal")?;
        Ok(changed > 0)
    }

    /// List all leaf messages in a thread.
    ///
    /// A leaf message is one that has no other message referencing it as a parent.
    /// In a branching conversation tree, there may be multiple leaf messages.
    pub fn list_leaf_messages(&self, thread_id: &str) -> Result<Vec<MessageRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT m1.id, m1.thread_id, m1.role, m1.content, m1.item_json, m1.created_at, m1.parent_entry_id
                FROM messages m1
                LEFT JOIN messages m2 ON m1.id = m2.parent_entry_id
                WHERE m1.thread_id = ?1 AND m2.id IS NULL
                "#,
            )
            .context("failed to prepare message listing query")?;
        let mut rows = stmt
            .query(params![thread_id])
            .with_context(|| format!("failed to list leaf messages for thread {thread_id}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().context("failed to iterate message rows")? {
            let item_json: Option<String> = row.get(4).context("failed to read item json")?;
            let item = item_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .with_context(|| {
                    format!("failed to parse message item json in thread {thread_id}")
                })?;
            out.push(MessageRecord {
                id: row.get(0).context("failed to read message id")?,
                thread_id: row.get(1).context("failed to read message thread id")?,
                role: row.get(2).context("failed to read message role")?,
                content: row.get(3).context("failed to read message content")?,
                item,
                created_at: row.get(5).context("failed to read message timestamp")?,
                parent_entry_id: row.get(6).context("failed to read parent entry id")?,
            });
        }
        Ok(out)
    }

    /// Replace the dynamic tools for a thread.
    ///
    /// All existing dynamic tools for the thread are deleted and replaced with the
    /// provided list. The operation is performed within a transaction.
    pub fn persist_dynamic_tools(
        &self,
        thread_id: &str,
        tools: &[DynamicToolRecord],
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn
            .transaction()
            .context("failed to begin dynamic tools transaction")?;
        tx.execute(
            "DELETE FROM thread_dynamic_tools WHERE thread_id = ?1",
            params![thread_id],
        )
        .context("failed to clear dynamic tools")?;
        for tool in tools {
            tx.execute(
                "INSERT INTO thread_dynamic_tools(thread_id, position, name, description, input_schema) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    thread_id,
                    tool.position,
                    tool.name,
                    tool.description,
                    tool.input_schema.to_string()
                ],
            )
            .with_context(|| format!("failed to persist dynamic tool {}", tool.name))?;
        }
        tx.commit().context("failed to commit dynamic tools")?;
        Ok(())
    }

    /// Retrieve all dynamic tools registered for a thread, ordered by position.
    pub fn get_dynamic_tools(&self, thread_id: &str) -> Result<Vec<DynamicToolRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT position, name, description, input_schema FROM thread_dynamic_tools WHERE thread_id = ?1 ORDER BY position ASC",
            )
            .context("failed to prepare get dynamic tools query")?;
        let mut rows = stmt
            .query(params![thread_id])
            .context("failed to query dynamic tools")?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().context("failed to iterate dynamic tools")? {
            let input_schema_raw: String =
                row.get(3).context("failed to read tool input schema")?;
            let input_schema: Value =
                serde_json::from_str(&input_schema_raw).with_context(|| {
                    format!("failed to parse input schema for dynamic tool in thread {thread_id}")
                })?;
            out.push(DynamicToolRecord {
                position: row.get(0).context("failed to read tool position")?,
                name: row.get(1).context("failed to read tool name")?,
                description: row.get(2).context("failed to read tool description")?,
                input_schema,
            });
        }
        Ok(out)
    }

    /// Append a new message to a thread.
    ///
    /// The message is linked to the thread's current leaf as its parent, and the
    /// thread's `current_leaf_id` is updated to the new message. Returns the ID
    /// of the newly created message.
    #[cfg(test)]
    fn append_message(
        &self,
        thread_id: &str,
        role: &str,
        content: &str,
        item: Option<Value>,
    ) -> Result<i64> {
        let ids = self.append_messages(
            thread_id,
            &[NewMessage {
                role: role.to_string(),
                content: content.to_string(),
                item,
            }],
        )?;
        ids.first()
            .copied()
            .context("append message transaction returned no id")
    }

    /// Append `messages` to a thread's current branch in one transaction.
    ///
    /// Each message is linked to the one before it (the first to the thread's
    /// current leaf), and the leaf ends on the last. All or nothing: a failure
    /// part-way leaves the thread exactly as it was, never a partial history.
    /// Returns the new message ids in order.
    #[cfg(test)]
    fn append_messages(&self, thread_id: &str, messages: &[NewMessage]) -> Result<Vec<i64>> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }
        let encoded = messages
            .iter()
            .map(|message| {
                message
                    .item
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .context("failed to serialize message item payload")
            })
            .collect::<Result<Vec<_>>>()?;
        let mut conn = self.conn()?;
        let created_at = Utc::now().timestamp();

        let tx = conn
            .transaction()
            .context("failed to begin append message transaction")?;

        let mut leaf_id: Option<i64> = tx
            .query_row(
                "SELECT current_leaf_id FROM threads WHERE id = ?1",
                params![thread_id],
                |row| row.get(0),
            )
            .with_context(|| {
                format!("failed to query thread current leaf id for thread {thread_id}")
            })?;

        let mut ids = Vec::with_capacity(messages.len());
        for (message, item_json) in messages.iter().zip(encoded) {
            let next_leaf_id: i64 = tx.query_row(
                r#"
                    INSERT INTO messages(thread_id, role, content, item_json, created_at, parent_entry_id)
                    SELECT ?1, ?2, ?3, ?4, ?5, ?6
                    RETURNING id
                "#, params![thread_id, message.role, message.content, item_json, created_at, leaf_id], |row| row.get(0)
            ).with_context(|| format!("failed to append message for thread {thread_id}"))?;
            leaf_id = Some(next_leaf_id);
            ids.push(next_leaf_id);
        }

        tx.execute(
            r#"
            UPDATE threads
            SET current_leaf_id = ?1
            WHERE id = ?2;
            "#,
            params![leaf_id, thread_id],
        )
        .with_context(|| {
            format!("failed to update thread current leaf id for thread {thread_id}")
        })?;

        tx.commit()
            .context("failed to commit append message transaction")?;

        Ok(ids)
    }

    /// List messages in the current conversation branch, walking backwards from
    /// the thread's `current_leaf_id`.
    ///
    /// Messages are returned in chronological order (oldest first). The `limit`
    /// parameter caps how many ancestor messages are traversed; it defaults to 500.
    pub fn list_messages(
        &self,
        thread_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<MessageRecord>> {
        let conn = self.conn()?;
        let limit = i64::try_from(limit.unwrap_or(500)).unwrap_or(500);
        let mut stmt = conn
            .prepare(
                r#"
                WITH RECURSIVE
                    leaf_id AS (
                        SELECT current_leaf_id FROM threads WHERE id = ?1
                    ),
                    ancestors AS (
                        SELECT id, thread_id, role, content, item_json, created_at, parent_entry_id, 0 AS depth
                        FROM messages
                        WHERE id = (SELECT current_leaf_id FROM leaf_id)

                        UNION ALL

                        SELECT m.id, m.thread_id, m.role, m.content, m.item_json, m.created_at, m.parent_entry_id, a.depth + 1
                        FROM messages m
                        JOIN ancestors a ON m.id = a.parent_entry_id
                        WHERE a.depth < ?2
                    )
                    SELECT id, thread_id, role, content, item_json, created_at, parent_entry_id FROM ancestors
                    ORDER BY depth DESC
                "#
            )
            .context("failed to prepare message listing query")?;
        let mut rows = stmt
            .query(params![thread_id, limit - 1])
            .with_context(|| format!("failed to list messages for thread {thread_id}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().context("failed to iterate message rows")? {
            let item_json: Option<String> = row.get(4).context("failed to read item json")?;
            let item = item_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .with_context(|| {
                    format!("failed to parse message item json in thread {thread_id}")
                })?;
            out.push(MessageRecord {
                id: row.get(0).context("failed to read message id")?,
                thread_id: row.get(1).context("failed to read message thread id")?,
                role: row.get(2).context("failed to read message role")?,
                content: row.get(3).context("failed to read message content")?,
                item,
                created_at: row.get(5).context("failed to read message timestamp")?,
                parent_entry_id: row.get(6).context("failed to read parent entry id")?,
            });
        }
        Ok(out)
    }

    /// Delete all messages belonging to a thread and reset its `current_leaf_id`.
    ///
    /// Returns the number of messages deleted.
    #[cfg(test)]
    fn clear_messages(&self, thread_id: &str) -> Result<usize> {
        let mut conn = self.conn()?;
        let tx = conn
            .transaction()
            .context("failed to begin clear messages transaction")?;

        tx.execute(
            r#"
            UPDATE threads
            SET current_leaf_id = NULL
            WHERE id = ?1;
            "#,
            params![thread_id],
        )
        .with_context(|| format!("failed to clear messages for thread {thread_id}"))?;
        let result = tx
            .execute(
                r#"
                DELETE FROM messages WHERE thread_id = ?1
                "#,
                params![thread_id],
            )
            .with_context(|| format!("failed to clear messages for thread {thread_id}"))?;
        tx.commit()
            .context("failed to commit clear messages transaction")?;

        Ok(result)
    }

    /// Save (or update) a named checkpoint for a thread.
    ///
    /// If a checkpoint with the same `thread_id` and `checkpoint_id` already exists,
    /// its state and timestamp are overwritten.
    #[cfg(test)]
    fn save_checkpoint(&self, thread_id: &str, checkpoint_id: &str, state: &Value) -> Result<()> {
        let conn = self.conn()?;
        let state_json =
            serde_json::to_string(state).context("failed to encode checkpoint state")?;
        conn.execute(
            r#"
            INSERT INTO checkpoints(thread_id, checkpoint_id, state_json, created_at)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(thread_id, checkpoint_id) DO UPDATE SET
                state_json = excluded.state_json,
                created_at = excluded.created_at
            "#,
            params![thread_id, checkpoint_id, state_json, Utc::now().timestamp()],
        )
        .with_context(|| {
            format!("failed to save checkpoint {checkpoint_id} for thread {thread_id}")
        })?;
        Ok(())
    }

    /// Load a checkpoint for a thread.
    ///
    /// If `checkpoint_id` is provided, loads that specific checkpoint. Otherwise,
    /// loads the most recently created checkpoint for the thread. Returns `None`
    /// if no matching checkpoint exists.
    pub fn load_checkpoint(
        &self,
        thread_id: &str,
        checkpoint_id: Option<&str>,
    ) -> Result<Option<CheckpointRecord>> {
        let conn = self.conn()?;
        if let Some(checkpoint_id) = checkpoint_id {
            let row = conn
                .query_row(
                    "SELECT thread_id, checkpoint_id, state_json, created_at FROM checkpoints WHERE thread_id = ?1 AND checkpoint_id = ?2",
                    params![thread_id, checkpoint_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    },
                )
                .optional()
                .with_context(|| {
                    format!("failed to load checkpoint {checkpoint_id} for thread {thread_id}")
                })?;
            if let Some((thread_id, checkpoint_id, state_json, created_at)) = row {
                let state = parse_checkpoint_state(&state_json)?;
                return Ok(Some(CheckpointRecord {
                    thread_id,
                    checkpoint_id,
                    state,
                    created_at,
                }));
            }
            return Ok(None);
        }

        let row = conn
            .query_row(
                "SELECT thread_id, checkpoint_id, state_json, created_at FROM checkpoints WHERE thread_id = ?1 ORDER BY created_at DESC LIMIT 1",
                params![thread_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .with_context(|| format!("failed to load latest checkpoint for thread {thread_id}"))?;
        if let Some((thread_id, checkpoint_id, state_json, created_at)) = row {
            let state = parse_checkpoint_state(&state_json)?;
            return Ok(Some(CheckpointRecord {
                thread_id,
                checkpoint_id,
                state,
                created_at,
            }));
        }
        Ok(None)
    }

    /// List checkpoints for a thread, ordered by creation time (newest first).
    ///
    /// The `limit` parameter caps the number of results and defaults to 100.
    pub fn list_checkpoints(
        &self,
        thread_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<CheckpointRecord>> {
        let conn = self.conn()?;
        let limit = i64::try_from(limit.unwrap_or(100)).unwrap_or(100);
        let mut stmt = conn
            .prepare(
                "SELECT thread_id, checkpoint_id, state_json, created_at FROM checkpoints WHERE thread_id = ?1 ORDER BY created_at DESC LIMIT ?2",
            )
            .context("failed to prepare checkpoint list query")?;
        let mut rows = stmt
            .query(params![thread_id, limit])
            .with_context(|| format!("failed to list checkpoints for thread {thread_id}"))?;

        let mut out = Vec::new();
        while let Some(row) = rows.next().context("failed to iterate checkpoint rows")? {
            let state_json: String = row.get(2).context("failed to read checkpoint state json")?;
            let state = parse_checkpoint_state(&state_json)?;
            out.push(CheckpointRecord {
                thread_id: row.get(0).context("failed to read checkpoint thread id")?,
                checkpoint_id: row.get(1).context("failed to read checkpoint id")?,
                state,
                created_at: row.get(3).context("failed to read checkpoint timestamp")?,
            });
        }
        Ok(out)
    }

    /// Look up the rollout file path for a thread by its ID.
    pub fn find_rollout_path_by_id(&self, id: &str) -> Result<Option<PathBuf>> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT rollout_path FROM threads WHERE id = ?1",
            params![id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .context("failed to lookup rollout path")
        .map(|opt| opt.flatten().map(PathBuf::from))
    }

    /// Append an entry to the JSONL session index file.
    ///
    /// The session index is an append-only log that maps thread IDs to their names,
    /// update timestamps, and rollout paths. It is used for fast name-based lookups
    /// without opening the SQLite database.
    pub fn append_thread_name(
        &self,
        thread_id: &str,
        thread_name: Option<String>,
        updated_at: i64,
        rollout_path: Option<PathBuf>,
    ) -> Result<()> {
        // Hold the index lock for the entire append + compaction so a concurrent
        // `StateStore` clone cannot rename the index while we are appending.
        let _guard = SESSION_INDEX_LOCK.lock().unwrap();
        if let Some(parent) = self.session_index_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create session index directory {}",
                    parent.display()
                )
            })?;
        }
        let entry = SessionIndexEntry {
            thread_id: thread_id.to_string(),
            thread_name,
            updated_at,
            rollout_path,
        };
        let encoded =
            serde_json::to_string(&entry).context("failed to serialize session index entry")?;
        // Append and compaction share one lock. Compaction rewrites the file
        // from a snapshot and renames over it, so an append landing between
        // that snapshot and the rename would be discarded — silently, since
        // the append already returned success to its caller.
        self.with_session_index_lock(|| {
            let mut file = OpenOptions::new()
                .create(true)
                .read(true)
                .append(true)
                .open(&self.session_index_path)
                .with_context(|| {
                    format!(
                        "failed to open session index {}",
                        self.session_index_path.display()
                    )
                })?;
            let mut last = *b"\n";
            if file.metadata()?.len() > 0 {
                file.seek(SeekFrom::End(-1))?;
                file.read_exact(&mut last)?;
            }
            let separator = if last[0] == b'\n' { "" } else { "\n" };
            writeln!(file, "{separator}{encoded}")
                .context("failed to append session index entry")?;
            // Durability: without this a crash mid-write can leave a torn
            // final line. Reads tolerate one (see `session_index_map`), but
            // not losing the entry beats recovering from having lost it.
            file.sync_data()
                .context("failed to flush session index entry")?;
            drop(file);
            self.compact_session_index_locked()
        })
    }

    /// Run `operation` holding the exclusive session-index lock.
    ///
    /// The lock is an adjacent `.lock` file rather than the index itself, so
    /// compaction's rename cannot pull the lock out from under a waiter. This
    /// mirrors the discipline `codewhale-config` uses for `config.toml`.
    fn with_session_index_lock<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        if let Some(parent) = self.session_index_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create session index directory {}",
                    parent.display()
                )
            })?;
        }
        let lock_path = self.session_index_path.with_extension("jsonl.lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            // The file is only a lock handle; its contents are never read and
            // truncating it would race other holders for no benefit.
            .truncate(false)
            .open(&lock_path)
            .with_context(|| {
                format!("failed to open session index lock {}", lock_path.display())
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            lock_file
                .set_permissions(fs::Permissions::from_mode(0o600))
                .with_context(|| {
                    format!(
                        "failed to secure session index lock {}",
                        lock_path.display()
                    )
                })?;
        }
        let mut lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock
            .write()
            .with_context(|| format!("failed to lock session index {}", lock_path.display()))?;
        operation()
    }

    /// Find the display name for a thread by its ID, using the session index.
    ///
    /// Returns `None` if the thread is not in the index or has no name.
    pub fn find_thread_name_by_id(&self, thread_id: &str) -> Result<Option<String>> {
        let map = self.session_index_map()?;
        Ok(map
            .get(thread_id)
            .and_then(|entry| entry.thread_name.clone()))
    }

    /// Look up display names for multiple thread IDs at once.
    ///
    /// Returns a map from thread ID to its name (which may be `None`).
    pub fn find_thread_names_by_ids(
        &self,
        ids: &[String],
    ) -> Result<HashMap<String, Option<String>>> {
        let map = self.session_index_map()?;
        let mut out = HashMap::new();
        for id in ids {
            let name = map.get(id).and_then(|entry| entry.thread_name.clone());
            out.insert(id.clone(), name);
        }
        Ok(out)
    }

    /// Find the rollout path for a thread by its display name (case-insensitive).
    ///
    /// If multiple threads share the same name, the most recently updated one is returned.
    /// Returns `None` if no matching thread is found.
    pub fn find_thread_path_by_name_str(&self, name: &str) -> Result<Option<PathBuf>> {
        let map = self.session_index_map()?;
        let matched = map
            .values()
            .filter(|entry| {
                entry
                    .thread_name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
            .max_by_key(|entry| entry.updated_at);
        Ok(matched.and_then(|entry| entry.rollout_path.clone()))
    }

    /// Compact the session index. The caller must already hold the lock from
    /// [`Self::with_session_index_lock`]: this reads a snapshot and renames a
    /// rewritten file over the live one, and an append interleaved between
    /// those two steps is lost.
    fn compact_session_index_locked(&self) -> Result<()> {
        if !self.session_index_path.exists() {
            return Ok(());
        }
        let line_count = BufReader::new(
            OpenOptions::new()
                .read(true)
                .open(&self.session_index_path)
                .with_context(|| {
                    format!(
                        "failed to read session index {}",
                        self.session_index_path.display()
                    )
                })?,
        )
        .lines()
        .filter(|line| {
            line.as_ref()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
        })
        .count();
        if line_count <= session_index_compact_line_threshold() {
            return Ok(());
        }

        let latest = self.session_index_map()?;
        self.rewrite_session_index_locked(&latest)
    }

    /// Replace the session index with exactly `latest`, one line per thread.
    /// The caller holds the lock from [`Self::with_session_index_lock`].
    fn rewrite_session_index_locked(
        &self,
        latest: &HashMap<String, SessionIndexEntry>,
    ) -> Result<()> {
        let compact_path = self.session_index_path.with_extension("jsonl.compact");
        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&compact_path)
                .with_context(|| {
                    format!(
                        "failed to open compact session index {}",
                        compact_path.display()
                    )
                })?;
            for entry in latest.values() {
                let encoded = serde_json::to_string(entry)
                    .context("failed to serialize compact session index entry")?;
                writeln!(file, "{encoded}")
                    .context("failed to write compact session index entry")?;
            }
        }
        // The snapshot is written but the live file is still the old one:
        // this is the window an unsynchronized appender would write into and
        // lose. Tests widen it deliberately to prove the lock closes it.
        #[cfg(test)]
        tests::compaction_midpoint(&self.session_index_path);
        fs::rename(&compact_path, &self.session_index_path).with_context(|| {
            format!(
                "failed to replace session index {}",
                self.session_index_path.display()
            )
        })?;
        Ok(())
    }

    #[cfg(test)]
    fn session_index_line_count(&self) -> Result<usize> {
        if !self.session_index_path.exists() {
            return Ok(0);
        }
        Ok(BufReader::new(
            OpenOptions::new()
                .read(true)
                .open(&self.session_index_path)
                .with_context(|| {
                    format!(
                        "failed to read session index {}",
                        self.session_index_path.display()
                    )
                })?,
        )
        .lines()
        .filter(|line| {
            line.as_ref()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
        })
        .count())
    }

    fn session_index_map(&self) -> Result<HashMap<String, SessionIndexEntry>> {
        if !self.session_index_path.exists() {
            return Ok(HashMap::new());
        }
        let file = OpenOptions::new()
            .read(true)
            .open(&self.session_index_path)
            .with_context(|| {
                format!(
                    "failed to read session index {}",
                    self.session_index_path.display()
                )
            })?;
        let reader = BufReader::new(file);
        let mut latest = HashMap::<String, SessionIndexEntry>::new();
        for line in reader.lines() {
            let line = line.context("failed to read session index line")?;
            if line.trim().is_empty() {
                continue;
            }
            // Skip a line we can't parse instead of failing the whole read.
            // An append that was interrupted mid-write leaves a torn final
            // line; aborting here broke every thread-name lookup, and because
            // compaction reads through this same function, the index could
            // never repair itself either — the file stayed broken until
            // someone deleted it by hand.
            match serde_json::from_str::<SessionIndexEntry>(&line) {
                Ok(parsed) => {
                    latest.insert(parsed.thread_id.clone(), parsed);
                }
                Err(err) => {
                    tracing::warn!(
                        "skipping unparseable session index entry in {}: {err}",
                        self.session_index_path.display()
                    );
                }
            }
        }
        Ok(latest)
    }
}

/// Resolve the default SQLite state path without opening or creating it.
///
/// An explicit `CODEWHALE_HOME` always yields `<override>/state.db` and blocks
/// ambient legacy fallback. Without an override, an existing legacy database
/// remains readable until it is migrated.
#[must_use]
pub fn default_state_db_path() -> PathBuf {
    // $CODEWHALE_HOME is a hard override of the base data directory
    // (docs/CONFIGURATION.md): when set, the state DB lives under it and we do
    // NOT fall back to the legacy ~/.deepseek path — silent fallback would
    // defeat the isolation the override promises (CI, containers, multi-project,
    // test harnesses). Legacy ~/.deepseek migration only applies to the default
    // home location.
    if let Some(overridden) = codewhale_home_override().ok().flatten() {
        return overridden.join("state.db");
    }
    let home = codewhale_paths::user_home().unwrap_or_else(|| PathBuf::from("."));
    // Prefer the CodeWhale directory, falling back to legacy DeepSeek path
    // so existing installs don't lose their session history.
    let primary = home.join(CODEWHALE_APP_DIR).join("state.db");
    if primary.exists() || !home.join(LEGACY_APP_DIR).join("state.db").exists() {
        primary
    } else {
        home.join(LEGACY_APP_DIR).join("state.db")
    }
}

fn write_thread_metadata_on(conn: &Connection, thread: &ThreadMetadata) -> Result<()> {
    conn.execute(
            r#"
            INSERT INTO threads (
                id, rollout_path, preview, ephemeral, model_provider, created_at, updated_at, status, path, cwd,
                cli_version, source, title, sandbox_policy, approval_mode, archived, archived_at,
                git_sha, git_branch, git_origin_url, memory_mode
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                ?18, ?19, ?20, ?21
            )
            ON CONFLICT(id) DO UPDATE SET
                rollout_path=excluded.rollout_path,
                preview=excluded.preview,
                ephemeral=excluded.ephemeral,
                model_provider=excluded.model_provider,
                created_at=excluded.created_at,
                updated_at=excluded.updated_at,
                status=excluded.status,
                path=excluded.path,
                cwd=excluded.cwd,
                cli_version=excluded.cli_version,
                source=excluded.source,
                title=excluded.title,
                sandbox_policy=excluded.sandbox_policy,
                approval_mode=excluded.approval_mode,
                archived=excluded.archived,
                archived_at=excluded.archived_at,
                git_sha=excluded.git_sha,
                git_branch=excluded.git_branch,
                git_origin_url=excluded.git_origin_url,
                memory_mode=excluded.memory_mode
            "#,
            params![
                thread.id,
                path_to_opt_string(thread.rollout_path.as_deref()),
                thread.preview,
                bool_to_i64(thread.ephemeral),
                thread.model_provider,
                thread.created_at,
                thread.updated_at,
                thread_status_to_str(&thread.status),
                path_to_opt_string(thread.path.as_deref()),
                thread.cwd.display().to_string(),
                thread.cli_version,
                session_source_to_str(&thread.source),
                thread.name,
                thread.sandbox_policy,
                thread.approval_mode,
                bool_to_i64(thread.archived),
                thread.archived_at,
                thread.git_sha,
                thread.git_branch,
                thread.git_origin_url,
                thread.memory_mode,
            ],
        )
        .context("failed to upsert thread metadata")?;
    Ok(())
}

fn write_thread_goal_on(conn: &Connection, goal: &ThreadGoalRecord) -> Result<()> {
    codewhale_protocol::validate_goal_stall_state(
        goal.last_gap_fingerprint.as_deref(),
        goal.repeated_gap_count,
        goal.last_gap_pass,
        u32::try_from(goal.continuation_count.max(0)).unwrap_or(u32::MAX),
    )
    .map_err(anyhow::Error::msg)?;
    let pause_reason = goal
        .pause_reason
        .map(|reason| serde_json::to_string(&reason))
        .transpose()?;
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM threads WHERE id = ?1",
            params![goal.thread_id],
            |row| row.get(0),
        )
        .optional()
        .context("failed to verify thread before saving goal")?;
    if exists.is_none() {
        anyhow::bail!("thread {} not found", goal.thread_id);
    }

    conn.execute(
        r#"
            INSERT INTO thread_goals (
                thread_id, goal_id, objective, status, token_budget, tokens_used,
                time_used_seconds, continuation_count, created_at, updated_at,
                last_gap_fingerprint, repeated_gap_count, last_gap_pass, pause_reason
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            ON CONFLICT(thread_id) DO UPDATE SET
                goal_id=excluded.goal_id,
                objective=excluded.objective,
                status=excluded.status,
                token_budget=excluded.token_budget,
                tokens_used=excluded.tokens_used,
                time_used_seconds=excluded.time_used_seconds,
                continuation_count=excluded.continuation_count,
                created_at=excluded.created_at,
                updated_at=excluded.updated_at,
                last_gap_fingerprint=excluded.last_gap_fingerprint,
                repeated_gap_count=excluded.repeated_gap_count,
                last_gap_pass=excluded.last_gap_pass,
                pause_reason=excluded.pause_reason
            "#,
        params![
            goal.thread_id,
            goal.goal_id,
            goal.objective,
            thread_goal_status_to_str(&goal.status),
            goal.token_budget,
            goal.tokens_used,
            goal.time_used_seconds,
            goal.continuation_count,
            goal.created_at,
            goal.updated_at,
            goal.last_gap_fingerprint,
            goal.repeated_gap_count,
            goal.last_gap_pass,
            pause_reason,
        ],
    )
    .context("failed to upsert thread goal")?;
    Ok(())
}

fn bool_to_i64(value: bool) -> i64 {
    if value { 1 } else { 0 }
}

/// Whether `table` currently has a column named `column`.
///
/// Used to guard `ALTER TABLE ... ADD COLUMN` migrations so they are
/// idempotent. Both identifiers are compile-time literals at every call
/// site, never user input. A missing table reports `false`, matching the
/// fresh-database case where the migration must still run.
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn i64_to_bool(value: i64) -> bool {
    value != 0
}

fn thread_status_to_str(status: &ThreadStatus) -> &'static str {
    match status {
        ThreadStatus::Running => "running",
        ThreadStatus::Idle => "idle",
        ThreadStatus::Completed => "completed",
        ThreadStatus::Failed => "failed",
        ThreadStatus::Paused => "paused",
        ThreadStatus::Archived => "archived",
    }
}

fn thread_status_from_str(value: &str) -> ThreadStatus {
    match value {
        "running" => ThreadStatus::Running,
        "idle" => ThreadStatus::Idle,
        "completed" => ThreadStatus::Completed,
        "failed" => ThreadStatus::Failed,
        "paused" => ThreadStatus::Paused,
        "archived" => ThreadStatus::Archived,
        _ => ThreadStatus::Idle,
    }
}

fn session_source_to_str(source: &SessionSource) -> &'static str {
    match source {
        SessionSource::Interactive => "interactive",
        SessionSource::Resume => "resume",
        SessionSource::Fork => "fork",
        SessionSource::Api => "api",
        SessionSource::Unknown => "unknown",
    }
}

fn session_source_from_str(value: &str) -> SessionSource {
    match value {
        "interactive" => SessionSource::Interactive,
        "resume" => SessionSource::Resume,
        "fork" => SessionSource::Fork,
        "api" => SessionSource::Api,
        _ => SessionSource::Unknown,
    }
}

fn path_to_opt_string(path: Option<&Path>) -> Option<String> {
    path.map(|p| p.display().to_string())
}

fn parse_checkpoint_state(state_json: &str) -> Result<Value> {
    serde_json::from_str(state_json).context("failed to parse checkpoint state json")
}

fn thread_goal_status_to_str(status: &ThreadGoalStatus) -> &'static str {
    match status {
        ThreadGoalStatus::Active => "active",
        ThreadGoalStatus::Paused => "paused",
        ThreadGoalStatus::Blocked => "blocked",
        ThreadGoalStatus::UsageLimited => "usage_limited",
        ThreadGoalStatus::BudgetLimited => "budget_limited",
        ThreadGoalStatus::Complete => "complete",
    }
}

fn thread_goal_status_from_str(value: &str) -> ThreadGoalStatus {
    match value {
        "active" => ThreadGoalStatus::Active,
        "paused" => ThreadGoalStatus::Paused,
        "blocked" => ThreadGoalStatus::Blocked,
        "usage_limited" => ThreadGoalStatus::UsageLimited,
        "budget_limited" => ThreadGoalStatus::BudgetLimited,
        "complete" => ThreadGoalStatus::Complete,
        // Fail closed: an unknown or corrupted persisted value must never
        // resurrect a self-driving goal. The user can inspect and explicitly
        // resume a paused goal after repairing or replacing the record.
        _ => ThreadGoalStatus::Paused,
    }
}

fn row_to_thread(row: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadMetadata> {
    let status_raw: String = row.get(7)?;
    let source_raw: String = row.get(11)?;
    let rollout_path: Option<String> = row.get(1)?;
    let path: Option<String> = row.get(8)?;
    Ok(ThreadMetadata {
        id: row.get(0)?,
        rollout_path: rollout_path.map(PathBuf::from),
        preview: row.get(2)?,
        ephemeral: i64_to_bool(row.get(3)?),
        model_provider: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        status: thread_status_from_str(&status_raw),
        path: path.map(PathBuf::from),
        cwd: PathBuf::from(row.get::<_, String>(9)?),
        cli_version: row.get(10)?,
        source: session_source_from_str(&source_raw),
        name: row.get(12)?,
        sandbox_policy: row.get(13)?,
        approval_mode: row.get(14)?,
        archived: i64_to_bool(row.get(15)?),
        archived_at: row.get(16)?,
        git_sha: row.get(17)?,
        git_branch: row.get(18)?,
        git_origin_url: row.get(19)?,
        memory_mode: row.get(20)?,
        current_leaf_id: row.get(21)?,
    })
}

fn row_to_thread_goal(row: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadGoalRecord> {
    let status_raw: String = row.get(3)?;
    let pause_reason: Option<String> = row.get(13)?;
    let goal = ThreadGoalRecord {
        thread_id: row.get(0)?,
        goal_id: row.get(1)?,
        objective: row.get(2)?,
        status: thread_goal_status_from_str(&status_raw),
        token_budget: row.get(4)?,
        tokens_used: row.get(5)?,
        time_used_seconds: row.get(6)?,
        continuation_count: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        last_gap_fingerprint: row.get(10)?,
        repeated_gap_count: row.get(11)?,
        last_gap_pass: row.get(12)?,
        pause_reason: pause_reason
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    13,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
    };
    codewhale_protocol::validate_goal_stall_state(
        goal.last_gap_fingerprint.as_deref(),
        goal.repeated_gap_count,
        goal.last_gap_pass,
        u32::try_from(goal.continuation_count.max(0)).unwrap_or(u32::MAX),
    )
    .map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            10,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
        )
    })?;
    Ok(goal)
}

impl ThreadGoalRecord {
    /// Same restore rule as `codewhale_protocol::ThreadGoal`: an Active
    /// record with an exhausted stall window is corrupt and reads as paused.
    fn normalize_restored_stall_state(&mut self) {
        if matches!(self.status, ThreadGoalStatus::Active)
            && self.repeated_gap_count >= codewhale_protocol::MAX_REPEATED_GAP_COUNT
        {
            self.status = ThreadGoalStatus::Paused;
            self.pause_reason = Some(codewhale_protocol::GoalPauseReason::NoProgress);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Barrier, Mutex, mpsc};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn temp_state_dir(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "codewhale-state-{name}-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp state dir");
        dir
    }

    fn temp_state_store(name: &str) -> StateStore {
        let dir = temp_state_dir(name);
        StateStore::open(Some(dir.join("state.db"))).expect("open state store")
    }

    fn test_thread(id: &str) -> ThreadMetadata {
        ThreadMetadata {
            id: id.to_string(),
            rollout_path: None,
            preview: "test thread".to_string(),
            ephemeral: false,
            model_provider: "deepseek".to_string(),
            created_at: 10,
            updated_at: 10,
            status: ThreadStatus::Running,
            path: None,
            cwd: PathBuf::from("/tmp/codewhale"),
            cli_version: "0.0.0-test".to_string(),
            source: SessionSource::Interactive,
            name: None,
            sandbox_policy: None,
            approval_mode: None,
            archived: false,
            archived_at: None,
            git_sha: None,
            git_branch: None,
            git_origin_url: None,
            memory_mode: None,
            current_leaf_id: None,
        }
    }

    fn test_goal(thread_id: &str, objective: &str) -> ThreadGoalRecord {
        ThreadGoalRecord {
            thread_id: thread_id.to_string(),
            goal_id: "goal-1".to_string(),
            objective: objective.to_string(),
            status: ThreadGoalStatus::Active,
            token_budget: Some(123),
            tokens_used: 7,
            time_used_seconds: 11,
            continuation_count: 0,
            last_gap_fingerprint: None,
            repeated_gap_count: 0,
            last_gap_pass: None,
            pause_reason: None,
            created_at: 100,
            updated_at: 101,
        }
    }

    #[test]
    fn unknown_persisted_goal_status_fails_closed() {
        assert_eq!(
            thread_goal_status_from_str("future_or_corrupt_status"),
            ThreadGoalStatus::Paused
        );
    }

    #[test]
    fn thread_goal_crud_round_trips_and_replaces() {
        let store = temp_state_store("thread-goal-crud");
        store
            .upsert_thread(&test_thread("thread-1"))
            .expect("upsert thread");

        let goal = test_goal("thread-1", "Ship v0.8.59");
        store.upsert_thread_goal(&goal).expect("upsert goal");
        assert_eq!(
            store
                .get_thread_goal("thread-1")
                .expect("read goal")
                .as_ref(),
            Some(&goal)
        );

        let mut replacement = test_goal("thread-1", "Ship v0.8.59 safely");
        replacement.goal_id = "goal-2".to_string();
        replacement.status = ThreadGoalStatus::BudgetLimited;
        replacement.token_budget = None;
        replacement.updated_at = 202;
        store
            .upsert_thread_goal(&replacement)
            .expect("replace goal");
        assert_eq!(
            store.get_thread_goal("thread-1").expect("read replacement"),
            Some(replacement)
        );

        assert!(store.delete_thread_goal("thread-1").expect("delete goal"));
        assert!(
            store
                .get_thread_goal("thread-1")
                .expect("read empty")
                .is_none()
        );
        assert!(!store.delete_thread_goal("thread-1").expect("delete empty"));
    }

    #[test]
    fn thread_goal_requires_existing_thread() {
        let store = temp_state_store("thread-goal-missing-thread");
        let err = store
            .upsert_thread_goal(&test_goal("missing-thread", "nope"))
            .expect_err("goal without a thread should fail");
        assert!(err.to_string().contains("thread missing-thread not found"));
    }

    /// Audit R03-07: two first opens of one fresh database. The loser must
    /// decide from the winner's committed schema, not from a probe taken
    /// before it could write. The old code read "column missing" outside its
    /// transaction, waited out the winner, then failed its own `ADD COLUMN`
    /// with "duplicate column name".
    #[test]
    fn a_first_open_that_loses_the_migration_race_still_opens() {
        let dir = temp_state_dir("migration-race");
        let path = dir.join("state.db");
        let winner = Connection::open(&path).expect("open winner");
        let mode: String = winner
            .query_row("PRAGMA journal_mode=WAL;", [], |row| row.get(0))
            .expect("wal");
        assert_eq!(mode, "wal");
        // The winner is mid-migration: the v1 columns exist inside its
        // uncommitted write transaction, user_version is still 0.
        winner
            .execute_batch(
                r#"
                BEGIN IMMEDIATE;
                CREATE TABLE threads (
                    id TEXT PRIMARY KEY, rollout_path TEXT, preview TEXT NOT NULL,
                    ephemeral INTEGER NOT NULL, model_provider TEXT NOT NULL,
                    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                    status TEXT NOT NULL, path TEXT, cwd TEXT NOT NULL,
                    cli_version TEXT NOT NULL, source TEXT NOT NULL, title TEXT,
                    sandbox_policy TEXT, approval_mode TEXT,
                    archived INTEGER NOT NULL DEFAULT 0, archived_at INTEGER,
                    git_sha TEXT, git_branch TEXT, git_origin_url TEXT,
                    memory_mode TEXT, current_leaf_id INTEGER NULL
                );
                CREATE TABLE messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, thread_id TEXT NOT NULL,
                    role TEXT NOT NULL, content TEXT NOT NULL, item_json TEXT,
                    created_at INTEGER NOT NULL, parent_entry_id INTEGER NULL,
                    FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
                );
                "#,
            )
            .expect("winner begins its migration");

        let loser = {
            let path = path.clone();
            thread::spawn(move || StateStore::open(Some(path)).map(|_| ()))
        };
        // Long enough for the loser to probe the schema; well inside its
        // five-second busy timeout.
        thread::sleep(Duration::from_millis(500));
        winner.execute_batch("COMMIT;").expect("winner commits");

        loser
            .join()
            .expect("loser thread")
            .expect("the losing first open must succeed");
    }

    /// Audit R05-05: a batch append is all or nothing. The pattern it
    /// replaces (one `append_message` transaction per item) leaves a
    /// partial chain when a later item fails.
    #[test]
    fn append_messages_is_all_or_nothing() {
        let store = temp_state_store("append-batch-atomic");
        store
            .upsert_thread(&test_thread("thread-1"))
            .expect("upsert thread");
        store
            .conn()
            .expect("conn")
            .execute_batch(
                "CREATE TRIGGER refuse_boom BEFORE INSERT ON messages \
                 WHEN NEW.content = 'boom' BEGIN SELECT RAISE(ABORT, 'boom'); END;",
            )
            .expect("install failing trigger");
        let batch = ["one", "boom"].map(|content| NewMessage {
            role: "history".to_string(),
            content: content.to_string(),
            item: None,
        });

        // The per-item pattern leaves "one" behind.
        let per_item: Result<Vec<i64>> = batch
            .iter()
            .map(|message| store.append_message("thread-1", &message.role, &message.content, None))
            .collect();
        assert!(per_item.is_err());
        let partial = store.list_messages("thread-1", None).expect("list");
        assert_eq!(partial.len(), 1, "per-item appends leave a partial chain");
        store.clear_messages("thread-1").expect("reset");

        assert!(store.append_messages("thread-1", &batch).is_err());
        assert!(
            store
                .list_messages("thread-1", None)
                .expect("list")
                .is_empty(),
            "a failed batch must leave no messages"
        );
        let leaf: Option<i64> = store
            .conn()
            .expect("conn")
            .query_row(
                "SELECT current_leaf_id FROM threads WHERE id = 'thread-1'",
                [],
                |row| row.get(0),
            )
            .expect("leaf");
        assert_eq!(leaf, None, "a failed batch must not move the leaf");

        let ok = ["a", "b", "c"].map(|content| NewMessage {
            role: "history".to_string(),
            content: content.to_string(),
            item: None,
        });
        let ids = store
            .append_messages("thread-1", &ok)
            .expect("append batch");
        let chain = store.list_messages("thread-1", None).expect("list");
        assert_eq!(chain.iter().map(|m| m.id).collect::<Vec<_>>(), ids);
        assert_eq!(chain[1].parent_entry_id, Some(ids[0]));
        assert_eq!(chain[2].parent_entry_id, Some(ids[1]));
    }

    /// Audit R03-09: a deleted thread must not keep answering for its name
    /// in the session index, shadowing a live thread of the same name.
    #[test]
    fn deleted_thread_leaves_the_session_index() {
        let store = temp_state_store("delete-index");
        store
            .append_thread_name(
                "live",
                Some("Build".to_string()),
                100,
                Some(PathBuf::from("/live")),
            )
            .expect("index live");
        store
            .append_thread_name(
                "gone",
                Some("build".to_string()),
                200,
                Some(PathBuf::from("/gone")),
            )
            .expect("index gone");
        assert_eq!(
            store.find_thread_path_by_name_str("build").expect("find"),
            Some(PathBuf::from("/gone"))
        );

        store.delete_thread("gone").expect("delete");

        assert_eq!(
            store.find_thread_path_by_name_str("build").expect("find"),
            Some(PathBuf::from("/live"))
        );
    }

    #[test]
    fn delete_thread_cascades_child_rows() {
        let store = temp_state_store("thread-delete-cascade");
        store
            .upsert_thread(&test_thread("thread-1"))
            .expect("upsert thread");
        store
            .append_message("thread-1", "user", "hello", None)
            .expect("append message");
        store
            .save_checkpoint("thread-1", "checkpoint-1", &serde_json::json!({"ok": true}))
            .expect("save checkpoint");
        store
            .persist_dynamic_tools(
                "thread-1",
                &[DynamicToolRecord {
                    position: 0,
                    name: "test_tool".to_string(),
                    description: Some("test".to_string()),
                    input_schema: serde_json::json!({"type": "object"}),
                }],
            )
            .expect("persist dynamic tools");
        store
            .upsert_thread_goal(&test_goal("thread-1", "Ship v0.8.67"))
            .expect("upsert goal");

        store.delete_thread("thread-1").expect("delete thread");

        let conn = store.conn().expect("conn");
        for table in [
            "messages",
            "checkpoints",
            "thread_dynamic_tools",
            "thread_goals",
        ] {
            let sql = format!("SELECT COUNT(*) FROM {table} WHERE thread_id = ?1");
            let count: i64 = conn
                .query_row(&sql, params!["thread-1"], |row| row.get(0))
                .expect("count child rows");
            assert_eq!(count, 0, "{table} row survived thread deletion");
        }
    }

    #[test]
    fn state_store_reuses_one_connection_across_operations_and_clones() {
        let store = temp_state_store("conn-reuse");
        {
            let conn = store.conn().expect("conn");
            conn.execute_batch("CREATE TEMP TABLE conn_reuse_probe(id INTEGER);")
                .expect("create temp table");
        }
        // TEMP tables are visible only on the connection that created them, so
        // seeing the probe again — through a clone, after real operations ran —
        // proves the store holds one long-lived connection instead of
        // reopening the database (and reapplying pragmas) per call.
        let clone = store.clone();
        clone
            .upsert_thread(&test_thread("thread-conn-reuse"))
            .expect("upsert thread");
        let conn = clone.conn().expect("conn");
        let probe_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_temp_master WHERE name = 'conn_reuse_probe'",
                [],
                |row| row.get(0),
            )
            .expect("query temp master");
        assert_eq!(
            probe_count, 1,
            "temp table not visible: a fresh connection was opened"
        );
        // The pragma applied once at open still governs the shared connection.
        let foreign_keys: i64 = conn
            .query_row("PRAGMA foreign_keys;", [], |row| row.get(0))
            .expect("read foreign_keys pragma");
        assert_eq!(foreign_keys, 1);
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode;", [], |row| row.get(0))
            .expect("read journal_mode pragma");
        assert_eq!(
            journal_mode.to_ascii_lowercase(),
            "wal",
            "open should enable WAL for multi-process readers/writers"
        );
    }

    #[test]
    fn connection_setup_waits_for_database_lock_before_enabling_wal() {
        let dir = temp_state_dir("locked-open");
        let db_path = dir.join("state.db");

        let candidate = Connection::open(&db_path).expect("open candidate connection");
        // Do not let rusqlite's current default mask StateStore's own setup
        // contract: configure_connection must install the wait policy before
        // it performs any operation that can need a database lock.
        candidate
            .busy_timeout(Duration::ZERO)
            .expect("disable dependency default timeout");
        let blocker = Connection::open(&db_path).expect("open blocking connection");
        let (locked_tx, locked_rx) = mpsc::sync_channel(0);
        let blocker_thread = thread::spawn(move || {
            blocker
                .execute_batch("BEGIN EXCLUSIVE;")
                .expect("acquire exclusive database lock");
            locked_tx.send(()).expect("announce database lock");
            thread::sleep(Duration::from_millis(200));
            blocker
                .execute_batch("COMMIT;")
                .expect("release exclusive database lock");
        });

        locked_rx.recv().expect("wait for database lock");
        StateStore::configure_connection(&candidate, &db_path)
            .expect("connection setup should wait for the brief database lock");
        blocker_thread.join().expect("blocking thread panicked");

        let journal_mode: String = candidate
            .query_row("PRAGMA journal_mode;", [], |row| row.get(0))
            .expect("read journal_mode");
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");

        drop(candidate);
        let _ = fs::remove_dir_all(dir);
    }

    /// A second process must wait for a brief active writer instead of
    /// surfacing SQLITE_BUSY (#4734).
    ///
    /// The lock handoff is explicit: unlike a race between many autocommit
    /// writes, this proves the busy timeout while keeping the contention
    /// duration below its documented five-second bound on every platform.
    #[test]
    fn second_connection_waits_for_active_writer() {
        let dir = temp_state_dir("concurrent-write");
        let db_path = dir.join("state.db");

        let store_a = StateStore::open(Some(db_path.clone())).expect("open store a");
        let store_b = StateStore::open(Some(db_path.clone())).expect("open store b");
        let (locked_tx, locked_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);

        let writer_a = thread::spawn(move || {
            let conn = store_a.conn().expect("connection a");
            conn.execute_batch(
                r#"
                BEGIN IMMEDIATE;
                INSERT INTO jobs(id, name, status, created_at, updated_at)
                VALUES ('job-a', 'writer-a', 'running', 0, 0);
                "#,
            )
            .expect("writer a should acquire the database write lock");
            locked_tx.send(()).expect("announce active writer");
            release_rx.recv().expect("wait to release active writer");
            conn.execute_batch("COMMIT;")
                .expect("writer a should commit");
        });

        locked_rx.recv().expect("wait for active writer");
        let (attempting_tx, attempting_rx) = mpsc::sync_channel(0);
        let writer_b = thread::spawn(move || -> Result<()> {
            attempting_tx.send(()).expect("announce second write");
            store_b.conn()?.execute(
                "INSERT INTO jobs(id, name, status, created_at, updated_at) VALUES ('job-b', 'writer-b', 'running', 1, 1)",
                [],
            )?;
            Ok(())
        });

        attempting_rx.recv().expect("wait for second write attempt");
        thread::sleep(Duration::from_millis(100));
        assert!(
            !writer_b.is_finished(),
            "second writer should still be waiting while the first holds the lock"
        );
        release_tx.send(()).expect("release active writer");
        writer_a.join().expect("writer a panicked");
        writer_b
            .join()
            .expect("writer b panicked")
            .expect("writer b should succeed after the lock is released");

        let store = StateStore::open(Some(db_path)).expect("reopen for verify");
        let listed: i64 = store
            .conn()
            .expect("connection")
            .query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get(0))
            .expect("count jobs");
        assert_eq!(listed, 2, "both writers should persist their jobs");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn runtime_thread_link_round_trips_and_goes_with_its_thread() {
        let store = temp_state_store("runtime-thread-link");
        store
            .upsert_thread(&test_thread("thread-linked"))
            .expect("upsert thread");
        assert_eq!(
            store
                .get_runtime_thread_link("thread-linked")
                .expect("read"),
            None
        );
        store
            .set_runtime_thread_link("thread-linked", "thr_runtime_1")
            .expect("link");
        store
            .set_runtime_thread_link("thread-linked", "thr_runtime_2")
            .expect("relink");
        assert_eq!(
            store
                .get_runtime_thread_link("thread-linked")
                .expect("read")
                .as_deref(),
            Some("thr_runtime_2")
        );
        assert!(
            store
                .set_runtime_thread_link("no-such-thread", "thr_runtime_3")
                .is_err(),
            "a link needs an existing thread"
        );
        store.delete_thread("thread-linked").expect("delete thread");
        assert_eq!(
            store
                .get_runtime_thread_link("thread-linked")
                .expect("read"),
            None
        );
    }

    #[test]
    fn migration_runs_cleanly_when_schema_predates_user_version_header() {
        // Simulate a restore (or a racing process that crashed before
        // stamping user_version): the on-disk schema is fully migrated but
        // the header still says 0. The v0 block used to re-run unconditional
        // ADD COLUMN statements and abort the open with
        // "duplicate column name".
        let dir = temp_state_dir("migration-v0-idempotent");
        let db_path = dir.join("state.db");
        drop(StateStore::open(Some(db_path.clone())).expect("initial open"));
        {
            let conn = Connection::open(&db_path).expect("raw connection");
            conn.pragma_update(None, "user_version", 0)
                .expect("reset user_version");
        }

        let store = StateStore::open(Some(db_path.clone())).expect("reopen with v0 header");
        store
            .upsert_thread(&test_thread("thread-migrated"))
            .expect("write after guarded migration");

        // Reopening again (now stamped at the current version) still works.
        drop(store);
        let store = StateStore::open(Some(db_path)).expect("third open");
        let persisted = store
            .get_thread("thread-migrated")
            .expect("read after reopen");
        assert!(persisted.is_some());

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn record_thread_goal_usage_accumulates_tokens_and_time() {
        let store = temp_state_store("thread-goal-usage");
        store
            .upsert_thread(&test_thread("thread-1"))
            .expect("upsert thread");

        // Mirror the runtime, which creates goals with zeroed accounting.
        let mut goal = test_goal("thread-1", "Ship the persistent goal loop");
        goal.tokens_used = 0;
        goal.time_used_seconds = 0;
        goal.updated_at = 100;
        store.upsert_thread_goal(&goal).expect("upsert goal");

        // First accrual lands the deltas and advances updated_at.
        let after_first = store
            .record_thread_goal_usage("thread-1", 250, 12, 150)
            .expect("record usage")
            .expect("goal exists");
        assert_eq!(after_first.tokens_used, 250);
        assert_eq!(after_first.time_used_seconds, 12);
        assert_eq!(after_first.updated_at, 150);
        // Identity fields are preserved across accrual.
        assert_eq!(after_first.goal_id, goal.goal_id);
        assert_eq!(after_first.objective, goal.objective);
        assert_eq!(after_first.status, goal.status);
        assert_eq!(after_first.token_budget, goal.token_budget);
        assert_eq!(after_first.created_at, goal.created_at);
        assert_eq!(after_first.continuation_count, 0);

        // Second accrual adds on top of the first (additive, not replacing).
        let after_second = store
            .record_thread_goal_usage("thread-1", 75, 8, 200)
            .expect("record usage")
            .expect("goal exists");
        assert_eq!(after_second.tokens_used, 325);
        assert_eq!(after_second.time_used_seconds, 20);
        assert_eq!(after_second.updated_at, 200);

        // A stale `now` must not move updated_at backwards.
        let after_stale = store
            .record_thread_goal_usage("thread-1", 5, 1, 1)
            .expect("record usage")
            .expect("goal exists");
        assert_eq!(after_stale.tokens_used, 330);
        assert_eq!(after_stale.time_used_seconds, 21);
        assert_eq!(after_stale.updated_at, 200);

        // Read back through the normal getter to confirm durability.
        let persisted = store
            .get_thread_goal("thread-1")
            .expect("read goal")
            .expect("goal exists");
        assert_eq!(persisted.tokens_used, 330);
        assert_eq!(persisted.time_used_seconds, 21);
    }

    #[test]
    fn record_thread_goal_usage_returns_none_without_goal() {
        let store = temp_state_store("thread-goal-usage-missing");
        store
            .upsert_thread(&test_thread("thread-1"))
            .expect("upsert thread");
        // Thread exists but has no goal row yet: accrual is a no-op, not an error,
        // and must not create a goal.
        let result = store
            .record_thread_goal_usage("thread-1", 100, 5, 999)
            .expect("record usage on goalless thread");
        assert!(result.is_none());
        assert!(
            store
                .get_thread_goal("thread-1")
                .expect("read goal")
                .is_none()
        );
    }

    #[test]
    fn record_thread_goal_continuation_accumulates_durably() {
        let store = temp_state_store("thread-goal-continuation");
        store
            .upsert_thread(&test_thread("thread-1"))
            .expect("upsert thread");

        let mut goal = test_goal("thread-1", "Keep working across turns");
        goal.updated_at = 100;
        store.upsert_thread_goal(&goal).expect("upsert goal");

        let after_first = store
            .record_thread_goal_continuation("thread-1", 120)
            .expect("record continuation")
            .expect("goal exists");
        assert_eq!(after_first.continuation_count, 1);
        assert_eq!(after_first.tokens_used, goal.tokens_used);
        assert_eq!(after_first.time_used_seconds, goal.time_used_seconds);
        assert_eq!(after_first.updated_at, 120);

        let after_second = store
            .record_thread_goal_continuation("thread-1", 110)
            .expect("record second continuation")
            .expect("goal exists");
        assert_eq!(after_second.continuation_count, 2);
        assert_eq!(after_second.updated_at, 120);

        let persisted = store
            .get_thread_goal("thread-1")
            .expect("read goal")
            .expect("goal exists");
        assert_eq!(persisted.continuation_count, 2);
    }

    // ── $CODEWHALE_HOME override tests ──────────────────────────────
    //
    // These touch a process-global env var, so they serialize against each
    // other (and restore the prior value) to stay hermetic under parallel test
    // runs — the same concern AGENTS.md flags for config_command_allow_shell_*.

    static CODEWHALE_HOME_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct CodeWhaleHomeGuard {
        prior: Option<std::ffi::OsString>,
    }
    impl CodeWhaleHomeGuard {
        fn set(value: &str) -> Self {
            let prior = std::env::var_os("CODEWHALE_HOME");
            // SAFETY: serialised by CODEWHALE_HOME_TEST_LOCK.
            unsafe { std::env::set_var("CODEWHALE_HOME", value) };
            Self { prior }
        }
        fn remove() -> Self {
            let prior = std::env::var_os("CODEWHALE_HOME");
            // SAFETY: serialised by CODEWHALE_HOME_TEST_LOCK.
            unsafe { std::env::remove_var("CODEWHALE_HOME") };
            Self { prior }
        }
    }
    impl Drop for CodeWhaleHomeGuard {
        fn drop(&mut self) {
            // SAFETY: serialised by CODEWHALE_HOME_TEST_LOCK.
            unsafe {
                match &self.prior {
                    Some(value) => std::env::set_var("CODEWHALE_HOME", value),
                    None => std::env::remove_var("CODEWHALE_HOME"),
                }
            }
        }
    }

    #[test]
    fn codewhale_home_override_returns_the_env_value_verbatim() {
        let _lock = CODEWHALE_HOME_TEST_LOCK.lock().unwrap();
        let override_path = std::env::temp_dir().join("cw-isolated-state");
        let _g = CodeWhaleHomeGuard::set(override_path.to_str().unwrap());
        // The env var IS the home dir — no ".codewhale" appended. This matches
        // codewhale_home() in config ($CODEWHALE_HOME=/x means home is /x).
        assert_eq!(
            codewhale_home_override().unwrap().as_deref(),
            Some(override_path.as_path())
        );
    }

    #[test]
    fn codewhale_home_override_none_when_unset() {
        let _lock = CODEWHALE_HOME_TEST_LOCK.lock().unwrap();
        let _g = CodeWhaleHomeGuard::remove();
        assert!(codewhale_home_override().unwrap().is_none());
    }

    #[test]
    fn codewhale_home_override_none_when_whitespace_only() {
        let _lock = CODEWHALE_HOME_TEST_LOCK.lock().unwrap();
        let _g = CodeWhaleHomeGuard::set("   ");
        assert!(
            codewhale_home_override().unwrap().is_none(),
            "whitespace-only CODEWHALE_HOME must not establish isolation"
        );
    }

    #[test]
    fn default_state_db_path_uses_codewhale_home_when_set() {
        let _lock = CODEWHALE_HOME_TEST_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "cw-home-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _g = CodeWhaleHomeGuard::set(dir.to_str().unwrap());
        // Hard override: the DB is <CODEWHALE_HOME>/state.db, NOT
        // <CODEWHALE_HOME>/.codewhale/state.db, and the legacy ~/.deepseek
        // fallback is bypassed entirely.
        assert_eq!(default_state_db_path(), dir.join("state.db"));
    }

    #[test]
    fn implicit_state_database_refuses_unresolved_home() {
        const PROBE: &str = "CODEWHALE_STATE_DEFAULT_PATH_PROBE";
        if std::env::var_os(PROBE).is_some() {
            let error =
                StateStore::open(None).expect_err("implicit relative state must be refused");
            assert!(error.to_string().contains("absolute home directory"));
            assert!(!Path::new(".codewhale").exists());
            StateStore::open(Some(PathBuf::from("explicit/state.db")))
                .expect("an explicitly chosen relative path remains supported");
            return;
        }
        let probe = std::env::temp_dir().join(format!(
            "cw-state-default-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&probe).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::implicit_state_database_refuses_unresolved_home",
            ])
            .env(PROBE, "1")
            .env_remove("CODEWHALE_HOME")
            .env("HOME", "relative-home")
            .current_dir(&probe)
            .output()
            .unwrap();
        fs::remove_dir_all(&probe).unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn load_checkpoint_propagates_invalid_state_json() {
        let store = temp_state_store("checkpoint-parse-error");
        store
            .upsert_thread(&test_thread("thread-1"))
            .expect("upsert thread");
        store
            .save_checkpoint("thread-1", "broken", &json!({"ok": true}))
            .expect("save checkpoint");

        {
            let conn = store.conn().expect("conn");
            conn.execute(
                "UPDATE checkpoints SET state_json = ?1 WHERE thread_id = ?2 AND checkpoint_id = ?3",
                params!["not-json", "thread-1", "broken"],
            )
            .expect("corrupt checkpoint");
        }

        let err = store
            .load_checkpoint("thread-1", Some("broken"))
            .expect_err("invalid checkpoint json should fail");
        assert!(
            err.to_string()
                .contains("failed to parse checkpoint state json")
        );
    }

    #[test]
    fn session_index_compacts_after_threshold() {
        let store = temp_state_store("session-index-compact");
        for idx in 0..6 {
            store
                .append_thread_name("thread-1", Some(format!("name-{idx}")), idx, None)
                .expect("append session index entry");
        }

        let line_count = store
            .session_index_line_count()
            .expect("count session index lines");
        assert_eq!(line_count, 1);

        let name = store
            .find_thread_name_by_id("thread-1")
            .expect("lookup thread name");
        assert_eq!(name.as_deref(), Some("name-5"));
    }

    #[test]
    fn session_index_read_skips_a_torn_line() {
        // #4735: a crash mid-append leaves a truncated final line. Failing the
        // whole read broke every thread-name lookup at once, and compaction
        // reads through the same path, so the index could not repair itself.
        let store = temp_state_store("session-index-torn");
        store
            .append_thread_name("thread-1", Some("first".to_string()), 1, None)
            .expect("append first entry");

        {
            let mut file = OpenOptions::new()
                .append(true)
                .open(&store.session_index_path)
                .expect("open session index");
            // A write cut off mid-JSON, exactly as a crash would leave it.
            writeln!(file, "{{\"thread_id\":\"thread-2\",\"thread_na").expect("write torn line");
        }

        store
            .append_thread_name("thread-3", Some("third".to_string()), 3, None)
            .expect("append third entry");

        assert_eq!(
            store
                .find_thread_name_by_id("thread-1")
                .expect("lookup thread-1")
                .as_deref(),
            Some("first"),
        );
        assert_eq!(
            store
                .find_thread_name_by_id("thread-3")
                .expect("lookup thread-3")
                .as_deref(),
            Some("third"),
        );
    }

    #[test]
    fn session_index_append_after_an_unterminated_torn_line_is_kept() {
        let store = temp_state_store("session-index-unterminated");
        store
            .append_thread_name("thread-1", Some("first".to_string()), 1, None)
            .expect("append first entry");
        {
            let mut file = OpenOptions::new()
                .append(true)
                .open(&store.session_index_path)
                .expect("open session index");
            write!(file, "{{\"thread_id\":\"thread-2\",\"thread_na").expect("write torn line");
        }

        store
            .append_thread_name("thread-3", Some("third".to_string()), 3, None)
            .expect("append third entry");

        assert_eq!(
            store
                .find_thread_name_by_id("thread-3")
                .expect("lookup thread-3")
                .as_deref(),
            Some("third"),
        );
    }

    /// Hook fired by compaction between writing the snapshot and renaming it
    /// over the live index — the window a concurrent append can be lost in.
    /// Only the store whose path a test registered is affected, so tests
    /// running in parallel don't disturb each other.
    type MidpointHook = Box<dyn Fn() + Send + Sync>;
    static COMPACTION_MIDPOINT: Mutex<Option<(PathBuf, MidpointHook)>> = Mutex::new(None);

    /// Fires at most once: the racing append compacts too, and a hook that
    /// fired twice would re-enter the test's one-shot handshake.
    pub(super) fn compaction_midpoint(index_path: &Path) {
        let mut hook = COMPACTION_MIDPOINT.lock().expect("midpoint hook lock");
        let registered_for_this_store = match hook.as_ref() {
            Some((registered, _)) => registered == index_path,
            None => return,
        };
        if !registered_for_this_store {
            return;
        }
        let (_, callback) = hook.take().expect("presence checked above");
        drop(hook);
        callback();
    }

    #[test]
    fn session_index_compaction_does_not_drop_a_concurrent_append() {
        // #4736: compaction snapshots the file, rewrites it, and renames over
        // the live one. An append landing between those two steps used to
        // vanish — silently, since it had already returned success to its
        // caller. The shared lock serializes the two.
        //
        // The race is real but narrow, so the test drives it deterministically:
        // a hook at the compaction midpoint releases the appender and then
        // waits. Without the lock the appender writes into the doomed file and
        // the rename discards it; with the lock it blocks until compaction
        // finishes, and its entry survives.
        let store = Arc::new(temp_state_store("session-index-race"));
        let threshold = session_index_compact_line_threshold();

        // One line short of the threshold, so the next append compacts.
        for idx in 0..threshold {
            store
                .append_thread_name(
                    &format!("thread-{idx}"),
                    Some(format!("name-{idx}")),
                    1,
                    None,
                )
                .expect("append filler entry");
        }

        let appender_released = Arc::new(Barrier::new(2));
        {
            let released = Arc::clone(&appender_released);
            *COMPACTION_MIDPOINT.lock().expect("midpoint hook lock") = Some((
                store.session_index_path.clone(),
                Box::new(move || {
                    released.wait();
                    // Give the appender time to complete its write into the
                    // window. Under the fix it is blocked on the lock instead.
                    thread::sleep(Duration::from_millis(300));
                }),
            ));
        }

        let appender = {
            let store = Arc::clone(&store);
            let released = Arc::clone(&appender_released);
            thread::spawn(move || {
                released.wait();
                store
                    .append_thread_name("racer", Some("racer-name".to_string()), 2, None)
                    .expect("append racing entry");
            })
        };

        store
            .append_thread_name("trigger", Some("trigger-name".to_string()), 1, None)
            .expect("append entry that triggers compaction");
        appender.join().expect("appender thread");
        *COMPACTION_MIDPOINT.lock().expect("midpoint hook lock") = None;

        assert_eq!(
            store
                .find_thread_name_by_id("racer")
                .expect("lookup racer")
                .as_deref(),
            Some("racer-name"),
            "append was dropped by a concurrent compaction",
        );
        assert_eq!(
            store
                .find_thread_name_by_id("trigger")
                .expect("lookup trigger")
                .as_deref(),
            Some("trigger-name"),
        );
    }
    fn legacy_archive_fixture(id: &str) -> LegacyThreadArchive {
        let mut thread = test_thread(id);
        thread.current_leaf_id = Some(3);
        LegacyThreadArchive {
            thread,
            messages: [
                (1, None, "root"),
                (2, Some(1), "inactive"),
                (3, Some(1), "active"),
            ]
            .into_iter()
            .map(|(entry_id, parent_entry_id, content)| MessageRecord {
                id: entry_id,
                thread_id: id.into(),
                role: "user".into(),
                content: content.into(),
                item: Some(serde_json::json!({"content":content})),
                created_at: 100 + entry_id,
                parent_entry_id,
            })
            .collect(),
            goal: Some(test_goal(id, "historical goal")),
            checkpoints: vec![CheckpointRecord {
                thread_id: id.into(),
                checkpoint_id: "retained".into(),
                state: serde_json::json!({"receipt":"historical"}),
                created_at: 123,
            }],
        }
    }

    #[test]
    fn legacy_archive_restore_keeps_full_graph_goal_checkpoint_and_refuses_replacement() {
        let store = temp_state_store("archive-once");
        let archive = legacy_archive_fixture("legacy");
        store.restore_legacy_thread_archive(&archive).unwrap();
        let snapshot = store.snapshot_legacy_thread_history("legacy").unwrap();
        assert_eq!(
            serde_json::to_value(&snapshot.messages).unwrap(),
            serde_json::to_value(&archive.messages).unwrap()
        );
        assert_eq!(snapshot.current_leaf_id, Some(3));
        assert_eq!(store.list_messages("legacy", None).unwrap().len(), 2);
        let goal = snapshot.goal.unwrap();
        assert_eq!(goal.status, codewhale_protocol::ThreadGoalStatus::Active);
        assert_eq!(goal.tokens_used, archive.goal.as_ref().unwrap().tokens_used);
        assert_eq!(goal.updated_at, archive.goal.as_ref().unwrap().updated_at);
        let checkpoint = store
            .load_checkpoint("legacy", Some("retained"))
            .unwrap()
            .unwrap();
        assert_eq!(checkpoint.created_at, 123);
        assert_eq!(checkpoint.state, archive.checkpoints[0].state);
        let mut replacement = archive.clone();
        replacement.messages[1].content = "changed inactive history".into();
        assert!(store.restore_legacy_thread_archive(&replacement).is_err());
        assert_eq!(
            store
                .snapshot_legacy_thread_history("legacy")
                .unwrap()
                .messages[1]
                .content,
            "inactive"
        );
    }

    #[test]
    fn legacy_archive_restore_refuses_cycles_duplicates_foreign_rows_and_bounds_without_publication()
     {
        let store = temp_state_store("archive-refusals");
        let valid = legacy_archive_fixture("legacy");
        let mut invalid = Vec::new();
        let mut archive = valid.clone();
        archive.messages[0].parent_entry_id = Some(2);
        invalid.push(archive);
        let mut archive = valid.clone();
        archive.messages[1].id = 1;
        invalid.push(archive);
        let mut archive = valid.clone();
        archive.messages[1].parent_entry_id = Some(99);
        invalid.push(archive);
        let mut archive = valid.clone();
        archive.messages[1].thread_id = "foreign".into();
        invalid.push(archive);
        let mut archive = valid.clone();
        archive.thread.current_leaf_id = Some(99);
        invalid.push(archive);
        let mut archive = valid.clone();
        archive.goal.as_mut().unwrap().thread_id = "foreign".into();
        invalid.push(archive);
        let mut archive = valid.clone();
        archive.checkpoints[0].thread_id = "foreign".into();
        invalid.push(archive);
        let mut archive = valid.clone();
        archive.checkpoints.push(archive.checkpoints[0].clone());
        invalid.push(archive);
        let mut archive = valid.clone();
        archive.messages[0].content =
            "x".repeat(codewhale_protocol::MAX_CANONICAL_HISTORY_BYTES + 1);
        invalid.push(archive);
        for archive in invalid {
            assert!(store.restore_legacy_thread_archive(&archive).is_err());
            assert!(store.get_thread("legacy").unwrap().is_none());
        }
        store.restore_legacy_thread_archive(&valid).unwrap();
    }

    #[test]
    fn legacy_archive_restore_rolls_back_metadata_messages_goal_on_late_checkpoint_failure() {
        let store = temp_state_store("archive-transaction");
        store.conn().unwrap().execute_batch("CREATE TRIGGER refuse_archive_checkpoint BEFORE INSERT ON checkpoints BEGIN SELECT RAISE(ABORT, 'fixture checkpoint failure'); END;").unwrap();
        let archive = legacy_archive_fixture("legacy");
        assert!(store.restore_legacy_thread_archive(&archive).is_err());
        let conn = store.conn().unwrap();
        for table in ["threads", "messages", "thread_goals", "checkpoints"] {
            let count: i64 = conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(
                count, 0,
                "{table} must roll back with the immutable archive"
            );
        }
        conn.execute_batch("DROP TRIGGER refuse_archive_checkpoint")
            .unwrap();
        drop(conn);
        store.restore_legacy_thread_archive(&archive).unwrap();
    }
}
