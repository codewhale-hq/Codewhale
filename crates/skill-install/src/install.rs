//! Community-skill installer (#140).
//!
//! Pulls user-authored skills from GitHub or direct tarball URLs, validates them
//! against a path-traversal- and size-bounded extractor, and writes them into
//! `<skills_dir>/<name>/`. No backend service, no auto-execution: every install
//! is gated by the per-domain [`codewhale_net::network_policy::NetworkPolicy`] and
//! validation rejects any tarball entry that escapes the destination directory.
//!
//! Public surface:
//!
//! * [`InstallSource`] — `github:owner/repo`, raw URL, or curated registry
//!   name. Parsed from a single string with [`InstallSource::parse`].
//! * [`install`] / [`update`] / [`uninstall`] — async install, atomic update,
//!   and clean uninstall. All three preserve a `.installed-from` marker so the
//!   bundled `skill-creator` (which lacks the marker) is never touched.
//! * [`InstallOutcome`] — `Installed` / `NeedsApproval(host)` /
//!   `NetworkDenied(host)`. The `NeedsApproval` variant is returned without
//!   side effects so the caller (slash-command, runtime API, etc.) can route
//!   through its own approval flow.
//!
//! # Hard rules
//!
//! * Validation extracts to a temp directory first. The destination path is
//!   only created (via atomic rename) once the tarball clears every check.
//!   Half-installed skills can never appear on disk.
//! * Path traversal rejection covers both `..` segments and absolute paths.
//!   Symlinks inside the selected skill subtree are rejected — there's no use
//!   case for them in a SKILL.md bundle and they're a notorious foothold for
//!   escape. Multi-skill repository archives may contain unrelated symlinks
//!   outside that selected subtree; those entries are ignored and never
//!   extracted.
//! * Archive executable intent is preserved for the owner only; extraction
//!   never runs files or accepts an archive's trust/installation markers.
//!   `/skill trust <name>` writes the local `.trusted` receipt separately.
//! * Claude Code plugin archives that contain multiple skills are rejected with
//!   an explicit migration message. Codewhale can install individual
//!   `SKILL.md` bundles, including `.claude/skills/<name>/SKILL.md`, but it
//!   does not execute `plugin.json` plugin runtimes or custom command bundles.

use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use futures_util::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use codewhale_net::guard::{
    DnsPin, fetch_target_host, guarded_reqwest_client_builder, validate_fetch_destination,
};
use codewhale_net::network_policy::{Decision, NetworkPolicy, NetworkPolicyDecider, host_from_url};

const MAX_INSTALL_REDIRECTS: usize = 5;

/// Connect and total budgets for install and registry-sync HTTP requests.
fn install_http_timeouts() -> (std::time::Duration, std::time::Duration) {
    if cfg!(any(test, feature = "test-support")) {
        // Short enough that the stalled-server regression test finishes
        // quickly, long enough that happy-path tests never approach it.
        (
            std::time::Duration::from_millis(250),
            std::time::Duration::from_secs(2),
        )
    } else {
        (
            std::time::Duration::from_secs(10),
            std::time::Duration::from_secs(600),
        )
    }
}

fn reqwest_client(proxy: Option<reqwest::Proxy>, pin: DnsPin) -> reqwest::Client {
    let (connect_timeout, total_timeout) = install_http_timeouts();
    let mut builder = guarded_reqwest_client_builder()
        .redirect(reqwest::redirect::Policy::none())
        // The shared platform builder sets no timeouts; without a bound, a
        // connection that opens but stalls (dead proxy, black-holed route)
        // hangs installs and registry sync forever. Connect is bounded tightly;
        // the total budget is generous for 5 MiB tarballs on slow links
        // (registry sync fans out `SYNC_REGISTRY_CONCURRENCY` of these in
        // parallel).
        .connect_timeout(connect_timeout)
        .timeout(total_timeout);
    if let Some(proxy) = proxy {
        builder = builder.proxy(proxy);
    } else if let Some((host, ip)) = pin {
        builder = builder.resolve(&host, std::net::SocketAddr::new(ip, 0));
    }
    builder.build().expect("build platform HTTP client")
}

async fn guarded_get(
    url: &str,
    network: &NetworkPolicy,
    if_none_match: Option<&str>,
) -> Result<reqwest::Response> {
    let decider = NetworkPolicyDecider::new(network.clone(), None);
    let mut current = reqwest::Url::parse(url).with_context(|| format!("invalid url: {url}"))?;
    let mut initial_pin: Option<DnsPin> = (cfg!(any(test, feature = "test-support"))
        && current.host_str() == Some("127.0.0.1"))
    .then_some(None);
    for _ in 0..=MAX_INSTALL_REDIRECTS {
        let host = fetch_target_host(&current)?;
        if matches!(network.decide(&host), Decision::Deny) {
            bail!("network policy denies {host}");
        }
        let proxy = if initial_pin.is_some() {
            None
        } else {
            codewhale_net::proxy::configured_mcp_proxy(&current, false, |key| std::env::var(key))?
        };
        let pin = match initial_pin.take() {
            Some(pin) => pin,
            None => {
                validate_fetch_destination(host, Some(&decider), "skill_install", proxy.is_none())
                    .await?
            }
        };
        let mut request = reqwest_client(proxy, pin).get(current.clone());
        if let Some(etag) = if_none_match {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("failed to GET {current}"))?;
        if !response.status().is_redirection() {
            return Ok(response);
        }
        let Some(location) = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
        else {
            return Ok(response);
        };
        let next = response
            .url()
            .join(location)
            .with_context(|| format!("invalid redirect location from {current}"))?;
        if current.scheme() == "https" && next.scheme() != "https" {
            bail!("redirect from {current} would downgrade HTTPS");
        }
        current = next;
    }
    bail!("{url} exceeded {MAX_INSTALL_REDIRECTS} redirects")
}

/// Cache directory for registry-synced skills.
///
/// Lives at `~/.codewhale/cache/skills/` so it's separate from user-installed
/// skills and can be blown away without losing anything irreplaceable.
/// A missing home has no cache destination; callers must refuse the sync.
pub fn default_cache_skills_dir() -> Option<PathBuf> {
    codewhale_paths::user_home().map(|home| home.join(".codewhale").join("cache").join("skills"))
}

/// Default registry. Falls back to a community-curated `index.json` hosted on
/// GitHub raw; users can override via `[skills] registry_url` in config.toml.
pub const DEFAULT_REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/Hmbown/deepseek-skills/main/index.json";

/// Default per-skill size cap (5 MiB). Honored at unpack time so a malicious
/// gzip bomb can't blow up RAM.
pub const DEFAULT_MAX_SIZE_BYTES: u64 = 5 * 1024 * 1024;
const SYNC_REGISTRY_CONCURRENCY: usize = 8;
/// Upper bound on a registry index document.
const MAX_REGISTRY_BYTES: usize = 4 * 1024 * 1024;

/// File written under each installed skill so [`update`] / [`uninstall`] can
/// recover the original [`InstallSource`] without re-parsing user input.
pub const INSTALLED_FROM_MARKER: &str = ".installed-from";

/// File written under each trusted skill. Currently advisory (the install path
/// never auto-runs anything) — the runtime tool-invocation gate consults this
/// marker before executing scripts that ship with the skill.
pub const TRUSTED_MARKER: &str = ".trusted";
const RESERVED_ROOT_METADATA: [&str; 3] = [
    INSTALLED_FROM_MARKER,
    TRUSTED_MARKER,
    ".system-installed-version",
];

/// Installer-owned root metadata must never arrive from a remote package.
/// Descendants of a reserved root directory are excluded too.
pub(super) fn is_reserved_root_metadata(relative: &Path) -> bool {
    let first = relative
        .components()
        .find(|component| !matches!(component, Component::CurDir));
    matches!(first, Some(Component::Normal(name)) if name.to_str().is_some_and(|name|
        RESERVED_ROOT_METADATA.iter().any(|reserved| name.eq_ignore_ascii_case(reserved))))
}

// ─────────────────────────────────────────────────────────────────────────────
// Source parsing
// ─────────────────────────────────────────────────────────────────────────────

/// Where a skill is being installed from. See [`InstallSource::parse`] for the
/// accepted spec syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallSource {
    /// `github:owner/repo`. Resolved to
    /// `https://github.com/<owner>/<repo>/archive/refs/heads/main.tar.gz`
    /// with a `master.tar.gz` fallback on 404.
    GitHubRepo(String),
    /// Raw `http(s)://…` tarball URL. Used as-is.
    DirectUrl(String),
    /// Curated registry lookup key. Looked up via the configured `registry_url`.
    Registry(String),
}

impl InstallSource {
    /// Parse a user-supplied spec. Empty / whitespace-only input is rejected.
    ///
    /// * `github:owner/repo` → [`InstallSource::GitHubRepo`]
    /// * `https://github.com/owner/repo[.git]` (no path past the repo) →
    ///   [`InstallSource::GitHubRepo`]
    /// * any other `http://` or `https://` prefix → [`InstallSource::DirectUrl`]
    /// * anything else → [`InstallSource::Registry`]
    pub fn parse(spec: &str) -> Result<Self> {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            bail!("install source must not be empty");
        }
        if let Some(rest) = trimmed.strip_prefix("github:") {
            let rest = rest.trim();
            // Reject obviously bogus values up front. We intentionally accept
            // case-insensitive owner/repo so `github:Hmbown/Foo` works.
            let (owner, repo) = rest.split_once('/').with_context(|| {
                format!("github source must be 'github:owner/repo' (got {spec})")
            })?;
            let owner = owner.trim();
            let repo = repo.trim().trim_end_matches('/');
            if owner.is_empty() || repo.is_empty() {
                bail!("github source must be 'github:owner/repo' (got {spec})");
            }
            if owner.contains('/') || repo.contains('/') {
                bail!("github source must be 'github:owner/repo' (got {spec})");
            }
            return Ok(Self::GitHubRepo(format!("{owner}/{repo}")));
        }
        if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
            if let Some(repo) = parse_github_browser_url(trimmed) {
                return Ok(Self::GitHubRepo(repo));
            }
            return Ok(Self::DirectUrl(trimmed.to_string()));
        }
        Ok(Self::Registry(trimmed.to_string()))
    }
}

