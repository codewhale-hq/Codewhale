use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// On-disk schema for the `[fleet]` table (#3165). See `config.example.toml`
/// and `docs/FLEET.md` for documentation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FleetConfigToml {
    /// User-defined and built-in role presets.
    ///
    /// Each role defines default tool profiles, capabilities, and execution
    /// requests that task specs can reference by name. Built-in roles
    /// (`smoke-runner`, `reviewer`, `builder`, `read-only`) are always
    /// available; user-defined roles in config override or extend them.
    #[serde(default)]
    pub roles: BTreeMap<String, FleetRolePreset>,
    /// Fleet profile vocabulary (#3167). Profiles group role semantics,
    /// loadout hints, route identity, and delegation bounds. Runtime authority
    /// is intentionally not a profile property.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, FleetProfile>,
    /// Headless worker execution hardening (#3027).
    #[serde(default)]
    pub exec: FleetExecConfig,
}

/// Largest representable absolute depth; admission rejects arithmetic overflow.
pub const MAX_SPAWN_DEPTH_CEILING: u32 = u32::MAX;

/// Headless worker execution constraints (#3027).
///
/// These limits apply to all fleet workers and sub-agents spawned through
/// the headless worker runtime. Task specs can tighten but not loosen them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetExecConfig {
    /// Tools that are always allowed regardless of role or task spec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    /// Tools that are always disallowed, overriding role and task spec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disallowed_tools: Vec<String>,
    /// Optional hard ceiling on sub-agent steps (tool calls + model turns).
    /// Zero keeps the normal agent loop unbounded; a positive value terminates
    /// workers that exceed the explicit operator cap.
    #[serde(default = "default_fleet_max_turns")]
    pub max_turns: u32,
    /// Recursive child-agent budget for headless fleet workers.
    /// Defaults to [`codewhale_protocol::fleet::DEFAULT_SPAWN_DEPTH`], with no product depth cap.
    /// Set zero to block child delegation while preserving the root worker.
    #[serde(default = "default_fleet_max_spawn_depth")]
    pub max_spawn_depth: u32,
    /// Extra system prompt text appended to every headless worker.
    /// Useful for injecting org-wide policy or behavior constraints.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub append_system_prompt: String,
    /// Output format for fleet worker results.
    /// `"text"` (default) or `"stream-json"` for newline-delimited JSON events.
    #[serde(default = "default_fleet_output_format")]
    pub output_format: String,
}

/// Fleet workers run until the model finishes unless an operator supplies a
/// positive `max_turns` value. Individual task budgets may still opt into a
/// narrower explicit model-turn cap through `budget.max_steps`; tool-call
/// admission is enforced independently through `budget.max_tool_calls`.
pub const FLEET_DEFAULT_MAX_TURNS: u32 = 0;

pub(crate) fn default_fleet_max_turns() -> u32 {
    FLEET_DEFAULT_MAX_TURNS
}

pub(crate) fn default_fleet_max_spawn_depth() -> u32 {
    codewhale_protocol::fleet::DEFAULT_SPAWN_DEPTH
}

pub(crate) fn default_fleet_output_format() -> String {
    "text".to_string()
}

impl Default for FleetExecConfig {
    fn default() -> Self {
        Self {
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            max_turns: default_fleet_max_turns(),
            max_spawn_depth: default_fleet_max_spawn_depth(),
            append_system_prompt: String::new(),
            output_format: default_fleet_output_format(),
        }
    }
}

