//! Fleet profile vocabulary, local profile discovery, and config-facing aliases.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::reasoning_preference::ReasoningEffort;

#[allow(unused_imports)]
pub use codewhale_config::{
    FleetDelegationHints, FleetLoadout, FleetProfile, FleetProfilePermissions, FleetRole, FleetSlot,
};

pub use super::roster::ProfileOrigin;

pub const WORKSPACE_AGENT_PROFILE_DIR: &str = ".codewhale/agents";
pub const PERSONAL_AGENT_PROFILE_DIR: &str = "agents";
/// Claude Code agent definitions, read from both the project and the home
/// directory (`<workspace>/.claude/agents`, `~/.claude/agents`).
pub const CLAUDE_AGENT_DIR: &str = ".claude/agents";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FleetProfileScope {
    Project,
    Personal,
}

impl FleetProfileScope {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Personal => "personal",
        }
    }

    #[must_use]
    pub fn display_dir(self) -> &'static str {
        match self {
            Self::Project => WORKSPACE_AGENT_PROFILE_DIR,
            Self::Personal => "$CODEWHALE_HOME/agents",
        }
    }

    #[must_use]
    pub fn toggled(self) -> Self {
        match self {
            Self::Project => Self::Personal,
            Self::Personal => Self::Project,
        }
    }
}

pub fn personal_agent_profile_dir() -> Result<PathBuf> {
    #[cfg(any(test, feature = "test-support"))]
    if !crate::test_support::guarded_environment_provides_state_paths() {
        return Ok(crate::test_support::unsealed_test_state_root().join(PERSONAL_AGENT_PROFILE_DIR));
    }
    Ok(codewhale_config::codewhale_home()?.join(PERSONAL_AGENT_PROFILE_DIR))
}

