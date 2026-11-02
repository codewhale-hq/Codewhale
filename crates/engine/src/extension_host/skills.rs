//! Owned reviewed roots feed the existing SkillRegistry; no parser or store here.
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use codewhale_skills::NativeSkillRef;

use super::ManagerShared;
use super::protocol::{RegisterParams, RegisterResult};
use super::supervisor::HostRequestContext;
use super::tier::HostTier;
use codewhale_config::OwnerRef;
use codewhale_config::PluginAuthority;
use codewhale_plugins::activation::PluginActivationCapability;
use codewhale_plugins::types::PluginSkillSnapshot;

pub use codewhale_plugins::discovery::{MAX_BYTES_PER_OWNER, MAX_SKILLS_PER_OWNER};
pub const MAX_ROOTS_PER_OWNER: usize = 8;
pub const MAX_ROOTS_PER_HOST: usize = 64;
pub const MAX_SKILLS_PER_HOST: usize = 1024;
pub const MAX_BYTES_PER_HOST: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct SkillRootRegistration {
    pub handle: u64,
    pub owner: OwnerRef,
    pub scope: Option<codewhale_config::EntryRef>,
    pub host_generation: u64,
    pub content_hash: String,
    pub path: String,
    pub snapshots: Vec<PluginSkillSnapshot>,
    pub bytes: usize,
}

pub fn root_path(path: &str) -> Result<PathBuf, String> {
    if path.is_empty()
        || path.len() > 512
        || path
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':'))
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        || Path::new(path)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("skill root must be a bounded bundle-relative path with normal slash-separated components".to_string());
    }
    Ok(PathBuf::from(path))
}

pub fn snapshot_bytes(skill: &PluginSkillSnapshot) -> usize {
    skill.name.len()
        + skill.description.len()
        + skill.body.len()
        + skill.source_hash.len()
        + skill.legacy_activation_name.as_ref().map_or(0, String::len)
        + skill.argument_hint.as_ref().map_or(0, String::len)
        + skill.aliases.iter().map(String::len).sum::<usize>()
        + skill
            .localized_descriptions
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
}

/// The existing asynchronous request seam performs disk work outside every
/// registry lock; final admission rechecks cancellation and exact lifetimes.
pub(super) async fn admit_root(
    shared: &Arc<ManagerShared>,
    tier: HostTier,
    host_generation: u64,
    params: RegisterParams,
    cx: &HostRequestContext,
) -> RegisterResult {
    let result = async {
        params.check_spec()?;
        if !params.spec.description.is_empty() {
            return Err("skill root has no description".to_string());
        }
        let relative = root_path(&params.spec.name)?;
        if tier != HostTier::Plugin {
            return Err("skill roots require a reviewed plugin bundle".to_string());
        }
        let authority = shared
            .live_owner_authority(tier, |registry| {
                registry
                    .authority_for(&params.owner)
                    .ok_or_else(|| "stale or unknown skill owner".to_string())?;
                Ok(params.owner.clone())
            })?
            .ok_or_else(|| "skill owner has no reviewed authority".to_string())?;
        let snapshots =
            bounded_review_check(Arc::clone(&shared.skill_admission), &cx.cancel, move || {
                // One permit covers every disk check, including the Native receipt
                // phase; cancellation never leaves that phase outside the bound.
                codewhale_plugins::registry::verify_plugin_component_authority(
                    &authority,
                    PluginActivationCapability::Native,
                )?;
                let snapshots = codewhale_plugins::discovery::load_staged_skill_root_snapshots(
                    &authority, &relative,
                )?;
                codewhale_plugins::registry::verify_plugin_component_authority(
                    &authority,
                    PluginActivationCapability::Native,
                )?;
                Ok(snapshots)
            })
            .await?;
        shared
            .ready_host(tier)
            .map_err(|status| status.to_string())?;
        let runtime = shared.tier_runtime(tier);
        let _slot = runtime.host.lock().expect("host lock");
        if runtime.host_generation.load(Ordering::SeqCst) != host_generation
            || cx.cancel.is_cancelled()
        {
            return Err(
                "skill root host generation changed or admission was cancelled".to_string(),
            );
        }
        shared
            .registry
            .lock()
            .expect("registry lock")
            .register_skill_root(&params, snapshots, host_generation)
    }
    .await;
    match result {
        Ok(handle) => RegisterResult::Admitted { handle },
        Err(refused) => {
            shared.plugin_diagnostic(
                &params.owner.plugin_id,
                format!("skill root refused: {refused}"),
            );
            RegisterResult::Refused { refused }
        }
    }
}

