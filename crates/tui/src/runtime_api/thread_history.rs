//! Full graph migration into the held canonical Runtime. The old SQLite file
//! remains recovery evidence; this adapter never becomes another transcript writer.
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail, ensure};
use axum::{Json, extract::State};
use chrono::{DateTime, Utc};
use codewhale_models::{ContentBlock, Message, Role};
use codewhale_protocol::{
    CanonicalHistoryImportRequest, CanonicalHistoryOptions, CanonicalHistorySource,
    CanonicalThreadMutation, CanonicalThreadMutationRequest, CanonicalThreadReceipt,
    LegacyThreadHistory, MAX_CANONICAL_HISTORY_BYTES, MAX_CANONICAL_HISTORY_ENTRIES,
};

use super::{ApiError, RuntimeApiState};
use crate::runtime_threads::{
    CreateThreadRequest, RuntimeHistoryOperation, RuntimeHistoryWitness, RuntimeThreadManager,
    ThreadListFilter,
};
use crate::session_manager::{SavedSession, SessionManager};
use codewhale_runtime::session_tree::{
    SessionEntry, SessionEntryKind, SessionImportContainer, SessionJournal,
};

pub(crate) async fn history_owner_work<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    #[cfg(test)]
    let ticket = crate::test_support::env_scope_ticket();
    codewhale_app_server::daemon_socket::owner_work(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(ticket);
        work()
    })
    .await
}

/// Validate every branch before traversing the active projection or creating a
/// canonical record. The existing journal validator alone does not reject cycles
/// or duplicate IDs, so this boundary must establish those stronger invariants.
fn legacy_journal(history: &LegacyThreadHistory) -> Result<SessionJournal> {
    ensure!(
        history.version == 1,
        "unsupported legacy history schema; source retained for recovery"
    );
    ensure!(
        history.state_store_id.len() == 64
            && history
                .state_store_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "invalid source database identity"
    );
    ensure!(
        !history.thread_id.is_empty() && history.thread_id.len() <= 128,
        "invalid source thread identity"
    );
    ensure!(
        history.messages.len() <= MAX_CANONICAL_HISTORY_ENTRIES,
        "legacy history entry bound exceeded"
    );
    let mut parents = HashMap::with_capacity(history.messages.len());
    for row in &history.messages {
        ensure!(
            row.id > 0
                && row.thread_id == history.thread_id
                && parents.insert(row.id, row.parent_entry_id).is_none(),
            "duplicate or foreign legacy history entry"
        );
    }
    for parent in parents.values().flatten() {
        ensure!(
            parents.contains_key(parent),
            "dangling legacy history parent; source retained for recovery"
        );
    }
    match history.current_leaf_id {
        Some(leaf) => ensure!(parents.contains_key(&leaf), "legacy history leaf is absent"),
        None => ensure!(
            parents.is_empty(),
            "nonempty legacy history has no selected leaf"
        ),
    }
    if let Some(goal) = history.goal.as_ref() {
        ensure!(
            goal.thread_id == history.thread_id,
            "legacy goal belongs to another source thread"
        );
        goal.validate_stall_state().map_err(anyhow::Error::msg)?;
    }
    // Three-color iterative traversal bounds work and stack even for a deeply
    // nested imported branch; no recursive call uses untrusted graph depth.
    let mut colors = HashMap::with_capacity(parents.len());
    for id in parents.keys().copied() {
        let mut cursor = Some(id);
        let mut path = Vec::new();
        while let Some(id) = cursor {
            match colors.get(&id) {
                Some(1) => {
                    return Err(anyhow!(
                        "cyclic legacy history; source retained for recovery"
                    ));
                }
                Some(2) => break,
                _ => {
                    colors.insert(id, 1);
                    path.push(id);
                    cursor = parents[&id];
                }
            }
        }
        for id in path {
            colors.insert(id, 2);
        }
    }
    let id = |id: i64| format!("legacy:{}:{id}", history.state_store_id);
    let mut entries = Vec::with_capacity(history.messages.len());
    for row in &history.messages {
        let message = match (&row.item, row.role.as_str()) {
            (Some(item), "history") => {
                ensure!(
                    serde_json::from_str::<serde_json::Value>(&row.content)
                        .ok()
                        .as_ref()
                        == Some(item),
                    "legacy history payload and content disagree; source retained for recovery"
                );
                let message = serde_json::from_value::<Message>(item.clone()).context("opaque legacy item cannot be imported as model content; source retained for recovery")?;
                ensure!(
                    serde_json::to_value(&message)? == *item,
                    "legacy message has unsupported fields; source retained for recovery"
                );
                message
            }
            (None, role @ ("user" | "assistant" | "system")) => Message {
                role: match role {
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    _ => Role::System,
                },
                content: vec![ContentBlock::Text {
                    text: row.content.clone(),
                    cache_control: None,
                }],
            },
            _ => {
                return Err(anyhow!(
                    "unsupported legacy message representation; source retained for recovery"
                ));
            }
        };
        if message.role == Role::User {
            crate::image_attach::runtime_images_from_blocks(&message.content)
                .map_err(|error| anyhow!("invalid imported image: {error}"))?;
        }
        entries.push(SessionEntry {
            id: id(row.id),
            parent_id: row.parent_entry_id.map(id),
            kind: std::sync::Arc::new(SessionEntryKind::Message { message }),
            created_at: DateTime::<Utc>::from_timestamp(row.created_at, 0)
                .ok_or_else(|| anyhow!("invalid legacy history timestamp"))?,
            spawn_depth: 0,
        });
    }
    let journal = SessionJournal {
        entries,
        leaf_id: history.current_leaf_id.map(id),
        schema_version: 1,
        spawn_depth: 0,
    };
    journal.validate().map_err(anyhow::Error::msg)?;
    Ok(journal)
}

pub(super) async fn import_thread_history(
    State(state): State<RuntimeApiState>,
    Json(request): Json<CanonicalHistoryImportRequest>,
) -> Result<Json<CanonicalThreadReceipt>, ApiError> {
    ensure_request_scope(&state.runtime_threads, &request)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    // Immediate refusal avoids an unbounded queue retaining whole histories.
    let admission = state
        .runtime_threads
        .try_history_import_guard()
        .map_err(|error| ApiError::conflict(error.to_string()))?;
    // The actual owner job keeps the admission and request after the HTTP waiter
    // disappears. A retry observes its durable identity, never a fresh creation.
    let job = tokio::spawn(async move {
        let _admission = admission;
        import_into_owner(state, request).await
    });
    job.await
        .map_err(|_| {
            ApiError::internal("canonical history worker did not settle; retry the same operation")
        })?
        .map(Json)
        .map_err(|error| ApiError::conflict(error.to_string()))
}

fn ensure_request_scope(
    runtime: &RuntimeThreadManager,
    request: &CanonicalHistoryImportRequest,
) -> Result<()> {
    ensure!(
        request.version == 1,
        "unsupported canonical history request schema"
    );
    let binding = runtime.session_store_binding();
    ensure!(
        request.expected_data_dir == binding.data_dir
            && request.expected_execution_scope == binding.execution_scope,
        "canonical history request belongs to a different held owner store"
    );
    ensure!(
        !request.operation_key.is_empty()
            && request.operation_key.len() <= 128
            && !request.operation_key.chars().any(char::is_control),
        "invalid history operation key"
    );
    ensure!(
        serde_json::to_vec(request)?.len() <= MAX_CANONICAL_HISTORY_BYTES,
        "encoded history request exceeds canonical import bound"
    );
    Ok(())
}

