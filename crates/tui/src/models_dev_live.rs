//! Live Models.dev catalog fetch + secret-free disk cache (#4187).
//!
//! OpenCode-style producer that:
//! - reads a stale/fresh disk cache on startup (never blocks model selection),
//! - fetches `https://models.dev/catalog.json` in the background with a bounded
//!   timeout and explicit user-agent (no credentials),
//! - writes the cache atomically via temp file + rename,
//! - compiles parsed rows into `CatalogOffering`s and publishes them into
//!   [`crate::provider_lake`],
//! - falls back to the prior cache or the bundled snapshot on any failure.
//!
//! Override knobs (tests / dogfood):
//! - `CODEWHALE_MODELS_DEV_URL` — base URL (appends `/catalog.json`) or a full
//!   `*.json` URL.
//! - `CODEWHALE_MODELS_DEV_PATH` — local file path; skips the network.
//! - `CODEWHALE_DISABLE_MODELS_DEV_FETCH` — when truthy, never hits the network.

#[cfg(test)]
use codewhale_runtime::catalog_cache::CACHE_FILE;
use codewhale_runtime::catalog_cache::cache_path;
use std::path::Path;
use std::sync::RwLock;
use std::time::Duration;

use codewhale_config::catalog::{
    CatalogSnapshot, base_url_fingerprint, live_offerings_from_models_dev, now_unix,
};
use codewhale_config::models_dev::{MODELS_DEV_CATALOG_URL, ModelsDevCatalog};
use codewhale_config::persistence::atomic_write;
use serde::{Deserialize, Serialize};

/// Default TTL for a live Models.dev snapshot (24h, #4187 / #4114).
pub const DEFAULT_MODELS_DEV_TTL_SECS: u64 = 24 * 60 * 60;

/// Bounded HTTP timeout for the Models.dev fetch.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Explicit user-agent; no credentials, no session cookies.
pub const USER_AGENT: &str = concat!("CodeWhale/", env!("CARGO_PKG_VERSION"), " (+models-dev)");

/// Env: override Models.dev base URL or full catalog URL.
pub const ENV_MODELS_DEV_URL: &str = "CODEWHALE_MODELS_DEV_URL";
/// Env: load catalog JSON from a local path (skips network).
pub const ENV_MODELS_DEV_PATH: &str = "CODEWHALE_MODELS_DEV_PATH";
/// Env: disable network fetch entirely (`1`/`true`/`yes`/`on`).
pub const ENV_DISABLE_FETCH: &str = "CODEWHALE_DISABLE_MODELS_DEV_FETCH";

const CACHE_SCHEMA_VERSION: u32 = 1;
/// Largest catalog body accepted from the network, an override file, or the
/// disk cache. The public catalog is a few MiB; anything past this is refused
/// instead of being read whole into memory.
const MAX_CATALOG_BYTES: usize = 32 * 1024 * 1024;
/// Clock skew tolerated on a cache timestamp. A `fetched_at` further in the
/// future than this is not "age zero": it is untrustworthy, so the cache is
/// published as stale and the next refresh fetches.
const MAX_CACHE_CLOCK_SKEW_SECS: u64 = 5 * 60;

/// Provenance / freshness of the Models.dev live layer for UI chips (#4187).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelsDevFreshness {
    /// No live/cache layer; pickers see bundled rows only.
    #[default]
    Bundled,
    /// Live (or disk-cache) rows within TTL.
    Live,
    /// Disk-cache / prior live rows past TTL; still visible.
    Stale,
    /// Last refresh failed; prior/bundled rows remain available.
    Failed,
}

/// Where the rows a caller can currently see were produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelsDevProvenance {
    #[default]
    Bundled,
    Live,
    Cache,
    File,
}

impl ModelsDevProvenance {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Bundled => "bundled_snapshot",
            Self::Live => "models_dev_live",
            Self::Cache => "models_dev_cache",
            Self::File => "models_dev_file",
        }
    }
}

/// Quiet status snapshot for UI / `/model refresh` feedback.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelsDevStatus {
    pub freshness: ModelsDevFreshness,
    pub provenance: ModelsDevProvenance,
    pub offering_count: usize,
    pub fetched_at: Option<u64>,
    pub source_label: String,
    pub last_error: Option<String>,
}

