//! Core work-graph data model.
//!
//! One graph carries plan, todo, operations, evidence, and approvals; every
//! user-visible projection derives from it and never writes back. The model is
//! plain data — no threads, no service objects — mutated only through the
//! reducer in [`super::reducer`].
//!
//! Design notes where the cutover spec is silent:
//! - Timestamps are a plain `i64` of milliseconds since the Unix epoch
//!   ([`Ts`]); the reducer never reads clocks, so callers supply them.
//! - [`WorkEdge`] is `{id, kind, from, to}` — the minimal directed labeled
//!   edge. `Contains` points parent → child; `Verifies` points evidence →
//!   verified node; `Supersedes` points replacement → superseded.
//! - Evidence payloads live on Evidence-kind nodes via [`WorkNode::evidence`];
//!   verification checks walk `Verifies` edges to those nodes.
//! - [`BoundedVec`] / [`BoundedSet`] are small deterministic FIFO containers
//!   (oldest entry evicted first); no hashing, so iteration order is stable.

use codewhale_models::reasoning_effort_tier::ReasoningEffortTier;
use serde::{Deserialize, Deserializer, Serialize};

use codewhale_config::ProviderKind;

use super::events::{ChangeReceipt, ObservationSummary, WorkGraphProposal};
use super::ids::{BindingId, WorkEdgeId, WorkNodeId};

/// Milliseconds since the Unix epoch (UTC). Supplied by callers via
/// [`super::ChangeCtx`]; the reducer never reads clocks itself.
pub type Ts = i64;

/// Current snapshot schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// Bounded change-history window kept on the snapshot.
pub const HISTORY_CAP: usize = 256;

/// Bounded user-visible configuration activity kept on the snapshot.
pub const ACTIVITY_CAP: usize = 256;

/// Bounded idempotency-key dedup window kept on the snapshot.
pub const SEEN_KEYS_CAP: usize = 1024;

/// Ended, non-durable Operation nodes (one per finished shell call) kept on
/// the snapshot (#6842). They are a derived index — the calls and their output
/// live in the session transcript — so older ones are evicted, not archived.
pub const ENDED_OPERATION_CAP: usize = 256;

/// Bounded, receipt-only activity attached to the session graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkActivityEvent {
    ReasoningEffortChanged {
        requested: ReasoningEffortTier,
        effective: ReasoningEffortTier,
        /// Immutable routing kind, distinct from the exact provider identity.
        /// A custom table may legally use a built-in slug as its identity.
        provider_kind: Option<ProviderKind>,
        provider: String,
        endpoint_identity: Option<String>,
        model: Option<String>,
        ts: Ts,
        operation: Option<WorkNodeId>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WorkActivityEventWire {
    ReasoningEffortChanged {
        requested: ReasoningEffortTier,
        effective: ReasoningEffortTier,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_kind: Option<String>,
        provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint_identity: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        ts: Ts,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation: Option<WorkNodeId>,
    },
}

impl Serialize for WorkActivityEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::ReasoningEffortChanged {
                requested,
                effective,
                provider_kind,
                provider,
                endpoint_identity,
                model,
                ts,
                operation,
            } => {
                let provider_kind = provider_kind
                    .map(|kind| {
                        codewhale_config::descriptors::tui_wire_tag_for_route(kind, provider)
                            .map(str::to_string)
                            .ok_or_else(|| {
                                serde::ser::Error::custom(
                                    "contradictory activity provider identity",
                                )
                            })
                    })
                    .transpose()?;
                WorkActivityEventWire::ReasoningEffortChanged {
                    requested: *requested,
                    effective: *effective,
                    provider_kind,
                    provider: provider.clone(),
                    endpoint_identity: endpoint_identity.clone(),
                    model: model.clone(),
                    ts: *ts,
                    operation: operation.clone(),
                }
                .serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for WorkActivityEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match WorkActivityEventWire::deserialize(deserializer)? {
            WorkActivityEventWire::ReasoningEffortChanged {
                requested,
                mut effective,
                provider_kind,
                provider,
                endpoint_identity,
                model,
                ts,
                operation,
            } => {
                let provider_kind = provider_kind
                    .map(|tag| {
                        codewhale_config::descriptors::kind_from_tui_wire_tag(&tag, &provider)
                            .ok_or_else(|| {
                                serde::de::Error::custom("contradictory activity provider identity")
                            })
                    })
                    .transpose()?;
                // Pre-provenance snapshots cannot prove what route received
                // the control. Keep them loadable, but never preserve a
                // claimed effective tier as if kind/endpoint/model were known.
                // In particular, reparsing `provider` is unsafe because a
                // custom table may legally be named after a built-in slug.
                if provider_kind.is_none() || endpoint_identity.is_none() || model.is_none() {
                    effective = ReasoningEffortTier::Unavailable;
                }
                Ok(Self::ReasoningEffortChanged {
                    requested,
                    effective,
                    provider_kind,
                    provider,
                    endpoint_identity,
                    model,
                    ts,
                    operation,
                })
            }
        }
    }
}

