//! Durable CLI route edits use the same Config owner as Runtime and the TUI.
//! `model` and the legacy `default_text_model` address the saved active route;
//! `default_model` addresses DeepSeek (CN while that route is active). Project root keys retain their scope.

use std::path::Path;

use anyhow::{Context, Result, ensure};

use codewhale_config::ProviderKind;
use codewhale_runtime::config::{BaseUrlEnvReceipt, Config, ProviderIdentity};
use codewhale_runtime::config_persistence as persistence;

/// Whether a config key names a provider or model selection.
pub fn is_route_key(key: &str) -> bool {
    matches!(
        key,
        "provider" | "model" | "default_model" | "default_text_model"
    ) || provider_model_id(key).is_some()
}

fn provider_model_id(key: &str) -> Option<&str> {
    key.strip_prefix("providers.")?
        .strip_suffix(".model")
        .filter(|id| !id.is_empty())
}

fn parse_config(body: &str) -> Result<Config> {
    codewhale_runtime::config::parse_config_base(body)
        .map_err(|_| anyhow::anyhow!("Could not parse route configuration; contents omitted"))
}

fn model_identity(config: &Config, key: &str) -> Result<ProviderIdentity> {
    let identity = if key == "default_model" {
        let china = codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id;
        let selector = if config
            .active_provider_identity()
            .is_ok_and(|identity| identity.key.as_str() == china)
        {
            china
        } else {
            ProviderKind::Deepseek.as_str()
        };
        config.resolve_provider_pin_identity(selector)
    } else if let Some(id) = provider_model_id(key) {
        let custom = config
            .providers
            .as_ref()
            .and_then(|providers| providers.custom_provider_config(id))
            .is_some();
        let selector = if custom {
            id
        } else {
            codewhale_config::descriptors::provider_compatibility()
                .iter()
                .find(|row| row.config_key == id)
                .map_or(id, |row| row.id)
        };
        config.resolve_provider_selection_identity(selector)
    } else {
        config.active_provider_identity()
    };
    identity.map_err(anyhow::Error::msg)
}

fn model_slot(identity: &ProviderIdentity) -> Result<Vec<&str>> {
    Ok(vec!["providers", identity.config_table_key()?, "model"])
}

/// Locate the canonical model leaf in a saved document without applying
/// device preferences or launch overrides. Export uses this same identity
/// resolution to omit only root aliases shadowed by that leaf.
pub fn model_slot_for_document(body: &str, key: &str) -> Result<Vec<String>> {
    ensure!(
        is_route_key(key) && key != "provider",
        "Not a model preference key: {key}"
    );
    let config = parse_config(body)?;
    let identity = model_identity(&config, key)?;
    Ok(model_slot(&identity)?
        .into_iter()
        .map(str::to_string)
        .collect())
}

fn document_slot_value(document: &toml::value::Table, slot: &[String]) -> Option<String> {
    let [root, provider, field] = slot else {
        return None;
    };
    document
        .get(root)?
        .as_table()?
        .get(provider)?
        .as_table()?
        .get(field)?
        .as_str()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
}

fn insert_document_slot_value(
    document: &mut toml::value::Table,
    slot: &[String],
    value: &str,
) -> bool {
    let [root, provider, field] = slot else {
        return false;
    };
    let Some(providers) = document
        .entry(root.clone())
        .or_insert_with(|| toml::Value::Table(toml::value::Table::new()))
        .as_table_mut()
    else {
        return false;
    };
    let Some(entry) = providers
        .entry(provider.clone())
        .or_insert_with(|| toml::Value::Table(toml::value::Table::new()))
        .as_table_mut()
    else {
        return false;
    };
    entry.insert(field.clone(), toml::Value::String(value.to_string()));
    true
}

