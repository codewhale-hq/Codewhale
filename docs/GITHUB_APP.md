# GitHub App Setup (Codewhale Agent reviews)

`codewhale review --pr N` writes an advisory code review of a pull request. With
`--post` (or from CI) the review is published to GitHub. Published reviews can
appear under two identities:

- the default token the CI job already has (`github.token`), or
- a dedicated **GitHub App** so the review shows as a bot — e.g.
  `codewhale-agent[bot]` — instead of a personal account.

The App identity is optional. Nothing below is needed to run
`codewhale review --pr N` locally and print the report to your terminal.

Related docs:

- [Automatic Workflows](AUTOMATIC_WORKFLOWS.md) — the review workflow in context
- [Providers](PROVIDERS.md) — the model/key used to write the review
- [Receipts](RECEIPTS.md) — how posted reviews are anchored to a head SHA

## Actions setup and model selection

Use [the reusable GitHub Action setup](GITHUB_ACTION.md) for the workflow,
account machine key, exact model, release pin, limits, outcomes and retries.
The repository workflow is a thin caller of the same analysis and publication
actions. Select your own provider and model; the checksummed CLI release does
not compile or execute a PR's candidate source. An App controls the publishing
identity independently of the model provider. The workflow generator can use
an existing App with `--app-login 'YOUR-APP[bot]'`.

## Review evidence and precision

The Actions-backed GitHub App and the `review` tool use the same PR review
contract. Findings must explain an introduced defect's trigger, source evidence,
impact and a useful fix. Generic requests for more tests, style preferences and
unsupported compiler claims do not qualify as findings. An empty findings list
is valid; unresolved assumptions belong in the assessment.

When the exact PR head is available locally, each pass also receives numbered
source excerpts around its changed hunks and nearby module declarations. These
come from regular Git blobs at the pinned head, never from dirty checkout files
or symlink targets. Source is not executed and no additional model call is made.
The excerpts use only the unused portion of `CODEWHALE_REVIEW_MAX_CHARS`, capped
at 50000 characters and 32 files per pass; individual blobs above 128 KiB are
omitted. The complete diff remains intact and remains the inline-comment scope.

The request explicitly records unavailable files and omitted context. It does
not inspect unchanged caller files or run builds/tests, and a completed review
does not establish either. These source and local-fixture guarantees do not
establish a model's bug-detection rate or parity with another review product.

## Output budget

`CODEWHALE_REVIEW_MAX_OUTPUT_TOKENS` optionally sets the CLI's output budget
through `CODEWHALE_MAX_OUTPUT_TOKENS`. Without it, the CLI chooses its automatic
cap. The workflow rejects values below **8192** to leave room for reasoning
and the final review. Provider accounting and supported limits vary; an empty
response is not proof of any one cause. A zero-exit review with empty output
fails the job.

## One-time setup, five steps

You need owner access to the GitHub repository once. After setup, eligible non-draft
same-repository pull requests can post reviews as the App.

1. **Create the App.** GitHub → *Settings → Developer settings → GitHub Apps →
   New GitHub App*. Name it (e.g. `Codewhale Agent`), set a homepage URL, and
   **uncheck Webhook → Active** — the review is pulled on PR events by Actions,
   so no webhook is needed.
2. **Grant two repository permissions.**
   - *Pull requests* → **Read & write** (to post the review and inline comments)
   - *Contents* → **Read-only** (to read the diff; read-only is enough — avoid
     write unless you have another reason)
   Choose *Only on this account*, then **Create GitHub App**.
3. **Download the private key.** On the App's page, *Private keys → Generate a
   private key*. Keep the `.pem` file secret; it is the App's credential.
4. **Install the App** on your account (*Install App* on the same page) and
   select the repositories reviews should cover.
