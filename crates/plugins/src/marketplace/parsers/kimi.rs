//! Kimi / Moonshot `marketplace.json` parser.
//!
//! Schema source: `MoonshotAI/kimi-code/plugins/marketplace.json` and
//! `docs/en/customization/plugins.md` ("Custom marketplace JSON"):
//!
//! ```json
//! {
//!   "version": "2",
//!   "plugins": [
//!     { "id": "my-plugin", "displayName": "My Plugin", "source": "./my-plugin" }
//!   ]
//! }
//! ```
//!
//! Entry fields: `id` (required), `source` (required: local path, zip
//! URL, or GitHub URL), `tier` (`official` | `curated`), `displayName`,
//! `version`, `description`, `homepage`, `keywords`. Nothing else is
//! documented; nothing else is parsed.

use serde_json::Value;

use crate::agent_plugin::{is_standard_plugin_name, slugify_plugin_name};

use super::super::types::{
    CatalogProvenance, CatalogTier, MarketplaceCandidate, MarketplaceCandidateId,
    MarketplaceCatalog, MarketplaceDiagnostic, MarketplaceEntryKind, MarketplaceFormat,
    MarketplaceInstallPlan, MarketplaceSourceSpec,
};
use super::{MarketplaceDocument, str_array_field, str_field, unknown_fields_warning};

const TOP_LEVEL_FIELDS: &[&str] = &["version", "plugins"];
pub const KIMI_ZIP_UNSUPPORTED_REASON: &str = "kimi_zip_unsupported";
pub const KIMI_REMOTE_UNSUPPORTED_REASON: &str = "kimi_remote_archive_unsupported";
pub const KIMI_GZIP_TARBALL_SOURCE_KIND: &str = "kimi_gzip_tarball_url";
const ENTRY_FIELDS: &[&str] = &[
    "id",
    "source",
    "tier",
    "displayName",
    "version",
    "description",
    "homepage",
    "keywords",
];

pub fn parse_kimi_catalog(document: MarketplaceDocument) -> MarketplaceCatalog {
    let MarketplaceDocument {
        catalog_id,
        root,
        base,
        ..
    } = document;
    let mut diagnostics = Vec::new();

    let obj = root.as_object();
    let (version, bad_version) = obj.map(|o| str_field(o, "version")).unwrap_or((None, None));
    let version = version.map(ToString::to_string);
    if let Some(diag) = bad_version {
        diagnostics.push(diag);
    }
    if let Some(diag) = obj.and_then(|o| unknown_fields_warning(o, TOP_LEVEL_FIELDS)) {
        diagnostics.push(diag);
    }

    let name = catalog_id.as_str().to_string();

    let Some(entries) = obj.and_then(|o| o.get("plugins")).and_then(Value::as_array) else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MISSING_PLUGINS",
            "Kimi marketplace must contain a `plugins` array",
            None,
            None,
        ));
        return MarketplaceCatalog {
            id: catalog_id,
            format: MarketplaceFormat::Kimi,
            name,
            display_name: None,
            description: None,
            version,
            base,
            provenance: CatalogProvenance::default(),
            candidates: Vec::new(),
            diagnostics,
        };
    };

    let mut candidates = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        match parse_kimi_entry(&catalog_id, index, entry, &mut diagnostics) {
            Some(candidate) => candidates.push(candidate),
            None => continue,
        }
    }

    MarketplaceCatalog {
        id: catalog_id,
        format: MarketplaceFormat::Kimi,
        name,
        display_name: None,
        description: None,
        version,
        base,
        provenance: CatalogProvenance::default(),
        candidates,
        diagnostics,
    }
}

