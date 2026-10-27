//! The **Adaptive Reasoning Router** — a saved, reusable *service*, not a Fleet
//! member.
//!
//! A Fleet answers *who*: the exact provider/model assignments an operator
//! saved for each worker. A Reasoning Router answers a much smaller question,
//! for an already frozen worker route: *how hard should this already-chosen
//! model think on this task?* Those are different kinds of thing, so they are
//! different kinds of value here:
//!
//! - A Router is **never dispatchable**. It has no role, no tools, no shell, no
//!   write authority, and no delegation budget. It cannot be named by a task.
//! - A Router is **referenced, not embedded**. It is saved once at
//!   `routers/<name>.toml` and referenced by name from any number of Fleets, so
//!   two Fleets can share one Router configuration without duplicating it.
//! - A Router **never changes a route**. Provider, model, member, role, tools,
//!   and permissions are all frozen before it is called and are not among the
//!   things it is allowed to answer.
//!
//! ## Cheap by construction
//!
//! A Router call is a per-task tax on someone's tokens, so its own reasoning is
//! capped at [`RouterCallReasoning`] — `off` or `low`, nothing else. `medium`,
//! `high`, and `max` are **rejected at parse time rather than silently clamped**:
//! an operator who wrote `high` asked for something this service will not do,
//! and quietly running at `off` while the file says `high` is exactly the kind
//! of invisible substitution receipts exist to prevent.
//!
//! The reverse lie is equally forbidden. A profile that asks for `low` is
//! *called* at `low` wherever the route can express it; nothing here forces
//! `off` and then reports `low`. Normalization against the route's real
//! capability is recorded on the receipt (see
//! [`crate::fleet_reasoning::RouterCallDisclosure`]).
//!
//! ## Legacy inline routers
//!
//! The prototype form — a `[[members]]` entry with `kind = "router"` inside the
//! Fleet file — still parses, is labelled `legacy_inline`, and is **normalized
//! into the same [`CapturedReasoningRouter`]** the named store produces. There
//! is one runtime representation of the service, whichever way it was written.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::fleet_exact::{FrozenRoute, ReasoningTier, RouterMember};
use crate::named_fleet::FleetSearchRoot;
use codewhale_protocol::fleet::PermissionCeiling;

/// Directory (under each search root) that holds saved Router profiles.
pub const REASONING_ROUTER_DIR: &str = "routers";
/// Wire value of the `schema` key that selects a Router profile document.
pub const REASONING_ROUTER_SCHEMA_KIND: &str = "reasoning_router";
/// Current revision of the Router profile schema.
pub const REASONING_ROUTER_SCHEMA_REVISION: u32 = 1;
/// Stable service label a receipt prints so the reader can tell at a glance
/// that this is the reasoning service and not a Fleet member.
pub const REASONING_ROUTER_SERVICE_KIND: &str = "reasoning_router";
/// Origin recorded for a Router that was written inline in a Fleet file.
pub const LEGACY_INLINE_ROUTER_ORIGIN: &str = "legacy_inline";

/// The reasoning a **Router call itself** may run at.
///
/// Deliberately not [`ReasoningTier`]: this type exists precisely so that
/// `medium`/`high`/`max` are unrepresentable. A Router emits one ~15-byte JSON
/// object; anything above `low` spends a user's tokens on thinking about a
/// question that does not need thought.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouterCallReasoning {
    #[default]
    Off,
    Low,
}

impl RouterCallReasoning {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
        }
    }

    /// The concrete tier this maps onto for capability normalization.
    #[must_use]
    pub const fn tier(self) -> ReasoningTier {
        match self {
            Self::Off => ReasoningTier::Off,
            Self::Low => ReasoningTier::Low,
        }
    }

    /// Parse a configured value.
    ///
    /// `medium`/`high`/`max` are a distinct, named error rather than a clamp —
    /// see the module docs.
    pub fn parse(value: &str, router: &str) -> Result<Self, ReasoningRouterError> {
        let trimmed = value.trim();
        match trimmed.to_ascii_lowercase().as_str() {
            "off" | "none" | "disabled" => Ok(Self::Off),
            "low" | "minimal" => Ok(Self::Low),
            "medium" | "mid" | "high" | "max" | "maximum" | "xhigh" => {
                Err(ReasoningRouterError::CallReasoningTooExpensive {
                    router: router.to_string(),
                    value: trimmed.to_string(),
                })
            }
            _ => Err(ReasoningRouterError::InvalidCallReasoning {
                router: router.to_string(),
                value: trimmed.to_string(),
            }),
        }
    }
}

