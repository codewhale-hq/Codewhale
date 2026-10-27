//! Plan tool implementation with step tracking and validation

use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

// === Types ===

/// Status of a plan step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
}

impl StepStatus {
    #[must_use]
    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "pending" => Some(StepStatus::Pending),
            "in_progress" | "inprogress" => Some(StepStatus::InProgress),
            "completed" | "done" => Some(StepStatus::Completed),
            _ => None,
        }
    }
}

/// Input representation for a plan item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanItemArg {
    pub step: String,
    pub status: StepStatus,
}

/// Update payload used by the plan tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdatePlanArgs {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub objective: Option<String>,
    #[serde(default)]
    pub context_summary: Option<String>,
    #[serde(default)]
    pub explanation: Option<String>,
    #[serde(default)]
    pub sources_used: Vec<String>,
    #[serde(default)]
    pub critical_files: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub recommended_approach: Option<String>,
    #[serde(default)]
    pub verification_plan: Option<String>,
    #[serde(default)]
    pub risks_and_unknowns: Option<String>,
    #[serde(default)]
    pub handoff_packet: Option<String>,
    #[serde(default)]
    pub plan: Vec<PlanItemArg>,
}

// === Plan State ===

/// A plan step with timing information
#[derive(Debug, Clone)]
pub struct PlanStep {
    pub text: String,
    pub status: StepStatus,
    /// When the step was started (transitioned to `InProgress`)
    pub started_at: Option<Instant>,
    /// When the step was completed
    pub completed_at: Option<Instant>,
}

impl PlanStep {
    /// Create a new plan step.
    pub fn new(text: String, status: StepStatus) -> Self {
        Self {
            text,
            status,
            started_at: None,
            completed_at: None,
        }
    }
}

/// Serializable snapshot for display
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSnapshot {
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<PlanItemArg>,
}

impl PlanSnapshot {
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
            && self.items.is_empty()
    }

    /// Parse the user/model-facing `update_plan` payload into a displayable
    /// snapshot. This is intentionally tolerant so saved transcript replay can
    /// keep legacy and partially streamed payloads visible.
    #[must_use]
    pub fn from_tool_input(input: &serde_json::Value) -> Self {
        let mut items = Vec::new();
        if let Some(plan_items) = input.get("plan").and_then(|v| v.as_array()) {
            for item in plan_items {
                let step = item
                    .get("step")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .unwrap_or("");
                if step.is_empty() {
                    continue;
                }
                let status = item
                    .get("status")
                    .and_then(|v| v.as_str())
                    .and_then(StepStatus::from_str)
                    .unwrap_or(StepStatus::Pending);
                items.push(PlanItemArg {
                    step: step.to_string(),
                    status,
                });
            }
        }

        Self {
            title: clean_optional(string_field(input, "title")),
            objective: clean_optional(string_field(input, "objective")),
            context_summary: clean_optional(string_field(input, "context_summary")),
            explanation: clean_optional(string_field(input, "explanation")),
            sources_used: clean_list(string_vec_field(input, "sources_used")),
            critical_files: clean_list(string_vec_field(input, "critical_files")),
            constraints: clean_list(string_vec_field(input, "constraints")),
            recommended_approach: clean_optional(string_field(input, "recommended_approach")),
            verification_plan: clean_optional(string_field(input, "verification_plan")),
            risks_and_unknowns: clean_optional(string_field(input, "risks_and_unknowns")),
            handoff_packet: clean_optional(string_field(input, "handoff_packet")),
            items,
        }
    }
}

/// State tracking for the current plan
#[derive(Debug, Clone, Default)]
pub struct PlanState {
    title: Option<String>,
    objective: Option<String>,
    context_summary: Option<String>,
    explanation: Option<String>,
    sources_used: Vec<String>,
    critical_files: Vec<String>,
    constraints: Vec<String>,
    recommended_approach: Option<String>,
    verification_plan: Option<String>,
    risks_and_unknowns: Option<String>,
    handoff_packet: Option<String>,
    steps: Vec<PlanStep>,
}

