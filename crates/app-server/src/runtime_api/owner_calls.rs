use super::*;
use crate::{RuntimeOwnerFailure, RuntimeOwnerRequest};

async fn validate_workspace_scope(state: &RuntimeApiState) -> Result<()> {
    state
        .workspace_scope
        .validate()
        .await
        .map_err(|_| RuntimeOwnerFailure {
            status: 409,
            message: "selected workspace identity changed".to_owned(),
            code: None,
        })?;
    Ok(())
}

async fn validate(state: &RuntimeApiState) -> Result<()> {
    if state.runtime_threads.is_acp_host() {
        return Err(RuntimeOwnerFailure {
            status: 403,
            message: "immutable ACP-base owner cannot admit ordinary native services".to_owned(),
            code: Some("acp_owner_read_only".to_owned()),
        }
        .into());
    }
    validate_workspace_scope(state).await?;
    if state.shutdown.requested.is_cancelled() {
        return Err(RuntimeOwnerFailure {
            status: 503,
            message: "captured Runtime is stopping".to_owned(),
            code: Some("owner_stopping".to_owned()),
        }
        .into());
    }
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
    match request {
        RuntimeOwnerRequest::Native { workspace, request } => {
            Box::pin(execute_native(state, workspace, request)).await
        }
        request => Box::pin(execute_ordinary(state, request)).await,
    }
}

