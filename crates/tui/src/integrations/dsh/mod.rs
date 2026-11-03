//! DeepSeek Harness (`dsh`) connected through Codewhale.
//!
//! A thin, reversible adapter around DSH's *documented* seams:
//!
//! - detection reads `dsh --version` / `--help`, `$DSH_HOME` inventory;
//! - connection writes only under `$CODEWHALE_HOME/integrations/dsh/`: a
//!   `--patch` overlay pinning the exact Codewhale route identity and an
//!   append-only receipt; the Codewhale palette rides the bundle profile
//!   via `overrideTokens`, never the overlay;
//! - launch runs `dsh --profile <web|headless> --patch <overlay>` with the
//!   permission posture exported as `DSH_PERMISSION_MODE`, keeping the
//!   user's own `$DSH_HOME` (credentials, sessions, profiles) untouched;
//! - removal deletes only Codewhale-owned files.
//!
//! Codewhale never copies, prints, or embeds API keys, OAuth documents, or
//! filesystem contents; never silently switches the model; never broadens
//! permissions. DSH is an integrated harness surface, not a second Fleet
//! scheduler.

pub(crate) mod brand;
pub(crate) mod bundle;
pub(crate) mod detect;
pub(crate) mod identity;
pub(crate) mod receipt;
pub(crate) mod scene;
pub(crate) mod skin;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub(crate) use bundle::{BundleAvailability, DshAppBundle, DshBundleRecord};
pub(crate) use detect::{DetectEnv, DshCompatibility, DshDetection, DshRunner, ProcessRunner};
pub(crate) use identity::{
    CodewhaleRouteIdentity, DshAdapter, MappedIdentity, WireProtocol, map_identity, render_overlay,
    sha256_hex,
};
pub(crate) use receipt::{
    DshConnectionRecord, DshReceiptDocument, DshReceiptEntry, DshReceiptEvent, now_rfc3339,
    write_atomic,
};

pub(crate) const INTEGRATION_DIR: &str = "integrations/dsh";
pub(crate) const OVERLAY_FILE: &str = "codewhale.patch.yml";
pub(crate) const RECEIPT_FILE: &str = "receipt.json";
pub(crate) const SKIN_FILE: &str = "codewhale-dsh-skin.css";
pub(crate) const SKIN_PREVIEW_FILE: &str = "codewhale-dsh-skin-preview.html";
pub(crate) const RELATIONSHIP_LABEL: &str = "DeepSeek Harness connected through Codewhale";
pub(crate) const CLI_COMMAND: &str = "codewhale integrations dsh";

/// Codewhale-owned files for this integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DshPaths {
    /// The Codewhale home these paths live under; the integration's audit
    /// events are written to its `audit.log` (#6534).
    pub(crate) codewhale_home: PathBuf,
    pub(crate) root: PathBuf,
    pub(crate) overlay: PathBuf,
    pub(crate) receipt: PathBuf,
    pub(crate) skin: PathBuf,
    pub(crate) skin_preview: PathBuf,
    /// Codewhale-owned bundle package directory (documented DSH plugin path).
    pub(crate) bundle_dir: PathBuf,
}

impl DshPaths {
    pub(crate) fn under(codewhale_home: &Path) -> Self {
        let root = codewhale_home.join(INTEGRATION_DIR);
        Self {
            overlay: root.join(OVERLAY_FILE),
            receipt: root.join(RECEIPT_FILE),
            skin: root.join(SKIN_FILE),
            skin_preview: root.join(SKIN_PREVIEW_FILE),
            bundle_dir: root.join(bundle::BUNDLE_DIR),
            root,
            codewhale_home: codewhale_home.to_path_buf(),
        }
    }

    pub(crate) fn from_process() -> Result<Self> {
        Ok(Self::under(&codewhale_config::codewhale_home()?))
    }
}

/// Honest integration state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub(crate) enum DshIntegrationState {
    /// No `dsh` on PATH.
    NotInstalled,
    /// `dsh` exists but could not report a version.
    Offline { reason: String },
    /// `dsh` is older than verified or lacks `--patch`.
    Incompatible {
        version: Option<String>,
        reason: String,
    },
    /// Installed and usable, but no Codewhale overlay exists.
    Detected { version: String },
    /// Overlay present and matches the current Codewhale route.
    Connected { version: String },
    /// Overlay present but the current Codewhale route (or its file) drifted.
    StaleConfig { version: String, reason: String },
    /// Connected, but `dsh` is newer than the verified release.
    StaleVersion { version: String, verified: String },
    /// Overlay kept on disk but launches are refused.
    Disabled { version: Option<String> },
}

impl DshIntegrationState {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::NotInstalled => "not-installed",
            Self::Offline { .. } => "offline",
            Self::Incompatible { .. } => "incompatible",
            Self::Detected { .. } => "detected",
            Self::Connected { .. } => "connected",
            Self::StaleConfig { .. } => "stale-config",
            Self::StaleVersion { .. } => "stale-version",
            Self::Disabled { .. } => "disabled",
        }
    }

    pub(crate) fn launchable(&self) -> bool {
        matches!(self, Self::Connected { .. } | Self::StaleVersion { .. })
    }
}

