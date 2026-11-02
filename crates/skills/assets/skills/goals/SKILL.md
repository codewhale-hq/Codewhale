---
name: goals
description: Set, review, and update the user's goals. Use when: goals, resolutions, track progress, what am I working toward, or holding me accountable.
invocation: model+user
---

# Goals

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
