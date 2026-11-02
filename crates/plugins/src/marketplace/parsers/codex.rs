//! Codex `.agents/plugins/marketplace.json` parser.
//!
//! Schema source: OpenAI plugin packaging docs
//! (developers.openai.com/plugins/build/plugins). The marketplace file
//! lives at `$REPO_ROOT/.agents/plugins/marketplace.json`,
//! `~/.agents/plugins/marketplace.json`, or legacy
//! `.claude-plugin/marketplace.json`.
//!
//! ```json
//! {
//!   "name": "local-repo",
//!   "plugins": [
//!     {
//!       "name": "my-plugin",
//!       "source": { "source": "local", "path": "./plugins/my-plugin" },
//!       "policy": { "installation": "AVAILABLE", "authentication": "ON_INSTALL" },
//!       "category": "Productivity"
//!     }
//!   ]
//! }
//! ```
//!
//! Top level: `name`; optional `interface` (`displayName`, …). Entry:
//! `name`, `source` object (`local` | `url` | `git-subdir` | `npm`),
//! optional `policy` (`installation`: `AVAILABLE` |
//! `INSTALLED_BY_DEFAULT` | `NOT_AVAILABLE`; `authentication`), optional
//! `category`.
//!
//! `policy` is display-only in Codewhale: `INSTALLED_BY_DEFAULT` never
//! triggers an install, and `NOT_AVAILABLE` only downgrades the install
//! plan with an honest reason.

use serde_json::Value;

use crate::agent_plugin::{is_standard_plugin_name, slugify_plugin_name};

use super::super::types::{
    CatalogProvenance, CatalogTier, MarketplaceCandidate, MarketplaceCandidateId,
    MarketplaceCatalog, MarketplaceDiagnostic, MarketplaceEntryKind, MarketplaceFormat,
    MarketplaceInstallPlan, MarketplaceSourceSpec,
};
use super::{MarketplaceDocument, str_field, unknown_fields_warning};

const TOP_LEVEL_FIELDS: &[&str] = &["name", "plugins", "interface"];
const ENTRY_FIELDS: &[&str] = &["name", "source", "policy", "category"];

pub fn parse_codex_catalog(document: MarketplaceDocument) -> MarketplaceCatalog {
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
                "Codex marketplace must be a JSON object",
                None,
                None,
            ),
        );
    };

    if let Some(diag) = unknown_fields_warning(obj, TOP_LEVEL_FIELDS) {
        diagnostics.push(diag);
    }

    let (name, bad_name) = str_field(obj, "name");
    if let Some(diag) = bad_name {
        diagnostics.push(diag);
    }
    let name = name
        .map(ToString::to_string)
        .unwrap_or_else(|| catalog_id.as_str().to_string());

    let display_name = obj
        .get("interface")
        .and_then(|i| i.get("displayName"))
        .and_then(Value::as_str)
        .map(ToString::to_string);

    let Some(entries) = obj.get("plugins").and_then(Value::as_array) else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MISSING_PLUGINS",
            "Codex marketplace must contain a `plugins` array",
            None,
            None,
        ));
        return MarketplaceCatalog {
            id: catalog_id,
            format: MarketplaceFormat::Codex,
            name,
            display_name,
            description: None,
            version: None,
            base,
            provenance: CatalogProvenance::default(),
            candidates: Vec::new(),
            diagnostics,
        };
    };

    let mut candidates = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if let Some(candidate) = parse_codex_entry(&catalog_id, index, entry, &mut diagnostics) {
            candidates.push(candidate);
        }
    }

    MarketplaceCatalog {
        id: catalog_id,
        format: MarketplaceFormat::Codex,
        name,
        display_name,
        description: None,
        version: None,
        base,
        provenance: CatalogProvenance {
            tier: CatalogTier::Community,
            publisher: Some("Codex marketplace".to_string()),
            source_url: None,
        },
        candidates,
        diagnostics,
    }
}

