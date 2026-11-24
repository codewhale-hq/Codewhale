//! Cloud facts client: fetch `https://codewhale.net/api/facts/v1/<channel>`,
//! verify the Ed25519 envelope against the keys pinned in
//! `codewhale_config::cloud_facts::keys`, cache it under
//! `$CODEWHALE_HOME/facts/cloud-facts.json`, and install the scoped view as the
//! process-wide overlay. Modeled on the TUI's `models_dev_live` producer.
//!
//! Guarantees:
//! - Never a startup dependency: [`maybe_load_persisted_cache`] is a bounded
//!   synchronous disk read; all network happens in [`spawn_background_refresh`].
//! - Off by default (`[cloud_facts].enabled = false`); `CODEWHALE_CLOUD_FACTS=1`
//!   flips it, `CODEWHALE_DISABLE_CLOUD_FACTS=1` beats everything, CI markers
//!   suppress the fetch.
//! - The disk cache is re-verified on every load; untrusted bytes are cleared while the rollback floor is retained.
//! - The fetch sends only a fixed user agent and `If-None-Match`; no
//!   identifiers, cookies, or query parameters (PRD §5).
//! - With no active pinned key the layer is inert even when enabled.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use codewhale_config::catalog::now_unix;
use codewhale_config::cloud_facts::{
    CloudFactsState, CloudFactsStatus, FactsOrigin, FactsRejection, TrustedKey, VerifiedFacts,
    overlay, scoped_view, verify_envelope,
};
use codewhale_config::persistence::atomic_write;
use serde::{Deserialize, Serialize};

pub mod registry;
pub use registry::RegistrySettings;

/// `{channel}` is replaced with the channel slug.
pub const DEFAULT_URL_TEMPLATE: &str = "https://codewhale.net/api/facts/v1/{channel}";
/// Refresh interval for a verified payload (6 h).
pub const DEFAULT_TTL_SECS: u64 = 6 * 60 * 60;
/// Bounded HTTP budget.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest response body accepted.
pub const MAX_BODY_BYTES: usize = codewhale_config::cloud_facts::MAX_ENVELOPE_BYTES;
/// Fixed, identifier-free user agent.
pub const USER_AGENT: &str = concat!("CodeWhale/", env!("CARGO_PKG_VERSION"), " (+cloud-facts)");
/// State subdir + file under `$CODEWHALE_HOME`.
pub const STATE_SUBDIR: &str = "facts";
pub const CACHE_FILE: &str = "cloud-facts.json";
const CACHE_SCHEMA_VERSION: u32 = 2;
const MAX_SOURCE_BYTES: usize = 4096;
const MAX_CACHE_BYTES: usize = MAX_BODY_BYTES * 6 + 32 * 1024;
static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const BACKOFF_BASE_SECS: u64 = 10 * 60;

/// Env: `1`/`0` overrides `[cloud_facts].enabled`.
pub const ENV_ENABLED: &str = "CODEWHALE_CLOUD_FACTS";
/// Env: hard kill switch (truthy) — beats config and `ENV_ENABLED`.
pub const ENV_DISABLE: &str = "CODEWHALE_DISABLE_CLOUD_FACTS";
/// Env: full URL override (may contain `{channel}`).
pub const ENV_URL: &str = "CODEWHALE_CLOUD_FACTS_URL";
/// Env: channel slug override.
pub const ENV_CHANNEL: &str = "CODEWHALE_CLOUD_FACTS_CHANNEL";
/// Env: read the envelope from a local file instead of the network.
pub const ENV_PATH: &str = "CODEWHALE_CLOUD_FACTS_PATH";
const CI_MARKERS: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "JENKINS_URL",
    "TEAMCITY_VERSION",
    "TF_BUILD",
];

/// Resolved runtime settings (config + env).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub channel: String,
    pub url: Option<String>,
    pub ttl_secs: u64,
    /// Explicit cache file (tests); otherwise `$CODEWHALE_HOME/facts/cloud-facts.json`.
    pub cache_path: Option<PathBuf>,
    /// Local envelope path (`ENV_PATH`); skips the network.
    pub local_path: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            channel: "stable".to_string(),
            url: None,
            ttl_secs: DEFAULT_TTL_SECS,
            cache_path: None,
            local_path: None,
        }
    }
}

