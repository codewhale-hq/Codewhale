//! Pure roster facts tied to existing Native entry receipts. No new profile,
//! session, filesystem root or writable catalog authority.
use super::{PluginRegistry, activation::PluginActivationCapability};
use crate::composition_scope::NativePresetRef;
use codewhale_config::EntryRef;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

const MARKER: &str = "// codewhale-native-preset-v1 ";
const MAX_ENTRY: u64 = 16 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePresetMetadata {
    pub id: String,
    pub trust: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub order: Option<f64>,
    pub is_default: bool,
    pub broken: Option<String>,
}

pub fn metadata_from_bytes(bytes: &[u8]) -> Option<NativePresetMetadata> {
    let source = std::str::from_utf8(bytes).ok()?;
    let first = source.lines().next()?.strip_prefix(MARKER)?;
    let data: NativePresetMetadata = serde_json::from_str(first).ok()?;
    if data.id.is_empty()
        || data.id.len() > 64
        || !data.id.as_bytes()[0].is_ascii_alphanumeric()
        || !data
            .id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        || !matches!(data.trust.as_str(), "system" | "user")
        || data.name.as_ref().is_some_and(|v| v.len() > 4096)
        || data
            .description
            .as_ref()
            .is_some_and(|v| v.len() > 16 * 1024)
        || data.order.is_some_and(|v| !v.is_finite())
        || data.broken.is_some()
    {
        return None;
    }
    Some(data)
}

pub fn metadata(
    plugins: &PluginRegistry,
    preset: &NativePresetRef,
) -> Option<NativePresetMetadata> {
    let plugin = plugins.get(&preset.plugin_id)?;
    if plugin.content_hash != preset.content_hash
        || !plugin.component_active(PluginActivationCapability::Native)
    {
        return None;
    }
    let root = plugin.staged_root.as_deref()?;
    let path = Path::new(&preset.entry.path);
    // Reuse the sole discovery-to-stage translator: it resolves both the
    // root and entry, proves confinement and requires an actual Native entry.
    if !plugin.components.native.iter().any(|source| {
        super::runtime::staged_component_path(&plugin.canonical_root, root, source)
            .is_ok_and(|admitted| admitted == path)
    }) {
        return None;
    }
    let mut file = codewhale_util::bundle_file::open_bundle_file(path).ok()?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_ENTRY + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_ENTRY as usize
        || codewhale_runtime::hashing::sha256_hex(&bytes) != preset.entry.sha256
    {
        return None;
    }
    metadata_from_bytes(&bytes)
}

fn reviewed_entry(
    source: &super::runtime::PluginComponentSource,
    checked: &mut BTreeMap<String, Option<super::manifest::ValidatedManifest>>,
) -> Option<NativePresetRef> {
    let id = source.authority.plugin_id.to_string();
    let receipt = checked.entry(id.clone()).or_insert_with(|| {
        let validated =
            super::manifest::PluginManifest::validate_from_path(&source.authority.staged_manifest)
                .ok()?;
        (validated.content_hash == source.authority.content_hash
            && validated.capability_hash == source.authority.capability_hash)
            .then_some(validated)
    });
    let receipt = receipt.as_ref()?;
    // Component paths and the validated receipt share canonical identity;
    // the selected manifest's lexical alias is not a containment root.
    let relative = source.path.strip_prefix(&receipt.canonical_root).ok()?;
    Some(NativePresetRef {
        plugin_id: id,
        content_hash: source.authority.content_hash.clone(),
        entry: EntryRef {
            path: source.path.to_string_lossy().into_owned(),
            sha256: receipt.native_entry_hashes.get(relative)?.clone(),
        },
    })
}

/// New roster choices come from a whole admitted bundle validation, not a
/// fresh hash of an unmounted mutable file mistaken for a reviewed receipt.
pub fn admitted_entries(
    plugins: &PluginRegistry,
) -> Vec<(
    NativePresetRef,
    NativePresetMetadata,
    codewhale_config::PluginAuthority,
)> {
    let (sources, _) =
        super::runtime::active_component_sources(plugins, PluginActivationCapability::Native);
    let mut checked = BTreeMap::new();
    let mut result = Vec::new();
    for source in sources {
        // Ordinary Native modules have no catalog metadata; do not rehash
        // their whole bundles just to rediscover their existing roster view.
        // active_component_sources already validated the authority and used
        // the shared translator to prove this canonical entry is confined.
        let Ok(mut file) = codewhale_util::bundle_file::open_bundle_file(&source.path) else {
            continue;
        };
        let mut header = Vec::new();
        if file
            .by_ref()
            .take(MAX_ENTRY + 1)
            .read_to_end(&mut header)
            .is_err()
            || header.len() > MAX_ENTRY as usize
            || metadata_from_bytes(&header).is_none()
        {
            continue;
        }
        let Some(preset) = reviewed_entry(&source, &mut checked) else {
            continue;
        };
        if let Some(data) = metadata(plugins, &preset) {
            result.push((preset, data, source.authority));
        }
    }
    result
}

/// One caller snapshot: each raw catalog chooses one default; ordinary Native
/// plugins keep their existing complete entry set. The manager may own a union
/// across callers but no caller receives a union of that catalog's presets.
pub fn default_selection(plugins: &PluginRegistry) -> (Vec<NativePresetRef>, BTreeSet<String>) {
    let (sources, _) =
        super::runtime::active_component_sources(plugins, PluginActivationCapability::Native);
    let admitted = admitted_entries(plugins);
    if admitted.is_empty() {
        return (Vec::new(), BTreeSet::new());
    }
    let mut normal = Vec::new();
    let mut catalogs: BTreeMap<String, Vec<(NativePresetRef, NativePresetMetadata)>> =
        BTreeMap::new();
    for (preset, data, _) in admitted {
        catalogs
            .entry(preset.plugin_id.clone())
            .or_default()
            .push((preset, data));
    }
    let mut checked = BTreeMap::new();
    for source in sources {
        if catalogs.contains_key(source.authority.plugin_id.as_str()) {
            continue;
        }
        if let Some(entry) = reviewed_entry(&source, &mut checked) {
            normal.push(entry);
        }
    }
    if catalogs.is_empty() {
        return (Vec::new(), BTreeSet::new());
    }
    let mut unselected = BTreeSet::new();
    for (id, rows) in catalogs {
        if let Some((preset, _)) = rows.into_iter().find(|(_, data)| data.is_default) {
            normal.push(preset);
        } else {
            // Absence is upstream data, never permission to mount the first row.
            unselected.insert(id);
        }
    }
    (normal, unselected)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_preset_metadata_is_data_and_cannot_mint_trust_or_an_entry() {
        let good = br#"// codewhale-native-preset-v1 {"id":"reviewer","trust":"user","name":"Repo reviewer","description":"bounded","is_default":true}
export function apply() {}"#;
        assert_eq!(metadata_from_bytes(good).unwrap().id, "reviewer");
        for source in [
            r#"{"id":"../escape","trust":"user","is_default":true}"#,
            r#"{"id":"ok","trust":"admin","is_default":true}"#,
            r#"{"id":"ok","trust":"user","is_default":true,"entry":{"path":"outside"}}"#,
            r#"{"id":"ok","trust":"user","is_default":true,"broken":"missing"}"#,
        ] {
            assert!(metadata_from_bytes(format!("{MARKER}{source}\n").as_bytes()).is_none());
        }
    }
}
