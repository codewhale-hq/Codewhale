# Maintainer / agent skills

GitHub-stewardship and release-QA workflows for maintaining Codewhale, codified as
`SKILL.md` skills (same format Claude Code and Codewhale both load). They encode the
issue-triage, PR-review, credit, and release-QA workflows the maintainers run each
release. Contributor PRs merge as themselves; harvesting is a fallback.

For end-user Skills Manager behavior (ownership, audit, import, trust), see
[../SKILLS.md](../SKILLS.md).

To activate:
- **Claude Code:** symlink or copy a skill dir into `.claude/skills/` (project) or your user skills dir.
- **Codewhale:** copy into a Codewhale-owned root (e.g. `~/.codewhale/skills/`), import via
  `/skills`, or bundle into `crates/skills/assets/skills/` + register in
  `crates/skills/src/system.rs` to ship it.

The maintainer loop, in order — each skill links the next:

| Stage | Skill | Use it for |
| --- | --- | --- |
| 1 | [cw-orient](cw-orient/SKILL.md) | Canonical-checkout, dirt and version truth before editing |
| 2 | [cw-slice](cw-slice/SKILL.md) | Find the existing owner, bound one slice, fix the evidence bar |
| 3 | [cw-gates](cw-gates/SKILL.md) | The focused-to-broad verification ladder and CI budget checks |
| 4 | [cw-dogfood](cw-dogfood/SKILL.md) | Stamped build, atomic install, real-product QA |
| 5 | [cw-land](cw-land/SKILL.md) | Commits, PRs, contributor credit, merging under a gate |
| 6 | [cw-handoff](cw-handoff/SKILL.md) | A paste-ready continuation grounded in live state |

GitHub stewardship and release: gh-file-issue, gh-assign-issues, gh-find-prs
(the read-only queue and coverage matrix), gh-close-issues,
codew-release-qa-sweep (a pointer to `docs/RELEASE_CHECKLIST.md`),
contributor-onboarding, feedback.
