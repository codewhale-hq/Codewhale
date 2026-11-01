//! Declarative art registered through the existing reviewed Native owner.
//! No script, lifecycle event, input handler, URL, or filesystem authority is
//! delivered to clients. Named actions are clips on the caller's existing clock.
use super::ManagerShared;
use super::protocol::{RegisterParams, RegisterResult};
use super::supervisor::HostRequestContext;
use super::tier::HostTier;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use codewhale_config::PluginAuthority;
use codewhale_config::{EntryRef, OwnerRef};
use codewhale_plugins::{
    activation::PluginActivationCapability,
    manifest::{PluginManifest, ValidatedManifest},
};
use codewhale_ratatui::avatar::{MAX_PACK_BYTES, MAX_PNG_BYTES, Pack, RegisteredPack};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::Path,
    sync::{Arc, atomic::Ordering},
};

#[derive(Clone, Debug)]
pub struct AvatarRegistration {
    pub handle: u64,
    pub owner: OwnerRef,
    pub scope: Option<EntryRef>,
    pub host_generation: u64,
    pub content_hash: String,
    pub pack: Pack,
    pub png: Arc<Vec<Vec<u8>>>,
}

fn reviewed_bytes(
    review: &ValidatedManifest,
    relative: &Path,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let expected = review
        .file_hashes
        .get(relative)
        .ok_or("avatar file is outside reviewed inventory")?;
    let path = review.canonical_root.join(relative);
    codewhale_util::files::reject_linked_path(&review.canonical_root, &path)
        .map_err(|e| e.to_string())?;
    let file = codewhale_util::fs_confined::open_read(&review.canonical_root, &path)
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("avatar asset is not a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("avatar file exceeds its byte budget".into());
    }
    let mut digest = Sha256::new();
    digest.update(b"codewhale-plugin-file-bytes-v1\0");
    digest.update(&bytes);
    if digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
        != *expected
    {
        return Err("avatar asset changed since review".into());
    }
    Ok(bytes)
}

fn snapshot(authority: &PluginAuthority, relative: &Path) -> Result<(Pack, Vec<Vec<u8>>), String> {
    codewhale_plugins::registry::verify_plugin_component_authority(
        authority,
        PluginActivationCapability::Native,
    )?;
    let review = PluginManifest::validate_from_path(&authority.staged_manifest)?;
    if review.content_hash != authority.content_hash
        || review.capability_hash != authority.capability_hash
    {
        return Err("avatar bundle no longer matches its review".into());
    }
    let pack = Pack::parse(&reviewed_bytes(&review, relative, MAX_PACK_BYTES)?)?;
    let mut images = Vec::new();
    let mut total = 0;
    for atlas in &pack.atlases {
        let path = relative.parent().unwrap_or(Path::new("")).join(atlas);
        let png = reviewed_bytes(&review, &path, MAX_PNG_BYTES)?;
        total += png.len();
        if total > 16 * 1024 * 1024 {
            return Err("avatar pages exceed the pack byte budget".into());
        }
        pack.validate_png(&png)?;
        image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .map_err(|_| "avatar PNG cannot be decoded")?;
        images.push(png);
    }
    codewhale_plugins::registry::verify_plugin_component_authority(
        authority,
        PluginActivationCapability::Native,
    )?;
    Ok((pack, images))
}