pub fn agent_profile_dir_for_scope(scope: FleetProfileScope, workspace: &Path) -> Result<PathBuf> {
    match scope {
        FleetProfileScope::Project => Ok(workspace.join(WORKSPACE_AGENT_PROFILE_DIR)),
        FleetProfileScope::Personal => personal_agent_profile_dir(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProfile {
    pub id: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    /// Closed capability requirements carried by selected v2 Fleet members.
    /// Legacy/profile sources leave this empty; consumers must never infer a
    /// capability from the member name or model prefix.
    pub requires: Vec<String>,
    pub profile: FleetProfile,
    pub source: PathBuf,
    /// Roster layer this profile came from (#fleet-roster cutover (v0.8.67)).
    /// File-based loading in this module always yields `Workspace`; the
    /// roster stamps `BuiltIn` / `Config` for the other layers.
    pub origin: ProfileOrigin,
    /// Runtime authority for a profile loaded from an immutable plugin
    /// snapshot. Rechecked at Agent spawn so another process can revoke it.
    pub plugin_authority: Option<codewhale_config::PluginAuthority>,
    pub native_preset: Option<codewhale_plugins::composition_scope::NativePresetRef>,
}

/// The minimum profile information needed to prevent a save from clobbering
/// another file.  Identity discovery intentionally accepts otherwise legacy
/// profile keys: an old route-policy field must not block authoring an
/// unrelated, current profile, but malformed TOML or an invalid id still fails
/// closed because the collision check cannot be trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProfileIdentity {
    pub id: String,
    pub source: PathBuf,
}

/// Keep a failed definition's identity so selecting it cannot run a lower
/// roster layer by accident. Parser excerpts stay in logs, not tool output.
#[derive(Debug, Clone, Serialize)]
pub struct AgentProfileLoadIssue {
    pub id: String,
    pub source: PathBuf,
    pub origin: ProfileOrigin,
    #[serde(skip_serializing)]
    pub detail: String,
}

impl AgentProfileLoadIssue {
    fn new(path: &Path, id: Option<&str>, origin: ProfileOrigin, detail: String) -> Self {
        Self {
            id: id
                .or_else(|| path.file_stem().and_then(|stem| stem.to_str()))
                .unwrap_or("profile")
                .to_string(),
            source: path.to_path_buf(),
            origin,
            detail,
        }
    }
}

impl std::fmt::Display for AgentProfileLoadIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentProfileToml {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    role_hint: Option<String>,
    #[serde(default)]
    base_role: Option<String>,
    #[serde(default)]
    persona: Option<String>,
    #[serde(default)]
    loadout: Option<String>,
    #[serde(default, alias = "model_hint", alias = "model_id")]
    model: Option<String>,
    /// Explicit provider id for `model` (#4093), e.g. `"deepseek"` or
    /// `"openrouter"`. Validated against the known `ProviderKind` vocabulary at
    /// load time — never inferred by sniffing `model` for a provider-shaped
    /// substring (EPIC #2608). `deny_unknown_fields` no longer needs to guard
    /// this name: it is now a first-class, validated field instead of a
    /// smuggled one.
    #[serde(default)]
    provider: Option<String>,
    /// Optional saved thinking tier for this profile (#4137). TOML may use
    /// the canonical `reasoning_effort` spelling or the UI-facing `thinking`
    /// / `reasoning` aliases; loading normalizes to a canonical setting label.
    #[serde(default, alias = "thinking", alias = "reasoning")]
    reasoning_effort: Option<String>,
    #[serde(default)]
    instructions: Option<AgentProfileInstructions>,
    #[serde(default)]
    tools: Option<AgentProfileTools>,
    #[serde(default)]
    permissions: Option<AgentProfilePermissionsToml>,
}

#[derive(Debug, Deserialize)]
struct AgentProfileIdentityToml {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentProfileInstructions {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentProfileTools {
    #[serde(default)]
    posture: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentProfilePermissionsToml {
    #[serde(default)]
    allow_shell: Option<bool>,
    #[serde(default)]
    trust: Option<bool>,
    #[serde(default)]
    approval_required: Option<bool>,
}

pub fn load_workspace_agent_profiles(workspace: impl AsRef<Path>) -> Result<Vec<AgentProfile>> {
    let workspace = workspace.as_ref();
    load_agent_profiles_from_dir_in(Some(workspace), workspace.join(WORKSPACE_AGENT_PROFILE_DIR))
}

/// Read one profile file. Under a workspace `root` a linked file or linked
/// parent is refused (the shared confined-file refusal), so a repository
/// cannot make a profile read land outside itself. `None` is for the user's
/// own directories and installed plugins, which are not workspace content.
fn read_profile_text(root: Option<&Path>, path: &Path) -> std::io::Result<String> {
    match root {
        Some(root) => codewhale_util::fs_confined::read_to_string(root, path),
        None => std::fs::read_to_string(path),
    }
}

/// Refuse a profile directory that reaches outside `root` through a link.
fn check_profile_dir_confined(root: Option<&Path>, dir: &Path) -> Result<()> {
    match root {
        Some(root) => codewhale_util::files::reject_linked_path(root, dir)
            .with_context(|| format!("agent profile directory {}", dir.display())),
        None => Ok(()),
    }
}

/// Load every valid workspace profile while reporting invalid neighbors
/// individually.  The runtime roster uses this path so one stale profile does
/// not hide a newly-authored valid profile (or the rest of the party).
pub fn load_workspace_agent_profiles_tolerant(
    workspace: impl AsRef<Path>,
) -> Result<(Vec<AgentProfile>, Vec<AgentProfileLoadIssue>)> {
    let workspace = workspace.as_ref();
    let dir = workspace.join(WORKSPACE_AGENT_PROFILE_DIR);
    load_agent_profiles_from_dir_tolerant_in(Some(workspace), dir, ProfileOrigin::Workspace)
}

pub fn load_agent_profiles_from_dir_tolerant(
    dir: impl AsRef<Path>,
    origin: ProfileOrigin,
) -> Result<(Vec<AgentProfile>, Vec<AgentProfileLoadIssue>)> {
    load_agent_profiles_from_dir_tolerant_in(None, dir, origin)
}

fn load_agent_profiles_from_dir_tolerant_in(
    root: Option<&Path>,
    dir: impl AsRef<Path>,
    origin: ProfileOrigin,
) -> Result<(Vec<AgentProfile>, Vec<AgentProfileLoadIssue>)> {
    let dir = dir.as_ref();
    let paths = agent_profile_paths(root, dir)?;
    let mut profiles = Vec::new();
    let mut issues = Vec::new();
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    let mut identified = Vec::new();

    // Resolve identities first so duplicate ids fail closed as a group rather
    // than allowing whichever filename happens to sort first to win.
    for path in paths {
        match load_agent_profile_identity_file(root, &path) {
            Ok(identity) => {
                let canonical_id = identity.id.to_ascii_lowercase();
                if !seen.insert(canonical_id.clone()) {
                    duplicates.insert(canonical_id.clone());
                }
                identified.push((path, identity, canonical_id));
            }
            Err(err) => issues.push(AgentProfileLoadIssue::new(
                &path,
                None,
                origin,
                format!("{err:#}"),
            )),
        }
    }

    for (path, identity, canonical_id) in identified {
        if duplicates.contains(&canonical_id) {
            issues.push(AgentProfileLoadIssue::new(
                &path,
                Some(&identity.id),
                origin,
                format!(
                    "duplicate agent profile id {} includes {}",
                    canonical_id,
                    path.display()
                ),
            ));
            continue;
        }
        match load_agent_profile_file(root, &path) {
            Ok(mut profile) => {
                profile.origin = origin;
                profiles.push(profile);
            }
            Err(err) => issues.push(AgentProfileLoadIssue::new(
                &path,
                Some(&identity.id),
                origin,
                format!("{err:#}"),
            )),
        }
    }

    Ok((profiles, issues))
}

pub fn load_plugin_agent_profiles_from_component(
    component: &Path,
    authority: &codewhale_config::PluginAuthority,
) -> Result<(Vec<AgentProfile>, Vec<AgentProfileLoadIssue>)> {
    let (mut profiles, issues) = if component.is_dir() {
        load_agent_profiles_from_dir_tolerant(component, ProfileOrigin::Plugin)?
    } else if component.is_file() {
        match load_agent_profile_file(None, component) {
            Ok(mut profile) => {
                profile.origin = ProfileOrigin::Plugin;
                (vec![profile], Vec::new())
            }
            Err(error) => {
                let identity = load_agent_profile_identity_file(None, component).ok();
                (
                    Vec::new(),
                    vec![AgentProfileLoadIssue::new(
                        component,
                        identity.as_ref().map(|identity| identity.id.as_str()),
                        ProfileOrigin::Plugin,
                        format!("{error:#}"),
                    )],
                )
            }
        }
    } else {
        return Err(anyhow!(
            "plugin Agent component is unavailable: {}",
            component.display()
        ));
    };
    for profile in &mut profiles {
        profile.plugin_authority = Some(authority.clone());
    }
    Ok((profiles, issues))
}

/// Claude Code's user-level agent directory (`~/.claude/agents`). Tests never
/// read the developer's real home directory.
pub fn claude_user_agent_dir() -> Option<PathBuf> {
    if cfg!(any(test, feature = "test-support")) {
        return None;
    }
    dirs::home_dir().map(|home| home.join(CLAUDE_AGENT_DIR))
}

/// Claude Code tools that only read. An agent whose `tools:` list stays inside
/// this set runs on the read-only `explore` posture.
const CLAUDE_READ_ONLY_TOOLS: &[&str] = &[
    "read",
    "grep",
    "glob",
    "ls",
    "webfetch",
    "websearch",
    "notebookread",
    "todowrite",
    "todoread",
];

/// Claude Code tools that change files. Any of these in `tools:` maps the
/// agent to the `implement` posture.
const CLAUDE_WRITE_TOOLS: &[&str] = &["write", "edit", "multiedit", "notebookedit"];

/// Load Claude Code agent definitions (`*.md` with YAML frontmatter) from one
/// directory. Frontmatter is read by the same parser as `SKILL.md`.
///
/// The mapping is deliberately small: `name` → id, `description` →
/// description, the Markdown body → role instructions, `tools` → the closest
/// role posture, `model` → model hint (Claude's `inherit`/`sonnet`/`opus`/
/// `haiku` aliases inherit the session route). `color` is cosmetic and
/// ignored. Any other key would change behavior Codewhale cannot reproduce, so
/// that file becomes a load issue instead of silently loading as something
/// else. Permissions stay at the floor: a Markdown agent can never grant shell
/// or trust — only a `.codewhale/agents/*.toml` profile can.
pub fn load_claude_agent_profiles_from_dir(
    dir: impl AsRef<Path>,
) -> Result<(Vec<AgentProfile>, Vec<AgentProfileLoadIssue>)> {
    load_claude_agent_profiles_from_dir_in(None, dir)
}

/// [`load_claude_agent_profiles_from_dir`] for a directory that belongs to a
/// workspace: with a `root`, links below it are refused.
pub fn load_claude_agent_profiles_from_dir_in(
    root: Option<&Path>,
    dir: impl AsRef<Path>,
) -> Result<(Vec<AgentProfile>, Vec<AgentProfileLoadIssue>)> {
    let dir = dir.as_ref();
    check_profile_dir_confined(root, dir)?;
    if !dir.is_dir() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut paths = std::fs::read_dir(dir)
        .with_context(|| format!("reading Claude agent dir {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("md"))
        .collect::<Vec<_>>();
    paths.sort();

    let origin = ProfileOrigin::ClaudeCode;
    let mut profiles: Vec<AgentProfile> = Vec::new();
    let mut issues = Vec::new();
    for path in paths {
        match load_claude_agent_file(root, &path) {
            Ok(profile) => {
                if profiles
                    .iter()
                    .any(|existing| existing.id.eq_ignore_ascii_case(&profile.id))
                {
                    issues.push(AgentProfileLoadIssue::new(
                        &path,
                        Some(&profile.id),
                        origin,
                        format!(
                            "duplicate Claude agent name {} in {}",
                            profile.id,
                            dir.display()
                        ),
                    ));
                    continue;
                }
                profiles.push(profile);
            }
            Err(err) => issues.push(AgentProfileLoadIssue::new(
                &path,
                None,
                origin,
                format!("{err:#}"),
            )),
        }
    }
    Ok((profiles, issues))
}

fn load_claude_agent_file(root: Option<&Path>, path: &Path) -> Result<AgentProfile> {
    let raw = read_profile_text(root, path)
        .with_context(|| format!("reading Claude agent {}", path.display()))?;
    let (metadata, body) = codewhale_skill_install::frontmatter::parse_frontmatter(&raw)
        .map_err(|err| anyhow!("parsing Claude agent {}: {err}", path.display()))?
        .ok_or_else(|| {
            anyhow!(
                "Claude agent {} has no `---` frontmatter block",
                path.display()
            )
        })?;

    let mut unmapped: Vec<&str> = metadata
        .keys()
        .map(String::as_str)
        .filter(|key| !matches!(*key, "name" | "description" | "tools" | "model" | "color"))
        .collect();
    if !unmapped.is_empty() {
        unmapped.sort_unstable();
        bail!(
            "Claude agent {} uses frontmatter Codewhale cannot honor ({}); remove it, or define this agent as a .codewhale/agents/*.toml profile",
            path.display(),
            unmapped.join(", ")
        );
    }

    let fallback_id = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("agent");
    let id = non_empty_trimmed(metadata.get("name").map(String::as_str))
        .unwrap_or(fallback_id)
        .to_string();
    validate_agent_profile_token(path, "name", &id)?;

    let role_name = claude_tools_role(metadata.get("tools").map(String::as_str)).to_string();
    let model = non_empty_trimmed(metadata.get("model").map(String::as_str))
        .filter(|model| {
            !matches!(
                model.to_ascii_lowercase().as_str(),
                "inherit" | "sonnet" | "opus" | "haiku"
            )
        })
        .map(str::to_string);
    validate_agent_profile_model_hint(path, model.as_deref())?;

    let description =
        non_empty_trimmed(metadata.get("description").map(String::as_str)).map(str::to_string);
    let instructions = trimmed_non_empty(body).map(str::to_string);
    Ok(AgentProfile {
        native_preset: None,
        id,
        display_name: None,
        description: description.clone(),
        requires: Vec::new(),
        profile: FleetProfile {
            slot: FleetSlot::from_name(&role_name),
            role: FleetRole {
                name: role_name,
                description,
                instructions,
            },
            loadout: FleetLoadout::default(),
            model,
            provider: None,
            reasoning_effort: None,
            permissions: FleetProfilePermissions::default(),
            delegation: FleetDelegationHints::default(),
        },
        source: path.to_path_buf(),
        origin: ProfileOrigin::ClaudeCode,
        plugin_authority: None,
    })
}

/// Closest Codewhale role posture for a Claude `tools:` allowlist. No list
/// means Claude's "all tools", which is the documented `general` default.
fn claude_tools_role(tools: Option<&str>) -> &'static str {
    let Some(tools) = tools.and_then(trimmed_non_empty) else {
        return "general";
    };
    let names: Vec<String> = tools
        .split([',', ' ', '\t'])
        .map(|tool| {
            tool.trim()
                .trim_matches(['[', ']', '"', '\''])
                .to_ascii_lowercase()
        })
        .filter(|tool| !tool.is_empty())
        .collect();
    if names
        .iter()
        .any(|tool| CLAUDE_WRITE_TOOLS.contains(&tool.as_str()))
    {
        "implement"
    } else if names
        .iter()
        .all(|tool| CLAUDE_READ_ONLY_TOOLS.contains(&tool.as_str()))
    {
        "explore"
    } else {
        // Bash or MCP tools without file writes: the shell-capable posture.
        "test"
    }
}

/// Read only the identity-bearing fields from workspace profiles for the
/// authoring collision gate.  Unknown legacy fields are harmless here because
/// no profile behavior is loaded or executed from this representation.
pub fn load_workspace_agent_profile_identities(
    workspace: impl AsRef<Path>,
) -> Result<Vec<AgentProfileIdentity>> {
    let workspace = workspace.as_ref();
    let dir = workspace.join(WORKSPACE_AGENT_PROFILE_DIR);
    load_agent_profile_identities_from_dir_in(Some(workspace), dir)
}

pub fn load_agent_profile_identities_from_dir(
    dir: impl AsRef<Path>,
) -> Result<Vec<AgentProfileIdentity>> {
    load_agent_profile_identities_from_dir_in(None, dir)
}

fn load_agent_profile_identities_from_dir_in(
    root: Option<&Path>,
    dir: impl AsRef<Path>,
) -> Result<Vec<AgentProfileIdentity>> {
    let dir = dir.as_ref();
    agent_profile_paths(root, dir)?
        .into_iter()
        .map(|path| load_agent_profile_identity_file(root, &path))
        .collect()
}

pub fn load_agent_profiles_from_dir(dir: impl AsRef<Path>) -> Result<Vec<AgentProfile>> {
    load_agent_profiles_from_dir_in(None, dir)
}

fn load_agent_profiles_from_dir_in(
    root: Option<&Path>,
    dir: impl AsRef<Path>,
) -> Result<Vec<AgentProfile>> {
    let dir = dir.as_ref();
    let mut profiles = Vec::new();
    let mut seen = BTreeSet::new();
    for path in agent_profile_paths(root, dir)? {
        let profile = load_agent_profile_file(root, &path)?;
        if !seen.insert(profile.id.to_ascii_lowercase()) {
            bail!("duplicate agent profile id {}", profile.id);
        }
        profiles.push(profile);
    }
    Ok(profiles)
}

fn agent_profile_paths(root: Option<&Path>, dir: &Path) -> Result<Vec<PathBuf>> {
    check_profile_dir_confined(root, dir)?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    if !dir.is_dir() {
        bail!("agent profile path {} is not a directory", dir.display());
    }

    let mut paths = std::fs::read_dir(dir)
        .with_context(|| format!("reading agent profile dir {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("reading agent profile entries in {}", dir.display()))?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("toml"))
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn load_agent_profile_identity_file(
    root: Option<&Path>,
    path: &Path,
) -> Result<AgentProfileIdentity> {
    let raw = read_profile_text(root, path)
        .with_context(|| format!("reading agent profile identity {}", path.display()))?;
    let parsed: AgentProfileIdentityToml = toml::from_str(&raw)
        .map_err(|err| anyhow!("parsing agent profile identity {}: {err}", path.display()))?;
    let fallback_id = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("profile");
    let id = first_present([parsed.id.as_deref(), parsed.name.as_deref()])
        .unwrap_or(fallback_id)
        .to_string();
    validate_agent_profile_token(path, "id/name", &id)?;
    Ok(AgentProfileIdentity {
        id,
        source: path.to_path_buf(),
    })
}

fn load_agent_profile_file(root: Option<&Path>, path: &Path) -> Result<AgentProfile> {
    let raw = read_profile_text(root, path)
        .with_context(|| format!("reading agent profile {}", path.display()))?;
    let parsed: AgentProfileToml = toml::from_str(&raw)
        .map_err(|err| anyhow!("parsing agent profile {}: {err}", path.display()))?;
    agent_profile_from_toml(path, parsed)
}

fn agent_profile_from_toml(path: &Path, parsed: AgentProfileToml) -> Result<AgentProfile> {
    reject_permission_expansion(path, parsed.tools.as_ref(), parsed.permissions.as_ref())?;

    let fallback_id = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("profile");
    let id = first_present([parsed.id.as_deref(), parsed.name.as_deref()])
        .unwrap_or(fallback_id)
        .to_string();
    validate_agent_profile_token(path, "id/name", &id)?;

    let role_name = canonical_public_role_name(
        first_present([
            parsed.base_role.as_deref(),
            parsed.role_hint.as_deref(),
            parsed.name.as_deref(),
        ])
        .unwrap_or(&id),
    );
    validate_agent_profile_token(path, "base_role/role_hint", &role_name)?;

    let loadout = first_present([parsed.loadout.as_deref()])
        .map(FleetLoadout::from_name)
        .unwrap_or_default();
    let model = non_empty_trimmed(parsed.model.as_deref()).map(str::to_string);
    validate_agent_profile_model_hint(path, model.as_deref())?;

    let provider = non_empty_trimmed(parsed.provider.as_deref())
        .map(str::to_string)
        .map(|provider| validate_agent_profile_provider(path, &provider).map(|()| provider))
        .transpose()?;
    let reasoning_effort =
        normalize_agent_profile_reasoning_effort(path, parsed.reasoning_effort.as_deref())?;

    let instructions = parsed
        .instructions
        .as_ref()
        .and_then(|instructions| non_empty_trimmed(instructions.text.as_deref()))
        .or_else(|| non_empty_trimmed(parsed.persona.as_deref()))
        .map(str::to_string);

    let description = non_empty_trimmed(parsed.description.as_deref()).map(str::to_string);
    let profile = FleetProfile {
        slot: FleetSlot::from_name(&role_name),
        role: FleetRole {
            name: role_name,
            description: description.clone(),
            instructions,
        },
        loadout,
        model,
        provider,
        reasoning_effort,
        permissions: FleetProfilePermissions::default(),
        delegation: FleetDelegationHints::default(),
    };

    Ok(AgentProfile {
        native_preset: None,
        id,
        display_name: non_empty_trimmed(parsed.display_name.as_deref()).map(str::to_string),
        description,
        requires: Vec::new(),
        profile,
        source: path.to_path_buf(),
        origin: ProfileOrigin::Workspace,
        plugin_authority: None,
    })
}

/// Canonicalize renamed public Fleet roles at profile load boundaries.
///
/// Profile ids remain untouched so an older file can still be addressed by
/// its saved id. Only the semantic role is migrated; every new receipt and UI
/// label derived from it therefore uses the canonical public token.
pub fn canonical_public_role_name(role: &str) -> String {
    codewhale_agent_contract::role::public_role_label(role)
}

fn reject_permission_expansion(
    path: &Path,
    tools: Option<&AgentProfileTools>,
    permissions: Option<&AgentProfilePermissionsToml>,
) -> Result<()> {
    if let Some(posture) = tools
        .and_then(|tools| tools.posture.as_deref())
        .and_then(trimmed_non_empty)
    {
        match posture {
            "read-only" | "readonly" | "read_only" => {}
            other => bail!(
                "agent profile {} tools.posture={other:?} would widen permissions; use FleetProfile policy for grants",
                path.display()
            ),
        }
    }

    if let Some(permissions) = permissions {
        if permissions.allow_shell.unwrap_or(false) {
            bail!(
                "agent profile {} may not request allow_shell=true",
                path.display()
            );
        }
        if permissions.trust.unwrap_or(false) {
            bail!(
                "agent profile {} may not request trust=true",
                path.display()
            );
        }
        if permissions.approval_required == Some(false) {
            bail!(
                "agent profile {} may not disable approval_required",
                path.display()
            );
        }
    }
    Ok(())
}

fn validate_agent_profile_token(path: &Path, field: &str, value: &str) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("agent profile {} {field} cannot be empty", path.display());
    }
    if trimmed != value || !trimmed.chars().all(is_agent_profile_token_char) {
        bail!(
            "agent profile {} {field} must be a simple token",
            path.display()
        );
    }
    Ok(())
}

fn validate_agent_profile_model_hint(path: &Path, value: Option<&str>) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if !is_model_hint(value) {
        bail!(
            "agent profile {} model must be a visible model id without whitespace or secrets",
            path.display()
        );
    }
    Ok(())
}

/// Validate an explicit `provider` field as a safe provider id (#4093).
///
/// Built-in providers are accepted by the runtime vocabulary, and user-named
/// OpenAI-compatible custom providers are accepted as simple tokens so the
/// launch path can resolve `[providers.<id>]` from the session config (#3965).
/// This field remains the ONLY place a profile's provider is established:
/// callers never infer it from `model` (EPIC #2608).
fn validate_agent_profile_provider(path: &Path, value: &str) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("agent profile {} provider cannot be empty", path.display());
    }
    if trimmed != value || !trimmed.chars().all(is_agent_profile_token_char) {
        bail!(
            "agent profile {} provider must be a simple provider id",
            path.display()
        );
    }
    Ok(())
}

fn normalize_agent_profile_reasoning_effort(
    path: &Path,
    value: Option<&str>,
) -> Result<Option<String>> {
    let Some(value) = non_empty_trimmed(value) else {
        return Ok(None);
    };
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "inherit" | "parent" | "same" | "current" | "default" | "unset"
    ) {
        return Ok(None);
    }
    ReasoningEffort::parse_strict(value)
        .map(|effort| Some(effort.as_setting().to_string()))
        .map_err(|_| {
            anyhow!(
                "agent profile {} reasoning_effort {value:?} must be one of: inherit, auto, off, low, medium, high, max",
                path.display()
            )
        })
}

fn is_agent_profile_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')
}

fn is_model_hint(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed == value
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_graphic() && !matches!(ch, '=' | '\'' | '"'))
}

fn first_present<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Option<&'a str> {
    values.into_iter().flatten().find_map(trimmed_non_empty)
}

