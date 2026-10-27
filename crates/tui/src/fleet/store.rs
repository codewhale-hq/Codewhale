//! The saved named Fleet — the single configuration concept for the whole
//! Fleet surface. Its v2 compatibility storage keeps `schema = "fleet"`.
//!
//! A Fleet is one self-contained TOML file. It owns:
//!
//! - its **operator** route (provider + exact model + reasoning), or the
//!   explicit absence of one ("inherit the session route");
//! - its **roster**: each member's stable id, optional human-facing name, role,
//!   exact model pin or inherit policy, provider (pins only — never inferred
//!   from a model string), reasoning level, optional instructions, and
//!   capability requirements;
//! - its **save scope and source**: personal (`$CODEWHALE_HOME/fleets/`) or
//!   workspace (`.codewhale/fleets/`), with the exact file path surfaced.
//!
//! There is exactly one store. The legacy per-role profile files
//! (`~/.codewhale/agents/*.toml`, `.codewhale/agents/*.toml`,
//! `[fleet.profiles]`) and the workflow crate's `exact`/legacy named-fleet
//! files are migration/compat input only — read here, never shadowed, never
//! the runtime winner alongside a v2 Fleet.
//!
//! Selection is a scope-explicit file: `fleets/selected` under the personal
//! root is the user-global default; the same file under the workspace root is
//! an intentional workspace selection. Workspace selection wins; both are
//! labeled in the UI. A workspace selection can never hide or rewrite a
//! personal Fleet.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use codewhale_config::ProviderKind;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::roster::FleetRoster;

pub const FLEET_SCHEMA_KIND: &str = "fleet";
pub const FLEET_SCHEMA_REVISION: u32 = 2;
const MAX_MEMBER_DISPLAY_NAME_CHARS: usize = 80;
const MAX_FLEET_NAME_CHARS: usize = 120;
const MAX_MEMBER_ID_CHARS: usize = 64;
const MAX_MEMBER_ROLE_CHARS: usize = 80;
const MAX_ROUTE_FIELD_CHARS: usize = 256;
const MAX_MEMBER_INSTRUCTIONS_CHARS: usize = 32 * 1024;
const MAX_FLEET_DESCRIPTION_CHARS: usize = 32 * 1024;

/// A value that reaches selectors, receipts, and terminal rendering: printable
/// and bounded. `multiline` admits newlines and tabs (instruction overlays)
/// but no other control characters.
fn validate_text(
    what: &str,
    value: &str,
    max_chars: usize,
    multiline: bool,
) -> Result<(), FleetStoreError> {
    let printable = value
        .chars()
        .all(|ch| !ch.is_control() || (multiline && matches!(ch, '\n' | '\r' | '\t')));
    if !printable || value.chars().count() > max_chars {
        return Err(FleetStoreError::Invalid(format!(
            "{what} must be printable {} no longer than {max_chars} characters",
            if multiline {
                "text"
            } else {
                "single-line text"
            }
        )));
    }
    Ok(())
}

/// The directory name used by both roots (next to `agents/` for legacy
/// profiles). Also used by the workflow crate for its own legacy/exact files;
/// v2 files in the same directory are simply a newer schema.
pub const FLEET_DIR: &str = "fleets";
pub const SELECTED_FILE: &str = "selected";

/// Where a Fleet was saved. This is the pin target: personal = user-global,
/// workspace = folder-scoped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetScope {
    Personal,
    Workspace,
}

impl FleetScope {
    /// Short label for UI and receipts: "user" / "folder".
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Personal => "user",
            Self::Workspace => "folder",
        }
    }

    #[must_use]
    pub const fn long_label(self) -> &'static str {
        match self {
            Self::Personal => "user-global",
            Self::Workspace => "folder (this workspace)",
        }
    }

    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Personal => Self::Workspace,
            Self::Workspace => Self::Personal,
        }
    }
}

/// A Fleet's own operator route. Absent = inherit the live session route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FleetOperator {
    /// Exact provider id (a `[providers.<id>]` key or a built-in id).
    pub provider: String,
    /// Exact model id on that provider's route.
    pub model: String,
    /// Reasoning level, only when the resolved route genuinely supports it.
    /// Absent = inherit the session tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

/// Capability requirements a member must satisfy. The vocabulary is closed so
/// an unknown requirement is a specific error, never a silent reinterpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberCapability {
    /// Image input: the member must run on a route that accepts images.
    Vision,
}

impl MemberCapability {
    pub const VOCABULARY: [&'static str; 1] = ["vision"];

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "vision" | "image" | "image-input" => Some(Self::Vision),
            _ => None,
        }
    }

    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Vision => "vision",
        }
    }
}

/// One roster member of a Fleet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FleetMember {
    /// Stable member id — the role identity (e.g. `scout`, `builder`).
    pub id: String,
    /// Optional human-facing name used by roster views and member selectors.
    ///
    /// `name` is accepted as an authoring alias, while canonical saves use
    /// `display_name`. Existing revision-2 files omit this field and continue
    /// to deserialize unchanged.
    #[serde(default, alias = "name", skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// A role-less model choice, not an executable roster member. Omitted
    /// in legacy files, whose role/id interpretation stays unchanged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shortlist: bool,
    /// Role label; defaults to `id` when absent on a non-shortlist member.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    /// Exact model pin. Absent with `provider` absent = inherit the session
    /// route (the operator route when the Fleet has one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Exact provider id for `model`. Pins only: a member must never carry
    /// `provider` without `model` (rejected at parse), and the provider is
    /// never inferred from the model string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Reasoning level for this member, only when the resolved route
    /// supports it. Absent = inherit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Optional instruction overlay for the role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Capability requirements, e.g. `["vision"]`. Validated against
    /// [`MemberCapability::VOCABULARY`] at parse.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
}

impl FleetMember {
    /// The role this member fills: `role`, or `id` when the document left
    /// the role field off. A shortlisted model has no role.
    #[must_use]
    pub fn role_label(&self) -> &str {
        if self.shortlist {
            return "";
        }
        let role = self.role.trim();
        if role.is_empty() {
            self.id.trim()
        } else {
            role
        }
    }

    /// A row that was a model pin promoted to a member: no role, and an id
    /// that is just the model's slug. Such rows are not members.
    #[must_use]
    pub fn is_bare_model_pin(&self) -> bool {
        !self.shortlist
            && self.role.trim().is_empty()
            && self
                .model
                .as_deref()
                .is_some_and(|model| self.id.trim().starts_with(slugify(model).as_str()))
    }
}

/// Provider kinds have documented aliases; named custom routes have exact
/// keys. Treating every provider name as case-insensitive merges distinct
/// endpoints before the configured route binder can resolve them.
pub(crate) fn provider_ids_match(saved: &str, requested: &str) -> bool {
    saved.trim() == requested.trim()
        || ProviderKind::parse(saved)
            .filter(|provider| *provider != ProviderKind::Custom)
            .is_some_and(|provider| Some(provider) == ProviderKind::parse(requested))
}

/// The member pins exactly `provider`/`model`.
pub(crate) fn member_pins(member: &FleetMember, provider: &str, model: &str) -> bool {
    member
        .provider
        .as_deref()
        .is_some_and(|p| provider_ids_match(p, provider))
        && member.model.as_deref().is_some_and(|id| id == model)
}

/// The saved named Fleet document (compatibility `schema = "fleet"`, revision 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FleetFile {
    pub schema: String,
    pub schema_revision: u32,
    /// Editable display name. Unique per scope (the file slug is derived
    /// from it); the same name may exist in both scopes, distinguished by
    /// origin, never silently shadowed.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The Fleet's own operator route. Absent = inherit the session route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<FleetOperator>,
    #[serde(default)]
    pub members: Vec<FleetMember>,
}

/// Why a Fleet file could not be used.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FleetStoreError {
    #[error("invalid fleet: {0}")]
    Invalid(String),
    #[error(
        "fleet `{0}` is defined in both {1} and {2}; name one explicitly as {1}/{0} or {2}/{0}"
    )]
    Ambiguous(String, String, String),
    #[error("fleet file not found: {0}")]
    NotFound(String),
    #[error("failed to read {path}: {message}")]
    Io { path: String, message: String },
    #[error("failed to parse {path}: {message}")]
    Parse { path: String, message: String },
    #[error("a fleet named `{name}` already exists at {path}; rename it or choose another name")]
    NameTaken { name: String, path: String },
}

