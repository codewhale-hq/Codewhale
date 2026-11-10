//! DeepSeek Harness plugin market index, format 1.
//!
//! Schema source: `reference/dsh` `packages/util/plugin-market-index`
//! (`types.ts` + `index.ts`, the format's executable rule set) and its
//! README ("format 1 … the single JSON file that lists installable DSH
//! bundles with their display metadata").
//!
//! ```json
//! {
//!   "format": 1,
//!   "title": "My market",
//!   "metadata": { "categories": { "tools": { "title": "Tools" } } },
//!   "plugins": [{
//!     "name": "@scope/plugin",
//!     "title": { "en": "Plugin" },
//!     "icon": "./icon.png",
//!     "metadata": { "category": "tools", "keywords": ["x"] },
//!     "versions": [{
//!       "version": "1.2.3",
//!       "publishTimestamp": 1760000000,
//!       "source": { "type": "npm" },
//!       "engines": { "dsh": ">=0.2.0 <0.3.0" }
//!     }]
//!   }]
//! }
//! ```
//!
//! Parsed as the format documents it: `format` must be the number `1`,
//! `name` is the plugin's npm package name, display text is a string or a
//! language-keyed object whose `en` member is authoritative, and relative
//! `icon` references resolve against the index URL when that URL is known.
//! `plugins[].versions` is ordered by descending SemVer precedence, and the
//! highest version decides the row's version and install source.
//!
//! # Install reality, stated honestly
//!
//! DSH installs `name@version` through npm or a commit-pinned `git+https`
//! URL. Codewhale does not execute npm, and its installer fetches whole
//! GitHub repositories (archive tarballs) rather than a package
//! subdirectory. So:
//!
//! - `npm` sources parse into [`MarketplaceSourceSpec::Npm`] and stay
//!   [`MarketplaceInstallPlan::Unsupported`]: visible and truthful, never
//!   implied installable.
//! - `git` sources on `github.com` without a `path` become a pinned
//!   `github:` install plan. A `path` (package inside a subdirectory) or a
//!   non-GitHub host stays unsupported.
//! - A highest version whose `engines.dsh` range admits none of the DSH
//!   versions this host targets is reported as unsupported with that range,
//!   before any download.

use serde_json::Value;

use super::super::types::{
    CatalogProvenance, CatalogTier, MarketplaceCandidate, MarketplaceCandidateId,
    MarketplaceCatalog, MarketplaceCatalogId, MarketplaceDiagnostic, MarketplaceEntryKind,
    MarketplaceFormat, MarketplaceInstallPlan, MarketplaceSourceSpec,
};
use super::{MarketplaceDocument, str_field, unknown_fields_warning};

const TOP_LEVEL_FIELDS: &[&str] = &["format", "title", "description", "metadata", "plugins"];
const ENTRY_FIELDS: &[&str] = &[
    "name",
    "title",
    "description",
    "icon",
    "author",
    "homepage",
    "repository",
    "metadata",
    "versions",
];
const VERSION_FIELDS: &[&str] = &["version", "publishTimestamp", "source", "engines"];

