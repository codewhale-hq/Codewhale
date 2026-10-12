//! Plugin install on-ramp (#5182).
//!
//! Fetches a plugin bundle from a local directory, a `github:owner/repo`
//! archive, or a direct tarball URL, and places it under the user plugins
//! root (`~/.codewhale/plugins/<name>/`). This module deliberately mirrors
//! [`crate::skills::install`]: the download, network-gating, traversal
//! rejection, and marker machinery is *reused* from there (`fetch_tarball`,
//! `is_safe_path`, `write_installed_from_v2`, `INSTALLED_FROM_MARKER`), while
//! the scan/extract step is plugin-shaped (a bundle is rooted at the single
//! supported plugin manifest in the tree, not at a `SKILL.md`).
//!
//! # Hard rules
//!
//! * Everything is staged in a private `.staging-*` sibling first. The
//!   destination is only created (via atomic rename) once the bundle clears
//!   every check — half-installed plugins never appear on disk.
//! * The fetched tree must contain **exactly one** plugin bundle root holding
//!   `plugin.json`, `kimi.plugin.json`, or `plugin.toml`. Zero (not a plugin)
//!   or more than one (ambiguous mono-repo) are both rejected.
//! * Path traversal (`..`, absolute paths) and symlinks/hard links inside the
//!   selected bundle subtree are rejected. Entries outside the subtree are
//!   never extracted.
//! * The manifest `[plugin].name` must be a single path-safe segment; it
//!   becomes the destination directory name.
//! * Overwriting a bundle that lacks the `.installed-from` marker is refused
//!   — hand-placed bundles are never clobbered. `update` publishes a replacement
//!   when source or installed content differs; a changed bundle automatically
//!   invalidates the hash-bound trust receipt at the next discovery.
//! * Installed bits land **disabled and untrusted**; trust/enablement is the
//!   existing registry flow, not this module's concern.

//!
//! # Module map
//!
//! This module owns the source spec, the result types, and the three
//! verbs. The pipeline stages each live next door:
//!
//! * [`stage`] — copy a local bundle into a private `.staging-*` sibling
//!   (symlink, file-count, and size rejection; manifest validation).
//! * [`tarball`] — the two-pass archive reader: scan for the single
//!   supported manifest under the size cap, then extract just that subtree.
//! * [`place`] — atomic rename into `<name>/`, marker write, and the
//!   containment guards shared with discovery.
//!
//! Fetching is not ours: remote bytes come from
//! [`crate::skills::install::fetch_tarball`], network gating included.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use thiserror::Error;

/// A validated manifest name is already occupied by another loaded bundle.
/// Keep this typed so API clients receive a conflict, not a server failure.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct PluginNameConflict(pub String);

use crate::network_policy::NetworkPolicy;
use crate::plugins::manifest::PluginManifest;
use crate::skills::install::{
    self as skill_install, FetchOutcome, InstallSource, InstalledFromMarker, fetch_tarball,
    sha256_hex, source_spec_string,
};

pub(crate) mod dsh;
mod place;
mod stage;
mod tarball;

#[cfg(test)]
mod tests;

use place::{ensure_target_within_plugins_dir, finalize_install, plugin_target_path};
use stage::stage_local_copy;
use tarball::stage_tarball;

/// Marker file shared with the skill installer. Its presence means "this
/// bundle was placed by `/plugin install`" and enables update/uninstall.
pub use crate::skills::install::INSTALLED_FROM_MARKER;

/// Default per-bundle size cap. Mirrors the skill installer; the runtime
/// staging budget in `registry.rs` stays the outer bound.
pub const DEFAULT_MAX_SIZE_BYTES: u64 = skill_install::DEFAULT_MAX_SIZE_BYTES;

// ─────────────────────────────────────────────────────────────────────────────
// Source parsing
// ─────────────────────────────────────────────────────────────────────────────