impl FleetFile {
    /// Create a validated v2 Fleet file.
    pub fn new(name: String, description: Option<String>) -> Result<Self, FleetStoreError> {
        let fleet = Self {
            schema: FLEET_SCHEMA_KIND.to_string(),
            schema_revision: FLEET_SCHEMA_REVISION,
            name,
            description,
            operator: None,
            members: Vec::new(),
        };
        fleet.validate()?;
        Ok(fleet)
    }

    /// Validate the document: name, member ids, pin symmetry, capability
    /// vocabulary. Invalid input is rejected with a specific error — never
    /// silently reinterpreted.
    pub fn validate(&self) -> Result<(), FleetStoreError> {
        if self.schema != FLEET_SCHEMA_KIND {
            return Err(FleetStoreError::Invalid(format!(
                "unknown schema `{}`; expected `{FLEET_SCHEMA_KIND}`",
                self.schema
            )));
        }
        if self.schema_revision != FLEET_SCHEMA_REVISION {
            return Err(FleetStoreError::Invalid(format!(
                "unsupported schema revision {}; this build reads revision {FLEET_SCHEMA_REVISION}",
                self.schema_revision
            )));
        }
        let name = self.name.trim();
        if name.is_empty() {
            return Err(FleetStoreError::Invalid(
                "fleet name must not be empty".to_string(),
            ));
        }
        // The stored name, not its trimmed view: a control character before
        // or after the name reaches selectors and terminal rendering too.
        validate_text("fleet name", &self.name, MAX_FLEET_NAME_CHARS, false)?;
        if let Some(description) = self.description.as_deref() {
            validate_text(
                "fleet description",
                description,
                MAX_FLEET_DESCRIPTION_CHARS,
                true,
            )?;
        }
        // The operator route flows into every inheriting member's route, so
        // it is bounded exactly like a member's pin.
        if let Some(operator) = &self.operator {
            for (field, value) in [
                ("provider", Some(operator.provider.as_str())),
                ("model", Some(operator.model.as_str())),
                ("reasoning", operator.reasoning.as_deref()),
            ] {
                if let Some(value) = value {
                    validate_text(
                        &format!("operator {field}"),
                        value,
                        MAX_ROUTE_FIELD_CHARS,
                        false,
                    )?;
                }
            }
        }
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for member in &self.members {
            let member_id = member.id.trim();
            if member_id.is_empty() {
                return Err(FleetStoreError::Invalid(
                    "member id must not be empty".to_string(),
                ));
            }
            // The id is dispatch identity: exact, not merely equal once trimmed.
            if member_id != member.id {
                return Err(FleetStoreError::Invalid(format!(
                    "member id {:?} must not carry leading or trailing whitespace",
                    member.id
                )));
            }
            validate_text("member id", member_id, MAX_MEMBER_ID_CHARS, false)?;
            let fields = [
                (
                    "role",
                    Some(member.role.as_str()),
                    MAX_MEMBER_ROLE_CHARS,
                    false,
                ),
                (
                    "provider",
                    member.provider.as_deref(),
                    MAX_ROUTE_FIELD_CHARS,
                    false,
                ),
                (
                    "model",
                    member.model.as_deref(),
                    MAX_ROUTE_FIELD_CHARS,
                    false,
                ),
                (
                    "reasoning",
                    member.reasoning.as_deref(),
                    MAX_ROUTE_FIELD_CHARS,
                    false,
                ),
                (
                    "instructions",
                    member.instructions.as_deref(),
                    MAX_MEMBER_INSTRUCTIONS_CHARS,
                    true,
                ),
            ];
            for (field, value, max_chars, multiline) in fields {
                if let Some(value) = value {
                    validate_text(
                        &format!("member `{member_id}` {field}"),
                        value,
                        max_chars,
                        multiline,
                    )?;
                }
            }
            let member_key = member_id.to_ascii_lowercase();
            if let Some(existing) = seen.insert(member_key, member.id.clone()) {
                return Err(FleetStoreError::Invalid(format!(
                    "duplicate member id `{}` conflicts case-insensitively with `{existing}`",
                    member.id,
                )));
            }
            if let Some(display_name) = member.display_name.as_deref() {
                let trimmed = display_name.trim();
                if trimmed.is_empty() {
                    return Err(FleetStoreError::Invalid(format!(
                        "member `{}` display_name must not be empty",
                        member.id,
                    )));
                }
                if trimmed != display_name
                    || display_name.chars().any(char::is_control)
                    || display_name.chars().count() > MAX_MEMBER_DISPLAY_NAME_CHARS
                {
                    return Err(FleetStoreError::Invalid(format!(
                        "member `{}` display_name must be one trimmed printable line no longer than {MAX_MEMBER_DISPLAY_NAME_CHARS} characters",
                        member.id,
                    )));
                }
            }
            match (&member.provider, &member.model) {
                (Some(_), None) | (None, Some(_)) => {
                    return Err(FleetStoreError::Invalid(format!(
                        "member `{}` must pin both provider and model, or neither (inherit); a lone {} is rejected",
                        member.id,
                        if member.provider.is_some() {
                            "provider"
                        } else {
                            "model"
                        }
                    )));
                }
                _ => {}
            }
            if member.shortlist
                && (!member.role.trim().is_empty()
                    || member
                        .provider
                        .as_deref()
                        .is_none_or(|id| id.trim().is_empty())
                    || member
                        .model
                        .as_deref()
                        .is_none_or(|id| id.trim().is_empty()))
            {
                return Err(FleetStoreError::Invalid(format!(
                    "shortlisted member `{}` must have no role and pin both provider and model",
                    member.id,
                )));
            }
            if member.shortlist
                && (member
                    .reasoning
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                    || member
                        .instructions
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
                    || !member.requires.is_empty())
            {
                return Err(FleetStoreError::Invalid(format!(
                    "shortlisted member `{}` cannot set role reasoning, instructions, or capability requirements",
                    member.id,
                )));
            }
            for requirement in &member.requires {
                if MemberCapability::parse(requirement).is_none() {
                    return Err(FleetStoreError::Invalid(format!(
                        "member `{}` requires unknown capability `{requirement}`; valid values: {}",
                        member.id,
                        MemberCapability::VOCABULARY.join(", ")
                    )));
                }
            }
        }
        Ok(())
    }

    /// Render the canonical TOML document.
    pub fn render_toml(&self) -> Result<String, FleetStoreError> {
        self.validate()?;
        let rendered = toml::to_string_pretty(self)
            .map_err(|e| FleetStoreError::Invalid(format!("failed to serialize fleet: {e}")))?;
        Ok(rendered)
    }

    /// Parse a v2 fleet document from TOML text.
    pub fn parse(text: &str) -> Result<Self, FleetStoreError> {
        let mut fleet: Self = toml::from_str(text)
            .map_err(|e| FleetStoreError::Invalid(format!("invalid fleet TOML: {e}")))?;
        // Compat (0.9.12): every model the user ever selected was enrolled
        // as a role-less member with a slug id. Roles are the members; drop
        // those rows on read so the roster reads as roles again. The next
        // save writes the clean document.
        fleet.members.retain(|member| !member.is_bare_model_pin());
        // #6037: a member pinned to the fleet's own operator route resolves
        // to that route either way; the pin only stops it following when the
        // operator moves (a vendor retiring the id, an operator switching
        // models). Read the redundant pin as the inheritance it always meant.
        // Shortlist rows keep their pin — it is their entire content.
        if let Some(operator) = &fleet.operator {
            for member in &mut fleet.members {
                if !member.shortlist && member_pins(member, &operator.provider, &operator.model) {
                    member.provider = None;
                    member.model = None;
                }
            }
        }
        fleet.validate()?;
        Ok(fleet)
    }

    /// A stable file slug derived from the display name. Safe across the
    /// filesystems Codewhale supports; collisions are detected at save.
    #[must_use]
    pub fn file_slug(&self) -> String {
        slugify(&self.name)
    }

    /// Look up an executable member by role id. Shortlisted model ids never
    /// select a role, even when they happen to match one.
    #[must_use]
    pub fn member(&self, id: &str) -> Option<&FleetMember> {
        let id = id.trim();
        self.members
            .iter()
            .find(|member| !member.shortlist && member.id.trim().eq_ignore_ascii_case(id))
    }

