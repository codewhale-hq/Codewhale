//! Codewhale native catalog parser.
//!
//! This is Codewhale's own documented catalog format. It is deliberately
//! the smallest of the supported formats: entries carry a `name` and a
//! `source` that is exactly the install spec `/plugin install` accepts
//! (`github:owner/repo`, `path:dir`, an http(s) tarball URL). A tarball
//! entry may add `sha256`; the install refuses archive bytes that differ.
//!
//! ```json
//! {
//!   "name": "my-catalog",
//!   "description": "Team plugins",
//!   "version": "1",
//!   "plugins": [
//!     { "name": "formatter", "source": "github:owner/repo", "version": "2.1.0" }
//!   ]
//! }
//! ```

use serde_json::Value;

use crate::install::PluginInstallSource;

use super::super::types::{
    CatalogProvenance, CatalogTier, MarketplaceCandidate, MarketplaceCandidateId,
    MarketplaceCatalog, MarketplaceDiagnostic, MarketplaceEntryKind, MarketplaceFormat,
    MarketplaceInstallPlan, MarketplaceSourceSpec,
};
use super::{MarketplaceDocument, str_field, unknown_fields_warning};

const TOP_LEVEL_FIELDS: &[&str] = &["name", "description", "version", "plugins"];
const ENTRY_FIELDS: &[&str] = &[
    "name",
    "source",
    "kind",
    "description",
    "version",
    "homepage",
    "display_name",
    "author",
    "icon",
    "platforms",
    "sha256",
];

