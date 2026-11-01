//! Compatibility control projection onto the already held canonical owner.
//! SQLite supplies immutable migration input and committed alias receipts only.
use super::*;
use crate::bridge::acquire_live_runtime_bridge;
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

fn endpoint(bridge: &RuntimeBridge, segments: &[&str]) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(&bridge.base_url)?;
    url.path_segments_mut()
        .map_err(|_| anyhow!("invalid canonical owner URL"))?
        .clear()
        .extend(segments.iter().copied());
    Ok(url)
}

/// Reused by every bridge JSON read, including full canonical history. The
/// declared length and actual streamed bytes must both fit; nothing is cut.
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
    bridge: &RuntimeBridge,
    method: Method,
    segments: &[&str],
    body: Option<&Value>,
) -> Result<Value> {
    let mut request = bridge.authed(bridge.client.request(method, endpoint(bridge, segments)?));
    if let Some(body) = body {
        anyhow::ensure!(
            serde_json::to_vec(body)?.len() <= MAX_CANONICAL_HISTORY_BYTES,
            "encoded canonical control exceeds bound"
        );
        request = request.json(body);
    }
    tokio::time::timeout(Duration::from_secs(30), bridge.request_json(request))
        .await
        .context(
            "canonical control deadline expired; outcome may be committed, no automatic replay",
        )?
}

#[cfg(any(unix, windows))]
async fn store_work<T, F>(work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    daemon_socket::owner_work(work).await
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

/// Holds the existing bridge serialization through import and exact source
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
    let bridge = acquire_live_runtime_bridge(state)
        .await
        .map_err(|e| anyhow!("{}", e.message))?;
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
            let value = request(&bridge, Method::POST, &["v1", "thread-history", "import"], Some(&serde_json::to_value(request_body)?)).await
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
    let value = request(&bridge, Method::GET, &["v1", "threads", &id], None)
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
    state
        .runtime_thread_map
        .lock()
        .await
        .insert(key.to_owned(), id.clone());
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
    let bridge = acquire_live_runtime_bridge(state)
        .await
        .map_err(|e| anyhow!("{}", e.message))?;
    let mut url = endpoint(&bridge, &["v1", "threads"])?;
    url.query_pairs_mut()
        .append_pair("include_archived", "true")
        .append_pair("limit", &(MAX_CANONICAL_HISTORY_ENTRIES + 1).to_string());
    let rows = bridge
        .request_json(bridge.authed(bridge.client.get(url)))
        .await?;
    let rows = rows
        .as_array()
        .context("canonical thread list is not an array")?;
    anyhow::ensure!(
        rows.len() <= MAX_CANONICAL_HISTORY_ENTRIES,
        "canonical thread list exceeds bound"
    );
    let mut canonical = HashMap::new();
    let active = running_ids(&bridge).await?;
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
) -> std::result::Result<ThreadResponse, JsonRpcError> {
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
        .map_err(|e| JsonRpcError::internal(e.to_string()))?
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
        return Err(JsonRpcError::runtime_unavailable(format!("{error:#}")));
    }
    Ok(result)
}

pub(super) fn rpc_error(error: anyhow::Error, key: Option<&str>) -> JsonRpcError {
    if error.downcast_ref::<CommittedControlFailure>().is_some() {
        JsonRpcError::runtime_unavailable(format!("{error:#}"))
    } else if error
        .downcast_ref::<HttpFailure>()
        .is_some_and(|error| error.status == StatusCode::NOT_FOUND)
    {
        JsonRpcError::thread_not_found(key.unwrap_or("unknown"))
    } else {
        JsonRpcError::runtime_unavailable(format!("{error:#}"))
    }
}

