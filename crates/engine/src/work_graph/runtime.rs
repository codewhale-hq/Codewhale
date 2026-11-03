//! Active-session Work Graph authority and legacy tool adapters.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::fleet::ledger::FleetLedger;
use crate::tools::plan::{PlanSnapshot, PlanState, SharedPlanState, StepStatus};
use crate::tools::todo::{SharedTodoList, TodoList, TodoListSnapshot, TodoStatus};
use codewhale_lane::LaneRegistry;

use super::{
    BindingId, ChangeCtx, CompatPlanMetadata, CompatProjectionState, CompatTodoBinding, EdgeKind,
    IdempotencyKey, NodeKind, NodeState, OperationBinding, OperationIntent, OperationObservation,
    OperationOwnerSnapshot, Provenance, WorkActivityEvent, WorkEdge, WorkEdgeId, WorkGraph,
    WorkGraphChange, WorkGraphSnapshot, WorkNode, WorkNodeId, WorkNodePatch,
    external_identity_is_well_formed, fleet_task_owner_snapshot, import_legacy,
    lane_owner_snapshot, project_plan, project_todos, validate,
};
use codewhale_models::reasoning_effort_tier::ReasoningEffortTier;

pub const ACTIVE_OPERATION_SUMMARY_START: &str = "<!-- codewhale:active-work-operations:start -->";
pub const ACTIVE_OPERATION_SUMMARY_END: &str = "<!-- codewhale:active-work-operations:end -->";

#[derive(Debug, Clone, PartialEq)]
pub struct WorkRuntimeSnapshot {
    pub graph: WorkGraphSnapshot,
    pub todos: TodoListSnapshot,
    pub plan: PlanSnapshot,
}

#[derive(Debug, Default)]
struct ActiveGraph {
    session_id: Option<String>,
    snapshot: Option<WorkGraphSnapshot>,
    pending_publish: bool,
}

/// One active session graph plus the read-only legacy views it publishes.
pub struct WorkRuntime {
    todos: SharedTodoList,
    plan: SharedPlanState,
    graph: Mutex<ActiveGraph>,
}

pub type SharedWorkRuntime = Arc<WorkRuntime>;

impl std::fmt::Debug for WorkRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let graph = lock_unpoisoned(&self.graph);
        f.debug_struct("WorkRuntime")
            .field("session_id", &graph.session_id)
            .field("has_graph", &graph.snapshot.is_some())
            .field("pending_publish", &graph.pending_publish)
            .finish()
    }
}

#[must_use]
pub fn new_shared_work_runtime(todos: SharedTodoList, plan: SharedPlanState) -> SharedWorkRuntime {
    Arc::new(WorkRuntime {
        todos,
        plan,
        graph: Mutex::new(ActiveGraph::default()),
    })
}

impl WorkRuntime {
    #[must_use]
    pub fn matches_todos(&self, todos: &SharedTodoList) -> bool {
        Arc::ptr_eq(&self.todos, todos)
    }

    #[must_use]
    pub fn matches_plan(&self, plan: &SharedPlanState) -> bool {
        Arc::ptr_eq(&self.plan, plan)
    }

    #[must_use]
    pub fn has_operation_binding(&self, session_id: Option<&str>, external: &str) -> bool {
        let active = lock_unpoisoned(&self.graph);
        if let (Some(expected), Some(actual)) = (session_id, active.session_id.as_deref())
            && expected != actual
        {
            return false;
        }
        active.snapshot.as_ref().is_some_and(|graph| {
            graph.nodes.iter().any(|node| {
                node.binding
                    .as_ref()
                    .is_some_and(|binding| binding.external == external)
            })
        })
    }

    /// Durable owner bindings that still require restore-time confirmation.
    /// Terminal operations are excluded because their saved owner receipt is
    /// already sufficient and must not be reopened by a missing live handle.
    #[must_use]
    pub fn reconcilable_durable_bindings(&self, session_id: Option<&str>) -> Vec<String> {
        let active = lock_unpoisoned(&self.graph);
        if let (Some(expected), Some(actual)) = (session_id, active.session_id.as_deref())
            && expected != actual
        {
            return Vec::new();
        }
        active
            .snapshot
            .as_ref()
            .into_iter()
            .flat_map(|graph| graph.nodes.iter())
            .filter(|node| node.state.is_live() || node.state == NodeState::Stale)
            .filter_map(|node| node.binding.as_ref())
            .filter(|binding| binding.durable)
            .map(|binding| binding.external.clone())
            .collect()
    }

    /// Register an Operation before its owner starts work. The operation is
    /// first added inert, connected to an Objective/PlanStep, and only then
    /// advanced to `Initializing`, so every reducer intermediate satisfies
    /// the no-orphan invariant.
    pub fn register_operation(
        &self,
        session_id: &str,
        intent: OperationIntent,
    ) -> Result<WorkNodeId, String> {
        if !external_identity_is_well_formed(&intent.external) {
            return Err(format!(
                "invalid lifecycle binding external {:?}",
                intent.external
            ));
        }
        let todos = retry_lock(&self.todos, 100)
            .ok_or_else(|| "To-do state is busy; operation was not registered".to_string())?;
        let plan = retry_lock(&self.plan, 100)
            .ok_or_else(|| "Plan state is busy; operation was not registered".to_string())?;
        let mut active = lock_unpoisoned(&self.graph);
        let base = graph_for_update(&mut active, session_id, &plan.snapshot(), &todos.snapshot())?;
        if let Some(existing) = base.nodes.iter().find(|node| {
            node.binding
                .as_ref()
                .is_some_and(|binding| binding.external == intent.external)
        }) {
            let binding = existing.binding.as_ref().expect("binding matched above");
            if binding.durable != intent.durable {
                return Err(format!(
                    "lifecycle binding {} changed durability",
                    intent.external
                ));
            }
            return Ok(existing.id.clone());
        }

        let mut graph = WorkGraph::from_snapshot(base);
        let parent = operation_parent(&mut graph, session_id, &intent.source)?;
        let node_id = WorkNodeId::derive(session_id, &format!("operation:{}", intent.external));
        let now = now_ms();
        apply_change(
            &mut graph,
            session_id,
            &intent.source,
            WorkGraphChange::AddNode {
                node: WorkNode {
                    id: node_id.clone(),
                    kind: NodeKind::Operation,
                    title: bounded_operation_title(&intent.title),
                    state: NodeState::Ready,
                    acceptance: intent.acceptance,
                    binding: Some(OperationBinding {
                        external: intent.external,
                        durable: intent.durable,
                        last_observation: None,
                    }),
                    evidence: None,
                    provenance: Provenance::ToolUpdate {
                        tool: intent.source.clone(),
                        call_id: intent.call_id,
                    },
                    created_at: now,
                    updated_at: now,
                },
            },
        )?;
        ensure_contains(&mut graph, session_id, &intent.source, &parent, &node_id)?;
        let title = graph
            .snapshot()
            .node(&node_id)
            .map(|node| node.title.clone())
            .ok_or_else(|| format!("operation {node_id} disappeared during registration"))?;
        patch_existing_node(
            &mut graph,
            session_id,
            &intent.source,
            &node_id,
            title,
            NodeState::Initializing,
        )?;
        // Registration is where finished shell calls accumulate (#6842):
        // keep only the newest ended, non-durable operations.
        if super::reducer::evictable_operations(graph.snapshot()).len()
            > super::model::ENDED_OPERATION_CAP
        {
            apply_change(
                &mut graph,
                session_id,
                &intent.source,
                WorkGraphChange::PruneEndedOperations {
                    keep: super::model::ENDED_OPERATION_CAP,
                },
            )?;
        }
        let next = graph.into_snapshot();
        validate_combined(&next, &project_plan(&next), &project_todos(&next))?;
        active.snapshot = Some(next);
        active.pending_publish = true;
        Ok(node_id)
    }

