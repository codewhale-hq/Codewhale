//! Config file path resolution and TOML persistence helpers.
//!
//! These helpers are used by command handlers and non-command UI code, so
//! persistence lives outside the command tree.
//!
//! Every `config.toml` mutation funnels through [`mutate_config_document`]:
//! the file is edited in place with `toml_edit` so unrelated comments,
//! ordering, and formatting survive, and the result is replaced atomically
//! (same-directory temp file + rename) with owner-only permissions.

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::config::{ProviderIdentity, StatusItem, expand_path};
use codewhale_config::ProviderKind;

/// Parse the TOML document at `path` (an absent or empty file yields an empty
/// document), apply `mutate`, and atomically persist the result.
///
/// This is the single write path for TUI config mutations: `toml_edit` keeps
/// user comments and formatting intact, and the temp-file + rename write can
/// never leave a half-written config behind.
pub fn mutate_config_document<F>(path: &Path, mutate: F) -> anyhow::Result<()>
where
    F: FnOnce(&mut toml_edit::DocumentMut) -> anyhow::Result<()>,
{
    mutate_config_document_with_migration(path, |doc, _| mutate(doc))
}

/// [`mutate_config_document`], also handing `mutate` the receipt of the legacy
/// top-level `base_url` / `api_key` move this same write made (#6394).
pub(crate) fn mutate_config_document_with_migration<F>(path: &Path, mutate: F) -> anyhow::Result<()>
where
    F: FnOnce(
        &mut toml_edit::DocumentMut,
        &codewhale_config::legacy_root::LegacyRootMigration,
    ) -> anyhow::Result<()>,
{
    codewhale_config::mutate_config_document_with_migration(path, |doc, moved| {
        migrate_legacy_route_preferences(path, doc)?;
        mutate(doc, moved)
    })
}

/// Commit the legacy effective startup selection with its receipt in the same
/// atomic config write. Settings remains untouched, so an interrupted cleanup
/// or an older binary cannot erase the user's historical choices. After this
/// marker, Config never consults those legacy route fields again.
pub fn migrate_legacy_route_preferences(
    path: &Path,
    doc: &mut toml_edit::DocumentMut,
) -> anyhow::Result<()> {
    if !crate::config::is_home_config_path(path)
        || doc
            .get("route_preferences_version")
            .and_then(toml_edit::Item::as_integer)
            .is_some()
    {
        return Ok(());
    }
    let mut config = crate::config::parse_config_base(&doc.to_string()).map_err(|_| {
        anyhow::anyhow!(
            "Could not parse configuration for route preference migration; contents omitted"
        )
    })?;
    let previous_config = config.clone();
    let settings =
        crate::settings::Settings::load_legacy_route_preferences_read_only().map_err(|_| {
            anyhow::anyhow!(
                "Could not read legacy route preferences; configuration was not changed"
            )
        })?;
    // An unparsable settings.toml loads as defaults carrying `load_error`, which
    // keeps the UI usable but is not evidence that no preferences were saved.
    // This migration is one-way: stamping the version over defaults would retire
    // the user's real legacy choices unread. Refuse instead, exactly as
    // `Settings::save_to_path` refuses to overwrite an unreadable document.
    // Contents stay omitted; the file may hold private text.
    anyhow::ensure!(
        settings.load_error.is_none(),
        "Could not read legacy route preferences; configuration was not changed"
    );
    if let Some(provider) = settings
        .default_provider
        .as_deref()
        .filter(|provider| !provider.trim().is_empty())
    {
        let identity = config.resolve_provider_identity(provider).map_err(|_| anyhow::anyhow!("Legacy saved provider is unavailable in this configuration; route preference migration did not change files"))?;
        let mut scoped = config.clone();
        scoped.scope_to_provider_identity(&identity).map_err(|_| anyhow::anyhow!("Legacy saved provider cannot be selected from this configuration; route preference migration did not change files"))?;
    }
    config.apply_saved_selection(&settings);
    if let Some(models) = &settings.provider_models {
        for (provider, model) in models {
            let model = model.trim();
            if model.is_empty() || model.chars().any(char::is_control) {
                continue;
            }
            let identity = config.legacy_selection_identity(provider).map_err(|_| anyhow::anyhow!("Legacy saved model refers to an unavailable provider; route preference migration did not change files"))?;
            let mut scoped = config.clone();
            scoped.scope_to_provider_identity(&identity).map_err(|_| anyhow::anyhow!("Legacy saved model provider cannot be selected from this configuration; route preference migration did not change files"))?;
        }
    }
    let active_identity = config.active_provider_identity().ok();
    let selector = active_identity
        .as_ref()
        .and_then(|identity| {
            identity
                .migrated_legacy_ollama_cloud_route
                .then(|| identity.persisted_id())
                .flatten()
        })
        .or(config.provider.as_deref());
    if let Some(provider) = selector {
        if previous_config.provider.as_deref() != Some(provider)
            && let Some(previous) = previous_config.provider.as_deref()
        {
            set_document_value(
                doc,
                &["route_preferences_migration", "previous_provider"],
                previous,
            )?;
        }
        set_document_value(doc, &["provider"], provider)?;
    }
    let mut providers: Vec<&str> = settings
        .provider_models
        .as_ref()
        .map(|models| models.keys().map(String::as_str).collect())
        .unwrap_or_default();
    if settings.default_model.is_some() {
        for provider in [
            ProviderKind::Deepseek.as_str(),
            codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id,
        ] {
            if !providers.contains(&provider) {
                providers.push(provider);
            }
        }
    }
    providers.sort_unstable();
    for provider in providers {
        let Ok(identity) = config.legacy_selection_identity(provider) else {
            continue;
        };
        let mut scoped = config.clone();
        scoped
            .scope_to_provider_identity(&identity)
            .map_err(anyhow::Error::msg)?;
        let model =
            if identity.provider == ProviderKind::Custom && identity.persisted_id().is_none() {
                scoped.default_text_model.as_deref()
            } else {
                scoped
                    .provider_config_for(&identity)
                    .and_then(|entry| entry.model.as_deref())
            };
        if let Some(model) = model {
            let mut previous = previous_config.clone();
            previous
                .scope_to_provider_identity(&identity)
                .map_err(anyhow::Error::msg)?;
            if let Some(old_model) = previous
                .provider_config_for(&identity)
                .and_then(|entry| entry.model.as_deref())
                .or_else(|| {
                    (previous_config.active_provider_identity().ok().as_ref() == Some(&identity))
                        .then_some(previous_config.default_text_model.as_deref())
                        .flatten()
                })
                && old_model != model
            {
                // Keep the displaced Config choice as an inert migration
                // receipt. Nothing resolves routes from this archive.
                set_document_value(
                    doc,
                    &[
                        "route_preferences_migration",
                        "previous_models",
                        identity.key.as_str(),
                    ],
                    old_model,
                )?;
            }
            set_verified_provider_model_document(doc, &config, &identity, model)?;
        }
    }
    set_document_value(doc, &["route_preferences_version"], 1_i64)
}

