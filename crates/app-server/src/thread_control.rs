//! Compatibility control projection onto the already held canonical owner.
//! SQLite supplies immutable migration input and committed alias receipts only.
use super::*;
use crate::owner_calls;
use anyhow::{Context as _, bail};
#[cfg(test)]
use axum::Router;
use codewhale_protocol::{
    CanonicalHistoryImportRequest, CanonicalHistoryOptions, CanonicalHistorySource,
    CanonicalThreadMutation, CanonicalThreadMutationRequest, CanonicalThreadOperationKind,
    CanonicalThreadOperationLookup, CanonicalThreadOperationRecovery,
    CanonicalThreadOperationStatus, CanonicalThreadReceipt, CanonicalThreadSnapshot,
    LegacyThreadHistory, MAX_CANONICAL_HISTORY_BYTES, MAX_CANONICAL_HISTORY_ENTRIES,
    RuntimeOwnerReceipt, SessionSource, Thread, ThreadStatus,
};

#[cfg(test)]
#[derive(Debug, thiserror::Error)]
#[error("runtime API returned {status}: {detail}")]
struct HttpFailure {
    status: StatusCode,
    detail: String,
}

#[derive(Debug, thiserror::Error)]
#[error(
    "canonical operation {operation} completed as thread {thread} / session {session}; inspect this result without replay: {source}"
)]
struct CommittedControlFailure {
    operation: String,
    thread: String,
    session: String,
    #[source]
    source: anyhow::Error,
}

fn committed_failure(receipt: &CanonicalThreadReceipt, source: anyhow::Error) -> anyhow::Error {
    CommittedControlFailure {
        operation: receipt.operation_key.clone(),
        thread: receipt.runtime_thread_id.clone(),
        session: receipt.session_id.clone(),
        source,
    }
    .into()
}

fn owner(state: &AppState) -> Result<RuntimeOwnerReceipt> {
    state.captured_owner.clone().context(
        "thread controls require the authenticated canonical owner; no standalone history writer",
    )
}

/// declared length and actual streamed bytes must both fit; nothing is cut.
#[cfg(test)]
pub(super) async fn read_json_response(mut response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|len| len > MAX_CANONICAL_HISTORY_BYTES as u64)
    {
        bail!("canonical response exceeds complete-document bound");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes
            .len()
            .checked_add(chunk.len())
            .is_none_or(|len| len > MAX_CANONICAL_HISTORY_BYTES)
        {
            bail!("canonical response exceeds complete-document bound");
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let detail = String::from_utf8_lossy(&bytes);
        return Err(HttpFailure {
            status,
            detail: detail.trim().to_owned(),
        }
        .into());
    }
    if bytes.is_empty() && status == StatusCode::NO_CONTENT {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).context("invalid canonical Runtime JSON")
}

async fn request(
    owner: &Arc<dyn RuntimeOwnerFrontend>,
    operation: RuntimeOwnerRequest,
) -> Result<Value> {
    let encoded = match &operation {
        RuntimeOwnerRequest::CreateThread(body)
        | RuntimeOwnerRequest::UpdateThread { body, .. }
        | RuntimeOwnerRequest::StartTurn { body, .. } => Some(serde_json::to_vec(body)?.len()),
        RuntimeOwnerRequest::HistoryImport(body) => Some(serde_json::to_vec(body)?.len()),
        RuntimeOwnerRequest::HistoryMutate(body) => Some(serde_json::to_vec(body)?.len()),
        RuntimeOwnerRequest::HistoryLookup(body) => Some(serde_json::to_vec(body)?.len()),
        RuntimeOwnerRequest::HistoryRecover(body) => Some(serde_json::to_vec(body)?.len()),
        _ => None,
    };
    anyhow::ensure!(
        encoded.is_none_or(|len| len <= MAX_CANONICAL_HISTORY_BYTES),
        "encoded canonical control exceeds bound"
    );
    owner_calls::execute(owner, operation).await
}

#[cfg(any(unix, windows))]
async fn store_work<T, F>(work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    codewhale_runtime::runtime_control::owner_work(work).await
}
#[cfg(not(any(unix, windows)))]
async fn store_work<T, F>(_work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    bail!("canonical owner attachment is unsupported on this platform")
}

struct LegacySource {
    metadata: codewhale_state::ThreadMetadata,
    previous: Option<String>,
    receipt: Option<CanonicalThreadReceipt>,
    history: Option<LegacyThreadHistory>,
}

async fn legacy_source(
    state: &AppState,
    key: &str,
    owner: &RuntimeOwnerReceipt,
) -> Result<Option<(StateStore, LegacySource)>> {
    let store = state.runtime.read().await.state_store().clone();
    let read_store = store.clone();
    let key = key.to_owned();
    let owner = owner.clone();
    let source = store_work(move || {
        let Some(metadata) = read_store.get_thread(&key)? else {
            return Ok(None);
        };
        let previous = read_store.get_runtime_thread_link(&key)?;
        let receipt = read_store.get_canonical_runtime_link(&key, &owner)?;
        let history = if receipt.is_none() {
            Some(read_store.snapshot_legacy_thread_history(&key)?)
        } else {
            None
        };
        Ok(Some(LegacySource {
            metadata,
            previous,
            receipt,
            history,
        }))
    })
    .await?;
    Ok(source.map(|source| (store, source)))
}

fn migration_key(history: &LegacyThreadHistory) -> Result<String> {
    // JSON tuple delimiters make this an exact source identity even when a
    // legacy ID contains punctuation. Overlong identities visibly refuse.
    let key = format!(
        "legacy-import:{}",
        serde_json::to_string(&(&history.state_store_id, &history.thread_id))?
    );
    anyhow::ensure!(
        key.len() <= 128 && !key.chars().any(char::is_control),
        "legacy operation identity exceeds the owner's bound; source retained for recovery"
    );
    Ok(key)
}

fn verify_receipt(
    receipt: &CanonicalThreadReceipt,
    owner: &RuntimeOwnerReceipt,
    operation: &str,
) -> Result<()> {
    anyhow::ensure!(
        receipt.version == 1
            && receipt.data_dir == owner.data_dir
            && receipt.execution_scope == owner.execution_scope
            && receipt.operation_key == operation,
        "canonical import receipt does not match the selected owner operation"
    );
    Ok(())
}

