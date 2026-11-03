use super::*;
use anyhow::bail;

pub(crate) fn captured(state: &AppState) -> Result<Arc<dyn RuntimeOwnerFrontend>> {
    state
        .owner_frontend
        .clone()
        .context("the captured Runtime native services are unavailable; refusing another owner")
}

pub(crate) async fn execute(
    owner: &Arc<dyn RuntimeOwnerFrontend>,
    request: RuntimeOwnerRequest,
) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(30), owner.execute(request))
        .await
        .context("native owner deadline expired; outcome may be committed, no automatic replay")?
}

pub(crate) async fn runtime_catalog_models(
    owner: &Arc<dyn RuntimeOwnerFrontend>,
    provider: &str,
    provider_id: Option<&str>,
) -> Result<Vec<Value>> {
    let mut models = Vec::new();
    let mut cursor = None;
    let mut cursors = std::collections::HashSet::new();
    let mut ids = std::collections::HashSet::new();
    let mut total = None;
    for _ in 0..40 {
        let catalog = execute(
            owner,
            RuntimeOwnerRequest::ProviderModels {
                provider: provider.to_owned(),
                provider_id: provider_id.map(str::to_owned),
                cursor: cursor.clone(),
            },
        )
        .await?;
        anyhow::ensure!(
            catalog.get("provider").and_then(Value::as_str) == Some(provider)
                && catalog.get("model_provider_id").unwrap_or(&Value::Null) == &json!(provider_id),
            "Runtime model catalog changed provider identity"
        );
        let count = catalog
            .get("total")
            .and_then(Value::as_u64)
            .context("Runtime model catalog omitted its total")?;
        anyhow::ensure!(
            count <= 10_000 && total.is_none_or(|total| total == count),
            "Runtime model catalog size changed or exceeds its limit"
        );
        total = Some(count);
        let entries = catalog
            .get("models")
            .and_then(Value::as_array)
            .context("Runtime model catalog omitted its models")?;
        anyhow::ensure!(
            entries.len() <= 250,
            "Runtime model catalog page exceeds its limit"
        );
        for entry in entries {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .context("Runtime model catalog contains an invalid model")?;
            anyhow::ensure!(
                ids.insert(id.to_string()),
                "Runtime model catalog repeated a model"
            );
            models.push(entry.clone());
        }
        anyhow::ensure!(
            models.len() as u64 <= count,
            "Runtime model catalog exceeds its total"
        );
        match catalog.get("nextCursor") {
            None | Some(Value::Null) => {
                anyhow::ensure!(
                    models.len() as u64 == count,
                    "Runtime model catalog is incomplete"
                );
                return Ok(models);
            }
            Some(Value::String(next)) if !next.is_empty() && next.len() <= 1_368 => {
                anyhow::ensure!(
                    !entries.is_empty() && models.len() < count as usize,
                    "Runtime model catalog continuation is invalid"
                );
                anyhow::ensure!(
                    cursors.insert(next.clone()),
                    "Runtime model catalog cursor repeated"
                );
                cursor = Some(next.clone());
            }
            _ => bail!("Runtime model catalog cursor is invalid"),
        }
    }
    bail!("Runtime model catalog exceeded its page limit")
}