/// Where a plugin bundle is installed from. See [`PluginInstallSource::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginInstallSource {
    /// Local bundle directory (copied, never executed). Parsed from a plain
    /// path or an explicit `path:<dir>` spec (the marker round-trip form).
    LocalPath(PathBuf),
    /// `github:owner/repo` or a direct `http(s)://…` tarball URL, downloaded
    /// through the shared skill-install machinery. There is no registry
    /// index in v1.
    Remote(InstallSource),
    /// A local DeepSeek Harness bundle package, converted by [`dsh`] into a
    /// native bundle that then takes the same staged, reviewed install path.
    /// Parsed from `dsh:<dir>`, or from a plain local path that holds a DSH
    /// `package.json` and no native manifest.
    Dsh(PathBuf),
}

impl PluginInstallSource {
    /// Parse a user-supplied spec.
    ///
    /// * `github:owner/repo`, `https://…` → [`PluginInstallSource::Remote`]
    ///   (via [`InstallSource::parse`]; registry names are unreachable here)
    /// * `path:<dir>` or any other value → [`PluginInstallSource::LocalPath`]
    pub fn parse(spec: &str) -> Result<Self> {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            bail!("install source must not be empty");
        }
        if let Some(spec) = trimmed.strip_prefix("git:") {
            let spec = spec.strip_prefix("https://").unwrap_or(spec);
            let spec = spec
                .strip_prefix("github.com/")
                .context("git plugin sources must use git:github.com/owner/repo[@ref]")?;
            let (repo, revision) = spec
                .rsplit_once('@')
                .map_or((spec, None), |(repo, revision)| (repo, Some(revision)));
            let repo = repo.strip_suffix(".git").unwrap_or(repo);
            let valid = |part: &str| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && part
                        .bytes()
                        .all(|ch| ch.is_ascii_alphanumeric() || b"-_.".contains(&ch))
            };
            let parts: Vec<_> = repo.split('/').collect();
            if parts.len() != 2 || !parts.iter().all(|part| valid(part)) {
                bail!("git plugin source must name one GitHub owner/repository");
            }
            return match revision {
                None => Ok(Self::Remote(InstallSource::GitHubRepo(repo.to_owned()))),
                Some(revision) if valid(revision) => Ok(Self::Remote(InstallSource::DirectUrl(
                    format!("https://github.com/{repo}/archive/{revision}.tar.gz"),
                ))),
                _ => bail!("git plugin ref must be a single safe tag, branch name or commit"),
            };
        }
        if let Some(spec) = trimmed.strip_prefix("npm:") {
            let (package, version) = spec
                .rsplit_once('@')
                .context("npm plugin sources require an exact version: npm:package@1.2.3")?;
            let valid = |part: &str| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && part.bytes().all(|ch| {
                        ch.is_ascii_lowercase() || ch.is_ascii_digit() || b"-_.".contains(&ch)
                    })
            };
            let name = if let Some(scoped) = package.strip_prefix('@') {
                let (scope, name) = scoped.split_once('/').context("invalid npm scope/name")?;
                if !valid(scope) || !valid(name) {
                    bail!("invalid npm scope/name");
                }
                name
            } else {
                if !valid(package) {
                    bail!("invalid npm package name");
                }
                package
            };
            let parsed =
                semver::Version::parse(version).context("npm plugin version must be exact")?;
            if parsed.to_string() != version {
                bail!("npm plugin version must be canonical");
            }
            return Ok(Self::Remote(InstallSource::DirectUrl(format!(
                "https://registry.npmjs.org/{package}/-/{name}-{version}.tgz"
            ))));
        }
        if let Some(path) = trimmed.strip_prefix("path:") {
            return Self::local(path);
        }
        if let Some(path) = trimmed.strip_prefix("dsh:") {
            let path = path.trim();
            if path.is_empty() {
                bail!("DSH package path must not be empty");
            }
            return Ok(Self::Dsh(PathBuf::from(path)));
        }
        if trimmed.starts_with("github:")
            || trimmed.starts_with("https://")
            || trimmed.starts_with("http://")
        {
            let source = InstallSource::parse(trimmed)?;
            return match source {
                InstallSource::GitHubRepo(_) | InstallSource::DirectUrl(_) => {
                    remote_bundle_path(&source)?;
                    Ok(Self::Remote(source))
                }
                InstallSource::Registry(_) => {
                    unreachable!("prefixed specs never parse as a registry name")
                }
            };
        }
        Self::local(trimmed)
    }

    fn local(spec: &str) -> Result<Self> {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            bail!("local install path must not be empty");
        }
        let path = PathBuf::from(trimmed);
        if crate::plugins::agent_plugin::resolve_manifest_path(&path).is_none()
            && dsh::is_dsh_package(&path)
        {
            return Ok(Self::Dsh(path));
        }
        Ok(Self::LocalPath(path))
    }
}