async fn import_into_owner(
    state: RuntimeApiState,
    request: CanonicalHistoryImportRequest,
) -> Result<CanonicalThreadReceipt> {
    let runtime = Arc::clone(&state.runtime_threads);
    // Validate and fingerprint the bounded graph on the existing filesystem/
    // owner worker boundary before retaining or mutating canonical records.
    let (request, journal, request_digest, history_digest) = history_owner_work(move || {
        let journal = legacy_journal(&request.history)?;
        let request_digest = codewhale_runtime::hashing::sha256_hex(
            codewhale_runtime::hashing::canonical_json(&serde_json::to_value(&request)?).as_bytes(),
        );
        let history_digest = codewhale_runtime::hashing::sha256_hex(
            codewhale_runtime::hashing::canonical_json(&serde_json::to_value(&journal)?).as_bytes(),
        );
        Ok((request, journal, request_digest, history_digest))
    })
    .await?;
    let target = if let Some(id) = request.target_runtime_thread_id.as_deref() {
        let thread = runtime.get_thread(id).await?;
        ensure!(
            request.workspace == thread.workspace,
            "existing canonical target belongs to a different selected workspace"
        );
        let session = thread.session_id.ok_or_else(|| {
            anyhow!("existing canonical target has no bound full document; recovery required")
        })?;
        Some((id.to_string(), session))
    } else {
        None
    };
    let target_for_reservation = target.clone();
    let reservation_runtime = Arc::clone(&runtime);
    let key = request.operation_key.clone();
    let operation = history_owner_work(move || match target_for_reservation.as_ref() {
        Some((thread, session)) => reservation_runtime.reserve_history_operation_for_target(
            &key,
            &request_digest,
            &history_digest,
            Some((thread.as_str(), session.as_str())),
        ),
        None => {
            reservation_runtime.reserve_history_operation(&key, &request_digest, &history_digest)
        }
    })
    .await?;
    if operation.committed {
        // A missing committed target is lost history, never a reason to remint.
        let thread = runtime
            .get_thread(&operation.receipt.runtime_thread_id)
            .await?;
        ensure!(
            thread.session_id.as_deref() == Some(operation.receipt.session_id.as_str()),
            "committed history target changed; recovery required"
        );
        let sessions_dir = state.sessions_dir.clone();
        let binding = runtime.session_store_binding();
        let expected = journal.clone();
        history_owner_work(move || {
            verify_import_checkpoint(
                &SessionManager::new(sessions_dir)?,
                &thread,
                &binding,
                &expected,
            )
        })
        .await?;
        let goal_runtime = Arc::clone(&runtime);
        let original_thread = request.history.thread_id.clone();
        let goal = request.history.goal.clone();
        let expected = operation.clone();
        history_owner_work(move || {
            goal_runtime.adopt_history_goal(&expected, &original_thread, goal)
        })
        .await?;
        return Ok(operation.receipt);
    }
    let lookup_runtime = Arc::clone(&runtime);
    let thread_id = operation.receipt.runtime_thread_id.clone();
    let existing =
        history_owner_work(move || lookup_runtime.history_operation_thread(&thread_id)).await?;
    let thread = match existing {
        Some(thread) => thread,
        None => {
            runtime
                .create_thread_with_reserved_id(
                    CreateThreadRequest {
                        workspace: Some(request.workspace),
                        model: request.model,
                        ..Default::default()
                    },
                    state.config_path.as_deref(),
                    state.config_profile.as_deref(),
                    Some(operation.receipt.runtime_thread_id.clone()),
                )
                .await?
        }
    };
    let mut session = SavedSession::import_foreign(
        SessionImportContainer::new("legacy-state-sqlite".into(), &journal, None),
        thread.workspace.clone(),
        thread.model.clone(),
    )
    .map_err(anyhow::Error::msg)?;
    session.metadata.id = operation.receipt.session_id.clone();
    session.system_prompt.clone_from(&thread.system_prompt);
    session.metadata.created_at = operation.created_at;
    session.metadata.updated_at = operation.created_at;
    session.metadata.runtime_store = Some(runtime.session_store_binding());
    session.metadata.set_model_provider_route(
        thread
            .model_provider
            .as_deref()
            .ok_or_else(|| anyhow!("canonical thread has no captured provider"))?,
        thread.model_provider_id.as_deref(),
    );
    let mut captured_target_document_digest = None;
    if target.is_some() {
        let detail = runtime.get_thread_detail(&thread.id).await?;
        ensure!(
            !super::sessions::thread_detail_has_live_work(&detail),
            "canonical target has live work; retry the same import after settlement"
        );
        let full = snapshot_in_owner(state.clone(), thread.id.clone())
            .await
            .map_err(|error| anyhow!(error.message))?;
        captured_target_document_digest = full.saved_document_digest;
        ensure!(
            captured_target_document_digest.is_some(),
            "canonical target full document witness is absent"
        );
        let mut observed: SavedSession = serde_json::from_value(full.session)?;
        ensure!(
            observed.metadata.id == operation.receipt.session_id,
            "canonical target session changed before source merge"
        );
        let imported = session
            .journal
            .take()
            .ok_or_else(|| anyhow!("source journal missing"))?;
        let destination = observed
            .journal
            .as_mut()
            .ok_or_else(|| anyhow!("target full journal missing"))?;
        let mut positions: HashMap<String, usize> = destination
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id.clone(), index))
            .collect();
        for entry in imported.entries {
            if let Some(index) = positions.get(&entry.id).copied() {
                ensure!(
                    destination.entries[index] == entry,
                    "imported entry identity conflicts with canonical history"
                );
            } else {
                ensure!(
                    destination.entries.len() < MAX_CANONICAL_HISTORY_ENTRIES,
                    "merged full history exceeds entry transport bound; both sources retained"
                );
                positions.insert(entry.id.clone(), destination.entries.len());
                destination.entries.push(entry);
            }
        }
        if destination.leaf_id.is_none() {
            destination.leaf_id = imported.leaf_id;
            observed.leaf_id = destination.leaf_id.clone();
            observed.messages = destination.to_messages();
        }
        ensure!(
            destination.entries.len() <= MAX_CANONICAL_HISTORY_ENTRIES,
            "merged full history exceeds entry transport bound; both sources retained"
        );
        let mut bound = HistorySizeBound(MAX_CANONICAL_HISTORY_BYTES);
        serde_json::to_writer(&mut bound, &observed)?;
        session = observed;
    }
    let target_merge = target.is_some();
    let sessions_dir = state.sessions_dir.clone();
    let (session, _target_lease) = history_owner_work(move || {
        let manager = SessionManager::new(sessions_dir)?;
        let lease = manager.reserve_session_for_external_write(&session.metadata.id)?;
        match manager
            .load_session_snapshot_bounded(&session.metadata.id, MAX_CANONICAL_HISTORY_BYTES)
        {
            Ok(observed) if target_merge => {
                ensure!(
                    Some(saved_document_digest(&observed)?) == captured_target_document_digest,
                    "canonical full document changed before source merge; retry the same operation"
                );
                ensure!(
                    observed.metadata.runtime_store.as_ref().is_none_or(
                        |binding| Some(binding) == session.metadata.runtime_store.as_ref()
                    ) && observed.metadata.workspace == session.metadata.workspace,
                    "canonical target store binding changed before merge"
                );
                manager.save_session(&session)?;
                return Ok((session, lease));
            }
            Ok(observed) => {
                ensure!(
                    observed.journal == session.journal
                        && observed.metadata.runtime_store == session.metadata.runtime_store
                        && observed.metadata.workspace == session.metadata.workspace,
                    "reserved imported session diverged; recovery required"
                );
                return Ok((observed, lease));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !target_merge => {}
            Err(error) => return Err(error.into()),
        }
        manager.save_session(&session)?;
        Ok((session, lease))
    })
    .await?;
    let detail = runtime.get_thread_detail(&thread.id).await?;
    ensure!(
        !super::sessions::thread_detail_has_live_work(&detail),
        "reserved imported thread has active work; recovery required"
    );
    if detail.turns.is_empty() && detail.items.is_empty() {
        runtime
            .seed_thread_from_messages_with_history_operation(
                &thread.id,
                &session.messages,
                Some(&operation),
                &session.messages,
            )
            .await?;
    } else if !target_merge {
        let sessions_dir = state.sessions_dir.clone();
        let binding = runtime.session_store_binding();
        let expected = journal.clone();
        let observed_thread = runtime.get_thread(&thread.id).await?;
        history_owner_work(move || {
            verify_import_checkpoint_under_lease(
                &SessionManager::new(sessions_dir)?,
                &observed_thread,
                &binding,
                &expected,
            )
        })
        .await?;
    }
    runtime
        .set_thread_session_checkpoint(&thread.id, &session)
        .await?;
    let goal_runtime = Arc::clone(&runtime);
    let original_thread = request.history.thread_id.clone();
    let goal = request.history.goal.clone();
    let expected = operation.clone();
    history_owner_work(move || goal_runtime.adopt_history_goal(&expected, &original_thread, goal))
        .await?;
    let commit_runtime = Arc::clone(&runtime);
    history_owner_work(move || commit_runtime.commit_history_operation(operation)).await
}

fn verify_import_checkpoint(
    manager: &SessionManager,
    thread: &crate::runtime_threads::ThreadRecord,
    binding: &crate::runtime_threads::RuntimeStoreBinding,
    expected: &SessionJournal,
) -> Result<()> {
    let id = thread
        .session_id
        .as_deref()
        .ok_or_else(|| anyhow!("committed history target has no session; recovery required"))?;
    let _lease = manager.reserve_session_for_external_write(id)?;
    verify_import_checkpoint_under_lease(manager, thread, binding, expected)
}

fn verify_import_checkpoint_under_lease(
    manager: &SessionManager,
    thread: &crate::runtime_threads::ThreadRecord,
    binding: &crate::runtime_threads::RuntimeStoreBinding,
    expected: &SessionJournal,
) -> Result<()> {
    let id = thread
        .session_id
        .as_deref()
        .ok_or_else(|| anyhow!("canonical target session is absent"))?;
    let observed = manager.load_session_snapshot_bounded(id, MAX_CANONICAL_HISTORY_BYTES)?;
    ensure!(
        observed.metadata.runtime_store.as_ref() == Some(binding)
            && observed.metadata.workspace == thread.workspace,
        "committed history session binding changed; recovery required"
    );
    let checkpoint = thread
        .saved_session_checkpoint
        .as_ref()
        .ok_or_else(|| anyhow!("committed history has no verifiable checkpoint"))?;
    ensure!(
        crate::runtime_threads::checkpoint_prefix_len(checkpoint, &observed.messages)?.is_some(),
        "committed history checkpoint differs from its document; recovery required"
    );
    let journal = observed
        .journal
        .as_ref()
        .ok_or_else(|| anyhow!("committed full history journal is missing"))?;
    let entries: HashMap<_, _> = journal
        .entries
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect();
    ensure!(
        entries.len() == journal.entries.len()
            && expected.entries.iter().all(|entry| entries
                .get(entry.id.as_str())
                .is_some_and(|observed| *observed == entry)),
        "committed full history branches changed; recovery required"
    );
    Ok(())
}