/// Everything status/plan/doctor need, computed without side effects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DshStatusReport {
    pub(crate) state: DshIntegrationState,
    pub(crate) detection: DshDetection,
    pub(crate) record: Option<DshConnectionRecord>,
    pub(crate) overlay_present: bool,
    pub(crate) overlay_sha256_on_disk: Option<String>,
    /// The identity Codewhale would write *now* (may be `None` when the route
    /// could not be resolved).
    pub(crate) current_identity: Option<MappedIdentity>,
    pub(crate) current_identity_error: Option<String>,
    /// DSH `settings.yaml` namespaces that shadow overlay rows per field.
    pub(crate) shadowing_namespaces: Vec<String>,
    /// Whether the documented plugin path (pnpm) is usable here.
    pub(crate) bundle_availability: BundleAvailability,
    /// The dedicated profile's `dsh.profile.bundles` when the bundle is
    /// installed (read from DSH's manifest, read-only).
    pub(crate) bundle_profile_bundles: Option<Vec<String>>,
    pub(crate) bundle_patch_present: bool,
    pub(crate) paths_root: PathBuf,
    pub(crate) overlay_path: PathBuf,
    pub(crate) receipt_path: PathBuf,
}

const SHADOWING_NAMESPACES: &[&str] = &["agent-default-model", "llm-deepseek", "llm-pi-ai"];

pub(crate) fn shadowing_namespaces(detection: &DshDetection) -> Vec<String> {
    detection
        .settings_namespaces
        .iter()
        .filter(|ns| SHADOWING_NAMESPACES.contains(&ns.as_str()))
        .cloned()
        .collect()
}

/// Drifted client half or a `cordis.patch.yml` that no longer matches the
/// identity overlay plus the optional skin insert row.
fn client_or_patch_stale(
    record: &DshConnectionRecord,
    overlay_bytes: Option<&[u8]>,
    disk_patch_sha256: Option<&str>,
    bundle: &bundle::DshBundleRecord,
) -> Option<String> {
    if let Some(reason) = bundle::client_half_stale(
        &bundle.bundle_dir,
        record.skin_enabled,
        record.ocean_enabled,
    ) {
        return Some(reason);
    }
    let overlay_text = overlay_bytes.and_then(|b| std::str::from_utf8(b).ok());
    let expected_patch =
        overlay_text.map(|text| bundle::render_bundle_patch(text, record.skin_enabled));
    let expected_sha = expected_patch
        .as_ref()
        .map(|text| sha256_hex(text.as_bytes()));
    if disk_patch_sha256 != expected_sha.as_deref() {
        return Some(
            "bundle cordis.patch.yml was modified outside Codewhale; run `update`".to_string(),
        );
    }
    None
}

