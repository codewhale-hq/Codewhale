//! Durable, secret-free per-provider `/models` catalog cache.
//!
//! This is the persistence owner for [`codewhale_config::catalog::ProviderCatalogCache`].
//! It replaces the process-only client refresh and provider_lake CLI cache writers: successful
//! refreshes replace one exact `(provider kind, identity, base URL fingerprint)`
//! partition, failures retain that partition's prior rows, and startup loads
//! only the active route's exact partition. Credentials authorize the fetch in
//! `client`; they never enter this module or its disk envelope. Baseten and
//! Codewhale account rosters are memory-only and cleared before each refresh;
//! a named custom route at either official endpoint follows the same rule.
//!
//! The former unscoped OpenRouter cache reader is retired; installed old files
//! are preserved and never imported as provider authority.
//! `catalog/provider-catalogs.json` is the sole writer-owned live roster store.
//! Older per-endpoint `provider-*.json` files also lack the built-in/custom
//! kind boundary and account-roster exclusion, so they are left untouched and
//! replaced only by a newly authenticated refresh into this store.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, RwLock};

use anyhow::{Context, Result};
use codewhale_config::catalog::now_unix;
use codewhale_config::catalog::{
    CatalogRefreshError, CatalogSnapshot, CatalogStatus, ProviderCatalogCache,
    ProviderCatalogDelta, base_url_fingerprint,
};
use codewhale_config::persistence::atomic_write_json;
use codewhale_config::pricing::{Currency, OfferingPricing, PricingProvenance};
use serde::{Deserialize, Serialize};

use crate::config::{Config, ProviderKind};

const CACHE_SCHEMA_VERSION: u32 = 2;
const CACHE_FILE: &str = "provider-catalogs.json";
const MAX_CACHE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_CACHE_SCOPES: usize = 64;
const MAX_CACHE_ROWS: usize = 50_000;

#[derive(Debug, Clone, Copy)]
struct CachePersistenceLimits {
    max_bytes: u64,
    max_scopes: usize,
    max_rows: usize,
}

const CACHE_PERSISTENCE_LIMITS: CachePersistenceLimits = CachePersistenceLimits {
    max_bytes: MAX_CACHE_BYTES,
    max_scopes: MAX_CACHE_SCOPES,
    max_rows: MAX_CACHE_ROWS,
};

/// Provider-owned catalogs are refreshed daily. Past-TTL rows remain visible
/// with an explicit stale receipt until a successful replacement arrives.
pub const DEFAULT_PROVIDER_CATALOG_TTL_SECS: u64 = 24 * 60 * 60;

static DISK_LOADED: AtomicBool = AtomicBool::new(false);

static CACHE: LazyLock<RwLock<ProviderCatalogCache>> =
    LazyLock::new(|| RwLock::new(ProviderCatalogCache::new()));
static REFRESH_GENERATIONS: LazyLock<RwLock<BTreeMap<String, u64>>> =
    LazyLock::new(|| RwLock::new(BTreeMap::new()));

#[derive(Debug, Clone)]
pub struct ProviderCatalogRefreshTicket {
    provider: String,
    provider_kind: ProviderKind,
    fingerprint: Option<String>,
    generation: u64,
}

/// Immutable, secret-free catalog rate evidence captured at dispatch.
///
/// Rates are stored as canonical decimal strings rather than `f64` so route
/// receipts retain exact equality and stable JSON. `catalog_revision` binds
/// every identity, scope, timestamp, currency, provenance, and rate field; it
/// therefore changes even when two refreshes land in the same Unix second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderLivePricingQuote {
    pub(crate) provider: ProviderKind,
    pub(crate) provider_identity: String,
    pub(crate) wire_model: String,
    pub(crate) endpoint_fingerprint: String,
    pub(crate) catalog_fetched_at: u64,
    pub(crate) catalog_revision: String,
    pub(crate) currency: Currency,
    pub(crate) provenance: PricingProvenance,
    pub(crate) cloud_facts: Option<CloudFactsPricingSource>,
    pub(crate) input_per_million: Option<String>,
    pub(crate) output_per_million: Option<String>,
    pub(crate) cache_read_per_million: Option<String>,
    pub(crate) cache_write_per_million: Option<String>,
}

/// Signed source identity and validity, bound into a frozen rate receipt.
/// The base URL is admitted only through the canonical official-endpoint
/// contract; custom URLs or credential-bearing URLs cannot enter this field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CloudFactsPricingSource {
    pub(crate) facts_version: u64,
    pub(crate) key_id: String,
    pub(crate) valid_until: Option<u64>,
    pub(crate) base_url: String,
}

#[derive(Serialize, Deserialize)]
struct ProviderLivePricingQuoteWire {
    provider: String,
    provider_identity: String,
    wire_model: String,
    endpoint_fingerprint: String,
    catalog_fetched_at: u64,
    catalog_revision: String,
    currency: Currency,
    provenance: PricingProvenance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cloud_facts: Option<CloudFactsPricingSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input_per_million: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_per_million: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_read_per_million: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_write_per_million: Option<String>,
}

impl TryFrom<&ProviderLivePricingQuote> for ProviderLivePricingQuoteWire {
    type Error = &'static str;
    fn try_from(quote: &ProviderLivePricingQuote) -> Result<Self, Self::Error> {
        let provider = codewhale_config::descriptors::tui_wire_tag_for_route(
            quote.provider,
            &quote.provider_identity,
        )
        .ok_or("contradictory pricing provider identity")?;
        Ok(Self {
            provider: provider.into(),
            provider_identity: quote.provider_identity.clone(),
            wire_model: quote.wire_model.clone(),
            endpoint_fingerprint: quote.endpoint_fingerprint.clone(),
            catalog_fetched_at: quote.catalog_fetched_at,
            catalog_revision: quote.catalog_revision.clone(),
            currency: quote.currency.clone(),
            provenance: quote.provenance.clone(),
            cloud_facts: quote.cloud_facts.clone(),
            input_per_million: quote.input_per_million.clone(),
            output_per_million: quote.output_per_million.clone(),
            cache_read_per_million: quote.cache_read_per_million.clone(),
            cache_write_per_million: quote.cache_write_per_million.clone(),
        })
    }
}

impl Serialize for ProviderLivePricingQuote {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if !self.is_structurally_valid() {
            return serializer.serialize_none();
        }
        ProviderLivePricingQuoteWire::try_from(self)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ProviderLivePricingQuote {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ProviderLivePricingQuoteWire::deserialize(deserializer)?;
        let provider = codewhale_config::descriptors::kind_from_tui_wire_tag(
            &wire.provider,
            &wire.provider_identity,
        )
        .ok_or_else(|| serde::de::Error::custom("contradictory pricing provider identity"))?;
        let quote = Self {
            provider,
            provider_identity: wire.provider_identity,
            wire_model: wire.wire_model,
            endpoint_fingerprint: wire.endpoint_fingerprint,
            catalog_fetched_at: wire.catalog_fetched_at,
            catalog_revision: wire.catalog_revision,
            currency: wire.currency,
            provenance: wire.provenance,
            cloud_facts: wire.cloud_facts,
            input_per_million: wire.input_per_million,
            output_per_million: wire.output_per_million,
            cache_read_per_million: wire.cache_read_per_million,
            cache_write_per_million: wire.cache_write_per_million,
        };
        quote
            .is_structurally_valid()
            .then_some(quote)
            .ok_or_else(|| serde::de::Error::custom("invalid provider-live pricing quote"))
    }
}

pub(crate) fn deserialize_optional_provider_live_pricing<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<ProviderLivePricingQuote>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

impl ProviderLivePricingQuote {
    /// Whether the quote names at least one rate. A `[[custom_models]]` row
    /// declared only to add a model to a roster freezes a rate-less quote.
    #[must_use]
    pub(crate) fn carries_rates(&self) -> bool {
        [
            &self.input_per_million,
            &self.output_per_million,
            &self.cache_read_per_million,
            &self.cache_write_per_million,
        ]
        .into_iter()
        .any(Option::is_some)
    }

    fn is_structurally_valid(&self) -> bool {
        self.pricing_for_route(
            self.provider,
            &self.provider_identity,
            &self.wire_model,
            &self.endpoint_fingerprint,
            self.catalog_fetched_at,
        )
        .is_some()
    }
    fn canonical_rate(rate: Option<f64>) -> Option<String> {
        rate.map(|rate| rate.to_string())
    }

    fn revision_for(
        provider: ProviderKind,
        provider_identity: &str,
        wire_model: &str,
        endpoint_fingerprint: &str,
        catalog_fetched_at: u64,
        currency: &Currency,
        provenance: &PricingProvenance,
        input_per_million: &Option<String>,
        output_per_million: &Option<String>,
        cache_read_per_million: &Option<String>,
        cache_write_per_million: &Option<String>,
        cloud_facts: Option<&CloudFactsPricingSource>,
    ) -> Option<String> {
        let payload = serde_json::to_vec(&(
            "codewhale-provider-live-pricing-quote-v1",
            codewhale_config::descriptors::tui_wire_tag_for_route(provider, provider_identity)?,
            provider_identity,
            wire_model,
            endpoint_fingerprint,
            catalog_fetched_at,
            currency,
            provenance,
            input_per_million,
            output_per_million,
            cache_read_per_million,
            cache_write_per_million,
        ))
        .ok()?;
        // Preserve the existing provider-live wire revision. The additional
        // cloud source is independently domain-separated and hashes the whole
        // original binding as well as the signed version/key/expiry.
        let payload = match cloud_facts {
            Some(source) => {
                serde_json::to_vec(&("codewhale-cloud-facts-pricing-quote-v1", payload, source))
                    .ok()?
            }
            None => payload,
        };
        Some(format!("sha256:{}", crate::hashing::sha256_hex(payload)))
    }

    fn from_pricing(
        provider: ProviderKind,
        provider_identity: &str,
        wire_model: &str,
        endpoint_fingerprint: &str,
        catalog_fetched_at: u64,
        pricing: &OfferingPricing,
    ) -> Option<Self> {
        let provider_identity = provider_identity.trim();
        let wire_model = wire_model.trim();
        if crate::cost_status::sanitize_persisted_route_label(provider_identity)
            != provider_identity
            || crate::cost_status::sanitize_persisted_route_label(wire_model) != wire_model
        {
            return None;
        }
        let input_per_million = Self::canonical_rate(pricing.input_per_million);
        let output_per_million = Self::canonical_rate(pricing.output_per_million);
        let cache_read_per_million = Self::canonical_rate(pricing.cache_read_per_million);
        let cache_write_per_million = Self::canonical_rate(pricing.cache_write_per_million);
        let catalog_revision = Self::revision_for(
            provider,
            provider_identity,
            wire_model,
            endpoint_fingerprint,
            catalog_fetched_at,
            &pricing.currency,
            &pricing.provenance,
            &input_per_million,
            &output_per_million,
            &cache_read_per_million,
            &cache_write_per_million,
            None,
        )?;
        Some(Self {
            provider,
            provider_identity: provider_identity.to_string(),
            wire_model: wire_model.to_string(),
            endpoint_fingerprint: endpoint_fingerprint.to_string(),
            catalog_fetched_at,
            catalog_revision,
            currency: pricing.currency.clone(),
            provenance: pricing.provenance.clone(),
            cloud_facts: None,
            input_per_million,
            output_per_million,
            cache_read_per_million,
            cache_write_per_million,
        })
    }