// `config` is the freshly parsed same-lock document, before any intended delta.
// Reuse that projection so a root receipt is not dropped by a second parse of
// the canonicalized document. Every path verifies before writing the delta.
pub fn set_verified_provider_model_document(
    doc: &mut toml_edit::DocumentMut,
    config: &crate::config::Config,
    identity: &ProviderIdentity,
    model: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !model.trim().is_empty() && !model.chars().any(char::is_control),
        "model must be nonempty and contain no control characters"
    );
    config
        .verify_provider_identity(identity)
        .map_err(anyhow::Error::msg)?;
    let provider_key = identity.config_table_key()?;
    set_document_value(doc, &["providers", provider_key, "model"], model)
}

/// Atomically replace `path` with `body` via a same-directory temp file and
/// rename. On Unix the file lands with 0o600 permissions: config.toml can
/// hold API keys, so this matches `ConfigStore::save` and the auth save path.
pub(crate) fn write_config_toml_atomic(path: &Path, body: &str) -> anyhow::Result<()> {
    codewhale_config::create_config_document(path, body)
}

/// Set the value at `segments` (parent tables plus the final key), creating
/// missing intermediate tables. Replacing an existing value keeps its decor,
/// so comments above the key and trailing same-line comments survive.
///
/// Segments are separate strings rather than one dotted key, so table names
/// that need quoting (`[providers."my.provider"]`) resolve correctly.
pub fn set_document_value(
    doc: &mut toml_edit::DocumentMut,
    segments: &[&str],
    value: impl Into<toml_edit::Value>,
) -> anyhow::Result<()> {
    codewhale_config::set_config_document_value(doc, segments, value)
}

/// Remove the value at `segments`. Returns `Ok(true)` when an entry was
/// removed; missing keys and missing (or non-table) parents are a no-op.
pub fn unset_document_value(
    doc: &mut toml_edit::DocumentMut,
    segments: &[&str],
) -> anyhow::Result<bool> {
    codewhale_config::unset_config_document_value(doc, segments)
}

/// Remove every entry named `key` from `table` and, recursively, from nested
/// tables, inline tables, and arrays of tables. Used by `/logout` to strip
/// `api_key` everywhere without disturbing keys like `api_key_env`.
pub(crate) fn remove_document_key_recursive(table: &mut dyn toml_edit::TableLike, key: &str) {
    remove_key_preserving_leading_decor(table, key);
    for (_, item) in table.iter_mut() {
        if let toml_edit::Item::ArrayOfTables(tables) = item {
            for nested in tables.iter_mut() {
                remove_document_key_recursive(nested, key);
            }
        } else if let Some(nested) = item.as_table_like_mut() {
            remove_document_key_recursive(nested, key);
        }
    }
}

fn remove_key_preserving_leading_decor(table: &mut dyn toml_edit::TableLike, key: &str) -> bool {
    let mut found = false;
    let next_key = table.iter().find_map(|(candidate, _)| {
        if found {
            Some(candidate.to_owned())
        } else {
            found = candidate == key;
            None
        }
    });
    let leading_prefix = leading_prefix_for_key(table, key);
    if table.remove(key).is_none() {
        return false;
    }
    let Some(prefix) = leading_prefix else {
        return true;
    };
    let Some(next_key) = next_key else {
        return true;
    };
    if prefix.as_str() == Some("") {
        return true;
    }
    if let Some(mut next_key_decor) = table.key_mut(&next_key)
        && decor_prefix_is_empty(next_key_decor.leaf_decor())
    {
        next_key_decor.leaf_decor_mut().set_prefix(prefix);
    }
    true
}

fn decor_prefix_is_empty(decor: &toml_edit::Decor) -> bool {
    match decor.prefix() {
        Some(prefix) => prefix.as_str() == Some(""),
        None => true,
    }
}

fn leading_prefix_for_key(
    table: &dyn toml_edit::TableLike,
    key: &str,
) -> Option<toml_edit::RawString> {
    table
        .key(key)
        .and_then(|key| key.leaf_decor().prefix().cloned())
        .or_else(|| {
            table
                .get(key)
                .and_then(|item| item.as_value())
                .and_then(|value| value.decor().prefix().cloned())
        })
}

pub fn persist_status_items(items: &[StatusItem]) -> anyhow::Result<PathBuf> {
    let path = config_toml_path(None)?;
    let items: toml_edit::Array = items.iter().map(|item| item.key()).collect();
    mutate_config_document(&path, |doc| {
        set_document_value(doc, &["tui", "status_items"], items)
    })?;
    Ok(path)
}

pub fn persist_root_string_key(
    config_path: Option<&Path>,
    key: &str,
    value: &str,
) -> anyhow::Result<PathBuf> {
    let path = config_toml_path(config_path)?;
    mutate_config_document(&path, |doc| set_document_value(doc, &[key], value))?;
    Ok(path)
}

pub fn persist_unset_root_key(config_path: Option<&Path>, key: &str) -> anyhow::Result<PathBuf> {
    let path = config_toml_path(config_path)?;
    mutate_config_document(&path, |doc| unset_document_value(doc, &[key]).map(|_| ()))?;
    Ok(path)
}

pub fn persist_root_bool_key(
    config_path: Option<&Path>,
    key: &str,
    value: bool,
) -> anyhow::Result<PathBuf> {
    let path = config_toml_path(config_path)?;
    mutate_config_document(&path, |doc| set_document_value(doc, &[key], value))?;
    Ok(path)
}

pub fn persist_subagents_bool_key(
    config_path: Option<&Path>,
    key: &str,
    value: bool,
) -> anyhow::Result<PathBuf> {
    persist_table_value_key(config_path, "subagents", key, value.into())
}

pub fn persist_mini_window_bool_key(
    config_path: Option<&Path>,
    key: &str,
    value: bool,
) -> anyhow::Result<PathBuf> {
    persist_table_value_key(config_path, "mini_window", key, value.into())
}