static STATUS: RwLock<ModelsDevStatus> = RwLock::new(ModelsDevStatus {
    freshness: ModelsDevFreshness::Bundled,
    provenance: ModelsDevProvenance::Bundled,
    offering_count: 0,
    fetched_at: None,
    source_label: String::new(),
    last_error: None,
});

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedModelsDevCache {
    schema_version: u32,
    /// Unix seconds the payload was fetched (or loaded from an override path).
    fetched_at: u64,
    /// Fingerprint of the source URL/path this body was fetched from. It scopes
    /// the on-disk cache; it is deliberately not carried on the published rows,
    /// which describe a model rather than an endpoint (`ModelsDevLive`).
    source_fingerprint: String,
    /// Human-readable source label (URL or `file:…`); never a secret.
    source_label: String,
    /// Raw Models.dev catalog JSON body (secret-free by construction).
    body: String,
}

/// Metadata header for the v2 cache format.
///
/// v1 serialized the whole cache as one JSON envelope with the catalog body
/// escaped inside it, so loading parsed ~5MB twice (envelope, then body) plus
/// a full-body copy on every interactive boot. v2 stores the metadata as a
/// single JSON header line followed by the raw catalog body bytes, so boot
/// performs exactly one catalog parse and zero body copies.
const CACHE_SCHEMA_VERSION_V2: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedModelsDevCacheV2 {
    schema_version: u32,
    fetched_at: u64,
    source_fingerprint: String,
    source_label: String,
}

/// Why a Models.dev refresh did not publish new rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelsDevRefreshError {
    Disabled,
    Network(String),
    HttpStatus(u16),
    InvalidResponse(String),
    EmptyCatalog,
    Io(String),
}

impl std::fmt::Display for ModelsDevRefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => write!(f, "Models.dev fetch disabled"),
            Self::Network(msg) => write!(f, "network: {msg}"),
            Self::HttpStatus(code) => write!(f, "HTTP {code}"),
            Self::InvalidResponse(msg) => write!(f, "invalid response: {msg}"),
            Self::EmptyCatalog => write!(f, "empty catalog"),
            Self::Io(msg) => write!(f, "io: {msg}"),
        }
    }
}

/// Current quiet status (for UI / slash-command feedback).
#[must_use]
pub fn status() -> ModelsDevStatus {
    let current = STATUS.read().map(|guard| guard.clone()).unwrap_or_default();
    honor_bundled_staleness(
        current,
        codewhale_config::catalog::reviewed::bundled_source_fetched_at()
            .is_none_or(|fetched_at| !within_ttl(fetched_at, now_unix())),
    )
}

/// A Bundled-only report whose snapshot is itself past TTL reports `Stale`:
/// rows stay visible for offline use, but never as a current catalog (#A2).
fn honor_bundled_staleness(mut status: ModelsDevStatus, bundled_stale: bool) -> ModelsDevStatus {
    if status.freshness == ModelsDevFreshness::Bundled && bundled_stale {
        status.freshness = ModelsDevFreshness::Stale;
    }
    status
}

fn set_status(next: ModelsDevStatus) {
    if let Ok(mut guard) = STATUS.write() {
        *guard = next;
    }
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Resolve the catalog URL from env override or the Models.dev default.
#[must_use]
pub fn resolve_catalog_url() -> String {
    match std::env::var(ENV_MODELS_DEV_URL) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                MODELS_DEV_CATALOG_URL.to_string()
            } else if trimmed.ends_with(".json") {
                trimmed.to_string()
            } else {
                format!("{}/catalog.json", trimmed.trim_end_matches('/'))
            }
        }
        Err(_) => MODELS_DEV_CATALOG_URL.to_string(),
    }
}