/// Scrub root model aliases from a serialized config document for export.
///
/// Root `model`/`default_text_model` address the *active* route on import and
/// `default_model` addresses DeepSeek (CN when that route is active), so a raw root alias exported next to
/// the canonical `[providers.<id>].model` leaf fails the importer's replay
/// check. This rewrites `document` in place:
///
/// - A root alias the active route still consumes is shadowed state once the
///   route's canonical leaf is exported; it is dropped.
/// - A root alias the active route ignores but DeepSeek recognizes is
///   DeepSeek's saved fallback; it moves to `providers.deepseek.model` unless
///   that slot already carries a value. Any other unrecognized value is dead
///   state that would only conflict on import and is dropped.
/// - `default_model` folds into the selected DeepSeek region's model slot when
///   nothing live occupies that slot, and is dropped otherwise.
///
/// Only route-selection keys are parsed as `Config` here. Export documents
/// intentionally preserve unknown or differently typed local-authority
/// extras, and reparsing them merely to locate the model slot would fail the
/// whole export.
pub fn scrub_root_model_aliases_for_export(document: &mut toml::value::Table) -> Result<()> {
    let mut scratch = toml::value::Table::new();
    for key in [
        "provider",
        "model",
        "default_text_model",
        "defaultTextModel",
        "base_url",
        "baseUrl",
        "providers",
    ] {
        if let Some(value) = document.get(key) {
            scratch.insert(key.to_string(), value.clone());
        }
    }
    let body = toml::to_string(&toml::Value::Table(scratch))
        .context("serializing route selection for export")?;
    // Slot resolution reuses the exact document identity rules; the extra
    // parse below only exists so the ownership test can scope a Config clone.
    let active_slot = model_slot_for_document(&body, "model")?;
    let deepseek_slot = model_slot_for_document(&body, "default_model")?;
    let config = parse_config(&body)?;
    let identity = model_identity(&config, "model")?;

    if document_slot_value(document, &active_slot).is_some() {
        for root_key in ["model", "default_text_model"] {
            let Some(value) = document
                .get(root_key)
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
            else {
                continue;
            };
            // Same ownership test as `unset`: scope to the active route with
            // its canonical leaf cleared and ask whether this root value is
            // what the route would then resolve. A foreign DeepSeek root
            // ignored by the active vendor remains DeepSeek's fallback.
            let mut scoped = config.clone();
            scoped
                .scope_to_provider_identity(&identity)
                .map_err(anyhow::Error::msg)?;
            scoped.set_provider_model_override(&identity, None)?;
            scoped.legacy_model = None;
            scoped.default_text_model = Some(value.to_string());
            let wire_model = codewhale_runtime::config::wire_model_for_provider_route(
                identity.provider,
                &scoped.active_route_base_url(),
                &value,
            );
            if scoped.default_model() == wire_model {
                document.remove(root_key);
                continue;
            }
            if codewhale_runtime::config::normalize_model_name(&value).is_none() {
                document.remove(root_key);
                continue;
            }
            if document_slot_value(document, &deepseek_slot).is_some() {
                document.remove(root_key);
                continue;
            }
            ensure!(
                insert_document_slot_value(document, &deepseek_slot, &value),
                "Cannot export a root model alias into a non-table provider slot"
            );
            document.remove(root_key);
        }
    }

    match document.get("default_model").and_then(toml::Value::as_str) {
        Some(value) => {
            let value = value.trim().to_string();
            // A root alias the DeepSeek route still consumes lands in this
            // same slot on import; the live choice wins over the dead alias.
            let root_covers_slot = active_slot == deepseek_slot
                && ["model", "default_text_model"]
                    .iter()
                    .any(|key| document.get(*key).and_then(toml::Value::as_str).is_some());
            let folded = !value.is_empty()
                && !root_covers_slot
                && document_slot_value(document, &deepseek_slot).is_none();
            if folded {
                ensure!(
                    insert_document_slot_value(document, &deepseek_slot, &value),
                    "Cannot export default_model into a non-table provider slot"
                );
            }
            document.remove("default_model");
        }
        None => {
            ensure!(
                !document.contains_key("default_model"),
                "Cannot export a non-string default_model without losing its value"
            );
        }
    }
    Ok(())
}

fn project_root_key<'a>(path: &Path, key: &'a str) -> Option<&'a str> {
    (codewhale_config::config_path_is_workspace_scoped(path)
        && matches!(key, "model" | "default_text_model"))
    .then_some(key)
}

fn saved_config(store: &codewhale_config::ConfigStore) -> Result<Config> {
    let rendered;
    let body = if let Some(original) = store.original_body() {
        original
    } else {
        rendered = store.rendered_body()?;
        &rendered
    };
    let mut config = parse_config(body)?;
    if config.route_preferences_version.is_none()
        && codewhale_runtime::config::is_home_config_path(store.path())
    {
        config.apply_saved_selection(
            &codewhale_runtime::settings::Settings::load_legacy_route_preferences_read_only()?,
        );
    }
    Ok(config)
}

/// Read the saved route without applying launch overrides or credentials.
pub fn get(path: &Path, key: &str) -> Result<Option<String>> {
    ensure!(is_route_key(key), "Not a route preference key: {key}");
    let store = codewhale_config::ConfigStore::load(Some(path.to_path_buf()))?;
    if let Some(key) = project_root_key(store.path(), key) {
        return Ok(store.config.get_value(key));
    }
    let mut config = saved_config(&store)?;
    if key == "provider" {
        return Ok(Some(
            config.provider.unwrap_or_else(|| "deepseek".to_string()),
        ));
    }
    let identity = model_identity(&config, key)?;
    config
        .scope_to_provider_identity(&identity)
        .map_err(anyhow::Error::msg)?;
    Ok(config
        .provider_config_for(&identity)
        .and_then(|entry| entry.model.clone())
        .or_else(|| {
            (provider_model_id(key).is_none()
                && (config.default_text_model.is_some() || config.legacy_model.is_some()))
            .then(|| config.default_model())
        }))
}