pub(crate) fn compute_status(
    paths: &DshPaths,
    detection: DshDetection,
    current_identity: Result<CodewhaleRouteIdentity, String>,
    allow_full_access: bool,
    bundle_availability: BundleAvailability,
) -> Result<DshStatusReport> {
    let doc = DshReceiptDocument::load(&paths.receipt)?;
    let record = doc.current;
    let overlay_bytes = std::fs::read(&paths.overlay).ok();
    let overlay_present = overlay_bytes.is_some();
    let overlay_sha256_on_disk = overlay_bytes.as_deref().map(sha256_hex);
    let bundle_patch_bytes = std::fs::read(paths.bundle_dir.join(bundle::BUNDLE_PATCH_FILE)).ok();
    let bundle_patch_present = bundle_patch_bytes.is_some();
    let bundle_patch_sha256 = bundle_patch_bytes.as_deref().map(sha256_hex);
    let bundle_profile_bundles = record
        .as_ref()
        .and_then(|r| r.bundle.as_ref())
        .and_then(|b| bundle::profile_bundles(&b.profile_dir));
    let (current_identity, current_identity_error) = match current_identity {
        Ok(identity) => (Some(map_identity(&identity, allow_full_access)), None),
        Err(error) => (None, Some(error)),
    };
    let shadowing = shadowing_namespaces(&detection);

    let state = if !detection.installed() {
        DshIntegrationState::NotInstalled
    } else {
        match &detection.compatibility {
            DshCompatibility::Offline { reason } => DshIntegrationState::Offline {
                reason: reason.clone(),
            },
            DshCompatibility::Unparsed { raw } => DshIntegrationState::Offline {
                reason: format!("dsh --version printed unparseable text `{raw}`"),
            },
            DshCompatibility::Incompatible { reason } => DshIntegrationState::Incompatible {
                version: detection.version.clone(),
                reason: reason.clone(),
            },
            DshCompatibility::Verified | DshCompatibility::NewerUnverified { .. } => {
                let version = detection.version.clone().unwrap_or_default();
                match record.as_ref() {
                    None => DshIntegrationState::Detected { version },
                    Some(record) if record.disabled => DshIntegrationState::Disabled {
                        version: Some(version),
                    },
                    Some(record) => {
                        let bundle_stale = record.bundle.as_ref().and_then(|b| {
                            if !bundle_patch_present {
                                Some("bundle cordis.patch.yml is missing; run `update`".to_string())
                            } else if let Some(reason) =
                                client_or_patch_stale(record, overlay_bytes.as_deref(), bundle_patch_sha256.as_deref(), b)
                            {
                                Some(reason)
                            } else if b.patch_sha256 != record.overlay_sha256 {
                                Some("bundle and overlay identities differ; run `update`".to_string())
                            } else if !bundle_profile_bundles
                                .as_ref()
                                .is_some_and(|list| list.iter().any(|n| n == bundle::BUNDLE_PACKAGE_NAME))
                            {
                                Some(format!(
                                    "DSH profile `{}` no longer lists {}; run `remove-bundle` then `install-bundle`",
                                    bundle::BUNDLE_PROFILE,
                                    bundle::BUNDLE_PACKAGE_NAME
                                ))
                            } else {
                                None
                            }
                        });
                        let stale_reason = if !overlay_present {
                            Some("overlay file is missing; run `update`".to_string())
                        } else if overlay_sha256_on_disk.as_deref()
                            != Some(record.overlay_sha256.as_str())
                        {
                            Some(
                                "overlay file was modified outside Codewhale; run `update`"
                                    .to_string(),
                            )
                        } else if let Some(reason) = bundle_stale {
                            Some(reason)
                        } else {
                            match current_identity.as_ref() {
                                Some(now) if !now.mappable() => Some(
                                    "current Codewhale route cannot be carried by DSH; overlay is stale"
                                        .to_string(),
                                ),
                                Some(now) => {
                                    let expected = render_overlay(now).map(|text| sha256_hex(text.as_bytes()));
                                    match expected {
                                        Some(expected) if expected == record.overlay_sha256 => None,
                                        Some(_) => Some(format!(
                                            "Codewhale route is now {}/{}; overlay pins {}/{}; run `update`",
                                            now.source.provider_id,
                                            now.source.model,
                                            record.identity.source.provider_id,
                                            record.identity.source.model
                                        )),
                                        None => Some(
                                            "current Codewhale route cannot be carried by DSH; overlay is stale"
                                                .to_string(),
                                        ),
                                    }
                                }
                                None => None,
                            }
                        };
                        match stale_reason {
                            Some(reason) => DshIntegrationState::StaleConfig { version, reason },
                            None => match &detection.compatibility {
                                DshCompatibility::NewerUnverified { verified } => {
                                    DshIntegrationState::StaleVersion {
                                        version,
                                        verified: verified.clone(),
                                    }
                                }
                                _ => DshIntegrationState::Connected { version },
                            },
                        }
                    }
                }
            }
        }
    };

    Ok(DshStatusReport {
        state,
        detection,
        record,
        overlay_present,
        overlay_sha256_on_disk,
        current_identity,
        current_identity_error,
        shadowing_namespaces: shadowing,
        bundle_availability,
        bundle_profile_bundles,
        bundle_patch_present,
        paths_root: paths.root.clone(),
        overlay_path: paths.overlay.clone(),
        receipt_path: paths.receipt.clone(),
    })
}

/// What `connect`/`update` will write, spelled out before any write happens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DshPlan {
    pub(crate) mapped: MappedIdentity,
    pub(crate) overlay_path: PathBuf,
    pub(crate) overlay_text: String,
    pub(crate) overlay_sha256: String,
    pub(crate) receipt_path: PathBuf,
    /// Palette decision for the bundle profile. The `--patch` overlay never
    /// carries skin code; `skin_path` stays unset (no CSS export).
    pub(crate) skin: bool,
    pub(crate) skin_path: Option<PathBuf>,
    /// Ambient ocean scene inside the bundle's client half; only meaningful
    /// with `skin`. Default on; `update --ocean false` turns it off.
    pub(crate) ocean: bool,
    pub(crate) profile: String,
    pub(crate) launch_command: String,
    pub(crate) env_exports: Vec<(String, String)>,
    pub(crate) shadowing_namespaces: Vec<String>,
    pub(crate) disclosures: Vec<String>,
}

