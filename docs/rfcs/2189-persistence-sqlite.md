# RFC: Persistence SQLite Migration
...

### 1.1 `crates/state` — partial SQLite (rusqlite)

**Backend**: SQLite via `rusqlite` (not sqlx).  
**Path**: `~/.deepseek/state.db`  
**Tables**: `threads`, `thread_dynamic_tools`, `messages`, `checkpoints`, `jobs`  
**Also**: `session_index.jsonl` — append-only JSONL for thread-name lookups.  
**Schema versioning**: none — table shape is versioned implicitly by the binary.

### 1.2 `crates/engine/src/session_manager.rs` — JSON sessions

**Backend**: individual JSON files + atomic writes via `write_atomic`.  
**Paths**:
- `~/.codewhale/sessions/{id}.json` (preferred, v0.8.44+) or `~/.deepseek/sessions/{id}.json` (fallback)
- `~/.deepseek/sessions/checkpoints/latest.json` — crash-recovery checkpoint
- `~/.deepseek/sessions/checkpoints/offline_queue.json` — offline/degraded-mode queue

**Schema constants**:
- `CURRENT_SESSION_SCHEMA_VERSION: u32 = 1` (`SavedSession`)
- `CURRENT_QUEUE_SCHEMA_VERSION: u32 = 1` (`OfflineQueueState`)

**Policy**: reject-newer — older binary will refuse to load data written by a newer version.

### 1.3 `crates/engine/src/runtime_threads.rs` — JSON runtime store

**Backend**: per-record JSON files + append-only JSONL for events.  
**Paths** (under `~/.deepseek/tasks/runtime/` or `DEEPSEEK_RUNTIME_DIR`):
- `threads/{id}.json`
- `turns/{id}.json`
- `items/{id}.json`
- `events/{thread_id}.jsonl` — append-only JSONL event timeline
- `state.json` — global monotonic sequence counter

**Schema constants**:
- `CURRENT_RUNTIME_SCHEMA_VERSION: u32 = 2`

**Policy**: reject-newer.

### 1.4 `crates/engine/src/task_manager.rs` — JSON task store

**Backend**: per-record JSON files + atomic writes.  
**Paths** (under `~/.deepseek/tasks/` or `DEEPSEEK_TASKS_DIR`):
- `{id}.json` — per-task records
- `queue.json` — queue state

**Schema constants**:
- `CURRENT_TASK_SCHEMA_VERSION: u32 = 2`

**Policy**: reject-newer.

### 1.5 `crates/engine/src/automation_manager.rs` — JSON automation store

**Backend**: per-record JSON files.  
**Paths** (under `~/.deepseek/automations/` or `DEEPSEEK_AUTOMATIONS_DIR`):
- `{id}.json`

**Schema constants**:
- `CURRENT_AUTOMATION_SCHEMA_VERSION: u32 = 1`

### 1.6 `crates/tui/src/audit.rs` — JSONL audit log

**Backend**: append-only JSONL with fsync after each event.  
**Path**: `~/.deepseek/audit.log`  
**Schema**: no version field — each line is a `{"ts", "event", "details"}` blob.

### 1.7 Summary of issues

| Area | Backend | Schema Version | Write Strategy | Queryability |
|------|---------|---------------|----------------|-------------|
| state (threads/messages/jobs) | SQLite | implicit | direct SQL | SQL |
| sessions | JSON files | v1 | atomic rename | file scan |
| runtime threads/turns/items | JSON files | v2 | atomic rename | file scan |
| runtime events | JSONL | v2 | append+fsync | linear scan |
| tasks | JSON files | v2 | atomic rename | file scan |
| automations | JSON files | v1 | atomic rename | file scan |
| audit | JSONL | none | append+fsync | linear scan |

**Key pain points**:
1. **Listing** threads/sessions/tasks requires scanning directories and deserializing every file.
2. **Filtering** (e.g., "all failed tasks in last 7 days") requires full scans.
3. **No transactional consistency** — a crash between saving a turn and its items can leave orphans.
4. **Event timeline growth** — JSONL append is O(n) for replay; no indexing.
5. **Six different schema version constants** across four modules, each with the same reject-newer policy.