/// Seed ProviderLake from the on-disk Models.dev cache before any picker read.
///
/// Missing / corrupt / empty caches are a no-op — bundled rows remain available.
/// Stale caches still publish (freshness = Stale) so offline startups keep the
/// last-known live rows.
pub fn maybe_load_persisted_cache() {
    let Some(path) = cache_path() else {
        return;
    };
    let Some(cache) = load_cache_file(&path) else {
        return;
    };
    // The cache belongs to the selected source. Another mirror or override
    // cannot supply fallback facts for this selection.
    if cache.source_fingerprint != current_source_fingerprint() {
        mark_failed(ModelsDevRefreshError::InvalidResponse(
            "cached catalog belongs to a different source".into(),
        ));
        return;
    }
    let freshness = if within_ttl(cache.fetched_at, now_unix()) {
        ModelsDevFreshness::Live
    } else {
        ModelsDevFreshness::Stale
    };
    if let Err(err) = publish_from_body(
        &cache.body,
        cache.fetched_at,
        cache.source_label.as_str(),
        freshness,
        ModelsDevProvenance::Cache,
    ) {
        tracing::debug!(
            target: "models_dev_live",
            error = %err,
            "persisted Models.dev cache failed to publish; keeping bundled"
        );
    }
}

/// Force a refresh: prefer `CODEWHALE_MODELS_DEV_PATH`, else network fetch.
///
/// On success, publishes to ProviderLake and updates the disk cache. On
/// failure, keeps any prior live/bundled rows and records a quiet Failed
/// status. A cache write that fails after a successful publish keeps the fresh
/// rows and reports only the persistence error.
pub async fn refresh(force_network: bool) -> Result<usize, ModelsDevRefreshError> {
    if let Ok(path) = std::env::var(ENV_MODELS_DEV_PATH) {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return refresh_from_path(Path::new(trimmed)).await;
        }
    }

    if env_truthy(ENV_DISABLE_FETCH) {
        mark_failed(ModelsDevRefreshError::Disabled);
        return Err(ModelsDevRefreshError::Disabled);
    }

    if !force_network {
        let current = status();
        if current.freshness == ModelsDevFreshness::Live
            && current
                .fetched_at
                .is_some_and(|ts| within_ttl(ts, now_unix()))
        {
            return Ok(current.offering_count);
        }
    }

    let url = resolve_catalog_url();
    let body = match fetch_catalog_body(&url).await {
        Ok(body) => body,
        Err(err) => {
            mark_failed(err.clone());
            return Err(err);
        }
    };
    let fetched_at = now_unix();
    let fingerprint = base_url_fingerprint(&url);
    let count = publish_from_body(
        &body,
        fetched_at,
        &url,
        ModelsDevFreshness::Live,
        ModelsDevProvenance::Live,
    )?;
    if let Some(path) = cache_path() {
        save_cache_file(
            &path,
            &PersistedModelsDevCache {
                schema_version: CACHE_SCHEMA_VERSION,
                fetched_at,
                source_fingerprint: fingerprint,
                source_label: url,
                body,
            },
        )
        .inspect_err(note_persistence_error)?;
    }
    Ok(count)
}

/// Whether a payload fetched at `fetched_at` is still inside the TTL at
/// `now`. A timestamp beyond the tolerated clock skew is never fresh: a
/// saturating age would read it as zero and suppress refresh until the clock
/// caught up.
fn within_ttl(fetched_at: u64, now: u64) -> bool {
    if fetched_at > now.saturating_add(MAX_CACHE_CLOCK_SKEW_SECS) {
        return false;
    }
    now.saturating_sub(fetched_at) <= DEFAULT_MODELS_DEV_TTL_SECS
}

/// Fingerprint of the source a refresh would read right now: the override
/// file when `CODEWHALE_MODELS_DEV_PATH` is set, else the catalog URL. Same
/// derivation `refresh` stores in the cache.
fn current_source_fingerprint() -> String {
    match std::env::var(ENV_MODELS_DEV_PATH) {
        Ok(path) if !path.trim().is_empty() => {
            base_url_fingerprint(&format!("file:{}", Path::new(path.trim()).display()))
        }
        _ => base_url_fingerprint(&resolve_catalog_url()),
    }
}

/// Best-effort background refresh: never panics, never blocks callers.
pub fn spawn_background_refresh() {
    if env_truthy(ENV_DISABLE_FETCH) && std::env::var(ENV_MODELS_DEV_PATH).is_err() {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async {
        match refresh(false).await {
            Ok(count) => {
                tracing::debug!(
                    target: "models_dev_live",
                    offering_count = count,
                    "Models.dev live catalog refreshed"
                );
            }
            Err(err) => {
                tracing::debug!(
                    target: "models_dev_live",
                    error = %err,
                    "Models.dev live catalog refresh skipped"
                );
            }
        }
    });
}