fn non_empty_trimmed(value: Option<&str>) -> Option<&str> {
    value.and_then(trimmed_non_empty)
}

fn trimmed_non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Outcome of parsing untrusted model output into a fleet profile draft.
/// Mirrors `UntrustedDraftParse` from the constitution pipeline: the reply is
/// data, never trusted, and any failure is a reason string for the status
/// line — drafting failures degrade to the manual authoring flow.
#[derive(Debug)]
pub enum UntrustedProfileParse {
    Drafted(Box<FleetProfileDraft>),
    Empty,
    Invalid(String),
}

/// A model-drafted fleet agent profile that has passed the untrusted gate:
/// balanced-JSON extraction, serde parse with `deny_unknown_fields` (so
/// provider/base_url/api_key/permissions/tools cannot ride along), the same
/// escalation rejections the profile loader applies, token and model-hint
/// validation, prose bounds, and control-character stripping. The persisted
/// TOML is rendered deterministically from this struct — model bytes are
/// never written to disk verbatim.
///
/// `provider` (#4093) is set ONLY by the structured Fleet setup picker (a
/// user's explicit, credential-checked selection) — never by
/// [`Self::from_untrusted_json`], whose wire schema
/// ([`FleetProfileDraftJson`]) has no `provider` field and rejects one via
/// `deny_unknown_fields`. A model's untrusted reply can never smuggle a
/// provider; only an interactive pick can set this field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetProfileDraft {
    pub id: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub role_hint: String,
    pub model_class_hint: Option<String>,
    pub model: Option<String>,
    /// Explicit provider id for `model` (e.g. `"deepseek"`), set only by the
    /// structured picker. `None` means "no route pin" (inherit) — matching
    /// `model: None` — or a legacy/untrusted draft that predates this field.
    pub provider: Option<String>,
    /// Explicit saved thinking tier, set only by structured setup controls.
    /// `None` means inherit the operator/session reasoning tier.
    pub reasoning_effort: Option<String>,
    pub instructions: Option<String>,
}

