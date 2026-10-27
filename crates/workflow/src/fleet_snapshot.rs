//! Immutable Fleet snapshot taken at Workflow start.
//!
//! A saved Fleet is editable; a *running* Workflow is not. At start we capture
//! a secret-free, durable value containing the qualified Fleet identity, the
//! schema kind/revision/hash, the exact members, the exact routes, the
//! reasoning policies. Authority is absent by design: Runtime derives it from
//! the selected member's Runtime role and the live parent. Editing the saved file
//! afterwards changes only future runs — the snapshot in flight is unaffected,
//! because it owns copies and exposes no mutators.
//!
//! **No-secrets invariant**: every field here is a non-sensitive id, model
//! string, tier label, or boolean. There is deliberately no field that could
//! hold a credential, token, or base URL.

use serde::{Deserialize, Serialize};

use crate::fleet_exact::{
    ExactFleet, ExactFleetError, FrozenRoute, RequestedReasoning, canonical_member_key,
    canonical_role_key,
};
use crate::named_fleet::{FleetDocument, FleetSchema};
use crate::reasoning_router::CapturedReasoningRouter;
use codewhale_protocol::fleet::PermissionCeiling;

/// A Fleet identity qualified by where the definition came from.
///
/// Deliberately **path-free**. An absolute filesystem path in a durable receipt
/// leaks the operator's home directory, username, and machine layout into
/// journals and events that travel further than the machine that wrote them.
/// `origin/name` plus the schema/content hashes identify a definition precisely
/// enough to compare two runs, without any of that. Local diagnostic errors
/// (fleet not found, ambiguous fleet) still name paths — those are read on the
/// machine that produced them and never persisted onto a receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualifiedFleetId {
    /// Fleet name as declared in the file.
    pub name: String,
    /// Non-secret origin label, e.g. `workspace` or `codewhale_home`.
    pub origin: String,
}

impl QualifiedFleetId {
    /// `origin/name` — the stable display form.
    #[must_use]
    pub fn qualified(&self) -> String {
        format!("{}/{}", self.origin, self.name)
    }
}

/// One member as frozen into the snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetSnapshotMember {
    pub id: String,
    pub role: String,
    /// The exact route, frozen before any reasoning resolution.
    pub route: FrozenRoute,
    /// The reasoning policy the member requested (not the effective tier —
    /// that is resolved per run and recorded on the receipt).
    pub requested_reasoning: RequestedReasoning,
    /// Prototype snapshot compatibility only.
    ///
    /// New captures leave this absent. Old replay snapshots need the historic
    /// value solely to verify their original content hash; selection and
    /// Runtime authority never read it. Re-serializing a verified old snapshot
    /// preserves the field so its evidence remains round-trippable, while a
    /// fresh recapture emits the canonical authority-free shape.
    #[serde(
        default,
        rename = "permissions",
        skip_serializing_if = "Option::is_none"
    )]
    legacy_permissions: Option<PermissionCeiling>,
}

/// The Reasoning Router service a snapshot is attached to.
///
/// This is [`CapturedReasoningRouter`] under its historic name — the Router is
/// no longer a Fleet member, so the alias exists only to keep older call sites
/// and serialized shapes readable.
pub type FleetSnapshotRouter = CapturedReasoningRouter;

/// A legacy fleet's role → profile binding, recorded for provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetSnapshotLegacyRole {
    pub role: String,
    pub profile: String,
}

/// The immutable value captured at Workflow start.
///
/// Fields are private and there are no setters: once captured, the only way to
/// change a snapshot is to take a new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetSnapshot {
    fleet: QualifiedFleetId,
    schema_kind: String,
    schema_revision: u32,
    /// SHA-256 of the fleet definition bytes.
    schema_hash: String,
    /// SHA-256 over the captured members/routes/policies themselves, so two
    /// snapshots can be compared without re-reading the source file.
    content_hash: String,
    members: Vec<FleetSnapshotMember>,
    /// The attached Reasoning Router service, if this Fleet references one.
    /// Resolved by the host (which owns the search roots) and handed in, so a
    /// snapshot stays a pure value with no loader inside it.
    router: Option<FleetSnapshotRouter>,
    legacy_roles: Vec<FleetSnapshotLegacyRole>,
    /// Caller-supplied timestamp; this crate has no clock.
    captured_at: String,
}

