//! Catalog parsers, one per real published schema.
//!
//! Each parser consumes only fields its format documents. Unknown fields
//! produce visible warnings, never silent acceptance and never invented
//! fallbacks: a field the format does not define is not parsed as some
//! plausible equivalent. There is no network access at this layer.

pub mod claude;
pub mod codewhale;
pub mod codex;
pub mod dsh;
pub mod kimi;

use serde_json::Value;

use crate::manifest::{PluginCompatibility, PluginInventory};

use super::types::{
    MarketplaceCatalog, MarketplaceCatalogId, MarketplaceDiagnostic, MarketplaceFormat,
    MarketplaceInstallPlan, MarketplaceSourceSpec,
};

/// Catalog-side compatibility. Unlike an installed bundle's reviewed
/// inventory, catalog declarations do not state MCP transport, so any
/// declared `mcp_servers` count is treated as a supported-capable
/// declaration; the install-time review decides the real activation
/// policy binding.
pub(super) fn declared_catalog_compatibility(declared: &PluginInventory) -> PluginCompatibility {
    let supported = declared.skills > 0 || declared.mcp_servers > 0;
    let unsupported = declared.commands > 0
        || declared.agents > 0
        || declared.hooks > 0
        || declared.lsp > 0
        || declared.native > 0
        || !declared.filesystem_roots.is_empty()
        || !declared.network_hosts.is_empty()
        || declared.lifecycle_mutation;
    match (supported, unsupported) {
        (true, false) => PluginCompatibility::Full,
        (true, true) => PluginCompatibility::Partial,
        (false, true) => PluginCompatibility::Unsupported,
        (false, false) => PluginCompatibility::Full,
    }
}

/// Input to catalog parsing. `base` is where the document was read from
/// (path or URL) — kept for install-time resolution of relative sources;
/// the parser itself never touches it.
pub struct MarketplaceDocument {
    pub catalog_id: MarketplaceCatalogId,
    pub format: MarketplaceFormat,
    pub root: Value,
    pub base: Option<String>,
}

/// Parse one catalog document. `MarketplaceFormat::Auto` detects the
/// format from documented structural markers only, and reports ambiguity
/// as an error diagnostic rather than guessing.
pub fn parse_catalog(document: MarketplaceDocument) -> MarketplaceCatalog {
    let format = match document.format {
        MarketplaceFormat::Auto => match detect_format(&document.root) {
            Ok(format) => format,
            Err(diagnostic) => {
                let name = document.catalog_id.as_str().to_string();
                return MarketplaceCatalog {
                    id: document.catalog_id,
                    format: MarketplaceFormat::Auto,
                    name,
                    display_name: None,
                    description: None,
                    version: None,
                    base: document.base,
                    provenance: super::types::CatalogProvenance::default(),
                    candidates: Vec::new(),
                    diagnostics: vec![diagnostic],
                };
            }
        },
        explicit => explicit,
    };

    let mut catalog = match format {
        MarketplaceFormat::Kimi => kimi::parse_kimi_catalog(document),
        MarketplaceFormat::Claude => claude::parse_claude_catalog(document),
        MarketplaceFormat::Codex => codex::parse_codex_catalog(document),
        MarketplaceFormat::Codewhale => codewhale::parse_codewhale_catalog(document),
        MarketplaceFormat::Dsh => dsh::parse_dsh_catalog(document),
        MarketplaceFormat::Auto => unreachable!("resolved above"),
    };
    flag_duplicate_names(&mut catalog);
    catalog
}

