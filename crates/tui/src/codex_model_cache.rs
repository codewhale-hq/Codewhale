use chrono::Utc;
use codewhale_config::ProviderKind;
use codewhale_runtime::codex_model_roster::{
    CatalogSnapshot, CodexModelCacheFreshness, CodexModelMetadata, CodexModelRoster,
    MAX_MODEL_CACHE_BYTES, cached_roster_for, invalidate_roster_cache, load_snapshot,
    snapshot_path,
};
use codewhale_runtime::config::Config;

#[must_use]
pub(crate) fn model_roster_for(config: &Config) -> CodexModelRoster {
    let mut roster = cached_roster_for(config);
    enrich_reasoning_from_catalog(&mut roster);
    roster
}

fn enrich_reasoning_from_catalog(roster: &mut CodexModelRoster) {
    for model in &mut roster.models {
        if model.reasoning.is_some() && !model.efforts.is_empty() {
            continue;
        }
        let Some(offering) =
            crate::provider_lake::catalog_offering_for_model(ProviderKind::Openai, &model.id)
        else {
            continue;
        };
        if model.reasoning.is_none() {
            model.reasoning = offering.reasoning;
        }
        if model.efforts.is_empty()
            && let Some(efforts) =
                crate::reasoning_preference::ReasoningEffort::catalog_effort_values_for_offering(
                    &offering,
                )
        {
            model.efforts = efforts
                .into_iter()
                .map(|effort| effort.as_setting().to_string())
                .collect();
        }
    }
}

/// Fetch through the existing provider client. A registration change during
/// the request cannot publish an old account's models under a new account.
pub(crate) async fn update_from_chatgpt(config: &Config) -> Result<CodexModelRoster, &'static str> {
    let config = config.clone();
    let prepared = config.clone();
    #[cfg(test)]
    let ticket = crate::test_support::env_scope_ticket();
    let (path, client) = tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(ticket);
        let path = snapshot_path(&prepared).ok_or("chatgpt_plan_permission_required")?;
        let client = crate::client::CodewhaleClient::for_catalog_refresh(&prepared)
            .map_err(|_| "chatgpt_plan_credentials_unavailable")?;
        Ok::<_, &'static str>((path, client))
    })
    .await
    .map_err(|_| "chatgpt_plan_credentials_unavailable")??;
    let available = tokio::time::timeout(std::time::Duration::from_secs(20), client.list_models())
        .await
        .map_err(|_| "chatgpt_models_timeout")?
        .map_err(|_| "chatgpt_models_unavailable")?;
    // Catalog ordering and labels are provider facts. The basic official
    // listing does not establish context limits or reasoning effort tiers.
    if available.iter().any(|model| {
        !codewhale_runtime::catalog_cache::valid_catalog_model_id(&model.id)
            || model
                .display_name
                .as_ref()
                .is_some_and(|name| name.len() > 512 || name.chars().any(char::is_control))
    }) {
        return Err("chatgpt_models_invalid_response");
    }
    let models = available
        .into_iter()
        .map(|model| CodexModelMetadata {
            id: model.id,
            display_name: model.display_name,
            context_window: None,
            reasoning: None,
            efforts: Vec::new(),
        })
        .collect();
    let snapshot = CatalogSnapshot {
        fetched_at: Utc::now(),
        models,
    };
    #[cfg(test)]
    let ticket = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(ticket);
        if snapshot_path(&config).as_ref() != Some(&path) {
            return Err("refresh_credentials_changed");
        }
        let encoded = serde_json::to_vec(&snapshot).map_err(|_| "cache_write_failed")?;
        if encoded.len() as u64 > MAX_MODEL_CACHE_BYTES {
            return Err("chatgpt_models_response_too_large");
        }
        codewhale_config::persistence::atomic_write(&path, &encoded)
            .map_err(|_| "cache_write_failed")?;
        invalidate_roster_cache();
        Ok(load_snapshot(&path, Utc::now()))
    })
    .await
    .map_err(|_| "cache_write_failed")?
}

pub(crate) fn spawn_refresh_if_stale(config: &Config) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let config = config.clone();
    handle.spawn(async move {
        let probe = config.clone();
        let Ok(freshness) =
            tokio::task::spawn_blocking(move || model_roster_for(&probe).freshness).await
        else {
            return;
        };
        if freshness == CodexModelCacheFreshness::Fresh {
            return;
        }
        match update_from_chatgpt(&config).await {
            Ok(roster) => tracing::debug!(
                target: "codex_model_cache",
                model_count = roster.models.len(),
                "ChatGPT plan roster refreshed"
            ),
            Err(err) => tracing::debug!(
                target: "codex_model_cache",
                error = %err,
                "ChatGPT plan roster refresh skipped"
            ),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_runtime::codex_model_roster::install_test_chatgpt_roster;

    #[test]
    fn own_roster_follows_selected_registration_and_disappears_after_sign_out() {
        let _env = crate::test_support::lock_test_env();
        let directory = tempfile::tempdir().unwrap();
        let directory_path = directory.path().canonicalize().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &directory_path);
        let mut account_a = Config::default();
        codewhale_runtime::oauth::install_test_chatgpt_registration_for(
            &mut account_a,
            "account-a",
            "oaiapp_workspace_a",
        )
        .unwrap();
        install_test_chatgpt_roster(&account_a, &["z-first", "a-second"]).unwrap();
        let path_a = snapshot_path(&account_a).unwrap();
        assert_eq!(
            model_roster_for(&account_a).model_ids(),
            ["z-first", "a-second"]
        );

        let mut account_b = Config::default();
        codewhale_runtime::oauth::install_test_chatgpt_registration_for(
            &mut account_b,
            "account-b",
            "oaiapp_workspace_a",
        )
        .unwrap();
        assert_ne!(snapshot_path(&account_b).unwrap(), path_a);
        assert_eq!(
            model_roster_for(&account_b).freshness,
            CodexModelCacheFreshness::Missing
        );
        install_test_chatgpt_roster(&account_b, &["b-only"]).unwrap();
        assert_eq!(model_roster_for(&account_b).model_ids(), ["b-only"]);
        assert_eq!(
            model_roster_for(&account_a).model_ids(),
            ["z-first", "a-second"]
        );

        let mut workspace_b = Config::default();
        codewhale_runtime::oauth::install_test_chatgpt_registration_for(
            &mut workspace_b,
            "account-a",
            "oaiapp_workspace_b",
        )
        .unwrap();
        assert!(model_roster_for(&workspace_b).models.is_empty());
        let generation = account_a
            .provider_config_for(&account_a.test_identity_for_kind(ProviderKind::OpenaiCodex))
            .unwrap()
            .oauth_credential_generation
            .as_ref()
            .unwrap()
            .clone();
        let path = codewhale_config::chatgpt_oauth_generation_path(&generation).unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(model_roster_for(&account_a).models.is_empty());
        assert!(path_a.exists());
        assert_eq!(model_roster_for(&account_b).model_ids(), ["b-only"]);
    }
}