pub fn persist_subagents_integer_key(
    config_path: Option<&Path>,
    key: &str,
    value: u64,
) -> anyhow::Result<PathBuf> {
    let value = i64::try_from(value).context("integer value is too large for TOML")?;
    persist_table_value_key(config_path, "subagents", key, value.into())
}

pub fn persist_table_bool_key(
    config_path: Option<&Path>,
    table_name: &str,
    key: &str,
    value: bool,
) -> anyhow::Result<PathBuf> {
    persist_table_value_key(config_path, table_name, key, value.into())
}

pub fn persist_table_string_key(
    config_path: Option<&Path>,
    table_name: &str,
    key: &str,
    value: &str,
) -> anyhow::Result<PathBuf> {
    persist_table_value_key(config_path, table_name, key, value.into())
}

pub fn persist_table_value_key(
    config_path: Option<&Path>,
    table_name: &str,
    key: &str,
    value: toml_edit::Value,
) -> anyhow::Result<PathBuf> {
    let path = config_toml_path(config_path)?;
    mutate_config_document(&path, |doc| {
        set_document_value(doc, &[table_name, key], value)
    })?;
    Ok(path)
}

/// Persist the endpoint that `GET /v1/config` reports as `base_url`.
///
/// That value belongs to exactly one route, and the route keeps it in exactly
/// one place: its own `[providers.<table>]` table — the typed table for a
/// built-in, the exact `[providers.<name>]` table for a custom route. There is
/// no top-level `base_url` any more (#6394); writing one used to hand a
/// DeepSeek endpoint to every route that inherited it.
pub fn persist_route_base_url(
    config_path: Option<&Path>,
    identity: &ProviderIdentity,
    value: &str,
) -> anyhow::Result<PathBuf> {
    let path = config_toml_path(config_path)?;
    mutate_config_document_with_migration(&path, |doc, moved| {
        let config = crate::config::parse_config_after_locked_migration(&doc.to_string(), moved)
            .map_err(|_| anyhow::anyhow!("Could not parse destination route; contents omitted"))?;
        config
            .verify_provider_identity(identity)
            .map_err(anyhow::Error::msg)?;
        let table = if identity.provider == ProviderKind::Custom {
            identity.key.as_str()
        } else {
            provider_base_url_table_key(identity)?
        };
        set_document_value(doc, &["providers", table, "base_url"], value)
    })?;
    Ok(path)
}

/// Persist the model for one exact provider route without rewriting the
/// legacy root DeepSeek fallback used by unrelated providers.
///
/// Built-in providers write to their typed `[providers.<name>]` table, while
/// named custom routes use their exact user-owned table id. The locked writer first preserves legacy root provenance, then writes the
/// canonical leaf created by its one-way migration.
pub fn persist_provider_model_key(
    config_path: Option<&Path>,
    identity: &ProviderIdentity,
    value: &str,
) -> anyhow::Result<PathBuf> {
    let path = config_toml_path(config_path)?;
    mutate_config_document_with_migration(&path, |doc, moved| {
        let config = crate::config::parse_config_after_locked_migration(&doc.to_string(), moved)
            .map_err(|_| anyhow::anyhow!("Could not parse destination route; contents omitted"))?;
        set_verified_provider_model_document(doc, &config, identity, value)
    })?;
    Ok(path)
}

fn provider_base_url_table_key(identity: &ProviderIdentity) -> anyhow::Result<&'static str> {
    identity
        .compatibility()
        .map(|row| row.base_url_config_key)
        .context("custom providers store base_url in their named [providers.<name>] table")
}

pub fn persist_custom_provider(
    config_path: Option<&Path>,
    provider_id: &str,
    base_url: &str,
    model: Option<&str>,
    api_key_env: Option<&str>,
) -> anyhow::Result<PathBuf> {
    let provider_id = normalize_custom_provider_id(provider_id)?;
    let base_url = normalize_custom_provider_base_url(base_url)?;
    let model = model.and_then(normalize_optional_custom_provider_field);
    let api_key_env = api_key_env.and_then(normalize_optional_custom_provider_field);

    let path = config_toml_path(config_path)?;
    mutate_config_document(&path, |doc| {
        let entry = ["providers", provider_id.as_str()];
        set_document_value(doc, &["provider"], provider_id.as_str())?;
        set_document_value(doc, &[entry[0], entry[1], "kind"], "openai-compatible")?;
        set_document_value(doc, &[entry[0], entry[1], "base_url"], base_url.as_str())?;
        if provider_id == "ds4" && crate::config::base_url_uses_local_host(&base_url) {
            // Match the documented starter server. DS4 explicitly requires
            // clients not to budget beyond the server's --ctx value.
            set_document_value(doc, &[entry[0], entry[1], "context_window"], 100_000)?;
        }
        match model.as_deref() {
            Some(model) => set_document_value(doc, &[entry[0], entry[1], "model"], model)?,
            None => {
                unset_document_value(doc, &[entry[0], entry[1], "model"])?;
            }
        }
        match api_key_env.as_deref() {
            Some(env) => {
                set_document_value(doc, &[entry[0], entry[1], "api_key_env"], env)?;
                unset_document_value(doc, &[entry[0], entry[1], "auth_mode"])?;
            }
            None => {
                unset_document_value(doc, &[entry[0], entry[1], "api_key_env"])?;
                if provider_id == "ds4" && crate::config::base_url_uses_local_host(&base_url) {
                    set_document_value(doc, &[entry[0], entry[1], "auth_mode"], "none")?;
                } else {
                    unset_document_value(doc, &[entry[0], entry[1], "auth_mode"])?;
                }
            }
        }
        Ok(())
    })?;
    Ok(path)
}

fn normalize_custom_provider_id(raw: &str) -> anyhow::Result<String> {
    use anyhow::bail;

    let value = raw.trim();
    if value.is_empty() {
        bail!("custom provider name is required");
    }
    if value == "__custom__" {
        bail!("custom provider name is reserved");
    }
    if codewhale_config::ProviderKind::parse(value).is_some() {
        bail!("custom provider name must not shadow a built-in provider");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        bail!("custom provider name may only use letters, numbers, '-' and '_'");
    }
    Ok(value.to_string())
}

