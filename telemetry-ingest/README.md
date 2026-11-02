# `telemetry-ingest` — Codewhale's first-party telemetry endpoint

A Cloudflare Worker that accepts the batches described in
[`docs/TELEMETRY.md`](../docs/TELEMETRY.md) and writes them to Workers Analytics
Engine. One POST route. No response body on any path. The handler never reads
client-IP metadata and the closed payload schema accepts no address field.

It lives here and not in `web/` because the site is a separate deploy with its
own build (Next.js via OpenNext); this is a single 13 KiB script with no assets,
and coupling the two would mean a telemetry change rebuilding the marketing site.

**Deployed and live** at `https://telemetry.codewhale.net/v1/telemetry`, which
is the shipped default for `telemetry_endpoint`. workers.dev is disabled; that
hostname is the only way in.

The current source defaults usage analytics on under notice version5, with a
clear opt-out. Prior explicit declines remain off; a missing acceptance record
is not treated as consent. Schema3 carries `notice_version: 5` policy metadata;
legacy schema2 retains its original explicit `consent_version: 4` contract.
Schema1 clients are accepted only into first-party Analytics Engine. Local
schema3 validation is not proof this updated Worker has been deployed. A user who wants
to contact nobody sets `telemetry_endpoint = ""`, which writes batches to
`$CODEWHALE_HOME/telemetry/dryrun.jsonl` instead.

---

## Optional PostHog processor

The new sink is **unconfigured by default**. It uses no analytics SDK and does
not create a second collector. `POSTHOG_HOST` must be exactly
`https://us.i.posthog.com` or `https://eu.i.posthog.com`, and
`POSTHOG_PROJECT_TOKEN` must be supplied as a Worker secret. The separate
`POSTHOG_IP_SAFE_EGRESS_VERIFIED="true"` prerequisite requires the staging
receipt below. Never commit a real token. Missing any prerequisite or using
an untrusted host leaves the sink off.
Deployment, token configuration, retention/privacy settings, and activation
require separate operator approval; none is established by the local tests.

