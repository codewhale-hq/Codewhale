---
name: webapp-testing
description: Start/reuse a local app, wait for readiness, inspect rendered state/console/network, act from observed selectors, and verify with evidence.
invocation: model+user
---

# Webapp Testing

## When to use
Use to prove a web app flow works in a real browser/runtime context.

## Non-goals
- Do not claim success from unit tests alone.
- Do not hardcode fragile selectors without observation.

## Workflow
1. Start or reuse the local app.
2. Wait for readiness.
3. Inspect DOM/console/network.
4. Drive the flow from observed state.
5. Record evidence of pass/fail.
