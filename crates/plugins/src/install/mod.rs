//! Plugin install on-ramp (#5182).
//!
//! Fetches a plugin bundle from a local directory, a `github:owner/repo`
//! archive, or a direct tarball URL, and places it under the user plugins
//! root (`~/.codewhale/plugins/<name>/`). This module deliberately mirrors
//! [`codewhale_skill_install::install`]: the download, network-gating, traversal
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
//! * `stage` — copy a local bundle into a private `.staging-*` sibling
//!   (symlink, file-count, and size rejection; manifest validation).
//! * `tarball` — the two-pass archive reader: scan for the single
//!   supported manifest under the size cap, then extract just that subtree.
//! * `place` — atomic rename into `<name>/`, marker write, and the
//!   containment guards shared with discovery.
//!
//! Fetching is not ours: remote bytes come from
//! [`codewhale_skill_install::install::fetch_tarball`], network gating included.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use thiserror::Error;

/// A validated manifest name is already occupied by another loaded bundle.
/// Keep this typed so API clients receive a conflict, not a server failure.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct PluginNameConflict(pub String);

#[derive(Debug, Error)]
#[error(
    "plugin source changed after review: expected content hash {expected}, copied bytes hash is {actual}; nothing was installed"
)]
pub struct PluginContentMismatch {
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Error)]
#[error(
    "plugin archive {url} does not match its pinned sha256: expected {expected}, downloaded bytes hash {actual}; nothing was installed"
)]
pub struct PluginArchiveMismatch {
    pub url: String,
    pub expected: String,
    pub actual: String,
}

use crate::manifest::PluginManifest;
use codewhale_net::network_policy::NetworkPolicy;
use codewhale_skill_install::install::{
    FetchOutcome, INSTALLED_FROM_MARKER, InstallSource, InstalledFromMarker, fetch_tarball,
    sha256_hex, source_spec_string,
};

pub mod dsh;
pub(crate) mod npm_deps;
mod place;
mod stage;
mod summary;
mod tarball;

#[cfg(test)]
mod tests;

pub use npm_deps::NpmDepsNetworkError;
use place::{ensure_target_within_plugins_dir, finalize_install, plugin_target_path};
use stage::stage_local_copy;
pub use summary::{PluginSummary, PreviousInstall, SummaryClause, SummaryInput, build_summary};
use tarball::stage_tarball_with_network;

/// Default per-bundle size cap. Mirrors the skill installer; the runtime
/// staging budget in `registry.rs` stays the outer bound.
pub const DEFAULT_MAX_SIZE_BYTES: u64 = 256 * 1024 * 1024;

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
        Self::parse_inner(spec, true)
    }

    pub fn parse_syntax(spec: &str) -> Result<Self> {
        Self::parse_inner(spec, false)
    }

    fn parse_inner(spec: &str, inspect_fs: bool) -> Result<Self> {
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
            return Self::local(path, inspect_fs);
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
                    remote_fragment(&source)?;
                    Ok(Self::Remote(source))
                }
                InstallSource::Registry(_) => {
                    unreachable!("prefixed specs never parse as a registry name")
                }
            };
        }
        Self::local(trimmed, inspect_fs)
    }

    fn local(spec: &str, inspect_fs: bool) -> Result<Self> {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            bail!("local install path must not be empty");
        }
        let path = PathBuf::from(trimmed);
        if inspect_fs && dsh::try_is_dsh_package(&path)? {
            return Ok(Self::Dsh(path));
        }
        Ok(Self::LocalPath(path))
    }

    pub(crate) fn pin_archive_spec(spec: &str, sha256: &str) -> Result<(String, String), String> {
        let digest = sha256.trim().to_ascii_lowercase();
        if digest.len() != 64 || !digest.bytes().all(|ch| ch.is_ascii_hexdigit()) {
            return Err(format!(
                "archive sha256 pin `{sha256}` must be 64 hexadecimal characters"
            ));
        }
        let spec = spec.trim();
        let separator = if spec.contains('#') { '&' } else { '#' };
        let pinned = format!("{spec}{separator}sha256={digest}");
        match Self::parse(&pinned) {
            Ok(Self::Remote(InstallSource::DirectUrl(_))) => Ok((pinned, digest)),
            Ok(_) => Err("a sha256 pin applies only to an http(s) archive URL source".to_string()),
            Err(err) => Err(format!("cannot pin `{spec}` to a sha256: {err}")),
        }
    }
}

