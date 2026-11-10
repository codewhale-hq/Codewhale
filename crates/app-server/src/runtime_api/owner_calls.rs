use super::*;
use crate::{RuntimeOwnerFailure, RuntimeOwnerRequest};

async fn validate(state: &RuntimeApiState) -> Result<()> {
    anyhow::ensure!(
        !state.runtime_threads.is_acp_host(),
        "immutable ACP-base owner cannot admit ordinary native services"
    );
    state
        .workspace_scope
        .validate()
        .await
        .map_err(|_| RuntimeOwnerFailure {
            status: 409,
            message: "selected workspace identity changed".to_owned(),
            code: None,
        })?;
    anyhow::ensure!(
        !state.shutdown.requested.is_cancelled(),
        "captured Runtime is stopping"
    );
    Ok(())
}

fn api<T>(result: std::result::Result<T, ApiError>) -> Result<T> {
    result.map_err(|error| {
        RuntimeOwnerFailure {
            status: error.status.as_u16(),
            message: error.message,
            code: error.code.map(str::to_owned),
        }
        .into()
    })
}

fn decode<T: serde::de::DeserializeOwned>(body: Value) -> Result<T> {
    serde_json::from_value(body).map_err(|error| {
        RuntimeOwnerFailure {
            status: 400,
            message: error.to_string(),
            code: None,
        }
        .into()
    })
}

fn user_input_receipt(input_id: String, delivered: bool) -> Result<Value, ApiError> {
    if !delivered {
        return Err(ApiError::not_found(format!(
            "no pending user-input request with id '{input_id}'"
        )));
    }
    serde_json::to_value(SubmitUserInputResponse {
        ok: true,
        input_id,
        delivered,
    })
    .map_err(|error| ApiError::internal(error.to_string()))
}

