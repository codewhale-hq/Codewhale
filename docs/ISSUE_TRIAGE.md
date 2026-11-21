# Issue Triage

Triage decides which issues are startable and how a behaviour change is
recorded. The [Agent task](../.github/ISSUE_TEMPLATE/agent-task.yml) form
describes the shape an executable issue should have, but it no longer marks
itself executable: `agent-ready` is a maintainer grant. This page covers that
gate, the regression standard, the closing standard, and the `needs-info`
lifecycle that runs alongside them.

## Startability

An issue is startable by an autonomous agent only when a maintainer has added
`agent-ready`. The Agent task form does not apply the label, so a reporter
cannot pre-authorise execution. Absence means the issue waits for triage: do
not implement it or dispatch a worker against it.

`agent-in-progress` and `needs-human` describe the state after work starts.
`autonomous-ready` remains the stronger autonomous-loop grant; whether to reuse
it, a milestone, or a maintainer comment is the maintainer's choice.

## Regressions

A behaviour change is a **regression** when all three hold:

1. the behaviour was documented — a `docs/features.toml` row, a `docs/*.md`
   statement, or a guard test;
2. it changed or disappeared; and
3. the commit did not say so.

Otherwise it is a normal change. Record a regression with the existing scale,
not a new label:

- `priority:p1` at minimum;
- `release-blocker` at a maintainer's decision;
- `priority:p0` only when data loss, security, or the release path is involved.

`release-blocker` is protected from stale cleanup, so marking one also keeps
the issue out of automated closure. Every regression should gain a positive
guard in `crates/engine/tests/behavioural_guards.rs` when the behaviour is
expressible there, so a later rename cannot invert it silently.

The protocol is forward-looking: it does not re-tag history. The skills-index
case (#7432) is closed by restoring the parent-prompt behaviour or by
correcting the docs to state that the parent renders no index; either is
acceptable, leaving the docs wrong is not. That choice belongs to a maintainer.

## Closing an issue

An issue whose body asks for a user-observable outcome may be closed when that
outcome is demonstrable — a measurement, a config showing the new value in
effect, or a transcript. "The implementation is in source" is not a closing
standard. If the requested outcome is not observable yet, leave the issue open
and say what remains.

## Stale `needs-info` cleanup

The stale workflow only acts on issues that a maintainer has explicitly labeled
`needs-info`. This keeps old roadmap, release, security, and current milestone
work out of automatic cleanup unless a maintainer first marks the issue as
waiting on reporter input.

Required labels:

- `needs-info`: waiting on reporter information or current-version reproduction details.
- `stale`: inactive `needs-info` issue pending automatic closure.
- `keep-open`: protected because maintainers intentionally keep it open.
- `pinned`: protected maintainer issue.

Protected labels for stale cleanup:

- `pinned`
- `keep-open`
- `release-blocker`
- `security`

A `bug` issue is not protected just because it is a bug. If a maintainer has
also labeled it `needs-info`, it is eligible for stale warning and closure
unless one of the protected labels above is present.

## Dry-run queries

Run these before changing stale policy or doing a manual cleanup pass:

```sh
STALE_CUTOFF=$(python3 -c 'from datetime import date, timedelta; print(date.today() - timedelta(days=45))')
NEEDS_INFO_CUTOFF=$(python3 -c 'from datetime import date, timedelta; print(date.today() - timedelta(days=30))')

gh issue list --repo codewhale-hq/CodeWhale --state open \
  --search "updated:<${STALE_CUTOFF}" \
  --limit 100 \
  --json number,title,updatedAt,labels,url

gh issue list --repo codewhale-hq/CodeWhale --state open \
  --search "label:needs-info updated:<${NEEDS_INFO_CUTOFF}" \
  --limit 100 \
  --json number,title,updatedAt,labels,url

gh issue list --repo codewhale-hq/CodeWhale --state open \
  --search "created:<${STALE_CUTOFF} comments:0 -label:keep-open -label:release-blocker -label:security" \
  --limit 100 \
  --json number,title,createdAt,updatedAt,labels,url
```

Use `updatedAt`, labels, and current release relevance as the closure basis.
Creation date alone is too aggressive.

## First cleanup pass

Before relying on automation, perform one manual pass:

- Label unresolved old bug reports as `needs-info` only after asking for
  current-version reproduction details.
- Close obvious GUI, VS Code, and web UI duplicates with links to canonical
  desktop/runtime issues.
- Close old brand-discussion issues as superseded when the Codewhale rebrand
  and README/history work already covers them.
- Protect intentional v0.9.0 roadmap shards with `keep-open` or close them as
  superseded by a canonical epic.

Do not close release blockers, security issues, or active milestone work from
stale automation alone.