/// Every Native root or MCP disk phase uses this job. Its permit belongs to the
/// blocking closure, so RPC cancellation or abandonment cannot release it
/// until receipt validation and parsing actually stop.
pub(super) async fn bounded_review_check<T: Send + 'static>(
    admission: Arc<tokio::sync::Semaphore>,
    cancel: &tokio_util::sync::CancellationToken,
    check: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    if cancel.is_cancelled() {
        return Err("Native review admission cancelled".to_string());
    }
    let permit = tokio::select! {
        _ = cancel.cancelled() => return Err("Native review admission cancelled".to_string()),
        permit = admission.acquire_owned() => permit.map_err(|_| "Native review admission is unavailable".to_string())?,
    };
    if cancel.is_cancelled() {
        return Err("Native review admission cancelled".to_string());
    }
    let policy = super::activation::extension_host_policy_enabled();
    #[cfg(any(test, feature = "test-support"))]
    let env_scope = crate::test_support::env_scope_ticket();
    let work = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        #[cfg(any(test, feature = "test-support"))]
        let _env_scope = crate::test_support::join_env_scope(env_scope);
        let _policy = codewhale_plugins::activation::PolicyScope::propagate(policy);
        check()
    });
    tokio::select! {
        _ = cancel.cancelled() => Err("Native review admission cancelled".to_string()),
        result = work => result.map_err(|error| format!("Native review check failed: {error}"))?,
    }
}

fn live(
    manager: &super::ExtensionHostManager,
    authority: &PluginAuthority,
    reference: &NativeSkillRef,
) -> Result<(), String> {
    if reference.boot_id != crate::session_manager::current_session_boot_id() {
        return Err(
            "native skill belongs to an earlier Codewhale process; select the skill again"
                .to_string(),
        );
    }
    if !super::activation::extension_host_policy_enabled() {
        return Err("extension host is disabled".to_string());
    }
    let host = manager
        .shared
        .ready_host(HostTier::Plugin)
        .map_err(|status| status.to_string())?;
    if host.generation != reference.host_generation {
        return Err("native skill host restarted; select the skill again".to_string());
    }
    let registry = manager.shared.registry.lock().expect("registry lock");
    if !registry.is_live_skill_root(
        reference.handle,
        authority.plugin_id.as_str(),
        reference.owner_generation,
        reference.host_generation,
        &authority.content_hash,
        authority.state_generation,
    ) {
        return Err(
            "native skill registration was disposed, revoked or replaced; select the skill again"
                .to_string(),
        );
    }
    Ok(())
}

pub fn verify_native_skill(
    authority: &PluginAuthority,
    reference: &NativeSkillRef,
) -> Result<(), String> {
    let manager = super::manager();
    if reference.scope.is_some()
        && !reference.selection.is_some_and(|selected| {
            manager.shared.selection_current(
                selected,
                authority.plugin_id.as_str(),
                &authority.content_hash,
                reference.scope.as_ref(),
            )
        })
    {
        return Err("Native skill is no longer selected".into());
    }
    live(&manager, authority, reference)?;
    codewhale_plugins::registry::verify_plugin_component_authority(
        authority,
        PluginActivationCapability::Native,
    )?;
    // Full receipt validation does disk I/O; do not accept a registration
    // removed while that check was in progress.
    live(&manager, authority, reference)?;
    if reference.scope.is_some()
        && !reference.selection.is_some_and(|selected| {
            manager.shared.selection_current(
                selected,
                authority.plugin_id.as_str(),
                &authority.content_hash,
                reference.scope.as_ref(),
            )
        })
    {
        return Err("Native skill selection changed during verification".into());
    }
    Ok(())
}

