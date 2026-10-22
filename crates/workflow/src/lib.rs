//! Typed Workflow IR and validation for CodeWhale.
//!
//! This crate deliberately stops at the Rust-owned IR boundary. Runtime tool
//! exposure, worktree application, replay, and model execution are layered on
//! top only after their cancellation and evidence semantics are proven.

mod elevation;
/// Setup-time Fleet composition: a suggestion schema with no runtime authority.
///
/// Deliberately **not** re-exported from the crate root. The setup wizard uses
/// it by its explicit module path
/// (`codewhale_workflow::fleet_composition::…`), keeping the advisory boundary
/// visible instead of making the schema look like runtime Fleet authority.
pub mod fleet_composition;
pub mod fleet_exact;
pub mod fleet_preflight;
pub mod fleet_reasoning;
pub mod fleet_snapshot;
mod gates;
mod js_authoring;
mod model_policy;
mod named_fleet;
pub mod reasoning_router;
pub mod redaction;
mod role_resolve;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use elevation::{
    DEFAULT_HIGH_BUDGET_THRESHOLD, ElevationOptions, PlanRiskHint, WorkflowPlanElevation,
    assess_plan_risk_string, assess_workflow_elevation, is_shell_tool, is_write_tool,
};
pub use fleet_exact::{
    EXACT_FLEET_SCHEMA_KIND, EXACT_FLEET_SCHEMA_REVISION, ExactFleet, ExactFleetError, ExactMember,
    FrozenRoute, LEGACY_FLEET_SCHEMA_KIND, PermissionCeiling, ROLE_ALIASES, ROUTER_PUBLIC_ID,
    ROUTER_PUBLIC_ROLE, ReasoningTier, RequestedReasoning, RouterMember, ShellCeiling,
    canonical_member_key, canonical_role_key,
};
pub use fleet_preflight::{
    CredentialReadiness, EndpointIdentity, PreflightError, PreflightedRoute, RoutePreflight,
};
pub use fleet_reasoning::{
    EffectiveReasoning, EffectiveReasoningSource, FAITHFUL_WIRE_TIERS, FleetTaskReceipt,
    ProviderEffectiveReasoning, ProviderReasoningControl, ROUTER_CALL_REASONING,
    ROUTER_MAX_OUTPUT_TOKENS, ROUTER_REASONING_FIELD, ROUTER_SUMMARY_MAX_CHARS, ROUTING_SCOPE,
    ReasoningCapability, ReasoningResolveError, ResolvedReasoning, RouterAvailability,
    RouterCallDisclosure, RouterCallInput, RouterCallPlan, RouterDecision, RouterDecisionError,
    RouterIdentity, RoutingDisclosure, RoutingPayload, TaskShape, bounded_routing_payload,
    parse_router_decision, resolve_exact_member_reasoning, resolve_legacy_reasoning,
    router_call_plan, router_system_prompt, router_user_message, transport_disclosure,
};
pub use fleet_snapshot::{
    FleetSnapshot, FleetSnapshotLegacyRole, FleetSnapshotMember, FleetSnapshotRouter,
    QualifiedFleetId, captured_legacy_inline_router, verify_snapshot_content_hash,
};
pub use gates::{
    GateError, GateKind, GateOn, GateOnFail, GateOutcome, GateSpec, GateState, GateStatusLine,
    HandoffArtifact, LaneGateBoard, stopship_gate_pipeline,
};
pub use js_authoring::{
    JavascriptWorkflowError, JavascriptWorkflowResult, compile_javascript_workflow,
    compile_typescript_workflow,
};
pub use model_policy::*;
pub use named_fleet::{
    FleetDocument, FleetSchema, FleetSearchRoot, NamedFleet, NamedFleetError,
    STOPSHIP_REQUIRED_ROLES, exact_schema_revision, load_named_fleet, load_named_fleet_file,
    parse_named_fleet, split_qualified_fleet_name, validate_fleet_file_stem,
};
pub use reasoning_router::{
    CapturedReasoningRouter, FleetRouterRef, LEGACY_INLINE_ROUTER_ORIGIN, QualifiedRouterId,
    REASONING_ROUTER_DIR, REASONING_ROUTER_SCHEMA_KIND, REASONING_ROUTER_SERVICE_KIND,
    ReasoningRouterError, ReasoningRouterProfile, RouterCallReasoning,
};
pub use redaction::{
    REDACTION_ABSOLUTE_PATH, REDACTION_RELATIVE_PATH, REDACTION_SECRET, Redaction,
    redact_for_disclosure,
};
pub use role_resolve::{
    FleetRoleMap, FleetRoleResolveError, ResolvedWorkflowAgent, normalize_token,
    resolve_workflow_agent, validate_role_token,
};

/// Default hard ceiling on total agents a Fleet-shaped Workflow plan may launch.
/// Matches the imperative VM lifetime cap (1_000 agents per run).
pub const DEFAULT_FLEET_WORKFLOW_MAX_AGENTS: usize = 1000;
pub const DEFAULT_FLEET_WORKFLOW_MAX_DEPTH: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowConfig {
    pub goal: String,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u8,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub phases: Vec<Phase>,
}

impl WorkflowConfig {
    pub fn validate(&self) -> Result<(), WorkflowValidationError> {
        WorkflowPlan::from_config(self).map(|_| ())
    }

