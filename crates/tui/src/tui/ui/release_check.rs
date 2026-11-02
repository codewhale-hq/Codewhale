//! The startup update check: resolving a latest-release tag from the
//! configured source and turning it into a version hint.
//!
//! Moved verbatim out of `ui.rs`.

use super::*;

pub(crate) fn startup_version_check_source(config: &UpdateConfig) -> StartupVersionCheckSource {
    if codewhale_release::autoupdate::resolve_mode(config.mode.as_deref(), config.check_for_updates)
        .mode
        == codewhale_release::autoupdate::UpdateMode::Off
    {
        return StartupVersionCheckSource::Disabled;
    }
    resolve_version_check_source(config, codewhale_release::suppression_reason())
}

/// Pure form of [`startup_version_check_source`]: the environment is read once
/// by the caller and passed in, so the decision itself is testable without
/// mutating process-global state (and so this crate's own CI run does not
/// change the answer).
pub(crate) fn resolve_version_check_source(
    config: &UpdateConfig,
    suppression: Option<codewhale_release::SuppressionReason>,
) -> StartupVersionCheckSource {
    if !config.check_for_updates {
        return StartupVersionCheckSource::Disabled;
    }
    // CI runners and explicit opt-outs never reach the network: there is
    // nobody at the terminal to read the notice, and an unexplained outbound
    // request from a build agent is a support ticket waiting to happen.
    if let Some(reason) = suppression {
        tracing::debug!(
            variable = reason.variable(),
            "skipping startup update check"
        );
        return StartupVersionCheckSource::Disabled;
    }
    if let Some(update_uri) = config.update_uri() {
        return StartupVersionCheckSource::ConfiguredUrl(update_uri.to_string());
    }
    StartupVersionCheckSource::ReleaseResolver
}

/// Where the throttling cache for the startup check lives.
///
/// `None` when the CodeWhale home cannot be resolved — the check still runs,
/// it just cannot be throttled, which is the right tradeoff for a homeless
/// install (rare, and better than never checking).
pub(crate) fn update_check_cache_path() -> Option<PathBuf> {
    codewhale_config::codewhale_home()
        .ok()
        .map(|home| codewhale_release::check::cache_path_in(&home))
}

pub(crate) fn spawn_startup_version_check(
    config: UpdateConfig,
) -> Option<tokio::task::JoinHandle<Option<UpdateNotice>>> {
    let source = startup_version_check_source(&config);
    if source == StartupVersionCheckSource::Disabled {
        return None;
    }

    let current = env!("CARGO_PKG_VERSION").to_string();
    let cache_path = update_check_cache_path();
    let interval_hours = config.check_interval_hours;
    let auto = codewhale_release::autoupdate::resolve_mode(
        config.mode.as_deref(),
        config.check_for_updates,
    )
    .mode
        == codewhale_release::autoupdate::UpdateMode::Auto;
    Some(tokio::spawn(async move {
        let mut notice =
            cached_version_hint(source, &current, cache_path.as_deref(), interval_hours).await?;
        if auto {
            let latest = notice.latest.clone();
            notice.staged = tokio::task::spawn_blocking(move || {
                staged_or_start_staging(&latest, interval_hours)
            })
            .await
            .ok()
            .flatten();
        }
        Some(notice)
    }))
}

