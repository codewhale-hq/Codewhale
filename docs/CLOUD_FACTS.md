# Signed cloud facts

Cloud facts are an optional signed overlay for model metadata and unset provider
model defaults. They are **off by default**: a client fetches nothing unless the
setting is turned on.

One production key, `cwf-2026-09`, is pinned in the Rust and website trust
tables with the `founder` role. This document and the source it describes do
not establish a deployed endpoint, a published database row, signing-key
custody, or a real provider request; check the live channel for those. The JSON
under `docs/cloud-facts/stable.json` is unsigned authoring material; its release
entry matches the checked-in `web/data/latest-published-release.json` receipt.
It is not a publication receipt. Public test fixtures establish local behavior
only.

Related: [catalog refresh](CATALOG_REFRESH.md) and [provider routes](PROVIDERS.md).

## Authority and configuration

```toml
[cloud_facts]
enabled = false
channel = "stable"
ttl_hours = 6
```

`CODEWHALE_CLOUD_FACTS=1|0` overrides the setting;
`CODEWHALE_DISABLE_CLOUD_FACTS=1` is the hard disable. Channel, URL and local
signed-envelope overrides are `CODEWHALE_CLOUD_FACTS_CHANNEL`,
`CODEWHALE_CLOUD_FACTS_URL` (optional `{channel}` placeholder) and
`CODEWHALE_CLOUD_FACTS_PATH`. A local file is still subject to all trust checks.
Production network refresh is suppressed in CI; tests opt into an explicit
loopback fixture transport policy.

Loading a config object is structural. Accepted startup/reload settings admit
one process-wide source generation. A refresh captures that generation before
work and must still own it to publish either memory or disk state. Disabling or
changing the source invalidates earlier work, clears the prior overlay and
invalidates catalog readers. Hard disable also blocks local-file/cache reads,
new network work, cache writes and a late refresh's publication.

Startup can read a bounded regular cache file and launch a background refresh;
network success is never a startup dependency. Missing, rejected, inapplicable
or expired facts leave the remaining catalog authorities usable. Status reports
whether facts are off, inert, verified, rejected or unavailable through the
existing compact catalog/status surface.

## Key roles

Every pinned key has a role, recorded in both trust tables and compared by
`check:facts`. One public key may appear under one key ID only.

- `founder`: held by a person, may sign any payload the client understands, on
  any channel.
- `automation`: for a scheduled publisher. A payload authenticated only by an
  automation key is accepted when all of the following hold, and is otherwise
  rejected whole as outside the key's scope:
  - the channel the client asked for ends in `-models`. An automation key can
    never authenticate `stable`, `beta` or any other founder channel, so it
    cannot replace a founder release or move that channel's rollback floor;
  - `facts_version` equals `published_at` in Unix seconds, so a version cannot
    run ahead of the clock and exhaust the channel's version space;
  - `not_after` is present, later than `published_at`, and at most seven days
    after it. Both timestamps are whole-second UTC;
  - it carries at most 256 model entries and no provider defaults, release
    facts, announcements or field outside the known payload fields;
  - every model entry is an upsert or a deprecation using only `provider`,
    `id`, `op`, `context_window`, `max_output`, `reasoning`, `display_name`,
    `deprecated_at`, `replacement`, `note`, `applies_to` and
    `pricing_withheld`. Prices, `allow_unlisted`, `reasoning_options` and any
    other field are refused;
  - text fields are plain (no control characters) and bounded, a context window
    is between 1,024 and 20,000,000 tokens, and a maximum output is between 1
    and 2,000,000 and not above the context window.

  The same rule is written three times: `automation_scope_violation` in
  `crates/config/src/cloud_facts/verify.rs`, `automationScopeViolation` in
  `web/lib/cloud-facts.ts`, and the function of that name in
  `web/scripts/facts-publish.mjs`. All three are tested against
  `docs/cloud-facts/fixtures/automation-scope-cases.json`.

If an envelope carries both a founder and an automation signature that verify,
the founder signature decides and the scope does not apply.

What this does not do:

- **No automation key is pinned**, and a test in each language fails if one
  appears without that test being changed.
- **No client fetches a `-models` channel.** Until one does, a pinned
  automation key would authenticate nothing a client asks for.
- **No scheduled publisher exists**, and `emit-sql --public-key` still performs
  no pin, role or scope check.
- The seven-day limit bounds one payload. A client keeps applying a cached
  payload through the existing 48-hour grace after `not_after`, and whoever
  holds the key can sign a new payload at any time.

## Verification and expiration