    /// Retain a crossed approval boundary as provenance attached to an
    /// operation. The completed Approval node is historical evidence only: it
    /// does not grant capabilities or change the owner's runtime authority.
    pub fn record_operation_approval(
        &self,
        session_id: &str,
        external: &str,
        reference: &str,
        source: &str,
        call_id: &str,
    ) -> Result<(), String> {
        let todos = retry_lock(&self.todos, 100)
            .ok_or_else(|| "To-do state is busy; approval was not recorded".to_string())?;
        let plan = retry_lock(&self.plan, 100)
            .ok_or_else(|| "Plan state is busy; approval was not recorded".to_string())?;
        let mut active = lock_unpoisoned(&self.graph);
        let base = graph_for_update(&mut active, session_id, &plan.snapshot(), &todos.snapshot())?;
        let operation = base
            .nodes
            .iter()
            .find(|node| {
                node.kind == NodeKind::Operation
                    && node
                        .binding
                        .as_ref()
                        .is_some_and(|binding| binding.external == external)
            })
            .map(|node| node.id.clone())
            .ok_or_else(|| format!("operation binding {external} is not registered"))?;
        let approval = WorkNodeId::derive(
            session_id,
            &format!("approval:operation:{external}:{reference}"),
        );
        let mut graph = WorkGraph::from_snapshot(base);
        if graph.snapshot().node(&approval).is_none() {
            let now = now_ms();
            apply_change(
                &mut graph,
                session_id,
                source,
                WorkGraphChange::AddNode {
                    node: WorkNode {
                        id: approval.clone(),
                        kind: NodeKind::Approval,
                        title: bounded_operation_title(&format!(
                            "verification approved: {reference}"
                        )),
                        state: NodeState::Completed,
                        acceptance: Vec::new(),
                        binding: None,
                        evidence: None,
                        provenance: Provenance::ToolUpdate {
                            tool: source.to_string(),
                            call_id: call_id.to_string(),
                        },
                        created_at: now,
                        updated_at: now,
                    },
                },
            )?;
        }
        let edge = WorkEdgeId::derive(
            session_id,
            &format!(
                "requires-approval:{}:{}",
                operation.as_str(),
                approval.as_str()
            ),
        );
        if graph.snapshot().edge(&edge).is_none() {
            apply_change(
                &mut graph,
                session_id,
                source,
                WorkGraphChange::AddEdge {
                    edge: WorkEdge {
                        id: edge,
                        kind: EdgeKind::RequiresApproval,
                        from: operation,
                        to: approval,
                    },
                },
            )?;
        }
        let next = graph.into_snapshot();
        validate_combined(&next, &project_plan(&next), &project_todos(&next))?;
        active.snapshot = Some(next);
        active.pending_publish = true;
        Ok(())
    }

    /// Apply one owner observation through the reducer. Unknown bindings are
    /// rejected rather than materialized after the fact: spawn intent must be
    /// registered before work begins.
    pub fn reconcile_operation(
        &self,
        session_id: &str,
        snapshot: OperationOwnerSnapshot,
    ) -> Result<bool, String> {
        let external = snapshot.external.clone();
        self.reconcile_observation(session_id, &external, snapshot.into_observation())
    }

    pub fn reconcile_observation(
        &self,
        session_id: &str,
        external: &str,
        observation: OperationObservation,
    ) -> Result<bool, String> {
        let mut active = lock_unpoisoned(&self.graph);
        if active
            .session_id
            .as_deref()
            .is_some_and(|id| id != session_id)
        {
            return Err(format!(
                "lifecycle observation for session {session_id} does not match active session"
            ));
        }
        let base = active
            .snapshot
            .clone()
            .ok_or_else(|| "lifecycle observation arrived before graph registration".to_string())?;
        let Some(next) = apply_observation_to_snapshot(&base, session_id, external, observation)?
        else {
            return Ok(false);
        };
        active.snapshot = Some(next);
        active.pending_publish = true;
        Ok(true)
    }

    /// Compact graph-owned continuity injected after context compaction.
    /// It contains identities and state only; no raw output or reasoning.
    #[must_use]
    pub fn active_operation_summary(&self, session_id: Option<&str>) -> Option<String> {
        let active = lock_unpoisoned(&self.graph);
        if let (Some(expected), Some(actual)) = (session_id, active.session_id.as_deref())
            && expected != actual
        {
            return None;
        }
        let graph = active.snapshot.as_ref()?;
        let operations = graph
            .nodes
            .iter()
            .filter(|node| {
                node.kind == NodeKind::Operation
                    && (node.state.is_live() || node.state == NodeState::Stale)
            })
            .take(24)
            .collect::<Vec<_>>();
        if operations.is_empty() {
            return None;
        }
        let mut out = format!(
            "{ACTIVE_OPERATION_SUMMARY_START}\n## Active Work Graph Operations\n\nOwner records remain authoritative; reconcile before acting on a restored operation.\n"
        );
        for node in operations {
            let external = node
                .binding
                .as_ref()
                .map_or("unbound", |binding| binding.external.as_str());
            out.push_str(&format!(
                "- `{}` - {} - {}\n",
                external,
                operation_state_label(node.state),
                prompt_safe_title(&node.title)
            ));
        }
        out.push_str(ACTIVE_OPERATION_SUMMARY_END);
        Some(out)
    }