/// Fleet org-chart profile.
///
/// A profile is an additive config record for future fleet scheduling policy.
/// Loading one must not grant runtime permissions by itself: shell and trust
/// escalation default off, and approvals default on.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct FleetProfile {
    /// Org-chart slot this profile describes.
    #[serde(default)]
    pub slot: FleetSlot,
    /// Semantic role name and optional instruction overlay.
    #[serde(default)]
    pub role: FleetRole,
    /// Model class / route-role hint. This is data only in this slice.
    #[serde(default)]
    pub loadout: FleetLoadout,
    /// Optional explicit model id for this profile on the active/resolved route.
    ///
    /// This is not an auth or endpoint selector. Provider-scoped routing still
    /// validates the executable provider/model/wire-model decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional explicit provider id for this profile's model (#4093).
    ///
    /// Present only when the profile was created against a specific,
    /// credential-checked provider (e.g. via the Fleet setup model picker),
    /// so a worker can be pinned to a route independent of the parent/current
    /// session provider. `None` means "no route pin" (inherit), matching
    /// `model: None`; a profile must never carry `provider` without `model`.
    ///
    /// EPIC #2608 explicit-config-only mandate: this field is the ONLY
    /// authority for the profile's provider. It is never inferred by sniffing
    /// a substring/prefix out of `model` — callers that need the provider for
    /// this profile must read this field, not guess from the model id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Optional explicit reasoning/thinking tier for this profile (#4137).
    ///
    /// This is a safe, non-secret route tuning value. `None` means inherit the
    /// operator/session reasoning tier. Concrete values are normalized by the
    /// TUI loader before they are used at runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Legacy ignored input retained for old Fleet-profile files. Runtime
    /// authority is derived after identity selection and is never persisted in
    /// this profile.
    #[doc(hidden)]
    #[serde(default, skip_serializing)]
    pub permissions: FleetProfilePermissions,
    /// Delegation hints for future manager policy.
    #[serde(default)]
    pub delegation: FleetDelegationHints,
}

/// Semantic role declaration for a fleet profile.
///
/// TOML may use either `role = "reviewer"` or a role table with `name` and
/// `instructions`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FleetRole {
    /// Stable role name, e.g. `scout`, `implementer`, or `verifier`.
    pub name: String,
    /// Optional short description for config UIs and docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional instruction overlay to apply when the role is later consumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

impl Default for FleetRole {
    fn default() -> Self {
        Self {
            name: "general".to_string(),
            description: None,
            instructions: None,
        }
    }
}

impl<'de> Deserialize<'de> for FleetRole {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum FleetRoleWire {
            Name(String),
            Full {
                #[serde(default)]
                name: Option<String>,
                #[serde(default)]
                description: Option<String>,
                #[serde(default)]
                instructions: Option<String>,
            },
        }

        match FleetRoleWire::deserialize(deserializer)? {
            FleetRoleWire::Name(name) => Ok(Self {
                name,
                ..Self::default()
            }),
            FleetRoleWire::Full {
                name,
                description,
                instructions,
            } => Ok(Self {
                name: name.unwrap_or_else(|| Self::default().name),
                description,
                instructions,
            }),
        }
    }
}

/// Org-chart slot for grouping fleet profiles.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FleetSlot {
    Manager,
    Scout,
    Planner,
    Implementer,
    Reviewer,
    Verifier,
    Operator,
    Summarizer,
    #[default]
    General,
    Custom(String),
}

impl FleetSlot {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Manager => "manager",
            Self::Scout => "scout",
            Self::Planner => "planner",
            Self::Implementer => "implementer",
            Self::Reviewer => "reviewer",
            Self::Verifier => "verifier",
            Self::Operator => "operator",
            Self::Summarizer => "summarizer",
            Self::General => "general",
            Self::Custom(value) => value.as_str(),
        }
    }

    #[must_use]
    pub fn from_name(value: &str) -> Self {
        match value.trim() {
            "manager" | "coordinator" => Self::Manager,
            "scout" | "research" | "research-worker" => Self::Scout,
            "planner" | "plan" | "awaiter" => Self::Planner,
            "implementer" | "builder" => Self::Implementer,
            "reviewer" => Self::Reviewer,
            "verifier" | "tester" => Self::Verifier,
            "operator" | "incident" | "incident-worker" => Self::Operator,
            "summarizer" | "reducer" => Self::Summarizer,
            "general" | "" => Self::General,
            // Removed slots (e.g. the old "tool-heavy") and unknown names parse
            // as Custom. Note the runtime side no longer treats an undeclared
            // role as write-capable: it fails closed to the read-only `explore`
            // posture (#5575), so this is narrower than the removed variants.
            other => Self::Custom(other.to_string()),
        }
    }
}

impl Serialize for FleetSlot {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for FleetSlot {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_name(&value))
    }
}

/// Model class or route-role hint for a profile.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FleetLoadout {
    /// Reuse the active session route (the operator's model). Default.
    #[default]
    Inherit,
    /// Route to the provider's faster/cheaper model class for wide fan-out.
    Fast,
    /// Unrecognized loadout names parse here (including the retired
    /// strong/balanced/deep-reasoning/code/review/tool-heavy tiers, which
    /// never routed differently). Treated as auto routing.
    Custom(String),
}