pub(super) async fn admit(
    shared: &Arc<ManagerShared>,
    tier: HostTier,
    host_generation: u64,
    params: RegisterParams,
    cx: &HostRequestContext,
) -> RegisterResult {
    let result = async {
        params.check_spec()?;
        if tier != HostTier::Plugin || !params.spec.description.is_empty() {
            return Err("avatar packs require a reviewed Native bundle and a manifest path".into());
        }
        if !codewhale_ratatui::avatar::relative_asset(&params.spec.name)
            || !params.spec.name.ends_with(".json")
        {
            return Err("avatar manifest must be a bounded bundle-relative JSON path".into());
        }
        let authority = shared
            .live_owner_authority(tier, |registry| {
                registry
                    .authority_for(&params.owner)
                    .ok_or("stale avatar owner")?;
                Ok(params.owner.clone())
            })?
            .ok_or("avatar owner has no reviewed authority")?;
        let relative = std::path::PathBuf::from(&params.spec.name);
        let (pack, png) = super::skills::bounded_review_check(
            Arc::clone(&shared.skill_admission),
            &cx.cancel,
            move || snapshot(&authority, &relative),
        )
        .await?;
        shared.ready_host(tier).map_err(|s| s.to_string())?;
        let runtime = shared.tier_runtime(tier);
        let _slot = runtime.host.lock().expect("host lock");
        if runtime.host_generation.load(Ordering::SeqCst) != host_generation
            || cx.cancel.is_cancelled()
        {
            return Err("avatar admission cancelled or host replaced".into());
        }
        shared
            .registry
            .lock()
            .expect("registry lock")
            .register_avatar(&params, pack, png, host_generation)
    }
    .await;
    match result {
        Ok(handle) => RegisterResult::Admitted { handle },
        Err(refused) => {
            shared.plugin_diagnostic(
                &params.owner.plugin_id,
                format!("avatar pack refused: {refused}"),
            );
            RegisterResult::Refused { refused }
        }
    }
}

/// Workspace-wide decorative catalog: every entry still needs a live owner,
/// live composition scope, current host, and current persisted Native receipt.
/// Each request is bounded by the same disk-admission semaphore as Native Skills.
async fn live(workspace: std::path::PathBuf) -> Vec<AvatarRegistration> {
    if !super::activation::extension_host_policy_enabled() {
        return Vec::new();
    }
    let manager = super::manager();
    let shared = Arc::clone(&manager.shared);
    let admission = Arc::clone(&shared.skill_admission);
    super::skills::bounded_review_check(
        admission,
        &tokio_util::sync::CancellationToken::new(),
        move || {
            let entries = shared
                .registry
                .lock()
                .expect("registry lock")
                .live_avatars();
            let mut catalog = Vec::new();
            for entry in entries {
                let authority = shared
                    .registry
                    .lock()
                    .expect("registry lock")
                    .authority_for(&entry.owner);
                let Some(authority) = authority else { continue };
                if authority.workspace != workspace
                    || authority.content_hash != entry.content_hash
                    || codewhale_plugins::registry::verify_plugin_component_authority(
                        &authority,
                        PluginActivationCapability::Native,
                    )
                    .is_err()
                {
                    continue;
                }
                let Ok(host) = shared.ready_host(HostTier::Plugin) else {
                    continue;
                };
                if host.generation != entry.host_generation {
                    continue;
                }
                if !shared
                    .registry
                    .lock()
                    .expect("registry lock")
                    .live_avatars()
                    .iter()
                    .any(|r| r.handle == entry.handle)
                {
                    continue;
                }
                catalog.push(entry);
            }
            Ok(catalog)
        },
    )
    .await
    .unwrap_or_default()
}

pub async fn catalog(workspace: std::path::PathBuf) -> Vec<RegisteredPack> {
    let mut packs: Vec<_> = live(workspace)
        .await
        .into_iter()
        .map(|entry| RegisteredPack {
            key: format!("plugin:{}:{}", entry.owner.plugin_id, entry.pack.id),
            handle: entry.handle,
            content_hash: entry.content_hash,
            pack: entry.pack,
        })
        .collect();
    packs.sort_by(|a, b| a.key.cmp(&b.key).then(a.handle.cmp(&b.handle)));
    packs.dedup_by(|a, b| a.key == b.key);
    packs
}
pub async fn atlas(
    workspace: std::path::PathBuf,
    handle: u64,
    page: usize,
    content_hash: &str,
) -> Option<String> {
    live(workspace)
        .await
        .into_iter()
        .find(|r| r.handle == handle && r.content_hash == content_hash)
        .and_then(|r| r.png.get(page).map(|png| STANDARD.encode(png)))
}