/// Detect bare `https://github.com/<owner>/<repo>` URLs (with or without a
/// trailing `.git`) and return `owner/repo`. Returns `None` for any URL that
/// already points at a specific archive / blob / tree path — those are real
/// direct URLs and the caller fetches them as-is.
fn parse_github_browser_url(url: &str) -> Option<String> {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, rest) = after_scheme.split_once('/')?;
    if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("www.github.com") {
        return None;
    }
    let trimmed = rest.trim_end_matches('/');
    let mut parts = trimmed.splitn(3, '/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim().trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    // If there is a third segment, the URL points at a sub-resource
    // (`/archive/...`, `/blob/...`, `/tree/...`). Treat that as a real direct
    // URL — the user explicitly wants whatever lives at that path.
    if parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

// ─────────────────────────────────────────────────────────────────────────────
// Outcome / result types
// ─────────────────────────────────────────────────────────────────────────────

/// Outcome of an install attempt.
#[derive(Debug)]
pub enum InstallOutcome {
    /// The skill was installed (or already present and idempotent).
    Installed(InstalledSkill),
    /// The host requires user approval before the install can proceed. The
    /// caller should surface this through whatever approval pathway it has and
    /// retry once approved (typically by adding the host to the policy's
    /// allow list).
    NeedsApproval(String),
    /// The host is denied by network policy. The install is aborted.
    NetworkDenied(String),
}

/// Metadata for a successfully installed skill.
#[derive(Debug, Clone)]
pub struct InstalledSkill {
    /// Skill name (taken from SKILL.md frontmatter).
    pub name: String,
    /// Final on-disk path: `<skills_dir>/<name>/`.
    pub path: PathBuf,
    /// SHA-256 over the downloaded tarball bytes. Used by [`update`] to detect
    /// upstream changes without re-extracting; also surfaced for telemetry /
    /// future signature-verification work.
    pub source_checksum: String,
}

/// Result of an [`update`] call.
#[derive(Debug)]
pub enum UpdateResult {
    /// Upstream tarball is byte-identical to the on-disk checksum; no action.
    NoChange,
    /// Upstream changed and the on-disk install was atomically replaced.
    Updated(InstalledSkill),
    /// Network policy short-circuited the update. Same semantics as
    /// [`InstallOutcome::NeedsApproval`].
    NeedsApproval(String),
    /// Network policy denied the update.
    NetworkDenied(String),
}

/// Errors that can happen during install. Most variants are flattened into
/// `anyhow::Error` at the public boundary; this enum is used internally so
/// tests can pattern-match without parsing strings.
#[derive(Debug, Error)]
pub enum InstallError {
    #[error("entry escapes destination directory: {0}")]
    PathTraversal(String),
    #[error("entry is too large; uncompressed total would exceed {limit} bytes")]
    OversizedTarball { limit: u64 },
    #[error("missing SKILL.md in archive")]
    MissingSkillMd,
    #[error("symlinks are not allowed in skill tarballs")]
    SymlinkRejected,
    #[error(
        "Claude Code plugin archive contains multiple SKILL.md entries; Codewhale installs one SKILL.md bundle at a time and does not run plugin.json/custom-command runtimes. Install or migrate an individual skills/<name> directory instead"
    )]
    ClaudePluginBundle,
    #[error("skill '{0}' is already installed; use update or remove it first")]
    AlreadyInstalled(String),
    #[error("skill '{0}' was not installed via /skill install (no .installed-from marker)")]
    NotInstalledHere(String),
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Install a community skill into `skills_dir`.
///
/// Steps:
///
/// 1. Resolve `source` to one or more candidate URLs (GitHub adds a
///    `master` fallback after `main`).
/// 2. Consult `network` for the host. `Allow` proceeds; `Deny` returns
///    [`InstallOutcome::NetworkDenied`]; `Prompt` returns
///    [`InstallOutcome::NeedsApproval`] without touching disk.
/// 3. Stream the tarball into a tempfile (capped at `max_size`).
/// 4. Validate the archive (path-traversal, size, no symlinks in the selected
///    skill subtree, SKILL.md present with required frontmatter fields) into a
///    sibling `<name>.tmp/` directory.
/// 5. Atomic-rename `<name>.tmp/` → `<name>/`.
/// 6. Write `.installed-from` and return [`InstalledSkill`].
///
/// `update = false` rejects an existing destination. Pass `update = true`
/// from [`update`] to allow replacement.
///
/// Convenience wrapper over [`install_with_registry`] that uses the bundled
/// [`DEFAULT_REGISTRY_URL`]. Public for downstream consumers (tests, runtime
/// API) even though the slash-command path always goes through
/// [`install_with_registry`] so the user's configured registry wins.
pub async fn install(
    source: InstallSource,
    skills_dir: &Path,
    max_size: u64,
    network: &NetworkPolicy,
    update: bool,
) -> Result<InstallOutcome> {
    install_with_registry(
        source,
        skills_dir,
        max_size,
        network,
        update,
        DEFAULT_REGISTRY_URL,
    )
    .await
}

/// Same as [`install`] but lets the caller override the registry URL. Useful
/// for tests; the slash-command path always uses the configured registry.
pub async fn install_with_registry(
    source: InstallSource,
    skills_dir: &Path,
    max_size: u64,
    network: &NetworkPolicy,
    update: bool,
    registry_url: &str,
) -> Result<InstallOutcome> {
    let urls = candidate_urls(&source, network, registry_url).await?;
    let urls = match urls {
        UrlResolution::Resolved(urls) => urls,
        UrlResolution::NeedsApproval(host) => return Ok(InstallOutcome::NeedsApproval(host)),
        UrlResolution::Denied(host) => return Ok(InstallOutcome::NetworkDenied(host)),
    };

    // Try each URL in order — GitHub returns 404 for `main` on master-only
    // repos, and we don't want to fail the install on that.
    let (bytes, source_url) = match download_first_success(&urls, network, max_size).await? {
        DownloadOutcome::Bytes { bytes, url } => (bytes, url),
        DownloadOutcome::NeedsApproval(host) => return Ok(InstallOutcome::NeedsApproval(host)),
        DownloadOutcome::Denied(host) => return Ok(InstallOutcome::NetworkDenied(host)),
    };

    // Compute a checksum before unpacking so [`update`] can detect upstream
    // no-op changes without redoing the extract.
    let checksum = sha256_hex(&bytes);

    let staged = stage_tarball(&bytes, skills_dir, max_size)?;

    // Move the staged dir into its final location. If `update` is set and the
    // destination exists, replace it; otherwise reject.
    // Keep any backup until content digest + marker write succeed so a failed
    // finalize can restore the previous install.
    let final_path = skills_dir.join(&staged.skill_name);
    let mut backup_path: Option<PathBuf> = None;
    if tokio::fs::try_exists(&final_path).await.unwrap_or(false) {
        if !update {
            // Clean up the staging dir before returning the error.
            let _ = tokio::fs::remove_dir_all(&staged.staged_path).await;
            return Err(InstallError::AlreadyInstalled(staged.skill_name).into());
        }
        // Same ownership gate as plugins/install/place.rs: an update may only
        // replace a tree this installer created. The tarball's top-level name
        // is not proof of ownership — without the marker we would delete a
        // user-authored or system skill that happened to share the name.
        if let Err(err) = reject_unmarked_update(&final_path, &staged.skill_name) {
            let _ = tokio::fs::remove_dir_all(&staged.staged_path).await;
            return Err(err.into());
        }
        let backup = skills_dir.join(format!("{}.bak", staged.skill_name));
        if tokio::fs::try_exists(&backup).await.unwrap_or(false) {
            tokio::fs::remove_dir_all(&backup).await.ok();
        }
        tokio::fs::rename(&final_path, &backup)
            .await
            .with_context(|| {
                format!(
                    "failed to backup existing skill at {}",
                    final_path.display()
                )
            })?;
        if let Err(err) = tokio::fs::rename(&staged.staged_path, &final_path).await {
            tokio::fs::rename(&backup, &final_path).await.ok();
            return Err(err).context("failed to install staged skill");
        }
        backup_path = Some(backup);
    } else {
        if let Some(parent) = final_path.parent() {
            tokio::fs::create_dir_all(parent).await.with_context(|| {
                format!("failed to create skills directory {}", parent.display())
            })?;
        }
        tokio::fs::rename(&staged.staged_path, &final_path)
            .await
            .context("failed to install staged skill")?;
    }

    // Write the marker last so a partial install never leaves a stale
    // .installed-from on disk. Prefer v2 with package content digest.
    let spec = source_spec_string(&source);
    let content_digest = match crate::package_digest::compute_package_digest(&final_path) {
        Ok(digest) => digest,
        Err(err) => {
            let _ = tokio::fs::remove_dir_all(&final_path).await;
            if let Some(backup) = backup_path.take() {
                let _ = tokio::fs::rename(&backup, &final_path).await;
            }
            return Err(anyhow::anyhow!(
                "installed package failed content digest validation: {err}"
            ));
        }
    };
    if let Err(err) = write_installed_from_v2(
        &final_path,
        &spec,
        Some(&source_url),
        &checksum,
        &content_digest,
        &staged.skill_name,
    ) {
        let _ = tokio::fs::remove_dir_all(&final_path).await;
        if let Some(backup) = backup_path.take() {
            let _ = tokio::fs::rename(&backup, &final_path).await;
        }
        return Err(err);
    }
    if let Some(backup) = backup_path {
        tokio::fs::remove_dir_all(&backup).await.ok();
    }

    Ok(InstallOutcome::Installed(InstalledSkill {
        name: staged.skill_name,
        path: final_path,
        source_checksum: checksum,
    }))
}