fn normalize_custom_provider_base_url(raw: &str) -> anyhow::Result<String> {
    use anyhow::bail;

    let value = raw.trim().trim_end_matches('/');
    if value.is_empty() {
        bail!("custom provider base URL is required");
    }
    let parsed = reqwest::Url::parse(value)
        .map_err(|err| anyhow::anyhow!("custom provider base URL is invalid: {err}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        bail!("custom provider base URL must be an http(s) URL with a host");
    }
    Ok(value.to_string())
}

fn normalize_optional_custom_provider_field(raw: &str) -> Option<String> {
    let value = raw.trim();
    (!value.is_empty()).then(|| value.to_string())
}

pub fn persist_hotbar_bindings(
    config_path: Option<&Path>,
    bindings: &[codewhale_config::HotbarBindingToml],
) -> anyhow::Result<PathBuf> {
    let path = config_toml_path(config_path)?;
    mutate_config_document(&path, |doc| {
        let table = doc.as_table_mut();
        table.remove("hotbar");
        if bindings.is_empty() {
            table.insert(
                "hotbar",
                toml_edit::Item::Value(toml_edit::Value::Array(toml_edit::Array::new())),
            );
        } else {
            let mut hotbar = toml_edit::ArrayOfTables::new();
            for binding in bindings {
                let mut entry = toml_edit::Table::new();
                entry["slot"] = toml_edit::value(i64::from(binding.slot));
                entry["action"] = toml_edit::value(binding.action.clone());
                if let Some(label) = binding.label.as_deref() {
                    entry["label"] = toml_edit::value(label);
                }
                hotbar.push(entry);
            }
            table.insert("hotbar", toml_edit::Item::ArrayOfTables(hotbar));
        }
        Ok(())
    })?;
    Ok(path)
}

pub fn config_toml_path(config_path: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(path) = config_path {
        return Ok(expand_path(path.to_string_lossy().as_ref()));
    }
    crate::config::resolve_load_config_path(None)?
        .context("failed to resolve the active config.toml path")
}

#[cfg(test)]
mod tests {

    use super::*;

    use std::env;

    use std::fs;

    use std::path::Path;

    use std::time::SystemTime;

    use super::test_support::*;
    use std::time::UNIX_EPOCH;

    struct EnvGuard {
        _home: crate::test_support::EnvVarGuard,
        _userprofile: crate::test_support::EnvVarGuard,
        _codewhale_home: crate::test_support::EnvVarGuard,
        _codewhale_config_path: crate::test_support::EnvVarGuard,
        _deepseek_config_path: crate::test_support::EnvVarGuard,
        _lock: crate::test_support::TestEnvLock,
    }

    impl EnvGuard {
        fn new(home: &Path) -> Self {
            let lock = crate::test_support::lock_test_env();
            let config_path = home.join(".deepseek").join("config.toml");
            Self {
                _home: crate::test_support::EnvVarGuard::set("HOME", home),
                _userprofile: crate::test_support::EnvVarGuard::set("USERPROFILE", home),
                _codewhale_home: crate::test_support::EnvVarGuard::remove("CODEWHALE_HOME"),
                _codewhale_config_path: crate::test_support::EnvVarGuard::remove(
                    "CODEWHALE_CONFIG_PATH",
                ),
                _deepseek_config_path: crate::test_support::EnvVarGuard::set(
                    "DEEPSEEK_CONFIG_PATH",
                    &config_path,
                ),
                _lock: lock,
            }
        }
    }

    fn temp_root(prefix: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn persist_status_items_writes_tui_section_to_config_toml() {
        let temp_root = temp_root("codewhale-statusline-persist");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);

        let items = vec![
            crate::config::StatusItem::Mode,
            crate::config::StatusItem::Model,
            crate::config::StatusItem::Cost,
        ];

        let path = persist_status_items(&items).expect("persist should succeed");
        let body = fs::read_to_string(&path).expect("written file should be readable");
        assert!(body.contains("[tui]"), "expected [tui] section in {body}");
        assert!(
            body.contains("status_items"),
            "expected status_items key in {body}"
        );
        assert!(body.contains("\"mode\""), "expected mode key in {body}");
        assert!(body.contains("\"cost\""), "expected cost key in {body}");
    }

    #[test]
    fn config_toml_path_uses_codewhale_home_for_fresh_installs() {
        let temp_root = temp_root("codewhale-config-path-fresh");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);

        unsafe {
            env::remove_var("DEEPSEEK_CONFIG_PATH");
        }

        assert_eq!(
            config_toml_path(None).unwrap(),
            temp_root.join(".codewhale").join("config.toml")
        );
    }

    #[test]
    fn config_toml_path_preserves_legacy_config_when_it_exists() {
        let temp_root = temp_root("codewhale-config-path-legacy");
        let legacy_config = temp_root.join(".deepseek").join("config.toml");
        fs::create_dir_all(legacy_config.parent().unwrap()).unwrap();
        fs::write(&legacy_config, "").unwrap();
        let _guard = EnvGuard::new(&temp_root);

        unsafe {
            env::remove_var("DEEPSEEK_CONFIG_PATH");
        }

        assert_eq!(config_toml_path(None).unwrap(), legacy_config);
    }

    #[test]
    fn config_toml_path_ignores_legacy_config_when_codewhale_home_is_explicit() {
        let temp_root = temp_root("codewhale-config-path-explicit-home");
        let explicit_home = temp_root.join("isolated-codewhale");
        let legacy_config = temp_root.join(".deepseek").join("config.toml");
        fs::create_dir_all(legacy_config.parent().unwrap()).unwrap();
        fs::write(&legacy_config, "").unwrap();
        let _guard = EnvGuard::new(&temp_root);

        unsafe {
            env::remove_var("DEEPSEEK_CONFIG_PATH");
            env::set_var("CODEWHALE_HOME", &explicit_home);
        }

        assert_eq!(
            config_toml_path(None).unwrap(),
            explicit_home.join("config.toml")
        );
    }

    #[test]
    fn config_toml_path_prefers_codewhale_env_over_legacy_env() {
        let temp_root = temp_root("codewhale-config-path-env");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);
        let preferred = temp_root.join("preferred.toml");
        let legacy = temp_root.join("legacy.toml");

        unsafe {
            env::set_var("CODEWHALE_CONFIG_PATH", &preferred);
            env::set_var("DEEPSEEK_CONFIG_PATH", &legacy);
        }

        let expected = preferred
            .parent()
            .expect("preferred path has a parent")
            .canonicalize()
            .expect("preferred parent should canonicalize")
            .join("preferred.toml");
        assert_eq!(config_toml_path(None).unwrap(), expected);
    }

    #[test]
    fn config_toml_path_keeps_missing_env_target_authoritative() {
        let temp_root = temp_root("codewhale-config-path-missing-env-fallback");
        let home_config = temp_root.join(".codewhale").join("config.toml");
        fs::create_dir_all(home_config.parent().unwrap()).unwrap();
        fs::write(&home_config, "# existing fallback\n").unwrap();
        let _guard = EnvGuard::new(&temp_root);
        let missing_env = temp_root.join("override").join("missing.toml");

        unsafe {
            env::set_var("DEEPSEEK_CONFIG_PATH", &missing_env);
        }

        assert_eq!(config_toml_path(None).unwrap(), missing_env);
        assert!(home_config.exists());
        assert!(!missing_env.exists());
    }

    #[test]
    fn persist_status_items_preserves_existing_unrelated_keys() {
        let temp_root = temp_root("codewhale-statusline-preserve");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);

        let path = temp_root.join(".deepseek").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "api_key = \"sentinel-key\"\nmodel = \"deepseek-v4-pro\"\n",
        )
        .unwrap();

        let written = persist_status_items(&[crate::config::StatusItem::Mode])
            .expect("persist should succeed");
        let body = fs::read_to_string(&written).expect("written file should be readable");
        assert!(
            body.contains("api_key = \"sentinel-key\""),
            "round-trip lost api_key: {body}"
        );
        assert!(
            body.contains("model = \"deepseek-v4-pro\""),
            "round-trip lost model: {body}"
        );
        assert!(
            body.contains("status_items"),
            "expected status_items in {body}"
        );
    }

    #[test]
    fn persist_bool_key_preserves_comments() {
        let temp_root = temp_root("codewhale-persist-comments");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);

        let path = temp_root.join(".deepseek").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "# my note\nmodel = \"deepseek-v4-flash\"\n# disabled = true\n",
        )
        .unwrap();

        let written = persist_root_bool_key(Some(&path), "allow_shell", true)
            .expect("persist should succeed");
        let body = fs::read_to_string(&written).expect("written file should be readable");
        assert!(body.contains("# my note"), "prefix comment lost: {body}");
        assert!(
            body.contains("# disabled = true"),
            "disabled key lost: {body}"
        );
        assert!(
            body.contains("allow_shell = true"),
            "new key not written: {body}"
        );
    }

    #[test]
    fn persist_table_bool_key_updates_existing_memory_enabled() {
        let temp_root = temp_root("codewhale-persist-memory-update");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);

        let path = temp_root.join(".deepseek").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "allow_shell = true\n\n[memory]\nenabled = true\n").unwrap();

        let written = persist_table_bool_key(Some(&path), "memory", "enabled", false)
            .expect("persist should succeed");
        let body = fs::read_to_string(&written).expect("written file should be readable");
        assert!(
            body.contains("enabled = false"),
            "memory enabled should be false: {body}"
        );
        assert!(
            !body.contains("enabled = true"),
            "memory enabled should not still be true: {body}"
        );
    }

    #[test]
    fn persist_memory_enabled_round_trips_through_config_load() {
        let temp_root = temp_root("codewhale-persist-memory-roundtrip");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);

        let path = temp_root.join(".deepseek").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Initial config has memory enabled = true
        fs::write(&path, "allow_shell = true\n\n[memory]\nenabled = true\n").unwrap();

        // Verify initial state
        let cfg0 = crate::config::Config::load(Some(path.clone()), None)
            .expect("initial config should load");
        assert!(cfg0.memory_enabled(), "memory should be enabled initially");

        // Persist memory.enabled = false (what the GUI's set_config endpoint does)
        persist_table_bool_key(Some(&path), "memory", "enabled", false)
            .expect("persist should succeed");

        // Reload config from disk and verify memory_enabled() reflects the change
        let cfg1 = crate::config::Config::load(Some(path.clone()), None)
            .expect("reloaded config should load");
        assert!(
            !cfg1.memory_enabled(),
            "memory should be disabled after persisting false"
        );
    }

    #[test]
    fn persist_custom_provider_writes_named_openai_compatible_table() {
        let temp_root = temp_root("codewhale-custom-provider-persist");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);

        let path = temp_root.join(".codewhale").join("config.toml");
        let written = persist_custom_provider(
            Some(&path),
            "acme_ai",
            "https://api.acme.example/v1/",
            Some("acme/code-1"),
            Some("ACME_API_KEY"),
        )
        .expect("custom provider should persist");
        let body = fs::read_to_string(&written).expect("written file should be readable");

        assert!(body.contains("provider = \"acme_ai\""), "{body}");
        assert!(body.contains("[providers.acme_ai]"), "{body}");
        assert!(body.contains("kind = \"openai-compatible\""), "{body}");
        assert!(
            body.contains("base_url = \"https://api.acme.example/v1\""),
            "{body}"
        );
        assert!(body.contains("model = \"acme/code-1\""), "{body}");
        assert!(body.contains("api_key_env = \"ACME_API_KEY\""), "{body}");
        assert!(
            !body.contains("sk-"),
            "helper must not persist raw secret values: {body}"
        );

        let loaded =
            crate::config::Config::load(Some(written.clone()), None).expect("config should load");
        assert_eq!(loaded.provider.as_deref(), Some("acme_ai"));
        assert_eq!(
            loaded.active_provider_identity().unwrap().provider,
            codewhale_config::ProviderKind::Custom
        );
        let entry = loaded
            .providers
            .as_ref()
            .and_then(|providers| providers.custom_provider_config("acme_ai"))
            .expect("custom provider entry");
        assert!(entry.is_openai_compatible_custom());
        assert_eq!(
            entry.base_url.as_deref(),
            Some("https://api.acme.example/v1")
        );
        assert_eq!(entry.model.as_deref(), Some("acme/code-1"));
        assert_eq!(entry.api_key_env.as_deref(), Some("ACME_API_KEY"));

        let dispatcher = codewhale_config::ConfigStore::load(Some(written))
            .expect("the dispatcher must parse the exact config written by the TUI");
        assert_eq!(
            dispatcher.config.provider,
            codewhale_config::ProviderKind::Custom
        );
        assert_eq!(dispatcher.config.provider_id(), "acme_ai");
    }

    #[test]
    fn persist_custom_provider_rejects_builtin_or_invalid_names() {
        let temp_root = temp_root("codewhale-custom-provider-invalid");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);
        let path = temp_root.join(".codewhale").join("config.toml");

        let builtin = persist_custom_provider(
            Some(&path),
            "openrouter",
            "https://api.example.invalid/v1",
            None,
            None,
        )
        .expect_err("built-in names should be rejected");
        assert!(builtin.to_string().contains("built-in provider"));

        let bad_chars = persist_custom_provider(
            Some(&path),
            "my provider",
            "https://api.example.invalid/v1",
            None,
            None,
        )
        .expect_err("space in name should be rejected");
        assert!(bad_chars.to_string().contains("letters, numbers"));
    }

    #[test]
    fn persist_local_custom_provider_records_keyless_auth() {
        let temp_root = temp_root("codewhale-custom-provider-local-keyless");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);
        let path = temp_root.join(".codewhale").join("config.toml");

        let written = persist_custom_provider(
            Some(&path),
            "ds4",
            "http://127.0.0.1:8000/v1",
            Some("deepseek-v4-flash"),
            None,
        )
        .expect("DS4 preset should persist");
        let body = fs::read_to_string(&written).expect("written config");

        assert!(body.contains("provider = \"ds4\""), "{body}");
        assert!(body.contains("auth_mode = \"none\""), "{body}");
        assert!(body.contains("context_window = 100000"), "{body}");
        assert!(!body.contains("api_key"), "{body}");
    }

    #[test]
    fn persist_hotbar_bindings_writes_primary_config_path_for_fresh_installs() {
        let temp_root = temp_root("codewhale-hotbar-persist-fresh");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);

        unsafe {
            env::remove_var("DEEPSEEK_CONFIG_PATH");
        }

        let bindings = vec![codewhale_config::HotbarBindingToml {
            slot: 1,
            action: "mode.plan".to_string(),
            label: Some("Plan".to_string()),
        }];
        let path = persist_hotbar_bindings(None, &bindings).expect("persist should succeed");

        assert_eq!(path, temp_root.join(".codewhale").join("config.toml"));
        let body = fs::read_to_string(&path).expect("written file should be readable");
        assert!(body.contains("[[hotbar]]"), "hotbar table missing: {body}");
        let parsed: codewhale_config::ConfigToml =
            toml::from_str(&body).expect("written hotbar config should parse");
        assert_eq!(parsed.hotbar, Some(bindings));
    }

    #[test]
    fn persist_default_hotbar_bindings_round_trips_for_hotbar_on() {
        // #3807: `/hotbar on` persists the explicit default slots (an absent key
        // now means hidden), and they read back as the eight recommended slots.
        let temp_root = temp_root("codewhale-hotbar-on-defaults");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);

        let defaults = codewhale_config::default_hotbar_bindings_toml();
        assert_eq!(defaults.len(), codewhale_config::HOTBAR_SLOT_COUNT as usize);

        let path = persist_hotbar_bindings(None, &defaults).expect("persist should succeed");
        let body = fs::read_to_string(&path).expect("written file should be readable");
        assert!(body.contains("[[hotbar]]"), "hotbar table missing: {body}");

        let parsed: codewhale_config::ConfigToml =
            toml::from_str(&body).expect("written hotbar config should parse");
        assert_eq!(parsed.hotbar, Some(defaults));

        // The persisted defaults resolve back to all eight recommended slots.
        let resolved = parsed.resolve_hotbar_bindings(&codewhale_config::DEFAULT_HOTBAR_ACTIONS);
        assert_eq!(
            resolved.bindings,
            codewhale_config::default_hotbar_bindings()
        );
    }

    #[test]
    fn persist_hotbar_bindings_preserves_comments_and_replaces_existing_tables() {
        let temp_root = temp_root("codewhale-hotbar-persist-comments");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);

        let path = temp_root.join(".codewhale").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"# model note