pub fn parse_dsh_catalog(document: MarketplaceDocument) -> MarketplaceCatalog {
    let MarketplaceDocument {
        catalog_id,
        root,
        base,
        ..
    } = document;
    let mut diagnostics = Vec::new();

    let Some(obj) = root.as_object() else {
        return empty_catalog(
            catalog_id,
            base,
            MarketplaceDiagnostic::error(
                "NOT_AN_OBJECT",
                "a DSH market index must be a JSON object",
                None,
                None,
            ),
        );
    };

    // The format's own discriminator: `format` must be the number 1.
    if obj.get("format").and_then(Value::as_f64) != Some(1.0) {
        diagnostics.push(MarketplaceDiagnostic::error(
            "UNDECLARED_FORMAT",
            "a DSH market index must declare `\"format\": 1`",
            None,
            None,
        ));
        return MarketplaceCatalog {
            id: catalog_id,
            format: MarketplaceFormat::Dsh,
            name: String::new(),
            display_name: None,
            description: None,
            version: None,
            base,
            provenance: CatalogProvenance::default(),
            candidates: Vec::new(),
            diagnostics,
        };
    }

    if let Some(diag) = unknown_fields_warning(obj, TOP_LEVEL_FIELDS) {
        diagnostics.push(diag);
    }

    let title = localized_field(obj, "title", &mut diagnostics);
    let description = localized_field(obj, "description", &mut diagnostics);
    let declarations = index_declarations(obj.get("metadata"));

    let Some(entries) = obj.get("plugins").and_then(Value::as_array) else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MISSING_PLUGINS",
            "a DSH market index must contain a `plugins` array",
            None,
            None,
        ));
        let name = title
            .clone()
            .unwrap_or_else(|| catalog_id.as_str().to_string());
        return MarketplaceCatalog {
            id: catalog_id,
            format: MarketplaceFormat::Dsh,
            name,
            display_name: title,
            description,
            version: None,
            base,
            provenance: CatalogProvenance::default(),
            candidates: Vec::new(),
            diagnostics,
        };
    };

    let mut candidates = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if let Some(candidate) = parse_entry(
            &catalog_id,
            index,
            entry,
            base.as_deref(),
            &declarations,
            &mut diagnostics,
        ) {
            candidates.push(candidate);
        }
    }

    MarketplaceCatalog {
        id: catalog_id,
        format: MarketplaceFormat::Dsh,
        name: title
            .clone()
            .unwrap_or_else(|| "DeepSeek Harness market".to_string()),
        display_name: title,
        description,
        version: None,
        base,
        provenance: CatalogProvenance {
            tier: CatalogTier::Community,
            publisher: None,
            source_url: None,
        },
        candidates,
        diagnostics,
    }
}

fn empty_catalog(
    catalog_id: MarketplaceCatalogId,
    base: Option<String>,
    diagnostic: MarketplaceDiagnostic,
) -> MarketplaceCatalog {
    MarketplaceCatalog {
        id: catalog_id,
        format: MarketplaceFormat::Dsh,
        name: String::new(),
        display_name: None,
        description: None,
        version: None,
        base,
        provenance: CatalogProvenance::default(),
        candidates: Vec::new(),
        diagnostics: vec![diagnostic],
    }
}

/// Category/label ids the index declares. An entry may only reference a
/// declared id; anything else is dropped with a warning, as the validator
/// does.
#[derive(Default)]
struct IndexDeclarations {
    categories: Vec<String>,
    labels: Vec<String>,
}

fn index_declarations(metadata: Option<&Value>) -> IndexDeclarations {
    let mut declarations = IndexDeclarations::default();
    let Some(groups) = metadata.and_then(Value::as_object) else {
        return declarations;
    };
    declarations.categories = declared_ids(groups.get("categories"));
    declarations.labels = declared_ids(groups.get("labels"));
    declarations
}

fn declared_ids(group: Option<&Value>) -> Vec<String> {
    group
        .and_then(Value::as_object)
        .map(|record| record.keys().cloned().collect())
        .unwrap_or_default()
}

/// `title`/`description` are either a string or a language-keyed object. The
/// format requires an `en` member on the object form, so `en` is the value
/// shown; a first member is only a degraded fallback carrying a warning.
fn localized_field(
    obj: &serde_json::Map<String, Value>,
    field: &str,
    diagnostics: &mut Vec<MarketplaceDiagnostic>,
) -> Option<String> {
    match obj.get(field) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Object(languages)) => {
            if let Some(en) = languages.get("en").and_then(Value::as_str) {
                return Some(en.to_string());
            }
            let first = languages.values().find_map(Value::as_str);
            if first.is_some() {
                diagnostics.push(MarketplaceDiagnostic::warning(
                    "MISSING_EN",
                    format!("`{field}` is language-keyed without `en`; showing another locale"),
                    None,
                    None,
                ));
            }
            first.map(str::to_string)
        }
        Some(_) => {
            diagnostics.push(MarketplaceDiagnostic::warning(
                "FIELD_TYPE",
                format!("field `{field}` must be a string or a language-keyed object"),
                None,
                None,
            ));
            None
        }
    }
}