The `facts/v1` envelope contains exact base64 payload bytes, their SHA-256,
Ed25519 signatures and repeated metadata. The signed message is:

```
"codewhale-facts/v1\0" || key_id || "\0" || payload_bytes
```

Clients verify bounded envelope/payload sizes, supported envelope and algorithm,
an active pinned key, signature and digest, signed/outer metadata agreement,
channel, schema, semantic-version applicability and the accepted version floor.
The key ID participates in the signature. Rotation can carry extra signatures;
at least one active approved key must verify. A database key registry is not a
trust root.

Publication, expiration and announcement dates must be valid UTC timestamps.
Future publications are rejected outside the bounded clock tolerance. Signed
expiry is never extended by a successful refresh or `304`. The client's stated
48-hour expiry grace is included in the scoped validity bound; after that bound
facts are stale and cannot supply catalog prices or defaults. The public relay
rejects expired delivery. Per-item applicability and announcement windows are
re-evaluated when cached data is reused.

A `304` authenticates nothing by itself: cached bytes must re-verify against the
current keys, channel, binary version, rollback floor and clock. Cache and ETag
identity are partitioned by source/channel, and channel rollback protection
survives a source change. HTTP bodies, outer disk cache records and labels are
bounded; cache/local readers reject symlinks, non-regular files, multiply linked
files and oversized input. A failed or untrusted response cannot become a new
catalog authority.

## Catalog and cost behavior

The existing compiler inserts cloud facts at layer 15:

```
0 bundled Models.dev < 10 live Models.dev < 12 Codewhale corrections
< 15 verified cloud facts < 20 provider-owned live < 25 Codewhale account
< 30 config < 40 user overrides < policy DENY
```

An upsert patches specified metadata fields. `pricing_withheld` (a reason)
clears a row's price so it reads as unknown rather than as a misleading flat
rate; the bundled corrections in `crates/config/assets/catalog_corrections.json`
use the same field and patch code. Creating a row requires either its
context window or an `allow_unlisted` assertion (below); an attested ID-only row
is created with every limit, price and capability **unknown** rather than
inferred from a sibling model or a lower stale layer. Deprecation annotates;
hide only removes lower bundled/Models.dev rows. Cloud data cannot delete
provider-live, account, config or user rows.

**Which rows a patch reaches.** An upsert replaces fields on a row held at
layer 0 or 10 — the bundled Models.dev seed or a live Models.dev refresh,
including rows a bundled Codewhale correction patched — and is skipped with a receipt on anything at
layer 20 and above. That reach is the point of the layer split: most models a
user sees are described by Models.dev rather than by the provider, so a stale
context window or a changed rate on such a model is exactly what a signed
correction exists to fix, without a reinstall.

The distinction is what was *asked*, not what was fetched most recently. A
provider `/v1/models` answer is a fact about an endpoint the user
authenticated to, so it outranks a signed correction and is only ever
completed, never displaced. A Models.dev refresh is a public third-party
catalog that is merely fresher than the copy compiled into the binary, so it
is corrigible on the same terms as that copy. A refreshed row therefore carries
`CatalogSource::ModelsDevLive` and no endpoint fingerprint; only a provider
roster carries `CatalogSource::Live`.

A provider `/v1/models` roster is authoritative for the IDs it lists **and for
its own omissions**. This client keeps no history of past rosters, so it cannot
tell a never-listed preview from a model the provider retired, and it does not
guess: no local layer — bundled, Models.dev, or anything else — is evidence
about what a provider once served. Without an explicit assertion the roster
stands, and a signed patch can never put an omitted ID back.

`allow_unlisted` is that explicit assertion: a signed boolean on one model
patch, default false, meaning "this exact ID is available on this provider's
official endpoint even though the roster omits it". It is honored only on an
`upsert` and only in a payload that carries `not_after`, so the claim always
expires and has to be renewed by publishing rather than lived with. An older
client that predates the field deserializes it as false and simply keeps roster
dominance. The assertion grants nothing else: it does not bypass identity,
region, endpoint, account/OAuth entitlement, or user configuration precedence,
and it names one exact ID — no prefix, family or fallback.

`hide` and `deprecate` act on a row the local catalog holds. An attested row is
retracted by dropping its upsert from the next payload or letting `not_after`
lapse. A failed or rejected request is never treated as evidence a model is
absent, and no fallback model is substituted for one.

