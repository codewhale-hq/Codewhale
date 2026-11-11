# Fleet + Workflow Tutorial

> 阅读简体中文版：[zh_hans/FLEET_WORKFLOW_TUTORIAL.md](zh_hans/FLEET_WORKFLOW_TUTORIAL.md)。

Fleet and Workflow are meant to work together, but they solve different parts
of the problem:

- **Fleet** configures and manages the same sub-agents: reusable roles, model
  routes, permissions, logs, artifacts, and status/restart/stop controls.
- **Workflow** describes orchestration: phases, branches, reducers, loops, and
  agent leaves that can dispatch through the fleet/sub-agent runtime.

**Default product path:** ask in natural language. Work handles small or
tightly coupled work directly under the active posture. Multi-step delegation
uses a compact Workflow plan with named steps, dependencies, bounded scopes,
and completion checks; results and evidence pass to the steps that need them.
One bounded, independent task can use a direct background agent. Reuse that
agent with `followup` for continued work. Background work keeps the composer
available, and ordinary multi-agent work does not require workflow files. Details:
[Automatic Workflows](AUTOMATIC_WORKFLOWS.md).

This tutorial covers the **manual** fleet task-spec / checked-in Workflow path
for operators who want durable host workers and reviewable specs. A
one-sentence request should still not silently generate `tasks.json`; worker
cards and permission posture make dispatch visible without exposing authoring
mechanics.

The examples use `codewhale fleet` and `/fleet`.
On-disk paths, config keys, and the Workflow `--fleet` flag use the Fleet name.

## 1. Prepare The Workspace

Run fleet from the workspace you want workers to inspect or modify:

```sh
codewhale fleet init
```

This creates the workspace ledger at `.codewhale/fleet.jsonl`. Worker logs and
bounded artifacts live under `.codewhale/fleet/`; host adapter logs live under
`.codewhale/fleet-host/`.

If you want named reusable workers, open the TUI and run:

```text
/fleet setup
```

Pick a role, choose whether that profile inherits the operator route or pins a
specific provider/model, choose where the profile lives (**This project** →
`.codewhale/agents/<role>.toml`, or **Personal** →
`$CODEWHALE_HOME/agents/<role>.toml`, available across repositories while a
same-id project profile remains the higher-priority override), then review the
exact file, permissions/tools/route posture, and save. The save control names
its effect ("Save to this project" / "Save as Personal profile"), and
replacing an existing file always asks for a second confirmation. fleet task
specs can reference either resolved profile with `worker.agent_profile` or the
shorter `worker.profile` alias.

This makes the fleet definition cross-repository, not the authority of one
running session. For a multi-repository operation, launch Codewhale from a
shared parent workspace. Profile availability does not grant filesystem access;
the session's workspace, explicit trusted paths, trust mode, and permission
posture remain authoritative.

## 2. Write A fleet Task Spec

`codewhale fleet run` accepts JSON or TOML. The checked-in
`docs/examples/fleet-dogfood.toml` file is the realistic manual smoke example;
the JSON below shows the same authoring shape with one read-only reviewer and
one bounded docs-note worker. The live Runtime policy controls secrets and
trust; fleet identity carries neither.

```json
{
  "name": "docs readiness check",
  "labels": {
    "kind": "tutorial"
  },
  "tasks": [
    {
      "id": "map-docs",
      "name": "Map current docs",
      "objective": "Find the docs that describe fleet and Workflow.",
      "instructions": "Read docs/FLEET.md and docs/WORKFLOW_AUTHORING.md. Report the command surfaces, current limitations, and any confusing gaps.",
      "worker": {
        "role": "reviewer",
        "tools": [
          "rg",
          "sed",
          "git"
        ]
      },
      "workspace": {
        "required_files": [
          "docs/FLEET.md",
          "docs/WORKFLOW_AUTHORING.md"
        ],
        "writable_paths": [],
        "environment": {
          "required": [],
          "allowlist": []
        }
      },
      "input_files": [
        "docs/FLEET.md",
        "docs/WORKFLOW_AUTHORING.md"
      ],
      "expected_artifacts": [
        "log",
        "report"
      ],
      "scorer": {
        "kind": "manual"
      },
      "retry_policy": {
        "max_attempts": 1
      }
    },
    {
      "id": "draft-gap-note",
      "name": "Draft gap note",
      "objective": "Draft a short local note for any missing tutorial steps.",
      "instructions": "Write a concise Markdown note with the missing fleet + Workflow tutorial steps. Do not edit public docs unless explicitly asked.",
      "worker": {
        "role": "builder",
        "tools": [
          "rg",
          "sed"
        ]
      },
      "workspace": {
        "required_files": [
          "docs/FLEET.md"
        ],
        "writable_paths": [
          ".codewhale/fleet"
        ],
        "environment": {
          "allowlist": []
        }
      },
      "expected_artifacts": [
        "log",
        "report"
      ],
      "scorer": {
        "kind": "manual"
      }
    }
  ]
}
```

Save it as `tasks.json`. These workers inherit the active provider and model;
connect a provider before validating their route. `--check` validates the
specification and route without creating or launching a run; it does not
prove that credentials or a remote model work.