pub(super) async fn execute(state: RuntimeApiState, request: RuntimeOwnerRequest) -> Result<Value> {
    validate(&state).await?;
    Ok(match request {
        RuntimeOwnerRequest::Capabilities => {
            json!({ "capabilities": default_runtime_capabilities() })
        }
        RuntimeOwnerRequest::Providers => {
            let Json(value) = api(list_providers(State(state)).await)?;
            serde_json::to_value(value)?
        }
        RuntimeOwnerRequest::ProviderModels {
            provider,
            provider_id,
            cursor,
        } => {
            let Json(value) = api(list_provider_models(
                State(state),
                Path(provider),
                Query(ListProviderModelsParams {
                    model_provider_id: provider_id,
                    cursor,
                    filter: None,
                    limit: Some(250),
                }),
            )
            .await)?;
            serde_json::to_value(value)?
        }
        RuntimeOwnerRequest::ReloadConfig => {
            let Json(value) = api(reload_config(State(state)).await)?;
            serde_json::to_value(value)?
        }
        RuntimeOwnerRequest::CreateThread(body) => {
            let (_, Json(value)) = api(create_thread(State(state), Json(decode(body)?)).await)?;
            serde_json::to_value(value)?
        }
        RuntimeOwnerRequest::ListThreads {
            include_archived,
            limit,
        } => {
            let Json(value) = api(list_threads(
                State(state),
                Query(ThreadsQuery {
                    limit: Some(limit),
                    include_archived: Some(include_archived),
                    archived_only: None,
                }),
            )
            .await)?;
            serde_json::to_value(value)?
        }
        RuntimeOwnerRequest::LoadedThreads => {
            serde_json::to_value(state.runtime_threads.loaded_threads().await?)?
        }
        RuntimeOwnerRequest::RunningThreads => {
            let Json(value) = api(list_running_threads(State(state)).await)?;
            serde_json::to_value(value)?
        }
        RuntimeOwnerRequest::GetThread { thread_id } => {
            let Json(value) = api(get_thread(State(state), Path(thread_id)).await)?;
            serde_json::to_value(value)?
        }
        RuntimeOwnerRequest::UpdateThread { thread_id, body } => {
            let Json(value) =
                api(update_thread(State(state), Path(thread_id), Json(decode(body)?)).await)?;
            serde_json::to_value(value)?
        }
        RuntimeOwnerRequest::GetGoal { thread_id } => {
            let Json(value) = api(get_thread_goal(State(state), Path(thread_id)).await)?;
            serde_json::to_value(value)?
        }
        RuntimeOwnerRequest::ClearGoal { thread_id } => {
            api(delete_thread_goal(State(state), Path(thread_id)).await)?;
            Value::Null
        }
        RuntimeOwnerRequest::HistorySnapshot { thread_id } => {
            let Json(value) =
                api(thread_history::snapshot_thread_history(State(state), Path(thread_id)).await)?;
            serde_json::to_value(value)?
        }
        RuntimeOwnerRequest::HistoryImport(body) => {
            let Json(value) =
                api(thread_history::import_thread_history(State(state), Json(body)).await)?;
            serde_json::to_value(value)?
        }
        RuntimeOwnerRequest::HistoryMutate(body) => {
            let Json(value) =
                api(thread_history::mutate_thread_history(State(state), Json(body)).await)?;
            serde_json::to_value(value)?
        }
        RuntimeOwnerRequest::HistoryLookup(body) => {
            let Json(value) =
                api(
                    thread_history::lookup_thread_history_operation(State(state), Json(body)).await,
                )?;
            serde_json::to_value(value)?
        }
        RuntimeOwnerRequest::HistoryRecover(body) => {
            let Json(value) = api(thread_history::recover_thread_history_operation(
                State(state),
                Json(body),
            )
            .await)?;
            serde_json::to_value(value)?
        }
        RuntimeOwnerRequest::StartTurn { thread_id, body } => {
            let (_, Json(value)) =
                api(start_thread_turn(State(state), Path(thread_id), Json(decode(body)?)).await)?;
            serde_json::to_value(value)?
        }
        RuntimeOwnerRequest::DecideApproval {
            approval_id,
            binding,
            decision,
        } => {
            let value = api(settle_approval_request(
                &state,
                approval_id,
                binding.as_ref(),
                decision,
                None,
            )
            .await)?;
            serde_json::to_value(value)?
        }
        RuntimeOwnerRequest::SubmitUserInput {
            thread_id,
            input_id,
            turn_id,
            response,
        } => {
            let delivered = api(state
                .runtime_threads
                .submit_user_input_for_turn(&thread_id, &input_id, turn_id.as_deref(), response)
                .await
                .map_err(map_thread_err))?;
            api(user_input_receipt(input_id, delivered))?
        }
        RuntimeOwnerRequest::CancelUserInput {
            thread_id,
            input_id,
            turn_id,
        } => {
            let delivered = api(state
                .runtime_threads
                .cancel_user_input_for_turn(&thread_id, &input_id, turn_id.as_deref())
                .await
                .map_err(map_thread_err))?;
            api(user_input_receipt(input_id, delivered))?
        }
        RuntimeOwnerRequest::DeliverDynamicToolResult {
            thread_id,
            turn_id,
            call_id,
            result,
        } => {
            api(deliver_dynamic_tool_result(
                State(state),
                Path((thread_id, turn_id, call_id)),
                Json(result),
            )
            .await)?;
            json!({ "ok": true, "delivered": true })
        }
        RuntimeOwnerRequest::InterruptTurn { thread_id, turn_id } => {
            let Json(value) =
                api(interrupt_thread_turn(State(state), Path((thread_id, turn_id))).await)?;
            serde_json::to_value(value)?
        }
    })
}

pub(super) async fn events(
    state: RuntimeApiState,
    thread_id: String,
    since_seq: u64,
) -> Result<crate::RuntimeOwnerEventStream> {
    validate(&state).await?;
    api(open_thread_event_frames(
        state,
        thread_id,
        ThreadEventsQuery {
            since_seq: Some(since_seq),
            replay_limit: None,
            progress: false,
        },
    )
    .await)
}
