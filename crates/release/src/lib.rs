use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub mod autoupdate;
pub mod check;
pub mod install;
pub mod installs;
pub mod launch;
pub mod signing;
pub mod tls;

pub use check::{SuppressionReason, UpdateCheckCache, suppression_reason};
pub use install::{InstallMethod, current_install_method};
pub use launch::{LaunchOutcome, VersionChange, record_launch};

/// Filename of the SHA-256 checksum manifest included in every release.
///
/// Mirror directories must contain this file alongside platform binaries so
/// that download integrity can be verified.
pub const CHECKSUM_MANIFEST_ASSET: &str = "codewhale-artifacts-sha256.txt";

/// GitHub API URL for the single latest stable release.
pub const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/codewhale-hq/CodeWhale/releases/latest";

/// GitHub API URL listing recent releases (up to 100), used to find beta tags.
pub const RELEASES_URL: &str =
    "https://api.github.com/repos/codewhale-hq/CodeWhale/releases?per_page=100";

/// Base URL of the CodeWhale repository on the CNB mirror platform.
pub const CNB_REPO_URL: &str = "https://cnb.cool/codewhale.net/codewhale";

/// Environment variable that overrides the base URL for release asset downloads.
pub const RELEASE_BASE_URL_ENV: &str = "CODEWHALE_RELEASE_BASE_URL";

/// Legacy environment variable (alias for [`RELEASE_BASE_URL_ENV`]).
pub const LEGACY_RELEASE_BASE_URL_ENV: &str = "DEEPSEEK_TUI_RELEASE_BASE_URL";

/// Legacy environment variable (alias for [`RELEASE_BASE_URL_ENV`]).
pub const DEEPSEEK_RELEASE_BASE_URL_ENV: &str = "DEEPSEEK_RELEASE_BASE_URL";

/// Environment variable that, when set, enables the CNB mirror for downloads.
pub const CNB_MIRROR_ENV: &str = "CODEWHALE_USE_CNB_MIRROR";

/// Environment variable that pins the update target version.
pub const UPDATE_VERSION_ENV: &str = "CODEWHALE_VERSION";

/// Pre-rebrand environment variable (alias for [`UPDATE_VERSION_ENV`]).
pub const LEGACY_TUI_UPDATE_VERSION_ENV: &str = "DEEPSEEK_TUI_VERSION";

/// Legacy environment variable (alias for [`UPDATE_VERSION_ENV`]).
pub const LEGACY_UPDATE_VERSION_ENV: &str = "DEEPSEEK_VERSION";

/// User-Agent header sent with release metadata requests.
pub const UPDATE_USER_AGENT: &str = "codewhale-updater";

const CNB_RELEASE_ASSET_BASE: &str = "https://cnb.cool/codewhale.net/codewhale/-/releases/download";
const RELEASE_METADATA_TIMEOUT: Duration = Duration::from_secs(5);

/// Build a reqwest client builder with the TLS roots appropriate for the
/// current platform.
///
/// Android command-line programs such as Termux do not run inside a Java
/// application and therefore cannot initialize reqwest's Android platform
/// verifier with a `JavaVM` and `Context`. Use the Mozilla WebPKI roots there
/// so native CLI/TUI HTTPS works without a JNI host. Other platforms keep
/// reqwest's platform verifier.
pub fn platform_http_client_builder() -> reqwest::ClientBuilder {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let builder = reqwest::Client::builder();
    #[cfg(target_os = "android")]
    {
        builder.tls_backend_preconfigured(android_rustls_config())
    }
    #[cfg(not(target_os = "android"))]
    {
        builder
    }
}

/// Blocking counterpart of [`platform_http_client_builder`].
pub fn platform_blocking_http_client_builder() -> reqwest::blocking::ClientBuilder {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let builder = reqwest::blocking::Client::builder();
    #[cfg(target_os = "android")]
    {
        builder.tls_backend_preconfigured(android_rustls_config())
    }
    #[cfg(not(target_os = "android"))]
    {
        builder
    }
}