async fn refresh_from_path(path: &Path) -> Result<usize, ModelsDevRefreshError> {
    let body = match read_catalog_file(path).await {
        Ok(body) => body,
        Err(mapped) => {
            mark_failed(mapped.clone());
            return Err(mapped);
        }
    };
    let fetched_at = now_unix();
    let label = format!("file:{}", path.display());
    let fingerprint = base_url_fingerprint(&label);
    let count = publish_from_body(
        &body,
        fetched_at,
        &label,
        ModelsDevFreshness::Live,
        ModelsDevProvenance::File,
    )?;
    if let Some(cache) = cache_path() {
        save_cache_file(
            &cache,
            &PersistedModelsDevCache {
                schema_version: CACHE_SCHEMA_VERSION,
                fetched_at,
                source_fingerprint: fingerprint,
                source_label: label,
                body,
            },
        )
        .inspect_err(note_persistence_error)?;
    }
    Ok(count)
}

async fn fetch_catalog_body(url: &str) -> Result<String, ModelsDevRefreshError> {
    let client = codewhale_release::tls::reqwest_client_builder()
        .timeout(FETCH_TIMEOUT)
        .connect_timeout(Duration::from_secs(10))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|err| ModelsDevRefreshError::Network(err.to_string()))?;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| ModelsDevRefreshError::Network(err.to_string()))?;

    let status = response.status();
    if !status.is_success() {
        return Err(ModelsDevRefreshError::HttpStatus(status.as_u16()));
    }

    read_catalog_response(response, MAX_CATALOG_BYTES).await
}

fn oversized_catalog(limit: usize) -> ModelsDevRefreshError {
    ModelsDevRefreshError::InvalidResponse(format!("catalog exceeds {limit} bytes"))
}

/// Read a catalog response body without ever holding more than `limit` bytes.
async fn read_catalog_response(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<String, ModelsDevRefreshError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(oversized_catalog(limit));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| ModelsDevRefreshError::Network(err.to_string()))?
    {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(oversized_catalog(limit));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes)
        .map_err(|_| ModelsDevRefreshError::InvalidResponse("catalog is not UTF-8".into()))
}

/// Read an override catalog file, refusing one larger than the catalog limit.
async fn read_catalog_file(path: &Path) -> Result<String, ModelsDevRefreshError> {
    use tokio::io::AsyncReadExt as _;

    let file = tokio::fs::File::open(path)
        .await
        .map_err(|err| ModelsDevRefreshError::Io(err.to_string()))?;
    let mut bytes = Vec::new();
    file.take(MAX_CATALOG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|err| ModelsDevRefreshError::Io(err.to_string()))?;
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err(oversized_catalog(MAX_CATALOG_BYTES));
    }
    String::from_utf8(bytes)
        .map_err(|_| ModelsDevRefreshError::InvalidResponse("catalog is not UTF-8".into()))
}

fn publish_from_body(
    body: &str,
    fetched_at: u64,
    source_label: &str,
    freshness: ModelsDevFreshness,
    provenance: ModelsDevProvenance,
) -> Result<usize, ModelsDevRefreshError> {
    let catalog = ModelsDevCatalog::parse_json(body).map_err(|err| {
        let mapped = ModelsDevRefreshError::InvalidResponse(err.to_string());
        mark_failed(mapped.clone());
        mapped
    })?;
    // The source fingerprint scopes the *disk cache*, not the rows: a
    // models.dev row describes a model, not an endpoint (`ModelsDevLive`).
    let offerings = live_offerings_from_models_dev(&catalog, fetched_at);
    if offerings.is_empty() {
        let err = ModelsDevRefreshError::EmptyCatalog;
        mark_failed(err.clone());
        return Err(err);
    }
    let count = offerings.len();
    crate::provider_lake::set_live_snapshot(
        CatalogSnapshot { offerings },
        crate::provider_lake::LiveSource::ModelsDev,
    );
    set_status(ModelsDevStatus {
        freshness,
        provenance,
        offering_count: count,
        fetched_at: Some(fetched_at),
        source_label: source_label.to_string(),
        last_error: None,
    });
    Ok(count)
}