/// Return the only valid effective receipt for routes whose reasoning-control
/// dialect is narrower than the generic provider normalization.
#[must_use]
pub fn constrained_effective_reasoning_for_route(
    requested: ReasoningEffortTier,
    provider: ProviderKind,
    endpoint_identity: &str,
    model: &str,
) -> Option<ReasoningEffortTier> {
    use ReasoningEffortTier::{
        Auto, High, Low, Medium, Off, ThinkingEnabledGranularityUnavailable, Unavailable,
    };

    if provider == ProviderKind::Zai {
        if !codewhale_runtime::config::is_exact_zai_chat_route(provider, endpoint_identity) {
            return Some(Unavailable);
        }
        if codewhale_runtime::config::is_exact_zai_forced_thinking_route(
            provider,
            endpoint_identity,
            model,
        ) {
            // GLM-5.3 / GLM-5.3-Flash cannot disable thinking; the wire sends
            // `off` as the lowest documented tier and honours `low` natively.
            return Some(match requested {
                Off => Low,
                Medium => High,
                other => other,
            });
        }
        if codewhale_runtime::config::is_exact_zai_tiered_effort_route(
            provider,
            endpoint_identity,
            model,
        ) {
            return Some(match requested {
                Low | Medium => High,
                other => other,
            });
        }
        if codewhale_runtime::config::is_exact_known_zai_reasoning_route(
            provider,
            endpoint_identity,
            model,
        ) {
            return Some(match requested {
                Off | Auto => requested,
                _ => ThinkingEnabledGranularityUnavailable,
            });
        }
        return Some(Unavailable);
    }

    if provider == ProviderKind::Minimax {
        if codewhale_runtime::config::is_exact_minimax_m3_route(provider, endpoint_identity, model)
        {
            return Some(match requested {
                Off | Auto => requested,
                _ => ThinkingEnabledGranularityUnavailable,
            });
        }
        return Some(Unavailable);
    }

    if provider == ProviderKind::MinimaxAnthropic {
        if codewhale_runtime::config::is_exact_minimax_anthropic_m3_route(
            provider,
            endpoint_identity,
            model,
        ) {
            return Some(match requested {
                Off | Auto => requested,
                _ => ThinkingEnabledGranularityUnavailable,
            });
        }
        return Some(Unavailable);
    }

    // A named OpenAI-compatible endpoint has no verified reasoning dialect
    // merely because it has a bounded URL and model string. Until immutable
    // route provenance carries a validated capability contract, its effective
    // tier must remain unavailable.
    if provider == ProviderKind::Custom {
        return Some(Unavailable);
    }

    if codewhale_runtime::config::is_exact_kimi_code_k3_route(provider, endpoint_identity, model) {
        return Some(match requested {
            Off => Low,
            other => other,
        });
    }
    if codewhale_runtime::config::is_exact_direct_moonshot_k3_route(
        provider,
        endpoint_identity,
        model,
    ) {
        return Some(match requested {
            Off => Low,
            Medium => High,
            other => other,
        });
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Objective,
    PlanStep,
    Operation,
    Evidence,
    Blocker,
    Approval,
    RuntimeRef,
    LaneRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Contains,
    DependsOn,
    Blocks,
    Produces,
    Verifies,
    RunsOn,
    RequiresApproval,
    Supersedes,
}

/// Node lifecycle state. The load-bearing distinction: [`NodeState::Completed`]
/// means an operation *ended*; only [`NodeState::Verified`] — reachable solely
/// when an evidence path satisfies every acceptance requirement — means done.
/// An ended process is never proof its acceptance criteria hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    Ready,
    /// The owner has accepted spawn intent but has not yet reported a live
    /// handle. Registering this state before process creation prevents work
    /// from existing outside the graph during the spawn window.
    Initializing,
    Active,
    Waiting,
    Blocked,
    /// Operation ended — NOT done.
    Completed,
    /// Evidence path satisfies acceptance — this is "done".
    Verified,
    /// The owner can no longer confirm the process (distinct from
    /// silent-but-live; a confirmed-live silent job stays `Active`).
    Stale,
    Superseded,
    Cancelled,
    Failed,
}