/// A candidate's name is its identity: `/plugin show`, install, and registry
/// lookups all resolve by it, and a lookup returns the first match. Entries
/// sharing a name (after each format's normalization) are ambiguous, so every
/// one of them carries an error and none installs from the catalog.
fn flag_duplicate_names(catalog: &mut super::types::MarketplaceCatalog) {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for candidate in &catalog.candidates {
        *counts.entry(candidate.name.clone()).or_default() += 1;
    }
    for candidate in &mut catalog.candidates {
        let count = counts.get(&candidate.name).copied().unwrap_or(0);
        if count > 1 {
            candidate.diagnostics.push(MarketplaceDiagnostic::error(
                "DUPLICATE_NAME",
                format!(
                    "{count} catalog entries are named `{}`; the entry's identity is ambiguous",
                    candidate.name
                ),
                Some(candidate.name.clone()),
                None,
            ));
        }
    }
}

/// Detection uses only markers each format's own documentation defines:
///
/// - **Kimi**: `plugins[]` whose entries carry `id` + `source` (Kimi uses
///   `id`; the Claude-family formats use `name`).
/// - **Claude**: top-level `owner` object, a `plugins[]` entry with a
///   `source` object using the Claude discriminators (`github`, `url`,
///   `git-subdir`, `npm`, `archive`, `command`), or `metadata.pluginRoot`.
/// - **Codex**: a `plugins[]` entry with a `policy` object, a `source`
///   object with the `local` discriminator, or a top-level `interface`
///   object.
/// - **Codewhale**: `plugins[]` entries with `name` + a string `source`
///   that is a Codewhale install spec (`github:`, `path:`, URL).
/// - **DSH**: a top-level numeric `format` (the DSH plugin market index
///   format; version `1` is the only one defined, and any other value is
///   reported by the parser rather than guessed).
///
/// Documents matching no documented marker are ambiguous, not guessed.
fn detect_format(root: &Value) -> Result<MarketplaceFormat, MarketplaceDiagnostic> {
    let Some(obj) = root.as_object() else {
        return Err(MarketplaceDiagnostic::error(
            "NOT_AN_OBJECT",
            "marketplace catalog must be a JSON object",
            None,
            None,
        ));
    };
    let Some(entries) = obj.get("plugins").and_then(Value::as_array) else {
        return Err(MarketplaceDiagnostic::error(
            "UNKNOWN_FORMAT",
            "catalog has no documented marker: expected Kimi `plugins` with `id` entries, \
             Claude `owner`/`plugins`, Codex `policy`/`interface`, DSH `format: 1` with \
             `versions` entries, or Codewhale `plugins` with install-spec sources",
            None,
            None,
        ));
    };

    // DSH market index: the format declares a numeric `format`, and 1 is the
    // only version of it. Any numeric `format` routes here so the parser can
    // report the version it cannot read instead of calling the document
    // ambiguous. No other supported format declares a numeric `format`.
    if obj.get("format").is_some_and(Value::is_number) {
        return Ok(MarketplaceFormat::Dsh);
    }

    // Claude: `owner` and `metadata.pluginRoot` are documented top-level
    // fields no other format defines.
    if obj.contains_key("owner")
        || obj
            .get("metadata")
            .and_then(|m| m.get("pluginRoot"))
            .is_some()
    {
        return Ok(MarketplaceFormat::Claude);
    }

    let entry_markers: Vec<MapMarker> = entries
        .iter()
        .map(|entry| {
            let entry_obj = entry.as_object();
            MapMarker {
                has_id: entry_obj.is_some_and(|o| o.contains_key("id")),
                has_name: entry_obj.is_some_and(|o| o.contains_key("name")),
                source_kind: entry_obj.and_then(|o| o.get("source")).map(source_marker),
                has_policy: entry_obj.is_some_and(|o| o.contains_key("policy")),
            }
        })
        .collect();

    // Codex: `policy` blocks or `local` sources are Codex-only markers.
    if entry_markers
        .iter()
        .any(|m| m.has_policy || m.source_kind == Some(SourceMarker::Local))
        || obj.contains_key("interface")
    {
        return Ok(MarketplaceFormat::Codex);
    }

    // Claude source discriminators other than `local`.
    if entry_markers.iter().any(|m| {
        matches!(
            m.source_kind,
            Some(SourceMarker::Github)
                | Some(SourceMarker::Url)
                | Some(SourceMarker::GitSubdir)
                | Some(SourceMarker::Npm)
                | Some(SourceMarker::Archive)
                | Some(SourceMarker::Command)
        )
    }) {
        return Ok(MarketplaceFormat::Claude);
    }

    // Kimi: `id`-keyed entries with string sources.
    if entry_markers.iter().any(|m| {
        m.has_id
            && matches!(
                m.source_kind,
                Some(SourceMarker::String | SourceMarker::UrlString)
            )
    }) {
        return Ok(MarketplaceFormat::Kimi);
    }

    // Codewhale: `name` entries with install-spec string sources.
    if entry_markers.iter().any(|m| {
        m.has_name
            && matches!(
                m.source_kind,
                Some(SourceMarker::InstallSpec | SourceMarker::UrlString)
            )
    }) {
        return Ok(MarketplaceFormat::Codewhale);
    }

    Err(MarketplaceDiagnostic::error(
        "AMBIGUOUS_FORMAT",
        "catalog markers match no documented format uniquely; pass the format explicitly",
        None,
        None,
    ))
}

