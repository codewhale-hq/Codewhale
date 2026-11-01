//! Claude `.claude-plugin/marketplace.json` parser.
//!
//! Schema source: <https://code.claude.com/docs/en/plugin-marketplaces>
//!
//! Top level: `name` (kebab-case, required), `owner` (`{name`, `email?`,
//! `url?}`, required), `plugins[]` (required); optional `$schema`,
//! `description`, `version`, `metadata.pluginRoot`, `renames`.
//!
//! Plugin entry: `name` (kebab-case) and `source` (required). `source`
//! is either a `./`-relative string or an object tagged with its
//! `source` discriminator: `github` (`repo`, `ref?`, `sha?`), `url`
//! (`url`, `ref?`, `sha?`), `git-subdir` (`url`, `path`, `ref?`, `sha?`),
//! `npm` (`package`, `version?`, `registry?`), `archive` (`url`,
//! `sha256?`), `command` (`command`, `timeout?`, `mode?`). Optional
//! entry fields: `displayName`, `description`, `version`, `author`,
//! `homepage`, `repository`, `license`, `keywords`, `category`, `tags`,
//! `strict`, and component config (`skills`, `commands`, `agents`,
//! `hooks`, `mcpServers`, `lspServers`) which this parser counts for
//! display only.
//!
//! No invented `entries`, `npm_package`, or `download_url` fields exist
//! in this format and none are parsed.

use serde_json::Value;

use crate::agent_plugin::{is_standard_plugin_name, slugify_plugin_name};
use crate::manifest::PluginInventory;

use super::super::types::{
    CatalogProvenance, CatalogTier, MarketplaceCandidate, MarketplaceCandidateId,
    MarketplaceCatalog, MarketplaceDiagnostic, MarketplaceEntryKind, MarketplaceFormat,
    MarketplaceInstallPlan, MarketplaceSourceSpec,
};
use super::{MarketplaceDocument, str_array_field, str_field, unknown_fields_warning};

const TOP_LEVEL_FIELDS: &[&str] = &[
    "$schema",
    "name",
    "owner",
    "plugins",
    "description",
    "version",
    "metadata",
    "renames",
    "allowCrossMarketplaceDependenciesOn",
];
const ENTRY_FIELDS: &[&str] = &[
    "name",
    "source",
    "displayName",
    "description",
    "version",
    "author",
    "homepage",
    "repository",
    "license",
    "keywords",
    "category",
    "tags",
    "strict",
    "relevance",
    "defaultEnabled",
    "skills",
    "commands",
    "agents",
    "hooks",
    "mcpServers",
    "lspServers",
    "metadata",
];