impl PlanState {
    pub fn update(&mut self, args: UpdatePlanArgs) {
        self.title = clean_optional(args.title);
        self.objective = clean_optional(args.objective);
        self.context_summary = clean_optional(args.context_summary);
        self.explanation = clean_optional(args.explanation);
        self.sources_used = clean_list(args.sources_used);
        self.critical_files = clean_list(args.critical_files);
        self.constraints = clean_list(args.constraints);
        self.recommended_approach = clean_optional(args.recommended_approach);
        self.verification_plan = clean_optional(args.verification_plan);
        self.risks_and_unknowns = clean_optional(args.risks_and_unknowns);
        self.handoff_packet = clean_optional(args.handoff_packet);

        let now = Instant::now();
        let mut new_steps = Vec::new();
        let mut in_progress_seen = false;

        for item in args.plan {
            let step_text = item.step.trim();
            if step_text.is_empty() {
                continue;
            }
            // Try to find existing step to preserve timing
            let existing = self.steps.iter().find(|s| s.text == step_text);

            let mut status = item.status;
            // Enforce single in_progress
            if status == StepStatus::InProgress {
                if in_progress_seen {
                    status = StepStatus::Pending;
                } else {
                    in_progress_seen = true;
                }
            }

            let step = if let Some(old) = existing {
                let mut s = old.clone();
                let old_status = s.status.clone();
                s.status = status.clone();

                // Track timing transitions
                if old_status == StepStatus::Pending && status == StepStatus::InProgress {
                    s.started_at = Some(now);
                }
                if old_status == StepStatus::InProgress && status == StepStatus::Completed {
                    s.completed_at = Some(now);
                }

                s
            } else {
                let mut s = PlanStep::new(step_text.to_string(), status.clone());
                if status == StepStatus::InProgress {
                    s.started_at = Some(now);
                }
                s
            };

            new_steps.push(step);
        }

        self.steps = new_steps;
    }

    pub fn snapshot(&self) -> PlanSnapshot {
        PlanSnapshot {
            title: self.title.clone(),
            objective: self.objective.clone(),
            context_summary: self.context_summary.clone(),
            explanation: self.explanation.clone(),
            sources_used: self.sources_used.clone(),
            critical_files: self.critical_files.clone(),
            constraints: self.constraints.clone(),
            recommended_approach: self.recommended_approach.clone(),
            verification_plan: self.verification_plan.clone(),
            risks_and_unknowns: self.risks_and_unknowns.clone(),
            handoff_packet: self.handoff_packet.clone(),
            items: self
                .steps
                .iter()
                .map(|s| PlanItemArg {
                    step: s.text.clone(),
                    status: s.status.clone(),
                })
                .collect(),
        }
    }

    /// Restore persisted plan data through the same normalization path used by
    /// `update_plan`. Timing is intentionally session-local and starts fresh.
    #[must_use]
    pub fn from_snapshot(snapshot: &PlanSnapshot) -> Self {
        let mut state = Self::default();
        state.update(UpdatePlanArgs {
            title: snapshot.title.clone(),
            objective: snapshot.objective.clone(),
            context_summary: snapshot.context_summary.clone(),
            explanation: snapshot.explanation.clone(),
            sources_used: snapshot.sources_used.clone(),
            critical_files: snapshot.critical_files.clone(),
            constraints: snapshot.constraints.clone(),
            recommended_approach: snapshot.recommended_approach.clone(),
            verification_plan: snapshot.verification_plan.clone(),
            risks_and_unknowns: snapshot.risks_and_unknowns.clone(),
            handoff_packet: snapshot.handoff_packet.clone(),
            plan: snapshot.items.clone(),
        });
        state
    }

    /// Get counts of steps by status
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut pending = 0;
        let mut in_progress = 0;
        let mut completed = 0;
        for s in &self.steps {
            match s.status {
                StepStatus::Pending => pending += 1,
                StepStatus::InProgress => in_progress += 1,
                StepStatus::Completed => completed += 1,
            }
        }
        (pending, in_progress, completed)
    }

    /// Get progress as a percentage
    pub fn progress_percent(&self) -> u8 {
        if self.steps.is_empty() {
            return 0;
        }
        let completed = self
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Completed)
            .count();
        let percent = completed.saturating_mul(100) / self.steps.len();
        u8::try_from(percent).unwrap_or(u8::MAX)
    }
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn clean_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

// === UpdatePlanTool - ToolSpec implementation ===

/// Shared reference to `PlanState` for use across tools
pub type SharedPlanState = Arc<Mutex<PlanState>>;

/// Create a new shared `PlanState`
pub fn new_shared_plan_state() -> SharedPlanState {
    Arc::new(Mutex::new(PlanState::default()))
}

/// Tool for updating the implementation plan
pub struct UpdatePlanTool {
    plan_state: SharedPlanState,
}

