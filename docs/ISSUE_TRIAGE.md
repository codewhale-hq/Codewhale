# Issue Triage

Triage aims issues at the agent-ready standard: the
[Agent task](../.github/ISSUE_TEMPLATE/agent-task.yml) issue form, whose
bodies a fresh agent can execute without extra context. This page covers the
stale/`needs-info` lifecycle that runs alongside that standard.

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