pub fn parse_codewhale_catalog(document: MarketplaceDocument) -> MarketplaceCatalog {
    let MarketplaceDocument {
        catalog_id,
        root,
        base,
        ..
    } = document;
    let mut diagnostics = Vec::new();

    let Some(obj) = root.as_object() else {
        return MarketplaceCatalog {
            id: catalog_id,
            format: MarketplaceFormat::Codewhale,
            name: String::new(),
            display_name: None,
            description: None,
            version: None,
            base,
            provenance: CatalogProvenance::default(),
            candidates: Vec::new(),
            diagnostics: vec![MarketplaceDiagnostic::error(
                "NOT_AN_OBJECT",
                "Codewhale catalog must be a JSON object",
                None,
                None,
            )],
        };
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
    let (description, bad_desc) = str_field(obj, "description");
    if let Some(diag) = bad_desc {
        diagnostics.push(diag);
    }
    let (version, bad_version) = str_field(obj, "version");
    if let Some(diag) = bad_version {
        diagnostics.push(diag);
    }

    let Some(entries) = obj.get("plugins").and_then(Value::as_array) else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MISSING_PLUGINS",
            "Codewhale catalog must contain a `plugins` array",
            None,
            None,
        ));
        return MarketplaceCatalog {
            id: catalog_id,
            format: MarketplaceFormat::Codewhale,
            name,
            display_name: None,
            description: description.map(ToString::to_string),
            version: version.map(ToString::to_string),
            base,
            provenance: CatalogProvenance::default(),
            candidates: Vec::new(),
            diagnostics,
        };
    };

    let mut candidates = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if let Some(candidate) = parse_codewhale_entry(&catalog_id, index, entry, &mut diagnostics)
        {
            candidates.push(candidate);
        }
    }

    MarketplaceCatalog {
        id: catalog_id,
        format: MarketplaceFormat::Codewhale,
        name,
        display_name: None,
        description: description.map(ToString::to_string),
        version: version.map(ToString::to_string),
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

fn parse_codewhale_entry(
    catalog_id: &super::super::types::MarketplaceCatalogId,
    index: usize,
    entry: &Value,
    diagnostics: &mut Vec<MarketplaceDiagnostic>,
) -> Option<MarketplaceCandidate> {
    let Some(obj) = entry.as_object() else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MALFORMED_ENTRY",
            format!("Codewhale plugin at index {index} must be a JSON object"),
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
    let Some(name) = name else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MISSING_NAME",
            format!("Codewhale plugin at index {index} is missing required `name`"),
            None,
            Some(index),
        ));
        return None;
    };
    // The name is the candidate's identity (registry lookup, `/plugin show`,
    // install), so it is held to the plugin-name rules the other formats
    // normalize to, rather than stored verbatim with controls or unbounded.
    if crate::manifest::validate_plugin_name(name).is_err()
        && !crate::agent_plugin::is_standard_plugin_name(name)
    {
        diagnostics.push(MarketplaceDiagnostic::error(
            "INVALID_NAME",
            format!(
                "Codewhale plugin at index {index} has an invalid name {:?}: use lowercase letters, digits, and internal hyphens or dots",
                name.chars().take(80).collect::<String>()
            ),
            None,
            Some(index),
        ));
        return None;
    }
    let name = name.to_string();

    let (source, bad_source) = str_field(obj, "source");
    if let Some(diag) = bad_source {
        entry_diags.push(diag);
    }
    let Some(source) = source else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MISSING_SOURCE",
            format!("Codewhale plugin `{name}` is missing required `source`"),
            Some(name.clone()),
            Some(index),
        ));
        return None;
    };

    let mut install_plan = match PluginInstallSource::parse(source) {
        Ok(parsed) => {
            let source_kind = match &parsed {
                PluginInstallSource::Remote(
                    codewhale_skill_install::install::InstallSource::GitHubRepo(_),
                ) => "GitHub repository".to_string(),
                PluginInstallSource::Remote(
                    codewhale_skill_install::install::InstallSource::DirectUrl(_),
                ) => "Tarball URL".to_string(),
                PluginInstallSource::LocalPath { .. } => "Local directory".to_string(),
                PluginInstallSource::Dsh(_) => "DeepSeek Harness bundle package".to_string(),
                PluginInstallSource::Remote(
                    codewhale_skill_install::install::InstallSource::Registry(_),
                ) => "Registry".to_string(),
            };
            MarketplaceInstallPlan::Supported {
                spec: source.to_string(),
                source_kind,
            }
        }
        Err(err) => MarketplaceInstallPlan::Unsupported {
            reason: format!("invalid Codewhale install spec: {err}"),
            raw: source.to_string(),
        },
    };
    let mut normalized = normalize_native_source(source);
    if let Some(pin) = obj.get("sha256").filter(|value| !value.is_null()) {
        let pinned = pin
            .as_str()
            .ok_or_else(|| "sha256 must be a string".to_string())
            .and_then(|pin| PluginInstallSource::pin_archive_spec(source, pin));
        match pinned {
            Ok((pinned_spec, digest)) => {
                if let MarketplaceInstallPlan::Supported { spec, .. } = &mut install_plan {
                    *spec = pinned_spec;
                }
                if let MarketplaceSourceSpec::ArchiveUrl { sha256, .. } = &mut normalized {
                    *sha256 = Some(digest);
                }
            }
            Err(reason) => entry_diags.push(MarketplaceDiagnostic::error(
                "INVALID_SHA256",
                reason,
                Some(name.clone()),
                Some(index),
            )),
        }
    }

    let (description, bad_desc) = str_field(obj, "description");
    if let Some(diag) = bad_desc {
        entry_diags.push(diag);
    }
    let (version, bad_version) = str_field(obj, "version");
    if let Some(diag) = bad_version {
        entry_diags.push(diag);
    }
    let (homepage, bad_home) = str_field(obj, "homepage");
    if let Some(diag) = bad_home {
        entry_diags.push(diag);
    }

    let mut labels = Vec::new();
    for field in ["display_name", "author", "icon"] {
        let (value, bad) = str_field(obj, field);
        if let Some(diag) = bad {
            entry_diags.push(diag);
        }
        let bounded = value.filter(|text| {
            field == "icon" || (text.chars().count() <= 128 && !text.chars().any(char::is_control))
        });
        if value.is_some() && bounded.is_none() {
            entry_diags.push(MarketplaceDiagnostic::error(
                "INVALID_IDENTITY",
                format!("{field} must fit on one line within 128 characters"),
                Some(name.clone()),
                Some(index),
            ));
        }
        labels.push(bounded.map(ToString::to_string));
    }
    let mut icon = labels.pop().flatten();
    if let Some(value) = &icon
        && let Err(reason) = crate::manifest::validate_icon(value)
    {
        entry_diags.push(MarketplaceDiagnostic::error(
            "INVALID_ICON",
            reason,
            Some(name.clone()),
            Some(index),
        ));
        icon = None;
    }
    let author = labels.pop().flatten();
    let display_name = labels.pop().flatten();
    let platforms = obj
        .get("platforms")
        .and_then(Value::as_array)
        .filter(|values| {
            values.len() <= 3
                && values
                    .iter()
                    .enumerate()
                    .all(|(i, value)| !values[..i].contains(value))
        })
        .and_then(|values| {
            values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .filter(|os| ["macos", "linux", "windows"].contains(os))
                        .map(ToString::to_string)
                })
                .collect::<Option<Vec<_>>>()
        });
    if obj.contains_key("platforms") && platforms.is_none() {
        entry_diags.push(MarketplaceDiagnostic::error(
            "INVALID_PLATFORMS",
            "platforms must be an array of macos, linux or windows",
            Some(name.clone()),
            Some(index),
        ));
    }
    // What this entry is. The Codewhale marketplace keeps plugins and
    // skills in separate top-level directories, so the source path is the
    // signal; a document may also declare `kind` explicitly. A skill entry
    // stays installable, but it is not a plugin and is never suggested as
    // one (#6290 rework).
    let kind = match obj.get("kind").and_then(Value::as_str) {
        Some("skill") => MarketplaceEntryKind::Skill,
        Some("plugin") | None => {
            let path = source
                .strip_prefix("path:")
                .unwrap_or(source)
                .trim_start_matches("./");
            if path == "skills" || path.starts_with("skills/") {
                MarketplaceEntryKind::Skill
            } else {
                MarketplaceEntryKind::Plugin
            }
        }
        Some(other) => {
            entry_diags.push(MarketplaceDiagnostic::warning(
                "UNKNOWN_ENTRY_KIND",
                format!("unknown entry kind `{other}`; treated as a plugin"),
                Some(name.clone()),
                Some(index),
            ));
            MarketplaceEntryKind::Plugin
        }
    };
    Some(MarketplaceCandidate {
        id: MarketplaceCandidateId::new(catalog_id, &name),
        catalog_id: catalog_id.clone(),
        kind,
        icon,
        name,
        display_name,
        description: description.map(ToString::to_string),
        version: version.map(ToString::to_string),
        author,
        homepage: homepage.map(ToString::to_string),
        repository: None,
        license: None,
        keywords: Vec::new(),
        categories: Vec::new(),
        source: normalized,
        install_plan,
        declared_components: None,
        compatibility: None,
        provenance: CatalogProvenance {
            tier: CatalogTier::Community,
            publisher: None,
            source_url: None,
        },
        when: platforms.map(|os| crate::manifest::PluginWhen {
            os: Some(os),
            binaries: None,
        }),
        diagnostics: entry_diags,
    })
}

fn normalize_native_source(spec: &str) -> MarketplaceSourceSpec {
    match PluginInstallSource::parse(spec) {
        Ok(PluginInstallSource::LocalPath(path) | PluginInstallSource::Dsh(path)) => {
            MarketplaceSourceSpec::LocalPath { path }
        }
        Ok(PluginInstallSource::Remote(
            codewhale_skill_install::install::InstallSource::GitHubRepo(repo),
        )) => {
            let (owner, name) = repo.split_once('/').unwrap_or((&repo, ""));
            MarketplaceSourceSpec::GitHub {
                owner: owner.to_string(),
                repo: name.to_string(),
                git_ref: None,
                sha: None,
            }
        }
        Ok(PluginInstallSource::Remote(
            codewhale_skill_install::install::InstallSource::DirectUrl(url),
        )) => MarketplaceSourceSpec::ArchiveUrl { url, sha256: None },
        Ok(other) => MarketplaceSourceSpec::Invalid {
            reason: format!("registry source {other:?} is not a marketplace install"),
        },
        Err(err) => MarketplaceSourceSpec::Invalid {
            reason: err.to_string(),
        },
    }
}