A roster that answers with IDs alone has said nothing about limits or
capabilities — it has not said they are unknown. Signed values therefore
**complete** a provider-live row where it is silent, and never displace what the
provider stated: layer 20 still wins every field it sets. Completion covers
context, max output and reasoning support. One helper does this for the picker,
the metadata lookup and the route resolver alike, so those three cannot drift;
on the route-scoped surfaces it is gated by the identity/endpoint rule below,
while the cross-provider merged view stays partition-scoped as it already is for
ordinary patches. It deliberately excludes price: a
filled price would sit on a provider-live row with a signed price source, which
the dispatch-quote check does not admit, so it would render without being
billable. Cloud prices continue to apply only where no fresh roster owns the
row, keeping the price classes atomic and the source recorded.

Signed rows are scoped to one canonical provider identity on that provider's
official HTTPS endpoint contract, so a custom or proxied base URL never inherits
them. Catalog partitions collapse regional and dual-wire aliases onto a vendor
primary (`deepseek-cn` and `deepseek-anthropic` read `deepseek`;
`siliconflow-CN` reads `siliconflow`), and that collapse is not a channel for
facts: only a route whose own canonical identity is the identity the payload
names consumes them, matching how provider defaults have always been keyed.
Signing for an identity the catalog collapses is therefore inert rather than
cross-applied. The cross-provider merged view remains partition-scoped by
design; the endpoint contract is enforced at the route-scoped surfaces that
execution, pricing, and the model list read.

Capability and price provenance are independent. A capability-only patch keeps
the original price source. A cloud price block replaces all token classes
atomically; omitted cache/input/output classes remain unknown. The source
records signed facts version, verifying key, fetch time and validity bound.
Mutable cloud prices are frozen with the exact dispatch route and persisted
with the existing cost receipt. Later refresh/disable cannot reprice that turn,
and an old receipt with no frozen cloud quote cannot borrow a later cloud price.
Provider-owned billing tiers, subscription/local surfaces and routing-dependent
prices retain their existing checks.

Cloud model defaults are consulted only when no explicit selection or stronger
provider/account roster applies, through the normal route resolver. Codex model
availability and Ollama endpoint tags retain their own authority. Cloud data
cannot introduce a provider implementation, billing owner or wire protocol.
A cloud `base_url` field is accepted only by the shared static public HTTPS
endpoint contract; it is not consumed to migrate an execution endpoint.

## Model and provider registry (`registry.json`)

The signed models-and-providers registry (`codewhale.registry.v1`)
travels in the same `facts/v1` envelope on the `registry`
channel and is verified by the same pinned-key code
(`crates/config/src/cloud_facts/registry.rs`); the client is
`crates/cloud-facts/src/registry.rs`. The envelope's `facts_version` carries
the registry `revision` and `published_at` its `generated_at`.

A pulled registry is accepted only when a `founder` key signs it (an
automation key is refused), `schema` is exactly `codewhale.registry.v1`, the
payload parses (`dialects` is an object, as in CONTRACT §1), `generated_at`
is not in the future, and `revision` is not below the highest revision this
client already accepted. The payload may be up to 8 MiB. The floor check and
the install run under the same overlay lock as facts publication, so
overlapping pulls cannot install an older revision or lower the cached floor.

Pulled provider rows are admitted only when they are new (no id or alias that
names a built-in or bundled provider, including its config keys, catalog ids,
selector and subagent aliases and the legacy TUI row, and no `kind`), use an
`https` URL on a public host name with the default port and without
credentials, query or fragment (IP literals, single-label names, `localhost`,
`*.localhost`, `*.local` and `*.internal` are refused), and declare
`wire_policy` `openai-compatible`. A row may only
name its own id as `auth.secret_slot` and its own `<ID>_API_KEY` as
`auth.env_vars`, and that variable may not belong to a built-in provider.
Rows for built-in providers are ignored: their address, key and protocol come
only from the release. Refused rows are kept with a reason.

Wired: the registry follows `[cloud_facts]` (`enabled` and the hard disable)
and needs a source, `CODEWHALE_REGISTRY_URL` (an HTTP(S) URL) or
`CODEWHALE_REGISTRY_PATH` (a local envelope). Startup reads a verified
`facts/registry-cache.json`; a background pull, and `/model refresh`,
replace it. A failed or rejected pull keeps the copy already in use, and a
rejected cache is cleared while its revision floor is kept. The cache records
its source; a body from another source is dropped and only the floor is kept.
Turning cloud facts off, an invalid facts source, or the hard disable drops
the registry in the same step. CI suppresses the network pull.

What this does not do yet:

- There is no default URL and no published registry, so nothing is fetched by
  default and no bundled `registry.json` sets a floor; the floor is the
  highest revision this client cached.
