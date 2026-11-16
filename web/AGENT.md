# Community Assistant Agent

The community assistant is a small set of Cloudflare Cron Triggers: the weekly digest drafter (`deepseek-flash`), the facts-drift refresher and the content watchers. Triage, PR-review, stale, duplicate and Today's Dispatch tasks were retired (§132.4); scheduled website upkeep now runs as pull requests from `.github/workflows/web-maintainer.yml` (see `maintainer/README.md`). **The Worker never posts to GitHub directly.** Every drafted output is staged in Workers KV for maintainer review.

## Architecture

```
Cloudflare Cron Triggers
  └─ worker.ts scheduled() handler
       ├─ daily     → linkcheck + semantic-drift watchers
       ├─ weekly    → digest (Mon 09:00 UTC)
       └─ 6h       → facts-drift

Drafts stored in Workers KV (keys always derived via `draftStorageKey` in `lib/community-agent.ts`):
  draft:digest:<year>-W<week>
  draft:linkcheck:<slug>-<sha256-16>          (identity: full broken URL)
  draft:semantic-drift:<slug>-<sha256-16>     (identity: page+claim+evidence+replacement)

Watcher draft IDs are deterministic — readable slug plus a 64-bit SHA-256
suffix over the finding's full identity, max 80 chars — so unchanged findings
dedup and changed findings land as new drafts. Semantic-drift model output is
validated and capped (10 drafts/run) before any KV writes.

Before sending a GitHub post, the action records its target/text identity in the
existing durable claim object and KV. Read or write failures post nothing.
The durable receipt survives lease expiry; retries with changed text or target
are refused while unresolved. Reconciliation searches up to ten pages under
one 30-second deadline and a per-page byte cap; an incomplete search posts
nothing. The KV fallback has eventual-consistency limits; deployment of the
existing durable binding still needs its own receipt.

A post whose GitHub outcome was unknown (network error, 5xx, 408, 429) leaves
  draft-post-unknown:<type>:<id>              (until resolved; the next post of that
                                               draft first looks on GitHub for
                                               the earlier attempt's post)

Usage logged to (one record per model call; sum the day's prefix):
  usage:<YYYY-MM-DD>:<timestamp>:<uuid>
```

## Cron schedule

| Expression | Frequency | Tasks |
|---|---|---|
| `0 */6 * * *` | Every 6 hours | Facts drift |
| `0 0 * * *` | Daily 00:00 UTC | Link check + semantic drift |
| `0 9 * * 1` | Monday 09:00 UTC | Weekly digest |

## Voice constraints

All drafts follow these rules:

- Calm, factual, never breathless.
- Never uses first person plural ("we"/"我们") — the maintainer is one person.
- Never commits to timing, prioritisation, or merge intent.
- Never apologises on the maintainer's behalf.
- Cites specific files / line numbers / linked issues when discussing code.
- Ends with: "— drafted by community assistant, pending maintainer review"
- Chinese drafts end with: "— 由社区助理草拟，待维护者审阅"
- Chinese output is rewritten in zh-CN, not machine-translated.

## Cost guardrails

- Each cron invocation caps at ~30k input tokens and ~2k output tokens.
- Issue/PR bodies are truncated to 1000–4000 chars before sending to the model.
- Deduplication: the existing `DRAFT_CLAIM_LOCK` authority serializes each cron task before source reads or model calls (45-minute crash lease, then a two-minute propagation hold). Missing KV or lock bindings skip generation before spending. Inside the claim, `hasFreshDraft` skips drafts newer than the item's `updated_at`.
- Token usage is logged as one `usage:<YYYY-MM-DD>:…` KV record per model call
  (retained 90 days); list the day's prefix and sum `calls`/`inputTokens`/
  `outputTokens`. Records are append-only because KV has no atomic increment.
- If `DEEPSEEK_API_KEY` is missing or the API errors, the cron returns 200 with `{ skipped: true, reason }` — never crashes, never retry-loops.

## Maintainer review surface

Open `/en/admin` (or `/zh/admin`) and enter `MAINTAINER_TOKEN` in the login
form. The form posts to `/api/admin/login`; the token never goes in the URL.

- Lists all pending drafts with source link, draft body, and three actions:
  - **Post as comment** — calls GitHub REST API using `MAINTAINER_GITHUB_PAT`
  - **Edit & post** — opens a textarea for editing before posting
  - **Discard** — removes the draft from KV
- The auth token is set via `MAINTAINER_TOKEN` env var. A successful login sets an httpOnly `mt_sid` session cookie that lasts 24 hours.
- **Nothing posts to GitHub without an explicit maintainer click.**

## Environment variables

| Variable | Required | Purpose |
|---|---|---|
| `DEEPSEEK_API_KEY` | Yes | DeepSeek API key for the community agent |
| `GITHUB_TOKEN` | Optional | Fine-grained PAT for GitHub API (raises rate limit) |
| `CRON_SECRET` | Optional | Shared secret for manual cron invocation (32 to 512 characters, at least 8 distinct, and a shorter value repeated is refused; otherwise the trigger stays disabled); attempts are rate limited (5 per minute per location) |
| `MAINTAINER_TOKEN` | Optional | Auth token for /admin panel; 32 to 512 characters with at least 8 distinct, and a shorter value repeated is refused (otherwise sign-in stays disabled); changing it signs out open sessions |
| `MAINTAINER_GITHUB_PAT` | Optional | GitHub PAT with `issues:write` scope for posting comments |

## Initial deployment

One-time setup before the first `npm run deploy`:

1. **Create the KV namespaces:**
   ```bash
   npx wrangler kv namespace create CURATED_KV
   npx wrangler kv namespace create NEXT_INC_CACHE_KV
   ```
   Copy the returned `id` values into the matching `CURATED_KV` and
   `NEXT_INC_CACHE_KV` bindings in `cloudflare.config.ts`.

2. **Set secrets:** generate `MAINTAINER_TOKEN` and `CRON_SECRET` with
   `openssl rand -base64 33` (any random value of 32 to 512 characters with at
   least 8 distinct and not a shorter value repeated works; `openssl rand -hex 32` is fine).
   ```bash
   npx wrangler secret put DEEPSEEK_API_KEY
   npx wrangler secret put MAINTAINER_TOKEN
   npx wrangler secret put MAINTAINER_GITHUB_PAT
   npx wrangler secret put CRON_SECRET
   ```

3. **(Optional) Raise GitHub rate limit:**
   ```bash
   npx wrangler secret put GITHUB_TOKEN
   ```

4. **Verify:**
   ```bash
   npm run predeploy   # checks KV ID is set
   npm run deploy      # builds + deploys
   ```

## Kill switch

To disable the community agent entirely:

1. Remove all cron triggers from `cloudflare.config.ts` except the `0 */6 * * *` facts-drift cron.
2. Redeploy: `npm run deploy`.

Facts drift continues working independently. Individual tasks remain callable manually for testing through `/api/cron?task=digest`, `/api/cron?task=linkcheck`, etc.

To disable a specific cron task, remove its cron expression from `cloudflare.config.ts` and redeploy.

## Bilingual output

Every draft contains both `bodyEn` (English) and `bodyZh` (Chinese zh-CN). The admin panel shows the version matching the current locale. The zh version is rewritten natively by the model, not translated from English.
