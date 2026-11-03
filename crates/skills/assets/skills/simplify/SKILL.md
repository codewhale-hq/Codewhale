---
name: simplify
description: Improve clarity and reduce needless complexity after behavior is understood; preserve behavior and keep cleanup separate from correctness fixes.
invocation: model+user
---

# Simplify

## When to use
Use after behavior is understood and the user wants clarity or less complexity.

## Non-goals
- Do not mix cleanup with speculative refactors that change behavior.
- Do not “simplify” by deleting necessary safety checks.

## Workflow
1. Confirm current behavior with tests or reproduction.
2. Reduce complexity in small, reviewable steps.
3. Keep behavior-preserving verification green.
