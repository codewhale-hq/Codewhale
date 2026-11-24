use std::path::{Path, PathBuf};
use std::sync::Arc;

use codewhale_config::catalog::now_unix;
use codewhale_config::cloud_facts::registry::{
    AdmittedRegistry, BuiltinNames, MAX_REGISTRY_ENVELOPE_BYTES, REGISTRY_CHANNEL, REGISTRY_SCHEMA,
    admit_registry, verify_registry_envelope,
};
use codewhale_config::cloud_facts::{FactsRejection, TrustedKey, overlay};
use codewhale_config::persistence::atomic_write;
use serde::{Deserialize, Serialize};

use super::{
    Fetched, RefreshError, STATE_SUBDIR, Settings, checked_url, fetch_envelope, fetch_limited,
    fetch_suppressed, off_runtime, publish_off_runtime, read_bounded_regular,
};

pub const ENV_REGISTRY_URL: &str = "CODEWHALE_REGISTRY_URL";
pub const ENV_REGISTRY_PATH: &str = "CODEWHALE_REGISTRY_PATH";
pub const REGISTRY_CACHE_FILE: &str = "registry-cache.json";
const REGISTRY_CACHE_SCHEMA: u32 = 1;
const MAX_REGISTRY_CACHE_BYTES: usize = MAX_REGISTRY_ENVELOPE_BYTES + 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RegistrySettings {
    pub enabled: bool,
    pub url: Option<String>,
    pub local_path: Option<PathBuf>,
    pub cache_path: Option<PathBuf>,
}

#[must_use]
pub fn default_registry_url(facts_url: Option<&str>) -> String {
    let template = facts_url
        .map(str::trim)
        .filter(|url| url.contains("{channel}"))
        .unwrap_or(super::DEFAULT_URL_TEMPLATE);
    template.replace("{channel}", REGISTRY_CHANNEL)
}

impl RegistrySettings {
    #[must_use]
    pub fn from_facts(settings: &Settings) -> Self {
        let env = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let local_path = env(ENV_REGISTRY_PATH).map(PathBuf::from);
        let explicit_url =
            env(ENV_REGISTRY_URL).map(|url| url.replace("{channel}", REGISTRY_CHANNEL));
        let url = if local_path.is_some() {
            explicit_url
        } else {
            explicit_url.or_else(|| {
                let facts_url = settings.url.clone().or_else(|| env(super::ENV_URL));
                Some(default_registry_url(facts_url.as_deref()))
            })
        };
        Self {
            enabled: settings.enabled && !overlay::hard_disabled(),
            url,
            local_path,
            cache_path: settings
                .cache_path
                .as_ref()
                .map(|path| path.with_file_name(REGISTRY_CACHE_FILE)),
        }
    }

    #[must_use]
    pub fn active(&self) -> bool {
        self.enabled
            && !overlay::hard_disabled()
            && (self.url.is_some() || self.local_path.is_some())
    }

    fn source(&self) -> String {
        match (&self.local_path, &self.url) {
            (Some(path), _) => format!("file:{}", path.display()),
            (None, Some(url)) => url.clone(),
            (None, None) => String::new(),
        }
    }

