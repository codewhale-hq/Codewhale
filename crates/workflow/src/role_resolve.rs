//! Fleet role resolution for Workflow steps (#4177).
//!
//! Workflow owns **what order**; Fleet owns **who**. Steps declare a fleet
//! `role` (and optional task prompt). At run time the fleet roster maps
//! `role → AgentProfile id`. This module is the pure resolution path used by
//! unit tests and by the dispatcher before spawn — it never imports tmux or
//! session management.
//!
//! Precedence (aligned with #4111 / #4136):
//! 1. Explicit `profile` on the step
//! 2. Fleet role map entry for `role`
//! 3. Role name used as profile id when the map has no alias
//!
//! Inline provider/model are **not** identity fields. They remain optional
//! overrides on [`crate::ModelPolicy`]; step identity is role/profile only.

use std::collections::BTreeMap;

use thiserror::Error;

/// Named fleet roster: role name → AgentProfile id.
///
/// Role and profile tokens are compared case-insensitively after trim +
/// lowercase normalization.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FleetRoleMap {
    /// Lowercased role → profile id (as configured; not re-cased).
    roles: BTreeMap<String, String>,
}

impl FleetRoleMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a role → profile binding. Empty tokens are rejected.
    pub fn insert(
        &mut self,
        role: impl Into<String>,
        profile: impl Into<String>,
    ) -> Result<(), FleetRoleResolveError> {
        let role = normalize_token(&role.into()).ok_or(FleetRoleResolveError::EmptyRole)?;
        let profile =
            normalize_token(&profile.into()).ok_or(FleetRoleResolveError::EmptyProfile)?;
        self.roles.insert(role, profile);
        Ok(())
    }

    pub fn from_pairs<I, R, P>(pairs: I) -> Result<Self, FleetRoleResolveError>
    where
        I: IntoIterator<Item = (R, P)>,
        R: Into<String>,
        P: Into<String>,
    {
        let mut map = Self::new();
        for (role, profile) in pairs {
            map.insert(role, profile)?;
        }
        Ok(map)
    }

    /// Look up the profile id bound to `role`, if any.
    pub fn get(&self, role: &str) -> Option<&str> {
        let key = normalize_token(role)?;
        self.roles.get(&key).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.roles.is_empty()
    }

    pub fn len(&self) -> usize {
        self.roles.len()
    }
}

/// Result of resolving a workflow step against a fleet roster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkflowAgent {
    /// Fleet role declared on the step, if any.
    pub resolved_role: Option<String>,
    /// AgentProfile id to spawn.
    pub resolved_profile: String,
    /// How the profile was chosen: `explicit_profile`, `fleet_role`, or
    /// `role_as_profile`.
    pub route_source: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FleetRoleResolveError {
    #[error("fleet role name must be a non-empty token")]
    EmptyRole,
    #[error("fleet profile id must be a non-empty token")]
    EmptyProfile,
    #[error("unknown fleet role `{role}`: not present in fleet roster (known roles: {known})")]
    UnknownRole { role: String, known: String },
    #[error(
        "workflow step requires a fleet role or explicit profile; provider/model alone are not identity"
    )]
    MissingRoleOrProfile,
    #[error("role `{role}` must be a non-empty token without whitespace, quotes, or `=`")]
    InvalidRoleToken { role: String },
}

/// Normalize a role/profile token: trim, lowercase. Returns `None` if empty
/// or if the token contains whitespace / quotes / backticks / `=`.
pub fn normalize_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed
        .chars()
        .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\'' | '`' | '='))
    {
        return None;
    }
    Some(trimmed.to_ascii_lowercase())
}

/// Validate a role token the same way leaf profiles are validated.
pub fn validate_role_token(role: &str) -> Result<String, FleetRoleResolveError> {
    normalize_token(role).ok_or_else(|| FleetRoleResolveError::InvalidRoleToken {
        role: role.to_string(),
    })
}