5. **Add repository settings.** GitHub → *Settings → Secrets and
   variables → Actions*:

   | Kind     | Name                        | Value                               |
   |----------|-----------------------------|-------------------------------------|
   | Variable | `CODEWHALE_APP_ID`          | the App ID shown on the App's page  |
   | Secret   | `CODEWHALE_APP_PRIVATE_KEY` | the full `.pem` file contents       |
   | Secret   | `CODEWHALE_API_KEY`         | a Codewhale machine key for this repository workflow |
   | Variable | `CODEWHALE_REVIEW_MODEL`    | exact account catalog `provider/model` id (required) |
   | Variable | `CODEWHALE_REVIEW_VERSION` | exact released CLI tag; generator default v0.10.2 |

   App settings control identity. Model access separately requires a review
   key and, for account mode, the catalog model. Optional budget variables are
   `CODEWHALE_REVIEW_MAX_CHARS`, `CODEWHALE_REVIEW_MAX_PASSES`, and
   `CODEWHALE_REVIEW_MAX_OUTPUT_TOKENS`.

## How the pieces connect

[The review workflow](../.github/workflows/codewhale-review.yml) uses
`pull_request` and a manual `workflow_dispatch` recovery trigger. The analysis action reads the PR's exact Git objects in a fresh repository
without checking them out. Automatic fork events receive no model key. Draft
and closed PRs are rejected. With `allow-forks: true`, an authorized
maintainer can review a fork diff through a PR issue comment or manual run
from the default branch. Both analysis and publication recheck permissions,
triggering comments and revisions.

When generated with an App login, the publisher mints a short-lived
installation token restricted to contents:read and pull_requests:write.
App credentials and model credentials are kept in separate jobs. The publisher
verifies the returned author against that exact login. Without App setup,
generated workflows use `github.token` and `github-actions[bot]`. The action
emits one COMMENT review, never approval or a request for changes. CODEOWNERS
stays the human authority. Setup or provider failures fail the optional review
job and save a sanitized receipt; they do not post additional status comments.

The Actions-only App setup above does not describe the managed hosted App.
Do not disable the webhook on an existing App that also serves hosted mentions.

## Running a review yourself

```sh
# print a report locally (uses your configured provider key)
codewhale review --pr 1234

# pin the route when a model is reachable through more than one provider
codewhale --provider deepseek --model MODEL_ID review --pr 1234

# account mode: check the agent, then use an exact id from the account catalog
codewhale --no-project-config account agent
codewhale --no-project-config --provider codewhale --model PROVIDER/MODEL_ID review --pr 1234

# explicitly increase a complete-diff input limit when needed
codewhale review --pr 1234 --repo OWNER/REPO --max-chars 6000000

# explicitly authorize at most 8 complete ordered model passes
codewhale review --pr 1234 --repo OWNER/REPO --max-passes 8

# publish it to GitHub as whichever identity GH_TOKEN carries
codewhale review --pr 1234 --post
```

`GH_TOKEN` may be your `gh` CLI token (posts as you) or an App installation
token (posts as the App). The `--post` flag is always opt-in.

## Troubleshooting

- **Review posts as you, not the bot.** Generate the workflow with the exact App
  login, and set both App ID and private-key settings. App-enabled generated
  workflows fail on missing credentials instead of changing the author.
- **No model review completed.** Read the outcome receipt and the
  [repair guide](GITHUB_ACTION.md#outcomes-and-recovery).
- **Account model or provider error.** Set the provider to `codewhale` (or
  unset it), choose the exact model from the account catalog, and check that
  the machine key has the required scopes and the account has a configured
  agent. A vendor key belongs in its own secret, never `CODEWHALE_API_KEY`.
- **Complete diff exceeds the input limit.** Inspect the reported size and
  model context capacity before raising `CODEWHALE_REVIEW_MAX_CHARS`. An 8 MiB
  transport-bound failure cannot be bypassed with that variable.
- **PR head changed or history is unavailable.** Rerun for the current
  revision. The workflow refuses to review an unverified snapshot.
- **"available from configured provider route(s): ...".** Two provider keys are
  configured and the model is reachable from both. Use an explicit provider in your own Action configuration.
- **Empty review.** The job fails. Inspect provider errors and output-budget
  receipts; increasing `CODEWHALE_REVIEW_MAX_OUTPUT_TOKENS` may help when
  reasoning exhausted the budget, but does not diagnose the cause by itself.
- **App token step fails.** The `.pem` was regenerated after the secret was
  set — paste the newest key into `CODEWHALE_APP_PRIVATE_KEY` again, and
  confirm the App is actually installed on the repository.
- **Name already taken.** GitHub App names are global; pick another name. The
  bot's display login is `<slug>[bot]`, derived from the name.
