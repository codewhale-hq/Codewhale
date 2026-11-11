use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use codewhale_engine::runtime_threads::job_operations::{
    JobOperationTransaction, MEMBER_ABANDONED, MEMBER_ACCEPTED, MEMBER_RESERVED, MEMBER_UNKNOWN,
    MemberOperation, MemberOperationRecord, member_request_fingerprint, new_member_agent_id,
    valid_member_key,
};
use codewhale_engine::tools::subagent::AgentWorkerRecord;

use super::jobs::operation_error;
use super::{ApiError, RuntimeApiState, map_thread_err};

const MEMBER_START_WAIT: Duration = Duration::from_secs(20);
pub(super) const MAX_MEMBER_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_MEMBER_PROMPT_CHARS: usize = 256_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StartMemberRequest {
    operation_key: String,
    member_id: String,
    display_name: String,
    prompt: String,
    fleet: codewhale_engine::fleet::store::FleetFile,
    #[serde(default)]
    expected_store_id: Option<String>,
    #[serde(default)]
    request_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct MemberReceipt {
    thread_id: String,
    member_id: String,
    operation_key: String,
    request_fingerprint: String,
    runtime_store_id: String,
    agent_id: String,
    state: String,
}

impl MemberReceipt {
    fn new(record: &MemberOperationRecord, state: &str) -> Self {
        Self {
            thread_id: record.operation.thread_id.clone(),
            member_id: record.operation.member_id.clone(),
            operation_key: record.operation.operation_key.clone(),
            request_fingerprint: record.request_fingerprint.clone(),
            runtime_store_id: record.operation.store_id.clone(),
            agent_id: record.operation.agent_id.clone(),
            state: state.to_string(),
        }
    }

    fn status(&self) -> StatusCode {
        if self.state == MEMBER_ACCEPTED {
            StatusCode::OK
        } else {
            StatusCode::ACCEPTED
        }
    }
}

fn worker_proves(operation: &MemberOperation, worker: Option<&AgentWorkerRecord>) -> bool {
    worker.is_some_and(|worker| {
        worker.owner_session_id == operation.thread_id
            && worker.spec.worker_id == operation.agent_id
            && worker
                .spec
                .child_route
                .as_ref()
                .and_then(|route| route.requested_profile.as_deref())
                == Some(operation.member_id.as_str())
    })
}

fn project(
    transaction: &JobOperationTransaction<'_>,
    record: &mut MemberOperationRecord,
    worker: Option<&AgentWorkerRecord>,
    generation: &str,
) -> anyhow::Result<String> {
    let proven = worker_proves(&record.operation, worker);
    match record.operation.state.as_str() {
        MEMBER_ACCEPTED if proven => Ok(MEMBER_ACCEPTED.to_string()),
        MEMBER_ACCEPTED => Ok(MEMBER_UNKNOWN.to_string()),
        MEMBER_RESERVED if proven => {
            settle(transaction, record, MEMBER_ACCEPTED)?;
            Ok(MEMBER_ACCEPTED.to_string())
        }
        MEMBER_RESERVED if record.owner_generation != generation => {
            settle(transaction, record, MEMBER_ABANDONED)?;
            Ok(MEMBER_ABANDONED.to_string())
        }
        MEMBER_RESERVED => Ok(MEMBER_UNKNOWN.to_string()),
        state => Ok(state.to_string()),
    }
}

fn settle(
    transaction: &JobOperationTransaction<'_>,
    record: &mut MemberOperationRecord,
    state: &str,
) -> anyhow::Result<()> {
    record.operation.state = state.to_string();
    record.operation.updated_at_ms = chrono::Utc::now()
        .timestamp_millis()
        .max(record.operation.created_at_ms);
    transaction.save_member(record)
}

async fn member_worker(
    state: &RuntimeApiState,
    thread_id: &str,
    agent_id: &str,
) -> Option<AgentWorkerRecord> {
    state
        .sub_agent_manager
        .read()
        .await
        .get_worker_record_for_session(thread_id, agent_id)
}

async fn settle_outcome(
    state: RuntimeApiState,
    operation: &MemberOperation,
    outcome: Option<Result<String, String>>,
) -> Result<(), ApiError> {
    let worker = match &outcome {
        Some(Ok(agent_id)) => member_worker(&state, &operation.thread_id, agent_id).await,
        _ => None,
    };
    let id = operation.id.clone();
    let store = state.runtime_threads.job_operation_store();
    tokio::task::spawn_blocking(move || {
        store.with_job_operations(|transaction| {
            let Some(mut record) = transaction.get_member(&id)? else {
                return Ok(());
            };
            if record.operation.state != MEMBER_RESERVED {
                return Ok(());
            }
            match outcome {
                Some(Ok(agent_id))
                    if agent_id == record.operation.agent_id
                        && worker_proves(&record.operation, worker.as_ref()) =>
                {
                    settle(transaction, &mut record, MEMBER_ACCEPTED)
                }
                Some(Err(_)) => transaction.remove_member(&id),
                _ => Ok(()),
            }
        })
    })
    .await
    .map_err(|_| ApiError::internal("member operation settlement failed"))?
    .map_err(operation_error)
}

fn validate(request: &StartMemberRequest) -> Result<(String, String), ApiError> {
    if !valid_member_key(&request.operation_key) || !valid_member_key(&request.member_id) {
        return Err(ApiError::bad_request(
            "operation_key and member_id must be 1-128 URL-safe characters",
        ));
    }
    let display_name = request.display_name.trim();
    if display_name.is_empty()
        || display_name.chars().count() > 80
        || display_name.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request(
            "display_name must be one printable line of at most 80 characters",
        ));
    }
    let prompt = request.prompt.trim();
    if prompt.is_empty() || prompt.chars().count() > MAX_MEMBER_PROMPT_CHARS {
        return Err(ApiError::bad_request(
            "prompt must contain 1-256000 characters",
        ));
    }
    Ok((display_name.to_string(), prompt.to_string()))
}

