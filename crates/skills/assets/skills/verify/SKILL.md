---
name: verify
description: Exercise the real app/API/CLI and collect observable evidence; tests alone do not count as end-to-end verification.
invocation: model+user
aliases-for: check-work
---

# Verify

## When to use
Use when the user wants proof the real surface works, not only unit tests.

## Non-goals
- Unit tests alone are not end-to-end verification.
- Do not fake success without observable evidence.

## Workflow
1. Identify the real entrypoint (CLI binary, API, UI).
2. Exercise it with realistic inputs.
3. Capture outputs, exit codes, logs, or screenshots.
4. Report evidence and remaining uncertainty.