impl NodeState {
    /// Terminal states protected by invariant V9: never overwritten except
    /// via an explicit `Supersede` change or a reconcile-rule change.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            NodeState::Verified | NodeState::Superseded | NodeState::Cancelled
        )
    }

    /// Live states for invariant V2 (no orphaned live work).
    #[must_use]
    pub fn is_live(self) -> bool {
        matches!(
            self,
            NodeState::Initializing | NodeState::Active | NodeState::Waiting
        )
    }
}

/// Fieldless discriminant of [`EvidenceKind`], used by acceptance
/// requirements so they can match a kind without naming payload values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKindTag {
    ToolRun,
    Artifact,
    TestSummary,
    Receipt,
    Approval,
    Route,
    WebCitation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    ToolRun,
    Artifact {
        digest: String,
    },
    TestSummary,
    Receipt {
        owner: String,
    },
    Approval,
    Route,
    WebCitation {
        ref_id: String,
        url: String,
        retrieved_at: String,
    },
}

impl EvidenceKind {
    #[must_use]
    pub fn tag(&self) -> EvidenceKindTag {
        match self {
            EvidenceKind::ToolRun => EvidenceKindTag::ToolRun,
            EvidenceKind::Artifact { .. } => EvidenceKindTag::Artifact,
            EvidenceKind::TestSummary => EvidenceKindTag::TestSummary,
            EvidenceKind::Receipt { .. } => EvidenceKindTag::Receipt,
            EvidenceKind::Approval => EvidenceKindTag::Approval,
            EvidenceKind::Route => EvidenceKindTag::Route,
            EvidenceKind::WebCitation { .. } => EvidenceKindTag::WebCitation,
        }
    }
}

/// Reason an [`EvidenceRef`] could not be constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceRefError {
    EmptyReference,
    ReferenceTooLong { len: usize },
    AbsolutePath,
    HomeRelativePath,
    ContainsWhitespaceOrControl,
    LooksLikeKeyMaterial,
    WebCitationReferenceMismatch,
    InvalidWebCitationUrl,
    InvalidWebCitationTimestamp,
}

impl std::fmt::Display for EvidenceRefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvidenceRefError::EmptyReference => write!(f, "evidence reference is empty"),
            EvidenceRefError::ReferenceTooLong { len } => {
                write!(f, "evidence reference too long ({len} chars)")
            }
            EvidenceRefError::AbsolutePath => {
                write!(f, "evidence reference must not be an absolute path")
            }
            EvidenceRefError::HomeRelativePath => {
                write!(f, "evidence reference must not be a home-relative path")
            }
            EvidenceRefError::ContainsWhitespaceOrControl => {
                write!(
                    f,
                    "evidence reference must not contain whitespace or control chars"
                )
            }
            EvidenceRefError::LooksLikeKeyMaterial => {
                write!(f, "evidence reference must not embed key material")
            }
            EvidenceRefError::WebCitationReferenceMismatch => {
                write!(f, "web citation reference must match its ref_id")
            }
            EvidenceRefError::InvalidWebCitationUrl => {
                write!(f, "web citation URL must be HTTP(S) without credentials")
            }
            EvidenceRefError::InvalidWebCitationTimestamp => {
                write!(f, "web citation retrieved_at must be RFC 3339")
            }
        }
    }
}