async fn running_ids(bridge: &RuntimeBridge) -> Result<std::collections::HashSet<String>> {
    let value = request(bridge, Method::GET, &["v1", "threads", "running"], None).await?;
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
    bridge: &RuntimeBridge,
    id: &str,
) -> Result<CanonicalThreadSnapshot> {
    let snapshot: CanonicalThreadSnapshot = serde_json::from_value(
        request(bridge, Method::GET, &["v1", "threads", id, "history"], None).await?,
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
    let bridge = acquire_live_runtime_bridge(state)
        .await
        .map_err(|error| anyhow!("{}", error.message))?;
    let body = CanonicalThreadMutationRequest {
        version: 1,
        operation_key: operation.clone(),
        expected_data_dir: owner.data_dir.clone(),
        expected_execution_scope: owner.execution_scope.clone(),
        workspace,
        mutation,
    };
    let value = request(
        &bridge,
        Method::POST,
        &["v1", "thread-history", "mutate"],
        Some(&serde_json::to_value(body)?),
    )
    .await
    .with_context(|| format!("canonical operation {operation} may have committed; retain this key, no automatic replay or replacement"))?;
    let receipt: CanonicalThreadReceipt = serde_json::from_value(value).with_context(|| {
        format!("canonical operation {operation} returned an invalid result; retain this key, no replacement")
    })?;
    verify_receipt(&receipt, &owner, &operation).with_context(|| {
        format!("canonical operation {operation} returned an unbound receipt; retain this key without replay or replacement")
    })?;
    present_receipt(&bridge, receipt, status, public_alias).await
}

async fn present_receipt(
    bridge: &RuntimeBridge,
    receipt: CanonicalThreadReceipt,
    status: &str,
    public_alias: Option<&str>,
) -> Result<ThreadResponse> {
    let public_id = public_alias.unwrap_or(&receipt.runtime_thread_id);
    let value = request(
        bridge,
        Method::GET,
        &["v1", "threads", &receipt.runtime_thread_id],
        None,
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
    let bridge = acquire_live_runtime_bridge(state)
        .await
        .map_err(|error| anyhow!("{}", error.message))?;
    let body = CanonicalThreadOperationLookup {
        version: 1,
        operation_key: operation.to_owned(),
        expected_data_dir: selected.data_dir.clone(),
        expected_execution_scope: selected.execution_scope.clone(),
        workspace: workspace.to_owned(),
    };
    let outcome: CanonicalThreadOperationStatus = serde_json::from_value(request(
        &bridge, Method::POST, &["v1","thread-history","operations","lookup"],
        Some(&serde_json::to_value(&body)?),
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
                &bridge,
                Method::POST,
                &["v1", "thread-history", "operations", "recover"],
                Some(&serde_json::to_value(recovery)?),
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
    present_receipt(&bridge, receipt, status, public_alias)
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
    let bridge = acquire_live_runtime_bridge(state)
        .await
        .map_err(|error| anyhow!("{}", error.message))?;
    let snapshot = full_snapshot(state, &bridge, &id).await?;
    drop(bridge);
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
        ThreadRequest::GoalSet(p) => (
            p.thread_id,
            Method::PUT,
            Some("goal"),
            Some(json!({"objective":p.objective,"token_budget":p.token_budget})),
        ),
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
    let bridge = acquire_live_runtime_bridge(state)
        .await
        .map_err(|e| anyhow!("{}", e.message))?;
    let mut result = response(key.clone());
    let mut segments = vec!["v1", "threads", &id];
    if let Some(action) = action {
        segments.push(action)
    }
    let value = match request(&bridge, method.clone(), &segments, body.as_ref()).await {
        Ok(value) => value,
        Err(error)
            if action == Some("goal")
                && (method == Method::GET || method == Method::DELETE)
                && error
                    .downcast_ref::<HttpFailure>()
                    .is_some_and(|e| e.status == StatusCode::NOT_FOUND) =>
        {
            // Distinguish an absent goal from a concurrently removed thread.
            request(&bridge, Method::GET, &["v1", "threads", &id], None).await?;
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
        if running_ids(&bridge).await?.contains(&id) && thread.status != ThreadStatus::Archived {
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
            let snapshot = full_snapshot(state, &bridge, &id).await?;
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
