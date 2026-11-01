use super::*;
use codewhale_config::user_constitution::ProfileConstitution;

fn model_guidance(document: &ProfileConstitution) -> Result<String, ApiError> {
    Ok(document
        .as_user_constitution()
        .map_err(|error| ApiError::bad_request(error.to_string()))?
        .render_body())
}

pub(super) async fn get_constitution(
    State(state): State<RuntimeApiState>,
) -> Result<Json<Value>, ApiError> {
    let snapshot = codewhale_engine::profile_constitution::load(state.config_profile.as_deref())
        .await
        .map_err(|error| ApiError::conflict(error.to_string()))?;
    match snapshot {
        Some(snapshot) => {
            let rendered = model_guidance(&snapshot.constitution)?;
            Ok(Json(
                json!({"source":"profile", "accountId":snapshot.account_id,
                "revision":snapshot.revision, "constitution":snapshot.constitution,
                "modelGuidance":rendered, "applies":"next_turn"}),
            ))
        }
        None => {
            #[cfg(test)]
            let env_ticket = crate::test_support::env_scope_ticket();
            let rendered = tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                let _membership = crate::test_support::join_env_scope(env_ticket);
                codewhale_engine::prompts::load_active_user_constitution()
                    .map(|document| document.render_body())
            })
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?;
            Ok(Json(json!({"source":"local", "revision":null,
                "modelGuidance":rendered, "applies":"next_turn"})))
        }
    }
}

pub(super) async fn preview_constitution(
    Json(document): Json<ProfileConstitution>,
) -> Result<Json<Value>, ApiError> {
    let rendered = model_guidance(&document)?;
    Ok(Json(json!({"modelGuidance":rendered, "saved":false})))
}