fn check_workspace(state: &AppState, workspace: &Path) -> Result<()> {
    if let Some(selected) = &state.frontend_workspace {
        anyhow::ensure!(
            workspace == selected,
            "thread belongs to another selected frontend workspace; attach its owner scope"
        );
    }
    Ok(())
}

/// CAS. A cancelled waiter does not cancel publication of a committed result.
pub(super) async fn resolve(
    state: &AppState,
    key: &str,
    execution: bool,
) -> Result<(String, PathBuf)> {
    let state = state.clone();
    let key = key.to_owned();
    anyhow::ensure!(
        !key.is_empty() && key.len() <= 1024 && !key.chars().any(char::is_control),
        "invalid thread identity"
    );
    tokio::spawn(async move { resolve_owned(&state, &key, execution).await })
        .await
        .context("canonical resolution task failed")?
}

async fn resolve_owned(state: &AppState, key: &str, execution: bool) -> Result<(String, PathBuf)> {
    let owner = owner(state)?;
    let source = legacy_source(state, key, &owner).await?;
    let native = owner_calls::captured(state)?;
    let id = if let Some((store, source)) = source {
        if execution || source.receipt.is_none() {
            check_workspace(state, &source.metadata.cwd)?;
        }
        if let Some(receipt) = source.receipt {
            receipt.runtime_thread_id
        } else {
            // Existing targets retain their canonical active branch; the
            // owner admits complete legacy branches without replacing work.
            let history = source.history.context("legacy source snapshot missing")?;
            let operation = migration_key(&history)?;
            let request_body = CanonicalHistoryImportRequest {
                version: 1,
                target_runtime_thread_id: source.previous.clone(),
                operation_key: operation.clone(),
                expected_data_dir: owner.data_dir.clone(),
                expected_execution_scope: owner.execution_scope.clone(),
                workspace: source.metadata.cwd,
                model: None,
                history: history.clone(),
            };
            let value = request(&native, RuntimeOwnerRequest::HistoryImport(request_body)).await
                .with_context(|| format!("canonical import {operation} may have committed; inspect or retry the same operation, never remint"))?;
            let receipt: CanonicalThreadReceipt = serde_json::from_value(value)?;
            verify_receipt(&receipt, &owner, &operation)?;
            let publication = receipt.clone();
            let thread_key = key.to_owned();
            let previous = source.previous;
            store_work(move || store.publish_canonical_runtime_link(&thread_key, previous.as_deref(), &history, &owner, &publication)).await
                .context("canonical result committed but compatibility publication failed; result and source retained for recovery")?;
            receipt.runtime_thread_id
        }
    } else {
        key.to_owned()
    };
    let value = request(
        &native,
        RuntimeOwnerRequest::GetThread {
            thread_id: id.clone(),
        },
    )
    .await
    .context(
        "canonical target is unavailable; existing identity retained, no replacement thread",
    )?;
    let record = value.get("thread").unwrap_or(&value);
    anyhow::ensure!(
        record.get("id").and_then(Value::as_str) == Some(id.as_str()),
        "canonical resolution returned another target identity"
    );
    let thread = project_thread(record, key)?;
    if execution {
        check_workspace(state, &thread.cwd)?;
    }
    Ok((id, thread.cwd))
}

fn timestamp(record: &Value, key: &str) -> Result<i64> {
    Ok(chrono::DateTime::parse_from_rfc3339(
        record
            .get(key)
            .and_then(Value::as_str)
            .context("canonical timestamp missing")?,
    )?
    .timestamp())
}

