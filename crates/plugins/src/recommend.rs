//! Plugin suggestions for a user task.
//!
//! Ranks installed bundles and locally-added marketplace candidates. A
//! suggestion is never an install, trust, enable, or network side effect.
//!
//! The send-time toast is driven by the declared-keyword matcher
//! (`match_plugin_for_draft`), not by the score below. Scoring only ranks the
//! user-invoked `/plugin suggest` list. Nothing here writes to the model's
//! request: the former `<recommended_plugins>` user-turn block is gone
//! (0.10.1 plugin offering policy, rule 2).

use std::collections::{BTreeMap, BTreeSet};

use codewhale_skill_install::install::{RegistryDocument, RegistryEntry};
use codewhale_skill_install::recommend::recommend_remote_skills;

use super::marketplace::types::MarketplaceCandidate;
use super::registry::PluginRegistry;
use super::types::LoadedPlugin;

const DEFAULT_LIMIT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecommendOptions {
    pub limit: usize,
    pub min_score: usize,
    pub include_active: bool,
}

impl Default for RecommendOptions {
    fn default() -> Self {
        Self {
            limit: DEFAULT_LIMIT,
            min_score: 0,
            include_active: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginMatchSource {
    Installed { id: String },
    Marketplace { catalog_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginNextStep {
    AlreadyActive,
    Trust,
    Enable,
    Inspect,
    MarketplaceInstall {
        catalog_id: String,
    },
    /// `/plugin install` only when a catalog entry carries a real source spec.
    SourceInstall {
        spec: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginTaskRecommendation {
    pub name: String,
    pub source: PluginMatchSource,
    pub matched_terms: Vec<String>,
    pub score: usize,
    pub next_step: PluginNextStep,
}

impl PluginTaskRecommendation {
    #[must_use]
    pub fn command(&self) -> String {
        match &self.next_step {
            PluginNextStep::AlreadyActive | PluginNextStep::Inspect => {
                format!("/plugin show {}", self.name)
            }
            PluginNextStep::Trust => format!("/plugin trust {}", self.name),
            PluginNextStep::Enable => format!("/plugin enable {}", self.name),
            PluginNextStep::MarketplaceInstall { catalog_id } => {
                format!("/plugin marketplace install {catalog_id} {}", self.name)
            }
            PluginNextStep::SourceInstall { spec } => format!("/plugin install {spec}"),
        }
    }
}

/// One matcher-driven candidate for the send-time toast or a model-requested
/// review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginKeywordMatch {
    pub name: String,
    pub matched_term: Option<String>,
    pub id: String,
    pub next_step: PluginNextStep,
    keywords: Vec<String>,
    domains: Vec<String>,
}

impl PluginKeywordMatch {
    #[must_use]
    pub fn command(&self) -> String {
        PluginTaskRecommendation {
            name: self.name.clone(),
            source: match &self.next_step {
                PluginNextStep::MarketplaceInstall { catalog_id } => {
                    PluginMatchSource::Marketplace {
                        catalog_id: catalog_id.clone(),
                    }
                }
                _ => PluginMatchSource::Installed {
                    id: self.id.clone(),
                },
            },
            matched_terms: Vec::new(),
            score: 0,
            next_step: self.next_step.clone(),
        }
        .command()
    }
}

/// Load the bundled first-party catalog and user-added catalogs from the
/// shared store. Browsing never fetches or installs plugin content.
#[must_use]
pub fn load_marketplace_candidates(
    state_path: Option<&std::path::Path>,
) -> Vec<MarketplaceCandidate> {
    let Some(store) = crate::marketplace::store::MarketplaceStore::open(state_path) else {
        return Vec::new();
    };
    let Ok(state) = store.load() else {
        return Vec::new();
    };
    state
        .catalogs()
        .values()
        .flat_map(|catalog| catalog.catalog.candidates.iter().cloned())
        .collect()
}

/// Keyword candidates that can still be reviewed: installed-but-idle plugins
/// and uninstalled catalog entries. Already-active plugins are omitted, and so
/// are:
///
/// - bundled (`PluginScope::Builtin`) plugins, which are never advertised and
///   appear passively in `/plugin list` and Extensions only (policy rule 5);
/// - plugins that cannot run on this machine: an installed bundle whose
///   `when` gate is not met, or a catalog entry whose `when.os` excludes the
///   current OS (policy rule 7).
#[must_use]
pub fn idle_and_catalog_keyword_matches(
    registry: &PluginRegistry,
    marketplace: &[MarketplaceCandidate],
) -> Vec<PluginKeywordMatch> {
    idle_and_catalog_keyword_matches_for_os(registry, marketplace, std::env::consts::OS)
}

/// True when a catalog entry's `when.os` (if any) admits `os`. Binary gates
/// are left to install review: the binary may arrive with the plugin.
fn catalog_os_allows(when: Option<&super::manifest::PluginWhen>, os: &str) -> bool {
    when.and_then(|when| when.os.as_ref())
        .is_none_or(|os_list| os_list.iter().any(|entry| entry.eq_ignore_ascii_case(os)))
}

fn idle_and_catalog_keyword_matches_for_os(
    registry: &PluginRegistry,
    marketplace: &[MarketplaceCandidate],
    os: &str,
) -> Vec<PluginKeywordMatch> {
    let installed = registry.list();
    let installed_names = installed
        .iter()
        .map(|plugin| plugin.name().to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let mut out = Vec::new();
    for plugin in &installed {
        if plugin.active()
            || plugin.scope == super::types::PluginScope::Builtin
            || !plugin.applicable
        {
            continue;
        }
        let next_step = if !plugin.trusted() {
            PluginNextStep::Trust
        } else if !plugin.enabled {
            PluginNextStep::Enable
        } else {
            continue;
        };
        let mut keywords = plugin.manifest.plugin.keywords.clone();
        keywords.push(plugin.name().to_string());
        out.push(PluginKeywordMatch {
            matched_term: None,
            name: plugin.name().to_string(),
            id: plugin.id.as_str().to_string(),
            next_step,
            keywords,
            domains: plugin.inventory.network_hosts.clone(),
        });
    }
    for candidate in marketplace {
        if candidate.has_errors() {
            continue;
        }
        // Only plugins are plugin suggestions (#6290 rework): skill entries
        // are installable, but this pool feeds the composer toast, so a skill
        // must not be dressed as one. This replaces #6274's name suppression, which existed only
        // because the catalog mixed the two kinds.
        if candidate.kind != crate::marketplace::types::MarketplaceEntryKind::Plugin {
            continue;
        }
        if installed_names.contains(&candidate.name.to_ascii_lowercase())
            || !catalog_os_allows(candidate.when.as_ref(), os)
        {
            continue;
        }
        let mut keywords = candidate.keywords.clone();
        keywords.push(candidate.name.clone());
        if let Some(display) = &candidate.display_name {
            keywords.push(display.clone());
        }
        // Categories are deliberately *not* matchable, for the same reason a
        // code-hosting homepage is not (see `matcher::effective_keywords`): a
        // category names the bucket a catalog files the plugin under, not what
        // the plugin is. The bundled catalog buckets read `development`,
        // `productivity`, `design`, `testing`, `security` — ordinary English
        // words, shared by up to 121 entries each — so folding them in made a
        // normal sentence open an unsolicited install prompt. Scored
        // `/plugin suggest` still weighs them (`index_entry_from_marketplace`);
        // that path is user-invoked and ranked, not a proactive interruption.
        let mut domains = Vec::new();
        if let Some(homepage) = &candidate.homepage {
            domains.push(homepage.clone());
        }
        let next_step = match &candidate.install_plan {
            crate::marketplace::types::MarketplaceInstallPlan::Supported { spec, .. }
                if candidate.catalog_id.as_str().is_empty() =>
            {
                PluginNextStep::SourceInstall { spec: spec.clone() }
            }
            _ => PluginNextStep::MarketplaceInstall {
                catalog_id: candidate.catalog_id.as_str().to_string(),
            },
        };
        out.push(PluginKeywordMatch {
            matched_term: None,
            name: candidate.name.clone(),
            id: candidate.id.as_str().to_string(),
            next_step,
            keywords,
            domains,
        });
    }
    out
}

/// One matcher-driven hit for a live draft. Already-active plugins never
/// match. `/plugin install` is returned only when a catalog entry carries a
/// real install spec.
#[must_use]
pub fn match_plugin_for_draft(
    draft: &str,
    registry: &PluginRegistry,
    marketplace: &[MarketplaceCandidate],
    dismissed: &BTreeSet<String>,
) -> Option<PluginKeywordMatch> {
    let mut candidates = idle_and_catalog_keyword_matches(registry, marketplace);
    candidates.retain(|candidate| !dismissed.contains(&candidate.name.to_ascii_lowercase()));
    match_plugin_for_draft_among(draft, &candidates)
}

#[must_use]
pub fn match_plugin_for_draft_among(
    draft: &str,
    candidates: &[PluginKeywordMatch],
) -> Option<PluginKeywordMatch> {
    let keyword_candidates = candidates
        .iter()
        .map(|candidate| crate::matcher::KeywordCandidate {
            name: candidate.name.as_str(),
            domains: &candidate.domains,
            keywords: &candidate.keywords,
        })
        .collect::<Vec<_>>();
    let (idx, term) = crate::matcher::match_plugin_keyword(draft, &keyword_candidates)?;
    let mut matched = candidates.get(idx)?.clone();
    matched.matched_term = Some(term);
    Some(matched)
}

/// Resolve a model-requested plugin name against installed and catalog
/// entries. Fails closed (None) when the name is unknown.
#[must_use]
pub fn lookup_reviewable_plugin(
    name: &str,
    registry: &PluginRegistry,
    marketplace: &[MarketplaceCandidate],
) -> Option<PluginKeywordMatch> {
    let needle = name.trim();
    if needle.is_empty() {
        return None;
    }
    idle_and_catalog_keyword_matches(registry, marketplace)
        .into_iter()
        .find(|candidate| {
            candidate.name.eq_ignore_ascii_case(needle) || candidate.id.eq_ignore_ascii_case(needle)
        })
}

#[must_use]
pub fn recommend_plugins_for_task(
    task: &str,
    registry: &PluginRegistry,
    marketplace: &[MarketplaceCandidate],
    options: RecommendOptions,
) -> Vec<PluginTaskRecommendation> {
    let installed = registry.list();
    let installed_names = installed
        .iter()
        .map(|plugin| plugin.name().to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let mut entries = Vec::new();
    for plugin in &installed {
        entries.push(index_entry_from_installed(plugin));
    }
    for candidate in marketplace {
        if candidate.has_errors() {
            continue;
        }
        if candidate.kind != crate::marketplace::types::MarketplaceEntryKind::Plugin {
            continue;
        }
        if installed_names.contains(&candidate.name.to_ascii_lowercase()) {
            continue;
        }
        entries.push(index_entry_from_marketplace(candidate));
    }
    recommend_from_entries(task, &entries, &installed, options)
}

fn index_entry_from_installed(plugin: &LoadedPlugin) -> (String, RegistryEntry) {
    let mut keywords = plugin.manifest.plugin.keywords.clone();
    keywords.push(plugin.name().to_string());
    let mut description_parts = plugin
        .manifest
        .plugin
        .description
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    for skill in &plugin.skill_snapshots {
        description_parts.push(skill.name.clone());
        description_parts.push(skill.description.clone());
        keywords.push(skill.name.clone());
        keywords.extend(skill.aliases.iter().cloned());
    }
    (
        format!("installed:{}", plugin.name()),
        RegistryEntry {
            source: plugin.id.as_str().to_string(),
            description: (!description_parts.is_empty()).then(|| description_parts.join(" ")),
            keywords,
            domains: plugin.inventory.network_hosts.clone(),
        },
    )
}

fn index_entry_from_marketplace(candidate: &MarketplaceCandidate) -> (String, RegistryEntry) {
    let mut keywords = candidate.keywords.clone();
    keywords.push(candidate.name.clone());
    if let Some(display) = &candidate.display_name {
        keywords.push(display.clone());
    }
    keywords.extend(candidate.categories.iter().cloned());
    (
        format!(
            "marketplace:{}:{}",
            candidate.catalog_id.as_str(),
            candidate.name
        ),
        RegistryEntry {
            source: format!(
                "marketplace:{}:{}",
                candidate.catalog_id.as_str(),
                candidate.name
            ),
            description: candidate.description.clone(),
            keywords,
            domains: Vec::new(),
        },
    )
}

fn recommend_from_entries(
    task: &str,
    entries: &[(String, RegistryEntry)],
    installed: &[&LoadedPlugin],
    options: RecommendOptions,
) -> Vec<PluginTaskRecommendation> {
    if options.limit == 0 {
        return Vec::new();
    }
    let index = RegistryDocument {
        skills: entries.iter().cloned().collect::<BTreeMap<_, _>>(),
    };
    let ranked = recommend_remote_skills(task, &index, options.limit.saturating_mul(2));
    let mut out = Vec::new();
    let mut seen_names = BTreeSet::new();
    for recommendation in ranked {
        if recommendation.score() < options.min_score {
            continue;
        }
        let (source, name, next_step) =
            match recommendation.entry.source.strip_prefix("marketplace:") {
                Some(rest) => {
                    let Some((catalog_id, name)) = rest.split_once(':') else {
                        continue;
                    };
                    (
                        PluginMatchSource::Marketplace {
                            catalog_id: catalog_id.to_string(),
                        },
                        name.to_string(),
                        PluginNextStep::MarketplaceInstall {
                            catalog_id: catalog_id.to_string(),
                        },
                    )
                }
                None => {
                    let Some(plugin) = installed
                        .iter()
                        .find(|plugin| plugin.id.as_str() == recommendation.entry.source)
                    else {
                        continue;
                    };
                    let next_step = if plugin.active() {
                        PluginNextStep::AlreadyActive
                    } else if !plugin.trusted() {
                        PluginNextStep::Trust
                    } else if !plugin.enabled {
                        PluginNextStep::Enable
                    } else {
                        PluginNextStep::Inspect
                    };
                    (
                        PluginMatchSource::Installed {
                            id: plugin.id.as_str().to_string(),
                        },
                        plugin.name().to_string(),
                        next_step,
                    )
                }
            };
        if !options.include_active && next_step == PluginNextStep::AlreadyActive {
            continue;
        }
        let name_key = name.to_ascii_lowercase();
        if !seen_names.insert(name_key) {
            continue;
        }
        out.push(PluginTaskRecommendation {
            name,
            source,
            matched_terms: recommendation.matched_terms.clone(),
            score: recommendation.score(),
            next_step,
        });
        if out.len() >= options.limit {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::types::{
        CatalogProvenance, CatalogTier, MarketplaceCandidate, MarketplaceCandidateId,
        MarketplaceCatalogId, MarketplaceEntryKind, MarketplaceInstallPlan, MarketplaceSourceSpec,
    };
    use codewhale_runtime::test_support::{EnvVarGuard, lock_test_env};
    use std::fs;
    use tempfile::TempDir;

    fn write_keyword_bundle(
        root: &std::path::Path,
        name: &str,
        description: &str,
        keywords: &[&str],
    ) {
        let bundle = root.join(".codewhale/plugins").join(name);
        fs::create_dir_all(&bundle).unwrap();
        let keyword_list = keywords
            .iter()
            .map(|keyword| format!("\"{keyword}\""))
            .collect::<Vec<_>>()
            .join(", ");
        fs::write(
            bundle.join("plugin.toml"),
            format!(
                "schema_version = 1\n[plugin]\nname = \"{name}\"\nversion = \"1.0.0\"\ndescription = \"{description}\"\nkeywords = [{keyword_list}]\n"
            ),
        )
        .unwrap();
    }

    fn marketplace_candidate(catalog: &str, name: &str, keywords: &[&str]) -> MarketplaceCandidate {
        MarketplaceCandidate {
            id: MarketplaceCandidateId::new(&MarketplaceCatalogId::new(catalog), name),
            catalog_id: MarketplaceCatalogId::new(catalog),
            kind: MarketplaceEntryKind::Plugin,
            name: name.to_string(),
            display_name: Some(format!("{name} plugin")),
            icon: None,
            description: Some(format!("{name} integration")),
            version: None,
            author: None,
            homepage: None,
            repository: None,
            license: None,
            keywords: keywords.iter().map(|value| (*value).to_string()).collect(),
            categories: Vec::new(),
            source: MarketplaceSourceSpec::GitHub {
                owner: "example".to_string(),
                repo: name.to_string(),
                git_ref: None,
                sha: None,
            },
            install_plan: MarketplaceInstallPlan::Supported {
                spec: format!("github:example/{name}"),
                source_kind: "github".to_string(),
            },
            declared_components: None,
            compatibility: None,
            provenance: CatalogProvenance {
                tier: CatalogTier::Community,
                publisher: None,
                source_url: None,
            },
            when: None,
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn keyword_match_ranks_installed_supabase_plugin() {
        let _lock = lock_test_env();
        let root = TempDir::new().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
        write_keyword_bundle(
            root.path(),
            "supabase",
            "Hosted Postgres and auth",
            &["supabase", "postgres"],
        );
        let registry =
            crate::PluginDiscoveryContext::capture_pre_dotenv().registry_for_workspace(root.path());

        let recs = recommend_plugins_for_task(
            "add supabase auth to this app",
            &registry,
            &[],
            RecommendOptions::default(),
        );
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].name, "supabase");
        assert!(recs[0].score > 0);
        assert_eq!(recs[0].next_step, PluginNextStep::Trust);
        assert_eq!(recs[0].command(), "/plugin trust supabase");
    }

    #[test]
    fn marketplace_fills_in_a_missing_plugin() {
        let _lock = lock_test_env();
        let root = TempDir::new().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
        let registry =
            crate::PluginDiscoveryContext::capture_pre_dotenv().registry_for_workspace(root.path());
        let catalog = [marketplace_candidate("official", "supabase", &["supabase"])];

        let recs = recommend_plugins_for_task(
            "wire up supabase row level security",
            &registry,
            &catalog,
            RecommendOptions::default(),
        );
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].name, "supabase");
        assert_eq!(
            recs[0].next_step,
            PluginNextStep::MarketplaceInstall {
                catalog_id: "official".to_string()
            }
        );
        assert_eq!(
            recs[0].command(),
            "/plugin marketplace install official supabase"
        );
    }

    #[test]
    fn already_active_plugins_are_skipped_when_active_excluded() {
        let _lock = lock_test_env();
        let root = TempDir::new().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
        write_keyword_bundle(root.path(), "supabase", "Hosted Postgres", &["supabase"]);
        let mut registry = crate::PluginDiscoveryContext::capture_pre_dotenv()
            .registry_for_workspace(root.path())
            .as_ref()
            .clone();
        registry.trust("supabase").unwrap();
        registry.enable("supabase").unwrap();

        let recs = recommend_plugins_for_task(
            "add supabase auth",
            &registry,
            &[],
            RecommendOptions {
                include_active: false,
                ..RecommendOptions::default()
            },
        );
        assert!(recs.is_empty(), "{recs:?}");
    }

    /// Policy rule 5: a bundled plugin is never advertised, however well its
    /// keywords match; it stays visible in `/plugin list` and Extensions.
    #[test]
    fn builtin_plugins_are_never_suggested() {
        let root = TempDir::new().unwrap();
        let config = crate::discovery::DiscoveryConfig {
            workspace: root.path().join("project"),
            user_plugins_dir: root.path().join("user"),
            workspace_plugins_dir: root.path().join("workspace"),
            builtin_plugin_dirs: vec![root.path().join("builtin")],
            state_path: root.path().join("state.json"),
        };
        let bundle = root.path().join("builtin/computer-use");
        fs::create_dir_all(&bundle).unwrap();
        fs::write(
            bundle.join("plugin.toml"),
            "schema_version = 1\n[plugin]\nname = \"computer-use\"\nversion = \"1.0.0\"\nkeywords = [\"accessibility\", \"screenshot\", \"desktop control\"]\n",
        )
        .unwrap();
        let registry = crate::discovery::discover_with_config(&config);
        let plugin = registry.get("computer-use").expect("builtin discovered");
        assert_eq!(plugin.scope, crate::types::PluginScope::Builtin);
        assert!(!plugin.active(), "fixture must be idle to prove the skip");

        let catalog = [marketplace_candidate(
            "official",
            "computer-use",
            &["desktop control"],
        )];
        assert!(idle_and_catalog_keyword_matches(&registry, &catalog).is_empty());
        for draft in [
            "improve accessibility",
            "take a screenshot",
            "fix the accessibility of the login form",
            "use desktop control to click the button",
        ] {
            assert_eq!(
                match_plugin_for_draft(draft, &registry, &catalog, &BTreeSet::new()),
                None,
                "{draft}"
            );
        }
        assert!(lookup_reviewable_plugin("computer-use", &registry, &catalog).is_none());
    }

    /// Policy rule 6: generic words never trigger an offer, even for a
    /// non-bundled plugin that declares them.
    #[test]
    fn generic_words_do_not_suggest_an_installed_plugin() {
        let _lock = lock_test_env();
        let root = TempDir::new().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
        write_keyword_bundle(
            root.path(),
            "chromewhale",
            "Codewhale in your own Chrome",
            &["chrome", "browser", "extension", "side-panel", "web"],
        );
        let registry =
            crate::PluginDiscoveryContext::capture_pre_dotenv().registry_for_workspace(root.path());
        let catalog = [marketplace_candidate(
            "official",
            "screen-tools",
            &["accessibility", "screenshot", "automation"],
        )];
        for draft in [
            "improve accessibility",
            "take a screenshot",
            "open chrome and check the web page",
            "write a browser extension",
            "add automation to the docs site",
        ] {
            assert_eq!(
                match_plugin_for_draft(draft, &registry, &catalog, &BTreeSet::new()),
                None,
                "{draft}"
            );
        }
        // Specific terms still work.
        assert_eq!(
            match_plugin_for_draft("open the side-panel", &registry, &catalog, &BTreeSet::new())
                .map(|matched| matched.name),
            Some("chromewhale".to_string())
        );
    }

    /// Policy rule 7: only offer what can run here.
    #[test]
    fn plugins_for_another_os_are_not_suggested() {
        let _lock = lock_test_env();
        let root = TempDir::new().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
        let registry = crate::PluginRegistry::empty(root.path());
        let mut mac_only = marketplace_candidate("official", "mac-control", &["mac control"]);
        mac_only.when = Some(crate::manifest::PluginWhen {
            os: Some(vec!["macos".to_string()]),
            binaries: None,
        });
        let catalog = std::slice::from_ref(&mac_only);
        assert!(idle_and_catalog_keyword_matches_for_os(&registry, catalog, "linux").is_empty());
        assert!(idle_and_catalog_keyword_matches_for_os(&registry, catalog, "windows").is_empty());
        assert_eq!(
            idle_and_catalog_keyword_matches_for_os(&registry, catalog, "macos").len(),
            1,
            "control: the same entry is offered on macOS"
        );

        // An installed bundle whose `when` gate fails here is not offered.
        let bundle = root.path().join(".codewhale/plugins/elsewhere");
        fs::create_dir_all(&bundle).unwrap();
        let other_os = if cfg!(target_os = "windows") {
            "linux"
        } else {
            "windows"
        };
        fs::write(
            bundle.join("plugin.toml"),
            format!(
                "schema_version = 1\n[plugin]\nname = \"elsewhere\"\nversion = \"1.0.0\"\nkeywords = [\"elsewhere\"]\n[when]\nos = [\"{other_os}\"]\n"
            ),
        )
        .unwrap();
        let registry =
            crate::PluginDiscoveryContext::capture_pre_dotenv().registry_for_workspace(root.path());
        let plugin = registry.get("elsewhere").expect("bundle discovered");
        assert!(!plugin.applicable);
        assert_eq!(
            match_plugin_for_draft("run elsewhere", &registry, &[], &BTreeSet::new()),
            None
        );
    }

    /// A skill entry in a catalog is installable but is never a plugin
    /// suggestion — the structural replacement for #6274's name suppression.
    #[test]
    fn skill_entries_never_enter_the_plugin_suggestion_pool() {
        let _lock = lock_test_env();
        let root = TempDir::new().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
        let registry = crate::PluginRegistry::empty(root.path());
        let mut skill = marketplace_candidate("cw2", "test", &["test"]);
        skill.kind = MarketplaceEntryKind::Skill;
        let slice = std::slice::from_ref(&skill);
        assert!(
            idle_and_catalog_keyword_matches(&registry, slice).is_empty(),
            "a skill entry must not be a plugin candidate"
        );

        // Control: the same entry as a plugin is a candidate, so the
        // exclusion is the kind and not a broken fixture.
        skill.kind = MarketplaceEntryKind::Plugin;
        assert_eq!(
            idle_and_catalog_keyword_matches(&registry, std::slice::from_ref(&skill)).len(),
            1,
            "the same entry as a plugin is a candidate"
        );
    }

    /// A catalog category names the bucket an entry is filed under, not what
    /// the plugin *is* — the same class of thing as a code-hosting homepage,
    /// which the matcher already refuses. Folding categories into the
    /// matchable keyword set made ordinary English in a draft ("productivity",
    /// "development") open an unsolicited install prompt.
    #[test]
    fn catalog_categories_never_fire_a_plugin_suggestion() {
        let _lock = lock_test_env();
        let root = TempDir::new().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
        let registry = crate::PluginRegistry::empty(root.path());
        let mut candidate = marketplace_candidate("anthropic", "receipts", &["expense"]);
        candidate.display_name = None;
        candidate.categories = vec!["productivity".to_string(), "development".to_string()];
        let slice = std::slice::from_ref(&candidate);
        let candidates = idle_and_catalog_keyword_matches(&registry, slice);
        assert_eq!(candidates.len(), 1, "fixture must supply one candidate");

        for draft in [
            "some notes on productivity today",
            "walk me through the development workflow",
        ] {
            assert!(
                match_plugin_for_draft_among(draft, &candidates).is_none(),
                "a category must not fire an install prompt: {draft}"
            );
        }

        // Control: the declared keyword and the entry name still match, so the
        // exclusion is the category and not a dead fixture.
        for (draft, term) in [
            ("track this expense", "expense"),
            ("open receipts", "receipts"),
        ] {
            let matched = match_plugin_for_draft_among(draft, &candidates)
                .unwrap_or_else(|| panic!("declared term must still match: {draft}"));
            assert_eq!(matched.name, "receipts");
            assert_eq!(matched.matched_term.as_deref(), Some(term));
        }
    }

    #[test]
    fn matcher_driven_cta_skips_already_active_plugins() {
        let _lock = lock_test_env();
        let root = TempDir::new().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
        write_keyword_bundle(root.path(), "supabase", "Hosted Postgres", &["supabase"]);
        let mut registry = crate::PluginDiscoveryContext::capture_pre_dotenv()
            .registry_for_workspace(root.path())
            .as_ref()
            .clone();
        registry.trust("supabase").unwrap();
        registry.enable("supabase").unwrap();

        assert!(
            match_plugin_for_draft("add supabase auth", &registry, &[], &BTreeSet::new()).is_none(),
            "active plugins must not produce a live CTA"
        );
    }
}
