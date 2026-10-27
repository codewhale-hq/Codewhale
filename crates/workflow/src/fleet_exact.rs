//! Exact named-Fleet schema — fully resolved members, no late model choice.
//!
//! A named Fleet is a saved, reusable team. Two forms of `fleets/<name>.toml`
//! exist and both keep working:
//!
//! - **Legacy** (`[roles]` role → AgentProfile id). See [`crate::NamedFleet`].
//!   Legacy files declare no `schema` key, which is what makes the legacy form
//!   *explicitly detectable* rather than inferred from a missing table.
//! - **Exact** (`schema = "exact"`). Every member owns a stable member id/role,
//!   an exact configured provider id, an exact model id, a requested reasoning
//!   policy. Authority is deliberately absent: Runtime derives the effective
//!   child posture from the selected member's Runtime role and the live parent.
//!
//! The exact form deliberately has **no** late-binding selectors. `inherit`,
//! `faster`/fast siblings, model-strength classes, and `model = "auto"` are
//! rejected at parse time, not silently resolved later — a Fleet the operator
//! saved is the Fleet that runs. Users switch Fleets; models never switch
//! themselves.
//!
//! The **Adaptive Reasoning Router is not a Fleet member.** A Fleet says *who*
//! runs; a Router is a separate, optional, reusable service that decides only
//! *how hard an already frozen route thinks*. An exact Fleet points at one by
//! name — `reasoning_router = "luna-low"` — and the same saved profile may be
//! referenced by any number of Fleets. See [`crate::reasoning_router`].
//!
//! The prototype form (`[[members]]` with `kind = "router"`) still parses, is
//! labelled **legacy inline**, and normalizes into the same captured service.
//! It is retained for compatibility only; it is not a second runtime concept.

use std::collections::BTreeMap;

use codewhale_protocol::fleet::PermissionCeiling;
#[cfg(test)]
use codewhale_protocol::fleet::ShellCeiling;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::reasoning_router::{FleetRouterRef, ReasoningRouterError, RouterCallReasoning};

/// Wire value of the `schema` key that selects the exact form.
pub const EXACT_FLEET_SCHEMA_KIND: &str = "exact";
/// Wire value recorded for files in the pre-exact role→profile form.
pub const LEGACY_FLEET_SCHEMA_KIND: &str = "legacy";
/// Current revision of the exact schema.
pub const EXACT_FLEET_SCHEMA_REVISION: u32 = 1;

/// Member kind that selects the Fleet Router.
pub const ROUTER_MEMBER_KIND: &str = "router";
/// Member kind for an ordinary dispatchable worker.
pub const WORKER_MEMBER_KIND: &str = "worker";

/// The Router's public id. A Router is addressed by this literal everywhere a
/// receipt, decision, or error names it, whatever the file called the member.
/// No worker may claim it — see [`ExactFleetError::ReservedRouterIdentity`].
pub const ROUTER_PUBLIC_ID: &str = "router";
/// The Router's public role. Identical to [`ROUTER_PUBLIC_ID`]: a Router has
/// exactly one identity and it is not a dispatchable role.
pub const ROUTER_PUBLIC_ROLE: &str = "router";

/// Public role names that were renamed, and what they are now called.
///
/// A saved Fleet, a gate, a handoff record, and a task option are four
/// different places the *same* role name is written down, and they are written
/// at different times: a Fleet file saved a year ago says `oracle`, a workflow
/// script written today says `consultant`. Canonicalizing in only one of those
/// places is what turns a rename into a lookup failure, so every boundary that
/// compares a role goes through [`canonical_role_key`].
///
/// New schemas and receipts always record the canonical name — the alias is an
/// input spelling, never an output one.
pub const ROLE_ALIASES: &[(&str, &str)] = &[("oracle", "consultant"), ("advisor", "consultant")];

/// The canonical, case-folded key a role compares under.
///
/// Trims, lowercases, and resolves a renamed public role to its current name.
/// This is the *only* way roles are compared anywhere in the exact-Fleet path:
/// parse writes the canonical name into the roster, `validate` detects
/// duplicates under it, and every lookup resolves the caller's spelling through
/// it. A member saved as `oracle` and a task naming `consultant` therefore meet,
/// and so do the reverse.
#[must_use]
pub fn canonical_role_key(value: &str) -> String {
    let key = value.trim().to_ascii_lowercase();
    ROLE_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map_or(key, |(_, canonical)| (*canonical).to_string())
}

/// The case-folded key a member **id** compares under.
///
/// Ids are identities, not names, so they get no alias table — but they do get
/// case folding, because `ExactFleet` is `Deserialize` and a roster can reach a
/// lookup without having passed the parser that lowercased it.
#[must_use]
pub fn canonical_member_key(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

/// Selector tokens that are legal elsewhere in CodeWhale but are exactly what
/// the exact schema exists to forbid. Rejecting them by value (in addition to
/// `deny_unknown_fields` rejecting `model_strength`/`loadout`/`model_class` as
/// keys) is what keeps "exact" honest.
const FORBIDDEN_ROUTE_SELECTORS: &[&str] = &[
    "auto", "inherit", "parent", "same", "faster", "fast", "cheap", "strong", "balanced", "default",
];

/// A concrete reasoning tier. Unlike [`RequestedReasoning`] this has no `auto`
/// — it is what a request actually runs at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningTier {
    Off,
    Low,
    Medium,
    High,
    Max,
}

impl ReasoningTier {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    /// Parse a concrete tier. `auto` is intentionally NOT accepted here.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "disabled" => Some(Self::Off),
            "low" | "minimal" => Some(Self::Low),
            "medium" | "mid" => Some(Self::Medium),
            "high" => Some(Self::High),
            "max" | "maximum" | "xhigh" => Some(Self::Max),
            _ => None,
        }
    }
}