    fn cache_file(&self) -> Option<PathBuf> {
        self.cache_path.clone().or_else(|| {
            codewhale_config::resolve_state_dir(STATE_SUBDIR)
                .ok()
                .map(|dir| dir.join(REGISTRY_CACHE_FILE))
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RegistryCache {
    schema_version: u32,
    #[serde(default)]
    source: String,
    #[serde(default)]
    highest_revision: Option<u64>,
    #[serde(default)]
    fetched_at: u64,
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    envelope: String,
}

fn load_cache(path: Option<&Path>) -> RegistryCache {
    path.and_then(|path| read_bounded_regular(path, MAX_REGISTRY_CACHE_BYTES).ok())
        .and_then(|bytes| serde_json::from_slice::<RegistryCache>(&bytes).ok())
        .filter(|cache| {
            cache.schema_version == REGISTRY_CACHE_SCHEMA
                && cache.envelope.len() <= MAX_REGISTRY_ENVELOPE_BYTES
        })
        .unwrap_or_default()
}

fn save_cache(path: Option<&Path>, cache: &RegistryCache) {
    let Some(path) = path else {
        return;
    };
    if let Ok(bytes) = serde_json::to_vec(cache)
        && bytes.len() <= MAX_REGISTRY_CACHE_BYTES
        && let Err(err) = atomic_write(path, &bytes)
    {
        tracing::debug!(target: "cloud_facts", error = %err, "registry cache write failed");
    }
}

fn cache_for(settings: &RegistrySettings, path: Option<&Path>) -> RegistryCache {
    let source = settings.source();
    let mut cache = load_cache(path);
    if cache.source != source {
        cache = RegistryCache {
            highest_revision: cache.highest_revision,
            ..RegistryCache::default()
        };
    }
    cache.schema_version = REGISTRY_CACHE_SCHEMA;
    cache.source = source;
    cache.highest_revision = cache
        .highest_revision
        .max(overlay::highest_seen(REGISTRY_SCHEMA));
    cache
}

fn verify_and_admit(
    bytes: &[u8],
    floor: Option<u64>,
    keys: &[TrustedKey],
) -> Result<AdmittedRegistry, FactsRejection> {
    let verified = verify_registry_envelope(bytes, floor, keys, now_unix())?;
    Ok(admit_registry(verified, &BuiltinNames::bundled()))
}

pub(crate) fn load_cached_registry(
    settings: &RegistrySettings,
    ticket: &overlay::OverlayTicket,
    keys: &[TrustedKey],
) -> Option<u64> {
    if !settings.active() {
        overlay::publish_registry(ticket, None, |_| {});
        return None;
    }
    let path = settings.cache_file();
    let cache = cache_for(settings, path.as_deref());
    if cache.envelope.is_empty() {
        overlay::publish_registry(ticket, None, |_| {});
        return None;
    }
    match verify_and_admit(cache.envelope.as_bytes(), cache.highest_revision, keys) {
        Ok(admitted) => {
            let revision = admitted.revision;
            overlay::publish_registry(ticket, Some(Arc::new(admitted)), |_| {}).then_some(revision)
        }
        Err(FactsRejection::Rollback { .. }) => None,
        Err(reason) => {
            tracing::debug!(target: "cloud_facts", %reason, "cached registry rejected");
            overlay::publish_registry(ticket, None, |floor| {
                let mut current = load_cache(path.as_deref());
                if current.envelope == cache.envelope {
                    current.envelope.clear();
                    current.etag = None;
                    current.highest_revision = current.highest_revision.max(floor);
                    save_cache(path.as_deref(), &current);
                }
            });
            None
        }
    }
}

pub(crate) async fn refresh_registry_using<F, Fut>(
    settings: &RegistrySettings,
    ticket: &overlay::OverlayTicket,
    keys: &[TrustedKey],
    suppress_network: bool,
    transport: F,
) -> Result<Option<u64>, RefreshError>
where
    F: FnOnce(String, Option<String>, usize) -> Fut,
    Fut: std::future::Future<Output = Result<Fetched, RefreshError>>,
{
    if !settings.active() {
        return Err(RefreshError::Disabled);
    }
    let path = settings.cache_file();
    let mut cache = {
        let (settings, path) = (settings.clone(), path.clone());
        off_runtime(move || Ok(cache_for(&settings, path.as_deref()))).await?
    };
    let fetched = fetch_envelope(
        settings.local_path.clone(),
        MAX_REGISTRY_ENVELOPE_BYTES,
        suppress_network,
        || checked_url(settings.url.as_deref().unwrap_or_default()),
        cache.etag.clone().filter(|_| !cache.envelope.is_empty()),
        |url, etag| transport(url, etag, MAX_REGISTRY_ENVELOPE_BYTES),
    )
    .await?;
    let (bytes, etag) = match fetched {
        Fetched::NotModified => (cache.envelope.as_bytes().to_vec(), cache.etag.clone()),
        Fetched::NotFound => return Ok(None),
        Fetched::Body { bytes, etag } => (bytes, etag),
    };
    let admitted =
        verify_and_admit(&bytes, cache.highest_revision, keys).map_err(RefreshError::Rejected)?;
    let revision = admitted.revision;
    cache.fetched_at = now_unix();
    cache.etag = etag;
    cache.envelope = String::from_utf8(bytes).map_err(|e| RefreshError::Io(e.to_string()))?;
    let ticket = ticket.clone();
    publish_off_runtime(move || {
        overlay::publish_registry(&ticket, Some(Arc::new(admitted)), |floor| {
            cache.highest_revision = cache.highest_revision.max(floor);
            save_cache(path.as_deref(), &cache);
        })
    })
    .await?;
    Ok(Some(revision))
}

pub(crate) fn spawn_registry_refresh(
    settings: RegistrySettings,
    ticket: overlay::OverlayTicket,
    on_update: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    if !settings.active() || (settings.local_path.is_none() && fetch_suppressed()) {
        return;
    }
    tokio::spawn(async move {
        let before = overlay::snapshot().generation;
        let outcome = refresh_registry_using(
            &settings,
            &ticket,
            codewhale_config::cloud_facts::TRUSTED_KEYS,
            fetch_suppressed(),
            fetch_limited,
        )
        .await;
        tracing::debug!(target: "cloud_facts", ?outcome, "registry refresh settled");
        if overlay::snapshot().generation != before
            && let Some(hook) = on_update
        {
            hook();
        }
    });
}

#[cfg(test)]
mod tests;