    fn parse_rate(rate: &Option<String>) -> Option<Option<f64>> {
        let Some(rate) = rate else {
            return Some(None);
        };
        let parsed = rate.parse::<f64>().ok()?;
        (parsed.is_finite() && parsed >= 0.0 && parsed.to_string() == *rate).then_some(Some(parsed))
    }

    /// Rehydrate the frozen row only when every receipt binding is intact.
    /// This is deliberately cache-free: a refresh after dispatch cannot alter
    /// an earlier turn, while malformed or legacy receipts fail closed.
    pub(crate) fn pricing_for_route(
        &self,
        provider: ProviderKind,
        provider_identity: &str,
        wire_model: &str,
        endpoint_fingerprint: &str,
        dispatched_at_unix: u64,
    ) -> Option<OfferingPricing> {
        let provider_identity = provider_identity.trim();
        let wire_model = wire_model.trim();
        if self.provider != provider
            || crate::cost_status::sanitize_persisted_route_label(&self.provider_identity)
                != self.provider_identity
            || crate::cost_status::sanitize_persisted_route_label(&self.wire_model)
                != self.wire_model
            || self.endpoint_fingerprint.len() != 64
            || !self
                .endpoint_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || self.provider_identity != provider_identity
            || self.wire_model != wire_model
            || self.endpoint_fingerprint != endpoint_fingerprint
            || self.catalog_fetched_at > dispatched_at_unix
            || self.currency != Currency::Usd
        {
            return None;
        }
        match (&self.provenance, &self.cloud_facts) {
            (PricingProvenance::UserOverride, None) => {}
            (PricingProvenance::ProviderLive, None)
                if dispatched_at_unix.saturating_sub(self.catalog_fetched_at)
                    < DEFAULT_PROVIDER_CATALOG_TTL_SECS
                    && reviewed_provider_live_scope(
                        provider,
                        provider_identity,
                        endpoint_fingerprint,
                    ) => {}
            (PricingProvenance::CloudFacts, Some(source))
                if source.facts_version > 0
                    && !source.key_id.is_empty()
                    && source.key_id.len() <= 128
                    && source
                        .key_id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                    && source
                        .valid_until
                        .is_none_or(|expires| dispatched_at_unix <= expires)
                    && cloud_pricing_scope(provider, provider_identity, &source.base_url)
                    && base_url_fingerprint(&source.base_url) == endpoint_fingerprint => {}
            _ => return None,
        }
        let input_per_million = Self::parse_rate(&self.input_per_million)?;
        let output_per_million = Self::parse_rate(&self.output_per_million)?;
        let cache_read_per_million = Self::parse_rate(&self.cache_read_per_million)?;
        let cache_write_per_million = Self::parse_rate(&self.cache_write_per_million)?;
        let cost = codewhale_config::models_dev::ModelsDevCost {
            input: input_per_million,
            output: output_per_million,
            cache_read: cache_read_per_million,
            cache_write: cache_write_per_million,
            ..Default::default()
        };
        if !codewhale_config::pricing::catalog_cost_is_valid(&cost) {
            return None;
        }
        // A reviewed per-token route needs both ordinary request classes. Cache
        // classes remain optional and fail closed later if a turn used them.
        if self.provenance == PricingProvenance::ProviderLive
            && (cost.input.is_none() || cost.output.is_none())
        {
            return None;
        }
        if cost.input.is_none()
            && cost.output.is_none()
            && cost.cache_read.is_none()
            && cost.cache_write.is_none()
            && self.provenance != PricingProvenance::UserOverride
        {
            return None;
        }
        let expected_revision = Self::revision_for(
            self.provider,
            &self.provider_identity,
            &self.wire_model,
            &self.endpoint_fingerprint,
            self.catalog_fetched_at,
            &self.currency,
            &self.provenance,
            &self.input_per_million,
            &self.output_per_million,
            &self.cache_read_per_million,
            &self.cache_write_per_million,
            self.cloud_facts.as_ref(),
        )?;
        if self.catalog_revision != expected_revision {
            return None;
        }
        Some(OfferingPricing {
            provider: self.provider_identity.clone(),
            wire_model_id: self.wire_model.clone(),
            canonical_model: None,
            currency: self.currency.clone(),
            input_per_million: cost.input,
            output_per_million: cost.output,
            cache_read_per_million: cost.cache_read,
            cache_write_per_million: cost.cache_write,
            provenance: self.provenance.clone(),
            effective_at: Some(self.catalog_fetched_at),
            endpoint_fingerprint: Some(self.endpoint_fingerprint.clone()),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedProviderCatalogs {
    schema_version: u32,
    cache: ProviderCatalogCache,
}

#[derive(Serialize)]
struct PersistedProviderCatalogsRef<'a> {
    schema_version: u32,
    cache: &'a ProviderCatalogCache,
}

/// Resolve the cache under Codewhale's catalog state directory.
///
/// Unguarded tests are confined to the TUI test root, matching the Models.dev
/// cache contract, so they never inspect a developer's real provider catalog.
#[must_use]
pub fn cache_path() -> Option<PathBuf> {
    #[cfg(test)]
    {
        if !crate::test_support::guarded_environment_provides_state_paths() {
            return Some(
                crate::test_support::unsealed_test_state_root()
                    .join("catalog")
                    .join(CACHE_FILE),
            );
        }
    }
    codewhale_config::resolve_state_dir("catalog")
        .ok()
        .map(|dir| dir.join(CACHE_FILE))
}

fn canonical_provider_scope(provider: &str) -> String {
    // Despite the historical name, this is the exact configured ownership
    // scope. Never collapse a custom table that happens to resemble a built-in
    // or setup-template alias.
    crate::provider_lake::catalog_partition_key(provider)
}

#[cfg(test)]
fn inferred_provider_kind(identity: &str) -> ProviderKind {
    // No recognized built-in spelling resolves to a compatible-template id,
    // so the parse fallback below already answers Custom for every named
    // custom table (#6289).
    ProviderKind::parse(identity).unwrap_or(ProviderKind::Custom)
}

fn storage_provider(kind: ProviderKind, identity: &str) -> String {
    format!("{}:{}", kind.as_str(), identity.trim())
}

/// Providers whose model list is owned by their own `/v1/models` roster
/// rather than the cross-provider Models.dev snapshot: the named live
/// gateways, plus custom hosts whose private roster no snapshot can serve
/// (#6289 widened). The active-provider refresh and the picker's freshness
/// receipt both gate on this one predicate, so they cannot drift apart.
#[cfg(test)]
#[test]
fn orcarouter_and_existing_custom_routes_own_live_catalogs() {
    assert!(provider_owns_live_catalog(ProviderKind::Orcarouter));
    assert!(provider_owns_live_catalog(ProviderKind::Custom));
    assert!(provider_owns_live_catalog(ProviderKind::Openrouter));
    assert!(provider_owns_live_catalog(ProviderKind::Ollama));
    assert!(!provider_owns_live_catalog(ProviderKind::Openai));
}

pub(crate) fn provider_owns_live_catalog(provider: ProviderKind) -> bool {
    matches!(
        provider,
        ProviderKind::Openrouter
            | ProviderKind::Orcarouter
            | ProviderKind::Telecomjs
            | ProviderKind::Edenai
            | ProviderKind::Zenmux
            | ProviderKind::Concentrate
            | ProviderKind::Codewhale
            | ProviderKind::Ollama
    ) || provider == ProviderKind::Custom
}

/// Whether a catalog scope holds an account-scoped roster that must never be
/// shared across credentials (#6289).
///
/// Baseten's `/models` answers per workspace, so its rows are fenced by
/// endpoint fingerprint — never by table name. The Codewhale API's own rows
/// are fenced the same way.
fn is_account_scoped_scope(provider: &str, fingerprint: &str) -> bool {
    crate::plugins::providers::is_account_catalog_scope(provider, fingerprint)
        || provider.starts_with("codewhale:")
        || fingerprint == base_url_fingerprint(codewhale_config::catalog::BASETEN_BASE_URL)
        || fingerprint
            == base_url_fingerprint(ProviderKind::Codewhale.provider().default_base_url())
}

fn cache_lock_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| CACHE_FILE.into());
    name.push(".lock");
    path.with_file_name(name)
}

fn open_cache_lock(path: &Path) -> Result<fs::File> {
    let parent = path
        .parent()
        .context("provider catalog lock path has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create provider catalog directory {}", parent.display()))?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options
        .open(path)
        .with_context(|| format!("open provider catalog lock {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("inspect provider catalog lock {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "provider catalog lock {} must be a regular file",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        anyhow::ensure!(
            metadata.nlink() == 1,
            "provider catalog lock {} must not be hard linked",
            path.display()
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        anyhow::ensure!(
            metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "provider catalog lock {} must not be a reparse point",
            path.display()
        );
    }
    Ok(file)
}

fn load_from_disk_unlocked_with_limit(path: &Path, max_bytes: u64) -> Option<ProviderCatalogCache> {
    let mut options = OpenOptions::new();
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
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() != 1 {
            return None;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & 0x0000_0400 != 0 {
            return None;
        }
    }
    if metadata.len() > max_bytes {
        tracing::debug!(
            target: "provider_catalog",
            path = %path.display(),
            max_bytes,
            "provider catalog cache exceeds read limit"
        );
        return None;
    }
    // Re-check through `take`: the file can grow after metadata is sampled.
    let mut body = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut body)
        .ok()?;
    if body.len() as u64 > max_bytes {
        return None;
    }
    let persisted: PersistedProviderCatalogs = serde_json::from_slice(&body).ok()?;
    if persisted.schema_version != CACHE_SCHEMA_VERSION {
        return None;
    }
    let mut cache = persisted.cache;
    if cache.entries.len() > MAX_CACHE_SCOPES || cached_row_count(&cache) > MAX_CACHE_ROWS {
        return None;
    }
    if !cache.entries.iter().all(|(key, entry)| {
        let Some((kind, identity)) = entry.provider.split_once(':') else { return false; };
        ProviderKind::parse(kind).is_some_and(|parsed| parsed.as_str() == kind)
            && !identity.is_empty()
            && key == &ProviderCatalogCache::cache_key(&entry.provider, &entry.base_url_fingerprint)
            && entry.offerings.iter().all(|row| {
                row.provider == identity
                    && crate::provider_lake::valid_catalog_model_id(&row.wire_model_id)
                    && provider_cost_source_allowed(row)
                    && matches!(&row.source, codewhale_config::catalog::CatalogSource::Live {
                        base_url_fingerprint, fetched_at
                    } if base_url_fingerprint == &entry.base_url_fingerprint && *fetched_at == entry.fetched_at)
            })
    }) { return None; }
    // Older builds could durably cache account-scoped rosters. Scrub
    // those entries on every load so upgrading cannot attach one workspace's
    // catalog to a different credential.
    cache
        .entries
        .retain(|_, entry| !is_account_scoped_scope(&entry.provider, &entry.base_url_fingerprint));
    Some(cache)
}

fn provider_cost_source_allowed(row: &codewhale_config::catalog::CatalogOffering) -> bool {
    use codewhale_config::catalog::CatalogSource;
    matches!(
        row.cost_source,
        None | Some(
            CatalogSource::Bundled
                | CatalogSource::CodewhaleBundled { .. }
                | CatalogSource::ModelsDevLive { .. }
        )
    )
}

fn load_from_disk_unlocked(path: &Path) -> Option<ProviderCatalogCache> {
    load_from_disk_unlocked_with_limit(path, MAX_CACHE_BYTES)
}

fn load_from_disk() -> Option<ProviderCatalogCache> {
    let path = cache_path()?;
    if !path.is_file() {
        return None;
    }
    let lock_file = open_cache_lock(&cache_lock_path(&path)).ok()?;
    let lock = fd_lock::RwLock::new(lock_file);
    let _guard = lock.read().ok()?;
    load_from_disk_unlocked(&path)
}

fn ensure_cache_loaded() -> Result<()> {
    if DISK_LOADED.load(Ordering::Acquire) {
        return Ok(());
    }
    let mut cache = CACHE
        .write()
        .map_err(|_| anyhow::anyhow!("catalog cache unavailable"))?;
    if DISK_LOADED.load(Ordering::Acquire) {
        return Ok(());
    }
    if let Some(path) = cache_path() {
        match fs::symlink_metadata(&path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
            Ok(_) => {
                let loaded = load_from_disk().context("invalid provider catalog cache")?;
                for (key, entry) in loaded.entries {
                    if cache
                        .entries
                        .get(&key)
                        .is_none_or(|local| entry.fetched_at >= local.fetched_at)
                    {
                        cache.entries.insert(key, entry);
                    }
                }
            }
        }
    }
    DISK_LOADED.store(true, Ordering::Release);
    Ok(())
}

/// Read one exact cached route without creating a client or fetching credentials.
pub(crate) fn cached_entry_for_route(
    kind: ProviderKind,
    identity: &str,
    base_url: &str,
) -> Result<Option<codewhale_config::catalog::CachedProviderCatalog>> {
    ensure_cache_loaded()?;
    let cache = CACHE
        .read()
        .map_err(|_| anyhow::anyhow!("catalog cache unavailable"))?;
    Ok(cache
        .get(
            &storage_provider(kind, identity),
            &base_url_fingerprint(base_url),
        )
        .cloned())
}

/// Whether a saved `(provider, model)` pin is absent from that exact route's
/// FRESH live roster (#6035). `None` when no fresh roster exists: a stale,
/// failed, or absent roster cannot prove drift, and bundled catalog rows say
/// nothing about what the account serves today. Absence is a warning, never a
/// reason to rewrite the pin: the id may still answer (soft deprecation) and
/// other hosts may serve it on their own routes.
pub(crate) fn pin_missing_from_fresh_roster(
    config: &Config,
    provider: &str,
    model: &str,
) -> Option<bool> {
    let captured = config.resolve_provider_pin_identity(provider).ok()?;
    let kind = captured.provider;
    let identity = captured.key.as_str();
    let base_url = config.base_url_for_route(&captured);
    // `status_for_route` reads memory only. A fresh process (doctor, a
    // just-started TUI) must see the roster an earlier process persisted.
    ensure_cache_loaded().ok()?;
    if status_for_route(kind, identity, &base_url) != CatalogStatus::Fresh {
        return None;
    }
    let listed = cached_entry_for_route(kind, identity, &base_url)
        .ok()
        .flatten()
        .is_some_and(|entry| {
            entry.offerings.iter().any(|offering| {
                offering.wire_model_id == model
                    || offering.canonical_model.as_deref() == Some(model)
            })
        });
    Some(!listed)
}

fn merge_durable_scope(
    mut durable_cache: ProviderCatalogCache,
    process_cache: &ProviderCatalogCache,
    provider: &str,
    fingerprint: &str,
) -> ProviderCatalogCache {
    durable_cache
        .entries
        .retain(|_, entry| !is_account_scoped_scope(&entry.provider, &entry.base_url_fingerprint));
    if !is_account_scoped_scope(provider, fingerprint)
        && let Some(entry) = process_cache.get(provider, fingerprint).cloned()
    {
        durable_cache.entries.insert(
            ProviderCatalogCache::cache_key(provider, fingerprint),
            entry,
        );
    }
    durable_cache
}

fn persisted_envelope_len(cache: &ProviderCatalogCache) -> Result<u64> {
    struct Counter(u64);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len() as u64);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let envelope = PersistedProviderCatalogsRef {
        schema_version: CACHE_SCHEMA_VERSION,
        cache,
    };
    let mut counter = Counter(0);
    serde_json::to_writer_pretty(&mut counter, &envelope)
        .context("measure provider catalog cache for bounded persistence")?;
    Ok(counter.0.saturating_add(1))
}

fn cached_row_count(cache: &ProviderCatalogCache) -> usize {
    cache.entries.values().fold(0usize, |total, entry| {
        total.saturating_add(entry.offerings.len())
    })
}

/// Compact a durable cache without ever truncating one provider roster.
///
/// The exact scope being written is protected: if that scope alone fits, older
/// failed/stale scopes are evicted whole until the envelope is bounded. If the
/// protected scope alone does not fit, persistence is refused and the prior
/// atomic file remains intact. This avoids both self-bricking the 32 MiB read
/// limit and turning a partial provider roster into false authoritative truth.
fn bounded_cache_for_persistence(
    mut cache: ProviderCatalogCache,
    protected_scope: Option<(&str, &str)>,
    now: u64,
    limits: CachePersistenceLimits,
) -> Result<ProviderCatalogCache> {
    cache
        .entries
        .retain(|_, entry| !is_account_scoped_scope(&entry.provider, &entry.base_url_fingerprint));

    let protected_key = protected_scope
        .filter(|(provider, fingerprint)| !is_account_scoped_scope(provider, fingerprint))
        .map(|(provider, fingerprint)| ProviderCatalogCache::cache_key(provider, fingerprint));

    if let Some(key) = protected_key.as_deref()
        && let Some(entry) = cache.entries.get(key).cloned()
    {
        let mut protected_only = ProviderCatalogCache::new();
        protected_only.entries.insert(key.to_string(), entry);
        anyhow::ensure!(
            protected_only.entries.len() <= limits.max_scopes.min(MAX_CACHE_SCOPES)
                && cached_row_count(&protected_only) <= limits.max_rows
                && persisted_envelope_len(&protected_only)? <= limits.max_bytes,
            "provider catalog scope {key:?} exceeds bounded persistence limits"
        );
    }

    // Rank once while the cache/file locks are held. An older implementation
    // reserialized and rescanned the entire envelope for every eviction, which
    // made a valid sub-32-MiB file with many tiny scopes quadratic to compact.
    let mut eviction_keys = cache
        .entries
        .iter()
        .filter(|(key, _)| protected_key.as_deref() != Some(key.as_str()))
        .map(|(key, entry)| {
            let health_rank = if matches!(entry.status, CatalogStatus::Failed { .. }) {
                0u8
            } else if entry.is_stale(now) || matches!(entry.status, CatalogStatus::Stale { .. }) {
                1u8
            } else {
                2u8
            };
            (health_rank, entry.fetched_at, key.clone())
        })
        .collect::<Vec<_>>();
    eviction_keys.sort();
    let eviction_keys = eviction_keys
        .into_iter()
        .map(|(_, _, key)| key)
        .collect::<Vec<_>>();
    let mut eviction_index = 0usize;
    let mut rows = cached_row_count(&cache);
    let max_scopes = limits.max_scopes.min(MAX_CACHE_SCOPES);

    let mut evict_next = |cache: &mut ProviderCatalogCache| -> Result<usize> {
        let key = eviction_keys
            .get(eviction_index)
            .context("provider catalog envelope cannot fit even after whole-scope compaction")?;
        eviction_index = eviction_index.saturating_add(1);
        let entry = cache
            .entries
            .remove(key)
            .context("provider catalog eviction candidate disappeared")?;
        Ok(entry.offerings.len())
    };

    // First enforce the cheap cardinality limits in bulk. Only after at most 64
    // scopes remain do we serialize to enforce the exact on-disk byte limit.
    while cache.entries.len() > max_scopes || rows > limits.max_rows {
        rows = rows.saturating_sub(evict_next(&mut cache)?);
    }
    while persisted_envelope_len(&cache)? > limits.max_bytes {
        let _removed_rows = evict_next(&mut cache)?;
    }

    Ok(cache)
}

fn write_bounded_cache(
    path: &Path,
    cache: ProviderCatalogCache,
    protected_scope: Option<(&str, &str)>,
    limits: CachePersistenceLimits,
) -> Result<()> {
    let cache = bounded_cache_for_persistence(cache, protected_scope, now_unix(), limits)?;
    let envelope = PersistedProviderCatalogs {
        schema_version: CACHE_SCHEMA_VERSION,
        cache,
    };
    anyhow::ensure!(
        persisted_envelope_len(&envelope.cache)? <= limits.max_bytes,
        "bounded provider catalog cache exceeds its write limit"
    );
    atomic_write_json(path, &envelope)
}

fn persist_scope(cache: &ProviderCatalogCache, provider: &str, fingerprint: &str) -> bool {
    let Some(path) = cache_path() else {
        return false;
    };
    let provider = canonical_provider_scope(provider);
    let result = (|| -> Result<()> {
        let lock_file = open_cache_lock(&cache_lock_path(&path))?;
        let mut lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock
            .write()
            .with_context(|| format!("write-lock provider catalog cache {}", path.display()))?;
        // Merge only the exact scope this process just changed into the latest
        // disk snapshot. A stale long-running TUI therefore cannot erase a
        // different scope written by the Runtime API (or vice versa).
        let durable_cache = merge_durable_scope(
            if path.exists() {
                load_from_disk_unlocked(&path).context("invalid prior catalog cache")?
            } else {
                ProviderCatalogCache::new()
            },
            cache,
            &provider,
            fingerprint,
        );
        write_bounded_cache(
            &path,
            durable_cache,
            Some((&provider, fingerprint)),
            CACHE_PERSISTENCE_LIMITS,
        )
        .with_context(|| format!("atomically write provider catalog {}", path.display()))
    })();
    if let Err(error) = result {
        tracing::debug!(
            target: "provider_catalog",
            error = %error,
            "provider catalog cache write failed"
        );
        return false;
    }
    true
}

/// Persist a failure without letting a stale process replace newer rows from
/// another Codewhale process for the same exact scope.
///
/// The ordinary scoped merge is sufficient for successes because the response
/// being committed is the new roster. A failure is different: its process may
/// have started with an older last-known-good entry. Re-read the durable exact
/// scope while holding the cross-process write lock, prefer it when it is at
/// least as recent, then change only the status before writing. Thus a failed
/// refresh can preserve the newest roster without resurrecting its own stale
/// snapshot over another process's success.
fn persist_failure_scope(
    cache: &mut ProviderCatalogCache,
    provider: &str,
    fingerprint: &str,
    reason: CatalogRefreshError,
) {
    let Some(path) = cache_path() else {
        return;
    };
    let provider = canonical_provider_scope(provider);
    let result = (|| -> Result<()> {
        let lock_file = open_cache_lock(&cache_lock_path(&path))?;
        let mut lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock
            .write()
            .with_context(|| format!("write-lock provider catalog cache {}", path.display()))?;
        let durable_cache = if path.exists() {
            load_from_disk_unlocked(&path).context("invalid prior catalog cache")?
        } else {
            ProviderCatalogCache::new()
        };

        if !is_account_scoped_scope(&provider, fingerprint)
            && let Some(durable_entry) = durable_cache.get(&provider, fingerprint).cloned()
        {
            let durable_is_newer = cache
                .get(&provider, fingerprint)
                .is_none_or(|local| durable_entry.fetched_at >= local.fetched_at);
            if durable_is_newer {
                cache.entries.insert(
                    ProviderCatalogCache::cache_key(&provider, fingerprint),
                    durable_entry,
                );
                cache.record_failure(&provider, fingerprint, reason);
            }
        }

        let durable_cache = merge_durable_scope(durable_cache, cache, &provider, fingerprint);
        write_bounded_cache(
            &path,
            durable_cache,
            Some((&provider, fingerprint)),
            CACHE_PERSISTENCE_LIMITS,
        )
        .with_context(|| format!("atomically write provider catalog {}", path.display()))
    })();
    if let Err(error) = result {
        tracing::debug!(
            target: "provider_catalog",
            error = %error,
            "provider catalog failure receipt write failed"
        );
    }
}

fn publish_exact_scope_for_identity(
    cache: &ProviderCatalogCache,
    provider_kind: ProviderKind,
    provider_identity: &str,
    fingerprint: &str,
) -> usize {
    let provider = canonical_provider_scope(provider_identity);
    let offerings = cache
        .get(&storage_provider(provider_kind, &provider), fingerprint)
        .map(|entry| entry.offerings.clone())
        .unwrap_or_default();
    let count = offerings.len();
    crate::provider_lake::replace_provider_live_snapshot_for_identity(
        provider_kind,
        &provider,
        CatalogSnapshot { offerings },
    );
    count
}

/// Load and publish only the active route's exact provider/base-URL scope.
///
/// A cache created for another custom endpoint or for an old endpoint override
/// is retained on disk but cannot leak into the active picker.
pub fn maybe_load_persisted_cache_for_config(config: &Config) -> usize {
    let Ok(identity) = config.active_provider_identity() else {
        return 0;
    };
    let provider = identity.provider;
    let provider_identity = canonical_provider_scope(identity.key.as_str());
    let fingerprint = base_url_fingerprint(&config.base_url_for_route(&identity));
    if is_account_scoped_scope(
        &storage_provider(provider, &provider_identity),
        &fingerprint,
    ) {
        forget_account_scoped_provider(provider, &provider_identity);
        return 0;
    }
    if let Ok(mut guard) = CACHE.write()
        && let Some(loaded) = load_from_disk()
    {
        // Keep session-only scopes that cannot exist on disk, while allowing a
        // newer durable scope from another Codewhale process to refresh this
        // process. Every in-process writer takes CACHE before the file lock, so
        // this read/merge cannot overwrite a concurrent local refresh.
        for (key, entry) in loaded.entries {
            let should_replace = guard
                .entries
                .get(&key)
                .is_none_or(|current| entry.fetched_at >= current.fetched_at);
            if should_replace {
                guard.entries.insert(key, entry);
            }
        }
    }
    CACHE
        .read()
        .map(|guard| {
            publish_exact_scope_for_identity(&guard, provider, &provider_identity, &fingerprint)
        })
        .unwrap_or(0)
}

fn forget_account_scoped_provider(provider_kind: ProviderKind, provider: &str) {
    let provider = canonical_provider_scope(provider);
    if let Ok(mut cache) = CACHE.write() {
        cache
            .entries
            .retain(|_, entry| entry.provider != storage_provider(provider_kind, &provider));
    }
    crate::provider_lake::replace_provider_live_snapshot_for_identity(
        provider_kind,
        &provider,
        CatalogSnapshot::default(),
    );
}

/// Begin a provider refresh and invalidate older in-flight results.
///
/// Account-scoped Baseten and Codewhale routes additionally drop their prior
/// in-memory rosters: the same URL can expose different models after a credential
/// change, and no safe account identifier is available for cache reuse.
#[cfg(test)]
pub fn begin_refresh(provider: &str) -> ProviderCatalogRefreshTicket {
    begin_refresh_inner(inferred_provider_kind(provider), provider, None)
}

pub fn begin_refresh_for_identity(
    provider_kind: ProviderKind,
    provider: &str,
    base_url: &str,
) -> ProviderCatalogRefreshTicket {
    begin_refresh_inner(
        provider_kind,
        provider,
        Some(base_url_fingerprint(base_url)),
    )
}

fn begin_refresh_inner(
    provider_kind: ProviderKind,
    provider: &str,
    fingerprint: Option<String>,
) -> ProviderCatalogRefreshTicket {
    let provider = canonical_provider_scope(provider);
    let scope = storage_provider(provider_kind, &provider);
    // Hold the generation gate through account-roster invalidation, so an older
    // refresh can never publish between the new ticket and the clear.
    let generation = if let Ok(mut generations) = REFRESH_GENERATIONS.write() {
        let generation = generations.entry(scope.clone()).or_default();
        *generation = generation.saturating_add(1);
        if fingerprint
            .as_deref()
            .is_some_and(|fp| is_account_scoped_scope(&scope, fp))
        {
            forget_account_scoped_provider(provider_kind, &provider);
        }
        *generation
    } else {
        0
    };
    ProviderCatalogRefreshTicket {
        provider,
        provider_kind,
        fingerprint,
        generation,
    }
}

fn with_current_ticket<T>(
    ticket: &ProviderCatalogRefreshTicket,
    provider: &str,
    operation: impl FnOnce() -> T,
) -> Option<T> {
    let provider = canonical_provider_scope(provider);
    if ticket.provider != provider {
        return None;
    }
    let generations = REFRESH_GENERATIONS.read().ok()?;
    if generations
        .get(&storage_provider(ticket.provider_kind, &ticket.provider))
        .copied()
        != Some(ticket.generation)
    {
        return None;
    }
    // Keep the generation read guard alive through publication. A newer
    // `begin_refresh` needs the write lock, so it cannot slip between the
    // current-ticket check and this operation's cache/lake update.
    let result = operation();
    drop(generations);
    Some(result)
}

/// Record a successful refresh only if no newer refresh superseded it.
pub fn record_success_if_current(
    ticket: &ProviderCatalogRefreshTicket,
    delta: ProviderCatalogDelta,
) -> Option<CatalogStatus> {
    let provider = canonical_provider_scope(&delta.provider);
    if ticket
        .fingerprint
        .as_ref()
        .is_some_and(|fp| fp != &delta.base_url_fingerprint)
    {
        return None;
    }
    with_current_ticket(ticket, &provider, || {
        record_success_for_identity(ticket.provider_kind, delta)
    })
}

/// Record a failed refresh only if no newer refresh superseded it.
pub fn record_failure_if_current(
    ticket: &ProviderCatalogRefreshTicket,
    provider: &str,
    fingerprint: &str,
    reason: CatalogRefreshError,
) -> Option<CatalogStatus> {
    let provider = canonical_provider_scope(provider);
    if ticket
        .fingerprint
        .as_deref()
        .is_some_and(|fp| fp != fingerprint)
    {
        return None;
    }
    with_current_ticket(ticket, &provider, || {
        record_failure_for_identity(ticket.provider_kind, &provider, fingerprint, reason)
    })
}

/// Current freshness receipt for one exact provider/base-URL scope.
///
/// Runtime route resolution uses this independently from picker visibility:
/// stale or failed rows may remain selectable as an explicit fallback, but
/// their limits, capabilities, and prices are not treated as current endpoint
/// facts during execution.
#[cfg(test)]
pub fn status_for_scope(provider: &str, base_url: &str) -> CatalogStatus {
    let fingerprint = base_url_fingerprint(base_url);
    status_for_fingerprint(provider, &fingerprint)
}

/// Current freshness receipt when the caller already owns the endpoint
/// fingerprint (for example, an immutable usage-pricing receipt).
#[cfg(test)]
pub(crate) fn status_for_fingerprint(provider: &str, fingerprint: &str) -> CatalogStatus {
    status_for_route_fingerprint(inferred_provider_kind(provider), provider, fingerprint)
}

pub(crate) fn status_for_route(
    provider: ProviderKind,
    identity: &str,
    base_url: &str,
) -> CatalogStatus {
    status_for_route_fingerprint(provider, identity, &base_url_fingerprint(base_url))
}

fn status_for_route_fingerprint(
    kind: ProviderKind,
    provider: &str,
    fingerprint: &str,
) -> CatalogStatus {
    let provider = storage_provider(kind, provider);
    CACHE
        .read()
        .map(|cache| cache.status(&provider, fingerprint, now_unix()))
        .unwrap_or(CatalogStatus::Unknown)
}

/// Freeze the exact reviewed provider-live rate row fresh at CodeWhale's
/// pre-permit application-dispatch boundary.
///
/// Status, scope, model, source, and rates are all read beneath one `CACHE`
/// read guard. The returned value owns every fact needed by later auditing, so
/// completion-time code never re-opens mutable catalog or provider-lake state.
fn reviewed_provider_live_scope(
    provider: ProviderKind,
    provider_identity: &str,
    endpoint_fingerprint: &str,
) -> bool {
    match provider {
        ProviderKind::Openrouter => {
            provider_identity == ProviderKind::Openrouter.as_str()
                && endpoint_fingerprint
                    == base_url_fingerprint(crate::config::DEFAULT_OPENROUTER_BASE_URL)
        }
        ProviderKind::Custom => {
            endpoint_fingerprint
                == base_url_fingerprint(codewhale_config::catalog::BASETEN_BASE_URL)
        }
        _ => false,
    }
}

#[must_use]
pub(crate) fn fresh_provider_live_pricing_quote_at(
    provider: ProviderKind,
    provider_identity: &str,
    wire_model: &str,
    endpoint_fingerprint: &str,
    dispatched_at_unix: u64,
) -> Option<ProviderLivePricingQuote> {
    let provider_identity = canonical_provider_scope(provider_identity);
    let wire_model = wire_model.trim();
    let endpoint_fingerprint = endpoint_fingerprint.trim();
    if provider_identity.is_empty()
        || wire_model.is_empty()
        || !reviewed_provider_live_scope(provider, &provider_identity, endpoint_fingerprint)
    {
        return None;
    }

    let cache = CACHE.read().ok()?;
    let storage_scope = storage_provider(provider, &provider_identity);
    if cache.status(&storage_scope, endpoint_fingerprint, dispatched_at_unix)
        != CatalogStatus::Fresh
    {
        return None;
    }
    let entry = cache.get(&storage_scope, endpoint_fingerprint)?;
    if entry.provider != storage_scope
        || entry.base_url_fingerprint.trim() != endpoint_fingerprint
        || entry.fetched_at > dispatched_at_unix
    {
        return None;
    }
    let offering = entry.offerings.iter().find(|offering| {
        offering.provider.trim() == provider_identity && offering.wire_model_id.trim() == wire_model
    })?;
    let pricing = OfferingPricing::from_catalog_offering(offering)?;
    if pricing.provider.trim() != provider_identity
        || pricing.wire_model_id.trim() != wire_model
        || pricing.currency != Currency::Usd
        || pricing.provenance != PricingProvenance::ProviderLive
        || pricing.effective_at != Some(entry.fetched_at)
        || pricing.endpoint_fingerprint.as_deref() != Some(endpoint_fingerprint)
        || pricing.input_per_million.is_none()
        || pricing.output_per_million.is_none()
    {
        return None;
    }
    ProviderLivePricingQuote::from_pricing(
        provider,
        &provider_identity,
        wire_model,
        endpoint_fingerprint,
        entry.fetched_at,
        &pricing,
    )
}

/// A price is a fact, so it is in scope exactly where a fact is.
///
/// This defers to [`crate::provider_lake::cloud_facts_apply_to_route`] rather
/// than repeating the scope table. The copy it replaces admitted the dual-wire
/// and regional routes (`deepseek-anthropic`, `siliconflow-CN`) that the
/// catalog gate refuses; no price actually escaped through it, because the
/// offering lookup below independently returns a non-`CloudFacts` row on those
/// routes and the quote then fails — but that is one authority masking another,
/// not agreement, and it would become a real leak the moment either moved. The
/// copy also carried its own `!= OpenaiCodex` test, which `cloud_facts::scope`
/// has always enforced for every consumer.
///
/// The one condition that is this file's own: the *configured* identity must be
/// the canonical provider. A differently-named provider table pointing at the
/// official host is a separate credential and billing relationship.
fn cloud_pricing_scope(provider: ProviderKind, identity: &str, base_url: &str) -> bool {
    crate::provider_lake::cloud_facts_apply_to_route(provider, identity, base_url)
}

/// Capture the effective mutable price authority once. Provider-owned live
/// prices retain priority; signed cloud prices are admitted only on the exact
/// canonical official route. The historical wire field name remains stable.
pub(crate) fn configured_dispatch_pricing_quote_at(
    models: &[codewhale_config::catalog::configured::ConfiguredModel],
    provider: ProviderKind,
    identity: &str,
    model: &str,
    base_url: &str,
    dispatched_at: u64,
) -> Option<ProviderLivePricingQuote> {
    if provider == ProviderKind::OpenaiCodex {
        return None;
    }
    codewhale_config::catalog::configured::validate_configured_models(models).ok()?;
    let declared = models
        .iter()
        .find(|row| row.id == model && row.matches_route(identity, base_url))?;
    let cost = declared.cost.clone().unwrap_or_default();
    let pricing = OfferingPricing {
        provider: identity.to_string(),
        wire_model_id: model.to_string(),
        canonical_model: None,
        currency: Currency::Usd,
        input_per_million: cost.input,
        output_per_million: cost.output,
        cache_read_per_million: cost.cache_read,
        cache_write_per_million: cost.cache_write,
        provenance: PricingProvenance::UserOverride,
        effective_at: None,
        endpoint_fingerprint: Some(base_url_fingerprint(base_url)),
    };
    // Freeze even an unpriced declaration: missing rates must not fall through
    // to a same-named bundled or subsequently refreshed price.
    ProviderLivePricingQuote::from_pricing(
        provider,
        identity,
        model,
        &base_url_fingerprint(base_url),
        dispatched_at,
        &pricing,
    )
}

/// Pick the dispatch quote from an operator declaration and the endpoint's
/// catalog. A declared rate wins. A declaration with no rates yields to the
/// catalog's price for the same exact endpoint (#6690), and is kept (frozen,
/// rate-less) only when the catalog has none, so a same-named bundled price
/// still cannot fill the gap.
pub(crate) fn declared_or_catalog_quote(
    declared: Option<ProviderLivePricingQuote>,
    catalog: impl FnOnce() -> Option<ProviderLivePricingQuote>,
) -> Option<ProviderLivePricingQuote> {
    match declared {
        Some(quote) if quote.carries_rates() => Some(quote),
        declared => catalog().or(declared),
    }
}

pub(crate) fn fresh_dispatch_pricing_quote_at(
    provider: ProviderKind,
    provider_identity: &str,
    wire_model: &str,
    base_url: &str,
    dispatched_at_unix: u64,
) -> Option<ProviderLivePricingQuote> {
    let endpoint_fingerprint = base_url_fingerprint(base_url);
    if let Some(quote) = fresh_provider_live_pricing_quote_at(
        provider,
        provider_identity,
        wire_model,
        &endpoint_fingerprint,
        dispatched_at_unix,
    ) {
        return Some(quote);
    }
    if !cloud_pricing_scope(provider, provider_identity, base_url) {
        return None;
    }
    let snapshot = codewhale_config::cloud_facts::overlay::snapshot();
    let facts = snapshot.facts.as_ref()?;
    let offering = crate::provider_lake::catalog_offering_for_route(
        provider,
        provider_identity,
        base_url,
        wire_model,
    )?;
    let codewhale_config::catalog::CatalogSource::CloudFacts {
        facts_version,
        key_id,
        fetched_at,
        valid_until,
    } = offering.pricing_source()
    else {
        return None;
    };
    // Provider cache files cannot authenticate a cloud price by copying a
    // source stamp. This row must be a projection of the current verified
    // overlay, with matching independent price authority and exact wire ID.
    if offering.wire_model_id != wire_model
        || !matches!(
            offering.source,
            codewhale_config::catalog::CatalogSource::CloudFacts { .. }
        )
        || *facts_version != facts.facts_version
        || key_id != &facts.key_id
        || *valid_until != facts.valid_until
    {
        return None;
    }
    let pricing = OfferingPricing::from_catalog_offering_at(&offering, dispatched_at_unix)?;
    let mut quote = ProviderLivePricingQuote::from_pricing(
        provider,
        provider_identity,
        wire_model,
        &endpoint_fingerprint,
        *fetched_at,
        &pricing,
    )?;
    quote.cloud_facts = Some(CloudFactsPricingSource {
        facts_version: *facts_version,
        key_id: key_id.clone(),
        valid_until: *valid_until,
        base_url: base_url.to_string(),
    });
    quote.catalog_revision = ProviderLivePricingQuote::revision_for(
        quote.provider,
        &quote.provider_identity,
        &quote.wire_model,
        &quote.endpoint_fingerprint,
        quote.catalog_fetched_at,
        &quote.currency,
        &quote.provenance,
        &quote.input_per_million,
        &quote.output_per_million,
        &quote.cache_read_per_million,
        &quote.cache_write_per_million,
        quote.cloud_facts.as_ref(),
    )?;
    quote.pricing_for_route(
        provider,
        provider_identity,
        wire_model,
        &endpoint_fingerprint,
        dispatched_at_unix,
    )?;
    (snapshot.generation == codewhale_config::cloud_facts::overlay::snapshot().generation)
        .then_some(quote)
}

/// Record and atomically persist a successful provider refresh.
///
/// `ProviderCatalogCache::record_success` replaces the exact scope, so models
/// removed upstream disappear instead of accumulating forever.
#[cfg(test)]
pub fn record_success(delta: ProviderCatalogDelta) -> CatalogStatus {
    record_success_for_identity(inferred_provider_kind(&delta.provider), delta)
}

fn record_success_for_identity(
    kind: ProviderKind,
    mut delta: ProviderCatalogDelta,
) -> CatalogStatus {
    let provider = canonical_provider_scope(&delta.provider);
    delta.provider = storage_provider(kind, &provider);
    if delta.offerings.iter().any(|row| {
        row.provider != provider
            || !crate::provider_lake::valid_catalog_model_id(&row.wire_model_id)
            || !provider_cost_source_allowed(row)
    }) {
        return record_failure_for_identity(
            kind,
            &provider,
            &delta.base_url_fingerprint,
            CatalogRefreshError::InvalidResponse,
        );
    }
    let fingerprint = delta.base_url_fingerprint.clone();
    let Ok(mut guard) = CACHE.write() else {
        return CatalogStatus::Unknown;
    };
    guard.record_success(delta, DEFAULT_PROVIDER_CATALOG_TTL_SECS);
    let persisted = persist_scope(&guard, &storage_provider(kind, &provider), &fingerprint);
    publish_exact_scope_for_identity(&guard, kind, &provider, &fingerprint);
    if persisted {
        CatalogStatus::Fresh
    } else {
        CatalogStatus::Unknown
    }
}

/// Record a typed failure while preserving and republishing prior rows for the
/// exact route scope.
#[cfg(test)]
pub fn record_failure(
    provider: &str,
    fingerprint: &str,
    reason: CatalogRefreshError,
) -> CatalogStatus {
    record_failure_for_identity(
        inferred_provider_kind(provider),
        provider,
        fingerprint,
        reason,
    )
}

fn record_failure_for_identity(
    kind: ProviderKind,
    provider: &str,
    fingerprint: &str,
    reason: CatalogRefreshError,
) -> CatalogStatus {
    let provider = canonical_provider_scope(provider);
    let scope = storage_provider(kind, &provider);
    let Ok(mut guard) = CACHE.write() else {
        return CatalogStatus::Failed { reason };
    };
    guard.record_failure(&scope, fingerprint, reason);
    persist_failure_scope(&mut guard, &scope, fingerprint, reason);
    publish_exact_scope_for_identity(&guard, kind, &provider, fingerprint);
    CatalogStatus::Failed { reason }
}

#[cfg(test)]
pub(crate) fn reset_cache_for_test() {
    DISK_LOADED.store(false, Ordering::Release);
    if let Ok(mut cache) = CACHE.write() {
        *cache = ProviderCatalogCache::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ProviderConfig, ProviderKind, ProvidersConfig};
    use crate::test_support::{EnvVarGuard, lock_test_env};
    use codewhale_config::catalog::{CatalogOffering, CatalogSource};

    fn delta(provider: &str, fingerprint: &str, ids: &[&str]) -> ProviderCatalogDelta {
        delta_at(provider, fingerprint, ids, now_unix())
    }

    fn delta_at(
        provider: &str,
        fingerprint: &str,
        ids: &[&str],
        fetched_at: u64,
    ) -> ProviderCatalogDelta {
        ProviderCatalogDelta {
            provider: provider.to_string(),
            base_url_fingerprint: fingerprint.to_string(),
            fetched_at,
            offerings: ids
                .iter()
                .map(|id| CatalogOffering {
                    provider: provider.to_string(),
                    wire_model_id: (*id).to_string(),
                    endpoint_key: "chat".to_string(),
                    source: CatalogSource::Live {
                        base_url_fingerprint: fingerprint.to_string(),
                        fetched_at,
                    },
                    ..CatalogOffering::default()
                })
                .collect(),
        }
    }

    fn scope(identity: &str) -> String {
        storage_provider(inferred_provider_kind(identity), identity)
    }

    fn stored_delta(provider: &str, fingerprint: &str, ids: &[&str]) -> ProviderCatalogDelta {
        stored_delta_at(provider, fingerprint, ids, now_unix())
    }

    fn stored_delta_at(
        provider: &str,
        fingerprint: &str,
        ids: &[&str],
        fetched_at: u64,
    ) -> ProviderCatalogDelta {
        let mut delta = delta_at(provider, fingerprint, ids, fetched_at);
        delta.provider = scope(provider);
        delta
    }

    #[test]
    fn success_replaces_scope_and_failure_preserves_last_rows_on_disk() {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        if let Ok(mut cache) = CACHE.write() {
            *cache = ProviderCatalogCache::new();
        }

        assert_eq!(
            record_success(delta("openrouter", "fp", &["old"])),
            CatalogStatus::Fresh
        );
        assert_eq!(
            record_success(delta("openrouter", "fp", &["new"])),
            CatalogStatus::Fresh
        );
        assert!(matches!(
            record_failure("openrouter", "fp", CatalogRefreshError::RateLimited),
            CatalogStatus::Failed {
                reason: CatalogRefreshError::RateLimited
            }
        ));

        let loaded = load_from_disk().expect("persisted cache");
        let entry = loaded
            .get(&scope("openrouter"), "fp")
            .expect("OpenRouter scope");
        assert_eq!(entry.offerings.len(), 1);
        assert_eq!(entry.offerings[0].wire_model_id, "new");
        assert!(matches!(entry.status, CatalogStatus::Failed { .. }));
    }

    #[test]
    fn baseten_workspace_roster_is_session_only_and_clears_before_reauthentication() {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();

        let base_url = codewhale_config::catalog::BASETEN_BASE_URL;
        let fingerprint = base_url_fingerprint(base_url);
        record_success(delta(
            codewhale_config::catalog::BASETEN_PROVIDER_ID,
            &fingerprint,
            &["workspace-a-only-model"],
        ));
        assert!(
            crate::provider_lake::all_catalog_models_for_provider_identity(
                ProviderKind::Custom,
                Some(codewhale_config::catalog::BASETEN_PROVIDER_ID),
            )
            .contains(&"workspace-a-only-model".to_string())
        );
        assert!(
            load_from_disk().is_none_or(|cache| cache
                .get(
                    &scope(codewhale_config::catalog::BASETEN_PROVIDER_ID),
                    &fingerprint
                )
                .is_none()),
            "an account-scoped Baseten roster must never be durable without a safe account id"
        );

        let mut custom = std::collections::HashMap::new();
        custom.insert(
            codewhale_config::catalog::BASETEN_PROVIDER_ID.to_string(),
            ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some(base_url.to_string()),
                model: Some(codewhale_config::catalog::BASETEN_DEFAULT_MODEL.to_string()),
                ..ProviderConfig::default()
            },
        );
        let config = Config {
            provider: Some(codewhale_config::catalog::BASETEN_PROVIDER_ID.to_string()),
            providers: Some(ProvidersConfig {
                custom,
                ..ProvidersConfig::default()
            }),
            ..Config::default()
        };
        assert_eq!(maybe_load_persisted_cache_for_config(&config), 0);
        assert!(matches!(
            status_for_scope(codewhale_config::catalog::BASETEN_PROVIDER_ID, base_url),
            CatalogStatus::Unknown
        ));
        assert!(
            !crate::provider_lake::all_catalog_models_for_provider_identity(
                ProviderKind::Custom,
                Some(codewhale_config::catalog::BASETEN_PROVIDER_ID),
            )
            .contains(&"workspace-a-only-model".to_string()),
            "a new credential attempt must not see the previous workspace roster"
        );

        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
    }