#[cfg(test)]
mod tests {
    use super::super::{
        protocol::{RegisterKind, RegisterSpecWire},
        registry::OwnerRegistry,
        test_support::fake_authority,
    };
    use super::*;
    fn params(owner: OwnerRef) -> RegisterParams {
        RegisterParams {
            owner,
            scope: None,
            kind: RegisterKind::AvatarPack,
            spec: RegisterSpecWire {
                name: "avatars/avatar.json".into(),
                description: String::new(),
                input_schema: None,
                argument_hint: None,
            },
        }
    }
    #[test]
    fn avatar_registration_is_native_owned_and_revocation_removes_it() {
        let mut registry = OwnerRegistry::new();
        let a = registry
            .begin_owner(
                HostTier::Plugin,
                "a",
                "a",
                Some(fake_authority("a")),
                "hash-a",
            )
            .unwrap();
        let b = registry
            .begin_owner(
                HostTier::Plugin,
                "b",
                "b",
                Some(fake_authority("b")),
                "hash-b",
            )
            .unwrap();
        let pack = codewhale_ratatui::whale_girl::pack().clone();
        // Ownership does not depend on native artwork. Generate a valid page
        // so the test also works from a package without repository-only PNGs.
        let mut page = std::io::Cursor::new(Vec::new());
        image::RgbaImage::new(
            u32::from(pack.columns) * u32::from(pack.tile_width),
            u32::from(pack.rows) * u32::from(pack.tile_height),
        )
        .write_to(&mut page, image::ImageFormat::Png)
        .unwrap();
        let png = vec![page.into_inner(); pack.atlases.len()];
        assert!(
            registry.register(&params(a.clone())).is_err(),
            "synchronous admission cannot skip review"
        );
        let handle = registry
            .register_avatar(&params(a.clone()), pack.clone(), png.clone(), 7)
            .unwrap();
        assert!(
            registry.live_avatars().is_empty(),
            "activation must settle first"
        );
        registry.mark_active(&a);
        assert_eq!(registry.live_avatars().len(), 1);
        registry.unregister(&b, handle);
        assert_eq!(registry.live_avatars().len(), 1);
        assert!(
            registry
                .register_avatar(&params(a.clone()), pack.clone(), png.clone(), 7)
                .is_err(),
            "duplicate"
        );
        registry.unregister(&a, handle);
        assert!(registry.live_avatars().is_empty());
        let new_handle = registry
            .register_avatar(&params(a.clone()), pack.clone(), png.clone(), 7)
            .unwrap();
        assert_ne!(handle, new_handle);
        registry.revoke_owner("a");
        assert!(registry.live_avatars().is_empty());
        let replacement = registry
            .begin_owner(
                HostTier::Plugin,
                "a",
                "a",
                Some(fake_authority("a")),
                "hash-a",
            )
            .unwrap();
        assert_ne!(a.generation, replacement.generation);
        assert!(
            registry.register_avatar(&params(a), pack, png, 7).is_err(),
            "stale owner"
        );
    }
    #[test]
    fn avatar_snapshot_requires_inventory_hash_and_bounded_file() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        // Reuse a real validated Native bundle; admission also checks its receipt.
        std::fs::write(root.join("plugin.json"),r#"{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"avatar-fixture","version":"1.0.0","extensions":{"net.codewhale":{"native":{"path":"index.mjs"}}}}"#).unwrap();
        std::fs::write(root.join("index.mjs"), "export default () => {};").unwrap();
        std::fs::write(root.join("avatar.json"), "{}").unwrap();
        let review = PluginManifest::validate_from_path(&root.join("plugin.json")).unwrap();
        assert_eq!(
            reviewed_bytes(&review, Path::new("avatar.json"), 2).unwrap(),
            b"{}"
        );
        assert!(reviewed_bytes(&review, Path::new("avatar.json"), 1).is_err());
        assert!(reviewed_bytes(&review, Path::new("missing.json"), 100).is_err());
        std::fs::OpenOptions::new()
            .append(true)
            .open(root.join("avatar.json"))
            .unwrap()
            .write_all(b" ")
            .unwrap();
        assert!(reviewed_bytes(&review, Path::new("avatar.json"), 100).is_err());
    }
}
