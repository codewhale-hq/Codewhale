---
name: github
description: GitHub via gh — issues, PRs, reviews, search, and CI runs. Use when: user mentions a repo, issue, PR, review, release, workflow run, or anything on github.com.
invocation: model+user
---

# GitHub

## When to use
Any GitHub work: read or triage issues, open or review PRs, search code,
check CI runs, manage releases.

## Setup
Requires the `gh` CLI, authenticated. Fail loud when it is missing:

```
gh auth status || gh auth login
```

Never proceed with `curl` against api.github.com as a silent fallback —
`gh` owns auth, pagination, and output shape.

## Workflow
1. Confirm the repo (`owner/name`) and, for mutations, the exact target.
2. Read: `gh issue view`, `gh pr view`, `gh pr diff`, `gh search`, `gh run view`.
3. Mutate only when asked: `gh issue create`, `gh pr create`, `gh pr review`,
   `gh pr merge`. Mutations need explicit user intent, not inference.
4. Quote what `gh` returned; never summarize a diff or check rollup you did not read.

## Non-goals
- Do not merge, close, or approve on your own initiative.
- Do not invent issue/PR numbers or check conclusions.
- Do not push, retag, or rewrite history without explicit authorization.