    /// Record one reasoning-effort configuration change as bounded graph
    /// activity. The event contains typed tiers and a non-secret route
    /// identity only; there is no field capable of carrying reasoning text.
    /// When live operations exist, the most recently updated one receives the
    /// historical link. Later terminalization does not invalidate that link.
    #[allow(clippy::too_many_arguments)]
    pub fn record_reasoning_effort_change(
        &self,
        session_id: Option<&str>,
        requested: ReasoningEffortTier,
        effective: ReasoningEffortTier,
        provider_kind: codewhale_config::ProviderKind,
        provider: &str,
        endpoint_identity: Option<&str>,
        model: Option<&str>,
    ) -> Result<Option<WorkNodeId>, String> {
        let todos = retry_lock(&self.todos, 100)
            .ok_or_else(|| "To-do state is busy; effort change was not recorded".to_string())?;
        let plan = retry_lock(&self.plan, 100)
            .ok_or_else(|| "Plan state is busy; effort change was not recorded".to_string())?;
        let mut active = lock_unpoisoned(&self.graph);
        let session_id = resolved_session_id(&active, session_id);
        let base = graph_for_update(
            &mut active,
            &session_id,
            &plan.snapshot(),
            &todos.snapshot(),
        )?;
        let operation = base
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Operation && node.state.is_live())
            .max_by(|left, right| {
                left.updated_at
                    .cmp(&right.updated_at)
                    .then_with(|| left.id.as_str().cmp(right.id.as_str()))
            })
            .map(|node| node.id.clone());
        let now = now_ms();
        let mut graph = WorkGraph::from_snapshot(base);
        graph
            .apply(
                WorkGraphChange::RecordActivity {
                    event: WorkActivityEvent::ReasoningEffortChanged {
                        requested,
                        effective,
                        provider_kind: Some(provider_kind),
                        provider: provider.to_string(),
                        endpoint_identity: endpoint_identity.map(str::to_string),
                        model: model.map(str::to_string),
                        ts: now,
                        operation: operation.clone(),
                    },
                },
                ChangeCtx {
                    session_id,
                    now,
                    idempotency_key: None,
                },
            )
            .map_err(|err| format!("reasoning effort: {err}"))?;
        let next = graph.into_snapshot();
        validate_combined(&next, &project_plan(&next), &project_todos(&next))?;
        active.snapshot = Some(next);
        active.pending_publish = true;
        Ok(operation)
    }

    /// Apply an `update_plan` payload through the graph and publish both
    /// legacy projections only after the candidate graph validates.
    pub async fn apply_plan_update(
        &self,
        session_id: &str,
        tool: &str,
        plan: &PlanSnapshot,
    ) -> Result<PlanSnapshot, String> {
        let todos_guard = self.todos.lock().await;
        let plan_guard = self.plan.lock().await;
        let mut active = lock_unpoisoned(&self.graph);
        let base = graph_for_update(
            &mut active,
            session_id,
            &plan_guard.snapshot(),
            &todos_guard.snapshot(),
        )?;
        let next = update_plan_graph(base, session_id, tool, plan)?;
        let derived_plan = project_plan(&next);
        let derived_todos = project_todos(&next);
        let next_plan = PlanState::from_snapshot(&derived_plan);
        let next_todos = TodoList::from_snapshot(&derived_todos)?;
        validate_combined(&next, &next_plan.snapshot(), &next_todos.snapshot())?;
        active.snapshot = Some(next);
        active.pending_publish = true;
        Ok(derived_plan)
    }

    /// Apply a legacy To-do/checklist payload through the graph and publish
    /// both projections from the committed candidate.
    pub async fn apply_todo_update(
        &self,
        session_id: &str,
        tool: &str,
        todos: &TodoListSnapshot,
    ) -> Result<TodoListSnapshot, String> {
        let todos_guard = self.todos.lock().await;
        let plan_guard = self.plan.lock().await;
        let mut active = lock_unpoisoned(&self.graph);
        let base = graph_for_update(
            &mut active,
            session_id,
            &plan_guard.snapshot(),
            &todos_guard.snapshot(),
        )?;
        let next = update_todo_graph(base, session_id, tool, todos)?;
        let derived_plan = project_plan(&next);
        let derived_todos = project_todos(&next);
        let next_plan = PlanState::from_snapshot(&derived_plan);
        let next_todos = TodoList::from_snapshot(&derived_todos)?;
        validate_combined(&next, &next_plan.snapshot(), &next_todos.snapshot())?;
        active.snapshot = Some(next);
        active.pending_publish = true;
        Ok(derived_todos)
    }

    /// Publish the latest validated legacy views after the caller has queued
    /// their graph-backed session/checkpoint write.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn publish_pending(&self) -> Result<bool, String> {
        self.publish_pending_guarded(None).await
    }

    pub async fn publish_pending_matching(
        &self,
        session_id: Option<&str>,
        graph: Option<&WorkGraphSnapshot>,
    ) -> Result<bool, String> {
        self.publish_pending_guarded(Some((session_id, graph)))
            .await
    }

    async fn publish_pending_guarded(
        &self,
        expected: Option<(Option<&str>, Option<&WorkGraphSnapshot>)>,
    ) -> Result<bool, String> {
        let mut todos = self.todos.lock().await;
        let mut plan = self.plan.lock().await;
        let mut active = lock_unpoisoned(&self.graph);
        if !active.pending_publish {
            return Ok(false);
        }
        if expected.is_some_and(|(owner, graph)| {
            active.session_id.as_deref() != Some(owner.unwrap_or("unsaved-work"))
                || active.snapshot.as_ref() != graph
        }) {
            return Ok(false);
        }
        let graph = active
            .snapshot
            .as_ref()
            .ok_or_else(|| "pending Work projection has no graph".to_string())?;
        let derived_plan = project_plan(graph);
        let derived_todos = project_todos(graph);
        let next_plan = PlanState::from_snapshot(&derived_plan);
        let next_todos = TodoList::from_snapshot(&derived_todos)?;
        validate_combined(graph, &next_plan.snapshot(), &next_todos.snapshot())?;
        *plan = next_plan;
        *todos = next_todos;
        active.pending_publish = false;
        Ok(true)
    }

    /// Synchronous counterpart for explicit save/rename/fork commands that
    /// have already completed their atomic disk write.
    pub fn publish_pending_sync_matching(
        &self,
        session_id: Option<&str>,
        expected_graph: Option<&WorkGraphSnapshot>,
    ) -> Result<bool, String> {
        let mut todos = retry_lock(&self.todos, 100).ok_or_else(|| {
            "To-do state is busy; saved Work views were not published".to_string()
        })?;
        let mut plan = retry_lock(&self.plan, 100)
            .ok_or_else(|| "Plan state is busy; saved Work views were not published".to_string())?;
        let mut active = lock_unpoisoned(&self.graph);
        if !active.pending_publish {
            return Ok(false);
        }
        if active.session_id.as_deref() != Some(session_id.unwrap_or("unsaved-work"))
            || active.snapshot.as_ref() != expected_graph
        {
            return Ok(false);
        }
        let graph = active
            .snapshot
            .as_ref()
            .ok_or_else(|| "pending Work projection has no graph".to_string())?;
        let derived_plan = project_plan(graph);
        let derived_todos = project_todos(graph);
        let next_plan = PlanState::from_snapshot(&derived_plan);
        let next_todos = TodoList::from_snapshot(&derived_todos)?;
        validate_combined(graph, &next_plan.snapshot(), &next_todos.snapshot())?;
        *plan = next_plan;
        *todos = next_todos;
        active.pending_publish = false;
        Ok(true)
    }

    #[must_use]
    pub fn has_pending_publish(&self) -> bool {
        lock_unpoisoned(&self.graph).pending_publish
    }

    /// Latest graph-derived To-do view, including an unpublished transaction.
    pub async fn current_todos(&self) -> Result<TodoListSnapshot, String> {
        let projected = {
            let active = lock_unpoisoned(&self.graph);
            active.snapshot.as_ref().map(project_todos)
        };
        if let Some(projected) = projected {
            return Ok(projected);
        }
        Ok(self.todos.lock().await.snapshot())
    }

    /// Capture a persistence-ready graph plus fully populated old views.
    /// Legacy-only in-memory state is imported once and normalized in place.
    pub fn capture(&self, session_id: Option<&str>) -> Result<Option<WorkRuntimeSnapshot>, String> {
        self.capture_with_retries(session_id, 100)
    }

    /// Non-blocking capture for the render/event loop.
    pub fn try_capture(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<WorkRuntimeSnapshot>, String> {
        self.capture_with_retries(session_id, 1)
    }

    fn capture_with_retries(
        &self,
        session_id: Option<&str>,
        retries: u32,
    ) -> Result<Option<WorkRuntimeSnapshot>, String> {
        let todos = retry_lock(&self.todos, retries)
            .ok_or_else(|| "To-do state is busy; try saving again".to_string())?;
        let plan = retry_lock(&self.plan, retries)
            .ok_or_else(|| "Plan state is busy; try saving again".to_string())?;
        let mut active = lock_unpoisoned(&self.graph);
        let todos_snapshot = todos.snapshot();
        let plan_snapshot = plan.snapshot();
        if todos_snapshot.is_empty()
            && plan_snapshot.is_empty()
            && active
                .snapshot
                .as_ref()
                .is_none_or(WorkGraphSnapshot::is_empty)
        {
            return Ok(None);
        }
        let had_graph = active.snapshot.is_some();
        let had_pending_publish = active.pending_publish;
        let session_id = resolved_session_id(&active, session_id);
        let graph = graph_for_update(&mut active, &session_id, &plan_snapshot, &todos_snapshot)?;
        let derived_plan = project_plan(&graph);
        let derived_todos = project_todos(&graph);
        validate_combined(&graph, &derived_plan, &derived_todos)?;
        if had_graph
            && !had_pending_publish
            && (derived_plan != plan_snapshot || derived_todos != todos_snapshot)
        {
            return Err("live Work Graph and legacy views disagree".to_string());
        }
        active.snapshot = Some(graph.clone());
        if !had_graph {
            active.pending_publish = true;
        }
        Ok(Some(WorkRuntimeSnapshot {
            graph,
            todos: derived_todos,
            plan: derived_plan,
        }))
    }

    /// Validate and atomically activate persisted state. Sessions without a
    /// graph are deterministically imported from their complete old views.
    pub fn restore(
        &self,
        session_id: &str,
        graph: Option<&WorkGraphSnapshot>,
        todos: &TodoListSnapshot,
        plan: &PlanSnapshot,
    ) -> Result<Option<WorkRuntimeSnapshot>, String> {
        self.restore_internal(session_id, graph, todos, plan, None)
    }

    /// Restore a saved graph and reconcile workspace-scoped durable owners as
    /// one candidate transaction. No live state changes until the restored
    /// graph, owner observations, and both legacy projections all validate.
    pub fn restore_with_workspace_owner_bindings(
        &self,
        session_id: &str,
        workspace: &Path,
        graph: Option<&WorkGraphSnapshot>,
        todos: &TodoListSnapshot,
        plan: &PlanSnapshot,
    ) -> Result<Option<WorkRuntimeSnapshot>, String> {
        self.restore_internal(session_id, graph, todos, plan, Some(workspace))
    }

    fn restore_internal(
        &self,
        session_id: &str,
        graph: Option<&WorkGraphSnapshot>,
        todos: &TodoListSnapshot,
        plan: &PlanSnapshot,
        workspace: Option<&Path>,
    ) -> Result<Option<WorkRuntimeSnapshot>, String> {
        let had_graph = graph.is_some();
        let graph = match graph {
            Some(graph) => {
                validate(graph).map_err(|err| err.to_string())?;
                graph.clone()
            }
            None if todos.is_empty() && plan.is_empty() => WorkGraphSnapshot::new(),
            None => import_legacy(session_id, plan, todos)?,
        };
        let (mut graph, reconciled_ephemeral) =
            mark_restored_ephemeral_operations_stale(graph, session_id)?;
        let reconciled_workspace = if let Some(workspace) = workspace {
            let (reconciled, changed) = reconcile_workspace_snapshot(graph, session_id, workspace)?;
            graph = reconciled;
            changed > 0
        } else {
            false
        };
        let derived_plan = project_plan(&graph);
        let derived_todos = project_todos(&graph);
        if graph.is_empty() {
            if !todos.is_empty() || !plan.is_empty() {
                return Err("empty Work Graph cannot carry non-empty legacy views".to_string());
            }
        } else if graph.import_digest.is_some() && graph.compat.is_empty() {
            return Err("imported Work Graph is missing compatibility projections".to_string());
        }
        validate_combined(&graph, &derived_plan, &derived_todos)?;
        if had_graph && (&derived_plan != plan || &derived_todos != todos) {
            return Err("persisted Work Graph and legacy views disagree".to_string());
        }
        let next_plan = PlanState::from_snapshot(&derived_plan);
        let next_todos = TodoList::from_snapshot(&derived_todos)?;
        let mut todos_guard = retry_lock(&self.todos, 100)
            .ok_or_else(|| "To-do state is busy; session was not restored".to_string())?;
        let mut plan_guard = retry_lock(&self.plan, 100)
            .ok_or_else(|| "Plan state is busy; session was not restored".to_string())?;
        let mut active = lock_unpoisoned(&self.graph);
        *todos_guard = next_todos;
        *plan_guard = next_plan;
        active.session_id = Some(session_id.to_string());
        active.snapshot = Some(graph.clone());
        // A legacy load has already restored its complete old views, but its
        // newly imported graph still needs one acknowledged graph-bearing
        // write (and pre-import archive) before the migration is settled.
        active.pending_publish =
            reconciled_ephemeral || reconciled_workspace || (!had_graph && !graph.is_empty());
        if graph.is_empty() {
            Ok(None)
        } else {
            Ok(Some(WorkRuntimeSnapshot {
                graph,
                todos: derived_todos,
                plan: derived_plan,
            }))
        }
    }

    pub fn clear(&self, session_id: Option<&str>) -> bool {
        let Some(mut todos) = retry_lock(&self.todos, 100) else {
            return false;
        };
        let Some(mut plan) = retry_lock(&self.plan, 100) else {
            return false;
        };
        let mut active = lock_unpoisoned(&self.graph);
        todos.clear();
        *plan = PlanState::default();
        active.session_id = Some(resolved_session_id(&active, session_id));
        active.snapshot = Some(WorkGraphSnapshot::new());
        active.pending_publish = false;
        true
    }
}

