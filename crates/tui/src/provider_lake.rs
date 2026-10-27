//! Configured provider/model lake facade (#3830, Wave 5b / #4188).
//!
//! Single seam over the Models.dev catalog layers and the configured-provider
//! predicate shared with `/provider`. Precedence is **provider-scoped live >
//! live Models.dev > bundled offline snapshot > legacy hardcoded fallback**.
//! Pickers, hotbar route slots, [`crate::model_inventory::ModelInventory`],
//! slash completions, and subagent validation should read model lists from here.
//!
//! [`codewhale_runtime::config::model_completion_names_for_provider`] is retained only as a
//! compatibility fallback for CodeWhale-only / local providers that Models.dev
//! does not represent (and for unbundled gateways until the live catalog covers
//! them).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use codewhale_config::catalog::{
    CatalogOffering, CatalogSnapshot, CatalogSource, CatalogStatus, base_url_fingerprint,
    bundled_catalog_offerings,
};
use codewhale_config::route::{ProviderModelOffering, RouteResolver, bundled_offerings};

use crate::codex_model_cache;
use crate::provider_readiness::provider_is_configured_for_active;
use codewhale_config::ProviderKind;
use codewhale_runtime::config::{
    Config, ProviderIdentity, model_completion_names_for_provider, opencode_go_model_id,
};

static BUNDLED_SNAPSHOT: std::sync::OnceLock<SharedSnapshot> = std::sync::OnceLock::new();

/// A catalog layer whose rows are reference-counted so the merged view can
/// share them instead of deep-cloning every offering.
///
/// The Models.dev layer is several thousand rows. Holding the merge as owned
/// `CatalogOffering`s kept a second full copy of that layer (and of the
/// bundled layer) resident for the life of the process; sharing rows means
/// only the rows a merge actually changes (cutlines, signed-facts patches,
/// provider-roster completion) are materialized again.
#[derive(Debug, Default)]
struct SharedSnapshot {
    offerings: Vec<Arc<CatalogOffering>>,
    /// `catalog_partition_key(provider) -> rows`, built once with the
    /// snapshot.
    ///
    /// Callers ask for one provider's rows per model row; scanning every
    /// offering in the process for each ask computed a `trim().to_string()`
    /// partition key per offering.
    by_provider: BTreeMap<String, Vec<Arc<CatalogOffering>>>,
}

impl SharedSnapshot {
    fn from_owned(snapshot: CatalogSnapshot) -> Self {
        Self::new(snapshot.offerings.into_iter().map(Arc::new).collect())
    }

    fn new(offerings: Vec<Arc<CatalogOffering>>) -> Self {
        let mut by_provider: BTreeMap<String, Vec<Arc<CatalogOffering>>> = BTreeMap::new();
        for row in &offerings {
            by_provider
                .entry(catalog_partition_key(&row.provider))
                .or_default()
                .push(Arc::clone(row));
        }
        Self {
            offerings,
            by_provider,
        }
    }

    fn offerings_for_provider(&self, provider: &str) -> Vec<&CatalogOffering> {
        self.by_provider
            .get(provider)
            .map(|rows| rows.iter().map(Arc::as_ref).collect())
            .unwrap_or_default()
    }
}

/// Source tag for live-catalog rows. Models.dev is a cross-provider catalog
/// that serves as the primary live layer; per-provider refreshes (e.g.
/// TelecomJS `/v1/models`) are a secondary layer that must coexist alongside
/// Models.dev rows without being wiped by a Models.dev refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LiveSource {
    /// The cross-provider Models.dev catalog refresh.
    ModelsDev,
    /// A per-provider `/v1/models` catalog refresh (e.g. TelecomJS TokenHub).
    PerProvider,
}

/// Optional live catalog snapshot(s), source-scoped (#4188 race fix).
///
/// Models.dev and every provider fetch maintain distinct partitions of live
/// rows. A Models.dev refresh replaces only Models.dev-sourced rows; a
/// per-provider merge adds/replaces only that provider's rows. This prevents a
/// later Models.dev `set_live_snapshot` from erasing TelecomJS rows and keeps
/// independent provider refreshes from erasing each other.
static LIVE_SNAPSHOT: RwLock<LiveSnapshotPartitions> = RwLock::new(LiveSnapshotPartitions {
    models_dev: None,
    per_provider: BTreeMap::new(),
});

/// Internal partition map: one Models.dev snapshot plus one snapshot per
/// provider-specific live fetch.
#[derive(Default)]
struct LiveSnapshotPartitions {
    models_dev: Option<SharedSnapshot>,
    per_provider: BTreeMap<LivePartitionOwner, CatalogSnapshot>,
}

/// Internal ownership key for one provider-owned live roster.
///
/// Catalog rows intentionally keep their public provider string for receipts and
/// cache compatibility. The storage key carries the route kind separately so an
/// exact custom table named `openai` cannot overwrite, suppress, or borrow the
/// built-in OpenAI partition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum LivePartitionOwner {
    BuiltIn(String),
    Custom(String),
}

impl LivePartitionOwner {
    fn identity(&self) -> &str {
        match self {
            Self::BuiltIn(identity) | Self::Custom(identity) => identity,
        }
    }
}

fn live_partition_owner_for_route(
    provider: ProviderKind,
    provider_identity: Option<&str>,
) -> LivePartitionOwner {
    let identity = catalog_provider_id_for_identity(provider, provider_identity);
    if provider == ProviderKind::Custom {
        LivePartitionOwner::Custom(catalog_partition_key(identity.as_ref()))
    } else {
        LivePartitionOwner::BuiltIn(catalog_partition_key(identity.as_ref()))
    }
}

fn inferred_live_partition_owner(provider: &str) -> LivePartitionOwner {
    let identity = catalog_partition_key(provider);
    ProviderKind::parse(&identity).map_or_else(
        || LivePartitionOwner::Custom(identity),
        |provider| {
            LivePartitionOwner::BuiltIn(catalog_partition_key(catalog_provider_id(provider)))
        },
    )
}

fn offerings_by_provider(
    offerings: Vec<CatalogOffering>,
) -> BTreeMap<LivePartitionOwner, Vec<CatalogOffering>> {
    let mut grouped = BTreeMap::new();
    for mut offering in offerings {
        let owner = inferred_live_partition_owner(&offering.provider);
        offering.provider = owner.identity().to_string();
        grouped.entry(owner).or_insert_with(Vec::new).push(offering);
    }
    grouped
}

/// Generation stamp for the live snapshot. Bumped (under the `LIVE_SNAPSHOT`
/// write lock) by [`set_live_snapshot`], [`merge_live_offerings`], and
/// [`clear_live_snapshot`] so the memoized merged snapshot below can detect
/// staleness without re-merging.
static LIVE_GENERATION: AtomicU64 = AtomicU64::new(0);

type MergedCacheEntry = ((u64, u64), Arc<SharedSnapshot>);

/// Memoized result of [`merged_snapshot`], tagged with the `LIVE_GENERATION`
/// it was computed from. Re-merging ~5,700 offerings per call made every
/// `/model` open pay a multi-second, UI-thread-blocking cost; the merge result
/// only changes when the live snapshot changes, so cache it.
static MERGED_CACHE: RwLock<Option<MergedCacheEntry>> = RwLock::new(None);

/// Generation/freshness-scoped route resolvers for provider-owned catalogs.
/// Picker calls read the merged snapshot directly; execution projects that
/// snapshot into the immutable `RouteResolver` seam and must not rebuild a
/// 600+ row OpenRouter catalog for every route candidate.
static RUNTIME_RESOLVER_CACHE: RwLock<BTreeMap<String, RuntimeResolverCacheEntry>> =
    RwLock::new(BTreeMap::new());
/// Most distinct endpoints the resolver cache holds at once. Entries from an
/// older catalog generation can never hit again and are dropped on insert;
/// this caps the endpoints of the current generation.
const MAX_RUNTIME_RESOLVER_CACHE_ENTRIES: usize = 64;

#[derive(Clone)]
struct RuntimeResolverCacheEntry {
    generation: u64,
    cloud_generation: u64,
    status_is_fresh: bool,
    endpoint_catalog_authoritative: bool,
    resolver: RouteResolver,
}

#[derive(Clone)]
pub(crate) struct RuntimeCatalogResolver {
    pub(crate) resolver: RouteResolver,
    pub(crate) endpoint_catalog_authoritative: bool,
}

fn bundled_snapshot() -> &'static SharedSnapshot {
    BUNDLED_SNAPSHOT.get_or_init(|| {
        SharedSnapshot::from_owned(CatalogSnapshot {
            offerings: bundled_catalog_offerings(),
        })
    })
}

/// Remove catalog rows that cannot use the selected provider's wire protocol.
///
/// OpenCode Go publishes one `/models` roster for both Chat Completions and
/// Anthropic Messages and Responses. Keep saved and live Go rows on the same
/// documented protocol roster, correcting stale endpoint metadata.
fn apply_provider_model_cutlines(mut snapshot: CatalogSnapshot) -> CatalogSnapshot {
    let mut is_opencode_go = provider_parse_memo();
    snapshot.offerings = snapshot
        .offerings
        .into_iter()
        .filter_map(|mut offering| {
            if is_opencode_go(&offering.provider) {
                canonicalize_opencode_go_row(&mut offering)?;
            }
            Some(offering)
        })
        .collect();
    snapshot
}

/// [`apply_provider_model_cutlines`] over shared rows: only the rows the
/// cutline rewrites are copied; every other row stays shared with its layer.
fn apply_provider_model_cutlines_shared(rows: Vec<Arc<CatalogOffering>>) -> SharedSnapshot {
    let mut is_opencode_go = provider_parse_memo();
    let offerings = rows
        .into_iter()
        .filter_map(|mut offering| {
            if is_opencode_go(&offering.provider) {
                canonicalize_opencode_go_row(Arc::make_mut(&mut offering))?;
            }
            Some(offering)
        })
        .collect();
    SharedSnapshot::new(offerings)
}

/// `ProviderKind::parse` scans every provider and alias list per call; the
/// distinct provider strings in a catalog are few, so resolve each distinct
/// string once instead of once per offering (boot-path profiles showed this
/// loop as the largest post-parse compute block).
fn provider_parse_memo() -> impl FnMut(&str) -> bool {
    let mut resolved: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    move |provider: &str| {
        if let Some(hit) = resolved.get(provider) {
            return *hit;
        }
        let hit = ProviderKind::parse(provider) == Some(ProviderKind::OpencodeGo);
        resolved.insert(provider.to_string(), hit);
        hit
    }
}

/// Canonicalize one OpenCode Go row onto its documented protocol roster.
/// `None` means the row is not on that roster and must be dropped.
fn canonicalize_opencode_go_row(offering: &mut CatalogOffering) -> Option<()> {
    let canonical = opencode_go_model_id(&offering.wire_model_id)?;
    let endpoint_key = codewhale_config::opencode_go_endpoint_key(canonical)?;
    offering.provider = ProviderKind::OpencodeGo.as_str().to_string();
    offering.wire_model_id = canonical.to_string();
    offering.endpoint_key = endpoint_key.to_string();
    Some(())
}