/// One saved snapshot for CLI route reports, including exact legacy identities.
/// The source distinguishes an explicit model from the provider default.
pub fn selected_route(path: &Path) -> Result<(String, String, codewhale_config::ModelSource)> {
    let store = codewhale_config::ConfigStore::load(Some(path.to_path_buf()))?;
    let config = saved_config(&store)?;
    let identity = config
        .active_provider_identity()
        .map_err(anyhow::Error::msg)?;
    let provider = identity.key.to_string();
    let source = if config
        .provider_config_for(&identity)
        .and_then(|entry| entry.model.as_ref())
        .is_some()
    {
        codewhale_config::ModelSource::ProviderConfig
    } else if config.default_text_model.is_some() || config.legacy_model.is_some() {
        if store.config.default_text_model.is_none() && store.config.model.is_some() {
            codewhale_config::ModelSource::RootModel
        } else {
            codewhale_config::ModelSource::RootDefaultTextModel
        }
    } else {
        codewhale_config::ModelSource::ProviderDefault
    };
    Ok((provider, config.default_model(), source))
}

pub fn list_models(
    store: &codewhale_config::ConfigStore,
    filter: Option<ProviderKind>,
) -> Result<Vec<(ProviderKind, Vec<String>)>> {
    crate::models_dev_live::maybe_load_persisted_cache();
    let mut config = saved_config(store)?;
    config.base_url_env_receipt = config
        .active_provider_identity()
        .map_or(BaseUrlEnvReceipt::NoOwner, |identity| {
            BaseUrlEnvReceipt::Route(identity.provider, identity.key.to_string())
        });
    let providers = filter
        .as_ref()
        .map_or(ProviderKind::all(), std::slice::from_ref);
    let mut catalogs = Vec::with_capacity(providers.len());
    for provider in providers {
        if *provider == ProviderKind::Custom
            && config
                .providers
                .as_ref()
                .and_then(|providers| providers.custom_provider_config(provider.as_str()))
                .is_none()
        {
            continue;
        }
        let identity = config
            .legacy_selection_identity(provider.as_str())
            .map_err(anyhow::Error::msg)?;
        let mut route_config = config.clone();
        route_config
            .scope_to_provider_identity(&identity)
            .map_err(anyhow::Error::msg)?;
        let mut models = crate::provider_lake::configured_catalog_models_for_route(
            &route_config,
            identity.provider,
            identity.key.as_str(),
            &route_config.active_route_base_url(),
        );
        if identity.provider != ProviderKind::OpenaiCodex {
            let default_model = route_config.default_model();
            if !default_model.is_empty() && !default_model.eq_ignore_ascii_case("auto") {
                models.push(default_model);
            }
            models.sort();
            models.dedup();
        }
        catalogs.push((*provider, models));
    }
    Ok(catalogs)
}

pub fn resolve_model(
    path: &Path,
    provider_identity: &str,
    base_url: &str,
    model: Option<&str>,
) -> Result<codewhale_config::route::ReadyRouteCandidate> {
    crate::models_dev_live::maybe_load_persisted_cache();
    let store = codewhale_config::ConfigStore::load(Some(path.to_path_buf()))?;
    let mut config = saved_config(&store)?;
    let identity = config
        .resolve_provider_selection_identity(provider_identity)
        .map_err(anyhow::Error::msg)?;
    config.set_provider_base_url_override(&identity, Some(base_url.into()))?;
    crate::provider_catalog_live::maybe_load_persisted_cache_for_config(&config);
    let route = crate::route_runtime::resolve_runtime_route_for_identity(&config, &identity, model)
        .map_err(anyhow::Error::msg);
    if !route.as_ref().is_ok_and(|route| {
        (route.identity.provider == ProviderKind::OpenaiCodex
            && crate::codex_model_cache::model_roster_for(&route.config)
                .metadata_for(route.candidate.wire_model_id().as_str())
                .is_some())
            || crate::provider_lake::configured_catalog_offering_for_route(
                &route.config,
                route.identity.provider,
                route.identity.key.as_str(),
                &route.candidate.endpoint().base_url,
                route.candidate.wire_model_id().as_str(),
            )
            .is_some()
    }) {
        if crate::provider_catalog_live::cached_entry_for_route(
            identity.provider,
            identity.key.as_str(),
            base_url,
        )?
        .is_some_and(|entry| entry.fetched_at > 0)
        {
            return Err(
                codewhale_agent::ModelResolutionError::ModelNotAvailableForProvider {
                    provider: identity.provider,
                    requested: model.unwrap_or("auto").to_string(),
                }
                .into(),
            );
        }
        codewhale_agent::ModelRegistry::default().resolve(
            model.filter(|model| *model != "auto"),
            Some(identity.provider),
        )?;
    }
    Ok(route?.candidate)
}

/// Save one explicit route preference after atomically adopting legacy choices.
pub fn set(path: &Path, key: &str, value: &str) -> Result<()> {
    persistence::mutate_config_document(path, |doc| set_document(path, doc, key, value))
}

/// Prepare a validated snapshot for a caller's preview and atomic save.
/// Migration changes only this document; this function never writes a file.
pub fn prepare_document(path: &Path, raw: &str) -> Result<toml_edit::DocumentMut> {
    let mut doc = raw
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| anyhow::anyhow!("Could not parse route configuration; contents omitted"))?;
    parse_config(raw)?;
    persistence::migrate_legacy_route_preferences(path, &mut doc)?;
    Ok(doc)
}

