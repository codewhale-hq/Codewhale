# GitHub review bot

The root `action.yml` runs the existing `codewhale review` command with an
exact, checksummed release. It needs Node.js 22+, `gh`, Git, and a macOS or
Linux runner. It fetches Git objects without checking out or executing PR code.
The Engine owns review coverage, prompts, routing and finding generation.

Use two jobs: analysis with a read-only GitHub token and the selected provider
key, followed by publication with a write token and no provider key. Analysis
exports a validated findings artifact. The publisher validates its repository,
PR, workflow run and attempt, and current base/head before submitting a
`COMMENT` review. It never approves, requests changes, commits, or runs a model.

Automatic reviews and `@codewhale review` comments use the same review path.
The bot posts an advisory summary and validated inline findings. It never
approves or requests changes. Coding tasks that push a branch or open a PR
remain part of the hosted agent; this Action handles code review.

## Set up your own bot

You need a GitHub repository with Actions enabled and one model API key.
Node.js 22+ and Git are needed only to generate the workflow locally; GitHub's
Ubuntu runner provides the review tools. No server or GitHub App is required.

1. From a clean Codewhale source checkout, generate a workflow in your project:

   ```sh
   node /path/to/Codewhale/scripts/github-action/setup.mjs \
     --directory /path/to/your-project \
     --provider deepseek --model deepseek-flash
   ```

   Select your own provider and exact model ID. The generator pins both
   Codewhale actions to the source checkout's full commit and the CLI to
   `v0.10.2`, with a 16384-token output allowance for the completed review.
   It refuses to overwrite an existing workflow. Use `--print` to
   inspect the YAML, `--version vX.Y.Z` to choose another released CLI, or
   `--action-ref FULL_COMMIT_SHA` with a separately downloaded `setup.mjs`.
   `--max-output-tokens N` changes the explicit output allowance. Reasoning
   models can exhaust smaller automatic allowances before writing findings.
   Generated workflows use `reasoning-effort: off`: the released v0.10.2
   binary has an internal 4000-token cap for small reviews that can be consumed
   entirely by hidden reasoning. `--reasoning-effort auto` restores the
   Engine default; other explicit effort levels are also available. The Action
   writes only this non-secret setting into its isolated run configuration.
   Increasing the Action allowance does not override a released CLI's smaller
   internal cap. A completed review is still required before publication.
   The generated workflow is `.github/workflows/codewhale-review.yml`.

2. Store only the selected provider's API key as a repository Actions secret:

   ```sh
   gh secret set DEEPSEEK_API_KEY --repo OWNER/REPO
   ```

   This command prompts for the key. DeepSeek is the example; the provider
   table below lists the other supported secrets. Never put a key in YAML.

3. Commit the workflow to your repository's default branch and push it.
   Same-repository PRs are reviewed automatically when opened or updated.
   For any open, non-draft PR, a maintainer can add exactly this comment,
   or choose **Actions → Codewhale PR Review → Run workflow**
   and enter its number:

   ```text
   @codewhale review
   ```

The generated workflow uses `github-actions[bot]`. To use your own existing
App, add `--app-login 'YOUR-APP[bot]'` when generating, set the
`CODEWHALE_APP_ID` variable and `CODEWHALE_APP_PRIVATE_KEY` secret, and install
the App on the repository. App workflows verify installation access in a
separate job before making a model request. The publisher requests only contents read and pull
requests write. A missing App configuration fails; it does not switch identity.
See [GitHub App setup](GITHUB_APP.md) for creating a new App.

`--trigger-phrase @my-bot` changes the mention to `@my-bot review`. A mention
is an exact standalone comment; quoted text, arbitrary instructions, issue-only
comments, edited comments and bot authors do not trigger inference. New PR
review comments also work for same-repository PRs. Both the original actor
and an actor rerunning the workflow must still have write, maintain or admin
access when GitHub is queried. Association labels alone grant no access.
A deleted, edited or mismatched triggering comment is rejected.

