//! Replaces the bare `WorkflowTool.plan` object with a provider-facing schema
//! for the common structured Workflow launch path.

use serde_json::{Value, json};

/// Keep this shape aligned with `StructuredWorkflowPlan`, `StructuredPlanPhase`,
/// `StructuredPlanChild`, and `GateSpec`. The runtime's legacy aliases and IR
/// parser remain available; the advertised path needs no JavaScript authoring.
pub(super) fn structured_plan_schema() -> Value {
    let child = json!({
        "type": "object",
        "properties": {
            "id": {
                "type": "string",
                "description": "Stable child id. Defaults to label, then a generated phase-local id."
            },
            "label": {
                "type": "string",
                "description": "Short child label, also used as its id when id is absent."
            },
            "prompt": {
                "type": "string",
                "minLength": 1,
                "description": "Concrete assignment and expected result for this child."
            },
            "type": {
                "type": "string",
                "enum": ["general", "explore", "planner", "reviewer", "implement", "test"],
                "description": "Optional worker type. Prefer role/profile for Fleet steps; do not combine them with a conflicting type. Legacy type aliases remain accepted by the runtime."
            },
            "role": {
                "type": "string",
                "description": "Fleet role name. The Fleet supplies its route and authority."
            },
            "profile": {
                "type": "string",
                "description": "Worker profile name, resolved with the selected Fleet and caller policy."
            },
            "model": {
                "type": "string",
                "description": "Optional model selector from agent(action=roster)'s saved shortlist. Omit to use the selected role/profile route. Exact Fleets fix each member's model, so select the member instead of overriding it."
            },
            "mode": {
                "type": "string",
                "enum": ["read_only", "read_write"],
                "description": "Requested child mode. Defaults to the plan risk; it cannot increase caller authority."
            },
            "file_scope": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Workspace-relative file scopes for this child. Use [] when no narrower scope is requested."
            },
            "cwd": {
                "type": "string",
                "description": "Optional repository-relative working directory for this child. Required in multi-repository workspaces so the child (and worktree isolation) resolves the right repository."
            }
        },
        "required": ["prompt", "file_scope"],
        "additionalProperties": false
    });
    let phase = json!({
        "type": "object",
        "properties": {
            "id": {
                "type": "string",
                "description": "Stable phase id. Defaults to title, then a generated phase id."
            },
            "title": {
                "type": "string",
                "description": "Short phase title."
            },
            "parallel": {
                "type": "boolean",
                "description": "Run independent children concurrently. Defaults to true for multiple children; use false when they depend on each other."
            },
            "children": {
                "type": "array",
                "minItems": 1,
                "items": child.clone(),
                "description": "At least one child. Phases run in order and receive prior-phase results; missing required results skip dependent steps while independent work continues."
            }
        },
        "required": ["children"],
        "additionalProperties": false
    });
    let gate = json!({
        "type": "object",
        "properties": {
            "id": { "type": "string" },
            "role": {
                "type": "string",
                "description": "Role whose lifecycle triggers this gate."
            },
            "on": {
                "type": "string",
                "enum": ["role_complete"]
            },
            "gate": {
                "type": "string",
                "enum": ["verify", "review", "approve"]
            },
            "on_fail": {
                "type": "string",
                "enum": ["retry", "block", "escalate"]
            },
            "blocks_role": {
                "type": "string",
                "description": "Downstream role blocked until the gate passes."
            },
            "max_retries": {
                "type": "integer",
                "minimum": 0,
                "maximum": u32::MAX,
                "default": 1,
                "description": "Maximum retries before escalation. Use 1 for the runtime default."
            },
            "artifact_kind": {
                "type": "string",
                "description": "Optional handoff artifact kind, such as findings or verify_report."
            },
            "require_explicit_verdict": {
                "type": "boolean",
                "default": false,
                "description": "Require a standalone first-line PASS, APPROVE, BLOCK, or FAIL verdict. False preserves completion-based gate evaluation."
            }
        },
        "required": ["id", "role", "on", "gate", "on_fail", "max_retries", "require_explicit_verdict"],
        "additionalProperties": false
    });

    // Strict providers require every property and make optional properties
    // nullable. Serde's default Vec/bool/u32 fields accept omission but reject
    // null, so advertise their concrete empty/default values as required.
    json!({
        "type": "object",
        "description": "Structured Workflow plan. Provide goal and either ordered phases or parallel top-level children; use [] for unused collections. No JavaScript is required. Role/profile steps use the selected Fleet. Advanced Workflow IR remains available through script/source_path.",
        "properties": {
            "goal": {
                "type": "string",
                "minLength": 1,
                "description": "Non-empty goal for the complete workflow."
            },
            "risk": {
                "type": "string",
                "enum": ["read_only", "writes", "elevated"],
                "description": "Plan risk and default child mode. Defaults to read_only; writes/elevated remain subject to approval and caller policy."
            },
            "phases": {
                "type": "array",
                "items": phase,
                "description": "Ordered phases with prior-result handoff. Use [] for a flat children plan."
            },
            "children": {
                "type": "array",
                "items": child,
                "description": "Independent children run in parallel when phases is empty. Use [] when phases are provided."
            },
            "gates": {
                "type": "array",
                "items": gate,
                "description": "Role lifecycle gates. Use [] when no additional gates are needed."
            }
        },
        "required": ["goal", "phases", "children", "gates"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::structured_plan_schema;
    use codewhale_models::schema_sanitize::{sanitize, sanitize_for_strict};
    use serde_json::{Value, json};

    fn property_names(schema: &Value) -> Vec<&str> {
        let mut names = schema["properties"]
            .as_object()
            .expect("explicit object properties")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        names.sort_unstable();
        names
    }

    fn assert_strict_objects(schema: &Value) {
        if schema["type"] == "object" {
            let names = property_names(schema);
            assert!(
                !names.is_empty(),
                "a closed empty object cannot carry a plan"
            );
            assert_eq!(schema["additionalProperties"], false);
            let required = schema["required"].as_array().expect("required properties");
            assert_eq!(required.len(), names.len());
            for name in names {
                assert!(required.contains(&json!(name)), "missing required {name}");
            }
        }
        match schema {
            Value::Object(object) => {
                for value in object.values() {
                    assert_strict_objects(value);
                }
            }
            Value::Array(array) => {
                for value in array {
                    assert_strict_objects(value);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn structured_plan_properties_survive_general_and_strict_sanitizers() {
        let mut schema = structured_plan_schema();
        let original = schema.clone();
        sanitize(&mut schema);
        assert_eq!(schema, original);
        sanitize_for_strict(&mut schema);

        assert_eq!(
            property_names(&schema),
            ["children", "gates", "goal", "phases", "risk"]
        );
        let phase = &schema["properties"]["phases"]["items"];
        assert_eq!(
            property_names(phase),
            ["children", "id", "parallel", "title"]
        );
        let child = &phase["properties"]["children"]["items"];
        assert_eq!(
            property_names(child),
            [
                "cwd",
                "file_scope",
                "id",
                "label",
                "mode",
                "model",
                "profile",
                "prompt",
                "role",
                "type"
            ]
        );
        assert_eq!(child, &schema["properties"]["children"]["items"]);
        assert_strict_objects(&schema);
    }

    #[test]
    fn strict_plan_only_marks_actual_option_fields_nullable() {
        let mut schema = structured_plan_schema();
        sanitize_for_strict(&mut schema);
        let phase = &schema["properties"]["phases"]["items"];
        let child = &phase["properties"]["children"]["items"];
        let gate = &schema["properties"]["gates"]["items"];

        for (object, names) in [
            (&schema, &["goal", "phases", "children", "gates"][..]),
            (phase, &["children"][..]),
            (child, &["prompt", "file_scope"][..]),
            (
                gate,
                &[
                    "id",
                    "role",
                    "on",
                    "gate",
                    "on_fail",
                    "max_retries",
                    "require_explicit_verdict",
                ][..],
            ),
        ] {
            for name in names {
                assert!(
                    object["properties"][name].get("nullable").is_none(),
                    "{name} rejects null at runtime"
                );
            }
        }
        for (object, names) in [
            (&schema, &["risk"][..]),
            (phase, &["id", "title", "parallel"][..]),
            (
                child,
                &["id", "label", "type", "role", "profile", "model", "mode"][..],
            ),
            (gate, &["blocks_role", "artifact_kind"][..]),
        ] {
            for name in names {
                assert_eq!(
                    object["properties"][name]["nullable"], true,
                    "{name} is an Option at runtime"
                );
            }
        }
    }

    #[test]
    fn strict_shaped_plan_with_null_options_lowers_without_javascript_authoring() {
        let plan = json!({
            "goal": "Inspect the release receipts",
            "risk": null,
            "phases": [{
                "id": null,
                "title": null,
                "parallel": null,
                "children": [{
                    "id": null,
                    "label": null,
                    "prompt": "Read the existing receipts and report missing evidence.",
                    "type": null,
                    "role": null,
                    "profile": null,
                    "model": null,
                    "mode": null,
                    "file_scope": []
                }]
            }],
            "children": [],
            "gates": [{
                "id": "review",
                "role": "reviewer",
                "on": "role_complete",
                "gate": "review",
                "on_fail": "block",
                "blocks_role": null,
                "max_retries": 1,
                "artifact_kind": null,
                "require_explicit_verdict": false
            }]
        });
        let spec = super::super::structured_plan_to_workflow_spec(&plan)
            .expect("strict provider values match runtime types");
        assert_eq!(spec.goal, "Inspect the release receipts");
        assert_eq!(spec.nodes.len(), 1);
        assert_eq!(spec.gates.len(), 1);
        assert_eq!(spec.gates[0].max_retries, 1);
        let source = super::super::workflow_source_from_plan(&plan)
            .expect("ordinary plans lower without caller-authored JavaScript");
        assert!(source.spec.is_some());
        assert!(!source.source.is_empty());
    }

    #[test]
    fn typed_gate_schema_matches_serialized_gate_spec() {
        let gate: codewhale_workflow::GateSpec = serde_json::from_value(json!({
            "id": "verify",
            "role": "verifier",
            "on": "role_complete",
            "gate": "verify",
            "on_fail": "retry",
            "blocks_role": "builder",
            "max_retries": 2,
            "artifact_kind": "verify_report",
            "require_explicit_verdict": true
        }))
        .expect("existing GateSpec fields");
        let serialized = serde_json::to_value(gate).expect("serialize gate");
        let schema = structured_plan_schema();
        let fields = property_names(&schema["properties"]["gates"]["items"]);
        let mut serialized_fields = serialized
            .as_object()
            .expect("gate object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        serialized_fields.sort_unstable();
        assert_eq!(fields, serialized_fields);
    }

    #[test]
    fn schema_does_not_remove_legacy_runtime_child_aliases_or_defaults() {
        let spec = super::super::structured_plan_to_workflow_spec(&json!({
            "goal": "Inspect the workspace",
            "risk": "safe",
            "children": [{
                "description": "Report the existing behavior.",
                "agent_type": "explorer",
                "mode": "readonly"
            }]
        }))
        .expect("legacy aliases and omitted collections remain runtime-compatible");
        assert_eq!(spec.nodes.len(), 1);
        assert!(spec.gates.is_empty());
    }
}