    /// Whether the roster contains a scout member (the fast exploratory role).
    #[must_use]
    pub fn has_scout(&self) -> bool {
        self.member("scout").is_some()
    }
}

/// Sanitize a display name into a safe file slug.
pub(crate) fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if (ch.is_whitespace() || ch == '-' || ch == '_') && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "fleet".to_string()
    } else {
        slug
    }
}

/// One entry in the Fleet list: name, scope, exact path, and health.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetEntry {
    pub name: String,
    pub scope: FleetScope,
    /// Exact path of the saved file.
    pub path: PathBuf,
    /// Parse failure, when the file exists but cannot be read as a v2 Fleet.
    pub parse_error: Option<String>,
    /// Whether the file is a legacy (pre-v2) named-fleet file (exact or
    /// roles map) that is read for compatibility but not editable as v2.
    pub legacy: bool,
}

/// The resolved selection: which Fleet a session should start on, and which
/// scope made the choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedFleet {
    pub name: String,
    pub scope: FleetScope,
    pub path: PathBuf,
}

fn personal_fleets_dir() -> Result<PathBuf, FleetStoreError> {
    #[cfg(test)]
    if !crate::test_support::guarded_environment_provides_state_paths() {
        return Ok(crate::test_support::unsealed_test_state_root().join(FLEET_DIR));
    }
    codewhale_config::codewhale_home()
        .map(|home| home.join(FLEET_DIR))
        .map_err(|e| FleetStoreError::Io {
            path: "$CODEWHALE_HOME/fleets".to_string(),
            message: e.to_string(),
        })
}

fn workspace_fleets_dir(workspace: &Path) -> PathBuf {
    workspace.join(".codewhale").join(FLEET_DIR)
}

/// The fleet directory for a scope, creating it if needed.
fn ensure_fleets_dir(scope: FleetScope, workspace: &Path) -> Result<PathBuf, FleetStoreError> {
    let dir = match scope {
        FleetScope::Personal => personal_fleets_dir()?,
        FleetScope::Workspace => workspace_fleets_dir(workspace),
    };
    reject_linked_workspace_dir(scope, workspace, &dir)?;
    fs::create_dir_all(&dir).map_err(|e| FleetStoreError::Io {
        path: dir.display().to_string(),
        message: e.to_string(),
    })?;
    Ok(dir)
}

/// A workspace's `.codewhale/fleets` is repository content: refuse it when it,
/// or `.codewhale`, is a link, so saving, selecting or deleting a Fleet cannot
/// reach outside the workspace. The personal directory is the user's own.
fn reject_linked_workspace_dir(
    scope: FleetScope,
    workspace: &Path,
    dir: &Path,
) -> Result<(), FleetStoreError> {
    if scope != FleetScope::Workspace {
        return Ok(());
    }
    codewhale_util::files::reject_linked_path(workspace, dir).map_err(|e| FleetStoreError::Io {
        path: dir.display().to_string(),
        message: e.to_string(),
    })
}

