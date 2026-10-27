# Catalog refresh

> 阅读简体中文版：[zh_hans/CATALOG_REFRESH.md](zh_hans/CATALOG_REFRESH.md)。

How Codewhale keeps model metadata current — what already auto-updates, what
is hand-maintained, and what a scheduled catalog job should (and should not) do.

Related docs: [`PROVIDERS.md`](./PROVIDERS.md), RFC
[`rfcs/UNIFIED_PROVIDER_LOGIN.md`](./rfcs/UNIFIED_PROVIDER_LOGIN.md).

---

## Short answer

| Question | Answer |
|---|---|
| Do users need a special model just to refresh models? | **No.** |
| Does Codewhale auto-update the public model catalog? | **Yes, at runtime**, from [Models.dev](https://models.dev/catalog.json), ~24 h TTL. |
| Is the offline bundled seed auto-committed in CI? | **No, but it is generated.** A maintainer runs `seed lock` and `seed render` and lands the verified generated slice on main; CI fails a hand edit (`seed render --check`) and a pin nobody re-locked (`seed age --max-days 30`). |
| Should an LLM rewrite catalog JSON? | **No.** Ingest is deterministic public JSON. An LLM can *review* a PR, not own the source of truth. |

---

## Layers (lowest → highest priority)

The shared catalog compiler applies these layers from lowest to highest:

```
0 bundled Models.dev
10 live Models.dev
12 Codewhale corrections (applied to layers 0 and 10 as they load)
15 verified cloud facts (optional, off by default)
20 exact provider-owned live roster
25 Codewhale account roster
30 config.toml
40 user overrides
policy DENY (final)
```

Codewhale corrections live in `crates/config/assets/catalog_corrections.json`.
They are field patches in the cloud-facts `ModelFact` shape, applied by the same
patch code to every Models.dev row, offline seed and live refresh alike, so a
correction holds on every install. Use one when an upstream fact is true but
misleading for a Codewhale route: `pricing_withheld` (a reason) clears the price
so the route reports it as unknown, for tiered rates, plan quota and billing
surfaces the catalog cannot tell apart; `max_output` and the other fields patch
limits. Every entry carries its reason. Corrections only fix rows that exist,
never add or hide one, and signed cloud facts can still override them. A
corrected row keeps its own source; a price a correction owns reports
`CatalogSource::CodewhaleBundled` as its price source. Do not hand-edit the offline seed
to hold a value back: a live refresh replaces the seed row, so the hold would
work only offline.

Cloud facts use the existing compiler and provider lake, as described in
[`CLOUD_FACTS.md`](./CLOUD_FACTS.md). Capability provenance and price provenance
are separate: a capability patch cannot relabel inherited prices. Cloud price
patches replace the entire price block; unspecified token classes stay unknown.

Route resolution also binds provider kind, configured identity and endpoint.
A fresh provider-owned roster is authoritative for its exact scope. Explicit
model selections remain explicit. Codex account observations/native cache and
Ollama endpoint tags keep their dedicated availability rules; a public catalog
row does not prove that an account can call that model. The installed Codex
`account/read` and `model/list` path is documented in
[`PROVIDERS.md`](./PROVIDERS.md).

Legacy completion lists remain a last fallback where no applicable catalog
exists. Bundled seeds and static transport/billing rules remain release-owned;
refreshing catalog metadata does not introduce a new wire dialect or change
credential/billing ownership.

Key code:

| Piece | Path | Role |
|---|---|---|
| Live fetch + cache | `crates/tui/src/models_dev_live.rs` | Background refresh, TTL, atomic write, freshness status |
| Schema / parse | `crates/config/src/models_dev.rs` | Network-free Models.dev JSON shape |
| Compile + provenance | `crates/config/src/catalog.rs` | Ordered sources, independent price provenance, policy deny, id normalization |
| Provider lake merge | `crates/tui/src/provider_lake.rs` | Shared catalog projection with exact route-scoped provider authority |
| Offline seed asset | `crates/config/assets/models_dev.bundled.json` | Compact offline fallback only (`_meta.role` says so) |
| Codewhale corrections | `crates/config/assets/catalog_corrections.json` | Field patches applied to every Models.dev row (`crates/config/src/catalog/corrections.rs`) |
| Validation script | `scripts/catalog_models_dev.py` | Secret-free fetch/validate dry-run (#4117) |
| Script tests | `scripts/catalog_models_dev_test.py` | Offline shape/scrub checks |

---

## What already auto-updates (runtime)

When the TUI/runtime starts (and is not disabled):

1. Seed pickers from the **on-disk cache** if present (even if stale).
2. If the cache is missing or older than **24 hours**, **background-fetch**
   Models.dev (15 s timeout, explicit Codewhale user-agent, **no credentials**).
3. On success: atomic write to
   `~/.codewhale/catalog/models-dev-catalog.json` and publish rows into
   ProviderLake as `CatalogSource::ModelsDevLive` — layer 10, carrying no
   endpoint fingerprint. Models.dev is a public catalog describing a model, so
   a refreshed row is treated exactly like the layer-0 seed it supersedes and
   stays correctable by layer 15. `CatalogSource::Live` is reserved for a
   provider's own credential-scoped `/models` answer at layer 20.
4. On failure: keep prior cache or fall back to the **bundled** seed. Model
   selection never hard-fails because Models.dev is down.

### Manual force refresh

In the TUI:

```text
/model refresh
```

That dispatches `AppAction::RefreshModelsDevCatalog` (async; does not block
the composer). When admitted cloud-facts settings are enabled, it also requests
a cloud refresh; hard-disable and trust-key checks still apply. Implementation lives under
`crates/tui/src/commands/groups/core/core.rs` and
`crates/tui/src/models_dev_live.rs`.

### Env knobs (tests / dogfood / offline)

| Variable | Effect |
|---|---|
| `CODEWHALE_MODELS_DEV_URL` | Override base URL or full `*.json` catalog URL |
| `CODEWHALE_MODELS_DEV_PATH` | Load catalog from a local file; skip network |
| `CODEWHALE_DISABLE_MODELS_DEV_FETCH` | Truthy → never hit the network (`1` / `true` / `yes` / `on`) |

Defaults:

- Catalog URL: `https://models.dev/catalog.json`
- TTL: `24 * 60 * 60` seconds (`DEFAULT_MODELS_DEV_TTL_SECS`)
- Cache file name: `models-dev-catalog.json` under the Codewhale `catalog`
  state dir

Freshness values exposed for UI / status chips: `bundled` | `live` | `stale` |
`failed`.

---

## What does **not** auto-update (repo / release)

These stay hand-maintained or release-lane work until a scheduled PR lands:

| Surface | Why it drifts |
|---|---|
| `models_dev.bundled.json` | Offline seed, generated from a reviewed spec and a pinned lock (see below); refreshed by a verified maintainer slice, not at runtime |
| `provider_descriptors.json` default model IDs | Product choice, not pure catalog dump; constant projections are generated |
| `catalog_corrections.json` `reviewed` | Intrinsic/selector/transport compatibility facts with exact source receipts; generated into the same seed |
| Rust pricing policy | Vendor billing windows, independent USD/CNY rates, withheld/tiered behavior; pure reference observations live in the reviewed supplement |
| New `ProviderKind` / wire dialect | Needs code, not only JSON |

Runtime live refresh **does not** rewrite those files. Users on a recent
install with network still see new Models.dev rows; fresh clones offline, CI
hermetic runs, and first-boot without cache still depend on the seed.

### The failure this guards: a default that silently stops existing

An account roster can rotate out from under a hand-written default. Observed
2026-10-23: the ChatGPT plan roster served exactly seven ids
(`gpt-6.1-sol`, `gpt-6-astra`, `gpt-6-sol`, `gpt-6-luna`, `gpt-5.6-sol`,
`gpt-5.6-terra`, `gpt-5.6-luna`) and no bare `gpt-5.6`, which is what the
`openai-codex` descriptor defaulted to. A fresh roster redirects the default
(`CodexModelRoster::preferred_model_id`), so a live account avoided it; a stale,
expired or missing roster fell through to `gpt-5.6` and the request failed.

Defaults are the dangerous half of staleness because a stale *price* misreports
and a stale *default* breaks the request. Two mechanisms now carry the duty:

- **Hermetic drift guard.** `seed lock` and `seed render` compare every
  descriptor `default_model` against the seed default and refuse to write when
  they disagree (`--descriptors` to point at another asset). A deliberate,
  explained disagreement goes in `SEED_DEFAULT_EXCEPTIONS` in
  `scripts/catalog_models_dev.py`; an unexplained one fails the run.
- **Review-time freshness report.** `seed lock --dry-run` prints which
  providers have newer upstream ids than the seed default, plus rows in the
  lock the spec no longer references. Nothing offline can know a model
  shipped, so this report is the review step, not a gate.

- **Release-time default audit.** `defaults audit` re-reads a models.dev
  payload (the fetched snapshot, or `--snapshot PATH`) and lists each provider
  whose default a newer stable row of its own namespace has overtaken. An
  unrecorded one fails the run; a deliberate hold is recorded with its reason
  in `DEFAULT_AUDIT_REASONS`, and a curated or marketplace route is advisory.

Honest limit: the hermetic guard catches descriptor↔seed disagreement, and
`seed age` catches a pin nobody re-locked: it fails once `source.fetched_at`
is older than the limit CI enforces (30 days). Neither can tell that a vendor
shipped a model on the day it happened, so the report above is still the step
that catches a brand-new release.

---

The current quote is frozen at dispatch with its exact provider identity, wire
ID, endpoint fingerprint, source and rates. Refreshing the catalog does not
reprice old turns. Live rows carry fetch and expiry clocks separately from an
upstream update or a declared effective date. After 24 hours a live price is
stale; a bundled row is a dated estimate, never proof of account access.

The shared decimal calculator selects each request's context tier from its
entire input, including cache reads and writes. It prices seven published
classes and subtracts separately charged reasoning/audio subsets from the
ordinary classes. Missing rates or a required usage breakdown stay unpriced.
The result is an exact decimal string; display rounding does not change it.
CNY prices remain separate from USD. These are estimates, not reconciled bills.

Two default wire IDs missing from the 2026-10-12 upstream snapshot remain as
explicitly dated curated compatibility rows in the seed spec, with their prior
snapshot receipts. They do not establish present account availability or a
renamed wire ID. Other removed non-default rows are no longer seeded.

## Maintainer tooling (no LLM)

### Validate / dry-run fetch

```bash
# Fetch Models.dev + print counts (never writes disk)
python3 scripts/catalog_models_dev.py refresh

# Validate the committed offline seed still parses as Models.dev-shaped JSON
python3 scripts/catalog_models_dev.py snapshot --check \
  crates/config/assets/models_dev.bundled.json

# OpenRouter public /models listing (no API key), dry-run only
python3 scripts/catalog_models_dev.py refresh --provider openrouter \
  --sort newest --limit 100

# Drift report against the committed seed: field changes, defaults that a
# newer upstream id has overtaken, and lock rows no longer referenced
python3 scripts/catalog_models_dev.py seed lock --dry-run
```

Design constraints of the script (intentional):

- Public endpoints only — no `Authorization` headers, no API keys.
- Credential-shaped keys are scrubbed if present in remote JSON.
- `refresh` and `snapshot` never write (`--write` / `--write-cache` fail
  closed). The one write path is `seed lock`, which pins only the rows the
  spec references, projected onto allowlisted fields.

### Regenerating the offline seed (#6396)

`crates/config/assets/models_dev.bundled.json` is generated. Never edit it by
hand: CI runs `seed render --check` and fails on any difference.

| File | Holds | Edited by |
|---|---|---|
| `scripts/catalog/models_dev_seed.toml` | Which upstream rows to carry, their Codewhale provider id, wire id, default, canonical join, and the few curated rows upstream does not list | Hand, reviewed |
| `scripts/catalog/models_dev_seed.lock.json` | The referenced upstream rows, allowlisted, plus the source URL, fetch time and sha256 | `seed lock` only |
| `crates/config/assets/catalog_corrections.json` | Deliberate holds: withheld prices, clamped limits, reasoning controls | Hand, reviewed; applies online too |
| `crates/config/assets/catalog_corrections.json` `reviewed` | Source-preserved intrinsic facts, scoped aliases, completion references, public labels/source-support dates, pure route facts and reference prices | Hand, reviewed; source receipts retained |
| `crates/config/assets/models_dev.bundled.json` | The rendered seed including the reviewed supplement | `seed render` only |

The spec selects and maps; it cannot state a value that disagrees with
upstream (unknown keys are refused). If an upstream value is wrong for a
Codewhale route, add a correction instead. Corrections apply to both the seed
and live rows; a hold made only by hand-editing the seed would vanish on the
first live refresh.

1. `python3 scripts/catalog_models_dev.py seed lock --dry-run` prints the
   review report: field changes per row, corrections that upstream now
   agrees with (delete them), upstream models not carried, a
   **default freshness** block (providers whose default is older than a
   newer upstream id) and a **lock rot** block (pinned rows the spec no
   longer references). It fails when a referenced row disappeared upstream,
   when a curated row now exists upstream (switch it to a derived row), or
   when a descriptor default disagrees with the seed default.
2. Edit the spec or the corrections as the report requires.
3. `python3 scripts/catalog_models_dev.py seed lock` writes the lock.
4. `python3 scripts/catalog_models_dev.py seed render` writes the seed.
5. Check that default wire IDs still match `DEFAULT_*_MODEL`, run the
   catalog checks, and land the verified slice on main with its report in the commit.
6. `python3 scripts/catalog_models_dev.py seed age --max-days 30` is what turns
   the above from a habit into a cadence: it fails, locally and in CI, when
   `source.fetched_at` in the lock is older than the limit. It is offline and
   reads only the lock, so it cannot excuse a stale pin either way — the fix is
   always another re-lock.

Optional: use a cheap model to summarize “new / removed / default-risk” in
the PR body — never as the author of the JSON.

---

## Recommended scheduled job (not shipped yet)

Goal: keep the **in-repo offline seed** from rotting, without giving CI write
power over secrets or unsupervised LLM rewrites.

```text
cron (daily or weekly)
  → fetch Models.dev (public, no keys)
  → validate shape + scrub
  → compare against crates/config/assets/models_dev.bundled.json
     (and optionally report new ids vs provider defaults)
  → if material change: open PR
       title: chore(catalog): refresh Models.dev offline seed
  → optional: include an agent-written, human-readable diff summary in the PR body
```

Such a job would run `seed lock` and `seed render` and open the PR. A PR
opened with the default `GITHUB_TOKEN` does not trigger CI, so it needs a bot
token or GitHub App, which a maintainer has to provision.

### In scope for automation

- Deterministic catalog ingest from Models.dev
- Secret-free PR diffs
- Drift reports (new model ids, missing defaults, pricing presence)

### Out of scope for automation

- Claude Pro/Max / subscription OAuth “model discovery” (not a supported
  third-party path; Anthropic expects API keys for third-party tools)
- LLM-authored edits to `models.rs` / `provider.rs` without review
- Force-pushing `main` or silent asset rewrites on the default branch
- Treating Models.dev as the only truth for OAuth-scoped routes (Codex
  roster remains special-cased)

### Suggested workflow home

`CodeWhale/.github/workflows/catalog-refresh.yml` (or similar), reusing
`scripts/catalog_models_dev.py` after a deliberate **write-safe** extension
that only runs in CI with a bot token for PR creation — still not on
`workflow_dispatch` without review if writes land in-repo.

Nightly today (`/.github/workflows/nightly.yml`) builds release artifacts
only; it does **not** refresh catalogs.

---

## Do we need a “model dedicated to updating models”?

**No for the core loop.**

| Job | Right tool |
|---|---|
| Keep known models/windows/prices from Models.dev fresh for users | Runtime live fetch (already shipped) |
| Keep offline seed + release assets current in git | Scheduled CI → PR (to build) |
| Decide whether to bump a product default model | Human (or agent *review* on the PR) |
| Wire a brand-new provider kind / dialect | Human PR + tests |

An LLM is optional **review** of a catalog PR. It is a poor **source of
truth** for catalog JSON.

---

## Auth note (Claude / Anthropic)

Anthropic model **catalog** refresh does not require Claude Pro/Max OAuth.
Models.dev is public. Codewhale’s Anthropic route remains **API-key-based**
for inference (`ANTHROPIC_API_KEY`). Do not couple catalog automation to
subscription OAuth or Claude Code identity headers.

---

## Quick operator checklist

- [ ] Running install: confirm network not blocked; optional
      `/model refresh` after a big vendor launch.
- [ ] Offline / CI hermetic: set `CODEWHALE_DISABLE_MODELS_DEV_FETCH=1` or
      point `CODEWHALE_MODELS_DEV_PATH` at a fixture.
- [ ] Before release: `seed lock --dry-run` to see how far the offline seed
      has drifted from Models.dev, including the default-freshness block —
      a stale default is a broken request, not just a stale price. Re-lock
      by PR if it matters. Skim `PROVIDERS.md` for known drift.
- [ ] After Models.dev adds a major family you ship by default: consider
      seed PR + default-model decision separately.
- [ ] Never paste API keys into catalog assets or the automation script env
      for Models.dev refresh.

---

## Issue / design anchors

- Live Models.dev layer: #4187
- Bundled seed demoted (not competing truth): #4188
- Catalog automation script (validate / dry-run): #4117
- Generated offline seed and runtime corrections: #6396
- Deeper metadata inventory and drift list: the `codewhale-ops` repo

### Retired unscoped metadata reader

The former models crate cache reader and its separate bundled asset, and the
TUI-only model registry, are retired. Existing installed legacy cache files are
preserved and are never imported as provider or public-label authority.
`config::catalog` owns the immutable compiled intrinsic projection; Engine's
existing provider lake and scoped catalog cache still own live/account/config
facts. Identical wire names at different endpoints do not create a canonical
join, a public label, or an unscoped price. The bundled freshness clock uses the
actual seed lock fetch timestamp and rechecks the current clock on each query.

Compatibility completion lists reference the provider descriptor defaults and
catalog groups rather than repeating their values. Kimi's generation default
remains distinct from direct/membership route limits. Unknown capabilities and
name-suffix budgets remain explicitly unverified. Website model dates describe
source support, with the prior proven dates retained in the same reviewed owner;
they do not assert a provider release date or current API availability.