impl std::error::Error for EvidenceRefError {}

const EVIDENCE_REFERENCE_MAX_LEN: usize = 512;

fn web_citation_url_has_sensitive_query(url: &reqwest::Url) -> bool {
    url.query_pairs().any(|(name, _)| {
        let name = name.to_ascii_lowercase();
        matches!(
            name.as_ref(),
            "access_token"
                | "api_key"
                | "authorization"
                | "auth"
                | "credential"
                | "key"
                | "session"
                | "session_id"
                | "sig"
                | "signature"
                | "token"
                | "x-amz-credential"
                | "x-amz-signature"
                | "x-goog-credential"
                | "x-goog-signature"
        ) || name.ends_with("_token")
            || name.ends_with("_key")
    })
}

/// Summary/reference-only pointer to evidence: a logical artifact ID, run ID,
/// or receipt handle — never absolute paths, never secrets, never raw logs or
/// reasoning text.
///
/// Enforced by construction where feasible: fields are private, [`Self::new`]
/// is the only way to build one (serde routes through it via `try_from`), and
/// it rejects absolute/home paths, whitespace/control characters (which also
/// blocks pasted log or prose content), and PEM-style key-material markers.
/// `raw_bytes` records the pre-truncation size of the underlying output so
/// "still growing" and "stuck" stay distinguishable after truncation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "EvidenceRefRaw", into = "EvidenceRefRaw")]
pub struct EvidenceRef {
    kind: EvidenceKind,
    reference: String,
    raw_bytes: Option<u64>,
    truncated: bool,
}

impl EvidenceRef {
    pub fn new(
        kind: EvidenceKind,
        reference: impl Into<String>,
        raw_bytes: Option<u64>,
        truncated: bool,
    ) -> Result<Self, EvidenceRefError> {
        let reference = reference.into();
        if reference.is_empty() {
            return Err(EvidenceRefError::EmptyReference);
        }
        if reference.chars().count() > EVIDENCE_REFERENCE_MAX_LEN {
            return Err(EvidenceRefError::ReferenceTooLong {
                len: reference.chars().count(),
            });
        }
        let mut chars = reference.chars();
        let first = chars.next().unwrap_or('\0');
        // Unix absolute, UNC/backslash, or `X:/`-style drive paths.
        let drive_absolute = {
            let bytes = reference.as_bytes();
            bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && (bytes[2] == b'/' || bytes[2] == b'\\')
        };
        if first == '/' || first == '\\' || drive_absolute {
            return Err(EvidenceRefError::AbsolutePath);
        }
        if first == '~' {
            return Err(EvidenceRefError::HomeRelativePath);
        }
        if reference
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(EvidenceRefError::ContainsWhitespaceOrControl);
        }
        if reference.contains("-----BEGIN") {
            return Err(EvidenceRefError::LooksLikeKeyMaterial);
        }
        if let EvidenceKind::WebCitation {
            ref_id,
            url,
            retrieved_at,
        } = &kind
        {
            if ref_id != &reference {
                return Err(EvidenceRefError::WebCitationReferenceMismatch);
            }
            let parsed = reqwest::Url::parse(url)
                .ok()
                .filter(|url| matches!(url.scheme(), "http" | "https"))
                .filter(|url| url.host_str().is_some())
                .filter(|url| url.username().is_empty() && url.password().is_none())
                .filter(|url| !web_citation_url_has_sensitive_query(url));
            if parsed.is_none() {
                return Err(EvidenceRefError::InvalidWebCitationUrl);
            }
            if chrono::DateTime::parse_from_rfc3339(retrieved_at).is_err() {
                return Err(EvidenceRefError::InvalidWebCitationTimestamp);
            }
        }
        Ok(Self {
            kind,
            reference,
            raw_bytes,
            truncated,
        })
    }

    #[must_use]
    pub fn kind(&self) -> &EvidenceKind {
        &self.kind
    }

    #[must_use]
    pub fn reference(&self) -> &str {
        &self.reference
    }

    /// Pre-truncation size of the underlying output, persisted on the node.
    #[must_use]
    pub fn raw_bytes(&self) -> Option<u64> {
        self.raw_bytes
    }

    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Serde shadow for [`EvidenceRef`] so deserialization re-runs constructor