pub(super) async fn start_thread_member(
    State(state): State<RuntimeApiState>,
    Path(thread_id): Path<String>,
    Json(request): Json<StartMemberRequest>,
) -> Result<(StatusCode, Json<MemberReceipt>), ApiError> {
    let (display_name, prompt) = validate(&request)?;
    let store_id = state.runtime_threads.job_operation_store_id().to_string();
    if request
        .expected_store_id
        .as_deref()
        .is_some_and(|expected| expected != store_id)
    {
        return Err(ApiError::conflict("member operation store changed")
            .with_code("member_operation_store_changed"));
    }
    let fingerprint = member_request_fingerprint(
        &thread_id,
        &request.member_id,
        &display_name,
        &prompt,
        &request.fleet,
    )
    .map_err(|_| ApiError::internal("member request fingerprint failed"))?;
    if request
        .request_fingerprint
        .as_deref()
        .is_some_and(|claimed| claimed != fingerprint)
    {
        return Err(
            ApiError::bad_request("request_fingerprint does not match this request")
                .with_code("member_request_fingerprint_mismatch"),
        );
    }
    state
        .runtime_threads
        .get_thread(&thread_id)
        .await
        .map_err(map_thread_err)?;
    let generation = state
        .runtime_threads
        .job_operation_generation()
        .map_err(operation_error)?;
    let store = state.runtime_threads.job_operation_store();
    let key = request.operation_key.clone();
    let lookup_thread = thread_id.clone();
    let existing = {
        let store = store.clone();
        tokio::task::spawn_blocking(move || {
            store.with_job_operations(|transaction| {
                let id = transaction.member_operation_id(&lookup_thread, &key)?;
                transaction.get_member(&id)
            })
        })
        .await
        .map_err(|_| ApiError::internal("member operation lookup failed"))?
        .map_err(operation_error)?
    };
    if let Some(existing) = existing {
        if existing.request_fingerprint != fingerprint {
            return Err(
                ApiError::conflict("member operation key belongs to a different request")
                    .with_code("member_operation_mismatch"),
            );
        }
        let receipt = replay(&state, existing, generation).await?;
        if receipt.state == MEMBER_ABANDONED {
            return Err(ApiError::conflict(
                "member operation was abandoned before it started; use a new operation_key",
            )
            .with_code("member_operation_abandoned"));
        }
        return Ok((receipt.status(), Json(receipt)));
    }

    let reserve_thread = thread_id.clone();
    let reserve_key = request.operation_key.clone();
    let reserve_member = request.member_id.clone();
    let reserve_fingerprint = fingerprint.clone();
    let reserve_generation = generation.clone();
    let reserved = {
        let store = store.clone();
        tokio::task::spawn_blocking(move || {
            store.with_job_operations(|transaction| {
                let id = transaction.member_operation_id(&reserve_thread, &reserve_key)?;
                if let Some(existing) = transaction.get_member(&id)? {
                    return Ok(Err(existing));
                }
                let now = chrono::Utc::now().timestamp_millis();
                let record = MemberOperationRecord {
                    schema_version: 1,
                    operation: MemberOperation {
                        id,
                        store_id,
                        thread_id: reserve_thread,
                        member_id: reserve_member,
                        operation_key: reserve_key,
                        agent_id: new_member_agent_id(),
                        state: MEMBER_RESERVED.to_string(),
                        created_at_ms: now,
                        updated_at_ms: now,
                    },
                    request_fingerprint: reserve_fingerprint,
                    owner_generation: reserve_generation,
                };
                transaction.save_member(&record)?;
                Ok(Ok(record))
            })
        })
        .await
        .map_err(|_| ApiError::internal("member operation reservation failed"))?
        .map_err(operation_error)?
    };
    let record = match reserved {
        Ok(record) => record,
        Err(existing) => {
            if existing.request_fingerprint != fingerprint {
                return Err(ApiError::conflict(
                    "member operation key belongs to a different request",
                )
                .with_code("member_operation_mismatch"));
            }
            let receipt = replay(&state, existing, generation).await?;
            return Ok((receipt.status(), Json(receipt)));
        }
    };

    let engine = match state.runtime_threads.get_engine(&thread_id).await {
        Ok(engine) => engine,
        Err(error) => {
            settle_outcome(state.clone(), &record.operation, Some(Err(String::new()))).await?;
            return Err(map_thread_err(error));
        }
    };
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    if let Err(error) = engine
        .send(codewhale_engine::core::ops::Op::StartFleetMember {
            start: Box::new(codewhale_engine::tools::subagent::FleetMemberStart {
                operation_key: request.operation_key.clone(),
                fleet: request.fleet,
                member_id: request.member_id.clone(),
                display_name,
                prompt,
                reserved_agent_id: Some(record.operation.agent_id.clone()),
            }),
            tx: std::sync::Arc::new(std::sync::Mutex::new(Some(tx))),
        })
        .await
    {
        settle_outcome(state.clone(), &record.operation, Some(Err(String::new()))).await?;
        return Err(ApiError::internal(format!(
            "Failed to reach the thread's engine: {error}"
        )));
    }
    match tokio::time::timeout(MEMBER_START_WAIT, &mut rx).await {
        Ok(Ok(Ok(agent_id))) => {
            let settled = settle_outcome(state.clone(), &record.operation, Some(Ok(agent_id)))
                .await
                .is_ok();
            let receipt = if settled {
                replay(&state, record.clone(), generation)
                    .await
                    .unwrap_or_else(|_| MemberReceipt::new(&record, MEMBER_UNKNOWN))
            } else {
                MemberReceipt::new(&record, MEMBER_UNKNOWN)
            };
            Ok((receipt.status(), Json(receipt)))
        }
        Ok(Ok(Err(message))) => {
            settle_outcome(state.clone(), &record.operation, Some(Err(message.clone()))).await?;
            Err(ApiError::conflict(message))
        }
        Ok(Err(_)) => Err(ApiError::internal(
            "The engine closed before starting the member",
        )),
        Err(_) => {
            let operation = record.operation.clone();
            let background = state.clone();
            tokio::spawn(async move {
                let outcome = rx.await.ok();
                if let Err(error) = settle_outcome(background, &operation, outcome).await {
                    tracing::warn!(error = %error.message, "member operation settlement failed");
                }
            });
            let receipt = MemberReceipt::new(&record, MEMBER_UNKNOWN);
            Ok((receipt.status(), Json(receipt)))
        }
    }
}

