//! Named fleet roster files for dogfood lanes (#4178).
//!
//! Format: TOML at `fleets/<name>.toml` (workspace) or
//! `$CODEWHALE_HOME/fleets/<name>.toml`.
//!
//! Two forms share this one store — there is no parallel fleet directory:
//!
//! - **Legacy**: `[roles]` maps role → AgentProfile id. Fleet resolves roles →
//!   profile ids only; Runtime owns tmux/worktrees. Legacy files declare no
//!   `schema` key, which is what makes the form explicitly detectable rather
//!   than guessed from a missing table.
//! - **Exact**: `schema = "exact"` with fully resolved `[[members]]`. See
//!   [`crate::fleet_exact`].
//!
//! [`FleetDocument`] is the form-agnostic entry point: it reports which form a
//! file is in and carries the content hash a Workflow snapshot records.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::fleet_exact::{
    EXACT_FLEET_SCHEMA_KIND, EXACT_FLEET_SCHEMA_REVISION, ExactFleet, ExactFleetError,
    LEGACY_FLEET_SCHEMA_KIND, declared_schema_kind,
};
use crate::fleet_snapshot::QualifiedFleetId;

/// One labelled place fleet files are looked up.
///
/// The label is what makes a Fleet identity *qualified*: `workspace/glm-pair`
/// and `codewhale_home/glm-pair` are different Fleets, and the loader refuses
/// to guess between them for exact definitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetSearchRoot {
    /// Non-secret origin label, e.g. `workspace` or `codewhale_home`.
    pub origin: String,
    /// Directory that contains a `fleets/` subdirectory.
    pub root: PathBuf,
}

impl FleetSearchRoot {
    pub fn new(origin: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            origin: origin.into(),
            root: root.into(),
        }
    }
}

/// Split `origin/name` into its parts. A bare name yields `(None, name)`.
///
/// The bare part becomes a `<name>.toml` file name, so it is validated here
/// with [`validate_fleet_file_stem`] before any caller touches the disk.
pub fn split_qualified_fleet_name(name: &str) -> Result<(Option<&str>, &str), NamedFleetError> {
    let trimmed = name.trim();
    let (origin, bare) = match trimmed.split_once('/') {
        Some((origin, bare)) if !origin.trim().is_empty() && !bare.trim().is_empty() => {
            (Some(origin.trim()), bare.trim())
        }
        _ => (None, trimmed),
    };
    validate_fleet_file_stem(bare)?;
    Ok((origin, bare))
}

/// A fleet or router name is one file name inside a `fleets/`-style
/// directory: non-empty, a single normal path component, and free of path
/// separators, drive prefixes and NUL. Anything else is refused before a
/// path is built from it.
pub fn validate_fleet_file_stem(stem: &str) -> Result<(), NamedFleetError> {
    let single_component = matches!(
        Path::new(stem).components().collect::<Vec<_>>().as_slice(),
        [std::path::Component::Normal(_)]
    );
    if stem.is_empty()
        || stem == "."
        || stem == ".."
        || stem.contains(['/', '\\', ':', '\0'])
        || !single_component
    {
        return Err(NamedFleetError::InvalidName);
    }
    Ok(())
}

/// Parsed named fleet file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedFleet {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// role name → AgentProfile id
    pub roles: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NamedFleetError {
    #[error("fleet file not found: {0}")]
    NotFound(String),
    /// Carries no text from the rejected name, so the refusal cannot echo a
    /// path back.
    #[error(
        "invalid fleet name: use a plain name (optionally `origin/name`) without path \
         separators or `..`"
    )]
    InvalidName,
    #[error("failed to read fleet file {path}: {message}")]
    Io { path: String, message: String },
    #[error("failed to parse fleet file {path}: {message}")]
    Parse { path: String, message: String },
    #[error("fleet `{fleet}` is missing required role `{role}`")]
    MissingRole { fleet: String, role: String },
    #[error("fleet name mismatch: file declares `{declared}`, expected `{expected}`")]
    NameMismatch { declared: String, expected: String },
    #[error(
        "fleet `{name}` is defined in more than one place ({}); an exact fleet must not be \
         resolved by shadowing. Name one explicitly as `origin/{name}`.",
        origins.join(", ")
    )]
    AmbiguousFleet { name: String, origins: Vec<String> },
    #[error("exact fleet `{fleet}`: {source}")]
    Exact {
        fleet: String,
        #[source]
        source: ExactFleetError,
    },
}