/// Bounds for model-drafted profile prose. Same philosophy as the
/// constitution bounds: roomy enough for a real profile, hard enough that a
/// misbehaving provider cannot bloat the store.
pub const MAX_PROFILE_DESCRIPTION_LEN: usize = 1000;
pub const MAX_PROFILE_INSTRUCTIONS_LEN: usize = 4000;
const MAX_PROFILE_DISPLAY_NAME_LEN: usize = 80;
const MAX_PROFILE_TOKEN_LEN: usize = 64;

/// The JSON shape the drafting prompt asks for. `deny_unknown_fields` is the
/// first escalation gate: a draft that tries to smuggle `permissions`,
/// `tools`, `provider`, `base_url`, or `api_key` fails the parse outright
/// instead of being silently stripped.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FleetProfileDraftJson {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    role_hint: Option<String>,
    #[serde(default)]
    model_class_hint: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    instructions: Option<String>,
}

impl FleetProfileDraft {
    /// Parse untrusted model output. Any structural problem is `Invalid`
    /// with a short reason; a parse that carries no usable content is
    /// `Empty`.
    #[must_use]
    pub fn from_untrusted_json(raw: &str) -> UntrustedProfileParse {
        let Some(json) = extract_first_json_object(raw) else {
            return UntrustedProfileParse::Invalid("no JSON object found".to_string());
        };
        let parsed: FleetProfileDraftJson = match serde_json::from_str(json) {
            Ok(parsed) => parsed,
            Err(err) => return UntrustedProfileParse::Invalid(err.to_string()),
        };

        let role_hint = match parsed
            .role_hint
            .as_deref()
            .and_then(trimmed_non_empty)
            .map(sanitize_profile_token)
        {
            Some(token) if !token.is_empty() => canonical_public_role_name(&token),
            _ => return UntrustedProfileParse::Invalid("role_hint missing".to_string()),
        };
        let id = parsed
            .id
            .as_deref()
            .and_then(trimmed_non_empty)
            .map(sanitize_profile_token)
            .filter(|token| !token.is_empty())
            .unwrap_or_else(|| role_hint.clone());
        let model_class_hint = parsed
            .model_class_hint
            .as_deref()
            .and_then(trimmed_non_empty)
            .map(sanitize_profile_token)
            .filter(|token| !token.is_empty());
        let model = parsed
            .model
            .as_deref()
            .and_then(trimmed_non_empty)
            .map(str::to_string);
        if let Some(ref model) = model
            && !is_model_hint(model)
        {
            return UntrustedProfileParse::Invalid(
                "model must be a visible model id without whitespace or secrets".to_string(),
            );
        }
        let display_name = parsed
            .display_name
            .as_deref()
            .map(|text| sanitize_profile_prose(text, MAX_PROFILE_DISPLAY_NAME_LEN))
            .and_then(|text| trimmed_non_empty(&text).map(str::to_string));
        let description = parsed
            .description
            .as_deref()
            .map(|text| sanitize_profile_prose(text, MAX_PROFILE_DESCRIPTION_LEN))
            .and_then(|text| trimmed_non_empty(&text).map(str::to_string));
        let instructions = parsed
            .instructions
            .as_deref()
            .map(|text| sanitize_profile_prose(text, MAX_PROFILE_INSTRUCTIONS_LEN))
            .and_then(|text| trimmed_non_empty(&text).map(str::to_string));

        let draft = FleetProfileDraft {
            id,
            display_name,
            description,
            role_hint,
            model_class_hint,
            model,
            // Never set from untrusted model output — `FleetProfileDraftJson`
            // has no `provider` field, so there is nothing to read here.
            provider: None,
            reasoning_effort: None,
            instructions,
        };
        if draft.description.is_none() && draft.instructions.is_none() {
            return UntrustedProfileParse::Empty;
        }
        UntrustedProfileParse::Drafted(Box::new(draft))
    }