impl FleetSnapshot {
    /// Capture a snapshot from a parsed fleet document and an already-resolved
    /// Reasoning Router service.
    ///
    /// Exact rosters are **revalidated here**, not trusted. `ExactFleet` is
    /// public and `Deserialize`, so a document can reach this point without
    /// having passed the TOML parser's invariant checks; capture is the last
    /// place to catch a duplicate role, an id/role collision, or a worker
    /// claiming the Router's identity before those become a running Workflow.
    ///
    /// `router` is the captured service, whether it came from a saved reusable
    /// profile or was normalized out of the legacy inline form. Resolution
    /// happens in the host because it needs the fleet search roots; capture
    /// only records the result.
    pub fn capture(
        fleet: QualifiedFleetId,
        document: &FleetDocument,
        captured_at: impl Into<String>,
        router: Option<CapturedReasoningRouter>,
    ) -> Result<Self, ExactFleetError> {
        let (members, legacy_roles) = match document.schema() {
            FleetSchema::Exact(exact) => {
                exact.validate()?;
                (exact_members(exact), Vec::new())
            }
            FleetSchema::Legacy(legacy) => (
                Vec::new(),
                legacy
                    .roles
                    .iter()
                    .map(|(role, profile)| FleetSnapshotLegacyRole {
                        role: role.clone(),
                        profile: profile.clone(),
                    })
                    .collect(),
            ),
        };

        let mut snapshot = Self {
            fleet,
            schema_kind: document.schema_kind().to_string(),
            schema_revision: document.schema_revision(),
            schema_hash: document.source_hash().to_string(),
            content_hash: String::new(),
            members,
            router,
            legacy_roles,
            captured_at: captured_at.into(),
        };
        snapshot.content_hash = snapshot.compute_content_hash();
        Ok(snapshot)
    }

    /// Recompute the canonical content hash and reject a snapshot whose
    /// recorded hash does not describe its own contents.
    ///
    /// `FleetSnapshot` is `Deserialize` and its `content_hash` is an ordinary
    /// field, so a snapshot can reach a launch without ever having passed
    /// [`Self::capture`] — through a replay file, a cache, or an IPC hop. That
    /// hash is then stamped onto the durable receipt as the evidence that a run
    /// matched a saved definition, so an unverified one is not weak evidence but
    /// *false* evidence: it asserts a definition the members may not describe.
    ///
    /// Call this before anything durable or costly happens. It is cheap (one
    /// canonical serialization plus a SHA-256) and it is the only thing standing
    /// between a tampered or migrated snapshot and a receipt that vouches for
    /// it.
    pub fn verify_content_hash(&self) -> Result<(), ExactFleetError> {
        let recomputed = if self
            .members
            .iter()
            .any(|member| member.legacy_permissions.is_some())
        {
            // Once an old authority-bearing field is present, it must be
            // covered by the historic hash. Accepting the new canonical hash
            // here would let arbitrary compatibility bytes hitchhike on an
            // otherwise valid snapshot without verification.
            self.compute_legacy_content_hash().ok_or_else(|| {
                ExactFleetError::ContentHashMismatch {
                    fleet: self.fleet.qualified(),
                    recorded: self.content_hash.clone(),
                    recomputed: "unverifiable mixed legacy-permissions shape".to_string(),
                }
            })?
        } else {
            self.compute_content_hash()
        };
        if recomputed == self.content_hash {
            return Ok(());
        }
        Err(ExactFleetError::ContentHashMismatch {
            fleet: self.fleet.qualified(),
            recorded: self.content_hash.clone(),
            recomputed,
        })
    }

    /// [`Self::verify_content_hash`], as a guard that yields the snapshot.
    ///
    /// Exists so a load path cannot verify and then accidentally go on to use a
    /// *different* value: the only thing this returns is the snapshot it just
    /// checked.
    pub fn into_verified(self) -> Result<Self, ExactFleetError> {
        self.verify_content_hash()?;
        Ok(self)
    }

