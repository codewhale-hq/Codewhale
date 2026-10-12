use std::collections::HashMap;
use std::path::PathBuf;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::runtime_threads::{
    CreateThreadRequest, RuntimeThreadManager, RuntimeTurnStatus, ThreadDetail, ThreadListFilter,
    TurnItemLifecycleStatus,
};
use crate::session_manager::{
    SavedSession, SessionListFilter, SessionManager, SessionMetadata, SessionMutator,
    create_saved_session_with_id_and_mode,
};
use crate::session_peek::{MAX_PEEK_ENTRIES, SessionPeek, build_peek};
use crate::session_projection::{SessionQuery, SessionSortMode, SessionSummary, project_sessions};
use codewhale_models::{Message, Role};

use super::{ApiError, RuntimeApiState, map_thread_err, truncate_text};

#[derive(Debug, Serialize)]
pub(super) struct SessionsResponse {
    sessions: Vec<SessionMetadata>,
}

#[derive(Debug, Serialize)]
pub(super) struct SessionDetailResponse {
    pub(super) metadata: SessionMetadata,
    pub(super) messages: Vec<Value>,
    pub(super) system_prompt: Option<String>,
    /// Turns that ended `Failed`, with the redacted reason the transcript
    /// showed. Absent when none did.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) turn_outcomes: Vec<crate::session_manager::SavedTurnOutcome>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CreateSessionRequest {
    thread_id: String,
    title: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct CreateSessionResponse {
    session_id: String,
    thread_id: String,
    message_count: usize,
    title: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResumeSessionRequest {
    pub(crate) model: Option<String>,
    pub(crate) mode: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResumeSessionResponse {
    pub(crate) thread_id: String,
    pub(crate) session_id: String,
    pub(crate) message_count: usize,
    pub(crate) summary: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct SessionsQuery {
    limit: Option<usize>,
    search: Option<String>,
    /// Include archived sessions. Same name and meaning as the `/v1/threads`
    /// query pair, so a client does not need two mental models (#4397).
    #[serde(default)]
    include_archived: Option<bool>,
    /// Return archived sessions only. Overrides `include_archived`.
    #[serde(default)]
    archived_only: Option<bool>,
    /// Restrict to sessions recorded against this workspace. Absent means
    /// every workspace, matching the historical behaviour of this route.
    #[serde(default)]
    workspace: Option<PathBuf>,
    /// `recent` (default), `name`, or `size`.
    #[serde(default)]
    sort: Option<String>,
}

/// `PATCH /v1/sessions/{id}` body. Both fields are optional; omitting one
/// leaves it untouched.
#[derive(Debug, Deserialize)]
pub(super) struct PatchSessionRequest {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    archived: Option<bool>,
}

/// Lifecycle receipt for a session mutation.
///
/// Deliberately shaped like the thread patch receipt: the caller gets the
/// resulting record plus an explicit `changes` map of what actually moved, so
/// a no-op patch is distinguishable from an applied one without diffing.
#[derive(Debug, Serialize)]
pub(super) struct PatchSessionResponse {
    session: SessionMetadata,
    changes: HashMap<String, Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SaveSessionRequest {
    /// Thread ID to save as a session. If omitted, saves the most recently
    /// active thread.
    #[serde(default)]
    pub(crate) thread_id: Option<String>,
    /// If provided, update the existing session with this ID instead of
    /// creating a new one. This matches TUI's `build_session_snapshot`
    /// behavior where it updates the current session in-place.
    #[serde(default)]
    pub(crate) session_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SaveSessionResponse {
    pub(crate) session_id: String,
    pub(super) session: SessionDetailResponse,
}

/// Turn a `SessionsQuery` into the shared projection query.
///
/// The whole point of routing through [`SessionQuery`] is that the API's
/// filter/sort/search semantics are the *same code* the TUI picker and the
/// sidebar rail run, not a parallel reimplementation that drifts.
fn projection_query(query: &SessionsQuery) -> SessionQuery {
    let mut projected = SessionQuery::default()
        .with_filter(SessionListFilter::from_query(
            query.include_archived,
            query.archived_only,
        ))
        .with_sort(
            query
                .sort
                .as_deref()
                .map_or(SessionSortMode::Recent, SessionSortMode::from_str_or_recent),
        )
        .with_search(query.search.clone().unwrap_or_default())
        .with_limit(query.limit.unwrap_or(50).clamp(1, 500));
    if let Some(workspace) = query.workspace.as_deref() {
        projected = projected.scoped_to(workspace);
    }
    projected
}

pub(super) async fn list_sessions(
    State(state): State<RuntimeApiState>,
    Query(query): Query<SessionsQuery>,
) -> Result<Json<SessionsResponse>, ApiError> {
    let manager = SessionManager::new(state.sessions_dir.clone())
        .map_err(|e| ApiError::internal(format!("Failed to open sessions dir: {e}")))?;
    let all = manager
        .list_sessions()
        .map_err(|e| ApiError::internal(format!("Failed to list sessions: {e}")))?;
    // This route keeps returning full `SessionMetadata` for compatibility;
    // `/v1/sessions/summary` is the projected shape. Membership *and* order
    // come from the shared projection so the two routes never disagree.
    let sessions: Vec<SessionMetadata> = project_sessions(&all, &projection_query(&query), None)
        .into_iter()
        .filter_map(|summary| all.iter().find(|m| m.id == summary.id).cloned())
        .collect();
    Ok(Json(SessionsResponse { sessions }))
}

/// `GET /v1/sessions/summary` — the projected row shape.
///
/// Field-compatible with `/v1/threads/summary` so the embedded dashboard can
/// render a saved session and a live thread with one row renderer, which is
/// what "one projection" means in practice rather than as an aspiration.
pub(super) async fn list_sessions_summary(
    State(state): State<RuntimeApiState>,
    Query(query): Query<SessionsQuery>,
) -> Result<Json<Vec<SessionSummary>>, ApiError> {
    let manager = SessionManager::new(state.sessions_dir.clone())
        .map_err(|e| ApiError::internal(format!("Failed to open sessions dir: {e}")))?;
    let all = manager
        .list_sessions()
        .map_err(|e| ApiError::internal(format!("Failed to list sessions: {e}")))?;
    Ok(Json(project_sessions(
        &all,
        &projection_query(&query),
        None,
    )))
}

/// `PATCH /v1/sessions/{id}` — rename and/or archive a saved session.
///
/// Both mutations go through the manager's single writers
/// (`rename_session`, `set_session_archived`), which is what keeps the web
/// dashboard, the TUI picker, and `/sessions archive` from producing three
/// different notions of the same lifecycle state.
pub(super) async fn patch_session(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Json(req): Json<PatchSessionRequest>,
) -> Result<Json<PatchSessionResponse>, ApiError> {
    if req.title.is_none() && req.archived.is_none() {
        return Err(ApiError::bad_request(
            "PATCH /v1/sessions/{id} requires at least one of `title` or `archived`",
        ));
    }
    // The single writers hold the session's live lease across their load and
    // save; taking it may retry with short sleeps. Keep all of it off the
    // async worker (#6149).
    tokio::task::spawn_blocking(move || patch_session_blocking(state.sessions_dir, &id, &req))
        .await
        .map_err(|_| ApiError::internal("session update failed"))?
        .map(Json)
}

fn patch_session_blocking(
    sessions_dir: PathBuf,
    id: &str,
    req: &PatchSessionRequest,
) -> Result<PatchSessionResponse, ApiError> {
    let manager = SessionManager::new(sessions_dir)
        .map_err(|e| ApiError::internal(format!("Failed to open sessions dir: {e}")))?;

    let before = manager
        .load_session(id)
        .map_err(|e| map_session_err(id, e, "read"))?
        .metadata;
    let mut metadata = before.clone();
    let mut changes: HashMap<String, Value> = HashMap::new();

    if let Some(title) = req.title.as_deref() {
        // Validate the title before touching the store so a rejected title
        // reports *why* it was rejected rather than the generic "invalid
        // session id" that `map_session_err` produces for `InvalidInput`.
        crate::session_manager::normalize_session_title(title)
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        metadata = manager
            .rename_session(id, title, SessionMutator::External)
            .map_err(|e| map_session_err(id, e, "rename"))?;
        if metadata.title != before.title {
            changes.insert("title".to_string(), json!(metadata.title));
        }
    }
    if let Some(archived) = req.archived {
        metadata = manager
            .set_session_archived(id, archived, SessionMutator::External)
            .map_err(|e| map_session_err(id, e, "archive"))?;
        if metadata.archived != before.archived {
            changes.insert("archived".to_string(), json!(metadata.archived));
        }
    }

    Ok(PatchSessionResponse {
        session: metadata,
        changes,
    })
}

/// Hold `id`'s live lease across an external load and save, refusing (409)
/// as rename, archive and delete do when an interactive session holds the
/// document open — its next autosave would revert the write — and rejecting
/// a malformed id (400). A released liveness probe cannot protect the write
/// that follows it (#6144). Taking the lease may retry with short sleeps, so
/// it runs off the async worker; drop the lease only after the save.
async fn reserve_external_session_write(
    state: &RuntimeApiState,
    id: &str,
    action: &'static str,
) -> Result<crate::session_manager::SessionLease, ApiError> {
    reserve_session_write(&state.sessions_dir, id, action).await
}

async fn reserve_session_write(
    sessions_dir: &std::path::Path,
    id: &str,
    action: &'static str,
) -> Result<crate::session_manager::SessionLease, ApiError> {
    let sessions_dir = sessions_dir.to_path_buf();
    let id = id.to_string();
    tokio::task::spawn_blocking(move || {
        SessionManager::new(sessions_dir)
            .map_err(|e| ApiError::internal(format!("Failed to open sessions dir: {e}")))?
            .reserve_session_for_external_write(&id)
            .map_err(|e| map_session_err(&id, e, action))
    })
    .await
    .map_err(|_| ApiError::internal("session lease reservation failed"))?
}

/// `GET /v1/sessions/{id}` query options.
#[derive(Debug, Deserialize, Default)]
pub(super) struct SessionDetailQuery {
    /// When true, return a bounded, redacted [`SessionPeek`] instead of the
    /// full transcript. The dashboard always asks for this: shipping a
    /// multi-megabyte transcript to a browser in order to show twelve lines is
    /// both wasteful and a needless place to re-emit secrets.
    #[serde(default)]
    peek: Option<bool>,
    /// Entry budget for the peek, clamped to [`MAX_PEEK_ENTRIES`].
    #[serde(default)]
    entries: Option<usize>,
}

/// Either the full session or a bounded peek, chosen by `?peek=true`.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(super) enum SessionDetailOrPeek {
    Peek(Box<SessionPeek>),
    Detail(Box<SessionDetailResponse>),
}

pub(super) async fn get_session(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Query(query): Query<SessionDetailQuery>,
) -> Result<Json<SessionDetailOrPeek>, ApiError> {
    let manager = SessionManager::new(state.sessions_dir.clone())
        .map_err(|e| ApiError::internal(format!("Failed to open sessions dir: {e}")))?;
    let session = manager
        .load_session(&id)
        .map_err(|e| map_session_err(&id, e, "read"))?;

    if query.peek.unwrap_or(false) {
        let entries = query.entries.unwrap_or(MAX_PEEK_ENTRIES);
        return Ok(Json(SessionDetailOrPeek::Peek(Box::new(build_peek(
            &session, entries,
        )))));
    }
    Ok(Json(SessionDetailOrPeek::Detail(Box::new(
        session_to_detail(session),
    ))))
}

/// `POST /v1/sessions/{id}/resume-thread` — open a saved session as a live
/// thread.
///
/// Idempotent for a conversation that is already open: when an active thread
/// already holds this session (and its checkpoint still describes the file),
/// that thread is returned with `200 OK` instead of minting a second one with
/// `201 Created`. Minting unconditionally is what made "continue this
/// conversation" grow the rail by a row per visit.
///
/// `req.model` / `req.mode` apply only when a thread is created. An open thread
/// keeps the route it was opened with — a caller that needs a different route
/// is creating a conversation, not resuming one.
///
/// `message_count` reports the *saved session's* count. A reused thread may hold
/// more than that: it keeps the turns it ran after the session's last save.
pub(super) async fn resume_session_thread(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Json(req): Json<ResumeSessionRequest>,
) -> Result<(StatusCode, Json<ResumeSessionResponse>), ApiError> {
    resume_session_in_runtime(
        &state.runtime_threads,
        &state.sessions_dir,
        &id,
        req,
        (
            state.config_path.as_deref(),
            state.config_profile.as_deref(),
        ),
        None,
    )
    .await
}

pub(crate) async fn resume_session_in_runtime(
    runtime: &std::sync::Arc<RuntimeThreadManager>,
    sessions_dir: &std::path::Path,
    id: &str,
    req: ResumeSessionRequest,
    config_source: (Option<&std::path::Path>, Option<&str>),
    allow_shell: Option<bool>,
) -> Result<(StatusCode, Json<ResumeSessionResponse>), ApiError> {
    let _checkpoint_admission = runtime.session_checkpoint_guard().await;
    let manager = SessionManager::new(sessions_dir.to_path_buf())
        .map_err(|e| ApiError::internal(format!("Failed to open sessions dir: {e}")))?;
    let session = manager
        .resume_session(id)
        .map_err(|e| map_session_err(id, e, "read"))?
        .session;

    if runtime.is_acp_host()
        && session
            .metadata
            .runtime_store
            .as_ref()
            .is_some_and(|binding| *binding != runtime.session_store_binding())
    {
        return Err(ApiError::conflict(
            "This saved conversation belongs to another Runtime store; secure owner attachment is not qualified, so ACP will not copy its history",
        ));
    }

    // Validate imported image bytes before allocating a Runtime thread. This
    // retains local history's existing bounds; invalid content cannot leave an
    // empty session, and no path or remote image reference is dereferenced.
    for message in session
        .messages
        .iter()
        .filter(|message| message.role == Role::User)
    {
        crate::image_attach::runtime_images_from_blocks(&message.content).map_err(|error| {
            ApiError::bad_request(format!("Cannot restore session image: {error}"))
        })?;
    }

    // The conversation may already be open. Answer with the thread that holds
    // it rather than adding a second row for the same history (see
    // `RuntimeThreadManager::thread_holding_session`).
    if let Some(existing) = runtime.thread_holding_session(id, &session) {
        let thread_id = existing.id;
        let message_count = session.messages.len();
        let summary = format!(
            "Session '{}' is already open in thread {thread_id} ({message_count} messages)",
            session.metadata.title
        );
        return Ok((
            StatusCode::OK,
            Json(ResumeSessionResponse {
                thread_id,
                session_id: id.to_string(),
                message_count,
                summary,
            }),
        ));
    }

    let model = req.model.unwrap_or_else(|| session.metadata.model.clone());
    let mode = req.mode.unwrap_or_else(|| {
        session
            .metadata
            .mode
            .clone()
            .unwrap_or_else(|| "agent".to_string())
    });

    let thread = runtime
        .create_thread_with_shell_policy(
            CreateThreadRequest {
                model: Some(model),
                model_provider: Some(session.metadata.model_provider.clone()),
                model_provider_id: session.metadata.model_provider_id.clone(),
                workspace: Some(session.metadata.workspace.clone()),
                mode: Some(mode),
                allow_shell,
                trust_mode: None,
                auto_approve: None,
                archived: false,
                system_prompt: session.system_prompt.clone(),
                task_id: None,
                ..Default::default()
            },
            config_source.0,
            config_source.1,
        )
        .await
        .map_err(map_resume_thread_create_err)?;

    let msg_count = session.messages.len();
    runtime
        .seed_thread_from_messages(&thread.id, &session.messages)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to seed thread history: {e}")))?;

    // Link the session to the new thread so that `ensure_engine_loaded`
    // can restore the full message history from the session file.
    runtime
        .set_thread_session_checkpoint(&thread.id, &session)
        .await
        .map_err(|e| {
            ApiError::internal(format!(
                "Saved session was read but its Runtime checkpoint could not be bound: {e}"
            ))
        })?;

    let summary = format!(
        "Resumed session '{}' ({} messages) into thread {}",
        session.metadata.title, msg_count, thread.id
    );

    Ok((
        StatusCode::CREATED,
        Json(ResumeSessionResponse {
            thread_id: thread.id,
            session_id: id.to_string(),
            message_count: msg_count,
            summary,
        }),
    ))
}

pub(super) async fn create_session_from_thread(
    State(state): State<RuntimeApiState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<CreateSessionResponse>), ApiError> {
    let _checkpoint_admission = state.runtime_threads.session_checkpoint_guard().await;
    let thread_id = req.thread_id.trim();
    if thread_id.is_empty() {
        return Err(ApiError::bad_request("thread_id is required"));
    }

    let detail = state
        .runtime_threads
        .get_thread_detail(thread_id)
        .await
        .map_err(map_thread_err)?;

    if thread_detail_has_live_work(&detail) {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            message: format!(
                "Thread {thread_id} has a queued or active turn; wait for completion before saving as a session"
            ),
            code: None,
        });
    }

    let messages = messages_from_thread_detail(&detail).map_err(|error| {
        ApiError::internal(format!("Failed to reconstruct thread history: {error}"))
    })?;
    if messages.is_empty() {
        return Err(ApiError::bad_request(format!(
            "Thread {thread_id} has no user or assistant messages to save"
        )));
    }

    let manager = SessionManager::new(state.sessions_dir.clone())
        .map_err(|e| ApiError::internal(format!("Failed to open sessions dir: {e}")))?;
    let total_tokens = total_tokens_from_thread_detail(&detail);
    // Export is idempotent (#6144). Every POST used to mint a fresh document
    // and rebind the thread to it, so each re-export left the previous
    // document unreferenced, and a crash between the save and the bind below
    // left the new one unreferenced too. Export writes only the document id
    // derived from the thread — the one its engine already writes artifacts
    // under — so a re-export or a retry after such a crash updates and binds
    // the document it already wrote.
    //
    // The document the thread is currently bound to is deliberately *not*
    // the target: a thread opened with `resume-thread` is bound to the
    // original saved session, often a TUI conversation, and rewriting it from
    // this thread's lossier projection would drop its images, tool work and
    // system prompt. Export leaves that document untouched.
    let session_handle = crate::runtime_threads::thread_session_id(&detail.thread.id);
    let _lease = reserve_external_session_write(&state, &session_handle, "export").await?;
    let existing = match manager.load_session(&session_handle) {
        Ok(existing) => Some(existing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(map_session_err(&session_handle, error, "read")),
    };
    let created = existing.is_none();
    let mut session = match existing {
        Some(existing) => {
            let mut updated =
                crate::session_manager::update_session(existing, &messages, total_tokens, None);
            updated.metadata.model = detail.thread.model.clone();
            updated.metadata.mode = Some(detail.thread.mode.clone());
            updated
        }
        None => create_saved_session_with_id_and_mode(
            session_handle.clone(),
            &messages,
            &detail.thread.model,
            &detail.thread.workspace,
            total_tokens,
            None,
            Some(&detail.thread.mode),
        ),
    };
    {
        let config = state.runtime_threads.read_config();
        stamp_session_provider_from_thread(&config, &detail, &mut session.metadata).map_err(
            |reason| {
            ApiError::bad_request(format!(
                    "Thread {thread_id} provider route is unavailable; session export will not fall back: {reason}"
            ))
            },
        )?;
    }
    session.system_prompt = detail.thread.system_prompt.clone();

    if let Some(title) =
        session_title_override(req.title.as_deref(), detail.thread.title.as_deref())
    {
        session.metadata.title = title;
    }
    let title = session.metadata.title.clone();
    let message_count = session.metadata.message_count;

    persist_thread_cost(&state.runtime_threads, thread_id, &mut session).await?;

    manager
        .save_session(&session)
        .map_err(|e| ApiError::internal(format!("Failed to save session: {e}")))?;

    // Link the session to the thread so that `ensure_engine_loaded` can
    // restore the full message history from the session file.
    state
        .runtime_threads
        .set_thread_session_checkpoint(&detail.thread.id, &session)
        .await
        .map_err(|e| {
            ApiError::internal(format!(
                "Session was saved but its Runtime checkpoint could not be bound: {e}"
            ))
        })?;

    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(CreateSessionResponse {
            session_id: session_handle,
            thread_id: detail.thread.id,
            message_count,
            title,
        }),
    ))
}

pub(super) fn stamp_session_provider_from_thread(
    config: &crate::config::Config,
    detail: &ThreadDetail,
    metadata: &mut crate::session_manager::SessionMetadata,
) -> Result<(), String> {
    let thread_has_route = detail
        .thread
        .model_provider
        .as_deref()
        .is_some_and(|provider| !provider.trim().is_empty())
        || detail.thread.model_provider_id.is_some();
    let provider_identity = if thread_has_route {
        config.resolve_persisted_provider_identity(
            detail.thread.model_provider.as_deref(),
            detail.thread.model_provider_id.as_deref(),
        )?
    } else if let Some(turn) = detail.turns.iter().rev().find(|turn| {
        turn.effective_provider
            .as_deref()
            .is_some_and(|provider| !provider.trim().is_empty())
            || turn.effective_provider_id.is_some()
    }) {
        config.resolve_persisted_provider_identity(
            turn.effective_provider.as_deref(),
            turn.effective_provider_id.as_deref(),
        )?
    } else {
        let key = config
            .provider
            .as_deref()
            .unwrap_or(crate::config::ProviderKind::Deepseek.as_str());
        config.resolve_provider_identity(key)?
    };
    metadata.set_model_provider_route(
        provider_identity.provider.as_str(),
        provider_identity.persisted_id(),
    );
    Ok(())
}

pub(super) fn thread_detail_has_live_work(detail: &ThreadDetail) -> bool {
    detail.turns.iter().any(|turn| {
        matches!(
            turn.status,
            RuntimeTurnStatus::Queued | RuntimeTurnStatus::InProgress
        )
    }) || detail.items.iter().any(|item| {
        matches!(
            item.status,
            TurnItemLifecycleStatus::Queued | TurnItemLifecycleStatus::InProgress
        )
    })
}

pub(super) fn messages_from_thread_detail(detail: &ThreadDetail) -> anyhow::Result<Vec<Message>> {
    let mut items_by_turn = HashMap::new();
    for item in &detail.items {
        items_by_turn
            .entry(item.turn_id.clone())
            .or_insert_with(Vec::new)
            .push(item.clone());
    }
    RuntimeThreadManager::reconstruct_messages_from_turns_with(&detail.turns, &items_by_turn)
}

/// Merge the thread's authoritative cost into a session about to be saved.
///
/// The engine snapshot carries messages/tokens but no cost — cost lives in
/// the turn records' route-audited usage — so derive it from the same
/// accumulation that powers `/v1/usage` (recorded-time pricing, both
/// published currencies). The parent/child split mirrors the TUI writer's
/// field semantics (`sync_cost_to_metadata`): `session_cost_*` carries
/// parent-turn spend and `subagent_cost_*` routed-child spend, so a session
/// previously saved by the TUI never gets child spend counted twice in
/// `total_estimate()`. Merging each side with max keeps a session resumed
/// across threads from losing previously persisted spend, and extends the
/// monotonic display guarantee (#244) to the persisted shape. Coverage
/// travels with the money it qualifies (#4318): the counters are
/// parent-turn coverage (the TUI's own session-level accounting), CNY
/// included, and `coverage_recorded` marks that this writer computed them
/// from audited turn records rather than deserializing a legacy default.
async fn persist_thread_cost(
    runtime: &std::sync::Arc<RuntimeThreadManager>,
    thread_id: &str,
    session: &mut crate::session_manager::SavedSession,
) -> Result<(), ApiError> {
    let usage = runtime
        .aggregate_usage_for_thread(thread_id)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to aggregate thread usage: {e}")))?;
    let combined = usage.combined();
    let cost = &mut session.metadata.cost;
    cost.session_cost_usd = cost.session_cost_usd.max(usage.parent.cost_usd);
    cost.session_cost_cny = cost.session_cost_cny.max(usage.parent.cost_cny);
    cost.subagent_cost_usd = cost.subagent_cost_usd.max(usage.routed_children.cost_usd);
    cost.subagent_cost_cny = cost.subagent_cost_cny.max(usage.routed_children.cost_cny);
    // The display total is session + subagent, so the high-water mark rides
    // the combined figure in both currencies.
    cost.displayed_cost_high_water_usd = cost.displayed_cost_high_water_usd.max(combined.cost_usd);
    cost.displayed_cost_high_water_cny = cost.displayed_cost_high_water_cny.max(combined.cost_cny);
    cost.priced_turns = cost
        .priced_turns
        .max(u32::try_from(usage.parent.priced_turns).unwrap_or(u32::MAX));
    cost.unpriced_turns = cost
        .unpriced_turns
        .max(u32::try_from(usage.parent.unpriced_turns).unwrap_or(u32::MAX));
    cost.cny_priced_turns = cost
        .cny_priced_turns
        .max(u32::try_from(usage.parent.cny_priced_turns).unwrap_or(u32::MAX));
    cost.cny_unpriced_turns = cost
        .cny_unpriced_turns
        .max(u32::try_from(usage.parent.cny_unpriced_turns).unwrap_or(u32::MAX));
    // Coverage travels with the money (#4318): reasons and classes are the
    // qualifiers a reload needs to treat these totals as known, not a
    // legacy-unknown complete zero. Parent-turn coverage only — the same
    // field the TUI writer uses; routed-child spend lives in subagent_cost_*.
    cost.unpriced_reasons
        .extend(usage.parent.unpriced_reasons.iter().cloned());
    cost.cny_unpriced_reasons
        .extend(usage.parent.cny_unpriced_reasons.iter().cloned());
    cost.unpriced_classes
        .extend(usage.parent.unpriced_classes.iter().cloned());
    cost.pricing_provenances
        .extend(usage.parent.pricing_provenances.iter().cloned());
    cost.live_pricing_defects
        .extend(usage.parent.live_pricing_defects.iter().cloned());
    cost.live_pricing_unusable_defects
        .extend(usage.parent.live_pricing_unusable_defects.iter().cloned());
    cost.route_receipts
        .extend(usage.parent.route_receipts.iter().cloned());
    cost.coverage_recorded = true;
    Ok(())
}

/// `PUT /v1/sessions` — save a thread's current engine state as a session.
///
/// Unlike `POST /v1/sessions` (which reconstructs messages from stored turn
/// items), this endpoint asks the engine for its live session snapshot so
/// token counts and message ordering are authoritative.
///
/// `session_id` names the document to write. Omitted, the thread's bound
/// document is updated, or, for a thread bound to none, a document is created
/// under the thread's own conversation id — see
/// [`crate::core::ops::SessionSnapshot::session_id`].
pub(super) async fn save_current_session(
    State(state): State<RuntimeApiState>,
    Json(req): Json<SaveSessionRequest>,
) -> Result<Json<SaveSessionResponse>, ApiError> {
    save_session_in_runtime(&state.runtime_threads, &state.sessions_dir, req).await
}

pub(crate) async fn save_session_in_runtime(
    runtime: &std::sync::Arc<RuntimeThreadManager>,
    sessions_dir: &std::path::Path,
    req: SaveSessionRequest,
) -> Result<Json<SaveSessionResponse>, ApiError> {
    let _checkpoint_admission = runtime.session_checkpoint_guard().await;
    // Find the thread to save.
    let thread_id = match req.thread_id {
        Some(id) => id,
        None => {
            // Find the most recently updated thread.
            let threads = runtime
                .list_threads(ThreadListFilter::IncludeArchived, Some(100))
                .await
                .map_err(map_thread_err)?;
            threads
                .into_iter()
                .max_by_key(|t| t.updated_at)
                .map(|t| t.id)
                .ok_or_else(|| ApiError::bad_request("No threads to save"))?
        }
    };

    // Get the engine handle (loads the thread into an engine if needed),
    // then request a session snapshot. This reuses the same code path as
    // TUI's `build_session_snapshot`: the engine holds the authoritative
    // messages and token usage, so we don't need to reconstruct from turns.
    let engine = runtime
        .get_engine(&thread_id)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to get engine for thread: {e}")))?;

    let snapshot = engine
        .get_session_snapshot()
        .await
        .map_err(|e| ApiError::internal(format!("Failed to get session snapshot: {e}")))?;

    let manager = SessionManager::new(sessions_dir.to_path_buf())
        .map_err(|e| ApiError::internal(format!("Failed to open sessions dir: {e}")))?;

    // A document another thread is bound to is that thread's conversation.
    // Rebinding this thread onto it would leave the other thread's checkpoint
    // describing a document it no longer owns (#6144). A thread already bound
    // to the same document (a legacy shared link) keeps saving to it.
    if let Some(requested) = req.session_id.as_deref() {
        let own = runtime
            .get_thread(&thread_id)
            .await
            .map_err(map_thread_err)?;
        if own.session_id.as_deref() != Some(requested)
            && let Some(other) = runtime.thread_bound_to_session(requested, &thread_id)
        {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                message: format!(
                    "Session '{requested}' belongs to thread {other}; save this thread without a session_id, or into its own session"
                ),
                code: None,
            });
        }
    }
    // Which document this save writes. A named `session_id` wins. With none,
    // the thread's own document answers it: the one it is bound to (a
    // `resume-thread` from document X runs bound to X, and saving it must
    // update X, not start a second copy under another name), else a new
    // document under the engine's conversation id, which for a Runtime thread
    // is the thread's own id (see `ensure_engine_loaded`) and so cannot
    // collide with another thread's document. Snapshot ownership does not
    // depend on this binding: a thread owns the restore points recorded on
    // its turns.
    let document_id = match req.session_id {
        Some(named) => named,
        None => runtime
            .get_thread(&thread_id)
            .await
            .map_err(map_thread_err)?
            .session_id
            .unwrap_or_else(|| snapshot.session_id.clone()),
    };
    let _lease = reserve_session_write(sessions_dir, &document_id, "save").await?;