fn apply_observation_to_snapshot(
    base: &WorkGraphSnapshot,
    session_id: &str,
    external: &str,
    observation: OperationObservation,
) -> Result<Option<WorkGraphSnapshot>, String> {
    let node = base
        .nodes
        .iter()
        .find(|node| {
            node.binding
                .as_ref()
                .is_some_and(|binding| binding.external == external)
        })
        .ok_or_else(|| format!("operation binding {external} is not registered"))?;
    let stale_owner_recovery = if let OperationObservation::OwnerReported {
        state, seq, output, ..
    } = &observation
        && let Some(previous) = node
            .binding
            .as_ref()
            .and_then(|binding| binding.last_observation.as_ref())
    {
        if *seq < previous.seq {
            return Err(format!(
                "operation owner {external} sequence regressed from {} to {seq}",
                previous.seq
            ));
        }
        if *seq == previous.seq {
            if node.state != NodeState::Stale {
                return Ok(None);
            }
            if *state != previous.owner_state || *output != previous.output {
                return Err(format!(
                    "operation owner {external} changed observation at sequence {seq}"
                ));
            }
            true
        } else {
            false
        }
    } else {
        false
    };
    if matches!(&observation, OperationObservation::OwnerMissing { .. })
        && node.state == NodeState::Stale
    {
        return Ok(None);
    }
    let idempotency_key = match &observation {
        OperationObservation::OwnerReported { .. } if stale_owner_recovery => None,
        OperationObservation::OwnerReported { seq, .. } => Some(IdempotencyKey {
            binding: BindingId::derive(session_id, &format!("binding:{external}")),
            seq: *seq,
        }),
        OperationObservation::OwnerMissing { .. } | OperationObservation::CancelUpdate { .. } => {
            None
        }
    };
    let (next, receipt) = super::reducer::apply(
        base,
        WorkGraphChange::ReconcileOperation {
            node: node.id.clone(),
            obs: observation,
        },
        ChangeCtx {
            session_id: session_id.to_string(),
            now: now_ms(),
            idempotency_key,
        },
    )
    .map_err(|err| format!("runtime reconcile: {err}"))?;
    Ok((!receipt.no_op).then_some(next))
}

fn reconcile_workspace_snapshot(
    mut graph: WorkGraphSnapshot,
    session_id: &str,
    workspace: &Path,
) -> Result<(WorkGraphSnapshot, usize), String> {
    let candidates = graph
        .nodes
        .iter()
        .filter(|node| node.state.is_live() || node.state == NodeState::Stale)
        .filter_map(|node| node.binding.as_ref())
        .filter(|binding| binding.durable)
        .map(|binding| binding.external.clone())
        .filter(|external| external.starts_with("fleet:") || external.starts_with("lane:"))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok((graph, 0));
    }

    let fleet = candidates
        .iter()
        .any(|external| external.starts_with("fleet:"))
        .then(|| FleetLedger::open(workspace).and_then(|ledger| ledger.rebuild_state()));
    if let Some(Err(err)) = fleet.as_ref() {
        tracing::warn!(
            workspace = %workspace.display(),
            error = %err,
            "Fleet owner store could not be replayed; restored bindings will be stale"
        );
    }
    let lanes = candidates
        .iter()
        .any(|external| external.starts_with("lane:"))
        .then(LaneRegistry::open_default);
    if let Some(Err(err)) = lanes.as_ref() {
        tracing::warn!(
            error = %err,
            "Lane owner registry could not be opened; restored bindings will be stale"
        );
    }
    let observed_at = now_ms();
    let mut changed = 0usize;
    for external in candidates {
        let observation = if let Some(rest) = external.strip_prefix("fleet:") {
            rest.split_once('/')
                .and_then(|(run_id, task_id)| {
                    fleet
                        .as_ref()
                        .and_then(|state| state.as_ref().ok())
                        .and_then(|state| state.tasks.get(&format!("{run_id}:{task_id}")))
                })
                .map(|record| fleet_task_owner_snapshot(record, observed_at).into_observation())
                .unwrap_or(OperationObservation::OwnerMissing {
                    checked_at: observed_at,
                })
        } else if let Some(lane_id) = external.strip_prefix("lane:") {
            lanes
                .as_ref()
                .and_then(|registry| registry.as_ref().ok())
                .and_then(|registry| registry.load(lane_id).ok())
                .map(|record| lane_owner_snapshot(&record, observed_at).into_observation())
                .unwrap_or(OperationObservation::OwnerMissing {
                    checked_at: observed_at,
                })
        } else {
            continue;
        };
        if let Some(next) =
            apply_observation_to_snapshot(&graph, session_id, &external, observation)?
        {
            graph = next;
            changed = changed.saturating_add(1);
        }
    }
    Ok((graph, changed))
}