/// A Router identity qualified by the origin its definition came from.
///
/// Path-free for the same reason [`crate::QualifiedFleetId`] is: an absolute
/// path in a durable receipt leaks the operator's home directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualifiedRouterId {
    pub name: String,
    pub origin: String,
}

impl QualifiedRouterId {
    #[must_use]
    pub fn qualified(&self) -> String {
        format!("{}/{}", self.origin, self.name)
    }
}

/// A saved Router profile: one exact provider/model plus a cheap call ceiling.
///
/// This is *the* reusable unit. Any number of Fleets may reference the same
/// profile by name; none of them owns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningRouterProfile {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub schema_revision: u32,
    /// Exact configured provider id.
    pub provider: String,
    /// Exact model id.
    pub model: String,
    /// What the Router's own call runs at. `off` or `low` only.
    pub call_reasoning: RouterCallReasoning,
}

impl ReasoningRouterProfile {
    /// Parse a Router profile document.
    pub fn parse(text: &str) -> Result<Self, ReasoningRouterError> {
        let doc: RouterProfileToml =
            toml::from_str(text).map_err(|error| ReasoningRouterError::Parse(error.to_string()))?;
        if !doc
            .schema
            .trim()
            .eq_ignore_ascii_case(REASONING_ROUTER_SCHEMA_KIND)
        {
            return Err(ReasoningRouterError::UnknownSchema {
                schema: doc.schema.trim().to_string(),
            });
        }
        if doc.schema_revision != REASONING_ROUTER_SCHEMA_REVISION {
            return Err(ReasoningRouterError::UnsupportedRevision {
                revision: doc.schema_revision,
                supported: REASONING_ROUTER_SCHEMA_REVISION,
            });
        }
        let name = crate::role_resolve::normalize_token(&doc.name).ok_or_else(|| {
            ReasoningRouterError::InvalidToken {
                field: "name".to_string(),
                value: doc.name.trim().to_string(),
            }
        })?;
        let provider = exact_token(&doc.provider, &name, "provider")?;
        let model = exact_token(&doc.model, &name, "model")?;
        let call_reasoning = match doc.call_reasoning.as_deref() {
            None => RouterCallReasoning::default(),
            Some(value) => RouterCallReasoning::parse(value, &name)?,
        };

        Ok(Self {
            name,
            description: doc.description,
            schema_revision: doc.schema_revision,
            provider,
            model,
            call_reasoning,
        })
    }

    /// Load one profile from a labelled search root set.
    ///
    /// A bare name present under more than one origin is **ambiguous**: a
    /// personal `~/.codewhale` Router silently shadowing a project Router would
    /// change which provider sees every task's routing summary. Naming the
    /// origin (`codewhale_home/fast`) resolves it.
    pub fn load_by_name(
        name: &str,
        search_roots: &[FleetSearchRoot],
    ) -> Result<(Self, QualifiedRouterId), ReasoningRouterError> {
        let (requested_origin, bare) = split_qualified(name);
        if crate::named_fleet::validate_fleet_file_stem(bare).is_err() {
            return Err(ReasoningRouterError::InvalidToken {
                field: "router reference".to_string(),
                value: name.trim().to_string(),
            });
        }
        let file_name = format!("{bare}.toml");

        let mut candidates: Vec<(&FleetSearchRoot, PathBuf)> = Vec::new();
        for root in search_roots {
            if let Some(origin) = requested_origin
                && !root.origin.eq_ignore_ascii_case(origin)
            {
                continue;
            }
            let path = root.root.join(REASONING_ROUTER_DIR).join(&file_name);
            if path.is_file() {
                candidates.push((root, path));
            }
        }

        let Some((first_root, first_path)) = candidates.first() else {
            return Err(ReasoningRouterError::NotFound {
                name: name.trim().to_string(),
            });
        };
        if candidates.len() > 1 {
            // Unlike legacy Fleet role maps, there is no first-hit-wins fallback
            // here: every Router profile names an exact provider/model, so a
            // shadowed one always changes behavior.
            return Err(ReasoningRouterError::AmbiguousRouter {
                name: bare.to_string(),
                origins: candidates
                    .iter()
                    .map(|(root, _)| format!("{}/{bare}", root.origin))
                    .collect(),
            });
        }

        let text =
            std::fs::read_to_string(first_path).map_err(|error| ReasoningRouterError::Io {
                path: first_path.display().to_string(),
                message: error.to_string(),
            })?;
        let profile = Self::parse(&text)?;
        if profile.name != bare {
            return Err(ReasoningRouterError::NameMismatch {
                declared: profile.name.clone(),
                expected: bare.to_string(),
            });
        }
        let id = QualifiedRouterId {
            name: profile.name.clone(),
            origin: first_root.origin.clone(),
        };
        Ok((profile, id))
    }
}

