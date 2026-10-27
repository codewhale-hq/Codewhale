use super::*;
use codewhale_protocol::op::ReviewOperation;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartReviewRequest {
    pub review: ReviewOperation,
    pub operation_key: String,
    pub account_model_owner: Option<String>,
    pub model: String,
    pub model_provider: Option<String>,
    pub model_provider_id: Option<String>,
    pub reasoning_effort: Option<String>,
    #[serde(rename = "maxOutputTokens", alias = "max_output_tokens")]
    pub max_output_tokens: std::num::NonZeroU32,
}

pub(super) async fn start_thread_review(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Json(req): Json<StartReviewRequest>,
) -> Result<(StatusCode, Json<StartTurnResponse>), ApiError> {
    let (turn, replayed) = state
        .runtime_threads
        .start_review_turn(&id, req)
        .await
        .map_err(map_thread_err)?;
    let thread = state
        .runtime_threads
        .get_thread(&id)
        .await
        .map_err(map_thread_err)?;
    Ok((
        if replayed {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        },
        Json(StartTurnResponse {
            thread,
            turn,
            idempotent_replay: replayed,
        }),
    ))
}
