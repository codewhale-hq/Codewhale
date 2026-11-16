# `telemetry-stats`

A separate, read-only Cloudflare Worker that shows aggregate charts of the
Codewhale telemetry dataset to the owner, and keeps per-day totals with no
install ids beyond Analytics Engine's three-month window. It is not a route on
`telemetry-ingest`, which stays write-only (see `docs/TELEMETRY.md`).

- **Reads in place.** Prefers the Analytics SQL Workers binding (`ANALYTICS_SQL`, Wrangler 4.145.0 or later, no token). If the binding does not work on the account, remove the `analytics` block from `wrangler.jsonc` and set `CF_ACCOUNT_ID` and `CF_API_TOKEN` (Account Analytics Read) as Worker secrets; the code falls back to the SQL API.
- **Aggregates only.** The only use of `index1` is `count(DISTINCT index1)`; no response field is an install id, and a dimension value that does not fit its closed list or pattern (surface, OS, tool, version, provider, model), or that looks like an id, is reported as `other`. On the REST path counts are weighted by `_sample_interval`; the binding weights automatically. Distinct-install counts are not weighted.
- **Private.** Every route, static page included, requires a valid Cloudflare Access JWT (`ACCESS_TEAM_DOMAIN`, `ACCESS_AUD`); without them the Worker answers 503. workers.dev and preview URLs are off.
- **Daily totals.** A cron (`17 2 * * *` UTC) writes `daily_totals` rows (day, metric, dimension, key, count) to D1, for the whole-population dimension and the funnels only, never per version, tool or model, so no small cell can pin down one install. D1 was chosen over R2 because the rows are tiny and relational, the write is an idempotent upsert, and the page reads a year of trend with one query, where R2 would need one object read per day. The first run backfills the 90 days still available, one week at a time, writing each week as it goes; later runs re-read the last two days to catch late batches and any missed run.

Routes: `/` the page, `/api/stats?days=N` (default 14, max 31, cached 5 minutes), `/api/trend?days=N` (kept totals from D1).

```
npm ci && npm test && npm run typecheck && npm run check
```
Deploying is the owner's action; see the review packet for the steps.