fn operation_parent(
    graph: &mut WorkGraph,
    session_id: &str,
    source: &str,
) -> Result<WorkNodeId, String> {
    let snapshot = graph.snapshot();
    // Only a step the current plan or To-do projection still lists may adopt
    // new work. Replacing a plan keeps the dropped steps as history (their
    // ids are reused if the plan grows back, so they are not retired), but a
    // step nobody can see must never become an operation's parent.
    let projected_step = |node: &&WorkNode, state: NodeState| {
        node.kind == NodeKind::PlanStep
            && node.state == state
            && (snapshot.compat.plan_order.contains(&node.id)
                || snapshot
                    .compat
                    .todos
                    .iter()
                    .any(|binding| binding.node == node.id))
    };
    if let Some(parent) = snapshot
        .nodes
        .iter()
        .find(|node| projected_step(node, NodeState::Active))
        .or_else(|| {
            snapshot
                .nodes
                .iter()
                .find(|node| projected_step(node, NodeState::Ready))
        })
        .or_else(|| {
            snapshot
                .nodes
                .iter()
                .find(|node| node.kind == NodeKind::Objective)
        })
    {
        return Ok(parent.id.clone());
    }

    let now = now_ms();
    let id = WorkNodeId::derive(session_id, "objective:runtime-operations");
    apply_change(
        graph,
        session_id,
        source,
        WorkGraphChange::AddNode {
            node: WorkNode {
                id: id.clone(),
                kind: NodeKind::Objective,
                title: "Runtime operations".to_string(),
                state: NodeState::Ready,
                acceptance: Vec::new(),
                binding: None,
                evidence: None,
                provenance: Provenance::RuntimeReconcile {
                    source: source.to_string(),
                    observed_at: now,
                },
                created_at: now,
                updated_at: now,
            },
        },
    )?;
    Ok(id)
}

fn mark_restored_ephemeral_operations_stale(
    graph: WorkGraphSnapshot,
    session_id: &str,
) -> Result<(WorkGraphSnapshot, bool), String> {
    let candidates = graph
        .nodes
        .iter()
        .filter_map(|node| {
            let binding = node.binding.as_ref()?;
            (!binding.durable && node.state.is_live()).then(|| node.id.clone())
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok((graph, false));
    }
    let mut graph = WorkGraph::from_snapshot(graph);
    for node in candidates {
        graph
            .apply(
                WorkGraphChange::ReconcileOperation {
                    node,
                    obs: OperationObservation::OwnerMissing {
                        checked_at: now_ms(),
                    },
                },
                ChangeCtx {
                    session_id: session_id.to_string(),
                    now: now_ms(),
                    idempotency_key: None,
                },
            )
            .map_err(|err| format!("restart reconcile: {err}"))?;
    }
    Ok((graph.into_snapshot(), true))
}

fn bounded_operation_title(title: &str) -> String {
    let normalized = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let bounded = chars.by_ref().take(180).collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}...")
    } else if bounded.is_empty() {
        "Runtime operation".to_string()
    } else {
        bounded
    }
}

fn prompt_safe_title(title: &str) -> String {
    bounded_operation_title(&title.replace('`', "'"))
}

const fn operation_state_label(state: NodeState) -> &'static str {
    match state {
        NodeState::Ready => "ready",
        NodeState::Initializing => "initializing",
        NodeState::Active => "running",
        NodeState::Waiting => "waiting",
        NodeState::Blocked => "blocked",
        NodeState::Completed => "completed",
        NodeState::Verified => "verified",
        NodeState::Stale => "stale",
        NodeState::Superseded => "superseded",
        NodeState::Cancelled => "cancelled",
        NodeState::Failed => "failed",
    }
}

fn graph_for_update(
    active: &mut ActiveGraph,
    session_id: &str,
    plan: &PlanSnapshot,
    todos: &TodoListSnapshot,
) -> Result<WorkGraphSnapshot, String> {
    match active.session_id.as_deref() {
        // App session transitions are already blocked while runtime work is
        // active. Rebind the authority namespace without re-keying graph IDs
        // so save-as/fork/new-session flows keep one coherent snapshot.
        Some(active_id) if active_id != session_id => {
            active.session_id = Some(session_id.to_string());
        }
        None => active.session_id = Some(session_id.to_string()),
        Some(_) => {}
    }
    if let Some(snapshot) = active.snapshot.as_ref() {
        validate(snapshot).map_err(|err| err.to_string())?;
        return Ok(snapshot.clone());
    }
    let graph = if plan.is_empty() && todos.is_empty() {
        WorkGraphSnapshot::new()
    } else {
        import_legacy(session_id, plan, todos)?
    };
    active.snapshot = Some(graph.clone());
    Ok(graph)
}

fn update_plan_graph(
    base: WorkGraphSnapshot,
    session_id: &str,
    tool: &str,
    plan: &PlanSnapshot,
) -> Result<WorkGraphSnapshot, String> {
    let mut graph = WorkGraph::from_snapshot(base);
    let objective = ensure_objective(&mut graph, session_id, tool, plan)?;
    let desired_active_alias = plan.items.iter().enumerate().find_map(|(index, item)| {
        (item.status == StepStatus::InProgress)
            .then(|| graph.snapshot().compat.plan_order.get(index).cloned())
            .flatten()
            .filter(|node| {
                graph
                    .snapshot()
                    .compat
                    .todos
                    .iter()
                    .any(|binding| &binding.node == node)
            })
    });
    if desired_active_alias.is_some() {
        deactivate_projected_todos(&mut graph, session_id, tool)?;
    }

    let mut order = Vec::with_capacity(plan.items.len());
    for (index, item) in plan.items.iter().enumerate() {
        let id = graph
            .snapshot()
            .compat
            .plan_order
            .get(index)
            .cloned()
            .unwrap_or_else(|| WorkNodeId::derive(session_id, &format!("plan:{index}")));
        let provenance = tool_provenance(graph.snapshot(), tool);
        upsert_node(
            &mut graph,
            session_id,
            tool,
            WorkNode {
                id: id.clone(),
                kind: NodeKind::PlanStep,
                title: item.step.trim().to_string(),
                state: plan_node_state(&item.status),
                acceptance: Vec::new(),
                binding: None,
                evidence: None,
                provenance,
                created_at: now_ms(),
                updated_at: now_ms(),
            },
        )?;
        ensure_contains(&mut graph, session_id, tool, &objective, &id)?;
        order.push(id);
    }
    let mut compat = graph.snapshot().compat.clone();
    compat.plan = CompatPlanMetadata::from_plan_snapshot(plan);
    compat.plan_order = order;
    compat.todos.retain(|binding| {
        binding.plan_index.is_none_or(|index| {
            usize::try_from(index)
                .ok()
                .is_some_and(|i| i < plan.items.len())
        })
    });
    for binding in &mut compat.todos {
        if let Some(index) = binding.plan_index
            && let Some(node) = compat
                .plan_order
                .get(usize::try_from(index).unwrap_or(usize::MAX))
        {
            binding.node.clone_from(node);
        }
    }
    apply_change(
        &mut graph,
        session_id,
        tool,
        WorkGraphChange::ReplaceCompatProjection { compat },
    )?;
    Ok(graph.into_snapshot())
}