    // Build or update the session, mirroring TUI's `build_session_snapshot`.
    // Only `io::ErrorKind::NotFound` falls back to creating a new session;
    // other I/O errors (e.g. PermissionDenied) are propagated so callers
    // don't silently overwrite a corrupt or inaccessible session file.
    let mut session = match manager.load_session(&document_id) {
        Ok(existing) => {
            let mut updated = crate::session_manager::update_session(
                existing,
                &snapshot.messages,
                snapshot.total_tokens,
                snapshot.system_prompt.as_ref(),
            );
            updated.metadata.model = snapshot.model.clone();
            updated.metadata.set_model_provider_route(
                &snapshot.model_provider,
                snapshot.model_provider_id.as_deref(),
            );
            updated.metadata.mode = Some(snapshot.mode.clone());
            updated
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut session = crate::session_manager::create_saved_session_with_id_and_mode(
                document_id.clone(),
                &snapshot.messages,
                &snapshot.model,
                &snapshot.workspace,
                snapshot.total_tokens,
                snapshot.system_prompt.as_ref(),
                Some(snapshot.mode.as_str()),
            );
            session.metadata.set_model_provider_route(
                &snapshot.model_provider,
                snapshot.model_provider_id.as_deref(),
            );
            session
        }
        Err(e) => {
            return Err(ApiError::internal(format!(
                "Failed to load session {document_id}: {e}"
            )));
        }
    };

