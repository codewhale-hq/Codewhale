use super::*;
use codewhale_engine::whale_soul::{self, Soul};

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