/// validation instead of bypassing it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRefRaw {
    kind: EvidenceKind,
    reference: String,
    raw_bytes: Option<u64>,
    truncated: bool,
}

impl TryFrom<EvidenceRefRaw> for EvidenceRef {
    type Error = EvidenceRefError;

    fn try_from(raw: EvidenceRefRaw) -> Result<Self, Self::Error> {
        EvidenceRef::new(raw.kind, raw.reference, raw.raw_bytes, raw.truncated)
    }
}

impl From<EvidenceRef> for EvidenceRefRaw {
    fn from(value: EvidenceRef) -> Self {
        EvidenceRefRaw {
            kind: value.kind,
            reference: value.reference,
            raw_bytes: value.raw_bytes,
            truncated: value.truncated,
        }
    }
}

/// A requirement that must be satisfied by evidence before a node may be
/// [`NodeState::Verified`] (invariant V4).
///
/// Deliberately minimal for this slice: one variant matching an evidence kind.
/// Richer predicates (thresholds, specific commands) can be added as variants
/// without touching the verification walk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceRequirement {
    /// Satisfied when at least one attached evidence item has this kind.
    EvidenceOfKind { kind: EvidenceKindTag },
}

impl AcceptanceRequirement {
    #[must_use]
    pub fn is_satisfied_by(&self, evidence: &EvidenceRef) -> bool {
        match self {
            AcceptanceRequirement::EvidenceOfKind { kind } => evidence.kind().tag() == *kind,
        }
    }
}

/// Where a fact in the graph came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Import {
        source_digest: String,
        ordinal: Option<u32>,
    },
    ToolUpdate {
        tool: String,
        call_id: String,
    },
    RuntimeReconcile {
        source: String,
        observed_at: Ts,
    },
    UserEdit {
        proposal_id: super::ids::ProposalId,
    },
}

/// Binding from an Operation node to the external process that owns its
/// lifecycle. `external` uses the existing identity scheme verbatim:
/// `"task:{id}" | "shell:{id}" | "worker:{id}" | "workflow:{id}" |
/// "fleet:{run}/{task}" | "lane:{id}"` — the same strings the live work
/// surface already parses for actions, so bindings stay joinable with
/// today's owners without translation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationBinding {
    pub external: String,
    /// Whether the owner persists lifecycle records across restart. Shell
    /// sessions are in-memory only (`durable == false`): after a restart they
    /// become [`NodeState::Stale`], never silently "still running".
    pub durable: bool,
    #[serde(default)]
    pub last_observation: Option<ObservationSummary>,
}