#[cfg(target_os = "android")]
fn android_rustls_config() -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// The release channel to query for updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseChannel {
    /// Official stable releases only.
    Stable,
    /// Pre-release / beta versions.
    Beta,
}

impl ReleaseChannel {
    /// Creates a channel from a boolean flag (`true` → [`Beta`](Self::Beta)).
    pub fn from_beta_flag(beta: bool) -> Self {
        if beta { Self::Beta } else { Self::Stable }
    }

    /// Returns a lowercase human-readable label (`"stable"` or `"beta"`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
        }
    }
}

/// Describes where to fetch release metadata from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseQuery {
    /// Use a custom mirror base URL and a pinned version.
    Mirror { base_url: String, version: String },
    /// Query the GitHub single-latest-release endpoint.
    GitHubLatest { url: &'static str },
    /// Query the GitHub release-list endpoint (used for beta discovery).
    GitHubReleaseList { url: &'static str },
}

/// Determines the appropriate [`ReleaseQuery`] for the given channel, taking
/// environment-variable overrides (mirror URL, pinned version) into account.
pub fn resolve_release_query(channel: ReleaseChannel) -> ReleaseQuery {
    let version = update_version_from_env().unwrap_or_else(|| env!("CARGO_PKG_VERSION").into());
    if let Some(base_url) = release_base_url_from_env(&version) {
        return ReleaseQuery::Mirror { base_url, version };
    }

    match channel {
        ReleaseChannel::Stable => ReleaseQuery::GitHubLatest {
            url: LATEST_RELEASE_URL,
        },
        ReleaseChannel::Beta => ReleaseQuery::GitHubReleaseList { url: RELEASES_URL },
    }
}

/// Reads the release base URL from environment variables, falling back to the
/// CNB mirror if `CODEWHALE_USE_CNB_MIRROR=1`. Returns `None` when no override
/// is configured.
pub fn release_base_url_from_env(version: &str) -> Option<String> {
    if let Some(base_url) = explicit_release_base_url_from_env() {
        return Some(base_url);
    }
    if cnb_mirror_requested_from_env() {
        return Some(cnb_release_base_url(version));
    }
    None
}

fn cnb_mirror_requested_from_env() -> bool {
    std::env::var(CNB_MIRROR_ENV).is_ok_and(|value| value == "1")
}

/// Reads an operator-supplied release base URL, ignoring the CNB mirror flag.
///
/// Kept separate from [`release_base_url_from_env`] so callers can tell an
/// explicit mirror directory apart from "use the CNB mirror for this version".
pub fn explicit_release_base_url_from_env() -> Option<String> {
    for env_name in [
        RELEASE_BASE_URL_ENV,
        LEGACY_RELEASE_BASE_URL_ENV,
        DEEPSEEK_RELEASE_BASE_URL_ENV,
    ] {
        if let Ok(value) = std::env::var(env_name) {
            let trimmed = value.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

/// True when `CODEWHALE_USE_CNB_MIRROR` is the override actually in effect —
/// it is exactly `1`, and no explicit base URL outranks it.
pub fn cnb_mirror_override_active() -> bool {
    explicit_release_base_url_from_env().is_none() && cnb_mirror_requested_from_env()
}

/// True when the first-party CNB mirror publishes release binaries for this
/// target.
///
/// The CNB tag pipeline (`.cnb.yml`) builds exactly one artifact set — Linux
/// x64, statically linked against musl. Every other platform is served by
/// canonical GitHub Releases or by an explicit
/// [`RELEASE_BASE_URL_ENV`] mirror, so the updater must not offer CNB there.
pub fn cnb_mirror_supports_target(os: &str, rust_arch: &str) -> bool {
    os == "linux" && rust_arch == "x86_64"
}

/// Constructs the CNB mirror asset URL for a given version tag.
pub fn cnb_release_base_url(version: &str) -> String {
    format!(
        "{}/v{}",
        CNB_RELEASE_ASSET_BASE.trim_end_matches('/'),
        version.trim_start_matches('v')
    )
}

/// Returns the pinned update version from environment variables, or `None`
/// if `CODEWHALE_VERSION` and its two legacy aliases are unset or empty.
pub fn update_version_from_env() -> Option<String> {
    [
        UPDATE_VERSION_ENV,
        LEGACY_TUI_UPDATE_VERSION_ENV,
        LEGACY_UPDATE_VERSION_ENV,
    ]
    .into_iter()
    .find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().trim_start_matches('v').to_string())
            .filter(|value| !value.is_empty())
    })
}

/// Joins a mirror base URL with an asset filename to produce a full download URL.
pub fn mirror_asset_url(base_url: &str, asset_name: &str) -> String {
    format!("{}/{}", base_url.trim_end_matches('/'), asset_name)
}

/// Returns a human-readable hint explaining how to use a mirror when GitHub
/// downloads are blocked or slow (e.g. on mainland China networks).
pub fn update_network_fallback_hint() -> String {
    format!(
        "GitHub release downloads may be blocked or slow on this network.\n\
         For mainland China, use one of these fallback paths:\n\
           1. Source build from the CNB mirror, installing the shipped binary:\n\
              cargo install --git {CNB_REPO_URL} --tag vX.Y.Z codewhale-cli --locked --force\n\
           2. Use a binary asset mirror:\n\
              {RELEASE_BASE_URL_ENV}=https://<mirror>/<release-assets>/ {UPDATE_VERSION_ENV}=X.Y.Z codewhale update\n\
         The mirror directory must contain {CHECKSUM_MANIFEST_ASSET} and the codewhale platform binary."
    )
}

/// Fetches a release JSON payload from `url` (async).
///
/// `description` is included in error messages to identify the request purpose.
pub async fn fetch_release_json_async(url: &str, description: &str) -> Result<String> {
    let client = platform_http_client_builder()
        .user_agent(UPDATE_USER_AGENT)
        .timeout(RELEASE_METADATA_TIMEOUT)
        .build()
        .context("failed to build release check HTTP client")?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .with_context(|| format!("failed to fetch {description} from {url}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .with_context(|| format!("failed to read {description} response from {url}"));
    release_response_body(status, body, url, description)
}

fn release_response_body(
    status: reqwest::StatusCode,
    body: Result<String>,
    url: &str,
    description: &str,
) -> Result<String> {
    let body = body.with_context(|| format!("failed to read {description} response from {url}"))?;
    if !status.is_success() {
        bail!("GitHub release request failed with HTTP {status}: {body}");
    }
    Ok(body)
}

#[derive(Deserialize)]
struct ReleaseTag {
    tag_name: String,
}

#[derive(Deserialize)]
struct ReleaseListEntry {
    tag_name: String,
}

/// Extracts the `tag_name` field from a GitHub single-release JSON response.
pub fn latest_tag_from_release_json(body: &str) -> Result<String> {
    let release: ReleaseTag = serde_json::from_str(body).with_context(|| {
        format!("failed to parse release JSON from GitHub API. Response: {body}")
    })?;
    Ok(release.tag_name)
}

/// Scans a GitHub release-list JSON response and returns the tag of the first
/// entry whose name contains `"beta"`.
pub fn latest_beta_tag_from_release_list_json(body: &str) -> Result<String> {
    let releases: Vec<ReleaseListEntry> = serde_json::from_str(body).with_context(|| {
        format!("failed to parse release list JSON from GitHub API. Response: {body}")
    })?;
    releases
        .into_iter()
        .find(|release| is_beta_tag(&release.tag_name))
        .map(|release| release.tag_name)
        .context("no beta release found in GitHub releases")
}

/// Async helper that resolves the latest release tag for the given channel.
///
/// For mirrors the version is derived from the pinned environment variable;
/// for GitHub channels the appropriate API endpoint is queried.
pub async fn latest_release_tag_async(channel: ReleaseChannel) -> Result<String> {
    match resolve_release_query(channel) {
        ReleaseQuery::Mirror { version, .. } => Ok(format!("v{}", version.trim_start_matches('v'))),
        ReleaseQuery::GitHubLatest { url } => {
            let body = fetch_release_json_async(url, "latest release").await?;
            latest_tag_from_release_json(&body)
        }
        ReleaseQuery::GitHubReleaseList { url } => {
            let body = fetch_release_json_async(url, "release list").await?;
            latest_beta_tag_from_release_list_json(&body)
        }
    }
}

/// Compares a current version string against a release tag using semver
/// ordering. Both `v` prefixes and trailing build metadata (e.g. `(abc123)`)
/// are stripped before comparison.
pub fn compare_release_versions(
    current_version: &str,
    latest_tag: &str,
) -> Result<std::cmp::Ordering> {
    let current = parse_release_version(current_version)
        .with_context(|| format!("failed to parse current version {current_version:?}"))?;
    let latest = parse_release_version(latest_tag)
        .with_context(|| format!("failed to parse latest release tag {latest_tag:?}"))?;
    Ok(current.cmp(&latest))
}

/// Determines whether an update is needed for the given channel.
///
/// For [`Stable`](ReleaseChannel::Stable) an update is needed when the latest
/// release is strictly newer. For [`Beta`](ReleaseChannel::Beta) the logic also
/// allows switching from a stable release to a beta on the same release line.
pub fn update_is_needed(
    channel: ReleaseChannel,
    current_version: &str,
    latest_tag: &str,
) -> Result<bool> {
    let current = parse_release_version(current_version)
        .with_context(|| format!("failed to parse current version {current_version:?}"))?;
    let latest = parse_release_version(latest_tag)
        .with_context(|| format!("failed to parse latest release tag {latest_tag:?}"))?;

    match channel {
        ReleaseChannel::Stable => Ok(current < latest),
        ReleaseChannel::Beta => {
            if current == latest {
                return Ok(false);
            }
            let latest_is_beta = version_is_beta(&latest);
            let current_is_stable = current.pre.is_empty();
            let same_release_line = current.major == latest.major
                && current.minor == latest.minor
                && current.patch == latest.patch;
            if current > latest && !(current_is_stable && same_release_line) {
                return Ok(false);
            }
            Ok(latest_is_beta)
        }
    }
}

/// Parses a version string (with optional `v` prefix and trailing build info)
/// into a [`semver::Version`].
pub fn parse_release_version(value: &str) -> Result<semver::Version> {
    let version = value
        .trim()
        .trim_start_matches('v')
        .split_whitespace()
        .next()
        .unwrap_or("");
    semver::Version::parse(version).with_context(|| format!("invalid semver: {value:?}"))
}

/// Returns `true` if the tag name contains `"beta"` (case-insensitive).
pub fn is_beta_tag(tag_name: &str) -> bool {
    tag_name.to_ascii_lowercase().contains("beta")
}

fn version_is_beta(version: &semver::Version) -> bool {
    version.pre.as_str().to_ascii_lowercase().contains("beta")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard};

    use super::*;

    static RELEASE_ENV_LOCK: Mutex<()> = Mutex::new(());
    const RELEASE_ENV_VARS: &[&str] = &[
        RELEASE_BASE_URL_ENV,
        LEGACY_RELEASE_BASE_URL_ENV,
        DEEPSEEK_RELEASE_BASE_URL_ENV,
        CNB_MIRROR_ENV,
        UPDATE_VERSION_ENV,
        LEGACY_TUI_UPDATE_VERSION_ENV,
        LEGACY_UPDATE_VERSION_ENV,
    ];

    struct ReleaseEnvGuard {
        previous: Vec<(&'static str, Option<OsString>)>,
        _lock: MutexGuard<'static, ()>,
    }

    impl ReleaseEnvGuard {
        fn clear() -> Self {
            let lock = RELEASE_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let previous = RELEASE_ENV_VARS
                .iter()
                .map(|&name| (name, std::env::var_os(name)))
                .collect();
            for &name in RELEASE_ENV_VARS {
                // SAFETY: tests that mutate these process-wide vars hold RELEASE_ENV_LOCK.
                unsafe { std::env::remove_var(name) };
            }
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for ReleaseEnvGuard {
        fn drop(&mut self) {
            for (name, value) in &self.previous {
                // SAFETY: the guard still holds RELEASE_ENV_LOCK while restoring state.
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    fn set_release_env(name: &str, value: &str) {
        // SAFETY: callers hold a ReleaseEnvGuard, which serializes env mutation.
        unsafe { std::env::set_var(name, value) };
    }

    #[test]
    fn release_channel_from_beta_flag_maps_booleans() {
        assert_eq!(
            ReleaseChannel::from_beta_flag(false),
            ReleaseChannel::Stable
        );
        assert_eq!(ReleaseChannel::from_beta_flag(true), ReleaseChannel::Beta);
    }

    #[test]
    fn release_channel_label_matches_channel_names() {
        assert_eq!(ReleaseChannel::Stable.label(), "stable");
        assert_eq!(ReleaseChannel::Beta.label(), "beta");
    }

    #[test]
    fn is_beta_tag_detects_beta_prereleases_case_insensitively() {
        for tag in [
            "beta",
            "BETA",
            "BeTa",
            "v1.0.0-beta.1",
            "v1.0.0-BETA.1",
            "v2.0.0-beta",
            "something-beta-something",
            "beta-1.0",
        ] {
            assert!(is_beta_tag(tag), "{tag} should be beta");
        }

        for tag in [
            "",
            "bet",
            "alpha",
            "rc",
            "v1.0.0",
            "v1.0.0-alpha.1",
            "v1.0.0-rc.1",
        ] {
            assert!(!is_beta_tag(tag), "{tag} should not be beta");
        }
    }

    #[test]
    fn release_base_url_from_env_returns_none_without_overrides() {
        let _env = ReleaseEnvGuard::clear();

        assert_eq!(release_base_url_from_env("1.0.0"), None);
    }

    #[test]
    fn release_base_url_from_env_prefers_primary_override() {
        let _env = ReleaseEnvGuard::clear();
        set_release_env(RELEASE_BASE_URL_ENV, "https://primary.example.com");
        set_release_env(LEGACY_RELEASE_BASE_URL_ENV, "https://legacy.example.com");

        assert_eq!(
            release_base_url_from_env("1.0.0"),
            Some("https://primary.example.com".to_string())
        );
    }

    #[test]
    fn release_base_url_from_env_falls_back_to_legacy_overrides() {
        let _env = ReleaseEnvGuard::clear();
        set_release_env(LEGACY_RELEASE_BASE_URL_ENV, "https://legacy.example.com");
        set_release_env(
            DEEPSEEK_RELEASE_BASE_URL_ENV,
            "https://deepseek.example.com",
        );

        assert_eq!(
            release_base_url_from_env("1.0.0"),
            Some("https://legacy.example.com".to_string())
        );

        set_release_env(LEGACY_RELEASE_BASE_URL_ENV, "");

        assert_eq!(
            release_base_url_from_env("1.0.0"),
            Some("https://deepseek.example.com".to_string())
        );
    }

    #[test]
    fn release_base_url_from_env_trims_and_ignores_empty_overrides() {
        let _env = ReleaseEnvGuard::clear();
        set_release_env(RELEASE_BASE_URL_ENV, "  https://spaced.example.com  \n");

        assert_eq!(
            release_base_url_from_env("1.0.0"),
            Some("https://spaced.example.com".to_string())
        );

        set_release_env(RELEASE_BASE_URL_ENV, "   ");
        set_release_env(LEGACY_RELEASE_BASE_URL_ENV, "");
        set_release_env(DEEPSEEK_RELEASE_BASE_URL_ENV, "\n");

        assert_eq!(release_base_url_from_env("1.0.0"), None);
    }

    #[test]
    fn release_base_url_from_env_uses_cnb_mirror_last() {
        let _env = ReleaseEnvGuard::clear();
        set_release_env(CNB_MIRROR_ENV, "1");

        assert_eq!(
            release_base_url_from_env("v1.2.3"),
            Some("https://cnb.cool/codewhale.net/codewhale/-/releases/download/v1.2.3".to_string())
        );

        set_release_env(RELEASE_BASE_URL_ENV, "https://explicit.example.com");

        assert_eq!(
            release_base_url_from_env("1.0.0"),
            Some("https://explicit.example.com".to_string())
        );

        set_release_env(RELEASE_BASE_URL_ENV, "");
        set_release_env(CNB_MIRROR_ENV, "0");
        assert_eq!(
            release_base_url_from_env("1.0.0"),
            None,
            "the Rust updater must match npm and require an exact =1 override"
        );
    }

    #[test]
    fn cnb_mirror_override_is_active_only_without_an_explicit_base_url() {
        let _env = ReleaseEnvGuard::clear();
        assert!(!cnb_mirror_override_active());

        set_release_env(CNB_MIRROR_ENV, "1");
        assert!(cnb_mirror_override_active());

        set_release_env(RELEASE_BASE_URL_ENV, "https://explicit.example.com");
        assert!(
            !cnb_mirror_override_active(),
            "an explicit base URL outranks the CNB flag"
        );

        set_release_env(RELEASE_BASE_URL_ENV, "");
        for disabled in ["", "0", "true", "yes", " 1 "] {
            set_release_env(CNB_MIRROR_ENV, disabled);
            assert!(
                !cnb_mirror_override_active(),
                "only CODEWHALE_USE_CNB_MIRROR=1 may force CNB, got {disabled:?}"
            );
        }
    }

    #[test]
    fn cnb_mirror_publishes_linux_x64_only() {
        assert!(cnb_mirror_supports_target("linux", "x86_64"));

        for (os, arch) in [
            ("linux", "aarch64"),
            ("linux", "riscv64"),
            ("macos", "x86_64"),
            ("macos", "aarch64"),
            ("windows", "x86_64"),
            ("windows", "aarch64"),
            ("android", "aarch64"),
        ] {
            assert!(
                !cnb_mirror_supports_target(os, arch),
                "CNB must not claim {os}/{arch}"
            );
        }
    }

    #[test]
    fn update_version_from_env_prefers_primary_then_legacy() {
        {
            let _env = ReleaseEnvGuard::clear();
            set_release_env(UPDATE_VERSION_ENV, "  v1.2.3  ");

            assert_eq!(update_version_from_env().as_deref(), Some("1.2.3"));
        }

        {
            let _env = ReleaseEnvGuard::clear();
            set_release_env(LEGACY_UPDATE_VERSION_ENV, "v1.2.4");

            assert_eq!(update_version_from_env().as_deref(), Some("1.2.4"));
        }
    }

    #[test]
    fn update_version_from_env_ignores_missing_or_empty_values() {
        let _env = ReleaseEnvGuard::clear();
        assert_eq!(update_version_from_env(), None);

        set_release_env(UPDATE_VERSION_ENV, "   ");
        set_release_env(LEGACY_UPDATE_VERSION_ENV, "");

        assert_eq!(update_version_from_env(), None);
    }

    #[test]
    fn canonical_version_pin_outranks_both_preserved_legacy_aliases() {
        let _env = ReleaseEnvGuard::clear();
        set_release_env("CODEWHALE_VERSION", "v0.9.11");
        set_release_env("DEEPSEEK_TUI_VERSION", "v0.9.10");
        set_release_env("DEEPSEEK_VERSION", "v0.9.9");
        assert_eq!(update_version_from_env().as_deref(), Some("0.9.11"));
        set_release_env("CODEWHALE_VERSION", " ");
        assert_eq!(update_version_from_env().as_deref(), Some("0.9.10"));
        set_release_env("DEEPSEEK_TUI_VERSION", " ");
        assert_eq!(update_version_from_env().as_deref(), Some("0.9.9"));
    }

    #[test]
    fn update_network_fallback_hint_mentions_required_mirror_inputs() {
        let hint = update_network_fallback_hint();

        assert!(hint.contains(CNB_REPO_URL), "hint missing CNB_REPO_URL");
        assert!(
            hint.contains(RELEASE_BASE_URL_ENV),
            "hint missing RELEASE_BASE_URL_ENV"
        );
        assert!(
            hint.contains(UPDATE_VERSION_ENV),
            "hint missing UPDATE_VERSION_ENV"
        );
        assert!(
            hint.contains(CHECKSUM_MANIFEST_ASSET),
            "hint missing CHECKSUM_MANIFEST_ASSET"
        );
        assert!(
            hint.contains("codewhale platform binary"),
            "hint must describe the sole implementation asset"
        );
        assert!(
            !hint.contains("codewhale-tui"),
            "hint must not request the removed TUI implementation asset"
        );
    }

    #[test]
    fn mirror_asset_url_trims_trailing_base_slashes() {
        for base_url in [
            "https://example.com/assets",
            "https://example.com/assets/",
            "https://example.com/assets//",
        ] {
            assert_eq!(
                mirror_asset_url(base_url, "file.zip"),
                "https://example.com/assets/file.zip",
                "{base_url} should join with a single slash"
            );
        }
        assert_eq!(mirror_asset_url("", "file.zip"), "/file.zip");
    }

    #[test]
    fn resolve_release_query_uses_github_without_overrides() {
        let _env = ReleaseEnvGuard::clear();

        assert_eq!(
            resolve_release_query(ReleaseChannel::Stable),
            ReleaseQuery::GitHubLatest {
                url: LATEST_RELEASE_URL
            }
        );
        assert_eq!(
            resolve_release_query(ReleaseChannel::Beta),
            ReleaseQuery::GitHubReleaseList { url: RELEASES_URL }
        );
    }

    #[test]
    fn resolve_release_query_uses_release_base_url_overrides() {
        let default_version = env!("CARGO_PKG_VERSION").to_string();

        for (env_name, expected_url) in [
            (RELEASE_BASE_URL_ENV, "https://primary.example.com/mirror"),
            (
                LEGACY_RELEASE_BASE_URL_ENV,
                "https://legacy.example.com/mirror",
            ),
            (
                DEEPSEEK_RELEASE_BASE_URL_ENV,
                "https://deepseek.example.com/mirror",
            ),
        ] {
            let _env = ReleaseEnvGuard::clear();
            set_release_env(env_name, expected_url);

            assert_eq!(
                resolve_release_query(ReleaseChannel::Stable),
                ReleaseQuery::Mirror {
                    base_url: expected_url.to_string(),
                    version: default_version.clone(),
                },
                "{env_name} should drive mirror query"
            );
        }
    }

    #[test]
    fn resolve_release_query_uses_cnb_mirror_override() {
        let _env = ReleaseEnvGuard::clear();
        let default_version = env!("CARGO_PKG_VERSION").to_string();
        set_release_env(CNB_MIRROR_ENV, "1");

        assert_eq!(
            resolve_release_query(ReleaseChannel::Stable),
            ReleaseQuery::Mirror {
                base_url: cnb_release_base_url(&default_version),
                version: default_version,
            }
        );
    }

    #[test]
    fn resolve_release_query_uses_pinned_release_versions_for_mirrors() {
        {
            let _env = ReleaseEnvGuard::clear();
            set_release_env(RELEASE_BASE_URL_ENV, "https://example.com/mirror");
            set_release_env(UPDATE_VERSION_ENV, "v1.2.3");

            assert_eq!(
                resolve_release_query(ReleaseChannel::Stable),
                ReleaseQuery::Mirror {
                    base_url: "https://example.com/mirror".to_string(),
                    version: "1.2.3".to_string(),
                }
            );
        }

        {
            let _env = ReleaseEnvGuard::clear();
            set_release_env(RELEASE_BASE_URL_ENV, "https://example.com/mirror");
            set_release_env(LEGACY_UPDATE_VERSION_ENV, "v1.2.3-legacy");

            assert_eq!(
                resolve_release_query(ReleaseChannel::Stable),
                ReleaseQuery::Mirror {
                    base_url: "https://example.com/mirror".to_string(),
                    version: "1.2.3-legacy".to_string(),
                }
            );
        }
    }

    #[test]
    fn cnb_release_base_url_includes_tag_directory() {
        assert_eq!(
            cnb_release_base_url("0.8.47"),
            "https://cnb.cool/codewhale.net/codewhale/-/releases/download/v0.8.47"
        );
        assert_eq!(
            cnb_release_base_url("v0.8.47"),
            "https://cnb.cool/codewhale.net/codewhale/-/releases/download/v0.8.47"
        );
    }

    #[test]
    fn stable_update_is_needed_only_when_latest_is_newer() {
        assert!(update_is_needed(ReleaseChannel::Stable, "0.8.45", "v0.8.46").unwrap());
        assert!(update_is_needed(ReleaseChannel::Stable, "0.8.45", "v0.9.0-beta.1").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Stable, "0.8.45", "v0.8.45").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Stable, "0.9.0", "v0.9.0-beta.1").unwrap());
        assert!(
            !update_is_needed(ReleaseChannel::Stable, "0.9.0-beta.2", "v0.9.0-beta.1").unwrap()
        );
    }

    #[test]
    fn beta_update_allows_switching_from_same_stable_to_beta() {
        assert!(update_is_needed(ReleaseChannel::Beta, "1.0.0", "v1.0.0-beta.2").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Beta, "1.0.0-beta.2", "v1.0.0-beta.2").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Beta, "1.0.0-beta.3", "v1.0.0-beta.2").unwrap());
        assert!(update_is_needed(ReleaseChannel::Beta, "1.0.0-beta.2", "v1.0.0-beta.3").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Beta, "2.0.0", "v1.0.0-beta.3").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Beta, "1.0.0-rc.1", "v1.0.0-beta.3").unwrap());
    }

    #[test]
    fn parse_release_version_accepts_tags_and_build_suffixes() {
        assert_eq!(
            parse_release_version("v0.9.0-beta.1").unwrap(),
            semver::Version::parse("0.9.0-beta.1").unwrap()
        );
        assert_eq!(
            parse_release_version("0.8.45 (abcdef123456)").unwrap(),
            semver::Version::parse("0.8.45").unwrap()
        );
    }

    #[test]
    fn release_version_compare_ignores_v_prefix_and_build_sha() {
        assert_eq!(
            compare_release_versions("0.8.39 (eeccf7d)", "v0.8.39").unwrap(),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            compare_release_versions("0.8.39", "v0.8.40").unwrap(),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_release_versions("0.8.40", "v0.8.39").unwrap(),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn latest_beta_tag_selects_first_beta_release() {
        let body = r#"[
          { "tag_name": "v0.9.0" },
          { "tag_name": "v0.9.0-rc.1" },
          { "tag_name": "v0.9.0-beta.2" },
          { "tag_name": "v0.9.0-beta.1" }
        ]"#;
        assert_eq!(
            latest_beta_tag_from_release_list_json(body).unwrap(),
            "v0.9.0-beta.2"
        );
    }

    #[test]
    fn latest_beta_tag_reports_missing_beta() {
        let body = r#"[{ "tag_name": "v0.9.0" }]"#;
        let err = latest_beta_tag_from_release_list_json(body).expect_err("missing beta");
        assert!(
            err.to_string().contains("no beta release found"),
            "unexpected error: {err:#}"
        );
    }
}