/// Read a repository archive's fragment: `path=` selects one manifest-rooted
/// bundle and `sha256=` pins the archive bytes. The fragment is local install
/// metadata, never sent to or executed by a server.
fn remote_fragment(source: &InstallSource) -> Result<(Option<String>, Option<String>)> {
    let InstallSource::DirectUrl(raw) = source else {
        return Ok((None, None));
    };
    let url = reqwest::Url::parse(raw).context("invalid plugin archive URL")?;
    let Some(fragment) = url.fragment() else {
        return Ok((None, None));
    };
    let (mut path, mut sha256) = (None, None);
    for part in fragment.split('&') {
        match part.split_once('=') {
            Some(("path", value)) if path.is_none() => path = Some(value),
            Some(("sha256", value)) if sha256.is_none() => sha256 = Some(value),
            _ => bail!(
                "plugin archive fragment must be #path=<bundle-directory>, #sha256=<hex>, or both joined by &"
            ),
        }
    }
    if let Some(path) = path
        && (path.is_empty()
            || !path.split('/').all(|part| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && part
                        .bytes()
                        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_' | b'.'))
            }))
    {
        bail!("plugin archive bundle path must contain only safe relative directory names");
    }
    if let Some(digest) = sha256
        && (digest.len() != 64
            || !digest
                .bytes()
                .all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch)))
    {
        bail!("plugin archive sha256 pin must be 64 lowercase hexadecimal characters");
    }
    Ok((path.map(str::to_string), sha256.map(str::to_string)))
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
    #[error("archive entry name is longer than {limit} bytes")]
    EntryNameTooLong { limit: usize },
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
            let path = path.clone();
            let dir = user_plugins_dir.to_path_buf();
            let expected = expected_content_hash.map(str::to_string);
            let (staged, canonical) = off_runtime(move || {
                let staged = stage_local_copy(&path, &dir, max_size)?;
                verify_expected_content_hash(&staged, expected.as_deref())?;
                match path.canonicalize() {
                    Ok(canonical) => Ok((staged, canonical)),
                    Err(error) => {
                        let _ = fs::remove_dir_all(&staged.staged_path);
                        Err(error).with_context(|| format!("failed to resolve {}", path.display()))
                    }
                }
            })
            .await?;
            publish_staged(
                StagedOutcome::Ready(staged),
                name_conflict,
                plugin_spec_string(&source, Some(&canonical)),
                None,
                String::new(),
                user_plugins_dir,
                update,
            )
            .await
            .map(|outcome| outcome.expect("fresh installs are never unchanged"))
        }
        PluginInstallSource::Dsh(package) => {
            let converted = match convert_dsh_off_runtime(package.clone(), network.clone()).await {
                Ok(converted) => converted,
                Err(err) => {
                    if let Some(net_err) = err.downcast_ref::<NpmDepsNetworkError>() {
                        return Ok(match net_err {
                            NpmDepsNetworkError::NeedsApproval(host) => {
                                PluginInstallOutcome::NeedsApproval((*host).to_string())
                            }
                            NpmDepsNetworkError::Denied(host) => {
                                PluginInstallOutcome::NetworkDenied((*host).to_string())
                            }
                        });
                    }
                    return Err(err);
                }
            };
            install_converted_dsh(
                converted,
                user_plugins_dir,
                max_size,
                update,
                name_conflict,
                expected_content_hash,
                None,
            )
            .await
            .map(|outcome| outcome.expect("fresh installs are never unchanged"))
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
                bytes,
                &url,
                network,
                user_plugins_dir,
                max_size,
                update,
                name_conflict,
                expected_content_hash,
                None,
            )
            .await
            .map(|outcome| outcome.expect("fresh installs are never unchanged"))
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
async fn convert_dsh_off_runtime(package: PathBuf, network: NetworkPolicy) -> Result<ConvertedDsh> {
    #[cfg(any(test, feature = "test-support"))]
    let env_scope = codewhale_runtime::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _env_scope = codewhale_runtime::test_support::join_env_scope(env_scope);
        let canonical = package
            .canonicalize()
            .with_context(|| format!("failed to resolve {}", package.display()))?;
        let (scratch, bundle, _conversion) =
            dsh::convert_to_scratch_with_network(&canonical, Some(&network))?;
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
#[allow(clippy::too_many_arguments)]
async fn install_converted_dsh(
    converted: ConvertedDsh,
    user_plugins_dir: &Path,
    max_size: u64,
    update: bool,
    name_conflict: &(dyn Fn(&str) -> Option<String> + Send + Sync),
    expected_content_hash: Option<&str>,
    unchanged_from: Option<(PathBuf, String)>,
) -> Result<Option<PluginInstallOutcome>> {
    let canonical = converted.canonical.clone();
    let dir = user_plugins_dir.to_path_buf();
    let expected = expected_content_hash.map(str::to_string);
    let staged = off_runtime(move || {
        let staged = stage_local_copy(&converted.bundle, &dir, max_size)?;
        verify_expected_content_hash(&staged, expected.as_deref())?;
        ready_or_unchanged(staged, unchanged_from)
    })
    .await?;
    let checksum = match &staged {
        StagedOutcome::Ready(staged) => staged.content_hash.clone(),
        _ => String::new(),
    };
    publish_staged(
        staged,
        name_conflict,
        plugin_spec_string(
            &PluginInstallSource::Dsh(canonical.clone()),
            Some(&canonical),
        ),
        None,
        checksum,
        user_plugins_dir,
        update,
    )
    .await
}

enum StagedOutcome {
    Ready(stage::StagedPlugin),
    Blocked(PluginInstallOutcome),
    Unchanged,
}

async fn off_runtime<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    #[cfg(any(test, feature = "test-support"))]
    let env_scope = codewhale_runtime::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _env_scope = codewhale_runtime::test_support::join_env_scope(env_scope);
        work()
    })
    .await
    .context("plugin install task failed")?
}