    fn compute_content_hash(&self) -> String {
        // Hash only the captured shape, not the timestamp: two Workflows
        // started from the same saved Fleet must agree.
        #[derive(Serialize)]
        struct CanonicalMember<'a> {
            id: &'a str,
            role: &'a str,
            route: &'a FrozenRoute,
            requested_reasoning: RequestedReasoning,
        }
        #[derive(Serialize)]
        struct Shape<'a> {
            fleet: &'a QualifiedFleetId,
            schema_kind: &'a str,
            schema_revision: u32,
            schema_hash: &'a str,
            members: Vec<CanonicalMember<'a>>,
            router: &'a Option<FleetSnapshotRouter>,
            legacy_roles: &'a [FleetSnapshotLegacyRole],
        }

        let shape = Shape {
            fleet: &self.fleet,
            schema_kind: &self.schema_kind,
            schema_revision: self.schema_revision,
            schema_hash: &self.schema_hash,
            members: self
                .members
                .iter()
                .map(|member| CanonicalMember {
                    id: &member.id,
                    role: &member.role,
                    route: &member.route,
                    requested_reasoning: member.requested_reasoning,
                })
                .collect(),
            router: &self.router,
            legacy_roles: &self.legacy_roles,
        };
        let encoded = serde_json::to_vec(&shape).expect("snapshot shape is serializable");
        crate::named_fleet::sha256_label(&encoded)
    }

    /// Recompute the prototype content hash when (and only when) every exact
    /// member carried its historic permission field. This validates old
    /// evidence without projecting that field into current authority.
    fn compute_legacy_content_hash(&self) -> Option<String> {
        #[derive(Serialize)]
        struct LegacyMember<'a> {
            id: &'a str,
            role: &'a str,
            route: &'a FrozenRoute,
            requested_reasoning: RequestedReasoning,
            permissions: PermissionCeiling,
        }
        #[derive(Serialize)]
        struct Shape<'a> {
            fleet: &'a QualifiedFleetId,
            schema_kind: &'a str,
            schema_revision: u32,
            schema_hash: &'a str,
            members: Vec<LegacyMember<'a>>,
            router: &'a Option<FleetSnapshotRouter>,
            legacy_roles: &'a [FleetSnapshotLegacyRole],
        }

        if self.members.is_empty()
            || self
                .members
                .iter()
                .any(|member| member.legacy_permissions.is_none())
        {
            return None;
        }
        let shape = Shape {
            fleet: &self.fleet,
            schema_kind: &self.schema_kind,
            schema_revision: self.schema_revision,
            schema_hash: &self.schema_hash,
            members: self
                .members
                .iter()
                .map(|member| LegacyMember {
                    id: &member.id,
                    role: &member.role,
                    route: &member.route,
                    requested_reasoning: member.requested_reasoning,
                    permissions: member
                        .legacy_permissions
                        .expect("checked every legacy permission above"),
                })
                .collect(),
            router: &self.router,
            legacy_roles: &self.legacy_roles,
        };
        let encoded = serde_json::to_vec(&shape).expect("legacy snapshot shape is serializable");
        Some(crate::named_fleet::sha256_label(&encoded))
    }

    #[must_use]
    pub fn fleet(&self) -> &QualifiedFleetId {
        &self.fleet
    }

    #[must_use]
    pub fn schema_kind(&self) -> &str {
        &self.schema_kind
    }

    #[must_use]
    pub const fn schema_revision(&self) -> u32 {
        self.schema_revision
    }

    #[must_use]
    pub fn schema_hash(&self) -> &str {
        &self.schema_hash
    }

    #[must_use]
    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }

    #[must_use]
    pub fn members(&self) -> &[FleetSnapshotMember] {
        &self.members
    }

    #[must_use]
    pub fn router(&self) -> Option<&FleetSnapshotRouter> {
        self.router.as_ref()
    }

    #[must_use]
    pub fn legacy_roles(&self) -> &[FleetSnapshotLegacyRole] {
        &self.legacy_roles
    }

    #[must_use]
    pub fn captured_at(&self) -> &str {
        &self.captured_at
    }

    /// Look up a member by its **member id** — what addresses a roster entry.
    #[must_use]
    pub fn member(&self, id: &str) -> Option<&FleetSnapshotMember> {
        let key = canonical_member_key(id);
        self.members
            .iter()
            .find(|member| canonical_member_key(&member.id) == key)
    }

    /// Look up a member by its **semantic role** — what gates, handoffs, and
    /// records use. Kept separate from id lookup so a task can carry a
    /// meaningful role while the runtime resolves a distinct profile id.
    ///
    /// Both sides resolve through [`canonical_role_key`], so a snapshot frozen
    /// from a Fleet saved under a renamed role is still addressable by a gate or
    /// handoff that spells the role the old way.
    #[must_use]
    pub fn member_by_role(&self, role: &str) -> Option<&FleetSnapshotMember> {
        let key = canonical_role_key(role);
        self.members
            .iter()
            .find(|member| canonical_role_key(&member.role) == key)
    }

    /// Look up by id first, then by role. Roster invariants forbid an id/role
    /// collision, so this can never be order-dependent.
    #[must_use]
    pub fn member_by_id_or_role(&self, id_or_role: &str) -> Option<&FleetSnapshotMember> {
        self.member(id_or_role)
            .or_else(|| self.member_by_role(id_or_role))
    }

    /// Whether any frozen member requested `auto` reasoning — i.e. whether this
    /// Workflow needs a working Reasoning Router at all.
    #[must_use]
    pub fn has_auto_member(&self) -> bool {
        self.members
            .iter()
            .any(|member| member.requested_reasoning.is_auto())
    }

    /// Ids of the members that requested `auto`, for a startup error that names
    /// who actually needs the Router.
    #[must_use]
    pub fn auto_member_ids(&self) -> Vec<String> {
        self.members
            .iter()
            .filter(|member| member.requested_reasoning.is_auto())
            .map(|member| member.id.clone())
            .collect()
    }
}