/// Which form a `fleets/<name>.toml` file is in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum FleetSchema {
    /// Pre-exact role → AgentProfile id map.
    Legacy(NamedFleet),
    /// Fully resolved exact members.
    Exact(ExactFleet),
}

/// A parsed fleet file plus the provenance a Workflow snapshot needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetDocument {
    schema: FleetSchema,
    source: Option<PathBuf>,
    source_hash: String,
}

impl FleetDocument {
    /// Parse either form. The exact form is selected by an explicit
    /// `schema = "exact"`; everything else is the legacy form.
    pub fn parse(text: &str) -> Result<Self, NamedFleetError> {
        let schema = match declared_schema_kind(text).as_deref() {
            Some(EXACT_FLEET_SCHEMA_KIND) => {
                let exact = ExactFleet::parse(text).map_err(|source| NamedFleetError::Exact {
                    fleet: "<memory>".to_string(),
                    source,
                })?;
                FleetSchema::Exact(exact)
            }
            Some(other) => {
                return Err(NamedFleetError::Parse {
                    path: "<memory>".into(),
                    message: format!(
                        "unknown fleet schema `{other}`; expected `exact` or a saved Fleet \
                         (`schema = \"fleet\"`, loaded from `.codewhale/fleets/` or \
                         `$CODEWHALE_HOME/fleets/`)"
                    ),
                });
            }
            None => FleetSchema::Legacy(parse_named_fleet(text)?),
        };
        Ok(Self {
            schema,
            source: None,
            source_hash: content_hash(text),
        })
    }