/// Full graph read through the existing owner Engine and session checkpoint.
/// It never publishes an alias or becomes a second journal writer.
pub(super) async fn snapshot_thread_history(
    State(state): State<RuntimeApiState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<codewhale_protocol::CanonicalThreadSnapshot>, ApiError> {
    let _admission = state.runtime_threads.session_checkpoint_guard().await;
    snapshot_in_owner(state, id).await.map(Json)
}

async fn snapshot_in_owner(
    state: RuntimeApiState,
    id: String,
) -> Result<codewhale_protocol::CanonicalThreadSnapshot, ApiError> {
    snapshot_in_runtime(&state.runtime_threads, &state.sessions_dir, id).await
}

async fn snapshot_in_runtime(
    runtime: &Arc<RuntimeThreadManager>,
    sessions_dir: &Path,
    id: String,
) -> Result<codewhale_protocol::CanonicalThreadSnapshot, ApiError> {
    let runtime = Arc::clone(runtime);
    let thread = runtime
        .get_thread(&id)
        .await
        .map_err(super::map_thread_err)?;
    let bound_id = thread.session_id.clone();
    let expected_binding = runtime.session_store_binding();
    let document = if let Some(session_id) = bound_id.clone() {
        let sessions_dir = sessions_dir.to_path_buf();
        let checkpoint = thread.saved_session_checkpoint.clone();
        let binding = expected_binding.clone();
        let workspace = thread.workspace.clone();
        Some(
            history_owner_work(move || {
                let manager = SessionManager::new(sessions_dir)?;
                let _lease = manager.reserve_session_for_external_write(&session_id)?;
                let document = manager
                    .load_session_snapshot_bounded(&session_id, MAX_CANONICAL_HISTORY_BYTES)?;
                ensure!(
                    document.metadata.workspace == workspace,
                    "saved history workspace differs from its held thread"
                );
                if let Some(saved_binding) = document.metadata.runtime_store.as_ref() {
                    ensure!(
                        saved_binding == &binding,
                        "saved history belongs to a different Runtime store"
                    );
                }
                let checkpoint = checkpoint
                    .ok_or_else(|| anyhow!("bound history has no verifiable checkpoint"))?;
                ensure!(
                    crate::runtime_threads::checkpoint_prefix_len(&checkpoint, &document.messages)?
                        .is_some(),
                    "bound full history checkpoint changed; recovery required"
                );
                let digest = saved_document_digest(&document)?;
                Ok((document, digest, manager.load_session_goal(&session_id)?))
            })
            .await
            .map_err(|error| ApiError::conflict(error.to_string()))?,
        )
    } else {
        None
    };
    // The snapshot is taken on the one existing Engine mailbox. A saved prefix
    // can lag a live turn, so merge that actual projection in memory while
    // retaining all prior branches. No read writes or repairs the saved file.
    let engine = runtime
        .get_engine(&id)
        .await
        .map_err(super::map_thread_err)?;
    let snapshot = engine
        .get_session_snapshot()
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let response = history_owner_work(move || {
        let persisted_entry_count = document
            .as_ref()
            .and_then(|(saved, _, _)| saved.journal.as_ref())
            .map_or(0, |journal| journal.entries.len());
        let previous_updated_at = document
            .as_ref()
            .map(|(saved, _, _)| saved.metadata.updated_at);
        let has_projection_delta = document
            .as_ref()
            .is_none_or(|(saved, _, _)| saved.messages != snapshot.messages);
        let source_goal_digest =
            session_goal_digest(&document.as_ref().and_then(|(_, _, goal)| goal.clone()))?;
        let persisted_document_digest = document.as_ref().map(|(_, digest, _)| digest.clone());
        let mut session = match document {
            Some((existing, _, _)) => crate::session_manager::update_session(
                existing,
                &snapshot.messages,
                snapshot.total_tokens,
                snapshot.system_prompt.as_ref(),
            ),
            None => crate::session_manager::create_saved_session_with_id_and_mode(
                snapshot.session_id.clone(),
                &snapshot.messages,
                &snapshot.model,
                &snapshot.workspace,
                snapshot.total_tokens,
                snapshot.system_prompt.as_ref(),
                Some(&snapshot.mode),
            ),
        };
        // The read projection uses the actual thread boundary, rather than
        // fresh UUIDs and the reader's wall clock. Re-reading unchanged Engine
        // content must not fabricate a changed full-history digest.
        stable_projection_entries(&mut session, persisted_entry_count, &thread)?;
        if previous_updated_at.is_none() {
            session.metadata.created_at = thread.created_at;
        }
        session.metadata.updated_at = if has_projection_delta {
            thread.updated_at
        } else {
            previous_updated_at.unwrap_or(thread.updated_at)
        };
        session.metadata.model = snapshot.model;
        session.metadata.set_model_provider_route(
            &snapshot.model_provider,
            snapshot.model_provider_id.as_deref(),
        );
        session.metadata.runtime_store = Some(expected_binding.clone());
        let graph = session
            .journal
            .as_ref()
            .ok_or_else(|| anyhow!("full journal is absent"))?;
        ensure!(
            graph.entries.len() <= MAX_CANONICAL_HISTORY_ENTRIES,
            "full history exceeds entry transport bound; source retained"
        );
        // Measure before constructing the Value allocation. Refuse the complete
        // result rather than truncating branches or returning a partial graph.
        let mut size = HistorySizeBound(MAX_CANONICAL_HISTORY_BYTES);
        serde_json::to_writer(&mut size, &session)?;
        let response = codewhale_protocol::CanonicalThreadSnapshot {
            version: 1,
            data_dir: expected_binding.data_dir,
            execution_scope: expected_binding.execution_scope,
            runtime_thread_id: id,
            saved_session_id: bound_id,
            saved_document_digest: persisted_document_digest,
            document_digest: history_source_digest(&session, &source_goal_digest)?,
            session_goal_digest: source_goal_digest,
            session: serde_json::to_value(session)?,
        };
        let mut size = HistorySizeBound(MAX_CANONICAL_HISTORY_BYTES);
        serde_json::to_writer(&mut size, &response)?;
        Ok(response)
    })
    .await
    .map_err(|error| ApiError::conflict(error.to_string()))?;
    Ok(response)
}

fn stable_projection_entries(
    session: &mut SavedSession,
    retained_count: usize,
    thread: &crate::runtime_threads::ThreadRecord,
) -> Result<()> {
    let journal = session
        .journal
        .as_mut()
        .ok_or_else(|| anyhow!("full journal is absent"))?;
    let mut ids = HashMap::new();
    for (index, entry) in journal.entries.iter_mut().enumerate().skip(retained_count) {
        let old = entry.id.clone();
        let digest = codewhale_runtime::hashing::sha256_hex(
            codewhale_runtime::hashing::canonical_json(&serde_json::to_value(&entry.kind)?)
                .as_bytes(),
        );
        entry.id = format!(
            "projection:{}:{}:{index}:{digest}",
            thread.id,
            thread.latest_turn_id.as_deref().unwrap_or("initial")
        );
        entry.created_at = thread.updated_at;
        ids.insert(old, entry.id.clone());
    }
    for entry in journal.entries.iter_mut().skip(retained_count) {
        if let Some(id) = entry.parent_id.as_mut()
            && let Some(replacement) = ids.get(id)
        {
            *id = replacement.clone();
        }
    }
    if let Some(leaf) = journal.leaf_id.as_mut()
        && let Some(replacement) = ids.get(leaf)
    {
        *leaf = replacement.clone();
    }
    session.leaf_id = journal.leaf_id.clone();
    Ok(())
}

const CANONICAL_REQUEST_ENVELOPE_BYTES: usize = 64 * 1024;

pub(crate) fn saved_document_digest(session: &SavedSession) -> Result<String> {
    let mut size = HistorySizeBound(MAX_CANONICAL_HISTORY_BYTES);
    serde_json::to_writer(&mut size, &session.transport_form())?;
    Ok(codewhale_runtime::hashing::sha256_hex(
        codewhale_runtime::hashing::canonical_json(&serde_json::to_value(session)?).as_bytes(),
    ))
}

pub(super) async fn mutate_thread_history(
    State(state): State<RuntimeApiState>,
    Json(request): Json<CanonicalThreadMutationRequest>,
) -> Result<Json<CanonicalThreadReceipt>, ApiError> {
    mutate_thread_history_in_runtime(
        &state.runtime_threads,
        &state.sessions_dir,
        state.config_path.as_deref(),
        state.config_profile.as_deref(),
        request,
    )
    .await
    .map(Json)
    .map_err(|error| ApiError::conflict(error.to_string()))
}

/// The HTTP frontend and the inactive local mounted handoff invoke this same
/// actual held manager. It creates no transport, guest manager or policy owner.
pub(crate) async fn mutate_thread_history_in_runtime(
    runtime: &Arc<RuntimeThreadManager>,
    sessions_dir: &Path,
    config_path: Option<&Path>,
    config_profile: Option<&str>,
    request: CanonicalThreadMutationRequest,
) -> Result<CanonicalThreadReceipt> {
    let binding = runtime.session_store_binding();
    ensure!(
        request.version == 1
            && request.expected_data_dir == binding.data_dir
            && request.expected_execution_scope == binding.execution_scope,
        "canonical mutation belongs to a different held owner store"
    );
    ensure!(
        !request.operation_key.is_empty()
            && request.operation_key.len() <= 128
            && !request.operation_key.chars().any(char::is_control),
        "invalid canonical operation key"
    );
    let mut size = HistorySizeBound(MAX_CANONICAL_HISTORY_BYTES + CANONICAL_REQUEST_ENVELOPE_BYTES);
    serde_json::to_writer(&mut size, &request)?;
    let admission = runtime.try_history_import_guard()?;
    let runtime = Arc::clone(runtime);
    let sessions_dir = sessions_dir.to_path_buf();
    let config_path = config_path.map(Path::to_path_buf);
    let config_profile = config_profile.map(str::to_string);
    // Cancellation of a socket/HTTP waiter cannot release admission or abandon
    // the reserved write. The caller retries only the same captured intent.
    tokio::spawn(async move {
        let _admission = admission;
        mutate_in_owner(runtime, sessions_dir, config_path, config_profile, request).await
    })
    .await
    .context("canonical operation outcome is uncertain; retain and retry the same operation key")?
}

fn journal_digest(journal: &SessionJournal) -> Result<String> {
    Ok(codewhale_runtime::hashing::sha256_hex(
        codewhale_runtime::hashing::canonical_json(&serde_json::to_value(journal)?).as_bytes(),
    ))
}

pub(crate) fn session_goal_digest(
    goal: &Option<crate::session_manager::SessionGoalState>,
) -> Result<String> {
    Ok(codewhale_runtime::hashing::sha256_hex(
        codewhale_runtime::hashing::canonical_json(&serde_json::to_value(goal)?).as_bytes(),
    ))
}

fn history_source_digest(session: &SavedSession, goal_digest: &str) -> Result<String> {
    Ok(codewhale_runtime::hashing::sha256_hex(
        format!(
            "codewhale:full-history-source:v1\0{}\0{goal_digest}",
            saved_document_digest(session)?
        )
        .as_bytes(),
    ))
}

fn forked_session_goal(
    mut goal: Option<crate::session_manager::SessionGoalState>,
) -> Option<crate::session_manager::SessionGoalState> {
    if let Some(goal) = goal.as_mut()
        && goal.status == crate::session_manager::SessionGoalStatus::Active
    {
        goal.status = crate::session_manager::SessionGoalStatus::Paused;
        goal.pause_reason = None;
    }
    goal
}

fn operation_witness(journal: &SessionJournal, workspace: &Path) -> RuntimeHistoryWitness {
    RuntimeHistoryWitness {
        entries_len: journal.entries.len(),
        leaf_id: journal.leaf_id.clone(),
        schema_version: journal.schema_version,
        spawn_depth: journal.spawn_depth,
        seed_from_message_index: 0,
        workspace: workspace.to_path_buf(),
    }
}

fn verify_operation_checkpoint(
    manager: &SessionManager,
    thread: &crate::runtime_threads::ThreadRecord,
    binding: &crate::runtime_threads::RuntimeStoreBinding,
    operation: &RuntimeHistoryOperation,
) -> Result<()> {
    let id = thread
        .session_id
        .as_deref()
        .ok_or_else(|| anyhow!("canonical operation target lost its session"))?;
    ensure!(
        id == operation.receipt.session_id,
        "canonical operation session changed"
    );
    let _lease = manager.reserve_session_for_external_write(id)?;
    let observed = manager.load_session_snapshot_bounded(id, MAX_CANONICAL_HISTORY_BYTES)?;
    ensure!(
        observed.metadata.runtime_store.as_ref() == Some(binding)
            && observed.metadata.workspace == thread.workspace,
        "canonical operation document binding changed"
    );
    let checkpoint = thread
        .saved_session_checkpoint
        .as_ref()
        .ok_or_else(|| anyhow!("canonical operation checkpoint is absent"))?;
    ensure!(
        crate::runtime_threads::checkpoint_prefix_len(checkpoint, &observed.messages)?.is_some(),
        "canonical operation checkpoint differs from its document"
    );
    verify_operation_journal(&observed, operation)
}

fn verify_operation_journal(
    observed: &SavedSession,
    operation: &RuntimeHistoryOperation,
) -> Result<()> {
    let witness = operation
        .journal_witness
        .as_ref()
        .ok_or_else(|| anyhow!("canonical operation witness missing; recovery required"))?;
    let journal = observed
        .journal
        .as_ref()
        .ok_or_else(|| anyhow!("canonical full graph missing"))?;
    ensure!(
        journal.entries.len() >= witness.entries_len
            && journal.schema_version == witness.schema_version
            && journal.spawn_depth == witness.spawn_depth,
        "canonical operation full graph shrank or changed schema"
    );
    let original = SessionJournal {
        entries: journal.entries[..witness.entries_len].to_vec(),
        leaf_id: witness.leaf_id.clone(),
        schema_version: witness.schema_version,
        spawn_depth: witness.spawn_depth,
    };
    ensure!(
        journal_digest(&original)? == operation.receipt.history_digest,
        "canonical operation branches changed; recovery required"
    );
    Ok(())
}

/// Complete only the target whose bytes and process checkpoint were prepared by
/// this retained intent. Missing preparation never authorizes source recreation.
async fn settle_prepared_history_operation(
    runtime: &Arc<RuntimeThreadManager>,
    sessions_dir: &Path,
    operation: &RuntimeHistoryOperation,
    workspace: &Path,
) -> Result<Option<CanonicalThreadReceipt>> {
    let witness = operation
        .journal_witness
        .as_ref()
        .ok_or_else(|| anyhow!("prepared operation lacks its full graph witness"))?;
    ensure!(
        witness.workspace == workspace,
        "operation selected workspace changed"
    );
    let captured = Arc::clone(runtime);
    let id = operation.receipt.runtime_thread_id.clone();
    let Some(thread) = history_owner_work(move || captured.history_operation_thread(&id)).await?
    else {
        return Ok(None);
    };

    ensure!(
        thread.workspace == workspace,
        "reserved target workspace changed"
    );
    let detail = runtime.get_thread_detail(&thread.id).await?;
    ensure!(
        !super::sessions::thread_detail_has_live_work(&detail),
        "reserved target has pending work"
    );
    let dir = sessions_dir.to_path_buf();
    let binding = runtime.session_store_binding();
    let expected = operation.clone();
    let selected_workspace = workspace.to_path_buf();
    let saved = history_owner_work(move || {
        let manager = SessionManager::new(dir)?;
        let lease = manager.reserve_session_for_external_write(&expected.receipt.session_id)?;
        let saved = match manager.load_session_snapshot_bounded(
            &expected.receipt.session_id,
            MAX_CANONICAL_HISTORY_BYTES,
        ) {
            Ok(saved) => saved,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        // An old source document is not the new prepared publication.
        // The original source CAS below still decides whether it can be retried.
        if expected.target_document_digest.as_ref() != Some(&saved_document_digest(&saved)?) {
            return Ok(None);
        }
        ensure!(
            saved.metadata.runtime_store.as_ref() == Some(&binding)
                && saved.metadata.workspace == selected_workspace,
            "reserved saved document binding changed"
        );
        if let Some(digest) = expected.session_goal_target_digest.as_ref() {
            ensure!(
                session_goal_digest(&manager.load_session_goal(&expected.receipt.session_id)?)?
                    == *digest,
                "prepared local goal sidecar changed; no recovery write admitted"
            );
        }
        verify_operation_journal(&saved, &expected)?;
        let witness = expected
            .journal_witness
            .as_ref()
            .ok_or_else(|| anyhow!("operation witness absent"))?;
        let journal = saved
            .journal
            .as_ref()
            .ok_or_else(|| anyhow!("prepared graph is absent"))?;
        ensure!(
            journal.entries.len() == witness.entries_len && journal.leaf_id == witness.leaf_id,
            "uncommitted target acquired a successor; no replay is safe"
        );
        Ok(Some((saved, lease)))
    })
    .await?;
    if let Some((saved, _target_lease)) = saved {
        runtime.admit_history_execution().await?;
        let from = witness.seed_from_message_index;
        ensure!(
            from <= saved.messages.len(),
            "history suffix boundary is invalid"
        );
        let checkpoint = thread
            .saved_session_checkpoint
            .as_ref()
            .map(|checkpoint| {
                crate::runtime_threads::checkpoint_prefix_len(checkpoint, &saved.messages)
            })
            .transpose()?
            .flatten();
        if checkpoint != Some(saved.messages.len()) {
            if from == 0 {
                ensure!(
                    detail.turns.is_empty() && detail.items.is_empty(),
                    "reserved target process history is unproven; recovery required"
                );
            } else {
                ensure!(
                    checkpoint == Some(from)
                        && thread.session_id.as_deref() == Some(saved.metadata.id.as_str()),
                    "reserved Resume prefix changed; recovery required"
                );
            }
            runtime
                .seed_thread_from_messages_with_history_operation(
                    &thread.id,
                    &saved.messages[from..],
                    Some(operation),
                    &saved.messages,
                )
                .await?;
        }
        runtime
            .set_thread_session_checkpoint(&thread.id, &saved)
            .await?;
        runtime
            .synchronize_history_operation(&thread.id, &saved)
            .await?;
        let manager = Arc::clone(runtime);
        let operation = operation.clone();
        return history_owner_work(move || manager.commit_history_operation(operation))
            .await
            .map(Some);
    }
    Ok(None)
}

async fn mutate_in_owner(
    runtime: Arc<RuntimeThreadManager>,
    sessions_dir: std::path::PathBuf,
    config_path: Option<std::path::PathBuf>,
    config_profile: Option<String>,
    request: CanonicalThreadMutationRequest,
) -> Result<CanonicalThreadReceipt> {
    let request_digest = codewhale_runtime::hashing::sha256_hex(
        codewhale_runtime::hashing::canonical_json(&serde_json::to_value(&request)?).as_bytes(),
    );
    let lookup_runtime = Arc::clone(&runtime);
    let key = request.operation_key.clone();
    let digest = request_digest.clone();
    let reserved =
        history_owner_work(move || lookup_runtime.lookup_history_operation(&key, &digest)).await?;
    if let Some(operation) = reserved
        .as_ref()
        .filter(|operation| operation.journal_witness.is_some())
    {
        let witness = operation.journal_witness.as_ref().ok_or_else(|| {
            anyhow!("operation scope witness absent; recover the retained intent")
        })?;
        ensure!(
            witness.workspace == request.workspace,
            "operation selected workspace changed"
        );
        let thread_id = operation.receipt.runtime_thread_id.clone();
        let captured = Arc::clone(&runtime);
        let thread =
            history_owner_work(move || captured.history_operation_thread(&thread_id)).await?;
        if operation.committed {
            let thread = thread.ok_or_else(|| anyhow!("committed canonical target is missing"))?;
            ensure!(
                thread.workspace == request.workspace,
                "canonical operation workspace changed"
            );
            let manager_dir = sessions_dir.clone();
            let binding = runtime.session_store_binding();
            let expected = operation.clone();
            history_owner_work(move || {
                verify_operation_checkpoint(
                    &SessionManager::new(manager_dir)?,
                    &thread,
                    &binding,
                    &expected,
                )
            })
            .await?;
            return Ok(operation.receipt.clone());
        }
        if let Some(receipt) = settle_prepared_history_operation(
            &runtime,
            &sessions_dir,
            operation,
            &request.workspace,
        )
        .await?
        {
            return Ok(receipt);
        }
    }
    runtime.admit_history_execution().await?;
    let is_resume = matches!(&request.mutation, CanonicalThreadMutation::Resume { .. });
    let mut create = CreateThreadRequest::default();
    let mut source_thread = None;
    let mut source_disk_digest = None;
    let mut source_goal = None;
    let mut source = match &request.mutation {
        CanonicalThreadMutation::Create { config } => {
            create = serde_json::from_value(config.clone())
                .context("invalid create-thread configuration")?;
            // The accepted existing shape has no credentials. Unknown fields
            // cannot masquerade as an ignored policy or operation parameter.
            let encoded = serde_json::to_value(&create)?;
            ensure!(
                config.as_object().is_some_and(|fields| fields
                    .iter()
                    .all(|(key, value)| encoded.get(key) == Some(value))),
                "unsupported create-thread field"
            );
            ensure!(
                create
                    .workspace
                    .as_ref()
                    .is_none_or(|workspace| workspace == &request.workspace),
                "create configuration workspace differs from selected admission"
            );
            None
        }
        CanonicalThreadMutation::Resume { source, .. }
        | CanonicalThreadMutation::Fork { source, .. } => Some(match source {
            CanonicalHistorySource::Thread {
                runtime_thread_id,
                expected_document_digest,
            } => {
                let thread = runtime.get_thread(runtime_thread_id).await?;
                ensure!(
                    !is_resume || thread.workspace == request.workspace,
                    "Resume source workspace differs from selected admission"
                );
                let detail = runtime.get_thread_detail(runtime_thread_id).await?;
                ensure!(
                    !super::sessions::thread_detail_has_live_work(&detail),
                    "selected history has pending work; wait before resume or fork"
                );
                let snapshot =
                    snapshot_in_runtime(&runtime, &sessions_dir, runtime_thread_id.clone())
                        .await
                        .map_err(|error| anyhow!(error.message))?;
                ensure!(
                    &snapshot.document_digest == expected_document_digest,
                    "selected full history changed before canonical admission"
                );
                source_disk_digest = snapshot.saved_document_digest;
                let saved: SavedSession = serde_json::from_value(snapshot.session)?;
                let id = saved.metadata.id.clone();
                let dir = sessions_dir.clone();
                let expected = snapshot.session_goal_digest;
                source_goal = history_owner_work(move || {
                    let manager = SessionManager::new(dir)?;
                    let _lease = manager.reserve_session_for_external_write(&id)?;
                    let goal = manager.load_session_goal(&id)?;
                    ensure!(
                        session_goal_digest(&goal)? == expected,
                        "source local goal changed before admission"
                    );
                    Ok(goal)
                })
                .await?;
                source_thread = Some(thread);
                saved
            }
            CanonicalHistorySource::SavedSession {
                session,
                expected_document_digest,
            } => {
                let mut proposed: SavedSession = serde_json::from_value(session.clone())?;
                ensure!(
                    serde_json::to_value(&proposed)? == *session,
                    "saved history contains unsupported fields; source retained"
                );
                proposed.ensure_journal();
                let id = proposed.metadata.id.clone();
                let dir = sessions_dir.clone();
                let expected = expected_document_digest.clone();
                let binding = runtime.session_store_binding();
                let selected_workspace = request.workspace.clone();
                let (observed, goal) = history_owner_work(move || {
                    let manager = SessionManager::new(dir)?;
                    let _lease = manager.reserve_session_for_external_write(&id)?;
                    let observed =
                        manager.load_session_snapshot_bounded(&id, MAX_CANONICAL_HISTORY_BYTES)?;
                    ensure!(
                        saved_document_digest(&observed)? == expected,
                        "protected saved document changed before admission"
                    );
                    ensure!(
                        (!is_resume || observed.metadata.workspace == selected_workspace)
                            && observed
                                .metadata
                                .runtime_store
                                .as_ref()
                                .is_none_or(|saved| saved == &binding),
                        "saved history belongs to another workspace or owner store"
                    );
                    Ok((observed, manager.load_session_goal(&id)?))
                })
                .await?;
                ensure!(
                    saved_document_digest(&proposed)? == saved_document_digest(&observed)?,
                    "proposed history differs from protected saved document"
                );
                source_goal = goal;
                source_disk_digest = Some(expected_document_digest.clone());
                let candidates = runtime
                    .list_threads(ThreadListFilter::IncludeArchived, None)
                    .await?
                    .into_iter()
                    .filter(|thread| {
                        thread.session_id.as_deref() == Some(observed.metadata.id.as_str())
                    })
                    .collect::<Vec<_>>();
                ensure!(
                    candidates.len() <= 1,
                    "saved history has multiple canonical holders; explicit target required"
                );
                if let Some(thread) = candidates.into_iter().next() {
                    ensure!(
                        thread.workspace == observed.metadata.workspace,
                        "saved history holder source workspace changed"
                    );
                    let detail = runtime.get_thread_detail(&thread.id).await?;
                    ensure!(
                        !super::sessions::thread_detail_has_live_work(&detail),
                        "saved history holder has pending work"
                    );
                    let checkpoint = thread
                        .saved_session_checkpoint
                        .as_ref()
                        .ok_or_else(|| anyhow!("saved holder checkpoint is missing"))?;
                    let covered = crate::runtime_threads::checkpoint_prefix_len(
                        checkpoint,
                        &observed.messages,
                    )?;
                    let released = if crate::runtime_threads::checkpoint_covers_every_turn(
                        checkpoint,
                        &detail.turns,
                    ) {
                        match covered {
                            Some(len) if len == observed.messages.len() => {
                                source_thread = Some(thread);
                                None
                            }
                            Some(_) => Some((
                                thread,
                                "the saved document advanced past this thread after a resume",
                            )),
                            None => Some((
                                thread,
                                "the saved document's head was rewritten after this thread's checkpoint",
                            )),
                        }
                    } else {
                        ensure!(
                            covered.is_some(),
                            "saved holder checkpoint differs from the saved history; refresh history"
                        );
                        let full = snapshot_in_runtime(&runtime, &sessions_dir, thread.id.clone())
                            .await
                            .map_err(|error| anyhow!(error.message))?;
                        let live: SavedSession = serde_json::from_value(full.session)?;
                        if live.messages == observed.messages && live.journal == observed.journal {
                            source_thread = Some(thread);
                            None
                        } else if live.messages.len() < observed.messages.len()
                            && observed.messages.starts_with(&live.messages)
                        {
                            let fresh = runtime.get_thread(&thread.id).await?;
                            ensure!(
                                fresh.session_id == thread.session_id
                                    && fresh.latest_turn_id == thread.latest_turn_id,
                                "saved holder changed during admission; retry"
                            );
                            Some((
                                fresh,
                                "the saved document advanced past this thread after a resume",
                            ))
                        } else {
                            bail!(
                                "saved holder has a successor outside the selected document; refresh history"
                            );
                        }
                    };
                    if let Some((holder, reason)) = released {
                        let release_runtime = Arc::clone(&runtime);
                        history_owner_work(move || {
                            release_runtime.release_stale_session_holder(&holder, reason)
                        })
                        .await?;
                    }
                }
                observed
            }
        }),
    };
    let options = match &request.mutation {
        CanonicalThreadMutation::Resume { options, .. }
        | CanonicalThreadMutation::Fork { options, .. } => options.clone(),
        CanonicalThreadMutation::Create { .. } => CanonicalHistoryOptions::default(),
    };
    if matches!(
        &request.mutation,
        CanonicalThreadMutation::Resume {
            source: CanonicalHistorySource::SavedSession { .. },
            ..
        } | CanonicalThreadMutation::Fork {
            source: CanonicalHistorySource::SavedSession { .. },
            ..
        }
    ) {
        ensure!(
            options
                .expected_session_goal_digest
                .as_ref()
                .is_some_and(|expected| session_goal_digest(&source_goal)
                    .is_ok_and(|observed| &observed == expected))
                || (options.expected_session_goal_digest.is_none() && source_goal.is_none()),
            "saved source local goal witness is absent or changed; source retained"
        );
    }
    let captured_source_goal_digest = session_goal_digest(&source_goal)?;
    let target_goal = if is_resume {
        source_goal.clone()
    } else {
        forked_session_goal(source_goal.clone())
    };
    let prepared_target_goal_digest = session_goal_digest(&target_goal)?;
    let base_message_count = source.as_ref().map_or(0, |session| session.messages.len());
    if let Some(path) = options.source_path.as_ref() {
        let session = source
            .as_ref()
            .ok_or_else(|| anyhow!("source path requires protected saved history"))?;
        ensure!(
            *path == sessions_dir.join(format!("{}.json", session.metadata.id)),
            "source path does not name the protected selected session; opaque legacy source retained"
        );
    }
    let updates = decode_history_overrides(
        &options.overrides,
        &runtime.read_config(),
        &request.workspace,
    )?;
    if let Some(session) = source.as_mut() {
        append_offered_history(session, &options.offered_history, &request.operation_key)?;
    } else {
        ensure!(
            options.offered_history.is_empty(),
            "offered history requires a full source"
        );
    }
    if let Some(session) = source.as_mut() {
        let journal = session
            .journal
            .as_ref()
            .ok_or_else(|| anyhow!("saved full journal is absent; source retained"))?;
        if let CanonicalThreadMutation::Fork {
            selected_entry_id, ..
        } = &request.mutation
        {
            let fork = match journal.fork_from(selected_entry_id.as_deref()) {
                Ok(fork) => fork,
                Err(error) => {
                    // An entry a bounded TUI save archived (#6842) is not in
                    // the document; fail closed and say where it went.
                    let dir = sessions_dir.clone();
                    let id = session.metadata.id.clone();
                    let entry = selected_entry_id.clone();
                    let archived = history_owner_work(move || {
                        let Some(entry) = entry else {
                            return Ok(false);
                        };
                        Ok(crate::session_manager::load_journal_archive(&dir, &id)?
                            .iter()
                            .any(|archived| archived.id == entry))
                    })
                    .await
                    .unwrap_or(false);
                    if archived {
                        bail!(
                            "{error}: the entry was moved to this session's journal archive to keep \
                             the saved document bounded; restore it with `/branch {}` first. \
                             Source retained",
                            selected_entry_id.as_deref().unwrap_or_default()
                        );
                    }
                    return Err(anyhow::Error::msg(error));
                }
            };
            session.metadata.mark_forked_from(&session.metadata.clone());
            session.metadata.spawn_depth = fork.spawn_depth;
            session.leaf_id = fork.leaf_id.clone();
            session.messages = fork.to_messages();
            session.journal = Some(fork);
            session.ensure_journal();
        }
    }
    let journal = source
        .as_ref()
        .and_then(|session| session.journal.clone())
        .unwrap_or_else(SessionJournal::new);
    ensure!(
        journal.entries.len() <= MAX_CANONICAL_HISTORY_ENTRIES,
        "complete graph exceeds entry bound"
    );
    // Import's strong validator is reused without changing ordinary module
    // limits or creating a new parser. It rejects duplicate/cycle/unknown graph.
    SavedSession::import_foreign(
        SessionImportContainer::new("canonical-operation-validation".into(), &journal, None),
        request.workspace.clone(),
        "validation".into(),
    )
    .map_err(anyhow::Error::msg)?;
    let history_digest = journal_digest(&journal)?;
    let mut witness = operation_witness(&journal, &request.workspace);
    if is_resume && source_thread.is_some() {
        witness.seed_from_message_index = base_message_count;
    }
    let target = if is_resume {
        match source_thread.as_ref() {
            Some(thread) => Some((
                thread.id.clone(),
                thread
                    .session_id
                    .clone()
                    .ok_or_else(|| anyhow!("resume holder has no durable session"))?,
            )),
            None => source.as_ref().map(|session| {
                (
                    format!("thr_{}", uuid::Uuid::new_v4()),
                    session.metadata.id.clone(),
                )
            }),
        }
    } else {
        None
    };
    let association = codewhale_protocol::CanonicalThreadOperationAssociation {
        kind: match &request.mutation {
            CanonicalThreadMutation::Create { .. } => {
                codewhale_protocol::CanonicalThreadOperationKind::Create
            }
            CanonicalThreadMutation::Resume { .. } => {
                codewhale_protocol::CanonicalThreadOperationKind::Resume
            }
            CanonicalThreadMutation::Fork { .. } => {
                codewhale_protocol::CanonicalThreadOperationKind::Fork
            }
        },
        source_runtime_thread_id: source_thread.as_ref().map(|thread| thread.id.clone()),
        source_session_id: source.as_ref().map(|session| session.metadata.id.clone()),
    };
    let reserve_runtime = Arc::clone(&runtime);
    let key = request.operation_key.clone();
    let operation = history_owner_work(move || {
        let operation = reserve_runtime.reserve_history_operation_for_target(
            &key,
            &request_digest,
            &history_digest,
            target
                .as_ref()
                .map(|(thread, session)| (thread.as_str(), session.as_str())),
        )?;
        reserve_runtime.bind_history_operation_witness(operation, witness, association)
    })
    .await?;
    create.workspace = Some(request.workspace.clone());
    if source_thread.is_none()
        && let Some(saved) = source.as_ref()
    {
        create.model = Some(saved.metadata.model.clone());
        create.model_provider = Some(saved.metadata.model_provider.clone());
        create.model_provider_id = saved.metadata.model_provider_id.clone();
        create.system_prompt = saved.system_prompt.clone();
    }
    if let Some(thread) = source_thread.as_ref() {
        create.model = Some(thread.model.clone());
        create.model_provider = thread.model_provider.clone();
        create.model_provider_id = thread.model_provider_id.clone();
        create.reasoning_effort = thread.reasoning_effort.clone();
        create.allowed_tools = thread.allowed_tools.clone();
        create.mode = Some(thread.mode.clone());
        create.permission_posture = thread.permission_posture.clone();
        create.allow_shell = Some(thread.allow_shell);
        create.trust_mode = Some(thread.trust_mode);
        create.auto_approve = Some(thread.auto_approve);
        create.system_prompt = thread.system_prompt.clone();
    }
    if let Some(updates) = updates.as_ref() {
        apply_history_create_overrides(&mut create, updates);
    }
    let lookup_runtime = Arc::clone(&runtime);
    let id = operation.receipt.runtime_thread_id.clone();
    let mut thread =
        match history_owner_work(move || lookup_runtime.history_operation_thread(&id)).await? {
            Some(thread) => {
                ensure!(
                    thread.workspace == request.workspace,
                    "reserved target workspace changed"
                );
                thread
            }
            None => {
                runtime
                    .create_thread_with_reserved_id(
                        create,
                        config_path.as_deref(),
                        config_profile.as_deref(),
                        Some(operation.receipt.runtime_thread_id.clone()),
                    )
                    .await?
            }
        };
    if is_resume && let Some(updates) = updates {
        thread = runtime
            .update_thread_under_history_guard(
                &thread.id,
                updates,
                config_path.as_deref(),
                config_profile.as_deref(),
            )
            .await?;
    }
    let source_id = source.as_ref().map(|session| session.metadata.id.clone());
    let mut session = source.unwrap_or_else(|| {
        crate::session_manager::create_saved_session_with_id_and_mode(
            operation.receipt.session_id.clone(),
            &[],
            &thread.model,
            &thread.workspace,
            0,
            None,
            Some(&thread.mode),
        )
    });
    session.metadata.id = operation.receipt.session_id.clone();
    if !is_resume {
        session.metadata.created_at = operation.created_at;
    }
    session.metadata.updated_at = operation.created_at;
    session.system_prompt.clone_from(&thread.system_prompt);
    session.metadata.runtime_store = Some(runtime.session_store_binding());
    session.metadata.workspace = thread.workspace.clone();
    session.metadata.model = thread.model.clone();
    session.metadata.mode = Some(thread.mode.clone());
    session.metadata.set_model_provider_route(
        thread
            .model_provider
            .as_deref()
            .ok_or_else(|| anyhow!("canonical thread has no provider identity"))?,
        thread.model_provider_id.as_deref(),
    );
    let document_digest = saved_document_digest(&session)?;
    let prepare_runtime = Arc::clone(&runtime);
    let operation = history_owner_work(move || {
        let operation =
            prepare_runtime.bind_history_operation_document(operation, &document_digest)?;
        prepare_runtime.bind_history_operation_session_goal(
            operation,
            &captured_source_goal_digest,
            &prepared_target_goal_digest,
        )
    })
    .await?;
    // Revalidate the exact source file immediately before target publication.
    // Both existing session leases are held until the target write settles.
    let dir = sessions_dir.clone();
    let expected_journal = journal.clone();
    let source_goal_digest = session_goal_digest(&source_goal)?;
    let target_goal_digest = session_goal_digest(&target_goal)?;
    let target_id = session.metadata.id.clone();
    let (session, _source_lease, _target_lease) = history_owner_work(move || {
        let manager = SessionManager::new(dir)?;
        let source_lease = source_id
            .as_ref()
            .filter(|id| **id != target_id)
            .map(|id| manager.reserve_session_for_external_write(id))
            .transpose()?;
        let target_lease = manager.reserve_session_for_external_write(&target_id)?;
        if let (Some(id), Some(digest)) = (source_id.as_ref(), source_disk_digest.as_ref()) {
            let observed =
                manager.load_session_snapshot_bounded(id, MAX_CANONICAL_HISTORY_BYTES)?;
            ensure!(
                session_goal_digest(&manager.load_session_goal(id)?)? == source_goal_digest,
                "source local goal changed before publication"
            );
            ensure!(
                saved_document_digest(&observed)? == *digest,
                "source full document changed before publication"
            );
        }
        if source_id.as_ref() != Some(&target_id) {
            let previous_goal = manager.load_session_goal(&target_id)?;
            ensure!(
                previous_goal.is_none()
                    || session_goal_digest(&previous_goal)? == target_goal_digest,
                "reserved target has a conflicting local goal sidecar; source retained"
            );
        }
        match manager.load_session_snapshot_bounded(&target_id, MAX_CANONICAL_HISTORY_BYTES) {
            Ok(observed) if source_id.as_ref() != Some(&target_id) => {
                ensure!(
                    observed.journal.as_ref() == Some(&expected_journal)
                        && observed.metadata.runtime_store == session.metadata.runtime_store,
                    "reserved canonical target changed; recovery required"
                );
                ensure!(
                    session_goal_digest(&manager.load_session_goal(&target_id)?)?
                        == target_goal_digest,
                    "reserved target local goal changed; recovery required"
                );
                Ok((observed, source_lease, target_lease))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                manager.save_session(&session)?;
                manager.save_session_goal(&target_id, target_goal.as_ref())?;
                Ok((session, source_lease, target_lease))
            }
            Ok(_) => {
                manager.save_session(&session)?;
                manager.save_session_goal(&target_id, target_goal.as_ref())?;
                Ok((session, source_lease, target_lease))
            }
            Err(error) => Err(error.into()),
        }
    })
    .await?;
    let detail = runtime.get_thread_detail(&thread.id).await?;
    ensure!(
        !super::sessions::thread_detail_has_live_work(&detail),
        "canonical target has pending work"
    );
    if detail.turns.is_empty() && detail.items.is_empty() {
        runtime
            .seed_thread_from_messages_with_history_operation(
                &thread.id,
                &session.messages,
                Some(&operation),
                &session.messages,
            )
            .await?;
    } else {
        ensure!(
            is_resume,
            "uncommitted new target has unproven history; recovery required"
        );
        let from = operation
            .journal_witness
            .as_ref()
            .ok_or_else(|| anyhow!("history witness absent"))?
            .seed_from_message_index;
        ensure!(
            from <= session.messages.len(),
            "history suffix boundary is invalid"
        );
        if from < session.messages.len() {
            runtime
                .seed_thread_from_messages_with_history_operation(
                    &thread.id,
                    &session.messages[from..],
                    Some(&operation),
                    &session.messages,
                )
                .await?;
        }
    }
    runtime
        .set_thread_session_checkpoint(&thread.id, &session)
        .await?;
    runtime
        .synchronize_history_operation(&thread.id, &session)
        .await?;
    let commit_runtime = Arc::clone(&runtime);
    history_owner_work(move || commit_runtime.commit_history_operation(operation)).await
}

pub(super) async fn lookup_thread_history_operation(
    State(state): State<RuntimeApiState>,
    Json(request): Json<codewhale_protocol::CanonicalThreadOperationLookup>,
) -> Result<Json<codewhale_protocol::CanonicalThreadOperationStatus>, ApiError> {
    lookup_thread_history_operation_in_runtime(&state.runtime_threads, &state.sessions_dir, request)
        .await
        .map(Json)
        .map_err(|error| ApiError::conflict(error.to_string()))
}

/// A read-only exact-key lookup never resumes an Engine, mutates a thread, or
/// guesses absence from a damaged intent/document. It authenticates current
/// binding separately from the historical receipt.
pub(crate) async fn lookup_thread_history_operation_in_runtime(
    runtime: &Arc<RuntimeThreadManager>,
    sessions_dir: &Path,
    request: codewhale_protocol::CanonicalThreadOperationLookup,
) -> Result<codewhale_protocol::CanonicalThreadOperationStatus> {
    let binding = runtime.session_store_binding();
    ensure!(
        request.version == 1
            && request.expected_data_dir == binding.data_dir
            && request.expected_execution_scope == binding.execution_scope,
        "operation lookup belongs to a different held owner"
    );
    let manager = Arc::clone(runtime);
    let key = request.operation_key.clone();
    let operation =
        history_owner_work(move || manager.lookup_history_operation_by_key(&key)).await?;
    let Some(operation) = operation else {
        return Ok(codewhale_protocol::CanonicalThreadOperationStatus::Absent);
    };
    ensure!(
        operation
            .journal_witness
            .as_ref()
            .is_some_and(|witness| witness.workspace == request.workspace),
        "operation belongs to another acknowledged workspace"
    );
    let association = operation.association.clone().ok_or_else(|| {
        anyhow!("legacy operation lacks a typed action association; recovery required")
    })?;
    if !operation.committed {
        return Ok(
            codewhale_protocol::CanonicalThreadOperationStatus::Pending {
                receipt: operation.receipt,
                association,
            },
        );
    }
    let admission = runtime.try_history_import_guard()?;
    let manager = Arc::clone(runtime);
    let dir = sessions_dir.to_path_buf();
    history_owner_work(move || {
        let _admission = admission;
        let thread = manager
            .history_operation_thread(&operation.receipt.runtime_thread_id)?
            .ok_or_else(|| anyhow!("committed operation target is absent; recovery required"))?;
        ensure!(
            thread.workspace == request.workspace,
            "committed target belongs to another selected workspace"
        );
        verify_operation_checkpoint(&SessionManager::new(dir)?, &thread, &binding, &operation)?;
        Ok(
            codewhale_protocol::CanonicalThreadOperationStatus::Committed {
                receipt: operation.receipt,
                association,
            },
        )
    })
    .await
}

pub(super) async fn recover_thread_history_operation(
    State(state): State<RuntimeApiState>,
    Json(request): Json<codewhale_protocol::CanonicalThreadOperationRecovery>,
) -> Result<Json<codewhale_protocol::CanonicalThreadOperationStatus>, ApiError> {
    recover_thread_history_operation_in_runtime(
        &state.runtime_threads,
        &state.sessions_dir,
        request,
    )
    .await
    .map(Json)
    .map_err(|error| ApiError::conflict(error.to_string()))
}

/// Explicit exact-key recovery settles already-published preparation; it never
/// reconstructs an intent from a changed source. Lookup remains read-only.
pub(crate) async fn recover_thread_history_operation_in_runtime(
    runtime: &Arc<RuntimeThreadManager>,
    sessions_dir: &Path,
    request: codewhale_protocol::CanonicalThreadOperationRecovery,
) -> Result<codewhale_protocol::CanonicalThreadOperationStatus> {
    let binding = runtime.session_store_binding();
    ensure!(
        request.operation.version == 1
            && request.operation.expected_data_dir == binding.data_dir
            && request.operation.expected_execution_scope == binding.execution_scope,
        "operation recovery belongs to a different held owner"
    );
    let admission = runtime.try_history_import_guard()?;
    let runtime = Arc::clone(runtime);
    let dir = sessions_dir.to_path_buf();
    tokio::spawn(async move {
        let _admission = admission;
        let captured = Arc::clone(&runtime);
        let key = request.operation.operation_key.clone();
        let Some(operation) =
            history_owner_work(move || captured.lookup_history_operation_by_key(&key)).await?
        else {
            return Ok(codewhale_protocol::CanonicalThreadOperationStatus::Absent);
        };
        ensure!(
            operation.association.as_ref() == Some(&request.association),
            "retained operation action/source association differs; no recovery write admitted"
        );
        ensure!(
            operation
                .journal_witness
                .as_ref()
                .is_some_and(|witness| witness.workspace == request.operation.workspace),
            "operation belongs to another acknowledged workspace"
        );
        let committed = if operation.committed {
            let captured = Arc::clone(&runtime);
            let expected = operation.clone();
            let workspace = request.operation.workspace.clone();
            history_owner_work(move || {
                let thread = captured
                    .history_operation_thread(&expected.receipt.runtime_thread_id)?
                    .ok_or_else(|| anyhow!("committed operation target is absent"))?;
                ensure!(
                    thread.workspace == workspace,
                    "operation target workspace changed"
                );
                verify_operation_checkpoint(
                    &SessionManager::new(dir)?,
                    &thread,
                    &binding,
                    &expected,
                )?;
                Ok(expected.receipt)
            })
            .await?
        } else {
            let Some(receipt) = settle_prepared_history_operation(
                &runtime,
                &dir,
                &operation,
                &request.operation.workspace,
            )
            .await?
            else {
                return Ok(
                    codewhale_protocol::CanonicalThreadOperationStatus::Pending {
                        receipt: operation.receipt,
                        association: request.association,
                    },
                );
            };
            receipt
        };
        Ok(
            codewhale_protocol::CanonicalThreadOperationStatus::Committed {
                receipt: committed,
                association: request.association,
            },
        )
    })
    .await
    .context("operation recovery outcome is uncertain; retain the same operation key")?
}

fn append_offered_history(
    session: &mut SavedSession,
    values: &[serde_json::Value],
    key: &str,
) -> Result<()> {
    let offered: Vec<Message> = values
        .iter()
        .map(|value| {
            let message: Message = serde_json::from_value(value.clone()).context(
                "opaque offered history retained; only complete model messages are admitted",
            )?;
            ensure!(
                serde_json::to_value(&message)? == *value,
                "offered message has unsupported fields"
            );
            if message.role == Role::User {
                crate::image_attach::runtime_images_from_blocks(&message.content)
                    .map_err(anyhow::Error::msg)?;
            }
            Ok(message)
        })
        .collect::<Result<_>>()?;
    let overlap = codewhale_core::persisted_overlap(&session.messages, &offered);
    let journal = session
        .journal
        .as_mut()
        .ok_or_else(|| anyhow!("full journal missing"))?;
    ensure!(
        journal
            .entries
            .len()
            .checked_add(offered.len() - overlap)
            .is_some_and(|count| count <= MAX_CANONICAL_HISTORY_ENTRIES),
        "complete offered history exceeds entry bound"
    );
    for (index, message) in offered.into_iter().enumerate().skip(overlap) {
        journal.append_stamped(
            SessionEntryKind::Message { message },
            session.metadata.updated_at,
        );
        let entry = journal
            .entries
            .last_mut()
            .ok_or_else(|| anyhow!("offered journal append missing"))?;
        entry.id = format!(
            "offered:{}:{index}",
            codewhale_runtime::hashing::sha256_hex(key.as_bytes())
        );
        journal.leaf_id = Some(entry.id.clone());
    }
    session.leaf_id = journal.leaf_id.clone();
    // Publish the same active branch/count that the protected reader restores,
    // before retaining the full-document recovery witness.
    session.ensure_journal();
    Ok(())
}

fn decode_history_overrides(
    value: &serde_json::Value,
    config: &codewhale_runtime::config::Config,
    workspace: &Path,
) -> Result<Option<crate::runtime_threads::UpdateThreadRequest>> {
    if value.is_null() {
        return Ok(None);
    }
    let fields = value
        .as_object()
        .ok_or_else(|| anyhow!("history overrides must be an object"))?;
    let mut wire = serde_json::Map::new();
    let mut prompt_parts = Vec::new();
    for (key, value) in fields {
        match key.as_str() {
            "model" | "model_provider" | "model_provider_id" | "mode" | "permission_posture"
            | "allow_shell" | "trust_mode" | "auto_approve" | "system_prompt" => {
                ensure!(
                    wire.insert(key.clone(), value.clone()).is_none(),
                    "conflicting history configuration fields"
                );
            }
            "cwd" | "workspace" => {
                let selected: std::path::PathBuf = serde_json::from_value(value.clone())?;
                ensure!(
                    selected == workspace,
                    "history workspace override differs from selected owner scope"
                );
            }
            "approval_policy" => {
                let policy = value
                    .as_str()
                    .ok_or_else(|| anyhow!("approval policy must be a string"))?;
                let posture = match policy.trim().to_ascii_lowercase().as_str() {
                    "ask" | "suggest" | "on-request" | "untrusted" => "ask",
                    "auto" | "auto-review" | "auto_review" => "auto_review",
                    "full" | "full-access" | "full_access" | "bypass" | "never" => "full_access",
                    _ => {
                        return Err(anyhow!(
                            "unsupported legacy approval policy; owner policy retained"
                        ));
                    }
                };
                ensure!(
                    wire.insert("permission_posture".into(), serde_json::json!(posture))
                        .is_none(),
                    "conflicting history approval policy fields"
                );
            }
            "sandbox" => ensure!(
                value.as_str() == config.sandbox_mode.as_deref(),
                "legacy sandbox override conflicts with the captured owner sandbox ceiling"
            ),
            "config" => {
                let nested = value
                    .as_object()
                    .ok_or_else(|| anyhow!("history config must be a typed override object"))?;
                for (name, setting) in nested {
                    ensure!(
                        matches!(
                            name.as_str(),
                            "model"
                                | "model_provider"
                                | "model_provider_id"
                                | "mode"
                                | "permission_posture"
                                | "allow_shell"
                                | "trust_mode"
                                | "auto_approve"
                                | "system_prompt"
                        ),
                        "unsupported history config field; no credentials or global policy are imported"
                    );
                    ensure!(
                        wire.insert(name.clone(), setting.clone()).is_none(),
                        "conflicting history configuration fields"
                    );
                }
            }
            "base_instructions" | "developer_instructions" | "personality" => {
                let text = value
                    .as_str()
                    .ok_or_else(|| anyhow!("history instruction must be a string"))?;
                prompt_parts.push(format!("{key}:\n{text}"));
            }
            _ => {
                return Err(anyhow!(
                    "unsupported history override {key}; source retained"
                ));
            }
        }
    }
    if !prompt_parts.is_empty() {
        ensure!(
            !wire.contains_key("system_prompt"),
            "conflicting history instruction fields"
        );
        wire.insert(
            "system_prompt".into(),
            serde_json::json!(prompt_parts.join("\n\n")),
        );
    }
    if wire.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_value(serde_json::Value::Object(
        wire,
    ))?))
}