/// The reasoning policy a member *requests*. `Auto` is an explicit per-member
/// opt-in, never a global mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestedReasoning {
    Off,
    Low,
    Medium,
    High,
    Max,
    Auto,
}

impl RequestedReasoning {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
            Self::Auto => "auto",
        }
    }

    #[must_use]
    pub const fn is_auto(self) -> bool {
        matches!(self, Self::Auto)
    }

    /// The concrete tier this request names, or `None` for `auto`.
    #[must_use]
    pub const fn tier(self) -> Option<ReasoningTier> {
        match self {
            Self::Off => Some(ReasoningTier::Off),
            Self::Low => Some(ReasoningTier::Low),
            Self::Medium => Some(ReasoningTier::Medium),
            Self::High => Some(ReasoningTier::High),
            Self::Max => Some(ReasoningTier::Max),
            Self::Auto => None,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        if matches!(value.trim().to_ascii_lowercase().as_str(), "auto") {
            return Some(Self::Auto);
        }
        ReasoningTier::parse(value).map(|tier| match tier {
            ReasoningTier::Off => Self::Off,
            ReasoningTier::Low => Self::Low,
            ReasoningTier::Medium => Self::Medium,
            ReasoningTier::High => Self::High,
            ReasoningTier::Max => Self::Max,
        })
    }
}

/// The exact provider/model a member is frozen to before any reasoning
/// resolution runs. Nothing downstream may change these two strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenRoute {
    pub provider: String,
    pub model: String,
}

/// A dispatchable exact Fleet member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactMember {
    /// Stable member id — the identity a run refers to.
    pub id: String,
    /// Role name; defaults to the member id.
    pub role: String,
    /// Exact configured provider id (a `[providers.<id>]` key or a built-in id).
    pub provider: String,
    /// Exact model id.
    pub model: String,
    /// Requested reasoning policy for this member.
    pub reasoning: RequestedReasoning,
}

impl ExactMember {
    /// The provider/model pair, frozen. Callers resolve reasoning *after* this.
    #[must_use]
    pub fn frozen_route(&self) -> FrozenRoute {
        FrozenRoute {
            provider: self.provider.clone(),
            model: self.model.clone(),
        }
    }

    #[must_use]
    pub const fn is_dispatchable(&self) -> bool {
        true
    }
}

/// The **legacy inline** Router form: a `[[members]]` entry with
/// `kind = "router"`.
///
/// Retained for compatibility with Fleet files written against the prototype.
/// It is normalized into [`crate::reasoning_router::CapturedReasoningRouter`]
/// at capture, so nothing downstream sees two kinds of Router. New Fleets
/// should use `reasoning_router = "<name>"` and a saved profile, which is what
/// lets several Fleets share one Router configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterMember {
    pub id: String,
    pub provider: String,
    pub model: String,
    /// What the Router's own call runs at — `off` or `low` only, exactly as for
    /// a saved profile. Defaults to `off`; `auto` is rejected (a router cannot
    /// ask a router what to think) and `medium`/`high`/`max` are rejected
    /// rather than clamped.
    #[serde(default, alias = "reasoning")]
    pub call_reasoning: RouterCallReasoning,
}

impl RouterMember {
    /// The Router's public id — always the literal `router`, regardless of the
    /// member id the file used. Receipts and errors name this, so a Fleet
    /// cannot disguise its Router behind a friendly label.
    #[must_use]
    pub const fn public_id(&self) -> &'static str {
        ROUTER_PUBLIC_ID
    }

    /// The Router's public role — always the literal `router`.
    #[must_use]
    pub const fn public_role(&self) -> &'static str {
        ROUTER_PUBLIC_ROLE
    }

    /// A Router is never a worker. This is a constant, not a policy lookup.
    #[must_use]
    pub const fn is_dispatchable(&self) -> bool {
        false
    }

    /// The Router's tool surface is empty, always.
    #[must_use]
    pub const fn tool_surface(&self) -> &'static [&'static str] {
        &[]
    }

    /// The Router's fixed permission ceiling.
    #[must_use]
    pub const fn permissions(&self) -> PermissionCeiling {
        PermissionCeiling::ROUTER
    }

    #[must_use]
    pub fn frozen_route(&self) -> FrozenRoute {
        FrozenRoute {
            provider: self.provider.clone(),
            model: self.model.clone(),
        }
    }
}

/// A parsed exact Fleet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactFleet {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub schema_revision: u32,
    pub members: Vec<ExactMember>,
    /// Name of the saved Reasoning Router profile this Fleet references. The
    /// profile is a separate, reusable service — several Fleets may name the
    /// same one. Accepts a qualified `origin/name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_router: Option<String>,
    /// The legacy inline Router, if the file used the prototype form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router: Option<RouterMember>,
}

impl ExactFleet {
    /// Look up a dispatchable member by its **member id only**.
    ///
    /// Roles are semantic labels used by gates, handoffs, and records; ids are
    /// what addresses a roster entry. Keeping the two lookups separate is what
    /// lets a task carry a meaningful role (`builder`) while the runtime
    /// resolves a distinct profile id (`implementer`) — see
    /// [`Self::member_by_role`].
    #[must_use]
    pub fn member(&self, id: &str) -> Option<&ExactMember> {
        let key = canonical_member_key(id);
        self.members
            .iter()
            .find(|member| canonical_member_key(&member.id) == key)
    }

    /// Look up a dispatchable member by its semantic role.
    ///
    /// Both sides resolve through [`canonical_role_key`], so a member saved
    /// under a renamed role (`oracle`) is found by a task, gate, or handoff that
    /// names either spelling.
    #[must_use]
    pub fn member_by_role(&self, role: &str) -> Option<&ExactMember> {
        let key = canonical_role_key(role);
        self.members
            .iter()
            .find(|member| canonical_role_key(&member.role) == key)
    }

    /// Look up a member by id first, then by role. Roster invariants forbid an
    /// id/role collision, so this can never be order-dependent.
    #[must_use]
    pub fn member_by_id_or_role(&self, id_or_role: &str) -> Option<&ExactMember> {
        self.member(id_or_role)
            .or_else(|| self.member_by_role(id_or_role))
    }