pub(crate) fn plan(
    paths: &DshPaths,
    detection: &DshDetection,
    identity: &CodewhaleRouteIdentity,
    profile: &str,
    allow_full_access: bool,
    skin: bool,
    ocean: bool,
) -> Result<DshPlan> {
    let mapped = map_identity(identity, allow_full_access);
    let overlay_text = render_overlay(&mapped).ok_or_else(|| match &mapped.adapter {
        DshAdapter::Unsupported { reason } => anyhow::anyhow!(
            "current Codewhale route {}/{} cannot be carried by DSH: {reason}",
            identity.provider_id,
            identity.model
        ),
        _ => anyhow::anyhow!("overlay could not be rendered"),
    })?;
    let overlay_sha256 = sha256_hex(overlay_text.as_bytes());
    let mut disclosures = mapped.disclosures.clone();
    let shadowing = shadowing_namespaces(detection);
    if !shadowing.is_empty() {
        disclosures.push(format!(
            "$DSH_HOME/settings.yaml has [{}] sections; DSH layers those over the overlay per field, so the saved DSH selection can shadow the pinned identity until you clear it in DSH.",
            shadowing.join(", ")
        ));
    }
    if !detection.profiles.iter().any(|p| p == profile) {
        disclosures.push(format!(
            "DSH profile `{profile}` is not initialized yet; dsh will create $DSH_HOME/profiles/{profile} on first launch (its own documented behavior)."
        ));
    }
    if skin {
        disclosures.push(
            "Skin: Codewhale palette is applied through the bundle profile via overrideTokens (on by default for install-bundle). The --patch overlay path is unchanged; launch --profile web|headless stays overlay-only."
                .to_string(),
        );
        if ocean {
            disclosures.push(format!(
                "Ocean: an ambient canvas scene (whales, glyph fish, bubbles) is spliced into the bundle's client half and a few DSH background tokens become translucent so it shows through; `update --ocean false` turns it off, and in the browser `localStorage[\"{}\"] = \"off\"` or body class `{}` disables it per machine.",
                scene::OCEAN_STORAGE_KEY,
                scene::OCEAN_OFF_CLASS
            ));
        }
    }
    let env_exports = vec![(
        "DSH_PERMISSION_MODE".to_string(),
        mapped.permission_mode.as_str().to_string(),
    )];
    let launch_command = format!(
        "DSH_PERMISSION_MODE={} dsh --profile {profile} --patch {}",
        mapped.permission_mode.as_str(),
        paths.overlay.display()
    );
    Ok(DshPlan {
        mapped,
        overlay_path: paths.overlay.clone(),
        overlay_text,
        overlay_sha256,
        receipt_path: paths.receipt.clone(),
        skin,
        skin_path: None,
        ocean: skin && ocean,
        profile: profile.to_string(),
        launch_command,
        env_exports,
        shadowing_namespaces: shadowing,
        disclosures,
    })
}

fn codewhale_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn identity_summary(mapped: &MappedIdentity) -> String {
    format!(
        "{}/{} via {}",
        mapped.source.provider_id,
        mapped.source.model,
        mapped.dsh_provider().unwrap_or("unsupported")
    )
}

/// Write the overlay and the receipt. `event` is `Connect` for a first
/// connection or `Update` for a rewrite. Skin is a receipt decision for the
/// bundle profile; no stylesheet is written.
pub(crate) fn apply_plan(
    paths: &DshPaths,
    detection: &DshDetection,
    plan: &DshPlan,
    event: DshReceiptEvent,
) -> Result<DshConnectionRecord> {
    std::fs::create_dir_all(&paths.root)
        .with_context(|| format!("create {}", paths.root.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&paths.root, std::fs::Permissions::from_mode(0o700));
    }
    write_atomic(&paths.overlay, plan.overlay_text.as_bytes())?;
    // The 0.9.8 CSS/preview export is gone; drop leftovers so `remove` stays
    // the only cleanup path for those names.
    for leftover in [&paths.skin, &paths.skin_preview] {
        let _ = std::fs::remove_file(leftover);
    }
    let skin_sha256 = plan.skin.then(skin::skin_tokens_sha256);
    let mut doc = DshReceiptDocument::load(&paths.receipt)?;
    let now = now_rfc3339();
    let connected_at = doc
        .current
        .as_ref()
        .map(|r| r.connected_at.clone())
        .filter(|_| event == DshReceiptEvent::Update)
        .unwrap_or_else(|| now.clone());
    // An installed bundle is a `link:` to our directory: rewriting its patch
    // is the whole update, no pnpm needed.
    let bundle_record = match doc.current.as_ref().and_then(|r| r.bundle.clone()) {
        Some(mut b) if event == DshReceiptEvent::Update => {
            let sha = bundle::write_bundle(
                &paths.bundle_dir,
                &codewhale_version(),
                &plan.overlay_text,
                plan.skin,
                plan.ocean,
            )?;
            b.patch_sha256 = sha.clone();
            b.package_version = bundle::bundle_version(&codewhale_version(), &sha);
            b.updated_at = now.clone();
            Some(b)
        }
        _ => None,
    };
    let record = DshConnectionRecord {
        connected_at,
        updated_at: now.clone(),
        dsh_version: detection.version.clone(),
        dsh_binary: detection.binary.clone(),
        dsh_home: detection.dsh_home.clone(),
        profile: plan.profile.clone(),
        overlay_path: paths.overlay.clone(),
        overlay_sha256: plan.overlay_sha256.clone(),
        skin_enabled: plan.skin,
        skin_path: None,
        skin_sha256: skin_sha256.clone(),
        ocean_enabled: plan.ocean,
        disabled: false,
        bundle: bundle_record,
        identity: plan.mapped.clone(),
    };
    doc.push(DshReceiptEntry {
        event: event.clone(),
        at: now,
        codewhale_version: codewhale_version(),
        dsh_version: detection.version.clone(),
        dsh_home: detection.dsh_home.clone(),
        overlay_sha256: Some(plan.overlay_sha256.clone()),
        skin_sha256,
        identity_summary: Some(identity_summary(&plan.mapped)),
        permission_mode: Some(plan.mapped.permission_mode.as_str().to_string()),
        note: None,
    });
    doc.current = Some(record.clone());
    doc.save(&paths.receipt)?;
    codewhale_runtime::audit::log_sensitive_event_in(
        &paths.codewhale_home,
        &format!("integration.dsh.{}", event.as_str()),
        serde_json::json!({
            "overlay_path": paths.overlay.display().to_string(),
            "overlay_sha256": plan.overlay_sha256,
            "identity": identity_summary(&plan.mapped),
            "permission_mode": plan.mapped.permission_mode.as_str(),
            "skin": plan.skin,
            "ocean": plan.ocean,
        }),
    );
    Ok(record)
}