pub fn parse_claude_catalog(document: MarketplaceDocument) -> MarketplaceCatalog {
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
                "Claude marketplace must be a JSON object",
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

    let (description, bad_desc) = str_field(obj, "description");
    if let Some(diag) = bad_desc {
        diagnostics.push(diag);
    }
    let (version, bad_version) = str_field(obj, "version");
    if let Some(diag) = bad_version {
        diagnostics.push(diag);
    }

    // `owner` is a documented required object with `name` required.
    let publisher = obj
        .get("owner")
        .and_then(|o| o.get("name"))
        .and_then(|n| n.as_str());
    if obj
        .get("owner")
        .is_some_and(|o| o.get("name").and_then(Value::as_str).is_none())
    {
        diagnostics.push(MarketplaceDiagnostic::warning(
            "FIELD_TYPE",
            "Claude `owner.name` is missing or not a string",
            None,
            None,
        ));
    }

    let Some(entries) = obj.get("plugins").and_then(Value::as_array) else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MISSING_PLUGINS",
            "Claude marketplace must contain a `plugins` array",
            None,
            None,
        ));
        return MarketplaceCatalog {
            id: catalog_id,
            format: MarketplaceFormat::Claude,
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
        if let Some(candidate) =
            parse_claude_entry(&catalog_id, index, entry, publisher, &mut diagnostics)
        {
            candidates.push(candidate);
        }
    }

    MarketplaceCatalog {
        id: catalog_id,
        format: MarketplaceFormat::Claude,
        name,
        display_name: None,
        description: description.map(ToString::to_string),
        version: version.map(ToString::to_string),
        base,
        provenance: CatalogProvenance {
            tier: CatalogTier::Community,
            publisher: publisher.map(ToString::to_string),
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
        format: MarketplaceFormat::Claude,
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

fn parse_claude_entry(
    catalog_id: &super::super::types::MarketplaceCatalogId,
    index: usize,
    entry: &Value,
    catalog_publisher: Option<&str>,
    diagnostics: &mut Vec<MarketplaceDiagnostic>,
) -> Option<MarketplaceCandidate> {
    let Some(obj) = entry.as_object() else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MALFORMED_ENTRY",
            format!("Claude plugin at index {index} must be a JSON object"),
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
            format!("Claude plugin at index {index} is missing required `name`"),
            None,
            Some(index),
        ));
        return None;
    };
    let (name, display_name) = if is_standard_plugin_name(raw_name) {
        (raw_name.to_string(), None)
    } else {
        match slugify_plugin_name(raw_name) {
            Ok(slug) => {
                entry_diags.push(MarketplaceDiagnostic::warning(
                    "NON_STANDARD_NAME",
                    format!("Claude name `{raw_name}` normalized to `{slug}`"),
                    Some(slug.clone()),
                    Some(index),
                ));
                (slug, Some(raw_name.to_string()))
            }
            Err(err) => {
                diagnostics.push(MarketplaceDiagnostic::error(
                    "INVALID_NAME",
                    format!("Claude name `{raw_name}` cannot be normalized: {err}"),
                    Some(raw_name.to_string()),
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

    let Some(source_value) = obj.get("source") else {
        diagnostics.push(MarketplaceDiagnostic::error(
            "MISSING_SOURCE",
            format!("Claude plugin `{name}` is missing required `source`"),
            Some(name.clone()),
            Some(index),
        ));
        return None;
    };
    let (source, install_plan, source_diags) = normalize_claude_source(source_value);
    entry_diags.extend(source_diags);

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
    let (repository, bad_repo) = str_field(obj, "repository");
    if let Some(diag) = bad_repo {
        entry_diags.push(diag);
    }
    let (license, bad_license) = str_field(obj, "license");
    if let Some(diag) = bad_license {
        entry_diags.push(diag);
    }
    let (keywords, bad_keywords) = str_array_field(obj, "keywords");
    if let Some(diag) = bad_keywords {
        entry_diags.push(diag);
    }
    let (tags, bad_tags) = str_array_field(obj, "tags");
    if let Some(diag) = bad_tags {
        entry_diags.push(diag);
    }
    let (category, bad_category) = str_field(obj, "category");
    if let Some(diag) = bad_category {
        entry_diags.push(diag);
    }
    let author = obj
        .get("author")
        .and_then(|a| a.get("name"))
        .and_then(|n| n.as_str());

    let (declared, component_diags) = count_declared_components(obj);
    entry_diags.extend(component_diags);
    let compatibility = declared.as_ref().map(super::declared_catalog_compatibility);

    if obj.get("strict") == Some(&Value::Bool(false)) {
        entry_diags.push(MarketplaceDiagnostic::warning(
            "NON_STRICT_ENTRY",
            format!("Claude plugin `{name}` sets strict=false; Codewhale always reviews the installed manifest"),
            Some(name.clone()),
            Some(index),
        ));
    }

    Some(MarketplaceCandidate {
        id: MarketplaceCandidateId::new(catalog_id, &name),
        catalog_id: catalog_id.clone(),
        kind: MarketplaceEntryKind::Plugin,
        icon: None,
        name,
        display_name,
        description: description.map(ToString::to_string),
        version: version.map(ToString::to_string),
        author: author
            .map(ToString::to_string)
            .or_else(|| catalog_publisher.map(ToString::to_string)),
        homepage: homepage.map(ToString::to_string),
        repository: repository.map(ToString::to_string),
        license: license.map(ToString::to_string),
        keywords,
        categories: category
            .map(|c| vec![c.to_string()])
            .into_iter()
            .flatten()
            .chain(tags)
            .collect(),
        source,
        install_plan,
        declared_components: declared,
        compatibility,
        provenance: CatalogProvenance {
            tier: CatalogTier::Community,
            publisher: author
                .map(ToString::to_string)
                .or_else(|| catalog_publisher.map(ToString::to_string)),
            source_url: None,
        },
        when: None,
        diagnostics: entry_diags,
    })
}

/// Claude documents per-entry component config arrays (`skills`,
/// `commands`, `agents`, `hooks`, `lspServers`) and an `mcpServers` map.
/// These are catalog-side declarations; Codewhale counts them for
/// display and compatibility only — the reviewed installed manifest
/// remains the authority.
fn count_declared_components(
    obj: &serde_json::Map<String, Value>,
) -> (Option<PluginInventory>, Vec<MarketplaceDiagnostic>) {
    let has_any = [
        "skills",
        "commands",
        "agents",
        "hooks",
        "mcpServers",
        "lspServers",
    ]
    .iter()
    .any(|k| obj.contains_key(*k));
    if !has_any {
        return (None, Vec::new());
    }
    let mut diags = Vec::new();
    let count = |key: &str, diags: &mut Vec<MarketplaceDiagnostic>| -> usize {
        match obj.get(key) {
            None | Some(Value::Null) => 0,
            Some(Value::Array(items)) => items.len(),
            Some(other) => {
                diags.push(MarketplaceDiagnostic::warning(
                    "FIELD_TYPE",
                    format!("`{key}` must be an array, got {};", json_kind(other)),
                    None,
                    None,
                ));
                0
            }
        }
    };
    let mcp_servers = match obj.get("mcpServers") {
        None | Some(Value::Null) => 0,
        Some(Value::Object(map)) => map.len(),
        Some(other) => {
            diags.push(MarketplaceDiagnostic::warning(
                "FIELD_TYPE",
                format!("`mcpServers` must be an object, got {}", json_kind(other)),
                None,
                None,
            ));
            0
        }
    };
    (
        Some(PluginInventory {
            skills: count("skills", &mut diags),
            mcp_servers,
            stdio_mcp_servers: 0,
            remote_mcp_servers: 0,
            commands: count("commands", &mut diags),
            agents: count("agents", &mut diags),
            hooks: count("hooks", &mut diags),
            lsp: count("lspServers", &mut diags),
            native: 0,
            providers: 0,
            filesystem_roots: Vec::new(),
            network_hosts: Vec::new(),
            lifecycle_mutation: false,
        }),
        diags,
    )
}

/// Normalize a documented Claude `source`. Only the six documented
/// discriminators plus the `./`-relative string form are accepted.
fn normalize_claude_source(
    value: &Value,
) -> (
    MarketplaceSourceSpec,
    MarketplaceInstallPlan,
    Vec<MarketplaceDiagnostic>,
) {
    let mut diags = Vec::new();
    if let Some(s) = value.as_str() {
        if s.starts_with("./") || s == "." {
            return (
                MarketplaceSourceSpec::LocalPath { path: s.into() },
                MarketplaceInstallPlan::Supported {
                    spec: format!("path:{s}"),
                    source_kind: "Marketplace-relative directory".to_string(),
                },
                diags,
            );
        }
        return (
            MarketplaceSourceSpec::Invalid {
                reason: format!("Claude string sources must be `./`-relative (got `{s}`)"),
            },
            MarketplaceInstallPlan::Unsupported {
                reason: "Claude string sources must be `./`-relative paths".to_string(),
                raw: s.to_string(),
            },
            diags,
        );
    }
    let Some(obj) = value.as_object() else {
        return (
            MarketplaceSourceSpec::Invalid {
                reason: "source must be a string or a tagged object".to_string(),
            },
            MarketplaceInstallPlan::Unsupported {
                reason: "Claude source must be a `./` string or a `{source: ...}` object"
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
        "github" => {
            let Some(repo) = field("repo") else {
                return invalid("github source requires `repo` (\"owner/repo\")", value, &mut diags);
            };
            let (owner, repo) = match repo.split_once('/') {
                Some((o, r)) if !o.is_empty() && !r.is_empty() => (o, r),
                _ => return invalid("github `repo` must be \"owner/repo\"", value, &mut diags),
            };
            let git_ref = field("ref").map(ToString::to_string);
            let sha = field("sha").map(ToString::to_string);
            if git_ref.is_some() || sha.is_some() {
                diags.push(MarketplaceDiagnostic::warning(
                    "UNAPPLIED_PIN",
                    "Codewhale's GitHub installer resolves the default branch; ref/sha pins are recorded but not yet applied".to_string(),
                    None,
                    None,
                ));
            }
            (
                MarketplaceSourceSpec::GitHub { owner: owner.to_string(), repo: repo.to_string(), git_ref, sha },
                MarketplaceInstallPlan::Supported {
                    spec: format!("github:{owner}/{repo}"),
                    source_kind: "GitHub repository".to_string(),
                },
                diags,
            )
        }
        "url" | "git-subdir" => {
            let Some(url) = field("url") else {
                return invalid(&format!("{discriminator} source requires `url`"), value, &mut diags);
            };
            if let Some((owner, repo)) = github_url_parts(url) {
                diags.push(MarketplaceDiagnostic::warning(
                    "UNAPPLIED_PIN",
                    "Codewhale's GitHub installer resolves the default branch; ref/sha pins are recorded but not yet applied".to_string(),
                    None,
                    None,
                ));
                let spec = format!("github:{owner}/{repo}");
                return (
                    MarketplaceSourceSpec::GitHub { owner, repo, git_ref: field("ref").map(ToString::to_string), sha: field("sha").map(ToString::to_string) },
                    MarketplaceInstallPlan::Supported {
                        spec,
                        source_kind: "GitHub repository".to_string(),
                    },
                    diags,
                );
            }
            (
                MarketplaceSourceSpec::GitUrl { url: url.to_string() },
                MarketplaceInstallPlan::Unsupported {
                    reason: format!("{discriminator} installs of non-GitHub git URLs are not supported yet"),
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
        "archive" => {
            let Some(url) = field("url") else {
                return invalid("archive source requires `url`", value, &mut diags);
            };
            let (spec, sha256) = match obj.get("sha256").filter(|pin| !pin.is_null()) {
                None => (url.to_string(), None),
                Some(pin) => match pin
                    .as_str()
                    .ok_or_else(|| "archive `sha256` must be a string".to_string())
                    .and_then(|pin| {
                        crate::install::PluginInstallSource::pin_archive_spec(url, pin)
                    })
                {
                    Ok((spec, digest)) => (spec, Some(digest)),
                    Err(reason) => return invalid(&reason, value, &mut diags),
                },
            };
            (
                MarketplaceSourceSpec::ArchiveUrl { url: url.to_string(), sha256 },
                MarketplaceInstallPlan::Supported {
                    spec,
                    source_kind: "Tarball archive URL".to_string(),
                },
                diags,
            )
        }
        "command" => (
            MarketplaceSourceSpec::Refused {
                reason: "Claude `command` sources execute arbitrary shell commands and are never run by Codewhale".to_string(),
            },
            MarketplaceInstallPlan::Unsupported {
                reason: "command sources are never executed".to_string(),
                raw: value.to_string(),
            },
            diags,
        ),
        other => invalid(
            &format!("unknown Claude source discriminator `{other}`"),
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

fn github_url_parts(url: &str) -> Option<(String, String)> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let mut parts = rest.trim_end_matches('/').split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(repo), None) if !owner.is_empty() && !repo.is_empty() => {
            Some((owner.to_string(), repo.trim_end_matches(".git").to_string()))
        }
        _ => None,
    }
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}
