---
name: plan
description: Turn a sufficiently understood task into an ordered implementation plan with dependencies and verification. Orchestrate Codewhale’s native plan state; do not build a parallel planner.
invocation: model+user
---

# Plan

## When to use
Use when the task is understood well enough to sequence work, but not yet a single obvious next edit.

## Non-goals
- Do not plan while key goals are still unknown — use interview first.
- Do not create a second planner outside Codewhale plan/Work state.

## Workflow
1. Restate the goal and constraints.
2. List ordered steps with dependencies.
3. Mark verification for each risky step.
4. Write the plan into Codewhale’s native plan/Work surface.
5. Wait for authorization before broad implementation.

## Output shape
- Goal
- Steps (ordered)
- Verification
- Risks / open questions