/// Re-fetch a previously installed skill and replace it on disk if the
/// upstream tarball changed.
///
/// Reads `.installed-from` to recover the original [`InstallSource`], so
/// a skill installed via `/skill install github:foo/bar` can be updated via
/// `/skill update bar` without the user re-typing the spec.
///
/// Convenience wrapper over [`update_with_registry`].
pub async fn update(
    name: &str,
    skills_dir: &Path,
    max_size: u64,
    network: &NetworkPolicy,
) -> Result<UpdateResult> {
    update_with_registry(name, skills_dir, max_size, network, DEFAULT_REGISTRY_URL).await
}

/// Same as [`update`] but lets the caller override the registry URL.
pub async fn update_with_registry(
    name: &str,
    skills_dir: &Path,
    max_size: u64,
    network: &NetworkPolicy,
    registry_url: &str,
) -> Result<UpdateResult> {
    let target = skill_target_path(name, skills_dir)?;
    if tokio::fs::try_exists(&target).await.unwrap_or(false) {
        ensure_target_within_skills_dir(&target, skills_dir)?;
    }
    let marker_path = target.join(INSTALLED_FROM_MARKER);
    if !tokio::fs::try_exists(&marker_path).await.unwrap_or(false) {
        return Err(InstallError::NotInstalledHere(name.to_string()).into());
    }
    let marker_body = tokio::fs::read_to_string(&marker_path)
        .await
        .with_context(|| format!("failed to read {}", marker_path.display()))?;
    let marker: InstalledFromMarker = serde_json::from_str(&marker_body)
        .with_context(|| format!("malformed {INSTALLED_FROM_MARKER} for {name}"))?;
    if !is_registry_updatable_spec(&marker.spec) {
        bail!(
            "skill '{name}' was imported locally (spec '{}') and cannot be updated from a registry; \
             re-import or remove it first",
            marker.spec
        );
    }

    // Re-resolve the URL, taking the existing checksum as a short-circuit hint:
    // we still hit the network so the user gets a useful "no upstream change"
    // signal, but we skip the unpack step if the bytes match.
    let source = InstallSource::parse(&marker.spec)?;
    let urls = match candidate_urls(&source, network, registry_url).await? {
        UrlResolution::Resolved(urls) => urls,
        UrlResolution::NeedsApproval(host) => return Ok(UpdateResult::NeedsApproval(host)),
        UrlResolution::Denied(host) => return Ok(UpdateResult::NetworkDenied(host)),
    };
    let (bytes, _url) = match download_first_success(&urls, network, max_size).await? {
        DownloadOutcome::Bytes { bytes, url } => (bytes, url),
        DownloadOutcome::NeedsApproval(host) => return Ok(UpdateResult::NeedsApproval(host)),
        DownloadOutcome::Denied(host) => return Ok(UpdateResult::NetworkDenied(host)),
    };

    let checksum = sha256_hex(&bytes);
    if checksum == marker.source_checksum() {
        return Ok(UpdateResult::NoChange);
    }

    // Bytes changed — fall back to the regular install path with `update = true`
    // so we get the same atomic-replace semantics. Content updates must not
    // inherit a previous trust marker. The shared extractor excludes archive
    // markers on every install, including an update of an untrusted package.
    let outcome =
        install_with_registry(source, skills_dir, max_size, network, true, registry_url).await?;
    match outcome {
        InstallOutcome::Installed(installed) => Ok(UpdateResult::Updated(installed)),
        InstallOutcome::NeedsApproval(host) => Ok(UpdateResult::NeedsApproval(host)),
        InstallOutcome::NetworkDenied(host) => Ok(UpdateResult::NetworkDenied(host)),
    }
}

/// Remove a community-installed skill.
///
/// Refuses to touch any directory that doesn't carry the `.installed-from`
/// marker — that's our cue that it's user-owned and not a system skill.
pub fn uninstall(name: &str, skills_dir: &Path) -> Result<()> {
    let target = skill_target_path(name, skills_dir)?;
    if !target.exists() {
        bail!("skill '{name}' is not installed at {}", target.display());
    }
    ensure_target_within_skills_dir(&target, skills_dir)?;
    if !target.join(INSTALLED_FROM_MARKER).exists() {
        return Err(InstallError::NotInstalledHere(name.to_string()).into());
    }
    fs::remove_dir_all(&target)
        .with_context(|| format!("failed to remove {}", target.display()))?;
    Ok(())
}

/// Mark a community-installed skill as trusted, binding the marker to the
/// current package content digest (schema v2).
///
/// Refuses to mark system skills (no `.installed-from`) so the bundled
/// `skill-creator` doesn't accidentally inherit elevated tool privileges.
#[cfg(any(test, feature = "test-support"))]
pub fn trust(name: &str, skills_dir: &Path) -> Result<()> {
    let target = skill_target_path(name, skills_dir)?;
    if !target.exists() {
        bail!("skill '{name}' is not installed at {}", target.display());
    }
    ensure_target_within_skills_dir(&target, skills_dir)?;
    if !target.join(INSTALLED_FROM_MARKER).exists() {
        return Err(InstallError::NotInstalledHere(name.to_string()).into());
    }
    let content_digest = crate::package_digest::compute_package_digest(&target)
        .with_context(|| format!("cannot compute content digest for {}", target.display()))?;
    write_trust_v2(&target, &content_digest)?;
    Ok(())
}

/// Fetch the curated registry and return the parsed entries.
///
/// Honours `network` (skipping the call entirely on Deny / Prompt).
pub async fn fetch_registry(
    network: &NetworkPolicy,
    registry_url: &str,
) -> Result<RegistryFetchResult> {
    let host = match host_from_url(registry_url) {
        Some(host) => host,
        None => bail!("invalid registry url: {registry_url}"),
    };
    match network.decide(&host) {
        Decision::Allow => {}
        Decision::Deny => return Ok(RegistryFetchResult::Denied(host)),
        Decision::Prompt => return Ok(RegistryFetchResult::NeedsApproval(host)),
    }
    let response = guarded_get(registry_url, network, None)
        .await
        .with_context(|| format!("failed to fetch registry {registry_url}"))?
        .error_for_status()
        .with_context(|| format!("registry {registry_url} returned an error status"))?;
    let body = codewhale_util::utils::read_response_body_capped(response, MAX_REGISTRY_BYTES)
        .await
        .with_context(|| format!("failed to read registry body from {registry_url}"))?;
    let parsed: RegistryDocument = serde_json::from_slice(&body)
        .with_context(|| format!("failed to parse registry json from {registry_url}"))?;
    Ok(RegistryFetchResult::Loaded(parsed))
}

// ─────────────────────────────────────────────────────────────────────────────
// Registry sync (issue #433)
// ─────────────────────────────────────────────────────────────────────────────

/// Outcome of a single skill entry during [`sync_registry`].
#[derive(Debug, Clone)]
pub enum SkillSyncOutcome {
    /// Skill downloaded and written to the cache directory.
    Downloaded { name: String, path: PathBuf },
    /// Cached bytes match the upstream ETag / SHA-256; nothing written.
    Fresh { name: String },
    /// Skill download failed; the error is non-fatal so the sync continues.
    Failed { name: String, reason: String },
    /// Network policy blocked the download host.
    Denied { name: String, host: String },
    /// Network policy requires user approval for the download host.
    NeedsApproval { name: String, host: String },
}

/// Overall result of [`sync_registry`].
#[derive(Debug)]
pub enum SyncResult {
    /// Sync completed. `outcomes` contains one entry per skill in the index.
    Done { outcomes: Vec<SkillSyncOutcome> },
    /// The registry fetch was blocked by network policy.
    RegistryDenied(String),
    /// The registry fetch requires user approval.
    RegistryNeedsApproval(String),
}

/// Freshness metadata written alongside each cached skill so subsequent syncs
/// can skip unchanged content.
#[derive(Debug, Serialize, Deserialize)]
struct CacheMeta {
    /// ETag returned by the server for the primary asset, if any.
    #[serde(default)]
    etag: Option<String>,
    /// SHA-256 hex digest of the downloaded bytes.
    sha256: String,
    /// Source URL the asset was fetched from.
    url: String,
}