Validated schema3 batches carrying `notice_version: 5` and legacy schema2
batches carrying explicit `consent_version: 4` can reach the processor. Legacy v1 always remains first-party only. Capture uses
`/batch/`, fixed anonymous/no-geo controls, no incoming headers, no cookies,
no redirects, no response parsing, no retries, and a 1.5-second timeout.
PostHog failure is isolated from the successful Analytics Engine write.
See [the complete disclosure](../docs/TELEMETRY.md) and
[PostHog's capture contract](https://posthog.com/docs/api/capture).

**Egress activation remains unproved.** Cloudflare can add client-IP headers to
Worker subrequests destined for non-Cloudflare zones, even when application
code builds fresh headers. The guard is an operator prerequisite, not code
that strips platform headers. Before setting it, record a staging receipt for
the actual deployment and regional destination proving original client IPs
are absent from all received headers, including `CF-Connecting-IP`,
`X-Forwarded-For`, and `X-Real-IP`. Requalify if the egress path changes. Without
that receipt, leave the sink disabled; use a detached first-party delivery
context if the deployed network cannot satisfy the invariant. Local fetch
mocks do not prove edge behavior. See [Cloudflare's header behavior](https://developers.cloudflare.com/fundamentals/reference/http-headers/#cf-connecting-ip-in-worker-subrequests).

The cross-repository CWC contract is generated, not independently authored:
`node scripts/export-product-schema.mjs` (Node 22.18+) exports
`schema/cwc-product-v3.schema.json` from this validator's constants. The v2
artifact remains available for existing clients. It accepts
only `web-app` / `desktop`, one closed `product_usage` event, and the fixed
no-fingerprint browser envelope. Copies must be compared with this generated
artifact when changing the schema. `test/golden/browser-v3.json` is a complete
website wire example; CWC changes only its surface to `web-app` or `desktop`.
`operations_summary` is separate operator-consented service health: six `u32`
aggregates on `control-plane` with a new random install ID per batch, never
individual request records or end-user identifiers.

The website/app same-origin proxy, when configured, must validate and forward
only the closed batch body and reconstruct a content-type header. Do not pass
through the incoming request, cookies, user agent, authentication, or metadata.
The ingest's two-header invariant remains unchanged.

## The one property that matters

`docs/TELEMETRY.md` publishes:

> Batches are **IP-stripped at ingest**. No IP is stored, logged, or joined to
> `install_id`.

This Worker is the whole of what makes that sentence true. There is no other
component. So:

- `src/index.ts` reads exactly two request headers — `content-type` and
  `content-length` — and nothing else, ever.
- It never touches the `cf` property of the request, so country, colo, city,
  region, ASN and coordinates are never in scope.
- `src/datapoint.ts` builds every stored row, and it cannot see the request at
  all: its input type is the validated batch body.
- Nothing logs. `invocation_logs` is off in `wrangler.jsonc`, because Cloudflare
  describes those as "enriched with information available to Cloudflare in the
  context of the invocation" — exactly the class of automatic per-request record
  this service promises not to keep.
- `test/no-ip.test.ts` reads the shipped source as text and fails the build if
  any of those names appears, if the set of headers read grows past two, if a
  `console.*` call is added, or if a `Response` is ever constructed with a body.
  That file is the only place in this directory where the forbidden header names
  are written down. A later edit cannot add one quietly.

Debugging without an IP is a solved problem: the schema carries `os`, `arch`,
`libc`, `surface`, `app_version` and `git_sha`, which is what crash triage
actually needs.

## What it stores

Everything in [`docs/TELEMETRY.md`](../docs/TELEMETRY.md) and nothing else. The
validator in `src/schema.ts` is a **closed** field set: an unexpected key
anywhere in the batch — envelope, event, `counters`, `errors`, `turn_wall` —
rejects the whole batch with `400`. That is the point of the design. A future
client bug that starts attaching a path, a prompt, or a customer's provider
table name gets refused by the server rather than quietly stored.

`test/schema-doc.test.ts` parses the field names and enum spellings back out of
`docs/TELEMETRY.md` and asserts set equality against the validator, and
`test/ingest.test.ts` posts `crates/telemetry/tests/golden/v1.json` — the
client's own pinned v1 wire form — and asserts it is accepted byte for byte and
that deleting *any* key from it is rejected. The doc, the Rust client, and this
endpoint cannot drift apart without a red test.

### Column layout

One Analytics Engine data point per event. A batch carries at most 200 events
(`BATCH_MAX_EVENTS`) and Analytics Engine allows 250 data points per invocation,
so a conforming batch never needs a second pass.

The layout is **positional and append-only** — Analytics Engine columns are
`blob1..blob20` / `double1..double20`, and the names below exist only in the SQL
you write. Renumbering silently rewrites every historical query. To add a field,
take the next free slot.

| column | contents |
|---|---|
| `index1` | `install_id` — random v4 UUID, client-rotated every 90 days. The only identifier in the schema. |
| `blob1` | `event` — `install_or_upgrade` \| `session_start` \| `session_end` \| `panic` \| `product_usage` \| `operations_summary` |
| `blob2` | `surface` |
| `blob3` | `os` |
| `blob4` | `arch` |
| `blob5` | `libc` |
| `blob6` | `app_version` |
| `blob7` | `git_sha`, `''` for every locally built binary |
| `blob8` | `tty` — `'true'` \| `'false'` |
| `blob9` | `install_kind` (`install_or_upgrade` only) |
| `blob10` | `previous_version` (`install_or_upgrade` only) |
| `blob11` | `session_source` (`session_start` only) |
| `blob12` | `duration_bucket` (`session_end` only) |
| `blob13` | `exit_class` (`session_end` only) |
| `blob14` | `cold_start_bucket` (`session_end` only; `''` on surfaces that do not measure it) |
| `blob15` | `providers`, comma-joined, already sorted and deduplicated; the one `provider` on `model_usage` |
| `blob16` | `panic_site` (`panic`) — a `crates/…` path or the literal `<dep>`; `model` (`model_usage`); `tool` (`tool_usage`) |
| `blob17` | `sent_at` — the *batch* timestamp. Events carry none. |
| `blob18` | `aggregate_counters` — closed JSON counts, for `product_usage` or `operations_summary` |
| `blob19` | `schema_version` |
| `blob20` | `privacy_version`: notice6 for v4, notice5 for v3, consent4 for v2, empty for v1; `blob19` keeps their meanings distinct |
| `double1..10` | `counters`: `turns`, `tool_calls`, `fleet_dispatch`, `workflow_run`, `subagent_spawn`, `mcp_server_connected`, `memory_search`, `approval_modal_shown`, `approval_auto_allowed`, `command_palette_open` |
| `double11..16` | `errors`: `auth_preflight_failed`, `provider_http_4xx`, `provider_http_5xx`, `tool_denied_by_policy`, `tool_timeout`, `network_error` |
| `double17..20` | `turn_wall`: `lt_5s`, `5_30s`, `30_120s`, `gte_120s` |
| `double1..11` on `model_usage` | `turns_ok`, `turns_failed`, `turns_interrupted`, `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`, then `turn_wall` `lt_5s`, `5_30s`, `30_120s`, `gte_120s` |
| `double1..2` on `tool_usage` | `calls`, `errors` |

Columns not relevant to an event are `''` / `0`. `tty` is a blob because the 20
doubles are exactly used by the three numeric structs — Analytics Engine's
ceiling is 20.

### What it structurally cannot store

Not "does not"; **cannot**, given the code as written:

- **The client IP, and anything derived from it** — never read. See above.
- **Any geo** — country, colo, city, region, ASN, coordinates, timezone.
- **Any key the schema does not name.** Unknown key ⇒ `400` for the entire
  batch, so there is no path from an unexpected field to storage.
- **Any free-form string.** The published schema has no free-form string type and
  no open-keyed map. Every field is an integer, a boolean, or a closed enum,
  except `app_version`, `git_sha` and `panic_site` — each of which has a regex
  here, so a path, a prompt, a URL, or a branch name fails the shape check.
- **A provider table name.** `providers` entries must be lowercase hyphenated ids
  and the array must be sorted and deduplicated; `acme_internal_gateway` is
  rejected. (See "known gap" below.)
- **A panic message.** Only `panic_site`, and only inside the `crates/`
  allowlist or the literal `<dep>`.
- **Per-event timestamps.** There are none in the schema; only `sent_at`,
  per batch.
- **Response content.** Every response is a bare status with a `null` body, so
  the endpoint cannot echo back what it received or what it holds.

**Known gap, stated plainly.** `providers` is the one field whose *value* space
this endpoint cannot close. The authoritative list is
`codewhale_config::provider::all_providers()`, a Rust registry with no generated
artifact to read, and hard-coding a copy here would drift into silently dropping
a real user's route. The client closes it (`Event::is_bounded` →
`is_known_provider_id`) before the POST is made; the server enforces the shape a
closed `&'static str` enum can produce, plus the doc's sorted-and-deduplicated
rule. If a generated provider-id list ever lands in the repo, wire it in here.

### Retention

Cloudflare stores Analytics Engine data for **three months**, and that is not
configurable — a ceiling rather than a policy, since no setting could make it
longer. `docs/TELEMETRY.md` states it.

---

## Deploy

Live. The commands below are the ones that produced the current deployment and
the ones that will produce the next one.

```sh
cd telemetry-ingest
npm install
npm test                    # 109 tests, including the doc weld and the IP guard
npx wrangler deploy --dry-run --outdir=.wrangler/dry-run   # no account touched
npx wrangler deploy         # <- the only command that publishes anything
```

The `routes` block in `wrangler.jsonc` binds it to `telemetry.codewhale.net` as
a custom domain, so the endpoint URL is
`https://telemetry.codewhale.net/v1/telemetry`. The workers.dev subdomain is
disabled: that hostname is the only way in.

Verified against the live endpoint before the client default was changed: the
client's golden batch returns `204` with a zero-byte body; an unknown key
returns `400`; `GET` returns `405`; a wrong content type returns `415`; a `POST`
to `/` returns `404`. Reading back from Analytics Engine returned exactly two
rows — `session_start` and `session_end`, carrying `install_id`, `surface=tui`,
`os=macos` — the documented shape and nothing else.

**Re-run the verification below after any deploy.** The client default now
points here, so a regression in this Worker is a regression in a promise
`docs/TELEMETRY.md` makes to users.

### Analytics Engine dataset setup

There is none. The dataset named in `wrangler.jsonc`
(`codewhale_telemetry`) is created implicitly on the first successful
`writeDataPoint`, so there is nothing to provision ahead of the deploy. Confirm
it exists after the first batch:

```sh
curl "https://api.cloudflare.com/client/v4/accounts/$CF_ACCOUNT_ID/analytics_engine/sql" \
  --header "Authorization: Bearer $CF_API_TOKEN" \
  --data "SHOW TABLES"
```

The token needs **Account → Account Analytics → Read**. Querying is out of band
through this API; the Worker itself has no read path at all.

### Rate limiting

`ratelimits` binding, 20 POSTs per 60s, **keyed on `install_id`** from the
validated batch body. Never on a network address — an IP-keyed limiter would
mean this Worker handles IPs, which is the whole thing it must not do. That is a
weaker limiter (an `install_id.json` can be rewritten between POSTs) and it is
the right trade: Cloudflare's edge already absorbs volumetric abuse, and the
client only flushes once per session anyway (one three-second attempt at
shutdown).

### Size cap

`MAX_BODY_BYTES` is 72 KiB, computed rather than guessed. A conforming client
sends at most 200 events totalling at most 65536 bytes (`BATCH_MAX_EVENTS` and
`BATCH_MAX_BYTES` in `crates/telemetry/src/actor.rs`, both hard ceilings because
`parse_events` breaks *before* crossing them), plus 199 commas and ~375 bytes of
envelope keys and values — 66110 bytes worst case. 72 KiB is ~11% headroom.

The 512-record / 256 KiB rings in `crates/telemetry/src/buffer.rs` are the *disk*
cap, not the wire cap: a full ring drains as three batches, never as one POST.

`content-length` is checked first as a cheap reject, but it is client-supplied,
so the real bound is enforced while reading the body and the stream is cancelled
the moment it goes over.

---

## Owner reports and queries

The routine activity report is a checked-in command rather than SQL copied
from a chat:

```sh
CF_ACCOUNT_ID=... CF_API_TOKEN=... npm run report:active-installs
CF_ACCOUNT_ID=... CF_API_TOKEN=... npm run report:active-installs -- --days 30 --json
```

The metric is **observed active installs by UTC day**: distinct rotating
anonymous `install_id` values that produced a `session_start` event that day,
plus the weighted number of sessions started. It is never a count of people,
accounts, or total installs, and the report says so next to the numbers.
`npm run report:dau` still works — `scripts/report-dau.mjs` is a pure
re-export of the canonical `scripts/report-active-installs.mjs`, kept only so
the old name keeps working — but the canonical name matches what the number
actually is.

One command produces all four things the ledger asks for:

- the **daily series** (default `--days 15` — 14 complete UTC days plus the
  partial current day, marked `*`);
- a **7-day trend** over complete UTC days only: the last 7 days' sum of
  daily observed active installs against the previous 7, with the percentage
  change. When the window cannot cover both sides it says so instead of
  printing zeros;
- **event freshness**: the timestamp of the newest ingested event of any kind
  and how stale it is, or a plain statement that nothing has been ingested;
- the **coverage caveats, printed with the numbers** in both text and
  `--json` modes: clients older than the telemetry feature, opted-out
  installs, and non-emitting environments (kill switches, fleet workers,
  offline shutdowns, dropped flushes) are invisible, so every count is a
  lower bound; and the 90-day id rotation means week-over-week comparisons
  are not a retention metric.

The token needs **Account → Account Analytics → Read**. The report performs
two read-only SQL requests (the series and the freshness probe), does not
print credentials, and rejects windows beyond Analytics Engine's fixed 90-day
retention. Its read path is pinned by `test/report-active-installs.test.ts`:
the install id appears only inside `count(DISTINCT …)`, no payload column
(`blob2`+, `double*`) is ever selected, and the output wording can never label
the result as users, people, or accounts.

The underlying ad-hoc queries are one query each, which is what the column
layout was chosen for. Run them against the SQL API:

```sh
query() {
  curl -sS "https://api.cloudflare.com/client/v4/accounts/$CF_ACCOUNT_ID/analytics_engine/sql" \
    --header "Authorization: Bearer $CF_API_TOKEN" --data "$1"
}
```

`_sample_interval` says how many original rows a stored row represents;
Analytics Engine downsamples high-volume indexes, so every count is weighted by
it rather than using bare `count()`.

### (a) How many installs and sessions

```sql
SELECT
  count(DISTINCT index1)                                          AS installs,
  sumIf(_sample_interval, blob1 = 'session_start')                AS sessions_started,
  sumIf(_sample_interval, blob1 = 'session_end')                  AS sessions_ended,
  sumIf(_sample_interval, blob9 = 'install')                      AS first_installs,
  sumIf(_sample_interval, blob9 = 'upgrade')                      AS upgrades,
  sumIf(_sample_interval, blob13 = 'clean')                       AS clean_exits
FROM codewhale_telemetry
WHERE timestamp > NOW() - INTERVAL '7' DAY
```

Read `installs` as what the published doc says it is and nothing more: the id
rotates every 90 days and is regenerated whenever the telemetry directory is
cleared, so **no count derived from `install_id` is a user count**. It is a lower
bound on distinct machine-installs seen in the window, and it undercounts a
returning user across a rotation.

Add `, blob6` to `SELECT` and `GROUP BY blob6` to cut by `app_version`; `blob3`
for OS, `blob2` for surface.

### (b) Which error classes and panic sites dominate

```sql
SELECT
  blob16                                   AS panic_site,
  sum(_sample_interval)                     AS rows,
  sum(double11 * _sample_interval)          AS auth_preflight_failed,
  sum(double12 * _sample_interval)          AS provider_http_4xx,
  sum(double13 * _sample_interval)          AS provider_http_5xx,
  sum(double14 * _sample_interval)          AS tool_denied_by_policy,
  sum(double15 * _sample_interval)          AS tool_timeout,
  sum(double16 * _sample_interval)          AS network_error
FROM codewhale_telemetry
WHERE timestamp > NOW() - INTERVAL '7' DAY
  AND blob1 IN ('session_end', 'panic')
GROUP BY panic_site
ORDER BY rows DESC
```

One query, two answers, because `panic_site` is `''` on every non-panic row:

- the **`panic_site = ''` row** carries the six error-class totals across all
  `session_end` events in the window — that is the error ranking;
- every **other row** is one panic site, ranked by how often it fired.

Sessions that ended in a panic are visible either way: `blob13 = 'panic'` on the
`session_end` row, and the `panic` event carries the site.

---

## Verifying no IP is stored

Three checks, in increasing order of how convincing they are.

**1. The source cannot ask for it.** `npm test` runs `test/no-ip.test.ts`, which
greps the shipped source. Prove the guard is live by adding a
`request.headers.get("CF-Connecting-IP")` line to `src/index.ts` and re-running —
three tests go red — then revert.

**2. Every stored column is accounted for.** The schema is closed and the layout
above is exhaustive; there is no free slot an address could occupy. Confirm the
deployed dataset has exactly the columns you expect:

```sh
query "SELECT * FROM codewhale_telemetry LIMIT 1 FORMAT JSON"
```

The result carries `dataset`, `timestamp`, `_sample_interval`, `index1`, and the
`blob*`/`double*` columns. There is no address column, because Analytics Engine
has no implicit one — every column is written by `writeDataPoint`.

**3. Search the stored data for an address shape.** After the first real
batches, this returns zero rows:

```sql
SELECT count() AS suspicious
FROM codewhale_telemetry
WHERE timestamp > NOW() - INTERVAL '7' DAY
  AND (
    match(index1, '\\d+\\.\\d+\\.\\d+\\.\\d+')
    OR match(concat(blob1, blob2, blob3, blob4, blob5, blob6, blob7, blob8,
                    blob9, blob10, blob11, blob12, blob13, blob14, blob15,
                    blob16, blob17), '\\d{1,3}\\.\\d{1,3}\\.\\d{1,3}\\.\\d{1,3}|:[0-9a-f]{1,4}:')
  )
```

Also confirm nothing is being logged: with `invocation_logs` off and no
`console.*` call in the source, **Workers Logs → this Worker** should show no
per-request entries at all.

---

## Local development

```sh
npm install
npm test          # vitest, 109 tests
npm run typecheck # tsc --noEmit
npm run check     # wrangler deploy --dry-run — touches no account
npm run dev       # wrangler dev --local
```

`wrangler dev --local` runs the real `workerd` with local Analytics Engine and
rate-limit bindings. Post the client's own golden batch at it:

```sh
curl -i -X POST http://127.0.0.1:8787/v1/telemetry \
  -H 'content-type: application/json' \
  --data-binary @../crates/telemetry/tests/golden/v1.json
# HTTP/1.1 204 No Content, zero-byte body

curl -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8787/v1/telemetry
# 405, Allow: POST
```

### Responses

| status | when |
|---|---|
| `204` | accepted, zero-byte body, no headers |
| `400` | not JSON, or fails the published schema — including any unknown key |
| `404` | POST to a path other than `/v1/telemetry` |
| `405` | any method other than POST, on any path (`Allow: POST`) |
| `413` | body over `MAX_BODY_BYTES` |
| `415` | content type is not `application/json` |
| `429` | this `install_id` is over the rate limit |
| `500` | internal error; nothing was written |

Every one of them has an empty body. The client
(`crates/telemetry/src/client.rs`) reads only the status class and drops the
batch on anything that is not 2xx — no retry, no backoff, no re-queue — so a
rejection is invisible to the user by construction, and a 5xx here can never
become a client-visible error. That is what lets this endpoint fail closed:
when in doubt, refuse the batch.
