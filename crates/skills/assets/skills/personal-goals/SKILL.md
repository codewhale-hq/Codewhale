---
name: personal-goals
description: The user's personal goals and resolutions in goals.md: set, review, track progress. Not the session goal (create_goal).
invocation: model+user
aliases-for: goals
---

# Personal goals

## When to use
Recording what the user is working toward, reviewing progress, and updating
goals as life changes.

## Setup
Goals live in `$CODEWHALE_HOME/goals.md`, one file the user owns. Create it
on first use; fail loud when the home directory is not writable.

## Workflow
1. Read the current file before discussing goals; quote, don't paraphrase,
   when reporting status.
2. Setting: one goal per line with a measurable target and a date. Confirm
   the wording with the user before writing.
3. Review: compare stated goals against what the user reports; update status
   lines, never rewrite history — append dated entries.
4. Nudge, don't nag: progress check-ins only when the user asks or scheduled.

## Non-goals
- Do not silently rewrite or delete goals.
- Do not invent goals the user never stated.
- Do not mix goals with task tracking — goals are outcomes, not todos.