    pub fn load(path: &Path, expect_name: Option<&str>) -> Result<Self, NamedFleetError> {
        let text = std::fs::read_to_string(path).map_err(|e| NamedFleetError::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        let mut document = Self::parse(&text).map_err(|e| match e {
            NamedFleetError::Parse { message, .. } => NamedFleetError::Parse {
                path: path.display().to_string(),
                message,
            },
            NamedFleetError::Exact { source, .. } => NamedFleetError::Exact {
                fleet: path.display().to_string(),
                source,
            },
            other => other,
        })?;
        if let Some(expected) = expect_name
            && document.name() != expected
        {
            return Err(NamedFleetError::NameMismatch {
                declared: document.name().to_string(),
                expected: expected.to_string(),
            });
        }
        document.source = Some(path.to_path_buf());
        Ok(document)
    }

    /// Load a fleet document by name from labelled search roots.
    ///
    /// A bare `name` that exists under more than one origin is **ambiguous**
    /// once any candidate is an exact fleet: a personal `~/.codewhale` Fleet
    /// silently shadowing (or being shadowed by) a project Fleet would change
    /// which exact provider/model actually runs, so the caller is asked for a
    /// qualified `origin/name` instead. Purely legacy collisions keep the
    /// historic first-hit-wins behavior, because a role→profile map resolves
    /// through the same profile store either way.
    ///
    /// Ambiguity is decided from a `schema`-key probe, not from a full parse of
    /// every candidate: a malformed file in a *shadowed* origin must not break a
    /// legacy load that has always worked. A file whose TOML does not even
    /// parse therefore counts as legacy for this decision, and the first hit
    /// still wins — the same outcome the pre-exact loader gave.
    ///
    /// Accepts `origin/name` to name one origin explicitly.
    pub fn load_by_name(
        name: &str,
        search_roots: &[FleetSearchRoot],
    ) -> Result<(Self, QualifiedFleetId), NamedFleetError> {
        let (requested_origin, bare_name) = split_qualified_fleet_name(name)?;
        let file_name = format!("{bare_name}.toml");

        let mut candidates: Vec<(&FleetSearchRoot, PathBuf)> = Vec::new();
        for root in search_roots {
            if let Some(origin) = requested_origin
                && !root.origin.eq_ignore_ascii_case(origin)
            {
                continue;
            }
            let path = root.root.join("fleets").join(&file_name);
            if path.is_file() {
                candidates.push((root, path));
            }
        }

        let Some((first_root, first_path)) = candidates.first() else {
            return Err(NamedFleetError::NotFound(name.to_string()));
        };

        if candidates.len() > 1 {
            // Decide ambiguity from the `schema` key alone — a cheap probe that
            // does not parse the rest of the file. Fully parsing every sibling
            // would mean a malformed *shadowed* file could fail a load that
            // legacy first-hit-wins has always satisfied, which is a regression
            // in a path the exact schema was never meant to touch. The
            // ambiguity that actually matters is "one of these is exact", and
            // the probe answers exactly that.
            let mut any_exact = false;
            for (_, path) in &candidates {
                let text = std::fs::read_to_string(path).map_err(|e| NamedFleetError::Io {
                    path: path.display().to_string(),
                    message: e.to_string(),
                })?;
                if declared_schema_kind(&text).is_some() {
                    any_exact = true;
                    break;
                }
            }
            if any_exact {
                return Err(NamedFleetError::AmbiguousFleet {
                    name: bare_name.to_string(),
                    origins: candidates
                        .iter()
                        .map(|(root, path)| {
                            format!("{}/{bare_name} ({})", root.origin, path.display())
                        })
                        .collect(),
                });
            }
            // Purely legacy collision: first hit wins, and only the first hit
            // is parsed.
        }

        let document = Self::load(first_path, Some(bare_name))?;
        Ok((
            document,
            QualifiedFleetId {
                name: bare_name.to_string(),
                origin: first_root.origin.clone(),
            },
        ))
    }

    #[must_use]
    pub fn name(&self) -> &str {
        match &self.schema {
            FleetSchema::Legacy(fleet) => &fleet.name,
            FleetSchema::Exact(fleet) => &fleet.name,
        }
    }

    #[must_use]
    pub fn description(&self) -> Option<&str> {
        match &self.schema {
            FleetSchema::Legacy(fleet) => fleet.description.as_deref(),
            FleetSchema::Exact(fleet) => fleet.description.as_deref(),
        }
    }

    #[must_use]
    pub fn schema(&self) -> &FleetSchema {
        &self.schema
    }

    /// Explicit legacy detection — never inferred from a missing table.
    #[must_use]
    pub const fn is_legacy(&self) -> bool {
        matches!(self.schema, FleetSchema::Legacy(_))
    }

    #[must_use]
    pub fn legacy(&self) -> Option<&NamedFleet> {
        match &self.schema {
            FleetSchema::Legacy(fleet) => Some(fleet),
            FleetSchema::Exact(_) => None,
        }
    }

    #[must_use]
    pub fn exact(&self) -> Option<&ExactFleet> {
        match &self.schema {
            FleetSchema::Exact(fleet) => Some(fleet),
            FleetSchema::Legacy(_) => None,
        }
    }

    #[must_use]
    pub fn schema_kind(&self) -> &'static str {
        match self.schema {
            FleetSchema::Legacy(_) => LEGACY_FLEET_SCHEMA_KIND,
            FleetSchema::Exact(_) => EXACT_FLEET_SCHEMA_KIND,
        }
    }

    #[must_use]
    pub fn schema_revision(&self) -> u32 {
        match &self.schema {
            // Legacy files carry no revision; report 0 so a snapshot can tell
            // "pre-versioned" from exact revision 1.
            FleetSchema::Legacy(_) => 0,
            FleetSchema::Exact(fleet) => fleet.schema_revision,
        }
    }

