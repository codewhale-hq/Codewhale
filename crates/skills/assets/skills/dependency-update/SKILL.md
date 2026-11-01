---
name: dependency-update
description: Read release notes/changelogs, update a defined dependency scope, handle breaking changes, and verify. Explicit-only; no broad update-everything by inference.
invocation: explicit-only
---

# Dependency Update

## Invocation
Explicit-only with a defined dependency scope.

## Non-goals
- No inferred “update everything”.
- No publish/release.

## Workflow
1. Identify the dependency set and current versions.
2. Read release notes/changelogs.
3. Apply updates and fix breakages.
4. Verify with the project’s test/build gates.