    /// Deterministic TOML rendering — the exact bytes the ratify keypress
    /// would persist. Loading this back through the profile loader must
    /// succeed with the default (floor) permissions.
    #[must_use]
    pub fn render_toml(&self) -> String {
        let mut root = toml::value::Table::new();
        root.insert("id".to_string(), toml::Value::String(self.id.clone()));
        if let Some(ref display_name) = self.display_name {
            root.insert(
                "display_name".to_string(),
                toml::Value::String(display_name.clone()),
            );
        }
        if let Some(ref description) = self.description {
            root.insert(
                "description".to_string(),
                toml::Value::String(description.clone()),
            );
        }
        root.insert(
            "role_hint".to_string(),
            toml::Value::String(self.role_hint.clone()),
        );
        if let Some(ref hint) = self.model_class_hint {
            root.insert("loadout".to_string(), toml::Value::String(hint.clone()));
        }
        if let Some(ref model) = self.model {
            root.insert("model".to_string(), toml::Value::String(model.clone()));
            // A provider pin is only meaningful alongside a concrete model
            // (#4093): an `inherit` draft (`model: None`) never carries one,
            // so the rendered TOML can't imply a route it doesn't have.
            if let Some(ref provider) = self.provider {
                root.insert(
                    "provider".to_string(),
                    toml::Value::String(provider.clone()),
                );
            }
        }
        if let Some(ref reasoning_effort) = self.reasoning_effort {
            root.insert(
                "reasoning_effort".to_string(),
                toml::Value::String(reasoning_effort.clone()),
            );
        }
        if let Some(ref instructions) = self.instructions {
            let mut table = toml::value::Table::new();
            table.insert(
                "text".to_string(),
                toml::Value::String(instructions.clone()),
            );
            root.insert("instructions".to_string(), toml::Value::Table(table));
        }
        toml::to_string_pretty(&toml::Value::Table(root))
            .unwrap_or_else(|_| String::from("# failed to render profile"))
    }

    /// File name (stem + `.toml`) for this draft, always derived from the
    /// sanitized id — never a model-chosen free-form path.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{}.toml", self.id)
    }
}

/// Keep only the loader's token alphabet, lowercased, bounded.
fn sanitize_profile_token(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|ch| ch.to_ascii_lowercase())
        .filter(|ch| is_agent_profile_token_char(*ch))
        .take(MAX_PROFILE_TOKEN_LEN)
        .collect()
}

/// Strip control characters (newline/tab survive) and bound length by chars.
fn sanitize_profile_prose(text: &str, max_len: usize) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .take(max_len)
        .collect()
}