/// The Router service as frozen into a Workflow snapshot.
///
/// Whether it came from a named profile or from the legacy inline member, this
/// is the single runtime representation. Nothing downstream branches on which
/// form the operator wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedReasoningRouter {
    /// Always [`REASONING_ROUTER_SERVICE_KIND`]. Recorded explicitly so a
    /// receipt states what kind of thing this is instead of implying it.
    #[serde(default = "default_service_kind")]
    pub service_kind: String,
    /// The Router's id: the profile name, or the inline member's id.
    pub id: String,
    /// The origin the definition came from, or [`LEGACY_INLINE_ROUTER_ORIGIN`].
    #[serde(default = "default_router_origin")]
    pub origin: String,
    /// True when this was written inline in the Fleet file rather than saved as
    /// a reusable profile.
    #[serde(default)]
    pub legacy_inline: bool,
    /// The Router's own exact provider/model.
    pub route: FrozenRoute,
    /// What the operator configured this Router's call to run at.
    #[serde(default)]
    pub requested_call_reasoning: RouterCallReasoning,
    /// Always `false`. Stated rather than implied.
    #[serde(default)]
    pub dispatchable: bool,
    /// Always [`PermissionCeiling::ROUTER`].
    #[serde(default = "router_permissions")]
    pub permissions: PermissionCeiling,
}

fn default_service_kind() -> String {
    REASONING_ROUTER_SERVICE_KIND.to_string()
}

fn default_router_origin() -> String {
    LEGACY_INLINE_ROUTER_ORIGIN.to_string()
}

fn router_permissions() -> PermissionCeiling {
    PermissionCeiling::ROUTER
}

impl CapturedReasoningRouter {
    /// Capture a saved, reusable profile.
    #[must_use]
    pub fn from_profile(profile: &ReasoningRouterProfile, origin: impl Into<String>) -> Self {
        Self {
            service_kind: default_service_kind(),
            id: profile.name.clone(),
            origin: origin.into(),
            legacy_inline: false,
            route: FrozenRoute {
                provider: profile.provider.clone(),
                model: profile.model.clone(),
            },
            requested_call_reasoning: profile.call_reasoning,
            dispatchable: false,
            permissions: PermissionCeiling::ROUTER,
        }
    }

    /// Normalize the prototype inline form into the same captured service.
    ///
    /// The inline member's own `reasoning` was a full [`ReasoningTier`]; it is
    /// mapped onto the cheap call ceiling here, and anything above `low` is
    /// rejected by the Fleet parser rather than clamped silently.
    #[must_use]
    pub fn from_legacy_inline(member: &RouterMember) -> Self {
        Self {
            service_kind: default_service_kind(),
            id: member.id.clone(),
            origin: default_router_origin(),
            legacy_inline: true,
            route: member.frozen_route(),
            requested_call_reasoning: member.call_reasoning,
            dispatchable: false,
            permissions: PermissionCeiling::ROUTER,
        }
    }

    /// `origin/id` — the stable display form a receipt prints.
    #[must_use]
    pub fn qualified(&self) -> String {
        format!("{}/{}", self.origin, self.id)
    }

    /// A Router is never a worker. Constant, not a policy lookup.
    #[must_use]
    pub const fn is_dispatchable(&self) -> bool {
        false
    }