async fn replay(
    state: &RuntimeApiState,
    record: MemberOperationRecord,
    generation: String,
) -> Result<MemberReceipt, ApiError> {
    let worker = member_worker(
        state,
        &record.operation.thread_id,
        &record.operation.agent_id,
    )
    .await;
    let store = state.runtime_threads.job_operation_store();
    let id = record.operation.id.clone();
    tokio::task::spawn_blocking(move || {
        store.with_job_operations(|transaction| {
            let mut record = transaction.get_member(&id)?.unwrap_or(record);
            let state = project(transaction, &mut record, worker.as_ref(), &generation)?;
            Ok(MemberReceipt::new(&record, &state))
        })
    })
    .await
    .map_err(|_| ApiError::internal("member operation lookup failed"))?
    .map_err(operation_error)
}

pub(super) async fn get_member_operation(
    State(state): State<RuntimeApiState>,
    Path((thread_id, key)): Path<(String, String)>,
) -> Result<Json<MemberReceipt>, ApiError> {
    if !valid_member_key(&key) {
        return Err(ApiError::bad_request("invalid member operation key"));
    }
    state
        .runtime_threads
        .get_thread(&thread_id)
        .await
        .map_err(map_thread_err)?;
    let generation = state
        .runtime_threads
        .job_operation_generation()
        .map_err(operation_error)?;
    let store = state.runtime_threads.job_operation_store();
    let record = tokio::task::spawn_blocking(move || {
        store.with_job_operations(|transaction| {
            let id = transaction.member_operation_id(&thread_id, &key)?;
            transaction.get_member(&id)
        })
    })
    .await
    .map_err(|_| ApiError::internal("member operation lookup failed"))?
    .map_err(operation_error)?
    .ok_or_else(|| ApiError::not_found("member operation not found"))?;
    Ok(Json(replay(&state, record, generation).await?))
}
