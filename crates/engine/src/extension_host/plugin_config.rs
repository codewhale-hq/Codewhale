//! Plugin configuration: what a plugin's `apply(ctx, config)` receives.
//!
//! The source is `[plugins."<name>".config]` in the user's `config.toml`
//! ([`codewhale_config::extension_host::PluginSettings`]); there was no per-plugin configuration
//! before this, so nothing is replaced. It is *user* input, not reviewed
//! plugin content: the user owns it and may edit it without a new review, which
//! is why project-scope config cannot set it and why it is delivered as data
//! only (the plugin's own `Config` schema, when it declares one, validates it
//! inside the host; Cordis applies that schema and its defaults).
//!
//! The manager holds the parsed settings. A change (read again at
//! `/plugin reload`) changes the config's digest, which reconcile compares with
//! the one each live owner was activated with: a different digest revokes the
//! owner and activates a new generation, like a changed plugin.
//!
//! Known limitations:
//! * Values are plain TOML (strings, numbers, booleans, arrays, tables); a
//!   TOML date-time is refused. There are no secrets references: do not put a
//!   secret here, because the plugin's code can read every value, and so can
//!   every other plugin sharing the host process.
//! * Only a manual `/plugin reload` re-reads the file; a running TUI does not
//!   watch it. A reload that cannot read or parse the file keeps the previous
//!   settings and says so in the host diagnostics.
//! * A refused config (too large, wrong shape) fails that plugin's activation
//!   with the reason, in `/plugin show`, rather than activating it with
//!   defaults.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;
use sha2::{Digest, Sha256};

use codewhale_config::extension_host::PluginSettings;

/// Largest accepted config for one plugin, serialized as JSON.
pub const MAX_PLUGIN_CONFIG_BYTES: usize = 16 * 1024;
/// Deepest nesting of tables and arrays accepted.
const MAX_CONFIG_DEPTH: usize = 16;
/// How the `toml` crate serializes a date-time (a one-key table).
const TOML_DATETIME_KEY: &str = "$__toml_private_datetime";

/// The config one activation is given, and its digest.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginConfig {
    /// Always a JSON object.
    pub value: Value,
    /// SHA-256 of `value`'s compact JSON, hex.
    pub hash: String,
}

impl PluginConfig {
    fn new(value: Value) -> Self {
        let hash = super::hex(Sha256::digest(
            serde_json::to_vec(&value).expect("a JSON value serializes"),
        ));
        Self { value, hash }
    }

    /// What a plugin with no settings is given (and what a built-in module is).
    pub fn empty() -> Self {
        Self::new(Value::Object(serde_json::Map::new()))
    }
}

/// Settings for every plugin by manifest name, and where they were read from.
#[derive(Default)]
pub struct PluginConfigs {
    by_name: BTreeMap<String, Result<PluginConfig, String>>,
    /// The user config file `/plugin reload` reads `[plugins]` from again.
    source: Option<PathBuf>,
}

fn depth_ok(value: &Value, depth: usize) -> bool {
    depth <= MAX_CONFIG_DEPTH
        && match value {
            Value::Array(items) => items.iter().all(|item| depth_ok(item, depth + 1)),
            Value::Object(map) => map.values().all(|item| depth_ok(item, depth + 1)),
            _ => true,
        }
}

fn has_datetime(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(has_datetime),
        Value::Object(map) => map.contains_key(TOML_DATETIME_KEY) || map.values().any(has_datetime),
        _ => false,
    }
}

/// One plugin's `[plugins."<name>".config]`, checked.
fn convert(name: &str, settings: &PluginSettings) -> Result<PluginConfig, String> {
    let Some(table) = &settings.config else {
        return Ok(PluginConfig::empty());
    };
    let value = serde_json::to_value(table)
        .map_err(|error| format!("plugin `{name}` config is not representable as JSON: {error}"))?;
    if has_datetime(&value) {
        return Err(format!(
            "plugin `{name}` config holds a TOML date-time; use a string"
        ));
    }
    if !depth_ok(&value, 1) {
        return Err(format!(
            "plugin `{name}` config nests deeper than {MAX_CONFIG_DEPTH} levels"
        ));
    }
    let size = serde_json::to_vec(&value).map_or(usize::MAX, |bytes| bytes.len());
    if size > MAX_PLUGIN_CONFIG_BYTES {
        return Err(format!(
            "plugin `{name}` config is {size} bytes, over the {MAX_PLUGIN_CONFIG_BYTES}-byte limit"
        ));
    }
    Ok(PluginConfig::new(value))
}

impl PluginConfigs {
    /// Replace every plugin's settings. `source` (when given) is where a
    /// later reload reads them from.
    pub fn replace(
        &mut self,
        settings: &BTreeMap<String, PluginSettings>,
        source: Option<PathBuf>,
    ) {
        self.by_name = settings
            .iter()
            .map(|(name, settings)| (name.clone(), convert(name, settings)))
            .collect();
        if source.is_some() {
            self.source = source;
        }
    }

    /// Replace the settings read again from [`Self::source`]'s file.
    pub fn replace_reloaded(&mut self, settings: &BTreeMap<String, PluginSettings>) {
        self.replace(settings, None);
    }

    /// The file to re-read, if one was named at boot.
    pub fn source(&self) -> Option<PathBuf> {
        self.source.clone()
    }

    /// What `name` is activated with, or why it must not be.
    pub fn select(&self, name: &str) -> Result<PluginConfig, String> {
        self.by_name
            .get(name)
            .cloned()
            .unwrap_or_else(|| Ok(PluginConfig::empty()))
    }

    /// The top-level keys configured for `name` (never the values), or the
    /// reason its config is refused. `None` when it has no settings.
    pub fn summary(&self, name: &str) -> Option<Result<Vec<String>, String>> {
        match self.by_name.get(name)? {
            Ok(config) => {
                let keys: Vec<String> = config
                    .value
                    .as_object()
                    .map(|map| map.keys().cloned().collect())
                    .unwrap_or_default();
                (!keys.is_empty()).then_some(Ok(keys))
            }
            Err(reason) => Some(Err(reason.clone())),
        }
    }
}

/// A digest stable across reloads for the activation compare: the config's
/// own digest, or, for a refused config, one derived from the reason so the
/// refused owner is neither retried every turn nor kept once the file changes.
pub fn activation_hash(selection: &Result<PluginConfig, String>) -> String {
    match selection {
        Ok(config) => config.hash.clone(),
        Err(reason) => format!("refused:{}", super::hex(Sha256::digest(reason.as_bytes()))),
    }
}