fn update_todo_graph(
    base: WorkGraphSnapshot,
    session_id: &str,
    tool: &str,
    todos: &TodoListSnapshot,
) -> Result<WorkGraphSnapshot, String> {
    let mut graph = WorkGraph::from_snapshot(base);
    deactivate_projected_todos(&mut graph, session_id, tool)?;
    let current_plan = project_plan(graph.snapshot());
    let objective = ensure_objective(&mut graph, session_id, tool, &current_plan)?;
    let plan_order = graph.snapshot().compat.plan_order.clone();
    let mut bindings = Vec::with_capacity(todos.items.len());
    for item in &todos.items {
        let title = item.content.trim().to_string();
        let alias = graph
            .snapshot()
            .compat
            .todos
            .iter()
            .find(|binding| binding.legacy_id == item.id)
            .and_then(|binding| {
                binding
                    .plan_index
                    .map(|index| (index, binding.node.clone()))
            })
            .filter(|(index, node)| {
                plan_order.get(usize::try_from(*index).unwrap_or(usize::MAX)) == Some(node)
            });
        let (node, plan_index) = if let Some((index, node)) = alias {
            patch_existing_node(
                &mut graph,
                session_id,
                tool,
                &node,
                title,
                todo_node_state(item.status),
            )?;
            (node, Some(index))
        } else {
            let node = graph
                .snapshot()
                .compat
                .todos
                .iter()
                .find(|binding| binding.legacy_id == item.id && binding.plan_index.is_none())
                .map(|binding| binding.node.clone())
                .unwrap_or_else(|| WorkNodeId::derive(session_id, &format!("todo:{}", item.id)));
            let desired = todo_node_state(item.status);
            let provenance = tool_provenance(graph.snapshot(), tool);
            upsert_node(
                &mut graph,
                session_id,
                tool,
                WorkNode {
                    id: node.clone(),
                    kind: NodeKind::PlanStep,
                    title,
                    state: if desired == NodeState::Active {
                        NodeState::Ready
                    } else {
                        desired
                    },
                    acceptance: Vec::new(),
                    binding: None,
                    evidence: None,
                    provenance,
                    created_at: now_ms(),
                    updated_at: now_ms(),
                },
            )?;
            ensure_contains(&mut graph, session_id, tool, &objective, &node)?;
            if desired == NodeState::Active {
                let clean_title = graph
                    .snapshot()
                    .node(&node)
                    .map(|node| node.title.clone())
                    .ok_or_else(|| format!("node {node} not found after insert"))?;
                patch_existing_node(&mut graph, session_id, tool, &node, clean_title, desired)?;
            }
            (node, None)
        };
        bindings.push(CompatTodoBinding {
            legacy_id: item.id,
            node,
            plan_index,
        });
    }
    let mut compat = graph.snapshot().compat.clone();
    compat.todos = bindings;
    apply_change(
        &mut graph,
        session_id,
        tool,
        WorkGraphChange::ReplaceCompatProjection { compat },
    )?;
    Ok(graph.into_snapshot())
}

fn ensure_objective(
    graph: &mut WorkGraph,
    session_id: &str,
    tool: &str,
    plan: &PlanSnapshot,
) -> Result<WorkNodeId, String> {
    let id = graph
        .snapshot()
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Objective)
        .map(|node| node.id.clone())
        .unwrap_or_else(|| WorkNodeId::derive(session_id, "objective"));
    let title = plan
        .objective
        .as_deref()
        .or(plan.title.as_deref())
        .unwrap_or("Session work")
        .to_string();
    upsert_node(
        graph,
        session_id,
        tool,
        WorkNode {
            id: id.clone(),
            kind: NodeKind::Objective,
            title,
            state: NodeState::Ready,
            acceptance: Vec::new(),
            binding: None,
            evidence: None,
            provenance: tool_provenance(graph.snapshot(), tool),
            created_at: now_ms(),
            updated_at: now_ms(),
        },
    )?;
    Ok(id)
}

fn upsert_node(
    graph: &mut WorkGraph,
    session_id: &str,
    tool: &str,
    node: WorkNode,
) -> Result<(), String> {
    if let Some(existing) = graph.snapshot().node(&node.id) {
        if existing.kind != node.kind {
            return Err(format!("node {} changed kind", node.id));
        }
        patch_existing_node(graph, session_id, tool, &node.id, node.title, node.state)
    } else {
        apply_change(graph, session_id, tool, WorkGraphChange::AddNode { node })
    }
}

fn patch_existing_node(
    graph: &mut WorkGraph,
    session_id: &str,
    tool: &str,
    id: &WorkNodeId,
    title: String,
    state: NodeState,
) -> Result<(), String> {
    let current = graph
        .snapshot()
        .node(id)
        .ok_or_else(|| format!("node {id} not found"))?;
    if current.title == title && current.state == state {
        // Semantic no-ops must stay no-ops. In particular, terminal nodes are
        // immutable, so refreshing another To-do item must not try to rewrite
        // a settled sibling merely to stamp newer tool provenance.
        return Ok(());
    }
    let provenance = tool_provenance(graph.snapshot(), tool);
    apply_change(
        graph,
        session_id,
        tool,
        WorkGraphChange::UpdateNode {
            id: id.clone(),
            patch: WorkNodePatch {
                title: Some(title),
                state: Some(state),
                provenance: Some(provenance),
                ..WorkNodePatch::default()
            },
        },
    )
}

fn ensure_contains(
    graph: &mut WorkGraph,
    session_id: &str,
    tool: &str,
    parent: &WorkNodeId,
    child: &WorkNodeId,
) -> Result<(), String> {
    let id = WorkEdgeId::derive(
        session_id,
        &format!("contains:{}:{}", parent.as_str(), child.as_str()),
    );
    if graph.snapshot().edge(&id).is_some() {
        return Ok(());
    }
    apply_change(
        graph,
        session_id,
        tool,
        WorkGraphChange::AddEdge {
            edge: WorkEdge {
                id,
                kind: EdgeKind::Contains,
                from: parent.clone(),
                to: child.clone(),
            },
        },
    )
}

fn deactivate_projected_todos(
    graph: &mut WorkGraph,
    session_id: &str,
    tool: &str,
) -> Result<(), String> {
    let active = graph
        .snapshot()
        .compat
        .todos
        .iter()
        .filter_map(|binding| {
            graph
                .snapshot()
                .node(&binding.node)
                .filter(|node| node.state == NodeState::Active)
                .map(|node| (node.id.clone(), node.title.clone()))
        })
        .collect::<Vec<_>>();
    for (id, title) in active {
        patch_existing_node(graph, session_id, tool, &id, title, NodeState::Ready)?;
    }
    Ok(())
}

fn apply_change(
    graph: &mut WorkGraph,
    session_id: &str,
    tool: &str,
    change: WorkGraphChange,
) -> Result<(), String> {
    graph
        .apply(
            change,
            ChangeCtx {
                session_id: session_id.to_string(),
                now: now_ms(),
                idempotency_key: None,
            },
        )
        .map(|_| ())
        .map_err(|err| format!("{tool}: {err}"))
}

fn validate_combined(
    graph: &WorkGraphSnapshot,
    plan: &PlanSnapshot,
    todos: &TodoListSnapshot,
) -> Result<(), String> {
    validate(graph).map_err(|err| err.to_string())?;
    if &project_plan(graph) != plan {
        return Err("Work Graph Plan projection is inconsistent".to_string());
    }
    if &project_todos(graph) != todos {
        return Err("Work Graph To-do projection is inconsistent".to_string());
    }
    TodoList::from_snapshot(todos)?;
    Ok(())
}

fn tool_provenance(snapshot: &WorkGraphSnapshot, tool: &str) -> Provenance {
    Provenance::ToolUpdate {
        tool: tool.to_string(),
        call_id: format!("{tool}:{}", snapshot.revision.saturating_add(1)),
    }
}

fn plan_node_state(status: &StepStatus) -> NodeState {
    match status {
        StepStatus::Pending => NodeState::Ready,
        StepStatus::InProgress => NodeState::Active,
        StepStatus::Completed => NodeState::Completed,
    }
}

fn todo_node_state(status: TodoStatus) -> NodeState {
    match status {
        TodoStatus::Pending => NodeState::Ready,
        TodoStatus::InProgress => NodeState::Active,
        TodoStatus::Completed => NodeState::Completed,
        TodoStatus::Cancelled => NodeState::Cancelled,
    }
}