pub(crate) async fn runtime_app_models(state: &AppState) -> Result<Vec<Value>> {
    let owner = captured(state)?;
    let providers = execute(&owner, RuntimeOwnerRequest::Providers).await?;
    let providers = providers
        .get("providers")
        .and_then(Value::as_array)
        .context("Runtime provider catalog is invalid")?;
    anyhow::ensure!(
        providers.len() <= 256,
        "Runtime provider catalog exceeds its limit"
    );
    let aliases = state.registry.list();
    let mut models = Vec::new();
    let mut routes = std::collections::HashSet::new();
    for provider in providers {
        if !provider
            .get("has_model_catalog")
            .and_then(Value::as_bool)
            .context("Runtime provider omitted its catalog availability")?
        {
            continue;
        }
        let kind = provider
            .get("id")
            .and_then(Value::as_str)
            .context("Runtime provider omitted its identity")?;
        let parsed: codewhale_config::ProviderKind =
            serde_json::from_value(json!(kind)).context("Runtime provider kind is unsupported")?;
        anyhow::ensure!(
            kind == parsed.as_str()
                || codewhale_config::descriptors::compatibility_for_id(kind)
                    .is_some_and(|compatibility| compatibility.kind == parsed),
            "Runtime provider kind is not a recognized wire identity"
        );
        let provider_id = match provider.get("model_provider_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) if !id.is_empty() => Some(id.as_str()),
            _ => bail!("Runtime configured provider identity is invalid"),
        };
        if !routes.insert((kind.to_string(), provider_id.map(str::to_string))) {
            continue;
        }
        for mut model in runtime_catalog_models(&owner, kind, provider_id).await? {
            let id = model
                .get("id")
                .and_then(Value::as_str)
                .context("Runtime model identity is invalid")?;
            let compatibility_aliases = aliases
                .iter()
                .find(|entry| {
                    parsed != codewhale_config::ProviderKind::Custom
                        && provider_id.is_none_or(|identity| identity == kind)
                        && entry.provider == parsed
                        && entry.id == id
                })
                .map(|entry| entry.aliases.clone())
                .unwrap_or_default();
            let capabilities: codewhale_config::route::RouteCapabilities =
                match model.pointer("/facts/capabilities") {
                    None | Some(Value::Null) => Default::default(),
                    Some(value) => serde_json::from_value(value.clone())
                        .context("Runtime model capabilities are invalid")?,
                };
            let row = model
                .as_object_mut()
                .context("Runtime model entry is invalid")?;
            row.insert("provider".into(), serde_json::to_value(parsed)?);
            row.insert("model_provider_id".into(), json!(provider_id));
            row.insert("aliases".into(), json!(compatibility_aliases));
            row.insert(
                "supports_tools".into(),
                json!(
                    capabilities.native_tool_calls
                        == codewhale_config::route::CapabilityState::Supported
                ),
            );
            row.insert(
                "supports_reasoning".into(),
                json!(
                    capabilities.reasoning == codewhale_config::route::CapabilityState::Supported
                ),
            );
            row.insert("capabilities".into(), serde_json::to_value(capabilities)?);
            models.push(model);
            anyhow::ensure!(
                models.len() <= 100_000,
                "Runtime model catalog exceeds the response limit"
            );
        }
    }
    Ok(models)
}

pub(crate) async fn require_output_limited_model(
    owner: &Arc<dyn RuntimeOwnerFrontend>,
    requested_model: Option<&str>,
) -> Result<()> {
    let providers = execute(owner, RuntimeOwnerRequest::Providers).await?;
    let current = providers
        .get("current")
        .and_then(Value::as_str)
        .context("Runtime provider is unavailable")?;
    let current_id = match providers.get("current_provider_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) if !id.is_empty() => Some(id.as_str()),
        _ => bail!("Runtime configured provider identity is invalid"),
    };
    let provider = providers
        .get("providers")
        .and_then(Value::as_array)
        .and_then(|providers| {
            providers.iter().find(|provider| {
                provider.get("id").and_then(Value::as_str) == Some(current)
                    && provider.get("model_provider_id").and_then(Value::as_str) == current_id
            })
        })
        .context("Runtime provider is unavailable")?;
    let model = requested_model
        .or_else(|| provider.get("default_model").and_then(Value::as_str))
        .context("maxOutputTokens requires an exact model")?;
    if model.trim().is_empty() || model.eq_ignore_ascii_case("auto") {
        bail!("maxOutputTokens requires an exact model");
    }
    if !current
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("Runtime provider identity is invalid");
    }
    let catalog = runtime_catalog_models(owner, current, current_id).await?;
    let entry = catalog
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(model))
        .context("Output-limit support is unknown for the selected Runtime model")?;
    anyhow::ensure!(
        entry.get("output_token_limit").and_then(Value::as_str) == Some("supported"),
        "The selected Runtime model does not support maxOutputTokens"
    );
    Ok(())
}