    /// SHA-256 of the exact file bytes this document was parsed from.
    #[must_use]
    pub fn source_hash(&self) -> &str {
        &self.source_hash
    }

    #[must_use]
    pub fn source_path(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// An exact document frozen from a saved v2 Fleet at Workflow start.
    ///
    /// `frozen_text` is the exact-schema rendering of the saved Fleet with every
    /// route and reasoning request resolved; it goes through the same exact
    /// parser as a hand-written file, and [`Self::source_hash`] covers those
    /// frozen bytes — what actually runs — while [`Self::source_path`] names the
    /// saved Fleet file it came from.
    pub fn from_frozen_saved_fleet(
        frozen_text: &str,
        source: &Path,
    ) -> Result<Self, NamedFleetError> {
        if declared_schema_kind(frozen_text).as_deref() != Some(EXACT_FLEET_SCHEMA_KIND) {
            return Err(NamedFleetError::Parse {
                path: source.display().to_string(),
                message: "a frozen saved Fleet must be in the exact schema".to_string(),
            });
        }
        let mut document = Self::parse(frozen_text).map_err(|error| match error {
            NamedFleetError::Exact { source: inner, .. } => NamedFleetError::Exact {
                fleet: source.display().to_string(),
                source: inner,
            },
            other => other,
        })?;
        document.source = Some(source.to_path_buf());
        Ok(document)
    }

    /// Build a document around an already-constructed exact roster.
    ///
    /// Test-only, and deliberately so: it is how a roster that never passed
    /// through the TOML parser reaches [`crate::FleetSnapshot::capture`], which
    /// is exactly the bypass capture-time revalidation exists to close.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_exact_for_tests(exact: ExactFleet) -> Self {
        Self {
            schema: FleetSchema::Exact(exact),
            source: None,
            source_hash: content_hash("<constructed>"),
        }
    }
}

/// The schema revision an exact document is expected to declare.
#[must_use]
pub const fn exact_schema_revision() -> u32 {
    EXACT_FLEET_SCHEMA_REVISION
}

pub(crate) fn content_hash(text: &str) -> String {
    sha256_label(text.as_bytes())
}

/// `sha256:<hex>` over arbitrary bytes, written out by hand rather than
/// relying on a digest `LowerHex` impl.
pub(crate) fn sha256_label(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(7 + digest.len() * 2);
    out.push_str("sha256:");
    for byte in digest.iter() {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// Required roles for the stopship dogfood fleet (#4178).
pub const STOPSHIP_REQUIRED_ROLES: &[&str] =
    &["explore", "implement", "reviewer", "test", "release_lead"];

/// Parse a fleet TOML document.
pub fn parse_named_fleet(toml_text: &str) -> Result<NamedFleet, NamedFleetError> {
    // Minimal TOML subset without adding a toml dep to workflow:
    // accept JSON as well for tests; for TOML use a tiny hand parser for
    // the documented shape, or serde via json for unit tests.
    // Prefer JSON if the text looks like JSON; otherwise use line-oriented TOML.
    let trimmed = toml_text.trim();
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed).map_err(|e| NamedFleetError::Parse {
            path: "<memory>".into(),
            message: e.to_string(),
        });
    }
    parse_fleet_toml_minimal(trimmed)
}

/// Strip comments from the single-line basic and literal strings supported by
/// the minimal fleet parser.
fn strip_toml_comment(line: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;

    for (index, character) in line.char_indices() {
        match quote {
            Some('"') => {
                if escaped {
                    escaped = false;
                } else {
                    match character {
                        '\\' => escaped = true,
                        '"' => quote = None,
                        _ => {}
                    }
                }
            }
            Some('\'') => {
                if character == '\'' {
                    quote = None;
                }
            }
            Some(_) => unreachable!("only TOML string delimiters are tracked"),
            None => match character {
                '"' | '\'' => quote = Some(character),
                '#' => return &line[..index],
                _ => {}
            },
        }
    }

    line
}