Fork PRs do not receive automatic model reviews. The generated workflow opts
into reviewing their inert diffs only through an authorized issue comment or
manual run from the default branch. It reads pinned Git objects without
checking out PR files, reading project configuration or executing candidate
code. The provider key belongs to that trusted job; no fork workflow receives
it. Leave `allow-forks: false` on both actions to disable this opt-in.

The Action ref and exact CLI release are separate pins. Existing release tags
contain the original review action; mention support and the generator require
a commit containing these features. A moving major tag is not published by
this setup. Source installation does not prove a live provider review.

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

Choose a provider and exact model, then pass only its key to the analysis job:

| Provider | Actions secret |
| --- | --- |
| `deepseek` | `DEEPSEEK_API_KEY` |
| `anthropic` | `ANTHROPIC_API_KEY` |
| `openrouter` | `OPENROUTER_API_KEY` |
| `orcarouter` | `ORCAROUTER_API_KEY` |
| `zai` | `ZAI_API_KEY` |
| `modelstudio-token-plan` | `MODELSTUDIO_API_KEY` |
| `codewhale` | `CODEWHALE_API_KEY`, with a separately verified account machine-key route |

The Action never switches credentials after an authentication, rate-limit or
payment failure. OrcaRouter is optional; its advertised free route still needs
a key and has provider limits. GitHub runner usage has its own cost and limits.

The [workflow generator](../scripts/github-action/setup.mjs) is the complete
copy-ready example. It uses two ephemeral jobs, immutable action references,
read permissions for analysis, and write permission only for publication.
Neither job checks out repository code. The publishing job must use trusted
Action code and must not run files from the findings artifact. Do not combine
these jobs on a persistent runner: a later step on the analysis runner is not
a credential boundary. The publisher accepts only the current workflow run
and attempt; cross-workflow `workflow_run` publication is not implemented.
Never use `pull_request_target` with these actions.

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
| `reasoning-effort` | Engine automatic | Optional explicit reasoning level; generated v0.10.2 workflows use `off` |
| `max-chars` | 200000 | Complete diff characters per pass, maximum 8388608 |
| `max-passes` | 1 | Complete ordered passes, maximum 64 |
| `max-output-tokens` | CLI automatic | Optional per-pass ceiling, 8192–1000000 |
| `timeout-seconds` | 600 | Model/publication deadline, 30–1200 |
| `post` | `true` | Legacy direct publication; generated workflows use `false` |
| `trigger-phrase` | `@codewhale` | Exact mention name, followed by a standalone `review` command |
| `allow-forks` | `false` | Maintainer-triggered inert fork analysis from the default branch; set the same value on the publisher |

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
supplies a complete valid patch containing their line and the Engine records
introduced authorship. A finding needs a `reproduced` or `traced` evidence tier
with nonempty detail to appear in the review. Missing, empty and suspected
evidence stays in the analysis artifact; the review reports only the withheld
count. This checks the evidence record, not the truth of the claim.
Pre-existing findings appear in a collapsed section; unanchorable findings
and findings without recorded authorship appear in the body. Up to 50
eligible findings are also posted inline; every retained finding stays in the
summary. Oversized review bodies fail before any write instead of silently
dropping findings.

Freeform model summaries, merge-risk assessments and unlinked suggestions
stay in the analysis artifact so they cannot repeat a withheld allegation.
The published summary links to analysis and coverage and never treats an
empty finding list as approval. Publication receipts count evidence-eligible
`findings` and report `withheld_findings` separately. These rules apply to the
separate publisher; legacy `post:true` uses the pinned CLI's policy.

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
| `not_eligible` | Unapproved fork, draft, closed PR, unsupported event, stale comment or missing write access; no model run/write |

Failures fail the optional job; whether it is a required merge check is the
repository owner's policy. A provider failure is not a negative verdict about
the PR. No additional failure comment is posted.

After repairing setup, run the entire workflow with a PR number. Rerunning
only publication in a new run attempt rejects the old artifact; this prevents
silently publishing a bundle from another execution. Enabling a disabled
workflow does not replay old events. Legacy `post:true` preserves the CLI's
direct publication behavior and does not receive the separate publisher's
cross-run deduplication or write-token isolation.