impl UpdatePlanTool {
    pub fn new(plan_state: SharedPlanState) -> Self {
        Self { plan_state }
    }
}

#[async_trait]
impl ToolSpec for UpdatePlanTool {
    fn name(&self) -> &'static str {
        "update_plan"
    }

    fn description(&self) -> &'static str {
        "Legacy compatibility tool for loading older Plan artifacts. New work uses the canonical work_update list and a normal Plan-mode response."
    }

    fn model_visible(&self) -> bool {
        // Older transcripts and sessions can still replay this tool, but new
        // model turns get one progress model (`work_update`) instead of the
        // retired Strategy/Plan surface.
        false
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Optional short title for the plan artifact"
                },
                "objective": {
                    "type": "string",
                    "description": "What the plan is trying to accomplish"
                },
                "context_summary": {
                    "type": "string",
                    "description": "Brief summary of the evidence and current state behind the plan"
                },
                "explanation": {
                    "type": "string",
                    "description": "Legacy-compatible high-level explanation of the plan or approach"
                },
                "sources_used": {
                    "type": "array",
                    "description": "Files, issues, PRs, commands, or other evidence used to ground the plan. Do not include secrets.",
                    "items": { "type": "string" }
                },
                "critical_files": {
                    "type": "array",
                    "description": "Repo paths or surfaces likely to be edited or verified. Do not include secrets.",
                    "items": { "type": "string" }
                },
                "constraints": {
                    "type": "array",
                    "description": "Hard requirements, user preferences, or boundaries the implementation must respect",
                    "items": { "type": "string" }
                },
                "recommended_approach": {
                    "type": "string",
                    "description": "Recommended implementation strategy and important trade-offs"
                },
                "verification_plan": {
                    "type": "string",
                    "description": "Tests, checks, or manual verification expected before the work is considered done"
                },
                "risks_and_unknowns": {
                    "type": "string",
                    "description": "Known risks, blockers, or unresolved questions"
                },
                "handoff_packet": {
                    "type": "string",
                    "description": "Concise continuation notes for another agent or a later session"
                },
                "plan": {
                    "type": "array",
                    "description": "Legacy replay field; new work must use work_update",
                    "deprecated": true,
                    "items": { "type": "object" }
                }
            }
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let empty_plan = Vec::new();
        let plan_items = match input.get("plan") {
            Some(value) => value
                .as_array()
                .ok_or_else(|| ToolError::invalid_input("Invalid 'plan' array"))?,
            None => &empty_plan,
        };

        let mut plan_args = Vec::new();
        for item in plan_items {
            let step = item
                .get("step")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::invalid_input("Plan item missing 'step'"))?;

            let status_str = item
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending");

            let status = StepStatus::from_str(status_str).unwrap_or(StepStatus::Pending);

            plan_args.push(PlanItemArg {
                step: step.to_string(),
                status,
            });
        }

        let args = UpdatePlanArgs {
            title: string_field(&input, "title"),
            objective: string_field(&input, "objective"),
            context_summary: string_field(&input, "context_summary"),
            explanation: string_field(&input, "explanation"),
            sources_used: string_vec_field(&input, "sources_used"),
            critical_files: string_vec_field(&input, "critical_files"),
            constraints: string_vec_field(&input, "constraints"),
            recommended_approach: string_field(&input, "recommended_approach"),
            verification_plan: string_field(&input, "verification_plan"),
            risks_and_unknowns: string_field(&input, "risks_and_unknowns"),
            handoff_packet: string_field(&input, "handoff_packet"),
            plan: plan_args,
        };

        let mut next_state = PlanState::default();
        next_state.update(args);
        let desired = next_state.snapshot();
        let snapshot = if let Some(work) = context.runtime.work.as_ref()
            && work.matches_plan(&self.plan_state)
        {
            work.apply_plan_update(&context.state_namespace, self.name(), &desired)
                .await
                .map_err(ToolError::execution_failed)?
        } else {
            let mut state = self.plan_state.lock().await;
            state.update(UpdatePlanArgs {
                title: desired.title.clone(),
                objective: desired.objective.clone(),
                context_summary: desired.context_summary.clone(),
                explanation: desired.explanation.clone(),
                sources_used: desired.sources_used.clone(),
                critical_files: desired.critical_files.clone(),
                constraints: desired.constraints.clone(),
                recommended_approach: desired.recommended_approach.clone(),
                verification_plan: desired.verification_plan.clone(),
                risks_and_unknowns: desired.risks_and_unknowns.clone(),
                handoff_packet: desired.handoff_packet.clone(),
                plan: desired.items.clone(),
            });
            state.snapshot()
        };
        let state = PlanState::from_snapshot(&snapshot);
        let (pending, in_progress, completed) = state.counts();
        let progress = state.progress_percent();

        let result = serde_json::to_string_pretty(&snapshot).unwrap_or_else(|_| "{}".to_string());

        Ok(ToolResult::success(format!(
            "Plan updated: {pending} pending, {in_progress} in progress, {completed} completed ({progress}% done)\n{result}"
        )))
    }
}