fn staged_or_start_staging(latest: &str, interval_hours: u64) -> Option<bool> {
    use codewhale_release::autoupdate;
    let home = codewhale_config::codewhale_home().ok()?;
    if let Some(staged) = autoupdate::load_staged(&home)
        && staged.version == latest
    {
        return Some(staged.signed);
    }
    let exe = std::env::current_exe().ok()?;
    if !codewhale_release::InstallMethod::detect(&exe).supports_self_update()
        || !matches!(
            exe.file_stem().and_then(|stem| stem.to_str()),
            Some("codewhale" | "codew")
        )
    {
        return None;
    }
    let state = autoupdate::load_state(&home);
    let retry_after = interval_hours.clamp(1, 24) * 3600;
    if state.skip_version.as_deref() == Some(latest)
        || (state.last_stage_version.as_deref() == Some(latest)
            && codewhale_release::check::now_unix().saturating_sub(state.last_stage_attempt_unix)
                < retry_after)
    {
        return None;
    }
    let mut command = std::process::Command::new(exe);
    command
        .args(["update", "--stage"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Err(error) = command.spawn() {
        tracing::debug!(error = %error, "could not start background update staging");
    }
    None
}

/// Resolve the update notice, reaching for the network at most once per
/// configured interval.
///
/// The cache holds the *tag we last saw*, not a "checked recently" flag, so a
/// user who relaunches all afternoon still sees the notice every time while
/// GitHub is asked once. A cached `None` means "we asked, there was nothing" —
/// also a valid answer worth not re-asking for.
pub(crate) async fn cached_version_hint(
    source: StartupVersionCheckSource,
    current: &str,
    cache_path: Option<&Path>,
    interval_hours: u64,
) -> Option<UpdateNotice> {
    let now = codewhale_release::check::now_unix();
    if let Some(path) = cache_path
        && let Some(entry) = codewhale_release::UpdateCheckCache::load(path)
        && entry.is_fresh(now, interval_hours)
    {
        return entry
            .latest_tag
            .as_deref()
            .and_then(|tag| version_hint_from_latest_tag(tag, current));
    }

    let latest_tag = latest_tag_from_startup_source(source).await;

    if let Some(path) = cache_path
        && latest_tag.is_some()
        && let Err(err) = codewhale_release::UpdateCheckCache::now(latest_tag.clone()).store(path)
    {
        tracing::debug!(error = %err, "failed to persist update-check cache");
    }
    if latest_tag.is_none()
        && interval_hours > 1
        && let Some(path) = cache_path
        && let Some(previous) = codewhale_release::UpdateCheckCache::load(path)
    {
        let retry = codewhale_release::UpdateCheckCache {
            checked_at_unix: now.saturating_sub(interval_hours.saturating_sub(1) * 3600),
            latest_tag: previous.latest_tag.clone(),
        };
        let _ = retry.store(path);
        return previous
            .latest_tag
            .as_deref()
            .and_then(|tag| version_hint_from_latest_tag(tag, current));
    }

    latest_tag
        .as_deref()
        .and_then(|tag| version_hint_from_latest_tag(tag, current))
}

/// The latest publishable release tag for this source, or `None` when the
/// lookup failed or the release is not one we would offer.
pub(crate) async fn latest_tag_from_startup_source(
    source: StartupVersionCheckSource,
) -> Option<String> {
    match source {
        StartupVersionCheckSource::Disabled => None,
        StartupVersionCheckSource::ConfiguredUrl(url) => {
            match latest_tag_from_configured_update_uri(&url).await {
                Ok(tag) => tag,
                Err(_) => latest_tag_from_release_mirror_env().await,
            }
        }
        StartupVersionCheckSource::ReleaseResolver => {
            if release_mirror_env_configured() {
                return latest_tag_from_release_mirror_env().await;
            }

            let body = codewhale_release::fetch_release_json_async(
                codewhale_release::LATEST_RELEASE_URL,
                "latest release",
            )
            .await
            .ok()?;
            let json: serde_json::Value = serde_json::from_str(&body).ok()?;
            publishable_release_tag(&json).map(str::to_string)
        }
    }
}

pub(crate) async fn latest_tag_from_release_mirror_env() -> Option<String> {
    if !release_mirror_env_configured() {
        return None;
    }
    codewhale_release::latest_release_tag_async(codewhale_release::ReleaseChannel::Stable)
        .await
        .ok()
}

pub(crate) fn release_mirror_env_configured() -> bool {
    let version = codewhale_release::update_version_from_env()
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    codewhale_release::release_base_url_from_env(&version).is_some()
}

pub(crate) async fn latest_tag_from_configured_update_uri(
    update_uri: &str,
) -> Result<Option<String>> {
    let body = codewhale_release::fetch_release_json_async(update_uri, "configured latest release")
        .await?;
    let json: serde_json::Value = serde_json::from_str(&body).with_context(|| {
        format!("failed to parse release JSON from configured URI {update_uri}")
    })?;
    Ok(custom_release_tag(&json).map(str::to_string))
}

/// Tag of a GitHub release we would actually offer: published, and with every
/// asset the updater needs already uploaded.
pub(crate) fn publishable_release_tag(json: &serde_json::Value) -> Option<&str> {
    if !release_has_required_assets(json) {
        return None;
    }
    json["tag_name"].as_str()
}

/// Tag from a user-configured release endpoint. Asset completeness is only
/// enforced when the payload advertises assets at all, since a custom mirror
/// may legitimately publish metadata in a different shape.
pub(crate) fn custom_release_tag(json: &serde_json::Value) -> Option<&str> {
    if !release_is_publishable(json) {
        return None;
    }
    if json.get("assets").is_some() && !release_has_required_assets(json) {
        return None;
    }
    json["tag_name"].as_str()
}

/// Test-only shorthand pairing tag extraction with the newness comparison.
/// Production code splits the two so the tag can be cached independently of
/// the version we happen to be running.
#[cfg(test)]
pub(crate) fn version_hint_from_release_json(
    json: &serde_json::Value,
    current: &str,
) -> Option<UpdateNotice> {
    version_hint_from_latest_tag(publishable_release_tag(json)?, current)
}

#[cfg(test)]
pub(crate) fn version_hint_from_custom_release_json(
    json: &serde_json::Value,
    current: &str,
) -> Option<UpdateNotice> {
    version_hint_from_latest_tag(custom_release_tag(json)?, current)
}

pub(crate) fn version_hint_from_latest_tag(tag: &str, current: &str) -> Option<UpdateNotice> {
    let latest = tag.trim_start_matches('v');
    if !codewhale_release::compare_release_versions(current, tag).is_ok_and(|order| order.is_lt()) {
        return None;
    }

    Some(UpdateNotice {
        current: current.to_string(),
        latest: latest.to_string(),
        staged: None,
    })
}

pub(crate) fn release_has_required_assets(json: &serde_json::Value) -> bool {
    if !release_is_publishable(json) {
        return false;
    }

    REQUIRED_RELEASE_ASSETS
        .iter()
        .all(|required| release_has_uploaded_asset(json, required))
}

pub(crate) fn release_is_publishable(json: &serde_json::Value) -> bool {
    !json
        .get("draft")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        && !json
            .get("prerelease")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
}

pub(crate) fn release_has_uploaded_asset(json: &serde_json::Value, required: &str) -> bool {
    let Some(assets) = json.get("assets").and_then(serde_json::Value::as_array) else {
        return false;
    };
    assets.iter().any(|asset| {
        asset.get("name").and_then(serde_json::Value::as_str) == Some(required)
            && asset.get("state").and_then(serde_json::Value::as_str) == Some("uploaded")
    })
}

#[cfg(test)]
mod version_tests {
    use super::version_hint_from_latest_tag;

    #[test]
    fn version_hint_keeps_a_newer_development_or_stamped_build() {
        for current in ["0.9.12-pre", "0.9.12-beta.2", "0.9.12 (abc123)", "0.9.12"] {
            assert!(
                version_hint_from_latest_tag("v0.9.11", current).is_none(),
                "{current}"
            );
        }
    }

    #[test]
    fn version_hint_offers_stable_after_its_prerelease() {
        let notice = version_hint_from_latest_tag("v0.9.12", "0.9.12-beta.2")
            .expect("the stable release is newer than its beta");
        assert_eq!(notice.latest, "0.9.12");
        assert_eq!(notice.current, "0.9.12-beta.2");
    }

    #[test]
    fn version_hint_does_not_replace_stable_with_a_prerelease() {
        assert!(version_hint_from_latest_tag("v0.9.12-beta.2", "0.9.12").is_none());
        assert!(version_hint_from_latest_tag("v0.9.12", "0.9.12 (abc123)").is_none());
    }

    #[test]
    fn version_hint_does_not_guess_when_a_version_is_invalid() {
        for (latest, current) in [("nightly", "0.9.12"), ("0.9.12", "unknown")] {
            assert!(version_hint_from_latest_tag(latest, current).is_none());
        }
    }
}