/// List every named Fleet across both scopes, personal first. A file that is
/// not a v2 Fleet is listed as `legacy` with its parse error, so an old exact
/// fleet is visible — never silently absent — while the user decides whether
/// to migrate it.
pub fn list_fleets(workspace: &Path) -> Vec<FleetEntry> {
    let mut entries = Vec::new();
    if let Ok(dir) = personal_fleets_dir() {
        collect_entries(&dir, FleetScope::Personal, &mut entries);
    }
    collect_entries(
        &workspace_fleets_dir(workspace),
        FleetScope::Workspace,
        &mut entries,
    );
    entries.sort_by(|a, b| {
        a.scope
            .label()
            .cmp(b.scope.label())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries
}

fn collect_entries(dir: &Path, scope: FleetScope, out: &mut Vec<FleetEntry>) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = read
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    files.sort();
    for path in files {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = fs::read_to_string(&path).ok();
        let parse_error = text
            .as_deref()
            .and_then(|text| FleetFile::parse(text).err())
            .map(|e| e.to_string());
        let legacy = parse_error.as_deref().is_some_and(|err| {
            err.contains("unknown schema") || err.contains("invalid fleet TOML")
        });
        // The row shows the Fleet's own display name, never the file slug —
        // a file saved as `Temp Fleet` must not appear as `temp-fleet`.
        let name = text
            .as_deref()
            .and_then(|text| toml::from_str::<toml::Value>(text).ok())
            .and_then(|value| {
                value
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or(stem);
        out.push(FleetEntry {
            name,
            scope,
            path,
            parse_error,
            legacy,
        });
    }
}

/// Every v2 Fleet file that answers to `name`, personal first.
///
/// Only files that declare `schema = "fleet"` count. The personal `fleets/`
/// directory is shared with the workflow crate's legacy/exact files, and a
/// file in another schema is a different Fleet form, not a v2 Fleet that
/// failed to parse — the caller that owns that form reports on it.
pub(crate) fn v2_fleet_candidates(name: &str, workspace: &Path) -> Vec<(FleetScope, PathBuf)> {
    let file_name = format!("{}.toml", slugify(name.trim()));
    let mut found = Vec::new();
    let personal = personal_fleets_dir().ok().map(|dir| dir.join(&file_name));
    let workspace = Some(workspace_fleets_dir(workspace).join(&file_name));
    for (scope, path) in [
        (FleetScope::Personal, personal),
        (FleetScope::Workspace, workspace),
    ] {
        if let Some(path) = path
            && path.is_file()
            && declares_v2_schema(&path)
        {
            found.push((scope, path));
        }
    }
    found
}

/// The `schema` a Fleet file declares, normalized to lowercase. `None` when
/// the file cannot be read; `Some(None)` when it is readable but declares no
/// schema (or is not valid TOML).
pub(crate) fn read_declared_schema(path: &Path) -> Option<Option<String>> {
    fs::read_to_string(path)
        .ok()
        .map(|text| codewhale_workflow::fleet_exact::declared_schema_kind(&text))
}

/// Whether a file declares the v2 `schema = "fleet"`. Unreadable or
/// malformed TOML is not a v2 declaration.
pub(crate) fn declares_v2_schema(path: &Path) -> bool {
    read_declared_schema(path).flatten().as_deref() == Some(FLEET_SCHEMA_KIND)
}

/// Load a v2 Fleet by name. Ambiguity between the two scopes is an error that
/// names both origins — the caller resolves it by asking for a scope. A file
/// under the same name in another schema (legacy/exact) is not a v2 hit.
/// Used by `workflow(fleet:)` through `fleet::exact::load_fleet_document`.
pub fn load_fleet(
    name: &str,
    workspace: &Path,
) -> Result<(FleetFile, FleetScope, PathBuf), FleetStoreError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(FleetStoreError::NotFound("<empty name>".to_string()));
    }
    let mut found = v2_fleet_candidates(name, workspace);
    if found.len() > 1 {
        return Err(FleetStoreError::Ambiguous(
            name.to_string(),
            FleetScope::Personal.label().to_string(),
            FleetScope::Workspace.label().to_string(),
        ));
    }
    let Some((scope, path)) = found.pop() else {
        return Err(FleetStoreError::NotFound(name.to_string()));
    };
    let text = fs::read_to_string(&path).map_err(|e| FleetStoreError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    let fleet = FleetFile::parse(&text).map_err(|e| FleetStoreError::Parse {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    Ok((fleet, scope, path))
}

/// Load a v2 Fleet by name in one explicit scope. Unlike [`load_fleet`],
/// this never resolves ambiguity — the caller already knows where the Fleet
/// lives (e.g. the row the user just picked).
pub fn load_fleet_in_scope(
    name: &str,
    scope: FleetScope,
    workspace: &Path,
) -> Result<(FleetFile, PathBuf), FleetStoreError> {
    let dir = match scope {
        FleetScope::Personal => personal_fleets_dir()?,
        FleetScope::Workspace => workspace_fleets_dir(workspace),
    };
    let path = dir.join(format!("{}.toml", slugify(name)));
    if !path.is_file() {
        return Err(FleetStoreError::NotFound(format!(
            "{} ({})",
            name,
            scope.label()
        )));
    }
    let text = fs::read_to_string(&path).map_err(|e| FleetStoreError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    let fleet = FleetFile::parse(&text).map_err(|e| FleetStoreError::Parse {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    Ok((fleet, path))
}

/// Load a v2 Fleet from a specific path (used by the editor on the currently
/// open entry, so the saved scope is exact). API surface for the path-based
/// editor flows; currently exercised by tests.
pub fn load_fleet_at(path: &Path) -> Result<(FleetFile, FleetScope), FleetStoreError> {
    let text = fs::read_to_string(path).map_err(|e| FleetStoreError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    let fleet = FleetFile::parse(&text).map_err(|e| FleetStoreError::Parse {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    let scope = if path.starts_with(personal_fleets_dir().unwrap_or_default()) {
        FleetScope::Personal
    } else {
        FleetScope::Workspace
    };
    Ok((fleet, scope))
}

/// Save a Fleet to a scope with an atomic write. Refuses to clobber a
/// different Fleet of the same slug (the name is the identity).
pub fn save_fleet(
    fleet: &FleetFile,
    scope: FleetScope,
    workspace: &Path,
) -> Result<PathBuf, FleetStoreError> {
    fleet.validate()?;
    let dir = ensure_fleets_dir(scope, workspace)?;
    let path = dir.join(format!("{}.toml", fleet.file_slug()));
    if path.is_file() {
        // Only a readable v2 Fleet of this same name may be replaced. Any
        // other non-empty file at this slug (a legacy roster or exact fleet,
        // a Fleet from a newer build, a Fleet mid-edit that no longer parses)
        // is left unchanged.
        let text = fs::read_to_string(&path).map_err(|e| FleetStoreError::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        if !text.trim().is_empty() {
            match FleetFile::parse(&text) {
                Ok(existing) if existing.name == fleet.name => {}
                Ok(_) => {
                    return Err(FleetStoreError::NameTaken {
                        name: fleet.name.clone(),
                        path: path.display().to_string(),
                    });
                }
                Err(parse_error) => {
                    let reason = if codewhale_workflow::fleet_exact::declared_schema_kind(&text)
                        .as_deref()
                        == Some(FLEET_SCHEMA_KIND)
                    {
                        format!("is a Fleet file this build cannot read ({parse_error})")
                    } else {
                        "holds another fleet file (a legacy roster or exact fleet)".to_string()
                    };
                    return Err(FleetStoreError::Invalid(format!(
                        "{} {reason}; it was left unchanged. Fix or move that file, or save `{}` under another name",
                        path.display(),
                        fleet.name
                    )));
                }
            }
        }
    }
    let rendered = fleet.render_toml()?;
    write_scoped(scope, workspace, &path, rendered.as_bytes())?;
    Ok(path)
}

/// Delete a saved Fleet (UI confirms first). Returns the removed path.
pub fn delete_fleet(
    name: &str,
    scope: FleetScope,
    workspace: &Path,
) -> Result<PathBuf, FleetStoreError> {
    let dir = match scope {
        FleetScope::Personal => personal_fleets_dir()?,
        FleetScope::Workspace => workspace_fleets_dir(workspace),
    };
    reject_linked_workspace_dir(scope, workspace, &dir)?;
    let path = dir.join(format!("{}.toml", slugify(name)));
    if !path.is_file() {
        return Err(FleetStoreError::NotFound(name.to_string()));
    }
    fs::remove_file(&path).map_err(|e| FleetStoreError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    // A selection that pointed at the deleted Fleet must not linger: it would
    // render as a phantom selection. The write is best-effort; a leftover
    // selection is reported by the reader as missing, never as valid.
    clear_selection_if_matching(scope, workspace, name);
    Ok(path)
}

/// The active selection: workspace selection wins, then the personal
/// user-global default. Each file is scope-explicit; a workspace selection
/// can never hide the personal Fleet — the personal default is only overridden
/// for this folder, visibly.
pub fn resolve_selected_fleet(workspace: &Path) -> Result<Option<SelectedFleet>, FleetStoreError> {
    let ws_dir = workspace_fleets_dir(workspace);
    if let Some(name) = read_selection_result(&ws_dir)? {
        // A workspace selection may name a personal Fleet (selected for this
        // folder only): resolve workspace first, then personal, and report
        // the scope the Fleet actually lives in.
        let ws_path = ws_dir.join(format!("{}.toml", slugify(&name)));
        if ws_path.is_file() {
            return Ok(Some(SelectedFleet {
                name,
                scope: FleetScope::Workspace,
                path: ws_path,
            }));
        }
        if let Ok(dir) = personal_fleets_dir() {
            let personal_path = dir.join(format!("{}.toml", slugify(&name)));
            if personal_path.is_file() {
                return Ok(Some(SelectedFleet {
                    name,
                    scope: FleetScope::Personal,
                    path: personal_path,
                }));
            }
        }
        return Err(FleetStoreError::NotFound(format!(
            "selected fleet `{name}` (folder selection at {})",
            ws_dir.join(SELECTED_FILE).display()
        )));
    }
    if let Ok(dir) = personal_fleets_dir()
        && let Some(name) = read_selection_result(&dir)?
    {
        let path = dir.join(format!("{}.toml", slugify(&name)));
        if path.is_file() {
            return Ok(Some(SelectedFleet {
                name,
                scope: FleetScope::Personal,
                path,
            }));
        }
        return Err(FleetStoreError::NotFound(format!(
            "selected fleet `{name}` (user selection at {})",
            dir.join(SELECTED_FILE).display()
        )));
    }
    Ok(None)
}

/// Compatibility projection for display-only callers. Runtime callers must
/// use [`resolve_selected_fleet`] so a broken explicit selection cannot be
/// mistaken for "no selection" and silently fall back to legacy profiles.
#[must_use]
pub fn selected_fleet(workspace: &Path) -> Option<SelectedFleet> {
    resolve_selected_fleet(workspace).ok().flatten()
}

fn read_selection_result(dir: &Path) -> Result<Option<String>, FleetStoreError> {
    let path = dir.join(SELECTED_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(FleetStoreError::Io {
                path: path.display().to_string(),
                message: error.to_string(),
            });
        }
    };
    let name = text.trim();
    if name.is_empty() {
        Ok(None)
    } else {
        Ok(Some(name.to_string()))
    }
}

fn read_selection(dir: &Path) -> Option<String> {
    read_selection_result(dir).ok().flatten()
}

/// Write the selection for a scope. Returns the exact file written.
///
/// The selection file lives in the scope's `fleets/` directory, but the
/// Fleet it names may live in either scope: a workspace selection may point
/// at a personal Fleet (selecting it for this folder only), and a personal
/// selection always points at a personal Fleet. The validation only refuses
/// a name that exists NOWHERE — a phantom selection would be a lie.
pub fn set_selected(
    name: &str,
    scope: FleetScope,
    workspace: &Path,
) -> Result<PathBuf, FleetStoreError> {
    let dir = ensure_fleets_dir(scope, workspace)?;
    let name = name.trim();
    let exists_in_scope = |target: FleetScope| {
        let target_dir = match target {
            FleetScope::Personal => personal_fleets_dir().ok(),
            FleetScope::Workspace => Some(workspace_fleets_dir(workspace)),
        };
        target_dir
            .map(|d| d.join(format!("{}.toml", slugify(name))).is_file())
            .unwrap_or(false)
    };
    let exists = exists_in_scope(scope) || exists_in_scope(FleetScope::Personal);
    if !exists {
        return Err(FleetStoreError::NotFound(format!(
            "{} ({})",
            name,
            scope.label()
        )));
    }
    let selected = dir.join(SELECTED_FILE);
    write_scoped(scope, workspace, &selected, name.as_bytes())?;
    Ok(selected)
}

fn clear_selection_if_matching(scope: FleetScope, workspace: &Path, name: &str) {
    let dir = match scope {
        FleetScope::Personal => personal_fleets_dir().ok(),
        FleetScope::Workspace => Some(workspace_fleets_dir(workspace)),
    };
    let Some(dir) = dir else { return };
    if reject_linked_workspace_dir(scope, workspace, &dir).is_err() {
        return;
    }
    let selected = dir.join(SELECTED_FILE);
    if read_selection(&dir).as_deref() == Some(name.trim()) {
        let _ = fs::remove_file(selected);
    }
}

/// Write a Fleet or selection file for `scope`. A workspace file goes through
/// the pinned no-follow writer (parents created without following a link, the
/// final name replaced, never written through); the personal scope is the
/// user's own directory and keeps the plain atomic write.
fn write_scoped(
    scope: FleetScope,
    workspace: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<(), FleetStoreError> {
    if scope != FleetScope::Workspace {
        return atomic_write(path, bytes);
    }
    let io_error = |e: std::io::Error| FleetStoreError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    };
    let relative = path
        .strip_prefix(workspace)
        .map_err(|_| FleetStoreError::Io {
            path: path.display().to_string(),
            message: "a workspace Fleet file must stay within the workspace".to_string(),
        })?;
    codewhale_util::files::WorkspaceFile::open_shared(workspace, relative, true)
        .and_then(|file| file.replace(bytes))
        .map_err(io_error)
}

/// Atomic write: temp file in the same directory, then rename. A failed write
/// never leaves a half-written Fleet or selection.
/// Each write gets its own temp file, so two concurrent savers can never
/// interleave bytes into one shared temp and publish a mixed file.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), FleetStoreError> {
    codewhale_util::utils::write_atomic_workspace(path, bytes).map_err(|e| FleetStoreError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    })
}

/// One row of the migration receipt: how a legacy role profile maps into the
/// new Fleet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRow {
    /// Role id, e.g. `scout`.
    pub id: String,
    /// The pin that will be saved (model + provider, or "inherit").
    pub pin: Option<(String, String)>,
    /// The winning origin under the legacy precedence.
    pub winner: String,
    /// A lower-precedence copy with identical content — not a conflict.
    pub identical_shadow: Option<String>,
    /// A lower-precedence copy that differed and was NOT carried over.
    pub conflicting_shadow: Option<String>,
}

/// The result of migrating the legacy per-role roster into a v2 Fleet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReceipt {
    /// The Fleet that was (or would be) saved.
    pub fleet: FleetFile,
    /// Per-role mapping, including every conflict that was resolved.
    pub rows: Vec<MigrationRow>,
    /// Path the Fleet was saved to.
    pub saved_to: PathBuf,
}

/// Build (and optionally save) a v2 Fleet from the legacy per-role roster:
/// built-ins + `[fleet.profiles]` + personal + workspace profile files.
///
/// Nothing is discarded: every role becomes a member, every pin survives, and
/// each lower-precedence copy that differed is named in the receipt. The
/// legacy files themselves are left untouched — they become migration input,
/// not live config, once a Fleet is selected.
pub fn migrate_legacy_roster(
    fleet_config: &codewhale_config::FleetConfigToml,
    workspace: &Path,
    save: bool,
    save_scope: FleetScope,
) -> Result<MigrationReceipt, FleetStoreError> {
    let roster = FleetRoster::load(fleet_config, workspace);
    let mut fleet = FleetFile::new(
        "Default".to_string(),
        Some("Migrated from the legacy per-role profile configuration.".to_string()),
    )?;
    let mut rows = Vec::new();
    for member in roster.members() {
        let profile = &member.profile;
        let (model, provider) = match (&profile.model, &profile.provider) {
            (Some(model), Some(provider)) => (Some(model.clone()), Some(provider.clone())),
            _ => (None, None),
        };
        // Legacy profiles carry no capability requirements; a migration
        // never invents one. Requirements start empty in the v2 Fleet.
        let requires: Vec<String> = Vec::new();
        let row = MigrationRow {
            id: member.id.clone(),
            pin: model
                .as_ref()
                .map(|m| (m.clone(), provider.clone().unwrap_or_default())),
            winner: member.origin.to_string(),
            identical_shadow: None,
            conflicting_shadow: None,
        };
        // Record shadowed copies (the roster already resolved them; here we
        // name them so the conflict is visible before anyone accepts it).
        let shadows: Vec<String> = roster
            .shadowed()
            .iter()
            .filter(|s| s.id == member.id)
            .map(|s| {
                format!(
                    "{} copy at {} ignored in favor of {}",
                    s.shadowed_origin,
                    s.shadowed_source.display(),
                    s.winner_origin
                )
            })
            .collect();
        let mut row = row;
        if let Some(first) = shadows.first() {
            if shadows.len() == 1 && first.contains("built-in") {
                row.identical_shadow = Some(first.clone());
            } else {
                row.conflicting_shadow = Some(shadows.join("; "));
            }
        }
        rows.push(row);
        fleet.members.push(FleetMember {
            id: member.id.clone(),
            display_name: member.display_name.clone(),
            shortlist: false,
            role: profile.role.name.clone(),
            model,
            provider,
            reasoning: profile.reasoning_effort.clone(),
            instructions: profile.role.instructions.clone(),
            requires,
        });
    }
    fleet.validate()?;
    let saved_to = if save {
        save_fleet(&fleet, save_scope, workspace)?
    } else {
        match save_scope {
            FleetScope::Personal => {
                personal_fleets_dir()?.join(format!("{}.toml", fleet.file_slug()))
            }
            FleetScope::Workspace => {
                workspace_fleets_dir(workspace).join(format!("{}.toml", fleet.file_slug()))
            }
        }
    };
    Ok(MigrationReceipt {
        fleet,
        rows,
        saved_to,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    /// A sealed CODEWHALE_HOME for personal-scope tests, created once per
    /// process. Tests must still hold `lock_test_env` before touching it.
    fn sealed_home() -> &'static Path {
        static HOME: OnceLock<PathBuf> = OnceLock::new();
        HOME.get_or_init(|| {
            let dir = tempfile::TempDir::new()
                .expect("temp dir for sealed home")
                .keep();
            std::fs::create_dir_all(dir.join("fleets")).expect("fleets dir");
            dir
        })
    }

    /// Point CODEWHALE_HOME at a sealed temp dir. Caller must hold
    /// `lock_test_env`.
    fn set_sealed_home() -> crate::test_support::EnvVarGuard {
        crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", sealed_home())
    }

    /// A workspace's `.codewhale/fleets` is repository content. Saving,
    /// selecting and deleting refuse it when it is a link, and a link at the
    /// Fleet's own file name is replaced rather than written through.
    #[cfg(unix)]
    #[test]
    fn workspace_fleet_writes_never_follow_links_out_of_the_workspace() {
        use std::os::unix::fs::symlink;
        let _lock = crate::test_support::lock_test_env();
        let _home = set_sealed_home();
        let outside = tempfile::tempdir().unwrap();

        // `.codewhale/fleets` linked: save, select and delete are all refused.
        let linked = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(linked.path().join(".codewhale")).unwrap();
        symlink(
            outside.path(),
            linked.path().join(".codewhale").join(FLEET_DIR),
        )
        .unwrap();
        let fleet = sample_fleet();
        assert!(save_fleet(&fleet, FleetScope::Workspace, linked.path()).is_err());
        assert!(set_selected("DeepSeek Flash", FleetScope::Workspace, linked.path()).is_err());
        assert!(delete_fleet("DeepSeek Flash", FleetScope::Workspace, linked.path()).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);

        // A link at the Fleet's file name is replaced, never written through.
        let plain = tempfile::tempdir().unwrap();
        let dir = plain.path().join(".codewhale").join(FLEET_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let victim = outside.path().join("victim.toml");
        std::fs::write(&victim, "keep me").unwrap();
        symlink(&victim, dir.join(format!("{}.toml", fleet.file_slug()))).unwrap();
        let saved = save_fleet(&fleet, FleetScope::Workspace, plain.path());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
        if let Ok(path) = saved {
            assert!(
                !std::fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
    }

    #[test]
    fn declared_schema_separates_unreadable_from_undeclared() {
        let dir = tempfile::tempdir().unwrap();
        let v2 = dir.path().join("v2.toml");
        std::fs::write(&v2, "name = \"a\"\nschema = \" Fleet \"\n").unwrap();
        let legacy = dir.path().join("legacy.toml");
        std::fs::write(&legacy, "name = \"b\"\n[roles]\nscout = \"scout\"\n").unwrap();
        let malformed = dir.path().join("bad.toml");
        std::fs::write(&malformed, "schema = [").unwrap();
        let missing = dir.path().join("missing.toml");

        assert!(
            declares_v2_schema(&v2),
            "case and whitespace are normalized"
        );
        assert_eq!(read_declared_schema(&legacy), Some(None));
        assert_eq!(read_declared_schema(&malformed), Some(None));
        assert_eq!(read_declared_schema(&missing), None);
        assert!(!declares_v2_schema(&legacy));
        assert!(!declares_v2_schema(&malformed));
        assert!(!declares_v2_schema(&missing));
    }

    #[test]
    fn unsealed_personal_routes_ignore_ambient_home() {
        const PROBE: &str = "CODEWHALE_TEST_AMBIENT_FLEET_PROBE";
        if std::env::var_os(PROBE).is_some() {
            let workspace = tempfile::tempdir().unwrap();
            for hold_env_lock in [false, true] {
                let _lock = hold_env_lock.then(crate::test_support::lock_test_env);
                let root = crate::test_support::unsealed_test_state_root();
                assert_eq!(personal_fleets_dir().unwrap(), root.join(FLEET_DIR));
                assert_eq!(
                    crate::fleet::profile::personal_agent_profile_dir().unwrap(),
                    root.join("agents")
                );
                assert!(resolve_selected_fleet(workspace.path()).unwrap().is_none());
                assert!(list_fleets(workspace.path()).is_empty());
                let roster = crate::fleet::identity::load_effective_roster(
                    &Default::default(),
                    workspace.path(),
                    None,
                );
                assert!(roster.load_error().is_none());
                assert!(roster.members().iter().all(|member| {
                    member.origin == crate::fleet::roster::ProfileOrigin::BuiltIn
                }));
            }
            return;
        }

        // A fresh process inherits populated operator state, without earning
        // the explicit EnvVarGuard seal used by deliberate path fixtures.
        let ambient = tempfile::tempdir().unwrap();
        let state = ambient.path().join(".codewhale");
        let fleets = state.join(FLEET_DIR);
        std::fs::create_dir_all(&fleets).unwrap();
        let fleet = sample_fleet();
        let fleet_path = fleets.join(format!("{}.toml", fleet.file_slug()));
        let contents = fleet.render_toml().unwrap();
        std::fs::write(&fleet_path, &contents).unwrap();
        std::fs::write(fleets.join(SELECTED_FILE), &fleet.name).unwrap();
        for explicit_override in [false, true] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "fleet::store::tests::unsealed_personal_routes_ignore_ambient_home",
                    "--test-threads=1",
                ])
                .env(PROBE, "1")
                .env("HOME", ambient.path())
                .env("USERPROFILE", ambient.path())
                .env_remove("CODEWHALE_HOME")
                .env_remove("CODEWHALE_CONFIG_PATH")
                .env_remove("DEEPSEEK_CONFIG_PATH");
            if explicit_override {
                command.env("CODEWHALE_HOME", &state);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "ambient route probe failed (override={explicit_override})\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(std::fs::read_to_string(fleet_path).unwrap(), contents);
        assert_eq!(
            std::fs::read_to_string(fleets.join(SELECTED_FILE)).unwrap(),
            fleet.name
        );
    }

    fn sample_fleet() -> FleetFile {
        FleetFile::new("DeepSeek Flash".to_string(), None)
            .expect("valid fleet")
            .with_operator(FleetOperator {
                provider: "deepseek".to_string(),
                model: "deepseek-v4-flash".to_string(),
                reasoning: Some("low".to_string()),
            })
            .with_member(FleetMember {
                id: "scout".to_string(),
                display_name: Some("Flash Scout".to_string()),
                shortlist: false,
                role: "scout".to_string(),
                provider: None,
                model: None,
                reasoning: None,
                instructions: None,
                requires: Vec::new(),
            })
            .with_member(FleetMember {
                id: "builder".to_string(),
                display_name: None,
                shortlist: false,
                role: "builder".to_string(),
                provider: Some("deepseek".to_string()),
                model: Some("deepseek-v4-pro".to_string()),
                reasoning: Some("high".to_string()),
                instructions: Some("Implement exactly the task slice.".to_string()),
                requires: vec!["vision".to_string()],
            })
    }

    trait FleetBuilder {
        fn with_operator(self, operator: FleetOperator) -> Self;
        fn with_member(self, member: FleetMember) -> Self;
    }

    impl FleetBuilder for FleetFile {
        fn with_operator(mut self, operator: FleetOperator) -> Self {
            self.operator = Some(operator);
            self
        }
        fn with_member(mut self, member: FleetMember) -> Self {
            self.members.push(member);
            self
        }
    }

    #[test]
    fn validation_rejects_bad_documents_with_specific_errors() {
        let _lock = crate::test_support::lock_test_env();

        // Empty name.
        let err = FleetFile::new("   ".to_string(), None).unwrap_err();
        assert!(err.to_string().contains("name must not be empty"), "{err}");

        // Duplicate member ids.
        let mut fleet = sample_fleet();
        fleet.members.push(fleet.members[0].clone());
        let err = fleet.validate().unwrap_err();
        assert!(
            err.to_string().contains("duplicate member id `scout`"),
            "{err}"
        );

        // Dispatch identity is case-insensitive, so validation must reject a
        // pair lookup could not distinguish.
        let mut fleet = sample_fleet();
        let mut duplicate = fleet.members[0].clone();
        duplicate.id = "SCOUT".to_string();
        fleet.members.push(duplicate);
        let err = fleet.validate().unwrap_err();
        assert!(err.to_string().contains("case-insensitively"), "{err}");

        // Human-facing names stay bounded and single-line before they can
        // enter selectors, roster discovery, or terminal rendering.
        let mut fleet = sample_fleet();
        fleet.members[0].display_name = Some("x".repeat(MAX_MEMBER_DISPLAY_NAME_CHARS + 1));
        let err = fleet.validate().unwrap_err();
        assert!(err.to_string().contains("no longer than 80"), "{err}");
        let mut fleet = sample_fleet();
        fleet.members[0].display_name = Some("Flash\nScout".to_string());
        let err = fleet.validate().unwrap_err();
        assert!(
            err.to_string().contains("one trimmed printable line"),
            "{err}"
        );

        // Identity and route values are bounded and control-free too; only
        // the instruction overlay may span lines.
        let mut fleet = sample_fleet();
        fleet.members[0].id = "scout\u{1b}[2J".to_string();
        let err = fleet.validate().unwrap_err();
        assert!(
            err.to_string().contains("member id must be printable"),
            "{err}"
        );
        let mut fleet = sample_fleet();
        fleet.members[0].id = "x".repeat(MAX_MEMBER_ID_CHARS + 1);
        assert!(fleet.validate().is_err());
        let mut fleet = sample_fleet();
        fleet.members[0].id = format!("{} ", fleet.members[0].id);
        assert!(
            fleet.validate().is_err(),
            "an untrimmed id is not its trimmed twin"
        );
        let mut fleet = sample_fleet();
        fleet.name = "Team\u{7}".to_string();
        assert!(fleet.validate().is_err());
        // F01-04: the rest of the document's free text is bounded too.
        let mut fleet = sample_fleet();
        fleet.name = "Team\n".to_string();
        assert!(
            fleet.validate().is_err(),
            "a trailing control is still a control"
        );
        let mut fleet = sample_fleet();
        fleet.description = Some("about\u{7}".to_string());
        assert!(fleet.validate().is_err());
        fleet.description = Some("line one\nline two".to_string());
        fleet.validate().expect("a description may span lines");
        fleet.description = Some("x".repeat(MAX_FLEET_DESCRIPTION_CHARS + 1));
        assert!(fleet.validate().is_err());
        let mut fleet = sample_fleet();
        fleet.operator = Some(FleetOperator {
            provider: "deepseek".to_string(),
            model: "deepseek-v4\u{1b}[2J".to_string(),
            reasoning: None,
        });
        let err = fleet.validate().unwrap_err();
        assert!(err.to_string().contains("operator model"), "{err}");
        let mut fleet = sample_fleet();
        fleet.members[0].instructions = Some("line one\nline two\ttabbed".to_string());
        fleet.validate().expect("instructions may span lines");
        fleet.members[0].instructions = Some("x".repeat(MAX_MEMBER_INSTRUCTIONS_CHARS + 1));
        assert!(fleet.validate().is_err());

        // Lone provider / lone model: never silently reinterpreted.
        let mut fleet = sample_fleet();
        fleet.members[0].provider = Some("deepseek".to_string());
        let err = fleet.validate().unwrap_err();
        assert!(
            err.to_string().contains("must pin both provider and model"),
            "{err}"
        );
        let mut fleet = sample_fleet();
        fleet.members[0].model = Some("deepseek-v4-pro".to_string());
        let err = fleet.validate().unwrap_err();
        assert!(
            err.to_string().contains("must pin both provider and model"),
            "{err}"
        );

        // Unknown capability requirement.
        let mut fleet = sample_fleet();
        fleet.members[0].requires = vec!["telepathy".to_string()];
        let err = fleet.validate().unwrap_err();
        assert!(
            err.to_string().contains("unknown capability `telepathy`"),
            "{err}"
        );
        assert!(err.to_string().contains("vision"), "{err}");
    }

    #[test]
    fn member_pin_matching_the_operator_route_reads_as_inheritance() {
        // #6037: a role member pinned to the fleet's own operator route
        // resolves to that route either way — the pin only stops it
        // following when the operator moves. Parse drops the redundant pin;
        // a different-route pin and a shortlist row keep theirs.
        let fleet = FleetFile::parse(
            r#"schema = "fleet"
schema_revision = 2
name = "Inherit"

[operator]
provider = "openrouter"
model = "z-ai/glm-5.3"

[[members]]
id = "planner"
role = "planner"
provider = "openrouter"
model = "z-ai/glm-5.3"

[[members]]
id = "builder"
role = "builder"
provider = "openrouter"
model = "z-ai/glm-5.3-pro"

[[members]]
id = "choice"
shortlist = true
provider = "openrouter"
model = "z-ai/glm-5.3"
"#,
        )
        .expect("parse");
        let planner = fleet.member("planner").expect("planner member");
        assert_eq!(planner.provider, None);
        assert_eq!(planner.model, None);
        let builder = fleet.member("builder").expect("builder member");
        assert_eq!(builder.provider.as_deref(), Some("openrouter"));
        assert_eq!(builder.model.as_deref(), Some("z-ai/glm-5.3-pro"));
        let choice = fleet
            .members
            .iter()
            .find(|member| member.shortlist)
            .expect("shortlist row");
        assert_eq!(choice.provider.as_deref(), Some("openrouter"));
        assert_eq!(choice.model.as_deref(), Some("z-ai/glm-5.3"));
        // The listing still attributes the inherited role to the route it runs.
        let models = crate::fleet::members::models_of(&fleet);
        assert_eq!(models[0].model, "z-ai/glm-5.3");
        assert_eq!(models[0].roles, ["operator", "planner"]);
        assert_eq!(models[1].roles, ["builder"]);
        // The cleaned document round-trips: inherit stays inherit.
        assert_eq!(
            FleetFile::parse(&fleet.render_toml().expect("render")).expect("reparse"),
            fleet
        );
    }

    #[test]
    fn render_parse_round_trip_preserves_every_field() {
        let fleet = sample_fleet();
        let text = fleet.render_toml().expect("render");
        let parsed = FleetFile::parse(&text).expect("parse");
        assert_eq!(parsed, fleet);
        assert!(text.contains("schema = \"fleet\""));
        assert!(text.contains("schema_revision = 2"));
        assert!(text.contains("display_name = \"Flash Scout\""));
        assert!(text.contains("deepseek-v4-flash"));
        assert!(
            !text.contains("shortlist"),
            "legacy members do not gain a marker"
        );
    }

    #[test]
    fn marked_shortlist_round_trips_without_becoming_a_role_or_legacy_bare_pin() {
        let fleet = FleetFile::parse(
            r#"schema = "fleet"
schema_revision = 2
name = "Shortlist"

[[members]]
id = "scout"
shortlist = true
provider = "fixture-provider"
model = "scout"
"#,
        )
        .expect("explicitly marked model survives legacy bare-pin migration");
        assert_eq!(fleet.members.len(), 1);
        let choice = &fleet.members[0];
        assert!(choice.shortlist);
        assert_eq!(choice.provider.as_deref(), Some("fixture-provider"));
        assert_eq!(choice.model.as_deref(), Some("scout"));
        assert!(choice.role_label().is_empty());
        assert!(
            !fleet.has_scout(),
            "a model named scout cannot select the scout role"
        );
        assert!(fleet.member("scout").is_none());
        let models = crate::fleet::members::models_of(&fleet);
        assert_eq!(models.len(), 1);
        assert!(models[0].roles.is_empty());
        let text = fleet.render_toml().expect("serialize marker");
        assert!(text.contains("shortlist = true"));
        assert!(!text.contains("role ="));
        assert_eq!(FleetFile::parse(&text).expect("reload marker"), fleet);
    }

    #[test]
    fn shortlist_marker_rejects_roles_inheritance_and_incomplete_routes() {
        for (role, provider, model) in [
            ("scout", Some("deepseek"), Some("deepseek-v4-flash")),
            ("", None, None),
            ("", None, Some("deepseek-v4-flash")),
            ("", Some("deepseek"), None),
            ("", Some("  "), Some("deepseek-v4-flash")),
            ("", Some("deepseek"), Some("  ")),
        ] {
            let member: FleetMember = serde_json::from_value(serde_json::json!({
                "id": "choice", "shortlist": true, "role": role,
                "provider": provider, "model": model,
            }))
            .expect("typed fixture");
            let mut fleet = FleetFile::new("Malformed shortlist".into(), None).unwrap();
            fleet.members.push(member);
            assert!(
                fleet.validate().is_err(),
                "accepted invalid marker: {fleet:?}"
            );
            assert!(
                fleet.render_toml().is_err(),
                "render accepted invalid marker"
            );
            let text = toml::to_string(&fleet).expect("unchecked fixture serialization");
            assert!(
                FleetFile::parse(&text).is_err(),
                "parse accepted invalid marker: {text}"
            );
        }

        for metadata in [
            serde_json::json!({"reasoning": "high"}),
            serde_json::json!({"instructions": "Review the changes."}),
            serde_json::json!({"requires": ["vision"]}),
        ] {
            let mut row = serde_json::json!({
                "id": "choice", "shortlist": true,
                "provider": "deepseek", "model": "deepseek-v4-flash",
            });
            row.as_object_mut()
                .unwrap()
                .extend(metadata.as_object().unwrap().clone());
            let mut fleet = FleetFile::new("Malformed shortlist".into(), None).unwrap();
            fleet.members.push(serde_json::from_value(row).unwrap());
            let text = toml::to_string(&fleet).expect("unchecked metadata fixture");
            for error in [
                fleet.validate().unwrap_err(),
                fleet.render_toml().unwrap_err(),
                FleetFile::parse(&text).unwrap_err(),
            ] {
                assert!(
                    error.to_string().contains("cannot set role reasoning"),
                    "metadata was not rejected as role-only: {error}"
                );
            }
        }
    }

    #[test]
    fn member_name_alias_is_accepted_and_old_files_remain_valid() {
        let aliased = FleetFile::parse(
            r#"schema = "fleet"
schema_revision = 2
name = "Named"

[[members]]
id = "scout"
name = "Scout One"
role = "scout"
"#,
        )
        .expect("name alias");
        assert_eq!(
            aliased.members[0].display_name.as_deref(),
            Some("Scout One")
        );
        let canonical = aliased.render_toml().expect("canonical render");
        assert!(canonical.contains("display_name = \"Scout One\""));

        let without_name = FleetFile::parse(
            r#"schema = "fleet"
schema_revision = 2
name = "Existing"

[[members]]
id = "scout"
role = "scout"
"#,
        )
        .expect("pre-display-name revision-2 file");
        assert!(without_name.members[0].display_name.is_none());
    }

    #[test]
    fn save_load_round_trips_in_workspace_scope() {
        let _lock = crate::test_support::lock_test_env();
        let ws = tempfile::TempDir::new().unwrap();
        let fleet = sample_fleet();
        let path = save_fleet(&fleet, FleetScope::Workspace, ws.path()).expect("save");
        assert!(
            path.ends_with(".codewhale/fleets/deepseek-flash.toml"),
            "{path:?}"
        );

        let (loaded, scope, path) = load_fleet("DeepSeek Flash", ws.path()).expect("load");
        assert_eq!(loaded, fleet);
        assert_eq!(scope, FleetScope::Workspace);
        assert_eq!(path, load_fleet("DeepSeek Flash", ws.path()).unwrap().2);

        // A same-name Fleet in the personal scope makes the bare name
        // ambiguous — the reader names both origins instead of shadowing.
        let _home = set_sealed_home();
        save_fleet(&fleet, FleetScope::Personal, ws.path()).expect("save personal");
        let err = load_fleet("DeepSeek Flash", ws.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("defined in both"), "{msg}");
        assert!(msg.contains("user") && msg.contains("folder"), "{msg}");
    }

    #[test]
    fn selection_is_scope_explicit_and_workspace_wins() {
        let _lock = crate::test_support::lock_test_env();
        let _home = set_sealed_home();
        let ws = tempfile::TempDir::new().unwrap();
        let fleet = sample_fleet();

        // No selection yet.
        assert!(selected_fleet(ws.path()).is_none());

        // Personal selection: the user-global default.
        save_fleet(&fleet, FleetScope::Personal, ws.path()).unwrap();
        let selected_file =
            set_selected("DeepSeek Flash", FleetScope::Personal, ws.path()).expect("select");
        assert!(
            selected_file.ends_with("fleets/selected"),
            "{selected_file:?}"
        );
        let sel = selected_fleet(ws.path()).expect("selected");
        assert_eq!(sel.scope, FleetScope::Personal);
        assert_eq!(sel.name, "DeepSeek Flash");

        // A selection naming a missing Fleet is refused — a phantom selection
        // would be a lie.
        let err = set_selected("No Such Fleet", FleetScope::Personal, ws.path()).unwrap_err();
        assert!(err.to_string().contains("No Such Fleet"), "{err}");

        // Workspace selection overrides for this folder only.
        save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();
        set_selected("DeepSeek Flash", FleetScope::Workspace, ws.path()).unwrap();
        let sel = selected_fleet(ws.path()).expect("selected");
        assert_eq!(sel.scope, FleetScope::Workspace);
        // Deleting the workspace Fleet clears the workspace selection; the
        // personal default reappears rather than a phantom.
        delete_fleet("DeepSeek Flash", FleetScope::Workspace, ws.path()).unwrap();
        let sel = selected_fleet(ws.path()).expect("personal default returns");
        assert_eq!(sel.scope, FleetScope::Personal);
    }

    #[test]
    fn stale_explicit_selection_is_an_error_not_legacy_fallback() {
        let _lock = crate::test_support::lock_test_env();
        let _home = set_sealed_home();
        let ws = tempfile::TempDir::new().unwrap();
        let dir = workspace_fleets_dir(ws.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(SELECTED_FILE), "Missing Fleet\n").unwrap();

        let error = resolve_selected_fleet(ws.path()).expect_err("stale selection must fail");
        assert!(
            error.to_string().contains("selected fleet `Missing Fleet`"),
            "{error}"
        );
        assert!(error.to_string().contains("folder selection"), "{error}");
    }

    #[test]
    fn list_marks_legacy_files_without_hiding_them() {
        let _lock = crate::test_support::lock_test_env();
        let _home = set_sealed_home();
        let ws = tempfile::TempDir::new().unwrap();

        save_fleet(&sample_fleet(), FleetScope::Personal, ws.path()).unwrap();
        // A legacy exact fleet file (workflow schema) in the same directory
        // must be listed as legacy, never silently absent.
        let legacy = r#"schema = "exact"
schema_revision = 1
name = "stopship"
members = []"#;
        std::fs::write(sealed_home().join("fleets/stopship.toml"), legacy).unwrap();

        let entries = list_fleets(ws.path());
        assert_eq!(entries.len(), 2, "{entries:?}");
        let stopship = entries
            .iter()
            .find(|e| e.name == "stopship")
            .expect("legacy fleet listed");
        assert!(stopship.legacy, "{stopship:?}");
        assert!(stopship.parse_error.is_some(), "{stopship:?}");
        let flash = entries.iter().find(|e| e.name == "DeepSeek Flash").unwrap();
        assert!(!flash.legacy && flash.parse_error.is_none(), "{flash:?}");
    }

    #[test]
    fn save_refuses_to_clobber_a_different_fleet_of_the_same_slug() {
        let _lock = crate::test_support::lock_test_env();
        let ws = tempfile::TempDir::new().unwrap();
        let fleet = sample_fleet();
        save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();

        let mut other = FleetFile::new("DeepSeek Flash!".to_string(), None).unwrap();
        other.members = fleet.members.clone();
        let err = save_fleet(&other, FleetScope::Workspace, ws.path()).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn save_refuses_to_overwrite_a_legacy_fleet_file_of_the_same_slug() {
        let _lock = crate::test_support::lock_test_env();
        let ws = tempfile::TempDir::new().unwrap();
        let dir = ws.path().join(".codewhale/fleets");
        std::fs::create_dir_all(&dir).unwrap();
        let legacy_path = dir.join("default.toml");
        let legacy = "[roles.builder]\nmodel = \"deepseek-v4-flash\"\n";
        std::fs::write(&legacy_path, legacy).unwrap();

        let fleet = FleetFile::new("Default".to_string(), None).unwrap();
        let err = save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap_err();
        assert!(err.to_string().contains("left unchanged"), "{err}");
        assert_eq!(std::fs::read_to_string(&legacy_path).unwrap(), legacy);

        // A saved v2 Fleet of the same name is still updated in place.
        std::fs::remove_file(&legacy_path).unwrap();
        save_fleet(&fleet, FleetScope::Workspace, ws.path()).expect("first save");
        save_fleet(&fleet, FleetScope::Workspace, ws.path()).expect("re-save");
    }

    #[test]
    fn save_leaves_an_unreadable_fleet_file_of_the_same_slug_unchanged() {
        let _lock = crate::test_support::lock_test_env();
        let ws = tempfile::TempDir::new().unwrap();
        let dir = ws.path().join(".codewhale/fleets");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("default.toml");
        let fleet = FleetFile::new("Default".to_string(), None).unwrap();

        for existing in [
            // Written by a newer build.
            "schema = \"fleet\"\nschema_revision = 3\nname = \"Default\"\n",
            // The schema declaration other readers normalize.
            "schema = \"Fleet\"\nschema_revision = 2\nname = \"Default\"\n",
            // A v2 Fleet the user is mid-edit on.
            "schema = \"fleet\"\nschema_revision = 2\nname = \"Default\"\nmembers = [\n",
        ] {
            std::fs::write(&path, existing).unwrap();
            let err = save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap_err();
            let message = err.to_string();
            assert!(message.contains("left unchanged"), "{message}");
            assert!(!message.contains("migrate"), "{message}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), existing);
        }
        let declared = "schema = \"fleet\"\nschema_revision = 3\nname = \"Default\"\n";
        std::fs::write(&path, declared).unwrap();
        let message = save_fleet(&fleet, FleetScope::Workspace, ws.path())
            .unwrap_err()
            .to_string();
        assert!(message.contains("cannot read"), "{message}");

        // An empty placeholder is not a Fleet worth keeping.
        std::fs::write(&path, "").unwrap();
        save_fleet(&fleet, FleetScope::Workspace, ws.path()).expect("replace empty file");
        assert!(
            load_fleet_at(&path).is_ok(),
            "the saved Fleet must read back"
        );
    }

    #[test]
    fn migration_preserves_pins_and_names_shadowing() {
        let _lock = crate::test_support::lock_test_env();
        let ws = tempfile::TempDir::new().unwrap();

        // A workspace legacy profile file with a pin.
        let agents_dir = ws.path().join(".codewhale/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("scout.toml"),
            r#"id = "scout"
display_name = "Scout One"
role_hint = "scout"
model = "deepseek-v4-flash"
provider = "deepseek"
"#,
        )
        .unwrap();

        let receipt = migrate_legacy_roster(
            &codewhale_config::FleetConfigToml::default(),
            ws.path(),
            true,
            FleetScope::Workspace,
        )
        .expect("migration");

        assert_eq!(receipt.fleet.name, "Default");
        let scout = receipt.fleet.member("scout").expect("scout member");
        assert_eq!(scout.display_name.as_deref(), Some("Scout One"));
        assert_eq!(scout.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(scout.provider.as_deref(), Some("deepseek"));
        assert!(receipt.saved_to.ends_with("fleets/default.toml"));
        // The legacy profile file itself is untouched.
        assert!(
            std::fs::read_to_string(agents_dir.join("scout.toml"))
                .unwrap()
                .contains("model = \"deepseek-v4-flash\"")
        );
    }
}
