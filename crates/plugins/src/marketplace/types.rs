//! Normalized marketplace catalog model.
//!
//! Every parser (`kimi`, `claude`, `codex`, `codewhale`) funnels foreign
//! catalog entries into [`MarketplaceCandidate`]. Parsers are pure: no
//! network, no filesystem, no process execution. Sources are normalized
//! and mapped onto an install plan, but nothing is fetched here.
//!
//! # Security invariants
//!
//! - Catalog labels (`official`, `curated`, `verified`, `partner`) are
//!   display provenance only. They never grant trust, enablement,
//!   installation, or any runtime permission. Every installed plugin still
//!   enters Codewhale disabled and untrusted and goes through the existing
//!   content/capability hash review.
//! - Foreign `policy` blocks (Codex `INSTALLED_BY_DEFAULT`) never trigger
//!   auto-install; Codewhale only installs on an explicit operator action.
//! - A malformed entry degrades that entry alone; it never hides the rest
//!   of the catalog.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::super::manifest::{PluginCompatibility, PluginInventory, PluginWhen};
use super::super::types::PluginDiagnosticLevel;
use codewhale_config::PluginId;

/// Which real catalog format a document follows.
///
/// Every variant corresponds to a published schema; there is no
/// synthetic or guessed format. `Auto` detection only uses documented
/// structural markers and reports ambiguity instead of guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MarketplaceFormat {
    /// Detect from documented structural markers; ambiguous documents fail.
    #[default]
    Auto,
    /// Codewhale native catalog: `plugins[]` with `name` + install-spec
    /// `source` strings.
    Codewhale,
    /// Kimi / Moonshot `marketplace.json`: `version` + `plugins[]` with
    /// `id`, `displayName`, `tier`, `source` (path / GitHub URL / zip URL).
    Kimi,
    /// Claude `.claude-plugin/marketplace.json`: `name` + `owner` +
    /// `plugins[]`; `source` is a `./`-relative string or a
    /// `{source: github|url|git-subdir|npm|archive|command}` object.
    Claude,
    /// Codex `.agents/plugins/marketplace.json`: `name` + `plugins[]` with
    /// `source` objects (`local`/`url`/`git-subdir`/`npm`) and `policy`.
    Codex,
}

impl MarketplaceFormat {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Codewhale => "codewhale",
            Self::Kimi => "kimi",
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

impl fmt::Display for MarketplaceFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stable identifier for a marketplace catalog (typically its configured
/// name or path alias).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MarketplaceCatalogId(pub String);

impl MarketplaceCatalogId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MarketplaceCatalogId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Stable, deterministic identifier for one catalog entry:
/// `<catalog_id>:<canonical plugin name>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MarketplaceCandidateId(pub String);

impl MarketplaceCandidateId {
    #[must_use]
    pub fn new(catalog_id: &MarketplaceCatalogId, candidate_name: &str) -> Self {
        Self(format!("{}:{candidate_name}", catalog_id.as_str()))
    }

    #[must_use]
    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MarketplaceCandidateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Advisory curation tier carried from the catalog. Display only; never
/// grants trust (see module invariants).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CatalogTier {
    #[default]
    Community,
    Official,
    Curated,
    Partner,
}

impl CatalogTier {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Community => "community",
            Self::Official => "official",
            Self::Curated => "curated",
            Self::Partner => "partner",
        }
    }

    /// Parse a catalog-declared tier string. Unknown strings stay
    /// community-tier with the raw value preserved by the caller's
    /// diagnostic; tiers are display-only either way.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw {
            "official" => Self::Official,
            "curated" => Self::Curated,
            "partner" => Self::Partner,
            _ => Self::Community,
        }
    }
}

impl fmt::Display for CatalogTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Display-only provenance for a catalog or candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CatalogProvenance {
    #[serde(default)]
    pub tier: CatalogTier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

impl CatalogProvenance {
    /// Provenance never grants trust. Constant by construction; exists so
    /// tests and reviewers can assert the invariant at call sites.
    #[must_use]
    pub fn grants_trust(&self) -> bool {
        false
    }
}

/// Normalized description of where a plugin bundle lives. Pure data: the
/// parser never fetches any of these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MarketplaceSourceSpec {
    /// Directory path. May be relative to the catalog's own location;
    /// resolution happens at install time, not parse time.
    LocalPath { path: PathBuf },
    /// GitHub `owner/repo`, optionally with a pinned ref or commit the
    /// current installer cannot yet honor (recorded, warned).
    GitHub {
        owner: String,
        repo: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha: Option<String>,
    },
    /// Tarball archive URL with an optional sha256 pin, verified at install.
    ArchiveUrl {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
    },
    /// Non-GitHub git URL. The installer cannot fetch these yet; the plan
    /// stays [`MarketplaceInstallPlan::Unsupported`].
    GitUrl { url: String },
    /// npm package declaration. Codewhale does not execute npm; the plan
    /// stays unsupported.
    Npm { package: String },
    /// A source form the catalog format defines but Codewhale refuses to
    /// execute (e.g. Claude `command` sources), with the reason.
    Refused { reason: String },
    /// Required source field present but not a documented shape for the
    /// format; entry degrades with an error diagnostic.
    Invalid { reason: String },
}