/// Sync the remote registry to the local cache.
///
/// For every skill listed in `index.json` this function:
///
/// 1. Resolves the download URL (same logic as `install`).
/// 2. Checks the cached `CacheMeta` (etag + sha256) for freshness; skips
///    the download if unchanged.
/// 3. Downloads SKILL.md (and any companion files if the source is a tarball)
///    into `<cache_dir>/<name>/`.
/// 4. Writes updated `CacheMeta` so the next sync is fast.
///
/// Failures per-skill are non-fatal: [`SkillSyncOutcome::Failed`] is recorded
/// and the sync continues. The caller decides how to surface per-skill errors.
pub async fn sync_registry(
    network: &NetworkPolicy,
    registry_url: &str,
    cache_dir: &Path,
    max_size: u64,
) -> Result<SyncResult> {
    let doc = match fetch_registry(network, registry_url).await? {
        RegistryFetchResult::Loaded(doc) => doc,
        RegistryFetchResult::Denied(host) => return Ok(SyncResult::RegistryDenied(host)),
        RegistryFetchResult::NeedsApproval(host) => {
            return Ok(SyncResult::RegistryNeedsApproval(host));
        }
    };

    let outcomes = stream::iter(doc.skills.iter())
        .map(|(name, entry)| sync_one_skill(name, entry, network, cache_dir, max_size))
        .buffered(SYNC_REGISTRY_CONCURRENCY)
        .collect()
        .await;

    Ok(SyncResult::Done { outcomes })
}

/// Sync a single skill entry from the registry into the cache directory.
async fn sync_one_skill(
    name: &str,
    entry: &RegistryEntry,
    network: &NetworkPolicy,
    cache_dir: &Path,
    max_size: u64,
) -> SkillSyncOutcome {
    // The registry key names the cache directory joined below; it gets the
    // same single-segment check as an installed skill name.
    if let Err(err) = validate_skill_name_segment(name) {
        return SkillSyncOutcome::Failed {
            name: name.to_string(),
            reason: format!("{err:#}"),
        };
    }
    // Resolve the source to a concrete URL list.
    let source = match InstallSource::parse(&entry.source) {
        Ok(s) => s,
        Err(err) => {
            return SkillSyncOutcome::Failed {
                name: name.to_string(),
                reason: format!("invalid source spec '{}': {err:#}", entry.source),
            };
        }
    };

    // Registry sources in index.json must not point back at another registry.
    if matches!(source, InstallSource::Registry(_)) {
        return SkillSyncOutcome::Failed {
            name: name.to_string(),
            reason: format!("registry entry for '{name}' must not point to another registry entry"),
        };
    }

    let urls = match &source {
        InstallSource::GitHubRepo(repo) => vec![
            format!("https://github.com/{repo}/archive/refs/heads/main.tar.gz"),
            format!("https://github.com/{repo}/archive/refs/heads/master.tar.gz"),
        ],
        InstallSource::DirectUrl(url) => vec![url.clone()],
        InstallSource::Registry(_) => unreachable!("guarded above"),
    };

    // Check the first downloadable URL against any cached meta.
    let skill_cache_dir = cache_dir.join(name);
    let meta_path = skill_cache_dir.join(".cache-meta.json");

    // Try each candidate URL in order.
    for url in &urls {
        let host = match host_from_url(url) {
            Some(h) => h,
            None => continue,
        };
        match network.decide(&host) {
            Decision::Allow => {}
            Decision::Deny => {
                return SkillSyncOutcome::Denied {
                    name: name.to_string(),
                    host,
                };
            }
            Decision::Prompt => {
                return SkillSyncOutcome::NeedsApproval {
                    name: name.to_string(),
                    host,
                };
            }
        }

        // Perform a HEAD request (or conditional GET) for freshness. We use a
        // simple GET with If-None-Match when we have an ETag, falling back to
        // an unconditional GET for servers that don't support ETags.
        let existing_meta: Option<CacheMeta> = tokio::fs::read_to_string(&meta_path)
            .await
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());

        // Build the request — add If-None-Match if we have a cached ETag.
        let cached_etag = existing_meta.as_ref().and_then(|meta| meta.etag.as_deref());

        let resp = match guarded_get(url, network, cached_etag).await {
            Ok(r) => r,
            Err(err) => {
                // Network error — try the next candidate URL.
                let _ = err;
                continue;
            }
        };

        let status = resp.status();

        // 304 Not Modified: cached copy is still fresh.
        if status == reqwest::StatusCode::NOT_MODIFIED {
            return SkillSyncOutcome::Fresh {
                name: name.to_string(),
            };
        }

        if status == reqwest::StatusCode::NOT_FOUND {
            // Try next URL (main → master fallback).
            continue;
        }

        if !status.is_success() {
            return SkillSyncOutcome::Failed {
                name: name.to_string(),
                reason: format!("GET {url} returned HTTP {status}"),
            };
        }

        // Capture ETag before consuming the response body.
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let compressed_cap = compressed_download_cap(max_size);
        let bytes = match codewhale_util::utils::read_response_body_capped(resp, compressed_cap)
            .await
        {
            Ok(b) => b,
            Err(err) => {
                return SkillSyncOutcome::Failed {
                    name: name.to_string(),
                    reason: format!(
                        "failed to read body from {url} (compressed size cap {compressed_cap} bytes): {err:#}"
                    ),
                };
            }
        };

        // Compute SHA-256 of the downloaded bytes.
        let sha256 = sha256_hex(&bytes);

        // Short-circuit: if the hash matches the cached one, we're fresh even
        // without a 304 (some CDNs strip ETags on redirects).
        if let Some(ref meta) = existing_meta
            && meta.sha256 == sha256
            && meta.url == *url
        {
            return SkillSyncOutcome::Fresh {
                name: name.to_string(),
            };
        }

        // Determine whether this is a tarball or a plain SKILL.md.
        // Heuristic: the URL ends with `.tar.gz` or `.tgz`, or the content
        // starts with the gzip magic bytes (0x1f 0x8b).
        let is_tarball =
            url.ends_with(".tar.gz") || url.ends_with(".tgz") || bytes.starts_with(&[0x1f, 0x8b]);

        let final_path: PathBuf = if is_tarball {
            // Extract into a temp staging dir, then rename atomically.
            let staged = match stage_tarball(&bytes, cache_dir, max_size) {
                Ok(s) => s,
                Err(err) => {
                    return SkillSyncOutcome::Failed {
                        name: name.to_string(),
                        reason: format!("tarball extraction failed: {err:#}"),
                    };
                }
            };
            // Move staged dir into its final location, replacing any prior cache.
            let dest = cache_dir.join(name);
            if tokio::fs::try_exists(&dest).await.unwrap_or(false) {
                let _ = tokio::fs::remove_dir_all(&dest).await;
            }
            if let Err(err) = tokio::fs::rename(&staged.staged_path, &dest).await {
                let _ = tokio::fs::remove_dir_all(&staged.staged_path).await;
                return SkillSyncOutcome::Failed {
                    name: name.to_string(),
                    reason: format!("failed to move staged skill into cache: {err:#}"),
                };
            }
            dest
        } else {
            // Plain SKILL.md (or other companion text file). Write directly.
            if let Err(err) = tokio::fs::create_dir_all(&skill_cache_dir).await {
                return SkillSyncOutcome::Failed {
                    name: name.to_string(),
                    reason: format!("failed to create cache dir: {err:#}"),
                };
            }
            let skill_md_path = skill_cache_dir.join("SKILL.md");
            if let Err(err) = tokio::fs::write(&skill_md_path, &bytes).await {
                return SkillSyncOutcome::Failed {
                    name: name.to_string(),
                    reason: format!("failed to write SKILL.md to cache: {err:#}"),
                };
            }
            skill_cache_dir.clone()
        };

        // Write the updated freshness metadata.
        let meta = CacheMeta {
            etag,
            sha256,
            url: url.clone(),
        };
        let meta_json = serde_json::to_string(&meta).unwrap_or_default();
        let _ = tokio::fs::write(final_path.join(".cache-meta.json"), meta_json).await;

        return SkillSyncOutcome::Downloaded {
            name: name.to_string(),
            path: final_path,
        };
    }

    // All candidate URLs exhausted without a successful response.
    SkillSyncOutcome::Failed {
        name: name.to_string(),
        reason: format!(
            "all candidate URLs for '{}' failed or were not found",
            entry.source
        ),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct InstalledFromMarker {
    pub spec: String,
    /// v1 download checksum field.
    #[serde(default)]
    checksum: String,
    #[serde(default)]
    source_checksum: Option<String>,
    #[serde(default)]
    #[expect(dead_code)]
    schema_version: Option<u32>,
    #[serde(default)]
    #[expect(dead_code)]
    content_digest: Option<String>,
}

impl InstalledFromMarker {
    pub fn source_checksum(&self) -> &str {
        self.source_checksum
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(self.checksum.as_str())
    }
}

/// Remote/registry update is only valid for install specs that are not local imports.
#[must_use]
pub fn is_registry_updatable_spec(spec: &str) -> bool {
    let spec = spec.trim();
    !spec.is_empty() && !spec.starts_with("import:")
}

/// Write schema-v2 `.installed-from` metadata (last step of a successful install).
pub fn write_installed_from_v2(
    skill_dir: &Path,
    spec: &str,
    url: Option<&str>,
    source_checksum: &str,
    content_digest: &str,
    installed_name: &str,
) -> Result<()> {
    let body = serde_json::json!({
        "schema_version": 2,
        "spec": spec,
        "url": url,
        "source_checksum": source_checksum,
        "content_digest": content_digest,
        "installed_name": installed_name,
        "registry_version": null,
    });
    fs::write(skill_dir.join(INSTALLED_FROM_MARKER), body.to_string()).with_context(|| {
        format!(
            "failed to write {} for {}",
            INSTALLED_FROM_MARKER,
            skill_dir.display()
        )
    })?;
    Ok(())
}

/// Write schema-v2 `.trusted` bound to a package content digest.
pub fn write_trust_v2(skill_dir: &Path, content_digest: &str) -> Result<()> {
    let body = serde_json::json!({
        "schema_version": 2,
        "content_digest": content_digest,
    });
    fs::write(skill_dir.join(TRUSTED_MARKER), body.to_string()).with_context(|| {
        format!(
            "failed to write {} for {}",
            TRUSTED_MARKER,
            skill_dir.display()
        )
    })?;
    Ok(())
}

/// Curated-registry document. The shape is intentionally minimal so adding
/// optional metadata later (homepage, version, signature) is forward-compatible.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryDocument {
    /// Map of skill name → entry.
    #[serde(default)]
    pub skills: std::collections::BTreeMap<String, RegistryEntry>,
}

