---
name: feedback
description: Report a Codewhale bug or idea as a GitHub issue. Use when: codewhale bug, feature request, feedback on codewhale, report an issue, or this should work differently.
invocation: model+user
---

# Feedback

## When to use
The user hit a Codewhale bug or wants Codewhale to do something new, and
wants it reported upstream.

## Setup
Requires the `gh` CLI, authenticated (`gh auth status`). Fail loud when it
is missing — fall back to printing the issue text for manual filing.

## Workflow
1. Reproduce or pin the behavior first: version (`codewhale --version`),
   command, expected vs actual, minimal steps.
2. Draft the issue (title, repro, expected, actual, version) and read it back.
3. File only with explicit approval:
   `gh issue create -R codewhale-hq/Codewhale --title "..." --body "..."`.
4. Report the issue URL back.

## Non-goals
- Do not file without showing the user the exact title and body first.
- Do not file duplicates — search open issues for the same report first.
- Do not include secrets, tokens, or private data in the issue body.
