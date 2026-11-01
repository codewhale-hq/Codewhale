---
name: skill-creator
description: Create or improve codewhale skills. Use when the user wants a new skill, wants to update an existing skill, or needs guidance on when a skill should be a skill versus MCP, hooks, tools, or a plugin scaffold.
metadata:
  short-description: Create Codewhale skills
aliases-for: create-skill
---

# Skill Creator

Use this skill to create small, useful codewhale skills that match the
runtime this repository actually ships.

## What A Skill Is

A skill is a local folder with a `SKILL.md` file. Codewhale reads the skill name
and description during discovery, then loads the body only when the user or task
matches the skill.

Discovery paths, in precedence order:

- `<workspace>/.agents/skills`
- `<workspace>/skills`
- `<workspace>/.opencode/skills`
- `<workspace>/.claude/skills`
- `<workspace>/.cursor/skills`
- `<workspace>/.codewhale/skills`
- `~/.agents/skills`
- `~/.claude/skills`
- `~/.codewhale/skills`

Use skills for model instructions, workflows, and lightweight conventions. Use
MCP for live external APIs or durable tools. Use hooks for automatic local
events. To distribute a skill through a plugin bundle, give the bundle a
`plugin.json` whose `net.codewhale` extension points `skills.path` at a
directory holding `<name>/SKILL.md` folders; each skill then loads namespaced
as `<plugin>:<skill>` after review, trust, and enablement. A bare `SKILL.md`
directory is discoverable on the skills paths above but is not a plugin
bundle and cannot be installed through `/plugin install`.

## Minimum Shape

```text
my-skill/
`-- SKILL.md
```

```markdown
---
name: my-skill
description: Use when Codewhale should follow this specific workflow.
---

# My Skill

Instructions for the agent.
```

Frontmatter parsing is intentionally simple. Keep `name` and `description` as
plain single-line values. Use lower-case hyphen-case names.

## Writing Rules

- Make the `description` action-oriented and trigger-specific. It is the main
  signal Codewhale sees before loading the body.
- Keep the body operational. Include what to do, what to avoid, and how to
  verify the result.
- Do not include general programming advice, marketing copy, or long background
  material.
- Move bulky details to `references/` and mention exactly when to open them.
- Add `scripts/` only for deterministic helpers that are worth maintaining.
- Add `assets/` only for templates, fixtures, examples, or files reused by the
  workflow.
- Do not assume scripts are safe to run. Community skill scripts require user
  intent and trust review.

## Creation Workflow

1. Define the skill boundary in one sentence.
2. Decide whether a skill is the right surface:
   - Instructional workflow: skill
   - External service/API: MCP server plus an optional skill
   - Repeated shell helper: local tool or script plus an optional skill
   - Packaging multiple pieces: plugin scaffold plus skill/MCP activation notes
3. Create `<skill-name>/SKILL.md`.
4. Write frontmatter with `name` and `description`.
5. Write a concise body with:
   - trigger and scope
   - required inputs or assumptions
   - step-by-step workflow
   - validation checks
   - safety notes
6. Add companion files only when they reduce real complexity.
7. Validate by loading the skill through `/skills` or by running the relevant
   skill discovery tests if editing this repository.

## Updating Existing Skills

- Preserve the user's local intent. Avoid replacing a working skill wholesale
  unless the user asked for a rewrite.
- Tighten descriptions when the skill is under-triggering or over-triggering.
- Remove stale tool names, unavailable dependencies, and copied instructions
  from other agents that do not apply to codewhale.
- Keep examples short and directly tied to this runtime's commands and tools.

## Validation Checklist

- `SKILL.md` starts with `---`.
- `name` matches the directory name unless there is a deliberate reason.
- `description` says when to use the skill, not just what it is.
- The body references only tools, commands, and paths that exist or are clearly
  optional.
- Any scripts or external-service steps explain credential and trust handling.
