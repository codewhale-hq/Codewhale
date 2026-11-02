---
name: release
description: Prepare a named version: preflight, version consistency, build/package, smoke test, checksums/notes, and release readiness. Publishing/tagging/deploy need separate authorization. Explicit-only.
invocation: explicit-only
---

# Release

## Invocation
Explicit-only for a named version. Loading this skill is not publish authority.

## Non-goals
- Do not tag, publish, deploy, or ship packages without separate authorization.
- Do not invent version numbers.

## Workflow
1. Preflight version consistency and changelog readiness.
2. Build/package with locked release commands.
3. Smoke test artifacts.
4. Produce checksums/notes and a readiness report.
5. Stop before any public release action unless explicitly authorized.