/// Select one manifest-rooted bundle from a repository archive. The fragment
/// is local extraction metadata, never a path sent to or executed by a server.
fn remote_bundle_path(source: &InstallSource) -> Result<Option<String>> {
    let InstallSource::DirectUrl(raw) = source else {
        return Ok(None);
    };
    let url = reqwest::Url::parse(raw).context("invalid plugin archive URL")?;
    let Some(fragment) = url.fragment() else {
        return Ok(None);
    };
    let path = fragment
        .strip_prefix("path=")
        .context("plugin archive fragment must be #path=<bundle-directory>")?;
    if path.is_empty()
        || !path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_' | b'.'))
        })
    {
        bail!("plugin archive bundle path must contain only safe relative directory names");
    }
    Ok(Some(path.to_string()))
}

/// Serialize a source for the `.installed-from` marker. Must round-trip
/// through [`PluginInstallSource::parse`].
fn plugin_spec_string(source: &PluginInstallSource, canonical_source: Option<&Path>) -> String {
    match source {
        PluginInstallSource::LocalPath(_) => {
            let path = canonical_source.expect("local installs record the canonical source");
            format!("path:{}", path.display())
        }
        PluginInstallSource::Remote(remote) => source_spec_string(remote),
        PluginInstallSource::Dsh(_) => {
            let path = canonical_source.expect("DSH installs record the canonical source");
            format!("dsh:{}", path.display())
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Outcome / result types
// ─────────────────────────────────────────────────────────────────────────────

/// Outcome of an install attempt. Same shape as the skill installer's so the
/// caller can drop `NeedsApproval`/`NetworkDenied` into its approval flow.
#[derive(Debug)]
pub enum PluginInstallOutcome {
    /// The bundle was installed (atomic rename + marker write succeeded).
    Installed(InstalledPlugin),
    /// The download host requires user approval; nothing touched disk.
    NeedsApproval(String),
    /// The download host is denied by network policy.
    NetworkDenied(String),
}

/// Metadata for a successfully installed plugin bundle.
#[derive(Debug, Clone)]
pub struct InstalledPlugin {
    /// Plugin name from `[plugin].name`; also the destination directory name.
    pub name: String,
    /// Final on-disk path: `<user_plugins_dir>/<name>/`.
    pub path: PathBuf,
    /// Whole-bundle content hash of the staged tree (pre-marker). Informational;
    /// trust receipts always bind to the discovery-time hash.
    pub content_hash: String,
    /// Whole-bundle hash after the provenance marker is written. Callers can
    /// compare this with immediate rediscovery before reporting success.
    pub installed_content_hash: String,
    /// SHA-256 over the downloaded tarball bytes (empty for local copies).
    /// Used by [`update`] to detect upstream changes without re-extracting.
    pub source_checksum: String,
}

/// Result of an [`update`] call.
#[derive(Debug)]
pub enum PluginUpdateResult {
    /// Source is unchanged; local bundles also match the actual installed tree.
    NoChange,
    /// Content changed and a replacement bundle was published.
    Updated(InstalledPlugin),
    /// Network policy requires approval for the download host.
    NeedsApproval(String),
    /// Network policy denied the download host.
    NetworkDenied(String),
}

/// Install-time errors, kept as an enum so tests can pattern-match without
/// parsing strings.
#[derive(Debug, Error)]
pub enum PluginInstallError {
    #[error("entry escapes destination directory: {0}")]
    PathTraversal(String),
    #[error("bundle is too large; uncompressed total would exceed {limit} bytes")]
    OversizedBundle { limit: u64 },
    #[error(
        "archive must contain exactly one plugin bundle root (a directory holding plugin.json, kimi.plugin.json, or plugin.toml); found {0} (install a single plugin bundle, not a mono-repo)"
    )]
    PluginTomlRoots(usize),
    #[error("symlinks and hard links are not allowed in plugin bundles")]
    SymlinkRejected,
    #[error("plugin '{0}' is already installed; use /plugin update or uninstall it first")]
    AlreadyInstalled(String),
    #[error(
        "plugin '{0}' was not installed via /plugin install (no .installed-from marker); refusing to touch the hand-placed bundle"
    )]
    NotInstalledHere(String),
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Install a plugin bundle into `user_plugins_dir`.
///
/// Steps: resolve source → (remote only) network-gate and download under the
/// size cap → stage into a `.staging-*` sibling, enforcing traversal/symlink/
/// size rules and the single-manifest-root requirement → validate the staged
/// manifest → `name_conflict` check → atomic rename into `<name>/` → write
/// `.installed-from` last.
///
/// `update = false` rejects an existing destination. `update = true` (only
/// called from [`update`]) requires the marker and reserves a unique backup.
/// A failed placement restores only into an absent destination; errors after
/// publication retain the current path and prior backup for recovery.
///
/// `name_conflict` is consulted with the validated manifest name before the
/// rename; returning `Some(message)` aborts the install. It lets the caller
/// reject names already claimed by builtin/workspace bundles.
pub async fn install(
    source: PluginInstallSource,
    user_plugins_dir: &Path,
    max_size: u64,
    network: &NetworkPolicy,
    update: bool,
    name_conflict: &(dyn Fn(&str) -> Option<String> + Send + Sync),
) -> Result<PluginInstallOutcome> {
    install_inner(
        source,
        user_plugins_dir,
        max_size,
        network,
        update,
        name_conflict,
        None,
    )
    .await
}

