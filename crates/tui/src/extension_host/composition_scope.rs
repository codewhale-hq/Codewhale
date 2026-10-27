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
pub(super) fn name_key(owner: &str, name: &str, scope: Option<&EntryRef>) -> String {
    match scope {
        None => name.to_string(),
        Some(scope) => format!("{owner}\0{}\0{}\0{name}", scope.path, scope.sha256),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_host::protocol::{RegisterKind, RegisterParams, RegisterSpecWire};
    use crate::extension_host::registry::{MAX_TOOLS_PER_OWNER, OwnerRegistry};
    use crate::extension_host::tests::fake_authority;
    use crate::extension_host::tier::HostTier;
    use crate::extension_host::{ExtensionHostManager, ExtensionHostOptions};
    use crate::plugins::PluginRegistry;
    use codewhale_config::OwnerRef;
    use std::sync::Arc;
    fn scope(name: &str) -> EntryRef {
        EntryRef {
            path: format!("/reviewed/{name}.mjs"),
            sha256: name.repeat(64),
        }
    }
    fn tool(owner: &OwnerRef, scope: Option<EntryRef>, name: &str) -> RegisterParams {
        RegisterParams {
            owner: owner.clone(),
            scope,
            kind: RegisterKind::Tool,
            spec: RegisterSpecWire {
                name: name.into(),
                description: "scoped fixture".into(),
                input_schema: Some(
                    serde_json::json!({"type":"object","properties":{}})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                argument_hint: None,
            },
        }
    }
    fn owner(registry: &mut OwnerRegistry) -> OwnerRef {
        registry
            .begin_owner(
                HostTier::Plugin,
                "local/scoped",
                "scoped",
                Some(fake_authority("local/scoped")),
                "build",
            )
            .unwrap()
    }

    #[test]
    fn scoped_name_identity_includes_owner_even_for_identical_staged_entry_paths() {
        let entry = scope("a");
        assert_ne!(
            name_key("workspace-a", "echo", Some(&entry)),
            name_key("workspace-b", "echo", Some(&entry))
        );
        assert_eq!(name_key("a", "core", None), name_key("b", "core", None));
    }
    #[test]
    fn composition_selection_requires_exact_owner_build_and_entry() {
        let a = scope("a");
        let b = scope("b");
        let selected = CompositionSelection {
            revision: Some(SelectionRevision {
                attachment_id: 1,
                revision: 2,
            }),
            desired: BTreeMap::from([("owner".into(), "build".into())]),
            entries: vec![NativePresetRef {
                plugin_id: "owner".into(),
                content_hash: "build".into(),
                entry: a.clone(),
            }],
        };
        assert!(selected.includes("owner", "build", Some(&a)));
        assert!(!selected.includes("owner", "build", Some(&b)));
        assert!(!selected.includes("foreign", "build", Some(&a)));
        assert!(!selected.includes("owner", "changed", Some(&a)));
    }
    #[test]
    fn scoped_equal_names_keep_distinct_handles_and_withdraw_exact_entry() {
        let mut registry = OwnerRegistry::default();
        let owner = owner(&mut registry);
        let a = scope("a");
        let b = scope("b");
        registry.begin_scope(&owner, a.clone()).unwrap();
        registry.begin_scope(&owner, b.clone()).unwrap();
        let first = registry
            .register(&tool(&owner, Some(a.clone()), "scoped_echo"))
            .unwrap();
        let second = registry
            .register(&tool(&owner, Some(b.clone()), "scoped_echo"))
            .unwrap();
        assert_ne!(first, second);
        assert!(registry.mark_scope_active(&owner, &a));
        assert!(registry.mark_scope_active(&owner, &b));
        assert!(registry.mark_active(&owner));
        assert_eq!(registry.live_tools().len(), 2);
        assert!(
            registry
                .register(&tool(&owner, None, "unscoped_escape"))
                .is_err()
        );
        let retired = registry.revoke_scope(&owner, &a);
        assert_eq!(retired, [first]);
        assert!(!registry.is_live(first, &owner));
        assert!(registry.is_live(second, &owner));
        registry.host_exited(HostTier::Builtin, "fixture");
        assert!(registry.is_live(second, &owner));
        registry.host_exited(HostTier::Plugin, "fixture");
        assert!(registry.live_tools().is_empty());
        assert!(registry.owner(&owner.plugin_id).is_none());
    }
    #[test]
    fn scoped_aggregate_owner_limit_cannot_be_multiplied_by_entries() {
        let mut registry = OwnerRegistry::default();
        let owner = owner(&mut registry);
        let a = scope("a");
        let b = scope("b");
        registry.begin_scope(&owner, a.clone()).unwrap();
        registry.begin_scope(&owner, b.clone()).unwrap();
        for index in 0..MAX_TOOLS_PER_OWNER {
            registry
                .register(&tool(
                    &owner,
                    Some(if index % 2 == 0 { a.clone() } else { b.clone() }),
                    &format!("scoped_{index}"),
                ))
                .unwrap();
        }
        assert!(
            registry
                .register(&tool(&owner, Some(b), "one_too_many"))
                .unwrap_err()
                .contains("at most")
        );
    }
    #[test]
    fn caller_revision_withdrawal_does_not_revoke_another_attachment() {
        let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions::default()));
        let one = manager.attach(Arc::new(PluginRegistry::empty(std::path::Path::new(
            "/workspace",
        ))));
        let two = manager.attach(Arc::new(PluginRegistry::empty(std::path::Path::new(
            "/workspace",
        ))));
        let entry = scope("a");
        let selected = NativePresetRef {
            plugin_id: "owner".into(),
            content_hash: "build".into(),
            entry: entry.clone(),
        };
        {
            let mut attachments = manager.shared.attachments.lock().unwrap();
            for state in attachments.values_mut() {
                state.selection = CompositionSelection {
                    revision: state.plugins.caller_selection(),
                    desired: BTreeMap::from([("owner".into(), "build".into())]),
                    entries: vec![selected.clone()],
                };
            }
        }
        let old = one.plugin_view();
        let other = two.plugin_view();
        assert!(
            manager
                .shared
                .check_selection(
                    old.caller_selection(),
                    Some(&old),
                    "owner",
                    "build",
                    Some(&entry)
                )
                .is_ok()
        );
        assert!(
            manager
                .shared
                .check_selection(
                    old.caller_selection(),
                    Some(&other),
                    "owner",
                    "build",
                    Some(&entry)
                )
                .is_err()
        );
        one.set_plugins(Arc::new(PluginRegistry::empty(std::path::Path::new(
            "/workspace",
        ))));
        assert!(
            manager
                .shared
                .check_selection(
                    old.caller_selection(),
                    Some(&old),
                    "owner",
                    "build",
                    Some(&entry)
                )
                .is_err()
        );
        assert!(
            manager
                .shared
                .check_selection(
                    other.caller_selection(),
                    Some(&other),
                    "owner",
                    "build",
                    Some(&entry)
                )
                .is_ok()
        );
    }
    #[test]
    fn failed_scope_requires_reload_and_explicit_null_cannot_erase_scope() {
        let mut registry = OwnerRegistry::default();
        let owner = owner(&mut registry);
        let a = scope("a");
        registry.begin_scope(&owner, a.clone()).unwrap();
        registry.fail_scope(&owner, &a);
        assert!(registry.begin_scope(&owner, a.clone()).is_err());
        registry.forget_inactive();
        assert!(registry.begin_scope(&owner, a.clone()).is_ok());
        let mut wire = serde_json::to_value(tool(&owner, Some(a), "scoped_fixture")).unwrap();
        wire["scope"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<RegisterParams>(wire).is_err());
    }
}