pub(crate) fn set_disabled(paths: &DshPaths, disabled: bool) -> Result<DshConnectionRecord> {
    let mut doc = DshReceiptDocument::load(&paths.receipt)?;
    let Some(mut record) = doc.current.take() else {
        anyhow::bail!(
            "DSH is not connected; nothing to {}",
            if disabled { "disable" } else { "enable" }
        );
    };
    record.disabled = disabled;
    record.updated_at = now_rfc3339();
    let event = if disabled {
        DshReceiptEvent::Disable
    } else {
        DshReceiptEvent::Enable
    };
    doc.push(DshReceiptEntry {
        event: event.clone(),
        at: record.updated_at.clone(),
        codewhale_version: codewhale_version(),
        dsh_version: record.dsh_version.clone(),
        dsh_home: record.dsh_home.clone(),
        overlay_sha256: Some(record.overlay_sha256.clone()),
        skin_sha256: record.skin_sha256.clone(),
        identity_summary: Some(identity_summary(&record.identity)),
        permission_mode: Some(record.identity.permission_mode.as_str().to_string()),
        note: None,
    });
    doc.current = Some(record.clone());
    doc.save(&paths.receipt)?;
    codewhale_runtime::audit::log_sensitive_event_in(
        &paths.codewhale_home,
        &format!("integration.dsh.{}", event.as_str()),
        serde_json::json!({ "overlay_path": paths.overlay.display().to_string() }),
    );
    Ok(record)
}

/// Delete only Codewhale-owned files; keep the receipt history with a
/// terminal `remove` entry. Never touches `$DSH_HOME`.
pub(crate) fn remove(paths: &DshPaths) -> Result<Vec<PathBuf>> {
    let mut doc = DshReceiptDocument::load(&paths.receipt)?;
    if doc.current.as_ref().is_some_and(|r| r.bundle.is_some()) {
        anyhow::bail!(
            "the Codewhale bundle is still installed in DSH profile `{}`; run `{CLI_COMMAND} remove-bundle` first",
            bundle::BUNDLE_PROFILE
        );
    }
    let mut removed = Vec::new();
    for path in [&paths.overlay, &paths.skin, &paths.skin_preview] {
        match std::fs::remove_file(path) {
            Ok(()) => removed.push(path.clone()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("remove {}", path.display())),
        }
    }
    let previous = doc.current.take();
    let now = now_rfc3339();
    doc.push(DshReceiptEntry {
        event: DshReceiptEvent::Remove,
        at: now,
        codewhale_version: codewhale_version(),
        dsh_version: previous.as_ref().and_then(|r| r.dsh_version.clone()),
        dsh_home: previous
            .as_ref()
            .map(|r| r.dsh_home.clone())
            .unwrap_or_default(),
        overlay_sha256: previous.as_ref().map(|r| r.overlay_sha256.clone()),
        skin_sha256: previous.as_ref().and_then(|r| r.skin_sha256.clone()),
        identity_summary: previous.as_ref().map(|r| identity_summary(&r.identity)),
        permission_mode: previous
            .as_ref()
            .map(|r| r.identity.permission_mode.as_str().to_string()),
        note: Some(format!(
            "removed {} Codewhale-owned file(s); $DSH_HOME untouched",
            removed.len()
        )),
    });
    doc.save(&paths.receipt)?;
    codewhale_runtime::audit::log_sensitive_event_in(
        &paths.codewhale_home,
        "integration.dsh.remove",
        serde_json::json!({ "removed": removed.iter().map(|p| p.display().to_string()).collect::<Vec<_>>() }),
    );
    Ok(removed)
}

/// The exact process a launch runs. Returned (not spawned) so callers and
/// tests can inspect it; `spawn_launch` executes it with inherited stdio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchSpec {
    pub(crate) binary: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) env: Vec<(String, String)>,
    /// Variables Codewhale itself injected into this process (a `--api-key`
    /// or keyring-bridged credential) that must not leak into the child.
    pub(crate) strip_env: Vec<String>,
    pub(crate) cwd: PathBuf,
}

/// Names to strip from the child environment when Codewhale's own dispatcher
/// materialized a credential into this process. A key the user exported in
/// their shell is theirs and is left alone; a key Codewhale bridged from
/// `--api-key` or the keyring is Codewhale's and is not handed over.
pub(crate) fn launch_env_strip_list(
    api_key_source: Option<&str>,
    provider_env_vars: &[String],
) -> Vec<String> {
    let mut out = vec![
        codewhale_config::CLI_API_KEY_ENV.to_string(),
        codewhale_config::CLI_API_KEY_SOURCE_ENV.to_string(),
        codewhale_config::LEGACY_CLI_API_KEY_SOURCE_ENV.to_string(),
    ];
    if matches!(api_key_source, Some("cli" | "keyring")) {
        for var in provider_env_vars {
            if !out.contains(var) {
                out.push(var.clone());
            }
        }
    }
    out
}