struct MapMarker {
    has_id: bool,
    has_name: bool,
    source_kind: Option<SourceMarker>,
    has_policy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceMarker {
    String,
    UrlString,
    InstallSpec,
    Github,
    Url,
    GitSubdir,
    Npm,
    Archive,
    Command,
    Local,
    OtherObject,
}

fn source_marker(source: &Value) -> SourceMarker {
    match source {
        Value::String(s) => {
            if s.starts_with("github:") || s.starts_with("path:") {
                SourceMarker::InstallSpec
            } else if s.starts_with("https://") || s.starts_with("http://") {
                SourceMarker::UrlString
            } else {
                SourceMarker::String
            }
        }
        Value::Object(o) => match o.get("source").and_then(Value::as_str) {
            Some("github") => SourceMarker::Github,
            Some("url") => SourceMarker::Url,
            Some("git-subdir") => SourceMarker::GitSubdir,
            Some("npm") => SourceMarker::Npm,
            Some("archive") => SourceMarker::Archive,
            Some("command") => SourceMarker::Command,
            Some("local") => SourceMarker::Local,
            _ => SourceMarker::OtherObject,
        },
        _ => SourceMarker::OtherObject,
    }
}

/// Shared helper: read a documented string field; a wrong-typed value is
/// a per-entry warning and treated as absent, not a guess.
pub(super) fn str_field<'a>(
    entry: &'a serde_json::Map<String, Value>,
    field: &str,
) -> (Option<&'a str>, Option<MarketplaceDiagnostic>) {
    match entry.get(field) {
        None | Some(Value::Null) => (None, None),
        Some(Value::String(s)) => (Some(s.as_str()), None),
        Some(other) => (
            None,
            Some(MarketplaceDiagnostic::warning(
                "FIELD_TYPE",
                format!("field `{field}` must be a string, got {}", type_name(other)),
                None,
                None,
            )),
        ),
    }
}

/// Shared helper: read a documented string-array field.
pub(super) fn str_array_field(
    entry: &serde_json::Map<String, Value>,
    field: &str,
) -> (Vec<String>, Option<MarketplaceDiagnostic>) {
    match entry.get(field) {
        None | Some(Value::Null) => (Vec::new(), None),
        Some(Value::Array(items)) => {
            let mut out = Vec::new();
            let mut skipped = false;
            for item in items {
                match item {
                    Value::String(s) => out.push(s.clone()),
                    _ => skipped = true,
                }
            }
            let diag = skipped.then(|| {
                MarketplaceDiagnostic::warning(
                    "FIELD_TYPE",
                    format!(
                        "field `{field}` must be an array of strings; non-string items skipped"
                    ),
                    None,
                    None,
                )
            });
            (out, diag)
        }
        Some(other) => (
            Vec::new(),
            Some(MarketplaceDiagnostic::warning(
                "FIELD_TYPE",
                format!(
                    "field `{field}` must be an array of strings, got {}",
                    type_name(other)
                ),
                None,
                None,
            )),
        ),
    }
}

