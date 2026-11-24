use super::*;
use codewhale_engine::whale_soul::{
    self, CreateWhaleInput, Soul, UpdateWhaleInput, WhaleError, WhaleRecord,
};

fn map_whale_error(error: WhaleError) -> ApiError {
    match error {
        WhaleError::InvalidId(msg) | WhaleError::BadRequest(msg) => ApiError::bad_request(msg),
        WhaleError::DefaultProtected => ApiError::bad_request(error.to_string()),
        WhaleError::NotFound(msg) => ApiError::not_found(msg),
        WhaleError::PreconditionFailed => ApiError {
            status: StatusCode::PRECONDITION_FAILED,
            code: None,
            message: error.to_string(),
        },
        WhaleError::ReadOnly(msg) | WhaleError::Conflict(msg) => ApiError::conflict(msg),
        WhaleError::Other(err) => ApiError::conflict(err.to_string()),
    }
}

fn parse_if_match(headers: &HeaderMap, required_message: &str) -> Result<String, ApiError> {
    headers
        .get("if-match")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .map(|value| {
            value
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(value)
        })
        .filter(|value| {
            *value == "new"
                || (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        })
        .map(str::to_owned)
        .ok_or_else(|| ApiError::bad_request(required_message))
}

fn parse_optional_if_match(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    if !headers.contains_key("if-match") {
        return Ok(None);
    }
    parse_if_match(headers, "Read the whale and send its revision in If-Match").map(Some)
}

pub(super) async fn get() -> Result<Json<Soul>, ApiError> {
    codewhale_runtime::runtime_control::owner_work(whale_soul::load)
        .await
        .map(Json)
        .map_err(|error| ApiError::conflict(error.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Edit {
    text: String,
}

pub(super) async fn save(
    headers: HeaderMap,
    Json(edit): Json<Edit>,
) -> Result<Json<Soul>, ApiError> {
    let expected = headers
        .get("if-match")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix('"')?.strip_suffix('"'))
        .filter(|value| {
            *value == "new"
                || (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        })
        .ok_or_else(|| ApiError::bad_request("Read SOUL.md and send its revision in If-Match"))?
        .to_owned();
    if edit.text.len() > whale_soul::MAX_BYTES {
        return Err(ApiError::bad_request(
            "SOUL.md exceeds the local whale file limit",
        ));
    }
    codewhale_runtime::runtime_control::owner_work(move || whale_soul::save(&expected, &edit.text))
        .await
        .map_err(|error| ApiError::conflict(error.to_string()))?
        .map(Json)
        .ok_or_else(|| ApiError {
            status: StatusCode::PRECONDITION_FAILED,
            code: None,
            message: "SOUL.md changed since it was opened".into(),
        })
}

pub(super) async fn list() -> Result<Json<Vec<WhaleRecord>>, ApiError> {
    let records = codewhale_runtime::runtime_control::owner_work(|| Ok(whale_soul::list_whales()))
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .map_err(map_whale_error)?;
    Ok(Json(records))
}

pub(super) async fn create(
    Json(input): Json<CreateWhaleInput>,
) -> Result<(StatusCode, Json<WhaleRecord>), ApiError> {
    let created =
        codewhale_runtime::runtime_control::owner_work(move || Ok(whale_soul::create_whale(input)))
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?
            .map_err(map_whale_error)?;
    Ok((StatusCode::CREATED, Json(created)))
}

pub(super) async fn get_one(Path(id): Path<String>) -> Result<Json<WhaleRecord>, ApiError> {
    let record =
        codewhale_runtime::runtime_control::owner_work(move || Ok(whale_soul::get_whale(&id)))
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?
            .map_err(map_whale_error)?;
    Ok(Json(record))
}

pub(super) async fn patch_one(
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<UpdateWhaleInput>,
) -> Result<Json<WhaleRecord>, ApiError> {
    let expected = parse_if_match(&headers, "Read the whale and send its revision in If-Match")?;
    let updated = codewhale_runtime::runtime_control::owner_work(move || {
        Ok(whale_soul::update_whale(&id, &expected, input))
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(map_whale_error)?;
    Ok(Json(updated))
}

pub(super) async fn delete_one(Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    let id_for_delete = id.clone();
    codewhale_runtime::runtime_control::owner_work(move || {
        Ok(whale_soul::delete_whale(&id_for_delete))
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(map_whale_error)?;
    Ok(Json(json!({
        "deleted": true,
        "id": id,
    })))
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct RecycleWhaleRequest {
    #[serde(default)]
    workspace: Option<PathBuf>,
}

#[derive(Serialize)]
pub(super) struct RecycleWhaleResponse {
    whale: WhaleRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_home_thread_id: Option<String>,
    home_thread_id: String,
    thread: ThreadRecord,
}

pub(super) async fn recycle(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<RecycleWhaleResponse>, ApiError> {
    let parsed_body = if body.iter().all(u8::is_ascii_whitespace) {
        RecycleWhaleRequest::default()
    } else {
        serde_json::from_slice::<RecycleWhaleRequest>(&body)
            .map_err(|e| ApiError::bad_request(e.to_string()))?
    };
    let expected = parse_optional_if_match(&headers)?;
    let id_for_check = id.clone();
    let existing = codewhale_runtime::runtime_control::owner_work(move || {
        Ok(whale_soul::get_whale(&id_for_check))
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(map_whale_error)?;
    if let Some(ref exp) = expected
        && existing.revision != *exp
    {
        return Err(map_whale_error(WhaleError::PreconditionFailed));
    }
    if !existing.writable {
        return Err(map_whale_error(WhaleError::ReadOnly(
            "The account owns this imported whale".into(),
        )));
    }

    let workspace = parsed_body
        .workspace
        .unwrap_or_else(|| state.workspace.clone());
    refresh_local_default_catalog(
        &state,
        existing.model.as_deref(),
        None,
        existing.model_provider_id.as_deref(),
    )
    .await;

    let req = CreateThreadRequest {
        workspace: Some(workspace),
        mode: Some("agent".to_string()),
        whale_id: Some(id.clone()),
        ..Default::default()
    };
    let thread = state
        .runtime_threads
        .create_thread_with_shell_policy(
            req,
            state.config_path.as_deref(),
            state.config_profile.as_deref(),
        )
        .await
        .map_err(|e| {
            if let Some(refusal) = external_agent_refusal(&e).or_else(|| mode_refusal(&e)) {
                refusal
            } else {
                ApiError::bad_request(e.to_string())
            }
        })?;

    let thread_id = thread.id.clone();
    let id_for_save = id.clone();
    let (previous_home_thread_id, updated_whale) =
        codewhale_runtime::runtime_control::owner_work(move || {
            Ok(whale_soul::set_home_thread_id(
                &id_for_save,
                expected.as_deref(),
                &thread_id,
            ))
        })
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .map_err(map_whale_error)?;

    Ok(Json(RecycleWhaleResponse {
        whale: updated_whale,
        previous_home_thread_id,
        home_thread_id: thread.id.clone(),
        thread,
    }))
}