impl LaunchSpec {
    pub(crate) fn display(&self) -> String {
        let mut out = String::new();
        for (k, v) in &self.env {
            out.push_str(&format!("{k}={v} "));
        }
        out.push_str(&self.binary.display().to_string());
        for arg in &self.args {
            out.push(' ');
            out.push_str(arg);
        }
        out
    }
}

pub(crate) fn launch_spec(
    report: &DshStatusReport,
    profile_override: Option<&str>,
    extra_args: &[String],
    workspace: &Path,
) -> Result<LaunchSpec> {
    let record = report.record.as_ref().ok_or_else(|| {
        anyhow::anyhow!("DSH is not connected; run `{CLI_COMMAND} connect` first")
    })?;
    if !report.state.launchable() {
        match &report.state {
            DshIntegrationState::Connected { .. } | DshIntegrationState::StaleVersion { .. } => {}
            DshIntegrationState::Disabled { .. } => {
                anyhow::bail!(
                    "DSH integration is disabled; run `{CLI_COMMAND} enable` to launch again"
                )
            }
            DshIntegrationState::StaleConfig { reason, .. } => {
                anyhow::bail!(
                    "DSH overlay is stale ({reason}); run `{CLI_COMMAND} update` before launching"
                )
            }
            DshIntegrationState::Incompatible { reason, .. } => {
                anyhow::bail!("installed dsh is incompatible: {reason}")
            }
            DshIntegrationState::Offline { reason } => anyhow::bail!("dsh is offline: {reason}"),
            DshIntegrationState::NotInstalled => anyhow::bail!("dsh is not installed"),
            DshIntegrationState::Detected { .. } => {
                anyhow::bail!("DSH is detected but not connected; run `{CLI_COMMAND} connect`")
            }
        }
    }
    let binary = report
        .detection
        .binary
        .clone()
        .ok_or_else(|| anyhow::anyhow!("dsh binary path is unknown"))?;
    let bundle_installed = record.bundle.is_some();
    let profile = profile_override.unwrap_or(if bundle_installed {
        bundle::BUNDLE_PROFILE
    } else {
        record.profile.as_str()
    });
    if !matches!(profile, "web" | "headless") && profile != bundle::BUNDLE_PROFILE {
        anyhow::bail!(
            "DSH profile must be `web`, `headless`, or `{}` (bundle), got `{profile}`",
            bundle::BUNDLE_PROFILE
        );
    }
    if profile == bundle::BUNDLE_PROFILE && !bundle_installed {
        anyhow::bail!(
            "the `{}` profile carries identity only after `{CLI_COMMAND} install-bundle`",
            bundle::BUNDLE_PROFILE
        );
    }
    let mut args = vec!["--profile".to_string(), profile.to_string()];
    if profile != bundle::BUNDLE_PROFILE {
        // The dedicated bundle profile carries the identity itself; the
        // shipped profiles get it through the overlay.
        args.push("--patch".to_string());
        args.push(report.overlay_path.display().to_string());
    }
    args.extend(extra_args.iter().cloned());
    let provider_env_vars: Vec<String> =
        record.identity.source.api_key_env.iter().cloned().collect();
    let strip_env = launch_env_strip_list(
        codewhale_runtime::config::cli_api_key_source().as_deref(),
        &provider_env_vars,
    );
    Ok(LaunchSpec {
        binary,
        args,
        env: vec![(
            "DSH_PERMISSION_MODE".to_string(),
            record.identity.permission_mode.as_str().to_string(),
        )],
        strip_env,
        cwd: workspace.to_path_buf(),
    })
}

pub(crate) fn spawn_launch(spec: &LaunchSpec) -> Result<i32> {
    let mut command = std::process::Command::new(&spec.binary);
    command.args(&spec.args).current_dir(&spec.cwd);
    for name in &spec.strip_env {
        command.env_remove(name);
    }
    for (k, v) in &spec.env {
        command.env(k, v);
    }
    let status = command
        .status()
        .with_context(|| format!("launch {}", spec.binary.display()))?;
    Ok(status.code().unwrap_or(1))
}

