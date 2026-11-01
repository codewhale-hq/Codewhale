---
name: debug
description: Reproduce, minimize, localize, identify root cause, and distinguish diagnosis from an authorized fix. Prefer root-cause over symptom patches.
invocation: model+user
---

# Debug

## When to use
Use for failures, flakes, wrong outputs, crashes, and regressions.

## Non-goals
- Do not jump to a fix before reproduction/localization when the bug is unclear.
- Do not treat “run all tests” as debugging by default.

## Workflow
1. Reproduce or gather the strongest available failure signal.
2. Minimize the case.
3. Localize to component/file/line.
4. State root cause vs symptoms.
5. Fix only when authorized; otherwise hand back a diagnosis with evidence.

## Output shape
- Symptom
- Reproduction
- Localization
- Root cause
- Fix status / next step