fn env_truthy(name: &str) -> Option<bool> {
    let value = std::env::var(name).ok()?;
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

impl Settings {
    /// Apply env overrides on top of config-derived settings.
    #[must_use]
    pub fn resolve(mut self) -> Self {
        if let Some(enabled) = env_truthy(ENV_ENABLED) {
            self.enabled = enabled;
        }
        if let Ok(channel) = std::env::var(ENV_CHANNEL) {
            let channel = channel.trim();
            if valid_channel(channel) {
                self.channel = channel.to_string();
            }
        }
        if let Ok(url) = std::env::var(ENV_URL) {
            let url = url.trim();
            if !url.is_empty() {
                self.url = Some(url.to_string());
            }
        }
        if let Ok(path) = std::env::var(ENV_PATH) {
            let path = path.trim();
            if !path.is_empty() {
                self.local_path = Some(PathBuf::from(path));
            }
        }
        if hard_disabled() {
            self.enabled = false;
        }
        self.ttl_secs = self.ttl_secs.max(60);
        self
    }

    /// The effective envelope URL.
    #[must_use]
    pub fn url(&self) -> String {
        self.url
            .as_deref()
            .unwrap_or(DEFAULT_URL_TEMPLATE)
            .replace("{channel}", &self.channel)
    }

    fn cache_file(&self) -> Option<PathBuf> {
        self.cache_path.clone().or_else(|| {
            let path = cache_path()?;
            if self.channel == "stable" {
                Some(path)
            } else {
                Some(path.with_file_name(format!("cloud-facts-{}.json", self.channel)))
            }
        })
    }
}

/// Channel slugs are `[a-z0-9][a-z0-9-]{0,31}`.
#[must_use]
pub fn valid_channel(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    (1..=32).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// Production network policy. Local fixtures inject their transport explicitly.
#[must_use]
pub fn fetch_suppressed() -> bool {
    CI_MARKERS.iter().any(|name| {
        env_truthy(name).unwrap_or_else(|| std::env::var(name).is_ok_and(|v| !v.trim().is_empty()))
    })
}

/// Default cache path under the CodeWhale state root.
#[must_use]
pub fn cache_path() -> Option<PathBuf> {
    codewhale_config::resolve_state_dir(STATE_SUBDIR)
        .ok()
        .map(|dir| dir.join(CACHE_FILE))
}

fn hard_disabled() -> bool {
    overlay::hard_disabled()
}

fn source(settings: &Settings) -> Result<String, RefreshError> {
    if !valid_channel(&settings.channel) {
        return Err(RefreshError::InvalidSettings("invalid channel".into()));
    }
    if let Some(path) = &settings.local_path {
        let path = if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir()
                .map_err(|e| RefreshError::Io(e.to_string()))?
                .join(path)
        };
        let value = format!("file:{}", path.display());
        if value.len() > MAX_SOURCE_BYTES {
            return Err(RefreshError::TooLarge(value.len()));
        }
        return Ok(value);
    }
    checked_url(&settings.url())
}

fn checked_url(raw: &str) -> Result<String, RefreshError> {
    if raw.len() > MAX_SOURCE_BYTES {
        return Err(RefreshError::TooLarge(raw.len()));
    }
    let url = reqwest::Url::parse(raw)
        .map_err(|_| RefreshError::InvalidSettings("invalid URL".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return Err(RefreshError::InvalidSettings(
            "URL must be HTTP(S), without credentials, query or fragment".into(),
        ));
    }
    Ok(url.to_string())
}

fn source_identity(settings: &Settings, keys: &[TrustedKey]) -> Result<String, RefreshError> {
    // Trust inputs are public pins, not credentials. Including them invalidates
    // a previously issued ticket when a test or a future reload changes trust.
    Ok(format!(
        "{}\n{}\n{}\n{}\n{:?}",
        settings.channel,
        source(settings)?,
        settings.ttl_secs,
        codewhale_config::cloud_facts::current_version(),
        keys
    ))
}

/// Publish admitted settings synchronously, before spawning work. Refreshing
/// an old Settings value can never re-enable or change this authority.
pub fn configure(settings: &Settings) {
    configure_with_keys(settings, codewhale_config::cloud_facts::TRUSTED_KEYS);
}

fn configure_with_keys(settings: &Settings, keys: &[TrustedKey]) {
    if !settings.enabled || hard_disabled() {
        overlay::configure(false, "");
        return;
    }
    let identity = match source_identity(settings, keys) {
        Ok(identity) => identity,
        Err(_) => {
            overlay::configure(false, "");
            return;
        }
    };
    if let Some(ticket) = overlay::configure(true, &identity)
        && !keys
            .iter()
            .any(|key| key.status == codewhale_config::cloud_facts::KeyStatus::Active)
    {
        overlay::publish(
            &ticket,
            None,
            state_status(CloudFactsState::Inert, None, ""),
        );
    }
}

fn ticket(
    settings: &Settings,
    keys: &[TrustedKey],
) -> Result<overlay::OverlayTicket, RefreshError> {
    if !settings.enabled || hard_disabled() {
        return Err(RefreshError::Disabled);
    }
    if !keys
        .iter()
        .any(|key| key.status == codewhale_config::cloud_facts::KeyStatus::Active)
    {
        return Err(RefreshError::Inert);
    }
    overlay::current_ticket(&source_identity(settings, keys)?).ok_or(RefreshError::Superseded)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedCache {
    schema_version: u32,
    channel: String,
    url: String,
    #[serde(default)]
    source_identity: String,
    fetched_at: u64,
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    highest_seen_version: Option<u64>,
    #[serde(default)]
    backoff_until: Option<u64>,
    #[serde(default)]
    failures: u32,
    #[serde(default)]
    envelope: String,
}

fn read_bounded_regular(path: &Path, limit: usize) -> Result<Vec<u8>, RefreshError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(0x0020_0000);
    }
    let file = options
        .open(path)
        .map_err(|e| RefreshError::Io(e.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|e| RefreshError::Io(e.to_string()))?;
    let mut regular = metadata.is_file();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        regular &= metadata.nlink() == 1;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: this is the live handle already opened without following
        // reparse points; `information` is writable for the synchronous call.
        let inspected =
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
        regular &= metadata.file_attributes() & 0x0000_0400 == 0
            && inspected != 0
            && information.nNumberOfLinks == 1;
    }
    if !regular {
        return Err(RefreshError::Io(
            "facts file must be a regular file with one link".into(),
        ));
    }
    if metadata.len() > limit as u64 {
        return Err(RefreshError::TooLarge(limit.saturating_add(1)));
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| RefreshError::Io(e.to_string()))?;
    if bytes.len() > limit {
        return Err(RefreshError::TooLarge(bytes.len()));
    }
    Ok(bytes)
}

fn load_cache(path: &Path) -> Option<PersistedCache> {
    let bytes = read_bounded_regular(path, MAX_CACHE_BYTES).ok()?;
    let cache: PersistedCache = serde_json::from_slice(&bytes).ok()?;
    (cache.schema_version == CACHE_SCHEMA_VERSION
        && valid_channel(&cache.channel)
        && cache.envelope.len() <= MAX_BODY_BYTES
        && cache.url.len() <= MAX_SOURCE_BYTES
        && cache.source_identity.len() <= 16 * MAX_SOURCE_BYTES
        && cache
            .etag
            .as_ref()
            .is_none_or(|etag| etag.len() <= MAX_SOURCE_BYTES))
    .then_some(cache)
}

fn save_cache(path: &Path, cache: &PersistedCache) {
    if cache.envelope.len() > MAX_BODY_BYTES
        || cache.url.len() > MAX_SOURCE_BYTES
        || cache.source_identity.len() > 16 * MAX_SOURCE_BYTES
        || cache
            .etag
            .as_ref()
            .is_some_and(|etag| etag.len() > MAX_SOURCE_BYTES)
    {
        return;
    }
    if let Ok(bytes) = serde_json::to_vec(cache)
        && bytes.len() <= MAX_CACHE_BYTES
        && let Err(err) = atomic_write(path, &bytes)
    {
        tracing::debug!(target: "cloud_facts", error = %err, "cache write failed");
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshError {
    Disabled,
    Inert,
    Suppressed,
    Superseded,
    InvalidSettings(String),
    BackingOff { until: u64 },
    Network(String),
    HttpStatus(u16),
    TooLarge(usize),
    Rejected(FactsRejection),
    Io(String),
}
impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => write!(f, "cloud facts disabled"),
            Self::Inert => write!(f, "no active trusted key"),
            Self::Suppressed => write!(f, "production network fetch suppressed by CI"),
            Self::Superseded => write!(f, "facts settings changed before publication"),
            Self::InvalidSettings(e) => write!(f, "invalid settings: {e}"),
            Self::BackingOff { until } => write!(f, "backing off until {until}"),
            Self::Network(e) => write!(f, "network: {e}"),
            Self::HttpStatus(code) => write!(f, "HTTP {code}"),
            Self::TooLarge(n) => write!(f, "response too large ({n} bytes)"),
            Self::Rejected(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "io: {e}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    NotModified { facts_version: Option<u64> },
    Updated { facts_version: u64 },
    Fresh { facts_version: Option<u64> },
    NoFacts,
}

fn state_status(
    state: CloudFactsState,
    etag: Option<String>,
    source_label: &str,
) -> CloudFactsStatus {
    CloudFactsStatus {
        state,
        last_attempt: Some(now_unix()),
        etag,
        source_label: source_label.into(),
    }
}

fn verify_with(
    bytes: &[u8],
    settings: &Settings,
    highest: Option<u64>,
    keys: &[TrustedKey],
    now: u64,
) -> Result<VerifiedFacts, FactsRejection> {
    verify_envelope(
        bytes,
        &settings.channel,
        &codewhale_config::cloud_facts::current_version(),
        highest.max(overlay::highest_seen(&settings.channel)),
        keys,
        now,
    )
}

fn publish_verified(
    ticket: &overlay::OverlayTicket,
    verified: &VerifiedFacts,
    origin: FactsOrigin,
    cache: &PersistedCache,
    path: Option<&Path>,
    now: u64,
) -> Result<u64, RefreshError> {
    let scoped = scoped_view(
        verified,
        &codewhale_config::cloud_facts::current_version(),
        now,
    );
    let (patches, defaults, announcements) = scoped.item_counts();
    let version = scoped.facts_version;
    let status = state_status(
        CloudFactsState::Verified {
            channel: scoped.channel.clone(),
            facts_version: version,
            key_id: scoped.key_id.clone(),
            fetched_at: cache.fetched_at,
            origin,
            stale: scoped.stale,
            patches,
            defaults,
            announcements,
        },
        cache.etag.clone(),
        &cache.url,
    );
    if overlay::publish_with(ticket, Some(scoped), status, || {
        if let Some(path) = path {
            save_cache(path, cache);
        }
    }) {
        Ok(version)
    } else {
        Err(RefreshError::Superseded)
    }
}

fn cache_for(settings: &Settings, keys: &[TrustedKey]) -> Result<PersistedCache, RefreshError> {
    let identity = source_identity(settings, keys)?;
    let mut cache = settings
        .cache_file()
        .as_deref()
        .and_then(load_cache)
        .filter(|cache| cache.channel == settings.channel)
        .unwrap_or_default();
    // The persisted high-water hint is unsigned. Recover any stronger floor
    // from the authenticated body before a source switch discards its bytes,
    // including refreshes that did not first seed the process overlay.
    if !cache.envelope.is_empty()
        && let Ok(verified) =
            verify_with(cache.envelope.as_bytes(), settings, None, keys, now_unix())
    {
        cache.highest_seen_version = cache
            .highest_seen_version
            .max(Some(verified.facts.facts_version));
    }
    if cache.source_identity != identity {
        // Retain the channel rollback floor while discarding another source's
        // validators, body and retry state. Channels use separate default files.
        let floor = cache.highest_seen_version;
        cache = PersistedCache {
            highest_seen_version: floor,
            ..PersistedCache::default()
        };
    }
    cache.highest_seen_version = cache
        .highest_seen_version
        .max(overlay::highest_seen(&settings.channel));
    // Local cache metadata is only a hint. A forged/future timestamp must
    // not grant an indefinitely fresh view or suppress all future refreshes.
    let now = now_unix();
    if cache.fetched_at > now {
        cache.fetched_at = 0;
        cache.backoff_until = None;
    }
    let max_backoff = (BACKOFF_BASE_SECS << 9).min(settings.ttl_secs);
    if cache
        .backoff_until
        .is_some_and(|until| until > now.saturating_add(max_backoff))
    {
        cache.backoff_until = None;
    }
    cache.schema_version = CACHE_SCHEMA_VERSION;
    cache.channel = settings.channel.clone();
    cache.source_identity = identity;
    cache.url = source(settings)?;
    Ok(cache)
}

pub fn maybe_load_persisted_cache(settings: &Settings) -> Option<u64> {
    maybe_load_persisted_cache_with_keys(settings, codewhale_config::cloud_facts::TRUSTED_KEYS)
}
fn maybe_load_persisted_cache_with_keys(settings: &Settings, keys: &[TrustedKey]) -> Option<u64> {
    let ticket = ticket(settings, keys).ok()?;
    registry::load_cached_registry(&RegistrySettings::from_facts(settings), &ticket, keys);
    let cache = cache_for(settings, keys).ok()?;
    if cache.envelope.is_empty() {
        return None;
    }
    let now = now_unix();
    match verify_with(
        cache.envelope.as_bytes(),
        settings,
        cache.highest_seen_version,
        keys,
        now,
    ) {
        Ok(mut verified) => {
            verified.stale |= now.saturating_sub(cache.fetched_at) >= settings.ttl_secs;
            publish_verified(
                &ticket,
                &verified,
                FactsOrigin::DiskCache,
                &cache,
                None,
                now,
            )
            .ok()
        }
        Err(reason) => {
            // Retain the channel's rollback floor, replacing the rejected body
            // through the same generation-guarded cache publication boundary.
            let mut rejected = cache.clone();
            rejected.envelope.clear();
            rejected.etag = None;
            overlay::publish_with(
                &ticket,
                None,
                state_status(
                    CloudFactsState::Rejected {
                        reason: reason.to_string(),
                        at: now,
                    },
                    None,
                    &cache.url,
                ),
                || {
                    if let Some(path) = settings.cache_file() {
                        save_cache(&path, &rejected);
                    }
                },
            );
            None
        }
    }
}

enum Fetched {
    NotModified,
    NotFound,
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
    },
}

async fn fetch(url: String, etag: Option<String>) -> Result<Fetched, RefreshError> {
    fetch_limited(url, etag, MAX_BODY_BYTES).await
}

async fn fetch_limited(
    url: String,
    etag: Option<String>,
    limit: usize,
) -> Result<Fetched, RefreshError> {
    let client = codewhale_release::tls::reqwest_client_builder()
        .timeout(FETCH_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| RefreshError::Network(e.to_string()))?;
    let mut request = client.get(url).header("Accept", "application/json");
    if let Some(etag) = etag {
        request = request.header("If-None-Match", etag);
    }
    let mut response = request
        .send()
        .await
        .map_err(|e| RefreshError::Network(e.to_string()))?;
    let status = response.status().as_u16();
    if status == 304 {
        return Ok(Fetched::NotModified);
    }
    if status == 404 {
        return Ok(Fetched::NotFound);
    }
    if !(200..300).contains(&status) {
        return Err(RefreshError::HttpStatus(status));
    }
    if response
        .content_length()
        .is_some_and(|len| len > limit as u64)
    {
        return Err(RefreshError::TooLarge(limit + 1));
    }
    let etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .filter(|value| value.len() <= MAX_SOURCE_BYTES)
        .map(str::to_string);
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| RefreshError::Network(e.to_string()))?
    {
        let size = bytes.len().saturating_add(chunk.len());
        if size > limit {
            return Err(RefreshError::TooLarge(size));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Fetched::Body { bytes, etag })
}

/// Uses only admitted settings; callers must configure synchronously first.
pub async fn refresh(settings: &Settings, force: bool) -> Result<RefreshOutcome, RefreshError> {
    if let Ok(admitted) = ticket(settings, codewhale_config::cloud_facts::TRUSTED_KEYS) {
        registry::spawn_registry_refresh(RegistrySettings::from_facts(settings), admitted, None);
    }
    refresh_with_keys(settings, force, codewhale_config::cloud_facts::TRUSTED_KEYS).await
}
async fn refresh_with_keys(
    settings: &Settings,
    force: bool,
    keys: &[TrustedKey],
) -> Result<RefreshOutcome, RefreshError> {
    refresh_using(settings, force, keys, None, fetch_suppressed(), fetch).await
}

// Tests inject an explicit transport/policy, leaving the production CI gate
// intact. Their dependencies cannot override settings/trust admission.
async fn refresh_using<F, Fut>(
    settings: &Settings,
    force: bool,
    keys: &[TrustedKey],
    admitted: Option<overlay::OverlayTicket>,
    suppress_network: bool,
    transport: F,
) -> Result<RefreshOutcome, RefreshError>
where
    F: FnOnce(String, Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Fetched, RefreshError>>,
{
    let ticket = admitted.map(Ok).unwrap_or_else(|| ticket(settings, keys))?;
    let _refresh = REFRESH_LOCK.lock().await;
    // A queued old request must fail without even reading a file.
    let _ = self::ticket(settings, keys)?;
    if !overlay::is_current(&ticket) {
        return Err(RefreshError::Superseded);
    }
    let mut cache = cache_for(settings, keys)?;
    let path = settings.cache_file();
    let now = now_unix();
    let fetched = if let Some(local) = &settings.local_path {
        read_bounded_regular(local, MAX_BODY_BYTES).map(|bytes| Fetched::Body { bytes, etag: None })
    } else {
        if suppress_network {
            return Err(RefreshError::Suppressed);
        }
        if !force {
            if let Some(until) = cache.backoff_until
                && now < until
            {
                return Err(RefreshError::BackingOff { until });
            }
            if !cache.envelope.is_empty()
                && now.saturating_sub(cache.fetched_at) < settings.ttl_secs
                && let Ok(verified) = verify_with(
                    cache.envelope.as_bytes(),
                    settings,
                    cache.highest_seen_version,
                    keys,
                    now,
                )
            {
                let version = publish_verified(
                    &ticket,
                    &verified,
                    FactsOrigin::DiskCache,
                    &cache,
                    None,
                    now,
                )?;
                return Ok(RefreshOutcome::Fresh {
                    facts_version: Some(version),
                });
            }
        }
        transport(
            cache.url.clone(),
            cache.etag.clone().filter(|_| !cache.envelope.is_empty()),
        )
        .await
    };
    let mut not_modified = false;
    let (bytes, etag) = match fetched {
        Ok(Fetched::NotModified) => {
            not_modified = true;
            (cache.envelope.as_bytes().to_vec(), cache.etag.clone())
        }
        Ok(Fetched::NotFound) => {
            cache.envelope.clear();
            cache.etag = None;
            cache.failures = 0;
            cache.backoff_until = None;
            cache.fetched_at = now;
            if !overlay::publish_with(
                &ticket,
                None,
                state_status(CloudFactsState::BundledOnly, None, &cache.url),
                || {
                    if let Some(path) = &path {
                        save_cache(path, &cache);
                    }
                },
            ) {
                return Err(RefreshError::Superseded);
            }
            return Ok(RefreshOutcome::NoFacts);
        }
        Ok(Fetched::Body { bytes, etag }) => (bytes, etag),
        Err(err) => {
            cache.failures = cache.failures.saturating_add(1);
            cache.backoff_until = Some(
                now.saturating_add(
                    (BACKOFF_BASE_SECS << cache.failures.min(10).saturating_sub(1))
                        .min(settings.ttl_secs),
                ),
            );
            // Reverify retained facts on failure too; the status must never
            // hide a revoked or expired overlay behind a prior successful fetch.
            let kept = verify_with(
                cache.envelope.as_bytes(),
                settings,
                cache.highest_seen_version,
                keys,
                now,
            )
            .ok()
            .map(|mut verified| {
                verified.stale |= now.saturating_sub(cache.fetched_at) >= settings.ttl_secs;
                scoped_view(
                    &verified,
                    &codewhale_config::cloud_facts::current_version(),
                    now,
                )
            });
            let keeping = kept
                .as_ref()
                .filter(|facts| !facts.stale)
                .map(|facts| facts.facts_version);
            if !overlay::publish_with(
                &ticket,
                kept,
                state_status(
                    CloudFactsState::Failed {
                        last_error: err.to_string(),
                        at: now,
                        keeping,
                    },
                    cache.etag.clone(),
                    &cache.url,
                ),
                || {
                    if let Some(path) = &path {
                        save_cache(path, &cache);
                    }
                },
            ) {
                return Err(RefreshError::Superseded);
            }
            return Err(err);
        }
    };
    // 304 is a transport optimization, never a trust decision. This also
    // rejects a 304 without an authenticated matching cached envelope.
    match verify_with(
        &bytes,
        settings,
        cache.highest_seen_version,
        keys,
        now_unix(),
    ) {
        Ok(verified) => {
            cache.fetched_at = now_unix();
            cache.etag = etag;
            cache.failures = 0;
            cache.backoff_until = None;
            cache.highest_seen_version = Some(
                cache
                    .highest_seen_version
                    .unwrap_or(0)
                    .max(verified.facts.facts_version),
            );
            cache.envelope =
                String::from_utf8(bytes).map_err(|e| RefreshError::Io(e.to_string()))?;
            let origin = if settings.local_path.is_some() {
                FactsOrigin::LocalFile
            } else {
                FactsOrigin::Network
            };
            let version = publish_verified(
                &ticket,
                &verified,
                origin,
                &cache,
                path.as_deref(),
                now_unix(),
            )?;
            Ok(if not_modified {
                RefreshOutcome::NotModified {
                    facts_version: Some(version),
                }
            } else {
                RefreshOutcome::Updated {
                    facts_version: version,
                }
            })
        }
        Err(reason) => {
            // Drop the now-untrusted body, retain rollback floor, and do not
            // let the next response reuse its ETag.
            cache.envelope.clear();
            cache.etag = None;
            let status = match &reason {
                FactsRejection::NotApplicable { applies_to } => CloudFactsState::NotApplicable {
                    applies_to: applies_to.clone(),
                },
                _ => CloudFactsState::Rejected {
                    reason: reason.to_string(),
                    at: now_unix(),
                },
            };
            if !overlay::publish_with(
                &ticket,
                None,
                state_status(status, None, &cache.url),
                || {
                    if let Some(path) = &path {
                        save_cache(path, &cache);
                    }
                },
            ) {
                return Err(RefreshError::Superseded);
            }
            Err(RefreshError::Rejected(reason))
        }
    }
}

pub fn spawn_background_refresh(
    settings: Settings,
    on_update: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    let Ok(admitted) = ticket(&settings, codewhale_config::cloud_facts::TRUSTED_KEYS) else {
        return;
    };
    registry::spawn_registry_refresh(
        RegistrySettings::from_facts(&settings),
        admitted.clone(),
        on_update.clone(),
    );
    if settings.local_path.is_none() && fetch_suppressed() {
        return;
    }
    tokio::spawn(async move {
        let before = overlay::snapshot().generation;
        let outcome = refresh_using(
            &settings,
            false,
            codewhale_config::cloud_facts::TRUSTED_KEYS,
            Some(admitted),
            fetch_suppressed(),
            fetch,
        )
        .await;
        tracing::debug!(target: "cloud_facts", ?outcome, "cloud facts refresh settled");
        if overlay::snapshot().generation != before
            && let Some(hook) = on_update
        {
            hook();
        }
    });
}

#[must_use]
pub fn status() -> CloudFactsStatus {
    overlay::status()
}

#[cfg(test)]
mod tests;
