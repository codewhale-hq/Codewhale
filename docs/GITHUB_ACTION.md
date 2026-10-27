# GitHub PR reviews

The root `action.yml` runs the existing `codewhale review` command with an
exact, checksummed release. It needs Node.js 22+, `gh`, Git, and a macOS or
Linux runner. It fetches Git objects without checking out or executing PR code.
The Engine owns review coverage, prompts, routing and finding generation.

Use two jobs: analysis with a read-only GitHub token and the selected provider
key, followed by publication with a write token and no provider key. Analysis
exports a validated findings artifact. The publisher validates its repository,
PR, workflow run and attempt, and current base/head before submitting a
`COMMENT` review. It never approves, requests changes, commits, or runs a model.

Review mode only. Autonomous mention/fix mode is not implemented in this
Action. The original direct-review Action has been in release tags since
`v0.10.1`; the findings output and separate publisher require a commit containing
both features. The Action ref and the exact released CLI `version` are separate
pins. Adding these source files does not enable a workflow.

## Account configuration

Codewhale-hosted GitHub reviews use the provider and model configured in the
Codewhale account, with any explicit saved repository override. OrcaRouter is
an optional provider connection, not a review default. Missing usable account
configuration requires setup; it does not select another provider.

This standalone Action has explicit provider/model inputs and no default
provider. It does not fetch the hosted GitHub App settings automatically. Use
your own provider key, shown next; the account machine-key route is described
below.

## Use your own provider key

Pick `orcarouter`, `deepseek`, `anthropic`, `openrouter`, `zai`, or `modelstudio-token-plan`,
an exact model ID, and a released CLI tag. Store the matching provider key as
an Actions secret. Pass only that provider's key to the analysis job. The
Action never chooses a different credential after an authentication or payment
failure. The example illustrates an explicitly selected OrcaRouter connection;
it does not set the Codewhale account default. Replace its provider and secret
together when choosing another route.

Create or connect your own [OrcaRouter account](https://www.orcarouter.ai/ref/ref_df843907ad10d887b867),
then store its API key as `ORCAROUTER_API_KEY`. Codewhale earns a referral fee
from OrcaRouter. `orcarouter/free` is an available starting model when listed
for your workspace; it still requires an API key. A direct no-key request on
2026-10-26 returned HTTP 401. OrcaRouter's [free-route documentation](https://www.orcarouter.ai/models/orcarouter/free)
describes zero-priced usage with request-rate limits. A limit or authentication
failure does not authorize a paid fallback. GitHub runner compute has its own
cost and limits.

Set `CODEWHALE_REVIEW_VERSION` to an exact `vX.Y.Z` release and
`CODEWHALE_REVIEW_MODEL` to the chosen provider's exact model ID (for example
`orcarouter/free`). Replace both
`REPLACE_WITH_REVIEWED_COMMIT_SHA` placeholders with the same reviewed Action
commit containing the publisher. Pin third-party actions to reviewed commits
under your repository's Actions policy.

```yaml
name: Codewhale review
on:
  pull_request:
    types: [opened, synchronize, reopened, ready_for_review]
  workflow_dispatch:
    inputs:
      pr-number:
        description: Same-repository PR number
        required: true
        type: string
permissions: {}
concurrency:
  group: codewhale-review-${{ github.event.pull_request.number || inputs.pr-number }}
  cancel-in-progress: false
jobs:
  review:
    runs-on: ubuntu-latest
    timeout-minutes: 25
    permissions:
      contents: read
      pull-requests: read
    outputs:
      findings: ${{ steps.review.outputs.findings }}
    steps:
      - uses: actions/setup-node@v7
        with:
          node-version: '22'
      - uses: codewhale-hq/CodeWhale@REPLACE_WITH_REVIEWED_COMMIT_SHA
        id: review
        with:
          version: ${{ vars.CODEWHALE_REVIEW_VERSION }}
          provider: orcarouter
          model: ${{ vars.CODEWHALE_REVIEW_MODEL }}
          pr-number: ${{ inputs.pr-number }}
          post: 'false'
        env:
          ORCAROUTER_API_KEY: ${{ (github.event_name == 'workflow_dispatch' || github.event.pull_request.head.repo.full_name == github.repository) && secrets.ORCAROUTER_API_KEY || '' }}
      - uses: actions/upload-artifact@v7
        if: steps.review.outputs.findings != ''
        with:
          name: codewhale-findings-${{ github.run_id }}-${{ github.run_attempt }}
          path: ${{ steps.review.outputs.findings }}
          retention-days: 1
      - uses: actions/upload-artifact@v7
        if: always() && steps.review.outputs.receipt != ''
        with:
          name: codewhale-review-receipt-${{ github.run_id }}-${{ github.run_attempt }}
          path: ${{ steps.review.outputs.receipt }}
          retention-days: 14
  publish:
    needs: review
    if: needs.review.outputs.findings != ''
    runs-on: ubuntu-latest
    permissions:
      contents: read
      pull-requests: write
    steps:
      - uses: actions/setup-node@v7
        with:
          node-version: '22'
      - uses: actions/download-artifact@v7
        with:
          name: codewhale-findings-${{ github.run_id }}-${{ github.run_attempt }}
          path: findings
      - uses: codewhale-hq/CodeWhale/scripts/github-action/publish@REPLACE_WITH_REVIEWED_COMMIT_SHA
        id: publish
        with:
          findings: findings/findings.json
          pr-number: ${{ inputs.pr-number }}
      - uses: actions/upload-artifact@v7
        if: always() && steps.publish.outputs.receipt != ''
        with:
          name: codewhale-publication-receipt-${{ github.run_id }}-${{ github.run_attempt }}
          path: ${{ steps.publish.outputs.receipt }}
          retention-days: 14
```