/// Install only when the exact bytes copied into staging match a prior
/// review hash. The comparison happens before atomic placement, so a source
/// that changes between inspection and copying leaves no installed bundle.
pub async fn install_with_expected_content_hash(
    source: PluginInstallSource,
    user_plugins_dir: &Path,
    max_size: u64,
    network: &NetworkPolicy,
    name_conflict: &(dyn Fn(&str) -> Option<String> + Send + Sync),
    expected_content_hash: &str,
) -> Result<PluginInstallOutcome> {
    install_inner(
        source,
        user_plugins_dir,
        max_size,
        network,
        false,
        name_conflict,
        Some(expected_content_hash),
    )
    .await
}

async fn install_inner(
    source: PluginInstallSource,
    user_plugins_dir: &Path,
    max_size: u64,
    network: &NetworkPolicy,
    update: bool,
    name_conflict: &(dyn Fn(&str) -> Option<String> + Send + Sync),
    expected_content_hash: Option<&str>,
) -> Result<PluginInstallOutcome> {
    match &source {
        PluginInstallSource::LocalPath(path) => {
            let staged = stage_local_copy(path, user_plugins_dir, max_size)?;
            verify_expected_content_hash(&staged, expected_content_hash)?;
            if let Some(conflict) = name_conflict(&staged.name) {
                let _ = fs::remove_dir_all(&staged.staged_path);
                return Err(PluginNameConflict(conflict).into());
            }
            let canonical = path
                .canonicalize()
                .with_context(|| format!("failed to resolve {}", path.display()))?;
            finalize_install(
                staged,
                &plugin_spec_string(&source, Some(&canonical)),
                None,
                "",
                user_plugins_dir,
                update,
            )
        }
        PluginInstallSource::Dsh(package) => {
            let converted = convert_dsh_off_runtime(package.clone()).await?;
            install_converted_dsh(
                converted,
                user_plugins_dir,
                max_size,
                update,
                name_conflict,
                expected_content_hash,
            )
        }
        PluginInstallSource::Remote(remote) => {
            let (bytes, url) = match fetch_tarball(remote, network, max_size).await? {
                FetchOutcome::Bytes { bytes, url } => (bytes, url),
                FetchOutcome::NeedsApproval(host) => {
                    return Ok(PluginInstallOutcome::NeedsApproval(host));
                }
                FetchOutcome::Denied(host) => {
                    return Ok(PluginInstallOutcome::NetworkDenied(host));
                }
            };
            install_remote_bytes(
                remote,
                &bytes,
                &url,
                user_plugins_dir,
                max_size,
                update,
                name_conflict,
                expected_content_hash,
            )
        }
    }
}

