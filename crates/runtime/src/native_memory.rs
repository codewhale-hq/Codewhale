//! Structured replacement for the old Markdown-backed native memory store.
//! All existing entry points share root/store.sqlite3 with the Context Lens.
//! MEMORY.md paths are legacy anchors, NOT an alternative source of truth.
//! Capture is candidate-only. Human review is through authenticated Lens routes.
use anyhow::{Result, anyhow, bail};
use codewhale_memory::hooks::{self, Boundary, HookEvent, HookPolicy};
use codewhale_memory::{
    Access, ByteCounter, ContextBudget, Draft, Evidence, Freshness, Memory, MemoryBackend, Recall,
    Scope, Snapshot, SourceKind, Status, Store, policy, workspace,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryScope {
    Global,
    Workspace,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryHit {
    pub id: i64,
    pub text: String,
    pub source: PathBuf,
    pub line_start: usize,
    pub line_end: usize,
    pub stale: bool,
}
#[derive(Debug, Clone)]
pub struct NativeMemoryStore {
    root: PathBuf,
}
impl NativeMemoryStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn global_path(&self) -> PathBuf {
        self.root.join("global/MEMORY.md")
    }
    pub fn index_path(&self) -> PathBuf {
        self.root.join("store.sqlite3")
    }
    pub fn from_global_path(path: &Path) -> Option<Self> {
        if path.file_name()?.to_str()? != "MEMORY.md"
            || path.parent()?.file_name()?.to_str()? != "global"
        {
            return None;
        }
        let root = path.parent()?.parent()?;
        (root.file_name()?.to_str()? == "memory").then(|| Self::new(root))
    }
    pub fn from_memory_anchor(path: &Path) -> Self {
        Self::from_global_path(path)
            .unwrap_or_else(|| Self::new(path.parent().unwrap_or(Path::new(".")).join("memory")))
    }
    pub fn owner_scope() -> Scope {
        Scope::user("local", "owner")
    }
    pub fn workspace_scope(id: &str) -> Result<Scope> {
        safe_component(id)?;
        Ok(Self::owner_scope().workspace(id))
    }
    pub fn workspace_id(workspace: &Path) -> Result<Option<String>> {
        let out = Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(["config", "--get", "remote.origin.url"])
            .output()?;
        if out.status.success() {
            let value = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if !value.is_empty() {
                return Ok(Some(policy::sha256(value.as_bytes())));
            }
        }
        let toplevel = Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        let anchor = toplevel
            .or_else(|| workspace.canonicalize().ok())
            .unwrap_or_else(|| workspace.to_path_buf());
        Ok(Some(policy::sha256(anchor.as_os_str().as_encoded_bytes())))
    }
    pub fn open_structured(&self) -> Result<Store> {
        Ok(Store::open(self.index_path())?)
    }
    fn scopes(&self, id: Option<&str>) -> Result<Vec<Scope>> {
        let mut scopes = vec![Self::owner_scope()];
        if let Some(id) = id {
            scopes.push(Self::workspace_scope(id)?);
        }
        Ok(scopes)
    }
    fn hit(
        &self,
        store: &Store,
        access: &Access,
        m: &Memory,
        snapshot: &Snapshot,
    ) -> Result<MemoryHit> {
        // The scope's legacy anchor names the memory's origin for callers that
        // label records by source path; it is an identifier, not a live file.
        let source = match &m.draft.scope.workspace {
            Some(id) => self.root.join("workspace").join(id).join("MEMORY.md"),
            None => self.global_path(),
        };
        Ok(MemoryHit {
            id: store.numeric_id(access, &m.id)?,
            text: m.draft.body.clone(),
            source,
            line_start: 0,
            line_end: 0,
            stale: store.freshness(access, m, snapshot)? != Freshness::Current,
        })
    }
    fn access_for(&self, id: Option<&str>) -> Result<Access> {
        Ok(Access::operator(self.scopes(id)?)?)
    }
    pub fn remember(
        &self,
        scope: MemoryScope,
        workspace_id: Option<&str>,
        note: &str,
    ) -> Result<MemoryHit> {
        let selected = match scope {
            MemoryScope::Global => Self::owner_scope(),
            MemoryScope::Workspace => Self::workspace_scope(
                workspace_id.ok_or_else(|| anyhow!("workspace scope requires an id"))?,
            )?,
        };
        let access = Access::agent(vec![selected.clone()])?;
        let mut store = self.open_structured()?;
        let draft = Draft::note(
            selected,
            policy::excerpt(note, 80),
            note,
            Evidence {
                kind: SourceKind::Agent,
                uri: "codewhale:native-capture".into(),
                locator: "Unreviewed capture through legacy native entry point".into(),
                sha256: None,
                observed_at: 0,
            },
        );
        let key = format!("native-note:{}", policy::sha256(note.as_bytes()));
        let r = store.capture(&access, &key, draft)?;
        let mut hit = self.hit(&store, &access, &r.memory, &Snapshot::default())?;
        hit.text = format!("Pending review in Context Lens: {}", hit.text);
        Ok(hit)
    }
    /// Authenticated operator surfaces (`POST /v1/memory`, the Lens remember
    /// action): the explicit request IS the review, so the capture is promoted
    /// in the same call, exactly as `Action::Remember` does. Model-reachable
    /// paths must keep using `remember`, which can only propose a candidate.
    pub fn remember_reviewed(
        &self,
        scope: MemoryScope,
        workspace_id: Option<&str>,
        note: &str,
    ) -> Result<MemoryHit> {
        let selected = match scope {
            MemoryScope::Global => Self::owner_scope(),
            MemoryScope::Workspace => Self::workspace_scope(
                workspace_id.ok_or_else(|| anyhow!("workspace scope requires an id"))?,
            )?,
        };
        let access = Access::operator(vec![selected.clone()])?;
        let mut store = self.open_structured()?;
        let draft = Draft::note(
            selected,
            policy::excerpt(note, 80),
            note,
            Evidence {
                kind: SourceKind::User,
                uri: "codewhale:operator".into(),
                locator: "Explicit remember through an authenticated host surface".into(),
                sha256: None,
                observed_at: 0,
            },
        );
        let key = format!("operator-note:{}", policy::sha256(note.as_bytes()));
        let r = store.capture(&access, &key, draft)?;
        let memory = if r.memory.status == Status::Candidate {
            store.approve(
                &access,
                &r.memory.id,
                r.memory.revision,
                None,
                &Snapshot::default(),
            )?
        } else {
            r.memory
        };
        self.hit(&store, &access, &memory, &Snapshot::default())
    }
    pub fn revise(
        &self,
        scope: MemoryScope,
        workspace_id: Option<&str>,
        from: &str,
        to: &str,
        evidence: &str,
    ) -> Result<MemoryHit> {
        let selected = match scope {
            MemoryScope::Global => Self::owner_scope(),
            MemoryScope::Workspace => Self::workspace_scope(
                workspace_id.ok_or_else(|| anyhow!("workspace id required"))?,
            )?,
        };
        let access = Access::agent(vec![selected.clone()])?;
        let mut store = self.open_structured()?;
        let entries = store.list(&access, None, 500)?;
        let matches: Vec<_> = entries
            .iter()
            .filter(|m| m.status == Status::Active && m.draft.body.trim() == from.trim())
            .collect();
        if matches.len() != 1 {
            bail!(
                "correction must match exactly one active note on the bounded page; use Context Lens UUIDs"
            );
        }
        let old = matches[0];
        let mut draft = Draft::note(
            selected,
            policy::excerpt(to, 80),
            to,
            Evidence {
                kind: SourceKind::Agent,
                uri: format!("codewhale:memory:{}", old.id),
                locator: policy::excerpt(evidence, 256),
                sha256: None,
                observed_at: 0,
            },
        );
        draft.kind = old.draft.kind;
        draft.key = old.draft.key.clone();
        let request = format!(
            "native-correction:{}:{}",
            old.id,
            policy::sha256(format!("{to}\0{evidence}").as_bytes())
        );
        let r = store.capture(&access, &request, draft)?;
        let mut hit = self.hit(&store, &access, &r.memory, &Snapshot::default())?;
        hit.text = format!(
            "Correction candidate; original unchanged. Review in Context Lens: {}",
            hit.text
        );
        Ok(hit)
    }
    // Retirement is a review operation: the Lens `forget` action owns it. No
    // unreviewed delete entry point exists on this facade.
    pub fn prompt_block(
        &self,
        workspace: &Path,
        max_entries: usize,
        max_chars: usize,
    ) -> Result<Option<String>> {
        let id = Self::workspace_id(workspace)?;
        let access = self.access_for(id.as_deref())?;
        let store = self.open_structured()?;
        let snapshot = workspace::snapshot(workspace, store.dependency_paths(&access)?)?;
        let (_report, packet) = store.context_packet(
            &access,
            &Recall {
                limit: max_entries.clamp(1, 64),
                snapshot,
                ..Default::default()
            },
            &ByteCounter,
            &ContextBudget {
                max_units: max_chars,
                max_bytes: max_chars,
                max_entries,
            },
        )?;
        Ok(if packet.selected.is_empty() {
            None
        } else {
            Some(packet.text)
        })
    }
    /// Session-scoped structured access for engine boundaries. Scopes are
    /// owner + workspace + this session's context scope, so receipts are
    /// attributable to the session that made them and nothing else can write
    /// into them.
    fn session_binding(
        &self,
        workspace: &Path,
        session_id: &str,
    ) -> Result<(Store, Access, Scope)> {
        let id = Self::workspace_id(workspace)?;
        let context_scope = match &id {
            Some(id) => Self::workspace_scope(id)?,
            None => Self::owner_scope(),
        }
        .session(session_id);
        let mut scopes = vec![Self::owner_scope()];
        if let Some(id) = &id {
            scopes.push(Self::workspace_scope(id)?);
        }
        scopes.push(context_scope.clone());
        let access = Access::operator(scopes)?;
        Ok((self.open_structured()?, access, context_scope))
    }
    /// Like `prompt_block`, but the packet is prepared through the engine's
    /// durable receipt path: `memory_contexts` records which bytes and memory
    /// revisions were assembled, under this session's trace. The request key is
    /// bound to the recall inputs, so an identical rebuild is idempotent while
    /// changed inputs mint a new receipt. A receipt records preparation only —
    /// append/dispatch acknowledgements remain the engine's to make.
    ///
    /// Receipt failure falls back to the same packet bytes without one: the
    /// receipt is observability, never a precondition for shipping context.
    pub fn prompt_block_traced(
        &self,
        workspace: &Path,
        session_id: &str,
        max_entries: usize,
        max_chars: usize,
    ) -> Result<Option<String>> {
        if session_id.is_empty()
            || session_id.len() > 128
            || !session_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
        {
            return self.prompt_block(workspace, max_entries, max_chars);
        }
        let (store, access, context_scope) = self.session_binding(workspace, session_id)?;
        let snapshot = workspace::snapshot(workspace, store.dependency_paths(&access)?)?;
        let query = Recall {
            limit: max_entries.clamp(1, 64),
            snapshot,
            ..Default::default()
        };
        let key_material = serde_json::to_vec(
            &serde_json::json!({"q":query.query,"snapshot":query.snapshot,"limit":query.limit,
            "graph":query.expand_graph,"vector_cap":query.vector_scan_limit,"budget":[max_chars,max_entries],"unit":"utf8_bytes"}),
        )?;
        let request_key = format!("session-prompt:{}", policy::sha256(&key_material));
        let budget = ContextBudget {
            max_units: max_chars,
            max_bytes: max_chars,
            max_entries,
        };
        match store.prepare_context(
            &access,
            &context_scope,
            session_id,
            &request_key,
            &query,
            &ByteCounter,
            &budget,
        ) {
            Ok(prepared) => Ok(if prepared.packet.selected.is_empty() {
                None
            } else {
                Some(prepared.packet.text)
            }),
            Err(_) => self.prompt_block(workspace, max_entries, max_chars),
        }
    }
    /// Run the session-start boundary for `session_id`. The hook plan maps it
    /// to ReconcilePendingOperations, realized here as detection — contexts
    /// this session prepared but never saw dispatch-acknowledged — because the
    /// store owns the receipts and only a host can replay them. Completion is
    /// recorded durably, so a restarted session reconciles once.
    /// Returns the number of interrupted contexts found.
    pub fn session_start(&self, workspace: &Path, session_id: &str) -> Result<usize> {
        if session_id.is_empty()
            || session_id.len() > 128
            || !session_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
        {
            bail!("invalid session id for memory reconcile");
        }
        let (store, access, context_scope) = self.session_binding(workspace, session_id)?;
        let snapshot = workspace::snapshot(workspace, store.dependency_paths(&access)?)?;
        let event = HookEvent {
            id: format!("session-start:{session_id}"),
            boundary: Boundary::SessionStart,
            trace_id: session_id.to_owned(),
            sequence: 0,
            observed_at: store.timestamp(),
            explicit_user_request: false,
            success: None,
        };
        let plan = hooks::plan(
            &HookPolicy {
                enabled: true,
                auto_candidates: false,
                recall_on_task: true,
            },
            &event,
        );
        let mut interrupted = 0usize;
        for intent in &plan.intents {
            if intent == &hooks::Intent::ReconcilePendingOperations {
                interrupted = store
                    .context_receipts(&access, Some(session_id), &snapshot)?
                    .iter()
                    .filter(|r| r.stage != "dispatched")
                    .count();
            }
        }
        store.record_completed_hook(&access, &context_scope, &event)?;
        Ok(interrupted)
    }
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryHit>> {
        self.search_scoped(None, None, query, limit)
    }
    pub fn search_for_workspace(
        &self,
        workspace: &Path,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryHit>> {
        let id = Self::workspace_id(workspace)?;
        self.search_in_workspace(id.as_deref(), workspace, query, limit)
    }
    /// [`Self::search_for_workspace`] for a caller that already resolved the
    /// workspace's memory identity.
    pub fn search_in_workspace(
        &self,
        workspace_id: Option<&str>,
        workspace: &Path,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryHit>> {
        self.search_scoped(workspace_id, Some(workspace), query, limit)
    }
    fn search_scoped(
        &self,
        id: Option<&str>,
        root: Option<&Path>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryHit>> {
        let access = self.access_for(id)?;
        let store = self.open_structured()?;
        let snapshot = match root {
            Some(root) => workspace::snapshot(root, store.dependency_paths(&access)?)?,
            None => Snapshot::default(),
        };
        let report = store.recall(
            &access,
            &Recall {
                query: query.into(),
                limit,
                snapshot: snapshot.clone(),
                ..Default::default()
            },
        )?;
        report
            .hits
            .iter()
            .map(|h| self.hit(&store, &access, &h.memory, &snapshot))
            .collect()
    }
    pub fn get_for_workspace(&self, workspace: &Path, id: i64) -> Result<Option<MemoryHit>> {
        let workspace_id = Self::workspace_id(workspace)?;
        self.get_scoped(workspace_id.as_deref(), Some(workspace), id)
    }
    fn get_scoped(
        &self,
        workspace_id: Option<&str>,
        root: Option<&Path>,
        id: i64,
    ) -> Result<Option<MemoryHit>> {
        let access = self.access_for(workspace_id)?;
        let store = self.open_structured()?;
        let m = match store.get_numeric(&access, id) {
            Ok(m) => m,
            Err(codewhale_memory::Error::NotFound) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if !matches!(m.status, Status::Active | Status::Stale) {
            return Ok(None);
        }
        let snapshot = match root {
            Some(root) => workspace::snapshot(root, store.dependency_paths(&access)?)?,
            None => Snapshot::default(),
        };
        Ok(Some(self.hit(&store, &access, &m, &snapshot)?))
    }
    pub fn list_all(
        &self,
        scope: Option<MemoryScope>,
        workspace_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryHit>> {
        let scopes = match scope {
            None => self.scopes(workspace_id)?,
            Some(MemoryScope::Global) => vec![Self::owner_scope()],
            Some(MemoryScope::Workspace) => vec![Self::workspace_scope(
                workspace_id.ok_or_else(|| anyhow!("workspace id required"))?,
            )?],
        };
        let access = Access::operator(scopes)?;
        let store = self.open_structured()?;
        // Entry surfaces show only live memory; candidates and reviewed-off
        // entries are visible in the Context Lens, not here.
        store
            .list(&access, None, limit)?
            .iter()
            .filter(|m| matches!(m.status, Status::Active | Status::Stale))
            .map(|m| self.hit(&store, &access, m, &Snapshot::default()))
            .collect()
    }
    pub fn import_legacy(&self, path: &Path) -> Result<bool> {
        if !path.is_file() {
            return Ok(false);
        }
        let meta = fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() || meta.len() > 1024 * 1024 {
            bail!("legacy import refused: symlink or size bound");
        }
        let text = fs::read_to_string(path)?;
        let scope = Self::owner_scope();
        let access = Access::operator(vec![scope.clone()])?;
        let mut store = self.open_structured()?;
        let report = codewhale_memory::import::markdown(
            &mut store,
            &access,
            &scope,
            "codewhale:legacy-memory",
            &text,
        )?;
        if !report.rejected.is_empty() {
            bail!(
                "legacy import left {} rejected notes unchanged; inspect before retry",
                report.rejected.len()
            );
        }
        Ok(report.created > 0)
    }
    pub fn export(&self) -> Result<String> {
        let store = self.open_structured()?;
        let owner = Access::operator(vec![Self::owner_scope()])?;
        let scopes = store.local_scope_inventory(&owner)?;
        let mut output = String::from(
            "# CodeWhale memory export\n\nNot an instruction file. This Markdown is not authoritative.\n",
        );
        for scope in scopes {
            let access = Access::operator(vec![scope])?;
            let mut after = None;
            loop {
                let batch = store.list(&access, after.as_deref(), 500)?;
                if batch.is_empty() {
                    break;
                }
                for m in &batch {
                    output.push_str(&format!(
                        "\n## {} [{}] {}\n\n{}\n",
                        m.id,
                        m.status.as_str(),
                        m.draft.title,
                        m.draft.body
                    ));
                }
                after = batch.last().map(|m| m.id.clone());
            }
        }
        Ok(output)
    }
    pub fn reindex(&self) -> Result<usize> {
        let mut store = self.open_structured()?;
        let access = Access::operator(vec![Self::owner_scope()])?;
        store.reindex(&access)?;
        let mut count = 0;
        for scope in store.local_scope_inventory(&access)? {
            let access = Access::operator(vec![scope])?;
            let mut after = None;
            loop {
                let batch = store.list(&access, after.as_deref(), 500)?;
                if batch.is_empty() {
                    break;
                }
                count += batch.len();
                after = batch.last().map(|m| m.id.clone());
            }
        }
        Ok(count)
    }
    pub fn delete_all(&self, scope: Option<MemoryScope>, workspace_id: Option<&str>) -> Result<()> {
        let scopes = match scope {
            Some(MemoryScope::Global) => vec![Self::owner_scope()],
            Some(MemoryScope::Workspace) => vec![Self::workspace_scope(
                workspace_id.ok_or_else(|| anyhow!("workspace id required"))?,
            )?],
            None => {
                let store = self.open_structured()?;
                let owner = Access::operator(vec![Self::owner_scope()])?;
                let mut scopes = store.local_scope_inventory(&owner)?;
                if scopes.len() >= 10000 {
                    bail!("Scope inventory reached its bound; delete narrower scopes explicitly");
                }
                if scopes.is_empty() {
                    scopes.push(Self::owner_scope());
                }
                scopes
            }
        };
        let access = Access::operator(scopes)?;
        let mut store = self.open_structured()?;
        loop {
            let batch = store.list(&access, None, 500)?;
            if batch.is_empty() {
                break;
            }
            for m in batch {
                match store.get(&access, &m.id) {
                    Ok(current) => {
                        store.forget(&access, &current.id, current.revision)?;
                    }
                    Err(codewhale_memory::Error::NotFound) => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Ok(())
    }
}
fn safe_component(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        bail!("workspace identity must be a lowercase SHA-256 hash");
    }
    Ok(())
}

/// Compose the user-memory prompt block for the native store resolved from a
/// memory path. Single seam used by the engine, the TUI system-prompt
/// builder, and the context report so all three describe the same bytes.
/// Returns `None` when memory is disabled, the path is not a native
/// `memory/global/MEMORY.md` layout, or there is nothing worth injecting.
/// The block is a `codewhale.memory.context.v1` envelope: memory entries are
/// untrusted evidence, never instructions.
#[must_use]
pub fn native_prompt_block(enabled: bool, memory_path: &Path, workspace: &Path) -> Option<String> {
    if !enabled {
        return None;
    }
    NativeMemoryStore::from_global_path(memory_path)?
        .prompt_block(workspace, 32, 12_000)
        .ok()
        .flatten()
}

/// The engine's session-boundary variant: identical bytes plus a durable
/// `prepared` context receipt under the session's trace, so the Context Lens
/// can show what was assembled for it. Diagnostics and previews keep using
/// `native_prompt_block` — a receipt is only meaningful when it names a real
/// session. The receipt never gates the packet.
#[must_use]
pub fn native_prompt_block_traced(
    enabled: bool,
    memory_path: &Path,
    workspace: &Path,
    session_id: &str,
) -> Option<String> {
    if !enabled {
        return None;
    }
    NativeMemoryStore::from_global_path(memory_path)?
        .prompt_block_traced(workspace, session_id, 32, 12_000)
        .ok()
        .flatten()
}