No repository checkout is needed in either job. The publishing job must use
trusted Action code and must not run files from the findings artifact. Keep it
on a separate ephemeral runner; a later step on the analysis runner is not the
same credential boundary. The analysis CLI still receives a read-only GitHub
token for PR metadata and Git objects, alongside its selected provider key.

Never use `pull_request_target` with these actions. Fork PRs and drafts are
ineligible even on a manual run. A skipped event is not a clean review. This
publisher accepts artifacts only from the current workflow run and attempt;
cross-workflow `workflow_run` publication is not implemented.

To publish as a dedicated GitHub App, create an installation token only in the
publisher job, pass it as `github-token`, and set `reviewer-login` to that App's
exact bot login, such as `codewhale-agent[bot]`. The publisher verifies the
returned review author; a mismatch is an uncertain publication, not permission
to retry. See [App identity setup](GITHUB_APP.md).

### Account machine keys

The `codewhale` provider uses `CODEWHALE_API_KEY` and an exact `provider/model`
account-catalog entry. Account sign-up availability does not establish that
machine-key relay access is qualified for Actions. Use explicit BYOK unless
your account's machine-key route has been separately verified. The Action runs
`codewhale account agent` before using that route.

## Bounds and evidence

| Analysis input | Default | Meaning |
| --- | --- | --- |
| `version` | Required | Exact released CLI tag, never `latest` |
| `provider` | Required | Explicit BYOK provider, or `codewhale` for a verified account machine key |
| `model` | Required | Exact model ID; account mode uses `provider/model` |
| `max-chars` | 200000 | Complete diff characters per pass, maximum 8388608 |
| `max-passes` | 1 | Complete ordered passes, maximum 64 |
| `max-output-tokens` | CLI automatic | Optional per-pass ceiling, 8192–1000000 |
| `timeout-seconds` | 600 | Model/publication deadline, 30–1200 |
| `post` | `true` | Legacy direct publication; set `false` for the two-job setup |

These bounds do not guarantee a dollar cost. Model prices and reasoning
accounting vary. Raising the pass count authorizes more model requests.
Inference is never automatically retried. Set the intended review scope and
provider spend controls before enabling a workflow.

Analysis outputs are `outcome`, `receipt`, `findings`, and `pr-url`. The
sanitized receipt records revision, route, bounds, completion counts,
publication state, and reported token usage. It excludes raw model/provider
output, PR text and credentials. Missing token accounting stays missing.
A runner shutdown before receipt creation is not a completed review.

`findings` is populated only for complete `post:false` reviews. It contains
validated structured review text, evidence, file locations and exact revision
and workflow identity. Unknown fields, raw model output, provider errors and
replacement code are excluded; known supplied credentials are redacted. It
can still contain private source excerpts or other repository-sensitive text.
Store it only with the repository's intended artifact access and retention.

The publisher reads the complete current PR file list and existing reviews,
then rechecks the base/head. It anchors inline findings only where GitHub
supplies a complete valid patch containing their line. Pre-existing,
unanchorable and suspected findings remain in the summary. Suggestions are
prose only, with no one-click replacement blocks. Up to 50 findings are also
posted inline; every retained finding stays in the summary. Oversized review
bodies fail before any write instead of silently dropping findings.

The publisher recognizes its own review/finding fingerprints only on reviews
by `reviewer-login`. Identical review reruns are skipped. Findings with matching
normalized title/description, severity, location and unchanged file content are not repeated,
including after unrelated files change. Changed file contents can produce a
fresh finding. This is deterministic deduplication, not semantic similarity
judgment; differently worded findings can still recur. Concurrency must remain
serialized per PR because GitHub does not offer an atomic idempotency key for
review creation.

The CLI validates complete diff coverage. It reads bounded excerpts from
pinned Git blobs; it does not execute PR tests or inspect arbitrary unchanged
callers. “Reviewed clean” means zero issues were reported in a completed
review, not proof that the PR contains no bugs.

## Outcomes and recovery

| Outcome | Meaning / next action |
| --- | --- |
| `reviewed_clean` | Complete analysis with zero reported issues |
| `reviewed_with_findings` | Complete analysis; findings remain advisory |
| `published` | Publisher confirmed a `COMMENT` review at the captured head |
| `already_published` | Matching review or findings already exist under the configured publisher |
| `configuration_missing` | Check release/model, keys, bounds, artifact and workflow identity |
| `failed` | No complete analysis/publication receipt; inspect the safe reason code |
| `incomplete` | Coverage incomplete; revise scope or limits |
| `publication_uncertain` | Check the PR before retrying; the write may have succeeded |
| `superseded` | Base or head changed; analyze the current revision |
| `not_eligible` | Fork, draft, closed PR or unsupported event; no model run/write |

Failures fail the optional job; whether it is a required merge check is the
repository owner's policy. A provider failure is not a negative verdict about
the PR. No additional failure comment is posted.

After repairing setup, run the entire workflow with a PR number. Rerunning
only publication in a new run attempt rejects the old artifact; this prevents
silently publishing a bundle from another execution. Enabling a disabled
workflow does not replay old events. Legacy `post:true` preserves the CLI's
direct publication behavior and does not receive the separate publisher's
cross-run deduplication or write-token isolation.