fn string_field(input: &serde_json::Value, field: &str) -> Option<String> {
    input
        .get(field)
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string)
}

fn string_vec_field(input: &serde_json::Value, field: &str) -> Vec<String> {
    input
        .get(field)
        .and_then(|v| v.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(std::string::ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::spec::{ToolContext, ToolSpec};
    use serde_json::json;

    #[test]
    fn update_plan_is_hidden_replay_compatibility() {
        let tool = UpdatePlanTool::new(new_shared_plan_state());
        let description = tool.description();

        assert!(!tool.model_visible());
        assert!(description.contains("Legacy compatibility"));
        assert!(description.contains("canonical work_update list"));
    }

    #[tokio::test]
    async fn update_plan_routes_through_attached_work_graph() {
        let plan = new_shared_plan_state();
        let todos = crate::tools::todo::new_shared_todo_list();
        let work = crate::work_graph::new_shared_work_runtime(todos, plan.clone());
        let tool = UpdatePlanTool::new(plan);
        let mut context = ToolContext::new(std::env::temp_dir());
        context.runtime.work = Some(work.clone());

        tool.execute(
            json!({
                "objective": "Prove the real tool path",
                "plan": [{"step": "Update graph", "status": "in_progress"}]
            }),
            &context,
        )
        .await
        .expect("update_plan succeeds");

        let state = work
            .capture(Some(&context.state_namespace))
            .expect("capture")
            .expect("graph state");
        assert_eq!(
            state.plan.objective.as_deref(),
            Some("Prove the real tool path")
        );
        assert_eq!(state.graph.compat.plan_order.len(), 1);
    }

    #[test]
    fn plan_state_treats_every_artifact_field_as_non_empty() {
        let cases = vec![
            UpdatePlanArgs {
                title: Some("Title".to_string()),
                ..UpdatePlanArgs::default()
            },
            UpdatePlanArgs {
                objective: Some("Objective".to_string()),
                ..UpdatePlanArgs::default()
            },
            UpdatePlanArgs {
                context_summary: Some("Context".to_string()),
                ..UpdatePlanArgs::default()
            },
            UpdatePlanArgs {
                explanation: Some("Explanation".to_string()),
                ..UpdatePlanArgs::default()
            },
            UpdatePlanArgs {
                sources_used: vec!["gh issue view 2691".to_string()],
                ..UpdatePlanArgs::default()
            },
            UpdatePlanArgs {
                critical_files: vec!["crates/tui/src/tools/plan.rs".to_string()],
                ..UpdatePlanArgs::default()
            },
            UpdatePlanArgs {
                constraints: vec!["Preserve legacy payloads".to_string()],
                ..UpdatePlanArgs::default()
            },
            UpdatePlanArgs {
                recommended_approach: Some("Do the narrow slice".to_string()),
                ..UpdatePlanArgs::default()
            },
            UpdatePlanArgs {
                verification_plan: Some("Run focused tests".to_string()),
                ..UpdatePlanArgs::default()
            },
            UpdatePlanArgs {
                risks_and_unknowns: Some("Replay may drift".to_string()),
                ..UpdatePlanArgs::default()
            },
            UpdatePlanArgs {
                handoff_packet: Some("Next agent should inspect rendering".to_string()),
                ..UpdatePlanArgs::default()
            },
        ];

        for args in cases {
            let mut state = PlanState::default();
            state.update(args);
            assert!(
                !state.snapshot().is_empty(),
                "artifact metadata must keep plan state visible"
            );
        }
    }

    #[test]
    fn plan_state_snapshot_trims_blank_artifact_values() {
        let mut state = PlanState::default();
        state.update(UpdatePlanArgs {
            title: Some("  Rich plan  ".to_string()),
            sources_used: vec![" ".to_string(), " gh issue view 2691 ".to_string()],
            critical_files: vec![" crates/tui/src/tools/plan.rs ".to_string()],
            constraints: vec!["".to_string(), " no secrets ".to_string()],
            plan: vec![
                PlanItemArg {
                    step: "   ".to_string(),
                    status: StepStatus::Pending,
                },
                PlanItemArg {
                    step: "  render sections  ".to_string(),
                    status: StepStatus::InProgress,
                },
            ],
            ..UpdatePlanArgs::default()
        });

        let snapshot = state.snapshot();
        assert_eq!(snapshot.title.as_deref(), Some("Rich plan"));
        assert_eq!(snapshot.sources_used, vec!["gh issue view 2691"]);
        assert_eq!(
            snapshot.critical_files,
            vec!["crates/tui/src/tools/plan.rs"]
        );
        assert_eq!(snapshot.constraints, vec!["no secrets"]);
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0].step, "render sections");
        assert_eq!(snapshot.items[0].status, StepStatus::InProgress);
    }

    #[test]
    fn plan_state_restores_from_persisted_snapshot() {
        let snapshot = PlanSnapshot {
            objective: Some("Restore Work state".to_string()),
            items: vec![
                PlanItemArg {
                    step: "inspect".to_string(),
                    status: StepStatus::Completed,
                },
                PlanItemArg {
                    step: "verify".to_string(),
                    status: StepStatus::InProgress,
                },
            ],
            ..PlanSnapshot::default()
        };

        let restored = PlanState::from_snapshot(&snapshot);
        assert_eq!(restored.snapshot(), snapshot);
    }

    #[test]
    fn snapshot_serde_skips_empty_fields_and_deserializes_legacy() {
        let snapshot = PlanSnapshot {
            objective: Some("Ship PlanArtifact".to_string()),
            items: vec![PlanItemArg {
                step: "keep legacy replay working".to_string(),
                status: StepStatus::Completed,
            }],
            ..PlanSnapshot::default()
        };

        let value = serde_json::to_value(&snapshot).expect("serialize snapshot");
        assert!(value.get("objective").is_some());
        assert!(value.get("title").is_none());
        assert!(value.get("sources_used").is_none());
        assert!(value.get("constraints").is_none());

        let legacy: PlanSnapshot = serde_json::from_value(json!({
            "explanation": "Legacy explanation",
            "items": [
                { "step": "legacy step", "status": "pending" }
            ]
        }))
        .expect("legacy snapshot should deserialize");
        assert_eq!(legacy.explanation.as_deref(), Some("Legacy explanation"));
        assert_eq!(legacy.items.len(), 1);
        assert!(legacy.sources_used.is_empty());
    }

    #[tokio::test]
    async fn legacy_update_plan_still_works() {
        let state = new_shared_plan_state();
        let tool = UpdatePlanTool::new(state.clone());
        let context = ToolContext::new(std::env::temp_dir());

        tool.execute(
            json!({
                "explanation": "Legacy shape",
                "plan": [
                    { "step": "inspect", "status": "completed" },
                    { "step": "patch", "status": "in_progress" }
                ]
            }),
            &context,
        )
        .await
        .expect("legacy update_plan should succeed");

        let snapshot = state.lock().await.snapshot();
        assert_eq!(snapshot.explanation.as_deref(), Some("Legacy shape"));
        assert_eq!(snapshot.items.len(), 2);
        assert_eq!(snapshot.items[0].status, StepStatus::Completed);
        assert_eq!(snapshot.items[1].status, StepStatus::InProgress);
    }

    #[tokio::test]
    async fn update_plan_tool_accepts_metadata_only_payload() {
        let state = new_shared_plan_state();
        let tool = UpdatePlanTool::new(state.clone());
        let context = ToolContext::new(std::env::temp_dir());

        let result = tool
            .execute(
                json!({
                    "objective": "Make Plan mode reviewable",
                    "sources_used": ["gh issue view 2691"],
                    "critical_files": ["crates/tui/src/tools/plan.rs"],
                    "verification_plan": "Run focused plan tests"
                }),
                &context,
            )
            .await
            .expect("metadata-only update_plan should succeed");

        assert!(result.content.contains("Make Plan mode reviewable"));
        let snapshot = state.lock().await.snapshot();
        assert!(!snapshot.is_empty());
        assert!(snapshot.items.is_empty());
        assert_eq!(
            snapshot.critical_files,
            vec!["crates/tui/src/tools/plan.rs"]
        );
    }
}