/// Set the live-catalog snapshot for a given source (#4188 race fix).
///
/// Source-scoped: a Models.dev refresh replaces only Models.dev-sourced rows;
/// a per-provider refresh replaces only the layers for providers represented
/// in that snapshot. Other providers and sources are preserved. This
/// eliminates the race where a Models.dev `set_live_snapshot` would erase
/// TelecomJS rows merged earlier.
pub fn set_live_snapshot(snapshot: CatalogSnapshot, source: LiveSource) {
    if let Ok(mut guard) = LIVE_SNAPSHOT.write() {
        let snapshot = apply_provider_model_cutlines(snapshot);
        let changed = match source {
            LiveSource::ModelsDev => {
                guard.models_dev = Some(SharedSnapshot::from_owned(snapshot));
                true
            }
            LiveSource::PerProvider => {
                let grouped = offerings_by_provider(snapshot.offerings);
                let changed = !grouped.is_empty();
                for (provider, offerings) in grouped {
                    guard
                        .per_provider
                        .insert(provider, CatalogSnapshot { offerings });
                }
                changed
            }
        };
        // Invalidate the memoized merged snapshot while still holding the
        // write lock so no reader can cache the old merge against the new
        // generation.
        if changed {
            LIVE_GENERATION.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// Replace one exact provider-owned live partition, including with no rows.
///
/// The generic [`set_live_snapshot`] derives partitions from rows, so an empty
/// snapshot cannot say which previous partition should disappear. Endpoint-
/// scoped persistent caches need that distinction: switching Baseten to a new
/// base URL with no matching cache must remove the old URL's Baseten rows
/// immediately instead of presenting them as if they belonged to the new host.
#[cfg(test)]
pub fn replace_provider_live_snapshot(provider: &str, snapshot: CatalogSnapshot) {
    let provider = provider.trim();
    if provider.is_empty() {
        return;
    }
    let owner = inferred_live_partition_owner(provider);
    replace_provider_live_snapshot_for_owner(owner, snapshot);
}

/// Replace one provider-owned partition with an explicit route-kind boundary.
///
/// Callers that know the concrete route must use this form. The legacy
/// string-only wrapper above remains for built-in publishers and older generic
/// tests, where a built-in-looking string necessarily denotes the built-in.
pub(crate) fn replace_provider_live_snapshot_for_identity(
    provider: ProviderKind,
    provider_identity: &str,
    snapshot: CatalogSnapshot,
) {
    let owner = live_partition_owner_for_route(provider, Some(provider_identity));
    if owner.identity().is_empty() {
        return;
    }
    replace_provider_live_snapshot_for_owner(owner, snapshot);
}

fn replace_provider_live_snapshot_for_owner(owner: LivePartitionOwner, snapshot: CatalogSnapshot) {
    let provider_key = owner.identity().to_string();
    let mut snapshot = if matches!(&owner, LivePartitionOwner::Custom(_)) {
        snapshot
    } else {
        apply_provider_model_cutlines(snapshot)
    };
    snapshot.offerings.retain_mut(|row| {
        if catalog_partition_key(&row.provider) != provider_key {
            return false;
        }
        row.provider.clone_from(&provider_key);
        true
    });

    if let Ok(mut guard) = LIVE_SNAPSHOT.write() {
        let previous = guard.per_provider.remove(&owner);
        let next = (!snapshot.offerings.is_empty()).then_some(snapshot);
        if let Some(next) = next.clone() {
            guard.per_provider.insert(owner, next);
        }
        if previous != next {
            LIVE_GENERATION.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// Clear all live snapshots (both Models.dev and per-provider partitions).
/// Used by tests and shutdown paths that need a full reset.
#[cfg_attr(not(test), expect(dead_code))]
pub fn clear_live_snapshot() {
    if let Ok(mut guard) = LIVE_SNAPSHOT.write() {
        guard.models_dev = None;
        guard.per_provider.clear();
        LIVE_GENERATION.fetch_add(1, Ordering::SeqCst);
    }
}

/// Merge additional live offerings into provider-scoped live partitions (#4188).
///
/// Unlike [`set_live_snapshot`] for `LiveSource::PerProvider` (which replaces
/// each represented provider's partition), this merges new rows by
/// `(provider, wire_model_id)` identity within that provider's partition,
/// preserving every other provider and the Models.dev partition. Production
/// rosters publish through `provider_catalog_live` (the guided-setup key probe
/// included), so this remains a fixture seam for lake tests.
#[cfg(test)]
pub fn merge_live_offerings(new_offerings: Vec<CatalogOffering>) {
    if new_offerings.is_empty() {
        return;
    }
    if let Ok(mut guard) = LIVE_SNAPSHOT.write() {
        for (provider, new_rows) in offerings_by_provider(new_offerings) {
            let existing = guard.per_provider.remove(&provider).unwrap_or_default();
            let mut merged: BTreeMap<(String, String), CatalogOffering> = BTreeMap::new();
            for row in existing.offerings {
                merged.insert((row.provider.clone(), row.wire_model_id.clone()), row);
            }
            for row in new_rows {
                merged.insert((row.provider.clone(), row.wire_model_id.clone()), row);
            }
            guard.per_provider.insert(
                provider,
                CatalogSnapshot {
                    offerings: merged.into_values().collect(),
                },
            );
        }
        LIVE_GENERATION.fetch_add(1, Ordering::SeqCst);
    }
}

/// Which live partition currently holds `(provider, wire_model_id)`, if any.
///
/// Per-provider `/models` rows win on collision, matching merge precedence.
/// Pricing uses this so a Models.dev capabilities overlay is never treated as
/// a rate source (#5241).
#[must_use]
pub fn live_catalog_origin(provider: ProviderKind, wire_model_id: &str) -> Option<LiveSource> {
    let catalog_id = catalog_provider_id(provider);
    let owner = LivePartitionOwner::BuiltIn(catalog_partition_key(catalog_id));
    let needle = wire_model_id.trim();
    if needle.is_empty() {
        return None;
    }
    let Ok(guard) = LIVE_SNAPSHOT.read() else {
        return None;
    };
    // Wire ids are opaque and case-sensitive: an exact row in either
    // partition decides before a case-folded one does.
    for exact in [true, false] {
        let matches = |row: &CatalogOffering| {
            row.provider.eq_ignore_ascii_case(catalog_id)
                && if exact {
                    row.wire_model_id == needle
                } else {
                    row.wire_model_id.eq_ignore_ascii_case(needle)
                }
        };
        if guard
            .per_provider
            .get(&owner)
            .is_some_and(|snap| snap.offerings.iter().any(matches))
        {
            return Some(LiveSource::PerProvider);
        }
        if guard
            .models_dev
            .as_ref()
            .is_some_and(|snap| snap.offerings.iter().any(|row| matches(row)))
        {
            return Some(LiveSource::ModelsDev);
        }
    }
    None
}

/// The row whose wire id is exactly `needle`, else the one wire id equal to
/// it ignoring ASCII case. Wire ids are opaque and case-sensitive, so the
/// fallback only forgives a case slip when it cannot choose between two
/// distinct ids; otherwise it answers nothing rather than another model's
/// metadata.
fn find_wire_model<'a>(
    rows: impl IntoIterator<Item = &'a CatalogOffering>,
    needle: &str,
) -> Option<&'a CatalogOffering> {
    let mut folded: Option<&'a CatalogOffering> = None;
    let mut ambiguous = false;
    for row in rows {
        if row.wire_model_id == needle {
            return Some(row);
        }
        if row.wire_model_id.eq_ignore_ascii_case(needle) {
            match folded {
                None => folded = Some(row),
                Some(first) if first.wire_model_id != row.wire_model_id => ambiguous = true,
                Some(_) => {}
            }
        }
    }
    folded.filter(|_| !ambiguous)
}

/// Serialize tests that mutate the process-wide live snapshot.
///
/// Lock ordering: this takes the test env barrier FIRST (skipped when the
/// calling thread already sealed the environment). Under `#[cfg(test)]` every
/// `codewhale_env_var` read blocks on that barrier, so a thread holding the
/// live-snapshot mutex while it waits for the barrier deadlocks against a
/// thread holding the barrier while it waits for this mutex — and libtest has
/// no per-test timeout, so one inverted pair hangs the whole test binary.
/// Acquiring the barrier here, before the mutex, makes that inversion
/// impossible for every caller at once.
#[cfg(test)]
pub(crate) struct LiveSnapshotLock {
    _live: std::sync::MutexGuard<'static, ()>,
    _env: Option<crate::test_support::TestEnvLock>,
}

#[cfg(test)]
pub(crate) fn lock_live_snapshot() -> LiveSnapshotLock {
    let env = if crate::test_support::current_thread_holds_test_env_lock() {
        None
    } else {
        Some(crate::test_support::lock_test_env())
    };
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    let live = LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    LiveSnapshotLock {
        _live: live,
        _env: env,
    }
}

/// The merged catalog snapshot: Models.dev rows override bundled rows on
/// `(provider, wire_model_id)` identity (#4188). A provider-owned live
/// partition is authoritative for that provider's complete roster, so it
/// suppresses both bundled and Models.dev rows for the provider rather than
/// merely overlaying matching ids. This is what lets a successful
/// `/v1/models` refresh remove models retired upstream. Failed refreshes retain
/// the last successful provider partition; clearing a partition restores the
/// offline/cross-provider fallbacks. The one row a provider partition does not
/// suppress is a signed row the payload explicitly attests is unlisted — see
/// the `retain` in [`compute_merged_snapshot`]. Roster rows are completed, not
/// replaced, where the roster itself stated nothing.
///
/// Memoized: the merge is recomputed only after a live-layer mutation bumps
/// `LIVE_GENERATION`; every other call returns the cached `Arc` (the picker
/// calls this per row, so it must be cheap).
fn merged_snapshot() -> Arc<SharedSnapshot> {
    let generation = (
        LIVE_GENERATION.load(Ordering::SeqCst),
        codewhale_config::cloud_facts::overlay::snapshot().generation,
    );
    if let Ok(guard) = MERGED_CACHE.read()
        && let Some((cached_generation, cached)) = guard.as_ref()
        && *cached_generation == generation
    {
        return Arc::clone(cached);
    }
    let merged = Arc::new(compute_merged_snapshot());
    if let Ok(mut guard) = MERGED_CACHE.write() {
        // `generation` was sampled before the live snapshot was read, so a
        // concurrent set/clear leaves this entry stale-tagged and the next
        // reader recomputes; the merge itself is always internally consistent.
        *guard = Some((generation, Arc::clone(&merged)));
    }
    merged
}

/// Uncached merge (see [`merged_snapshot`] for the caching seam).
fn compute_merged_snapshot() -> SharedSnapshot {
    let cloud = codewhale_config::cloud_facts::overlay::snapshot();
    let Ok(live) = LIVE_SNAPSHOT.read() else {
        return apply_provider_model_cutlines_shared(bundled_snapshot().offerings.clone());
    };
    if live.models_dev.is_none() && live.per_provider.is_empty() && cloud.facts.is_none() {
        return apply_provider_model_cutlines_shared(bundled_snapshot().offerings.clone());
    }

    let authoritative_providers: std::collections::BTreeSet<&str> = live
        .per_provider
        .keys()
        .filter_map(|owner| match owner {
            LivePartitionOwner::BuiltIn(identity) => Some(identity.as_str()),
            LivePartitionOwner::Custom(_) => None,
        })
        .collect();
    let is_authoritative = |provider: &str| {
        let key = catalog_partition_key(provider);
        authoritative_providers.contains(key.as_str())
    };
    let mut merged: BTreeMap<(String, String), Arc<CatalogOffering>> = BTreeMap::new();
    for row in &bundled_snapshot().offerings {
        merged.insert(
            (row.provider.clone(), row.wire_model_id.clone()),
            Arc::clone(row),
        );
    }
    if let Some(models_dev) = &live.models_dev {
        for row in &models_dev.offerings {
            merged.insert(
                (row.provider.clone(), row.wire_model_id.clone()),
                Arc::clone(row),
            );
        }
    }
    if let Some(facts) = &cloud.facts {
        // The patcher only reads, writes, or removes the keys a signed fact
        // names, so materialize just those rows as owned values and share the
        // rest untouched.
        let mut patched: BTreeMap<(String, String), CatalogOffering> = BTreeMap::new();
        for fact in &facts.models {
            let key = (fact.provider.clone(), fact.id.clone());
            if let Some(row) = merged.remove(&key) {
                patched.insert(key, Arc::unwrap_or_clone(row));
            }
        }
        codewhale_config::cloud_facts::catalog_patch::apply_model_patches(
            &mut patched,
            facts,
            cloud.fetched_at.unwrap_or(0),
        );
        merged.extend(patched.into_iter().map(|(key, row)| (key, Arc::new(row))));
    }
    let lower_rows = merged.clone();
    merged.retain(|(provider, model), row| {
        !is_authoritative(provider)
            || (matches!(row.source, CatalogSource::CloudFacts { .. })
                && cloud.facts.as_ref().is_some_and(|facts| {
                    codewhale_config::cloud_facts::catalog_patch::is_unlisted_attested(
                        facts, provider, model,
                    )
                }))
    });
    for (owner, provider_snapshot) in
        live.per_provider
            .iter()
            .filter_map(|(owner, snapshot)| match owner {
                LivePartitionOwner::BuiltIn(_) => Some((owner, snapshot)),
                LivePartitionOwner::Custom(identity) if ProviderKind::parse(identity).is_none() => {
                    Some((owner, snapshot))
                }
                LivePartitionOwner::Custom(_) => None,
            })
    {
        for row in &provider_snapshot.offerings {
            let mut row = row.clone();
            if matches!(owner, LivePartitionOwner::BuiltIn(_))
                && let Some(lower) =
                    lower_rows.get(&(row.provider.clone(), row.wire_model_id.clone()))
            {
                row.complete_from(lower, None);
            }
            merged.insert(
                (row.provider.clone(), row.wire_model_id.clone()),
                Arc::new(row),
            );
        }
    }
    apply_provider_model_cutlines_shared(merged.into_values().collect())
}

fn apply_cloud_facts_for_provider(
    rows: &mut BTreeMap<(String, String), CatalogOffering>,
    provider: &str,
    cloud: &codewhale_config::cloud_facts::overlay::OverlaySnapshot,
) {
    if let Some(facts) = &cloud.facts {
        let mut scoped = (**facts).clone();
        scoped.models.retain(|model| model.provider == provider);
        codewhale_config::cloud_facts::catalog_patch::apply_model_patches(
            rows,
            &scoped,
            cloud.fetched_at.unwrap_or(0),
        );
    }
}

static SOURCE_ROWS_CACHE: RwLock<BTreeMap<String, SourceRowsCacheEntry>> =
    RwLock::new(BTreeMap::new());

/// Most provider partitions [`SOURCE_ROWS_CACHE`] holds. A partition is one
/// provider's bundled + Models.dev rows, so this bounds resident memory to a
/// few providers at a time without tracking recency.
const MAX_SOURCE_ROWS_CACHE_ENTRIES: usize = 32;

type SourceRowsCacheEntry = (
    (u64, u64, bool),
    Arc<BTreeMap<(String, String), CatalogOffering>>,
);

/// One provider's bundled + Models.dev rows, memoized per live generation.
///
/// `live_generation` must be sampled **before** the caller reads
/// `LIVE_SNAPSHOT`: tagging a snapshot read with a newer generation would let a
/// concurrent refresh leave stale rows cached under the current generation.
fn source_rows_for_provider(
    provider: &str,
    live_generation: u64,
    models_dev: Option<&SharedSnapshot>,
    cloud: Option<&codewhale_config::cloud_facts::overlay::OverlaySnapshot>,
) -> Arc<BTreeMap<(String, String), CatalogOffering>> {
    let generation = (
        live_generation,
        cloud.map_or(u64::MAX, |cloud| cloud.generation),
        models_dev.is_some(),
    );
    if let Ok(cache) = SOURCE_ROWS_CACHE.read()
        && let Some((cached_generation, cached)) = cache.get(provider)
        && *cached_generation == generation
    {
        return Arc::clone(cached);
    }
    let rows = Arc::new(build_source_rows_for_provider(provider, models_dev, cloud));
    if let Ok(mut cache) = SOURCE_ROWS_CACHE.write() {
        cache.retain(|_, (cached_generation, _)| *cached_generation == generation);
        if cache.len() >= MAX_SOURCE_ROWS_CACHE_ENTRIES && !cache.contains_key(provider) {
            cache.pop_first();
        }
        cache.insert(provider.to_string(), (generation, Arc::clone(&rows)));
    }
    rows
}

fn build_source_rows_for_provider(
    provider: &str,
    models_dev: Option<&SharedSnapshot>,
    cloud: Option<&codewhale_config::cloud_facts::overlay::OverlaySnapshot>,
) -> BTreeMap<(String, String), CatalogOffering> {
    let mut rows: BTreeMap<_, _> = bundled_snapshot()
        .offerings_for_provider(provider)
        .into_iter()
        .cloned()
        .map(|row| ((row.provider.clone(), row.wire_model_id.clone()), row))
        .collect();
    if let Some(models_dev) = models_dev {
        for row in models_dev.offerings_for_provider(provider) {
            rows.insert(
                (row.provider.clone(), row.wire_model_id.clone()),
                row.clone(),
            );
        }
    }
    if let Some(cloud) = cloud {
        apply_cloud_facts_for_provider(&mut rows, provider, cloud);
    }
    rows
}

/// Does the signed cloud layer describe this exact route?
///
/// Every condition is load-bearing:
/// - the captured route identity is canonical; `deepseek-cn` remains a
///   separate identity even though its transport shares the DeepSeek kind;
/// - the identity the signer names is this route's own. Catalog rows collapse
///   regional and dual-wire aliases onto a vendor primary
///   ([`catalog_provider_id`]), so SiliconFlow China and DeepSeek's
///   Anthropic-wire route read the `siliconflow` / `deepseek` partitions — a
///   fact signed for the primary is not a fact about those other endpoints and
///   only an exact identity match may consume it. This is the same exact-
///   identity keying `cloud_default_model_for_route` already uses for defaults;
/// - the base URL is on that provider's official HTTPS contract, so a custom,
///   proxied or redirected endpoint never inherits signed facts.
///
/// `cloud_facts::scope` stays the single authority for which providers and
/// hosts are in scope at all (it is what excludes custom/local routes and the
/// Codex account roster); this must not grow a second copy of that table.
pub(crate) fn cloud_facts_apply_to_route(
    provider: ProviderKind,
    identity: &str,
    base_url: &str,
) -> bool {
    identity == provider.as_str()
        && provider.as_str() == catalog_provider_id(provider)
        && codewhale_config::cloud_facts::scope::base_url_allowed(provider.as_str(), base_url)
}

/// Signed rows for `provider` on this endpoint that the payload explicitly
/// attests exist despite the provider roster omitting them.
///
/// A provider `/v1/models` roster is authoritative for every id it lists **and
/// for its own omissions**: this client keeps no history of past rosters, so it
/// cannot tell a never-listed preview from a model the provider retired, and it
/// does not guess. The single exception is an explicit signed `allow_unlisted`
/// assertion, which the signer must renew as it expires (`not_after` is
/// mandatory for one). Everything else the payload says about this provider is
/// still a patch on rows that exist — never a reason to add one back.
///
/// The assertion carries exactly that: existence of that exact id. It is
/// filtered here by the same route gate as every other signed fact, so it
/// cannot reach a custom, proxied, regional or dual-wire endpoint, and it does
/// not touch account entitlement (an OAuth/account roster provider is outside
/// the signed scope entirely).
fn cloud_unlisted_offerings_for_route(
    provider: ProviderKind,
    identity: &str,
    base_url: &str,
) -> BTreeMap<(String, String), CatalogOffering> {
    let mut rows = BTreeMap::new();
    if !cloud_facts_apply_to_route(provider, identity, base_url) {
        return rows;
    }
    let cloud = codewhale_config::cloud_facts::overlay::snapshot();
    let Some(facts) = cloud.facts.as_ref() else {
        return rows;
    };
    let catalog_id = catalog_provider_id(provider);
    apply_cloud_facts_for_provider(&mut rows, catalog_id, &cloud);
    rows.retain(|(row_provider, row_id), _| {
        codewhale_config::cloud_facts::catalog_patch::is_unlisted_attested(
            facts,
            row_provider,
            row_id,
        )
    });
    rows
}

/// Maps an [`ProviderKind`] to its bundled-catalog provider id.
fn catalog_provider_id(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::DeepseekAnthropic => "deepseek",
        ProviderKind::SiliconflowCN => "siliconflow",
        _ => provider.as_str(),
    }
}

/// Exact partition key for one provider-owned catalog.
///
/// Publishers of built-in catalogs already emit their canonical provider id.
/// Custom table identities are ownership boundaries and therefore remain
/// case-sensitive even when their spelling resembles a built-in provider or a
/// reviewed setup-template alias: `[providers.openai]` may intentionally shadow
/// the built-in, and `CustomA` / `customa` may be different hosts.
pub(crate) fn catalog_partition_key(provider: &str) -> String {
    provider.trim().to_string()
}

/// Resolve the catalog partition for a concrete route.
///
/// `ProviderKind::Custom` is only the wire family. Named compatible providers
/// such as Baseten own independent catalogs and must keep their exact config
/// identity instead of collapsing into a shared `custom` bucket.
fn catalog_provider_id_for_identity<'a>(
    provider: ProviderKind,
    provider_identity: Option<&'a str>,
) -> Cow<'a, str> {
    if provider == ProviderKind::Custom
        && let Some(identity) = provider_identity.map(str::trim).filter(|id| !id.is_empty())
    {
        return Cow::Owned(catalog_partition_key(identity));
    }
    Cow::Borrowed(catalog_provider_id(provider))
}

fn offering_key(offering: &ProviderModelOffering) -> (String, String) {
    (
        offering.provider.as_str().trim().to_ascii_lowercase(),
        offering.wire_model_id.as_str().to_string(),
    )
}

fn offering_with_absent_facts_filled(
    mut offering: ProviderModelOffering,
    lower: &ProviderModelOffering,
) -> ProviderModelOffering {
    if offering.canonical_model.is_none() {
        offering.canonical_model.clone_from(&lower.canonical_model);
    }
    if offering.limits.context_tokens.is_none() {
        offering.limits.context_tokens = lower.limits.context_tokens;
    }
    if offering.limits.input_tokens.is_none() {
        offering.limits.input_tokens = lower.limits.input_tokens;
    }
    if offering.limits.output_tokens.is_none() {
        offering.limits.output_tokens = lower.limits.output_tokens;
    }
    for (target, source) in [
        (
            &mut offering.capabilities.attachments,
            lower.capabilities.attachments,
        ),
        (
            &mut offering.capabilities.image_input,
            lower.capabilities.image_input,
        ),
        (
            &mut offering.capabilities.reasoning,
            lower.capabilities.reasoning,
        ),
        (
            &mut offering.capabilities.native_tool_calls,
            lower.capabilities.native_tool_calls,
        ),
        (
            &mut offering.capabilities.structured_output,
            lower.capabilities.structured_output,
        ),
    ] {
        if *target == codewhale_config::route::CapabilityState::Unknown {
            *target = source;
        }
    }
    offering
}

fn row_matches_endpoint_fingerprint(row: &CatalogOffering, fingerprint: &str) -> bool {
    matches!(
        &row.source,
        CatalogSource::Live {
            base_url_fingerprint,
            ..
        } if base_url_fingerprint == fingerprint
    )
}

fn current_local_context(provider: ProviderKind, mut row: CatalogOffering) -> CatalogOffering {
    if provider != ProviderKind::Ollama {
        return row;
    }
    let now = codewhale_config::catalog::now_unix();
    let current = matches!(
        row.fact_source("limit.context"),
        CatalogSource::Live { base_url_fingerprint, fetched_at }
            if row_matches_endpoint_fingerprint(&row, base_url_fingerprint)
                && *fetched_at <= now
                && now - *fetched_at < 60
    );
    if !current && let Some(limit) = row.limit.as_mut() {
        limit.context = None;
    }
    row
}

/// Build or reuse the runtime resolver for an exact provider identity.
///
/// Only a fresh provider-owned partition whose source fingerprint matches the
/// selected endpoint can carry live limits, capabilities, and pricing into an
/// executable route. Stale, failed, unknown, or wrong-endpoint partitions stay
/// visible to the picker but are removed from this resolver and replaced by the
/// ordinary Models.dev/bundled fallback. Named compatible providers such as
/// Baseten are remapped from their exact catalog identity to the resolver's
/// `custom` transport scope only after this check.
pub(crate) fn runtime_catalog_resolver_for_identity(
    provider: ProviderKind,
    provider_identity: Option<&str>,
    base_url: &str,
    status: CatalogStatus,
) -> RuntimeCatalogResolver {
    let catalog_id = catalog_provider_id_for_identity(provider, provider_identity);
    let catalog_key = catalog_partition_key(catalog_id.as_ref());
    let fingerprint = base_url_fingerprint(base_url);
    let status_is_fresh = matches!(status, CatalogStatus::Fresh);
    let generation = LIVE_GENERATION.load(Ordering::SeqCst);
    let cloud = codewhale_config::cloud_facts::overlay::snapshot();
    let cloud_generation = cloud.generation;
    let cache_key = format!(
        "{}\u{1f}{}\u{1f}{}",
        provider.as_str(),
        catalog_key,
        fingerprint
    );

    if provider != ProviderKind::Ollama
        && let Ok(cache) = RUNTIME_RESOLVER_CACHE.read()
        && let Some(cached) = cache.get(&cache_key)
        && cached.generation == generation
        && cached.cloud_generation == cloud_generation
        && cached.status_is_fresh == status_is_fresh
    {
        return RuntimeCatalogResolver {
            resolver: cached.resolver.clone(),
            endpoint_catalog_authoritative: cached.endpoint_catalog_authoritative,
        };
    }

    let partition_owner = live_partition_owner_for_route(provider, provider_identity);
    let route_cloud = cloud_facts_apply_to_route(
        provider,
        provider_identity.unwrap_or(provider.as_str()),
        base_url,
    )
    .then_some(&cloud);
    let (endpoint_catalog_authoritative, selected_rows, lower_rows) =
        if let Ok(live) = LIVE_SNAPSHOT.read() {
            let exact_partition = live.per_provider.get(&partition_owner);
            let exact_matches = status_is_fresh
                && exact_partition.is_some_and(|partition| {
                    !partition.offerings.is_empty()
                        && partition.offerings.iter().all(|row| {
                            catalog_partition_key(&row.provider) == catalog_key
                                && row_matches_endpoint_fingerprint(row, &fingerprint)
                        })
                });
            let rows = if exact_matches {
                exact_partition
                    .map(|partition| partition.offerings.clone())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let lower = if !matches!(provider, ProviderKind::Custom | ProviderKind::OpenaiCodex) {
                source_rows_for_provider(
                    catalog_id.as_ref(),
                    generation,
                    live.models_dev.as_ref(),
                    route_cloud,
                )
            } else {
                Arc::new(BTreeMap::new())
            };
            (exact_matches, rows, lower)
        } else {
            let lower = if !matches!(provider, ProviderKind::Custom | ProviderKind::OpenaiCodex) {
                source_rows_for_provider(catalog_id.as_ref(), generation, None, route_cloud)
            } else {
                Arc::new(BTreeMap::new())
            };
            (false, Vec::new(), lower)
        };

    // Nonselected providers retain the bundled/curated resolver baseline.
    // Another endpoint's live roster must not alter this route's ownership
    // checks (including strict-direct rejection of known foreign model ids).
    let mut source_rows: BTreeMap<(String, String), CatalogOffering> = bundled_snapshot()
        .offerings
        .iter()
        .map(|row| CatalogOffering::clone(row))
        .map(|row| ((row.provider.clone(), row.wire_model_id.clone()), row))
        .collect();
    let cloud_applies = !endpoint_catalog_authoritative
        && cloud_facts_apply_to_route(
            provider,
            provider_identity.unwrap_or(provider.as_str()),
            base_url,
        );
    if !endpoint_catalog_authoritative {
        source_rows.retain(|(row_provider, _), _| row_provider != catalog_id.as_ref());
        source_rows.extend(
            lower_rows
                .iter()
                .map(|(key, row)| (key.clone(), row.clone())),
        );
    }
    let mut route_offerings: BTreeMap<(String, String), ProviderModelOffering> = source_rows
        .values()
        .map(CatalogOffering::to_offering)
        .map(|offering| (offering_key(&offering), offering))
        .collect();
    // Curated transport facts win ordinary Models.dev collisions, exactly as
    // in RouteResolver::new(). A fresh exact roster replaces its whole scope.
    for offering in bundled_offerings() {
        let key = offering_key(&offering);
        let enriched = match route_offerings.get(&key) {
            Some(lower) => offering_with_absent_facts_filled(offering, lower),
            None => offering,
        };
        route_offerings.insert(key, enriched);
    }
    // Keep curated transport identity, applying only fields explicitly signed
    // at the lower cloud layer. Hidden rows must not be resurrected here.
    if cloud_applies && let Some(facts) = &cloud.facts {
        for patch in facts
            .models
            .iter()
            .filter(|patch| patch.provider == catalog_id.as_ref())
        {
            let key = (patch.provider.clone(), patch.id.clone());
            match patch.op {
                codewhale_config::cloud_facts::types::ModelOp::Hide => {
                    route_offerings.remove(&key);
                }
                codewhale_config::cloud_facts::types::ModelOp::Upsert => {
                    if let Some(offering) = route_offerings.get_mut(&key) {
                        if let Some(context) = patch.context_window {
                            offering.limits.context_tokens = Some(context);
                        }
                        if let Some(output) = patch.max_output {
                            offering.limits.output_tokens = Some(output);
                        }
                        if let Some(reasoning) = patch.reasoning {
                            offering.capabilities.reasoning =
                                codewhale_config::route::CapabilityState::from_optional_bool(Some(
                                    reasoning,
                                ));
                        }
                        if patch.pricing.is_some()
                            && let Some(row) = source_rows.get(&key)
                        {
                            offering.pricing = codewhale_config::pricing::route_pricing_sku(row);
                        }
                    }
                }
                codewhale_config::cloud_facts::types::ModelOp::Deprecate => {}
            }
        }
    }
    if provider == ProviderKind::Ollama {
        route_offerings.retain(|_, offering| offering.provider.as_str() != catalog_id.as_ref());
    }
    if endpoint_catalog_authoritative {
        let transport_provider = if provider == ProviderKind::Custom {
            ProviderKind::Custom.as_str()
        } else {
            catalog_id.as_ref()
        };
        route_offerings.retain(|_, offering| offering.provider.as_str() != transport_provider);
        for mut row in selected_rows {
            if let Some(lower) = lower_rows.get(&(row.provider.clone(), row.wire_model_id.clone()))
            {
                row.complete_from(lower, Some(base_url));
            }
            row.provider = transport_provider.to_string();
            let offering = current_local_context(provider, row).to_offering();
            route_offerings.insert(offering_key(&offering), offering);
        }
        // The roster replaced its whole scope, but an id it explicitly attests
        // is unlisted is not an id it denied. That signed row is executable
        // beside the roster, carrying only the facts the payload stated —
        // otherwise the picker would offer a model the executor cannot resolve
        // with the same metadata.
        for row in cloud_unlisted_offerings_for_route(
            provider,
            provider_identity.unwrap_or(provider.as_str()),
            base_url,
        )
        .into_values()
        {
            let offering = current_local_context(provider, row).to_offering();
            route_offerings
                .entry(offering_key(&offering))
                .or_insert(offering);
        }
    }

    let resolver = RouteResolver::from_offerings(route_offerings.into_values().collect());
    if provider != ProviderKind::Ollama
        && let Ok(mut cache) = RUNTIME_RESOLVER_CACHE.write()
    {
        cache.retain(|_, entry| {
            entry.generation == generation && entry.cloud_generation == cloud_generation
        });
        if cache.len() >= MAX_RUNTIME_RESOLVER_CACHE_ENTRIES && !cache.contains_key(&cache_key) {
            cache.pop_first();
        }
        cache.insert(
            cache_key,
            RuntimeResolverCacheEntry {
                generation,
                cloud_generation,
                status_is_fresh,
                endpoint_catalog_authoritative,
                resolver: resolver.clone(),
            },
        );
    }
    RuntimeCatalogResolver {
        resolver,
        endpoint_catalog_authoritative,
    }
}

fn offerings_for_provider_identity<'a>(
    snapshot: &'a SharedSnapshot,
    provider_id: &str,
) -> Vec<&'a CatalogOffering> {
    snapshot.offerings_for_provider(&catalog_partition_key(provider_id))
}

