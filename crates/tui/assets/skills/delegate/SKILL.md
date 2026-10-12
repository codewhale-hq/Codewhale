---
name: delegate
description: Strategic delegation for multi-step coding, research, or verification work. Use when a task can be split into parent reasoning plus focused sub-agent execution through the agent tool.
metadata:
  short-description: Delegate focused work to sub-agents
---

# Delegate

Use sub-agents when they can do focused work in parallel while the parent keeps architectural judgment, integration, and final verification.

## Keep vs Delegate

Keep in the parent:

- Understanding the user's actual request and constraints
- Architecture, security, product, and release-risk decisions
- Cross-module integration
- Final review, test interpretation, and user-facing summary

Delegate to sub-agents:

- Read-only exploration over a bounded file set
- Mechanical edits with a clear file ownership boundary
- Focused test or lint runs
- Boilerplate generation from an explicit spec
- Independent checks that can run while parent work continues

Do not delegate tiny one-step tasks, ambiguous product decisions, destructive operations without a clear acceptance criterion, or final verification.

## Launch Focused Work

Use `agent` for a focused child run. Launch independent children together so they can run in parallel.

Prefer provider-neutral `model_strength` over hardcoded model ids. Children inherit the active model by default (`model_strength: "same"`), including `type: "explore"`, so pass `model_strength: "faster"` explicitly to get the cheaper same-family sibling for read-only exploration:

```json
{
  "name": "config_audit",
  "prompt": "Inspect crates/tui/src/config.rs and crates/tui/src/settings.rs for duplicate model-default logic. Return file/line findings only; do not edit files.",
  "type": "explore",
  "model_strength": "faster",
  "cwd": "."
}
```

For code changes, give the child a precise write boundary and tell it not to revert unrelated edits. Keep implementation children capable with `model_strength: "same"`:

```json
{
  "name": "docs_patch",
  "prompt": "Update only docs/configuration.md to document the new [statusline] keys. Match the surrounding style. Do not edit other files.",
  "type": "implement",
  "model_strength": "same",
  "cwd": "."
}
```

Use `fork_context: true` only when the child genuinely needs the current conversation prefix. Leave it omitted for fresh, narrower context.

## Route the Work

Choose the route per child from `agent(action:"roster")` (no provider request), not from memory. It lists each role's resolved route and the models you may pick.

- `thinking` overrides a saved tier on any role. Dial: `off low medium high xhigh max ultra`, plus `inherit` (role default, then session effort) and `auto`.
- `model` and `model_strength` apply to unpinned routes only. A pinned profile refuses a conflicting model; to choose a model, start an unpinned role such as `general`. `model_strength: "faster"` is the same-family faster sibling, `"same"` the session model.
- Read the receipt, not your request: `child_route` shows the provider, model and route source, and `requested_reasoning` vs `effective_reasoning` shows what the route actually ran.
- Route by stakes. Mechanical work: the cheapest sufficient route with `thinking: "off"` or `"low"`. Implementation: a capable route at `medium` or above. Review and verification: a strong route at `high`, on a different route than the author when independence matters.

## Evaluate and Verify

Sub-agent outputs are self-reports. Re-check material claims before relying on them:

- Read changed files directly.
- Run the relevant tests locally.
- Inspect unexpected diffs before committing.
- Verify externally visible or destructive claims against source data.

## Prompt Shape

A good delegation prompt includes:

- The exact task
- Files or modules owned by the child
- Files or behavior the child must not touch
- Expected output format
- Acceptance criteria

Weak prompt:

```text
Fix the settings bug.
```

Strong prompt:

```text
Own only crates/tui/src/settings.rs and its tests. Preserve existing config key names. Add a regression test showing that provider-specific API key changes do not restart provider onboarding. Return the changed paths and test command output.
```