/// Returns true when `external` is well-formed under exactly one prefix of
/// the existing identity scheme.
#[must_use]
pub fn external_identity_is_well_formed(external: &str) -> bool {
    fn plain(id: &str) -> bool {
        !id.is_empty() && !id.chars().any(|c| c.is_whitespace() || c.is_control())
    }
    if let Some(rest) = external.strip_prefix("fleet:") {
        return match rest.split_once('/') {
            Some((run, task)) => plain(run) && plain(task),
            None => false,
        };
    }
    ["task:", "shell:", "worker:", "workflow:", "lane:"]
        .iter()
        .any(|prefix| external.strip_prefix(prefix).is_some_and(plain))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkNode {
    pub id: WorkNodeId,
    pub kind: NodeKind,
    pub title: String,
    pub state: NodeState,
    /// Empty acceptance means [`NodeState::Completed`] may render as done;
    /// non-empty acceptance makes `Verified` (evidence-gated) the only done.
    pub acceptance: Vec<AcceptanceRequirement>,
    /// Operation nodes only (invariant V3).
    pub binding: Option<OperationBinding>,
    /// Evidence-kind nodes only; the payload the `Verifies` walk reads.
    pub evidence: Option<EvidenceRef>,
    pub provenance: Provenance,
    pub created_at: Ts,
    pub updated_at: Ts,
}

/// Directed labeled edge. Minimal by design; edge-level metadata can be
/// modeled as nodes (e.g. Approval) rather than edge payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkEdge {
    pub id: WorkEdgeId,
    pub kind: EdgeKind,
    pub from: WorkNodeId,
    pub to: WorkNodeId,
}

/// Graph-owned presentation metadata for the legacy Strategy/Plan surface.
///
/// Plan steps themselves live as `PlanStep` nodes. These fields have no
/// first-class node equivalent yet, so they remain attached to the graph as
/// presentation metadata rather than living in a separately writable store.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatPlanMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources_used: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub critical_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_approach: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risks_and_unknowns: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_packet: Option<String>,
}

impl CompatPlanMetadata {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.objective.is_none()
            && self.context_summary.is_none()
            && self.explanation.is_none()
            && self.sources_used.is_empty()
            && self.critical_files.is_empty()
            && self.constraints.is_empty()
            && self.recommended_approach.is_none()
            && self.verification_plan.is_none()
            && self.risks_and_unknowns.is_none()
            && self.handoff_packet.is_none()
    }
}

/// One row in the legacy To-do projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatTodoBinding {
    pub legacy_id: u32,
    pub node: WorkNodeId,
    /// When present, the legacy row aliases this ordinal in `plan_order`.
    /// The retired invisible marker is never reconstructed; the graph keeps
    /// the provenance explicitly while old readers receive clean content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_index: Option<u32>,
}

/// Ordering and presentation state needed to derive old Plan/To-do snapshots.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatProjectionState {
    #[serde(default, skip_serializing_if = "CompatPlanMetadata::is_empty")]
    pub plan: CompatPlanMetadata,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plan_order: Vec<WorkNodeId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todos: Vec<CompatTodoBinding>,
}

impl CompatProjectionState {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plan.is_empty() && self.plan_order.is_empty() && self.todos.is_empty()
    }

    /// Whether `node` is still part of the live Plan or To-do projection.
    /// Replacing either list drops bindings but keeps the old plan-step nodes
    /// in the graph (there is no node removal), so presentation must treat an
    /// unreferenced plan step as retired, not as live work (#6546).
    #[must_use]
    pub fn projects(&self, node: &WorkNodeId) -> bool {
        self.plan_order.contains(node) || self.todos.iter().any(|binding| &binding.node == node)
    }
}

/// Idempotency key for owner-reported observations: `(binding, seq)`. Applied
/// changes carrying a key already inside the snapshot's dedup window become
/// receipts without effect, so replayed runtime events cannot double-apply.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct IdempotencyKey {
    pub binding: BindingId,
    pub seq: u64,
}

/// Deterministic FIFO vector bounded at `N`: pushing beyond capacity evicts
/// the oldest entry. Kept as a plain `Vec` so ordering (and serialization) is
/// stable. The bound is re-checked by validation (V8), so an oversized
/// deserialized snapshot fails closed rather than growing unbounded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BoundedVec<T, const N: usize> {
    items: Vec<T>,
}