fn exact_members(exact: &ExactFleet) -> Vec<FleetSnapshotMember> {
    exact
        .members
        .iter()
        .map(|member| FleetSnapshotMember {
            id: canonical_member_key(&member.id),
            // The snapshot is what every receipt is built from, so it records
            // the *canonical* role even when the saved file used a renamed one.
            // Old files keep working (lookup resolves either spelling); new
            // receipts never print a name the current schema does not use.
            role: canonical_role_key(&member.role),
            route: member.frozen_route(),
            requested_reasoning: member.reasoning,
            legacy_permissions: None,
        })
        .collect()
}

/// Verify a snapshot that arrived from anywhere other than [`FleetSnapshot::capture`].
///
/// The free function exists for load/deserialize seams that hold a snapshot by
/// reference and only need the yes/no answer — a durable-write guard, a replay
/// loader, a cache read. It is the same check as
/// [`FleetSnapshot::verify_content_hash`]; having a named entry point is what
/// lets those call sites read as "verify before use" rather than as an
/// incidental method call.
pub fn verify_snapshot_content_hash(snapshot: &FleetSnapshot) -> Result<(), ExactFleetError> {
    snapshot.verify_content_hash()
}

/// Normalize an exact Fleet's **legacy inline** Router into the captured
/// service, if it used the prototype form.
///
/// A Fleet that references a saved profile resolves through
/// [`crate::ReasoningRouterProfile::load_by_name`] instead, in the host that
/// owns the search roots. Both paths land on the same value, which is the whole
/// point of keeping only one runtime representation.
#[must_use]
pub fn captured_legacy_inline_router(exact: &ExactFleet) -> Option<CapturedReasoningRouter> {
    exact
        .legacy_inline_router()
        .map(CapturedReasoningRouter::from_legacy_inline)
}

#[cfg(test)]
mod content_hash_tests {
    use super::*;

    const EXACT_FLEET: &str = r#"
name = "glm-pair"
schema = "exact"

[[members]]
id = "implementer"
role = "builder"
provider = "zai"
model = "glm-5"
reasoning = "high"
permissions = "read_write"

[[members]]
id = "advisor-one"
role = "oracle"
provider = "zai"
model = "glm-5"
reasoning = "low"
permissions = "analyst"
"#;

    fn captured() -> FleetSnapshot {
        let document = FleetDocument::parse(EXACT_FLEET).expect("parse fleet document");
        FleetSnapshot::capture(
            QualifiedFleetId {
                name: "glm-pair".to_string(),
                origin: "workspace".to_string(),
            },
            &document,
            "2026-07-26T00:00:00Z",
            None,
        )
        .expect("capture")
    }

    #[test]
    fn a_freshly_captured_snapshot_verifies() {
        let snapshot = captured();
        assert!(snapshot.verify_content_hash().is_ok());
        assert!(verify_snapshot_content_hash(&snapshot).is_ok());
        assert!(snapshot.into_verified().is_ok());
    }

    /// The round trip a replay file, a cache read, or an IPC hop performs. An
    /// untouched snapshot must survive it — otherwise the guard below would be
    /// unusable at exactly the seams it exists for.
    #[test]
    fn an_untouched_round_trip_still_verifies() {
        let snapshot = captured();
        let encoded = serde_json::to_string(&snapshot).expect("serialize");
        let decoded: FleetSnapshot = serde_json::from_str(&encoded).expect("deserialize");

        assert_eq!(decoded, snapshot);
        assert!(decoded.verify_content_hash().is_ok());
    }

    /// The tamper case. A snapshot whose members were edited after capture
    /// keeps its old hash, and that hash is what a receipt would vouch for.
    /// Verification must reject it *before* any launch or durable write.
    #[test]
    fn an_edited_member_is_rejected_while_the_hash_still_claims_the_original() {
        let snapshot = captured();
        let original_hash = snapshot.content_hash().to_string();

        let mut value = serde_json::to_value(&snapshot).expect("serialize");
        // Widen a member's route — the single most consequential edit, and the
        // one a stale hash would silently certify.
        value["members"][0]["route"]["model"] = serde_json::json!("glm-5-max");
        let tampered: FleetSnapshot = serde_json::from_value(value).expect("deserialize");

        assert_eq!(
            tampered.content_hash(),
            original_hash,
            "the tamper does not touch the recorded hash — that is the point"
        );
        let error = tampered
            .verify_content_hash()
            .expect_err("a tampered snapshot must not verify");
        assert!(matches!(
            error,
            ExactFleetError::ContentHashMismatch { ref recorded, .. } if *recorded == original_hash
        ));
        assert!(tampered.into_verified().is_err());
    }

