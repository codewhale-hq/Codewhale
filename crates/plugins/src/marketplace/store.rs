//! Persistence for locally-added marketplace catalogs (#5311).
//!
//! Catalogs live in one sibling JSON file next to the plugin registry's
//! `state.json`, under the same Codewhale-owned `plugins/` root, and reuse the
//! registry's audited persistence machinery: private parent directory, atomic
//! temp-file publication, no-follow opens, and an `fd_lock` sidecar lock so a
//! concurrent TUI cannot interleave `add`/`remove` with a read.
//!
//! This is deliberately not a second database — it is the same store pattern
//! with a different schema, and it hard-fails closed on a malformed file
//! rather than rewriting it.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::types::{MarketplaceCatalog, MarketplaceCatalogId};
use crate::registry::{
    ensure_private_plugin_state_directory, harden_plugin_state_file, open_existing_regular_file,
    open_state_lock, path_entry_exists, save_state_with_hardener, state_lock_path,
    validate_existing_plugin_state_parent,
};

const MARKETPLACE_SCHEMA_VERSION: u32 = 1;
const MARKETPLACE_STATE_FILE: &str = "marketplaces.json";

/// One stored catalog: the parsed result plus where the document was read
/// from, so relative sources resolve against the catalog's own location at
/// install time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMarketplaceCatalog {
    pub added_at: String,
    /// Absolute path the catalog document was read from.
    pub source_path: String,
    pub catalog: MarketplaceCatalog,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceState {
    schema_version: u32,
    #[serde(default)]
    first_party_removed: bool,
    #[serde(default)]
    catalogs: BTreeMap<String, StoredMarketplaceCatalog>,
}

impl Default for MarketplaceState {
    fn default() -> Self {
        Self {
            schema_version: MARKETPLACE_SCHEMA_VERSION,
            first_party_removed: false,
            catalogs: BTreeMap::new(),
        }
    }
}

impl MarketplaceState {
    #[must_use]
    pub fn catalogs(&self) -> &BTreeMap<String, StoredMarketplaceCatalog> {
        &self.catalogs
    }

    pub fn get(&self, name: &str) -> Option<&StoredMarketplaceCatalog> {
        self.catalogs.get(name)
    }
}

/// Handle on the sibling catalog store. `None` from [`MarketplaceStore::open`]
/// means the registry has no persistence root at all (fail-closed registry);
/// callers report that honestly instead of inventing a location.
pub struct MarketplaceStore {
    path: PathBuf,
}