    /// How this Fleet points at its Router, if it does at all.
    #[must_use]
    pub fn router_ref(&self) -> Option<FleetRouterRef> {
        if let Some(name) = &self.reasoning_router {
            return Some(FleetRouterRef::Profile { name: name.clone() });
        }
        self.router
            .as_ref()
            .map(|member| FleetRouterRef::LegacyInline(Box::new(member.clone())))
    }

    /// The legacy inline Router member, if the file used the prototype form.
    #[must_use]
    pub fn legacy_inline_router(&self) -> Option<&RouterMember> {
        self.router.as_ref()
    }

    /// Whether any member explicitly requested `reasoning = "auto"`.
    #[must_use]
    pub fn has_auto_member(&self) -> bool {
        self.members.iter().any(|member| member.reasoning.is_auto())
    }

    /// Re-check every roster invariant that [`Self::parse`] enforces.
    ///
    /// `ExactFleet` is `pub` and `Deserialize`, so a value can reach a snapshot
    /// without ever passing through the TOML parser. Capture calls this so a
    /// hand-built or round-tripped roster cannot smuggle in a duplicate role, an
    /// id/role collision, or a worker claiming the Router's identity.
    pub fn validate(&self) -> Result<(), ExactFleetError> {
        if self.members.is_empty() {
            return Err(ExactFleetError::NoMembers {
                fleet: self.name.clone(),
            });
        }

        let mut ids: BTreeMap<String, ()> = BTreeMap::new();
        let mut roles: BTreeMap<String, ()> = BTreeMap::new();

        for member in &self.members {
            let id = canonical_member_key(&member.id);
            // Duplicate detection runs on the canonical role key, so a roster
            // carrying both `oracle` and `consultant` is caught as the collision
            // it is rather than resolving by list order at lookup time.
            let role = canonical_role_key(&member.role);
            if id.is_empty() {
                return Err(ExactFleetError::InvalidToken {
                    field: "member id".to_string(),
                    value: member.id.clone(),
                });
            }
            if role.is_empty() {
                return Err(ExactFleetError::InvalidToken {
                    field: "member role".to_string(),
                    value: member.role.clone(),
                });
            }
            // A worker may never be called `router`, by id or by role: the
            // Router's public identity is that literal, and a worker wearing it
            // would make a receipt ambiguous about who decided the reasoning.
            for (field, value) in [("id", &id), ("role", &role)] {
                if value.as_str() == ROUTER_PUBLIC_ID {
                    return Err(ExactFleetError::ReservedRouterIdentity {
                        id: member.id.clone(),
                        field: field.to_string(),
                    });
                }
            }
            if ids.insert(id.clone(), ()).is_some() {
                return Err(ExactFleetError::DuplicateMember { id });
            }
            if roles.insert(role.clone(), ()).is_some() {
                return Err(ExactFleetError::DuplicateRole { role });
            }
        }

        // An id belonging to one member and a role belonging to a *different*
        // member would make `member()` lookup order-dependent, so it is a
        // collision even though neither set has an internal duplicate.
        for member in &self.members {
            let id = canonical_member_key(&member.id);
            if let Some(other) = self
                .members
                .iter()
                .find(|other| other.id != member.id && canonical_role_key(&other.role) == id)
            {
                return Err(ExactFleetError::IdRoleCollision {
                    id: member.id.clone(),
                    other: other.id.clone(),
                });
            }
        }

        Ok(())
    }

    /// Parse an exact Fleet from TOML text.
    pub fn parse(text: &str) -> Result<Self, ExactFleetError> {
        let doc: ExactFleetToml =
            toml::from_str(text).map_err(|error| ExactFleetError::Parse(error.to_string()))?;
        Self::from_toml(doc)
    }

    fn from_toml(doc: ExactFleetToml) -> Result<Self, ExactFleetError> {
        if !doc
            .schema
            .trim()
            .eq_ignore_ascii_case(EXACT_FLEET_SCHEMA_KIND)
        {
            return Err(ExactFleetError::UnknownSchema {
                schema: doc.schema.trim().to_string(),
            });
        }
        if doc.schema_revision != EXACT_FLEET_SCHEMA_REVISION {
            return Err(ExactFleetError::UnsupportedRevision {
                revision: doc.schema_revision,
                supported: EXACT_FLEET_SCHEMA_REVISION,
            });
        }
        let name = require_token(&doc.name, "name")?;

        let mut members = Vec::new();
        let mut router: Option<RouterMember> = None;
        let mut seen: BTreeMap<String, ()> = BTreeMap::new();

        for raw in doc.members {
            let id = require_token(&raw.id, "member id")?;
            if seen.insert(id.clone(), ()).is_some() {
                return Err(ExactFleetError::DuplicateMember { id });
            }
            let provider = require_exact_route_token(&raw.provider, &id, "provider")?;
            let model = require_exact_route_token(&raw.model, &id, "model")?;
            let kind = raw
                .kind
                .as_deref()
                .map(str::trim)
                .filter(|kind| !kind.is_empty())
                .unwrap_or(WORKER_MEMBER_KIND)
                .to_ascii_lowercase();

            match kind.as_str() {
                ROUTER_MEMBER_KIND => {
                    if router.is_some() {
                        return Err(ExactFleetError::MultipleRouters);
                    }
                    if raw.role.is_some() {
                        return Err(ExactFleetError::RouterRoleDeclared { id });
                    }
                    let call_reasoning = match raw.reasoning.as_deref() {
                        None => RouterCallReasoning::default(),
                        Some(value) if value.trim().eq_ignore_ascii_case("auto") => {
                            return Err(ExactFleetError::RouterAutoReasoning { id });
                        }
                        // The cheap ceiling is a property of the *service*, not
                        // of how it was written down, so the legacy inline form
                        // gets the identical rejection a saved profile gets.
                        Some(value) => RouterCallReasoning::parse(value, &id)
                            .map_err(|source| ExactFleetError::Router { source })?,
                    };
                    router = Some(RouterMember {
                        id,
                        provider,
                        model,
                        call_reasoning,
                    });
                }
                WORKER_MEMBER_KIND => {
                    let role = match raw.role.as_deref() {
                        Some(role) => require_member_role(role)?,
                        None => id.clone(),
                    };
                    let reasoning = match raw.reasoning.as_deref() {
                        None => RequestedReasoning::Off,
                        Some(value) => RequestedReasoning::parse(value).ok_or_else(|| {
                            ExactFleetError::InvalidReasoning {
                                id: id.clone(),
                                value: value.trim().to_string(),
                            }
                        })?,
                    };
                    members.push(ExactMember {
                        id,
                        role,
                        provider,
                        model,
                        reasoning,
                    });
                }
                other => {
                    return Err(ExactFleetError::UnknownMemberKind {
                        id,
                        kind: other.to_string(),
                    });
                }
            }
        }

        if members.is_empty() {
            return Err(ExactFleetError::NoMembers { fleet: name });
        }

        // One Router per Fleet, named exactly one way. Declaring both forms is
        // an error rather than a precedence rule: a silent winner here would
        // decide which provider sees every routing summary.
        let reasoning_router = match doc.reasoning_router.as_deref().map(str::trim) {
            Some(value) if !value.is_empty() => {
                if router.is_some() {
                    return Err(ExactFleetError::ConflictingRouterDeclarations { fleet: name });
                }
                Some(value.to_string())
            }
            _ => None,
        };

        let fleet = Self {
            name,
            description: doc.description,
            schema_revision: doc.schema_revision,
            members,
            reasoning_router,
            router,
        };
        // One authority for the roster invariants, shared with capture-time
        // revalidation so the two can never drift.
        fleet.validate()?;
        Ok(fleet)
    }
}

