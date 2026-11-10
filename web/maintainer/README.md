# Web maintainer

A scheduled GitHub workflow (`.github/workflows/web-maintainer.yml`) that keeps
codewhale.net current by opening pull requests. It never pushes to `main`, never
deploys and never posts to issues. It is off until the founder enables it.

## What runs

| Lane | Model | May change |
| --- | --- | --- |
| `derived` | none | Regenerated files only: `web/lib/facts.generated.ts`, `web/data/agent-docs.generated.json`, `web/data/features.generated.json`, `web/lib/terminal-capture.generated.ts`, `web/data/latest-published-release.json`, `docs/public-surface-facts.json`, `docs/cloud-facts/stable.json` |
| `copy-freshness` | DeepSeek | Stale version, model, provider or command mentions in `web/lib/content/**` and `web/lib/i18n/dictionaries/en/**` |
| `link-health` | DeepSeek | A moved URL in the same files, only where a probe saw a permanent redirect |
| `docs-drift` | DeepSeek | `web/lib/content/docs-*.ts` and `web/lib/i18n/dictionaries/en/docs-*.ts`, to match `docs/*.md` |

Each lane has its prompt in `lanes/<lane>.md` and its allowed paths in
`allowlist.txt` (the `derived` lane's are in `derived.txt`). Translations
(zh and the other locales) are not touched; they follow `npm run i18n:gt`.

## How it is contained

- The model step runs `codewhale exec --auto --sandbox workspace-write` with
  `deepseek-flash`, a pinned CLI release with checksums, a read-only checkout
  with no persisted credentials, and a scrubbed environment holding only
  `DEEPSEEK_API_KEY`. No GitHub token is present.
- After the run the job fails unless every changed path is an in-place edit of
  an allowlisted file: no untracked files, additions, deletions, renames or mode
  changes. The allowlist script and prompt are copied before the model runs.
- `npm run check` and `vitest` run on the proposal before it is uploaded.
- A separate `publish` job holds the GitHub App token (contents and pull
  requests write), re-applies each patch to current `main`, re-checks the
  allowlist, and opens or updates one pull request per lane on
  `web-maintainer/<lane>`. PR bodies contain only the diffstat, never model text.
- Files, issues and fetched pages are data in every prompt, not instructions.
- One run at a time (`concurrency: web-maintainer`), job timeouts, and a step
  summary for every lane. The workflow uses no `pull_request_target`.

## Enable it

1. Create a DeepSeek API key used only for this workflow and set a monthly
   spend cap on the provider side.
2. Add it as the repository secret `WEB_MAINTAINER_DEEPSEEK_API_KEY`.
3. Set the repository variable `WEB_MAINTAINER_ENABLED` to `true`.
4. Run the workflow once by hand (`workflow_dispatch`, lane `derived` first) and
   read the summary. The model lanes fail with a clear message when the secret
   is missing.

`CODEWHALE_APP_ID` and `CODEWHALE_APP_PRIVATE_KEY` are the same App credentials
the catalog refresh uses.

## Stop it

Unset `WEB_MAINTAINER_ENABLED` (or set it to anything but `true`). Every job is
skipped and nothing spends. To stop the spend itself, revoke the key or lower
its cap at the provider.

## Known limits

- Only the English dictionaries are edited; locale copy lags until translated.
- The link probe reads only the content and English dictionary files.
- A proposal is as good as the checks; a person reviews every pull request.