/// Caller-snapshot scope, rather than the first workspace that activated an
/// owner. All normal discovery consumers use this one existing catalog merge.
pub fn roots_for_plugins(
    plugins: &codewhale_plugins::PluginRegistry,
) -> Vec<(SkillRootRegistration, PluginAuthority, NativeSkillRef)> {
    if !super::activation::extension_host_policy_enabled() {
        return Vec::new();
    }
    if plugins.state_path().is_none() {
        return Vec::new();
    }
    let manager = super::manager();
    let roots = manager
        .shared
        .registry
        .lock()
        .expect("registry lock")
        .live_skill_roots();
    let mut selected: Vec<_> = roots
        .into_iter()
        .filter_map(|root| {
            let plugin = plugins.get(&root.owner.plugin_id)?;
            if !plugin.component_active(PluginActivationCapability::Native)
                || plugin.content_hash != root.content_hash
            {
                return None;
            }
            let authority = plugins.authority_for(plugin.id.as_str())?;
            let selected = plugins.caller_selection();
            if root.scope.is_some()
                && !selected.is_some_and(|selection| {
                    manager.shared.selection_current(
                        selection,
                        &root.owner.plugin_id,
                        &root.content_hash,
                        root.scope.as_ref(),
                    )
                })
            {
                return None;
            }
            let reference = NativeSkillRef {
                boot_id: crate::session_manager::current_session_boot_id().to_string(),
                host_generation: root.host_generation,
                owner_generation: root.owner.generation,
                handle: root.handle,
                selection: selected,
                scope: root.scope.clone(),
            };
            verify_native_skill(&authority, &reference).ok()?;
            Some((root, authority, reference))
        })
        .collect();
    let mut counts = std::collections::BTreeMap::new();
    for (root, _, _) in &selected {
        for skill in &root.snapshots {
            *counts
                .entry((root.owner.plugin_id.clone(), skill.name.clone()))
                .or_insert(0usize) += 1;
        }
    }
    for (root, _, _) in &mut selected {
        root.snapshots
            .retain(|skill| counts[&(root.owner.plugin_id.clone(), skill.name.clone())] == 1);
    }
    selected.retain(|(root, _, _)| !root.snapshots.is_empty());
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_host::protocol::{RegisterKind, RegisterSpecWire};
    use crate::extension_host::registry::OwnerRegistry;
    use crate::extension_host::test_support::{FixturePlugins, fake_authority, node_for_tests};
    use crate::tools::spec::{ToolContext, ToolSpec};
    use codewhale_plugins::activation::TestPolicyGuard;
    use codewhale_skill_install::skill_parse::SkillInvocation;
    use codewhale_skills::{SkillDiscoveryMode, SkillProvenance};
    use std::collections::HashMap;

    fn params(owner: &OwnerRef, path: &str) -> RegisterParams {
        RegisterParams {
            scope: None,
            owner: owner.clone(),
            kind: RegisterKind::SkillRoot,
            spec: RegisterSpecWire {
                name: path.to_string(),
                description: String::new(),
                input_schema: None,
                argument_hint: None,
            },
        }
    }
    fn owner(registry: &mut OwnerRegistry, id: &str) -> OwnerRef {
        registry
            .begin_owner(
                HostTier::Plugin,
                id,
                id,
                Some(fake_authority(id)),
                &format!("hash-{id}"),
            )
            .unwrap()
    }
    fn snapshot(name: &str, body: String) -> PluginSkillSnapshot {
        PluginSkillSnapshot {
            name: name.to_string(),
            legacy_activation_name: None,
            description: "focused fixture".to_string(),
            localized_descriptions: HashMap::new(),
            invocation: SkillInvocation::ModelAndUser,
            aliases: Vec::new(),
            argument_hint: None,
            body,
            path: PathBuf::from(format!("/staged/skills/{name}/SKILL.md")),
            source_hash: "0".repeat(64),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_native_root_disk_job_keeps_admission_permit_until_completion() {
        // Exercise the production helper for both cooperative cancellation and
        // a channel abandoning its handler. A blocked first disk phase cannot
        // release its permit or start another queued check in either case.
        for abandon_handler in [false, true] {
            let admission = Arc::new(tokio::sync::Semaphore::new(1));
            let cancel = tokio_util::sync::CancellationToken::new();
            let (entered, started) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let running_admission = Arc::clone(&admission);
            let running_cancel = cancel.clone();
            let running = tokio::spawn(async move {
                bounded_review_check(running_admission, &running_cancel, move || {
                    let _ = entered.send(());
                    wait.recv().map_err(|error| error.to_string())?;
                    Ok(7)
                })
                .await
            });
            started.await.unwrap();
            if abandon_handler {
                running.abort();
                assert!(running.await.unwrap_err().is_cancelled());
            } else {
                cancel.cancel();
                assert!(running.await.unwrap().unwrap_err().contains("cancelled"));
            }
            assert_eq!(
                admission.available_permits(),
                0,
                "cancelled handler must not release the live disk job's permit"
            );
            let queued_cancel = tokio_util::sync::CancellationToken::new();
            let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let queued_ran = Arc::clone(&ran);
            let queued_admission = Arc::clone(&admission);
            let queued_token = queued_cancel.clone();
            let queued = tokio::spawn(async move {
                bounded_review_check(queued_admission, &queued_token, move || {
                    queued_ran.store(true, Ordering::SeqCst);
                    Ok(9)
                })
                .await
            });
            tokio::task::yield_now().await;
            queued_cancel.cancel();
            assert!(queued.await.unwrap().is_err());
            assert!(
                !ran.load(Ordering::SeqCst),
                "a cancelled queued check never enters the blocking pool"
            );
            release.send(()).unwrap();
            let resumed = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                bounded_review_check(
                    Arc::clone(&admission),
                    &tokio_util::sync::CancellationToken::new(),
                    || Ok(11),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                resumed, 11,
                "completed disk work releases admission for the next live owner"
            );
            assert_eq!(admission.available_permits(), 1);
        }
    }

    #[test]
    fn skill_root_paths_are_bounded_and_bundle_relative() {
        assert!(root_path("profiles/review-skills").is_ok());
        for path in [
            "",
            "/absolute",
            "../outside",
            "./same",
            "a/../b",
            "a//b",
            "a/",
            "C:/disk",
            "a\\b",
            "a\u{1b}b",
        ] {
            assert!(root_path(path).is_err(), "{path:?}");
        }
        assert!(root_path(&"界".repeat(171)).is_err());
    }

    #[test]
    fn skill_root_withdrawal_is_exact_and_owner_generation_is_not_reusable() {
        let mut registry = OwnerRegistry::new();
        let a = owner(&mut registry, "a");
        let b = owner(&mut registry, "b");
        let handle = registry
            .register_skill_root(
                &params(&a, "skills"),
                vec![snapshot("one", "body".into())],
                7,
            )
            .unwrap();
        assert!(registry.live_skill_roots().is_empty());
        registry.mark_active(&a);
        assert!(registry.is_live_skill_root(handle, "a", a.generation, 7, "hash-a", 1));
        registry.unregister(&b, handle);
        assert_eq!(registry.live_skill_roots().len(), 1);
        registry.unregister(&a, handle);
        assert!(registry.live_skill_roots().is_empty());
        let next = registry
            .register_skill_root(
                &params(&a, "skills"),
                vec![snapshot("one", "body".into())],
                7,
            )
            .unwrap();
        assert_ne!(handle, next);
        registry.revoke_owner("a");
        let replacement = owner(&mut registry, "a");
        registry.mark_active(&replacement);
        assert!(!registry.is_live_skill_root(next, "a", a.generation, 7, "hash-a", 1));
        assert!(
            registry
                .register_skill_root(
                    &params(&a, "skills"),
                    vec![snapshot("one", "body".into())],
                    7
                )
                .is_err()
        );
    }

    #[test]
    fn skill_root_caps_retire_on_plugin_exit_and_preserve_other_tier() {
        let mut registry = OwnerRegistry::new();
        for index in 0..8 {
            let current = owner(&mut registry, &format!("p{index}"));
            for root in 0..8 {
                registry
                    .register_skill_root(
                        &params(&current, &format!("s{root}")),
                        vec![snapshot(&format!("skill{root}"), "body".into())],
                        7,
                    )
                    .unwrap();
            }
            registry.mark_active(&current);
        }
        let extra = owner(&mut registry, "extra");
        assert!(
            registry
                .register_skill_root(
                    &params(&extra, "skills"),
                    vec![snapshot("one", "body".into())],
                    7
                )
                .is_err()
        );
        registry.host_exited(HostTier::Builtin, "builtin crash");
        assert_eq!(registry.live_skill_roots().len(), MAX_ROOTS_PER_HOST);
        registry.host_exited(HostTier::Plugin, "plugin crash");
        assert!(registry.live_skill_roots().is_empty());
        let replacement = owner(&mut registry, "replacement");
        registry
            .register_skill_root(
                &params(&replacement, "skills"),
                vec![snapshot("one", "body".into())],
                8,
            )
            .unwrap();
        registry.revoke_all(HostTier::Plugin, "shutdown");
        let restarted = owner(&mut registry, "replacement");
        registry
            .register_skill_root(
                &params(&restarted, "skills"),
                vec![snapshot("one", "body".into())],
                9,
            )
            .unwrap();
    }

    #[test]
    fn skill_root_admission_enforces_owner_count_bytes_and_duplicate_names() {
        let mut registry = OwnerRegistry::new();
        let current = owner(&mut registry, "a");
        assert!(
            registry
                .register_skill_root(
                    &params(&current, "oversized"),
                    vec![snapshot("big", "x".repeat(MAX_BYTES_PER_OWNER))],
                    7
                )
                .is_err()
        );
        assert!(
            registry
                .register_skill_root(
                    &params(&current, "too-many"),
                    (0..129)
                        .map(|i| snapshot(&format!("s{i}"), "x".into()))
                        .collect(),
                    7
                )
                .is_err()
        );
        registry
            .register_skill_root(
                &params(&current, "first"),
                vec![snapshot("one", "x".into())],
                7,
            )
            .unwrap();
        assert!(
            registry
                .register_skill_root(
                    &params(&current, "second"),
                    vec![snapshot("one", "x".into())],
                    7
                )
                .is_err()
        );
        for i in 1..8 {
            registry
                .register_skill_root(
                    &params(&current, &format!("r{i}")),
                    vec![snapshot(&format!("s{i}"), "x".into())],
                    7,
                )
                .unwrap();
        }
        assert!(
            registry
                .register_skill_root(
                    &params(&current, "ninth"),
                    vec![snapshot("last", "x".into())],
                    7
                )
                .is_err()
        );
    }

    #[test]
    fn queued_skill_provenance_preserves_legacy_receipts_and_rejects_process_restart() {
        super::super::install_ports();
        let authority = fake_authority("a");
        let legacy = serde_json::to_string(&authority).unwrap();
        let parsed: SkillProvenance = serde_json::from_str(&legacy).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), legacy);
        assert!(parsed.authority().managed_policy_source.is_none());
        let mut old_receipt = serde_json::to_value(&authority).unwrap();
        old_receipt
            .as_object_mut()
            .unwrap()
            .remove("managed_policy_path");
        let old_receipt: SkillProvenance = serde_json::from_value(old_receipt).unwrap();
        assert!(old_receipt.authority().managed_policy_path.is_none());
        assert!(
            codewhale_plugins::registry::verify_plugin_state_authority(old_receipt.authority())
                .unwrap_err()
                .contains("managed policy source")
        );
        let provenance = SkillProvenance::NativeRoot(codewhale_skills::NativeSkillProvenance {
            authority: authority.clone(),
            registration: NativeSkillRef {
                selection: None,
                scope: None,
                boot_id: "earlier-process".into(),
                host_generation: 1,
                owner_generation: 1,
                handle: 1,
            },
        });
        let restored: SkillProvenance =
            serde_json::from_str(&serde_json::to_string(&provenance).unwrap()).unwrap();
        assert!(
            restored
                .verify(&authority.workspace)
                .unwrap_err()
                .contains("earlier Codewhale process")
        );
        assert!(
            restored
                .verify(Path::new("/other"))
                .unwrap_err()
                .contains("different workspace")
        );
    }

    fn parser_bundle(skills: usize, bytes: usize) -> (tempfile::TempDir, PluginAuthority) {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("plugin.json"), r#"{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"bounded-skills","version":"0.1.0","description":"bounded fixture","extensions":{"net.codewhale":{"native":{"path":"index.mjs"}}}}"#).unwrap();
        std::fs::write(
            temp.path().join("index.mjs"),
            "export function apply() {}\n",
        )
        .unwrap();
        for index in 0..skills {
            let directory = temp.path().join("skills").join(format!("s{index}"));
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join("SKILL.md"),
                format!(
                    "---\nname: s{index}\ndescription: bounded fixture\n---\n{}",
                    "x".repeat(bytes)
                ),
            )
            .unwrap();
        }
        let validated = codewhale_plugins::manifest::PluginManifest::validate_from_path(
            &temp.path().join("plugin.json"),
        )
        .unwrap();
        let mut authority = fake_authority("bounded-skills");
        authority.staged_manifest = validated.canonical_root.join("plugin.json");
        authority.content_hash = validated.content_hash;
        authority.capability_hash = validated.capability_hash;
        (temp, authority)
    }

    #[test]
    fn reviewed_skill_root_parser_bounds_accumulation_and_parent_claims() {
        let (_many, authority) = parser_bundle(129, 1);
        assert!(
            codewhale_plugins::discovery::load_staged_skill_root_snapshots(
                &authority,
                Path::new("skills")
            )
            .unwrap_err()
            .contains("candidate count")
        );
        let (_large, authority) = parser_bundle(8, 600 * 1024);
        assert!(
            codewhale_plugins::discovery::load_staged_skill_root_snapshots(
                &authority,
                Path::new("skills")
            )
            .unwrap_err()
            .contains("byte limit")
        );
        let (one, mut authority) = parser_bundle(1, 1);
        let nested = one.path().join("skills/s0/examples/nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join("SKILL.md"),
            "---\nname: nested\ndescription: nested fixture\n---\nbody",
        )
        .unwrap();
        let validated = codewhale_plugins::manifest::PluginManifest::validate_from_path(
            &authority.staged_manifest,
        )
        .unwrap();
        authority.content_hash = validated.content_hash;
        authority.capability_hash = validated.capability_hash;
        let snapshots = codewhale_plugins::discovery::load_staged_skill_root_snapshots(
            &authority,
            Path::new("skills"),
        )
        .unwrap();
        assert_eq!(
            snapshots
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["s0"]
        );
    }

    #[test]
    fn declared_and_native_reviewed_roots_share_nested_hidden_and_depth_semantics() {
        let (temp, _) = parser_bundle(1, 1);
        let skill_root = temp.path().join("skills");
        for (relative, name) in [
            ("vendor/organized", "organized"),
            ("s0/examples/nested", "nested"),
            (".hidden/hidden", "hidden"),
            ("a/b/c/d/e/f/g/h/deep8", "deep8"),
            ("a/b/c/d/e/f/g/h/i/deep9", "deep9"),
        ] {
            let directory = skill_root.join(relative);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: parity fixture\n---\n{name} body"),
            )
            .unwrap();
        }
        std::fs::remove_file(temp.path().join("plugin.json")).unwrap();
        let manifest = temp.path().join("plugin.toml");
        std::fs::write(&manifest, "schema_version = 1\n[plugin]\nname = \"bounded-skills\"\nversion = \"0.1.0\"\n[skills]\npath = \"skills\"\n[native]\npath = \"index.mjs\"\n").unwrap();
        let validated =
            codewhale_plugins::manifest::PluginManifest::validate_from_path(&manifest).unwrap();
        let declared = codewhale_plugins::discovery::load_staged_skill_snapshots(
            &validated.canonical_root,
            &validated.content_hash,
            &validated.capability_hash,
        )
        .unwrap();
        let mut authority = fake_authority("bounded-skills");
        authority.staged_manifest = validated.canonical_root.join("plugin.toml");
        authority.content_hash = validated.content_hash;
        authority.capability_hash = validated.capability_hash;
        let native = codewhale_plugins::discovery::load_staged_skill_root_snapshots(
            &authority,
            Path::new("skills"),
        )
        .unwrap();
        let baseline = codewhale_skills::SkillRegistry::discover(&skill_root);
        let projection = |snapshots: &[PluginSkillSnapshot]| {
            snapshots
                .iter()
                .map(|skill| (skill.name.clone(), skill.body.clone()))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let expected = baseline
            .list()
            .iter()
            .map(|skill| (skill.name.clone(), skill.body.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            expected.keys().map(String::as_str).collect::<Vec<_>>(),
            ["deep8", "organized", "s0"]
        );
        assert_eq!(projection(&declared), expected);
        assert_eq!(projection(&native), expected);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_only_root_reaches_shared_catalog_model_tool_and_queued_provenance() {
        let Some(node) = node_for_tests(
            "native_only_root_reaches_shared_catalog_model_tool_and_queued_provenance",
        ) else {
            return;
        };
        let _home = crate::test_support::SealedHome::new();
        let _policy = TestPolicyGuard::extension_host(true);
        let fixture = FixturePlugins::new(&["skills-root"]).await;
        let plugins = fixture.registry();
        let plugin = plugins.get("skills-root").unwrap();
        assert_eq!(plugin.inventory.skills, 0);
        assert!(
            plugin.skill_snapshots.is_empty(),
            "fixture has no declarative Skill adapter snapshots"
        );
        let manager = fixture.manager(node);
        let _manager = super::super::TestManagerGuard::install(Arc::clone(&manager));
        let engine = manager.attach(Arc::clone(&plugins));
        engine.sync().await.unwrap();
        let plugins = engine.plugin_view();
        let skills_dir = fixture.workspace().join("empty-skills");
        let catalog = codewhale_skills::discover_for_workspace_and_dir_with_mode_and_plugins(
            fixture.workspace(),
            &skills_dir,
            SkillDiscoveryMode::CodeWhaleOnly,
            Some(&plugins),
        );
        let skill = catalog
            .get("skills-root:quick-check")
            .expect("Native-only root merged into existing catalog");
        assert!(skill.invocation.user_invocable());
        let provenance = skill.source.provenance().unwrap();
        provenance
            .verify_for(fixture.workspace(), Some(&plugins))
            .unwrap();
        let block = codewhale_skills::render_available_skills_context_for_workspace_and_dir_with_mode_and_plugins(fixture.workspace(), &skills_dir, SkillDiscoveryMode::CodeWhaleOnly, "en", Some(&plugins), codewhale_skills::SkillListingConfig::default(), 8192).unwrap();
        assert!(block.contains("skills-root:quick-check"));
        let context =
            ToolContext::new(fixture.workspace()).with_plugin_registry(Arc::clone(&plugins));
        let result = crate::tools::skill::LoadSkillTool
            .execute(
                serde_json::json!({"name":"skills-root:quick-check"}),
                &context,
            )
            .await
            .unwrap();
        assert!(result.content.contains("Inspect the exact source"));
        assert!(result.metadata.as_ref().unwrap()["skill_path"].is_null());
        let restored: SkillProvenance =
            serde_json::from_str(&serde_json::to_string(&provenance).unwrap()).unwrap();
        let root = manager
            .shared
            .registry
            .lock()
            .unwrap()
            .live_skill_roots()
            .pop()
            .unwrap();
        manager
            .shared
            .registry
            .lock()
            .unwrap()
            .unregister(&root.owner, root.handle);
        assert!(
            restored
                .verify_for(fixture.workspace(), Some(&plugins))
                .is_err()
        );
        let empty = codewhale_skills::discover_for_workspace_and_dir_with_mode_and_plugins(
            fixture.workspace(),
            &skills_dir,
            SkillDiscoveryMode::CodeWhaleOnly,
            Some(&plugins),
        );
        assert!(
            empty.get("skills-root:quick-check").is_none(),
            "the cache must not retain disposed roots"
        );
        manager.shutdown().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_skill_receipt_refuses_disable_source_tamper_and_host_restart() {
        let Some(node) =
            node_for_tests("native_skill_receipt_refuses_disable_source_tamper_and_host_restart")
        else {
            return;
        };
        let _home = crate::test_support::SealedHome::new();
        let _policy = TestPolicyGuard::extension_host(true);
        let fixture = FixturePlugins::new(&["skills-root"]).await;
        let plugins = fixture.registry();
        let manager = fixture.manager(node);
        let _manager = super::super::TestManagerGuard::install(Arc::clone(&manager));
        let engine = manager.attach(Arc::clone(&plugins));
        engine.sync().await.unwrap();
        let plugins = engine.plugin_view();
        let catalog = codewhale_skills::discover_in_workspace_with_mode_and_plugins(
            fixture.workspace(),
            SkillDiscoveryMode::CodeWhaleOnly,
            Some(&plugins),
        );
        let provenance = catalog
            .get("skills-root:quick-check")
            .unwrap()
            .source
            .provenance()
            .unwrap();
        manager.shutdown().await;
        assert!(
            provenance
                .verify_for(fixture.workspace(), Some(&plugins))
                .is_err()
        );
        let id = plugins.get("skills-root").unwrap().id.as_str();
        assert!(matches!(
            manager.owner_state(id),
            Some(super::super::registry::OwnerState::Failed(_))
        ));
        engine.sync().await.unwrap();
        assert!(
            manager
                .shared
                .registry
                .lock()
                .unwrap()
                .live_skill_roots()
                .is_empty(),
            "unchanged failed activations require an explicit retry"
        );
        manager.retry();
        engine.sync().await.unwrap();
        assert_eq!(
            manager.owner_state(id),
            Some(super::super::registry::OwnerState::Active),
            "owner: {:?}; diagnostics: {:?}",
            manager.owner_state(id),
            manager.diagnostics()
        );
        assert!(
            provenance
                .verify_for(fixture.workspace(), Some(&plugins))
                .is_err(),
            "restart must not revive old handles"
        );
        let current = codewhale_skills::discover_in_workspace_with_mode_and_plugins(
            fixture.workspace(),
            SkillDiscoveryMode::CodeWhaleOnly,
            Some(&plugins),
        );
        let current = current
            .get("skills-root:quick-check")
            .unwrap_or_else(|| {
                panic!(
                    "Native root missing after explicit retry; owner: {:?}; diagnostics: {:?}",
                    manager.owner_state(id),
                    manager.diagnostics()
                )
            })
            .source
            .provenance()
            .unwrap();
        let source_root = codewhale_plugins::agent_plugin::plugin_root_for_manifest(
            &current.authority().source_manifest,
        )
        .unwrap();
        let source_skill = source_root.join("profiles/review-skills/quick-check/SKILL.md");
        let reviewed = std::fs::read(&source_skill).unwrap();
        std::fs::write(&source_skill, "changed after review").unwrap();
        assert!(
            current
                .verify_for(fixture.workspace(), Some(&plugins))
                .is_err(),
            "mutable source tamper fails before reconcile"
        );
        std::fs::write(&source_skill, reviewed).unwrap();
        current
            .verify_for(fixture.workspace(), Some(&plugins))
            .unwrap();
        fixture.disable("skills-root");
        assert!(
            current
                .verify_for(fixture.workspace(), Some(&plugins))
                .is_err(),
            "persisted disable wins before reconcile"
        );
        manager.shutdown().await;
    }
}