fn empty_catalog(
    catalog_id: super::super::types::MarketplaceCatalogId,
    base: Option<String>,
    diagnostic: MarketplaceDiagnostic,
) -> MarketplaceCatalog {
    MarketplaceCatalog {
        id: catalog_id,
        format: MarketplaceFormat::Codex,
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

fn parse_codex_entry(
    catalog_id: &super::super::types::MarketplaceCatalogId,
    index: usize,
    entry: &Value,
    diagnostics: &mut Vec<MarketplaceDiagnostic>,
) -> Option<MarketplaceCandidate> {
    let Some(obj) = entry.as_object() else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MALFORMED_ENTRY",
            format!("Codex plugin at index {index} must be a JSON object"),
            None,
            Some(index),
        ));
        return None;
    };

    let mut entry_diags = Vec::new();
    if let Some(diag) = unknown_fields_warning(obj, ENTRY_FIELDS) {
        entry_diags.push(diag);
    }

    let (raw_name, bad_name) = str_field(obj, "name");
    if let Some(diag) = bad_name {
        entry_diags.push(diag);
    }
    let Some(raw_name) = raw_name else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MISSING_NAME",
            format!("Codex plugin at index {index} is missing required `name`"),
            None,
            Some(index),
        ));
        return None;
    };
    let name = if is_standard_plugin_name(raw_name) {
        raw_name.to_string()
    } else {
        match slugify_plugin_name(raw_name) {
            Ok(slug) => {
                entry_diags.push(MarketplaceDiagnostic::warning(
                    "NON_STANDARD_NAME",
                    format!("Codex name `{raw_name}` normalized to `{slug}`"),
                    Some(slug.clone()),
                    Some(index),
                ));
                slug
            }
            Err(err) => {
                diagnostics.push(MarketplaceDiagnostic::error(
                    "INVALID_NAME",
                    format!("Codex name `{raw_name}` cannot be normalized: {err}"),
                    Some(raw_name.to_string()),
                    Some(index),
                ));
                return None;
            }
        }
    };

    let Some(source_value) = obj.get("source") else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MISSING_SOURCE",
            format!("Codex plugin `{name}` is missing required `source`"),
            Some(name.clone()),
            Some(index),
        ));
        return None;
    };
    let (source, mut install_plan, source_diags) = normalize_codex_source(source_value);
    entry_diags.extend(source_diags);

    // `policy.installation` is honored as display/availability metadata
    // only. Codewhale never auto-installs and never auto-authenticates.
    let installation = obj
        .get("policy")
        .and_then(|p| p.get("installation"))
        .and_then(Value::as_str);
    match installation {
        Some("NOT_AVAILABLE") => {
            if install_plan.is_supported() {
                install_plan = MarketplaceInstallPlan::Unsupported {
                    reason: "catalog policy marks this plugin NOT_AVAILABLE".to_string(),
                    raw: source_value.to_string(),
                };
            }
        }
        Some("INSTALLED_BY_DEFAULT") => {
            entry_diags.push(MarketplaceDiagnostic::warning(
                "NO_AUTO_INSTALL",
                "Codex policy `INSTALLED_BY_DEFAULT` is ignored: Codewhale installs only on an explicit operator action".to_string(),
                Some(name.clone()),
                Some(index),
            ));
        }
        Some("AVAILABLE") | None => {}
        Some(other) => {
            entry_diags.push(MarketplaceDiagnostic::warning(
                "UNKNOWN_POLICY",
                format!(
                    "Codex installation policy `{other}` is not documented; treated as AVAILABLE"
                ),
                Some(name.clone()),
                Some(index),
            ));
        }
    }

    let (category, bad_category) = str_field(obj, "category");
    if let Some(diag) = bad_category {
        entry_diags.push(diag);
    }

    Some(MarketplaceCandidate {
        id: MarketplaceCandidateId::new(catalog_id, &name),
        catalog_id: catalog_id.clone(),
        kind: MarketplaceEntryKind::Plugin,
        icon: None,
        name,
        display_name: None,
        description: None,
        version: None,
        author: None,
        homepage: None,
        repository: None,
        license: None,
        keywords: Vec::new(),
        categories: category.map(|c| vec![c.to_string()]).unwrap_or_default(),
        source,
        install_plan,
        // Codex marketplace entries declare no component counts; the
        // `.codex-plugin/plugin.json` manifest at install time decides.
        declared_components: None,
        compatibility: None,
        provenance: CatalogProvenance {
            tier: CatalogTier::Community,
            publisher: Some("Codex marketplace".to_string()),
            source_url: None,
        },
        when: None,
        diagnostics: entry_diags,
    })
}