    persist_thread_cost(runtime, &thread_id, &mut session).await?;

    if runtime.is_acp_host() {
        session.metadata.runtime_store = Some(runtime.session_store_binding());
    }

    // Save the session.
    manager
        .save_session(&session)
        .map_err(|e| ApiError::internal(format!("Failed to save session: {e}")))?;

    // Link the session to the thread so that `ensure_engine_loaded` can
    // restore the full message history (including thinking/tool blocks)
    // from the session file instead of reconstructing from turns.
    let session_handle = session.metadata.id.clone();
    runtime
        .set_thread_session_checkpoint(&thread_id, &session)
        .await
        .map_err(|e| {
            ApiError::internal(format!(
                "Session was saved but its Runtime checkpoint could not be bound: {e}"
            ))
        })?;

    Ok(Json(SaveSessionResponse {
        session_id: session_handle,
        session: session_to_detail(session),
    }))
}

fn total_tokens_from_thread_detail(detail: &ThreadDetail) -> u64 {
    detail
        .turns
        .iter()
        .filter_map(|turn| turn.usage.as_ref())
        .map(|usage| u64::from(usage.input_tokens) + u64::from(usage.output_tokens))
        .sum()
}

fn session_title_override(requested: Option<&str>, thread_title: Option<&str>) -> Option<String> {
    requested
        .and_then(nonempty_title)
        .or_else(|| thread_title.and_then(nonempty_title))
}