    #[test]
    fn superseded_refresh_ticket_cannot_publish_a_late_response() {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();

        let old = begin_refresh("openrouter");
        let current = begin_refresh("openrouter");
        assert!(
            record_success_if_current(&old, delta("openrouter", "fp", &["late-old-model"]))
                .is_none()
        );
        assert!(
            record_success_if_current(&current, delta("openrouter", "fp", &["current-model"]),)
                .is_some()
        );
        assert_eq!(
            CACHE
                .read()
                .expect("cache")
                .get(&scope("openrouter"), "fp")
                .expect("current scope")
                .offerings[0]
                .wire_model_id,
            "current-model"
        );

        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
    }

    #[test]
    fn current_ticket_holds_generation_gate_through_publication() {
        let ticket = begin_refresh("generation-barrier-provider");
        let entered = std::sync::Arc::new(std::sync::Barrier::new(2));
        let release = std::sync::Arc::new(std::sync::Barrier::new(2));
        let publish_entered = std::sync::Arc::clone(&entered);
        let publish_release = std::sync::Arc::clone(&release);
        let publisher = std::thread::spawn(move || {
            with_current_ticket(&ticket, "generation-barrier-provider", || {
                publish_entered.wait();
                publish_release.wait();
            })
        });
        entered.wait();

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let newer = std::thread::spawn(move || {
            started_tx.send(()).expect("signal refresh start");
            let next = begin_refresh("generation-barrier-provider");
            finished_tx.send(next).expect("signal refresh finish");
        });
        started_rx.recv().expect("new refresh thread started");
        assert!(
            finished_rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "a newer generation must wait until the accepted result finishes publication"
        );

        release.wait();
        assert!(publisher.join().expect("publisher thread").is_some());
        assert!(
            finished_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .is_ok()
        );
        newer.join().expect("newer refresh thread");
    }

