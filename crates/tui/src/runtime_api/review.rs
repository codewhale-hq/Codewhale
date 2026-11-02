use super::*;
use codewhale_engine::runtime_threads::manager::StartReviewRequest;

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