/// Only the four documented Codex source discriminators are accepted.
fn normalize_codex_source(
    value: &Value,
) -> (
    MarketplaceSourceSpec,
    MarketplaceInstallPlan,
    Vec<MarketplaceDiagnostic>,
) {
    let mut diags = Vec::new();
    let Some(obj) = value.as_object() else {
        return (
            MarketplaceSourceSpec::Invalid {
                reason: "Codex source must be a tagged object".to_string(),
            },
            MarketplaceInstallPlan::Unsupported {
                reason: "Codex source must be a `{source: local|url|git-subdir|npm}` object"
                    .to_string(),
                raw: value.to_string(),
            },
            diags,
        );
    };
    let discriminator = obj
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let field = |name: &str| obj.get(name).and_then(Value::as_str);
    match discriminator {
        "local" => {
            let Some(path) = field("path") else {
                return invalid("local source requires `path`", value, &mut diags);
            };
            (
                MarketplaceSourceSpec::LocalPath { path: path.into() },
                MarketplaceInstallPlan::Supported {
                    spec: format!("path:{path}"),
                    source_kind: "Local directory".to_string(),
                },
                diags,
            )
        }
        "url" | "git-subdir" => {
            let Some(url) = field("url") else {
                return invalid(
                    &format!("{discriminator} source requires `url`"),
                    value,
                    &mut diags,
                );
            };
            if let Some(rest) = url
                .strip_prefix("https://github.com/")
                .or_else(|| url.strip_prefix("http://github.com/"))
            {
                let mut parts = rest.trim_end_matches('/').split('/');
                if let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next())
                {
                    let repo = repo.trim_end_matches(".git");
                    if !owner.is_empty() && !repo.is_empty() {
                        diags.push(MarketplaceDiagnostic::warning(
                            "UNAPPLIED_PIN",
                            "Codewhale's GitHub installer resolves the default branch; ref/sha pins are recorded but not yet applied".to_string(),
                            None,
                            None,
                        ));
                        return (
                            MarketplaceSourceSpec::GitHub {
                                owner: owner.to_string(),
                                repo: repo.to_string(),
                                git_ref: field("ref").map(ToString::to_string),
                                sha: field("sha").map(ToString::to_string),
                            },
                            MarketplaceInstallPlan::Supported {
                                spec: format!("github:{owner}/{repo}"),
                                source_kind: "GitHub repository".to_string(),
                            },
                            diags,
                        );
                    }
                }
            }
            (
                MarketplaceSourceSpec::GitUrl {
                    url: url.to_string(),
                },
                MarketplaceInstallPlan::Unsupported {
                    reason: format!(
                        "{discriminator} installs of non-GitHub git URLs are not supported yet"
                    ),
                    raw: value.to_string(),
                },
                diags,
            )
        }
        "npm" => {
            let Some(package) = field("package") else {
                return invalid("npm source requires `package`", value, &mut diags);
            };
            (
                MarketplaceSourceSpec::Npm { package: package.to_string() },
                MarketplaceInstallPlan::Unsupported {
                    reason: "Codewhale does not execute npm; install the plugin from a GitHub repo, tarball, or local path".to_string(),
                    raw: value.to_string(),
                },
                diags,
            )
        }
        other => invalid(
            &format!("unknown Codex source discriminator `{other}`"),
            value,
            &mut diags,
        ),
    }
}

fn invalid(
    reason: &str,
    value: &Value,
    diags: &mut Vec<MarketplaceDiagnostic>,
) -> (
    MarketplaceSourceSpec,
    MarketplaceInstallPlan,
    Vec<MarketplaceDiagnostic>,
) {
    diags.push(MarketplaceDiagnostic::error(
        "INVALID_SOURCE",
        reason.to_string(),
        None,
        None,
    ));
    (
        MarketplaceSourceSpec::Invalid {
            reason: reason.to_string(),
        },
        MarketplaceInstallPlan::Unsupported {
            reason: reason.to_string(),
            raw: value.to_string(),
        },
        std::mem::take(diags),
    )
}