/// Shared helper: warn on fields this format does not document. Unknown
/// fields are preserved in diagnostics so catalog authors see them; they
/// are never silently reinterpreted.
pub(super) fn unknown_fields_warning(
    entry: &serde_json::Map<String, Value>,
    known: &[&str],
) -> Option<MarketplaceDiagnostic> {
    let unknown: Vec<&String> = entry
        .keys()
        .filter(|k| !known.contains(&k.as_str()))
        .collect();
    if unknown.is_empty() {
        return None;
    }
    Some(MarketplaceDiagnostic::warning(
        "UNKNOWN_FIELD",
        format!(
            "undeclared field(s) ignored: {}",
            unknown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None,
        None,
    ))
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
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

type GitSourcePlan = (
    MarketplaceSourceSpec,
    MarketplaceInstallPlan,
    Vec<MarketplaceDiagnostic>,
);

fn git_url_source(value: &Value) -> GitSourcePlan {
    let url = value["url"].as_str().unwrap_or_default();
    if value["source"] == "url"
        && let Some((owner, repo)) = github_url_parts(url)
    {
        return github_source(value, &owner, &repo);
    }
    let reason = if value["source"] == "git-subdir" {
        "Git subdirectory installs are unavailable. Install a reviewed local copy of the requested subdirectory."
    } else {
        "Git URL sources must point to a GitHub repository."
    };
    (
        MarketplaceSourceSpec::GitUrl {
            url: url.to_string(),
        },
        MarketplaceInstallPlan::Unsupported {
            reason: reason.to_string(),
            raw: value.to_string(),
        },
        Vec::new(),
    )
}

fn github_source(value: &Value, owner: &str, repo: &str) -> GitSourcePlan {
    let git_ref = value.get("ref").and_then(Value::as_str);
    let sha = value.get("sha").and_then(Value::as_str);
    let source = MarketplaceSourceSpec::GitHub {
        owner: owner.to_string(),
        repo: repo.to_string(),
        git_ref: git_ref.map(str::to_string),
        sha: sha.map(str::to_string),
    };
    if value.get("path").is_some() {
        return (
            source,
            MarketplaceInstallPlan::Unsupported {
                reason: "Git subdirectory installs are unavailable. Install a reviewed local copy of the requested subdirectory.".to_string(),
                raw: value.to_string(),
            },
            Vec::new(),
        );
    }
    let mut spec = format!("git:github.com/{owner}/{repo}");
    if let Some(revision) = sha.or(git_ref) {
        spec.push('@');
        spec.push_str(revision);
    }
    let valid_sha = value.get("sha").is_none()
        || sha.is_some_and(|pin| pin.len() == 40 && pin.bytes().all(|b| b.is_ascii_hexdigit()));
    let valid_ref =
        value.get("ref").is_none() || git_ref.is_some_and(|revision| !revision.is_empty());
    let plan = if !repo.contains('@')
        && spec.trim() == spec
        && valid_sha
        && valid_ref
        && crate::install::PluginInstallSource::parse(&spec).is_ok()
    {
        MarketplaceInstallPlan::Supported {
            spec: if git_ref.is_some() || sha.is_some() {
                spec
            } else {
                format!("github:{owner}/{repo}")
            },
            source_kind: "GitHub repository".to_string(),
        }
    } else {
        MarketplaceInstallPlan::Unsupported { reason: "The GitHub repository or requested revision cannot be installed exactly as declared.".to_string(), raw: value.to_string() }
    };
    (source, plan, Vec::new())
}