impl MarketplaceStore {
    /// Open the store that siblings the registry's `state.json`.
    #[must_use]
    pub fn open(state_path: Option<&Path>) -> Option<Self> {
        let parent = state_path?.parent()?.to_path_buf();
        Some(Self {
            path: parent.join(MARKETPLACE_STATE_FILE),
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read all catalogs. Never writes; never creates the file or its lock.
    pub fn load(&self) -> Result<MarketplaceState, String> {
        validate_existing_plugin_state_parent(&self.path)?;
        let lock_path = state_lock_path(&self.path);
        let mut state = if path_entry_exists(&lock_path)? {
            let lock_file = open_state_lock(&lock_path, false)?;
            let lock = fd_lock::RwLock::new(lock_file);
            let _guard = lock
                .read()
                .map_err(|e| format!("failed to read-lock marketplace state: {e}"))?;
            self.load_unlocked()?
        } else {
            self.load_unlocked()?
        };
        // Every surface consumes this same projection. Browsing is offline
        // and read-only; installation and updates use the reviewed installer.
        // A user's local catalog or removal always takes precedence.
        if !state.first_party_removed && !state.catalogs.contains_key("codewhale") {
            state
                .catalogs
                .insert("codewhale".into(), first_party_catalog()?);
        }
        Ok(state)
    }

    /// Maximum bytes read from the marketplace state file.
    const MAX_STATE_BYTES: u64 = 1024 * 1024;

    fn load_unlocked(&self) -> Result<MarketplaceState, String> {
        let Some(file) = open_existing_regular_file(&self.path, false)? else {
            return Ok(MarketplaceState::default());
        };
        let mut raw = String::new();
        file.take(Self::MAX_STATE_BYTES + 1)
            .read_to_string(&mut raw)
            .map_err(|e| format!("failed to read {}: {e}", self.path.display()))?;
        if raw.len() as u64 > Self::MAX_STATE_BYTES {
            return Err(format!(
                "marketplace state {} exceeds the 1 MiB limit",
                self.path.display()
            ));
        }
        let state: MarketplaceState = serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse {}: {e}", self.path.display()))?;
        if state.schema_version != MARKETPLACE_SCHEMA_VERSION {
            return Err(format!(
                "unsupported marketplace state schema {}; expected {MARKETPLACE_SCHEMA_VERSION} at {}",
                state.schema_version,
                self.path.display()
            ));
        }
        Ok(collapse_same_source_catalogs(state))
    }

    /// Insert a catalog under `id`, keyed by its source document.
    ///
    /// A catalog *is* its source: re-adding the same document updates that
    /// catalog in place — and renames it to the requested `id` when the name
    /// differs — instead of colliding on whatever name the previous add chose.
    /// That collision is what produced a second, stale snapshot of the
    /// codewhale marketplace under a hand-made name ("cw2") sitting beside
    /// `codewhale`, two copies of one source. An existing *different* source
    /// under `id` still refuses, so a name never silently re-points.
    pub fn add(
        &self,
        id: &MarketplaceCatalogId,
        entry: StoredMarketplaceCatalog,
    ) -> Result<(), String> {
        self.mutate(|state| {
            let source = canonical_source_path(&entry.source_path);
            if let Some(existing) = state.catalogs.get(id.as_str())
                && canonical_source_path(&existing.source_path) != source
            {
                return Err(format!(
                    "a marketplace named `{}` already exists; /plugin marketplace remove {} first",
                    id.as_str(),
                    id.as_str()
                ));
            }
            let duplicates: Vec<String> = state
                .catalogs
                .iter()
                .filter(|(key, catalog)| {
                    key.as_str() != id.as_str()
                        && canonical_source_path(&catalog.source_path) == source
                })
                .map(|(key, _)| key.clone())
                .collect();
            for key in duplicates {
                state.catalogs.remove(&key);
            }
            state.catalogs.insert(id.as_str().to_string(), entry);
            Ok(())
        })
    }

    /// Remove a catalog by name. `Ok(false)` means it was not stored.
    pub fn remove(&self, name: &str) -> Result<bool, String> {
        self.mutate(|state| {
            let removed = state.catalogs.remove(name).is_some();
            let bundled = name == "codewhale" && !state.first_party_removed;
            if name == "codewhale" {
                state.first_party_removed = true;
            }
            Ok(removed || bundled)
        })
    }

    fn mutate<R>(
        &self,
        mutate: impl FnOnce(&mut MarketplaceState) -> Result<R, String>,
    ) -> Result<R, String> {
        let lock_path = state_lock_path(&self.path);
        if let Some(parent) = lock_path.parent() {
            ensure_private_plugin_state_directory(parent)?;
        }
        let lock_file = open_state_lock(&lock_path, true)?;
        let mut lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock
            .write()
            .map_err(|e| format!("failed to lock marketplace state for update: {e}"))?;
        let mut next = self.load_unlocked()?;
        let result = mutate(&mut next)?;
        save_state_with_hardener(&self.path, &next, harden_plugin_state_file)?;
        Ok(result)
    }
}

/// Canonical form of a catalog's source document, for source-identity
/// comparison. This is the identity the store keys updates and duplicate
/// detection on (the marketplace sibling of grokbuild's
/// `MarketplaceSource::identity`). Falls back to the stored string when the
/// path is not readable as a file — a GitHub URL, or a document that has
/// since moved away.
///
/// The `std::fs` site below carries a budget entry in
/// `scripts/check-blocking-calls-budget.json`: the ratchet counts a
/// synchronous helper's site regardless of its callers, and this helper
/// cannot move to the blocking pool without restructuring the store's
/// synchronous `load`/`add`/`remove` API (#6149).
///
/// Known limitation, recorded because the ratchet cannot see it: the scanner
/// is lexical and per-file, so it does not notice that
/// `runtime_api::plugins` calls [`MarketplaceStore::load`]/`add`/`remove`
/// inline from async route handlers. The budget entry is therefore debt
/// acknowledged, not debt paid.
fn canonical_source_path(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}

/// Collapse catalogs that point at the same source document.
///
/// Older states can hold one marketplace twice under two names (that is what
/// `"cw2"` was: a second snapshot of the same document, taken only because the
/// first name was already in use). Every surface should see one catalog per
/// source. The survivor is the entry whose key is the document's own declared
/// name when one exists, otherwise the most recently added; the projection is
/// in-memory, and the next write persists it.
fn collapse_same_source_catalogs(mut state: MarketplaceState) -> MarketplaceState {
    let mut groups: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for (key, catalog) in &state.catalogs {
        groups
            .entry(canonical_source_path(&catalog.source_path))
            .or_default()
            .push(key.clone());
    }
    for keys in groups.values() {
        if keys.len() < 2 {
            continue;
        }
        let named_after_document = keys.iter().find(|key| {
            state
                .catalogs
                .get(key.as_str())
                .is_some_and(|catalog| catalog.catalog.name == key.as_str())
        });
        let keeper = named_after_document.cloned().or_else(|| {
            keys.iter()
                .max_by(|left, right| {
                    let added = |key: &String| {
                        state
                            .catalogs
                            .get(key.as_str())
                            .map(|catalog| catalog.added_at.as_str())
                            .unwrap_or_default()
                    };
                    added(left).cmp(added(right))
                })
                .cloned()
        });
        let Some(keeper) = keeper else { continue };
        for key in keys {
            if *key != keeper {
                state.catalogs.remove(key);
            }
        }
    }
    state
}

fn first_party_catalog() -> Result<StoredMarketplaceCatalog, String> {
    #[derive(Deserialize)]
    struct Snapshot {
        repository: String,
        revision: String,
        catalog: serde_json::Value,
    }
    let snapshot: Snapshot =
        serde_json::from_str(include_str!("../../assets/first-party-marketplace.json"))
            .map_err(|error| format!("invalid bundled marketplace: {error}"))?;
    let source = format!("{}/tree/{}", snapshot.repository, snapshot.revision);
    let mut catalog = super::parsers::parse_catalog(super::parsers::MarketplaceDocument {
        catalog_id: MarketplaceCatalogId::new("codewhale"),
        format: super::types::MarketplaceFormat::Codewhale,
        root: snapshot.catalog,
        base: Some(source.clone()),
    });
    if catalog.error_count() > 0 {
        return Err("bundled marketplace contains invalid entries".into());
    }
    catalog.provenance.tier = super::types::CatalogTier::Official;
    catalog.provenance.source_url = Some(source.clone());
    for candidate in &mut catalog.candidates {
        candidate.provenance = catalog.provenance.clone();
    }
    Ok(StoredMarketplaceCatalog {
        added_at: String::new(),
        source_path: source,
        catalog,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn bundled_catalog_is_offline_installable_and_removable_without_rewriting_plugin_state() {
        let root = tempfile::tempdir().unwrap();
        let state_path = root.path().join("plugins/state.json");
        let store = MarketplaceStore::open(Some(&state_path)).unwrap();
        let initial = store.load().unwrap();
        let catalog = initial.get("codewhale").expect("first-party catalog");
        assert_eq!(catalog.catalog.total_candidates(), 19);
        assert_eq!(catalog.catalog.error_count(), 0);
        assert_eq!(catalog.catalog.warning_count(), 0);
        assert!(!catalog.catalog.provenance.grants_trust());
        let registry = crate::PluginRegistry::empty(root.path());
        for candidate in &catalog.catalog.candidates {
            assert!(candidate.install_plan.is_supported());
            assert!(!candidate.provenance.grants_trust());
            let super::super::document::CatalogInstallResolution::Supported { spec, .. } =
                super::super::document::resolve_candidate_install(catalog, candidate, &registry)
            else {
                panic!("uninstallable candidate")
            };
            if candidate.name == "computer-use" {
                assert_eq!(
                    spec,
                    "https://github.com/codewhale-hq/codewhale-cu-plugin/releases/latest/download/computer-use.tar.gz"
                );
            } else {
                assert!(spec.starts_with(
                    "https://codeload.github.com/codewhale-hq/codewhale-plugin-marketplace/"
                ));
                assert!(spec.contains("#path="));
            }
        }
        assert!(!store.path().exists(), "browsing must not write or fetch");
        assert!(store.remove("codewhale").unwrap());
        assert!(!store.remove("codewhale").unwrap());
        assert!(store.load().unwrap().catalogs().is_empty());
        assert!(
            !state_path.exists(),
            "catalog removal must not change plugin trust state"
        );
        let mut local = first_party_catalog().unwrap();
        local.source_path = "/reviewed/local/marketplace.json".into();
        store
            .add(&MarketplaceCatalogId::new("codewhale"), local)
            .unwrap();
        assert_eq!(
            store.load().unwrap().get("codewhale").unwrap().source_path,
            "/reviewed/local/marketplace.json"
        );
    }

    #[test]
    fn malformed_store_is_not_hidden_by_bundled_catalog() {
        let root = tempfile::tempdir().unwrap();
        let store = MarketplaceStore::open(Some(&root.path().join("state.json"))).unwrap();
        fs::write(store.path(), "broken").unwrap();
        assert!(store.load().is_err());
        assert_eq!(fs::read_to_string(store.path()).unwrap(), "broken");
    }

    fn catalog_from_source(source: &Path) -> StoredMarketplaceCatalog {
        let mut catalog = first_party_catalog().unwrap();
        catalog.source_path = source.display().to_string();
        catalog
    }

    /// `add` keys on the source document, not the name the previous add used:
    /// re-adding the same marketplace under its real name replaces the
    /// hand-made duplicate instead of colliding or leaving both behind.
    #[test]
    fn re_adding_the_same_source_renames_in_place() {
        let root = tempfile::tempdir().unwrap();
        let store = MarketplaceStore::open(Some(&root.path().join("plugins/state.json"))).unwrap();
        let document = root.path().join("marketplace.json");
        fs::write(&document, "{}").unwrap();

        store
            .add(
                &MarketplaceCatalogId::new("cw2"),
                catalog_from_source(&document),
            )
            .unwrap();
        store
            .add(
                &MarketplaceCatalogId::new("codewhale"),
                catalog_from_source(&document),
            )
            .unwrap();

        let state = store.load().unwrap();
        assert!(state.get("cw2").is_none(), "the duplicate name is gone");
        assert_eq!(
            state.get("codewhale").unwrap().source_path,
            document.display().to_string()
        );
    }

    /// A name is never silently re-pointed at a different source.
    #[test]
    fn a_name_never_re_points_at_a_different_source() {
        let root = tempfile::tempdir().unwrap();
        let store = MarketplaceStore::open(Some(&root.path().join("plugins/state.json"))).unwrap();
        let first = root.path().join("first.json");
        let second = root.path().join("second.json");
        fs::write(&first, "{}").unwrap();
        fs::write(&second, "{}").unwrap();

        store
            .add(
                &MarketplaceCatalogId::new("codewhale"),
                catalog_from_source(&first),
            )
            .unwrap();
        let refused = store
            .add(
                &MarketplaceCatalogId::new("codewhale"),
                catalog_from_source(&second),
            )
            .expect_err("a different source under a taken name must refuse");
        assert!(refused.contains("already exists"), "{refused}");
        assert_eq!(
            store.load().unwrap().get("codewhale").unwrap().source_path,
            first.display().to_string()
        );
    }

    /// A state written before source identity was keyed can hold one source
    /// twice under two names; the projection collapses it to the entry named
    /// after the document itself.
    #[test]
    fn load_collapses_one_source_stored_under_two_names() {
        let root = tempfile::tempdir().unwrap();
        let store = MarketplaceStore::open(Some(&root.path().join("plugins/state.json"))).unwrap();
        let document = root.path().join("marketplace.json");
        fs::write(&document, "{}").unwrap();

        let mut state = MarketplaceState::default();
        for (key, added_at) in [
            ("codewhale", "2026-09-02T00:00:00Z"),
            ("cw2", "2026-09-16T00:00:00Z"),
        ] {
            let mut catalog = catalog_from_source(&document);
            catalog.added_at = added_at.to_string();
            state.catalogs.insert(key.to_string(), catalog);
        }
        ensure_private_plugin_state_directory(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), serde_json::to_string(&state).unwrap()).unwrap();

        let loaded = store.load().unwrap();
        assert!(loaded.get("cw2").is_none());
        assert!(loaded.get("codewhale").is_some());
        assert_eq!(
            loaded.get("codewhale").unwrap().source_path,
            document.display().to_string()
        );
    }
}