model = "deepseek-v4-flash"

[[hotbar]]
slot = 1
action = "mode.plan"
label = "Plan"

# notification note
[notifications]
enabled = true
"#,
        )
        .unwrap();

        let bindings = vec![codewhale_config::HotbarBindingToml {
            slot: 2,
            action: "session.compact".to_string(),
            label: Some("Compact".to_string()),
        }];
        let written =
            persist_hotbar_bindings(Some(&path), &bindings).expect("persist should succeed");
        let body = fs::read_to_string(&written).expect("written file should be readable");

        assert!(body.contains("# model note"), "prefix comment lost: {body}");
        assert!(
            body.contains("# notification note"),
            "section comment lost: {body}"
        );
        assert!(
            !body.contains("mode.plan"),
            "old hotbar table was not replaced: {body}"
        );
        assert!(body.contains("[[hotbar]]"), "hotbar table missing: {body}");
        assert!(
            body.contains("action = \"session.compact\""),
            "new action missing: {body}"
        );
        let parsed: codewhale_config::ConfigToml =
            toml::from_str(&body).expect("written hotbar config should parse");
        assert_eq!(parsed.hotbar, Some(bindings));
    }

    #[test]
    fn persist_hotbar_bindings_writes_empty_array_to_disable_defaults() {
        let temp_root = temp_root("codewhale-hotbar-persist-empty");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);

        let path = temp_root.join(".codewhale").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        let written = persist_hotbar_bindings(Some(&path), &[]).expect("persist should succeed");
        let body = fs::read_to_string(&written).expect("written file should be readable");

        assert!(body.contains("hotbar = []"), "empty hotbar missing: {body}");
        let parsed: codewhale_config::ConfigToml =
            toml::from_str(&body).expect("written hotbar config should parse");
        assert_eq!(parsed.hotbar, Some(Vec::new()));
    }

    // ------------------------------------------------------------------
    // Golden-file coverage for the shared toml_edit mutation path
    // (findings #18/#19/#20): unrelated comments, ordering, and quoted
    // provider tables must survive every supported mutation.
    // ------------------------------------------------------------------

    const GOLDEN_CONFIG: &str = r#"# CodeWhale golden config fixture, top note.