fn project_thread(record: &Value, public_id: &str) -> Result<Thread> {
    let id = record
        .get("id")
        .and_then(Value::as_str)
        .context("canonical thread identity missing")?;
    anyhow::ensure!(!id.is_empty(), "canonical thread identity missing");
    Ok(Thread {
        id: public_id.to_owned(),
        preview: String::new(),
        ephemeral: false,
        model_provider: record
            .get("model_provider_id")
            .or_else(|| record.get("model_provider"))
            .and_then(Value::as_str)
            .context("canonical provider identity missing")?
            .to_owned(),
        created_at: timestamp(record, "created_at")?,
        updated_at: timestamp(record, "updated_at")?,
        status: if record.get("archived").and_then(Value::as_bool) == Some(true) {
            ThreadStatus::Archived
        } else {
            ThreadStatus::Idle
        },
        path: None,
        cwd: serde_json::from_value(
            record
                .get("workspace")
                .cloned()
                .context("canonical workspace missing")?,
        )?,
        cli_version: env!("CARGO_PKG_VERSION").to_owned(),
        source: SessionSource::Api,
        name: record
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn response(id: String) -> ThreadResponse {
    ThreadResponse {
        thread_id: id,
        status: "ok".into(),
        thread: None,
        threads: Vec::new(),
        goal: None,
        model: None,
        model_provider: None,
        cwd: None,
        approval_policy: None,
        sandbox: None,
        events: Vec::new(),
        data: json!({}),
    }
}

pub(super) async fn loaded(state: &AppState) -> Result<ThreadResponse> {
    let native = owner_calls::captured(state)?;
    let rows = request(&native, RuntimeOwnerRequest::LoadedThreads).await?;
    let rows = rows
        .as_array()
        .context("loaded thread snapshot is not an array")?;
    anyhow::ensure!(
        rows.len() <= MAX_CANONICAL_HISTORY_ENTRIES,
        "loaded thread snapshot exceeds bound"
    );
    let active = running_ids(&native).await?;
    let mut result = response("loaded".into());
    let mut ids = std::collections::HashSet::new();
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .context("loaded thread identity missing")?;
        anyhow::ensure!(
            !id.is_empty()
                && id.len() <= 1024
                && !id.chars().any(char::is_control)
                && ids.insert(id.to_owned()),
            "loaded thread identity invalid or repeated"
        );
        let mut thread = project_thread(row, id)?;
        if active.contains(id) && thread.status != ThreadStatus::Archived {
            thread.status = ThreadStatus::Running;
        }
        result.threads.push(thread);
    }
    result.threads.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(result)
}

fn wire_thread(thread: Thread) -> codewhale_app_server_protocol::rpc::ThreadListItem {
    use codewhale_app_server_protocol::rpc::{ThreadListItem, ThreadListSource, ThreadListStatus};
    ThreadListItem {
        session_id: thread.id.clone(),
        id: thread.id,
        preview: thread.preview,
        ephemeral: thread.ephemeral,
        model_provider: thread.model_provider,
        created_at: thread.created_at,
        updated_at: thread.updated_at,
        status: match thread.status {
            ThreadStatus::Running => ThreadListStatus::Active {
                active_flags: Vec::new(),
            },
            ThreadStatus::Idle | ThreadStatus::Completed | ThreadStatus::Paused => {
                ThreadListStatus::Idle
            }
            ThreadStatus::Archived => ThreadListStatus::NotLoaded,
            ThreadStatus::Failed => ThreadListStatus::SystemError,
        },
        cwd: thread.cwd.to_string_lossy().into_owned(),
        cli_version: thread.cli_version,
        source: match thread.source {
            SessionSource::Interactive => ThreadListSource::Cli,
            SessionSource::Api => ThreadListSource::AppServer,
            SessionSource::Resume | SessionSource::Fork | SessionSource::Unknown => {
                ThreadListSource::Unknown
            }
        },
        project_id: None,
        turns: Vec::new(),
        name: thread.name,
        executor: None,
        environment: None,
    }
}

pub(super) fn list_wire(
    response: ThreadResponse,
) -> codewhale_app_server_protocol::rpc::ThreadListResponse {
    codewhale_app_server_protocol::rpc::ThreadListResponse {
        data: response.threads.into_iter().map(wire_thread).collect(),
        next_cursor: None,
        backwards_cursor: None,
    }
}

#[derive(serde::Deserialize)]
struct WireTurnRow {
    id: String,
    status: String,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    ended_at: Option<chrono::DateTime<chrono::Utc>>,
    duration_ms: Option<u64>,
    error: Option<String>,
    #[serde(default)]
    item_ids: Vec<String>,
}

#[derive(serde::Deserialize)]
struct WireItemRow {
    id: String,
    turn_id: String,
    kind: String,
    summary: String,
    detail: Option<String>,
}

fn wire_turns(data: &Value) -> Result<Vec<codewhale_app_server_protocol::rpc::ThreadTurn>> {
    use codewhale_app_server_protocol::rpc::{
        ThreadTurn, ThreadTurnError, ThreadTurnItem, ThreadTurnItemsView, ThreadTurnStatus,
        ThreadUserInput,
    };
    let turns: Vec<WireTurnRow> =
        serde_json::from_value(data.get("turns").cloned().unwrap_or(Value::Null))
            .context("canonical turn records are malformed")?;
    let items: Vec<WireItemRow> =
        serde_json::from_value(data.get("items").cloned().unwrap_or(Value::Null))
            .context("canonical item records are malformed")?;
    anyhow::ensure!(
        turns.len() <= MAX_CANONICAL_HISTORY_ENTRIES
            && items.len() <= MAX_CANONICAL_HISTORY_ENTRIES,
        "canonical turn history exceeds bound"
    );
    turns
        .into_iter()
        .map(|turn| {
            let mut own: Vec<&WireItemRow> = items
                .iter()
                .filter(|item| item.turn_id == turn.id)
                .collect();
            own.sort_by_key(|item| {
                turn.item_ids
                    .iter()
                    .position(|id| *id == item.id)
                    .unwrap_or(usize::MAX)
            });
            let mut complete = true;
            let mut mapped = Vec::with_capacity(own.len());
            for item in own {
                let text = item.detail.clone().unwrap_or_else(|| item.summary.clone());
                let id = item.id.clone();
                match item.kind.as_str() {
                    "user_message" => mapped.push(ThreadTurnItem::UserMessage {
                        id,
                        content: if text.is_empty() {
                            Vec::new()
                        } else {
                            vec![ThreadUserInput::Text { text }]
                        },
                    }),
                    "agent_message" => mapped.push(ThreadTurnItem::AgentMessage { id, text }),
                    "agent_reasoning" => mapped.push(ThreadTurnItem::Reasoning {
                        id,
                        content: item.detail.iter().cloned().collect(),
                    }),
                    "context_compaction" => mapped.push(ThreadTurnItem::ContextCompaction { id }),
                    _ => complete = false,
                }
            }
            Ok(ThreadTurn {
                id: turn.id,
                items: mapped,
                items_view: if complete {
                    ThreadTurnItemsView::Full
                } else {
                    ThreadTurnItemsView::Summary
                },
                status: match turn.status.as_str() {
                    "completed" => ThreadTurnStatus::Completed,
                    "failed" => ThreadTurnStatus::Failed,
                    "interrupted" | "canceled" => ThreadTurnStatus::Interrupted,
                    "queued" | "in_progress" => ThreadTurnStatus::InProgress,
                    other => bail!("unknown canonical turn status {other}"),
                },
                error: turn.error.map(|message| ThreadTurnError { message }),
                started_at: turn.started_at.map(|at| at.timestamp()),
                completed_at: turn.ended_at.map(|at| at.timestamp()),
                duration_ms: turn.duration_ms.and_then(|ms| i64::try_from(ms).ok()),
            })
        })
        .collect()
}

pub(super) fn start_params(
    params: codewhale_app_server_protocol::rpc::ThreadStartParams,
) -> std::result::Result<codewhale_protocol::ThreadStartParams, DispatchError> {
    let refused: Vec<&str> = [
        ("ephemeral", params.ephemeral == Some(true)),
        ("approvalPolicy", params.approval_policy.is_some()),
        ("approvalsReviewer", params.approvals_reviewer.is_some()),
        ("sandbox", params.sandbox.is_some()),
        ("config", params.config.is_some()),
        ("baseInstructions", params.base_instructions.is_some()),
        (
            "developerInstructions",
            params.developer_instructions.is_some(),
        ),
        ("serviceName", params.service_name.is_some()),
        ("serviceTier", params.service_tier.is_some()),
        ("sessionStartSource", params.session_start_source.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, set)| set.then_some(name))
    .collect();
    if !refused.is_empty() {
        return Err(DispatchError {
            wire: RpcErrorDetails {
                code: -32602,
                message: format!(
                    "thread/start cannot honor {}: threads are durable and tool-free with a fixed read-only sandbox, and these settings are not applied",
                    refused.join(", ")
                ),
                data: Some(json!({
                    "error": "unsupported_thread_start_parameter",
                    "parameters": refused,
                })),
            },
            transport_error: None,
        });
    }
    Ok(codewhale_protocol::ThreadStartParams {
        operation_key: Some(
            params
                .operation_key
                .unwrap_or_else(|| Uuid::new_v4().to_string()),
        ),
        model: params.model,
        model_provider: params.model_provider,
        cwd: params.cwd.map(PathBuf::from),
        persist_extended_history: false,
    })
}

pub(super) fn start_wire(
    response: ThreadResponse,
) -> Result<codewhale_app_server_protocol::rpc::ThreadStartResponse> {
    let thread = wire_thread(
        response
            .thread
            .context("canonical start returned no thread")?,
    );
    Ok(codewhale_app_server_protocol::rpc::ThreadStartResponse {
        model: response
            .model
            .context("canonical start returned no model")?,
        model_provider: thread.model_provider.clone(),
        cwd: thread.cwd.clone(),
        approval_policy: "never".to_owned(),
        approvals_reviewer: "user".to_owned(),
        sandbox: codewhale_app_server_protocol::rpc::ThreadSandboxPolicy::ReadOnly,
        thread,
    })
}

pub(super) fn read_wire(
    response: ThreadResponse,
    include_turns: bool,
) -> Result<codewhale_app_server_protocol::rpc::ThreadReadResponse> {
    let thread = response
        .thread
        .context("canonical read returned no thread")?;
    let mut thread = wire_thread(thread);
    if include_turns {
        thread.turns = wire_turns(&response.data)?
            .into_iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<_, _>>()?;
    }
    Ok(codewhale_app_server_protocol::rpc::ThreadReadResponse { thread })
}

async fn list(
    state: &AppState,
    params: codewhale_protocol::ThreadListParams,
) -> Result<ThreadResponse> {
    let owner = owner(state)?;
    let store = state.runtime.read().await.state_store().clone();
    let legacy = store_work(move || {
        let rows = store.list_threads(codewhale_state::ThreadListFilters {
            include_archived: true,
            limit: Some(MAX_CANONICAL_HISTORY_ENTRIES + 1),
        })?;
        anyhow::ensure!(
            rows.len() <= MAX_CANONICAL_HISTORY_ENTRIES,
            "legacy metadata list exceeds bound"
        );
        rows.into_iter()
            .map(|row| {
                let receipt = store.get_canonical_runtime_link(&row.id, &owner)?;
                Ok((row, receipt))
            })
            .collect::<Result<Vec<_>>>()
    })
    .await?;
    let limit = params.limit.unwrap_or(50);
    anyhow::ensure!(
        limit <= MAX_CANONICAL_HISTORY_ENTRIES,
        "thread list exceeds bound"
    );
    let native = owner_calls::captured(state)?;
    let rows = request(
        &native,
        RuntimeOwnerRequest::ListThreads {
            include_archived: true,
            limit: MAX_CANONICAL_HISTORY_ENTRIES + 1,
        },
    )
    .await?;
    let rows = rows
        .as_array()
        .context("canonical thread list is not an array")?;
    anyhow::ensure!(
        rows.len() <= MAX_CANONICAL_HISTORY_ENTRIES,
        "canonical thread list exceeds bound"
    );
    let mut canonical = HashMap::new();
    let active = running_ids(&native).await?;
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .context("canonical list identity missing")?;
        anyhow::ensure!(
            canonical.insert(id.to_owned(), row).is_none(),
            "duplicate canonical list identity"
        );
    }
    let mut result = response("list".into());
    let mut aliased = std::collections::HashSet::new();
    for (metadata, receipt) in legacy {
        if let Some(receipt) = receipt {
            let row = canonical
                .get(&receipt.runtime_thread_id)
                .context("bound compatibility alias has no canonical record; recovery required")?;
            let mut thread = project_thread(row, &metadata.id)?;
            if active.contains(&receipt.runtime_thread_id)
                && thread.status != ThreadStatus::Archived
            {
                thread.status = ThreadStatus::Running;
            }
            thread.preview = metadata.preview;
            thread.source = serde_json::from_value(serde_json::to_value(metadata.source)?)?;
            if params.include_archived || thread.status != ThreadStatus::Archived {
                result.threads.push(thread);
            }
            aliased.insert(receipt.runtime_thread_id);
        } else {
            // Read projection only: listing never imports or mutates history.
            if params.include_archived || !metadata.archived {
                result
                    .threads
                    .push(serde_json::from_value(serde_json::to_value(metadata)?)?);
            }
        }
    }
    for (id, row) in canonical {
        if !aliased.contains(&id) {
            let mut thread = project_thread(row, &id)?;
            if active.contains(&id) && thread.status != ThreadStatus::Archived {
                thread.status = ThreadStatus::Running;
            }
            if params.include_archived || thread.status != ThreadStatus::Archived {
                result.threads.push(thread);
            }
        }
    }
    anyhow::ensure!(
        result.threads.len() <= MAX_CANONICAL_HISTORY_ENTRIES,
        "combined thread list exceeds bound"
    );
    result.threads.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    result.threads.truncate(limit); // Explicit caller list limit; history is never truncated.
    Ok(result)
}

pub(super) async fn handle(
    state: &AppState,
    request_value: ThreadRequest,
) -> std::result::Result<ThreadResponse, DispatchError> {
    let key = match &request_value {
        ThreadRequest::Read(p) => Some(p.thread_id.as_str()),
        ThreadRequest::SetName(p) => Some(p.thread_id.as_str()),
        ThreadRequest::Archive { thread_id } | ThreadRequest::Unarchive { thread_id } => {
            Some(thread_id.as_str())
        }
        ThreadRequest::GoalGet(p) => Some(p.thread_id.as_str()),
        ThreadRequest::GoalSet(p) => Some(p.thread_id.as_str()),
        ThreadRequest::GoalClear(p) => Some(p.thread_id.as_str()),
        ThreadRequest::Resume(p) => Some(p.thread_id.as_str()),
        ThreadRequest::Fork(p) => Some(p.thread_id.as_str()),
        ThreadRequest::GoalRecordProgress(p) => Some(p.thread_id.as_str()),
        ThreadRequest::Message { thread_id, .. } => Some(thread_id.as_str()),
        ThreadRequest::Create { .. } | ThreadRequest::Start(_) | ThreadRequest::List(_) => None,
    }
    .map(str::to_owned);
    let result = handle_owned(state, request_value)
        .await
        .map_err(|error| rpc_error(error, key.as_deref()))?;
    if serde_json::to_vec(&result)
        .map_err(|e| DispatchError::internal(e.to_string()))?
        .len()
        > MAX_CANONICAL_HISTORY_BYTES
    {
        let message = "complete thread response exceeds transport bound; no truncation";
        let error = result
            .data
            .get("receipt")
            .and_then(|value| serde_json::from_value::<CanonicalThreadReceipt>(value.clone()).ok())
            .map(|receipt| committed_failure(&receipt, anyhow!(message)))
            .unwrap_or_else(|| anyhow!(message));
        return Err(DispatchError::runtime_unavailable(format!("{error:#}")));
    }
    Ok(result)
}

pub(super) fn rpc_error(error: anyhow::Error, key: Option<&str>) -> DispatchError {
    if error.downcast_ref::<CommittedControlFailure>().is_some() {
        DispatchError::runtime_unavailable(format!("{error:#}"))
    } else if error
        .downcast_ref::<RuntimeOwnerFailure>()
        .is_some_and(|error| error.status == StatusCode::NOT_FOUND.as_u16())
    {
        DispatchError::thread_not_found(key.unwrap_or("unknown"))
    } else {
        DispatchError::runtime_unavailable(format!("{error:#}"))
    }
}

async fn running_ids(
    native: &Arc<dyn RuntimeOwnerFrontend>,
) -> Result<std::collections::HashSet<String>> {
    let value = request(native, RuntimeOwnerRequest::RunningThreads).await?;
    let rows = value
        .as_array()
        .context("canonical running list is not an array")?;
    anyhow::ensure!(
        rows.len() <= MAX_CANONICAL_HISTORY_ENTRIES,
        "canonical running list exceeds bound"
    );
    rows.iter()
        .map(|row| {
            row.get("thread_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .context("canonical running identity missing")
        })
        .collect()
}

fn operation_key(key: Option<String>) -> Result<String> {
    let key = key.context("canonical mutation requires a client-captured operation_key; retain it when retrying an uncertain outcome")?;
    anyhow::ensure!(
        !key.is_empty() && key.len() <= 128 && !key.chars().any(char::is_control),
        "invalid canonical operation_key"
    );
    Ok(key)
}

fn selected_workspace(state: &AppState, explicit: Option<PathBuf>) -> Result<PathBuf> {
    let workspace = explicit.or_else(|| state.frontend_workspace.clone()).context(
        "canonical mutation needs the owner's acknowledged workspace or an explicit checked selection",
    )?;
    check_workspace(state, &workspace)?;
    Ok(workspace)
}

async fn full_snapshot(
    state: &AppState,
    native: &Arc<dyn RuntimeOwnerFrontend>,
    id: &str,
) -> Result<CanonicalThreadSnapshot> {
    let snapshot: CanonicalThreadSnapshot = serde_json::from_value(
        request(
            native,
            RuntimeOwnerRequest::HistorySnapshot {
                thread_id: id.to_owned(),
            },
        )
        .await?,
    )?;
    let selected = owner(state)?;
    anyhow::ensure!(
        snapshot.version == 1
            && snapshot.data_dir == selected.data_dir
            && snapshot.execution_scope == selected.execution_scope
            && snapshot.runtime_thread_id == id,
        "canonical full history response has another owner/thread binding"
    );
    Ok(snapshot)
}

async fn mutate(
    state: &AppState,
    operation: String,
    workspace: PathBuf,
    mutation: CanonicalThreadMutation,
    status: &str,
    public_alias: Option<&str>,
) -> Result<ThreadResponse> {
    let owner = owner(state)?;
    check_workspace(state, &workspace)?;
    let native = owner_calls::captured(state)?;
    let body = CanonicalThreadMutationRequest {
        version: 1,
        operation_key: operation.clone(),
        expected_data_dir: owner.data_dir.clone(),
        expected_execution_scope: owner.execution_scope.clone(),
        workspace,
        mutation,
    };
    let value = request(&native, RuntimeOwnerRequest::HistoryMutate(body))
    .await
    .with_context(|| format!("canonical operation {operation} may have committed; retain this key, no automatic replay or replacement"))?;
    let receipt: CanonicalThreadReceipt = serde_json::from_value(value).with_context(|| {
        format!("canonical operation {operation} returned an invalid result; retain this key, no replacement")
    })?;
    verify_receipt(&receipt, &owner, &operation).with_context(|| {
        format!("canonical operation {operation} returned an unbound receipt; retain this key without replay or replacement")
    })?;
    present_receipt(&native, receipt, status, public_alias).await
}

async fn present_receipt(
    native: &Arc<dyn RuntimeOwnerFrontend>,
    receipt: CanonicalThreadReceipt,
    status: &str,
    public_alias: Option<&str>,
) -> Result<ThreadResponse> {
    let public_id = public_alias.unwrap_or(&receipt.runtime_thread_id);
    let value = request(
        native,
        RuntimeOwnerRequest::GetThread {
            thread_id: receipt.runtime_thread_id.clone(),
        },
    )
    .await
    .map_err(|error| committed_failure(&receipt, error.context("metadata is unavailable")))?;
    let record = value.get("thread").unwrap_or(&value);
    if record.get("id").and_then(Value::as_str) != Some(receipt.runtime_thread_id.as_str()) {
        return Err(committed_failure(
            &receipt,
            anyhow!("another metadata identity was returned"),
        ));
    }
    let mut result = response(public_id.to_owned());
    result.status = status.to_owned();
    result.thread = Some(project_thread(record, public_id).map_err(|error| {
        committed_failure(&receipt, error.context("metadata projection failed"))
    })?);
    result.model = record
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    result.model_provider = result
        .thread
        .as_ref()
        .map(|thread| thread.model_provider.clone());
    result.cwd = result.thread.as_ref().map(|thread| thread.cwd.clone());
    result.data = json!({"receipt":receipt,"thread":record});
    Ok(result)
}

async fn source_identity(state: &AppState, key: &str) -> Result<String> {
    let selected = owner(state)?;
    let store = state.runtime.read().await.state_store().clone();
    let key = key.to_owned();
    store_work(move || {
        if let Some(receipt) = store.get_canonical_runtime_link(&key, &selected)? {
            Ok(receipt.runtime_thread_id)
        } else {
            Ok(store.get_runtime_thread_link(&key)?.unwrap_or(key))
        }
    })
    .await
}

async fn recover_operation(
    state: &AppState,
    operation: &str,
    workspace: &Path,
    kind: CanonicalThreadOperationKind,
    source: Option<&str>,
    status: &str,
    public_alias: Option<&str>,
) -> Result<Option<ThreadResponse>> {
    let selected = owner(state)?;
    let native = owner_calls::captured(state)?;
    let body = CanonicalThreadOperationLookup {
        version: 1,
        operation_key: operation.to_owned(),
        expected_data_dir: selected.data_dir.clone(),
        expected_execution_scope: selected.execution_scope.clone(),
        workspace: workspace.to_owned(),
    };
    let outcome: CanonicalThreadOperationStatus = serde_json::from_value(request(
        &native, RuntimeOwnerRequest::HistoryLookup(body.clone()),
    ).await.with_context(|| format!("canonical key lookup {operation} is unavailable; retain key, no absence inference or replacement"))?)
        .with_context(|| format!("canonical key lookup {operation} returned an invalid result; retain key, no absence inference or replacement"))?;
    let (receipt, association, committed) = match outcome {
        CanonicalThreadOperationStatus::Absent => return Ok(None),
        CanonicalThreadOperationStatus::Pending {
            receipt,
            association,
        } => (receipt, association, false),
        CanonicalThreadOperationStatus::Committed {
            receipt,
            association,
        } => (receipt, association, true),
    };
    verify_receipt(&receipt, &selected, operation)?;
    if association.kind != kind || association.source_runtime_thread_id.as_deref() != source {
        let error = anyhow!("retained key belongs to another action or source; no new operation");
        return Err(if committed {
            committed_failure(&receipt, error)
        } else {
            error.context(format!(
                "canonical operation {operation} is pending; inspect reserved thread {}, no replay",
                receipt.runtime_thread_id
            ))
        });
    }
    if !committed {
        // Lookup is read-only. Explicit retained-key recovery can settle only
        // the owner's already prepared target, never reconstruct this source.
        let recovery = CanonicalThreadOperationRecovery {
            operation: body,
            association: association.clone(),
        };
        let recovered: CanonicalThreadOperationStatus = serde_json::from_value(
            request(
                &native, RuntimeOwnerRequest::HistoryRecover(recovery),
            )
            .await
            .with_context(|| format!(
                "canonical operation {operation} remains uncertain as thread {} / session {}; retain key, no resume, replay or replacement",
                receipt.runtime_thread_id, receipt.session_id
            ))?,
        ).with_context(|| format!(
            "canonical operation {operation} returned an invalid recovery result for reserved thread {} / session {}; retain key, no replacement",
            receipt.runtime_thread_id, receipt.session_id
        ))?;
        match recovered {
            CanonicalThreadOperationStatus::Committed {
                receipt: recovered_receipt,
                association: recovered_association,
            } => {
                anyhow::ensure!(
                    recovered_receipt == receipt && recovered_association == association,
                    "canonical recovery changed retained operation {operation}; reserved thread {} / session {}, no replay",
                    receipt.runtime_thread_id,
                    receipt.session_id
                );
            }
            CanonicalThreadOperationStatus::Pending {
                receipt: pending_receipt,
                association: pending_association,
            } => {
                anyhow::ensure!(
                    pending_receipt == receipt && pending_association == association,
                    "canonical recovery changed retained operation {operation}; no replay"
                );
                bail!(
                    "canonical operation {operation} is pending as thread {} / session {}; no resume, replay or replacement",
                    receipt.runtime_thread_id,
                    receipt.session_id
                );
            }
            CanonicalThreadOperationStatus::Absent => bail!(
                "canonical retained operation {operation} disappeared as thread {} / session {}; no resume, replay or replacement",
                receipt.runtime_thread_id,
                receipt.session_id
            ),
        }
    }
    present_receipt(&native, receipt, status, public_alias)
        .await
        .map(Some)
}

async fn resume_or_fork(
    state: &AppState,
    key: &str,
    operation: String,
    cwd: Option<PathBuf>,
    fork: bool,
    options: CanonicalHistoryOptions,
) -> Result<ThreadResponse> {
    let selected_workspace = selected_workspace(state, cwd.clone())?;
    let expected_source = source_identity(state, key).await?;
    if let Some(result) = recover_operation(
        state,
        &operation,
        &selected_workspace,
        if fork {
            CanonicalThreadOperationKind::Fork
        } else {
            CanonicalThreadOperationKind::Resume
        },
        Some(&expected_source),
        if fork { "forked" } else { "resumed" },
        (!fork).then_some(key),
    )
    .await?
    {
        return Ok(result);
    }
    let (id, workspace) = resolve(state, key, !fork).await?;
    if let Some(cwd) = cwd {
        anyhow::ensure!(
            fork || cwd == workspace,
            "moving a history source to another workspace requires the canonical owner's explicit admission"
        );
    }
    let native = owner_calls::captured(state)?;
    let snapshot = full_snapshot(state, &native, &id).await?;
    let source = CanonicalHistorySource::Thread {
        runtime_thread_id: id,
        expected_document_digest: snapshot.document_digest,
    };
    let mutation = if fork {
        CanonicalThreadMutation::Fork {
            source,
            options,
            selected_entry_id: None,
        }
    } else {
        CanonicalThreadMutation::Resume { source, options }
    };
    // Only resume preserves the old public alias. Fork returns its new actual
    // canonical identity and never creates another SQLite transcript writer.
    mutate(
        state,
        operation,
        if fork { selected_workspace } else { workspace },
        mutation,
        if fork { "forked" } else { "resumed" },
        (!fork).then_some(key),
    )
    .await
}

fn history_parameters(
    params: Value,
) -> Result<(String, String, Option<PathBuf>, CanonicalHistoryOptions)> {
    let mut fields = params
        .as_object()
        .cloned()
        .context("invalid typed history control")?;
    let key = serde_json::from_value(
        fields
            .remove("thread_id")
            .context("thread identity missing")?,
    )?;
    let operation = operation_key(
        fields
            .remove("operation_key")
            .map(serde_json::from_value)
            .transpose()?,
    )?;
    let cwd = fields
        .get("cwd")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?;
    let source_path = fields
        .remove("path")
        .map(serde_json::from_value)
        .transpose()?;
    let offered_history = fields
        .remove("history")
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    // Complete canonical history is durable for both values of the old flag.
    fields.remove("persist_extended_history");
    Ok((
        key,
        operation,
        cwd,
        CanonicalHistoryOptions {
            offered_history,
            overrides: Value::Object(fields),
            source_path,
            expected_session_goal_digest: None,
        },
    ))
}

async fn creation(state: &AppState, request_value: ThreadRequest) -> Result<ThreadResponse> {
    match request_value {
        ThreadRequest::Create { metadata } => {
            let mut config = metadata.as_object().cloned().context(
                "thread/create metadata must carry an operation_key and existing create-thread fields",
            )?;
            let operation = operation_key(
                config
                    .remove("operation_key")
                    .map(serde_json::from_value)
                    .transpose()?,
            )?;
            let explicit = config
                .get("workspace")
                .cloned()
                .map(serde_json::from_value)
                .transpose()?;
            let workspace = selected_workspace(state, explicit)?;
            if let Some(result) = recover_operation(
                state,
                &operation,
                &workspace,
                CanonicalThreadOperationKind::Create,
                None,
                "created",
                None,
            )
            .await?
            {
                return Ok(result);
            }
            config.insert("workspace".into(), serde_json::to_value(&workspace)?);
            mutate(
                state,
                operation,
                workspace,
                CanonicalThreadMutation::Create {
                    config: Value::Object(config),
                },
                "created",
                None,
            )
            .await
        }
        ThreadRequest::Start(params) => {
            let operation = operation_key(params.operation_key)?;
            let workspace = selected_workspace(state, params.cwd)?;
            if let Some(result) = recover_operation(
                state,
                &operation,
                &workspace,
                CanonicalThreadOperationKind::Create,
                None,
                "started",
                None,
            )
            .await?
            {
                return Ok(result);
            }
            let mut config = json!({"workspace":workspace});
            if let Some(model) = params.model {
                config["model"] = json!(model);
            }
            if let Some(provider) = params.model_provider {
                config["model_provider"] = json!(provider);
            }
            // Canonical history is always complete and durable; the legacy
            // extended-history flag cannot turn off branches or receipts.
            mutate(
                state,
                operation,
                workspace,
                CanonicalThreadMutation::Create { config },
                "started",
                None,
            )
            .await
        }
        ThreadRequest::Resume(params) => {
            let (key, operation, cwd, options) = history_parameters(serde_json::to_value(params)?)?;
            resume_or_fork(state, &key, operation, cwd, false, options).await
        }
        ThreadRequest::Fork(params) => {
            let (key, operation, cwd, options) = history_parameters(serde_json::to_value(params)?)?;
            resume_or_fork(state, &key, operation, cwd, true, options).await
        }
        _ => unreachable!("closed creation request dispatch"),
    }
}

async fn handle_owned(state: &AppState, request_value: ThreadRequest) -> Result<ThreadResponse> {
    if let ThreadRequest::List(params) = request_value {
        return list(state, params).await;
    }
    if matches!(
        &request_value,
        ThreadRequest::Create { .. }
            | ThreadRequest::Start(_)
            | ThreadRequest::Resume(_)
            | ThreadRequest::Fork(_)
    ) {
        return creation(state, request_value).await;
    }
    let (key, method, action, body) = match request_value {
        ThreadRequest::Read(p) => (p.thread_id, Method::GET, None, None),
        ThreadRequest::SetName(p) => (
            p.thread_id,
            Method::PATCH,
            None,
            Some(json!({"title":p.name})),
        ),
        ThreadRequest::Archive { thread_id } => (
            thread_id,
            Method::PATCH,
            None,
            Some(json!({"archived":true})),
        ),
        ThreadRequest::Unarchive { thread_id } => (
            thread_id,
            Method::PATCH,
            None,
            Some(json!({"archived":false})),
        ),
        ThreadRequest::GoalGet(p) => (p.thread_id, Method::GET, Some("goal"), None),
        ThreadRequest::GoalSet(_) => {
            bail!("goal execution requires the canonical Runtime interaction API")
        }
        ThreadRequest::GoalClear(p) => (p.thread_id, Method::DELETE, Some("goal"), None),
        ThreadRequest::GoalRecordProgress(_) => bail!(
            "goal progress requires producing Engine usage/time receipts; client deltas are not accounting authority"
        ),
        ThreadRequest::Create { .. }
        | ThreadRequest::Start(_)
        | ThreadRequest::Resume(_)
        | ThreadRequest::Fork(_) => unreachable!("creation handled above"),
        ThreadRequest::Message { .. } => {
            bail!("thread messages use the existing canonical turn transport")
        }
        ThreadRequest::List(_) => unreachable!("list handled above"),
    };
    let (id, _) = resolve(state, &key, false).await?;
    let native = owner_calls::captured(state)?;
    let mut result = response(key.clone());
    let operation = match (method.as_str(), action) {
        ("GET", None) => RuntimeOwnerRequest::GetThread {
            thread_id: id.clone(),
        },
        ("PATCH", None) => RuntimeOwnerRequest::UpdateThread {
            thread_id: id.clone(),
            body: body.clone().context("missing update")?,
        },
        ("GET", Some("goal")) => RuntimeOwnerRequest::GetGoal {
            thread_id: id.clone(),
        },
        ("DELETE", Some("goal")) => RuntimeOwnerRequest::ClearGoal {
            thread_id: id.clone(),
        },
        _ => bail!("unsupported native thread operation"),
    };
    let value = match request(&native, operation).await {
        Ok(value) => value,
        Err(error)
            if action == Some("goal")
                && (method == Method::GET || method == Method::DELETE)
                && error
                    .downcast_ref::<RuntimeOwnerFailure>()
                    .is_some_and(|e| e.status == StatusCode::NOT_FOUND.as_u16()) =>
        {
            // Distinguish an absent goal from a concurrently removed thread.
            request(
                &native,
                RuntimeOwnerRequest::GetThread {
                    thread_id: id.clone(),
                },
            )
            .await?;
            if method == Method::DELETE {
                result.status = "empty".into();
            }
            Value::Null
        }
        Err(error) => return Err(error),
    };
    if action == Some("goal") {
        if method == Method::DELETE && result.status != "empty" {
            result.status = "cleared".into();
        }
        if !value.is_null() {
            let mut goal: codewhale_protocol::ThreadGoal = serde_json::from_value(value.clone())?;
            anyhow::ensure!(
                goal.thread_id == id,
                "canonical goal response belongs to another thread"
            );
            goal.thread_id = key;
            result.goal = Some(goal);
        }
        result.data =
            json!({"goal":result.goal,"cleared":method==Method::DELETE && result.status!="empty"});
    } else {
        let record = value.get("thread").unwrap_or(&value);
        anyhow::ensure!(
            record.get("id").and_then(Value::as_str) == Some(id.as_str()),
            "canonical control returned another target identity"
        );
        let mut thread = project_thread(record, &key)?;
        if running_ids(&native).await?.contains(&id) && thread.status != ThreadStatus::Archived {
            thread.status = ThreadStatus::Running;
        }
        result.thread = Some(thread);
        result.model = record
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        result.model_provider = result.thread.as_ref().map(|t| t.model_provider.clone());
        result.cwd = result.thread.as_ref().map(|t| t.cwd.clone());
        result.data = value;
        if method == Method::GET {
            let snapshot = full_snapshot(state, &native, &id).await?;
            result.data["history"] = serde_json::to_value(snapshot)?;
        }
    }
    Ok(result)
}

/// Explicit CLI startup facts. The helper fills scheduler facts only from
/// the authenticated owner's routing, never from a guessed local Config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadControlSelection {
    pub workspace: Option<PathBuf>,
    pub config_profile: Option<String>,
    pub config_source: Option<PathBuf>,
}

#[cfg(any(unix, windows))]
pub async fn request_thread_control(
    config_path: Option<PathBuf>,
    selected: Option<PathBuf>,
    selection: Option<ThreadControlSelection>,
    request: ThreadRequest,
) -> Result<ThreadResponse> {
    let mut client = daemon_client::connect(config_path.clone(), selected).await?;
    if let Some(selection) = selection {
        let routing = client
            .routing()
            .context("selected owner has no acknowledged control scope")?;
        let scope = RuntimeFrontendScope {
            workers: routing
                .workers
                .context("selected owner has no captured worker setting")?,
            workspace: selection
                .workspace
                .or_else(|| routing.workspace.clone())
                .context("selected owner has no acknowledged workspace")?,
            config_profile: selection.config_profile,
            config_source: selection.config_source,
        };
        scope.validate_bounds()?;
        let owner = client.receipt().clone();
        let socket = owner.socket_path.clone();
        drop(client);
        client = daemon_client::connect_scoped_control_if_published(
            config_path,
            Some(socket),
            scope,
            owner,
        )
        .await?
        .context("captured owner disappeared during scope admission; no replacement")?;
    }
    let request_id = json!(format!("thread-control-{}", Uuid::new_v4()));
    client
        .send(
            request_id.clone(),
            "thread/request",
            serde_json::to_value(request)?,
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut notifications = 0usize;
        loop {
            let frame = client
                .recv()
                .await?
                .context("selected owner closed; outcome uncertain, not replayed")?;
            if frame.get("id") == Some(&request_id) {
                if let Some(error) = frame.get("error") {
                    bail!("canonical owner refused thread control: {error}")
                }
                return serde_json::from_value(
                    frame
                        .get("result")
                        .cloned()
                        .context("canonical control result missing")?,
                )
                .context("invalid canonical control response");
            }
            anyhow::ensure!(
                frame.get("id").is_none(),
                "canonical owner returned another request identity"
            );
            notifications += 1;
            anyhow::ensure!(
                notifications <= MAX_CANONICAL_HISTORY_ENTRIES,
                "canonical owner notification bound exceeded"
            );
        }
    })
    .await
    .context("canonical control deadline expired; outcome uncertain, not replayed")?
}
#[cfg(not(any(unix, windows)))]
pub async fn request_thread_control(
    _config_path: Option<PathBuf>,
    _selected: Option<PathBuf>,
    _selection: Option<ThreadControlSelection>,
    _request: ThreadRequest,
) -> Result<ThreadResponse> {
    bail!("canonical owner attachment is unsupported on this platform")
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(super) async fn compatibility_fixture()
-> (AppState, tempfile::TempDir, tokio::task::JoinHandle<()>) {
    tests::compatibility_fixture().await
}

#[cfg(test)]
pub(super) fn compatibility_router() -> (AppState, tempfile::TempDir, Router) {
    tests::compatibility_router()
}