fn network_outcome(err: anyhow::Error) -> Result<PluginInstallOutcome> {
    match err.downcast_ref::<NpmDepsNetworkError>() {
        Some(NpmDepsNetworkError::NeedsApproval(host)) => {
            Ok(PluginInstallOutcome::NeedsApproval((*host).to_string()))
        }
        Some(NpmDepsNetworkError::Denied(host)) => {
            Ok(PluginInstallOutcome::NetworkDenied((*host).to_string()))
        }
        None => Err(err),
    }
}

fn installed_matches_staged(staged_path: &Path, target: &Path, marker_body: &str) -> Result<bool> {
    fs::write(staged_path.join(INSTALLED_FROM_MARKER), marker_body)?;
    let (_, expected_hash) = stage::validate_staged(staged_path)?;
    let (_, installed_hash) = stage::validate_staged(target).context(
        "installed plugin failed validation; unchanged source was not accepted and the installed copy was preserved",
    )?;
    fs::remove_file(staged_path.join(INSTALLED_FROM_MARKER))?;
    Ok(expected_hash == installed_hash)
}

fn ready_or_unchanged(
    staged: stage::StagedPlugin,
    unchanged_from: Option<(PathBuf, String)>,
) -> Result<StagedOutcome> {
    let Some((target, marker_body)) = unchanged_from else {
        return Ok(StagedOutcome::Ready(staged));
    };
    match installed_matches_staged(&staged.staged_path, &target, &marker_body) {
        Ok(true) => {
            fs::remove_dir_all(&staged.staged_path)
                .context("failed to remove unchanged plugin staging copy")?;
            Ok(StagedOutcome::Unchanged)
        }
        Ok(false) => Ok(StagedOutcome::Ready(staged)),
        Err(err) => {
            let _ = fs::remove_dir_all(&staged.staged_path);
            Err(err)
        }
    }
}

async fn publish_staged(
    staged: StagedOutcome,
    name_conflict: &(dyn Fn(&str) -> Option<String> + Send + Sync),
    spec: String,
    url: Option<String>,
    checksum: String,
    user_plugins_dir: &Path,
    update: bool,
) -> Result<Option<PluginInstallOutcome>> {
    let staged = match staged {
        StagedOutcome::Ready(staged) => staged,
        StagedOutcome::Blocked(outcome) => return Ok(Some(outcome)),
        StagedOutcome::Unchanged => return Ok(None),
    };
    if let Some(conflict) = name_conflict(&staged.name) {
        let _ = tokio::fs::remove_dir_all(&staged.staged_path).await;
        return Err(PluginNameConflict(conflict).into());
    }
    let dir = user_plugins_dir.to_path_buf();
    off_runtime(move || {
        finalize_install(staged, &spec, url.as_deref(), &checksum, &dir, update).map(Some)
    })
    .await
}