fn nonempty_title(title: &str) -> Option<String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(truncate_text(trimmed, 50))
    }
}

pub(super) async fn delete_session(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    // Deletion validates the id (400), refuses an unknown one (404) before
    // creating any lease file, and holds the session's live lease, refusing
    // (409) a document an interactive session holds open: its next autosave,
    // in whichever process holds it, would undo the delete. Taking the lease
    // may retry with short sleeps, so all of it runs off the async worker.
    tokio::task::spawn_blocking(move || {
        let manager = SessionManager::new(state.sessions_dir.clone())
            .map_err(|e| ApiError::internal(format!("Failed to open sessions dir: {e}")))?;
        manager
            .delete_session(&id)
            .map_err(|e| map_session_err(&id, e, "delete"))?;
        // Threads bound to the document keep their turns; drop the dead link
        // so they load from those instead of failing (#6144).
        if let Err(error) = state.runtime_threads.unbind_session_threads(&id) {
            tracing::warn!(session_id = %id, %error, "deleted session's threads were not unbound");
        }
        Ok::<_, ApiError>(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| ApiError::internal("session delete failed"))?
}

/// `GET /v1/sessions/repair`: what the last session-store repair did (#6144).
/// `null` when none has completed.
pub(super) async fn get_session_repair(
    State(state): State<RuntimeApiState>,
) -> Json<Option<crate::session_reconcile::ReconcileSummary>> {
    Json(crate::session_reconcile::last_run(&state.sessions_dir))
}

pub(super) fn session_to_detail(session: SavedSession) -> SessionDetailResponse {
    let messages: Vec<Value> = session
        .messages
        .iter()
        .map(|msg| {
            let content_blocks: Vec<Value> = msg
                .content
                .iter()
                .map(|block| match block {
                    codewhale_models::ContentBlock::Text { text, .. } => {
                        json!({ "type": "text", "text": text })
                    }
                    codewhale_models::ContentBlock::Thinking { thinking, .. } => {
                        json!({ "type": "thinking", "text": thinking })
                    }
                    codewhale_models::ContentBlock::ToolUse {
                        id,
                        name,
                        input,
                        caller, ..} => {
                        let mut obj =
                            json!({ "type": "tool_use", "id": id, "name": name, "input": input });
                        if let Some(caller) = caller {
                            obj["caller"] = json!(caller);
                        }
                        obj
                    }
                    codewhale_models::ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                        content_blocks,
                        ..
                    } => {
                        let mut obj = json!({ "type": "tool_result", "tool_use_id": tool_use_id });
                        if let Some(cbs) = content_blocks {
                            obj["content_blocks"] = json!(cbs);
                            if !content.is_empty() {
                                obj["content"] = json!(content);
                            }
                        } else {
                            obj["content"] = json!(content);
                        }
                        if let Some(e) = is_error {
                            obj["is_error"] = json!(e);
                        }
                        obj
                    }
                    codewhale_models::ContentBlock::ServerToolUse { id, name, input } => {
                        json!({ "type": "tool_use", "id": id, "name": name, "input": input })
                    }
                    codewhale_models::ContentBlock::ToolSearchToolResult {
                        tool_use_id,
                        content,
                    } => {
                        json!({ "type": "tool_result", "tool_use_id": tool_use_id, "content": content })
                    }
                    codewhale_models::ContentBlock::CodeExecutionToolResult {
                        tool_use_id,
                        content,
                    } => {
                        json!({ "type": "tool_result", "tool_use_id": tool_use_id, "content": content })
                    }
                    codewhale_models::ContentBlock::ImageUrl { .. } => Value::Null,
                })
                .collect();
            json!({
                "role": msg.role,
                "content": content_blocks,
            })
        })
        .collect();
    SessionDetailResponse {
        metadata: session.metadata,
        messages,
        system_prompt: session.system_prompt,
        turn_outcomes: session.turn_outcomes,
    }
}

fn map_session_err(id: &str, err: std::io::Error, action: &str) -> ApiError {
    match err.kind() {
        std::io::ErrorKind::NotFound => ApiError::not_found(format!("Session '{id}' not found")),
        std::io::ErrorKind::InvalidData => {
            ApiError::bad_request(format!("Failed to parse session '{id}': {err}"))
        }
        std::io::ErrorKind::InvalidInput => {
            ApiError::bad_request(format!("Invalid session id '{id}'"))
        }
        // The session is open in an interactive Codewhale session, which holds
        // the authoritative copy in memory. Fail closed with a typed conflict
        // rather than write something its next autosave would revert.
        std::io::ErrorKind::ResourceBusy => ApiError {
            status: StatusCode::CONFLICT,
            message: err.to_string(),
            code: None,
        },
        _ => ApiError::internal(format!("Failed to {action} session '{id}': {err}")),
    }
}

fn map_resume_thread_create_err(err: anyhow::Error) -> ApiError {
    let reason = err.to_string();
    let message = format!("Failed to create thread: {reason}");
    if reason.starts_with("saved session has an empty provider identity")
        || reason.starts_with("saved session requires custom provider")
        || reason.starts_with("legacy session records only the generic `custom` provider kind")
        || reason.starts_with("legacy `provider = \"custom\"`")
    {
        ApiError::bad_request(message)
    } else {
        // Thread-store writes, event persistence, and other runtime failures
        // are server-side faults; never disguise them as a client config error.
        ApiError::internal(message)
    }
}

#[cfg(test)]
mod session_query_tests {
    use super::*;

    fn query(
        include_archived: Option<bool>,
        archived_only: Option<bool>,
        sort: Option<&str>,
        workspace: Option<&str>,
        limit: Option<usize>,
    ) -> SessionsQuery {
        SessionsQuery {
            limit,
            search: Some("whale".to_string()),
            include_archived,
            archived_only,
            workspace: workspace.map(PathBuf::from),
            sort: sort.map(str::to_string),
        }
    }

    #[test]
    fn archive_params_resolve_like_the_threads_routes() {
        assert_eq!(
            projection_query(&query(None, None, None, None, None)).filter,
            SessionListFilter::ActiveOnly
        );
        assert_eq!(
            projection_query(&query(Some(true), None, None, None, None)).filter,
            SessionListFilter::IncludeArchived
        );
        assert_eq!(
            projection_query(&query(Some(true), Some(true), None, None, None)).filter,
            SessionListFilter::ArchivedOnly
        );
    }

    #[test]
    fn sort_and_workspace_scope_flow_through_and_bad_sorts_fall_back() {
        let projected = projection_query(&query(None, None, Some("name"), Some("/repo"), Some(9)));
        assert_eq!(projected.sort, SessionSortMode::Name);
        // `Path` in this module is `axum::extract::Path`; spell out the std one.
        assert_eq!(
            projected.workspace_scope.as_deref(),
            Some(std::path::Path::new("/repo"))
        );
        assert_eq!(projected.limit, 9);
        assert_eq!(projected.search, "whale");

        // An unknown sort must not fail the request — a stale client should
        // still get a listing, just in the default order.
        assert_eq!(
            projection_query(&query(None, None, Some("nonsense"), None, None)).sort,
            SessionSortMode::Recent
        );
    }

    #[test]
    fn limit_is_clamped_at_both_ends() {
        assert_eq!(
            projection_query(&query(None, None, None, None, Some(0))).limit,
            1
        );
        assert_eq!(
            projection_query(&query(None, None, None, None, Some(10_000))).limit,
            500
        );
        // Absent limit keeps the historical page size.
        assert_eq!(
            projection_query(&query(None, None, None, None, None)).limit,
            50
        );
    }

    #[test]
    fn absent_workspace_means_every_workspace() {
        assert!(
            projection_query(&query(None, None, None, None, None))
                .workspace_scope
                .is_none(),
            "the API must not silently scope to the runtime's own CWD"
        );
    }
}

#[cfg(test)]
mod resume_thread_error_tests {
    use super::*;

    #[test]
    fn provider_config_errors_are_client_errors_but_storage_errors_stay_internal() {
        let provider = map_resume_thread_create_err(anyhow::anyhow!(
            "saved session requires custom provider 'lm-studio', but `[providers.lm-studio]` is missing"
        ));
        assert_eq!(provider.status, StatusCode::BAD_REQUEST);

        let storage = map_resume_thread_create_err(anyhow::anyhow!(
            "Failed to save runtime thread: permission denied"
        ));
        assert_eq!(storage.status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}

// ---------------------------------------------------------------------------
// Session artifacts (#6163): the oversized tool outputs a session recorded as
// `ArtifactRecord`s live under `sessions/<id>/artifacts/`. These routes list
// the records a saved session carries and read one artifact through the same
// confined opener the workspace file routes use. Nothing is copied anywhere.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(super) struct SessionArtifactSummary {
    id: String,
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_type: Option<String>,
    kind: crate::artifacts::ArtifactKind,
    tool_call_id: String,
    tool_name: String,
    created_at: chrono::DateTime<chrono::Utc>,
    byte_size: u64,
    preview: String,
    /// Session-relative storage path with `/` separators.
    path: String,
}

#[derive(Debug, Serialize)]
pub(super) struct SessionArtifactsResponse {
    session_id: String,
    artifacts: Vec<SessionArtifactSummary>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionArtifactReadQuery {
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(super) struct SessionArtifactReadResponse {
    artifact: SessionArtifactSummary,
    size: u64,
    revision: String,
    offset: usize,
    bytes: usize,
    truncated: bool,
    encoding: &'static str,
    content: String,
}

fn artifact_summary(record: &crate::artifacts::ArtifactRecord) -> SessionArtifactSummary {
    SessionArtifactSummary {
        id: record.id.clone(),
        session_id: record.session_id.clone(),
        content_type: None,
        kind: record.kind.clone(),
        tool_call_id: record.tool_call_id.clone(),
        tool_name: record.tool_name.clone(),
        created_at: record.created_at,
        byte_size: record.byte_size,
        preview: record.preview.clone(),
        path: crate::artifacts::format_artifact_relative_path(&record.storage_path),
    }
}

pub(super) async fn list_session_artifacts(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<SessionArtifactsResponse>, ApiError> {
    let manager = SessionManager::new(state.sessions_dir.clone())
        .map_err(|e| ApiError::internal(format!("Failed to open sessions dir: {e}")))?;
    let session = manager
        .load_session(&id)
        .map_err(|e| map_session_err(&id, e, "read"))?;
    Ok(Json(SessionArtifactsResponse {
        session_id: session.metadata.id.clone(),
        artifacts: session.artifacts.iter().map(artifact_summary).collect(),
    }))
}

pub(super) async fn read_session_artifact(
    State(state): State<RuntimeApiState>,
    Path((id, artifact_id)): Path<(String, String)>,
    Query(query): Query<SessionArtifactReadQuery>,
) -> Result<Json<SessionArtifactReadResponse>, ApiError> {
    let (offset, limit) = super::workspace::parse_read_window(query.offset, query.limit)?;
    tokio::task::spawn_blocking(move || {
        read_session_artifact_window(&state.sessions_dir, &id, &artifact_id, offset, limit)
    })
    .await
    .map_err(|_| ApiError::internal("session artifact read failed"))?
    .map(Json)
}

fn read_session_artifact_window(
    sessions_dir: &std::path::Path,
    id: &str,
    artifact_id: &str,
    offset: usize,
    limit: usize,
) -> Result<SessionArtifactReadResponse, ApiError> {
    let resolved = resolve_session_artifact(
        sessions_dir,
        id,
        artifact_id,
        ArtifactAuthority::SavedSession,
    )?;
    let summary = resolved
        .summary
        .ok_or_else(|| ApiError::internal("saved-session artifact has no summary"))?;
    let read = resolved.read;
    let (window, truncated) = super::workspace::read_window(&read.bytes, offset, limit);
    let (encoding, content) = super::workspace::encode_window(window);
    Ok(SessionArtifactReadResponse {
        artifact: summary,
        size: read.size,
        revision: read.revision,
        offset: offset.min(read.bytes.len()),
        bytes: window.len(),
        truncated,
        encoding,
        content,
    })
}

/// Who vouches that `artifact_id` belongs to session `id`.
pub(super) enum ArtifactAuthority<'a> {
    /// The SavedSession JSON's `artifacts` index.
    SavedSession,
    /// A runtime turn's recorded reference. A Runtime engine runs under its
    /// thread's own id (#6621), which has no SavedSession index, so the turn
    /// record is the ownership proof: the bytes must sit at `path` and, when `revision` is
    /// known, hash to it.
    TurnRef {
        path: &'a str,
        revision: Option<&'a str>,
    },
}

/// One session artifact's bytes, read through the confined opener.
pub(super) struct ResolvedSessionArtifact {
    /// The index or manifest record; `None` for a turn-ref read of a
    /// non-image artifact, whose caller already holds the reference.
    pub(super) summary: Option<SessionArtifactSummary>,
    pub(super) read: super::workspace::ConfinedFileBytes,
}

/// The one resolver behind both session-artifact reads: the session route
/// (SavedSession authority) and the turn route (turn-ref authority). Both
/// get the same session-id validation, confinement, image-manifest checks
/// and integrity checks.
pub(super) fn resolve_session_artifact(
    sessions_dir: &std::path::Path,
    id: &str,
    artifact_id: &str,
    authority: ArtifactAuthority<'_>,
) -> Result<ResolvedSessionArtifact, ApiError> {
    if !crate::artifacts::is_valid_session_id(id) {
        return Err(ApiError::bad_request("invalid session id"));
    }
    // The reserved image namespace always requires its immutable manifest,
    // even if a later SavedSession index also mentions that handle.
    let image_handle = artifact_id.strip_prefix("art_image_").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    let (summary, relative_path, evidence) = if image_handle {
        let (summary, evidence) = image_evidence_summary(sessions_dir, id, artifact_id)?;
        let path = summary.path.clone();
        (Some(summary), path, Some(evidence))
    } else {
        match &authority {
            ArtifactAuthority::SavedSession => {
                let summary = saved_session_summary(sessions_dir, id, artifact_id)?;
                let path = summary.path.clone();
                (Some(summary), path, None)
            }
            ArtifactAuthority::TurnRef { path, .. } => {
                let relative = PathBuf::from(path);
                if relative.is_absolute() || !crate::fleet::files::path_is_confined(&relative) {
                    return Err(ApiError::forbidden(
                        "artifact reference is not confined to its session",
                    ));
                }
                (None, (*path).to_string(), None)
            }
        }
    };
    let relative = PathBuf::from(id).join(&relative_path);
    let opened = super::workspace::open_confined_file_resolved(sessions_dir, &relative, false)
        .and_then(|file| super::workspace::read_confined_bytes(&file));
    let read = match (opened, &authority) {
        (Ok(read), _) => read,
        // A turn recorded these bytes; their absence means the session
        // directory was pruned since.
        (Err(error), ArtifactAuthority::TurnRef { .. })
            if error.status == StatusCode::NOT_FOUND =>
        {
            return Err(ApiError::gone(
                "this artifact's bytes are no longer stored (its session was pruned)",
            ));
        }
        (Err(error), _) => return Err(error),
    };
    if let Some(evidence) = evidence
        && (read.size != evidence.size_bytes
            || read.revision != evidence.digest
            || crate::image_attach::sniff_media_type(&read.bytes)
                != Some(evidence.content_type.as_str())
            || crate::image_attach::decode_and_guard_image(&read.bytes).is_err())
    {
        return Err(ApiError::bad_request(
            "image evidence integrity check failed",
        ));
    }
    if let ArtifactAuthority::TurnRef {
        revision: Some(expected),
        ..
    } = authority
        && read.revision != expected
    {
        return Err(ApiError::conflict(format!(
            "artifact bytes changed since the turn recorded them; current revision is {}",
            read.revision
        )));
    }
    Ok(ResolvedSessionArtifact { summary, read })
}

fn saved_session_summary(
    sessions_dir: &std::path::Path,
    id: &str,
    artifact_id: &str,
) -> Result<SessionArtifactSummary, ApiError> {
    let manager = SessionManager::new(sessions_dir.to_path_buf())
        .map_err(|e| ApiError::internal(format!("Failed to open sessions dir: {e}")))?;
    let record = match manager.load_session_snapshot(id) {
        Ok(session) if session.metadata.id == id => session
            .artifacts
            .into_iter()
            .find(|record| record.id == artifact_id),
        Ok(_) => return Err(ApiError::forbidden("artifact session owner does not match")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(map_session_err(id, error, "read")),
    };
    let record = record.ok_or_else(|| ApiError::not_found("artifact not found"))?;
    if record.storage_path.is_absolute()
        || !crate::fleet::files::path_is_confined(&record.storage_path)
        || (!record.session_id.is_empty() && record.session_id != id)
    {
        return Err(ApiError::forbidden(
            "artifact record is not confined to its session",
        ));
    }
    let mut summary = artifact_summary(&record);
    summary.session_id = id.to_owned();
    Ok(summary)
}

/// Fresh Engine sessions can publish immutable observations before a
/// SavedSession JSON exists. Only the exact owned image manifest grants
/// access; arbitrary relative paths and other evidence are not a fallback.
fn image_evidence_summary(
    sessions_dir: &std::path::Path,
    id: &str,
    artifact_id: &str,
) -> Result<
    (
        SessionArtifactSummary,
        crate::tools::large_output_router::EvidenceArtifact,
    ),
    ApiError,
> {
    let relative = PathBuf::from(id)
        .join(crate::tools::large_output_router::evidence_metadata_relative_path(artifact_id));
    let file = super::workspace::open_confined_file_resolved(sessions_dir, &relative, false)?;
    let evidence = crate::tools::large_output_router::read_evidence_metadata_file(&file)
        .map_err(|error| super::workspace::map_fs_error(error, "evidence metadata"))?;
    let expected_path =
        PathBuf::from(crate::artifacts::ARTIFACTS_DIR_NAME).join(format!("{artifact_id}.image"));
    if evidence.origin_session != id
        || evidence.handle != artifact_id
        || evidence.storage_path != expected_path
        || evidence.generation != 1
        || evidence.encoding != "binary"
        || evidence.call_id.is_empty()
    {
        return Err(ApiError::forbidden(
            "image evidence owner or path does not match",
        ));
    }
    if evidence.redacted
        || crate::tools::large_output_router::evidence_is_expired(
            &evidence,
            crate::tools::large_output_router::unix_millis_now(),
        )
    {
        return Err(ApiError::forbidden("image evidence is no longer available"));
    }
    if evidence.size_bytes > crate::image_attach::MAX_IMAGE_BYTES as u64
        || !matches!(
            evidence.content_type.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        )
    {
        return Err(ApiError::bad_request("invalid image evidence"));
    }
    let created_at = i64::try_from(evidence.created_at_unix_ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .ok_or_else(|| ApiError::bad_request("invalid evidence timestamp"))?;
    let summary = SessionArtifactSummary {
        id: artifact_id.to_owned(),
        session_id: id.to_owned(),
        content_type: Some(evidence.content_type.clone()),
        kind: crate::artifacts::ArtifactKind::ToolOutput,
        tool_call_id: evidence.call_id.clone(),
        tool_name: evidence.tool_name.clone(),
        created_at,
        byte_size: evidence.size_bytes,
        preview: String::new(),
        path: crate::artifacts::format_artifact_relative_path(&evidence.storage_path),
    };
    Ok((summary, evidence))
}

#[cfg(test)]
mod tool_media_artifact_tests {
    use super::*;
    use crate::tools::large_output_router::{
        EvidenceArtifact, EvidenceRetentionState, evidence_metadata_relative_path, unix_millis_now,
    };
    use base64::Engine as _;

    fn fixture(root: &std::path::Path) -> EvidenceArtifact {
        let id = format!("art_image_{}", "a".repeat(64));
        let relative = PathBuf::from("artifacts").join(format!("{id}.image"));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 1)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let bytes = bytes.into_inner();
        let now = unix_millis_now();
        let evidence = EvidenceArtifact {
            handle: id,
            digest: crate::hashing::sha256_hex(&bytes),
            size_bytes: bytes.len() as u64,
            content_type: "image/png".into(),
            tool_name: "screenshot".into(),
            call_id: "image-call".into(),
            origin_session: "media_owner".into(),
            generation: 1,
            redacted: false,
            encoding: "binary".into(),
            retention_state: EvidenceRetentionState::Live,
            created_at_unix_ms: now,
            retain_until_unix_ms: now + 60_000,
            storage_path: relative,
        };
        std::fs::create_dir_all(root.join("media_owner/artifacts")).unwrap();
        std::fs::write(root.join("media_owner").join(&evidence.storage_path), bytes).unwrap();
        save_manifest(root, &evidence);
        evidence
    }

    fn save_manifest(root: &std::path::Path, evidence: &EvidenceArtifact) {
        std::fs::write(
            root.join("media_owner")
                .join(evidence_metadata_relative_path(&evidence.handle)),
            serde_json::to_vec(evidence).unwrap(),
        )
        .unwrap();
    }

    fn decode(response: &SessionArtifactReadResponse) -> Vec<u8> {
        if response.encoding == "base64" {
            base64::engine::general_purpose::STANDARD
                .decode(&response.content)
                .unwrap()
        } else {
            response.content.as_bytes().to_vec()
        }
    }

    #[test]
    fn tool_media_artifact_reads_without_saved_session_and_retains_window_revision() {
        let temp = tempfile::tempdir().unwrap();
        let evidence = fixture(temp.path());
        assert!(!temp.path().join("media_owner.json").exists());
        let first =
            read_session_artifact_window(temp.path(), "media_owner", &evidence.handle, 0, 7)
                .unwrap();
        let rest =
            read_session_artifact_window(temp.path(), "media_owner", &evidence.handle, 7, 1024)
                .unwrap();
        assert_eq!(first.artifact.session_id, "media_owner");
        assert_eq!(first.artifact.tool_call_id, "image-call");
        assert_eq!(first.artifact.content_type.as_deref(), Some("image/png"));
        assert_eq!(first.revision, rest.revision);
        assert!(first.truncated);
        assert!(!rest.truncated);
        let mut bytes = decode(&first);
        bytes.extend(decode(&rest));
        assert_eq!(crate::hashing::sha256_hex(&bytes), evidence.digest);
        assert_eq!(
            read_session_artifact_window(temp.path(), "other_owner", &evidence.handle, 0, 1024)
                .unwrap_err()
                .status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            read_session_artifact_window(temp.path(), "../media_owner", &evidence.handle, 0, 1024)
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn tool_media_artifact_rejects_wrong_owner_expiry_spoofed_mime_and_changed_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let evidence = fixture(temp.path());
        let mut owner = evidence.clone();
        owner.origin_session = "foreign".into();
        let mut handle = evidence.clone();
        handle.handle = "forged".into();
        let mut path = evidence.clone();
        path.storage_path = PathBuf::from("../outside.png");
        let mut generation = evidence.clone();
        generation.generation = 2;
        for invalid in [owner, handle, path, generation] {
            save_manifest(temp.path(), &invalid);
            assert_eq!(
                read_session_artifact_window(temp.path(), "media_owner", &evidence.handle, 0, 1024)
                    .unwrap_err()
                    .status,
                StatusCode::FORBIDDEN
            );
        }
        let mut expired = evidence.clone();
        expired.retain_until_unix_ms = 0;
        save_manifest(temp.path(), &expired);
        assert_eq!(
            read_session_artifact_window(temp.path(), "media_owner", &evidence.handle, 0, 1024)
                .unwrap_err()
                .status,
            StatusCode::FORBIDDEN
        );
        let mut redacted = evidence.clone();
        redacted.redacted = true;
        save_manifest(temp.path(), &redacted);
        assert_eq!(
            read_session_artifact_window(temp.path(), "media_owner", &evidence.handle, 0, 1024)
                .unwrap_err()
                .status,
            StatusCode::FORBIDDEN
        );
        let mut mime = evidence.clone();
        mime.content_type = "image/jpeg".into();
        save_manifest(temp.path(), &mime);
        assert_eq!(
            read_session_artifact_window(temp.path(), "media_owner", &evidence.handle, 0, 1024)
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST
        );
        save_manifest(temp.path(), &evidence);
        std::fs::write(
            temp.path().join("media_owner").join(&evidence.storage_path),
            b"changed",
        )
        .unwrap();
        assert_eq!(
            read_session_artifact_window(temp.path(), "media_owner", &evidence.handle, 0, 1024)
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST
        );
    }

    #[cfg(unix)]
    #[test]
    fn tool_media_artifact_rejects_manifest_and_payload_symlinks() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let evidence = fixture(temp.path());
        let manifest = temp
            .path()
            .join("media_owner")
            .join(evidence_metadata_relative_path(&evidence.handle));
        let outside = temp.path().join("outside.json");
        std::fs::rename(&manifest, &outside).unwrap();
        symlink(&outside, &manifest).unwrap();
        assert!(
            read_session_artifact_window(temp.path(), "media_owner", &evidence.handle, 0, 1024)
                .is_err()
        );
        std::fs::remove_file(&manifest).unwrap();
        save_manifest(temp.path(), &evidence);
        let payload = temp.path().join("media_owner").join(&evidence.storage_path);
        let outside = temp.path().join("outside.png");
        std::fs::rename(&payload, &outside).unwrap();
        symlink(&outside, &payload).unwrap();
        assert!(
            read_session_artifact_window(temp.path(), "media_owner", &evidence.handle, 0, 1024)
                .is_err()
        );
    }
}

/// Trusted transport creation uses the same lease/checkpoint writer before
/// the first provider call. Ordinary HTTP export still rejects empty history.
pub(crate) async fn initialize_empty_session(
    runtime: &std::sync::Arc<RuntimeThreadManager>,
    sessions_dir: &std::path::Path,
    thread_id: &str,
    session_id: &str,
) -> Result<(), ApiError> {
    let _checkpoint_admission = runtime.session_checkpoint_guard().await;
    let detail = runtime
        .get_thread_detail(thread_id)
        .await
        .map_err(map_thread_err)?;
    if thread_detail_has_live_work(&detail)
        || !detail.turns.is_empty()
        || detail.thread.session_id.is_some()
    {
        return Err(ApiError::conflict(
            "Initial ACP checkpoint requires a new idle, unbound thread",
        ));
    }
    let _lease = reserve_session_write(sessions_dir, session_id, "initialize").await?;
    let manager = SessionManager::new(sessions_dir.to_path_buf())
        .map_err(|error| ApiError::internal(error.to_string()))?;
    match manager.load_session(session_id) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(ApiError::conflict(
                "Initial session identity already exists",
            ));
        }
        Err(error) => return Err(map_session_err(session_id, error, "read")),
    }
    let mut session = create_saved_session_with_id_and_mode(
        session_id.to_string(),
        &[],
        &detail.thread.model,
        &detail.thread.workspace,
        0,
        None,
        Some(&detail.thread.mode),
    );
    stamp_session_provider_from_thread(&runtime.read_config(), &detail, &mut session.metadata)
        .map_err(ApiError::bad_request)?;
    session.metadata.runtime_store = Some(runtime.session_store_binding());
    manager
        .save_session(&session)
        .map_err(|error| ApiError::internal(error.to_string()))?;
    runtime
        .set_thread_session_checkpoint(thread_id, &session)
        .await
        .map_err(|error| {
            ApiError::internal(format!(
                "Initial session was saved but checkpoint binding failed: {error}"
            ))
        })?;
    Ok(())
}