/// Peek at a fleet document's `schema` key without committing to a form.
///
/// Returns `None` for legacy files, which declare no `schema` key at all.
/// A malformed document returns `None` too; the legacy parser then owns the
/// error, keeping old files on the old diagnostics.
#[must_use]
pub fn declared_schema_kind(text: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct SchemaProbe {
        #[serde(default)]
        schema: Option<String>,
    }

    let probe: SchemaProbe = toml::from_str(text).ok()?;
    probe
        .schema
        .map(|schema| schema.trim().to_ascii_lowercase())
        .filter(|schema| !schema.is_empty())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactFleetToml {
    name: String,
    #[serde(default)]
    description: Option<String>,
    schema: String,
    #[serde(default = "default_schema_revision")]
    schema_revision: u32,
    /// Reference to a saved Reasoning Router profile. Optional: a Fleet whose
    /// members all pin explicit tiers needs no Router at all.
    #[serde(default)]
    reasoning_router: Option<String>,
    #[serde(default)]
    members: Vec<ExactMemberToml>,
}

/// `deny_unknown_fields` is load-bearing here: it is what rejects
/// `model_strength`, `loadout`, `model_class`, and any other late-binding
/// selector someone tries to smuggle into an exact member.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactMemberToml {
    id: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    role: Option<String>,
    provider: String,
    model: String,
    #[serde(default)]
    reasoning: Option<String>,
    /// Compatibility-only input from the prototype exact-Fleet schema.
    ///
    /// It is intentionally a generic TOML value and intentionally unread:
    /// old files and replays remain loadable, while no spelling can influence
    /// active identity, snapshots, selection, or Runtime authority.
    #[serde(default, rename = "permissions")]
    _legacy_permissions: Option<toml::Value>,
}

const fn default_schema_revision() -> u32 {
    EXACT_FLEET_SCHEMA_REVISION
}

fn require_token(value: &str, field: &str) -> Result<String, ExactFleetError> {
    crate::role_resolve::normalize_token(value).ok_or_else(|| ExactFleetError::InvalidToken {
        field: field.to_string(),
        value: value.trim().to_string(),
    })
}

/// Canonicalize the renamed public roles at the saved-Fleet boundary, so a new
/// schema and every receipt it produces record only the current name.
///
/// Exact Fleets otherwise permit domain-specific semantic roles (for example
/// `auditor`), so this is intentionally not a closed-role parser. The alias
/// table is shared with [`canonical_role_key`], which is what makes an *old*
/// file — parsed before this canonicalization existed, or reaching the roster
/// through `Deserialize` — still resolvable by either spelling at lookup time.
fn require_member_role(value: &str) -> Result<String, ExactFleetError> {
    let role = require_token(value, "member role")?;
    Ok(canonical_role_key(&role))
}