fn parse_entry(
    catalog_id: &MarketplaceCatalogId,
    index: usize,
    value: &Value,
    base: Option<&str>,
    declarations: &IndexDeclarations,
    diagnostics: &mut Vec<MarketplaceDiagnostic>,
) -> Option<MarketplaceCandidate> {
    let Some(obj) = value.as_object() else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "ENTRY_NOT_OBJECT",
            "each DSH market entry must be an object",
            None,
            Some(index),
        ));
        return None;
    };
    let mut entry_diags = Vec::new();
    if let Some(diag) = unknown_fields_warning(obj, ENTRY_FIELDS) {
        entry_diags.push(diag);
    }

    let (name, bad_name) = str_field(obj, "name");
    if let Some(diag) = bad_name {
        entry_diags.push(diag);
    }
    let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MISSING_NAME",
            "a DSH market entry must declare its npm package `name`",
            None,
            Some(index),
        ));
        return None;
    };

    let title = localized_field(obj, "title", &mut entry_diags);
    let description = localized_field(obj, "description", &mut entry_diags);
    let (author, bad_author) = str_field(obj, "author");
    if let Some(diag) = bad_author {
        entry_diags.push(diag);
    }
    let (homepage, bad_homepage) = str_field(obj, "homepage");
    if let Some(diag) = bad_homepage {
        entry_diags.push(diag);
    }
    let (icon, bad_icon) = str_field(obj, "icon");
    if let Some(diag) = bad_icon {
        entry_diags.push(diag);
    }
    let icon = icon.and_then(|icon| resolve_icon(icon, base, &mut entry_diags));
    let repository = obj
        .get("repository")
        .and_then(Value::as_object)
        .and_then(|repo| repo.get("url"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let (keywords, categories) =
        entry_metadata(obj.get("metadata"), declarations, &mut entry_diags);

    let Some(version) = pick_version(obj.get("versions"), &mut entry_diags) else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MISSING_VERSIONS",
            format!("DSH market entry `{name}` must carry a non-empty `versions` array"),
            Some(name.to_string()),
            Some(index),
        ));
        return None;
    };

    let (source, install_plan) = normalize_version_source(name, &version);

    Some(MarketplaceCandidate {
        id: MarketplaceCandidateId::new(catalog_id, name),
        catalog_id: catalog_id.clone(),
        kind: MarketplaceEntryKind::Plugin,
        name: name.to_string(),
        display_name: title,
        icon,
        description,
        version: Some(version.version.clone()),
        author: author.map(str::to_string),
        homepage: homepage.map(str::to_string),
        repository,
        license: None,
        keywords,
        categories,
        source,
        install_plan,
        // The format lists packages, not their components; the DSH bundle's
        // own manifest decides activation at install review.
        declared_components: None,
        compatibility: None,
        provenance: CatalogProvenance {
            tier: CatalogTier::Community,
            publisher: author.map(str::to_string),
            source_url: base.map(str::to_string),
        },
        when: None,
        diagnostics: entry_diags,
    })
}

/// One version's install-relevant facts, validated as the format documents
/// them: exact SemVer, a tagged `npm`/`git` source, and an optional
/// `engines.dsh` SemVer range that must be a range, never `workspace:`.
struct DshVersion {
    version: String,
    engines: Option<String>,
    source_type: String,
    registry: Option<String>,
    git_url: Option<String>,
    git_commit: Option<String>,
    git_path: Option<String>,
}