/// One row in the curated registry. Descriptive matching metadata is optional
/// so old indices keep parsing and new registries can publish it gradually.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryEntry {
    /// Source spec (e.g. `github:owner/repo`).
    pub source: String,
    /// Optional human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Task phrases that should rank this skill above description fallbacks.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Relevant web domains, optionally written as full URLs by the registry.
    #[serde(default)]
    pub domains: Vec<String>,
}

/// Successful registry fetch result. Same shape as [`InstallOutcome`] for the
/// network-policy outcomes so the caller can drop directly into approval flow.
#[derive(Debug)]
pub enum RegistryFetchResult {
    Loaded(RegistryDocument),
    NeedsApproval(String),
    Denied(String),
}

enum UrlResolution {
    Resolved(Vec<String>),
    NeedsApproval(String),
    Denied(String),
}

enum DownloadOutcome {
    Bytes { bytes: Vec<u8>, url: String },
    NeedsApproval(String),
    Denied(String),
}

/// Outcome of [`fetch_tarball`], shared with the plugin installer (#5182) so
/// both route `Prompt`/`Deny` hosts through their own approval flows instead
/// of growing a second download path.
#[derive(Debug)]
pub enum FetchOutcome {
    Bytes { bytes: Vec<u8>, url: String },
    NeedsApproval(String),
    Denied(String),
}

/// Resolve a *remote* [`InstallSource`] (GitHub repo or direct tarball URL)
/// and download the first reachable candidate under the network policy.
/// Registry sources are rejected: skill registry resolution stays inside
/// `candidate_urls`, and the plugin install on-ramp has no registry index.
pub async fn fetch_tarball(
    source: &InstallSource,
    network: &NetworkPolicy,
    max_size: u64,
) -> Result<FetchOutcome> {
    let urls = match source {
        InstallSource::GitHubRepo(repo) => vec![
            format!("https://github.com/{repo}/archive/refs/heads/main.tar.gz"),
            format!("https://github.com/{repo}/archive/refs/heads/master.tar.gz"),
        ],
        InstallSource::DirectUrl(url) => vec![url.clone()],
        InstallSource::Registry(name) => {
            bail!("registry source '{name}' cannot be fetched as a plain tarball")
        }
    };
    Ok(
        match download_first_success(&urls, network, max_size).await? {
            DownloadOutcome::Bytes { bytes, url } => FetchOutcome::Bytes { bytes, url },
            DownloadOutcome::NeedsApproval(host) => FetchOutcome::NeedsApproval(host),
            DownloadOutcome::Denied(host) => FetchOutcome::Denied(host),
        },
    )
}

/// Resolve the source spec into one or more candidate URLs to try in order.
async fn candidate_urls(
    source: &InstallSource,
    network: &NetworkPolicy,
    registry_url: &str,
) -> Result<UrlResolution> {
    match source {
        InstallSource::GitHubRepo(repo) => {
            // GitHub's archive endpoint lives on `codeload.github.com` after
            // the redirect, but the public URL we hit is `github.com`. Every
            // hop is checked, so a deny-default allow list needs both hosts.
            Ok(UrlResolution::Resolved(vec![
                format!("https://github.com/{repo}/archive/refs/heads/main.tar.gz"),
                format!("https://github.com/{repo}/archive/refs/heads/master.tar.gz"),
            ]))
        }
        InstallSource::DirectUrl(url) => Ok(UrlResolution::Resolved(vec![url.clone()])),
        InstallSource::Registry(name) => {
            match fetch_registry(network, registry_url).await? {
                RegistryFetchResult::Loaded(doc) => {
                    let entry = doc
                        .skills
                        .get(name)
                        .with_context(|| format!("skill '{name}' not found in registry"))?
                        .clone();
                    let inner = InstallSource::parse(&entry.source).with_context(|| {
                        format!(
                            "registry entry for '{name}' has invalid source: {}",
                            entry.source
                        )
                    })?;
                    // Recurse only one level — registry pointing at registry is
                    // disallowed to avoid cycles.
                    if matches!(inner, InstallSource::Registry(_)) {
                        bail!("registry entry for '{name}' must not point to another registry");
                    }
                    // Reuse this function for the inner source so GitHub fallback
                    // still applies.
                    Box::pin(candidate_urls(&inner, network, registry_url)).await
                }
                RegistryFetchResult::NeedsApproval(host) => Ok(UrlResolution::NeedsApproval(host)),
                RegistryFetchResult::Denied(host) => Ok(UrlResolution::Denied(host)),
            }
        }
    }
}

/// Download the first URL whose host the policy allows and which returns 2xx.
/// Returns `NeedsApproval` if every candidate hit `Prompt`, or `Denied` if every
/// candidate was denied.
async fn download_first_success(
    urls: &[String],
    network: &NetworkPolicy,
    max_size: u64,
) -> Result<DownloadOutcome> {
    let mut last_status: Option<reqwest::StatusCode> = None;
    let mut prompt_host: Option<String> = None;
    let mut denied_host: Option<String> = None;
    for url in urls {
        let host = match host_from_url(url) {
            Some(h) => h,
            None => bail!("invalid download url: {url}"),
        };
        match network.decide(&host) {
            Decision::Allow => {}
            Decision::Deny => {
                denied_host.get_or_insert(host);
                continue;
            }
            Decision::Prompt => {
                prompt_host.get_or_insert(host);
                continue;
            }
        }
        match download_with_cap(url, network, max_size).await? {
            DownloadAttempt::Bytes(bytes) => {
                return Ok(DownloadOutcome::Bytes {
                    bytes,
                    url: url.clone(),
                });
            }
            DownloadAttempt::NotFound(status) => {
                last_status = Some(status);
                continue;
            }
        }
    }
    if let Some(host) = denied_host {
        return Ok(DownloadOutcome::Denied(host));
    }
    if let Some(host) = prompt_host {
        return Ok(DownloadOutcome::NeedsApproval(host));
    }
    bail!(
        "failed to download skill (last status: {})",
        last_status
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    );
}

enum DownloadAttempt {
    Bytes(Vec<u8>),
    NotFound(reqwest::StatusCode),
}

/// Stream a URL into memory with a size cap. Aborts on the first read that
/// would push the buffer over `max_size * 4` (the *4 accounts for compression;
/// the unpack step still enforces `max_size` on the *uncompressed* bytes).
async fn download_with_cap(
    url: &str,
    network: &NetworkPolicy,
    max_size: u64,
) -> Result<DownloadAttempt> {
    let resp = guarded_get(url, network, None).await?;
    let status = resp.status();
    if !status.is_success() {
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(DownloadAttempt::NotFound(status));
        }
        bail!("download {url} returned {status}");
    }
    let compressed_cap = compressed_download_cap(max_size);
    let bytes = codewhale_util::utils::read_response_body_capped(resp, compressed_cap)
        .await
        .with_context(|| {
            format!("failed to read body of {url} (compressed size cap {compressed_cap} bytes)")
        })?;
    Ok(DownloadAttempt::Bytes(bytes))
}

/// Soft cap on the *compressed* download — well above `max_size` to allow for
/// highly compressible payloads but still bounded.
fn compressed_download_cap(max_size: u64) -> usize {
    usize::try_from(max_size.saturating_mul(4)).unwrap_or(usize::MAX)
}

struct StagedSkill {
    skill_name: String,
    staged_path: PathBuf,
}

/// Validate a tarball and extract it into `<skills_dir>/<name>.tmp/`.
fn stage_tarball(bytes: &[u8], skills_dir: &Path, max_size: u64) -> Result<StagedSkill> {
    fs::create_dir_all(skills_dir)
        .with_context(|| format!("failed to create skills directory {}", skills_dir.display()))?;

    // Two passes: first determine the skill name (and therefore the staged
    // dir) by finding the SKILL.md, then extract under that staged dir.
    // Both passes share the same archive bytes; we reset by wrapping fresh
    // decoders.

    let scan = scan_tarball(bytes, max_size)?;

    // Prepare staged directory. Use a `.tmp` suffix so a crashed install
    // never collides with a real name; remove any leftover from a prior
    // failed attempt.
    let staged_path = skills_dir.join(format!("{}.tmp", scan.skill_name));
    if staged_path.exists() {
        fs::remove_dir_all(&staged_path).with_context(|| {
            format!(
                "failed to clean stale staging dir {}",
                staged_path.display()
            )
        })?;
    }
    fs::create_dir_all(&staged_path)
        .with_context(|| format!("failed to create staging dir {}", staged_path.display()))?;

    // Second pass — extract.
    let result = extract_into(&scan, bytes, &staged_path, max_size);
    if let Err(err) = result {
        // Cleanup on failure so a half-staged directory doesn't survive.
        let _ = fs::remove_dir_all(&staged_path);
        return Err(err);
    }

    Ok(StagedSkill {
        skill_name: scan.skill_name,
        staged_path,
    })
}