/// A DSH package converted into scratch; the scratch directory lives as long
/// as this value.
struct ConvertedDsh {
    canonical: PathBuf,
    _scratch: tempfile::TempDir,
    bundle: PathBuf,
}

/// Parse and convert off the async runtime: conversion reads and copies the
/// whole package synchronously.
async fn convert_dsh_off_runtime(package: PathBuf) -> Result<ConvertedDsh> {
    #[cfg(test)]
    let env_scope = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _env_scope = crate::test_support::join_env_scope(env_scope);
        let canonical = package
            .canonicalize()
            .with_context(|| format!("failed to resolve {}", package.display()))?;
        let (scratch, bundle, _conversion) = dsh::convert_to_scratch(&canonical)?;
        Ok(ConvertedDsh {
            canonical,
            _scratch: scratch,
            bundle,
        })
    })
    .await
    .context("DSH conversion task failed")?
}

/// Stage, verify and place a converted DSH bundle exactly like a local one.
/// The marker records the package and the converted bundle's content hash,
/// so update re-converts and can tell an unchanged package from a changed one.
fn install_converted_dsh(
    converted: ConvertedDsh,
    user_plugins_dir: &Path,
    max_size: u64,
    update: bool,
    name_conflict: &(dyn Fn(&str) -> Option<String> + Send + Sync),
    expected_content_hash: Option<&str>,
) -> Result<PluginInstallOutcome> {
    let canonical = converted.canonical.clone();
    let staged = stage_local_copy(&converted.bundle, user_plugins_dir, max_size)?;
    verify_expected_content_hash(&staged, expected_content_hash)?;
    if let Some(conflict) = name_conflict(&staged.name) {
        let _ = fs::remove_dir_all(&staged.staged_path);
        return Err(PluginNameConflict(conflict).into());
    }
    let checksum = staged.content_hash.clone();
    finalize_install(
        staged,
        &plugin_spec_string(
            &PluginInstallSource::Dsh(canonical.clone()),
            Some(&canonical),
        ),
        None,
        &checksum,
        user_plugins_dir,
        update,
    )
}

/// Review a DSH package without installing it: the conversion receipt and the
/// content hash that an exact install of the same package will stage.
pub(crate) fn preview_dsh(package: &Path) -> Result<(dsh::DshConversion, String)> {
    let (_scratch, bundle, conversion) = dsh::convert_to_scratch(package)?;
    let manifest = crate::plugins::agent_plugin::resolve_manifest_path(&bundle)
        .context("converted DSH bundle has no manifest")?;
    let validated = PluginManifest::validate_from_path(&manifest)
        .map_err(|error| anyhow::anyhow!("converted DSH bundle failed validation: {error}"))?;
    Ok((conversion, validated.content_hash))
}

fn verify_expected_content_hash(
    staged: &stage::StagedPlugin,
    expected_content_hash: Option<&str>,
) -> Result<()> {
    let Some(expected) = expected_content_hash else {
        return Ok(());
    };
    if staged.content_hash == expected {
        return Ok(());
    }
    let actual = staged.content_hash.clone();
    let _ = fs::remove_dir_all(&staged.staged_path);
    bail!(
        "plugin source changed after review: expected content hash {expected}, copied bytes hash is {actual}; nothing was installed"
    )
}