/// Edit one route selection in an already-prepared candidate without saving.
/// Callers must prepare migration first and atomically save the final snapshot.
pub fn set_document(
    path: &Path,
    doc: &mut toml_edit::DocumentMut,
    key: &str,
    value: &str,
) -> Result<()> {
    ensure!(is_route_key(key), "Not a route preference key: {key}");
    let value = value.trim();
    ensure!(
        !value.is_empty() && !value.chars().any(char::is_control),
        "Route preference must be nonempty and contain no control characters"
    );
    if let Some(key) = project_root_key(path, key) {
        return persistence::set_document_value(doc, &[key], value);
    }
    let config = parse_config(&doc.to_string())?;
    if key == "provider" {
        let identity = config
            .resolve_provider_selection_identity(value)
            .map_err(anyhow::Error::msg)?;
        persistence::set_document_value(
            doc,
            &["provider"],
            identity.persisted_id().unwrap_or(identity.key.as_str()),
        )?;
        // Same root-alias authority as the Runtime/TUI provider writer: a CLI
        // switch must not leave the incoming route holding the outgoing one's
        // fallback, and must not delete a choice to get there.
        return reconcile_root_model_aliases(doc, &config, &identity);
    }
    let identity = model_identity(&config, key)?;
    set_provider_model_document(doc, &identity, value)
}

/// Clear a canonical selection without allowing archived Settings to restore it.
pub fn unset(path: &Path, key: &str) -> Result<()> {
    ensure!(is_route_key(key), "Not a route preference key: {key}");
    persistence::mutate_config_document(path, |doc| {
        if key == "provider" || project_root_key(path, key).is_some() {
            persistence::unset_document_value(doc, &[key])?;
            return Ok(());
        }
        let config = parse_config(&doc.to_string())?;
        let identity = model_identity(&config, key)?;
        persistence::unset_document_value(doc, &model_slot(&identity)?)?;
        // Clear relevant legacy fallbacks as well, or deleting the canonical
        // leaf would restore an older choice on reload. Root fields belong to
        // the active route, with DeepSeek's historical default as an exception.
        if matches!(identity.provider, ProviderKind::Deepseek)
            || config
                .active_provider_identity()
                .is_ok_and(|active| active == identity)
        {
            let mut scoped = config.clone();
            scoped
                .scope_to_provider_identity(&identity)
                .map_err(anyhow::Error::msg)?;
            scoped.set_provider_model_override(&identity, None)?;
            scoped.legacy_model = None;
            for root_key in ["default_text_model", "model"] {
                let Some(model) = doc.get(root_key).and_then(toml_edit::Item::as_str) else {
                    continue;
                };
                scoped.default_text_model = Some(model.to_string());
                let wire_model = codewhale_runtime::config::wire_model_for_provider_route(
                    identity.provider,
                    &scoped.active_route_base_url(),
                    model,
                );
                // Reuse Config's root-model guards: a foreign DeepSeek root
                // ignored by the active vendor remains that provider's fallback.
                if scoped.default_model() == wire_model {
                    persistence::unset_document_value(doc, &[root_key])?;
                }
            }
        }
        Ok(())
    })
}

pub fn set_provider_model_document(
    doc: &mut toml_edit::DocumentMut,
    identity: &ProviderIdentity,
    model: &str,
) -> anyhow::Result<()> {
    let mut config =
        codewhale_runtime::config::parse_config_base(&doc.to_string()).map_err(|_| {
            anyhow::anyhow!("Could not parse destination route identity; contents omitted")
        })?;
    codewhale_plugins::providers::apply_startup_providers(&mut config)?;
    persistence::set_verified_provider_model_document(doc, &config, identity, model)
}

/// One persistent owner and atomic write for an explicitly saved route.
///
/// Also returns the undo for exactly this write, taken under the config lock,
/// so a caller whose follow-up apply step is rejected can take back only the
/// switch.
pub fn persist_provider_selection(
    config_path: Option<&Path>,
    identity: &ProviderIdentity,
    model: Option<&str>,
) -> anyhow::Result<(std::path::PathBuf, codewhale_config::ConfigDocumentUndo)> {
    let path = persistence::config_toml_path(config_path)?;
    let ((), undo) =
        codewhale_config::mutate_config_document_undoable_with_migration(&path, |doc, moved| {
            persistence::migrate_legacy_route_preferences(&path, doc)?;
            let mut config = codewhale_runtime::config::parse_config_after_locked_migration(
                &doc.to_string(),
                moved,
            )
            .map_err(|_| anyhow::anyhow!("Could not parse destination route; contents omitted"))?;
            codewhale_plugins::providers::apply_startup_providers(&mut config)?;
            config
                .verify_provider_identity(identity)
                .map_err(anyhow::Error::msg)?;
            if let Some(model) = model {
                persistence::set_verified_provider_model_document(doc, &config, identity, model)?;
            }
            persistence::set_document_value(
                doc,
                &["provider"],
                identity.persisted_id().unwrap_or(identity.key.as_str()),
            )?;
            reconcile_root_model_aliases(doc, &config, identity)
        })?;
    Ok((path, undo))
}