/// Provider/model ids keep their configured casing (a model id is
/// case-sensitive on the wire) but must be non-empty, whitespace-free, and must
/// not be a late-binding selector.
fn require_exact_route_token(
    value: &str,
    member: &str,
    field: &str,
) -> Result<String, ExactFleetError> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\'' | '`' | '='))
    {
        return Err(ExactFleetError::InvalidToken {
            field: format!("{member}.{field}"),
            value: trimmed.to_string(),
        });
    }
    if FORBIDDEN_ROUTE_SELECTORS
        .iter()
        .any(|selector| trimmed.eq_ignore_ascii_case(selector))
    {
        return Err(ExactFleetError::LateBindingSelector {
            id: member.to_string(),
            field: field.to_string(),
            value: trimmed.to_string(),
        });
    }
    Ok(trimmed.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExactFleetError {
    #[error("failed to parse exact fleet file: {0}")]
    Parse(String),
    #[error("unknown fleet schema `{schema}`; expected `exact`")]
    UnknownSchema { schema: String },
    #[error(
        "exact fleet schema revision {revision} is not supported (this build reads {supported})"
    )]
    UnsupportedRevision { revision: u32, supported: u32 },
    #[error("{field} must be a non-empty token without whitespace, quotes, or `=` (got `{value}`)")]
    InvalidToken { field: String, value: String },
    #[error("duplicate fleet member id `{id}`")]
    DuplicateMember { id: String },
    #[error(
        "duplicate fleet member role `{role}`; two members cannot answer to the same role or a \
         task naming it would resolve to whichever one happened to be listed first"
    )]
    DuplicateRole { role: String },
    #[error(
        "member `{id}` collides with member `{other}`: one member's id is another member's role, \
         so a task naming it would resolve by list order rather than by identity"
    )]
    IdRoleCollision { id: String, other: String },
    #[error(
        "member `{id}` claims the reserved {field} `router`; that identity belongs to the fleet \
         router, which is declared with `kind = \"router\"` and is never dispatchable"
    )]
    ReservedRouterIdentity { id: String, field: String },
    #[error(
        "fleet `{fleet}` snapshot content hash does not describe its own contents (recorded \
         `{recorded}`, recomputed `{recomputed}`). The snapshot was edited or migrated after \
         capture, so its hash cannot be used as evidence that a run matched a saved definition. \
         Re-capture the fleet."
    )]
    ContentHashMismatch {
        fleet: String,
        recorded: String,
        recomputed: String,
    },
    #[error("fleet `{fleet}` declares no dispatchable members")]
    NoMembers { fleet: String },
    #[error("member `{id}` has unknown kind `{kind}`; expected `worker` or `router`")]
    UnknownMemberKind { id: String, kind: String },
    #[error("a fleet may declare at most one router member")]
    MultipleRouters,
    #[error(
        "member `{id}`.{field} is `{value}`, but exact fleets forbid late-binding route selectors \
         (inherit, fast siblings, model strength, or model=auto). Name the exact provider/model."
    )]
    LateBindingSelector {
        id: String,
        field: String,
        value: String,
    },
    #[error(
        "member `{id}` has invalid reasoning `{value}`; expected off, low, medium, high, max, or auto"
    )]
    InvalidReasoning { id: String, value: String },
    #[error(
        "router member `{id}` may not declare a role; a router is never dispatched as a worker"
    )]
    RouterRoleDeclared { id: String },
    #[error(
        "router member `{id}` may not request reasoning `auto`; a router's own thinking is a fixed tier (default off)"
    )]
    RouterAutoReasoning { id: String },
    #[error(
        "fleet `{fleet}` declares both `reasoning_router = \"...\"` and an inline \
         `kind = \"router\"` member. A fleet references exactly one reasoning router service; \
         pick the saved profile (preferred, and shareable across fleets) or the legacy inline \
         form, not both."
    )]
    ConflictingRouterDeclarations { fleet: String },
    #[error(transparent)]
    Router {
        #[from]
        source: ReasoningRouterError,
    },
}

#[cfg(test)]
mod role_alias_tests {
    use super::*;

    /// A Fleet saved before the rename. The file spells the advisory role
    /// `oracle`; everything downstream must call it `consultant`.
    const RENAMED_ROLE_FLEET: &str = r#"
name = "counsel"
schema = "exact"

[[members]]
id = "advisor-one"
role = "oracle"
provider = "zai"
model = "glm-5"
permissions = "analyst"
"#;

    /// Parse canonicalizes on the way in, so the roster — and therefore every
    /// receipt built from it — records only the current name.
    #[test]
    fn parsing_a_renamed_role_stores_the_canonical_name() {
        let fleet = ExactFleet::parse(RENAMED_ROLE_FLEET).expect("parse");
        assert_eq!(fleet.members[0].role, "consultant");
    }

    /// The compatibility half: a saved task, gate, or handoff that still spells
    /// the role the old way resolves to the same member. This is the lookup that
    /// used to fail, because parse canonicalized and the lookup did not.
    #[test]
    fn every_alias_spelling_resolves_to_the_same_member() {
        let fleet = ExactFleet::parse(RENAMED_ROLE_FLEET).expect("parse");

        for spelling in ["consultant", "oracle", "advisor", "Oracle", " ADVISOR "] {
            let member = fleet
                .member_by_role(spelling)
                .unwrap_or_else(|| panic!("`{spelling}` must resolve"));
            assert_eq!(member.id, "advisor-one");
            assert_eq!(member.role, "consultant", "receipts stay canonical");
        }

        // `member_by_id_or_role` is what the runtime actually calls.
        assert_eq!(
            fleet
                .member_by_id_or_role("oracle")
                .expect("alias resolves through the combined lookup")
                .id,
            "advisor-one"
        );
    }