# api_key = "sk-placeholder" (uncomment to set the key by hand)
model = "deepseek-v4-pro" # pinned for release QA

# workspace trust note
[projects."/Users/example/work"]
trust_level = "trusted" # granted manually

# providers note
[providers.openrouter]
base_url = "https://openrouter.ai/api/v1" # keep in sync with docs

[providers."quoted.provider"]
base_url = "https://quoted.example/v1"

[[hotbar]]
slot = 1
action = "mode.plan"
"#;

    fn write_golden_config(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, GOLDEN_CONFIG).unwrap();
    }

    #[test]
    fn golden_replacing_existing_root_value_only_touches_that_value() {
        let temp_root = temp_root("codewhale-golden-root-value");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);
        let path = temp_root.join(".deepseek").join("config.toml");
        write_golden_config(&path);

        persist_root_string_key(Some(&path), "model", "deepseek-v4-flash")
            .expect("persist should succeed");

        let body = fs::read_to_string(&path).unwrap();
        // The fixture is a pre-migration home config, so this first write also
        // commits the one-way route-preference migration receipt in the same
        // atomic replacement. That stamp and the model value are the only two
        // permitted edits: comments, ordering and quoted tables stay byte-exact.
        let expected = GOLDEN_CONFIG.replace(
            "model = \"deepseek-v4-pro\" # pinned for release QA",
            "model = \"deepseek-v4-flash\" # pinned for release QA\nroute_preferences_version = 1",
        );
        assert_eq!(
            body, expected,
            "only the model value and the migration stamp may change"
        );
    }

    #[test]
    fn golden_mutations_preserve_unrelated_comments_order_and_quoted_tables() {
        let temp_root = temp_root("codewhale-golden-mutations");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);
        let path = temp_root.join(".deepseek").join("config.toml");
        write_golden_config(&path);

        persist_root_bool_key(Some(&path), "allow_shell", true).unwrap();
        persist_table_value_key(Some(&path), "tui", "scrollback_lines", 4000_i64.into()).unwrap();
        persist_table_string_key(Some(&path), "memory", "backend", "native").unwrap();
        persist_subagents_bool_key(Some(&path), "enabled", true).unwrap();
        persist_route_base_url(
            Some(&path),
            &selection_identity(&path, "openrouter"),
            "https://openrouter.example/v2",
        )
        .unwrap();
        persist_status_items(&[crate::config::StatusItem::Mode]).unwrap();
        persist_hotbar_bindings(
            Some(&path),
            &[codewhale_config::HotbarBindingToml {
                slot: 2,
                action: "session.compact".to_string(),
                label: None,
            }],
        )
        .unwrap();

        let body = fs::read_to_string(&path).unwrap();
        for comment in [
            "# CodeWhale golden config fixture, top note.",
            "# api_key = \"sk-placeholder\" (uncomment to set the key by hand)",
            "# pinned for release QA",
            "# workspace trust note",
            "# granted manually",
            "# providers note",
            "# keep in sync with docs",
        ] {
            assert!(body.contains(comment), "comment lost: {comment}\n{body}");
        }
        // Updated in place, keeping the trailing comment on the same line.
        assert!(
            body.contains("base_url = \"https://openrouter.example/v2\" # keep in sync with docs"),
            "{body}"
        );
        assert!(body.contains("[providers.\"quoted.provider\"]"), "{body}");
        assert!(
            !body.contains("mode.plan"),
            "old hotbar entry must be replaced: {body}"
        );

        // Original section order is intact.
        let model_at = body.find("model = ").unwrap();
        let projects_at = body.find("[projects.").unwrap();
        let providers_at = body.find("[providers.openrouter]").unwrap();
        assert!(
            model_at < projects_at && projects_at < providers_at,
            "{body}"
        );

        let parsed: toml::Value = toml::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("allow_shell").and_then(toml::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            parsed
                .get("tui")
                .and_then(|t| t.get("scrollback_lines"))
                .and_then(toml::Value::as_integer),
            Some(4000)
        );
        assert_eq!(
            parsed
                .get("memory")
                .and_then(|t| t.get("backend"))
                .and_then(toml::Value::as_str),
            Some("native")
        );
        assert_eq!(
            parsed
                .get("subagents")
                .and_then(|t| t.get("enabled"))
                .and_then(toml::Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn set_document_value_inserts_api_key_even_when_a_comment_mentions_it() {
        // Finding #20 at the primitive level: the old string scan treated a
        // comment mentioning api_key as an existing assignment and skipped
        // the insert entirely.
        let temp_root = temp_root("codewhale-golden-api-key-comment");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);
        let path = temp_root.join(".deepseek").join("config.toml");
        write_golden_config(&path);

        mutate_config_document(&path, |doc| {
            set_document_value(doc, &["api_key"], "sk-fresh")
        })
        .expect("mutation should succeed");

        let body = fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("# api_key = \"sk-placeholder\""),
            "comment lost: {body}"
        );
        let parsed: toml::Value = toml::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("api_key").and_then(toml::Value::as_str),
            Some("sk-fresh"),
            "real key must be inserted despite the comment: {body}"
        );
    }

    #[test]
    fn unset_document_value_reports_removal_and_tolerates_missing_parents() {
        let mut doc = "model = \"deepseek-v4-pro\"\n"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert!(!unset_document_value(&mut doc, &["providers", "openrouter", "api_key"]).unwrap());
        assert!(!unset_document_value(&mut doc, &["model", "nested"]).unwrap());
        assert!(unset_document_value(&mut doc, &["model"]).unwrap());
        assert!(!unset_document_value(&mut doc, &["model"]).unwrap());
    }

    #[test]
    fn unset_last_root_value_preserves_its_leading_comment() {
        let mut doc = "# keep this explanation\napproval_policy = \"on-request\"\n"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();

        assert!(unset_document_value(&mut doc, &["approval_policy"]).unwrap());

        let saved = doc.to_string();
        assert!(saved.contains("# keep this explanation"), "{saved:?}");
        assert!(!saved.contains("approval_policy"), "{saved:?}");
    }

    #[test]
    fn set_document_value_rejects_non_table_parents() {
        let mut doc = "model = \"deepseek-v4-pro\"\n"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        let err = set_document_value(&mut doc, &["model", "nested"], "x")
            .expect_err("scalar parent must be rejected");
        assert!(err.to_string().contains("must be a table"), "{err}");
    }

    #[test]
    fn remove_document_key_recursive_strips_nested_and_quoted_tables() {
        let mut doc = r#"# root note
api_key = "root"
api_key_env = "KEEP_ENV"

[providers.openrouter]
api_key = "or"
base_url = "https://openrouter.ai/api/v1"

[providers."quoted.provider"]
api_key = "quoted"

[[hotbar]]
slot = 1
"#
        .parse::<toml_edit::DocumentMut>()
        .unwrap();

        remove_document_key_recursive(doc.as_table_mut(), "api_key");

        let body = doc.to_string();
        assert!(!body.contains("api_key = "), "{body}");
        assert!(body.contains("# root note"), "{body}");
        assert!(body.contains("api_key_env = \"KEEP_ENV\""), "{body}");
        assert!(body.contains("base_url"), "{body}");
        assert!(body.contains("[[hotbar]]"), "{body}");
    }

    #[test]
    fn persist_custom_provider_unsets_removed_optional_fields() {
        let temp_root = temp_root("codewhale-custom-provider-unset");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);
        let path = temp_root.join(".codewhale").join("config.toml");

        persist_custom_provider(
            Some(&path),
            "acme_ai",
            "https://api.acme.example/v1",
            Some("acme/code-1"),
            Some("ACME_API_KEY"),
        )
        .expect("first persist should succeed");
        persist_custom_provider(
            Some(&path),
            "acme_ai",
            "https://api.acme.example/v2",
            None,
            None,
        )
        .expect("second persist should succeed");

        let body = fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = toml::from_str(&body).unwrap();
        let entry = parsed
            .get("providers")
            .and_then(|providers| providers.get("acme_ai"))
            .expect("provider entry");
        assert_eq!(
            entry.get("base_url").and_then(toml::Value::as_str),
            Some("https://api.acme.example/v2")
        );
        assert!(entry.get("model").is_none(), "model must be unset: {body}");
        assert!(
            entry.get("api_key_env").is_none(),
            "api_key_env must be unset: {body}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_writes_land_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp_root = temp_root("codewhale-persist-perms");
        fs::create_dir_all(&temp_root).unwrap();
        let _guard = EnvGuard::new(&temp_root);
        let path = temp_root.join(".deepseek").join("config.toml");
        write_golden_config(&path);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        persist_root_bool_key(Some(&path), "allow_shell", true).expect("persist should succeed");

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "config.toml can hold api keys");
    }

    /// The active model must be one answer, not two.
    ///
    /// The TUI resolves it with `Config::default_model()`, which is what
    /// `client.rs` puts on the wire and what `doctor` reports. The dispatcher
    /// resolves it independently in `codewhale-config`'s
    /// `resolve_runtime_options`, which is what `codewhale model resolve`
    /// reports and what the app-server and route descriptors consume. The two
    /// silently disagreed for every non-DeepSeek provider (#4832, #4838): the
    /// dispatcher gated root `default_text_model` behind `provider == Deepseek`
    /// and so reported a provider default while the wire carried the user's
    /// chosen model.
    ///
    /// A diagnostic that contradicts the request it is diagnosing is worse than
    /// no diagnostic, so this pins the two chains together by construction
    /// rather than asserting either one's internals.
    #[test]
    fn the_dispatcher_and_the_tui_resolve_the_same_active_model() {
        // (case, config body, what both chains must answer)
        let cases: &[(&str, &str, &str)] = &[
            (
                "a non-DeepSeek provider honours the user's chosen model",
                "provider = \"zai\"\ndefault_text_model = \"GLM-4.6\"\n\n[providers.zai]\napi_key = \"k\"\n",
                "GLM-4.6",
            ),
            (
                "a stale DeepSeek id must not be forwarded to a native non-DeepSeek endpoint",
                "provider = \"zai\"\ndefault_text_model = \"deepseek-chat\"\n\n[providers.zai]\napi_key = \"k\"\n",
                codewhale_config::descriptors::defaults::DEFAULT_ZAI_MODEL,
            ),
            (
                "no root default falls through to the provider default",
                "provider = \"zai\"\n\n[providers.zai]\napi_key = \"k\"\n",
                codewhale_config::descriptors::defaults::DEFAULT_ZAI_MODEL,
            ),
            (
                "a provider-scoped model outranks the root default",
                "provider = \"zai\"\ndefault_text_model = \"GLM-4.6\"\n\n[providers.zai]\napi_key = \"k\"\nmodel = \"GLM-4.5-Air\"\n",
                "GLM-4.5-Air",
            ),
            (
                "DeepSeek itself keeps honouring the root default",
                "provider = \"deepseek\"\ndefault_text_model = \"deepseek-v4-pro\"\n\n[providers.deepseek]\napi_key = \"k\"\n",
                "deepseek-v4-pro",
            ),
            (
                "a vendor-locked endpoint refuses a DeepSeek id (#3227)",
                "provider = \"xai\"\ndefault_text_model = \"deepseek-v4-pro\"\n\n[providers.xai]\napi_key = \"k\"\n",
                codewhale_config::descriptors::defaults::DEFAULT_XAI_MODEL,
            ),
            (
                "an aggregator legitimately serves DeepSeek ids",
                "provider = \"openrouter\"\ndefault_text_model = \"deepseek/deepseek-v4-pro\"\n\n[providers.openrouter]\napi_key = \"k\"\n",
                "deepseek/deepseek-v4-pro",
            ),
            (
                "a local runtime passes its own tag through",
                "provider = \"ollama\"\ndefault_text_model = \"qwen3-coder:30b\"\n",
                "qwen3-coder:30b",
            ),
            (
                "a custom base URL keeps full pass-through (#1519)",
                "provider = \"zai\"\ndefault_text_model = \"deepseek-chat\"\n\n[providers.zai]\napi_key = \"k\"\nbase_url = \"https://proxy.example.invalid/v1\"\n",
                "deepseek-chat",
            ),
        ];

        for (case, body, expected) in cases {
            let temp_root = temp_root("codewhale-model-chain-agreement");
            fs::create_dir_all(&temp_root).unwrap();
            let _guard = EnvGuard::new(&temp_root);
            let _model_guard = ModelEnvGuard::new();
            let path = temp_root.join(".deepseek").join("config.toml");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, body).unwrap();

            let tui = crate::config::Config::load(Some(path.clone()), None)
                .expect("the TUI must parse this config");
            let tui_model = tui.default_model();

            let dispatcher = codewhale_config::ConfigStore::load(Some(path.clone()))
                .expect("the dispatcher must parse the same config");
            let runtime = dispatcher
                .config
                .resolve_runtime_options(&codewhale_config::CliRuntimeOverrides::default());

            assert_eq!(
                tui_model, *expected,
                "{case}: the TUI chain (what actually reaches the provider) is wrong"
            );
            assert_eq!(
                runtime.model, *expected,
                "{case}: the dispatcher chain (what `model resolve` reports) is wrong"
            );

            let _ = fs::remove_dir_all(&temp_root);
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