/// Keep a root `default_text_model` alias from stranding a route switch,
/// without discarding the choice it holds.
///
/// The root alias is the *active* route's fallback: `Config` resolves it only
/// when the selected route has no model of its own. A switch that leaves the
/// incoming route without a leaf therefore hands it an alias naming the route
/// on its way out, and `Config::load` rejects the file the caller just wrote —
/// a committed switch that produces an unloadable config — or, on a
/// pass-through incoming route that loads it, a model the incoming route never
/// chose while the outgoing route comes back on its catalog default.
///
/// When the outgoing route has no leaf and was resolving the alias, relocate
/// that value onto the outgoing route's own canonical leaf, because it is real
/// saved state, and only then clear the alias. Any other alias the incoming
/// route already shadows with its own leaf is inert and stays: deleting a
/// saved choice to satisfy validation of a value nothing resolves is data
/// loss, not a repair. Likewise an unnamed custom route stores its
/// model in the alias itself and has no leaf to receive it. If the incoming
/// route cannot shadow that value, refuse the switch atomically and ask for an
/// explicit destination model instead of saving an unloadable configuration.
///
/// `previous` is the document's configuration before the switch; `incoming` is
/// the identity now selected. Every route writer calls this, so no writer can
/// keep a private rule about which route owns the root alias.
pub fn reconcile_root_model_aliases(
    doc: &mut toml_edit::DocumentMut,
    previous: &codewhale_runtime::config::Config,
    incoming: &codewhale_runtime::config::ProviderIdentity,
) -> anyhow::Result<()> {
    // A freshly admitted id-less root keeps its legacy alias as a receipt.
    // Its effective model is written to the canonical leaf created by this
    // same-lock migration; the alias is never another route's saved choice.
    if incoming.provider == ProviderKind::Custom && incoming.persisted_id().is_none() {
        return Ok(());
    }
    // The outgoing route was resolving the alias as its own model. Move it
    // onto that route's leaf even when the incoming route could load it: a
    // pass-through incoming route would otherwise inherit a model it never
    // chose, and the outgoing route would come back on its catalog default.
    if let Some((outgoing, value)) = previous.root_model_alias_owned_by_outgoing(incoming) {
        persistence::set_verified_provider_model_document(doc, previous, &outgoing, &value)?;
        unset_root_model_aliases(doc)?;
        return Ok(());
    }
    // Below, only `default_text_model` is read. The legacy root `model` key is
    // never what blocks a load: `Config::default_model` already refuses to
    // route a foreign legacy value to a provider that cannot serve it, and
    // `Config::validate` does not consult it, so relocating it would move a
    // value nothing is asking about. It is still cleared together with the
    // alias that shadows it (`unset_root_model_aliases`).
    const ROOT_KEY: &str = "default_text_model";
    let Some(value) = doc
        .get(ROOT_KEY)
        .and_then(toml_edit::Item::as_str)
        .map(str::to_owned)
    else {
        return Ok(());
    };
    let mut switched = codewhale_runtime::config::parse_config_base(&doc.to_string())
        .map_err(|_| anyhow::anyhow!("Could not parse switched route; contents omitted"))?;
    codewhale_plugins::providers::apply_startup_providers(&mut switched)?;
    // `Config::validate` is the single authority on what the incoming route can
    // serve, so a writer cannot disagree with the loader. Act only when this
    // alias is what the loader rejects: a document already broken for an
    // unrelated reason is not this writer's to rewrite.
    let mut without_alias = switched.clone();
    without_alias.default_text_model = None;
    if switched
        .provider_config_for(incoming)
        .and_then(|entry| entry.model.as_deref())
        .is_some()
        || switched.validate().is_ok()
        || without_alias.validate().is_err()
    {
        return Ok(());
    }
    // An empty or control-bearing alias names no saved model. Nothing to
    // relocate, and clearing it loses nothing.
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        unset_root_model_aliases(doc)?;
        return Ok(());
    }
    // No leaf can hold this value. Keep the only copy of the user's choice
    // rather than discard it for a document the incoming route loads as soon as
    // it saves a model of its own.
    let outgoing = previous.active_provider_identity().ok();
    if outgoing.as_ref().is_some_and(|outgoing| {
        outgoing != incoming
            && outgoing.provider == ProviderKind::Custom
            && outgoing.persisted_id().is_none()
    }) {
        // This same-lock canonicalizer copied the root model before moving
        // its endpoint. Clear an alias only when that exact outgoing leaf
        // proves the choice survives; otherwise retain the refusal.
        if outgoing.as_ref().is_some_and(|outgoing| {
            previous
                .provider_config_for(outgoing)
                .and_then(|entry| entry.model.as_deref())
                == Some(value.as_str())
        }) {
            return unset_root_model_aliases(doc);
        }
        anyhow::bail!(
            "Choose a model for the destination provider before switching from a legacy custom connection; the saved configuration was not changed"
        );
    }
    let Some(outgoing) = outgoing.as_ref().filter(|outgoing| *outgoing != incoming) else {
        return Ok(());
    };
    let mut scoped = switched;
    scoped
        .scope_to_provider_identity(outgoing)
        .map_err(anyhow::Error::msg)?;
    if scoped
        .provider_config_for(outgoing)
        .and_then(|entry| entry.model.as_deref())
        .is_none()
    {
        persistence::set_verified_provider_model_document(doc, previous, outgoing, &value)?;
    }
    // The outgoing route either already saved its own choice or has just
    // received this one, so the alias is now a shadowed duplicate that only
    // blocks the incoming route.
    unset_root_model_aliases(doc)?;
    Ok(())
}