fn note_persistence_error(err: &ModelsDevRefreshError) {
    let mut next = status();
    next.last_error = Some(err.to_string());
    set_status(next);
}

fn mark_failed(err: ModelsDevRefreshError) {
    let mut next = status();
    // Keep prior offering_count / fetched_at so UI can still show the last
    // rows, but mark the last refresh outcome distinctly from TTL staleness.
    next.freshness = ModelsDevFreshness::Failed;
    next.last_error = Some(err.to_string());
    set_status(next);
}

/// Load the on-disk Models.dev cache.
///
/// Reads v2 (single-parse: one header line + raw body) and v1 (JSON envelope
/// with an escaped body) formats. Returns metadata and the *unescaped* body
/// without copying it in the v2 path.
fn load_cache_file(path: &Path) -> Option<PersistedModelsDevCache> {
    use std::io::Read as _;

    // Header line plus body; a larger file is not ours to trust or to read
    // whole, so the bundled rows stand instead.
    const MAX_CACHE_FILE_BYTES: u64 = MAX_CATALOG_BYTES as u64 + 64 * 1024;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_CACHE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_CACHE_FILE_BYTES {
        return None;
    }
    // v2: single-line JSON header terminated by a newline, then the verbatim
    // catalog body. One small parse, zero body copies.
    if bytes.first() == Some(&b'{') && bytes.contains(&b'\n') {
        let split = bytes.iter().position(|b| *b == b'\n')?;
        if let Ok(header) = serde_json::from_slice::<PersistedModelsDevCacheV2>(&bytes[..split])
            && header.schema_version == CACHE_SCHEMA_VERSION_V2
            && !bytes[split + 1..].is_empty()
        {
            let body = String::from_utf8(bytes[split + 1..].to_vec()).ok()?;
            return Some(PersistedModelsDevCache {
                schema_version: CACHE_SCHEMA_VERSION,
                fetched_at: header.fetched_at,
                source_fingerprint: header.source_fingerprint,
                source_label: header.source_label,
                body,
            });
        }
    }
    // v1 fallback: whole-file JSON envelope with the body escaped inside.
    let cache: PersistedModelsDevCache = serde_json::from_slice(&bytes).ok()?;
    if cache.schema_version != CACHE_SCHEMA_VERSION {
        return None;
    }
    if cache.body.trim().is_empty() {
        return None;
    }
    Some(cache)
}

fn save_cache_file(
    path: &Path,
    cache: &PersistedModelsDevCache,
) -> Result<(), ModelsDevRefreshError> {
    // Write the single-parse format so the next boot parses the catalog once.
    let header = PersistedModelsDevCacheV2 {
        schema_version: CACHE_SCHEMA_VERSION_V2,
        fetched_at: cache.fetched_at,
        source_fingerprint: cache.source_fingerprint.clone(),
        source_label: cache.source_label.clone(),
    };
    let mut header_line =
        serde_json::to_vec(&header).map_err(|err| ModelsDevRefreshError::Io(err.to_string()))?;
    header_line.push(b'\n');
    let mut payload = header_line;
    payload.extend_from_slice(cache.body.as_bytes());
    atomic_write(path, &payload).map_err(|err| ModelsDevRefreshError::Io(err.to_string()))
}