    /// A Fleet written against the *new* name keeps working, and is equally
    /// reachable by the old one — the rename is bidirectional at the lookup.
    #[test]
    fn a_canonical_role_is_reachable_by_its_alias() {
        let text = RENAMED_ROLE_FLEET.replace(r#"role = "oracle""#, r#"role = "consultant""#);
        let fleet = ExactFleet::parse(&text).expect("parse");

        assert_eq!(fleet.members[0].role, "consultant");
        assert!(fleet.member_by_role("oracle").is_some());
        assert!(fleet.member_by_role("advisor").is_some());
    }

    /// A roster that reaches `validate` through `Deserialize` — never having
    /// passed the parser — is still judged on canonical keys. `oracle` and
    /// `consultant` are one role, so declaring both is the collision it looks
    /// like, not a pair that resolves by list order.
    #[test]
    fn an_alias_and_its_canonical_name_collide_on_reload() {
        let member = |id: &str, role: &str| ExactMember {
            id: id.to_string(),
            role: role.to_string(),
            provider: "zai".to_string(),
            model: "glm-5".to_string(),
            reasoning: RequestedReasoning::Off,
        };
        let fleet = ExactFleet {
            name: "counsel".to_string(),
            description: None,
            schema_revision: EXACT_FLEET_SCHEMA_REVISION,
            members: vec![member("a", "oracle"), member("b", "consultant")],
            reasoning_router: None,
            router: None,
        };

        assert!(matches!(
            fleet.validate(),
            Err(ExactFleetError::DuplicateRole { role }) if role == "consultant"
        ));
    }

    /// Ids are identities, not names: no alias table, but case folding, because
    /// a deserialized roster never passed the parser that lowercased it.
    #[test]
    fn member_ids_resolve_case_insensitively_without_aliasing() {
        let fleet = ExactFleet {
            name: "counsel".to_string(),
            description: None,
            schema_revision: EXACT_FLEET_SCHEMA_REVISION,
            members: vec![ExactMember {
                id: "Builder".to_string(),
                role: "auditor".to_string(),
                provider: "zai".to_string(),
                model: "glm-5".to_string(),
                reasoning: RequestedReasoning::Off,
            }],
            reasoning_router: None,
            router: None,
        };

        assert!(fleet.member("builder").is_some());
        assert!(fleet.member("Builder").is_some());
        // `oracle` is a role alias, never an id alias.
        assert!(fleet.member("oracle").is_none());
    }

    #[test]
    fn canonical_role_key_maps_only_the_declared_aliases() {
        assert_eq!(canonical_role_key(" Oracle "), "consultant");
        assert_eq!(canonical_role_key("ADVISOR"), "consultant");
        assert_eq!(canonical_role_key("consultant"), "consultant");
        // Unrelated semantic roles pass through untouched, case-folded only.
        assert_eq!(canonical_role_key("Auditor"), "auditor");
        assert_eq!(canonical_role_key("router"), "router");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GLM_FLEET: &str = r#"
name = "glm-pair"
description = "GLM worker with a GLM Turbo router"
schema = "exact"
schema_revision = 1

[[members]]
id = "implementer"
role = "builder"
provider = "zai"
model = "glm-5"
reasoning = "auto"
permissions = "read_write"

[[members]]
id = "router"
kind = "router"
provider = "zai"
model = "glm-5-turbo"
"#;

    /// A Fleet that references a saved, reusable Router service by name — the
    /// form new Fleets use.
    const NAMED_ROUTER_FLEET: &str = r#"
name = "glm-pair"
schema = "exact"
reasoning_router = "luna-low"

[[members]]
id = "implementer"
role = "builder"
provider = "zai"
model = "glm-5"
reasoning = "auto"
"#;

    #[test]
    fn exact_fleet_parses_members_and_a_legacy_inline_router() {
        let fleet = ExactFleet::parse(GLM_FLEET).expect("parse");
        assert_eq!(fleet.name, "glm-pair");
        assert_eq!(fleet.schema_revision, EXACT_FLEET_SCHEMA_REVISION);
        assert_eq!(fleet.members.len(), 1);

        // Id and role are separate lookups: a role is a semantic label, an id
        // addresses a roster entry.
        let member = fleet.member_by_role("builder").expect("role lookup");
        assert_eq!(member.id, "implementer");
        assert_eq!(
            fleet.member("implementer").expect("id lookup").id,
            "implementer"
        );
        assert!(
            fleet.member("builder").is_none(),
            "an id lookup must not answer to a role"
        );
        assert_eq!(member.provider, "zai");
        assert_eq!(member.model, "glm-5");
        assert_eq!(member.reasoning, RequestedReasoning::Auto);
        let serialized = serde_json::to_value(member).expect("serialize member");
        assert!(
            serialized.get("permissions").is_none(),
            "exact member identity must not carry authority: {serialized}"
        );

        let router = fleet.legacy_inline_router().expect("inline router");
        assert_eq!(router.provider, "zai");
        assert_eq!(router.model, "glm-5-turbo");
        // The call tier defaults to off when the file says nothing.
        assert_eq!(router.call_reasoning, RouterCallReasoning::Off);
        assert!(matches!(
            fleet.router_ref(),
            Some(FleetRouterRef::LegacyInline(_))
        ));
    }

    #[test]
    fn legacy_advisory_role_names_canonicalize_to_consultant() {
        for legacy in ["oracle", "advisor"] {
            let text = GLM_FLEET.replace("role = \"builder\"", &format!("role = \"{legacy}\""));
            let fleet = ExactFleet::parse(&text).expect("legacy role parses");
            assert_eq!(fleet.members[0].role, "consultant");
            assert!(fleet.member_by_role("consultant").is_some());
            // The rename resolves in both directions: the roster stores the
            // canonical name, and a caller still spelling the legacy one lands
            // on the same member rather than on nothing.
            assert_eq!(
                fleet
                    .member_by_role(legacy)
                    .map(|member| member.id.as_str()),
                fleet
                    .member_by_role("consultant")
                    .map(|member| member.id.as_str()),
            );
        }
    }

    /// The preferred form: the Router is a *reference* to a saved service, so
    /// several Fleets can point at one configuration.
    #[test]
    fn a_fleet_references_a_named_reasoning_router_service() {
        let fleet = ExactFleet::parse(NAMED_ROUTER_FLEET).expect("parse");

        assert_eq!(fleet.reasoning_router.as_deref(), Some("luna-low"));
        assert!(fleet.legacy_inline_router().is_none());
        assert!(matches!(
            fleet.router_ref(),
            Some(FleetRouterRef::Profile { ref name }) if name == "luna-low"
        ));
        assert!(fleet.has_auto_member());

        // A qualified origin is accepted verbatim; resolution happens in the
        // host that owns the search roots.
        let qualified = NAMED_ROUTER_FLEET.replace("\"luna-low\"", "\"codewhale_home/luna-low\"");
        assert!(matches!(
            ExactFleet::parse(&qualified).expect("parse").router_ref(),
            Some(FleetRouterRef::Profile { ref name }) if name == "codewhale_home/luna-low"
        ));
    }

    /// Both forms at once would make a silent winner decide which provider sees
    /// every routing summary, so it is an error instead.
    #[test]
    fn declaring_both_router_forms_is_rejected() {
        let both = GLM_FLEET.replace(
            "schema_revision = 1",
            "schema_revision = 1\nreasoning_router = \"luna-low\"",
        );
        let err = ExactFleet::parse(&both).expect_err("two router declarations");
        assert!(
            matches!(err, ExactFleetError::ConflictingRouterDeclarations { .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("exactly one"), "{err}");
    }

    /// The cheap call ceiling belongs to the service, not to how it was written
    /// down: the inline form gets the identical rejection a saved profile does.
    #[test]
    fn a_legacy_inline_router_may_not_request_an_expensive_call_tier() {
        for value in ["medium", "high", "max"] {
            let text = format!("{GLM_FLEET}reasoning = \"{value}\"\n");
            let err = ExactFleet::parse(&text).expect_err("expensive router tier");
            assert!(
                matches!(
                    err,
                    ExactFleetError::Router {
                        source: ReasoningRouterError::CallReasoningTooExpensive { .. }
                    }
                ),
                "value={value} err={err:?}"
            );
        }

        let low = format!("{GLM_FLEET}reasoning = \"low\"\n");
        assert_eq!(
            ExactFleet::parse(&low)
                .expect("low is allowed")
                .legacy_inline_router()
                .expect("router")
                .call_reasoning,
            RouterCallReasoning::Low
        );
    }

    #[test]
    fn a_router_is_not_dispatchable_and_holds_no_authority() {
        let fleet = ExactFleet::parse(GLM_FLEET).expect("parse");
        let router = fleet.legacy_inline_router().expect("router");

        assert!(!router.is_dispatchable());
        assert!(router.tool_surface().is_empty());
        let permissions = router.permissions();
        assert!(!permissions.tools);
        assert!(!permissions.write);
        assert!(!permissions.network_tool);
        assert_eq!(permissions.shell, ShellCeiling::None);
        assert_eq!(permissions.delegation_depth, 0);

        // The router is not reachable through worker lookup either.
        assert!(fleet.member_by_id_or_role("router").is_none());
        assert!(fleet.members.iter().all(ExactMember::is_dispatchable));
    }

    #[test]
    fn late_binding_selectors_are_rejected() {
        for (field, value) in [
            ("model", "auto"),
            ("model", "inherit"),
            ("model", "faster"),
            ("model", "strong"),
            ("provider", "inherit"),
        ] {
            let text = format!(
                r#"
name = "f"
schema = "exact"

[[members]]
id = "w"
provider = "{provider}"
model = "{model}"
"#,
                provider = if field == "provider" { value } else { "zai" },
                model = if field == "model" { value } else { "glm-5" },
            );
            let err = ExactFleet::parse(&text).expect_err("selector must be rejected");
            assert!(
                matches!(err, ExactFleetError::LateBindingSelector { .. }),
                "field={field} value={value} err={err:?}"
            );
        }
    }

    #[test]
    fn model_strength_and_loadout_keys_are_rejected() {
        for key in ["model_strength", "loadout", "model_class", "model_hint"] {
            let text = format!(
                r#"
name = "f"
schema = "exact"

[[members]]
id = "w"
provider = "zai"
model = "glm-5"
{key} = "strong"
"#
            );
            let err = ExactFleet::parse(&text).expect_err("unknown key must be rejected");
            assert!(
                matches!(err, ExactFleetError::Parse(_)),
                "key={key} err={err:?}"
            );
        }
    }

    #[test]
    fn legacy_permissions_are_accepted_as_ignored_input_only() {
        let text = r#"
name = "f"
schema = "exact"

[[members]]
id = "w"
role = "builder"
provider = "zai"
model = "glm-5"
permissions = "a-value-no-current-preset-recognizes"
"#;
        let fleet = ExactFleet::parse(text).expect("legacy permissions must remain loadable");
        let member = fleet.member("w").expect("member");
        let encoded = serde_json::to_value(member).expect("serialize current member identity");

        assert!(encoded.get("permissions").is_none(), "{encoded}");
        assert_eq!(member.role, "builder");
        assert_eq!(member.frozen_route().model, "glm-5");
    }

    #[test]
    fn router_ignores_legacy_permissions_but_rejects_role_and_auto_reasoning() {
        let base = r#"
name = "f"
schema = "exact"

[[members]]
id = "w"
provider = "zai"
model = "glm-5"

[[members]]
id = "router"
kind = "router"
provider = "zai"
model = "glm-5-turbo"
"#;
        let permissions = format!("{base}permissions = \"full\"\n");
        let parsed = ExactFleet::parse(&permissions).expect("legacy permissions are ignored");
        assert!(parsed.legacy_inline_router().is_some());

        let role = format!("{base}role = \"builder\"\n");
        assert!(matches!(
            ExactFleet::parse(&role).expect_err("role rejected"),
            ExactFleetError::RouterRoleDeclared { .. }
        ));

        let auto = format!("{base}reasoning = \"auto\"\n");
        assert!(matches!(
            ExactFleet::parse(&auto).expect_err("auto rejected"),
            ExactFleetError::RouterAutoReasoning { .. }
        ));
    }

    #[test]
    fn duplicate_members_and_multiple_routers_fail() {
        let duplicate = r#"
name = "f"
schema = "exact"

[[members]]
id = "w"
provider = "zai"
model = "glm-5"

[[members]]
id = "w"
provider = "zai"
model = "glm-5"
"#;
        assert!(matches!(
            ExactFleet::parse(duplicate).expect_err("duplicate"),
            ExactFleetError::DuplicateMember { .. }
        ));

        let two_routers = r#"
name = "f"
schema = "exact"

[[members]]
id = "w"
provider = "zai"
model = "glm-5"

[[members]]
id = "r1"
kind = "router"
provider = "zai"
model = "glm-5-turbo"

[[members]]
id = "r2"
kind = "router"
provider = "zai"
model = "glm-5-turbo"
"#;
        assert!(matches!(
            ExactFleet::parse(two_routers).expect_err("two routers"),
            ExactFleetError::MultipleRouters
        ));
    }

    /// Two members answering to one role, or one member's id being another's
    /// role, would make `member()` resolve by list order instead of identity.
    #[test]
    fn duplicate_roles_and_id_role_collisions_are_rejected() {
        let duplicate_role = r#"
name = "f"
schema = "exact"

[[members]]
id = "a"
role = "builder"
provider = "zai"
model = "glm-5"

[[members]]
id = "b"
role = "builder"
provider = "zai"
model = "glm-5"
"#;
        assert!(matches!(
            ExactFleet::parse(duplicate_role).expect_err("duplicate role"),
            ExactFleetError::DuplicateRole { .. }
        ));

        // `b`'s role is `a`'s id: naming "a" would be ambiguous.
        let collision = r#"
name = "f"
schema = "exact"

[[members]]
id = "a"
role = "builder"
provider = "zai"
model = "glm-5"

[[members]]
id = "b"
role = "a"
provider = "zai"
model = "glm-5"
"#;
        assert!(matches!(
            ExactFleet::parse(collision).expect_err("id/role collision"),
            ExactFleetError::IdRoleCollision { .. }
        ));
    }

    /// `router` is the Router's public identity. A worker may not wear it by
    /// either id or role.
    #[test]
    fn a_worker_may_not_claim_the_router_identity() {
        for (id, role) in [("router", None), ("helper", Some("router"))] {
            let role_line = role.map_or(String::new(), |role| format!("role = \"{role}\"\n"));
            let text = format!(
                r#"
name = "f"
schema = "exact"

[[members]]
id = "{id}"
{role_line}provider = "zai"
model = "glm-5"
"#
            );
            let err = ExactFleet::parse(&text).expect_err("reserved router identity");
            assert!(
                matches!(err, ExactFleetError::ReservedRouterIdentity { .. }),
                "id={id} role={role:?} err={err:?}"
            );
        }
    }

    /// `ExactFleet` is `pub` and `Deserialize`, so the invariants must be
    /// re-checkable on a value that never went through the TOML parser.
    #[test]
    fn capture_time_revalidation_catches_a_hand_built_roster() {
        let member = |id: &str, role: &str| ExactMember {
            id: id.to_string(),
            role: role.to_string(),
            provider: "zai".to_string(),
            model: "glm-5".to_string(),
            reasoning: RequestedReasoning::Off,
        };

        let valid = ExactFleet {
            name: "f".to_string(),
            description: None,
            schema_revision: EXACT_FLEET_SCHEMA_REVISION,
            members: vec![member("a", "scout"), member("b", "builder")],
            reasoning_router: None,
            router: None,
        };
        valid.validate().expect("a clean roster validates");

        for (fleet, label) in [
            (
                ExactFleet {
                    members: vec![member("a", "scout"), member("a", "builder")],
                    ..valid.clone()
                },
                "duplicate id",
            ),
            (
                ExactFleet {
                    members: vec![member("a", "scout"), member("b", "scout")],
                    ..valid.clone()
                },
                "duplicate role",
            ),
            (
                ExactFleet {
                    members: vec![member("router", "scout")],
                    ..valid.clone()
                },
                "reserved router id",
            ),
            (
                ExactFleet {
                    members: vec![member("a", "scout"), member("b", "a")],
                    ..valid.clone()
                },
                "id/role collision",
            ),
        ] {
            assert!(
                fleet.validate().is_err(),
                "{label} must not survive revalidation"
            );
        }

        // A serde round-trip is exactly how such a value reaches a snapshot.
        let smuggled: ExactFleet = serde_json::from_str(
            &serde_json::to_string(&ExactFleet {
                members: vec![member("a", "scout"), member("b", "scout")],
                ..valid
            })
            .expect("serialize"),
        )
        .expect("deserialize");
        assert!(matches!(
            smuggled
                .validate()
                .expect_err("round-trip must not launder it"),
            ExactFleetError::DuplicateRole { .. }
        ));
    }

    #[test]
    fn a_router_only_fleet_has_no_dispatchable_members() {
        let text = r#"
name = "f"
schema = "exact"

[[members]]
id = "router"
kind = "router"
provider = "zai"
model = "glm-5-turbo"
"#;
        assert!(matches!(
            ExactFleet::parse(text).expect_err("router alone is not a fleet"),
            ExactFleetError::NoMembers { .. }
        ));
    }

    #[test]
    fn permission_ceiling_can_only_narrow_the_session_posture() {
        let session = PermissionCeiling {
            write: false,
            network_tool: false,
            shell: ShellCeiling::ReadOnly,
            delegation_depth: 0,
            tools: true,
        };
        let member = PermissionCeiling::preset("full").expect("preset");

        let clamped = member.clamp_to(session);

        assert!(
            !clamped.write,
            "member must not gain write over a read-only session"
        );
        assert!(!clamped.network_tool);
        assert_eq!(clamped.shell, ShellCeiling::ReadOnly);
        assert_eq!(clamped.delegation_depth, 0);
    }

    #[test]
    fn declared_schema_kind_distinguishes_the_two_forms() {
        assert_eq!(declared_schema_kind(GLM_FLEET).as_deref(), Some("exact"));
        assert_eq!(
            declared_schema_kind("name = \"stopship\"\n\n[roles]\nscout = \"scout\"\n"),
            None
        );
    }

    #[test]
    fn unsupported_revision_fails_closed() {
        let text = r#"
name = "f"
schema = "exact"
schema_revision = 99

[[members]]
id = "w"
provider = "zai"
model = "glm-5"
"#;
        assert!(matches!(
            ExactFleet::parse(text).expect_err("future revision"),
            ExactFleetError::UnsupportedRevision { revision: 99, .. }
        ));
    }
}