/// Clear the root model alias: `default_text_model` and the legacy root
/// `model` that `Config::default_model` falls back to when the former is
/// unset. While `default_text_model` is present the legacy key resolves on no
/// route, so removing only the alias would resurrect a stale legacy model on
/// the incoming route (a pass-through route would then use a model nobody
/// chose for it). When the legacy key *is* the alias, the caller has already
/// moved its value onto the outgoing route's leaf.
fn unset_root_model_aliases(doc: &mut toml_edit::DocumentMut) -> anyhow::Result<()> {
    persistence::unset_document_value(doc, &["default_text_model"])?;
    persistence::unset_document_value(doc, &["model"])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{EnvVarGuard, lock_test_env};

    fn document(path: &Path) -> toml::Value {
        toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn route_edits_adopt_legacy_selection_once_and_preserve_settings() -> Result<()> {
        let _env = lock_test_env();
        let home = tempfile::tempdir()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _path = EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
        let _legacy_path = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
        let path = home.path().join("config.toml");
        std::fs::write(
            &path,
            "provider = \"deepseek\"\ndefault_text_model = \"deepseek-v4-pro\"\n[providers.zai]\nmodel = \"GLM-5.2\"\n",
        )?;
        let settings_path = home.path().join("settings.toml");
        let settings = "default_provider = \"zai\"\n[provider_models]\nzai = \"GLM-5.3\"\n";
        std::fs::write(&settings_path, settings)?;
        let before = std::fs::read(&path)?;
        assert_eq!(get(&path, "provider")?.as_deref(), Some("zai"));
        assert_eq!(get(&path, "model")?.as_deref(), Some("GLM-5.3"));
        assert_eq!(std::fs::read(&path)?, before);

        let mut candidate = prepare_document(&path, &std::fs::read_to_string(&path)?)?;
        set_document(&path, &mut candidate, "model", "GLM-5.2")?;
        assert_eq!(std::fs::read(&path)?, before);
        set(&path, "model", "GLM-5.2")?;
        assert_eq!(
            document(&path),
            toml::from_str::<toml::Value>(&candidate.to_string())?
        );
        assert_eq!(
            document(&path)["route_preferences_version"].as_integer(),
            Some(1)
        );
        assert_eq!(
            get(&path, "default_text_model")?.as_deref(),
            Some("GLM-5.2")
        );
        assert_eq!(
            get(&path, "providers.zai.model")?.as_deref(),
            Some("GLM-5.2")
        );
        set(&path, "default_model", "deepseek-v4-flash")?;
        assert_eq!(
            get(&path, "default_model")?.as_deref(),
            Some("deepseek-v4-flash")
        );
        set(&path, "provider", "deepseek")?;
        assert_eq!(get(&path, "model")?.as_deref(), Some("deepseek-v4-flash"));
        unset(&path, "providers.deepseek.model")?;
        assert!(get(&path, "default_model")?.is_none());
        unset(&path, "providers.zai.model")?;
        assert!(get(&path, "providers.zai.model")?.is_none());
        unset(&path, "provider")?;
        assert_eq!(get(&path, "provider")?.as_deref(), Some("deepseek"));
        assert_eq!(std::fs::read_to_string(settings_path)?, settings);
        // An unrelated typed store write must preserve the migration receipt.
        let mut store = codewhale_config::ConfigStore::load(Some(path.clone()))?;
        store.config.set_value("verbosity", "concise")?;
        store.save()?;
        assert_eq!(
            document(&path)["route_preferences_version"].as_integer(),
            Some(1)
        );
        Ok(())
    }

    #[test]
    fn route_edits_keep_exact_named_provider_keys_and_reject_unknown_routes() -> Result<()> {
        let _env = lock_test_env();
        let home = tempfile::tempdir()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _path = EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
        let _legacy_path = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
        let path = home.path().join("config.toml");
        std::fs::write(
            &path,
            r#"provider = "Team.A"
[providers."Team.A"]
kind = "openai-compatible"
base_url = "http://127.0.0.1:9/v1"
model = "Model-X"
[providers."team.a"]
kind = "openai-compatible"
base_url = "http://127.0.0.1:10/v1"
model = "Other-X"
"#,
        )?;
        set(&path, "providers.Team.A.model", "Model-Y")?;
        assert_eq!(get(&path, "model")?.as_deref(), Some("Model-Y"));
        assert_eq!(
            get(&path, "providers.team.a.model")?.as_deref(),
            Some("Other-X")
        );
        let before = std::fs::read(&path)?;
        assert!(set(&path, "providers.TEAM.A.model", "Model-Z").is_err());
        assert!(set(&path, "provider", "unconfigured-route").is_err());
        assert_eq!(std::fs::read(&path)?, before);
        unset(&path, "providers.Team.A.model")?;
        assert!(get(&path, "providers.Team.A.model")?.is_none());
        assert_eq!(
            document(&path)["providers"]["team.a"]["model"].as_str(),
            Some("Other-X")
        );
        Ok(())
    }

    #[test]
    fn cli_provider_edits_share_the_runtime_writer_root_alias_authority() -> Result<()> {
        let _env = lock_test_env();
        let home = tempfile::tempdir()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _path = EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
        let _legacy_path = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
        let _cloud = EnvVarGuard::set("CODEWHALE_DISABLE_CLOUD_FACTS", "1");
        let _overrides: Vec<_> = [
            "CODEWHALE_MODEL",
            "DEEPSEEK_MODEL",
            "DEEPSEEK_DEFAULT_TEXT_MODEL",
            "CODEWHALE_PROVIDER",
            "DEEPSEEK_PROVIDER",
            "CODEWHALE_BASE_URL",
            "DEEPSEEK_BASE_URL",
            "CODEWHALE_PROFILE",
            "DEEPSEEK_PROFILE",
            "ZAI_MODEL",
            "ZAI_BASE_URL",
        ]
        .into_iter()
        .map(EnvVarGuard::remove)
        .collect();
        let path = home.path().join("config.toml");

        // The outgoing route was resolving the root fallback as its own
        // model, so a CLI switch moves it onto that route's leaf (never
        // deletes it), and switching back must still find it.
        std::fs::write(
            &path,
            "route_preferences_version = 1\nprovider = \"zai\"\ndefault_text_model = \"GLM-4.6\"\n[providers.deepseek]\nmodel = \"deepseek-v4-pro\"\n",
        )?;
        set(&path, "provider", "deepseek")?;
        let doc = document(&path);
        assert!(doc.get("default_text_model").is_none());
        assert_eq!(doc["providers"]["zai"]["model"].as_str(), Some("GLM-4.6"));
        let switched = codewhale_plugins::providers::load_config(Some(path.clone()), None)
            .expect("a CLI provider switch must remain loadable");
        assert_eq!(
            switched.active_provider_identity().unwrap().provider,
            ProviderKind::Deepseek
        );
        assert_eq!(switched.default_model(), "deepseek-v4-pro");
        set(&path, "provider", "zai")?;
        assert_eq!(
            codewhale_plugins::providers::load_config(Some(path.clone()), None)
                .expect("switching back must remain loadable")
                .default_model(),
            "GLM-4.6"
        );

        // With no leaf on the incoming route the alias is what `Config::load`
        // rejects. Move it to the route that owns it rather than drop it.
        std::fs::write(
            &path,
            "route_preferences_version = 1\nprovider = \"volcengine\"\ndefault_text_model = \"ark-private-id\"\n",
        )?;
        set(&path, "provider", "deepseek")?;
        let doc = document(&path);
        assert!(doc.get("default_text_model").is_none());
        assert_eq!(
            doc["providers"]["volcengine"]["model"].as_str(),
            Some("ark-private-id")
        );
        codewhale_plugins::providers::load_config(Some(path.clone()), None)
            .expect("a CLI switch must not commit an unloadable config");
        set(&path, "provider", "volcengine")?;
        assert_eq!(
            codewhale_plugins::providers::load_config(Some(path), None)
                .expect("switching back must remain loadable")
                .default_model(),
            "ark-private-id"
        );
        Ok(())
    }

    #[test]
    fn project_model_edits_keep_root_fields_and_skip_device_migration() -> Result<()> {
        let _env = lock_test_env();
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        std::fs::create_dir_all(&home)?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", &home);
        let _path = EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
        let _legacy_path = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
        std::fs::write(home.join("settings.toml"), "default_provider = \"zai\"\n")?;
        let project = root.path().join("project/.codewhale");
        std::fs::create_dir_all(&project)?;
        // An absolute config outside the process workspace is project-scoped
        // only when its parent is a checkout, not merely named `.codewhale`.
        std::fs::create_dir(root.path().join("project/.git"))?;
        let path = project.join("config.toml");
        std::fs::write(&path, "model = \"project-old\"\n")?;
        set(&path, "model", "project-new")?;
        assert_eq!(get(&path, "model")?.as_deref(), Some("project-new"));
        assert!(document(&path).get("providers").is_none());
        assert!(document(&path).get("route_preferences_version").is_none());
        unset(&path, "model")?;
        assert!(document(&path).get("model").is_none());
        Ok(())
    }

    #[test]
    fn saved_routes_preserve_regional_and_legacy_table_identity() -> Result<()> {
        let _env = lock_test_env();
        let home = tempfile::tempdir()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _path = EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
        let _legacy_path = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
        let _cloud = EnvVarGuard::set("CODEWHALE_DISABLE_CLOUD_FACTS", "1");
        let _overrides: Vec<_> = [
            "CODEWHALE_MODEL",
            "DEEPSEEK_MODEL",
            "DEEPSEEK_DEFAULT_TEXT_MODEL",
            "CODEWHALE_PROVIDER",
            "DEEPSEEK_PROVIDER",
            "CODEWHALE_BASE_URL",
            "DEEPSEEK_BASE_URL",
            "CODEWHALE_PROFILE",
            "DEEPSEEK_PROFILE",
            "ZAI_MODEL",
            "ZAI_BASE_URL",
            "OLLAMA_MODEL",
            "OLLAMA_CLOUD_MODEL",
            "OLLAMA_BASE_URL",
            "OLLAMA_CLOUD_BASE_URL",
        ]
        .into_iter()
        .map(EnvVarGuard::remove)
        .collect();
        let path = home.path().join("config.toml");
        for (provider, table, endpoint, model) in [
            (
                "deepseek-cn",
                "deepseek_cn",
                "https://api.deepseek.cn",
                "deepseek-v4-flash",
            ),
            (
                "ollama",
                "ollama",
                "https://ollama.com/v1",
                "saved-cloud-model",
            ),
        ] {
            std::fs::write(
                &path,
                format!(
                    "route_preferences_version = 1\nprovider = '{provider}'\n[providers.{table}]\nbase_url = '{endpoint}'\n"
                ),
            )?;
            set(&path, "model", model)?;
            assert_eq!(document(&path)["provider"].as_str(), Some(provider));
            assert_eq!(
                document(&path)["providers"][table]["model"].as_str(),
                Some(model)
            );
            assert_eq!(get(&path, "model")?.as_deref(), Some(model));
            assert_eq!(
                selected_route(&path)?,
                (
                    if provider == "ollama" {
                        "ollama-cloud"
                    } else {
                        provider
                    }
                    .to_string(),
                    model.to_string(),
                    codewhale_config::ModelSource::ProviderConfig
                )
            );
            unset(&path, "model")?;
            assert!(document(&path)["providers"][table].get("model").is_none());
            let leaf = format!("providers.{table}.model");
            set(&path, &leaf, model)?;
            assert_eq!(get(&path, &leaf)?.as_deref(), Some(model));
            assert_eq!(
                codewhale_plugins::providers::load_config(Some(path.clone()), None)?
                    .default_model(),
                model
            );
            assert_eq!(document(&path)["provider"].as_str(), Some(provider));
            unset(&path, &leaf)?;
            assert!(get(&path, &leaf)?.is_none());
            assert!(document(&path)["providers"][table].get("model").is_none());
        }

        std::fs::write(
            &path,
            "route_preferences_version = 1\nprovider = 'zai'\nmodel = 'GLM-5.3'\n",
        )?;
        assert_eq!(get(&path, "model")?.as_deref(), Some("GLM-5.3"));
        assert_eq!(
            codewhale_plugins::providers::load_config(Some(path.clone()), None)?.default_model(),
            "GLM-5.3"
        );
        assert_eq!(
            selected_route(&path)?.2,
            codewhale_config::ModelSource::RootModel
        );
        // A foreign active-route root must never become the DeepSeek default.
        assert_eq!(
            get(&path, "default_model")?.as_deref(),
            Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL)
        );
        unset(&path, "providers.zai.model")?;
        assert!(document(&path).get("model").is_none());
        assert_eq!(
            codewhale_plugins::providers::load_config(Some(path.clone()), None)?.default_model(),
            selected_route(&path)?.1
        );

        for (provider, root, leaf) in [
            (
                "deepseek",
                "default_text_model = 'deepseek-v4-flash'\nmodel = 'deepseek-v4-flash-vision-exp'",
                "deepseek-v4-pro",
            ),
            (
                "zai",
                "default_text_model = 'GLM-5.1'\nmodel = 'GLM-5.2'",
                "GLM-5.3",
            ),
        ] {
            std::fs::write(
                &path,
                format!(
                    "route_preferences_version = 1\nprovider = '{provider}'\n{root}\n[providers.{provider}]\nmodel = '{leaf}'\n"
                ),
            )?;
            assert_eq!(
                codewhale_plugins::providers::load_config(Some(path.clone()), None)?
                    .default_model(),
                leaf
            );
            unset(&path, &format!("providers.{provider}.model"))?;
            let doc = document(&path);
            assert!(doc.get("model").is_none());
            assert!(doc.get("default_text_model").is_none());
            assert!(doc["providers"][provider].get("model").is_none());
            let loaded = codewhale_plugins::providers::load_config(Some(path.clone()), None)?;
            assert_eq!(loaded.default_model(), selected_route(&path)?.1);
            assert!(loaded.legacy_model.is_none());
        }

        // Clearing Z.ai cannot erase the independent DeepSeek root fallback.
        std::fs::write(
            &path,
            "route_preferences_version = 1\nprovider = 'zai'\ndefault_text_model = 'deepseek-v4-flash'\nmodel = 'GLM-5.1'\n[providers.zai]\nmodel = 'GLM-5.2'\n",
        )?;
        unset(&path, "providers.zai.model")?;
        assert_eq!(
            document(&path)["default_text_model"].as_str(),
            Some("deepseek-v4-flash")
        );
        assert!(document(&path).get("model").is_none());
        let loaded = codewhale_plugins::providers::load_config(Some(path.clone()), None)?;
        assert_ne!(loaded.default_model(), "GLM-5.1");
        assert_eq!(loaded.default_model(), selected_route(&path)?.1);
        Ok(())
    }
}