/// Whether and how Codewhale's existing installer could fetch a source.
/// Unsupported plans are visible, with an honest reason — a listed plugin
/// is never implied to be installable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MarketplaceInstallPlan {
    /// Maps onto a real [`PluginInstallSource`](crate::install::PluginInstallSource) spec.
    Supported { spec: String, source_kind: String },
    /// Cannot be installed by Codewhale today, and why.
    Unsupported { reason: String, raw: String },
}

impl MarketplaceInstallPlan {
    #[must_use]
    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Supported { .. })
    }

    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Supported { .. } => None,
            Self::Unsupported { reason, .. } => Some(reason.as_str()),
        }
    }
}

/// What a catalog entry is.
///
/// A marketplace carries more than plugins: the Codewhale marketplace lists
/// standalone skills as their own entries, and imported third-party catalogs
/// carry whatever their format allows. The kind is how a surface knows which
/// pool an entry belongs to — in particular, only `Plugin` entries are plugin
/// suggestions. (grokbuild's `MarketplaceEntry` carries the same distinction
/// as inventory flags — `skill_count`, `has_mcp`, … — on an always-a-plugin
/// entry; we keep the kind explicit because our catalogs list skills
/// directly.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketplaceEntryKind {
    #[default]
    Plugin,
    /// A standalone skill (instruction pack). Installable, but never
    /// suggested as a plugin.
    Skill,
}

/// A normalized catalog entry. Catalog-declared component lists are kept
/// for display; the reviewed staged-tree manifest at install time remains
/// the only authority on what a bundle actually contains.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketplaceCandidate {
    pub id: MarketplaceCandidateId,
    pub catalog_id: MarketplaceCatalogId,
    /// What this entry is. Defaults to `Plugin` so stored snapshots and
    /// formats that do not declare a kind keep their previous meaning.
    #[serde(default)]
    pub kind: MarketplaceEntryKind,
    /// Canonical (Agent Plugins standard) plugin name.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    pub source: MarketplaceSourceSpec,
    pub install_plan: MarketplaceInstallPlan,
    /// Components the catalog entry itself declares. Many formats declare
    /// none; `None` compatibility then means "decided at install review".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_components: Option<PluginInventory>,
    /// Compatibility of declared components with this build's activation
    /// policy. `None` when the format does not declare components.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<PluginCompatibility>,
    pub provenance: CatalogProvenance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<PluginWhen>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<MarketplaceDiagnostic>,
}

impl MarketplaceCandidate {
    #[must_use]
    pub fn plugin_id(&self) -> PluginId {
        PluginId(self.name.clone())
    }

    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.level == PluginDiagnosticLevel::Error)
    }
}

/// A parsed catalog: survivors plus diagnostics. Fault isolation is
/// per-entry: one malformed entry never removes the others.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketplaceCatalog {
    pub id: MarketplaceCatalogId,
    pub format: MarketplaceFormat,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Resolution context: where the catalog document itself was read
    /// from, so relative sources can be resolved at install time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    pub provenance: CatalogProvenance,
    pub candidates: Vec<MarketplaceCandidate>,
    pub diagnostics: Vec<MarketplaceDiagnostic>,
}

impl MarketplaceCatalog {
    #[must_use]
    pub fn candidate_by_name(&self, name: &str) -> Option<&MarketplaceCandidate> {
        self.candidates.iter().find(|c| c.name == name)
    }

    #[must_use]
    pub fn total_candidates(&self) -> usize {
        self.candidates.len()
    }

    #[must_use]
    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.level == PluginDiagnosticLevel::Error)
            .count()
    }

    #[must_use]
    pub fn warning_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.level == PluginDiagnosticLevel::Warning)
            .count()
    }
}

/// A diagnostic emitted while parsing a catalog or entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplaceDiagnostic {
    pub level: PluginDiagnosticLevel,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_index: Option<usize>,
}

impl MarketplaceDiagnostic {
    #[must_use]
    pub fn warning(
        code: impl Into<String>,
        message: impl Into<String>,
        candidate_name: Option<String>,
        entry_index: Option<usize>,
    ) -> Self {
        Self {
            level: PluginDiagnosticLevel::Warning,
            code: code.into(),
            message: message.into(),
            candidate_name,
            entry_index,
        }
    }

    #[must_use]
    pub fn error(
        code: impl Into<String>,
        message: impl Into<String>,
        candidate_name: Option<String>,
        entry_index: Option<usize>,
    ) -> Self {
        Self {
            level: PluginDiagnosticLevel::Error,
            code: code.into(),
            message: message.into(),
            candidate_name,
            entry_index,
        }
    }
}