/// Install the Codewhale bundle into the dedicated `codewhale` DSH profile via
/// the documented `dsh plugin --profile codewhale add <path>` (pnpm).
pub(crate) fn install_bundle(
    paths: &DshPaths,
    detection: &DshDetection,
    runner: &dyn DshRunner,
    availability: &BundleAvailability,
    app: DshAppBundle,
) -> Result<DshBundleRecord> {
    let pnpm_version = match availability {
        BundleAvailability::Available { pnpm_version } => pnpm_version.clone(),
        BundleAvailability::NotAvailable { reason } => {
            anyhow::bail!("DSH plugin path not available: {reason}")
        }
    };
    let mut doc = DshReceiptDocument::load(&paths.receipt)?;
    let Some(mut record) = doc.current.take() else {
        anyhow::bail!("DSH is not connected; run `{CLI_COMMAND} connect` first");
    };
    if record.bundle.is_some() {
        anyhow::bail!("bundle already installed; use `{CLI_COMMAND} update` or `remove-bundle`");
    }
    let overlay_text = std::fs::read_to_string(&paths.overlay)
        .with_context(|| format!("read {}", paths.overlay.display()))?;
    if sha256_hex(overlay_text.as_bytes()) != record.overlay_sha256 {
        anyhow::bail!("overlay is stale; run `{CLI_COMMAND} update` before install-bundle");
    }
    // install-bundle defaults the palette on; `update --skin false` is the
    // off switch. `connect --skin` records the same decision for a later update.
    let skin = true;
    // The ocean scene follows the recorded decision (default on).
    let ocean = record.ocean_enabled;
    let patch_sha = bundle::write_bundle(
        &paths.bundle_dir,
        &codewhale_version(),
        &overlay_text,
        skin,
        ocean,
    )?;
    let (app_source, outcomes) =
        match bundle::install_into_profile(runner, detection, app, &paths.bundle_dir) {
            Ok(ok) => ok,
            Err(error) => {
                // Leave DSH state as dsh left it; drop our half-written package.
                let _ = bundle::remove_bundle_files(&paths.bundle_dir);
                return Err(error);
            }
        };
    let now = now_rfc3339();
    let mut digest_input = String::new();
    for outcome in &outcomes {
        digest_input.push_str(&outcome.output_sha256);
        digest_input.push('\n');
    }
    let bundle_record = DshBundleRecord {
        installed_at: now.clone(),
        updated_at: now.clone(),
        profile: bundle::BUNDLE_PROFILE.to_string(),
        profile_dir: detection
            .dsh_home
            .join("profiles")
            .join(bundle::BUNDLE_PROFILE),
        bundle_dir: paths.bundle_dir.clone(),
        package_name: bundle::BUNDLE_PACKAGE_NAME.to_string(),
        package_version: bundle::bundle_version(&codewhale_version(), &patch_sha),
        patch_sha256: patch_sha.clone(),
        app_bundle: app,
        app_bundle_source: app_source,
        pnpm_version,
        pnpm_output_sha256: sha256_hex(digest_input.as_bytes()),
    };
    record.bundle = Some(bundle_record.clone());
    record.skin_enabled = skin;
    record.skin_path = None;
    record.skin_sha256 = Some(skin::skin_tokens_sha256());
    record.ocean_enabled = ocean;
    record.updated_at = now.clone();
    doc.push(DshReceiptEntry {
        event: DshReceiptEvent::InstallBundle,
        at: now,
        codewhale_version: codewhale_version(),
        dsh_version: detection.version.clone(),
        dsh_home: detection.dsh_home.clone(),
        overlay_sha256: Some(record.overlay_sha256.clone()),
        skin_sha256: record.skin_sha256.clone(),
        identity_summary: Some(identity_summary(&record.identity)),
        permission_mode: Some(record.identity.permission_mode.as_str().to_string()),
        note: Some(format!(
            "dsh plugin --profile {} add {} + {}; pnpm {}; output sha256 {}",
            bundle::BUNDLE_PROFILE,
            app.package_name(),
            bundle::BUNDLE_PACKAGE_NAME,
            bundle_record.pnpm_version,
            bundle_record.pnpm_output_sha256
        )),
    });
    doc.current = Some(record);
    doc.save(&paths.receipt)?;
    codewhale_runtime::audit::log_sensitive_event_in(
        &paths.codewhale_home,
        "integration.dsh.install_bundle",
        serde_json::json!({
            "profile_dir": bundle_record.profile_dir.display().to_string(),
            "bundle_dir": paths.bundle_dir.display().to_string(),
            "patch_sha256": bundle_record.patch_sha256,
            "app_bundle": app.package_name(),
        }),
    );
    Ok(bundle_record)
}

/// `dsh plugin --profile codewhale remove codewhale-dsh-bundle`, then delete
/// only the Codewhale-owned bundle files. The DSH profile directory (and the
/// app bundle link dsh recorded there) is DSH-owned and is left in place.
pub(crate) fn remove_bundle(
    paths: &DshPaths,
    detection: &DshDetection,
    runner: &dyn DshRunner,
) -> Result<Vec<PathBuf>> {
    let mut doc = DshReceiptDocument::load(&paths.receipt)?;
    let Some(mut record) = doc.current.take() else {
        anyhow::bail!("DSH is not connected; nothing to remove");
    };
    let Some(bundle_record) = record.bundle.take() else {
        anyhow::bail!("no bundle is installed");
    };
    let outcome = bundle::remove_from_profile(runner, detection)?;
    let removed = bundle::remove_bundle_files(&paths.bundle_dir)?;
    let now = now_rfc3339();
    record.updated_at = now.clone();
    doc.push(DshReceiptEntry {
        event: DshReceiptEvent::RemoveBundle,
        at: now,
        codewhale_version: codewhale_version(),
        dsh_version: detection.version.clone(),
        dsh_home: detection.dsh_home.clone(),
        overlay_sha256: Some(record.overlay_sha256.clone()),
        skin_sha256: record.skin_sha256.clone(),
        identity_summary: Some(identity_summary(&record.identity)),
        permission_mode: Some(record.identity.permission_mode.as_str().to_string()),
        note: Some(format!(
            "dsh plugin --profile {} remove {} (output sha256 {}); removed {} owned file(s); profile dir {} left in place (DSH-owned)",
            bundle::BUNDLE_PROFILE,
            bundle::BUNDLE_PACKAGE_NAME,
            outcome.output_sha256,
            removed.len(),
            bundle_record.profile_dir.display()
        )),
    });
    doc.current = Some(record);
    doc.save(&paths.receipt)?;
    codewhale_runtime::audit::log_sensitive_event_in(
        &paths.codewhale_home,
        "integration.dsh.remove_bundle",
        serde_json::json!({ "removed": removed.iter().map(|p| p.display().to_string()).collect::<Vec<_>>() }),
    );
    Ok(removed)
}
/// Probe the documented plugin path (pnpm on `PATH`) with the real runner.
pub(crate) fn bundle_availability_now() -> BundleAvailability {
    bundle::bundle_availability(std::env::var_os("PATH").as_ref(), &ProcessRunner)
}