- The website relay (`web/app/api/facts/v1/[channel]`) validates facts
  payloads only and does not serve the `registry` channel.
- Admitted providers are inert: they are held in memory with the credential
  slot and variable they may use, but the route resolver, picker and status
  surface do not read them yet.
- Models, offerings and dialects are parsed and held but not applied to the
  catalog.
- The `base_url` check is static. A public host name that resolves to a
  private address is not caught here; the client that first calls a pulled
  provider must use the guarded network client.
- A pulled id's `<ID>_API_KEY` is checked only against model-provider
  variables, not other credentials the product reads (for example
  `CODEWHALE_CLI_API_KEY` or web search keys). This must close before a
  consumer reads pulled credentials.
- As for facts, an equal revision replaces the held copy, and a torn cache
  file loses the floor at the next start.

## Website transport

`web/app/api/facts/v1/[channel]/route.ts` implements GET/HEAD for the public
channel. It reads `facts_current` over PostgREST using only the publishable
Supabase key, validates the complete signed envelope and caches only verified
responses. Existing `CURATED_KV` can retain a last-good copy, which is bounded
and revalidated under the same current trust/time rules before stale fallback.

With no active pinned key, delivery fails closed. Missing connection settings,
invalid upstream data or unavailable backing storage produce explicit errors.
HEAD responses, including errors, have no body. A strong ETag binds the complete
verified envelope, including signatures, so trust-material changes cannot reuse
an old representation validator. Conditional requests do not bypass validation.

Required deployment configuration, if separately authorized, is `SUPABASE_URL`
and `SUPABASE_PUBLISHABLE_KEY`. A service-role credential never belongs in the
website. `facts_current` must be a read-only view with explicit SELECT grants,
RLS and policies limited to published public channels. This repository slice
performs no remote schema, grant, key, or data mutation; those controls require
separate deployment evidence.

Two storage properties are part of the delivery contract rather than an
implementation detail, because a published fact is retracted through them:

- **A channel serves its head version only.** Revoking, expiring or
  future-dating the head must make the channel serve *nothing*, never the
  previous release. Silently re-serving an older version is a rollback
  delivered to every client whose version floor is not yet set; the repair for
  a bad release is publishing a higher `facts_version`, and the client's own
  rollback floor is the second line of defence, not the first.
- **`facts_version` is monotonic per channel.** Accepting a version at or below
  a channel's published high-water mark would let a withdrawn payload return.

Retraction therefore has two independent halves, and the operator should know
which one they are using. Publishing a later payload that drops the entry (or
letting `not_after` lapse) retracts the *fact*, and a client applies that at its
next successful refresh. `facts-publish.mjs revoke` stops the *release* at the
transport instead: it hands nothing to a client that asks, so a client already
holding the revoked envelope keeps applying it until its cached copy goes stale
— `ttl_secs`, 6 h by default, after which the payload stops being applied
whether or not a refresh succeeds. Neither half is instantaneous, and this layer
has no recall channel; a fact that must stop applying at an exact moment belongs
in `not_after`, not in a later revocation.

## Authoring and public fixtures

`web/scripts/check-cloud-facts.mjs` checks the unsigned source, release receipt,
Rust/web key-table equality and the public signed fixtures. It distinguishes a
valid empty trust table from a parser failure and rejects fixture trust anchors.

`web/scripts/facts-publish.mjs` supports validation, key generation, signing,
verification, SQL generation and publication. Signing/publishing are operator
actions requiring the relevant authority. Production verification/signing
requires an active pinned key; explicit fixture verification is separate. Key
generation creates a new private file exclusively, and CI signing is rejected
before any private-key read. Numeric version fields must be safe positive
integers before SQL or publication. Do not use real private keys in a repository,
logs or test fixtures.

`docs/cloud-facts/fixtures/test-only-signing-key.pem` is deliberately public and
has one exact GitGuardian path exception. Its public key is never pinned in a
production table. Tests may sign synthetic payloads using that fixture or an
ephemeral in-memory test key. No fixture signature establishes production trust.

To activate a future channel: approve key custody and its public anchor, update
both trust tables, verify and ship that anchor, then separately approve signing
and publication. Before signing, choose a facts version above the channel’s
verified published floor; the unsigned source version is not a live-channel receipt. Rotation pins the next key before dual-signing and retiring the
old key; there is no in-band command that can install or expand trust anchors.

Release notices and announcements are represented and scoped but do not replace
the existing release checker or introduce announcement rendering in this slice.
Organization-specific trust, automated publication and endpoint migration are
not implemented.