/// Stage and finalize an already-downloaded remote tarball. Kept separate
/// from [`install`] so [`update`] can compare the checksum of the bytes it
/// already fetched instead of downloading twice.
#[allow(clippy::too_many_arguments)]
fn install_remote_bytes(
    remote: &InstallSource,
    bytes: &[u8],
    url: &str,
    user_plugins_dir: &Path,
    max_size: u64,
    update: bool,
    name_conflict: &(dyn Fn(&str) -> Option<String> + Send + Sync),
    expected_content_hash: Option<&str>,
) -> Result<PluginInstallOutcome> {
    let checksum = sha256_hex(bytes);
    let bundle_path = remote_bundle_path(remote)?;
    let staged = stage_tarball(bytes, user_plugins_dir, max_size, bundle_path.as_deref())?;
    verify_expected_content_hash(&staged, expected_content_hash)?;
    if let Some(conflict) = name_conflict(&staged.name) {
        let _ = fs::remove_dir_all(&staged.staged_path);
        return Err(PluginNameConflict(conflict).into());
    }
    finalize_install(
        staged,
        &source_spec_string(remote),
        Some(url),
        &checksum,
        user_plugins_dir,
        update,
    )
}

/// Refresh a previously installed plugin from its recorded source and publish
/// a replacement if its content differs. A changed discovery-time hash requires
/// current review through the registry's existing hash-bound trust receipts.
///
/// Local bundles use the same bounded staging and backup/restore publication
/// as installation. A failed source validation leaves the installed copy intact.
pub async fn update(
    name: &str,
    user_plugins_dir: &Path,
    max_size: u64,
    network: &NetworkPolicy,
) -> Result<PluginUpdateResult> {
    let target = plugin_target_path(name, user_plugins_dir)?;
    if tokio::fs::try_exists(&target).await.unwrap_or(false) {
        ensure_target_within_plugins_dir(&target, user_plugins_dir)?;
    }
    let marker_path = target.join(INSTALLED_FROM_MARKER);
    if !tokio::fs::try_exists(&marker_path).await.unwrap_or(false) {
        return Err(PluginInstallError::NotInstalledHere(name.to_string()).into());
    }
    let marker_body = tokio::fs::read_to_string(&marker_path)
        .await
        .with_context(|| format!("failed to read {}", marker_path.display()))?;
    let marker: InstalledFromMarker = serde_json::from_str(&marker_body)
        .with_context(|| format!("malformed {INSTALLED_FROM_MARKER} for {name}"))?;
    let source = PluginInstallSource::parse(&marker.spec)?;
    if let PluginInstallSource::LocalPath(path) = &source {
        let path = path.clone();
        let user_plugins_dir = user_plugins_dir.to_path_buf();
        let name = name.to_string();
        #[cfg(test)]
        let env_scope = crate::test_support::env_scope_ticket();
        return tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            let _env_scope = crate::test_support::join_env_scope(env_scope);
            let staged = stage_local_copy(&path, &user_plugins_dir, max_size)?;
            let staged_path = staged.staged_path.clone();
            let result = (|| {
                if staged.name != name {
                    return Err(PluginNameConflict(format!(
                        "updated plugin changed name from {name} to {}; original plugin preserved",
                        staged.name
                    ))
                    .into());
                }
                // Compare the actual installed tree using the existing complete
                // bundle hash, including executable intent and provenance. A historical
                // source digest alone cannot detect an altered installed payload.
                fs::write(staged_path.join(INSTALLED_FROM_MARKER), marker_body)?;
                let (_, expected_hash) = stage::validate_staged(&staged_path)?;
                let (_, installed_hash) = stage::validate_staged(&target).context(
                    "installed plugin failed validation; unchanged source was not accepted and the installed copy was preserved",
                )?;
                fs::remove_file(staged_path.join(INSTALLED_FROM_MARKER))?;
                if expected_hash == installed_hash {
                    fs::remove_dir_all(&staged_path)
                        .context("failed to remove unchanged plugin staging copy")?;
                    return Ok(PluginUpdateResult::NoChange);
                }
                match finalize_install(staged, &marker.spec, None, "", &user_plugins_dir, true)? {
                    PluginInstallOutcome::Installed(installed) => {
                        Ok(PluginUpdateResult::Updated(installed))
                    }
                    PluginInstallOutcome::NeedsApproval(host) => {
                        Ok(PluginUpdateResult::NeedsApproval(host))
                    }
                    PluginInstallOutcome::NetworkDenied(host) => {
                        Ok(PluginUpdateResult::NetworkDenied(host))
                    }
                }
            })();
            if result.is_err() {
                let _ = fs::remove_dir_all(&staged_path);
            }
            result
        })
        .await
        .context("local plugin update task failed")?;
    }
    if let PluginInstallSource::Dsh(package) = &source {
        // Re-convert the recorded package. An identical converted bundle is
        // no change; a different one replaces the installed copy, and its
        // new content hash invalidates the trust receipt at next discovery.
        let converted = convert_dsh_off_runtime(package.clone()).await?;
        let content_hash = {
            let manifest = crate::plugins::agent_plugin::resolve_manifest_path(&converted.bundle)
                .context("converted DSH bundle has no manifest")?;
            PluginManifest::validate_from_path(&manifest)
                .map_err(|error| {
                    anyhow::anyhow!("converted DSH bundle failed validation: {error}")
                })?
                .content_hash
        };
        if content_hash == marker.source_checksum() {
            return Ok(PluginUpdateResult::NoChange);
        }
        let outcome = install_converted_dsh(
            converted,
            user_plugins_dir,
            max_size,
            true,
            &|actual| {
                (actual != name).then(|| {
                    format!("updated plugin changed name from {name} to {actual}; original plugin preserved")
                })
            },
            None,
        )?;
        return match outcome {
            PluginInstallOutcome::Installed(installed) => {
                Ok(PluginUpdateResult::Updated(installed))
            }
            PluginInstallOutcome::NeedsApproval(host) => {
                Ok(PluginUpdateResult::NeedsApproval(host))
            }
            PluginInstallOutcome::NetworkDenied(host) => {
                Ok(PluginUpdateResult::NetworkDenied(host))
            }
        };
    }
    let PluginInstallSource::Remote(remote) = source else {
        unreachable!("local and converted sources returned above");
    };

    let (bytes, url) = match fetch_tarball(&remote, network, max_size).await? {
        FetchOutcome::Bytes { bytes, url } => (bytes, url),
        FetchOutcome::NeedsApproval(host) => {
            return Ok(PluginUpdateResult::NeedsApproval(host));
        }
        FetchOutcome::Denied(host) => return Ok(PluginUpdateResult::NetworkDenied(host)),
    };
    if sha256_hex(&bytes) == marker.source_checksum() {
        return Ok(PluginUpdateResult::NoChange);
    }

    let outcome = install_remote_bytes(
        &remote,
        &bytes,
        &url,
        user_plugins_dir,
        max_size,
        true,
        &|actual| {
            (actual != name).then(|| {
                format!(
                    "updated plugin changed name from {name} to {actual}; original plugin preserved"
                )
            })
        },
        None,
    )?;
    match outcome {
        PluginInstallOutcome::Installed(installed) => Ok(PluginUpdateResult::Updated(installed)),
        PluginInstallOutcome::NeedsApproval(host) => Ok(PluginUpdateResult::NeedsApproval(host)),
        PluginInstallOutcome::NetworkDenied(host) => Ok(PluginUpdateResult::NetworkDenied(host)),
    }
}

/// Remove a plugin installed via `/plugin install`.
///
/// Refuses to touch any directory that doesn't carry the `.installed-from`
/// marker — that's our cue that it's hand-placed and not ours to delete.
/// Callers must require the bundle to be disabled first (the mutation
/// controller does) and prune the registry state entry afterwards.
pub fn uninstall(name: &str, user_plugins_dir: &Path) -> Result<()> {
    let target = plugin_target_path(name, user_plugins_dir)?;
    if !target.exists() {
        bail!("plugin '{name}' is not installed at {}", target.display());
    }
    ensure_target_within_plugins_dir(&target, user_plugins_dir)?;
    if !target.join(INSTALLED_FROM_MARKER).exists() {
        return Err(PluginInstallError::NotInstalledHere(name.to_string()).into());
    }
    fs::remove_dir_all(&target)
        .with_context(|| format!("failed to remove {}", target.display()))?;
    Ok(())
}
