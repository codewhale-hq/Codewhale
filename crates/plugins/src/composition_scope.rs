//! Caller snapshots in AttachmentState. No second registry, store or owner token.
use codewhale_config::EntryRef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionRevision {
    pub attachment_id: u64,
    pub revision: u64,
}

/// A core-selected entry from the owner's existing reviewed Native inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePresetRef {
    pub plugin_id: String,
    pub content_hash: String,
    pub entry: EntryRef,
}

#[derive(Debug, Clone, Default)]
pub struct CompositionSelection {
    pub revision: Option<SelectionRevision>,
    pub desired: BTreeMap<String, String>,
    pub entries: Vec<NativePresetRef>,
}
impl CompositionSelection {
    pub fn includes(&self, plugin_id: &str, content_hash: &str, scope: Option<&EntryRef>) -> bool {
        self.desired
            .get(plugin_id)
            .is_some_and(|hash| hash == content_hash)
            && scope.is_none_or(|scope| {
                self.entries.iter().any(|entry| {
                    entry.plugin_id == plugin_id
                        && entry.content_hash == content_hash
                        && entry.entry == *scope
                })
            })
    }
}

/// Separate scopes may spell a name alike; core names remain fenced unconditionally.
pub fn name_key(owner: &str, name: &str, scope: Option<&EntryRef>) -> String {
    match scope {
        None => name.to_string(),
        Some(scope) => format!("{owner}\0{}\0{}\0{name}", scope.path, scope.sha256),
    }
}