impl<T, const N: usize> BoundedVec<T, N> {
    #[must_use]
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn push_bounded(&mut self, item: T) {
        if self.items.len() >= N {
            self.items.remove(0);
        }
        self.items.push(item);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub fn last(&self) -> Option<&T> {
        self.items.last()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.items.iter()
    }

    #[must_use]
    pub const fn capacity() -> usize {
        N
    }
}

impl<T, const N: usize> Default for BoundedVec<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Deterministic FIFO set bounded at `N`: inserting a duplicate is a no-op;
/// inserting beyond capacity evicts the oldest member. Linear scans keep it
/// hash-free and iteration-order stable for reproducible serialization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BoundedSet<T, const N: usize> {
    items: Vec<T>,
}

impl<T: PartialEq, const N: usize> BoundedSet<T, N> {
    #[must_use]
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    #[must_use]
    pub fn contains(&self, item: &T) -> bool {
        self.items.contains(item)
    }

    /// Returns true if the item was newly inserted.
    pub fn insert(&mut self, item: T) -> bool {
        if self.contains(&item) {
            return false;
        }
        if self.items.len() >= N {
            self.items.remove(0);
        }
        self.items.push(item);
        true
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

impl<T: PartialEq, const N: usize> Default for BoundedSet<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

/// The whole graph as a value. Serialized opaquely inside session state by a
/// later slice; this slice keeps it standalone.
///
/// `proposals` is a spec-silent addition: pending plan-diff proposals must
/// live somewhere the reducer can find them when `AcceptPlanDiff` arrives by
/// ID, and the snapshot is the only state the reducer sees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkGraphSnapshot {
    pub schema: u32,
    pub revision: u64,
    pub nodes: Vec<WorkNode>,
    pub edges: Vec<WorkEdge>,
    pub history: BoundedVec<ChangeReceipt, HISTORY_CAP>,
    /// Configuration facts only; never prompts, output, or reasoning text.
    #[serde(default, skip_serializing_if = "BoundedVec::is_empty")]
    pub activities: BoundedVec<WorkActivityEvent, ACTIVITY_CAP>,
    pub import_digest: Option<String>,
    /// `(binding, seq)` dedup window for replayed runtime observations.
    pub seen_keys: BoundedSet<IdempotencyKey, SEEN_KEYS_CAP>,
    pub proposals: Vec<WorkGraphProposal>,
    /// Graph-owned inputs for the fully populated legacy Plan/To-do views.
    #[serde(default, skip_serializing_if = "CompatProjectionState::is_empty")]
    pub compat: CompatProjectionState,
}

impl WorkGraphSnapshot {
    #[must_use]
    pub fn new() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            revision: 0,
            nodes: Vec::new(),
            edges: Vec::new(),
            history: BoundedVec::new(),
            activities: BoundedVec::new(),
            import_digest: None,
            seen_keys: BoundedSet::new(),
            proposals: Vec::new(),
            compat: CompatProjectionState::default(),
        }
    }

    #[must_use]
    pub fn node(&self, id: &WorkNodeId) -> Option<&WorkNode> {
        self.nodes.iter().find(|n| &n.id == id)
    }

    pub(super) fn node_mut(&mut self, id: &WorkNodeId) -> Option<&mut WorkNode> {
        self.nodes.iter_mut().find(|n| &n.id == id)
    }

    #[must_use]
    pub fn edge(&self, id: &WorkEdgeId) -> Option<&WorkEdge> {
        self.edges.iter().find(|e| &e.id == id)
    }

    /// "Done" for dependency/approval purposes: verified, or completed with
    /// no acceptance requirements (nothing left to verify).
    #[must_use]
    pub fn node_is_done(node: &WorkNode) -> bool {
        matches!(node.state, NodeState::Verified)
            || (matches!(node.state, NodeState::Completed) && node.acceptance.is_empty())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
            && self.edges.is_empty()
            && self.history.is_empty()
            && self.activities.is_empty()
            && self.import_digest.is_none()
            && self.proposals.is_empty()
            && self.compat.is_empty()
    }
}

impl Default for WorkGraphSnapshot {
    fn default() -> Self {
        Self::new()
    }
}