/// Extract the first balanced `{...}` object from untrusted output, so fenced
/// or prose-wrapped JSON still parses. Mirrors the constitution pipeline's
/// extractor (which is private to codewhale-config).
fn extract_first_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in raw[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(&raw[start..=start + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn draft_gate_rejects_unknown_and_escalation_fields() {
        for raw in [
            r#"{"id":"x","role_hint":"reviewer","description":"d","permissions":{"allow_shell":true}}"#,
            r#"{"id":"x","role_hint":"reviewer","description":"d","tools":{"posture":"full"}}"#,
            r#"{"id":"x","role_hint":"reviewer","description":"d","provider":"openai"}"#,
            r#"{"id":"x","role_hint":"reviewer","description":"d","api_key":"sk-nope"}"#,
        ] {
            assert!(
                matches!(
                    FleetProfileDraft::from_untrusted_json(raw),
                    UntrustedProfileParse::Invalid(_)
                ),
                "{raw} must be rejected, not stripped"
            );
        }
    }

    #[test]
    fn draft_gate_bounds_and_sanitizes() {
        let huge = "x".repeat(MAX_PROFILE_INSTRUCTIONS_LEN + 500);
        // \u0007 (BEL) inside the description must be stripped by the
        // prose sanitizer; the oversized instructions must be bounded.
        let raw = format!(
            "{{\"id\":\"  Weird ID!!  \",\"role_hint\":\"Code Reviewer\",\"description\":\"has\\u0007control\",\"instructions\":\"{huge}\"}}"
        );
        let UntrustedProfileParse::Drafted(draft) = FleetProfileDraft::from_untrusted_json(&raw)
        else {
            panic!("draft should parse");
        };
        assert_eq!(draft.id, "weirdid");
        assert_eq!(draft.role_hint, "codereviewer");
        assert_eq!(draft.description.as_deref(), Some("hascontrol"));
        assert_eq!(
            draft.instructions.as_deref().unwrap().chars().count(),
            MAX_PROFILE_INSTRUCTIONS_LEN
        );
    }

    #[test]
    fn draft_gate_rejects_secret_shaped_model_and_missing_role() {
        assert!(matches!(
            FleetProfileDraft::from_untrusted_json(
                r#"{"id":"x","role_hint":"reviewer","description":"d","model":"has secret ="}"#
            ),
            UntrustedProfileParse::Invalid(_)
        ));
        assert!(matches!(
            FleetProfileDraft::from_untrusted_json(r#"{"id":"x","description":"d"}"#),
            UntrustedProfileParse::Invalid(_)
        ));
        assert!(matches!(
            FleetProfileDraft::from_untrusted_json(r#"{"id":"x","role_hint":"reviewer"}"#),
            UntrustedProfileParse::Empty
        ));
    }

    #[test]
    fn draft_gate_accepts_fenced_output() {
        let raw = "Here you go:\n```json\n{\"id\":\"reviewer\",\"role_hint\":\"reviewer\",\"description\":\"Reviews diffs.\"}\n```";
        assert!(matches!(
            FleetProfileDraft::from_untrusted_json(raw),
            UntrustedProfileParse::Drafted(_)
        ));
    }

    #[test]
    fn rendered_draft_round_trips_through_the_loader_with_floor_permissions() {
        let UntrustedProfileParse::Drafted(draft) = FleetProfileDraft::from_untrusted_json(
            r#"{"id":"reviewer","display_name":"Reviewer","description":"Reviews diffs for correctness.","role_hint":"reviewer","model_class_hint":"cheap","model":"glm-5.2","instructions":"Read the diff.\nReport findings, then stop."}"#,
        ) else {
            panic!("draft should parse");
        };

        let dir = TempDir::new().unwrap();
        let path = write_profile(dir.path(), &draft.file_name(), &draft.render_toml());
        let profiles = load_agent_profiles_from_dir(dir.path()).expect("rendered TOML loads");
        assert_eq!(profiles.len(), 1);
        let loaded = &profiles[0];
        assert_eq!(loaded.id, "reviewer");
        assert_eq!(loaded.display_name.as_deref(), Some("Reviewer"));
        assert_eq!(loaded.profile.model.as_deref(), Some("glm-5.2"));
        assert_eq!(
            loaded.profile.role.instructions.as_deref(),
            Some("Read the diff.\nReport findings, then stop.")
        );
        // The loader always installs the permission floor, no matter what.
        assert_eq!(
            loaded.profile.permissions,
            FleetProfilePermissions::default()
        );
        assert_eq!(path, loaded.source);
    }

    #[test]
    fn draft_with_explicit_provider_round_trips_through_the_loader() {
        // A structured (picker-driven) draft that pins a model on a provider
        // other than whatever the parent session happens to use (#4093): the
        // rendered TOML must carry both fields explicitly, and the loader
        // must read the provider back out verbatim — never re-derive it by
        // sniffing `model` for a provider-shaped substring.
        let draft = FleetProfileDraft {
            id: "scout-deepseek".to_string(),
            display_name: Some("Scout".to_string()),
            description: Some("Cross-provider scout profile.".to_string()),
            role_hint: "scout".to_string(),
            model_class_hint: None,
            model: Some("deepseek-v4-flash".to_string()),
            provider: Some("deepseek".to_string()),
            reasoning_effort: None,
            instructions: None,
        };

        let rendered = draft.render_toml();
        assert!(
            rendered.contains("provider = \"deepseek\""),
            "rendered TOML must persist the explicit provider: {rendered}"
        );
        assert!(rendered.contains("model = \"deepseek-v4-flash\""));

        let dir = TempDir::new().unwrap();
        write_profile(dir.path(), &draft.file_name(), &rendered);
        let profiles = load_agent_profiles_from_dir(dir.path()).expect("rendered TOML loads");
        assert_eq!(profiles.len(), 1);
        let loaded = &profiles[0];
        assert_eq!(loaded.profile.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(loaded.profile.provider.as_deref(), Some("deepseek"));
    }

    #[test]
    fn draft_with_reasoning_effort_round_trips_through_the_loader() {
        let draft = FleetProfileDraft {
            id: "scout-deep".to_string(),
            display_name: Some("Scout".to_string()),
            description: Some("Deep scout profile.".to_string()),
            role_hint: "scout".to_string(),
            model_class_hint: None,
            model: Some("deepseek-v4-pro".to_string()),
            provider: Some("deepseek".to_string()),
            reasoning_effort: Some("max".to_string()),
            instructions: None,
        };

        let rendered = draft.render_toml();
        assert!(
            rendered.contains("reasoning_effort = \"max\""),
            "rendered TOML must persist explicit reasoning: {rendered}"
        );

        let dir = TempDir::new().unwrap();
        write_profile(dir.path(), &draft.file_name(), &rendered);
        let profiles = load_agent_profiles_from_dir(dir.path()).expect("rendered TOML loads");
        assert_eq!(profiles.len(), 1);
        let loaded = &profiles[0];
        assert_eq!(loaded.profile.provider.as_deref(), Some("deepseek"));
        assert_eq!(loaded.profile.model.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(loaded.profile.reasoning_effort.as_deref(), Some("max"));
    }

    #[test]
    fn profile_loader_normalizes_reasoning_aliases() {
        let dir = TempDir::new().unwrap();
        write_profile(
            dir.path(),
            "scout.toml",
            r#"
id = "scout"
role_hint = "scout"
thinking = "ultracode"

[instructions]
text = "Scout deeply."
"#,
        );

        let profiles = load_agent_profiles_from_dir(dir.path()).expect("profile TOML loads");
        assert_eq!(profiles.len(), 1);
        // `xhigh` used to land here too; the thinking ladder made it a rung of
        // its own, so `ultracode` is the alias left to exercise.
        assert_eq!(
            profiles[0].profile.reasoning_effort.as_deref(),
            Some("ultra")
        );
    }

    #[test]
    fn profile_loader_migrates_advisory_role_aliases_to_consultant() {
        let dir = tempfile::tempdir().unwrap();
        for alias in ["oracle", "advisor"] {
            let path = dir.path().join(format!("{alias}.toml"));
            std::fs::write(
                &path,
                format!("id = \"{alias}\"\nrole_hint = \"{alias}\"\n"),
            )
            .unwrap();
            let loaded = load_agent_profile_file(None, &path).expect("load compatibility profile");
            assert_eq!(loaded.id, alias, "saved identity remains addressable");
            assert_eq!(loaded.profile.role.name, "advisor");
            assert_eq!(loaded.profile.slot.as_str(), "advisor");
        }
    }

    #[test]
    fn model_draft_migrates_advisory_role_alias_to_consultant() {
        let UntrustedProfileParse::Drafted(draft) = FleetProfileDraft::from_untrusted_json(
            r#"{"id":"second-opinion","role_hint":"oracle","description":"Counsel."}"#,
        ) else {
            panic!("expected a drafted profile");
        };
        assert_eq!(draft.role_hint, "advisor");
        assert!(draft.render_toml().contains("role_hint = \"advisor\""));
    }

    #[test]
    fn profile_loader_rejects_unknown_reasoning_effort() {
        let dir = TempDir::new().unwrap();
        write_profile(
            dir.path(),
            "scout.toml",
            r#"
id = "scout"
role_hint = "scout"
reasoning = "expensive"
"#,
        );

        let err = load_agent_profiles_from_dir(dir.path()).expect_err("invalid effort must fail");
        assert!(
            err.to_string().contains("reasoning_effort"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn inherit_draft_never_renders_a_provider_without_a_model() {
        // `provider` is only meaningful alongside a concrete model pin; an
        // `inherit` draft (no `model`) must never render one even if a stale
        // caller sets the field.
        let draft = FleetProfileDraft {
            id: "inherit".to_string(),
            display_name: None,
            description: None,
            role_hint: "general".to_string(),
            model_class_hint: None,
            model: None,
            provider: Some("deepseek".to_string()),
            reasoning_effort: None,
            instructions: None,
        };
        let rendered = draft.render_toml();
        assert!(!rendered.contains("provider"), "{rendered}");
    }

    #[test]
    fn claude_tools_map_to_the_closest_role_posture() {
        assert_eq!(claude_tools_role(None), "general");
        assert_eq!(claude_tools_role(Some("  ")), "general");
        assert_eq!(
            claude_tools_role(Some("Read, Grep, Glob, WebFetch")),
            "explore"
        );
        assert_eq!(claude_tools_role(Some("[Read, Grep]")), "explore");
        assert_eq!(claude_tools_role(Some("Read, Bash")), "test");
        assert_eq!(claude_tools_role(Some("Read, mcp__github__search")), "test");
        assert_eq!(claude_tools_role(Some("Read, Edit")), "implement");
        assert_eq!(claude_tools_role(Some("Bash MultiEdit")), "implement");
    }

    #[test]
    fn claude_agent_without_frontmatter_or_name_is_handled() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_profile(tmp.path(), "plain.md", "# Just a heading\nno frontmatter\n");
        write_profile(
            tmp.path(),
            "from-stem.md",
            "---\ndescription: >\n  Folded\n  description\nmodel: deepseek-v4-pro\n---\nBody.\n",
        );
        let (profiles, issues) = load_claude_agent_profiles_from_dir(tmp.path()).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, "from-stem");
        assert_eq!(
            profiles[0].description.as_deref(),
            Some("Folded description")
        );
        assert_eq!(
            profiles[0].profile.model.as_deref(),
            Some("deepseek-v4-pro")
        );
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "plain");
        assert!(
            issues[0].detail.contains("no `---` frontmatter"),
            "{}",
            issues[0].detail
        );
    }

    #[test]
    fn claude_agent_tools_as_a_yaml_list_keep_the_read_only_posture() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_profile(
            tmp.path(),
            "reader.md",
            "---\nname: reader\ntools:\n  - Read\n  - \"Grep\"\ndescription: Reads\n---\nBody.\n",
        );
        write_profile(
            tmp.path(),
            "writer.md",
            "---\nname: writer\ntools:\n- Read\n- Edit\n---\nBody.\n",
        );
        let (mut profiles, issues) = load_claude_agent_profiles_from_dir(tmp.path()).unwrap();
        assert!(issues.is_empty(), "{issues:?}");
        profiles.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(profiles[0].id, "reader");
        assert_eq!(profiles[0].profile.role.name, "explore");
        assert_eq!(profiles[0].description.as_deref(), Some("Reads"));
        assert_eq!(profiles[1].id, "writer");
        assert_eq!(profiles[1].profile.role.name, "implement");
    }

    fn write_profile(dir: &Path, filename: &str, contents: &str) -> PathBuf {
        let path = dir.join(filename);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn fleet_profile_round_trips_through_serde_with_safe_defaults() {
        let profile = FleetProfile::default();

        let serialized = toml::to_string(&profile).expect("profile serializes");
        let round_tripped: FleetProfile =
            toml::from_str(&serialized).expect("profile deserializes");

        assert_eq!(round_tripped, profile);
        assert_eq!(round_tripped.role.name, "general");
        assert_eq!(round_tripped.loadout, FleetLoadout::Inherit);
        assert!(!round_tripped.permissions.allow_shell);
        assert!(!round_tripped.permissions.trust);
        assert!(round_tripped.permissions.approval_required);
        assert_eq!(round_tripped.delegation.max_spawn_depth, None);
        assert_eq!(round_tripped.delegation.max_concurrency, None);
    }

    #[test]
    fn fleet_profile_explicit_toml_parses_role_loadout_permissions() {
        let profile: FleetProfile = toml::from_str(
            r#"
slot = "reviewer"
loadout = "deep-reasoning"

[role]
name = "verifier"
instructions = "Review the patch and produce verification evidence."

[permissions]
allow_shell = true
trust = true
approval_required = false

[delegation]
max_spawn_depth = 1
concurrency = 2
"#,
        )
        .expect("explicit fleet profile parses");

        assert_eq!(profile.slot, FleetSlot::Reviewer);
        assert_eq!(profile.role.name, "verifier");
        assert_eq!(
            profile.role.instructions.as_deref(),
            Some("Review the patch and produce verification evidence.")
        );
        assert_eq!(
            profile.loadout,
            FleetLoadout::Custom("deep-reasoning".to_string())
        );
        assert!(profile.permissions.allow_shell);
        assert!(profile.permissions.trust);
        assert!(!profile.permissions.approval_required);
        assert_eq!(profile.delegation.max_spawn_depth, Some(1));
        assert_eq!(profile.delegation.max_concurrency, Some(2));
    }

    #[test]
    fn fleet_profile_accepts_compact_role_string() {
        let profile: FleetProfile = toml::from_str(
            r#"
role = "scout"
loadout = "fast"
model = "deepseek-v4-flash"
"#,
        )
        .expect("compact fleet profile parses");

        assert_eq!(profile.role.name, "scout");
        assert_eq!(profile.loadout, FleetLoadout::Fast);
        assert_eq!(profile.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(profile.permissions, FleetProfilePermissions::default());
    }

    #[test]
    fn agent_profile_loader_returns_empty_for_missing_workspace_dir() {
        let tmp = TempDir::new().unwrap();

        let profiles = load_workspace_agent_profiles(tmp.path()).unwrap();

        assert!(profiles.is_empty());
    }

    #[test]
    fn profile_identity_loader_accepts_legacy_route_policy_fields() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join(WORKSPACE_AGENT_PROFILE_DIR);
        std::fs::create_dir_all(&agents_dir).unwrap();
        let source = write_profile(
            &agents_dir,
            "reviewer.toml",
            r#"
id = "reviewer"
role_hint = "reviewer"
model_class_hint = "heavy"
models = ["glm-5.2", "deepseek-v4-pro"]
"#,
        );

        let identities = load_workspace_agent_profile_identities(tmp.path())
            .expect("legacy fields do not obscure identity");

        assert_eq!(
            identities,
            vec![AgentProfileIdentity {
                id: "reviewer".to_string(),
                source,
            }]
        );
    }

    #[test]
    fn profile_identity_loader_fails_closed_for_malformed_toml() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join(WORKSPACE_AGENT_PROFILE_DIR);
        std::fs::create_dir_all(&agents_dir).unwrap();
        write_profile(&agents_dir, "broken.toml", "id = [\n");

        let err = load_workspace_agent_profile_identities(tmp.path())
            .expect_err("malformed TOML cannot prove collision safety")
            .to_string();

        assert!(err.contains("broken.toml"), "unexpected error: {err}");
        assert!(err.contains("profile identity"), "unexpected error: {err}");
    }

    #[test]
    fn tolerant_loader_keeps_valid_profile_beside_legacy_profile() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join(WORKSPACE_AGENT_PROFILE_DIR);
        std::fs::create_dir_all(&agents_dir).unwrap();
        write_profile(
            &agents_dir,
            "reviewer.toml",
            "id = \"reviewer\"\nmodel_class_hint = \"heavy\"\n",
        );
        write_profile(
            &agents_dir,
            "scout.toml",
            "id = \"scout\"\nrole_hint = \"scout\"\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-flash\"\n",
        );

        let (profiles, issues) = load_workspace_agent_profiles_tolerant(tmp.path())
            .expect("directory discovery succeeds");

        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, "scout");
        assert_eq!(
            profiles[0].profile.model.as_deref(),
            Some("deepseek-v4-flash")
        );
        assert_eq!(issues.len(), 1);
        assert!(issues[0].detail.contains("reviewer.toml"), "{issues:?}");
        assert!(issues[0].detail.contains("model_class_hint"), "{issues:?}");
    }

    /// A workspace must not make a profile read land outside itself: a linked
    /// profile file, or a linked profile directory, is refused and the content
    /// behind the link never loads.
    #[cfg(unix)]
    #[test]
    fn workspace_profiles_do_not_follow_links_out_of_the_workspace() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let hidden = "id = \"smuggled\"\nrole_hint = \"scout\"\n";
        write_profile(outside.path(), "smuggled.toml", hidden);

        let agents_dir = tmp.path().join(WORKSPACE_AGENT_PROFILE_DIR);
        std::fs::create_dir_all(&agents_dir).unwrap();
        write_profile(
            &agents_dir,
            "scout.toml",
            "id = \"scout\"\nrole_hint = \"scout\"\n",
        );
        symlink(
            outside.path().join("smuggled.toml"),
            agents_dir.join("linked.toml"),
        )
        .unwrap();

        let (profiles, issues) = load_workspace_agent_profiles_tolerant(tmp.path())
            .expect("directory discovery succeeds");
        assert_eq!(
            profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["scout"]
        );
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].detail.contains("linked.toml"), "{issues:?}");
        assert!(
            load_workspace_agent_profile_identities(tmp.path()).is_err(),
            "the identity reader refuses the linked file too"
        );
        assert!(load_workspace_agent_profiles(tmp.path()).is_err());

        // A linked profile directory is refused as a whole.
        let other = TempDir::new().unwrap();
        std::fs::create_dir_all(other.path().join(".codewhale")).unwrap();
        symlink(
            outside.path(),
            other.path().join(WORKSPACE_AGENT_PROFILE_DIR),
        )
        .unwrap();
        let error = load_workspace_agent_profiles_tolerant(other.path())
            .expect_err("a linked profile directory is refused");
        assert!(
            format!("{error:#}").contains("Refusing symlinked"),
            "{error:#}"
        );

        // The same directory is the user's own when no workspace root applies.
        let (personal, _) = load_agent_profiles_from_dir_tolerant(
            other.path().join(WORKSPACE_AGENT_PROFILE_DIR),
            ProfileOrigin::Personal,
        )
        .expect("an operator-owned directory may be a link");
        assert_eq!(personal.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn workspace_claude_agent_files_do_not_follow_links_out_of_the_workspace() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        write_profile(
            outside.path(),
            "smuggled.md",
            "---\nname: smuggled\ndescription: x\n---\nBody.\n",
        );
        let dir = tmp.path().join(CLAUDE_AGENT_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        symlink(outside.path().join("smuggled.md"), dir.join("linked.md")).unwrap();

        let (profiles, issues) =
            load_claude_agent_profiles_from_dir_in(Some(tmp.path()), &dir).unwrap();
        assert!(profiles.is_empty(), "{profiles:?}");
        assert_eq!(issues.len(), 1, "{issues:?}");
        // Without a workspace root the same file still loads: only workspace
        // content is confined.
        let (profiles, _) = load_claude_agent_profiles_from_dir(&dir).unwrap();
        assert_eq!(profiles.len(), 1);
    }

    #[test]
    fn tolerant_loader_skips_every_duplicate_id_but_keeps_unique_neighbors() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join(WORKSPACE_AGENT_PROFILE_DIR);
        std::fs::create_dir_all(&agents_dir).unwrap();
        write_profile(&agents_dir, "a.toml", "id = \"reviewer\"\n");
        write_profile(&agents_dir, "b.toml", "name = \"reviewer\"\n");
        write_profile(&agents_dir, "scout.toml", "id = \"scout\"\n");

        let (profiles, issues) = load_workspace_agent_profiles_tolerant(tmp.path())
            .expect("directory discovery succeeds");

        assert_eq!(
            profiles
                .iter()
                .map(|profile| profile.id.as_str())
                .collect::<Vec<_>>(),
            vec!["scout"]
        );
        assert_eq!(issues.len(), 2);
        assert!(
            issues
                .iter()
                .all(|issue| issue.detail.contains("duplicate agent profile id reviewer")),
            "{issues:?}"
        );
    }

    #[test]
    fn profile_identity_loader_fails_closed_for_invalid_id_token() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join(WORKSPACE_AGENT_PROFILE_DIR);
        std::fs::create_dir_all(&agents_dir).unwrap();
        write_profile(&agents_dir, "broken.toml", "id = \"bad id\"\n");

        let err = load_workspace_agent_profile_identities(tmp.path())
            .expect_err("invalid identity tokens cannot prove collision safety")
            .to_string();

        assert!(err.contains("broken.toml"), "unexpected error: {err}");
        assert!(err.contains("simple token"), "unexpected error: {err}");
    }

    #[test]
    fn scout_save_succeeds_beside_untouched_legacy_reviewer() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join(WORKSPACE_AGENT_PROFILE_DIR);
        std::fs::create_dir_all(&agents_dir).unwrap();
        let legacy = r#"
id = "reviewer"
role_hint = "reviewer"
model_class_hint = "heavy"
models = ["glm-5.2", "deepseek-v4-pro"]
"#;
        let reviewer_path = write_profile(&agents_dir, "reviewer.toml", legacy);
        let before = std::fs::read_to_string(&reviewer_path).unwrap();

        let identities = load_workspace_agent_profile_identities(tmp.path())
            .expect("legacy neighbor must not block identity discovery");
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].id, "reviewer");
        assert!(
            identities
                .iter()
                .all(|identity| !identity.id.eq_ignore_ascii_case("scout")),
            "scout id must be free beside legacy reviewer"
        );

        let draft = FleetProfileDraft {
            id: "scout".to_string(),
            display_name: Some("Scout".to_string()),
            description: Some("Workspace scout.".to_string()),
            role_hint: "scout".to_string(),
            model_class_hint: None,
            model: Some("deepseek-v4-flash".to_string()),
            provider: Some("deepseek".to_string()),
            reasoning_effort: None,
            instructions: None,
        };
        let scout_path = write_profile(&agents_dir, &draft.file_name(), &draft.render_toml());

        let after = std::fs::read_to_string(&reviewer_path).unwrap();
        assert_eq!(before, after, "legacy reviewer must remain unmodified");
        assert!(scout_path.exists());

        let (profiles, issues) = load_workspace_agent_profiles_tolerant(tmp.path())
            .expect("directory discovery succeeds");
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, "scout");
        assert_eq!(
            profiles[0].profile.model.as_deref(),
            Some("deepseek-v4-flash")
        );
        assert_eq!(issues.len(), 1);
        assert!(issues[0].detail.contains("reviewer.toml"), "{issues:?}");
    }

    #[test]
    fn agent_profile_loader_normalizes_project_agent_toml() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join(WORKSPACE_AGENT_PROFILE_DIR);
        std::fs::create_dir_all(&agents_dir).unwrap();
        let source = write_profile(
            &agents_dir,
            "reviewer.toml",
            r#"
name = "adversarial_reviewer"
display_name = "Adversarial Reviewer"
description = "Skeptical read-only review posture"
role_hint = "reviewer"
loadout = "balanced"
model = "deepseek-v4-pro"

[instructions]
text = "Focus on regressions, missing tests, and fragile assumptions."

[tools]
posture = "read-only"
"#,
        );

        let profiles = load_workspace_agent_profiles(tmp.path()).unwrap();

        assert_eq!(profiles.len(), 1);
        let profile = &profiles[0];
        assert_eq!(profile.id, "adversarial_reviewer");
        assert_eq!(
            profile.display_name.as_deref(),
            Some("Adversarial Reviewer")
        );
        assert_eq!(
            profile.description.as_deref(),
            Some("Skeptical read-only review posture")
        );
        assert_eq!(profile.profile.slot, FleetSlot::Reviewer);
        assert_eq!(profile.profile.role.name, "reviewer");
        assert_eq!(
            profile.profile.role.instructions.as_deref(),
            Some("Focus on regressions, missing tests, and fragile assumptions.")
        );
        assert_eq!(
            profile.profile.loadout,
            FleetLoadout::Custom("balanced".to_string())
        );
        assert_eq!(profile.profile.model.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(
            profile.profile.permissions,
            FleetProfilePermissions::default()
        );
        assert_eq!(profile.source, source);
    }

    #[test]
    fn agent_profile_loader_rejects_retired_model_policy_aliases() {
        for (field, value) in [("model_class_hint", "balanced"), ("route_tier", "fast")] {
            let tmp = TempDir::new().unwrap();
            write_profile(
                tmp.path(),
                "reviewer.toml",
                &format!(
                    r#"
name = "reviewer"
role_hint = "reviewer"
{field} = "{value}"
"#
                ),
            );

            let err = load_agent_profiles_from_dir(tmp.path())
                .unwrap_err()
                .to_string();

            assert!(
                err.contains(field) || err.contains("unknown field"),
                "unexpected error for {field}: {err}"
            );
        }
    }

    #[test]
    fn agent_profile_loader_accepts_and_round_trips_explicit_provider_field() {
        // #4093: `provider` is now a first-class, validated field — a Fleet
        // profile can name its own route explicitly, independent of whatever
        // provider is active when the profile is later loaded/launched.
        let tmp = TempDir::new().unwrap();
        write_profile(
            tmp.path(),
            "reviewer.toml",
            r#"
name = "reviewer"
provider = "openrouter"
model = "deepseek/deepseek-v4-pro"
"#,
        );

        let profiles = load_agent_profiles_from_dir(tmp.path()).expect("profile loads");
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].profile.provider.as_deref(), Some("openrouter"));
        assert_eq!(
            profiles[0].profile.model.as_deref(),
            Some("deepseek/deepseek-v4-pro")
        );
    }

    #[test]
    fn agent_profile_loader_accepts_custom_provider_name() {
        // #3965: LM Studio and other user-named OpenAI-compatible providers
        // are resolved from `[providers.<id>]` at launch time, so the profile
        // loader must preserve the safe id instead of requiring a built-in.
        let tmp = TempDir::new().unwrap();
        write_profile(
            tmp.path(),
            "reviewer.toml",
            r#"
name = "reviewer"
provider = "lm-studio"
model = "qwen-2.5-7b"
"#,
        );

        let profiles = load_agent_profiles_from_dir(tmp.path()).expect("profile loads");

        assert_eq!(profiles[0].profile.provider.as_deref(), Some("lm-studio"));
        assert_eq!(profiles[0].profile.model.as_deref(), Some("qwen-2.5-7b"));
    }

    #[test]
    fn agent_profile_loader_rejects_malformed_provider_name() {
        let tmp = TempDir::new().unwrap();
        write_profile(
            tmp.path(),
            "reviewer.toml",
            r#"
name = "reviewer"
provider = "lm studio"
model = "some-model"
"#,
        );

        let err = load_agent_profiles_from_dir(tmp.path())
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("provider must be a simple provider id"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn agent_profile_loader_rejects_permission_expansion() {
        let tmp = TempDir::new().unwrap();
        write_profile(
            tmp.path(),
            "builder.toml",
            r#"
name = "builder"

[tools]
posture = "read-write"
"#,
        );

        let err = load_agent_profiles_from_dir(tmp.path())
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("would widen permissions"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn agent_profile_loader_rejects_secret_like_model_hint() {
        let tmp = TempDir::new().unwrap();
        write_profile(
            tmp.path(),
            "reviewer.toml",
            r#"
name = "reviewer"
model = "deepseek-v4-pro api_key=secret"
"#,
        );

        let err = load_agent_profiles_from_dir(tmp.path())
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("model must be a visible model id"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn agent_profile_loader_rejects_duplicate_ids() {
        let tmp = TempDir::new().unwrap();
        write_profile(tmp.path(), "a.toml", "name = \"reviewer\"\n");
        write_profile(tmp.path(), "b.toml", "id = \"reviewer\"\n");

        let err = load_agent_profiles_from_dir(tmp.path())
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("duplicate agent profile id reviewer"),
            "unexpected error: {err}"
        );
    }
}