fn resolved_session_id(active: &ActiveGraph, requested: Option<&str>) -> String {
    requested
        .map(str::to_string)
        .or_else(|| active.session_id.clone())
        .unwrap_or_else(|| "unsaved-work".to_string())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// These callers are synchronous methods reached from tool code on the Tokio
// runtime, so this contention retry must not park the worker with
// `thread::sleep`. `yield_now` hands the thread to the holder long enough for
// its microsecond-scale critical section; if the lock is still contended the
// caller surfaces the existing "state is busy" error instead of stalling.
fn retry_lock<T>(
    mutex: &tokio::sync::Mutex<T>,
    retries: u32,
) -> Option<tokio::sync::MutexGuard<'_, T>> {
    for _ in 0..retries {
        if let Ok(guard) = mutex.try_lock() {
            return Some(guard);
        }
        std::thread::yield_now();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::plan::PlanItemArg;
    use crate::work_graph::{EvidenceKind, EvidenceRef, OwnerState};

    #[test]
    fn operation_lifecycle_is_registered_idempotent_and_receipt_only() {
        let runtime = new_shared_work_runtime(
            crate::tools::todo::new_shared_todo_list(),
            crate::tools::plan::new_shared_plan_state(),
        );
        let intent = OperationIntent::new(
            "shell:shell_test",
            "silent `owner` command",
            false,
            "exec_shell",
            "shell_test",
        );
        let node_id = runtime
            .register_operation("session", intent.clone())
            .expect("register before spawn");
        assert_eq!(
            runtime.register_operation("session", intent),
            Ok(node_id.clone()),
            "repeat spawn intent must not duplicate the operation"
        );
        let initialized = runtime
            .capture(Some("session"))
            .expect("capture")
            .expect("graph");
        let node = initialized.graph.node(&node_id).expect("operation node");
        assert_eq!(node.state, NodeState::Initializing);
        assert!(
            initialized
                .graph
                .edges
                .iter()
                .any(|edge| { edge.kind == EdgeKind::Contains && edge.to == node_id })
        );

        let output = EvidenceRef::new(
            EvidenceKind::Receipt {
                owner: "shell".to_string(),
            },
            "shell:shell_test:output",
            Some(4_096),
            true,
        )
        .expect("safe logical receipt");
        assert_eq!(
            runtime.reconcile_operation(
                "session",
                OperationOwnerSnapshot::new("shell:shell_test", OwnerState::Running, 7, 10,)
                    .with_output(output),
            ),
            Ok(true)
        );
        assert_eq!(
            runtime.reconcile_operation(
                "session",
                OperationOwnerSnapshot::new("shell:shell_test", OwnerState::Completed, 7, 11,),
            ),
            Ok(false),
            "the same binding sequence is an idempotent no-op"
        );
        assert!(
            runtime
                .reconcile_operation(
                    "session",
                    OperationOwnerSnapshot::new("shell:unknown", OwnerState::Running, 1, 12,),
                )
                .expect_err("unknown owner must not materialize after spawn")
                .contains("not registered")
        );
        let running = runtime
            .capture(Some("session"))
            .expect("capture running")
            .expect("graph");
        let binding = running
            .graph
            .node(&node_id)
            .and_then(|node| node.binding.as_ref())
            .expect("binding");
        assert_eq!(
            running.graph.node(&node_id).map(|node| node.state),
            Some(NodeState::Active)
        );
        assert_eq!(
            binding
                .last_observation
                .as_ref()
                .and_then(|obs| obs.output.as_ref())
                .and_then(EvidenceRef::raw_bytes),
            Some(4_096)
        );
        let summary = runtime
            .active_operation_summary(Some("session"))
            .expect("compaction re-anchor");
        assert!(summary.contains("shell:shell_test"), "{summary}");
        assert!(summary.contains("silent 'owner' command"), "{summary}");
        assert!(!summary.contains("4,096"), "{summary}");

        assert_eq!(
            runtime.record_reasoning_effort_change(
                Some("session"),
                ReasoningEffortTier::Low,
                ReasoningEffortTier::High,
                codewhale_config::ProviderKind::Moonshot,
                "moonshot",
                Some(codewhale_config::descriptors::defaults::DEFAULT_MOONSHOT_BASE_URL),
                Some("kimi-k2.5"),
            ),
            Ok(Some(node_id.clone()))
        );
        let activity = runtime
            .capture(Some("session"))
            .expect("capture effort activity")
            .expect("graph")
            .graph
            .activities
            .last()
            .cloned()
            .expect("effort activity");
        let ts = match &activity {
            WorkActivityEvent::ReasoningEffortChanged { ts, .. } => *ts,
        };
        assert_eq!(
            activity,
            WorkActivityEvent::ReasoningEffortChanged {
                requested: ReasoningEffortTier::Low,
                effective: ReasoningEffortTier::High,
                provider_kind: Some(codewhale_config::ProviderKind::Moonshot),
                provider: "moonshot".to_string(),
                endpoint_identity: Some(
                    codewhale_config::descriptors::defaults::DEFAULT_MOONSHOT_BASE_URL.to_string()
                ),
                model: Some("kimi-k2.5".to_string()),
                ts,
                operation: Some(node_id.clone()),
            }
        );

        runtime
            .record_operation_approval(
                "session",
                "shell:shell_test",
                "operate-verification:shell_test",
                "exec_shell",
                "approval_test",
            )
            .expect("approval provenance");
        let approved = runtime
            .capture(Some("session"))
            .expect("capture approval")
            .expect("graph");
        assert!(
            approved
                .graph
                .nodes
                .iter()
                .any(|node| node.kind == NodeKind::Approval)
        );
        assert!(
            approved
                .graph
                .edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::RequiresApproval)
        );

        runtime
            .reconcile_operation(
                "session",
                OperationOwnerSnapshot::new("shell:shell_test", OwnerState::Completed, 8, 13),
            )
            .expect("terminal owner report");
        runtime
            .reconcile_observation(
                "session",
                "shell:shell_test",
                OperationObservation::CancelUpdate {
                    outcome: super::super::CancelOutcome::AlreadyFinished,
                    at: 14,
                },
            )
            .expect("already-finished cancellation receipt");
        assert_eq!(
            runtime
                .capture(Some("session"))
                .expect("capture terminal")
                .expect("graph")
                .graph
                .node(&node_id)
                .map(|node| node.state),
            Some(NodeState::Completed),
            "already-finished cancellation must not rewrite owner state"
        );
    }

    #[test]
    fn restore_stales_only_live_ephemeral_operations() {
        let runtime = new_shared_work_runtime(
            crate::tools::todo::new_shared_todo_list(),
            crate::tools::plan::new_shared_plan_state(),
        );
        for id in ["shell:live", "shell:done"] {
            runtime
                .register_operation(
                    "session",
                    OperationIntent::new(id, id, false, "exec_shell", id),
                )
                .expect("register shell");
        }
        runtime
            .reconcile_operation(
                "session",
                OperationOwnerSnapshot::new("shell:live", OwnerState::Running, 1, 1),
            )
            .expect("live shell");
        runtime
            .reconcile_operation(
                "session",
                OperationOwnerSnapshot::new("shell:done", OwnerState::Completed, 1, 1),
            )
            .expect("completed shell");
        let saved = runtime
            .capture(Some("session"))
            .expect("capture")
            .expect("saved graph");

        let restored = new_shared_work_runtime(
            crate::tools::todo::new_shared_todo_list(),
            crate::tools::plan::new_shared_plan_state(),
        );
        restored
            .restore("session", Some(&saved.graph), &saved.todos, &saved.plan)
            .expect("restore graph");
        let graph = restored
            .capture(Some("session"))
            .expect("capture restored")
            .expect("restored graph")
            .graph;
        let state_for = |external: &str| {
            graph
                .nodes
                .iter()
                .find(|node| {
                    node.binding
                        .as_ref()
                        .is_some_and(|binding| binding.external == external)
                })
                .map(|node| node.state)
        };
        assert_eq!(state_for("shell:live"), Some(NodeState::Stale));
        assert_eq!(state_for("shell:done"), Some(NodeState::Completed));
        assert!(restored.has_pending_publish());
    }

    #[test]
    fn durable_owner_same_sequence_recovers_stale_once_and_rejects_regression() {
        let runtime = new_shared_work_runtime(
            crate::tools::todo::new_shared_todo_list(),
            crate::tools::plan::new_shared_plan_state(),
        );
        runtime
            .register_operation(
                "session",
                OperationIntent::new(
                    "task:task_restore",
                    "restored task",
                    true,
                    "task_create",
                    "task_restore",
                ),
            )
            .expect("register durable owner");
        let running = OperationOwnerSnapshot::new("task:task_restore", OwnerState::Running, 7, 10);
        assert_eq!(
            runtime.reconcile_operation("session", running.clone()),
            Ok(true)
        );
        assert_eq!(
            runtime.reconcile_observation(
                "session",
                "task:task_restore",
                OperationObservation::OwnerMissing { checked_at: 11 },
            ),
            Ok(true)
        );
        assert_eq!(
            runtime
                .capture(Some("session"))
                .expect("capture stale")
                .expect("graph")
                .graph
                .nodes
                .iter()
                .find(|node| {
                    node.binding
                        .as_ref()
                        .is_some_and(|binding| binding.external == "task:task_restore")
                })
                .map(|node| node.state),
            Some(NodeState::Stale)
        );

        let replay = OperationOwnerSnapshot::new("task:task_restore", OwnerState::Running, 7, 12);
        assert_eq!(
            runtime.reconcile_operation("session", replay.clone()),
            Ok(true)
        );
        assert_eq!(
            runtime.reconcile_operation("session", replay),
            Ok(false),
            "same-sequence recovery must happen only while the node is stale"
        );
        assert_eq!(
            runtime
                .capture(Some("session"))
                .expect("capture recovered")
                .expect("graph")
                .graph
                .nodes
                .iter()
                .find(|node| {
                    node.binding
                        .as_ref()
                        .is_some_and(|binding| binding.external == "task:task_restore")
                })
                .map(|node| node.state),
            Some(NodeState::Active)
        );
        assert_eq!(
            runtime.reconcile_operation(
                "session",
                OperationOwnerSnapshot::new("task:task_restore", OwnerState::Waiting, 7, 13,),
            ),
            Ok(false),
            "ordinary same-key duplicates retain the reducer no-op contract"
        );
        runtime
            .reconcile_observation(
                "session",
                "task:task_restore",
                OperationObservation::OwnerMissing { checked_at: 14 },
            )
            .expect("mark owner missing again");
        assert!(
            runtime
                .reconcile_operation(
                    "session",
                    OperationOwnerSnapshot::new("task:task_restore", OwnerState::Waiting, 7, 15,),
                )
                .expect_err("inconsistent replay cannot revive a stale node")
                .contains("changed observation")
        );
        assert!(
            runtime
                .reconcile_operation(
                    "session",
                    OperationOwnerSnapshot::new("task:task_restore", OwnerState::Running, 6, 16,),
                )
                .expect_err("owner sequence cannot regress")
                .contains("sequence regressed")
        );
    }

    #[tokio::test]
    async fn replaced_plan_steps_never_parent_new_operations() {
        let runtime = new_shared_work_runtime(
            crate::tools::todo::new_shared_todo_list(),
            crate::tools::plan::new_shared_plan_state(),
        );
        let step = |step: &str, status: StepStatus| PlanItemArg {
            step: step.to_string(),
            status,
        };
        let three = PlanSnapshot {
            items: vec![
                step("first", StepStatus::Pending),
                step("second", StepStatus::Pending),
                step("third", StepStatus::InProgress),
            ],
            ..PlanSnapshot::default()
        };
        runtime
            .apply_plan_update("session", "update_plan", &three)
            .await
            .expect("three-step plan");
        let one = PlanSnapshot {
            items: vec![step("only", StepStatus::Pending)],
            ..PlanSnapshot::default()
        };
        runtime
            .apply_plan_update("session", "update_plan", &one)
            .await
            .expect("replacement plan");

        let operation = runtime
            .register_operation(
                "session",
                OperationIntent::new(
                    "shell:after",
                    "after replacement",
                    false,
                    "exec_shell",
                    "c1",
                ),
            )
            .expect("register operation");
        let graph = runtime
            .capture(Some("session"))
            .expect("capture")
            .expect("graph")
            .graph;
        assert_eq!(graph.compat.plan_order.len(), 1);
        let parent = graph
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Contains && edge.to == operation)
            .map(|edge| edge.from.clone());
        assert_eq!(
            parent,
            Some(graph.compat.plan_order[0].clone()),
            "an orphaned step from the replaced plan must not adopt new work"
        );
    }

    #[test]
    fn legacy_restore_stays_pending_until_first_graph_bearing_write() {
        let todos = crate::tools::todo::new_shared_todo_list();
        let plan = crate::tools::plan::new_shared_plan_state();
        let runtime = new_shared_work_runtime(todos.clone(), plan.clone());
        let legacy_plan = PlanSnapshot {
            items: vec![PlanItemArg {
                step: "Migrate once".to_string(),
                status: StepStatus::InProgress,
            }],
            ..PlanSnapshot::default()
        };

        runtime
            .restore(
                "legacy-session",
                None,
                &TodoListSnapshot::default(),
                &legacy_plan,
            )
            .expect("restore legacy state");
        assert!(runtime.has_pending_publish());
        let captured = runtime
            .capture(Some("legacy-session"))
            .expect("capture imported graph")
            .expect("state");
        assert!(captured.graph.import_digest.is_some());
        assert_eq!(plan.blocking_lock().snapshot(), legacy_plan);
        assert_eq!(
            runtime.publish_pending_sync_matching(Some("other-session"), Some(&captured.graph)),
            Ok(false)
        );
        let mut newer_plan = legacy_plan.clone();
        newer_plan.items[0].step = "Keep the newer work".to_string();
        let newer = runtime
            .restore(
                "legacy-session",
                None,
                &TodoListSnapshot::default(),
                &newer_plan,
            )
            .expect("restore newer work")
            .expect("newer graph");
        assert_eq!(
            runtime.publish_pending_sync_matching(Some("legacy-session"), Some(&captured.graph)),
            Ok(false)
        );
        assert!(runtime.has_pending_publish());
        assert_eq!(
            runtime.publish_pending_sync_matching(Some("legacy-session"), Some(&newer.graph)),
            Ok(true)
        );
        assert!(!runtime.has_pending_publish());
        assert!(todos.blocking_lock().snapshot().is_empty());
        let unsaved = runtime
            .restore(
                "unsaved-work",
                None,
                &TodoListSnapshot::default(),
                &legacy_plan,
            )
            .expect("restore unsaved work")
            .expect("unsaved graph");
        assert_eq!(
            runtime.publish_pending_sync_matching(None, Some(&unsaved.graph)),
            Ok(true)
        );
    }

    /// #6842: one Operation node per finished shell call grew without bound.
    #[test]
    fn ended_shell_operations_are_capped_oldest_first() {
        let runtime = new_shared_work_runtime(
            crate::tools::todo::new_shared_todo_list(),
            crate::tools::plan::new_shared_plan_state(),
        );
        let total = super::super::model::ENDED_OPERATION_CAP + 44;
        let mut ids = Vec::new();
        for i in 0..total {
            let external = format!("shell:cap_{i}");
            let intent =
                OperationIntent::new(&external, "ls", false, "exec_shell", format!("c{i}"));
            ids.push(
                runtime
                    .register_operation("session", intent)
                    .expect("register"),
            );
            for (seq, state) in [(1, OwnerState::Running), (2, OwnerState::Completed)] {
                runtime
                    .reconcile_operation(
                        "session",
                        OperationOwnerSnapshot::new(
                            &external,
                            state,
                            seq,
                            i as i64 * 10 + seq as i64,
                        ),
                    )
                    .expect("reconcile");
            }
        }
        let graph = runtime
            .capture(Some("session"))
            .expect("capture")
            .expect("graph")
            .graph;
        crate::work_graph::validate(&graph).expect("pruned graph validates");
        let operations = graph
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Operation)
            .count();
        // Pruned to the cap at each registration; the last call then ended.
        assert!(
            operations <= super::super::model::ENDED_OPERATION_CAP + 1,
            "{operations}"
        );
        assert!(graph.node(&ids[0]).is_none(), "oldest ended call evicted");
        assert!(graph.node(&ids[total - 1]).is_some(), "newest call kept");
        assert!(
            graph
                .edges
                .iter()
                .all(|edge| graph.node(&edge.from).is_some() && graph.node(&edge.to).is_some())
        );
    }
}