fn parse_kimi_entry(
    catalog_id: &super::super::types::MarketplaceCatalogId,
    index: usize,
    entry: &Value,
    diagnostics: &mut Vec<MarketplaceDiagnostic>,
) -> Option<MarketplaceCandidate> {
    let Some(obj) = entry.as_object() else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MALFORMED_ENTRY",
            format!("Kimi plugin at index {index} must be a JSON object"),
            None,
            Some(index),
        ));
        return None;
    };

    let mut entry_diags = Vec::new();
    if let Some(diag) = unknown_fields_warning(obj, ENTRY_FIELDS) {
        entry_diags.push(diag);
    }

    let (raw_id, bad_id) = str_field(obj, "id");
    if let Some(diag) = bad_id {
        entry_diags.push(diag);
    }
    let Some(raw_id) = raw_id else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MISSING_ID",
            format!("Kimi plugin at index {index} is missing required `id`"),
            None,
            Some(index),
        ));
        return None;
    };

    // Kimi ids are already kebab-case; normalize only when they are not.
    let (name, display_name) = if is_standard_plugin_name(raw_id) {
        (raw_id.to_string(), None)
    } else {
        match slugify_plugin_name(raw_id) {
            Ok(slug) => {
                entry_diags.push(MarketplaceDiagnostic::warning(
                    "NON_STANDARD_NAME",
                    format!("Kimi id `{raw_id}` normalized to `{slug}`"),
                    Some(slug.clone()),
                    Some(index),
                ));
                (slug, Some(raw_id.to_string()))
            }
            Err(err) => {
                diagnostics.push(MarketplaceDiagnostic::error(
                    "INVALID_NAME",
                    format!("Kimi id `{raw_id}` cannot be normalized: {err}"),
                    Some(raw_id.to_string()),
                    Some(index),
                ));
                return None;
            }
        }
    };
    let (explicit_display, bad_display) = str_field(obj, "displayName");
    if let Some(diag) = bad_display {
        entry_diags.push(diag);
    }
    let display_name = explicit_display.map(ToString::to_string).or(display_name);

    let (source_raw, bad_source) = str_field(obj, "source");
    if let Some(diag) = bad_source {
        entry_diags.push(diag);
    }
    let Some(source_raw) = source_raw else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MISSING_SOURCE",
            format!(
                "Kimi plugin `{name}` is missing required `source` (path, zip URL, or GitHub URL)"
            ),
            Some(name.clone()),
            Some(index),
        ));
        return None;
    };

    let (source, install_plan) = normalize_kimi_source(source_raw);

    let (description, bad_desc) = str_field(obj, "description");
    if let Some(diag) = bad_desc {
        entry_diags.push(diag);
    }
    let (entry_version, bad_version) = str_field(obj, "version");
    if let Some(diag) = bad_version {
        entry_diags.push(diag);
    }
    let (homepage, bad_home) = str_field(obj, "homepage");
    if let Some(diag) = bad_home {
        entry_diags.push(diag);
    }
    let (tier_raw, bad_tier) = str_field(obj, "tier");
    if let Some(diag) = bad_tier {
        entry_diags.push(diag);
    }
    let tier = tier_raw.map(CatalogTier::parse).unwrap_or_default();
    if let Some(raw) = tier_raw
        && !matches!(raw, "official" | "curated")
    {
        entry_diags.push(MarketplaceDiagnostic::warning(
            "UNKNOWN_TIER",
            format!("Kimi tier `{raw}` is not documented; shown as community"),
            Some(name.clone()),
            Some(index),
        ));
    }
    let (keywords, bad_keywords) = str_array_field(obj, "keywords");
    if let Some(diag) = bad_keywords {
        entry_diags.push(diag);
    }

    let provenance = CatalogProvenance {
        tier,
        publisher: Some("Kimi marketplace".to_string()),
        source_url: homepage.map(ToString::to_string),
    };

    Some(MarketplaceCandidate {
        id: MarketplaceCandidateId::new(catalog_id, &name),
        catalog_id: catalog_id.clone(),
        kind: MarketplaceEntryKind::Plugin,
        icon: None,
        name,
        display_name,
        description: description.map(ToString::to_string),
        version: entry_version.map(ToString::to_string),
        author: None,
        homepage: homepage.map(ToString::to_string),
        repository: repository_of(&source),
        license: None,
        keywords,
        categories: Vec::new(),
        source,
        install_plan,
        // Kimi marketplace entries declare no components; compatibility
        // is decided by the reviewed manifest at install time.
        declared_components: None,
        compatibility: None,
        provenance,
        when: None,
        diagnostics: entry_diags,
    })
}

/// Kimi documents three source forms: local path, zip URL, GitHub URL.
/// Everything else is invalid for this format — no guessed fallbacks.
fn normalize_kimi_source(raw: &str) -> (MarketplaceSourceSpec, MarketplaceInstallPlan) {
    if let Some(rest) = raw
        .strip_prefix("https://github.com/")
        .or_else(|| raw.strip_prefix("http://github.com/"))
    {
        let mut parts = rest.trim_end_matches('/').split('/');
        if let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) {
            let repo = repo.trim_end_matches(".git");
            if !owner.is_empty() && !repo.is_empty() {
                let spec = format!("github:{owner}/{repo}");
                return (
                    MarketplaceSourceSpec::GitHub {
                        owner: owner.to_string(),
                        repo: repo.to_string(),
                        git_ref: None,
                        sha: None,
                    },
                    MarketplaceInstallPlan::Supported {
                        spec,
                        source_kind: "GitHub repository".to_string(),
                    },
                );
            }
        }
        return (
            MarketplaceSourceSpec::Invalid {
                reason: format!("`{raw}` is not a plain GitHub owner/repo URL"),
            },
            MarketplaceInstallPlan::Unsupported {
                reason: "Kimi source is a GitHub URL with extra path segments Codewhale cannot map"
                    .to_string(),
                raw: raw.to_string(),
            },
        );
    }
    if raw.starts_with("https://") || raw.starts_with("http://") {
        let lower = raw.to_ascii_lowercase();
        if lower.ends_with(".zip") {
            return (
                MarketplaceSourceSpec::ArchiveUrl {
                    url: raw.to_string(),
                    sha256: None,
                },
                MarketplaceInstallPlan::Unsupported {
                    reason: KIMI_ZIP_UNSUPPORTED_REASON.to_string(),
                    raw: raw.to_string(),
                },
            );
        }
        if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
            return (
                MarketplaceSourceSpec::ArchiveUrl {
                    url: raw.to_string(),
                    sha256: None,
                },
                MarketplaceInstallPlan::Supported {
                    spec: raw.to_string(),
                    source_kind: KIMI_GZIP_TARBALL_SOURCE_KIND.to_string(),
                },
            );
        }
        return (
            MarketplaceSourceSpec::Invalid {
                reason: format!(
                    "`{raw}` is not a documented Kimi source (path, zip URL, or GitHub URL)"
                ),
            },
            MarketplaceInstallPlan::Unsupported {
                reason: KIMI_REMOTE_UNSUPPORTED_REASON.to_string(),
                raw: raw.to_string(),
            },
        );
    }
    (
        MarketplaceSourceSpec::LocalPath { path: raw.into() },
        MarketplaceInstallPlan::Supported {
            spec: format!("path:{raw}"),
            source_kind: "Local directory".to_string(),
        },
    )
}

fn repository_of(source: &MarketplaceSourceSpec) -> Option<String> {
    match source {
        MarketplaceSourceSpec::GitHub { owner, repo, .. } => {
            Some(format!("https://github.com/{owner}/{repo}"))
        }
        _ => None,
    }
}