/// Derive the non-secret route identity from a loaded Codewhale config.
pub(crate) fn codewhale_route_identity(
    config: &codewhale_runtime::config::Config,
    workspace: &Path,
) -> Result<CodewhaleRouteIdentity, String> {
    let identity = config.active_provider_identity()?;
    let provider = identity.provider;
    let configured_model = config.default_model();
    let route = codewhale_engine::route_runtime::resolve_runtime_route_for_identity(
        config,
        &identity,
        Some(&configured_model),
    )?;
    let candidate = &route.candidate;
    let base_url = candidate.endpoint().base_url.clone();
    let protocol = match candidate.protocol() {
        codewhale_config::provider::WireFormat::ChatCompletions => WireProtocol::ChatCompletions,
        codewhale_config::provider::WireFormat::Responses => WireProtocol::Responses,
        codewhale_config::provider::WireFormat::AnthropicMessages => {
            WireProtocol::AnthropicMessages
        }
    };
    let keyless_local =
        codewhale_runtime::config::provider_route_is_keyless_self_hosted(provider, &base_url);
    let api_key_env = provider
        .provider()
        .env_vars()
        .first()
        .map(|s| (*s).to_string());
    Ok(CodewhaleRouteIdentity {
        provider_id: candidate.provider_id().as_str().to_string(),
        provider_label: route
            .identity
            .compatibility()
            .map_or(route.identity.key.as_str(), |row| row.label)
            .to_string(),
        model: candidate.wire_model_id().as_str().to_string(),
        base_url,
        protocol,
        api_key_env,
        keyless_local,
        reasoning_effort: config.reasoning_effort().map(str::to_string),
        sandbox_mode: config.sandbox_mode.clone(),
        approval_policy: config.approval_policy.clone(),
        yolo: config.yolo.unwrap_or(false),
        workspace: workspace.display().to_string(),
    })
}

/// One-line status for the TUI setup on-ramp and doctor. Side-effect free.
pub(crate) fn status_line(report: &DshStatusReport) -> String {
    let version = report.detection.version.as_deref().unwrap_or("?");
    match &report.state {
        DshIntegrationState::NotInstalled => {
            format!("not installed — `dsh` not on PATH; connect later with `{CLI_COMMAND} connect`")
        }
        DshIntegrationState::Offline { reason } => format!("offline — {reason}"),
        DshIntegrationState::Incompatible { reason, .. } => {
            format!("incompatible — dsh {version}: {reason}")
        }
        DshIntegrationState::Detected { .. } => {
            // Surface route carry-ability before `plan` is ever run, so a
            // refuse-at-plan-time surprise is visible in `status`/doctor.
            let carry = match report.current_identity.as_ref() {
                Some(now) if now.mappable() => format!(
                    "current route {}/{} is carryable via {}",
                    now.source.provider_id,
                    now.source.model,
                    now.dsh_provider().unwrap_or("(unknown adapter)")
                ),
                Some(now) => match &now.adapter {
                    DshAdapter::Unsupported { reason } => format!(
                        "current route {}/{} cannot be carried by DSH: {reason}",
                        now.source.provider_id, now.source.model
                    ),
                    _ => String::new(),
                },
                None => String::new(),
            };
            let carry = if carry.is_empty() {
                String::new()
            } else {
                format!("; {carry}")
            };
            format!(
                "detected — dsh {version}, not connected{carry}; `{CLI_COMMAND} plan` explains what would be written"
            )
        }
        DshIntegrationState::Connected { .. } => {
            let identity = report
                .record
                .as_ref()
                .map(|r| identity_summary(&r.identity))
                .unwrap_or_default();
            let bundle = report
                .record
                .as_ref()
                .and_then(|r| r.bundle.as_ref())
                .map(|b| format!(", bundle in profile `{}`", b.profile))
                .unwrap_or_default();
            format!("connected — dsh {version}, {identity}{bundle} ({RELATIONSHIP_LABEL})")
        }
        DshIntegrationState::StaleConfig { reason, .. } => {
            format!("stale-config — dsh {version}: {reason}")
        }
        DshIntegrationState::StaleVersion { verified, .. } => format!(
            "stale-version — dsh {version} is newer than verified {verified}; connected but unverified"
        ),
        DshIntegrationState::Disabled { .. } => {
            format!("disabled — overlay kept, launches refused; `{CLI_COMMAND} enable`")
        }
    }
}