    pub fn compile(&self) -> Result<WorkflowPlan, WorkflowValidationError> {
        WorkflowPlan::from_config(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowSpec {
    #[serde(default)]
    pub id: Option<String>,
    pub goal: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub budget: BudgetSpec,
    #[serde(default)]
    pub permissions: PermissionSpec,
    #[serde(default)]
    pub model_policy: ModelPolicy,
    #[serde(default)]
    pub promotion_policy: PromotionPolicy,
    /// Workflow-owned role gates and handoffs (#4179). Fleet supplies roles;
    /// gate semantics stay attached to the Workflow definition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<GateSpec>,
    #[serde(default)]
    pub nodes: Vec<WorkflowNode>,
}

impl WorkflowSpec {
    pub fn validate_for_fleet(&self) -> Result<WorkflowFleetShape, WorkflowFleetLimitError> {
        self.validate_for_fleet_with_limits(WorkflowFleetLimits::default())
    }

    pub fn validate_for_fleet_with_limits(
        &self,
        limits: WorkflowFleetLimits,
    ) -> Result<WorkflowFleetShape, WorkflowFleetLimitError> {
        validate_workflow_nodes(&self.nodes)
            .map_err(|source| WorkflowFleetLimitError::InvalidWorkflow { source })?;
        let shape = estimate_fleet_shape(&self.nodes)?;
        if shape.total_agents > limits.max_total_agents {
            return Err(WorkflowFleetLimitError::TooManyAgents {
                total_agents: shape.total_agents,
                max_total_agents: limits.max_total_agents,
            });
        }
        if shape.max_depth > limits.max_depth {
            return Err(WorkflowFleetLimitError::RecursionTooDeep {
                depth: shape.max_depth,
                max_depth: limits.max_depth,
            });
        }
        Ok(shape)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "spec", rename_all = "snake_case")]
pub enum WorkflowNode {
    BranchSet(BranchSpec),
    Leaf(LeafSpec),
    Sequence(SequenceSpec),
    Reduce(ReduceSpec),
    TeacherReview(TeacherReviewSpec),
    LoopUntil(LoopUntilSpec),
    Cond(CondSpec),
    Expand(ExpandSpec),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchSpec {
    pub id: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parallel: bool,
    #[serde(default)]
    pub budget: BudgetSpec,
    #[serde(default)]
    pub permissions: PermissionSpec,
    #[serde(default)]
    pub model_policy: ModelPolicy,
    #[serde(default)]
    pub children: Vec<WorkflowNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeafSpec {
    pub id: String,
    pub prompt: String,
    #[serde(default)]
    pub agent_type: AgentType,
    /// Fleet role this step should run as (e.g. `scout`, `implementer`).
    /// Resolved via the fleet roster at dispatch time (#4177). Preferred
    /// identity field for workflow steps; `profile` remains an explicit
    /// AgentProfile override. Provider/model live on [`ModelPolicy`] as
    /// optional overrides — they are **not** required identity fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Named Fleet roster profile this agent should run as. Resolved against
    /// the saved Fleet roster at dispatch time; unknown names fail validation
    /// before any spawn. When set, role/model/loadout defaults come from the
    /// roster member; explicit fields on this spec override the profile.
    /// Precedence over `role` when both are set (#4177 / #4111).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default)]
    pub mode: TaskMode,
    #[serde(default)]
    pub isolation: IsolationMode,
    #[serde(default)]
    pub file_scope: Vec<String>,
    /// Optional child working directory, repository-relative like
    /// `task({cwd})`. Disambiguates multi-repo workspaces (#6232).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub depends_on_results: Vec<String>,
    #[serde(default)]
    pub budget: BudgetSpec,
    #[serde(default)]
    pub permissions: PermissionSpec,
    #[serde(default)]
    pub model_policy: ModelPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceSpec {
    pub id: String,
    #[serde(default)]
    pub children: Vec<WorkflowNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReduceSpec {
    pub id: String,
    #[serde(default)]
    pub inputs: Vec<String>,
    pub prompt: String,
    #[serde(default)]
    pub model_policy: ModelPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeacherReviewSpec {
    pub id: String,
    #[serde(default)]
    pub candidates: Vec<String>,
    #[serde(default)]
    pub promotion_policy: PromotionPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopUntilSpec {
    pub id: String,
    pub condition: String,
    #[serde(default)]
    pub max_iterations: Option<u32>,
    #[serde(default)]
    pub children: Vec<WorkflowNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CondSpec {
    pub id: String,
    pub condition: String,
    #[serde(default)]
    pub then_nodes: Vec<WorkflowNode>,
    #[serde(default)]
    pub else_nodes: Vec<WorkflowNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpandSpec {
    pub id: String,
    pub source: String,
    #[serde(default)]
    pub max_children: Option<usize>,
    #[serde(default)]
    pub template: Option<Box<WorkflowNode>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BudgetSpec {
    #[serde(default)]
    pub max_steps: Option<u32>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub max_parallel: Option<u8>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PermissionSpec {
    #[serde(default)]
    pub allow_write: bool,
    #[serde(default)]
    pub allow_network: bool,
    /// Expose no tools to the child. This is distinct from an empty
    /// `allowed_tools` list, which preserves the role's default tool surface.
    #[serde(default)]
    pub deny_all_tools: bool,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub file_scope: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ModelPolicy {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub fallback_models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PromotionPolicy {
    #[serde(default)]
    pub strategy: PromotionStrategy,
    #[serde(default)]
    pub require_teacher_review: bool,
    #[serde(default)]
    pub min_successful_branches: Option<u32>,
    #[serde(default)]
    pub promotion_gate: PromotionGate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PromotionStrategy {
    #[default]
    All,
    FirstSuccess,
    BestScore,
    TeacherSelected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowPlan {
    goal: String,
    max_concurrent: u8,
    phases: Vec<PhasePlan>,
}

impl WorkflowPlan {
    pub fn from_config(config: &WorkflowConfig) -> Result<Self, WorkflowValidationError> {
        validate_non_empty("workflow goal", &config.goal)?;
        if !(1..=20).contains(&config.max_concurrent) {
            return Err(WorkflowValidationError::InvalidMaxConcurrent {
                value: config.max_concurrent,
            });
        }
        if config.phases.is_empty() {
            return Err(WorkflowValidationError::EmptyWorkflow);
        }

        let mut phase_indices = BTreeMap::new();
        let mut all_tasks = BTreeMap::new();
        let mut task_phase = BTreeMap::new();

        for (phase_index, phase) in config.phases.iter().enumerate() {
            validate_non_empty("phase name", &phase.name)?;
            if phase.tasks.is_empty() {
                return Err(WorkflowValidationError::EmptyPhase {
                    phase: phase.name.clone(),
                });
            }
            if phase_indices
                .insert(phase.name.clone(), phase_index)
                .is_some()
            {
                return Err(WorkflowValidationError::DuplicatePhase {
                    phase: phase.name.clone(),
                });
            }

            for task in &phase.tasks {
                validate_non_empty("task id", &task.id)?;
                validate_non_empty("task prompt", &task.prompt)?;
                if all_tasks.insert(task.id.clone(), task).is_some() {
                    return Err(WorkflowValidationError::DuplicateTask {
                        task: task.id.clone(),
                    });
                }
                task_phase.insert(task.id.clone(), phase.name.clone());
            }
        }

        for phase in &config.phases {
            for dependency in &phase.depends_on {
                if dependency == &phase.name || !phase_indices.contains_key(dependency) {
                    return Err(WorkflowValidationError::InvalidPhaseDependency {
                        phase: phase.name.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
            validate_parallel_write_scope(phase)?;
        }

        let ordered_phase_names = ordered_phases(config, &phase_indices)?;
        let phase_order: BTreeMap<_, _> = ordered_phase_names
            .iter()
            .enumerate()
            .map(|(index, phase)| (phase.clone(), index))
            .collect();

        for phase in &config.phases {
            for task in &phase.tasks {
                for dependency in &task.depends_on_results {
                    let Some(dependency_phase) = task_phase.get(dependency) else {
                        return Err(WorkflowValidationError::InvalidTaskResultDependency {
                            task: task.id.clone(),
                            dependency: dependency.clone(),
                        });
                    };
                    if phase_order[dependency_phase] >= phase_order[&phase.name] {
                        return Err(WorkflowValidationError::UnavailableTaskResultDependency {
                            task: task.id.clone(),
                            dependency: dependency.clone(),
                            dependency_phase: dependency_phase.clone(),
                            task_phase: phase.name.clone(),
                        });
                    }
                }
            }
        }

        let phases = ordered_phase_names
            .iter()
            .map(|phase_name| {
                let phase = &config.phases[phase_indices[phase_name]];
                PhasePlan {
                    name: phase.name.clone(),
                    parallel: phase.parallel,
                    on_failure: phase.on_failure,
                    tasks: phase.tasks.clone(),
                }
            })
            .collect();

        Ok(Self {
            goal: config.goal.clone(),
            max_concurrent: config.max_concurrent,
            phases,
        })
    }

    pub fn goal(&self) -> &str {
        &self.goal
    }

    pub fn max_concurrent(&self) -> u8 {
        self.max_concurrent
    }

    pub fn phases(&self) -> &[PhasePlan] {
        &self.phases
    }

    pub fn phase_names(&self) -> impl Iterator<Item = &str> {
        self.phases.iter().map(|phase| phase.name.as_str())
    }
}

pub type WorkflowIr = WorkflowPlan;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhasePlan {
    pub name: String,
    pub parallel: bool,
    pub on_failure: FailurePolicy,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phase {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub parallel: bool,
    #[serde(default)]
    pub on_failure: FailurePolicy,
    #[serde(default)]
    pub tasks: Vec<Task>,
}

pub type WorkflowPhase = Phase;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    #[default]
    SkipContinue,
    Abort,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub prompt: String,
    #[serde(default)]
    pub agent_type: AgentType,
    #[serde(default)]
    pub mode: TaskMode,
    #[serde(default)]
    pub isolation: IsolationMode,
    #[serde(default)]
    pub file_scope: Vec<String>,
    #[serde(default)]
    pub depends_on_results: Vec<String>,
    #[serde(default)]
    pub max_steps: Option<u32>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

pub type WorkflowTask = Task;
pub type WorkflowRole = AgentType;

/// The workflow wire's agent type. The serialized spellings are the canonical
/// Codewhale role vocabulary (founder, 2026-09-02); the pre-rename JS
/// spellings stay accepted as aliases so checked-in and saved workflows keep
/// compiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentType {
    #[default]
    General,
    #[serde(alias = "scout")]
    Explore,
    #[serde(rename = "planner", alias = "plan", alias = "awaiter")]
    Plan,
    #[serde(
        alias = "review",
        alias = "reviewer",
        alias = "consultant",
        alias = "oracle"
    )]
    Review,
    #[serde(rename = "implement", alias = "implementer", alias = "builder")]
    Implementer,
    #[serde(rename = "test", alias = "verifier", alias = "verify")]
    Verifier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskMode {
    #[default]
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMode {
    /// Runtime chooses isolation.
    ///
    /// Parallel write-capable children resolve to [`IsolationMode::Worktree`]
    /// so concurrent writers do not collide in the parent checkout. Explicit
    /// [`IsolationMode::Shared`] is the plan-level same-worktree override.
    #[default]
    Auto,
    /// Share the parent checkout (same-worktree).
    Shared,
    /// Dedicated git worktree / branch.
    Worktree,
}

impl IsolationMode {
    /// Resolve [`IsolationMode::Auto`] for a leaf.
    ///
    /// When `parallel_write` is true (leaf is write-capable inside a parallel
    /// branch), Auto becomes Worktree. Otherwise Auto becomes Shared.
    #[must_use]
    pub fn resolve(self, parallel_write: bool) -> Self {
        match self {
            Self::Auto if parallel_write => Self::Worktree,
            Self::Auto => Self::Shared,
            other => other,
        }
    }

    /// Whether the resolved mode provisions a dedicated worktree.
    #[must_use]
    pub fn wants_worktree(self, parallel_write: bool) -> bool {
        matches!(self.resolve(parallel_write), Self::Worktree)
    }
}

/// A leaf is write-capable when it can mutate the workspace.
///
/// Authority comes from the declared mode and permissions, not the agent's
/// role identity. In particular, an implementer may be deliberately confined
/// to a read-only verification task. Used by workflow lowering to decide the
/// default isolation for parallel children (#4120).
#[must_use]
pub fn leaf_is_write_capable(spec: &LeafSpec) -> bool {
    spec.mode == TaskMode::ReadWrite || spec.permissions.allow_write
}

/// Effective worktree flag for a leaf given whether it is being lowered inside
/// a parallel branch.
#[must_use]
pub fn leaf_wants_worktree(spec: &LeafSpec, parallel: bool) -> bool {
    let parallel_write = parallel && leaf_is_write_capable(spec);
    spec.isolation.wants_worktree(parallel_write)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchResult {
    pub branch_id: String,
    pub task_id: String,
    pub status: WorkflowRunStatus,
    #[serde(default)]
    pub usage: WorkflowUsage,
    #[serde(default, skip_serializing_if = "WorkflowMemoUsage::is_zero")]
    pub memo_usage: WorkflowMemoUsage,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeafResult {
    pub leaf_id: String,
    pub task_id: String,
    /// Fleet role the leaf was declared to run as, if any (#4177).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Fleet roster profile the leaf was declared to run as, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub status: WorkflowRunStatus,
    #[serde(default)]
    pub usage: WorkflowUsage,
    #[serde(default, skip_serializing_if = "WorkflowMemoUsage::is_zero")]
    pub memo_usage: WorkflowMemoUsage,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    /// Post-hoc validation failure for the leaf's structured response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WorkflowUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_microusd: Option<u64>,
}

impl WorkflowUsage {
    #[must_use]
    pub fn total_tokens(self) -> Option<u64> {
        self.input_tokens
            .zip(self.output_tokens)
            .map(|(input, output)| input.saturating_add(output))
    }

    /// Add two independently observed usage receipts. A field remains known
    /// only when both contributors reported it; `Some(0)` is still observed.
    pub(crate) fn add_assign(&mut self, other: Self) {
        self.input_tokens = sum_reported(self.input_tokens, other.input_tokens);
        self.output_tokens = sum_reported(self.output_tokens, other.output_tokens);
        self.cost_microusd = sum_reported(self.cost_microusd, other.cost_microusd);
    }
}

fn sum_reported(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    left.zip(right)
        .map(|(left, right)| left.saturating_add(right))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WorkflowMemoUsage {
    #[serde(default)]
    pub armh_hits: u64,
    #[serde(default)]
    pub armh_misses: u64,
    #[serde(default)]
    pub armh_saved_estimated_tokens: u64,
    #[serde(default)]
    pub provider_prompt_cache_hits: u64,
    #[serde(default)]
    pub provider_prompt_cache_misses: u64,
}

impl WorkflowMemoUsage {
    /// True when every counter is zero: 136 bytes of nothing on the wire.
    pub fn is_zero(&self) -> bool {
        self.armh_hits == 0
            && self.armh_misses == 0
            && self.armh_saved_estimated_tokens == 0
            && self.provider_prompt_cache_hits == 0
            && self.provider_prompt_cache_misses == 0
    }

    pub(crate) fn add_assign(&mut self, other: Self) {
        self.armh_hits = self.armh_hits.saturating_add(other.armh_hits);
        self.armh_misses = self.armh_misses.saturating_add(other.armh_misses);
        self.armh_saved_estimated_tokens = self
            .armh_saved_estimated_tokens
            .saturating_add(other.armh_saved_estimated_tokens);
        self.provider_prompt_cache_hits = self
            .provider_prompt_cache_hits
            .saturating_add(other.provider_prompt_cache_hits);
        self.provider_prompt_cache_misses = self
            .provider_prompt_cache_misses
            .saturating_add(other.provider_prompt_cache_misses);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlNodeResult {
    pub node_id: String,
    pub kind: ControlNodeKind,
    pub status: WorkflowRunStatus,
    #[serde(default)]
    pub selected_children: Vec<String>,
    #[serde(default)]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunStatus {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    BudgetExceeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlNodeKind {
    BranchSet,
    Leaf,
    Sequence,
    Reduce,
    TeacherReview,
    LoopUntil,
    Cond,
    Expand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowExecution {
    pub status: WorkflowRunStatus,
    #[serde(default)]
    pub usage: WorkflowUsage,
    #[serde(default, skip_serializing_if = "WorkflowMemoUsage::is_zero")]
    pub memo_usage: WorkflowMemoUsage,
    #[serde(default)]
    pub leaf_results: Vec<LeafResult>,
    #[serde(default)]
    pub branch_results: Vec<BranchResult>,
    #[serde(default)]
    pub control_node_results: Vec<ControlNodeResult>,
}

impl Default for WorkflowExecution {
    fn default() -> Self {
        Self {
            status: WorkflowRunStatus::Succeeded,
            usage: WorkflowUsage::default(),
            memo_usage: WorkflowMemoUsage::default(),
            leaf_results: Vec::new(),
            branch_results: Vec::new(),
            control_node_results: Vec::new(),
        }
    }
}

impl WorkflowExecution {
    pub fn mark_failed(&mut self) {
        self.status = WorkflowRunStatus::Failed;
    }

    pub fn mark_cancelled(&mut self) {
        self.status = WorkflowRunStatus::Cancelled;
    }

    pub fn mark_budget_exceeded(&mut self) {
        self.status = WorkflowRunStatus::BudgetExceeded;
    }

    fn should_stop_mock_execution(&self) -> bool {
        matches!(
            self.status,
            WorkflowRunStatus::Cancelled | WorkflowRunStatus::BudgetExceeded
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MockLeafOutcome {
    pub status: WorkflowRunStatus,
    #[serde(default)]
    pub usage: WorkflowUsage,
    #[serde(default, skip_serializing_if = "WorkflowMemoUsage::is_zero")]
    pub memo_usage: WorkflowMemoUsage,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
}

impl MockLeafOutcome {
    pub fn succeeded(output: impl Into<String>) -> Self {
        Self {
            status: WorkflowRunStatus::Succeeded,
            usage: WorkflowUsage::default(),
            memo_usage: WorkflowMemoUsage::default(),
            output: Some(output.into()),
            artifacts: Vec::new(),
        }
    }

    pub fn failed(output: impl Into<String>) -> Self {
        Self {
            status: WorkflowRunStatus::Failed,
            usage: WorkflowUsage::default(),
            memo_usage: WorkflowMemoUsage::default(),
            output: Some(output.into()),
            artifacts: Vec::new(),
        }
    }

    pub fn with_usage(mut self, usage: WorkflowUsage) -> Self {
        self.usage = usage;
        self
    }

    pub fn with_memo_usage(mut self, memo_usage: WorkflowMemoUsage) -> Self {
        self.memo_usage = memo_usage;
        self
    }
}

#[derive(Debug, Default, Clone)]
pub struct MockWorkflowExecutor {
    leaf_outcomes: BTreeMap<String, MockLeafOutcome>,
    predicate_results: BTreeMap<String, Vec<bool>>,
    generated_nodes: BTreeMap<String, Vec<WorkflowNode>>,
    cancelled: bool,
    max_leaf_steps: Option<u32>,
    leaf_steps_executed: u32,
    max_leaf_tokens: Option<u64>,
    leaf_tokens_used: u64,
}

impl MockWorkflowExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_leaf_outcome(
        mut self,
        leaf_id: impl Into<String>,
        outcome: MockLeafOutcome,
    ) -> Self {
        self.leaf_outcomes.insert(leaf_id.into(), outcome);
        self
    }

    pub fn with_predicate_results(
        mut self,
        node_id: impl Into<String>,
        results: Vec<bool>,
    ) -> Self {
        self.predicate_results.insert(node_id.into(), results);
        self
    }

    pub fn with_generated_nodes(
        mut self,
        node_id: impl Into<String>,
        nodes: Vec<WorkflowNode>,
    ) -> Self {
        self.generated_nodes.insert(node_id.into(), nodes);
        self
    }

    pub fn with_cancelled(mut self) -> Self {
        self.cancelled = true;
        self
    }

    pub fn with_max_leaf_steps(mut self, max_leaf_steps: u32) -> Self {
        self.max_leaf_steps = Some(max_leaf_steps);
        self
    }

    pub fn with_max_leaf_tokens(mut self, max_leaf_tokens: u64) -> Self {
        self.max_leaf_tokens = Some(max_leaf_tokens);
        self
    }

    pub fn run(
        &mut self,
        spec: &WorkflowSpec,
    ) -> Result<WorkflowExecution, WorkflowExecutionError> {
        validate_workflow_nodes(&spec.nodes)?;
        let mut execution = WorkflowExecution::default();
        self.execute_nodes(&spec.nodes, &mut execution)?;
        Ok(execution)
    }

    fn execute_nodes(
        &mut self,
        nodes: &[WorkflowNode],
        execution: &mut WorkflowExecution,
    ) -> Result<(), WorkflowExecutionError> {
        for node in nodes {
            if execution.should_stop_mock_execution() {
                break;
            }
            self.execute_node(node, execution)?;
        }
        Ok(())
    }

    fn execute_node(
        &mut self,
        node: &WorkflowNode,
        execution: &mut WorkflowExecution,
    ) -> Result<(), WorkflowExecutionError> {
        match node {
            WorkflowNode::BranchSet(spec) => self.execute_branch_set(spec, execution),
            WorkflowNode::Leaf(spec) => {
                self.execute_leaf(spec, execution);
                Ok(())
            }
            WorkflowNode::Sequence(spec) => {
                self.execute_nodes(&spec.children, execution)?;
                execution.control_node_results.push(ControlNodeResult {
                    node_id: spec.id.clone(),
                    kind: ControlNodeKind::Sequence,
                    status: execution.status,
                    selected_children: spec.children.iter().map(node_id).collect(),
                    summary: Some("sequence executed in declaration order".to_string()),
                });
                Ok(())
            }
            WorkflowNode::Reduce(spec) => {
                execution.control_node_results.push(ControlNodeResult {
                    node_id: spec.id.clone(),
                    kind: ControlNodeKind::Reduce,
                    status: WorkflowRunStatus::Succeeded,
                    selected_children: spec.inputs.clone(),
                    summary: Some(spec.prompt.clone()),
                });
                Ok(())
            }
            WorkflowNode::TeacherReview(spec) => {
                execution.control_node_results.push(ControlNodeResult {
                    node_id: spec.id.clone(),
                    kind: ControlNodeKind::TeacherReview,
                    status: WorkflowRunStatus::Succeeded,
                    selected_children: spec.candidates.clone(),
                    summary: Some(
                        "teacher review scaffold selected declared candidates".to_string(),
                    ),
                });
                Ok(())
            }
            WorkflowNode::LoopUntil(spec) => self.execute_loop_until(spec, execution),
            WorkflowNode::Cond(spec) => self.execute_cond(spec, execution),
            WorkflowNode::Expand(spec) => self.execute_expand(spec, execution),
        }
    }

    fn execute_branch_set(
        &mut self,
        spec: &BranchSpec,
        execution: &mut WorkflowExecution,
    ) -> Result<(), WorkflowExecutionError> {
        let before = execution.leaf_results.len();
        self.execute_nodes(&spec.children, execution)?;
        let status = aggregate_mock_status(&execution.leaf_results[before..]);
        let mut usage = WorkflowUsage::default();
        let mut memo_usage = WorkflowMemoUsage::default();
        for (index, result) in execution.leaf_results[before..].iter().enumerate() {
            if index == 0 {
                usage = result.usage;
            } else {
                usage.add_assign(result.usage);
            }
            memo_usage.add_assign(result.memo_usage);
        }
        mark_execution_for_status(execution, status);
        execution.branch_results.push(BranchResult {
            branch_id: spec.id.clone(),
            task_id: spec.id.clone(),
            status,
            usage,
            memo_usage,
            artifacts: Vec::new(),
            notes: Some("mock branch set executed without runtime fanout".to_string()),
        });
        execution.control_node_results.push(ControlNodeResult {
            node_id: spec.id.clone(),
            kind: ControlNodeKind::BranchSet,
            status,
            selected_children: spec.children.iter().map(node_id).collect(),
            summary: Some("branch set scaffold executed children deterministically".to_string()),
        });
        Ok(())
    }

    fn execute_leaf(&mut self, spec: &LeafSpec, execution: &mut WorkflowExecution) {
        let outcome = self.mock_leaf_outcome(spec);
        mark_execution_for_status(execution, outcome.status);
        if execution.leaf_results.is_empty() {
            execution.usage = outcome.usage;
        } else {
            execution.usage.add_assign(outcome.usage);
        }
        execution.memo_usage.add_assign(outcome.memo_usage);
        execution.leaf_results.push(LeafResult {
            leaf_id: spec.id.clone(),
            task_id: spec.id.clone(),
            role: spec.role.clone(),
            profile: spec.profile.clone(),
            status: outcome.status,
            usage: outcome.usage,
            memo_usage: outcome.memo_usage,
            output: outcome.output,
            artifacts: outcome.artifacts,
            schema_error: None,
        });
    }

    fn execute_loop_until(
        &mut self,
        spec: &LoopUntilSpec,
        execution: &mut WorkflowExecution,
    ) -> Result<(), WorkflowExecutionError> {
        let max_iterations = spec.max_iterations.unwrap_or(1).max(1);
        let mut iterations = 0;
        let mut passed = false;
        while iterations < max_iterations {
            if execution.should_stop_mock_execution() {
                break;
            }
            iterations += 1;
            self.execute_nodes(&spec.children, execution)?;
            if execution.should_stop_mock_execution() {
                break;
            }
            if self.next_predicate_result(&spec.id) {
                passed = true;
                break;
            }
        }
        let status = if execution.should_stop_mock_execution() {
            execution.status
        } else if passed {
            WorkflowRunStatus::Succeeded
        } else {
            WorkflowRunStatus::Failed
        };
        mark_execution_for_status(execution, status);
        execution.control_node_results.push(ControlNodeResult {
            node_id: spec.id.clone(),
            kind: ControlNodeKind::LoopUntil,
            status,
            selected_children: spec.children.iter().map(node_id).collect(),
            summary: Some(format!("loop_until iterations={iterations}")),
        });
        Ok(())
    }

    fn execute_cond(
        &mut self,
        spec: &CondSpec,
        execution: &mut WorkflowExecution,
    ) -> Result<(), WorkflowExecutionError> {
        let passed = self.next_predicate_result(&spec.id);
        let selected_nodes = if passed {
            &spec.then_nodes
        } else {
            &spec.else_nodes
        };
        self.execute_nodes(selected_nodes, execution)?;
        let status = if execution.should_stop_mock_execution() {
            execution.status
        } else {
            WorkflowRunStatus::Succeeded
        };
        execution.control_node_results.push(ControlNodeResult {
            node_id: spec.id.clone(),
            kind: ControlNodeKind::Cond,
            status,
            selected_children: selected_nodes.iter().map(node_id).collect(),
            summary: Some(format!("predicate_result={passed}")),
        });
        Ok(())
    }

    fn execute_expand(
        &mut self,
        spec: &ExpandSpec,
        execution: &mut WorkflowExecution,
    ) -> Result<(), WorkflowExecutionError> {
        let mut nodes = self.generated_nodes.remove(&spec.id).unwrap_or_default();
        if let Some(max_children) = spec.max_children {
            nodes.truncate(max_children);
        }
        validate_workflow_node_shapes(&nodes)?;
        self.execute_nodes(&nodes, execution)?;
        let status = if execution.should_stop_mock_execution() {
            execution.status
        } else {
            WorkflowRunStatus::Succeeded
        };
        execution.control_node_results.push(ControlNodeResult {
            node_id: spec.id.clone(),
            kind: ControlNodeKind::Expand,
            status,
            selected_children: nodes.iter().map(node_id).collect(),
            summary: Some(format!("expanded_from={}", spec.source)),
        });
        Ok(())
    }

    fn mock_leaf_outcome(&mut self, spec: &LeafSpec) -> MockLeafOutcome {
        if self.cancelled {
            return MockLeafOutcome {
                status: WorkflowRunStatus::Cancelled,
                usage: WorkflowUsage {
                    input_tokens: Some(0),
                    output_tokens: Some(0),
                    cost_microusd: Some(0),
                },
                memo_usage: WorkflowMemoUsage::default(),
                output: Some("mock workflow cancelled before leaf execution".to_string()),
                artifacts: Vec::new(),
            };
        }
        if self
            .max_leaf_steps
            .is_some_and(|max| max > 0 && self.leaf_steps_executed >= max)
        {
            return MockLeafOutcome {
                status: WorkflowRunStatus::BudgetExceeded,
                // The leaf was rejected before execution, so zero usage is a
                // known observation rather than missing provider telemetry.
                usage: WorkflowUsage {
                    input_tokens: Some(0),
                    output_tokens: Some(0),
                    cost_microusd: Some(0),
                },
                memo_usage: WorkflowMemoUsage::default(),
                output: Some("mock workflow leaf step budget exhausted".to_string()),
                artifacts: Vec::new(),
            };
        }
        if self
            .max_leaf_tokens
            .is_some_and(|max| self.leaf_tokens_used >= max)
            || spec.budget.max_tokens == Some(0)
        {
            return MockLeafOutcome {
                status: WorkflowRunStatus::BudgetExceeded,
                usage: WorkflowUsage {
                    input_tokens: Some(0),
                    output_tokens: Some(0),
                    cost_microusd: Some(0),
                },
                memo_usage: WorkflowMemoUsage::default(),
                output: Some("mock workflow leaf token budget exhausted".to_string()),
                artifacts: Vec::new(),
            };
        }
        self.leaf_steps_executed = self.leaf_steps_executed.saturating_add(1);
        let outcome = self
            .leaf_outcomes
            .remove(&spec.id)
            .unwrap_or_else(|| MockLeafOutcome::succeeded(format!("mock leaf {}", spec.id)));
        let tokens = outcome.usage.total_tokens();
        if let (Some(per_leaf_token_cap), Some(tokens)) = (spec.budget.max_tokens, tokens)
            && tokens > per_leaf_token_cap
        {
            return MockLeafOutcome {
                status: WorkflowRunStatus::BudgetExceeded,
                usage: outcome.usage,
                memo_usage: outcome.memo_usage,
                output: Some(format!(
                    "mock workflow leaf token budget exhausted ({tokens} > {per_leaf_token_cap})"
                )),
                artifacts: outcome.artifacts,
            };
        }
        if let Some(tokens) = tokens {
            self.leaf_tokens_used = self.leaf_tokens_used.saturating_add(tokens);
        }
        outcome
    }

    fn next_predicate_result(&mut self, node_id: &str) -> bool {
        let Some(results) = self.predicate_results.get_mut(node_id) else {
            return false;
        };
        if results.is_empty() {
            return false;
        }
        results.remove(0)
    }
}

fn aggregate_mock_status(results: &[LeafResult]) -> WorkflowRunStatus {
    if results
        .iter()
        .any(|result| result.status == WorkflowRunStatus::Cancelled)
    {
        WorkflowRunStatus::Cancelled
    } else if results
        .iter()
        .any(|result| result.status == WorkflowRunStatus::BudgetExceeded)
    {
        WorkflowRunStatus::BudgetExceeded
    } else if results
        .iter()
        .any(|result| result.status != WorkflowRunStatus::Succeeded)
    {
        WorkflowRunStatus::Failed
    } else {
        WorkflowRunStatus::Succeeded
    }
}

fn mark_execution_for_status(execution: &mut WorkflowExecution, status: WorkflowRunStatus) {
    match status {
        WorkflowRunStatus::Succeeded | WorkflowRunStatus::Pending | WorkflowRunStatus::Running => {}
        WorkflowRunStatus::Failed => execution.mark_failed(),
        WorkflowRunStatus::Cancelled => execution.mark_cancelled(),
        WorkflowRunStatus::BudgetExceeded => execution.mark_budget_exceeded(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchCandidate {
    pub branch_id: String,
    pub status: WorkflowRunStatus,
    pub score: u32,
    pub cost: u64,
    #[serde(default)]
    pub diversity_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeacherCandidateKind {
    Note,
    WorkflowRecipe,
    SkillPatch,
    RegressionTest,
    CachePolicyPatch,
    BranchHeuristic,
    AuthoringPromptPatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TeacherCandidateStatus {
    #[default]
    Proposed,
    Accepted,
    Rejected,
    Promoted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeacherCandidate {
    pub candidate_id: String,
    pub kind: TeacherCandidateKind,
    #[serde(default)]
    pub status: TeacherCandidateStatus,
    pub source_node_id: String,
    #[serde(default)]
    pub source_branch_id: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub replay_results: Vec<StudentReplayResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StudentReplayMetrics {
    #[serde(default)]
    pub score: i32,
    #[serde(default)]
    pub cost_microusd: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudentReplayTestResult {
    pub name: String,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudentReplayResult {
    pub trace_id: String,
    pub candidate_id: String,
    pub baseline: StudentReplayMetrics,
    pub candidate: StudentReplayMetrics,
    #[serde(default)]
    pub required_tests: Vec<StudentReplayTestResult>,
    #[serde(default)]
    pub policy_violations: Vec<String>,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionGate {
    #[serde(default = "default_min_replay_score_delta")]
    pub min_score_delta: i32,
    #[serde(default)]
    pub max_cost_delta_microusd: Option<i64>,
    #[serde(default = "default_true")]
    pub require_all_tests_pass: bool,
    #[serde(default = "default_true")]
    pub reject_policy_violations: bool,
    #[serde(default = "default_true")]
    pub reject_stale_replay: bool,
}

impl Default for PromotionGate {
    fn default() -> Self {
        Self {
            min_score_delta: default_min_replay_score_delta(),
            max_cost_delta_microusd: None,
            require_all_tests_pass: true,
            reject_policy_violations: true,
            reject_stale_replay: true,
        }
    }
}

impl PromotionGate {
    pub fn evaluate_candidate(&self, candidate: &TeacherCandidate) -> PromotionGateDecision {
        let Some(replay) = candidate.replay_results.last() else {
            return PromotionGateDecision {
                candidate_id: candidate.candidate_id.clone(),
                status: TeacherCandidateStatus::Rejected,
                score_delta: 0,
                cost_delta_microusd: 0,
                reasons: vec!["no student replay result recorded".to_string()],
            };
        };
        self.evaluate_replay(&candidate.candidate_id, replay)
    }

    pub fn evaluate_replay(
        &self,
        candidate_id: &str,
        replay: &StudentReplayResult,
    ) -> PromotionGateDecision {
        let score_delta = replay.score_delta();
        let cost_delta_microusd = replay.cost_delta_microusd();
        let mut reasons = Vec::new();

        if score_delta < self.min_score_delta {
            reasons.push(format!(
                "score delta {score_delta} is below required {}",
                self.min_score_delta
            ));
        }
        if let Some(max_cost_delta) = self.max_cost_delta_microusd
            && cost_delta_microusd > max_cost_delta
        {
            reasons.push(format!(
                "cost delta {cost_delta_microusd} exceeds allowed {max_cost_delta}"
            ));
        }
        if self.require_all_tests_pass {
            for test in replay.required_tests.iter().filter(|test| !test.passed) {
                reasons.push(format!("required test `{}` failed", test.name));
            }
        }
        if self.reject_policy_violations {
            for violation in &replay.policy_violations {
                reasons.push(format!("policy violation: {violation}"));
            }
        }
        if self.reject_stale_replay && replay.stale {
            reasons.push("student replay result is stale".to_string());
        }

        let status = if reasons.is_empty() {
            TeacherCandidateStatus::Promoted
        } else {
            TeacherCandidateStatus::Rejected
        };
        PromotionGateDecision {
            candidate_id: candidate_id.to_string(),
            status,
            score_delta,
            cost_delta_microusd,
            reasons,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionGateDecision {
    pub candidate_id: String,
    pub status: TeacherCandidateStatus,
    pub score_delta: i32,
    pub cost_delta_microusd: i64,
    #[serde(default)]
    pub reasons: Vec<String>,
}

impl PromotionGateDecision {
    pub fn promoted(&self) -> bool {
        self.status == TeacherCandidateStatus::Promoted
    }
}

impl StudentReplayResult {
    pub fn score_delta(&self) -> i32 {
        self.candidate.score.saturating_sub(self.baseline.score)
    }

    pub fn cost_delta_microusd(&self) -> i64 {
        signed_u64_delta(self.candidate.cost_microusd, self.baseline.cost_microusd)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TeacherReviewReport {
    pub review_node_id: String,
    #[serde(default)]
    pub candidates: Vec<TeacherCandidate>,
}

impl TeacherReviewReport {
    pub fn from_execution(review: &TeacherReviewSpec, execution: &WorkflowExecution) -> Self {
        let candidates = teacher_candidates_from_execution(review, execution);
        Self {
            review_node_id: review.id.clone(),
            candidates,
        }
    }
}

pub fn teacher_candidates_from_execution(
    review: &TeacherReviewSpec,
    execution: &WorkflowExecution,
) -> Vec<TeacherCandidate> {
    let mut candidates = Vec::new();
    for source in &review.candidates {
        if let Some(branch) = execution
            .branch_results
            .iter()
            .find(|branch| branch.branch_id == *source || branch.task_id == *source)
        {
            candidates.push(teacher_candidate_from_branch(review, branch));
            continue;
        }
        if let Some(leaf) = execution
            .leaf_results
            .iter()
            .find(|leaf| leaf.leaf_id == *source || leaf.task_id == *source)
        {
            candidates.push(teacher_candidate_from_leaf(review, leaf));
            continue;
        }
        if let Some(control) = execution
            .control_node_results
            .iter()
            .find(|control| control.node_id == *source)
        {
            candidates.push(teacher_candidate_from_control(review, control));
        }
    }
    candidates
}

fn teacher_candidate_from_branch(
    review: &TeacherReviewSpec,
    branch: &BranchResult,
) -> TeacherCandidate {
    let kind =
        if branch.memo_usage.armh_hits > 0 || branch.memo_usage.provider_prompt_cache_hits > 0 {
            TeacherCandidateKind::CachePolicyPatch
        } else if branch.status == WorkflowRunStatus::Succeeded {
            TeacherCandidateKind::WorkflowRecipe
        } else {
            TeacherCandidateKind::BranchHeuristic
        };
    let mut evidence = vec![format!("status={:?}", branch.status)];
    if branch.usage.total_tokens().is_some_and(|tokens| tokens > 0)
        || branch.usage.cost_microusd.is_some_and(|cost| cost > 0)
    {
        evidence.push(format!(
            "tokens={}, cost_microusd={}",
            branch
                .usage
                .total_tokens()
                .map_or_else(|| "unknown".to_string(), |value| value.to_string()),
            branch
                .usage
                .cost_microusd
                .map_or_else(|| "unknown".to_string(), |value| value.to_string())
        ));
    }
    if branch.memo_usage.armh_hits > 0 || branch.memo_usage.provider_prompt_cache_hits > 0 {
        evidence.push(format!(
            "armh_hits={}, provider_prompt_cache_hits={}",
            branch.memo_usage.armh_hits, branch.memo_usage.provider_prompt_cache_hits
        ));
    }
    if let Some(notes) = branch.notes.as_deref() {
        evidence.push(format!("notes={notes}"));
    }
    TeacherCandidate {
        candidate_id: format!("{}:{}", review.id, branch.branch_id),
        kind,
        status: TeacherCandidateStatus::Proposed,
        source_node_id: branch.task_id.clone(),
        source_branch_id: Some(branch.branch_id.clone()),
        summary: format!(
            "TeacherReview candidate from branch `{}` with {:?} status.",
            branch.branch_id, branch.status
        ),
        evidence,
        replay_results: Vec::new(),
    }
}

fn teacher_candidate_from_leaf(review: &TeacherReviewSpec, leaf: &LeafResult) -> TeacherCandidate {
    let kind = if leaf.status == WorkflowRunStatus::Failed {
        TeacherCandidateKind::RegressionTest
    } else if leaf.memo_usage.armh_hits > 0 || leaf.memo_usage.provider_prompt_cache_hits > 0 {
        TeacherCandidateKind::CachePolicyPatch
    } else {
        TeacherCandidateKind::Note
    };
    let mut evidence = vec![format!("status={:?}", leaf.status)];
    if let Some(output) = leaf.output.as_deref() {
        evidence.push(format!("output={}", truncate_evidence(output)));
    }
    TeacherCandidate {
        candidate_id: format!("{}:{}", review.id, leaf.leaf_id),
        kind,
        status: TeacherCandidateStatus::Proposed,
        source_node_id: leaf.leaf_id.clone(),
        source_branch_id: None,
        summary: format!(
            "TeacherReview candidate from leaf `{}` with {:?} status.",
            leaf.leaf_id, leaf.status
        ),
        evidence,
        replay_results: Vec::new(),
    }
}

fn teacher_candidate_from_control(
    review: &TeacherReviewSpec,
    control: &ControlNodeResult,
) -> TeacherCandidate {
    let mut evidence = vec![format!("status={:?}", control.status)];
    if !control.selected_children.is_empty() {
        evidence.push(format!(
            "selected_children={}",
            control.selected_children.join(",")
        ));
    }
    if let Some(summary) = control.summary.as_deref() {
        evidence.push(format!("summary={}", truncate_evidence(summary)));
    }
    TeacherCandidate {
        candidate_id: format!("{}:{}", review.id, control.node_id),
        kind: TeacherCandidateKind::AuthoringPromptPatch,
        status: TeacherCandidateStatus::Proposed,
        source_node_id: control.node_id.clone(),
        source_branch_id: None,
        summary: format!(
            "TeacherReview candidate from control node `{}` ({:?}).",
            control.node_id, control.kind
        ),
        evidence,
        replay_results: Vec::new(),
    }
}

fn default_min_replay_score_delta() -> i32 {
    1
}

fn default_true() -> bool {
    true
}

fn signed_u64_delta(candidate: u64, baseline: u64) -> i64 {
    if candidate >= baseline {
        i64::try_from(candidate - baseline).unwrap_or(i64::MAX)
    } else {
        -i64::try_from(baseline - candidate).unwrap_or(i64::MAX)
    }
}

fn truncate_evidence(value: &str) -> String {
    const MAX_EVIDENCE_CHARS: usize = 240;
    if value.chars().count() <= MAX_EVIDENCE_CHARS {
        return value.to_string();
    }
    let mut truncated = value
        .chars()
        .take(MAX_EVIDENCE_CHARS.saturating_sub(1))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchTournament {
    #[serde(default)]
    pub min_score: u32,
    #[serde(default)]
    pub ordering: TournamentOrdering,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TournamentOrdering {
    /// Historical behavior: choose the cheapest passing branch, then score.
    #[default]
    CostThenScore,
    /// Quality-first behavior for explicitly configured evaluation workflows.
    ScoreThenCost,
}

impl BranchTournament {
    pub fn select(&self, candidates: &[BranchCandidate]) -> Option<BranchCandidate> {
        let passing = || {
            candidates.iter().filter(|candidate| {
                candidate.status == WorkflowRunStatus::Succeeded
                    && candidate.score >= self.min_score
            })
        };
        match self.ordering {
            TournamentOrdering::CostThenScore => passing()
                .min_by_key(|candidate| (candidate.cost, std::cmp::Reverse(candidate.score))),
            TournamentOrdering::ScoreThenCost => passing()
                .min_by_key(|candidate| (std::cmp::Reverse(candidate.score), candidate.cost)),
        }
        .cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParetoFrontier {
    #[serde(default = "default_frontier_limit")]
    pub max_items: usize,
}

impl Default for ParetoFrontier {
    fn default() -> Self {
        Self {
            max_items: default_frontier_limit(),
        }
    }
}

impl ParetoFrontier {
    pub fn select(&self, candidates: &[BranchCandidate]) -> Vec<BranchCandidate> {
        let mut frontier: Vec<_> = candidates
            .iter()
            .filter(|candidate| candidate.status == WorkflowRunStatus::Succeeded)
            .filter(|candidate| {
                !candidates.iter().any(|other| {
                    other.status == WorkflowRunStatus::Succeeded
                        && other.score >= candidate.score
                        && other.cost <= candidate.cost
                        && (other.score > candidate.score || other.cost < candidate.cost)
                })
            })
            .cloned()
            .collect();
        frontier.sort_by_key(|candidate| (std::cmp::Reverse(candidate.score), candidate.cost));
        frontier.truncate(self.max_items.max(1));
        frontier
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowExecutionError {
    #[error("{kind} node id must not be empty")]
    EmptyNodeId { kind: &'static str },
    #[error("leaf `{leaf}` prompt must not be empty")]
    EmptyLeafPrompt { leaf: String },
    #[error(
        "leaf `{leaf}` profile `{profile}` must be a non-empty token without whitespace, quotes, or `=`"
    )]
    InvalidLeafProfile { leaf: String, profile: String },
    #[error(
        "leaf `{leaf}` role `{role}` must be a non-empty token without whitespace, quotes, or `=`"
    )]
    InvalidLeafRole { leaf: String, role: String },
    #[error("duplicate workflow node `{node}`")]
    DuplicateNodeId { node: String },
    #[error("workflow node `{node}` has unknown {field} reference `{reference}`")]
    UnknownNodeReference {
        node: String,
        field: &'static str,
        reference: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowFleetLimits {
    pub max_total_agents: usize,
    pub max_depth: usize,
}

impl Default for WorkflowFleetLimits {
    fn default() -> Self {
        Self {
            max_total_agents: DEFAULT_FLEET_WORKFLOW_MAX_AGENTS,
            max_depth: DEFAULT_FLEET_WORKFLOW_MAX_DEPTH,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkflowFleetShape {
    pub total_agents: usize,
    pub max_depth: usize,
}

impl WorkflowFleetShape {
    fn add(self, other: Self) -> Self {
        Self {
            total_agents: self.total_agents.saturating_add(other.total_agents),
            max_depth: self.max_depth.max(other.max_depth),
        }
    }

    fn repeat(self, times: usize) -> Self {
        Self {
            total_agents: self.total_agents.saturating_mul(times),
            max_depth: self.max_depth,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowFleetLimitError {
    #[error("workflow IR is invalid for Fleet: {source}")]
    InvalidWorkflow {
        #[from]
        source: WorkflowExecutionError,
    },
    #[error(
        "workflow would launch {total_agents} agents; Fleet Workflow limit is {max_total_agents}"
    )]
    TooManyAgents {
        total_agents: usize,
        max_total_agents: usize,
    },
    #[error("workflow reaches recursion depth {depth}; Fleet Workflow limit is {max_depth}")]
    RecursionTooDeep { depth: usize, max_depth: usize },
    #[error("expand node `{node}` must declare max_children before Fleet launch")]
    UnboundedExpand { node: String },
    #[error("expand node `{node}` must include a template before Fleet launch")]
    MissingExpandTemplate { node: String },
    #[error("loop_until node `{node}` must declare max_iterations before Fleet launch")]
    UnboundedLoop { node: String },
}

fn estimate_fleet_shape(
    nodes: &[WorkflowNode],
) -> Result<WorkflowFleetShape, WorkflowFleetLimitError> {
    estimate_fleet_shape_at_depth(nodes, 1)
}

fn estimate_fleet_shape_at_depth(
    nodes: &[WorkflowNode],
    depth: usize,
) -> Result<WorkflowFleetShape, WorkflowFleetLimitError> {
    nodes
        .iter()
        .try_fold(WorkflowFleetShape::default(), |shape, node| {
            Ok(shape.add(estimate_node_fleet_shape(node, depth)?))
        })
}

fn estimate_node_fleet_shape(
    node: &WorkflowNode,
    depth: usize,
) -> Result<WorkflowFleetShape, WorkflowFleetLimitError> {
    match node {
        WorkflowNode::Leaf(_) => Ok(WorkflowFleetShape {
            total_agents: 1,
            max_depth: depth,
        }),
        WorkflowNode::BranchSet(spec) => estimate_fleet_shape_at_depth(&spec.children, depth + 1),
        WorkflowNode::Sequence(spec) => estimate_fleet_shape_at_depth(&spec.children, depth),
        WorkflowNode::Reduce(_) | WorkflowNode::TeacherReview(_) => Ok(WorkflowFleetShape {
            total_agents: 0,
            max_depth: 0,
        }),
        WorkflowNode::LoopUntil(spec) => {
            let iterations =
                spec.max_iterations
                    .ok_or_else(|| WorkflowFleetLimitError::UnboundedLoop {
                        node: spec.id.clone(),
                    })? as usize;
            Ok(estimate_fleet_shape_at_depth(&spec.children, depth)?.repeat(iterations.max(1)))
        }
        WorkflowNode::Cond(spec) => Ok(estimate_fleet_shape_at_depth(&spec.then_nodes, depth)?
            .add(estimate_fleet_shape_at_depth(&spec.else_nodes, depth)?)),
        WorkflowNode::Expand(spec) => {
            let max_children =
                spec.max_children
                    .ok_or_else(|| WorkflowFleetLimitError::UnboundedExpand {
                        node: spec.id.clone(),
                    })?;
            let template = spec.template.as_deref().ok_or_else(|| {
                WorkflowFleetLimitError::MissingExpandTemplate {
                    node: spec.id.clone(),
                }
            })?;
            validate_workflow_node_shapes(std::slice::from_ref(template))
                .map_err(|source| WorkflowFleetLimitError::InvalidWorkflow { source })?;
            Ok(estimate_node_fleet_shape(template, depth)?.repeat(max_children))
        }
    }
}

fn default_frontier_limit() -> usize {
    8
}

fn node_id(node: &WorkflowNode) -> String {
    match node {
        WorkflowNode::BranchSet(spec) => spec.id.clone(),
        WorkflowNode::Leaf(spec) => spec.id.clone(),
        WorkflowNode::Sequence(spec) => spec.id.clone(),
        WorkflowNode::Reduce(spec) => spec.id.clone(),
        WorkflowNode::TeacherReview(spec) => spec.id.clone(),
        WorkflowNode::LoopUntil(spec) => spec.id.clone(),
        WorkflowNode::Cond(spec) => spec.id.clone(),
        WorkflowNode::Expand(spec) => spec.id.clone(),
    }
}

pub(crate) fn validate_workflow_nodes(
    nodes: &[WorkflowNode],
) -> Result<(), WorkflowExecutionError> {
    let mut seen = BTreeSet::new();
    validate_workflow_nodes_inner(nodes, &mut seen)?;
    validate_workflow_references(nodes, &seen)
}

pub(crate) fn validate_workflow_node_shapes(
    nodes: &[WorkflowNode],
) -> Result<(), WorkflowExecutionError> {
    let mut seen = BTreeSet::new();
    validate_workflow_nodes_inner(nodes, &mut seen)
}

fn validate_workflow_nodes_inner(
    nodes: &[WorkflowNode],
    seen: &mut BTreeSet<String>,
) -> Result<(), WorkflowExecutionError> {
    for node in nodes {
        let id = node_id(node);
        let kind = control_kind_name(node);
        if id.trim().is_empty() {
            return Err(WorkflowExecutionError::EmptyNodeId { kind });
        }
        if !seen.insert(id.clone()) {
            return Err(WorkflowExecutionError::DuplicateNodeId { node: id });
        }
        match node {
            WorkflowNode::BranchSet(spec) => validate_workflow_nodes_inner(&spec.children, seen)?,
            WorkflowNode::Leaf(spec) => {
                if spec.prompt.trim().is_empty() {
                    return Err(WorkflowExecutionError::EmptyLeafPrompt {
                        leaf: spec.id.clone(),
                    });
                }
                if let Some(role) = spec.role.as_deref() {
                    validate_leaf_role(&spec.id, role)?;
                }
                if let Some(profile) = spec.profile.as_deref() {
                    validate_leaf_profile(&spec.id, profile)?;
                }
            }
            WorkflowNode::Sequence(spec) => validate_workflow_nodes_inner(&spec.children, seen)?,
            WorkflowNode::LoopUntil(spec) => validate_workflow_nodes_inner(&spec.children, seen)?,
            WorkflowNode::Cond(spec) => {
                validate_workflow_nodes_inner(&spec.then_nodes, seen)?;
                validate_workflow_nodes_inner(&spec.else_nodes, seen)?;
            }
            WorkflowNode::Reduce(_) | WorkflowNode::TeacherReview(_) | WorkflowNode::Expand(_) => {}
        }
    }
    Ok(())
}

fn validate_workflow_references(
    nodes: &[WorkflowNode],
    known_ids: &BTreeSet<String>,
) -> Result<(), WorkflowExecutionError> {
    for node in nodes {
        match node {
            WorkflowNode::BranchSet(spec) => {
                validate_workflow_references(&spec.children, known_ids)?;
            }
            WorkflowNode::Leaf(spec) => {
                validate_known_references(
                    spec.id.as_str(),
                    "depends_on_results",
                    &spec.depends_on_results,
                    known_ids,
                )?;
            }
            WorkflowNode::Sequence(spec) => {
                validate_workflow_references(&spec.children, known_ids)?;
            }
            WorkflowNode::Reduce(spec) => {
                validate_known_references(spec.id.as_str(), "inputs", &spec.inputs, known_ids)?;
            }
            WorkflowNode::TeacherReview(spec) => {
                validate_known_references(
                    spec.id.as_str(),
                    "candidates",
                    &spec.candidates,
                    known_ids,
                )?;
            }
            WorkflowNode::LoopUntil(spec) => {
                validate_workflow_references(&spec.children, known_ids)?;
            }
            WorkflowNode::Cond(spec) => {
                validate_workflow_references(&spec.then_nodes, known_ids)?;
                validate_workflow_references(&spec.else_nodes, known_ids)?;
            }
            WorkflowNode::Expand(_) => {}
        }
    }
    Ok(())
}

// Token rule only. Roster membership is resolved by the dispatcher (tui crate)
// at spawn time; this crate never sees the saved Fleet roster.
fn validate_leaf_profile(leaf: &str, profile: &str) -> Result<(), WorkflowExecutionError> {
    let invalid = profile.is_empty()
        || profile
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\'' | '`' | '='));
    if invalid {
        return Err(WorkflowExecutionError::InvalidLeafProfile {
            leaf: leaf.to_string(),
            profile: profile.to_string(),
        });
    }
    Ok(())
}

fn validate_leaf_role(leaf: &str, role: &str) -> Result<(), WorkflowExecutionError> {
    if validate_role_token(role).is_err() {
        return Err(WorkflowExecutionError::InvalidLeafRole {
            leaf: leaf.to_string(),
            role: role.to_string(),
        });
    }
    Ok(())
}

fn validate_known_references(
    node: &str,
    field: &'static str,
    references: &[String],
    known_ids: &BTreeSet<String>,
) -> Result<(), WorkflowExecutionError> {
    for reference in references {
        if !known_ids.contains(reference) {
            return Err(WorkflowExecutionError::UnknownNodeReference {
                node: node.to_string(),
                field,
                reference: reference.clone(),
            });
        }
    }
    Ok(())
}

fn control_kind_name(node: &WorkflowNode) -> &'static str {
    match node {
        WorkflowNode::BranchSet(_) => "branch_set",
        WorkflowNode::Leaf(_) => "leaf",
        WorkflowNode::Sequence(_) => "sequence",
        WorkflowNode::Reduce(_) => "reduce",
        WorkflowNode::TeacherReview(_) => "teacher_review",
        WorkflowNode::LoopUntil(_) => "loop_until",
        WorkflowNode::Cond(_) => "cond",
        WorkflowNode::Expand(_) => "expand",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowValidationError {
    #[error("{field} must not be empty")]
    EmptyField { field: &'static str },
    #[error("workflow must contain at least one phase")]
    EmptyWorkflow,
    #[error("phase `{phase}` must contain at least one task")]
    EmptyPhase { phase: String },
    #[error("max_concurrent must be between 1 and 20, got {value}")]
    InvalidMaxConcurrent { value: u8 },
    #[error("duplicate workflow phase `{phase}`")]
    DuplicatePhase { phase: String },
    #[error("duplicate workflow task `{task}`")]
    DuplicateTask { task: String },
    #[error("phase `{phase}` has invalid dependency `{dependency}`")]
    InvalidPhaseDependency { phase: String, dependency: String },
    #[error("phase dependency cycle includes `{phase}`")]
    PhaseDependencyCycle { phase: String },
    #[error("task `{task}` has invalid result dependency `{dependency}`")]
    InvalidTaskResultDependency { task: String, dependency: String },
    #[error(
        "task `{task}` depends on result `{dependency}` from unavailable phase `{dependency_phase}` while running in `{task_phase}`"
    )]
    UnavailableTaskResultDependency {
        task: String,
        dependency: String,
        dependency_phase: String,
        task_phase: String,
    },
    #[error("parallel read-write task `{task}` must declare a file_scope")]
    MissingParallelWriteScope { task: String },
    #[error("parallel read-write tasks `{left}` and `{right}` have overlapping file scopes")]
    OverlappingParallelWriteScope { left: String, right: String },
}

fn default_max_concurrent() -> u8 {
    4
}

fn validate_non_empty(field: &'static str, value: &str) -> Result<(), WorkflowValidationError> {
    if value.trim().is_empty() {
        return Err(WorkflowValidationError::EmptyField { field });
    }
    Ok(())
}

fn ordered_phases(
    config: &WorkflowConfig,
    phase_indices: &BTreeMap<String, usize>,
) -> Result<Vec<String>, WorkflowValidationError> {
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut ordered = Vec::with_capacity(config.phases.len());

    for phase in &config.phases {
        visit_phase(
            &phase.name,
            config,
            phase_indices,
            &mut visiting,
            &mut visited,
            &mut ordered,
        )?;
    }

    Ok(ordered)
}

fn visit_phase(
    phase_name: &str,
    config: &WorkflowConfig,
    phase_indices: &BTreeMap<String, usize>,
    visiting: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
    ordered: &mut Vec<String>,
) -> Result<(), WorkflowValidationError> {
    if visited.contains(phase_name) {
        return Ok(());
    }
    if !visiting.insert(phase_name.to_string()) {
        return Err(WorkflowValidationError::PhaseDependencyCycle {
            phase: phase_name.to_string(),
        });
    }

    let phase = &config.phases[phase_indices[phase_name]];
    for dependency in &phase.depends_on {
        visit_phase(
            dependency,
            config,
            phase_indices,
            visiting,
            visited,
            ordered,
        )?;
    }

    visiting.remove(phase_name);
    visited.insert(phase_name.to_string());
    ordered.push(phase_name.to_string());
    Ok(())
}

fn validate_parallel_write_scope(phase: &Phase) -> Result<(), WorkflowValidationError> {
    if !phase.parallel {
        return Ok(());
    }

    let write_tasks: Vec<_> = phase
        .tasks
        .iter()
        .filter(|task| task.mode == TaskMode::ReadWrite)
        .collect();

    for task in &write_tasks {
        if task.file_scope.is_empty() {
            return Err(WorkflowValidationError::MissingParallelWriteScope {
                task: task.id.clone(),
            });
        }
    }

    for (left_index, left) in write_tasks.iter().enumerate() {
        for right in write_tasks.iter().skip(left_index + 1) {
            if scopes_overlap(&left.file_scope, &right.file_scope) {
                return Err(WorkflowValidationError::OverlappingParallelWriteScope {
                    left: left.id.clone(),
                    right: right.id.clone(),
                });
            }
        }
    }

    Ok(())
}

pub fn scopes_overlap(left: &[String], right: &[String]) -> bool {
    left.iter().any(|left_scope| {
        right
            .iter()
            .any(|right_scope| scope_overlaps(left_scope, right_scope))
    })
}

fn scope_overlaps(left: &str, right: &str) -> bool {
    let left = normalize_file_scope_root(left);
    let right = normalize_file_scope_root(right);

    if left == right || left == "." || right == "." {
        return true;
    }

    if left.contains('*') || right.contains('*') {
        return glob_prefix(&left) == glob_prefix(&right);
    }

    let left_path = Path::new(&left);
    let right_path = Path::new(&right);
    left_path.starts_with(right_path) || right_path.starts_with(left_path)
}

/// Normalize the suffix-glob spelling accepted by declarative workflow
/// `file_scope` into the concrete directory root enforced at runtime.
#[must_use]
pub fn normalize_file_scope_root(scope: &str) -> String {
    let trimmed = scope.trim().trim_start_matches("./").trim_end_matches('/');
    trimmed
        .strip_suffix("/**")
        .or_else(|| trimmed.strip_suffix("/*"))
        .unwrap_or(trimmed)
        .to_string()
}

fn glob_prefix(scope: &str) -> String {
    scope
        .split('*')
        .next()
        .unwrap_or(scope)
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str) -> Task {
        Task {
            id: id.to_string(),
            prompt: format!("run {id}"),
            agent_type: AgentType::General,
            mode: TaskMode::ReadOnly,
            isolation: IsolationMode::Shared,
            file_scope: Vec::new(),
            depends_on_results: Vec::new(),
            max_steps: None,
            timeout_secs: None,
        }
    }

    fn config(phases: Vec<Phase>) -> WorkflowConfig {
        WorkflowConfig {
            goal: "cache-change".to_string(),
            max_concurrent: 4,
            description: None,
            phases,
        }
    }

    fn phase(name: &str, depends_on: &[&str], tasks: Vec<Task>) -> Phase {
        Phase {
            name: name.to_string(),
            description: None,
            depends_on: depends_on.iter().map(|value| value.to_string()).collect(),
            parallel: false,
            on_failure: FailurePolicy::SkipContinue,
            tasks,
        }
    }

    fn leaf_node(id: &str) -> WorkflowNode {
        WorkflowNode::Leaf(LeafSpec {
            id: id.to_string(),
            prompt: format!("run {id}"),
            agent_type: AgentType::General,
            role: None,
            profile: None,
            mode: TaskMode::ReadOnly,
            isolation: IsolationMode::Shared,
            file_scope: Vec::new(),
            cwd: None,
            depends_on_results: Vec::new(),
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
        })
    }

    fn leaf_node_with_budget(id: &str, budget: BudgetSpec) -> WorkflowNode {
        WorkflowNode::Leaf(LeafSpec {
            id: id.to_string(),
            prompt: format!("run {id}"),
            agent_type: AgentType::General,
            role: None,
            profile: None,
            mode: TaskMode::ReadOnly,
            isolation: IsolationMode::Shared,
            file_scope: Vec::new(),
            cwd: None,
            depends_on_results: Vec::new(),
            budget,
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
        })
    }

    fn invalid_leaf_node(id: &str) -> WorkflowNode {
        WorkflowNode::Leaf(LeafSpec {
            id: id.to_string(),
            prompt: " ".to_string(),
            agent_type: AgentType::General,
            role: None,
            profile: None,
            mode: TaskMode::ReadOnly,
            isolation: IsolationMode::Shared,
            file_scope: Vec::new(),
            cwd: None,
            depends_on_results: Vec::new(),
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
        })
    }

    fn workflow_spec(nodes: Vec<WorkflowNode>) -> WorkflowSpec {
        WorkflowSpec {
            id: Some("mock-workflow".to_string()),
            goal: "prove mock executor control flow".to_string(),
            description: None,
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
            promotion_policy: PromotionPolicy::default(),
            gates: Vec::new(),
            nodes,
        }
    }

    fn control_result<'a>(
        execution: &'a WorkflowExecution,
        node_id: &str,
    ) -> &'a ControlNodeResult {
        execution
            .control_node_results
            .iter()
            .find(|result| result.node_id == node_id)
            .expect("control node result should exist")
    }

    fn candidate(
        branch_id: &str,
        status: WorkflowRunStatus,
        score: u32,
        cost: u64,
        diversity_key: &str,
    ) -> BranchCandidate {
        BranchCandidate {
            branch_id: branch_id.to_string(),
            status,
            score,
            cost,
            diversity_key: Some(diversity_key.to_string()),
        }
    }

    #[test]
    fn independent_phases_preserve_declaration_order() {
        let workflow = config(vec![
            phase("discover", &[], vec![task("scan")]),
            phase("report", &[], vec![task("summarize")]),
        ]);

        let plan = workflow.compile().expect("workflow should compile");

        assert_eq!(
            plan.phase_names().collect::<Vec<_>>(),
            vec!["discover", "report"]
        );
    }

    #[test]
    fn dependencies_override_declaration_order_deterministically() {
        let workflow = config(vec![
            phase("review", &["implement"], vec![task("review-results")]),
            phase("discover", &[], vec![task("scan")]),
            phase("implement", &["discover"], vec![task("patch")]),
            phase("report", &["review"], vec![task("summarize")]),
        ]);

        let plan = workflow.compile().expect("workflow should compile");

        assert_eq!(
            plan.phase_names().collect::<Vec<_>>(),
            vec!["discover", "implement", "review", "report"]
        );
    }

    #[test]
    fn rejects_empty_workflow() {
        let err = config(Vec::new())
            .validate()
            .expect_err("empty workflow should fail");

        assert_eq!(err, WorkflowValidationError::EmptyWorkflow);
    }

    #[test]
    fn rejects_empty_phase() {
        let err = config(vec![phase("empty", &[], Vec::new())])
            .validate()
            .expect_err("empty phase should fail");

        assert_eq!(
            err,
            WorkflowValidationError::EmptyPhase {
                phase: "empty".to_string()
            }
        );
    }

    #[test]
    fn rejects_invalid_max_concurrent() {
        let mut workflow = config(vec![phase("discover", &[], vec![task("scan")])]);
        workflow.max_concurrent = 0;

        let err = workflow
            .validate()
            .expect_err("zero concurrency should fail");

        assert_eq!(
            err,
            WorkflowValidationError::InvalidMaxConcurrent { value: 0 }
        );
    }

    #[test]
    fn rejects_duplicate_phase_names() {
        let err = config(vec![
            phase("discover", &[], vec![task("scan")]),
            phase("discover", &[], vec![task("scan-again")]),
        ])
        .validate()
        .expect_err("duplicate phase should fail");

        assert!(matches!(
            err,
            WorkflowValidationError::DuplicatePhase { .. }
        ));
    }

    #[test]
    fn rejects_duplicate_task_ids() {
        let err = config(vec![
            phase("discover", &[], vec![task("scan")]),
            phase("report", &[], vec![task("scan")]),
        ])
        .validate()
        .expect_err("duplicate task should fail");

        assert!(matches!(err, WorkflowValidationError::DuplicateTask { .. }));
    }

    #[test]
    fn rejects_unknown_phase_dependency() {
        let err = config(vec![phase("report", &["missing"], vec![task("summarize")])])
            .validate()
            .expect_err("unknown dependency should fail");

        assert!(matches!(
            err,
            WorkflowValidationError::InvalidPhaseDependency { .. }
        ));
    }

    #[test]
    fn rejects_phase_dependency_cycles() {
        let workflow = config(vec![
            phase("a", &["b"], vec![task("a-task")]),
            phase("b", &["a"], vec![task("b-task")]),
        ]);

        let err = workflow.validate().expect_err("cycle should fail");

        assert!(matches!(
            err,
            WorkflowValidationError::PhaseDependencyCycle { .. }
        ));
    }

    #[test]
    fn rejects_task_result_dependency_from_same_parallel_phase() {
        let mut first = task("first");
        first.depends_on_results.push("second".to_string());
        let mut parallel = phase("parallel", &[], vec![first, task("second")]);
        parallel.parallel = true;

        let err = config(vec![parallel])
            .validate()
            .expect_err("same-phase result dependency should fail");

        assert!(matches!(
            err,
            WorkflowValidationError::UnavailableTaskResultDependency { .. }
        ));
    }

    #[test]
    fn rejects_task_result_dependency_from_later_phase() {
        let mut summarize = task("summarize");
        summarize.depends_on_results.push("scan".to_string());
        let workflow = config(vec![
            phase("report", &[], vec![summarize]),
            phase("discover", &[], vec![task("scan")]),
        ]);

        let err = workflow
            .validate()
            .expect_err("later-phase result dependency should fail");

        assert!(matches!(
            err,
            WorkflowValidationError::UnavailableTaskResultDependency { .. }
        ));
    }

    #[test]
    fn allows_task_result_dependency_from_earlier_phase() {
        let upstream = phase("discover", &[], vec![task("scan")]);
        let mut summarize = task("summarize");
        summarize.depends_on_results.push("scan".to_string());
        let downstream = phase("report", &["discover"], vec![summarize]);

        config(vec![upstream, downstream])
            .validate()
            .expect("earlier-phase result should be available");
    }

    #[test]
    fn rejects_parallel_read_write_without_file_scope() {
        let mut write = task("write");
        write.mode = TaskMode::ReadWrite;
        let mut parallel = phase("parallel", &[], vec![write]);
        parallel.parallel = true;

        let err = config(vec![parallel])
            .validate()
            .expect_err("write task needs a scope");

        assert!(matches!(
            err,
            WorkflowValidationError::MissingParallelWriteScope { .. }
        ));
    }

    #[test]
    fn detects_overlapping_parallel_write_scopes_with_path_boundaries() {
        let mut left = task("auth");
        left.mode = TaskMode::ReadWrite;
        left.file_scope = vec!["src/auth/**".to_string()];
        let mut right = task("auth-login");
        right.mode = TaskMode::ReadWrite;
        right.file_scope = vec!["src/auth/login.rs".to_string()];
        let mut parallel = phase("parallel", &[], vec![left, right]);
        parallel.parallel = true;

        let err = config(vec![parallel])
            .validate()
            .expect_err("nested scopes should overlap");

        assert!(matches!(
            err,
            WorkflowValidationError::OverlappingParallelWriteScope { .. }
        ));
    }

    #[test]
    fn does_not_confuse_path_prefixes_for_overlapping_scopes() {
        let mut left = task("auth");
        left.mode = TaskMode::ReadWrite;
        left.file_scope = vec!["src/auth/**".to_string()];
        let mut right = task("auth-admin");
        right.mode = TaskMode::ReadWrite;
        right.file_scope = vec!["src/auth_admin/**".to_string()];
        let mut parallel = phase("parallel", &[], vec![left, right]);
        parallel.parallel = true;

        config(vec![parallel])
            .validate()
            .expect("component boundary scopes should not overlap");
    }

    #[test]
    fn json_roundtrip_keeps_snake_case_enum_names() {
        let mut task = task("patch");
        task.agent_type = AgentType::Implementer;
        task.mode = TaskMode::ReadWrite;
        task.isolation = IsolationMode::Worktree;
        task.file_scope = vec!["src/auth/**".to_string()];
        let mut parallel = phase("implement", &[], vec![task]);
        parallel.parallel = true;
        parallel.on_failure = FailurePolicy::Abort;
        let workflow = config(vec![parallel]);

        let json = serde_json::to_string(&workflow).expect("serialize workflow");

        assert!(json.contains("\"agent_type\":\"implement\""));
        assert!(json.contains("\"mode\":\"read_write\""));
        assert!(json.contains("\"isolation\":\"worktree\""));
        assert!(json.contains("\"on_failure\":\"abort\""));

        let parsed: WorkflowConfig = serde_json::from_str(&json).expect("parse workflow");
        assert_eq!(parsed, workflow);
    }

    #[test]
    fn json_accepts_pre_rename_agent_type_spellings_as_aliases() {
        for (legacy, expected) in [
            ("implementer", AgentType::Implementer),
            ("builder", AgentType::Implementer),
            ("verifier", AgentType::Verifier),
            ("verify", AgentType::Verifier),
            ("scout", AgentType::Explore),
            ("review", AgentType::Review),
        ] {
            let parsed: AgentType =
                serde_json::from_value(serde_json::json!(legacy)).expect("legacy alias must parse");
            assert_eq!(parsed, expected, "alias {legacy}");
        }
        // Canonical spellings parse and round-trip back as themselves.
        for (canonical, expected) in [
            ("implement", AgentType::Implementer),
            ("test", AgentType::Verifier),
            ("explore", AgentType::Explore),
            ("general", AgentType::General),
        ] {
            let parsed: AgentType =
                serde_json::from_value(serde_json::json!(canonical)).expect("canonical parses");
            assert_eq!(parsed, expected);
            assert_eq!(
                serde_json::to_value(parsed).expect("serialize"),
                serde_json::json!(canonical),
                "canonical spelling must serialize as itself"
            );
        }
    }

    #[test]
    fn isolation_auto_defaults_parallel_write_to_worktree() {
        assert_eq!(IsolationMode::default(), IsolationMode::Auto);
        assert_eq!(
            IsolationMode::Auto.resolve(/* parallel_write */ true),
            IsolationMode::Worktree
        );
        assert_eq!(
            IsolationMode::Auto.resolve(/* parallel_write */ false),
            IsolationMode::Shared
        );
        // Explicit shared is the approved same-worktree override.
        assert_eq!(
            IsolationMode::Shared.resolve(/* parallel_write */ true),
            IsolationMode::Shared
        );
        assert!(IsolationMode::Worktree.wants_worktree(false));
        assert!(!IsolationMode::Shared.wants_worktree(true));
    }

    #[test]
    fn leaf_write_capable_and_worktree_defaults() {
        let read_only = LeafSpec {
            id: "ro".to_string(),
            prompt: "inspect".to_string(),
            agent_type: AgentType::Explore,
            role: None,
            profile: None,
            mode: TaskMode::ReadOnly,
            isolation: IsolationMode::Auto,
            file_scope: Vec::new(),
            cwd: None,
            depends_on_results: Vec::new(),
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
        };
        assert!(!leaf_is_write_capable(&read_only));
        assert!(!leaf_wants_worktree(&read_only, true));

        let mut read_only_implementer = read_only.clone();
        read_only_implementer.id = "ro-implementer".to_string();
        read_only_implementer.agent_type = AgentType::Implementer;
        assert!(
            !leaf_is_write_capable(&read_only_implementer),
            "role identity must not grant write authority"
        );
        assert!(
            !leaf_wants_worktree(&read_only_implementer, true),
            "parallel read-only implementers stay shared under auto isolation"
        );

        let mut write = read_only.clone();
        write.id = "rw".to_string();
        write.mode = TaskMode::ReadWrite;
        write.agent_type = AgentType::Implementer;
        assert!(leaf_is_write_capable(&write));
        // Parallel write-capable + Auto → worktree by default.
        assert!(leaf_wants_worktree(&write, true));
        // Sequential write-capable stays shared unless isolation is worktree.
        assert!(!leaf_wants_worktree(&write, false));

        write.isolation = IsolationMode::Shared;
        assert!(
            !leaf_wants_worktree(&write, true),
            "explicit shared is the same-worktree override"
        );

        write.isolation = IsolationMode::Worktree;
        assert!(leaf_wants_worktree(&write, true));
        assert!(leaf_wants_worktree(&write, false));
    }

    #[test]
    fn workflow_ir_roundtrip() {
        let discover_leaf = LeafSpec {
            id: "scan-readme".to_string(),
            prompt: "Inspect README setup gaps".to_string(),
            agent_type: AgentType::Explore,
            role: None,
            profile: Some("scout".to_string()),
            mode: TaskMode::ReadOnly,
            isolation: IsolationMode::Shared,
            file_scope: vec!["README.md".to_string()],
            cwd: None,
            depends_on_results: Vec::new(),
            budget: BudgetSpec {
                max_steps: Some(8),
                timeout_secs: Some(300),
                max_parallel: None,
                max_tokens: None,
            },
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy {
                provider: Some("openai".to_string()),
                model: Some("gpt-5.4".to_string()),
                fallback_models: Vec::new(),
            },
        };
        let workflow = WorkflowSpec {
            id: Some("v090-readme-check".to_string()),
            goal: "tighten setup docs".to_string(),
            description: Some("metadata-only typed Workflow IR".to_string()),
            budget: BudgetSpec {
                max_steps: Some(30),
                timeout_secs: Some(1_800),
                max_parallel: Some(2),
                max_tokens: None,
            },
            permissions: PermissionSpec {
                allow_write: false,
                allow_network: false,
                deny_all_tools: false,
                allowed_tools: vec!["rg".to_string()],
                file_scope: vec!["README.md".to_string()],
            },
            model_policy: ModelPolicy {
                provider: Some("openai".to_string()),
                model: Some("gpt-5.4".to_string()),
                fallback_models: vec!["gpt-5.4-mini".to_string()],
            },
            promotion_policy: PromotionPolicy {
                strategy: PromotionStrategy::TeacherSelected,
                require_teacher_review: true,
                min_successful_branches: Some(1),
                promotion_gate: PromotionGate::default(),
            },
            gates: Vec::new(),
            nodes: vec![
                WorkflowNode::BranchSet(BranchSpec {
                    id: "discover".to_string(),
                    description: Some("parallel doc inspection".to_string()),
                    parallel: true,
                    budget: BudgetSpec {
                        max_steps: Some(12),
                        timeout_secs: Some(600),
                        max_parallel: Some(2),
                        max_tokens: None,
                    },
                    permissions: PermissionSpec::default(),
                    model_policy: ModelPolicy::default(),
                    children: vec![WorkflowNode::Leaf(discover_leaf)],
                }),
                WorkflowNode::Sequence(SequenceSpec {
                    id: "review-and-reduce".to_string(),
                    children: vec![
                        WorkflowNode::TeacherReview(TeacherReviewSpec {
                            id: "select-best".to_string(),
                            candidates: vec!["scan-readme".to_string()],
                            promotion_policy: PromotionPolicy {
                                strategy: PromotionStrategy::BestScore,
                                require_teacher_review: true,
                                min_successful_branches: Some(1),
                                promotion_gate: PromotionGate::default(),
                            },
                        }),
                        WorkflowNode::Reduce(ReduceSpec {
                            id: "summarize".to_string(),
                            inputs: vec!["scan-readme".to_string()],
                            prompt: "Summarize the smallest safe patch".to_string(),
                            model_policy: ModelPolicy::default(),
                        }),
                    ],
                }),
                WorkflowNode::Cond(CondSpec {
                    id: "maybe-expand".to_string(),
                    condition: "summary identifies multiple independent gaps".to_string(),
                    then_nodes: vec![WorkflowNode::Expand(ExpandSpec {
                        id: "split-followups".to_string(),
                        source: "summarize".to_string(),
                        max_children: None,
                        template: Some(Box::new(WorkflowNode::Leaf(LeafSpec {
                            id: "followup-template".to_string(),
                            prompt: "Patch one independent gap".to_string(),
                            agent_type: AgentType::Implementer,
                            role: None,
                            profile: None,
                            mode: TaskMode::ReadWrite,
                            isolation: IsolationMode::Worktree,
                            file_scope: vec!["README.md".to_string()],
                            cwd: None,
                            depends_on_results: Vec::new(),
                            budget: BudgetSpec::default(),
                            permissions: PermissionSpec {
                                allow_write: true,
                                allow_network: false,
                                deny_all_tools: false,
                                allowed_tools: Vec::new(),
                                file_scope: vec!["README.md".to_string()],
                            },
                            model_policy: ModelPolicy::default(),
                        }))),
                    })],
                    else_nodes: vec![WorkflowNode::LoopUntil(LoopUntilSpec {
                        id: "verify-once".to_string(),
                        condition: "local verification passes".to_string(),
                        max_iterations: Some(1),
                        children: Vec::new(),
                    })],
                }),
            ],
        };

        let json = serde_json::to_string_pretty(&workflow).expect("serialize workflow ir");

        assert!(json.contains("\"kind\": \"branch_set\""));
        assert!(json.contains("\"strategy\": \"teacher_selected\""));
        assert!(json.contains("\"profile\": \"scout\""));
        let parsed: WorkflowSpec = serde_json::from_str(&json).expect("parse workflow ir");
        assert_eq!(parsed, workflow);

        let minimal: WorkflowSpec = serde_json::from_str(r#"{"goal":"ship v0.9","nodes":[]}"#)
            .expect("parse minimal workflow ir");
        assert_eq!(minimal.budget, BudgetSpec::default());
        assert_eq!(minimal.permissions, PermissionSpec::default());
        assert_eq!(minimal.model_policy, ModelPolicy::default());

        // Pre-profile leaf IR stays parseable and profile-less leaves omit the key.
        let legacy_leaf: LeafSpec = serde_json::from_str(r#"{"id":"scan","prompt":"scan safely"}"#)
            .expect("parse pre-profile leaf ir");
        assert_eq!(legacy_leaf.profile, None);
        let legacy_json = serde_json::to_string(&legacy_leaf).expect("serialize legacy leaf");
        assert!(!legacy_json.contains("profile"));
    }

    #[test]
    fn fleet_validation_accepts_one_thousand_agents_and_variable_models() {
        let nodes = (0..DEFAULT_FLEET_WORKFLOW_MAX_AGENTS)
            .map(|index| {
                let mut leaf = match leaf_node(&format!("agent-{index}")) {
                    WorkflowNode::Leaf(leaf) => leaf,
                    _ => unreachable!("leaf helper returns a leaf"),
                };
                leaf.model_policy = if index == 0 {
                    ModelPolicy {
                        provider: Some("deepseek".to_string()),
                        model: Some("deepseek-v4-pro".to_string()),
                        fallback_models: Vec::new(),
                    }
                } else {
                    ModelPolicy {
                        provider: Some("deepseek".to_string()),
                        model: Some("deepseek-v4-flash".to_string()),
                        fallback_models: Vec::new(),
                    }
                };
                WorkflowNode::Leaf(leaf)
            })
            .collect();
        let workflow = workflow_spec(nodes);

        let shape = workflow
            .validate_for_fleet()
            .expect("one thousand agents should fit the Fleet Workflow limit");

        assert_eq!(shape.total_agents, DEFAULT_FLEET_WORKFLOW_MAX_AGENTS);
        assert_eq!(shape.max_depth, 1);
    }

    #[test]
    fn fleet_validation_rejects_more_than_one_thousand_agents() {
        let nodes = (0..=DEFAULT_FLEET_WORKFLOW_MAX_AGENTS)
            .map(|index| leaf_node(&format!("agent-{index}")))
            .collect();
        let workflow = workflow_spec(nodes);

        let err = workflow
            .validate_for_fleet()
            .expect_err("agent population should be bounded before Fleet launch");

        assert_eq!(
            err,
            WorkflowFleetLimitError::TooManyAgents {
                total_agents: DEFAULT_FLEET_WORKFLOW_MAX_AGENTS + 1,
                max_total_agents: DEFAULT_FLEET_WORKFLOW_MAX_AGENTS,
            }
        );
    }

    #[test]
    fn fleet_validation_rejects_depth_beyond_five() {
        let mut node = leaf_node("deep-leaf");
        for depth in (0..DEFAULT_FLEET_WORKFLOW_MAX_DEPTH).rev() {
            node = WorkflowNode::BranchSet(BranchSpec {
                id: format!("ring-{depth}"),
                description: None,
                parallel: true,
                budget: BudgetSpec::default(),
                permissions: PermissionSpec::default(),
                model_policy: ModelPolicy::default(),
                children: vec![node],
            });
        }
        let workflow = workflow_spec(vec![node]);

        let err = workflow
            .validate_for_fleet()
            .expect_err("sixth agent ring should be rejected");

        assert_eq!(
            err,
            WorkflowFleetLimitError::RecursionTooDeep {
                depth: DEFAULT_FLEET_WORKFLOW_MAX_DEPTH + 1,
                max_depth: DEFAULT_FLEET_WORKFLOW_MAX_DEPTH,
            }
        );
    }

    #[test]
    fn fleet_validation_counts_loop_and_expand_fanout_conservatively() {
        let workflow = workflow_spec(vec![
            WorkflowNode::LoopUntil(LoopUntilSpec {
                id: "retry-ring".to_string(),
                condition: "verifier passes".to_string(),
                max_iterations: Some(3),
                children: vec![leaf_node("retry-worker")],
            }),
            WorkflowNode::Expand(ExpandSpec {
                id: "split".to_string(),
                source: "retry-ring".to_string(),
                max_children: Some(4),
                template: Some(Box::new(leaf_node("split-template"))),
            }),
        ]);

        let shape = workflow
            .validate_for_fleet()
            .expect("bounded loop and expand should validate");

        assert_eq!(shape.total_agents, 7);
        assert_eq!(shape.max_depth, 1);
    }

    #[test]
    fn fleet_validation_rejects_unbounded_loop_or_expand_before_launch() {
        let workflow = workflow_spec(vec![
            WorkflowNode::LoopUntil(LoopUntilSpec {
                id: "retry-ring".to_string(),
                condition: "verifier passes".to_string(),
                max_iterations: None,
                children: vec![leaf_node("retry-worker")],
            }),
            WorkflowNode::Expand(ExpandSpec {
                id: "split".to_string(),
                source: "retry-ring".to_string(),
                max_children: Some(4),
                template: Some(Box::new(leaf_node("split-template"))),
            }),
        ]);

        assert!(matches!(
            workflow.validate_for_fleet(),
            Err(WorkflowFleetLimitError::UnboundedLoop { node }) if node == "retry-ring"
        ));

        let workflow = workflow_spec(vec![WorkflowNode::Expand(ExpandSpec {
            id: "split".to_string(),
            source: "retry-ring".to_string(),
            max_children: None,
            template: Some(Box::new(leaf_node("split-template"))),
        })]);

        assert!(matches!(
            workflow.validate_for_fleet(),
            Err(WorkflowFleetLimitError::UnboundedExpand { node }) if node == "split"
        ));
    }

    #[test]
    fn branch_result_serialization() {
        let result = BranchResult {
            branch_id: "discover".to_string(),
            task_id: "scan".to_string(),
            status: WorkflowRunStatus::Succeeded,
            usage: WorkflowUsage {
                input_tokens: Some(100),
                output_tokens: Some(25),
                cost_microusd: Some(42),
            },
            memo_usage: WorkflowMemoUsage::default(),
            artifacts: vec!["trace://branches/discover".to_string()],
            notes: Some("validated prompt surfaces".to_string()),
        };

        let json = serde_json::to_string(&result).expect("serialize branch result");

        assert!(json.contains("\"status\":\"succeeded\""));
        assert!(json.contains("\"cost_microusd\":42"));
        let parsed: BranchResult = serde_json::from_str(&json).expect("parse branch result");
        assert_eq!(parsed, result);

        let minimal: BranchResult =
            serde_json::from_str(r#"{"branch_id":"discover","task_id":"scan","status":"pending"}"#)
                .expect("parse minimal branch result");
        assert_eq!(minimal.usage, WorkflowUsage::default());
        assert_eq!(minimal.memo_usage, WorkflowMemoUsage::default());
        assert!(minimal.artifacts.is_empty());
        assert_eq!(minimal.notes, None);
    }

    #[test]
    fn workflow_usage_serialization_distinguishes_unknown_from_reported_zero() {
        let unknown = serde_json::to_value(WorkflowUsage::default()).expect("unknown usage JSON");
        assert_eq!(unknown, serde_json::json!({}));

        let reported_zero = serde_json::to_value(WorkflowUsage {
            input_tokens: Some(0),
            output_tokens: Some(0),
            cost_microusd: Some(0),
        })
        .expect("reported zero usage JSON");
        assert_eq!(
            reported_zero,
            serde_json::json!({
                "input_tokens": 0,
                "output_tokens": 0,
                "cost_microusd": 0,
            })
        );
    }

    #[test]
    fn leaf_result_serialization() {
        let result = LeafResult {
            leaf_id: "scan-readme".to_string(),
            task_id: "scan".to_string(),
            role: None,
            profile: Some("reviewer".to_string()),
            status: WorkflowRunStatus::Failed,
            usage: WorkflowUsage {
                input_tokens: Some(11),
                output_tokens: Some(7),
                cost_microusd: Some(3),
            },
            memo_usage: WorkflowMemoUsage {
                armh_hits: 1,
                armh_misses: 0,
                armh_saved_estimated_tokens: 128,
                provider_prompt_cache_hits: 2,
                provider_prompt_cache_misses: 1,
            },
            output: Some("README needs clearer setup steps".to_string()),
            artifacts: vec!["trace://leaves/scan-readme".to_string()],
            schema_error: None,
        };

        let json = serde_json::to_string(&result).expect("serialize leaf result");

        assert!(json.contains("\"status\":\"failed\""));
        assert!(json.contains("\"input_tokens\":11"));
        assert!(json.contains("\"armh_saved_estimated_tokens\":128"));
        assert!(json.contains("\"profile\":\"reviewer\""));
        let parsed: LeafResult = serde_json::from_str(&json).expect("parse leaf result");
        assert_eq!(parsed, result);

        let minimal: LeafResult = serde_json::from_str(
            r#"{"leaf_id":"scan-readme","task_id":"scan","status":"pending"}"#,
        )
        .expect("parse minimal leaf result");
        assert_eq!(minimal.profile, None);
        assert_eq!(minimal.usage, WorkflowUsage::default());
        assert_eq!(minimal.memo_usage, WorkflowMemoUsage::default());
        assert_eq!(minimal.output, None);
        assert!(minimal.artifacts.is_empty());
    }

    #[test]
    fn control_node_result_serialization() {
        let result = ControlNodeResult {
            node_id: "select-fix".to_string(),
            kind: ControlNodeKind::TeacherReview,
            status: WorkflowRunStatus::Running,
            selected_children: vec!["branch-a".to_string(), "branch-c".to_string()],
            summary: Some("teacher review is waiting on verifier evidence".to_string()),
        };

        let json = serde_json::to_string(&result).expect("serialize control node result");

        assert!(json.contains("\"kind\":\"teacher_review\""));
        assert!(json.contains("\"status\":\"running\""));
        let parsed: ControlNodeResult =
            serde_json::from_str(&json).expect("parse control node result");
        assert_eq!(parsed, result);

        let minimal: ControlNodeResult = serde_json::from_str(
            r#"{"node_id":"select-fix","kind":"branch_set","status":"pending"}"#,
        )
        .expect("parse minimal control node result");
        assert!(minimal.selected_children.is_empty());
        assert_eq!(minimal.summary, None);
    }

    #[test]
    fn run_mock_three_branch_workflow() {
        let workflow = workflow_spec(vec![WorkflowNode::BranchSet(BranchSpec {
            id: "discover".to_string(),
            description: None,
            parallel: true,
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
            children: vec![
                leaf_node("scan-readme"),
                leaf_node("scan-config"),
                leaf_node("scan-tests"),
            ],
        })]);

        let mut executor = MockWorkflowExecutor::new();
        let execution = executor.run(&workflow).expect("mock workflow should run");

        assert_eq!(execution.status, WorkflowRunStatus::Succeeded);
        assert_eq!(
            execution
                .leaf_results
                .iter()
                .map(|result| result.leaf_id.as_str())
                .collect::<Vec<_>>(),
            vec!["scan-readme", "scan-config", "scan-tests"]
        );
        assert_eq!(execution.branch_results.len(), 1);
        assert_eq!(execution.branch_results[0].branch_id, "discover");
        assert_eq!(
            control_result(&execution, "discover").selected_children,
            vec!["scan-readme", "scan-config", "scan-tests"]
        );
    }

    #[test]
    fn mock_executor_surfaces_leaf_profile() {
        let mut profiled_leaf = match leaf_node("review-change") {
            WorkflowNode::Leaf(leaf) => leaf,
            _ => unreachable!("leaf helper returns a leaf"),
        };
        profiled_leaf.profile = Some("reviewer".to_string());
        let workflow = workflow_spec(vec![
            WorkflowNode::Leaf(profiled_leaf),
            leaf_node("scan-readme"),
        ]);

        let execution = MockWorkflowExecutor::new()
            .run(&workflow)
            .expect("mock workflow should run");

        assert_eq!(execution.status, WorkflowRunStatus::Succeeded);
        assert_eq!(
            execution.leaf_results[0].profile.as_deref(),
            Some("reviewer")
        );
        assert_eq!(execution.leaf_results[1].profile, None);
    }

    #[test]
    fn mock_executor_surfaces_leaf_role() {
        let mut role_leaf = match leaf_node("scout-issue") {
            WorkflowNode::Leaf(leaf) => leaf,
            _ => unreachable!("leaf helper returns a leaf"),
        };
        role_leaf.role = Some("scout".to_string());
        let workflow = workflow_spec(vec![WorkflowNode::Leaf(role_leaf)]);

        let execution = MockWorkflowExecutor::new()
            .run(&workflow)
            .expect("mock workflow should run");

        assert_eq!(execution.leaf_results[0].role.as_deref(), Some("scout"));
    }

    #[test]
    fn leaf_role_token_rule_rejects_invalid_names() {
        for bad in ["", "has space", "role=scout"] {
            let mut leaf = match leaf_node("scan") {
                WorkflowNode::Leaf(leaf) => leaf,
                _ => unreachable!("leaf helper returns a leaf"),
            };
            leaf.role = Some(bad.to_string());
            let workflow = workflow_spec(vec![WorkflowNode::Leaf(leaf)]);

            let err = MockWorkflowExecutor::new()
                .run(&workflow)
                .expect_err("invalid role token should fail validation");

            assert!(
                matches!(&err, WorkflowExecutionError::InvalidLeafRole { role, .. } if role == bad),
                "role `{bad}` should be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn leaf_role_roundtrips_without_required_provider_model() {
        let leaf = LeafSpec {
            id: "scout-1".to_string(),
            prompt: "Investigate #4090. Read-only.".to_string(),
            agent_type: AgentType::Explore,
            role: Some("scout".to_string()),
            profile: None,
            mode: TaskMode::ReadOnly,
            isolation: IsolationMode::Shared,
            file_scope: Vec::new(),
            cwd: None,
            depends_on_results: Vec::new(),
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
        };
        let json = serde_json::to_string(&leaf).expect("serialize");
        assert!(json.contains("\"role\":\"scout\""));
        let parsed: LeafSpec = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed.role.as_deref(), Some("scout"));
        // Provider/model are optional overrides, not required identity fields.
        assert_eq!(parsed.model_policy.provider, None);
        assert_eq!(parsed.model_policy.model, None);
        assert_eq!(parsed.model_policy, ModelPolicy::default());
    }

    #[test]
    fn leaf_profile_token_rule_rejects_invalid_names() {
        for bad in ["", "has space", "quote\"y", "role=reviewer", "back`tick"] {
            let mut leaf = match leaf_node("scan") {
                WorkflowNode::Leaf(leaf) => leaf,
                _ => unreachable!("leaf helper returns a leaf"),
            };
            leaf.profile = Some(bad.to_string());
            let workflow = workflow_spec(vec![WorkflowNode::Leaf(leaf)]);

            let err = MockWorkflowExecutor::new()
                .run(&workflow)
                .expect_err("invalid profile token should fail validation");

            assert!(
                matches!(&err, WorkflowExecutionError::InvalidLeafProfile { profile, .. } if profile == bad),
                "profile `{bad}` should be rejected, got {err:?}"
            );
        }

        let mut leaf = match leaf_node("scan") {
            WorkflowNode::Leaf(leaf) => leaf,
            _ => unreachable!("leaf helper returns a leaf"),
        };
        leaf.profile = Some("reviewer".to_string());
        let workflow = workflow_spec(vec![WorkflowNode::Leaf(leaf)]);
        MockWorkflowExecutor::new()
            .run(&workflow)
            .expect("valid profile token should pass validation");
    }

    #[test]
    fn mock_executor_aggregates_leaf_usage() {
        let workflow = workflow_spec(vec![WorkflowNode::BranchSet(BranchSpec {
            id: "discover".to_string(),
            description: None,
            parallel: true,
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
            children: vec![leaf_node("scan-readme"), leaf_node("scan-tests")],
        })]);

        let mut executor = MockWorkflowExecutor::new()
            .with_leaf_outcome(
                "scan-readme",
                MockLeafOutcome::succeeded("readme ok").with_usage(WorkflowUsage {
                    input_tokens: Some(100),
                    output_tokens: Some(25),
                    cost_microusd: Some(500),
                }),
            )
            .with_leaf_outcome(
                "scan-tests",
                MockLeafOutcome::succeeded("tests ok").with_usage(WorkflowUsage {
                    input_tokens: Some(50),
                    output_tokens: Some(10),
                    cost_microusd: Some(250),
                }),
            );

        let execution = executor.run(&workflow).expect("mock workflow should run");

        assert_eq!(
            execution.usage,
            WorkflowUsage {
                input_tokens: Some(150),
                output_tokens: Some(35),
                cost_microusd: Some(750),
            }
        );
        assert_eq!(execution.usage.total_tokens(), Some(185));
        assert_eq!(execution.branch_results[0].usage, execution.usage);
        assert_eq!(
            execution
                .leaf_results
                .iter()
                .map(|result| result.usage.cost_microusd)
                .collect::<Vec<_>>(),
            vec![Some(500), Some(250)]
        );
    }

    #[test]
    fn mock_executor_aggregates_memo_usage() {
        let workflow = workflow_spec(vec![WorkflowNode::BranchSet(BranchSpec {
            id: "cache-branches".to_string(),
            description: None,
            parallel: true,
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
            children: vec![leaf_node("rlm-hit"), leaf_node("rlm-miss")],
        })]);

        let mut executor = MockWorkflowExecutor::new()
            .with_leaf_outcome(
                "rlm-hit",
                MockLeafOutcome::succeeded("memo hit").with_memo_usage(WorkflowMemoUsage {
                    armh_hits: 1,
                    armh_misses: 0,
                    armh_saved_estimated_tokens: 4096,
                    provider_prompt_cache_hits: 1,
                    provider_prompt_cache_misses: 0,
                }),
            )
            .with_leaf_outcome(
                "rlm-miss",
                MockLeafOutcome::succeeded("memo miss").with_memo_usage(WorkflowMemoUsage {
                    armh_hits: 0,
                    armh_misses: 1,
                    armh_saved_estimated_tokens: 0,
                    provider_prompt_cache_hits: 0,
                    provider_prompt_cache_misses: 1,
                }),
            );

        let execution = executor.run(&workflow).expect("mock workflow should run");

        assert_eq!(
            execution.memo_usage,
            WorkflowMemoUsage {
                armh_hits: 1,
                armh_misses: 1,
                armh_saved_estimated_tokens: 4096,
                provider_prompt_cache_hits: 1,
                provider_prompt_cache_misses: 1,
            }
        );
        assert_eq!(execution.branch_results[0].memo_usage, execution.memo_usage);
        assert_eq!(
            execution
                .leaf_results
                .iter()
                .map(|result| (result.memo_usage.armh_hits, result.memo_usage.armh_misses))
                .collect::<Vec<_>>(),
            vec![(1, 0), (0, 1)]
        );
    }

    #[test]
    fn mock_executor_marks_cancelled_before_leaf() {
        let workflow = workflow_spec(vec![WorkflowNode::BranchSet(BranchSpec {
            id: "discover".to_string(),
            description: None,
            parallel: true,
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
            children: vec![leaf_node("scan-readme"), leaf_node("scan-tests")],
        })]);

        let mut executor = MockWorkflowExecutor::new().with_cancelled();
        let execution = executor.run(&workflow).expect("mock workflow should run");

        assert_eq!(execution.status, WorkflowRunStatus::Cancelled);
        assert_eq!(execution.leaf_results.len(), 1);
        assert_eq!(
            execution.leaf_results[0].status,
            WorkflowRunStatus::Cancelled
        );
        assert_eq!(
            execution.branch_results[0].status,
            WorkflowRunStatus::Cancelled
        );
        assert_eq!(
            control_result(&execution, "discover").status,
            WorkflowRunStatus::Cancelled
        );
    }

    #[test]
    fn mock_executor_stops_when_global_leaf_budget_is_exhausted() {
        let workflow = workflow_spec(vec![WorkflowNode::BranchSet(BranchSpec {
            id: "discover".to_string(),
            description: None,
            parallel: true,
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
            children: vec![
                leaf_node("scan-readme"),
                leaf_node("scan-config"),
                leaf_node("scan-tests"),
            ],
        })]);

        let mut executor = MockWorkflowExecutor::new().with_max_leaf_steps(1);
        let execution = executor.run(&workflow).expect("mock workflow should run");

        assert_eq!(execution.status, WorkflowRunStatus::BudgetExceeded);
        assert_eq!(
            execution
                .leaf_results
                .iter()
                .map(|result| (result.leaf_id.as_str(), result.status))
                .collect::<Vec<_>>(),
            vec![
                ("scan-readme", WorkflowRunStatus::Succeeded),
                ("scan-config", WorkflowRunStatus::BudgetExceeded)
            ]
        );
        assert_eq!(
            execution.branch_results[0].status,
            WorkflowRunStatus::BudgetExceeded
        );
    }

    #[test]
    fn mock_executor_treats_zero_step_budgets_as_unbounded() {
        let workflow = workflow_spec(vec![WorkflowNode::BranchSet(BranchSpec {
            id: "verify".to_string(),
            description: None,
            parallel: false,
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
            children: vec![
                leaf_node_with_budget(
                    "run-tests",
                    BudgetSpec {
                        max_steps: Some(0),
                        timeout_secs: None,
                        max_parallel: None,
                        max_tokens: None,
                    },
                ),
                leaf_node("summarize"),
            ],
        })]);

        let mut executor = MockWorkflowExecutor::new().with_max_leaf_steps(0);
        let execution = executor.run(&workflow).expect("mock workflow should run");

        assert_eq!(execution.status, WorkflowRunStatus::Succeeded);
        assert_eq!(execution.leaf_results.len(), 2);
        assert!(
            execution
                .leaf_results
                .iter()
                .all(|result| result.status == WorkflowRunStatus::Succeeded)
        );
    }

    #[test]
    fn mock_executor_stops_when_global_token_budget_is_exhausted() {
        let workflow = workflow_spec(vec![WorkflowNode::BranchSet(BranchSpec {
            id: "discover".to_string(),
            description: None,
            parallel: true,
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
            children: vec![
                leaf_node("scan-readme"),
                leaf_node("scan-config"),
                leaf_node("scan-tests"),
            ],
        })]);

        // First leaf uses 600 tokens (300 in + 300 out); after the second leaf
        // (500 tokens) the running total is 1100, exceeding the 1000-token
        // global cap, so the third leaf hits the exhausted budget and halts the
        // run.
        let mut executor = MockWorkflowExecutor::new()
            .with_max_leaf_tokens(1000)
            .with_leaf_outcome(
                "scan-readme",
                MockLeafOutcome::succeeded("readme done").with_usage(WorkflowUsage {
                    input_tokens: Some(300),
                    output_tokens: Some(300),
                    cost_microusd: Some(0),
                }),
            )
            .with_leaf_outcome(
                "scan-config",
                MockLeafOutcome::succeeded("config done").with_usage(WorkflowUsage {
                    input_tokens: Some(250),
                    output_tokens: Some(250),
                    cost_microusd: Some(0),
                }),
            );
        let execution = executor.run(&workflow).expect("mock workflow should run");

        assert_eq!(execution.status, WorkflowRunStatus::BudgetExceeded);
        // Leaves 1+2 consume 1100 tokens, exhausting the 1000-token global cap.
        // The third leaf is attempted, sees the budget already exceeded, and is
        // recorded as BudgetExceeded — the same boundary-leaf behaviour used by
        // step budgets (max_leaf_steps). The budget outcome carries no tokens,
        // so total usage stays at 1100.
        assert_eq!(execution.leaf_results.len(), 3);
        assert_eq!(
            execution.leaf_results[0].status,
            WorkflowRunStatus::Succeeded
        );
        assert_eq!(
            execution.leaf_results[1].status,
            WorkflowRunStatus::Succeeded
        );
        assert_eq!(
            execution.leaf_results[2].status,
            WorkflowRunStatus::BudgetExceeded
        );
        assert_eq!(execution.usage.total_tokens(), Some(1100));
    }

    #[test]
    fn mock_executor_honors_zero_token_leaf_budget() {
        let workflow = workflow_spec(vec![WorkflowNode::BranchSet(BranchSpec {
            id: "verify".to_string(),
            description: None,
            parallel: false,
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
            children: vec![
                leaf_node_with_budget(
                    "run-tests",
                    BudgetSpec {
                        max_steps: None,
                        timeout_secs: None,
                        max_parallel: None,
                        max_tokens: Some(0),
                    },
                ),
                leaf_node("summarize"),
            ],
        })]);

        let mut executor = MockWorkflowExecutor::new();
        let execution = executor.run(&workflow).expect("mock workflow should run");

        assert_eq!(execution.status, WorkflowRunStatus::BudgetExceeded);
        assert_eq!(execution.leaf_results.len(), 1);
        assert_eq!(
            execution.leaf_results[0].status,
            WorkflowRunStatus::BudgetExceeded
        );
        assert!(
            execution.leaf_results[0]
                .output
                .as_deref()
                .unwrap_or_default()
                .contains("token budget exhausted")
        );
    }

    #[test]
    fn mock_executor_honors_per_leaf_token_cap() {
        let workflow = workflow_spec(vec![WorkflowNode::BranchSet(BranchSpec {
            id: "review".to_string(),
            description: None,
            parallel: false,
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
            children: vec![
                leaf_node_with_budget(
                    "expensive-scan",
                    BudgetSpec {
                        max_steps: None,
                        timeout_secs: None,
                        max_parallel: None,
                        max_tokens: Some(500),
                    },
                ),
                leaf_node("summarize"),
            ],
        })]);

        // The leaf outcome uses 800 tokens which exceeds the per-leaf cap of 500.
        let mut executor = MockWorkflowExecutor::new().with_leaf_outcome(
            "expensive-scan",
            MockLeafOutcome::succeeded("scan done").with_usage(WorkflowUsage {
                input_tokens: Some(500),
                output_tokens: Some(300),
                cost_microusd: Some(0),
            }),
        );
        let execution = executor.run(&workflow).expect("mock workflow should run");

        assert_eq!(execution.status, WorkflowRunStatus::BudgetExceeded);
        assert_eq!(execution.leaf_results.len(), 1);
        assert_eq!(
            execution.leaf_results[0].status,
            WorkflowRunStatus::BudgetExceeded
        );
        assert!(
            execution.leaf_results[0]
                .output
                .as_deref()
                .unwrap_or_default()
                .contains("token budget exhausted")
        );
    }

    #[test]
    fn budget_spec_serializes_max_tokens() {
        let budget = BudgetSpec {
            max_steps: Some(10),
            timeout_secs: Some(600),
            max_parallel: Some(4),
            max_tokens: Some(50_000),
        };
        let json = serde_json::to_string(&budget).expect("serialize budget");
        let parsed: BudgetSpec = serde_json::from_str(&json).expect("parse budget");
        assert_eq!(parsed, budget);
        assert!(json.contains("\"max_tokens\":50000"));

        // Default (all None) round-trips without the field present.
        let default_json =
            serde_json::to_string(&BudgetSpec::default()).expect("serialize default");
        let parsed_default: BudgetSpec =
            serde_json::from_str(&default_json).expect("parse default budget");
        assert_eq!(parsed_default, BudgetSpec::default());
        assert!(parsed_default.max_tokens.is_none());
    }

    #[test]
    fn loop_until_stops_on_pass() {
        let workflow = workflow_spec(vec![WorkflowNode::LoopUntil(LoopUntilSpec {
            id: "verify".to_string(),
            condition: "verification passed".to_string(),
            max_iterations: Some(5),
            children: vec![leaf_node("run-check")],
        })]);

        let mut executor =
            MockWorkflowExecutor::new().with_predicate_results("verify", vec![false, false, true]);
        let execution = executor.run(&workflow).expect("loop should run");

        assert_eq!(execution.status, WorkflowRunStatus::Succeeded);
        assert_eq!(execution.leaf_results.len(), 3);
        assert_eq!(
            control_result(&execution, "verify").summary.as_deref(),
            Some("loop_until iterations=3")
        );
    }

    #[test]
    fn loop_until_honors_max_iters() {
        let workflow = workflow_spec(vec![WorkflowNode::LoopUntil(LoopUntilSpec {
            id: "verify".to_string(),
            condition: "verification passed".to_string(),
            max_iterations: Some(2),
            children: vec![leaf_node("run-check")],
        })]);

        let mut executor =
            MockWorkflowExecutor::new().with_predicate_results("verify", vec![false, false, true]);
        let execution = executor.run(&workflow).expect("loop should run");

        assert_eq!(execution.status, WorkflowRunStatus::Failed);
        assert_eq!(execution.leaf_results.len(), 2);
        assert_eq!(
            control_result(&execution, "verify").summary.as_deref(),
            Some("loop_until iterations=2")
        );
    }

    #[test]
    fn cond_uses_logged_predicate_result() {
        let workflow = workflow_spec(vec![WorkflowNode::Cond(CondSpec {
            id: "should-fix".to_string(),
            condition: "finding requires a patch".to_string(),
            then_nodes: vec![leaf_node("patch")],
            else_nodes: vec![leaf_node("report-only")],
        })]);

        let mut executor =
            MockWorkflowExecutor::new().with_predicate_results("should-fix", vec![true]);
        let execution = executor.run(&workflow).expect("cond should run");

        assert_eq!(
            execution
                .leaf_results
                .iter()
                .map(|result| result.leaf_id.as_str())
                .collect::<Vec<_>>(),
            vec!["patch"]
        );
        assert_eq!(
            control_result(&execution, "should-fix").summary.as_deref(),
            Some("predicate_result=true")
        );
    }

    #[test]
    fn expand_respects_max_children() {
        let workflow = workflow_spec(vec![WorkflowNode::Expand(ExpandSpec {
            id: "split".to_string(),
            source: "plan".to_string(),
            max_children: Some(2),
            template: None,
        })]);

        let generated = vec![leaf_node("first"), leaf_node("second"), leaf_node("third")];
        let mut executor = MockWorkflowExecutor::new().with_generated_nodes("split", generated);
        let execution = executor.run(&workflow).expect("expand should run");

        assert_eq!(
            execution
                .leaf_results
                .iter()
                .map(|result| result.leaf_id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert_eq!(
            control_result(&execution, "split").selected_children,
            vec!["first", "second"]
        );
    }

    #[test]
    fn expand_generated_nodes_validate_before_run() {
        let workflow = workflow_spec(vec![WorkflowNode::Expand(ExpandSpec {
            id: "split".to_string(),
            source: "plan".to_string(),
            max_children: None,
            template: None,
        })]);

        let mut executor = MockWorkflowExecutor::new()
            .with_generated_nodes("split", vec![invalid_leaf_node("bad")]);
        let err = executor
            .run(&workflow)
            .expect_err("invalid generated leaf should fail before execution");

        assert_eq!(
            err,
            WorkflowExecutionError::EmptyLeafPrompt {
                leaf: "bad".to_string()
            }
        );
    }

    #[test]
    fn workflow_spec_rejects_unknown_leaf_dependency() {
        let mut summarize = leaf_node("summarize");
        let WorkflowNode::Leaf(spec) = &mut summarize else {
            panic!("expected leaf");
        };
        spec.depends_on_results = vec!["missing-scan".to_string()];
        let workflow = workflow_spec(vec![summarize]);

        let mut executor = MockWorkflowExecutor::new();
        let err = executor
            .run(&workflow)
            .expect_err("unknown leaf dependency should fail before execution");

        assert_eq!(
            err,
            WorkflowExecutionError::UnknownNodeReference {
                node: "summarize".to_string(),
                field: "depends_on_results",
                reference: "missing-scan".to_string(),
            }
        );
    }

    #[test]
    fn workflow_spec_rejects_unknown_reduce_input() {
        let workflow = workflow_spec(vec![
            leaf_node("scan"),
            WorkflowNode::Reduce(ReduceSpec {
                id: "summarize".to_string(),
                inputs: vec!["scan".to_string(), "missing-review".to_string()],
                prompt: "Summarize safe fixes".to_string(),
                model_policy: ModelPolicy::default(),
            }),
        ]);

        let mut executor = MockWorkflowExecutor::new();
        let err = executor
            .run(&workflow)
            .expect_err("unknown reduce input should fail before execution");

        assert_eq!(
            err,
            WorkflowExecutionError::UnknownNodeReference {
                node: "summarize".to_string(),
                field: "inputs",
                reference: "missing-review".to_string(),
            }
        );
    }

    #[test]
    fn workflow_spec_rejects_unknown_teacher_candidate() {
        let workflow = workflow_spec(vec![
            leaf_node("candidate-a"),
            WorkflowNode::TeacherReview(TeacherReviewSpec {
                id: "teacher-review".to_string(),
                candidates: vec!["candidate-a".to_string(), "candidate-b".to_string()],
                promotion_policy: PromotionPolicy::default(),
            }),
        ]);

        let mut executor = MockWorkflowExecutor::new();
        let err = executor
            .run(&workflow)
            .expect_err("unknown teacher candidate should fail before execution");

        assert_eq!(
            err,
            WorkflowExecutionError::UnknownNodeReference {
                node: "teacher-review".to_string(),
                field: "candidates",
                reference: "candidate-b".to_string(),
            }
        );
    }

    #[test]
    fn teacher_candidate_serialization() {
        let candidate = TeacherCandidate {
            candidate_id: "teacher-review:branch-a".to_string(),
            kind: TeacherCandidateKind::WorkflowRecipe,
            status: TeacherCandidateStatus::Proposed,
            source_node_id: "branch-a".to_string(),
            source_branch_id: Some("branch-a".to_string()),
            summary: "Winning branch found a reusable workflow recipe.".to_string(),
            evidence: vec![
                "status=Succeeded".to_string(),
                "tokens=42, cost_microusd=7".to_string(),
            ],
            replay_results: vec![StudentReplayResult {
                trace_id: "trace-a".to_string(),
                candidate_id: "teacher-review:branch-a".to_string(),
                baseline: StudentReplayMetrics {
                    score: 70,
                    cost_microusd: 10,
                },
                candidate: StudentReplayMetrics {
                    score: 74,
                    cost_microusd: 12,
                },
                required_tests: vec![StudentReplayTestResult {
                    name: "cargo test -p codewhale-workflow".to_string(),
                    passed: true,
                }],
                policy_violations: Vec::new(),
                stale: false,
                notes: Some("offline replay improved the constrained student".to_string()),
            }],
        };

        let json = serde_json::to_string(&candidate).expect("serialize teacher candidate");

        assert!(json.contains("\"kind\":\"workflow_recipe\""));
        assert!(json.contains("\"status\":\"proposed\""));
        assert!(json.contains("\"replay_results\""));
        let parsed: TeacherCandidate =
            serde_json::from_str(&json).expect("parse teacher candidate");
        assert_eq!(parsed, candidate);
    }

    #[test]
    fn teacher_review_produces_candidate_from_trace() {
        let review = TeacherReviewSpec {
            id: "teacher-review".to_string(),
            candidates: vec!["winning-branch".to_string()],
            promotion_policy: PromotionPolicy::default(),
        };
        let execution = WorkflowExecution {
            branch_results: vec![BranchResult {
                branch_id: "winning-branch".to_string(),
                task_id: "winning-branch".to_string(),
                status: WorkflowRunStatus::Succeeded,
                usage: WorkflowUsage {
                    input_tokens: Some(30),
                    output_tokens: Some(12),
                    cost_microusd: Some(7),
                },
                memo_usage: WorkflowMemoUsage::default(),
                artifacts: vec!["trace://branches/winning-branch".to_string()],
                notes: Some("branch produced a minimal verified patch".to_string()),
            }],
            ..WorkflowExecution::default()
        };

        let report = TeacherReviewReport::from_execution(&review, &execution);

        assert_eq!(report.review_node_id, "teacher-review");
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(
            report.candidates[0].kind,
            TeacherCandidateKind::WorkflowRecipe
        );
        assert_eq!(
            report.candidates[0].status,
            TeacherCandidateStatus::Proposed
        );
        assert!(
            report.candidates[0]
                .evidence
                .iter()
                .any(|line| line.contains("tokens=42"))
        );
    }

    #[test]
    fn failed_leaf_becomes_regression_test_candidate() {
        let review = TeacherReviewSpec {
            id: "teacher-review".to_string(),
            candidates: vec!["verify-failure".to_string()],
            promotion_policy: PromotionPolicy::default(),
        };
        let execution = WorkflowExecution {
            leaf_results: vec![LeafResult {
                leaf_id: "verify-failure".to_string(),
                task_id: "verify-failure".to_string(),
                role: None,
                profile: None,
                status: WorkflowRunStatus::Failed,
                usage: WorkflowUsage::default(),
                memo_usage: WorkflowMemoUsage::default(),
                output: Some("cargo test failed with a replay mismatch".to_string()),
                artifacts: Vec::new(),
                schema_error: None,
            }],
            ..WorkflowExecution::default()
        };

        let candidates = teacher_candidates_from_execution(&review, &execution);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].kind, TeacherCandidateKind::RegressionTest);
        assert_eq!(candidates[0].status, TeacherCandidateStatus::Proposed);
        assert!(
            candidates[0]
                .evidence
                .iter()
                .any(|line| { line.contains("cargo test failed with a replay mismatch") })
        );
    }

    #[test]
    fn student_replay_promotes_only_on_delta() {
        let gate = PromotionGate {
            min_score_delta: 3,
            max_cost_delta_microusd: Some(25),
            ..PromotionGate::default()
        };
        let replay = StudentReplayResult {
            trace_id: "trace-a".to_string(),
            candidate_id: "teacher-review:branch-a".to_string(),
            baseline: StudentReplayMetrics {
                score: 80,
                cost_microusd: 100,
            },
            candidate: StudentReplayMetrics {
                score: 84,
                cost_microusd: 120,
            },
            required_tests: vec![StudentReplayTestResult {
                name: "workflow replay".to_string(),
                passed: true,
            }],
            policy_violations: Vec::new(),
            stale: false,
            notes: None,
        };

        let promoted = gate.evaluate_replay("teacher-review:branch-a", &replay);
        assert!(promoted.promoted());
        assert_eq!(promoted.status, TeacherCandidateStatus::Promoted);
        assert_eq!(promoted.score_delta, 4);

        let weak_replay = StudentReplayResult {
            candidate: StudentReplayMetrics {
                score: 82,
                cost_microusd: 120,
            },
            ..replay
        };
        let rejected = gate.evaluate_replay("teacher-review:branch-a", &weak_replay);
        assert!(!rejected.promoted());
        assert_eq!(rejected.status, TeacherCandidateStatus::Rejected);
        assert!(
            rejected
                .reasons
                .iter()
                .any(|reason| reason.contains("below required 3"))
        );
    }

    #[test]
    fn promotion_gate_rejects_stale_policy_cost_and_failed_tests() {
        let gate = PromotionGate {
            min_score_delta: 1,
            max_cost_delta_microusd: Some(10),
            ..PromotionGate::default()
        };
        let replay = StudentReplayResult {
            trace_id: "trace-a".to_string(),
            candidate_id: "teacher-review:branch-a".to_string(),
            baseline: StudentReplayMetrics {
                score: 70,
                cost_microusd: 10,
            },
            candidate: StudentReplayMetrics {
                score: 90,
                cost_microusd: 30,
            },
            required_tests: vec![StudentReplayTestResult {
                name: "required regression".to_string(),
                passed: false,
            }],
            policy_violations: vec!["writes outside file scope".to_string()],
            stale: true,
            notes: None,
        };

        let decision = gate.evaluate_replay("teacher-review:branch-a", &replay);

        assert_eq!(decision.status, TeacherCandidateStatus::Rejected);
        assert!(
            decision
                .reasons
                .iter()
                .any(|reason| { reason.contains("cost delta 20 exceeds allowed 10") })
        );
        assert!(
            decision
                .reasons
                .iter()
                .any(|reason| { reason.contains("required test `required regression` failed") })
        );
        assert!(
            decision
                .reasons
                .iter()
                .any(|reason| { reason.contains("policy violation: writes outside file scope") })
        );
        assert!(
            decision
                .reasons
                .iter()
                .any(|reason| { reason.contains("student replay result is stale") })
        );
    }

    #[test]
    fn promotion_gate_requires_recorded_replay_before_candidate_promotion() {
        let candidate = TeacherCandidate {
            candidate_id: "teacher-review:branch-a".to_string(),
            kind: TeacherCandidateKind::WorkflowRecipe,
            status: TeacherCandidateStatus::Proposed,
            source_node_id: "branch-a".to_string(),
            source_branch_id: Some("branch-a".to_string()),
            summary: "candidate waits for replay".to_string(),
            evidence: Vec::new(),
            replay_results: Vec::new(),
        };

        let decision = PromotionGate::default().evaluate_candidate(&candidate);

        assert_eq!(decision.status, TeacherCandidateStatus::Rejected);
        assert_eq!(
            decision.reasons,
            vec!["no student replay result recorded".to_string()]
        );
    }

    #[test]
    fn tournament_selects_passing_minimal_branch() {
        let tournament = BranchTournament {
            min_score: 60,
            ordering: TournamentOrdering::CostThenScore,
        };
        let candidates = vec![
            candidate(
                "expensive-pass",
                WorkflowRunStatus::Succeeded,
                90,
                90,
                "quality",
            ),
            candidate("failed-cheap", WorkflowRunStatus::Failed, 100, 1, "broken"),
            candidate(
                "cheap-pass",
                WorkflowRunStatus::Succeeded,
                70,
                10,
                "minimal",
            ),
            candidate("too-low", WorkflowRunStatus::Succeeded, 40, 2, "weak"),
        ];

        let selected = tournament
            .select(&candidates)
            .expect("one passing branch should be selected");

        assert_eq!(selected.branch_id, "cheap-pass");
    }

    #[test]
    fn tournament_can_select_score_before_cost_explicitly() {
        let tournament = BranchTournament {
            min_score: 60,
            ordering: TournamentOrdering::ScoreThenCost,
        };
        let candidates = vec![
            candidate("quality", WorkflowRunStatus::Succeeded, 95, 100, "quality"),
            candidate("minimal", WorkflowRunStatus::Succeeded, 70, 10, "minimal"),
        ];

        let selected = tournament
            .select(&candidates)
            .expect("one passing branch should be selected");

        assert_eq!(selected.branch_id, "quality");
    }

    #[test]
    fn pareto_frontier_keeps_diverse_candidates() {
        let frontier = ParetoFrontier { max_items: 4 };
        let candidates = vec![
            candidate("quality", WorkflowRunStatus::Succeeded, 95, 100, "quality"),
            candidate("minimal", WorkflowRunStatus::Succeeded, 70, 10, "small"),
            candidate("dominated", WorkflowRunStatus::Succeeded, 60, 40, "middle"),
            candidate("failed", WorkflowRunStatus::Failed, 100, 1, "broken"),
        ];

        let selected = frontier.select(&candidates);

        assert_eq!(
            selected
                .iter()
                .map(|candidate| candidate.branch_id.as_str())
                .collect::<Vec<_>>(),
            vec!["quality", "minimal"]
        );
        assert_eq!(
            selected
                .iter()
                .filter_map(|candidate| candidate.diversity_key.as_deref())
                .collect::<Vec<_>>(),
            vec!["quality", "small"]
        );
    }
}
