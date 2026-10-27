export default workflow({
  "id": "issue-audit-js",
  "goal": "Audit an issue fix with parallel specialist agents, then synthesize a release note",
  "description": "Declarative JavaScript authoring example lowered to typed Workflow IR without executing JS.",
  "nodes": [
    {
      "branch": {
        "id": "parallel-audit",
        "parallel": true,
        "children": [
          {
            "agent": {
              "id": "code-audit",
              "prompt": "Inspect the implementation for correctness and regression risk.",
              "agent_type": "review",
              "mode": "read_only",
              "file_scope": ["crates"]
            }
          },
          {
            "agent": {
              "id": "test-audit",
              "prompt": "Inspect targeted tests and identify missing verification.",
              "agent_type": "verifier",
              "mode": "read_only",
              "file_scope": ["crates", "tests"]
            }
          },
          {
            "agent": {
              "id": "docs-audit",
              "prompt": "Check whether docs or release notes should mention the change.",
              "agent_type": "review",
              "mode": "read_only",
              "file_scope": ["docs"]
            }
          }
        ]
      }
    },
    {
      "reduce": {
        "id": "synthesize-release-risk",
        "inputs": ["code-audit", "test-audit", "docs-audit"],
        "prompt": "Combine specialist findings into a release-ready risk summary."
      }
    }
  ]
});