struct TarballScan {
    /// Skill name from SKILL.md frontmatter.
    skill_name: String,
    /// Archive prefix to strip from each entry (e.g. `repo-main/`). May be empty.
    prefix: String,
    /// Sub-directory inside `prefix` that the SKILL.md lives in (`""` if root,
    /// or `skills/<name>` for repos that bundle multiple skills).
    skill_root: String,
}

/// First pass: locate SKILL.md, validate frontmatter, compute total size,
/// reject path-traversal entries and symlinks inside the selected install
/// subtree. We do not write anything in this pass; that's the second pass's job.
fn scan_tarball(bytes: &[u8], max_size: u64) -> Result<TarballScan> {
    let cursor = std::io::Cursor::new(bytes);
    let gz = GzDecoder::new(cursor);
    let mut archive = tar::Archive::new(gz);

    let mut total_size: u64 = 0;
    let mut prefix: Option<String> = None;
    let mut skill_md_relative: Option<(SkillMdCandidate, Vec<u8>)> = None;
    let mut skill_md_candidate_count: usize = 0;
    let mut has_claude_plugin_manifest = false;
    let mut link_paths: Vec<String> = Vec::new();

    for entry in archive
        .entries()
        .context("failed to read tar entries (corrupt archive?)")?
    {
        let mut entry = entry.context("failed to read tar entry")?;
        let header = entry.header().clone();
        let entry_type = header.entry_type();
        let path = entry
            .path()
            .context("tar entry has invalid path")?
            .to_path_buf();
        let path_str = path.to_string_lossy().into_owned();
        if !is_safe_path(&path) {
            return Err(InstallError::PathTraversal(path_str).into());
        }
        if is_claude_plugin_manifest_path(&path) {
            has_claude_plugin_manifest = true;
        }

        // Track total size against `max_size` (uncompressed). We honor `header
        // .size` rather than streaming-read every file; tar archives are
        // self-describing so this is reliable for non-malicious inputs and
        // catches the gzip-bomb case.
        if let Ok(size) = header.size() {
            total_size = total_size.saturating_add(size);
            if total_size > max_size {
                return Err(InstallError::OversizedTarball { limit: max_size }.into());
            }
        }

        // Detect prefix from the first entry. GitHub archives wrap everything
        // in `<repo>-<branch>/`; direct tarballs may have no prefix. We treat
        // the first path component as the prefix iff the archive has more than
        // one entry under it, but for SKILL.md detection we just strip the
        // first component if every entry shares it.
        if prefix.is_none() {
            if let Some(Component::Normal(first)) = path.components().next() {
                let candidate = first.to_string_lossy().into_owned();
                // Only treat the first component as a prefix if it's a
                // directory-like (no extension and the path has more
                // components). Otherwise leave prefix empty.
                if path.components().count() > 1 {
                    prefix = Some(candidate);
                } else {
                    prefix = Some(String::new());
                }
            } else {
                prefix = Some(String::new());
            }
        }

        if entry_type.is_symlink() || entry_type.is_hard_link() {
            link_paths.push(path_str);
            continue;
        }

        // SKILL.md detection. Match the same workflow layouts that runtime
        // discovery understands:
        //   * `<prefix>/SKILL.md`
        //   * `<prefix>/*/skills/<name>/SKILL.md`
        //   * `<prefix>/<name>/SKILL.md`
        if entry_type.is_file() {
            let stripped = strip_prefix(&path_str, prefix.as_deref().unwrap_or(""));
            if let Some(candidate) = skill_md_candidate(&stripped) {
                skill_md_candidate_count += 1;
                let mut buf = Vec::new();
                entry
                    .read_to_end(&mut buf)
                    .context("failed to read SKILL.md from archive")?;
                // Prefer the most explicit match: repo-root SKILL.md first,
                // then known skill-directory layouts, then a single nested
                // `<name>/SKILL.md` repository.
                let replace = skill_md_relative
                    .as_ref()
                    .is_none_or(|(current, _)| candidate.rank < current.rank);
                if replace {
                    skill_md_relative = Some((candidate, buf));
                }
            }
        }
    }

    let prefix = prefix.unwrap_or_default();
    if has_claude_plugin_manifest && skill_md_candidate_count > 1 {
        return Err(InstallError::ClaudePluginBundle.into());
    }
    let (skill_md, skill_md_bytes) = skill_md_relative
        .ok_or(InstallError::MissingSkillMd)
        .map_err(anyhow::Error::from)?;

    for link_path in link_paths {
        if is_within_selected_root(&link_path, &prefix, &skill_md.skill_root) {
            return Err(InstallError::SymlinkRejected.into());
        }
    }

    // Install and discovery share the same frontmatter reader; install adds
    // the required description and path-safe destination-name checks.
    let name = parse_frontmatter_name(&skill_md_bytes)?;

    Ok(TarballScan {
        skill_name: name,
        prefix,
        skill_root: skill_md.skill_root,
    })
}

struct SkillMdCandidate {
    rank: u8,
    skill_root: String,
}

fn skill_md_candidate(stripped_path: &str) -> Option<SkillMdCandidate> {
    if stripped_path.eq_ignore_ascii_case("SKILL.md") {
        return Some(SkillMdCandidate {
            rank: 0,
            skill_root: String::new(),
        });
    }

    let parts: Vec<&str> = stripped_path.split('/').collect();
    if parts
        .last()
        .is_none_or(|last| !last.eq_ignore_ascii_case("SKILL.md"))
    {
        return None;
    }

    // Common workflow-pack layouts:
    // `skills/<name>/SKILL.md`, `.agents/skills/<name>/SKILL.md`,
    // `.claude/skills/<name>/SKILL.md`, and nested package layouts such as
    // `packages/foo/skills/<name>/SKILL.md`.
    if parts.len() >= 3 {
        let container = parts[parts.len() - 3];
        let name = parts[parts.len() - 2];
        if container.eq_ignore_ascii_case("skills") && !name.is_empty() {
            return Some(SkillMdCandidate {
                rank: 1,
                skill_root: parts[..parts.len() - 1].join("/"),
            });
        }
    }

    // Single-skill repos sometimes keep their root tidy with
    // `<skill-name>/SKILL.md` plus sibling docs at repo root.
    if parts.len() == 2 && !parts[0].is_empty() {
        return Some(SkillMdCandidate {
            rank: 2,
            skill_root: parts[0].to_string(),
        });
    }

    None
}

fn is_claude_plugin_manifest_path(path: &Path) -> bool {
    let parts: Vec<String> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();

    parts.windows(2).any(|window| {
        window[0].eq_ignore_ascii_case(".claude-plugin")
            && window[1].eq_ignore_ascii_case("plugin.json")
    })
}

fn extract_into(scan: &TarballScan, bytes: &[u8], dest: &Path, max_size: u64) -> Result<()> {
    let cursor = std::io::Cursor::new(bytes);
    let gz = GzDecoder::new(cursor);
    let mut archive = tar::Archive::new(gz);

    let mut total_size: u64 = 0;
    let prefix_with_root = if scan.skill_root.is_empty() {
        scan.prefix.clone()
    } else if scan.prefix.is_empty() {
        scan.skill_root.clone()
    } else {
        format!("{}/{}", scan.prefix, scan.skill_root)
    };

    for entry in archive
        .entries()
        .context("failed to read tar entries (corrupt archive?)")?
    {
        let mut entry = entry.context("failed to read tar entry")?;
        let header = entry.header().clone();
        let entry_type = header.entry_type();
        let path = entry
            .path()
            .context("tar entry has invalid path")?
            .to_path_buf();
        let path_str = path.to_string_lossy().into_owned();
        if !is_safe_path(&path) {
            return Err(InstallError::PathTraversal(path_str).into());
        }

        // Only extract entries that live under our skill root. For simple
        // tarballs (`SKILL.md` at root) that's everything; for multi-skill
        // repos it's the `skills/<name>/` slice.
        let stripped = strip_prefix(&path_str, &prefix_with_root).into_owned();
        if stripped.is_empty() && entry_type.is_dir() {
            // The root directory itself — already created.
            continue;
        }
        if stripped == path_str && !prefix_with_root.is_empty() {
            // Nothing to strip => entry is outside our subtree, skip.
            continue;
        }
        // Defense-in-depth: re-validate the stripped path.
        let stripped_path = Path::new(&stripped);
        if !is_safe_path(stripped_path) {
            return Err(InstallError::PathTraversal(stripped).into());
        }
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(InstallError::SymlinkRejected.into());
        }
        if is_reserved_root_metadata(stripped_path) {
            continue;
        }

        let target = dest.join(stripped_path);
        // Final paranoia check: ensure the resolved target stays under dest.
        // We can't canonicalize (target doesn't exist yet), so we walk
        // components one more time after composing.
        let target_components: Vec<_> = target.components().collect();
        let dest_components: Vec<_> = dest.components().collect();
        if !target_components.starts_with(dest_components.as_slice()) {
            return Err(InstallError::PathTraversal(stripped).into());
        }

        if entry_type.is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("failed to create dir {}", target.display()))?;
            continue;
        }
        if entry_type.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create dir {}", parent.display()))?;
            }
            // Read into a buffer so we can enforce `max_size`. Files inside
            // a SKILL bundle are small; copying through a buffer is fine.
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .with_context(|| format!("failed to read {}", path.display()))?;
            total_size = total_size.saturating_add(buf.len() as u64);
            if total_size > max_size {
                return Err(InstallError::OversizedTarball { limit: max_size }.into());
            }
            let mut options = fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let mut out = options
                .open(&target)
                .with_context(|| format!("failed to create {}", target.display()))?;
            out.write_all(&buf)
                .with_context(|| format!("failed to write {}", target.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let executable = header.mode().context("invalid archive file mode")? & 0o111 != 0;
                let mode = if executable { 0o700 } else { 0o600 };
                out.set_permissions(fs::Permissions::from_mode(mode))
                    .with_context(|| format!("failed to set mode for {}", target.display()))?;
            }
        }
    }
    // Let the host filesystem resolve any platform-specific aliases (for
    // example a trailing dot on Windows) before this staged tree is published.
    // Remote content can never supply the local install or trust receipt.
    for marker in RESERVED_ROOT_METADATA {
        match fs::remove_file(dest.join(marker)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("failed to discard archive metadata"),
        }
    }
    Ok(())
}