    /// The Router's tool surface is empty, always.
    #[must_use]
    pub const fn tool_surface(&self) -> &'static [&'static str] {
        &[]
    }
}

/// How a Fleet points at its Router.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum FleetRouterRef {
    /// A saved, reusable profile named by `reasoning_router = "<name>"`.
    Profile { name: String },
    /// The prototype inline `[[members]] kind = "router"` form.
    LegacyInline(Box<RouterMember>),
}

fn split_qualified(name: &str) -> (Option<&str>, &str) {
    let trimmed = name.trim();
    match trimmed.split_once('/') {
        Some((origin, bare)) if !origin.trim().is_empty() && !bare.trim().is_empty() => {
            (Some(origin.trim()), bare.trim())
        }
        _ => (None, trimmed),
    }
}

fn exact_token(value: &str, router: &str, field: &str) -> Result<String, ReasoningRouterError> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\'' | '`' | '='))
    {
        return Err(ReasoningRouterError::InvalidToken {
            field: format!("{router}.{field}"),
            value: trimmed.to_string(),
        });
    }
    Ok(trimmed.to_string())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouterProfileToml {
    name: String,
    #[serde(default)]
    description: Option<String>,
    schema: String,
    #[serde(default = "default_router_revision")]
    schema_revision: u32,
    provider: String,
    model: String,
    #[serde(default, alias = "reasoning")]
    call_reasoning: Option<String>,
}