/// Compile helper exposed for unit tests: body → live offerings with normalized
/// provider ids.
#[cfg(test)]
pub(crate) fn offerings_from_json_for_test(
    body: &str,
) -> Result<Vec<codewhale_config::catalog::CatalogOffering>, String> {
    let catalog = ModelsDevCatalog::parse_json(body).map_err(|e| e.to_string())?;
    Ok(live_offerings_from_models_dev(&catalog, 1_700_000_000))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_lake::{
        all_catalog_models_for_provider, clear_live_snapshot, lock_live_snapshot,
    };
    use crate::test_support::{EnvVarGuard, lock_test_env};
    use codewhale_config::ProviderKind;
    use codewhale_config::catalog::CatalogSource;

    const FIXTURE: &str = r#"{
      "models": {},
      "providers": {
        "togetherai": {
          "id": "togetherai",
          "models": {
            "deepseek-ai/DeepSeek-V4-Pro": {
              "id": "deepseek-ai/DeepSeek-V4-Pro",
              "name": "DeepSeek V4 Pro",
              "modalities": { "input": ["text"], "output": ["text"] },
              "limit": { "context": 128000, "output": 8192 }
            }
          }
        },
        "moonshotai": {
          "id": "moonshotai",
          "models": {
            "kimi-k2.5": {
              "id": "kimi-k2.5",
              "name": "Kimi K2.5",
              "modalities": { "input": ["text"], "output": ["text"] },
              "limit": { "context": 256000, "output": 8192 }
            }
          }
        },
        "unknown-gateway": {
          "id": "unknown-gateway",
          "models": {
            "mystery-1": {
              "id": "mystery-1",
              "modalities": { "input": ["text"], "output": ["text"] }
            }
          }
        }
      }
    }"#;

    /// An unguarded test must not resolve the developer's catalog cache.
    ///
    /// `resolve_state_dir` lives in `codewhale-config`, which is a plain
    /// dependency here and cannot see this crate's isolated test root, so this
    /// path had no equivalent of the settings confinement (#5359). A picker
    /// test then rendered against whatever models the developer last fetched.
    #[test]
    fn unguarded_cache_path_stays_inside_the_isolated_test_root() {
        let path = cache_path().expect("cache path");
        let isolated = crate::test_support::isolated_test_state_root();
        assert!(
            path.starts_with(isolated),
            "catalog cache escaped the isolated test root: {}",
            path.display()
        );
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some(CACHE_FILE));
    }

    /// A test that does seal the environment still resolves it, so the
    /// confinement above cannot silently break the guarded callers.
    #[test]
    fn guarded_cache_path_follows_the_sealed_home() {
        let _lock = lock_test_env();
        let home = tempfile::tempdir().expect("tempdir");
        let _guard = EnvVarGuard::set("CODEWHALE_HOME", home.path());

        let path = cache_path().expect("cache path");

        assert!(
            path.starts_with(home.path()),
            "sealed CODEWHALE_HOME was ignored: {}",
            path.display()
        );
    }

    #[test]
    fn resolve_catalog_url_defaults_and_overrides() {
        let _lock = lock_test_env();
        let _url = EnvVarGuard::remove(ENV_MODELS_DEV_URL);
        assert_eq!(resolve_catalog_url(), MODELS_DEV_CATALOG_URL);

        let _url = EnvVarGuard::set(ENV_MODELS_DEV_URL, "https://example.test");
        assert_eq!(resolve_catalog_url(), "https://example.test/catalog.json");

        let _url = EnvVarGuard::set(ENV_MODELS_DEV_URL, "https://example.test/api.json");
        assert_eq!(resolve_catalog_url(), "https://example.test/api.json");
    }

    #[test]
    fn live_offerings_normalize_models_dev_provider_ids() {
        let rows = offerings_from_json_for_test(FIXTURE).expect("fixture");
        let providers: Vec<_> = rows.iter().map(|r| r.provider.as_str()).collect();
        assert!(providers.contains(&"together"));
        assert!(providers.contains(&"moonshot"));
        assert!(providers.contains(&"unknown-gateway"));
        assert!(!providers.contains(&"togetherai"));
        assert!(!providers.contains(&"moonshotai"));
        // Layer 10, not layer 20: a refresh of a public catalog is external
        // enrichment about a model, not a provider's answer about an endpoint,
        // so it stays correctable by the signed layer above it.
        assert!(
            rows.iter()
                .all(|r| matches!(r.source, CatalogSource::ModelsDevLive { .. }))
        );
    }

    #[test]
    fn publish_from_path_updates_provider_lake() {
        let _lock = lock_test_env();
        let _live = lock_live_snapshot();
        clear_live_snapshot();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("catalog.json");
        std::fs::write(&path, FIXTURE).expect("write fixture");

        let _home = EnvVarGuard::set("CODEWHALE_HOME", dir.path().join("home"));
        let _disable = EnvVarGuard::set(ENV_DISABLE_FETCH, "1");
        let _path = EnvVarGuard::set(ENV_MODELS_DEV_PATH, &path);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let count = rt.block_on(refresh(true)).expect("refresh from path");
        assert!(count >= 2);

        let together = all_catalog_models_for_provider(ProviderKind::Together);
        assert!(
            together.iter().any(|m| m == "deepseek-ai/DeepSeek-V4-Pro"),
            "Together lake missing live Models.dev row: {together:?}"
        );
        let moonshot = all_catalog_models_for_provider(ProviderKind::Moonshot);
        assert!(
            moonshot.iter().any(|m| m == "kimi-k2.5"),
            "Moonshot lake missing live Models.dev row: {moonshot:?}"
        );

        let st = status();
        assert_eq!(st.freshness, ModelsDevFreshness::Live);
        assert!(st.last_error.is_none());
        assert!(st.offering_count >= 2);

        // Cache file should exist and be secret-free.
        let cache = cache_path().expect("cache path");
        assert!(cache.exists());
        let on_disk = std::fs::read_to_string(&cache).expect("read cache");
        let lowered = on_disk.to_lowercase();
        for needle in ["api_key", "authorization", "bearer", "password"] {
            assert!(
                !lowered.contains(&format!("\"{needle}\"")),
                "cache must not persist `{needle}`"
            );
        }

        clear_live_snapshot();
    }

    #[test]
    fn bundled_staleness_flips_only_bundled_reports() {
        let bundled = ModelsDevStatus::default();
        assert_eq!(bundled.freshness, ModelsDevFreshness::Bundled);
        assert_eq!(
            honor_bundled_staleness(bundled.clone(), true).freshness,
            ModelsDevFreshness::Stale
        );
        assert_eq!(
            honor_bundled_staleness(bundled, false).freshness,
            ModelsDevFreshness::Bundled
        );
        let live = ModelsDevStatus {
            freshness: ModelsDevFreshness::Live,
            ..ModelsDevStatus::default()
        };
        assert_eq!(
            honor_bundled_staleness(live, true).freshness,
            ModelsDevFreshness::Live
        );
    }

    #[test]
    fn invalid_json_keeps_bundled_and_marks_failed() {
        let _lock = lock_test_env();
        let _live = lock_live_snapshot();
        clear_live_snapshot();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "{not-json").expect("write");

        let _home = EnvVarGuard::set("CODEWHALE_HOME", dir.path().join("home"));
        let _path = EnvVarGuard::set(ENV_MODELS_DEV_PATH, &path);

        let before = all_catalog_models_for_provider(ProviderKind::Together);
        assert!(!before.is_empty(), "bundled Together rows required");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let err = rt.block_on(refresh(true)).expect_err("bad json");
        assert!(matches!(err, ModelsDevRefreshError::InvalidResponse(_)));

        let after = all_catalog_models_for_provider(ProviderKind::Together);
        assert_eq!(after, before, "bundled rows must survive parse failure");
        let st = status();
        assert_eq!(st.freshness, ModelsDevFreshness::Failed);
        assert!(st.last_error.is_some());
        clear_live_snapshot();
    }

    #[test]
    fn stale_disk_cache_still_publishes() {
        let _lock = lock_test_env();
        let _live = lock_live_snapshot();
        clear_live_snapshot();
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", &home);

        let cache_dir = home.join("catalog");
        std::fs::create_dir_all(&cache_dir).expect("mkdir");
        let cache = cache_dir.join(CACHE_FILE);
        let stale = PersistedModelsDevCache {
            schema_version: CACHE_SCHEMA_VERSION,
            fetched_at: 1, // far in the past → stale
            source_fingerprint: current_source_fingerprint(),
            source_label: "https://models.dev/catalog.json".into(),
            body: FIXTURE.into(),
        };
        save_cache_file(&cache, &stale).expect("save");

        maybe_load_persisted_cache();
        let st = status();
        assert_eq!(st.freshness, ModelsDevFreshness::Stale);
        assert!(st.offering_count >= 2);
        let together = all_catalog_models_for_provider(ProviderKind::Together);
        assert!(together.iter().any(|m| m == "deepseek-ai/DeepSeek-V4-Pro"));
        clear_live_snapshot();
    }

    #[test]
    fn a_future_or_foreign_cache_is_never_live() {
        let now = 2_000_000_000;
        assert!(within_ttl(now - 60, now));
        assert!(within_ttl(now + 30, now), "small skew is tolerated");
        assert!(
            !within_ttl(now + 3 * 24 * 60 * 60, now),
            "a future timestamp is not age zero"
        );
        assert!(!within_ttl(now - DEFAULT_MODELS_DEV_TTL_SECS - 1, now));

        let _lock = lock_test_env();
        let _live = lock_live_snapshot();
        clear_live_snapshot();
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", &home);
        let _path = EnvVarGuard::remove(ENV_MODELS_DEV_PATH);
        let _url = EnvVarGuard::remove(ENV_MODELS_DEV_URL);
        let cache_dir = home.join("catalog");
        std::fs::create_dir_all(&cache_dir).expect("mkdir");
        let cache = cache_dir.join(CACHE_FILE);

        // Fresh by the clock, but fetched from another source.
        let foreign = PersistedModelsDevCache {
            schema_version: CACHE_SCHEMA_VERSION,
            fetched_at: now_unix(),
            source_fingerprint: base_url_fingerprint("https://mirror.example/catalog.json"),
            source_label: "https://mirror.example/catalog.json".into(),
            body: FIXTURE.into(),
        };
        save_cache_file(&cache, &foreign).expect("save");
        maybe_load_persisted_cache();
        assert_eq!(status().freshness, ModelsDevFreshness::Failed);

        // From the current source, but stamped far in the future.
        let future = PersistedModelsDevCache {
            fetched_at: now_unix() + 30 * 24 * 60 * 60,
            source_fingerprint: base_url_fingerprint(MODELS_DEV_CATALOG_URL),
            source_label: MODELS_DEV_CATALOG_URL.into(),
            ..foreign.clone()
        };
        save_cache_file(&cache, &future).expect("save");
        maybe_load_persisted_cache();
        assert_eq!(status().freshness, ModelsDevFreshness::Stale);

        // The same cache stamped now is live.
        let current = PersistedModelsDevCache {
            fetched_at: now_unix(),
            ..future
        };
        save_cache_file(&cache, &current).expect("save");
        maybe_load_persisted_cache();
        assert_eq!(status().freshness, ModelsDevFreshness::Live);
        clear_live_snapshot();
    }

    #[test]
    fn an_oversized_override_catalog_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("huge.json");
        let file = std::fs::File::create(&path).expect("create");
        file.set_len(MAX_CATALOG_BYTES as u64 + 1)
            .expect("sparse size");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let error = rt
            .block_on(read_catalog_file(&path))
            .expect_err("oversized catalog must be refused");
        assert!(error.to_string().contains("exceeds"), "{error}");
    }

    #[test]
    fn network_failure_keeps_prior_rows_and_marks_failed() {
        let _lock = lock_test_env();
        let _live = lock_live_snapshot();
        clear_live_snapshot();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("catalog.json");
        std::fs::write(&path, FIXTURE).expect("write");

        let _home = EnvVarGuard::set("CODEWHALE_HOME", dir.path().join("home"));
        let _path = EnvVarGuard::set(ENV_MODELS_DEV_PATH, &path);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let count = rt.block_on(refresh(true)).expect("seed from path");
        assert!(count >= 2);

        // Point at a dead URL and force network (clear path override).
        let _path = EnvVarGuard::remove(ENV_MODELS_DEV_PATH);
        let _disable = EnvVarGuard::remove(ENV_DISABLE_FETCH);
        let _url = EnvVarGuard::set(ENV_MODELS_DEV_URL, "http://127.0.0.1:1");

        let err = rt.block_on(refresh(true)).expect_err("dead URL");
        assert!(matches!(err, ModelsDevRefreshError::Network(_)));

        let together = all_catalog_models_for_provider(ProviderKind::Together);
        assert!(
            together.iter().any(|m| m == "deepseek-ai/DeepSeek-V4-Pro"),
            "prior live rows must survive network failure"
        );
        let st = status();
        assert_eq!(st.freshness, ModelsDevFreshness::Failed);
        assert!(st.last_error.is_some());
        assert!(
            st.offering_count >= 2,
            "status should retain prior live row count after failure"
        );
        clear_live_snapshot();
    }
}
