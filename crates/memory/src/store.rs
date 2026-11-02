use crate::{
    Error, Result,
    auth::{Access, Capability},
    model::*,
    policy,
};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, named_params, params};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

/// Whether this path's directory refuses the mode a file is created with *and*
/// the one it is then asked for.
///
/// Some mounts synthesise mode bits instead of storing them: HarmonyOS hmdfs
/// (the 2in1 user directory) reports a group-readable mode for every file an
/// app creates, and `chmod` answers `EPERM` because that owner is not the
/// calling process. There the mode says nothing about who can
/// read the database, and refusing the store would leave the platform with no
/// memory database at all. This creates one probe file at `0600` beside `path`,
/// and — when the mode came back wider — asks the mount to narrow it and looks
/// again. A default ACL or a setgid parent widens a new file on a filesystem
/// that stores modes perfectly well; such a mount honours the second request
/// and keeps the strict refusal, because its mode really does describe access.
///
/// Any failure to run the probe answers `false`, so a directory we cannot write
/// to keeps the strict check rather than being reclassified as a mount limit.
///
/// The same rule, with the same reasoning, lives in
/// `codewhale_config::private_directory::private_footing`; this crate has no
/// dependency on it and the probe is a dozen lines, so it is repeated here
/// rather than widening this crate's dependency graph.
#[cfg(unix)]
fn mount_ignores_unix_modes(path: &Path) -> bool {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return false;
    };
    let probe = parent.join(format!(
        ".codewhale-mode-probe-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos())
    ));
    let created = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&probe);
    let Ok(file) = created else { return false };
    let private = |file: &fs::File| {
        file.metadata()
            .is_ok_and(|metadata| metadata.permissions().mode() & 0o077 == 0)
    };
    let wider = file
        .metadata()
        .is_ok_and(|metadata| metadata.permissions().mode() & 0o077 != 0);
    let refuses_narrowing = wider
        && match file.set_permissions(fs::Permissions::from_mode(0o600)) {
            Ok(()) => !private(&file),
            Err(_) => true,
        };
    drop(file);
    let _ = fs::remove_file(&probe);
    refuses_narrowing
}

/// Nesting-safe transaction scope for `&self` methods. rusqlite's
/// `unchecked_transaction` issues a literal `BEGIN`, which fails inside an
/// already-open transaction; engine methods compose inside each other's
/// scopes (e.g. `prepare_context` → `recall`), so this issues a `SAVEPOINT`
/// instead — a top-level savepoint behaves as a deferred transaction, and a
/// nested one rolls back only its own work when dropped uncommitted.
pub(crate) struct Tx<'c> {
    conn: &'c Connection,
    name: String,
    done: bool,
}
static TX_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
impl<'c> Tx<'c> {
    pub(crate) fn begin(conn: &'c Connection) -> Result<Self> {
        let name = format!(
            "tx_{}",
            TX_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        conn.execute_batch(&format!("SAVEPOINT \"{name}\""))?;
        Ok(Self {
            conn,
            name,
            done: false,
        })
    }
    pub(crate) fn commit(mut self) -> Result<()> {
        self.conn
            .execute_batch(&format!("RELEASE \"{}\"", self.name))?;
        self.done = true;
        Ok(())
    }
}
impl Drop for Tx<'_> {
    fn drop(&mut self) {
        if !self.done {
            let _ = self
                .conn
                .execute_batch(&format!("ROLLBACK TO \"{0}\"; RELEASE \"{0}\"", self.name));
        }
    }
}

const APP_ID: i64 = 1129794866;
const SCHEMA: &str = include_str!("sql/schema.sql");
const ELIGIBLE: &str = include_str!("sql/eligible.sql");
const SELECT: &str = "m.id,m.revision,m.status,m.payload,m.created_at,m.updated_at,m.content_hash";

/// Vendor-neutral lifecycle seam. Transport adapters must preserve authorization,
/// revision checks, freshness, correction and forgetting, not just store/search.
pub trait MemoryBackend: Send {
    fn capture(&mut self, access: &Access, request: &str, draft: Draft) -> Result<CaptureReceipt>;
    fn get(&self, access: &Access, id: &str) -> Result<Memory>;
    fn recall(&self, access: &Access, query: &Recall) -> Result<RecallReport>;
    fn approve(
        &mut self,
        access: &Access,
        id: &str,
        revision: i64,
        validation: Option<&ValidationReceipt>,
        snapshot: &Snapshot,
    ) -> Result<Memory>;
    fn reject(&mut self, access: &Access, id: &str, revision: i64) -> Result<Memory>;
    fn supersede(
        &mut self,
        access: &Access,
        old: (&str, i64),
        new: (&str, i64),
        validation: Option<&ValidationReceipt>,
        snapshot: &Snapshot,
    ) -> Result<Memory>;
    fn forget(&mut self, access: &Access, id: &str, revision: i64) -> Result<ForgetReport>;
    fn save_checkpoint(
        &mut self,
        access: &Access,
        draft: CheckpointDraft,
        expected_revision: Option<i64>,
        snapshot: &Snapshot,
    ) -> Result<Checkpoint>;
    fn resume(
        &self,
        access: &Access,
        scope: &Scope,
        key: &str,
        snapshot: &Snapshot,
    ) -> Result<Resume>;
}