/// Review a DSH package without installing it: the conversion receipt and the
/// content hash that an exact install of the same package will stage.
pub fn preview_dsh(package: &Path) -> Result<(dsh::DshConversion, String)> {
    preview_dsh_with_network_opt(package, None)
}

pub fn preview_dsh_with_network(
    package: &Path,
    network: &NetworkPolicy,
) -> Result<(dsh::DshConversion, String)> {
    preview_dsh_with_network_opt(package, Some(network))
}

fn preview_dsh_with_network_opt(
    package: &Path,
    network: Option<&NetworkPolicy>,
) -> Result<(dsh::DshConversion, String)> {
    let (_scratch, bundle, conversion) = dsh::convert_to_scratch_with_network(package, network)?;
    let manifest = crate::agent_plugin::resolve_manifest_path(&bundle)
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
    Err(PluginContentMismatch {
        expected: expected.to_string(),
        actual,
    }
    .into())
}

#[derive(Debug, Clone)]
pub struct DownloadedRemote {
    pub remote: InstallSource,
    pub resolved_url: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub enum RemoteDownloadOutcome {
    Downloaded {
        archive: DownloadedRemote,
        has_npm_deps: bool,
    },
    NeedsApproval(String),
    NetworkDenied(String),
}

#[derive(Debug, Clone)]
pub struct PreparedRemote {
    pub remote: InstallSource,
    pub resolved_url: String,
    pub bytes: Vec<u8>,
    pub name: String,
    pub content_hash: String,
}

#[derive(Debug)]
pub enum RemotePreviewOutcome {
    Ready(PreparedRemote),
    NeedsApproval(String),
    NetworkDenied(String),
}

pub async fn download_remote_install_archive(
    remote: InstallSource,
    max_size: u64,
    network: &NetworkPolicy,
) -> Result<RemoteDownloadOutcome> {
    let (bytes, url) = match fetch_tarball(&remote, network, max_size).await? {
        FetchOutcome::Bytes { bytes, url } => (bytes, url),
        FetchOutcome::NeedsApproval(host) => {
            return Ok(RemoteDownloadOutcome::NeedsApproval(host));
        }
        FetchOutcome::Denied(host) => {
            return Ok(RemoteDownloadOutcome::NetworkDenied(host));
        }
    };
    let checksum = sha256_hex(&bytes);
    let (bundle_path, pinned_sha256) = remote_fragment(&remote)?;
    if let Some(pinned) = pinned_sha256
        && pinned != checksum
    {
        return Err(PluginArchiveMismatch {
            url,
            expected: pinned,
            actual: checksum,
        }
        .into());
    }
    let bytes_clone = bytes.clone();
    #[cfg(any(test, feature = "test-support"))]
    let env_scope = codewhale_runtime::test_support::env_scope_ticket();
    let has_npm_deps = tokio::task::spawn_blocking(move || -> Result<bool> {
        #[cfg(any(test, feature = "test-support"))]
        let _env_scope = codewhale_runtime::test_support::join_env_scope(env_scope);
        let scan =
            tarball::scan_tarball_for_bundle(&bytes_clone, max_size, bundle_path.as_deref())?;
        if !scan.is_dsh {
            return Ok(false);
        }
        let raw_dir = tempfile::tempdir().context("failed to create DSH extraction dir")?;
        tarball::extract_into(&scan, &bytes_clone, raw_dir.path(), max_size)?;
        dsh::has_unresolved_npm_dependencies(raw_dir.path())
    })
    .await
    .context("remote archive inspection task failed")??;
    Ok(RemoteDownloadOutcome::Downloaded {
        archive: DownloadedRemote {
            remote,
            resolved_url: url,
            bytes,
        },
        has_npm_deps,
    })
}

pub async fn stage_downloaded_remote_install(
    archive: DownloadedRemote,
    max_size: u64,
    network: Option<&NetworkPolicy>,
) -> Result<RemotePreviewOutcome> {
    let (bundle_path, _) = remote_fragment(&archive.remote)?;
    let network_clone = network.cloned();
    let bytes_clone = archive.bytes.clone();
    #[cfg(any(test, feature = "test-support"))]
    let env_scope = codewhale_runtime::test_support::env_scope_ticket();
    let staged = tokio::task::spawn_blocking(move || -> Result<_> {
        #[cfg(any(test, feature = "test-support"))]
        let _env_scope = codewhale_runtime::test_support::join_env_scope(env_scope);
        let stage_root = tempfile::Builder::new()
            .prefix("codewhale-remote-preview-")
            .tempdir()?;
        let staged = stage_tarball_with_network(
            &bytes_clone,
            stage_root.path(),
            max_size,
            bundle_path.as_deref(),
            network_clone.as_ref(),
        )?;
        Ok((staged.name, staged.content_hash))
    })
    .await
    .context("remote preview staging task failed")?;
    match staged {
        Ok((name, content_hash)) => Ok(RemotePreviewOutcome::Ready(PreparedRemote {
            remote: archive.remote,
            resolved_url: archive.resolved_url,
            bytes: archive.bytes,
            name,
            content_hash,
        })),
        Err(err) => match err.downcast_ref::<NpmDepsNetworkError>() {
            Some(NpmDepsNetworkError::NeedsApproval(host)) => {
                Ok(RemotePreviewOutcome::NeedsApproval((*host).to_string()))
            }
            Some(NpmDepsNetworkError::Denied(host)) => {
                Ok(RemotePreviewOutcome::NetworkDenied((*host).to_string()))
            }
            None => Err(err),
        },
    }
}

pub async fn prepare_remote_install(
    remote: InstallSource,
    max_size: u64,
    network: &NetworkPolicy,
) -> Result<RemotePreviewOutcome> {
    match download_remote_install_archive(remote, max_size, network).await? {
        RemoteDownloadOutcome::Downloaded { archive, .. } => {
            stage_downloaded_remote_install(archive, max_size, Some(network)).await
        }
        RemoteDownloadOutcome::NeedsApproval(host) => Ok(RemotePreviewOutcome::NeedsApproval(host)),
        RemoteDownloadOutcome::NetworkDenied(host) => Ok(RemotePreviewOutcome::NetworkDenied(host)),
    }
}

pub async fn install_prepared_remote(
    prepared: &PreparedRemote,
    user_plugins_dir: &Path,
    max_size: u64,
    network: &NetworkPolicy,
    name_conflict: &(dyn Fn(&str) -> Option<String> + Send + Sync),
    expected_content_hash: &str,
) -> Result<PluginInstallOutcome> {
    install_remote_bytes(
        &prepared.remote,
        prepared.bytes.clone(),
        &prepared.resolved_url,
        network,
        user_plugins_dir,
        max_size,
        false,
        name_conflict,
        Some(expected_content_hash),
        None,
    )
    .await
    .map(|outcome| outcome.expect("fresh installs are never unchanged"))
}

/// Stage and finalize an already-downloaded remote tarball. Kept separate
/// from [`install`] so [`update`] can compare the checksum of the bytes it
/// already fetched instead of downloading twice.
#[allow(clippy::too_many_arguments)]
async fn install_remote_bytes(
    remote: &InstallSource,
    bytes: Vec<u8>,
    url: &str,
    network: &NetworkPolicy,
    user_plugins_dir: &Path,
    max_size: u64,
    update: bool,
    name_conflict: &(dyn Fn(&str) -> Option<String> + Send + Sync),
    expected_content_hash: Option<&str>,
    unchanged_from: Option<(PathBuf, String)>,
) -> Result<Option<PluginInstallOutcome>> {
    let checksum = sha256_hex(&bytes);
    let (bundle_path, pinned_sha256) = remote_fragment(remote)?;
    if let Some(pinned) = pinned_sha256
        && pinned != checksum
    {
        return Err(PluginArchiveMismatch {
            url: url.to_string(),
            expected: pinned,
            actual: checksum,
        }
        .into());
    }
    let dir = user_plugins_dir.to_path_buf();
    let network = network.clone();
    let expected = expected_content_hash.map(str::to_string);
    let staged = off_runtime(move || {
        let staged = match stage_tarball_with_network(
            &bytes,
            &dir,
            max_size,
            bundle_path.as_deref(),
            Some(&network),
        ) {
            Ok(staged) => staged,
            Err(err) => return network_outcome(err).map(StagedOutcome::Blocked),
        };
        verify_expected_content_hash(&staged, expected.as_deref())?;
        ready_or_unchanged(staged, unchanged_from)
    })
    .await?;
    publish_staged(
        staged,
        name_conflict,
        source_spec_string(remote),
        Some(url.to_string()),
        checksum,
        user_plugins_dir,
        update,
    )
    .await
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
    let source = if let Some(path) = marker.spec.trim().strip_prefix("path:") {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            bail!("local install path must not be empty");
        }
        PluginInstallSource::LocalPath(PathBuf::from(trimmed))
    } else {
        PluginInstallSource::parse(&marker.spec)?
    };
    if let PluginInstallSource::LocalPath(path) = &source {
        let path = path.clone();
        let user_plugins_dir = user_plugins_dir.to_path_buf();
        let name = name.to_string();
        #[cfg(any(test, feature = "test-support"))]
        let env_scope = codewhale_runtime::test_support::env_scope_ticket();
        return tokio::task::spawn_blocking(move || {
            #[cfg(any(test, feature = "test-support"))]
            let _env_scope = codewhale_runtime::test_support::join_env_scope(env_scope);
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
                if installed_matches_staged(&staged_path, &target, &marker_body)? {
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
        let converted = match convert_dsh_off_runtime(package.clone(), network.clone()).await {
            Ok(converted) => converted,
            Err(err) => {
                if let Some(net_err) = err.downcast_ref::<NpmDepsNetworkError>() {
                    return Ok(match net_err {
                        NpmDepsNetworkError::NeedsApproval(host) => {
                            PluginUpdateResult::NeedsApproval((*host).to_string())
                        }
                        NpmDepsNetworkError::Denied(host) => {
                            PluginUpdateResult::NetworkDenied((*host).to_string())
                        }
                    });
                }
                return Err(err);
            }
        };
        let content_hash = {
            let manifest = crate::agent_plugin::resolve_manifest_path(&converted.bundle)
                .context("converted DSH bundle has no manifest")?;
            PluginManifest::validate_from_path(&manifest)
                .map_err(|error| {
                    anyhow::anyhow!("converted DSH bundle failed validation: {error}")
                })?
                .content_hash
        };
        let unchanged_from = (content_hash == marker.source_checksum())
            .then(|| (target.clone(), marker_body.clone()));
        let Some(outcome) = install_converted_dsh(
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
            unchanged_from,
        )
        .await?
        else {
            return Ok(PluginUpdateResult::NoChange);
        };
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
    let unchanged_from =
        (sha256_hex(&bytes) == marker.source_checksum()).then(|| (target.clone(), marker_body));

    let Some(outcome) = install_remote_bytes(
        &remote,
        bytes,
        &url,
        network,
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
        unchanged_from,
    )
    .await?
    else {
        return Ok(PluginUpdateResult::NoChange);
    };
    match outcome {
        PluginInstallOutcome::Installed(installed) => Ok(PluginUpdateResult::Updated(installed)),
        PluginInstallOutcome::NeedsApproval(host) => Ok(PluginUpdateResult::NeedsApproval(host)),
        PluginInstallOutcome::NetworkDenied(host) => Ok(PluginUpdateResult::NetworkDenied(host)),
    }
}

/// Check whether `bundle_root` carries a regular, non-link `.installed-from` file.
pub fn has_regular_install_marker(bundle_root: &Path) -> bool {
    let marker = bundle_root.join(INSTALLED_FROM_MARKER);
    let Ok(meta) = fs::symlink_metadata(&marker) else {
        return false;
    };
    if codewhale_util::path_identity::metadata_is_link_or_reparse(&meta) || !meta.is_file() {
        return false;
    }
    let Ok(file) = codewhale_util::bundle_file::open_bundle_file(&marker) else {
        return false;
    };
    file.metadata().is_ok_and(|opened| {
        opened.is_file() && !codewhale_util::path_identity::metadata_is_link_or_reparse(&opened)
    })
}

/// Remove a plugin installed via `/plugin install`.
///
/// Refuses to touch any directory that doesn't carry the `.installed-from`
/// marker — that's our cue that it's hand-placed and not ours to delete.
/// Callers must require the bundle to be disabled first (the mutation
/// controller does) and prune the registry state entry afterwards.
pub fn uninstall(name: &str, user_plugins_dir: &Path) -> Result<()> {
    let target = plugin_target_path(name, user_plugins_dir)?;
    let Ok(target_meta) = fs::symlink_metadata(&target) else {
        bail!("plugin '{name}' is not installed at {}", target.display());
    };
    ensure_target_within_plugins_dir(&target, user_plugins_dir)?;
    if codewhale_util::path_identity::metadata_is_link_or_reparse(&target_meta)
        || !target_meta.is_dir()
        || !has_regular_install_marker(&target)
    {
        return Err(PluginInstallError::NotInstalledHere(name.to_string()).into());
    }
    fs::remove_dir_all(&target)
        .with_context(|| format!("failed to remove {}", target.display()))?;
    Ok(())
}

#[derive(PartialEq, Eq)]
enum NormalizedSegment {
    Disk(u8),
    Unc(String, String),
    Device(String),
    Verbatim(String),
    Root,
    Normal(std::ffi::OsString),
}

fn normalized_segments(path: &Path) -> Option<Vec<NormalizedSegment>> {
    use std::path::{Component, Prefix};

    let mut segments = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                let seg = match prefix.kind() {
                    Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                        NormalizedSegment::Disk(drive.to_ascii_uppercase())
                    }
                    Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                        NormalizedSegment::Unc(
                            server.to_string_lossy().to_ascii_lowercase(),
                            share.to_string_lossy().to_ascii_lowercase(),
                        )
                    }
                    Prefix::DeviceNS(device) => {
                        NormalizedSegment::Device(device.to_string_lossy().to_ascii_lowercase())
                    }
                    Prefix::Verbatim(rest) => {
                        NormalizedSegment::Verbatim(rest.to_string_lossy().to_ascii_lowercase())
                    }
                };
                segments.push(seg);
            }
            Component::RootDir => segments.push(NormalizedSegment::Root),
            Component::CurDir => {}
            Component::ParentDir => return None,
            Component::Normal(part) => {
                #[cfg(windows)]
                let normalized =
                    std::ffi::OsString::from(part.to_string_lossy().to_ascii_lowercase());
                #[cfg(not(windows))]
                let normalized = part.to_os_string();
                segments.push(NormalizedSegment::Normal(normalized));
            }
        }
    }
    Some(segments)
}

