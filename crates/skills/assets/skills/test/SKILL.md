---
name: test
description: Detect the project’s test stack, run the narrowest useful tests, create tests when authorized, and report coverage/gaps honestly.
invocation: model+user
---

# Test

## When to use
Use when the user wants tests run, added, or improved for a concrete surface.

## Non-goals
- Do not invent a test framework the repo does not use.
- Do not claim coverage you did not measure.

## Workflow
1. Detect the repo’s test runner and conventions.
2. Run the narrowest useful suite for the change.
3. Add or update tests only when authorized.
4. Report pass/fail with commands and residual gaps.
