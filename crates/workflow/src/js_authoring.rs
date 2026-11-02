use serde::Deserialize;
use thiserror::Error;

use crate::{
    BranchSpec, BudgetSpec, CondSpec, ExpandSpec, GateSpec, LeafSpec, LoopUntilSpec, ModelPolicy,
    PermissionSpec, PromotionPolicy, ReduceSpec, SequenceSpec, TeacherReviewSpec, WorkflowNode,
    WorkflowSpec, validate_workflow_nodes,
};

pub type JavascriptWorkflowResult<T> = std::result::Result<T, JavascriptWorkflowError>;

#[derive(Debug, Error)]
pub enum JavascriptWorkflowError {
    #[error("workflow source contains unsupported construct `{construct}`")]
    UnsupportedConstruct { construct: &'static str },
    #[error("workflow source did not call workflow({{...}})")]
    MissingWorkflowCall,
    #[error("workflow({{...}}) object could not be extracted: {0}")]
    InvalidWorkflowObject(String),
    #[error("invalid workflow JSON object: {0}")]
    InvalidJson(serde_json::Error),
    #[error("invalid workflow node: {0}")]
    InvalidNode(String),
}

pub fn compile_javascript_workflow(
    identifier: &str,
    source: &str,
) -> JavascriptWorkflowResult<WorkflowSpec> {
    compile_js_like_workflow(identifier, source)
}

pub fn compile_typescript_workflow(
    identifier: &str,
    source: &str,
) -> JavascriptWorkflowResult<WorkflowSpec> {
    compile_js_like_workflow(identifier, source)
}

fn compile_js_like_workflow(
    _identifier: &str,
    source: &str,
) -> JavascriptWorkflowResult<WorkflowSpec> {
    reject_unsupported_constructs(source)?;
    let object = extract_workflow_object(source)?;
    let authored = serde_json::from_str::<JsWorkflowSpec>(object)
        .map_err(JavascriptWorkflowError::InvalidJson)?;
    let mut workflow = authored.into_workflow();
    normalize_leaf_profiles(&mut workflow.nodes);
    normalize_gate_roles(&mut workflow.gates);
    if workflow.goal.trim().is_empty() {
        return Err(JavascriptWorkflowError::InvalidNode(
            "workflow goal cannot be empty".to_string(),
        ));
    }
    validate_workflow_nodes(&workflow.nodes)
        .map_err(|error| JavascriptWorkflowError::InvalidNode(error.to_string()))?;
    Ok(workflow)
}

// Role/profile names are case-insensitive roster keys; the IR stores the
// canonical lowercase form. Invalid tokens are left as-is so validation
// reports them.
fn normalize_leaf_profiles(nodes: &mut [WorkflowNode]) {
    for node in nodes {
        match node {
            WorkflowNode::Leaf(spec) => {
                if let Some(role) = spec.role.as_mut() {
                    *role = role.trim().to_lowercase();
                }
                if let Some(profile) = spec.profile.as_mut() {
                    *profile = profile.trim().to_lowercase();
                }
            }
            WorkflowNode::BranchSet(spec) => normalize_leaf_profiles(&mut spec.children),
            WorkflowNode::Sequence(spec) => normalize_leaf_profiles(&mut spec.children),
            WorkflowNode::LoopUntil(spec) => normalize_leaf_profiles(&mut spec.children),
            WorkflowNode::Cond(spec) => {
                normalize_leaf_profiles(&mut spec.then_nodes);
                normalize_leaf_profiles(&mut spec.else_nodes);
            }
            WorkflowNode::Expand(spec) => {
                if let Some(template) = spec.template.as_deref_mut() {
                    normalize_leaf_profiles(std::slice::from_mut(template));
                }
            }
            WorkflowNode::Reduce(_) | WorkflowNode::TeacherReview(_) => {}
        }
    }
}

fn normalize_gate_roles(gates: &mut [GateSpec]) {
    for gate in gates {
        gate.role = gate.role.trim().to_lowercase();
        if let Some(blocks_role) = gate.blocks_role.as_mut() {
            *blocks_role = blocks_role.trim().to_lowercase();
        }
    }
}

fn reject_unsupported_constructs(source: &str) -> JavascriptWorkflowResult<()> {
    for (needle, construct) in [
        ("import ", "import"),
        ("import(", "dynamic import"),
        ("require(", "require"),
        ("fetch(", "fetch"),
        ("XMLHttpRequest", "XMLHttpRequest"),
        ("WebSocket", "WebSocket"),
        ("process.", "process"),
        ("Deno.", "Deno"),
        ("Bun.", "Bun"),
        ("child_process", "child_process"),
        ("exec(", "exec"),
        ("spawn(", "spawn"),
        ("open(", "open"),
        ("readFile", "readFile"),
        ("writeFile", "writeFile"),
        ("async ", "async"),
        ("await ", "await"),
        ("eval(", "eval"),
        ("new Function", "Function"),
    ] {
        if source.contains(needle) {
            return Err(JavascriptWorkflowError::UnsupportedConstruct { construct });
        }
    }
    Ok(())
}

fn extract_workflow_object(source: &str) -> JavascriptWorkflowResult<&str> {
    let workflow_pos = source
        .find("workflow")
        .ok_or(JavascriptWorkflowError::MissingWorkflowCall)?;
    let open_paren_rel = source[workflow_pos..]
        .find('(')
        .ok_or(JavascriptWorkflowError::MissingWorkflowCall)?;
    let open_paren = workflow_pos + open_paren_rel;
    let object_start = source[open_paren + 1..]
        .char_indices()
        .find_map(|(idx, ch)| {
            if ch.is_whitespace() {
                None
            } else {
                Some((open_paren + 1 + idx, ch))
            }
        })
        .ok_or(JavascriptWorkflowError::MissingWorkflowCall)?;
    if object_start.1 != '{' {
        return Err(JavascriptWorkflowError::InvalidWorkflowObject(
            "workflow(...) must receive a JSON-compatible object literal".to_string(),
        ));
    }

    let mut depth = 0usize;
    let mut in_string: Option<char> = None;
    let mut escape = false;
    for (idx, ch) in source[object_start.0..].char_indices() {
        let absolute = object_start.0 + idx;
        if let Some(quote) = in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == quote {
                in_string = None;
            }
            continue;
        }

        match ch {
            '"' | '\'' | '`' => in_string = Some(ch),
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1).ok_or_else(|| {
                    JavascriptWorkflowError::InvalidWorkflowObject(
                        "unbalanced closing brace".to_string(),
                    )
                })?;
                if depth == 0 {
                    return Ok(&source[object_start.0..=absolute]);
                }
            }
            _ => {}
        }
    }

    Err(JavascriptWorkflowError::InvalidWorkflowObject(
        "missing closing brace for workflow object".to_string(),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsWorkflowSpec {
    #[serde(default)]
    id: Option<String>,
    goal: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    budget: BudgetSpec,
    #[serde(default)]
    permissions: PermissionSpec,
    #[serde(default)]
    model_policy: ModelPolicy,
    #[serde(default)]
    promotion_policy: PromotionPolicy,
    #[serde(default)]
    gates: Vec<GateSpec>,
    #[serde(default)]
    nodes: Vec<JsWorkflowNode>,
}

impl JsWorkflowSpec {
    fn into_workflow(self) -> WorkflowSpec {
        WorkflowSpec {
            id: self.id,
            goal: self.goal,
            description: self.description,
            budget: self.budget,
            permissions: self.permissions,
            model_policy: self.model_policy,
            promotion_policy: self.promotion_policy,
            gates: self.gates,
            nodes: self
                .nodes
                .into_iter()
                .map(JsWorkflowNode::into_node)
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum JsWorkflowNode {
    Raw(WorkflowNode),
    Agent(JsAgentNode),
    Branch(JsBranchNode),
    Sequence(JsSequenceNode),
    Reduce(JsReduceNode),
    TeacherReview(JsTeacherReviewNode),
    LoopUntil(JsLoopUntilNode),
    Cond(JsCondNode),
    Expand(JsExpandNode),
}

impl JsWorkflowNode {
    fn into_node(self) -> WorkflowNode {
        match self {
            Self::Raw(node) => node,
            Self::Agent(node) => WorkflowNode::Leaf(node.agent),
            Self::Branch(node) => WorkflowNode::BranchSet(node.branch.into_branch()),
            Self::Sequence(node) => WorkflowNode::Sequence(node.sequence.into_sequence()),
            Self::Reduce(node) => WorkflowNode::Reduce(node.reduce),
            Self::TeacherReview(node) => WorkflowNode::TeacherReview(node.teacher_review),
            Self::LoopUntil(node) => WorkflowNode::LoopUntil(node.loop_until.into_loop_until()),
            Self::Cond(node) => WorkflowNode::Cond(node.cond.into_cond()),
            Self::Expand(node) => WorkflowNode::Expand(node.expand.into_expand()),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsAgentNode {
    agent: LeafSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsBranchNode {
    branch: JsBranchSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsBranchSpec {
    id: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "default_true")]
    parallel: bool,
    #[serde(default)]
    budget: BudgetSpec,
    #[serde(default)]
    permissions: PermissionSpec,
    #[serde(default)]
    model_policy: ModelPolicy,
    #[serde(default)]
    children: Vec<JsWorkflowNode>,
}

impl JsBranchSpec {
    fn into_branch(self) -> BranchSpec {
        BranchSpec {
            id: self.id,
            description: self.description,
            parallel: self.parallel,
            budget: self.budget,
            permissions: self.permissions,
            model_policy: self.model_policy,
            children: self
                .children
                .into_iter()
                .map(JsWorkflowNode::into_node)
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsSequenceNode {
    sequence: JsSequenceSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsSequenceSpec {
    id: String,
    #[serde(default)]
    children: Vec<JsWorkflowNode>,
}

impl JsSequenceSpec {
    fn into_sequence(self) -> SequenceSpec {
        SequenceSpec {
            id: self.id,
            children: self
                .children
                .into_iter()
                .map(JsWorkflowNode::into_node)
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsReduceNode {
    reduce: ReduceSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsTeacherReviewNode {
    teacher_review: TeacherReviewSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsLoopUntilNode {
    loop_until: JsLoopUntilSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsLoopUntilSpec {
    id: String,
    condition: String,
    #[serde(default)]
    max_iterations: Option<u32>,
    #[serde(default)]
    children: Vec<JsWorkflowNode>,
}

impl JsLoopUntilSpec {
    fn into_loop_until(self) -> LoopUntilSpec {
        LoopUntilSpec {
            id: self.id,
            condition: self.condition,
            max_iterations: self.max_iterations,
            children: self
                .children
                .into_iter()
                .map(JsWorkflowNode::into_node)
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsCondNode {
    cond: JsCondSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsCondSpec {
    id: String,
    condition: String,
    #[serde(default)]
    then_nodes: Vec<JsWorkflowNode>,
    #[serde(default)]
    else_nodes: Vec<JsWorkflowNode>,
}

impl JsCondSpec {
    fn into_cond(self) -> CondSpec {
        CondSpec {
            id: self.id,
            condition: self.condition,
            then_nodes: self
                .then_nodes
                .into_iter()
                .map(JsWorkflowNode::into_node)
                .collect(),
            else_nodes: self
                .else_nodes
                .into_iter()
                .map(JsWorkflowNode::into_node)
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsExpandNode {
    expand: JsExpandSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsExpandSpec {
    id: String,
    source: String,
    #[serde(default)]
    max_children: Option<usize>,
    #[serde(default)]
    template: Option<Box<JsWorkflowNode>>,
}

impl JsExpandSpec {
    fn into_expand(self) -> ExpandSpec {
        ExpandSpec {
            id: self.id,
            source: self.source,
            max_children: self.max_children,
            template: self.template.map(|node| Box::new(node.into_node())),
        }
    }
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentType, GateKind, GateOn, GateOnFail, GateOutcome, GateState, LaneGateBoard, TaskMode,
    };

    #[test]
    fn javascript_workflow_compiles_branch_reduce_to_ir() {
        let source = r#"
export default workflow({
  "id": "js-audit",
  "goal": "Audit a change with parallel agents",
  "nodes": [
    {
      "branch": {
        "id": "parallel-audit",
        "children": [
          {
            "agent": {
              "id": "docs-audit",
              "prompt": "Inspect docs for missing updates",
              "agent_type": "review",
              "file_scope": ["docs"]
            }
          },
          {
            "agent": {
              "id": "tests-audit",
              "prompt": "Inspect targeted tests",
              "agent_type": "verifier",
              "budget": { "max_steps": 4 }
            }
          }
        ]
      }
    },
    {
      "reduce": {
        "id": "synthesize",
        "inputs": ["docs-audit", "tests-audit"],
        "prompt": "Merge the branch findings"
      }
    }
  ]
});
"#;

        let workflow =
            compile_javascript_workflow("audit.workflow.js", source).expect("compile JS workflow");

        assert_eq!(workflow.id.as_deref(), Some("js-audit"));
        assert_eq!(workflow.nodes.len(), 2);
        let WorkflowNode::BranchSet(branch) = &workflow.nodes[0] else {
            panic!("first node should be a branch");
        };
        assert!(branch.parallel);
        assert_eq!(branch.children.len(), 2);
        let WorkflowNode::Leaf(leaf) = &branch.children[1] else {
            panic!("second branch child should be a leaf");
        };
        assert_eq!(leaf.agent_type, AgentType::Verifier);
        assert_eq!(leaf.budget.max_steps, Some(4));
        assert!(matches!(workflow.nodes[1], WorkflowNode::Reduce(_)));
    }

    #[test]
    fn typescript_workflow_allows_satisfies_suffix_without_executing_js() {
        let source = r#"
export default workflow({
  "goal": "TS authored workflow",
  "nodes": [
    { "agent": { "id": "scan", "prompt": "scan safely" } }
  ]
} satisfies WorkflowSpec);
"#;

        let workflow =
            compile_typescript_workflow("scan.workflow.ts", source).expect("compile TS workflow");

        assert_eq!(workflow.goal, "TS authored workflow");
        assert_eq!(workflow.nodes.len(), 1);
    }

    #[test]
    fn javascript_workflow_accepts_and_normalizes_agent_profile() {
        let source = r#"
workflow({
  "goal": "profile routing",
  "nodes": [
    { "agent": { "id": "review", "prompt": "review the diff", "profile": " Reviewer " } },
    { "agent": { "id": "scan", "prompt": "scan safely" } }
  ]
});
"#;

        let workflow = compile_javascript_workflow("profile.workflow.js", source)
            .expect("profile-carrying workflow should compile");

        let WorkflowNode::Leaf(review) = &workflow.nodes[0] else {
            panic!("first node should be a leaf");
        };
        assert_eq!(review.profile.as_deref(), Some("reviewer"));
        let WorkflowNode::Leaf(scan) = &workflow.nodes[1] else {
            panic!("second node should be a leaf");
        };
        assert_eq!(scan.profile, None);
    }

    #[test]
    fn javascript_workflow_accepts_and_normalizes_agent_role() {
        let source = r#"
workflow({
  "goal": "role routing",
  "nodes": [
    { "agent": { "id": "scout-issue", "prompt": "Investigate #4090. Read-only.", "role": " Scout " } },
    { "agent": { "id": "fix-it", "prompt": "Apply minimal fix.", "role": "implementer" } }
  ]
});
"#;

        let workflow = compile_javascript_workflow("role.workflow.js", source)
            .expect("role-carrying workflow should compile");

        let WorkflowNode::Leaf(scout) = &workflow.nodes[0] else {
            panic!("first node should be a leaf");
        };
        assert_eq!(scout.role.as_deref(), Some("scout"));
        assert_eq!(scout.profile, None);
        // Provider/model are not required identity fields on role steps.
        assert_eq!(scout.model_policy.provider, None);
        assert_eq!(scout.model_policy.model, None);

        let WorkflowNode::Leaf(fix) = &workflow.nodes[1] else {
            panic!("second node should be a leaf");
        };
        assert_eq!(fix.role.as_deref(), Some("implementer"));
    }

    #[test]
    fn javascript_workflow_accepts_gate_specs() {
        let source = r#"
workflow({
  "goal": "role gates",
  "gates": [
    {
      "id": "scout-findings",
      "role": " Scout ",
      "on": "role_complete",
      "gate": "approve",
      "on_fail": "block",
      "blocks_role": " Implementer ",
      "artifact_kind": "findings"
    }
  ],
  "nodes": [
    { "agent": { "id": "scout", "prompt": "Find risk.", "role": "scout" } },
    { "agent": { "id": "fix", "prompt": "Use findings.", "role": "implementer" } }
  ]
});
"#;

        let workflow =
            compile_javascript_workflow("gates.workflow.js", source).expect("compile gates");

        assert_eq!(workflow.gates.len(), 1);
        let gate = &workflow.gates[0];
        assert_eq!(gate.id, "scout-findings");
        assert_eq!(gate.role, "scout");
        assert_eq!(gate.on, GateOn::RoleComplete);
        assert_eq!(gate.gate, GateKind::Approve);
        assert_eq!(gate.on_fail, GateOnFail::Block);
        assert_eq!(gate.blocks_role.as_deref(), Some("implementer"));
        assert_eq!(gate.artifact_kind.as_deref(), Some("findings"));
    }

    #[test]
    fn stopship_acceptance_fixture_is_read_only_and_gate_complete() {
        let source = include_str!("../../../workflows/stopship.workflow.js");
        let workflow = compile_javascript_workflow("stopship.workflow.js", source)
            .expect("compile stopship acceptance fixture");

        assert_eq!(workflow.id.as_deref(), Some("stopship-release-acceptance"));
        let WorkflowNode::Sequence(sequence) = &workflow.nodes[0] else {
            panic!("acceptance fixture should begin with one ordered role chain");
        };
        let expected_children = ["explore", "implement", "reviewer", "test", "release_lead"];
        assert_eq!(sequence.children.len(), expected_children.len());
        for (node, expected_role) in sequence.children.iter().zip(expected_children) {
            let WorkflowNode::Leaf(leaf) = node else {
                panic!("acceptance role chain must contain only agent leaves");
            };
            assert_eq!(leaf.role.as_deref(), Some(expected_role));
            assert_eq!(leaf.mode, TaskMode::ReadOnly);
            assert!(!leaf.permissions.allow_write);
            // No explicit `allowed_tools` anywhere: the read-only lowering
            // alone scopes the source-gathering explore role, and its
            // catalog-visible surface (tool_search + deferred grep_files) is
            // pinned from the TUI side (scout_surface_keeps_tool_search_
            // grep_files_activation_path).
            assert!(leaf.permissions.allowed_tools.is_empty());
            assert_eq!(
                leaf.permissions.deny_all_tools,
                expected_role != "explore",
                "only the source-gathering explore role should receive tools"
            );
            assert!(
                leaf.prompt.contains(
                    "first non-empty line of your response must be exactly APPROVE or exactly BLOCK"
                ),
                "{expected_role} must declare the host-readable verdict contract"
            );
            assert!(
                leaf.prompt
                    .contains("Do not put any words before that verdict")
                    && leaf.prompt.contains("Here is the verdict"),
                "{expected_role} must reject verdict preambles that the host cannot parse"
            );
            if expected_role == "explore" {
                assert!(
                    leaf.prompt.contains("use `tool_search` with")
                        && leaf.prompt.contains("Use `grep_files` with")
                        && leaf.prompt.contains(
                            "the content-search tool is deferred, so this activation is required before it can be called"
                        ),
                    "the scout must activate the deferred content-search tool before searching with it"
                );
                assert!(
                    leaf.prompt
                        .contains("Read and search these files as needed"),
                    "discovery must be driven by missing evidence"
                );
                assert_eq!(
                    leaf.file_scope
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                    vec![
                        "fleets/stopship.toml",
                        "crates/cli/src/lib.rs",
                        "crates/workflow/src/role_resolve.rs",
                        "crates/engine/src/tools/workflow/mod.rs",
                        "crates/lane/src/runtime.rs",
                    ],
                    "the scout grep must not include its own authored prompt"
                );
                let repository_root =
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
                for path in &leaf.file_scope {
                    assert!(
                        repository_root.join(path).is_file(),
                        "scout evidence path must exist: {path}"
                    );
                }
                assert!(
                    leaf.prompt.contains(
                        "`include` set exactly to [`fleets/stopship.toml`, `crates/cli/src/lib.rs`, `crates/workflow/src/role_resolve.rs`, `crates/engine/src/tools/workflow/mod.rs`, `crates/lane/src/runtime.rs`]"
                    ) && leaf.prompt.contains("Matches outside that exact include list do not count"),
                    "the grep_files search must constrain the actual tool input, not only file scope metadata"
                );
                assert!(
                    leaf.prompt.contains("if you can populate all seven")
                        && leaf
                            .prompt
                            .contains("never return BLOCK after citing all seven")
                        && leaf
                            .prompt
                            .contains("identify each missing owner as MISSING"),
                    "the scout verdict must follow its own complete evidence artifact"
                );
            } else {
                assert!(
                    leaf.prompt.contains("Tools are intentionally unavailable")
                        && leaf.prompt.contains("promoted handoff")
                        && leaf.prompt.contains("all seven owners")
                        && leaf.prompt.contains("one concise row per owner"),
                    "{expected_role} must consume promoted evidence without reopening discovery"
                );
            }
            assert_eq!(
                leaf.file_scope
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                vec![
                    "fleets/stopship.toml",
                    "crates/cli/src/lib.rs",
                    "crates/workflow/src/role_resolve.rs",
                    "crates/engine/src/tools/workflow/mod.rs",
                    "crates/lane/src/runtime.rs",
                ],
                "every acceptance role must carry the same promoted evidence boundary"
            );
            assert_eq!(leaf.budget.max_steps, None, "{expected_role}");
            assert_eq!(leaf.budget.timeout_secs, None, "{expected_role}");
            assert_eq!(leaf.budget.max_tokens, None, "{expected_role}");
            assert!(!leaf.prompt.contains("at most"), "{expected_role}");
            assert!(
                !leaf.prompt.contains("reserve the verdict"),
                "{expected_role}"
            );
            assert!(
                leaf.profile.is_none(),
                "Fleet must resolve the declared role"
            );
        }
        let expected_gates = [
            ("explore", Some("implement"), "source_evidence"),
            ("implement", Some("reviewer"), "verification_plan"),
            ("reviewer", Some("test"), "review_report"),
            ("test", Some("release_lead"), "verification_report"),
            ("release_lead", None, "final_receipt"),
        ];
        assert_eq!(workflow.gates.len(), expected_gates.len());
        for (gate, (role, blocked_role, artifact_kind)) in workflow.gates.iter().zip(expected_gates)
        {
            assert_eq!(gate.role, role);
            assert_eq!(gate.on, GateOn::RoleComplete);
            assert_eq!(gate.on_fail, GateOnFail::Block);
            assert_eq!(gate.blocks_role.as_deref(), blocked_role);
            assert_eq!(gate.max_retries, 0);
            assert_eq!(gate.artifact_kind.as_deref(), Some(artifact_kind));
            assert!(
                gate.require_explicit_verdict,
                "{role} gate must fail closed when its verdict is missing or malformed"
            );
        }

        let mut board = LaneGateBoard::new("lane-fixture-contract");
        board.install_gates(&workflow.gates);
        assert_eq!(
            board
                .evaluate(&workflow.gates[0], GateOutcome::Pass)
                .expect("successful role promotes its gate"),
            GateState::Passed
        );
        let failure = board
            .evaluate(
                &workflow.gates[3],
                GateOutcome::Fail {
                    reason: "verifier receipt missing".to_string(),
                },
            )
            .expect("failed verifier updates its gate");
        assert!(matches!(failure, GateState::Blocked { .. }));
        assert!(
            board
                .role_is_blocked(&workflow.gates, "release_lead")
                .is_some()
        );
    }

    #[test]
    fn javascript_workflow_rejects_invalid_agent_profiles() {
        for bad in [r#""""#, r#""has space""#, r#""quote\"y""#, r#""a=b""#] {
            let source = format!(
                r#"
workflow({{
  "goal": "bad profile",
  "nodes": [
    {{ "agent": {{ "id": "scan", "prompt": "scan safely", "profile": {bad} }} }}
  ]
}});
"#
            );

            let err = compile_javascript_workflow("bad-profile.workflow.js", &source)
                .expect_err("invalid profile should be rejected");

            assert!(
                matches!(err, JavascriptWorkflowError::InvalidNode(_)),
                "profile {bad} should fail as an invalid node, got {err:?}"
            );
            assert!(err.to_string().contains("profile"));
        }
    }

    #[test]
    fn javascript_workflow_rejects_runtime_effects() {
        let source = r#"
import fs from "fs";
workflow({ "goal": "bad", "nodes": [] });
"#;

        let err = compile_javascript_workflow("bad.workflow.js", source)
            .expect_err("imports must be rejected");

        assert!(matches!(
            err,
            JavascriptWorkflowError::UnsupportedConstruct {
                construct: "import"
            }
        ));
    }

    #[test]
    fn javascript_workflow_rejects_unknown_result_reference() {
        let source = r#"
workflow({
  "goal": "bad dependency",
  "nodes": [
    {
      "agent": {
        "id": "scan",
        "prompt": "scan safely",
        "depends_on_results": ["missing"]
      }
    }
  ]
});
"#;

        let err = compile_javascript_workflow("bad-reference.workflow.js", source)
            .expect_err("validation must reject unknown result references");

        assert!(matches!(err, JavascriptWorkflowError::InvalidNode(_)));
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn javascript_example_compiles_to_validated_ir() {
        let source = include_str!("../../../workflows/issue_audit.workflow.js");
        let workflow =
            compile_javascript_workflow("issue_audit.workflow.js", source).expect("compile");

        workflow
            .validate_for_fleet()
            .expect("example lowers to IR that passes Fleet validation");
        assert_eq!(workflow.id.as_deref(), Some("issue-audit-js"));
        assert_eq!(workflow.nodes.len(), 2, "{:#?}", workflow.nodes);
        let WorkflowNode::BranchSet(branch) = &workflow.nodes[0] else {
            panic!("first node should be the parallel audit branch");
        };
        let leaf_ids: Vec<&str> = branch
            .children
            .iter()
            .map(|child| match child {
                WorkflowNode::Leaf(leaf) => leaf.id.as_str(),
                other => panic!("audit branch child should be an agent leaf: {other:?}"),
            })
            .collect();
        assert_eq!(leaf_ids, ["code-audit", "test-audit", "docs-audit"]);
        let WorkflowNode::Reduce(reduce) = &workflow.nodes[1] else {
            panic!("second node should reduce the audit findings");
        };
        assert_eq!(reduce.id, "synthesize-release-risk");
        assert_eq!(reduce.inputs, leaf_ids);
    }
}