fn exact_custom_offerings(provider_identity: &str) -> Vec<CatalogOffering> {
    let provider_identity = provider_identity.trim();
    if provider_identity.is_empty() {
        return Vec::new();
    }
    let owner = LivePartitionOwner::Custom(catalog_partition_key(provider_identity));
    LIVE_SNAPSHOT
        .read()
        .ok()
        .and_then(|live| live.per_provider.get(&owner).cloned())
        .map(|snapshot| snapshot.offerings)
        .unwrap_or_default()
}

/// Wire model ids are opaque and case-sensitive (`org/Model-A` and
/// `org/model-a` may be two models), so dedup compares them exactly; folding
/// case here would hide a real row. Only lookup forgives a case slip, and only
/// when it is unambiguous (see `find_wire_model`).
fn push_unique_model(models: &mut Vec<String>, model: &str) {
    let model = model.trim();
    if model.is_empty() {
        return;
    }
    if !models.iter().any(|existing| existing == model) {
        models.push(model.to_string());
    }
}

fn catalog_models_from_offerings<'a>(
    offerings: impl IntoIterator<Item = &'a CatalogOffering>,
) -> Vec<String> {
    let mut rows: Vec<_> = offerings.into_iter().collect();
    rows.sort_by(|left, right| {
        right
            .default_for_provider
            .cmp(&left.default_for_provider)
            .then_with(|| left.wire_model_id.cmp(&right.wire_model_id))
    });
    let mut models = Vec::new();
    for row in rows {
        push_unique_model(&mut models, &row.wire_model_id);
    }
    models
}

/// Tags from the provider's own live `/v1/models` partition.
///
/// Models.dev rows must not satisfy a LOCAL default (Ollama). This reads only
/// the PerProvider snapshot so a cross-provider catalog cannot costume a
/// machine that has not answered with its own tags.
#[cfg(test)]
#[must_use]
pub fn live_per_provider_models(provider: ProviderKind) -> Vec<String> {
    let catalog_id = catalog_provider_id(provider).to_ascii_lowercase();
    let Ok(guard) = LIVE_SNAPSHOT.read() else {
        return Vec::new();
    };
    let owner = LivePartitionOwner::BuiltIn(catalog_id);
    let Some(snapshot) = guard.per_provider.get(&owner) else {
        return Vec::new();
    };
    catalog_models_from_offerings(&snapshot.offerings)
}

/// Catalog-backed model ids for one provider (#4188).
///
/// Precedence: live Models.dev rows (when published) override bundled offline
/// rows on `(provider, wire_model_id)`; if the merged catalog still has no rows
/// for the provider, fall back to
/// `codewhale_runtime::config::model_completion_names_for_provider` so CodeWhale-only /
/// local providers (and gateways not yet in the offline seed) keep defaults.
#[must_use]
pub fn all_catalog_models_for_provider(provider: ProviderKind) -> Vec<String> {
    all_catalog_models_for_provider_identity(provider, None)
}

/// Catalog-backed model ids for one exact provider route.
///
/// Built-in providers retain their canonical ids. Named custom routes use
/// `provider_identity`, so one host's live `/v1/models` rows remain isolated
/// from every other custom host. There are no compiled seed models: a custom
/// route with no live, bundled, or configured rows offers nothing (#6289).
#[must_use]
pub fn all_catalog_models_for_provider_identity(
    provider: ProviderKind,
    provider_identity: Option<&str>,
) -> Vec<String> {
    // ChatGPT OAuth availability is account-scoped. A generic OpenAI or
    // Models.dev catalog is not evidence that a model can be routed through
    // a ChatGPT plan, so this provider owns a registration-scoped source.
    if provider == ProviderKind::OpenaiCodex {
        return codewhale_runtime::codex_model_roster::model_roster().model_ids();
    }

    let catalog_id = catalog_provider_id_for_identity(provider, provider_identity);
    let custom_offerings =
        (provider == ProviderKind::Custom).then(|| exact_custom_offerings(catalog_id.as_ref()));
    let merged = merged_snapshot();
    let mut models = match custom_offerings.as_ref() {
        Some(rows) => catalog_models_from_offerings(rows.iter()),
        None => catalog_models_from_offerings(offerings_for_provider_identity(
            &merged,
            catalog_id.as_ref(),
        )),
    };
    if models.is_empty() {
        for model in model_completion_names_for_provider(provider) {
            push_unique_model(&mut models, model);
        }
    }
    models
}

/// Look up a merged-catalog offering for `(provider, wire_model_id)` (#4115).
///
/// Returns the live-over-bundled row when present so picker metadata (context,
/// pricing, tools, reasoning, freshness) can be projected without a second
/// catalog walk. `None` for CodeWhale-only / legacy-fallback ids that have no
/// Models.dev row.
#[must_use]
pub fn catalog_offering_for_model(
    provider: ProviderKind,
    wire_model_id: &str,
) -> Option<CatalogOffering> {
    catalog_offering_for_model_identity(provider, None, wire_model_id)
}

/// Look up a merged-catalog offering for one exact provider route.
#[must_use]
pub fn catalog_offering_for_model_identity(
    provider: ProviderKind,
    provider_identity: Option<&str>,
    wire_model_id: &str,
) -> Option<CatalogOffering> {
    if provider == ProviderKind::OpenaiCodex {
        return None;
    }
    let catalog_id = catalog_provider_id_for_identity(provider, provider_identity);
    let needle = wire_model_id.trim();
    if needle.is_empty() {
        return None;
    }
    if provider == ProviderKind::Custom {
        let rows = exact_custom_offerings(catalog_id.as_ref());
        return find_wire_model(&rows, needle).cloned();
    }
    find_wire_model(
        offerings_for_provider_identity(&merged_snapshot(), catalog_id.as_ref()),
        needle,
    )
    .cloned()
    .map(|row| current_local_context(provider, row))
}

/// What one exact-route lookup found in the provider's own catalog entry.
enum RouteCatalogLookup {
    /// The roster lists this id; the row is the route's own metadata.
    Listed(Box<CatalogOffering>),
    /// A successful fetch answered and does not list this id, so nothing else
    /// may supply facts for it.
    NotListed,
    /// No usable roster for this route (absent, or never fetched).
    NoRoster,
}

/// Metadata from the exact route, without borrowing another endpoint's live facts.
pub(crate) fn catalog_offering_for_route(
    provider: ProviderKind,
    identity: &str,
    base_url: &str,
    model: &str,
) -> Option<CatalogOffering> {
    // Only the matched row is cloned, and the cache lock is released here
    // instead of spanning the lower-layer completion below.
    let lookup = crate::provider_catalog_live::with_cached_entry_for_route(
        provider,
        identity,
        base_url,
        |entry| {
            if entry.fetched_at == 0 {
                return RouteCatalogLookup::NoRoster;
            }
            match entry
                .offerings
                .iter()
                .find(|row| row.wire_model_id == model)
            {
                Some(row) => RouteCatalogLookup::Listed(Box::new(row.clone())),
                None => RouteCatalogLookup::NotListed,
            }
        },
    );
    match lookup {
        Ok(Some(RouteCatalogLookup::Listed(mut row))) => {
            if !matches!(provider, ProviderKind::Custom | ProviderKind::OpenaiCodex) {
                let cloud = codewhale_config::cloud_facts::overlay::snapshot();
                let route_cloud =
                    cloud_facts_apply_to_route(provider, identity, base_url).then_some(&cloud);
                let catalog_id = catalog_provider_id_for_identity(provider, Some(identity));
                let live_generation = LIVE_GENERATION.load(Ordering::SeqCst);
                let lower_rows = if let Ok(live) = LIVE_SNAPSHOT.read() {
                    source_rows_for_provider(
                        catalog_id.as_ref(),
                        live_generation,
                        live.models_dev.as_ref(),
                        route_cloud,
                    )
                } else {
                    source_rows_for_provider(
                        catalog_id.as_ref(),
                        live_generation,
                        None,
                        route_cloud,
                    )
                };
                if let Some(lower) = lower_rows.get(&(row.provider.clone(), model.to_string())) {
                    row.complete_from(lower, Some(base_url));
                }
            }
            return Some(current_local_context(provider, *row));
        }
        // A successful fetch that does not list this id is authoritative about
        // its absence. Only an explicitly attested unlisted row may still name
        // it here: falling through to the bundled/Models.dev merge would hand
        // back facts for a model the roster has retired.
        Ok(Some(RouteCatalogLookup::NotListed)) => {
            return cloud_unlisted_offerings_for_route(provider, identity, base_url)
                .into_values()
                .find(|row| row.wire_model_id == model)
                .map(|row| current_local_context(provider, row));
        }
        Ok(Some(RouteCatalogLookup::NoRoster)) | Ok(None) | Err(_) => {}
    }
    if codewhale_config::provider_preserves_custom_base_url_model(provider, base_url) {
        return None;
    }
    let offering = catalog_offering_for_model_identity(provider, Some(identity), model)?;
    if matches!(offering.source, CatalogSource::CloudFacts { .. })
        && !cloud_facts_apply_to_route(provider, identity, base_url)
    {
        return bundled_catalog_offering_for_model(provider, model);
    }
    if matches!(offering.source, CatalogSource::Live { .. })
        && !row_matches_endpoint_fingerprint(&offering, &base_url_fingerprint(base_url))
    {
        return None;
    }
    Some(current_local_context(provider, offering))
}

/// Config declarations take precedence only for the exact selected route.
pub(crate) fn configured_catalog_offering_for_route(
    config: &Config,
    provider: ProviderKind,
    identity: &str,
    base_url: &str,
    model: &str,
) -> Option<CatalogOffering> {
    let lower = catalog_offering_for_route(provider, identity, base_url, model);
    codewhale_runtime::config::configured_model_for_route(
        config, provider, identity, base_url, model,
    )
    .map(|row| {
        let mut offering = row.to_catalog_offering();
        if let Some(lower) = lower.as_ref() {
            offering.complete_from(lower, Some(base_url));
        }
        offering
    })
    .or(lower)
}

pub(crate) fn configured_catalog_models_for_route(
    config: &Config,
    provider: ProviderKind,
    identity: &str,
    base_url: &str,
) -> Vec<String> {
    if provider == ProviderKind::OpenaiCodex {
        return codex_model_cache::model_roster_for(config).model_ids();
    }
    let mut ids = catalog_models_for_route(provider, identity, base_url);
    if provider != ProviderKind::OpenaiCodex {
        let models = config.custom_models.as_deref().unwrap_or_default();
        if codewhale_config::catalog::configured::validate_configured_models(models).is_ok() {
            for model in models
                .iter()
                .filter(|row| row.matches_route(identity, base_url))
            {
                if !ids.contains(&model.id) {
                    ids.push(model.id.clone());
                }
            }
        }
    }
    ids
}

/// Look up the **bundled-snapshot** offering for `(provider, wire_model_id)`,
/// ignoring any live rows merged over it.
///
/// Pricing uses this as an honest fallback when a live row cannot be verified as
/// authoritative for the endpoint being priced (stale fetch, or a fetch from a
/// different base URL). The bundled snapshot is a published Models.dev seed with
/// no endpoint scoping, so it is authoritative for the model without needing a
/// freshness proof — degrading to it is strictly more truthful than billing
/// against an unverified live rate (#4318).
#[must_use]
pub fn bundled_catalog_offering_for_model(
    provider: ProviderKind,
    wire_model_id: &str,
) -> Option<CatalogOffering> {
    if provider == ProviderKind::OpenaiCodex {
        return None;
    }
    let catalog_id = catalog_provider_id(provider);
    let needle = wire_model_id.trim();
    if needle.is_empty() {
        return None;
    }
    find_wire_model(
        bundled_snapshot().offerings_for_provider(catalog_id),
        needle,
    )
    .cloned()
}

/// Count of merged-catalog models for one provider (catalog view / dashboard).
#[must_use]
pub fn catalog_model_count_for_provider(provider: ProviderKind) -> usize {
    all_catalog_models_for_provider(provider).len()
}

/// Providers the user has set up — active provider, working credentials/OAuth,
/// or an explicit `[providers.<name>]` entry (#3830).
#[must_use]
pub fn configured_providers(config: &Config) -> Vec<ProviderIdentity> {
    let Ok(active) = config.active_provider_identity() else {
        return Vec::new();
    };
    config
        .provider_identities()
        .into_iter()
        .filter(|identity| provider_is_configured_for_active(config, identity, &active))
        .collect()
}

/// Catalog models for providers that qualify as configured for `active`.
#[must_use]
pub fn models_for_provider(config: &Config, identity: &ProviderIdentity) -> Vec<String> {
    let Ok(active) = config.active_provider_identity() else {
        return Vec::new();
    };
    if provider_is_configured_for_active(config, identity, &active) {
        configured_catalog_models_for_route(
            config,
            identity.provider,
            identity.key.as_str(),
            &config.base_url_for_route(identity),
        )
    } else {
        Vec::new()
    }
}

/// Endpoint-scoped roster for CLI, pickers and inventory. A cached provider
/// listing is authoritative for the IDs it lists **and for its own omissions**;
/// the only ID appended after it is one the signed payload explicitly attests
/// as unlisted. A stale listing for an official endpoint is not current
/// evidence, so it cannot shadow the snapshot roster; its own IDs survive only
/// as an offline tail. Configured models are retained by the caller.
#[must_use]
pub(crate) fn catalog_models_for_route(
    provider: ProviderKind,
    identity: &str,
    base_url: &str,
) -> Vec<String> {
    if provider == ProviderKind::OpenaiCodex {
        return codewhale_runtime::codex_model_roster::model_roster().model_ids();
    }
    let cached = crate::provider_catalog_live::cached_entry_for_route(provider, identity, base_url)
        .ok()
        .flatten()
        .filter(|entry| entry.fetched_at > 0);
    let cached_is_authoritative = cached.as_ref().is_some_and(|entry| {
        crate::provider_catalog_live::cached_roster_is_authoritative(provider, base_url, entry)
    });
    if let Some(entry) = cached.as_ref().filter(|_| cached_is_authoritative) {
        let mut models = Vec::with_capacity(entry.offerings.len());
        for row in &entry.offerings {
            push_unique_model(&mut models, &row.wire_model_id);
        }
        // Appended, never interleaved: the roster keeps its own order and its
        // own authority, and an explicitly attested unlisted id is offered
        // after it. This is the same list the picker, metadata lookups and the
        // route resolver read, so a user pin stays exactly what it was.
        for row in cloud_unlisted_offerings_for_route(provider, identity, base_url).values() {
            push_unique_model(&mut models, &row.wire_model_id);
        }
        return models;
    }
    if provider == ProviderKind::Custom {
        // No compiled seeds: without a cached listing the caller retains the
        // configured model and the live refresh fills the roster (#6289).
        return Vec::new();
    }
    if codewhale_config::provider_preserves_custom_base_url_model(provider, base_url) {
        return Vec::new();
    }
    // Do not borrow a live partition published for another endpoint.
    let catalog_id = catalog_provider_id(provider);
    let live_generation = LIVE_GENERATION.load(Ordering::SeqCst);
    let live = LIVE_SNAPSHOT.read().ok();
    let cloud = codewhale_config::cloud_facts::overlay::snapshot();
    let rows = source_rows_for_provider(
        catalog_id,
        live_generation,
        live.as_ref().and_then(|live| live.models_dev.as_ref()),
        cloud_facts_apply_to_route(provider, identity, base_url).then_some(&cloud),
    );
    let mut models = catalog_models_from_offerings(rows.values());
    if models.is_empty() && cloud.facts.is_none() {
        models.extend(
            model_completion_names_for_provider(provider)
                .into_iter()
                .map(str::to_string),
        );
    }
    for row in cached.iter().flat_map(|entry| entry.offerings.iter()) {
        push_unique_model(&mut models, &row.wire_model_id);
    }
    models
}

#[derive(serde::Serialize)]
pub(crate) struct CatalogUpdateReceipt {
    provider: String,
    source: &'static str,
    outcome: &'static str,
    status: CatalogStatus,
    fetched_at: Option<u64>,
    observed_at: Option<u64>,
    base_url_fingerprint: Option<String>,
    model_count: usize,
    error: Option<&'static str>,
}

fn cached_receipt(config: &Config, identity: &ProviderIdentity) -> CatalogUpdateReceipt {
    let base_url = config.base_url_for_route(identity);
    let fingerprint = base_url_fingerprint(&base_url);
    let entry = crate::provider_catalog_live::cached_entry_for_route(
        identity.provider,
        identity.key.as_str(),
        &base_url,
    );
    let cached = entry.as_ref().ok().and_then(Option::as_ref);
    CatalogUpdateReceipt {
        provider: identity.key.to_string(),
        source: "provider_models",
        outcome: "cached",
        status: crate::provider_catalog_live::status_for_route(
            identity.provider,
            identity.key.as_str(),
            &base_url,
        ),
        fetched_at: cached
            .map(|entry| entry.fetched_at)
            .filter(|timestamp| *timestamp > 0),
        observed_at: None,
        base_url_fingerprint: Some(fingerprint),
        model_count: cached.map_or(0, |entry| entry.offerings.len()),
        error: entry.is_err().then_some("cache_read_failed"),
    }
}

fn roster_receipt(
    config: &Config,
    identity: &ProviderIdentity,
    model_count: usize,
) -> CatalogUpdateReceipt {
    let base_url = config.base_url_for_route(identity);
    let live = crate::provider_catalog_live::cached_entry_for_route(
        identity.provider,
        identity.key.as_str(),
        &base_url,
    )
    .ok()
    .flatten()
    .is_some_and(|entry| {
        entry.fetched_at > 0
            && crate::provider_catalog_live::cached_roster_is_authoritative(
                identity.provider,
                &base_url,
                &entry,
            )
    });
    if live {
        return cached_receipt(config, identity);
    }
    let snapshot = crate::models_dev_live::status();
    let bundled_fetched_at = codewhale_config::catalog::reviewed::bundled_source_fetched_at();
    let (status, fetched_at) = match snapshot.freshness {
        crate::models_dev_live::ModelsDevFreshness::Live => (
            CatalogStatus::Fresh,
            snapshot.fetched_at.or(bundled_fetched_at),
        ),
        crate::models_dev_live::ModelsDevFreshness::Bundled => {
            (CatalogStatus::Fresh, bundled_fetched_at)
        }
        crate::models_dev_live::ModelsDevFreshness::Stale
        | crate::models_dev_live::ModelsDevFreshness::Failed => {
            let fetched_at = snapshot.fetched_at.or(bundled_fetched_at);
            let now = codewhale_config::catalog::now_unix();
            let status =
                fetched_at.map_or(CatalogStatus::Unknown, |fetched_at| CatalogStatus::Stale {
                    age_secs: now.saturating_sub(fetched_at),
                });
            (status, fetched_at)
        }
    };
    CatalogUpdateReceipt {
        provider: identity.key.to_string(),
        source: snapshot.provenance.label(),
        outcome: "cached",
        status,
        fetched_at,
        observed_at: None,
        base_url_fingerprint: Some(base_url_fingerprint(&base_url)),
        model_count,
        error: None,
    }
}

fn catalog_identities(
    config: &Config,
    selected: Option<&str>,
    update: bool,
) -> anyhow::Result<Vec<ProviderIdentity>> {
    if let Some(selected) = selected {
        return Ok(vec![
            config
                .resolve_provider_pin_identity(selected)
                .map_err(anyhow::Error::msg)?,
        ]);
    }
    let active = config
        .active_provider_identity()
        .map_err(anyhow::Error::msg)?;
    if !update {
        return Ok(vec![active]);
    }
    let mut identities = configured_providers(config);
    if !identities.contains(&active) {
        identities.push(active);
    }
    identities.sort_by(|a, b| a.key.as_str().cmp(b.key.as_str()));
    identities.dedup_by(|a, b| a == b);
    Ok(identities)
}

fn codex_receipt(config: &Config, identity: &ProviderIdentity) -> CatalogUpdateReceipt {
    let roster = codex_model_cache::model_roster_for(config);
    codex_roster_receipt(identity, &roster)
}

fn codex_roster_receipt(
    identity: &ProviderIdentity,
    roster: &codewhale_runtime::codex_model_roster::CodexModelRoster,
) -> CatalogUpdateReceipt {
    let fresh =
        roster.freshness == codewhale_runtime::codex_model_roster::CodexModelCacheFreshness::Fresh;
    CatalogUpdateReceipt {
        provider: identity.key.to_string(),
        source: roster.source,
        outcome: "cached",
        status: if fresh {
            CatalogStatus::Fresh
        } else {
            CatalogStatus::Unknown
        },
        fetched_at: roster
            .fetched_at
            .and_then(|timestamp| u64::try_from(timestamp.timestamp()).ok()),
        observed_at: roster
            .observed_at
            .and_then(|timestamp| u64::try_from(timestamp.timestamp()).ok()),
        base_url_fingerprint: None,
        model_count: roster.models.len(),
        error: if !fresh {
            Some("chatgpt_catalog_unavailable")
        } else if !roster.observation_persisted {
            Some("chatgpt_catalog_not_persisted")
        } else {
            None
        },
    }
}

/// Whether a provider-scoped live listing can never exist for this route.
/// OAuth-authenticated routes (other than Codex, which owns its own roster)
/// have no catalog endpoint Codewhale may call, so listings fall back to the
/// next catalog layer — live Models.dev, then the bundled snapshot — instead
/// of treating the missing live listing as "no models".
pub(crate) fn live_catalog_unavailable(config: &Config, identity: &ProviderIdentity) -> bool {
    config.verify_provider_identity(identity).is_ok()
        && identity.provider != ProviderKind::OpenaiCodex
        && config
            .auth_mode_for_provider(identity)
            .is_some_and(|mode| mode.eq_ignore_ascii_case("oauth"))
}