const fn default_router_revision() -> u32 {
    REASONING_ROUTER_SCHEMA_REVISION
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReasoningRouterError {
    #[error("failed to parse reasoning router profile: {0}")]
    Parse(String),
    #[error("failed to read reasoning router profile `{path}`: {message}")]
    Io { path: String, message: String },
    #[error("reasoning router `{name}` was not found in any configured origin")]
    NotFound { name: String },
    #[error(
        "reasoning router `{name}` is defined in more than one place ({}); a router names an \
         exact provider/model, so shadowing would silently change which provider sees every \
         routing summary. Name one explicitly as `origin/{name}`.",
        origins.join(", ")
    )]
    AmbiguousRouter { name: String, origins: Vec<String> },
    #[error("unknown reasoning router schema `{schema}`; expected `reasoning_router`")]
    UnknownSchema { schema: String },
    #[error(
        "reasoning router schema revision {revision} is not supported (this build reads {supported})"
    )]
    UnsupportedRevision { revision: u32, supported: u32 },
    #[error("{field} must be a non-empty token without whitespace, quotes, or `=` (got `{value}`)")]
    InvalidToken { field: String, value: String },
    #[error("reasoning router name mismatch: file declares `{declared}`, expected `{expected}`")]
    NameMismatch { declared: String, expected: String },
    #[error(
        "reasoning router `{router}` requests call reasoning `{value}`; a router may only run at \
         `off` or `low`. This is rejected rather than clamped: a router answers one tiny JSON \
         object per task, and running it at `{value}` would spend your tokens on thinking nobody \
         asked for. Set `call_reasoning` to `off` or `low`."
    )]
    CallReasoningTooExpensive { router: String, value: String },
    #[error(
        "reasoning router `{router}` has invalid call reasoning `{value}`; expected off or low"
    )]
    InvalidCallReasoning { router: String, value: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    const LUNA: &str = r#"
name = "luna-low"
description = "GPT-5.6 Luna, called at low"
schema = "reasoning_router"
schema_revision = 1
provider = "openai"
model = "gpt-5.6-luna"
call_reasoning = "low"
"#;

    #[test]
    fn a_profile_parses_its_exact_route_and_cheap_call_tier() {
        let profile = ReasoningRouterProfile::parse(LUNA).expect("parse");
        assert_eq!(profile.name, "luna-low");
        assert_eq!(profile.provider, "openai");
        assert_eq!(profile.model, "gpt-5.6-luna");
        assert_eq!(profile.call_reasoning, RouterCallReasoning::Low);
        assert_eq!(profile.schema_revision, REASONING_ROUTER_SCHEMA_REVISION);
    }

    #[test]
    fn call_reasoning_defaults_to_off_when_unset() {
        let text = LUNA.replace("call_reasoning = \"low\"\n", "");
        let profile = ReasoningRouterProfile::parse(&text).expect("parse");
        assert_eq!(profile.call_reasoning, RouterCallReasoning::Off);
    }

    /// The whole point of the cheap ceiling: an expensive tier is an error the
    /// operator can see, never a clamp they cannot.
    #[test]
    fn medium_high_and_max_are_rejected_not_clamped() {
        for value in ["medium", "high", "max", "xhigh"] {
            let text = LUNA.replace("\"low\"", &format!("\"{value}\""));
            let err = ReasoningRouterProfile::parse(&text)
                .expect_err("an expensive router tier must be rejected");
            assert!(
                matches!(err, ReasoningRouterError::CallReasoningTooExpensive { .. }),
                "value={value} err={err:?}"
            );
            let message = err.to_string();
            assert!(message.contains("off"), "{message}");
            assert!(message.contains("low"), "{message}");
            assert!(
                !message.contains("clamped to"),
                "the error must not describe a clamp: {message}"
            );
        }
    }

    #[test]
    fn an_unknown_tier_is_its_own_error() {
        let text = LUNA.replace("\"low\"", "\"turbo\"");
        assert!(matches!(
            ReasoningRouterProfile::parse(&text).expect_err("garbage"),
            ReasoningRouterError::InvalidCallReasoning { .. }
        ));
    }

    #[test]
    fn an_unknown_schema_or_future_revision_fails_closed() {
        let wrong_schema = LUNA.replace("\"reasoning_router\"", "\"exact\"");
        assert!(matches!(
            ReasoningRouterProfile::parse(&wrong_schema).expect_err("schema"),
            ReasoningRouterError::UnknownSchema { .. }
        ));

        let future = LUNA.replace("schema_revision = 1", "schema_revision = 99");
        assert!(matches!(
            ReasoningRouterProfile::parse(&future).expect_err("revision"),
            ReasoningRouterError::UnsupportedRevision { revision: 99, .. }
        ));
    }

    #[test]
    fn a_captured_profile_holds_no_authority_and_is_never_dispatchable() {
        let profile = ReasoningRouterProfile::parse(LUNA).expect("parse");
        let captured = CapturedReasoningRouter::from_profile(&profile, "workspace");

        assert_eq!(captured.qualified(), "workspace/luna-low");
        assert_eq!(captured.service_kind, REASONING_ROUTER_SERVICE_KIND);
        assert!(!captured.legacy_inline);
        assert!(!captured.is_dispatchable());
        assert!(captured.tool_surface().is_empty());
        assert!(!captured.permissions.tools);
        assert!(!captured.permissions.write);
        assert!(!captured.permissions.network_tool);
        assert_eq!(captured.permissions.delegation_depth, 0);
    }

    /// One saved profile, two Fleets. The service is referenced, not owned.
    #[test]
    fn one_saved_profile_serves_more_than_one_fleet() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::create_dir_all(tmp.path().join(REASONING_ROUTER_DIR)).expect("dir");
        std::fs::write(
            tmp.path().join(REASONING_ROUTER_DIR).join("luna-low.toml"),
            LUNA,
        )
        .expect("write");

        let roots = vec![FleetSearchRoot::new("workspace", tmp.path())];
        let (first, first_id) =
            ReasoningRouterProfile::load_by_name("luna-low", &roots).expect("load");
        let (second, second_id) =
            ReasoningRouterProfile::load_by_name("workspace/luna-low", &roots).expect("qualified");

        assert_eq!(first, second);
        assert_eq!(first_id, second_id);
        assert_eq!(first_id.qualified(), "workspace/luna-low");

        // Two independent captures of the same saved service agree exactly.
        let a = CapturedReasoningRouter::from_profile(&first, &first_id.origin);
        let b = CapturedReasoningRouter::from_profile(&second, &second_id.origin);
        assert_eq!(a, b);
    }

    #[test]
    fn router_names_cannot_leave_the_router_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(ws.join(REASONING_ROUTER_DIR)).expect("dir");
        let planted = LUNA.replace("\"luna-low\"", "\"outside\"");
        std::fs::write(ws.join("outside.toml"), &planted).expect("sibling");
        std::fs::write(tmp.path().join("outside.toml"), &planted).expect("outside");
        let roots = vec![FleetSearchRoot::new("workspace", &ws)];
        let absolute = tmp.path().join("outside");

        for name in [
            "workspace/../outside",
            "workspace/../../outside",
            absolute.to_string_lossy().as_ref(),
        ] {
            let err = ReasoningRouterProfile::load_by_name(name, &roots).expect_err(name);
            assert!(
                matches!(err, ReasoningRouterError::InvalidToken { .. }),
                "{name}: expected InvalidToken, got {err:?}"
            );
        }
    }

    /// Bare-name ambiguity across origins must fail; a qualified origin works.
    #[test]
    fn a_bare_name_defined_in_two_origins_is_ambiguous_until_qualified() {
        let tmp = tempfile::tempdir().expect("tmp");
        let home = tmp.path().join("home");
        let workspace = tmp.path().join("workspace");
        for root in [&home, &workspace] {
            std::fs::create_dir_all(root.join(REASONING_ROUTER_DIR)).expect("dir");
        }
        std::fs::write(
            home.join(REASONING_ROUTER_DIR).join("luna-low.toml"),
            LUNA.replace("gpt-5.6-luna", "gpt-5.6-luna-mini"),
        )
        .expect("home");
        std::fs::write(
            workspace.join(REASONING_ROUTER_DIR).join("luna-low.toml"),
            LUNA,
        )
        .expect("workspace");

        let roots = vec![
            FleetSearchRoot::new("codewhale_home", &home),
            FleetSearchRoot::new("workspace", &workspace),
        ];

        let err = ReasoningRouterProfile::load_by_name("luna-low", &roots)
            .expect_err("bare name must not be resolved by shadowing");
        assert!(
            matches!(err, ReasoningRouterError::AmbiguousRouter { .. }),
            "{err:?}"
        );
        let message = err.to_string();
        assert!(message.contains("codewhale_home"), "{message}");
        assert!(message.contains("workspace"), "{message}");

        let (workspace_profile, id) =
            ReasoningRouterProfile::load_by_name("workspace/luna-low", &roots).expect("qualified");
        assert_eq!(id.qualified(), "workspace/luna-low");
        assert_eq!(workspace_profile.model, "gpt-5.6-luna");

        let (home_profile, home_id) =
            ReasoningRouterProfile::load_by_name("codewhale_home/luna-low", &roots)
                .expect("qualified");
        assert_eq!(home_id.qualified(), "codewhale_home/luna-low");
        assert_eq!(home_profile.model, "gpt-5.6-luna-mini");
    }

    #[test]
    fn a_missing_profile_is_a_named_error() {
        let tmp = tempfile::tempdir().expect("tmp");
        let roots = vec![FleetSearchRoot::new("workspace", tmp.path())];
        assert!(matches!(
            ReasoningRouterProfile::load_by_name("nope", &roots).expect_err("missing"),
            ReasoningRouterError::NotFound { .. }
        ));
    }

    /// A captured router serializes into a durable snapshot with no secrets and
    /// no paths, and older records without the newer fields still read.
    #[test]
    fn a_captured_router_is_durable_and_backward_compatible() {
        let profile = ReasoningRouterProfile::parse(LUNA).expect("parse");
        let captured = CapturedReasoningRouter::from_profile(&profile, "workspace");
        let json = serde_json::to_string(&captured).expect("serialize");
        let lowered = json.to_ascii_lowercase();
        for forbidden in ["api_key", "secret", "token", "bearer", "/users/", ".toml"] {
            assert!(!lowered.contains(forbidden), "{forbidden} in {json}");
        }
        let back: CapturedReasoningRouter = serde_json::from_str(&json).expect("round-trip");
        assert_eq!(back, captured);

        let older = r#"{"id":"router","route":{"provider":"zai","model":"glm-5-turbo"}}"#;
        let legacy: CapturedReasoningRouter = serde_json::from_str(older).expect("serde defaults");
        assert_eq!(legacy.service_kind, REASONING_ROUTER_SERVICE_KIND);
        assert_eq!(legacy.origin, LEGACY_INLINE_ROUTER_ORIGIN);
        assert_eq!(legacy.requested_call_reasoning, RouterCallReasoning::Off);
        assert!(!legacy.dispatchable);
    }
}