    /// Prototype snapshots carried member `permissions`. A valid old content
    /// hash remains verifiable for replay, but the field is compatibility
    /// evidence only and a fresh capture emits the authority-free shape.
    #[test]
    fn a_valid_legacy_permission_snapshot_verifies_and_round_trips() {
        let snapshot = captured();
        let mut value = serde_json::to_value(&snapshot).expect("serialize");
        for index in 0..2 {
            value["members"][index]["permissions"] = serde_json::json!({
                "write": index == 0,
                "network_tool": false,
                "shell": "read_only",
                "delegation_depth": 0,
                "tools": true
            });
        }
        let mut replay: FleetSnapshot = serde_json::from_value(value).expect("legacy replay loads");
        replay.content_hash = replay
            .compute_legacy_content_hash()
            .expect("old shape has a legacy hash");

        assert_ne!(replay.compute_content_hash(), replay.content_hash);
        assert!(replay.verify_content_hash().is_ok());
        let encoded = serde_json::to_string(&replay).expect("legacy replay remains durable");
        assert!(encoded.contains("\"permissions\""), "{encoded}");
        let decoded: FleetSnapshot = serde_json::from_str(&encoded).expect("round trip");
        assert!(decoded.verify_content_hash().is_ok());
    }

    #[test]
    fn a_tampered_legacy_permission_snapshot_fails_closed() {
        let snapshot = captured();
        let mut value = serde_json::to_value(&snapshot).expect("serialize");
        for index in 0..2 {
            value["members"][index]["permissions"] = serde_json::json!({
                "write": false,
                "network_tool": false,
                "shell": "read_only",
                "delegation_depth": 0,
                "tools": true
            });
        }
        let mut replay: FleetSnapshot = serde_json::from_value(value).expect("legacy replay loads");
        replay.content_hash = replay
            .compute_legacy_content_hash()
            .expect("old shape has a legacy hash");
        let recorded = replay.content_hash.clone();

        let mut tampered = serde_json::to_value(&replay).expect("serialize old replay");
        tampered["members"][0]["permissions"]["write"] = serde_json::json!(true);
        let tampered: FleetSnapshot = serde_json::from_value(tampered).expect("deserialize");
        assert_eq!(tampered.content_hash(), recorded);
        assert!(tampered.verify_content_hash().is_err());
    }

    #[test]
    fn legacy_permission_bytes_must_be_covered_by_the_legacy_hash() {
        let snapshot = captured();
        let canonical_hash = snapshot.content_hash().to_string();
        let mut value = serde_json::to_value(&snapshot).expect("serialize");
        for index in 0..2 {
            value["members"][index]["permissions"] = serde_json::json!({
                "write": false,
                "network_tool": false,
                "shell": "read_only",
                "delegation_depth": 0,
                "tools": true
            });
        }
        let replay: FleetSnapshot = serde_json::from_value(value).expect("legacy shape loads");

        assert_eq!(replay.content_hash(), canonical_hash);
        assert!(
            replay.verify_content_hash().is_err(),
            "legacy bytes may not hitchhike on the authority-free canonical hash"
        );
    }

    /// A forged hash fails the same way an edited body does: the check is a
    /// recomputation, not a presence test, so neither side can be trusted alone.
    #[test]
    fn a_forged_hash_is_rejected() {
        let snapshot = captured();
        let mut value = serde_json::to_value(&snapshot).expect("serialize");
        value["content_hash"] = serde_json::json!("0".repeat(64));
        let forged: FleetSnapshot = serde_json::from_value(value).expect("deserialize");

        assert!(forged.verify_content_hash().is_err());
    }

    /// The migration case: a snapshot written by an older build that recorded a
    /// renamed role verbatim. Capture now canonicalizes, so the *stored* role is
    /// `consultant` and the hash covers that — an old snapshot carrying
    /// `oracle` cannot pass verification and must be re-captured rather than
    /// quietly relabelled at read time.
    #[test]
    fn a_pre_rename_snapshot_is_rejected_rather_than_silently_relabelled() {
        let snapshot = captured();
        assert_eq!(
            snapshot.members()[1].role,
            "consultant",
            "capture records the canonical role"
        );

        let mut value = serde_json::to_value(&snapshot).expect("serialize");
        value["members"][1]["role"] = serde_json::json!("oracle");
        let migrated: FleetSnapshot = serde_json::from_value(value).expect("deserialize");

        assert!(migrated.verify_content_hash().is_err());
        // The alias still *resolves* — compatibility is a lookup property, not a
        // licence to accept an unverified hash.
        assert!(migrated.member_by_role("consultant").is_some());
    }

