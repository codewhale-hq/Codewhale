---
name: batch
description: Break a large, parallelizable goal into bounded work units, coordinate existing agent/worktree machinery, integrate, and verify. Explicit-only.
invocation: explicit-only
---

# Batch

## Invocation
Explicit-only. Do not start from ambient wording alone.

## When to use
Use for large parallelizable goals that need coordinated sub-work.

## Non-goals
- Do not invent a second orchestration engine; reuse delegate/fleet.
- Do not publish or deploy as part of batching.

## Workflow
1. Partition into bounded units.
2. Run units with existing agent/worktree tools.
3. Integrate results and verify.