/// Resolve step identity from optional `role` + optional explicit `profile`
/// against a fleet role map.
///
/// When `require_known_role` is true and `role` is set without an explicit
/// profile, the role must exist in `fleet` (unknown roles fail clearly).
/// When false, an unknown role falls through to `role_as_profile` (useful
/// when the dispatcher will validate membership later against a full roster).
pub fn resolve_workflow_agent(
    role: Option<&str>,
    profile: Option<&str>,
    fleet: &FleetRoleMap,
    require_known_role: bool,
) -> Result<ResolvedWorkflowAgent, FleetRoleResolveError> {
    let role_norm = match role {
        Some(raw) => Some(validate_role_token(raw)?),
        None => None,
    };
    let profile_norm =
        match profile {
            Some(raw) => Some(validate_role_token(raw).map_err(|_| {
                FleetRoleResolveError::InvalidRoleToken {
                    role: raw.to_string(),
                }
            })?),
            None => None,
        };

    // Explicit profile always wins (task-field precedence).
    if let Some(resolved_profile) = profile_norm {
        return Ok(ResolvedWorkflowAgent {
            resolved_role: role_norm,
            resolved_profile,
            route_source: "explicit_profile",
        });
    }

    let Some(role_name) = role_norm else {
        return Err(FleetRoleResolveError::MissingRoleOrProfile);
    };

    if let Some(mapped) = fleet.get(&role_name) {
        return Ok(ResolvedWorkflowAgent {
            resolved_role: Some(role_name),
            resolved_profile: mapped.to_string(),
            route_source: "fleet_role",
        });
    }

    if require_known_role {
        let known = if fleet.is_empty() {
            "(none)".to_string()
        } else {
            fleet.roles.keys().cloned().collect::<Vec<_>>().join(", ")
        };
        return Err(FleetRoleResolveError::UnknownRole {
            role: role_name,
            known,
        });
    }

    Ok(ResolvedWorkflowAgent {
        resolved_role: Some(role_name.clone()),
        resolved_profile: role_name,
        route_source: "role_as_profile",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stopship_fleet() -> FleetRoleMap {
        FleetRoleMap::from_pairs([
            ("scout", "scout"),
            ("implementer", "builder"),
            ("reviewer", "reviewer"),
            ("verifier", "verifier"),
            ("release_lead", "manager"),
        ])
        .expect("valid fleet pairs")
    }

    #[test]
    fn known_role_resolves_to_configured_profile() {
        let fleet = stopship_fleet();
        let resolved =
            resolve_workflow_agent(Some("implementer"), None, &fleet, true).expect("resolve");
        assert_eq!(resolved.resolved_role.as_deref(), Some("implementer"));
        assert_eq!(resolved.resolved_profile, "builder");
        assert_eq!(resolved.route_source, "fleet_role");
    }

    #[test]
    fn unknown_role_fails_clearly() {
        let fleet = stopship_fleet();
        let err = resolve_workflow_agent(Some("wizard"), None, &fleet, true)
            .expect_err("unknown role must fail");
        match err {
            FleetRoleResolveError::UnknownRole { role, known } => {
                assert_eq!(role, "wizard");
                assert!(known.contains("scout"), "known={known}");
                assert!(known.contains("implementer"), "known={known}");
            }
            other => panic!("expected UnknownRole, got {other:?}"),
        }
    }

    #[test]
    fn explicit_profile_wins_over_role_map() {
        let fleet = stopship_fleet();
        let resolved = resolve_workflow_agent(Some("scout"), Some("custom-scout"), &fleet, true)
            .expect("resolve");
        assert_eq!(resolved.resolved_role.as_deref(), Some("scout"));
        assert_eq!(resolved.resolved_profile, "custom-scout");
        assert_eq!(resolved.route_source, "explicit_profile");
    }

    #[test]
    fn missing_role_and_profile_fails() {
        let fleet = stopship_fleet();
        let err = resolve_workflow_agent(None, None, &fleet, true).expect_err("identity required");
        assert!(matches!(err, FleetRoleResolveError::MissingRoleOrProfile));
    }

    #[test]
    fn role_token_rejects_whitespace_and_equals() {
        for bad in ["", "has space", "role=x", "quote\"y"] {
            assert!(
                validate_role_token(bad).is_err(),
                "token {bad:?} should be rejected"
            );
        }
        assert_eq!(validate_role_token("  Scout  ").unwrap(), "scout");
    }

    #[test]
    fn require_known_role_false_falls_back_to_role_as_profile() {
        let fleet = FleetRoleMap::new();
        let resolved = resolve_workflow_agent(Some("scout"), None, &fleet, false).expect("resolve");
        assert_eq!(resolved.resolved_profile, "scout");
        assert_eq!(resolved.route_source, "role_as_profile");
    }
}