fn pick_version(
    versions: Option<&Value>,
    entry_diags: &mut Vec<MarketplaceDiagnostic>,
) -> Option<DshVersion> {
    let entries = versions?.as_array()?;
    if entries.is_empty() {
        return None;
    }
    let mut parsed: Vec<(semver::Version, DshVersion)> = Vec::new();
    for (position, entry) in entries.iter().enumerate() {
        let Some(obj) = entry.as_object() else {
            entry_diags.push(MarketplaceDiagnostic::warning(
                "VERSION_NOT_OBJECT",
                format!("`versions[{position}]` is not an object and was ignored"),
                None,
                None,
            ));
            continue;
        };
        if let Some(diag) = unknown_fields_warning(obj, VERSION_FIELDS) {
            entry_diags.push(diag);
        }
        let (raw_version, bad_version) = str_field(obj, "version");
        if let Some(diag) = bad_version {
            entry_diags.push(diag);
        }
        let Some(raw_version) = raw_version else {
            entry_diags.push(MarketplaceDiagnostic::warning(
                "MISSING_VERSION",
                format!("`versions[{position}]` declares no `version` and was ignored"),
                None,
                None,
            ));
            continue;
        };
        let Ok(precedence) = semver::Version::parse(raw_version) else {
            entry_diags.push(MarketplaceDiagnostic::warning(
                "INVALID_VERSION",
                format!("`versions[{position}].version` `{raw_version}` is not exact SemVer"),
                None,
                None,
            ));
            continue;
        };
        let engines = obj
            .get("engines")
            .and_then(Value::as_object)
            .and_then(|engines| engines.get("dsh"))
            .and_then(Value::as_str);
        if let Some(range) = engines
            && !range_is_dsh_range(range)
        {
            entry_diags.push(MarketplaceDiagnostic::warning(
                "INVALID_ENGINES",
                format!(
                    "`versions[{position}].engines.dsh` `{range}` is not a SemVer range; the version was ignored"
                ),
                None,
                None,
            ));
            continue;
        }
        let Some(source) = obj.get("source").and_then(Value::as_object) else {
            entry_diags.push(MarketplaceDiagnostic::warning(
                "MISSING_SOURCE",
                format!("`versions[{position}]` declares no `source` object and was ignored"),
                None,
                None,
            ));
            continue;
        };
        let source_type = source
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !matches!(source_type, "npm" | "git") {
            entry_diags.push(MarketplaceDiagnostic::warning(
                "UNKNOWN_SOURCE",
                format!(
                    "`versions[{position}].source.type` must be \"npm\" or \"git\"; the version was ignored"
                ),
                None,
                None,
            ));
            continue;
        }
        parsed.push((
            precedence,
            DshVersion {
                version: raw_version.to_string(),
                engines: engines.map(str::to_string),
                source_type: source_type.to_string(),
                registry: source
                    .get("registry")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                git_url: source
                    .get("url")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                git_commit: source
                    .get("commit")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                git_path: source
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            },
        ));
    }
    parsed.sort_by(|left, right| right.0.cmp(&left.0));
    parsed.into_iter().next().map(|(_, version)| version)
}

fn entry_metadata(
    metadata: Option<&Value>,
    declarations: &IndexDeclarations,
    entry_diags: &mut Vec<MarketplaceDiagnostic>,
) -> (Vec<String>, Vec<String>) {
    let Some(metadata) = metadata.and_then(Value::as_object) else {
        return (Vec::new(), Vec::new());
    };
    let keywords = string_list(metadata.get("keywords"));
    let mut categories = Vec::new();
    if let Some(category) = metadata.get("category").and_then(Value::as_str) {
        if declarations.categories.iter().any(|id| id == category) {
            categories.push(category.to_string());
        } else {
            entry_diags.push(MarketplaceDiagnostic::warning(
                "UNDECLARED_CATEGORY",
                format!("entry category `{category}` is not declared by the index"),
                None,
                None,
            ));
        }
    }
    for label in string_list(metadata.get("labels")) {
        if declarations.labels.contains(&label) {
            categories.push(label);
        } else {
            entry_diags.push(MarketplaceDiagnostic::warning(
                "UNDECLARED_LABEL",
                format!("entry label `{label}` is not declared by the index"),
                None,
                None,
            ));
        }
    }
    (keywords, categories)
}

/// The DSH validator accepts what node-semver's `validRange` accepts: bare
/// exact versions, space-separated comparators and `||` clauses. The npm
/// range parser already used for `engines.dsh` admission is the authority
/// here, so a range the installer would refuse is never accepted as valid.
fn range_is_dsh_range(range: &str) -> bool {
    !range.starts_with("workspace:")
        && crate::install::dsh::match_supported_dsh_range(range).is_ok()
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn resolve_icon(
    icon: &str,
    base: Option<&str>,
    entry_diags: &mut Vec<MarketplaceDiagnostic>,
) -> Option<String> {
    if icon.starts_with("data:") || icon.starts_with("https://") || icon.starts_with("http://") {
        return Some(icon.to_string());
    }
    let Some(base) = base else {
        entry_diags.push(MarketplaceDiagnostic::warning(
            "UNRESOLVED_ICON",
            format!("relative icon `{icon}` has no index URL to resolve against; ignored"),
            None,
            None,
        ));
        return None;
    };
    let Some((prefix, _)) = base.rsplit_once('/') else {
        entry_diags.push(MarketplaceDiagnostic::warning(
            "UNRESOLVED_ICON",
            format!("relative icon `{icon}` cannot be resolved against `{base}`; ignored"),
            None,
            None,
        ));
        return None;
    };
    // Resolve the reference the way a URL parser would: `.` segments drop
    // and `..` pops one segment, but never above the origin's first path
    // segment (an index URL is absolute, so it always has one).
    let scheme_end = prefix.find("://").map_or(0, |at| at + 3);
    let (origin, path) = prefix.split_at(scheme_end);
    let mut segments: Vec<&str> = path.split('/').collect();
    for part in icon.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if segments.len() > 1 {
                    segments.pop();
                }
            }
            other => segments.push(other),
        }
    }
    Some(format!("{origin}{}", segments.join("/")))
}