fn toml_string_value(value: &str) -> String {
    if let Some(literal) = value
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        return literal.to_string();
    }
    if value.starts_with('"')
        && let Ok(basic) = serde_json::from_str::<String>(value)
    {
        return basic;
    }
    value.trim_matches('"').to_string()
}

fn parse_fleet_toml_minimal(text: &str) -> Result<NamedFleet, NamedFleetError> {
    let mut name = None;
    let mut description = None;
    let mut roles = BTreeMap::new();
    let mut section = "";
    for raw in text.lines() {
        let line = strip_toml_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = &line[1..line.len() - 1];
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = toml_string_value(value.trim());
        match section {
            "" => match key {
                "name" => name = Some(value),
                "description" => description = Some(value),
                _ => {}
            },
            "roles" => {
                roles.insert(key.to_string(), value);
            }
            _ => {}
        }
    }
    let name = name.ok_or_else(|| NamedFleetError::Parse {
        path: "<memory>".into(),
        message: "missing name".into(),
    })?;
    Ok(NamedFleet {
        name,
        description,
        roles,
    })
}

/// Load fleet by name from search paths (first hit wins).
pub fn load_named_fleet(
    name: &str,
    search_roots: &[PathBuf],
) -> Result<NamedFleet, NamedFleetError> {
    validate_fleet_file_stem(name)?;
    let file_name = format!("{name}.toml");
    for root in search_roots {
        let path = root.join("fleets").join(&file_name);
        if path.is_file() {
            return load_named_fleet_file(&path, Some(name));
        }
    }
    Err(NamedFleetError::NotFound(name.to_string()))
}