fn apply_history_create_overrides(
    create: &mut CreateThreadRequest,
    update: &crate::runtime_threads::UpdateThreadRequest,
) {
    if update.model.is_some() {
        create.model.clone_from(&update.model);
    }
    if update.model_provider.is_some() {
        create.model_provider.clone_from(&update.model_provider);
        create
            .model_provider_id
            .clone_from(&update.model_provider_id);
    } else if update.model_provider_id.is_some() {
        create
            .model_provider_id
            .clone_from(&update.model_provider_id);
    }
    if update.mode.is_some() {
        create.mode.clone_from(&update.mode);
    }
    if update.permission_posture.is_some() {
        create
            .permission_posture
            .clone_from(&update.permission_posture);
    }
    if update.allow_shell.is_some() {
        create.allow_shell = update.allow_shell;
    }
    if update.trust_mode.is_some() {
        create.trust_mode = update.trust_mode;
    }
    if update.auto_approve.is_some() {
        create.auto_approve = update.auto_approve;
    }
    if update.system_prompt.is_some() {
        create.system_prompt.clone_from(&update.system_prompt);
    }
}

pub(crate) struct HistorySizeBound(pub(crate) usize);
impl std::io::Write for HistorySizeBound {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "full history exceeds byte transport bound; source retained",
            ));
        }
        self.0 -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod graph_tests {
    use super::*;
    use codewhale_protocol::MessageRecord;

    fn history() -> LegacyThreadHistory {
        LegacyThreadHistory {
            goal: None,
            version: 1,
            state_store_id: "a".repeat(64),
            thread_id: "legacy".into(),
            current_leaf_id: Some(3),
            messages: [
                (1, None, "system"),
                (2, Some(1), "user"),
                (3, Some(2), "assistant"),
                (4, Some(1), "user"),
            ]
            .into_iter()
            .map(|(id, parent_entry_id, role)| MessageRecord {
                id,
                thread_id: "legacy".into(),
                role: role.into(),
                content: format!("content-{id}"),
                item: None,
                created_at: 1_700_000_000 + id,
                parent_entry_id,
            })
            .collect(),
        }
    }

    #[test]
    fn history_override_conflicts_refuse_every_insert_and_preserve_full_access_posture() {
        let config = codewhale_runtime::config::Config::default();
        let workspace = std::path::PathBuf::from("/checked-workspace");
        for value in [
            serde_json::json!({"config":{"model":"first"},"model":"second"}),
            serde_json::json!({"approval_policy":"on-request","permission_posture":"full_access"}),
            serde_json::json!({"config":{"system_prompt":"first"},"system_prompt":"second"}),
            serde_json::json!({"base_instructions":"first","system_prompt":"second"}),
        ] {
            assert!(decode_history_overrides(&value, &config, &workspace).is_err());
        }
        for policy in ["full-access", "never"] {
            let update = decode_history_overrides(
                &serde_json::json!({"approval_policy":policy}),
                &config,
                &workspace,
            )
            .unwrap()
            .unwrap();
            assert_eq!(update.permission_posture.as_deref(), Some("full_access"));
        }
        assert!(
            decode_history_overrides(
                &serde_json::json!({"approval_policy":"never","permission_posture":"ask"}),
                &config,
                &workspace,
            )
            .is_err()
        );
    }

    #[test]
    fn full_history_measurement_refuses_before_allocating_a_truncated_value() {
        let mut size = HistorySizeBound(32);
        assert!(serde_json::to_writer(&mut size, &"x".repeat(64)).is_err());
        assert!(size.0 <= 32);
        let mut size = HistorySizeBound(4);
        serde_json::to_writer(&mut size, &"ok").unwrap();
        assert_eq!(size.0, 0);
    }

    #[test]
    fn import_graph_preserves_all_branches_stamps_and_selected_system_prefix() {
        let source = history();
        let before = serde_json::to_value(&source).unwrap();
        let graph = legacy_journal(&source).unwrap();
        assert_eq!(graph.entries.len(), 4);
        assert_eq!(graph.to_messages().len(), 3);
        assert_eq!(graph.to_messages()[0].role, Role::System);
        assert_eq!(graph.entries[3].parent_id, graph.entries[1].parent_id);
        assert_eq!(graph.entries[3].created_at.timestamp(), 1_700_000_004);
        assert_eq!(before, serde_json::to_value(source).unwrap());
    }

    #[test]
    fn import_graph_refuses_cycles_duplicates_dangling_foreign_and_opaque_rows() {
        let original = history();
        let mut cases = Vec::new();
        let mut graph = original.clone();
        graph.messages[0].parent_entry_id = Some(3);
        cases.push(graph);
        let mut graph = original.clone();
        graph.messages[3].id = 2;
        cases.push(graph);
        let mut graph = original.clone();
        graph.messages[3].parent_entry_id = Some(99);
        cases.push(graph);
        let mut graph = original.clone();
        graph.messages[3].thread_id = "foreign".into();
        cases.push(graph);
        let mut graph = original.clone();
        graph.messages[3].item = Some(serde_json::json!({"unknown":"opaque"}));
        cases.push(graph);
        let mut graph = original.clone();
        graph.current_leaf_id = None;
        cases.push(graph);
        let mut graph = original.clone();
        graph.version = 2;
        cases.push(graph);
        for source in cases {
            let before = serde_json::to_value(&source).unwrap();
            assert!(legacy_journal(&source).is_err());
            assert_eq!(
                before,
                serde_json::to_value(source).unwrap(),
                "refusal preserves source"
            );
        }
    }
}