Common task fields:

| Field | Purpose |
| --- | --- |
| `id`, `name` | Stable task identity and display name. |
| `objective`, `instructions` | The worker goal and exact operating instructions. |
| `worker.role` | Built-in or custom role intent, such as `reviewer`, `builder`, `read-only`, or `smoke-runner`. |
| `worker.profile` / `worker.agent_profile` | Saved fleet roster profile resolved from project `.codewhale/agents/`, personal `$CODEWHALE_HOME/agents/`, or `[fleet.profiles]`. |
| `worker.tools` | Tool names the task expects the worker to use. |
| `worker.model` | Preferred explicit model pin. Route resolution still owns provider/model validation. |
| `worker.model_class`, `worker.loadout` | Compatibility routing hints for older task specs; prefer `worker.profile` plus saved profile route pins for new specs. |
| `workspace.required_files` | Files that must exist before the task starts. |
| `workspace.writable_paths` | Paths the task is allowed to write when the effective runtime posture allows writing. |
| `workspace.environment` | Required or allowlisted environment variables, by name only. |
| `input_files`, `context` | Extra files and strings to thread into the task prompt. |
| `expected_artifacts` | Artifact kinds to expect: `log`, `report`, `patch`, `test_result`, `checkpoint`, or `receipt`. |
| `scorer` | Deterministic or manual verification rule. |
| `retry_policy`, `timeout_seconds`, `budget` | Retry and budget controls. |

Do not put `security_policy` or worker `trust_level` in a new fleet task spec.
Those legacy fields remain readable only for old ledger replay and new-run
validation rejects them. Project trust, filesystem/network reach, secrets,
approvals, sandboxing, and tool authority are Runtime policy inputs.

## 3. Start And Monitor fleet

Launch the run:

```sh
codewhale fleet run tasks.json --max-workers 4
```

The command prints the run id and worker ids. In another terminal, monitor the
ledgered state:

```sh
codewhale fleet status
codewhale fleet inspect <worker-id>
codewhale fleet logs <worker-id>
codewhale fleet artifacts <worker-id>
```

Use typed controls when a worker needs intervention:

```sh
codewhale fleet interrupt <worker-id>
codewhale fleet restart <worker-id>
codewhale fleet resume <run-id>
codewhale fleet stop --all
```

`resume` is for restart recovery after a manager exit, laptop sleep, or stale
lease. It replays the ledger and reconciles stale work without creating a new
run.

## 4. Author A Workflow

Workflow source is declarative JavaScript or TypeScript that lowers to typed
Rust `WorkflowSpec`. It is not a general JavaScript runtime: imports, process
access, filesystem reads/writes, network calls, `eval`, `async`, and `await`
are rejected.

Create a checked-in file such as `workflows/docs_readiness.workflow.js`. The
repo also includes `workflows/issue_audit.workflow.js` as a maintained example.

```js
export default workflow({
  "id": "docs-readiness",
  "goal": "Inspect fleet and Workflow docs, then synthesize a readiness note",
  "nodes": [
    {
      "branch": {
        "id": "parallel-docs-audit",
        "parallel": true,
        "children": [
          {
            "agent": {
              "id": "fleet-docs",
              "prompt": "Inspect docs/FLEET.md for command and task-spec coverage.",
              "agent_type": "review",
              "mode": "read_only",
              "profile": "reviewer",
              "file_scope": ["docs/FLEET.md"]
            }
          },
          {
            "agent": {
              "id": "workflow-docs",
              "prompt": "Inspect docs/WORKFLOW_AUTHORING.md for Workflow authoring coverage.",
              "agent_type": "review",
              "mode": "read_only",
              "profile": "reviewer",
              "file_scope": ["docs/WORKFLOW_AUTHORING.md"]
            }
          }
        ]
      }
    },
    {
      "reduce": {
        "id": "readiness-summary",
        "inputs": ["fleet-docs", "workflow-docs"],
        "prompt": "Summarize the exact docs gaps and the safest next edit."
      }
    }
  ]
});
```

Current Workflow node wrappers are `agent`, `branch`, `sequence`, `reduce`,
`teacher_review`, `loop_until`, `cond`, and `expand`. `agent.profile` names a
fleet roster profile; explicit agent fields override profile defaults.

The model-facing `workflow` tool can start, run, inspect, or cancel a workflow
from inline source or a `source_path`. When Codewhale uses this path, ask it to
show the plan first if the workflow will launch multiple workers or touch files.

## 5. Natural Language Intake

A good prompt today is:

```text
Draft a fleet task spec for this goal, but do not run it yet.
Show the proposed tasks, worker profiles, writable paths, expected artifacts,
scorers, and security policy. Keep secrets disabled unless I explicitly grant
them.
```

After reviewing the generated spec, save it as `tasks.json` and run the fleet
commands above. For workflows, ask Codewhale to draft a `.workflow.js` file,
show the plan, and use the workflow tool path only after approval.

This review step is intentional. It keeps provider routing, DeepSeek or other
model support, writable paths, network access, and secret use explicit before
durable workers start.