    /// Lookup canonicalization survives capture: a snapshot frozen from a Fleet
    /// saved under the old name answers to either spelling.
    #[test]
    fn snapshot_role_lookup_accepts_both_spellings() {
        let snapshot = captured();

        for spelling in ["consultant", "oracle", "advisor", "ORACLE"] {
            assert_eq!(
                snapshot
                    .member_by_role(spelling)
                    .unwrap_or_else(|| panic!("`{spelling}` must resolve"))
                    .id,
                "advisor-one"
            );
        }
        assert!(snapshot.member_by_id_or_role("oracle").is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reasoning_router::{
        LEGACY_INLINE_ROUTER_ORIGIN, REASONING_ROUTER_SERVICE_KIND, ReasoningRouterProfile,
        RouterCallReasoning,
    };
    use codewhale_protocol::fleet::ShellCeiling;

    /// A Fleet that references a saved, reusable Router profile — the shape new
    /// Fleets use.
    const EXACT: &str = r#"
name = "glm-pair"
schema = "exact"
reasoning_router = "luna-low"

[[members]]
id = "implementer"
role = "builder"
provider = "zai"
model = "glm-5"
reasoning = "auto"
permissions = "read_write"

[[members]]
id = "auditor"
provider = "zai"
model = "glm-5"
reasoning = "high"
permissions = "read_only"
"#;

    /// The prototype form, retained for compatibility.
    const LEGACY_INLINE: &str = r#"
name = "glm-pair"
schema = "exact"

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

    const LEGACY_ROLE_MAP: &str = r#"
name = "stopship"
description = "legacy roster"

[roles]
scout = "scout"
implementer = "builder"
"#;

    const LUNA: &str = r#"
name = "luna-low"
schema = "reasoning_router"
provider = "openai"
model = "gpt-5.6-luna"
call_reasoning = "low"
"#;

    fn id() -> QualifiedFleetId {
        QualifiedFleetId {
            name: "glm-pair".to_string(),
            origin: "workspace".to_string(),
        }
    }

    fn luna() -> CapturedReasoningRouter {
        let profile = ReasoningRouterProfile::parse(LUNA).expect("router profile");
        CapturedReasoningRouter::from_profile(&profile, "workspace")
    }

    fn capture(text: &str, router: Option<CapturedReasoningRouter>) -> FleetSnapshot {
        let document = FleetDocument::parse(text).expect("parse");
        FleetSnapshot::capture(id(), &document, "2026-07-26T00:00:00Z", router).expect("capture")
    }

    #[test]
    fn snapshot_captures_identity_schema_routes_and_reasoning() {
        let snapshot = capture(EXACT, Some(luna()));

        assert_eq!(snapshot.fleet().qualified(), "workspace/glm-pair");
        assert_eq!(snapshot.schema_kind(), "exact");
        assert_eq!(snapshot.schema_revision(), 1);
        assert!(snapshot.schema_hash().starts_with("sha256:"));
        assert!(snapshot.content_hash().starts_with("sha256:"));

        // Roles and ids are separate lookups, and both find the same member.
        let by_role = snapshot.member_by_role("builder").expect("role lookup");
        let by_id = snapshot.member("implementer").expect("id lookup");
        assert_eq!(by_role.id, by_id.id);
        assert_eq!(by_id.route.provider, "zai");
        assert_eq!(by_id.route.model, "glm-5");
        assert_eq!(by_id.requested_reasoning, RequestedReasoning::Auto);
        let member_json = serde_json::to_value(by_id).expect("serialize member");
        assert!(member_json.get("permissions").is_none(), "{member_json}");

        // An id lookup must not answer to a role, or a task naming one would
        // silently resolve the other.
        assert!(snapshot.member("builder").is_none());
        assert!(snapshot.member_by_role("implementer").is_none());

        assert!(snapshot.has_auto_member());
        assert_eq!(snapshot.auto_member_ids(), vec!["implementer".to_string()]);
    }

    /// The Router is a referenced service, not a Fleet member: it holds no
    /// authority, is never dispatchable, and is not in the roster.
    #[test]
    fn the_attached_router_is_a_service_and_not_a_roster_member() {
        let snapshot = capture(EXACT, Some(luna()));
        let router = snapshot.router().expect("router service");

        assert_eq!(router.service_kind, REASONING_ROUTER_SERVICE_KIND);
        assert_eq!(router.qualified(), "workspace/luna-low");
        assert!(!router.legacy_inline);
        assert!(!router.is_dispatchable());
        assert!(!router.dispatchable);
        assert!(router.tool_surface().is_empty());
        assert_eq!(router.route.provider, "openai");
        assert_eq!(router.route.model, "gpt-5.6-luna");
        assert_eq!(router.requested_call_reasoning, RouterCallReasoning::Low);
        assert_eq!(router.permissions.shell, ShellCeiling::None);
        assert!(!router.permissions.tools);
        assert_eq!(router.permissions.delegation_depth, 0);

        // Not reachable through worker lookup by either id or role.
        assert!(snapshot.member("luna-low").is_none());
        assert!(snapshot.member_by_role("luna-low").is_none());
        assert!(snapshot.member_by_id_or_role("router").is_none());
    }

    /// One saved profile, two different Fleets. The service is referenced, not
    /// owned, so both snapshots capture the identical value.
    #[test]
    fn one_router_profile_serves_two_fleets() {
        let first = capture(EXACT, Some(luna()));
        let second_text = EXACT.replace("name = \"glm-pair\"", "name = \"other-pair\"");
        let document = FleetDocument::parse(&second_text).expect("parse");
        let second = FleetSnapshot::capture(
            QualifiedFleetId {
                name: "other-pair".to_string(),
                origin: "workspace".to_string(),
            },
            &document,
            "2026-07-26T00:00:00Z",
            Some(luna()),
        )
        .expect("capture");

        assert_eq!(first.router(), second.router());
        assert_ne!(first.fleet(), second.fleet());
        assert_ne!(
            first.content_hash(),
            second.content_hash(),
            "different fleets are still different snapshots"
        );
    }

    /// The prototype inline form normalizes into the same captured service, so
    /// nothing downstream has to know which way the operator wrote it.
    #[test]
    fn a_legacy_inline_router_normalizes_into_the_same_captured_service() {
        let document = FleetDocument::parse(LEGACY_INLINE).expect("parse");
        let exact = document.exact().expect("exact");
        let captured = captured_legacy_inline_router(exact).expect("inline router");

        assert!(captured.legacy_inline);
        assert_eq!(captured.origin, LEGACY_INLINE_ROUTER_ORIGIN);
        assert_eq!(captured.service_kind, REASONING_ROUTER_SERVICE_KIND);
        assert_eq!(captured.route.model, "glm-5-turbo");
        assert_eq!(captured.requested_call_reasoning, RouterCallReasoning::Off);
        assert!(!captured.is_dispatchable());
        assert!(!captured.permissions.tools);

        let snapshot = FleetSnapshot::capture(
            id(),
            &document,
            "2026-07-26T00:00:00Z",
            Some(captured.clone()),
        )
        .expect("capture");
        assert_eq!(snapshot.router(), Some(&captured));
        // The inline member is not in the roster.
        assert!(snapshot.member("router").is_none());
        assert_eq!(snapshot.members().len(), 1);
    }

    #[test]
    fn editing_the_saved_fleet_does_not_touch_a_running_snapshot() {
        let snapshot = capture(EXACT, Some(luna()));

        // The operator edits the saved file mid-run: different model and
        // reasoning. (The historic permissions key remains ignored input.)
        let edited = EXACT
            .replace(
                "model = \"glm-5\"\nreasoning = \"auto\"",
                "model = \"glm-4\"\nreasoning = \"off\"",
            )
            .replace("permissions = \"read_write\"", "permissions = \"full\"");
        let next = capture(&edited, Some(luna()));

        // The in-flight snapshot is untouched.
        let member = snapshot.member("implementer").expect("member");
        assert_eq!(member.route.model, "glm-5");
        assert_eq!(member.requested_reasoning, RequestedReasoning::Auto);

        // The next run sees the edit, and the hashes prove they differ.
        assert_eq!(next.member("implementer").unwrap().route.model, "glm-4");
        assert_ne!(snapshot.schema_hash(), next.schema_hash());
        assert_ne!(snapshot.content_hash(), next.content_hash());
    }

    #[test]
    fn identical_definitions_produce_an_identical_content_hash() {
        let document = FleetDocument::parse(EXACT).expect("parse");
        let a = FleetSnapshot::capture(id(), &document, "2026-07-26T00:00:00Z", Some(luna()))
            .expect("capture");
        // Different capture time, same fleet: the content hash must not move.
        let b = FleetSnapshot::capture(id(), &document, "2026-07-27T09:30:00Z", Some(luna()))
            .expect("capture");

        assert_eq!(a.content_hash(), b.content_hash());
        assert_ne!(a.captured_at(), b.captured_at());
    }

    /// Swapping the attached Router is a real change to what will run, so it
    /// must move the content hash.
    #[test]
    fn changing_the_attached_router_changes_the_content_hash() {
        let with_luna = capture(EXACT, Some(luna()));
        let without = capture(EXACT, None);
        assert_ne!(with_luna.content_hash(), without.content_hash());
    }

    #[test]
    fn legacy_role_map_fleets_snapshot_as_legacy() {
        let document = FleetDocument::parse(LEGACY_ROLE_MAP).expect("parse legacy");
        let snapshot = FleetSnapshot::capture(
            QualifiedFleetId {
                name: "stopship".to_string(),
                origin: "workspace".to_string(),
            },
            &document,
            "2026-07-26T00:00:00Z",
            None,
        )
        .expect("capture");

        assert_eq!(snapshot.schema_kind(), "legacy");
        assert_eq!(snapshot.schema_revision(), 0);
        assert!(snapshot.members().is_empty());
        assert!(snapshot.router().is_none());
        assert!(!snapshot.has_auto_member());
        assert_eq!(snapshot.legacy_roles().len(), 2);
        assert!(
            snapshot
                .legacy_roles()
                .iter()
                .any(|role| role.role == "implementer" && role.profile == "builder")
        );
    }

    #[test]
    fn snapshot_serialization_carries_no_secret_shaped_fields() {
        let snapshot = capture(EXACT, Some(luna()));
        let json = serde_json::to_string(&snapshot).expect("serialize");
        let lowered = json.to_ascii_lowercase();

        for forbidden in [
            "api_key",
            "apikey",
            "secret",
            "token",
            "bearer",
            "password",
            "base_url",
            "credential",
            "authorization",
        ] {
            assert!(
                !lowered.contains(forbidden),
                "snapshot must not carry `{forbidden}`: {json}"
            );
        }

        // Round-trips as a durable value.
        let back: FleetSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, snapshot);
    }

    /// A durable snapshot identifies its definition by qualified origin/name
    /// and by hash — never by a filesystem path, which would leak the
    /// operator's home directory and username into anything that stores it.
    #[test]
    fn a_snapshot_carries_no_filesystem_path() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::create_dir_all(tmp.path().join("fleets")).expect("dirs");
        let path = tmp.path().join("fleets/glm-pair.toml");
        std::fs::write(&path, EXACT).expect("write");
        let document = FleetDocument::load(&path, Some("glm-pair")).expect("load from disk");
        // The document still knows where it came from, for local diagnostics.
        assert!(document.source_path().is_some());

        let snapshot =
            FleetSnapshot::capture(id(), &document, "2026-07-26T00:00:00Z", Some(luna()))
                .expect("capture");
        let json = serde_json::to_string(&snapshot).expect("serialize");

        assert!(!json.contains(&tmp.path().display().to_string()), "{json}");
        for fragment in ["/Users/", "/home/", "/private/", ".toml", "\\Users\\"] {
            assert!(
                !json.contains(fragment),
                "snapshot must not carry `{fragment}`: {json}"
            );
        }
        assert_eq!(snapshot.fleet().qualified(), "workspace/glm-pair");
        assert!(snapshot.content_hash().starts_with("sha256:"));
    }

    /// Capture is the last gate before a roster becomes a running Workflow, so
    /// a value that never saw the TOML parser must still be rejected here.
    #[test]
    fn capture_revalidates_a_roster_that_bypassed_the_parser() {
        use crate::fleet_exact::ExactMember;

        let member = |id: &str, role: &str| ExactMember {
            id: id.to_string(),
            role: role.to_string(),
            provider: "zai".to_string(),
            model: "glm-5".to_string(),
            reasoning: RequestedReasoning::Off,
        };
        let smuggled = ExactFleet {
            name: "f".to_string(),
            description: None,
            schema_revision: 1,
            reasoning_router: None,
            // Two members, one role: role lookup would resolve by list order.
            members: vec![member("a", "builder"), member("b", "builder")],
            router: None,
        };

        let document = FleetDocument::from_exact_for_tests(smuggled);
        let err = FleetSnapshot::capture(id(), &document, "2026-07-26T00:00:00Z", None)
            .expect_err("capture must revalidate");
        assert!(
            matches!(err, ExactFleetError::DuplicateRole { .. }),
            "{err:?}"
        );
    }
}