async fn execute_ordinary(state: RuntimeApiState, request: RuntimeOwnerRequest) -> Result<Value> {
    Ok(match request {
        RuntimeOwnerRequest::Native { .. } => anyhow::bail!("nested native dispatch refused"),
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

pub(super) async fn events_in_workspace(
    state: RuntimeApiState,
    workspace: PathBuf,
    thread_id: String,
    since_seq: u64,
) -> Result<crate::RuntimeOwnerEventStream> {
    validate(&state).await?;
    if !workspace.is_absolute() || workspace != state.workspace {
        return api(Err(ApiError::forbidden(
            "native workspace does not match captured owner",
        )
        .with_code("workspace_mismatch")));
    }
    api(open_thread_event_frames_in_workspace(
        state,
        thread_id,
        ThreadEventsQuery {
            since_seq: Some(since_seq),
            replay_limit: None,
            progress: false,
        },
        Some(workspace),
    )
    .await)
}

async fn native_thread(state: &RuntimeApiState, thread_id: &str) -> Result<()> {
    let (_, selected) = api(workspace::thread_workspace(state, thread_id).await)?;
    if selected != state.workspace {
        return api(Err(ApiError::forbidden(
            "thread is outside the selected workspace",
        )
        .with_code("workspace_mismatch")));
    }
    Ok(())
}

async fn native_response(response: Response) -> Result<Value> {
    let status = response.status();
    let bytes = axum::body::to_bytes(
        response.into_body(),
        codewhale_app_server_protocol::runtime::MAX_RUNTIME_IMAGE_BODY_BYTES,
    )
    .await?;
    let value: Value = serde_json::from_slice(&bytes)?;
    if !status.is_success() {
        return Err(RuntimeOwnerFailure {
            status: status.as_u16(),
            message: value["error"]["message"]
                .as_str()
                .unwrap_or("Runtime operation refused")
                .to_owned(),
            code: value["error"]["code"].as_str().map(str::to_owned),
        }
        .into());
    }
    Ok(value)
}

fn native_headers(key: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "idempotency-key",
        api(HeaderValue::from_str(key)
            .map_err(|_| ApiError::bad_request("operation_key must be visible ASCII")))?,
    );
    api(automation_idempotency_key(&headers))?;
    Ok(headers)
}

async fn native_automation(state: &RuntimeApiState, record: &AutomationRecord) -> Result<()> {
    if record.cwds.len() != 1 || record.cwds[0] != state.workspace {
        return api(Err(ApiError::forbidden(
            "automation is outside the selected workspace",
        )
        .with_code("workspace_mismatch")));
    }
    native_thread(
        state,
        record
            .destination_thread_id
            .as_deref()
            .ok_or_else(|| RuntimeOwnerFailure {
                status: 403,
                message: "automation has no selected destination".to_owned(),
                code: Some("workspace_mismatch".to_owned()),
            })?,
    )
    .await
}

async fn execute_native(
    state: RuntimeApiState,
    selected: PathBuf,
    request: codewhale_app_server_protocol::runtime::NativeRuntimeRequest,
) -> Result<Value> {
    use codewhale_app_server_protocol::runtime::NativeApprovalDecision;
    use codewhale_app_server_protocol::runtime::NativeRuntimeRequest as N;
    use codewhale_engine::runtime_threads::manager::{
        ExternalApprovalBinding, ExternalApprovalDecision,
    };
    if !selected.is_absolute() || selected != state.workspace {
        return api(Err(ApiError::forbidden(
            "native workspace does not match captured owner",
        )
        .with_code("workspace_mismatch")));
    }
    validate_workspace_scope(&state).await?;
    let resume = matches!(&request, N::ResumeAutomation(_));
    let delegated = match request {
        N::Capabilities(_) => {
            return Ok(
                json!({"capabilities": default_runtime_capabilities(), "native_runtime": crate::native_runtime::capabilities()}),
            );
        }
        N::ListThreads(params) => {
            let limit = params.limit.unwrap_or(100);
            if !(1..=100).contains(&limit) {
                return api(Err(ApiError::bad_request("limit must be 1..100")));
            }
            let Json(records) = api(list_threads(
                State(state.clone()),
                Query(ThreadsQuery {
                    limit: None,
                    include_archived: params.include_archived,
                    archived_only: None,
                }),
            )
            .await)?;
            let mut scoped = Vec::new();
            for record in records {
                if record.workspace == state.workspace
                    && native_thread(&state, &record.id).await.is_ok()
                {
                    scoped.push(record);
                    if scoped.len() == limit as usize {
                        break;
                    }
                }
            }
            return Ok(serde_json::to_value(scoped)?);
        }
        N::ReadThread(params) => {
            native_thread(&state, &params.thread_id).await?;
            return Ok(serde_json::to_value(api(state
                .runtime_threads
                .get_thread_detail_in_workspace(&params.thread_id, Some(&selected))
                .await
                .map_err(map_thread_err))?)?);
        }
        N::CreateThread(params) => {
            let mut body = serde_json::to_value(params.body)?;
            body["workspace"] = json!(selected);
            RuntimeOwnerRequest::CreateThread(body)
        }
        N::UpdateThread(params) => {
            native_thread(&state, &params.thread_id).await?;
            let mut body = serde_json::to_value(params.body)?;
            body["expected_workspace"] = json!(selected);
            RuntimeOwnerRequest::UpdateThread {
                thread_id: params.thread_id,
                body,
            }
        }
        N::Providers(_) => RuntimeOwnerRequest::Providers,
        N::ProviderModels(params) => RuntimeOwnerRequest::ProviderModels {
            provider: params.provider,
            provider_id: params.provider_id,
            cursor: params.cursor,
        },
        N::StartTurn(params) => {
            native_thread(&state, &params.thread_id).await?;
            if params
                .body
                .allowed_tools
                .as_ref()
                .is_some_and(|tools| !tools.is_empty())
            {
                return api(Err(ApiError::bad_request(
                    "allowed_tools may only narrow to an empty list",
                )));
            }
            let images = params.body.images.clone().unwrap_or_default();
            let mut body = serde_json::to_value(params.body)?;
            body["images"] = json!(images);
            body["expected_workspace"] = json!(selected);
            RuntimeOwnerRequest::StartTurn {
                thread_id: params.thread_id,
                body,
            }
        }
        N::InterruptTurn(params) => {
            native_thread(&state, &params.thread_id).await?;
            let turn = api(state
                .runtime_threads
                .interrupt_turn_in_workspace(&params.thread_id, &params.turn_id, Some(&selected))
                .await
                .map_err(map_thread_err))?;
            return Ok(serde_json::to_value(turn)?);
        }
        N::DecideApproval(params) => {
            native_thread(&state, &params.thread_id).await?;
            if params.remember.unwrap_or(false) {
                return api(Err(ApiError::bad_request("remember is not supported")));
            }
            let binding = ExternalApprovalBinding {
                thread_id: params.thread_id,
                turn_id: params.turn_id,
                tool_call_id: params.tool_call_id,
            };
            let decision = match params.decision {
                NativeApprovalDecision::Allow => {
                    ExternalApprovalDecision::Allow { remember: false }
                }
                NativeApprovalDecision::Deny => ExternalApprovalDecision::Deny { remember: false },
            };
            return Ok(serde_json::to_value(api(
                settle_approval_request_in_workspace(
                    &state,
                    params.approval_id,
                    Some(&binding),
                    decision,
                    None,
                    Some(&selected),
                )
                .await,
            )?)?);
        }
        N::Subscribe(params) => {
            native_thread(&state, &params.thread_id).await?;
            return Ok(Value::Null);
        }
        N::Unsubscribe(_) => {
            return api(Err(ApiError::bad_request(
                "subscription belongs to its connection",
            )));
        }
        N::AccountStatus(_) => return api(account::status_receipt(state).await),
        N::ListAutomations(_) => {
            let Json(records) = api(list_automations(State(state.clone())).await)?;
            let mut scoped = Vec::new();
            for record in records {
                if native_automation(&state, &record).await.is_ok() {
                    scoped.push(record);
                }
            }
            return Ok(serde_json::to_value(scoped)?);
        }
        N::CreateAutomation(params) => {
            if params.body.auto_approve.unwrap_or(false) || params.body.trust_mode.unwrap_or(false)
            {
                return api(Err(ApiError::bad_request(
                    "automation permissions may not be elevated",
                )));
            }
            native_thread(&state, &params.body.destination_thread_id).await?;
            let headers = native_headers(&params.operation_key)?;
            let mut body = serde_json::to_value(params.body)?;
            body["cwds"] = json!([selected]);
            return native_response(api(create_automation_in_workspace(
                state,
                headers,
                decode(body)?,
                Some(selected.clone()),
            )
            .await)?)
            .await;
        }
        N::PauseAutomation(params) | N::ResumeAutomation(params) => {
            let existing = state
                .automations
                .lock()
                .await
                .get_automation(&params.automation_id);
            if let Ok(record) = existing {
                native_automation(&state, &record).await?;
            }
            let headers = native_headers(&params.operation_key)?;
            let query = AutomationPreconditionQuery {
                expected_revision: Some(params.expected_revision),
            };
            let (action, status) = if resume {
                (
                    "resume",
                    codewhale_engine::automation_manager::AutomationStatus::Active,
                )
            } else {
                (
                    "pause",
                    codewhale_engine::automation_manager::AutomationStatus::Paused,
                )
            };
            return native_response(api(set_automation_status_in_workspace(
                state,
                params.automation_id,
                query,
                headers,
                action,
                status,
                Some(selected),
            )
            .await)?)
            .await;
        }
    };
    execute_ordinary(state, delegated).await
}