fn normalize_version_source(
    name: &str,
    version: &DshVersion,
) -> (MarketplaceSourceSpec, MarketplaceInstallPlan) {
    let incompatible = version.engines.as_deref().and_then(|range| {
        match crate::install::dsh::match_supported_dsh_range(range) {
            Ok(Some(_)) => None,
            Ok(None) => Some(format!(
                "incompatible-version: the bundle requires DSH `{range}`, but this host targets DSH 0.2.1-rc.5 / 0.2.2-rc.1"
            )),
            Err(_) => Some(format!(
                "incompatible-version: `engines.dsh` value `{range}` is not a SemVer range"
            )),
        }
    });
    if let Some(reason) = incompatible {
        return (
            MarketplaceSourceSpec::Npm {
                package: name.to_string(),
            },
            MarketplaceInstallPlan::Unsupported {
                reason,
                raw: version.version.clone(),
            },
        );
    }

    if version.source_type == "npm" {
        let mut reason = format!(
            "DSH market npm source `{name}@{}` installs through npm; Codewhale does not execute npm. Install a DSH bundle from a GitHub repository, tarball, or local path",
            version.version
        );
        if let Some(registry) = &version.registry {
            reason.push_str(&format!(" (declared registry: {registry})"));
        }
        return (
            MarketplaceSourceSpec::Npm {
                package: name.to_string(),
            },
            MarketplaceInstallPlan::Unsupported {
                reason,
                raw: version.version.clone(),
            },
        );
    }

    let url = version.git_url.clone().unwrap_or_default();
    let Some(commit) = version.git_commit.as_deref() else {
        return (
            MarketplaceSourceSpec::GitUrl { url },
            MarketplaceInstallPlan::Unsupported {
                reason: "a DSH git source must pin an exact 40-hex commit SHA".to_string(),
                raw: version.version.clone(),
            },
        );
    };
    let Some((owner, repo)) = super::github_url_parts(&url) else {
        return (
            MarketplaceSourceSpec::GitUrl { url },
            MarketplaceInstallPlan::Unsupported {
                reason: "Codewhale installs DSH bundles from GitHub repositories; this source is not a GitHub repository"
                    .to_string(),
                raw: version.version.clone(),
            },
        );
    };
    let sha_valid = commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit());
    let spec = format!("git:github.com/{owner}/{repo}@{commit}");
    if version.git_path.is_some()
        || !sha_valid
        || crate::install::PluginInstallSource::parse(&spec).is_err()
    {
        return (
            MarketplaceSourceSpec::GitHub {
                owner,
                repo,
                git_ref: None,
                sha: Some(commit.to_string()),
            },
            MarketplaceInstallPlan::Unsupported {
                reason: if version.git_path.is_some() {
                    "this version lives in a repository subdirectory; Codewhale installs a whole repository and cannot select the package directory"
                        .to_string()
                } else {
                    "the git source cannot be installed exactly as declared (the commit must be a 40-hex SHA)".to_string()
                },
                raw: version.version.clone(),
            },
        );
    }
    (
        MarketplaceSourceSpec::GitHub {
            owner,
            repo,
            git_ref: None,
            sha: Some(commit.to_string()),
        },
        MarketplaceInstallPlan::Supported {
            spec,
            source_kind: "GitHub repository".to_string(),
        },
    )
}