pub struct Store {
    pub(crate) conn: Connection,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn row_memory(row: &Row<'_>) -> rusqlite::Result<Memory> {
    let json: String = row.get(3)?;
    let status: String = row.get(2)?;
    let decode = |e: Box<dyn std::error::Error + Send + Sync>| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, e)
    };
    Ok(Memory {
        id: row.get(0)?,
        revision: row.get(1)?,
        status: Status::parse(&status).map_err(|e| decode(Box::new(e)))?,
        draft: serde_json::from_str(&json).map_err(|e| decode(Box::new(e)))?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        content_hash: row.get(6)?,
    })
}
fn event(
    conn: &Connection,
    access: &Access,
    id: &str,
    action: &str,
    revision: i64,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO events(entity_id,action,actor_hash,revision,at) VALUES(?1,?2,?3,?4,?5)",
        params![id, action, access.actor_hash(), revision, now],
    )?;
    Ok(())
}
fn get_at(conn: &Connection, access: &Access, id: &str) -> Result<Memory> {
    access.require(Capability::Read)?;
    let sql = format!(
        "SELECT {SELECT} FROM memories m WHERE m.id=?1 AND m.scope IN (SELECT value FROM json_each(?2))"
    );
    conn.query_row(&sql, params![id, access.scope_json()?], row_memory)
        .optional()?
        .ok_or(Error::NotFound)
}
pub(crate) fn effective_freshness(
    conn: &Connection,
    access: &Access,
    memory: &Memory,
    snapshot: &Snapshot,
    now: i64,
) -> Result<Freshness> {
    let own = memory.freshness(snapshot, now);
    if own != Freshness::Current {
        return Ok(own);
    }
    let mut pending = memory.draft.parent_ids.clone();
    let mut seen = std::collections::BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if seen.len() > 4096 {
            return Ok(Freshness::Unknown);
        }
        let parent = match get_at(conn, access, &id) {
            Ok(m) => m,
            Err(Error::NotFound) => return Ok(Freshness::Unknown),
            Err(e) => return Err(e),
        };
        let freshness = parent.freshness(snapshot, now);
        if freshness != Freshness::Current {
            return Ok(if freshness == Freshness::Inactive {
                Freshness::Changed
            } else {
                freshness
            });
        }
        pending.extend(parent.draft.parent_ids);
    }
    Ok(Freshness::Current)
}
fn require_revision(m: &Memory, expected: i64) -> Result<()> {
    if m.revision == expected {
        Ok(())
    } else {
        Err(Error::RevisionConflict)
    }
}
fn ensure_key_available(
    conn: &Connection,
    access: &Access,
    draft: &Draft,
    except: Option<&str>,
    now: i64,
) -> Result<()> {
    if let Some(key) = &draft.key {
        let old: Option<(String,i64,Option<i64>)> = conn.query_row(
            "SELECT id,revision,expires_at FROM memories WHERE scope=?1 AND semantic_key=?2 AND status='active'",
            params![draft.scope.key()?,key],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((id, rev, expiry)) = old {
            if Some(id.as_str()) == except {
                return Ok(());
            }
            if expiry.is_some_and(|t| t <= now) {
                conn.execute("UPDATE memories SET status='stale',revision=revision+1,updated_at=?2 WHERE id=?1",params![id,now])?;
                event(conn, access, &id, "expired", rev + 1, now)?;
                invalidate_descendants(conn, access, &id, now)?;
            } else {
                return Err(Error::KeyConflict);
            }
        }
    }
    Ok(())
}
fn descendants(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let mut q = conn.prepare("WITH RECURSIVE children(id) AS (SELECT child_id FROM lineage WHERE parent_id=?1 UNION SELECT l.child_id FROM lineage l JOIN children c ON l.parent_id=c.id) SELECT id FROM children ORDER BY id")?;
    Ok(q.query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
fn invalidate_descendants(conn: &Connection, access: &Access, id: &str, now: i64) -> Result<()> {
    for child in descendants(conn, id)? {
        let m = get_at(conn, access, &child)?;
        if m.status == Status::Active {
            conn.execute(
                "UPDATE memories SET status='stale',revision=revision+1,updated_at=?2 WHERE id=?1",
                params![child, now],
            )?;
            event(
                conn,
                access,
                &child,
                "parent_invalidated",
                m.revision + 1,
                now,
            )?;
        }
    }
    Ok(())
}
fn check_activation(
    conn: &Connection,
    access: &Access,
    m: &Memory,
    receipt: Option<&ValidationReceipt>,
    snapshot: &Snapshot,
    now: i64,
) -> Result<()> {
    if m.status != Status::Candidate {
        return Err(Error::InvalidState);
    }
    let mut projected = m.clone();
    projected.status = Status::Active;
    if effective_freshness(conn, access, &projected, snapshot, now)? != Freshness::Current {
        return Err(Error::InvalidState);
    }
    for parent in &m.draft.parent_ids {
        let p = get_at(conn, access, parent)?;
        if p.status != Status::Active
            || effective_freshness(conn, access, &p, snapshot, now)? != Freshness::Current
        {
            return Err(Error::InvalidParent);
        }
    }
    if matches!(m.draft.kind, Kind::Procedure | Kind::Lesson) && receipt.is_none() {
        return Err(Error::ValidationRequired);
    }
    if let Some(receipt) = receipt {
        if !receipt.passed || receipt.content_hash != m.content_hash {
            return Err(Error::ValidationRequired);
        }
        policy::bounded(&receipt.validator, "validator", 256, true)?;
        policy::bounded(&receipt.evidence_uri, "validation evidence URI", 1024, true)?;
        policy::ensure_no_secret(&serde_json::to_string(receipt)?)?;
    }
    Ok(())
}
fn activate(
    conn: &Connection,
    access: &Access,
    m: &Memory,
    receipt: Option<&ValidationReceipt>,
    now: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE memories SET status='active',revision=revision+1,updated_at=?2 WHERE id=?1",
        params![m.id, now],
    )?;
    if let Some(r) = receipt {
        conn.execute(
            "INSERT INTO validations(memory_id,receipt) VALUES(?1,?2)",
            params![m.id, serde_json::to_string(r)?],
        )?;
    }
    event(conn, access, &m.id, "approved", m.revision + 1, now)?;
    Ok(())
}
impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
            && !parent.exists()
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(parent)?;
            }
            #[cfg(not(unix))]
            fs::create_dir_all(parent)?;
        }
        // Explicit local file security. No URI filenames and no extension loading.
        match fs::symlink_metadata(path) {
            Ok(meta) => {
                if meta.file_type().is_symlink() || !meta.is_file() {
                    return Err(Error::Invalid(
                        "database must be a regular, non-symlink file".into(),
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if meta.permissions().mode() & 0o077 != 0 && !mount_ignores_unix_modes(path) {
                        return Err(Error::Invalid(
                            "database permissions must exclude group and world access".into(),
                        ));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut opts = fs::OpenOptions::new();
                opts.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.mode(0o600);
                }
                match opts.open(path) {
                    Ok(_) => (),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        return Self::open(path);
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            Err(e) => return Err(e.into()),
        }
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Self::from_connection(conn, Arc::new(now))
    }
    pub fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?, Arc::new(now))
    }
    pub fn in_memory_with_clock(clock: impl Fn() -> i64 + Send + Sync + 'static) -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?, Arc::new(clock))
    }
    fn from_connection(
        mut conn: Connection,
        clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> Result<Self> {
        let app: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let count: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )?;
        if !((app == 0 && version == 0 && count == 0)
            || (app == APP_ID && (1..=2).contains(&version)))
        {
            return Err(Error::DatabaseMismatch);
        }
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON; PRAGMA temp_store=MEMORY; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
        if version < 2 {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let v: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            if v == 0 {
                tx.execute_batch(SCHEMA)?;
            } else if !(1..=2).contains(&v) {
                return Err(Error::DatabaseMismatch);
            }
            if v < 2 {
                tx.execute_batch(include_str!("sql/migrate_002.sql"))?;
            }
            tx.commit()?;
        }
        Ok(Self { conn, clock })
    }
    pub fn timestamp(&self) -> i64 {
        (self.clock)()
    }
    /// Validate the complete provenance chain, not just this record's own hashes.
    pub fn freshness(
        &self,
        access: &Access,
        memory: &Memory,
        snapshot: &Snapshot,
    ) -> Result<Freshness> {
        access.read(&memory.draft.scope)?;
        effective_freshness(&self.conn, access, memory, snapshot, self.timestamp())
    }
    pub fn list(&self, access: &Access, after: Option<&str>, limit: usize) -> Result<Vec<Memory>> {
        access.require(Capability::Read)?;
        let sql = format!(
            "SELECT {SELECT} FROM memories m WHERE m.scope IN (SELECT value FROM json_each(?1)) AND m.id>?2 ORDER BY m.id LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        Ok(stmt
            .query_map(
                params![
                    access.scope_json()?,
                    after.unwrap_or(""),
                    limit.clamp(1, 500) as i64
                ],
                row_memory,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn dependency_paths(&self, access: &Access) -> Result<Vec<String>> {
        access.require(Capability::Read)?;
        let mut q = self.conn.prepare("SELECT DISTINCT d.path FROM dependencies d JOIN memories m ON m.id=d.memory_id WHERE m.scope IN (SELECT value FROM json_each(?1)) ORDER BY d.path LIMIT 4096")?;
        Ok(q.query_map([access.scope_json()?], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn status(&self, access: &Access) -> Result<serde_json::Value> {
        access.require(Capability::Read)?;
        let mut q = self.conn.prepare("SELECT status,count(*) FROM memories WHERE scope IN (SELECT value FROM json_each(?1)) GROUP BY status")?;
        let counts = q
            .query_map([access.scope_json()?], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
        Ok(
            serde_json::json!({"schema_version":2,"backend":"sqlite-local-v2","counts":counts,"network_calls":false,"encrypted_at_rest":false}),
        )
    }
    pub fn link(
        &mut self,
        access: &Access,
        from: &str,
        to: &str,
        relation: Relation,
    ) -> Result<()> {
        let now = self.timestamp();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let a = get_at(&tx, access, from)?;
        let b = get_at(&tx, access, to)?;
        access.write(&a.draft.scope, Capability::Link)?;
        access.write(&b.draft.scope, Capability::Link)?;
        if a.draft.scope != b.draft.scope || from == to {
            return Err(Error::Invalid(
                "relations must join distinct memories in one exact scope".into(),
            ));
        }
        if tx.execute(
            "INSERT OR IGNORE INTO links(from_id,to_id,relation) VALUES(?1,?2,?3)",
            params![from, to, relation.as_str()],
        )? > 0
        {
            event(&tx, access, from, "linked", a.revision, now)?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn set_embedding(
        &mut self,
        access: &Access,
        id: &str,
        expected_hash: &str,
        embedding: &Embedding,
    ) -> Result<()> {
        let normalized = policy::normalize_embedding(embedding)?;
        let bytes: Vec<u8> = normalized.iter().flat_map(|v| v.to_le_bytes()).collect();
        let now = self.timestamp();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let m = get_at(&tx, access, id)?;
        access.write(&m.draft.scope, Capability::Index)?;
        if m.content_hash != expected_hash {
            return Err(Error::RevisionConflict);
        }
        tx.execute("INSERT INTO embeddings(memory_id,model,dimensions,content_hash,vector) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(memory_id) DO UPDATE SET model=excluded.model,dimensions=excluded.dimensions,content_hash=excluded.content_hash,vector=excluded.vector",params![id,embedding.model,normalized.len() as i64,expected_hash,bytes])?;
        event(&tx, access, id, "embedded", m.revision, now)?;
        tx.commit()?;
        Ok(())
    }
    pub fn reindex(&mut self, access: &Access) -> Result<()> {
        access.require(Capability::Maintenance)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch("INSERT INTO memory_fts(memory_fts) VALUES('rebuild'); INSERT INTO memory_grams(memory_grams) VALUES('rebuild'); INSERT INTO memory_fts(memory_fts,rank) VALUES('integrity-check',1); INSERT INTO memory_grams(memory_grams,rank) VALUES('integrity-check',1);")?;
        tx.commit()?;
        Ok(())
    }
    pub fn export_jsonl(&self, access: &Access, mut output: impl Write) -> Result<usize> {
        access.require(Capability::Export)?;
        let _read_snapshot = Tx::begin(&self.conn)?;
        let mut count = 0;
        let mut after = None;
        loop {
            let batch = self.list(access, after.as_deref(), 500)?;
            if batch.is_empty() {
                break;
            }
            for memory in &batch {
                serde_json::to_writer(
                    &mut output,
                    &serde_json::json!({"schema":"codewhale.memory.export.v1","memory":memory}),
                )?;
                output.write_all(b"\n")?;
                count += 1;
            }
            after = batch.last().map(|m| m.id.clone());
        }
        Ok(count)
    }
    /// Mark active expired records and their dependents stale. Expiry is already
    /// enforced in recall; correctness does not depend on scheduling this sweep.
    pub fn expire(&mut self, access: &Access) -> Result<usize> {
        access.require(Capability::Maintenance)?;
        let now = self.timestamp();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids: Vec<String> = {
            let mut q = tx.prepare("SELECT id FROM memories WHERE scope IN (SELECT value FROM json_each(?1)) AND status='active' AND expires_at<=?2")?;
            q.query_map(params![access.scope_json()?, now], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for id in &ids {
            let m = get_at(&tx, access, id)?;
            access.write(&m.draft.scope, Capability::Maintenance)?;
            tx.execute(
                "UPDATE memories SET status='stale',revision=revision+1,updated_at=?2 WHERE id=?1",
                params![id, now],
            )?;
            invalidate_descendants(&tx, access, id, now)?;
            event(&tx, access, id, "expired", m.revision + 1, now)?;
        }
        tx.commit()?;
        Ok(ids.len())
    }
}

impl MemoryBackend for Store {
    fn capture(
        &mut self,
        access: &Access,
        request: &str,
        mut draft: Draft,
    ) -> Result<CaptureReceipt> {
        access.write(&draft.scope, Capability::Propose)?;
        policy::bounded(request, "request id", 1024, true)?;
        let now = self.timestamp();
        draft.title = draft.title.trim().to_owned();
        draft.body = draft.body.trim().to_owned();
        draft.tags.sort();
        draft.tags.dedup();
        draft.parent_ids.sort();
        draft.parent_ids.dedup();
        policy::validate_draft(&draft, now)?;
        let scope = draft.scope.key()?;
        let request_hash = policy::sha256(request.as_bytes());
        let request_draft_hash = policy::sha256(&serde_json::to_vec(&draft)?);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT draft_hash,memory_id FROM requests WHERE scope=?1 AND request_hash=?2",
                params![scope, request_hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((old_hash, id)) = existing {
            if old_hash != request_draft_hash {
                return Err(Error::IdempotencyConflict);
            }
            let id = id.ok_or(Error::Forgotten)?;
            let memory = get_at(&tx, access, &id)?;
            return Ok(CaptureReceipt {
                memory,
                created: false,
            });
        }
        let content_hash = policy::content_hash(&draft)?;
        let forgotten: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tombstones WHERE scope=?1 AND content_hash=?2)",
            params![scope, content_hash],
            |r| r.get(0),
        )?;
        if forgotten {
            return Err(Error::Forgotten);
        }
        for parent in &draft.parent_ids {
            let p = get_at(&tx, access, parent)?;
            if p.draft.scope != draft.scope {
                return Err(Error::Invalid(
                    "derived memories cannot cross scope boundaries".into(),
                ));
            }
            if let Some(expiry) = p.draft.expires_at {
                draft.expires_at = Some(draft.expires_at.map_or(expiry, |e| e.min(expiry)));
            }
            // Inherit dependency fingerprints, not merely prose. Otherwise a
            // summary could remain "fresh" after its supporting file changed.
            for (path, digest) in &p.draft.dependencies {
                if draft.dependencies.get(path).is_some_and(|h| h != digest) {
                    return Err(Error::InvalidParent);
                }
                draft.dependencies.insert(path.clone(), digest.clone());
            }
            if draft.repository_revision.is_none() {
                draft.repository_revision = p.draft.repository_revision.clone();
            } else if p.draft.dependencies.is_empty()
                && p.draft.repository_revision.is_some()
                && p.draft.repository_revision != draft.repository_revision
            {
                return Err(Error::InvalidParent);
            }
        }
        if draft.expires_at.is_some_and(|t| t <= now) {
            return Err(Error::InvalidParent);
        }
        policy::validate_draft(&draft, now)?;
        let id = Uuid::new_v4().to_string();
        tx.execute("INSERT INTO memories(id,scope,kind,semantic_key,status,revision,title,body,tags,payload,content_hash,repo_revision,importance,confidence,created_at,updated_at,expires_at) VALUES(?1,?2,?3,?4,'candidate',1,?5,?6,?7,?8,?9,?10,?11,?12,?13,?13,?14)",
            params![id,scope,draft.kind.as_str(),draft.key,draft.title,draft.body,draft.tags.join(" "),serde_json::to_string(&draft)?,content_hash,draft.repository_revision,draft.importance,draft.confidence,now,draft.expires_at])?;
        for (path, digest) in &draft.dependencies {
            tx.execute(
                "INSERT INTO dependencies(memory_id,path,digest) VALUES(?1,?2,?3)",
                params![id, path, digest],
            )?;
        }
        for parent in &draft.parent_ids {
            tx.execute(
                "INSERT INTO lineage(parent_id,child_id) VALUES(?1,?2)",
                params![parent, id],
            )?;
        }
        tx.execute(
            "INSERT INTO requests(scope,request_hash,draft_hash,memory_id) VALUES(?1,?2,?3,?4)",
            params![scope, request_hash, request_draft_hash, id],
        )?;
        event(&tx, access, &id, "proposed", 1, now)?;
        let memory = get_at(&tx, access, &id)?;
        tx.commit()?;
        Ok(CaptureReceipt {
            memory,
            created: true,
        })
    }
    fn get(&self, access: &Access, id: &str) -> Result<Memory> {
        get_at(&self.conn, access, id)
    }
    fn approve(
        &mut self,
        access: &Access,
        id: &str,
        revision: i64,
        validation: Option<&ValidationReceipt>,
        snapshot: &Snapshot,
    ) -> Result<Memory> {
        let now = self.timestamp();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let m = get_at(&tx, access, id)?;
        access.write(&m.draft.scope, Capability::Review)?;
        require_revision(&m, revision)?;
        check_activation(&tx, access, &m, validation, snapshot, now)?;
        ensure_key_available(&tx, access, &m.draft, None, now)?;
        activate(&tx, access, &m, validation, now)?;
        let updated = get_at(&tx, access, id)?;
        tx.commit()?;
        Ok(updated)
    }
    fn reject(&mut self, access: &Access, id: &str, revision: i64) -> Result<Memory> {
        let now = self.timestamp();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let m = get_at(&tx, access, id)?;
        access.write(&m.draft.scope, Capability::Review)?;
        require_revision(&m, revision)?;
        if m.status != Status::Candidate {
            return Err(Error::InvalidState);
        }
        tx.execute(
            "UPDATE memories SET status='rejected',revision=revision+1,updated_at=?2 WHERE id=?1",
            params![id, now],
        )?;
        event(&tx, access, id, "rejected", m.revision + 1, now)?;
        let updated = get_at(&tx, access, id)?;
        tx.commit()?;
        Ok(updated)
    }
    fn supersede(
        &mut self,
        access: &Access,
        old: (&str, i64),
        new: (&str, i64),
        validation: Option<&ValidationReceipt>,
        snapshot: &Snapshot,
    ) -> Result<Memory> {
        let now = self.timestamp();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let a = get_at(&tx, access, old.0)?;
        let b = get_at(&tx, access, new.0)?;
        access.write(&a.draft.scope, Capability::Correct)?;
        access.write(&b.draft.scope, Capability::Review)?;
        require_revision(&a, old.1)?;
        require_revision(&b, new.1)?;
        if old.0 == new.0
            || a.draft.scope != b.draft.scope
            || a.draft.key != b.draft.key
            || a.draft.kind != b.draft.kind
            || !matches!(a.status, Status::Active | Status::Stale)
        {
            return Err(Error::InvalidState);
        }
        if descendants(&tx, old.0)?.iter().any(|id| id == new.0) {
            return Err(Error::Invalid(
                "a correction cannot depend on the memory it invalidates".into(),
            ));
        }
        check_activation(&tx, access, &b, validation, snapshot, now)?;
        ensure_key_available(&tx, access, &b.draft, Some(old.0), now)?;
        tx.execute(
            "UPDATE memories SET status='superseded',revision=revision+1,updated_at=?2 WHERE id=?1",
            params![old.0, now],
        )?;
        invalidate_descendants(&tx, access, old.0, now)?;
        tx.execute(
            "INSERT INTO replacements(old_id,new_id) VALUES(?1,?2)",
            params![old.0, new.0],
        )?;
        activate(&tx, access, &b, validation, now)?;
        event(&tx, access, old.0, "superseded", a.revision + 1, now)?;
        let updated = get_at(&tx, access, new.0)?;
        tx.commit()?;
        Ok(updated)
    }
    fn forget(&mut self, access: &Access, id: &str, revision: i64) -> Result<ForgetReport> {
        let now = self.timestamp();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let root = get_at(&tx, access, id)?;
        access.write(&root.draft.scope, Capability::Forget)?;
        require_revision(&root, revision)?;
        let ids: Vec<String> = {
            let mut q = tx.prepare(include_str!("sql/forget_closure.sql"))?;
            q.query_map(named_params! {":id":id}, |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for id in &ids {
            let m = get_at(&tx, access, id)?;
            access.write(&m.draft.scope, Capability::Forget)?;
            tx.execute("INSERT OR IGNORE INTO tombstones(scope,content_hash,forgotten_at) VALUES(?1,?2,?3)",params![m.draft.scope.key()?,m.content_hash,now])?;
            event(&tx, access, id, "forgotten", m.revision + 1, now)?;
        }
        let ids_json = serde_json::to_string(&ids)?;
        // A cited memory retracts dependent checkpoints, including their summaries.
        // Scope-crossing checkpoint references are same-tenant/user only (Access).
        let checkpoints_deleted=tx.execute("DELETE FROM checkpoints WHERE id IN (SELECT checkpoint_id FROM checkpoint_refs WHERE memory_id IN (SELECT value FROM json_each(?1)))",[&ids_json])?;
        let memories_deleted = tx.execute(
            "DELETE FROM memories WHERE id IN (SELECT value FROM json_each(?1))",
            [&ids_json],
        )?;
        tx.commit()?;
        // A busy reader may retain the WAL. Logical deletion still succeeded;
        // report that condition instead of claiming physical erasure or failing
        // the already-committed operation.
        let wal_truncated = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|busy| busy == 0)
            .unwrap_or(false);
        Ok(ForgetReport {
            memories_deleted,
            checkpoints_deleted,
            wal_truncated,
            physical_erasure_guaranteed: false,
        })
    }
    fn recall(&self, access: &Access, query: &Recall) -> Result<RecallReport> {
        self.recall_impl(access, query)
    }
    fn save_checkpoint(
        &mut self,
        access: &Access,
        mut draft: CheckpointDraft,
        expected_revision: Option<i64>,
        snapshot: &Snapshot,
    ) -> Result<Checkpoint> {
        access.write(&draft.scope, Capability::Checkpoint)?;
        if draft.scope.session.is_none() {
            return Err(Error::Invalid(
                "working state requires a session scope".into(),
            ));
        }
        policy::bounded(&draft.key, "checkpoint key", 256, true)?;
        policy::bounded(&draft.state.summary, "checkpoint summary", 8192, false)?;
        if draft.memory_ids.len() > 128
            || draft.state.next_steps.len() > 64
            || draft.state.artifact_refs.len() > 64
            || draft.state.pending_operations.len() > 64
        {
            return Err(Error::Invalid(
                "checkpoint collection limit exceeded".into(),
            ));
        }
        for item in draft
            .state
            .next_steps
            .iter()
            .chain(draft.state.artifact_refs.iter())
        {
            policy::bounded(item, "checkpoint item", 1024, true)?;
        }
        for op in &draft.state.pending_operations {
            policy::bounded(&op.operation_id, "operation id", 256, true)?;
            policy::bounded(&op.state, "operation state", 128, true)?;
        }
        let encoded = serde_json::to_string(&draft)?;
        policy::bounded(&encoded, "checkpoint", 32768, false)?;
        policy::ensure_no_secret(&encoded)?;
        draft.memory_ids.sort();
        draft.memory_ids.dedup();
        let now = self.timestamp();
        let expires = draft.expires_at.unwrap_or(now.saturating_add(7 * 86400));
        if expires <= now {
            return Err(Error::Invalid(
                "checkpoint expiry must be in the future".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old: Option<(String, i64)> = tx
            .query_row(
                "SELECT id,revision FROM checkpoints WHERE scope=?1 AND checkpoint_key=?2",
                params![draft.scope.key()?, draft.key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (id, revision) = match old {
            None if expected_revision.is_none() => (Uuid::new_v4().to_string(), 1),
            Some((id, rev)) if expected_revision == Some(rev) => (id, rev + 1),
            _ => return Err(Error::RevisionConflict),
        };
        let mut memories = Vec::new();
        for mid in &draft.memory_ids {
            let m = get_at(&tx, access, mid)?;
            if effective_freshness(&tx, access, &m, snapshot, now)? != Freshness::Current {
                return Err(Error::InvalidParent);
            }
            memories.push(MemoryRef {
                id: m.id,
                revision: m.revision,
                content_hash: m.content_hash,
            });
        }
        tx.execute("INSERT INTO checkpoints(id,scope,checkpoint_key,revision,payload,updated_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(scope,checkpoint_key) DO UPDATE SET revision=excluded.revision,payload=excluded.payload,updated_at=excluded.updated_at,expires_at=excluded.expires_at",params![id,draft.scope.key()?,draft.key,revision,serde_json::to_string(&draft)?,now,expires])?;
        tx.execute("DELETE FROM checkpoint_refs WHERE checkpoint_id=?1", [&id])?;
        for m in &memories {
            tx.execute("INSERT INTO checkpoint_refs(checkpoint_id,memory_id,revision,content_hash) VALUES(?1,?2,?3,?4)",params![id,m.id,m.revision,m.content_hash])?;
        }
        event(&tx, access, &id, "checkpoint_saved", revision, now)?;
        tx.commit()?;
        Ok(Checkpoint {
            id,
            revision,
            draft,
            memories,
            updated_at: now,
            expires_at: expires,
        })
    }
    fn resume(
        &self,
        access: &Access,
        scope: &Scope,
        key: &str,
        snapshot: &Snapshot,
    ) -> Result<Resume> {
        access.read(scope)?;
        let _read_snapshot = Tx::begin(&self.conn)?;
        let row: Option<(String,i64,String,i64,i64)>=self.conn.query_row("SELECT id,revision,payload,updated_at,expires_at FROM checkpoints WHERE scope=?1 AND checkpoint_key=?2 AND expires_at>?3",params![scope.key()?,key,self.timestamp()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        let (id, revision, payload, updated_at, expires_at) = row.ok_or(Error::NotFound)?;
        let mut q=self.conn.prepare("SELECT memory_id,revision,content_hash FROM checkpoint_refs WHERE checkpoint_id=?1 ORDER BY memory_id")?;
        let memories = q
            .query_map([&id], |r| {
                Ok(MemoryRef {
                    id: r.get(0)?,
                    revision: r.get(1)?,
                    content_hash: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut invalidated = Vec::new();
        for reference in &memories {
            match self.get(access, &reference.id) {
                Ok(m) => {
                    let current = m.revision == reference.revision
                        && m.content_hash == reference.content_hash
                        && effective_freshness(&self.conn, access, &m, snapshot, self.timestamp())?
                            == Freshness::Current;
                    if !current {
                        invalidated.push(reference.id.clone());
                    }
                }
                Err(Error::NotFound) => invalidated.push(reference.id.clone()),
                Err(error) => return Err(error),
            }
        }
        Ok(Resume {
            checkpoint: Checkpoint {
                id,
                revision,
                draft: serde_json::from_str(&payload)?,
                memories,
                updated_at,
                expires_at,
            },
            invalidated_memory_ids: invalidated,
            reconcile_pending_operations: true,
        })
    }
}

fn add_ranking(
    target: &mut BTreeMap<String, Hit>,
    rows: Vec<Memory>,
    reason: &str,
    weight: f64,
    snapshot: &Snapshot,
    now: i64,
) {
    for (rank, memory) in rows.into_iter().enumerate() {
        let freshness = memory.freshness(snapshot, now);
        let score = weight / (60.0 + rank as f64 + 1.0);
        let hit = target.entry(memory.id.clone()).or_insert(Hit {
            memory,
            freshness,
            score: 0.0,
            reasons: Vec::new(),
        });
        hit.score += score;
        hit.reasons.push(reason.to_string());
    }
}
impl Store {
    fn recall_impl(&self, access: &Access, query: &Recall) -> Result<RecallReport> {
        access.require(Capability::Read)?;
        policy::bounded(&query.query, "query", 1024, false)?;
        let scopes = access.scope_json()?;
        let files = serde_json::to_string(&query.snapshot.files)?;
        let now = self.timestamp();
        let limit = query.limit.clamp(1, 64);
        let candidates = (limit * 8).clamp(64, 512) as i64;
        let include_stale = if query.include_stale { 1_i64 } else { 0_i64 };
        // All retrieval branches share a consistent read transaction. A later
        // use still needs a revision/freshness check at the host execution boundary.
        let _read_snapshot = Tx::begin(&self.conn)?;
        let mut hits = BTreeMap::new();
        let run_text = |sql: &str, text: &str| -> Result<Vec<Memory>> {
            let mut q = self.conn.prepare(sql)?;
            Ok(q.query_map(named_params!{":scopes":scopes,":now":now,":include_stale":include_stale,":files":files,":repo":query.snapshot.revision,":query":text,":limit":candidates},row_memory)?.collect::<rusqlite::Result<Vec<_>>>()?)
        };
        if let Some(fts) = policy::fts_query(&query.query)? {
            let sql = format!(
                "SELECT {SELECT} FROM memory_fts JOIN memories m ON m.rowid=memory_fts.rowid WHERE ({ELIGIBLE}) AND memory_fts MATCH :query ORDER BY bm25(memory_fts,3.0,1.0,2.0),m.id LIMIT :limit"
            );
            add_ranking(
                &mut hits,
                run_text(&sql, &fts)?,
                "lexical",
                1.0,
                &query.snapshot,
                now,
            );
            let chars = query.query.trim().chars().count();
            if (3..=256).contains(&chars) {
                let sql = format!(
                    "SELECT {SELECT} FROM memory_grams JOIN memories m ON m.rowid=memory_grams.rowid WHERE ({ELIGIBLE}) AND memory_grams MATCH :query ORDER BY bm25(memory_grams,2.0,1.0),m.id LIMIT :limit"
                );
                let literal = format!("\"{}\"", query.query.trim().replace('"', "\"\""));
                add_ranking(
                    &mut hits,
                    run_text(&sql, &literal)?,
                    "substring",
                    0.6,
                    &query.snapshot,
                    now,
                );
            } else if chars <= 2 {
                let sql = format!(
                    "SELECT {SELECT} FROM memories m WHERE ({ELIGIBLE}) AND instr(lower(m.title || ' ' || m.body),lower(:query))>0 ORDER BY m.importance DESC,m.id LIMIT :limit"
                );
                add_ranking(
                    &mut hits,
                    run_text(&sql, query.query.trim())?,
                    "short_substring",
                    0.5,
                    &query.snapshot,
                    now,
                );
            }
        } else if query.query.trim().is_empty() && query.embedding.is_none() {
            let sql = format!(
                "SELECT {SELECT} FROM memories m WHERE ({ELIGIBLE}) AND :query='' ORDER BY m.importance DESC,m.updated_at DESC,m.id LIMIT :limit"
            );
            add_ranking(
                &mut hits,
                run_text(&sql, "")?,
                "working_set",
                1.0,
                &query.snapshot,
                now,
            );
        }
        let mut vector_candidates = 0;
        let mut vector_scan_truncated = false;
        if let Some(embedding) = &query.embedding {
            let normalized = policy::normalize_embedding(embedding)?;
            let scan_limit = query.vector_scan_limit.clamp(1, 20_000);
            let sql = format!(
                "SELECT {SELECT},e.vector FROM embeddings e JOIN memories m ON m.id=e.memory_id WHERE ({ELIGIBLE}) AND e.model=:model AND e.dimensions=:dimensions AND e.content_hash=m.content_hash ORDER BY m.id LIMIT :limit"
            );
            let mut q = self.conn.prepare(&sql)?;
            let rows=q.query_map(named_params!{":scopes":scopes,":now":now,":include_stale":include_stale,":files":files,":repo":query.snapshot.revision,":model":embedding.model,":dimensions":normalized.len() as i64,":limit":(scan_limit+1) as i64},|r|Ok((row_memory(r)?,r.get::<_,Vec<u8>>(7)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            vector_scan_truncated = rows.len() > scan_limit;
            let mut dense = Vec::new();
            for (m, bytes) in rows.into_iter().take(scan_limit) {
                vector_candidates += 1;
                if bytes.len() != normalized.len() * 4 {
                    return Err(Error::Invalid("stored embedding length mismatch".into()));
                }
                let mut cosine = 0.0_f64;
                for (chunk, q) in bytes.as_chunks::<4>().0.iter().zip(&normalized) {
                    let v = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    if !v.is_finite() {
                        return Err(Error::Invalid("stored embedding is non-finite".into()));
                    }
                    cosine += v as f64 * *q as f64;
                }
                if cosine > 0.0 {
                    dense.push((m, cosine));
                }
            }
            dense.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.id.cmp(&b.0.id)));
            add_ranking(
                &mut hits,
                dense
                    .into_iter()
                    .take(candidates as usize)
                    .map(|(m, _)| m)
                    .collect(),
                "semantic",
                1.0,
                &query.snapshot,
                now,
            );
        }
        if query.expand_graph && !hits.is_empty() {
            let mut ordered: Vec<_> = hits.values().collect();
            ordered.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then_with(|| a.memory.id.cmp(&b.memory.id))
            });
            let seeds = serde_json::to_string(
                &ordered
                    .iter()
                    .take(8)
                    .map(|h| &h.memory.id)
                    .collect::<Vec<_>>(),
            )?;
            let sql = format!(
                "SELECT DISTINCT {SELECT} FROM memories m JOIN links l ON ((l.from_id IN (SELECT value FROM json_each(:seeds)) AND l.to_id=m.id) OR (l.to_id IN (SELECT value FROM json_each(:seeds)) AND l.from_id=m.id)) WHERE ({ELIGIBLE}) ORDER BY m.id LIMIT 32"
            );
            let mut q = self.conn.prepare(&sql)?;
            let rows=q.query_map(named_params!{":scopes":scopes,":now":now,":include_stale":include_stale,":files":files,":repo":query.snapshot.revision,":seeds":seeds},row_memory)?.collect::<rusqlite::Result<Vec<_>>>()?;
            add_ranking(
                &mut hits,
                rows,
                "graph_neighbor",
                0.25,
                &query.snapshot,
                now,
            );
        }
        let mut hits: Vec<_> = hits.into_values().collect();
        for hit in &mut hits {
            hit.freshness =
                effective_freshness(&self.conn, access, &hit.memory, &query.snapshot, now)?;
            // Small, explainable tie-breakers. Retrieval never writes reinforcement
            // or turns model confidence into a probability of correctness.
            hit.score += 0.002 * hit.memory.draft.importance + 0.0005 * hit.memory.draft.confidence;
        }
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.memory.id.cmp(&b.memory.id))
        });
        hits.truncate(limit);
        Ok(RecallReport {
            hits,
            vector_candidates,
            vector_scan_truncated,
        })
    }
}