pub fn load_named_fleet_file(
    path: &Path,
    expect_name: Option<&str>,
) -> Result<NamedFleet, NamedFleetError> {
    let text = std::fs::read_to_string(path).map_err(|e| NamedFleetError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    let fleet = parse_named_fleet(&text).map_err(|e| match e {
        NamedFleetError::Parse { message, .. } => NamedFleetError::Parse {
            path: path.display().to_string(),
            message,
        },
        other => other,
    })?;
    if let Some(expected) = expect_name
        && fleet.name != expected
    {
        return Err(NamedFleetError::NameMismatch {
            declared: fleet.name,
            expected: expected.to_string(),
        });
    }
    Ok(fleet)
}

impl NamedFleet {
    /// Resolve a role name to a profile id.
    pub fn resolve(&self, role: &str) -> Result<&str, NamedFleetError> {
        let key = role.trim().to_ascii_lowercase();
        self.roles
            .get(&key)
            .or_else(|| {
                self.roles
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(role))
                    .map(|(_, v)| v)
            })
            .map(String::as_str)
            .ok_or_else(|| NamedFleetError::MissingRole {
                fleet: self.name.clone(),
                role: role.to_string(),
            })
    }

    /// Ensure all required stopship roles are present.
    pub fn validate_stopship_roles(&self) -> Result<(), NamedFleetError> {
        for role in STOPSHIP_REQUIRED_ROLES {
            self.resolve(role)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STOPSHIP_TOML: &str = r#"
name = "stopship"
description = "Stopship dogfood fleet"

[roles]
explore = "scout"
implement = "builder"
reviewer = "reviewer"
test = "verifier"
release_lead = "manager"
"#;

    #[test]
    fn stopship_fleet_resolves_all_five_roles() {
        let fleet = parse_named_fleet(STOPSHIP_TOML).expect("parse");
        assert_eq!(fleet.name, "stopship");
        fleet.validate_stopship_roles().expect("all roles");
        assert_eq!(fleet.resolve("explore").unwrap(), "scout");
        assert_eq!(fleet.resolve("implement").unwrap(), "builder");
        assert_eq!(fleet.resolve("reviewer").unwrap(), "reviewer");
        assert_eq!(fleet.resolve("test").unwrap(), "verifier");
        assert_eq!(fleet.resolve("release_lead").unwrap(), "manager");
    }

    #[test]
    fn literal_and_escaped_toml_strings_lose_their_quotes() {
        let fleet = parse_named_fleet(
            "name = 'stopship' # literal\ndescription = \"a \\\"quoted\\\" fleet\"\n[roles]\nexplore = 'scout'\n",
        )
        .expect("parse");
        assert_eq!(fleet.name, "stopship");
        assert_eq!(fleet.description.as_deref(), Some("a \"quoted\" fleet"));
        assert_eq!(fleet.resolve("explore").unwrap(), "scout");
    }

    #[test]
    fn fleet_names_cannot_leave_the_fleets_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(ws.join("fleets")).expect("fleets dir");
        let planted = STOPSHIP_TOML.replace("\"stopship\"", "\"outside\"");
        // Reachable by `workspace/../../outside` and by an absolute name.
        std::fs::write(tmp.path().join("outside.toml"), &planted).expect("outside");
        // Reachable by a bare `../outside` from `ws/fleets`.
        std::fs::write(ws.join("outside.toml"), &planted).expect("sibling");
        let roots = vec![FleetSearchRoot::new("workspace", &ws)];
        let absolute = tmp.path().join("outside");
        let absolute = absolute.to_string_lossy();

        for name in [
            "workspace/../../outside",
            "workspace/../outside",
            absolute.as_ref(),
            "workspace/sub/outside",
            "..",
            "",
        ] {
            let err = FleetDocument::load_by_name(name, &roots).expect_err(name);
            assert!(
                matches!(err, NamedFleetError::InvalidName),
                "{name}: expected InvalidName, got {err:?}"
            );
            assert!(
                !err.to_string()
                    .contains(tmp.path().to_string_lossy().as_ref())
            );
        }
        let err = load_named_fleet("../outside", std::slice::from_ref(&ws)).expect_err("legacy");
        assert!(matches!(err, NamedFleetError::InvalidName), "{err:?}");

        // A plain name still loads.
        std::fs::write(ws.join("fleets").join("stopship.toml"), STOPSHIP_TOML).expect("fleet");
        FleetDocument::load_by_name("stopship", &roots).expect("plain name");
        FleetDocument::load_by_name("workspace/stopship", &roots).expect("qualified name");
        load_named_fleet("stopship", std::slice::from_ref(&ws)).expect("legacy plain name");
    }

    #[test]
    fn unknown_role_fails_clearly() {
        let fleet = parse_named_fleet(STOPSHIP_TOML).unwrap();
        let err = fleet.resolve("wizard").unwrap_err();
        assert!(matches!(err, NamedFleetError::MissingRole { .. }));
    }

    #[test]
    fn quoted_hashes_are_not_treated_as_comments() {
        let fleet = parse_named_fleet(
            r#"
name = "issue-references"
description = "Tracks #4178 dogfood" # real comment

[roles]
scout = "scout#stable"
"#,
        )
        .expect("parse");

        assert_eq!(fleet.description.as_deref(), Some("Tracks #4178 dogfood"));
        assert_eq!(fleet.resolve("scout").unwrap(), "scout#stable");
    }

    #[test]
    fn comment_stripping_tracks_toml_quotes_and_escapes() {
        assert_eq!(
            strip_toml_comment(r##"description = "say \"#still-value\"" # comment"##).trim_end(),
            r##"description = "say \"#still-value\"""##
        );
        assert_eq!(
            strip_toml_comment("description = 'tracks #4178' # comment").trim_end(),
            "description = 'tracks #4178'"
        );
        assert_eq!(
            strip_toml_comment(r#"name = "stopship" # comment"#).trim_end(),
            r#"name = "stopship""#
        );
    }

    #[test]
    fn legacy_fleet_files_still_deserialize_and_resolve_through_the_document_api() {
        let document = FleetDocument::parse(STOPSHIP_TOML).expect("legacy parse");

        // Legacy is explicitly detectable, not inferred.
        assert!(document.is_legacy());
        assert_eq!(document.schema_kind(), "legacy");
        assert_eq!(document.schema_revision(), 0);
        assert!(document.exact().is_none());

        let legacy = document.legacy().expect("legacy body");
        legacy.validate_stopship_roles().expect("all roles");
        assert_eq!(legacy.resolve("implement").unwrap(), "builder");
        assert_eq!(document.name(), "stopship");
        assert!(document.source_hash().starts_with("sha256:"));
    }

    #[test]
    fn exact_fleet_files_are_selected_by_an_explicit_schema_key() {
        let document = FleetDocument::parse(
            r#"
name = "glm-pair"
schema = "exact"

[[members]]
id = "implementer"
provider = "zai"
model = "glm-5"
reasoning = "auto"

[[members]]
id = "router"
kind = "router"
provider = "zai"
model = "glm-5-turbo"
"#,
        )
        .expect("exact parse");

        assert!(!document.is_legacy());
        assert_eq!(document.schema_kind(), "exact");
        assert_eq!(document.schema_revision(), exact_schema_revision());
        assert!(document.legacy().is_none());
        let exact = document.exact().expect("exact body");
        assert!(exact.has_auto_member());
        // The prototype inline form still parses, and is reported as the legacy
        // inline router rather than as a second runtime concept.
        assert!(exact.legacy_inline_router().is_some());
        assert!(exact.router_ref().is_some());
    }

    #[test]
    fn an_unknown_schema_key_fails_instead_of_falling_back_to_legacy() {
        let err = FleetDocument::parse("name = \"f\"\nschema = \"experimental\"\n")
            .expect_err("unknown schema must not silently parse as legacy");
        assert!(matches!(err, NamedFleetError::Parse { .. }), "{err:?}");
    }

    #[test]
    fn document_hash_follows_the_file_bytes() {
        let a = FleetDocument::parse(STOPSHIP_TOML).expect("parse");
        let b = FleetDocument::parse(STOPSHIP_TOML).expect("parse");
        let c = FleetDocument::parse(&STOPSHIP_TOML.replace("builder", "implementer_profile"))
            .expect("parse");

        assert_eq!(a.source_hash(), b.source_hash());
        assert_ne!(a.source_hash(), c.source_hash());
    }

    #[test]
    fn loads_workspace_fleet_file() {
        // Relative to crate CARGO_MANIFEST_DIR → repo root fleets/
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let fleet = load_named_fleet("stopship", &[root]).expect("load workspace fleet");
        fleet.validate_stopship_roles().unwrap();
    }

    #[test]
    fn a_frozen_saved_fleet_is_exact_and_names_its_source_file() {
        let source = Path::new("/saved/.codewhale/fleets/release.toml");
        let frozen = "schema = \"exact\"\nschema_revision = 1\nname = \"release\"\n\n\
                      [[members]]\nid = \"builder\"\nrole = \"implement\"\n\
                      provider = \"zai\"\nmodel = \"glm-5\"\nreasoning = \"high\"\n";
        let document = FleetDocument::from_frozen_saved_fleet(frozen, source).expect("frozen");
        assert!(document.exact().is_some());
        assert_eq!(document.source_path(), Some(source));
        assert_eq!(document.source_hash(), content_hash(frozen));

        // Anything that is not the exact schema is refused, never parsed as a
        // legacy role map.
        let error = FleetDocument::from_frozen_saved_fleet(
            "name = \"release\"\n[roles]\nimplement = \"builder\"\n",
            source,
        )
        .expect_err("legacy text is not a frozen snapshot");
        assert!(error.to_string().contains("exact schema"), "{error}");
    }

    #[test]
    fn a_saved_fleet_schema_is_named_in_the_unknown_schema_error() {
        let error = FleetDocument::parse("schema = \"fleet\"\nname = \"x\"\n").unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("expected `exact` or a saved Fleet"),
            "{message}"
        );
        assert!(message.contains(".codewhale/fleets/"), "{message}");
    }
}