    #[test]
    fn stale_process_snapshots_merge_exact_scopes_under_file_lock() {
        let _env = lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());

        let mut process_a = ProviderCatalogCache::new();
        process_a.record_success(stored_delta("CustomA", "fp-a", &["upper-model"]), 60);
        persist_scope(&process_a, &scope("CustomA"), "fp-a");

        // Simulate another process that started before A wrote and therefore
        // has an empty/stale in-memory snapshot. Its scoped write must merge A
        // from disk rather than replacing the whole envelope.
        let mut process_b = ProviderCatalogCache::new();
        process_b.record_success(stored_delta("customa", "fp-b", &["lower-model"]), 60);
        persist_scope(&process_b, &scope("customa"), "fp-b");

        let loaded = load_from_disk().expect("merged durable cache");
        assert_eq!(
            loaded
                .get(&scope("CustomA"), "fp-a")
                .expect("case-sensitive upper scope")
                .offerings[0]
                .wire_model_id,
            "upper-model"
        );
        assert_eq!(
            loaded
                .get(&scope("customa"), "fp-b")
                .expect("case-sensitive lower scope")
                .offerings[0]
                .wire_model_id,
            "lower-model"
        );
    }

    #[test]
    fn stale_process_failure_preserves_newer_durable_rows_for_the_same_scope() {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();

        // Process B began with this old roster and still holds it in memory.
        let mut stale_process = ProviderCatalogCache::new();
        stale_process.record_success(stored_delta_at("openrouter", "fp", &["old-model"], 1), 60);
        persist_scope(&stale_process, &scope("openrouter"), "fp");
        *CACHE.write().expect("cache") = stale_process;

        // Process A completes a newer successful refresh for the same scope.
        let mut newer_process = ProviderCatalogCache::new();
        newer_process.record_success(stored_delta_at("openrouter", "fp", &["new-model"], 2), 60);
        persist_scope(&newer_process, &scope("openrouter"), "fp");

        // B then fails. Its failure status is current, but its old rows are
        // not: the transaction must retain A's newer durable roster.
        assert!(matches!(
            record_failure("openrouter", "fp", CatalogRefreshError::Network),
            CatalogStatus::Failed {
                reason: CatalogRefreshError::Network
            }
        ));
        let in_memory = CACHE.read().expect("cache");
        let entry = in_memory
            .get(&scope("openrouter"), "fp")
            .expect("failed scope");
        assert_eq!(entry.offerings[0].wire_model_id, "new-model");
        assert!(matches!(entry.status, CatalogStatus::Failed { .. }));
        drop(in_memory);

        let durable = load_from_disk().expect("durable cache");
        let entry = durable
            .get(&scope("openrouter"), "fp")
            .expect("durable failed scope");
        assert_eq!(entry.offerings[0].wire_model_id, "new-model");
        assert!(matches!(entry.status, CatalogStatus::Failed { .. }));

        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
    }

    #[test]
    fn oversized_cache_file_is_rejected_before_allocation() {
        let _env = lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let path = cache_path().expect("cache path");
        fs::create_dir_all(path.parent().expect("catalog directory")).expect("catalog directory");
        fs::File::create(&path)
            .and_then(|file| file.set_len(MAX_CACHE_BYTES + 1))
            .expect("sparse oversized cache");
        assert!(load_from_disk().is_none());
    }

    #[test]
    fn bounded_persistence_evicts_failed_then_stale_scopes_and_keeps_exact_owner() {
        let mut cache = ProviderCatalogCache::new();
        cache.record_success(
            stored_delta_at("failed", "fp", &["failed-model"], 10),
            1_000,
        );
        cache.record_failure(&scope("failed"), "fp", CatalogRefreshError::Network);
        cache.record_success(stored_delta_at("stale", "fp", &["stale-model"], 20), 1);
        cache.record_success(stored_delta_at("fresh", "fp", &["fresh-model"], 30), 1_000);
        cache.record_success(
            stored_delta_at("protected", "fp", &["protected-model"], 40),
            1_000,
        );

        let compacted = bounded_cache_for_persistence(
            cache,
            Some((&scope("protected"), "fp")),
            100,
            CachePersistenceLimits {
                max_bytes: u64::MAX,
                max_scopes: 2,
                max_rows: 100,
            },
        )
        .expect("bounded cache");

        assert!(compacted.get(&scope("protected"), "fp").is_some());
        assert!(compacted.get(&scope("fresh"), "fp").is_some());
        assert!(compacted.get(&scope("failed"), "fp").is_none());
        assert!(compacted.get(&scope("stale"), "fp").is_none());
    }

    #[test]
    fn bounded_persistence_evicts_whole_scopes_and_refuses_an_oversized_owner() {
        let mut cache = ProviderCatalogCache::new();
        cache.record_success(
            stored_delta_at("protected", "fp", &["one", "two"], 40),
            1_000,
        );
        cache.record_success(stored_delta_at("other", "fp", &["other"], 30), 1_000);
        let limits = CachePersistenceLimits {
            max_bytes: u64::MAX,
            max_scopes: 10,
            max_rows: 2,
        };

        let compacted = bounded_cache_for_persistence(
            cache.clone(),
            Some((&scope("protected"), "fp")),
            50,
            limits,
        )
        .expect("other scope can be evicted whole");
        assert_eq!(
            compacted
                .get(&scope("protected"), "fp")
                .expect("protected roster")
                .offerings
                .len(),
            2
        );
        assert!(compacted.get(&scope("other"), "fp").is_none());

        let mut oversized = cache;
        oversized.record_success(
            stored_delta_at("protected", "fp", &["one", "two", "three"], 50),
            1_000,
        );
        assert!(
            bounded_cache_for_persistence(oversized, Some((&scope("protected"), "fp")), 50, limits,)
                .is_err(),
            "a provider roster must be refused, never partially persisted"
        );
    }

    #[test]
    fn bounded_cache_write_matches_read_limit_and_round_trips_after_compaction() {
        let directory = tempfile::tempdir().expect("cache directory");
        let path = directory.path().join(CACHE_FILE);
        let mut protected_only = ProviderCatalogCache::new();
        protected_only.record_success(
            stored_delta_at("protected", "fp", &["protected-model"], 40),
            1_000,
        );
        let exact_bytes = persisted_envelope_len(&protected_only).expect("encoded length");
        let limits = CachePersistenceLimits {
            max_bytes: exact_bytes,
            max_scopes: 10,
            max_rows: 10,
        };

        let mut combined = protected_only;
        combined.record_success(
            stored_delta_at(
                "evicted",
                "fp",
                &["this-entire-scope-does-not-fit-the-byte-bound"],
                30,
            ),
            1_000,
        );
        write_bounded_cache(&path, combined, Some((&scope("protected"), "fp")), limits)
            .expect("bounded disk write");

        assert!(fs::metadata(&path).expect("cache metadata").len() <= exact_bytes);
        let loaded = load_from_disk_unlocked_with_limit(&path, exact_bytes)
            .expect("bounded cache must remain readable under the same cap");
        assert!(loaded.get(&scope("protected"), "fp").is_some());
        assert!(loaded.get(&scope("evicted"), "fp").is_none());
    }

    #[test]
    fn baseten_alias_roster_is_session_only_and_keeps_exact_ownership() {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();

        let alias = "base-ten";
        let fingerprint = base_url_fingerprint(codewhale_config::catalog::BASETEN_BASE_URL);
        record_success(delta(alias, &fingerprint, &["alias-workspace-model"]));

        assert!(
            crate::provider_lake::all_catalog_models_for_provider_identity(
                ProviderKind::Custom,
                Some(alias),
            )
            .contains(&"alias-workspace-model".to_string())
        );
        assert!(
            !crate::provider_lake::all_catalog_models_for_provider_identity(
                ProviderKind::Custom,
                Some(codewhale_config::catalog::BASETEN_PROVIDER_ID),
            )
            .contains(&"alias-workspace-model".to_string()),
            "a reviewed schema alias must not collapse distinct exact table ownership"
        );
        assert!(
            load_from_disk().is_none_or(|cache| cache.get(&scope(alias), &fingerprint).is_none()),
            "every Baseten schema alias must remain session-only"
        );

        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
    }

    #[test]
    fn different_base_url_fingerprints_do_not_share_rows() {
        let mut cache = ProviderCatalogCache::new();
        cache.record_success(stored_delta("baseten", "one", &["model-one"]), 60);
        cache.record_success(stored_delta("baseten", "two", &["model-two"]), 60);
        assert_eq!(
            cache.get(&scope("baseten"), "one").unwrap().offerings[0].wire_model_id,
            "model-one"
        );
        assert_eq!(
            cache.get(&scope("baseten"), "two").unwrap().offerings[0].wire_model_id,
            "model-two"
        );
    }

    #[test]
    fn missing_cache_for_changed_base_url_clears_the_previous_provider_partition() {
        let _live = crate::provider_lake::lock_live_snapshot();
        crate::provider_lake::clear_live_snapshot();
        let mut cache = ProviderCatalogCache::new();
        cache.record_success(
            stored_delta("baseten", "old-fp", &["old-endpoint-model"]),
            60,
        );

        assert_eq!(
            publish_exact_scope_for_identity(&cache, ProviderKind::Custom, "baseten", "old-fp"),
            1
        );
        assert_eq!(
            crate::provider_lake::all_catalog_models_for_provider_identity(
                crate::config::ProviderKind::Custom,
                Some("baseten"),
            ),
            vec!["old-endpoint-model".to_string()]
        );

        assert_eq!(
            publish_exact_scope_for_identity(&cache, ProviderKind::Custom, "baseten", "new-fp"),
            0
        );
        let after_switch = crate::provider_lake::all_catalog_models_for_provider_identity(
            crate::config::ProviderKind::Custom,
            Some("baseten"),
        );
        assert!(
            after_switch.is_empty(),
            "rows from the old Baseten endpoint must not survive a fingerprint change, and no compiled seed replaces them (#6289)"
        );
        crate::provider_lake::clear_live_snapshot();
    }

    #[test]
    fn disk_reload_rehydrates_and_exposes_six_hundred_openrouter_models() {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        crate::provider_lake::clear_live_snapshot();
        if let Ok(mut cache) = CACHE.write() {
            *cache = ProviderCatalogCache::new();
        }

        let config = Config {
            provider: Some("openrouter".to_string()),
            providers: Some(ProvidersConfig {
                openrouter: ProviderConfig {
                    base_url: Some("https://synthetic.openrouter.invalid/api/v1".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            }),
            ..Config::default()
        };
        let identity = config.active_provider_identity().unwrap();
        let provider = identity.key.as_str();
        let fingerprint = base_url_fingerprint(&config.active_route_base_url());
        let fetched_at = now_unix();
        let ids: Vec<String> = (0..600)
            .map(|index| format!("synthetic/openrouter-model-{index:03}"))
            .collect();
        let status = record_success(ProviderCatalogDelta {
            provider: provider.to_string(),
            base_url_fingerprint: fingerprint,
            fetched_at,
            offerings: ids
                .iter()
                .map(|id| CatalogOffering {
                    provider: provider.to_string(),
                    wire_model_id: id.clone(),
                    endpoint_key: "chat".to_string(),
                    source: CatalogSource::Live {
                        base_url_fingerprint: base_url_fingerprint(&config.active_route_base_url()),
                        fetched_at,
                    },
                    ..CatalogOffering::default()
                })
                .collect(),
        });
        assert_eq!(status, CatalogStatus::Fresh);
        assert!(cache_path().is_some_and(|path| path.is_file()));
        assert_eq!(
            crate::provider_lake::all_catalog_models_for_provider(ProviderKind::Openrouter),
            ids,
            "the string compatibility publisher must retain built-in OpenRouter ownership"
        );
        assert!(
            crate::provider_lake::all_catalog_models_for_provider_identity(
                ProviderKind::Custom,
                Some("openrouter"),
            )
            .is_empty(),
            "built-in OpenRouter rows must not enter the custom namespace"
        );

        // Simulate a new process: remove both in-memory owners, then republish
        // only through the durable startup load path.
        if let Ok(mut cache) = CACHE.write() {
            *cache = ProviderCatalogCache::new();
        }
        crate::provider_lake::clear_live_snapshot();

        assert_eq!(maybe_load_persisted_cache_for_config(&config), 600);
        let visible =
            crate::provider_lake::all_catalog_models_for_provider(ProviderKind::Openrouter);
        assert_eq!(visible.len(), 600);
        assert_eq!(visible.first(), ids.first());
        assert_eq!(visible.last(), ids.last());

        if let Ok(mut cache) = CACHE.write() {
            *cache = ProviderCatalogCache::new();
        }
        crate::provider_lake::clear_live_snapshot();
    }
    #[test]
    fn typed_refresh_keeps_builtin_and_same_named_custom_scopes_separate_on_disk() {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let endpoint = "https://api.openai.com/v1";
        let fingerprint = base_url_fingerprint(endpoint);
        let built_in = begin_refresh_for_identity(ProviderKind::Openai, "openai", endpoint);
        let custom = begin_refresh_for_identity(ProviderKind::Custom, "openai", endpoint);
        assert_eq!(
            record_success_if_current(
                &built_in,
                delta("openai", &fingerprint, &["built-in-model"])
            ),
            Some(CatalogStatus::Fresh)
        );
        assert_eq!(
            record_success_if_current(&custom, delta("openai", &fingerprint, &["custom-model"])),
            Some(CatalogStatus::Fresh)
        );
        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        for (kind, expected) in [
            (ProviderKind::Openai, "built-in-model"),
            (ProviderKind::Custom, "custom-model"),
        ] {
            let entry = cached_entry_for_route(kind, "openai", endpoint)
                .unwrap()
                .unwrap();
            assert_eq!(entry.offerings[0].wire_model_id, expected);
            assert_eq!(entry.offerings[0].provider, "openai");
        }
        assert!(
            cached_entry_for_route(ProviderKind::Custom, "OpenAI", endpoint)
                .unwrap()
                .is_none()
        );
        reset_cache_for_test();
    }

    #[test]
    fn typed_refresh_rejects_wrong_endpoint_and_superseded_endpoint_response() {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let first_url = "https://first.invalid/v1";
        let second_url = "https://second.invalid/v1";
        let first = begin_refresh_for_identity(ProviderKind::Custom, "ExactRoute", first_url);
        assert!(
            record_success_if_current(
                &first,
                delta(
                    "ExactRoute",
                    &base_url_fingerprint(second_url),
                    &["wrong-endpoint"]
                )
            )
            .is_none()
        );
        let second = begin_refresh_for_identity(ProviderKind::Custom, "ExactRoute", second_url);
        assert!(
            record_failure_if_current(
                &second,
                "ExactRoute",
                &base_url_fingerprint(first_url),
                CatalogRefreshError::Network
            )
            .is_none()
        );
        assert_eq!(
            record_success_if_current(
                &second,
                delta(
                    "ExactRoute",
                    &base_url_fingerprint(second_url),
                    &["current-model"]
                )
            ),
            Some(CatalogStatus::Fresh)
        );
        assert!(
            record_success_if_current(
                &first,
                delta(
                    "ExactRoute",
                    &base_url_fingerprint(first_url),
                    &["late-model"]
                )
            )
            .is_none()
        );
        assert!(
            cached_entry_for_route(ProviderKind::Custom, "ExactRoute", first_url)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            crate::provider_lake::catalog_models_for_route(
                ProviderKind::Custom,
                "ExactRoute",
                second_url
            ),
            vec!["current-model"]
        );
        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
    }

    #[test]
    fn differently_named_baseten_endpoint_never_persists_account_roster() {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let endpoint = codewhale_config::catalog::BASETEN_BASE_URL;
        let ticket = begin_refresh_for_identity(ProviderKind::Custom, "TeamServing", endpoint);
        assert_eq!(
            record_success_if_current(
                &ticket,
                delta(
                    "TeamServing",
                    &base_url_fingerprint(endpoint),
                    &["private-workspace-model"]
                )
            ),
            Some(CatalogStatus::Fresh)
        );
        assert_eq!(
            crate::provider_lake::catalog_models_for_route(
                ProviderKind::Custom,
                "TeamServing",
                endpoint
            ),
            vec!["private-workspace-model"]
        );
        assert!(
            !fs::read_to_string(cache_path().unwrap())
                .unwrap()
                .contains("private-workspace-model")
        );
        let _new_credentials =
            begin_refresh_for_identity(ProviderKind::Custom, "TeamServing", endpoint);
        assert!(
            crate::provider_lake::catalog_models_for_route(
                ProviderKind::Custom,
                "TeamServing",
                endpoint
            )
            .is_empty()
        );
        reset_cache_for_test();
        assert!(
            cached_entry_for_route(ProviderKind::Custom, "TeamServing", endpoint)
                .unwrap()
                .is_none()
        );
        crate::provider_lake::clear_live_snapshot();
    }

    #[test]
    fn codewhale_account_rosters_are_memory_only_and_replaced_after_credential_refresh() {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        for (kind, identity, endpoint) in [
            (
                ProviderKind::Codewhale,
                "codewhale",
                ProviderKind::Codewhale.provider().default_base_url(),
            ),
            (
                ProviderKind::Codewhale,
                "codewhale",
                "https://codewhale.account.invalid/v1",
            ),
            (
                ProviderKind::Custom,
                "PrivateAccount",
                ProviderKind::Codewhale.provider().default_base_url(),
            ),
        ] {
            let fingerprint = base_url_fingerprint(endpoint);
            let old = begin_refresh_for_identity(kind, identity, endpoint);
            assert_eq!(
                record_success_if_current(
                    &old,
                    delta(identity, &fingerprint, &["private-old-account-model"])
                ),
                Some(CatalogStatus::Fresh)
            );
            assert!(
                cached_entry_for_route(kind, identity, endpoint)
                    .unwrap()
                    .is_some()
            );
            assert!(
                !fs::read_to_string(cache_path().unwrap())
                    .unwrap()
                    .contains("private-old-account-model")
            );
            let current = begin_refresh_for_identity(kind, identity, endpoint);
            assert!(
                cached_entry_for_route(kind, identity, endpoint)
                    .unwrap()
                    .is_none()
            );
            assert!(
                !crate::provider_lake::catalog_models_for_route(kind, identity, endpoint)
                    .contains(&"private-old-account-model".to_string())
            );
            assert!(
                record_success_if_current(
                    &old,
                    delta(identity, &fingerprint, &["private-old-account-model"])
                )
                .is_none()
            );
            assert_eq!(
                record_success_if_current(
                    &current,
                    delta(identity, &fingerprint, &["private-new-account-model"])
                ),
                Some(CatalogStatus::Fresh)
            );
            assert_eq!(
                crate::provider_lake::catalog_models_for_route(kind, identity, endpoint),
                vec!["private-new-account-model"]
            );
            assert!(
                !fs::read_to_string(cache_path().unwrap())
                    .unwrap()
                    .contains("private-new-account-model")
            );
            reset_cache_for_test();
            crate::provider_lake::clear_live_snapshot();
            assert!(
                cached_entry_for_route(kind, identity, endpoint)
                    .unwrap()
                    .is_none()
            );
        }
        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
    }

    #[cfg(unix)]
    #[test]
    fn cache_reader_rejects_links_and_special_files_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.json");
        let envelope = PersistedProviderCatalogs {
            schema_version: CACHE_SCHEMA_VERSION,
            cache: ProviderCatalogCache::new(),
        };
        fs::write(&target, serde_json::to_vec(&envelope).unwrap()).unwrap();
        let symlink = dir.path().join("symlink.json");
        std::os::unix::fs::symlink(&target, &symlink).unwrap();
        assert!(load_from_disk_unlocked_with_limit(&symlink, MAX_CACHE_BYTES).is_none());
        let hardlink = dir.path().join("hardlink.json");
        fs::hard_link(&target, &hardlink).unwrap();
        assert!(load_from_disk_unlocked_with_limit(&hardlink, MAX_CACHE_BYTES).is_none());
        let fifo = dir.path().join("fifo.json");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo_c owns a NUL-terminated pathname for the call.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        assert!(load_from_disk_unlocked_with_limit(&fifo, MAX_CACHE_BYTES).is_none());
        assert!(open_cache_lock(&fifo).is_err());
        assert!(load_from_disk_unlocked_with_limit(dir.path(), MAX_CACHE_BYTES).is_none());
    }

    struct CloudQuoteReset;
    impl Drop for CloudQuoteReset {
        fn drop(&mut self) {
            codewhale_config::cloud_facts::overlay::clear();
            crate::provider_lake::clear_live_snapshot();
            reset_cache_for_test();
        }
    }

    fn install_cloud_quote_fixture(
        channel: &str,
        provider: ProviderKind,
        version: u64,
        input: f64,
        now: u64,
    ) {
        use codewhale_config::cloud_facts::{
            CloudFactsState, CloudFactsStatus, FactsOrigin, ModelFact, PricingFact, ScopedFacts,
            overlay,
        };
        let ticket = overlay::configure(true, "cloud-quote-fixture").unwrap();
        assert!(overlay::publish(
            &ticket,
            Some(ScopedFacts {
                channel: channel.into(),
                facts_version: version,
                key_id: "cwf-test-only".into(),
                valid_until: Some(now + 60),
                models: vec![ModelFact {
                    provider: provider.as_str().into(),
                    id: "cloud-quote-fixture".into(),
                    context_window: Some(16_384),
                    pricing: Some(PricingFact {
                        input_per_m: Some(input),
                        output_per_m: Some(2.0),
                        cache_read_per_m: None,
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            CloudFactsStatus {
                state: CloudFactsState::Verified {
                    channel: channel.into(),
                    facts_version: version,
                    key_id: "cwf-test-only".into(),
                    fetched_at: now,
                    origin: FactsOrigin::LocalFile,
                    stale: false,
                    patches: 1,
                    defaults: 0,
                    announcements: 0,
                },
                ..Default::default()
            },
        ));
    }

    #[test]
    fn cloud_quote_freezes_actual_dispatch_and_survives_refresh_disable_and_reload() {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _enabled = EnvVarGuard::remove("CODEWHALE_DISABLE_CLOUD_FACTS");
        let _reset = CloudQuoteReset;
        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let now = chrono::Utc::now();
        let at = now.timestamp() as u64;
        let base = crate::config::DEFAULT_OPENAI_BASE_URL;
        install_cloud_quote_fixture("quote-frozen", ProviderKind::Openai, 91, 1.0, at);
        let route = crate::cost_status::EffectiveRouteEnvelope::capture(
            None,
            ProviderKind::Openai,
            "openai",
            "cloud-quote-fixture",
            Some(base),
            now,
        );
        let quote = route
            .provider_live_pricing
            .as_ref()
            .expect("frozen cloud price");
        assert_eq!(quote.provenance, PricingProvenance::CloudFacts);
        assert_eq!(quote.cloud_facts.as_ref().unwrap().facts_version, 91);
        assert_eq!(quote.input_per_million.as_deref(), Some("1"));
        assert!(quote.cache_read_per_million.is_none());
        let encoded = serde_json::to_string(&route).unwrap();
        install_cloud_quote_fixture("quote-frozen", ProviderKind::Openai, 92, 9.0, at);
        let newer = fresh_dispatch_pricing_quote_at(
            ProviderKind::Openai,
            "openai",
            "cloud-quote-fixture",
            base,
            at,
        )
        .unwrap();
        assert_eq!(newer.input_per_million.as_deref(), Some("9"));
        assert_ne!(newer.catalog_revision, quote.catalog_revision);
        codewhale_config::cloud_facts::overlay::clear();
        assert!(
            fresh_dispatch_pricing_quote_at(
                ProviderKind::Openai,
                "openai",
                "cloud-quote-fixture",
                base,
                at,
            )
            .is_none()
        );
        let restored: crate::cost_status::EffectiveRouteEnvelope =
            serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored, route);
        let pricing = restored
            .provider_live_pricing
            .as_ref()
            .unwrap()
            .pricing_for_route(
                ProviderKind::Openai,
                "openai",
                "cloud-quote-fixture",
                &base_url_fingerprint(base),
                at,
            )
            .unwrap();
        assert_eq!(pricing.input_per_million, Some(1.0));
        // Expiry is checked at dispatch; loading later does not reprice history.
        assert!(
            quote
                .pricing_for_route(
                    ProviderKind::Openai,
                    "openai",
                    "cloud-quote-fixture",
                    &base_url_fingerprint(base),
                    at + 61,
                )
                .is_none()
        );
    }

    #[test]
    fn cloud_quote_rejects_cross_route_and_modified_source_receipts() {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _enabled = EnvVarGuard::remove("CODEWHALE_DISABLE_CLOUD_FACTS");
        let _reset = CloudQuoteReset;
        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let at = now_unix();
        let base = crate::config::DEFAULT_OPENAI_BASE_URL;
        install_cloud_quote_fixture("quote-binding", ProviderKind::Openai, 93, 1.0, at);
        let quote = fresh_dispatch_pricing_quote_at(
            ProviderKind::Openai,
            "openai",
            "cloud-quote-fixture",
            base,
            at,
        )
        .unwrap();
        for (provider, identity, endpoint) in [
            (ProviderKind::Openai, "openai", "https://proxy.example/v1"),
            (ProviderKind::Custom, "openai", base),
            (ProviderKind::Openai, "named-openai", base),
            (
                ProviderKind::Openai,
                "openai",
                "https://secret@api.openai.com/v1",
            ),
        ] {
            assert!(
                fresh_dispatch_pricing_quote_at(
                    provider,
                    identity,
                    "cloud-quote-fixture",
                    endpoint,
                    at
                )
                .is_none()
            );
        }
        let value = serde_json::to_value(&quote).unwrap();
        for (field, replacement) in [
            ("facts_version", serde_json::json!(94)),
            ("key_id", serde_json::json!("cwf-relabelled")),
            ("valid_until", serde_json::json!(at + 600)),
            ("base_url", serde_json::json!("https://proxy.example/v1")),
        ] {
            let mut modified = value.clone();
            modified["cloud_facts"][field] = replacement;
            assert!(
                serde_json::from_value::<ProviderLivePricingQuote>(modified).is_err(),
                "changed {field} accepted"
            );
        }
        let mut invalid = quote.clone();
        invalid.cloud_facts.as_mut().unwrap().base_url = "https://secret@api.openai.com/v1".into();
        assert!(serde_json::to_value(invalid).unwrap().is_null());
        assert!(
            quote
                .pricing_for_route(
                    ProviderKind::Openai,
                    "openai",
                    "different-model",
                    &base_url_fingerprint(base),
                    at,
                )
                .is_none()
        );
    }

    #[test]
    fn cloud_quote_static_endpoint_survives_operator_endpoint_changes() {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _enabled = EnvVarGuard::remove("CODEWHALE_DISABLE_CLOUD_FACTS");
        let _reset = CloudQuoteReset;
        reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let at = now_unix();
        let base = ProviderKind::Codewhale.provider().default_base_url();
        let private = "https://private.example/v1?api_key=public-test-marker";
        let declared = EnvVarGuard::set("CODEWHALE_API_BASE", private);
        // The ordinary runtime credential contract intentionally accepts this
        // explicit operator route. It is not a public signed-facts authority.
        assert!(codewhale_config::provider_base_url_is_official(
            codewhale_config::ProviderKind::Codewhale,
            private,
        ));
        install_cloud_quote_fixture("quote-static", ProviderKind::Codewhale, 94, 1.0, at);
        assert!(
            fresh_dispatch_pricing_quote_at(
                ProviderKind::Codewhale,
                "codewhale",
                "cloud-quote-fixture",
                private,
                at,
            )
            .is_none()
        );
        let quote = fresh_dispatch_pricing_quote_at(
            ProviderKind::Codewhale,
            "codewhale",
            "cloud-quote-fixture",
            base,
            at,
        )
        .unwrap();
        let encoded = serde_json::to_string(&quote).unwrap();
        assert!(!encoded.contains("private.example"));
        assert!(!encoded.contains("public-test-marker"));
        drop(declared);
        let removed = EnvVarGuard::remove("CODEWHALE_API_BASE");
        let reloaded: ProviderLivePricingQuote = serde_json::from_str(&encoded).unwrap();
        assert_eq!(reloaded, quote);
        drop(removed);
        let _changed = EnvVarGuard::set("CODEWHALE_API_BASE", "https://another-private.example/v1");
        assert_eq!(
            serde_json::from_str::<ProviderLivePricingQuote>(&encoded).unwrap(),
            quote
        );
        assert!(
            quote
                .pricing_for_route(
                    ProviderKind::Codewhale,
                    "codewhale",
                    "cloud-quote-fixture",
                    &base_url_fingerprint(base),
                    at,
                )
                .is_some()
        );
    }

    #[test]
    fn durable_provider_catalog_cannot_claim_cloud_or_override_price_authority() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let mut cache = ProviderCatalogCache::new();
        cache.record_success(
            stored_delta("openai", "fp", &["fixture"]),
            DEFAULT_PROVIDER_CATALOG_TTL_SECS,
        );
        let baseline = PersistedProviderCatalogs {
            schema_version: CACHE_SCHEMA_VERSION,
            cache,
        };
        fs::write(&path, serde_json::to_vec(&baseline).unwrap()).unwrap();
        assert!(load_from_disk_unlocked(&path).is_some());
        for source in [
            CatalogSource::CloudFacts {
                facts_version: 7,
                key_id: "cwf-test-only".into(),
                fetched_at: now_unix(),
                valid_until: None,
            },
            CatalogSource::ConfigOverride,
            CatalogSource::UserOverride,
            CatalogSource::Live {
                base_url_fingerprint: "other".into(),
                fetched_at: now_unix(),
            },
        ] {
            let mut forged = baseline.clone();
            forged.cache.entries.values_mut().next().unwrap().offerings[0].cost_source =
                Some(source);
            fs::write(&path, serde_json::to_vec(&forged).unwrap()).unwrap();
            assert!(load_from_disk_unlocked(&path).is_none());
        }
    }

    #[test]
    fn pin_drift_reads_a_fresh_roster_persisted_by_an_earlier_process() {
        // #6035: `codewhale doctor` and a just-started TUI have not touched
        // the in-process cache yet; the durable fresh roster must still count.
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        reset_cache_for_test();
        let config = Config::default();
        let base_url = config.base_url_for_route(
            &config
                .resolve_provider_selection_identity("deepseek")
                .unwrap(),
        );
        let fingerprint = base_url_fingerprint(&base_url);
        assert_eq!(
            record_success(delta("deepseek", &fingerprint, &["deepseek-flash"])),
            CatalogStatus::Fresh
        );
        // A new process: nothing loaded in memory, the roster only on disk.
        reset_cache_for_test();
        assert_eq!(
            pin_missing_from_fresh_roster(&config, "deepseek", "deepseek-retired"),
            Some(true)
        );
        assert_eq!(
            pin_missing_from_fresh_roster(&config, "deepseek", "deepseek-flash"),
            Some(false)
        );
        reset_cache_for_test();
    }
}