fn selected_root(prefix: &str, skill_root: &str) -> String {
    if skill_root.is_empty() {
        prefix.to_string()
    } else if prefix.is_empty() {
        skill_root.to_string()
    } else {
        format!("{prefix}/{skill_root}")
    }
}

fn is_within_selected_root(path: &str, prefix: &str, skill_root: &str) -> bool {
    let root = selected_root(prefix, skill_root);
    if root.is_empty() {
        return true;
    }
    path == root || path.starts_with(&format!("{root}/"))
}

/// Ensure a tar path has no `..` segments and is not absolute.
pub fn is_safe_path(path: &Path) -> bool {
    if path.is_absolute() {
        return false;
    }
    for component in path.components() {
        match component {
            Component::ParentDir => return false,
            Component::Prefix(_) | Component::RootDir => return false,
            _ => {}
        }
    }
    true
}

fn skill_target_path(name: &str, skills_dir: &Path) -> Result<PathBuf> {
    let name = validate_skill_name_segment(name)?;
    Ok(skills_dir.join(name))
}

pub fn validate_skill_name_segment(name: &str) -> Result<&str> {
    if !crate::frontmatter::is_path_safe_skill_name(name) {
        bail!("skill name must be a single path-safe segment (got '{name}')");
    }
    Ok(name)
}

fn ensure_target_within_skills_dir(target: &Path, skills_dir: &Path) -> Result<()> {
    let skills_dir = fs::canonicalize(skills_dir)
        .with_context(|| format!("failed to resolve {}", skills_dir.display()))?;
    let target = fs::canonicalize(target)
        .with_context(|| format!("failed to resolve {}", target.display()))?;
    if !target.starts_with(&skills_dir) {
        bail!(
            "skill path {} escapes skills directory {}",
            target.display(),
            skills_dir.display()
        );
    }
    Ok(())
}

/// Strip a leading directory prefix (e.g. `repo-main/`) from a tarball path.
fn strip_prefix<'a>(path: &'a str, prefix: &str) -> std::borrow::Cow<'a, str> {
    if prefix.is_empty() {
        return std::borrow::Cow::Borrowed(path);
    }
    let with_slash = format!("{prefix}/");
    if let Some(rest) = path.strip_prefix(&with_slash) {
        std::borrow::Cow::Owned(rest.to_string())
    } else if path == prefix {
        std::borrow::Cow::Borrowed("")
    } else {
        std::borrow::Cow::Borrowed(path)
    }
}

/// Extract `name:` and ensure `description:` exist in the SKILL.md frontmatter.
/// Also verifies the leading `---` fence so we reject malformed files early.
fn parse_frontmatter_name(bytes: &[u8]) -> Result<String> {
    let content = std::str::from_utf8(bytes).context("SKILL.md is not valid UTF-8")?;
    let parsed = crate::frontmatter::parse_frontmatter(content).map_err(anyhow::Error::msg)?;
    crate::frontmatter::validate_skill_frontmatter(
        parsed.as_ref().map(|(metadata, _)| metadata),
        None,
        crate::frontmatter::SkillValidationMode::Strict,
    )
    .map_err(anyhow::Error::msg)?;
    let (metadata, _) = parsed.expect("strict validation requires frontmatter");
    let name = metadata["name"].clone();
    Ok(name)
}

fn reject_unmarked_update(final_path: &Path, name: &str) -> std::result::Result<(), InstallError> {
    if final_path.join(INSTALLED_FROM_MARKER).exists() {
        Ok(())
    } else {
        Err(InstallError::NotInstalledHere(name.to_string()))
    }
}