impl FleetLoadout {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Inherit => "inherit",
            Self::Fast => "fast",
            Self::Custom(value) => value.as_str(),
        }
    }

    #[must_use]
    pub fn from_name(value: &str) -> Self {
        match value.trim() {
            "inherit" | "default" | "auto" | "" => Self::Inherit,
            "fast" => Self::Fast,
            // Retired tiers (strong/balanced/deep-reasoning/code/review/
            // tool-heavy) and unknown names parse as Custom → auto routing,
            // exactly what those tiers resolved to before removal.
            other => Self::Custom(other.to_string()),
        }
    }
}

impl Serialize for FleetLoadout {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for FleetLoadout {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_name(&value))
    }
}

/// Legacy Fleet-profile permission payload retained only for source and input
/// compatibility. Runtime ignores it; Fleet identity cannot grant authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetProfilePermissions {
    #[doc(hidden)]
    #[serde(default, skip_serializing)]
    pub allow_shell: bool,
    #[doc(hidden)]
    #[serde(default, skip_serializing)]
    pub trust: bool,
    #[doc(hidden)]
    #[serde(default = "default_fleet_profile_approval_required", skip_serializing)]
    pub approval_required: bool,
}

pub(crate) fn default_fleet_profile_approval_required() -> bool {
    true
}

impl Default for FleetProfilePermissions {
    fn default() -> Self {
        Self {
            allow_shell: false,
            trust: false,
            approval_required: true,
        }
    }
}

/// Delegation hints for future fleet manager scheduling.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct FleetDelegationHints {
    /// Optional profile-level child spawn depth. `None` means inherit existing
    /// fleet/sub-agent config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_spawn_depth: Option<u32>,
    /// Optional profile-level worker concurrency hint.
    #[serde(
        default,
        alias = "concurrency",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_concurrency: Option<usize>,
}

/// A named role preset that bundles common worker settings.
///
/// Task specs reference a role name (e.g. `"role": "reviewer"`), and the
/// fleet manager fills in any missing fields from the preset. User-defined
/// roles in `[fleet.roles]` override built-in defaults with the same name.
///
/// Token budgets and tool-call limits are task-level decisions — they don't
/// belong on role presets. Use `timeout_seconds` as the safety bound.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetRolePreset {
    /// Short description of what this role is for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Default tool profile (`"read-only"`, `"read-write"`, or `"custom"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_profile: Option<String>,
    /// Default set of tool names available to this role.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// Default capability tags (e.g. `"rust"`, `"git"`, `"gh"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Default timeout in seconds for tasks using this role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

impl FleetConfigToml {
    /// Resolve a role preset by name. Checks user-defined roles first,
    /// then falls back to built-in role defaults.
    #[must_use]
    pub fn resolve_role(&self, name: &str) -> Option<FleetRolePreset> {
        self.roles
            .get(name)
            .cloned()
            .or_else(|| built_in_role_presets().get(name).cloned())
    }
}

/// Built-in role presets that are always available without config.
#[must_use]
pub fn built_in_role_presets() -> BTreeMap<String, FleetRolePreset> {
    [
        (
            "smoke-runner".to_string(),
            FleetRolePreset {
                description: Some("Lightweight read-only smoke check worker".to_string()),
                tool_profile: Some("read-only".to_string()),
                tools: vec![],
                capabilities: vec![],
                timeout_seconds: Some(300),
            },
        ),
        (
            "reviewer".to_string(),
            FleetRolePreset {
                description: Some("Read-only code and documentation review".to_string()),
                tool_profile: Some("read-only".to_string()),
                tools: vec![],
                capabilities: vec![],
                timeout_seconds: Some(600),
            },
        ),
        (
            "builder".to_string(),
            FleetRolePreset {
                description: Some(
                    "Read-write builder with compilation and test access".to_string(),
                ),
                tool_profile: Some("read-write".to_string()),
                tools: vec![],
                capabilities: vec![],
                timeout_seconds: Some(1800),
            },
        ),
        (
            "read-only".to_string(),
            FleetRolePreset {
                description: Some(
                    "Minimal read-only observer with no writes or secrets".to_string(),
                ),
                tool_profile: Some("read-only".to_string()),
                tools: vec![],
                capabilities: vec![],
                timeout_seconds: Some(300),
            },
        ),
    ]
    .into()
}