#[must_use]
pub fn path_is_within(parent: &Path, child: &Path, allow_equal: bool) -> bool {
    let (Some(parent), Some(child)) = (normalized_segments(parent), normalized_segments(child))
    else {
        return false;
    };
    if child.len() < parent.len() || (!allow_equal && child.len() == parent.len()) {
        return false;
    }
    child[..parent.len()] == parent[..]
}

#[must_use]
pub fn clean_dir_is_strictly_inside(folder: &Path, project: &Path) -> bool {
    use std::ffi::OsStr;
    use std::path::Component;

    if !folder.is_absolute()
        || folder
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return false;
    }
    let (Ok(canon_folder), Ok(canon_project)) =
        (fs::canonicalize(folder), fs::canonicalize(project))
    else {
        return false;
    };
    if !path_is_within(&canon_project, &canon_folder, false) {
        return false;
    }
    let mut cursor = Some(folder);
    while let Some(current) = cursor {
        let Ok(canon_current) = fs::canonicalize(current) else {
            return false;
        };
        if !path_is_within(&canon_project, &canon_current, false) {
            return path_is_within(&canon_project, &canon_current, true);
        }
        let Ok(metadata) = fs::symlink_metadata(current) else {
            return false;
        };
        if codewhale_util::path_identity::metadata_is_link_or_reparse(&metadata)
            || !metadata.file_type().is_dir()
        {
            return false;
        }
        if current.file_name() == Some(OsStr::new(".git")) {
            return false;
        }
        cursor = current.parent();
    }
    false
}

#[must_use]
pub fn installed_by_the_agent(bundle_root: &Path, project: &Path) -> bool {
    use std::io::Read as _;

    let marker_path = bundle_root.join(INSTALLED_FROM_MARKER);
    let Ok(metadata) = fs::symlink_metadata(&marker_path) else {
        return false;
    };
    if codewhale_util::path_identity::metadata_is_link_or_reparse(&metadata)
        || !metadata.file_type().is_file()
    {
        return false;
    }
    let Ok(file) = codewhale_util::bundle_file::open_bundle_file(&marker_path) else {
        return false;
    };
    let mut body = String::new();
    if file.take(64 * 1024 + 1).read_to_string(&mut body).is_err() || body.len() > 64 * 1024 {
        return false;
    }
    let Ok(marker) = serde_json::from_str::<InstalledFromMarker>(&body) else {
        return false;
    };
    let Ok(source) = PluginInstallSource::parse(&marker.spec) else {
        return false;
    };
    match source {
        PluginInstallSource::LocalPath(folder) | PluginInstallSource::Dsh(folder) => {
            clean_dir_is_strictly_inside(&folder, project)
        }
        PluginInstallSource::Remote(_) => false,
    }
}