pub(crate) async fn update_provider_catalog(
    config: &Config,
    identity: &ProviderIdentity,
) -> CatalogUpdateReceipt {
    if config.verify_provider_identity(identity).is_err() {
        let mut receipt = cached_receipt(config, identity);
        receipt.outcome = "skipped";
        receipt.error = Some("provider_identity_changed");
        return receipt;
    }
    if identity.provider == ProviderKind::OpenaiCodex {
        let mut route_config = config.clone();
        if route_config.scope_to_provider_identity(identity).is_err() {
            let mut receipt = cached_receipt(config, identity);
            receipt.outcome = "skipped";
            receipt.error = Some("provider_identity_changed");
            return receipt;
        }
        return match codex_model_cache::update_from_chatgpt(&route_config).await {
            Ok(roster) => {
                let mut receipt = codex_roster_receipt(identity, &roster);
                receipt.outcome = "updated";
                receipt
            }
            Err(error) => {
                let mut receipt = codex_receipt(&route_config, identity);
                receipt.outcome = "failed";
                receipt.error = Some(error);
                receipt
            }
        };
    }
    let mut route_config = config.clone();
    if route_config.scope_to_provider_identity(identity).is_err() {
        let mut receipt = cached_receipt(config, identity);
        receipt.outcome = "skipped";
        receipt.error = Some("provider_identity_changed");
        return receipt;
    }
    let base_url = route_config.active_route_base_url();
    let fingerprint = base_url_fingerprint(&base_url);
    let mut receipt = cached_receipt(&route_config, identity);
    if identity.provider == ProviderKind::Antigravity {
        receipt.outcome = "skipped";
        receipt.error = Some("provider_retired");
        return receipt;
    }
    if codewhale_runtime::config::explicit_cli_api_key_override().is_some()
        && config.active_provider_identity().ok().as_ref() != Some(identity)
    {
        receipt.outcome = "skipped";
        receipt.error = Some("cli_key_is_scoped_to_active_provider");
        return receipt;
    }
    if live_catalog_unavailable(&route_config, identity) {
        receipt.outcome = "skipped";
        receipt.error = Some("oauth_catalog_unavailable");
        return receipt;
    }
    // Ordinary model listing never constructs a client. Explicit refresh uses
    // the existing read-only resolver: no secret migration or OAuth refresh.
    let account_owner = route_config.account_model_access.read().clone();
    let prepared = route_config.with_read_only_api_key_for_diagnostic();
    let credential = prepared
        .as_ref()
        .ok()
        .and_then(|config| config.active_route_api_key_read_only().ok());
    let client =
        prepared.and_then(|config| crate::client::CodewhaleClient::for_catalog_refresh(&config));
    let client = match client {
        Ok(client) => client,
        Err(_) => {
            receipt.outcome = "skipped";
            receipt.error = Some("credentials_or_route_unavailable");
            return receipt;
        }
    };
    if receipt.error.is_some() {
        receipt.outcome = "failed";
        return receipt;
    }
    let ticket = crate::provider_catalog_live::begin_refresh_for_identity(
        identity.provider,
        identity.key.as_str(),
        &base_url,
    );
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        client.fetch_catalog_delta(),
    )
    .await
    .unwrap_or(Err(codewhale_config::catalog::CatalogRefreshError::Network));
    // Resolve from the original route, not the materialized client clone: the
    // shared session or secure credential may have changed during the request.
    if route_config.active_route_api_key_read_only().ok() != credential {
        receipt.outcome = "skipped";
        receipt.error = Some("refresh_credentials_changed");
        return receipt;
    }
    // Serialize publication with explicit overlay install/remove. Resolve
    // above before taking this guard: the resolver itself reads the overlay.
    let access = route_config.account_model_access.read();
    let owner = |access: &codewhale_runtime::config::AccountModelAccess| {
        (
            access.session_id.clone(),
            access.profile.clone(),
            access.credential.expose_secret().to_string(),
        )
    };
    if access.as_ref().map(owner) != account_owner.as_ref().map(owner) {
        receipt.outcome = "skipped";
        receipt.error = Some("refresh_credentials_changed");
        return receipt;
    }
    match result {
        Ok(mut delta) => {
            if delta.base_url_fingerprint != fingerprint {
                receipt.outcome = "failed";
                receipt.error = Some("catalog_endpoint_mismatch");
                return receipt;
            }
            delta.provider = identity.key.to_string();
            match crate::provider_catalog_live::record_success_if_current(&ticket, delta) {
                None => {
                    receipt.outcome = "skipped";
                    receipt.error = Some("refresh_superseded");
                    return receipt;
                }
                Some(CatalogStatus::Fresh) => receipt.outcome = "updated",
                Some(CatalogStatus::Failed { reason }) => {
                    receipt.outcome = "failed";
                    receipt.error = Some(reason.label());
                }
                Some(_) => {
                    receipt.outcome = "failed";
                    receipt.error = Some("cache_write_failed");
                }
            }
        }
        Err(reason) => {
            crate::provider_catalog_live::record_failure_if_current(
                &ticket,
                identity.key.as_str(),
                &fingerprint,
                reason,
            );
            receipt.outcome = "failed";
            receipt.error = Some(reason.label());
        }
    }
    drop(access);
    let outcome = receipt.outcome;
    let failure = receipt.error;
    receipt = cached_receipt(&route_config, identity);
    receipt.outcome = outcome;
    if outcome == "failed" {
        receipt.error = failure.or(receipt.error);
    }
    receipt
}

pub(crate) async fn refresh_local_ollama_catalog(config: &Config, identity: &ProviderIdentity) {
    #[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
    let _ = (config, identity);
    #[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
    {
        if identity.provider != ProviderKind::Ollama {
            return;
        }
        static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let queued_at = codewhale_config::catalog::now_unix();
        let refresh_guard = REFRESH_LOCK.lock().await;
        let handle = tokio::runtime::Handle::current();
        let config = config.clone();
        let identity = identity.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _refresh_guard = refresh_guard;
            let now = codewhale_config::catalog::now_unix();
            let base_url = config.base_url_for_route(&identity);
            let recent = crate::provider_catalog_live::with_cached_entry_for_route(
                identity.provider,
                identity.key.as_str(),
                &base_url,
                |entry| {
                    entry.is_fresh(now)
                        && matches!(
                            entry.status,
                            codewhale_config::catalog::CatalogStatus::Fresh
                        )
                        && entry.fetched_at >= queued_at
                        && entry.fetched_at <= now
                        && entry.offerings.iter().any(|row| row.default_for_provider)
                },
            )
            .ok()
            .flatten()
            .unwrap_or(false);
            if recent
                && runtime_catalog_resolver_for_identity(
                    identity.provider,
                    Some(identity.key.as_str()),
                    &base_url,
                    codewhale_config::catalog::CatalogStatus::Fresh,
                )
                .endpoint_catalog_authoritative
            {
                return;
            }
            handle.block_on(update_provider_catalog(&config, &identity));
        })
        .await;
    }
}

pub(crate) async fn refresh_unverified_local_context(
    config: &Config,
    identity: &ProviderIdentity,
    model: &str,
) {
    if identity.provider != ProviderKind::Ollama {
        return;
    }
    let provider = identity.provider;
    let key = identity.key.as_str().to_string();
    let base_url = config.base_url_for_route(identity);
    let model = model.to_string();
    let known = tokio::task::spawn_blocking(move || {
        catalog_offering_for_route(provider, &key, &base_url, &model)
            .and_then(|row| row.limit)
            .and_then(|limit| limit.context)
            .is_some()
    })
    .await
    .unwrap_or(false);
    if !known {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            refresh_local_ollama_catalog(config, identity),
        )
        .await;
    }
}

pub(crate) async fn refresh_account_catalog_before_first_request(config: &Config) {
    let Ok(identity) = config.active_provider_identity() else {
        return;
    };
    if !crate::provider_catalog_live::route_is_account_scoped(config, &identity)
        || live_catalog_unavailable(config, &identity)
    {
        return;
    }
    let handle = tokio::runtime::Handle::current();
    let refresh_config = config.clone();
    let refresh_identity = identity.clone();
    let receipt = tokio::task::spawn_blocking(move || {
        handle.block_on(update_provider_catalog(&refresh_config, &refresh_identity))
    })
    .await
    .ok();
    let status = crate::provider_catalog_live::status_for_route(
        identity.provider,
        identity.key.as_str(),
        &config.active_route_base_url(),
    );
    if !matches!(status, CatalogStatus::Fresh) {
        let reason = match &status {
            CatalogStatus::Failed { reason } => reason.label(),
            _ => receipt.map_or("refresh_failed", |receipt| {
                receipt.error.unwrap_or(receipt.outcome)
            }),
        };
        tracing::warn!(
            target: "provider_catalog",
            provider = %identity.key,
            reason,
            "account model listing unavailable before the first request"
        );
        eprintln!(
            "codewhale: could not read the {} account model listing ({reason}); requesting with built-in model limits.",
            identity.key
        );
    }
}

pub(crate) async fn run_models(
    config: &Config,
    update: bool,
    selected: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    use codewhale_localization::{MessageId, resolve_locale, tr};
    let locale = resolve_locale(
        &codewhale_runtime::settings::Settings::load_persisted()
            .unwrap_or_default()
            .locale,
    );
    let identities = catalog_identities(config, selected, update)?;
    crate::models_dev_live::maybe_load_persisted_cache();
    if update {
        let mut receipts = Vec::new();
        if selected.is_none() {
            let result = crate::models_dev_live::refresh(true).await;
            let status = crate::models_dev_live::status();
            receipts.push(CatalogUpdateReceipt {
                provider: "models.dev".to_string(),
                source: "models.dev",
                outcome: if result.is_ok() { "updated" } else { "failed" },
                status: if result.is_ok() {
                    CatalogStatus::Fresh
                } else {
                    CatalogStatus::Unknown
                },
                fetched_at: status.fetched_at,
                observed_at: None,
                base_url_fingerprint: None,
                model_count: status.offering_count,
                error: result.err().map(|error| match error {
                    crate::models_dev_live::ModelsDevRefreshError::Disabled => "fetch_disabled",
                    crate::models_dev_live::ModelsDevRefreshError::Network(_) => "network",
                    crate::models_dev_live::ModelsDevRefreshError::HttpStatus(_) => "http_status",
                    crate::models_dev_live::ModelsDevRefreshError::InvalidResponse(_) => {
                        "invalid_response"
                    }
                    crate::models_dev_live::ModelsDevRefreshError::EmptyCatalog => "empty_catalog",
                    crate::models_dev_live::ModelsDevRefreshError::Io(_) => "cache_io",
                }),
            });
        }
        use futures_util::StreamExt;
        receipts.extend(
            futures_util::stream::iter(&identities)
                .map(|identity| update_provider_catalog(config, identity))
                .buffered(4)
                .collect::<Vec<_>>()
                .await,
        );
        let updated = receipts
            .iter()
            .filter(|receipt| receipt.outcome == "updated")
            .count();
        let loaded = receipts
            .iter()
            .filter(|receipt| receipt.outcome == "loaded")
            .count();
        let failed = receipts
            .iter()
            .filter(|receipt| receipt.outcome == "failed")
            .count();
        let skipped = receipts
            .iter()
            .filter(|receipt| receipt.outcome == "skipped")
            .count();
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "updated": updated, "loaded": loaded, "failed": failed, "skipped": skipped,
                    "catalogs": receipts,
                }))?
            );
        } else {
            println!(
                "{}",
                tr(locale, MessageId::ModelsUpdateSummary)
                    .replace("{updated}", &updated.to_string())
                    .replace("{loaded}", &loaded.to_string())
                    .replace("{failed}", &failed.to_string())
                    .replace("{skipped}", &skipped.to_string())
            );
            for receipt in &receipts {
                println!(
                    "{}\t{}\tmodels={}\tfetched_at={}\tobserved_at={}\tsource={}\tstatus={}{}",
                    receipt.provider,
                    receipt.outcome,
                    receipt.model_count,
                    receipt
                        .fetched_at
                        .map_or_else(|| "unknown".to_string(), |timestamp| timestamp.to_string()),
                    receipt
                        .observed_at
                        .map_or_else(|| "unknown".to_string(), |timestamp| timestamp.to_string()),
                    receipt.source,
                    serde_json::to_string(&receipt.status)?,
                    receipt
                        .error
                        .map_or_else(String::new, |error| format!("\terror={error}"))
                );
            }
        }
        if failed > 0 {
            anyhow::bail!("{}", tr(locale, MessageId::ModelsUpdatePartial));
        }
        return Ok(());
    }
    let identity = &identities[0];
    let mut route_config = config.clone();
    route_config
        .scope_to_provider_identity(identity)
        .map_err(anyhow::Error::msg)?;
    let mut models = configured_catalog_models_for_route(
        &route_config,
        identity.provider,
        identity.key.as_str(),
        &route_config.active_route_base_url(),
    );
    let default_model = route_config.default_model();
    if identity.provider != ProviderKind::OpenaiCodex
        && !default_model.is_empty()
        && !default_model.eq_ignore_ascii_case("auto")
    {
        push_unique_model(&mut models, &default_model);
    }
    if identity.provider != ProviderKind::OpenaiCodex {
        models.sort();
        models.dedup();
    }
    if json {
        // Preserve the existing array + AvailableModel field shape.
        let chatgpt_roster = (identity.provider == ProviderKind::OpenaiCodex)
            .then(|| codex_model_cache::model_roster_for(&route_config));
        let rows: Vec<_> = models
            .iter()
            .map(|id| crate::client::AvailableModel {
                id: id.clone(),
                display_name: chatgpt_roster
                    .as_ref()
                    .and_then(|roster| roster.metadata_for(id))
                    .and_then(|model| model.display_name.clone()),
                owned_by: None,
                created: None,
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        println!(
            "{}",
            tr(locale, MessageId::ModelsListHeader)
                .replace("{provider}", identity.key.as_str())
                .replace("{model}", &default_model)
        );
        let receipt = if identity.provider == ProviderKind::OpenaiCodex {
            codex_receipt(&route_config, identity)
        } else {
            roster_receipt(&route_config, identity, models.len())
        };
        println!(
            "source={}\tstatus={}\tfetched_at={}\tobserved_at={}",
            receipt.source,
            serde_json::to_string(&receipt.status)?,
            receipt
                .fetched_at
                .map_or_else(|| "unknown".to_string(), |timestamp| timestamp.to_string()),
            receipt
                .observed_at
                .map_or_else(|| "unknown".to_string(), |timestamp| timestamp.to_string())
        );
        if receipt.model_count == 0 {
            println!("{}", tr(locale, MessageId::ModelsSourceFallback));
        }
        for model in models {
            println!("{} {model}", if model == default_model { "*" } else { " " });
        }
        println!("{}", tr(locale, MessageId::ModelsListHint));
    }
    Ok(())
}

// ============================================================================
// Provider Capability Matrix
// ============================================================================

/// Known capabilities for a provider + resolved-model combination.
///
/// Returned by [`provider_capability`] to describe what a given provider
/// supports for the resolved model string.  All fields are derived from
/// static knowledge (release docs, API guides) rather than live API probes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ProviderCapability {
    /// Canonical provider identifier.
    pub provider: ProviderKind,
    /// Resolved model identifier that will be sent in the API payload.
    pub resolved_model: String,
    /// Context window in tokens (the maximum input the model can accept).
    pub context_window: u32,
    /// Known output ceiling for this provider/model metadata path, when one is
    /// actually known.
    ///
    /// `None` means "this route publishes no output maximum we can stand
    /// behind" — for example the Kimi Code membership ids, whose limits live in
    /// the membership catalog rather than the static model catalogue. Unknown
    /// must stay unknown: callers may **not** substitute a placeholder ceiling,
    /// and in particular [`crate::route_budget`] does not clamp a requested
    /// `max_tokens` against an unknown compatibility cap.
    ///
    /// When `Some`, the value is a documented exact-route maximum or a
    /// deliberately conservative provider ceiling (Anthropic's 64K floor, the
    /// Codex OAuth route). It is metadata for diagnostics and CI policy; normal
    /// turns use a separate, more conservative request cap in the engine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u32>,
    /// Whether the provider+model supports thinking/reasoning mode.
    pub thinking_supported: bool,
    /// Whether the provider returns prompt-cache telemetry fields.
    pub cache_telemetry_supported: bool,
    /// Which request-payload dialect the provider uses.
    pub request_payload_mode: RequestPayloadMode,
    /// Deprecation metadata for compatibility aliases that are still accepted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias_deprecation: Option<codewhale_runtime::config::ModelAliasDeprecation>,
}

/// Which request-payload dialect the provider speaks.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum RequestPayloadMode {
    /// Standard OpenAI-compatible `/v1/chat/completions` payload.
    ChatCompletions,
    /// OpenAI Responses API payload.
    Responses,
    /// Native Anthropic Messages API `/v1/messages` payload (#3014).
    AnthropicMessages,
}

/// Resolve the provider capability for a given [`ProviderKind`] and resolved
/// model string.
///
/// The `resolved_model` should be the final model identifier that will appear
/// in the API payload (after normalization / provider-specific mapping).
#[must_use]
pub fn provider_capability(provider: ProviderKind, resolved_model: &str) -> ProviderCapability {
    provider_capability_with_wire(provider, resolved_model, None)
}