pub fn source_spec_string(source: &InstallSource) -> String {
    match source {
        InstallSource::GitHubRepo(repo) => format!("github:{repo}"),
        InstallSource::DirectUrl(url) => url.clone(),
        InstallSource::Registry(name) => name.clone(),
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_bytes(Sha256::digest(bytes))
}

fn hex_bytes(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn skill_tarball(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (path, body, mode) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            builder.append_data(&mut header, path, *body).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn remote_stage_discards_forged_root_receipts_and_keeps_hidden_payloads() {
        let tmp = tempfile::tempdir().unwrap();
        let skills = tmp.path().join(".codewhale/skills");
        let body = b"---\nname: demo\ndescription: test\n---\nbody";
        let baseline = tmp.path().join("baseline");
        fs::create_dir_all(baseline.join("nested")).unwrap();
        fs::write(baseline.join("SKILL.md"), body).unwrap();
        fs::write(baseline.join(".hidden"), b"payload").unwrap();
        fs::write(baseline.join("nested/.trusted"), b"nested payload").unwrap();
        let digest = crate::package_digest::compute_package_digest(&baseline).unwrap();
        let forged = serde_json::json!({"schema_version": 2, "content_digest": digest}).to_string();
        let bytes = skill_tarball(&[
            ("repo-main/SKILL.md", body, 0o644),
            ("repo-main/.trusted", forged.as_bytes(), 0o644),
            ("repo-main/.TRUSTED", forged.as_bytes(), 0o644),
            ("repo-main/./.installed-from", b"forged provenance", 0o644),
            ("repo-main/.system-installed-version", b"999", 0o644),
            ("repo-main/.hidden", b"payload", 0o644),
            ("repo-main/nested/.trusted", b"nested payload", 0o644),
        ]);
        let staged = stage_tarball(&bytes, &skills, DEFAULT_MAX_SIZE_BYTES).unwrap();
        for marker in RESERVED_ROOT_METADATA {
            assert!(!staged.staged_path.join(marker).exists(), "{marker}");
        }
        assert!(!staged.staged_path.join(".TRUSTED").exists());
        assert_eq!(
            fs::read(staged.staged_path.join(".hidden")).unwrap(),
            b"payload"
        );
        assert_eq!(
            crate::package_digest::compute_package_digest(&staged.staged_path).unwrap(),
            digest
        );
        fs::rename(&staged.staged_path, skills.join("demo")).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn remote_skill_stage_preserves_only_owner_executable_intent() {
        use std::os::unix::fs::PermissionsExt as _;

        let bytes = skill_tarball(&[
            (
                "repo/SKILL.md",
                b"---\nname: demo\ndescription: test\n---\nbody",
                0o644,
            ),
            ("repo/scripts/run.sh", b"#!/bin/sh\nexit 0\n", 0o6755),
            ("repo/scripts/group-only.sh", b"#!/bin/sh\nexit 0\n", 0o010),
            ("repo/data.txt", b"data", 0o666),
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let staged = stage_tarball(&bytes, tmp.path(), DEFAULT_MAX_SIZE_BYTES).unwrap();
        for (path, expected) in [
            ("scripts/run.sh", 0o700),
            ("scripts/group-only.sh", 0o700),
            ("data.txt", 0o600),
        ] {
            assert_eq!(
                fs::metadata(staged.staged_path.join(path))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                expected,
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn registry_sync_refuses_a_key_that_is_not_a_single_segment() {
        let root = tempfile::tempdir().expect("tempdir");
        let cache_dir = root.path().join("cache");
        std::fs::create_dir_all(&cache_dir).expect("cache dir");
        let entry = RegistryEntry {
            source: "https://example.invalid/skill.tar.gz".to_string(),
            description: None,
            keywords: Vec::new(),
            domains: Vec::new(),
        };
        for name in ["../escape", "..", "a/b", "/abs"] {
            let outcome =
                sync_one_skill(name, &entry, &NetworkPolicy::default(), &cache_dir, 1024).await;
            assert!(
                matches!(outcome, SkillSyncOutcome::Failed { ref reason, .. }
                    if reason.contains("single path-safe segment")),
                "{name}: {outcome:?}"
            );
        }
        assert!(!root.path().join("escape").exists());
    }

    /// Serve one response per connection: an endless chunked body, or a
    /// small body that declares an oversized `Content-Length`.
    async fn oversized_body_server() -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0u8; 1024];
                    while let Ok(n) = socket.read(&mut buf).await {
                        if n == 0 {
                            return;
                        }
                        request.extend_from_slice(&buf[..n]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    if request.starts_with(b"GET /declared ") {
                        let _ = socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\nxx")
                            .await;
                        return;
                    }
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                        .await;
                    let chunk = [b'x'; 4096];
                    // Never terminates: only a streaming cap ends this read.
                    loop {
                        if socket.write_all(b"1000\r\n").await.is_err()
                            || socket.write_all(&chunk).await.is_err()
                            || socket.write_all(b"\r\n").await.is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn download_with_cap_stops_reading_at_the_cap() {
        let addr = oversized_body_server().await;
        for path in ["endless", "declared"] {
            let url = format!("http://{addr}/{path}");
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(20),
                download_with_cap(&url, &NetworkPolicy::default(), 16 * 1024),
            )
            .await
            .unwrap_or_else(|_| panic!("{path}: the size cap must end the read"));
            let err = match result {
                Ok(_) => panic!("{path}: an oversized body must be refused"),
                Err(err) => format!("{err:#}"),
            };
            assert!(err.contains("exceeds"), "{path}: {err}");
        }
    }

    /// A server that accepts the connection and then never reads or writes
    /// another byte, so the request can only end through the client's own
    /// timeouts.
    async fn stalled_server() -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // Hold every accepted socket open without responding.
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        addr
    }

    #[tokio::test]
    async fn stalled_registry_fetch_fails_through_the_client_timeouts() {
        let addr = stalled_server().await;
        let url = format!("http://{addr}/index.json");
        let policy = NetworkPolicy {
            default: Decision::Allow.into(),
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: false,
        };
        // Without the install client's own timeouts this call never returns;
        // the outer bound only converts that hang into a test failure.
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            fetch_registry(&policy, &url),
        )
        .await
        .unwrap_or_else(|_| panic!("install HTTP must honor its own timeouts"));
        let err = match outcome {
            Ok(_) => panic!("a stalled registry connection must fail, not succeed"),
            Err(err) => format!("{err:#}"),
        };
        assert!(err.contains("timed out"), "{err}");
    }

    #[test]
    fn parse_github_source() {
        let s = InstallSource::parse("github:Hmbown/test-skill").unwrap();
        assert_eq!(
            s,
            InstallSource::GitHubRepo("Hmbown/test-skill".to_string())
        );
    }

    #[test]
    fn parse_github_source_rejects_missing_repo() {
        let err = InstallSource::parse("github:Hmbown").unwrap_err();
        assert!(err.to_string().contains("github source must"), "got: {err}");
    }

    #[test]
    fn parse_github_source_rejects_extra_slashes() {
        let err = InstallSource::parse("github:Hmbown/repo/extra").unwrap_err();
        assert!(err.to_string().contains("github source must"), "got: {err}");
    }

    #[test]
    fn parse_direct_url_source() {
        let s = InstallSource::parse("https://example.com/skill.tar.gz").unwrap();
        assert_eq!(
            s,
            InstallSource::DirectUrl("https://example.com/skill.tar.gz".to_string())
        );
        let s = InstallSource::parse("http://example.com/skill.tar.gz").unwrap();
        assert_eq!(
            s,
            InstallSource::DirectUrl("http://example.com/skill.tar.gz".to_string())
        );
    }

    #[test]
    fn parse_github_browser_url_routes_to_github_repo() {
        // Regression for #269: `https://github.com/<owner>/<repo>` was being
        // parsed as a DirectUrl, so the installer downloaded the HTML repo
        // page and tried to gzip-decode HTML ("invalid gzip header").
        for spec in [
            "https://github.com/obra/superpowers",
            "https://github.com/obra/superpowers/",
            "https://github.com/obra/superpowers.git",
            "https://github.com/obra/superpowers.git/",
            "https://www.github.com/obra/superpowers",
            "http://github.com/obra/superpowers",
            "  https://github.com/obra/superpowers  ",
        ] {
            let parsed = InstallSource::parse(spec)
                .unwrap_or_else(|err| panic!("parse({spec}) failed: {err}"));
            assert_eq!(
                parsed,
                InstallSource::GitHubRepo("obra/superpowers".to_string()),
                "spec {spec} must route to GitHubRepo",
            );
        }
    }

    #[test]
    fn parse_github_archive_url_stays_direct() {
        // URLs that point at a specific subresource (archive tarball, blob,
        // tree) are real direct URLs — the user picked that exact path.
        for spec in [
            "https://github.com/obra/superpowers/archive/refs/heads/main.tar.gz",
            "https://github.com/obra/superpowers/blob/main/README.md",
            "https://github.com/obra/superpowers/tree/main",
        ] {
            let parsed = InstallSource::parse(spec).unwrap();
            assert!(
                matches!(parsed, InstallSource::DirectUrl(_)),
                "spec {spec} must stay DirectUrl, got {parsed:?}",
            );
        }
    }

    #[test]
    fn parse_registry_source() {
        let s = InstallSource::parse("my-skill").unwrap();
        assert_eq!(s, InstallSource::Registry("my-skill".to_string()));
    }

    #[test]
    fn parse_rejects_empty() {
        assert!(InstallSource::parse("").is_err());
        assert!(InstallSource::parse("   ").is_err());
    }

    #[test]
    fn is_safe_path_rejects_traversal() {
        assert!(!is_safe_path(Path::new("../etc/passwd")));
        assert!(!is_safe_path(Path::new("foo/../bar")));
        assert!(!is_safe_path(Path::new("/etc/passwd")));
        assert!(is_safe_path(Path::new("foo/bar/baz")));
        assert!(is_safe_path(Path::new("SKILL.md")));
    }

    #[test]
    fn parse_frontmatter_extracts_name() {
        let body = b"---\nname: hello\ndescription: greeter\n---\nbody\n";
        assert_eq!(parse_frontmatter_name(body).unwrap(), "hello");
    }

    #[test]
    fn installer_uses_shared_frontmatter_without_weakening_name_checks() {
        let body = "\u{feff}---\r\nname: 'hello'\r\ndescription: Deploy --- safely\r\n  with details: here\r\n---\r\nbody";
        assert_eq!(parse_frontmatter_name(body.as_bytes()).unwrap(), "hello");
        assert!(parse_frontmatter_name(body.replace("'hello'", "'../escape'").as_bytes()).is_err());
        assert!(parse_frontmatter_name(b"---\nname: hello\ndescription: ''\n---\n").is_err());
        assert!(
            parse_frontmatter_name(b"---\nname: hello\ndescription: missing --- fence").is_err()
        );
    }

    #[test]
    fn parse_frontmatter_missing_name_fails() {
        let body = b"---\ndescription: x\n---\n";
        let err = parse_frontmatter_name(body).unwrap_err();
        assert!(format!("{err}").contains("name"));
    }

    #[test]
    fn parse_frontmatter_missing_description_fails() {
        let body = b"---\nname: x\n---\n";
        let err = parse_frontmatter_name(body).unwrap_err();
        assert!(format!("{err}").contains("description"));
    }

    #[test]
    fn parse_frontmatter_rejects_unsafe_name() {
        let body = b"---\nname: ../evil\ndescription: x\n---\n";
        assert!(parse_frontmatter_name(body).is_err());

        let body = b"---\nname: a name with spaces\ndescription: x\n---\n";
        assert!(parse_frontmatter_name(body).is_err());

        let body = b"---\nname: tab\tname\ndescription: x\n---\n";
        assert!(parse_frontmatter_name(body).is_err());
    }

    #[test]
    fn parse_frontmatter_requires_opening_fence() {
        let body = b"name: hello\ndescription: x\n";
        assert!(parse_frontmatter_name(body).is_err());
    }

    #[test]
    fn update_refuses_to_replace_a_directory_without_an_install_marker() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dest = tmp.path().join("hand-authored");
        std::fs::create_dir_all(&dest).expect("dest");
        std::fs::write(dest.join("SKILL.md"), "---\nname: hand-authored\n---\n").expect("skill md");
        assert!(!dest.join(INSTALLED_FROM_MARKER).exists());
        let err = reject_unmarked_update(&dest, "hand-authored").unwrap_err();
        assert!(matches!(err, InstallError::NotInstalledHere(name) if name == "hand-authored"));
        assert!(dest.join("SKILL.md").exists(), "unmarked tree must survive");
    }

    #[test]
    fn user_skill_names_must_be_single_safe_segments() {
        for bad in [
            "",
            "../evil",
            "/tmp/evil",
            "two words",
            "two\twords",
            "evil/name",
            "evil\\name",
            ".",
            "..",
            " leading",
            "trailing ",
        ] {
            assert!(
                validate_skill_name_segment(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
        assert_eq!(
            validate_skill_name_segment("safe-name_1").unwrap(),
            "safe-name_1"
        );
    }

    #[test]
    fn uninstall_and_trust_reject_unsafe_skill_names_before_path_join() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).expect("skills dir");

        for bad in [
            "../evil",
            "/tmp/evil",
            "evil/name",
            "evil\\name",
            "two words",
        ] {
            assert!(uninstall(bad, &skills_dir).is_err());
            assert!(trust(bad, &skills_dir).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn uninstall_rejects_symlink_target_escaping_skills_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let skills_dir = tmp.path().join("skills");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&skills_dir).expect("skills dir");
        std::fs::create_dir_all(&outside).expect("outside dir");
        std::fs::write(outside.join(INSTALLED_FROM_MARKER), "{}").expect("marker");
        std::os::unix::fs::symlink(&outside, skills_dir.join("linked")).expect("symlink");

        let err = uninstall("linked", &skills_dir).unwrap_err();
        assert!(err.to_string().contains("escapes skills directory"));
        assert!(outside.exists());
    }

    #[test]
    fn strip_prefix_handles_all_cases() {
        assert_eq!(strip_prefix("foo/bar", "foo"), "bar");
        assert_eq!(strip_prefix("foo", "foo"), "");
        assert_eq!(strip_prefix("baz/bar", "foo"), "baz/bar");
        assert_eq!(strip_prefix("foo/bar", ""), "foo/bar");
    }

    #[test]
    fn source_spec_string_roundtrips() {
        assert_eq!(
            source_spec_string(&InstallSource::GitHubRepo("a/b".into())),
            "github:a/b"
        );
        assert_eq!(
            source_spec_string(&InstallSource::DirectUrl("https://x".into())),
            "https://x"
        );
        assert_eq!(
            source_spec_string(&InstallSource::Registry("x".into())),
            "x"
        );
    }
}