/// Wire-aware variant of [`provider_capability`] that respects
/// `wire = "responses" | "anthropic" | "chat"` for `Custom` providers.
///
/// Built-ins keep their fixed policy; `Custom` defaults to `Chat` when `wire`
/// is absent so existing configs stay compatible. Mirrors
/// `crates/tui/src/client.rs::provider_wire_format_for_config` and the
/// `Custom` comment in `crates/config/src/provider.rs`.
#[must_use]
pub fn provider_capability_with_wire(
    provider: ProviderKind,
    resolved_model: &str,
    wire: Option<&str>,
) -> ProviderCapability {
    // Custom wire overrides must be checked before the generic fallback so
    // `[providers.<name>] wire = "responses"` / `"anthropic"` is honored.
    if provider == ProviderKind::Custom {
        if codewhale_config::provider::wire_dialect_prefers_anthropic(wire) {
            return ProviderCapability {
                provider,
                resolved_model: resolved_model.to_string(),
                context_window: codewhale_models::context_window_for_model(resolved_model)
                    .unwrap_or(codewhale_models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS),
                max_output: codewhale_models::max_output_tokens_for_model(resolved_model),
                thinking_supported: codewhale_models::model_supports_reasoning(resolved_model),
                cache_telemetry_supported: false,
                request_payload_mode: RequestPayloadMode::AnthropicMessages,
                alias_deprecation: None,
            };
        }
        if codewhale_config::provider::wire_dialect_prefers_responses(wire) {
            return ProviderCapability {
                provider,
                resolved_model: resolved_model.to_string(),
                context_window: codewhale_models::context_window_for_model(resolved_model)
                    .unwrap_or(codewhale_models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS),
                max_output: codewhale_models::max_output_tokens_for_model(resolved_model),
                thinking_supported: codewhale_models::model_supports_reasoning(resolved_model),
                cache_telemetry_supported: false,
                request_payload_mode: RequestPayloadMode::Responses,
                alias_deprecation: None,
            };
        }
    }

    if matches!(
        provider,
        ProviderKind::Anthropic | ProviderKind::MinimaxAnthropic | ProviderKind::Openmodel
    ) {
        return ProviderCapability {
            provider,
            resolved_model: resolved_model.to_string(),
            // 200K is the conservative Anthropic floor; 4.6+ models resolve
            // their 1M windows from models.rs rows (#3014).
            context_window: codewhale_models::context_window_for_model(resolved_model)
                .unwrap_or(200_000),
            // 64K is the documented Anthropic Messages floor. For a model
            // the catalogue describes this carries its documented ceiling;
            // for an unknown one it is an *assumed* floor, and
            // `route_budget::output_ceiling_source` labels it unverified so
            // no receipt renders it as "documented" (#5440).
            max_output: Some(
                codewhale_models::max_output_tokens_for_model(resolved_model).unwrap_or(64_000),
            ),
            thinking_supported: codewhale_models::model_supports_reasoning(resolved_model),
            cache_telemetry_supported: matches!(provider, ProviderKind::Anthropic),
            request_payload_mode: RequestPayloadMode::AnthropicMessages,
            alias_deprecation: None,
        };
    }

    if matches!(provider, ProviderKind::OpenaiCodex) {
        return ProviderCapability {
            provider,
            resolved_model: resolved_model.to_string(),
            context_window: codewhale_runtime::config::OPENAI_CODEX_EFFECTIVE_CONTEXT_WINDOW_TOKENS,
            // The OAuth cache does not publish an output ceiling. This 4K is a
            // deliberate, long-standing product decision for the Codex route
            // (not a fallback): keep the compatibility capability conservative
            // instead of inheriting the public API model's output limit. It is
            // an assumption, not a documented fact — receipts label it
            // unverified (#5440).
            max_output: Some(4096),
            thinking_supported: true,
            cache_telemetry_supported: false,
            request_payload_mode: RequestPayloadMode::Responses,
            alias_deprecation: None,
        };
    }

    // #3023: Delete the Openai/Atlascloud/Moonshot early-return so these
    // providers use the generic model-based path below, which correctly
    // resolves context windows, output limits, and thinking support from
    // models.rs lookups.  Ollama also falls through to model-based lookups
    // with 8192 as the last-resort fallback instead of a hardcoded floor.
    if matches!(provider, ProviderKind::XiaomiMimo) {
        return ProviderCapability {
            provider,
            resolved_model: resolved_model.to_string(),
            context_window: codewhale_models::context_window_for_model(resolved_model)
                .unwrap_or(codewhale_models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS),
            // No documented output maximum for these routes: stay unknown so
            // no compatibility clamp is applied downstream.
            max_output: codewhale_models::max_output_tokens_for_model(resolved_model),
            thinking_supported: codewhale_models::model_supports_reasoning(resolved_model),
            cache_telemetry_supported: false,
            request_payload_mode: RequestPayloadMode::ChatCompletions,
            alias_deprecation: None,
        };
    }

    if matches!(provider, ProviderKind::Arcee) {
        return ProviderCapability {
            provider,
            resolved_model: resolved_model.to_string(),
            context_window: codewhale_models::context_window_for_model(resolved_model)
                .unwrap_or(codewhale_models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS),
            // No documented output maximum for these routes: stay unknown so
            // no compatibility clamp is applied downstream.
            max_output: codewhale_models::max_output_tokens_for_model(resolved_model),
            thinking_supported: codewhale_models::model_supports_reasoning(resolved_model),
            cache_telemetry_supported: false,
            request_payload_mode: RequestPayloadMode::ChatCompletions,
            alias_deprecation: None,
        };
    }

    let model_lower = resolved_model.to_ascii_lowercase();
    let alias_deprecation = if matches!(
        provider,
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
    ) {
        codewhale_runtime::config::deepseek_alias_deprecation(&model_lower)
    } else {
        None
    };
    let official_deepseek = codewhale_models::canonical_official_deepseek_model_id(&model_lower)
        .is_some()
        || alias_deprecation.is_some();
    let is_reasoner = matches!(provider, ProviderKind::WanjieArk)
        && (model_lower.contains("reasoner") || model_lower.contains("r1"));

    // Provider-owned wire IDs can have exact catalog facts without a legacy
    // model-only row. Reuse those facts before conservative fallback budgets.
    let offering = bundled_catalog_offering_for_model(provider, resolved_model);
    let context_window = if provider == ProviderKind::Ollama {
        8192
    } else if official_deepseek {
        codewhale_models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
    } else if let Some(window) = codewhale_models::context_window_for_model(resolved_model) {
        window
    } else if let Some(window) = offering
        .as_ref()
        .and_then(|row| row.limit.as_ref())
        .and_then(|limit| limit.context)
        .and_then(|window| u32::try_from(window).ok())
    {
        window
    } else {
        codewhale_models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS
    };

    // Output limits require an exact catalog row or an explicitly recognized
    // compatibility alias. A family-name match cannot document a new model.
    // The catalog answers
    // `None` when the catalogue has no row. That is the truthful state for
    // membership routes such as the `kimi-for-coding` family, whose ceilings
    // are owned by the membership catalog. It must not become a placeholder
    // number: a fabricated 4K here silently clamped offline membership routes
    // to 4K output via `route_budget`.
    let max_output = codewhale_models::max_output_tokens_for_model(resolved_model)
        .or_else(|| {
            // Provider-owned wire IDs need not exist in the legacy model-only
            // catalog (for example Fireworks' accounts/... slug). Reuse the
            // exact bundled offering instead of inferring a family ceiling.
            offering
                .as_ref()
                .and_then(|offering| offering.limit.as_ref())
                .and_then(|limit| limit.output)
                .and_then(|limit| u32::try_from(limit).ok())
        })
        .or_else(|| {
            codewhale_models::canonical_official_deepseek_model_id(resolved_model)
                .or_else(|| {
                    alias_deprecation.as_ref().map(|_| {
                        codewhale_config::catalog::reviewed::constants::DEEPSEEK_ALIAS_REPLACEMENT
                    })
                })
                .and_then(codewhale_models::max_output_tokens_for_model)
        });

    // Exact catalog reasoning facts and recognized compatibility aliases only.
    let thinking_supported = official_deepseek
        || is_reasoner
        || offering
            .as_ref()
            .is_some_and(|row| row.reasoning == Some(true))
        || codewhale_models::model_supports_reasoning(resolved_model);

    // Cache telemetry: returned only by DeepSeek-native and NVIDIA NIM endpoints.
    let cache_telemetry_supported = matches!(
        provider,
        ProviderKind::Deepseek | ProviderKind::NvidiaNim | ProviderKind::Volcengine
    );

    let request_payload_mode = if matches!(
        provider,
        ProviderKind::DeepseekAnthropic | ProviderKind::MinimaxAnthropic | ProviderKind::Openmodel
    ) {
        RequestPayloadMode::AnthropicMessages
    } else {
        RequestPayloadMode::ChatCompletions
    };

    ProviderCapability {
        provider,
        resolved_model: resolved_model.to_string(),
        context_window,
        max_output,
        thinking_supported,
        cache_telemetry_supported,
        request_payload_mode,
        alias_deprecation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_config::catalog::CatalogSource;
    use codewhale_config::descriptors::defaults::{
        DEFAULT_TOGETHER_FLASH_MODEL, DEFAULT_TOGETHER_MODEL,
    };

    #[test]
    fn wire_model_lookup_is_exact_first_and_never_guesses_between_case_variants() {
        let row = |id: &str| CatalogOffering {
            provider: "p".to_string(),
            wire_model_id: id.to_string(),
            ..Default::default()
        };
        let rows = vec![row("org/Model-A"), row("org/model-a"), row("org/Model-B")];
        let found =
            |needle: &str| find_wire_model(&rows, needle).map(|row| row.wire_model_id.clone());
        assert_eq!(found("org/model-a").as_deref(), Some("org/model-a"));
        assert_eq!(found("org/Model-A").as_deref(), Some("org/Model-A"));
        assert_eq!(found("ORG/MODEL-A"), None, "two distinct ids fold together");
        assert_eq!(found("org/model-b").as_deref(), Some("org/Model-B"));

        // Listing keeps both case variants: each is a distinct, pickable id.
        let listed = catalog_models_from_offerings(rows.iter());
        assert!(
            listed.contains(&"org/Model-A".to_string())
                && listed.contains(&"org/model-a".to_string()),
            "case variants of a wire id are distinct models: {listed:?}"
        );
    }

    fn catalog_test_config(first_url: &str, second_url: &str) -> Config {
        use codewhale_runtime::config::{ProviderConfig, ProvidersConfig};
        {
            let __cw_config_init_0_0 = Some("catalog-first".to_string());
            let __cw_config_init_0_1 = Some(ProvidersConfig {
                custom: [
                    ("catalog-first", first_url, "first-route-test-key"),
                    ("catalog-second", second_url, "second-route-test-key"),
                ]
                .into_iter()
                .map(|(name, base_url, key)| {
                    (
                        name.to_string(),
                        ProviderConfig {
                            kind: Some("openai-compatible".to_string()),
                            base_url: Some(base_url.to_string()),
                            api_key: Some(key.to_string()),
                            model: Some("saved-model".to_string()),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
                ..Default::default()
            });
            let mut __cw_config_init_0_value: Config = Default::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        }
    }

    async fn catalog_mock(
        server: &wiremock::MockServer,
        key: &str,
        status: u16,
        body: serde_json::Value,
    ) {
        use wiremock::matchers::{header, method, path};
        wiremock::Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("Authorization", format!("Bearer {key}")))
            .respond_with(wiremock::ResponseTemplate::new(status).set_body_json(body))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn models_update_refuses_credentials_changed_during_request() {
        let _env = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _cli = crate::test_support::EnvVarGuard::remove(codewhale_config::CLI_API_KEY_ENV);
        let _key =
            crate::test_support::EnvVarGuard::set("CWC_CATALOG_TEST_KEY", "first-route-test-key");
        let upstream = wiremock::MockServer::start().await;
        let mut config = catalog_test_config(&upstream.uri(), &upstream.uri());
        let entry = config
            .providers
            .as_mut()
            .unwrap()
            .custom
            .get_mut("catalog-first")
            .unwrap();
        entry.api_key = None;
        entry.api_key_env = Some("CWC_CATALOG_TEST_KEY".into());
        let identity = config.resolve_provider_identity("catalog-first").unwrap();
        crate::provider_catalog_live::reset_cache_for_test();
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_millis(100))
                    .set_body_json(
                        serde_json::json!({"data":[{"id":"old-account-private-model"}]}),
                    ),
            )
            .expect(1)
            .mount(&upstream)
            .await;
        let change = async {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while upstream.received_requests().await.unwrap().is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            // This guard stays alive until after the delayed refresh completes.
            crate::test_support::EnvVarGuard::set("CWC_CATALOG_TEST_KEY", "other-account-test-key")
        };
        let (receipt, _changed) = tokio::join!(update_provider_catalog(&config, &identity), change);
        assert_eq!(receipt.outcome, "skipped");
        assert_eq!(receipt.error, Some("refresh_credentials_changed"));
        assert!(
            crate::provider_catalog_live::cached_entry_for_route(
                ProviderKind::Custom,
                identity.key.as_str(),
                &upstream.uri()
            )
            .unwrap()
            .is_none()
        );
    }

    #[tokio::test]
    async fn models_update_persists_exact_routes_and_keeps_prior_rows_after_failure() {
        let _env = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        crate::provider_catalog_live::reset_cache_for_test();
        let _cli_key = crate::test_support::EnvVarGuard::remove(codewhale_config::CLI_API_KEY_ENV);
        let first = wiremock::MockServer::start().await;
        let second = wiremock::MockServer::start().await;
        let config = catalog_test_config(&first.uri(), &second.uri());
        let first_id = config.resolve_provider_identity("catalog-first").unwrap();
        let second_id = config.resolve_provider_identity("catalog-second").unwrap();
        catalog_mock(
            &first,
            "first-route-test-key",
            200,
            serde_json::json!({"data":[{"id":"new-first-model"}]}),
        )
        .await;
        catalog_mock(
            &second,
            "second-route-test-key",
            200,
            serde_json::json!({"data":[{"id":"new-second-model"}]}),
        )
        .await;
        assert_eq!(
            update_provider_catalog(&config, &first_id).await.outcome,
            "updated"
        );
        assert_eq!(
            update_provider_catalog(&config, &second_id).await.outcome,
            "updated"
        );
        assert_eq!(config.provider.as_deref(), Some("catalog-first"));
        assert_eq!(config.default_model(), "saved-model");
        // Simulate restart: remove only this memo, not any persistent state.
        crate::provider_catalog_live::reset_cache_for_test();
        assert_eq!(
            catalog_models_for_route(ProviderKind::Custom, "catalog-first", &first.uri()),
            ["new-first-model"]
        );
        assert_eq!(
            catalog_models_for_route(ProviderKind::Custom, "catalog-second", &second.uri()),
            ["new-second-model"]
        );
        assert!(
            catalog_models_for_route(ProviderKind::Custom, "catalog-first", &second.uri())
                .is_empty()
        );
        let prior = cached_receipt(&config, &first_id).fetched_at;
        first.reset().await;
        catalog_mock(
            &first,
            "first-route-test-key",
            401,
            serde_json::json!({"error":"first-route-test-key"}),
        )
        .await;
        let receipt = update_provider_catalog(&config, &first_id).await;
        assert_eq!(receipt.outcome, "failed");
        assert_eq!(receipt.fetched_at, prior);
        assert!(matches!(
            receipt.status,
            CatalogStatus::Failed {
                reason: codewhale_config::catalog::CatalogRefreshError::Unauthorized
            }
        ));
        assert_eq!(
            catalog_models_for_route(ProviderKind::Custom, "catalog-first", &first.uri()),
            ["new-first-model"]
        );
        let body =
            std::fs::read_to_string(crate::provider_catalog_live::cache_path().unwrap()).unwrap();
        assert!(!body.contains("first-route-test-key"));
        assert!(!body.contains(&first.uri()));
        assert!(
            !serde_json::to_string(&receipt)
                .unwrap()
                .contains("first-route-test-key")
        );
    }

    #[tokio::test]
    async fn models_update_removes_withdrawn_ids_and_rejects_secret_or_control_ids() {
        let _env = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        crate::provider_catalog_live::reset_cache_for_test();
        let _cli_key = crate::test_support::EnvVarGuard::remove(codewhale_config::CLI_API_KEY_ENV);
        let server = wiremock::MockServer::start().await;
        let config = catalog_test_config(&server.uri(), &server.uri());
        let identity = config.resolve_provider_identity("catalog-first").unwrap();
        for model in [
            "old-model",
            "new-model",
            "first-route-test-key",
            "bad\u{1b}[31m-model",
        ] {
            server.reset().await;
            catalog_mock(
                &server,
                "first-route-test-key",
                200,
                serde_json::json!({"data":[{"id": model}]}),
            )
            .await;
            let receipt = update_provider_catalog(&config, &identity).await;
            if model.starts_with("old-") || model.starts_with("new-") {
                assert_eq!(receipt.outcome, "updated");
                assert_eq!(
                    catalog_models_for_route(
                        ProviderKind::Custom,
                        identity.key.as_str(),
                        &server.uri()
                    ),
                    [model]
                );
            } else {
                assert_eq!(receipt.outcome, "failed");
                assert_eq!(
                    catalog_models_for_route(
                        ProviderKind::Custom,
                        identity.key.as_str(),
                        &server.uri()
                    ),
                    ["new-model"]
                );
            }
        }
        server.reset().await;
        catalog_mock(
            &server,
            "first-route-test-key",
            200,
            serde_json::json!({"data":[]}),
        )
        .await;
        let receipt = update_provider_catalog(&config, &identity).await;
        // The existing provider adapter treats an empty list as a failed
        // refresh. Preserve the last usable rows and disclose that failure.
        assert_eq!(receipt.outcome, "failed");
        assert_eq!(
            catalog_models_for_route(ProviderKind::Custom, identity.key.as_str(), &server.uri()),
            ["new-model"]
        );
    }

    #[tokio::test]
    async fn chatgpt_catalog_refresh_rejects_unregistered_legacy_credentials() {
        let _env = crate::test_support::lock_test_env();
        let _token = crate::test_support::EnvVarGuard::set("CODEX_ACCESS_TOKEN", "legacy-token");
        let config = {
            let __cw_config_init_1_0 = Some("openai-codex".to_string());
            let mut __cw_config_init_1_value: Config = Default::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value
        };
        let identity = config.resolve_provider_identity("openai-codex").unwrap();
        let receipt = update_provider_catalog(&config, &identity).await;
        assert_eq!(receipt.outcome, "failed");
        assert_eq!(receipt.error, Some("chatgpt_plan_permission_required"));
        assert_eq!(receipt.model_count, 0);
    }

    #[test]
    fn codex_live_roster_receipt_discloses_skipped_persistence() {
        let identity = Config::default()
            .resolve_provider_identity("openai-codex")
            .unwrap();
        let roster = codewhale_runtime::codex_model_roster::CodexModelRoster {
            models: Vec::new(),
            freshness: codewhale_runtime::codex_model_roster::CodexModelCacheFreshness::Fresh,
            fetched_at: None,
            observed_at: Some(chrono::Utc::now()),
            source: "chatgpt_plan_api",
            observation_persisted: false,
        };
        let receipt = codex_roster_receipt(&identity, &roster);
        assert_eq!(receipt.status, CatalogStatus::Fresh);
        assert_eq!(receipt.error, Some("chatgpt_catalog_not_persisted"));
        assert_eq!(receipt.fetched_at, None);
        assert!(receipt.observed_at.is_some());
    }

    #[tokio::test]
    async fn models_listing_is_offline_and_update_never_forwards_another_routes_cli_key() {
        let _env = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        crate::provider_catalog_live::reset_cache_for_test();
        let _source =
            crate::test_support::EnvVarGuard::set(codewhale_config::CLI_API_KEY_SOURCE_ENV, "cli");
        let _key = crate::test_support::EnvVarGuard::set(
            codewhale_config::CLI_API_KEY_ENV,
            "active-route-cli-key",
        );
        let server = wiremock::MockServer::start().await;
        let config = catalog_test_config(&server.uri(), &server.uri());
        run_models(&config, false, Some("catalog-first"), true)
            .await
            .unwrap();
        let other = config.resolve_provider_identity("catalog-second").unwrap();
        let receipt = update_provider_catalog(&config, &other).await;
        assert_eq!(receipt.outcome, "skipped");
        assert_eq!(receipt.error, Some("cli_key_is_scoped_to_active_provider"));
        assert!(server.received_requests().await.unwrap().is_empty());
        assert!(!crate::provider_catalog_live::cache_path().unwrap().exists());
    }

    #[tokio::test]
    async fn models_update_reports_io_failure_without_claiming_persistence() {
        let _env = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        crate::provider_catalog_live::reset_cache_for_test();
        let _cli_key = crate::test_support::EnvVarGuard::remove(codewhale_config::CLI_API_KEY_ENV);
        let server = wiremock::MockServer::start().await;
        let config = catalog_test_config(&server.uri(), &server.uri());
        let identity = config.resolve_provider_identity("catalog-first").unwrap();
        let path = crate::provider_catalog_live::cache_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"broken cache").unwrap();
        let receipt = update_provider_catalog(&config, &identity).await;
        assert_eq!(receipt.outcome, "failed");
        assert_eq!(receipt.error, Some("cache_read_failed"));
        assert_eq!(std::fs::read(&path).unwrap(), b"broken cache");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[test]
    fn models_update_scope_includes_every_named_identity_once() {
        let _env = crate::test_support::lock_test_env();
        let config = catalog_test_config("http://localhost:1", "http://localhost:2");
        let identities = catalog_identities(&config, None, true).unwrap();
        for name in ["catalog-first", "catalog-second"] {
            assert_eq!(
                identities
                    .iter()
                    .filter(|identity| identity.key.as_str() == name)
                    .count(),
                1
            );
        }
        assert_eq!(
            catalog_identities(&config, Some("catalog-second"), true)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn together_catalog_includes_flash_from_bundled_asset() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();
        let models = all_catalog_models_for_provider(ProviderKind::Together);
        assert!(
            models.contains(&DEFAULT_TOGETHER_MODEL.to_string()),
            "missing Together pro: {models:?}"
        );
        assert!(
            models.contains(&DEFAULT_TOGETHER_FLASH_MODEL.to_string()),
            "missing Together flash: {models:?}"
        );
    }

    #[test]
    fn configured_providers_matches_provider_predicate() {
        let _env_lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _auth_file = crate::test_support::EnvVarGuard::set(
            "OPENAI_CODEX_AUTH_FILE",
            tmp.path().join("missing-auth.json"),
        );
        let _openai_token = crate::test_support::EnvVarGuard::remove("OPENAI_CODEX_ACCESS_TOKEN");
        let _codex_token = crate::test_support::EnvVarGuard::remove("CODEX_ACCESS_TOKEN");
        let config = Config::default();
        let active = config.active_provider_identity().unwrap();
        let expected: Vec<_> = config
            .provider_identities()
            .into_iter()
            .filter(|identity| {
                crate::provider_readiness::provider_is_configured_for_active(
                    &config, identity, &active,
                )
            })
            .collect();
        assert_eq!(configured_providers(&config), expected);
    }

    #[test]
    fn models_for_provider_filters_unconfigured_gateways() {
        let _env_lock = crate::test_support::lock_test_env();
        let _together = crate::test_support::EnvVarGuard::remove("TOGETHER_API_KEY");
        let config = Config::default();
        assert!(
            models_for_provider(
                &config,
                &config.test_identity_for_kind(ProviderKind::Together)
            )
            .is_empty()
        );
        assert!(
            !models_for_provider(
                &config,
                &config.test_identity_for_kind(ProviderKind::Deepseek)
            )
            .is_empty()
        );
    }

    /// #4116 CRITICAL (no-narrowing guarantee for the migrated consumer): the
    /// catalog-backed facade must return a NON-EMPTY enumeration for every
    /// provider that has a non-empty legacy `model_completion_names_for_provider`
    /// table. `all_catalog_models_for_provider` falls back to that legacy table
    /// whenever the merged catalog has no rows for the provider, so this holds by
    /// construction — and it proves that the raw-legacy tail removed from the
    /// subagent `operator_model_for_subagent` consumer (which only ran when the
    /// facade was empty) was unreachable whenever legacy was non-empty. The
    /// migrated consumer is therefore behavior-preserving: it always has a
    /// catalog-sourced model to pick and never narrows to fewer choices than the
    /// legacy path offered.
    ///
    /// Note: the facade is intentionally *catalog-authoritative* (live >
    /// bundled > legacy fallback, #4188), so for some providers whose catalog
    /// supersedes stale entries in the legacy placeholder table (e.g.
    /// OpenRouter/MiniMax revisions), the facade is not a strict superset of
    /// every legacy id. That divergence does not affect subagent model
    /// *acceptance*, which is gated by `validate_route` /
    /// `requested_model_for_provider`, not by this list.
    #[test]
    fn catalog_facade_covers_every_provider_with_a_legacy_table() {
        let _env = crate::test_support::lock_test_env();
        let codex_home = tempfile::tempdir().expect("temporary CODEX_HOME");
        let _codex_home = crate::test_support::EnvVarGuard::set("CODEX_HOME", codex_home.path());
        let _live = lock_live_snapshot();
        clear_live_snapshot();
        for &provider in ProviderKind::all() {
            let legacy_len = model_completion_names_for_provider(provider).len();
            if legacy_len == 0 || provider == ProviderKind::OpenaiCodex {
                continue;
            }
            assert!(
                !all_catalog_models_for_provider(provider).is_empty(),
                "catalog facade returned no models for {provider:?} despite a \
                 non-empty legacy table ({legacy_len} entries): the operator-route \
                 consumer would have nothing to enumerate"
            );
        }
    }

    /// #4188: CodeWhale-only / local providers keep defaults via the legacy
    /// fallback when Models.dev (live or bundled) has no rows for them.
    #[test]
    fn codewhale_only_providers_keep_legacy_defaults() {
        let _env = crate::test_support::lock_test_env();
        let codex_home = tempfile::tempdir().expect("temporary CODEX_HOME");
        let _codex_home = crate::test_support::EnvVarGuard::set("CODEX_HOME", codex_home.path());
        let _live = lock_live_snapshot();
        clear_live_snapshot();
        // A public model table cannot attest account plan permission.
        assert!(all_catalog_models_for_provider(ProviderKind::OpenaiCodex).is_empty());

        // Ollama intentionally has an empty legacy table (user-supplied ids);
        // the lake must still return empty rather than inventing rows.
        assert!(all_catalog_models_for_provider(ProviderKind::Ollama).is_empty());
        assert!(model_completion_names_for_provider(ProviderKind::Ollama).is_empty());
        assert!(live_per_provider_models(ProviderKind::Ollama).is_empty());
    }

    #[test]
    fn ollama_live_default_uses_per_provider_tags_not_models_dev() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();

        set_live_snapshot(
            CatalogSnapshot {
                offerings: vec![CatalogOffering {
                    provider: "ollama".to_string(),
                    wire_model_id: "deepseek-v4-flash".to_string(),
                    endpoint_key: "chat".to_string(),
                    default_for_provider: true,
                    ..Default::default()
                }],
            },
            LiveSource::ModelsDev,
        );
        assert!(
            live_per_provider_models(ProviderKind::Ollama).is_empty(),
            "Models.dev must not satisfy a local Ollama default"
        );

        merge_live_offerings(vec![CatalogOffering {
            provider: "ollama".to_string(),
            wire_model_id: "qwen2.5:0.5b".to_string(),
            endpoint_key: "chat".to_string(),
            default_for_provider: true,
            ..Default::default()
        }]);
        assert_eq!(
            live_per_provider_models(ProviderKind::Ollama),
            vec!["qwen2.5:0.5b".to_string()]
        );
        clear_live_snapshot();
    }

    /// #4116 / #4188 (AC): a provider with no bundled/live catalog coverage must
    /// fall back to the legacy table verbatim, so CodeWhale-only routes stay
    /// usable. We assert this for every currently-unbundled provider that still
    /// carries a non-empty legacy list, and require at least one such provider
    /// to exist so the fallback path is actually exercised.
    #[test]
    fn unbundled_provider_falls_back_to_legacy_table() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();
        let merged = merged_snapshot();
        let mut exercised = 0usize;
        for &provider in ProviderKind::all() {
            // OpenAI Codex deliberately owns an account-scoped cache source;
            // its fallback behavior is covered separately above.
            if provider == ProviderKind::OpenaiCodex {
                continue;
            }
            let catalog_id = catalog_provider_id(provider);
            let has_catalog_rows = !merged.offerings_for_provider(catalog_id).is_empty();
            let legacy = model_completion_names_for_provider(provider);
            if has_catalog_rows || legacy.is_empty() {
                continue;
            }
            // Unbundled + non-empty legacy: the facade must echo the legacy list.
            let facade = all_catalog_models_for_provider(provider);
            let expected: Vec<String> = legacy.iter().map(|m| m.to_string()).collect();
            assert_eq!(
                facade, expected,
                "unbundled provider {provider:?} did not fall back to the legacy table"
            );
            exercised += 1;
        }
        assert!(
            exercised > 0,
            "expected at least one unbundled provider to exercise the legacy fallback path"
        );
    }

    /// #4188: live Models.dev rows win over bundled on identity, and clearing
    /// live restores the offline bundled snapshot (offline startup still works).
    #[test]
    fn live_snapshot_merges_over_bundled() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();
        // With no live snapshot, we get bundled models.
        let bundled = all_catalog_models_for_provider(ProviderKind::Deepseek);
        assert!(!bundled.is_empty());

        // Set a live snapshot that adds a synthetic model.
        let live = CatalogSnapshot {
            offerings: vec![CatalogOffering {
                provider: "deepseek".to_string(),
                wire_model_id: "deepseek-v4-synthetic".to_string(),
                endpoint_key: "chat".to_string(),
                ..Default::default()
            }],
        };
        set_live_snapshot(live, LiveSource::ModelsDev);
        let merged = all_catalog_models_for_provider(ProviderKind::Deepseek);
        assert!(merged.contains(&"deepseek-v4-synthetic".to_string()));
        // The bundled model is still present.
        assert!(merged.iter().any(|m| bundled.contains(m)));

        clear_live_snapshot();
        let after_clear = all_catalog_models_for_provider(ProviderKind::Deepseek);
        assert_eq!(after_clear, bundled);
    }

    #[test]
    fn provider_owned_roster_replaces_bundled_and_models_dev_rows() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();
        let bundled = all_catalog_models_for_provider(ProviderKind::Openrouter);
        assert!(
            !bundled.is_empty(),
            "OpenRouter must have an offline fallback roster"
        );

        set_live_snapshot(
            CatalogSnapshot {
                offerings: vec![CatalogOffering {
                    provider: "openrouter".to_string(),
                    wire_model_id: "models-dev-only-openrouter-model".to_string(),
                    endpoint_key: "chat".to_string(),
                    ..Default::default()
                }],
            },
            LiveSource::ModelsDev,
        );
        set_live_snapshot(
            CatalogSnapshot {
                offerings: vec![CatalogOffering {
                    provider: "openrouter".to_string(),
                    wire_model_id: "provider-owned-openrouter-model".to_string(),
                    endpoint_key: "chat".to_string(),
                    ..Default::default()
                }],
            },
            LiveSource::PerProvider,
        );

        assert_eq!(
            all_catalog_models_for_provider(ProviderKind::Openrouter),
            vec!["provider-owned-openrouter-model".to_string()],
            "a successful provider-owned refresh must remove stale bundled and Models.dev ids"
        );

        replace_provider_live_snapshot("openrouter", CatalogSnapshot::default());
        let restored_cross_provider = all_catalog_models_for_provider(ProviderKind::Openrouter);
        assert!(
            restored_cross_provider.contains(&"models-dev-only-openrouter-model".to_string()),
            "clearing the exact partition must restore the cross-provider fallback"
        );
        assert!(
            restored_cross_provider
                .iter()
                .any(|model| bundled.contains(model)),
            "clearing the exact partition must restore bundled fallbacks"
        );

        clear_live_snapshot();
        assert_eq!(
            all_catalog_models_for_provider(ProviderKind::Openrouter),
            bundled
        );
    }

    #[test]
    fn named_custom_catalogs_keep_exact_identity_without_compiled_seeds() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();

        // No live rows, no bundled rows, no configured rows: an ordinary
        // custom route offers nothing rather than a compiled default (#6289).
        for identity in ["baseten", "another-custom-host"] {
            assert!(
                all_catalog_models_for_provider_identity(ProviderKind::Custom, Some(identity))
                    .is_empty(),
                "{identity} must not invent models offline"
            );
        }

        set_live_snapshot(
            CatalogSnapshot {
                offerings: vec![CatalogOffering {
                    provider: "baseten".to_string(),
                    wire_model_id: "synthetic-live-baseten-model".to_string(),
                    endpoint_key: "chat".to_string(),
                    source: CatalogSource::Live {
                        base_url_fingerprint: "baseten-fp".to_string(),
                        fetched_at: 42,
                    },
                    ..Default::default()
                }],
            },
            LiveSource::PerProvider,
        );

        assert_eq!(
            all_catalog_models_for_provider_identity(ProviderKind::Custom, Some("baseten")),
            vec!["synthetic-live-baseten-model".to_string()]
        );
        let case_distinct =
            all_catalog_models_for_provider_identity(ProviderKind::Custom, Some("BASETEN"));
        assert!(
            case_distinct.is_empty(),
            "a case variant shares neither seeds nor another exact table's live roster"
        );
        assert!(
            catalog_offering_for_model_identity(
                ProviderKind::Custom,
                Some("baseten"),
                "synthetic-live-baseten-model",
            )
            .is_some()
        );
        assert!(
            catalog_offering_for_model(ProviderKind::Custom, "synthetic-live-baseten-model",)
                .is_none(),
            "the generic custom bucket must not see Baseten rows"
        );

        clear_live_snapshot();
    }

    #[test]
    fn case_colliding_and_builtin_named_custom_catalogs_stay_isolated() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();

        for (provider, model) in [("CustomA", "upper-model"), ("customa", "lower-model")] {
            replace_provider_live_snapshot(
                provider,
                CatalogSnapshot {
                    offerings: vec![CatalogOffering {
                        provider: provider.to_string(),
                        wire_model_id: model.to_string(),
                        endpoint_key: "chat".to_string(),
                        ..Default::default()
                    }],
                },
            );
        }

        assert_eq!(
            all_catalog_models_for_provider_identity(ProviderKind::Custom, Some("CustomA")),
            vec!["upper-model".to_string()]
        );
        assert_eq!(
            all_catalog_models_for_provider_identity(ProviderKind::Custom, Some("customa")),
            vec!["lower-model".to_string()]
        );
        let built_in_openai = all_catalog_models_for_provider(ProviderKind::Openai);
        assert!(!built_in_openai.is_empty());
        assert!(
            all_catalog_models_for_provider_identity(ProviderKind::Custom, Some("openai"))
                .is_empty(),
            "a custom table named openai must not borrow the first-class OpenAI template"
        );
        for model in &built_in_openai {
            assert!(
                catalog_offering_for_model_identity(ProviderKind::Custom, Some("openai"), model)
                    .is_none(),
                "an exact custom table named openai must not inherit built-in model {model}"
            );
        }

        let custom_model = "custom-openai-only-model";
        replace_provider_live_snapshot_for_identity(
            ProviderKind::Custom,
            "openai",
            CatalogSnapshot {
                offerings: vec![CatalogOffering {
                    provider: "openai".to_string(),
                    wire_model_id: custom_model.to_string(),
                    endpoint_key: "chat".to_string(),
                    ..Default::default()
                }],
            },
        );
        assert_eq!(
            all_catalog_models_for_provider_identity(ProviderKind::Custom, Some("openai")),
            vec![custom_model.to_string()],
            "the exact custom table must retrieve its own built-in-looking roster"
        );
        assert_eq!(
            all_catalog_models_for_provider(ProviderKind::Openai),
            built_in_openai,
            "publishing custom openai must not replace or suppress built-in OpenAI"
        );
        assert!(
            catalog_offering_for_model(ProviderKind::Openai, custom_model).is_none(),
            "the built-in OpenAI route must not see the custom table's row"
        );

        let built_in_live_model = "built-in-openai-only-model";
        replace_provider_live_snapshot_for_identity(
            ProviderKind::Openai,
            "openai",
            CatalogSnapshot {
                offerings: vec![CatalogOffering {
                    provider: "openai".to_string(),
                    wire_model_id: built_in_live_model.to_string(),
                    endpoint_key: "chat".to_string(),
                    ..Default::default()
                }],
            },
        );
        assert_eq!(
            all_catalog_models_for_provider(ProviderKind::Openai),
            vec![built_in_live_model.to_string()]
        );
        assert_eq!(
            all_catalog_models_for_provider_identity(ProviderKind::Custom, Some("openai")),
            vec![custom_model.to_string()],
            "publishing built-in OpenAI must not replace the custom table's roster"
        );

        clear_live_snapshot();
    }

    #[test]
    fn live_catalog_origin_prefers_per_provider_over_models_dev() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();
        let wire = "accounts/fireworks/models/deepseek-v4-flash-0731";
        assert_eq!(live_catalog_origin(ProviderKind::Fireworks, wire), None);

        set_live_snapshot(
            CatalogSnapshot {
                offerings: vec![CatalogOffering {
                    provider: "fireworks".to_string(),
                    wire_model_id: wire.to_string(),
                    endpoint_key: "chat".to_string(),
                    ..Default::default()
                }],
            },
            LiveSource::ModelsDev,
        );
        assert_eq!(
            live_catalog_origin(ProviderKind::Fireworks, wire),
            Some(LiveSource::ModelsDev)
        );

        set_live_snapshot(
            CatalogSnapshot {
                offerings: vec![CatalogOffering {
                    provider: "fireworks".to_string(),
                    wire_model_id: wire.to_string(),
                    endpoint_key: "chat".to_string(),
                    ..Default::default()
                }],
            },
            LiveSource::PerProvider,
        );
        assert_eq!(
            live_catalog_origin(ProviderKind::Fireworks, wire),
            Some(LiveSource::PerProvider)
        );
        clear_live_snapshot();
    }

    /// Footprint: the merge holds `Arc`s into the bundled and Models.dev
    /// layers, so only rows it rewrites exist twice in memory.
    #[test]
    fn merged_snapshot_shares_rows_with_its_layers_instead_of_copying_them() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();

        let live_id = "deepseek-shared-row-probe";
        set_live_snapshot(
            CatalogSnapshot {
                offerings: vec![CatalogOffering {
                    provider: "deepseek".to_string(),
                    wire_model_id: live_id.to_string(),
                    endpoint_key: "chat".to_string(),
                    ..Default::default()
                }],
            },
            LiveSource::ModelsDev,
        );
        let merged = merged_snapshot();
        let live_row = {
            let live = LIVE_SNAPSHOT.read().expect("live snapshot");
            let models_dev = live.models_dev.as_ref().expect("models.dev partition");
            Arc::clone(&models_dev.offerings[0])
        };
        let merged_live_row = merged
            .offerings
            .iter()
            .find(|row| row.wire_model_id == live_id)
            .expect("live row merged");
        assert!(
            Arc::ptr_eq(merged_live_row, &live_row),
            "the merge must share the Models.dev row, not hold a second copy"
        );

        let bundled = bundled_snapshot();
        let shared_bundled = merged
            .offerings
            .iter()
            .filter(|row| bundled.offerings.iter().any(|b| Arc::ptr_eq(b, row)))
            .count();
        let untouched_bundled = bundled
            .offerings
            .iter()
            .filter(|row| {
                ProviderKind::parse(&row.provider) != Some(ProviderKind::OpencodeGo)
                    && !(row.provider == "deepseek" && row.wire_model_id == live_id)
            })
            .count();
        assert_eq!(
            shared_bundled, untouched_bundled,
            "every bundled row the merge does not rewrite must be shared"
        );

        clear_live_snapshot();
    }

    /// Memoization: repeated `merged_snapshot()` calls return the cached merge
    /// (same `Arc` allocation), and publishing or clearing a live snapshot
    /// invalidates the cache so new content becomes visible.
    #[test]
    fn merged_snapshot_cache_invalidates_on_live_snapshot_change() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();

        let bundled_only = merged_snapshot();
        assert!(
            Arc::ptr_eq(&bundled_only, &merged_snapshot()),
            "repeated merged_snapshot() calls must return the cached Arc"
        );
        let probe = "deepseek-cache-probe-model";
        assert!(
            !bundled_only
                .offerings
                .iter()
                .any(|row| row.wire_model_id == probe),
            "probe model must not pre-exist in the bundled snapshot"
        );

        set_live_snapshot(
            CatalogSnapshot {
                offerings: vec![CatalogOffering {
                    provider: "deepseek".to_string(),
                    wire_model_id: probe.to_string(),
                    endpoint_key: "chat".to_string(),
                    ..Default::default()
                }],
            },
            LiveSource::ModelsDev,
        );
        let with_live = merged_snapshot();
        assert!(
            !Arc::ptr_eq(&bundled_only, &with_live),
            "set_live_snapshot must invalidate the memoized merge"
        );
        assert!(
            with_live
                .offerings
                .iter()
                .any(|row| row.wire_model_id == probe),
            "new live content must be visible after set_live_snapshot"
        );

        clear_live_snapshot();
        let after_clear = merged_snapshot();
        assert!(
            !after_clear
                .offerings
                .iter()
                .any(|row| row.wire_model_id == probe),
            "clear_live_snapshot must invalidate the memoized merge"
        );
        assert_eq!(
            after_clear.offerings, bundled_only.offerings,
            "clearing live must restore the bundled-only merge content"
        );
    }

    #[test]
    fn opencode_go_lake_corrects_stale_protocols_in_saved_and_live_rows() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();

        let mut offerings: Vec<_> = codewhale_config::opencode_go_models()
            .iter()
            .map(|model| CatalogOffering {
                provider: "opencode_go".to_string(),
                wire_model_id: if *model
                    == codewhale_config::descriptors::defaults::DEFAULT_OPENCODE_GO_MODEL
                {
                    format!("opencode-go/{model}")
                } else {
                    (*model).to_string()
                },
                endpoint_key: "chat".to_string(),
                ..Default::default()
            })
            .collect();
        offerings.extend(["minimax-m3", "qwen3.7-max"].map(|model| CatalogOffering {
            provider: "opencode-go".to_string(),
            wire_model_id: model.to_string(),
            endpoint_key: "messages".to_string(),
            ..Default::default()
        }));
        set_live_snapshot(CatalogSnapshot { offerings }, LiveSource::ModelsDev);

        let models: std::collections::BTreeSet<_> =
            all_catalog_models_for_provider(ProviderKind::OpencodeGo)
                .into_iter()
                .collect();
        let expected: std::collections::BTreeSet<_> = codewhale_config::opencode_go_models()
            .iter()
            .map(|model| (*model).to_string())
            .collect();
        assert_eq!(models, expected);
        for (model, endpoint) in [("minimax-m3", "messages"), ("grok-4.6", "responses")] {
            let row = catalog_offering_for_model(ProviderKind::OpencodeGo, model)
                .expect("documented model survives refresh");
            assert_eq!(row.endpoint_key, endpoint);
        }
        assert!(
            catalog_offering_for_model(
                ProviderKind::OpencodeGo,
                codewhale_config::descriptors::defaults::DEFAULT_OPENCODE_GO_MODEL,
            )
            .is_some()
        );

        clear_live_snapshot();
    }

    /// #4188: live > bundled > legacy fallback precedence, including live
    /// override of a bundled wire id and no duplicate rows after alias
    /// normalization (`moonshotai` → `moonshot`).
    #[test]
    fn live_over_bundled_over_legacy_precedence_and_alias_dedupe() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();

        let bundled_moonshot = all_catalog_models_for_provider(ProviderKind::Moonshot);
        assert!(
            !bundled_moonshot.is_empty(),
            "offline bundled Moonshot seed required: {bundled_moonshot:?}"
        );

        // Live rows use the Models.dev alias id; lake merge must normalize onto
        // CodeWhale `moonshot` and not leave a parallel `moonshotai` bucket.
        let live = CatalogSnapshot {
            offerings: vec![
                CatalogOffering {
                    provider: "moonshot".to_string(),
                    wire_model_id: "kimi-k2.5-live".to_string(),
                    endpoint_key: "chat".to_string(),
                    default_for_provider: true,
                    ..Default::default()
                },
                // Same identity as a typical bundled Moonshot default — live wins.
                CatalogOffering {
                    provider: "moonshot".to_string(),
                    wire_model_id: bundled_moonshot[0].clone(),
                    endpoint_key: "chat".to_string(),
                    family: Some("live-override".to_string()),
                    ..Default::default()
                },
            ],
        };
        set_live_snapshot(live, LiveSource::ModelsDev);

        let merged = merged_snapshot();
        let moonshot_rows = merged.offerings_for_provider("moonshot");
        assert!(
            moonshot_rows
                .iter()
                .any(|r| r.wire_model_id == "kimi-k2.5-live"),
            "live-only Moonshot row missing: {moonshot_rows:?}"
        );
        let overridden = moonshot_rows
            .iter()
            .find(|r| r.wire_model_id == bundled_moonshot[0])
            .expect("bundled Moonshot id should still exist after live merge");
        assert_eq!(
            overridden.family.as_deref(),
            Some("live-override"),
            "live row must replace bundled facts on the same wire id"
        );
        assert!(
            merged.offerings_for_provider("moonshotai").is_empty(),
            "alias-normalized providers must not leave a duplicate moonshotai bucket"
        );

        let models = all_catalog_models_for_provider(ProviderKind::Moonshot);
        let mut seen = std::collections::BTreeSet::new();
        for model in &models {
            assert!(
                seen.insert(model.to_ascii_lowercase()),
                "duplicate Moonshot model row after alias merge: {model}"
            );
        }
        assert!(models.contains(&"kimi-k2.5-live".to_string()));

        // Legacy fallback is skipped when catalog rows exist (even if legacy
        // lists additional ids) — catalog is authoritative once non-empty.
        assert!(
            !model_completion_names_for_provider(ProviderKind::Moonshot).is_empty(),
            "legacy Moonshot table should still exist as fallback documentation"
        );

        clear_live_snapshot();
        assert_eq!(
            all_catalog_models_for_provider(ProviderKind::Moonshot),
            bundled_moonshot,
            "clearing live must restore offline bundled Moonshot rows"
        );
    }

    /// #4188: when live Models.dev emits both an alias id and the CodeWhale id
    /// for the same provider, compiling through `live_offerings_from_models_dev`
    /// then merging into the lake must not produce duplicate model rows.
    #[test]
    fn alias_normalized_live_rows_do_not_duplicate_in_lake() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();
        let body = r#"{
          "models": {},
          "providers": {
            "moonshotai": {
              "id": "moonshotai",
              "models": {
                "kimi-k2.5": {
                  "id": "kimi-k2.5",
                  "modalities": { "input": ["text"], "output": ["text"] }
                }
              }
            },
            "moonshot": {
              "id": "moonshot",
              "models": {
                "kimi-k2.5": {
                  "id": "kimi-k2.5",
                  "modalities": { "input": ["text"], "output": ["text"] },
                  "limit": { "context": 262144, "output": 8192 }
                },
                "kimi-k2.7-code": {
                  "id": "kimi-k2.7-code",
                  "modalities": { "input": ["text"], "output": ["text"] }
                }
              }
            }
          }
        }"#;
        let catalog =
            codewhale_config::models_dev::ModelsDevCatalog::parse_json(body).expect("parse");
        let live_rows =
            codewhale_config::catalog::live_offerings_from_models_dev(&catalog, 1_700_000_000);
        assert!(
            live_rows.iter().all(|r| r.provider == "moonshot"),
            "both moonshotai and moonshot must normalize onto moonshot: {:?}",
            live_rows
                .iter()
                .map(|r| r.provider.as_str())
                .collect::<Vec<_>>()
        );
        set_live_snapshot(
            CatalogSnapshot {
                offerings: live_rows,
            },
            LiveSource::ModelsDev,
        );

        let models = all_catalog_models_for_provider(ProviderKind::Moonshot);
        let kimi_count = models.iter().filter(|m| m.as_str() == "kimi-k2.5").count();
        assert_eq!(
            kimi_count, 1,
            "alias-normalized providers must not duplicate kimi-k2.5: {models:?}"
        );
        assert!(
            merged_snapshot()
                .offerings_for_provider("moonshotai")
                .is_empty()
        );
        clear_live_snapshot();
    }

    // ── Source-scoped partition tests (#4188 race fix) ──────────────────────

    #[test]
    fn provider_live_snapshots_are_scoped_per_provider() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();

        set_live_snapshot(
            CatalogSnapshot {
                offerings: vec![CatalogOffering {
                    provider: "telecomjs".to_string(),
                    wire_model_id: "deepseek-v4-pro".to_string(),
                    endpoint_key: "chat".to_string(),
                    ..Default::default()
                }],
            },
            LiveSource::PerProvider,
        );
        let telecom_only = merged_snapshot();
        assert_eq!(telecom_only.offerings_for_provider("telecomjs").len(), 1);

        set_live_snapshot(
            CatalogSnapshot {
                offerings: vec![CatalogOffering {
                    provider: "another-gateway".to_string(),
                    wire_model_id: "another-model".to_string(),
                    endpoint_key: "chat".to_string(),
                    ..Default::default()
                }],
            },
            LiveSource::PerProvider,
        );

        let merged = merged_snapshot();
        assert_eq!(merged.offerings_for_provider("telecomjs").len(), 1);
        assert_eq!(merged.offerings_for_provider("another-gateway").len(), 1);
        assert!(
            !Arc::ptr_eq(&telecom_only, &merged),
            "publishing a second provider must invalidate the cached merge"
        );

        clear_live_snapshot();
    }

    /// Models.dev→TelecomJS completion order: Models.dev sets its snapshot first,
    /// then TelecomJS merges per-provider rows. Both sets must be present in the
    /// final merged view.
    #[test]
    fn models_dev_first_then_telecomjs_both_preserved() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();

        // 1) Models.dev publishes its cross-provider snapshot.
        let models_dev_rows = vec![
            CatalogOffering {
                provider: "deepseek".to_string(),
                wire_model_id: "deepseek-chat".to_string(),
                endpoint_key: "chat".to_string(),
                family: Some("deepseek".to_string()),
                source: CatalogSource::Live {
                    base_url_fingerprint: "modelsdev-fp".to_string(),
                    fetched_at: 1000,
                },
                ..Default::default()
            },
            CatalogOffering {
                provider: "zai".to_string(),
                wire_model_id: "glm-4".to_string(),
                endpoint_key: "chat".to_string(),
                family: Some("glm".to_string()),
                source: CatalogSource::Live {
                    base_url_fingerprint: "modelsdev-fp".to_string(),
                    fetched_at: 1000,
                },
                ..Default::default()
            },
        ];
        set_live_snapshot(
            CatalogSnapshot {
                offerings: models_dev_rows,
            },
            LiveSource::ModelsDev,
        );
        let before_provider_refresh = merged_snapshot();
        assert!(
            before_provider_refresh
                .offerings_for_provider("telecomjs")
                .is_empty()
        );

        // 2) TelecomJS merges its per-provider rows (after Models.dev completes).
        let telecomjs_rows = vec![
            CatalogOffering {
                provider: "telecomjs".to_string(),
                wire_model_id: "deepseek-chat".to_string(),
                endpoint_key: "chat".to_string(),
                family: Some("deepseek".to_string()),
                source: CatalogSource::Live {
                    base_url_fingerprint: "telecomjs-fp".to_string(),
                    fetched_at: 2000,
                },
                ..Default::default()
            },
            CatalogOffering {
                provider: "telecomjs".to_string(),
                wire_model_id: "glm-4".to_string(),
                endpoint_key: "chat".to_string(),
                family: Some("glm".to_string()),
                source: CatalogSource::Live {
                    base_url_fingerprint: "telecomjs-fp".to_string(),
                    fetched_at: 2000,
                },
                ..Default::default()
            },
        ];
        merge_live_offerings(telecomjs_rows);
        assert_eq!(
            merged_snapshot().offerings_for_provider("telecomjs").len(),
            2,
            "provider refresh should invalidate the cached Models.dev-only view"
        );

        // 3) Both sources' rows are present in the merged snapshot.
        let merged = merged_snapshot();
        let deepseek_rows = merged.offerings_for_provider("deepseek");
        assert!(
            deepseek_rows
                .iter()
                .any(|r| r.wire_model_id == "deepseek-chat"),
            "Models.dev deepseek row missing: {deepseek_rows:?}"
        );
        let zai_rows = merged.offerings_for_provider("zai");
        assert!(
            zai_rows.iter().any(|r| r.wire_model_id == "glm-4"),
            "Models.dev zai row missing: {zai_rows:?}"
        );
        let telecomjs_rows_merged = merged.offerings_for_provider("telecomjs");
        assert_eq!(
            telecomjs_rows_merged.len(),
            2,
            "TelecomJS rows missing: {telecomjs_rows_merged:?}"
        );
        assert!(
            telecomjs_rows_merged
                .iter()
                .any(|r| r.wire_model_id == "deepseek-chat"),
            "TelecomJS deepseek-chat row missing"
        );
        assert!(
            telecomjs_rows_merged
                .iter()
                .any(|r| r.wire_model_id == "glm-4"),
            "TelecomJS glm-4 row missing"
        );

        clear_live_snapshot();
    }

    /// TelecomJS→Models.dev completion order: TelecomJS merges first, then
    /// Models.dev replaces the cross-provider snapshot. TelecomJS rows must
    /// survive the Models.dev refresh (they live in a separate partition).
    #[test]
    fn telecomjs_first_then_models_dev_both_preserved() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();

        // 1) TelecomJS merges its per-provider rows first.
        let telecomjs_rows = vec![
            CatalogOffering {
                provider: "telecomjs".to_string(),
                wire_model_id: "deepseek-chat".to_string(),
                endpoint_key: "chat".to_string(),
                family: Some("deepseek".to_string()),
                source: CatalogSource::Live {
                    base_url_fingerprint: "telecomjs-fp".to_string(),
                    fetched_at: 2000,
                },
                ..Default::default()
            },
            CatalogOffering {
                provider: "telecomjs".to_string(),
                wire_model_id: "glm-4".to_string(),
                endpoint_key: "chat".to_string(),
                family: Some("glm".to_string()),
                source: CatalogSource::Live {
                    base_url_fingerprint: "telecomjs-fp".to_string(),
                    fetched_at: 2000,
                },
                ..Default::default()
            },
        ];
        merge_live_offerings(telecomjs_rows);
        assert_eq!(
            merged_snapshot().offerings_for_provider("telecomjs").len(),
            2,
            "provider rows should be visible before Models.dev completes"
        );

        // 2) Models.dev refreshes and replaces its cross-provider snapshot.
        //    Before the source-scoped fix, this would have wiped TelecomJS rows.
        let models_dev_rows = vec![CatalogOffering {
            provider: "deepseek".to_string(),
            wire_model_id: "deepseek-chat".to_string(),
            endpoint_key: "chat".to_string(),
            family: Some("deepseek".to_string()),
            source: CatalogSource::Live {
                base_url_fingerprint: "modelsdev-fp".to_string(),
                fetched_at: 3000,
            },
            ..Default::default()
        }];
        set_live_snapshot(
            CatalogSnapshot {
                offerings: models_dev_rows,
            },
            LiveSource::ModelsDev,
        );

        // 3) Both sources' rows are present — TelecomJS rows were NOT erased.
        let merged = merged_snapshot();
        let telecomjs_rows_merged = merged.offerings_for_provider("telecomjs");
        assert_eq!(
            telecomjs_rows_merged.len(),
            2,
            "TelecomJS rows were erased by Models.dev refresh: {telecomjs_rows_merged:?}"
        );
        assert!(
            telecomjs_rows_merged
                .iter()
                .any(|r| r.wire_model_id == "deepseek-chat"),
            "TelecomJS deepseek-chat row erased"
        );
        assert!(
            telecomjs_rows_merged
                .iter()
                .any(|r| r.wire_model_id == "glm-4"),
            "TelecomJS glm-4 row erased"
        );
        let deepseek_rows = merged.offerings_for_provider("deepseek");
        assert!(
            deepseek_rows
                .iter()
                .any(|r| r.wire_model_id == "deepseek-chat"),
            "Models.dev deepseek row missing: {deepseek_rows:?}"
        );

        clear_live_snapshot();
    }

    /// Catalog refresh never deletes previously published rows: a Models.dev
    /// refresh that adds new rows must preserve existing per-provider rows,
    /// and a per-provider merge must preserve existing Models.dev rows.
    #[test]
    fn catalog_refresh_never_deletes_previously_published_rows() {
        let _live = lock_live_snapshot();
        clear_live_snapshot();

        // 1) Initial state: Models.dev publishes rows for deepseek + zai.
        let initial_models_dev = vec![
            CatalogOffering {
                provider: "deepseek".to_string(),
                wire_model_id: "deepseek-chat".to_string(),
                endpoint_key: "chat".to_string(),
                source: CatalogSource::Live {
                    base_url_fingerprint: "modelsdev-fp".to_string(),
                    fetched_at: 1000,
                },
                ..Default::default()
            },
            CatalogOffering {
                provider: "zai".to_string(),
                wire_model_id: "glm-4".to_string(),
                endpoint_key: "chat".to_string(),
                source: CatalogSource::Live {
                    base_url_fingerprint: "modelsdev-fp".to_string(),
                    fetched_at: 1000,
                },
                ..Default::default()
            },
        ];
        set_live_snapshot(
            CatalogSnapshot {
                offerings: initial_models_dev,
            },
            LiveSource::ModelsDev,
        );

        // 2) TelecomJS merges its rows.
        let telecomjs_rows = vec![CatalogOffering {
            provider: "telecomjs".to_string(),
            wire_model_id: "deepseek-chat".to_string(),
            endpoint_key: "chat".to_string(),
            source: CatalogSource::Live {
                base_url_fingerprint: "telecomjs-fp".to_string(),
                fetched_at: 2000,
            },
            ..Default::default()
        }];
        merge_live_offerings(telecomjs_rows);

        // Record what we have before the second refresh.
        let before_refresh = merged_snapshot();
        let before_providers: std::collections::BTreeSet<_> = before_refresh
            .offerings
            .iter()
            .map(|r| (r.provider.clone(), r.wire_model_id.clone()))
            .collect();
        assert!(
            before_providers.contains(&("deepseek".to_string(), "deepseek-chat".to_string())),
            "deepseek row should exist before refresh"
        );
        assert!(
            before_providers.contains(&("telecomjs".to_string(), "deepseek-chat".to_string())),
            "telecomjs row should exist before refresh"
        );

        // 3) Models.dev refreshes again with an updated snapshot (adds a new row).
        let updated_models_dev = vec![
            CatalogOffering {
                provider: "deepseek".to_string(),
                wire_model_id: "deepseek-chat".to_string(),
                endpoint_key: "chat".to_string(),
                source: CatalogSource::Live {
                    base_url_fingerprint: "modelsdev-fp".to_string(),
                    fetched_at: 3000,
                },
                ..Default::default()
            },
            CatalogOffering {
                provider: "zai".to_string(),
                wire_model_id: "glm-4".to_string(),
                endpoint_key: "chat".to_string(),
                source: CatalogSource::Live {
                    base_url_fingerprint: "modelsdev-fp".to_string(),
                    fetched_at: 3000,
                },
                ..Default::default()
            },
            // New row added by the refresh.
            CatalogOffering {
                provider: "moonshot".to_string(),
                wire_model_id: "kimi-k2.5".to_string(),
                endpoint_key: "chat".to_string(),
                source: CatalogSource::Live {
                    base_url_fingerprint: "modelsdev-fp".to_string(),
                    fetched_at: 3000,
                },
                ..Default::default()
            },
        ];
        set_live_snapshot(
            CatalogSnapshot {
                offerings: updated_models_dev,
            },
            LiveSource::ModelsDev,
        );

        // 4) The TelecomJS row is STILL present — it was not deleted.
        let after_refresh = merged_snapshot();
        let after_telecomjs: Vec<_> = after_refresh
            .offerings_for_provider("telecomjs")
            .iter()
            .map(|r| r.wire_model_id.clone())
            .collect();
        assert!(
            after_telecomjs.iter().any(|id| id == "deepseek-chat"),
            "TelecomJS row was deleted by Models.dev refresh! Remaining: {after_telecomjs:?}"
        );

        // 5) New Models.dev row is also present.
        let after_moonshot: Vec<_> = after_refresh
            .offerings_for_provider("moonshot")
            .iter()
            .map(|r| r.wire_model_id.clone())
            .collect();
        assert!(
            after_moonshot.iter().any(|id| id == "kimi-k2.5"),
            "New Models.dev moonshot row missing: {after_moonshot:?}"
        );

        // 6) Also verify: a per-provider merge does not delete Models.dev rows.
        let extra_telecomjs = vec![CatalogOffering {
            provider: "telecomjs".to_string(),
            wire_model_id: "glm-4".to_string(),
            endpoint_key: "chat".to_string(),
            source: CatalogSource::Live {
                base_url_fingerprint: "telecomjs-fp".to_string(),
                fetched_at: 4000,
            },
            ..Default::default()
        }];
        merge_live_offerings(extra_telecomjs);

        let final_merged = merged_snapshot();
        let final_deepseek: Vec<_> = final_merged
            .offerings_for_provider("deepseek")
            .iter()
            .map(|r| r.wire_model_id.clone())
            .collect();
        assert!(
            final_deepseek.iter().any(|id| id == "deepseek-chat"),
            "Models.dev deepseek row was deleted by per-provider merge! Remaining: {final_deepseek:?}"
        );
        let final_moonshot: Vec<_> = final_merged
            .offerings_for_provider("moonshot")
            .iter()
            .map(|r| r.wire_model_id.clone())
            .collect();
        assert!(
            final_moonshot.iter().any(|id| id == "kimi-k2.5"),
            "Models.dev moonshot row was deleted by per-provider merge! Remaining: {final_moonshot:?}"
        );

        clear_live_snapshot();
    }

    #[test]
    fn cloud_generation_updates_exact_catalog_defaults_and_disable_restores_baseline() {
        use codewhale_config::cloud_facts::{
            CloudFactsStatus, ModelFact, ProviderDefaultFact, ScopedFacts, overlay,
        };
        let _live = lock_live_snapshot();
        let home = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _enabled = crate::test_support::EnvVarGuard::remove("CODEWHALE_DISABLE_CLOUD_FACTS");
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                overlay::clear();
                clear_live_snapshot();
                crate::provider_catalog_live::reset_cache_for_test();
            }
        }
        let _reset = Reset;
        overlay::clear();
        clear_live_snapshot();
        crate::provider_catalog_live::reset_cache_for_test();
        let provider = ProviderKind::Openai;
        let base = provider.provider().default_base_url();
        let model = "cloud-catalog-fixture";
        let config = {
            let __cw_config_init_2_0 = Some("openai".into());
            let mut __cw_config_init_2_value: Config = Default::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value
        };
        let baseline = crate::route_runtime::resolve_runtime_route(&config, provider, None)
            .unwrap()
            .model;
        assert!(
            !catalog_models_for_route(provider, "openai", base)
                .iter()
                .any(|id| id == model)
        );
        let ticket = overlay::configure(true, "catalog-generation-test").unwrap();
        let mut facts = ScopedFacts {
            channel: "catalog-generation-test".into(),
            facts_version: 1,
            key_id: "cwf-test-only".into(),
            models: vec![ModelFact {
                provider: "openai".into(),
                id: model.into(),
                context_window: Some(31_337),
                ..Default::default()
            }],
            provider_defaults: BTreeMap::from([(
                "openai".into(),
                ProviderDefaultFact {
                    default_model: Some(model.into()),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        assert!(overlay::publish(
            &ticket,
            Some(facts.clone()),
            CloudFactsStatus::default()
        ));
        assert!(
            catalog_models_for_route(provider, "openai", base)
                .iter()
                .any(|id| id == model)
        );
        assert!(
            catalog_models_for_route(provider, "openai", "https://catalog-proxy.invalid/v1")
                .is_empty()
        );
        let route = crate::route_runtime::resolve_runtime_route(&config, provider, None).unwrap();
        assert_eq!(route.model, model);
        assert_eq!(route.candidate.limits().context_tokens, Some(31_337));
        assert_eq!(
            crate::route_runtime::resolve_runtime_route(&config, provider, Some(&baseline))
                .unwrap()
                .model,
            baseline
        );
        facts.facts_version = 2;
        facts.models[0].context_window = Some(62_674);
        assert!(overlay::publish(
            &ticket,
            Some(facts),
            CloudFactsStatus::default()
        ));
        assert_eq!(
            crate::route_runtime::resolve_runtime_route(&config, provider, None)
                .unwrap()
                .candidate
                .limits()
                .context_tokens,
            Some(62_674)
        );
        overlay::clear();
        assert_eq!(
            crate::route_runtime::resolve_runtime_route(&config, provider, None)
                .unwrap()
                .model,
            baseline
        );
        assert!(
            !catalog_models_for_route(provider, "openai", base)
                .iter()
                .any(|id| id == model)
        );
    }

    /// Test scaffolding shared by the signed-catalog cases: an isolated home,
    /// cloud facts enabled, and every process-wide layer reset on the way out.
    ///
    /// Field order is the drop order and is load-bearing: the env guards must
    /// restore their variables while this thread still holds the test env
    /// barrier that [`lock_live_snapshot`] took, so `_live` is declared last.
    struct CloudFactsTestEnv {
        _enabled: crate::test_support::EnvVarGuard,
        _home: crate::test_support::EnvVarGuard,
        _home_dir: tempfile::TempDir,
        _live: LiveSnapshotLock,
    }

    impl Drop for CloudFactsTestEnv {
        fn drop(&mut self) {
            codewhale_config::cloud_facts::overlay::clear();
            clear_live_snapshot();
            crate::provider_catalog_live::reset_cache_for_test();
        }
    }

    fn cloud_facts_test_env() -> CloudFactsTestEnv {
        let live = lock_live_snapshot();
        let home_dir = tempfile::tempdir().unwrap();
        let home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home_dir.path());
        let enabled = crate::test_support::EnvVarGuard::remove("CODEWHALE_DISABLE_CLOUD_FACTS");
        codewhale_config::cloud_facts::overlay::clear();
        clear_live_snapshot();
        crate::provider_catalog_live::reset_cache_for_test();
        CloudFactsTestEnv {
            _enabled: enabled,
            _home: home,
            _home_dir: home_dir,
            _live: live,
        }
    }

    fn publish_test_facts(
        channel: &str,
        version: u64,
        valid_until: Option<u64>,
        models: Vec<codewhale_config::cloud_facts::ModelFact>,
    ) {
        use codewhale_config::cloud_facts::{CloudFactsStatus, ScopedFacts, overlay};
        let ticket = overlay::configure(true, channel).unwrap();
        assert!(overlay::publish(
            &ticket,
            Some(ScopedFacts {
                channel: channel.into(),
                facts_version: version,
                key_id: "cwf-test-only".into(),
                valid_until,
                models,
                ..Default::default()
            }),
            CloudFactsStatus::default()
        ));
    }

    fn upsert_fact(
        provider: &str,
        id: &str,
        context_window: u64,
    ) -> codewhale_config::cloud_facts::ModelFact {
        codewhale_config::cloud_facts::ModelFact {
            provider: provider.into(),
            id: id.into(),
            context_window: Some(context_window),
            ..Default::default()
        }
    }

    /// An id-only unlisted assertion: the signer says this exact id exists on
    /// the provider's official endpoint and states nothing else about it.
    fn attested_fact(provider: &str, id: &str) -> codewhale_config::cloud_facts::ModelFact {
        codewhale_config::cloud_facts::ModelFact {
            provider: provider.into(),
            id: id.into(),
            allow_unlisted: true,
            ..Default::default()
        }
    }

    /// `scoped_view` only keeps an assertion in a payload that expires; mirror
    /// that here so these tests publish what the client can actually receive.
    fn bounded() -> Option<u64> {
        Some(codewhale_config::catalog::now_unix() + 3_600)
    }

    fn record_roster(base: &str, offerings: Vec<CatalogOffering>) {
        use codewhale_config::catalog::ProviderCatalogDelta;
        crate::provider_catalog_live::record_success(ProviderCatalogDelta {
            provider: "deepseek".to_string(),
            base_url_fingerprint: base_url_fingerprint(base),
            fetched_at: codewhale_config::catalog::now_unix(),
            offerings,
        });
    }

    fn roster_row(base: &str, id: &str) -> CatalogOffering {
        CatalogOffering {
            provider: "deepseek".to_string(),
            wire_model_id: id.to_string(),
            endpoint_key: "chat".to_string(),
            source: CatalogSource::Live {
                base_url_fingerprint: base_url_fingerprint(base),
                fetched_at: codewhale_config::catalog::now_unix(),
            },
            ..Default::default()
        }
    }

    /// A provider roster owns the ids it lists **and its own omissions**. This
    /// client keeps no roster history, so nothing it holds locally — bundled or
    /// otherwise — is evidence about what the provider once served: only an
    /// explicit signed assertion may name an id the roster omits, and it does so
    /// for a bundled id and an unknown id alike.
    #[test]
    fn roster_omission_stands_unless_the_payload_explicitly_attests_the_id() {
        let _env = cloud_facts_test_env();

        let provider = ProviderKind::Deepseek;
        let base = provider.provider().default_base_url();
        // One id the bundled catalog knows, one it has never heard of. Neither
        // fact changes what the roster is authoritative about.
        let bundled = "deepseek-v4-flash";
        let unknown = "deepseek-v4-nano-preview";
        let listed = "deepseek-v4-pro";
        assert!(bundled_catalog_offering_for_model(provider, bundled).is_some());
        assert!(bundled_catalog_offering_for_model(provider, unknown).is_none());
        record_roster(base, vec![roster_row(base, listed)]);

        // No assertion: the roster's omission stands for both ids.
        publish_test_facts(
            "roster-dominance-test",
            1,
            bounded(),
            vec![
                upsert_fact("deepseek", bundled, 999_999),
                upsert_fact("deepseek", unknown, 131_072),
            ],
        );
        let models = catalog_models_for_route(provider, "deepseek", base);
        assert!(models.iter().any(|id| id == listed), "{models:?}");
        for id in [bundled, unknown] {
            assert!(
                !models.iter().any(|row| row == id),
                "an unattested patch must not survive the roster's omission: {models:?}"
            );
            assert!(
                catalog_offering_for_route(provider, "deepseek", base, id).is_none(),
                "{id} must not answer with signed facts either"
            );
            assert!(
                !all_catalog_models_for_provider(provider)
                    .iter()
                    .any(|row| row == id),
                "the merged view must not read it back out either"
            );
        }

        // Same ids, now explicitly attested. The bundled one carries no stated
        // limits, so it must not inherit the bundled row's.
        publish_test_facts(
            "roster-dominance-test",
            2,
            bounded(),
            vec![
                attested_fact("deepseek", bundled),
                codewhale_config::cloud_facts::ModelFact {
                    allow_unlisted: true,
                    ..upsert_fact("deepseek", unknown, 131_072)
                },
            ],
        );
        let models = catalog_models_for_route(provider, "deepseek", base);
        let merged = all_catalog_models_for_provider(provider);
        for id in [listed, bundled, unknown] {
            assert!(models.iter().any(|row| row == id), "{models:?}");
            assert!(merged.iter().any(|row| row == id), "{merged:?}");
        }
        let attested = catalog_offering_for_route(provider, "deepseek", base, bundled)
            .expect("an attested id resolves its own facts");
        assert_eq!(
            attested.limit, None,
            "an id-only assertion must not borrow limits from the bundled layer"
        );
        assert_eq!(attested.cost, None);
        assert_eq!(attested.tool_call, None);
        assert_eq!(attested.modalities, None);
        assert_eq!(attested.attachment, None);
        let offering = catalog_offering_for_route(provider, "deepseek", base, unknown)
            .expect("an attested id resolves its own facts");
        assert_eq!(offering.wire_model_id, unknown, "the exact id, verbatim");
        assert_eq!(
            offering.limit.and_then(|limit| limit.context),
            Some(131_072)
        );

        // The executor reads the same list the picker does.
        let config = {
            let __cw_config_init_3_0 = Some("deepseek".into());
            let mut __cw_config_init_3_value: Config = Default::default();
            __cw_config_init_3_value.provider = __cw_config_init_3_0;
            __cw_config_init_3_value
        };
        assert_eq!(
            crate::route_runtime::resolve_runtime_route(&config, provider, Some(unknown))
                .unwrap()
                .candidate
                .limits()
                .context_tokens,
            Some(131_072)
        );

        // Signed rows never reach a proxied endpoint, attested or not.
        assert!(
            !catalog_models_for_route(provider, "deepseek", "https://deepseek-proxy.invalid/v1")
                .iter()
                .any(|id| id == unknown)
        );
    }

    /// Every retraction path is the signer's, and none needs a provider
    /// request: `hide` removes a bundled row, an elapsed validity bound drops
    /// the whole overlay on read (including the hide it carried), and dropping
    /// an upsert withdraws the row it created.
    #[test]
    fn signed_hide_expiry_and_dropped_upsert_retract_rows_without_a_provider_request() {
        use codewhale_config::catalog::now_unix;
        use codewhale_config::cloud_facts::{ModelFact, ModelOp};
        let _env = cloud_facts_test_env();

        let provider = ProviderKind::Deepseek;
        let base = provider.provider().default_base_url();
        let hidden = "deepseek-v4-flash";
        let preview = "deepseek-v4-nano-preview";
        let before = catalog_models_for_route(provider, "deepseek", base);
        assert!(before.iter().any(|id| id == hidden), "{before:?}");

        publish_test_facts(
            "retraction-test",
            1,
            None,
            vec![
                ModelFact {
                    provider: "deepseek".into(),
                    id: hidden.into(),
                    op: ModelOp::Hide,
                    ..Default::default()
                },
                upsert_fact("deepseek", preview, 131_072),
            ],
        );
        let hidden_view = catalog_models_for_route(provider, "deepseek", base);
        assert!(
            !hidden_view.iter().any(|id| id == hidden),
            "hide must remove the bundled row: {hidden_view:?}"
        );
        assert!(
            hidden_view.iter().any(|id| id == preview),
            "{hidden_view:?}"
        );

        // Expiry is evaluated on read: no refresh, no provider request, and no
        // setting change is needed for the payload to stop being authority.
        publish_test_facts(
            "retraction-test",
            2,
            Some(now_unix().saturating_sub(1)),
            vec![
                ModelFact {
                    provider: "deepseek".into(),
                    id: hidden.into(),
                    op: ModelOp::Hide,
                    ..Default::default()
                },
                upsert_fact("deepseek", preview, 131_072),
            ],
        );
        let expired = catalog_models_for_route(provider, "deepseek", base);
        assert!(
            expired.iter().any(|id| id == hidden),
            "an expired payload cannot keep hiding a bundled row: {expired:?}"
        );
        assert!(
            !expired.iter().any(|id| id == preview),
            "an expired payload cannot keep offering its own row: {expired:?}"
        );

        // The third retraction: publish the same channel without the upsert.
        // An attested row is offered past a roster, so this is the path that
        // withdraws one without waiting for `not_after`.
        record_roster(base, vec![roster_row(base, "deepseek-v4-pro")]);
        publish_test_facts(
            "retraction-test",
            3,
            bounded(),
            vec![attested_fact("deepseek", preview)],
        );
        assert!(
            catalog_models_for_route(provider, "deepseek", base)
                .iter()
                .any(|id| id == preview)
        );
        publish_test_facts("retraction-test", 4, bounded(), Vec::new());
        let withdrawn = catalog_models_for_route(provider, "deepseek", base);
        assert!(
            !withdrawn.iter().any(|id| id == preview),
            "dropping the upsert must withdraw the row: {withdrawn:?}"
        );
        assert!(
            withdrawn.iter().any(|id| id == "deepseek-v4-pro"),
            "the roster is untouched by the withdrawal: {withdrawn:?}"
        );
    }

    /// A signed row names one canonical identity on one official endpoint.
    /// Catalog partitions deliberately collapse regional and dual-wire aliases
    /// onto a vendor primary, and that collapse must not become a channel for
    /// facts to reach an endpoint the signer did not name.
    #[test]
    fn signed_rows_do_not_cross_regional_wire_or_proxied_routes() {
        let _env = cloud_facts_test_env();

        let preview = "deepseek-v4-nano-preview";
        let siliconflow_preview = "sf-preview-not-in-any-catalog";
        publish_test_facts(
            "route-scope-test",
            1,
            bounded(),
            vec![
                // Attested: the assertion must not widen the endpoint or
                // identity boundary either.
                codewhale_config::cloud_facts::ModelFact {
                    allow_unlisted: true,
                    pricing: Some(codewhale_config::cloud_facts::PricingFact {
                        input_per_m: Some(0.25),
                        output_per_m: Some(1.0),
                        ..Default::default()
                    }),
                    ..upsert_fact("deepseek", preview, 131_072)
                },
                upsert_fact("siliconflow", siliconflow_preview, 65_536),
            ],
        );
        let offers = |provider: ProviderKind, identity: &str, model: &str| {
            catalog_models_for_route(provider, identity, provider.provider().default_base_url())
                .iter()
                .any(|id| id == model)
        };

        assert!(
            offers(ProviderKind::Deepseek, "deepseek", preview),
            "the exact signed route must offer the row"
        );
        // Same host, but a TUI-only legacy alias with no canonical identity.
        assert!(
            !offers(ProviderKind::Deepseek, "deepseek-cn", preview),
            "the legacy CN alias inherits nothing from the primary identity"
        );
        // Reads the `deepseek` partition, but is a separate endpoint contract.
        assert!(
            !offers(
                ProviderKind::DeepseekAnthropic,
                "deepseek-anthropic",
                preview
            ),
            "the Anthropic-wire endpoint is not the identity the signer named"
        );
        // A regional sibling that shares a catalog partition, not an identity.
        assert!(
            offers(
                ProviderKind::Siliconflow,
                "siliconflow",
                siliconflow_preview
            ),
            "the exact signed SiliconFlow route must offer the row"
        );
        assert!(
            !offers(
                ProviderKind::SiliconflowCN,
                "siliconflow-CN",
                siliconflow_preview
            ),
            "the China endpoint is a different identity, even where the host allowlist overlaps"
        );
        // A proxy or redirect on the right identity is still the wrong endpoint.
        assert!(
            !catalog_models_for_route(
                ProviderKind::Deepseek,
                "deepseek",
                "https://deepseek-proxy.invalid/v1"
            )
            .iter()
            .any(|id| id == preview),
            "a custom base URL never inherits signed rows"
        );

        // The price travels with the row and no further. A signed rate that
        // renders somewhere it cannot be billed is the failure this layer must
        // not have, so the offered row and the dispatch quote answer together.
        let quote = |provider: ProviderKind, identity: &str, base: &str| {
            crate::provider_catalog_live::fresh_dispatch_pricing_quote_at(
                provider,
                identity,
                preview,
                base,
                codewhale_config::catalog::now_unix(),
            )
        };
        assert!(
            quote(
                ProviderKind::Deepseek,
                "deepseek",
                ProviderKind::Deepseek.provider().default_base_url()
            )
            .is_some(),
            "a signed price on the exact signed route is billable — this is what \
             makes a rate change data rather than a release"
        );
        for (provider, identity) in [
            (ProviderKind::Deepseek, "deepseek-cn"),
            (ProviderKind::DeepseekAnthropic, "deepseek-anthropic"),
        ] {
            assert!(
                quote(provider, identity, provider.provider().default_base_url()).is_none(),
                "{identity} must not mint a quote from another endpoint's facts"
            );
        }
        assert!(
            quote(
                ProviderKind::Deepseek,
                "deepseek",
                "https://deepseek-proxy.invalid/v1"
            )
            .is_none(),
            "and neither may a proxied base URL"
        );
        assert!(
            quote(
                ProviderKind::Deepseek,
                "deepseek-custom-table",
                ProviderKind::Deepseek.provider().default_base_url()
            )
            .is_none(),
            "a differently-named provider table is a separate billing relationship"
        );
    }

    /// A roster that answers with ids alone has not said its models have no
    /// limits. Signed facts complete that silence for the picker, the metadata
    /// lookup and the executor alike — and lose every field the provider did
    /// state. A completed price keeps its signed source separately from the
    /// provider roster's existence claim.
    #[test]
    fn provider_id_only_rows_take_signed_limits_while_provider_facts_and_prices_win() {
        use codewhale_config::cloud_facts::{ModelFact, PricingFact};
        use codewhale_config::models_dev::ModelsDevLimit;
        let _env = cloud_facts_test_env();

        let provider = ProviderKind::Deepseek;
        let base = provider.provider().default_base_url();
        let id_only = "deepseek-roster-bare";
        let detailed = "deepseek-roster-detailed";
        record_roster(
            base,
            vec![
                roster_row(base, id_only),
                CatalogOffering {
                    limit: Some(ModelsDevLimit {
                        context: Some(12_345),
                        ..Default::default()
                    }),
                    reasoning: Some(false),
                    ..roster_row(base, detailed)
                },
            ],
        );
        let signed = |id: &str| ModelFact {
            max_output: Some(8_192),
            reasoning: Some(true),
            pricing: Some(PricingFact {
                input_per_m: Some(1.0),
                output_per_m: Some(2.0),
                ..Default::default()
            }),
            ..upsert_fact("deepseek", id, 131_072)
        };
        publish_test_facts(
            "roster-completion-test",
            1,
            bounded(),
            vec![signed(id_only), signed(detailed)],
        );

        let bare = catalog_offering_for_route(provider, "deepseek", base, id_only)
            .expect("the roster row is still there");
        let limit = bare.limit.clone().expect("signed limits complete it");
        assert_eq!(limit.context, Some(131_072));
        assert_eq!(limit.output, Some(8_192));
        assert_eq!(bare.reasoning, Some(true));
        assert!(
            matches!(bare.source, CatalogSource::Live { .. }),
            "the row is still the provider's: {:?}",
            bare.source
        );
        assert_eq!(bare.cost.as_ref().and_then(|cost| cost.input), Some(1.0));
        assert_eq!(bare.cost.as_ref().and_then(|cost| cost.output), Some(2.0));
        assert!(
            matches!(bare.pricing_source(), CatalogSource::CloudFacts { .. }),
            "the estimate must retain its signed price authority"
        );
        assert!(
            crate::provider_catalog_live::fresh_dispatch_pricing_quote_at(
                provider,
                "deepseek",
                id_only,
                base,
                codewhale_config::catalog::now_unix(),
            )
            .is_none(),
            "a completed estimate must not become a provider-confirmed quote"
        );

        let stated = catalog_offering_for_route(provider, "deepseek", base, detailed)
            .expect("the roster row is still there");
        let limit = stated.limit.clone().expect("provider limits are kept");
        assert_eq!(
            limit.context,
            Some(12_345),
            "the provider's own context must win"
        );
        assert_eq!(limit.output, Some(8_192), "only the silence is filled");
        assert_eq!(stated.reasoning, Some(false), "and its own capability wins");

        // The same completion reaches the merged picker view and the executor.
        assert_eq!(
            catalog_offering_for_model(provider, id_only)
                .and_then(|row| row.limit)
                .and_then(|limit| limit.context),
            Some(131_072)
        );
        let config = {
            let __cw_config_init_4_0 = Some("deepseek".into());
            let mut __cw_config_init_4_value: Config = Default::default();
            __cw_config_init_4_value.provider = __cw_config_init_4_0;
            __cw_config_init_4_value
        };
        let limits = |model: &str| {
            crate::route_runtime::resolve_runtime_route(&config, provider, Some(model))
                .unwrap()
                .candidate
                .limits()
                .context_tokens
        };
        assert_eq!(limits(id_only), Some(131_072));
        assert_eq!(limits(detailed), Some(12_345));
    }

    /// The population this layer exists to serve. Most models a user sees are
    /// described by a Models.dev refresh rather than by their provider, and a
    /// stale window or a changed rate on one of those is exactly what a signed
    /// correction must fix — arriving as data, not as a reinstall. The rows are
    /// published through the real producer, so this is also the runtime proof
    /// that external enrichment sits below the signed layer.
    #[test]
    fn signed_facts_correct_models_dev_enrichment_on_the_exact_route() {
        use codewhale_config::cloud_facts::{ModelFact, PricingFact};
        let _env = cloud_facts_test_env();

        let provider = ProviderKind::Deepseek;
        let base = provider.provider().default_base_url();
        let model = "deepseek-enriched-only";
        let body = format!(
            r#"{{
              "models": {{}},
              "providers": {{
                "deepseek": {{
                  "id": "deepseek",
                  "models": {{
                    "{model}": {{
                      "id": "{model}",
                      "modalities": {{ "input": ["text"], "output": ["text"] }},
                      "limit": {{ "context": 65536, "output": 4096 }},
                      "cost": {{ "input": 2.0, "output": 8.0 }}
                    }}
                  }}
                }}
              }}
            }}"#
        );
        let catalog =
            codewhale_config::models_dev::ModelsDevCatalog::parse_json(&body).expect("parse");
        set_live_snapshot(
            CatalogSnapshot {
                offerings: codewhale_config::catalog::live_offerings_from_models_dev(
                    &catalog,
                    codewhale_config::catalog::now_unix(),
                ),
            },
            LiveSource::ModelsDev,
        );

        // A refreshed row describes a model, not an endpoint, so the route
        // resolves it without an endpoint fingerprint to match against.
        let enriched = catalog_offering_for_route(provider, "deepseek", base, model)
            .expect("the enriched row answers on the provider's own endpoint");
        assert_eq!(
            enriched.limit.as_ref().and_then(|limit| limit.context),
            Some(65_536)
        );

        publish_test_facts(
            "models-dev-correction-test",
            1,
            bounded(),
            vec![ModelFact {
                pricing: Some(PricingFact {
                    input_per_m: Some(0.5),
                    output_per_m: Some(1.5),
                    ..Default::default()
                }),
                ..upsert_fact("deepseek", model, 131_072)
            }],
        );

        let corrected = catalog_offering_for_route(provider, "deepseek", base, model)
            .expect("the corrected row is still offered");
        let limit = corrected.limit.clone().expect("limits are kept");
        assert_eq!(
            limit.context,
            Some(131_072),
            "the stale window is corrected"
        );
        assert_eq!(
            limit.output,
            Some(4_096),
            "and only what the payload states is replaced"
        );
        assert_eq!(
            corrected.cost.as_ref().and_then(|cost| cost.input),
            Some(0.5),
            "the signed rate replaces the enriched one"
        );
        assert!(matches!(
            corrected.pricing_source(),
            CatalogSource::CloudFacts { .. }
        ));

        // The executor reads the same correction the picker does.
        let config = {
            let __cw_config_init_5_0 = Some("deepseek".into());
            let mut __cw_config_init_5_value: Config = Default::default();
            __cw_config_init_5_value.provider = __cw_config_init_5_0;
            __cw_config_init_5_value
        };
        assert_eq!(
            crate::route_runtime::resolve_runtime_route(&config, provider, Some(model))
                .unwrap()
                .candidate
                .limits()
                .context_tokens,
            Some(131_072)
        );

        // A corrected rate is only worth rendering where it is billable, and
        // only on the endpoint and identity the signer named.
        let quote = |identity: &str, endpoint: &str| {
            crate::provider_catalog_live::fresh_dispatch_pricing_quote_at(
                provider,
                identity,
                model,
                endpoint,
                codewhale_config::catalog::now_unix(),
            )
        };
        assert!(
            quote("deepseek", base).is_some(),
            "the corrected rate bills on the exact signed route"
        );
        assert!(
            quote("deepseek", "https://deepseek-proxy.invalid/v1").is_none(),
            "a proxied base URL never inherits it"
        );
        assert!(
            quote("deepseek-custom-table", base).is_none(),
            "a differently-named provider table is a separate billing relationship"
        );

        // A fresh roster is still the authority for the ids it lists: it
        // suppresses the enrichment and the correction that rode on it.
        record_roster(base, vec![roster_row(base, "deepseek-v4-pro")]);
        assert!(
            catalog_offering_for_route(provider, "deepseek", base, model).is_none(),
            "an unattested correction cannot survive the roster's omission"
        );
    }

    /// #6705: through the production runtime resolver, a curated Zen
    /// transport row wins over a Models.dev row for the same id, a
    /// catalog-only row routes on the wire its row declares, and a deprecated
    /// catalog row stays unroutable.
    #[test]
    fn opencode_zen_runtime_resolver_keeps_curated_wire_over_models_dev_row() {
        use codewhale_config::models_dev::ModelsDevCatalog;
        use codewhale_config::route::{LogicalModelRef, RequestProtocol, RouteError, RouteRequest};

        let _live = lock_live_snapshot();
        clear_live_snapshot();
        let raw = r#"{
          "providers": {
            "opencode": {
              "id": "opencode",
              "npm": "@ai-sdk/openai-compatible",
              "models": {
                "gpt-5.6-sol": { "id": "gpt-5.6-sol", "provider": { "npm": "@ai-sdk/anthropic" } },
                "claude-opus-9": { "id": "claude-opus-9", "provider": { "npm": "@ai-sdk/anthropic" } },
                "claude-2-retired": {
                  "id": "claude-2-retired",
                  "status": "deprecated",
                  "provider": { "npm": "@ai-sdk/anthropic" }
                }
              }
            }
          }
        }"#;
        let catalog = ModelsDevCatalog::parse_json(raw).expect("fixture parses");
        set_live_snapshot(
            CatalogSnapshot {
                offerings: codewhale_config::catalog::live_offerings_from_models_dev(&catalog, 1),
            },
            LiveSource::ModelsDev,
        );

        let provider = ProviderKind::OpencodeZen;
        let resolver = runtime_catalog_resolver_for_identity(
            provider,
            None,
            provider.provider().default_base_url(),
            CatalogStatus::Unknown,
        )
        .resolver;
        let resolve = |model: &str| {
            resolver.resolve(&RouteRequest {
                explicit_provider: Some(provider),
                model_selector: Some(LogicalModelRef::from(model)),
                saved_provider_model: None,
                base_url_override: None,
                limit_overrides: Vec::new(),
                wire_override: None,
            })
        };

        let curated = resolve("gpt-5.6-sol").expect("curated Zen row resolves");
        assert_eq!(
            curated.protocol(),
            RequestProtocol::Responses,
            "the curated transport row must win over the catalog's conflicting row"
        );
        let catalog_only = resolve("claude-opus-9").expect("catalog-proven Zen row resolves");
        assert_eq!(catalog_only.protocol(), RequestProtocol::AnthropicMessages);
        match resolve("claude-2-retired") {
            Err(RouteError::UnsupportedModelProtocol { endpoint_key, .. }) => {
                assert_eq!(endpoint_key, "deprecated");
            }
            other => panic!("a deprecated Zen row must fail closed, got {other:?}"),
        }

        clear_live_snapshot();
    }
}
